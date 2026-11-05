//! Extra unit and mutation-killing tests for `services::combat_crimewatch::combat_weapons_spool`
//! covering default spool attributes, inactive DB state reactivation, target unlock all-spool clearing,
//! capacitor/lock/death stop conditions in `step_system_entropic_spools`, ammo fallback & depletion,
//! and player ship/pod/abyssal/sleeper/drifter destruction paths in `handle_ship_or_npc_destruction`.

use crate::router::ServerContext;
use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::{
    make_fighter_ctx_and_session, make_fighter_item, make_fighter_type,
};
use crate::services::combat_crimewatch::combat_weapons_spool::*;
use crate::services::combat_crimewatch::{
    with_combat_runtime, with_mut_combat_runtime, AssistLogEntry,
};
use eve_db::InventoryFlag;
use eve_destiny::{Ball, CombatState, DamageVector, MovementMode, TargetLock};
use eve_dogma::{FittedModule, ModuleState, ShipFittingEngine};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use glam::DVec3;
use std::collections::HashMap;
use std::sync::Arc;

async fn spawn_spool_ball(
    ctx: &ServerContext,
    sys_id: u32,
    id: i64,
    type_id: u32,
    pos: DVec3,
    shield: f64,
    armor: f64,
    hull: f64,
    cap: f64,
) -> Ball {
    let ball = Ball {
        id,
        type_id,
        position: pos,
        velocity: DVec3::ZERO,
        max_velocity: 250.0,
        inertia: 1.0,
        mass: 1_000_000.0,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    ctx.systems
        .get_or_spawn(sys_id)
        .spawn_ball_with_combat(
            ball.clone(),
            CombatState {
                shield_hp: shield,
                max_shield_hp: shield.max(100.0),
                armor_hp: armor,
                max_armor_hp: armor.max(100.0),
                structure_hp: hull,
                max_structure_hp: hull.max(100.0),
                capacitor: cap,
                max_capacitor: cap.max(100.0),
                signature_radius: 120.0,
                base_signature_radius: Some(120.0),
                ..CombatState::default()
            },
        )
        .await
        .unwrap();
    ball
}

fn make_spool_test_ctx() -> (ServerContext, SessionState, i64, i64, u32) {
    let (mut ctx, session, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    let mut sde = (*ctx.sde).clone();
    sde.types.insert(
        47_271,
        make_fighter_type(
            47_271,
            26,
            6,
            10_000.0,
            vec![(263, 1_000.0), (265, 1_000.0), (9, 1_000.0)],
        ),
    );
    sde.types.insert(
        24_690,
        make_fighter_type(
            24_690,
            27,
            6,
            50_000.0,
            vec![(3020, 1.0), (263, 2_000.0), (265, 2_000.0), (9, 2_000.0)],
        ),
    );
    sde.types
        .insert(670, make_fighter_type(670, 29, 6, 500.0, vec![(9, 400.0)]));
    sde.types.insert(
        11_393,
        make_fighter_type(11_393, 324, 6, 2_500.0, vec![(9, 400.0)]),
    );
    let mut disint = make_fighter_type(
        47_240,
        1986,
        7,
        10.0,
        vec![
            (54, 20_000.0),
            (158, 10_000.0),
            (160, 0.5),
            (620, 40_000.0),
            (604, 1987.0),
            (2733, 0.10),
            (2734, 1.50),
            (137, 47_245.0),
        ],
    );
    disint.effect_ids = vec![42];
    sde.types.insert(47_240, disint);
    sde.types.insert(
        47_245,
        make_fighter_type(47_245, 1987, 8, 0.01, vec![(118, 200.0), (116, 100.0)]),
    );
    sde.types
        .insert(587, make_fighter_type(587, 25, 6, 2_500.0, vec![(9, 300.0)]));
    sde.types.insert(
        500_001,
        make_fighter_type(500_001, 550, 11, 5_000.0, vec![(481, 150_000.0)]),
    );
    ctx.sde = Arc::new(sde);
    ctx.db
        .acquire_online_lock(1, 1, Some(char_id), true, ctx.now_unix())
        .unwrap();
    (ctx, session, char_id, ship_id, sys_id)
}

#[test]
fn entropic_spool_defaults_inactive_reactivation_and_unlock_all_targets() {
    let (mut ctx, _session, char_id, ship_id, sys_id) = make_spool_test_ctx();
    let sde_mut = Arc::make_mut(&mut ctx.sde);
    // Disintegrator without 2733/2734 attributes -> uses defaults per_cycle=0.07, max_bonus=2.125
    let mut bare_disint = make_fighter_type(47_299, 1986, 7, 10.0, vec![(604, 1.0)]);
    bare_disint.effect_ids.clear();
    sde_mut.types.insert(47_299, bare_disint);

    // 1. Advancing a non-existent spool returns (0, 1.0)
    assert_eq!(advance_entropic_spool_cycle(&ctx, ship_id, 999_999), (0, 1.0));

    // 2. Activate bare_disint -> uses default 0.07 per cycle and 2.125 max bonus (cap 3.125)
    let bare_t = ctx.sde.types.get(&47_299).unwrap();
    let mod_a = 741_001i64;
    let mod_b = 741_002i64;
    let (c0, m0) = handle_entropic_activation(
        &ctx,
        char_id,
        ship_id,
        sys_id,
        mod_a,
        300_100,
        bare_t,
        None,
        DamageVector::new(100.0, 100.0, 0.0, 0.0),
        10.0,
        2.5,
    )
    .unwrap();
    assert_eq!(c0, 0);
    assert!((m0 - 1.0).abs() < 1e-6);

    let (c1, m1) = advance_entropic_spool_cycle(&ctx, ship_id, mod_a);
    assert_eq!(c1, 1);
    assert!((m1 - 1.07).abs() < 1e-6);

    for _ in 0..40 {
        advance_entropic_spool_cycle(&ctx, ship_id, mod_a);
    }
    assert!((get_entropic_spool_multiplier(&ctx, ship_id, mod_a) - 3.125).abs() < 1e-6);

    // 3. Reactivating on the SAME target when module is NOT in snap.module_active_states resets to (0, 1.0)
    let (cr, mr) = handle_entropic_activation(
        &ctx,
        char_id,
        ship_id,
        sys_id,
        mod_a,
        300_100,
        bare_t,
        None,
        DamageVector::new(100.0, 100.0, 0.0, 0.0),
        10.0,
        2.5,
    )
    .unwrap();
    assert_eq!(cr, 0);
    assert!((mr - 1.0).abs() < 1e-6);

    // 4. Seed fitting with active module and test clear_entropic_spools_on_target_unlock(..., None)
    let mut fit = ShipFittingEngine::from_sde(ship_id, 47_271, &ctx.sde);
    fit.modules.insert(
        mod_a,
        FittedModule {
            item_id: mod_a,
            type_id: 47_299,
            flag_id: 27,
            state: ModuleState::Active,
            charge_type_id: None,
            charge_quantity: 0,
            damage_hp: 0.0,
            max_hp: 40.0,
            dynamic_attribute_overrides: HashMap::new(),
        },
    );
    ctx.store_ship_fitting(ship_id, fit);
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_active_states.insert(mod_a);
        snap.module_active_states.insert(mod_b);
        snap.module_active_charges.insert(mod_a, 47_245);
    });
    handle_entropic_activation(
        &ctx,
        char_id,
        ship_id,
        sys_id,
        mod_b,
        300_200,
        bare_t,
        None,
        DamageVector::new(100.0, 100.0, 0.0, 0.0),
        10.0,
        2.5,
    )
    .unwrap();

    // Passing target_id_opt = None stops ALL spools on ship_id and deactivates fitted modules
    clear_entropic_spools_on_target_unlock(&ctx, ship_id, char_id, None);
    assert!(!with_combat_runtime(&ctx, |rt| rt.entropic_spools.contains_key(&(ship_id, mod_a))));
    assert!(!with_combat_runtime(&ctx, |rt| rt.entropic_spools.contains_key(&(ship_id, mod_b))));
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.module_active_states.contains(&mod_a));
        assert!(!snap.module_active_charges.contains_key(&mod_a));
    });
    let notifs = ctx.drain_notifications_for_char(char_id);
    assert!(notifs.iter().any(|n| n.event == "OnModulesDeactivating"));
    assert!(notifs.iter().any(|n| n.event == "OnGodmaShipEffect"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn step_system_entropic_spools_handles_cap_depletion_last_round_unlock_and_target_kill() {
    let (ctx, _session, char_id, ship_id, sys_id) = make_spool_test_ctx();
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let disint_t = ctx.sde.types.get(&47_240).unwrap();

    let target_id = 300_000_090i64;
    let mod_cap = 742_001i64;
    let mod_inactive = 742_002i64;
    let mod_kill = 742_003i64;
    let ammo_one = 742_010i64;

    // Spawn attacker with 30.0 capacitor and fragile target with 10.0 hull HP
    spawn_spool_ball(&ctx, sys_id, ship_id, 47_271, DVec3::ZERO, 1_000.0, 1_000.0, 1_000.0, 30.0).await;
    spawn_spool_ball(&ctx, sys_id, target_id, 500_001, DVec3::new(2_000.0, 0.0, 0.0), 0.0, 0.0, 10.0, 0.0).await;
    sys_handle
        .add_target_lock(
            ship_id,
            TargetLock {
                target_id,
                lock_time_ms: 1_000.0,
                remaining_lock_ms: 0.0,
                locked: true,
                max_range_m: None,
            },
        )
        .await
        .unwrap();

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(mod_cap, make_fighter_item(mod_cap, 47_240, char_id, ship_id, InventoryFlag::HiSlot0, 1));
        snap.items.insert(mod_kill, make_fighter_item(mod_kill, 47_240, char_id, ship_id, InventoryFlag::HiSlot1, 1));
        // Only 1 round of ammo in HiSlot1 (charge_item_id = None on spool so it resolves from slot flag!)
        snap.items.insert(ammo_one, make_fighter_item(ammo_one, 47_245, char_id, ship_id, InventoryFlag::HiSlot1, 1));
        snap.module_active_states.insert(mod_cap);
        snap.module_active_states.insert(mod_kill);
        // mod_inactive is NOT in module_active_states -> should be cleared from entropic_spools immediately
    });

    // 1. mod_cap needs 50.0 cap (ship only has 30.0) -> stops due to insufficient capacitor
    handle_entropic_activation(
        &ctx,
        char_id,
        ship_id,
        sys_id,
        mod_cap,
        target_id,
        disint_t,
        Some(ammo_one),
        DamageVector::new(500.0, 500.0, 0.0, 0.0),
        50.0,
        3.0,
    )
    .unwrap();
    // 2. mod_inactive is not active in DB -> cleared without firing
    handle_entropic_activation(
        &ctx,
        char_id,
        ship_id,
        sys_id,
        mod_inactive,
        target_id,
        disint_t,
        None,
        DamageVector::new(500.0, 500.0, 0.0, 0.0),
        0.0,
        3.0,
    )
    .unwrap();
    // 3. mod_kill needs 10.0 cap, resolves ammo_one by slot flag, consumes last round, and destroys target_id!
    handle_entropic_activation(
        &ctx,
        char_id,
        ship_id,
        sys_id,
        mod_kill,
        target_id,
        disint_t,
        None,
        DamageVector::new(5_000.0, 5_000.0, 0.0, 0.0),
        10.0,
        3.0,
    )
    .unwrap();

    step_system_entropic_spools(&ctx, sys_id, 3.1).await;

    ctx.db.with_snapshot(|snap| {
        assert!(!snap.module_active_states.contains(&mod_cap), "mod_cap should stop on low cap");
        assert!(!snap.module_active_states.contains(&mod_kill), "mod_kill should stop after target destroyed");
        assert!(!snap.items.contains_key(&ammo_one), "single round of ammo should be removed on consumption");
        assert!(!snap.killmails.is_empty(), "target destruction should record killmail");
    });
    assert!(!with_combat_runtime(&ctx, |rt| rt.entropic_spools.contains_key(&(ship_id, mod_inactive))));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn handle_destruction_player_capsule_ejection_zarzakh_podding_and_abyssal_sleeper_wrecks() {
    let (mut ctx, _session, attacker_id, attacker_ship, sys_id) = make_spool_test_ctx();
    let sde_mut = Arc::make_mut(&mut ctx.sde);
    // Sleeper NPC (group 301 -> derive_npc_archetype "Sleeper" -> wreck 30495, 0 bounty)
    sde_mut.types.insert(
        500_002,
        make_fighter_type(500_002, 301, 11, 5_000.0, vec![(481, 200_000.0)]),
    );
    // Drifter NPC (group 1313 -> derive_npc_archetype "Drifter" -> wreck 37531)
    sde_mut.types.insert(
        500_003,
        make_fighter_type(500_003, 1313, 11, 5_000.0, vec![]),
    );

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let now_unix = ctx.now_unix();

    // 1. Player ship WITHOUT Frigate Escape Bay (type 47271):
    //    - Fitted module in ctx.ship_fittings (not yet in snap.items)
    //    - Rig in flag 92 (always destroyed)
    //    - Container item in cargo containing an orphan item (both destroyed when container destroyed)
    //    - Assist log with 1 valid attacker and 1 expired attacker
    let victim_id = 90_000_002i64;
    let victim_ship = 200_000_901i64;
    let fit_mod_id = 760_001i64;
    let rig_id = 760_002i64;
    let cont_id = 760_003i64;
    let orphan_id = 760_004i64;

    let mut fit = ShipFittingEngine::from_sde(victim_ship, 47_271, &ctx.sde);
    fit.modules.insert(
        fit_mod_id,
        FittedModule {
            item_id: fit_mod_id,
            type_id: 47_240,
            flag_id: 27,
            state: ModuleState::Online,
            charge_type_id: None,
            charge_quantity: 0,
            damage_hp: 0.0,
            max_hp: 40.0,
            dynamic_attribute_overrides: HashMap::new(),
        },
    );
    ctx.store_ship_fitting(victim_ship, fit);

    ctx.db.with_mut_snapshot(|snap| {
        let base_ch = snap.characters[&attacker_id].clone();
        let mut vch = base_ch;
        vch.character_id = victim_id;
        vch.active_ship_id = victim_ship;
        vch.solar_system_id = sys_id;
        snap.characters.insert(victim_id, vch);

        snap.items.insert(victim_ship, make_fighter_item(victim_ship, 47_271, victim_id, sys_id as i64, InventoryFlag::None, 1));
        snap.items.insert(rig_id, make_fighter_item(rig_id, 47_240, victim_id, victim_ship, InventoryFlag::RigSlot0, 1));
        snap.items.insert(cont_id, make_fighter_item(cont_id, 587, victim_id, victim_ship, InventoryFlag::SubSystemSlot0, 1));
        snap.items.insert(orphan_id, make_fighter_item(orphan_id, 587, victim_id, cont_id, InventoryFlag::Cargo, 3));
    });

    with_mut_combat_runtime(&ctx, |rt| {
        rt.aggression_assist_log.insert(
            victim_ship,
            vec![
                AssistLogEntry {
                    attacker_char_id: attacker_id,
                    corporation_id: 1_000_009,
                    alliance_id: None,
                    ship_type_id: 47_271,
                    weapon_type_id: 47_240,
                    damage_done: 1_500.0,
                    is_ewar: false,
                    last_action_unix: now_unix,
                },
                AssistLogEntry {
                    attacker_char_id: 90_000_099,
                    corporation_id: 1_000_009,
                    alliance_id: None,
                    ship_type_id: 47_271,
                    weapon_type_id: 47_240,
                    damage_done: 500.0,
                    is_ewar: false,
                    last_action_unix: now_unix.saturating_sub(10_000), // expired assist
                },
            ],
        );
    });

    let v_ball = spawn_spool_ball(&ctx, sys_id, victim_ship, 47_271, DVec3::new(1_000.0, 0.0, 0.0), 0.0, 0.0, 5.0, 0.0).await;
    let (km_id, wreck_id, corpse_id, _) = handle_ship_or_npc_destruction(
        &ctx,
        &sys_handle,
        attacker_id,
        attacker_ship,
        47_271,
        47_240,
        victim_ship,
        &v_ball,
        Some((victim_id, victim_ship)),
        sys_id,
        1_500.0,
        now_unix,
    )
    .await;
    assert!(km_id > 0 && wreck_id > 0 && corpse_id == 0);

    // Victim should now be in a newly spawned capsule (type 670), and orphan_id + rig_id must be removed
    let new_pod_id = ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&rig_id));
        assert!(!snap.items.contains_key(&orphan_id));
        let active_sid = snap.characters[&victim_id].active_ship_id;
        assert_ne!(active_sid, victim_ship);
        assert_eq!(snap.items[&active_sid].type_id, 670);
        active_sid
    });
    with_combat_runtime(&ctx, |rt| {
        let dkm = rt.detailed_killmails.get(&km_id).expect("detailed killmail");
        assert_eq!(dkm.attackers.len(), 1, "expired assist entry should be pruned");
        assert!(dkm.attackers[0].final_blow);
    });

    // 2. Pod destruction with Zarzakh gate lock clears zarzakh_gate_locks
    ctx.db.with_mut_snapshot(|snap| {
        snap.zarzakh_gate_locks.insert(victim_id, (50_000_001, now_unix + 3_600));
    });
    let pod_ball = sys_handle.get_ball_snapshot(new_pod_id).await.unwrap().unwrap().1;
    let (pod_km, pod_wreck, pod_corpse, _) = handle_ship_or_npc_destruction(
        &ctx,
        &sys_handle,
        attacker_id,
        attacker_ship,
        47_271,
        47_240,
        new_pod_id,
        &pod_ball,
        Some((victim_id, new_pod_id)),
        sys_id,
        400.0,
        now_unix,
    )
    .await;
    assert!(pod_km > 0 && pod_wreck == 0 && pod_corpse > 0);
    assert!(!ctx.db.with_snapshot(|s| s.zarzakh_gate_locks.contains_key(&victim_id)));

    // 3. Abyssal system (32_000_001) NPC kill spawns NO wreck
    let abyss_sys = 32_000_001u32;
    let abyss_handle = ctx.systems.get_or_spawn(abyss_sys);
    let abyss_npc_ball = spawn_spool_ball(&ctx, abyss_sys, 1_896_001, 500_001, DVec3::ZERO, 0.0, 0.0, 5.0, 0.0).await;
    let (ab_km, ab_wreck, _, _) = handle_ship_or_npc_destruction(
        &ctx,
        &abyss_handle,
        attacker_id,
        attacker_ship,
        47_271,
        47_240,
        1_896_001,
        &abyss_npc_ball,
        None,
        abyss_sys,
        500.0,
        now_unix,
    )
    .await;
    assert!(ab_km > 0 && ab_wreck == 0);
}

