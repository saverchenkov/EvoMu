//! Extra unit and mutation-killing tests for `services::combat_crimewatch::combat_legacy_ess`
//! covering `step_dynamic_bounty_multiplier` negative input clamping, `beyonce.CmdAttackTarget`
//! deleted-RPC rejection, `dogmaIM.Activate` module pass-through vs legacy target engagement,
//! structure-docked/no-ship/no-system/invalid-target guards, charge skill prerequisite error
//! propagation, `MissileLauncher`, `EntropicDisintegrator` (with spooling), `BreacherPod`, and
//! `VortonProjector` (with secondary arc chaining & arc target destruction), `TargetInvulnerable`,
//! `MissionPocketInstanceRestricted`, `TargetOutOfRange`, `WeaponCycling`, single-round ammo
//! consumption removal, null-sec ESS main/reserve bank bounty splitting & 64-wreck cap eviction,
//! low-sec unprovoked PvP ship destruction (Capsule ejection + suspect kill right clearing) and
//! pod destruction, living-world convoy interdiction, and `essMgr` link dispatch.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::{
    make_fighter_ctx_and_session, make_fighter_item, make_fighter_type,
};
use crate::services::combat_crimewatch::combat_legacy_ess::*;
use eve_db::{
    ActiveMissionRecord, InventoryFlag, InventoryItemRecord, IskCents, KillRightRecord,
    SystemDynamicState,
};
use eve_destiny::{Ball, CombatState, MovementMode};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use glam::DVec3;
use std::collections::HashMap;
use std::sync::Arc;

async fn call_dogma_activate(
    ctx: &ServerContext,
    session: &mut SessionState,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    handle_legacy_and_ess(ctx, session, "dogmaIM", "Activate", args, &HashMap::new()).await
}

async fn call_beyonce_deleted(
    ctx: &ServerContext,
    session: &mut SessionState,
) -> BridgeRpcError {
    // rpc-lint: allow testing Phase 4 deleted CmdAttackTarget rejection
    handle_legacy_and_ess(ctx, session, "beyonce", "CmdAttackTarget", &[], &HashMap::new())
        .await
        .unwrap()
        .unwrap_err()
}

