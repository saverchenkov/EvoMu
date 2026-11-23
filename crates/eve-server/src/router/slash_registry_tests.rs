//! Tests for [`crate::router::slash_registry`] enforcing 1-to-1 bijection between advertised
//! remote slash commands, role enforcement, `/help` output, central dispatcher routing, and
//! shrink-only legacy leading-slash prefix parsing outside the canonical registry (`#1193`).

#![forbid(unsafe_code)]

use super::*;
use crate::services::account_services::account_services_tests::{auth_session, test_ctx};
use crate::services::auth_cache::{
    format_remote_command_list_reason as auth_format_remote_command_list_reason,
    remote_slash_commands_for_role as auth_remote_slash_commands_for_role,
    required_slash_role as auth_required_slash_role,
};
use eve_machonet::service_roles;
use eve_proto::PyRep;
use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

fn collect_rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_rs_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// Strips trailing inline `#[cfg(test)]` test modules so source checks scan only production code.
fn production_prefix(src: &str) -> &str {
    for marker in ["\n#[cfg(test)]\nmod ", "\n#[cfg(test)] #[path"] {
        if let Some(idx) = src.find(marker) {
            return &src[..idx];
        }
    }
    src
}

#[test]
fn slash_registry_bijection_no_duplicates_and_handler_binding_ratchet() {
    let all = all_remote_slash_commands();
    assert_eq!(all.len(), 72, "expected exact 72 advertised slash commands");
    assert!(
        all.windows(2).all(|w| w[0] < w[1]),
        "advertised slash command list must remain sorted and strictly unique"
    );

    let full_mask = u64::MAX;
    let derived_all = remote_slash_commands_for_role(full_mask);
    assert_eq!(derived_all.as_slice(), all);
    assert_eq!(auth_remote_slash_commands_for_role(full_mask).as_slice(), all);

    let pending = count_pending_handlers(SLASH_COMMAND_REGISTRY);
    assert_eq!(pending, PENDING_SLASH_HANDLER_COUNT);
    assert!(
        pending <= 67,
        "PENDING_SLASH_HANDLER_COUNT must be shrink-only (got {pending})"
    );
    for bound in ["operation", "pd", "setomega", "setunderage", "survey"] {
        assert!(
            lookup_slash_entry(bound).and_then(|e| e.handler).is_some(),
            "Batch 1 command '/{bound}' must bind Some(handler)"
        );
    }

    let mut seen_names = HashSet::new();
    let mut advertised_count = 0usize;
    for entry in SLASH_COMMAND_REGISTRY {
        assert!(!entry.name.is_empty(), "empty slash command name in registry");
        assert!(
            entry.name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit()),
            "slash command name '{}' must be lowercase ASCII alphanumeric",
            entry.name
        );
        assert!(
            seen_names.insert(entry.name),
            "duplicate slash command name '{}' in SLASH_COMMAND_REGISTRY",
            entry.name
        );
        for &alias in entry.aliases {
            assert!(
                seen_names.insert(alias),
                "alias '{}' collides with another registry name/alias",
                alias
            );
        }
        assert!(
            entry.usage.starts_with('/'),
            "usage for '{}' must start with '/'",
            entry.name
        );
        assert!(
            !entry.description.is_empty(),
            "description for '{}' must not be empty",
            entry.name
        );
        assert_ne!(
            entry.min_role, 0,
            "slash command '{}' must have a non-zero required role mask",
            entry.name
        );
        if entry.advertised {
            advertised_count += 1;
        }
        let with_slash = format!("/{}", entry.name);
        assert_eq!(
            lookup_slash_entry(&with_slash).map(|e| e.name),
            Some(entry.name)
        );
        assert_eq!(
            lookup_slash_entry_icase(&entry.name.to_ascii_uppercase()).map(|e| e.name),
            Some(entry.name)
        );
        assert_eq!(slash_command_usage(entry.name), entry.detailed_usage);
        assert_eq!(slash_command_usage(&with_slash), entry.detailed_usage);
    }
    assert_eq!(advertised_count, all.len());
}

