# Open findings carried forward (re-verified 2026-10-22 against `df2894b`)

Re-check each item and mark it FIXED / PARTIAL / OPEN / REGRESSED with `file:line` evidence. IDs are stable. Items verified FIXED or OBSOLETE on 2026-10-22 were removed (OF-01..11, OF-13, OF-14, OF-17, OF-20, OF-21, OF-23..27, OF-29, OF-30, OF-32); their history is in git. If one regresses, re-open it under its old ID.

## Partial or open
- **OF-12 (structure docking ACL):** `Dock` now checks distance, in-space and already-docked, but the structure ACL is a process-global deny-set plus a hardcoded corp `98_000_099` (`station_structure.rs`), not structure access profiles.
- **OF-15 (hollow handlers):** named examples (slash.SlashCmd, ship.Board, AssembleShip) now do real work. `metrics_stubs.txt` still lists literal-stub arms (33 arms / 39 pairs at `98587ac`). Re-measure each pass and cite the metrics file.
- **OF-16 (shared bodies):** 25 arms serve 5+ methods (141 of 1452 pairs at `98587ac`). Example of wrong semantics: the `incursion.*` arm in `pve_special_sites.rs` returns one keyval for 5 methods, including `JoinIncursionChat`.
- **OF-18 (dogma expression VM):** the modifierInfo engine (`eve-dogma/src/context.rs`) is real, but the expression VM returns `1.0` for `SkillCheck`/`VerifyTargetGroup`/`ToolTargetSkills`, and has a catch-all `_ => value_f64.unwrap_or(1.0)`.
- **OF-19 (LOD):** systems hibernate when idle, but players are classified by `id < 700_000_000` (`system_actor.rs`), and NPC ball IDs (200M+) count as players.
- **OF-22 (client bridge):** `evomu_bridge.py` runs only inside the harness's fake `HarnessSM`; the real client never loads it.
- **OF-28 (mining notifications):** `OnMiningLaserFiredLocal` is never emitted; the `charMiningLedger` service name isn't in `docs/index/rpc_service_catalog.json`.
- **OF-31 (legacy combat path):** `dogmaIM.FireWeapon` with 1 argument and `beyonce.CmdAttackTarget` still route into the legacy combat engine (`combat_legacy_ess.rs`). Group 46 is still special-cased in fitting.
- **OF-33 (Jita fallbacks) — worse:** about 95 non-test `unwrap_or(60003760|30000142)` fallbacks hide missing data (was 21).
- **OF-36 (derived data labelling):** mission ISK is formula-based with no DERIVED label (`agents_missions_helpers.rs`). NPC spawn groups are labelled only in an importer docstring; the `sde_npc_spawn_groups` table has no provenance column.
- **OF-37 (non-client names):** `rpc_names` reports 0 NOT_IN_CLIENT and 2143 NOT_IN_CATALOG (at `98587ac`). Still live: 1-argument `FireWeapon`, and `*ToPoint` suffix dispatch. 84 `rpc-lint: allow` comments exist (23 in non-test code); check every reason.

## New (2026-10-22 verification)
- **OF-38 (state outside the DB):** about 25 process-global statics hold gameplay state: `WAR_STATE`, `CORP_GOV`, `EMP_HISTORY`, `BM_SCAN_STATE`, `STRUCTURE_DOCKING_ACL_DENIED`, `INSURGENCY_RUNTIME`, and the combat runtime incl. `player_bounty_pool`. They are not persisted, not rolled back by `try_with_mut_snapshot`, and lost on restart.
- **OF-39 (build-number mismatch):** the handshake uses `V24_BOOT_BUILD = 3_624_425` (`eve-machonet/src/lib.rs`), while `cache.GetServerVersionAndBuild` returns `3_619_879` (`auth_cache.rs`).
- **OF-40 (suffix dispatch hides names):** `beyonce.*ToPoint` and `*ChangeTimer` are routed by `ends_with` (`space_warp_jump.rs`), so those names never reach the RPC-name lint.
- **OF-41 (duplicate handler arms):** `wormholeMgr.WormholeJump` has two different implementations (`space_cyno_filaments.rs`, `fleet_social_scanning.rs`); one is dead, and tests may exercise the wrong one.
- **OF-42 (NPC spawning):** belt rats pick `candidate_groups[idx % len]` across all derived groups, ignoring system security and faction despite the doc comment (`combat/npc_ai.rs`). With no belts they fall back to a fixed position. Systems step physics only when someone is subscribed (`receiver_count() > 0`), so NPC AI and in-flight warps freeze in unobserved systems.

