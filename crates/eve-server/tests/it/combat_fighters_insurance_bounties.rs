//! Integration tests for Carrier/Supercarrier Fighter Squadrons (`C-6`), Ship Insurance (`C-3`),
//! Player Bounties & Leaderboards, GM Bounty Tools, and Null-Sec ESS Main/Reserve Banks & Dynamic Bounty System.

use eve_db::{
    InventoryFlag, InventoryItemRecord, IskCents, SystemDynamicState,
};
use eve_destiny::{Ball, CombatState, DamageVector, MovementMode, TargetLock};
use eve_machonet::{service_roles, SessionState};
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::combat_crimewatch::{
    apply_damage_to_fighter_squadron, compute_ship_insurance_quote_levels, dispatch_combat_rpc,
    step_dynamic_bounty_multiplier,
};
use glam::DVec3;
use std::collections::HashMap;

const ARCHON_CARRIER_TYPE_ID: u32 = 23757; // Archon Carrier
const DRAGONFLY_FIGHTER_TYPE_ID: u32 = 23057; // Dragonfly I Light Fighter (max squadron size 9)
const RIFTER_TYPE_ID: u32 = 587; // Rifter Frigate
const AUTOCANNON_TYPE_ID: u32 = 2889; // 200mm AutoCannon I
const EMP_S_TYPE_ID: u32 = 185; // EMP S
const GURISTAS_PIRATE_TYPE_ID: u32 = 1001; // Guristas Pirate NPC
const ESS_BEACON_TYPE_ID: u32 = 32882; // Encounter Surveillance System
const ESS_RESERVE_KEY_TYPE_ID: u32 = 57714; // ESS-NW-15 Reserve Security Key (group 4102)

fn build_ctx() -> ServerContext {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    ctx
}

