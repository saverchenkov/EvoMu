//! End-to-end integration tests for Batch 29 open `feat:lifecycle.*` parity rows.

#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crate::common;
use crate::plan_c7_crimewatch_closure::{far_celestial_id, item_rec, setup_pilot_in_space};
use eve_db::{
    InventoryFlag, IskCents, TrainedSkillRecord, VirtualServerClock, WorldRepository,
};
use eve_destiny::MovementMode;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use eve_server::services::fleet_social::fleet_social_notify::{
    handle_fleet_member_disconnect, tick_fleet_lifecycle, FLEET_DISCONNECT_GRACE_SECS,
    FLEET_INVITE_EXPIRY_SECS,
};
use glam::DVec3;

const PILOT_1: i64 = 90_000_001;
const PILOT_2: i64 = 90_000_002;
const PILOT_3: i64 = 90_000_003;
const PILOT_4: i64 = 90_000_004;

fn temp_sqlite_path(tag: &str) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let seq = NEXT.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let p = std::env::temp_dir().join(format!("evomu_b29_{tag}_{pid}_{seq}.sqlite"));
    let _ = std::fs::remove_file(&p);
    p
}

fn open_ctx(db_path: &Path, now_unix: u64) -> (ServerContext, Arc<VirtualServerClock>) {
    let exists = db_path.exists();
    let repo = Arc::new(WorldRepository::open_sqlite(db_path).expect("open_sqlite"));
    if !exists {
        let seed_snap = WorldRepository::with_demo_seed().with_snapshot(|s| s.clone());
        repo.with_mut_snapshot(|snap| *snap = seed_snap);
    }
    let clock = Arc::new(VirtualServerClock::with_fixed_base(now_unix));
    repo.set_clock(clock.clone());
    let ctx = ServerContext::with_repository(repo);
    ctx.set_clock(clock.clone());
    (ctx, clock)
}

async fn login_select(
    ctx: &ServerContext,
    sid: u64,
    user: &str,
    pass: &str,
    char_id: i64,
) -> SessionState {
    let mut sess = SessionState::new(sid);
    common::login_pilot(ctx, &mut sess, user, pass).await;
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &HashMap::new(),
        None,
    )
    .await
    .expect("SelectCharacterID");
    eve_server::services::space_destiny::clear_session_change_timer(ctx, char_id);
    sess
}

async fn rpc_res(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, svc, None, method, args, &HashMap::new(), None)
        .await
}

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    method: &str,
    args: &[PyRep],
) -> PyRep {
    rpc_res(ctx, sess, svc, method, args)
        .await
        .unwrap_or_else(|e| panic!("{svc}.{method} failed: {} {}", e.code, e.message))
}

fn pyrep_contains_int(rep: &PyRep, target: i64) -> bool {
    match rep {
        PyRep::Integer(v) => *v == target,
        PyRep::Tuple(items) | PyRep::List(items) => {
            items.iter().any(|x| pyrep_contains_int(x, target))
        }
        PyRep::Dict(entries) => entries
            .iter()
            .any(|(k, v)| pyrep_contains_int(k, target) || pyrep_contains_int(v, target)),
        PyRep::KeyVal(entries) => entries.iter().any(|(_, v)| pyrep_contains_int(v, target)),
        PyRep::Rowset { rows, .. } => rows
            .iter()
            .any(|row| row.iter().any(|x| pyrep_contains_int(x, target))),
        PyRep::Object { args, .. } => pyrep_contains_int(args, target),
        _ => false,
    }
}

fn session_changed_has_field(notifs: &[eve_server::router::PendingMachoNotification], field: &str, expected: i64) -> bool {
    notifs.iter().filter(|n| n.event == "OnSessionChanged").any(|n| {
        n.args.iter().any(|arg| match arg {
            PyRep::Dict(entries) => entries.iter().any(|(k, v)| {
                k.as_str() == Some(field)
                    && matches!(v, PyRep::Tuple(pair) if pair.get(1) == Some(&PyRep::Integer(expected)))
            }),
            PyRep::KeyVal(entries) => entries.get(field).is_some_and(|v| {
                matches!(v, PyRep::Tuple(pair) if pair.get(1) == Some(&PyRep::Integer(expected)))
            }),
            _ => false,
        })
    })
}

