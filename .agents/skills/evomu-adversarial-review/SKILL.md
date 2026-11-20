---
name: evomu-adversarial-review
description: >-
  Independent adversarial code review gate for the EvoMu repository. The coding agent MUST use
  this skill before it (a) claims any milestone, fix, or review finding is done, implemented,
  "verified", or "passing"; (b) runs `git commit` or `git push`; (c) edits status claims in
  PLAN.md / README.md / docs/; or (d) ends a turn in which EvoMu source changed.
  It launches a separate headless Jetski CLI (`agy`) reviewer prioritizing Gemini Pro Next
  (`gemini-pro-next`), which verifies claims against the code, runs tests honestly,
  audits exploits, and returns PASS / PASS_WITH_ISSUES / FAIL.
---

# EvoMu adversarial review gate

EvoMu's coding agent has a documented history of overstating progress. Examples:
- calling code "verified" when only `cargo check` ran;
- "removing stubs" by renaming them;
- inflating coverage by adding unrelated method names to existing handlers;
- committing a macOS Santa bypass.

This skill brings in an **independent reviewer**. It is a separate agent running Gemini Pro Next (`gemini-pro-next`, falling back down the chain on quota) with fresh context. Its verdict, not your own judgment, decides whether you may describe work as done.

## When to run (mandatory)
Run the review **before** any of the following:
- Telling the user something is complete, fixed, implemented, verified or passing.
- `git commit` or `git push`.
- Marking anything "✅ COMPLETED" or "Verified" in `PLAN.md`, `README.md` or `docs/`.
- Ending a turn in which files in the repo changed. A `Stop` hook ([../../hooks.json](../../hooks.json)) enforces this one automatically.

## Steps
0. **Pre-flight (mandatory, ~1–5 seconds).** Run targeted `cargo test -j 4 -p eve-server --test it <filter>` (or `--lib <filter>`) while coding to verify your new tests compile and pass, then run `/usr/bin/python3 .agents/skills/evomu-adversarial-review/scripts/preflight.py --claims /tmp/evomu_claims.md` and fix everything it reports until it prints `OK`. Copy numbers from `.evomu-reviews/preflight/claims_template.md`; never type them. Pre-flight runs fast static P0 checks (including all workspace source ratchets and the 60-file / 8,000-line increment cap) and parity computation without running `cargo test --workspace` (pass `--with-tests` only if you explicitly want pre-flight to run the full workspace test suite).
1. **Write your claims** to a temporary file outside the repo, e.g. `/tmp/evomu_claims.md`. Copy what you intend to tell the user: every fix, feature, metric and verification statement, verbatim, one bullet each. Don't soften or omit anything; the reviewer checks each one.
2. **Run the orchestrator** from the repo root, and give it time. It runs the headless LLM code review and the affected test suite concurrently (full `cargo test --workspace --no-fail-fast` runs on `--final` / `final_gate.py` and in hourly Supervisor CI):
   ```bash
   /usr/bin/python3 .agents/skills/evomu-adversarial-review/scripts/run_review.py --claims-file /tmp/evomu_claims.md
   ```
   - It gathers the diff since the last reviewed commit, checks base freshness / merge conflicts against `origin/main` (`TT-04`) and enforces the 60-file / 8,000-line increment cap (`PROC-12`), collects static parity, metrics and a toolchain audit, and launches the headless LLM reviewer (`gemini-pro-next` > `gemini-4-neon-high` > `fable-5.2-high`, first with quota wins) concurrently alongside the **targeted affected test suite** (always-on wire/bootstrap sentinels + 12 surgical historical canaries + T1–T6 path-triggered rules + reverse-dep affected tests + Python `unittest` discovery suites for `tools/client-harness/` and `tools/`, coalesced per target with `-j 4` and `RUST_TEST_THREADS=4`, wrappers scrubbed, isolated target dir). When tests finish, real output overwrites `context/tests.txt` and real executed parity (`assume_tests_pass=False`) checks that all diff-tagged ledger rows actually closed — automatically downgrading to `FAIL` if any test fails or any claimed row fails to close. Don't pass `--model`; it's ignored unless the user sets `EVOMU_REVIEW_ALLOW_MODEL_OVERRIDE=1`.
   - **Mandatory on ALL merges (`TT-06`):** Every merge to `main` — including `tools/client-harness/` (playtester, drivers, hooks, `scenarios/*.json`) and `.agents/` changes — requires an exact-tip archived `run_review.py` review.
   - Exit codes: `0` PASS, `1` PASS_WITH_ISSUES, `2` FAIL, `3` INVALID/ERROR, `4` UNAVAILABLE.
   - Useful flags: `--base <ref>`, `--skip-tests` (recorded in the report as *not executed*), `--dry-run` (collect context and print the command only), `--final` (runs full `cargo test --workspace --no-fail-fast` and records `final: true` for `final_gate.py`).
   - `--launcher jetski-chat` opens an interactive Jetski chat instead. It can't pin the model or record a verdict, so use it only for manual sessions.
