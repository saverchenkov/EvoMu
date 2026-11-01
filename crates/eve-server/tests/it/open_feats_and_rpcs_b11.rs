//! Integration tests for `feat:serverops.session-changes`,
//! `feat:removed.medical-clone-grades`, and cluster/DB2 fail-closed guards (batch 11).

#![forbid(unsafe_code)]

use crate::common;
use crate::sp6_logoff_and_server_tick::persist_and_reopen_sqlite;
use eve_db::{corp_roles, service_roles, InventoryFlag, InventoryItemRecord, TrainedSkillRecord};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::combat::death_pipeline::execute_death_pipeline_sync;
use eve_server::router::ServerContext;
use glam::DVec3;
use std::collections::HashMap;

const P1: i64 = 90_000_001;
const P2: i64 = 90_000_002;
const JITA_4_4: i64 = 60_003_760;
const JITA_SYS: u32 = 30_000_142;

async fn select_char(
    ctx: &ServerContext,
    sid: u64,
    user: &str,
    pass: &str,
    char_id: i64,
) -> SessionState {
    let mut s = SessionState::new(sid);
    common::login_pilot(ctx, &mut s, user, pass).await;
    ctx.dispatch_rpc(
        &mut s,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &HashMap::new(),
        None,
    )
    .await
    .expect("SelectCharacterID");
    ctx.advance_secs(12);
    s
}

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, eve_proto::BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None)
        .await
}

fn find_session_change_tuple(
    notifs: &[eve_server::router::cache_notifications::PendingMachoNotification],
    attr_key: &str,
) -> Option<(PyRep, PyRep)> {
    for n in notifs.iter().filter(|n| n.event == "OnSessionChanged") {
        for arg in &n.args {
            if let PyRep::Dict(entries) = arg {
                for (k, v) in entries {
                    if k.as_str() == Some(attr_key) {
                        if let PyRep::Tuple(pair) = v {
                            if pair.len() == 2 {
                                return Some((pair[0].clone(), pair[1].clone()));
                            }
                        }
                    }
                }
            }
        }
    }
    None
}

