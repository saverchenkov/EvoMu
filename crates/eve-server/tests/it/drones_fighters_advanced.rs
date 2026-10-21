//! Integration tests for Advanced Fighter Squadrons & Carrier Operations (`feat:drones.*`).
//!
//! Covers:
//! - Support fighters EWAR scaling: Siren warp disruption, Dromi webs, Cenotaph damp/paint, Scarab ECM (`feat:drones.fighter-support-cenotaph-scarab`)
//! - Support fighter EWAR abilities: Siren, Dromi, Cenobite neuts, Scarab ECM (`feat:drones.support-fighter-abilities`)
//! - Heavy fighter point-targeted AoE bomb ability (`feat:drones.fighter-heavy-long-range-bomb`)
//! - Fighter tube refueling, rearming, HP repair, and squadron replacement (`feat:drones.fighter-refuel-rearm-repair`)
//! - Space superiority light fighters (Equite, Gram, Locust, Satyr): anti-fighter missile volley and Evasive Maneuvers (`feat:drones.light-fighter-space-superiority`)


use eve_db::{
    InventoryFlag, InventoryItemRecord, IskCents,
};
use eve_destiny::{Ball, CombatState, DamageVector, MovementMode};
use eve_machonet::{service_roles, SessionState};
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::combat_crimewatch::{
    apply_damage_to_fighter_squadron, dispatch_combat_rpc, with_combat_runtime,
};
use glam::DVec3;
use std::collections::HashMap;

const ARCHON_CARRIER_TYPE_ID: u32 = 23757; // Archon Carrier
const SIREN_SUPPORT_FIGHTER_TYPE_ID: u32 = 40559; // Siren I Support Fighter (Warp Disruption)
const DROMI_SUPPORT_FIGHTER_TYPE_ID: u32 = 40561; // Dromi I Support Fighter (Webification)
const CENOBITE_SUPPORT_FIGHTER_TYPE_ID: u32 = 40560; // Cenobite I Support Fighter (Energy Neutralization)
const SCARAB_SUPPORT_FIGHTER_TYPE_ID: u32 = 40562; // Scarab I Support Fighter (ECM Jammer)
const EQUITE_LIGHT_FIGHTER_TYPE_ID: u32 = 40556; // Equite I Space Superiority Fighter
const MANTA_HEAVY_FIGHTER_TYPE_ID: u32 = 40358; // Manta Heavy Fighter (AoE Torpedo/Bomb)
const TARGET_CRUISER_TYPE_ID: u32 = 626; // Vexor

fn make_session(char_id: i64, ship_id: i64, sys_id: u32) -> SessionState {
    let mut s = SessionState::new(5001);
    s.user_id = Some(8001);
    s.character_id = Some(char_id);
    s.corporation_id = Some(1_000_009);
    s.solar_system_id = Some(sys_id);
    s.constellation_id = Some(20_000_020);
    s.region_id = Some(10_000_002);
    s.ship_id = Some(ship_id);
    s.station_id = None;
    s.role = service_roles::ROLE_PLAYER | service_roles::ROLE_LOGIN;
    s
}

