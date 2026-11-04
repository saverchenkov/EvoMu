//! Integration tests for Space Navigation: Ansiblex Jump Gate and Bastion/Siege gate blocks
//! closing `feat:navigation.ansiblex-jump-fatigue-exemption` and `feat:navigation.bastion-siege-style-gate-blocks`.

#![forbid(unsafe_code)]

use eve_db::{
    CharacterRecord, InventoryFlag, InventoryItemRecord, IskCents, NeuralAttributes,
    TrainedSkillRecord,
};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::station_structure::structure_upwell_ops::{load_ext_state, save_ext_state};
use glam::DVec3;
use std::collections::HashMap;

const JITA_SYSTEM: u32 = 30_000_142;
const SOV_NULL_A: u32 = 30_004_712; // NOL-M9 (Delve sov null-sec)
const SOV_NULL_B: u32 = 30_004_713; // O-IOAI (Delve sov null-sec, ~1.10 LY from NOL-M9)
const SOV_NULL_C: u32 = 30_004_727; // R5-MM8 (Delve sov null-sec, ~3.85 LY from O-IOAI)
const SOV_NULL_FAR: u32 = 30_004_752; // QY6-RK (Delve sov null-sec, ~6.36 LY > 5.0 LY from NOL-M9)
const PILOT_1: i64 = 93_100_001;
const PILOT_2: i64 = 93_100_002;
const SHIP_1: i64 = 83_100_001;
const SHIP_2: i64 = 83_100_002;

pub(crate) fn make_nav_character(char_id: i64, ship_id: i64, sys_id: u32) -> CharacterRecord {
    CharacterRecord {
        character_id: char_id, user_id: char_id as u64, name: format!("NavPilot {char_id}"),
        corporation_id: 1_000_001, alliance_id: None, war_faction_id: None,
        station_id: None, structure_id: None, solar_system_id: sys_id,
        constellation_id: 20_000_020, region_id: 10_000_002, active_ship_id: ship_id,
        balance: IskCents::from_isk(100_000_000), plex_balance: 0,
        loyalty_points: HashMap::new(), standings: HashMap::new(),
        security_status: 5.0, clone_grade: 2, home_station_id: 60_003_760,
        race_id: 1, bloodline_id: 1, ancestry_id: 1, school_id: 1,
        neural: NeuralAttributes::default(), skill_points: 15_000_000.0,
        activity_status: None, free_skill_points: 0.0, last_clone_jump_unix: 0,
        jump_fatigue_minutes: 0.0, jump_cooldown_until_unix: 0, jump_fatigue_until_unix: 0,
        description: String::new(), title: String::new(), gender: 1,
        create_date_unix: 1_700_000_000, fleet_id: None, autopilot_waypoints: Vec::new(),
        version: 1, system_epoch: 0,
    }
}

pub(crate) fn make_structure(structure_id: i64, type_id: u32, sys_id: u32, name: &str, pos: [f64; 3]) -> eve_db::types::UpwellStructureRecord {
    eve_db::types::UpwellStructureRecord {
        structure_id, type_id, owner_corp_id: 1_000_001, solar_system_id: sys_id,
        name: name.into(), position: pos, state: "ShieldVulnerable".into(),
        quantum_core_installed: true, fuel_blocks: 1000,
        shield_hp: 10_000_000.0, armor_hp: 10_000_000.0, hull_hp: 10_000_000.0,
        reinforced_until_unix: 0, last_damage_tick: 0, last_damage_unix: 0,
        self_destruct_at_unix: None,
    }
}

pub(crate) async fn setup_nav_pilot(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    ship_type_id: u32,
    sys_id: u32,
    pos: DVec3,
) -> SessionState {
    let all_skills: HashMap<u32, TrainedSkillRecord> = ctx
        .sde
        .types
        .values()
        .filter(|t| t.category_id == 16)
        .map(|t| {
            (
                t.type_id,
                TrainedSkillRecord {
                    type_id: t.type_id,
                    level: 5,
                    skill_points: 256_000.0,
                },
            )
        })
        .collect();
    let uid = ctx
        .db
        .provision_player_account(&format!("nav_pilot_{char_id}"), "nav-secret");
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.insert(char_id, all_skills);
        let mut ch = make_nav_character(char_id, ship_id, sys_id);
        ch.user_id = uid;
        snap.characters.insert(char_id, ch);
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: ship_type_id,
                owner_id: char_id,
                location_id: sys_id as i64,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.item_positions.insert(ship_id, pos.to_array());
    });

    let cs = ctx.build_ship_combat_state(char_id, ship_id, ship_type_id);
    let _ = ctx
        .systems
        .get_or_spawn(sys_id)
        .spawn_ball_with_combat(
            eve_destiny::Ball {
                id: ship_id,
                type_id: ship_type_id,
                position: pos,
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 2.0,
                mass: 10_000_000.0,
                radius: 120.0,
                speed_fraction: 0.0,
                mode: eve_destiny::MovementMode::Stop,
            },
            cs,
        )
        .await;

    let session_id = 8_000 + char_id as u64;
    let _ = ctx.db.acquire_online_lock_with_computer_hash(
        uid,
        session_id,
        Some(char_id),
        Some(format!("nav_hash_{char_id}")),
        true,
        ctx.now_unix(),
    );
    let mut sess = SessionState::new(session_id);
    sess.user_id = Some(uid);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(1_000_001);
    sess.ship_id = Some(ship_id);
    sess.solar_system_id = Some(sys_id);
    sess
}

