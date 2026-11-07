//! Adversarial regression tests for Round 8 Shard D:
//! - `#499`: AoE Titan Doomsdays cancelled on Titan death / leave-space, dead caster balls purged in
//!   `step_bosonic_doomsday_system`, and fuel consumption restricted to `Cargo` (`5`) / `SpecializedFuelBay` (`133`)
//!   (preferring `133`) while rejecting contract-locked isotope stacks.
//! - `#501`: Recursive nested SMA ship & cargo container Loot Fairy rolls, rig destruction, multi-depth orphan
//!   cleanup, full nested killmail ISK valuation, and disconnected-ship capsule + cyno cleanup on turret kills.
//! - `#511`: Mobile Depot anchoring/unanchored destruction full cleanup (`RemoveBall`, DB/deployables, `5 km`
//!   exclusion zone), pre-reinforcement shield resistances, and MMJU immobilization + POS forcefield checks.

#![forbid(unsafe_code)]

use crate::common;
use eve_db::{
    ContractRecord, DisconnectedShipEntry, InventoryFlag, InventoryItemRecord,
    StarbaseDetailSettings,
};
use eve_destiny::{Ball, Ballpark, CombatState, DamageVector, DestinyEvent, MovementMode};
use eve_proto::PyRep;
use eve_server::combat::death_pipeline::execute_death_pipeline_sync_inner;
use eve_server::router::ServerContext;
use eve_server::services::combat_crimewatch::with_combat_runtime;
use eve_server::services::combat_weapons_spool::handle_ship_or_npc_destruction;
use eve_server::services::doomsday_aoe_ops::activate_cone_doomsday;
use eve_server::services::doomsday_aoe_tick::{
    mutate_superweapon_runtime, read_superweapon_runtime, step_bosonic_doomsday_system,
    PersistedSuperweaponCycle, SuperweaponCycleKind,
};
use eve_server::services::mobile_deployables::{
    check_mobile_deployment_restrictions, handle_activate_micro_jump_unit,
    handle_ball_damage_in_actor, read_deployable_state, register_mobile_deployable,
    GROUP_MOBILE_DEPOT, GROUP_MOBILE_MICRO_JUMP_UNIT, TYPE_MOBILE_DEPOT, TYPE_MOBILE_MJU,
};
use eve_server::services::mobile_deployables_ext::step_system_deployables_sync;
use eve_server::services::space_navigation_advanced::find_attr_id_by_name;
use glam::DVec3;
use std::sync::Arc;

const NULLSEC_SYS: u32 = 30_000_021;
const AVATAR: u32 = 11_567;
const ARCHON: u32 = 23_757;
const VEXOR: u32 = 626;
const RIFTER: u32 = 587;

fn paused_ctx() -> ServerContext {
    let sde = Arc::new(eve_sde::SdeRegistry::open(&eve_sde::SdeRegistry::default_db_path()).expect("sde"));
    let ctx = ServerContext::with_sde_and_repository(sde, Arc::new(eve_db::WorldRepository::with_demo_seed()));
    ctx.set_clock_paused(true);
    ctx
}

