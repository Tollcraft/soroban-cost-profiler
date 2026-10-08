# Tracer Architecture

The execution tracer is designed to intercept WASM instruction boundaries using `wasmi` Engine Hooks. Because evaluating every single instruction adds massive overhead (a single Soroban transaction can run up to 100M instructions), the tracer employs a sampling mechanism.

## Instruction Metering

We leverage `wasmi`'s built-in fuel consumption (`consume_fuel(true)`) to measure computational cost natively. As the engine executes blocks of code, it deducts fuel. The profiler periodically reads this fuel consumption to attribute cost without needing to manually map every single instruction.

## The Event Buffer

`ExecutionTracer` maintains an internal vector of `TraceEvent` structures.
Instead of pushing an event for every WASM step, `record_step` aggregates `cpu_cost` into `current_step_cost`. When this accumulator hits the `sample_rate` threshold, a single `TraceEvent::Step` is pushed to the buffer, reducing memory overhead by several orders of magnitude.

Function boundaries (`TraceEvent::Call` and `TraceEvent::Return`) bypass the sampling filter completely. Capturing exact call stack boundaries is critical for attributing the sampled instruction costs to the correct parent function later in the pipeline.

## Soroban Host

The profiler builds a native `soroban_env_host::Host` and registers its whole interface — 199 functions across the eleven one-character Soroban modules — into the `wasmi` linker the module is instantiated against (`src/host.rs`). That is what lets a real `soroban-sdk` build run here at all: the guest's imports resolve, each call crosses into the production host, and the tracer's `CallingHost`/`ReturningFromHost` hooks cost the call from the host's own budget.

Two things the host still cannot give a contract. Ledger and authorization functions fail with a host error against the unpopulated `Host::default`, because nothing has been written into it — that is what a `--state` flag would supply. And `call` (contract-to-contract) returns a host error instead of re-entering the engine, because the `Env` methods here run with no live caller to hand to the host's dispatcher.