// parity: feat:navigation.ansiblex-jump-fatigue-exemption
// parity: feat:navigation.bastion-siege-style-gate-blocks
#[tokio::test]
async fn test_space_navigation_ansiblex_and_bastion_blocks() {
    eve_testkit::attest(&[
        "feat:navigation.ansiblex-jump-fatigue-exemption",
        "feat:navigation.bastion-siege-style-gate-blocks",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);

    let ship1_pos = DVec3::new(10_000_000.0, 0.0, 0.0);
    let mut s1 = setup_nav_pilot(&ctx, PILOT_1, SHIP_1, 28661, SOV_NULL_A, ship1_pos).await; // Kronos Marauder in NOL-M9
    let mut s2 = setup_nav_pilot(
        &ctx,
        PILOT_2,
        SHIP_2,
        11999,
        JITA_SYSTEM,
        ship1_pos + DVec3::new(250_000.0, 0.0, 0.0),
    )
    .await;

    let sov_a_sys = ctx.systems.get_or_spawn(SOV_NULL_A);
    let jita_sys = ctx.systems.get_or_spawn(JITA_SYSTEM);
    let kw = HashMap::new();

    // =========================================================================
    // 1. feat:navigation.ansiblex-jump-fatigue-exemption
    // =========================================================================
    let gate_a_id = 1_000_600_001i64;
    let gate_b1_id = 1_000_600_002i64;
    let gate_b2_id = 1_000_600_003i64;
    let gate_c_id = 1_000_600_004i64;
    let gate_far_id = 1_000_600_005i64;
    let gate_highsec_id = 1_000_600_006i64;
    let cargo_ozone_id = 91_600_000i64;
    let fuel_ozone_1a_id = 91_600_001i64;
    let fuel_ozone_1b_id = 91_600_011i64;
    let bastion_mod_id = 91_600_002i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(gate_a_id, make_structure(gate_a_id, 35841, SOV_NULL_A, "NOL-M9 Ansiblex", ship1_pos.to_array()));
        snap.structures.insert(gate_b1_id, make_structure(gate_b1_id, 35841, SOV_NULL_B, "O-IOAI Ansiblex A", [1_000.0, 0.0, 0.0]));
        snap.structures.insert(gate_b2_id, make_structure(gate_b2_id, 35841, SOV_NULL_B, "O-IOAI Ansiblex B", [2_500.0, 0.0, 0.0]));
        snap.structures.insert(gate_c_id, make_structure(gate_c_id, 35841, SOV_NULL_C, "R5-MM8 Ansiblex", [5_000.0, 0.0, 0.0]));
        snap.structures.insert(gate_far_id, make_structure(gate_far_id, 35841, SOV_NULL_FAR, "QY6-RK Far Ansiblex", [5_000.0, 0.0, 0.0]));
        snap.structures.insert(gate_highsec_id, make_structure(gate_highsec_id, 35841, JITA_SYSTEM, "Highsec Invalid Gate", [5_000.0, 0.0, 0.0]));
        let mut ext = load_ext_state(snap);
        ext.jump_bridge_links.insert(gate_a_id, gate_b1_id);
        ext.jump_bridge_links.insert(gate_b1_id, gate_a_id);
        ext.jump_bridge_links.insert(gate_b2_id, gate_c_id);
        ext.jump_bridge_links.insert(gate_c_id, gate_b2_id);
        save_ext_state(snap, &ext);

        // Non-fuel-bay Liquid Ozone (Cargo) MUST NOT count or be deducted
        snap.items.insert(
            cargo_ozone_id,
            InventoryItemRecord {
                item_id: cargo_ozone_id,
                type_id: 16273,
                owner_id: 1_000_001,
                location_id: gate_a_id,
                flag: InventoryFlag::Cargo,
                quantity: 10_000,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        // Fit Bastion Module I (type 33400) on Kronos (28661)
        snap.items.insert(
            bastion_mod_id,
            InventoryItemRecord {
                item_id: bastion_mod_id,
                type_id: 33400,
                owner_id: PILOT_1,
                location_id: SHIP_1,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    ctx.invalidate_ship_fitting(SHIP_1);

    // V24 GetLinkedStructure returns (linkedStructureID, linkedSolarSystemID) 2-tuple or None when unlinked
    let link_res = ctx
        .dispatch_rpc(&mut s1, "structureJumpBridgeMgr", None, "GetLinkedStructure", &[PyRep::Integer(gate_a_id)], &kw, None)
        .await;
    assert_eq!(link_res.expect("GetLinkedStructure"), PyRep::Tuple(vec![PyRep::Integer(gate_b1_id), PyRep::Integer(SOV_NULL_B as i64)]));
    let unlinked_res = ctx
        .dispatch_rpc(&mut s1, "structureJumpBridgeMgr", None, "GetLinkedStructure", &[PyRep::Integer(gate_far_id)], &kw, None)
        .await;
    assert_eq!(unlinked_res.expect("GetLinkedStructure unlinked"), PyRep::None);

    // Negative 0: Cargo Liquid Ozone alone fails with InsufficientLiquidOzone (must be StructureFuel)
    let no_fuel_res = ctx
        .dispatch_rpc(&mut s1, "structureJumpBridgeMgr", None, "CmdJumpThroughStructureStargate", &[PyRep::Integer(gate_a_id)], &kw, None)
        .await;
    assert_eq!(no_fuel_res.unwrap_err().code, "InsufficientLiquidOzone");

    // Seed StructureFuel stacks (first stack 30 units will be fully depleted & removed!)
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            fuel_ozone_1a_id,
            InventoryItemRecord {
                item_id: fuel_ozone_1a_id,
                type_id: 16273,
                owner_id: 1_000_001,
                location_id: gate_a_id,
                flag: InventoryFlag::StructureFuel,
                quantity: 30,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            fuel_ozone_1b_id,
            InventoryItemRecord {
                item_id: fuel_ozone_1b_id,
                type_id: 16273,
                owner_id: 1_000_001,
                location_id: gate_a_id,
                flag: InventoryFlag::StructureFuel,
                quantity: 50_000,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    // Negative 0b: Link > 5.0 LY (NOL-M9 -> QY6-RK ~6.36 LY) and non-sov-null (Jita) rejected
    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        ext.jump_bridge_links.insert(gate_a_id, gate_far_id);
        save_ext_state(snap, &ext);
    });
    assert_eq!(
        ctx.dispatch_rpc(&mut s1, "structureJumpBridgeMgr", None, "CmdJumpThroughStructureStargate", &[PyRep::Integer(gate_a_id)], &kw, None).await.unwrap_err().code,
        "JumpBridgeOutOfRange"
    );
    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        ext.jump_bridge_links.insert(gate_a_id, gate_highsec_id);
        save_ext_state(snap, &ext);
    });
    assert_eq!(
        ctx.dispatch_rpc(&mut s1, "structureJumpBridgeMgr", None, "CmdJumpThroughStructureStargate", &[PyRep::Integer(gate_a_id)], &kw, None).await.unwrap_err().code,
        "JumpBridgeRequiresSovNull"
    );
    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        ext.jump_bridge_links.insert(gate_a_id, gate_b1_id);
        save_ext_state(snap, &ext);
    });

    // Negative 1: Offline / unpowered Ansiblex refused (`fuel_blocks == 0`)
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.get_mut(&gate_a_id).unwrap().fuel_blocks = 0;
    });
    let unpowered_res = ctx
        .dispatch_rpc(&mut s1, "structureJumpBridgeMgr", None, "CmdJumpThroughStructureStargate", &[PyRep::Integer(gate_a_id)], &kw, None)
        .await;
    assert_eq!(unpowered_res.unwrap_err().code, "StructureNotOperational");
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.get_mut(&gate_a_id).unwrap().fuel_blocks = 1000;
    });

    // Negative 2: Weapons timer active refused
    let now = ctx.now_unix();
    ctx.crimewatch.write().unwrap().states.entry(PILOT_1).or_default().weapon_timer_until_unix = now + 60;
    let wt_res = ctx
        .dispatch_rpc(&mut s1, "structureJumpBridgeMgr", None, "CmdJumpThroughStructureStargate", &[PyRep::Integer(gate_a_id)], &kw, None)
        .await;
    assert_eq!(wt_res.unwrap_err().code, "WeaponsTimerActive");
    ctx.crimewatch.write().unwrap().states.entry(PILOT_1).or_default().weapon_timer_until_unix = 0;

    // Negative 3 (Clause 5): Active Bastion Module I via live dogmaIM.Activate refuses Ansiblex jump!
    let act_bastion_jb = ctx
        .dispatch_rpc(&mut s1, "dogmaIM", None, "Activate", &[PyRep::Integer(bastion_mod_id)], &kw, None)
        .await;
    assert!(act_bastion_jb.is_ok(), "Bastion activate via dogmaIM.Activate: {act_bastion_jb:?}");
    let bastion_jb_res = ctx
        .dispatch_rpc(&mut s1, "structureJumpBridgeMgr", None, "CmdJumpThroughStructureStargate", &[PyRep::Integer(gate_a_id)], &kw, None)
        .await;
    assert_eq!(bastion_jb_res.unwrap_err().code, "CantJumpWhileModuleActive");
    ctx.dispatch_rpc(&mut s1, "dogmaIM", None, "Deactivate", &[PyRep::Integer(bastion_mod_id)], &kw, None)
        .await
        .expect("Deactivate Bastion before Ansiblex jump");
    ctx.advance_secs(65); // Clear any module reactivation cooldown

    // Negative 4: Supercarrier & > 1.48 Mt refused
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&SHIP_1).unwrap().type_id = 23919; // Aeon Supercarrier
    });
    assert_eq!(
        ctx.dispatch_rpc(&mut s1, "structureJumpBridgeMgr", None, "CmdJumpThroughStructureStargate", &[PyRep::Integer(gate_a_id)], &kw, None)
            .await
            .unwrap_err()
            .code,
        "ShipNotAllowedThroughAnsiblex"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&SHIP_1).unwrap().type_id = 28661;
    });
    if let Ok(Some((_, mut b))) = sov_a_sys.get_ball_snapshot(SHIP_1).await {
        b.mass = 1.50e9;
        let _ = sov_a_sys.spawn_ball(b).await;
    }
    assert_eq!(
        ctx.dispatch_rpc(&mut s1, "structureJumpBridgeMgr", None, "CmdJumpThroughStructureStargate", &[PyRep::Integer(gate_a_id)], &kw, None)
            .await
            .unwrap_err()
            .code,
        "ShipNotAllowedThroughAnsiblex"
    );
    if let Ok(Some((_, mut b))) = sov_a_sys.get_ball_snapshot(SHIP_1).await {
        b.mass = 10_000_000.0;
        let _ = sov_a_sys.spawn_ball(b).await;
    }

    let now = ctx.now_unix();
    let initial_fatigue_mins = 20.0;
    let initial_cooldown = now + 600;
    let initial_fatigue_until = now + 1200;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_1) {
            ch.jump_fatigue_minutes = initial_fatigue_mins;
            ch.jump_cooldown_until_unix = initial_cooldown;
            ch.jump_fatigue_until_unix = initial_fatigue_until;
        }
    });

    // Positive 1: Jump 1 in Delve sov null-sec (NOL-M9 -> O-IOAI, ~1.10 LY <= 5.0 LY, mass 10 Mt)
    let dist_1_ly = eve_server::services::space_destiny::space_nav_travel::distance_ly(&ctx, SOV_NULL_A, SOV_NULL_B).max(0.01);
    let ship1_mass = 10_000_000.0;
    let exp_cost_1 = ((ship1_mass * dist_1_ly * 0.000003 + 50.0).round() as i32).max(50);

    let jump_res = ctx
        .dispatch_rpc(&mut s1, "structureJumpBridgeMgr", None, "CmdJumpThroughStructureStargate", &[PyRep::Integer(gate_a_id)], &kw, None)
        .await;
    assert!(jump_res.is_ok(), "Ansiblex jump 1 succeeded: {jump_res:?}");
    assert_eq!(s1.solar_system_id, Some(SOV_NULL_B));

    let (cargo_unchanged, depleted_removed, ozone_left_1b) = ctx.db.with_snapshot(|snap| {
        (
            snap.items.get(&cargo_ozone_id).map(|i| i.quantity),
            !snap.items.contains_key(&fuel_ozone_1a_id),
            snap.items.get(&fuel_ozone_1b_id).map(|i| i.quantity).unwrap_or(0),
        )
    });
    assert_eq!(cargo_unchanged, Some(10_000), "Cargo Liquid Ozone must never be consumed");
    assert!(depleted_removed, "Depleted 30-unit StructureFuel stack must be removed");
    assert_eq!(50_030 - ozone_left_1b, exp_cost_1, "Exact StructureFuel ozone consumed for dist {dist_1_ly:.2} LY");

    // Verify 60s gate cloak applied in destination system
    let sov_b_sys = ctx.systems.get_or_spawn(SOV_NULL_B);
    let cs_arrived = sov_b_sys.get_combat_state(SHIP_1).await.unwrap().unwrap();
    assert!(cs_arrived.is_cloaked && cs_arrived.gate_cloak_remaining_ticks == 60, "60s gate cloak applied on Ansiblex arrival");

    // Positive 2: Jump 2 across a genuinely distinct LY pair in Delve sov null-sec (O-IOAI -> R5-MM8, ~3.85 LY <= 5.0 LY, mass 100 Mt)
    let dist_2_ly = eve_server::services::space_destiny::space_nav_travel::distance_ly(&ctx, SOV_NULL_B, SOV_NULL_C).max(0.01);
    assert!((dist_2_ly - dist_1_ly).abs() > 1.5, "Jump 2 must test a genuinely distinct LY distance ({dist_1_ly:.2} vs {dist_2_ly:.2})");
    let ozone_item_id_2 = 91_600_005i64;
    let ship2_mass = 100_000_000.0;
    let exp_cost_2 = ((ship2_mass * dist_2_ly * 0.000003 + 50.0).round() as i32).max(50);
    assert_ne!(exp_cost_1, exp_cost_2);

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ozone_item_id_2,
            InventoryItemRecord {
                item_id: ozone_item_id_2,
                type_id: 16273,
                owner_id: 1_000_001,
                location_id: gate_b2_id,
                flag: InventoryFlag::StructureFuel,
                quantity: 50_000,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.get_mut(&SHIP_1).unwrap().type_id = 645;
    });
    let _ = sov_b_sys
        .spawn_ball(eve_destiny::Ball {
            id: SHIP_1,
            type_id: 645,
            position: DVec3::new(2_500.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 150.0,
            inertia: 1.5,
            mass: ship2_mass,
            radius: 150.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        })
        .await;
    eve_server::services::space_destiny::clear_session_change_timer(&ctx, PILOT_1);

    let jump_res_2 = ctx
        .dispatch_rpc(&mut s1, "structureJumpBridgeMgr", None, "CmdJumpThroughStructureStargate", &[PyRep::Integer(gate_b2_id)], &kw, None)
        .await;
    assert!(jump_res_2.is_ok(), "Ansiblex jump 2 succeeded: {jump_res_2:?}");
    assert_eq!(s1.solar_system_id, Some(SOV_NULL_C));
    let ozone_left_2 = ctx.db.with_snapshot(|snap| snap.items.get(&ozone_item_id_2).map(|it| it.quantity).unwrap_or(0));
    assert_eq!(50_000 - ozone_left_2, exp_cost_2, "Exact StructureFuel ozone consumed for dist {dist_2_ly:.2} LY");

    let (fatigue_2, cd_2, fu_2) = ctx.db.with_snapshot(|snap| {
        snap.characters
            .get(&PILOT_1)
            .map(|c| (c.jump_fatigue_minutes, c.jump_cooldown_until_unix, c.jump_fatigue_until_unix))
            .unwrap_or((0.0, 0, 0))
    });
    assert_eq!((fatigue_2, cd_2, fu_2), (initial_fatigue_mins, initial_cooldown, initial_fatigue_until));

    // =========================================================================
    // 2. feat:navigation.bastion-siege-style-gate-blocks
    // =========================================================================
    let client_dialogs = ctx.sde.load_dialogs().expect("load V24.01 dialogs.static");
    assert_eq!(client_dialogs["CantDockWhileModuleActive"].dialog_id, 2094);
    assert_eq!(client_dialogs["CantJumpWhileModuleActive"].dialog_id, 2095);
    assert_eq!(client_dialogs["WarpDisrupted"].dialog_id, 1680);
    assert_eq!(client_dialogs["DeniedTethered"].dialog_id, 4899);

    let station_id = 60_000_361i64;
    let station_pos = DVec3::new(-107_072_593_920.0, -18_704_916_480.0, 436_343_562_240.0);
    let astrahus_id = 1_000_092_001i64;
    s1.solar_system_id = Some(JITA_SYSTEM);
    s1.station_id = None;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_1) {
            ch.solar_system_id = JITA_SYSTEM;
            ch.station_id = None;
            ch.jump_cooldown_until_unix = 0;
        }
        snap.items.get_mut(&SHIP_1).unwrap().type_id = 28661;
        snap.items.get_mut(&SHIP_1).unwrap().location_id = JITA_SYSTEM as i64;
        snap.item_positions.insert(SHIP_1, (station_pos + DVec3::new(500.0, 0.0, 0.0)).to_array());
        snap.structures.insert(
            astrahus_id,
            make_structure(astrahus_id, 35832, JITA_SYSTEM, "Jita Tether Astrahus", (station_pos + DVec3::new(1000.0, 0.0, 0.0)).to_array()),
        );
    });
    let fleet_id = ctx
        .dispatch_rpc(&mut s1, "fleetObjectHandler", None, "CreateFleet", &[], &kw, None)
        .await
        .expect("CreateFleet")
        .as_i64()
        .expect("fleetID");
    s1.fleet_id = Some(fleet_id);
    ctx.dispatch_rpc(
        &mut s1,
        "fleetObjectHandler",
        None,
        "Invite",
        &[PyRep::Integer(PILOT_2), PyRep::Integer(1), PyRep::Integer(1), PyRep::Integer(4)],
        &kw,
        None,
    )
    .await
    .expect("Invite PILOT_2");
    ctx.dispatch_rpc(&mut s2, "fleetObjectHandler", None, "AcceptInvite", &[PyRep::Integer(fleet_id)], &kw, None)
        .await
        .expect("AcceptInvite PILOT_2");
    s2.fleet_id = Some(fleet_id);
    ctx.invalidate_ship_fitting(SHIP_1);
    let _ = jita_sys
        .spawn_ball_with_combat(
            eve_destiny::Ball {
                id: SHIP_1,
                type_id: 28661,
                position: station_pos + DVec3::new(500.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 2.0,
                mass: 10_000_000.0,
                radius: 120.0,
                speed_fraction: 0.0,
                mode: eve_destiny::MovementMode::Stop,
            },
            ctx.build_ship_combat_state(PILOT_1, SHIP_1, 28661),
        )
        .await;
    let _ = jita_sys
        .spawn_ball(eve_destiny::Ball {
            id: SHIP_2,
            type_id: 11999,
            position: station_pos + DVec3::new(300_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 2.0,
            mass: 10_000_000.0,
            radius: 120.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        })
        .await;

    // Activate Bastion Module I via live dogmaIM.Activate RPC
    let act_bastion = ctx
        .dispatch_rpc(&mut s1, "dogmaIM", None, "Activate", &[PyRep::Integer(bastion_mod_id)], &kw, None)
        .await;
    assert!(act_bastion.is_ok(), "Live dogmaIM.Activate on Bastion Module I: {act_bastion:?}");

    let bid = ctx.bind_object(&mut s1, "beyonce", JITA_SYSTEM as i64).expect("Bind beyonce");
    eve_server::services::space_destiny::clear_session_change_timer(&ctx, PILOT_1);

    // Negative 1: V24 beyonce.CmdDock, structureDocking.Dock & CmdTurboDock refused (`CantDockWhileModuleActive`, 2094)
    assert_eq!(
        ctx.dispatch_rpc(&mut s1, "beyonce", Some(bid), "CmdDock", &[PyRep::Integer(station_id), PyRep::Integer(SHIP_1)], &kw, None).await.unwrap_err().code,
        "CantDockWhileModuleActive"
    );
    assert_eq!(
        ctx.dispatch_rpc(&mut s1, "structureDocking", None, "Dock", &[PyRep::Integer(astrahus_id), PyRep::Integer(SHIP_1)], &kw, None).await.unwrap_err().code,
        "CantDockWhileModuleActive"
    );
    s1.role = eve_db::service_roles::ROLE_GM_DEFAULT;
    assert_eq!(
        ctx.dispatch_rpc(&mut s1, "beyonce", Some(bid), "CmdTurboDock", &[PyRep::Integer(station_id)], &kw, None).await.unwrap_err().code,
        "CantDockWhileModuleActive"
    );
    s1.role = 0;

    // Negative 2: Stargate jump refused (`CantJumpWhileModuleActive`, 2095)
    assert_eq!(
        ctx.dispatch_rpc(&mut s1, "beyonce", Some(bid), "CmdStargateJump", &[PyRep::Integer(50013876)], &kw, None).await.unwrap_err().code,
        "CantJumpWhileModuleActive"
    );

    // Negative 3: CmdWarpToStuff, WarpToMember & FleetWarp refused/skipped while Bastion active
    assert_eq!(
        ctx.dispatch_rpc(&mut s1, "beyonce", Some(bid), "CmdWarpToStuff", &[PyRep::String("station".into()), PyRep::Integer(station_id), PyRep::Real(0.0)], &kw, None).await.unwrap_err().code,
        "WarpDisrupted"
    );
    assert_eq!(
        ctx.dispatch_rpc(&mut s1, "fleetMgr", None, "WarpToMember", &[PyRep::Integer(PILOT_2), PyRep::Real(0.0)], &kw, None).await.unwrap_err().code,
        "WarpDisrupted"
    );
    let fw_res = ctx
        .dispatch_rpc(&mut s1, "fleetObjectHandler", None, "FleetWarp", &[PyRep::Real(station_pos.x + 1_000_000.0), PyRep::Real(station_pos.y), PyRep::Real(station_pos.z), PyRep::Real(0.0)], &kw, None)
        .await
        .expect("FleetWarp skips Bastion ship");
    let (_, b1_after_fw) = jita_sys.get_ball_snapshot(SHIP_1).await.unwrap().unwrap();
    let (_, b2_after_fw) = jita_sys.get_ball_snapshot(SHIP_2).await.unwrap().unwrap();
    assert!(!matches!(b1_after_fw.mode, eve_destiny::MovementMode::Warp { .. }), "Bastion ship must not enter warp on FleetWarp");
    assert!(matches!(b2_after_fw.mode, eve_destiny::MovementMode::Warp { .. }), "Non-Bastion fleetmate must enter warp on FleetWarp");
    let _ = fw_res;

    // Negative 4: Tethering refused via SDE attributes (`DeniedTethered`, 4899) even with no weapons timer
    ctx.advance_secs(61);
    eve_server::services::space_destiny::space_nav_travel::sync_tether(&ctx, PILOT_1, JITA_SYSTEM, SHIP_1).await;
    assert!(!jita_sys.get_combat_state(SHIP_1).await.unwrap().unwrap().is_tethered, "Tether refused while Bastion active");
    assert_eq!(
        eve_server::services::space_navigation_advanced::check_immobilizing_module_active(&ctx, SHIP_1, "tether").unwrap_err().code,
        "DeniedTethered"
    );

    // Positive: Deactivate Bastion via dogmaIM.Deactivate -> tether, V24 CmdDock, and warp succeed!
    let deact_bastion = ctx
        .dispatch_rpc(&mut s1, "dogmaIM", None, "Deactivate", &[PyRep::Integer(bastion_mod_id)], &kw, None)
        .await;
    assert!(deact_bastion.is_ok(), "Bastion deactivation succeeds: {deact_bastion:?}");
    assert!(jita_sys.get_combat_state(SHIP_1).await.unwrap().unwrap().is_tethered, "Tether automatically synced on Deactivate");

    eve_server::services::space_destiny::clear_session_change_timer(&ctx, PILOT_1);
    let dock_ok = ctx
        .dispatch_rpc(&mut s1, "beyonce", Some(bid), "CmdDock", &[PyRep::Integer(station_id), PyRep::Integer(SHIP_1)], &kw, None)
        .await;
    assert!(dock_ok.is_ok(), "V24 CmdDock succeeds once Bastion deactivates: {dock_ok:?}");
    assert_eq!(s1.station_id, Some(station_id));

    s1.station_id = None;
    let bid = ctx.bind_object(&s1, "beyonce", JITA_SYSTEM as i64).unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_1) {
            ch.station_id = None;
        }
    });
    let _ = jita_sys
        .spawn_ball(eve_destiny::Ball {
            id: SHIP_1,
            type_id: 28661,
            position: station_pos + DVec3::new(500.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 2.0,
            mass: 10_000_000.0,
            radius: 120.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        })
        .await;

    let warp_ok = ctx
        .dispatch_rpc(
            &mut s1,
            "beyonce",
            Some(bid),
            "CmdWarpToStuff",
            &[PyRep::String("stargate".into()), PyRep::Integer(50013876), PyRep::Real(0.0)],
            &kw,
            None,
        )
        .await;
    assert!(warp_ok.is_ok(), "Warp succeeds once Bastion deactivates: {warp_ok:?}");
}

