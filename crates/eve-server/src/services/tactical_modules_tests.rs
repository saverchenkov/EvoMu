#![forbid(unsafe_code)]

use super::*;
use crate::router::ServerContext;
use crate::system_actor::ActiveModuleCycle;
use eve_db::{
    FleetRecord, InventoryFlag, InventoryItemRecord, TrainedSkillRecord, VirtualServerClock,
    WorldRepository,
};
use eve_destiny::{Ball, CombatState, DamageVector, MovementMode, SlimItemMeta, TargetLock};
use eve_dogma::{DogmaItemState, ShipFittingEngine};
use eve_sde::{InvType, SdeRegistry};
use glam::DVec3;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn custom_inv_type(sde: &SdeRegistry, type_id: u32, group_id: u32, category_id: u32, name: &str, attrs: &[(u16, f64)]) -> InvType {
    let mut t = sde.types.get(&587).expect("seed type").clone();
    t.type_id = type_id;
    t.group_id = group_id;
    t.category_id = category_id;
    t.name = name.into();
    t.default_attributes.clear();
    for &(k, v) in attrs {
        t.default_attributes.insert(k, v);
    }
    t
}

fn test_ctx_with_custom_types<F: FnOnce(&mut SdeRegistry)>(mutate_sde: F) -> ServerContext {
    let mut sde = (*shared_sde()).clone();
    mutate_sde(&mut sde);
    let db = Arc::new(WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(Arc::new(sde), db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_800_000_000)));
    ctx
}

fn ensure_char(ctx: &ServerContext, char_id: i64, corp_id: i64, sys_id: u32, ship_id: i64, clone_grade: u32) {
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().expect("demo seed character");
        ch.character_id = char_id;
        ch.user_id = char_id as u64;
        ch.name = format!("Pilot {char_id}");
        ch.corporation_id = corp_id;
        ch.station_id = None;
        ch.structure_id = None;
        ch.solar_system_id = sys_id;
        ch.active_ship_id = ship_id;
        ch.security_status = 5.0;
        ch.clone_grade = clone_grade;
        ch.fleet_id = None;
        snap.characters.entry(char_id).or_insert(ch);
        snap.accounts.insert(
            format!("user_{char_id}"),
            eve_db::AccountRecord {
                user_id: char_id as u64,
                username: format!("user_{char_id}"),
                password_token: String::new(),
                role: 0,
                banned_until_unix: None,
                ban_reason: None,
            },
        );
    });
    let _ = ctx
        .db
        .acquire_online_lock(char_id as u64, char_id as u64, Some(char_id), true, ctx.now_unix_secs());
}

fn grant_skill(ctx: &ServerContext, char_id: i64, skill_tid: u32, level: u8) {
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(char_id).or_default().insert(
            skill_tid,
            TrainedSkillRecord {
                type_id: skill_tid,
                level,
                skill_points: 256_000.0,
            },
        );
    });
}

fn insert_item(
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
                ..Default::default()
            },
        );
    });
}

