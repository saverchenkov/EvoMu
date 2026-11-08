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

#[allow(unused_imports, dead_code, unused_variables, unused_mut)]
pub(crate) mod atomic_rollback_and_quasar_gateway_domains {
    use super::*;

// Integration tests for `feat:invariant.atomic-rollback-all-stores` and
// Quasar `plex` / `character` / `user` gateway domains.


use crate::common;
use crate::sp6_logoff_and_server_tick::persist_and_reopen_sqlite;
use eve_db::{InventoryFlag, IskCents, MiningLedgerRecord, WorldRepository};
use eve_machonet::SessionState;
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, EvePublicNotice,
    EvePublicRequest, EvePublicResponse, GoogleAny, GoogleTimestamp, QuasarDescriptorRegistry,
    QuasarMessage, WireMessage, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::quasar_gateway::{
    STATUS_BAD_REQUEST, STATUS_NOT_FOUND, STATUS_OK, STATUS_UNAUTHORIZED,
};
use eve_server::router::ServerContext;
use eve_server::services::quasar_auth_user::{
    activate_user_omega_license, expire_user_omega_license,
};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::mpsc::Receiver;

const P1: i64 = 90_000_001;
const P2: i64 = 90_000_002;
const JITA_4_4: i64 = 60_003_760;

pub(crate) fn temp_sqlite_path(tag: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("evomu_{tag}_{}.sqlite", std::process::id()));
    let _ = std::fs::remove_file(&p);
    let _ = std::fs::remove_file(p.with_extension("sqlite-wal"));
    let _ = std::fs::remove_file(p.with_extension("sqlite-shm"));
    p
}

pub(crate) fn cleanup_sqlite_path(p: &PathBuf) {
    let _ = std::fs::remove_file(p);
    let _ = std::fs::remove_file(p.with_extension("sqlite-wal"));
    let _ = std::fs::remove_file(p.with_extension("sqlite-shm"));
}

pub(crate) async fn select_char(
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

pub(crate) async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None)
        .await
}

pub(crate) async fn quasar_rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    type_url: &str,
    payload_value: Vec<u8>,
) -> EvePublicResponse {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let req = EvePublicRequest {
        issued: Some(GoogleTimestamp {
            seconds: ctx.now_unix() as i64,
            nanos: 0,
        }),
        correlation_uuid: format!("b11-q-{n:08}").into_bytes(),
        payload: Some(GoogleAny {
            type_url: if type_url.starts_with(TYPE_GOOGLEAPIS_PREFIX) {
                type_url.to_string()
            } else {
                format!("{TYPE_GOOGLEAPIS_PREFIX}{type_url}")
            },
            value: payload_value,
        }),
        external_origin: "eve-client".into(),
        application_instance_uuid: b"app-b11-0001".to_vec(),
        authoritative_context: None,
        trace_context: None,
    };
    let rep = rpc(
        ctx,
        sess,
        "publicGatewaySvc",
        "SendRequest",
        &[PyRep::Bytes(req.encode_proto())],
    )
    .await
    .expect("publicGatewaySvc.SendRequest");
    if let PyRep::KeyVal(entries) = &rep {
        for (k, v) in entries {
            if k == "response_proto" {
                if let PyRep::Bytes(b) = v {
                    return EvePublicResponse::decode_proto(b).expect("decode EvePublicResponse");
                }
            }
        }
    }
    panic!("expected response_proto KeyVal, got {rep:?}");
}