#[tokio::test]
async fn test_serverops_session_changes_all_attributes() {
    // parity: feat:serverops.session-changes
    eve_testkit::attest(&["feat:serverops.session-changes"]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let mut p1 = select_char(&ctx, 71_001, "demo", "demo-secret", P1).await;
    let mut p2 = select_char(&ctx, 71_002, "other", "other-secret", P2).await;
    assert_eq!(p1.station_id, Some(JITA_4_4));

    // 1. Corporation attribute change (`corpid`) + 10s session-change timer
    let old_corp_id = p1.corporation_id.expect("initial corp");
    let _ = ctx.drain_notifications_for_char(P1);
    rpc(
        &ctx,
        &mut p1,
        "corpRegistry",
        "AddCorporation",
        &[
            PyRep::String("Batch11 Session Ops Corp".into()),
            PyRep::String("B11S".into()),
            PyRep::String("Session propagation verification".into()),
        ],
    )
    .await
    .expect("AddCorporation");
    let new_corp_id = p1.corporation_id.expect("new corp id");
    assert_ne!(new_corp_id, old_corp_id);

    let corp_notifs = ctx.drain_notifications_for_char(P1);
    let (old_c, new_c) =
        find_session_change_tuple(&corp_notifs, "corpid").expect("corpid OnSessionChanged");
    assert_eq!(old_c.as_i64(), Some(old_corp_id));
    assert_eq!(new_c.as_i64(), Some(new_corp_id));

    assert!(
        eve_server::services::space_destiny::check_session_change_timer_active(
            &ctx,
            P1,
            ctx.now_unix(),
            None
        )
        .is_some(),
        "session-change timer must activate on corp change"
    );
    ctx.advance_secs(15);
    assert!(
        eve_server::services::space_destiny::check_session_change_timer_active(
            &ctx,
            P1,
            ctx.now_unix(),
            None
        )
        .is_none(),
        "session-change timer must expire after 15s"
    );

    // 2. Corporation role attribute change (`corprole`) propagating to client & downstream services
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&P2) {
            ch.corporation_id = new_corp_id;
        }
        if let Some(corp) = snap.corporations.get_mut(&new_corp_id) {
            corp.members.insert(P2);
            corp.member_roles.insert(P2, 0);
        }
    });
    p2.corporation_id = Some(new_corp_id);
    p2.corp_role = 0;

    // Without ROLE_STATION_MANAGER, P2 cannot create a structure profile
    assert!(
        rpc(
            &ctx,
            &mut p2,
            "structureSettings",
            "CreateProfile",
            &[
                PyRep::String("Denied Profile".into()),
                PyRep::String("No role yet".into()),
            ],
        )
        .await
        .is_err(),
        "member without station manager/director role must be rejected"
    );

    let _ = ctx.drain_notifications_for_char(P2);
    rpc(
        &ctx,
        &mut p1,
        "corpRegistry",
        "UpdateMember",
        &[
            PyRep::Integer(P2),
            PyRep::Integer(corp_roles::ROLE_STATION_MANAGER as i64),
        ],
    )
    .await
    .expect("UpdateMember grant ROLE_STATION_MANAGER");

    let role_notifs = ctx.drain_notifications_for_char(P2);
    let (old_r, new_r) =
        find_session_change_tuple(&role_notifs, "corprole").expect("corprole OnSessionChanged");
    assert_eq!(old_r.as_i64(), Some(0));
    assert_eq!(new_r.as_i64(), Some(corp_roles::ROLE_STATION_MANAGER as i64));

    // Next RPC syncs P2's live SessionState.corp_role and authorizes structureSettings.CreateProfile
    let prof_id = rpc(
        &ctx,
        &mut p2,
        "structureSettings",
        "CreateProfile",
        &[
            PyRep::String("Authorized Profile".into()),
            PyRep::String("Granted via live session role sync".into()),
        ],
    )
    .await
    .expect("CreateProfile after role grant")
    .as_i64()
    .expect("profile ID");
    assert!(prof_id > 0);
    assert_eq!(p2.corp_role, corp_roles::ROLE_STATION_MANAGER);

    // Revoking ROLE_STATION_MANAGER propagates immediately to P2's live session and revokes access
    let _ = ctx.drain_notifications_for_char(P2);
    rpc(
        &ctx,
        &mut p1,
        "corpRegistry",
        "UpdateMember",
        &[PyRep::Integer(P2), PyRep::Integer(0)],
    )
    .await
    .expect("UpdateMember revoke ROLE_STATION_MANAGER");
    let revoke_notifs = ctx.drain_notifications_for_char(P2);
    let (prev_r, zero_r) = find_session_change_tuple(&revoke_notifs, "corprole")
        .expect("corprole OnSessionChanged on revoke");
    assert_eq!(
        prev_r.as_i64(),
        Some(corp_roles::ROLE_STATION_MANAGER as i64)
    );
    assert_eq!(zero_r.as_i64(), Some(0));
    assert!(
        rpc(
            &ctx,
            &mut p2,
            "structureSettings",
            "CreateProfile",
            &[
                PyRep::String("Revoked Profile".into()),
                PyRep::String("Must fail after role revocation".into()),
            ],
        )
        .await
        .is_err(),
        "revoked role must immediately block downstream service call"
    );
    assert_eq!(p2.corp_role, 0);

    // 3. Ship attribute change (`shipid`) in station hangar via `ship.Board`
    let old_ship_id = p1.ship_id.expect("initial ship");
    let alt_ship_id = 100_911_001i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            alt_ship_id,
            InventoryItemRecord {
                item_id: alt_ship_id,
                type_id: 606, // Velator (corvette, 0 skill prereqs)
                owner_id: P1,
                location_id: JITA_4_4,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    let _ = ctx.drain_notifications_for_char(P1);
    rpc(&ctx, &mut p1, "ship", "Board", &[PyRep::Integer(alt_ship_id)])
        .await
        .expect("ship.Board in station");
    assert_eq!(p1.ship_id, Some(alt_ship_id));
    let ship_notifs = ctx.drain_notifications_for_char(P1);
    let (old_s, new_s) =
        find_session_change_tuple(&ship_notifs, "shipid").expect("shipid OnSessionChanged");
    assert_eq!(old_s.as_i64(), Some(old_ship_id));
    assert_eq!(new_s.as_i64(), Some(alt_ship_id));
    ctx.advance_secs(15);

    // 4. Location attribute changes (`stationid` / `solarsystemid`) across Undock and Dock
    let _ = ctx.drain_notifications_for_char(P1);
    rpc(&ctx, &mut p1, "stationSvc", "Undock", &[])
        .await
        .expect("stationSvc.Undock");
    assert_eq!(p1.station_id, None);
    assert_eq!(p1.solar_system_id, Some(JITA_SYS));
    let undock_notifs = ctx.drain_notifications_for_char(P1);
    let (st_before, st_after) =
        find_session_change_tuple(&undock_notifs, "stationid").expect("Undock stationid change");
    assert_eq!(st_before.as_i64(), Some(JITA_4_4));
    assert_eq!(st_after, PyRep::None);

    let sys_handle = ctx.systems.get_or_spawn(JITA_SYS);
    assert!(
        sys_handle
            .get_ball_snapshot(alt_ship_id)
            .await
            .expect("ball query")
            .is_some(),
        "undocked ship must be spawned in solar system ballpark"
    );

    ctx.advance_secs(15);
    let _ = ctx.drain_notifications_for_char(P1);
    rpc(
        &ctx,
        &mut p1,
        "stationSvc",
        "Dock",
        &[PyRep::Integer(JITA_4_4)],
    )
    .await
    .expect("stationSvc.Dock");
    assert_eq!(p1.station_id, Some(JITA_4_4));
    let dock_notifs = ctx.drain_notifications_for_char(P1);
    let (dock_before, dock_after) =
        find_session_change_tuple(&dock_notifs, "stationid").expect("Dock stationid change");
    assert_eq!(dock_before, PyRep::None);
    assert_eq!(dock_after.as_i64(), Some(JITA_4_4));
}

