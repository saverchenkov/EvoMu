//! Unit tests for `structure_weapons_ops` (defense volley, structure target locking, Standup weapon activation, auto-defense).

#![forbid(unsafe_code)]

use super::*;
use crate::services::fleet_social::fleet_social_fleet_ops::fleet_social_fleet_ops_tests::{
    clone_char_with, test_ctx_with_clock,
};
use eve_db::{CorporationRecord, InventoryItemRecord, UpwellStructureRecord};
use eve_destiny::{Ball, CombatState, MovementMode};
use std::collections::HashMap;

#[tokio::test]
async fn structure_defense_volley_target_locking_weapon_activation_and_auto_defense() {
    let ctx = test_ctx_with_clock(1_800_000_000);
    // Use a null-sec system so AoE/Standup weapons are legal
    let null_sys = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| s.security_status < -0.2 && s.solar_system_id < 31_000_000)
        .map(|s| s.solar_system_id)
        .unwrap();

    // Find a Standup Missile Launcher (group 1327 or 1562) and matching ammo charge
    let (launcher_tid, req_charge_grp) = ctx
        .sde
        .types
        .values()
        .find_map(|t| {
            matches!(t.group_id, 1327 | 1562).then(|| {
                let cgrp = t.default_attributes.get(&604).copied().unwrap_or(0.0) as u32;
                (t.type_id, cgrp)
            })
        })
        .unwrap();
    let charge_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.category_id == 8 && t.group_id == req_charge_grp)
        .map(|t| t.type_id)
        .unwrap();

    let gunner_id = 910_901_i64;
    let hostile_id = 910_902_i64;
    let corp_id = 98_900_901_i64;
    let hostile_corp = 98_900_902_i64;
    let st_id = 1_000_000_990_001_i64;
    let mod_id = 910_903_i64;
    let charge_id = 910_904_i64;
    let hostile_ship = 910_905_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let g_ch = clone_char_with(snap, gunner_id, "Gunner", corp_id, null_sys, st_id);
        let h_ch = clone_char_with(snap, hostile_id, "Hostile", hostile_corp, null_sys, hostile_ship);
        snap.characters.insert(gunner_id, g_ch);
        snap.characters.insert(hostile_id, h_ch);
        snap.corporations.insert(
            corp_id,
            { let mut __corp = CorporationRecord::default();
                __corp.corporation_id = corp_id;
                __corp.ceo_id = gunner_id;
                __corp },
        );
        snap.structures.insert(
            st_id,
            UpwellStructureRecord {
                structure_id: st_id,
                type_id: 35_834, // Keepstar
                owner_corp_id: corp_id,
                solar_system_id: null_sys,
                name: "Defense Keepstar".into(),
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 1_000,
                shield_hp: 1_000_000.0,
                armor_hp: 1_000_000.0,
                hull_hp: 1_000_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
        snap.items.insert(
            mod_id,
            InventoryItemRecord {
                item_id: mod_id,
                type_id: launcher_tid,
                owner_id: corp_id,
                location_id: st_id,
                flag: InventoryFlag::HiSlot0,
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
            charge_id,
            InventoryItemRecord {
                item_id: charge_id,
                type_id: charge_tid,
                owner_id: corp_id,
                location_id: st_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 50,
                singleton: false,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        snap.module_online_states.insert(mod_id, true);
    });

    // structure_online_defense_volley_omni returns > 0 when online and 0 when offline
    ctx.db.with_snapshot(|snap| {
        let volley = structure_online_defense_volley_omni(&ctx, snap, st_id);
        assert!(volley > 0.0);
    });

    let sys_handle = ctx.systems.get_or_spawn(null_sys);
    let _ = sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: hostile_ship,
                type_id: 587,
                position: DVec3::new(25_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 2.0,
                mass: 1e6,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState {
                shield_hp: 50_000.0,
                max_shield_hp: 50_000.0,
                armor_hp: 50_000.0,
                max_armor_hp: 50_000.0,
                structure_hp: 50_000.0,
                max_structure_hp: 50_000.0,
                ..CombatState::new()
            },
        )
        .await;

    let mut session = SessionState::new(1);
    session.user_id = Some(gunner_id as u64);
    session.character_id = Some(gunner_id);
    session.corporation_id = Some(corp_id);
    session.solar_system_id = Some(null_sys);
    session.ship_id = Some(st_id);

    // Without active_pilot_by_structure -> NotControllingStructure
    assert_eq!(
        handle_structure_add_target(&ctx, &session, gunner_id, st_id, hostile_ship)
            .await
            .unwrap_err()
            .code,
        "NotControllingStructure"
    );

    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        ext.active_pilot_by_structure.insert(st_id, gunner_id);
        save_ext_state(snap, &ext);
    });

    // Lock hostile_ship on structure -> starts locking with realistic lock_time_ms > 0 (`flag = 1, targetList = []`)
    let lock_rep = handle_structure_add_target(&ctx, &session, gunner_id, st_id, hostile_ship)
        .await
        .expect("lock target on structure");
    assert_eq!(
        lock_rep,
        PyRep::Tuple(vec![PyRep::Integer(1), PyRep::List(Vec::new())])
    );
    let locks_pending = sys_handle.get_target_locks(st_id).await.unwrap();
    assert_eq!(locks_pending.len(), 1);
    assert!(!locks_pending[0].locked);
    assert!(locks_pending[0].lock_time_ms > 0.0);

    // Activate while still locking -> TargetNotLocked
    assert_eq!(
        handle_structure_module_activate(
            &ctx,
            &mut session,
            gunner_id,
            st_id,
            mod_id,
            "launcher",
            Some(hostile_ship),
            false,
        )
        .await
        .unwrap_err()
        .code,
        "TargetNotLocked"
    );

    // Advance simulation past lock completion
    let wait_secs = (locks_pending[0].lock_time_ms / 1_000.0).ceil() as usize + 1;
    for _ in 0..wait_secs {
        sys_handle.step_manual(1.0).await.unwrap();
    }
    let locks_done = sys_handle.get_target_locks(st_id).await.unwrap();
    assert!(locks_done[0].locked, "lock should complete after lock_time_ms elapses");

    // Overload rejected
    assert_eq!(
        handle_structure_module_activate(
            &ctx,
            &mut session,
            gunner_id,
            st_id,
            mod_id,
            "launcher",
            Some(hostile_ship),
            true,
        )
        .await
        .unwrap_err()
        .code,
        "CannotOverloadStructureModule"
    );

    // Activate Standup missile launcher against locked target -> deals damage & consumes 1 charge
    let PyRep::KeyVal(act_kv) = handle_structure_module_activate(
        &ctx,
        &mut session,
        gunner_id,
        st_id,
        mod_id,
        "launcher",
        Some(hostile_ship),
        false,
    )
    .await
    .expect("activate Standup launcher") else {
        panic!("expected keyval");
    };
    assert_eq!(act_kv.get("moduleID").and_then(|v| v.as_i64()), Some(mod_id));
    assert_eq!(act_kv.get("targetID").and_then(|v| v.as_i64()), Some(hostile_ship));
    assert!(act_kv.get("damageDealt").and_then(|v| v.as_f64()).unwrap_or(0.0) > 0.0);
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&charge_id].quantity, 49);
    });

    // Immediate re-activation while cycling -> ModuleAlreadyCycling
    assert_eq!(
        handle_structure_module_activate(
            &ctx,
            &mut session,
            gunner_id,
            st_id,
            mod_id,
            "launcher",
            Some(hostile_ship),
            false,
        )
        .await
        .unwrap_err()
        .code,
        "ModuleAlreadyCycling"
    );

    // Auto-defense trigger records attacker and runs grid defense sweep
    trigger_structure_auto_defense_on_grid(
        &ctx,
        &sys_handle,
        null_sys,
        st_id,
        Some(hostile_id),
        1_800_001_000,
    )
    .await;
    trigger_system_structures_auto_defense(&ctx, null_sys).await;
    ctx.db.with_snapshot(|snap| {
        let def = load_defense_settings(snap, st_id);
        assert!(def.known_attackers.contains(&hostile_id));
    });

    // When access_profiles marks auto_defense_enabled = false, grid defense must NOT fire or lock
    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        ext.standup_module_cycle_until_unix.clear();
        ext.access_profiles.insert(
            st_id,
            crate::services::station_structure::structure_upwell_ops::StructureAccessProfile {
                profile_id: 901,
                owner_corp_id: corp_id,
                name: "No AutoDefense".into(),
                has_defense_config: true,
                auto_defense_enabled: false,
                ..Default::default()
            },
        );
        save_ext_state(snap, &ext);
        let mut def = load_defense_settings(snap, st_id);
        def.auto_defense_enabled = true;
        def.auto_target_attackers = true;
        save_defense_settings(snap, &def);
    });
    let _ = sys_handle.clear_target_locks(st_id).await;
    let charge_before = ctx.db.with_snapshot(|s| s.items[&charge_id].quantity);
    trigger_structure_auto_defense_on_grid(
        &ctx,
        &sys_handle,
        null_sys,
        st_id,
        Some(hostile_id),
        1_800_002_000,
    )
    .await;
    assert_eq!(ctx.db.with_snapshot(|s| s.items[&charge_id].quantity), charge_before);
    assert!(sys_handle.get_target_locks(st_id).await.unwrap().is_empty());
}
