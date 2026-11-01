//! Integration tests for Pirate FOBs, Pochven Observatory Flashpoint & World Ark,
//! Abyssal Deadspace Weather Dogma Modifiers, and Deathless Shipcaster to FOB:
//! - `feat:pve.pirate-fob-forward-operating-base`
//! - `feat:pve.triglavian-world-ark-observatory`
//! - `feat:navigation.deathless-shipcaster-fob`

use crate::common::*;
use eve_db::{InventoryFlag, InventoryItemRecord, IskCents};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use eve_server::services::pve_abyssal_hazards::{
    apply_abyssal_weather_dogma_modifiers_to_ship, restore_abyssal_weather_dogma_modifiers,
};
use eve_server::services::pve_incursion_sites::make_site_npc_ball;
use eve_server::services::pve_special_sites_abyssal::AbyssalWeatherKind;
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

async fn call_svc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: Vec<PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, None, method, &args, &HashMap::new(), None)
        .await
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
        if !snap.characters.contains_key(&char_id) {
            let mut template = snap.characters.get(&90_001).or_else(|| snap.characters.values().next()).cloned().unwrap();
            template.character_id = char_id;
            template.name = format!("Pilot{char_id}");
            template.war_faction_id = None;
            template.loyalty_points.clear();
            snap.characters.insert(char_id, template);
        }
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
                dynamic_attributes: HashMap::new(),
                ..Default::default()
            },
        );
    });
    let sys = ctx.systems.get_or_spawn(sys_id);
    let _ = sys
        .spawn_ball_with_combat(
            make_site_npc_ball(ship_id, ship_type_id, pos),
            ctx.build_type_combat_state(ship_type_id),
        )
        .await;
}

