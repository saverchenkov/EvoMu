# PLAN.md review, pass 2

**Bottom line:** This pass fixed a lot. The §3.12 mapping now checks out against the catalogs, most of the factual errors are gone, and there are real milestones and a legal section. What remains falls into three groups:

1. Claims of *completion* that nothing on this machine actually backs up.
2. Numbers in the plan that the generated artifacts don't support.
3. Service names in the prose outside §3.12, which the new verifier doesn't check.

---

## ✅ Fixed, and confirmed

| Earlier finding | Evidence |
|---|---|
| §3.12 table invented or missed names | `verify_plan_mapping.py --check` → 217/217 services and 135/135 domains, 0 invented, 0 missing |
| Crucible class names and `.pyj` files counted as services/domains | Crucible names are now merged into V24 names; `.pyj` pseudo-domains removed |
| School corp IDs | Corrected (e.g. SoAK `1000044`, Caille `1000115`, Minmatar schools `1000170–172`) |
| SP ceiling equality, stacking-penalty indexing, Vorton count, Epic Arc count, wrecking-shot rule | All corrected and consistent |
| CONCORD timing labels, Bastion fuel, per-rack heat, Mobile Observatory 10 min, kill-right scope | Corrected, with wiki line citations |
| NPC attribute ID collisions | I checked all **45** IDs in [§3.10.1](file:///Users/sergeya/ai/EvoMu/PLAN.md#L629-L637) against Crucible `AttributeEnum.h`. All match. |
| No MVP, no legal note, no LOD/economy constraints | M0–M8 milestones added, plus §6 (legal) and §3.9.3.4 (LOD/economy limits) |

---

## P0: still open

### 1. "M0 COMPLETED" and "currently implemented" rest on Rust code that has never been compiled
- 1,236 lines of Rust were added after the only commit (`2c4c3f5`, 09:44). The files were changed between 10:22 and 10:30 today, and **there is no `rustc`, `cargo` or `rustup` on this host**.
- [M0's exit criteria](file:///Users/sergeya/ai/EvoMu/PLAN.md#L889-L891) list only the Python self-test and the plan verifier. `cargo build`/`cargo test` was left out, and M0 is still marked COMPLETED.
- [§2.1](file:///Users/sergeya/ai/EvoMu/PLAN.md#L35) links to `crates/eve-server/src/lib.rs`, which doesn't exist.
- None of this work is committed yet: PLAN, README, the Rust changes, `docs/index/` and `tools/` are all uncommitted or untracked.

**Fix:** mark M0 "pending Rust verification" and add `cargo build && cargo test --workspace` to its exit criteria. Then install the toolchain or run it on the Windows box mentioned in AGENT_HANDOFF, and commit.

### 2. Some numbers in the plan don't match the artifacts
| Claim in PLAN | What the artifact says |
|---|---|
| "217 services = 193 V24 + **24** legacy" | `crucible_comparison.json` lists **23** legacy services. The 217th key is `auth`, which has **no call sites and no Crucible class**, so the count is padded by one. |
| "**13** `DSTBALL_*` ball modes, **28** `DoDestinyUpdate` handlers" ([L98-L99](file:///Users/sergeya/ai/EvoMu/PLAN.md#L98-L99)) | `destiny_opcodes.json`: `"ball_modes": {}` (empty), and `v24_destiny_actions` has **5** entries. The file is 1 KB. |
| "`1,390` attribute and `822` effect constants" ([L97](file:///Users/sergeya/ai/EvoMu/PLAN.md#L97)) | `dogma_effects.json`: **1,343** and **305** |
| "2,854 Dogma effects" presented as the V24 effect set | These come from Crucible's 2010-era `dgmEffects.sql.gz`. The V24 effect set should come from the client's `dogmaeffects.fsdbinary`. |
| "135 clean `eve_public.*` domains" | **66 of the 135 have no `eve_public/` files at all.** They are internal `eveProto/generated/eve/*` schemas, not the public gateway the client calls. |

**Fix:** have the verifier generate or check every count quoted in §1–§2 directly from the JSON files. Drop `auth` (or give it evidence). Split the Quasar catalog into public and internal domains.

### 3. The verifier only checks §3.12, so service names elsewhere have drifted
About 20 service-like names elsewhere in the plan are not catalog keys. Several of them appear in **milestone deliverables**:
- `contractMgr` (M3) → the catalog has `contractProxy`
- `scanSvc` (M7) → `scanMgr`
- `dynamicItemSvc` (M2) → `dynamicItemService`
- `cloneGradeSvc` (M2), `securityOfficeSvc` (M7), `insurgencyCampaignSvc` and `corruptionSuppressionSvc` (M6), `incursionSvc` (M4) → no catalog entry
- `bookmarkSvc`, `raffleSvc`, `seasonMgr` (catalog: `seasonManager`), `cosmeticsLicenseMgr`, `structureCosmeticsMgr`, `shipProgressionNotifier`, `allianceTournamentSvc`, `cloneVatBayMgr`, `redeem`, `medalMgr`, `projectDiscovery` (catalog: `ProjectDiscovery`), `moonMining`
- `eve_public.accessgroup` → the catalog domain is `acl`

Also, [§3.5.1](file:///Users/sergeya/ai/EvoMu/PLAN.md#L245) cites `ui/shared/trade/tradeSvc.pyj`, but the catalog's evidence for `trademgr` is `ui/station/pvptrade/pvptradesvc.pyj`, and only `InitiateTrade` is evidenced. `Add`, `OfferMoney` and `ToggleAccept` are probably methods on the bound trade session object. They should be listed as such, or marked unverified.

**Fix:** extend `--check` so that any backticked `*Mgr|*Svc|*Service|*Proxy|*Manager` name must be a catalog key or appear on an explicit allowlist (client-side services such as `skillQueueSvc`, and space components such as `orbitalSkyhook`/`sovHub`).

---

## P1: correctness and design

4. **The Dogma pipeline is missing two operators.** The artifact lists `dgmAssPostPercentUnNerfed` (a percent bonus that is exempt from stacking penalties) and `dgmAssSpecialSkillRelationship`. The 6-stage pipeline in [§2.1](file:///Users/sergeya/ai/EvoMu/PLAN.md#L30) and §3.4 doesn't handle either, which will give wrong fits.
5. **The economy price clamp is undefined for non-manufactured items.** `P ∈ [0.5, 2.5] × C_build` ([L604](file:///Users/sergeya/ai/EvoMu/PLAN.md#L604)) has no `C_build` for loot, PLEX, injectors or faction/officer modules, yet §3.9.3.1 prices those by drop rate × demand. Define a second anchor for them. The LOD CPU and RSS targets also need a benchmark harness before they mean anything.
6. **Some numbers still have no source:**
   - `maxChainTargets` (Vorton) — the attribute name isn't verified
   - "175 allowed skills" (§3.2)
   - "`188` goals in `careerpathgoals.fsdbinary`" (M4)
   - "100 components" (§3.4/M6)
   - Frigate Escape Bay on capitals
   - The on-grid/pulled CONCORD times for 0.7–1.0 are interpolated; the wiki only gives the 1.0, 0.6 and 0.5 values

   Tag each as `source:` or `unverified`.
7. **The §6 "100% clean-room" wording is wrong for this workflow.** Legally, clean-room means the people who write the spec are separate from the people who write the code. Here the same agent reads Crucible C++ and disassembled CCP bytecode, then writes the Rust. Call it an "independent implementation" instead. Also note there is no `LICENSE` file, and check evemu_Crucible's license before porting logic from it.
8. **[docs/AGENT_HANDOFF.md](file:///Users/sergeya/ai/EvoMu/docs/AGENT_HANDOFF.md) is stale.** It still points to a Windows path (`c:\code\evomu`) and the old step plan, which conflicts with §4's "Agent Handoff Plan". Update or remove it.
9. **Milestone gates are still coarse.** Some examples:
   - E2E-05 (the M2 gate) bundles CONCORD, insurance and medical-clone respawn.
   - E2E-14 (the M4 gate) requires killing an Incursion Mothership.
   - M7 bundles exploration, special weapons, officers and all of the Tier-2 AI.
   - Kill rights are scheduled in M8, even though CrimeWatch ships in M2.

   Give each milestone smaller unit/contract-test targets, and keep the mega-journeys as release smoke tests.
10. **The document got bigger instead of being split.** It is now 184 KB and 1,013 lines, up from 164 KB. Splitting it into `docs/spec/*` and keeping PLAN.md as status plus milestones is still worthwhile.

---

## Suggested order
1. Make the Rust build verifiable (toolchain, `cargo test`), fix the M0 status, and commit.
2. Have the verifier check every count in §1–§2 against the artifacts. Fix the `auth` padding, the destiny/dogma numbers and the public/internal Quasar split.
3. Extend the verifier to cover service names in the prose, then fix the ~20 drifted names, starting with the ones in milestone deliverables.
4. Add the missing Dogma operators and the second price anchor for non-manufactured items.
5. Clean up the §6 wording and AGENT_HANDOFF, and split the document.
