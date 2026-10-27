# EvoMu action plan: 100% live-server parity (machoNet + Quasar + ESI)

**Audience:** the EvoMu coding agent.
**Status baseline:** commit `479e633` (27/27 tests pass). The feature-gap pass of 2026-10-17 found 4 of 60 tasks done or mostly done, 28 partial and 27 not started. The full mining system was added as Phase 5M (MN-01 … MN-18). Parity at that point was about 6% of client RPC methods.
**This is the canonical plan.** Its pinned copy lives at `.agents/skills/evomu-adversarial-review/references/action_plan.md`. Don't rewrite, renumber or drop tasks. Status is generated into `docs/ACTION_PLAN_STATUS.md` by pre-flight. Descoping a task requires the user's approval.
**Sources:**
- Crucible comparison (`crucible_vs_evomu.md`, summarized below).
- Review findings (`.agents/skills/evomu-adversarial-review/references/open_findings.md` and `.evomu-reviews/latest.md`).
- Reference implementation: `~/ai/evemu_Crucible`.
- The real V24.01 client and its static data: `~/Library/Application Support/EVE Online/SharedCache/` (`tq/EVE.app`, `ResFiles/`, `index_tranquility.txt`).

## Goal and definition of done
- **Goal.** 100% feature parity with the live EVE Online server for the V24.01 client. This covers:
  - the classic machoNet RPC, notification and destiny surface;
  - **all Quasar / public-gateway backend services**;
  - **ESI**, including the SSO/OAuth2 it depends on.
- **The parity ledger is the scope.** `.agents/skills/evomu-adversarial-review/references/parity/ledger.json` is pinned and read-only for you. It has one row per:
  - client RPC method;
  - notification;
  - destiny action;
  - Quasar proto;
  - ESI operation;
  - client static-data file;
  - plus all dogma effects, curated game-mechanic rows (`feat:*`), plan tasks and real-client journeys.

  The gap auditor appends rows over time (`auditor_rows.json`). A row is closed only when a test tagged with its ID (`// parity: <row id>`) passes and the implementation check passes. See `scripts/parity/parity_status.py`.
- **Done = `scripts/final_gate.py` exits 0 on the current tree.** That requires all of the following on the same tree:
  - 100% of ledger rows closed, with no rows flagged fake;
  - the claims review is `PASS` and the gap audit is `CLEAN`;
  - workspace line coverage ≥ 90%;
  - mutation score ≥ 85% on every crate;
  - every real-client journey passes, with zero `UnimplementedRPC`, panics or client tracebacks in the logs.
- **No interim human review.** Run as an unbounded `/goal` loop. Don't stop, don't ask the user to review or test, and don't report "done" before the final gate passes. Stop only for a true external blocker (credentials, hardware, a legal or licensing question). If that happens, write it in `docs/BLOCKERS.md` and keep working on anything that isn't blocked.

---

## 0. Ground rules (apply to every task)
1. **Review gate.** Each workstream ends with the `evomu-adversarial-review` skill (see `AGENTS.md`). A phase is done only when its exit gate is met **and** the review verdict is `PASS` or `PASS_WITH_ISSUES` with no P0.
2. **Use Crucible as a reference only.** Crucible is LGPL; EvoMu is MIT. Read Crucible to understand algorithms, state machines and data needs, then **re-implement in Rust**. Never copy code, comments or SQL verbatim. Cite the Crucible file you used in the PR or commit message.
3. **Target V24, not 2011.** Crucible speaks the Nov-2011 client (7.31 / build 360229). Method names, argument layouts and result shapes must match the **V24 client call sites** in `docs/index/rpc_service_catalog.json` and the client bytecode. Don't trust Crucible's packet shapes.
4. **Unhandled beats wrong.** Every client method either implements its real semantics or returns `UnimplementedRPC`. That rules out:
   - shared bodies for unrelated methods;
   - echo or constant replies;
   - query methods (`Get*`) that mutate state;
   - client-supplied trust (destinations, damage, skill levels, positions, mission IDs).
5. **Transactions.** Every mutating handler uses `WorldRepository::try_with_mut_snapshot` and returns `Err` before *or* after mutating. Never mutate and then return `Err` inside `with_mut_snapshot`.
6. **No fixtures in production paths.** Test NPCs, wormholes, ESS banks, hub balances and similar live in test setup or seed data with real positions. They never belong in `SystemActor::spawn`, jump handlers, or `with_demo_seed()` used by `main`.
7. **Tests prove behaviour.**
   - Every task adds tests under `crates/<crate>/tests/` or `crates/eve-server/tests/<area>.rs` that would fail without the change.
   - Every test that closes a ledger row carries `// parity: <row id>[, <row id>…]` in the comment block above it. No wildcards. Data-driven tests over the static game data (`sde:*`, `effect:*`) may attest IDs at runtime through `EVOMU_PARITY_ATTEST_DIR` (see T-1).
   - Tests assert real game values (wiki/SDE/client-derived), never tautologies. Mutation testing at the final gate catches weak tests.
   - Tests use real SDE data once Phase 1 lands.
   - `cargo test --workspace` must actually run. `cargo check` is not verification.
   - Expect thousands of tests: at least one per ledger row, plus property tests and data-driven sweeps.
8. **Docs reflect reality.** Don't use "Implemented & Verified" in `PLAN.md`/`README.md` unless the feature has a passing test **and** a non-FAIL review. Use the labels `Not started`, `In progress`, `Implemented (review: <verdict>, <date>)`.
9. **Small, reviewable increments.** Aim for one workstream (a few task IDs) per commit series, each followed by a review run. Put task IDs in commit messages and in the claims file you pass to the reviewer.

---

## Phase 0: Stabilize (close the current FAIL)
**Goal:** a clean `PASS_WITH_ISSUES` baseline before adding features.

| ID | Task | Done when |
|---|---|---|
| S-1 | **Reprocessing mints Tritanium from anything** (`industry_pi.rs:41-62`) | Only types with SDE `typeMaterials` can be reprocessed. Output = materials × batch × yield. Tritanium input is rejected. Test: reprocessing Tritanium errors; reprocessing Veldspar yields the SDE material list. |
| S-2 | **fighterMgr remote damage** (`combat_crimewatch.rs:640-678`) | Return `UnimplementedRPC` until Phase 5 fighters land. Test: the call errors, and no ball HP changes. |
| S-3 | **CmdJumpToStructureBeacon teleport** (`space_destiny.rs:658-709`) | `UnimplementedRPC` until capital jumps exist (cyno, LY range, fuel, ACL). |
| S-4 | **Jump clone remote dock / no cooldown** (`character_skills.rs:404-505`) | Install requires being docked at a station/structure with a clone bay. CloneJump requires being docked, enforces the cooldown, changes system/station/ship correctly, and swaps implants. Otherwise `UnimplementedRPC`. Separate handlers for each `jumpCloneSvc` method. |
| S-5 | **Fixtures in production** (gate-arrival NPC, global 20 rats, K162, ESS banks) | Removed from `system_actor.rs`, `space_destiny.rs` and seeded ESS balances. Tests create their own NPCs through a test-only helper. |
| S-6 | **Client-driven wormhole/probe spawn** (`space_destiny.rs:736-768`) | `RequestScans` returns `UnimplementedRPC` until Phase 8 signatures. |
| S-7 | **Remote mutations** (`AttackStructure`, `ClaimSovereignty`, `CaptureComplex`, PI writers) | Proximity, ownership and state-machine checks, or `UnimplementedRPC`. Every PI read method is read-only. |
| S-8 | **Remaining with_mut_snapshot mutators** (52 call sites) | Every handler that can return `Err` uses `try_with_mut_snapshot`. CI grep check: no `return Err` inside a `with_mut_snapshot` closure. |
| S-9 | **GM role** (`auth_cache.rs:233`, demo accounts role 0x03) | Use CCP role constants (`ROLE_GML`/`ROLE_ADMIN` etc. from the client consts). Demo/player accounts get no GM role. Admin accounts come from config. |
| S-10 | **Docs honesty** (`PLAN.md:24-139`, README) | Status labels follow rule 8, and the "generated from .static SQLite" claim is removed until Phase 1. |
| S-11 | **Seed lineage data** (duplicate ancestry, shifted IDs) | Fixed by Phase 1 (D-1). Until then, test-only data is clearly labeled. |

**Exit gate:** review `PASS` or `PASS_WITH_ISSUES`, no P0, `cargo test --workspace` green.

