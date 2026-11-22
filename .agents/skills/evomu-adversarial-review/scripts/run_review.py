#!/usr/bin/python3
"""EvoMu independent adversarial review orchestrator.

Collects evidence (git diff, honest test run, metrics, toolchain audit), then launches a
headless Jetski/Antigravity CLI reviewer (`agy -p ... --model auto`)
with a structured-output schema, validates that the reviewer did not touch the workspace,
and records the verdict in <repo>/.evomu-reviews/ (gitignored).

Exit codes: 0 PASS, 1 PASS_WITH_ISSUES, 2 FAIL, 3 INVALID / ERROR, 4 UNAVAILABLE (every reviewer model out of
quota/capacity; nothing was reviewed; see AGENTS.md "Reviewer unavailable").
"""
import argparse
import datetime
import glob
import json
import math
import os
import re
import shutil
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import review_lib as rl  # noqa: E402

SKILL_DIR = os.path.dirname(HERE)
BRIEF = os.path.join(SKILL_DIR, "references", "reviewer_brief.md")
OPEN_FINDINGS = os.path.join(SKILL_DIR, "references", "open_findings.md")
HISTORY_DIR = os.path.join(SKILL_DIR, "references", "history")
SCHEMA = os.path.join(HERE, "verdict_schema.json")
GAP_BRIEF = os.path.join(SKILL_DIR, "references", "gap_auditor_brief.md")
GAP_SCHEMA = os.path.join(HERE, "gap_schema.json")
PARITY_DIR = os.path.join(SKILL_DIR, "references", "parity")
PARITY_STATUS = os.path.join(HERE, "parity", "parity_status.py")
WIKI = os.path.expanduser("~/ai/eve-wiki")
CRUCIBLE = os.path.expanduser("~/ai/evemu_Crucible")
WRAPPER_ENV = ["RUSTC_WRAPPER", "CARGO_BUILD_RUSTC_WRAPPER", "RUSTDOC", "CARGO_BUILD_RUSTDOC",
               "RUSTC", "CARGO_BUILD_RUSTC", "RUSTC_WORKSPACE_WRAPPER", "CARGO_TARGET_DIR"]
EXIT = {"PASS": 0, "PASS_WITH_ISSUES": 1, "FAIL": 2}
GAP_EXIT = {"CLEAN": 0, "GAPS": 2}
UNAVAILABLE_EXIT = 4  # every reviewer model was out of quota/capacity: NOTHING was reviewed
QUOTA_RE = re.compile(r"RESOURCE_EXHAUSTED|[Qq]uota|rate.?limit|\b429\b|No capacity|UNAVAILABLE \(code 503\)|"
                      r"\b503\b|overloaded|insufficient (?:credits|balance)|usage limit", re.I)
FALLBACK_MODELS = [m.strip() for m in os.environ.get(
    "EVOMU_REVIEW_FALLBACK_MODELS", "gemini-pro-next,gemini-4-neon-high,fable-5.2-high").split(",") if m.strip()]
# Default reviewer mode (user decision 2026-11-18): `--model auto` tries each model in this order and uses the first
# one that has quota/capacity. Callers must not pin a weaker model; an explicit --model / EVOMU_REVIEW_MODEL other
# than `auto` is ignored unless EVOMU_REVIEW_ALLOW_MODEL_OVERRIDE=1 (set only on the user's instruction).
AUTO_MODE = "auto"
AUTO_CHAIN = [m.strip() for m in os.environ.get(
    "EVOMU_REVIEW_AUTO_CHAIN", "gemini-pro-next,gemini-4-neon-high,fable-5.2-high").split(",") if m.strip()]
# Final-acceptance reviewer chain (user decision 2026-11-18): `--final` claims reviews try Gemini Pro Next first,
# then fall back on quota/capacity errors only. final_gate.py only accepts a PASS recorded
# with "final": true. Use it sparingly: one --final review per milestone tree that is otherwise ready for final_gate.py.
FINAL_CHAIN = [m.strip() for m in os.environ.get(
    "EVOMU_FINAL_REVIEW_CHAIN", "gemini-pro-next,gemini-4-neon-high,fable-5.2-high").split(",") if m.strip()]
# Readable EvoMu model names -> agy model ids. agy's own id for Gemini Pro Next is literally "auto"; we never pass
# EvoMu's AUTO_MODE to agy, only the resolved chain entries.
AGY_MODEL_IDS = {"gemini-pro-next": "auto", "gemini-neon-high": "gemini-4-neon-high"}
# Models whose agy selection has no effort levels (passing --effort makes agy refuse to start).
NO_EFFORT_MODELS = {m.strip() for m in os.environ.get(
    "EVOMU_REVIEW_NO_EFFORT_MODELS", "gemini-pro-next,gemini-flash-next,gemini-pro-coordinator").split(",") if m.strip()}

# Map model names from agy/review format to Claude CLI format
CLAUDE_CLI_MODELS = {
    "opus-5.5-medium": "claude-opus-5-5",
    "opus-5.5-high": "claude-opus-5-5",
    "opus-5.5-low": "claude-opus-5-5",
    "sonnet-5.5-medium": "claude-sonnet-5-5",
    "sonnet-5.5-high": "claude-sonnet-5-5",
}

# Map effort levels to Claude CLI flags
CLAUDE_CLI_EFFORT = {
    "low": "low",
    "medium": "medium",
    "high": "high",
    "xhigh": "xhigh",
    "max": "max",
}
MODEL_UNUSABLE_RE = re.compile(r"invalid model selection|not recognized as a known model|is not supported for model", re.I)
CONTINUE_PROMPT = (
    "Continue the EvoMu review exactly where you left off. Do not restart and do not summarize progress. "
    "Keep using file-reading/search tools silently (no prose between tool calls) until every step of the "
    "reviewer brief is done, then return ONLY the final JSON object required by the schema."
)


def sh(cmd, cwd, env=None, timeout=None):
    try:
        p = subprocess.run(cmd, cwd=cwd, env=env, capture_output=True, text=True,
                           timeout=timeout)
        return p.returncode, p.stdout + (("\n[stderr]\n" + p.stderr) if p.stderr else "")
    except subprocess.TimeoutExpired as e:
        return 124, "TIMEOUT after %ss\n%s" % (timeout, (e.stdout or ""))
    except FileNotFoundError as e:
        return 127, "NOT FOUND: %s" % e


def detect_cli():
    """Detect which CLI is available: claude, agy, or none. Returns ('claude'|'agy'|None, version_string)."""
    # Check for claude CLI first
    try:
        result = subprocess.run(["claude", "--version"], capture_output=True, text=True, timeout=5)
        if result.returncode == 0:
            return "claude", result.stdout.strip()
    except (FileNotFoundError, subprocess.TimeoutExpired):
        pass

    # Fall back to agy
    try:
        result = subprocess.run(["agy", "--version"], capture_output=True, text=True, timeout=5)
        if result.returncode == 0:
            return "agy", result.stdout.strip()
    except (FileNotFoundError, subprocess.TimeoutExpired):
        pass

    return None, ""


def get_launcher_env(launcher="agy"):
    """Get environment variables for the launcher.
    NOTE: ANTHROPIC_API_KEY (or ANTHROPIC_AUTH_TOKEN) and ANTHROPIC_BASE_URL must be set
    in the environment by the user if using a custom API endpoint (claude launcher only)."""
    env = dict(os.environ)
    # Verify required environment variables are set (agy authenticates on its own)
    if launcher == "claude" and not env.get("ANTHROPIC_API_KEY") and not env.get("ANTHROPIC_AUTH_TOKEN"):
        print("[review] WARNING: ANTHROPIC_API_KEY or ANTHROPIC_AUTH_TOKEN not set in environment")
        print("[review] Set via: export ANTHROPIC_API_KEY='your-key'")
        print("[review] For custom endpoints: export ANTHROPIC_BASE_URL='your-endpoint'")
    return env


def build_reviewer_command(prompt, schema, model, effort, timeout, root, launcher="claude"):
    """Build a reviewer command for either claude or agy CLI.
    Returns (command_list, actual_launcher)."""
    # Auto-detect if launcher was set to 'claude' but it's not available
    if launcher == "claude":
        cli, _ = detect_cli()
        if cli != "claude":
            print(f"[review] claude CLI not available, falling back to {cli or 'agy'}")
            launcher = cli or "agy"

    if launcher == "claude":
        # Claude CLI command structure
        model_name = CLAUDE_CLI_MODELS.get(model, model)
        effort_level = CLAUDE_CLI_EFFORT.get(effort, effort)
        cmd = ["claude", "-p", prompt, "--model", model_name, "--effort", effort_level,
               "--output", "json", "--response-format", schema,
               "--sandbox", "--add-dir", root]
    else:
        # agy command structure (original)
        cmd = ["agy", "-p", prompt, "--model", model, "--effort", effort,
               "--output-format", "json", "--json-schema", schema,
               "--print-timeout", timeout, "--sandbox", "--add-dir", root]

    for d in (WIKI, CRUCIBLE):
        if os.path.isdir(d):
            cmd += ["--add-dir", d]

    # Add skill dir if reviewing a separate worktree
    if not os.path.abspath(SKILL_DIR).startswith(os.path.abspath(root) + os.sep):
        cmd += ["--add-dir", SKILL_DIR]

    return cmd, launcher


def write(path, text):
    with open(path, "w", encoding="utf-8") as f:
        f.write(text)


def extract_verdict(env, valid=None):
    """Return the structured verdict dict from an agy JSON envelope, salvaging it from the raw
    response text when the stream ended in ERROR or structured_output is missing."""
    so = env.get("structured_output") or {}
    if so.get("verdict") in (valid or EXIT):
        return so
    resp = env.get("response") or ""
    candidates = re.findall(r"```(?:json)?\s*(\{.*?\})\s*```", resp, re.S)
    m = re.search(r"\{.*\}", resp, re.S)
    if m:
        candidates.append(m.group(0))
    for c in candidates:
        try:
            obj = json.loads(c, strict=False)  # tolerate raw newlines/tabs inside strings
        except Exception:  # noqa: BLE001
            continue
        if isinstance(obj, dict) and obj.get("verdict") in (valid or EXIT):
            return obj
    return {}


def pick_base(root, state, explicit):
    if explicit:
        return explicit
    head_sha = subprocess.run(["git", "rev-parse", "HEAD"], cwd=root, capture_output=True, text=True).stdout.strip()
    mb_main = None
    for ref in ("main", "origin/main"):
        proc = subprocess.run(["git", "merge-base", ref, "HEAD"], cwd=root, capture_output=True, text=True)
        if proc.returncode == 0 and proc.stdout.strip():
            cand = proc.stdout.strip()
            if cand != head_sha:
                mb_main = cand
                break
    rev_base = None
    for r in reversed(state.get("reviews", [])):
        head = r.get("head")
        if head and head != head_sha and subprocess.run(["git", "merge-base", "--is-ancestor", head, "HEAD"],
                                                        cwd=root).returncode == 0:
            rev_base = head
            break
    if mb_main and rev_base:
        # Pick whichever ancestor is closer to HEAD
        if subprocess.run(["git", "merge-base", "--is-ancestor", mb_main, rev_base], cwd=root).returncode == 0:
            return rev_base
        return mb_main
    if mb_main:
        return mb_main
    if rev_base:
        return rev_base
    prev = subprocess.run(["git", "rev-parse", "--verify", "-q", "HEAD~1"], cwd=root, capture_output=True, text=True)
    if prev.returncode == 0 and prev.stdout.strip():
        return prev.stdout.strip()
    return subprocess.run(["git", "rev-list", "--max-parents=0", "HEAD"], cwd=root,
                          capture_output=True, text=True).stdout.split()[0]


def env_audit(root):
    lines = []
    for p in [os.path.join(root, ".cargo", "config.toml"), os.path.join(root, ".cargo", "config"),
              os.path.expanduser("~/.cargo/config.toml"), os.path.expanduser("~/.cargo/config")]:
        if os.path.exists(p):
            lines.append("FOUND %s:\n%s" % (p, open(p).read()))
    for k in WRAPPER_ENV:
        if os.environ.get(k):
            lines.append("ENV %s=%s" % (k, os.environ[k]))
    rc, out = sh(["git", "grep", "-n", "-I", "-E",
                  r"ctypes\.CDLL|-Wl,-dylib|test result: ok\. 0 passed|rustc-wrapper|santa",
                  "--", ".", ":(exclude).agents"], root)
    lines.append("SUSPICIOUS PATTERN GREP (rc=%d):\n%s" % (rc, out.strip() or "(none)"))
    return "\n\n".join(lines) or "(no wrappers found)"


def _inspect_cargo_pid_meta(pids):
    """Best-effort inspection of `cargo` PIDs to exclude exempt Playtester / hourly CI builds and
    child `cargo` processes already accounted for by a held review flock slot.
    Returns `{pid: {"ppid": int|None, "exempt": bool}}`."""
    if not pids:
        return {}
    meta = {pid: {"ppid": None, "exempt": False} for pid in pids}
    pid_arg = ",".join(str(p) for p in sorted(pids))
    try:
        ps_proc = subprocess.run(
            ["ps", "-ww", "-o", "pid=,ppid=,args=", "-p", pid_arg],
            capture_output=True,
            text=True,
            timeout=5,
        )
        if ps_proc.returncode == 0 and ps_proc.stdout.strip():
            for ln in ps_proc.stdout.splitlines():
                parts = ln.strip().split(None, 2)
                if len(parts) < 2:
                    continue
                try:
                    pid = int(parts[0])
                    ppid = int(parts[1])
                except ValueError:
                    continue
                if pid not in meta:
                    continue
                args = parts[2] if len(parts) >= 3 else ""
                meta[pid]["ppid"] = ppid
                if "EvoMu-playtester" in args or "hourly_test_suite" in args:
                    meta[pid]["exempt"] = True
    except (OSError, subprocess.SubprocessError):
        pass
    try:
        lsof_proc = subprocess.run(
            ["lsof", "-a", "-d", "cwd", "-Fn", "-p", pid_arg],
            capture_output=True,
            text=True,
            timeout=5,
        )
        if lsof_proc.returncode == 0 and lsof_proc.stdout.strip():
            cur_pid = None
            for ln in lsof_proc.stdout.splitlines():
                if ln.startswith("p"):
                    try:
                        cur_pid = int(ln[1:])
                    except ValueError:
                        cur_pid = None
                elif ln.startswith("n") and cur_pid in meta:
                    cwd_s = ln[1:]
                    if "EvoMu-playtester" in cwd_s or "hourly_test_suite" in cwd_s:
                        meta[cur_pid]["exempt"] = True
    except (OSError, subprocess.SubprocessError):
        pass
    return meta


def _count_active_cargo_processes(exclude_parent_pids=None):
    """Return the count of live non-exempt host `cargo` processes (`pgrep -x cargo`), excluding
    exempt Playtester / hourly-CI builds and children of `exclude_parent_pids` (which already hold
    a review flock slot).

    Returns `0` when `pgrep` cleanly reports no matching processes (`returncode == 1`), or `None`
    when `pgrep` itself fails/errors (never silently fails open to `0`, TT-22)."""
    try:
        proc = subprocess.run(["pgrep", "-x", "cargo"], capture_output=True, text=True, timeout=5)
    except (OSError, subprocess.SubprocessError):
        return None
    if proc.returncode == 1:
        return 0
    if proc.returncode != 0:
        return None
    pids = []
    for ln in proc.stdout.splitlines():
        s = ln.strip()
        if s.isdigit():
            pids.append(int(s))
    if not pids:
        return 0
    excl = set(exclude_parent_pids or ())
    meta = _inspect_cargo_pid_meta(pids)
    count = 0
    for pid in pids:
        info = meta.get(pid) or {}
        if info.get("exempt"):
            continue
        if excl and info.get("ppid") in excl:
            continue
        count += 1
    return count


