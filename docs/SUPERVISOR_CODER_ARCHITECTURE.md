# EvoMu Supervisor / Coder Multi-Agent Architecture & Operating Manual

## 1. Executive Summary & Core Mission

**EvoMu** is an asynchronous, high-concurrency clean-room server emulator for **EVE Online V24.01 (Client Build 3619879)**, written in idiomatic, memory-safe Rust with `#![forbid(unsafe_code)]`.

The emulator delivers complete end-to-end server functionality spanning:
1. **The Classic machoNet Surface:** Authenticated RPC dispatch across 216 services, dynamic moniker bindings (`beyonce`, `dogmaIM`, `invbroker`, etc.), session change pipelines, 1 Hz spatial physics and combat loops (`Ballpark`), and dogmatic attribute calculation (`DogmaVM`).
2. **The Quasar Microservice Architecture:** 135 protobuf-driven backend packages communicating over HTTP/2, gRPC, and the client Public Gateway.
3. **The ESI OpenAPI Surface:** 197 REST endpoints backed by OAuth2/PKCE SSO tokens.

### Definition of Done: The Final Gate
The project is complete when `/usr/bin/python3 .agents/skills/evomu-adversarial-review/scripts/final_gate.py` exits with code `0`. Mechanically, this requires:
- **100% Honest Closure** across all 6,110 rows in `.agents/skills/evomu-adversarial-review/references/parity/ledger.json` (either `DONE` backed by real, passing tests or `NA` backed by client-bytecode evidence in `supervisor_triage.json`).
- An authoritative **`PASS` Review Verdict** from an independent LLM judge (`run_review.py`) and a **`CLEAN` Gap Audit**.
- **Workspace Line Coverage $\ge 90\%$** and **Mutation Score $\ge 85\%$** (`cargo mutants`).
- All 29 real-client journey benchmarks (`plan:T-6`) passing against the real V24.01 client via `tools/client-harness` and `evomu-visual-check`.

---

## 2. Multi-Agent Architecture & Operating Model

To achieve honest parity without human micromanagement, development operates under an autonomous **Dual-Agent Governance Topology**:

```mermaid
flowchart TD
    subgraph Supervisor["Supervisor Agent (Conversation: 3d6533b5-943c-48df-8b5c-4ccec1132b65)"]
        direction TB
        SupLoop["Autonomous Steering Loop\n(30-min checks, 2-hr reflection)"]
        Gates["Mechanical Gate Suite\n(preflight.py, run_review.py, review_lib.py)"]
        Ledger["Scope Triage & Ledger\n(ledger.json, supervisor_triage.json)"]
    end

    subgraph Coder["Coding Agent (Incoming / Active Instance)"]
        direction TB
        Orch["Orchestrator / Implementation\n(crates/**/*.rs, tests/it/*.rs)"]
        WTIso["Worktree Isolation\n(EvoMu-p7-destiny, EvoMu-s-refactor-tests, etc.)"]
        Claims["Claims Preparation\n(.evomu-reviews/claims_*.md)"]
    end

    subgraph Reviewer["Adversarial Review Engine (run_review.py)"]
        direction TB
        RevStack["Fallback Stack Rank:\n1. Claude Opus 5.5 Medium\n2. Gemini 4 Argon High\n3. Gemini Flash Next"]
        Audit["Independent Static Inspection\n(AST, state mutation, RPC audit)"]
    end

    subgraph GitTree["Repository State (origin/main)"]
        MainHead["origin/main (Reviewed & Gate-Checked Commits)"]
    end

    SupLoop -- "Periodic Steering (send_message)" --> Orch
    Gates -- "Enforces preflight & review" --> Coder
    Ledger -- "Triaged Scope (1,593 NA rows)" --> Coder
    Orch -- "Submits code + claims" --> Reviewer
    Reviewer -- "PASS / PASS_WITH_ISSUES / FAIL" --> Orch
    Orch -- "Fast-forwards reviewed tree" --> MainHead
    Gates -- "Validates code_tree_of hash" --> MainHead
```