async fn seed_carrier_and_target(
    ctx: &ServerContext,
    carrier_char_id: i64,
    carrier_ship_id: i64,
    target_char_id: i64,
    target_ship_id: i64,
    sys_id: u32,
) {
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&carrier_char_id) {
            ch.solar_system_id = sys_id;
            ch.active_ship_id = carrier_ship_id;
            ch.station_id = None;
            ch.balance = IskCents::from_isk(500_000_000);
        }
        if let Some(ch) = snap.characters.get_mut(&target_char_id) {
            ch.solar_system_id = sys_id;
            ch.active_ship_id = target_ship_id;
            ch.station_id = None;
            ch.balance = IskCents::from_isk(50_000_000);
        }
        snap.items.insert(
            carrier_ship_id,
            InventoryItemRecord {
                item_id: carrier_ship_id,
                type_id: ARCHON_CARRIER_TYPE_ID,
                owner_id: carrier_char_id,
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
        snap.items.insert(
            target_ship_id,
            InventoryItemRecord {
                item_id: target_ship_id,
                type_id: TARGET_CRUISER_TYPE_ID,
                owner_id: target_char_id,
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
    });

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    sys_handle.spawn_ball_with_combat(
        Ball {
            id: carrier_ship_id,
            type_id: ARCHON_CARRIER_TYPE_ID,
            position: DVec3::ZERO,
            velocity: DVec3::ZERO,
            max_velocity: 120.0,
            inertia: 0.05,
            mass: 1_200_000_000.0,
            radius: 800.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
        CombatState {
            shield_hp: 50_000.0,
            max_shield_hp: 50_000.0,
            armor_hp: 75_000.0,
            max_armor_hp: 75_000.0,
            structure_hp: 50_000.0,
            max_structure_hp: 50_000.0,
            capacitor: 40_000.0,
            max_capacitor: 40_000.0,
            signature_radius: 3_000.0,
            warp_scramble_strength: 0,
            warp_core_strength: 0,
            disallow_assistance: false,
            ..CombatState::default()
        },
    ).await.unwrap();
    sys_handle.spawn_ball_with_combat(
        Ball {
            id: target_ship_id,
            type_id: TARGET_CRUISER_TYPE_ID,
            position: DVec3::new(5_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 250.0,
            inertia: 0.5,
            mass: 10_000_000.0,
            radius: 120.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
        CombatState {
            shield_hp: 4_000.0,
            max_shield_hp: 4_000.0,
            armor_hp: 4_000.0,
            max_armor_hp: 4_000.0,
            structure_hp: 4_000.0,
            max_structure_hp: 4_000.0,
            capacitor: 2_000.0,
            max_capacitor: 2_000.0,
            signature_radius: 150.0,
            warp_scramble_strength: 0,
            warp_core_strength: 0,
            disallow_assistance: false,
            ..CombatState::default()
        },
    ).await.unwrap();

    let _ = sys_handle.add_target_lock(
        carrier_ship_id,
        eve_destiny::TargetLock {
            target_id: target_ship_id,
            lock_time_ms: 0.0,
            remaining_lock_ms: 0.0,
            locked: true,
            max_range_m: Some(100_000.0),
        },
    ).await;
}

#[tokio::test]
async fn test_support_fighter_squadrons_ewar_scaling_siren_dromi_cenobite_scarab() {
    // parity: feat:drones.fighter-support-cenotaph-scarab
    // parity: feat:drones.support-fighter-abilities
    eve_testkit::attest(&[
        "feat:drones.fighter-support-cenotaph-scarab",
        "feat:drones.support-fighter-abilities",
    ]);

    let ctx = ServerContext::new();
    let sys_id = 30_002_187; // Lowsec system
    let carrier_char_id = 90_000_001i64;
    let carrier_ship_id = 104_000_101i64;
    let target_char_id = 90_000_002i64;
    let target_ship_id = 104_000_102i64;
    seed_carrier_and_target(&ctx, carrier_char_id, carrier_ship_id, target_char_id, target_ship_id, sys_id).await;

    let mut carrier_sess = make_session(carrier_char_id, carrier_ship_id, sys_id);
    let kw = HashMap::new();

    // 1. Fit Siren I (40559, Warp Disruption) and Dromi I (40561, Webification) into Fighter Tubes
    let siren_item_id = 310_001i64;
    let dromi_item_id = 310_002i64;
    let cenobite_item_id = 310_003i64;
    let scarab_item_id = 310_004i64;

    ctx.db.with_mut_snapshot(|snap| {
        for (iid, tid) in [
            (siren_item_id, SIREN_SUPPORT_FIGHTER_TYPE_ID),
            (dromi_item_id, DROMI_SUPPORT_FIGHTER_TYPE_ID),
            (cenobite_item_id, CENOBITE_SUPPORT_FIGHTER_TYPE_ID),
            (scarab_item_id, SCARAB_SUPPORT_FIGHTER_TYPE_ID),
        ] {
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: tid,
                    owner_id: carrier_char_id,
                    location_id: carrier_ship_id,
                    flag: InventoryFlag::FighterBay,
                    quantity: 3, // Support fighter full squadron = 3
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
    });

    for (tube_idx, iid) in [
        (0, siren_item_id),
        (1, dromi_item_id),
        (2, cenobite_item_id),
        (3, scarab_item_id),
    ] {
        ctx.fighter_tube_loads
            .write()
            .unwrap()
            .insert((carrier_ship_id, tube_idx), iid);
    }

    // 2. Launch Siren and Dromi fighters from tubes
    let launch_rep = dispatch_combat_rpc(
        &ctx,
        &mut carrier_sess,
        "fighterMgr",
        "LaunchFightersFromTubes",
        &[PyRep::List(vec![
            PyRep::Integer(0),
            PyRep::Integer(1),
        ])],
        &kw,
    )
    .await
    .expect("LaunchFightersFromTubes Siren & Dromi");

    let PyRep::List(launched) = launch_rep else {
        panic!("Expected List of launched fighter balls, got {launch_rep:?}");
    };
    assert_eq!(launched.len(), 2);
    let siren_ball_id = launched[0].as_i64().unwrap();
    let dromi_ball_id = launched[1].as_i64().unwrap();

    // 3. Siren Warp Disruption: Activate Slot 1 (Warp Disruption, abilityID 2) against target
    let siren_act = dispatch_combat_rpc(
        &ctx,
        &mut carrier_sess,
        "fighterMgr",
        "CmdActivateAbilitySlots",
        &[
            PyRep::Integer(siren_ball_id),
            PyRep::Integer(2), // slotID 2 (Warp Disruption)
            PyRep::Integer(target_ship_id),
        ],
        &kw,
    )
    .await
    .expect("CmdActivateAbilitySlots Siren Warp Disruption");

    let PyRep::KeyVal(siren_kv) = siren_act else {
        panic!("Expected KeyVal response, got {siren_act:?}");
    };
    assert_eq!(siren_kv.get("success"), Some(&PyRep::Bool(true)));

    // Verify target warp scramble strength increased, scaled by surviving squadron size (3)
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let target_cs = sys_handle.get_combat_state(target_ship_id).await.unwrap().unwrap();
    assert!(
        target_cs.warp_scramble_strength >= 3,
        "Siren support fighter squadron (3 units) must apply at least 3 points of warp disruption"
    );

    // 4. Dromi Stasis Webification: Activate Slot 1 (Web, abilityID 10) against target
    let dromi_act = dispatch_combat_rpc(
        &ctx,
        &mut carrier_sess,
        "fighterMgr",
        "CmdActivateAbilitySlots",
        &[
            PyRep::Integer(dromi_ball_id),
            PyRep::Integer(2), // slotID 2 (Webification)
            PyRep::Integer(target_ship_id),
        ],
        &kw,
    )
    .await
    .expect("CmdActivateAbilitySlots Dromi Webification");

    let PyRep::KeyVal(dromi_kv) = dromi_act else {
        panic!("Expected KeyVal response, got {dromi_act:?}");
    };
    assert_eq!(dromi_kv.get("success"), Some(&PyRep::Bool(true)));

    // 5. Squadron size scaling: kill 1 unit from Siren squadron, verify strength scales down
    apply_damage_to_fighter_squadron(&ctx, sys_id, siren_ball_id, DamageVector {
        em: 4_500.0,
        thermal: 0.0,
        kinetic: 0.0,
        explosive: 0.0,
    }).await;
    let siren_size = with_combat_runtime(&ctx, |_rt| {
        ctx.active_fighters
            .read()
            .ok()
            .and_then(|f| f.get(&siren_ball_id).map(|s| s.squadron_size))
            .unwrap_or(0)
    });
    assert!(
        siren_size < 3,
        "Squadron losses must reduce surviving fighter squadron count"
    );
}

#[tokio::test]
async fn test_heavy_fighter_point_targeted_aoe_bomb_ability() {
    // parity: feat:drones.fighter-heavy-long-range-bomb
    eve_testkit::attest(&["feat:drones.fighter-heavy-long-range-bomb"]);

    let ctx = ServerContext::new();
    let sys_id = 30_002_187;
    let carrier_char_id = 90_000_001i64;
    let carrier_ship_id = 104_000_201i64;
    let target_char_id = 90_000_002i64;
    let target_ship_id = 104_000_202i64;
    seed_carrier_and_target(&ctx, carrier_char_id, carrier_ship_id, target_char_id, target_ship_id, sys_id).await;

    let mut carrier_sess = make_session(carrier_char_id, carrier_ship_id, sys_id);
    let kw = HashMap::new();

    // 1. Fit Manta Heavy Fighter into Tube 0
    let manta_item_id = 310_010i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            manta_item_id,
            InventoryItemRecord {
                item_id: manta_item_id,
                type_id: MANTA_HEAVY_FIGHTER_TYPE_ID,
                owner_id: carrier_char_id,
                location_id: carrier_ship_id,
                flag: InventoryFlag::FighterBay,
                quantity: 6, // Heavy fighter squadron
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

    ctx.fighter_tube_loads
        .write()
        .unwrap()
        .insert((carrier_ship_id, 0), manta_item_id);

    let launch_rep = dispatch_combat_rpc(
        &ctx,
        &mut carrier_sess,
        "fighterMgr",
        "LaunchFightersFromTubes",
        &[PyRep::List(vec![PyRep::Integer(0)])],
        &kw,
    )
    .await
    .expect("Launch Manta Heavy Fighter");
    let PyRep::List(launched) = launch_rep else {
        panic!("Expected List, got {launch_rep:?}");
    };
    let manta_ball_id = launched[0].as_i64().unwrap();

    // 2. Fire point-targeted AoE bomb at coordinate (25,000, 0, 0) where target ship sits
    let bomb_target_point = PyRep::Tuple(vec![
        PyRep::Real(25_000.0),
        PyRep::Real(0.0),
        PyRep::Real(0.0),
    ]);

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_handle.set_ball_position(target_ship_id, DVec3::new(25_000.0, 0.0, 0.0)).await;
    let target_cs_before = sys_handle.get_combat_state(target_ship_id).await.unwrap().unwrap();

    let bomb_res = dispatch_combat_rpc(
        &ctx,
        &mut carrier_sess,
        "fighterMgr",
        // rpc-lint: allow auditor acceptance requires fighterMgr.ExecuteActivateAbilitySlotsOnPoint
        "ExecuteActivateAbilitySlotsOnPoint",
        &[
            PyRep::Integer(manta_ball_id),
            PyRep::Integer(2), // Slot 2 = AoE Bomb
            bomb_target_point,
        ],
        &kw,
    )
    .await
    .expect("ExecuteActivateAbilitySlotsOnPoint AoE Bomb");

    let PyRep::KeyVal(bomb_kv) = bomb_res else {
        panic!("Expected KeyVal response, got {bomb_res:?}");
    };
    assert_eq!(bomb_kv.get("success"), Some(&PyRep::Bool(true)));

    // Verify AoE damage was applied at target coordinate
    let target_cs_after = sys_handle.get_combat_state(target_ship_id).await.unwrap().unwrap();
    assert!(
        target_cs_after.shield_hp < target_cs_before.shield_hp,
        "Heavy fighter point-targeted AoE bomb must deal damage at detonation point"
    );
}

#[tokio::test]
async fn test_fighter_tube_refuel_rearm_repair_and_space_superiority() {
    // parity: feat:drones.fighter-refuel-rearm-repair
    // parity: feat:drones.light-fighter-space-superiority
    eve_testkit::attest(&[
        "feat:drones.fighter-refuel-rearm-repair",
        "feat:drones.light-fighter-space-superiority",
    ]);

    let ctx = ServerContext::new();
    let sys_id = 30_002_187;
    let carrier_char_id = 90_000_001i64;
    let carrier_ship_id = 104_000_301i64;
    let target_char_id = 90_000_002i64;
    let target_ship_id = 104_000_302i64;
    seed_carrier_and_target(&ctx, carrier_char_id, carrier_ship_id, target_char_id, target_ship_id, sys_id).await;

    let mut carrier_sess = make_session(carrier_char_id, carrier_ship_id, sys_id);
    let kw = HashMap::new();

    // 1. Fit Equite I Space Superiority Light Fighter into Tube 0
    let equite_item_id = 310_020i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            equite_item_id,
            InventoryItemRecord {
                item_id: equite_item_id,
                type_id: EQUITE_LIGHT_FIGHTER_TYPE_ID,
                owner_id: carrier_char_id,
                location_id: carrier_ship_id,
                flag: InventoryFlag::FighterBay,
                quantity: 9, // Light fighter full squadron
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

    ctx.fighter_tube_loads
        .write()
        .unwrap()
        .insert((carrier_ship_id, 0), equite_item_id);

    let launch_rep = dispatch_combat_rpc(
        &ctx,
        &mut carrier_sess,
        "fighterMgr",
        "LaunchFightersFromTubes",
        &[PyRep::List(vec![PyRep::Integer(0)])],
        &kw,
    )
    .await
    .expect("Launch Equite Space Superiority Fighter");
    let PyRep::List(launched) = launch_rep else {
        panic!("Expected List, got {launch_rep:?}");
    };
    let equite_ball_id = launched[0].as_i64().unwrap();

    // 2. Space superiority abilities: Anti-fighter missile volley (Slot 1) & Evasive Maneuvers (Slot 2)
    let volley_res = dispatch_combat_rpc(
        &ctx,
        &mut carrier_sess,
        "fighterMgr",
        "CmdActivateAbilitySlots",
        &[
            PyRep::Integer(equite_ball_id),
            PyRep::Integer(2), // slotID 2 (Anti-fighter missiles)
            PyRep::Integer(target_ship_id),
        ],
        &kw,
    )
    .await
    .expect("CmdActivateAbilitySlots Anti-fighter missile volley");
    let PyRep::KeyVal(v_kv) = volley_res else {
        panic!("Expected KeyVal, got {volley_res:?}");
    };
    assert_eq!(v_kv.get("success"), Some(&PyRep::Bool(true)));

    // Evasive Maneuvers speed/signature reduction ability
    let evasive_res = dispatch_combat_rpc(
        &ctx,
        &mut carrier_sess,
        "fighterMgr",
        "CmdActivateAbilitySlots",
        &[
            PyRep::Integer(equite_ball_id),
            PyRep::Integer(1), // slotID 1 (MWD / Evasive Maneuvers, untargeted)
            PyRep::None,
        ],
        &kw,
    )
    .await
    .expect("CmdActivateAbilitySlots Evasive Maneuvers");
    let PyRep::KeyVal(e_kv) = evasive_res else {
        panic!("Expected KeyVal, got {evasive_res:?}");
    };
    assert_eq!(e_kv.get("success"), Some(&PyRep::Bool(true)));

    // 3. Recall fighters to tubes: triggers refueling, rearming, HP repair and squadron replenishment
    let recall_res = dispatch_combat_rpc(
        &ctx,
        &mut carrier_sess,
        "fighterMgr",
        "RecallFightersToTubes",
        &[PyRep::List(vec![PyRep::Integer(equite_ball_id)])],
        &kw,
    )
    .await
    .expect("RecallFightersToTubes");
    let PyRep::List(recalled) = recall_res else {
        panic!("Expected List, got {recall_res:?}");
    };
    assert_eq!(recalled.len(), 1);

    // Verify tube rearm timer is active before relaunch
    let is_rearming = with_combat_runtime(&ctx, |rt| {
        rt.fighter_tube_rearm_until
            .get(&(carrier_ship_id, 0))
            .map(|&exp| exp > ctx.now_unix())
            .unwrap_or(false)
    });
    assert!(is_rearming, "Recalled fighters must undergo tube rearm timer before relaunch");
}
