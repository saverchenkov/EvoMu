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

    assert_eq!(PENDING_SLASH_HANDLER_COUNT, 0);
    assert_eq!(SLASH_COMMAND_REGISTRY.len(), all.len());
    for bound in [
        "achievements",
        "advancetime",
        "autotidi",
        "backintime",
        "ban",
        "campaign",
        "chatban",
        "chatunban",
        "cleartimers",
        "completeindustry",
        "completeskills",
        "contact",
        "createccpevent",
        "createitem",
        "deadwarp",
        "despawn",
        "downtime",
        "energy",
        "fastcheckout",
        "fastforward",
        "ff",
        "finishjobs",
        "finishskills",
        "finishstructure",
        "fit",
        "fwcapture",
        "getattr",
        "giveisk",
        "giveitem",
        "giveskill",
        "heal",
        "help",
        "industrytimer",
        "kick",
        "kill",
        "load",
        "location",
        "moveme",
        "online",
        "operation",
        "pd",
        "reinforce",
        "role",
        "season",
        "secstatus",
        "session",
        "setattr",
        "setomega",
        "setskill",
        "setunderage",
        "shutdown",
        "soak",
        "spawn",
        "spawnanomaly",
        "spawnnpc",
        "spawnwreck",
        "standing",
        "structure",
        "structuretimer",
        "survey",
        "suspend",
        "sysjunkclear",
        "teleport",
        "tidi",
        "tr",
        "trainskills",
        "translocate",
        "unban",
        "unspawn",
        "unsuspend",
        "upwelltimer",
        "whoami",
    ] {
        assert!(
            lookup_slash_entry(bound).is_some(),
            "Migrated command '/{bound}' must be registered in SLASH_COMMAND_REGISTRY"
        );
    }
    assert!(matches!(
        lookup_slash_entry("backintime").map(|e| e.handler),
        Some(SlashHandler::Sync(_))
    ));
    for async_b4 in ["deadwarp", "soak", "sysjunkclear"] {
        assert!(
            matches!(
                lookup_slash_entry(async_b4).map(|e| e.handler),
                Some(SlashHandler::Async(_))
            ),
            "Batch-4 command '/{async_b4}' must bind SlashHandler::Async"
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

    // Bound sync rows execute cleanly (including case-insensitive `cmd_icase!` heads and
    // Batch-4 `/backintime`); async Batch-4 rows yield RequiresAsync on the sync fast-path.
    let SlashDispatchOutcome::Handled(Ok(who_rep)) =
        dispatch_slash_command(&ctx, &sess, "slash", &[PyRep::String("/whoami".into())])
    else {
        panic!("expected Handled Ok for /whoami");
    };
    assert!(who_rep.as_str().is_some_and(|s| s.contains("charID=")));

    let SlashDispatchOutcome::Handled(Ok(back_rep)) =
        dispatch_slash_command(&ctx, &sess, "slash", &[PyRep::String("/backintime".into())])
    else {
        panic!("expected Handled Ok for sync /backintime");
    };
    assert_eq!(back_rep.dict_get("queueLength"), Some(&PyRep::Integer(0)));

    let SlashDispatchOutcome::Handled(Err(struct_err)) = dispatch_slash_command(
        &ctx,
        &sess,
        "slash",
        &[PyRep::String("/STRUCTURE state".into())],
    ) else {
        panic!("expected Handled Err for malformed /STRUCTURE state");
    };
    assert_eq!(struct_err.code, "InvalidSlashArgument"); // rpc-lint: allow MachoNet error code assertion

    for async_cmd in ["/soak logi status", "/deadwarp status", "/sysjunkclear"] {
        assert!(
            matches!(
                dispatch_slash_command(
                    &ctx,
                    &sess,
                    "slash",
                    &[PyRep::String(async_cmd.into())],
                ),
                SlashDispatchOutcome::RequiresAsync
            ),
            "expected RequiresAsync on sync fast-path for {async_cmd}"
        );
    }

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

    // All GM slash command parsing now routes through `slash_registry`; 0 legacy
    // leading-slash prefix chains or `strip_slash_command*` helpers remain.
    let shrink_only_max_by_file: BTreeMap<&str, usize> = BTreeMap::new();

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
    assert!(
        shrink_only_max_by_file.is_empty() && observed.is_empty(),
        "expected 0 legacy slash prefix parse sites remaining, got {observed:?}"
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

async fn exec_slash_async(
    ctx: &crate::router::ServerContext,
    sess: &mut eve_machonet::SessionState,
    svc: &str,
    cmd: &str,
) -> Result<PyRep, BridgeRpcError> {
    dispatch_slash_command_async(ctx, sess, svc, &[PyRep::String(cmd.into())]).await
}

#[tokio::test]
async fn slash_registry_async_and_account_admin_dispatch_parity() {
    let ctx = test_ctx();
    let demo_uid = ctx
        .db
        .with_snapshot(|s| s.accounts.values().next().map(|a| a.user_id))
        .expect("seeded account");
    let cid = 90_000_001_i64;
    let mut sess = auth_session(demo_uid, cid, 98_000_001, 30_000_142);

    ctx.db.with_mut_snapshot(|snap| {
        snap.accounts.insert(
            "slash_reg_target".into(),
            eve_db::AccountRecord {
                user_id: 77_701,
                username: "slash_reg_target".into(),
                password_token: String::new(),
                role: eve_db::service_roles::ROLE_PLAYER_DEFAULT,
                banned_until_unix: None,
                ban_reason: None,
            },
        );
    });

    // 1. /help and /help <topic>
    sess.role = service_roles::ROLE_GML;
    let help_all = exec_slash_async(&ctx, &mut sess, "slash", "/help")
        .await
        .expect("/help succeeds");
    assert!(help_all
        .as_str()
        .is_some_and(|s| s.contains("/giveisk") && s.contains("/heal")));
    let help_role_denied = exec_slash_async(&ctx, &mut sess, "slash", "/help role")
        .await
        .expect("/help role succeeds");
    assert!(help_role_denied
        .as_str()
        .is_some_and(|s| s.contains("/role: requires a higher role")));

    // 2. /giveisk sync + async dispatch and bare usage parity
    let giveisk_usage = exec_slash_async(&ctx, &mut sess, "slash", "/giveisk")
        .await
        .expect_err("bare /giveisk must return usage error");
    assert_eq!(giveisk_usage.code, "InvalidSlashArgument"); // rpc-lint: allow MachoNet error code assertion
    let SlashDispatchOutcome::Handled(Ok(sync_isk)) = dispatch_slash_command(
        &ctx,
        &sess,
        "slash",
        &[PyRep::String("/giveisk 250".into())],
    ) else {
        panic!("expected sync dispatch_slash_command Ok for /giveisk");
    };
    assert!(sync_isk.dict_get("newBalanceISK").is_some());

    // 3. Account admin commands (/ban, /unban, /suspend, /unsuspend, /role, /kick)
    sess.role = service_roles::ROLE_GMH
        | service_roles::ROLE_GMS
        | service_roles::ROLE_ADMIN
        | service_roles::ROLE_ROLEADMIN
        | service_roles::ROLE_SPAWN;

    // Sync fast-path leaves SyncMut (/role) and Async (/tr, /spawnnpc, /heal) to async dispatcher.
    assert!(matches!(
        dispatch_slash_command(
            &ctx,
            &sess,
            "slash",
            &[PyRep::String("/role slash_reg_target GML".into())],
        ),
        SlashDispatchOutcome::RequiresAsync
    ));
    assert!(matches!(
        dispatch_slash_command(
            &ctx,
            &sess,
            "slash",
            &[PyRep::String("/heal".into())],
        ),
        SlashDispatchOutcome::RequiresAsync
    ));

    // Bare commands that require trailing whitespace in legacy auth_cache still return UnknownSlashCommand.
    for bare in [
        "/ban",
        "/unban",
        "/suspend",
        "/unsuspend",
        "/role",
        "/kick",
        "/giveitem",
        "/createitem",
        "/spawnnpc",
        "/createccpevent",
    ] {
        let err = exec_slash_async(&ctx, &mut sess, "slash", bare)
            .await
            .expect_err("bare command without args must preserve UnknownSlashCommand");
        assert_eq!(err.code, "UnknownSlashCommand", "cmd={bare}: {err:?}"); // rpc-lint: allow MachoNet error code assertion
    }

    let ban_rep = exec_slash_async(&ctx, &mut sess, "slash", "/ban slash_reg_target 600 test")
        .await
        .expect("/ban ok");
    assert_eq!(ban_rep.dict_get("userID"), Some(&PyRep::Integer(77_701)));
    let unban_rep = exec_slash_async(&ctx, &mut sess, "slash", "/unban slash_reg_target")
        .await
        .expect("/unban ok");
    assert_eq!(unban_rep.dict_get("unbanned"), Some(&PyRep::Bool(true)));
    let susp_rep = exec_slash_async(&ctx, &mut sess, "slash", "/suspend slash_reg_target 300 wait")
        .await
        .expect("/suspend ok");
    assert_eq!(susp_rep.dict_get("userID"), Some(&PyRep::Integer(77_701)));
    let unsusp_rep = exec_slash_async(&ctx, &mut sess, "slash", "/unsuspend slash_reg_target")
        .await
        .expect("/unsuspend ok");
    assert_eq!(unsusp_rep.dict_get("unsuspended"), Some(&PyRep::Bool(true)));

    // /role updates target and mutates current session.role when targeting caller's own account.
    let role_rep = exec_slash_async(&ctx, &mut sess, "slash", "/role slash_reg_target GML")
        .await
        .expect("/role target ok");
    assert_eq!(
        role_rep.dict_get("role"),
        Some(&PyRep::Integer(
            (eve_db::service_roles::ROLE_PLAYER_DEFAULT | eve_db::service_roles::ROLE_GML) as i64
        ))
    );
    let kick_rep = exec_slash_async(&ctx, &mut sess, "slash", "/kick slash_reg_target bye")
        .await
        .expect("/kick ok");
    assert_eq!(kick_rep.dict_get("userID"), Some(&PyRep::Integer(77_701)));

    let saved_role = sess.role;
    let self_role_rep = exec_slash_async(
        &ctx,
        &mut sess,
        "slash",
        &format!("/role {demo_uid} GMH"),
    )
    .await
    .expect("/role self ok");
    assert_eq!(
        self_role_rep.dict_get("role"),
        Some(&PyRep::Integer(
            (eve_db::service_roles::ROLE_PLAYER_DEFAULT | eve_db::service_roles::ROLE_GMH) as i64
        ))
    );
    assert_eq!(
        sess.role,
        eve_db::service_roles::ROLE_PLAYER_DEFAULT | eve_db::service_roles::ROLE_GMH
    );
    sess.role = saved_role;
    ctx.db
        .set_account_role(&demo_uid.to_string(), saved_role)
        .expect("restore demo account role");

    // 4. /giveitem, /createitem alias, /setskill, /giveskill alias
    let item_rep = exec_slash_async(&ctx, &mut sess, "slash", "/giveitem 34 25")
        .await
        .expect("/giveitem ok");
    let item_id = item_rep.dict_get("itemID").and_then(PyRep::as_i64).expect("itemID");
    let ship_rep = exec_slash_async(&ctx, &mut sess, "slash", "/createitem 587 1")
        .await
        .expect("/createitem alias ok");
    let ship_item_id = ship_rep.dict_get("itemID").and_then(PyRep::as_i64).expect("ship itemID");
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items.get(&item_id).map(|i| i.quantity), Some(25));
        assert_eq!(snap.items.get(&ship_item_id).map(|i| i.singleton), Some(true));
    });

    let bare_skill_err = exec_slash_async(&ctx, &mut sess, "slash", "/setskill")
        .await
        .expect_err("bare /setskill returns usage error");
    assert_eq!(bare_skill_err.code, "InvalidSlashArgument"); // rpc-lint: allow MachoNet error code assertion
    let set_skill_rep = exec_slash_async(&ctx, &mut sess, "slash", "/setskill 3300 4")
        .await
        .expect("/setskill ok");
    assert_eq!(set_skill_rep.dict_get("level"), Some(&PyRep::Integer(4)));
    let give_skill_rep = exec_slash_async(&ctx, &mut sess, "slash", "/giveskill 3300 0")
        .await
        .expect("/giveskill alias ok");
    assert_eq!(give_skill_rep.dict_get("level"), Some(&PyRep::Integer(0)));

    // 5. Async space/gameplay commands: /spawnnpc, /tr, /teleport, /moveme, /translocate, /heal
    let npc_rep = exec_slash_async(&ctx, &mut sess, "slash", "/spawnnpc 587")
        .await
        .expect("/spawnnpc ok");
    assert!(npc_rep.dict_get("npcID").and_then(PyRep::as_i64).unwrap_or(0) > 0);

    let bare_tr_err = exec_slash_async(&ctx, &mut sess, "slash", "/tr")
        .await
        .expect_err("bare /tr returns usage error");
    assert_eq!(bare_tr_err.code, "InvalidSlashArgument"); // rpc-lint: allow MachoNet error code assertion

    let tr_rep = exec_slash_async(&ctx, &mut sess, "slash", "/tr me 30000144")
        .await
        .expect("/tr me 30000144 ok");
    assert_eq!(tr_rep.dict_get("solarSystemID"), Some(&PyRep::Integer(30_000_144)));
    assert_eq!(sess.solar_system_id, Some(30_000_144));

    let tp_rep = exec_slash_async(&ctx, &mut sess, "slash", "/teleport 30000142")
        .await
        .expect("/teleport 30000142 ok");
    assert_eq!(tp_rep.dict_get("solarSystemID"), Some(&PyRep::Integer(30_000_142)));

    // QA service parity: ROLE_QA can use /moveme and /translocate on publicQaToolsServer, not /tr.
    let mut qa_sess = auth_session(demo_uid, cid, 98_000_001, 30_000_142);
    qa_sess.role = service_roles::ROLE_QA;
    let qa_tr_err = exec_slash_async(&ctx, &mut qa_sess, "publicQaToolsServer", "/tr me 30000144")
        .await
        .expect_err("ROLE_QA must be denied /tr");
    assert_eq!(qa_tr_err.code, "InsufficientRole"); // rpc-lint: allow MachoNet error code assertion

    let qa_move_rep =
        exec_slash_async(&ctx, &mut qa_sess, "publicQaToolsServer", "/moveme 30000144")
            .await
            .expect("ROLE_QA /moveme ok on publicQaToolsServer");
    assert_eq!(
        qa_move_rep.dict_get("solarSystemID"),
        Some(&PyRep::Integer(30_000_144))
    );
    let qa_trans_rep = exec_slash_async(
        &ctx,
        &mut qa_sess,
        "publicQaToolsServer",
        "/translocate 30000142",
    )
    .await
    .expect("ROLE_QA /translocate ok on publicQaToolsServer");
    assert_eq!(
        qa_trans_rep.dict_get("solarSystemID"),
        Some(&PyRep::Integer(30_000_142))
    );

    let heal_rep = exec_slash_async(&ctx, &mut sess, "publicQaToolsServer", "/heal")
        .await
        .expect("/heal ok on publicQaToolsServer");
    assert_eq!(heal_rep.dict_get("healed"), Some(&PyRep::Bool(true)));

    // 6. /createccpevent ROLE_GML vs ROLE_GMS vs ROLE_GMH parity
    let future_ft = ((ctx.now_unix() as i64) + 11_644_473_600 + 3_600) * 10_000_000;
    let ccp_cmd = format!("/createccpevent {future_ft} 60 1 Registry Event | Body");

    let mut gml_sess = sess.clone();
    gml_sess.role = service_roles::ROLE_GML;
    let gml_err = exec_slash_async(&ctx, &mut gml_sess, "slash", &ccp_cmd)
        .await
        .expect_err("ROLE_GML must fail /createccpevent");
    assert_eq!(gml_err.code, "InsufficientRole"); // rpc-lint: allow MachoNet error code assertion

    let mut gms_sess = sess.clone();
    gms_sess.role = service_roles::ROLE_GMS;
    let gms_bare_err = exec_slash_async(&ctx, &mut gms_sess, "slash", "/createccpevent")
        .await
        .expect_err("bare /createccpevent with ROLE_GMS returns UnknownSlashCommand");
    assert_eq!(gms_bare_err.code, "UnknownSlashCommand"); // rpc-lint: allow MachoNet error code assertion
    let gms_args_err = exec_slash_async(&ctx, &mut gms_sess, "slash", &ccp_cmd)
        .await
        .expect_err("ROLE_GMS /createccpevent with args requires ROLE_GMH");
    assert_eq!(gms_args_err.code, "InsufficientRole"); // rpc-lint: allow MachoNet error code assertion

    let ccp_rep = exec_slash_async(&ctx, &mut sess, "slash", &ccp_cmd)
        .await
        .expect("ROLE_GMH /createccpevent ok");
    assert!(ccp_rep.dict_get("eventID").and_then(PyRep::as_i64).unwrap_or(0) > 0);
}

