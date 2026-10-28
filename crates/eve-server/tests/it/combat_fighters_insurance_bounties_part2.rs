#![allow(unused_imports)]
//! Integration tests for Carrier/Supercarrier Fighter Squadrons (`C-6`), Ship Insurance (`C-3`),
//! Player Bounties & Leaderboards, GM Bounty Tools, and Null-Sec ESS Main/Reserve Banks & Dynamic Bounty System.

use eve_db::{InventoryFlag, InventoryItemRecord, IskCents, SystemDynamicState};
use eve_destiny::{Ball, CombatState, DamageVector, MovementMode, TargetLock};
use eve_machonet::{service_roles, SessionState};
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::combat_crimewatch::{
    apply_damage_to_fighter_squadron, compute_ship_insurance_quote_levels, dispatch_combat_rpc, step_dynamic_bounty_multiplier,
};
use glam::DVec3;
use std::collections::HashMap;

pub(crate) const ARCHON_CARRIER_TYPE_ID: u32 = 23757; // Archon Carrier
pub(crate) const DRAGONFLY_FIGHTER_TYPE_ID: u32 = 23057; // Dragonfly I Light Fighter (max squadron size 9)
pub(crate) const RIFTER_TYPE_ID: u32 = 587; // Rifter Frigate
pub(crate) const AUTOCANNON_TYPE_ID: u32 = 2889; // 200mm AutoCannon I
pub(crate) const EMP_S_TYPE_ID: u32 = 185; // EMP S
pub(crate) const GURISTAS_PIRATE_TYPE_ID: u32 = 1001; // Guristas Pirate NPC
pub(crate) const ESS_BEACON_TYPE_ID: u32 = 32882; // Encounter Surveillance System
pub(crate) const ESS_RESERVE_KEY_TYPE_ID: u32 = 57720; // ESS-SW-15 Reserve Security Key (group 4102, Delve)

pub(crate) fn build_ctx() -> ServerContext {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    ctx
}

pub(crate) fn make_session(session_id: u64, user_id: u64, char_id: i64, sys_id: u32, ship_id: i64, station_id: Option<i64>) -> SessionState {
    let mut s = SessionState::new(session_id);
    s.user_id = Some(user_id);
    s.character_id = Some(char_id);
    s.corporation_id = Some(1_000_009);
    s.solar_system_id = Some(sys_id);
    s.constellation_id = Some(20_000_020);
    s.region_id = Some(10_000_002);
    s.ship_id = Some(ship_id);
    s.station_id = station_id;
    s.role = service_roles::ROLE_PLAYER | service_roles::ROLE_LOGIN;
    s
}

pub(crate) fn kv_map(rep: &PyRep) -> HashMap<String, PyRep> {
    match rep {
        PyRep::KeyVal(entries) => entries.clone(),
        PyRep::Dict(entries) => entries
            .iter()
            .filter_map(|(k, v)| match k {
                PyRep::String(s) | PyRep::Token(s) => Some((s.clone(), v.clone())),
                _ => None,
            })
            .collect(),
        _ => panic!("Expected KeyVal/Dict, got {rep:?}"),
    }
}

