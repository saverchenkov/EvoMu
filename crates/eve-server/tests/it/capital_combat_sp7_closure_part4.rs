//! Integration tests for Pirate FOBs, Pochven Observatory Flashpoint & World Ark,
//! and Abyssal Deadspace Weather Dogma Modifiers:
//! - `feat:pve.pirate-fob-forward-operating-base`
//! - `feat:pve.triglavian-world-ark-observatory`
//! - `feat:abyssal.weather-effects-and-penalties`

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

fn py_keyval_to_map(rep: &PyRep) -> HashMap<String, PyRep> {
    let mut out = HashMap::new();
    if let PyRep::KeyVal(entries) = rep {
        for (k, v) in entries {
            out.insert(k.clone(), v.clone());
        }
    }
    out
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
            let mut template = snap.characters.values().next().cloned().unwrap();
            template.character_id = char_id;
            template.name = format!("Pilot{char_id}");
            template.war_faction_id = None;
            snap.characters.insert(char_id, template);
        }
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.active_ship_id = ship_id;
            ch.station_id = None;
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
    let sys_id = 30_000_142; // Jita (high-sec)
    let char_id = 90_001;
    let ship_id = 88_500_001;
    seed_ship_in_space(&ctx, char_id, ship_id, 587, sys_id, DVec3::ZERO).await;
    let mut sess = make_sess(char_id, ship_id, sys_id, None);
    sess.role = u64::MAX;

    // 1. Spawn Blood Raider (500012) FOB in high-sec
    let spawn_res = call_svc(
        &ctx,
        &mut sess,
        "slash",
        "SlashCmd",
        vec![PyRep::String(format!("/fob spawn {sys_id} 500012"))],
    )
    .await
    .unwrap();
    let spawn_kv = py_keyval_to_map(&spawn_res);
    let fob_id = spawn_kv.get("fobID").and_then(|v| v.as_i64()).unwrap();
    let raiders: Vec<i64> = match spawn_kv.get("raidingFleetIDs") {
        Some(PyRep::List(l)) => l.iter().filter_map(|x| x.as_i64()).collect(),
        _ => Vec::new(),
    };
    assert_eq!(raiders.len(), 3);

    // 2. Trigger defense response scaled to 3 attacking ships -> 6 defenders
    let def_res = call_svc(
        &ctx,
        &mut sess,
        "slash",
        "SlashCmd",
        vec![PyRep::String(format!("/fob defense {fob_id} 1,2,3"))],
    )
    .await
    .unwrap();
    let def_kv = py_keyval_to_map(&def_res);
    assert_eq!(def_kv.get("defenderCount").and_then(|v| v.as_i64()), Some(6));

    // 3. Destroy FOB -> drops structure loot container & pays 300M ISK bounty
    let bal_before = ctx.db.with_snapshot(|snap| {
        snap.characters.get(&char_id).map(|c| c.balance.to_wire_f64()).unwrap_or(0.0)
    });
    let dest_res = call_svc(
        &ctx,
        &mut sess,
        "slash",
        "SlashCmd",
        vec![PyRep::String(format!("/fob destroy {fob_id} {char_id}"))],
    )
    .await
    .unwrap();
    let dest_kv = py_keyval_to_map(&dest_res);
    let loot_cont_id = dest_kv.get("lootContainerID").and_then(|v| v.as_i64()).unwrap();

    let (st_state, has_loot, bal_after) = ctx.db.with_snapshot(|snap| {
        let state = snap.structures.get(&fob_id).map(|s| s.state.clone()).unwrap_or_default();
        let loot = snap.items.values().any(|i| i.location_id == loot_cont_id);
        let bal = snap.characters.get(&char_id).map(|c| c.balance.to_wire_f64()).unwrap_or(0.0);
        (state, loot, bal)
    });
    assert_eq!(st_state, "Destroyed");
    assert!(has_loot);
    assert!((bal_after - bal_before - 300_000_000.0).abs() < 1.0);
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
    seed_ship_in_space(&ctx, char_id, ship_id, 587, pochven_sys_id, DVec3::ZERO).await;
    let mut sess = make_sess(char_id, ship_id, pochven_sys_id, None);
    sess.role = u64::MAX;

    // 1. Negative: non-Pochven system refused
    let err_non_poch = call_svc(
        &ctx,
        &mut sess,
        "slash",
        "SlashCmd",
        vec![PyRep::String(format!("/observatory spawn {highsec_sys_id}"))],
    )
    .await
    .unwrap_err();
    assert_eq!(err_non_poch.code, "NotInPochven");

    // 2. Spawn Observatory Flashpoint in Pochven
    let spawn_res = call_svc(
        &ctx,
        &mut sess,
        "slash",
        "SlashCmd",
        vec![PyRep::String(format!("/observatory spawn {pochven_sys_id}"))],
    )
    .await
    .unwrap();
    let spawn_kv = py_keyval_to_map(&spawn_res);
    let site_id = spawn_kv.get("siteID").and_then(|v| v.as_i64()).unwrap();

    // 3. Negative: under-sized fleet (< 3 pilots) refused
    let err_fleet = call_svc(
        &ctx,
        &mut sess,
        "slash",
        "SlashCmd",
        vec![PyRep::String(format!("/observatory enter {site_id} {char_id}"))],
    )
    .await
    .unwrap_err();
    assert_eq!(err_fleet.code, "InsufficientFleetComposition");

    // 4. Negative: hostile Triglavian standing (< 0.0) refused
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.standings.insert(500_026, -2.5);
        }
    });
    let err_stand = call_svc(
        &ctx,
        &mut sess,
        "slash",
        "SlashCmd",
        vec![PyRep::String(format!("/observatory enter {site_id} {char_id},90002,90003"))],
    )
    .await
    .unwrap_err();
    assert_eq!(err_stand.code, "StandingAccessDenied");

    // 5. Seed 18 fleet members and verify payout caps at 15 pilots
    let mut fleet_ids = Vec::new();
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.standings.insert(500_026, 1.0);
        }
        let template = snap.characters.get(&char_id).cloned().unwrap();
        for idx in 0..18_i64 {
            let cid = 95_000 + idx;
            fleet_ids.push(cid);
            snap.characters.entry(cid).or_insert_with(|| {
                let mut c = template.clone();
                c.character_id = cid;
                c.name = format!("PochvenPilot{idx}");
                c.balance = IskCents::ZERO;
                c.loyalty_points.clear();
                c
            });
        }
    });

    let fleet_csv = fleet_ids.iter().map(|id| id.to_string()).collect::<Vec<_>>().join(",");
    let comp_res = call_svc(
        &ctx,
        &mut sess,
        "slash",
        "SlashCmd",
        vec![PyRep::String(format!("/observatory complete {site_id} {fleet_csv}"))],
    )
    .await
    .unwrap();
    let comp_kv = py_keyval_to_map(&comp_res);
    assert_eq!(comp_kv.get("paidPilotCount").and_then(|v| v.as_i64()), Some(15));

    // Verify 15th pilot paid, 16th pilot unpaid
    let (p15_isk, p15_lp, p16_isk) = ctx.db.with_snapshot(|snap| {
        let c15 = snap.characters.get(&fleet_ids[14]).unwrap();
        let c16 = snap.characters.get(&fleet_ids[15]).unwrap();
        (
            c15.balance.to_wire_f64(),
            c15.loyalty_points.get(&1_000_137).copied().unwrap_or(0),
            c16.balance.to_wire_f64(),
        )
    });
    assert!((p15_isk - 100_000_000.0).abs() < 1.0);
    assert_eq!(p15_lp, 10_000);
    assert_eq!(p16_isk, 0.0);
}