def _acquire_cargo_test_slot(root):
    """Acquire one of EVOMU_MAX_CARGO_TEST_SLOTS (default 4) kernel flock slots using a coordinator
    mutex so concurrent reviews never TOCTOU-overshoot `max_slots` (TT-22), while also accounting
    for external non-playtester coder `cargo` processes without double-counting held review slots."""
    try:
        import fcntl
    except ImportError:
        return None
    max_slots = max(1, int(os.environ.get("EVOMU_MAX_CARGO_TEST_SLOTS", "4")))
    mdir = rl.main_state_dir(root)
    coord_path = os.path.join(mdir, "cargo_test_slot_coord.lock")
    waited_slot = False
    waited_cargo = False
    warned_pgrep = False
    while True:
        coord_fh = open(coord_path, "a+", encoding="utf-8")
        try:
            os.set_inheritable(coord_fh.fileno(), False)
            fcntl.flock(coord_fh.fileno(), fcntl.LOCK_EX)
            held_slots = 0
            held_pids = set()
            free_slot = None
            for slot in range(max_slots):
                lpath = os.path.join(mdir, "cargo_test_slot_%d.lock" % slot)
                fh = open(lpath, "a+", encoding="utf-8")
                try:
                    os.set_inheritable(fh.fileno(), False)
                    fcntl.flock(fh.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
                except (BlockingIOError, OSError):
                    held_slots += 1
                    try:
                        fh.seek(0)
                        rec = json.loads(fh.read().strip() or "{}")
                        if isinstance(rec.get("pid"), int):
                            held_pids.add(rec["pid"])
                    except Exception:  # noqa: BLE001
                        pass
                    fh.close()
                    continue
                if free_slot is None:
                    free_slot = (slot, fh)
                else:
                    try:
                        fcntl.flock(fh.fileno(), fcntl.LOCK_UN)
                    except OSError:
                        pass
                    fh.close()

            if free_slot is None:
                pass
            else:
                slot_idx, slot_fh = free_slot
                ext_cargo = _count_active_cargo_processes(exclude_parent_pids=held_pids)
                if ext_cargo is None:
                    if not warned_pgrep:
                        print(
                            "[review] WARNING: pgrep unavailable for global cargo check; "
                            "enforcing strict %d-slot kernel flock cap." % max_slots,
                            flush=True,
                        )
                        warned_pgrep = True
                    total_in_flight = held_slots
                else:
                    total_in_flight = held_slots + ext_cargo

                if total_in_flight < max_slots:
                    slot_fh.seek(0)
                    slot_fh.truncate()
                    slot_fh.write(json.dumps({
                        "pid": os.getpid(),
                        "root": root,
                        "slot": slot_idx,
                        "ts": time.time(),
                    }) + "\n")
                    slot_fh.flush()
                    print(
                        "[review] acquired cargo test slot %d/%d (held_slots=%d, external_cargo=%s, cap=%d)"
                        % (slot_idx + 1, max_slots, held_slots, ext_cargo, max_slots),
                        flush=True,
                    )
                    return slot_fh

                try:
                    fcntl.flock(slot_fh.fileno(), fcntl.LOCK_UN)
                except OSError:
                    pass
                slot_fh.close()
                if not waited_cargo:
                    print(
                        "[review] waiting for active cargo slots to drop below %d "
                        "(held_slots=%d, external_cargo=%s)..." % (max_slots, held_slots, ext_cargo),
                        flush=True,
                    )
                    waited_cargo = True
                need_cargo_wait = True
            need_cargo_wait = free_slot is not None
        finally:
            try:
                fcntl.flock(coord_fh.fileno(), fcntl.LOCK_UN)
            except OSError:
                pass
            coord_fh.close()

        if need_cargo_wait:
            time.sleep(5.0)
        else:
            if not waited_slot:
                print(
                    "[review] waiting for a free cargo test slot (max %d concurrent test runs)..." % max_slots,
                    flush=True,
                )
                waited_slot = True
            time.sleep(2.0)


def _release_cargo_test_slot(fh):
    if fh is None:
        return
    try:
        import fcntl
        fcntl.flock(fh.fileno(), fcntl.LOCK_UN)
    except Exception:  # noqa: BLE001
        pass
    try:
        fh.close()
    except Exception:  # noqa: BLE001
        pass


SENTINEL_IT_FILTERS = (
    "rpc_client_contracts",
    "login_space_bootstrap_sequence",
    "client_harness",
)
SENTINEL_LIB_FILTER = "macho_listener"

# Always-on surgical historical canaries (user decision 2026-11-20, mined from 49 historical P0 CI breaks):
# 12 single tests folded into the already-running `eve-server --test it` and `eve-server --lib` binaries
# (zero extra binary links or SDE loads; excludes slow/flaky tests).
SENTINEL_IT_CANARY_TESTS = (
    "features_structure_modules_rpcs::test_b22_overheat_rack_heat_and_damage_propagation",
    "features_daily_challenges::ess_grid_warp_and_module_restrictions::test_b36_ess_grid_warp_and_module_restrictions",
    "notify_services_and_events::criminal_timer_and_fw_capture_notices::parity_notify_capture_batch::test_parity_notify_criminal_timer_and_fw_capture_changed",
    "notify_services_and_events::dungeon_escalation_and_fw_bunker_notices::parity_notify_batch2::test_batch2_facwar_bunker_vulnerable_and_incursion_influence_update",
    "economy_industry_pi_extended::pi_poco_gantry_deploy_reinforce::test_wallet_evermarks_and_item_trader",
    "corporation_membership_and_titles::get_titles_returns_deterministic_10col_rowset_and_live_client_dict_of_util_rows",
    "world_space_exploration::world_features_ext_b::test_corp_member_audit_reinforce_default_and_fleet_handover_formations",
    "combat_npc_crimewatch::test_c3_wrecks_loot_50pct_ownership_killmails_esi_hash_pod_corpse_and_self_destruct",
    "modules_grappler_hull_energizer_and_spool::test_modules_grappler_ehe_mutadaptive_repairer_and_entropic_disintegrator",
)
SENTINEL_LIB_CANARY_TESTS = (
    "router::dispatch::dispatch_unit_tests::dispatch_rpc_bind_edges_wal_rollback_and_sov_respawn_triggered",
    "router::tests::journeys_and_catalog::test_all_18_e2e_journeys_and_catalog_coverage",
    "router::notify_registry_tests::raw_notify_ratchet_holds_and_migrated_files_stay_clean",
)

# Path-triggered surgical rules T1..T6 (user decision 2026-11-20)
T1_CLIENT_CONTRACT_TEST = (
    "client_contract::tests::user_error_key_catalogue_matches_v24_dialogs_static_and_ratchet_holds"
)
T2_DESTINY_IT_TESTS = (
    "space::test_p7_destiny_binary_stream_undock_movement_and_stargate_jump",
    "pve_escalations_expeditions_arena_live::test_live_proving_grounds_validation_queue_and_sphere_hazard",
)
T3_CORP_IT_TESTS = (
    "corp_fleet_social_edge_coverage::corp_medals_bulletins_votes_offices_and_hangars_errors",
)
T3_CORP_LIB_TESTS = (
    "router::dispatch::wal_poison_tests::test_issue_874_ignored_try_with_mut_snapshot_failure_poisons_rpc_without_poisoning_next_rpc",
)
T4_COMBAT_MODULE_IT_TESTS = (
    "features_homefront_operations::homefront_operations_it::test_homefront_operations_live_rpc_mining_salvage_and_nonhub_seeding",
    "combat_weapons_wrecks::test_c1_turret_families_hit_formula_wrecking_shots_and_crystal_degradation",
    "features_freight_and_courier::test_corruption_all_stages_1_to_5_and_drop_removal",
    "features_paragon_missions::test_operations_tutorial_tasks_categories_progress_and_rewards",
)
T4_COMBAT_MODULE_LIB_TESTS = (
    "services::pve_ess_reserve_banks::pve_ess_reserve_banks_tests::wave4_ess_boundary_thresholds_tax_exempt_and_autopayout_edges",
    "services::tactical_modules::superweapon_ops::tactical_modules_superweapon_ops_edges_tests::f02_superweapon_and_lance_preserve_gate_cloak_on_rejections_and_break_on_commit",
)
T5_STRUCTURE_IT_TESTS = (
    "structures_lifecycle_complete::test_damage_cap_and_repair_timer_with_pause_threshold",
    "structures_lifecycle_complete::test_reinforcement_cycle_per_size_and_security_band",
)
T6_PROTO_LIB_TEST = "pb::pb_unit_tests::raw_encode_field_ratchet_enforced_on_eve_server_src"

PATH_TRIGGERED_IT_CANARIES = (
    T2_DESTINY_IT_TESTS + T3_CORP_IT_TESTS + T4_COMBAT_MODULE_IT_TESTS + T5_STRUCTURE_IT_TESTS
)

MAX_AFFECTED_IT_FILTERS = 13
MAX_AFFECTED_LIB_FILTERS = 6
REV_DEP_CACHE_FILENAME = "reverse_dep_index_v3.json"
_REV_DEP_MEM_CACHE = {}

_USE_MOD_RE = re.compile(
    r"\b(?:eve_server|crate|super)::(?:services::|router::|combat::|esi::|system_actor::|sp6_lifecycle::)?([A-Za-z_][A-Za-z0-9_]*)"
)
_ANY_QUAL_SYM_RE = re.compile(r"\b([A-Za-z_][A-Za-z0-9_]*)::([A-Za-z_][A-Za-z0-9_]{4,})\b")
_FN_DEF_RE = re.compile(r"^\s*(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?(?:const\s+)?fn\s+([A-Za-z_][A-Za-z0-9_]{3,})\b")
_PATH_MOD_RE = re.compile(r'#\[path\s*=\s*"([A-Za-z0-9_./]+)\.rs"\s*\]')
_ON_NOTIF_RE = re.compile(r'"(On[A-Z][A-Za-z0-9_]{2,})"|\b(On[A-Z][A-Za-z0-9_]{2,})\b')
_FX_RE = re.compile(r'"(effects\.[A-Za-z0-9_]+)"')
_QUOTED_IDENT_RE = re.compile(r'"([A-Za-z_][A-Za-z0-9_]{2,})"')
_IT_HELPER_RE = re.compile(r"\bcrate::([A-Za-z_][A-Za-z0-9_]*)")
_STRIP_TEST_SUFFIXES = (
    "_deep_extra_unit_tests",
    "_extra_unit_tests",
    "_cov_unit_tests",
    "_gap_unit_tests",
    "_edges2_tests",
    "_edges_b4_tests",
    "_edge_tests",
    "_edges_tests",
    "_extra_tests",
    "_unit_tests",
    "_cov_tests",
    "_tests",
)
_DEP_STOP_WORDS = frozenset({
    "test", "tests", "complete", "live", "suite", "parity", "edge", "coverage", "features", "rpcs",
    "shard", "regressions", "handle", "check", "update", "state", "items", "player", "system",
    "character", "server", "client", "error", "value", "apply", "build", "create", "delete",
    "remove", "resolve", "parse", "fetch", "query", "with", "from", "into", "async", "await",
    "default", "clone", "write", "unwrap", "expect", "assert", "insert", "entry", "filter",
    "active", "ship", "ships", "module", "modules", "solar", "station", "world", "record",
    "activate", "sync", "stop", "start", "push", "pull", "load", "save", "read", "send", "emit",
})


def _ensure_cargo_j4(cmd):
    """Ensure every `cargo test` invocation passes `-j 4` (if not already present) per Workspace Rule 3."""
    out = list(cmd)
    if len(out) >= 2 and out[1] == "test":
        has_jobs = any(
            a in ("-j", "--jobs") or a.startswith("-j") or a.startswith("--jobs=")
            for a in out[2:]
        )
        if not has_jobs:
            out[2:2] = ["-j", "4"]
    return out


def _norm_src_stem(rel_unix):
    parts = rel_unix.replace("\\", "/").split("/")
    stem = os.path.splitext(parts[-1])[0]
    for suf in _STRIP_TEST_SUFFIXES:
        if stem.endswith(suf) and len(stem) > len(suf):
            stem = stem[:-len(suf)]
            break
    if stem == "mod" and len(parts) >= 2:
        stem = parts[-2]
    return stem


def _load_rpc_catalog_names(root):
    services = set()
    methods = set()
    cands = (
        os.path.join(PARITY_DIR, "sources", "rpc_service_catalog.json"),
        os.path.join(root, "docs", "index", "rpc_service_catalog.json"),
    )
    for cp in cands:
        if os.path.isfile(cp):
            try:
                cat = json.load(open(cp, encoding="utf-8"))
                svcs = cat.get("services") if isinstance(cat.get("services"), dict) else cat
                if isinstance(svcs, dict):
                    for svc, info in svcs.items():
                        if isinstance(info, dict):
                            services.add(svc)
                            for m in (info.get("v24_client_candidate_methods") or []) + (
                                info.get("evomu_implemented_methods") or []
                            ):
                                methods.add(m if isinstance(m, str) else m.get("name", ""))
                break
            except (OSError, ValueError):
                pass
    services.discard("")
    methods -= {"", "List", "Add", "Get", "Remove", "Update", "Delete", "Close", "Open", "Init", "Ping"}
    return services, methods


def _load_or_build_reverse_dep_index(root):
    """Build or load cached reverse-import / RPC-service / notification / symbol index from
    `crates/eve-server/src` and `crates/eve-server/tests/it/**/*.rs`."""
    src_dir = os.path.join(root, "crates", "eve-server", "src")
    it_dir = os.path.join(root, "crates", "eve-server", "tests", "it")
    src_files = sorted(glob.glob(os.path.join(src_dir, "**", "*.rs"), recursive=True)) if os.path.isdir(src_dir) else []
    it_files = sorted(glob.glob(os.path.join(it_dir, "**", "*.rs"), recursive=True)) if os.path.isdir(it_dir) else []

    src_max_m = max((os.stat(p).st_mtime_ns for p in src_files), default=0)
    it_max_m = max((os.stat(p).st_mtime_ns for p in it_files), default=0)
    sig = "%d:%d:%d:%d" % (len(src_files), src_max_m, len(it_files), it_max_m)

    mem = _REV_DEP_MEM_CACHE.get(root)
    if mem and mem.get("sig") == sig:
        return mem

    cache_path = None
    try:
        cache_path = os.path.join(rl.state_dir(root), REV_DEP_CACHE_FILENAME)
    except BaseException:  # noqa: BLE001
        cache_path = None

    if cache_path and os.path.isfile(cache_path):
        try:
            with open(cache_path, encoding="utf-8") as fh:
                disk = json.load(fh)
            if isinstance(disk, dict) and disk.get("sig") == sig:
                disk["services"] = set(disk.get("services", ()))
                disk["methods"] = set(disk.get("methods", ()))
                disk["known_src_mods"] = set(disk.get("known_src_mods", ()))
                disk["reverse_src"] = {k: set(v) for k, v in disk.get("reverse_src", {}).items()}
                disk["src_parent"] = {k: set(v) for k, v in disk.get("src_parent", {}).items()}
                disk["lib_notif_mods"] = {k: set(v) for k, v in disk.get("lib_notif_mods", {}).items()}
                disk["it_tokens"] = {k: set(v) for k, v in disk.get("it_tokens", {}).items()}
                _REV_DEP_MEM_CACHE[root] = disk
                return disk
        except (OSError, ValueError, TypeError):
            pass

    services, methods = _load_rpc_catalog_names(root)
    known_src_mods = set()
    reverse_src = {}
    src_parent = {}
    lib_notif_mods = {}

    for p in src_files:
        rel = os.path.relpath(p, src_dir).replace("\\", "/")
        fname = rel.rsplit("/", 1)[-1]
        is_test_f = rel.startswith("tests/") or "/tests/" in rel or fname == "tests.rs" or fname.endswith(("_tests.rs", "_test.rs"))
        stem = _norm_src_stem(rel)
        with open(p, encoding="utf-8", errors="replace") as fh:
            txt = fh.read()
        if is_test_f:
            for m1, m2 in _ON_NOTIF_RE.findall(txt):
                ev = m1 or m2
                if stem not in ("tests", SENTINEL_LIB_FILTER):
                    lib_notif_mods.setdefault(ev, set()).add(stem)
            continue
        known_src_mods.add(stem)
        parts = rel.split("/")
        if len(parts) >= 2 and parts[0] not in ("services", "router", "esi", "combat"):
            src_parent.setdefault(stem, set()).add(parts[0])
            known_src_mods.add(parts[0])
        ci = txt.find("#[cfg(test)]")
        prod = txt if ci < 0 else txt[:ci]
        imps = set(_USE_MOD_RE.findall(prod)) - {stem, "self", "super", "crate"}
        for imp in imps:
            reverse_src.setdefault(imp, set()).add(stem)
        for pm in _PATH_MOD_RE.findall(prod):
            child = os.path.splitext(os.path.basename(pm))[0]
            if not child.endswith(("_tests", "_test")):
                src_parent.setdefault(child, set()).add(stem)

    skip_it = {"main", "mod", "common"} | set(SENTINEL_IT_FILTERS)
    raw_it = {}
    it_reverse_helper = {}
    qual_prefixes = known_src_mods | {"eve_server", "services", "router", "sp6_lifecycle", "system_actor", "combat"}

    for p in it_files:
        rel_it = os.path.relpath(p, it_dir).replace("\\", "/")
        mod_name = os.path.splitext(os.path.basename(p))[0]
        if mod_name == "mod" and "/" in rel_it:
            mod_name = rel_it.split("/")[-2]
        with open(p, encoding="utf-8", errors="replace") as fh:
            txt = fh.read()
        toks = set()
        for m in _USE_MOD_RE.findall(txt):
            if m not in _DEP_STOP_WORDS:
                toks.add("mod:" + m)
        for lhs, rhs in _ANY_QUAL_SYM_RE.findall(txt):
            if lhs in qual_prefixes and rhs not in _DEP_STOP_WORDS and (rhs[0].islower() or rhs.isupper()):
                toks.add("sym:" + rhs)
        for m1, m2 in _ON_NOTIF_RE.findall(txt):
            toks.add("on:" + (m1 or m2))
        for fx in _FX_RE.findall(txt):
            toks.add("fx:" + fx)
        for q in _QUOTED_IDENT_RE.findall(txt):
            if q in services:
                toks.add("svc:" + q)
            elif q in methods:
                toks.add("rpc:" + q)
        for w in mod_name.split("_"):
            if len(w) >= 4 and w not in _DEP_STOP_WORDS:
                toks.add("kw:" + w)
        raw_it.setdefault(mod_name, set()).update(toks)
        for h in _IT_HELPER_RE.findall(txt):
            if h not in ("common", "main", mod_name):
                it_reverse_helper.setdefault(h, set()).add(mod_name)

    it_tokens = {m: set(t) for m, t in raw_it.items() if m not in skip_it}
    for h, users in it_reverse_helper.items():
        htoks = {t for t in raw_it.get(h, set()) if not t.startswith("kw:")}
        for u in users:
            if u in it_tokens:
                it_tokens[u].update(htoks)

    token_df = {}
    for toks in it_tokens.values():
        for t in toks:
            token_df[t] = token_df.get(t, 0) + 1

    n_it = max(1, len(it_tokens))
    avg_len = (sum(len(t) for t in it_tokens.values()) / float(n_it)) if it_tokens else 1.0

    serializable = {
        "sig": sig,
        "services": sorted(services),
        "methods": sorted(methods),
        "known_src_mods": sorted(known_src_mods),
        "reverse_src": {k: sorted(v) for k, v in reverse_src.items()},
        "src_parent": {k: sorted(v) for k, v in src_parent.items()},
        "lib_notif_mods": {k: sorted(v) for k, v in lib_notif_mods.items()},
        "it_tokens": {k: sorted(v) for k, v in it_tokens.items()},
        "token_df": token_df,
        "n_it": n_it,
        "avg_len": avg_len,
    }
    if cache_path:
        try:
            os.makedirs(os.path.dirname(cache_path), exist_ok=True)
            tmp = cache_path + ".tmp.%d" % os.getpid()
            with open(tmp, "w", encoding="utf-8") as fh:
                json.dump(serializable, fh)
            os.replace(tmp, cache_path)
        except OSError:
            pass

    mem_obj = dict(
        serializable,
        services=services,
        methods=methods,
        known_src_mods=known_src_mods,
        reverse_src=reverse_src,
        src_parent=src_parent,
        lib_notif_mods=lib_notif_mods,
        it_tokens=it_tokens,
    )
    _REV_DEP_MEM_CACHE[root] = mem_obj
    return mem_obj


def _enclosing_fns_at_ref(root, ref, rel_path, line_nums):
    raw = ""
    if ref == "HEAD":
        fp = os.path.join(root, rel_path)
        if os.path.isfile(fp):
            try:
                raw = open(fp, encoding="utf-8", errors="replace").read()
            except OSError:
                raw = ""
    if not raw:
        proc = subprocess.run(["git", "show", "%s:%s" % (ref, rel_path)], cwd=root, capture_output=True, text=True)
        raw = proc.stdout if proc.returncode == 0 else ""
    if not raw:
        return set()
    lines = raw.splitlines()
    ci = next((i for i, l in enumerate(lines) if "#[cfg(test)]" in l), len(lines))
    out = set()
    for ln in line_nums:
        if ln > ci:
            continue
        for j in range(min(max(ln - 1, 0), len(lines) - 1), -1, -1):
            m = _FN_DEF_RE.match(lines[j])
            if m:
                fn = m.group(1)
                out.add(fn)
                for suf in ("_sync", "_async", "_inner", "_impl", "_in_snap"):
                    if fn.endswith(suf) and len(fn) > len(suf) + 3:
                        out.add(fn[:-len(suf)])
                break
    return out


def _is_test_only_rs_path(rel):
    rel_u = rel.replace("\\", "/")
    fname = rel_u.rsplit("/", 1)[-1]
    return (
        "/tests/" in rel_u
        or rel_u.startswith("tests/")
        or fname == "tests.rs"
        or fname.endswith(("_unit_tests.rs", "_tests.rs", "_test.rs"))
    )


def _is_central_router_auth_file(rel):
    """Return True for central production router/auth files whose blast radius covers
    bound-object auth, bridge dispatch, Quasar gateway pre-auth, and property routing (#1393)."""
    rel_u = rel.replace("\\", "/")
    parts = rel_u.split("/")
    if len(parts) != 5 or parts[:4] != ["crates", "eve-server", "src", "router"]:
        return False
    fname = parts[4]
    if not fname.endswith(".rs") or _is_test_only_rs_path(rel_u):
        return False
    if fname in ("dispatch.rs", "context.rs", "mod.rs"):
        return True
    return fname == "binding.rs" or fname.startswith("binding_")


def _matches_t2_destiny_space_file(rel):
    rel_u = rel.replace("\\", "/")
    if rel_u.startswith("crates/eve-destiny/"):
        return True
    if not rel_u.startswith("crates/eve-server/src/") or _is_test_only_rs_path(rel_u):
        return False
    fname = rel_u.rsplit("/", 1)[-1]
    if "/system_actor/" in rel_u or fname.startswith("system_actor"):
        return True
    return fname in ("space_warp_jump.rs", "nav_warp_interdiction.rs", "space_destiny.rs")


def _matches_t3_corp_auth_file(rel):
    rel_u = rel.replace("\\", "/")
    if _is_test_only_rs_path(rel_u):
        return False
    if rel_u == "crates/eve-db/src/characters.rs":
        return True
    fname = rel_u.rsplit("/", 1)[-1]
    if fname == "snapshot_ext.rs":
        return True
    if "corp_roles" in rel_u or "corp_governance" in rel_u:
        return True
    return fname.startswith(("corp_registry", "social_corp"))


def _matches_t4_combat_module_file(rel):
    rel_u = rel.replace("\\", "/")
    if rel_u == "crates/eve-proto/src/marshal.rs":
        return True
    if not rel_u.startswith("crates/eve-server/src/") or _is_test_only_rs_path(rel_u):
        return False
    fname = rel_u.rsplit("/", 1)[-1]
    if "/tactical_modules/" in rel_u:
        return True
    return fname.startswith((
        "fitting_module_cycles",
        "combat_weapons",
        "tactical_modules",
        "pve_ess",
        "pve_homefront",
    ))


def _matches_t5_structure_pos_file(rel):
    rel_u = rel.replace("\\", "/")
    if not rel_u.startswith("crates/eve-server/src/") or _is_test_only_rs_path(rel_u):
        return False
    fname = rel_u.rsplit("/", 1)[-1]
    return (
        fname.startswith(("structure_", "structures_", "upwell", "pos_", "starbase"))
        or fname == "pos.rs"
        or "_upwell" in fname
        or "_pos." in fname
        or "_pos_" in fname
        or "starbase" in fname
    )


def _matches_t6_proto_quasar_file(rel):
    rel_u = rel.replace("\\", "/")
    if rel_u.startswith("crates/eve-proto/") or rel_u.startswith("crates/eve-quasar/"):
        return True
    if not rel_u.startswith("crates/") or _is_test_only_rs_path(rel_u):
        return False
    fname = rel_u.rsplit("/", 1)[-1]
    return "/quasar_" in rel_u or fname.startswith("quasar_")


def derive_affected_test_commands(root, base, cargo, head="HEAD"):
    """Derive targeted `cargo test -j 4` (and Python `unittest`) commands covering:
    1. Always-on wire/bootstrap sentinels (`rpc_client_contracts`, `login_space_bootstrap_sequence`,
       `client_harness`, `--lib macho_listener`) plus 12 always-on surgical historical canaries
       (`SENTINEL_IT_CANARY_TESTS`, `SENTINEL_LIB_CANARY_TESTS`) whenever Rust crate files change.
    2. Reverse-import / RPC-service (including core services with `df > 45`, TT-03) / `On*`-notification
       / symbol affected `tests/it/*.rs` (never truncating directly modified `tests/it/*.rs` or `--lib`
       modules) and crate `--lib` modules (including `crates/eve-quasar/`).
    3. Fast blast-radius & path-triggered surgical rules: central `router/` auth (`#1393`) plus `T1..T6`.
    4. Python `unittest` discovery suites for `tools/client-harness/`, `tools/sde-import/`, and `tools/`."""
    diff_base = base
    if head == "HEAD":
        for trunk_ref in ("main", "origin/main"):
            mb_proc = subprocess.run(["git", "merge-base", trunk_ref, "HEAD"], cwd=root, capture_output=True, text=True)
            head_proc = subprocess.run(["git", "rev-parse", "HEAD"], cwd=root, capture_output=True, text=True)
            if mb_proc.returncode == 0 and head_proc.returncode == 0:
                mb_sha = mb_proc.stdout.strip()
                head_sha = head_proc.stdout.strip()
                if mb_sha and mb_sha != head_sha:
                    if subprocess.run(["git", "merge-base", "--is-ancestor", diff_base, mb_sha], cwd=root).returncode == 0:
                        diff_base = mb_sha
                    break

    _, diff_files_out = sh(
        [
            "git", "diff", "--name-only", diff_base, head, "--",
            "crates/", "client-bridge/", "tools/", "Cargo.toml", "Cargo.lock", ".cargo/",
            ".agents/skills/evomu-adversarial-review/",
        ],
        root,
    )
    changed_all = [l.strip() for l in diff_files_out.splitlines() if l.strip()]
    changed_crates = [f for f in changed_all if f.startswith("crates/") and f.endswith((".rs", ".json", ".txt", ".toml", ".proto"))]
    rust_workspace_touched = any(
        f.startswith(("crates/", ".cargo/")) or f in ("Cargo.toml", "Cargo.lock")
        for f in changed_all
    )
    bridge_or_bin_touched = any(
        f.startswith(("client-bridge/", "crates/eve-server/src/bin/"))
        for f in changed_all
    )
    client_harness_touched = any(f.startswith("tools/client-harness/") for f in changed_all)
    sde_import_touched = any(f.startswith("tools/sde-import/") for f in changed_all)
    tools_py_touched = any(
        f.startswith("tools/tests/")
        or (f.startswith("tools/") and "/" not in f[len("tools/"):] and f.endswith(".py"))
        for f in changed_all
    )
    agents_gate_touched = any(f.startswith(".agents/skills/evomu-adversarial-review/") for f in changed_all)
    router_auth_touched = any(_is_central_router_auth_file(f) for f in changed_crates)
    t1_client_err_touched = any(
        f.startswith("crates/eve-testkit/") or f.rsplit("/", 1)[-1] == "client_error.rs"
        for f in changed_crates
    )
    t2_destiny_touched = any(_matches_t2_destiny_space_file(f) for f in changed_crates)
    t3_corp_touched = any(_matches_t3_corp_auth_file(f) for f in changed_crates)
    t4_combat_touched = any(_matches_t4_combat_module_file(f) for f in changed_crates)
    t5_struct_touched = any(_matches_t5_structure_pos_file(f) for f in changed_crates)
    t6_proto_touched = any(_matches_t6_proto_quasar_file(f) for f in changed_crates)
    # Only emit Rust sentinel/canary suites when at least one Rust/crate/workspace file changed.
    include_sentinel = rust_workspace_touched

    direct_lib_filters_by_crate = {}
    extra_lib_filters_by_crate = {}
    whole_lib_crates = set()
    non_server_integ = []
    direct_server_it = []
    if bridge_or_bin_touched:
        direct_server_it.append("rpc_dispatch_and_client_harness_e2e")
    changed_server_stems = set()
    quasar_crate_touched = False

    skip_server_it = {"main", "mod", "common"} | set(SENTINEL_IT_FILTERS)

    for rel in changed_crates:
        parts = rel.replace("\\", "/").split("/")
        if len(parts) < 3 or parts[0] != "crates":
            continue
        crate = parts[1]
        if crate == "eve-quasar":
            quasar_crate_touched = True
            if os.path.isfile(os.path.join(root, "crates", "eve-quasar", "Cargo.toml")):
                if len(parts) >= 4 and parts[2] == "src" and rel.endswith(".rs"):
                    st = _norm_src_stem("/".join(parts[3:]))
                    if st in ("lib", "main"):
                        whole_lib_crates.add("eve-quasar")
                    else:
                        direct_lib_filters_by_crate.setdefault("eve-quasar", set()).add(st)
                else:
                    whole_lib_crates.add("eve-quasar")
            direct_lib_filters_by_crate.setdefault("eve-server", set()).update(("quasar_gateway", "quasar_grpc"))
            changed_server_stems.update(("quasar_gateway", "quasar_grpc"))
            continue

        if rel.endswith((".json", ".txt")):
            fname = parts[-1]
            if crate == "eve-proto":
                whole_lib_crates.add("eve-proto")
            elif crate == "eve-testkit":
                whole_lib_crates.add("eve-testkit")
                if "notify" in fname:
                    direct_lib_filters_by_crate.setdefault("eve-server", set()).add("notify_registry")
                if "quasar_tap" in fname and "quasar_tap" not in direct_server_it:
                    direct_server_it.append("quasar_tap")
            elif crate == "eve-db":
                direct_lib_filters_by_crate.setdefault("eve-db", set()).add("lock_rank")
            elif crate != "eve-server":
                whole_lib_crates.add(crate)
            continue

        if not rel.endswith(".rs"):
            continue

        if parts[2] == "tests":
            if crate == "eve-server" and len(parts) >= 5 and parts[3] == "it":
                submod = os.path.splitext(parts[-1])[0]
                if submod == "mod" and len(parts) >= 6:
                    submod = parts[-2]
                if submod not in skip_server_it and submod not in direct_server_it:
                    direct_server_it.append(submod)
            elif len(parts) == 4:
                bin_name = os.path.splitext(parts[3])[0]
                non_server_integ.append((crate, bin_name, None))
            elif len(parts) >= 5:
                bin_name = parts[3]
                submod = os.path.splitext(parts[-1])[0]
                non_server_integ.append((crate, bin_name, submod))
        elif parts[2] == "src":
            stem = os.path.splitext(parts[-1])[0]
            if stem == "mod" and len(parts) >= 5:
                stem = parts[-2]
            if stem in ("lib", "main"):
                if crate != "eve-server":
                    whole_lib_crates.add(crate)
                else:
                    direct_lib_filters_by_crate.setdefault(crate, set()).add("config::tests")
                continue
            base_stem = _norm_src_stem("/".join(parts[3:]))
            if not (crate == "eve-server" and base_stem == SENTINEL_LIB_FILTER):
                direct_lib_filters_by_crate.setdefault(crate, set()).add(base_stem)
            if crate == "eve-server":
                rel_in_src = "/".join(parts[3:])
                fname = parts[-1]
                if not (rel_in_src.startswith("tests/") or "/tests/" in rel_in_src
                        or fname == "tests.rs" or fname.endswith(("_tests.rs", "_test.rs"))):
                    changed_server_stems.add(base_stem)
        else:
            if crate != "eve-server":
                whole_lib_crates.add(crate)

    # Reverse-import / RPC-service / On*-notification / symbol test selection for `eve-server`
    selected_server_it = []
    direct_present_count = 0
    diff_src = ""
    if changed_server_stems or direct_server_it or quasar_crate_touched or "eve-server" in direct_lib_filters_by_crate:
        _, diff_src = sh(["git", "diff", "-U0", diff_base, head, "--", "crates/eve-server/src/"], root)
        all_added_src = "\n".join(
            ln[1:] for ln in diff_src.splitlines() if ln.startswith("+") and not ln.startswith("+++")
        )
        all_changed_src = "\n".join(
            ln[1:]
            for ln in diff_src.splitlines()
            if (ln.startswith("+") or ln.startswith("-")) and not ln.startswith(("+++", "---"))
        )
        if re.search(r"\bencode_(?:varint|bytes|string|fixed32|fixed64)_field\s*\(", all_added_src):
            direct_lib_filters_by_crate.setdefault("eve-proto", set()).add("pb")
        if re.search(r"\buser_error\s*\(", all_added_src):
            direct_lib_filters_by_crate.setdefault("eve-testkit", set()).add("client_contract")
        if re.search(r"\b(?:BridgeRpcError|ClientError)\b|\buser_error\s*\(", all_changed_src):
            t1_client_err_touched = True
        if re.search(
            r"\.(?:notify|push_notification|notify_remote_message)\s*\(|\b(?:build_macho_notification_packet|notify_char)\s*\(",
            all_added_src,
        ):
            direct_lib_filters_by_crate.setdefault("eve-server", set()).add("notify_registry")

    if changed_server_stems or direct_server_it or quasar_crate_touched:
        idx = _load_or_build_reverse_dep_index(root)
        it_tokens = idx["it_tokens"]
        if it_tokens:
            if head == "HEAD":
                head_it = set(it_tokens.keys())
            else:
                ls_proc = subprocess.run(
                    ["git", "ls-tree", "-r", "--name-only", head, "crates/eve-server/tests/it/"],
                    cwd=root,
                    capture_output=True,
                    text=True,
                )
                head_it = set()
                for raw_l in ls_proc.stdout.splitlines():
                    l = raw_l.strip().replace("\\", "/")
                    if not l.endswith(".rs"):
                        continue
                    base_name = os.path.splitext(os.path.basename(l))[0]
                    if base_name == "mod":
                        segs = l.split("/")
                        if len(segs) >= 6:
                            base_name = segs[-2]
                    if base_name not in ("main", "mod", "common"):
                        head_it.add(base_name)
                head_it = head_it or set(it_tokens.keys())

            for par_s in list(changed_server_stems):
                changed_server_stems.update(idx["src_parent"].get(par_s, ()))

            hop1_stems = set()
            for st in changed_server_stems:
                rset = idx["reverse_src"].get(st, set())
                if len(rset) <= 35:
                    for r in rset:
                        hop1_stems.add(r)
                        hop1_stems.update(idx["src_parent"].get(r, ()))
            hop1_stems -= changed_server_stems

            # Propagate downstream `--lib` test modules into `extra_lib_filters_by_crate` (never displacing direct edits)
            ev_direct_lib = direct_lib_filters_by_crate.get("eve-server", set())
            ev_extra_lib = extra_lib_filters_by_crate.setdefault("eve-server", set())
            for h_st in sorted(hop1_stems):
                if len(ev_direct_lib) + len(ev_extra_lib) >= MAX_AFFECTED_LIB_FILTERS:
                    break
                if h_st not in ("tests", SENTINEL_LIB_FILTER) and h_st not in ev_direct_lib:
                    ev_extra_lib.add(h_st)

            file_lines = {}
            cur_f = None
            in_prod = False
            prod_lines = []
            for ln in diff_src.splitlines():
                if ln.startswith("+++ b/"):
                    cur_f = ln[6:]
                    in_prod = not (cur_f.endswith(("_tests.rs", "_test.rs")) or os.path.basename(cur_f) == "tests.rs")
                elif ln.startswith("@@") and cur_f and in_prod:
                    m = re.search(r"\+(\d+)(?:,(\d+))?", ln)
                    if m:
                        a0 = int(m.group(1))
                        cnt = int(m.group(2)) if m.group(2) is not None else 1
                        file_lines.setdefault(cur_f, set()).update(range(max(1, a0), max(1, a0) + max(1, min(cnt, 40))))
                elif in_prod and (ln.startswith("+") or ln.startswith("-")) and not ln.startswith(("+++", "---")):
                    prod_lines.append(ln[1:])
            changed_blob = "\n".join(prod_lines)

            enc_fns = set()
            for rf, lns in file_lines.items():
                enc_fns |= _enclosing_fns_at_ref(root, head, rf, lns)

            qw = {}
            if quasar_crate_touched:
                qw["kw:quasar"] = 24.0
            for st in changed_server_stems:
                qw["mod:" + st] = max(qw.get("mod:" + st, 0.0), 22.0)
            for st in hop1_stems:
                qw["mod:" + st] = max(qw.get("mod:" + st, 0.0), 8.0)
            for fn in enc_fns:
                if fn not in _DEP_STOP_WORDS:
                    qw["sym:" + fn] = max(qw.get("sym:" + fn, 0.0), 34.0)
                    for w in fn.split("_"):
                        if len(w) >= 4 and w not in _DEP_STOP_WORDS:
                            qw["kw:" + w] = max(qw.get("kw:" + w, 0.0), 18.0)
            for lhs, rhs in _ANY_QUAL_SYM_RE.findall(changed_blob):
                if lhs in idx["known_src_mods"] and rhs not in _DEP_STOP_WORDS and rhs[0].islower():
                    qw["sym:" + rhs] = max(qw.get("sym:" + rhs, 0.0), 24.0)
            for m1, m2 in _ON_NOTIF_RE.findall(changed_blob):
                ev = m1 or m2
                qw["on:" + ev] = max(qw.get("on:" + ev, 0.0), 14.0)
                for lmod in sorted(idx["lib_notif_mods"].get(ev, ())):
                    if len(ev_direct_lib) + len(ev_extra_lib) < MAX_AFFECTED_LIB_FILTERS and lmod not in ev_direct_lib:
                        ev_extra_lib.add(lmod)
            for fx in _FX_RE.findall(changed_blob):
                qw["fx:" + fx] = max(qw.get("fx:" + fx, 0.0), 18.0)
            for q in _QUOTED_IDENT_RE.findall(changed_blob):
                if q in idx["services"]:
                    qw["svc:" + q] = max(qw.get("svc:" + q, 0.0), 10.0)
                elif q in idx["methods"]:
                    qw["rpc:" + q] = max(qw.get("rpc:" + q, 0.0), 16.0)

            n_it = idx["n_it"]
            avg_len = idx["avg_len"]
            token_df = idx["token_df"]
            rem_weights = {}
            high_df_weights = {}
            for tok, w in qw.items():
                df = token_df.get(tok, 0)
                if 1 <= df <= 45:
                    rem_weights[tok] = w * math.log((n_it + 1.0) / (df + 0.5))
                elif df > 45 and tok.startswith(("svc:", "rpc:", "mod:", "sym:", "on:", "fx:")):
                    # TT-03: retain damped IDF signal for high-df core EVE services (charUnboundMgr,
                    # dogmaIM, beyonce, invbroker, stationSvc, corpRegistry, publicGatewaySvc, marketProxy,
                    # agentMgr) instead of dropping them to 0.0 at df > 45.
                    idf = max(0.20, math.log((n_it + 1.0) / (df + 0.5)))
                    high_df_weights[tok] = w * idf * 0.25
            if not rem_weights and high_df_weights:
                rem_weights.update(high_df_weights)
            elif high_df_weights:
                # Keep core-service `svc:` tokens as low-weight tie-breakers (never drowning specific df<=45 tokens)
                for tok, hw in high_df_weights.items():
                    if tok.startswith("svc:"):
                        rem_weights[tok] = hw * 0.08

            # TT-03: Never truncate directly modified `tests/it/**/*.rs` files at [:4]; keep all of them,
            # and always preserve at least `MAX_AFFECTED_IT_FILTERS - 4` (= 9) reverse-dep slots so
            # editing >4 IT files never starves reverse-dependency discovery.
            selected_server_it = [s for s in direct_server_it if s in head_it]
            direct_present_count = len(selected_server_it)
            rev_dep_target = max(MAX_AFFECTED_IT_FILTERS, direct_present_count + max(0, MAX_AFFECTED_IT_FILTERS - 4))
            for m in selected_server_it:
                for tok in it_tokens.get(m, ()):
                    if tok in rem_weights:
                        rem_weights[tok] *= 0.25

            candidates = [m for m in sorted(it_tokens) if m in head_it and m not in selected_server_it]
            while len(selected_server_it) < rev_dep_target and candidates:
                best_m, best_sc = None, 0.0
                for m in candidates:
                    toks = it_tokens[m]
                    sc = sum(rem_weights.get(t, 0.0) for t in toks)
                    if sc > 0:
                        sc /= (0.6 + 0.4 * (len(toks) / avg_len))
                        if sc > best_sc:
                            best_m, best_sc = m, sc
                if not best_m or best_sc <= 0:
                    break
                selected_server_it.append(best_m)
                candidates.remove(best_m)
                for tok in it_tokens[best_m]:
                    if tok in rem_weights:
                        rem_weights[tok] *= 0.2
        else:
            selected_server_it = list(direct_server_it)
            direct_present_count = len(selected_server_it)

    if router_auth_touched:
        # Avoid running `router::binding` twice when `router/binding.rs` also triggers the blast-radius rule below.
        direct_lib_filters_by_crate.get("eve-server", set()).discard("binding")
        extra_lib_filters_by_crate.get("eve-server", set()).discard("binding")

    cmds = []
    seen_cmds = set()

    def _add_cmd(raw_cmd):
        c = _ensure_cargo_j4(raw_cmd)
        k = tuple(c)
        if k not in seen_cmds:
            seen_cmds.add(k)
            cmds.append(c)

    if include_sentinel:
        for sflt in SENTINEL_IT_FILTERS:
            _add_cmd([cargo, "test", "-p", "eve-server", "--test", "it", sflt])
        for cflt in SENTINEL_IT_CANARY_TESTS:
            _add_cmd([cargo, "test", "-p", "eve-server", "--test", "it", cflt])
        _add_cmd([cargo, "test", "-p", "eve-server", "--lib", SENTINEL_LIB_FILTER])
        for cflt in SENTINEL_LIB_CANARY_TESTS:
            _add_cmd([cargo, "test", "-p", "eve-server", "--lib", cflt])

    for crate in sorted(whole_lib_crates):
        _add_cmd([cargo, "test", "-p", crate, "--lib"])

    all_lib_crates = sorted(set(direct_lib_filters_by_crate) | set(extra_lib_filters_by_crate))
    for crate in all_lib_crates:
        if crate in whole_lib_crates:
            continue
        direct_f = [f for f in sorted(direct_lib_filters_by_crate.get(crate, set())) if f != "tests"] or sorted(
            direct_lib_filters_by_crate.get(crate, set())
        )
        if "notify_registry" in direct_f:
            direct_f = ["notify_registry"] + [f for f in direct_f if f != "notify_registry"]
        extra_f = [
            f for f in sorted(extra_lib_filters_by_crate.get(crate, set()))
            if f != "tests" and f not in direct_f
        ]
        # TT-03: Always include every directly modified module; only cap propagated extra modules.
        cap = MAX_AFFECTED_LIB_FILTERS if crate == "eve-server" else 4
        remaining_extra = max(0, cap - len(direct_f))
        for flt in direct_f + extra_f[:remaining_extra]:
            _add_cmd([cargo, "test", "-p", crate, "--lib", flt])

    it_limit = max(MAX_AFFECTED_IT_FILTERS, direct_present_count + max(0, MAX_AFFECTED_IT_FILTERS - 4))
    for submod in selected_server_it[:it_limit]:
        _add_cmd([cargo, "test", "-p", "eve-server", "--test", "it", submod])

    non_root_integ = {(c, b) for c, b, s in non_server_integ if s and s not in ("main", "mod")}
    seen_integ = set()
    integ_per_bin = {}
    for crate, bin_name, submod in non_server_integ:
        if (crate, bin_name, None) in seen_integ:
            continue
        if submod in ("main", "mod") and (crate, bin_name) in non_root_integ:
            continue
        key = (crate, bin_name, submod)
        if key in seen_integ:
            continue
        if integ_per_bin.get((crate, bin_name), 0) >= 4:
            continue
        seen_integ.add(key)
        integ_per_bin[(crate, bin_name)] = integ_per_bin.get((crate, bin_name), 0) + 1
        if submod and submod not in ("main", "mod"):
            _add_cmd([cargo, "test", "-p", crate, "--test", bin_name, submod])
        else:
            # TT-03: Run root non-server integration binary unfiltered instead of hardcoded `"smoke"`.
            _add_cmd([cargo, "test", "-p", crate, "--test", bin_name])

    if router_auth_touched:
        _add_cmd([cargo, "test", "-p", "eve-server", "--lib", "router::binding"])
        _add_cmd([cargo, "test", "-p", "eve-server", "--bin", "eve-server", "main_unit_tests"])
        _add_cmd([cargo, "test", "-p", "eve-server", "--test", "it", "quasar_gateway"])
        _add_cmd([
            cargo, "test", "-p", "eve-testkit", "--test", "property_coverage_suite",
            "test_t5_server_dispatch_and_quasar_gateway_routing",
        ])

    # Path-triggered surgical rules T1..T6 (user decision 2026-11-20)
    if t1_client_err_touched:
        _add_cmd([cargo, "test", "-p", "eve-testkit", "--lib", T1_CLIENT_CONTRACT_TEST])
    if t2_destiny_touched:
        _add_cmd([cargo, "test", "-p", "eve-destiny", "--lib"])
        _add_cmd([cargo, "test", "-p", "eve-destiny", "--test", "t11_destiny_differential"])
        for tflt in T2_DESTINY_IT_TESTS:
            _add_cmd([cargo, "test", "-p", "eve-server", "--test", "it", tflt])
    if t3_corp_touched:
        for tflt in T3_CORP_IT_TESTS:
            _add_cmd([cargo, "test", "-p", "eve-server", "--test", "it", tflt])
        for tflt in T3_CORP_LIB_TESTS:
            _add_cmd([cargo, "test", "-p", "eve-server", "--lib", tflt])
    if t4_combat_touched:
        for tflt in T4_COMBAT_MODULE_IT_TESTS:
            _add_cmd([cargo, "test", "-p", "eve-server", "--test", "it", tflt])
        for tflt in T4_COMBAT_MODULE_LIB_TESTS:
            _add_cmd([cargo, "test", "-p", "eve-server", "--lib", tflt])
    if t5_struct_touched:
        for tflt in T5_STRUCTURE_IT_TESTS:
            _add_cmd([cargo, "test", "-p", "eve-server", "--test", "it", tflt])
    if t6_proto_touched:
        _add_cmd([cargo, "test", "-p", "eve-proto", "--lib", T6_PROTO_LIB_TEST])

    # TT-06 / TT-29: Python unittest discovery suites when `tools/` or `.agents/` changes
    if client_harness_touched and os.path.isdir(os.path.join(root, "tools", "client-harness")):
        _add_cmd([sys.executable, "-m", "unittest", "discover", "-s", "tools/client-harness", "-p", "test_*.py", "-v"])
    if sde_import_touched and os.path.isdir(os.path.join(root, "tools", "sde-import")):
        _add_cmd([sys.executable, "-m", "unittest", "discover", "-s", "tools/sde-import", "-p", "test_*.py", "-v"])
    if tools_py_touched and os.path.isdir(os.path.join(root, "tools", "tests")):
        _add_cmd([sys.executable, "-m", "unittest", "discover", "-s", "tools/tests", "-p", "test_*.py", "-v"])
    if agents_gate_touched and os.path.isdir(os.path.join(root, ".agents", "skills", "evomu-adversarial-review", "tests")):
        _add_cmd([
            sys.executable, "-m", "unittest", "discover",
            "-s", ".agents/skills/evomu-adversarial-review/tests", "-p", "test_*.py", "-v",
        ])

    if not cmds:
        if not rust_workspace_touched and os.path.isfile(os.path.join(root, "tools", "tests", "test_release_guard.py")):
            _add_cmd([sys.executable, "-m", "unittest", "discover", "-s", "tools/tests", "-p", "test_release_guard.py", "-v"])
        else:
            _add_cmd([cargo, "test", "-p", "eve-server", "--lib", "config::tests"])
    return cmds


def _parse_coalesceable_cargo_test(cmd):
    """Parse simple single-filter or unfiltered `cargo test [-j N] -p <pkg> (--lib | --bin <name> | --test <name>) [filter]`.

    Returns `(key_tuple, filter_or_None)` when safe to coalesce, or `None` for any other command
    (e.g. `cargo check`, `--workspace`, `--ignored`, `--no-fail-fast`, `-- --exact`, extra flags)."""
    if not cmd or len(cmd) < 5 or cmd[1] != "test" or "--" in cmd:
        return None
    i = 2
    prefix = [cmd[0], "test"]
    while i < len(cmd):
        tok = cmd[i]
        if tok in ("-j", "--jobs"):
            if i + 1 >= len(cmd) or cmd[i + 1].startswith("-"):
                return None
            prefix.extend((tok, cmd[i + 1]))
            i += 2
        elif tok.startswith("-j") or tok.startswith("--jobs="):
            prefix.append(tok)
            i += 1
        else:
            break
    if i + 2 >= len(cmd) or cmd[i] != "-p" or cmd[i + 1].startswith("-"):
        return None
    prefix.extend(("-p", cmd[i + 1]))
    i += 2
    target_flag = cmd[i]
    if target_flag == "--lib":
        prefix.append("--lib")
        i += 1
    elif target_flag in ("--bin", "--test"):
        if i + 1 >= len(cmd) or cmd[i + 1].startswith("-"):
            return None
        prefix.extend((target_flag, cmd[i + 1]))
        i += 2
    else:
        return None
    rest = cmd[i:]
    if not rest:
        return tuple(prefix), None
    if len(rest) == 1 and not rest[0].startswith("-"):
        return tuple(prefix), rest[0]
    return None


def _coalesce_cargo_test_commands(cmds):
    """Coalesce simple `cargo test` commands targeting the same crate/target into one process so
    `OnceLock` SDE loading happens once per target instead of once per filter.

    Preserves first-appearance ordering of each group and leaves non-matching commands untouched."""
    order = []
    groups = {}
    for cmd in cmds:
        parsed = _parse_coalesceable_cargo_test(cmd)
        if parsed is None:
            order.append(("raw", list(cmd)))
            continue
        key, flt = parsed
        grp = groups.get(key)
        if grp is None:
            grp = {"unfiltered": False, "filters": []}
            groups[key] = grp
            order.append(("group", key))
        if flt is None:
            grp["unfiltered"] = True
        elif not grp["unfiltered"] and flt not in grp["filters"]:
            grp["filters"].append(flt)

    out = []
    for kind, item in order:
        if kind == "raw":
            out.append(item)
            continue
        grp = groups[item]
        if grp["unfiltered"]:
            out.append(list(item))
        elif len(grp["filters"]) == 1:
            out.append(list(item) + [grp["filters"][0]])
        else:
            out.append(list(item) + ["--"] + grp["filters"])
    return out


_RUST_SUMMARY_RE = re.compile(r"^test result: (?:ok|FAILED)\.\s+(\d+) passed;\s+(\d+) failed;", re.M)
_PY_UNITTEST_RAN_RE = re.compile(r"^Ran (\d+) tests? in [0-9.]+s\s*$", re.M)
_PY_UNITTEST_STATUS_RE = re.compile(r"^(OK|FAILED)(?:\s*\(([^)]*)\))?\s*$", re.M)


def _parse_test_counts(out):
    """Parse anchored Rust `libtest` and Python `unittest` summaries without matching arbitrary log text (TT-23)."""
    rust_matches = _RUST_SUMMARY_RE.findall(out)
    passed = sum(int(p) for p, _ in rust_matches)
    failed = sum(int(f) for _, f in rust_matches)

    for m_ran in _PY_UNITTEST_RAN_RE.finditer(out):
        ran = int(m_ran.group(1))
        tail = out[m_ran.end():m_ran.end() + 240]
        m_st = _PY_UNITTEST_STATUS_RE.search(tail)
        py_fail = 0
        py_skip = 0
        if m_st:
            details = m_st.group(2) or ""
            for k in ("failures", "errors"):
                km = re.search(r"\b%s=(\d+)" % k, details)
                if km:
                    py_fail += int(km.group(1))
            sm = re.search(r"\bskipped=(\d+)", details)
            if sm:
                py_skip = int(sm.group(1))
            if m_st.group(1) == "FAILED" and py_fail == 0:
                py_fail = 1
        passed += max(0, ran - py_fail - py_skip)
        failed += py_fail
    return passed, failed


_TARGET_TEMPDIR_RACE_RE = re.compile(
    r"couldn['’]t create a temp dir:[^\n]*No such file or directory \(os error 2\)",
    re.I,
)
_OS_ERR2_RE = re.compile(r"No such file or directory \(os error 2\)", re.I)
_TARGET_PATH_RE = re.compile(r"(?:^|[\s`'\"(=/])target/[^\s`'\")]*", re.I)

DEFAULT_HOURLY_CI_STATE_PATH = (
    "/Users/sergeya/.gemini/antigravity/brain/3d6533b5-943c-48df-8b5c-4ccec1132b65/scratch/hourly_test_suite_state.json"
)


def _detect_cargo_target_infra_error(out, rc, failed=0, target_dir=None):
    """Return a concise detail string iff a non-zero cargo invocation with 0 failed tests hit a `target/` race."""
    if rc == 0 or rc < 0 or failed > 0 or not out:
        return ""
    m = _TARGET_TEMPDIR_RACE_RE.search(out)
    if m:
        return m.group(0).strip()
    if not _OS_ERR2_RE.search(out):
        return ""
    norm_tdir = os.path.normpath(target_dir).replace("\\", "/") if target_dir else None
    lines = out.splitlines()
    for idx, line in enumerate(lines):
        if not _OS_ERR2_RE.search(line):
            continue
        window = "\n".join(lines[max(0, idx - 3):idx + 1]).replace("\\", "/")
        if _TARGET_PATH_RE.search(window) or (norm_tdir and norm_tdir in window):
            return " | ".join(ln.strip() for ln in lines[max(0, idx - 2):idx + 1] if ln.strip())[:240]
    return ""


def _git_is_ancestor(root, anc, desc):
    if not anc or not desc:
        return False
    return subprocess.run(
        ["git", "merge-base", "--is-ancestor", anc, desc],
        cwd=root,
        capture_output=True,
    ).returncode == 0


def _closing_issues_in_git_range(root, older, newer):
    if not older or not newer or older == newer:
        return set()
    proc = subprocess.run(
        ["git", "log", "--format=%B%x1e", "%s..%s" % (older, newer)],
        cwd=root,
        capture_output=True,
        text=True,
    )
    if proc.returncode != 0 or not proc.stdout.strip():
        return set()
    import ratchet_checks as rc_mod  # noqa: E402
    closed = set()
    for body in proc.stdout.split("\x1e"):
        if body.strip():
            closed.update(rc_mod.extract_closing_issues(body))
    return closed


def _load_eligible_red_main_state(root, base, head, state_path=None):
    """Load `hourly_test_suite_state.json` ONLY when every strict Architect Check #34 guard holds."""
    path = os.environ.get("EVOMU_HOURLY_CI_STATE_PATH", DEFAULT_HOURLY_CI_STATE_PATH) if state_path is None else state_path
    if not path or not os.path.isfile(path):
        return None
    try:
        with open(path, encoding="utf-8") as fh:
            doc = json.load(fh)
    except (OSError, ValueError):
        return None
    if not isinstance(doc, dict) or doc.get("status") != "FAIL":
        return None
    p0 = doc.get("p0_issue")
    if not isinstance(p0, dict):
        return None
    p0_num = p0.get("number")
    if not isinstance(p0_num, int) or isinstance(p0_num, bool) or p0_num <= 0:
        return None
    failed_tests = [str(x).strip() for x in (doc.get("failed_tests") or []) if str(x).strip()]
    if not failed_tests:
        return None
    main_sha = str(doc.get("main_sha") or "").strip()
    if not main_sha:
        return None
    # Verify `main_sha` exists as a commit in this git repository.
    if subprocess.run(["git", "cat-file", "-e", main_sha + "^{commit}"], cwd=root, capture_output=True).returncode != 0:
        return None
    # Guard: never subtract when the reviewed branch itself claims to Fix/Close/Resolve `#<p0_num>`.
    if p0_num in _closing_issues_in_git_range(root, base, head):
        return None
    # Guard: `main_sha` must be an ancestor of (or equal to) the branch's merge-base, or within the current
    # `origin/main` (or `main`) lineage before any fix commit closing `#<p0_num>`.
    trunk_ref = ""
    for cand in ("origin/main", "main"):
        if subprocess.run(["git", "rev-parse", "--verify", "-q", cand], cwd=root, capture_output=True).returncode == 0:
            trunk_ref = cand
            break
    merge_base = ""
    if trunk_ref and head:
        mb_proc = subprocess.run(["git", "merge-base", trunk_ref, head], cwd=root, capture_output=True, text=True)
        if mb_proc.returncode == 0:
            merge_base = mb_proc.stdout.strip()
    if not merge_base:
        merge_base = base or ""
    in_merge_base = _git_is_ancestor(root, main_sha, merge_base)
    in_trunk = bool(trunk_ref) and _git_is_ancestor(root, main_sha, trunk_ref)
    if not (in_merge_base or in_trunk):
        return None
    if in_merge_base and p0_num in _closing_issues_in_git_range(root, main_sha, merge_base):
        return None
    if in_trunk and p0_num in _closing_issues_in_git_range(root, main_sha, trunk_ref):
        return None
    return {
        "p0_issue": p0_num,
        "main_sha": main_sha,
        "failed_tests": failed_tests,
        "state_path": path,
    }


def _matches_known_red_main(observed_name, known_failed_tests):
    """Exact match or `::`-bounded `module::test` suffix match against hourly CI `failed_tests`."""
    obs = str(observed_name or "").strip()
    if not obs:
        return False
    for known in known_failed_tests:
        if obs == known:
            return True
        if "::" in obs and known.endswith("::" + obs):
            return True
        if "::" in known and obs.endswith("::" + known):
            return True
    return False


def _partition_red_main_failures(observed_failing_names, known_failed_tests):
    new_failures = []
    preexisting = []
    for name in observed_failing_names:
        if _matches_known_red_main(name, known_failed_tests):
            preexisting.append(name)
        else:
            new_failures.append(name)
    return new_failures, preexisting


def _evaluate_post_review_test_gate(
    root, base, head, ctx, text, tm, verdict, so, unclosed_rows=(), final_run=False, state_path=None
):
    """Evaluate post-review test results with target/ infra-error handling and guarded known-red-main subtraction."""
    tm_out = dict(tm)
    # 1. Persistent cargo `target/` filesystem/compile race with 0 failed tests -> infra `ERROR`, never code `FAIL`.
    if tm_out.get("infra_error") and int(tm_out.get("failed", 0) or 0) == 0:
        detail = tm_out.get("infra_error_detail") or "target/ filesystem/compile race (os error 2)"
        was_passing = verdict in ("PASS", "PASS_WITH_ISSUES")
        if was_passing:
            verdict = "ERROR"
        err_tail = "\n".join(text.splitlines()[-40:])
        so["summary"] = (
            "[TEST INFRA ERROR (retried once, 0 failed tests: %s)%s] "
            % (detail, " -> verdict set to ERROR" if was_passing else "")
        ) + so.get("summary", "")
        so["report_markdown"] = (
            "## Orchestrator Test Infrastructure Error\n\n"
            "The test phase hit a `target/` filesystem/compile race (`%s`) with `0` failed tests and still failed "
            "after automatic retry. Recorded as infrastructure `ERROR` rather than code `FAIL`.\n\n```\n%s\n```\n\n"
            % (detail, err_tail)
        ) + so.get("report_markdown", "")
        return verdict, text, tm_out

    tests_ok = (
        tm_out["exit"] == 0
        and tm_out["failed"] == 0
        and tm_out["passed"] > 0
        and not tm_out.get("killed")
        and tm_out.get("santa_blocks", 0) == 0
    )
    if tests_ok and not unclosed_rows:
        return verdict, text, tm_out

    raw_failing = re.findall(r"^test (\S+) \.\.\. FAILED", text, re.M)
    raw_failing += re.findall(r"^(?:FAIL|ERROR): (\S+)", text, re.M)
    failing_names = list(dict.fromkeys(raw_failing))
    failed_count = int(tm_out.get("failed", 0) or 0)
    all_failures_named = failed_count > 0 and len(raw_failing) >= failed_count and len(failing_names) > 0

    # 2. Guarded known-red-main subtraction (Architect Check #34 #3).
    red_main = None
    new_failures = list(failing_names)
    preexisting_red_main = []
    if (
        not final_run
        and not unclosed_rows
        and not tm_out.get("killed")
        and tm_out.get("santa_blocks", 0) == 0
        and not tm_out.get("non_test_failure")
        and all_failures_named
    ):
        red_main = _load_eligible_red_main_state(root, base, head, state_path=state_path)
        if red_main:
            new_failures, preexisting_red_main = _partition_red_main_failures(failing_names, red_main["failed_tests"])
            if not new_failures and preexisting_red_main:
                p0_num = red_main["p0_issue"]
                main_short = red_main["main_sha"][:9]
                sec_lines = [
                    "## Pre-existing red tests on main (#%d)" % p0_num,
                    "",
                    "Subtracted %d pre-existing failing test(s) tracked in #%d (`hourly_test_suite_state.json`, "
                    "`main_sha=%s`); 0 new failures observed (%d passed; branch does not claim to fix #%d):"
                    % (len(preexisting_red_main), p0_num, main_short, tm_out["passed"], p0_num),
                    "",
                ] + ["- `%s`" % name for name in preexisting_red_main]
                sec_md = "\n".join(sec_lines) + "\n"
                text = text.rstrip() + "\n\n" + sec_md
                so["summary"] = (
                    "[0 new test failures; %d pre-existing red test(s) on main (#%d) subtracted] "
                    % (len(preexisting_red_main), p0_num)
                ) + so.get("summary", "")
                so["report_markdown"] = sec_md + "\n" + so.get("report_markdown", "")
                tm_out["red_main_subtracted"] = True
                tm_out["preexisting_red_main_issue"] = p0_num
                tm_out["preexisting_red_main_tests"] = preexisting_red_main
                tm_out["new_failures"] = []
                return verdict, text, tm_out

    # 3. Real failure (or new failures alongside pre-existing red-main failures): fail hard as usual.
    err_tail = "\n".join(text.splitlines()[-40:])
    extra_pre = ""
    if red_main and preexisting_red_main:
        tm_out["preexisting_red_main_issue"] = red_main["p0_issue"]
        tm_out["preexisting_red_main_tests"] = preexisting_red_main
        tm_out["new_failures"] = new_failures
        extra_pre = ", new_failures=%s, preexisting_red_main(#%d)=%s" % (
            new_failures[:10],
            red_main["p0_issue"],
            preexisting_red_main[:10],
        )
    reason = (
        "exit=%d, passed=%d, failed=%d, scoped=%s, killed=%s, santa=%d, failing=%s, unclosed_rows=%s%s"
        % (
            tm_out["exit"],
            tm_out["passed"],
            tm_out["failed"],
            bool(tm_out.get("scoped")),
            tm_out.get("killed"),
            tm_out.get("santa_blocks", 0),
            failing_names[:10],
            list(unclosed_rows)[:10],
            extra_pre,
        )
    )
    downgraded = verdict in ("PASS", "PASS_WITH_ISSUES")
    if downgraded:
        verdict = "FAIL"
    gate_label = "cargo test --workspace gate" if final_run else "affected test gate"
    so.setdefault("blocking_findings", []).insert(0, {
        "id": "ORCHESTRATOR-POST-REVIEW-TEST-FAIL",
        "severity": "P0",
        "scope": "own_diff",
        "title": "%s failed" % gate_label,
        "evidence": "%s/tests.txt (%s)" % (ctx, reason),
        "fix": "Fix the compilation error, failing test(s), or unclosed parity row(s) in %s/tests.txt and re-run run_review.py." % ctx,
    })
    so["summary"] = (
        "[TEST GATE FAILED (%s)%s] " % (reason, " -> verdict downgraded to FAIL" if downgraded else "")
    ) + so.get("summary", "")
    so["report_markdown"] = (
        "## Orchestrator Test Gate Failure\n\n`%s` failed (%s).\n\n```\n%s\n```\n\n"
        % ("cargo test --workspace --no-fail-fast" if final_run else "affected test suite", reason, err_tail)
    ) + so.get("report_markdown", "")
    return verdict, text, tm_out


def run_tests(root, sdir, timeout, attest_dir=None, base="main", full_workspace=False):
    env = {k: v for k, v in os.environ.items() if k not in WRAPPER_ENV}
    # Isolated from agent artifacts. Disable incremental cache (~7.5 GiB/slot) so concurrent review slots stay lean.
    env["CARGO_TARGET_DIR"] = os.environ.get("EVOMU_REVIEW_TARGET_DIR") or os.path.join(sdir, "target")
    env["CARGO_INCREMENTAL"] = "0"
    # Strictly enforce Workspace Rule 3 (`-j 4`, `RUST_TEST_THREADS=4`) on every child `cargo test`
    # so concurrent review slots never exhaust macOS memory/swap.
    env["CARGO_BUILD_JOBS"] = "4"
    env["RUST_TEST_THREADS"] = "4"
    # Reuse sccache for fast compilation when available.
    if os.path.exists("/opt/homebrew/bin/sccache"):
        env["RUSTC_WRAPPER"] = "/opt/homebrew/bin/sccache"
    if attest_dir:
        shutil.rmtree(attest_dir, ignore_errors=True)
        os.makedirs(attest_dir, exist_ok=True)
        env["EVOMU_PARITY_ATTEST_DIR"] = attest_dir
    cargo = shutil.which("cargo", path=env.get("PATH")) or "/opt/homebrew/bin/cargo"
    use_full = full_workspace or os.environ.get("EVOMU_FULL_WORKSPACE_TESTS") == "1"
    raw_cmds = [[cargo, "test", "-j", "4", "--workspace", "--no-fail-fast"]] if use_full else derive_affected_test_commands(root, base, cargo)
    cmds = [_ensure_cargo_j4(c) for c in raw_cmds]
    if not use_full and any(c and os.path.basename(c[0]) == "cargo" and c[-1] in SENTINEL_IT_FILTERS for c in cmds):
        cmds.insert(0, [cargo, "check", "-j", "4", "--workspace", "--bins"])
    cmds = _coalesce_cargo_test_commands(cmds)
    has_cargo = any(c and os.path.basename(c[0]) == "cargo" for c in cmds)
    slot_fh = _acquire_cargo_test_slot(root) if has_cargo else None
    outs = []
    rc = 0
    infra_retried = False
    infra_error = False
    infra_error_detail = ""
    non_test_failure = False

    def _run_one(cmd):
        nonlocal infra_retried, infra_error, infra_error_detail, non_test_failure
        is_cargo = bool(cmd and os.path.basename(cmd[0]) == "cargo")
        print("[review] running affected test command: %s" % " ".join(cmd), flush=True)
        c_rc, c_out = sh(cmd, root, env=env, timeout=timeout)
        _, c_failed = _parse_test_counts(c_out)
        detail1 = _detect_cargo_target_infra_error(c_out, c_rc, failed=c_failed, target_dir=env["CARGO_TARGET_DIR"]) if is_cargo else ""
        if detail1:
            infra_retried = True
            print(
                "[review] cargo target/ infra error detected (exit=%d, 0 failed tests: %s); retrying once: %s"
                % (c_rc, detail1, " ".join(cmd)),
                flush=True,
            )
            os.makedirs(env["CARGO_TARGET_DIR"], exist_ok=True)
            c_rc2, c_out2 = sh(cmd, root, env=env, timeout=timeout)
            _, c_failed2 = _parse_test_counts(c_out2)
            detail2 = _detect_cargo_target_infra_error(c_out2, c_rc2, failed=c_failed2, target_dir=env["CARGO_TARGET_DIR"])
            if c_rc2 == 0:
                outs.append(
                    "$ %s\n[review] first attempt hit target/ infra race (exit=%d, 0 failed tests); retried once -> exit=0\n%s"
                    % (" ".join(cmd), c_rc, c_out2)
                )
                return 0
            if detail2:
                infra_error = True
                if not infra_error_detail:
                    infra_error_detail = detail2
                outs.append(
                    "$ %s\n[review] cargo target/ infra race persisted on retry (exit=%d, 0 failed tests: %s)\n%s"
                    % (" ".join(cmd), c_rc2, detail2, c_out2)
                )
                return c_rc2
            if c_failed2 == 0:
                non_test_failure = True
            outs.append(
                "$ %s\n[review] first attempt hit target/ infra race; retry exited %d (non-infra failure)\n%s"
                % (" ".join(cmd), c_rc2, c_out2)
            )
            return c_rc2
        if c_rc != 0 and c_failed == 0:
            non_test_failure = True
        outs.append("$ %s\n%s" % (" ".join(cmd), c_out))
        return c_rc

    try:
        for cmd in cmds:
            c_rc = _run_one(cmd)
            if c_rc != 0 and rc == 0:
                rc = c_rc
        out = "\n\n".join(outs)
        if not use_full and rc == 0:
            passed_so_far, _ = _parse_test_counts(out)
            if passed_so_far == 0:
                if slot_fh is None:
                    slot_fh = _acquire_cargo_test_slot(root)
                fallback_cmd = _ensure_cargo_j4([cargo, "test", "-j", "4", "-p", "eve-server", "--lib", "config::tests"])
                c_rc = _run_one(fallback_cmd)
                out = "\n\n".join(outs)
                if c_rc != 0:
                    rc = c_rc
    finally:
        _release_cargo_test_slot(slot_fh)
    santa = max(out.count("This application has been blocked from executing"),
                len(re.findall(r"signal: 9, SIGKILL", out)))  # Santa banner goes to the TTY
    passed, failed = _parse_test_counts(out)
    killed = rc < 0
    is_infra = bool(infra_error and failed == 0 and not non_test_failure and not killed)
    scoped = not use_full
    cmd_desc = " ; ".join(" ".join(c) for c in cmds)
    exec_label = (
        "INCOMPLETE (test runner killed by signal %d; tests and parity are partial)" % -rc
        if killed
        else (
            "INFRA_ERROR (cargo target/ race persisted after retry; 0 failed tests)"
            if is_infra
            else (("yes (scoped)" if scoped else "yes (full_workspace)") if (passed + failed) > 0 else "NO")
        )
    )
    header = ("ORCHESTRATOR SUMMARY: exit=%d santa_blocks=%d passed=%d failed=%d scoped=%s executed=%s\n"
              "command: CARGO_TARGET_DIR=%s %s\n\n"
              % (rc, santa, passed, failed, "true" if scoped else "false", exec_label,
                 env["CARGO_TARGET_DIR"], cmd_desc))
    return header + out, {
        "exit": rc,
        "santa_blocks": santa,
        "passed": passed,
        "failed": failed,
        "killed": killed,
        "scoped": scoped,
        "infra_error": is_infra,
        "infra_retried": bool(infra_retried),
        "infra_error_detail": infra_error_detail if is_infra else "",
        "non_test_failure": bool(non_test_failure),
    }


class TestRun(object):
    """Runs affected tests (or full `--workspace` on `--final`) in a background thread alongside the LLM code review."""

    def __init__(self, root, sdir, timeout, attest_dir, base="main", full_workspace=False):
        import threading
        self.result = None
        self.t0 = time.time()

        def work():
            try:
                self.result = run_tests(root, sdir, timeout, attest_dir=attest_dir, base=base, full_workspace=full_workspace)
            except Exception as e:  # noqa: BLE001
                self.result = ("ORCHESTRATOR SUMMARY: exit=-1 test runner crashed: %r\n" % e,
                               {"exit": -1, "santa_blocks": 0, "passed": 0, "failed": 0, "killed": True})
            self.wall = time.time() - self.t0

        self.thread = threading.Thread(target=work, name="evomu-review-tests", daemon=True)
        self.thread.start()

    def join(self):
        self.thread.join()
        return self.result[0], self.result[1], self.wall


# Carry-over of a code review across a rebase (user decision 2026-11-11). A track that already has a non-FAIL claims
# review and is rebased onto a newer `main` with its own change byte-identical (same `git patch-id --verbatim` of its
# code diff against `main`) keeps that review's verdict; only the test gate is re-run on the rebased tree. The test run
# is what catches interactions with the commits it was rebased over. --final reviews never carry over.
CARRY_MAX_AGE_DAYS = 7
CARRY_VERDICTS = ("PASS", "PASS_WITH_ISSUES")


def code_patch_id(root, commit):
    """Patch id of `commit`'s code change against `main` (merge base to commit, gate/generated paths excluded).
    Because `main` only moves forward, the merge base of an older commit is the `main` it was branched from, so a
    clean rebase leaves this value unchanged. Returns "" for an unknown commit or an empty change."""
    mb = subprocess.run(["git", "merge-base", "main", commit], cwd=root, capture_output=True, text=True).stdout.strip()
    if not mb or mb == subprocess.run(["git", "rev-parse", commit], cwd=root, capture_output=True,
                                      text=True).stdout.strip():
        return ""
    diff = subprocess.run(["git", "diff", "--binary", mb, commit, "--", "."] +
                          [":(exclude)%s" % p for p in rl.CODE_TREE_EXCLUDES] + [":(exclude)%s" % rl.AUDITOR_ROWS_REL],
                          cwd=root, capture_output=True).stdout
    if not diff.strip():
        return ""
    out = subprocess.run(["git", "patch-id", "--verbatim"], cwd=root, input=diff, capture_output=True).stdout.decode()
    return out.split()[0] if out.split() else ""


_TEST_REGRESSION_RE = re.compile(
    r"\b(?:flaky|flake|flakiness|now\s+fails?|breaks?\s+existing\s+test|"
    r"existing\s+tests?\b[^.\n]{0,60}\b(?:fail|fails|failing|flaky|broke|broken)|"
    r"tests?\s+(?:is|are)\s+(?:now\s+)?(?:flaky|failing))\b",
    re.I,
)
_EVIDENCE_PATH_RE = re.compile(r"([A-Za-z0-9_./\-]+\.(?:rs|py|md|toml|json|sql|sh|proto|txt))\b")


def _is_test_regression_finding(finding):
    blob = " ".join(str(finding.get(k, "") or "") for k in ("title", "evidence", "fix"))
    return bool(_TEST_REGRESSION_RE.search(blob))


def _finding_touches_changed_files(finding, changed_files):
    if not changed_files:
        return False
    norm_changed = {f.replace("\\", "/") for f in changed_files}
    basenames = {f.rsplit("/", 1)[-1] for f in norm_changed}
    blob = "%s %s" % (finding.get("evidence", ""), finding.get("title", ""))
    cited = [m.replace("\\", "/") for m in _EVIDENCE_PATH_RE.findall(blob)]
    if not cited:
        return True  # No file cited -> cannot prove finding is strictly pre-existing outside the diff
    for path in cited:
        if path in norm_changed:
            return True
        if any(cf.endswith("/" + path) or cf == path for cf in norm_changed):
            return True
        if "/" not in path and path in basenames:
            return True
    return False


def _enforce_verdict_rubric(verdict, so, changed_files=()):
    """Mechanically enforce PROC-01 verdict rules:
    - Any P0 finding -> FAIL
    - Any FALSE claim -> FAIL
    - Any P1 finding in the branch's own diff (`scope == "own_diff"`, or unspecified scope citing changed
      files / no file) -> FAIL
    - Any finding reporting a flaky/failing existing test (`scope == "existing_test_regression"` or
      test-regression text) -> FAIL
    - PASS_WITH_ISSUES is permitted ONLY for strictly pre-existing P1s outside the branch's own changes."""
    if verdict not in ("PASS", "PASS_WITH_ISSUES"):
        return verdict, []
    reasons = []
    for c in so.get("claims_assessment", []) or []:
        if str(c.get("status", "")).upper() == "FALSE":
            reasons.append("FALSE claim: %s" % str(c.get("claim", ""))[:80])
    for f in so.get("blocking_findings", []) or []:
        sev = str(f.get("severity", "")).upper()
        fid = str(f.get("id", "finding"))
        scope = str(f.get("scope", "") or "").strip().lower()
        if sev == "P0":
            reasons.append("P0 finding %s" % fid)
        elif sev == "P1":
            if scope == "existing_test_regression" or _is_test_regression_finding(f):
                reasons.append("flaky/failing existing-test finding %s" % fid)
            elif scope == "own_diff":
                reasons.append("own-diff P1 %s" % fid)
            elif scope == "pre_existing":
                continue
            elif _finding_touches_changed_files(f, changed_files):
                reasons.append("own-diff P1 %s (unscoped)" % fid)
    if reasons:
        so["summary"] = (
            "[RUBRIC ENFORCEMENT (PROC-01): %s -> verdict promoted from %s to FAIL] "
            % ("; ".join(reasons[:5]), verdict)
        ) + so.get("summary", "")
        return "FAIL", reasons
    return verdict, []


def _newest_trunk_ref(root):
    """Return the newest available trunk ref among `origin/main` and `main`, or None."""
    has_om = subprocess.run(["git", "rev-parse", "--verify", "-q", "origin/main"], cwd=root, capture_output=True).returncode == 0
    has_m = subprocess.run(["git", "rev-parse", "--verify", "-q", "main"], cwd=root, capture_output=True).returncode == 0
    if has_om and has_m:
        if subprocess.run(["git", "merge-base", "--is-ancestor", "origin/main", "main"], cwd=root, capture_output=True).returncode == 0:
            return "main"
        return "origin/main"
    if has_om:
        return "origin/main"
    if has_m:
        return "main"
    return None


def check_base_freshness(root, head="HEAD"):
    """Cheap (<50ms) base-freshness & in-memory 3-way trial-merge check against trunk (`origin/main`/`main`, TT-04).
    Never mutates the working tree or index."""
    trunk = _newest_trunk_ref(root)
    info = {
        "trunk_ref": trunk,
        "commits_behind": 0,
        "commits_ahead": 0,
        "merge_base": "",
        "merge_conflict": False,
        "conflict_files": [],
        "overlapping_files": [],
    }
    if not trunk:
        return info
    mb = subprocess.run(["git", "merge-base", trunk, head], cwd=root, capture_output=True, text=True).stdout.strip()
    info["merge_base"] = mb
    behind_p = subprocess.run(["git", "rev-list", "--count", "%s..%s" % (head, trunk)], cwd=root, capture_output=True, text=True)
    ahead_p = subprocess.run(["git", "rev-list", "--count", "%s..%s" % (trunk, head)], cwd=root, capture_output=True, text=True)
    if behind_p.returncode == 0 and behind_p.stdout.strip().isdigit():
        info["commits_behind"] = int(behind_p.stdout.strip())
    if ahead_p.returncode == 0 and ahead_p.stdout.strip().isdigit():
        info["commits_ahead"] = int(ahead_p.stdout.strip())
    if info["commits_behind"] > 0 and mb:
        t_files = set(
            subprocess.run(["git", "diff", "--name-only", mb, trunk, "--", ".", ":(exclude).agents"],
                           cwd=root, capture_output=True, text=True).stdout.splitlines()
        )
        h_files = set(
            subprocess.run(["git", "diff", "--name-only", mb, head, "--", ".", ":(exclude).agents"],
                           cwd=root, capture_output=True, text=True).stdout.splitlines()
        )
        info["overlapping_files"] = sorted(f for f in (t_files & h_files) if f.strip())
        mt = subprocess.run(
            ["git", "merge-tree", "--write-tree", "--name-only", trunk, head],
            cwd=root,
            capture_output=True,
            text=True,
        )
        if mt.returncode == 1:
            info["merge_conflict"] = True
            lines = [ln.strip() for ln in mt.stdout.splitlines() if ln.strip()]
            # First line is the conflict tree SHA; remaining non-CONFLICT lines list conflicted paths.
            cfiles = []
            for ln in lines[1:]:
                if ln.startswith("CONFLICT"):
                    continue
                if not re.fullmatch(r"[0-9a-f]{40}", ln):
                    cfiles.append(ln)
            info["conflict_files"] = sorted(set(cfiles))
    return info


def _collect_diff_parity_rows(root, base, head="HEAD"):
    """Collect ledger row IDs referenced by `// parity:` tags on any test function added/modified
    in `base..head` (plus untracked `.rs` files when `head == 'HEAD'`). Used to enforce real
    `assume_tests_pass=False` parity closure on scoped test runs (TT-01)."""
    parity_tag_re = re.compile(r"//+!?\s*parity:\s*(.+)$")
    test_attr_re = re.compile(r"#\[(?:tokio::)?test\b|#\[rstest\b|#\[test_case\b|#\[proptest\b|#\[test\(")
    fn_re = re.compile(r"\bfn\s+([A-Za-z_][A-Za-z0-9_]*)\s*[<(]")

    def _extract_tags(line):
        m = parity_tag_re.search(line)
        if not m:
            return set()
        return {t.strip() for t in m.group(1).split(",") if t.strip() and "*" not in t}

    diff_cmd = ["git", "diff", "-U0", base]
    if head != "HEAD":
        diff_cmd.append(head)
    diff_cmd += ["--", "crates/"]
    _, diff_out = sh(diff_cmd, root)

    touched_lines = {}
    rows = set()
    cur_f = None
    for ln in diff_out.splitlines():
        if ln.startswith("+++ b/"):
            cur_f = ln[6:]
        elif ln.startswith("@@") and cur_f and cur_f.endswith(".rs"):
            m = re.search(r"\+(\d+)(?:,(\d+))?", ln)
            if m:
                a0 = int(m.group(1))
                cnt = int(m.group(2)) if m.group(2) is not None else 1
                touched_lines.setdefault(cur_f, set()).update(range(max(1, a0), max(1, a0) + max(1, cnt)))

    if head == "HEAD":
        _, untracked_out = sh(["git", "ls-files", "--others", "--exclude-standard", "--", "crates/"], root)
        for uf in untracked_out.splitlines():
            uf = uf.strip()
            if uf.endswith(".rs"):
                touched_lines[uf] = None

    for rel_f, lset in touched_lines.items():
        if head == "HEAD":
            fp = os.path.join(root, rel_f)
            if not os.path.isfile(fp):
                continue
            try:
                with open(fp, encoding="utf-8", errors="replace") as fh:
                    lines = fh.read().splitlines()
            except OSError:
                continue
        else:
            p = subprocess.run(["git", "show", "%s:%s" % (head, rel_f)], cwd=root, capture_output=True, text=True)
            if p.returncode != 0:
                continue
            lines = p.stdout.splitlines()

        test_starts = [i for i, l in enumerate(lines) if test_attr_re.search(l)]
        blocks = []
        for k, attr_idx in enumerate(test_starts):
            next_attr = test_starts[k + 1] if k + 1 < len(test_starts) else len(lines)
            start_idx = attr_idx
            while start_idx > 0 and lines[start_idx - 1].lstrip().startswith(("//", "#[")):
                start_idx -= 1
            fn_idx = None
            for j in range(attr_idx, min(attr_idx + 8, next_attr)):
                if fn_re.search(lines[j]):
                    fn_idx = j
                    break
            if fn_idx is not None:
                blocks.append((start_idx, attr_idx, fn_idx))
        for k, (start_idx, _attr_idx, fn_idx) in enumerate(blocks):
            end_idx = blocks[k + 1][0] if k + 1 < len(blocks) else len(lines)
            while end_idx > fn_idx + 1 and not lines[end_idx - 1].strip():
                end_idx -= 1
            if lset is not None and not any((ln_i + 1) in lset for ln_i in range(start_idx, end_idx)):
                continue
            for j in range(start_idx, min(fn_idx + 13, end_idx)):
                rows |= _extract_tags(lines[j])
    return rows


def _compute_unclosed_rows(pre_test_done_rows, real_rows, diff_parity_rows, scoped):
    """Return sorted ledger rows expected to be closed that failed to close in `real_rows`
    (`assume_tests_pass=False`). Full-workspace runs verify all `pre_test_done_rows`;
    scoped runs verify every ledger row tagged/touched by the branch diff (TT-01)."""
    acceptable = ("DONE", "NA", "FLAGGED_FAKE", "BLOCKED_BY_FEATURES", "PARKED", "SUPERVISOR_OWNED")
    if not scoped:
        target_rows = pre_test_done_rows
    else:
        target_rows = {rid for rid in diff_parity_rows if rid in real_rows}
    return sorted(
        rid for rid in target_rows
        if (real_rows.get(rid) or {}).get("status") not in acceptable
    )


def code_review_verdict(r):
    """The code reviewer's own verdict for a claims review record. A review the code reviewer passed but the test
    gate downgraded is recorded as FAIL; its code verdict is still PASS/PASS_WITH_ISSUES only when the test gate
    failed alone and the reviewer raised no P0, own-diff P1, or test-regression finding (PROC-01). None when unknown."""
    if r.get("verdict") in CARRY_VERDICTS:
        return r["verdict"]
    if r.get("verdict") != "FAIL" or not r.get("report"):
        return None
    try:
        so = json.load(open(os.path.join(os.path.dirname(r["report"]), "result.json"))).get("structured", {})
    except Exception:  # noqa: BLE001
        return None
    if so.get("verdict") not in CARRY_VERDICTS:
        return None
    non_orch = [
        f for f in (so.get("blocking_findings", []) or [])
        if not str(f.get("id", "")).startswith("ORCHESTRATOR-")
    ]
    check_so = dict(so, blocking_findings=non_orch)
    enforced, _ = _enforce_verdict_rubric(so["verdict"], check_so, changed_files=())
    if enforced not in CARRY_VERDICTS:
        return None
    return enforced


def find_carry_review(root, head, head_pid=None):
    """Most recent non-final claims review (any worktree or the archive) of a different commit whose code patch
    equals `head`'s and whose code review passed (PASS/PASS_WITH_ISSUES, including reviews the test gate alone turned
    into FAIL). Returns (review_dict with 'code_verdict', head_patch_id); review_dict is None when nothing qualifies."""
    head_pid = head_pid if head_pid is not None else code_patch_id(root, head)
    if not head_pid:
        return None, head_pid
    cutoff = (datetime.datetime.now() - datetime.timedelta(days=CARRY_MAX_AGE_DAYS)).strftime("%Y%m%d-%H%M%S")
    cands = []
    for r in rl.all_review_records(root):
        if (r.get("mode", "claims") == "claims" and r.get("verdict") in CARRY_VERDICTS + ("FAIL",)
                and not r.get("carried_from") and not r.get("final") and r.get("timestamp", "") >= cutoff):
            cands.append(r)
    cands.sort(key=lambda r: r.get("timestamp", ""), reverse=True)
    for r in cands:
        commit = r.get("snapshot_commit") or r.get("head") or ""
        if not commit or commit[:9] == head[:9]:
            continue
        # A recorded patch id was computed against an older main; it is still valid because the merge base of that
        # commit with today's main is the same commit (main is append-only). Recompute when none was recorded.
        pid = r.get("patch_id") or code_patch_id(root, commit)
        if not pid or pid != head_pid:
            continue
        cv = code_review_verdict(r)
        if cv:
            return dict(r, code_verdict=cv), head_pid
    return None, head_pid


def carried_structured(r):
    """Structured reviewer output for a carried-over review: the original findings are listed, not re-judged."""
    return {
        "verdict": r.get("code_verdict", r["verdict"]),
        "summary": ("Code review carried over from review %s (%s, verdict %s) of commit %s, after a rebase that left "
                    "this track's code change byte-identical (git patch-id --verbatim). The code review was not "
                    "repeated; the test gate below ran on this rebased tree. Original report: %s"
                    % (r.get("timestamp"), r.get("model"), "%s (recorded %s)" % (r.get("code_verdict", r["verdict"]), r["verdict"]),
                       (r.get("snapshot_commit") or r.get("head") or "")[:9], r.get("report"))),
        "blocking_findings": [],
        "non_blocking_findings": [],
        "report_markdown": "## Carried-over review\n\nSee the original report for the code review: `%s`.\n\n"
                           % r.get("report"),
    }


def py2_check(root):
    p = os.path.join(root, "client-bridge", "evomu_bridge.py")
    if not os.path.exists(p):
        return "(bridge missing)"
    if shutil.which("python2"):
        return sh(["python2", "-m", "py_compile", p], root)[1] or "python2 py_compile OK"
    raw = open(p, "rb").read()
    head = raw.split(b"\n", 2)[:2]
    cookie = any(re.search(rb"coding[:=]\s*([-\w.]+)", l) for l in head)
    non_ascii = [i + 1 for i, l in enumerate(raw.split(b"\n")) if any(b > 127 for b in l)]
    probs = []
    if non_ascii and not cookie:
        probs.append("non-ASCII bytes on lines %s without PEP 263 coding cookie -> SyntaxError on Py2"
                     % non_ascii[:10])
    src = raw.decode("utf-8", "replace")
    for pat, why in [(r'(?<![\w])f"', "f-string"), (r"(?<![\w])f'", "f-string"),
                     (r"\bnonlocal\b", "nonlocal"), (r"\byield from\b", "yield from"),
                     (r"daemon\s*=\s*True\s*\)", "Thread(daemon=) kwarg"),
                     (r"\bTimeoutError\b", "TimeoutError builtin")]:
        if re.search(pat, src):
            probs.append("Py3-only construct: " + why)
    return "python2 not installed; static check:\n" + ("\n".join(probs) or "no obvious Py3-only constructs")


def build_prompt(ctx_dir, has_claims, mode="claims"):
    brief = open(GAP_BRIEF if mode == "gap-audit" else BRIEF).read()
    lines = [brief, "\n\n---\n## This run\n",
             "- Context directory (read these first): `%s` (includes `base_freshness.txt`, `increment_size.txt`, `tests.txt`, `diff.patch`)\n" % ctx_dir,
             "- Parity status (mechanical, authoritative for completeness): `%s/parity.md` and `parity.json`\n"
             % ctx_dir,
             "- Pinned ledger and auditor rows: `%s/`\n" % PARITY_DIR]
    if mode == "gap-audit":
        lines.append("- NA triage claimed by the agent: `docs/parity/triage.json` in the repo.\n")
    else:
        lines += ["- Open findings to re-check: `%s`\n" % OPEN_FINDINGS,
                  "- Previous review reports: `%s/`\n" % HISTORY_DIR,
                  ("- Claims to verify: `%s/claims.md`\n" % ctx_dir if has_claims
                   else "- No claims file was provided: review the diff and all open findings.\n")]
    lines += [
        "- Do NOT modify any file. Produce your final answer strictly as the JSON object required "
        "by the schema; put the full human-readable report in `report_markdown`.\n",
        "- PROC-01 VERDICT RUBRIC: Return `FAIL` for any P0, any FALSE claim, ANY P1 in this branch's own new/changed "
        "code (`scope: \"own_diff\"`), or ANY finding that an existing test is now flaky/failing "
        "(`scope: \"existing_test_regression\"`). Return `PASS_WITH_ISSUES` ONLY for strictly pre-existing P1 issues "
        "outside this branch's own changes (`scope: \"pre_existing\"`). The orchestrator mechanically promotes "
        "own-diff P1s and existing-test regressions to `FAIL`.\n",
        "- IMPORTANT (headless mode): any message that contains text without a tool call ends your session. "
        "Work silently: call tools back-to-back with no narration, and emit text only once, as the final JSON.\n",
        "- IMPORTANT: shell/terminal commands are auto-denied in this headless session, and a denied command "
        "ends your turn with no output. Do NOT run commands (no cargo, git, grep, python). Use only file "
        "view/list/search tools. The diff, test output, metrics, parity status and tool outputs are already "
        "pre-computed in the context directory.\n",
        "- Ignore any Stop-hook message asking you to run the `evomu-adversarial-review` skill; that hook is for "
        "the coding agent, not for you.\n"]
    return "".join(lines)


def render_report(meta, so, parity_md=""):
    out = ["# EvoMu independent %s: %s" % ("gap audit" if meta.get("mode") == "gap-audit" else "review",
                                          meta["verdict"]), "",
           "| Field | Value |", "|---|---|"]
    for k in ["verdict", "mode", "timestamp", "model", "effort", "head", "base", "tree", "tests_executed",
              "test_summary", "commits_behind_trunk", "merge_conflict", "parity_percent", "conversation_id"]:
        out.append("| %s | `%s` |" % (k, meta.get(k)))
    out += ["", "## Summary", so.get("summary", "")]
    if meta.get("mode") == "gap-audit":
        out += ["", "## New ledger rows (%d)" % len(so.get("new_rows", []))]
        for r in so.get("new_rows", []):
            out.append("- `%s`: %s (%s)" % (r.get("id"), r.get("title"), ", ".join(r.get("sources", [])[:3])))
        out += ["", "## Rejected NA triage (%d)" % len(so.get("rejected_triage", []))]
        for r in so.get("rejected_triage", []):
            out.append("- `%s`: %s" % (r.get("id"), r.get("reason")))
        out += ["", "## Fake DONE rows (%d)" % len(so.get("fake_done", []))]
        for r in so.get("fake_done", []):
            out.append("- `%s`: %s" % (r.get("id"), r.get("evidence")))
        out += ["", "Cleared fake flags: %s" % ", ".join(so.get("cleared_fake", []) or ["none"])]
    else:
        out += ["", "## Blocking findings", "| ID | Sev | Scope | Title | Evidence | Fix |", "|---|---|---|---|---|---|"]
        for f in so.get("blocking_findings", []):
            out.append("| %s | %s | %s | %s | %s | %s |" % tuple(
                str(f.get(k, "")).replace("|", "\\|").replace("\n", " ")
                for k in ["id", "severity", "scope", "title", "evidence", "fix"]))
        out += ["", "## Claims assessment", "| Claim | Status | Evidence |", "|---|---|---|"]
        for c in so.get("claims_assessment", []):
            out.append("| %s | %s | %s |" % tuple(
                str(c.get(k, "")).replace("|", "\\|").replace("\n", " ")
                for k in ["claim", "status", "evidence"]))
    if parity_md:
        out += ["", "## Parity (mechanical)", "", parity_md.split("\n## First open")[0]]
    out += ["", "## Full report", "", so.get("report_markdown", "")]
    return "\n".join(out)


def launch_reviewer(first_cmd, root, sdir, run_dir, tree_before, ts, valid_verdicts, prefix="",
                    lock_name="review_in_progress.json", check_tree=True, chain=None):
    """Run the reviewer, falling back to other models when the primary is out of quota/capacity.
    `chain` overrides AUTO_CHAIN for `auto` (used by --final with FINAL_CHAIN).
    Returns (verdict, structured, conversation_id, model_used). verdict == "UNAVAILABLE" means every model
    was out of quota: nothing was reviewed and the result must never be treated as a pass."""
    i = first_cmd.index("--model")
    primary = first_cmd[i + 1]
    if primary == AUTO_MODE:
        models = list(chain or AUTO_CHAIN)
        print("[review] %sreviewer model: auto (%s; first with quota wins)" % (prefix, " > ".join(models)))
    else:
        models = [primary] + [m for m in FALLBACK_MODELS if m != primary]
    exhausted = []
    for n, model in enumerate(models):
        cmd = list(first_cmd)
        cmd[i + 1] = AGY_MODEL_IDS.get(model, model) if cmd[0] == "agy" else model
        if model in NO_EFFORT_MODELS and "--effort" in cmd:
            j = cmd.index("--effort")
            del cmd[j:j + 2]  # agy rejects --effort for these models ("--effort is not supported for model ...")
        elif "--effort" in cmd and re.search(r"-(low|medium|high|xhigh|max)$", model):
            # Effort is baked into the model id (e.g. gemini-4-neon-high, fable-5.2-high); agy rejects a conflicting --effort.
            cmd[cmd.index("--effort") + 1] = re.search(r"-(low|medium|high|xhigh|max)$", model).group(1)
        pfx = prefix if n == 0 else "%sfb%d_" % (prefix, n)
        verdict, so, conv, quota = _launch_one(cmd, root, sdir, run_dir, tree_before, ts, valid_verdicts, pfx,
                                               lock_name, check_tree)
        if not quota and verdict not in valid_verdicts and verdict != "INVALID" and \
                MODEL_UNUSABLE_RE.search(so.get("summary", "") + so.get("report_markdown", "")):
            quota = True  # the model can't be launched at all (bad selection/flags): fall through to the next one
        if not quota or verdict in valid_verdicts or verdict == "INVALID":
            if n:
                so["summary"] = ("(Reviewed by FALLBACK model %s because %s %s out of quota/capacity.) "
                                 % (model, ", ".join(exhausted), "was" if len(exhausted) == 1 else "were")) \
                    + so.get("summary", "")
            return verdict, so, conv, model
        exhausted.append(model)
        print("[review] %smodel %s is out of quota/capacity; %s" % (
            prefix, model, "trying %s" % models[n + 1] if n + 1 < len(models) else "no fallback left"))
    so = {"summary": ("REVIEWER UNAVAILABLE: every reviewer model (%s) is out of quota or capacity. NOTHING was "
                      "reviewed. This is not a pass. Follow AGENTS.md 'Reviewer unavailable': keep working only on "
                      "local wip/<task-id> branches, don't merge or push main, and re-run the review when quota "
                      "returns." % ", ".join(exhausted)),
          "report_markdown": "All reviewer models returned quota/capacity errors: %s" % ", ".join(exhausted)}
    return "UNAVAILABLE", so, "", ",".join(exhausted)


def _launch_one(first_cmd, root, sdir, run_dir, tree_before, ts, valid_verdicts, prefix="",
                lock_name="review_in_progress.json", check_tree=True):
    max_rounds = int(os.environ.get("EVOMU_REVIEW_MAX_ROUNDS", "8"))
    max_errors = int(os.environ.get("EVOMU_REVIEW_MAX_STREAM_ERRORS", "2"))
    # Detect which CLI we're using from the first command
    cli_name = first_cmd[0]  # "claude" or "agy"
    base_flags = first_cmd[3:]  # everything after `<cli> -p <prompt>`
    # The reviewer runs inside the same workspace, so it loads .agents/hooks.json. Without this, the
    # coding-agent Stop hook fires inside the reviewer session, forces "continue", burns the per-tree
    # loop guard, and interrupts the reviewer's stream.
    reviewer_env = dict(get_launcher_env(cli_name), EVOMU_REVIEW_HOOK="off", EVOMU_REVIEWER="1")
    lock = os.path.join(sdir, lock_name)
    write(lock, json.dumps({"pid": os.getpid(), "tree": tree_before, "ts": ts}))
    verdict, so, conv, env = "ERROR", {}, "", {}
    errors = 0
    quota = False
    try:
        for rnd in range(1, max_rounds + 1):
            if rnd == 1:
                run_cmd = first_cmd
            else:
                # Resumed rounds may have lost the first prompt (session restart or denied action): restate where
                # the evidence lives so the reviewer never has to guess paths (review 20261021-165850 failed that way).
                cont = CONTINUE_PROMPT + (
                    " Your evidence is in the context directory %s (claims.md, diff.patch, tests.txt, parity.json and "
                    "the rest); the repository under review is %s. Read files only with file-reading/search tools: do "
                    "NOT run shell commands (they are denied in headless mode). If you lost the reviewer brief, re-read "
                    "prompt.md (claims review) or gap_prompt.md (gap audit) in %s." % (os.path.join(run_dir, "context"), root, run_dir))
                run_cmd = [cli_name, "-p", cont, "--conversation", conv] + base_flags
                print("[review] %sround %d: resuming conversation %s" % (prefix, rnd, conv))
            p = subprocess.run(run_cmd, cwd=root, capture_output=True, text=True, env=reviewer_env)
            write(os.path.join(run_dir, "%s%s_stdout_r%d.json" % (prefix, cli_name, rnd)), p.stdout)
            write(os.path.join(run_dir, "%s%s_stderr_r%d.txt" % (prefix, cli_name, rnd)), p.stderr)
            try:
                env = json.loads(p.stdout.strip().splitlines()[-1], strict=False) if p.stdout.strip() else {}
            except Exception:  # noqa: BLE001
                env = {}
            conv = env.get("conversation_id") or conv
            so = extract_verdict(env, valid_verdicts)
            if so.get("verdict") in valid_verdicts:
                verdict = so["verdict"]
                if env.get("status") != "SUCCESS":
                    so["summary"] = ("(Verdict salvaged from a reviewer stream that ended with status %s: %s) "
                                     % (env.get("status"), env.get("error"))) + so.get("summary", "")
                break
            if env.get("status") != "SUCCESS" or not p.stdout.strip():
                if QUOTA_RE.search("%s\n%s" % (env.get("error") or "", p.stderr[-4000:])):
                    quota = True
                    so = {"summary": "Reviewer model out of quota/capacity: %s" % (
                        env.get("error") or p.stderr[-500:]).strip()}
                    verdict = "ERROR"
                    break
            if env.get("status") != "SUCCESS":
                errors += 1
                so = {"summary": "Reviewer run failed: %s" % (env.get("error") or p.stderr[-2000:] or "no output"),
                      "report_markdown": "%s exit code %d. stderr tail:\n\n```\n%s\n```"
                                         % (cli_name, p.returncode, p.stderr[-4000:])}
                if conv and errors <= max_errors:
                    print("[review] %sround %d: reviewer stream error (%s); resuming" % (prefix, rnd, env.get("error")))
                    continue
                verdict = "ERROR"
                break
            if not conv:
                break
    finally:
        try:
            os.remove(lock)
        except OSError:
            pass
    if verdict == "ERROR" and not so.get("summary"):
        so = {"summary": "Reviewer did not return a verdict after %d rounds." % max_rounds,
              "report_markdown": (env.get("response") or "")[-4000:]}
    tree_after = rl.worktree_fingerprint(root) if check_tree is not None else tree_before
    if check_tree is None:
        pass  # the caller compares trees itself (a concurrent test run may still be writing transient files)
    elif tree_after != tree_before and not check_tree:
        so["summary"] = ("(Background audit of tree %s; the workspace has since moved to %s, so this audit does "
                         "not count for the final gate on the new tree.) " % (tree_before[:12], tree_after[:12])) \
            + so.get("summary", "")
    elif tree_after != tree_before:
        verdict = "INVALID"
        so["summary"] = ("INVALID: the workspace changed during the review (tree %s -> %s). "
                         "Either the reviewer wrote files or someone edited concurrently. Re-run on a quiet tree. "
                         % (tree_before[:12], tree_after[:12])) + so.get("summary", "")
    return verdict, so, conv, quota


def apply_gap_audit(so, ts):
    """Apply auditor findings to the pinned auditor_rows.json.

    After 12 gap audits (+390 feat:* rows, 1,531 total feat:* rows), new_rows is frozen by default to prevent
    duplicate/renamed row inflation unless EVOMU_ALLOW_NEW_AUDITOR_ROWS=1 is set; fake_done, cleared_fake, and
    rejected_triage are always applied.
    """
    path = os.path.join(PARITY_DIR, "auditor_rows.json")
    aud = {"rows": [], "rejected_triage": [], "fake_done": []}
    if os.path.exists(path):
        aud = json.load(open(path))
    with open(os.path.join(PARITY_DIR, "ledger.json")) as f:
        known = {r["id"] for r in json.load(f)["rows"]} | {r["id"] for r in aud.get("rows", [])}
    added = 0
    if os.environ.get("EVOMU_ALLOW_NEW_AUDITOR_ROWS") == "1":
        for r in so.get("new_rows", []):
            rid = str(r.get("id", ""))
            if not re.match(r"^feat:[a-z0-9_]+\.[a-z0-9_.\-]+$", rid) or rid in known:
                continue
            aud.setdefault("rows", []).append(dict(r, category="feat", na_allowed=False, attest_ok=False,
                                                   added_by_audit=ts))
            known.add(rid)
            added += 1
    rej = set(aud.get("rejected_triage", []))
    rej |= {str(r.get("id")) for r in so.get("rejected_triage", [])}
    aud["rejected_triage"] = sorted(rej)
    cleared = set(so.get("cleared_fake", []))
    fake = {f["id"]: f for f in aud.get("fake_done", []) if f["id"] not in cleared}
    for f in so.get("fake_done", []):
        fake[str(f.get("id"))] = dict(f, flagged_by_audit=ts)
    aud["fake_done"] = sorted(fake.values(), key=lambda x: x["id"])
    write(path, json.dumps(aud, indent=1))
    return added


def run_parity(root, sdir, ctx, assume_tests_pass=False, out_name="parity.json", md_name="parity.md"):
    tests = os.path.join(ctx, "tests.txt")
    out_path = os.path.join(ctx, out_name)
    md_path = os.path.join(ctx, md_name)
    cmd = [sys.executable, PARITY_STATUS, "--tests", tests, "--attest-dir", os.path.join(ctx, "attest"),
           "--out", out_path, "--md", md_path, "--state-dir", sdir]
    if assume_tests_pass:
        cmd.append("--assume-tests-pass")
    rc, out = sh(cmd, root, timeout=900)
    try:
        summ = json.load(open(out_path))["summary"]
    except Exception:  # noqa: BLE001
        summ = {"percent_closed": None, "complete": False, "error": out[-2000:]}
    md = open(md_path).read() if os.path.exists(md_path) else out
    return summ, md


def reviews_since_last_audit(state):
    n = 0
    for r in reversed(state.get("reviews", [])):
        if r.get("mode") == "gap-audit" and r.get("verdict") in ("CLEAN", "GAPS"):
            break
        if r.get("mode", "claims") == "claims":
            n += 1
    return n


def gap_audit_running(sdir):
    p = os.path.join(sdir, "gap_audit_in_progress.json")
    try:
        pid = int(json.load(open(p)).get("pid", 0))
        if pid > 0:
            os.kill(pid, 0)
            return True
    except PermissionError:
        return True
    except (OSError, ValueError, TypeError):
        pass
    return False


def background_gap_audit(a, root, sdir):
    """Gap audit of an already-finished claims run (detached). Reuses its context; never blocks the agent."""
    run_dir = os.path.abspath(a.reuse_context)
    ctx = os.path.join(run_dir, "context")
    meta0 = json.load(open(os.path.join(ctx, "run_meta.json")))
    if (meta0.get("test_meta") or {}).get("pending"):
        print("[review] refusing to run background gap-audit on provisional/pending tests.txt (TT-23)")
        return 3
    ts, tree_before = meta0["ts"], meta0["tree"]
    parity = meta0.get("parity") or {}
    parity_md = open(os.path.join(ctx, "parity.md")).read() if os.path.exists(os.path.join(ctx, "parity.md")) else ""
    prompt = build_prompt(ctx, False, "gap-audit")
    write(os.path.join(run_dir, "gap_prompt.md"), prompt)
    cmd, launcher = build_reviewer_command(prompt, GAP_SCHEMA, a.model, a.effort, a.reviewer_timeout, root, a.launcher)
    verdict, so, conv, model_used = launch_reviewer(cmd, root, sdir, run_dir, tree_before, ts, GAP_EXIT, "gap_",
                                                    lock_name="gap_audit_in_progress.json", check_tree=False)
    if verdict in GAP_EXIT:
        added = apply_gap_audit(so, ts)
        so["summary"] = ("[%d new ledger rows appended to auditor_rows.json] " % added) + so.get("summary", "")
    meta = {"verdict": verdict, "mode": "gap-audit", "timestamp": ts, "model": model_used, "effort": a.effort,
            "head": meta0["head"], "base": meta0["base"], "tree": tree_before,
            "tests_executed": (meta0.get("test_meta") or {}).get("executed"),
            "test_summary": json.dumps(meta0.get("test_meta")), "parity_percent": parity.get("percent_closed"),
            "parity_complete": parity.get("complete"), "conversation_id": conv, "background": True}
    report = render_report(meta, so, parity_md)
    rpath = os.path.join(run_dir, "gap_report.md")
    write(rpath, report)
    write(os.path.join(sdir, "latest_gap_audit.md"), report)
    write(os.path.join(run_dir, "gap_result.json"), json.dumps({"meta": meta, "structured": so}, indent=2))
    rec = dict(meta, report=rpath)
    st = rl.load_state(root)
    st.setdefault("reviews", []).append(rec)
    rl.save_state(root, st)
    try:
        rl.archive_review(root, rec, run_dir)
    except Exception as e:  # noqa: BLE001
        print("[review] WARNING: could not archive background gap-audit %s: %s" % (ts, e))
    print("[review] background gap-audit VERDICT: %s (%s)" % (verdict, rpath))
    return UNAVAILABLE_EXIT if verdict == "UNAVAILABLE" else GAP_EXIT.get(verdict, 3)


LOCAL_DATA_FILES = ("evomu_sde.sqlite", "quasar_descriptors.sqlite", "quasar_core_golden.json")


def snapshot_review(a, root):
    """Review a fixed commit in a temporary detached worktree so the agent's own worktree stays free to change.

    - The snapshot gets the CURRENT gate (`main`'s .agents/) overlaid, so every review runs the latest rules no matter
      how old the branch is; no rebase is needed just to pick up gate changes.
    - Local client data files are symlinked from this worktree's data/ (they are gitignored).
    - The verdict is recorded in THIS worktree's .evomu-reviews/state.json against the snapshot tree. Coverage is
      keyed by code tree (review_lib.code_tree_of), so it covers the reviewed commit wherever it is merged.
    Uncommitted changes are NOT reviewed: commit (a local wip branch is fine) before starting a review.
    """
    sha = subprocess.run(["git", "rev-parse", "--verify", a.commit + "^{commit}"], cwd=root, capture_output=True,
                         text=True).stdout.strip()
    if not sha:
        print("[review] unknown commit: %s" % a.commit)
        return 3
    if a.mode == "claims" and a.claims_file:
        stale_head_viols = rl.check_claims_head_sha(root, os.path.abspath(a.claims_file), target_commit=sha)
        if stale_head_viols:
            for cat, vmsg, fpath, lno in stale_head_viols:
                print("[review] FAST-FAIL [%s] %s (%s:%s)" % (cat, vmsg, fpath, lno), flush=True)
            return EXIT["FAIL"]
    dirty = subprocess.run(["git", "status", "--porcelain", "--", ".", ":(exclude).agents"], cwd=root,
                           capture_output=True, text=True).stdout.strip()
    if dirty and a.commit == "HEAD":
        print("[review] NOTE: %d uncommitted change(s) in this worktree are NOT part of this review; only commit %s "
              "is reviewed." % (len(dirty.splitlines()), sha[:9]))
    sdir = rl.state_dir(root)
    ts = datetime.datetime.now().strftime("%Y%m%d-%H%M%S")
    snap_parent = os.path.join(rl.main_state_dir(root), "snapshots", "%s-%s" % (ts, sha[:9]))
    snap = os.path.join(snap_parent, os.path.basename(root))  # same basename: preflight history lookups still match
    os.makedirs(snap_parent, exist_ok=True)
    if subprocess.run(["git", "worktree", "add", "--detach", snap, sha], cwd=root, capture_output=True).returncode:
        print("[review] could not create snapshot worktree at %s" % snap)
        return 3
    try:
        gate_ref = None
        for cand_ref in ("origin/main", "main"):
            if subprocess.run(["git", "rev-parse", "--verify", "-q", cand_ref], cwd=root, capture_output=True).returncode == 0:
                gate_ref = cand_ref
                break
        if gate_ref == "origin/main" and subprocess.run(
                ["git", "merge-base", "--is-ancestor", "origin/main", "main"], cwd=root, capture_output=True).returncode == 0:
            gate_ref = "main"
        if gate_ref:
            shutil.rmtree(os.path.join(snap, ".agents"), ignore_errors=True)
            subprocess.run(["git", "checkout", gate_ref, "--", ".agents"], cwd=snap, capture_output=True)
        os.makedirs(os.path.join(snap, "data"), exist_ok=True)
        main_root = os.path.dirname(rl.main_state_dir(root))
        for f in LOCAL_DATA_FILES:
            # A new track worktree may not have its data/ symlinks yet; fall back to the main checkout's copy so the
            # test gate doesn't fail thousands of tests on a missing SDE (review 20261111-194648: 5,675 failures).
            src = next((c for c in (os.path.join(root, "data", f), os.path.join(main_root, "data", f))
                        if os.path.exists(c)), None)
            if src and not os.path.exists(os.path.join(snap, "data", f)):
                os.symlink(os.path.realpath(src), os.path.join(snap, "data", f))
        snap_sdir = rl.state_dir(snap)
        if os.path.exists(os.path.join(sdir, "state.json")):
            shutil.copy(os.path.join(sdir, "state.json"), os.path.join(snap_sdir, "state.json"))
        before = {(r.get("timestamp"), r.get("mode")) for r in rl.load_state(root).get("reviews", [])}
        argv, skip = [], False
        for i, x in enumerate(sys.argv[1:]):
            if skip:
                skip = False
                continue
            if x in ("--commit", "--claims-file"):
                skip = True
                continue
            if x.startswith("--commit=") or x.startswith("--claims-file="):
                continue
            argv.append(x)
        if a.claims_file:
            argv += ["--claims-file", os.path.abspath(a.claims_file)]
        argv += ["--in-place", "--from-snapshot"]
        if not a.no_auto_audit and not a.foreground_audit:
            argv += ["--foreground-audit"]  # the snapshot is deleted afterwards, so no detached audits from it
        env = dict(os.environ, EVOMU_REVIEW_TARGET_DIR=os.path.join(sdir, "target"))
        print("[review] reviewing commit %s in snapshot %s (current gate from main); you may keep working in %s"
              % (sha[:9], snap, root), flush=True)
        main_script = os.path.join(main_root, ".agents", "skills", "evomu-adversarial-review", "scripts", "run_review.py")
        if not os.path.exists(main_script):
            main_script = os.path.join(snap, ".agents", "skills", "evomu-adversarial-review", "scripts", "run_review.py")
        rc = subprocess.run([sys.executable, main_script] + argv, cwd=snap, env=env).returncode
        # Bring results home: run dirs, new review records, latest reports, auditor rows.
        st = rl.load_state(root)
        audited = False
        for r in rl.load_state(snap).get("reviews", []):
            if (r.get("timestamp"), r.get("mode")) in before:
                continue
            audited = audited or r.get("mode") == "gap-audit"
            rd = os.path.join(snap_sdir, r.get("timestamp", ""))
            dst = os.path.join(sdir, r.get("timestamp", ""))
            if os.path.isdir(rd) and not os.path.exists(dst):
                shutil.copytree(rd, dst, symlinks=True, ignore=shutil.ignore_patterns("target", "attest"))
            if r.get("report"):
                r["report"] = r["report"].replace(snap_sdir, sdir)
            r["snapshot_commit"] = sha
            st.setdefault("reviews", []).append(r)
            try:
                rl.archive_review(root, r, dst)  # survives `git worktree remove` of this track
            except Exception as e:  # noqa: BLE001
                print("[review] WARNING: could not archive review %s to the main checkout: %s" % (r.get("timestamp"), e))
        rl.save_state(root, st)
        for f in ("latest.md", "latest_gap_audit.md"):
            if os.path.exists(os.path.join(snap_sdir, f)):
                shutil.copy(os.path.join(snap_sdir, f), os.path.join(sdir, f))
        aud = os.path.join(".agents", "skills", "evomu-adversarial-review", "references", "parity", "auditor_rows.json")
        if audited and os.path.exists(os.path.join(snap, aud)):
            # Never write into the agent's .agents/ (that trips AGENTS_DIR_TAMPERED). Park the updated ledger rows
            # for the supervisor, who commits them to main.
            pend = os.path.join(rl.main_state_dir(root), "auditor_rows_pending")
            os.makedirs(pend, exist_ok=True)
            dst = os.path.join(pend, "%s-%s-auditor_rows.json" % (ts, os.path.basename(root)))
            shutil.copy(os.path.join(snap, aud), dst)
            print("[review] gap audit updated auditor_rows.json: parked at %s for the supervisor to commit to main"
                  % dst)
        return rc
    finally:
        subprocess.run(["git", "worktree", "remove", "--force", snap], cwd=root, capture_output=True)
        shutil.rmtree(snap_parent, ignore_errors=True)


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--claims-file", help="Markdown file with the coding agent's claims/summary")
    ap.add_argument("--base", help="Base ref for the diff (default: last reviewed HEAD, else root commit)")
    ap.add_argument("--mode", choices=["claims", "gap-audit"], default="claims",
                    help="claims = precision review of the change; gap-audit = recall audit of the parity ledger")
    ap.add_argument("--model", default=os.environ.get("EVOMU_REVIEW_MODEL", AUTO_MODE),
                    help="Reviewer model. Default 'auto' = first of %s with quota. Other values are ignored unless "
                         "EVOMU_REVIEW_ALLOW_MODEL_OVERRIDE=1." % " > ".join(AUTO_CHAIN))
    ap.add_argument("--effort", default=os.environ.get("EVOMU_REVIEW_EFFORT", "medium"))
    ap.add_argument("--reviewer-timeout", default=os.environ.get("EVOMU_REVIEW_TIMEOUT", "0s"),
                    help="agy --print-timeout (0s = wait for completion)")
    ap.add_argument("--test-timeout", type=int, default=int(os.environ.get("EVOMU_TEST_TIMEOUT", "7200")))
    ap.add_argument("--skip-tests", action="store_true", help="Do not run cargo test (recorded in report)")
    ap.add_argument("--no-auto-audit", action="store_true", help="Do not chain a due gap audit after claims")
    ap.add_argument("--foreground-audit", action="store_true",
                    help="Run a chained gap audit in the foreground instead of detaching it")
    ap.add_argument("--reuse-context", help=argparse.SUPPRESS)  # internal: background gap audit of a finished run
    ap.add_argument("--launcher", choices=["agy", "claude", "jetski-chat"], default="agy")
    ap.add_argument("--dry-run", action="store_true", help="Collect context and print the reviewer command only")
    ap.add_argument("--commit", default="HEAD",
                    help="Commit to review (default HEAD). It is reviewed in a temporary snapshot worktree, so you "
                         "can keep editing and committing in this worktree while the review runs.")
    ap.add_argument("--in-place", action="store_true",
                    help="Review this worktree's working tree directly (legacy; the tree must stay untouched)")
    ap.add_argument("--from-snapshot", action="store_true", help=argparse.SUPPRESS)
    ap.add_argument("--final", action="store_true",
                    help="Final-acceptance claims review: reviewer chain %s (first with quota wins). Only a PASS "
                         "from a --final review satisfies final_gate.py. Use once per milestone tree, not for "
                         "routine reviews." % " > ".join(FINAL_CHAIN))
    a = ap.parse_args()
    if a.final and a.mode != "claims":
        ap.error("--final applies to claims reviews only")
    if a.model != AUTO_MODE and os.environ.get("EVOMU_REVIEW_ALLOW_MODEL_OVERRIDE") != "1":
        print("[review] NOTE: --model %s ignored; reviews use 'auto' (%s). Pinning a reviewer model needs the user's "
              "approval (EVOMU_REVIEW_ALLOW_MODEL_OVERRIDE=1)." % (a.model, " > ".join(AUTO_CHAIN)))
        a.model = AUTO_MODE
    if a.effort != "medium" and os.environ.get("EVOMU_REVIEW_ALLOW_EFFORT_OVERRIDE") != "1":
        print("[review] NOTE: --effort %s ignored; reviews use 'medium' to conserve reviewer quota." % a.effort)
        a.effort = "medium"

    root = rl.repo_root(os.getcwd())
    if not (a.in_place or a.reuse_context):
        return snapshot_review(a, root)
    sdir = rl.state_dir(root)
    state = rl.load_state(root)
    if a.reuse_context:
        return background_gap_audit(a, root, sdir)
    head = subprocess.run(["git", "rev-parse", "HEAD"], cwd=root, capture_output=True, text=True).stdout.strip()
    if a.mode == "claims" and a.claims_file:
        stale_head_viols = rl.check_claims_head_sha(root, os.path.abspath(a.claims_file), target_commit=head or "HEAD")
        if stale_head_viols:
            for cat, vmsg, fpath, lno in stale_head_viols:
                print("[review] FAST-FAIL [%s] %s (%s:%s)" % (cat, vmsg, fpath, lno), flush=True)
            return EXIT["FAIL"]
    ts = datetime.datetime.now().strftime("%Y%m%d-%H%M%S")
    run_dir = os.path.join(sdir, ts)
    ctx = os.path.join(run_dir, "context")
    os.makedirs(ctx, exist_ok=True)

    tree_before = rl.worktree_fingerprint(root)
    base = pick_base(root, state, a.base)
    print("[review] mode=%s repo=%s head=%s base=%s tree=%s" % (a.mode, root, head[:9], base[:9], tree_before[:12]))

    write(os.path.join(ctx, "git_log.txt"), sh(["git", "log", "--oneline", "-n", "30"], root)[1])
    write(os.path.join(ctx, "git_status.txt"), sh(["git", "status", "--short"], root)[1])
    write(os.path.join(ctx, "diffstat.txt"), sh(["git", "diff", "--stat", base, "--", ".", ":(exclude).agents"], root)[1])
    write(os.path.join(ctx, "diff.patch"), sh(["git", "diff", base, "--", ".", ":(exclude).agents", ":(exclude)docs/index/*.json"], root)[1])
    _, changed_files_out = sh(["git", "diff", "--name-only", base, "--", ".", ":(exclude).agents"], root)
    changed_files = sorted({ln.strip() for ln in changed_files_out.splitlines() if ln.strip()})

    # TT-04: Cheap (<50ms) base-freshness & in-memory trial-merge check against trunk (`origin/main`/`main`).
    freshness = check_base_freshness(root, head)
    fresh_lines = [
        "trunk_ref: %s" % (freshness["trunk_ref"] or "(none)"),
        "commits_behind: %d" % freshness["commits_behind"],
        "commits_ahead: %d" % freshness["commits_ahead"],
        "merge_base: %s" % (freshness["merge_base"][:12] if freshness["merge_base"] else "(none)"),
        "merge_conflict: %s" % ("YES" if freshness["merge_conflict"] else "no"),
    ]
    if freshness["conflict_files"]:
        fresh_lines.append("conflict_files: %s" % ", ".join(freshness["conflict_files"]))
    if freshness["overlapping_files"]:
        fresh_lines.append("overlapping_files_modified_on_trunk: %s" % ", ".join(freshness["overlapping_files"][:30]))
    write(os.path.join(ctx, "base_freshness.txt"), "\n".join(fresh_lines) + "\n")
    if freshness["commits_behind"] > 0:
        print(
            "[review] base freshness: %d commit(s) behind %s (merge_conflict=%s, overlapping_files=%d)"
            % (
                freshness["commits_behind"],
                freshness["trunk_ref"],
                freshness["merge_conflict"],
                len(freshness["overlapping_files"]),
            ),
            flush=True,
        )

    # PROC-12: Enforce 60-file / 8,000-line increment-size cap directly inside run_review.py.
    inc_size = None
    inc_viols = []
    try:
        import preflight as pf_mod  # noqa: E402
        inc_size = pf_mod.increment_size(root)
        inc_viols = pf_mod.lint_increment_size(root, a.claims_file)
    except Exception:  # noqa: BLE001
        inc_size, inc_viols = None, []
    if inc_size is not None:
        write(
            os.path.join(ctx, "increment_size.txt"),
            "files=%d lines=%d cap_files=60 cap_lines=8000 violations=%d\n" % (inc_size[0], inc_size[1], len(inc_viols)),
        )

    agents_ref = base
    for cand_ref in ("origin/main", "main"):
        if subprocess.run(["git", "rev-parse", "--verify", "-q", cand_ref], cwd=root, capture_output=True).returncode == 0:
            agents_ref = cand_ref
            break
    if agents_ref == "origin/main" and subprocess.run(
            ["git", "merge-base", "--is-ancestor", "origin/main", "main"], cwd=root, capture_output=True).returncode == 0:
        agents_ref = "main"
    write(os.path.join(ctx, "agents_dir_diff.txt"),
          sh(["git", "diff", "--stat", agents_ref, "--", ".agents", ":(exclude)%s" % rl.AUDITOR_ROWS_REL], root)[1]
          or "(no changes under .agents/ other than auditor_rows.json)")
    write(os.path.join(ctx, "env_audit.txt"), env_audit(root))

    test_meta = {"executed": False, "note": "skipped by --skip-tests"}
    pre_test_done_rows = set()
    diff_parity_rows = set()
    if a.skip_tests:
        write(os.path.join(ctx, "tests.txt"), "ORCHESTRATOR SUMMARY: tests skipped by --skip-tests (NOT executed)\n")
        parity, parity_md = run_parity(root, sdir, ctx, assume_tests_pass=False)
    elif a.mode == "claims":
        # TT-02 / PROC-03: NEVER fabricate synthetic `test <fn> ... ok` lines or fake workspace pass counts.
        # Tests run concurrently alongside the LLM code review; write an honest PENDING notice into `tests.txt`,
        # which is overwritten with the real scoped (or `--final` full-workspace) test output once tests finish.
        scope_label = "CONCURRENT_WORKSPACE_TEST_GATE" if a.final else "CONCURRENT_SCOPED_TEST_GATE"
        provisional_header = (
            "ORCHESTRATOR SUMMARY: status=PENDING exit=0 santa_blocks=0 passed=0 failed=0 scoped=%s executed=%s\n"
            "(%s are running concurrently while you review; no test pass count exists yet. "
            "Real test output replaces this file before the verdict is recorded, and any compile/test failure "
            "or unclosed parity row automatically downgrades the verdict to FAIL. "
            "Do NOT cite a workspace-wide pass count from this placeholder.)\n"
            % (
                "false" if a.final else "true",
                scope_label,
                "Full workspace tests (`cargo test --workspace --no-fail-fast`)" if a.final
                else "Targeted affected tests (sentinels + surgical canaries + path/reverse-dep/Python suites)",
            )
        )
        write(os.path.join(ctx, "tests.txt"), provisional_header)
        test_meta = {
            "exit": None,
            "santa_blocks": 0,
            "passed": 0,
            "failed": 0,
            "killed": False,
            "scoped": not a.final,
            "pending": True,
            "executed": False,
        }
        # Pre-test static parity view (saved separately as parity_static.json for preflight baseline diffing)
        parity, parity_md = run_parity(
            root, sdir, ctx, assume_tests_pass=True, out_name="parity_static.json", md_name="parity_static.md"
        )
        shutil.copy(os.path.join(ctx, "parity_static.json"), os.path.join(ctx, "parity.json"))
        write(
            os.path.join(ctx, "parity.md"),
            "> Provisional pre-test static parity (`--assume-tests-pass`); overwritten with real executed parity "
            "(`assume_tests_pass=False`) after tests complete.\n\n" + parity_md,
        )
        try:
            pre_test_done_rows = {
                rid for rid, rinfo in json.load(open(os.path.join(ctx, "parity_static.json")))["rows"].items()
                if rinfo.get("status") == "DONE"
            }
        except Exception:  # noqa: BLE001
            pre_test_done_rows = set()
        diff_parity_rows = _collect_diff_parity_rows(root, base)
        print("[review] pre-test static parity: %s%% closed (%s/%s rows); %d diff-tagged row(s); tests run alongside code review"
              % (parity.get("percent_closed"), parity.get("closed_rows"), parity.get("total_rows"), len(diff_parity_rows)))
    else:
        print("[review] running affected tests (isolated target dir)...")
        text, tm = run_tests(root, sdir, a.test_timeout, attest_dir=os.path.join(ctx, "attest"), base=base,
                             full_workspace=a.final)
        write(os.path.join(ctx, "tests.txt"), text)
        test_meta = dict(tm, executed=(tm["passed"] + tm["failed"]) > 0 and not tm.get("killed"))
        print("[review] tests: %s" % test_meta)
        # TT-01: Always compute real post-test parity (`assume_tests_pass=False`).
        parity, parity_md = run_parity(root, sdir, ctx, assume_tests_pass=False)
        print("[review] parity (executed): %s%% closed (%s/%s rows)" % (parity.get("percent_closed"), parity.get("closed_rows"),
                                                                        parity.get("total_rows")))

    # Test-loop timing recorded by preflight (claims_template's "Test loop" line) so the reviewer can verify it.
    try:
        hp = os.path.join(rl.main_state_dir(root), "preflight_history.jsonl")
        recs = [l for l in open(hp) if '"worktree": "%s"' % os.path.basename(root) in l] if os.path.exists(hp) else []
        write(os.path.join(ctx, "test_loop.jsonl"), "".join(recs[-5:]) or "(no preflight timing recorded for this worktree)\n")
    except OSError:
        pass

    for name in ["coverage", "hollow", "stubs", "padding", "rpc_names"]:
        write(os.path.join(ctx, "metrics_%s.txt" % name),
              sh([sys.executable, os.path.join(HERE, "metrics", name + ".py")], root)[1])
    vpm = os.path.join(root, "tools", "eve-indexer", "verify_plan_mapping.py")
    if os.path.exists(vpm):
        write(os.path.join(ctx, "verify_plan_mapping.txt"), sh([sys.executable, vpm, "--check"], root, timeout=300)[1])
    bridge = os.path.join(root, "client-bridge", "evomu_bridge.py")
    if os.path.exists(bridge):
        write(os.path.join(ctx, "bridge_selftest.txt"), sh([sys.executable, bridge, "--self-test"], root, timeout=120)[1])
    write(os.path.join(ctx, "py2_compile.txt"), py2_check(root))

    write(os.path.join(ctx, "run_meta.json"), json.dumps({
        "ts": ts, "head": head, "base": base, "tree": tree_before, "test_meta": test_meta,
        "parity": parity, "base_freshness": freshness, "model": a.model, "effort": a.effort}))
    has_claims = bool(a.claims_file and os.path.exists(a.claims_file))
    if has_claims:
        shutil.copy(a.claims_file, os.path.join(ctx, "claims.md"))

    if a.launcher == "jetski-chat":
        cmd = ["jetski", "chat", "-m", "agent", "-a", BRIEF, "-a", OPEN_FINDINGS,
               "Act as the reviewer described in the attached brief. Context directory: %s. "
               "Select the model 'Claude Opus 5.5 (Medium)' before running. Do not modify files." % ctx]
        print("[review] jetski chat cannot pin a model or return structured output; launching interactive session:")
        print("  " + " ".join(cmd))
        if not a.dry_run:
            subprocess.Popen(cmd, cwd=root)
        print("[review] Interactive review launched; verdict will NOT be recorded automatically.")
        return 3

    def reviewer_cmd(prompt, schema, launcher=None):
        """Build reviewer command for claude CLI or agy, depending on what's available."""
        if launcher is None:
            launcher = a.launcher
        return build_reviewer_command(prompt, schema, a.model, a.effort, a.reviewer_timeout, root, launcher)

    extra_meta = {}

    def record(mode, verdict, so, conv, prefix, model_used=None):
        meta = {"verdict": verdict, "mode": mode, "timestamp": ts, "model": model_used or a.model, "effort": a.effort,
                "head": head, "base": base, "tree": tree_before, "tests_executed": test_meta.get("executed"),
                "test_summary": json.dumps(test_meta), "parity_percent": parity.get("percent_closed"),
                "parity_complete": parity.get("complete"), "commits_behind_trunk": freshness.get("commits_behind", 0),
                "merge_conflict": bool(freshness.get("merge_conflict")), "conversation_id": conv}
        if mode == "claims":
            meta.update(extra_meta)  # patch_id always; carried_from / carried_from_head on a carried-over review
        if a.final and mode == "claims":
            meta["final"] = True  # reviewed through FINAL_CHAIN; the only kind of PASS final_gate.py accepts
        report = render_report(meta, so, parity_md)
        rpath = os.path.join(run_dir, "%sreport.md" % prefix)
        write(rpath, report)
        write(os.path.join(sdir, "latest_gap_audit.md" if mode == "gap-audit" else "latest.md"), report)
        write(os.path.join(run_dir, "%sresult.json" % prefix), json.dumps({"meta": meta, "structured": so}, indent=2))
        rec = dict(meta, report=rpath)
        st = rl.load_state(root)
        st.setdefault("reviews", []).append(rec)
        rl.save_state(root, st)
        if not a.from_snapshot:
            # TT-06: Always archive direct `--in-place` reviews to `.evomu-reviews/review_archive/`
            # (snapshot_review archives once on the parent checkout with snapshot_commit).
            try:
                rl.archive_review(root, rec, run_dir)
            except Exception as e:  # noqa: BLE001
                print("[review] WARNING: could not archive --in-place review %s: %s" % (ts, e))
        print("[review] %s VERDICT: %s" % (mode, verdict))
        print("[review] report: %s" % rpath)
        return rpath

    # Mechanical pre-reviewer hard gates for claims mode (TT-04 merge conflict, PROC-12 increment cap)
    if a.mode == "claims" and not a.dry_run:
        pre_blockers = []
        if freshness.get("merge_conflict"):
            pre_blockers.append({
                "id": "ORCHESTRATOR-MERGE-CONFLICT",
                "severity": "P0",
                "scope": "own_diff",
                "title": "Branch has merge conflict against %s" % (freshness.get("trunk_ref") or "origin/main"),
                "evidence": "%s/base_freshness.txt (conflicts: %s)" % (ctx, ", ".join(freshness.get("conflict_files") or ["(see git merge-tree)"])),
                "fix": "Rebase onto latest %s, resolve conflicts, re-run preflight and targeted tests, then re-run run_review.py." % (freshness.get("trunk_ref") or "origin/main"),
            })
        if inc_viols:
            pre_blockers.append({
                "id": "ORCHESTRATOR-INCREMENT-TOO-LARGE",
                "severity": "P0",
                "scope": "own_diff",
                "title": "Unmerged diff exceeds 60-file / 8,000-line increment cap (PROC-12)",
                "evidence": "%s/increment_size.txt (%s)" % (ctx, inc_viols[0][1]),
                "fix": "Split the branch into smaller increments (<= 60 files and <= 8,000 changed lines) and land separately.",
            })
        if pre_blockers:
            so_block = {
                "verdict": "FAIL",
                "summary": "Orchestrator pre-review gate FAIL: %s" % "; ".join(b["title"] for b in pre_blockers),
                "blocking_findings": pre_blockers,
                "claims_assessment": [],
                "open_findings_status": [],
                "report_markdown": "## Orchestrator Pre-Review Gate Failure\n\n"
                + "\n".join("- **%s**: %s (`%s`)" % (b["id"], b["title"], b["evidence"]) for b in pre_blockers) + "\n",
            }
            record("claims", "FAIL", so_block, "", "", model_used="orchestrator-pregate")
            return EXIT["FAIL"]

    rc = 0
    modes = [a.mode]
    raw_gap_every = int(os.environ.get("EVOMU_GAP_AUDIT_EVERY", "3"))
    gap_every = raw_gap_every if os.environ.get("EVOMU_FORCE_GAP_AUDIT") == "1" else max(3, raw_gap_every)
    if a.mode == "claims" and not a.no_auto_audit and not gap_audit_running(sdir) and \
            reviews_since_last_audit(state) + 1 >= gap_every:
        modes.append("gap-audit")
    for mode in modes:
        prefix = "" if mode == a.mode else "gap_"
        if mode == "gap-audit" and mode != a.mode and not a.foreground_audit and not a.dry_run:
            if test_meta.get("pending"):
                print("[review] skipping detached gap-audit because test run did not finalize (TT-23)")
                continue
            log = open(os.path.join(run_dir, "gap_background.log"), "w")
            subprocess.Popen([sys.executable, os.path.abspath(__file__), "--mode", "gap-audit",
                              "--reuse-context", run_dir, "--model", a.model, "--effort", a.effort,
                              "--reviewer-timeout", a.reviewer_timeout],
                             cwd=root, stdout=log, stderr=subprocess.STDOUT, stdin=subprocess.DEVNULL,
                             start_new_session=True)
            print("[review] gap audit is due: launched it in the BACKGROUND (log: %s). You may commit and "
                  "continue; new ledger rows land in auditor_rows.json when it finishes, and its report goes "
                  "to .evomu-reviews/latest_gap_audit.md." % os.path.join(run_dir, "gap_background.log"))
            continue
        prompt = build_prompt(ctx, has_claims, mode)
        write(os.path.join(run_dir, "%sprompt.md" % prefix), prompt)
        cmd, actual_launcher = reviewer_cmd(prompt, GAP_SCHEMA if mode == "gap-audit" else SCHEMA)
        printable = [c if c != prompt else "<prompt: %s>" % os.path.join(run_dir, "%sprompt.md" % prefix) for c in cmd]
        print("[review] %s reviewer command (%s): %s" % (mode, actual_launcher, " ".join(printable)))
        if a.dry_run:
            continue
        valid = GAP_EXIT if mode == "gap-audit" else EXIT
        final_run = a.final and mode == "claims"
        test_run = None
        carry = None
        if mode == "claims":
            carry, extra_meta["patch_id"] = (None, "") if a.final else find_carry_review(root, head)
            if os.environ.get("EVOMU_REVIEW_NO_CARRY") == "1":
                carry = None
            if not a.skip_tests:
                print("[review] starting %s tests now, alongside the code review..." % (
                    "full workspace" if final_run else "affected"
                ), flush=True)
                test_run = TestRun(root, sdir, a.test_timeout, os.path.join(ctx, "attest"), base=base,
                                   full_workspace=final_run)
        if carry:
            print("[review] this commit's code change is byte-identical to %s, reviewed %s (%s, %s): carrying that "
                  "verdict over; only the test gate runs." % ((carry.get("snapshot_commit") or carry.get("head"))[:9],
                                                              carry.get("timestamp"), carry.get("model"),
                                                              carry["code_verdict"]), flush=True)
            extra_meta.update(carried_from=carry.get("timestamp"),
                              carried_from_head=carry.get("snapshot_commit") or carry.get("head"),
                              carried_from_report=carry.get("report"))
            verdict, so, conv, model_used = carry["code_verdict"], carried_structured(carry), carry.get("conversation_id"), \
                carry.get("model")
            if test_run is None:
                # A carried verdict rests entirely on the test run; without one there is nothing to record.
                print("[review] --skip-tests with a carried-over review: nothing was checked; not recording.")
                return 3
        else:
            print("[review] launching headless %s reviewer (this can take a while)..." % mode)
            verdict, so, conv, model_used = launch_reviewer(cmd, root, sdir, run_dir, tree_before, ts, valid, prefix,
                                                            check_tree=None if test_run is not None else True,
                                                            chain=FINAL_CHAIN if final_run else None)
            if mode == "claims" and verdict in ("PASS", "PASS_WITH_ISSUES"):
                verdict, _rubric_reasons = _enforce_verdict_rubric(verdict, so, changed_files=changed_files)
        if test_run is not None:
            if verdict not in ("PASS", "PASS_WITH_ISSUES"):
                print("[review] code review returned %s; waiting for the test run so this report covers both..."
                      % verdict, flush=True)
            text, tm, wall_test = test_run.join()
            # Clean transient crate-local `.evomu-bugs` queues created by `alert`/`bugReport` contract tests
            # when `cargo test -p <crate>` executes with `CARGO_MANIFEST_DIR` as cwd.
            for bd in glob.glob(os.path.join(root, "crates", "*", ".evomu-bugs")):
                shutil.rmtree(bd, ignore_errors=True)
            # The tree check is done here, after the tests finished, so files a test creates and deletes again
            # cannot make the review INVALID; files a test (or the reviewer) leaves behind still do.
            tree_after = rl.worktree_fingerprint(root)
            if tree_after != tree_before and verdict in EXIT:
                verdict = "INVALID"
                so["summary"] = ("INVALID: the workspace changed during the review and test run (tree %s -> %s). "
                                 "The reviewer or a test wrote files that are not gitignored. Re-run on a quiet "
                                 "tree. " % (tree_before[:12], tree_after[:12])) + so.get("summary", "")
            write(os.path.join(ctx, "tests.txt"), text)
            test_meta = dict(tm, executed=(tm["passed"] + tm["failed"]) > 0 and not tm.get("killed"))
            print("[review] tests: %s (wall %.1fs, ran alongside the code review)" % (test_meta, wall_test), flush=True)
            try:
                import preflight as pf_mod  # noqa: E402
                pf_mod.record_timing(root, wall_test, text)
            except Exception:  # noqa: BLE001
                pass
            # TT-01: ALWAYS evaluate post-test parity with `assume_tests_pass=False` against real `tests.txt`.
            parity, parity_md = run_parity(root, sdir, ctx, assume_tests_pass=False)
            print("[review] post-review parity (executed): %s%% closed (%s/%s rows)"
                  % (parity.get("percent_closed"), parity.get("closed_rows"), parity.get("total_rows")), flush=True)
            write(os.path.join(ctx, "run_meta.json"), json.dumps({
                "ts": ts, "head": head, "base": base, "tree": tree_before, "test_meta": test_meta,
                "parity": parity, "base_freshness": freshness, "model": a.model, "effort": a.effort}))
            real_rows = {}
            try:
                real_rows = json.load(open(os.path.join(ctx, "parity.json"))).get("rows", {})
            except Exception:  # noqa: BLE001
                pass
            unclosed_rows = _compute_unclosed_rows(
                pre_test_done_rows, real_rows, diff_parity_rows, scoped=bool(tm.get("scoped"))
            )
            orig_text = text
            verdict, text, tm_eval = _evaluate_post_review_test_gate(
                root, base, head, ctx, text, tm, verdict, so, unclosed_rows=unclosed_rows, final_run=final_run
            )
            if text != orig_text:
                write(os.path.join(ctx, "tests.txt"), text)
            if tm_eval != tm:
                test_meta = dict(tm_eval, executed=(tm_eval["passed"] + tm_eval["failed"]) > 0 and not tm_eval.get("killed"))
                write(os.path.join(ctx, "run_meta.json"), json.dumps({
                    "ts": ts, "head": head, "base": base, "tree": tree_before, "test_meta": test_meta,
                    "parity": parity, "base_freshness": freshness, "model": a.model, "effort": a.effort}))
        if mode == "gap-audit" and verdict in GAP_EXIT:
            added = apply_gap_audit(so, ts)
            so["summary"] = ("[%d new ledger rows appended to auditor_rows.json] " % added) + so.get("summary", "")
        record(mode, verdict, so, conv, prefix, model_used)
        if verdict == "UNAVAILABLE":
            print("[review] " + so["summary"])
        if so.get("blocking_findings"):
            print("[review] blocking findings: %d" % len(so["blocking_findings"]))
        code = UNAVAILABLE_EXIT if verdict == "UNAVAILABLE" else (GAP_EXIT if mode == "gap-audit" else EXIT).get(verdict, 3)
        if mode == a.mode:
            rc = code
        if mode == "claims" and verdict not in ("PASS", "PASS_WITH_ISSUES") and len(modes) > 1:
            print("[review] claims verdict %s: skipping the chained gap audit (it only runs on a committable "
                  "tree; fix the findings and re-run)." % verdict)
            break
        if mode == "claims" and carry and len(modes) > 1:
            print("[review] carried-over review: skipping the chained gap audit (the code is unchanged since the "
                  "review it was carried from).")
            break
    if a.dry_run:
        print("[review] dry run: context in %s" % ctx)
        return 0
    return rc


if __name__ == "__main__":
    sys.exit(main())
