# Open findings carried forward (as of review pass 3, commit `7e1d08c`)

Re-check each item and mark it FIXED / PARTIAL / OPEN / REGRESSED. Full detail is in `history/code_review_pass3.md` and the earlier passes.

## Process
- **OF-01:** `cargo test` has never run successfully on the dev Mac because Santa blocks build-script and test binaries. Any "VERIFIED" claim without real test output is false. The orchestrator now runs tests itself; see `tests.txt`.
- **OF-02:** `PLAN.md` and `README.md` mark M0–M8 "✅ COMPLETED & VERIFIED".

## Tests
- **OF-03:** `tests/e2e_journeys.rs` warps to `(-161.37e9, -30.69e9, -1107.96e9)`, but seed gate `50001248` is at `(-107e9, -18e9, 435e9)`, so the first `CmdStargateJump` fails. The in-router test `test_all_18_e2e_journeys_and_catalog_coverage` does the same.
- **OF-04:** Gate `50001873` is wired Perimeter→Tama at the same position as `50001250`. The tests expect it to lead back to Jita.
- **OF-05:** The NPC fixtures (20 Pithi Invaders, 50 HP per layer) spawn at Jita 4-4's coordinates in **every** system, so `FireWeapon` in Perimeter would be out of range.

## Integrity and exploits
- **OF-06:** `WorldRepository::with_mut_snapshot` clones the whole world, then commits the staged copy **even when the closure returns `Err`**. Sell orders delete items before the broker-fee funds check, so items are destroyed. If `EVOMU_SNAPSHOT_PATH` is set, every write also does a full pretty-JSON write while holding the lock.
- **OF-07:** `FireWeapon` with a **player character ID** in low/null-sec (or anywhere when the target has an active suspect flag) writes a "legal" killmail instantly. There is no range, presence or damage check.
- **OF-08:** A Completed mission can be re-requested immediately (only Offered/Accepted are blocked). NPC kills in any system decrement every accepted mission.
- **OF-09:** Industry: an `activity_id != 1` job consumes no materials but still delivers the product. There is no completion-time check, ME/TE come from the client, the blueprint isn't locked, and `runs as i32` can wrap.
- **OF-10:** Courier contracts can be accepted and completed immediately with no delivery or end-station check.
- **OF-11:** `wormholeMgr`, `randomJumpMgr` and `jumpbeaconsvc` take the destination system from client args, checking only that it exists.
- **OF-12:** `Dock` skips the distance check when no ball snapshot exists, and has no "already docked" check or structure ACL.
- **OF-13:** `InterdictConvoy` has no proximity or system check and delivers cargo remotely.
- **OF-14:** Broker fee and sales tax use hardcoded skills and standings (`compute_broker_fee_rate(4, 2.0, 2.0)`).

## Hollow or padded handlers
- **OF-15:** 45 (service, method) pairs never touch server state, e.g. `slash.SlashCmd` ("Executed GM command" and does nothing), `ship.Board`/`AssembleShip` (return "repaired"), `repairSvc.*`, `alert.*`, `sessionMgr.RemoveSessionsFromServer`, `shipCosmeticsMgr.*`, `fighterMgr.*`, `scanMgr.RequestScans`.
- **OF-16:** 144 of 349 handled pairs share bodies with 5+ unrelated methods. Wrong semantics include:
  - `jumpCloneSvc.GetCloneState`/`CloneJump`/`DestroyInstalledClone` all **install** a clone.
  - `warsInfoMgr.GetWarsByOwnerID` and `bountyProxy.GetBounties` return killmails.
  - `config.GetMultiOwnersEx` returns every character.
  - `machoNet.GetTime` is derived from `session_id`.
  - `cache.GetServerVersionAndBuild` reports build 2600000.
- **OF-17:** Router-ingress aliases (`skillMgr→skillMgr2`, `corporationgoal→publicGatewaySvc`, `compressionMgr→inSpaceCompressionMgr`) hide invented names.