#[tokio::test]
async fn slash_registry_batch3_gm_tools_and_notify_emitters_parity() {
    let ctx = test_ctx();
    let demo_uid = ctx
        .db
        .with_snapshot(|s| s.accounts.values().next().map(|a| a.user_id))
        .expect("seeded account");
    let cid = 90_000_001_i64;
    let ship_id = ctx
        .db
        .with_snapshot(|s| s.characters.get(&cid).map(|c| c.active_ship_id))
        .unwrap_or(0);
    let mut sess = auth_session(demo_uid, cid, 98_000_001, 30_000_142);
    if ship_id > 0 {
        sess.ship_id = Some(ship_id);
    }
    sess.role = service_roles::ROLE_GM_MASK
        | service_roles::ROLE_SPAWN
        | service_roles::ROLE_QA
        | service_roles::ROLE_ADMIN;

    // 1. Derived role matches legacy auth_cache policy + case/whitespace gates.
    for cmd in [
        "/achievements reset", "/advancetime 60", "/autotidi 50", "/campaign enroll 1",
        "/chatban 90000001 60", "/chatunban 90000001", "/cleartimers", "/completeindustry",
        "/completeskills", "/contact 90000002 5.0", "/despawn 1", "/downtime", "/energy 100",
        "/fastcheckout 1", "/FastCheckout 1", "/fastforward 60", "/ff 60", "/finishjobs",
        "/finishskills", "/finishstructure", "/fit me 2048", "/fwcapture 30000142 100",
        "/getattr me 37", "/industrytimer", "/kill me", "/load me 2048 1", "/location",
        "/online", "/reinforce", "/season reset_goals", "/secstatus 2.0", "/session",
        "/setattr me 37 500.0", "/shutdown cancel", "/spawn 606 1", "/spawnanomaly",
        "/spawnwreck 606 1", "/standing 90000002 5.0", "/structure state 1001 110",
        "/STRUCTURE state 1001 110", "/structuretimer", "/tidi status", "/trainskills",
        "/unspawn 1", "/upwelltimer", "/whoami", "/WHOAMI",
    ] {
        assert_eq!(required_slash_role(cmd), auth_required_slash_role(cmd), "role mismatch for '{cmd}'");
    }
    for bad in [
        "/WHOAMI", "/FIT me 2048", "/LOAD me 2048 1", "/spawn", "/unspawn", "/despawn",
        "/setattr", "/getattr", "/fit", "/kill", "/contact", "/standing", "/energy",
        "/secstatus", "/achievements", "/campaign", "/shutdown",
    ] {
        let err = exec_slash_async(&ctx, &mut sess, "slash", bad)
            .await
            .expect_err("must return UnknownSlashCommand");
        assert_eq!(err.code, "UnknownSlashCommand", "{bad}"); // rpc-lint: allow MachoNet error code assertion
    }

    // 2. Narrower role gates: ROLE_QA, ROLE_SPAWN, ROLE_STRUCTURE_GM.
    let mut qa_sess = auth_session(demo_uid, cid, 98_000_001, 30_000_142);
    qa_sess.role = service_roles::ROLE_QA;
    for ok_cmd in ["/whoami", "/online", "/location"] {
        assert!(exec_slash_async(&ctx, &mut qa_sess, "publicQaToolsServer", ok_cmd).await.is_ok());
    }
    let qa_spawn_err = exec_slash_async(&ctx, &mut qa_sess, "publicQaToolsServer", "/spawn 606 1")
        .await
        .expect_err("ROLE_QA must be denied /spawn");
    assert_eq!(qa_spawn_err.code, "InsufficientRole"); // rpc-lint: allow MachoNet error code assertion

    let mut gms_sess = auth_session(demo_uid, cid, 98_000_001, 30_000_142);
    gms_sess.role = service_roles::ROLE_GMS;
    let struct_role_err = exec_slash_async(&ctx, &mut gms_sess, "slash", "/STRUCTURE state 1001 110")
        .await
        .expect_err("ROLE_GMS must be denied /STRUCTURE");
    assert_eq!(struct_role_err.code, "InsufficientRole"); // rpc-lint: allow MachoNet error code assertion

    // 3. End-to-end registry execution for Batch 3 character_gm_tools & notify_emitters handlers.
    let campaign_id = crate::services::campaigns_and_milestones::get_sde_login_campaigns(&ctx.sde)
        .keys()
        .next()
        .copied()
        .unwrap_or(1);
    let enroll_cmd = format!("/Campaign Enroll {campaign_id}");
    for ok_cmd in [
        "/setattr me 37 450.5", "/getattr me 37", "/fit me 2048", "/load me 2048 1",
        "/contact 90000002 6.0", "/standing 90000002 4.0", "/downtime", "/tidi status",
        "/reinforce", "/autotidi off", "/energy 500", "/secstatus 3.5", "/season reset_goals",
        "/session", "/Achievements Reset", "/ChatBan 90000002 60", "/ChatUnban 90000002",
        enroll_cmd.as_str(), "/Shutdown Cancel", "/FastCheckout 1", "/SpawnAnomaly",
        "/FwCapture 30000142 100", "/AdvanceTime 30", "/FastForward 15", "/FF 10",
        "/CompleteSkills", "/TrainSkills", "/FinishSkills", "/UpwellTimer",
        "/StructureTimer", "/FinishStructure", "/CompleteIndustry", "/FinishJobs",
        "/IndustryTimer", "/ClearTimers",
    ] {
        assert!(exec_slash_async(&ctx, &mut sess, "slash", ok_cmd).await.is_ok(), "{ok_cmd}");
    }

    let s1 = exec_slash_async(&ctx, &mut sess, "slash", "/spawn 606 1").await.expect("/spawn 1");
    let id1 = s1.dict_get("spawnedID").and_then(PyRep::as_i64).expect("spawnedID 1");
    assert!(exec_slash_async(&ctx, &mut sess, "slash", &format!("/despawn {id1}")).await.is_ok());

    let s2 = exec_slash_async(&ctx, &mut sess, "slash", "/spawn 606 1").await.expect("/spawn 2");
    let id2 = s2.dict_get("spawnedID").and_then(PyRep::as_i64).expect("spawnedID 2");
    let k2 = exec_slash_async(&ctx, &mut sess, "slash", &format!("/kill {id2}")).await.expect("/kill");
    assert_eq!(k2.dict_get("destroyed"), Some(&PyRep::Bool(true)));

    let miss = exec_slash_async(&ctx, &mut sess, "slash", "/unspawn 999999991")
        .await
        .expect_err("missing entity returns EntityNotFound");
    assert_eq!(miss.code, "EntityNotFound"); // rpc-lint: allow MachoNet error code assertion

    let sw_err = exec_slash_async(&ctx, &mut sess, "slash", "/spawnwreck")
        .await
        .expect_err("bare /spawnwreck returns InvalidArguments");
    assert_eq!(sw_err.code, "InvalidArguments"); // rpc-lint: allow MachoNet error code assertion
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.station_id = None;
            ch.structure_id = None;
        }
    });
    if ship_id > 0 {
        let _ = ctx
            .systems
            .get_or_spawn(30_000_142)
            .spawn_ball_with_combat(
                eve_destiny::Ball {
                    id: ship_id,
                    type_id: 606,
                    position: glam::DVec3::ZERO,
                    velocity: glam::DVec3::ZERO,
                    max_velocity: 300.0,
                    inertia: 1.0,
                    mass: 1_000_000.0,
                    radius: 30.0,
                    speed_fraction: 0.0,
                    mode: eve_destiny::MovementMode::Stop,
                },
                eve_destiny::CombatState::default(),
            )
            .await;
    }
    assert!(exec_slash_async(&ctx, &mut sess, "slash", "/spawnwreck 606 1").await.is_ok());
}