async fn spawn_ship_ball_with_cs(
    ctx: &ServerContext,
    sys_id: u32,
    ball_id: i64,
    type_id: u32,
    owner_id: i64,
    pos: DVec3,
    cap: f64,
) {
    let sys = ctx.systems.get_or_spawn(sys_id);
    let ball = Ball {
        id: ball_id,
        type_id,
        position: pos,
        velocity: DVec3::ZERO,
        max_velocity: 2_000.0,
        inertia: 1.0,
        mass: 1_000_000.0,
        radius: 200.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let mut cs = CombatState::default();
    cs.capacitor = cap;
    cs.max_capacitor = cap;
    cs.shield_hp = 5_000.0;
    cs.max_shield_hp = 5_000.0;
    cs.armor_hp = 5_000.0;
    cs.max_armor_hp = 5_000.0;
    cs.structure_hp = 5_000.0;
    cs.max_structure_hp = 5_000.0;
    cs.signature_radius = 250.0;
    let meta = SlimItemMeta {
        group_id: 25,
        category_id: 6,
        owner_id,
        char_id: Some(owner_id),
        corp_id: 98_000_001,
        alliance_id: 0,
        security_status: 5.0,
        is_cloaked: false,
        ..Default::default()
    };
    sys.spawn_ball_with_meta(ball, cs, meta)
        .await
        .expect("spawn ball");
}

#[tokio::test]
async fn navigation_locks_wormhole_prep_and_projected_remote_assistance() {
    let ctx = test_ctx_with_custom_types(|sde| {
        sde.types.insert(
            999_190,
            custom_inv_type(sde, 999_190, 290, 7, "Remote Sensor Booster", &[(54, 40_000.0), (2044, 15_000.0), (309, 25.0), (566, 30.0)]),
        );
        sde.types.insert(
            999_191,
            custom_inv_type(sde, 999_191, 209, 7, "Remote Tracking Computer", &[(54, 40_000.0), (2044, 15_000.0), (351, 20.0)]),
        );
    });
    let char_id = 94_000_001;
    let logi_char_id = 94_000_002;
    let sys_id = 30_002_505;
    let ship_id = 1_000_400_001;
    let logi_ship_id = 1_000_400_002;
    let bastion_mod_id = 1_000_400_003;
    let rsb_mod_id = 1_000_400_004;
    let rtc_mod_id = 1_000_400_005;

    ensure_char(&ctx, char_id, 98_000_001, sys_id, ship_id, 1);
    ensure_char(&ctx, logi_char_id, 98_000_002, sys_id, logi_ship_id, 1);
    insert_item(&ctx, ship_id, 587, char_id, sys_id as i64, InventoryFlag::None, 1, true);
    insert_item(&ctx, logi_ship_id, 587, logi_char_id, sys_id as i64, InventoryFlag::None, 1, true);
    insert_item(&ctx, bastion_mod_id, 33400, char_id, ship_id, InventoryFlag::HiSlot0, 1, true);

    assert!(!is_clone_vat_bay_active_required(&ctx, ship_id));
    assert!(!has_lance_caster_lock(&ctx, ship_id));
    assert_eq!(incoming_remote_repair_multiplier(&ctx, ship_id), 1.0);
    assert!(check_ship_navigation_lock(&ctx, ship_id, "warp").is_ok());

    // Bastion active blocks wormhole jump
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_active_states.insert(bastion_mod_id);
    });
    let sys = ctx.systems.get_or_spawn(sys_id);
    let mut ball = Ball {
        id: ship_id,
        type_id: 587,
        position: DVec3::ZERO,
        velocity: DVec3::ZERO,
        max_velocity: 500.0,
        inertia: 1.0,
        mass: 1_000_000.0,
        radius: 200.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let wh_err = prepare_ship_for_wormhole_jump(&ctx, &sys, char_id, ship_id, &mut ball)
        .await
        .unwrap_err();
    assert_eq!(wh_err.code, "CannotJumpWormholeWhileImmobilized");
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_active_states.remove(&bastion_mod_id);
    });

    // Lance victim debuff reduces incoming remote repair multiplier to 0.50 and blocks navigation
    let db_key = db_ptr_key(&ctx.db);
    let now_unix = ctx.now_unix_secs();
    ctx.tactical_state
        .write()
        .unwrap()
        .lance_victim_debuff_until
        .insert((db_key, ship_id), now_unix + 60);
    assert_eq!(incoming_remote_repair_multiplier(&ctx, ship_id), 0.50);
    assert_eq!(
        check_ship_navigation_lock(&ctx, ship_id, "dock").unwrap_err().code,
        "DisruptiveLanceDebuffActive"
    );

    // Project Remote Sensor Booster & Remote Tracking Computer onto ship_id
    let rsb_type = ctx.sde.types.get(&999_190).unwrap().clone();
    let rsb_state = DogmaItemState::from_sde(logi_char_id, 999_190, &ctx.sde);

    assert_eq!(
        apply_projected_remote_assistance(&ctx, logi_char_id, logi_ship_id, rsb_mod_id, &rsb_type, &rsb_state, None, 1_000.0)
            .unwrap_err()
            .code,
        "TargetRequired"
    );
    assert_eq!(
        apply_projected_remote_assistance(&ctx, logi_char_id, logi_ship_id, rsb_mod_id, &rsb_type, &rsb_state, Some(ship_id), 90_000.0)
            .unwrap_err()
            .code,
        "OutOfRange"
    );
    apply_projected_remote_assistance(&ctx, logi_char_id, logi_ship_id, rsb_mod_id, &rsb_type, &rsb_state, Some(ship_id), 10_000.0)
        .expect("project RSB");

    let rtc_type = ctx.sde.types.get(&999_191).unwrap().clone();
    let rtc_state = DogmaItemState::from_sde(logi_char_id, 999_191, &ctx.sde);
    apply_projected_remote_assistance(&ctx, logi_char_id, logi_ship_id, rtc_mod_id, &rtc_type, &rtc_state, Some(ship_id), 10_000.0)
        .expect("project RTC");

    let mut mod_attrs = HashMap::from([(54u16, 10_000.0), (158, 5_000.0), (160, 0.05)]);
    apply_projected_rtc_to_module_attrs(&ctx, ship_id, &mut mod_attrs);
    assert!(*mod_attrs.get(&54).unwrap() > 11_900.0);

    let mut ship_attrs = HashMap::from([(76u16, 30_000.0), (79, 200.0), (208, 12.0)]);
    apply_tactical_module_ship_attributes(&ctx, ship_id, &mut ship_attrs);
    assert!(*ship_attrs.get(&76).unwrap() > 37_000.0);

    remove_projected_remote_assistance(&ctx, logi_ship_id, rtc_mod_id);
    remove_projected_remote_assistance(&ctx, logi_ship_id, rsb_mod_id);
}

