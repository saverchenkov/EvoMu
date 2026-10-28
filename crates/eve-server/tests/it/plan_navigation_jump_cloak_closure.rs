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