#[tokio::test]
async fn slash_registry_batch4_gm_soak_commands_final_cutover_parity() {
    let ctx = test_ctx();
    let demo_uid = ctx
        .db
        .with_snapshot(|s| s.accounts.values().next().map(|a| a.user_id))
        .expect("seeded account");
    let cid = 90_000_001_i64;
    let sys_id = 30_000_142_u32;
    let ship_id = ctx
        .db
        .with_snapshot(|s| s.characters.get(&cid).map(|c| c.active_ship_id))
        .filter(|&id| id > 0)
        .unwrap_or(100_000_001);
    let target_race = ctx
        .sde
        .races
        .iter()
        .find(|(_, r)| r.starter_skills.values().any(|&lvl| lvl > 0))
        .map(|(&rid, _)| rid)
        .or_else(|| ctx.sde.races.keys().next().copied())
        .unwrap_or(1);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.race_id = target_race;
            ch.station_id = None;
            ch.structure_id = None;
            ch.solar_system_id = sys_id;
            ch.active_ship_id = ship_id;
        }
        snap.skills.entry(cid).or_default().insert(
            99_999_999,
            eve_db::TrainedSkillRecord {
                type_id: 99_999_999,
                level: 5,
                skill_points: 256_000.0,
            },
        );
        snap.skill_queues.insert(
            cid,
            vec![eve_db::SkillQueueEntry {
                type_id: 3300,
                target_level: 5,
                start_sp: 0.0,
                end_sp: 1_000.0,
                duration_seconds: 60.0,
                started_at_unix: None,
            }],
        );
    });

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_handle
        .spawn_ball_with_combat(
            eve_destiny::Ball {
                id: ship_id,
                type_id: 606,
                position: glam::DVec3::ZERO,
                velocity: glam::DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 30.0,
                speed_fraction: 0.0,
                mode: eve_destiny::MovementMode::Stop,
            },
            eve_destiny::CombatState::default(),
        )
        .await;

    // 1. 72/72 mandatory handlers & exact-case role parity.
    assert_eq!(PENDING_SLASH_HANDLER_COUNT, 0);
    assert_eq!(SLASH_COMMAND_REGISTRY.len(), 72);
    assert_eq!(all_remote_slash_commands().len(), 72);
    for cmd in [
        "/backintime",
        "/backintime me",
        "/deadwarp",
        "/deadwarp status",
        "/deadwarp stop",
        "/soak logi status",
        "/soak logi 2",
        "/sysjunkclear",
    ] {
        assert_eq!(required_slash_role(cmd), service_roles::ROLE_GM_MASK, "{cmd}");
        assert_eq!(required_slash_role(cmd), auth_required_slash_role(cmd), "{cmd}");
    }

    // 2. Role gate rejection on both slash and publicQaToolsServer without mutating state.
    for role in [service_roles::ROLE_PLAYER, service_roles::ROLE_QA] {
        let mut denied_sess = auth_session(demo_uid, cid, 98_000_001, sys_id);
        denied_sess.ship_id = Some(ship_id);
        denied_sess.role = role;
        for svc in ["slash", "publicQaToolsServer"] {
            for cmd in ["/backintime", "/deadwarp status", "/soak logi status", "/sysjunkclear"] {
                let err = exec_slash_async(&ctx, &mut denied_sess, svc, cmd)
                    .await
                    .expect_err("unprivileged caller must be rejected");
                assert_eq!(err.code, "InsufficientRole", "{svc} {cmd}"); // rpc-lint: allow MachoNet error code assertion
            }
        }
    }
    ctx.db.with_snapshot(|snap| {
        assert!(snap.skills.get(&cid).is_some_and(|m| m.contains_key(&99_999_999)));
        assert!(snap.skill_queues.contains_key(&cid));
    });

    // 3. Exact-case enforcement & prefix collision rejection.
    let mut sess = auth_session(demo_uid, cid, 98_000_001, sys_id);
    sess.ship_id = Some(ship_id);
    sess.role = service_roles::ROLE_GM_MASK;
    for bad in [
        "/BACKINTIME",
        "/BackInTime",
        "/backintime2",
        "/DEADWARP status",
        "/deadwarpextra",
        "/SOAK logi status",
        "/soakextra logi status",
        "/SYSJUNKCLEAR",
        "/sysjunkclearall",
        "/SEASON reset_goals",
        "/seasonextra",
    ] {
        let err = exec_slash_async(&ctx, &mut sess, "slash", bad)
            .await
            .expect_err("uppercase or prefix-colliding command must fail");
        assert_eq!(err.code, "UnknownSlashCommand", "{bad}"); // rpc-lint: allow MachoNet error code assertion
    }

    // 4. Whitespace handling, argument forwarding, and end-to-end execution for all 4 commands.
    let back_bad = exec_slash_async(&ctx, &mut sess, "slash", "/backintime extra")
        .await
        .expect_err("extra arg to /backintime must fail");
    assert_eq!(back_bad.code, "InvalidArguments"); // rpc-lint: allow MachoNet error code assertion

    let back_rep = exec_slash_async(&ctx, &mut sess, "slash", "  /backintime   me  ")
        .await
        .expect("/backintime me succeeds with whitespace");
    assert_eq!(back_rep.dict_get("command"), Some(&PyRep::String("/backintime   me".into())));
    assert_eq!(back_rep.dict_get("characterID"), Some(&PyRep::Integer(cid)));
    assert_eq!(back_rep.dict_get("queueLength"), Some(&PyRep::Integer(0)));
    assert!(
        back_rep
            .dict_get("removedSkillCount")
            .and_then(PyRep::as_i64)
            .unwrap_or(0)
            >= 1
    );
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.skills.get(&cid).is_some_and(|m| m.contains_key(&99_999_999)));
        assert!(!snap.skill_queues.contains_key(&cid));
    });

    // /soak status, spawn ring, stop, and argument errors.
    let soak_bare = exec_slash_async(&ctx, &mut sess, "slash", "/soak")
        .await
        .expect_err("bare /soak returns InvalidArguments");
    assert_eq!(soak_bare.code, "InvalidArguments"); // rpc-lint: allow MachoNet error code assertion
    let soak_status = exec_slash_async(&ctx, &mut sess, "slash", "  /soak   logi   status  ")
        .await
        .expect("/soak logi status succeeds");
    assert_eq!(
        soak_status.dict_get("command"),
        Some(&PyRep::String("/soak   logi   status".into()))
    );
    assert_eq!(soak_status.dict_get("active"), Some(&PyRep::Bool(false)));
    let soak_spawn = exec_slash_async(&ctx, &mut sess, "slash", "/soak logi 2")
        .await
        .expect("/soak logi 2 spawns ring");
    assert_eq!(soak_spawn.dict_get("active"), Some(&PyRep::Bool(true)));
    assert_eq!(soak_spawn.dict_get("npcCount"), Some(&PyRep::Integer(2)));
    let soak_stop = exec_slash_async(&ctx, &mut sess, "publicQaToolsServer", "/soak   stop")
        .await
        .expect("/soak stop succeeds");
    assert_eq!(soak_stop.dict_get("active"), Some(&PyRep::Bool(false)));
    assert_eq!(soak_stop.dict_get("stoppedCount"), Some(&PyRep::Integer(2)));

    // /deadwarp status, start, stop, and argument errors.
    let dw_bad = exec_slash_async(&ctx, &mut sess, "slash", "/deadwarp bogus")
        .await
        .expect_err("/deadwarp bogus fails");
    assert_eq!(dw_bad.code, "InvalidArguments"); // rpc-lint: allow MachoNet error code assertion
    let dw_idle = exec_slash_async(&ctx, &mut sess, "slash", "  /deadwarp   status ")
        .await
        .expect("/deadwarp status idle");
    assert_eq!(dw_idle.dict_get("command"), Some(&PyRep::String("/deadwarp   status".into())));
    assert_eq!(dw_idle.dict_get("active"), Some(&PyRep::Bool(false)));
    let dw_start = exec_slash_async(&ctx, &mut sess, "slash", "/deadwarp")
        .await
        .expect("/deadwarp starts outbound warp");
    assert_eq!(dw_start.dict_get("active"), Some(&PyRep::Bool(true)));
    assert_eq!(dw_start.dict_get("phase"), Some(&PyRep::String("Outbound".into())));
    let dw_stop = exec_slash_async(&ctx, &mut sess, "slash", "/deadwarp stop")
        .await
        .expect("/deadwarp stop cancels");
    assert_eq!(dw_stop.dict_get("active"), Some(&PyRep::Bool(false)));
    assert_eq!(dw_stop.dict_get("stopped"), Some(&PyRep::Bool(true)));

    // /sysjunkclear argument check, live jetcan purge, and /season whitespace parity.
    let sj_bad = exec_slash_async(&ctx, &mut sess, "slash", "/sysjunkclear extra")
        .await
        .expect_err("/sysjunkclear extra fails");
    assert_eq!(sj_bad.code, "InvalidArguments"); // rpc-lint: allow MachoNet error code assertion
    let jetcan_id = 880_999_113_i64;
    sys_handle
        .spawn_ball_with_combat(
            eve_destiny::Ball {
                id: jetcan_id,
                type_id: 23,
                position: glam::DVec3::new(2_500.0, 0.0, 0.0),
                velocity: glam::DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 1.0,
                mass: 1_000.0,
                radius: 20.0,
                speed_fraction: 0.0,
                mode: eve_destiny::MovementMode::Stop,
            },
            eve_destiny::CombatState::default(),
        )
        .await
        .expect("spawn test jetcan");
    let sj_ok = exec_slash_async(&ctx, &mut sess, "slash", "  /sysjunkclear  ")
        .await
        .expect("/sysjunkclear succeeds");
    assert_eq!(sj_ok.dict_get("command"), Some(&PyRep::String("/sysjunkclear".into())));
    assert_eq!(sj_ok.dict_get("jetcansPurged"), Some(&PyRep::Integer(1)));
    assert_eq!(sj_ok.dict_get("totalPurged"), Some(&PyRep::Integer(1)));
    assert!(sys_handle.view().ball(jetcan_id).is_none());

    let season_rep = exec_slash_async(&ctx, &mut sess, "slash", "  /season   reset_goals  ")
        .await
        .expect("/season reset_goals succeeds with whitespace");
    assert_eq!(season_rep.dict_get("reset"), Some(&PyRep::Bool(true)));
}