### Division of Responsibilities

| Dimension | Supervisor Agent (`3d6533b5-...`) | Coding Agent (Incoming Agent) |
|---|---|---|
| **Primary Mandate** | Quality assurance, gate enforcement, scope governance, and high-level steering. | Feature implementation, test authoring, claims generation, adversarial review, and merges. |
| **Owned Paths** | `.agents/**`, gate scripts (`preflight.py`, `review_lib.py`, `run_review.py`, `final_gate.py`), `references/parity/*`, `AGENTS.md`. | `crates/**/*.rs`, `crates/eve-server/tests/it/*.rs`, `client-bridge/**`, `tools/**`, feature branches (`wip/*`). |
| **Strict Invariants (NEVER)** | **Never** modifies `crates/**/*.rs` or test files.<br>**Never** commits directly to the coder's worktrees.<br>**Never** kills coder processes (`cargo`, `run_review.py`).<br>**Never** weakens gates or relaxes honesty standards.<br>**Never** force-pushes `main`. | **Never** modifies `.agents/**` or alters gate scripts.<br>**Never** bypasses gates (`--no-verify`, removing hooks).<br>**Never** pushes or merges unreviewed trees to `main`.<br>**Never** fakes tests (no hollow match arms, no shape-only assertions, no direct DB test fixtures).<br>**Never** commits client-derived data (`data/*.sqlite`). |
| **Safe-Edit Protocol** | Uses temporary `GIT_INDEX_FILE` commits to `refs/heads/main` to push gate/doc updates without altering active working trees. | Works exclusively on dedicated `wip/*` branches in isolated worktrees. Merges to `main` only after non-FAIL review. |
| **Communication** | Sends actionable, evidence-based steering messages every 30 minutes (`[Supervisor check HH:MM ...]`). | Reads steering, folds priorities into plan, and replies via `send_message` with technical evidence if blocked or disputing a finding. |

---

## 3. Git Worktree Isolation & Topology

To prevent context contamination, lock contention, and accidental invalidation of code tree hashes, each functional area operates in an **isolated git worktree**:

### Active Worktree Directory Layout
```
/Users/sergeya/ai/
  ├── EvoMu/                    # Root checkout (main branch / orchestrator)
  ├── EvoMu-p7-destiny/         # Space, Destiny, Sov, Structures, FW (wip/space-structures-sov-fw)
  ├── EvoMu-s-refactor-tests/   # Combat, CrimeWatch, Fighters, NPCs, Drones
  ├── EvoMu-c5-rng/             # Character, Skills, Clones, Features (wip/features-destiny-plan-progression)
  ├── EvoMu-na-sde-destiny/     # RPC closure & SDE regressions (wip/rpc-closure-2-sde-regressions)
  ├── EvoMu-t2-sde-r2/          # Notifications & RPC closure (wip/notifications-rpc-closure-1)
  └── EvoMu-t6-harness/         # Live V24.01 real-client journey harness (wip/t6-client-harness)
```

### Invariants for Worktrees
1. **Single-Agent Invariant:** Exactly one agent writes to a given worktree at any time (concurrent writes corrupt git index state). Reviews run in a temporary snapshot worktree of the reviewed commit, overlaid with `main`'s current `.agents/`, so the coder keeps working while a review runs.
2. **Local SDE:** `data/evomu_sde.sqlite` is client-derived and never committed (raw or compressed). Build it with `tools/sde-import` from your own V24.01 client install, or copy it from another machine you control. Worktrees share the canonical SDE path:
   ```bash
   export EVOMU_SDE_PATH=/Users/sergeya/ai/EvoMu/data/evomu_sde.sqlite
   ```
