//! Mock ledger state for `--state` (#212).
//!
//! The profiler used to hand every run a `Host::default()`, whose ledger is blank in both of the
//! ways that matter: it has no [`LedgerInfo`] at all — `sequence_number`, `timestamp`,
//! `network_id` and the TTL settings are simply absent — and its storage is an empty, enforcing
//! [`Storage`]. A contract that reads any of it therefore traps before it does any work, which is
//! why the runs this repository could cost were the ones that never touch the chain. The host is
//! built before tracing starts, so the fix belongs here rather than in the tracer: read a state
//! file, build the host around it, and let the run proceed.
//!
//! The file format is the standard one: a Soroban ledger snapshot, exactly as `soroban-cli` and
//! `Env::to_ledger_snapshot_file` write it, parsed by the [`LedgerSnapshot`] type from the
//! `soroban-ledger-snapshot` crate. This module does not invent a state format of its own, and the
//! snapshots a user already has from an integration test or a network dump load unchanged.
//!
//! Two things a snapshot does for the run, and one it cannot:
//!
//! * **Ledger info** goes in through [`Host::set_ledger_info`], and every guest read of it
//!   (`get_ledger_sequence`, `get_ledger_timestamp`, `get_ledger_network_id`, `get_ledger_version`,
//!   `get_max_live_until_ledger`) is then served from the file instead of trapping. Those five are
//!   context-free host functions, so they work from a directly invoked export.
//! * **Ledger entries** go in as a recording [`Storage`] over the snapshot
//!   ([`Storage::with_recording_footprint`]): the host reads a key through the snapshot when its own
//!   map has nothing for it, so a read of an entry the file carries returns the mocked value and a
//!   read of one it does not carries on reporting `MissingValue` the way a real ledger would.
//! * What the file cannot supply is the **contract frame** a host call normally runs inside.
//!   `get_contract_data` and friends build their ledger key from the *current contract ID*, and the
//!   profiler invokes exports from outside a contract call, so the stack is empty and the key is
//!   never built. The read then answers with nothing more than `Error(Context, InternalError)` —
//!   measured here, in `a_contract_data_read_stops_at_the_missing_frame` — because the host's
//!   sentence for its reason is a `DebugInfo` it only builds with its own `testutils` feature. That
//!   is the ceiling `src/host.rs` documents, not a state problem: the entries are installed as the
//!   snapshot source the recording [`Storage`] falls back to, and no read reaches that fallback.
//!
//! [`LedgerInfo`]: soroban_env_host::LedgerInfo
//! [`Storage`]: soroban_env_host::storage::Storage
//! [`Storage::with_recording_footprint`]: soroban_env_host::storage::Storage::with_recording_footprint

use std::error::Error as _;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use soroban_env_host::Host;
use soroban_env_host::budget::Budget;
use soroban_env_host::storage::Storage;
use soroban_ledger_snapshot::LedgerSnapshot;

/// A state file that could not become a host.
///
/// Four shapes of failure, kept apart because the advice differs: a path that is wrong, a file
/// that is not a snapshot, a snapshot this host cannot cost, and ledger info this host refuses to
/// accept. `main` prints the message and exits 1 for all four — none of them is the profiler
/// failing at its own work.
#[derive(Debug)]
pub enum StateError {
    /// The path could not be read.
    Unreadable {
        /// What the user named.
        path: PathBuf,
        /// The OS's own reason.
        reason: String,
    },
    /// The bytes are not a ledger snapshot.
    NotASnapshot {
        /// What the user named.
        path: PathBuf,
        /// serde's reason, with its line and column.
        reason: String,
    },
    /// The snapshot names a protocol this host cannot cost.
    ProtocolMismatch {
        /// What the user named.
        path: PathBuf,
        /// The protocol the file declares.
        file: u32,
        /// The protocol the linked host implements.
        host: u32,
    },
    /// The host refused the ledger info.
    LedgerRejected {
        /// What the user named.
        path: PathBuf,
        /// The host's own words.
        reason: String,
    },
}

impl std::fmt::Display for StateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unreadable { path, reason } => {
                write!(f, "failed to read {}: {reason}", path.display())
            }
            Self::NotASnapshot { path, reason } => write!(
                f,
                "{} is not a Soroban ledger snapshot: {reason}. A snapshot is the JSON written by \
                 `soroban ledger json` or by `Env::to_ledger_snapshot_file`.",
                path.display()
            ),
            Self::ProtocolMismatch { path, file, host } => write!(
                f,
                "{} declares protocol {file} and this profiler's host implements {host}. Cost \
                 tables differ between protocols, so a snapshot from another protocol is refused \
                 rather than silently re-stamped; set `protocol_version` to {host} only when the \
                 ledger really is that protocol.",
                path.display()
            ),
            Self::LedgerRejected { path, reason } => write!(
                f,
                "the host refused the ledger info in {}: {reason}",
                path.display()
            ),
        }
    }
}

