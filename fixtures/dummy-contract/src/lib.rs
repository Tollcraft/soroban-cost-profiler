#![no_std]

use soroban_sdk::{contract, contractimpl, Env};

/// A minimal Soroban fixture contract that exposes a CPU-heavy helper.
///
/// The profiler consumes the compiled WASM to measure execution cost.
/// Keeping this contract independent of the profiler crate avoids
/// circular dependencies and lets the fixture evolve alongside each phase.
#[contract]
pub struct DummyContract;

#[contractimpl]
impl DummyContract {
    /// Execute a simple mathematical loop `iterations` times.
    ///
    /// The default test uses 10,000 iterations so the profiler has a
    /// non-trivial amount of work to measure without hitting CI timeouts.
    pub fn compute_heavy_loop(env: &Env, iterations: u32) -> u64 {
        // Silence the unused-parameter warning while keeping the standard
        // contract function signature expected by the Soroban SDK macros.
        let _ = env;

        let mut sum: u64 = 0;
        for i in 0..iterations {
            sum = sum.wrapping_add((i as u64).wrapping_mul(7));
        }
        sum
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn compute_heavy_loop_uses_ten_thousand_iterations() {
        let env = Env::default();
        let contract_id = env.register_contract(None, DummyContract);
        let client = DummyContractClient::new(&env, &contract_id);

        // 7 * sum(0..10000) = 7 * (9999 * 10000 / 2) = 349_965_000
        assert_eq!(client.compute_heavy_loop(&10_000), 349_965_000);
    }

    #[test]
    fn compute_heavy_loop_handles_zero_iterations() {
        let env = Env::default();
        let contract_id = env.register_contract(None, DummyContract);
        let client = DummyContractClient::new(&env, &contract_id);

        assert_eq!(client.compute_heavy_loop(&0), 0);
    }
}
