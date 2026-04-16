# Base Execution Engine

Base Execution Engine is a Rust workspace for pre-confirmation ingress,
protocol decoding, route discovery, simulation, and execution on Base.

## Documentation

- Architecture and system design: [docs/base-execution-engine-design.md](/Users/wenyiyu/wenyi/rust-arbitrage-searcher/docs/base-execution-engine-design.md:1)
- Contract deployment notes: [contracts/DEPLOYMENT.md](/Users/wenyiyu/wenyi/rust-arbitrage-searcher/contracts/DEPLOYMENT.md:1)

## Core Architecture

- Flashblocks is the primary pre-confirmation ingress layer.
- The local Base node is the canonical state and simulation backend.
- The runtime is split into ingress, decode, graph, simulation, and execution
  modules.

## Workspace Layout

```text
crates/
  app/         Runtime wiring and supervision
  common/      Shared models and Base address book
  config/      Typed runtime configuration
  decode/      Protocol decoding and intent normalization
  graph/       Liquidity graph and route planning
  ingress/     Flashblocks and local chain connectivity
  simulation/  Route-level simulation
  strategy/    Venue-specific execution adapters
contracts/
  BaseExecutionRouter.sol
config/
  base-liquidity-seeds.json
docs/
  base-execution-engine-design.md
```

## Runtime Modes

- `research`: ingress, decoding, planning, and simulation only
- `shadow`: full execution pipeline with request construction, but broadcasting is suppressed
- `live`: full execution pipeline with real broadcasting enabled

## Configuration

Key environment variables:

- `ENGINE_MODE`
- `BASE_TRANSPORT`
- `BASE_IPC_PATH`
- `BASE_RPC_URL`
- `BASE_WS_URL`
- `FLASHBLOCKS_WS_URL`
- `BASE_CHAIN_ID`
- `LOG_LEVEL`
- `MAX_ROUTE_HOPS`
- `MAX_CANDIDATE_ROUTES`
- `SETTLEMENT_TOKENS`
- `MIN_NET_SURPLUS`
- `MIN_OUTPUT_BPS`
- `POST_TRIGGER_ONLY`
- `EXECUTION_ROUTER_ADDRESS`
- `FLASHBLOCKS_SUBSCRIBE_TRANSACTIONS`
- `FLASHBLOCKS_SUBSCRIBE_PENDING_LOGS`
- `LIQUIDITY_SEEDS_PATH`
- `STATE_DIR`
- `EXECUTOR_PRIVATE_KEY`
- `RECONCILE_INTERVAL_SECS`
- `SUBMISSION_TIMEOUT_SECS`

Static liquidity seed data can be stored in
[config/base-liquidity-seeds.example.json](/Users/wenyiyu/wenyi/rust-arbitrage-searcher/config/base-liquidity-seeds.example.json:1).

## Base Node Notes

- This runtime assumes the Base node is the canonical read and simulation backend.
- Do not assume every Base full node exposes the same advanced simulation RPCs.
  Support for ordered replay methods such as `eth_simulateV1` or `eth_callMany`
  depends on the actual Base execution client, client version, and RPC exposure
  settings.
- In `shadow` and `live`, the engine defaults to `POST_TRIGGER_ONLY=true`.
  That means opportunities are rejected unless the runtime can confirm a
  post-trigger replay path instead of falling back to current-state-only quotes.
- Trigger-aware replay should be verified against the exact node you run in
  production. A self-hosted node may still lack the RPC methods required for
  ordered transaction simulation.
- Keep the planner and the node environment aligned: if the node cannot support
  the replay path needed for a mode, treat that as a deployment constraint, not
  as something to bypass in policy.

## Current Status

The current workspace currently includes:

- Flashblocks websocket ingress scaffolding
- Transport-agnostic Base connectivity over IPC, HTTP, or websocket RPC
- Canonical transaction and intent models
- Multi-protocol calldata decoding
- Liquidity graph primitives and bounded cycle planning
- Venue-specific quote and swap adapters
- Settlement-aware route simulation with trigger preflight and optional
  post-trigger replay
- Router-shaped execution requests and guarded contract submission

The current implementation is intentionally focused on architecture, module
boundaries, and runtime composition. Full policy, durable state, and expanded
execution support are tracked in the design document.

## Future Implementation Notes

- The latency-critical path is `decode -> plan -> simulate -> policy -> build execution request -> dispatch`.
- The current design intentionally avoids a mandatory synchronous database write between request construction and dispatch.
- If we later need stronger crash recovery before dispatch, the preferred intermediate implementation is a very lightweight in-memory journal, WAL, or ring-buffer record rather than a full blocking database write in the hot path.

## Implemented Components

### Ingress

- Flashblocks transaction subscription
- Flashblocks pending log subscription
- Local Base node health checks over IPC
- Normalization into typed ingress events

### Decode

- Uniswap V2-style router decoding
- Uniswap V3 single-hop decoding
- Universal Router V4 pool key extraction
- Aerodrome and Virtuals pattern handling

### Planning

- Token adjacency graph
- Bounded cycle search from configured settlement tokens
- Candidate ranking by simulated net surplus

### Simulation

- Sequential quote simulation over local chain state
- Trigger preflight checks using observed calldata
- Optional post-trigger replay when the node exposes ordered simulation RPCs
- Route-level projected output, gas cost, and net surplus aggregation

### Contracts

- `BaseExecutionRouter` with executor authorization
- Pause controls
- Settlement-token repayment and minimum surplus enforcement
- Structured execution events

## Development

```bash
cargo check
cargo run -p app
```

## Containers

- Engine runtime compose file: [docker-compose.yml](/Users/wenyiyu/wenyi/rust-arbitrage-searcher/docker-compose.yml:1)
- Base node compose file: [node/docker-compose.yml](/Users/wenyiyu/wenyi/rust-arbitrage-searcher/node/docker-compose.yml:1)

The engine container builds the `app` binary from the current Cargo workspace.
The node compose file expects `L1_RPC_URL` and `L1_BEACON_URL` in `.env`.