---

## Phase 1: Data and persistence foundation (blocks almost everything)
**Crucible reference:** `src/eve-server/StaticDataMgr.cpp:139-2190` lists the SDE tables a server needs. `sql/base/*.sql.gz` is the reference persistent schema (284 tables). Reference only.

| ID | Task | Done when |
|---|---|---|
| D-1 | **Real SDE importer** (`tools/eve-indexer/extract_staticdata.py` → new `tools/sde-import`) | Reads the client's `.static` SQLite3 archives with their **actual schemas** (inspect them with `sqlite3 .schema`), and CCP's public SDE export for data that only exists in `.fsdbinary`. Produces a versioned on-disk SDE database (SQLite) with types, groups, categories, dogma attributes/effects/`modifierInfo`, type materials, blueprints, all solar systems/constellations/regions, celestials (stars, planets, moons, belts), stargates with positions and destinations, stations, NPC corps/factions, agents, schools/races/bloodlines/ancestries, wormhole types and NPC spawn groups. The importer prints per-table counts. No hand-written literals. |
| D-2 | **eve-sde loads at runtime** | `SdeRegistry::open(path)` replaces `include_str!` seeding. Startup fails clearly if the DB is missing. Tests prove ≥5,000 solar systems, correct gate pairs for Jita↔Perimeter, and correct school→race/corp mapping. |
| D-3 | **Durable world DB** | SQLite via `rusqlite` or `sqlx` with migrations for accounts, characters, skills, queue, items, orders, journal, contracts, jobs, missions, standings, mail, fleets, corps, bookmarks and structures. `WorldRepository` writes through transactionally per request; no whole-world clone. Test: restart the server and the state survives. Concurrent writes from two sessions don't lose updates. |
| D-4 | **Accounts** | Salted password hashes (argon2), bans, an online lock, account creation through admin config. Remove plaintext demo secrets from production builds. |
| D-5 | **Config file** | `evomu.toml`: ports, bind address, DB paths, admin accounts, feature flags. |
| D-6 | **No gameplay state outside the world DB & no protocol split-brain** (user-added 2026-10-22; Audit A + OF-38) | Every piece of player-visible game state lives in SQLite and goes through the transactional snapshot path (`try_with_mut_snapshot`), so it rolls back on error and survives a restart. Specifically:<br>1. **All 61 unsynced `WorldStateSnapshot` fields** get real SQLite tables, load on startup, and roll back in `rollback_dirty_from_sqlite` (including `owner_notes`, `module_online_states`, `chat_channels`, `bookmark_folders`, `moon_extractions`, `auto_moon_miners`, `sov_campaign_scores`, `fw_system_occupations`, `war_lifecycle_meta`, `corp_titles`, `corp_member_titles`, `corp_role_assignments`, `corp_shareholders`, `char_research_agents`, `char_scoped_roles`, `corp_wallet_journals`, `market_order_meta`, `planetary_colony_layouts`).<br>2. **Zero process-global gameplay `static` stores or counters:** migrate all ~25 `static` Mutex/RwLock stores (`CORP_GOV`, `WAR_STATE`, `ALLIANCE_WAR_STATE`, `COMBAT_RUNTIME`, `PVE_EXPLORATION_STATE`, `PVE_SPECIAL_STATE`, `INSURGENCY_RUNTIME`, `SOV_RUNTIME`, `FW_RUNTIME`, `CONTAINER_SHIPS STATE`, `SEASON_STATES`, `BM_SCAN_STATE`, `EMP_HISTORY`, `STRUCTURE_DOCKING_ACL_DENIED`) and all `AtomicI64` ID counters (`NEXT_ALLIANCE_ID`, `NEXT_CORP_ID`, etc., persisted in `world_meta` so IDs never collide after restart).<br>3. **Eliminate protocol split-brain:** MachoNet, ESI (`EsiRuntimeState`), and Quasar (`IDENTITY_STORE`) plus `ServerContext.social_extras` and `notification_records` read/write the same SQLite tables for contacts, mail read/labels/trash, notifications, PLEX vault, and custom item names.<br>4. **No half-persisted transactions or stuck buffs:** player trade escrow persists atomically with ISK changes; active module cycle buffs (Siege, Bastion, RAH, rack heat) never overwrite persisted base item attributes across crashes/restarts; Upwell structure service `module_online_states` survive restart. |

**Exit gate:** all existing tests pass against the real SDE (fix tests that relied on fake coordinates or IDs), and the review is non-FAIL.

---

## Phase T: Test and acceptance infrastructure (start right after D-1/D-2; needed to close any ledger row)

