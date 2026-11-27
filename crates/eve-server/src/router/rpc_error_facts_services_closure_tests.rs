//! Crate-wide `#1195` guard and real-handler E2E tests for Track 3B-2 live-client
//! `BridgeRpcError` + `RejectionFacts` migrations across `dispatch.rs` and all
//! remaining service modules.

#![forbid(unsafe_code)]

use super::rpc_error_facts_domain_services_tests::{
    assert_rejected_wire_facts, assert_ship_not_in_space, collect_raw_amended_sites_with_helpers,
    ensure_nullsec_system, invoke_rpc_entry, seed_char_in_ctx,
    strip_cfg_test_items_preserving_strings, strip_rust_comments_keep_strings, upsert_test_type,
    AMENDED_LIVE_CLIENT_ERROR_CODES, CODE_FITTING_VALIDATION_FAILED, CODE_GM_ROLE_REQUIRED,
    CODE_INSUFFICIENT_ROLE, CODE_INVALID_TARGET, CODE_MODULE_NOT_FOUND,
    CODE_NOT_ENOUGH_CAPACITOR, CODE_SHIP_IN_WARP,
};
use crate::router::slash_registry::SlashInvocation;
use crate::services;
use crate::services::account_services::account_services_tests::{auth_session, test_ctx};
use eve_db::{
    AsteroidRecord, CosmicAnomalyRecord, FleetMemberRecord, FleetRecord, FleetRole, InventoryFlag,
    InventoryItemRecord,
};
use eve_destiny::{Ball, CombatState, MovementMode, WarpPhase};
use eve_dogma::{FittedModule, ModuleState};
use eve_machonet::service_roles;
use eve_proto::{PyRep, RejectionFacts, UserErrorKey};
use eve_sde::{DungeonAsteroidEntry, DungeonTemplate};
use glam::DVec3;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

const EXTRA_LOCAL_ERROR_HELPERS: &[&str] = &[
    "hf_err",
    "hf_ops_err",
    "rpc_msg_err",
    "scan_rpc_err",
    "probe_rpc_err",
    "nav_err",
    "fc_err",
    "fighter_rpc_err",
    "drone_launch_error",
    "drone_rpc_err",
    "corp_tail_err",
];

fn is_test_only_rel_path(rel_unix: &str) -> bool {
    let fname = rel_unix.rsplit('/').next().unwrap_or(rel_unix);
    rel_unix.starts_with("tests/")
        || rel_unix.contains("/tests/")
        || fname == "tests.rs"
        || fname.ends_with("_tests.rs")
        || fname.ends_with("_test.rs")
}

fn collect_rs_files_recursive(
    base: &Path,
    dir: &Path,
    out: &mut Vec<(String, PathBuf)>,
) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_rs_files_recursive(base, &path, out)?;
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            let rel = path
                .strip_prefix(base)
                .expect("path must be inside src_dir")
                .to_string_lossy()
                .replace('\\', "/");
            out.push((rel, path));
        }
    }
    Ok(())
}

#[test]
fn crate_wide_production_sources_have_zero_raw_amended_live_error_sites() {
    let src_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    collect_rs_files_recursive(&src_dir, &src_dir, &mut files)
        .expect("walk crates/eve-server/src");
    files.sort_by(|a, b| a.0.cmp(&b.0));

    let mut scanned = 0usize;
    let mut violations = Vec::new();
    for (rel_file, path) in files {
        if is_test_only_rel_path(&rel_file)
            || matches!(rel_file.as_str(), "router/rpc_fault_policy.rs" | "domain/market_acl.rs")
        {
            continue;
        }
        scanned += 1;
        let src = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
        violations.extend(collect_raw_amended_sites_with_helpers(
            &rel_file,
            &src,
            EXTRA_LOCAL_ERROR_HELPERS,
        ));
    }
    assert!(
        scanned >= 100,
        "expected crate-wide scan to visit >= 100 production files, scanned {scanned}"
    );
    assert!(
        violations.is_empty(),
        "found remaining raw #1195 live-client error sites across crates/eve-server/src:\n{}",
        violations.join("\n")
    );

    // Guard router/dispatch.rs against reintroducing `const ERR_INSUFFICIENT_ROLE`
    let dispatch_src = std::fs::read_to_string(src_dir.join("router/dispatch.rs"))
        .expect("read router/dispatch.rs");
    let dispatch_prod =
        strip_cfg_test_items_preserving_strings(&strip_rust_comments_keep_strings(&dispatch_src));
    assert!(
        !dispatch_prod.contains("ERR_INSUFFICIENT_ROLE"),
        "router/dispatch.rs must not define or use ERR_INSUFFICIENT_ROLE"
    );
    let helper_start = dispatch_prod
        .find("fn insufficient_role_error")
        .expect("insufficient_role_error present");
    let helper_end = dispatch_prod[helper_start..]
        .find("fn extract_moniker_object_id")
        .map(|off| helper_start + off)
        .expect("extract_moniker_object_id present");
    let helper_body = &dispatch_prod[helper_start..helper_end];
    assert!(
        helper_body.contains("BridgeRpcError::insufficient_role("),
        "ServerContext::insufficient_role_error must delegate to BridgeRpcError::insufficient_role"
    );
    assert_eq!(AMENDED_LIVE_CLIENT_ERROR_CODES.len(), 29);
}

