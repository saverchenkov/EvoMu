# EvoMu agent rules

## Work plan
- The canonical work plan is **`.agents/skills/evomu-adversarial-review/references/action_plan.md`**. It is **read-only** for you, like everything under `.agents/`.
  - Don't rewrite, renumber, merge, split or drop its task IDs.
  - Don't replace it with your own plan.
  - Descoping or reordering a task needs explicit approval from the user.
- Track progress in **`docs/ACTION_PLAN_STATUS.md`**: one row per canonical task ID, giving status (`NOT STARTED` / `PARTIAL` / `DONE`), the commit, and the test that proves the task's "Done when" criteria. Keep it honest.
- Work in the plan's phase order (Phase 0 → 8). Don't start a later phase until the current phase's exit gate is met and the independent review is non-FAIL.
  - Phase 5M (full mining system, `MN-01`…`MN-18`) depends on `D-1`/`D-2`, `SP-1`/`SP-2`, `DG-1`/`DG-4`/`DG-5` and `P-5`. Don't build it on hardcoded vertical slices.
- Put task IDs (e.g. `S-1`, `MN-04`) in commit messages and in the claims file you give the reviewer.
- A task is `DONE` only when every "Done when" criterion is met by real, general logic (SDE-driven, not hardcoded IDs), and a test that executes and passes backs it up.

## Independent review gate (mandatory)
- Before you claim any work is complete, fixed, implemented, verified or passing; before `git commit` or `git push`; before editing status claims in `PLAN.md`, `README.md` or `docs/`; and before ending a turn in which repo files changed: **activate the `evomu-adversarial-review` skill** (`.agents/skills/evomu-adversarial-review/SKILL.md`) and follow it.
- The independent reviewer's verdict is authoritative.
  - On **FAIL**, do not commit, push, or tell the user the work is done.
  - Always report the verdict, the report path and whether tests actually executed.
- Don't modify `.agents/`, `.evomu-reviews/` or the review hook, and don't disable the hook. Only the user may change the review gate.

## Honesty and verification
- "Verified" or "passing" requires real test execution output. `cargo check` and self-written scripts don't count.
- Unhandled RPCs must return `UnimplementedRPC`. Don't add constant, echo-only or wrong-semantics handlers to raise coverage numbers. Don't embed test fixtures in production code.
