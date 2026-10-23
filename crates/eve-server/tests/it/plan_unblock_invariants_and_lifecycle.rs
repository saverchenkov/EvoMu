//! Integration tests verifying runtime features and lifecycle behaviors.

#![forbid(unsafe_code)]

use eve_db::{
    corp_roles, AllianceRecord, CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents,
    MiningLedgerRecord, WorldRepository,
};
use eve_destiny::{Ball, CombatState, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::esi::{EsiHttpRequest, EsiRouter};
use eve_server::router::ServerContext;
use eve_server::services::combat_crimewatch::{
    passive_shield_recharge_rate_hps, step_passive_shield_recharge, step_wreck_and_jetcan_decay,
};
use glam::DVec3;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

const JITA_SYSTEM_ID: u32 = 30_000_142;
const JITA_STATION_ID: i64 = 60_003_760;

fn temp_sqlite_path(tag: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("evomu_{tag}_{}.sqlite", std::process::id()));
    let _ = std::fs::remove_file(&p);
    let _ = std::fs::remove_file(p.with_extension("sqlite-wal"));
    let _ = std::fs::remove_file(p.with_extension("sqlite-shm"));
    p
}

fn cleanup_sqlite_path(p: &PathBuf) {
    let _ = std::fs::remove_file(p);
    let _ = std::fs::remove_file(p.with_extension("sqlite-wal"));
    let _ = std::fs::remove_file(p.with_extension("sqlite-shm"));
}

async fn rpc(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, eve_proto::BridgeRpcError> {
    let kw = HashMap::new();
    if service == "charUnboundMgr" && method == "SelectCharacterID" {
        let (u, p) = if args.first() == Some(&PyRep::Integer(90_000_002)) {
            ("other", "other-secret")
        } else {
            ("demo", "demo-secret")
        };
        let _ = ctx
            .dispatch_rpc(
                session,
                "authentication",
                None,
                "Login",
                &[PyRep::String(u.into()), PyRep::String(p.into())],
                &kw,
                None,
            )
            .await;
    }
    ctx.dispatch_rpc(session, service, None, method, args, &kw, None)
        .await
}

#[tokio::test]
async fn test_c2_dogma_shield_recharge_curve_and_damage_state() {

    // 1. Verify EVE's non-linear passive shield recharge curve peaks at ~25% shield capacity
    let (max_sh, tau_s) = (2_000.0, 200.0);
    let r_05 = passive_shield_recharge_rate_hps(max_sh * 0.05, max_sh, tau_s);
    let r_10 = passive_shield_recharge_rate_hps(max_sh * 0.10, max_sh, tau_s);
    let r_25 = passive_shield_recharge_rate_hps(max_sh * 0.25, max_sh, tau_s);
    let r_50 = passive_shield_recharge_rate_hps(max_sh * 0.50, max_sh, tau_s);
    let r_90 = passive_shield_recharge_rate_hps(max_sh * 0.90, max_sh, tau_s);
    let r_full = passive_shield_recharge_rate_hps(max_sh, max_sh, tau_s);

    assert!((r_25 - 25.0).abs() < 1e-6);
    assert!(r_25 > r_05 && r_25 > r_10 && r_25 > r_50 && r_25 > r_90);
    assert_eq!(r_full, 0.0);

    // 2. Step passive shield recharge on an in-space ship and verify CombatState + OnDamageStateChange
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut sess = SessionState::new(7101);
    let _ = rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)])
        .await
        .expect("SelectCharacterID");
    let ship_id = sess.ship_id.unwrap_or(100_000_001);
    let handle = ctx.systems.get_or_spawn(JITA_SYSTEM_ID);
    handle
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: 587,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 350.0,
                inertia: 2.5,
                mass: 1_200_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState {
                shield_hp: 500.0,
                max_shield_hp: 2_000.0,
                armor_hp: 1_000.0,
                max_armor_hp: 1_000.0,
                structure_hp: 1_000.0,
                max_structure_hp: 1_000.0,
                shield_recharge_time_ms: 200_000.0,
                ..CombatState::default()
            },
        )
        .await
        .expect("spawn_ball_with_combat");

    let (after_hp, delta_hp) =
        step_passive_shield_recharge(&ctx, JITA_SYSTEM_ID, Some(90_000_001), ship_id, 200.0, 10.0)
            .await
            .expect("step_passive_shield_recharge");
    assert!(delta_hp > 200.0 && after_hp > 700.0);
    let cs = handle.get_combat_state(ship_id).await.unwrap().expect("combat state");
    assert!((cs.shield_hp - after_hp).abs() < 1e-6);
}