3. **Consolidated Test Binary Invariant:** All integration tests across all worktrees **must** reside under `crates/eve-server/tests/it/*.rs` and be registered in `crates/eve-server/tests/it/main.rs`. Creating standalone test binaries directly under `crates/eve-server/tests/` violates the `NEW_STANDALONE_TEST_BINARY` gate and adds 5–10s of linker overhead per binary.

---

## 4. The Three Defense Rings (Gating System)

The codebase is protected by three consecutive rings of automated verification before any commit can reach `origin/main`:

```
[ Incoming Code ]
       |
       v
+-----------------------------------------------------------+
| Ring 1: Early Static Sanity Gate (< 1s execution)         |
|   • NOT_IN_CLIENT_RPC: rejects invented RPC names         |
|   • MERGE_CONFLICT_MARKER: rejects conflict markers       |
|   • S10_DOC_COUNT_MISMATCH: verifies doc count sync       |
|   • UNTESTED_RPC_TAG: requires RPC call in test file      |
|   • TOO_MANY_ROWS_PER_TEST: max 15 feature rows per test  |
|   • SHAPE_ONLY_TEST & THIN_ASSERTIONS: asserts values     |
|   • CONSTANT_RPC_HANDLER: blocks hollow match arms (<= 5) |
|   • NEW_STANDALONE_TEST_BINARY: enforces tests/it/main.rs |
+-----------------------------------------------------------+
       |
       v
+-----------------------------------------------------------+
| Ring 2: Full Preflight Verification (~10-15s execution)   |
|   • Runs: cargo test --workspace --no-fail-fast           |
|   • Verifies exact parity ledger closure numbers          |
|   • Generates authoritative claims_template.md            |
+-----------------------------------------------------------+
       |
       v
+-----------------------------------------------------------+
| Ring 3: Adversarial Review Engine (run_review.py)         |
|   • Independent LLM judge analyzes diff + AST + claims    |
|   • Stack: Claude Opus 5.5 -> Gemini 4 Argon -> Flash Next|
|   • Verdicts: PASS / PASS_WITH_ISSUES / FAIL / UNAVAILABLE|
+-----------------------------------------------------------+
       |
       v
[ origin/main ]
```

### Code Tree Fingerprinting (`review_lib.code_tree_of`)
To ensure that supervisor doc/gate updates or untracked local data files do not invalidate existing reviews, `review_lib.py` hashes the git tree with the following paths strictly excluded:
- `.agents/**` (supervisor-owned review skills and scripts)
- `AGENTS.md` (supervisor-owned rules)
- `docs/SUPERVISOR_CODER_ARCHITECTURE.md` (in-repo architecture manual)
- `docs/ACTION_PLAN_STATUS.md` (machine-generated status document)
- `docs/BLOCKERS.md` (supervisor-maintained environment notes)
- `data/quasar_core_golden.json`, `data/quasar_descriptors.sqlite`, `data/evomu_sde.sqlite` (local client data)

When `code_tree_of(tree_A) == code_tree_of(tree_B)`, a valid review of `tree_A` automatically covers `tree_B`.

---

## 5. Review Protocol & Fallback Model Stack

All code reaching `origin/main` must be reviewed by `/usr/bin/python3 .agents/skills/evomu-adversarial-review/scripts/run_review.py`.

### Reviewer Model Hierarchy
1. **Primary Model:** `opus-5.5-medium` / `opus-5.5-high` (Claude Opus 5.5).
2. **First Fallback:** `gemini-launch-candidate-high` (Gemini 4 Argon High) — highly effective at deep semantic analysis, detecting state-leakage, unauthenticated RPCs, and fake test assertions.
3. **Second Fallback:** `gemini-flash-next` (`--effort high`).

