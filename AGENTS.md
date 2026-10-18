# EvoMu agent rules

## Goal and definition of done
- **Goal:** 100% feature parity with the live EVE Online server for the V24.01 client. This covers the classic machoNet surface, **all Quasar/public-gateway backend services**, and **ESI with SSO**.
- **Scope is the pinned parity ledger:** `.agents/skills/evomu-adversarial-review/references/parity/ledger.json`, plus the rows the gap auditor appends in `auditor_rows.json`. It has thousands of rows:
  - every client RPC method, notification, destiny action, Quasar proto, ESI operation and client static-data file;
  - all dogma effects;
  - curated game-mechanic rows (`feat:*`), plan tasks and real-client journeys.
- **Done means `/usr/bin/python3 .agents/skills/evomu-adversarial-review/scripts/final_gate.py` exits 0 on the current tree.** On that same tree it requires:
  - 100% of ledger rows closed;
  - a `PASS` claims review and a `CLEAN` gap audit;
  - workspace line coverage ≥ 90%;
  - mutation score ≥ 85%;
  - every real-client journey passing on the real V24 client, with clean logs.
- **The user reviews only for completeness, and only after the final gate passes.** Work as an unbounded loop:
  - Don't stop, ask for interim review, or ask the user to test.
  - Don't report the project as done or "playable" before the gate passes.
  - If you hit a true external blocker (credentials, hardware, a legal question), record it in `docs/BLOCKERS.md` and keep working on everything that isn't blocked.

## Work plan
- The canonical work plan is **`.agents/skills/evomu-adversarial-review/references/action_plan.md`**. It's **read-only** for you, like everything under `.agents/`, including the ledger.
  - Don't rewrite, renumber, merge, split or drop task IDs or ledger rows.
  - Don't replace the plan with your own.
  - Descoping needs explicit approval from the user.
- **Phase order:** 0 → 1 → T → 2 → 3 → 4 → 5 → 5M → 6 → 7 → 8 → Q → ESI → Z.
  - ESI-1 (SSO/launcher tokens) and Q-1/Q-2 (protobuf codegen, gateway transport) may start as soon as Phase 2 lands, because the real client needs them to log in.
  - Don't start a phase until the previous phase's exit gate is met and the review is non-FAIL.
  - Phase 5M (mining) depends on `D-1`/`D-2`, `SP-1`/`SP-2`, `DG-1`/`DG-4`/`DG-5` and `P-5`. Don't build any feature on hardcoded vertical slices.
- **Track progress in `docs/ACTION_PLAN_STATUS.md`:** one row per canonical task ID, giving status (`NOT STARTED` / `PARTIAL` / `DONE`), the ledger rows closed, the commit and the proving tests. Keep it honest.
- **Tag every test that closes a ledger row** with `// parity: <row id>[, …]` in its comment block (no wildcards).
  - Data-driven static-data tests may attest IDs through `EVOMU_PARITY_ATTEST_DIR` (task T-1).
  - Rows you believe aren't server-facing go in `docs/parity/triage.json` with concrete client-code evidence. The gap auditor audits them.
- Put task IDs (e.g. `S-1`, `MN-04`, `Q-5`) in commit messages and in the claims file you give the reviewer.
- A task is `DONE` only when every "Done when" criterion is met by real, general logic (SDE-driven, no hardcoded IDs), and its tagged tests execute and pass.

## Independent review gate (mandatory)
- **When to run it.** Activate the `evomu-adversarial-review` skill (`.agents/skills/evomu-adversarial-review/SKILL.md`) and follow it before any of these:
  - claiming any work is complete, fixed, implemented, verified or passing;
  - `git commit` or `git push`;
  - editing status claims in `PLAN.md`, `README.md` or `docs/`;
  - ending a turn in which repo files changed.
- **The reviewer's verdict is authoritative.**
  - On **FAIL**, don't commit, push, or describe the work as done.
  - Always record the verdict, the report path and whether tests actually executed.
- **Gap audits.** The orchestrator automatically chains a recall-focused **gap audit** every few reviews (or run `run_review.py --mode gap-audit`). Rows it appends are in scope immediately, and rows it flags as fake DONE count as open until a later audit clears them.
- **The Stop hook enforces two things:** an unreviewed tree must be reviewed, and a reviewed tree must keep making progress until `final_gate.py` passes.
- **Don't touch the gate.** Don't modify `.agents/`, `.evomu-reviews/` or the review hook, and don't disable the hook. Only the user may change the review gate.

## Honesty and verification
- "Verified" or "passing" requires real test execution output. `cargo check` and self-written scripts don't count.
- Unhandled RPCs must return `UnimplementedRPC`. Don't add constant, echo-only or wrong-semantics handlers to raise coverage numbers. Don't embed test fixtures in production code.
- Never bypass macOS Santa or other permission systems.
- evemu_Crucible is LGPL: re-implement its logic, never copy it.