#[tokio::test]
async fn test_d6_single_source_of_truth_cross_surface_and_restart() {

    let db_path = temp_sqlite_path("d6_ssot");

    {
        let repo = Arc::new(WorldRepository::open_sqlite(&db_path).expect("open sqlite"));
        let ctx = Arc::new(ServerContext::with_repository(repo));
        let demo = WorldRepository::with_demo_seed().with_snapshot(|s| s.clone());
        ctx.db.with_mut_snapshot(|snap| *snap = demo);
        crate::common::pin_clock(&ctx);

        let mut sess = SessionState::new(7102);
        let _ = rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)])
            .await
            .expect("SelectCharacterID");

        let _ = rpc(
            &ctx,
            &mut sess,
            "addressbook",
            "AddContact",
            &[PyRep::Integer(90_000_002), PyRep::Real(10.0), PyRep::Bool(true)],
        )
        .await
        .expect("AddContact");
        let _ = rpc(
            &ctx,
            &mut sess,
            "invbroker",
            "SetLabel",
            &[PyRep::Integer(100_000_001), PyRep::String("SSOT Flagship".into())],
        )
        .await
        .expect("SetLabel");
        let _ = rpc(&ctx, &mut sess, "charMgr", "LogSettings", &[PyRep::Real(3_750.0)])
            .await
            .expect("charMgr.LogSettings");
        ctx.db.with_mut_snapshot(|snap| {
            snap.alliances.insert(
                99_000_777,
                AllianceRecord {
                    alliance_id: 99_000_777,
                    name: "Single Source Alliance".into(),
                    short_name: "SSOTA".into(),
                    executor_corp_id: 98_000_001,
                    member_corp_ids: [98_000_001].into_iter().collect(),
                },
            );
        });

        let esi = EsiRouter::new(Arc::clone(&ctx));
        let esi_alliances = esi.handle_request(&EsiHttpRequest::new("GET", "/v1/alliances/"));
        assert_eq!(esi_alliances.status_code, 200);
        assert!(String::from_utf8_lossy(&esi_alliances.body).contains("99000777"));
    }

    let repo2 = Arc::new(WorldRepository::open_sqlite(&db_path).expect("reopen sqlite"));
    let ctx2 = Arc::new(ServerContext::with_repository(repo2));
    let mut sess2 = SessionState::new(7103);
    let _ = rpc(&ctx2, &mut sess2, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)])
        .await
        .expect("SelectCharacterID after restart");

    let names_rep = rpc(
        &ctx2,
        &mut sess2,
        "dogmaIM",
        "ItemGetInfo",
        &[PyRep::Integer(100_000_001)],
    )
    .await
    .expect("ItemGetInfo after restart");
    assert!(format!("{names_rep:?}").contains("SSOT Flagship"));

    let contacts_rep = rpc(&ctx2, &mut sess2, "addressbook", "GetContactList", &[])
        .await
        .expect("addressbook after restart");
    assert!(format!("{contacts_rep:?}").contains("90000002"));

    let has_alliance = ctx2.db.with_snapshot(|snap| snap.alliances.contains_key(&99_000_777));
    assert!(has_alliance);

    let esi2 = EsiRouter::new(Arc::clone(&ctx2));
    let esi_alliances2 = esi2.handle_request(&EsiHttpRequest::new("GET", "/v1/alliances/"));
    assert_eq!(esi_alliances2.status_code, 200);
    assert!(String::from_utf8_lossy(&esi_alliances2.body).contains("99000777"));

    cleanup_sqlite_path(&db_path);
}