#[tokio::test]
async fn test_abyssal_weather_dogma_modifiers_all_five_storms() {
    // parity: feat:abyssal.weather-effects-and-penalties
    eve_testkit::attest(&["feat:abyssal.weather-effects-and-penalties"]);

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
        let saved = apply_abyssal_weather_dogma_modifiers_to_ship(&ctx, ship_id, weather, 50.0, 50.0);
        assert!(!saved.is_empty());
        ctx.db.with_snapshot(|snap| {
            let ship = snap.items.get(&ship_id).unwrap();
            match weather {
                AbyssalWeatherKind::Electrical => {
                    assert!(ship.dynamic_attributes.contains_key(&271)); // shieldEmDamageResonance
                    assert!(ship.dynamic_attributes.contains_key(&55)); // rechargeRate
                }
                AbyssalWeatherKind::Dark => {
                    assert!(ship.dynamic_attributes.contains_key(&37)); // maxVelocity
                    assert!(ship.dynamic_attributes.contains_key(&54)); // maxRange
                }
                AbyssalWeatherKind::Exotic => {
                    assert!(ship.dynamic_attributes.contains_key(&273)); // shieldKineticDamageResonance
                    assert!(ship.dynamic_attributes.contains_key(&564)); // scanResolution
                }
                AbyssalWeatherKind::Firestorm => {
                    assert!(ship.dynamic_attributes.contains_key(&265)); // armorHP
                    assert!(ship.dynamic_attributes.contains_key(&274)); // shieldThermalDamageResonance
                }
                AbyssalWeatherKind::Gamma => {
                    assert!(ship.dynamic_attributes.contains_key(&263)); // shieldCapacity
                    assert!(ship.dynamic_attributes.contains_key(&272)); // shieldExplosiveDamageResonance
                }
            }
        });
        restore_abyssal_weather_dogma_modifiers(&ctx, ship_id, &saved);
    }
}