| ID | Task | Done when |
|---|---|---|
| T-1 | **Parity tagging and attestation.** Write a small test-support crate (e.g. `eve-testkit`) with `attest(&[row_ids])`. It appends `{"test": <fn name>, "rows": [...]}` JSON lines to `$EVOMU_PARITY_ATTEST_DIR/<crate>.jsonl` (no-op when unset). Document the `// parity: <row id>` tag convention. Create `docs/parity/triage.json` for NA claims, each with a disposition and `path:line` evidence. | `parity_status.py` counts a tagged test and an attested data-driven test as DONE on a real row (e.g. one `sde:` file and one `rpc:` method). |
| T-2 | **Data-driven sweeps over the real static data.** For every client static-data file, assert that the importer's record count equals the client file's and spot-check values. For every type, effect, blueprint, reprocessing/compression recipe, dungeon and agent, run the generic engine and assert invariants plus wiki-derived values. Attest `sde:*` and `effect:<id>`. | Every `sde:*` row and `effect:*` (2,854 effects) is attested by passing tests. Counts match the client files (md5 recorded). |
| T-3 | **Property and fuzz tests.** Use `proptest` round-trips for every marshal opcode, PackedRow/DBRow, the destiny encoder, protobuf gateway frames and `IskCents` arithmetic. Add `cargo fuzz` targets for every decoder exposed to the network. | ≥10k cases per property in the default test profile. Each fuzz target runs 1h with no crash; the corpus is committed. |
| T-4 | **Golden wire tests against the client's own codecs.** Through the in-client bridge, round-trip EvoMu-encoded marshal, destiny and protobuf payloads through the client's own decoders, and client-encoded payloads through EvoMu's. Commit the captured fixtures. | Golden suite covers every opcode, every destiny action and a sample of every Quasar package. |
| T-5 | **Coverage and mutation tooling.** Install `cargo-llvm-cov` and `cargo-mutants`. Run `cargo mutants --in-diff` on every task's diff before review. | Both tools run on the workspace. The per-task mutation score on changed code is ≥85% before each review. |
| T-6 | **Real-client harness.** `tools/client-harness/run_journeys --out DIR --journeys FILE`: start EvoMu, launch the real V24.01 client (`SharedCache/tq/EVE.app`) pointed at it (emulated SSO/launcher token), and drive each journey in `references/parity/client_journeys.json` through in-client automation (bridge) or UI automation. Collect server and client logs and screenshots. Write `DIR/result.json` per the contract in `final_gate.py`. | The `launch_login` journey passes end to end on the real client. The harness supports two concurrent clients. |
| T-7 | **Time control.** Add an injectable server clock and time acceleration so skill training, industry, PI, timers, respawns and downtime can be exercised in tests and journeys without wall-clock waits. | The skill queue, an industry job, a PI extraction cycle and belt respawn each complete under accelerated time in tests. |
| T-8 | **Invariant checking with randomized play** (user-added 2026-10-22) | A seeded generator drives random but valid sequences of client RPCs from several concurrent characters (trade, fit, fly, fight, mine, industry, contracts, corp, structures). After every step it checks invariants: **conservation** (ISK, items and SP change only through defined sources and sinks such as NPC bounties, market seeding and injectors; anything else is minting or loss); **single location** (every item in exactly one place, no negative quantities); **authority** (no character changes another character's or corp's state without the required role or relationship); **monotonic timers**; **persistence** (the world DB equals in-memory state after each committed step). Runs in CI with a fixed seed plus a nightly random seed. Every violation found becomes a regression test. Done when it runs at least 10,000 steps across 4 characters with no violations, and when re-introducing any minting exploit that a review caught on 2026-10-22 makes it fail. |
| T-9 | **Crash, disconnect and restart harness** (user-added 2026-10-22) | Scripted multi-client sessions where the harness kills the server process (SIGKILL) or drops a client connection at random steps, including mid-warp, mid-jump, mid-dock, mid-trade, mid-contract and mid-industry-delivery. After the server restarts it compares the world with the last committed state and checks the lifecycle rules from SP-6, D-6 and the lifecycle matrix (no duplicated or lost items or ISK, no ghost ships, sessions resume correctly). Done when every scenario passes across 20 seeded kill points each. |
| T-10 | **Client-expectation oracle** (user-added 2026-10-22) | A deterministic extractor over the V24.01 client bytecode (`code.ccp`; reuse the `rpc_names.py` loader). For every server-facing interaction it records what the client expects: RPC call sites with the argument count and order sent, the return value's shape as the client unpacks it (tuple arity, keyval/dict keys read, rowset columns); every `__notifyevents__` handler with the arguments it reads; every client-side timer or constant that reflects a server rule (logoff, session-change, warp, jump-fatigue and similar durations); and the order the client expects for session changes and notifications. The extractor writes `references/parity/sources/client_expectations.json`, metadata only with no client code, pinned by the supervisor. Tests can then assert server replies and notifications against it mechanically, and the reviewer and gap auditor use it as ground truth. Done when the catalog covers every `rpc:` and `notify:` ledger row, a test helper validates server output against it, and spot checks of 20 rows match manual reading of the client. |
| T-11 | **Destiny differential physics harness & golden vector capture** (user-added 2026-10-23) | A differential test harness comparing EvoMu's `crates/eve-destiny` against CCP's native `bin64/_destiny.so` (Mach-O ARM64/x86_64 in `EVE.app`). **Exit criteria (no Python mirror models):**<br>1. **Bundled CCP runtime execution:** `tools/generate_golden_fixtures.py` (or its helper script) must execute CCP's compiled `bin64/_destiny.so` inside the client's bundled Stackless Python 2.7 environment (`bin64/libpython2.7.dylib` + `bin64/blue.so` or via `client-bridge/evomu_bridge.py` in `tools/client-harness/run_real_client.py` where `import destiny` is loaded) and record raw trajectory checkpoints directly from CCP's C++ `destiny.Ballpark` simulation. Computing trajectories with a hand-written Python re-implementation of `eve-destiny` formulas (`DERIVED_REFERENCE`) is forbidden and must be rejected by the reviewer.<br>2. **5 flight regimes & tolerances:** Evaluates identical initial states across submarine drag/acceleration, alignment time, orbital velocity plane stability (`DSTBALL_ORBIT`), 3-phase warp S-curve kinematics, and elastic sphere collisions, asserting position delta $\|\Delta P\| \le 1\text{mm}$, velocity delta $\|\Delta V\| \le 1\text{mm/s}$, and matching mode transitions.<br>3. **Client-version keying & `--diff` CLI:** Dumps `crates/eve-destiny/tests/fixtures/ccp_golden_vectors.json` keyed by `<codename>-<build>` (`"V24.01-3624425"`, parsed from `start.ini` and `carbon.json` `carbon_destiny` + `_destiny.so` SHA-256, with `"oracle_source": "CCP_BUNDLED_RUNTIME"`) and supports `python3 tools/generate_golden_fixtures.py --diff <old_ver> <new_ver>` so behavior diffs across future client versions can be compared without requiring client binaries in CI. |
| T-12 | **Dogma differential attribute harness & golden fit capture** (user-added 2026-10-23) | A differential test harness comparing EvoMu's `crates/eve-dogma` against CCP's Python 2.7 Dogma bytecode (`dogma/const.pyj`, `dogma/attributes/attribute.pyj`, `dogma/attributes/stackingNurfedAttribute.pyj`, `dogma/dogmaLocation.pyj` in `code.ccp`). **Exit criteria (no Python 3 mirror models):**<br>1. **Headless execution via CCP's bundled `libpython2.7.dylib` + `code.ccp`:** `tools/generate_golden_fixtures.py` must load the client's bundled Stackless Python 2.7 shared library (`EVE.app/Contents/Resources/build/bin64/libpython2.7.dylib`, via `ctypes` or a helper binary) to unmarshal (`marshal.loads`) and execute the actual compiled `.pyj` Dogma bytecode modules from `code.ccp` (`stackingNurfedAttribute.pyj`, `attribute.pyj`, `dogmaLocation.pyj`). Every expected attribute value (including the 11-operator precedence vectors and all ship fits) must be produced by invoking CCP's actual Python 2.7 Dogma classes/methods in `libpython2.7.dylib` against `data/evomu_sde.sqlite` data — never by a hand-written Python 3 port of EvoMu's evaluator (`DERIVED_REFERENCE`).<br>2. **Fit & modifier coverage:** Evaluates canonical and procedural ship fits across ship classes with modifier graphs (character skills, modules, rigs, implants, combat boosters, overheating, gang bursts, and mutaplasmids) and asserts `crates/eve-dogma` matches CCP's calculation within floating-point precision.<br>3. **Client-version keying & `--diff` CLI:** Dumps `crates/eve-dogma/tests/fixtures/ccp_golden_fits.json` keyed by `<codename>-<build>` (`"V24.01-3624425"`, parsed from `start.ini` + `code.ccp` SHA-256, with `"oracle_source": "CCP_BUNDLED_RUNTIME"`) and supports `python3 tools/generate_golden_fixtures.py --diff <old_ver> <new_ver>`. |

**Exit gate:** T-1…T-5 and T-7 Done-when met; review non-FAIL. T-6's harness skeleton lands here. Its `launch_login` criterion closes as soon as P-6 (native handshake) and ESI-1 (SSO/launcher tokens) land, and every later phase adds its journeys to the harness.

---

## Phase 2: Wire protocol
**Crucible reference:**
- `eve-common/marshal/EVEUnmarshal.cpp:506-680` (PackedRow decode)
- `EVEMarshal.cpp:306-470` and `EVEDBUtils.cpp:316-475` (row encoders)
- `cache/CachedObjectMgr.cpp:105-560` and `packets/ObjectCaching.xmlp:53-95` (object cache)
- `config/ConfigService.cpp`
- `EntityList.cpp:140-777` (broadcast)
- `network/EVESession.cpp:50-200` and `Client.cpp:2360-2519` (handshake)

| ID | Task | Done when |
|---|---|---|
| P-1 | **PyPackedRow trailing object columns** | Decode STR/WSTR/BYTES columns marshaled after the RLE blob. Round-trip test with mixed fixed and object columns, plus a test that the following stream elements still decode. |
| P-2 | **DBRow / CRowset / CIndexedRowset / PackedRowList encoders** | Build them from typed rows with a DBRowDescriptor. Golden tests decode them back. Use them in at least `invbroker.List`, `GetCharactersToSelect` and `marketProxy` order lists, matching V24 client expectations. |
| P-3 | **Object cache / CachedMethodCallResult** | Version and timestamp per cached object. `objectCaching.GetCachableObject` returns real cached payloads. Methods the V24 client calls through the cache wrapper return `CachedMethodCallResult`. |
| P-4 | **config service** | `GetMultiOwnersEx/LocationsEx/StationEx/InvTypesEx/AllianceShortNames/CorpTickerNames` return only the requested IDs, from the SDE plus the world DB, in V24 shapes. |
| P-5 | **Notifications** | Generic `notify(char/corp/system/all, event, payload)` over the macho transport (`OnRemoteMessage`, `OnItemChange`, `OnAccountChange`, `OnSkillTrained`…), sent as marshaled packets, not JSON. |
| P-6 | **Native handshake** | Implement the state machine with Crucible's shape (version → queue/command → crypto → challenge → ack → initial session), adapted to the V24 launcher/SSO token login as evidenced in the client bytecode. Never accept unauthenticated `AuthenticationReq('ok')`. Test: wrong credentials are rejected; correct ones produce a session-init notification. |
| P-7 | **Destiny binary stream** | `SetState`/`AddBalls2`/`DoDestinyUpdate` sent as marshaled Destiny packets (the existing `wire::encode_add_balls`), not JSON strings. |

**Exit gate:** protocol golden tests pass; the bridge and native modes share one encoder; the review is non-FAIL.

---

## Phase 3: Space simulation
**Crucible reference:**
- `system/SystemManager.cpp:380,1308,1395` (statics, SetState)
- `system/BubbleManager.*`
- `system/DestinyManager.cpp:702-1810,2327-2427` (movement, warp, dock)
- `ship/BeyonceService.cpp:156,188,354,405,583,688,782`