#[tokio::test]
async fn test_dg4_module_cycles_stop_on_leave_space_and_dock() {

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut sess = SessionState::new(7104);
    let _ = rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)])
        .await
        .expect("SelectCharacterID");
    let ship_id = sess.ship_id.unwrap_or(100_000_001);

    let ab_id = 880_001_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ab_id,
            InventoryItemRecord {
                item_id: ab_id,
                type_id: 434,
                owner_id: 90_000_001,
                location_id: ship_id,
                flag: InventoryFlag::MedSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });

    let _ = rpc(&ctx, &mut sess, "ship", "Undock", &[]).await.expect("Undock");
    let act_rep = rpc(
        &ctx,
        &mut sess,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(ab_id), PyRep::String("afterburner".into())],
    )
    .await
    .expect("Activate afterburner");
    assert_eq!(act_rep.dict_get("active"), Some(&PyRep::Bool(true)));
    assert!(ctx.db.with_snapshot(|snap| snap.module_active_states.contains(&ab_id)));

    let handle = ctx.systems.get_or_spawn(JITA_SYSTEM_ID);
    let st_pos = ctx
        .sde
        .stations
        .get(&JITA_STATION_ID)
        .map(|s| s.position)
        .unwrap_or(DVec3::ZERO);
    let _ = handle
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: 587,
                position: st_pos + DVec3::new(500.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 350.0,
                inertia: 2.5,
                mass: 1_200_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_type_combat_state(587),
        )
        .await;

    let _ = ctx.drain_notifications_for_char(90_000_001);
    let _ = rpc(&ctx, &mut sess, "beyonce", "Dock", &[PyRep::Integer(JITA_STATION_ID)])
        .await
        .expect("Dock at Jita 4-4");
    assert_eq!(sess.station_id, Some(JITA_STATION_ID));

    assert!(!ctx.db.with_snapshot(|snap| snap.module_active_states.contains(&ab_id)));
    let notifs = ctx.drain_notifications_for_char(90_000_001);
    assert!(notifs.iter().any(|n| n.event == "OnGodmaShipEffect"));
}

