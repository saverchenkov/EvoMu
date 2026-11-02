//! Integration tests for CrimeWatch, NPC Navy/Hauler, ServerOps Downtime, and OF-19.

use crate::common::*;
use eve_db::{InventoryFlag, InventoryItemRecord, IskCents};
use eve_destiny::{Ball, MovementMode, SlimItemMeta};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::living_world::NpcArchetype;
use eve_server::router::ServerContext;
use eve_server::services::combat_crimewatch::{dispatch_combat_rpc, evaluate_faction_police_response};
use eve_server::sp6_lifecycle::tick_background_cluster;
use glam::DVec3;
use std::collections::HashMap;

fn make_sess(char_id: i64, ship_id: i64, sys_id: u32, station_id: Option<i64>) -> SessionState {
    SessionState {
        user_id: Some(char_id as u64),
        character_id: Some(char_id),
        ship_id: Some(ship_id),
        solar_system_id: Some(sys_id),
        station_id,
        ..Default::default()
    }
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
                ..Default::default()
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
    if let PyRep::List(rows) = &tx_rep {
        assert!(!rows.is_empty(), "GetSecurityStatusTransactions must contain at least one transaction");
    }

    // Verify victim earned a 30-day expiring kill right on aggressor
    let (kr_id, kr_exp) = ctx.db.with_snapshot(|s| {
        let kr = s.kill_rights.values().find(|kr| kr.owner_id == victim_id && kr.target_id == aggressor_id)?;
        let exp = s.combat_crimewatch_state.kill_right_expires_unix.get(&kr.kill_right_id).copied().unwrap_or(0);
        Some((kr.kill_right_id, exp))
    }).expect("Victim must earn a kill right on criminal aggressor");
    assert!(kr_exp >= ctx.now_unix() + 30 * 86_400 - 120, "Kill right must carry a 30-day expiration timestamp");

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
    let (is_active, has_buyer_j, has_seller_j) = ctx.db.with_snapshot(|s| (
        s.kill_rights.get(&kr_id).map(|kr| kr.is_active_suspect),
        s.wallet_journal.iter().any(|j| j.character_id == buyer_id && j.entry_type == "KillRightFee" && j.amount.0 < 0),
        s.wallet_journal.iter().any(|j| j.character_id == victim_id && j.entry_type == "KillRightFee" && j.amount.0 > 0),
    ));
    assert_eq!(is_active, Some(true), "BuyKillRight must activate suspect flag on target");
    assert!(has_buyer_j && has_seller_j, "BuyKillRight must append KillRightFee wallet journal entries for buyer and seller");
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
    let sec_office_svc = ctx.sde.station_services.values().find(|s| s.name == "Security Office").map(|s| s.service_id).unwrap_or(26);
    let (lowsec_station, lowsec_sys) = ctx.sde.stations.values().find(|st| {
        (matches!(st.owner_corporation_id, 1_000_125 | 1_000_137) || st.owner_faction_id == Some(500_006))
            && ctx.sde.solar_systems.get(&st.solar_system_id).is_some_and(|s| s.security_status > 0.0 && s.security_status < 0.45)
            && ctx.sde.station_operations.get(&st.operation_id).is_some_and(|op| op.services.contains(&sec_office_svc))
    }).map(|st| (st.station_id, st.solar_system_id)).expect("lowsec CONCORD Security Office station");

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

    // 2. Negotiator tag (35664) at -0.3 gives full +0.5 -> 0.2 (consuming 1 tag), then refuses further tags >= 0.0
    ctx.db.with_mut_snapshot(|s| {
        if let Some(ch) = s.characters.get_mut(&char_id) {
            ch.security_status = -0.3;
            ch.balance = eve_db::IskCents::from_isk(20_000_000);
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
    assert!((sec_post_tag - 0.2).abs() < 1e-6, "Last tag at -0.3 must give full +0.5 -> 0.2 (got {sec_post_tag})");
    assert!(dispatch_combat_rpc(&ctx, &mut sess, "crimewatch", "ApplyTagsForSecStatus", &[PyRep::Integer(35_664), PyRep::Integer(1)], &HashMap::new()).await.is_err());

    // 3. Real belt, mission, and incursion NPC kills through live combat/death pipeline cap at +5.0
    ctx.db.with_mut_snapshot(|s| {
        if let Some(ch) = s.characters.get_mut(&char_id) {
            ch.security_status = 4.9999;
        }
    });
    for (idx, archetype) in ["Belt", "Mission", "Incursion"].iter().enumerate() {
        let npc_pos = DVec3::new(10_000.0 + (idx as f64) * 2_000.0, 0.0, 0.0);
        let npc_id = eve_server::services::combat_crimewatch::spawn_specialized_npc_entity(
            &ctx,
            lowsec_sys,
            16_934, // Corpus Prophet (431,250 ISK SDE bounty)
            archetype,
            1,
            npc_pos,
        )
        .await;
        eve_server::services::combat_crimewatch::combat_missiles_aoe::handle_destroyed_ball_from_missile_or_aoe(
            &ctx,
            lowsec_sys,
            char_id,
            npc_id,
            16_934,
            npc_pos,
            ctx.now_unix(),
        )
        .await;
    }
    let sec_at_cap = ctx.db.with_snapshot(|s| s.characters.get(&char_id).map(|c| c.security_status).unwrap_or(0.0));
    assert_eq!(sec_at_cap, 5.0, "Belt, mission, and incursion NPC kills must raise to +5.0 and stop at +5.0");

    // 4. Verify live server CrimeWatch security status clamping at -10.0 on illegal capsule kill
    let pod_victim_char = 90_000_002i64;
    let pod_ship_id = 105_000_199i64;
    seed_ship_in_space(&ctx, pod_victim_char, pod_ship_id, 670, lowsec_sys, DVec3::new(3_000.0, 0.0, 0.0)).await;
    ctx.db.with_mut_snapshot(|s| {
        if let Some(ch) = s.characters.get_mut(&char_id) {
            ch.security_status = -9.5;
        }
    });
    eve_server::services::combat_crimewatch::combat_missiles_aoe::handle_destroyed_ball_from_missile_or_aoe(
        &ctx,
        lowsec_sys,
        char_id,
        pod_ship_id,
        670,
        DVec3::new(3_000.0, 0.0, 0.0),
        ctx.now_unix(),
    )
    .await;
    let (sec_clamped_low, low_pod_kr) = ctx.db.with_snapshot(|s| (
        s.characters.get(&char_id).map(|c| c.security_status).unwrap_or(0.0),
        s.kill_rights.values().any(|kr| kr.owner_id == pod_victim_char && kr.target_id == char_id),
    ));
    assert_eq!(sec_clamped_low, -10.0, "Illegal capsule kill below -8.0 must clamp character security_status at -10.0");
    assert!(low_pod_kr, "Illegal Lowsec capsule kill must grant a 30-day Kill Right to the podded victim");
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
    ctx.set_clock(std::sync::Arc::new(eve_db::VirtualServerClock::with_fixed_base(1_700_045_970))); // 10:59:30 UTC
    let char_id = 90_000_001i64;
    let ship_id = 105_000_201i64;
    let highsec_sys = 30_000_142u32;

    seed_ship_in_space(&ctx, char_id, ship_id, 587, highsec_sys, DVec3::ZERO).await;
    tick_background_cluster(&ctx).await; // Seed pre-downtime day index at 10:59:30 UTC

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
    let convoy_steps = ctx.living_world.step_all_hauler_convoys(&ctx.systems, &ctx.db, &ctx.sde).await;
    assert!(convoy_steps.iter().all(|(_, r)| r.is_ok()), "step_all_hauler_convoys must succeed");
    let hauler_count = ctx
        .living_world
        .list_convoys()
        .iter()
        .filter(|c| c.archetype == NpcArchetype::HaulerConvoy && !c.destroyed)
        .count();
    assert!(hauler_count >= 1, "Active HaulerConvoy must be tracked in living_world (got {hauler_count})");

    // 3. ServerOps Downtime & Daily Jobs across 11:00 UTC: real NPC wreck cleanup & accepted mission reset
    let npc_id = 1_880_000_099i64;
    let npc_pos = DVec3::new(25_000.0, 0.0, 0.0);
    let sys = ctx.systems.get_or_spawn(highsec_sys);
    let npc_ball = eve_server::services::pve_incursion_sites::make_site_npc_ball(npc_id, 24_000, npc_pos);
    let _ = sys.spawn_ball_with_combat(npc_ball, ctx.build_type_combat_state(24_000)).await;
    eve_server::services::combat_crimewatch::combat_missiles_aoe::handle_destroyed_ball_from_missile_or_aoe(
        &ctx, highsec_sys, char_id, npc_id, 24_000, npc_pos, ctx.now_unix(),
    )
    .await;
    let expired_wreck_id = eve_server::sp6_lifecycle::with_sp6_state(&ctx.db, |st| {
        st.space_object_meta.iter().find(|(_, m)| m.is_npc_wreck).map(|(&id, _)| id)
    })
    .expect("Death pipeline must spawn an NPC wreck with is_npc_wreck = true");
    ctx.db.with_mut_snapshot(|s| {
        s.active_missions.insert((char_id, 3_008_416), eve_db::ActiveMissionRecord {
            mission_id: 1, character_id: char_id, agent_id: 3_008_416, solar_system_id: highsec_sys,
            state: "Accepted".into(), current_room_id: 2, npcs_remaining: 1,
            spawned_npc_ball_ids: vec![1_880_000_098], bookmark_id: None,
        });
    });

    ctx.advance_secs(60); // 11:00:30 UTC (only 60s elapsed, so 2h decay has not fired)
    tick_background_cluster(&ctx).await;
    let wreck_in_meta = eve_server::sp6_lifecycle::with_sp6_state(&ctx.db, |st| {
        st.space_object_meta.contains_key(&expired_wreck_id)
    });
    let (wreck_in_items, mission_room, mission_npcs) = ctx.db.with_snapshot(|s| {
        let m = s.active_missions.get(&(char_id, 3_008_416)).unwrap();
        (s.items.contains_key(&expired_wreck_id), m.current_room_id, m.npcs_remaining)
    });
    assert!(!wreck_in_items && !wreck_in_meta, "11:00 UTC downtime must delete NPC wreck");
    assert_eq!((mission_room, mission_npcs), (1, 4), "11:00 UTC downtime must reset accepted mission waves");
}

#[tokio::test]
async fn test_capital_construction_restrictions_and_insurance_contract_lifecycle() {
    // parity: feat:industry.capital-construction-restrictions, feat:combat.insurance-void-and-expiry
    eve_testkit::attest(&[
        "feat:industry.capital-construction-restrictions",
        "feat:combat.insurance-void-and-expiry",
    ]);

    let sde = match eve_sde::SdeRegistry::open(&eve_sde::SdeRegistry::default_db_path()) {
        Ok(reg) => std::sync::Arc::new(reg),
        Err(_) => std::sync::Arc::new(eve_sde::SdeRegistry::with_canonical_seed()),
    };
    let db = std::sync::Arc::new(eve_db::WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(sde, db)
        .with_seeded_rng(eve_server::rng::TEST_MODE_DEFAULT_SEED);
    pin_clock(&ctx);
    let char_id = 90_000_001i64;
    let ship_id = 104_000_301i64;
    let highsec_sys = 30_000_142u32;
    let jita_station = 60_003_760i64;

    seed_ship_in_space(&ctx, char_id, ship_id, 587, highsec_sys, DVec3::ZERO).await;

    // 1. Capital manufacturing job in high-sec NPC station is refused
    let bp_id = 104_000_302i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = Some(jita_station);
        }
        snap.items.insert(
            bp_id,
            InventoryItemRecord {
                item_id: bp_id,
                type_id: 19_721, // Revelation Blueprint
                owner_id: char_id,
                location_id: jita_station,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    let mut sess = make_sess(char_id, ship_id, highsec_sys, Some(jita_station));
    let cap_err = ctx
        .dispatch_rpc(
            &mut sess,
            "industryManager",
            None,
            "InstallJob",
            &[PyRep::keyval([
                ("blueprintID", PyRep::Integer(bp_id)),
                ("facilityID", PyRep::Integer(jita_station)),
                ("activityID", PyRep::Integer(1)),
                ("runs", PyRep::Integer(1)),
            ])],
            &HashMap::new(),
            None,
        )
        .await
        .expect_err("Capital ship manufacturing in high-sec NPC station must be refused");
    assert_eq!(cap_err.code, "CapitalConstructionForbiddenInHighSec");

    // 1b. Supercapital manufacturing in low-sec NPC station is refused
    let super_bp_id = 104_000_303i64;
    let lowsec_sys = 30_002_537u32;
    let lowsec_station = ctx
        .sde
        .solar_systems
        .get(&lowsec_sys)
        .and_then(|s| s.stations.first().map(|st| st.station_id))
        .unwrap_or(60_003_760);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = Some(lowsec_station);
        }
        snap.items.insert(
            super_bp_id,
            InventoryItemRecord {
                item_id: super_bp_id,
                type_id: 23_918, // Nyx Blueprint
                owner_id: char_id,
                location_id: lowsec_station,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    let mut low_sess = make_sess(char_id, ship_id, lowsec_sys, Some(lowsec_station));
    let super_err = ctx
        .dispatch_rpc(
            &mut low_sess,
            "industryManager",
            None,
            "InstallJob",
            &[PyRep::keyval([
                ("blueprintID", PyRep::Integer(super_bp_id)),
                ("facilityID", PyRep::Integer(lowsec_station)),
                ("activityID", PyRep::Integer(1)),
                ("runs", PyRep::Integer(1)),
            ])],
            &HashMap::new(),
            None,
        )
        .await
        .expect_err("Supercapital manufacturing in low-sec NPC station must be refused");
    assert_eq!(super_err.code, "SupercapitalRequiresSovereignNullSec");

    // 2. Insurance contract via real InsureShip RPC: 12-week expiry -> 40% fallback + EVE-mail, and CONCORD criminal voiding
    let cost_isk = 1.0_f64; // Platinum fraction (1.0)
    let _ = ctx
        .dispatch_rpc(
            &mut sess,
            "insuranceSvc",
            None,
            "InsureShip",
            &[PyRep::Integer(ship_id), PyRep::Real(cost_isk), PyRep::Bool(false), PyRep::Bool(true)],
            &HashMap::new(),
            None,
        )
        .await
        .expect("InsureShip");
    let full_payout = eve_server::services::combat_crimewatch::with_combat_runtime(&ctx, |rt| {
        rt.insurance_contracts.get(&ship_id).map(|c| c.payout_isk).unwrap_or(0.0)
    });
    assert!(full_payout > 0.0);

    // Advance past 12 weeks (7,257,600s) -> expired contract falls back to 40% payout and sends EVE-mail
    ctx.advance_secs(12 * 7 * 86_400 + 60);
    let mails_before = ctx.db.with_snapshot(|s| s.mails.len());
    let payout = eve_server::services::combat_crimewatch::process_ship_insurance_payout(&ctx, char_id, ship_id, 587, false);
    assert!(payout > 0.0 && payout < full_payout, "Expired insurance must fall back to 40% payout");
    let has_mail = ctx.db.with_snapshot(|s| {
        s.mails.len() > mails_before && s.mails.iter().any(|m| m.recipient_id == char_id && m.subject.contains("Insurance Payout"))
    });
    assert!(has_mail, "Insurance payout must send an EVE-mail");

    // CONCORD criminal death, self-destruct, and repackaging all void active insurance contracts (0.0 payout)
    let ship_id_2 = 104_000_304i64;
    seed_ship_in_space(&ctx, char_id, ship_id_2, 587, highsec_sys, DVec3::ZERO).await;
    let _ = ctx.dispatch_rpc(&mut sess, "insuranceSvc", None, "InsureShip", &[PyRep::Integer(ship_id_2), PyRep::Real(cost_isk), PyRep::Bool(false), PyRep::Bool(true)], &HashMap::new(), None).await.expect("InsureShip ship 2");
    assert_eq!(eve_server::services::combat_crimewatch::process_ship_insurance_payout(&ctx, char_id, ship_id_2, 587, true), 0.0);

    let ship_sd = 104_000_305i64;
    seed_ship_in_space(&ctx, char_id, ship_sd, 587, highsec_sys, DVec3::new(500.0, 0.0, 0.0)).await;
    let _ = ctx.dispatch_rpc(&mut sess, "insuranceSvc", None, "InsureShip", &[PyRep::Integer(ship_sd), PyRep::Real(cost_isk), PyRep::Bool(false), PyRep::Bool(true)], &HashMap::new(), None).await.expect("InsureShip self-destruct");
    let sd_res = eve_server::combat::death_pipeline::execute_death_pipeline_sync_inner(&ctx, highsec_sys, char_id, ship_sd, char_id, DVec3::ZERO, 7, true).expect("Self-destruct death");
    assert_eq!(sd_res.insurance_payout_isk, 0.0, "Self-destruct must void ship insurance payout");

    let ship_rep = 104_000_306i64;
    ctx.db.with_mut_snapshot(|s| {
        if let Some(ch) = s.characters.get_mut(&char_id) { ch.station_id = Some(jita_station); ch.active_ship_id = ship_id; }
        s.items.insert(ship_rep, InventoryItemRecord { item_id: ship_rep, type_id: 587, owner_id: char_id, location_id: jita_station, flag: InventoryFlag::Hangar, quantity: 1, singleton: true, version: 1, ..Default::default() });
    });
    let mut rep_sess = make_sess(char_id, ship_id, highsec_sys, Some(jita_station));
    let _ = ctx.dispatch_rpc(&mut rep_sess, "insuranceSvc", None, "InsureShip", &[PyRep::Integer(ship_rep), PyRep::Real(cost_isk), PyRep::Bool(false), PyRep::Bool(true)], &HashMap::new(), None).await.expect("InsureShip repackage");
    let _ = ctx.dispatch_rpc(&mut rep_sess, "repackagingSvc", None, "RepackageItems", &[PyRep::List(vec![PyRep::Integer(ship_rep)])], &HashMap::new(), None).await.expect("RepackageItems");
    assert!(!eve_server::services::combat_crimewatch::with_combat_runtime(&ctx, |rt| rt.insurance_contracts.contains_key(&ship_rep)), "RepackageItems must void active ship insurance contract");
}