## New (2026-10-23 Opus 5.5 High overnight audit against `26720183`)
- **OF-44 (Quasar user entitlement self-grant):** `eve.entitlement.user.api.GrantRequest` has no admin/role check; any user can self-grant arbitrary entitlements, now persisted in DB (`quasar_entitlement_membership.rs:113-140`).
- **OF-45 (Quasar PLEX & item minting):** `DepositRequest` adds client-supplied PLEX; `WithdrawRequest` with negative values mints PLEX; `DeliverItemRequest` spawns arbitrary typeID and quantity in Jita 4-4 hangar without role checks (`quasar_payment_store.rs:42-67, 100-125, 350-383`).
- **OF-46 (Corp governance share takeover):** Calling `corpRegistry.GetShareholders` on a corp with unseeded shares assigns caller 100% of shares, enabling full dividend payout theft (`fleet_social_corp_gov.rs:326-330, 343-347, 411-421`).
- **OF-47 (Bounty NaN/inf infinite pool & domain wipe):** `AddToBounty` accepts NaN/inf without validation, costing 0 ISK while creating infinite payout pool. Serialized as `null`, wiping the entire crimewatch domain on reload (`crimewatch_timers.rs:1040-1073`, `d6_corp_crimewatch_store.rs:318-325`).
- **OF-48 (Crimewatch lost updates & split money moves):** `with_mut_combat_runtime` has lost updates under concurrency; wallet debit and pool credit occur in separate non-atomic transactions (`combat_crimewatch.rs:492-511`, `crimewatch_timers.rs:1047-1075`).
- **OF-49 (ABBA deadlock between systems and DB):** `get_or_spawn` holds `systems.write()` while acquiring DB snapshot; `on_advance_secs` holds DB write lock while calling `systems.read()` via `is_ball_scrambled_sync` (`system_actor.rs:624-647`, `cluster_tick.rs:967`).
- **OF-50 (SelectCharacterID mid-flight teleport & lock drop):** Invoking `SelectCharacterID` on active character in space teleports to stale coordinates or `[0,0,0]` and drops all target locks, bypassing combat timers (`character_gm_tools.rs:434-436`, `sp6_lifecycle.rs:806-854`).
- **OF-51 (Non-turret weapons omit victim PvP timer):** Missiles, AoE smartbombs, drones, and fighters only trigger attacker weapon timers, leaving the victim free to emergency-warp after 60s or SafeLogoff in 30s (`combat_missiles_aoe.rs:445-480`, `combat_npcs_drones.rs:1013-1038`, `sp6_lifecycle.rs:729-735`).
- **OF-52 (Exploit assertions in test suite):** Test suite in `d6_persistence_industry_market_pve_ship.rs:609-612, 689` and `:597-599, 681` asserts that unauthenticated entitlement self-grant and repeated tutorial rewards succeed.

## Standing rules (always check)
- **OF-34:** every number in claims and docs must trace to tool output in the context; an untraceable number is a FALSE claim.
- **OF-35 (updated 2026-10-22 per user decision):** large binary databases (`data/*.sqlite`, `data/*.ccp`) stay gitignored (`data/`), while client-extracted reference catalogs and development reference extracts (such as `references/parity/sources/client_expectations.json` and `data/sde_import_manifest.json`) are permitted in git during active development; temporary client references will be removed and commit history reset before a public release. Do NOT raise findings for tracked client-derived development catalogs/manifests.
