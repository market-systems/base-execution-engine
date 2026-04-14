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
- `shadow`: full decision pipeline without broadcasting
- `live`: guarded execution path

## Configuration

Key environment variables:

- `ENGINE_MODE`
- `BASE_IPC_PATH`
- `FLASHBLOCKS_WS_URL`
- `BASE_CHAIN_ID`
- `LOG_LEVEL`
- `MAX_ROUTE_HOPS`
- `MAX_CANDIDATE_ROUTES`
- `FLASHBLOCKS_SUBSCRIBE_TRANSACTIONS`
- `FLASHBLOCKS_SUBSCRIBE_PENDING_LOGS`
- `LIQUIDITY_SEEDS_PATH`

Static liquidity seed data can be stored in
[config/base-liquidity-seeds.example.json](/Users/wenyiyu/wenyi/rust-arbitrage-searcher/config/base-liquidity-seeds.example.json:1).

## Current Status

The current workspace currently includes:

- Flashblocks websocket ingress scaffolding
- Local Base IPC connectivity
- Canonical transaction and intent models
- Multi-protocol calldata decoding
- Liquidity graph primitives and bounded route planning
- Venue-specific quote and swap adapters
- Quote-driven route simulation
- A guarded execution contract interface

The current implementation is intentionally focused on architecture, module
boundaries, and runtime composition. Full policy, durable state, and expanded
execution support are tracked in the design document.

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

- Token-to-token adjacency graph
- Bounded route search with cycle avoidance
- Candidate ranking by projected output and gas

### Simulation

- Sequential quote simulation over local chain state
- Route-level projected output and gas aggregation

### Contracts

- `BaseExecutionRouter` with executor authorization
- Pause controls
- Minimum output enforcement
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