#[tokio::test]
async fn test_b29_corp_governance_durability_and_persistence() {
    // parity: feat:lifecycle.corp-governance-durable
    // parity: feat:lifecycle.corp-governance-persist
    eve_testkit::attest(&[
        "feat:lifecycle.corp-governance-durable",
        "feat:lifecycle.corp-governance-persist",
    ]);

    let db_path = temp_sqlite_path("corp_gov");
    let t0 = 1_775_000_000u64;
    let hangar_role: u64 = 1u64 << 13;

    let (corp_id, medal_id) = {
        let (ctx, _clock) = open_ctx(&db_path, t0);
        ctx.db.with_mut_snapshot(|snap| {
            snap.skills.entry(PILOT_1).or_default().insert(
                3363,
                TrainedSkillRecord { type_id: 3363, level: 5, skill_points: 256_000.0 },
            );
            snap.characters.get_mut(&PILOT_1).unwrap().balance = IskCents::from_isk(5_000_000_000);
        });
        let mut sess1 = login_select(&ctx, 9101, "demo", "demo-secret", PILOT_1).await;
        let add_corp = rpc(
            &ctx,
            &mut sess1,
            "corpRegistry",
            "AddCorporation",
            &[PyRep::String("B29 Durable Corp".into()), PyRep::String("B29D".into()), PyRep::Real(0.05)],
        )
        .await;
        let corp_id = add_corp.dict_get("corporationID").and_then(|v| v.as_i64()).or_else(|| add_corp.as_i64()).unwrap();
        eve_server::services::space_destiny::clear_session_change_timer(&ctx, PILOT_1);

        let mut sess2 = login_select(&ctx, 9102, "other", "other-secret", PILOT_2).await;
        let app_res = rpc(&ctx, &mut sess2, "corpRegistry", "InsertApplication", &[PyRep::Integer(corp_id), PyRep::String("Join".into())]).await;
        let app_id = app_res
            .dict_get("applicationID")
            .and_then(|v| v.as_i64())
            .or_else(|| app_res.as_i64())
            .expect("applicationID");
        rpc(&ctx, &mut sess1, "corpRegistry", "UpdateApplicationOffer", &[PyRep::Integer(app_id), PyRep::String("Accepted".into())]).await;

        rpc(
            &ctx,
            &mut sess1,
            "corpRegistry",
            "UpdateTitle",
            &[PyRep::Integer(1), PyRep::String("Quartermaster".into()), PyRep::Integer(hangar_role as i64), PyRep::Integer(0), PyRep::Integer(0), PyRep::Integer(0)],
        )
        .await;
        rpc(
            &ctx,
            &mut sess1,
            "corpRegistry",
            "UpdateMember",
            &[
                PyRep::Integer(PILOT_2),
                PyRep::Integer((hangar_role | eve_db::corp_roles::ROLE_ACCOUNTANT) as i64),
                PyRep::Integer(1),
            ],
        )
        .await;
        let medal_res = rpc(&ctx, &mut sess1, "corporationSvc", "CreateMedal", &[PyRep::String("B29 Star".into()), PyRep::String("Awarded".into()), PyRep::List(vec![])]).await;
        let medal_id = medal_res
            .as_i64()
            .or_else(|| medal_res.dict_get("medalID").and_then(|v| v.as_i64()))
            .expect("medal_id");
        rpc(&ctx, &mut sess1, "corporationSvc", "GiveMedalToCharacters", &[PyRep::Integer(medal_id), PyRep::List(vec![PyRep::Integer(PILOT_2)]), PyRep::String("Service".into())]).await;

        let _ = ctx.db.try_with_mut_snapshot(|snap| -> Result<(), String> {
            snap.corporations.get_mut(&corp_id).unwrap().ticker = "ABORT".into();
            Err("abort".to_string())
        });
        assert_eq!(ctx.db.with_snapshot(|s| s.corporations[&corp_id].ticker.clone()), "B29D");
        (corp_id, medal_id)
    };

    {
        let (ctx2, _clock2) = open_ctx(&db_path, t0 + 60);
        let mut sess1 = login_select(&ctx2, 9103, "demo", "demo-secret", PILOT_1).await;
        let mut sess2 = login_select(&ctx2, 9104, "other", "other-secret", PILOT_2).await;
        let emp = rpc(&ctx2, &mut sess1, "corporationSvc", "GetEmploymentRecord", &[PyRep::Integer(PILOT_2)]).await;
        assert!(pyrep_contains_int(&emp, corp_id));
        let audit = rpc(&ctx2, &mut sess1, "corpmgr", "AuditMember", &[PyRep::Integer(PILOT_2)]).await;
        assert!(
            pyrep_contains_int(&audit, PILOT_2)
                && pyrep_contains_int(&audit, (hangar_role | eve_db::corp_roles::ROLE_ACCOUNTANT) as i64)
        );
        let medals = rpc(&ctx2, &mut sess2, "corporationSvc", "GetMedalsReceived", &[PyRep::Integer(PILOT_2)]).await;
        assert!(pyrep_contains_int(&medals, medal_id) && pyrep_contains_int(&medals, PILOT_2));
        ctx2.db.with_snapshot(|snap| {
            let gov = snap.corp_governance.get(&corp_id).expect("corp_governance");
            assert_eq!(gov.titles.get(&1).map(|t| t.0.as_str()), Some("Quartermaster"));
            assert!(gov.pending_role_grants.contains_key(&PILOT_2));
            assert!(gov.medals.iter().any(|m| m.0 == medal_id));
            assert!(gov.medal_recipients.iter().any(|r| r.0 == medal_id && r.1 == PILOT_2));
            assert!(snap.corporations[&corp_id].members.contains(&PILOT_2));
        });
    }
    let _ = std::fs::remove_file(&db_path);
}