### How to Interpret Review Verdicts
- **`PASS`**: Full approval. Merge immediately to `main` and push.
- **`PASS_WITH_ISSUES`**: **Merge immediately!** Do not re-review or spend cycles chasing minor issues. The reviewer identified non-blocking P1 observations; fold them into your next feature increment or fix them as you naturally touch those files.
- **`FAIL`**: Strict block. Read `.evomu-reviews/<timestamp>/report.md`. You must fix all P0 and P1 blocking findings in code, update your claims file, re-run preflight, and re-review.
- **`UNAVAILABLE` (Exit 4)**: Quota or API exhaustion across all models. Follow the Quaint Policy:
  - Commit only to local `wip/*` branches (never push `main`).
  - Maintain the local queue in `docs/parity/review_queue.md`.
  - Resume review once quota recovers.

---

## 6. Parity Tracking & Scope Triage

Parity is tracked row-by-row in `.agents/skills/evomu-adversarial-review/references/parity/ledger.json` across **6,110 discrete rows**:

| Category | Total Rows | Purpose & Scope |
|---|---|---|
| `esi` | 197 | EVE Swagger Interface OpenAPI operations (100% complete). |
| `effect` | 1 | Real SDE sweep over all 3,023 Dogma effects (100% complete). |
| `quasar` | 1,455 | Quasar protobuf services and messages (96.7% closed). |
| `sde` | 213 | Static Data Export tables and resource files (70.9% closed). |
| `destiny` | 52 | Destiny physics states, commands, and ball updates (61.5% closed). |
| `notify` | 1,311 | Asynchronous client notifications (`On*`). |
| `feat` | 1,513 | Granular, user-observable gameplay mechanics. |
| `plan` | 109 | Canonical action plan tasks (`S-*`, `D-*`, `P-*`, `SP-*`, etc.). |
| `rpc` | 1,230 | Classic machoNet client-callable RPC methods. |
| `journey` | 29 | Real-client automated e2e test journeys (`plan:T-6`). |

### Mechanical Scope Triage (`supervisor_triage.json`)
The supervisor maintains an evidence-backed triage ledger containing **1,593 `NA` rows** verified via static bytecode analysis of `code.ccp`:
- **1,328 `quasar:*` rows:** 1,048 internal `eve/*` protos, 64 unreachable `eve_public/*` protos, 181 pure data/identifier/enum packages (zero RPCs), and 35 client telemetry modules.
- **94 `feat:quasar.*` rows:** Internal-only packages.
- **111 `rpc:*` rows:** Client-side Python utilities, rowset attributes (`columns`, `itervalues`), legacy POS (`posMgr.*`), Resource Wars, IGB, Serenity billing (`kiringMgr.*`), devtools QA, and client cache methods.
- **60 `sde:res:*` rows:** Client localization pickles, unreferenced legacy challenge tables, and 46 client-only 3D visual/audio/paperdoll tables.

### Test Attestation Syntax
To close a ledger row, an integration test must exercise the full acceptance criteria and declare the tag in its docstring:
```rust
/// Demonstrates anchoring, fuel cycle, and structure reinforcement transitions.
// parity: feat:structures.upwell-anchoring, feat:structures.upwell-vulnerability
#[tokio::test]
async fn test_upwell_structure_lifecycle() {
    // Real server logic, real RPC invocations, concrete value assertions
}
```

---

## 7. Current Project State

### State on `origin/main` (2026-10-21, after the Windows session)
- `main` was rewritten on 2026-10-21 (force-push authorized by the user) to remove `data/evomu_sde.sqlite.xz` from history. The old `cc2162d` became `8a133f5`, with identical content apart from the archive. **Any other clone must re-clone or `git fetch && git reset --hard origin/main`.**
- The commits from the Windows session (Windows portability fixes, rpc:/plan: test tagging) are accepted by the user as reviewed and are the baseline.
- Supervisor pre-flight on macOS with all local data (`cc2162d` content): **596 tests, 594 passed + 2 failed only because `data/quasar_core_golden.json` was not linked in the scratch worktree; 3,912 / 6,110 rows closed (64.03%)**, 2,206 NA.
- Known quality debt: the bulk tagging commit `1556e2f` added incidental `rpc:` tags (RPCs a test calls only as setup). Prune these with evidence through normal reviewed branches; new tags must follow the 15-rows-per-test rule.