#[tokio::test]
async fn test_e3_corp_contract_wallet_roles_and_issuer_departure() {

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let (corp_id, item_id) = (98_000_501_i64, 880_002_001_i64);
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.insert(
            corp_id,
            CorporationRecord {
                corporation_id: corp_id,
                name: "Contract Wallet Corp".into(),
                ticker: "CWLT".into(),
                ceo_id: 90_000_002,
                alliance_id: None,
                tax_rate: 0.05,
                master_wallet: IskCents::from_f64_isk(10_000_000.0),
                members: [90_000_001, 90_000_002].into_iter().collect(),
                member_roles: HashMap::new(),
                applications: HashMap::new(),
                wallet_divisions: CorporationRecord::default_wallet_divisions(
                    IskCents::from_f64_isk(10_000_000.0),
                ),
                description: String::new(),
                url: String::new(),
                hq_station_id: Some(JITA_STATION_ID),
            },
        );
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.corporation_id = corp_id;
        }
        snap.items.insert(
            item_id,
            InventoryItemRecord {
                item_id,
                type_id: 34,
                owner_id: 90_000_001,
                location_id: JITA_STATION_ID,
                flag: InventoryFlag::Hangar,
                quantity: 1_000,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    let mut s1 = SessionState::new(7105);
    let _ = rpc(&ctx, &mut s1, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)])
        .await
        .expect("SelectCharacterID s1");
    let mut s2 = SessionState::new(7106);
    let _ = rpc(&ctx, &mut s2, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_002)])
        .await
        .expect("SelectCharacterID s2");

    let mut kw = HashMap::new();
    kw.insert("forCorp".into(), PyRep::Bool(true));
    let c_args = [
        PyRep::Integer(1),
        PyRep::Integer(0),
        PyRep::Integer(0),
        PyRep::Integer(1440),
        PyRep::Integer(1),
        PyRep::Integer(JITA_STATION_ID),
        PyRep::Integer(JITA_STATION_ID),
        PyRep::Real(500_000.0),
        PyRep::Real(50_000.0),
        PyRep::Real(0.0),
        PyRep::String("Corp Contract".into()),
        PyRep::String(String::new()),
        PyRep::List(vec![PyRep::Integer(item_id)]),
    ];
    let deny_err = ctx
        .dispatch_rpc(&mut s1, "contractProxy", None, "CreateContract", &c_args, &kw, None)
        .await
        .expect_err("for_corp contract without role must fail");
    assert_eq!(deny_err.code, "CrpAccessDenied");

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(corp) = snap.corporations.get_mut(&corp_id) {
            corp.member_roles
                .insert(90_000_001, corp_roles::ROLE_CONTRACT_MANAGER);
            if let Some(div) = corp.wallet_divisions.first_mut() {
                div.balance = IskCents(0); // empty division 1 to test short funds
            }
        }
    });
    // Fixes R1-P1-2: Assert division 1 shortage fails with InsufficientFunds
    let short_div_err = ctx
        .dispatch_rpc(&mut s1, "contractProxy", None, "CreateContract", &c_args, &kw, None)
        .await
        .expect_err("division 1 lacking funds must fail with InsufficientFunds");
    assert_eq!(short_div_err.code, "InsufficientFunds");

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(corp) = snap.corporations.get_mut(&corp_id) {
            if let Some(div) = corp.wallet_divisions.first_mut() {
                div.balance = IskCents::from_f64_isk(10_000_000.0);
            }
        }
    });
    let char_bal_before = ctx.db.get_character(90_000_001).unwrap().balance;
    let corp_bal_before = ctx.db.with_snapshot(|snap| snap.corporations[&corp_id].master_wallet);

    let created = ctx
        .dispatch_rpc(&mut s1, "contractProxy", None, "CreateContract", &c_args, &kw, None)
        .await
        .expect("CreateContract for_corp with ROLE_CONTRACT_MANAGER");
    let cid = created.dict_get("contractID").and_then(|v| v.as_i64()).unwrap();

    let char_bal_mid = ctx.db.get_character(90_000_001).unwrap().balance;
    let corp_bal_mid = ctx.db.with_snapshot(|snap| snap.corporations[&corp_id].master_wallet);
    assert_eq!(char_bal_mid, char_bal_before);
    assert!(corp_bal_mid.0 < corp_bal_before.0);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.corporation_id = 1_000_009;
        }
    });
    let _ = rpc(&ctx, &mut s2, "contractProxy", "AcceptContract", &[PyRep::Integer(cid)])
        .await
        .expect("AcceptContract");

    let char_bal_after = ctx.db.get_character(90_000_001).unwrap().balance;
    let corp_bal_after = ctx.db.with_snapshot(|snap| snap.corporations[&corp_id].master_wallet);
    assert_eq!(char_bal_after, char_bal_before);
    assert_eq!(
        corp_bal_after.0 - corp_bal_mid.0,
        IskCents::from_f64_isk(500_000.0).0
    );
}