fn make_session(
    session_id: u64,
    user_id: u64,
    char_id: i64,
    sys_id: u32,
    ship_id: i64,
    station_id: Option<i64>,
) -> SessionState {
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

fn kv_map(rep: &PyRep) -> HashMap<String, PyRep> {
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
        .dispatch_rpc(
            &mut s1,
            "fighterMgr",
            None,
            "LoadFightersToTube",
            &[PyRep::Integer(0), PyRep::Integer(squadron_item_id)],
            &kw,
            None,
        )
        .await
        .expect("LoadFightersToTube");
    let load_kv = kv_map(&load_res);
    assert_eq!(load_kv.get("tubeIndex"), Some(&PyRep::Integer(0)));
    assert_eq!(
        load_kv.get("fighterID"),
        Some(&PyRep::Integer(squadron_item_id))
    );

    let unload_res = ctx
        .dispatch_rpc(
            &mut s1,
            "fighterMgr",
            None,
            "UnloadTubeToFighterBay",
            &[PyRep::Integer(0)],
            &kw,
            None,
        )
        .await
        .expect("UnloadTubeToFighterBay");
    assert_eq!(unload_res, PyRep::Bool(true));

    ctx.dispatch_rpc(
        &mut s1,
        "fighterMgr",
        None,
        "LoadFightersToTube",
        &[PyRep::Integer(0), PyRep::Integer(squadron_item_id)],
        &kw,
        None,
    )
    .await
    .expect("Reload fighters into Tube 0");

    // 2. Launch fighters from Tube 0 into space (`feat:drones.fighter-launch-recall`)
    let launch_res = ctx
        .dispatch_rpc(
            &mut s1,
            "fighterMgr",
            None,
            "LaunchFightersFromTubes",
            &[PyRep::List(vec![PyRep::Integer(0)])],
            &kw,
            None,
        )
        .await
        .expect("LaunchFightersFromTubes");
    let PyRep::List(launched_list) = launch_res else {
        panic!("Expected list from LaunchFightersFromTubes");
    };
    assert_eq!(launched_list.len(), 1);
    let fighter_ball_id = launched_list[0].as_i64().unwrap();
    assert!(sys_handle
        .get_ball_snapshot(fighter_ball_id)
        .await
        .unwrap()
        .is_some());

    // 3. Command fighter movement in space (`feat:drones.fighter-movement`)
    let move_res = ctx
        .dispatch_rpc(
            &mut s1,
            "fighterMgr",
            None,
            "ExecuteMovementCommandOnFighters",
            &[
                PyRep::List(vec![PyRep::Integer(fighter_ball_id)]),
                PyRep::String("ORBIT".into()),
                PyRep::Integer(npc_ball_id),
                PyRep::Real(2_500.0),
            ],
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
            TargetLock {
                target_id: npc_ball_id,
                locked: true,
                lock_time_ms: 1_000.0,
                remaining_lock_ms: 0.0,
                max_range_m: Some(100_000.0),
            },
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
            &[
                PyRep::List(vec![PyRep::Integer(fighter_ball_id)]),
                PyRep::Integer(0),
                PyRep::Integer(npc_ball_id),
            ],
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
            &[
                PyRep::List(vec![PyRep::Integer(fighter_ball_id)]),
                PyRep::Integer(2),
                PyRep::Integer(npc_ball_id),
            ],
            &kw,
            None,
        )
        .await
        .expect("CmdActivateAbilitySlots slot 2"),
    );
    assert_eq!(ab2.get("success"), Some(&PyRep::Bool(true)));
    let charge_count = ab2.get("chargeCount").and_then(|v| v.as_i64()).unwrap_or(0);
    let rem_charges = ab2
        .get("remainingCharges")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    assert_eq!(rem_charges, charge_count - 1);

    // 5. Apply damage to fighter squadron and verify per-unit losses (`feat:drones.fighter-squadron-losses`)
    let (units_lost, remaining_size, sq_destroyed) = apply_damage_to_fighter_squadron(
        &ctx,
        sys_id,
        fighter_ball_id,
        DamageVector {
            em: 5_000.0,
            thermal: 0.0,
            kinetic: 0.0,
            explosive: 0.0,
        },
    )
    .await;
    assert_eq!(units_lost, 1, "Fighter squadron must lose 1 unit when damaged past unit_hp");
    assert_eq!(remaining_size, 6 - units_lost);
    assert!(!sq_destroyed);

    // 6. Recall fighters to tube (`feat:drones.fighter-launch-recall`, `feat:drones.fighter-squadron-losses`)
    // Verify squadron refills lost units from FighterBay reserve back to max squadron size (6) and rearms Slot 2 charges
    let recall_res = ctx
        .dispatch_rpc(
            &mut s1,
            "fighterMgr",
            None,
            "RecallFightersToTubes",
            &[PyRep::List(vec![PyRep::Integer(fighter_ball_id)])],
            &kw,
            None,
        )
        .await
        .expect("RecallFightersToTubes");
    let PyRep::List(recalled_list) = recall_res else {
        panic!("Expected list from RecallFightersToTubes");
    };
    assert_eq!(recalled_list.len(), 1);
    let sq_qty_after_recall = ctx
        .db
        .with_snapshot(|snap| snap.items.get(&squadron_item_id).unwrap().quantity);
    assert_eq!(
        sq_qty_after_recall, 6,
        "Recalled squadron must refill lost units from FighterBay reserve back to max squadron size"
    );
    assert!(
        ctx.fighter_charge_state.read().unwrap().is_empty(),
        "Recalled squadron must rearm ability charges"
    );

    // 7. Re-launch and abandon in space (`feat:drones.fighter-abandon`)
    let relaunch_early = ctx
        .dispatch_rpc(
            &mut s1,
            "fighterMgr",
            None,
            "LaunchFightersFromTubes",
            &[PyRep::List(vec![PyRep::Integer(0)])],
            &kw,
            None,
        )
        .await;
    assert_eq!(relaunch_early.unwrap_err().code, "TubeRearming");
    ctx.advance_virtual_clock(16);

    let relaunch_res = ctx
        .dispatch_rpc(
            &mut s1,
            "fighterMgr",
            None,
            "LaunchFightersFromTubes",
            &[PyRep::List(vec![PyRep::Integer(0)])],
            &kw,
            None,
        )
        .await
        .expect("Re-launch fighters");
    let PyRep::List(relaunched) = relaunch_res else {
        panic!("Expected list");
    };
    let relaunched_ball_id = relaunched[0].as_i64().unwrap();

    let PyRep::List(abandoned_list) = ctx
        .dispatch_rpc(
            &mut s1,
            "fighterMgr",
            None,
            "CmdAbandonFighter",
            &[PyRep::Integer(relaunched_ball_id)],
            &kw,
            None,
        )
        .await
        .expect("CmdAbandonFighter")
    else {
        panic!("Expected list from CmdAbandonFighter");
    };
    assert_eq!(abandoned_list, vec![PyRep::Integer(relaunched_ball_id)]);
    assert!(
        ctx.active_fighters
            .read()
            .unwrap()
            .get(&relaunched_ball_id)
            .unwrap()
            .is_abandoned
    );
}