#[test]
fn typed_giveisk_and_setskill_argument_parsers_enforce_contract() {
    let caller_cid = 90_000_001_i64;

    // GiveIskArgs valid forms
    assert_eq!(
        GiveIskArgs::parse(caller_cid, "2500").expect("plain amount"),
        GiveIskArgs {
            target_char_id: caller_cid,
            amount_cents: eve_db::IskCents::from_isk(2_500),
        }
    );
    assert_eq!(
        GiveIskArgs::parse(caller_cid, "  me   1000  ").expect("me target"),
        GiveIskArgs {
            target_char_id: caller_cid,
            amount_cents: eve_db::IskCents::from_isk(1_000),
        }
    );
    assert_eq!(
        GiveIskArgs::parse(caller_cid, "SELF 42").expect("self target"),
        GiveIskArgs {
            target_char_id: caller_cid,
            amount_cents: eve_db::IskCents::from_isk(42),
        }
    );
    assert_eq!(
        GiveIskArgs::parse(caller_cid, "90000002 500").expect("explicit char id"),
        GiveIskArgs {
            target_char_id: 90_000_002,
            amount_cents: eve_db::IskCents::from_isk(500),
        }
    );

    for bad in [
        "",
        "   ",
        "me",
        "0 100",
        "-1 100",
        "badtarget 100",
        "100 200 300",
        "10.5",
        "notanumber",
        "0",
        "-5",
        "me -5",
    ] {
        let err = GiveIskArgs::parse(caller_cid, bad)
            .expect_err(&format!("expected GiveIskArgs::parse({bad:?}) to reject"));
        assert_eq!(err.code, "InvalidSlashArgument", "for {bad:?}");
        assert!(matches!(
            err.client_error.as_deref(),
            Some(ClientError::User {
                key: UserErrorKey::SlashError,
                ..
            })
        ));
    }

    let overflow_err = GiveIskArgs::parse(caller_cid, "922337203685477580")
        .expect_err("cent multiplication overflow must reject");
    assert_eq!(overflow_err.code, "BalanceOverflow");
    assert!(matches!(
        overflow_err.client_error.as_deref(),
        Some(ClientError::User {
            key: UserErrorKey::SlashError,
            ..
        })
    ));

    // SetSkillArgs valid forms
    assert_eq!(
        SetSkillArgs::parse(caller_cid, "3300 5").expect("plain skill"),
        SetSkillArgs {
            target_char_id: caller_cid,
            skill_type_id: 3300,
            level: 5,
        }
    );
    assert_eq!(
        SetSkillArgs::parse(caller_cid, " me 3300 0 ").expect("me level 0"),
        SetSkillArgs {
            target_char_id: caller_cid,
            skill_type_id: 3300,
            level: 0,
        }
    );
    assert_eq!(
        SetSkillArgs::parse(caller_cid, "SELF 3333 3").expect("self level 3"),
        SetSkillArgs {
            target_char_id: caller_cid,
            skill_type_id: 3333,
            level: 3,
        }
    );
    assert_eq!(
        SetSkillArgs::parse(caller_cid, "90000002 3300 4").expect("explicit target"),
        SetSkillArgs {
            target_char_id: 90_000_002,
            skill_type_id: 3300,
            level: 4,
        }
    );

    for bad in [
        "",
        "3300",
        "0 3300 3",
        "-7 3300 3",
        "badtarget 3300 3",
        "3300 3 extra",
        "0 3",
        "notanid 3",
        "3300 -1",
        "3300 6",
        "3300 five",
    ] {
        let err = SetSkillArgs::parse(caller_cid, bad)
            .expect_err(&format!("expected SetSkillArgs::parse({bad:?}) to reject"));
        assert_eq!(err.code, "InvalidSlashArgument", "for {bad:?}");
        assert!(matches!(
            err.client_error.as_deref(),
            Some(ClientError::User {
                key: UserErrorKey::SlashError,
                ..
            })
        ));
    }
}

