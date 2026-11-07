//! Deep branch-covering & mutation-killing tests for `tactical_modules`,
//! `inventory_fitting::fitting_scanners_superweapons`, and `inventory_fitting::fitting_ship_bays`.

#![forbid(unsafe_code)]

use super::inventory_fitting::fitting_containers_ships::{
    write_container_ship_state, ShipAccessConfig,
};
use super::inventory_fitting::fitting_scanners_superweapons::{
    active_hacking_target_for_char, apply_active_dbuffs_and_system_effects_to_ship_attrs,
    get_aggregated_ship_dbuffs, handle_scanner_analyzer_entosis_activation,
    handle_superweapons_dbuffs_and_clones, mark_hacking_container_unlocked,
    scanners_superweapons_state, ActiveDbuffEntry,
};
use super::inventory_fitting::fitting_ship_bays::{
    board_stored_ship, ship_drop, ship_jettison, ship_scoop_and_store,
};
use super::tactical_modules::{
    activate_auto_targeting_system, activate_command_burst, activate_superweapon_with_args,
    activate_target_spectrum_breaker, apply_projected_remote_assistance,
    apply_projected_rtc_to_module_attrs, apply_tactical_module_ship_attributes,
    apply_transient_item_attrs, check_ship_navigation_lock, clear_entosis_warmup_on_unlock,
    fire_lance_warmup_in_ballpark, handle_entosis_link_activation,
    incoming_remote_repair_multiplier, is_clone_vat_bay_active_required,
    prepare_ship_for_wormhole_jump, remove_projected_remote_assistance,
};
use crate::router::ServerContext;
use crate::system_actor::{ActiveModuleCycle, PendingCycleAction};
use eve_db::{
    corp_roles, InventoryFlag, InventoryItemRecord, TrainedSkillRecord, UpwellStructureRecord,
    VirtualServerClock, WorldRepository,
};
use eve_destiny::{
    Ball, Ballpark, CombatState, DamageVector, MovementMode, SlimItemMeta, TargetLock,
};
use eve_dogma::DogmaItemState;
use eve_machonet::{service_roles, SessionState};
use eve_proto::PyRep;
use eve_sde::{InvType, SdeRegistry};
use glam::DVec3;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

#[allow(clippy::too_many_arguments)]
fn custom_type(
    sde: &SdeRegistry,
    type_id: u32,
    group_id: u32,
    category_id: u32,
    volume: f64,
    capacity: f64,
    name: &str,
    attrs: &[(u16, f64)],
    effects: &[u16],
) -> InvType {
    let mut t = sde.types.get(&587).expect("Rifter seed").clone();
    t.type_id = type_id;
    t.group_id = group_id;
    t.category_id = category_id;
    t.volume = volume;
    t.capacity = capacity;
    t.name = name.into();
    t.default_attributes.clear();
    for &(k, v) in attrs {
        t.default_attributes.insert(k, v);
    }
    t.effect_ids = effects.to_vec();
    t
}

