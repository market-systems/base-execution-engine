# Base Execution Engine

Base Execution Engine is a Rust workspace for low-latency onchain execution on
Base. It is designed around the full production loop: ingest market and chain
signals, build a runtime view of pools, opportunities, balances, and pending
executions, simulate and risk-check candidate routes, submit transactions, and
reconcile realized outcomes.

## Scope

The system is built for execution workloads that depend on both
pre-confirmation signals and canonical chain state. The intended runtime shape
is:

- ingest pre-confirmation transactions and logs
- normalize external payloads into internal events
- maintain an engine-owned runtime view of market state, account state, and
  execution progress
- detect and rank actionable opportunities
- plan routes and simulate expected outcomes
- apply risk checks before any submission
- build execution requests and broadcast transactions
- track receipts and classify final outcomes

## Architecture

```mermaid
flowchart LR
    subgraph INPUT["Input"]
        A["Flashblocks\ntransactions / logs"]
        B["Base node\nrpc / ipc / ws"]
        C["Chain data\nblocks / receipts / reads"]
    end

    subgraph RUNTIME["Runtime"]
        D["ingest\nconnect + decode + normalize"]
        E["decision\nmarkets + opportunities + planning + simulation + risk + build"]
        F["execution\nbalances + submit + reconcile"]
    end

    subgraph SHARED["Shared"]
        G["config"]
        H["types"]
        I["logging + storage"]
    end

    subgraph OUTPUT["Output"]
        J["execution transactions"]
        K["receipts"]
        L["execution records"]
    end

    A --> D
    B --> D
    C --> D

    D --> E --> F
    F --> J
    F --> K
    E --> I
    F --> I
    I --> L

    G --> D
    G --> E
    G --> F
    H --> D
    H --> E
    H --> F

    classDef input fill:#EAF3FF,stroke:#2B6CB0,color:#102A43,stroke-width:1.5px;
    classDef runtime fill:#EDF7ED,stroke:#2F855A,color:#17331F,stroke-width:1.5px;
    classDef shared fill:#FFF7E8,stroke:#C05621,color:#4A2B0F,stroke-width:1.5px;
    classDef output fill:#F6EEFF,stroke:#805AD5,color:#2D1F55,stroke-width:1.5px;

    class A,B,C input;
    class D,E,F runtime;
    class G,H,I shared;
    class J,K,L output;
```

## Runtime Flow

```text
input event
-> ingest
-> market/account/execution update
-> opportunity detection
-> route planning
-> simulation
-> risk decision
-> execution build
-> transaction submission
-> receipt reconciliation
-> recorded outcome
```

## Workspace Layout

```text
crates/
  app/        runtime entrypoint and wiring
  config/     typed configuration and validation
  types/      shared domain types and errors
  ingest/     connectors, decoding, normalization, protocol metadata
  decision/   markets, opportunities, planner, simulator, risk, builder
  execution/  balances, nonce handling, submission, reconciliation, outcomes
contracts/
  BaseExecutionRouter.sol
```

## Crate Responsibilities

### `app`

- starts the runtime
- loads configuration
- initializes logging
- wires dependencies across crates
- owns process lifecycle

### `config`

- environment loading
- typed config structs
- validation for limits, modes, and safety settings

### `types`

- shared identifiers and enums
- observed events and normalized intents
- opportunity and planning types
- simulation and risk result types
- execution request, attempt, and receipt types

### `ingest`

- external connectivity
- calldata and log decoding
- event normalization
- protocol and venue metadata needed during ingest

### `decision`

- market, account, and execution state needed for decision-making
- opportunity detection
- route planning
- simulation
- risk checks
- execution request building

### `execution`

- balances and allowances
- nonce handling
- transaction submission
- receipt tracking
- terminal outcome classification
