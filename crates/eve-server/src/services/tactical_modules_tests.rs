#![forbid(unsafe_code)]

use super::*;
use crate::router::ServerContext;
use eve_db::{
    InventoryFlag, InventoryItemRecord, TrainedSkillRecord, VirtualServerClock,
    WorldRepository,
};
use eve_destiny::{Ball, CombatState, MovementMode, SlimItemMeta, TargetLock};
use eve_dogma::{DogmaItemState, ShipFittingEngine};
use eve_sde::{InvType, SdeRegistry};
use glam::DVec3;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

#[cfg(test)]
#[path = "tactical_superweapons_and_entosis_tests.rs"]
mod tactical_superweapons_and_entosis_tests;

#[cfg(test)]
#[path = "tactical_superweapons_edge_tests.rs"]
mod tactical_superweapons_edge_tests;

pub(super) fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

pub(super) fn custom_inv_type(sde: &SdeRegistry, type_id: u32, group_id: u32, category_id: u32, name: &str, attrs: &[(u16, f64)]) -> InvType {
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

pub(super) fn test_ctx_with_custom_types<F: FnOnce(&mut SdeRegistry)>(mutate_sde: F) -> ServerContext {
    let mut sde = (*shared_sde()).clone();
    mutate_sde(&mut sde);
    let db = Arc::new(WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(Arc::new(sde), db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_800_000_000)));
    ctx
}