fn flush_and_drain_notices(
    ctx: &ServerContext,
    rx: &mut Receiver<EvePublicNotice>,
) -> Vec<EvePublicNotice> {
    let queued: Vec<_> = ctx
        .quasar_gateway
        .pending_notices
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .drain(..)
        .collect();
    for (scope, payload) in queued {
        let _ = ctx
            .quasar_gateway
            .publish_notice(scope, payload, ctx.now_unix());
    }
    let mut out = Vec::new();
    for _ in 0..512 {
        match rx.try_recv() {
            Ok(msg) => out.push(msg),
            Err(_) => break,
        }
    }
    out
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ObservableWorldState {
    p1_balance: i64,
    p2_balance: i64,
    p1_corp_id: i64,
    corp_count: usize,
    item_count: usize,
    journal_len: usize,
    mining_ledger_len: usize,
    mining_total_qty: i64,
    custom_names: Vec<(i64, String)>,
    contacts: Vec<(i64, i64)>,
    blobs_digest: Vec<(String, String)>,
    esi_ship_name: Option<String>,
}

pub(crate) fn capture_observable_state(ctx: &ServerContext) -> ObservableWorldState {
    let esi_ship_name = ctx.esi_state.get_custom_item_name(100_000_001);
    ctx.db.with_snapshot(|snap| {
        let mut custom_names: Vec<(i64, String)> = snap
            .item_custom_names
            .iter()
            .filter(|(&id, _)| id > 0)
            .map(|(&id, v)| (id, v.clone()))
            .collect();
        custom_names.sort_by_key(|(id, _)| *id);

        let mut contacts: Vec<(i64, i64)> = snap
            .char_contacts
            .get(&P1)
            .map(|list| {
                list.iter()
                    .map(|c| (c.contact_id, (c.standing * 100.0) as i64))
                    .collect()
            })
            .unwrap_or_default();
        contacts.sort_by_key(|(id, _)| *id);

        let mut blobs_digest: Vec<(String, String)> = snap
            .service_state_blobs
            .iter()
            .filter(|(k, _)| {
                !k.starts_with("season_manager:")
                    && *k != "market_seeder_version"
                    && *k != "nes_store_catalog"
            })
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        blobs_digest.sort_by(|a, b| a.0.cmp(&b.0));

        ObservableWorldState {
            p1_balance: snap.characters.get(&P1).map(|c| c.balance.0).unwrap_or(0),
            p2_balance: snap.characters.get(&P2).map(|c| c.balance.0).unwrap_or(0),
            p1_corp_id: snap
                .characters
                .get(&P1)
                .map(|c| c.corporation_id)
                .unwrap_or(0),
            corp_count: snap.corporations.len(),
            item_count: snap.items.len(),
            journal_len: snap.wallet_journal.len(),
            mining_ledger_len: snap.mining_ledger.len(),
            mining_total_qty: snap.mining_ledger.iter().map(|e| e.quantity).sum(),
            custom_names,
            contacts,
            blobs_digest,
            esi_ship_name,
        }
    })
}

#[tokio::test]
async fn test_invariant_atomic_rollback_randomized_fault_injection() {
    let db_path = temp_sqlite_path("b11_atomic_rollback");
    let repo = Arc::new(WorldRepository::open_sqlite(&db_path).expect("open_sqlite"));
    let ctx = Arc::new(ServerContext::with_repository(repo));
    let demo = WorldRepository::with_demo_seed().with_snapshot(|s| s.clone());
    ctx.db.with_mut_snapshot(|snap| *snap = demo);
    common::pin_clock(&ctx);

    let mut p1 = select_char(&ctx, 72_010, "demo", "demo-secret", P1).await;
    let mut rng_state: u64 = 0xB11A_701C_5EED_0042;

    for step in 0..40u64 {
        rng_state ^= rng_state << 13;
        rng_state ^= rng_state >> 7;
        rng_state ^= rng_state << 17;
        let op = rng_state % 8;
        let pre = capture_observable_state(&ctx);

        match op {
            0 => {
                rpc(
                    &ctx,
                    &mut p1,
                    "account",
                    "GiveCash",
                    &[
                        PyRep::Integer(P2),
                        PyRep::Real(100.0),
                        PyRep::String(format!("step-{step}")),
                    ],
                )
                .await
                .expect("valid GiveCash");
                let post = capture_observable_state(&ctx);
                assert_eq!(post.p1_balance, pre.p1_balance - 10_000);
                assert_eq!(post.p2_balance, pre.p2_balance + 10_000);
            }
            1 => {
                let name = format!("Ship-{step}");
                rpc(
                    &ctx,
                    &mut p1,
                    "invbroker",
                    "SetLabel",
                    &[PyRep::Integer(100_000_001), PyRep::String(name.clone())],
                )
                .await
                .expect("valid SetLabel");
                assert_eq!(capture_observable_state(&ctx).esi_ship_name, Some(name));
            }
            2 => {
                assert!(
                    rpc(
                        &ctx,
                        &mut p1,
                        "account",
                        "GiveCash",
                        &[
                            PyRep::Integer(P2),
                            PyRep::Real(999_999_999_999.0),
                            PyRep::String("overdraft".into()),
                        ],
                    )
                    .await
                    .is_err()
                );
                assert_eq!(capture_observable_state(&ctx), pre);
            }
            3 => {
                assert!(
                    rpc(
                        &ctx,
                        &mut p1,
                        "corpRegistry",
                        "AddCorporation",
                        &[
                            PyRep::String("".into()),
                            PyRep::String("BAD".into()),
                            PyRep::String("invalid".into()),
                        ],
                    )
                    .await
                    .is_err()
                );
                assert_eq!(capture_observable_state(&ctx), pre);
            }
            4 => {
                assert!(
                    rpc(
                        &ctx,
                        &mut p1,
                        "charMgr",
                        "AddContact",
                        &[PyRep::Integer(P1), PyRep::Real(5.0)],
                    )
                    .await
                    .is_err()
                );
                assert_eq!(capture_observable_state(&ctx), pre);
            }
            5 => {
                assert!(
                    rpc(
                        &ctx,
                        &mut p1,
                        "invbroker",
                        "SetLabel",
                        &[
                            PyRep::Integer(100_000_002),
                            PyRep::String("Hijacked".into()),
                        ],
                    )
                    .await
                    .is_err()
                );
                assert_eq!(capture_observable_state(&ctx), pre);
            }
            6 => {
                let err_res: Result<(), BridgeRpcError> = ctx.db.try_with_mut_snapshot(|snap| {
                    if let Some(ch) = snap.characters.get_mut(&P1) {
                        ch.balance = IskCents(ch.balance.0 - 500_000);
                    }
                    snap.mining_ledger.push(MiningLedgerRecord {
                        character_id: P1,
                        solar_system_id: 30_000_142,
                        type_id: 1230,
                        quantity: 7777,
                        date: "2026-10-31".into(),
                        timestamp_unix: ctx.now_unix(),
                    });
                    snap.service_state_blobs
                        .insert(format!("fault_blob_{step}"), "partial".into());
                    Err(BridgeRpcError {
                        code: "InjectedHandlerFault".into(),
                        message: "Simulated mid-transaction failure".into(),
                    })
                });
                assert!(err_res.is_err());
                assert_eq!(capture_observable_state(&ctx), pre);
            }
            _ => {
                ctx.db.with_mut_snapshot(|snap| {
                    snap.mining_ledger.push(MiningLedgerRecord {
                        character_id: P1,
                        solar_system_id: 30_000_142,
                        type_id: 1230,
                        quantity: 50,
                        date: "2026-10-31".into(),
                        timestamp_unix: ctx.now_unix(),
                    });
                });
                assert_eq!(
                    capture_observable_state(&ctx).mining_ledger_len,
                    pre.mining_ledger_len + 1
                );
            }
        }
    }

    // Persistence-layer fault injection via SQLite BEFORE UPDATE / BEFORE INSERT triggers
    {
        let raw_conn = rusqlite::Connection::open(&db_path).expect("open sqlite for trigger");
        raw_conn
            .execute_batch(
                "CREATE TRIGGER fail_char_persist BEFORE UPDATE ON characters \
                 BEGIN SELECT RAISE(ABORT, 'injected_disk_persist_failure'); END; \
                 CREATE TRIGGER fail_contact_ins BEFORE INSERT ON char_contacts \
                 BEGIN SELECT RAISE(ABORT, 'injected_contact_persist_failure'); END; \
                 CREATE TRIGGER fail_contact_upd BEFORE UPDATE ON char_contacts \
                 BEGIN SELECT RAISE(ABORT, 'injected_contact_persist_failure'); END; \
                 CREATE TRIGGER fail_label_ins BEFORE INSERT ON item_custom_names \
                 BEGIN SELECT RAISE(ABORT, 'injected_label_persist_failure'); END; \
                 CREATE TRIGGER fail_label_upd BEFORE UPDATE ON item_custom_names \
                 BEGIN SELECT RAISE(ABORT, 'injected_label_persist_failure'); END;",
            )
            .expect("install fault trigger");
    }
    let pre_persist_fault = capture_observable_state(&ctx);

    // 1. Dispatch real mutating RPCs while SQLite triggers reject persistence commits
    let fault_cash = rpc(
        &ctx,
        &mut p1,
        "account",
        "GiveCash",
        &[PyRep::Integer(P2), PyRep::Real(5_000.0), PyRep::String("fault_cash".into())],
    )
    .await;
    assert!(fault_cash.is_err());
    assert_eq!(capture_observable_state(&ctx), pre_persist_fault);

    let fault_contact = rpc(
        &ctx,
        &mut p1,
        "charMgr",
        "AddContact",
        &[PyRep::Integer(90_000_003), PyRep::Real(10.0), PyRep::Bool(true)],
    )
    .await;
    assert!(fault_contact.is_err());
    assert_eq!(capture_observable_state(&ctx), pre_persist_fault);

    let fault_label = rpc(
        &ctx,
        &mut p1,
        "invbroker",
        "SetLabel",
        &[PyRep::Integer(100_000_001), PyRep::String("Uncommitted Ship Name".into())],
    )
    .await;
    assert!(fault_label.is_err());
    assert_eq!(capture_observable_state(&ctx), pre_persist_fault);

    // 2. Direct multi-store snapshot transaction fault injection
    let persist_fault_res: Result<(), BridgeRpcError> = ctx.db.try_with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&P1) {
            ch.balance = IskCents(ch.balance.0 - 250_000);
        }
        snap.mining_ledger.push(MiningLedgerRecord {
            character_id: P1,
            solar_system_id: 30_000_142,
            type_id: 1228,
            quantity: 9_999,
            date: "2026-10-31".into(),
            timestamp_unix: ctx.now_unix(),
        });
        snap.service_state_blobs
            .insert("uncommitted_blob".into(), "must_rollback".into());
        Ok(())
    });
    assert!(persist_fault_res.is_err());
    assert_eq!(capture_observable_state(&ctx), pre_persist_fault);
    {
        let raw_conn = rusqlite::Connection::open(&db_path).expect("open sqlite to drop trigger");
        raw_conn
            .execute_batch(
                "DROP TRIGGER fail_char_persist; \
                 DROP TRIGGER fail_contact_ins; \
                 DROP TRIGGER fail_contact_upd; \
                 DROP TRIGGER fail_label_ins; \
                 DROP TRIGGER fail_label_upd;",
            )
            .expect("drop fault trigger");
    }

    let repo2 = Arc::new(WorldRepository::open_sqlite(&db_path).expect("reopen sqlite"));
    assert_eq!(
        repo2.with_snapshot(|s| s.service_state_blobs.clone()),
        ctx.db.with_snapshot(|s| s.service_state_blobs.clone())
    );
    assert!(repo2.with_snapshot(|s| !s.service_state_blobs.contains_key("uncommitted_blob")));
    let ctx2 = ServerContext::with_repository(repo2);
    assert_eq!(capture_observable_state(&ctx2), pre_persist_fault);

    cleanup_sqlite_path(&db_path);
}