#[tokio::test]
async fn test_pirate_fob_spawn_defense_scaling_and_loot_bounty() {
    // parity: feat:pve.pirate-fob-forward-operating-base
    eve_testkit::attest(&["feat:pve.pirate-fob-forward-operating-base"]);

    let ctx = ServerContext::new();
    pin_clock(&ctx);
    let sys_id = 30_002_537; // Amamake (low-sec 0.44 -> Blood Raiders Stronghold 46364)
    let char_id = 90_001;
    let ship_id = 88_500_001;

    // Negative: invalid faction and null-sec system refused
    assert_eq!(
        eve_server::services::pve_fob_observatory::spawn_pirate_fob(&ctx, sys_id, 500_001).await.unwrap_err().code,
        "InvalidPirateFaction"
    );
    assert_eq!(
        eve_server::services::pve_fob_observatory::spawn_pirate_fob(&ctx, 30_000_157, 500_012).await.unwrap_err().code,
        "InvalidSystemSecurity"
    );

    // 1. Seed pilots in 5 low/mid-sec systems and verify cluster tick enforces <= 3 active FOBs at dynamic coordinates
    let mut extra_sys: Vec<u32> = ctx.sde.solar_systems.values()
        .filter(|s| s.solar_system_id > sys_id && s.security_status > 0.1 && s.security_status < 0.7)
        .map(|s| s.solar_system_id)
        .collect();
    extra_sys.sort_unstable();
    seed_ship_in_space(&ctx, char_id, ship_id, 11_567, sys_id, DVec3::ZERO).await;
    for (i, &es) in extra_sys.iter().take(4).enumerate() {
        seed_ship_in_space(&ctx, 90_050 + (i as i64), 88_550_000 + (i as i64), 587, es, DVec3::ZERO).await;
    }

    tick_background_cluster(&ctx).await;

    let (fob_id, fob_pos, active_fob_count) = ctx.db.with_snapshot(|snap| {
        let total = snap.structures.values().filter(|s| matches!(s.type_id, 46_363 | 46_364)).count();
        let st = snap.structures.values().find(|s| s.solar_system_id == sys_id && s.type_id == 46_364).unwrap();
        (st.structure_id, DVec3::from_array(st.position), total)
    });
    assert_eq!(active_fob_count, 3, "Living-world cluster tick must cap active Pirate FOBs at 3");
    assert_ne!(fob_pos, DVec3::new(200_000.0, 0.0, -200_000.0));

    // Place attacking fleet on FOB grid (Titan=4, BS=3, non-combat Cruiser bystander=2 -> weight 9) + 1 off-grid pilot
    let ship2_id = 88_500_002;
    seed_ship_in_space(&ctx, char_id, ship_id, 11_567, sys_id, fob_pos + DVec3::new(0.0, 0.0, 3_500.0)).await;
    seed_ship_in_space(&ctx, 90_002, ship2_id, 638, sys_id, fob_pos + DVec3::new(0.0, 0.0, -3_500.0)).await;
    seed_ship_in_space(&ctx, 90_003, 88_500_003, 621, sys_id, fob_pos + DVec3::new(15_000.0, 0.0, 0.0)).await;
    seed_ship_in_space(&ctx, 90_004, 88_500_004, 587, sys_id, fob_pos + DVec3::new(2_000_000.0, 0.0, 0.0)).await;

    let sys = ctx.systems.get_or_spawn(sys_id);
    let init_balls = sys.list_balls().await.unwrap_or_default();
    assert_eq!(init_balls.iter().filter(|b| b.type_id == 24_000).count(), 3, "FOB must launch 3 Corpatis Bishop raiding cruisers");

    // 2. Fit Smartbombs (3995) on the 2 combat attackers (no superweapons in low-sec!) and attack FOB via live RPCs
    let sb1_id = 88_500_010;
    let sb2_id = 88_500_011;
    ctx.db.with_mut_snapshot(|snap| {
        for (mid, owner, loc) in [(sb1_id, char_id, ship_id), (sb2_id, 90_002, ship2_id)] {
            snap.items.insert(mid, InventoryItemRecord {
                item_id: mid, type_id: 3_995, owner_id: owner, location_id: loc,
                flag: InventoryFlag::HiSlot0, quantity: 1, singleton: true, ..Default::default()
            });
            snap.module_online_states.insert(mid, true);
        }
        for cid in [char_id, 90_002, 90_003, 90_004] {
            if let Some(ch) = snap.characters.get_mut(&cid) {
                ch.balance = IskCents::ZERO;
            }
        }
    });

    let mut sess1 = make_sess(char_id, ship_id, sys_id, None);
    call_svc(&ctx, &mut sess1, "dogmaIM", "Activate", vec![PyRep::Integer(sb1_id)])
        .await
        .expect("Smartbomb activation against FOB");

    let mid_balls = sys.list_balls().await.unwrap_or_default();
    assert_eq!(
        mid_balls.iter().filter(|b| b.type_id == 16_934).count(),
        9,
        "FOB defense response must scale to on-grid fleet composition (4+3+2=9 Corpus Prophets)"
    );

    // 3. Finish destroying FOB via second pilot's live Smartbomb -> drops Dark Blood loot (13791) & pays 30M ISK strictly to combat attackers
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(st) = snap.structures.get_mut(&fob_id) {
            st.shield_hp = 0.0;
            st.armor_hp = 0.0;
            st.hull_hp = 50.0;
        }
    });
    let mut sess2 = make_sess(90_002, ship2_id, sys_id, None);
    call_svc(&ctx, &mut sess2, "dogmaIM", "Activate", vec![PyRep::Integer(sb2_id)])
        .await
        .expect("Killing Smartbomb activation against FOB");

    let loot_cont_id = fob_id + 90;
    let (st_state, loot_type, b1, b2, bystander_bal, offgrid_bal) = ctx.db.with_snapshot(|snap| {
        let state = snap.structures.get(&fob_id).map(|s| s.state.clone()).unwrap_or_default();
        let loot = snap.items.values().find(|i| i.location_id == loot_cont_id).map(|i| i.type_id);
        let bal = |cid: i64| snap.characters.get(&cid).map(|c| c.balance.to_wire_f64()).unwrap_or(-1.0);
        (state, loot, bal(char_id), bal(90_002), bal(90_003), bal(90_004))
    });
    assert_eq!(st_state, "Destroyed");
    assert_eq!(loot_type, Some(13_791), "Blood Raider FOB must drop SDE Dark Blood Dual Heavy Pulse Laser (13791)");
    assert!((b1 - 30_000_000.0).abs() < 1.0 && (b2 - 30_000_000.0).abs() < 1.0, "Each on-grid combat attacker must receive 30M ISK base FOB bounty");
    assert_eq!(bystander_bal, 0.0, "Non-combatant on-grid bystander must receive 0 ISK");
    assert_eq!(offgrid_bal, 0.0, "Off-grid pilot must receive 0 ISK");
}