fn make_ctx() -> ServerContext {
    let mut sde = (*shared_sde()).clone();
    let specs: &[(u32, u32, u32, f64, f64, &str, &[(u16, f64)], &[u16])] = &[
        (997_001, 290, 7, 5.0, 0.0, "Remote Sensor Booster II", &[(54, 40_000.0), (2044, 20_000.0), (55, 25.0), (566, 30.0)], &[]),
        (997_002, 209, 7, 5.0, 0.0, "Remote Tracking Computer II", &[(54, 40_000.0), (2044, 20_000.0), (349, 15.0), (351, 20.0), (767, 25.0)], &[]),
        (997_003, 782, 7, 5.0, 0.0, "Small Cargohold Optimization I", &[], &[]),
        (997_004, 1313, 7, 5.0, 100.0, "Entosis Link II", &[(73, 50.0), (713, 1.0), (2112, 120_000.0), (2113, 120_000.0)], &[]),
        (997_005, 60, 7, 5.0, 0.0, "Reactive Armor Hardener", &[(974, 0.5), (975, 0.5), (976, 0.5), (977, 0.5)], &[6484]),
        (997_006, 1308, 7, 5.0, 0.0, "Target Spectrum Breaker", &[(6, 50.0), (73, 10.0), (2427, 15_000.0), (3114, 0.1)], &[]),
        (997_007, 96, 7, 5.0, 0.0, "Auto Targeting System I", &[(143, 80_000.0)], &[]),
        (997_010, 30, 6, 1_000_000.0, 10_000.0, "Avatar", &[(482, 50_000.0), (1137, 25.0), (2056, 30_000.0)], &[]),
        (997_011, 4594, 6, 800_000.0, 10_000.0, "Bane", &[(482, 50_000.0), (1137, 20.0), (2056, 30_000.0)], &[]),
        (997_012, 588, 7, 4_000.0, 0.0, "Judgement", &[(6, 100.0), (54, 100_000.0), (713, 16274.0), (714, 500.0), (2427, 300_000.0), (114, 50_000.0)], &[]),
        (997_013, 588, 7, 4_000.0, 0.0, "Disruptive Lance II", &[(6, 100.0), (54, 100_000.0), (2263, 5_000.0), (552, 500.0), (713, 16274.0), (714, 500.0), (2427, 300_000.0), (116, 20_000.0)], &[]),
        (997_014, 1770, 7, 5.0, 100.0, "Shield Command Burst II", &[(422, 2.0), (73, 10.0), (2468, 10.0), (2469, 12.0), (2474, 60.0)], &[]),
        (997_015, 1769, 8, 0.1, 0.0, "Shield Extension Charge", &[(137, 1.0)], &[]),
        (997_016, 538, 7, 5.0, 0.0, "Relic Analyzer II", &[(54, 10_000.0), (1909, 80.0), (1910, 30.0)], &[]),
        (997_017, 47, 7, 5.0, 0.0, "Cargo Scanner II", &[(54, 30_000.0), (126, 30_000.0)], &[]),
        (997_018, 48, 7, 5.0, 0.0, "Ship Scanner II", &[(54, 30_000.0)], &[]),
        (997_019, 538, 7, 5.0, 0.0, "Data Analyzer II", &[(54, 10_000.0), (1909, 80.0), (1910, 30.0)], &[]),
        (997_020, 485, 6, 500_000.0, 10_000.0, "Fleet Carrier", &[(908, 500_000.0), (912, 50_000.0), (5325, 10_000.0), (5646, 50_000.0)], &[]),
        (997_021, 25, 6, 2_500.0, 200.0, "Test Frigate", &[(76, 50_000.0), (192, 5.0), (552, 250.0)], &[]),
        (997_022, 1202, 6, 10_000.0, 1_000.0, "Prorator", &[], &[]),
        (997_023, 1246, 22, 50.0, 100.0, "Mobile Depot", &[], &[]),
    ];
    for &(tid, gid, cat, vol, cap, name, attrs, effs) in specs {
        sde.types.insert(tid, custom_type(&sde, tid, gid, cat, vol, cap, name, attrs, effs));
    }
    let db = Arc::new(WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(Arc::new(sde), db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_800_000_000)));
    ctx
}

#[allow(clippy::too_many_arguments)]
fn put_item(
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

fn put_skill(ctx: &ServerContext, char_id: i64, skill_type_id: u32, level: u8) {
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(char_id).or_default().insert(
            skill_type_id,
            TrainedSkillRecord {
                type_id: skill_type_id,
                level,
                skill_points: 256_000.0,
            },
        );
    });
}