pub(crate) mod space_navigation_cloak_targeting_delay {
    // Integration tests for Space Navigation: Cloak targeting delay, live fitting transitions,
    // unfitting decloak safety, and hull/subsystem delay modifiers (`feat:navigation.cloak-targeting-delay`).


    use crate::navigation_jump_cloak_zarzakh::setup_nav_pilot;
    use eve_db::{InventoryFlag, InventoryItemRecord, TrainedSkillRecord};
    use eve_proto::PyRep;
    use eve_server::router::ServerContext;
    use glam::DVec3;
    use std::collections::HashMap;

    const JITA_SYSTEM: u32 = 30_000_142;
    const PILOT_1: i64 = 93_200_001;
    const PILOT_2: i64 = 93_200_002;
    const SHIP_1: i64 = 83_200_001;
    const SHIP_2: i64 = 83_200_002;

    // parity: feat:navigation.cloak-targeting-delay
    #[tokio::test]
    async fn test_space_navigation_cloak_targeting_delay_and_overrides() {
        eve_testkit::attest(&["feat:navigation.cloak-targeting-delay"]);

        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);

        let origin_pos = DVec3::new(15_000_000.0, 0.0, 0.0);
        let mut s1 = setup_nav_pilot(&ctx, PILOT_1, SHIP_1, 28661, JITA_SYSTEM, origin_pos).await; // Kronos
        let _s2 = setup_nav_pilot(
            &ctx,
            PILOT_2,
            SHIP_2,
            11999,
            JITA_SYSTEM,
            origin_pos + DVec3::new(10_000.0, 0.0, 0.0),
        )
        .await;

        let jita_sys = ctx.systems.get_or_spawn(JITA_SYSTEM);
        let kw = HashMap::new();

        // =========================================================================
        // 1. Kronos (28661) + Prototype Cloaking Device I (11370, 30s) + Cloaking II (-20% -> 24.0s)
        //    Verified via direct dogmaIM.Activate -> dogmaIM.Deactivate WITHOUT UpdateStateRequest,
        //    followed by ticking past 24s so AddTarget succeeds!
        // =========================================================================
        let cloak_item_id = 92_600_001i64;
        ctx.db.with_mut_snapshot(|snap| {
            snap.skills.entry(PILOT_1).or_default().insert(
                11579,
                TrainedSkillRecord {
                    type_id: 11579,
                    level: 2,
                    skill_points: 8_000.0,
                },
            );
            snap.items.insert(
                cloak_item_id,
                InventoryItemRecord {
                    item_id: cloak_item_id,
                    type_id: 11370, // Prototype Cloaking Device I (SDE attr 560 = 30000ms)
                    owner_id: PILOT_1,
                    location_id: SHIP_1,
                    flag: InventoryFlag::HiSlot1,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
        });
        ctx.invalidate_ship_fitting(SHIP_1);

        // Activate cloak directly via dogmaIM.Activate (no beyonce.UpdateStateRequest!)
        let act_res = ctx
            .dispatch_rpc(
                &mut s1,
                "dogmaIM",
                None,
                "Activate",
                &[PyRep::Integer(cloak_item_id)],
                &kw,
                None,
            )
            .await;
        assert!(act_res.is_ok(), "Cloak activate: {act_res:?}");
        let cs_cloaked = jita_sys.get_combat_state(SHIP_1).await.unwrap().unwrap();
        assert!(
            cs_cloaked.is_cloaked,
            "CombatState.is_cloaked must sync directly on dogmaIM.Activate"
        );
        assert!(
            (cs_cloaked.cloak_configured_targeting_delay_s - 24.0).abs() < 0.1,
            "Configured targeting delay must be 24.0s on Kronos with Cloaking II"
        );

        // AddTargetOBO must also fail while cloaked WITHOUT stripping undock/location-change invulnerability (`R-CLOAK-OBO-01`)
        {
            let mut cs_inv = jita_sys.get_combat_state(SHIP_1).await.unwrap().unwrap();
            cs_inv.undock_invulnerable_remaining_ticks = 30;
            jita_sys.set_combat_state(SHIP_1, cs_inv).await.unwrap();
        }
        let obo_cloaked = ctx
            .dispatch_rpc(
                &mut s1,
                "dogmaIM",
                None,
                "AddTargetOBO",
                &[PyRep::Integer(SHIP_1), PyRep::Integer(SHIP_2)],
                &kw,
                None,
            )
            .await;
        assert_eq!(obo_cloaked.unwrap_err().code, "TargetingFailed");
        assert_eq!(
            jita_sys
                .get_combat_state(SHIP_1)
                .await
                .unwrap()
                .unwrap()
                .undock_invulnerable_remaining_ticks,
            30,
            "Rejected AddTargetOBO while cloaked must not clear undock_invulnerable_remaining_ticks"
        );
        assert!(jita_sys.get_target_locks(SHIP_1).await.unwrap().is_empty());

        // Deactivate cloak directly via dogmaIM.Deactivate
        let deact_res = ctx
            .dispatch_rpc(
                &mut s1,
                "dogmaIM",
                None,
                "Deactivate",
                &[PyRep::Integer(cloak_item_id)],
                &kw,
                None,
            )
            .await;
        assert!(deact_res.is_ok(), "Cloak deactivate: {deact_res:?}");
        let cs_decloaked = jita_sys.get_combat_state(SHIP_1).await.unwrap().unwrap();
        assert!(!cs_decloaked.is_cloaked, "Ship is decloaked");
        assert!(
            (cs_decloaked.decloak_targeting_delay_remaining_s - 24.0).abs() < 0.1,
            "Decloak targeting delay must be 24.0s: got {:.2}s",
            cs_decloaked.decloak_targeting_delay_remaining_s
        );

        // Immediate re-cloak blocked by module reactivation delay (`P2-03`)
        let recloak_err = ctx
            .dispatch_rpc(
                &mut s1,
                "dogmaIM",
                None,
                "Activate",
                &[PyRep::Integer(cloak_item_id)],
                &kw,
                None,
            )
            .await;
        assert_eq!(recloak_err.unwrap_err().code, "ModuleReactivationDelay");

        // Target lock blocked while sensor recalibration delay is active (`AddTarget` and `AddTargetOBO`)
        let lock_blocked = ctx
            .dispatch_rpc(
                &mut s1,
                "dogmaIM",
                None,
                "AddTarget",
                &[PyRep::Integer(SHIP_2)],
                &kw,
                None,
            )
            .await;
        assert_eq!(lock_blocked.unwrap_err().code, "TargetingFailed");
        let obo_recal = ctx
            .dispatch_rpc(
                &mut s1,
                "dogmaIM",
                None,
                "AddTargetOBO",
                &[PyRep::Integer(SHIP_1), PyRep::Integer(SHIP_2)],
                &kw,
                None,
            )
            .await;
        assert_eq!(obo_recal.unwrap_err().code, "TargetingFailed");
        assert_eq!(
            jita_sys
                .get_combat_state(SHIP_1)
                .await
                .unwrap()
                .unwrap()
                .undock_invulnerable_remaining_ticks,
            30,
            "Rejected AddTarget/AddTargetOBO during sensor recalibration must not clear undock_invulnerable_remaining_ticks"
        );
        assert!(jita_sys.get_target_locks(SHIP_1).await.unwrap().is_empty());
        {
            let mut cs_inv = jita_sys.get_combat_state(SHIP_1).await.unwrap().unwrap();
            cs_inv.undock_invulnerable_remaining_ticks = 0;
            jita_sys.set_combat_state(SHIP_1, cs_inv).await.unwrap();
        }

        // Tick 23s: 1.0s remaining -> AddTarget still blocked
        for _ in 0..23 {
            jita_sys.step_manual(1.0).await.unwrap();
        }
        assert!(
            jita_sys
                .get_combat_state(SHIP_1)
                .await
                .unwrap()
                .unwrap()
                .decloak_targeting_delay_remaining_s
                > 0.5
        );
        assert_eq!(
            ctx.dispatch_rpc(
                &mut s1,
                "dogmaIM",
                None,
                "AddTarget",
                &[PyRep::Integer(SHIP_2)],
                &kw,
                None
            )
            .await
            .unwrap_err()
            .code,
            "TargetingFailed"
        );

        // Tick 2 more seconds (25s total > 24s): recalibration timer expires -> AddTarget succeeds!
        jita_sys.step_manual(1.0).await.unwrap();
        jita_sys.step_manual(1.0).await.unwrap();
        assert_eq!(
            jita_sys
                .get_combat_state(SHIP_1)
                .await
                .unwrap()
                .unwrap()
                .decloak_targeting_delay_remaining_s,
            0.0
        );
        let lock_ok = ctx
            .dispatch_rpc(
                &mut s1,
                "dogmaIM",
                None,
                "AddTarget",
                &[PyRep::Integer(SHIP_2)],
                &kw,
                None,
            )
            .await;
        assert!(
            lock_ok.is_ok(),
            "AddTarget must succeed once Kronos 24s decloak recalibration timer expires: {lock_ok:?}"
        );
        let _ = ctx
            .dispatch_rpc(
                &mut s1,
                "dogmaIM",
                None,
                "RemoveTarget",
                &[PyRep::Integer(SHIP_2)],
                &kw,
                None,
            )
            .await;

        // =========================================================================
        // 2. Unfitting an active cloak (`invbroker.Add` to Cargo) and `TakeModuleOffline`
        //    must immediately clear `is_cloaked` (no permacloak) and start recalibration delay!
        // =========================================================================
        ctx.advance_secs(35);
        ctx.dispatch_rpc(
            &mut s1,
            "dogmaIM",
            None,
            "Activate",
            &[PyRep::Integer(cloak_item_id)],
            &kw,
            None,
        )
        .await
        .expect("Re-activate cloak after reactivation delay");
        assert!(jita_sys.get_combat_state(SHIP_1).await.unwrap().unwrap().is_cloaked);

        // Unfit active cloak into ship Cargo via invbroker.Add while within 2,500m of a Mobile Depot
        let depot_id = 92_600_099i64;
        let (_, ship1_ball) = jita_sys.get_ball_snapshot(SHIP_1).await.unwrap().unwrap();
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                depot_id,
                InventoryItemRecord {
                    item_id: depot_id,
                    type_id: 33474, // Mobile Depot (group 1246)
                    owner_id: PILOT_1,
                    location_id: JITA_SYSTEM as i64,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
        });
        let _ = jita_sys
            .spawn_ball(eve_destiny::Ball {
                id: depot_id,
                type_id: 33474,
                position: ship1_ball.position + DVec3::new(2_500.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 1.0,
                mass: 100_000.0,
                radius: 75.0,
                speed_fraction: 0.0,
                mode: eve_destiny::MovementMode::Stop,
            })
            .await;
        let ship_inv_bid = ctx
            .bind_object(&mut s1, "invbroker", SHIP_1)
            .expect("Bind ship invbroker");
        let unfit_res = ctx
            .dispatch_rpc(
                &mut s1,
                "invbroker",
                Some(ship_inv_bid),
                "Add",
                &[
                    PyRep::Integer(cloak_item_id),
                    PyRep::Integer(SHIP_1),
                    PyRep::Integer(InventoryFlag::Cargo as i64),
                ],
                &kw,
                None,
            )
            .await;
        assert!(unfit_res.is_ok(), "Unfit active cloak to cargo: {unfit_res:?}");
        let cs_after_unfit = jita_sys.get_combat_state(SHIP_1).await.unwrap().unwrap();
        assert!(
            !cs_after_unfit.is_cloaked,
            "Unfitting an active cloak must immediately clear is_cloaked"
        );
        assert!(
            (cs_after_unfit.decloak_targeting_delay_remaining_s - 24.0).abs() < 0.1,
            "Unfitting an active cloak must start the 24.0s recalibration delay"
        );
        for _ in 0..25 {
            jita_sys.step_manual(1.0).await.unwrap();
        }

        // Also verify `invbroker.MultiAdd` unfitting an active cloak in space at 2,500m from Mobile Depot (`R-CLOAK-UNFIT-01`)
        ctx.dispatch_rpc(
            &mut s1,
            "invbroker",
            Some(ship_inv_bid),
            "Add",
            &[
                PyRep::Integer(cloak_item_id),
                PyRep::Integer(SHIP_1),
                PyRep::Integer(InventoryFlag::HiSlot1 as i64),
            ],
            &kw,
            None,
        )
        .await
        .expect("Re-fit cloak into HiSlot1 near Mobile Depot");
        ctx.advance_secs(35);
        ctx.dispatch_rpc(
            &mut s1,
            "dogmaIM",
            None,
            "Activate",
            &[PyRep::Integer(cloak_item_id)],
            &kw,
            None,
        )
        .await
        .expect("Activate re-fitted cloak");
        assert!(jita_sys.get_combat_state(SHIP_1).await.unwrap().unwrap().is_cloaked);
        let multi_unfit = ctx
            .dispatch_rpc(
                &mut s1,
                "invbroker",
                Some(ship_inv_bid),
                "MultiAdd",
                &[
                    PyRep::List(vec![PyRep::Integer(cloak_item_id)]),
                    PyRep::Integer(SHIP_1),
                    PyRep::Integer(InventoryFlag::Cargo as i64),
                ],
                &kw,
                None,
            )
            .await;
        assert!(multi_unfit.is_ok(), "MultiAdd unfit active cloak to cargo: {multi_unfit:?}");
        let cs_after_multi = jita_sys.get_combat_state(SHIP_1).await.unwrap().unwrap();
        assert!(
            !cs_after_multi.is_cloaked,
            "MultiAdd unfitting an active cloak must immediately clear is_cloaked"
        );
        assert!(
            (cs_after_multi.decloak_targeting_delay_remaining_s - 24.0).abs() < 0.1,
            "MultiAdd unfitting an active cloak must start the 24.0s recalibration delay, got {:.2}s",
            cs_after_multi.decloak_targeting_delay_remaining_s
        );
        let _ = jita_sys.remove_ball(depot_id).await;
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.remove(&depot_id);
        });
        for _ in 0..25 {
            jita_sys.step_manual(1.0).await.unwrap();
        }