fn seed_item(
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

#[tokio::test]
async fn test_r8_499_aoe_doomsday_fuel_flags_contract_locks_and_post_death_cancellation() {
    // regression: feat:modules.doomsday-bosonic-field, feat:modules.doomsday-gtfo-reaper
    let ctx = paused_ctx();
    let titan = common::gm_pilot_in_space(&ctx, 1, 91_499_001, AVATAR, NULLSEC_SYS).await;
    let victim = common::gm_pilot_in_space(&ctx, 2, 91_499_002, VEXOR, NULLSEC_SYS).await;
    let sys = ctx.systems.get_or_spawn(NULLSEC_SYS);

    assert!(sys.set_ball_position(titan.ship_id, DVec3::ZERO).await);
    assert!(sys.set_ball_position(victim.ship_id, DVec3::new(10_000.0, 0.0, 0.0)).await);

    let (bfg_tid, fuel_tid, fuel_qty) = ctx
        .sde
        .types
        .values()
        .find_map(|t| {
            let ft = t.default_attributes.get(&713).copied().map(|v| v as u32)?;
            let fq = t.default_attributes.get(&714).copied().map(|v| v as i32)?;
            (t.name.contains("Bosonic Field Generator") && ft > 0 && fq > 0).then_some((t.type_id, ft, fq))
        })
        .unwrap_or((24_550, 16_274, 1_000));

    let mod_id = 849_900_001_i64;
    seed_item(&ctx, mod_id, bfg_tid, titan.char_id, titan.ship_id, InventoryFlag::HiSlot0, 1, true);
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(mod_id, true);
    });

    // 1. Isotopes in DroneBay (wrong flag) AND contract-locked Cargo stack must BOTH be rejected
    let drone_bay_fuel = 849_900_010_i64;
    let contract_locked_fuel = 849_900_011_i64;
    seed_item(&ctx, drone_bay_fuel, fuel_tid, titan.char_id, titan.ship_id, InventoryFlag::DroneBay, fuel_qty * 2, false);
    seed_item(&ctx, contract_locked_fuel, fuel_tid, titan.char_id, titan.ship_id, InventoryFlag::Cargo, fuel_qty * 2, false);
    ctx.db.with_mut_snapshot(|snap| {
        snap.contracts.insert(
            77_499_001,
            ContractRecord {
                contract_id: 77_499_001,
                issuer_id: titan.char_id,
                contract_type: "ItemExchange".into(),
                status: "Outstanding".into(),
                item_ids: vec![contract_locked_fuel],
                ..Default::default()
            },
        );
    });

    let dd_args = [
        PyRep::Integer(mod_id),
        PyRep::Tuple(vec![PyRep::Real(1.0), PyRep::Real(0.0), PyRep::Real(0.0)]),
    ];
    let bad_fuel_res = activate_cone_doomsday(&ctx, titan.char_id, titan.ship_id, NULLSEC_SYS, mod_id, &dd_args).await;
    assert!(bad_fuel_res.is_err(), "doomsday must reject DroneBay and contract-locked isotopes");
    assert_eq!(ctx.db.get_item(drone_bay_fuel).unwrap().quantity, fuel_qty * 2);
    assert_eq!(ctx.db.get_item(contract_locked_fuel).unwrap().quantity, fuel_qty * 2);

    // 2. Fuel in Cargo (5) + SpecializedFuelBay (133): must prefer SpecializedFuelBay (133) first!
    let cargo_fuel = 849_900_012_i64;
    let fuel_bay_stack = 849_900_013_i64;
    seed_item(&ctx, cargo_fuel, fuel_tid, titan.char_id, titan.ship_id, InventoryFlag::Cargo, fuel_qty, false);
    seed_item(
        &ctx,
        fuel_bay_stack,
        fuel_tid,
        titan.char_id,
        titan.ship_id,
        InventoryFlag::from_u16(133),
        fuel_qty + 250,
        false,
    );
    activate_cone_doomsday(&ctx, titan.char_id, titan.ship_id, NULLSEC_SYS, mod_id, &dd_args)
        .await
        .expect("Bosonic Field Generator should activate using SpecializedFuelBay");
    assert_eq!(ctx.db.get_item(fuel_bay_stack).unwrap().quantity, 250, "SpecializedFuelBay must be consumed first");
    assert_eq!(ctx.db.get_item(cargo_fuel).unwrap().quantity, fuel_qty, "Cargo fuel untouched when FuelBay suffices");
    assert_eq!(read_superweapon_runtime(&ctx.db).unwrap().active_cycles.len(), 1);

    // 3. Destroying the Titan immediately cancels its active superweapon cycle AND dead caster cycles deal 0 damage
    let victim_shield_before = sys.get_combat_state(victim.ship_id).await.unwrap().unwrap().shield_hp;
    execute_death_pipeline_sync_inner(&ctx, NULLSEC_SYS, titan.char_id, titan.ship_id, victim.char_id, DVec3::ZERO, 99, false)
        .expect("destroy Titan");
    assert!(
        read_superweapon_runtime(&ctx.db)
            .map_or(true, |rt| rt.active_cycles.iter().all(|c| c.ship_id != titan.ship_id)),
        "death_pipeline must purge active superweapon cycles for destroyed Titan"
    );

    // Inject a ghost cycle for a non-existent caster ball and verify step_bosonic_doomsday_system purges it without firing
    let now = ctx.now_unix_secs();
    mutate_superweapon_runtime(&ctx.db, |rt| {
        rt.active_cycles.push(PersistedSuperweaponCycle {
            sys_id: NULLSEC_SYS,
            char_id: titan.char_id,
            ship_id: 999_888_777,
            ship_type_id: AVATAR,
            mod_type_id: bfg_tid,
            kind: SuperweaponCycleKind::Cone {
                origin_pos: [0.0, 0.0, 0.0],
                dir_vec: [1.0, 0.0, 0.0],
                cone_len_m: 50_000.0,
                tick_dmg: [50_000.0; 4],
                sig_radius_m: 100.0,
            },
            started_at_unix: now.saturating_sub(30),
            warmup_secs: 5,
            total_ticks: 5,
            ticks_applied: 0,
            immobilize_until_unix: now,
            orig_max_velocity: 80.0,
            orig_warp_scramble_strength: 0,
            orig_disallow_assistance: false,
            mobility_restored: true,
        });
    });
    ctx.advance_secs(20);
    step_bosonic_doomsday_system(&ctx, NULLSEC_SYS).await;
    let victim_shield_after = sys.get_combat_state(victim.ship_id).await.unwrap().unwrap().shield_hp;
    assert_eq!(victim_shield_after, victim_shield_before, "dead Titan cycle must never damage targets");
    assert!(read_superweapon_runtime(&ctx.db).map_or(true, |rt| rt.active_cycles.is_empty()));
}