#[allow(clippy::too_many_arguments)]
async fn spawn_ball(
    ctx: &ServerContext,
    sys_id: u32,
    ball_id: i64,
    type_id: u32,
    group_id: u32,
    category_id: u32,
    owner_id: i64,
    pos: DVec3,
) {
    let sys = ctx.systems.get_or_spawn(sys_id);
    let _ = sys.remove_ball(ball_id).await;
    let _ = sys
        .spawn_ball_with_meta(
            Ball {
                id: ball_id,
                type_id,
                position: pos,
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState {
                shield_hp: 10_000.0,
                max_shield_hp: 10_000.0,
                armor_hp: 10_000.0,
                max_armor_hp: 10_000.0,
                structure_hp: 10_000.0,
                max_structure_hp: 10_000.0,
                capacitor: 50_000.0,
                max_capacitor: 50_000.0,
                signature_radius: 100.0,
                ..Default::default()
            },
            SlimItemMeta {
                group_id,
                category_id,
                owner_id,
                char_id: Some(owner_id),
                corp_id: 98_000_001,
                ..Default::default()
            },
        )
        .await;
}

#[tokio::test]
async fn tactical_projected_assistance_tsb_auto_target_and_wormhole_guards() {
    let ctx = make_ctx();
    let sys_id = 30_000_142;
    let c1 = 90_000_001;
    let c2 = 90_000_002;
    let s1 = 1_200_001;
    let s2 = 1_200_002;
    put_item(&ctx, s1, 997_021, c1, sys_id as i64, InventoryFlag::None, 1, true);
    put_item(&ctx, s2, 997_021, c2, sys_id as i64, InventoryFlag::None, 1, true);
    put_item(&ctx, 1_200_010, 997_001, c1, s1, InventoryFlag::MedSlot0, 1, true);
    put_item(&ctx, 1_200_011, 997_002, c1, s1, InventoryFlag::MedSlot1, 1, true);
    put_item(&ctx, 1_200_012, 997_003, c2, s2, InventoryFlag::RigSlot0, 1, true);
    put_item(&ctx, 1_200_013, 997_005, c2, s2, InventoryFlag::LoSlot0, 1, true);
    put_item(&ctx, 1_200_014, 997_006, c2, s2, InventoryFlag::MedSlot0, 1, true);
    put_item(&ctx, 1_200_015, 997_007, c2, s2, InventoryFlag::HiSlot0, 1, true);
    put_item(&ctx, 1_200_016, 33400, c2, s2, InventoryFlag::HiSlot1, 1, true);

    let rsb_type = ctx.sde.types.get(&997_001).unwrap().clone();
    let rtc_type = ctx.sde.types.get(&997_002).unwrap().clone();
    let rsb_state = DogmaItemState::from_sde(1_200_010, 997_001, &ctx.sde);
    let rtc_state = DogmaItemState::from_sde(1_200_011, 997_002, &ctx.sde);

    assert_eq!(apply_projected_remote_assistance(&ctx, c1, s1, 1_200_010, &rsb_type, &rsb_state, None, 10_000.0).unwrap_err().code, "TargetRequired");
    assert_eq!(apply_projected_remote_assistance(&ctx, c1, s1, 1_200_010, &rsb_type, &rsb_state, Some(s2), 90_000.0).unwrap_err().code, "OutOfRange");
    ctx.crimewatch.write().unwrap().get_or_create(c2).suspect_flag_until_unix = 1_800_000_600;
    apply_projected_remote_assistance(&ctx, c1, s1, 1_200_010, &rsb_type, &rsb_state, Some(s2), 60_000.0).unwrap();
    assert!(ctx.crimewatch.read().unwrap().states.get(&c1).is_some_and(|st| st.suspect_flag_until_unix >= 1_800_000_900 && st.criminal_flag_until_unix < 1_800_000_900));
    ctx.crimewatch.write().unwrap().get_or_create(c2).criminal_flag_until_unix = 1_800_000_600;
    apply_projected_remote_assistance(&ctx, c1, s1, 1_200_011, &rtc_type, &rtc_state, Some(s2), 40_000.0).unwrap();
    assert!(ctx.crimewatch.read().unwrap().states.get(&c1).is_some_and(|st| st.criminal_flag_until_unix >= 1_800_000_900));

    let mut tur_attrs = HashMap::from([(54u16, 10_000.0), (160u16, 0.05)]);
    apply_projected_rtc_to_module_attrs(&ctx, s2, &mut tur_attrs);
    assert!(tur_attrs[&54] > 10_000.0 && tur_attrs[&160] > 0.05);

    ctx.db.with_mut_snapshot(|snap| {
        snap.module_active_states.insert(1_200_013);
        snap.transient_item_attributes.insert(s2, HashMap::from([(1234u16, 77.5)]));
    });
    let mut ship_attrs = HashMap::from([(37u16, 5_000.0), (76u16, 100.0), (564u16, 200.0)]);
    apply_tactical_module_ship_attributes(&ctx, s2, &mut ship_attrs);
    apply_transient_item_attrs(&ctx, s2, &mut ship_attrs);
    assert!((ship_attrs[&37] - 4_500.0).abs() < 1e-6);
    put_item(&ctx, 1_200_017, 997_004, c2, s2, InventoryFlag::HiSlot2, 1, true);
    let mut entosis_attrs = HashMap::from([(37u16, 5_000.0)]);
    apply_tactical_module_ship_attributes(&ctx, s2, &mut entosis_attrs);
    assert!((entosis_attrs[&37] - 4_000.0).abs() < 1e-6);
    assert_eq!(ship_attrs[&113], 0.5);
    assert_eq!(ship_attrs[&1234], 77.5);
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(1_200_016, true);
        snap.module_active_states.insert(1_200_016);
    });
    let mut bast_tur = HashMap::from([(54u16, 10_000.0)]);
    apply_projected_rtc_to_module_attrs(&ctx, s2, &mut bast_tur);
    assert!(bast_tur[&54] < 10_500.0);
    remove_projected_remote_assistance(&ctx, s1, 1_200_010);

    let sys = ctx.systems.get_or_spawn(sys_id);
    spawn_ball(&ctx, sys_id, s1, 997_021, 25, 6, c1, DVec3::new(0.0, 0.0, 0.0)).await;
    spawn_ball(&ctx, sys_id, s2, 997_021, 25, 6, c2, DVec3::new(1_000.0, 0.0, 0.0)).await;
    let _ = sys.add_target_lock(s1, TargetLock { target_id: s2, lock_time_ms: 1_000.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(100_000.0) }).await;
    let tsb_type = ctx.sde.types.get(&997_006).unwrap().clone();
    let tsb_state = DogmaItemState::from_sde(1_200_014, 997_006, &ctx.sde);
    let fit = ctx.get_or_rebuild_ship_fitting(c2, s2).unwrap();
    let tsb_rep = activate_target_spectrum_breaker(&ctx, &sys, c2, s2, 1_200_014, &tsb_type, &tsb_state).await.unwrap();
    assert!(tsb_rep.dict_get("brokenLocks").and_then(|v| v.as_i64()).unwrap_or(0) >= 1);
    assert_eq!(sys.get_target_locks(s1).await.unwrap_or_default().len(), 0);

    let _ = sys.add_module_cycle(ActiveModuleCycle { ship_id: s1, module_id: 99, module_type_id: 482, target_id: Some(s2), duration_s: 10.0, damage_vector: DamageVector { em: 50.0, thermal: 0.0, kinetic: 0.0, explosive: 0.0 }, ..Default::default() }).await;
    let auto_type = ctx.sde.types.get(&997_007).unwrap().clone();
    let auto_state = DogmaItemState::from_sde(1_200_015, 997_007, &ctx.sde);
    let auto_rep = activate_auto_targeting_system(&ctx, &sys, c2, s2, 1_200_015, &auto_type, &auto_state, &fit).await.unwrap();
    assert!(auto_rep.dict_get("lockingCount").and_then(|v| v.as_i64()).unwrap_or(0) >= 1);

    ctx.db.with_mut_snapshot(|snap| {
        snap.module_active_states.insert(1_200_016);
    });
    let mut dummy_ball = Ball { id: s2, type_id: 997_021, position: DVec3::ZERO, velocity: DVec3::ZERO, max_velocity: 300.0, inertia: 1.0, mass: 1_000_000.0, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop };
    assert_eq!(prepare_ship_for_wormhole_jump(&ctx, &sys, c2, s2, &mut dummy_ball).await.unwrap_err().code, "CannotJumpWormholeWhileImmobilized");
    ctx.db.with_mut_snapshot(|snap| snap.module_active_states.remove(&1_200_016));
    assert!(!prepare_ship_for_wormhole_jump(&ctx, &sys, c2, s2, &mut dummy_ball).await.unwrap());
    assert_eq!(incoming_remote_repair_multiplier(&ctx, s2), 1.0);
    assert!(!is_clone_vat_bay_active_required(&ctx, s2));
    clear_entosis_warmup_on_unlock(&ctx, s2, None);
    let _ = check_ship_navigation_lock(&ctx, s2, "warp");
}