#[tokio::test]
async fn dispatch_and_rbac_handlers_emit_typed_insufficient_role_facts() {
    let ctx = test_ctx();
    let uid = 93_101_u64;
    let char_id = 93_101_001_i64;
    let corp_id = 98_301_001_i64;
    let sys_id = 30_000_142_u32;
    let ship_id = 1_931_000_100_i64;
    let kw = HashMap::new();

    seed_char_in_ctx(&ctx, uid, char_id, corp_id, sys_id, Some(60_003_760), ship_id);

    // 1. router/dispatch.rs SeniorGm gate (ROLE_QA rejected against SeniorGm mask)
    let mut qa_sess = auth_session(uid, char_id, corp_id, sys_id);
    qa_sess.role = service_roles::ROLE_QA;
    let senior_err = ctx
        .dispatch_rpc(
            &mut qa_sess,
            "charUnboundMgr",
            None,
            "GetQAStarterSystemIDs",
            &[],
            &kw,
            None,
        )
        .await
        .expect_err("ROLE_QA must fail SeniorGm dispatch gate");
    let (facts, _) = assert_rejected_wire_facts(
        "charUnboundMgr",
        "GetQAStarterSystemIDs",
        Some(char_id),
        &senior_err,
        CODE_INSUFFICIENT_ROLE,
        UserErrorKey::InsufficientRoles,
    );
    assert_eq!(
        facts,
        &RejectionFacts::InsufficientRole {
            required_role_mask: service_roles::ROLE_GMH
                | service_roles::ROLE_GML
                | service_roles::ROLE_GMS
                | service_roles::ROLE_ADMIN,
            actual_role_mask: service_roles::ROLE_QA,
            action: "charUnboundMgr.GetQAStarterSystemIDs".into(),
        }
    );

    // 2. router/dispatch.rs Gm gate (ROLE_PLAYER_DEFAULT rejected against ROLE_GM_MASK)
    let mut player_sess = auth_session(uid, char_id, corp_id, sys_id);
    player_sess.role = service_roles::ROLE_PLAYER_DEFAULT;
    let gm_dispatch_err = ctx
        .dispatch_rpc(
            &mut player_sess,
            "timeControl",
            None,
            "AdvanceClock",
            &[PyRep::Integer(5)],
            &kw,
            None,
        )
        .await
        .expect_err("player must fail Gm dispatch gate");
    let (facts, _) = assert_rejected_wire_facts(
        "timeControl",
        "AdvanceClock",
        Some(char_id),
        &gm_dispatch_err,
        CODE_INSUFFICIENT_ROLE,
        UserErrorKey::InsufficientRoles,
    );
    assert_eq!(
        facts,
        &RejectionFacts::InsufficientRole {
            required_role_mask: service_roles::ROLE_GM_MASK,
            actual_role_mask: service_roles::ROLE_PLAYER_DEFAULT,
            action: "timeControl.AdvanceClock".into(),
        }
    );

    // 3. router/dispatch.rs Qa gate (ROLE_PLAYER_DEFAULT rejected against ROLE_GM_MASK | ROLE_QA)
    let qa_dispatch_err = ctx
        .dispatch_rpc(
            &mut player_sess,
            "publicQaToolsServer",
            None,
            "MoveMeTo",
            &[PyRep::Integer(i64::from(sys_id))],
            &kw,
            None,
        )
        .await
        .expect_err("player must fail Qa dispatch gate");
    let (facts, _) = assert_rejected_wire_facts(
        "publicQaToolsServer",
        "MoveMeTo",
        Some(char_id),
        &qa_dispatch_err,
        CODE_INSUFFICIENT_ROLE,
        UserErrorKey::InsufficientRoles,
    );
    assert_eq!(
        facts,
        &RejectionFacts::InsufficientRole {
            required_role_mask: service_roles::ROLE_GM_MASK | service_roles::ROLE_QA,
            actual_role_mask: service_roles::ROLE_PLAYER_DEFAULT,
            action: "publicQaToolsServer.MoveMeTo".into(),
        }
    );

    // 4. auth_cache handlers (ForwardCharacterNotification, SetGlobalConfigValue, /createccpevent)
    let fwd_err = services::auth_cache::handle(
        &ctx,
        &mut player_sess,
        "machoNet",
        "ForwardCharacterNotification",
        &[],
        &kw,
    )
    .await
    .expect("handled")
    .expect_err("non-GM ForwardCharacterNotification must fail");
    let (facts, _) = assert_rejected_wire_facts(
        "machoNet",
        "ForwardCharacterNotification",
        Some(char_id),
        &fwd_err,
        CODE_INSUFFICIENT_ROLE,
        UserErrorKey::InsufficientRoles,
    );
    assert_eq!(
        facts,
        &RejectionFacts::InsufficientRole {
            required_role_mask: service_roles::ROLE_GM_MASK,
            actual_role_mask: service_roles::ROLE_PLAYER_DEFAULT,
            action: "machoNet.ForwardCharacterNotification".into(),
        }
    );

    let cfg_err = services::auth_cache::handle(
        &ctx,
        &mut player_sess,
        "machoNet",
        "SetGlobalConfigValue",
        &[],
        &kw,
    )
    .await
    .expect("handled")
    .expect_err("non-GM SetGlobalConfigValue must fail");
    let (facts, _) = assert_rejected_wire_facts(
        "machoNet",
        "SetGlobalConfigValue",
        Some(char_id),
        &cfg_err,
        CODE_INSUFFICIENT_ROLE,
        UserErrorKey::InsufficientRoles,
    );
    assert_eq!(
        facts,
        &RejectionFacts::InsufficientRole {
            required_role_mask: service_roles::ROLE_GM_MASK,
            actual_role_mask: service_roles::ROLE_PLAYER_DEFAULT,
            action: "machoNet.SetGlobalConfigValue".into(),
        }
    );

    let mut gml_sess = player_sess.clone();
    gml_sess.role = service_roles::ROLE_GML;
    let ccp_inv = SlashInvocation {
        raw: "/createccpevent 1 60 1 Gala | Desc",
        head_raw: "createccpevent",
        head_lower: "createccpevent".into(),
        rest: "1 60 1 Gala | Desc",
        had_whitespace_after_head: true,
    };
    let ccp_err = services::auth_cache::dispatch_createccpevent_slash(&ctx, &gml_sess, &ccp_inv)
        .expect_err("ROLE_GML without ROLE_GMH must fail /createccpevent");
    assert_eq!(ccp_err.code, CODE_INSUFFICIENT_ROLE);
    assert!(matches!(
        ccp_err.client_error.as_deref(),
        Some(eve_proto::ClientError::Rejected {
            key: UserErrorKey::InsufficientRoles,
            facts,
            ..
        }) if facts == &RejectionFacts::InsufficientRole {
            required_role_mask: service_roles::ROLE_GMH,
            actual_role_mask: service_roles::ROLE_GML,
            action: "slash./createccpevent".into(),
        }
    ));
    let (ccp_wire, ccp_fb) = crate::router::rpc_fault_policy::map_call_req_error_with_telemetry(
        &crate::flight_recorder::FlightRecorder::new(),
        Some(char_id),
        "slash",
        "SlashCmd",
        &ccp_err,
    );
    assert!(!ccp_fb);
    assert!(matches!(
        ccp_wire,
        eve_proto::ClientError::User {
            key: UserErrorKey::SlashError,
            ..
        }
    ));

    // 5. jump_clone_names (publicQaToolsServer.MoveMeTo), fitting_ships_mastery (SetShipKillCounter),
    //    combat_crimewatch (gm_admin_bounty_action), space_probe_scanning (QAScanSites / QAOverrideProbeExpiry)
    let move_err = invoke_rpc_entry(
        services::jump_clone_names::RPCS,
        &ctx,
        &mut player_sess,
        "publicQaToolsServer",
        "MoveMeTo",
        &[PyRep::Integer(i64::from(sys_id))],
        &kw,
    )
    .await
    .expect_err("non-GM/QA MoveMeTo must fail");
    let (facts, _) = assert_rejected_wire_facts(
        "publicQaToolsServer",
        "MoveMeTo",
        Some(char_id),
        &move_err,
        CODE_INSUFFICIENT_ROLE,
        UserErrorKey::InsufficientRoles,
    );
    assert_eq!(
        facts,
        &RejectionFacts::InsufficientRole {
            required_role_mask: service_roles::ROLE_GM_MASK | service_roles::ROLE_QA,
            actual_role_mask: service_roles::ROLE_PLAYER_DEFAULT,
            action: "publicQaToolsServer.MoveMeTo".into(),
        }
    );

    let kill_err = invoke_rpc_entry(
        services::inventory_fitting::fitting_ships_mastery::RPCS,
        &ctx,
        &mut player_sess,
        "shipKillCounter",
        "SetShipKillCounter",
        &[PyRep::Integer(ship_id), PyRep::Integer(1)],
        &kw,
    )
    .await
    .expect_err("non-GM SetShipKillCounter must fail");
    let (facts, _) = assert_rejected_wire_facts(
        "shipKillCounter",
        "SetShipKillCounter",
        Some(char_id),
        &kill_err,
        CODE_GM_ROLE_REQUIRED,
        UserErrorKey::InsufficientRoles,
    );
    assert_eq!(
        facts,
        &RejectionFacts::InsufficientRole {
            required_role_mask: service_roles::ROLE_GM_MASK,
            actual_role_mask: service_roles::ROLE_PLAYER_DEFAULT,
            action: "SetShipKillCounter".into(),
        }
    );

    let bounty_err = services::combat_crimewatch::gm_admin_bounty_action(
        &ctx,
        &player_sess,
        "clear",
        char_id,
        0.0,
    )
    .expect_err("non-GM/QA gm_admin_bounty_action must fail");
    let (facts, _) = assert_rejected_wire_facts(
        "bountySvc",
        "gm_admin_bounty_action",
        Some(char_id),
        &bounty_err,
        CODE_INSUFFICIENT_ROLE,
        UserErrorKey::InsufficientRoles,
    );
    assert_eq!(
        facts,
        &RejectionFacts::InsufficientRole {
            required_role_mask: service_roles::ROLE_GM_MASK | service_roles::ROLE_QA,
            actual_role_mask: service_roles::ROLE_PLAYER_DEFAULT,
            action: "gm_admin_bounty_action".into(),
        }
    );

    for scan_method in ["QAOverrideProbeExpiry", "QAScanSites"] {
        let scan_err = invoke_rpc_entry(
            services::space_probe_scanning::RPCS,
            &ctx,
            &mut player_sess,
            "scanMgr",
            scan_method,
            &[],
            &kw,
        )
        .await
        .expect_err("non-GM scanMgr QA RPC must fail");
        let (facts, _) = assert_rejected_wire_facts(
            "scanMgr",
            scan_method,
            Some(char_id),
            &scan_err,
            CODE_GM_ROLE_REQUIRED,
            UserErrorKey::InsufficientRoles,
        );
        assert_eq!(
            facts,
            &RejectionFacts::InsufficientRole {
                required_role_mask: service_roles::ROLE_GM_MASK,
                actual_role_mask: service_roles::ROLE_PLAYER_DEFAULT,
                action: format!("scanMgr.{scan_method}"),
            }
        );
    }
}