#[test]
fn slash_registry_role_enforcement_and_help_parity() {
    let qa_gm = service_roles::ROLE_QA | service_roles::ROLE_GM_MASK;
    let spawn_gm = service_roles::ROLE_SPAWN | service_roles::ROLE_GM_MASK;
    let high_gm = service_roles::ROLE_GMH
        | service_roles::ROLE_GMS
        | service_roles::ROLE_ADMIN
        | service_roles::ROLE_ROLEADMIN;
    let structure_gm = service_roles::ROLE_ADMIN | service_roles::ROLE_GMH | service_roles::ROLE_GML;

    assert_eq!(
        required_slash_role("/"),
        service_roles::ROLE_QA
            | service_roles::ROLE_SPAWN
            | service_roles::ROLE_ROLEADMIN
            | service_roles::ROLE_GM_MASK
    );
    assert_eq!(required_slash_role("/whoami"), qa_gm);
    assert_eq!(required_slash_role("/WHOAMI"), service_roles::ROLE_GM_MASK);
    assert_eq!(required_slash_role("whoami"), service_roles::ROLE_GM_MASK);
    assert_eq!(required_slash_role(""), service_roles::ROLE_GM_MASK);
    assert_eq!(required_slash_role("/online"), qa_gm);
    assert_eq!(
        slash_command_usage("/online"),
        Some("/online [moduleID|all] - Online fitted modules")
    );
    assert_eq!(required_slash_role("/location"), qa_gm);
    assert_eq!(required_slash_role("/getattr me 37"), qa_gm);
    assert_eq!(required_slash_role("/survey status"), qa_gm);
    assert_eq!(required_slash_role("/moveme 30000142"), qa_gm);
    assert_eq!(required_slash_role("/translocate 30000142"), qa_gm);
    assert_eq!(required_slash_role("/spawn 587 1"), spawn_gm);
    assert_eq!(required_slash_role("/unspawn 123"), spawn_gm);
    assert_eq!(required_slash_role("/despawn 123"), spawn_gm);
    assert_eq!(required_slash_role("/spawnnpc 1001"), spawn_gm);
    assert_eq!(required_slash_role("/role 1 GMH"), high_gm);
    assert_eq!(required_slash_role("/createccpevent 1 2 3 hi"), high_gm);
    assert_eq!(required_slash_role("/structure state 1001 110"), structure_gm);
    assert_eq!(required_slash_role("/giveisk 100"), service_roles::ROLE_GM_MASK);

    for cmd in [
        "/",
        "/whoami",
        "/WHOAMI",
        "/spawn 587",
        "/role 1 GMH",
        "/structure state 1001 110",
        "/giveisk 100",
    ] {
        assert_eq!(required_slash_role(cmd), auth_required_slash_role(cmd));
    }

    assert_eq!(
        format_remote_command_list_reason(service_roles::ROLE_PLAYER),
        "Commands: []"
    );
    assert_eq!(
        auth_format_remote_command_list_reason(service_roles::ROLE_GM_MASK),
        format_remote_command_list_reason(service_roles::ROLE_GM_MASK)
    );
    let gm_reason = format_remote_command_list_reason(service_roles::ROLE_GM_MASK);
    assert!(gm_reason.starts_with("Commands: ['"));
    assert!(gm_reason.contains("'giveisk'"));
    assert!(gm_reason.contains("'heal'"));
    assert!(!gm_reason.contains("'/giveisk'"));

    let help_all = format_slash_help(service_roles::ROLE_GM_MASK, None);
    assert!(help_all.starts_with("Available slash commands (72): "));
    assert!(!help_all.contains('\n'));
    assert!(format_slash_help(service_roles::ROLE_GM_MASK, Some("/load")).contains("/load"));
    assert!(format_slash_help(service_roles::ROLE_GM_MASK, Some("tidi"))
        .contains("no detailed usage entry"));
    assert!(format_slash_help(service_roles::ROLE_GML, Some("role"))
        .contains("requires a higher role"));
    assert!(format_slash_help(service_roles::ROLE_GM_MASK, Some("nonexistent_cmd"))
        .contains("Unknown slash command '/nonexistent_cmd'"));
}

