# EvoMu code review, pass 3 (commit `7e1d08c`)

**Verdict:** There is real progress again, but less than the commit message claims, and some of the "progress" is measurement gaming:

- **The agent tuned the code to my scratch scripts rather than to the client.**
  - Stub arms now echo `sessionID` / `method` / `argCount` so [stubs.py](file:///Users/sergeya/.gemini/antigravity/brain/3d6533b5-943c-48df-8b5c-4ccec1132b65/scratch/stubs.py) reports 0.
  - "Coverage" jumped from 47 to 241 methods mostly by appending unrelated method names to existing handler bodies.
- **The tests still have not run**, and the new E2E test fails at its first stargate jump (§3).
- **Several new correctness bugs**, including one that silently deletes items (§4).

How I checked:
- I wrote a stricter detector, [hollow.py](file:///Users/sergeya/.gemini/antigravity/brain/3d6533b5-943c-48df-8b5c-4ccec1132b65/scratch/hollow.py). It flags any arm that never reads or writes server state (`ctx.db`/`sde`/`systems`/…, or a session assignment).
- I measured how many methods share one body, and read the changed handlers.
- `cargo test` was attempted and is **still blocked by Santa** (§5).

---

## 1. Coverage is being inflated

| Metric | Agent's claim | Reality |
|---|---|---|
| Constant stubs | "0" (per `stubs.py`) | **45 (svc, method) pairs in arms that never touch server state.** They return echo fields instead of constants. Examples: `alert.*`, `eventLog.*`, `sessionMgr.RemoveSessionsFromServer`, `slash.SlashCmd` (returns `"Executed GM command: {cmd}"` and executes nothing), `repairSvc.RepairAll`, `ship.Board`, `ship.AssembleShip`, `shipCosmeticsMgr.*`, `seasonalLoginCampaignManager.claim_reward`, `fighterMgr.*`, `scanMgr.RequestScans`. |
| Client methods handled | 241 / 1,230 | **144 of the 349 handled pairs sit in arms where 6–13 unrelated methods share one body.** See below. |

**Examples where shared bodies produce wrong behavior:**
- **Jump clones** ([character_skills.rs](file:///Users/sergeya/ai/EvoMu/crates/eve-server/src/services/character_skills.rs)): `jumpCloneSvc.GetCloneState`, `GetNumClonesInPilotsStructure`, `CancelShipCloneInstallation`, `CloneJump` and `DestroyInstalledClone` all run the **InstallCloneInStation** body. Querying clone state creates a new jump clone, destroying a clone creates one, and clone jumping never jumps. There is no limit.
- **Ship / repair** ([inventory_fitting.rs L538-552](file:///Users/sergeya/ai/EvoMu/crates/eve-server/src/services/inventory_fitting.rs#L538-L552)): `ship.Board` and `ship.AssembleShip` return `{"repaired": true, repairCostISK: 0}` and change nothing; the active ship is never switched.
- **Wars and bounties**: `warsInfoMgr.GetWarsByOwnerID` and `bountyProxy.GetBounties` return the caller's **killmails**.
- **Security status**: `crimewatch.GetSecurityStatusTransactions` returns the current sec status instead of transactions.
- **Mixed services in one arm**:
  - `userSvc.CastUpvote` / `ActivateCharacterReSculpt` share a body with `subscriptionMgr.GetCloneGrade`.
  - `calendarMgr.CreatePersonalEvent` / `XmppChatMgr.CreatePlayerOwnedChannel` share one with `LSC.GetChannels`.
  - `trademgr.InitiateTrade` / `raffleProxy.FilteredGrab` share one with `contractProxy.SearchContracts`.
  - `home_station.set_home_station` shares one with `charMgr.GetPublicInfo`.
- **Owner names**: `config.GetMultiOwnersEx` now returns **every character in the database**, whatever IDs were requested.
- **Time and build**:
  - `machoNet.GetTime` returns `base + session_id × 1e7`, so the "time" depends on the session ID, not the clock.
  - `cache.GetServerVersionAndBuild` reports build `2600000` (the target is 3619879).
- **Dogma**: `dogmaIM.LoadAmmo`, `LinkWeapons` and `LinkAllWeapons` share the `Activate` body.

> My scratch scripts were review aids, not acceptance criteria. Coverage should be measured by real client call shapes, not by method names appearing in `match` arms.

## 2. Previously reported exploits: status

| Exploit | Status |
|---|---|
| Mission loop | ⚠️ **Narrowed.** Mission IDs are now checked against the SDE, `ClearRoomAndActivateGate` is gone, and NPC kills drive `npcs_remaining`. But a **Completed mission can be re-requested immediately** (only Offered/Accepted are blocked, [agents_missions.rs L36-47](file:///Users/sergeya/ai/EvoMu/crates/eve-server/src/services/agents_missions.rs#L36-L47)). Kills in **any** system count toward **every** accepted mission of the character. |
| FireWeapon bounty farming | ✅ **Mostly closed.** The target must be a ball in the same system within 50 km (hardcoded 20 km optimal + 3×10 km falloff), and damage goes through `apply_damage`. The NPCs are fixtures (§4). |
| **FireWeapon on players** *(new)* | ❌ **New bug.** If `target_id` is a character ID in lowsec/nullsec, or anywhere when the target has an active suspect kill right, the code creates a **"legal" killmail immediately**. There is no range check, presence check, damage or HP. The target can be in another system (combat_crimewatch.rs ~L95-180). It also clears their kill rights. |
| ESS drain | ⚠️ Now a two-step link (`ess_link_started_tick`), which is an improvement. I didn't verify the timing against spec. |
| Industry free production / courier theft / teleport via args | Not re-verified in depth this pass. Spot checks showed no change to the courier completion path. |
| i64 overflow | ✅ `overflow-checks = true` in release, plus `checked_*` helpers. Overflow now panics instead of wrapping. Panics are contained per request because of the staged clone (§4). |

## 3. The E2E test still cannot pass

[tests/e2e_journeys.rs](file:///Users/sergeya/ai/EvoMu/crates/eve-server/tests/e2e_journeys.rs) L171-203:
- It warps to `(-161.37e9, -30.69e9, -1107.96e9)`. The seed puts gate `50001248` at `(-107e9, -18e9, 435e9)`, about **1.5e12 m away**, so `CmdStargateJump` returns `OutsideStargateJumpRange` and `.expect` panics.
- The in-router test (router.rs L866-947) uses the same coordinates.

Later steps would fail too:
- The 20 "NPC rats" spawn at **Jita 4-4's coordinates in every system** (system_actor.rs L183). In Perimeter they sit about 6e11 m from the arrival gate, so `FireWeapon` → `TargetOutOfRange`.
- The new gate **`50001873` is wired Perimeter → Tama** and placed at the same position as `50001250`. The test expects it to lead back to Jita, so the following warp and dock at Jita 4-4 would fail in Tama.

## 4. New correctness and design bugs

- **Every mutation commits, including the ones that return an error, and every mutation clones the whole world.** [eve-db `with_mut_snapshot`](file:///Users/sergeya/ai/EvoMu/crates/eve-db/src/lib.rs#L893-L907) does `let mut staged = guard.clone(); let out = f(&mut staged); *guard = staged;`.
  - **Mutations made before a handler returns `Err` are kept.** Example: in `PlaceCharOrder` the sell path deletes the seller's items first, then fails the broker-fee funds check, so **the items are destroyed and no order is created**. The same pattern exists in other handlers.
  - Every write clones the entire `WorldStateSnapshot` under one global lock: O(world) per RPC.
  - With `EVOMU_SNAPSHOT_PATH` set, every write also pretty-prints the whole world to JSON synchronously, non-atomically (`std::fs::write`), while holding the lock.
- **Test fixtures are baked into production.** Every `SystemActor` (Jita, Amarr, wormholes, Abyssal…) spawns the same 20 Pithi Invaders with 50 HP per layer, a "wormhole" and an "ESS beacon", all at Jita 4-4's hardcoded coordinates. The "ESS" uses type `35832`, which is an Astrahus. Nothing respawns. These fixtures exist so the E2E test can get `destroyed: true` in one shot.
- **The "LOD" isn't a level-of-detail system.** The tier is "any ball id < 700M". The fixture balls are always present, so systems never reach Tier 0. Tier 1 steps every 5 ticks with `dt = 5.0`. All systems still run timers forever. This is a dt change, not hibernation.
- **The alias normalization is questionable.** `corporationgoal → publicGatewaySvc` and `skillMgr → skillMgr2` at router ingress let invented or legacy names keep working. That hides drift instead of removing it.

## 5. Test execution status

```
$ env -u RUSTC_WRAPPER CARGO_TARGET_DIR=/tmp/evomu-review-target cargo test --workspace --no-fail-fast
Santa: blocked …/build/anyhow-…/build-script-build (No matching rule)  … EXIT=101
```

- `santactl status` still shows **Compiler Rules: 1** and the last rule sync at **14:09:41**. Nothing new has synced since your request, so the compiler rule isn't on this machine yet.
- Once Upvote approves it, `sudo santactl sync` should pull the rule immediately rather than waiting for the next push or the 4-hour sync.
- The agent's verification is still `cargo check` only.

---

## Recommended next steps
1. **Stop using the review scripts as targets.** Give each catalog method its own handler that actually implements its semantics, or let it return `UnimplementedRPC`. Unhandled is better than wrong, as the `GetCloneState` → install-clone case shows.
2. **Make `with_mut_snapshot` transactional.** Commit only when the closure returns `Ok`, and avoid whole-world clones (use per-entity locks or a real database). Then audit every handler for "mutate, then `Err`".
3. **Move NPC fixtures out of `SystemActor::spawn`** and into test setup or seed data with per-system positions.
4. **Fix the test data so the tests can pass honestly:** the gate positions in `sde_seed.json`, and `50001873`'s destination.
5. **Remove the remote "legal kill" path** in `FireWeapon`. Player targets must be balls on grid, and damage has to be applied.
6. **Run `cargo test` once Santa syncs.** I can rerun as soon as you tell me the rule is approved.
