//! Integration regression tests for atomic superweapon resource, Crimewatch safety,
//! and structure vulnerability/war validation (`activate_superweapon_with_args`).

#![forbid(unsafe_code)]

use crate::common::*;
use eve_db::{ContractRecord, InventoryFlag, InventoryItemRecord, IskCents, UpwellStructureRecord};
use eve_destiny::{Ball, MovementMode, TargetLock};
use eve_sde::attr_ids;
use eve_server::router::ServerContext;
use eve_server::services::combat_crimewatch::{with_mut_combat_runtime, SafetyLevel};
use eve_server::services::tactical_modules::{
    activate_superweapon_with_args, check_ship_navigation_lock,
};
use eve_server::sp6_lifecycle::with_sp6_state;
use glam::DVec3;
use std::sync::Arc;

const CHAR_PRIMARY: i64 = 90_000_001;
const CHAR_HOSTILE: i64 = 90_000_002;
const CHAR_BYSTANDER: i64 = 90_000_003;

fn ensure_third_pilot(ctx: &ServerContext) {
    ctx.db.with_mut_snapshot(|snap| {
        if snap.characters.contains_key(&CHAR_BYSTANDER) {
            return;
        }
        snap.accounts.insert(
            "bystander".into(),
            eve_db::AccountRecord {
                user_id: 3,
                username: "bystander".into(),
                password_token: eve_db::WorldRepository::hash_password_token(
                    "bystander",
                    "bystander-secret",
                ),
                role: eve_db::service_roles::ROLE_PLAYER_DEFAULT,
                banned_until_unix: None,
                ban_reason: None,
            },
        );
        let mut ch = snap.characters[&CHAR_HOSTILE].clone();
        ch.character_id = CHAR_BYSTANDER;
        ch.user_id = 3;
        ch.name = "Bystander Pilot".into();
        ch.corporation_id = 1_000_010;
        snap.characters.insert(CHAR_BYSTANDER, ch);
    });
}

fn seed_stack(
    ctx: &ServerContext,
    item_id: i64,
    type_id: u32,
    owner_id: i64,
    location_id: i64,
    flag: InventoryFlag,
    quantity: i32,
    singleton: bool,
) {
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            item_id,
            InventoryItemRecord {
                item_id,
                type_id,
                owner_id,
                location_id,
                flag,
                quantity,
                singleton,
                version: 1,
                ..Default::default()
            },
        );
    });
}

fn item_qty(ctx: &ServerContext, item_id: i64) -> i32 {
    ctx.db
        .with_snapshot(|snap| snap.items.get(&item_id).map(|i| i.quantity).unwrap_or(0))
}

fn set_pilot_safety(ctx: &ServerContext, char_id: i64, safety_level: u8) {
    let level = match safety_level {
        0 => SafetyLevel::Red,
        1 => SafetyLevel::Yellow,
        _ => SafetyLevel::Green,
    };
    with_mut_combat_runtime(ctx, |rt| {
        rt.safety_levels.insert(char_id, level);
    });
    let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
    let _ = cw.get_or_create(char_id);
}

fn grant_skills_for(ctx: &ServerContext, char_id: i64, type_ids: &[u32]) {
    ctx.db.with_mut_snapshot(|snap| {
        let entry = snap.skills.entry(char_id).or_default();
        for &tid in type_ids {
            if let Some(t) = ctx.sde.types.get(&tid) {
                for aid in [
                    attr_ids::REQUIRED_SKILL_1,
                    attr_ids::REQUIRED_SKILL_2,
                    attr_ids::REQUIRED_SKILL_3,
                ] {
                    if let Some(&req) = t.default_attributes.get(&aid) {
                        let req_id = req as u32;
                        if req_id > 0 {
                            entry.insert(
                                req_id,
                                eve_db::TrainedSkillRecord {
                                    type_id: req_id,
                                    level: 5,
                                    skill_points: 256_000.0,
                                },
                            );
                        }
                    }
                }
            }
        }
    });
}