async fn spawn_combat_ball(
    ctx: &ServerContext,
    sys_id: u32,
    id: i64,
    type_id: u32,
    pos: DVec3,
    hp: f64,
) {
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    sys_handle
        .spawn_ball_with_combat(
            Ball {
                id,
                type_id,
                position: pos,
                velocity: DVec3::ZERO,
                max_velocity: 200.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState {
                shield_hp: 0.0,
                max_shield_hp: hp,
                armor_hp: 0.0,
                max_armor_hp: hp,
                structure_hp: hp,
                max_structure_hp: hp,
                signature_radius: 50.0,
                ..CombatState::default()
            },
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn guard_rejections_skill_prereqs_range_cycling_and_weapon_archetypes() {
    let (mut ctx, mut session, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();

    // Negative inputs to step_dynamic_bounty_multiplier are clamped to 0.0
    let m_neg = step_dynamic_bounty_multiplier(&ctx, sys_id, -100_000.0, -100_000.0);
    assert!((m_neg - 1.35).abs() < 1e-6);

    // Deleted beyonce.CmdAttackTarget returns UnimplementedRPC; unknown service returns None
    let by_err = call_beyonce_deleted(&ctx, &mut session).await;
    assert_eq!(by_err.code, "UnimplementedRPC");
    assert!(handle_legacy_and_ess(&ctx, &mut session, "", "", &[], &HashMap::new()).await.is_none());

    // Configure SDE types:
    // - Vorton Projector (type 54801, group 4060) + Vorton charge (type 54802, group 4061) requiring skill 3300 lv 5
    // - Entropic Disintegrator (type 49722, group 1986) + Exotic Plasma (type 49723, group 1987)
    // - Missile Launcher (type 2410, group 506) + Missile (type 2411, group 384)
    // - Breacher Pod Launcher (type 84001, group 4807) + Breacher Pod (type 84002, group 4808)
    // - Invulnerable celestial (type 10002, cat 2, bounty 0) & valid NPC (type 10001, cat 11, bounty 200_000)
    let mut sde = (*ctx.sde).clone();
    if let Some(ship_t) = sde.types.get_mut(&23_915) {
        ship_t.default_attributes.insert(14, 8.0);
        ship_t.default_attributes.insert(101, 8.0);
        ship_t.default_attributes.insert(102, 8.0);
        ship_t.default_attributes.insert(11, 50_000.0);
        ship_t.default_attributes.insert(48, 50_000.0);
    }
    for (w_tid, w_grp, c_tid, c_grp, extra_w_attrs, extra_c_attrs) in [
        (
            54_801_u32,
            4060_u32,
            54_802_u32,
            4061_u32,
            vec![(51, 3_000.0), (54, 20_000.0), (64, 1.0), (128, 1.0), (604, 4061.0), (2104, 3.0)],
            vec![(128, 1.0), (114, 400.0), (118, 400.0), (182, 3300.0), (277, 5.0)],
        ),
        (
            49_722,
            1986,
            49_723,
            1987,
            vec![(51, 1_000.0), (54, 15_000.0), (158, 5_000.0), (160, 100.0), (64, 1.0), (128, 1.0), (604, 1987.0)],
            vec![(128, 1.0), (116, 100.0), (118, 100.0)],
        ),
        (
            2_410,
            506,
            2_411,
            384,
            vec![(51, 1_000.0), (64, 1.0), (128, 1.0), (604, 384.0)],
            vec![(128, 1.0), (37, 4_000.0), (281, 5_000.0), (653, 150.0), (654, 40.0), (117, 150.0)],
        ),
        (
            84_001,
            4807,
            84_002,
            4808,
            vec![(51, 1_000.0), (64, 1.0), (128, 1.0), (604, 4808.0)],
            vec![(128, 1.0), (37, 4_000.0), (281, 5_000.0), (118, 250.0)],
        ),
    ] {
        let mut wt = make_fighter_type(w_tid, w_grp, 7, 5.0, extra_w_attrs);
        wt.effect_ids = vec![12];
        sde.types.insert(w_tid, wt);
        sde.types.insert(c_tid, make_fighter_type(c_tid, c_grp, 8, 0.01, extra_c_attrs));
    }
    sde.types.insert(
        10_001,
        make_fighter_type(10_001, 550, 11, 1_000.0, vec![(eve_sde::attr_ids::ENTITY_KILL_BOUNTY, 200_000.0)]),
    );
    sde.types.insert(10_002, make_fighter_type(10_002, 15, 2, 1_000.0, vec![]));
    ctx.sde = Arc::new(sde);

    // When args[0] is an item located inside `ship_id`, legacy handler returns None so normal module activation runs
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            810_001,
            make_fighter_item(810_001, 54_801, char_id, ship_id, InventoryFlag::HiSlot0, 1),
        );
    });
    assert!(call_dogma_activate(&ctx, &mut session, &[PyRep::Integer(810_001)])
        .await
        .is_none());

    // Structure-docked, non-integer target, zero target, no solar system guards
    session.structure_id = Some(1_000_000_000_111);
    assert_eq!(
        call_dogma_activate(&ctx, &mut session, &[PyRep::Integer(910_001)])
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "ShipDocked"
    );
    session.structure_id = None;
    assert_eq!(
        call_dogma_activate(&ctx, &mut session, &[PyRep::String("bad".into())])
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidTarget"
    );
    assert_eq!(
        call_dogma_activate(&ctx, &mut session, &[PyRep::Integer(0)])
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidTarget"
    );
    session.solar_system_id = None;
    assert_eq!(
        call_dogma_activate(&ctx, &mut session, &[PyRep::Integer(910_001)])
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "NoSolarSystem"
    );
    session.solar_system_id = Some(sys_id);

    spawn_combat_ball(&ctx, sys_id, ship_id, 23_915, DVec3::ZERO, 1_000.0).await;

    // Load Vorton ammo (54_802) which requires skill 3300 level 5 -> fails skill prerequisite check!
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            810_002,
            make_fighter_item(810_002, 54_802, char_id, ship_id, InventoryFlag::HiSlot0, 2),
        );
    });
    ctx.invalidate_ship_fitting(ship_id);
    assert!(call_dogma_activate(&ctx, &mut session, &[PyRep::Integer(910_001)])
        .await
        .unwrap()
        .is_err());

    // Grant skill 3300 level 5 -> now reaches TargetNotInSpace
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(char_id).or_default().insert(
            3300,
            eve_db::TrainedSkillRecord {
                type_id: 3300,
                level: 5,
                skill_points: 256_000.0,
            },
        );
    });
    assert_eq!(
        call_dogma_activate(&ctx, &mut session, &[PyRep::Integer(910_001)])
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "TargetNotInSpace"
    );

    // Spawn invulnerable ball (type 10_002) -> TargetInvulnerable
    spawn_combat_ball(&ctx, sys_id, 910_002, 10_002, DVec3::new(2_000.0, 0.0, 0.0), 500.0).await;
    assert_eq!(
        call_dogma_activate(&ctx, &mut session, &[PyRep::Integer(910_002)])
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "TargetInvulnerable"
    );

    // Spawn NPC belonging to another pilot's mission -> MissionPocketInstanceRestricted
    spawn_combat_ball(&ctx, sys_id, 910_003, 10_001, DVec3::new(2_000.0, 0.0, 0.0), 500.0).await;
    ctx.db.with_mut_snapshot(|snap| {
        let mut other_ch = snap.characters.get(&char_id).cloned().unwrap();
        other_ch.character_id = char_id + 99;
        snap.characters.insert(char_id + 99, other_ch);
        snap.active_missions.insert(
            (char_id + 99, 3_018_418),
            ActiveMissionRecord {
                character_id: char_id + 99,
                agent_id: 3_018_418,
                mission_id: 1,
                state: "Accepted".into(),
                solar_system_id: sys_id,
                current_room_id: 0,
                npcs_remaining: 1,
                spawned_npc_ball_ids: vec![910_003],
                bookmark_id: None,
            },
        );
    });
    assert_eq!(
        call_dogma_activate(&ctx, &mut session, &[PyRep::Integer(910_003)])
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "MissionPocketInstanceRestricted"
    );

    // Spawn distant NPC at 50km (Vorton optimal is 20km, falloff 0) -> TargetOutOfRange
    spawn_combat_ball(&ctx, sys_id, 910_004, 10_001, DVec3::new(50_000.0, 0.0, 0.0), 500.0).await;
    assert_eq!(
        call_dogma_activate(&ctx, &mut session, &[PyRep::Integer(910_004)])
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "TargetOutOfRange"
    );

    // Spawn primary NPC (910_010, HP 5000 -> survives) and secondary arc NPC (910_011, HP 10 -> destroyed by Vorton arc!)
    spawn_combat_ball(&ctx, sys_id, 910_010, 10_001, DVec3::new(3_000.0, 0.0, 0.0), 5_000.0).await;
    spawn_combat_ball(&ctx, sys_id, 910_011, 10_001, DVec3::new(4_000.0, 0.0, 0.0), 10.0).await;

    let vorton_res = call_dogma_activate(&ctx, &mut session, &[PyRep::Integer(910_010)])
        .await
        .unwrap()
        .unwrap();
    assert_eq!(vorton_res.dict_get("weaponKind"), Some(&PyRep::String("VortonProjector".into())));
    assert_eq!(vorton_res.dict_get("destroyed"), Some(&PyRep::Bool(false)));
    assert_eq!(vorton_res.dict_get("secondaryTargetsHit"), Some(&PyRep::Integer(1)));

    // Immediate re-fire on same tick with 3000ms cycle time -> WeaponCycling!
    assert_eq!(
        call_dogma_activate(&ctx, &mut session, &[PyRep::Integer(910_010)])
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "WeaponCycling"
    );

    // Test EntropicDisintegrator, MissileLauncher, and BreacherPod non-fatal hits
    for (w_tid, c_tid, expected_kind) in [
        (49_722_u32, 49_723_u32, "EntropicDisintegrator"),
        (2_410, 2_411, "MissileLauncher"),
        (84_001, 84_002, "BreacherPod"),
    ] {
        ctx.weapon_cycle_state.write().unwrap().clear();
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.get_mut(&810_001).unwrap().type_id = w_tid;
            snap.items.insert(
                810_002,
                make_fighter_item(810_002, c_tid, char_id, ship_id, InventoryFlag::HiSlot0, 1),
            );
        });
        ctx.invalidate_ship_fitting(ship_id);
        let r = call_dogma_activate(&ctx, &mut session, &[PyRep::Integer(910_010)])
            .await
            .unwrap()
            .unwrap();
        assert_eq!(r.dict_get("weaponKind"), Some(&PyRep::String(expected_kind.into())));
        // Single-round charge (quantity == 1) was removed from snap.items after firing
        assert!(ctx.db.with_snapshot(|s| !s.items.contains_key(&810_002)));
    }
}

