# EvoMu Coding Agent Handoff (2026-10-21)

## Quick Start for Next Agent

**Clone and checkout:**
```bash
git clone https://github.com/saverchenkov/EvoMu.git
cd EvoMu
git checkout main  # Currently at 395321d
```

**Verify baseline:**
```bash
# Run preflight to confirm metrics
python .agents/skills/evomu-adversarial-review/scripts/preflight.py

# Expected output:
# Parity: 3723 / 6110 rows closed (60.93%)
# Tests: 523 passed, 46 failed (environmental blockers)
# Early gate: CLEAN (0 P0 violations)
```

## Mission (From /goal)

**100% feature parity with EVE Online V24.01 server, validated by real client.**

**Done when:**
1. **6,110/6,110 ledger rows closed** (100% parity)
2. **29/29 real-client journeys passing** (launch → login → character creation → fitting → undocking → combat → market → mining → missions)
3. **final_gate.py exits 0** (≥90% coverage, ≥85% mutation score, PASS claims review, CLEAN gap audit)

**Current status:**
- Parity: 60.93% (3,723/6,110) - **2,387 rows remain open**
- Journeys: 0/29 passing
- Final gate: Not run

## Current State (main@395321d)

**Baseline metrics:**
- Ledger: 3,723 closed, 2,387 open, 2,206 NA
- Plan rows: 38/109 DONE
- Tests: 523 passing, 46 failing (environmental)
- Categories: RPC 28.6%, ESI 96.4%, feat 57.3%

**Last session completed:**
- Plan tagging: DG-1, DG-3, DG-4 (Dogma) → DONE
- Plan tagging: E-1 through E-5 (Economy) → tagged
- Net: +3 rows closed (60.88% → 60.93%)

**Platform:**
- Windows 10, B-4 blocker resolved (Python Unicode fixed)
- All commits pushed to origin/main
- No active worktrees (direct main commits)

## Critical Files to Read First

**Start here (in order):**
1. **`AGENTS.md`** - All rules including mandatory review gate, worktree workflow, supervisor coordination
2. **`.agents/skills/evomu-adversarial-review/references/action_plan.md`** - READ ONLY canonical work plan (14 phases, task dependencies)
3. **`docs/ACTION_PLAN_STATUS.md`** - Generated status (run preflight.py to regenerate)
4. **`HANDOFF.md`** - Comprehensive project context (created by supervisor, commit 6de0ff6)
5. **`docs/BLOCKERS.md`** - Environmental constraints (B-1, B-2, B-3 still open)

**Reference materials:**
- `.agents/skills/evomu-adversarial-review/references/parity/ledger.json` - All 6,110 ledger rows
- `.agents/skills/evomu-adversarial-review/references/parity/sources/` - V24 client RPC catalog
- `docs/parity/task_notes.json` - Agent assessment per task (edit this, not ACTION_PLAN_STATUS.md)

## Recommended Next Tracks (Priority Order)

### Track 1: High-Leverage Plan Tagging (Low-Risk, Fast ROI)

**Why:** Closes ledger rows by linking existing tests to plan rows. No new code, minimal risk.

**Phase 6 (Character Progression):**
- CH-1 (Skill queue): `test_action_plan_ch1_to_ch5_character_skills_and_gm` exists, needs `// parity: plan:CH-1` tag
- CH-2 (skillMgr2): Tests exist, currently MISSING plan tag
- CH-4 (Character info): Tests exist, verify complete plan tag coverage

**Phase 8 (Quasar Services):**
- Q-1 through Q-7: Comprehensive Quasar framework tests exist
- Focus on the 127 open `quasar:*` rows (1,328 already triaged as not client-facing by supervisor)

**Phase 2/3 (Navigation & Combat):**
- N-1, N-2, N-3 already DONE - verify complete plan tag coverage
- C-1 through C-8: Tests exist, verify plan tags present