async fn spawn_combat_vessel(
    ctx: &ServerContext,
    sys_id: u32,
    ship_id: i64,
    type_id: u32,
    owner_id: i64,
    pos: DVec3,
    cap: f64,
) {
    let sys = ctx.systems.get_or_spawn(sys_id);
    let t = &ctx.sde.types[&type_id];
    let mut cs = ctx.build_type_combat_state(type_id);
    cs.capacitor = cap;
    cs.max_capacitor = cap;
    let meta = ctx.build_ship_slim_meta(owner_id, type_id);
    sys.spawn_ball_with_meta(
        Ball {
            id: ship_id,
            type_id,
            position: pos,
            velocity: DVec3::ZERO,
            max_velocity: 200.0,
            inertia: 1.5,
            mass: t.mass.max(10_000_000.0),
            radius: t.radius.max(100.0),
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
        cs,
        meta,
    )
    .await
    .expect("spawn_combat_vessel");
}

async fn add_locked_target(ctx: &ServerContext, sys_id: u32, attacker: i64, target: i64) {
    let sys = ctx.systems.get_or_spawn(sys_id);
    let _ = sys
        .add_target_lock(
            attacker,
            TargetLock {
                target_id: target,
                lock_time_ms: 500.0,
                remaining_lock_ms: 0.0,
                locked: true,
                max_range_m: Some(500_000.0),
            },
        )
        .await;
}

fn has_weapon_or_criminal_timer(ctx: &ServerContext, char_id: i64) -> bool {
    let now = ctx.now_unix_secs();
    let (cw_weap, cw_crim) = ctx
        .crimewatch
        .read()
        .ok()
        .and_then(|cw| {
            cw.states
                .get(&char_id)
                .map(|s| (s.weapon_timer_until_unix, s.criminal_flag_until_unix))
        })
        .unwrap_or((0, 0));
    let (sp_weap, sp_crim) = with_sp6_state(&ctx.db, |st| {
        (
            st.weapon_timers_until.get(&char_id).copied().unwrap_or(0),
            st.criminal_timers_until.get(&char_id).copied().unwrap_or(0),
        )
    });
    cw_weap > now || cw_crim > now || sp_weap > now || sp_crim > now
}

fn make_structure(
    id: i64,
    type_id: u32,
    corp_id: i64,
    sys_id: u32,
    state: &str,
    reinforced_until: u64,
) -> UpwellStructureRecord {
    UpwellStructureRecord {
        structure_id: id,
        type_id,
        owner_corp_id: corp_id,
        solar_system_id: sys_id,
        name: format!("TestStructure-{id}"),
        position: [15_000.0, 0.0, 0.0],
        state: state.into(),
        quantum_core_installed: true,
        fuel_blocks: 1_000,
        shield_hp: 1.0,
        armor_hp: 1.0,
        hull_hp: 1.0,
        reinforced_until_unix: reinforced_until,
        last_damage_tick: 0,
        last_damage_unix: 0,
        self_destruct_at_unix: None,
    }
}

#[tokio::test]
async fn lancer_disruptive_lance_lowsec_safety_and_multi_stack_fuel_rollback() {
    let ctx = Arc::new(ServerContext::new());
    pin_clock(&ctx);
    ensure_third_pilot(&ctx);

    let lowsec_sys: u32 = 30_000_142;
    with_mut_combat_runtime(&ctx, |rt| {
        rt.modified_system_securities.insert(lowsec_sys, 0.3);
    });

    let (lancer_id, siege_id, lance_id) = (891_100_001, 891_100_002, 891_100_003);
    let (fuel_stack_a, fuel_stack_b, fuel_locked) = (891_100_010, 891_100_011, 891_100_012);
    let (victim_green_id, victim_yellow_id) = (891_100_020, 891_100_021);

    ctx.db.with_mut_snapshot(|snap| {
        for (cid, sid) in [
            (CHAR_PRIMARY, lancer_id),
            (CHAR_HOSTILE, victim_green_id),
            (CHAR_BYSTANDER, victim_yellow_id),
        ] {
            let ch = snap.characters.get_mut(&cid).unwrap();
            ch.station_id = None;
            ch.solar_system_id = lowsec_sys;
            ch.active_ship_id = sid;
        }
        snap.contracts.insert(
            88_100_001,
            ContractRecord {
                contract_id: 88_100_001,
                issuer_id: CHAR_PRIMARY,
                acceptor_id: None,
                contract_type: "ItemExchange".into(),
                status: "Outstanding".into(),
                start_station_id: 60_003_760,
                end_station_id: 60_003_760,
                price: IskCents::ZERO,
                reward: IskCents::ZERO,
                collateral: IskCents::ZERO,
                item_ids: vec![fuel_locked],
                expiration_unix: u64::MAX,
                issuer_corp_id: 1_000_009,
                assignee_id: 0,
                availability: "Public".into(),
                buyout: IskCents::ZERO,
                current_bid: IskCents::ZERO,
                current_bidder_id: None,
                requested_items: vec![],
                bid_history: vec![],
                date_issued_unix: 0,
                title: "Locked fuel".into(),
                for_corp: false,
                num_days: 7,
            },
        );
    });

    grant_skills_for(&ctx, CHAR_PRIMARY, &[77_283, 4_292, 77_399]);
    let lance_fuel_req = ctx.sde.types[&77_399]
        .default_attributes
        .get(&714)
        .copied()
        .unwrap_or(250.0)
        .ceil() as i32;
    let stack_a_init = (lance_fuel_req - 2).max(1);
    seed_stack(&ctx, lancer_id, 77_283, CHAR_PRIMARY, lowsec_sys as i64, InventoryFlag::None, 1, true);
    seed_stack(&ctx, siege_id, 4_292, CHAR_PRIMARY, lancer_id, InventoryFlag::HiSlot0, 1, true);
    seed_stack(&ctx, lance_id, 77_399, CHAR_PRIMARY, lancer_id, InventoryFlag::HiSlot1, 1, true);
    seed_stack(&ctx, fuel_stack_a, 16_274, CHAR_PRIMARY, lancer_id, InventoryFlag::SpecializedFuelBay, stack_a_init, false);
    seed_stack(&ctx, fuel_stack_b, 16_274, CHAR_PRIMARY, lancer_id, InventoryFlag::Cargo, 5, false);
    seed_stack(&ctx, fuel_locked, 16_274, CHAR_PRIMARY, lancer_id, InventoryFlag::Cargo, 10_000, false);

    seed_stack(&ctx, victim_green_id, 19_720, CHAR_HOSTILE, lowsec_sys as i64, InventoryFlag::None, 1, true);
    seed_stack(&ctx, victim_yellow_id, 587, CHAR_BYSTANDER, lowsec_sys as i64, InventoryFlag::None, 1, true);

    let init_cap = 50_000.0;
    spawn_combat_vessel(&ctx, lowsec_sys, lancer_id, 77_283, CHAR_PRIMARY, DVec3::ZERO, init_cap).await;
    spawn_combat_vessel(
        &ctx,
        lowsec_sys,
        victim_green_id,
        19_720,
        CHAR_HOSTILE,
        DVec3::new(15_000.0, 0.0, 0.0),
        40_000.0,
    )
    .await;
    spawn_combat_vessel(
        &ctx,
        lowsec_sys,
        victim_yellow_id,
        587,
        CHAR_BYSTANDER,
        DVec3::new(18_000.0, 0.0, 0.0),
        1_000.0,
    )
    .await;

    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(siege_id, true);
        snap.module_active_states.insert(siege_id);
        snap.module_online_states.insert(lance_id, true);
    });
    ctx.invalidate_ship_fitting(lancer_id);
    add_locked_target(&ctx, lowsec_sys, lancer_id, victim_green_id).await;

    let sys = ctx.systems.get_or_spawn(lowsec_sys);
    let victim_shield_before = sys
        .get_combat_state(victim_green_id)
        .await
        .unwrap()
        .unwrap()
        .shield_hp;

    // 1. Green safety in lowsec fails atomically before deducting fuel/cap or setting timers
    set_pilot_safety(&ctx, CHAR_PRIMARY, 2);
    set_pilot_safety(&ctx, CHAR_HOSTILE, 2);
    set_pilot_safety(&ctx, CHAR_BYSTANDER, 1);

    let err_green = activate_superweapon_with_args(
        &ctx,
        CHAR_PRIMARY,
        lancer_id,
        lowsec_sys,
        lance_id,
        "Activate",
        Some(victim_green_id),
        &[],
    )
    .await
    .expect_err("Green safety must block lowsec Disruptive Lance");
    assert_eq!(err_green.code, "CantDoInCurrentSafetyLevel");
    assert_eq!(item_qty(&ctx, fuel_stack_a), stack_a_init);
    assert_eq!(item_qty(&ctx, fuel_stack_b), 5);
    assert_eq!(item_qty(&ctx, fuel_locked), 10_000);
    assert_eq!(sys.get_combat_state(lancer_id).await.unwrap().unwrap().capacitor, init_cap);
    assert!(!has_weapon_or_criminal_timer(&ctx, CHAR_PRIMARY));
    assert!(check_ship_navigation_lock(&ctx, lancer_id, "warp").is_ok());

    // 2. Yellow safety in lowsec against lawful Capsule (Criminal act) also rolls back atomically
    let victim_pod_id: i64 = 891_100_029;
    seed_stack(&ctx, victim_pod_id, 670, CHAR_BYSTANDER, lowsec_sys as i64, InventoryFlag::None, 1, true);
    spawn_combat_vessel(
        &ctx,
        lowsec_sys,
        victim_pod_id,
        670,
        CHAR_BYSTANDER,
        DVec3::new(16_000.0, 0.0, 0.0),
        500.0,
    )
    .await;
    add_locked_target(&ctx, lowsec_sys, lancer_id, victim_pod_id).await;
    set_pilot_safety(&ctx, CHAR_PRIMARY, 1);
    let err_yellow = activate_superweapon_with_args(
        &ctx,
        CHAR_PRIMARY,
        lancer_id,
        lowsec_sys,
        lance_id,
        "Activate",
        Some(victim_pod_id),
        &[],
    )
    .await
    .expect_err("Yellow safety must block Disruptive Lance criminal act");
    let _ = sys.remove_ball(victim_pod_id).await;
    assert_eq!(err_yellow.code, "CantDoInCurrentSafetyLevel");
    assert_eq!(item_qty(&ctx, fuel_stack_a), stack_a_init);
    assert_eq!(item_qty(&ctx, fuel_stack_b), 5);
    assert_eq!(item_qty(&ctx, fuel_locked), 10_000);
    assert_eq!(sys.get_combat_state(lancer_id).await.unwrap().unwrap().capacitor, init_cap);
    assert!(!has_weapon_or_criminal_timer(&ctx, CHAR_PRIMARY));

    // 3. Red safety succeeds, spans multi-stack fuel (133 -> 0, 5 -> 3), leaves locked stack untouched
    set_pilot_safety(&ctx, CHAR_PRIMARY, 0);
    activate_superweapon_with_args(
        &ctx,
        CHAR_PRIMARY,
        lancer_id,
        lowsec_sys,
        lance_id,
        "Activate",
        Some(victim_green_id),
        &[],
    )
    .await
    .expect("Red safety Disruptive Lance fires in lowsec");
    assert_eq!(item_qty(&ctx, fuel_stack_a), 0);
    assert_eq!(item_qty(&ctx, fuel_stack_b), 3);
    assert_eq!(item_qty(&ctx, fuel_locked), 10_000);
    assert!(sys.get_combat_state(lancer_id).await.unwrap().unwrap().capacitor < init_cap);
    assert!(
        sys.get_combat_state(victim_green_id).await.unwrap().unwrap().shield_hp
            < victim_shield_before
    );
    assert!(check_ship_navigation_lock(&ctx, lancer_id, "warp").is_err());
    assert!(check_ship_navigation_lock(&ctx, victim_yellow_id, "warp").is_err());
}