#[test]
fn weapon_bank_linking_filters_by_slot_effect_and_minimum_count() {
    let (mut ctx, _session, char_id, ship_id, _sys_id) = make_spool_test_ctx();
    let sde_mut = Arc::make_mut(&mut ctx.sde);
    // Missile launcher with effect 40
    let mut launcher = make_fighter_type(47_301, 506, 7, 10.0, vec![]);
    launcher.effect_ids = vec![40];
    sde_mut.types.insert(47_301, launcher);
    // Non-weapon high-slot module (no effect 40/42, no attr 604)
    sde_mut.types.insert(47_302, make_fighter_type(47_302, 330, 7, 10.0, vec![]));

    ctx.db.with_mut_snapshot(|snap| {
        // Two launchers in HiSlot0 & HiSlot1 -> grouped
        snap.items.insert(801, make_fighter_item(801, 47_301, char_id, ship_id, InventoryFlag::HiSlot0, 1));
        snap.items.insert(802, make_fighter_item(802, 47_301, char_id, ship_id, InventoryFlag::HiSlot1, 1));
        // Only ONE disintegrator (47_240) in HiSlot2 -> not grouped (len < 2)
        snap.items.insert(803, make_fighter_item(803, 47_240, char_id, ship_id, InventoryFlag::HiSlot2, 1));
        // Two non-weapons in HiSlot3 & HiSlot4 -> not grouped
        snap.items.insert(804, make_fighter_item(804, 47_302, char_id, ship_id, InventoryFlag::HiSlot3, 1));
        snap.items.insert(805, make_fighter_item(805, 47_302, char_id, ship_id, InventoryFlag::HiSlot4, 1));
    });

    let rep = handle_link_all_weapons(&ctx, char_id, ship_id).unwrap();
    let PyRep::Dict(entries) = rep else { panic!("expected dict") };
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].0, PyRep::Integer(801));
    assert_eq!(entries[0].1, PyRep::List(vec![PyRep::Integer(801), PyRep::Integer(802)]));
}