| ID | Task | Done when |
|---|---|---|
| SP-1 | **Celestials and gates as balls** | Each SystemActor loads stars, planets, moons, belts, gates and stations from the SDE with real positions. SetState on entering a system lists them. |
| SP-2 | **Bubbles / grid interest** | Ball visibility and destiny updates are scoped per bubble (V24 grid sizes), using `balls_in_bubble`. Test: two ships 10,000 km apart don't receive each other's updates. |
| SP-3 | **Beyonce command set** | `CmdAlignTo`, `CmdSetSpeedFraction`, `CmdWarpToStuff(("item"\|"bookmark"\|"char", id), minRange)` resolves targets server-side. Add warp capacitor cost, minimum warp distance, warp-scramble rejection (strength vs stabilizers) and `UpdateStateRequest`. Approach, orbit and keep-at-range each get their own handler. |
| SP-4 | **Stargate jump polish** | Arrival at the destination gate's real position, the session-change notification, the gate cloak, and **no jump fatigue on stargates**. |
| SP-5 | **Dock / undock** | Dock range measured from the station's docking perimeter (radius-aware), a dock delay, and undock along the station's undock vector. |
| SP-6 | **Logoff, disconnect and login in space** (user-added 2026-10-22) | Follows the live rules (wiki *Timers*, *Logging off*). Clean `authentication.Logout` and a dropped connection both leave the ship in space for the logoff period: a single emergency-warp attempt to a random point about 1 million km away (subject to warp disruption), kept in place for the 5-minute NPC or 15-minute PvP log-off timer (renewed if the logged-off ship is shot). After that the ship leaves space. `ship.SafeLogoff` runs a real countdown that aborts on the documented conditions, is refused under a PvP timer, and emits `OnSafeLogoffTimerStarted` / `OnSafeLogoffActivated` / `OnSafeLogoffAborted` / `OnSafeLogoffFailed`. On the next login, a character who logged off in space resumes in the same ship, in the same system, at the persisted position (the ball is re-created from saved state, not left behind or missing). All of this holds across a server restart. Tests: disconnect mid-space then log in; clean logout then log in; logoff under an NPC and a PvP timer (ship stays and can be shot); restart between logoff and login. |
| SP-7 | **Background cluster scheduler, unobserved systems & ballpark reload** (user-added 2026-10-22; Audit B) | 1. **Production background scheduler (`main.rs`):** Continuously runs `run_daily_downtime_and_jobs` (at 11:00 UTC or accelerated clock, warning/disconnecting clients and refunding buyer escrow + returning seller items on order expiry), `step_concord` (spawning CONCORD in production after the security delay), `step_ai` (NPC AI & encounter waves), wreck/jetcan/anomaly despawn sweeps, skill-queue completion checks (`OnSkillTrained`), and Abyssal 20-minute pocket collapse destruction — never relying on test-only callers.<br>2. **Unobserved systems never freeze (`system_actor.rs`):** Decouple `SystemActor` physics/timer stepping from `events_tx.receiver_count() > 0` so in-flight warps, logoff timers, NPCs, and wrecks continue advancing when 0 players are subscribed.<br>3. **Ballpark reload from SQLite on server restart:** Startup rebuilds in-space ships, anchored Upwell structures, deployables, wrecks, jetcans, and abandoned drones into `SystemActor` ballparks from SQLite; generic `Anchor` only registers warp disruption bubbles for actual interdiction sphere groups; characters in expired Abyssal pockets are destroyed/returned on restart rather than stranded in `32000001`. |

**Exit gate:** wire-level test: undock → align → warp to gate → jump → warp to station → dock, using real SDE positions and no hardcoded coordinates. Review non-FAIL.

---

## Phase 4: Dogma and fitting
**Crucible reference:**
- `effects/EffectsDataMgr.cpp:239-297`, `effects/EffectsProcessor.cpp:30,277` (effect application; concepts only, since V24 uses `modifierInfo`)
- `ship/Ship.cpp:737-798` (fit checks)
- `ship/modules/GenericModule.cpp:90-152` (online)
- `ship/modules/ActiveModule.cpp:324-1053` (cycles)
- `system/TargetManager.cpp`

| ID | Task | Done when |
|---|---|---|
| DG-1 | **Modifier engine over V24 `modifierInfo`** | Domains (ship, char, item, target, other), funcs (ItemModifier, LocationGroupModifier, LocationRequiredSkillModifier, OwnerRequiredSkillModifier, …), operators and stacking. Applies skills, implants, ship, modules, charges and rigs. Remove the hardcoded group-762/774/46 code. Test: a Rifter with skills and 2 overdrives matches published attribute values. |
| DG-2 | **Fit validation** | Moving an item into a slot flag checks the slot type, hardpoints, rig size/calibration, `maxGroupFitted` and CPU/PG for online. Over-fit is rejected. |
| DG-3 | **Online/offline** | `SetModuleOnline`/`TakeModuleOffline` are real, persisted and notify `OnModuleAttributeChanges`. |
| DG-4 | **Module cycle engine** | `dogmaIM.Activate(itemID, effectName, target, repeat)` / `Deactivate`. Includes cycle timers in the SystemActor tick, capacitor need, charge consumption, auto-repeat, and deactivation on warp, dock or out of range. Emits `OnGodmaShipEffect`. |
| DG-5 | **Targeting** | `AddTarget`/`RemoveTarget`/`ClearTargets` with lock time from scan resolution and signature, max targets and max range. Targeted effects require a lock. |
| DG-6 | **Ammo and grouping** | `LoadAmmo`/`UnloadAmmo`/reload timers, and real `LinkWeapons` groups. |

