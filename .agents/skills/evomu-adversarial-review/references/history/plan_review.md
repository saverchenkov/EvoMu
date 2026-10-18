# Review of `PLAN.md` (gap-analysis rewrite)

**Bottom line:** The domain breakdown and data-source tables are useful, and most of the core formulas are right. The "100% feature-match / zero unmapped" claim does not hold up, though. The headline mapping table (§3.12) does not match the catalogs it says it was built from. The baseline numbers are inflated. There are a dozen-plus factual errors and internal contradictions. And the plan has no prioritized, shippable milestones. I would not treat this as `GOAL_COMPLETE`.

Checks I actually ran: a script diffing §3.12 against `docs/index/*.json` ([audit_mapping.py](file:///Users/sergeya/.gemini/antigravity/brain/3d6533b5-943c-48df-8b5c-4ccec1132b65/scratch/audit_mapping.py)), spot-checks against `~/ai/eve-wiki/articles`, and `git diff` of PLAN.md. I could **not** run `cargo test` because there is no Rust toolchain on PATH, so the "Completed & Verified" status in §2 is still unverified.

---

## P0: Claims that are false or can't be verified

### 1. The §3.12 "205-service / 155-domain master mapping" doesn't match the catalogs
[PLAN.md §3.12](file:///Users/sergeya/ai/EvoMu/PLAN.md#L739-L753) says every catalog entry is assigned to a module. A programmatic diff shows otherwise:

| | Table → catalog | Catalog → table |
|---|---|---|
| RPC services | **107 names in the table are not in `rpc_service_catalog.json`** (e.g. `structureDocking`, `standing2`, `scanSvc`, `contractMgr`, `aggressionMgr`, `turretMgr`, `missileMgr`, `targetMgr`, `bombMgr`). Most look invented. | **103 catalog entries are unmapped** (e.g. `TradeService`, `killRightMgr`, `raffleSvc`, `repackagingSvc`, `solarsystemInterferenceMgr`, `shipCasterLauncherMgr`, `pvpFilamentMgr`, `abyssalMgr`, `alphaInjectorMgr`, `overviewPresetMgr`) |
| Quasar domains | **334 of 397 names are not catalog domains** (dotted inventions like `character.skill`, `market.order`, `structure.tether`) | **92 real domains are unmapped** (`assetholding`, `corporationgoal`, `dynamicbountysystem`, `piratestronghold`, `planetinteraction`, `spacecomponent`, `epicarc`, `factionwarfare`, …) |

The per-row "(`N` services)" counts are hand-typed, and the table has stray whitespace tokens (`` ` contractProxy` ``).
**Fix:** generate this table from the catalog with a script and add a CI check that fails on any unmapped or unknown name.

### 2. The baseline numbers are inflated
- "205 remote services": **57 of the 205 catalog keys are evemu_Crucible C++ class names with no V24 call sites** (`DogmaService`, `InsuranceBound`, `StationSvc`, `EntityBound`, …). Several of them duplicate real V24 services (`DogmaService`/`dogmaIM`). Only **147** entries have V24 client call sites.
- "155 Quasar domains": **21 of them are `.pyj` files** (`eve_pb2.pyj`, `isk.pyj`, …), not domains.
- Services the spec depends on are **missing from the catalog entirely**: `air_npe` (§3.8 gives exact method signatures for it), `trademgr`, `miningLedgerSvc`, `cloneSecurityTagMgr`. Those signatures should be labeled as unverified.

### 3. "Goal complete" overstates what was delivered
- This is a spec. `world_seeder/`, `living_world/`, `services/`, `space_components/` and `tests/e2e_journeys.rs` don't exist yet. `crates/eve-server/src` has only 3 files.
- The rewrite **removed 230 lines** of the previous plan. That included the indexing methodology and the step-by-step plan for the next agent. It also dropped promised artifacts (`crucible_comparison.json`, `dogma_effects.json`, `destiny_opcodes.json`) without delivering them. `docs/index/` contains only 6 files. These items should either be restored as open TODOs or explicitly descoped.

---

## P1: Factual errors and internal contradictions

| Location | Issue |
|---|---|
| [L77](file:///Users/sergeya/ai/EvoMu/PLAN.md#L77) | `⌈250·R·√32^(L-1)⌉ = R × {250, 1415, …}` is **false for R>1**. Example: Rank 2, L2 is 2,829, not 2,830. The ceiling has to be applied after multiplying by R. |
| [L25](file:///Users/sergeya/ai/EvoMu/PLAN.md#L25) vs [L212](file:///Users/sergeya/ai/EvoMu/PLAN.md#L212) | Stacking penalty is written as `e^{-(n/2.67)²}` in one place and `e^{-((n-1)/2.67)²}` in the other. Pick one indexing convention. |
| [L119](file:///Users/sergeya/ai/EvoMu/PLAN.md#L119) / [L225](file:///Users/sergeya/ai/EvoMu/PLAN.md#L225) / [L567](file:///Users/sergeya/ai/EvoMu/PLAN.md#L567) | Vorton arcs are given as "4–9", "9 additional" and "5–10 targets". The wiki says up to 9 additional. |
| [L146](file:///Users/sergeya/ai/EvoMu/PLAN.md#L146) / [L284](file:///Users/sergeya/ai/EvoMu/PLAN.md#L284) / [L425](file:///Users/sergeya/ai/EvoMu/PLAN.md#L425) | Epic arcs are counted as "11", 8 are listed, and "9" appears elsewhere. The wiki has 9. |
| [L623-L628](file:///Users/sergeya/ai/EvoMu/PLAN.md#L623-L628) | The CONCORD "unpulled" labels are **inverted**. Per `CONCORD.md` L79-81: already on grid is *faster* (2–9 s); pulled elsewhere in the system is *slower* (19 s at 0.6, 24 s at 0.5). |
| [L109](file:///Users/sergeya/ai/EvoMu/PLAN.md#L109) | Bastion consumes no fuel, and Liquid Ozone isn't a siege/triage/industrial-core fuel. |
| [L220](file:///Users/sergeya/ai/EvoMu/PLAN.md#L220) | The wrecking-shot rule says "x < 0.01·CTH **(or** x < 0.01)". A spec can't contain an "or" here; the standard rule is x < 0.01. |
| [L239](file:///Users/sergeya/ai/EvoMu/PLAN.md#L239) | Heat damage chance uses a whole-ship slot ratio. `Overheating.md` describes it per rack. |
| [L662](file:///Users/sergeya/ai/EvoMu/PLAN.md#L662) | Mobile Observatory deploy time is given as 60 s. The wiki says 10 minutes. |
| [L512-L513](file:///Users/sergeya/ai/EvoMu/PLAN.md#L512-L513) | Attribute ID collisions: `507` is both `EntityChaseMaxDuration` and `EntityMissileTypeID`, and `506` is both `ChaseMaxDelayChance` and `MissileLaunchDuration`. At least one of each pair is wrong. |
| [L46](file:///Users/sergeya/ai/EvoMu/PLAN.md#L46)-[L58](file:///Users/sergeya/ai/EvoMu/PLAN.md#L58) | School corp IDs look shuffled. `1000115` is assigned to Caldari's School of Applied Knowledge, and "Imperial Academy" (an Amarr school) is listed under Minmatar. Validate against `schools.fsdbinary`/`npccorporations.fsdbinary`. |
| [L163](file:///Users/sergeya/ai/EvoMu/PLAN.md#L163) vs [L709](file:///Users/sergeya/ai/EvoMu/PLAN.md#L709) | Kill rights are "unlawfully podded" in one place and "High/Lowsec ship or pod" in another. Per `Kill_Rights.md` L23, ordinary lowsec aggression gives a Suspect flag and **no** kill right. |
| [L393](file:///Users/sergeya/ai/EvoMu/PLAN.md#L393) | "4 × 10 activities = **188** goals" doesn't add up. |
| [L789](file:///Users/sergeya/ai/EvoMu/PLAN.md#L789) | Says "AIR NPE Acts 1–4" and "Career Agents (420 missions)". §3.8 defines 2 acts and "45 missions per set". |
| [L66-L67](file:///Users/sergeya/ai/EvoMu/PLAN.md#L66-L67), E2E-01 | "~400k SP starter skills" and the corvette fit have no source, yet E2E-01 asserts them. Derive them from data or drop the assertion. |
| [L145](file:///Users/sergeya/ai/EvoMu/PLAN.md#L145), [L751](file:///Users/sergeya/ai/EvoMu/PLAN.md#L751) | Stray leading spaces inside code spans (`` ` compressibletypes` ``, `` ` contractProxy` ``). |

Formulas I checked that **are correct**: align time, capacitor ODE and closed form, lock time, turret hit chance, missile application, warp phases, injector brackets, Omega SP rate, and asset-safety 0.5%/15%.

---

## P1: Plan quality and structure

1. **There's no MVP or vertical-slice milestone.** Phase 3 runs from character creation to Drifter AI. Phase 5 bundles sov, wormholes, Abyssal and SKINR together. Add numbered milestones with exit criteria, for example:
   - M1: real client login → hangar → undock → warp → gate → dock.
   - M2: fitting and NPC belt combat.
   - M3: market and industry.
2. **Hard-coded values belong in data, not in the plan.** A lot of the text is wiki facts: incursion payouts, officer names, FOB HP, Quantum Core prices. These change with patches and conflict with the "data-driven from SDE" principle. The spec should name the **source file and field** (`typedogma` attr X, `dungeons.fsdbinary` key Y). Wiki numbers should only be used as test oracles, and marked as such.
3. **Version drift.** The target is V24.01 build 3619879, but the plan includes Catalyst (Nov 2025) and the "2025 Drifter Crisis" from the wiki. Check that these types and dungeons actually exist in the client's `types.fsdbinary`/`dungeons.fsdbinary` before writing a spec for them.
4. **Inferred vs. verified.** The `behaviors/` modules are *client-shipped* bytecode. Calling them "CCP's server AI engine" is an inference. Label it that way, and separate which parts are shared-code definitions and which parts the server must invent.
5. **The living economy (§3.9.3) is a big design with no constraints.** It needs:
   - a CPU budget (1 Hz actors across 8,000+ systems need sleeping/LOD),
   - determinism and seeding,
   - inflation and sink controls,
   - config defaults.

   It also needs its own unit and property tests, not just E2E-16.
6. **The E2E strategy is too coarse.** Each of the 18 journeys touches dozens of subsystems, so none can pass until nearly everything exists. "Every PR runs all 18 in both modes" contradicts the phasing. Use per-domain contract tests plus a few smoke journeys, gated by milestone.
7. **The §5.2 real-client hook isn't really non-invasive.** Supplying your own `thinclients/clientsetup.py` still means injecting code into the client package, so "without patching" is misleading. Also add a short legal/ToS note: the plan builds on proprietary `code.ccp` bytecode and staticdata, and `docs/index/` contains derived data.
8. **Reproducibility.** The sources (`code.ccp`, `evemu_Crucible`) currently live in another conversation's scratch directory. The indexer should take source paths from config, and PLAN should explain how to obtain them.
9. **Document hygiene.** The file is 164 KB with lines over 5,000 characters, a marketing tone ("Yes!", "100% Authentic", many "!"), and several sections that duplicate each other (§3.5, §3.7 and §3.11 overlap; kill rights and asset safety are specified twice). Split it into `docs/spec/<domain>.md` and keep `PLAN.md` as a short execution plan with status checkboxes.

---

## Recommended next steps (in order)
1. Regenerate §3.12 from the catalogs with a script, and add a CI lint for unmapped or unknown names. Normalize the catalog first: merge Crucible class names into V24 names and drop the `.pyj` pseudo-domains.
2. Fix the P1 factual table above. Add a `source:` tag (SDE field, client file:line, or wiki article) to every numeric claim.
3. Restore the dropped indexer deliverables as open TODOs, or record that they were descoped.
4. Replace Phases 2–5 with milestones M1…Mn that each have explicit exit criteria. Make M1 the real-client undock/warp/dock slice.
5. Install the Rust toolchain and re-verify the §2 "Completed & Verified" claims.