#[tokio::test]
async fn carrier_fighter_tubes_launch_movement_abilities_losses_and_abandon() {
    // parity: feat:drones.fighter-squadron-losses
    eve_testkit::attest(&["feat:drones.fighter-squadron-losses"]);

    let ctx = build_ctx();
    crate::common::pin_clock(&ctx);
    let sys_id: u32 = 30_000_142;
    let char_id: i64 = 90_000_001;
    let carrier_id: i64 = 910_001;
    let squadron_item_id: i64 = 910_010;
    let spare_fighters_item_id: i64 = 910_011;
    let npc_ball_id: i64 = 910_050;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.solar_system_id = sys_id;
            ch.active_ship_id = carrier_id;
            ch.balance = IskCents::from_isk(500_000_000);
        }
        snap.items.insert(
            carrier_id,
            InventoryItemRecord {
                item_id: carrier_id,
                type_id: ARCHON_CARRIER_TYPE_ID,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        // Primary squadron of 9 Dragonfly I fighters in FighterBay
        snap.items.insert(
            squadron_item_id,
            InventoryItemRecord {
                item_id: squadron_item_id,
                type_id: DRAGONFLY_FIGHTER_TYPE_ID,
                owner_id: char_id,
                location_id: carrier_id,
                flag: InventoryFlag::FighterBay,
                quantity: 9,
                singleton: false,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        // Reserve stack of 5 Dragonfly I fighters in FighterBay for replacement refill upon recall
        snap.items.insert(
            spare_fighters_item_id,
            InventoryItemRecord {
                item_id: spare_fighters_item_id,
                type_id: DRAGONFLY_FIGHTER_TYPE_ID,
                owner_id: char_id,
                location_id: carrier_id,
                flag: InventoryFlag::FighterBay,
                quantity: 5,
                singleton: false,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: carrier_id,
                type_id: ARCHON_CARRIER_TYPE_ID,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 85.0,
                inertia: 1.0,
                mass: 1_200_000_000.0,
                radius: 1_500.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_type_combat_state(ARCHON_CARRIER_TYPE_ID),
        )
        .await
        .unwrap();
    sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: npc_ball_id,
                type_id: GURISTAS_PIRATE_TYPE_ID,
                position: DVec3::new(3_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 250.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 45.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState {
                shield_hp: 5_000.0,
                max_shield_hp: 5_000.0,
                armor_hp: 5_000.0,
                max_armor_hp: 5_000.0,
                structure_hp: 5_000.0,
                max_structure_hp: 5_000.0,
                ..CombatState::default()
            },
        )
        .await
        .unwrap();

    let mut s1 = make_session(1, 1, char_id, sys_id, carrier_id, None);
    let kw = HashMap::new();

    // 1. Load fighter squadron into Tube 0 (`feat:drones.fighter-load-tubes`), unload back to FighterBay, then reload
    let load_res = ctx
        .dispatch_rpc(&mut s1, "fighterMgr", None, "LoadFightersToTube", &[PyRep::Integer(0), PyRep::Integer(squadron_item_id)], &kw, None)
        .await
        .expect("LoadFightersToTube");
    let load_kv = kv_map(&load_res);
    assert_eq!(load_kv.get("tubeIndex"), Some(&PyRep::Integer(0)));
    assert_eq!(load_kv.get("fighterID"), Some(&PyRep::Integer(squadron_item_id)));

    let unload_res =
        ctx.dispatch_rpc(&mut s1, "fighterMgr", None, "UnloadTubeToFighterBay", &[PyRep::Integer(0)], &kw, None).await.expect("UnloadTubeToFighterBay");
    assert_eq!(unload_res, PyRep::Bool(true));

    ctx.dispatch_rpc(&mut s1, "fighterMgr", None, "LoadFightersToTube", &[PyRep::Integer(0), PyRep::Integer(squadron_item_id)], &kw, None)
        .await
        .expect("Reload fighters into Tube 0");

    // 2. Launch fighters from Tube 0 into space (`feat:drones.fighter-launch-recall`)
    let launch_res = ctx
        .dispatch_rpc(&mut s1, "fighterMgr", None, "LaunchFightersFromTubes", &[PyRep::List(vec![PyRep::Integer(0)])], &kw, None)
        .await
        .expect("LaunchFightersFromTubes");
    let PyRep::List(launched_list) = launch_res else {
        panic!("Expected list from LaunchFightersFromTubes");
    };
    assert_eq!(launched_list.len(), 1);
    let fighter_ball_id = launched_list[0].as_i64().unwrap();
    assert!(sys_handle.get_ball_snapshot(fighter_ball_id).await.unwrap().is_some());

    // 3. Command fighter movement in space (`feat:drones.fighter-movement`)
    let move_res = ctx
        .dispatch_rpc(
            &mut s1,
            "fighterMgr",
            None,
            "ExecuteMovementCommandOnFighters",
            &[PyRep::List(vec![PyRep::Integer(fighter_ball_id)]), PyRep::String("ORBIT".into()), PyRep::Integer(npc_ball_id), PyRep::Real(2_500.0)],
            &kw,
            None,
        )
        .await
        .expect("ExecuteMovementCommandOnFighters");
    assert_eq!(move_res, PyRep::Bool(true));

    // Lock target on the carrier so targeted fighter abilities can engage (`feat:drones.fighter-abilities`)
    sys_handle
        .add_target_lock(
            carrier_id,
            TargetLock { target_id: npc_ball_id, locked: true, lock_time_ms: 1_000.0, remaining_lock_ms: 0.0, max_range_m: Some(100_000.0) },
        )
        .await
        .unwrap();

    // 4. Activate Fighter Ability Slots 0 (Attack) and 2 (Heavy Rocket Salvo) (`feat:drones.fighter-abilities`)
    let ab0 = kv_map(
        &ctx.dispatch_rpc(
            &mut s1,
            "fighterMgr",
            None,
            "CmdActivateAbilitySlots",
            &[PyRep::List(vec![PyRep::Integer(fighter_ball_id)]), PyRep::Integer(0), PyRep::Integer(npc_ball_id)],
            &kw,
            None,
        )
        .await
        .expect("CmdActivateAbilitySlots slot 0"),
    );
    assert_eq!(ab0.get("success"), Some(&PyRep::Bool(true)));

    let ab2 = kv_map(
        &ctx.dispatch_rpc(
            &mut s1,
            "fighterMgr",
            None,
            "CmdActivateAbilitySlots",
            &[PyRep::List(vec![PyRep::Integer(fighter_ball_id)]), PyRep::Integer(2), PyRep::Integer(npc_ball_id)],
            &kw,
            None,
        )
        .await
        .expect("CmdActivateAbilitySlots slot 2"),
    );
    assert_eq!(ab2.get("success"), Some(&PyRep::Bool(true)));
    let charge_count = ab2.get("chargeCount").and_then(|v| v.as_i64()).unwrap_or(0);
    let rem_charges = ab2.get("remainingCharges").and_then(|v| v.as_i64()).unwrap_or(0);
    assert_eq!(rem_charges, charge_count - 1);

    // 5. Apply damage to fighter squadron and verify per-unit losses (`feat:drones.fighter-squadron-losses`)
    let (units_lost, remaining_size, sq_destroyed) =
        apply_damage_to_fighter_squadron(&ctx, sys_id, fighter_ball_id, DamageVector { em: 5_000.0, thermal: 0.0, kinetic: 0.0, explosive: 0.0 }).await;
    assert_eq!(units_lost, 1, "Fighter squadron must lose 1 unit when damaged past unit_hp");
    assert_eq!(remaining_size, 6 - units_lost);
    assert!(!sq_destroyed);

    // 6. Recall fighters to tube (`feat:drones.fighter-launch-recall`, `feat:drones.fighter-squadron-losses`)
    // Verify squadron refills lost units from FighterBay reserve back to max squadron size (6) and rearms Slot 2 charges
    let recall_res = ctx
        .dispatch_rpc(&mut s1, "fighterMgr", None, "RecallFightersToTubes", &[PyRep::List(vec![PyRep::Integer(fighter_ball_id)])], &kw, None)
        .await
        .expect("RecallFightersToTubes");
    let PyRep::List(recalled_list) = recall_res else {
        panic!("Expected list from RecallFightersToTubes");
    };
    assert_eq!(recalled_list.len(), 1);
    let sq_qty_after_recall = ctx.db.with_snapshot(|snap| snap.items.get(&squadron_item_id).unwrap().quantity);
    assert_eq!(sq_qty_after_recall, 6, "Recalled squadron must refill lost units from FighterBay reserve back to max squadron size");
    assert!(ctx.fighter_charge_state.read().unwrap().is_empty(), "Recalled squadron must rearm ability charges");

    // 7. Re-launch and abandon in space (`feat:drones.fighter-abandon`)
    let relaunch_early = ctx.dispatch_rpc(&mut s1, "fighterMgr", None, "LaunchFightersFromTubes", &[PyRep::List(vec![PyRep::Integer(0)])], &kw, None).await;
    assert_eq!(relaunch_early.unwrap_err().code, "TubeRearming");
    ctx.advance_virtual_clock(16);

    let relaunch_res = ctx
        .dispatch_rpc(&mut s1, "fighterMgr", None, "LaunchFightersFromTubes", &[PyRep::List(vec![PyRep::Integer(0)])], &kw, None)
        .await
        .expect("Re-launch fighters");
    let PyRep::List(relaunched) = relaunch_res else {
        panic!("Expected list");
    };
    let relaunched_ball_id = relaunched[0].as_i64().unwrap();

    let PyRep::List(abandoned_list) =
        ctx.dispatch_rpc(&mut s1, "fighterMgr", None, "CmdAbandonFighter", &[PyRep::Integer(relaunched_ball_id)], &kw, None).await.expect("CmdAbandonFighter")
    else {
        panic!("Expected list from CmdAbandonFighter");
    };
    assert_eq!(abandoned_list, vec![PyRep::Integer(relaunched_ball_id)]);
    assert!(ctx.active_fighters.read().unwrap().get(&relaunched_ball_id).unwrap().is_abandoned);
}
