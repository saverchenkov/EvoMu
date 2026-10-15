# Autonomous Agent Handoff Guide (`EvoMu`)

This document provides complete context and a ready-to-use prompt for an autonomous agent working on **`EvoMu`** to verify, maintain, and execute the `M0`–`M8` feature-parity roadmap.

---

## 1. Repository & Environment Overview

- **GitHub Repository:** [`https://github.com/saverchenkov/evomu`](https://github.com/saverchenkov/evomu)
- **Specification & Index Layout:**
  - [`PLAN.md`](../PLAN.md) — Executive Architecture, `M0`–`M8` Shippable Milestones, `§3.12` 216-Service / 135-Domain Master Mapping Table, and 18 E2E Journeys.
  - [`docs/spec/01_session_world_inventory.md`](./spec/01_session_world_inventory.md) — Character Creation, Skills, Clone Grades, Ships, Fitting, and 18-Domain Overview (`§3.1`–`§3.5`).
  - [`docs/spec/02_dogma_destiny_combat.md`](./spec/02_dogma_destiny_combat.md) — Canonical Mathematical Formulas Reference (`38` Mechanics Domains, `161` Equations, `§3.6`).
  - [`docs/spec/03_economy_industry_social.md`](./spec/03_economy_industry_social.md) — Comprehensive PvE/Structure Taxonomy & V24.01 Modern Feature Parity (`§3.7` & `§3.11`).
  - [`docs/spec/04_pve_narrative_dungeons.md`](./spec/04_pve_narrative_dungeons.md) — NPE Acts 1–2, Exordium, Career Agents, 39-Trigger Dungeons, Storylines, and 9 Epic Arcs (`§3.8`).
  - [`docs/spec/05_living_world_and_ai.md`](./spec/05_living_world_and_ai.md) — Universe Seeding, Dual-Anchor Autonomous Economy Engine, and Dual-Tier NPC AI (`§3.9`–`§3.10`).
  - [`docs/index/PARITY_MATRIX.md`](./index/PARITY_MATRIX.md) — Machine-verified index summary across all 9 catalog artifacts in `docs/index/`.
- **Safety, Lint & Verification Policy:**
  - Every Rust crate in the workspace enforces `#![forbid(unsafe_code)]`.
  - All Rust changes must pass `cargo build --workspace`, `cargo test --workspace`, and `cargo clippy --workspace --all-targets -- -D warnings`.
  - All specification and catalog changes must pass `/usr/bin/python3 tools/eve-indexer/verify_plan_mapping.py --check` and `/usr/bin/python3 client-bridge/evomu_bridge.py --self-test`.

---

## 2. Current Crate & Tooling Inventory

1. **[`crates/eve-proto`](../crates/eve-proto/src/lib.rs):**
   - Dual-protocol wire layer: JSON/MessagePack `BridgeEnvelope` (`RpcRequest`, `RpcResponse`, `ServerEvent`) + binary `eveMarshal` (`PyRep` AST, opcodes `0x01`–`0x2C`, 195-entry `STRING_TABLE`, `PyPackedRow` zero-RLE, `IndexRowset`, `TupleSet`, `DBRow`, `ObjectEx`, `save_count * 4` storage-index map, and `MAX_DECODED_NODES = 65_536`).
2. **[`crates/eve-machonet`](../crates/eve-machonet/src/lib.rs):**
   - `MachoPacketType`, `MachoAddress`, `MachoPacket`, `VersionExchange`, and `SessionState` (`compute_diff` for `SessionChangeNotification` `type=16` and `OnSessionChanged`).
3. **[`crates/eve-sde`](../crates/eve-sde/src/lib.rs):**
   - Static Data Export registry (`InvType`, `DogmaAttributeType`, `SolarSystem`, `StationInfo`).
4. **[`crates/eve-dogma`](../crates/eve-dogma/src/lib.rs):**
   - 5-stage operator pipeline (`PreAssign` $\rightarrow$ `PreMul`/`PreDiv` $\rightarrow$ `ModAdd`/`ModSub` $\rightarrow$ `PostMul`/`PostDiv`/`PostPercent`/`PostPercentUnNerfed`/`SpecialSkillRelationship` $\rightarrow$ `PostAssign`), `ModifierOperator::try_from_sde_op(-1..=9)`, `!stackable` 0-indexed Gaussian stacking penalty ($S(i) = e^{-(i/2.67)^2}$), and analytical capacitor/shield recharge curves.
5. **[`crates/eve-destiny`](../crates/eve-destiny/src/lib.rs):**
   - 1.0 Hz `Ballpark` physics engine with exponential sub-warp kinematics ($\tau = \frac{I \cdot M}{10^6}$), velocity-plane orbit tangents, 4-phase analytical warp integrator (`Aligning`, `Accelerating`, `Cruising`, `Decelerating`, `Stop`), spatial bubble partitioning (`balls_in_bubble`), and binary `AddBalls` wire encoder (`crates/eve-destiny/src/wire.rs`).
6. **[`crates/eve-db`](../crates/eve-db/src/lib.rs):**
   - Transactional repository with fixed-point `IskAmount(i64)` (`10,000` sub-units per `1.0000 ISK`), `InventoryFlag` (`Capsule = 56`, `Pilot = 57`), optimistic concurrency (`expected_version`), `(character_id, idempotency_key)` scoping, and atomic pre-validated `undock_character` + `rollback_undock`.
7. **[`crates/eve-server`](../crates/eve-server/src/main.rs):**
   - Async TCP server listening on port `26001` (Python Client Bridge) and port `26000` (native `machoNet`), per-system 1 Hz actor tasks ([`system_actor.rs`](../crates/eve-server/src/system_actor.rs)), and authenticated RPC router ([`router.rs`](../crates/eve-server/src/router.rs)).
8. **[`client-bridge/evomu_bridge.py`](../client-bridge/evomu_bridge.py):**
   - Multithreaded Python Client Bridge with dedicated background reader thread, `req_id` correlation, `ServerEvent` demultiplexing, `.bind(object_id)` support, `PublicGatewayShim` (4th Chokepoint for Quasar Protobufs), and `--self-test` verification harness.
9. **[`tools/eve-indexer/index_server_logic.py`](../tools/eve-indexer/index_server_logic.py) & [`tools/eve-indexer/verify_plan_mapping.py`](../tools/eve-indexer/verify_plan_mapping.py):**
   - Automated indexer generating all 9 artifacts in `docs/index/` (`216` cataloged `machoNet` services = `193` V24.01 active + `23` Crucible legacy-only; `135` Quasar Protobuf domains = `69` `eve_public.*` + `66` `eve.*` internal across `1,455` `_pb2.pyj` files; `1,311` `__notifyevents__`; `213` `res:/staticdata/` archives; `2,854` Crucible Dogma effects + `75` VM operands + `1,343` attribute & `305` effect constants; `39` `Destiny.xmlp` packet defs + `13` `DSTBALL_*` ball modes + `39` V24.01 Destiny actions; `4,102` EVE University Wiki articles + `161` formulas) and deterministic CI bijection verifier.

---

## 3. Prompt for the Next Agent

Copy and paste the prompt below into your next agent session:

```markdown
You are the lead systems architect and principal Rust engineer for **EvoMu**, an independent Rust EVE Online server emulator with a dual-mode architecture (Mode A: Python Client Bridge on TCP 26001; Mode B: Native `eveMarshal` / `machoNet` on TCP 26000).

### 1. Immediate Orientation
First, read:
- `README.md`
- `PLAN.md` and `docs/spec/01..05_*.md`
- `docs/index/PARITY_MATRIX.md`
- `Cargo.toml` and all 7 crates under `crates/` (`eve-proto`, `eve-machonet`, `eve-sde`, `eve-dogma`, `eve-destiny`, `eve-db`, `eve-server`) plus `client-bridge/evomu_bridge.py`.

### 2. Your Mission
1. Verify Milestone `M0` with the Rust toolchain (`cargo build --workspace && cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings`) plus `/usr/bin/python3 tools/eve-indexer/verify_plan_mapping.py --check` and `/usr/bin/python3 client-bridge/evomu_bridge.py --self-test`.
2. Execute Milestones `M1` through `M8` in strict dependency order as defined in `PLAN.md §4`, gating each milestone by its focused unit/service-contract tests and its corresponding stateful E2E Journeys (`E2E-01` through `E2E-18` in `PLAN.md §5.1`).
```