#[tokio::test]
async fn space_warp_jump_handlers_emit_typed_facts_for_target_cap_warp_and_space() {
    let ctx = test_ctx();
    let uid = 93_201_u64;
    let char_id = 93_201_001_i64;
    let corp_id = 98_302_001_i64;
    let sys_id = 30_000_142_u32;
    let ship_id = 1_932_000_100_i64;
    let kw = HashMap::new();

    seed_char_in_ctx(&ctx, uid, char_id, corp_id, sys_id, None, ship_id);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: char_id,
                location_id: i64::from(sys_id),
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });

    let mut session = auth_session(uid, char_id, corp_id, sys_id);
    session.ship_id = Some(ship_id);

    // 1. ShipNotInSpace on CmdWarpToStuff, CmdStargateJump, and CmdDock when ship ball is absent
    let (gate_id, gate_pos) = ctx
        .sde
        .jumps
        .values()
        .find(|j| j.from_solar_system_id == sys_id)
        .map(|j| (j.stargate_id, j.position))
        .expect("SDE stargate in 30_000_142");
    let dock_id = ctx
        .sde
        .solar_systems
        .get(&sys_id)
        .and_then(|s| s.stations.first())
        .map(|st| (st.station_id, st.position))
        .expect("SDE station in 30_000_142");

    let warp_no_ball = ctx
        .dispatch_rpc(
            &mut session,
            "beyonce",
            None,
            "CmdWarpToStuff",
            &[
                PyRep::String("item".into()),
                PyRep::Integer(dock_id.0),
                PyRep::Real(0.0),
            ],
            &kw,
            None,
        )
        .await
        .expect_err("CmdWarpToStuff without ship ball must fail");
    assert_ship_not_in_space(
        "beyonce",
        "CmdWarpToStuff",
        Some(char_id),
        &warp_no_ball,
        ship_id,
        Some(char_id),
        Some(sys_id),
        "warp_to_stuff",
    );

    let jump_no_ball = ctx
        .dispatch_rpc(
            &mut session,
            "beyonce",
            None,
            "CmdStargateJump",
            &[PyRep::Integer(gate_id)],
            &kw,
            None,
        )
        .await
        .expect_err("CmdStargateJump without ship ball must fail");
    assert_ship_not_in_space(
        "beyonce",
        "CmdStargateJump",
        Some(char_id),
        &jump_no_ball,
        ship_id,
        Some(char_id),
        Some(sys_id),
        "stargate_jump",
    );

    let dock_no_ball = ctx
        .dispatch_rpc(
            &mut session,
            "beyonce",
            None,
            "CmdDock",
            &[PyRep::Integer(dock_id.0)],
            &kw,
            None,
        )
        .await
        .expect_err("CmdDock without ship ball must fail");
    assert_ship_not_in_space(
        "beyonce",
        "CmdDock",
        Some(char_id),
        &dock_no_ball,
        ship_id,
        Some(char_id),
        Some(sys_id),
        "dock",
    );

    // 2. InvalidTarget (non-positive & self-target) on CmdWarpToStuff
    let non_pos_err = ctx
        .dispatch_rpc(
            &mut session,
            "beyonce",
            None,
            "CmdWarpToStuff",
            &[
                PyRep::String("item".into()),
                PyRep::Integer(0),
                PyRep::Real(0.0),
            ],
            &kw,
            None,
        )
        .await
        .expect_err("non-positive warp target must fail");
    let (facts, args) = assert_rejected_wire_facts(
        "beyonce",
        "CmdWarpToStuff",
        Some(char_id),
        &non_pos_err,
        CODE_INVALID_TARGET,
        UserErrorKey::WarpDestinationGone,
    );
    assert!(args.is_empty());
    assert_eq!(
        facts,
        &RejectionFacts::InvalidTarget {
            target_id: Some(0),
            module_id: None,
            reason: "non_positive_target".into(),
        }
    );

    let self_warp_err = ctx
        .dispatch_rpc(
            &mut session,
            "beyonce",
            None,
            "CmdWarpToStuff",
            &[
                PyRep::String("item".into()),
                PyRep::Integer(ship_id),
                PyRep::Real(0.0),
            ],
            &kw,
            None,
        )
        .await
        .expect_err("self-warp target must fail");
    let (facts, args) = assert_rejected_wire_facts(
        "beyonce",
        "CmdWarpToStuff",
        Some(char_id),
        &self_warp_err,
        CODE_INVALID_TARGET,
        UserErrorKey::DeniedTargetSelf,
    );
    assert!(args.is_empty());
    assert_eq!(
        facts,
        &RejectionFacts::InvalidTarget {
            target_id: Some(ship_id),
            module_id: None,
            reason: "warp_to_self".into(),
        }
    );

    // 3. NotEnoughCapacitor on CmdWarpToStuff when capacitor cannot afford 150 km warp
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let ship_pos = dock_id.1 + DVec3::new(100_000_000.0, 0.0, 0.0);
    let mut ship_ball = Ball {
        id: ship_id,
        type_id: 587,
        position: ship_pos,
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 1.0,
        mass: 1_000_000_000_000.0,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let _ = sys_h.try_spawn_ball_with_combat(
        ship_ball.clone(),
        CombatState {
            capacitor: 0.01,
            max_capacitor: 500.0,
            ..Default::default()
        },
    );

    let cap_err = ctx
        .dispatch_rpc(
            &mut session,
            "beyonce",
            None,
            "CmdWarpToStuff",
            &[
                PyRep::String("item".into()),
                PyRep::Integer(dock_id.0),
                PyRep::Real(0.0),
            ],
            &kw,
            None,
        )
        .await
        .expect_err("CmdWarpToStuff with insufficient cap must fail");
    let (facts, args) = assert_rejected_wire_facts(
        "beyonce",
        "CmdWarpToStuff",
        Some(char_id),
        &cap_err,
        CODE_NOT_ENOUGH_CAPACITOR,
        UserErrorKey::NotEnoughEnergy,
    );
    assert!(args.iter().any(|(k, _)| k == "need"));
    assert!(matches!(
        facts,
        RejectionFacts::NotEnoughCapacitor {
            entity_id: Some(id),
            effect_name,
            required_gj,
            available_gj,
        } if *id == ship_id && effect_name == "warp_150km" && *required_gj > *available_gj
    ));

    // 4. ShipInWarp on CmdDock and evaluate_stargate_jump_perimeter
    let warp_dest = dock_id.1 + DVec3::new(100_000.0, 0.0, 0.0);
    ship_ball.position = warp_dest + DVec3::new(20_000.0, 0.0, 0.0);
    ship_ball.mode = MovementMode::Warp {
        origin: ship_pos,
        destination: warp_dest,
        warp_speed_au_s: 3.0,
        total_distance: ship_pos.distance(warp_dest),
        phase: WarpPhase::Cruising,
        elapsed_warp_time: 1.0,
    };
    let _ = sys_h.spawn_ball(ship_ball.clone()).await;

    let dock_warp_err = ctx
        .dispatch_rpc(
            &mut session,
            "beyonce",
            None,
            "CmdDock",
            &[PyRep::Integer(dock_id.0)],
            &kw,
            None,
        )
        .await
        .expect_err("CmdDock while mid-warp must fail with ShipInWarp");
    let (facts, _) = assert_rejected_wire_facts(
        "beyonce",
        "CmdDock",
        Some(char_id),
        &dock_warp_err,
        CODE_SHIP_IN_WARP,
        UserErrorKey::ShipInWarp,
    );
    assert_eq!(
        facts,
        &RejectionFacts::ShipInWarp {
            ship_id: Some(ship_id),
            action: "dock".into(),
            warp_phase: Some("Cruising".into()),
        }
    );

    let gate_warp_err = services::space_destiny::space_warp_jump::evaluate_stargate_jump_perimeter(
        &ship_ball,
        gate_pos,
        5_000.0,
    )
    .expect_err("stargate jump perimeter while mid-warp must fail with ShipInWarp");
    let (facts, _) = assert_rejected_wire_facts(
        "beyonce",
        "CmdStargateJump",
        Some(char_id),
        &gate_warp_err,
        CODE_SHIP_IN_WARP,
        UserErrorKey::ShipInWarp,
    );
    assert_eq!(
        facts,
        &RejectionFacts::ShipInWarp {
            ship_id: Some(ship_id),
            action: "stargate_jump".into(),
            warp_phase: Some("Cruising".into()),
        }
    );
}