#[tokio::test]
async fn ship_insurance_quotes_purchase_contracts_void_link_and_payout_vs_concord() {
    // parity: feat:combat.insurance-quote
    // parity: feat:combat.insurance-buy
    // parity: feat:combat.insurance-payout
    // parity: feat:ships.ship-insurance-link
    eve_testkit::attest(&["feat:combat.insurance-quote", "feat:combat.insurance-buy", "feat:combat.insurance-payout", "feat:ships.ship-insurance-link"]);

    let ctx = build_ctx();
    let lowsec_sys_id: u32 = 30_002_187; // Amamake (low-sec 0.4)
    let highsec_sys_id: u32 = 30_000_142; // Jita (high-sec 0.9)
    let killer_id: i64 = 90_000_001;
    let victim_id: i64 = 90_000_002;
    let killer_ship_id: i64 = 920_001;
    let killer_gun_id: i64 = 920_002;
    let killer_ammo_id: i64 = 920_003;
    let victim_ship_id: i64 = 920_010;
    let traded_ship_id: i64 = 920_020;

    // 1. Verify 6-tier insurance quotes (`0.5` Basic .. `1.0` Platinum) (`feat:combat.insurance-quote`)
    let (ref_val, quote_levels) = compute_ship_insurance_quote_levels(&ctx, RIFTER_TYPE_ID);
    assert!(ref_val > 0.0);
    assert_eq!(quote_levels.len(), 6);
    assert_eq!(quote_levels[0].1, "Basic");
    assert_eq!(quote_levels[5].1, "Platinum");
    assert!(quote_levels[5].3 > quote_levels[0].3);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(k) = snap.characters.get_mut(&killer_id) {
            k.station_id = None;
            k.solar_system_id = lowsec_sys_id;
            k.active_ship_id = killer_ship_id;
            k.balance = IskCents::from_isk(100_000_000);
        }
        if let Some(v) = snap.characters.get_mut(&victim_id) {
            v.station_id = None;
            v.solar_system_id = lowsec_sys_id;
            v.active_ship_id = victim_ship_id;
            v.balance = IskCents::from_isk(10_000_000);
        }
        for (sid, oid, loc) in [
            (killer_ship_id, killer_id, lowsec_sys_id as i64),
            (victim_ship_id, victim_id, lowsec_sys_id as i64),
            (traded_ship_id, victim_id, 60_003_760),
        ] {
            snap.items.insert(
                sid,
                InventoryItemRecord {
                    item_id: sid,
                    type_id: RIFTER_TYPE_ID,
                    owner_id: oid,
                    location_id: loc,
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
        }
        snap.items.insert(
            killer_gun_id,
            InventoryItemRecord {
                item_id: killer_gun_id,
                type_id: AUTOCANNON_TYPE_ID,
                owner_id: killer_id,
                location_id: killer_ship_id,
                flag: InventoryFlag::HiSlot0,
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
        snap.items.insert(
            killer_ammo_id,
            InventoryItemRecord {
                item_id: killer_ammo_id,
                type_id: EMP_S_TYPE_ID,
                owner_id: killer_id,
                location_id: killer_ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 200,
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

    let mut s_killer = make_session(10, 1, killer_id, lowsec_sys_id, killer_ship_id, None);
    let mut s_victim = make_session(11, 2, victim_id, lowsec_sys_id, victim_ship_id, None);
    let kw = HashMap::new();

    // 2. Quote & buy Platinum insurance on both victim ships (`feat:combat.insurance-quote`, `feat:combat.insurance-buy`)
    let quote_rep = ctx
        .dispatch_rpc(
            &mut s_victim,
            "insuranceSvc",
            None,
            "GetInsurancePrice",
            &[PyRep::Integer(victim_ship_id)],
            &kw,
            None,
        )
        .await
        .expect("GetInsurancePrice");
    let plat_payout = quote_rep.as_f64().unwrap();
    assert!(plat_payout > 0.0);

    let insure_kv = kv_map(
        &ctx.dispatch_rpc(
            &mut s_victim,
            "insuranceSvc",
            None,
            "InsureShip",
            &[PyRep::Integer(victim_ship_id), PyRep::Real(1.0)],
            &kw,
            None,
        )
        .await
        .expect("InsureShip victim_ship_id"),
    );
    let insured_payout = insure_kv.get("payoutISK").and_then(|v| v.as_f64()).unwrap();
    assert!((insured_payout - plat_payout).abs() < 1e-2);

    ctx.dispatch_rpc(
        &mut s_victim,
        "insuranceSvc",
        None,
        "InsureShip",
        &[PyRep::Integer(traded_ship_id), PyRep::Real(1.0)],
        &kw,
        None,
    )
    .await
    .expect("InsureShip traded_ship_id");

    // 3. Verify `GetContracts` lists both contracts, and repackaging/transferring `traded_ship_id` voids its contract (`feat:combat.insurance-contracts`, `feat:ships.ship-insurance-link`)
    let PyRep::List(contracts_before) = ctx
        .dispatch_rpc(&mut s_victim, "insuranceSvc", None, "GetContracts", &[], &kw, None)
        .await
        .expect("GetContracts before repackage")
    else {
        panic!("Expected list from GetContracts");
    };
    assert_eq!(contracts_before.len(), 2);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(it) = snap.items.get_mut(&traded_ship_id) {
            it.singleton = false; // Repackaged ship voids insurance link
        }
    });
    let PyRep::List(contracts_after_repack) = ctx
        .dispatch_rpc(&mut s_victim, "insuranceSvc", None, "GetContracts", &[], &kw, None)
        .await
        .expect("GetContracts after repackage")
    else {
        panic!("Expected list from GetContracts");
    };
    assert_eq!(
        contracts_after_repack.len(),
        1,
        "Repackaging a ship must void its active insurance contract"
    );

    // 4. Destroy `victim_ship_id` in low-sec PvP and verify insurance payout credited (`feat:combat.insurance-payout`)
    let lowsec_handle = ctx.systems.get_or_spawn(lowsec_sys_id);
    lowsec_handle
        .spawn_ball_with_combat(
            Ball {
                id: killer_ship_id,
                type_id: RIFTER_TYPE_ID,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 350.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 35.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_type_combat_state(RIFTER_TYPE_ID),
        )
        .await
        .unwrap();
    lowsec_handle
        .spawn_ball_with_combat(
            Ball {
                id: victim_ship_id,
                type_id: RIFTER_TYPE_ID,
                position: DVec3::new(1_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 350.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 400.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState {
                shield_hp: 0.0,
                max_shield_hp: 450.0,
                armor_hp: 0.0,
                max_armor_hp: 450.0,
                structure_hp: 5.0,
                max_structure_hp: 350.0,
                ..CombatState::default()
            },
        )
        .await
        .unwrap();

    let victim_bal_before_loss = ctx
        .db
        .with_snapshot(|s| s.characters[&victim_id].balance.to_wire_f64());
    let shot_kv = kv_map(
        &dispatch_combat_rpc(
            &ctx,
            &mut s_killer,
            "dogmaIM",
            "Activate",
            &[PyRep::Integer(killer_gun_id), PyRep::Integer(victim_ship_id)],
            &kw,
        )
        .await
        .expect("dogmaIM.Activate kill victim ship"),
    );
    assert_eq!(shot_kv.get("destroyed"), Some(&PyRep::Bool(true)));
    let victim_bal_after_loss = ctx
        .db
        .with_snapshot(|s| s.characters[&victim_id].balance.to_wire_f64());
    assert!(
        (victim_bal_after_loss - (victim_bal_before_loss + insured_payout)).abs() < 1.0,
        "Victim must receive Platinum insurance payout on legitimate PvP ship loss"
    );

    // 5. Verify CONCORD kill of a high-sec criminal voids insurance payout (`feat:combat.insurance-payout`)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(k) = snap.characters.get_mut(&killer_id) {
            k.solar_system_id = highsec_sys_id;
            k.active_ship_id = killer_ship_id;
        }
        if let Some(it) = snap.items.get_mut(&killer_ship_id) {
            it.location_id = highsec_sys_id as i64;
        }
    });
    s_killer.solar_system_id = Some(highsec_sys_id);
    ctx.dispatch_rpc(
        &mut s_killer,
        "insuranceSvc",
        None,
        "InsureShip",
        &[PyRep::Integer(killer_ship_id), PyRep::Real(1.0)],
        &kw,
        None,
    )
    .await
    .expect("Insure criminal ship");

    let jita_handle = ctx.systems.get_or_spawn(highsec_sys_id);
    let victim_jita_pod: i64 = 920_099;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(v) = snap.characters.get_mut(&victim_id) {
            v.solar_system_id = highsec_sys_id;
            v.active_ship_id = victim_jita_pod;
        }
    });
    jita_handle
        .spawn_ball_with_combat(
            Ball {
                id: killer_ship_id,
                type_id: RIFTER_TYPE_ID,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 350.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 35.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_type_combat_state(RIFTER_TYPE_ID),
        )
        .await
        .unwrap();
    jita_handle
        .spawn_ball_with_combat(
            Ball {
                id: victim_jita_pod,
                type_id: 670,
                position: DVec3::new(1_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 335.0,
                inertia: 1.0,
                mass: 16_000.0,
                radius: 15.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_type_combat_state(670),
        )
        .await
        .unwrap();

    let criminal_bal_before_concord = ctx
        .db
        .with_snapshot(|s| s.characters[&killer_id].balance.to_wire_f64());
    let concord_kv = kv_map(
        &ctx.dispatch_rpc(
            &mut s_killer,
            // rpc-lint: allow legacy CmdAttackTarget test alias
            "beyonce",
            None,
            // rpc-lint: allow legacy CmdAttackTarget test alias
            "CmdAttackTarget",
            &[PyRep::Integer(victim_id)],
            &kw,
            None,
        )
        .await
        .expect("Illegal high-sec attack triggers CONCORD"),
    );
    assert_eq!(
        concord_kv.get("criminalCharacterID"),
        Some(&PyRep::Integer(killer_id))
    );
    let criminal_bal_after_concord = ctx
        .db
        .with_snapshot(|s| s.characters[&killer_id].balance.to_wire_f64());
    assert!(
        (criminal_bal_after_concord - criminal_bal_before_concord).abs() < 1e-2,
        "CONCORD destruction of a criminal ship must void insurance payout (0 ISK paid)"
    );
}

#[tokio::test]
async fn player_bounties_claim_leaderboards_and_gm_bounty_tools() {
    // parity: feat:combat.player-bounties
    // parity: feat:combat.bounty-leaderboards
    // parity: feat:admin.gm-bounty-tools
    eve_testkit::attest(&["feat:combat.player-bounties", "feat:combat.bounty-leaderboards", "feat:admin.gm-bounty-tools"]);

    let ctx = build_ctx();
    let lowsec_sys_id: u32 = 30_002_187;
    let hunter_id: i64 = 90_000_001;
    let outlaw_id: i64 = 90_000_002;
    let hunter_ship_id: i64 = 930_001;
    let hunter_gun_id: i64 = 930_002;
    let hunter_ammo_id: i64 = 930_003;
    let outlaw_ship_id: i64 = 930_010;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(h) = snap.characters.get_mut(&hunter_id) {
            h.station_id = None;
            h.solar_system_id = lowsec_sys_id;
            h.active_ship_id = hunter_ship_id;
            h.balance = IskCents::from_isk(50_000_000);
        }
        if let Some(o) = snap.characters.get_mut(&outlaw_id) {
            o.station_id = None;
            o.solar_system_id = lowsec_sys_id;
            o.active_ship_id = outlaw_ship_id;
            o.balance = IskCents::from_isk(10_000_000);
        }
        for (sid, oid) in [(hunter_ship_id, hunter_id), (outlaw_ship_id, outlaw_id)] {
            snap.items.insert(
                sid,
                InventoryItemRecord {
                    item_id: sid,
                    type_id: RIFTER_TYPE_ID,
                    owner_id: oid,
                    location_id: lowsec_sys_id as i64,
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
        }
        snap.items.insert(
            hunter_gun_id,
            InventoryItemRecord {
                item_id: hunter_gun_id,
                type_id: AUTOCANNON_TYPE_ID,
                owner_id: hunter_id,
                location_id: hunter_ship_id,
                flag: InventoryFlag::HiSlot0,
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
        snap.items.insert(
            hunter_ammo_id,
            InventoryItemRecord {
                item_id: hunter_ammo_id,
                type_id: EMP_S_TYPE_ID,
                owner_id: hunter_id,
                location_id: hunter_ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 200,
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

    let mut s_hunter = make_session(20, 1, hunter_id, lowsec_sys_id, hunter_ship_id, None);
    let mut s_outlaw = make_session(21, 2, outlaw_id, lowsec_sys_id, outlaw_ship_id, None);
    let kw = HashMap::new();

    // 1. Place bounty on `outlaw_id` and `hunter_id` (`feat:combat.player-bounties`)
    let add_kv = kv_map(
        &ctx.dispatch_rpc(
            &mut s_hunter,
            "bountyProxy",
            None,
            "AddToBounty",
            &[PyRep::Integer(outlaw_id), PyRep::Real(5_000_000.0)],
            &kw,
            None,
        )
        .await
        .expect("AddToBounty outlaw"),
    );
    assert_eq!(add_kv.get("totalBountyISK"), Some(&PyRep::Real(5_000_000.0)));

    ctx.dispatch_rpc(
        &mut s_outlaw,
        "bountyProxy",
        None,
        "AddToBounty",
        &[PyRep::Integer(hunter_id), PyRep::Real(1_000_000.0)],
        &kw,
        None,
    )
    .await
    .expect("AddToBounty hunter");

    // 2. Verify bounty leaderboards rank `outlaw_id` (5M) above `hunter_id` (1M) (`feat:combat.bounty-leaderboards`)
    let PyRep::List(top_bounties) = ctx
        .dispatch_rpc(
            &mut s_hunter,
            "bountyProxy",
            None,
            "GetTopPilotBounties",
            &[],
            &kw,
            None,
        )
        .await
        .expect("GetTopPilotBounties")
    else {
        panic!("Expected list from GetTopPilotBounties");
    };
    assert_eq!(top_bounties.len(), 2);
    assert_eq!(
        kv_map(&top_bounties[0]).get("targetID"),
        Some(&PyRep::Integer(outlaw_id))
    );

    // 3. Destroy `outlaw_ship_id` and verify bounty payout (20% of destroyed ship reference value) (`feat:combat.player-bounties`)
    let lowsec_handle = ctx.systems.get_or_spawn(lowsec_sys_id);
    lowsec_handle
        .spawn_ball_with_combat(
            Ball {
                id: hunter_ship_id,
                type_id: RIFTER_TYPE_ID,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 350.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 35.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_type_combat_state(RIFTER_TYPE_ID),
        )
        .await
        .unwrap();
    lowsec_handle
        .spawn_ball_with_combat(
            Ball {
                id: outlaw_ship_id,
                type_id: RIFTER_TYPE_ID,
                position: DVec3::new(1_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 350.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 400.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState {
                shield_hp: 0.0,
                max_shield_hp: 450.0,
                armor_hp: 0.0,
                max_armor_hp: 450.0,
                structure_hp: 5.0,
                max_structure_hp: 350.0,
                ..CombatState::default()
            },
        )
        .await
        .unwrap();

    let hunter_bal_before = ctx
        .db
        .with_snapshot(|s| s.characters[&hunter_id].balance.to_wire_f64());
    dispatch_combat_rpc(
        &ctx,
        &mut s_hunter,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(hunter_gun_id), PyRep::Integer(outlaw_ship_id)],
        &kw,
    )
    .await
    .expect("Destroy outlaw ship");
    let hunter_bal_after = ctx
        .db
        .with_snapshot(|s| s.characters[&hunter_id].balance.to_wire_f64());
    assert!(
        hunter_bal_after > hunter_bal_before,
        "Bounty hunter must receive 20% ship value bounty payout"
    );

    let res = ctx
        .dispatch_rpc(
            &mut s_hunter,
            "bountyProxy",
            None,
            "GetTopPilotBountyHunters",
            &[],
            &kw,
            None,
        )
        .await
        .expect("GetTopPilotBountyHunters");
    let top_hunters = match res {
        PyRep::Tuple(t) => match t.into_iter().next() {
            Some(PyRep::List(l)) => l,
            _ => panic!("Expected list in tuple"),
        },
        PyRep::List(l) => l,
        _ => panic!("Expected list from GetTopPilotBountyHunters"),
    };
    assert_eq!(
        kv_map(&top_hunters[0]).get("targetID"),
        Some(&PyRep::Integer(hunter_id))
    );

    // 4. Verify GM Bounty Tools (`feat:admin.gm-bounty-tools`)
    let non_gm_err = ctx
        .dispatch_rpc(
            &mut s_hunter,
            "bountyProxy",
            None,
            "GMReimburseBounties",
            &[PyRep::Integer(outlaw_id)],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(non_gm_err.code, "InsufficientRole");

    s_hunter.role |= service_roles::ROLE_GMH;
    let reimburse_kv = kv_map(
        &ctx.dispatch_rpc(
            &mut s_hunter,
            "bountyProxy",
            None,
            "GMReimburseBounties",
            &[PyRep::Integer(outlaw_id)],
            &kw,
            None,
        )
        .await
        .expect("GMReimburseBounties"),
    );
    assert_eq!(
        reimburse_kv.get("amountISK"),
        Some(&PyRep::Real(4_920_000.0))
    );

    let clear_kv = kv_map(
        &ctx.dispatch_rpc(
            &mut s_hunter,
            "bountyProxy",
            None,
            "ClearBounty",
            &[PyRep::Integer(hunter_id)],
            &kw,
            None,
        )
        .await
        .expect("ClearBounty"),
    );
    assert_eq!(clear_kv.get("amountISK"), Some(&PyRep::Real(1_000_000.0)));
}

#[tokio::test]
async fn nullsec_ess_main_bank_reserve_security_key_and_dynamic_bounty_multiplier() {
    // parity: feat:pve.ess
    // parity: feat:pve.ess-reserve
    // parity: feat:pve.dynamic-bounty
    eve_testkit::attest(&["feat:pve.ess", "feat:pve.ess-reserve", "feat:pve.dynamic-bounty"]);

    let ctx = build_ctx();
    let nullsec_sys_id: u32 = 30_004_759; // VFK-IV (K-space null-sec)
    let char_id: i64 = 90_000_001;
    let ship_id: i64 = 940_001;
    let ess_ball_id: i64 = 940_010;
    let key_item_id: i64 = 940_020;

    // 1. Verify Dynamic Bounty System (`dbs_multiplier`) decay under ratting and recovery on PvP kills (`feat:pve.dynamic-bounty`)
    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states.insert(
            nullsec_sys_id,
            SystemDynamicState {
                solar_system_id: nullsec_sys_id,
                dbs_multiplier: 1.35,
                ess_main_bank: IskCents::from_isk(25_000_000),
                ess_reserve_bank: IskCents::from_isk(40_000_000),
                ess_linked_character_id: None,
                ess_link_started_tick: None,
                sov_alliance_id: None,
                military_index: 1,
                industrial_index: 1,
                strategic_index: 1,
                fw_contested_pct: 0.0,
                insurgency_corruption_stage: 0,
                insurgency_suppression_stage: 0,
                wormhole_remaining_mass_kg: 0.0,
                wormhole_destination_system_id: None,
                signature_position: None,
                fw_complex_position: None,
                fw_capture_seconds_elapsed: 0,
                fw_capture_duration_seconds: 0,
                last_fw_capture_tick: 0,
                last_sov_claim_tick: 0,
            },
        );
    });
    let after_ratting = step_dynamic_bounty_multiplier(&ctx, nullsec_sys_id, 100_000_000.0, 0.0);
    assert!(
        after_ratting < 1.35 && after_ratting >= 0.50,
        "Ratting volume must decay dbs_multiplier toward 0.50 floor"
    );
    let after_pvp = step_dynamic_bounty_multiplier(&ctx, nullsec_sys_id, 0.0, 75_000_000.0);
    assert!(
        after_pvp > after_ratting && after_pvp <= 2.00,
        "Capsuleer PvP destruction must boost dbs_multiplier toward 2.00 ceiling"
    );

    // 2. Seed ship and ESS beacon in VFK-IV (`feat:pve.ess`, `feat:pve.ess-reserve`)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.solar_system_id = nullsec_sys_id;
            ch.active_ship_id = ship_id;
            ch.balance = IskCents::from_isk(10_000_000);
        }
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: RIFTER_TYPE_ID,
                owner_id: char_id,
                location_id: nullsec_sys_id as i64,
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
    });

    let sys_handle = ctx.systems.get_or_spawn(nullsec_sys_id);
    sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: RIFTER_TYPE_ID,
                position: DVec3::new(1_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 350.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 35.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_type_combat_state(RIFTER_TYPE_ID),
        )
        .await
        .unwrap();
    sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: ess_ball_id,
                type_id: ESS_BEACON_TYPE_ID,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 1.0,
                mass: 50_000_000.0,
                radius: 500.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_type_combat_state(ESS_BEACON_TYPE_ID),
        )
        .await
        .unwrap();

    let mut s1 = make_session(30, 1, char_id, nullsec_sys_id, ship_id, None);
    let kw = HashMap::new();

    // 3. Link to ESS Main Bank, step 5 Destiny ticks, and steal 25M ISK (`feat:pve.ess`)
    let main_start = kv_map(
        &ctx.dispatch_rpc(
            &mut s1,
            "essMgr",
            None,
            "AttemptLinkToMainBank",
            &[PyRep::Integer(ess_ball_id)],
            &kw,
            None,
        )
        .await
        .expect("AttemptLinkToMainBank start"),
    );
    assert_eq!(main_start.get("linkStartedOnly"), Some(&PyRep::Bool(true)));

    for _ in 0..5 {
        let _ = sys_handle.step_manual(1.0).await.unwrap();
    }
    let main_complete = kv_map(
        &ctx.dispatch_rpc(
            &mut s1,
            "essMgr",
            None,
            "AttemptLinkToMainBank",
            &[PyRep::Integer(ess_ball_id)],
            &kw,
            None,
        )
        .await
        .expect("AttemptLinkToMainBank complete"),
    );
    assert_eq!(
        main_complete.get("stolenISK"),
        Some(&PyRep::Real(25_000_000.0))
    );

    // 4. Verify `AttemptLinkToReserveBank` rejects without a Reserve Security Key, then consumes key and pays out (`feat:pve.ess-reserve`)
    let no_key_err = ctx
        .dispatch_rpc(
            &mut s1,
            "essMgr",
            None,
            "AttemptLinkToReserveBank",
            &[PyRep::Integer(ess_ball_id)],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(no_key_err.code, "MissingEssReserveSecurityKey");

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            key_item_id,
            InventoryItemRecord {
                item_id: key_item_id,
                type_id: ESS_RESERVE_KEY_TYPE_ID,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 1,
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

    let res_start = kv_map(
        &ctx.dispatch_rpc(
            &mut s1,
            "essMgr",
            None,
            "AttemptLinkToReserveBank",
            &[PyRep::Integer(ess_ball_id)],
            &kw,
            None,
        )
        .await
        .expect("AttemptLinkToReserveBank start with key"),
    );
    assert_eq!(res_start.get("linkStartedOnly"), Some(&PyRep::Bool(true)));
    assert!(
        ctx.db.with_snapshot(|s| !s.items.contains_key(&key_item_id)),
        "ESS Reserve Security Key must be consumed when starting Reserve Bank link"
    );

    for _ in 0..5 {
        let _ = sys_handle.step_manual(1.0).await.unwrap();
    }
    let res_complete = kv_map(
        &ctx.dispatch_rpc(
            &mut s1,
            "essMgr",
            None,
            "AttemptLinkToReserveBank",
            &[PyRep::Integer(ess_ball_id)],
            &kw,
            None,
        )
        .await
        .expect("AttemptLinkToReserveBank complete"),
    );
    let stolen = match res_complete.get("stolenISK") {
        Some(PyRep::Real(v)) => *v,
        other => panic!("expected Real stolenISK, got {:?}", other),
    };
    assert!((stolen - 40_000_000.0 / 15.0).abs() < 0.1, "stolen: {stolen}");
}