#[tokio::test]
async fn test_pochven_observatory_flashpoint_and_world_ark() {
    // parity: feat:pve.triglavian-world-ark-observatory
    eve_testkit::attest(&["feat:pve.triglavian-world-ark-observatory"]);

    let ctx = ServerContext::new();
    pin_clock(&ctx);
    let pochven_sys_id = 30_000_157; // Otela in Pochven (region 10000070)
    let highsec_sys_id = 30_000_142;
    let char_id = 90_001;
    let ship_id = 88_600_001;

    // 1. Negative: non-Pochven system refused
    assert_eq!(
        eve_server::services::pve_fob_observatory::spawn_observatory_flashpoint(&ctx, highsec_sys_id).await.unwrap_err().code,
        "NotInPochven"
    );

    // 2. Seed pilots in 4 Pochven systems and verify cluster tick enforces <= 3 active Observatory Flashpoints
    let mut poch_systems: Vec<u32> = ctx.sde.solar_systems.values()
        .filter(|s| s.region_id == 10_000_070 && s.solar_system_id > pochven_sys_id)
        .map(|s| s.solar_system_id)
        .collect();
    poch_systems.sort_unstable();
    seed_ship_in_space(&ctx, char_id, ship_id, 587, pochven_sys_id, DVec3::ZERO).await;
    for (i, &ps) in poch_systems.iter().take(3).enumerate() {
        seed_ship_in_space(&ctx, 90_060 + (i as i64), 88_660_000 + (i as i64), 587, ps, DVec3::ZERO).await;
    }
    tick_background_cluster(&ctx).await;

    let active_obs = ctx.db.with_snapshot(|s| s.service_state_blobs.keys().filter(|k| k.starts_with("observatory_site:")).count());
    assert_eq!(active_obs, 3, "Pochven region must cap active Observatory Flashpoints at 3");

    let obs = eve_server::services::pve_fob_observatory::get_observatory_site_state(&ctx, pochven_sys_id)
        .expect("Observatory state must be persisted in Otela");
    let site_id = obs.site_id;
    let gate_id = obs.gate_id;
    let world_ark_id = obs.world_ark_id;
    let gate_pos = DVec3::from_array(obs.gate_pos);
    let site_pos = DVec3::from_array(obs.site_pos);
    let sys = ctx.systems.get_or_spawn(pochven_sys_id);
    let balls = sys.list_balls().await.unwrap_or_default();
    assert!(balls.iter().any(|b| b.id == world_ark_id && b.type_id == 52_361));
    assert_eq!(balls.iter().filter(|b| matches!(b.type_id, 52_338 | 52_184 | 52_187)).count(), 3);

    // 3. Negative gate/pocket checks: direct warp blocked, docked blocked, >2,500m range blocked, capital blocked, <3 fleet & <0 standing blocked
    let mut sess = make_sess(char_id, ship_id, pochven_sys_id, None);
    let err_warp = call_svc(&ctx, &mut sess, "beyonce", "CmdWarpToStuff", vec![PyRep::String("item".into()), PyRep::Integer(world_ark_id)]).await.unwrap_err();
    assert_eq!(err_warp.code, "WarpBlockedByDeadspacePocket");

    ctx.db.with_mut_snapshot(|s| { if let Some(c) = s.characters.get_mut(&char_id) { c.station_id = Some(60_003_760); } });
    let mut docked_sess = make_sess(char_id, ship_id, pochven_sys_id, Some(60_003_760));
    assert_eq!(call_svc(&ctx, &mut docked_sess, "keeper", "ActivateAccelerationGate", vec![PyRep::Integer(gate_id)]).await.unwrap_err().code, "CantDoThatWhileDocked");
    ctx.db.with_mut_snapshot(|s| { if let Some(c) = s.characters.get_mut(&char_id) { c.station_id = None; } });

    seed_ship_in_space(&ctx, char_id, ship_id, 587, pochven_sys_id, gate_pos + DVec3::new(10_000.0, 0.0, 0.0)).await;
    assert_eq!(call_svc(&ctx, &mut sess, "keeper", "ActivateAccelerationGate", vec![PyRep::Integer(gate_id)]).await.unwrap_err().code, "OutsideOperationalRange");

    seed_ship_in_space(&ctx, char_id, ship_id, 11_567, pochven_sys_id, gate_pos + DVec3::new(1_000.0, 0.0, 0.0)).await;
    assert_eq!(call_svc(&ctx, &mut sess, "keeper", "ActivateAccelerationGate", vec![PyRep::Integer(gate_id)]).await.unwrap_err().code, "ShipClassRestricted");

    seed_ship_in_space(&ctx, char_id, ship_id, 587, pochven_sys_id, gate_pos + DVec3::new(1_000.0, 0.0, 0.0)).await;
    assert_eq!(call_svc(&ctx, &mut sess, "keeper", "ActivateAccelerationGate", vec![PyRep::Integer(gate_id)]).await.unwrap_err().code, "InsufficientFleetComposition");

    seed_ship_in_space(&ctx, 90_002, 88_600_002, 587, pochven_sys_id, gate_pos + DVec3::new(1_200.0, 0.0, 0.0)).await;
    seed_ship_in_space(&ctx, 90_003, 88_600_003, 587, pochven_sys_id, gate_pos + DVec3::new(1_400.0, 0.0, 0.0)).await;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.standings.insert(500_026, -2.5);
        }
        snap.fleets.insert(777_001, eve_db::FleetRecord {
            fleet_id: 777_001, boss_id: char_id, members: vec![char_id, 90_002, 90_003], ..Default::default()
        });
    });
    assert_eq!(call_svc(&ctx, &mut sess, "keeper", "ActivateAccelerationGate", vec![PyRep::Integer(gate_id)]).await.unwrap_err().code, "StandingAccessDenied");

    // 4. Seed 16 subcapital fleet ships within 2,500m of gate + 1 off-grid fleet ship, enter pocket via gate, and kill World Ark via live subcap weapon
    let mut fleet_ids = Vec::new();
    for idx in 0..16_i64 {
        let cid = 95_000 + idx;
        let sid = 88_610_000 + idx;
        let hull = if idx == 0 { 638 } else { 587 };
        fleet_ids.push(cid);
        seed_ship_in_space(&ctx, cid, sid, hull, pochven_sys_id, gate_pos + DVec3::new((idx as f64) * 50.0, 0.0, 500.0)).await;
    }
    let offgrid_cid = 95_099_i64;
    seed_ship_in_space(&ctx, offgrid_cid, 88_610_099, 587, pochven_sys_id, DVec3::new(5_000_000.0, 0.0, 0.0)).await;
    let mut all_members = vec![offgrid_cid];
    all_members.extend_from_slice(&fleet_ids);

    let sb_id = 88_620_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        for &cid in &all_members {
            if let Some(ch) = snap.characters.get_mut(&cid) {
                ch.standings.insert(500_026, 1.0);
                ch.balance = IskCents::ZERO;
                ch.loyalty_points.clear();
            }
        }
        snap.fleets.insert(777_001, eve_db::FleetRecord {
            fleet_id: 777_001, boss_id: fleet_ids[0], members: all_members.clone(), ..Default::default()
        });
        snap.items.insert(sb_id, InventoryItemRecord {
            item_id: sb_id, type_id: 3_995, owner_id: fleet_ids[0], location_id: 88_610_000,
            flag: InventoryFlag::HiSlot0, quantity: 1, singleton: true, ..Default::default()
        });
        snap.module_online_states.insert(sb_id, true);
    });

    for (idx, &cid) in fleet_ids.iter().enumerate() {
        let mut fsess = make_sess(cid, 88_610_000 + (idx as i64), pochven_sys_id, None);
        call_svc(&ctx, &mut fsess, "keeper", "ActivateAccelerationGate", vec![PyRep::Integer(gate_id)])
            .await
            .expect("Observatory gate must admit subcapital fleet member within 2,500m");
    }

    let _ = sys.set_ball_position(88_610_000, site_pos + DVec3::new(0.0, 0.0, 2_000.0)).await;
    let mut ark_cs = sys.get_combat_state(world_ark_id).await.unwrap().unwrap();
    ark_cs.shield_hp = 0.0;
    ark_cs.armor_hp = 0.0;
    ark_cs.structure_hp = 50.0;
    let _ = sys.set_combat_state(world_ark_id, ark_cs).await;

    let mut leader_sess = make_sess(fleet_ids[0], 88_610_000, pochven_sys_id, None);
    call_svc(&ctx, &mut leader_sess, "dogmaIM", "Activate", vec![PyRep::Integer(sb_id)])
        .await
        .expect("Live subcapital weapon kills World Ark boss");

    let (off_isk, p15_isk, p15_lp, p16_isk, site_exists) = ctx.db.with_snapshot(|snap| {
        let coff = snap.characters.get(&offgrid_cid).unwrap();
        let c15 = snap.characters.get(&fleet_ids[14]).unwrap();
        let c16 = snap.characters.get(&fleet_ids[15]).unwrap();
        (
            coff.balance.to_wire_f64(),
            c15.balance.to_wire_f64(),
            c15.loyalty_points.get(&1_000_137).copied().unwrap_or(0),
            c16.balance.to_wire_f64(),
            snap.service_state_blobs.contains_key(&format!("observatory_site:{site_id}")),
        )
    });
    assert_eq!(off_isk, 0.0, "Off-grid fleet member must not consume an Observatory payout slot");
    assert!((p15_isk - 100_000_000.0).abs() < 1.0);
    assert_eq!(p15_lp, 10_000);
    assert_eq!(p16_isk, 0.0, "16th pocket fleet member must receive 0 payout");
    assert!(!site_exists, "observatory_site:<site_id> must be deleted on completion");
}