#[tokio::test]
async fn pve_dungeon_cache_editor_and_runtime_rpc_emit_typed_facts() {
    let mut ctx = test_ctx();
    let uid = 93_301_u64;
    let char_id = 93_301_001_i64;
    let corp_id = 98_303_001_i64;
    let sys_id = 30_000_142_u32;
    let ore_sys_id = 30_933_001_u32;
    let ship_id = 1_933_000_100_i64;
    let kw = HashMap::new();

    {
        let sde_mut = std::sync::Arc::make_mut(&mut ctx.sde);
        if !sde_mut.solar_systems.contains_key(&ore_sys_id) {
            let mut mid_sys = sde_mut
                .solar_systems
                .get(&sys_id)
                .cloned()
                .expect("base SDE system 30_000_142");
            mid_sys.solar_system_id = ore_sys_id;
            mid_sys.security_status = 0.7;
            sde_mut.solar_systems.insert(ore_sys_id, mid_sys);
        }
    }

    // Ensure "Small Kernite and Omber Deposit" resolves in both real SDE and TestSde
    if !ctx
        .sde
        .dungeons
        .values()
        .any(|d| d.name.trim() == "Small Kernite and Omber Deposit")
    {
        let ore_did = 990_101_u32;
        upsert_test_type(&mut ctx.sde, 34, 17453, 460, 25, &[]);
        upsert_test_type(&mut ctx.sde, 34, 17452, 460, 25, &[]);
        upsert_test_type(&mut ctx.sde, 34, 20, 460, 25, &[]);
        upsert_test_type(&mut ctx.sde, 34, 17868, 461, 25, &[]);
        upsert_test_type(&mut ctx.sde, 34, 17867, 461, 25, &[]);
        upsert_test_type(&mut ctx.sde, 34, 1227, 461, 25, &[]);
        let sde_mut = std::sync::Arc::make_mut(&mut ctx.sde);
        sde_mut.dungeons.insert(
            ore_did,
            DungeonTemplate {
                dungeon_id: ore_did,
                name: "Small Kernite and Omber Deposit".into(),
                archetype_id: Some(27),
                faction_id: None,
                difficulty: Some(1),
            },
        );
        sde_mut.dungeon_asteroids.insert(
            ore_did,
            vec![
                DungeonAsteroidEntry { room_id: 1, type_id: 17453, asteroid_count: 1 },
                DungeonAsteroidEntry { room_id: 1, type_id: 17452, asteroid_count: 1 },
                DungeonAsteroidEntry { room_id: 1, type_id: 20, asteroid_count: 1 },
                DungeonAsteroidEntry { room_id: 1, type_id: 17868, asteroid_count: 1 },
                DungeonAsteroidEntry { room_id: 1, type_id: 17867, asteroid_count: 1 },
                DungeonAsteroidEntry { room_id: 1, type_id: 1227, asteroid_count: 1 },
            ],
        );
    }

    seed_char_in_ctx(&ctx, uid, char_id, corp_id, ore_sys_id, None, ship_id);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: char_id,
                location_id: i64::from(ore_sys_id),
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });
    let mut player_sess = auth_session(uid, char_id, corp_id, ore_sys_id);
    player_sess.role = service_roles::ROLE_PLAYER_DEFAULT;
    player_sess.ship_id = Some(ship_id);

    // 1. pve_dungeon_cache InsufficientRole (dungeon.DEGetDungeons, keeper.GetLevelEditor)
    for (svc, method) in [
        ("dungeon", "DEGetArchetypes"),
        ("dungeon", "DEGetDungeons"),
        ("dungeon", "DEGetFactions"),
        ("dungeon", "DEGetRoomObjectPaletteData"),
        ("dungeon", "DEGetRooms"),
        ("keeper", "GetLevelEditor"),
    ] {
        let err = services::pve_dungeon_cache::handle(&ctx, &mut player_sess, svc, method, &[], &kw)
            .await
            .expect("handled")
            .expect_err("non-GM dungeon cache RPC must fail");
        let (facts, _) = assert_rejected_wire_facts(
            svc,
            method,
            Some(char_id),
            &err,
            CODE_INSUFFICIENT_ROLE,
            UserErrorKey::InsufficientRoles,
        );
        assert_eq!(
            facts,
            &RejectionFacts::InsufficientRole {
                required_role_mask: service_roles::ROLE_GM_MASK,
                actual_role_mask: service_roles::ROLE_PLAYER_DEFAULT,
                action: format!("{svc}.{method}"),
            }
        );
    }

    // 2. pve_dungeon_editor InsufficientRole (dungeon.EditRoom)
    let ed_err = services::pve_dungeon_editor::handle_gm_dungeon_editor_op(
        &ctx,
        &mut player_sess,
        "EditRoom",
        &[],
        &kw,
    )
    .await
    .expect_err("non-GM EditRoom must fail");
    let (facts, _) = assert_rejected_wire_facts(
        "dungeon",
        "EditRoom",
        Some(char_id),
        &ed_err,
        CODE_INSUFFICIENT_ROLE,
        UserErrorKey::InsufficientRoles,
    );
    assert_eq!(
        facts,
        &RejectionFacts::InsufficientRole {
            required_role_mask: service_roles::ROLE_GM_MASK,
            actual_role_mask: service_roles::ROLE_PLAYER_DEFAULT,
            action: "dungeon.EditRoom".into(),
        }
    );

    // 3. pve_dungeon_cache NotEnoughCapacitor on custom_agency_provider.warp_to_content
    // Query readonly while docked so ore_sys_id anomalies stay unseeded for validate_pre_agency_warp
    player_sess.station_id = Some(60_003_760);
    let ore_rep = services::pve_dungeon_cache::handle(
        &ctx,
        &mut player_sess,
        "dungeonInstanceCacheMgr",
        "GetOreAnomalyInstances",
        &[],
        &kw,
    )
    .await
    .expect("handled")
    .expect("GetOreAnomalyInstances");
    player_sess.station_id = None;
    let PyRep::Dict(sys_entries) = ore_rep else {
        panic!("expected outer Dict from GetOreAnomalyInstances");
    };
    let (_, PyRep::Dict(dng_entries)) = &sys_entries[0] else {
        panic!("expected inner Dict");
    };
    let (_, PyRep::List(inst_ids)) = &dng_entries[0] else {
        panic!("expected instance list");
    };
    let unseeded_ore_iid = inst_ids[0].as_i64().expect("unseeded ore instanceID");

    let sys_h = ctx.systems.get_or_spawn(ore_sys_id);
    let _ = sys_h.try_spawn_ball_with_combat(
        Ball {
            id: ship_id,
            type_id: 587,
            position: DVec3::ZERO,
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1_000_000_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
        CombatState {
            capacitor: 0.01,
            max_capacitor: 500.0,
            ..Default::default()
        },
    );

    let agency_cap_err = services::pve_dungeon_cache::handle(
        &ctx,
        &mut player_sess,
        "custom_agency_provider",
        "warp_to_content",
        &[PyRep::Integer(unseeded_ore_iid)],
        &kw,
    )
    .await
    .expect("handled")
    .expect_err("agency warp with insufficient capacitor must fail");
    let (facts, args) = assert_rejected_wire_facts(
        "custom_agency_provider",
        "warp_to_content",
        Some(char_id),
        &agency_cap_err,
        CODE_NOT_ENOUGH_CAPACITOR,
        UserErrorKey::NotEnoughEnergy,
    );
    assert!(args.iter().any(|(k, _)| k == "need"));
    assert!(matches!(
        facts,
        RejectionFacts::NotEnoughCapacitor {
            entity_id: Some(id),
            effect_name,
            required_gj,
            available_gj,
        } if *id == ship_id && effect_name == "warp_150km" && *required_gj > *available_gj
    ));

    // 4. pve_dungeon_cache ShipNotInSpace when resolved anomaly exists and ship ball is removed
    let _ = sys_h.remove_ball(ship_id).await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.cosmic_anomalies
            .entry(ore_sys_id)
            .or_default()
            .push(CosmicAnomalyRecord {
                anomaly_id: "DNG-100-933001".into(),
                solar_system_id: ore_sys_id,
                name: "Test Anomaly".into(),
                anomaly_type: "OreAnomaly".into(),
                position: [500_000.0, 0.0, 0.0],
                signal_strength: 1.0,
                ore_types: Vec::new(),
                remaining_volume: 100.0,
                created_unix: 1_000,
                lifetime_seconds: 0,
                cleared: false,
            });
    });
    let agency_space_err = services::pve_dungeon_cache::handle(
        &ctx,
        &mut player_sess,
        "custom_agency_provider",
        "warp_to_content",
        &[PyRep::Integer(933_001)],
        &kw,
    )
    .await
    .expect("handled")
    .expect_err("agency warp without ship ball must fail");
    assert_ship_not_in_space(
        "custom_agency_provider",
        "warp_to_content",
        Some(char_id),
        &agency_space_err,
        ship_id,
        Some(char_id),
        Some(ore_sys_id),
        "agency_warp",
    );

    // 5. pve_dungeon_runtime_rpc ShipNotInSpace on try_activate_dungeon_runtime_gate
    let any_did = ctx
        .sde
        .dungeons
        .keys()
        .copied()
        .min()
        .expect("SDE has dungeon template");
    let mut spawn_kw = HashMap::new();
    spawn_kw.insert("solarSystemID".into(), PyRep::Integer(i64::from(ore_sys_id)));
    spawn_kw.insert("dungeonID".into(), PyRep::Integer(i64::from(any_did)));
    spawn_kw.insert("dungeonName".into(), PyRep::String("Closure Gate Test".into()));
    spawn_kw.insert("hasEntryGate".into(), PyRep::Bool(true));
    spawn_kw.insert(
        "position".into(),
        PyRep::List(vec![PyRep::Real(1_000_000.0), PyRep::Real(0.0), PyRep::Real(0.0)]),
    );
    let spawned = services::pve_dungeon_runtime_rpc::spawn_dungeon_instance(
        &ctx,
        char_id,
        Some(ore_sys_id),
        &[],
        &spawn_kw,
    )
    .await
    .expect("spawn_dungeon_instance");
    let entry_gate_id = spawned
        .dict_get("entryGateBallID")
        .and_then(|v| v.as_i64())
        .expect("entryGateBallID");

    let gate_space_err = services::pve_dungeon_runtime_rpc::try_activate_dungeon_runtime_gate(
        &ctx,
        &mut player_sess,
        ore_sys_id,
        &[PyRep::Integer(entry_gate_id)],
    )
    .await
    .expect("handled")
    .expect_err("runtime gate without ship ball must fail with ShipNotInSpace");
    assert_ship_not_in_space(
        "beyonce",
        "ActivateAccelerationGate",
        Some(char_id),
        &gate_space_err,
        ship_id,
        Some(char_id),
        Some(ore_sys_id),
        "dungeon_gate_activate",
    );
}

