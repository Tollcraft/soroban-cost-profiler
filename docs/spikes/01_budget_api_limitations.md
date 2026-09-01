# SPIKE 01: `soroban-env-host` Budget API Limitations

## Objective
Investigate whether `soroban-env-host` allows intercepting per-call-site host costs without forking `rs-soroban-env`. If not, determine how to extract cumulative totals using public APIs.

## Findings

After investigating the `soroban_sdk::testutils::budget::Budget` API surface and the `rs-soroban-env` source, the following constraints were confirmed:

### 1. Cumulative Only, No Call-Site Granularity
The public `Budget` interface exposes `get_cost_tracker(cost_type)`. This provides a **cumulative** tracker of iterations and derived CPU/memory *per `CostType`*, aggregated across the whole execution.
- It is a read-after-the-fact rollup.
- It tells us "this transaction spent X total on `VmMemRead`", but it does **not** provide the granularity to say "this specific call inside `transfer()` spent Y."

### 2. Lack of Public Interception Hooks
While internal hooks like `invocation_metering` exist inside `rs-soroban-env`, they are internal and unstable. The `charge()` function does not expose a hookable call site that we can intercept live and correlate to the current state of our `ExecutionTracer`'s call stack.

## Architectural Decision
We will **not** fork `rs-soroban-env` to add unstable hooks, as this creates a severe upstream maintenance burden. 

Instead, for V1 we will scope down to **cumulative boundary totals**:
- During a WASM `HostCall`/`HostReturn` boundary event, we will snapshot the total cost using the public `Budget::get_cost_tracker()` API.
- We will diff the tracker snapshot before and after the host call.
- This provides an accurate *block* of cost attributed to the host, even if it cannot be broken down into individual sub-operations inside the host. 

## Next Steps
- Implement boundary-emission tracking inside `ExecutionTracer` (Issue: Distinguish Host Function boundaries from WASM call boundaries).
- Wire the tracker snapshots at these boundaries (Issue: Wire `Budget::get_cost_tracker()` snapshots into host-boundary events).