#[tokio::test]
async fn target_spectrum_breaker_and_auto_targeting_system() {
    let ctx = test_ctx_with_custom_types(|sde| {
        sde.types.insert(
            999_192,
            custom_inv_type(sde, 999_192, 1154, 7, "Target Spectrum Breaker", &[(6, 50.0), (3114, 0.35)]),
        );
        sde.types.insert(
            999_193,
            custom_inv_type(sde, 999_193, 96, 7, "Auto Targeting System", &[]),
        );
    });
    let char_id = 94_000_010;
    let enemy_char_id = 94_000_011;
    let sys_id = 30_002_505;
    let ship_id = 1_000_400_101;
    let enemy_ship_id = 1_000_400_102;
    let tsb_mod_id = 1_000_400_103;
    let ats_mod_id = 1_000_400_104;

    ensure_char(&ctx, char_id, 98_000_001, sys_id, ship_id, 1);
    ensure_char(&ctx, enemy_char_id, 98_000_002, sys_id, enemy_ship_id, 1);
    insert_item(&ctx, ship_id, 587, char_id, sys_id as i64, InventoryFlag::None, 1, true);
    insert_item(&ctx, enemy_ship_id, 587, enemy_char_id, sys_id as i64, InventoryFlag::None, 1, true);
    insert_item(&ctx, tsb_mod_id, 999_192, char_id, ship_id, InventoryFlag::MedSlot0, 1, true);
    insert_item(&ctx, ats_mod_id, 999_193, char_id, ship_id, InventoryFlag::HiSlot0, 1, true);

    spawn_ship_ball_with_cs(&ctx, sys_id, ship_id, 587, char_id, DVec3::ZERO, 1_000.0).await;
    spawn_ship_ball_with_cs(&ctx, sys_id, enemy_ship_id, 587, enemy_char_id, DVec3::new(5_000.0, 0.0, 0.0), 1_000.0).await;
    let sys = ctx.systems.get_or_spawn(sys_id);

    // Three hostile ships locking ship_id ensure lockbreak probability >= 0.50
    for idx in 1..=3 {
        let extra_id = 1_000_400_120 + idx;
        insert_item(&ctx, extra_id, 587, enemy_char_id, sys_id as i64, InventoryFlag::None, 1, true);
        spawn_ship_ball_with_cs(&ctx, sys_id, extra_id, 587, enemy_char_id, DVec3::new(6_000.0 + (idx as f64) * 100.0, 0.0, 0.0), 1_000.0).await;
        let _ = sys
            .add_target_lock(
                extra_id,
                TargetLock {
                    target_id: ship_id,
                    lock_time_ms: 1_000.0,
                    remaining_lock_ms: 0.0,
                    locked: true,
                    max_range_m: Some(50_000.0),
                },
            )
            .await;
    }

    let tsb_type = ctx.sde.types.get(&999_192).unwrap().clone();
    let tsb_state = DogmaItemState::from_sde(char_id, 999_192, &ctx.sde);

    let tsb_res = activate_target_spectrum_breaker(&ctx, &sys, char_id, ship_id, tsb_mod_id, &tsb_type, &tsb_state)
        .await
        .expect("TSB activation");
    assert_eq!(tsb_res.dict_get("lockingShipCount").and_then(|v| v.as_i64()), Some(3));
    assert_eq!(tsb_res.dict_get("brokenLocks").and_then(|v| v.as_i64()), Some(3));

    // Enemy has active weapons timer -> Auto Targeting System acquires target lock on enemy_ship_id
    ctx.crimewatch
        .write()
        .unwrap()
        .get_or_create(enemy_char_id)
        .criminal_flag_until_unix = ctx.now_unix_secs() + 60;
    let fit = ShipFittingEngine::from_sde(ship_id, 587, &ctx.sde);
    let ats_type = ctx.sde.types.get(&999_193).unwrap().clone();
    let ats_state = DogmaItemState::from_sde(char_id, 999_193, &ctx.sde);
    let ats_res = activate_auto_targeting_system(&ctx, &sys, char_id, ship_id, ats_mod_id, &ats_type, &ats_state, &fit)
        .await
        .expect("ATS activation");
    assert!(ats_res.dict_get("lockingCount").and_then(|v| v.as_i64()).unwrap_or(0) >= 1);
}