**Exit gate:** fit → online → lock → activate a propulsion module and a turret through `dogmaIM`, with attributes from the SDE. Review non-FAIL. **Delete `dogmaIM.FireWeapon`** (it isn't a client method).

---

## Phase 5: Combat and PvE loop
**Crucible reference:**
- `ship/modules/TurretFormulas.cpp`, `TurretModule.cpp:55`
- `ship/Missile.cpp`
- `system/Damage.cpp:99,364-634`
- `npc/SpawnMgr.cpp`, `npc/NPCAI.cpp:268-347`, `system/BeltMgr.*`
- `ship/modules/MiningLaser.cpp:170,362`, `Prospector.cpp`
- `station/ReprocessingService.cpp:162-287`
- `npc/DroneAI.cpp`

| ID | Task | Done when |
|---|---|---|
| C-1 | **Turret and missile damage from attributes** | Hit chance and damage from fitted module, charge, skills and target signature/velocity. Missiles are entities with flight time and explosion math. Resists come from target dogma. |
| C-2 | **HP from dogma; damage notifications** | `CombatState` is initialized from ship attributes and persisted. Emits `OnDamageStateChange`. |
| C-3 | **Death pipeline** | Wreck ball (SDE wreck type) and dropped loot belong to the **victim** (`owner_id = victim`, never the attacker, so the victim is not flagged Suspect for looting their own wreck), loot drop by fitted/cargo survival rules, pod ejection with `OnSessionChanged` to the capsule and session-change timer, insurance payout, killmail. Capsule destruction across **every** death path (including `death_pipeline`: self-destruct, CONCORD, missiles, smartbombs) destroys active clone implants and transitions the victim's session to their home station medical clone. A PvP kill requires actual destruction. |
| C-4 | **NPC spawns and AI** | Belt/gate spawns from SDE spawn groups filtered by system security and faction (OF-42) with respawn timers. AI: idle → chase → engage, with weapons, orbit range and reps, driven continuously by the production scheduler (`SP-7`). Bounty from the NPC type's bounty attribute. Remove the flat 100k. |
| C-5 | **Asteroid belts and mining** | *Superseded by **Phase 5M** (MN-01 … MN-18).* Phase 5's exit gate only needs MN-01 to MN-07 and MN-12. |
| C-6 | **Reprocessing (real)** | *Superseded by **MN-12** (reprocessing) and MN-11 (compression).* |
| C-7 | **Salvage, tractor, drones** | Salvager roll → salvage items. Tractor beam pulls the wreck. Drone launch/recall/engage with bandwidth limits. `CmdReturnBay` checks system/distance and flies drones back to the ship before baying (no instant cross-system teleport). Warping, jumping, docking, disconnect, or death marks in-space drones as `is_abandoned = true` so `CmdReconnectToDrones` reacquires them. (Mining drones are MN-09.) |
| C-8 | **CrimeWatch and CONCORD** | Real CONCORD ships spawn in production via `SP-7` after the security-based delay and destroy the criminal through C-3. Emits `OnWeaponsTimerUpdate` / `OnPvpTimerUpdate` / `OnNpcTimerUpdate` / `OnCriminalTimerUpdate` on state changes. Remove the instant ball deletion. |

**Exit gate:** integration test: undock in a belt system → lock a spawned NPC → kill it with a turret → bounty paid → loot the wreck → mine ore → dock → reprocess. All from SDE data. Review non-FAIL.

---

## Phase 5M: Full mining system
**Goal:** the complete V24 mining loop. Find a belt or site → mine ore, ice or gas with lasers, harvesters or drones → fill the ore hold → compress → haul → reprocess into minerals with the correct yields. Moon mining and the ledgers sit on top, with NPCs and economy hooks.

**Depends on:**
- D-1/D-2: real SDE.
- SP-1/SP-2: celestials, belts, bubbles.
- DG-1: modifier engine.
- DG-4: module cycle engine.
- DG-5: targeting.
- P-5: notifications.
- C-4: NPC spawns (for MN-17).

Don't start MN-04 or later until those land. Mining is a module-cycle feature, not a one-off RPC.

**Crucible reference** (re-implement; Crucible is LGPL):
- `system/BeltMgr.*` (belt spawning, ore by security, ice distribution)
- `ship/modules/MiningLaser.cpp:170,362` (`ProcessCycle`, `AddOreAndDeactivate`)
- `ship/modules/ActiveModule.cpp:746` (survey scanner)
- `station/ReprocessingService.cpp:162-287` (recoverables, efficiency, tax)
- `npc/DroneAI.cpp` (mining drones)

**V24 contracts** (from `docs/index/rpc_service_catalog.json` and `client_notifications.json`; confirm the argument layouts in the client bytecode):
- Mining runs through `dogmaIM.Activate` / `Deactivate` on the laser, harvester or drone.
- Services:
  - `miningScanMgr.perform_scan`
  - `characterMiningLedger.GetCharacterLogs`
  - `corpMiningLedger.GetObserverLedger` / `GetObserversWithMiningEvents`
  - `inSpaceCompressionMgr.CompressItemInSpace`
  - `structureCompressionMgr.CompressItemInStructure` / `DecompressGasInStructure` / `GetMyGasDecompressionEfficiency`
  - `moonExtractions.*` (StartNewExtraction, CancelExtraction, FractureChunkWithStructure, GetExtractionsForCorp, GetMoonResourcesForStructure, GetExtractionAndEventIDFromStructureID)
  - `autoMoonMining.RequestMiningDetails` / `RequestMiningCycleOutput`
  - `reprocessingSvc` bound object (find the V24 method names in `eve/client/script/ui/station/reprocessing*`)
- Notifications: `OnOreMined`, `OnOreMinedByOther`, `OnMiningLaserFiredLocal`, `OnMiningLaserFiredFailedLocal`, `OnAsteroidRevealed`, `OnAsteroidTerminated`, `OnWarpToAsteroidBelt`, `OnReprocessingMaterialsChanged`, `OnMoonExtractionUpdate`, `OnMoonScanComplete`, `OnNewMoonProbe`, `OnRemoveMoonProbe`, `OnMiningLedgerGraphSettingChanged`.

### A. Data
| ID | Task | Done when |
|---|---|---|
| MN-01 | **Mining static data in the SDE importer** | The real SDE (D-1) provides the following. No hand-written ore tables remain in Rust. Tests: Veldspar → Tritanium only; Scordite → Tritanium + Pyerite; an ice type yields its isotope and fuel materials; ore variant yields differ from the base ore.<ul><li>All asteroid ore types with their variants, moon ores (R4–R64), ice, gas (Mykoserocin/Cytoserocin/Fullerite), and Prismaticite if V24 has it.</li><li>For each: `typeMaterials` (with portionSize), volume, compressed counterpart (`compressibletypes`), and group/category.</li><li>Ore distribution by security band, region and system.</li><li>Asteroid belts and ice belts as celestials with positions.</li><li>Mining modules (lasers, strip miners, modulated, ice harvesters, gas harvesters), crystals (with volatility and damage), mining drones, MLUs, rigs, implants, and ships with ore/ice/gas holds. Each has dogma attributes: `miningAmount`, `duration`, `maxRange`, crystal and residue attributes, and specialized hold capacities.</li></ul> |

### B. Space content
| ID | Task | Done when |
|---|---|---|
| MN-02 | **Asteroid belts as balls** | Each system's belts come from SDE celestials (SP-1). Asteroids spawn per belt using the security/region ore distribution, with quantity and radius scaled from the ore amount. They appear in SetState for the bubble. `OnWarpToAsteroidBelt` and warp-to-belt work. Test: Jita belts contain only high-sec ores, and a 0.0 system has null-sec ores. |
| MN-03 | **Depletion and respawn** | Each asteroid's remaining quantity is persisted. It shrinks as it's mined, and `OnAsteroidTerminated` fires plus the ball is removed at 0. Belts regenerate on the downtime/maintenance tick (configurable), not instantly. Test: mining an asteroid to 0 removes it, and it stays gone until the respawn tick. |

### C. Mining cycle engine
| ID | Task | Done when |
|---|---|---|
| MN-04 | **Mining laser / strip miner cycle** | `dogmaIM.Activate` on a fitted mining module checks the requirements. On each cycle completion it moves ore into the hold and repeats. Test: a Venture with Miner I mines N cycles of Veldspar with the exact SDE-derived m³, and stops and deactivates when the hold is full or the rock is depleted.<ul><li>**Requirements:** an online module, a locked target that is an asteroid of a minable group, range ≤ `maxRange`, enough capacitor, and not cloaked or warping.</li><li>**Yield per cycle:** effective `miningAmount` (m³) after modifiers (skills, ship bonuses, MLUs, rigs, implants, crystals, command bursts) ÷ ore volume, clamped by the asteroid's remaining quantity and the free hold space.</li><li>**Partial cycles:** a partial final cycle on depletion, and deactivation when the hold is full or the asteroid is gone.</li><li>**Interruptions:** cycles stop on warp, dock, out-of-range or lost lock.</li><li>**Notifications:** `OnOreMined` / `OnMiningLaserFiredLocal`, with failures reported through `OnMiningLaserFiredFailedLocal`.</li></ul> |
| MN-05 | **Holds and inventory** | Ore goes to the ore/ice/gas hold flag (`specialMaterialBay`/`oreHold`) first, then cargo if allowed. Volume and capacity are enforced for every add, and partial stacks merge. Test: a full ore hold overflows to cargo only for ships that allow it, and capacity is never exceeded. |
| MN-06 | **Crystals and residue** | Mining crystals are loaded as charges (DG-6) and apply ore-type-specific bonuses. Crystal damage follows volatility (`Crystal_damage.md` formula), and the crystal breaks at 0 HP. Residue/waste is a chance and multiplier per cycle for the modules and crystals that have it: wasted ore is removed from the asteroid but not delivered. Tests: crystal HP decreases per cycle; residue removes extra asteroid quantity; a T1 laser without residue attributes never wastes. |
| MN-07 | **Skills and bonuses applied through the dogma engine** | Mining, Astrogeology, Mining Barge/Exhumers, ice/gas harvesting skills and ore-specific processing skills are applied through DG-1 `modifierInfo`, as are ship role bonuses, MLUs, rigs, implants (e.g. Michi's) and mining command bursts (Mining Foreman links: yield, cycle time, range). No hardcoded multipliers. Test: yield with Mining V + Astrogeology V + a Retriever + 2 MLU IIs matches the published value within 0.1%. |
| MN-08 | **Ice and gas harvesting** | The ice harvester cycle yields 1 unit of ice per cycle (ice volume), and the cycle time comes from attributes. Gas cloud harvesters work on gas site clouds (signature sites, MN-15). Both use the same cycle engine and holds. Test: an ice harvester produces 1 ice block per cycle into the ice hold; gas harvesting depletes the cloud. |
| MN-09 | **Mining drones** | Mining and excavator drones launch (bandwidth/bay), are ordered to mine the target asteroid, run their cycles, and deliver ore to the ship's ore hold when they return or on each cycle per V24 behaviour. They respect the drone control range. Test: two Mining Drone Is mine into the ore hold and stop when it's full. |
| MN-10 | **Survey scanner** | `miningScanMgr.perform_scan` returns the real remaining quantity of every asteroid in range of the fitted survey scanner (range from attributes). `OnAsteroidRevealed` behaves per V24. Test: the scan results match the asteroid quantities after mining. |

### D. Processing
| ID | Task | Done when |
|---|---|---|
| MN-11 | **Compression (all compressible types)** | In-space: `inSpaceCompressionMgr.CompressItemInSpace` requires being in a fleet within range of an Industrial Command Ship or Rorqual with an active compressor module/burst, and the ore in the caller's hold. In structures: requires a Standup Compressor service. Ratio and portion size come from the SDE (`compressibletypes` + portionSize), leftovers stay uncompressed, and gas decompression applies its efficiency. Remove the 2-ore table. Test: compressing 1,234 Veldspar yields the SDE-correct compressed quantity plus a remainder. |
| MN-12 | **Reprocessing (real)** | Recoverables come from SDE `typeMaterials` per portionSize; partial batches are returned. **Yield** = base facility yield (NPC station, or Upwell structure with refinery rigs/Tatara/security modifier) × (1 + Reprocessing skill) × (1 + Reprocessing Efficiency) × (1 + the ore-specific processing skill) × implant bonus.<br>**Tax** comes from station standings (NPC) or the structure owner's setting, and is journaled. The real V24 quote/info methods are implemented, plus `OnReprocessingMaterialsChanged`. Only types with `typeMaterials` can be reprocessed. Tests: Veldspar/Scordite/Pyroxeres yields at given skills and standings match the published calculator values; tax is charged and journaled; a non-reprocessable item is rejected. |

### E. Moons, structures and ledgers
| ID | Task | Done when |
|---|---|---|
| MN-13 | **Moon mining (Athanor/Tatara + Standup Moon Drill)** | Moon composition data comes from the SDE or a survey (moon probes / `OnMoonScanComplete`). `moonExtractions.StartNewExtraction` schedules a chunk and `CancelExtraction` cancels it. When it arrives, `FractureChunkWithStructure` (or auto-fracture) spawns a moon-ore asteroid field (MN-02 rules) that lasts the V24 duration, with jackpot chances. Corp ACL permissions apply. `OnMoonExtractionUpdate` fires. Test: start → fast-forward the clock → fracture → field present with the moon's ore mix → mined ore is logged to the corp ledger. |
| MN-14 | **Metenox Moon Drill (automated)** | `autoMoonMining.RequestMiningDetails` / `RequestMiningCycleOutput`. Hourly output into the structure's output bay at Metenox efficiency (40%), consuming fuel blocks and Magmatic Gas, and stopping when out of fuel. Test: 24 simulated hours produce the expected quantities and consume the fuel. |
| MN-15 | **Mining sites** | Ore anomalies (cosmic anomalies with special ore asteroids), ice belts/anomalies (respawn timers) and gas signatures (scan-dependent, SO-5). Each site has a lifetime and despawns, and every site uses MN-02/MN-03 asteroid rules. Test: an ore anomaly spawns with the correct ore set and despawns after it's cleared or expires. |
| MN-16 | **Mining ledgers** | Each cycle writes ledger entries (character, type, quantity, system, date). `characterMiningLedger.GetCharacterLogs` returns the caller's entries. For corp structures (moon fields), `corpMiningLedger.GetObserversWithMiningEvents` / `GetObserverLedger` returns entries per observer with corp ACL. Data is retained for the V24 window. Test: mining 3 cycles produces 3 aggregated ledger rows. Corp ledger access is denied to outsiders. |

### F. World and economy hooks
| ID | Task | Done when |
|---|---|---|
| MN-17 | **NPC interaction** | Belt rats spawn near belts (C-4) and target miners. Living-world `MiningOp` fleets actually mine belt asteroids through MN-04 rules, haul ore, and can be attacked. Their ore feeds the regional market (replacing the static `MiningOp` label). Test: an NPC mining op depletes asteroid quantity over ticks. |
| MN-18 | **Missions, goals and market** | Mining agent missions: mine X of an ore at a pocket asteroid field spawned for that mission (E-5 rules), and deliver it. Daily goal / career "mine N units" progress comes from ledger events (never from client-supplied counts). Ore, mineral and compressed-ore market seeding uses SDE types. Test: a mining mission completes only after the required quantity is delivered. |

**Exit gate (all items require public RPCs and the real SDE; no direct `snap` writes, no hand-made `CombatState`, no fixtures in production):**
1. `tests/mining_loop.rs::venture_mines_compresses_reprocesses`:
   1. Undock in a high-sec belt system and warp to a belt.
   2. Lock a Veldspar asteroid, activate Miner I, and run cycles until the ore hold is full.
   3. Check the exact quantities and that the asteroid quantity decreased.
   4. Check the ledger rows.
   5. Compress at a structure, dock, and reprocess.
   6. Check the mineral output and tax against the formula.
2. `tests/mining_loop.rs::ice_and_gas_harvesting`.
3. `tests/mining_loop.rs::mining_drones_fill_ore_hold`.
4. `tests/mining_loop.rs::moon_extraction_to_ledger`.
5. `tests/mining_loop.rs::belt_depletion_and_respawn_tick`.
6. Review non-FAIL, with every MN task ID's "Done when" checked by the reviewer.

---

## Phase 6: Character progression and GM tooling
**Crucible reference:**
- `character/Character.cpp:1000-1166` (SkillQueueLoop)
- `character/SkillMgrService.cpp:67-237`
- `character/CharUnboundMgrService.cpp:142-322`
- `admin/GMCommands.cpp`, `SystemCommands.cpp`, `debug_commands.cpp`, `CommandDispatcher.cpp`

| ID | Task | Done when |
|---|---|---|
| CH-1 | **Skill queue time engine** | The queue advances in real time using each skill's own primary/secondary attributes. Completes levels, writes history, notifies `OnSkillTrained`/`OnMultipleSkillsTrained`. Handles Alpha caps by the delta. |
| CH-2 | **skillMgr2 / skill handler semantics** | `SaveNewQueue`/`SaveSkillQueue` replaces the full list and checks prerequisites. Separate `GetSkillQueue`, `GetEndOfTraining` and `GetSkillHistory`. `InjectSkillIntoBrain` consumes the book, adds the skill at level 0, and rejects already-known skills (per V24 post-Ascension semantics, skillbooks may be injected before prerequisites are trained; `SaveSkillQueue` and `ApplyFreeSkillPoints` enforce prerequisites). Neural remap with cooldown. `ApplyFreeSkillPoints(typeID, points)` honours its arguments. |
| CH-3 | **Character creation & biomass lifecycle** | Correct V24 `CreateCharacterWithDoll` argument mapping. Real starter skills from the SDE/career tables. Doll/portrait stored. Corvette by race. Biomass/character deletion blocks active CEOs, cancels and refunds open market orders, removes the character from corporation and fleet membership, and cleans up active sessions. |
| CH-4 | **Character info** | Separate real handlers for public info, bio, home station, notes, contacts and labels. |
| CH-5 | **GM command dispatcher** | Parse `/cmd args`, check a per-command role, execute, and audit-log. Priority commands: `/giveskill(s)`, `/giveisk`, `/createitem`, `/tr`, `/heal`, `/spawn`, `/unspawn`, `/setattr`, `/getattr`, `/fit`, `/kill`, `/location`, `/online`. Each has a test proving its state change. |

**Exit gate:** a queue trains across a simulated clock and notifications fire. GM commands work only for GM roles. Review non-FAIL.

---

## Phase 7: Economy, industry and missions
**Crucible reference:**
- `market/MarketProxyService.cpp:326-335,640-719`, `market/MarketMgr.cpp:99-200,416-625`
- `contract/ContractProxy.cpp:509-770`
- `manufacturing/RamMethods.cpp:349-406`, `RamProxyService.cpp:340,617-643`
- `agents/AgentBound.cpp:72-390`, `agents/Agent.cpp:119,566-700`
- `planet/Colony.cpp:360-1507`, `planet/CustomsOffice.cpp`

| ID | Task | Done when |
|---|---|---|
| E-1 | **Fee inputs** | Broker fee and sales tax use the character's actual Broker Relations, Advanced Broker Relations and Accounting levels, and real faction/corp standings. Fees are journaled. |
| E-2 | **Order lifecycle** | `ModifyCharOrder`, `CancelCharOrder` (escrow refunded, items returned), automatic background expiry (`SP-7`) that refunds buyer escrow and returns unsold seller items (expired orders can never be matched or filled), partial-fill journal entries for both parties, and market history built from transactions. Real V24 method names (`PlaceBuyOrder`, `PlaceMultiSellOrder`, …). |
| E-3 | **Contracts** | Item exchange: price paid, items transferred to the acceptor, requested items. Courier: items travel in a courier package, completion requires delivery at the destination, expiry or failure pays collateral to the issuer. Auction bids if they're in V24 scope. |
| E-4 | **Industry** | Install cost (system cost index × EIV), completion only after `endTime`, `CompleteManyJobs` completes **every** job ID in the input list (not only index 0), output = portionSize × runs, ME/TE from the **blueprint item**, copying produces BPCs, invention consumes datacores plus a T1 BPC and rolls probability for a T2 BPC. Blueprint locked during the job. |
| E-5 | **Agents and missions** | Agent state machine (offer, accept, decline, defer, quit, complete) with a standing gate (effective standing with Connections/Diplomacy). Mission content spawns its own dungeon/NPCs; only those kills count. Courier missions need delivery. Rewards use real skills. Standing propagates to agent, corp and faction (Social). Re-requesting a completed mission respects the cooldown. |
| E-6 | **PI basics** | Pins, links, routes, extraction/factory cycles over time (`StartTicking` advances only by real elapsed clock time with no minimum 300s time-skip exploit, extractor programs expire at `endTime`, storage enforces capacity caps, and `planetMgr.Flush` never mints commodities from nothing), launch creates an orbital container or customs transfer with 5-day expiry, and export tax is charged through the POCO. |

**Exit gate:** buy → modify → cancel; manufacture with a time gate; courier contract that requires delivery; a mission that requires killing its own spawned NPCs. Review non-FAIL.

---

## Phase 8: Social and exploration
**Crucible reference:**
- `mail/MailMgrService.cpp`
- `fleet/FleetService.cpp` (FleetBound)
- `corporation/CorpRegistryBound.cpp:421-1490`
- `alliance/*`
- `system/BookmarkService.cpp`
- `exploration/AnomalyMgr.cpp`, `Scan.cpp:100-790`, `WormholeMgr.cpp`
- `dungeon/DungeonMgr.cpp`
- `chat/LSCService.cpp` (concepts only)

| ID | Task | Done when |
|---|---|---|
| SO-1 | **EVE mail** | Recipient lists, headers, body, sync, read state, labels, trash. Mailing lists after that. |
| SO-2 | **Fleets** | Invite, accept, leave, kick, wings/squads, roles, MOTD, broadcasts, and fleet warp. Logout, disconnect, and server restart clean up fleet membership and hand over fleet boss; a character can belong to at most one fleet; `fleetObjectHandler.Init` initializes the existing fleet rather than creating a second fleet; `FinishMove` does not demote the caller. |
| SO-3 | **Corporations, alliances, and structures** | Members, roles (with 24h role-change delay persisted in SQLite), titles, divisions, applications, leaving the old corp on join/create (single-corp invariant: a character never belongs to two corps at once), `InsertInvitation` requires target acceptance (never auto-joining via client `accept_now`), `OnSessionChanged` pushed to target on kick/join, ticker uniqueness, alliance create/join with Director/CEO checks, and Upwell structure Asset Safety (0.5% fee for manual same-system delivery, 15% cross-system, 20-day auto-delivery timer). |
| SO-4 | **Bookmarks** | Create, list, folders, delete, copy and warp-to-bookmark (via SP-3), with V24 shared folders. |
| SO-5 | **Exploration** | Anomaly/signature generation per system with timers. Probes as entities. Multi-probe certainty/deviation math. D-scan cone over real balls. Dungeon spawning with acceleration gates. |
| SO-6 | **Wormholes** | Per-type destinations, mass and lifetime with collapse; jumping deducts mass from that wormhole. |
| SO-7 | **Chat** | Design spike: V24 uses XMPP, so evaluate embedding or bridging an XMPP server. Local, corp and fleet channels follow membership. |

**Exit gate:** mail round trip between two characters, fleet formation with a broadcast, probe a spawned signature and enter its dungeon. Review non-FAIL.

---

## Phase Q: Quasar / public-gateway backend services (all of them)
The client ships 1,455 protobuf files across 135 packages (`references/parity/sources/quasar_protobuf_catalog.json`), and every one is a `quasar:` ledger row. Many features in the modern client exist only here. Examples: daily goals, corp projects, freelance jobs, SKINR/cosmetics, the store/PLEX/vault, Equinox sovereignty, the AIR career program and achievements.

| ID | Task | Done when |
|---|---|---|
| Q-1 | **Protobuf extraction and codegen.** Extract the serialized `FileDescriptorProto`s embedded in the client's `*_pb2` modules (inside `code.ccp`) and generate Rust types (`prost`) for every file. Don't hand-write them. | All 1,455 descriptors are extracted, with a recorded checksum. Generated types round-trip every message in property tests (T-3). |
| Q-2 | **Gateway transport.** Implement the public-gateway request/response/notice framing exactly as the client's gateway client uses it (`publicGatewaySvc` and Quasar streams). Add auth/identity context, idempotency keys, error/status codes, and notice fan-out to character/corp/alliance/fleet/system/all. | The golden tests (T-4) pass against the client's own gateway codec. A notice reaches two connected clients. |
| Q-3 | **Identity, accounts and entitlements:** `auth`, `login`, `user`, `entitlement`, `omega`, `membership`, `payment`, `plex`, `store`, `vendors`, `fulfillment`, `isk`, `legal`, `pii`, `device`, `cdn`, `localization`. | Every `quasar:` row in these packages is closed. |
| Q-4 | **Character and progression:** `character`, `skill`, `implant`, `clone`-related, `career`, `achievement`, `academy`, `goal`, `dailygoal`, `seasonalevent`, `battlepass`, `campaign`, `opportunity`, `narrative`, `tale`, `communitytrack`. | Closed. |
| Q-5 | **Social and organization:** `corporation`, `corporationgoal`, `freelance`, `alliance`, `contact`, `standing`, `mail`, `chat`, `notification`, `calendar`, `fleet`, `medal`, `presence`, `search`, `bookmark`, `tag`, `war`, `duel`, `securitystatus`. | Closed. |
| Q-6 | **Economy and industry:** `market`, `contract`, `industry`, `blueprint`, `asset`, `assetholding`, `inventory`, `container`, `accounting`, `loyaltypoints`, `planet`, `planetinteraction`, `ore`, `moon`, `asteroidbelt`. | Closed. |
| Q-7 | **Space and combat:** `ship`, `module`, `fighter`, `dogma`, `effect`, `capacitor`, `space`, `spacecomponent`, `bubble`, `solarsystem`, `constellation`, `region`, `universe`, `star`, `stargate`, `station`, `structure`, `starbase`, `wormhole`, `deadspace`, `dungeon`, `npc`, `pirate`, `piratestronghold`, `dynamicbountysystem`, `killmail`, `pvp`, `probe`, `vanguard`, `starterspace`. | Closed. |
| Q-8 | **Sovereignty, faction warfare and events:** `sovereignty`, `factionwarfare`, `faction`, `operation`, `tournament`, `raffle`, `contextualoffer`, `csat`, `projectdiscovery`, `thirdpartyapi`. | Closed. |
| Q-9 | **Cosmetics and app infrastructure:** `cosmetic` (SKINs, SKINR/paragon, emblems, logos), `color`, `graphics`, `app`, `client`, `eve`, `eve_public_root`, `quasar`, `monolith`, `network`, `id`, `idempotency`, `math`, `nodegraph`, `ui`, `debug`, `botdetection`, `legacy`, plus any package not listed above. | Closed, or triaged NA with evidence the auditor accepts. |

**Exit gate:** every `quasar:` row closed or accepted NA; the `daily_goals_events` and `cosmetics_store` journeys pass on the real client; review non-FAIL.

---

## Phase ESI: EVE Swagger Interface and SSO
Pinned spec: `references/parity/sources/esi_openapi.json` (197 operations, 182 paths). Every operation is an `esi:` ledger row.

| ID | Task | Done when |
|---|---|---|
| ESI-1 | **SSO / OAuth2 server** emulating `login.eveonline.com`. Covers the authorization code flow and PKCE, token and refresh endpoints, JWT access tokens with JWKS, scope grants, and revocation. It also issues the launcher/client login tokens the V24 client uses to connect (with P-6). | An external OAuth client obtains and refreshes a token. The real client logs in with an emulated launcher token (`launch_login` journey). |
| ESI-2 | **ESI HTTP framework.** OpenAPI-conformant routing, including compatibility-date/versioned paths. Bearer validation and per-route scope enforcement. `ETag`, `Expires`, `Last-Modified` and 304s. `X-Pages` pagination. Error and rate-limit headers and the error model. Response bodies are validated against the pinned schemas in tests. | A schema-validation test helper is in place and a sample route in each tag passes. |
| ESI-3 | Universe, Status, Meta, Routes, Search, Incursions, Insurance, Sovereignty, Wars, Faction Warfare, Dogma, Killmails. | Every `esi:` row in these tags is closed. |
| ESI-4 | Character, Clones, Skills, Location, Fittings, Assets, Wallet, Loyalty, Calendar, Contacts, Mail. | Closed. |
| ESI-5 | Corporation, Alliance. | Closed. |
| ESI-6 | Market, Contracts, Industry, Planetary Interaction. | Closed. |
| ESI-7 | Fleets and **User Interface** (open window, set waypoint, autopilot). These POST endpoints must act on the connected real client. | Closed; the `esi_external` journey passes. |

**Exit gate:** every `esi:` row closed; `esi_external` journey passes; review non-FAIL.

---

## Phase Z: Ledger closure and final acceptance
| ID | Task | Done when |
|---|---|---|
| Z-1 | Close every `rpc:` row (1,230 client-called methods) with V24 argument layouts and live semantics, or triage NA with evidence the auditor accepts (dead client code only). | `parity.md` shows rpc 100% closed. |
| Z-2 | Close every `notify:` row. Server-emitted notifications are sent with correct payloads and scopes; client-local events are triaged NA with evidence. | notify 100%. |
| Z-3 | Close every `destiny:` and `ballmode:` row on the binary destiny stream. | destiny 100%. |
| Z-4 | Close every `sde:` row and `effect:*`. | sde and effect 100%. |
| Z-5 | Close every `feat:` row, including every row the gap auditor has appended. Fix every auditor `fake_done` flag until a later audit clears it. | feat 100%, 0 flagged fake. |
| Z-6 | Pass every `journey:` row on the real client through `tools/client-harness/run_journeys`, with clean logs. | journey 100%. |
| Z-7 | Reach workspace line coverage ≥ 90% and mutation score ≥ 85% on every crate. | Both thresholds met in `final_gate.py`. |
| Z-8 | **Final acceptance.** The claims review is `PASS` and the gap audit is `CLEAN` on the final tree, and `final_gate.py` exits 0. Only then tell the user the project is complete, with the gate report path. | `.evomu-reviews/final_gate.json` shows `"passed": true` for HEAD's tree. |

---

## Out of scope / obsolete in V24
POS towers and reactions at POS (use Upwell refineries) · medical clone grades · LSC protocol (use XMPP) · XML API server · live updates · the pre-2013 certificate system · 2010 factional warfare.

## Gaps Crucible can't help with (design from the V24 client and wiki)
Fleet warp · jump clone rules · invention details · implants · notification center · CONCORD spawning · wormhole lifetime · PI depletion · Quasar/`publicGatewaySvc` real protobuf handling · Upwell structure services · Alpha/Omega and injector edge cases.

---

## Workflow per task (agent checklist)
1. Pick the next task in plan order and list the ledger rows it closes. Use the `## First open rows` section of `.evomu-reviews/latest.md`, `parity.json`, and the `feat:` rows whose `plan_task` matches.
2. Read the Crucible reference listed for the task, the V24 client call sites (`references/parity/sources/rpc_service_catalog.json`, the client's own code) and the wiki, for exact method names, arguments and mechanics.
3. Write failing tests first (wire-level where the task touches the protocol), tagged `// parity: <row id>`.
4. Implement in Rust, re-implementing rather than copying. Keep handlers one per method. Use `try_with_mut_snapshot`. Use real SDE data only.
5. Run `cargo test --workspace` (it must actually execute) and `cargo mutants --in-diff` on your diff (≥85% caught).
6. Write `/tmp/evomu_claims.md`: task IDs, the ledger rows you claim closed, and exact claims. Run `scripts/preflight.py --claims /tmp/evomu_claims.md` until it prints `OK`, pasting numbers from its template.
7. Run `/usr/bin/python3 .agents/skills/evomu-adversarial-review/scripts/run_review.py --claims-file /tmp/evomu_claims.md`. Every few runs it also chains a gap audit that may append ledger rows. Those rows are now in scope.
8. Fix until there is no P0 and no FALSE claim. On PASS_WITH_ISSUES, commit right away and fix the P1s in the next increment. `docs/ACTION_PLAN_STATUS.md` is regenerated by pre-flight; record your per-task assessment and a short note in `docs/parity/task_notes.json`. Never edit this plan or the ledger. Commit with the task IDs.
9. Go back to step 1. When `parity.md` shows 100%, run `scripts/final_gate.py`, fix what it reports, and repeat until it passes.

## Status snapshot (maintained by the supervisor/user; live per-task status is in `docs/ACTION_PLAN_STATUS.md`)

> [!NOTE]
> Authoritative, machine-verified task and parity closure is generated into [`docs/ACTION_PLAN_STATUS.md`](../../../../docs/ACTION_PLAN_STATUS.md) on every preflight run from test attestations.

| Phase | Current Status | Milestone & Notes |
|---|---|---|
| **Phase 0: Stabilize** | **Partial (9/11)** | S-1 reprocessing duplicate handler (`BF-REPROC-DUP-01`) and S-7 remote-mutation rows in remediation. |
| **Phase 1: Data/persistence** | **Partial (4/6)** | Real SQLite SDE ingested; `D-3` and `D-6` (61 unsynced `WorldStateSnapshot` fields, ~25 `static` stores, ESI/Quasar/MachoNet split-brain) in active remediation. |
| **Phase 2: Wire protocol** | **Partial (6/7)** | Native `machoNet` handshake (`V24.01 build 3619879`), session change, binary destiny packets active; `P-5` notifications open. |
| **Phase 3: Space** | **Partial (4/7)** | 1 Hz `Destiny` simulation & binary stream active; `SP-4` gate jump polish, `SP-6` logoff/disconnect/login lifecycle, and `SP-7` background cluster scheduler & ballpark reload open. |
| **Phase 4: Dogma** | **Partial (3/6)** | `DG-1` modifier engine, `DG-4` module cycle engine (incl. no permanent buff persistence on crash), `DG-6` ammo/grouping open. |
| **Phase 5: Combat/PvE** | **Partial (1/8)** | `C-2`..`C-8` open (victim wreck ownership, pod implant destruction, production CONCORD/NPC AI scheduler, drone distance/abandonment). |
| **Phase 5M: Mining** | **Partial (0/18)** | 60/63 mining feature rows done; task-level acceptance criteria (`MN-01`..`MN-18`) in progress. |
| **Phase 6: Character/GM** | **Partial (1/5)** | `CH-1`..`CH-5` open (skill queue background notifications, biomass cleanup of orders/corp/fleet, GM role checks). |
| **Phase 7: Economy/missions** | **Partial (1/6)** | `E-2`..`E-6` open (order expiry escrow refund, `CompleteManyJobs` all IDs, PI `StartTicking`/`Flush` exploit fixes). |
| **Phase 8: Social/exploration** | **Partial (0/7)** | `SO-1`..`SO-7` open (fleet disconnect/boss handover, single-corp invariant & invite consent, 0.5% same-system asset safety, persisted bookmark folders/expiry). |
| **Phase T: Test/acceptance infra** | **Partial (4/12)** | Unified test runner (`tests/it/main.rs`), 799 passing workspace tests; `T-10` `client_expectations.json` pinned; `T-2`, `T-4`, `T-6`, `T-8`, `T-9`, `T-11` (Destiny differential), `T-12` (Dogma differential) open. |
| **Phase Q: Quasar backends** | **Partial (1/9)** | 1,328 client-internal protos NA; ~60 `quasar:` rows reopened by gap audits for SQLite persistence and live semantics. |
| **Phase ESI: ESI + SSO** | **Partial (3/7)** | 197 ESI routes implemented; SSO launcher login (`ESI-1`) and shared SQLite state with MachoNet (`D-6`) open. |
| **Phase Z: Ledger closure** | **74.83% Overall Parity** | **4,750 / 6,349 total ledger rows closed**, 352 `fake_done` rows under active remediation, 0 Santa blocks. |