### Pending remote branches
1. `origin/wip/notifications-rpc-closure-1` (`727622f`): Batch G notifications and Part 1 RPC closure suites (next to rebase, review and merge).
2. `origin/wip/rpc-closure-2-sde-regressions` (`9f73e4c`): Batch B2 RPC closure and regressions.
3. `origin/wip/t6-client-harness` (`b0eeb8b`): character selection and client login dispatch for `plan:T-6`.

---

## 8. Operational Playbook for Incoming Coding Agent

When resuming work on any machine with GitHub access, follow this step-by-step workflow:

### Step 1: Clone & Environment Sanity Check
```bash
git clone https://github.com/saverchenkov/EvoMu.git
cd EvoMu
# provide data/evomu_sde.sqlite (tools/sde-import from your own client, or copy it from another machine you control)
/usr/bin/python3 .agents/skills/evomu-adversarial-review/scripts/preflight.py --early-only
```

### Step 2: Select Your Development Track
Pick one of the active pushed branches or start a new feature track:
- **Option A (RPC Closure & Regressions):**
  ```bash
  git checkout -b wip/rpc-closure-2 origin/wip/rpc-closure-2-sde-regressions
  git rebase main
  ```
- **Option C (Real-Client T-6 Harness):**
  ```bash
  git checkout -b wip/t6-harness origin/wip/t6-client-harness
  git rebase main
  ```

### Step 3: Test, Preflight, and Adversarial Review
1. Run workspace integration tests:
   ```bash
   cargo test --test it -- --nocapture
   ```
2. Run preflight to verify parity counts and lint claims:
   ```bash
   /usr/bin/python3 .agents/skills/evomu-adversarial-review/scripts/preflight.py --claims .evomu-reviews/claims_<track>.md
   ```
3. Run the independent adversarial review:
   ```bash
   /usr/bin/python3 .agents/skills/evomu-adversarial-review/scripts/run_review.py \
     --claims-file .evomu-reviews/claims_<track>.md \
     --base origin/main
   ```

### Step 4: Merge to `main` and Push
Once the review returns `PASS` or `PASS_WITH_ISSUES`:
```bash
git checkout main
git merge --ff-only wip/<track>
/usr/bin/python3 .agents/skills/evomu-adversarial-review/scripts/preflight.py --claims .evomu-reviews/claims_<track>.md
git push origin main
```
No git hooks are installed; the review gate is enforced by process (`AGENTS.md`), the Stop hook and the supervisor.

---

## 9. Platforms (macOS and Windows)

- EvoMu supports both macOS and Windows. The project can move between them at any time with no extra discussion; the rules are identical on both.
- All gates (pre-flight, `run_review.py`, `final_gate.py`) run **locally on the machine in use**. There are no git hooks.
- Client-derived data never goes into git: `data/evomu_sde.sqlite`, `data/quasar_descriptors.sqlite`, `data/quasar_core_golden.json`, raw client files. A machine without some of them has test failures whose panics name the missing file. Those are acceptable in a review only when listed in the claims file. Rows closed only by tests that couldn't run aren't claimed. `final_gate.py` runs on a machine with all local data. Details: `AGENTS.md` ("Supported platforms") and `docs/BLOCKERS.md` §4.
- Python scripts write files as UTF-8 and print through `safe_print`, so they work on Windows consoles. Use forward slashes in paths that go into SQLite URIs and TOML.
- **Switching machines:** push all work (`main` reviewed, `wip/*` branches as needed). On the new machine: clone or `git reset --hard origin/main`, provide the local data files, run `preflight.py --early-only`, and resume. Update the supervisor conversation ID in `AGENTS.md` if the supervisor changes.
- Handoff state lives in this document (§7). Don't add separate root-level handoff files.