#[tokio::test]
async fn tactical_superweapons_lance_entosis_and_command_bursts() {
    let ctx = make_ctx();
    let null_sys = 30_004_759;
    let jita_sys = 30_000_142;
    let c1 = 90_000_001;
    let c2 = 90_000_002;
    let titan_id = 1_300_001;
    let lancer_id = 1_300_002;
    let victim_id = 1_300_003;
    put_item(&ctx, titan_id, 997_010, c1, null_sys as i64, InventoryFlag::None, 1, true);
    put_item(&ctx, lancer_id, 997_011, c2, null_sys as i64, InventoryFlag::None, 1, true);
    put_item(&ctx, victim_id, 997_021, 90_000_003, null_sys as i64, InventoryFlag::None, 1, true);
    put_item(&ctx, 1_300_010, 997_012, c1, titan_id, InventoryFlag::HiSlot0, 1, true);
    put_item(&ctx, 1_300_011, 997_013, c2, lancer_id, InventoryFlag::HiSlot0, 1, true);
    put_item(&ctx, 1_300_012, 20280, c2, lancer_id, InventoryFlag::HiSlot1, 1, true);
    put_item(&ctx, 1_300_013, 997_004, c1, titan_id, InventoryFlag::HiSlot1, 1, true);
    put_item(&ctx, 1_300_014, 997_014, c1, titan_id, InventoryFlag::HiSlot2, 1, true);

    assert_eq!(activate_superweapon_with_args(&ctx, c2, lancer_id, jita_sys, 1_300_011, "ActivateSlashModule", None, &[]).await.unwrap_err().code, "CannotActivateDoomsdayInHighSec");
    assert_eq!(activate_superweapon_with_args(&ctx, c2, lancer_id, null_sys, 1_300_011, "ActivateSlashModule", None, &[]).await.unwrap_err().code, "SiegeRequiredForLance");
    ctx.db.with_mut_snapshot(|snap| snap.module_active_states.insert(1_300_012));
    assert_eq!(activate_superweapon_with_args(&ctx, c2, lancer_id, null_sys, 1_300_011, "ActivateSlashModule", None, &[]).await.unwrap_err().code, "SkillMissing");
    put_skill(&ctx, c2, 77739, 1);
    put_skill(&ctx, c2, 77738, 1);
    put_skill(&ctx, c1, 33099, 1);
    put_skill(&ctx, c1, 24563, 1);
    spawn_ball(&ctx, null_sys, titan_id, 997_010, 30, 6, c1, DVec3::ZERO).await;
    spawn_ball(&ctx, null_sys, lancer_id, 997_011, 4594, 6, c2, DVec3::new(50_000.0, 0.0, 0.0)).await;
    spawn_ball(&ctx, null_sys, victim_id, 997_021, 25, 6, 90_000_003, DVec3::new(70_000.0, 0.0, 0.0)).await;

    assert_eq!(activate_superweapon_with_args(&ctx, c1, titan_id, null_sys, 1_300_010, "ActivateSinglePointTargetedModule", None, &[]).await.unwrap_err().code, "TargetRequired");
    let sys = ctx.systems.get_or_spawn(null_sys);
    let _ = sys.add_target_lock(titan_id, TargetLock { target_id: lancer_id, lock_time_ms: 1_000.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(200_000.0) }).await;
    put_item(&ctx, 1_300_020, 16274, c1, titan_id, InventoryFlag::SpecializedFuelBay, 1_000, false);
    assert!(activate_superweapon_with_args(&ctx, c1, titan_id, null_sys, 1_300_010, "ActivateSinglePointTargetedModule", Some(lancer_id), &[]).await.is_ok());

    put_item(&ctx, 1_300_021, 16274, c2, lancer_id, InventoryFlag::SpecializedFuelBay, 1_000, false);
    let dir_arg = PyRep::Tuple(vec![PyRep::Real(1.0), PyRep::Real(0.0), PyRep::Real(0.0)]);
    let _ = activate_superweapon_with_args(&ctx, c2, lancer_id, null_sys, 1_300_011, "ActivateSlashModule", None, &[dir_arg]).await;

    let mut bp = Ballpark::new(null_sys);
    bp.add_ball(Ball { id: lancer_id, type_id: 997_011, position: DVec3::ZERO, velocity: DVec3::ZERO, max_velocity: 100.0, inertia: 1.0, mass: 1e8, radius: 1000.0, speed_fraction: 0.0, mode: MovementMode::Stop });
    bp.add_ball(Ball { id: victim_id, type_id: 997_021, position: DVec3::new(20_000.0, 0.0, 0.0), velocity: DVec3::ZERO, max_velocity: 300.0, inertia: 1.0, mass: 1e6, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop });
    bp.ball_slim_meta.insert(victim_id, SlimItemMeta { group_id: 25, category_id: 6, owner_id: 90_000_003, ..Default::default() });
    bp.combat_states.insert(lancer_id, CombatState { is_tethered: true, ..Default::default() });
    bp.combat_states.insert(victim_id, CombatState { shield_hp: 0.0, max_shield_hp: 0.0, armor_hp: 10_000.0, max_armor_hp: 10_000.0, structure_hp: 10_000.0, max_structure_hp: 10_000.0, signature_radius: 250.0, is_tethered: true, ..Default::default() });
    let cycle = ActiveModuleCycle {
        ship_id: lancer_id, module_id: 1_300_011, module_type_id: 997_013, owner_char_id: Some(c2),
        effect_name: "superWeaponLanceWarmup".into(), duration_s: 10.0,
        damage_vector: DamageVector { em: 10_000.0, thermal: 0.0, kinetic: 0.0, explosive: 0.0 },
        lance_beam_dir: Some(DVec3::X), ..Default::default()
    };
    let mut actions: Vec<PendingCycleAction> = Vec::new();
    let mut destroyed = Vec::new();
    fire_lance_warmup_in_ballpark(&mut bp, &cycle, &Some(Arc::clone(&ctx.sde)), &Some(Arc::clone(&ctx.db)), &ctx.tactical_state, &mut actions, &mut destroyed);
    assert!(!bp.combat_states[&lancer_id].is_tethered);
    assert_eq!(bp.combat_states[&victim_id].armor_hp, 10_000.0);
    bp.combat_states.get_mut(&victim_id).unwrap().is_tethered = false;
    fire_lance_warmup_in_ballpark(&mut bp, &cycle, &Some(Arc::clone(&ctx.sde)), &Some(Arc::clone(&ctx.db)), &ctx.tactical_state, &mut actions, &mut destroyed);
    assert!(bp.combat_states[&victim_id].armor_hp < 10_000.0);

    let ent_type = ctx.sde.types.get(&997_004).unwrap().clone();
    put_item(&ctx, 1_300_030, 2977, 98_000_009, null_sys as i64, InventoryFlag::None, 1, true);
    assert_eq!(handle_entosis_link_activation(&ctx, &sys, c1, titan_id, 1_300_013, &ent_type, None).await.unwrap_err().code, "TargetRequired");
    assert_eq!(handle_entosis_link_activation(&ctx, &sys, c1, titan_id, 1_300_013, &ent_type, Some(1_300_030)).await.unwrap_err().code, "InsufficientFuel");
    put_item(&ctx, 1_300_031, 16275, c1, titan_id, InventoryFlag::Cargo, 50, false);
    let ent_res = handle_entosis_link_activation(&ctx, &sys, c1, titan_id, 1_300_013, &ent_type, Some(1_300_030)).await.unwrap();
    assert_eq!(ent_res.dict_get("isWarmupCycle").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(ent_res.dict_get("cycleIndex").and_then(|v| v.as_i64()), Some(1));

    let burst_type = ctx.sde.types.get(&997_014).unwrap().clone();
    let burst_state = DogmaItemState::from_sde(1_300_014, 997_014, &ctx.sde);
    ctx.db.with_mut_snapshot(|snap| snap.characters.get_mut(&c1).unwrap().clone_grade = 0);
    let fit = ctx.get_or_rebuild_ship_fitting(c1, titan_id).unwrap();
    assert_eq!(activate_command_burst(&ctx, &sys, c1, titan_id, null_sys, 1_300_014, &burst_type, &burst_state, None, &fit).await.unwrap_err().code, "OmegaRequiredForCommandBurst");
    ctx.db.with_mut_snapshot(|snap| snap.characters.get_mut(&c1).unwrap().clone_grade = 1);
    assert_eq!(activate_command_burst(&ctx, &sys, c1, titan_id, null_sys, 1_300_014, &burst_type, &burst_state, None, &fit).await.unwrap_err().code, "NoChargesLoaded");
    put_item(&ctx, 1_300_022, 997_015, c1, titan_id, InventoryFlag::HiSlot2, 10, false);
    let mut fit = ctx.get_or_rebuild_ship_fitting(c1, titan_id).unwrap();
    assert_eq!(activate_command_burst(&ctx, &sys, c1, titan_id, null_sys, 1_300_014, &burst_type, &burst_state, Some(997_015), &fit).await.unwrap_err().code, "SkillMissing");
    fit.character_skills.insert(3350, 5);
    fit.character_skills.insert(3351, 4);
    let b_res = activate_command_burst(&ctx, &sys, c1, titan_id, null_sys, 1_300_014, &burst_type, &burst_state, Some(997_015), &fit).await.unwrap();
    assert_eq!(b_res.dict_get("active").and_then(|v| v.as_bool()), Some(true));
}

#[tokio::test]
async fn scanners_analyzers_dbuffs_and_jump_clone_rpcs() {
    let ctx = make_ctx();
    let sys_id = 30_000_142;
    let c1 = 90_000_001;
    let c2 = 90_000_002;
    let ship_id = 1_400_001;
    let br_id = 1_400_002;
    let cont_id = 1_400_003;
    put_item(&ctx, ship_id, 997_021, c1, sys_id as i64, InventoryFlag::None, 1, true);
    put_item(&ctx, br_id, 997_022, c2, sys_id as i64, InventoryFlag::None, 1, true);
    put_item(&ctx, cont_id, 23, c2, sys_id as i64, InventoryFlag::Locked, 1, true);
    put_item(&ctx, 1_400_004, 16274, c2, br_id, InventoryFlag::Cargo, 100, false);

    {
        let mut st = scanners_superweapons_state().write().unwrap();
        st.ship_dbuffs.insert((ctx.instance_id, ship_id), vec![
            ActiveDbuffEntry { dbuff_id: 99_999_991, value: 1.5, expires_at_unix: 1_900_000_000, source_module_id: 1 },
            ActiveDbuffEntry { dbuff_id: 99_999_991, value: 0.5, expires_at_unix: 1_700_000_000, source_module_id: 2 },
        ]);
    }
    assert!(get_aggregated_ship_dbuffs(&ctx, ship_id, 1_800_000_000).contains_key(&99_999_991));
    let mut attrs = HashMap::from([(37u16, 100.0)]);
    apply_active_dbuffs_and_system_effects_to_ship_attrs(&ctx, ship_id, &mut attrs);

    let mut sess = SessionState {
        user_id: Some(1),
        character_id: Some(c1),
        corporation_id: Some(98_000_001),
        solar_system_id: Some(sys_id),
        ship_id: Some(ship_id),
        ..Default::default()
    };
    let kw = HashMap::new();
    let dbuff_state = handle_superweapons_dbuffs_and_clones(&ctx, &mut sess, None, "dogmaIM", "GetDbuffStateForClient", &[PyRep::Integer(ship_id)], &kw).await.unwrap().unwrap();
    assert!(!matches!(dbuff_state, PyRep::None));
    assert_eq!(handle_superweapons_dbuffs_and_clones(&ctx, &mut sess, None, "dogmaIM", "ApplyBuffsToItems", &[], &kw).await.unwrap().unwrap_err().code, "GMRoleRequired");
    sess.role |= service_roles::ROLE_GML;
    assert!(handle_superweapons_dbuffs_and_clones(&ctx, &mut sess, None, "dogmaIM", "RefreshSystemWideEnvironment", &[PyRep::Integer(sys_id as i64)], &kw).await.unwrap().is_ok());
    assert!(handle_superweapons_dbuffs_and_clones(&ctx, &mut sess, None, "dogmaIM", "ResetSystemWideEffects", &[PyRep::Integer(sys_id as i64)], &kw).await.unwrap().is_ok());

    let sys = ctx.systems.get_or_spawn(sys_id);
    spawn_ball(&ctx, sys_id, ship_id, 997_021, 25, 6, c1, DVec3::ZERO).await;
    spawn_ball(&ctx, sys_id, cont_id, 23, 2, 2, c2, DVec3::new(3_000.0, 0.0, 0.0)).await;
    spawn_ball(&ctx, sys_id, br_id, 997_022, 1202, 6, c2, DVec3::new(50_000.0, 0.0, 0.0)).await;

    let relic_t = ctx.sde.types.get(&997_016).unwrap().clone();
    let relic_s = DogmaItemState::from_sde(10, 997_016, &ctx.sde);
    assert_eq!(handle_scanner_analyzer_entosis_activation(&ctx, &sys, c1, ship_id, 10, &relic_t, &relic_s, None).await.unwrap().unwrap_err().code, "TargetRequired");
    let _ = handle_scanner_analyzer_entosis_activation(&ctx, &sys, c1, ship_id, 10, &relic_t, &relic_s, Some(cont_id)).await.unwrap().unwrap();
    assert_eq!(active_hacking_target_for_char(c1), Some(cont_id));
    mark_hacking_container_unlocked(&ctx, c1, cont_id);
    assert_eq!(ctx.db.with_snapshot(|s| s.items[&cont_id].flag), InventoryFlag::None);

    let cargo_t = ctx.sde.types.get(&997_017).unwrap().clone();
    let cargo_s = DogmaItemState::from_sde(11, 997_017, &ctx.sde);
    spawn_ball(&ctx, sys_id, cont_id, 997_021, 12, 2, c2, DVec3::new(40_000.0, 0.0, 0.0)).await;
    assert_eq!(handle_scanner_analyzer_entosis_activation(&ctx, &sys, c1, ship_id, 11, &cargo_t, &cargo_s, Some(cont_id)).await.unwrap().unwrap_err().code, "TargetOutOfRange");
    spawn_ball(&ctx, sys_id, br_id, 997_022, 1202, 6, c2, DVec3::new(5_000.0, 0.0, 0.0)).await;
    let br_scan = handle_scanner_analyzer_entosis_activation(&ctx, &sys, c1, ship_id, 11, &cargo_t, &cargo_s, Some(br_id)).await.unwrap().unwrap();
    assert_eq!(br_scan.dict_get("cargoScanImmune").and_then(|v| v.as_bool()), Some(true));

    let ship_scan_t = ctx.sde.types.get(&997_018).unwrap().clone();
    let ship_scan_s = DogmaItemState::from_sde(12, 997_018, &ctx.sde);
    assert!(handle_scanner_analyzer_entosis_activation(&ctx, &sys, c1, ship_id, 12, &ship_scan_t, &ship_scan_s, Some(br_id)).await.unwrap().is_ok());

    let c1_corp = ctx.db.with_snapshot(|s| s.characters[&c1].corporation_id);
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(1_400_050, UpwellStructureRecord {
            structure_id: 1_400_050,
            solar_system_id: sys_id,
            type_id: 35832,
            owner_corp_id: c1_corp,
            name: "Own Citadel".into(),
            ..Default::default()
        });
        snap.structures.insert(1_400_051, UpwellStructureRecord {
            structure_id: 1_400_051,
            solar_system_id: sys_id,
            type_id: 35832,
            owner_corp_id: 98_000_002,
            name: "Enemy Citadel".into(),
            ..Default::default()
        });
    });
    spawn_ball(&ctx, sys_id, 1_400_050, 35832, 1657, 65, c1_corp, DVec3::new(1_000.0, 0.0, 0.0)).await;
    spawn_ball(&ctx, sys_id, 1_400_051, 35832, 1657, 65, 98_000_002, DVec3::new(1_200.0, 0.0, 0.0)).await;
    let data_t = ctx.sde.types.get(&997_019).unwrap().clone();
    let data_s = DogmaItemState::from_sde(13, 997_019, &ctx.sde);
    assert_eq!(handle_scanner_analyzer_entosis_activation(&ctx, &sys, c1, ship_id, 13, &data_t, &data_s, Some(1_400_050)).await.unwrap().unwrap_err().code, "CannotHackOwnStructure");
    assert_eq!(handle_scanner_analyzer_entosis_activation(&ctx, &sys, c1, ship_id, 10, &relic_t, &relic_s, Some(1_400_051)).await.unwrap().unwrap_err().code, "DataAnalyzerRequired");
}