#[tokio::test]
async fn test_b29_corp_invite_join_kick_quit_lifecycle() {
    // parity: feat:lifecycle.corp-invite-consent
    // parity: feat:lifecycle.corp-join-leaves-old-corp
    // parity: feat:lifecycle.corp-kick-online-session
    // parity: feat:lifecycle.corp-quit-cleanup
    eve_testkit::attest(&[
        "feat:lifecycle.corp-invite-consent",
        "feat:lifecycle.corp-join-leaves-old-corp",
        "feat:lifecycle.corp-kick-online-session",
        "feat:lifecycle.corp-quit-cleanup",
    ]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    ctx.db.with_mut_snapshot(|snap| {
        snap.accounts.insert(
            "third_b29".to_string(),
            eve_db::AccountRecord {
                user_id: 30,
                username: "third_b29".to_string(),
                password_token: WorldRepository::hash_password_token("third_b29", "third-secret"),
                role: eve_db::service_roles::ROLE_PLAYER_DEFAULT,
                banned_until_unix: None,
                ban_reason: None,
            },
        );
        let mut ch3 = snap.characters.get(&PILOT_2).cloned().expect("PILOT_2");
        ch3.character_id = PILOT_3;
        ch3.user_id = 30;
        ch3.name = "Third B29".into();
        ch3.station_id = Some(60_003_760);
        snap.characters.insert(PILOT_3, ch3);

        if let Some(ch2) = snap.characters.get_mut(&PILOT_2) {
            ch2.corporation_id = 98_000_001;
        }
        if let Some(old_corp) = snap.corporations.get_mut(&98_000_001) {
            old_corp.members.insert(PILOT_2);
            old_corp.member_roles.insert(PILOT_2, 0);
        }
    });

    let mut sess1 = login_select(&ctx, 9201, "demo", "demo-secret", PILOT_1).await;
    let mut sess2 = login_select(&ctx, 9202, "other", "other-secret", PILOT_2).await;

    let add_corp = rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "AddCorporation",
        &[PyRep::String("B29 Lifecycle Corp".into()), PyRep::String("B29L".into()), PyRep::Real(0.05)],
    )
    .await;
    let corp_id = add_corp.dict_get("corporationID").and_then(|v| v.as_i64()).or_else(|| add_corp.as_i64()).unwrap();
    eve_server::services::space_destiny::clear_session_change_timer(&ctx, PILOT_1);

    let orig_corp_p3 = ctx.db.with_snapshot(|s| s.characters[&PILOT_3].corporation_id);
    assert!(rpc_res(&ctx, &mut sess1, "corpRegistry", "InsertInvitation", &[PyRep::Integer(PILOT_3), PyRep::Bool(true)]).await.is_err());
    assert_eq!(ctx.db.with_snapshot(|s| s.characters[&PILOT_3].corporation_id), orig_corp_p3);

    let inv = rpc(&ctx, &mut sess1, "corpRegistry", "InsertInvitation", &[PyRep::Integer(PILOT_2), PyRep::Bool(false)]).await;
    let inv_id = inv.dict_get("applicationID").and_then(|v| v.as_i64()).unwrap();
    let old_corp_p2 = sess2.corporation_id.unwrap();
    rpc(&ctx, &mut sess2, "corpRegistry", "UpdateApplicationOffer", &[PyRep::Integer(inv_id), PyRep::String("Accepted".into())]).await;
    assert_eq!(sess2.corporation_id, Some(corp_id));
    ctx.db.with_snapshot(|s| {
        assert!(s.corporations[&corp_id].members.contains(&PILOT_2));
        assert!(!s.corporations[&old_corp_p2].members.contains(&PILOT_2));
        assert!(!s.corporations[&old_corp_p2].member_roles.contains_key(&PILOT_2));
    });

    ctx.drain_notifications_for_char(PILOT_2);
    eve_server::services::space_destiny::clear_session_change_timer(&ctx, PILOT_2);
    rpc(&ctx, &mut sess1, "corpRegistry", "KickOutMember", &[PyRep::Integer(PILOT_2)]).await;
    let notifs = ctx.drain_notifications_for_char(PILOT_2);
    assert!(notifs.iter().any(|n| n.event == "OnSessionChanged"));
    ctx.db.with_snapshot(|s| {
        assert!(!s.corporations[&corp_id].members.contains(&PILOT_2));
        if let Some(ch) = s.chat_channels.get(&format!("corpid:{corp_id}")) {
            assert!(!ch.members.contains(&PILOT_2));
        }
    });

    // Re-join PILOT_2, assign title + pending role grant + account key, then self-quit via KickOutMember(PILOT_2).
    ctx.advance_secs(25);
    eve_server::services::space_destiny::clear_session_change_timer(&ctx, PILOT_2);
    let inv2 = rpc(&ctx, &mut sess1, "corpRegistry", "InsertInvitation", &[PyRep::Integer(PILOT_2), PyRep::Bool(false)]).await;
    let inv2_id = inv2.dict_get("applicationID").and_then(|v| v.as_i64()).unwrap();
    rpc(&ctx, &mut sess2, "corpRegistry", "UpdateApplicationOffer", &[PyRep::Integer(inv2_id), PyRep::String("Accepted".into())]).await;
    rpc(&ctx, &mut sess1, "corpRegistry", "UpdateTitle", &[PyRep::Integer(1), PyRep::String("Officer".into()), PyRep::Integer(0), PyRep::Integer(0), PyRep::Integer(0), PyRep::Integer(0)]).await;
    rpc(&ctx, &mut sess1, "corpRegistry", "UpdateMember", &[PyRep::Integer(PILOT_2), PyRep::Integer((1u64 << 13) as i64), PyRep::Integer(1), PyRep::Integer(1001)]).await;

    ctx.advance_secs(25);
    eve_server::services::space_destiny::clear_session_change_timer(&ctx, PILOT_2);
    rpc(&ctx, &mut sess2, "corpRegistry", "KickOutMember", &[PyRep::Integer(PILOT_2)]).await;
    let bloodline_id = ctx.db.with_snapshot(|s| s.characters[&PILOT_2].bloodline_id);
    let expected_npc_corp = ctx.sde.bloodline(bloodline_id).expect("bloodline").corporation_id;
    assert_eq!(sess2.corporation_id, Some(expected_npc_corp));
    ctx.db.with_snapshot(|s| {
        if let Some(gov) = s.corp_governance.get(&corp_id) {
            assert!(!gov.member_titles.contains_key(&PILOT_2));
            assert!(!gov.pending_role_grants.contains_key(&PILOT_2));
            assert!(!gov.member_account_keys.contains_key(&PILOT_2));
        }
        assert!(!s.corp_member_titles.contains_key(&(corp_id, PILOT_2)));
        assert!(!s.chat_channels[&format!("corpid:{corp_id}")].members.contains(&PILOT_2));
        assert!(s.chat_channels[&format!("corpid:{expected_npc_corp}")].members.contains(&PILOT_2));
    });
}

