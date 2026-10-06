# Dummy Contract Fixture

This is a minimal `no_std` Soroban contract used as a fixture for testing the `soroban-cost-profiler`.
It contains a simple `compute_heavy_loop` function that allows us to test the CPU instruction counting and execution tracing without depending on complex external logic.

This library is standalone and does not depend on the profiler crate, avoiding circular dependencies.