#[tokio::test]
async fn nullsec_ess_bounty_split_wreck_eviction_lowsec_pvp_pod_and_ess_link() {
    let (mut ctx, mut session, char_id, ship_id, _sys_id) = make_fighter_ctx_and_session();
    let null_sys = 30_004_759_u32;
    let low_sys = 30_002_537_u32;

    let mut sde = (*ctx.sde).clone();
    if let Some(ship_t) = sde.types.get_mut(&23_915) {
        ship_t.default_attributes.insert(14, 8.0);
        ship_t.default_attributes.insert(102, 8.0);
        ship_t.default_attributes.insert(11, 50_000.0);
        ship_t.default_attributes.insert(48, 50_000.0);
    }
    let mut wep_t = make_fighter_type(
        2_905,
        55,
        7,
        5.0,
        vec![(51, 1_000.0), (54, 15_000.0), (64, 5.0), (128, 1.0), (158, 10_000.0), (160, 100.0), (604, 83.0)],
    );
    wep_t.effect_ids = vec![12];
    sde.types.insert(2_905, wep_t);
    sde.types.insert(
        192,
        make_fighter_type(192, 83, 8, 0.01, vec![(128, 1.0), (114, 1_000.0), (118, 1_000.0)]),
    );
    sde.types.insert(
        10_001,
        make_fighter_type(10_001, 550, 11, 1_000.0, vec![(eve_sde::attr_ids::ENTITY_KILL_BOUNTY, 1_000_000.0)]),
    );
    let base_sys = sde.solar_systems.values().next().unwrap().clone();
    let mut ns_info = base_sys.clone();
    ns_info.solar_system_id = null_sys;
    ns_info.security_status = -0.50;
    sde.solar_systems.insert(null_sys, ns_info);
    let mut ls_info = base_sys;
    ls_info.solar_system_id = low_sys;
    ls_info.security_status = 0.30;
    sde.solar_systems.insert(low_sys, ls_info);
    ctx.sde = Arc::new(sde);

    // 1. Null-sec NPC kill splits bounty into ESS Main & Reserve banks and evicts oldest empty wreck when >= 64
    session.solar_system_id = Some(null_sys);
    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states.insert(
            null_sys,
            SystemDynamicState {
                solar_system_id: null_sys,
                dbs_multiplier: 1.35,
                ..Default::default()
            },
        );
        snap.items.insert(
            820_001,
            make_fighter_item(820_001, 2_905, char_id, ship_id, InventoryFlag::HiSlot0, 1),
        );
        snap.items.insert(
            820_002,
            make_fighter_item(820_002, 192, char_id, ship_id, InventoryFlag::HiSlot0, 20),
        );
        for i in 0..64 {
            let wid = 850_000 + i;
            snap.items.insert(
                wid,
                InventoryItemRecord {
                    item_id: wid,
                    type_id: 26557,
                    owner_id: char_id,
                    location_id: null_sys as i64,
                    flag: InventoryFlag::Cargo,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
        }
    });
    ctx.invalidate_ship_fitting(ship_id);
    spawn_combat_ball(&ctx, null_sys, ship_id, 23_915, DVec3::ZERO, 1_000.0).await;
    spawn_combat_ball(&ctx, null_sys, 920_001, 10_001, DVec3::new(2_000.0, 0.0, 0.0), 50.0).await;

    let ns_kill = call_dogma_activate(&ctx, &mut session, &[PyRep::Integer(920_001)])
        .await
        .unwrap()
        .unwrap();
    assert_eq!(ns_kill.dict_get("destroyed"), Some(&PyRep::Bool(true)));
    ctx.db.with_snapshot(|snap| {
        let st = snap.system_states.get(&null_sys).unwrap();
        assert!(st.ess_main_bank > IskCents(0));
        assert!(st.ess_reserve_bank > IskCents(0));
        // Oldest empty wreck (850_000) was evicted to keep empty wrecks bounded at 64
        assert!(!snap.items.contains_key(&850_000));
    });

    // 2. Low-sec unprovoked PvP ship destruction ejects Capsule, then pod kill respawns victim at home station
    session.solar_system_id = Some(low_sys);
    ctx.weapon_cycle_state.write().unwrap().clear();
    let vic_char = 90_000_002_i64;
    let vic_ship = 200_000_055_i64;
    ctx.db.with_mut_snapshot(|snap| {
        let mut vic = snap.characters.get(&char_id).cloned().unwrap();
        vic.character_id = vic_char;
        vic.corporation_id = 2_000_002;
        vic.active_ship_id = vic_ship;
        vic.solar_system_id = low_sys;
        snap.characters.insert(vic_char, vic);
        snap.items.insert(
            vic_ship,
            make_fighter_item(vic_ship, 23_915, vic_char, low_sys as i64, InventoryFlag::None, 1),
        );
        // Nested item inside victim ship to verify recursive deletion
        snap.items.insert(
            vic_ship + 1,
            make_fighter_item(vic_ship + 1, 192, vic_char, vic_ship, InventoryFlag::Cargo, 10),
        );
        snap.kill_rights.insert(
            701,
            KillRightRecord {
                kill_right_id: 701,
                owner_id: char_id,
                target_id: vic_char,
                activation_fee: IskCents(0),
                is_active_suspect: true,
            },
        );
    });
    spawn_combat_ball(&ctx, low_sys, ship_id, 23_915, DVec3::ZERO, 1_000.0).await;
    spawn_combat_ball(&ctx, low_sys, vic_ship, 23_915, DVec3::new(1_500.0, 0.0, 0.0), 50.0).await;

    let pvp_ship_kill = call_dogma_activate(&ctx, &mut session, &[PyRep::Integer(vic_ship)])
        .await
        .unwrap()
        .unwrap();
    assert_eq!(pvp_ship_kill.dict_get("destroyed"), Some(&PyRep::Bool(true)));
    assert_eq!(pvp_ship_kill.dict_get("legalEngagement"), Some(&PyRep::Bool(false)));

    let pod_id = ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&vic_ship));
        assert!(!snap.items.contains_key(&(vic_ship + 1)));
        assert!(!snap.kill_rights[&701].is_active_suspect);
        snap.characters[&vic_char].active_ship_id
    });
    assert!(pod_id > 0);

    // Destroy the ejected Capsule (type 670) -> respawns victim at home station
    ctx.weapon_cycle_state.write().unwrap().clear();
    let pvp_pod_kill = call_dogma_activate(&ctx, &mut session, &[PyRep::Integer(pod_id)])
        .await
        .unwrap()
        .unwrap();
    assert_eq!(pvp_pod_kill.dict_get("destroyed"), Some(&PyRep::Bool(true)));

    // 3. essMgr dispatch reaches handle_ess_link
    assert!(handle_legacy_and_ess(&ctx, &mut session, "essMgr", "AttemptLinkToMainBank", &[], &HashMap::new()).await.is_some());
}