// parity: feat:lifecycle.secure_container-anchor
#[tokio::test]
async fn test_s7_secure_container_anchor_no_bubble_and_30d_despawn() {

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut sess = SessionState::new(7107);
    let _ = rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)])
        .await
        .expect("SelectCharacterID");
    let _ = rpc(&ctx, &mut sess, "ship", "Undock", &[]).await.expect("Undock");
    let ship_id = sess.ship_id.unwrap_or(100_000_001);

    let container_type_id = ctx.sde.types.values().find(|t| (t.group_id == 340 || t.group_id == 448) && t.effect_ids.contains(&649)).map(|t| t.type_id).unwrap_or(3465);
    let cont_id = 880_003_001_i64;
    let unanchorable_id = 880_003_002_i64;
    ctx.db.with_mut_snapshot(|snap| {
        for skill_id in [3426u32, 11584u32] {
            snap.skills.entry(90_000_001).or_default().insert(skill_id, eve_db::TrainedSkillRecord { type_id: skill_id, level: 5, skill_points: 256_000.0 });
        }
        snap.items.insert(unanchorable_id, InventoryItemRecord {
            item_id: unanchorable_id, type_id: 23, owner_id: 90_000_001, location_id: ship_id,
            flag: InventoryFlag::Cargo, quantity: 1, singleton: true, version: 1, ..Default::default()
        });
        snap.items.insert(cont_id, InventoryItemRecord {
            item_id: cont_id, type_id: container_type_id, owner_id: 90_000_001, location_id: ship_id,
            flag: InventoryFlag::Cargo, quantity: 1, singleton: true, version: 1, ..Default::default()
        });
    });

    let err = rpc(&ctx, &mut sess, "posMgr", "AnchorStructure", &[PyRep::Integer(unanchorable_id)])
        .await
        .expect_err("Cargo container cannot anchor");
    assert_eq!(err.code, "ItemNotAnchorable");

    let anchor_rep = rpc(&ctx, &mut sess, "posMgr", "AnchorStructure", &[PyRep::Integer(cont_id)]).await.expect("posMgr.AnchorStructure for secure container");
    assert_eq!(anchor_rep.dict_get("bubbleRadius"), Some(&PyRep::Real(0.0)));
    assert_eq!(anchor_rep.dict_get("state"), Some(&PyRep::String("Anchoring".into())));
    let now_u = ctx.now_unix();
    assert_eq!(anchor_rep.dict_get("despawnUnix"), Some(&PyRep::Integer((now_u + 30 * 86_400) as i64)));
    assert!(ctx.db.with_snapshot(|snap| snap.item_positions.contains_key(&cont_id)));

    // Advance virtual clock past anchoring delay (60s)
    ctx.advance_secs(65);
    let is_anchored = ctx.db.with_snapshot(|snap| {
        snap.sp6_lifecycle.space_object_meta.get(&cont_id).map(|m| m.anchor_completed).unwrap_or(false)
    });
    assert!(is_anchored, "Container must be marked anchored after anchoring delay elapses");

    let despawned = step_wreck_and_jetcan_decay(&ctx, JITA_SYSTEM_ID, now_u + 30 * 86_400 + 1).await;
    assert!(despawned.contains(&cont_id));
    assert!(ctx.db.get_item(cont_id).is_none());
}