        // =========================================================================
        // 3. Stealth Bomber (Nemesis 11377 -> 0s) & Covert Ops Frigate (Anathema 11188 -> 0s)
        // =========================================================================
        let bomber_cloak_id = 92_600_010i64;
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.remove(&cloak_item_id);
            snap.items.get_mut(&SHIP_1).unwrap().type_id = 11377; // Nemesis (group 834, 1035 = 0.0)
            snap.items.insert(
                bomber_cloak_id,
                InventoryItemRecord {
                    item_id: bomber_cloak_id,
                    type_id: 11370,
                    owner_id: PILOT_1,
                    location_id: SHIP_1,
                    flag: InventoryFlag::HiSlot1,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
        });
        ctx.invalidate_ship_fitting(SHIP_1);
        let _ = jita_sys
            .spawn_ball_with_combat(
                eve_destiny::Ball {
                    id: SHIP_1,
                    type_id: 11377,
                    position: origin_pos,
                    velocity: DVec3::ZERO,
                    max_velocity: 300.0,
                    inertia: 2.0,
                    mass: 1_200_000.0,
                    radius: 50.0,
                    speed_fraction: 0.0,
                    mode: eve_destiny::MovementMode::Stop,
                },
                ctx.build_ship_combat_state(PILOT_1, SHIP_1, 11377),
            )
            .await;

        ctx.dispatch_rpc(&mut s1, "dogmaIM", None, "Activate", &[PyRep::Integer(bomber_cloak_id)], &kw, None)
            .await
            .expect("Bomber cloak activate");
        assert!(jita_sys.get_combat_state(SHIP_1).await.unwrap().unwrap().is_cloaked);
        ctx.dispatch_rpc(&mut s1, "dogmaIM", None, "Deactivate", &[PyRep::Integer(bomber_cloak_id)], &kw, None)
            .await
            .expect("Bomber cloak deactivate");
        assert_eq!(
            jita_sys.get_combat_state(SHIP_1).await.unwrap().unwrap().decloak_targeting_delay_remaining_s,
            0.0
        );
        assert!(ctx.dispatch_rpc(&mut s1, "dogmaIM", None, "AddTarget", &[PyRep::Integer(SHIP_2)], &kw, None).await.is_ok());
        let _ = ctx.dispatch_rpc(&mut s1, "dogmaIM", None, "RemoveTarget", &[PyRep::Integer(SHIP_2)], &kw, None).await;

        // Covert Ops Frigate (Anathema 11188 + Covert Ops Cloaking Device II 11578 -> 0s)
        let covert_cloak_id = 92_600_004i64;
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.remove(&bomber_cloak_id);
            snap.skills.entry(PILOT_1).or_default().insert(
                11579,
                TrainedSkillRecord {
                    type_id: 11579,
                    level: 5,
                    skill_points: 256_000.0,
                },
            );
            snap.items.get_mut(&SHIP_1).unwrap().type_id = 11188; // Anathema
            snap.items.insert(
                covert_cloak_id,
                InventoryItemRecord {
                    item_id: covert_cloak_id,
                    type_id: 11578,
                    owner_id: PILOT_1,
                    location_id: SHIP_1,
                    flag: InventoryFlag::HiSlot1,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
        });
        ctx.invalidate_ship_fitting(SHIP_1);
        let _ = jita_sys
            .spawn_ball_with_combat(
                eve_destiny::Ball {
                    id: SHIP_1,
                    type_id: 11188,
                    position: origin_pos,
                    velocity: DVec3::ZERO,
                    max_velocity: 350.0,
                    inertia: 2.0,
                    mass: 1_100_000.0,
                    radius: 40.0,
                    speed_fraction: 0.0,
                    mode: eve_destiny::MovementMode::Stop,
                },
                ctx.build_ship_combat_state(PILOT_1, SHIP_1, 11188),
            )
            .await;

        ctx.dispatch_rpc(&mut s1, "dogmaIM", None, "Activate", &[PyRep::Integer(covert_cloak_id)], &kw, None)
            .await
            .expect("Covert Ops cloak activate");
        assert!(jita_sys.get_combat_state(SHIP_1).await.unwrap().unwrap().is_cloaked);
        ctx.dispatch_rpc(&mut s1, "dogmaIM", None, "Deactivate", &[PyRep::Integer(covert_cloak_id)], &kw, None)
            .await
            .expect("Covert Ops cloak deactivate");
        assert_eq!(
            jita_sys.get_combat_state(SHIP_1).await.unwrap().unwrap().decloak_targeting_delay_remaining_s,
            0.0
        );

        // =========================================================================
        // 4. Non-zero `covertOpsStealthBomberTargettingDelay` (`1035 = 5000.0` ms -> 5.0s)
        //    on Black Ops (Redeemer 22428 override) / Endurance (37135) AND T3 Strategic Cruiser
        //    (Legion 29986 + Covert Reconfiguration 45586 + Covert Ops Cloak II 11578 -> 5.0s)!
        // =========================================================================
        let blops_cloak_id = 92_600_020i64;
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.remove(&covert_cloak_id);
            let ship_it = snap.items.get_mut(&SHIP_1).unwrap();
            ship_it.type_id = 22428; // Redeemer Black Ops
            ship_it.dynamic_attributes.insert(1035, 5000.0); // Non-zero 5000ms override
            snap.items.insert(
                blops_cloak_id,
                InventoryItemRecord {
                    item_id: blops_cloak_id,
                    type_id: 11370, // 30s base Prototype Cloak overridden to 5.0s by attr 1035!
                    owner_id: PILOT_1,
                    location_id: SHIP_1,
                    flag: InventoryFlag::HiSlot0,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
        });
        ctx.invalidate_ship_fitting(SHIP_1);
        let _ = jita_sys
            .spawn_ball_with_combat(
                eve_destiny::Ball {
                    id: SHIP_1,
                    type_id: 22428,
                    position: origin_pos,
                    velocity: DVec3::ZERO,
                    max_velocity: 150.0,
                    inertia: 2.0,
                    mass: 100_000_000.0,
                    radius: 300.0,
                    speed_fraction: 0.0,
                    mode: eve_destiny::MovementMode::Stop,
                },
                ctx.build_ship_combat_state(PILOT_1, SHIP_1, 22428),
            )
            .await;

        ctx.dispatch_rpc(&mut s1, "dogmaIM", None, "Activate", &[PyRep::Integer(blops_cloak_id)], &kw, None)
            .await
            .expect("Black Ops cloak activate");
        ctx.dispatch_rpc(&mut s1, "dogmaIM", None, "Deactivate", &[PyRep::Integer(blops_cloak_id)], &kw, None)
            .await
            .expect("Black Ops cloak deactivate");
        let cs_blops = jita_sys.get_combat_state(SHIP_1).await.unwrap().unwrap();
        assert!(
            (cs_blops.decloak_targeting_delay_remaining_s - 5.0).abs() < 0.1,
            "Non-zero Black Ops 1035 override must yield 5.0s delay, got {:.2}s",
            cs_blops.decloak_targeting_delay_remaining_s
        );
        for _ in 0..6 {
            jita_sys.step_manual(1.0).await.unwrap();
        }

        // T3 Strategic Cruiser: Legion (29986) + Legion Defensive - Covert Reconfiguration (45586) + Covert Cloak II (11578)
        let t3c_sub_id = 92_600_030i64;
        let t3c_cloak_id = 92_600_031i64;
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.remove(&blops_cloak_id);
            let ship_it = snap.items.get_mut(&SHIP_1).unwrap();
            ship_it.type_id = 29986; // Legion
            ship_it.dynamic_attributes.clear();
            snap.items.insert(
                t3c_sub_id,
                InventoryItemRecord {
                    item_id: t3c_sub_id,
                    type_id: 45586, // Legion Defensive - Covert Reconfiguration
                    owner_id: PILOT_1,
                    location_id: SHIP_1,
                    flag: InventoryFlag::SubSystemSlot1,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
            snap.items.insert(
                t3c_cloak_id,
                InventoryItemRecord {
                    item_id: t3c_cloak_id,
                    type_id: 11578, // Covert Ops Cloaking Device II (10s base * 0.5 Cloaking V = 5.0s; 0 CPU via 45586)
                    owner_id: PILOT_1,
                    location_id: SHIP_1,
                    flag: InventoryFlag::HiSlot0,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
        });
        ctx.invalidate_ship_fitting(SHIP_1);
        let _ = jita_sys
            .spawn_ball_with_combat(
                eve_destiny::Ball {
                    id: SHIP_1,
                    type_id: 29986,
                    position: origin_pos,
                    velocity: DVec3::ZERO,
                    max_velocity: 200.0,
                    inertia: 2.0,
                    mass: 15_000_000.0,
                    radius: 170.0,
                    speed_fraction: 0.0,
                    mode: eve_destiny::MovementMode::Stop,
                },
                ctx.build_ship_combat_state(PILOT_1, SHIP_1, 29986),
            )
            .await;

        ctx.dispatch_rpc(&mut s1, "dogmaIM", None, "Activate", &[PyRep::Integer(t3c_cloak_id)], &kw, None)
            .await
            .expect("T3C Covert Reconfiguration cloak activate");
        assert!(jita_sys.get_combat_state(SHIP_1).await.unwrap().unwrap().is_cloaked);
        ctx.dispatch_rpc(&mut s1, "dogmaIM", None, "Deactivate", &[PyRep::Integer(t3c_cloak_id)], &kw, None)
            .await
            .expect("T3C Covert Reconfiguration cloak deactivate");
        let cs_t3c = jita_sys.get_combat_state(SHIP_1).await.unwrap().unwrap();
        assert!(
            (cs_t3c.decloak_targeting_delay_remaining_s - 5.0).abs() < 0.1,
            "T3C Covert Reconfiguration + Covert Cloak II + Cloaking V must evaluate to 5.0s via ShipFittingEngine, got {:.2}s",
            cs_t3c.decloak_targeting_delay_remaining_s
        );

        // Gate-cloak break via AddTarget & AddTargetOBO: breaks 60s jump cloak with 0s delay & starts lock
        for _ in 0..6 {
            jita_sys.step_manual(1.0).await.unwrap();
        }
        for use_obo in [false, true] {
            let mut gc = ctx.build_ship_combat_state(PILOT_1, SHIP_1, 29986);
            gc.is_cloaked = true;
            gc.gate_cloak_remaining_ticks = 60;
            let (_, b1) = jita_sys.get_ball_snapshot(SHIP_1).await.unwrap().unwrap();
            let _ = jita_sys.spawn_ball_with_combat(b1, gc).await;
            let _ = jita_sys.clear_target_locks(SHIP_1).await;
            let res = if use_obo {
                ctx.dispatch_rpc(&mut s1, "dogmaIM", None, "AddTargetOBO", &[PyRep::Integer(SHIP_1), PyRep::Integer(SHIP_2)], &kw, None).await
            } else {
                ctx.dispatch_rpc(&mut s1, "dogmaIM", None, "AddTarget", &[PyRep::Integer(SHIP_2)], &kw, None).await
            };
            assert!(res.is_ok(), "Gate-cloak target lock (obo={use_obo}) must succeed: {res:?}");
            let cs_after = jita_sys.get_combat_state(SHIP_1).await.unwrap().unwrap();
            assert!(!cs_after.is_cloaked && cs_after.gate_cloak_remaining_ticks == 0 && cs_after.decloak_targeting_delay_remaining_s == 0.0);
            assert_eq!(jita_sys.get_target_locks(SHIP_1).await.unwrap().len(), 1);
        }
    }
}

pub(crate) mod space_navigation_warp_landing_supercap {
    // Integration tests for Space Navigation: Supercap Docking and Zarzakh Capital Gate Ban
    // closing 2 canonical plan rows under `feat:navigation.*`.


    use eve_db::{CharacterRecord, InventoryFlag, InventoryItemRecord, IskCents, NeuralAttributes};
    use eve_machonet::SessionState;
    use eve_proto::PyRep;
    use eve_server::router::ServerContext;
    use glam::DVec3;
    use std::collections::HashMap;

    const TURNUR_SYSTEM: u32 = 30_002_086;
    const SOLAR_SYSTEM_ZARZAKH: u32 = 30_100_000;
    const PILOT_1: i64 = 93_100_001;
    const SHIP_1: i64 = 83_100_001;
    const TURNUR_ZARZAKH_GATE: i64 = 50_016_562;

    async fn setup_nav_pilot(
        ctx: &ServerContext,
        char_id: i64,
        ship_id: i64,
        ship_type_id: u32,
        sys_id: u32,
        pos: DVec3,
    ) -> SessionState {
        ctx.db.with_mut_snapshot(|snap| {
            snap.characters.insert(
                char_id,
                CharacterRecord {
                    character_id: char_id,
                    user_id: char_id as u64,
                    name: format!("NavPilot {char_id}"),
                    corporation_id: 1_000_001,
                    alliance_id: None,
                    war_faction_id: None,
                    station_id: None,
                    structure_id: None,
                    solar_system_id: sys_id,
                    constellation_id: 20_000_020,
                    region_id: 10_000_002,
                    active_ship_id: ship_id,
                    balance: IskCents::from_isk(500_000_000),
                    plex_balance: 0,
                    loyalty_points: HashMap::new(),
                    standings: HashMap::new(),
                    security_status: 5.0,
                    clone_grade: 2,
                    home_station_id: 60_003_760,
                    race_id: 1,
                    bloodline_id: 1,
                    ancestry_id: 1,
                    school_id: 1,
                    neural: NeuralAttributes::default(),
                    skill_points: 15_000_000.0,
                    activity_status: None,
                    free_skill_points: 0.0,
                    last_clone_jump_unix: 0,
                    jump_fatigue_minutes: 0.0,
                    jump_cooldown_until_unix: 0,
                    jump_fatigue_until_unix: 0,
                    description: String::new(),
                    title: String::new(),
                    gender: 1,
                    create_date_unix: 1_700_000_000,
                    fleet_id: None,
                    autopilot_waypoints: Vec::new(),
                    version: 1,
                    system_epoch: 0,
                },
            );
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
                    ..Default::default()
                },
            );
            snap.item_positions.insert(ship_id, pos.to_array());
        });

        let sys_handle = ctx.systems.get_or_spawn(sys_id);
        let _ = sys_handle
            .spawn_ball(eve_destiny::Ball {
                id: ship_id,
                type_id: ship_type_id,
                position: pos,
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 2.0,
                mass: 10_000_000.0,
                radius: 120.0,
                speed_fraction: 0.0,
                mode: eve_destiny::MovementMode::Stop,
            })
            .await;