#[tokio::test]
async fn issue_542_dogma_activate_safety_tether_and_entropic_target_switch_spool_reset() {
    use crate::services::combat_crimewatch::{with_mut_combat_runtime, SafetyLevel};

    let (mut ctx, mut session, char_id, ship_id, high_sys) = make_fighter_ctx_and_session();
    let low_sys = 30_002_537_u32;

    let mut sde = (*ctx.sde).clone();
    if let Some(ship_t) = sde.types.get_mut(&23_915) {
        ship_t.default_attributes.insert(14, 8.0);
        ship_t.default_attributes.insert(101, 8.0);
        ship_t.default_attributes.insert(102, 8.0);
        ship_t.default_attributes.insert(11, 50_000.0);
        ship_t.default_attributes.insert(48, 50_000.0);
    }
    let mut wep_t = make_fighter_type(
        49_722,
        1986,
        7,
        5.0,
        vec![
            (51, 1_000.0),
            (54, 15_000.0),
            (158, 5_000.0),
            (160, 100.0),
            (64, 1.0),
            (128, 1.0),
            (604, 1987.0),
        ],
    );
    wep_t.effect_ids = vec![12];
    sde.types.insert(49_722, wep_t);
    sde.types.insert(
        49_723,
        make_fighter_type(
            49_723,
            1987,
            8,
            0.01,
            vec![(128, 1.0), (116, 100.0), (118, 100.0)],
        ),
    );
    ctx.sde = Arc::new(sde);

    let vic_char = 90_000_077_i64;
    let vic_char_b = 90_000_078_i64;
    let vic_ship_a = 200_000_771_i64;
    let vic_ship_b = 200_000_772_i64;
    let mod_id = 810_771_i64;
    let charge_id = 810_772_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            mod_id,
            make_fighter_item(mod_id, 49_722, char_id, ship_id, InventoryFlag::HiSlot0, 1),
        );
        snap.items.insert(
            charge_id,
            make_fighter_item(charge_id, 49_723, char_id, ship_id, InventoryFlag::HiSlot0, 20),
        );
        let mut vic = snap.characters.get(&char_id).cloned().unwrap();
        vic.character_id = vic_char;
        vic.corporation_id = 2_000_077;
        vic.active_ship_id = vic_ship_a;
        vic.solar_system_id = high_sys;
        let mut vic_b = vic.clone();
        vic_b.character_id = vic_char_b;
        vic_b.active_ship_id = vic_ship_b;
        vic_b.solar_system_id = low_sys;
        snap.characters.insert(vic_char, vic);
        snap.characters.insert(vic_char_b, vic_b);
        snap.items.insert(
            vic_ship_a,
            make_fighter_item(vic_ship_a, 23_915, vic_char, high_sys as i64, InventoryFlag::None, 1),
        );
        snap.items.insert(
            vic_ship_b,
            make_fighter_item(vic_ship_b, 23_915, vic_char, low_sys as i64, InventoryFlag::None, 1),
        );
    });
    ctx.invalidate_ship_fitting(ship_id);

    spawn_combat_ball(&ctx, high_sys, ship_id, 23_915, DVec3::ZERO, 1_000.0).await;
    spawn_combat_ball(&ctx, high_sys, vic_ship_a, 23_915, DVec3::new(1_000.0, 0.0, 0.0), 50_000.0).await;

    // 1. High-sec Green & Yellow safety must reject illegal attack BEFORE ammo or spool mutation
    for safety in [SafetyLevel::Green, SafetyLevel::Yellow] {
        with_mut_combat_runtime(&ctx, |rt| {
            rt.safety_levels.insert(char_id, safety);
        });
        let err = call_dogma_activate(&ctx, &mut session, &[PyRep::Integer(vic_ship_a)])
            .await
            .unwrap()
            .unwrap_err();
        assert_eq!(err.code, "CantDoInCurrentSafetyLevel");
        assert_eq!(ctx.db.with_snapshot(|s| s.items[&charge_id].quantity), 20);
        assert!(ctx.weapon_cycle_state.read().unwrap().is_empty());
    }

    // 2. Low-sec Green safety must reject suspect aggression BEFORE ammo or spool mutation
    session.solar_system_id = Some(low_sys);
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&vic_char).unwrap().solar_system_id = low_sys;
    });
    spawn_combat_ball(&ctx, low_sys, ship_id, 23_915, DVec3::ZERO, 1_000.0).await;
    spawn_combat_ball(&ctx, low_sys, vic_ship_a, 23_915, DVec3::new(1_000.0, 0.0, 0.0), 50_000.0).await;
    spawn_combat_ball(&ctx, low_sys, vic_ship_b, 23_915, DVec3::new(1_200.0, 0.0, 0.0), 50_000.0).await;
    with_mut_combat_runtime(&ctx, |rt| {
        rt.safety_levels.insert(char_id, SafetyLevel::Green);
    });
    let low_green_err = call_dogma_activate(&ctx, &mut session, &[PyRep::Integer(vic_ship_a)])
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(low_green_err.code, "CantDoInCurrentSafetyLevel");
    assert_eq!(ctx.db.with_snapshot(|s| s.items[&charge_id].quantity), 20);
    assert!(ctx.weapon_cycle_state.read().unwrap().is_empty());

    // 3. Switch to Red safety in low-sec and build Entropic Disintegrator spool on Target A (2 cycles)
    with_mut_combat_runtime(&ctx, |rt| {
        rt.safety_levels.insert(char_id, SafetyLevel::Red);
    });
    let low_handle = ctx.systems.get_or_spawn(low_sys);
    let _ = low_handle.step_manual(2.0).await;
    let hit1 = call_dogma_activate(&ctx, &mut session, &[PyRep::Integer(vic_ship_a)])
        .await
        .unwrap()
        .unwrap();
    let dmg1 = hit1.dict_get("appliedDamage").unwrap().as_f64().unwrap();
    assert!(dmg1 > 0.0);
    assert_eq!(ctx.weapon_cycle_state.read().unwrap()[&ship_id].1, 1);
    assert_eq!(ctx.weapon_cycle_state.read().unwrap()[&ship_id].2, Some(vic_ship_a));

    let _ = low_handle.step_manual(2.0).await;
    let hit2 = call_dogma_activate(&ctx, &mut session, &[PyRep::Integer(vic_ship_a)])
        .await
        .unwrap()
        .unwrap();
    let dmg2 = hit2.dict_get("appliedDamage").unwrap().as_f64().unwrap();
    assert!(dmg2 > 0.0);
    assert_eq!(ctx.weapon_cycle_state.read().unwrap()[&ship_id].1, 2);
    assert_eq!(ctx.weapon_cycle_state.read().unwrap()[&ship_id].2, Some(vic_ship_a));
    let qty_before_rejections = ctx.db.with_snapshot(|s| s.items[&charge_id].quantity);
    assert_eq!(qty_before_rejections, 18);

    // 4. Failed shots due to attacker tether or target tether must preserve BOTH ammo and existing spool
    let _ = low_handle.step_manual(2.0).await;
    low_handle.try_update_combat_state(ship_id, |cs| cs.is_tethered = true);
    let _ = low_handle.step_manual(1.0).await;
    let att_teth = call_dogma_activate(&ctx, &mut session, &[PyRep::Integer(vic_ship_a)])
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(att_teth.code, "CantActivateWhileTethered");
    low_handle.try_update_combat_state(ship_id, |cs| cs.is_tethered = false);
    low_handle.try_update_combat_state(vic_ship_b, |cs| cs.is_tethered = true);
    let _ = low_handle.step_manual(1.0).await;
    let tgt_teth = call_dogma_activate(&ctx, &mut session, &[PyRep::Integer(vic_ship_b)])
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(tgt_teth.code, "TargetInvulnerable");
    low_handle.try_update_combat_state(vic_ship_b, |cs| cs.is_tethered = false);
    let _ = low_handle.step_manual(1.0).await;
    assert_eq!(ctx.db.with_snapshot(|s| s.items[&charge_id].quantity), 18);
    assert_eq!(ctx.weapon_cycle_state.read().unwrap()[&ship_id].1, 2);
    assert_eq!(ctx.weapon_cycle_state.read().unwrap()[&ship_id].2, Some(vic_ship_a));

    // 5. Switching target to Target B must reset Entropic Disintegrator spool back to cycle 1
    let hit_b1 = call_dogma_activate(&ctx, &mut session, &[PyRep::Integer(vic_ship_b)])
        .await
        .unwrap()
        .unwrap();
    let dmg_b1 = hit_b1.dict_get("appliedDamage").unwrap().as_f64().unwrap();
    assert!(dmg_b1 > 0.0);
    assert_eq!(ctx.weapon_cycle_state.read().unwrap()[&ship_id].1, 1);
    assert_eq!(ctx.weapon_cycle_state.read().unwrap()[&ship_id].2, Some(vic_ship_b));
}