#[tokio::test]
async fn test_r8_501_nested_sma_and_container_loot_rigs_orphans_and_turret_disconnected_capsule() {
    // regression: feat:ships.ship-maintenance-bay, feat:lifecycle.world-unobserved-system-sim
    let ctx = paused_ctx();
    let c1 = common::gm_pilot_in_space(&ctx, 1, 91_501_001, ARCHON, NULLSEC_SYS).await;
    let attacker = common::gm_pilot_in_space(&ctx, 2, 91_501_002, VEXOR, NULLSEC_SYS).await;

    // 1. Self-destruct (drop_threshold == 0.0) with 3-level nested containers: zero 2nd/3rd-level orphans & full nested ISK!
    let c1_cont_lvl1 = 850_100_001_i64;
    let c1_cont_lvl2 = 850_100_002_i64;
    let c1_deep_plex = 850_100_003_i64;
    seed_item(&ctx, c1_cont_lvl1, 3_293, c1.char_id, c1.ship_id, InventoryFlag::Cargo, 1, true);
    seed_item(&ctx, c1_cont_lvl2, 3_293, c1.char_id, c1_cont_lvl1, InventoryFlag::Cargo, 1, true);
    seed_item(&ctx, c1_deep_plex, 44_992, c1.char_id, c1_cont_lvl2, InventoryFlag::Cargo, 1_000, false);
    let plex_unit_isk = ctx.sde.types.get(&44_992).map(|t| t.base_price).filter(|&p| p > 0.0).unwrap_or(100.0);

    let sd_res = execute_death_pipeline_sync_inner(
        &ctx,
        NULLSEC_SYS,
        c1.char_id,
        c1.ship_id,
        attacker.char_id,
        DVec3::ZERO,
        7,
        true,
    )
    .expect("self-destruct carrier with 3-level nested items");
    assert!(ctx.db.get_item(c1_cont_lvl1).is_none());
    assert!(ctx.db.get_item(c1_cont_lvl2).is_none(), "2nd-level nested container must be deleted");
    assert!(ctx.db.get_item(c1_deep_plex).is_none(), "3rd-level nested item must not leak as orphan");
    let sd_km = ctx.db.with_snapshot(|s| s.killmails.get(&sd_res.killmail_id).cloned()).unwrap();
    assert!(sd_km.total_value_isk >= plex_unit_isk * 1_000.0);

    // 2. Turret kill (handle_ship_or_npc_destruction) of disconnected Carrier with SMA ship + rig + nested cargo:
    //    - SMA hull rig (flag 92) is ALWAYS destroyed even if the SMA hull survives;
    //    - Nested items inside surviving SMA hull / container roll ~50% (not 100% preserved);
    //    - DisconnectedShipEntry updates to spawned capsule (670) instead of being deleted!
    let c2 = common::gm_pilot_in_space(&ctx, 3, 91_501_003, ARCHON, NULLSEC_SYS).await;
    let sys = ctx.systems.get_or_spawn(NULLSEC_SYS);
    let sma_ship = 850_100_100_i64;
    let sma_rig = 850_100_101_i64;
    seed_item(&ctx, sma_ship, RIFTER, c2.char_id, c2.ship_id, InventoryFlag::ShipHangar, 1, true);
    seed_item(&ctx, sma_rig, 31_716, c2.char_id, sma_ship, InventoryFlag::RigSlot0, 1, true);
    let nested_mod_ids: Vec<i64> = (0..24).map(|k| 850_100_200 + k).collect();
    for &mid in &nested_mod_ids {
        seed_item(&ctx, mid, 438, c2.char_id, sma_ship, InventoryFlag::Cargo, 1, true);
    }

    let now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        snap.sp6_lifecycle.disconnected_ships.insert(
            c2.char_id,
            DisconnectedShipEntry {
                char_id: c2.char_id,
                ship_id: c2.ship_id,
                ship_type_id: ARCHON,
                solar_system_id: NULLSEC_SYS,
                disconnected_at_unix: now,
                remove_at_unix: now + 900,
                emergency_warp_dest: Some([1.0e9, 0.0, 0.0]),
                last_position: [0.0, 0.0, 0.0],
                removed_from_space: false,
                pre_warp_position: None,
            },
        );
    });

    let (_, c2_ball) = sys.get_ball_snapshot(c2.ship_id).await.unwrap().unwrap();
    let (kill_id, _wreck_id, _corpse_id, _) = handle_ship_or_npc_destruction(
        &ctx,
        &sys,
        attacker.char_id,
        attacker.ship_id,
        VEXOR,
        3_074,
        c2.ship_id,
        &c2_ball,
        Some((c2.char_id, c2.ship_id)),
        NULLSEC_SYS,
        50_000.0,
        now,
    )
    .await;

    assert!(ctx.db.get_item(sma_rig).is_none(), "Rig fitted on SMA ship must always be destroyed");
    let surviving_nested = nested_mod_ids.iter().filter(|&&id| ctx.db.get_item(id).is_some()).count();
    assert!(
        surviving_nested < nested_mod_ids.len(),
        "Nested items inside SMA ship must roll Loot Fairy (never 100% preserved), got {surviving_nested}/24"
    );
    let dkm = with_combat_runtime(&ctx, |rt| rt.detailed_killmails.get(&kill_id).cloned()).expect("detailed killmail");
    let recorded_nested = dkm
        .dropped_items
        .iter()
        .chain(dkm.destroyed_items.iter())
        .filter(|e| e.type_id == 438)
        .count();
    assert_eq!(recorded_nested, nested_mod_ids.len(), "All 24 nested items must be accounted on the killmail");

    let new_active_pod = ctx.db.get_character(c2.char_id).unwrap().active_ship_id;
    let disc = ctx
        .db
        .with_snapshot(|s| s.sp6_lifecycle.disconnected_ships.get(&c2.char_id).cloned())
        .expect("disconnected victim entry must be updated to capsule, not deleted");
    assert_eq!(disc.ship_id, new_active_pod);
    assert_eq!(disc.ship_type_id, 670);
    assert!(disc.emergency_warp_dest.is_none());
}