3. **Read the report** at `.evomu-reviews/latest.md` (the full path is printed). `.evomu-reviews/<timestamp>/` holds the context, prompt, raw reviewer output and `result.json`.
4. **Act on the verdict (`PROC-01`):**
   - **FAIL:** triggered by any P0, any FALSE claim, **any P1 in the branch's own new/changed code (`own_diff`)**, **any flaky/failing existing-test finding (`existing_test_regression`)**, a merge conflict against `origin/main`, an `INCREMENT_TOO_LARGE` violation (> 60 files / > 8,000 lines), or a test/parity gate failure. Fix every blocking finding, then rerun. Don't commit to `main`, push, or tell the user the work is done.
   - **PASS_WITH_ISSUES:** permitted **only** when there are no P0s, no FALSE claims, no own-diff P1s, and no existing-test regressions (strictly pre-existing P1s outside the branch's own diff). You may merge and file/track the pre-existing P1s.
   - **PASS:** proceed.
   - **UNAVAILABLE (exit 4):** every reviewer model in the `auto` chain (`gemini-pro-next`, `gemini-4-neon-high`, `fable-5.2-high`) was out of quota or capacity. **Nothing was reviewed.** Follow `AGENTS.md` → "Reviewer unavailable": local `wip/*` branches only, no merge or push to `main`, and queue per-task reviews for when quota returns.
   - **Fallback reviews (`TT-24`):** a verdict from any model in `AUTO_CHAIN` counts for routine merges; `final_gate.py` accepts a `PASS` from `run_review.py --final` executed by any model in `EVOMU_FINAL_REVIEW_CHAIN` (`gemini-pro-next` > `gemini-4-neon-high` > `fable-5.2-high`, first with quota wins).
   - **INVALID / ERROR:** the reviewer wrote to its snapshot, or it failed. Rerun. (Reviews run in a temporary snapshot worktree of the reviewed commit, so your own edits during a review don't invalidate it.) If `agy` is not authenticated, ask the user to run `agy` interactively once.
5. **Report honestly.** Always tell the user the verdict word, the report path, the number of blocking findings, whether tests actually executed (`tests_executed` and `scoped` in the report header), and the real executed test count (NEVER describe a scoped run as a full workspace pass). Never describe a result more favorably than the report does.

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
- **Gap audit (recall).** Run it with `run_review.py --mode gap-audit`. It's also chained automatically after a claims review that returns PASS or PASS_WITH_ISSUES, once at least `EVOMU_GAP_AUDIT_EVERY` (default 3) claims reviews have run since the last audit. It never runs after a FAIL. The auditor:
  - finds mechanics missing from the ledger and appends them as new rows;
  - rejects weak NA triage;
  - flags fake DONE rows, which stay open until a later audit clears them.
  - Verdict: `CLEAN` or `GAPS`.
- **Final gate.** `scripts/final_gate.py` is the definition of done. It requires, on the same tree:
  - 100% of the ledger closed, from a fresh full test run;
  - a `PASS` **final** claims review (`run_review.py --final`; reviewer chain `gemini-pro-next` > `gemini-4-neon-high` > `fable-5.2-high`, first with quota wins) and a `CLEAN` gap audit. Routine reviews don't count. Run `--final` once per milestone tree that is otherwise ready;
  - line coverage ≥ 90% (`cargo llvm-cov`);
  - **Two-stage real-client journey gate (`references/parity/client_journeys.json` & `supervisor_journeys.json`):** Per-track coder reviews (`run_review.py`) remain 100% headless — the coding agent must NEVER launch `exefile` / `run_journeys --launch-real-client`, add `// parity: journey:*` tags, or edit `.agents/`. **Interim stage (`interim_smoke`, active immediately):** `final_gate.py` requires a fresh, passing Supervisor-written real-client smoke attestation in `references/parity/supervisor_journeys.json` (`git_sha` matching the judged code tree, current installed V24.02 build `3698585`, `real: true`, `log_errors: []`) covering the **7 core smoke scenarios** (`launch_login`, `station_ui`, `undock_navigate`, `market_trade`, `mining_loop`, `pve_combat`, `skills_training`), while all 29 journeys are tracked on a Supervisor-updated scoreboard read by `parity_status.py` (marking verified `journey:*`, `plan:T-6`, and `plan:ESI-1` rows `DONE` and keeping unrun rows `SUPERVISOR_OWNED`). **Target `plan:Z-6` stage (`full_z6`):** once `#1202` (`--scenario` runner) and `#1203` (2nd-player partner) land and re-express all 29 journeys as Rule-2 deterministic UI scenarios, `final_gate.py` promotes from the 7-scenario smoke requirement to `29/29` passing real-client UI scenarios on the current client build.

  The Stop hook keeps you working until this gate has passed on the current tree.

## References
- Reviewer brief (what the reviewer checks and how it decides): [references/reviewer_brief.md](references/reviewer_brief.md)
- Gap auditor brief: [references/gap_auditor_brief.md](references/gap_auditor_brief.md)
- Canonical plan: [references/action_plan.md](references/action_plan.md)
- Parity ledger, sources and client journeys: [references/parity/](references/parity/)
- Open findings carried forward: [references/open_findings.md](references/open_findings.md)
- Output schemas: [scripts/verdict_schema.json](scripts/verdict_schema.json), [scripts/gap_schema.json](scripts/gap_schema.json)