**How to do plan tagging:**
1. Read the plan task in `action_plan.md`
2. Find tests that exercise the task's "Done when" criteria
3. Add `// parity: plan:<TASK-ID>` to test comment blocks
4. Run preflight, verify the task shows DONE in ACTION_PLAN_STATUS.md
5. Commit with message format: `feat: Tag plan row <TASK-ID> (<task title>)`

### Track 2: PARTIAL→DONE Feature Implementation (Medium-Risk)

**Why:** Many tasks have tests and partial logic. Finish them to close more rows.

**DG-2 (Skill bonuses):**
- Tests exist: `test_phase_4_dg2_fit_validation_and_rigs`
- Status: Claimed DONE but ledger shows BLOCKED_BY_FEATURES
- Action: Implement remaining 0/6 features

**DG-5 (Targeting):**
- Tests exist: `test_dg5_targeting_state_machine_sensor_strength_and_ewar`
- Status: 12/16 features done, 4 remaining
- Commit: baf8c8b already tagged plan:DG-5

**MN-04 to MN-18 (Mining System):**
- MN-04 was blocked by DG-1/DG-4/DG-5, now DG-1 and DG-4 are DONE
- MN-04 (Mining cycle): 1/3 features, tests exist
- Continue through MN-05 (Holds), MN-06 (Crystals), etc.

**Phase 7 Economy (E-1 through E-6):**
- All have tests tagged (this session)
- All show BLOCKED_BY_FEATURES with X/Y features incomplete
- Implement remaining features per plan acceptance criteria

### Track 3: Real-Client Journeys (High-Risk, User Decisions Required)

**Why:** 0/29 journeys passing. Required for mission completion.

