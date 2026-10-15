# EvoMu — Next-Generation EVE Online Server Emulator in Rust

[![Language: Rust](https://img.shields.io/badge/language-Rust_2021-orange.svg)](https://www.rust-lang.org/)
[![Unsafe: Forbidden](https://img.shields.io/badge/unsafe-forbidden-brightgreen.svg)](#security--safety-invariants)

**EvoMu** is a clean-room, memory-safe EVE Online server emulator written in **Rust**, designed to replace the legacy C++ [`evemu_Crucible`](https://github.com/EvEmu-Project/evemu_Crucible) architecture with an actor- and ECS-driven simulation core and a dual-protocol networking edge.

---

## Key Architectural Highlights

1. **Dual-Mode Client Integration:**
   - **Mode A — Python Client Bridge (`TCP 26001`):** Hooks the EVE client's transport/service chokepoints (`MachoTransport`, `sm.RemoteSvc`, `sm.ScatterEvent`, and `michelle` / `Destiny.Ballpark`) via [`client-bridge/evomu_bridge.py`](./client-bridge/evomu_bridge.py). Eliminates legacy C++ `blue.DBRow` (`PyPackedRow`) and binary `AddBalls` crash surfaces while turning unimplemented client UI calls into self-documenting RPC traces.
   - **Mode B — Native `eveMarshal` + `machoNet` Wire Protocol (`TCP 26000`):** Implements `evemu_Crucible`'s authoritative `EVEMarshalOpcodes.h` binary format (including trailing `save_count` reference tables, `zlib` streams, `PyObjectEx`, and `VersionExchange` handshakes) for unmodified client and MITM proxy interoperability.
2. **Actor-Per-Solar-System Space Physics (`eve-destiny`):**
   - Each active solar system runs as an isolated **1.0 Hz `tokio` actor** with bounded command mailboxes and `broadcast` tick streams.
   - Implements analytical exponential sub-warp kinematics ($\tau = \frac{I \cdot M}{10^6}$) and a 4-phase warp integrator (`Aligning` $\rightarrow$ `Accelerating` $\rightarrow$ `Cruising` $\rightarrow$ `Decelerating` $\rightarrow$ `Stop`) with symmetric short-warp peak speed ($v_{\text{peak}}$) and sub-warp dropout clamping.
3. **Deterministic Dogma Attribute & Effect Engine (`eve-dogma`):**
   - Evaluates EVE's 5-stage operator pipeline (`PreAssign` $\rightarrow$ `PreMul`/`PreDiv` $\rightarrow$ `ModAdd`/`ModSub` $\rightarrow$ `PostMul`/`PostDiv`/`PostPercent` $\rightarrow$ `PostAssign`), Gaussian stacking penalty curve $S(n) = e^{-(n / 2.67)^2}$, and analytical capacitor/shield recharge curves.
4. **Transactional Persistence & Security Invariants (`eve-db`, `eve-server`):**
   - Every crate enforces `#![forbid(unsafe_code)]`.
   - Unauthenticated session defaults, strict ownership checks on character selection, object binding (`MachoBindObject`), and inventory transfers, fixed-point currency (`IskCents`), optimistic concurrency (`expected_version`), and atomic pre-validated undocking with rollback compensation.

---

## Workspace Structure

```text
evomu/
├── Cargo.toml
├── README.md
├── ARCHITECTURE_PROPOSAL.md   # Deep-dive system architecture & design decisions
├── PLAN.md                    # Master execution roadmap & full server-logic indexing blueprint
├── docs/
│   └── AGENT_HANDOFF.md       # Context brief & prompt for autonomous agent handoff
├── crates/
│   ├── eve-proto/             # Dual wire codec: Bridge JSON/MsgPack + binary eveMarshal AST
│   ├── eve-machonet/          # machoNet packet types, MachoAddress, VersionExchange, SessionState
│   ├── eve-sde/               # Static Data Export (SDE) in-memory registry (types, attributes, systems)
│   ├── eve-dogma/             # Dogma modifier graph, stacking penalties, recharge curves
│   ├── eve-destiny/           # 1 Hz Ballpark physics engine, sub-warp & 4-phase warp integrator
│   ├── eve-db/                # Transactional repository, InventoryFlag, IskCents, concurrency control
│   └── eve-server/            # Async TCP server (ports 26000 & 26001), RPC router, solar-system actors
└── client-bridge/
    └── evomu_bridge.py        # Multithreaded Python ServiceManager & Michelle bridge + demo client
```

---

## Quick Start

### 1. Run Tests & Linter
```powershell
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

### 2. Start the `EvoMu` Server
```powershell
cargo run -p eve-server
```
The server starts two listeners:
- `127.0.0.1:26001` — Python Client Bridge (length-prefixed JSON/MessagePack)
- `127.0.0.1:26000` — Legacy `machoNet` / `eveMarshal` binary TCP

### 3. Run the Python Client Bridge Vertical-Slice Demo
In a second terminal:
```powershell
python client-bridge/evomu_bridge.py
```
This exercises the full vertical slice:
1. Authenticates via `auth.Login`
2. Queries characters via `charUnboundMgr.GetCharactersToSelect` and selects `AuraVanguard` (`90000001`)
3. Queries station inventory (`invbroker.List`) and ship Dogma attributes (`dogmaIM.GetShipAttributes`)
4. Atomically undocks from **Jita IV - Moon 4** (`stationSvc.Undock`), binds to the **Jita (`30000142`)** solar-system `beyonce` service, and streams live 1 Hz `DoDestinyUpdate` physics ticks (`GotoDirection` & `WarpTo`).

---

## Documentation & Roadmap

- **[`ARCHITECTURE_PROPOSAL.md`](./ARCHITECTURE_PROPOSAL.md):** Detailed system architecture, protocol design, mathematical models, and resolved cross-model review findings.
- **[`PLAN.md`](./PLAN.md):** Master execution plan and systematic indexing methodology to catalog and implement 100% of EVE Online server services, RPCs, opcodes, Dogma effects, and Destiny mechanics.
- **[`docs/AGENT_HANDOFF.md`](./docs/AGENT_HANDOFF.md):** Structured handoff guide and prompt for the next autonomous engineering agent.