#[tokio::test]
async fn test_b29_crimewatch_persist_restart_and_movement_restrictions() {
    // parity: feat:lifecycle.crimewatch-state-persist
    // parity: feat:lifecycle.crimewatch-across-restart
    // parity: feat:lifecycle.crimewatch-highsec-criminal-movement
    eve_testkit::attest(&[
        "feat:lifecycle.crimewatch-state-persist",
        "feat:lifecycle.crimewatch-across-restart",
        "feat:lifecycle.crimewatch-highsec-criminal-movement",
    ]);

    let db_path = temp_sqlite_path("cw_persist");
    let t0 = 1_775_100_000u64;
    let jita = 30_000_142u32;

    let (crim_exp, susp_exp, zz_gate_in, zz_allowed_gate) = {
        let (ctx, _clock) = open_ctx(&db_path, t0);
        let pos = DVec3::new(100_000.0, 0.0, 0.0);
        let p1_skills = ctx.db.with_snapshot(|s| s.skills.get(&PILOT_1).cloned());
        let mut s1 = setup_pilot_in_space(&ctx, 9301, PILOT_1, 1_000_009, 86_300_001, 603, jita, pos).await;
        let _s2 = setup_pilot_in_space(&ctx, 9302, PILOT_2, 1_000_009, 86_300_002, 603, jita, pos + DVec3::new(1_500.0, 0.0, 0.0)).await;
        let mut s3 = setup_pilot_in_space(&ctx, 9303, PILOT_3, 1_000_009, 86_300_003, 603, jita, pos + DVec3::new(3_000.0, 0.0, 0.0)).await;
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(sk) = p1_skills {
                snap.skills.insert(PILOT_3, sk);
            }
            snap.items.insert(86_300_010, item_rec(86_300_010, 448, PILOT_1, 86_300_001, InventoryFlag::MedSlot0, 1, true));
            snap.items.insert(86_300_012, item_rec(86_300_012, 1964, PILOT_3, 86_300_003, InventoryFlag::MedSlot0, 1, true));
        });
        let jita_h = ctx.systems.get_or_spawn(jita);
        for _ in 0..35 {
            let _ = jita_h.step_manual(1.0).await;
        }

        // 1. Live stargate jump into Zarzakh -> zarzakh_gate_locks
        let (&zz_gate_in, zz_jump) = ctx.sde.jumps.iter().find(|(_, j)| j.to_solar_system_id == 30_100_000).expect("gate to Zarzakh");
        let mut s4 = setup_pilot_in_space(&ctx, 9304, PILOT_4, 1_000_009, 86_300_004, 603, zz_jump.from_solar_system_id, zz_jump.position).await;
        ctx.db.with_mut_snapshot(|snap| {
            snap.characters.get_mut(&PILOT_4).unwrap().balance = IskCents::from_isk(50_000_000);
        });
        rpc(&ctx, &mut s4, "beyonce", "CmdStargateJump", &[PyRep::Integer(zz_gate_in as i64)]).await;

        // 2. Live illegal highsec warp-scramble -> criminal flag
        rpc(&ctx, &mut s1, "crimewatch", "SetSafetyLevel", &[PyRep::Integer(0)]).await;
        rpc(&ctx, &mut s1, "dogmaIM", "AddTarget", &[PyRep::Integer(86_300_002)]).await;
        ctx.advance_secs(10);
        for _ in 0..10 {
            let _ = jita_h.step_manual(1.0).await;
        }
        rpc(&ctx, &mut s1, "dogmaIM", "Activate", &[PyRep::Integer(86_300_010), PyRep::String("warpScramble".into()), PyRep::Integer(86_300_002)]).await;
        eve_server::services::combat_crimewatch::create_kill_right(&ctx, 500_001, PILOT_2, PILOT_1, 1_000_000.0);
        ctx.db.with_mut_snapshot(|s| s.characters.get_mut(&PILOT_1).unwrap().security_status = -2.5);

        // 3. Live remote-assisting Criminal PILOT_1 in highsec -> suspect flag on PILOT_3
        rpc(&ctx, &mut s3, "crimewatch", "SetSafetyLevel", &[PyRep::Integer(0)]).await;
        rpc(&ctx, &mut s3, "dogmaIM", "AddTarget", &[PyRep::Integer(86_300_001)]).await;
        ctx.advance_secs(10);
        for _ in 0..10 {
            let _ = jita_h.step_manual(1.0).await;
        }
        rpc(&ctx, &mut s3, "dogmaIM", "Activate", &[PyRep::Integer(86_300_012), PyRep::None, PyRep::Integer(86_300_001)]).await;

        let (ce, se) = {
            let cw = ctx.crimewatch.read().unwrap();
            (
                cw.states.get(&PILOT_1).map(|c| c.criminal_flag_until_unix).unwrap_or(0),
                cw.states.get(&PILOT_3).map(|c| c.suspect_flag_until_unix).unwrap_or(0),
            )
        };
        assert!(ce > t0 && se > t0, "expected ce > t0 ({ce} vs {t0}) and se > t0 ({se} vs {t0})");
        (ce, se, zz_gate_in, zz_jump.destination_stargate_id)
    };

    {
        let (ctx2, _clock2) = open_ctx(&db_path, t0 + 30);
        assert!(ctx2.db.with_snapshot(|s| s.characters[&PILOT_1].security_status < 0.0));
        let unix_to_ft = |u: u64| -> i64 { (u as i64 + 11_644_473_600) * 10_000_000 };
        let mut s1 = setup_pilot_in_space(&ctx2, 9305, PILOT_1, 1_000_009, 86_300_001, 603, jita, DVec3::new(100_000.0, 0.0, 0.0)).await;
        let st1 = rpc(&ctx2, &mut s1, "crimewatch", "GetClientStates", &[]).await;
        let PyRep::Tuple(top) = &st1 else { panic!("expected Tuple from GetClientStates, got {st1:?}"); };
        assert_eq!(top[3], PyRep::Integer(0));
        let PyRep::Tuple(timers) = &top[0] else { panic!("expected timers Tuple, got {:?}", top[0]); };
        assert_eq!(timers.get(3), Some(&PyRep::Tuple(vec![PyRep::Integer(301), PyRep::Integer(unix_to_ft(crim_exp))])));

        let mut s3 = SessionState::new(9306);
        s3.user_id = Some(30);
        s3.character_id = Some(PILOT_3);
        let st3 = rpc(&ctx2, &mut s3, "crimewatch", "GetClientStates", &[]).await;
        let PyRep::Tuple(top3) = &st3 else { panic!("expected Tuple from GetClientStates, got {st3:?}"); };
        let PyRep::Tuple(timers3) = &top3[0] else { panic!("expected timers Tuple, got {:?}", top3[0]); };
        assert!(timers3.contains(&PyRep::Tuple(vec![PyRep::Integer(302), PyRep::Integer(unix_to_ft(susp_exp))])));
        ctx2.db.with_snapshot(|snap| {
            assert!(!snap.kill_rights.is_empty());
            assert_eq!(snap.zarzakh_gate_locks.get(&PILOT_4).map(|(g, _)| *g), Some(zz_allowed_gate));
        });

        // Post-restart Zarzakh exit via a different gate must be rejected
        let (&other_zz_gate, other_zz_jump) = ctx2
            .sde
            .jumps
            .iter()
            .find(|(&gid, j)| j.from_solar_system_id == 30_100_000 && gid != zz_allowed_gate && gid != zz_gate_in)
            .expect("other Zarzakh gate");
        let mut s4 = setup_pilot_in_space(&ctx2, 9307, PILOT_4, 1_000_009, 86_300_004, 603, 30_100_000, other_zz_jump.position).await;
        let zz_err = rpc_res(&ctx2, &mut s4, "beyonce", "CmdStargateJump", &[PyRep::Integer(other_zz_gate as i64)]).await;
        assert!(zz_err.is_err());

        // All 4 live movement RPCs in highsec must reject criminal PILOT_1
        let cel_id = far_celestial_id(&ctx2, jita);
        let (&jita_gate, _) = ctx2.sde.jumps.iter().find(|(_, j)| j.from_solar_system_id == jita).expect("Jita gate");
        assert!(rpc_res(&ctx2, &mut s1, "beyonce", "CmdWarpToStuff", &[PyRep::Integer(cel_id), PyRep::Real(0.0)]).await.is_err());
        assert!(rpc_res(&ctx2, &mut s1, "beyonce", "CmdStargateJump", &[PyRep::Integer(jita_gate as i64)]).await.is_err());
        assert!(rpc_res(&ctx2, &mut s1, "wormholeMgr", "WormholeJump", &[PyRep::Integer(750_000_001)]).await.is_err());
        assert!(rpc_res(&ctx2, &mut s1, "beyonce", "CmdJumpThroughFleet", &[PyRep::Integer(PILOT_2), PyRep::Integer(86_300_002), PyRep::Integer(jita as i64)]).await.is_err());
    }
    let _ = std::fs::remove_file(&db_path);
}

