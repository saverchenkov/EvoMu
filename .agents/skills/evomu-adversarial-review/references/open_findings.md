# Open findings carried forward (re-verified 2026-10-24 against `2f23c1b9`; mirrored in `docs/OPEN_FINDINGS.md`)

Re-check each item and mark it FIXED / PARTIAL / OPEN / REGRESSED with `file:line` evidence. IDs are stable. Items verified FIXED or OBSOLETE were removed from the active list (`OF-01..11, OF-13, OF-14, OF-17, OF-20, OF-21, OF-23..27, OF-29, OF-30, OF-32` on 2026-10-22; `OF-38` and `OF-44..52` on 2026-10-24 after `fa6c80e9` and `security_p0_exploits.rs`); their full history and resolution evidence are tracked in [`docs/OPEN_FINDINGS.md`](../../../../docs/OPEN_FINDINGS.md). If one regresses, re-open it under its old ID.

## Partial or open
- **OF-12 (structure docking ACL):** `Dock` and `structure_access_allowed` now share `evaluate_character_structure_access_in_snap`, but `station_structure.rs:645` still has a hardcoded `st.owner_corp_id == 98_000_099` check, and V24 `structureSettings` profile-configuration RPCs need to be wired to `configure_structure_access_profile` / `modify_structure_access_node`.
- **OF-15 (hollow handlers):** `metrics_stubs.txt` lists **34** stubbed `(service, method)` pairs at `2f23c1b9`. Re-measure each pass and cite the metrics file.
- **OF-16 (shared bodies):** **17** arms serve 5+ methods (`metrics_padding.txt` at `2f23c1b9`, down from 25). Example: the `incursion.*` arm in `pve_special_sites.rs` returns one keyval for 5 methods, including `JoinIncursionChat`.
- **OF-18 (dogma expression VM):** the `modifierInfo` engine (`eve-dogma/src/context.rs`) is verified against CCP's bundled Python 2.7 Dogma runtime (`plan:T-12` `DONE`), but the legacy expression VM (`eve-dogma/src/lib.rs:329`) still returns `1.0` for `SkillCheck`/`VerifyTargetGroup`/`ToolTargetSkills`.
- **OF-19 (entity ID threshold):** combat and NPC AI (`combat_weapons.rs:352,762`, `combat_npc_ai.rs:299,425`, `fitting_ewar_capital.rs:210,611`) still classify balls using `b.id < 700_000_000`.
- **OF-22 (client bridge):** `evomu_bridge.py` runs inside `run_journeys`'s `HarnessSM` (`plan:T-7` `DONE`); the real `EVE.app` client journeys (`plan:T-6`) remain open (`docs/BLOCKERS.md` §3).
- **OF-28 (mining notifications):** `OnMiningLaserFiredLocal` is never emitted; the `charMiningLedger` service name (`space_cyno_filaments.rs:64`) isn't in `docs/index/rpc_service_catalog.json`.
- **OF-31 (legacy combat path):** `dogmaIM.FireWeapon` and `beyonce.CmdAttackTarget` still route into `combat_legacy_ess.rs:69,78`.
- **OF-33 (Jita fallbacks):** down from 95 to **11** non-test `unwrap_or(60003760|30000142)` fallbacks across `crates/`.
- **OF-36 (derived data labelling):** mission ISK is formula-based with no `DERIVED` label (`agents_missions_helpers.rs`). NPC spawn groups are labelled only in an importer docstring; the `sde_npc_spawn_groups` table has no provenance column.
- **OF-37 / OF-40 (non-client names & suffix dispatch):** `rpc_names` reports `0 NOT_IN_CLIENT` and `2517 NOT_IN_CATALOG` (at `2f23c1b9`). `beyonce.*ToPoint` and `*ChangeTimer` are still routed by `ends_with` (`space_warp_jump.rs:65-68`), hiding them from the RPC-name lint.
- **OF-39 (build-number mismatch):** the handshake uses `V24_BOOT_BUILD = 3_624_425` (`eve-machonet/src/lib.rs:434`), while `cache.GetServerVersionAndBuild` returns `3_619_879` (`auth_cache.rs:17`).
- **OF-41 (duplicate handler arms):** `wormholeMgr.WormholeJump` has two different implementations (`space_cyno_filaments.rs:32`, `fleet_social_scanning.rs:702`); the second is shadowed and dead.
- **OF-42 (NPC spawning):** `tick_background_cluster` steps unobserved systems, but belt rats still pick `candidate_groups[idx % len]` across all derived groups without filtering by system security and regional faction (`combat/npc_ai.rs`).

## Standing rules (always check)
- **OF-34:** every number in claims and docs must trace to tool output in the context; an untraceable number is a FALSE claim.
- **OF-35 (updated 2026-10-22 per user decision):** large binary databases (`data/*.sqlite`, `data/*.ccp`) stay gitignored (`data/`), while client-extracted reference catalogs and development reference extracts (such as `references/parity/sources/client_expectations.json`, `data/sde_import_manifest.json`, and golden fixture JSONs) are permitted in git during active development; temporary client references will be removed and commit history reset before a public release. Do NOT raise findings for tracked client-derived development catalogs/manifests.