#[tokio::test]
async fn test_abyssal_weather_dogma_modifiers_all_five_storms() {
    let ctx = ServerContext::new();
    pin_clock(&ctx);
    let sys_id = 32_000_001;
    let char_id = 90_001;
    let ship_id = 88_700_001;
    seed_ship_in_space(&ctx, char_id, ship_id, 587, sys_id, DVec3::ZERO).await;

    for weather in [
        AbyssalWeatherKind::Electrical,
        AbyssalWeatherKind::Dark,
        AbyssalWeatherKind::Exotic,
        AbyssalWeatherKind::Firestorm,
        AbyssalWeatherKind::Gamma,
    ] {
        let (orig_267, orig_269) = ctx.sde.types.get(&587).map(|t| (
            t.default_attributes.get(&267).copied().unwrap_or(0.5),
            t.default_attributes.get(&269).copied().unwrap_or(0.65),
        )).unwrap_or((0.5, 0.65));
        let saved = apply_abyssal_weather_dogma_modifiers_to_ship(&ctx, ship_id, weather, 50.0, 50.0);
        assert!(!saved.is_empty());
        ctx.db.with_snapshot(|snap| {
            let ship = snap.items.get(&ship_id).unwrap();
            match weather {
                AbyssalWeatherKind::Electrical => {
                    assert!(ship.dynamic_attributes[&271] > 0.5);
                    assert!(ship.dynamic_attributes[&267] > orig_267);
                    assert!(ship.dynamic_attributes[&55] < 250_000.0);
                }
                AbyssalWeatherKind::Dark => {
                    assert!(ship.dynamic_attributes[&37] > 250.0);
                    assert!((ship.dynamic_attributes[&54] - 7_500.0).abs() < 1e-6);
                }
                AbyssalWeatherKind::Exotic => {
                    assert!(ship.dynamic_attributes[&273] > 0.5);
                    assert!(ship.dynamic_attributes[&269] > orig_269);
                    assert!(ship.dynamic_attributes[&564] > 250.0);
                }
                AbyssalWeatherKind::Firestorm => {
                    assert!(ship.dynamic_attributes[&265] > 500.0);
                    assert!(ship.dynamic_attributes[&274] > 0.5);
                }
                AbyssalWeatherKind::Gamma => {
                    assert!(ship.dynamic_attributes[&263] > 500.0);
                    assert!(ship.dynamic_attributes[&272] > 0.5);
                }
            }
        });
        restore_abyssal_weather_dogma_modifiers(&ctx, ship_id, &saved);
    }

    // Enter an Abyssal pocket (Electrical T1 filament 47765) and verify V24 SDE Electrical weather modifiers on NPC
    let mut sess = make_sess(char_id, ship_id, 30_000_142, None);
    let (_pid, r1_npcs) = eve_server::services::pve_special_sites_abyssal::enter_abyssal_pocket_with_trace(
        &ctx,
        &mut sess,
        char_id,
        47_765,
        1,
        30_000_142,
        None,
    )
    .await;
    assert!(!r1_npcs.is_empty());
    let abyss_sys = ctx.systems.get_or_spawn(32_000_001);
    let (_, npc_ball) = abyss_sys.get_ball_snapshot(r1_npcs[0]).await.unwrap().unwrap();
    let base_npc_cs = ctx.build_type_combat_state(npc_ball.type_id);
    let npc_cs = abyss_sys.get_combat_state(r1_npcs[0]).await.unwrap().unwrap();
    assert!(
        npc_cs.armor_resonance[0] >= base_npc_cs.armor_resonance[0]
            && npc_cs.capacitor_recharge_time_ms < base_npc_cs.capacitor_recharge_time_ms,
        "Abyssal NPCs must receive V24 SDE Electrical EM resonance penalty and faster capacitor recharge"
    );
}