#[tokio::test]
async fn fitting_ship_bays_board_jettison_drop_and_scoop_deep() {
    let ctx = make_ctx();
    let sys_id = 30_000_142;
    let zarzakh_id = 30_100_000;
    let c1 = 90_000_001;
    let c2 = 90_000_002;
    let carrier_id = 1_500_001;
    let frig_id = 1_500_002;
    let stored_frig = 1_500_003;
    let cargo_stack = 1_500_004;
    let deploy_id = 1_500_005;
    let jetcan_id = 1_500_006;
    put_item(&ctx, carrier_id, 997_020, c1, sys_id as i64, InventoryFlag::None, 1, true);
    put_item(&ctx, frig_id, 997_021, c1, sys_id as i64, InventoryFlag::None, 1, true);
    put_item(&ctx, stored_frig, 997_021, c1, carrier_id, InventoryFlag::ShipHangar, 1, true);
    put_item(&ctx, cargo_stack, 34, c1, carrier_id, InventoryFlag::Cargo, 100, false);
    put_item(&ctx, deploy_id, 997_023, c1, carrier_id, InventoryFlag::Cargo, 2, false);
    put_item(&ctx, jetcan_id, 34, 0, sys_id as i64, InventoryFlag::None, 1, true);

    spawn_ball(&ctx, sys_id, carrier_id, 997_020, 485, 6, c1, DVec3::ZERO).await;
    spawn_ball(&ctx, sys_id, frig_id, 997_021, 25, 6, c1, DVec3::new(1_000.0, 0.0, 0.0)).await;
    spawn_ball(&ctx, sys_id, jetcan_id, 34, 18, 4, 0, DVec3::new(1_500.0, 0.0, 0.0)).await;

    let mut sess = SessionState {
        user_id: Some(1),
        character_id: Some(c1),
        corporation_id: Some(98_000_001),
        solar_system_id: Some(sys_id),
        ship_id: Some(carrier_id),
        corp_role: corp_roles::ROLE_EQUIPMENT_CONFIG,
        ..Default::default()
    };
    let kw = HashMap::new();

    assert!(ship_drop(&ctx, &mut sess, &[PyRep::Integer(deploy_id), PyRep::Integer(12_345)], &kw).await.is_err());
    let dropped_ids = ship_drop(&ctx, &mut sess, &[PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(deploy_id), PyRep::Integer(1)])])], &kw).await.unwrap();
    if let PyRep::Dict(v) = dropped_ids { assert_eq!(v.len(), 1); } else { panic!("expected Dict"); }
    assert!(ship_drop(&ctx, &mut sess, &[PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(deploy_id), PyRep::Integer(99)])])], &kw).await.is_err());

    let jettisoned = ship_jettison(&ctx, &mut sess, &[PyRep::List(vec![PyRep::Integer(cargo_stack)])], &kw).await.unwrap();
    if let PyRep::Tuple(v) = jettisoned { assert_eq!(v.len(), 2); } else { panic!("expected Tuple"); }
    assert_eq!(ship_jettison(&ctx, &mut sess, &[PyRep::List(vec![PyRep::Integer(cargo_stack)])], &kw).await.unwrap_err().code, "JettisonCooldownActive");

    assert!(ship_scoop_and_store(&ctx, &mut sess, "StoreVessel", &[PyRep::Integer(frig_id), PyRep::Integer(carrier_id)], &kw).await.is_ok());
    assert_eq!(ctx.db.with_snapshot(|s| s.items[&frig_id].flag), InventoryFlag::ShipHangar);
    assert!(ship_scoop_and_store(&ctx, &mut sess, "Scoop", &[PyRep::Integer(jetcan_id)], &kw).await.is_ok());

    write_container_ship_state(&ctx, |st| {
        st.ship_configs.insert(carrier_id, ShipAccessConfig { allow_corp_smb: true, allow_fleet_hangar: true, ..Default::default() });
    });
    let mut sess2 = SessionState {
        user_id: Some(2),
        character_id: Some(c2),
        corporation_id: Some(98_000_001),
        solar_system_id: Some(sys_id),
        ship_id: Some(1_500_099),
        corp_role: corp_roles::ROLE_EQUIPMENT_CONFIG,
        ..Default::default()
    };
    assert_eq!(board_stored_ship(&ctx, &mut sess2, &[PyRep::Integer(stored_frig)], &kw).await.unwrap_err().code, "SMAAccessDenied");
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&c1).unwrap().corporation_id = 98_000_001;
        snap.characters.get_mut(&c2).unwrap().corporation_id = 98_000_001;
    });
    assert_eq!(board_stored_ship(&ctx, &mut sess2, &[PyRep::Integer(stored_frig)], &kw).await.unwrap_err().code, "OwnershipViolation");
    ctx.db.with_mut_snapshot(|snap| snap.items.get_mut(&stored_frig).unwrap().owner_id = c2);
    assert_eq!(board_stored_ship(&ctx, &mut sess2, &[PyRep::Integer(stored_frig)], &kw).await.unwrap_err().code, "ShipNotInSpace");
    spawn_ball(&ctx, sys_id, 1_500_099, 997_021, 25, 6, c2, DVec3::new(10_000.0, 0.0, 0.0)).await;
    assert_eq!(board_stored_ship(&ctx, &mut sess2, &[PyRep::Integer(stored_frig)], &kw).await.unwrap_err().code, "ShipOutOfBoardingRange");

    put_item(&ctx, deploy_id + 10, 33474, c1, carrier_id, InventoryFlag::Cargo, 1, false);
    sess.solar_system_id = Some(zarzakh_id);
    assert_eq!(ship_drop(&ctx, &mut sess, &[PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(deploy_id + 10), PyRep::Integer(1)])])], &kw).await.unwrap_err().code, "AnchoringDisallowedInZarzakh");
}