pub(super) fn ensure_char(ctx: &ServerContext, char_id: i64, corp_id: i64, sys_id: u32, ship_id: i64, clone_grade: u32) {
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

pub(super) fn grant_skill(ctx: &ServerContext, char_id: i64, skill_tid: u32, level: u8) {
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

pub(super) fn insert_item(
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

pub(super) async fn spawn_ship_ball_with_cs(
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
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
    let now_unix = ctx.now_unix_secs();
    tactical_test_helpers::set_lance_victim_debuff_until(&ctx, ship_id, now_unix + 60);
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
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

    ensure_char(&ctx, char_id, 98_000_001, sys_id, ship_id, 2);
    ensure_char(&ctx, enemy_char_id, 98_000_002, sys_id, enemy_ship_id, 2);
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tactical_modules_sync_fit_projection_on_ats_entosis_and_command_burst() {
    let ctx = test_ctx_with_custom_types(|sde| {
        if let Some(ship_t) = sde.types.get_mut(&587) {
            ship_t.default_attributes.insert(eve_dogma::attr_ids::HI_SLOTS, 4.0);
            ship_t.default_attributes.insert(eve_dogma::attr_ids::CPU_OUTPUT, 500.0);
            ship_t.default_attributes.insert(eve_dogma::attr_ids::POWER_OUTPUT, 500.0);
        }
        let mut ats_t = custom_inv_type(sde, 999_193, 96, 7, "Auto Targeting System", &[]);
        ats_t.effect_ids = vec![12];
        sde.types.insert(999_193, ats_t);

        let mut ent_t = custom_inv_type(
            sde,
            999_194,
            1313,
            7,
            "Entosis Link II",
            &[(73, 50.0), (713, 1.0), (2112, 120_000.0), (2113, 120_000.0)],
        );
        ent_t.effect_ids = vec![12];
        sde.types.insert(999_194, ent_t);

        let mut burst_t = custom_inv_type(
            sde,
            999_195,
            1770,
            7,
            "Shield Command Burst II",
            &[(422, 2.0), (73, 10.0), (2468, 10.0), (2469, 12.0), (2474, 60.0)],
        );
        burst_t.effect_ids = vec![12];
        sde.types.insert(999_195, burst_t);

        sde.types.insert(
            999_196,
            custom_inv_type(sde, 999_196, 1769, 8, "Shield Extension Charge", &[(137, 1.0)]),
        );
    });
    let char_id = 94_000_030;
    let enemy_char_id = 94_000_031;
    let sys_id = 30_004_759;
    let ship_id = 1_000_400_301;
    let enemy_ship_id = 1_000_400_302;
    let ats_mod_id = 1_000_400_303;
    let ent_mod_id = 1_000_400_304;
    let burst_mod_id = 1_000_400_305;
    let burst_charge_id = 1_000_400_306;
    let ozone_id = 1_000_400_307;
    let node_id = 1_000_400_308;

    ensure_char(&ctx, char_id, 98_000_001, sys_id, ship_id, 2);
    ensure_char(&ctx, enemy_char_id, 98_000_002, sys_id, enemy_ship_id, 2);
    grant_skill(&ctx, char_id, 3350, 5);
    grant_skill(&ctx, char_id, 3351, 4);

    insert_item(&ctx, ship_id, 587, char_id, sys_id as i64, InventoryFlag::None, 1, true);
    insert_item(&ctx, enemy_ship_id, 587, enemy_char_id, sys_id as i64, InventoryFlag::None, 1, true);
    insert_item(&ctx, ats_mod_id, 999_193, char_id, ship_id, InventoryFlag::HiSlot0, 1, true);
    insert_item(&ctx, ent_mod_id, 999_194, char_id, ship_id, InventoryFlag::HiSlot1, 1, true);
    insert_item(&ctx, burst_mod_id, 999_195, char_id, ship_id, InventoryFlag::HiSlot2, 1, true);
    insert_item(&ctx, burst_charge_id, 999_196, char_id, ship_id, InventoryFlag::HiSlot2, 10, false);
    insert_item(&ctx, ozone_id, 16275, char_id, ship_id, InventoryFlag::Cargo, 50, false);
    insert_item(&ctx, node_id, 2977, 98_000_009, sys_id as i64, InventoryFlag::None, 1, true);

    spawn_ship_ball_with_cs(&ctx, sys_id, ship_id, 587, char_id, DVec3::ZERO, 1_000.0).await;
    spawn_ship_ball_with_cs(
        &ctx,
        sys_id,
        enemy_ship_id,
        587,
        enemy_char_id,
        DVec3::new(5_000.0, 0.0, 0.0),
        1_000.0,
    )
    .await;
    ctx.crimewatch
        .write()
        .unwrap()
        .get_or_create(enemy_char_id)
        .criminal_flag_until_unix = ctx.now_unix_secs() + 60;

    let sys = ctx.systems.get_or_spawn(sys_id);
    let rev0 = ctx
        .invalidate_and_push_fit(ship_id)
        .expect("initial FitProjection pushed");
    let _ = sys.get_ball_snapshot(ship_id).await;
    let proj0 = sys
        .latest_fit_projection(ship_id)
        .expect("actor must hold baseline FitProjection");
    assert!(!matches!(
        proj0.modules.get(&ats_mod_id).map(|m| m.state),
        Some(eve_dogma::ModuleState::Active | eve_dogma::ModuleState::Overloaded)
    ));

    // 1. Auto Targeting System activation must invalidate and push FitProjection to actor.
    let fit0 = ctx
        .get_or_rebuild_ship_fitting(char_id, ship_id)
        .expect("fit0");
    let ats_type = ctx.sde.types.get(&999_193).unwrap().clone();
    let ats_state = DogmaItemState::from_sde(char_id, 999_193, &ctx.sde);
    let _ = activate_auto_targeting_system(
        &ctx, &sys, char_id, ship_id, ats_mod_id, &ats_type, &ats_state, &fit0,
    )
    .await
    .expect("ATS activation");
    let _ = sys.get_ball_snapshot(ship_id).await;
    let rev1 = ctx.current_fit_rev(ship_id);
    assert!(rev1.0 > rev0.0, "ATS activation must advance FitRev");
    let proj1 = sys
        .latest_fit_projection(ship_id)
        .expect("ATS activation must push FitProjection to actor");
    assert_eq!(proj1.rev, rev1);
    assert_eq!(
        proj1.modules.get(&ats_mod_id).map(|m| m.state),
        Some(eve_dogma::ModuleState::Active)
    );
    assert!(
        sys.latest_fit_projection(enemy_ship_id).is_none(),
        "unrelated ship must not receive a FitProjection push"
    );

    // 2. Entosis Link activation must push updated FitProjection, preserve concurrent CombatState fields
    //    via monotonic modify_combat_state, and register in the actor's external_active_modules registry.
    let pre_ent_cs = sys
        .get_combat_state(ship_id)
        .await
        .ok()
        .flatten()
        .unwrap_or_default();
    let expected_cap = pre_ent_cs.capacitor;
    let ent_type = ctx.sde.types.get(&999_194).unwrap().clone();
    let _ = handle_entosis_link_activation(
        &ctx,
        &sys,
        char_id,
        ship_id,
        ent_mod_id,
        &ent_type,
        Some(node_id),
    )
    .await
    .expect("Entosis activation");
    assert!(
        ctx.systems.is_module_active_sync(sys_id, ship_id, ent_mod_id),
        "Entosis activation must register (ship_id, ent_mod_id) in actor external_active_modules"
    );
    let _ = sys.get_ball_snapshot(ship_id).await;
    let rev2 = ctx.current_fit_rev(ship_id);
    assert!(rev2.0 > rev1.0, "Entosis activation must advance FitRev");
    let proj2 = sys
        .latest_fit_projection(ship_id)
        .expect("Entosis activation must push FitProjection to actor");
    assert_eq!(proj2.rev, rev2);
    assert_eq!(
        proj2.modules.get(&ent_mod_id).map(|m| m.state),
        Some(eve_dogma::ModuleState::Active)
    );
    let cs_after_ent = sys
        .get_combat_state(ship_id)
        .await
        .ok()
        .flatten()
        .expect("combat state after entosis");
    assert!((cs_after_ent.capacitor - expected_cap).abs() < 1e-6);
    assert_eq!(
        cs_after_ent.max_velocity_cap,
        Some(4_000.0),
        "Entosis activation must declaratively set max_velocity_cap so FitProjection recompute preserves the clamp"
    );
    assert!(
        cs_after_ent.warp_scramble_strength >= 100,
        "Entosis activation must preserve warp_scramble_strength >= 100 after FitProjection"
    );

    // 3. Command Burst activation must perform a single invalidate_and_push_fit step per affected ship (no redundant double push).
    let fit_burst = ctx
        .get_or_rebuild_ship_fitting(char_id, ship_id)
        .expect("fit for command burst");
    let burst_type = ctx.sde.types.get(&999_195).unwrap().clone();
    let burst_state = DogmaItemState::from_sde(char_id, 999_195, &ctx.sde);
    let _ = activate_command_burst(
        &ctx,
        &sys,
        char_id,
        ship_id,
        sys_id,
        burst_mod_id,
        &burst_type,
        &burst_state,
        Some(999_196),
        &fit_burst,
    )
    .await
    .expect("Command Burst activation");
    let _ = sys.get_ball_snapshot(ship_id).await;
    let rev3 = ctx.current_fit_rev(ship_id);
    assert_eq!(
        rev3.0 - rev2.0,
        rev2.0 - rev1.0,
        "Command Burst must perform a single invalidate_and_push_fit step without duplicate push_fit_projection"
    );
    let proj3 = sys
        .latest_fit_projection(ship_id)
        .expect("Command Burst activation must leave latest FitProjection on actor");
    assert_eq!(proj3.rev, rev3);
    assert_eq!(
        proj3.modules.get(&burst_mod_id).map(|m| m.state),
        Some(eve_dogma::ModuleState::Active)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tactical_modules_expired_entosis_replacement_unregisters_prior_module() {
    let ctx = test_ctx_with_custom_types(|sde| {
        if let Some(ship_t) = sde.types.get_mut(&587) {
            ship_t.default_attributes.insert(eve_dogma::attr_ids::HI_SLOTS, 4.0);
            ship_t.default_attributes.insert(eve_dogma::attr_ids::CPU_OUTPUT, 500.0);
            ship_t.default_attributes.insert(eve_dogma::attr_ids::POWER_OUTPUT, 500.0);
        }
        let mut ent_t = custom_inv_type(
            sde,
            999_194,
            1313,
            7,
            "Entosis Link II",
            &[(73, 50.0), (713, 1.0), (2112, 120_000.0), (2113, 120_000.0)],
        );
        ent_t.effect_ids = vec![12];
        sde.types.insert(999_194, ent_t);
    });
    let char_id = 94_000_040;
    let sys_id = 30_004_759;
    let ship_id = 1_000_400_401;
    let ent_mod_1 = 1_000_400_402;
    let ent_mod_2 = 1_000_400_403;
    let ozone_id = 1_000_400_404;
    let node_id = 1_000_400_405;

    ensure_char(&ctx, char_id, 98_000_001, sys_id, ship_id, 2);
    grant_skill(&ctx, char_id, 3350, 5);
    grant_skill(&ctx, char_id, 3351, 4);

    insert_item(&ctx, ship_id, 587, char_id, sys_id as i64, InventoryFlag::None, 1, true);
    insert_item(&ctx, ent_mod_1, 999_194, char_id, ship_id, InventoryFlag::HiSlot0, 1, true);
    insert_item(&ctx, ent_mod_2, 999_194, char_id, ship_id, InventoryFlag::HiSlot1, 1, true);
    insert_item(&ctx, ozone_id, 16275, char_id, ship_id, InventoryFlag::Cargo, 50, false);
    insert_item(&ctx, node_id, 2977, 98_000_009, sys_id as i64, InventoryFlag::None, 1, true);
    spawn_ship_ball_with_cs(&ctx, sys_id, ship_id, 587, char_id, DVec3::ZERO, 1_000.0).await;

    let sys = ctx.systems.get_or_spawn(sys_id);
    let ent_type = ctx.sde.types.get(&999_194).unwrap().clone();
    let _ = handle_entosis_link_activation(
        &ctx,
        &sys,
        char_id,
        ship_id,
        ent_mod_1,
        &ent_type,
        Some(node_id),
    )
    .await
    .expect("first Entosis activation");
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_active_states.insert(ent_mod_1);
    });
    assert!(sys.is_module_active_sync(ship_id, ent_mod_1));
    assert!(sys.active_module_ships.read().unwrap().contains(&ship_id));
    assert!(sys.view().has_active_module(ship_id));

    ctx.advance_secs(121);
    let _ = handle_entosis_link_activation(
        &ctx,
        &sys,
        char_id,
        ship_id,
        ent_mod_2,
        &ent_type,
        Some(node_id),
    )
    .await
    .expect("expired Entosis replacement must succeed");
    assert!(
        !sys.is_module_active_sync(ship_id, ent_mod_1),
        "expired Entosis Link must be unregistered when replaced by another module"
    );
    assert!(
        !ctx.db
            .with_snapshot(|s| s.module_active_states.contains(&ent_mod_1)),
        "replaced Entosis module must be cleared from module_active_states"
    );
    assert!(sys.is_module_active_sync(ship_id, ent_mod_2));
    assert!(sys.active_module_ships.read().unwrap().contains(&ship_id));
    assert!(sys.view().has_active_module(ship_id));

    let mut sess = eve_machonet::SessionState {
        user_id: Some(char_id as u64),
        character_id: Some(char_id),
        corporation_id: Some(98_000_001),
        solar_system_id: Some(sys_id),
        ship_id: Some(ship_id),
        ..Default::default()
    };
    crate::services::notify_emitters_skills_trade_godma::handle_dogma_take_module_offline(
        &ctx,
        &mut sess,
        &[PyRep::Integer(ship_id), PyRep::Integer(ent_mod_2)],
    )
    .await
    .expect("take replacement Entosis offline");
    assert!(!sys.is_module_active_sync(ship_id, ent_mod_2));
    assert!(!sys.active_module_ships.read().unwrap().contains(&ship_id));
    assert!(!sys.view().has_active_module(ship_id));
}

#[tokio::test(flavor = "current_thread")]
async fn wire_module_deactivation_hook_flushes_remote_assistance_on_current_thread_and_stop_all_ewar() {
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
    ctx.wire_module_deactivation_hook();

    let (char_id, logi_char_id) = (94_001_001_i64, 94_001_002_i64);
    let sys_id = 30_002_506_u32;
    let (ship_id, logi_ship_id) = (1_000_410_001_i64, 1_000_410_002_i64);
    let (rsb_mod_id, rtc_mod_id) = (1_000_410_004_i64, 1_000_410_005_i64);

    ensure_char(&ctx, char_id, 98_000_001, sys_id, ship_id, 1);
    ensure_char(&ctx, logi_char_id, 98_000_002, sys_id, logi_ship_id, 1);
    insert_item(&ctx, ship_id, 587, char_id, sys_id as i64, InventoryFlag::None, 1, true);
    insert_item(&ctx, logi_ship_id, 587, logi_char_id, sys_id as i64, InventoryFlag::None, 1, true);
    insert_item(&ctx, rsb_mod_id, 999_190, logi_char_id, logi_ship_id, InventoryFlag::MedSlot0, 1, true);
    insert_item(&ctx, rtc_mod_id, 999_191, logi_char_id, logi_ship_id, InventoryFlag::MedSlot1, 1, true);
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_active_states.insert(rsb_mod_id);
        snap.module_active_states.insert(rtc_mod_id);
    });

    let sys = ctx.systems.get_or_spawn(sys_id);
    spawn_ship_ball_with_cs(&ctx, sys_id, ship_id, 587, char_id, DVec3::ZERO, 1_000.0).await;
    spawn_ship_ball_with_cs(&ctx, sys_id, logi_ship_id, 587, logi_char_id, DVec3::new(5_000.0, 0.0, 0.0), 1_000.0).await;

    let rsb_type = ctx.sde.types.get(&999_190).unwrap().clone();
    let rsb_state = DogmaItemState::from_sde(logi_char_id, 999_190, &ctx.sde);
    apply_projected_remote_assistance(&ctx, logi_char_id, logi_ship_id, rsb_mod_id, &rsb_type, &rsb_state, Some(ship_id), 10_000.0)
        .expect("project RSB");

    let rtc_type = ctx.sde.types.get(&999_191).unwrap().clone();
    let rtc_state = DogmaItemState::from_sde(logi_char_id, 999_191, &ctx.sde);
    apply_projected_remote_assistance(&ctx, logi_char_id, logi_ship_id, rtc_mod_id, &rtc_type, &rtc_state, Some(ship_id), 10_000.0)
        .expect("project RTC");

    assert_eq!(sys.view().remote_assistance_for(ship_id).map(|l| l.len()), Some(2));
    let mut ship_attrs = HashMap::from([(76u16, 30_000.0), (79, 200.0), (208, 12.0)]);
    apply_tactical_module_ship_attributes(&ctx, ship_id, &mut ship_attrs);
    assert!(*ship_attrs.get(&76).unwrap() > 37_000.0);

    let rev_before_deact = ctx.current_fit_rev(ship_id);
    sys.add_module_cycle(crate::system_actor::ActiveModuleCycle {
        ship_id: logi_ship_id,
        owner_char_id: Some(logi_char_id),
        module_id: rsb_mod_id,
        module_type_id: 999_190,
        duration_s: 10.0,
        ..Default::default()
    })
    .await
    .expect("register RSB cycle");

    // Deactivating on a `current_thread` runtime freezes the actor task while `try_remove_module_cycle`
    // waits for `post_unlock_worker`. Because `flush_module_ewar_in_state` pre-flushes
    // `remote_assistance` inside the actor and `finish_flushed_module_ewar` pushes the target fit
    // without blocking on the actor mailbox, this must complete without a 5s deadlock.
    let t0 = std::time::Instant::now();
    sys.try_remove_module_cycle(logi_ship_id, rsb_mod_id);
    assert!(
        t0.elapsed() < std::time::Duration::from_secs(2),
        "deactivation hook must not deadlock against current_thread actor"
    );
    assert!(
        ctx.current_fit_rev(ship_id) > rev_before_deact,
        "finish_flushed_module_ewar must invalidate/bump target ship fit rev"
    );
    let remaining = sys
        .view()
        .remote_assistance_for(ship_id)
        .expect("RTC entry still active");
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].module_id, rtc_mod_id);

    let mut ship_attrs_after = HashMap::from([(76u16, 30_000.0), (79, 200.0), (208, 12.0)]);
    apply_tactical_module_ship_attributes(&ctx, ship_id, &mut ship_attrs_after);
    assert!((*ship_attrs_after.get(&76).unwrap() - 30_000.0).abs() < 1e-6);

    // `flush_ship_all_ewar_sync` must also discover target-only `remote_assistance` entries (P2).
    let flushed_pairs = crate::services::notify_emitters_space_dungeons::flush_ship_all_ewar_sync(
        &ctx,
        char_id,
        Some(sys_id),
        ship_id,
    );
    assert_eq!(flushed_pairs, vec![(logi_ship_id, rtc_mod_id)]);
    assert!(sys.view().remote_assistance_for(ship_id).is_none());
    let mut mod_attrs = HashMap::from([(54u16, 10_000.0), (158, 5_000.0), (160, 0.05)]);
    apply_projected_rtc_to_module_attrs(&ctx, ship_id, &mut mod_attrs);
    assert!((*mod_attrs.get(&54).unwrap() - 10_000.0).abs() < 1e-6);

    // `apply_projected_remote_assistance` must propagate actor failure and skip fit invalidation.
    let rev_before_err = ctx.current_fit_rev(ship_id);
    {
        let lock: eve_db::lock_rank::RankedRwLock<(), { eve_db::lock_rank::RANK_NPC_SPAWNS }> =
            eve_db::lock_rank::RankedRwLock::new(());
        let _guard = lock.read().unwrap();
        let err = apply_projected_remote_assistance(
            &ctx,
            logi_char_id,
            logi_ship_id,
            rsb_mod_id,
            &rsb_type,
            &rsb_state,
            Some(ship_id),
            10_000.0,
        )
        .unwrap_err();
        assert!(matches!(err.client_error, Some(eve_proto::ClientError::Internal(_))));
    }
    assert_eq!(ctx.current_fit_rev(ship_id), rev_before_err);
    assert!(sys.view().remote_assistance_for(ship_id).is_none());

    assert!(
        tokio::time::timeout(std::time::Duration::from_secs(2), ctx.systems.stop_system(sys_id))
            .await
            .expect("stop_system must drain cleanly")
    );
}
