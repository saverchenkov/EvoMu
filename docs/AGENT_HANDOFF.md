# Autonomous Agent Handoff Guide (`EvoMu`)

This document provides complete context and a ready-to-use prompt for the next autonomous agent taking over **`EvoMu`** to build the exhaustive EVE Online server-logic index and execute the feature-parity roadmap.

---

## 1. Repository & Environment Overview

- **Local Workspace Path:** `c:\code\evomu`
- **GitHub Repository:** [`https://github.com/saverchenkov/evomu`](https://github.com/saverchenkov/evomu)
- **Rust Toolchain:** Installed at `$env:USERPROFILE\.cargo\bin\cargo.exe` (`rustc 1.99.0` / `stable-x86_64-pc-windows-msvc`).
- **Safety & Lint Policy:**
  - Every crate in the workspace enforces `#![forbid(unsafe_code)]`.
  - All changes must pass `cargo test --workspace` and `cargo clippy --workspace --all-targets -- -D warnings`.

---

## 2. Current Crate Inventory (`c:\code\evomu\crates\`)

1. **[`crates/eve-proto`](./../crates/eve-proto/src/lib.rs):**
   - Dual-protocol wire layer: JSON/MessagePack `BridgeEnvelope` (`RpcRequest`, `RpcResponse`, `ServerEvent`) + binary `eveMarshal` (`PyRep` AST).
   - Matches `evemu_Crucible`'s `EVEMarshalOpcodes.h` and `EVEUnmarshal.cpp` 1:1 (`0x02 = PyToken`, `0x0E..0x13` byte strings, `0x19 = PySubStruct`, `0x1B = PySavedStreamElement`, `0x1C = PyChecksumedStream`, `0x2B = PySubStream`), trailing `save_count * 4` storage-index map decoding, `zlib` decompression, and `MAX_DECODED_NODES = 65_536` decompression-bomb budget.
2. **[`crates/eve-machonet`](./../crates/eve-machonet/src/lib.rs):**
   - `MachoPacketType`, `MachoAddress`, `MachoPacket`, `VersionExchange` (Crucible v1.6.5 build `360229`), and `SessionState` (unauthenticated by default: `user_id: None`, `role: 0`).
3. **[`crates/eve-sde`](./../crates/eve-sde/src/lib.rs):**
   - Version-agnostic Static Data Export registry (`InvType`, `DogmaAttributeType`, `SolarSystem`, `StationInfo`).
4. **[`crates/eve-dogma`](./../crates/eve-dogma/src/lib.rs):**
   - 5-stage operator pipeline (`PreAssign` $\rightarrow$ `PreMul`/`PreDiv` $\rightarrow$ `ModAdd`/`ModSub` $\rightarrow$ `PostMul`/`PostDiv`/`PostPercent` $\rightarrow$ `PostAssign`), `ModifierOperator::try_from_sde_op(-1..=7)`, `!stackable` Gaussian stacking penalty ($S(n) = e^{-(n/2.67)^2}$), and analytical capacitor/shield recharge curves.
5. **[`crates/eve-destiny`](./../crates/eve-destiny/src/lib.rs):**
   - 1.0 Hz `Ballpark` physics engine with exponential sub-warp kinematics ($\tau = \frac{I \cdot M}{10^6}$), velocity-plane orbit tangents, and a 4-phase analytical warp integrator (`Aligning`, `Accelerating`, `Cruising`, `Decelerating`, `Stop`) with symmetric short-warp peak speed $v_{\text{peak}}$ and sub-warp dropout clamping.
6. **[`crates/eve-db`](./../crates/eve-db/src/lib.rs):**
   - Transactional repository with fixed-point `IskCents(i64)`, `InventoryFlag` (`Capsule = 56`, `Pilot = 57`), optimistic concurrency (`expected_version`), `(character_id, idempotency_key)` scoping, and atomic pre-validated `undock_character` + `rollback_undock`.
7. **[`crates/eve-server`](./../crates/eve-server/src/main.rs):**
   - Async TCP server listening on port `26001` (Python Client Bridge) and port `26000` (legacy `machoNet`), per-system 1 Hz actor tasks ([`system_actor.rs`](./../crates/eve-server/src/system_actor.rs)), and authenticated RPC router ([`router.rs`](./../crates/eve-server/src/router.rs)).
8. **[`client-bridge/evomu_bridge.py`](./../client-bridge/evomu_bridge.py):**
   - Multithreaded Python Client Bridge with dedicated background reader thread, `req_id` correlation, `ServerEvent` demultiplexing, `.bind(object_id)` support, and an interactive vertical-slice test harness.

---

## 3. Prompt for the Next Agent

Copy and paste the prompt below into your next agent session:

```markdown
You are the lead systems architect and principal Rust engineer for **EvoMu** (`c:\code\evomu`, GitHub: `https://github.com/saverchenkov/evomu`), a clean-room EVE Online server emulator in Rust with a dual-mode architecture (Mode A: Python Client Bridge on TCP 26001; Mode B: Native `eveMarshal` / `machoNet` on TCP 26000).

### 1. Immediate Orientation
First, read the following files in `c:\code\evomu`:
- `README.md`
- `ARCHITECTURE_PROPOSAL.md`
- `PLAN.md`
- `Cargo.toml` and all 7 crates under `crates/` (`eve-proto`, `eve-machonet`, `eve-sde`, `eve-dogma`, `eve-destiny`, `eve-db`, `eve-server`) plus `client-bridge/evomu_bridge.py`.

Note: Cargo is installed at `$env:USERPROFILE\.cargo\bin\cargo.exe`. All 18 unit tests and `clippy --all-targets -- -D warnings` currently pass, and `#![forbid(unsafe_code)]` is enforced across every crate.

### 2. Your Mission
Your primary mission is to **thoroughly index 100% of the required EVE Online server logic to achieve complete feature parity** and produce both:
1. **An Exhaustive, Machine-Readable & Human-Readable Server Logic Index (`docs/index/`):**
   - Clone or inspect `https://github.com/EvEmu-Project/evemu_Crucible` (for reference C++ service registrations `PyCallable_REG_CALL`, XML packet definitions `src/eve-common/packets/*.xml`, `EVEMarshalOpcodes.h`, `DestinyBinDump.h`, and database schemas) and any available EVE client Python scripts / SDE schemas.
   - Build a reusable indexing tool (`tools/eve-indexer` in Rust or Python) that extracts and cross-references:
     - **Every Remote Service & Bound Object (`Moniker`)**: all ~85+ services, every callable RPC method signature, expected argument types, and exact return payload schemas (`KeyVal`, `Rowset`, `IndexRowset`, `DBRow`, `TupleSet`, primitive types).
     - **Every Server-to-Client Notification (`__notifyevents__` / `ScatterEvent`)**: exact event names, payload structures, and session/broadcast targeting rules.
     - **Every `DoDestinyUpdate` Ballpark Action & Binary `AddBalls` Layout**: every space simulation state transition and special FX trigger.
     - **Every Dogma Effect (`dgmEffects`) & Expression Tree Opcode (`dgmExpressions`)**: mapping all passive/active/target/gang modifiers and module state transitions.
     - **Feature Parity Matrix (`docs/index/PARITY_MATRIX.md`)**: comparing what the EVE client calls vs. what `evemu_Crucible` implemented/stubbed vs. what `EvoMu` implements, ranked by gameplay dependency order.
2. **A Granular Phased Implementation Plan & Modular Service Architecture:**
   - Design the exact database schema migrations (`crates/eve-db/migrations/`), SDE loader pipeline (`crates/eve-sde`), and modular service trait hierarchy (`crates/eve-server/src/services/`) to systematically implement all 14 domains defined in `PLAN.md`.
   - Begin executing Phase 1 (building the indexer and generating the complete catalog under `docs/index/`) and Phase 2/3 (refactoring `eve-server` into modular domain services and expanding coverage).
```