#[test]
fn track2_wallet_and_skill_domain_parity_source_guard() {
    let src_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");

    // 1. No `domain/gm_*.rs` shadow domain files exist; shared domain ops must live in real domains.
    let domain_entries = fs::read_dir(src_root.join("domain")).expect("read src/domain");
    for entry in domain_entries.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        assert!(
            !name_str.starts_with("gm_"),
            "forbidden GM-only domain module: domain/{name_str}"
        );
    }

    // 2. `auth_cache.rs` delegates `/giveisk` and `/giveskill`/`/setskill` to shared domain ops
    //    and retains zero direct wallet balance or trained-skill table mutations.
    let auth_raw = fs::read_to_string(src_root.join("services/auth_cache.rs")).expect("auth_cache.rs");
    let auth_prod = production_prefix(&auth_raw);
    for forbidden in [
        "reconcile_skill_queue_after_set_skill",
        "ch.balance =",
        "snap.skills",
        "skills_map.insert",
        "skills_map.remove",
        "ch.skill_points =",
    ] {
        assert!(
            !auth_prod.contains(forbidden),
            "services/auth_cache.rs production code must not contain raw table edit `{forbidden}`"
        );
    }
    assert!(
        auth_prod.contains("credit_character_reward_tx("),
        "dispatch_giveisk_slash must route through domain::career_goals::credit_character_reward_tx"
    );
    assert!(
        auth_prod.contains("set_character_skill_level("),
        "dispatch_setskill_slash must route through domain::skills::set_character_skill_level"
    );

    // 3. Shared domain functions have genuine non-GM gameplay callers.
    for rel in [
        "services/quasar_daily_goals.rs",
        "services/quasar_career_goals.rs",
    ] {
        let raw = fs::read_to_string(src_root.join(rel)).expect(rel);
        assert!(
            production_prefix(&raw).contains("credit_character_reward_tx("),
            "{rel} must call shared domain::career_goals::credit_character_reward_tx"
        );
    }

    let char_skills_raw =
        fs::read_to_string(src_root.join("services/character_skills.rs")).expect("character_skills.rs");
    let char_skills_prod = production_prefix(&char_skills_raw);
    assert!(
        char_skills_prod.contains("apply_free_skill_points_to_skill("),
        "character_skills.rs must call domain::skills::apply_free_skill_points_to_skill"
    );
    assert!(
        char_skills_prod.contains("apply_character_skill_update_in_snap("),
        "character_skills.rs must call domain::skills::apply_character_skill_update_in_snap"
    );

    for rel in [
        "services/character_skills_helpers.rs",
        "services/character_skill_ops.rs",
    ] {
        let raw = fs::read_to_string(src_root.join(rel)).expect(rel);
        assert!(
            production_prefix(&raw).contains("apply_character_skill_update_in_snap("),
            "{rel} must call shared domain::skills::apply_character_skill_update_in_snap"
        );
    }
}

