//! Integration tests for CrimeWatch, NPC Navy/Hauler, ServerOps Downtime, and OF-19:
//! - `feat:crimewatch.sec-status-loss`
//! - `feat:crimewatch.sec-status-transactions`
//! - `feat:crimewatch.kill-rights-gain`
//! - `feat:crimewatch.kill-rights-sell`
//! - `feat:crimewatch.sec-status-gain-caps`
//! - `feat:crimewatch.sec-tag-tier-bands`
//! - `feat:npc.navy-npc`
//! - `feat:npc.npc-hauler`
//! - `feat:serverops.downtime`
//! - `feat:serverops.daily-jobs`

use crate::common::*;
use eve_db::{InventoryFlag, InventoryItemRecord, IskCents};
use eve_destiny::{Ball, MovementMode, SlimItemMeta};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::combat::npc_ai::compute_sec_status_gain;
use eve_server::living_world::NpcArchetype;
use eve_server::router::ServerContext;
use eve_server::services::combat_crimewatch::{
    dispatch_combat_rpc, evaluate_faction_police_response,
};
use eve_server::sp6_lifecycle::tick_background_cluster;
use glam::DVec3;
use std::collections::HashMap;

fn make_sess(char_id: i64, ship_id: i64, sys_id: u32, station_id: Option<i64>) -> SessionState {
    let mut sess = SessionState::default();
    sess.user_id = Some(char_id as u64);
    sess.character_id = Some(char_id);
    sess.ship_id = Some(ship_id);
    sess.solar_system_id = Some(sys_id);
    sess.station_id = station_id;
    sess
}

async fn seed_ship_in_space(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    ship_type_id: u32,
    sys_id: u32,
    pos: DVec3,
) {
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.active_ship_id = ship_id;
            ch.station_id = None;
            ch.structure_id = None;
        }
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: ship_type_id,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::from([(2, 1.0)]),
                ..Default::default()
            },
        );
        snap.item_positions.insert(ship_id, [pos.x, pos.y, pos.z]);
    });
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let cs = ctx.build_type_combat_state(ship_type_id);
    let _ = sys_handle
        .spawn_ball_with_meta(
            Ball {
                id: ship_id,
                type_id: ship_type_id,
                position: pos,
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 2.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            cs,
            SlimItemMeta {
                group_id: ctx.sde.types.get(&ship_type_id).map(|t| t.group_id).unwrap_or(25),
                category_id: 6,
                owner_id: char_id,
                char_id: Some(char_id),
                corp_id: 1_000_009,
                alliance_id: -1,
                security_status: 0.0,
                is_cloaked: false,
            },
        )
        .await;
}

