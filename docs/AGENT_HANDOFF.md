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
   - Static Data Export registry backed by [`tools/eve-indexer/extract_staticdata.py`](../tools/eve-indexer/extract_staticdata.py) and [`docs/index/sde_seed.json`](../docs/index/sde_seed.json) (`InvType`, `DogmaAttributeType`, `DogmaEffect`, `SolarSystem`, `StationInfo`, `StargateInfo`, `RaceInfo`, `BloodlineInfo`, `AncestryInfo`, `SchoolInfo`, `CloneGradeInfo`, `BlueprintInfo`, `DbuffCollection`, `FighterAbility`, `WreckMapping`, `MutaplasmidDef`, `SpaceComponentDef`, `MissionDef`, `DungeonDef`, `EpicArcDef`, `NodeGraphDef`, `CareerGoalDef`).
4. **[`crates/eve-dogma`](../crates/eve-dogma/src/lib.rs):**
   - 5-stage operator pipeline (`-1..=9`), `!stackable` 0-indexed Gaussian stacking penalty ($S(i) = e^{-(i/2.67)^2}$), 75-opcode `DogmaVmOpcode` expression VM, `ShipFittingEngine`, 3-rack thermodynamics, combat boosters, command burst `dbuff` aggregation, Abyssal mutaplasmid attribute rolling, and analytical capacitor/shield recharge curves.
5. **[`crates/eve-destiny`](../crates/eve-destiny/src/lib.rs):**
   - 1.0 Hz `Ballpark` physics engine with all 13 `DSTBALL_*` modes (`0..=12`), exponential sub-warp kinematics ($\tau = \frac{I \cdot M}{10^6}$), velocity-plane orbit tangents, 4-phase analytical warp integrator (`Aligning`, `Accelerating`, `Cruising`, `Decelerating`, `Stop`), spatial bubble partitioning (`balls_in_bubble`), binary `AddBalls` wire encoder, $2,000\text{ m}$ proximity decloak, turret/missile/Breacher/Entropic combat solver, and CONCORD response timing.
6. **[`crates/eve-db`](../crates/eve-db/src/lib.rs):**
   - Transactional repository with durable JSON snapshot persistence (`save_snapshot_to_file`/`load_snapshot_from_file`), fixed-point `IskCents(i64)` (`10,000` sub-units per `1.0000 ISK`), `InventoryFlag`, optimistic concurrency (`expected_version`), `(character_id, idempotency_key)` scoping, atomic `undock_character` + `rollback_undock`, and complete `M1`–`M8` domain tables.
7. **[`crates/eve-server`](../crates/eve-server/src/main.rs):**
   - Async TCP server listening on port `26001` (Python Client Bridge) and port `26000` (native `machoNet`), per-system 1 Hz actor tasks ([`system_actor.rs`](../crates/eve-server/src/system_actor.rs)), 161-formula engine ([`formulas.rs`](../crates/eve-server/src/formulas.rs)), Dual-Anchor Market Seeder ([`world_seeder.rs`](../crates/eve-server/src/world_seeder.rs)), 3-Tier LOD Living World Ecology ([`living_world.rs`](../crates/eve-server/src/living_world.rs)), all 12 domain service modules ([`services/`](../crates/eve-server/src/services/mod.rs)), authenticated RPC & Quasar router ([`router.rs`](../crates/eve-server/src/router.rs)), integration tests ([`tests/e2e_journeys.rs`](../crates/eve-server/tests/e2e_journeys.rs)), and LOD benchmarks ([`benches/lod_tick_bench.rs`](../crates/eve-server/benches/lod_tick_bench.rs)).
8. **[`client-bridge/evomu_bridge.py`](../client-bridge/evomu_bridge.py):**
   - Python 2.7 / Python 3 compatible multithreaded Client Bridge with dedicated background reader thread, `req_id` correlation, `ServerEvent` demultiplexing, `.bind(object_id)` support, `PublicGatewayShim` (4th Chokepoint for Quasar Protobufs), and `--self-test` verification harness.
9. **[`tools/eve-indexer/`](../tools/eve-indexer/index_server_logic.py):**
   - Automated indexer generating all 9 artifacts in `docs/index/` + `sde_seed.json` (`extract_staticdata.py`), and deterministic CI bijection verifier (`verify_plan_mapping.py --check`).

---

## 3. Verification Commands

```bash
/usr/bin/python3 tools/eve-indexer/verify_plan_mapping.py --check
/usr/bin/python3 client-bridge/evomu_bridge.py --self-test
RUSTFLAGS="-D warnings -D unused" cargo test --workspace --no-fail-fast
```