/// The protocol version the linked host costs for, which a snapshot has to name.
///
/// Read from the host's own interface version rather than written down, so this cannot drift when
/// the `soroban-env-host` dependency moves.
pub fn supported_protocol_version() -> u32 {
    soroban_env_host::meta::INTERFACE_VERSION.protocol
}

/// Read and parse a snapshot without building a host from it.
///
/// Split out because the CLI reports what it loaded — sequence, timestamp and entry count — and
/// because the host builder takes the snapshot by value rather than cloning it.
pub fn read_snapshot(path: &Path) -> Result<LedgerSnapshot, StateError> {
    let text = std::fs::read_to_string(path).map_err(|error| StateError::Unreadable {
        path: path.to_path_buf(),
        reason: error.to_string(),
    })?;
    LedgerSnapshot::read(std::io::Cursor::new(text)).map_err(|error| {
        // `LedgerSnapshot::read` wraps the serde failure in a two-variant `io`/`serde` error whose
        // own Display is the word "serde"; the message the user needs is the source it carries.
        let reason = error
            .source()
            .map(ToString::to_string)
            .unwrap_or_else(|| error.to_string());
        StateError::NotASnapshot {
            path: path.to_path_buf(),
            reason,
        }
    })
}

/// Build the host a `--state` run traces against, from a snapshot already read.
///
/// `path` names the file the snapshot came from and is carried into every message here, so a
/// refusal says which of possibly several files on the command line was rejected.
///
/// The storage is the snapshot's, in recording mode, over the same default [`Budget`]
/// [`Host::default`] uses — the budget is what the cost columns are read from, and a state file has
/// no business changing it.
///
/// [`Budget`]: soroban_env_host::budget::Budget
/// [`Host::default`]: soroban_env_host::Host::default
pub fn host_from_snapshot(snapshot: LedgerSnapshot, path: &Path) -> Result<Host, StateError> {
    let info = snapshot.ledger_info();
    let host_protocol = supported_protocol_version();
    if info.protocol_version != host_protocol {
        return Err(StateError::ProtocolMismatch {
            path: path.to_path_buf(),
            file: info.protocol_version,
            host: host_protocol,
        });
    }
    let host = Host::with_storage_and_budget(
        Storage::with_recording_footprint(Rc::new(snapshot)),
        Budget::default(),
    );
    // The protocol has just been checked, so what is left for the host to refuse is its own
    // judgement about a ledger it cannot run, which the user should read verbatim.
    host.set_ledger_info(info)
        .map_err(|error| StateError::LedgerRejected {
            path: path.to_path_buf(),
            reason: error.to_string(),
        })?;
    Ok(host)
}

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_env_host::{Env, StorageType, Symbol, TryFromVal};
    use std::io::Write;
    use std::path::Path;

    /// The committed fixture: a protocol-28 ledger with two entries.
    const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures/state/ledger.json");

    /// Write a one-off state file inside a temp dir.
    fn state_file(dir: &Path, name: &str, text: &str) -> PathBuf {
        let path = dir.join(name);
        let mut file = std::fs::File::create(&path).unwrap();
        file.write_all(text.as_bytes()).unwrap();
        path
    }

    /// The fixture with its protocol rewritten, for the refusal case.
    fn snapshot_for_protocol(dir: &Path, protocol: u32) -> PathBuf {
        let text = std::fs::read_to_string(FIXTURE).unwrap();
        let text = text.replace(
            "\"protocol_version\": 28,",
            &format!("\"protocol_version\": {protocol},"),
        );
        state_file(dir, "rewritten.json", &text)
    }

    #[test]
    fn a_snapshot_file_describes_the_ledger_it_carries() {
        let snapshot = read_snapshot(Path::new(FIXTURE)).unwrap();
        assert_eq!(snapshot.sequence_number, 500);
        assert_eq!(snapshot.timestamp, 1_700_000_000);
        assert_eq!(snapshot.ledger_entries.len(), 2);
        // The fixture is the path every other test here walks, so it has to stay loadable as the
        // host dependency moves; this is the assertion that turns a protocol bump into a red test
        // rather than into a suite that silently refuses its own input.
        assert_eq!(
            snapshot.protocol_version,
            supported_protocol_version(),
            "fixture protocol is {} while the host implements {}",
            snapshot.protocol_version,
            supported_protocol_version()
        );
    }

    #[test]
    fn a_mocked_ledger_answers_the_read_a_blank_one_refuses() {
        // The host's words for this refusal ("missing ledger info") live in its DebugInfo, which is
        // compiled out unless the host's own `testutils` feature is on, so the status is what a
        // caller of the released profiler actually sees.
        let refused = Env::get_ledger_sequence(&Host::default())
            .err()
            .unwrap()
            .to_string();
        assert!(
            refused.contains("Error(Context, InternalError)"),
            "{refused}"
        );

        let snapshot = read_snapshot(Path::new(FIXTURE)).unwrap();
        let host = host_from_snapshot(snapshot, Path::new(FIXTURE)).unwrap();
        // 500 is the fixture's `sequence_number`, read through the same host function the contract
        // calls: `Env::get_ledger_sequence` is `("x","3")` in the guest's import table.
        assert_eq!(u32::from(Env::get_ledger_sequence(&host).unwrap()), 500);
        // `U64Val` holds 1.7e9 as an object rather than a small value, so the decode goes through
        // the env's conversion trait; `u64::from` exists only for values that fit the small tag.
        let timestamp = Env::get_ledger_timestamp(&host).unwrap();
        assert_eq!(u64::try_from_val(&host, &timestamp).unwrap(), 1_700_000_000);
    }

    #[test]
    fn a_snapshot_from_another_protocol_is_refused_by_name() {
        let dir = tempfile::tempdir().unwrap();
        let path = snapshot_for_protocol(dir.path(), 24);
        let error = host_from_snapshot(read_snapshot(&path).unwrap(), &path)
            .err()
            .unwrap()
            .to_string();
        assert!(error.contains("protocol 24"), "{error}");
        assert!(error.contains("implements 28"), "{error}");
        assert!(error.contains(path.to_str().unwrap()), "{error}");
    }

    #[test]
    fn a_state_file_that_is_not_there_says_so() {
        let missing = Path::new(FIXTURE).with_file_name("absent.json");
        let error = read_snapshot(&missing).err().unwrap().to_string();
        assert!(error.starts_with("failed to read "), "{error}");
        assert!(error.contains("absent.json"), "{error}");
    }

    #[test]
    fn state_that_is_not_a_snapshot_names_the_field_it_wanted() {
        let dir = tempfile::tempdir().unwrap();
        let path = state_file(dir.path(), "not-a-snapshot.json", "{\"hello\":1}");
        let error = read_snapshot(&path).err().unwrap().to_string();
        assert!(error.contains("not a Soroban ledger snapshot"), "{error}");
        // serde's own words, not a bare "serde": the field it missed is what the user fixes.
        assert!(error.contains("protocol_version"), "{error}");
    }

    #[test]
    fn a_snapshot_with_entries_builds_a_host() {
        // The entries half of #212: they reach the host as a recording storage over the snapshot,
        // which is the shape that reads a key through the file when its own map has nothing. What
        // this can assert here is that a snapshot carrying them is accepted and stays intact — the
        // reads that consult them are the contract-frame path `src/host.rs` documents as its
        // ceiling, so a guest read is not observable from this side of the boundary.
        let snapshot = read_snapshot(Path::new(FIXTURE)).unwrap();
        let count = snapshot.ledger_entries.len();
        let host = host_from_snapshot(snapshot, Path::new(FIXTURE)).unwrap();
        assert_eq!(count, 2);
        assert_eq!(u32::from(Env::get_ledger_sequence(&host).unwrap()), 500);
    }

    /// The ceiling, measured: the fixture's `COUNT` entry is installed in the host, and the host
    /// function a guest `l.1` import calls still refuses before consulting it. Only the status is
    /// observable — the host's sentence for the case lives in a `DebugInfo` it builds solely with
    /// its own `testutils` feature on, which this crate does not enable.
    #[test]
    fn a_contract_data_read_stops_at_the_missing_frame() {
        let host = host_from_snapshot(
            read_snapshot(Path::new(FIXTURE)).unwrap(),
            Path::new(FIXTURE),
        )
        .unwrap();
        let key = Symbol::try_from_val(&host, &"COUNT").unwrap();
        let error = Env::get_contract_data(&host, key.to_val(), StorageType::Persistent)
            .err()
            .unwrap()
            .to_string();
        assert!(error.contains("Error(Context, InternalError)"), "{error}");
    }
}