#[tokio::test]
async fn test_b29_drones_and_fighters_space_lifecycle() {
    // parity: feat:lifecycle.drones-leave-grid-abandon
    // parity: feat:lifecycle.drones-in-space-persist-restart
    // parity: feat:lifecycle.drones-recall-same-grid
    // parity: feat:lifecycle.fighters-left-in-space
    eve_testkit::attest(&[
        "feat:lifecycle.drones-leave-grid-abandon",
        "feat:lifecycle.drones-in-space-persist-restart",
        "feat:lifecycle.drones-recall-same-grid",
        "feat:lifecycle.fighters-left-in-space",
    ]);

    let db_path = temp_sqlite_path("drones_fighters");
    let t0 = 1_775_200_000u64;
    let sys_id = 30_000_142u32;
    let kw = HashMap::new();
    let (char_id, vexor_id, carrier_id) = (96_200_001i64, 86_200_099i64, 86_200_100i64);
    let (drone_1, drone_2, fighter_1) = (86_200_110i64, 86_200_111i64, 86_200_120i64);
    let (f_pilot, f_sess_id) = (96_200_003i64, 9403u64);
    let (station_pos, drone_ball_1) = {
        let (ctx, _clock) = open_ctx(&db_path, t0);
        let pos = ctx.sde.stations[&60_003_760].position;
        let mut sess = setup_pilot_in_space(&ctx, 9401, char_id, 1_000_125, vexor_id, 626, sys_id, pos + DVec3::new(1_000.0, 0.0, 0.0)).await;
        ctx.db.with_mut_snapshot(|snap| {
            snap.skills.entry(char_id).or_default().insert(
                3436,
                TrainedSkillRecord { type_id: 3436, level: 5, skill_points: 256_000.0 },
            );
            for did in [drone_1, drone_2] {
                snap.items.insert(did, item_rec(did, 2454, char_id, vexor_id, InventoryFlag::DroneBay, 1, false));
            }
            snap.items.insert(fighter_1, item_rec(fighter_1, 23055, f_pilot, carrier_id, InventoryFlag::FighterBay, 9, false));
        });

        let launch_rep = ctx.dispatch_rpc(&mut sess, "ship", None, "LaunchDrones", &[PyRep::List(vec![PyRep::Integer(drone_1), PyRep::Integer(drone_2)])], &kw, None).await.expect("LaunchDrones");
        let drone_ball_1 = match &launch_rep {
            PyRep::List(items) => items.first().and_then(|v| v.as_i64()).unwrap_or(drone_1),
            _ => drone_1,
        };

        let mut f_sess = setup_pilot_in_space(&ctx, f_sess_id, f_pilot, 1_000_125, carrier_id, 23757, sys_id, pos + DVec3::new(1_000.0, 0.0, 0.0)).await;
        ctx.dispatch_rpc(&mut f_sess, "fighterMgr", None, "LoadFightersToTube", &[PyRep::Integer(0), PyRep::Integer(fighter_1)], &kw, None).await.expect("LoadFightersToTube");
        ctx.dispatch_rpc(&mut f_sess, "fighterMgr", None, "LaunchFightersFromTubes", &[PyRep::List(vec![PyRep::Integer(0)])], &kw, None).await.expect("LaunchFightersFromTubes");
        assert_eq!(ctx.db.get_item(fighter_1).unwrap().location_id, sys_id as i64);

        let sys_h = ctx.systems.get_or_spawn(sys_id);
        let (_, mut cball) = sys_h.get_ball_snapshot(vexor_id).await.unwrap().unwrap();
        cball.position = pos + DVec3::new(1_000.0, 0.0, 0.0);
        cball.velocity = DVec3::ZERO;
        cball.speed_fraction = 0.0;
        cball.mode = MovementMode::Stop;
        let _ = sys_h.spawn_ball(cball).await;
        ctx.advance_secs(12);
        ctx.dispatch_rpc(&mut sess, "stationSvc", None, "Dock", &[PyRep::Integer(60_003_760)], &kw, None).await.expect("Dock");
        ctx.dispatch_rpc(&mut f_sess, "stationSvc", None, "Dock", &[PyRep::Integer(60_003_760)], &kw, None).await.expect("Carrier Dock");
        assert_eq!(ctx.db.get_item(fighter_1).unwrap().location_id, sys_id as i64);
        assert!(ctx.active_fighters.read().unwrap().values().all(|f| f.is_abandoned));

        let _ = ctx.dispatch_rpc(&mut sess, "entity", None, "CmdReturnBay", &[PyRep::List(vec![PyRep::Integer(drone_ball_1)])], &kw, None).await;
        assert_eq!(ctx.db.get_item(drone_1).unwrap().location_id, sys_id as i64);
        (pos, drone_ball_1)
    };

    {
        let (ctx2, _clock2) = open_ctx(&db_path, t0 + 30);
        let (scooper_id, scooper_ship) = (96_200_002i64, 86_200_200i64);
        let mut scooper_sess = setup_pilot_in_space(&ctx2, 9402, scooper_id, 1_000_009, scooper_ship, 626, sys_id, station_pos + DVec3::new(1_000.0, 0.0, 0.0)).await;
        let sys_h2 = ctx2.systems.get_or_spawn(sys_id);
        assert!(sys_h2.get_ball_snapshot(drone_ball_1).await.unwrap().is_some());
        ctx2.dispatch_rpc(&mut scooper_sess, "ship", None, "ScoopToDroneBay", &[PyRep::Integer(drone_ball_1)], &kw, None).await.expect("ScoopToDroneBay");
        assert_eq!(ctx2.db.get_item(drone_1).unwrap().location_id, scooper_ship);

        // Undock carrier, scoop abandoned fighter, load into tube 0, and re-launch
        let mut f_sess2 = setup_pilot_in_space(&ctx2, f_sess_id, f_pilot, 1_000_125, carrier_id, 23757, sys_id, station_pos + DVec3::new(1_000.0, 0.0, 0.0)).await;
        ctx2.dispatch_rpc(&mut f_sess2, "fighterMgr", None, "CmdScoopAbandonedFighterFromSpace", &[PyRep::Integer(fighter_1)], &kw, None).await.expect("ScoopFighter");
        assert_eq!(ctx2.db.get_item(fighter_1).unwrap().location_id, carrier_id);
        ctx2.dispatch_rpc(&mut f_sess2, "fighterMgr", None, "LoadFightersToTube", &[PyRep::Integer(0), PyRep::Integer(fighter_1)], &kw, None).await.expect("ReloadTube");
        ctx2.dispatch_rpc(&mut f_sess2, "fighterMgr", None, "LaunchFightersFromTubes", &[PyRep::List(vec![PyRep::Integer(0)])], &kw, None).await.expect("RelaunchFighter");
        assert_eq!(ctx2.db.get_item(fighter_1).unwrap().location_id, sys_id as i64);
    }
    let _ = std::fs::remove_file(&db_path);
}