#[tokio::test]
async fn mining_combat_gm_fit_and_remaining_space_handlers_emit_typed_facts() {
    let mut ctx = test_ctx();
    let uid = 93_401_u64;
    let char_id = 93_401_001_i64;
    let other_char_id = 93_401_002_i64;
    let corp_id = 98_304_001_i64;
    let sys_id = ensure_nullsec_system(&mut ctx.sde);
    let ship_id = 1_934_000_100_i64;
    let missing_mod_id = 1_934_000_201_i64;
    let bad_type_mod_id = 1_934_000_202_i64;
    let mining_mod_id = 1_934_000_203_i64;
    let smartbomb_id = 1_934_000_204_i64;
    let ast_id = 1_934_000_301_i64;
    let fleet_id = 93_401_777_i64;
    let kw = HashMap::new();

    // Synthetic types for smartbomb (group 72) and drone (category 18)
    let drone_tid = 993_401_u32;
    let smartbomb_tid = 993_402_u32;
    upsert_test_type(&mut ctx.sde, 34, drone_tid, 100, 18, &[]);
    upsert_test_type(&mut ctx.sde, 34, smartbomb_tid, 72, 7, &[(6, 150.0), (99, 3_000.0)]);
    if !ctx.sde.types.contains_key(&1230) {
        upsert_test_type(&mut ctx.sde, 34, 1230, 462, 25, &[]);
    }

    seed_char_in_ctx(&ctx, uid, char_id, corp_id, sys_id, None, ship_id);
    seed_char_in_ctx(&ctx, uid + 1, other_char_id, corp_id, sys_id, None, ship_id + 1);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: char_id,
                location_id: i64::from(sys_id),
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            bad_type_mod_id,
            InventoryItemRecord {
                item_id: bad_type_mod_id,
                type_id: 999_999_888,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            mining_mod_id,
            InventoryItemRecord {
                item_id: mining_mod_id,
                type_id: 483,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot1,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.asteroids.insert(
            ast_id,
            AsteroidRecord {
                asteroid_id: ast_id,
                solar_system_id: sys_id,
                belt_id: 0,
                type_id: 1230,
                quantity: 500,
                initial_quantity: 500,
                position: (1_000.0, 0.0, 0.0),
                radius: 100.0,
            },
        );
        snap.fleets.insert(
            fleet_id,
            FleetRecord {
                fleet_id,
                boss_id: char_id,
                members: vec![char_id, other_char_id],
                member_details: HashMap::from([
                    (
                        char_id,
                        FleetMemberRecord {
                            character_id: char_id,
                            role: FleetRole::FleetCommander,
                            job: 2,
                            ..Default::default()
                        },
                    ),
                    (
                        other_char_id,
                        FleetMemberRecord {
                            character_id: other_char_id,
                            role: FleetRole::SquadMember,
                            job: 0,
                            ..Default::default()
                        },
                    ),
                ]),
                ..Default::default()
            },
        );
    });

    // 1. mining_cycle ModuleNotFound (missing item + unknown module type + deactivate non-owner)
    let mine_missing = services::mining_cycle::activate(
        &ctx,
        char_id,
        ship_id,
        sys_id,
        missing_mod_id,
        ast_id,
    )
    .await
    .expect_err("unfitted mining module must fail");
    let (facts, _) = assert_rejected_wire_facts(
        "dogmaIM",
        "Activate",
        Some(char_id),
        &mine_missing,
        CODE_MODULE_NOT_FOUND,
        UserErrorKey::ItemNotFound,
    );
    assert_eq!(
        facts,
        &RejectionFacts::ModuleNotFound {
            module_id: missing_mod_id,
            owner_entity_id: Some(ship_id),
            type_id: None,
        }
    );

    let mine_bad_type = services::mining_cycle::activate(
        &ctx,
        char_id,
        ship_id,
        sys_id,
        bad_type_mod_id,
        ast_id,
    )
    .await
    .expect_err("unknown mining module type must fail");
    let (facts, _) = assert_rejected_wire_facts(
        "dogmaIM",
        "Activate",
        Some(char_id),
        &mine_bad_type,
        CODE_MODULE_NOT_FOUND,
        UserErrorKey::ItemNotFound,
    );
    assert_eq!(
        facts,
        &RejectionFacts::ModuleNotFound {
            module_id: bad_type_mod_id,
            owner_entity_id: Some(ship_id),
            type_id: Some(999_999_888),
        }
    );

    ctx.mining_cycles.write().unwrap().insert(
        mining_mod_id,
        services::mining_cycle::ActiveMiningCycle {
            char_id,
            ship_id,
            module_id: mining_mod_id,
            target_id: ast_id,
            solar_system_id: sys_id,
            kind: services::mining_cycle::MiningKind::Asteroid,
            effect_name: "miningLaser".into(),
            cycle_start_ms: 0,
            duration_ms: 60_000,
        },
    );
    let mine_deact = services::mining_cycle::deactivate(
        &ctx,
        other_char_id,
        Some(ship_id),
        mining_mod_id,
    )
    .await
    .expect("handled")
    .expect_err("wrong owner deactivate must fail with ModuleNotFound");
    let (facts, _) = assert_rejected_wire_facts(
        "dogmaIM",
        "Deactivate",
        Some(other_char_id),
        &mine_deact,
        CODE_MODULE_NOT_FOUND,
        UserErrorKey::ItemNotFound,
    );
    assert_eq!(
        facts,
        &RejectionFacts::ModuleNotFound {
            module_id: mining_mod_id,
            owner_entity_id: Some(ship_id),
            type_id: None,
        }
    );

    // 2. character_gm_tools FittingValidationFailed (/fit drone into HiSlot0 = 11)
    let mut gm_sess = auth_session(uid, char_id, corp_id, sys_id);
    gm_sess.role = service_roles::ROLE_GM_MASK;
    gm_sess.ship_id = Some(ship_id);
    let fit_err = ctx
        .dispatch_rpc(
            &mut gm_sess,
            "slash",
            None,
            "SlashCmd",
            &[PyRep::String(format!("/fit {ship_id} {drone_tid} 11"))],
            &kw,
            None,
        )
        .await
        .expect_err("/fit drone into HiSlot0 must fail with FittingValidationFailed");
    assert_eq!(fit_err.code, CODE_FITTING_VALIDATION_FAILED);
    let Some(eve_proto::ClientError::Rejected { key, args, facts }) =
        fit_err.client_error.as_deref()
    else {
        panic!("expected ClientError::Rejected on /fit error, got {:?}", fit_err.client_error);
    };
    assert_eq!(*key, UserErrorKey::ModuleFitFailed);
    assert!(args
        .iter()
        .any(|(k, v)| k == "moduleName" && v == &PyRep::String(format!("TestType_{drone_tid}"))));
    assert!(matches!(
        facts,
        RejectionFacts::FittingValidationFailed {
            ship_id: Some(sid),
            item_id: None,
            module_type_id: Some(tid),
            reason,
        } if *sid == ship_id && *tid == i64::from(drone_tid) && reason.contains("cannot be fitted")
    ));
    let (fit_wire, fit_fb) = crate::router::rpc_fault_policy::map_call_req_error_with_telemetry(
        &crate::flight_recorder::FlightRecorder::new(),
        Some(char_id),
        "slash",
        "SlashCmd",
        &fit_err,
    );
    assert!(!fit_fb);
    assert!(matches!(
        fit_wire,
        eve_proto::ClientError::User {
            key: UserErrorKey::SlashError,
            ..
        }
    ));

    // 3. pve_ess_reserve_banks, fleet_social_fleet_ops, fleet_social_scanning, space_probe_scanning,
    //    and pve_homefront_sites ShipNotInSpace when ship ball is absent
    let ess_gate_err = services::pve_ess_reserve_banks::try_activate_ess_acceleration_gate(
        &ctx,
        char_id,
        sys_id,
        ship_id,
        services::pve_ess_reserve_banks::ESS_GATE_ID_BASE + i64::from(sys_id),
    )
    .await
    .expect("handled")
    .expect_err("ESS gate without ship ball must fail with ShipNotInSpace");
    assert_ship_not_in_space(
        "beyonce",
        "ActivateAccelerationGate",
        Some(char_id),
        &ess_gate_err,
        ship_id,
        Some(char_id),
        Some(sys_id),
        "ess_reserve_bank",
    );

    let mut player_sess = auth_session(uid, char_id, corp_id, sys_id);
    player_sess.ship_id = Some(ship_id);
    player_sess.fleet_id = Some(fleet_id);
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let other_ship_id = ship_id + 1;
    let _ = sys_h
        .spawn_ball(Ball {
            id: other_ship_id,
            type_id: 587,
            position: DVec3::new(500_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    // 3a. fleetMgr.WarpToMember while caller is docked -> fleet_social_fleet_ops ShipNotInSpace ("fleet_warp")
    player_sess.station_id = Some(60_003_760);
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().station_id = Some(60_003_760);
    });
    let fleet_warp_err = ctx
        .dispatch_rpc(
            &mut player_sess,
            "fleetMgr",
            None,
            "WarpToMember",
            &[PyRep::Integer(other_char_id)],
            &kw,
            None,
        )
        .await
        .expect_err("WarpToMember while caller is docked must fail with ShipNotInSpace");
    assert_ship_not_in_space(
        "fleetMgr",
        "WarpToMember",
        Some(char_id),
        &fleet_warp_err,
        ship_id,
        Some(char_id),
        Some(sys_id),
        "fleet_warp",
    );
    player_sess.station_id = None;
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().station_id = None;
    });

    let generic_gate_err = services::fleet_social::fleet_social_scanning::handle_scanning_and_wormholes(
        &ctx,
        &mut player_sess,
        "beyonce",
        "ActivateAccelerationGate",
        &[PyRep::Integer(99_999_001)],
        &kw,
    )
    .await
    .expect("handled")
    .expect_err("generic acceleration gate without ship ball must fail");
    assert_ship_not_in_space(
        "beyonce",
        "ActivateAccelerationGate",
        Some(char_id),
        &generic_gate_err,
        ship_id,
        Some(char_id),
        Some(sys_id),
        "ValidateShipMovement",
    );

    let mut docked_sess = player_sess.clone();
    docked_sess.station_id = Some(60_003_760);
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().station_id = Some(60_003_760);
    });
    let probe_err = services::space_probe_scanning::activate_scan_probe_launcher(
        &ctx,
        &docked_sess,
        &sys_h,
        char_id,
        ship_id,
        sys_id,
        missing_mod_id,
        17_938,
        InventoryFlag::HiSlot0 as u16,
        "scanProbe",
        None,
        &kw,
    )
    .await
    .expect_err("docked probe launch must fail with ShipNotInSpace");
    assert_ship_not_in_space(
        "dogmaIM",
        "Activate",
        Some(char_id),
        &probe_err,
        ship_id,
        Some(char_id),
        Some(sys_id),
        "activate_scan_probe_launcher",
    );

    let hs_sys = services::pve_homefront_sites::HOMEFRONT_SYSTEMS[0];
    if ctx.sde.solar_systems.contains_key(&hs_sys) {
        services::pve_special_sites::ensure_special_sites_initialized(&ctx);
        services::pve_homefront_sites::ensure_homefront_outer_entries_sync(
            &ctx,
            Some(hs_sys),
            None,
        );
        if let Some(hf_gate_id) = ctx.db.with_snapshot(|snap| {
            snap.dungeon_runtime
                .homefront_runtimes
                .values()
                .find(|r| r.solar_system_id == hs_sys && !r.completed && !r.failed)
                .map(|r| r.outer_gate_ball_id)
        }) {
            let mut hf_sess = auth_session(uid, char_id, corp_id, hs_sys);
            hf_sess.ship_id = Some(ship_id);
            let hf_err = services::pve_homefront_sites::activate_homefront_gate_by_ball_id(
                &ctx,
                &mut hf_sess,
                hs_sys,
                hf_gate_id,
            )
            .await
            .expect("handled")
            .expect_err("homefront gate without ship ball must fail");
            assert_ship_not_in_space(
                "beyonce",
                "ActivateAccelerationGate",
                Some(char_id),
                &hf_err,
                ship_id,
                Some(char_id),
                Some(hs_sys),
                "homefront_site",
            );
        }
    }

    // 4. combat_missiles_aoe NotEnoughCapacitor on smartbomb activation
    let attacker_ball = Ball {
        id: ship_id,
        type_id: 587,
        position: DVec3::ZERO,
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 1.0,
        mass: 1_000_000.0,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let _ = sys_h.try_spawn_ball_with_combat(
        attacker_ball.clone(),
        CombatState {
            capacitor: 12.0,
            max_capacitor: 500.0,
            ..Default::default()
        },
    );
    let smartbomb_type = ctx.sde.types.get(&smartbomb_tid).cloned().expect("smartbomb type");
    let smartbomb_mod = FittedModule {
        item_id: smartbomb_id,
        type_id: smartbomb_tid,
        flag_id: InventoryFlag::HiSlot2 as u16,
        state: ModuleState::Online,
        damage_hp: 0.0,
        max_hp: 40.0,
        charge_type_id: None,
        charge_quantity: 0,
        dynamic_attribute_overrides: HashMap::new(),
    };
    let sb_err = services::combat_crimewatch::combat_missiles_aoe::activate_aoe_or_defender_module(
        &ctx,
        char_id,
        ship_id,
        sys_id,
        -0.5,
        ctx.now_unix(),
        smartbomb_id,
        &smartbomb_mod,
        &smartbomb_type,
        &attacker_ball,
        None,
        &kw,
    )
    .await
    .expect("handled")
    .expect_err("smartbomb with low capacitor must fail with NotEnoughCapacitor");
    let (facts, args) = assert_rejected_wire_facts(
        "dogmaIM",
        "Activate",
        Some(char_id),
        &sb_err,
        CODE_NOT_ENOUGH_CAPACITOR,
        UserErrorKey::NotEnoughEnergy,
    );
    assert!(args
        .iter()
        .any(|(k, v)| k == "effectname" && v == &PyRep::String("smartbomb".into())));
    assert_eq!(
        facts,
        &RejectionFacts::NotEnoughCapacitor {
            entity_id: Some(ship_id),
            effect_name: "smartbomb".into(),
            required_gj: 150.0,
            available_gj: 12.0,
        }
    );
}