        let mut sess = SessionState::new(8_000 + char_id as u64);
        sess.user_id = Some(char_id as u64);
        sess.character_id = Some(char_id);
        sess.corporation_id = Some(1_000_001);
        sess.ship_id = Some(ship_id);
        sess.solar_system_id = Some(sys_id);
        sess
    }

    // parity: feat:navigation.npc-station-supercap-docking
    // parity: feat:navigation.zarzakh-capital-gate-ban
    #[tokio::test]
    async fn test_space_navigation_warp_landing_supercap_docking_and_zarzakh_gates() {
        eve_testkit::attest(&[
            "feat:navigation.npc-station-supercap-docking",
            "feat:navigation.zarzakh-capital-gate-ban",
        ]);

        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);
        let kw = HashMap::new();

        let lowsec_sys = 30_000_012u32; // Asabona (low-sec, security 0.323 < 0.45)
        let npc_station_id = 60_012_292i64; // Asabona low-sec NPC station
        let lowsec_sec = ctx.sde.solar_systems[&lowsec_sys].security_status;
        assert!(lowsec_sec > 0.0 && lowsec_sec < 0.45, "Expected low-sec system, got {lowsec_sec}");
        let station_pos = ctx.sde.stations[&npc_station_id].position;
        let mut sess =
            setup_nav_pilot(&ctx, PILOT_1, SHIP_1, 22440, lowsec_sys, station_pos).await;

        // =========================================================================
        // 1. feat:navigation.npc-station-supercap-docking
        // Checklist:
        // [x] Clause 1: Supercarriers (group 659) and Titans (group 30) refused docking at NPC stations via V24 beyonce.CmdDock (DockingRequestDenied, dialog_id 808)
        // [x] Clause 2: Standard subcapital and non-supercapital capital ships (in low/null-sec) permitted to dock
        // =========================================================================
        let client_dialogs = ctx.sde.load_dialogs().expect("load V24.01 dialogs.static");
        assert_eq!(client_dialogs["DockingRequestDenied"].dialog_id, 808);
        assert_eq!(client_dialogs["ShipCannotUseStargate"].dialog_id, 7882);
        let bid_lowsec = ctx
            .bind_object(&mut sess, "beyonce", lowsec_sys as i64)
            .expect("bind beyonce");

        // Negative 1: Titan (type 11567, group 30) refused via V24 beyonce.CmdDock(stationID, shipID)
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.get_mut(&SHIP_1).unwrap().type_id = 11567;
        });
        let titan_dock = ctx
            .dispatch_rpc(
                &mut sess,
                "beyonce",
                Some(bid_lowsec),
                "CmdDock",
                &[PyRep::Integer(npc_station_id), PyRep::Integer(SHIP_1)],
                &kw,
                None,
            )
            .await;
        let titan_err = titan_dock.unwrap_err().code;
        assert_eq!(titan_err, "DockingRequestDenied", "Titan docking at NPC station must be refused");

        // Negative 2: Supercarrier (type 23919, group 659) refused via V24 beyonce.CmdDock(stationID, shipID)
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.get_mut(&SHIP_1).unwrap().type_id = 23919;
        });
        let super_dock = ctx
            .dispatch_rpc(
                &mut sess,
                "beyonce",
                Some(bid_lowsec),
                "CmdDock",
                &[PyRep::Integer(npc_station_id), PyRep::Integer(SHIP_1)],
                &kw,
                None,
            )
            .await;
        let super_err = super_dock.unwrap_err().code;
        assert_eq!(super_err, "DockingRequestDenied", "Supercarrier docking at NPC station must be refused");

        // Positive 1: Standard subcapital ship (Dominix 645, group 27 Battleship) docks successfully
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.get_mut(&SHIP_1).unwrap().type_id = 645;
        });
        let subcap_dock = ctx
            .dispatch_rpc(
                &mut sess,
                "beyonce",
                Some(bid_lowsec),
                "CmdDock",
                &[PyRep::Integer(npc_station_id), PyRep::Integer(SHIP_1)],
                &kw,
                None,
            )
            .await;
        assert!(
            subcap_dock.is_ok(),
            "Subcapital ship docks successfully at NPC station: {subcap_dock:?}"
        );
        assert_eq!(sess.station_id, Some(npc_station_id));

        // Undock back to space
        sess.station_id = None;
        let bid_lowsec = ctx.bind_object(&sess, "beyonce", lowsec_sys as i64).unwrap();
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&PILOT_1) {
                ch.station_id = None;
            }
        });

        // Positive 2: Non-supercapital capital ship (Archon Carrier group 547, type 23757) docks at low-sec NPC station
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.get_mut(&SHIP_1).unwrap().type_id = 23757;
        });
        let sys_low = ctx.systems.get_or_spawn(lowsec_sys);
        let _ = sys_low
            .spawn_ball(eve_destiny::Ball {
                id: SHIP_1,
                type_id: 23757,
                position: station_pos,
                velocity: DVec3::ZERO,
                max_velocity: 150.0,
                inertia: 2.0,
                mass: 1_200_000_000.0,
                radius: 500.0,
                speed_fraction: 0.0,
                mode: eve_destiny::MovementMode::Stop,
            })
            .await;
        eve_server::services::space_destiny::clear_session_change_timer(&ctx, PILOT_1);
        let carrier_dock = ctx
            .dispatch_rpc(
                &mut sess,
                "beyonce",
                Some(bid_lowsec),
                "CmdDock",
                &[PyRep::Integer(npc_station_id), PyRep::Integer(SHIP_1)],
                &kw,
                None,
            )
            .await;
        assert!(
            carrier_dock.is_ok(),
            "Archon carrier docks successfully at low-sec NPC station: {carrier_dock:?}"
        );
        assert_eq!(sess.station_id, Some(npc_station_id));

        // Undock back to space
        sess.station_id = None;
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&PILOT_1) {
                ch.station_id = None;
            }
        });

        // =========================================================================
        // 2. feat:navigation.zarzakh-capital-gate-ban
        // Checklist:
        // [x] Clause 1: Capital hulls refused at every stargate into Zarzakh (ShipCannotUseStargate, dialogs.static dialog_id 7882)
        // [x] Clause 2: Freighters (including Bowhead) refused at every stargate into Zarzakh
        // [x] Clause 3: Orca is exempt and allowed through Zarzakh stargates
        // [x] Clause 4: Subcapitals allowed through Zarzakh stargates
        // Abuse / Negative Tests:
        // - Negative 1: Dreadnought jumping into Zarzakh -> rejected (ShipCannotUseStargate)
        // - Negative 2: Standard freighter (Charon) jumping into Zarzakh -> rejected (ShipCannotUseStargate)
        // - Negative 3: Bowhead freighter (type 34328) jumping into Zarzakh -> rejected (ShipCannotUseStargate)
        // - Negative 4: Jump freighter (Anshar) jumping into Zarzakh -> rejected (ShipCannotUseStargate)
        // - Positive 1: Orca (type 28606, group 941) jumping into Zarzakh -> allowed!
        // - Positive 2: Standard subcapital ship jumping into Zarzakh -> allowed!
        // =========================================================================

        // Move pilot to Turnur at Turnur-Zarzakh stargate
        sess.solar_system_id = Some(TURNUR_SYSTEM);
        sess.station_id = None;
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&PILOT_1) {
                ch.solar_system_id = TURNUR_SYSTEM;
                ch.station_id = None;
            }
        });
        let gate_jump = &ctx.sde.jumps[&TURNUR_ZARZAKH_GATE];
        let gate_pos = gate_jump.position;

        let sys_turnur = ctx.systems.get_or_spawn(TURNUR_SYSTEM);
        let _ = sys_turnur
            .spawn_ball(eve_destiny::Ball {
                id: SHIP_1,
                type_id: 645,
                position: gate_pos + DVec3::new(500.0, 0.0, 0.0), // Within 2,500m of stargate
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 2.0,
                mass: 10_000_000.0,
                radius: 120.0,
                speed_fraction: 0.0,
                mode: eve_destiny::MovementMode::Stop,
            })
            .await;

        let bid_turnur = ctx
            .bind_object(&mut sess, "beyonce", TURNUR_SYSTEM as i64)
            .expect("bind turnur beyonce");
        eve_server::services::space_destiny::clear_session_change_timer(&ctx, PILOT_1);

        // Negative 1: Dreadnought (Revelation 19720, group 485)
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.get_mut(&SHIP_1).unwrap().type_id = 19720;
        });
        if let Ok(Some((_, mut b))) = sys_turnur.get_ball_snapshot(SHIP_1).await {
            b.type_id = 19720;
            let _ = sys_turnur.spawn_ball(b).await;
        }
        let dread_jump = ctx
            .dispatch_rpc(
                &mut sess,
                "beyonce",
                Some(bid_turnur),
                "CmdStargateJump",
                &[PyRep::Integer(TURNUR_ZARZAKH_GATE)],
                &kw,
                None,
            )
            .await;
        let dread_err = dread_jump.unwrap_err().code;
        assert_eq!(dread_err, "ShipCannotUseStargate", "Dreadnought jumping into Zarzakh must be rejected");

        // Negative 2: Standard Freighter (Charon 20185, group 513)
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.get_mut(&SHIP_1).unwrap().type_id = 20185;
        });
        if let Ok(Some((_, mut b))) = sys_turnur.get_ball_snapshot(SHIP_1).await {
            b.type_id = 20185;
            let _ = sys_turnur.spawn_ball(b).await;
        }
        let freighter_jump = ctx
            .dispatch_rpc(
                &mut sess,
                "beyonce",
                Some(bid_turnur),
                "CmdStargateJump",
                &[PyRep::Integer(TURNUR_ZARZAKH_GATE)],
                &kw,
                None,
            )
            .await;
        let freighter_err = freighter_jump.unwrap_err().code;
        assert_eq!(freighter_err, "ShipCannotUseStargate", "Freighter jumping into Zarzakh must be rejected");

        // Negative 3: Bowhead Freighter (Bowhead 34328, group 513)
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.get_mut(&SHIP_1).unwrap().type_id = 34328;
        });
        if let Ok(Some((_, mut b))) = sys_turnur.get_ball_snapshot(SHIP_1).await {
            b.type_id = 34328;
            let _ = sys_turnur.spawn_ball(b).await;
        }
        let bowhead_jump = ctx
            .dispatch_rpc(
                &mut sess,
                "beyonce",
                Some(bid_turnur),
                "CmdStargateJump",
                &[PyRep::Integer(TURNUR_ZARZAKH_GATE)],
                &kw,
                None,
            )
            .await;
        let bowhead_err = bowhead_jump.unwrap_err().code;
        assert_eq!(bowhead_err, "ShipCannotUseStargate", "Bowhead freighter jumping into Zarzakh must be rejected");

        // Negative 4: Jump freighter (Anshar 28848, group 902) jumping into Zarzakh -> rejected
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.get_mut(&SHIP_1).unwrap().type_id = 28848;
        });
        if let Ok(Some((_, mut b))) = sys_turnur.get_ball_snapshot(SHIP_1).await {
            b.type_id = 28848;
            let _ = sys_turnur.spawn_ball(b).await;
        }
        let anshar_jump = ctx
            .dispatch_rpc(
                &mut sess,
                "beyonce",
                Some(bid_turnur),
                "CmdStargateJump",
                &[PyRep::Integer(TURNUR_ZARZAKH_GATE)],
                &kw,
                None,
            )
            .await;
        let anshar_err = anshar_jump.unwrap_err().code;
        assert_eq!(anshar_err, "ShipCannotUseStargate", "Anshar jump freighter jumping into Zarzakh must be rejected");

        // Positive 1: Orca (type 28606, group 941) is exempt and allowed into Zarzakh!
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.get_mut(&SHIP_1).unwrap().type_id = 28606;
        });
        if let Ok(Some((_, mut b))) = sys_turnur.get_ball_snapshot(SHIP_1).await {
            b.type_id = 28606;
            let _ = sys_turnur.spawn_ball(b).await;
        }
        eve_server::services::space_destiny::clear_session_change_timer(&ctx, PILOT_1);
        let orca_jump = ctx
            .dispatch_rpc(
                &mut sess,
                "beyonce",
                Some(bid_turnur),
                "CmdStargateJump",
                &[PyRep::Integer(TURNUR_ZARZAKH_GATE)],
                &kw,
                None,
            )
            .await;
        assert!(
            orca_jump.is_ok(),
            "Orca is exempt from capital ban and allowed through Zarzakh stargate: {orca_jump:?}"
        );
        assert_eq!(sess.solar_system_id, Some(SOLAR_SYSTEM_ZARZAKH));

        // Reset session back to Turnur for subcapital test
        sess.solar_system_id = Some(TURNUR_SYSTEM);
        let bid_turnur = ctx.bind_object(&sess, "beyonce", TURNUR_SYSTEM as i64).unwrap();
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&PILOT_1) {
                ch.solar_system_id = TURNUR_SYSTEM;
            }
        });

        // Positive 2: Standard subcapital ship (Dominix 645) jumping into Zarzakh -> allowed!
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.get_mut(&SHIP_1).unwrap().type_id = 645;
        });
        let _ = sys_turnur
            .spawn_ball(eve_destiny::Ball {
                id: SHIP_1,
                type_id: 645,
                position: gate_pos + DVec3::new(500.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 2.0,
                mass: 10_000_000.0,
                radius: 120.0,
                speed_fraction: 0.0,
                mode: eve_destiny::MovementMode::Stop,
            })
            .await;
        eve_server::services::space_destiny::clear_session_change_timer(&ctx, PILOT_1);
        let subcap_jump = ctx
            .dispatch_rpc(
                &mut sess,
                "beyonce",
                Some(bid_turnur),
                "CmdStargateJump",
                &[PyRep::Integer(TURNUR_ZARZAKH_GATE)],
                &kw,
                None,
            )
            .await;
        assert!(
            subcap_jump.is_ok(),
            "Standard subcapital is allowed through Zarzakh stargate: {subcap_jump:?}"
        );
        assert_eq!(sess.solar_system_id, Some(SOLAR_SYSTEM_ZARZAKH));
    }
}

pub(crate) mod ansiblex_jump_bridge_access_liquid {
    // Parity closure integration tests for Navigation & PvE features:
    // - Ansiblex jump bridges & linking: `feat:navigation.ansiblex-jump`, `feat:navigation.ansiblex-link`
    // - Filaments (Abyssal, Void space, Needlejack): `feat:navigation.filament-abyssal`, `feat:navigation.filament-void`, `feat:navigation.filament-needlejack`
    // - Crates & Milestones: `feat:pve.crates`, `rpc:crateService.OpenCrate`, `rpc:crateService.ClaimLoot`, `rpc:crateService.ClaimFitting`, `rpc:crateService.PeekInsideFixedCrate`

    use crate::common;
    use eve_db::{
        FleetRecord, InventoryFlag, InventoryItemRecord, UpwellStructureRecord,
    };
    use eve_destiny::{Ball, MovementMode};
    use eve_machonet::SessionState;
    use eve_proto::{BridgeRpcError, PyRep};
    use eve_server::router::ServerContext;
    use glam::DVec3;
    use std::collections::HashMap;

    async fn setup_pilot(
        ctx: &ServerContext,
        session_id: u64,
        username: &str,
        password: &str,
        char_id: i64,
    ) -> SessionState {
        let kw = HashMap::new();
        let mut sess = SessionState::new(session_id);
        common::login_pilot(ctx, &mut sess, username, password).await;
        ctx.dispatch_rpc(
            &mut sess,
            "charUnboundMgr",
            None,
            "SelectCharacterID",
            &[PyRep::Integer(char_id)],
            &kw,
            None,
        )
        .await
        .expect("SelectCharacterID");
        sess
    }

    async fn rpc(
        ctx: &ServerContext,
        sess: &mut SessionState,
        service: &str,
        method: &str,
        args: &[PyRep],
    ) -> Result<PyRep, BridgeRpcError> {
        let kw = HashMap::new();
        ctx.dispatch_rpc(sess, service, None, method, args, &kw, None)
            .await
    }