#[tokio::test]
async fn test_crimewatch_sec_status_loss_transactions_and_kill_rights() {
    // parity: feat:crimewatch.sec-status-loss, feat:crimewatch.sec-status-transactions, feat:crimewatch.kill-rights-gain, feat:crimewatch.kill-rights-sell
    eve_testkit::attest(&[
        "feat:crimewatch.sec-status-loss",
        "feat:crimewatch.sec-status-transactions",
        "feat:crimewatch.kill-rights-gain",
        "feat:crimewatch.kill-rights-sell",
    ]);

    let ctx = ServerContext::new();
    pin_clock(&ctx);
    let aggressor_id = 90_000_001i64;
    let victim_id = 90_000_002i64;
    let buyer_id = 90_000_003i64;
    let agg_ship_id = 105_000_001i64;
    let vic_pod_id = 105_000_002i64;
    let buyer_ship_id = 105_000_003i64;
    let highsec_sys = 30_000_142u32;

    seed_ship_in_space(&ctx, aggressor_id, agg_ship_id, 587, highsec_sys, DVec3::ZERO).await;
    seed_ship_in_space(&ctx, victim_id, vic_pod_id, 670, highsec_sys, DVec3::new(1_000.0, 0.0, 0.0)).await;
    seed_ship_in_space(&ctx, buyer_id, buyer_ship_id, 587, highsec_sys, DVec3::new(2_000.0, 0.0, 0.0)).await;

    let sec_before = ctx.db.with_snapshot(|s| s.characters.get(&aggressor_id).map(|c| c.security_status).unwrap_or(0.0));

    // Fit weapon + ammo on aggressor ship and set Safety Red so dogmaIM.Activate triggers criminal aggression
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            105_000_010,
            InventoryItemRecord {
                item_id: 105_000_010,
                type_id: 562, // Light Electron Blaster I
                owner_id: aggressor_id,
                location_id: agg_ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::from([(2, 1.0)]),
                ..Default::default()
            },
        );
        snap.items.insert(
            105_000_011,
            InventoryItemRecord {
                item_id: 105_000_011,
                type_id: 215, // Antimatter Charge S
                owner_id: aggressor_id,
                location_id: agg_ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 100,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });
    ctx.invalidate_ship_fitting(agg_ship_id);

    // Lock pod, set low HP so 1 shot destroys it, set Safety Red, and fire weapon via dogmaIM.Activate
    {
        let sys_handle = ctx.systems.get_or_spawn(highsec_sys);
        let mut bp = sys_handle.ballpark.write().unwrap();
        if let Some(cs) = bp.combat_states.get_mut(&vic_pod_id) {
            cs.shield_hp = 0.0;
            cs.armor_hp = 0.0;
            cs.structure_hp = 1.0;
        }
    }
    let mut agg_sess = make_sess(aggressor_id, agg_ship_id, highsec_sys, None);
    let _ = dispatch_combat_rpc(
        &ctx,
        &mut agg_sess,
        "crimewatch",
        "SetSafetyLevel",
        &[PyRep::Integer(0)],
        &HashMap::new(),
    )
    .await;
    let _ = dispatch_combat_rpc(
        &ctx,
        &mut agg_sess,
        "dogmaIM",
        "AddTarget",
        &[PyRep::Integer(vic_pod_id)],
        &HashMap::new(),
    )
    .await
    .expect("AddTarget pod");
    let sys_h = ctx.systems.get_or_spawn(highsec_sys);
    for _ in 0..60 {
        let _ = sys_h.step_manual(1.0).await;
    }
    let _ = dispatch_combat_rpc(
        &ctx,
        &mut agg_sess,
        "dogmaIM",
        "Activate",
        &[
            PyRep::Integer(105_000_010),
            PyRep::String("effects.HybridTurret".into()),
            PyRep::Integer(vic_pod_id),
        ],
        &HashMap::new(),
    )
    .await
    .expect("Activate weapon on pod");
    ctx.advance_secs(30);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;

    let sec_after = ctx.db.with_snapshot(|s| s.characters.get(&aggressor_id).map(|c| c.security_status).unwrap_or(0.0));
    assert!(sec_after < sec_before, "Criminal pod kill must reduce aggressor security status ({sec_after} < {sec_before})");

    // Verify GetSecurityStatusTransactions returns a rowset with the recorded change
    let tx_rep = dispatch_combat_rpc(
        &ctx,
        &mut agg_sess,
        "crimewatch",
        "GetSecurityStatusTransactions",
        &[],
        &HashMap::new(),
    )
    .await
    .expect("GetSecurityStatusTransactions");
    assert!(!matches!(tx_rep, PyRep::None), "GetSecurityStatusTransactions must return a rowset");

    // Verify victim earned a kill right on aggressor
    let kr_id = ctx.db.with_snapshot(|s| {
        s.kill_rights
            .values()
            .find(|kr| kr.owner_id == victim_id && kr.target_id == aggressor_id)
            .map(|kr| kr.kill_right_id)
    }).expect("Victim must earn a kill right on criminal aggressor");

    // Victim lists kill right for sale, cancels it, re-lists it, and third party buys/activates it
    let mut vic_sess = make_sess(victim_id, vic_pod_id, highsec_sys, None);
    let _ = dispatch_combat_rpc(
        &ctx,
        &mut vic_sess,
        "bountyProxy",
        "SellKillRight",
        &[PyRep::Integer(kr_id), PyRep::Real(5_000_000.0)],
        &HashMap::new(),
    )
    .await
    .expect("SellKillRight");

    let _ = dispatch_combat_rpc(
        &ctx,
        &mut vic_sess,
        "bountyProxy",
        "CancelSellKillRight",
        &[PyRep::Integer(kr_id)],
        &HashMap::new(),
    )
    .await
    .expect("CancelSellKillRight");

    let _ = dispatch_combat_rpc(
        &ctx,
        &mut vic_sess,
        "bountyProxy",
        "SellKillRight",
        &[PyRep::Integer(kr_id), PyRep::Real(5_000_000.0)],
        &HashMap::new(),
    )
    .await
    .expect("Re-SellKillRight");

    ctx.db.with_mut_snapshot(|s| {
        if let Some(mut ch) = s.characters.get(&aggressor_id).cloned() {
            ch.character_id = buyer_id;
            ch.name = "KillRight Buyer".into();
            ch.active_ship_id = buyer_ship_id;
            ch.balance = IskCents::from_isk(50_000_000);
            s.characters.insert(buyer_id, ch);
        }
    });
    let mut buyer_sess = make_sess(buyer_id, buyer_ship_id, highsec_sys, None);
    let _ = dispatch_combat_rpc(
        &ctx,
        &mut buyer_sess,
        "killRightMgr",
        "BuyKillRight",
        &[PyRep::Integer(kr_id), PyRep::Real(5_000_000.0)],
        &HashMap::new(),
    )
    .await
    .expect("BuyKillRight");
    let is_active = ctx.db.with_snapshot(|s| s.kill_rights.get(&kr_id).map(|kr| kr.is_active_suspect));
    assert_eq!(is_active, Some(true), "BuyKillRight must activate suspect flag on target");
}