**Blockers:**
- **B-1:** V24.01 client .fsdbinary extraction (macOS only, requires user's macOS machine)
- **B-2:** V24.01 client with Placebo crypto (requires user decision per BLOCKERS.md section 3)
- **B-3:** Compressed SDE archive out of date (missing celestials, requires macOS regeneration)

**Action:** Ask user for direction on resolving B-1, B-2, B-3 before starting journey work.

### Track 4: Gap Auditor Flagged Rows

**Why:** 122 rows flagged as potentially fake by gap auditor. Must be cleared before final gate.

**Action:** Run gap audit, review flagged rows, confirm or fix.

## Mandatory Process (AGENTS.md Rules)

### Review Gate (CRITICAL - Process Enforcement)

**Before ANY git commit or push to main:**
1. Run preflight:
   ```bash
   python .agents/skills/evomu-adversarial-review/scripts/preflight.py --claims /tmp/evomu_claims.md
   ```
   - Must print "OK" at end
   - Copy numbers from generated claims template
   - Fix any lint errors

2. Run review:
   ```bash
   python .agents/skills/evomu-adversarial-review/scripts/run_review.py \
     --mode claims \
     --claims /tmp/evomu_claims.md
   ```
   - On FAIL: Don't commit. Fix issues, re-run preflight and review.
   - On PASS_WITH_ISSUES: Commit immediately, fix P1s in next increment.
   - On PASS: Commit and proceed.

3. Commit only after non-FAIL verdict.

**Exception:** Local `wip/<task-id>` branches (never pushed) don't need review, but final merge to main does.

### Worktree Workflow for Feature Development

**Rule:** Never let two agents edit the same working tree.

**For parallel tracks, use git worktrees:**
```bash
# Create worktree for a feature track
git worktree add ../EvoMu-track-name -b track/track-name

# Work in that worktree
cd ../EvoMu-track-name
# ... make changes, run preflight, run review ...
git commit -m "feat: ..."
git push origin track/track-name

# Merge to main (in main worktree, after review)
cd ../EvoMu
git merge track/track-name
git push origin main
```

**Current state:** Working directly on main (no worktree) per previous session. For new feature work spanning multiple commits, create a worktree.

### Supervisor Coordination

**Who:** Supervisor agent runs in Antigravity conversation `3d6533b5-943c-48df-8b5c-4ccec1132b65`

**What it does:**
- Reviews strategy and progress (hourly + ad hoc)
- Sends steering messages starting with `[From the EvoMu supervisor` or `[Hourly supervisor check`
- Maintains `.agents/` directory and review gate infrastructure

**How to interact:**
- Treat supervisor messages as user direction (user appointed it)
- Reply via `send_message` to `3d6533b5-943c-48df-8b5c-4ccec1132b65` if you disagree with evidence
- Fold its points into your plan before starting new feature work

**Architecture reference:** See `docs/SUPERVISOR_CODER_ARCHITECTURE.md`

### Honesty and Verification Rules

**Never type a number from memory.** Every count, size, row total, line reference in claims must be copied from tool output in the same session.

**Only claim a ledger row closed when:**
- Its tagged test exercises every clause of the row's `acceptance` text
- The test uses RPC/notification names the V24 client actually uses (see `references/parity/sources/`)

**Data you derive or synthesize must be labelled as such.** Never present invented data as imported from client.

**Unhandled RPCs must return `UnimplementedRPC`.** Don't add constant/echo handlers to raise coverage.

## Testing and Validation

**Run tests:**
```bash
cargo test --workspace --no-fail-fast
```

**Expected baseline:**
- 523 passed
- 46 failed (environmental blockers: B-1, B-2, B-3)

**Test tagging format:**
```rust
/// Test dogma modifier engine with Rifter and overdrives.
///
/// Acceptance: Dogma modifierInfo evaluator over all 6 domains...
///
/// parity: plan:DG-1, rpc:dogmaIM.GetAttributeModifiers, feat:dogma.modifiers
#[tokio::test]
async fn test_plan_dg1_dogma_modifier_engine_rifter_overdrives() {
    // test code
}
```

**Validation tools:**
- `preflight.py` - Fast mechanical checks (< 2 min)
- `run_review.py` - Adversarial claims review (~10 min)
- `final_gate.py` - Full gate check (run when 100% complete)

## Known Issues and Environmental Blockers

**B-1: V24.01 Client .fsdbinary Extraction**
- Requires macOS machine with V24.01 client installed
- `tools/sde-import` extracts static data to `data/evomu_sde.sqlite`
- Windows agent cannot run this (macOS-only dylibs)

**B-2: Real-Client Handshake Encryption**
- V24.01 client uses CryptoAPI (CCP's RSA public key)
- EvoMu implements Placebo crypto pack
- Workaround: Copy client build, add common.ini with `cryptoPack = Placebo`
- User decision required: Whether to automate this copy

**B-3: Compressed SDE Archive Out of Date**
- Tracked `data/evomu_sde.sqlite.xz` predates celestials import
- 1 ESI test fails on fresh Windows checkout (Star 40000001 not found)
- Requires macOS agent to regenerate archive from current SDE

**B-4: Python Unicode Encoding** ✅ RESOLVED
- Fixed in commit 201d08a
- Added `safe_print()` wrapper for Windows console

## Common Commands

**Get current parity:**
```bash
python .agents/skills/evomu-adversarial-review/scripts/preflight.py 2>&1 | grep "Parity:"
```

**Run early gate only (fast sanity check):**
```bash
python .agents/skills/evomu-adversarial-review/scripts/preflight.py --early-only
```

**List open plan rows:**
```bash
grep "MISSING\|BLOCKED" docs/ACTION_PLAN_STATUS.md | head -20
```

**Check RPC names against client catalog:**
```bash
# Preflight does this automatically, but you can run manually:
grep -rn "rpc::" crates/eve-server/src/ | grep -v "UnimplementedRPC"
```

**Update task notes (your assessment):**
```bash
# Edit docs/parity/task_notes.json:
# {"DG-1": {"assessment": "DONE", "note": "Modifier engine complete, 217 expression effects remain"}}
# Then run preflight to regenerate ACTION_PLAN_STATUS.md
```

## What Not to Do (Red Lines)

**Never:**
- Commit or push without a non-FAIL review (except local `wip/*` branches)
- Modify `.agents/` directory or the review hook
- Disable the review hook or bypass it with `--no-verify`
- Commit client-derived data (`.fsdbinary`, raw SQLite, uncompressed data)
- Bypass macOS Santa or other permission systems
- Type numbers from memory in claims (copy from tool output)
- Claim rows closed without real, general logic (no hardcoded IDs)
- Add constant RPC handlers just to raise coverage
- Edit `ACTION_PLAN_STATUS.md` by hand (edit `task_notes.json` instead)
- Rewrite, renumber, merge, split or drop task IDs from the plan
- Copy code from evemu_Crucible (LGPL - re-implement logic only)

**Ask user before:**
- Descoping any ledger rows or plan tasks
- Changing the canonical action plan order or dependencies
- Resolving B-1, B-2, B-3 blockers (user decisions required)

## Session History Summary

**Recent commits (last 5):**
- `395321d` - Coder: Final status - plan tagging complete (60.93% parity)
- `765be6c` - Coder: Implementation agent status update
- `e83944c` - Supervisor status update (quota extension)
- `f22adc9` - feat: Tag Phase 7 economy plan rows (E-1, E-2, E-3, E-4)
- `baf8c8b` - feat: Tag Phase 4 dogma plan rows (DG-1, DG-4, DG-5)

**Work trajectory:**
- Oct 18-19: Phase 1 complete, RPC tagging sweep (+177 RPCs)
- Oct 20: B-4 blocker resolved (Python Unicode)
- Oct 21: Plan tagging Phase 4 + Phase 7 (+3 plan rows closed)

**Parity progress:**
- Start: 59.38% (commit 11d1dd0)
- After RPC tagging: 60.90% (commit 201d08a)
- Current: 60.93% (commit 395321d)
- Target: 100% (6,110/6,110 rows)

## Getting Help

**Documentation:**
- `AGENTS.md` - All rules and process
- `HANDOFF.md` - Comprehensive project context
- `docs/SUPERVISOR_CODER_ARCHITECTURE.md` - Supervisor/coder division of responsibilities
- `.agents/skills/evomu-adversarial-review/references/action_plan.md` - Canonical work plan

**If stuck:**
1. Check `docs/BLOCKERS.md` for known environmental issues
2. Review supervisor's `HANDOFF.md` (commit 6de0ff6)
3. Ask user for direction (don't guess on descoping or blocker resolution)

## Quick Win: First Hour Suggested Work

**Goal:** Close 5-10 plan rows through tagging (low-risk, high ROI)

**Steps:**
1. Read `AGENTS.md` and `action_plan.md` (30 min)
2. Run `preflight.py` to confirm 60.93% baseline (2 min)
3. Pick Phase 6 Character tasks (CH-1, CH-2, CH-4)
4. Find their tests in `crates/eve-server/tests/it/`
5. Add `// parity: plan:CH-1` etc. to test comment blocks
6. Run `preflight.py --early-only` (1 sec) to verify
7. Run `cargo test` to ensure no regressions (5 min)
8. Run full `preflight.py` (2 min) - copy numbers
9. Run `run_review.py` with claims (10 min)
10. On non-FAIL verdict: commit and push
11. Update `.agents/implementation_agent_status.md`

**Expected outcome:** 3-5 plan rows closed in Phase 6, parity ~61.0-61.1%

---

## Start Command

```bash
# After cloning and reading AGENTS.md + action_plan.md:
python .agents/skills/evomu-adversarial-review/scripts/preflight.py

# Then choose track and start work!
```

Ready to continue. Mission: 100% parity, 29/29 journeys, final_gate.py exits 0.

Current: 60.93% complete. **2,387 rows to go.**

---

**Handoff prepared by:** Implementation agent session 2026-10-21  
**Repository state:** main@395321d, origin synchronized  
**Next agent:** Start here, follow recommended tracks, obey review gate
