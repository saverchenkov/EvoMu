//! Live end-to-end integration tests for Crimewatch kill rights and timer/flag notifications
//! with SQLite persistence.

#![forbid(unsafe_code)]

use crate::common;
use crate::parity_shard_5_crimewatch_sec_status_tests::{
    find_timer_notification, fire_and_destroy_target, login_player_session, open_sqlite_ctx, rpc,
    undock_player_to_space, TempSqliteDb, ASABONA_SYSTEM, SHIP_VELATOR,
};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use glam::DVec3;

#[allow(dead_code)]
const CHAR_ALPHA_ATTACKER: i64 = 90_000_001;
#[allow(dead_code)]
const CHAR_BETA_VICTIM: i64 = 90_000_002;
#[allow(dead_code)]
const CHAR_GAMMA_BUYER: i64 = 93_510_003;

#[allow(dead_code)]
const JITA_SYSTEM: u32 = 30_000_142;
#[allow(dead_code)]
const JITA_STATION: i64 = 60_003_760;
#[allow(dead_code)]
const ZARZAKH_SYSTEM: u32 = 30_100_000;

#[allow(dead_code)]
const SHIP_RIFTER: u32 = 587;
#[allow(dead_code)]
const MODULE_AUTOCANNON_II: u32 = 2889;
#[allow(dead_code)]
const NPC_SERPENTIS_INITIATE: u32 = 3744;