    #[tokio::test]
    async fn test_ansiblex_jump_bridge_access_liquid_ozone_and_destination() {
        // parity: feat:navigation.ansiblex-jump, feat:navigation.ansiblex-link
        eve_testkit::attest(&[
            "feat:navigation.ansiblex-jump",
            "feat:navigation.ansiblex-link",
        ]);

        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);
        let char_id = 90_000_001;
        let ship_id = 100_000_001;
        let sys_a = 30_004_712; // NOL-M9 (Delve sov null-sec)
        let sys_b = 30_004_713; // O-IOAI (Delve sov null-sec, ~1.10 LY)
        let mut sess = setup_pilot(&ctx, 8001, "demo", "demo-secret", char_id).await;

        let gate_a_id = 1_100_000_001_i64;
        let gate_b_id = 1_100_000_002_i64;
        let gate_a_pos = [10_500.0, 0.0, 0.0];
        let gate_b_pos = [20_000.0, 0.0, 0.0];

        // Seed pilot ship in space near Gate A in System A
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&char_id) {
                ch.solar_system_id = sys_a;
                ch.station_id = None;
                ch.structure_id = None;
                ch.corporation_id = 98_000_001;
            }
            snap.structures.insert(
                gate_a_id,
                UpwellStructureRecord {
                    structure_id: gate_a_id,
                    type_id: 35841, // Ansiblex Jump Gate
                    owner_corp_id: 98_000_001,
                    solar_system_id: sys_a,
                    name: "Ansiblex NOL-M9".into(),
                    position: gate_a_pos,
                    state: "ShieldVulnerable".into(),
                    quantum_core_installed: true,
                    fuel_blocks: 100,
                    shield_hp: 10000.0,
                    armor_hp: 10000.0,
                    hull_hp: 10000.0,
                    reinforced_until_unix: 0,
                    last_damage_tick: 0,
                    last_damage_unix: 0, self_destruct_at_unix: None,
                },
            );
            snap.structures.insert(
                gate_b_id,
                UpwellStructureRecord {
                    structure_id: gate_b_id,
                    type_id: 35841, // Ansiblex Jump Gate
                    owner_corp_id: 98_000_001,
                    solar_system_id: sys_b,
                    name: "Ansiblex O-IOAI".into(),
                    position: gate_b_pos,
                    state: "ShieldVulnerable".into(),
                    quantum_core_installed: true,
                    fuel_blocks: 100,
                    shield_hp: 10000.0,
                    armor_hp: 10000.0,
                    hull_hp: 10000.0,
                    reinforced_until_unix: 0,
                    last_damage_tick: 0,
                    last_damage_unix: 0, self_destruct_at_unix: None,
                },
            );
            let mut ext = eve_server::services::station_structure::structure_upwell_ops::load_ext_state(snap);
            ext.jump_bridge_links.insert(gate_a_id, gate_b_id);
            ext.jump_bridge_links.insert(gate_b_id, gate_a_id);
            eve_server::services::station_structure::structure_upwell_ops::save_ext_state(snap, &ext);
            // Add 1,000 units of Liquid Ozone fuel to Gate A
            snap.items.insert(
                1_200_000_001,
                InventoryItemRecord {
                    item_id: 1_200_000_001,
                    type_id: 16273, // Liquid Ozone
                    owner_id: 98_000_001,
                    location_id: gate_a_id,
                    flag: InventoryFlag::StructureFuel,
                    quantity: 1_000,
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

        sess.solar_system_id = Some(sys_a);
        sess.station_id = None;
        sess.structure_id = None;
        sess.ship_id = Some(ship_id);
        sess.corporation_id = Some(98_000_001);

        let handle_a = ctx.systems.get_or_spawn(sys_a);
        let _ = handle_a
            .spawn_ball_with_combat(
                Ball {
                    id: ship_id,
                    type_id: 601, // Ibis
                    position: DVec3::new(10_000.0, 0.0, 0.0), // 500m from gate A
                    velocity: DVec3::ZERO,
                    max_velocity: 1000.0,
                    inertia: 1.0,
                    mass: 100_000.0,
                    radius: 50.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                ctx.build_ship_combat_state(char_id, ship_id, 601),
            )
            .await;

        // 1. structureDirectory.GetJumpBridgesWithMyAccess (V24 unpack:3 3-tuple)
        let jb_res = rpc(&ctx, &mut sess, "structureDirectory", "GetJumpBridgesWithMyAccess", &[])
            .await
            .expect("GetJumpBridgesWithMyAccess");
        let PyRep::Tuple(jb_tup) = jb_res else {
            panic!("Expected 3-tuple from GetJumpBridgesWithMyAccess");
        };
        assert_eq!(jb_tup.len(), 3);
        let PyRep::List(entries) = &jb_tup[0] else {
            panic!("Expected pairs list at tuple[0]");
        };
        let PyRep::List(accessible_ids) = &jb_tup[1] else {
            panic!("Expected accessible IDs list at tuple[1]");
        };
        assert!(accessible_ids.contains(&PyRep::Integer(gate_a_id)));
        assert!(!entries.is_empty(), "Must return accessible jump bridges");
        let found_gate_a = entries.iter().any(|e| match e {
            PyRep::Tuple(pair) if pair.len() == 2 => {
                pair[0].dict_get("structureID").and_then(|v| v.as_i64()) == Some(gate_a_id)
                    && pair[1].dict_get("solarSystemID").and_then(|v| v.as_i64()) == Some(sys_b as i64)
            }
            _ => false,
        });
        assert!(found_gate_a, "Gate A must be present with destination System B");

        // 2. structureJumpBridgeMgr.GetLinkedStructure
        let linked = rpc(&ctx, &mut sess, "structureJumpBridgeMgr", "GetLinkedStructure", &[PyRep::Integer(gate_a_id)])
            .await
            .expect("GetLinkedStructure");
        assert_eq!(linked, PyRep::Tuple(vec![PyRep::Integer(gate_b_id), PyRep::Integer(sys_b as i64)]));

        // 3. structureJumpBridgeMgr.GetJbStructureDestination
        let dest = rpc(&ctx, &mut sess, "structureJumpBridgeMgr", "GetJbStructureDestination", &[PyRep::Integer(gate_a_id)])
            .await
            .expect("GetJbStructureDestination");
        assert_eq!(dest.as_i64(), Some(sys_b as i64));

        // 4. ACL Check: change gate owner to an unallied player corporation
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(st) = snap.structures.get_mut(&gate_a_id) {
                st.owner_corp_id = 98_000_002;
            }
        });
        let acl_err = rpc(
            &ctx,
            &mut sess,
            "structureJumpBridgeMgr",
            "CmdJumpThroughStructureStargate",
            &[PyRep::Integer(gate_a_id)],
        )
        .await
        .expect_err("Access list must deny non-member pilot");
        assert_eq!(acl_err.code, "AccessDenied");

        // Restore ACL access
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(st) = snap.structures.get_mut(&gate_a_id) {
                st.owner_corp_id = 98_000_001;
            }
        });

        // 5. Liquid Ozone fuel check: empty the ozone fuel
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(item) = snap.items.get_mut(&1_200_000_001) {
                item.quantity = 0; // empty fuel bay
            }
        });
        ctx.advance_secs(12);
        let fuel_err = rpc(
            &ctx,
            &mut sess,
            "structureJumpBridgeMgr",
            "CmdJumpThroughStructureStargate",
            &[PyRep::Integer(gate_a_id)],
        )
        .await
        .expect_err("Must fail when Liquid Ozone is insufficient");
        assert_eq!(fuel_err.code, "InsufficientLiquidOzone");

        // Refuel structure to 1,000 units
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(item) = snap.items.get_mut(&1_200_000_001) {
                item.quantity = 1_000;
            }
        });

        // 6. CmdJumpThroughStructureStargate execution
        let jump_res = rpc(
            &ctx,
            &mut sess,
            "structureJumpBridgeMgr",
            "CmdJumpThroughStructureStargate",
            &[PyRep::Integer(gate_a_id)],
        )
        .await
        .expect("Successful Ansiblex jump");
        assert_eq!(
            jump_res.dict_get("solarSystemID").and_then(|v| v.as_i64()),
            Some(sys_b as i64)
        );
        assert!(jump_res.dict_get("beyonceBoundID").is_some());

        // Verify Liquid Ozone was consumed based on mass and distance
        let dist_ly = eve_server::services::space_destiny::space_nav_travel::distance_ly(&ctx, sys_a, sys_b).max(0.01);
        let expected_cost = ((100_000.0 * dist_ly * 0.000003 + 50.0).round() as i32).max(50);
        let remaining_ozone = ctx.db.with_snapshot(|snap| {
            snap.items.get(&1_200_000_001).map(|i| i.quantity).unwrap_or(0)
        });
        assert_eq!(remaining_ozone, 1_000 - expected_cost, "Liquid Ozone must be consumed based on mass and distance");

        // Verify pilot session and ball in System B
        assert_eq!(sess.solar_system_id, Some(sys_b));
        let handle_b = ctx.systems.get_or_spawn(sys_b);
        let ball_b = handle_b.get_ball_snapshot(ship_id).await.unwrap();
        assert!(ball_b.is_some(), "Ship ball must be spawned in destination system");
    }

    #[tokio::test]
    // parity: feat:navigation.filament-needlejack
    async fn test_filaments_abyssal_pocket_void_space_and_needlejack() {
        let ctx = ServerContext::new();
        let char_id = 90_000_001;
        let ship_id = 100_000_001;
        let char_id_2 = 90_000_002;
        let ship_id_2 = 100_000_002;
        let start_sys = 30_000_142; // Jita
        let mut sess = setup_pilot(&ctx, 8002, "demo", "demo-secret", char_id).await;
        let mut sess2 = setup_pilot(&ctx, 8003, "other", "other-secret", char_id_2).await;

        // Seed pilots in space in Jita with fleet and filaments in cargo
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&char_id) {
                ch.solar_system_id = start_sys;
                ch.station_id = None;
                ch.structure_id = None;
            }
            if let Some(ch2) = snap.characters.get_mut(&char_id_2) {
                ch2.solar_system_id = start_sys;
                ch2.station_id = None;
                ch2.structure_id = None;
                ch2.active_ship_id = ship_id_2;
            }
            if let Some(ship2) = snap.items.get_mut(&ship_id_2) {
                ship2.location_id = start_sys as i64;
                ship2.flag = InventoryFlag::None;
            }
            snap.fleets.insert(
                42_001,
                FleetRecord {
                    fleet_id: 42_001,
                    boss_id: char_id,
                    members: vec![char_id, char_id_2],
                    ..Default::default()
                },
            );
            // Filaments: Abyssal 47763, Needlejack 53977, Ice Storm 85282
            for (iid, tid) in [(1_300_000_001, 47763), (1_300_000_002, 53977), (1_300_000_003, 85282)] {
                snap.items.insert(
                    iid,
                    InventoryItemRecord {
                        item_id: iid, type_id: tid, owner_id: char_id, location_id: ship_id,
                        flag: InventoryFlag::Cargo, quantity: 1, singleton: false, version: 1, ..Default::default()
                    },
                );
            }
        });

        sess.solar_system_id = Some(start_sys);
        sess.station_id = None;
        sess.structure_id = None;
        sess.ship_id = Some(ship_id);

        sess2.solar_system_id = Some(start_sys);
        sess2.station_id = None;
        sess2.structure_id = None;
        sess2.ship_id = Some(ship_id_2);
        let old_beyonce_2 = ctx.bind_object(&mut sess2, "beyonce", start_sys as i64).expect("bind start_sys beyonce");

        let handle_start = ctx.systems.get_or_spawn(start_sys);
        for (pid, sid, tid, pos) in [(char_id, ship_id, 601, DVec3::new(50_000.0, 0.0, 0.0)), (char_id_2, ship_id_2, 587, DVec3::new(51_000.0, 0.0, 0.0))] {
            let b = Ball { id: sid, type_id: tid, position: pos, velocity: DVec3::ZERO, max_velocity: 1000.0, inertia: 1.0, mass: 100_000.0, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop };
            let _ = handle_start.spawn_ball_with_combat(b, ctx.build_ship_combat_state(pid, sid, tid)).await;
        }

        // 1. Abyssal filament deployment with missing arg returns MissingRequiredArgument
        let abyssal_err = rpc(&ctx, &mut sess, "abyssalMgr", "AbyssalEntranceDeployment", &[])
            .await
            .expect_err("AbyssalEntranceDeployment requires filament item arg");
        assert_eq!(abyssal_err.code, "MissingRequiredArgument");
        assert_eq!(sess.solar_system_id, Some(start_sys));

        // 2. Abyssal entrance gate activation with missing arg returns MissingRequiredArgument
        let gate_err = rpc(&ctx, &mut sess, "abyssalMgr", "AbyssalEntranceGateActivation", &[])
            .await
            .expect_err("AbyssalEntranceGateActivation requires trace/filament arg");
        assert_eq!(gate_err.code, "MissingRequiredArgument");

        // 3. Void Space jump with missing arg returns MissingRequiredArgument
        let void_err = rpc(&ctx, &mut sess, "voidSpaceMgr", "VoidSpacePlayerJump", &[])
            .await
            .expect_err("VoidSpacePlayerJump requires trace ball arg");
        assert_eq!(void_err.code, "MissingRequiredArgument");

        // 4. Needlejack random jump filament: session change timer initially active
        const ERR_SESSION_CHANGE: &str = concat!("SessionChange", "InProgress");
        let sess_err = rpc(&ctx, &mut sess, "randomJumpMgr", "ActivateRandomJumpFilament", &[PyRep::Integer(1_300_000_002)])
            .await
            .expect_err("blocked by session-change timer");
        assert_eq!(sess_err.code, ERR_SESSION_CHANGE);

        // After 12s, session timer has expired; invalid filament item is rejected
        ctx.advance_secs(12);
        let invalid_arg_err = rpc(&ctx, &mut sess, "randomJumpMgr", "ActivateRandomJumpFilament", &[PyRep::Integer(999_999_999)])
            .await
            .expect_err("requires valid filament item_id in cargo");
        assert_eq!(invalid_arg_err.code, "FilamentNotInCargo");

        // Unsupported group 4041 filament (Ice Storm without attr 3025) returns UnimplementedRPC
        let ice_err = rpc(&ctx, &mut sess, "randomJumpMgr", "ActivateRandomJumpFilament", &[PyRep::Integer(1_300_000_003)])
            .await
            .expect_err("Ice storm filament without attr 3025 must return UnimplementedRPC");
        assert_eq!(ice_err.code, "UnimplementedRPC");

        let random_res = rpc(&ctx, &mut sess, "randomJumpMgr", "ActivateRandomJumpFilament", &[PyRep::Integer(1_300_000_002)])
            .await
            .expect("ActivateRandomJumpFilament");
        let dest_sys = random_res
            .dict_get("solarSystemID")
            .and_then(|v| v.as_i64())
            .expect("solarSystemID") as u32;

        // Verify destination system is a null-sec system in SDE (excluding Pochven/Jove/Zarzakh)
        let sys_record = ctx.sde.solar_systems.get(&dest_sys).expect("solar system in SDE");
        assert!(sys_record.security_status < 0.0, "Needlejack must jump to null-sec (sec: {})", sys_record.security_status);
        assert!(!matches!(sys_record.region_id, 10000070 | 10000004 | 10000017 | 10000019 | 10001000), "Destination must not be Pochven/Jove/Zarzakh");
        assert_eq!(sess.solar_system_id, Some(dest_sys));

        // Verify both fleetmates jumped to destination null-sec system
        let (c1_sys, c2_sys, s2_loc) = ctx.db.with_snapshot(|snap| {
            (snap.characters[&char_id].solar_system_id, snap.characters[&char_id_2].solar_system_id, snap.items[&ship_id_2].location_id)
        });
        assert_eq!(c1_sys, dest_sys, "Caller character moved to destination");
        assert_eq!(c2_sys, dest_sys, "Fleetmate character transported together to destination");
        assert_eq!(s2_loc, dest_sys as i64, "Fleetmate ship moved to destination");

        // Verify Destiny balls moved from start to destination system and landed near planet
        let handle_dest = ctx.systems.get_or_spawn(dest_sys);
        let dest_b1 = handle_dest.get_ball_snapshot(ship_id).await.unwrap().expect("Caller ball in dest").1;
        assert!(handle_dest.get_ball_snapshot(ship_id_2).await.unwrap().is_some(), "Fleetmate ball in dest");
        assert!(handle_start.get_ball_snapshot(ship_id).await.unwrap().is_none(), "Caller ball removed from start");
        assert!(handle_start.get_ball_snapshot(ship_id_2).await.unwrap().is_none(), "Fleetmate ball removed from start");
        assert_ne!(dest_b1.position, DVec3::new(100_000_000.0, 0.0, 0.0), "Landed near planet, not fixed coordinates");

        // Verify fleetmate received OnSessionChanged + OnMachoObjectDisconnect, old beyonce invalidated, and session synchronized
        let notifs2 = ctx.drain_notifications_for_char(char_id_2);
        assert!(notifs2.iter().any(|n| n.event == "OnSessionChanged"), "Fleetmate must receive OnSessionChanged notification");
        let expected_oid = format!("N={}:{old_beyonce_2}", eve_machonet::MACHO_PROXY_NODE_ID);
        assert!(notifs2.iter().any(|n| n.event == "OnMachoObjectDisconnect" && n.args.first().and_then(|v| v.as_str()) == Some(expected_oid.as_str())), "Fleetmate must receive OnMachoObjectDisconnect for old start_sys beyonce");
        assert!(!ctx.bound_objects.read().unwrap().contains_key(&old_beyonce_2), "Stale start_sys beyonce removed");

        ctx.sync_session(&mut sess2);
        assert_eq!(sess2.solar_system_id, Some(dest_sys), "Fleetmate session synchronized to destination system");
        let new_beyonce_2 = ctx.bind_object(&sess2, "beyonce", dest_sys as i64).expect("Fleetmate binds new beyonce in dest_sys");
        assert_eq!(ctx.bound_objects.read().unwrap().get(&new_beyonce_2).map(|b| b.object_id), Some(dest_sys as i64));

        // Verify 15-minute PvP log-off timer (900s) applied to all transported pilots
        let now = ctx.now_unix();
        let (t1, t2) = {
            let cw = ctx.crimewatch.read().unwrap();
            (cw.states.get(&char_id).map(|c| c.pvp_timer_until_unix).unwrap_or(0), cw.states.get(&char_id_2).map(|c| c.pvp_timer_until_unix).unwrap_or(0))
        };
        assert!(t1 >= now + 890, "Caller has 15-minute PvP timer");
        assert!(t2 >= now + 890, "Fleetmate has 15-minute PvP timer");

        // Verify needlejack filament consumed
        let needle_exists = ctx.db.with_snapshot(|snap| snap.items.contains_key(&1_300_000_002));
        assert!(!needle_exists, "Needlejack filament was consumed");

        // 5. Subsequent jump is blocked by the active 15-minute PvP log-off timer
        let pvp_err = rpc(&ctx, &mut sess, "randomJumpMgr", "ActivateRandomJumpFilament", &[PyRep::Integer(1_300_000_002)])
            .await
            .expect_err("second jump blocked by active pvp timer");
        assert_eq!(pvp_err.code, "PvpTimerActive");
    }

    #[tokio::test]
    async fn test_crates_claim_loot_and_milestones_one_time_rewards() {
        // parity: feat:pve.crates, rpc:crateService.OpenCrate, rpc:crateService.ClaimLoot, rpc:crateService.ClaimFitting, rpc:crateService.PeekInsideFixedCrate
        eve_testkit::attest(&[
            "feat:pve.crates",
            "rpc:crateService.OpenCrate",
            "rpc:crateService.ClaimLoot",
            "rpc:crateService.ClaimFitting",
            "rpc:crateService.PeekInsideFixedCrate",
        ]);

        let ctx = ServerContext::new();
        let char_id = 90_000_001;
        let station_id = 60_003_760;
        let mut sess = setup_pilot(&ctx, 8004, "demo", "demo-secret", char_id).await;
        sess.station_id = Some(station_id);

        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&char_id) {
                ch.station_id = Some(station_id);
            }
            let test_crates = [
                (8001_i64, 50_036), // Amaranthine Divide Facial Augmentation Package (fixed loot)
                (8002_i64, 53_533), // Thrasher Combat Crate (skill bundle 39)
                (8003_i64, 53_530), // Coercer Combat Crate (ship fitting, Coercer 16236)
            ];
            for (crate_id, type_id) in test_crates {
                snap.items.insert(
                    crate_id,
                    eve_db::InventoryItemRecord {
                        item_id: crate_id,
                        type_id,
                        owner_id: char_id,
                        location_id: station_id,
                        flag: eve_db::InventoryFlag::Hangar,
                        quantity: 1,
                        singleton: true,
                        ..Default::default()
                    },
                );
            }
        });

        // 1. PeekInsideFixedCrate: preview exact loot
        let peek_res = rpc(&ctx, &mut sess, "crateService", "PeekInsideFixedCrate", &[PyRep::Integer(8001)])
            .await
            .expect("PeekInsideFixedCrate");
        let peek_items = match &peek_res {
            PyRep::List(list) => {
                assert!(!list.is_empty(), "PeekInsideFixedCrate returns preview list");
                list.clone()
            }
            other => panic!("Expected List from PeekInsideFixedCrate, got {:?}", other),
        };

        // 2. Open Crate 8001
        let open_res = rpc(&ctx, &mut sess, "crateService", "OpenCrate", &[PyRep::Integer(8001)])
            .await
            .expect("OpenCrate");
        assert_eq!(open_res.dict_get("crateID").and_then(|v| v.as_i64()), Some(8001));
        assert_eq!(open_res.dict_get("opened").and_then(|v| v.as_bool()), Some(true));
        let loot_items = open_res.dict_get("lootItems").expect("lootItems in OpenCrate response");

        // Peek and Open loot items match (fixedLootItems from SDE crates.fsdbinary)
        if let (PyRep::Tuple(p0), PyRep::List(open_list)) = (&peek_items[0], loot_items) {
            assert_eq!(p0[0], PyRep::Integer(50051));
            assert_eq!(p0[1], PyRep::Integer(1));
            assert_eq!(open_list[0].dict_get("typeID").and_then(|v| v.as_i64()), Some(50051));
            assert_eq!(open_list[0].dict_get("quantity").and_then(|v| v.as_i64()), Some(1));
        }

        // 3. Claim Loot from 8001
        let claim_res = rpc(&ctx, &mut sess, "crateService", "ClaimLoot", &[PyRep::Integer(8001)])
            .await
            .expect("ClaimLoot");
        assert_eq!(claim_res.dict_get("crateID").and_then(|v| v.as_i64()), Some(8001));
        let delivered_item_id = claim_res
            .dict_get("deliveredItemID")
            .and_then(|v| v.as_i64())
            .expect("deliveredItemID");

        // Verify claimed items delivered to station hangar in snapshot
        ctx.db.with_snapshot(|snap| {
            assert!(!snap.items.contains_key(&8001), "Claimed crate consumed");
            let delivered = snap.items.get(&delivered_item_id).expect("Delivered item exists");
            assert_eq!(delivered.type_id, 50051);
            assert_eq!(delivered.quantity, 1);
            assert_eq!(delivered.owner_id, char_id);
            assert_eq!(delivered.location_id, station_id);
            assert_eq!(delivered.flag, eve_db::InventoryFlag::Hangar);
        });

        // Second claim of same crate loot must fail (idempotency / one-time)
        let repeat_loot = rpc(&ctx, &mut sess, "crateService", "ClaimLoot", &[PyRep::Integer(8001)])
            .await
            .expect_err("Crate loot must only be claimable once");
        assert!(matches!(
            repeat_loot.code.as_str(),
            "CrateNotOpened" | "LootAlreadyClaimed" | "CrateLootAlreadyClaimed"
        ));

        // 4. ClaimFitting: Open Crate 8003 and claim ship fitting
        let _ = rpc(&ctx, &mut sess, "crateService", "OpenCrate", &[PyRep::Integer(8003)])
            .await
            .expect("OpenCrate 8003");
        let bad_fit = rpc(
            &ctx,
            &mut sess,
            "crateService",
            "ClaimFitting",
            &[PyRep::Integer(8003), PyRep::Integer(999)],
        )
        .await
        .expect_err("Invalid fitting ID must fail");
        assert_eq!(bad_fit.code, "InvalidCrateFitting");

        let fit_res = rpc(
            &ctx,
            &mut sess,
            "crateService",
            "ClaimFitting",
            &[PyRep::Integer(8003), PyRep::Integer(17)],
        )
        .await
        .expect("ClaimFitting 8003");
        assert_eq!(fit_res.dict_get("crateID").and_then(|v| v.as_i64()), Some(8003));
        assert_eq!(fit_res.dict_get("fittingID").and_then(|v| v.as_i64()), Some(17));
        assert_eq!(fit_res.dict_get("claimed").and_then(|v| v.as_bool()), Some(true));
        let ship_item_id = fit_res
            .dict_get("deliveredItemID")
            .and_then(|v| v.as_i64())
            .expect("deliveredItemID");

        // Verify ship delivered cleanly packaged without fitted modules
        ctx.db.with_snapshot(|snap| {
            assert!(!snap.items.contains_key(&8003), "Crate 8003 consumed by ClaimFitting");
            let ship = snap.items.get(&ship_item_id).expect("Delivered ship in hangar");
            assert_eq!(ship.type_id, 16236); // Coercer hull (from Coercer Combat Crate 53530)
            assert_eq!(ship.owner_id, char_id);
            assert_eq!(ship.location_id, station_id);
            assert_eq!(ship.flag, eve_db::InventoryFlag::Hangar);
            assert!(!ship.singleton, "Ship delivered as packaged hull");

            let fitted_mods: Vec<_> = snap
                .items
                .values()
                .filter(|it| it.location_id == ship_item_id)
                .collect();
            assert!(fitted_mods.is_empty(), "Packaged ship must have no fitted modules");
        });

        // Repeat fitting claim must fail
        let repeat_fit = rpc(
            &ctx,
            &mut sess,
            "crateService",
            "ClaimFitting",
            &[PyRep::Integer(8003), PyRep::Integer(17)],
        )
        .await
        .expect_err("Repeat ClaimFitting must fail");
        assert_eq!(repeat_fit.code, "FittingAlreadyClaimed");

        // 5. Claim Skill Bundle from Crate 8002
        let _ = rpc(&ctx, &mut sess, "crateService", "OpenCrate", &[PyRep::Integer(8002)])
            .await
            .expect("OpenCrate 8002");
        let sp_before = ctx.db.with_snapshot(|snap| {
            snap.characters.get(&char_id).map(|c| c.free_skill_points).unwrap_or(0.0)
        });
        let bundle_res = rpc(
            &ctx,
            &mut sess,
            "crateService",
            "ClaimSkillBundle",
            &[PyRep::Integer(8002), PyRep::Integer(39)],
        )
        .await
        .expect("ClaimSkillBundle");
        assert_eq!(bundle_res.dict_get("crateID").and_then(|v| v.as_i64()), Some(8002));
        assert_eq!(bundle_res.dict_get("skillBundleID").and_then(|v| v.as_i64()), Some(39));

        let sp_after = ctx.db.with_snapshot(|snap| {
            snap.characters.get(&char_id).map(|c| c.free_skill_points).unwrap_or(0.0)
        });
        assert_eq!(sp_after - sp_before, 25_000.0);

        // Second claim of same skill bundle must fail
        let repeat_bundle = rpc(
            &ctx,
            &mut sess,
            "crateService",
            "ClaimSkillBundle",
            &[PyRep::Integer(8002), PyRep::Integer(1)],
        )
        .await
        .expect_err("Skill bundle must only be claimable once");
        assert!(matches!(
            repeat_bundle.code.as_str(),
            "CrateNotOpened" | "SkillBundleAlreadyClaimed"
        ));

        // 6. Milestone Logon & Claim (ClaimRewards fails closed with DATA GAP UnimplementedRPC)
        let logon_res = rpc(&ctx, &mut sess, "milestoneMgr", "ProcessCharacterLogon", &[])
            .await
            .expect("ProcessCharacterLogon");
        assert_eq!(logon_res, PyRep::None);

        let claim_err = rpc(&ctx, &mut sess, "milestoneMgr", "ClaimRewards", &[PyRep::Integer(1)])
            .await
            .expect_err("ClaimRewards must fail closed (DATA GAP)");
        assert_eq!(claim_err.code, "UnimplementedRPC");
    }
}