#[tokio::test]
async fn disruptive_lance_superweapon_and_warmup_ballpark_hit() {
    let ctx = test_ctx_with_custom_types(|sde| {
        if let Some(mut sys) = sde.solar_systems.get(&30_000_142).cloned() {
            sys.solar_system_id = 30_002_505;
            sys.security_status = -0.5;
            sde.solar_systems.insert(30_002_505, sys);
        }
        sde.types.insert(
            999_201,
            custom_inv_type(sde, 999_201, 4594, 6, "Test Lancer Dreadnought", &[(5418, 20.0), (5417, 10.0)]),
        );
        sde.types.insert(
            77398,
            custom_inv_type(
                sde,
                77398,
                588,
                7,
                "Test Disruptive Lance",
                &[
                    (6, 10_000.0),
                    (54, 150_000.0),
                    (73, 300_000.0),
                    (114, 5_000.0),
                    (713, 16274.0),
                    (714, 1_000.0),
                    (2259, 10_000.0),
                    (2263, 2_500.0),
                    (2264, 5_000.0),
                    (2265, 1_000.0),
                    (2260, 5_000.0),
                ],
            ),
        );
    });

    let char_id = 94_000_020;
    let victim_char_id = 94_000_021;
    let lowsec_sys = 30_002_505;
    let lancer_id = 1_000_400_201;
    let victim_id = 1_000_400_202;
    let lance_mod_id = 1_000_400_203;
    let siege_mod_id = 1_000_400_204;
    let fuel_item_id = 1_000_400_205;

    ensure_char(&ctx, char_id, 98_000_010, lowsec_sys, lancer_id, 2);
    ensure_char(&ctx, victim_char_id, 98_000_011, lowsec_sys, victim_id, 1);
    insert_item(&ctx, lancer_id, 999_201, char_id, lowsec_sys as i64, InventoryFlag::None, 1, true);
    insert_item(&ctx, victim_id, 587, victim_char_id, lowsec_sys as i64, InventoryFlag::None, 1, true);
    insert_item(&ctx, lance_mod_id, 77398, char_id, lancer_id, InventoryFlag::HiSlot0, 1, true);
    insert_item(&ctx, siege_mod_id, 4292, char_id, lancer_id, InventoryFlag::HiSlot1, 1, true);

    spawn_ship_ball_with_cs(&ctx, lowsec_sys, lancer_id, 999_201, char_id, DVec3::ZERO, 50_000.0).await;
    spawn_ship_ball_with_cs(&ctx, lowsec_sys, victim_id, 587, victim_char_id, DVec3::new(10_000.0, 0.0, 0.0), 10_000.0).await;

    // 1. Highsec guard
    assert_eq!(
        activate_superweapon_or_lance(&ctx, char_id, lancer_id, 30_000_142, lance_mod_id, "ActivateSlashModule", Some(victim_id))
            .await
            .unwrap_err()
            .code,
        "CannotActivateDoomsdayInHighSec"
    );

    // 2. Requires active Siege module
    assert_eq!(
        activate_superweapon_or_lance(&ctx, char_id, lancer_id, lowsec_sys, lance_mod_id, "ActivateSlashModule", Some(victim_id))
            .await
            .unwrap_err()
            .code,
        "SiegeRequiredForLance"
    );

    // 3. Requires Disruptive Lance skills (77739, 77738)
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_active_states.insert(siege_mod_id);
    });
    assert_eq!(
        activate_superweapon_or_lance(&ctx, char_id, lancer_id, lowsec_sys, lance_mod_id, "ActivateSlashModule", Some(victim_id))
            .await
            .unwrap_err()
            .code,
        "SkillMissing"
    );

    // 4. Missing isotopes fails BEFORE draining capacitor
    grant_skill(&ctx, char_id, 77739, 4);
    grant_skill(&ctx, char_id, 77738, 4);
    let sys = ctx.systems.get_or_spawn(lowsec_sys);
    assert_eq!(
        activate_superweapon_or_lance(&ctx, char_id, lancer_id, lowsec_sys, lance_mod_id, "ActivateSlashModule", Some(victim_id))
            .await
            .unwrap_err()
            .code,
        "InsufficientFuel"
    );
    assert_eq!(sys.get_combat_state(lancer_id).await.unwrap().unwrap().capacitor, 50_000.0);

    // 5. Load isotopes and fire Disruptive Lance -> hits victim 10km along X axis, sets caster/victim locks
    insert_item(&ctx, fuel_item_id, 16274, char_id, lancer_id, InventoryFlag::Cargo, 2_500, false);
    let lance_res = activate_superweapon_or_lance(&ctx, char_id, lancer_id, lowsec_sys, lance_mod_id, "ActivateSlashModule", Some(victim_id))
        .await
        .expect("lancer beam fire");
    assert_eq!(lance_res.dict_get("fuelConsumed").and_then(|v| v.as_i64()), Some(1_000));
    assert_eq!(lance_res.dict_get("hitCount").and_then(|v| v.as_i64()), Some(1));
    assert!(has_lance_caster_lock(&ctx, lancer_id));
    assert_eq!(incoming_remote_repair_multiplier(&ctx, victim_id), 0.50);

    // 6. Direct fire_lance_warmup_in_ballpark queues LanceBeamFired pending action
    let mut bp = eve_destiny::Ballpark::new(lowsec_sys);
    bp.balls.insert(
        lancer_id,
        Ball {
            id: lancer_id,
            type_id: 999_201,
            position: DVec3::ZERO,
            velocity: DVec3::ZERO,
            max_velocity: 2_000.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 200.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
    );
    bp.balls.insert(
        victim_id,
        Ball {
            id: victim_id,
            type_id: 587,
            position: DVec3::new(5_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 500.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 100.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
    );
    let cyc = ActiveModuleCycle {
        module_id: lance_mod_id,
        ship_id: lancer_id,
        owner_char_id: Some(char_id),
        module_type_id: 77398,
        target_id: None,
        damage_vector: DamageVector::new(1_000.0, 0.0, 0.0, 0.0),
        duration_s: 300.0,
        elapsed_s: 15.0,
        repeat: 0,
        cap_need_per_cycle: 0.0,
        lance_beam_dir: Some(DVec3::X),
        effect_name: "superWeapon".into(),
        ..Default::default()
    };
    let mut pending = Vec::new();
    let mut destroyed = Vec::new();
    fire_lance_warmup_in_ballpark(
        &mut bp,
        &cyc,
        &Some(ctx.sde.clone()),
        &Some(ctx.db.clone()),
        &ctx.tactical_state,
        &mut pending,
        &mut destroyed,
    );
    assert_eq!(pending.len(), 1);
}

#[tokio::test]
async fn entosis_link_warmup_contest_pause_and_command_burst_activation() {
    let ctx = test_ctx_with_custom_types(|sde| {
        sde.types.insert(
            999_210,
            custom_inv_type(sde, 999_210, 1313, 7, "Test Entosis Link I", &[(73, 120_000.0)]),
        );
    });

    let char_id = 94_000_030;
    let char2_id = 94_000_031;
    let sys_id = 30_002_505;
    let ship_id = 1_000_400_301;
    let ship2_id = 1_000_400_302;
    let entosis_id = 1_000_400_303;
    let entosis2_id = 1_000_400_304;
    let stront_id = 1_000_400_305;
    let stront2_id = 1_000_400_306;
    let node_id = 1_895_000_123;

    ensure_char(&ctx, char_id, 98_000_020, sys_id, ship_id, 2);
    ensure_char(&ctx, char2_id, 98_000_021, sys_id, ship2_id, 2);
    insert_item(&ctx, ship_id, 587, char_id, sys_id as i64, InventoryFlag::None, 1, true);
    insert_item(&ctx, ship2_id, 587, char2_id, sys_id as i64, InventoryFlag::None, 1, true);
    insert_item(&ctx, entosis_id, 999_210, char_id, ship_id, InventoryFlag::HiSlot0, 1, true);
    insert_item(&ctx, entosis2_id, 999_210, char2_id, ship2_id, InventoryFlag::HiSlot0, 1, true);
    insert_item(&ctx, stront_id, 16275, char_id, ship_id, InventoryFlag::Cargo, 10, false);
    insert_item(&ctx, stront2_id, 16275, char2_id, ship2_id, InventoryFlag::Cargo, 10, false);

    spawn_ship_ball_with_cs(&ctx, sys_id, ship_id, 587, char_id, DVec3::ZERO, 1_000.0).await;
    spawn_ship_ball_with_cs(&ctx, sys_id, ship2_id, 587, char2_id, DVec3::new(1_000.0, 0.0, 0.0), 1_000.0).await;
    let sys = ctx.systems.get_or_spawn(sys_id);

    let ent_type = ctx.sde.types.get(&999_210).unwrap().clone();
    // Cycle 1: both opposing pilots start warmup cycle at t=1_800_000_000
    let c1 = handle_entosis_link_activation(&ctx, &sys, char_id, ship_id, entosis_id, &ent_type, Some(node_id))
        .await
        .expect("entosis cycle 1");
    assert_eq!(c1.dict_get("isWarmupCycle").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(c1.dict_get("captureRateMultiplier").and_then(|v| v.as_f64()), Some(0.0));
    let _ = handle_entosis_link_activation(&ctx, &sys, char2_id, ship2_id, entosis2_id, &ent_type, Some(node_id)).await;

    // Cycle 2: pilot 1 enters active capture (progressDelta == 1.0), then pilot 2 enters cycle 2 -> contests & pauses!
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_800_000_310)));
    let c2 = handle_entosis_link_activation(&ctx, &sys, char_id, ship_id, entosis_id, &ent_type, Some(node_id))
        .await
        .expect("entosis cycle 2");
    assert_eq!(c2.dict_get("isWarmupCycle").and_then(|v| v.as_bool()), Some(false));
    assert_eq!(c2.dict_get("captureRateMultiplier").and_then(|v| v.as_f64()), Some(1.0));

    let c2_contested = handle_entosis_link_activation(&ctx, &sys, char2_id, ship2_id, entosis2_id, &ent_type, Some(node_id))
        .await
        .expect("entosis contested cycle 2");
    assert_eq!(c2_contested.dict_get("isPaused").and_then(|v| v.as_bool()), Some(true));

    clear_entosis_warmup_on_unlock(&ctx, ship_id, Some(node_id));

    // Command Burst: Alpha clone guard vs Omega Fleet activation
    // 42826 = Armor Command Burst I, 42832 = Armor Energizing Charge
    if ctx.sde.types.contains_key(&42826) && ctx.sde.types.contains_key(&42832) {
        let burst_type = ctx.sde.types.get(&42826).unwrap().clone();
        let alpha_char = 94_000_040;
        let alpha_ship = 1_000_400_401;
        let alpha_burst = 1_000_400_402;
        ensure_char(&ctx, alpha_char, 98_000_020, sys_id, alpha_ship, 0);
        let fit_a = ShipFittingEngine::from_sde(alpha_ship, 587, &ctx.sde);
        let state_b = DogmaItemState::from_sde(alpha_char, 42826, &ctx.sde);
        let alpha_err = activate_command_burst(&ctx, &sys, alpha_char, alpha_ship, sys_id, alpha_burst, &burst_type, &state_b, Some(42832), &fit_a)
            .await
            .unwrap_err();
        assert_eq!(alpha_err.code, "OmegaRequiredForCommandBurst");

        let burst_mod_id = 1_000_400_410;
        let burst_charge_id = 1_000_400_411;
        insert_item(&ctx, burst_mod_id, 42826, char_id, ship_id, InventoryFlag::HiSlot1, 1, true);
        insert_item(&ctx, burst_charge_id, 42832, char_id, ship_id, InventoryFlag::HiSlot1, 200, false);
        ctx.db.with_mut_snapshot(|snap| {
            snap.fleets.insert(
                700_010,
                FleetRecord {
                    fleet_id: 700_010,
                    boss_id: char_id,
                    members: vec![char_id, char2_id],
                    ..Default::default()
                },
            );
            snap.characters.get_mut(&char_id).unwrap().fleet_id = Some(700_010);
            snap.characters.get_mut(&char2_id).unwrap().fleet_id = Some(700_010);
        });
        let fit_omega = ShipFittingEngine::from_sde(ship_id, 587, &ctx.sde);
        let state_omega = DogmaItemState::from_sde(char_id, 42826, &ctx.sde);
        let burst_ok = activate_command_burst(&ctx, &sys, char_id, ship_id, sys_id, burst_mod_id, &burst_type, &state_omega, None, &fit_omega)
            .await
            .expect("omega fleet command burst");
        assert_eq!(burst_ok.dict_get("chargeTypeID").and_then(|v| v.as_i64()), Some(42832));
        assert!(ctx
            .drain_notifications_for_char(char2_id)
            .iter()
            .any(|n| n.event == "OnDbuffUpdated"));
    }
}
