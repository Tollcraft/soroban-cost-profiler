//! Binding the real Soroban host to the engine the profiler runs contracts in.
//!
//! A compiled Soroban contract is a WASM module whose data structures live on the host side, so its
//! binary imports the host functions it uses — `vec_new`, `vec_push_back`, `obj_from_u64` — exactly
//! the way a program imports `malloc`. Before this module the profiler linked against an
//! [empty `Linker`](crate::tracer::instantiate_module), so instantiation failed on the first import
//! and no contract that touched an SDK data structure could be traced at all.
//!
//! The bindings are not written by hand: [`soroban_env_host::call_macro_with_all_host_functions`]
//! hands over the whole host interface as an x-macro — every function's WASM import name, its typed
//! argument list, and the [`Host`] method that implements it — and this module turns each entry into
//! one [`wasmi::Linker::func_new`] registration that converts at the boundary and calls straight
//! into the [`Host`] already living in [`ProfilerState`]. That is the same table the production host
//! builds its own linker from, so the profiler traces the real implementation and not a model of it.
//!
//! ```text
//! contract word (i64)        typed argument        host method
//!      --[ HostArg::from_word ]->  VecObject   --> Host::vec_len
//!      <-[ HostRet::write ]-----   U32Val      <--
//! ```
//!
//! # The boundary is 64 bits wide, everywhere
//!
//! Every parameter of every one of the 199 host functions fits in an `i64`, and every result is
//! either nothing or one more. That is not an assumption about the contracts the profiler sees; it
//! is how the interface is defined — the `Env` trait in `soroban-env-common` is documented as
//! "consists of functions that take or return only 64-bit values such as `Val` or `u64`", because
//! the Soroban ABI has no wider channel. It is why this module can declare every registration with
//! one signature shape built from the argument count alone, and it is measurable in a compiled
//! contract: `fixtures/build.sh`'s module imports four host functions that share three WASM types —
//! `() -> i64`, `(i64) -> i64` and `(i64, i64) -> i64` — so `vec_len`, whose Soroban return type is
//! `U32Val`, and `obj_from_u64`, whose argument is a bare `u64`, both land on the `(i64) -> i64`
//! type. A 32-bit return would have needed a type of its own.
//!
//! # Import names are positions, not words
//!
//! The names the guest links against are not `vec_new`. `soroban-env-macros` labels each function
//! inside its module by position, from the alphabet `_ 0-9 a-z A-Z` and then two-character
//! combinations of it, so `vec_new` is imported as `("v", "_")` and `vec_push_back` as `("v", "6")`.
//! The labels come from the same table these bindings are generated from, which is what keeps the
//! two ends of the boundary in step.
//!
//! # What still does not work
//!
//! * **Ledger and authorization.** Functions that read chain state fail with a host error against
//!   the unpopulated [`Host::default`] this module binds. Giving them a ledger is issue 212's
//!   `--state` flag, not a gap in these bindings.
//! * **Contract-to-contract calls.** `call` and its sibling need to re-enter the engine, which the
//!   production host does by handing its dispatch a live `wasmi::Caller`. The `Env` methods called
//!   here run with no caller — the same path the network's native contracts call through — so those
//!   two functions return a host error instead of recursing. Every other host function is
//!   unaffected, and neither is reachable from a single-module trace.
//!
//! [`Host::default`]: soroban_env_host::Host::default

use crate::tracer::ProfilerState;
use soroban_env_host::xdr::{ScErrorCode, ScErrorType};
use soroban_env_host::{
    AddressObject, Bool, BytesObject, ContractTtlExtension, DurationObject, Env, Error,
    ExecutableTagObject, Host, HostError, I64Object, I128Object, I256Object, I256Val, MapObject,
    MuxedAddressObject, StorageType, StringObject, Symbol, SymbolObject, TimepointObject, U32Val,
    U64Object, U64Val, U128Object, U256Object, U256Val, Val, VecObject, Void,
};
use wasmi::{Error as EngineError, FuncType, Linker, Val as WasmVal, ValType};