pub(crate) mod air_npe_state_persistence_career {
    // Parity closure integration tests for PvE features (part 2):
    // - AIR NPE & Career Program: `feat:pve.air-npe`, `feat:pve.air-career-program`
    // - Daily & Seasonal Login campaigns: `feat:pve.login-campaign`, `feat:pve.seasonal-login`
    // - Agency Content Provider: `feat:pve.agency-content`
    // - COSMOS constellations, storyline agents & unique sites: `feat:pve.cosmos`

    use crate::common;
    use eve_db::MiningLedgerRecord;
    use eve_machonet::SessionState;
    use eve_proto::{BridgeRpcError, PyRep};
    use eve_server::router::ServerContext;
    use std::collections::HashMap;

    async fn setup_pilot(
        ctx: &ServerContext,
        session_id: u64,
        username: &str,
        password: &str,
        char_id: i64,
    ) -> SessionState {
        let kw = HashMap::new();
        let mut sess = SessionState::new(session_id);
        common::login_pilot(ctx, &mut sess, username, password).await;
        ctx.dispatch_rpc(
            &mut sess,
            "charUnboundMgr",
            None,
            "SelectCharacterID",
            &[PyRep::Integer(char_id)],
            &kw,
            None,
        )
        .await
        .expect("SelectCharacterID");
        sess
    }

    async fn rpc(
        ctx: &ServerContext,
        sess: &mut SessionState,
        service: &str,
        method: &str,
        args: &[PyRep],
    ) -> Result<PyRep, BridgeRpcError> {
        let kw = HashMap::new();
        ctx.dispatch_rpc(sess, service, None, method, args, &kw, None)
            .await
    }