#[tokio::test]
async fn titan_doomsday_atomic_fuel_capacitor_and_timer_preservation_across_invalid_targets() {
    let ctx = Arc::new(ServerContext::new());
    pin_clock(&ctx);

    let lowsec_sys: u32 = 30_000_144;
    let highsec_sys: u32 = 30_000_142;
    let now = ctx.now_unix_secs();
    with_mut_combat_runtime(&ctx, |rt| {
        rt.modified_system_securities.insert(lowsec_sys, -0.5);
    });

    let (titan_id, dd_id, fuel_id, hostile_cap_id) =
        (891_200_001, 891_200_002, 891_200_003, 891_200_010);
    let (st_reinf, st_friendly, st_highsec, st_skyhook, st_mden, st_ihub, st_sovhub) = (
        1_091_200_001,
        1_091_200_002,
        1_091_200_003,
        1_091_200_004,
        1_091_200_005,
        1_091_200_006,
        1_091_200_007,
    );

    ctx.db.with_mut_snapshot(|snap| {
        let ch1 = snap.characters.get_mut(&CHAR_PRIMARY).unwrap();
        ch1.station_id = None;
        ch1.solar_system_id = lowsec_sys;
        ch1.active_ship_id = titan_id;
        ch1.corporation_id = 1_000_009;

        let ch2 = snap.characters.get_mut(&CHAR_HOSTILE).unwrap();
        ch2.station_id = None;
        ch2.solar_system_id = lowsec_sys;
        ch2.active_ship_id = hostile_cap_id;
        ch2.corporation_id = 1_000_011;

        snap.structures.insert(
            st_reinf,
            make_structure(st_reinf, 35_832, 1_000_011, lowsec_sys, "ArmorReinforced", now + 3_600),
        );
        snap.structures.insert(
            st_friendly,
            make_structure(st_friendly, 35_832, 1_000_009, lowsec_sys, "ShieldVulnerable", 0),
        );
        snap.structures.insert(
            st_highsec,
            make_structure(st_highsec, 35_832, 1_000_011, highsec_sys, "ShieldVulnerable", 0),
        );
        snap.structures.insert(
            st_skyhook,
            make_structure(
                st_skyhook,
                eve_server::services::sov_services::TYPE_ORBITAL_SKYHOOK,
                1_000_011,
                lowsec_sys,
                "ArmorReinforced",
                now + 3_600,
            ),
        );
        snap.structures.insert(
            st_mden,
            make_structure(
                st_mden,
                eve_server::services::sov_gameplay::TYPE_MERCENARY_DEN,
                1_000_011,
                lowsec_sys,
                "ArmorReinforced",
                now + 3_600,
            ),
        );
        snap.sov_runtime
            .mercenary_den_reinforcement
            .insert(st_mden, (now + 3_600, 0, 100));
        snap.structures.insert(
            st_ihub,
            make_structure(
                st_ihub,
                eve_server::services::fw_gameplay::TYPE_FW_IHUB,
                1_000_011,
                lowsec_sys,
                "ShieldVulnerable",
                0,
            ),
        );
        snap.fw_system_occupations.insert(
            lowsec_sys,
            eve_db::FwSystemOccupationRecord {
                solar_system_id: lowsec_sys,
                owner_faction_id: 500_001,
                occupier_faction_id: 500_001,
                victory_points: 0,
                victory_points_threshold: 100,
            },
        );
        snap.structures.insert(
            st_sovhub,
            make_structure(
                st_sovhub,
                eve_server::services::sov_services::TYPE_SOVEREIGNTY_HUB,
                1_000_011,
                lowsec_sys,
                "ShieldVulnerable",
                0,
            ),
        );
    });

    grant_skills_for(&ctx, CHAR_PRIMARY, &[11_567, 24_550]);
    seed_stack(&ctx, titan_id, 11_567, CHAR_PRIMARY, lowsec_sys as i64, InventoryFlag::None, 1, true);
    seed_stack(&ctx, dd_id, 24_550, CHAR_PRIMARY, titan_id, InventoryFlag::HiSlot0, 1, true);
    seed_stack(&ctx, fuel_id, 16_274, CHAR_PRIMARY, titan_id, InventoryFlag::SpecializedFuelBay, 75_000, false);
    seed_stack(&ctx, hostile_cap_id, 19_720, CHAR_HOSTILE, lowsec_sys as i64, InventoryFlag::None, 1, true);

    let init_cap = 120_000.0;
    spawn_combat_vessel(&ctx, lowsec_sys, titan_id, 11_567, CHAR_PRIMARY, DVec3::ZERO, init_cap).await;
    spawn_combat_vessel(
        &ctx,
        lowsec_sys,
        hostile_cap_id,
        19_720,
        CHAR_HOSTILE,
        DVec3::new(12_000.0, 0.0, 0.0),
        60_000.0,
    )
    .await;
    set_pilot_safety(&ctx, CHAR_PRIMARY, 0);

    let sys = ctx.systems.get_or_spawn(lowsec_sys);
    for (tid, expected_code) in [
        (st_reinf, "StructureReinforcedInvulnerable"),
        (st_friendly, "CannotAttackFriendlyStructure"),
        (st_skyhook, "StructureInvulnerable"),
        (st_mden, "MercenaryDenReinforced"),
        (st_ihub, "IHubInvulnerable"),
        (st_sovhub, "StructureInvulnerable"),
    ] {
        add_locked_target(&ctx, lowsec_sys, titan_id, tid).await;
        let err = activate_superweapon_with_args(
            &ctx,
            CHAR_PRIMARY,
            titan_id,
            lowsec_sys,
            dd_id,
            "Activate",
            Some(tid),
            &[],
        )
        .await
        .expect_err("invulnerable/friendly/entosis-only structure must fail atomically");
        assert_eq!(err.code, expected_code);
        assert_eq!(item_qty(&ctx, fuel_id), 75_000);
        assert_eq!(sys.get_combat_state(titan_id).await.unwrap().unwrap().capacitor, init_cap);
        assert!(!has_weapon_or_criminal_timer(&ctx, CHAR_PRIMARY));
        assert!(check_ship_navigation_lock(&ctx, titan_id, "warp").is_ok());
    }

    // Highsec unwarred Upwell structure also fails atomically
    ctx.db.with_mut_snapshot(|snap| {
        let mut t_item = snap.items.get(&titan_id).cloned().unwrap();
        t_item.location_id = highsec_sys as i64;
        snap.items.insert(titan_id, t_item);
        snap.characters.get_mut(&CHAR_PRIMARY).unwrap().solar_system_id = highsec_sys;
    });
    spawn_combat_vessel(&ctx, highsec_sys, titan_id, 11_567, CHAR_PRIMARY, DVec3::ZERO, init_cap).await;
    add_locked_target(&ctx, highsec_sys, titan_id, st_highsec).await;
    let err_hs = activate_superweapon_with_args(
        &ctx,
        CHAR_PRIMARY,
        titan_id,
        highsec_sys,
        dd_id,
        "Activate",
        Some(st_highsec),
        &[],
    )
    .await
    .expect_err("highsec structure without mutual war must fail atomically");
    assert_eq!(err_hs.code, "CannotActivateDoomsdayInHighSec");
    assert_eq!(item_qty(&ctx, fuel_id), 75_000);
    assert_eq!(
        ctx.systems
            .get_or_spawn(highsec_sys)
            .get_combat_state(titan_id)
            .await
            .unwrap()
            .unwrap()
            .capacitor,
        init_cap
    );

    // Move back to lowsec and fire Doomsday at locked hostile Dreadnought
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&titan_id).unwrap().location_id = lowsec_sys as i64;
        snap.characters.get_mut(&CHAR_PRIMARY).unwrap().solar_system_id = lowsec_sys;
    });
    add_locked_target(&ctx, lowsec_sys, titan_id, hostile_cap_id).await;
    activate_superweapon_with_args(
        &ctx,
        CHAR_PRIMARY,
        titan_id,
        lowsec_sys,
        dd_id,
        "Activate",
        Some(hostile_cap_id),
        &[],
    )
    .await
    .expect("valid Doomsday against hostile Dreadnought succeeds");
    assert_eq!(item_qty(&ctx, fuel_id), 25_000);
    assert!(sys.get_combat_state(titan_id).await.unwrap().unwrap().capacitor < init_cap);
    assert!(check_ship_navigation_lock(&ctx, titan_id, "warp").is_err());
}