#[tokio::test]
async fn test_b29_fleet_grace_invite_and_death_courier_lifecycle() {
    // parity: feat:lifecycle.fleet-disconnect-grace
    // parity: feat:lifecycle.fleet-invite-offline
    // parity: feat:lifecycle.death-session-change
    // parity: feat:lifecycle.courier-package-destroyed
    eve_testkit::attest(&[
        "feat:lifecycle.fleet-disconnect-grace",
        "feat:lifecycle.fleet-invite-offline",
        "feat:lifecycle.death-session-change",
        "feat:lifecycle.courier-package-destroyed",
    ]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let (cargo_1, cargo_2) = (620_003i64, 620_004i64);
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch3 = snap.characters.get(&PILOT_2).cloned().expect("PILOT_2");
        ch3.character_id = PILOT_3;
        ch3.name = "Offline Fleet Target".into();
        snap.characters.insert(PILOT_3, ch3);
        snap.characters.get_mut(&PILOT_1).unwrap().balance = IskCents::from_isk(500_000_000);
        snap.characters.get_mut(&PILOT_2).unwrap().balance = IskCents::from_isk(500_000_000);
        snap.items.insert(cargo_1, item_rec(cargo_1, 34, PILOT_2, 60_003_760, InventoryFlag::Hangar, 100, false));
        snap.items.insert(cargo_2, item_rec(cargo_2, 34, PILOT_2, 60_003_760, InventoryFlag::Hangar, 50, false));
    });
    let mut s1 = login_select(&ctx, 9501, "demo", "demo-secret", PILOT_1).await;
    let mut s2 = login_select(&ctx, 9502, "other", "other-secret", PILOT_2).await;

    let fid = rpc(&ctx, &mut s1, "fleetMgr", "CreateFleet", &[]).await.dict_get("fleetID").and_then(|v| v.as_i64()).unwrap();
    assert!(rpc_res(&ctx, &mut s1, "fleetObjectHandler", "Invite", &[PyRep::Integer(PILOT_3), PyRep::None, PyRep::None, PyRep::Integer(4)]).await.is_err());

    rpc(&ctx, &mut s1, "fleetObjectHandler", "Invite", &[PyRep::Integer(PILOT_2), PyRep::None, PyRep::None, PyRep::Integer(4)]).await;
    ctx.advance_secs(FLEET_INVITE_EXPIRY_SECS + 5);
    tick_fleet_lifecycle(&ctx);
    assert!(rpc_res(&ctx, &mut s2, "fleetObjectHandler", "AcceptInvite", &[PyRep::Integer(fid)]).await.is_err());

    rpc(&ctx, &mut s1, "fleetObjectHandler", "Invite", &[PyRep::Integer(PILOT_2), PyRep::None, PyRep::None, PyRep::Integer(4)]).await;
    eve_server::services::space_destiny::clear_session_change_timer(&ctx, PILOT_2);
    rpc(&ctx, &mut s2, "fleetObjectHandler", "AcceptInvite", &[PyRep::Integer(fid)]).await;
    handle_fleet_member_disconnect(&ctx, &mut s2);
    ctx.db.with_snapshot(|snap| assert!(snap.fleets[&fid].members.contains(&PILOT_2)));
    ctx.advance_secs(FLEET_DISCONNECT_GRACE_SECS + 5);
    tick_fleet_lifecycle(&ctx);
    ctx.db.with_snapshot(|snap| assert!(!snap.fleets[&fid].members.contains(&PILOT_2)));

    // Live CreateContract & AcceptContract for Courier #1 (1,000,000 ISK collateral)
    let mut s2 = login_select(&ctx, 9503, "other", "other-secret", PILOT_2).await;
    let c1_rep = rpc(&ctx, &mut s2, "contractProxy", "CreateContract", &[
        PyRep::Integer(3), PyRep::Integer(0), PyRep::Integer(0), PyRep::Integer(60), PyRep::Integer(1),
        PyRep::Integer(60_003_760), PyRep::Integer(60_004_588), PyRep::Real(0.0), PyRep::Real(50_000.0),
        PyRep::Real(1_000_000.0), PyRep::String("Courier 1".into()), PyRep::None,
        PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(cargo_1), PyRep::Integer(100)])]),
    ]).await;
    let c1_id = c1_rep.as_i64().or_else(|| c1_rep.dict_get("contractID").and_then(|v| v.as_i64())).expect("c1_id");
    rpc(&ctx, &mut s1, "contractProxy", "AcceptContract", &[PyRep::Integer(c1_id)]).await;
    ctx.db.with_mut_snapshot(|snap| {
        let ship_id = snap.characters[&PILOT_1].active_ship_id;
        if let Some(wrap) = snap.items.values_mut().find(|it| it.type_id == 3468 && it.location_id == 60_003_760) {
            wrap.location_id = ship_id;
            wrap.flag = InventoryFlag::Cargo;
        }
    });

    // Undock PILOT_1 into space, then self-destruct ship via live ship.SelfDestruct
    let ship1_id = ctx.db.with_snapshot(|s| s.characters[&PILOT_1].active_ship_id);
    s1 = setup_pilot_in_space(&ctx, 1, PILOT_1, 1_000_009, ship1_id, 603, 30_000_142, DVec3::new(100_000.0, 0.0, 0.0)).await;
    eve_server::services::space_destiny::clear_session_change_timer(&ctx, PILOT_1);
    let p2_bal_before = ctx.db.with_snapshot(|s| s.characters[&PILOT_2].balance);
    ctx.drain_notifications_for_char(PILOT_1);

    rpc_res(&ctx, &mut s1, "ship", "SelfDestruct", &[]).await.expect("SD1");
    ctx.advance_secs(125);
    rpc_res(&ctx, &mut s1, "ship", "SelfDestruct", &[]).await.expect("SD2");

    let capsule_id = ctx.db.with_snapshot(|s| s.characters[&PILOT_1].active_ship_id);
    let ship_death_notifs = ctx.drain_notifications_for_char(PILOT_1);
    assert!(session_changed_has_field(&ship_death_notifs, "shipid", capsule_id));
    assert!(rpc_res(&ctx, &mut s1, "stationSvc", "Dock", &[PyRep::Integer(60_003_760)]).await.is_err());

    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.contracts[&c1_id].status, "Failed");
        assert_eq!(snap.characters[&PILOT_2].balance.0, p2_bal_before.0 + IskCents::from_isk(1_050_000).0);
        assert!(!snap.items.values().any(|it| it.type_id == 3468 && it.location_id == 60_003_760));
    });

    // Pod self-destruct -> medical-clone station respawn + SessionChangeInProgress on immediate Undock
    ctx.advance_secs(15);
    let mut s4_pod = setup_pilot_in_space(&ctx, 9505, PILOT_4, 1_000_009, 620_099, 670, 30_000_142, DVec3::new(100_000.0, 0.0, 0.0)).await;
    eve_server::services::space_destiny::clear_session_change_timer(&ctx, PILOT_4);
    ctx.drain_notifications_for_char(PILOT_4);
    rpc(&ctx, &mut s4_pod, "ship", "SelfDestruct", &[]).await;
    ctx.advance_secs(125);
    if ctx.db.with_snapshot(|s| s.characters[&PILOT_4].station_id.is_none()) {
        rpc(&ctx, &mut s4_pod, "ship", "SelfDestruct", &[]).await;
    } else {
        s4_pod.station_id = ctx.db.with_snapshot(|s| s.characters[&PILOT_4].station_id);
    }
    let pod_notifs = ctx.drain_notifications_for_char(PILOT_4);
    assert!(session_changed_has_field(&pod_notifs, "stationid", 60_003_760));
    assert_eq!(s4_pod.station_id, Some(60_003_760));
    let undock_err = rpc_res(&ctx, &mut s4_pod, "stationSvc", "Undock", &[PyRep::Integer(60_003_760)])
        .await
        .expect_err("immediate Undock after pod respawn must be rejected");
    assert_eq!(undock_err.code, "SessionChangeInProgress");

    // Courier #2 deadline failure while in space -> Failed with collateral, wrap NOT teleported to start station
    ctx.advance_secs(15);
    eve_server::services::space_destiny::clear_session_change_timer(&ctx, PILOT_1);
    let c2_rep = rpc(&ctx, &mut s2, "contractProxy", "CreateContract", &[
        PyRep::Integer(3), PyRep::Integer(0), PyRep::Integer(0), PyRep::Integer(60), PyRep::Integer(1),
        PyRep::Integer(60_003_760), PyRep::Integer(60_004_588), PyRep::Real(0.0), PyRep::Real(50_000.0),
        PyRep::Real(500_000.0), PyRep::String("Courier 2".into()), PyRep::None,
        PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(cargo_2), PyRep::Integer(50)])]),
    ]).await;
    let c2_id = c2_rep.as_i64().or_else(|| c2_rep.dict_get("contractID").and_then(|v| v.as_i64())).expect("c2_id");
    let mut s1_c2 = login_select(&ctx, 9506, "demo", "demo-secret", PILOT_1).await;
    rpc(&ctx, &mut s1_c2, "contractProxy", "AcceptContract", &[PyRep::Integer(c2_id)]).await;
    let ship2_id = ctx.db.with_snapshot(|s| s.characters[&PILOT_1].active_ship_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(wrap) = snap.items.values_mut().find(|it| it.type_id == 3468 && it.location_id == 60_003_760) {
            wrap.location_id = ship2_id;
            wrap.flag = InventoryFlag::Cargo;
        }
    });
    rpc(&ctx, &mut s1_c2, "stationSvc", "Undock", &[PyRep::Integer(60_003_760)]).await;
    ctx.advance_secs(90_000);
    rpc(&ctx, &mut s1_c2, "contractProxy", "FailContract", &[PyRep::Integer(c2_id)]).await;
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.contracts[&c2_id].status, "Failed");
        assert_eq!(snap.items[&cargo_2].location_id, ship2_id);
        assert_eq!(snap.items[&cargo_2].owner_id, PILOT_1);
    });
}