#[test]
fn slash_invocation_parser_and_central_dispatcher_routing() {
    assert!(is_slash_command("  /heal"));
    assert!(!is_slash_command("heal"));
    assert!(is_slash_list_query(" / "));
    assert!(!is_slash_list_query("/help"));

    let inv = parse_slash_invocation("  /WHOAMI  foo bar ").expect("invocation");
    assert_eq!(inv.raw, "/WHOAMI  foo bar");
    assert_eq!(inv.head_raw, "WHOAMI");
    assert_eq!(inv.head_lower, "whoami");
    assert_eq!(inv.rest, "foo bar");
    assert!(inv.had_whitespace_after_head);
    assert_eq!(
        parse_slash_invocation("/setomega").map(|i| i.had_whitespace_after_head),
        Some(false)
    );
    assert!(parse_slash_invocation("not_a_slash").is_none());

    let ctx = test_ctx();
    let demo_uid = ctx
        .db
        .with_snapshot(|s| s.accounts.values().next().map(|a| a.user_id))
        .expect("seeded account");
    let mut sess = auth_session(demo_uid, 90_000_001, 1_000_009, 30_000_142);

    // List query parity on both services.
    sess.role = service_roles::ROLE_PLAYER;
    for svc in ["slash", "publicQaToolsServer"] {
        let SlashDispatchOutcome::Handled(Err(err)) =
            dispatch_slash_command(&ctx, &sess, svc, &[PyRep::String("/".into())])
        else {
            panic!("expected Handled Err for '/' on {svc}");
        };
        if svc == "publicQaToolsServer" {
            assert_eq!(err.code, "InsufficientRole"); // rpc-lint: allow MachoNet error code assertion
        } else {
            assert_eq!(err.code, "SlashError");
            assert_eq!(err.message, "Commands: []");
        }
    }

    // Unknown and unprivileged rejections happen centrally at the dispatcher.
    let SlashDispatchOutcome::Handled(Err(role_err)) =
        dispatch_slash_command(&ctx, &sess, "slash", &[PyRep::String("/pd bonus 5".into())])
    else {
        panic!("expected InsufficientRole");
    };
    assert_eq!(role_err.code, "InsufficientRole"); // rpc-lint: allow MachoNet error code assertion

    sess.role = service_roles::ROLE_GM_MASK;
    for bad in ["/does_not_exist_cmd", "plain chat line"] {
        let SlashDispatchOutcome::Handled(Err(unk)) =
            dispatch_slash_command(&ctx, &sess, "slash", &[PyRep::String(bad.into())])
        else {
            panic!("expected UnknownSlashCommand for {bad}");
        };
        assert_eq!(unk.code, "UnknownSlashCommand"); // rpc-lint: allow MachoNet error code assertion
    }

    // Pending legacy rows yield PendingLegacyChain when authorized.
    assert!(matches!(
        dispatch_slash_command(&ctx, &sess, "slash", &[PyRep::String("/whoami".into())]),
        SlashDispatchOutcome::PendingLegacyChain
    ));
    assert!(matches!(
        dispatch_slash_command(
            &ctx,
            &sess,
            "slash",
            &[PyRep::String("/STRUCTURE state 1001 110".into())]
        ),
        SlashDispatchOutcome::PendingLegacyChain
    ));

    // Bound Batch-1 rows execute cleanly via central dispatch.
    let SlashDispatchOutcome::Handled(Ok(pd_rep)) = dispatch_slash_command(
        &ctx,
        &sess,
        "slash",
        &[PyRep::String("/pd config 100 200 100 10 5".into())],
    ) else {
        panic!("expected Handled Ok for /pd");
    };
    assert_eq!(pd_rep.dict_get("configured"), Some(&PyRep::Bool(true)));

    let SlashDispatchOutcome::Handled(Ok(op_rep)) = dispatch_slash_command(
        &ctx,
        &sess,
        "publicQaToolsServer",
        &[PyRep::String("/operation reveal_ui".into())],
    ) else {
        panic!("expected Handled Ok for /operation");
    };
    assert_eq!(op_rep, PyRep::Bool(true));

    let SlashDispatchOutcome::Handled(Ok(survey_rep)) = dispatch_slash_command(
        &ctx,
        &sess,
        "slash",
        &[PyRep::String("/survey schedule 9901 3600 34:5".into())],
    ) else {
        panic!("expected Handled Ok for /survey");
    };
    assert_eq!(survey_rep.dict_get("surveyID"), Some(&PyRep::Integer(9901)));

    let SlashDispatchOutcome::Handled(Ok(underage_rep)) = dispatch_slash_command(
        &ctx,
        &sess,
        "slash",
        &[PyRep::String(format!("/setunderage {demo_uid} true"))],
    ) else {
        panic!("expected Handled Ok for /setunderage");
    };
    assert_eq!(underage_rep.dict_get("underage"), Some(&PyRep::Bool(true)));

    let SlashDispatchOutcome::Handled(Err(omega_err)) = dispatch_slash_command(
        &ctx,
        &sess,
        "slash",
        &[PyRep::String("/setomega 999999999 100".into())],
    ) else {
        panic!("expected AccountNotFound for /setomega");
    };
    assert_eq!(omega_err.code, "AccountNotFound"); // rpc-lint: allow MachoNet error code assertion
}