#[tokio::test]
async fn test_so7_chat_and_mail_item_fitting_hyperlinks_roundtrip() {

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut s1 = SessionState::new(7108);
    let _ = rpc(&ctx, &mut s1, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)]).await.expect("SelectCharacterID s1");
    let mut s2 = SessionState::new(7109);
    let _ = rpc(&ctx, &mut s2, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_002)]).await.expect("SelectCharacterID s2");

    let fit_rep = rpc(&ctx, &mut s1, "charFittingMgr", "SaveFitting", &[
        PyRep::Integer(90_000_001),
        PyRep::keyval([
            ("name", PyRep::String("Hyperlink Rifter".into())),
            ("shipTypeID", PyRep::Integer(587)),
            ("description", PyRep::String("Linked via chat".into())),
            ("fitData", PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(434), PyRep::Integer(19), PyRep::Integer(1)])])),
        ]),
    ]).await.expect("SaveFitting");
    let fit_id = fit_rep.as_i64().unwrap_or(1);

    let rich_msg = format!(
        "<a href=\"showinfo:587//100000001\">Rifter</a> | <a href=\"fitting:587:434;1::\">Fit #{fit_id}</a> | <a href=\"killReport:1:abcdef\">KM</a> | <a href=\"bookmarkFolder:1\">BM</a> | <a href=\"overviewPreset:default\">OV</a>"
    );

    let chan_id = format!("solarsystemid2:{JITA_SYSTEM_ID}");
    let _ = rpc(&ctx, &mut s1, "XmppChatMgr", "AddCharacterToChannel", &[PyRep::String(chan_id.clone())]).await.expect("AddCharacterToChannel");
    let chat_rep = rpc(&ctx, &mut s1, "LSC", "SendMessage", &[PyRep::String(chan_id), PyRep::String(rich_msg.clone())]).await.expect("LSC.SendMessage");
    assert_eq!(chat_rep.dict_get("text"), Some(&PyRep::String(rich_msg.clone())));

    let mail_id_rep = rpc(&ctx, &mut s1, "mailMgr", "SendMail", &[
        PyRep::List(vec![PyRep::Integer(90_000_002)]),
        PyRep::String("Shared Links".into()),
        PyRep::String(rich_msg.clone()),
    ]).await.expect("mailMgr.SendMail");
    let mail_id = mail_id_rep.dict_get("messageID").and_then(|v| v.as_i64()).or_else(|| mail_id_rep.as_i64()).unwrap_or(1);

    let body_rep = rpc(&ctx, &mut s2, "mailMgr", "GetBody", &[PyRep::Integer(mail_id), PyRep::Bool(false)]).await.expect("mailMgr.GetBody");
    let body_str = body_rep.as_str().or_else(|| body_rep.dict_get("body").and_then(|v| v.as_str())).unwrap_or("");
    assert!(body_str.contains("showinfo:587//100000001") && body_str.contains("fitting:587:434;1::"));

    let fittings = rpc(&ctx, &mut s1, "charFittingMgr", "GetFittings", &[PyRep::Integer(90_000_001)]).await.expect("GetFittings");
    assert!(format!("{fittings:?}").contains("Hyperlink Rifter"));
}

#[tokio::test]
async fn test_t8_atomic_rollback_all_stores_on_rpc_and_wal_failure() {

    let db_path = temp_sqlite_path("t8_atomic");
    let repo = Arc::new(WorldRepository::open_sqlite(&db_path).expect("open sqlite"));
    let ctx = ServerContext::with_repository(repo);
    let demo = WorldRepository::with_demo_seed().with_snapshot(|s| s.clone());
    ctx.db.with_mut_snapshot(|snap| *snap = demo);
    crate::common::pin_clock(&ctx);

    let mut sess = SessionState::new(7110);
    let _ = rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)]).await.expect("SelectCharacterID");

    let bal_before = ctx.db.get_character(90_000_001).unwrap().balance;
    let items_before = ctx.db.with_snapshot(|snap| snap.items.len());
    let ledger_before = ctx.db.with_snapshot(|snap| snap.mining_ledger.len());
    let contracts_before = ctx.db.with_snapshot(|snap| snap.contracts.len());

    let fault_res: Result<(), String> = ctx.db.try_with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) { ch.balance = IskCents(123); }
        snap.items.insert(999_888_777, InventoryItemRecord {
            item_id: 999_888_777, type_id: 34, owner_id: 90_000_001, location_id: JITA_STATION_ID,
            flag: InventoryFlag::Hangar, quantity: 500, singleton: false, version: 1, ..Default::default()
        });
        snap.mining_ledger.push(MiningLedgerRecord {
            character_id: 90_000_001, solar_system_id: JITA_SYSTEM_ID,
            type_id: 1230, quantity: 9_999, date: "2026-10-23".into(), timestamp_unix: 1_700_000_000,
        });
        Err("injected multi-store failure".to_string())
    });
    assert!(fault_res.is_err());

    let rpc_fail = rpc(&ctx, &mut sess, "contractProxy", "CreateContract", &[
        PyRep::Integer(1), PyRep::Integer(0), PyRep::Integer(0), PyRep::Integer(1440), PyRep::Integer(1),
        PyRep::Integer(JITA_STATION_ID), PyRep::Integer(JITA_STATION_ID),
        PyRep::Real(1_000.0), PyRep::Real(999_999_999_999.0), PyRep::Real(0.0),
        PyRep::String("Atomic Rollback".into()), PyRep::String(String::new()),
        PyRep::List(vec![PyRep::Integer(100_000_003)]),
    ]).await;
    assert!(rpc_fail.is_err());

    assert_eq!(ctx.db.get_character(90_000_001).unwrap().balance, bal_before);
    assert_eq!(ctx.db.with_snapshot(|snap| snap.items.len()), items_before);
    assert_eq!(ctx.db.with_snapshot(|snap| snap.mining_ledger.len()), ledger_before);
    assert_eq!(ctx.db.with_snapshot(|snap| snap.contracts.len()), contracts_before);
    assert_eq!(ctx.db.get_item(100_000_003).unwrap().flag, InventoryFlag::Hangar);
    cleanup_sqlite_path(&db_path);
}