#[test]
fn typed_giveitem_argument_parser_enforces_contract() {
    let caller_cid = 90_000_001_i64;
    let other_cid = 90_000_002_i64;
    let ctx = test_ctx();

    assert_eq!(
        GiveItemArgs::parse(caller_cid, "34").expect("single typeID"),
        GiveItemArgs::Explicit {
            target_char_id: caller_cid,
            type_id: 34,
            quantity: 1,
        }
    );
    assert_eq!(
        GiveItemArgs::parse(caller_cid, " me 34 ").expect("me typeID"),
        GiveItemArgs::Explicit {
            target_char_id: caller_cid,
            type_id: 34,
            quantity: 1,
        }
    );
    assert_eq!(
        GiveItemArgs::parse(caller_cid, "SELF 587").expect("self ship"),
        GiveItemArgs::Explicit {
            target_char_id: caller_cid,
            type_id: 587,
            quantity: 1,
        }
    );
    assert_eq!(
        GiveItemArgs::parse(caller_cid, "90000002 34 50").expect("explicit target qty"),
        GiveItemArgs::Explicit {
            target_char_id: other_cid,
            type_id: 34,
            quantity: 50,
        }
    );
    assert_eq!(
        GiveItemArgs::parse(caller_cid, "ME 484 2").expect("me typeID qty"),
        GiveItemArgs::Explicit {
            target_char_id: caller_cid,
            type_id: 484,
            quantity: 2,
        }
    );

    assert_eq!(
        GiveItemArgs::parse(caller_cid, "34 50")
            .unwrap()
            .resolve(&ctx.sde, |id| id == other_cid)
            .expect("typeID qty"),
        ResolvedGiveItemArgs {
            target_char_id: caller_cid,
            type_id: 34,
            quantity: 50,
        }
    );
    assert_eq!(
        GiveItemArgs::parse(caller_cid, "90000002 34")
            .unwrap()
            .resolve(&ctx.sde, |id| id == other_cid)
            .expect("charID typeID"),
        ResolvedGiveItemArgs {
            target_char_id: other_cid,
            type_id: 34,
            quantity: 1,
        }
    );
    assert_eq!(
        GiveItemArgs::parse(caller_cid, "999999999 1")
            .unwrap()
            .resolve(&ctx.sde, |id| id == other_cid)
            .expect("unknown typeID with non-type qty"),
        ResolvedGiveItemArgs {
            target_char_id: caller_cid,
            type_id: 999_999_999,
            quantity: 1,
        }
    );
    let missing_char = GiveItemArgs::parse(caller_cid, "999888777 34")
        .unwrap()
        .resolve(&ctx.sde, |id| id == other_cid)
        .expect_err("unknown charID with valid SDE typeID must fail");
    assert_eq!(missing_char.code, "CharacterNotFound");

    for bad in [
        "",
        "   ",
        "0",
        "-5",
        "notatype",
        "badtarget 34",
        "badtarget 34 5",
        "0 34 5",
        "-1 34 5",
        "90000002 0 5",
        "90000002 badtype 5",
        "90000002 34 0",
        "90000002 34 -3",
        "34 0",
        "34 -2",
        "me 0",
        "me badtype",
        "1 2 3 4",
    ] {
        let err = GiveItemArgs::parse(caller_cid, bad)
            .expect_err(&format!("expected GiveItemArgs::parse({bad:?}) to reject"));
        assert_eq!(err.code, "InvalidSlashArgument", "for {bad:?}");
        assert!(matches!(
            err.client_error.as_deref(),
            Some(ClientError::User {
                key: UserErrorKey::SlashError,
                ..
            })
        ));
    }
}