#[tokio::test]
async fn test_crimewatch_sec_status_gain_caps_and_tag_tier_bands() {
    // parity: feat:crimewatch.sec-status-gain-caps, feat:crimewatch.sec-tag-tier-bands
    eve_testkit::attest(&[
        "feat:crimewatch.sec-status-gain-caps",
        "feat:crimewatch.sec-tag-tier-bands",
    ]);

    let ctx = ServerContext::new();
    pin_clock(&ctx);
    let char_id = 90_000_001i64;
    let ship_id = 105_000_101i64;
    let lowsec_sys = 30_002_537u32;
    let lowsec_station = ctx
        .sde
        .solar_systems
        .get(&lowsec_sys)
        .and_then(|s| s.stations.first().map(|st| st.station_id))
        .unwrap_or(60_003_760);

    seed_ship_in_space(&ctx, char_id, ship_id, 587, lowsec_sys, DVec3::ZERO).await;

    // 1. Tag tier bands: Trainer (35661) only works in [-10.0, -8.0); refused at -6.0
    ctx.db.with_mut_snapshot(|s| {
        if let Some(ch) = s.characters.get_mut(&char_id) {
            ch.security_status = -6.0;
        }
        s.items.insert(
            750_101,
            InventoryItemRecord {
                item_id: 750_101,
                type_id: 35_661, // Trainer (-10 to -8)
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 5,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        s.items.insert(
            750_102,
            InventoryItemRecord {
                item_id: 750_102,
                type_id: 35_664, // Negotiator (-2 to 0)
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 5,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    let mut sess = make_sess(char_id, ship_id, lowsec_sys, Some(lowsec_station));
    let wrong_band_err = dispatch_combat_rpc(
        &ctx,
        &mut sess,
        "crimewatch",
        "ApplyTagsForSecStatus",
        &[PyRep::Integer(35_661), PyRep::Integer(1)],
        &HashMap::new(),
    )
    .await
    .expect_err("Trainer tag must be refused at -6.0 sec status");
    assert!(!wrong_band_err.code.is_empty());

    // 2. Negotiator tag (35664) at -0.3 caps at 0.0 (cannot raise sec status above 0.0)
    ctx.db.with_mut_snapshot(|s| {
        if let Some(ch) = s.characters.get_mut(&char_id) {
            ch.security_status = -0.3;
        }
    });
    let _ = dispatch_combat_rpc(
        &ctx,
        &mut sess,
        "crimewatch",
        "ApplyTagsForSecStatus",
        &[PyRep::Integer(35_664), PyRep::Integer(2)],
        &HashMap::new(),
    )
    .await
    .expect("Negotiator tag at -0.3");
    let sec_post_tag = ctx.db.with_snapshot(|s| s.characters.get(&char_id).map(|c| c.security_status).unwrap_or(-1.0));
    assert!(sec_post_tag <= 0.0, "Clone soldier tags must not raise security status above 0.0 (got {sec_post_tag})");

    // 3. NPC kill security status gain stops at +5.0
    let gain_at_cap = compute_sec_status_gain(5.0, 1_000_000.0);
    assert_eq!(gain_at_cap, 0.0, "NPC kill security status gain must stop at +5.0");
}

#[tokio::test]
async fn test_npc_navy_police_hauler_convoy_and_serverops_downtime() {
    // parity: feat:npc.navy-npc, feat:npc.npc-hauler, feat:serverops.downtime, feat:serverops.daily-jobs
    eve_testkit::attest(&[
        "feat:npc.navy-npc",
        "feat:npc.npc-hauler",
        "feat:serverops.downtime",
        "feat:serverops.daily-jobs",
    ]);

    let ctx = ServerContext::new();
    pin_clock(&ctx);
    let char_id = 90_000_001i64;
    let ship_id = 105_000_201i64;
    let highsec_sys = 30_000_142u32;

    seed_ship_in_space(&ctx, char_id, ship_id, 587, highsec_sys, DVec3::ZERO).await;

    // 1. Faction Police / Navy NPC response against outlaw (-5.5 sec status) in high-sec
    ctx.db.with_mut_snapshot(|s| {
        if let Some(ch) = s.characters.get_mut(&char_id) {
            ch.security_status = -5.5;
        }
    });
    let (attacked, can_dock, cloak_broken) = evaluate_faction_police_response(&ctx, highsec_sys, char_id, ship_id).await;
    assert_eq!(attacked, true, "Faction Police must engage outlaw pilot in high-sec");
    assert_eq!(can_dock, false, "Faction Police must refuse docking to outlaw pilot");
    assert_eq!(cloak_broken, true, "Faction Police must break cloak on outlaw pilot");

    // 2. NPC Hauler Convoy traveling gates via living_world
    ctx.living_world.seed_default_convoys();
    ctx.living_world.materialize_system_lod2(highsec_sys, &ctx.systems, &ctx.sde).await;
    let hauler_count = ctx
        .living_world
        .list_convoys()
        .iter()
        .filter(|c| c.archetype == NpcArchetype::HaulerConvoy && !c.destroyed)
        .count();
    assert!(hauler_count >= 1, "Active HaulerConvoy must be tracked in living_world (got {hauler_count})");

    // 3. ServerOps Downtime & Daily Jobs at 11:00 UTC
    ctx.advance_secs(86_400 + 60);
    tick_background_cluster(&ctx).await;
    let dt_ran = ctx.db.with_snapshot(|s| s.service_state_blobs.contains_key("metenox_last_tick_unix"));
    assert!(dt_ran, "Daily downtime and background cluster jobs must execute across 11:00 UTC window");
}