#[tokio::test]
async fn test_removed_medical_clone_grades_free_clone_and_zero_sp_loss() {
    // parity: feat:removed.medical-clone-grades
    eve_testkit::attest(&["feat:removed.medical-clone-grades"]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let mut p1 = select_char(&ctx, 71_010, "demo", "demo-secret", P1).await;

    // Seed > 5,000,000 SP across skills (far above pre-Rhea 900,000 SP basic medical clone cap)
    ctx.db.with_mut_snapshot(|snap| {
        let skills = snap.skills.entry(P1).or_default();
        skills.insert(
            3300,
            TrainedSkillRecord {
                type_id: 3300,
                level: 5,
                skill_points: 2_560_000.0,
            },
        );
        skills.insert(
            3380,
            TrainedSkillRecord {
                type_id: 3380,
                level: 5,
                skill_points: 2_560_000.0,
            },
        );
        skills.insert(
            3449,
            TrainedSkillRecord {
                type_id: 3449,
                level: 5,
                skill_points: 768_000.0,
            },
        );
    });

    let (bal_before, school_station_id, skills_before) = ctx.db.with_snapshot(|snap| {
        let ch = snap.characters.get(&P1).expect("P1");
        let school_st = ctx
            .sde
            .schools
            .get(&ch.school_id)
            .map(|s| s.station_id)
            .filter(|&id| id > 0)
            .unwrap_or(JITA_4_4);
        let sk: HashMap<u32, (u8, f64)> = snap
            .skills
            .get(&P1)
            .expect("skills")
            .iter()
            .map(|(&tid, r)| (tid, (r.level, r.skill_points)))
            .collect();
        (ch.balance.0, school_st, sk)
    });
    let total_sp_before: f64 = skills_before.values().map(|(_, sp)| *sp).sum();
    assert!(total_sp_before > 5_000_000.0);

    // 1. SetHomeStation to school station is free (0 ISK, no medical clone grade fee)
    let _ = ctx.drain_notifications_for_char(P1);
    let set_res = rpc(
        &ctx,
        &mut p1,
        "charMgr",
        "SetHomeStation",
        &[PyRep::Integer(school_station_id)],
    )
    .await
    .expect("SetHomeStation");
    assert_eq!(set_res.as_i64(), Some(school_station_id));
    let home = rpc(&ctx, &mut p1, "charMgr", "GetHomeStation", &[])
        .await
        .expect("GetHomeStation");
    assert_eq!(home.as_i64(), Some(school_station_id));

    let bal_after_set = ctx
        .db
        .with_snapshot(|snap| snap.characters.get(&P1).expect("P1").balance.0);
    assert_eq!(
        bal_before, bal_after_set,
        "medical clone home station registration must charge 0 ISK"
    );

    // 2. Undock, lose ship -> capsule, then lose capsule (pod death) -> respawn at home_station_id with 0 SP loss
    rpc(&ctx, &mut p1, "stationSvc", "Undock", &[])
        .await
        .expect("Undock");
    let ship_id = p1.ship_id.expect("active ship");
    let ship_death = execute_death_pipeline_sync(
        &ctx,
        JITA_SYS,
        P1,
        ship_id,
        P2,
        DVec3::new(1_000.0, 0.0, 0.0),
        42,
    )
    .expect("ship destruction");
    let capsule_id = ship_death.ejected_capsule_id.expect("ejected capsule");

    let _pod_death = execute_death_pipeline_sync(
        &ctx,
        JITA_SYS,
        P1,
        capsule_id,
        P2,
        DVec3::new(1_000.0, 0.0, 0.0),
        43,
    )
    .expect("pod destruction");

    let (respawn_station, bal_after_pod, skills_after) = ctx.db.with_snapshot(|snap| {
        let ch = snap.characters.get(&P1).expect("P1 after pod death");
        let sk: HashMap<u32, (u8, f64)> = snap
            .skills
            .get(&P1)
            .expect("skills after pod death")
            .iter()
            .map(|(&tid, r)| (tid, (r.level, r.skill_points)))
            .collect();
        (ch.station_id, ch.balance.0, sk)
    });
    assert_eq!(
        respawn_station,
        Some(school_station_id),
        "pod destruction must respawn pilot at home_station_id"
    );
    assert_eq!(
        skills_before, skills_after,
        "Rhea medical clone rules: pod destruction must cause zero skill-point loss"
    );
    assert!(
        bal_after_pod >= bal_before,
        "pod destruction must never deduct a medical clone upgrade fee"
    );

    // 3. Reload from SQLite and verify home station and zero-SP-loss skill state persist
    let ctx_reloaded = persist_and_reopen_sqlite(&ctx, "b11_medical_clone_reload");
    let mut p1_re = select_char(&ctx_reloaded, 71_011, "demo", "demo-secret", P1).await;
    let home_reloaded = rpc(&ctx_reloaded, &mut p1_re, "charMgr", "GetHomeStation", &[])
        .await
        .expect("GetHomeStation after reload");
    assert_eq!(home_reloaded.as_i64(), Some(school_station_id));
    let skills_reloaded: HashMap<u32, (u8, f64)> = ctx_reloaded.db.with_snapshot(|snap| {
        snap.skills
            .get(&P1)
            .expect("reloaded skills")
            .iter()
            .map(|(&tid, r)| (tid, (r.level, r.skill_points)))
            .collect()
    });
    assert_eq!(skills_before, skills_reloaded);
}

#[tokio::test]
async fn test_cluster_admin_and_db2_fail_closed_guards() {
    let ctx = ServerContext::new();
    let mut admin = select_char(&ctx, 71_020, "demo", "demo-secret", P1).await;
    let mut unpriv = select_char(&ctx, 71_021, "other", "other-secret", P2).await;

    // Unprivileged sessions must be rejected on all cluster/QA/admin RPCs
    for (svc, method, args) in [
        ("loadService", "StartLoad", vec![]),
        ("loadService", "StopLoad", vec![]),
        ("loadService", "GetTotalStats", vec![]),
        ("sessionMgr", "EndAllGameSessions", vec![]),
        ("raffleMgr", "QA_SeedRaffles", vec![PyRep::Integer(1)]),
        (
            "DB2",
            "CallProc",
            vec![PyRep::String("zcluster.Nodes_PolarisID".into())],
        ),
        ("debug", "Exec", vec![PyRep::String("status".into())]),
        ("localizationServer", "ReloadFSDPickle", vec![]),
        ("localizationServer", "UpdateLocalizationQAWrap", vec![]),
    ] {
        assert!(
            rpc(&ctx, &mut unpriv, svc, method, &args).await.is_err(),
            "{svc}.{method} must reject unprivileged player session"
        );
    }

    admin.role |= service_roles::ROLE_GM_DEFAULT | eve_machonet::service_roles::ROLE_PROGRAMMER;

    let polaris = rpc(
        &ctx,
        &mut admin,
        "DB2",
        "CallProc",
        &[PyRep::String("zcluster.Nodes_PolarisID".into())],
    )
    .await
    .expect("known DB2.CallProc");
    assert_eq!(polaris, PyRep::Integer(1));

    // Unknown stored procedures must fail closed instead of silently returning None
    let unknown_proc = rpc(
        &ctx,
        &mut admin,
        "DB2",
        "CallProc",
        &[PyRep::String("zcluster.NonExistentProc".into())],
    )
    .await
    .expect_err("unknown DB2.CallProc procedure must fail closed");
    assert_eq!(unknown_proc.message, "InvalidProcedure");
}