#[tokio::test]
async fn test_quasar_plex_character_and_user_gateway_domains() {
    // Hardened Quasar PLEX, character, and user flows (reopened pending full sub-feature coverage)

    let reg = QuasarDescriptorRegistry::open_default().expect("descriptor registry");
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let mut unauth = SessionState::new(72_040);
    for url in [
        "eve_public.plex.vault.api.BalanceRequest",
        "eve_public.character.GetNamesByUserRequest",
        "eve_public.user.license.api.GetRequest",
    ] {
        assert_eq!(
            quasar_rpc(&ctx, &mut unauth, url, vec![]).await.status_code,
            STATUS_UNAUTHORIZED
        );
    }

    let mut p1 = select_char(&ctx, 72_041, "demo", "demo-secret", P1).await;
    let mut p2 = select_char(&ctx, 72_042, "other", "other-secret", P2).await;
    let mut rx1 = ctx
        .quasar_gateway
        .subscribe_session(&p1, vec![], ctx.now_unix());

    // 1. feat:quasar.plex
    assert_eq!(
        quasar_rpc(&ctx, &mut p1, "eve_public.plex.vault.api.BalanceRequest", vec![])
            .await
            .status_code,
        STATUS_OK
    );
    let plex_item = ctx
        .db
        .add_or_stack_item(P1, JITA_4_4, InventoryFlag::Hangar, 44_992, 40)
        .expect("seed plex")
        .item_id;
    rpc(
        &ctx,
        &mut p1,
        "invbroker",
        "DepositPlexToVault",
        &[
            PyRep::Integer(JITA_4_4),
            PyRep::Integer(plex_item),
            PyRep::Integer(40),
        ],
    )
    .await
    .expect("DepositPlexToVault");
    let plex_notices = flush_and_drain_notices(&ctx, &mut rx1);
    assert!(plex_notices.iter().any(|n| {
        n.payload
            .as_ref()
            .is_some_and(|p| p.type_url.contains("plex.vault.api.BalanceChangedNotice"))
    }));

    assert_eq!(
        quasar_rpc(
            &ctx,
            &mut p1,
            "eve_public.plex.vault.transaction.api.GetAllLoggedForUserRequest",
            vec![]
        )
        .await
        .status_code,
        STATUS_OK
    );
    let tx_id = ctx.db.with_snapshot(|snap| {
        eve_server::services::quasar_payment_store::get_plex_transactions(snap)
            .into_iter()
            .find(|t| t.user_id == 1)
            .expect("user 1 plex tx")
            .id
    });
    let (mut b_id, mut log_req) = (Vec::new(), Vec::new());
    encode_varint_field(1, tx_id, &mut b_id);
    encode_bytes_field(1, &b_id, &mut log_req);
    assert_eq!(
        quasar_rpc(
            &ctx,
            &mut p1,
            "eve_public.plex.vault.transaction.api.GetLogRequest",
            log_req.clone()
        )
        .await
        .status_code,
        STATUS_OK
    );
    let log_url = "eve_public.plex.vault.transaction.api.GetLogRequest";
    assert_eq!(quasar_rpc(&ctx, &mut p2, log_url, log_req.clone()).await.status_code, STATUS_NOT_FOUND);

    // 2. feat:quasar.character
    let names_rsp = quasar_rpc(&ctx, &mut p1, "eve_public.character.GetNamesByUserRequest", vec![]).await;
    assert_eq!(names_rsp.status_code, STATUS_OK);
    assert!(!names_rsp.payload.expect("names").value.is_empty());
    let sp_create_url = "eve_public.character.skill.plan.CreateRequest";
    assert_eq!(quasar_rpc(&ctx, &mut p1, sp_create_url, vec![]).await.status_code, STATUS_BAD_REQUEST);

    let (mut attr_b, mut req_b, mut tid_b, mut create_b) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    encode_string_field(1, "B11 Gunnery Plan", &mut attr_b);
    encode_varint_field(1, 3300, &mut tid_b);
    encode_bytes_field(1, &tid_b, &mut req_b);
    encode_varint_field(2, 4, &mut req_b);
    encode_bytes_field(2, &req_b, &mut attr_b);
    encode_string_field(3, "Gunnery IV milestone plan", &mut attr_b);
    encode_bytes_field(1, &attr_b, &mut create_b);

    let sp_create = quasar_rpc(&ctx, &mut p1, sp_create_url, create_b).await;
    assert_eq!(sp_create.status_code, STATUS_OK);
    let sp_wm = WireMessage::decode(&sp_create.payload.expect("sp create").value).expect("wm");
    let id_b = sp_wm.fields.iter().find(|f| f.field_number == 1).expect("id").value.as_bytes().expect("bytes");
    let sp_id_wm = WireMessage::decode(id_b).expect("id wm");
    let uuid_b = sp_id_wm.fields.iter().find(|f| f.field_number == 1).expect("uuid").value.as_bytes().expect("uuid bytes");
    let plan_uuid: [u8; 16] = uuid_b.try_into().expect("16b uuid");
    let (mut id_inner, mut plan_id_req) = (Vec::new(), Vec::new());
    encode_bytes_field(1, &plan_uuid, &mut id_inner);
    encode_bytes_field(1, &id_inner, &mut plan_id_req);

    let sp_active_url = "eve_public.character.skill.plan.SetActiveRequest";
    let sp_ms_url = "eve_public.character.skill.plan.milestone.GetAllRequest";
    assert_eq!(quasar_rpc(&ctx, &mut p1, sp_active_url, plan_id_req.clone()).await.status_code, STATUS_OK);
    assert_eq!(quasar_rpc(&ctx, &mut p1, sp_ms_url, plan_id_req.clone()).await.status_code, STATUS_OK);

    // 3. feat:quasar.user
    let now = ctx.now_unix();
    let expected_exp = (now + 30 * 86_400).to_string();
    activate_user_omega_license(&ctx.db, &ctx.quasar_gateway.pending_notices, 1, 30 * 86_400, now);
    let lic_notices = flush_and_drain_notices(&ctx, &mut rx1);
    assert!(lic_notices.iter().any(|n| n.payload.as_ref().is_some_and(|p| p.type_url.contains("user.license.api.ActivatedNotice"))));
    let lic_url = "eve_public.user.license.api.GetRequest";
    let lic_active = quasar_rpc(&ctx, &mut p1, lic_url, vec![]).await;
    assert_eq!(lic_active.status_code, STATUS_OK);
    let lic_dyn = reg.decode_dynamic("eve_public.user.license.api.GetResponse", &lic_active.payload.expect("license").value).expect("decode");
    assert!(format!("{lic_dyn:?}").contains(&expected_exp));

    let (mut doc_id, mut accept_req) = (Vec::new(), Vec::new());
    encode_string_field(1, "privacy_policy_2026", &mut doc_id);
    encode_bytes_field(3, &doc_id, &mut accept_req);
    assert_eq!(
        quasar_rpc(&ctx, &mut p1, "eve.user.legal.agreement.api.AcceptRequest", accept_req)
            .await
            .status_code,
        STATUS_OK
    );
    let latest_eula = quasar_rpc(&ctx, &mut p1, "eve.legal.agreement.api.GetLatestRequest", vec![]).await;
    assert_eq!(latest_eula.status_code, STATUS_OK);
    let eula_wm = WireMessage::decode(&latest_eula.payload.expect("latest eula").value).expect("eula wm");
    let eula_doc = WireMessage::decode(
        eula_wm.fields.iter().find(|f| f.field_number == 1).expect("doc").value.as_bytes().expect("bytes"),
    )
    .expect("doc wm");
    let eula_id = std::str::from_utf8(
        eula_doc.fields.iter().find(|f| f.field_number == 2).expect("field 2").value.as_bytes().expect("bytes"),
    )
    .expect("utf8");
    assert_eq!(eula_id, "privacy_policy_2026");

    let ctx_reloaded = persist_and_reopen_sqlite(&ctx, "b11_quasar_domains_reload");
    let mut p1_re = select_char(&ctx_reloaded, 72_043, "demo", "demo-secret", P1).await;
    assert_eq!(quasar_rpc(&ctx_reloaded, &mut p1_re, log_url, log_req).await.status_code, STATUS_OK);
    let sp_get_url = "eve_public.character.skill.plan.GetRequest";
    assert_eq!(quasar_rpc(&ctx_reloaded, &mut p1_re, sp_get_url, plan_id_req).await.status_code, STATUS_OK);
    let lic_reloaded = quasar_rpc(&ctx_reloaded, &mut p1_re, lic_url, vec![]).await;
    assert_eq!(lic_reloaded.status_code, STATUS_OK);
    let lic_re_dyn = reg.decode_dynamic("eve_public.user.license.api.GetResponse", &lic_reloaded.payload.expect("reloaded").value).expect("decode");
    assert!(format!("{lic_re_dyn:?}").contains(&expected_exp));

    let mut rx_re = ctx_reloaded.quasar_gateway.subscribe_session(&p1_re, vec![], ctx_reloaded.now_unix());
    expire_user_omega_license(&ctx_reloaded.db, &ctx_reloaded.quasar_gateway.pending_notices, 1);
    let exp_notices = flush_and_drain_notices(&ctx_reloaded, &mut rx_re);
    assert!(exp_notices.iter().any(|n| n.payload.as_ref().is_some_and(|p| p.type_url.contains("user.license.api.ExpiredNotice"))));
    assert_eq!(ctx_reloaded.db.with_snapshot(|s| s.characters.get(&P1).expect("P1").clone_grade), 1);
}

}