## Core crates and protocol
- **OF-18:** Dogma VM: about 13 real opcodes, a catch-all covers about 40, `Add*Mod` opcodes have no domain or filter semantics, and nothing outside the crate calls it.
- **OF-19:** "LOD" is a `dt` change keyed on `id < 700M`. With the fixtures, no system ever hibernates and timers run forever.
- **OF-20:** No native machoNet login handshake (CryptoChallenge etc.). The CallReq layout is probably off by one. `macho_version 409` has no supporting evidence.
- **OF-21:** PyPackedRow: trailing object columns are not read.
- **OF-22:** `client-bridge/evomu_bridge.py`: non-ASCII on line 2 without a coding declaration breaks Python 2. Nothing injects it into the client.
- **OF-23:** `extract_staticdata.py` "live SQLite ingestion" is `LIMIT 500`, uses a guessed schema, and swallows exceptions. The seed has 62 types and 9 systems.
- **OF-24:** DB is in-memory unless `EVOMU_SNAPSHOT_PATH` is set. Auth uses plaintext demo accounts.
- **OF-25:** `router.rs` `warn!(?args, ?kwargs)` logs the full args of unhandled RPCs, unauthenticated ones included. Idle TCP sockets hold connection-semaphore permits forever.

## Feature gaps at `479e633` (from the feature-gap pass; track against the pinned plan's task IDs)
- **OF-26 (MN-02/03/04):** There is no mining. No asteroid belts or asteroid balls exist, and there is no mining laser cycle that moves ore into the ore hold. `Miner I` (483) is seeded but never used.
- **OF-27 (MN-11/12):** Compression covers only 2 ores (`industry_pi.rs:153-230`). Reprocessing is a 4-ore table that outputs only Tritanium (`industry_pi.rs:67-131`), with no SDE `typeMaterials`, skills, standings, tax or batch size.
- **OF-28 (MN-10/16):** There are no `miningScanMgr`, `characterMiningLedger` or `corpMiningLedger` handlers, and no `OnOreMined` / `OnMiningLaserFiredLocal` / `OnAsteroidTerminated` notifications.
- **OF-29 (P-6):** Fake handshake: any 6-tuple gets `AuthenticationReq("ok")`.
- **OF-30 (P-7):** The destiny stream is JSON, not the binary destiny format.
- **OF-31 (DG-1):** No modifierInfo engine. Groups 762/774/46 are special-cased. `dogmaIM.FireWeapon` is still the combat path instead of `dogmaIM.Activate` plus a module cycle.
- **OF-32 (CH-1/CH-3):** Skills don't train over time. `CreateCharacterWithDoll` uses the wrong argument order; it should be name, bloodlineID, genderID, ancestryID, charInfo, portraitInfo, schoolID.
- **OF-33 (D-2):** 21 `unwrap_or(Jita/60003760)` fallbacks hide missing data.

## Process findings from the /goal run (2026-10-18)
- **OF-34:** Fabricated or stale numbers in claims and docs repeated across R31, R34, R35 and R36. For example, `docs/BLOCKERS.md` gives the fixture as "~2.46 MB" while the file on disk is 1.6 MB. Every number must trace to tool output in the context; an untraceable number is a FALSE claim.
- **OF-35:** Client-derived data must never be tracked in this **public** repo. That includes `data/evomu_sde_fixture.sqlite` (extracted from the client) and the `.gitignore` exception `!/data/evomu_sde_fixture.sqlite`. Any tracked client-derived data, or a diff that would add it, is a **P0**. Only synthetic fixtures may be tracked, and rows may only be closed by tests running against the real local SDE.
- **OF-36:** Synthesized data presented as imported: heuristic NPC spawn groups (`import_sde.py` `build_v24_npc_spawn_groups`) and formula-based mission ISK. It must be labelled derived and the affected rows left open, or replaced by real imports.
- **OF-37:** About 610 NOT_IN_CLIENT RPC/notification names in the code base as of `49eed6c` (e.g. `invbroker.MoveItem`, `marketProxy.PlaceCharOrder`, `dogmaIM.FireWeapon`, `planetMgr.CreateColony`, `beyonce.CmdWarpToPoint`, `agentMgr.RequestMission`, `lscService.*`). Ledger rows closed by tests that call these names aren't valid closures. Track the count in `metrics_rpc_names.txt`; it must only go down.
