//! Semantic unit tests split from `binding_deep_extra_unit_tests`.

#![forbid(unsafe_code)]

use super::binding_deep_extra_unit_tests::*;

use super::binding::effect_id_for_module_sde;
use super::ServerContext;
use crate::system_actor::ActiveModuleCycle;
use eve_db::{
    InventoryFlag, InventoryItemRecord, TrainedSkillRecord,
    VirtualServerClock,
};
use eve_dogma::{DogmaItemState, ProjectedEwarEffect};
use eve_machonet::SessionState;
use std::collections::HashMap;
use std::sync::Arc;

#[test]
fn module_deactivation_and_target_lock_hooks_cloak_cooldown_and_auto_reload() {
    let ctx = test_ctx();
    let now = 1_800_000_000u64;
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(now)));

    // 1) effect_id_for_module_sde resolution
    let cloak_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.group_id == 330)
        .map(|t| t.type_id)
        .unwrap_or(11_370);
    assert!(effect_id_for_module_sde(&ctx.sde, cloak_tid, Some("cloaking")).is_some());
    assert!(effect_id_for_module_sde(&ctx.sde, cloak_tid, None).is_some());

    // 2) Wire hooks and invoke module deactivation callback for a Cloak module with auto-reload blob
    ctx.wire_module_deactivation_hook();
    let char1 = 90_000_001i64;
    let char2 = 90_000_002i64;
    let sys_id = 30_000_142u32;
    let ship1 = 100_000_001i64;
    let ship2 = 100_000_002i64;
    let mod_id = 777_888_001i64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c1) = snap.characters.get_mut(&char1) {
            c1.station_id = None;
            c1.solar_system_id = sys_id;
            c1.active_ship_id = ship1;
        }
        if let Some(c2) = snap.characters.get_mut(&char2) {
            c2.station_id = None;
            c2.solar_system_id = sys_id;
            c2.active_ship_id = ship2;
        }
        snap.items.insert(
            ship2,
            InventoryItemRecord {
                item_id: ship2,
                type_id: 606,
                owner_id: char2,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.module_active_states.insert(mod_id);
        snap.module_active_charges.insert(mod_id, 200);
        snap.service_state_blobs.insert(
            format!("auto_reloaded_event:{mod_id}"),
            serde_json::to_string(&(vec![mod_id], 200u32, 5_000.0f64)).unwrap(),
        );
    });

    let deact_hook = ctx
        .systems
        .module_deactivation_hook
        .read()
        .unwrap()
        .as_ref()
        .cloned()
        .expect("module_deactivation_hook wired");
    deact_hook(
        char1,
        &ActiveModuleCycle {
            module_id: mod_id,
            module_type_id: cloak_tid,
            ship_id: ship1,
            effect_name: "cloaking".into(),
            duration_s: 5.0,
            reactivation_delay_s: 15.0,
            ..Default::default()
        },
    );

    // Verify cooldown recorded, active state cleared, and OnGodmaShipEffect + OnSpecialFX + OnChargeBeingLoadedToModule queued
    assert!(ctx
        .module_reactivation_cooldowns
        .read()
        .unwrap()
        .contains_key(&(ship1, mod_id, cloak_tid)));
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.module_active_states.contains(&mod_id));
        assert!(!snap.module_active_charges.contains_key(&mod_id));
        assert!(!snap
            .service_state_blobs
            .contains_key(&format!("auto_reloaded_event:{mod_id}")));
    });
    let events: Vec<String> = ctx.pending_notifications.read().unwrap()[&char1]
        .iter()
        .map(|n| n.event.clone())
        .collect();
    assert!(events.contains(&"OnGodmaShipEffect".to_string()));
    assert!(events.contains(&"OnSpecialFX".to_string()));
    assert!(events.contains(&"OnChargeBeingLoadedToModule".to_string()));

    // 3) Invoke target-lock completed hook: queues OnMultiEvent ("add") to char1 and ("otheradd") to char2
    ctx.pending_notifications.write().unwrap().clear();
    let lock_hook = ctx
        .systems
        .target_lock_completed_hook
        .read()
        .unwrap()
        .as_ref()
        .cloned()
        .expect("target_lock_completed_hook wired");
    lock_hook(char1, ship2);
    let q = ctx.pending_notifications.read().unwrap();
    assert!(q[&char1].iter().any(|n| n.event == "OnMultiEvent"));
    assert!(q[&char2].iter().any(|n| n.event == "OnMultiEvent"));
}