#[allow(unused_imports, dead_code, unused_variables, unused_mut)]
pub(crate) mod cross_protocol_single_source_of_truth {
    use super::*;

// Integration test for `feat:invariant.single-source-of-truth`:
// Contacts, mail labels & deleted mail, custom item names, CSPA charges, war records,
// alliances, and PLEX balances read identically across MachoNet client RPC, ESI, and
// Quasar live and after SQLite restart.


use crate::common;
use crate::features_fleet_broadcast_rpcs::atomic_rollback_and_quasar_gateway_domains::{
    cleanup_sqlite_path, quasar_rpc, rpc, select_char, temp_sqlite_path,
};
use eve_db::{
    AllianceLifecycleMeta, AllianceRecord, CorporationRecord, InventoryFlag, InventoryItemRecord,
    IskCents, WarRecord, WorldRepository,
};
use eve_machonet::SessionState;
use eve_proto::quasar::{encode_bytes_field, encode_varint_field, QuasarDescriptorRegistry};
use eve_proto::PyRep;
use eve_server::esi::{EsiHttpRequest, EsiRouter};
use eve_server::quasar_gateway::STATUS_OK;
use eve_server::router::ServerContext;
use std::sync::Arc;

const P1: i64 = 90_000_001;
const P2: i64 = 90_000_002;
const JITA_4_4: i64 = 60_003_760;

const PKCE_VERIFIER: &str = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
const PKCE_CHALLENGE: &str = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";

fn issue_token(
    ctx: &ServerContext,
    user: &str,
    pass: &str,
    char_id: i64,
    scopes: &[&str],
) -> String {
    let now = ctx.now_unix();
    let code = ctx
        .db
        .authorize_pkce(
            user,
            pass,
            char_id,
            "evomu-b11-ssot",
            "https://localhost/callback",
            scopes.iter().map(|s| s.to_string()).collect(),
            PKCE_CHALLENGE,
            "S256",
            now,
        )
        .expect("authorize_pkce");
    ctx.db
        .exchange_pkce_code(&code, "evomu-b11-ssot", PKCE_VERIFIER, now)
        .expect("exchange_pkce_code")
        .access_token
}

fn esi_call(
    router: &EsiRouter,
    method: &str,
    path: &str,
    token: Option<&str>,
    body: Option<Vec<u8>>,
) -> serde_json::Value {
    let mut req = EsiHttpRequest::new(method, path);
    if let Some(t) = token {
        req = req.with_bearer_token(t);
    }
    if let Some(b) = body {
        req.body = b;
    }
    let resp = router.handle_request(&req);
    assert!(
        matches!(resp.status_code, 200 | 201),
        "ESI {method} {path} failed ({}): {}",
        resp.status_code,
        String::from_utf8_lossy(&resp.body)
    );
    serde_json::from_slice(&resp.body).expect("valid ESI JSON")
}

#[tokio::test]
async fn test_invariant_single_source_of_truth_client_esi_quasar_and_restart() {
    let db_path = temp_sqlite_path("b11_ssot");
    let corp_id = 98_200_001i64;
    let rival_corp_id = 98_200_002i64;
    let alliance_id = 99_200_001i64;
    let war_id = 88_001i64;
    let ship_id = 100_000_001i64;
    let plex_stack_id = 100_777_001i64;

    let (mail_deleted, mail_kept, mail_label_id) = {
        let repo = Arc::new(WorldRepository::open_sqlite(&db_path).expect("open sqlite 1"));
        let ctx = Arc::new(ServerContext::with_repository(repo));
        let demo = WorldRepository::with_demo_seed().with_snapshot(|s| s.clone());
        ctx.db.with_mut_snapshot(|snap| {
            *snap = demo;
            snap.corporations.insert(
                corp_id,
                { let mut __corp = CorporationRecord::default();
                    __corp.corporation_id = corp_id;
                    __corp.name = "B11 SSOT Corp".into();
                    __corp.ticker = "BSOT".into();
                    __corp.ceo_id = P1;
                    __corp.alliance_id = Some(alliance_id);
                    __corp.hq_station_id = Some(JITA_4_4);
                    __corp.members = [P1].into_iter().collect();
                    __corp },
            );
            snap.corporations.insert(
                rival_corp_id,
                { let mut __corp = CorporationRecord::default();
                    __corp.corporation_id = rival_corp_id;
                    __corp.name = "B11 Rival Corp".into();
                    __corp.ticker = "BRVL".into();
                    __corp.ceo_id = P2;
                    __corp.hq_station_id = Some(JITA_4_4);
                    __corp.members = [P2].into_iter().collect();
                    __corp },
            );
            snap.alliances.insert(
                alliance_id,
                AllianceRecord {
                    alliance_id,
                    name: "B11 Unified Alliance".into(),
                    short_name: "B11UA".into(),
                    executor_corp_id: corp_id,
                    member_corp_ids: [corp_id].into_iter().collect(),
                },
            );
            snap.alliance_lifecycle_meta.insert(
                alliance_id,
                AllianceLifecycleMeta {
                    creator_id: P1,
                    creator_corporation_id: corp_id,
                    date_founded_unix: 1_700_000_000,
                    faction_id: None,
                },
            );
            snap.wars.insert(
                war_id,
                WarRecord {
                    war_id,
                    aggressor_corp_id: corp_id,
                    defender_corp_id: rival_corp_id,
                    mutual: false,
                    started_unix: 1_700_000_000,
                    active: true,
                },
            );
            if let Some(c1) = snap.characters.get_mut(&P1) {
                c1.corporation_id = corp_id;
                c1.alliance_id = Some(alliance_id);
                c1.balance = IskCents(50_000_000_00);
            }
            if let Some(c2) = snap.characters.get_mut(&P2) {
                c2.corporation_id = rival_corp_id;
            }
            snap.items.insert(
                plex_stack_id,
                InventoryItemRecord {
                    item_id: plex_stack_id,
                    type_id: 44_992,
                    owner_id: P1,
                    location_id: JITA_4_4,
                    flag: InventoryFlag::Hangar,
                    quantity: 200,
                    singleton: false,
                    version: 1,
                    ..Default::default()
                },
            );
        });
        common::pin_clock(&ctx);

        let mut s1 = select_char(&ctx, 72_020, "demo", "demo-secret", P1).await;
        let mut s2 = select_char(&ctx, 72_021, "other", "other-secret", P2).await;

        rpc(
            &ctx,
            &mut s1,
            "account",
            "SetContactCost",
            &[PyRep::Integer(25_000)],
        )
        .await
        .expect("P1 SetContactCost");
        rpc(
            &ctx,
            &mut s2,
            "account",
            "SetContactCost",
            &[PyRep::Integer(12_500)],
        )
        .await
        .expect("P2 SetContactCost");
        rpc(
            &ctx,
            &mut s1,
            "charMgr",
            "AddContact",
            &[PyRep::Integer(P2), PyRep::Real(8.0), PyRep::Bool(true)],
        )
        .await
        .expect("AddContact");

        let lbl_id = rpc(
            &ctx,
            &mut s1,
            "mailMgr",
            "CreateLabel",
            &[
                PyRep::String("PriorityOps".into()),
                PyRep::String("#ffaa00".into()),
            ],
        )
        .await
        .expect("mailMgr.CreateLabel")
        .as_i64()
        .expect("mail label id");
        let m_del = rpc(
            &ctx,
            &mut s1,
            "mailMgr",
            "SendMail",
            &[
                PyRep::List(vec![PyRep::Integer(P1)]),
                PyRep::String("Purged Dispatch".into()),
                PyRep::String("Delete me".into()),
                PyRep::Integer(0),
            ],
        )
        .await
        .expect("SendMail 1")
        .as_i64()
        .expect("m_del");
        let m_keep = rpc(
            &ctx,
            &mut s1,
            "mailMgr",
            "SendMail",
            &[
                PyRep::List(vec![PyRep::Integer(P1)]),
                PyRep::String("Retained Dispatch".into()),
                PyRep::String("Keep me".into()),
                PyRep::Integer(0),
            ],
        )
        .await
        .expect("SendMail 2")
        .as_i64()
        .expect("m_keep");
        rpc(
            &ctx,
            &mut s1,
            "mailMgr",
            "DeleteMail",
            &[PyRep::List(vec![PyRep::Integer(m_del)])],
        )
        .await
        .expect("DeleteMail");

        rpc(
            &ctx,
            &mut s1,
            "invbroker",
            "SetLabel",
            &[
                PyRep::Integer(ship_id),
                PyRep::String("B11 Sovereign Hull".into()),
            ],
        )
        .await
        .expect("invbroker.SetLabel");

        rpc(
            &ctx,
            &mut s1,
            "invbroker",
            "DepositPlexToVault",
            &[
                PyRep::Integer(JITA_4_4),
                PyRep::Integer(plex_stack_id),
                PyRep::Integer(120),
                PyRep::Integer(0),
            ],
        )
        .await
        .expect("DepositPlexToVault");

        verify_all_seven_ssot_domains(
            &ctx,
            &mut s1,
            corp_id,
            rival_corp_id,
            alliance_id,
            war_id,
            ship_id,
            lbl_id,
            m_del,
            m_keep,
        )
        .await;

        (m_del, m_keep, lbl_id)
    };

    let repo2 = Arc::new(WorldRepository::open_sqlite(&db_path).expect("open sqlite 2"));
    let ctx2 = Arc::new(ServerContext::with_repository(repo2));
    common::pin_clock(&ctx2);
    let mut s1_re = select_char(&ctx2, 72_022, "demo", "demo-secret", P1).await;
    verify_all_seven_ssot_domains(
        &ctx2,
        &mut s1_re,
        corp_id,
        rival_corp_id,
        alliance_id,
        war_id,
        ship_id,
        mail_label_id,
        mail_deleted,
        mail_kept,
    )
    .await;

    cleanup_sqlite_path(&db_path);
}

#[allow(clippy::too_many_arguments)]
async fn verify_all_seven_ssot_domains(
    ctx: &Arc<ServerContext>,
    s1: &mut SessionState,
    corp_id: i64,
    rival_corp_id: i64,
    alliance_id: i64,
    war_id: i64,
    ship_id: i64,
    mail_label_id: i64,
    mail_deleted: i64,
    mail_kept: i64,
) {
    let router = EsiRouter::new(Arc::clone(ctx));
    let tok1 = issue_token(
        ctx,
        "demo",
        "demo-secret",
        P1,
        &[
            "esi-characters.read_contacts.v1",
            "esi-characters.cspa.v1",
            "esi-mail.read_mail.v1",
            "esi-assets.read_assets.v1",
        ],
    );

    // 1. Contacts
    let contacts_rpc = rpc(ctx, s1, "charMgr", "GetContacts", &[])
        .await
        .expect("GetContacts");
    assert!(format!("{contacts_rpc:?}").contains("90000002"));
    let contacts_esi = esi_call(
        &router,
        "GET",
        &format!("/latest/characters/{P1}/contacts/"),
        Some(&tok1),
        None,
    );
    assert!(contacts_esi.to_string().contains("90000002"));

    // 2. Mail labels & deleted mail
    let labels_rpc = rpc(ctx, s1, "mailMgr", "GetLabels", &[])
        .await
        .expect("mailMgr.GetLabels");
    assert!(format!("{labels_rpc:?}").contains("PriorityOps"));
    let labels_esi = esi_call(
        &router,
        "GET",
        &format!("/latest/characters/{P1}/mail/labels/"),
        Some(&tok1),
        None,
    );
    assert!(labels_esi["labels"]
        .as_array()
        .expect("labels array")
        .iter()
        .any(|l| l["label_id"].as_i64() == Some(mail_label_id) && l["name"] == "PriorityOps"));
    let labels_quasar = quasar_rpc(
        ctx,
        s1,
        "eve.contact.label.GetForCharacterRequest",
        vec![],
    )
    .await;
    assert_eq!(labels_quasar.status_code, STATUS_OK);
    let labels_bytes = labels_quasar.payload.expect("labels payload").value;
    assert!(!String::from_utf8_lossy(&labels_bytes).contains("PriorityOps"));
    let headers_rpc = rpc(ctx, s1, "mailMgr", "GetMailHeaders", &[])
        .await
        .expect("GetMailHeaders");
    let hdr_dbg = format!("{headers_rpc:?}");
    assert!(!hdr_dbg.contains(&format!("Integer({mail_deleted})")));
    assert!(hdr_dbg.contains(&format!("Integer({mail_kept})")));
    let mail_esi = esi_call(
        &router,
        "GET",
        &format!("/latest/characters/{P1}/mail/"),
        Some(&tok1),
        None,
    );
    let mail_str = mail_esi.to_string();
    assert!(!mail_str.contains("Purged Dispatch") && mail_str.contains("Retained Dispatch"));

    // 3. Custom item names
    let decoded_item = rpc(
        ctx,
        s1,
        "encodedItems",
        "DecodeItem",
        &[PyRep::Integer(ship_id)],
    )
    .await
    .expect("DecodeItem");
    assert_eq!(
        decoded_item.dict_get("customName").and_then(|v| v.as_str()),
        Some("B11 Sovereign Hull")
    );
    let names_esi = esi_call(
        &router,
        "POST",
        &format!("/latest/characters/{P1}/assets/names/"),
        Some(&tok1),
        Some(serde_json::to_vec(&vec![ship_id]).expect("json")),
    );
    assert_eq!(names_esi[0]["name"], "B11 Sovereign Hull");

    // 4. CSPA charges
    let p1_cspa = rpc(ctx, s1, "account", "GetDefaultContactCost", &[])
        .await
        .expect("GetDefaultContactCost");
    assert_eq!(p1_cspa.as_f64(), Some(25_000.0));
    let cspa_esi = esi_call(
        &router,
        "POST",
        &format!("/latest/characters/{P1}/cspa/"),
        Some(&tok1),
        Some(serde_json::to_vec(&vec![P2]).expect("json")),
    );
    assert_eq!(cspa_esi.as_f64(), Some(12_500.0));
    let cspa_q_p1 = quasar_rpc(ctx, s1, "eve.mail.cspa.GetRequest", vec![]).await;
    assert_eq!(cspa_q_p1.status_code, STATUS_OK);
    let cspa_wm_p1 = eve_proto::quasar::WireMessage::decode(&cspa_q_p1.payload.expect("cspa p1").value).expect("wm");
    let cspa_inner_p1 = eve_proto::quasar::WireMessage::decode(
        cspa_wm_p1.fields[0].value.as_bytes().expect("charge bytes"),
    )
    .expect("inner wm");
    assert_eq!(cspa_inner_p1.fields[0].value.as_varint(), Some(25_000));
    let (mut p2_id, mut p2_cspa_req) = (Vec::new(), Vec::new());
    encode_varint_field(1, P2 as u64, &mut p2_id);
    encode_bytes_field(1, &p2_id, &mut p2_cspa_req);
    let cspa_q_p2 = quasar_rpc(ctx, s1, "eve.mail.cspa.GetRequest", p2_cspa_req).await;
    assert_eq!(cspa_q_p2.status_code, STATUS_OK);
    let cspa_wm_p2 = eve_proto::quasar::WireMessage::decode(&cspa_q_p2.payload.expect("cspa p2").value).expect("wm");
    let cspa_inner_p2 = eve_proto::quasar::WireMessage::decode(
        cspa_wm_p2.fields[0].value.as_bytes().expect("charge bytes"),
    )
    .expect("inner wm");
    assert_eq!(cspa_inner_p2.fields[0].value.as_varint(), Some(12_500));

    // 5. War records
    let wars_rpc = rpc(ctx, s1, "warRegistry", "GetWars", &[PyRep::Integer(corp_id)])
        .await
        .expect("warRegistry.GetWars");
    assert!(format!("{wars_rpc:?}").contains(&war_id.to_string()));
    let war_esi = esi_call(
        &router,
        "GET",
        &format!("/latest/wars/{war_id}/"),
        None,
        None,
    );
    assert_eq!(war_esi["aggressor"]["corporation_id"], corp_id);
    assert_eq!(war_esi["defender"]["corporation_id"], rival_corp_id);
    let reg = QuasarDescriptorRegistry::open_default().expect("quasar registry");
    let wars_all_q = quasar_rpc(ctx, s1, "eve.war.GetAllRequest", vec![]).await;
    assert_eq!(wars_all_q.status_code, STATUS_OK);
    let wars_all_dyn = reg
        .decode_dynamic("eve.war.GetAllResponse", &wars_all_q.payload.expect("wars all").value)
        .expect("decode GetAllResponse");
    assert!(format!("{wars_all_dyn:?}").contains(&war_id.to_string()));
    let (mut war_id_b, mut war_get_req) = (Vec::new(), Vec::new());
    encode_varint_field(1, war_id as u64, &mut war_id_b);
    encode_bytes_field(1, &war_id_b, &mut war_get_req);
    let war_get_q = quasar_rpc(ctx, s1, "eve.war.GetRequest", war_get_req).await;
    assert_eq!(war_get_q.status_code, STATUS_OK);
    let war_wm = eve_proto::quasar::WireMessage::decode(&war_get_q.payload.expect("war get").value).expect("war wm");
    let war_attr = eve_proto::quasar::WireMessage::decode(war_wm.fields[0].value.as_bytes().expect("attr")).expect("attr wm");
    let agg_wm = eve_proto::quasar::WireMessage::decode(war_attr.fields[0].value.as_bytes().expect("agg")).expect("agg wm");
    let def_wm = eve_proto::quasar::WireMessage::decode(war_attr.fields[1].value.as_bytes().expect("def")).expect("def wm");
    assert_eq!(agg_wm.fields[0].value.as_varint(), Some(corp_id as u64));
    assert_eq!(def_wm.fields[0].value.as_varint(), Some(rival_corp_id as u64));

    // 6. Alliances
    let ally_rpc = rpc(
        ctx,
        s1,
        "allianceRegistry",
        "GetAlliance",
        &[PyRep::Integer(alliance_id)],
    )
    .await
    .expect("allianceRegistry.GetAlliance");
    assert!(format!("{ally_rpc:?}").contains("B11UA"));
    let ally_esi = esi_call(
        &router,
        "GET",
        &format!("/latest/alliances/{alliance_id}/"),
        None,
        None,
    );
    assert_eq!(ally_esi["ticker"], "B11UA");
    let mut char_seq = Vec::new();
    encode_varint_field(1, P1 as u64, &mut char_seq);
    let mut pres_req = Vec::new();
    encode_bytes_field(1, &char_seq, &mut pres_req);
    let pres_rsp = quasar_rpc(
        ctx,
        s1,
        "eve_public.presence.api.GetAllianceRequest",
        pres_req,
    )
    .await;
    assert_eq!(pres_rsp.status_code, STATUS_OK);
    let pres_dyn = reg
        .decode_dynamic(
            "eve_public.presence.api.GetAllianceResponse",
            &pres_rsp.payload.expect("presence payload").value,
        )
        .expect("decode GetAllianceResponse");
    assert!(format!("{pres_dyn:?}").contains(&alliance_id.to_string()));

    // 7. PLEX balances
    let plex_client = rpc(ctx, s1, "vaultManager", "get_account_balance", &[])
        .await
        .expect("vaultManager.get_account_balance")
        .as_i64()
        .expect("plex i64");
    assert_eq!(plex_client, 620);
    let plex_quasar = quasar_rpc(ctx, s1, "eve_public.plex.vault.api.BalanceRequest", vec![]).await;
    assert_eq!(plex_quasar.status_code, STATUS_OK);
    let bal_dyn = reg
        .decode_dynamic(
            "eve_public.plex.vault.api.BalanceResponse",
            &plex_quasar.payload.expect("balance payload").value,
        )
        .expect("decode BalanceResponse");
    assert!(format!("{bal_dyn:?}").contains("62000"));
}

#[tokio::test]
async fn test_b11_quasar_social_fail_closed_and_ssot_guards() {
    let ctx = ServerContext::new();
    let mut anon = SessionState::new(72_099);
    assert_eq!(
        quasar_rpc(&ctx, &mut anon, "eve.mail.cspa.GetRequest", vec![]).await.status_code,
        401
    );
    assert_eq!(
        quasar_rpc(&ctx, &mut anon, "eve.mail.CCPSendMailRequest", vec![]).await.status_code,
        401
    );

    let mut s1 = select_char(&ctx, 72_098, "demo", "demo-secret", P1).await;
    assert_eq!(
        quasar_rpc(&ctx, &mut s1, "eve.mail.CCPSendMailRequest", vec![]).await.status_code,
        403
    );

    let mut id_1001 = Vec::new();
    encode_varint_field(1, 1001, &mut id_1001);
    let mut war_1001 = Vec::new();
    eve_proto::quasar::encode_bytes_field(1, &id_1001, &mut war_1001);
    assert_eq!(quasar_rpc(&ctx, &mut s1, "eve.war.GetRequest", war_1001.clone()).await.status_code, 404);
    assert_eq!(quasar_rpc(&ctx, &mut s1, "eve.war.GetAlliesRequest", war_1001).await.status_code, 404);

    let empty_labels = quasar_rpc(&ctx, &mut s1, "eve.contact.label.GetForCharacterRequest", vec![]).await;
    assert_eq!(empty_labels.status_code, STATUS_OK);
    assert!(empty_labels.payload.expect("payload").value.is_empty());

    let mut id_p2 = Vec::new();
    encode_varint_field(1, P2 as u64, &mut id_p2);
    let mut other_char = Vec::new();
    eve_proto::quasar::encode_bytes_field(1, &id_p2, &mut other_char);
    assert_eq!(
        quasar_rpc(&ctx, &mut s1, "eve.contact.label.GetForCharacterRequest", other_char).await.status_code,
        403
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_b11_multithread_wal_error_survives_await_and_rolls_back() {
    let db_path = std::env::temp_dir().join(format!("b11_mt_wal_{}.sqlite", std::process::id()));
    let _ = std::fs::remove_file(&db_path);
    let mem_ctx = ServerContext::new();
    mem_ctx.db.save_to_sqlite(&db_path).expect("save_to_sqlite");
    let ctx = ServerContext::from_sqlite(&db_path).expect("from_sqlite");
    let mut sess = select_char(&ctx, 72_049, "demo", "demo-secret", P1).await;
    let before = crate::features_fleet_broadcast_rpcs::atomic_rollback_and_quasar_gateway_domains::capture_observable_state(&ctx);
    let conn = rusqlite::Connection::open(&db_path).expect("open sqlite");
    conn.execute_batch(
        "CREATE TRIGGER abort_char_upd BEFORE UPDATE ON characters BEGIN SELECT RAISE(ABORT, 'injected_mt_wal_fail'); END;",
    )
    .expect("trigger");
    drop(conn);
    let err = ctx
        .dispatch_rpc(
            &mut sess,
            "account",
            None,
            "GiveCash",
            &[PyRep::Integer(P2), PyRep::Real(100.0), PyRep::String("mt-test".into())],
            &std::collections::HashMap::new(),
            None,
        )
        .await
        .expect_err("must fail on WAL abort");
    assert!(
        err.code == "DatabasePersistenceError" || err.code == "PersistenceFailure",
        "unexpected error code: {}",
        err.code
    );
    assert!(err.message.contains("injected_mt_wal_fail"));
    tokio::task::yield_now().await;
    assert_eq!(crate::features_fleet_broadcast_rpcs::atomic_rollback_and_quasar_gateway_domains::capture_observable_state(&ctx), before);
    let _ = std::fs::remove_file(&db_path);
}

}