// parity: feat:crimewatch.kill-rights-gain
// parity: feat:crimewatch.kill-rights-sell
// parity: notify:OnWeaponsTimerUpdate
// parity: notify:OnPvpTimerUpdate
// parity: notify:OnCriminalTimerUpdate
// parity: notify:OnNpcTimerUpdate
// parity: notify:OnDisapprovalTimerUpdate
// parity: notify:OnSystemCriminalFlagUpdates
// parity: notify:OnSystemDisapprovalFlagUpdates
#[tokio::test]
async fn parity_shard_5_crimewatch_kill_rights_and_timers() {
    eve_testkit::attest(&[
        "feat:crimewatch.kill-rights-gain",
        "feat:crimewatch.kill-rights-sell",
        "notify:OnWeaponsTimerUpdate",
        "notify:OnPvpTimerUpdate",
        "notify:OnCriminalTimerUpdate",
        "notify:OnNpcTimerUpdate",
        "notify:OnDisapprovalTimerUpdate",
        "notify:OnSystemCriminalFlagUpdates",
        "notify:OnSystemDisapprovalFlagUpdates",
    ]);

    let db_guard = TempSqliteDb::new("crimewatch_kr_timers");
    let ctx = open_sqlite_ctx(db_guard.path(), common::TEST_CLOCK_BASE_UNIX);

    ctx.db
        .provision_player_account("kr_buyer_player", "kr-buyer-pass");
    let mut buyer_sess = SessionState::new(72_003);
    common::login_pilot(&ctx, &mut buyer_sess, "kr_buyer_player", "kr-buyer-pass").await;
    let anc = ctx.sde.ancestries.values().find(|a| a.bloodline_id == 1).map(|a| i64::from(a.ancestry_id)).unwrap_or(1);
    let sch = ctx.sde.schools.values().find(|s| s.race_id == 1).map(|s| i64::from(s.school_id)).unwrap_or(11);
    let b_rep = rpc(
        &ctx,
        &mut buyer_sess,
        "charUnboundMgr",
        "CreateCharacterWithDoll",
        &[
            PyRep::String("KrBuyerPlayer".into()),
            PyRep::Integer(1),
            PyRep::Integer(1),
            PyRep::Integer(anc),
            PyRep::Dict(vec![(PyRep::String("sculpting".into()), PyRep::List(vec![]))]),
            PyRep::Dict(vec![(
                PyRep::String("headLookTarget".into()),
                PyRep::Tuple(vec![PyRep::Real(0.0), PyRep::Real(0.0), PyRep::Real(1.0)]),
            )]),
            PyRep::Integer(sch),
        ],
    )
    .await
    .expect("CreateCharacterWithDoll buyer");
    let buyer_id = b_rep
        .as_i64()
        .or_else(|| b_rep.dict_get("characterID").and_then(|v| v.as_i64()))
        .expect("buyer char id");
    rpc(
        &ctx,
        &mut buyer_sess,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(buyer_id)],
    )
    .await
    .expect("Select buyer");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&buyer_id) {
            ch.balance = eve_db::IskCents::from_isk(50_000_000);
        }
    });

    let mut killer_sess =
        login_player_session(&ctx, 72_001, "demo", "demo-secret", CHAR_ALPHA_ATTACKER).await;
    let mut victim_sess =
        login_player_session(&ctx, 72_002, "other", "other-secret", CHAR_BETA_VICTIM).await;
    rpc(
        &ctx,
        &mut killer_sess,
        "crimewatch",
        "SetSafetyLevel",
        &[PyRep::Integer(0)],
    )
    .await
    .expect("SetSafetyLevel Red");

    // 1. High-sec aggression emits Weapons, PvP, Criminal, and SystemCriminalFlagUpdates notifications
    let (k_ship, _) =
        undock_player_to_space(&ctx, &mut killer_sess, JITA_SYSTEM, DVec3::new(30_000.0, 0.0, 0.0))
            .await;
    let (v_ship, _) =
        undock_player_to_space(&ctx, &mut victim_sess, JITA_SYSTEM, DVec3::new(31_500.0, 0.0, 0.0))
            .await;

    let _ = ctx.drain_notifications_for_char(CHAR_ALPHA_ATTACKER);
    let _ = killer_sess.drain_notifications();
    let jita_handle = ctx.systems.get_or_spawn(JITA_SYSTEM);
    rpc(
        &ctx,
        &mut killer_sess,
        "dogmaIM",
        "AddTarget",
        &[PyRep::Integer(v_ship)],
    )
    .await
    .expect("AddTarget Jita");
    jita_handle.step_manual(5.0).await.expect("lock step");
    rpc(
        &ctx,
        &mut killer_sess,
        "dogmaIM",
        "Activate",
        &[
            PyRep::Integer(100_000_005),
            PyRep::String("projectileFired".into()),
            PyRep::Integer(v_ship),
        ],
    )
    .await
    .expect("fire Jita");

    let jita_notifs = ctx.drain_notifications_for_char(CHAR_ALPHA_ATTACKER);
    let sess_notifs = killer_sess.drain_notifications();
    assert_eq!(
        find_timer_notification(&jita_notifs, "OnWeaponsTimerUpdate").map(|t| t.0),
        Some(101)
    );
    assert_eq!(
        find_timer_notification(&jita_notifs, "OnPvpTimerUpdate").map(|t| t.0),
        Some(201)
    );
    assert_eq!(
        find_timer_notification(&jita_notifs, "OnCriminalTimerUpdate").map(|t| t.0),
        Some(301)
    );
    assert!(
        jita_notifs
            .iter()
            .any(|n| n.event == "OnSystemCriminalFlagUpdates")
            || sess_notifs
                .iter()
                .any(|(ev, _)| ev == "OnSystemCriminalFlagUpdates"),
        "illegal high-sec aggression must broadcast OnSystemCriminalFlagUpdates"
    );

    // 2. High-sec ship kill creates 30-day kill right; test Sell -> CancelSell -> Sell -> Buy
    fire_and_destroy_target(&ctx, &mut killer_sess, JITA_SYSTEM, 100_000_005, v_ship).await;
    let PyRep::List(krs) = rpc(&ctx, &mut victim_sess, "bountyProxy", "GetMyKillRights", &[])
        .await
        .expect("GetMyKillRights") else {
        panic!("expected list of kill rights");
    };
    assert_eq!(krs.len(), 1);
    let kr_id = krs[0]
        .dict_get("killRightID")
        .and_then(|v| v.as_i64())
        .expect("killRightID");
    let exp = krs[0]
        .dict_get("expiryTime")
        .and_then(|v| v.as_i64())
        .expect("expiryTime");
    assert!(exp >= (ctx.now_unix() as i64) + 2_590_000);

    rpc(
        &ctx,
        &mut victim_sess,
        "bountyProxy",
        "SellKillRight",
        &[
            PyRep::Integer(kr_id),
            PyRep::Real(2_500_000.0),
            PyRep::None,
        ],
    )
    .await
    .expect("SellKillRight");
    rpc(
        &ctx,
        &mut victim_sess,
        "bountyProxy",
        "CancelSellKillRight",
        &[PyRep::Integer(kr_id)],
    )
    .await
    .expect("CancelSellKillRight");

    // Re-board ships in low-sec Asabona and verify Sell + Buy transfers 2.5M ISK & persists in SQLite
    ctx.db.with_mut_snapshot(|snap| {
        for (cid, tid) in [
            (CHAR_ALPHA_ATTACKER, SHIP_RIFTER),
            (CHAR_BETA_VICTIM, SHIP_VELATOR),
            (buyer_id, SHIP_VELATOR),
        ] {
            let sid = snap.next_entity_id + 1;
            snap.next_entity_id = sid + 1;
            snap.items.insert(
                sid,
                eve_db::InventoryItemRecord {
                    item_id: sid,
                    type_id: tid,
                    owner_id: cid,
                    location_id: JITA_STATION,
                    flag: eve_db::InventoryFlag::Hangar,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
            let ch = snap.characters.get_mut(&cid).unwrap();
            ch.active_ship_id = sid;
            ch.solar_system_id = ASABONA_SYSTEM;
            ch.station_id = None;
        }
    });
    let _ = undock_player_to_space(&ctx, &mut killer_sess, ASABONA_SYSTEM, DVec3::new(40_000.0, 0.0, 0.0)).await;
    let _ = undock_player_to_space(&ctx, &mut buyer_sess, ASABONA_SYSTEM, DVec3::new(42_000.0, 0.0, 0.0)).await;

    rpc(
        &ctx,
        &mut victim_sess,
        "bountyProxy",
        "SellKillRight",
        &[
            PyRep::Integer(kr_id),
            PyRep::Real(2_500_000.0),
            PyRep::Integer(buyer_id),
        ],
    )
    .await
    .expect("re-list SellKillRight");

    let vic_isk_pre = ctx.db.with_snapshot(|s| s.characters[&CHAR_BETA_VICTIM].balance.to_wire_f64());
    let buy_isk_pre = ctx.db.with_snapshot(|s| s.characters[&buyer_id].balance.to_wire_f64());
    rpc(
        &ctx,
        &mut buyer_sess,
        "killRightMgr",
        "BuyKillRight",
        &[PyRep::Integer(kr_id), PyRep::Real(2_500_000.0)],
    )
    .await
    .expect("BuyKillRight");
    assert!(
        (ctx.db.with_snapshot(|s| s.characters[&CHAR_BETA_VICTIM].balance.to_wire_f64())
            - vic_isk_pre
            - 2_500_000.0)
            .abs()
            < 1.0
    );
    assert!(
        (buy_isk_pre
            - ctx.db.with_snapshot(|s| s.characters[&buyer_id].balance.to_wire_f64())
            - 2_500_000.0)
            .abs()
            < 1.0
    );

    // 3. NPC combat timer (OnNpcTimerUpdate) & Zarzakh disapproval timers (OnDisapprovalTimerUpdate / OnSystemDisapprovalFlagUpdates)
    ctx.advance_secs(905);
    let _ = rpc(&ctx, &mut buyer_sess, "stationSvc", "Undock", &[]).await;
    let gun_for_killer = ctx.db.with_mut_snapshot(|snap| {
        let gid = snap.next_entity_id + 1;
        snap.next_entity_id = gid + 1;
        snap.items.insert(
            gid,
            eve_db::InventoryItemRecord {
                item_id: gid,
                type_id: MODULE_AUTOCANNON_II,
                owner_id: CHAR_ALPHA_ATTACKER,
                location_id: snap.characters[&CHAR_ALPHA_ATTACKER].active_ship_id,
                flag: eve_db::InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        gid
    });
    let npc_ball = eve_server::services::combat_crimewatch::combat_npc_ai::spawn_specialized_npc_entity(
        &ctx,
        ASABONA_SYSTEM,
        NPC_SERPENTIS_INITIATE,
        "Serpentis Initiate",
        4001,
        DVec3::new(41_000.0, 0.0, 0.0),
    )
    .await;
    let asa_handle = ctx.systems.get_or_spawn(ASABONA_SYSTEM);
    rpc(
        &ctx,
        &mut killer_sess,
        "dogmaIM",
        "AddTarget",
        &[PyRep::Integer(npc_ball)],
    )
    .await
    .expect("target npc");
    asa_handle.step_manual(5.0).await.expect("lock npc");
    let _ = ctx.drain_notifications_for_char(CHAR_ALPHA_ATTACKER);
    rpc(
        &ctx,
        &mut killer_sess,
        "dogmaIM",
        "Activate",
        &[
            PyRep::Integer(gun_for_killer),
            PyRep::String("projectileFired".into()),
            PyRep::Integer(npc_ball),
        ],
    )
    .await
    .expect("fire npc");
    let npc_notifs = ctx.drain_notifications_for_char(CHAR_ALPHA_ATTACKER);
    assert_eq!(
        find_timer_notification(&npc_notifs, "OnNpcTimerUpdate").map(|t| t.0),
        Some(401)
    );

    // Move to Zarzakh (30_100_000) and fire at buyer ship -> OnDisapprovalTimerUpdate + OnSystemDisapprovalFlagUpdates
    let _ = undock_player_to_space(&ctx, &mut killer_sess, ZARZAKH_SYSTEM, DVec3::new(10_000.0, 0.0, 0.0)).await;
    let (b_ship_z, _) = undock_player_to_space(&ctx, &mut buyer_sess, ZARZAKH_SYSTEM, DVec3::new(11_500.0, 0.0, 0.0)).await;
    let zar_handle = ctx.systems.get_or_spawn(ZARZAKH_SYSTEM);
    rpc(
        &ctx,
        &mut killer_sess,
        "dogmaIM",
        "AddTarget",
        &[PyRep::Integer(b_ship_z)],
    )
    .await
    .expect("target Zarzakh");
    zar_handle.step_manual(5.0).await.expect("lock Zarzakh");
    let _ = ctx.drain_notifications_for_char(CHAR_ALPHA_ATTACKER);
    let _ = killer_sess.drain_notifications();
    rpc(
        &ctx,
        &mut killer_sess,
        "dogmaIM",
        "Activate",
        &[
            PyRep::Integer(gun_for_killer),
            PyRep::String("projectileFired".into()),
            PyRep::Integer(b_ship_z),
        ],
    )
    .await
    .expect("fire Zarzakh");
    let z_notifs = ctx.drain_notifications_for_char(CHAR_ALPHA_ATTACKER);
    let z_sess = killer_sess.drain_notifications();
    assert!(
        find_timer_notification(&z_notifs, "OnDisapprovalTimerUpdate").is_some()
            || z_sess
                .iter()
                .any(|(ev, _)| ev == "OnDisapprovalTimerUpdate"),
        "combat in Zarzakh must emit OnDisapprovalTimerUpdate"
    );
    assert!(
        z_notifs
            .iter()
            .any(|n| n.event == "OnSystemDisapprovalFlagUpdates")
            || z_sess
                .iter()
                .any(|(ev, _)| ev == "OnSystemDisapprovalFlagUpdates"),
        "combat in Zarzakh must emit OnSystemDisapprovalFlagUpdates"
    );
    let _ = k_ship;
}