#[test]
fn track2_items_domain_parity_source_guard() {
    let src_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");

    // 1. `auth_cache.rs` routes `/giveitem` and `/createitem` through `domain::items`
    //    with zero raw `snap.items.insert` or `InventoryItemRecord` construction.
    let auth_raw = fs::read_to_string(src_root.join("services/auth_cache.rs")).expect("auth_cache.rs");
    let auth_prod = production_prefix(&auth_raw);
    for forbidden in [
        "handle_slash_give_item",
        "snap.items.insert",
        "InventoryItemRecord",
    ] {
        assert!(
            !auth_prod.contains(forbidden),
            "services/auth_cache.rs production code must not contain `{forbidden}`"
        );
    }
    assert!(
        auth_prod.contains("grant_character_item_tx("),
        "dispatch_giveitem_slash must route through domain::items::grant_character_item_tx"
    );

    // 2. `domain/items.rs` delegates storage to `eve_db::WorldStateSnapshot::mint_inventory_item_with_id_in_snap`
    //    and relies on commit ChangeSet delta projection without hand-written notifications.
    let items_raw = fs::read_to_string(src_root.join("domain/items.rs")).expect("domain/items.rs");
    let items_prod = production_prefix(&items_raw);
    assert!(items_prod.contains("mint_inventory_item_with_id_in_snap("));
    assert!(!items_prod.contains("items.insert("));
    assert!(!items_prod.contains("emit_"));
    assert!(!items_prod.contains("notify_"));

    // 3. Real non-GM gameplay item creation flows share `domain::items::grant_item_in_snap`.
    for rel in [
        "services/account_services.rs",
        "services/crate_loot_service.rs",
        "services/project_discovery_rewards.rs",
        "services/operations_manager_helpers.rs",
        "services/campaigns_and_milestones.rs",
        "services/season_manager.rs",
        "services/lp_store.rs",
        "services/notify_emitters_skills_trade_godma.rs",
    ] {
        let raw = fs::read_to_string(src_root.join(rel)).expect(rel);
        let prod = production_prefix(&raw);
        assert!(
            prod.contains("grant_item_in_snap("),
            "{rel} must call shared domain::items::grant_item_in_snap"
        );
        assert!(
            !prod.contains("snap.items.insert"),
            "{rel} production code must not insert raw rows into snap.items directly"
        );
    }
}

#[test]
fn typed_heal_and_kill_argument_parsers_enforce_contract() {
    assert_eq!(
        parse_heal_slash_args("").expect("empty /heal"),
        HealSlashArgs {
            target: HealSlashTarget::SelfShip,
        }
    );
    for alias in ["  ", "me", "ME", "myship", "self", "  \"me\"  "] {
        assert_eq!(
            parse_heal_slash_args(alias).expect("self alias /heal"),
            HealSlashArgs {
                target: HealSlashTarget::SelfShip,
            },
            "for {alias:?}"
        );
    }
    assert_eq!(
        parse_heal_slash_args(" 90000002 ").expect("explicit ID /heal"),
        HealSlashArgs {
            target: HealSlashTarget::EntityOrCharId(90_000_002),
        }
    );
    for bad in ["0", "-5", "badtarget", "me extra", "90000002 1", "\"\""] {
        let err = parse_heal_slash_args(bad).expect_err("bad /heal arg must fail");
        assert_eq!(err.code, "InvalidSlashArgument", "for /heal {bad:?}");
        assert!(matches!(
            err.client_error.as_deref(),
            Some(ClientError::User {
                key: UserErrorKey::SlashError,
                ..
            })
        ));
    }

    for alias in ["me", "MYSHIP", "self", " \"self\" "] {
        assert_eq!(
            parse_kill_slash_args(alias).expect("self alias /kill"),
            KillSlashArgs {
                target: KillSlashTarget::SelfChar,
            },
            "for {alias:?}"
        );
    }
    assert_eq!(
        parse_kill_slash_args(" 950001 ").expect("explicit ID /kill"),
        KillSlashArgs {
            target: KillSlashTarget::EntityOrCharId(950_001),
        }
    );
    for bad in ["", "   ", "0", "-1", "badtarget", "me extra", "950001 2", "\"\""] {
        let err = parse_kill_slash_args(bad).expect_err("bad /kill arg must fail");
        assert_eq!(err.code, "InvalidSlashArgument", "for /kill {bad:?}");
        assert!(matches!(
            err.client_error.as_deref(),
            Some(ClientError::User {
                key: UserErrorKey::SlashError,
                ..
            })
        ));
    }
}