#[test]
fn shrink_only_legacy_slash_prefix_parsing_outside_registry() {
    let src_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    collect_rs_files(&src_root, &mut files);
    files.sort();

    // Canonical registry + HTTP URL route handlers (never slash commands).
    let exempt_rel_paths: HashSet<&str> = [
        "bug_reporting.rs",
        "esi/router.rs",
        "router/slash_registry.rs",
        "sso.rs",
    ]
    .into_iter()
    .collect();

    // Shrink-only Batch-1 baseline for deferred legacy chains + `strip_slash_command*` helper
    // calls (prevents spelling-evasion by counting both patterns together). Each subsequent
    // Track 1 batch reduces its migrated file(s) to 0.
    let shrink_only_max_by_file: BTreeMap<&str, usize> = BTreeMap::from([
        ("services/auth_cache.rs", 14),
        ("services/character_gm_tools.rs", 25),
        ("services/gm_soak_commands.rs", 5),
        ("services/notify_emitters_seasons_tactical.rs", 1),
    ]);

    let mut observed: BTreeMap<String, usize> = BTreeMap::new();
    let mut unexpected_violations = Vec::new();
    for path in files {
        let Ok(rel) = path.strip_prefix(&src_root) else {
            continue;
        };
        let rel_str = rel.to_string_lossy().replace('\\', "/");
        if rel_str.ends_with("tests.rs")
            || rel_str.ends_with("_test.rs")
            || exempt_rel_paths.contains(rel_str.as_str())
        {
            continue;
        }
        let Ok(raw) = fs::read_to_string(&path) else {
            continue;
        };
        let prod = production_prefix(&raw);
        let mut file_hits = 0usize;
        for (idx, line) in prod.lines().enumerate() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("//") {
                continue;
            }
            if line.contains("strip_prefix(\"/")
                || line.contains("starts_with(\"/")
                || line.contains("strip_slash_command")
            {
                file_hits += 1;
                if !shrink_only_max_by_file.contains_key(rel_str.as_str()) {
                    unexpected_violations.push(format!("{rel_str}:{}: {trimmed}", idx + 1));
                }
            }
        }
        if file_hits > 0 {
            observed.insert(rel_str, file_hits);
        }
    }

    assert!(
        unexpected_violations.is_empty(),
        "found unmigrated leading-slash prefix checks outside deferred Track 1 chains:\n{}",
        unexpected_violations.join("\n")
    );
    for (rel, max_hits) in &shrink_only_max_by_file {
        let actual = observed.get(*rel).copied().unwrap_or(0);
        assert!(
            actual <= *max_hits,
            "shrink-only slash prefix parse regression in {rel}: actual={actual} > max={max_hits}"
        );
    }
}

#[test]
fn slash_case_insensitive_role_policy_and_createccpevent_parity() {
    assert_eq!(auth_required_slash_role("/structure state 1001 110"), ROLE_STRUCTURE_GM);
    assert_eq!(auth_required_slash_role("/STRUCTURE state 1001 110"), ROLE_STRUCTURE_GM);
    assert_eq!(auth_required_slash_role("/Structure state 1001 110"), ROLE_STRUCTURE_GM);
    assert_ne!(ROLE_STRUCTURE_GM, service_roles::ROLE_GM_MASK);
    assert_eq!(auth_required_slash_role("/WHOAMI"), service_roles::ROLE_GM_MASK);
    assert_eq!(auth_required_slash_role("/createccpevent 1 test"), ROLE_HIGH_GM);
    assert_eq!(
        service_roles::ROLE_GML & ROLE_HIGH_GM,
        0,
        "ROLE_GML must not satisfy /createccpevent ROLE_HIGH_GM gate"
    );
    assert_eq!(
        service_roles::ROLE_GMS & ROLE_STRUCTURE_GM,
        0,
        "ROLE_GMS must not satisfy /structure ROLE_STRUCTURE_GM gate"
    );
    assert_eq!(
        service_roles::ROLE_WORLDMOD & ROLE_STRUCTURE_GM,
        0,
        "ROLE_WORLDMOD must not satisfy /structure ROLE_STRUCTURE_GM gate"
    );
}