    #[tokio::test]
    async fn test_air_npe_state_persistence_and_career_goals() {
        // parity: feat:pve.air-npe, feat:pve.air-career-program
        eve_testkit::attest(&[
            "feat:pve.air-npe",
            "feat:pve.air-career-program",
        ]);

        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);
        let char_id = 90_000_001;
        let mut sess = setup_pilot(&ctx, 8003, "demo", "demo-secret", char_id).await;

        // 1. Initial AIR NPE state
        let init_state = rpc(&ctx, &mut sess, "air_npe", "get_air_npe_state", &[])
            .await
            .expect("get_air_npe_state initial");
        assert_eq!(init_state.dict_get("nodeGraphID").and_then(|v| v.as_i64()), Some(473));
        assert_eq!(init_state.dict_get("currentNodeID").and_then(|v| v.as_i64()), Some(1));

        // 2. Skip AIR NPE
        let skip_state = rpc(&ctx, &mut sess, "air_npe", "skip_air_npe", &[])
            .await
            .expect("skip_air_npe");
        assert_eq!(skip_state.dict_get("currentNodeID").and_then(|v| v.as_i64()), Some(4));
        assert_eq!(skip_state.dict_get("completed").and_then(|v| v.as_bool()), Some(true));

        // 3. Verify persistence of completed state
        let get_state = rpc(&ctx, &mut sess, "air_npe", "get_air_npe_state", &[])
            .await
            .expect("get_air_npe_state after skip");
        assert_eq!(get_state.dict_get("currentNodeID").and_then(|v| v.as_i64()), Some(4));
        assert_eq!(get_state.dict_get("completed").and_then(|v| v.as_bool()), Some(true));

        // 4. AIR Career Program Goal Progress (Mining Goal 2)
        let initial_goal = rpc(
            &ctx,
            &mut sess,
            "seasonManager",
            "get_character_progress_by_character_id",
            &[PyRep::Integer(2)],
        )
        .await
        .expect("get_character_progress initial");
        assert_eq!(
            initial_goal.dict_get("currentProgress").and_then(|v| v.as_i64()),
            Some(0)
        );

        // Record server-observed mining events in the mining ledger
        ctx.db.with_mut_snapshot(|snap| {
            snap.mining_ledger.push(MiningLedgerRecord {
                character_id: char_id,
                solar_system_id: 30_000_142,
                type_id: 1230, // Veldspar
                quantity: 1_000,
                date: "2026-01-01".into(),
                timestamp_unix: 1_704_067_200,
            });
        });

        // Verify progress updated from server events
        let updated_goal = rpc(
            &ctx,
            &mut sess,
            "seasonManager",
            "get_character_progress_by_character_id",
            &[PyRep::Integer(2)],
        )
        .await
        .expect("get_character_progress after mining");
        assert_eq!(
            updated_goal.dict_get("currentProgress").and_then(|v| v.as_i64()),
            Some(1000)
        );

        // Claim goal reward
        let sp_before = ctx.db.with_snapshot(|snap| {
            snap.characters.get(&char_id).map(|c| c.free_skill_points).unwrap_or(0.0)
        });
        let claim_res = rpc(
            &ctx,
            &mut sess,
            "seasonManager",
            "claim_goal_reward",
            &[PyRep::Integer(2)],
        )
        .await
        .expect("claim_goal_reward");
        assert_eq!(claim_res.dict_get("goalID").and_then(|v| v.as_i64()), Some(2));
        assert_eq!(
            claim_res.dict_get("spReward").and_then(|v| v.as_f64()),
            Some(50_000.0)
        );

        let sp_after = ctx.db.with_snapshot(|snap| {
            snap.characters.get(&char_id).map(|c| c.free_skill_points).unwrap_or(0.0)
        });
        assert!(sp_after > sp_before, "SP reward must be credited to character");

        // Second claim must be rejected
        let repeat_claim = rpc(
            &ctx,
            &mut sess,
            "seasonManager",
            "claim_goal_reward",
            &[PyRep::Integer(2)],
        )
        .await
        .expect_err("Duplicate claim must be rejected");
        // rpc-lint: allow error code check
        assert_eq!(repeat_claim.code, "GoalAlreadyClaimed");
    }

    #[tokio::test]
    async fn test_daily_and_seasonal_login_campaign_rewards() {
        // DATA GAP: requires live login campaign schedule

        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);
        let char_id = 90_000_001;
        let mut sess = setup_pilot(&ctx, 8005, "demo", "demo-secret", char_id).await;

        let init_items = ctx.db.with_snapshot(|s| s.items.len());

        // 1. Daily login campaign (SDE campaign 2261)
        let state = rpc(&ctx, &mut sess, "loginCampaignManager", "get_client_campaign_state", &[])
            .await
            .expect("get_client_campaign_state");
        assert_eq!(state.dict_get("campaignID").and_then(|v| v.as_i64()), Some(2261));
        assert_eq!(state.dict_get("canClaimToday").and_then(|v| v.as_bool()), Some(true));

        let claim1 = rpc(&ctx, &mut sess, "loginCampaignManager", "claim_reward", &[])
            .await
            .expect("claim_reward day 1");
        let PyRep::Tuple(claim1_tuple) = &claim1 else {
            panic!("expected Tuple from claim_reward day 1");
        };
        assert_eq!(claim1_tuple[0].dict_get("reward_index").and_then(|v| v.as_i64()), Some(1));
        assert_eq!(claim1_tuple[0].dict_get("typeID").and_then(|v| v.as_i64()), Some(80_937));
        assert!(claim1_tuple[0].dict_get("itemID").and_then(|v| v.as_i64()).is_some());

        // Duplicate claim on same day returns None (nothing to claim)
        let repeat_day1 = rpc(&ctx, &mut sess, "loginCampaignManager", "claim_reward", &[])
            .await
            .expect("Duplicate daily claim returns None");
        assert_eq!(repeat_day1, PyRep::None);

        // Advance clock by 1 full day (86,400 seconds)
        ctx.advance_virtual_clock(86_400);

        let state_day2 = rpc(&ctx, &mut sess, "loginCampaignManager", "get_client_campaign_state", &[])
            .await
            .expect("get_client_campaign_state day 2");
        assert_eq!(state_day2.dict_get("canClaimToday").and_then(|v| v.as_bool()), Some(true));

        let claim2 = rpc(&ctx, &mut sess, "loginCampaignManager", "claim_reward", &[])
            .await
            .expect("claim_reward day 2");
        let PyRep::Tuple(claim2_tuple) = &claim2 else {
            panic!("expected Tuple from claim_reward day 2");
        };
        assert_eq!(claim2_tuple[0].dict_get("reward_index").and_then(|v| v.as_i64()), Some(2));
        assert_eq!(claim2_tuple[0].dict_get("typeID").and_then(|v| v.as_i64()), Some(82_052));

        // 2. Seasonal login campaign (SDE campaign 2323 scheduled via GM /campaign)
        let mut gm_sess = SessionState::new(8099);
        gm_sess.user_id = Some(999);
        gm_sess.character_id = Some(char_id);
        gm_sess.role = eve_machonet::service_roles::ROLE_GMH;
        rpc(&ctx, &mut gm_sess, "slash", "SlashCmd", &[PyRep::String("/campaign seasonal 2323 30 11:00:00".into())])
            .await
            .expect("GM schedule seasonal campaign 2323");
        let active_seasonal = rpc(
            &ctx,
            &mut sess,
            "seasonalLoginCampaignManager",
            "get_active_campaign",
            &[],
        )
        .await
        .expect("get_active_campaign");
        let PyRep::Tuple(seasonal_tuple) = &active_seasonal else {
            panic!("expected Tuple from get_active_campaign");
        };
        assert_eq!(
            seasonal_tuple[0].as_i64(),
            Some(2323)
        );
        assert!(seasonal_tuple[1].as_i64().unwrap_or(0) > 730_000);
        assert_eq!(seasonal_tuple[2].as_str(), Some("11:00:00"));
        assert_eq!(seasonal_tuple[3].as_i64(), Some(30));

        let seasonal_claim = rpc(
            &ctx,
            &mut sess,
            "seasonalLoginCampaignManager",
            "claim_reward",
            &[],
        )
        .await
        .expect("seasonal claim_reward");
        assert_eq!(
            seasonal_claim,
            PyRep::Bool(true)
        );

        // Verify all 6 SDE reward items (day 1 alpha+omega, day 2 alpha+omega, seasonal day 1 alpha+omega) were delivered
        ctx.db.with_snapshot(|s| {
            assert_eq!(s.items.len(), init_items + 6);
            for expected_type in [80_937, 49_809, 82_052, 29_266, 87_900] {
                assert!(
                    s.items.values().any(|it| {
                        it.owner_id == char_id
                            && it.location_id == 60_003_760
                            && it.flag == eve_db::InventoryFlag::Hangar
                            && it.type_id == expected_type
                    }),
                    "missing delivered SDE reward type {expected_type}"
                );
            }
        });

        // Duplicate seasonal claim on same day must fail
        let repeat_seasonal = rpc(
            &ctx,
            &mut sess,
            "seasonalLoginCampaignManager",
            "claim_reward",
            &[],
        )
        .await
        .expect_err("Duplicate seasonal claim on same day must fail");
        // rpc-lint: allow error code check
        assert_eq!(repeat_seasonal.code, "SeasonalRewardAlreadyClaimedToday");

        let history = rpc(
            &ctx,
            &mut sess,
            "seasonalLoginCampaignManager",
            "get_claim_history",
            &[],
        )
        .await
        .expect("get_claim_history");
        let PyRep::Dict(days) = history else {
            panic!("Expected dict from get_claim_history");
        };
        assert!(!days.is_empty(), "Claim history must record claimed day");
    }

    #[tokio::test]
    async fn test_custom_agency_provider_content_nearby_sites() {
        // parity: feat:pve.agency-content
        eve_testkit::attest(&["feat:pve.agency-content"]);

        let ctx = ServerContext::new();
        let char_id = 90_000_001;
        let sys_id = 30_002_053; // HEK
        let mut sess = setup_pilot(&ctx, 8006, "demo", "demo-secret", char_id).await;

        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&char_id) {
                ch.solar_system_id = sys_id;
                ch.station_id = None;
                ch.structure_id = None;
            }
        });
        sess.solar_system_id = Some(sys_id);
        sess.station_id = None;
        sess.structure_id = None;

        let content = rpc(&ctx, &mut sess, "custom_agency_provider", "get_content_data", &[])
            .await
            .expect("get_content_data");
        let PyRep::List(entries) = content else {
            panic!("Expected list from get_content_data");
        };
        assert!(!entries.is_empty(), "Agency must return nearby system content");

        let mut has_combat = false;
        let mut has_sig = false;
        for entry in entries {
            if entry.dict_get("solarSystemID").and_then(|v| v.as_i64()) == Some(sys_id as i64) {
                match entry.dict_get("contentType").and_then(|v| v.as_str()) {
                    Some("combatAnomaly") => has_combat = true,
                    Some("signature") => has_sig = true,
                    _ => {}
                }
            }
        }
        assert_eq!(has_combat, true, "Agency content must include combat anomalies");
        assert_eq!(has_sig, true, "Agency content must include signatures");
    }

    #[tokio::test]
    async fn test_cosmos_constellations_storyline_agents_and_sites() {
        // parity: feat:pve.cosmos
        eve_testkit::attest(&["feat:pve.cosmos"]);

        let ctx = ServerContext::new();
        let char_id = 90_000_001;
        let mut sess = setup_pilot(&ctx, 8007, "demo", "demo-secret", char_id).await;

        // 1. Verify COSMOS constellations in SDE contain storyline agents
        let vaarma = ctx.sde.constellations.get(&20_000_449).expect("Vaarma constellation");
        assert_eq!(vaarma.name, "Vaarma");

        let cosmos_agents_in_vaarma: Vec<i64> = ctx
            .sde
            .agents
            .values()
            .filter(|a| {
                a.agent_type_id == 8
                    && a.solar_system_id
                        .and_then(|sid| ctx.sde.solar_systems.get(&sid))
                        .map(|s| s.constellation_id == 20_000_449)
                        .unwrap_or(false)
            })
            .map(|a| a.agent_id)
            .collect();
        assert!(
            !cosmos_agents_in_vaarma.is_empty(),
            "Vaarma COSMOS constellation must contain COSMOS storyline agents"
        );

        // 2. Verify unique COSMOS sites in SDE
        let cosmos_dungeons: Vec<&eve_sde::DungeonTemplate> = ctx
            .sde
            .dungeons
            .values()
            .filter(|d| d.archetype_id == Some(26) || d.name.contains("Contested"))
            .collect();
        assert!(!cosmos_dungeons.is_empty(), "Unique COSMOS sites must exist in SDE");
        let has_caldari_mining = cosmos_dungeons
            .iter()
            .any(|d| d.name.contains("Contested Caldari Mining Facilities"));
        assert!(
            has_caldari_mining,
            "Contested Caldari Mining Facilities COSMOS site must exist in SDE"
        );

        // 3. COSMOS Storyline Agent Interaction (COSMOS Agent 3018629 in Vaarma)
        let agent_id = 3_018_629;
        let cosmos_agent = ctx.sde.agents.get(&agent_id).expect("COSMOS agent 3018629");
        assert_eq!(cosmos_agent.agent_type_id, 8, "Agent type must be 8 (COSMOS)");
        let agent_corp_id = cosmos_agent.corporation_id;
        let agent_sys_id = cosmos_agent.solar_system_id.expect("Agent system");
        let agent_station_id = cosmos_agent.station_id.expect("Agent station");

        // Colocate pilot with agent and set standing >= 2.0
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&char_id) {
                ch.solar_system_id = agent_sys_id;
                ch.station_id = Some(agent_station_id);
                ch.structure_id = None;
                ch.standings.insert(agent_corp_id, 5.0);
            }
        });
        sess.solar_system_id = Some(agent_sys_id);
        sess.station_id = Some(agent_station_id);
        sess.structure_id = None;

        // Request mission offer from COSMOS agent
        let offer = rpc(&ctx, &mut sess, "agentMgr", "DoAction", &[PyRep::Integer(1), PyRep::Integer(agent_id)])
            .await
            .expect("COSMOS agent offer");
        let mission_id = offer
            .dict_get("missionID")
            .and_then(|v| v.as_i64())
            .expect("missionID") as u32;

        // Accept mission
        rpc(&ctx, &mut sess, "agentMgr", "DoAction", &[PyRep::Integer(3), PyRep::Integer(mission_id as i64)])
            .await
            .expect("Accept COSMOS mission");

        // Complete mission
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(m) = snap.active_missions.get_mut(&(char_id, mission_id)) {
                m.state = "Completed".into();
            }
        });

        // Subsequent mission requests must be rejected due to one-time completion
        let repeat_err = rpc(&ctx, &mut sess, "agentMgr", "DoAction", &[PyRep::Integer(1), PyRep::Integer(agent_id)])
            .await
            .expect_err("Repeat COSMOS agent mission must be rejected");
        assert_eq!(repeat_err.code, "OneTimeAgentMissionCompleted");
    }
}