#[test]
fn alpha_omega_skill_prerequisites_and_projected_ewar_signature_radius_math() {
    let mut ctx = test_ctx();
    let now = 1_800_000_000u64;
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(now)));

    let char_id = 90_000_001i64;
    let custom_ship_tid = 945_801u32;
    let custom_mod_tid = 945_802u32;
    let upwell_req_skill = 12_180u32; // in 12180..=12199 (satisfied by alt skill 60377)

    let mut base_ship = ctx.sde.types[&606].clone();
    base_ship.type_id = custom_ship_tid;
    base_ship.category_id = 6;
    base_ship.default_attributes = HashMap::from([(182, upwell_req_skill as f64), (277, 5.0)]);

    let mut base_mod = ctx.sde.types[&606].clone();
    base_mod.type_id = custom_mod_tid;
    base_mod.category_id = 7;
    base_mod.default_attributes = HashMap::from([(182, 3_402.0), (277, 5.0)]);

    let mut sde_mut = (*ctx.sde).clone();
    sde_mut.types.insert(custom_ship_tid, base_ship);
    sde_mut.types.insert(custom_mod_tid, base_mod);
    ctx.sde = Arc::new(sde_mut);

    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(char_id).or_default().clear();
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.clone_grade = 2; // Omega
        }

        // 1) Missing module skill -> SkillPrerequisitesNotMet; missing ship skill -> ShipHasSkillPrerequisites
        assert_eq!(
            ctx.check_type_skill_prerequisites_in_snapshot(snap, char_id, custom_mod_tid)
                .unwrap_err()
                .code,
            "SkillPrerequisitesNotMet"
        );
        assert_eq!(
            ctx.check_type_skill_prerequisites_in_snapshot(snap, char_id, custom_ship_tid)
                .unwrap_err()
                .code,
            "ShipHasSkillPrerequisites"
        );

        // 2) Satisfying `12180` via alternative skill `60377` at level 5 passes for Omega clone!
        snap.skills.entry(char_id).or_default().insert(
            60_377,
            TrainedSkillRecord {
                type_id: 60_377,
                level: 5,
                skill_points: 256_000.0,
            },
        );
        assert!(ctx
            .check_type_skill_prerequisites_in_snapshot(snap, char_id, custom_ship_tid)
            .is_ok());

        // 3) Demote character to Alpha clone (clone_grade = 1): level 5 requirement > Alpha cap -> TrialAccountRestriction!
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.clone_grade = 1;
        }
        assert_eq!(
            ctx.check_type_skill_prerequisites_in_snapshot(snap, char_id, custom_ship_tid)
                .unwrap_err()
                .code,
            "TrialAccountRestriction"
        );
    });

    // 4) Projected EWAR upsert (insert + update in place) & remove_projected_ewar_by_module ownership check
    let mut tp_mod = DogmaItemState::from_sde(700_001, 379, &ctx.sde);
    tp_mod.base_attributes.insert(554, 25.0);
    tp_mod
        .base_attributes
        .insert(eve_sde::attr_ids::MAX_RANGE, 30_000.0);
    let eff1 = ProjectedEwarEffect {
        source_module_id: 700_001,
        source_module_type_id: 379,
        charge_type_id: None,
        distance: 5_000.0,
        overloaded: false,
        effective_module: tp_mod.clone(),
    };
    ctx.upsert_projected_ewar_on_ship(800_001, 900_001, eff1);
    let sig1 = ctx.effective_target_signature_radius(900_001, 100.0);
    assert!((sig1 - 125.0).abs() < 1e-6);

    // Update existing source_module_id in place with 50% bonus
    tp_mod.base_attributes.insert(554, 50.0);
    let eff2 = ProjectedEwarEffect {
        source_module_id: 700_001,
        source_module_type_id: 379,
        charge_type_id: None,
        distance: 5_000.0,
        overloaded: false,
        effective_module: tp_mod,
    };
    ctx.upsert_projected_ewar_on_ship(800_001, 900_001, eff2);
    assert_eq!(ctx.projected_ewar.read().unwrap()[&900_001].len(), 1);
    let sig2 = ctx.effective_target_signature_radius(900_001, 100.0);
    assert!((sig2 - 150.0).abs() < 1e-6);

    // Wrong source_ship_id does not remove the effect
    assert!(ctx
        .remove_projected_ewar_by_module(800_999, 700_001)
        .is_empty());
    assert_eq!(
        ctx.remove_projected_ewar_by_module(800_001, 700_001),
        vec![900_001]
    );
    assert_eq!(ctx.effective_target_signature_radius(900_001, 100.0), 100.0);
}