#[test]
fn typed_teleport_argument_parser_enforces_contract() {
    assert_eq!(
        parse_teleport_slash_args("30000144").expect("[loc]"),
        TeleportSlashArgs {
            target: TeleportSlashTarget::SelfChar,
            location_id: 30_000_144,
            noblock: false,
        }
    );
    for alias in ["me", "ME", "myship", " \"MYSHIP\" "] {
        assert_eq!(
            parse_teleport_slash_args(&format!("{alias} 30000144")).expect("[me, loc]"),
            TeleportSlashArgs {
                target: TeleportSlashTarget::SelfChar,
                location_id: 30_000_144,
                noblock: false,
            },
            "for [{alias}, loc]"
        );
        assert_eq!(
            parse_teleport_slash_args(&format!("60003760 {alias}")).expect("[loc, me]"),
            TeleportSlashArgs {
                target: TeleportSlashTarget::SelfChar,
                location_id: 60_003_760,
                noblock: false,
            },
            "for [loc, {alias}]"
        );
    }
    assert_eq!(
        parse_teleport_slash_args("90000002 30000142").expect("[char_id, loc]"),
        TeleportSlashArgs {
            target: TeleportSlashTarget::CharId(90_000_002),
            location_id: 30_000_142,
            noblock: false,
        }
    );

    // `noblock` stripping (case-insensitive) across all valid argument shapes
    assert_eq!(
        parse_teleport_slash_args("30000144 noblock").expect("[loc, noblock]"),
        TeleportSlashArgs {
            target: TeleportSlashTarget::SelfChar,
            location_id: 30_000_144,
            noblock: true,
        }
    );
    assert_eq!(
        parse_teleport_slash_args("me 30000144 NOBLOCK").expect("[me, loc, noblock]"),
        TeleportSlashArgs {
            target: TeleportSlashTarget::SelfChar,
            location_id: 30_000_144,
            noblock: true,
        }
    );
    assert_eq!(
        parse_teleport_slash_args("60003760 myship noblock").expect("[loc, myship, noblock]"),
        TeleportSlashArgs {
            target: TeleportSlashTarget::SelfChar,
            location_id: 60_003_760,
            noblock: true,
        }
    );
    assert_eq!(
        parse_teleport_slash_args("90000002 30000142 noblock").expect("[char_id, loc, noblock]"),
        TeleportSlashArgs {
            target: TeleportSlashTarget::CharId(90_000_002),
            location_id: 30_000_142,
            noblock: true,
        }
    );

    for bad in [
        "",
        "   ",
        "noblock",
        "\"\"",
        "0",
        "-5",
        "badloc",
        "me 0",
        "0 me",
        "me me",
        "me -10",
        "self 30000144",
        "90000002 0",
        "0 30000144",
        "90000002 badloc",
        "me 30000144 extra",
        "90000002 30000144 extra",
    ] {
        let err = parse_teleport_slash_args(bad).expect_err("bad teleport arg must fail");
        assert_eq!(err.code, "InvalidSlashArgument", "for /tr {bad:?}");
        assert!(matches!(
            err.client_error.as_deref(),
            Some(ClientError::User {
                key: UserErrorKey::SlashError,
                ..
            })
        ));
    }
}

#[test]
fn track2_combat_domain_parity_source_guard() {
    let src_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");

    let helpers_raw =
        fs::read_to_string(src_root.join("services/character_gm_helpers.rs")).expect("helpers");
    let helpers_prod = production_prefix(&helpers_raw);
    assert!(helpers_prod.contains("restore_ship_vital_state("));
    for forbidden in ["get_or_spawn", "snap.items.values_mut", "heal_combat_state("] {
        assert!(
            !helpers_prod.contains(forbidden),
            "character_gm_helpers.rs must not contain `{forbidden}`"
        );
    }

    let tools_raw =
        fs::read_to_string(src_root.join("services/character_gm_tools.rs")).expect("tools");
    let tools_prod = production_prefix(&tools_raw);
    let kill_start = tools_prod
        .find("pub(crate) fn dispatch_kill_slash")
        .expect("dispatch_kill_slash");
    let kill_end = tools_prod[kill_start..]
        .find("pub(crate) fn dispatch_location_slash")
        .map(|off| kill_start + off)
        .expect("dispatch_location_slash");
    let kill_fn = &tools_prod[kill_start..kill_end];
    assert!(kill_fn.contains("destroy_entity_in_system("));
    for forbidden in ["get_or_spawn", "execute_death_pipeline(", "snap.items.insert", "10_000.0"] {
        assert!(
            !kill_fn.contains(forbidden),
            "dispatch_kill_slash must not contain `{forbidden}`"
        );
    }

    let combat_ops_raw =
        fs::read_to_string(src_root.join("domain/combat_ops.rs")).expect("domain/combat_ops.rs");
    let combat_ops_prod = production_prefix(&combat_ops_raw);
    assert!(!combat_ops_prod.contains("get_or_spawn"));
    assert!(!combat_ops_prod.contains("items.insert("));

    let repair_raw =
        fs::read_to_string(src_root.join("services/repair_service.rs")).expect("repair_service");
    let repair_prod = production_prefix(&repair_raw);
    assert!(repair_prod.contains("clear_ship_and_fitted_damage_in_snap("));
    assert!(repair_prod.contains("clear_fitted_module_damage_in_cache("));
}

#[test]
fn track2_transition_domain_parity_source_guard() {
    let src_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");

    let auth_raw =
        fs::read_to_string(src_root.join("services/auth_cache.rs")).expect("services/auth_cache.rs");
    let auth_prod = production_prefix(&auth_raw);
    let tp_start = auth_prod
        .find("async fn handle_slash_teleport(")
        .expect("handle_slash_teleport");
    let tp_tail = &auth_prod[tp_start..];
    assert!(tp_tail.contains("parse_teleport_slash_args("));
    assert!(tp_tail.contains("relocate_character_in_space("));
    for forbidden in [
        "get_or_spawn",
        "take_ball(",
        "leave_space_in_snap(",
        "set_character_solar_system(",
        "execute_session_transition_sync(",
    ] {
        assert!(
            !tp_tail.contains(forbidden),
            "handle_slash_teleport must not contain `{forbidden}`"
        );
    }

    let shipcaster_raw = fs::read_to_string(src_root.join("services/shipcaster_ops_starter.rs"))
        .expect("services/shipcaster_ops_starter.rs");
    let shipcaster_prod = production_prefix(&shipcaster_raw);
    let sc_start = shipcaster_prod
        .find("pub async fn execute_shipcaster_jump(")
        .expect("execute_shipcaster_jump");
    let sc_fn = &shipcaster_prod[sc_start..];
    assert!(sc_fn.contains("relocate_character_in_space("));
    for forbidden in [
        "get_or_spawn",
        "take_ball(",
        "restore_ball(",
        "leave_space_in_snap(",
        "set_character_solar_system(",
        "execute_session_transition_sync(",
    ] {
        assert!(
            !sc_fn.contains(forbidden),
            "execute_shipcaster_jump must not contain `{forbidden}`"
        );
    }

    let cyno_raw = fs::read_to_string(src_root.join("services/space_cyno_filaments.rs"))
        .expect("services/space_cyno_filaments.rs");
    let cyno_prod = production_prefix(&cyno_raw);
    let wh_start = cyno_prod
        .find("async fn execute_legacy_wormhole_jump(")
        .expect("execute_legacy_wormhole_jump");
    let wh_end = cyno_prod[wh_start..]
        .find("async fn handle_random_jump_filament(")
        .map(|off| wh_start + off)
        .expect("end of execute_legacy_wormhole_jump");
    let wh_fn = &cyno_prod[wh_start..wh_end];
    assert!(wh_fn.contains("relocate_character_in_space("));
    for forbidden in [
        "get_or_spawn",
        "take_ball(",
        "restore_ball(",
        "leave_space_in_snap(",
        "set_character_solar_system(",
        "execute_session_transition_sync(",
    ] {
        assert!(
            !wh_fn.contains(forbidden),
            "execute_legacy_wormhole_jump must not contain `{forbidden}`"
        );
    }

    let transitions_raw =
        fs::read_to_string(src_root.join("domain/transitions.rs")).expect("domain/transitions.rs");
    let transitions_prod = production_prefix(&transitions_raw);
    assert!(!transitions_prod.contains("get_or_spawn"));
}