/// Bind every Soroban host function into a linker for the profiler's store state.
///
/// The registrations are all of them, not only the imports of the module about to be instantiated:
/// `wasmi` resolves imports against the linker, so an unused entry costs a slot in a table and a
/// closure that never runs, while a missing entry fails instantiation. Building the complete
/// interface is what lets one linker serve any contract the profiler is handed.
///
/// The closures capture nothing, which is what `wasmi` 2.0 requires of a host function (`Send +
/// Sync + 'static`); the [`Host`] they call is reached through the store
/// state instead.
pub fn link_soroban_host(engine: &wasmi::Engine) -> Result<Linker<ProfilerState>, EngineError> {
    // The linker is bound before the macros below, which are defined in this same block and name it
    // directly: `macro_rules!` resolves a name written in its body against the scope it was defined
    // in, so this is the binding the generated registrations write into.
    let mut linker = <Linker<ProfilerState>>::new(engine);

    macro_rules! bind_host_function {
        {
            $(
                $(#[$module_attr:meta])*
                mod $module_id:ident $module_str:literal
                {
                    $(
                        $(#[$function_attr:meta])*
                        { $function_str:literal, $($min_protocol:literal)?,
                          $($max_protocol:literal)?,
                          fn $host_method:ident($($argument:ident : $type:ty),*) -> $result:ty }
                    )*
                }
            )*
        } => {
            $(
                $(
                    bind_one!(
                        linker,
                        $module_str,
                        $function_str,
                        $host_method,
                        [$($argument : $type),*],
                        $result
                    );
                )*
            )*
        };
    }

    /// Register one host function under its import name.
    macro_rules! bind_one {
        ($linker:expr, $module:literal, $name:literal, $method:ident,
         [$($argument:ident : $type:ty),*], $result:ty) => {
            {
                // One step per declared argument. `macro_rules!` cannot count a repetition without a
                // metavariable in the repeated tokens, so this walks it instead.
                #[allow(unused_mut, reason = "a host function with no arguments never takes the step")]
                let mut arity: usize = 0;
                $(
                    let _ = stringify!($argument);
                    arity += 1;
                )*
                // Every argument is a word and the result is at most one word: see "The boundary is
                // 64 bits wide" above. `HostRet::HAS_RESULT` decides the signature and writes the
                // value, so the two cannot disagree about the arity of the result.
                let signature = FuncType::new(
                    (0..arity).map(|_| ValType::I64).collect::<Vec<ValType>>(),
                    if <$result as HostRet>::HAS_RESULT {
                        vec![ValType::I64]
                    } else {
                        Vec::new()
                    },
                );
                let bound = $linker.func_new(
                    $module,
                    $name,
                    signature,
                    // `_arguments` is underscored because a zero-argument host function never reads
                    // it, while every other registration does.
                    |caller, _arguments, output| {
                        let host = &caller.data().host;
                        bind_arguments!(host, _arguments, 0usize, [$($argument : $type),*]);
                        let value = host
                            .$method($($argument),*)
                            .map_err(|error| {
                                EngineError::new(format!(
                                    "host function '{}.{}' failed: {}",
                                    $module, $name, error
                                ))
                            })?;
                        <_ as HostRet>::write(value, output)
                    },
                );
                bound.map_err(|error| EngineError::new(error.to_string()))?;
            }
        };
    }

    /// Convert the WASM words into typed arguments, each bound under the name the host interface
    /// gives it, so the call in [`bind_one`] reads `host.vec_len(v, x)` rather than a list of
    /// positions. The position advances one step per recursion because `macro_rules!` has no
    /// counter of its own.
    macro_rules! bind_arguments {
        ($host:expr, $arguments:expr, $index:expr, [$argument:ident : $type:ty]) => {
            let $argument = <$type as HostArg>::from_word($host, word($arguments, $index)?)?;
        };
        ($host:expr, $arguments:expr, $index:expr,
         [$argument:ident : $type:ty, $($rest:ident : $rest_type:ty),+]) => {
            let $argument = <$type as HostArg>::from_word($host, word($arguments, $index)?)?;
            bind_arguments!($host, $arguments, ($index + 1), [$($rest : $rest_type),+]);
        };
        ($host:expr, $arguments:expr, $index:expr, []) => {};
    }

    soroban_env_host::call_macro_with_all_host_functions! { bind_host_function }
    Ok(linker)
}

/// A host function's typed argument, read from the 64-bit word the contract passed.
trait HostArg: Sized {
    /// `Host` is the conversion context because that is where the production host reports a value
    /// of the wrong type; the tagged types here check their own tag and need nothing from it.
    fn from_word(host: &Host, word: i64) -> Result<Self, EngineError>;
}

/// A host function's typed result, written back as the 64-bit word the contract reads.
trait HostRet {
    /// Whether this result occupies a slot in the WASM signature.
    const HAS_RESULT: bool;
    fn write(self, out: &mut [WasmVal]) -> Result<(), EngineError>;
}

/// Turns a host-side failure into a trap the engine can report.
///
/// A `HostError` carries its own error type, code and diagnostic, which is the sentence a reader of
/// a trace needs. The function name is added at the call site rather than here, because only the
/// call site has it.
fn trap(error: HostError) -> EngineError {
    EngineError::new(error.to_string())
}

/// The host error the production host raises for a value it cannot use.
fn invalid_input() -> HostError {
    Error::from_type_and_code(ScErrorType::Value, ScErrorCode::InvalidInput).into()
}

/// The `index`th WASM argument as a 64-bit word.
///
/// `wasmi` checks an import's declared type against this [`FuncType`] at instantiation, so a
/// registered function is always handed exactly the words it declares. The two failure paths are
/// for the case where that stops being true — a wrong arity in the generated table, or an engine
/// that starts passing something else — either of which would otherwise be a silently misread
/// argument or a panic inside a callback the engine cannot unwind.
fn word(args: &[WasmVal], index: usize) -> Result<i64, EngineError> {
    match args.get(index) {
        Some(WasmVal::I64(value)) => Ok(*value),
        Some(other) => Err(EngineError::new(format!(
            "host function argument {index} is {other:?}, expected i64"
        ))),
        None => Err(EngineError::new(format!(
            "host function argument {index} is missing"
        ))),
    }
}

/// The raw bits a [`Val`] carries, which is what the WASM boundary passes in both directions.
///
/// The tag is part of the bits, so no conversion applies here: the guest arrives with a tagged word
/// and the host leaves with one.
fn to_word(value: Val) -> i64 {
    value.get_payload() as i64
}

fn from_word_payload(word: i64) -> Val {
    Val::from_payload(word as u64)
}

/// Implement the boundary conversions for a type that travels as a tagged [`Val`].
macro_rules! via_val {
    ($($type:ty),+ $(,)?) => {
        $(
            impl HostArg for $type {
                fn from_word(_host: &Host, word: i64) -> Result<Self, EngineError> {
                    <Self as TryFrom<Val>>::try_from(from_word_payload(word))
                        .map_err(|_| trap(invalid_input()))
                }
            }
            impl HostRet for $type {
                const HAS_RESULT: bool = true;
                fn write(self, out: &mut [WasmVal]) -> Result<(), EngineError> {
                    out[0] = WasmVal::I64(to_word(self.to_val()));
                    Ok(())
                }
            }
        )+
    };
}

// The complete set of argument and result types in the host interface, taken from the same table
// `call_macro_with_all_host_functions` expands — `soroban-env-common`'s `env.json` — and not from
// the handful one contract happens to import. A type added upstream that is missing here fails to
// compile the next time the dependency moves, which is the failure mode wanted: it arrives as a
// build error rather than as a contract that traps on a function nobody bound.
via_val!(
    AddressObject,
    Bool,
    BytesObject,
    DurationObject,
    Error,
    ExecutableTagObject,
    I128Object,
    I256Object,
    I256Val,
    I64Object,
    MapObject,
    MuxedAddressObject,
    StringObject,
    Symbol,
    SymbolObject,
    TimepointObject,
    U128Object,
    U256Object,
    U256Val,
    U32Val,
    U64Object,
    U64Val,
    VecObject,
);

// `Val` is the already-tagged word itself, so it crosses by bitcast and takes its tag on trust: a
// host function that accepts `Val` accepts any good value, and the object types above are the ones
// that check.
impl HostArg for Val {
    fn from_word(_host: &Host, word: i64) -> Result<Self, EngineError> {
        let value = from_word_payload(word);
        if value.is_good() {
            Ok(value)
        } else {
            Err(trap(invalid_input()))
        }
    }
}

impl HostRet for Val {
    const HAS_RESULT: bool = true;
    fn write(self, out: &mut [WasmVal]) -> Result<(), EngineError> {
        out[0] = WasmVal::I64(to_word(self));
        Ok(())
    }
}

/// Implement the boundary conversions for an `#[repr(u64)]` enum argument.
///
/// The production host marshals these two enums through `num_traits::FromPrimitive` on the raw
/// word. Naming the variants here instead keeps `num-traits` out of the dependency tree — the
/// interface has two such types, and both are arguments to functions that need a ledger, so they
/// cannot run against this host yet either way (see "What still does not work").
macro_rules! by_discriminant {
    ($type:ty => [$($variant:path),+ $(,)?]) => {
        impl HostArg for $type {
            fn from_word(_host: &Host, word: i64) -> Result<Self, EngineError> {
                const VARIANTS: &[$type] = &[$($variant),+];
                VARIANTS
                    .iter()
                    .copied()
                    .find(|variant| *variant as i64 == word)
                    .ok_or_else(|| trap(invalid_input()))
            }
        }
        impl HostRet for $type {
            const HAS_RESULT: bool = true;
            fn write(self, out: &mut [WasmVal]) -> Result<(), EngineError> {
                out[0] = WasmVal::I64(self as i64);
                Ok(())
            }
        }
    };
}

by_discriminant!(StorageType => [
    StorageType::Temporary,
    StorageType::Persistent,
    StorageType::Instance,
]);
by_discriminant!(ContractTtlExtension => [
    ContractTtlExtension::InstanceAndCode,
    ContractTtlExtension::Instance,
    ContractTtlExtension::Code,
]);

// Bare integers are the two types the ABI carries untagged.
impl HostArg for u64 {
    fn from_word(_host: &Host, word: i64) -> Result<Self, EngineError> {
        Ok(word as u64)
    }
}

impl HostRet for u64 {
    const HAS_RESULT: bool = true;
    fn write(self, out: &mut [WasmVal]) -> Result<(), EngineError> {
        out[0] = WasmVal::I64(self as i64);
        Ok(())
    }
}

impl HostArg for i64 {
    fn from_word(_host: &Host, word: i64) -> Result<Self, EngineError> {
        Ok(word)
    }
}

impl HostRet for i64 {
    const HAS_RESULT: bool = true;
    fn write(self, out: &mut [WasmVal]) -> Result<(), EngineError> {
        out[0] = WasmVal::I64(self);
        Ok(())
    }
}

// A host function returning `Void` reports success or failure but hands back no value: its WASM
// signature declares no result and its closure writes nothing, which is why it cannot share the
// list above.
impl HostRet for Void {
    const HAS_RESULT: bool = false;
    fn write(self, _out: &mut [WasmVal]) -> Result<(), EngineError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tracer::{
        ExecutionTracer, ProfilerState, instantiate_module, invoke_function, parse_module,
        setup_engine, setup_mock_env,
    };
    use std::collections::{BTreeMap, BTreeSet};
    use wasmi::Store;

    /// One row of the host interface, exactly as `call_macro_with_all_host_functions` hands it over.
    #[derive(Debug)]
    struct Entry {
        module: &'static str,
        name: &'static str,
        method: &'static str,
        arity: usize,
        has_result: bool,
    }

    /// Walk the real host table.
    ///
    /// The tests below read the interface through the same x-macro [`link_soroban_host`] expands, so
    /// they check the generated bindings against their source rather than against a transcription
    /// of it — a transcription would keep passing after the table moved.
    fn host_table() -> Vec<Entry> {
        let mut table: Vec<Entry> = Vec::new();
        macro_rules! collect {
            {
                $(
                    $(#[$module_attr:meta])*
                    mod $module_id:ident $module_str:literal
                    {
                        $(
                            $(#[$function_attr:meta])*
                            { $function_str:literal, $($min_protocol:literal)?,
                              $($max_protocol:literal)?,
                              fn $host_method:ident($($argument:ident : $type:ty),*) -> $result:ty }
                        )*
                    }
                )*
            } => {
                $(
                    $(
                        {
                            #[allow(unused_mut, reason = "a zero-argument entry never steps")]
                            let mut arity: usize = 0;
                            $(
                                let _ = stringify!($argument);
                                arity += 1;
                            )*
                            table.push(Entry {
                                module: $module_str,
                                name: $function_str,
                                method: stringify!($host_method),
                                arity,
                                has_result: <$result as HostRet>::HAS_RESULT,
                            });
                        }
                    )*
                )*
            };
        }
        soroban_env_host::call_macro_with_all_host_functions! { collect }
        table
    }

    /// The import names the guest links against, in table order.
    fn import_key(entry: &Entry) -> String {
        format!("{}.{}", entry.module, entry.name)
    }

    #[test]
    fn the_host_interface_is_199_uniquely_named_functions() {
        let table = host_table();
        // The count is the one the production host binds too, so a difference here means this
        // profiler and a network host disagree about what the interface is.
        assert_eq!(
            table.len(),
            199,
            "host table: {:#?}",
            table.iter().map(import_key).collect::<Vec<_>>()
        );
        // `Linker::func_new` refuses a second definition under one key, so duplicate names would
        // fail the linker build; uniqueness is what lets all 199 registrations coexist.
        let keys: BTreeSet<String> = table.iter().map(import_key).collect();
        assert_eq!(
            keys.len(),
            table.len(),
            "two host functions share an import name"
        );
        let modules: BTreeSet<&str> = table.iter().map(|e| e.module).collect();
        assert_eq!(modules.len(), 11, "host import modules: {modules:?}");
    }

    #[test]
    fn every_argument_is_one_word_and_every_result_at_most_one() {
        // This is the measured claim behind "The boundary is 64 bits wide, everywhere": the whole
        // table's WASM shape is `arity` i64 parameters plus zero or one i64 result, so `bind_one`
        // can build a signature from the argument count alone. Arity 8 is the widest function in the
        // interface, and the gaps in the histogram are real — nothing in it takes six or seven.
        let mut histogram: BTreeMap<usize, usize> = BTreeMap::new();
        let mut void: Vec<&str> = Vec::new();
        for entry in &host_table() {
            *histogram.entry(entry.arity).or_default() += 1;
            if !entry.has_result {
                void.push(entry.method);
            }
        }
        assert_eq!(
            histogram,
            BTreeMap::from([(0, 12), (1, 72), (2, 78), (3, 21), (4, 11), (5, 3), (8, 2)])
        );
        // `Void` is the only result type that declares no output word, and both the signature and
        // the write read `HostRet::HAS_RESULT`, so the two cannot disagree about it. A new result
        // *kind* has to be added to the conversion list, which arrives as a compile error; a new
        // void-returning function lands here.
        assert_eq!(void.len(), 27, "void-returning host functions: {void:?}");
    }

    /// A minimal module that imports every host function by its table name and calls nothing.
    ///
    /// `wasmi` resolves every import at instantiation and reports the first it cannot find, so a
    /// module asking for all 199 is the direct test that the linker answers all of them — a name
    /// the generator skipped would surface here as a missing import rather than as a contract that
    /// fails somewhere else, later.
    fn module_importing_every_host_function() -> Vec<u8> {
        let table = host_table();
        // One type entry per distinct (arity, has_result) shape, indexed in first-seen order.
        let mut types: Vec<(usize, bool)> = Vec::new();
        for entry in &table {
            if !types.contains(&(entry.arity, entry.has_result)) {
                types.push((entry.arity, entry.has_result));
            }
        }

        let mut bytes: Vec<u8> = vec![0x00, 0x61, 0x73, 0x6D, 0x01, 0x00, 0x00, 0x00];
        let mut type_section: Vec<u8> = Vec::new();
        for (arity, has_result) in &types {
            let mut entry = vec![0x60, *arity as u8];
            entry.extend(std::iter::repeat_n(0x7E_u8, *arity));
            entry.push(u8::from(*has_result));
            if *has_result {
                entry.push(0x7E);
            }
            type_section.extend(entry);
        }
        section(&mut bytes, 1, uleb(types.len() as u64), type_section);

        let mut import_section: Vec<u8> = Vec::new();
        for entry in &table {
            let index = types
                .iter()
                .position(|shape| *shape == (entry.arity, entry.has_result))
                .expect("every entry got a type above");
            let mut item = name_bytes(entry.module);
            item.extend(name_bytes(entry.name));
            item.push(0x00); // imported item kind: func
            item.extend(uleb(index as u64));
            import_section.extend(item);
        }
        section(&mut bytes, 2, uleb(table.len() as u64), import_section);
        bytes
    }

    /// Append one section: id, payload length, then the payload exactly as given.
    ///
    /// The length covers the count field too, which is why the count is passed in as part of the
    /// payload rather than written by the caller.
    fn section(out: &mut Vec<u8>, id: u64, count: Vec<u8>, entries: Vec<u8>) {
        let mut payload = count;
        payload.extend(entries);
        out.extend(uleb(id));
        out.extend(uleb(payload.len() as u64));
        out.extend(payload);
    }

    /// Encoded length of the name list section entries: each name is a length byte plus its bytes.
    fn name_bytes(text: &str) -> Vec<u8> {
        let mut out = uleb(text.len() as u64);
        out.extend(text.as_bytes());
        out
    }

    /// Unsigned LEB128, the only integer encoding the WASM sections below need.
    fn uleb(mut value: u64) -> Vec<u8> {
        let mut out = Vec::new();
        loop {
            let mut byte = (value & 0x7F) as u8;
            value >>= 7;
            if value != 0 {
                byte |= 0x80;
            }
            out.push(byte);
            if value == 0 {
                return out;
            }
        }
    }

    #[test]
    fn the_linker_resolves_every_host_import() {
        let engine = setup_engine();
        let module = parse_module(&engine, &module_importing_every_host_function())
            .expect("generated all-imports module should compile");
        let state = ProfilerState {
            tracer: ExecutionTracer::new(),
            host: setup_mock_env(),
            last_fuel: 0,
        };
        let mut store = Store::new(&engine, state);
        instantiate_module(&engine, &mut store, &module)
            .expect("the linker should provide every name in the host table");
    }

    /// A hand-assembled contract that builds an SDK vector through the host.
    ///
    /// ```text
    /// (module
    ///   (import "v" "_" (func (result i64)))              ;; vec_new
    ///   (import "i" "_" (func (param i64) (result i64)))   ;; obj_from_u64
    ///   (import "v" "6" (func (param i64 i64) (result i64)));; vec_push_back
    ///   (import "v" "3" (func (param i64) (result i64)))   ;; vec_len
    ///   (func (export "vec_round_trip") (result i64)       ;; vec_len(push(vec_new(), obj(7)))
    ///     ... ))
    /// ```
    ///
    /// The four import names are the positional labels the SDK's own build uses — see "Import names
    /// are positions, not words" above — and are the imports `fixtures/build.sh`'s artifact carries,
    /// decoded by hand from its import section and confirmed against the table in
    /// `the_positional_labels_name_the_functions_the_fixture_imports`.
    const VEC_ROUND_TRIP: &[u8] = &[
        0x00, 0x61, 0x73, 0x6D, // \0asm
        0x01, 0x00, 0x00, 0x00, // version
        // ---- section 1: three function types, all i64 words ----
        0x01, //   id: type
        0x10, //   payload length (16)
        0x03, //   three types
        0x60, 0x00, 0x01, 0x7E, //     () -> i64                     (vec_new)
        0x60, 0x01, 0x7E, 0x01, 0x7E, //     (i64) -> i64             (obj_from_u64, vec_len)
        0x60, 0x02, 0x7E, 0x7E, 0x01, 0x7E, //     (i64, i64) -> i64  (vec_push_back)
        // ---- section 2: the four host imports, indices 0 through 3 ----
        0x02, //   id: import
        0x19, //   payload length (25)
        0x04, //   four imports
        0x01, b'v', 0x01, b'_', 0x00, 0x00, //   "v"."_" -> type 0, func 0
        0x01, b'i', 0x01, b'_', 0x00, 0x01, //   "i"."_" -> type 1, func 1
        0x01, b'v', 0x01, b'6', 0x00, 0x02, //   "v"."6" -> type 2, func 2
        0x01, b'v', 0x01, b'3', 0x00, 0x01, //   "v"."3" -> type 1, func 3
        // ---- section 3: the contract's own function, index 4 ----
        0x03, //   id: function
        0x02, //   payload length
        0x01, //   one function
        0x00, //     type 0: () -> i64
        // ---- section 7: its export name ----
        0x07, //   id: export
        0x12, //   payload length (18)
        0x01, //   one export
        0x0E, b'v', b'e', b'c', b'_', b'r', b'o', b'u', b'n', b'd', b'_', b't', b'r', b'i', b'p',
        0x00, 0x04, //   func index 4
        // ---- section 10: the body ----
        0x0A, //   id: code
        0x0E, //   payload length (14)
        0x01, //   one body
        0x0C, //   body length (12)
        0x00, //   no locals
        0x10, 0x00, //   call 0  vec_new                       -> [vec]
        0x42, 0x07, //   i64.const 7                            -> [vec, 7]
        0x10, 0x01, //   call 1  obj_from_u64(7)                -> [vec, u64obj]
        0x10, 0x02, //   call 2  vec_push_back(vec, u64obj)     -> [vec']
        0x10, 0x03, //   call 3  vec_len(vec')                  -> [len]
        0x0B, //   end
    ];

    /// Instantiate the round-trip module and call its one export, returning its word.
    fn run_vec_round_trip() -> (i64, Vec<crate::models::TraceEvent>) {
        let engine = setup_engine();
        let module = parse_module(&engine, VEC_ROUND_TRIP)
            .expect("round-trip module should compile; the hand-assembled encoding is wrong");
        let state = ProfilerState {
            tracer: ExecutionTracer::new(),
            host: setup_mock_env(),
            last_fuel: 0,
        };
        let mut store = Store::new(&engine, state);
        // The engine has metering on and a fresh store starts at zero fuel, so a run that is
        // supposed to finish has to be given its budget — the same `u64::MAX` the CLI passes.
        store
            .set_fuel(u64::MAX)
            .expect("setup_engine() enables fuel metering");
        let instance = instantiate_module(&engine, &mut store, &module)
            .expect("round-trip module should instantiate against the host linker");
        let mut results = [WasmVal::I64(0)];
        invoke_function(&mut store, &instance, "vec_round_trip", &[], &mut results)
            .expect("the host calls the module makes all resolve");
        let WasmVal::I64(word) = results[0] else {
            panic!("vec_len returns a tagged word, got {:?}", results[0]);
        };
        (word, store.into_data().tracer.flush_trace())
    }

    #[test]
    fn running_wasm_calls_the_real_host() {
        // The word comes back as the host's own `U32Val`, so equality against a value built by
        // `soroban-env-common` is the assertion that the vector really grew: the handle in the
        // middle of the chain only exists because `vec_new` and `vec_push_back` wrote it into the
        // host's object store, and `vec_len` read it back out.
        let (word, _) = run_vec_round_trip();
        assert_eq!(word, to_word(U32Val::from(1u32).to_val()));
        assert_ne!(word, 0, "a zero word is the empty slot, not a length");
    }

    #[test]
    fn the_trace_names_each_host_boundary_the_contract_crossed() {
        // #210 exists so that a real contract can be *traced*, not only instantiated: the four host
        // functions above have to reach the tracer as call/return pairs.
        use crate::models::EventType;
        let (_, events) = run_vec_round_trip();
        let host_calls = events
            .iter()
            .filter(|e| e.event_type == EventType::HostCall)
            .count();
        let host_returns = events
            .iter()
            .filter(|e| e.event_type == EventType::HostReturn)
            .count();
        assert_eq!(host_calls, 4, "events: {events:?}");
        assert_eq!(host_returns, 4, "events: {events:?}");
    }

    #[test]
    fn the_positional_labels_name_the_functions_the_fixture_imports() {
        // `fixtures/build.sh`'s artifact imports these four pairs; the names alone say nothing, so
        // each is pinned to the `Host` method it must reach. A label that drifted from the table
        // would bind a contract to the wrong function silently — the import still resolves, the
        // profile is just of something else.
        let table = host_table();
        for (module, name, method) in [
            ("v", "_", "vec_new"),
            ("v", "3", "vec_len"),
            ("v", "6", "vec_push_back"),
            ("i", "_", "obj_from_u64"),
        ] {
            let entry = table
                .iter()
                .find(|e| e.module == module && e.name == name)
                .unwrap_or_else(|| {
                    panic!("nothing in the host table is imported as (\"{module}\", \"{name}\")")
                });
            assert_eq!(entry.method, method);
        }
    }

    #[test]
    fn an_argument_is_read_as_the_word_the_signature_declares() {
        let words = [WasmVal::I64(7), WasmVal::I32(1)];
        assert_eq!(word(&words, 0).unwrap(), 7);
        // `wasmi` validates the import's type at instantiation, so these are the engine's own bug or
        // a wrong arity in the generated table — either way the answer is a trap, never a misread.
        assert!(
            word(&words, 1)
                .unwrap_err()
                .to_string()
                .contains("argument 1 is I32(1), expected i64")
        );
        assert!(
            word(&words, 2)
                .unwrap_err()
                .to_string()
                .contains("argument 2 is missing")
        );
    }

    #[test]
    fn a_tagged_word_round_trips_through_the_boundary() {
        let host = setup_mock_env();
        let mut out = [WasmVal::I64(-1)];
        U32Val::from(42u32)
            .write(&mut out)
            .expect("writing a word cannot fail");
        let WasmVal::I64(raw) = out[0] else {
            panic!("results are written as i64")
        };
        // Equality against the word the type itself produces, so the test does not depend on which
        // of the small-value helpers the dependency happens to export.
        assert_eq!(
            to_word(
                U32Val::from_word(&host, raw)
                    .expect("the tag survives")
                    .to_val()
            ),
            to_word(U32Val::from(42u32).to_val())
        );

        // The other half of the check: a word tagged as something else is refused rather than
        // reinterpreted, which is what keeps a wrong import from silently reading a `Bool` as a
        // length.
        let mut boolean = [WasmVal::I64(0)];
        Val::TRUE
            .write(&mut boolean)
            .expect("writing a word cannot fail");
        let WasmVal::I64(raw) = boolean[0] else {
            unreachable!()
        };
        assert!(U32Val::from_word(&host, raw).is_err());
    }

    #[test]
    fn a_word_with_no_valid_tag_is_not_a_good_value() {
        let host = setup_mock_env();
        // `Val` takes its tag on trust, so `is_good()` is the only check it gets — and the one the
        // production host applies too. Every small-value tag the ABI defines is accepted; the
        // rejected case is the tag it reserves for nothing.
        let good = to_word(U32Val::from(7u32).to_val());
        assert!(Val::from_word(&host, good).is_ok());
        let bad = Val::from_payload(u64::MAX);
        assert!(!bad.is_good(), "the probe word {bad:?} has to be a bad tag");
        assert!(Val::from_word(&host, to_word(bad)).is_err());
    }

    #[test]
    fn enum_arguments_are_matched_by_discriminant() {
        let host = setup_mock_env();
        for variant in [
            StorageType::Temporary,
            StorageType::Persistent,
            StorageType::Instance,
        ] {
            let word = variant as i64;
            let read = StorageType::from_word(&host, word).expect("a listed discriminant");
            assert_eq!(read as i64, word, "wrong variant for word {word}");
        }
        // A word outside the listed variants is input the host cannot use, not a value to guess at.
        assert!(StorageType::from_word(&host, 9).is_err());
        for variant in [
            ContractTtlExtension::InstanceAndCode,
            ContractTtlExtension::Instance,
            ContractTtlExtension::Code,
        ] {
            let word = variant as i64;
            assert_eq!(
                ContractTtlExtension::from_word(&host, word).expect("a listed discriminant") as i64,
                word
            );
        }
        assert!(ContractTtlExtension::from_word(&host, 9).is_err());
    }
}
