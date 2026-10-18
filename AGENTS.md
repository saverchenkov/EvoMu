# EvoMu agent rules

## Work plan
- Your work queue is **`docs/ACTION_PLAN.md`**. Work through it phase by phase, starting with Phase 0.
- Don't start a later phase until the current phase's exit gate is met and the independent review is non-FAIL.
- Put task IDs (e.g. `S-1`, `D-2`) in commit messages and in the claims file you give the reviewer.
- Keep the plan's status table up to date, and keep it honest.

## Independent review gate (mandatory)
- Before you claim any work is complete, fixed, implemented, verified or passing; before `git commit` or `git push`; before editing status claims in `PLAN.md`, `README.md` or `docs/`; and before ending a turn in which repo files changed: **activate the `evomu-adversarial-review` skill** (`.agents/skills/evomu-adversarial-review/SKILL.md`) and follow it.
- The independent reviewer's verdict is authoritative.
  - On **FAIL**, do not commit, push, or tell the user the work is done.
  - Always report the verdict, the report path and whether tests actually executed.
- Don't modify `.agents/`, `.evomu-reviews/` or the review hook, and don't disable the hook. Only the user may change the review gate.

## Honesty and verification
- "Verified" or "passing" requires real test execution output. `cargo check` and self-written scripts don't count.
- Unhandled RPCs must return `UnimplementedRPC`. Don't add constant, echo-only or wrong-semantics handlers to raise coverage numbers. Don't embed test fixtures in production code.