#[tokio::test]
async fn test_t9_no_transient_active_cycle_attrs_persisted_on_restart() {

    let db_path = temp_sqlite_path("t9_transient");
    let (ship_id, active_mod_id, mutated_mod_id) = (100_000_001_i64, 880_004_001_i64, 880_004_002_i64);

    {
        let repo = Arc::new(WorldRepository::open_sqlite(&db_path).expect("open sqlite"));
        let ctx = ServerContext::with_repository(repo);
        let demo = WorldRepository::with_demo_seed().with_snapshot(|s| s.clone());
        ctx.db.with_mut_snapshot(|snap| {
            *snap = demo;
            snap.module_active_states.insert(active_mod_id);
            if let Some(ship) = snap.items.get_mut(&ship_id) {
                ship.dynamic_attributes.insert(267, 0.15);
                ship.dynamic_attributes.insert(271, 0.05);
            }
            snap.items.insert(active_mod_id, InventoryItemRecord {
                item_id: active_mod_id, type_id: 434, owner_id: 90_000_001, location_id: ship_id,
                flag: InventoryFlag::MedSlot0, quantity: 1, singleton: true, version: 1,
                ..Default::default()
            });
            snap.items.insert(mutated_mod_id, InventoryItemRecord {
                item_id: mutated_mod_id, type_id: 434, owner_id: 90_000_001, location_id: ship_id,
                flag: InventoryFlag::MedSlot1, quantity: 1, singleton: true, version: 1,
                dynamic_attributes: [(2114u16, 1.0), (30u16, 45.0)].into_iter().collect(),
                ..Default::default()
            });
        });
    }

    let repo2 = Arc::new(WorldRepository::open_sqlite(&db_path).expect("reopen sqlite"));
    let ctx2 = ServerContext::with_repository(repo2);
    ctx2.db.with_snapshot(|snap| {
        assert!(snap.module_active_states.is_empty());
        let ship = snap.items.get(&ship_id).expect("active ship item");
        assert!(!ship.dynamic_attributes.contains_key(&267));
        assert!(!ship.dynamic_attributes.contains_key(&271));

        let mutated = snap.items.get(&mutated_mod_id).expect("mutated item");
        assert_eq!(mutated.dynamic_attributes.get(&2114).copied(), Some(1.0));
        assert_eq!(mutated.dynamic_attributes.get(&30).copied(), Some(45.0));
    });
    cleanup_sqlite_path(&db_path);
}
