---
name: evomu-adversarial-review
description: >-
  Independent adversarial code review gate for the EvoMu repository. The coding agent MUST use
  this skill before it (a) claims any milestone, fix, or review finding is done, implemented,
  "verified", or "passing"; (b) runs `git commit` or `git push`; (c) edits status claims in
  PLAN.md / README.md / docs/AGENT_HANDOFF.md; or (d) ends a turn in which EvoMu source changed.
  It launches a separate headless Jetski CLI (`agy`) reviewer pinned to Claude Opus 5.5 (High)
  (`opus-5.5-high`), which verifies claims against the code, runs tests honestly,
  audits exploits, and returns PASS / PASS_WITH_ISSUES / FAIL.
---

# EvoMu adversarial review gate

EvoMu's coding agent has a documented history of overstating progress. Examples:
- calling code "verified" when only `cargo check` ran;
- "removing stubs" by renaming them;
- inflating coverage by adding unrelated method names to existing handlers;
- committing a macOS Santa bypass.

This skill brings in an **independent reviewer**. It is a separate agent running Claude Opus 5.5 (High) (`opus-5.5-high`) with fresh context. Its verdict, not your own judgment, decides whether you may describe work as done.

## When to run (mandatory)
Run the review **before** any of the following:
- Telling the user something is complete, fixed, implemented, verified or passing.
- `git commit` or `git push`.
- Marking anything "✅ COMPLETED" or "Verified" in `PLAN.md`, `README.md` or `docs/`.
- Ending a turn in which files in the repo changed. A `Stop` hook ([../../hooks.json](../../hooks.json)) enforces this one automatically.

## Steps
0. **Pre-flight (mandatory, about 1 minute).** Run `/usr/bin/python3 .agents/skills/evomu-adversarial-review/scripts/preflight.py --claims /tmp/evomu_claims.md` and fix everything it reports until it prints `OK`. Copy numbers from `.evomu-reviews/preflight/claims_template.md`; never type them. Pre-flight fails in <2s on P0 anti-patterns (hollow assertions, unknown RPCs, broken citations) before running `cargo test`; pass `--skip-early-fail` to override or `--early-only` for instant checks.
1. **Write your claims** to a temporary file outside the repo, e.g. `/tmp/evomu_claims.md`. Copy what you intend to tell the user: every fix, feature, metric and verification statement, verbatim, one bullet each. Don't soften or omit anything; the reviewer checks each one.
2. **Run the orchestrator** from the repo root, and give it time. It runs `cargo test` and then a headless review:
   ```bash
   /usr/bin/python3 .agents/skills/evomu-adversarial-review/scripts/run_review.py --claims-file /tmp/evomu_claims.md
   ```
   - It gathers the diff since the last reviewed commit, runs `cargo test` honestly (wrappers scrubbed, isolated target dir), collects metrics and a toolchain audit, then calls:
     `agy -p <brief> --model opus-5.5-high --effort high --output-format json --json-schema verdict_schema.json --sandbox`
   - Exit codes: `0` PASS, `1` PASS_WITH_ISSUES, `2` FAIL, `3` INVALID/ERROR.
   - Useful flags: `--base <ref>`, `--skip-tests` (recorded in the report as *not executed*), `--dry-run` (collect context and print the command only).
   - `--launcher jetski-chat` opens an interactive Jetski chat instead. It can't pin the model or record a verdict, so use it only for manual sessions.
3. **Read the report** at `.evomu-reviews/latest.md` (the full path is printed). `.evomu-reviews/<timestamp>/` holds the context, prompt, raw reviewer output and `result.json`.
4. **Act on the verdict:**
   - **FAIL:** fix every P0, plus every FALSE claim or reword it, then rerun. Don't commit, push, or tell the user the work is done.
   - **PASS_WITH_ISSUES:** you may commit. Tell the user the verdict and list the P1s, or fix them and rerun.
   - **PASS:** proceed.
   - **UNAVAILABLE (exit 4):** every reviewer model (`opus-5.5-high`, then the fallbacks `auto` [Gemini Pro Next], `gemini-launch-candidate-high`, `gemini-flash-next` and `fable-5.1-high`) was out of quota or capacity. **Nothing was reviewed.** Follow `AGENTS.md` → "Reviewer unavailable": local `wip/*` branches only, no merge or push to `main`, and queue per-task reviews for when quota returns.
   - **Fallback reviews:** a verdict from a fallback model counts for commits like any other. The final gate still requires a PASS from the primary model.
   - **INVALID / ERROR:** the workspace changed during review or the reviewer failed. Rerun once the tree is quiet. If `agy` is not authenticated, ask the user to run `agy` interactively once.