#[tokio::test]
async fn test_r8_511_mobile_depot_anchoring_cleanup_shield_resists_and_mmju_restrictions() {
    // regression: feat:structures.mobile-depot-lifetime-reinforce, feat:structures.mobile-micro-jump-unit
    let ctx = paused_ctx();
    let pilot = common::gm_pilot_in_space(&ctx, 1, 91_511_001, VEXOR, NULLSEC_SYS).await;
    let other = common::gm_pilot_in_space(&ctx, 2, 91_511_002, VEXOR, NULLSEC_SYS).await;
    let now = ctx.now_unix();

    // 1. Destroying an anchoring Mobile Depot removes ball (emitting RemoveBall), cleans DB & deployables, and frees 5km exclusion zone
    let depot_pos = DVec3::new(5.0e8, 0.0, 0.0);
    let anch_depot_id = 851_100_001_i64;
    let depot_loot_id = 851_100_002_i64;
    seed_item(&ctx, anch_depot_id, TYPE_MOBILE_DEPOT, pilot.char_id, NULLSEC_SYS as i64, InventoryFlag::None, 1, true);
    seed_item(&ctx, depot_loot_id, 34, pilot.char_id, anch_depot_id, InventoryFlag::Cargo, 100, false);
    ctx.db.with_mut_snapshot(|s| {
        s.item_positions.insert(anch_depot_id, depot_pos.to_array());
    });
    register_mobile_deployable(
        &ctx,
        anch_depot_id,
        TYPE_MOBILE_DEPOT,
        GROUP_MOBILE_DEPOT,
        pilot.char_id,
        NULLSEC_SYS,
        60,
        depot_pos,
    );

    let mut bp = Ballpark::new(NULLSEC_SYS);
    let obs_id = 9_999_999_i64;
    let _ = bp.add_ball(Ball {
        id: obs_id,
        type_id: RIFTER,
        position: depot_pos + DVec3::new(200.0, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 1.0,
        mass: 1.0e6,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    });
    let _ = bp.add_ball(Ball {
        id: anch_depot_id,
        type_id: TYPE_MOBILE_DEPOT,
        position: depot_pos,
        velocity: DVec3::ZERO,
        max_velocity: 0.0,
        inertia: 1.0,
        mass: 10_000.0,
        radius: 50.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    });
    bp.combat_states.insert(
        anch_depot_id,
        CombatState {
            shield_hp: 1_000.0,
            max_shield_hp: 1_000.0,
            armor_hp: 1_000.0,
            max_armor_hp: 1_000.0,
            structure_hp: 1_000.0,
            max_structure_hp: 1_000.0,
            shield_resonance: [1.0; 4],
            armor_resonance: [1.0; 4],
            structure_resonance: [1.0; 4],
            ..Default::default()
        },
    );
    let _ = bp.drain_observer_events(obs_id);
    let db_opt = Some(Arc::clone(&ctx.db));
    let sde_opt = Some(Arc::clone(&ctx.sde));
    let (_, destroyed, _) = handle_ball_damage_in_actor(
        &db_opt,
        &sde_opt,
        NULLSEC_SYS,
        anch_depot_id,
        DamageVector::new(10_000.0, 0.0, 0.0, 0.0),
        &mut bp,
        now + 10,
    )
    .expect("anchoring depot damage");
    assert!(destroyed);
    assert!(!bp.balls.contains_key(&anch_depot_id));
    assert!(!bp.combat_states.contains_key(&anch_depot_id));
    assert!(bp.drain_observer_events(obs_id).iter().any(|ev| matches!(ev, DestinyEvent::RemoveBall { ball_id } if *ball_id == anch_depot_id)));
    assert!(ctx.db.get_item(anch_depot_id).is_none(), "destroyed anchoring depot must be removed from DB");
    assert!(!read_deployable_state(&ctx).deployables.contains_key(&anch_depot_id));
    check_mobile_deployment_restrictions(
        &ctx,
        &pilot.sess,
        851_100_099,
        TYPE_MOBILE_DEPOT,
        GROUP_MOBILE_DEPOT,
        NULLSEC_SYS,
        depot_pos + DVec3::new(100.0, 0.0, 0.0),
    )
    .expect("5km anchor exclusion zone must clear after anchoring depot destruction");

    // 2. Anchored Mobile Depot pre-reinforcement shield damage respects shield_resonance
    let anchored_id = 851_100_010_i64;
    seed_item(&ctx, anchored_id, TYPE_MOBILE_DEPOT, pilot.char_id, NULLSEC_SYS as i64, InventoryFlag::None, 1, true);
    register_mobile_deployable(&ctx, anchored_id, TYPE_MOBILE_DEPOT, GROUP_MOBILE_DEPOT, pilot.char_id, NULLSEC_SYS, 60, depot_pos);
    let _ = bp.add_ball(Ball {
        id: anchored_id,
        type_id: TYPE_MOBILE_DEPOT,
        position: depot_pos,
        velocity: DVec3::ZERO,
        max_velocity: 0.0,
        inertia: 1.0,
        mass: 10_000.0,
        radius: 50.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    });
    bp.combat_states.insert(
        anchored_id,
        CombatState {
            shield_hp: 1_000.0,
            max_shield_hp: 1_000.0,
            armor_hp: 1_000.0,
            max_armor_hp: 1_000.0,
            structure_hp: 1_000.0,
            max_structure_hp: 1_000.0,
            shield_resonance: [0.5, 0.5, 0.5, 0.5], // 50% shield resists
            ..Default::default()
        },
    );
    let (applied, dest2, _) = handle_ball_damage_in_actor(
        &db_opt,
        &sde_opt,
        NULLSEC_SYS,
        anchored_id,
        DamageVector::new(800.0, 0.0, 0.0, 0.0), // 800 raw -> 400 resisted (< 750 needed to hit 25% threshold!)
        &mut bp,
        now + 120,
    )
    .unwrap();
    assert!(!dest2);
    assert!((applied - 400.0).abs() < 1e-6, "expected 400 resisted shield dmg, got {applied}");
    assert!((bp.combat_states[&anchored_id].shield_hp - 600.0).abs() < 1e-6);
    assert!(read_deployable_state(&ctx).deployables[&anchored_id].reinforced_until_unix.is_none());

    // 3. MMJU rejects active immobilizer (Bastion/Siege), rejects destination POS forcefield, and aborts spool if locked mid-spool
    let sys = ctx.systems.get_or_spawn(NULLSEC_SYS);
    let mmju_origin = DVec3::new(-5.0e8, 0.0, 0.0);
    sys.try_stop_ball(pilot.ship_id);
    sys.set_movement_mode(pilot.ship_id, eve_destiny::MovementMode::Stop).await.unwrap();
    assert!(sys.set_ball_position(pilot.ship_id, mmju_origin).await);
    let mju_id = 851_100_020_i64;
    seed_item(&ctx, mju_id, TYPE_MOBILE_MJU, pilot.char_id, NULLSEC_SYS as i64, InventoryFlag::None, 1, true);
    ctx.db.with_mut_snapshot(|s| {
        s.item_positions.insert(mju_id, (mmju_origin + DVec3::new(500.0, 0.0, 0.0)).to_array());
    });
    register_mobile_deployable(&ctx, mju_id, TYPE_MOBILE_MJU, GROUP_MOBILE_MICRO_JUMP_UNIT, pilot.char_id, NULLSEC_SYS, 60, mmju_origin + DVec3::new(500.0, 0.0, 0.0));
    ctx.advance_secs(65);

    let siege_attr = find_attr_id_by_name(&ctx.sde, "siegeModeWarpStatus").expect("siegeModeWarpStatus");
    let bastion_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.default_attributes.get(&siege_attr).copied().unwrap_or(0.0) != 0.0)
        .map(|t| t.type_id)
        .expect("Bastion/Siege type in SDE");
    let immob_mod_id = 851_100_021_i64;
    seed_item(&ctx, immob_mod_id, bastion_tid, pilot.char_id, pilot.ship_id, InventoryFlag::HiSlot0, 1, true);
    ctx.db.with_mut_snapshot(|s| {
        s.module_active_states.insert(immob_mod_id);
    });
    let immob_err = handle_activate_micro_jump_unit(&ctx, &pilot.sess, mju_id)
        .await
        .expect_err("MMJU must reject active Bastion/Siege module");
    assert_eq!(immob_err.code, "CantJumpWhileModuleActive");
    ctx.db.with_mut_snapshot(|s| {
        s.module_active_states.remove(&immob_mod_id);
    });

    // Place a hostile Starbase Control Tower (group 365) at the 100 km destination (+X * 100_000)
    let tower_tid = ctx.sde.types.values().find(|t| t.group_id == 365).map(|t| t.type_id).expect("POS tower in SDE");
    let tower_id = 851_100_030_i64;
    let landing_pos = mmju_origin + DVec3::X * 100_000.0;
    let hostile_corp = 98_851_102_i64;
    seed_item(&ctx, tower_id, tower_tid, hostile_corp, NULLSEC_SYS as i64, InventoryFlag::None, 1, true);
    ctx.db.with_mut_snapshot(|s| {
        if let Some(ch) = s.characters.get_mut(&other.char_id) {
            ch.corporation_id = hostile_corp;
        }
        s.item_positions.insert(tower_id, landing_pos.to_array());
        s.starbase_settings.insert(
            tower_id,
            StarbaseDetailSettings {
                password: Some("hostile-pos".into()),
                allow_corporation_members: false,
                allow_alliance_members: false,
                ..Default::default()
            },
        );
    });
    let ff_err = handle_activate_micro_jump_unit(&ctx, &pilot.sess, mju_id)
        .await
        .expect_err("MMJU must reject jumping into unauthorized POS forcefield");
    assert_eq!(ff_err.code, "ForcefieldAccessDenied");

    // Move tower far away temporarily so activation starts spool, then place hostile forcefield at landing_pos mid-spool -> jump cancelled!
    ctx.db.with_mut_snapshot(|s| {
        s.item_positions.insert(tower_id, [9.0e9, 0.0, 0.0]);
    });
    handle_activate_micro_jump_unit(&ctx, &pilot.sess, mju_id).await.expect("MMJU starts spooling");
    ctx.db.with_mut_snapshot(|s| {
        s.item_positions.insert(tower_id, landing_pos.to_array());
    });
    ctx.advance_secs(15);
    step_system_deployables_sync(&ctx, NULLSEC_SYS);
    let (_, post_ball) = sys.get_ball_snapshot(pilot.ship_id).await.unwrap().unwrap();
    assert!(
        post_ball.position.distance(mmju_origin) < 1_000.0,
        "MMJU spool completion must abort when landing inside unauthorized POS forcefield"
    );
}