#[test]
fn binding_auth_and_session_transitions_enforce_service_rules() {
    let ctx = ServerContext::new();
    let mut sess = SessionState::new(42);
    sess.user_id = Some(1);
    sess.character_id = Some(90_000_001);
    sess.corporation_id = Some(98_000_001);
    sess.solar_system_id = Some(30_000_142);
    sess.station_id = None;
    sess.structure_id = None;

    assert!(ctx.bind_object(&sess, "tourneyMgr", 0).is_err());
    assert!(ctx.bind_object(&sess, "tourneyMgr", 1).is_ok());

    assert!(ctx.bind_object(&sess, "planetOrbitalRegistryBroker", 99_999_999).is_err());
    let pi_sys_bind = ctx
        .bind_object(&sess, "planetOrbitalRegistryBroker", 30_000_142)
        .expect("valid system");

    // Undocked character cannot bind officeManager
    assert!(ctx.bind_object(&sess, "officeManager", 98_000_001).is_err());
    sess.station_id = Some(60_003_760);
    let office_bind = ctx
        .bind_object(&sess, "officeManager", 98_000_001)
        .expect("docked office bind");

    // Seeded vs unseeded warStatisticMgr
    assert!(ctx.bind_object(&sess, "warStatisticMgr", 888_888).is_err());
    ctx.db.with_mut_snapshot(|s| {
        s.wars.insert(
            700_000_002,
            eve_db::WarRecord {
                war_id: 700_000_002,
                aggressor_corp_id: 98_000_001,
                defender_corp_id: 98_000_002,
                mutual: false,
                started_unix: 1_800_000_000,
                active: true,
            },
        );
    });
    assert!(ctx.bind_object(&sess, "warStatisticMgr", 700_000_002).is_ok());

    assert!(ctx.bind_object(&sess, "entity", 0).is_err());
    let entity_bind = ctx.bind_object(&sess, "entity", 30_000_142).expect("entity bind");

    // Jumping to another solar system evicts system-scoped entity binding while valid SDE system stays bound.
    sess.solar_system_id = Some(30_000_144);
    ctx.invalidate_bound_objects_for_session_transition(90_000_001, 30_000_144, None, "stargate_jump");
    assert!(ctx.verify_bound_object(&sess, entity_bind).is_err());
    assert!(ctx.verify_bound_object(&sess, pi_sys_bind).is_ok());

    let entity_bind_2 = ctx.bind_object(&sess, "entity", 30_000_144).expect("entity bind 2");

    // Corporation change evicts officeManager
    ctx.invalidate_bound_objects_for_session_transition(90_000_001, 30_000_144, Some(60_003_760), "corp_change");
    assert!(ctx.verify_bound_object(&sess, office_bind).is_err());

    // Ship loss to clone station evicts space entity binding
    ctx.invalidate_bound_objects_for_session_transition(90_000_001, 30_000_142, Some(60_003_760), "ship_loss");
    assert!(ctx.verify_bound_object(&sess, entity_bind_2).is_err());
}