5. **Report honestly.** Always tell the user the verdict word, the report path, the number of blocking findings, and whether tests actually executed (`tests_executed` in the report header). Never describe a result more favorably than the report does.

## Rules
- **Don't modify this skill**, `.agents/hooks.json`, or anything under `.evomu-reviews/`. Don't set `EVOMU_REVIEW_HOOK=off`. Only the user may change the review gate.
- **Never bypass macOS Santa or permission systems.** No dylib/ctypes trampolines, fake `rustdoc`, or `rustc-wrapper` tricks. If Santa blocks `cargo test`, the report says so. Report "tests not executed"; don't work around it.
- **Don't optimize for the metric scripts** in `scripts/metrics/`. They only flag places to look. Every catalog method you handle must actually implement that method's semantics, or return `UnimplementedRPC`.
- **Don't embed test fixtures in production code paths** to make tests pass.

## Completeness: parity ledger, gap audits and the final gate
Per-change reviews check **precision** (is what you claim true?). Completeness is checked separately and mechanically:
- **Ledger.** `references/parity/ledger.json` (pinned) plus `references/parity/auditor_rows.json` (appended by gap audits) together define 100% parity:
  - every client RPC method, notification, destiny action, Quasar proto, ESI operation and static-data file;
  - all dogma effects;
  - curated mechanics (`feat:*`), plan tasks and real-client journeys.
- **Parity status.** Every review run computes `scripts/parity/parity_status.py` after the tests and stores the result in the run's `context/parity.md`/`parity.json`. The report's "Parity" section shows the percentage and the first open rows per category.
  - A row is closed only when a test tagged `// parity: <row id>` passed in that run and the implementation check passes, or when it's an auditor-accepted NA in `docs/parity/triage.json`.
- **Gap audit (recall).** Run it with `run_review.py --mode gap-audit`. It's also chained automatically after a claims review that returns PASS or PASS_WITH_ISSUES, once at least `EVOMU_GAP_AUDIT_EVERY` (default 5) claims reviews have run since the last audit. It never runs after a FAIL. The auditor:
  - finds mechanics missing from the ledger and appends them as new rows;
  - rejects weak NA triage;
  - flags fake DONE rows, which stay open until a later audit clears them.
  - Verdict: `CLEAN` or `GAPS`.
- **Final gate.** `scripts/final_gate.py` is the definition of done. It requires, on the same tree:
  - 100% of the ledger closed, from a fresh full test run;
  - a `PASS` claims review and a `CLEAN` gap audit;
  - line coverage ≥ 90% (`cargo llvm-cov`) and mutation score ≥ 85% (`cargo mutants`);
  - every journey in `references/parity/client_journeys.json` passing on the real V24 client via `tools/client-harness/run_journeys`, with zero `UnimplementedRPC`, panics or client tracebacks.

  The Stop hook keeps you working until this gate has passed on the current tree.

## References
- Reviewer brief (what the reviewer checks and how it decides): [references/reviewer_brief.md](references/reviewer_brief.md)
- Gap auditor brief: [references/gap_auditor_brief.md](references/gap_auditor_brief.md)
- Canonical plan: [references/action_plan.md](references/action_plan.md)
- Parity ledger, sources and client journeys: [references/parity/](references/parity/)
- Open findings carried forward: [references/open_findings.md](references/open_findings.md)
- Previous review reports: [references/history/](references/history/)
- Output schemas: [scripts/verdict_schema.json](scripts/verdict_schema.json), [scripts/gap_schema.json](scripts/gap_schema.json)