#[tokio::test]
async fn test_deathless_shipcaster_zarzakh_to_fob() {
    // parity: feat:navigation.deathless-shipcaster-fob
    eve_testkit::attest(&["feat:navigation.deathless-shipcaster-fob"]);

    let ctx = ServerContext::new();
    pin_clock(&ctx);
    let zarzakh_id = 30_100_000;
    let campaign_fob_sys = 30_002_813; // Tama
    let connected_sys_id = ctx
        .sde
        .jumps
        .values()
        .find(|j| j.to_solar_system_id == campaign_fob_sys)
        .map(|j| j.from_solar_system_id)
        .expect("Tama must have an SDE stargate neighbor");
    let unconnected_sys_id = 30_000_142; // Jita (not 1-hop connected to Tama)
    let char_id = 90_001;
    let ship_id = 88_400_001;
    let launcher_id = 88_400_010;
    let guristas_pad_id = 88_400_011;
    let unconnected_pad_id = 88_400_012;

    seed_ship_in_space(&ctx, char_id, ship_id, 587, zarzakh_id, DVec3::new(10_000.0, 0.0, 10_000.0)).await;
    let sys = ctx.systems.get_or_spawn(zarzakh_id);
    let mut l_ball = make_site_npc_ball(launcher_id, 77_262, DVec3::new(10_500.0, 0.0, 10_000.0));
    l_ball.mode = eve_destiny::MovementMode::Rigid;
    let _ = sys.spawn_ball(l_ball).await;

    let now_u = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        snap.insurgency_runtime.campaigns.insert(
            77,
            eve_db::PersistedInsurgencyCampaign {
                campaign_id: 77,
                pirate_faction_id: 500_010,
                target_faction_id: 500_001,
                fob_solar_system_id: campaign_fob_sys,
                systems: vec![campaign_fob_sys],
                created_at_unix: now_u,
                forecasting_end_unix: now_u,
                active_end_unix: now_u + 100_000,
                winddown_end_unix: now_u + 200_000,
                corruption_points: HashMap::new(),
                suppression_points: HashMap::new(),
            },
        );
    });

    eve_server::services::cluster_macho_misc_services::with_misc_state(&ctx, |st| {
        st.launchers.push(eve_server::services::cluster_macho_misc_services::LauncherRecord {
            item_id: launcher_id,
            type_id: 77_262,
            solar_system_id: zarzakh_id,
            faction_id: 0,
            position: DVec3::new(10_500.0, 0.0, 10_000.0),
            linked_pad_ids: vec![guristas_pad_id, unconnected_pad_id],
        });
        for (pid, psys) in [(guristas_pad_id, connected_sys_id), (unconnected_pad_id, unconnected_sys_id)] {
            st.landing_pads.push(eve_server::services::cluster_macho_misc_services::LandingPadRecord {
                item_id: pid,
                type_id: 77_263,
                solar_system_id: psys,
                faction_id: 500_010, // Guristas
                position: DVec3::new(50_000.0, 0.0, 50_000.0),
                is_built: true,
                is_disrupted: false,
                link_timestamp: 1,
            });
        }
    });

    let mut sess = make_sess(char_id, ship_id, zarzakh_id, None);

    // 1. Negative: non-pirate character refused
    let err_non_pirate = call_svc(
        &ctx,
        &mut sess,
        "shipcasterTravelMgr",
        "CmdJumpThroughShipcaster",
        vec![PyRep::Integer(launcher_id), PyRep::Integer(i64::from(connected_sys_id)), PyRep::Integer(guristas_pad_id)],
    )
    .await
    .unwrap_err();
    assert_eq!(err_non_pirate.code, "NonPirateRefused");

    // 2. Negative: Angel Cartel (500011) refused on Guristas (500010) FOB pad
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.war_faction_id = Some(500_011);
        }
    });
    let err_mismatch = call_svc(
        &ctx,
        &mut sess,
        "shipcasterTravelMgr",
        "CmdJumpThroughShipcaster",
        vec![PyRep::Integer(launcher_id), PyRep::Integer(i64::from(connected_sys_id)), PyRep::Integer(guristas_pad_id)],
    )
    .await
    .unwrap_err();
    assert_eq!(err_mismatch.code, "FobFactionMismatch");

    // 3. Negative: unconnected destination system rejected via SDE stargate topology (`InsurgencyWarzoneNotConnected`)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.war_faction_id = Some(500_010);
        }
    });
    let err_unconn = call_svc(
        &ctx,
        &mut sess,
        "shipcasterTravelMgr",
        "CmdJumpThroughShipcaster",
        vec![PyRep::Integer(launcher_id), PyRep::Integer(i64::from(unconnected_sys_id)), PyRep::Integer(unconnected_pad_id)],
    )
    .await
    .unwrap_err();
    assert_eq!(err_unconn.code, "InsurgencyWarzoneNotConnected");

    // 4. Positive: Guristas (500010) jumps to SDE stargate-connected insurgency warzone system
    call_svc(
        &ctx,
        &mut sess,
        "shipcasterTravelMgr",
        "CmdJumpThroughShipcaster",
        vec![PyRep::Integer(launcher_id), PyRep::Integer(i64::from(connected_sys_id)), PyRep::Integer(guristas_pad_id)],
    )
    .await
    .unwrap();
    assert_eq!(sess.solar_system_id, Some(connected_sys_id));

    // 5. Negative: persisted per-target cooldown blocks immediate repeat jump
    seed_ship_in_space(&ctx, char_id, ship_id, 587, zarzakh_id, DVec3::new(10_000.0, 0.0, 10_000.0)).await;
    sess.solar_system_id = Some(zarzakh_id);
    let err_cd = call_svc(
        &ctx,
        &mut sess,
        "shipcasterTravelMgr",
        "CmdJumpThroughShipcaster",
        vec![PyRep::Integer(launcher_id), PyRep::Integer(i64::from(connected_sys_id)), PyRep::Integer(guristas_pad_id)],
    )
    .await
    .unwrap_err();
    assert_eq!(err_cd.code, "ShipcasterTargetOnCooldown");
}
