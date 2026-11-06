//! Deep branch-coverage and mutation-killing unit tests for `router::binding`:
//! moniker authorization across `dogmaIM`, `ship`, `invbroker`, `station`, `corpStationMgr`,
//! `repairSvc`, `reprocessingSvc`, `insuranceSvc`, `skillMgr2`, `charMgr`, `jumpCloneSvc`,
//! `beyonce`, `scanMgr`, `crimewatch`, `i2`, and positive-ID services; session transition
//! invalidation & `OnMachoObjectDisconnect`; module deactivation & target-lock hooks; and
//! Alpha/Omega skill prerequisites & target painter signature bloom math.

#![forbid(unsafe_code)]

use super::binding::effect_id_for_module_sde;
use super::ServerContext;
use crate::system_actor::ActiveModuleCycle;
use eve_db::{
    InventoryFlag, InventoryItemRecord, TrainedSkillRecord, UpwellStructureRecord,
    VirtualServerClock, WorldRepository,
};
use eve_dogma::{DogmaItemState, ProjectedEwarEffect};
use eve_machonet::service_roles::ROLE_PLAYER;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn test_ctx() -> ServerContext {
    if let Some(sde) = SHARED_SDE.get() {
        ServerContext::with_sde_and_repository(
            Arc::clone(sde),
            Arc::new(WorldRepository::with_demo_seed()),
        )
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
    } else {
        let ctx = ServerContext::new();
        let _ = SHARED_SDE.set(Arc::clone(&ctx.sde));
        ctx
    }
}

#[test]
fn moniker_bind_and_verify_authorization_matrix_across_all_services() {
    let ctx = test_ctx();
    let char_id = 90_000_001i64;
    let other_char = 90_000_002i64;
    let sys_id = 30_000_142u32;
    let other_sys = 30_000_144u32;
    let st_id = 60_003_760i64;
    let other_st_in_jita = ctx
        .sde
        .stations
        .values()
        .find(|s| s.solar_system_id == sys_id && s.station_id != st_id)
        .map(|s| s.station_id)
        .unwrap_or(60_003_760);
    let ship_id = 100_000_001i64;
    let struct_id = 1_000_000_777_001i64;
    let item_in_station = 777_101i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            struct_id,
            UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 35_832,
                owner_corp_id: 1_000_009,
                solar_system_id: sys_id,
                name: "Jita Astrahus".into(),
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                shield_hp: 10_000.0,
                armor_hp: 10_000.0,
                hull_hp: 10_000.0,
                ..Default::default()
            },
        );
        snap.items.insert(
            item_in_station,
            InventoryItemRecord {
                item_id: item_in_station,
                type_id: 3293,
                owner_id: char_id,
                location_id: st_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });

    let mut sess = SessionState {
        session_id: 501,
        user_id: Some(1),
        character_id: Some(char_id),
        solar_system_id: Some(sys_id),
        station_id: Some(st_id),
        structure_id: None,
        ship_id: Some(ship_id),
        role: ROLE_PLAYER,
        ..Default::default()
    };

    // 1) dogmaIM & ship: allowed for ship_id, station_id, structure_id, solar_system_id; denied for unrelated
    for svc in ["dogmaIM", "ship"] {
        assert!(ctx.bind_object(&sess, svc, ship_id).is_ok());
        assert!(ctx.bind_object(&sess, svc, st_id).is_ok());
        assert!(ctx.bind_object(&sess, svc, sys_id as i64).is_ok());
        assert_eq!(
            ctx.bind_object(&sess, svc, 999_888_777).unwrap_err().code,
            "BindAuthorizationDenied"
        );
    }

    // 2) station, corpStationMgr, repairSvc, reprocessingSvc, insuranceSvc:
    //    allowed for active station_id or structure_id; denied for solar_system_id or other station
    let mut station_bids = Vec::new();
    for svc in [
        "station",
        "corpStationMgr",
        "repairSvc",
        "reprocessingSvc",
        "insuranceSvc",
    ] {
        let bid = ctx.bind_object(&sess, svc, st_id).expect(svc);
        assert!(ctx.verify_bound_object(&sess, bid).is_ok());
        station_bids.push((svc, bid));
        assert_eq!(
            ctx.bind_object(&sess, svc, sys_id as i64).unwrap_err().code,
            "BindAuthorizationDenied"
        );
    }
    // When undocked into space, verify_bound_object rejects all station-scoped monikers
    sess.station_id = None;
    for (svc, bid) in &station_bids {
        assert_eq!(
            ctx.verify_bound_object(&sess, *bid).unwrap_err().code,
            "BoundObjectOwnershipViolation",
            "{svc} moniker must fail verification when undocked"
        );
    }
    // When docked in Upwell structure `struct_id`, all 5 services bind & verify against `struct_id`
    sess.structure_id = Some(struct_id);
    for svc in [
        "station",
        "corpStationMgr",
        "repairSvc",
        "reprocessingSvc",
        "insuranceSvc",
    ] {
        let bid = ctx.bind_object(&sess, svc, struct_id).expect(svc);
        assert!(ctx.verify_bound_object(&sess, bid).is_ok());
    }
    sess.structure_id = None;
    sess.station_id = Some(st_id);

    // 3) invbroker:
    //    - While docked: station_id, ship_id, other station in same system, and structure in same system succeed;
    //      binding to solar_system_id while docked is DENIED!
    assert!(ctx.bind_object(&sess, "invbroker", st_id).is_ok());
    assert!(ctx.bind_object(&sess, "invbroker", ship_id).is_ok());
    assert!(ctx.bind_object(&sess, "invbroker", other_st_in_jita).is_ok());
    assert!(ctx.bind_object(&sess, "invbroker", struct_id).is_ok());
    assert_eq!(
        ctx.bind_object(&sess, "invbroker", sys_id as i64)
            .unwrap_err()
            .code,
        "BindAuthorizationDenied", // rpc-lint: allow error code assertion
        "invbroker cannot bind to solar_system_id while docked"
    );
    //    - Verified sub-object on `item_in_station` passes verify_bound_object while at `st_id`,
    //      and fails when pilot moves to `other_sys` undocked!
    let b_sub_item = ctx
        .bind_verified_sub_object(&sess, "invbroker", item_in_station)
        .unwrap();
    assert!(ctx.verify_bound_object(&sess, b_sub_item).is_ok());

    //    - While undocked in `sys_id`: binding `invbroker` to `sys_id` succeeds!
    sess.station_id = None;
    let b_inv_sys = ctx
        .bind_object(&sess, "invbroker", sys_id as i64)
        .expect("invbroker bind to solar_system_id while undocked");
    assert!(ctx.verify_bound_object(&sess, b_inv_sys).is_ok());
    sess.solar_system_id = Some(other_sys);
    assert_eq!(
        ctx.verify_bound_object(&sess, b_inv_sys).unwrap_err().code,
        "BoundObjectOwnershipViolation" // rpc-lint: allow error code assertion
    );
    assert_eq!(
        ctx.verify_bound_object(&sess, b_sub_item).unwrap_err().code,
        "BoundObjectOwnershipViolation" // rpc-lint: allow error code assertion
    );
    sess.solar_system_id = Some(sys_id);
    sess.station_id = Some(st_id);

    // 4) skillMgr2, charMgr, jumpCloneSvc: allowed for char_id, solar_system_id, station_id, structure_id, and 0;
    //    denied for `other_char`!
    sess.structure_id = Some(struct_id);
    for svc in ["skillMgr2", "charMgr", "jumpCloneSvc"] {
        for valid_id in [char_id, sys_id as i64, st_id, struct_id, 0] {
            assert!(
                ctx.bind_object(&sess, svc, valid_id).is_ok(),
                "{svc} should allow {valid_id}"
            );
        }
        assert_eq!(
            ctx.bind_object(&sess, svc, other_char).unwrap_err().code,
            "BindAuthorizationDenied" // rpc-lint: allow error code assertion
        );
    }

    // 5) crimewatch: allowed for char_id, solar_system_id, station_id, and 0;
    //    denied for `struct_id` and `other_char`!
    for valid_id in [char_id, sys_id as i64, st_id, 0] {
        assert!(ctx.bind_object(&sess, "crimewatch", valid_id).is_ok());
    }
    assert_eq!(
        ctx.bind_object(&sess, "crimewatch", struct_id)
            .unwrap_err()
            .code,
        "BindAuthorizationDenied" // rpc-lint: allow error code assertion
    );
    assert_eq!(
        ctx.bind_object(&sess, "crimewatch", other_char)
            .unwrap_err()
            .code,
        "BindAuthorizationDenied" // rpc-lint: allow error code assertion
    );

    // 6) beyonce & scanMgr: only solar_system_id allowed; verify_bound_object fails when system changes
    for svc in ["beyonce", "scanMgr"] {
        let bid = ctx.bind_object(&sess, svc, sys_id as i64).unwrap();
        assert!(ctx.verify_bound_object(&sess, bid).is_ok());
        assert_eq!(
            ctx.bind_object(&sess, svc, st_id).unwrap_err().code,
            "BindAuthorizationDenied" // rpc-lint: allow error code assertion
        );
        sess.solar_system_id = Some(other_sys);
        assert_eq!(
            ctx.verify_bound_object(&sess, bid).unwrap_err().code,
            "BoundObjectOwnershipViolation" // rpc-lint: allow error code assertion
        );
        sess.solar_system_id = Some(sys_id);
    }

    // 7) Positive-ID services: object_id > 0 allowed, object_id <= 0 denied
    for svc in [
        "agentMgr",
        "keeper",
        "planetMgr",
        "posMgr",
        "fleetObjectHandler",
        "corpRegistry",
        "corpmgr",
        "allianceRegistry",
        "warRegistry",
        "warStatisticMgr",
        "trademgr",
        "voucher",
        "structure",
        "devIndexManager",
        "contractProxy",
    ] {
        assert!(ctx.bind_object(&sess, svc, 1).is_ok(), "{svc} > 0");
        assert_eq!(
            ctx.bind_object(&sess, svc, 0).unwrap_err().code,
            "BindAuthorizationDenied", // rpc-lint: allow error code assertion
            "{svc} == 0"
        );
        assert_eq!(
            ctx.bind_object(&sess, svc, -5).unwrap_err().code,
            "BindAuthorizationDenied", // rpc-lint: allow error code assertion
            "{svc} < 0"
        );
    }
}

#[test]
fn session_transition_invalidation_and_macho_object_disconnect_notifications() {
    let ctx = test_ctx();
    let char_id = 90_000_001i64;
    let sys_id = 30_000_142u32;
    let st_id = 60_003_760i64;

    let sess = SessionState {
        session_id: 601,
        user_id: Some(1),
        character_id: Some(char_id),
        solar_system_id: Some(sys_id),
        station_id: Some(st_id),
        ship_id: Some(100_000_001),
        role: ROLE_PLAYER,
        ..Default::default()
    };
    ctx.db
        .acquire_online_lock(1, 601, Some(char_id), true, 1_800_000_000)
        .unwrap();

    let b_bey = ctx.bind_object(&sess, "beyonce", sys_id as i64).unwrap();
    let b_scan = ctx.bind_object(&sess, "scanMgr", sys_id as i64).unwrap();
    let b_keep_sys = ctx.bind_object(&sess, "keeper", sys_id as i64).unwrap();
    let b_pos_sys = ctx.bind_object(&sess, "posMgr", sys_id as i64).unwrap();
    let b_pos_non_sys = ctx.bind_object(&sess, "posMgr", 500).unwrap();
    let b_dev_sys = ctx
        .bind_object(&sess, "devIndexManager", sys_id as i64)
        .unwrap();
    let b_repair = ctx.bind_object(&sess, "repairSvc", st_id).unwrap();

    // 1) Exempt reasons never invalidate any bound objects
    for exempt in [
        "set_account_key",
        "corp_change",
        "fleet_change",
        "board_or_eject",
        "ship_loss",
        "clone_swap",
        "login",
    ] {
        ctx.invalidate_bound_objects_for_session_transition(char_id, sys_id, Some(st_id), exempt);
    }
    for bid in [
        b_bey,
        b_scan,
        b_keep_sys,
        b_pos_sys,
        b_pos_non_sys,
        b_dev_sys,
        b_repair,
    ] {
        assert!(ctx.bound_objects.read().unwrap().contains_key(&bid));
    }

    // 2) "dock" invalidates all solar-system-scoped bindings (including keeper/posMgr/devIndexManager
    //    with solar-system object_id in 30_000_000..=39_999_999), while preserving b_pos_non_sys (500)
    //    and b_repair (matching active_dock_id = Some(st_id))!
    ctx.pending_notifications.write().unwrap().clear();
    ctx.invalidate_bound_objects_for_session_transition(char_id, sys_id, Some(st_id), "dock");
    for sys_bid in [b_bey, b_scan, b_keep_sys, b_pos_sys, b_dev_sys] {
        assert!(!ctx.bound_objects.read().unwrap().contains_key(&sys_bid));
    }
    assert!(ctx.bound_objects.read().unwrap().contains_key(&b_pos_non_sys));
    assert!(ctx.bound_objects.read().unwrap().contains_key(&b_repair));

    let disconnect_notifs: Vec<_> = ctx.pending_notifications.read().unwrap()[&char_id]
        .iter()
        .filter(|n| n.event == "OnMachoObjectDisconnect")
        .cloned()
        .collect();
    assert_eq!(disconnect_notifs.len(), 5);
    assert_eq!(
        disconnect_notifs[0].args[1],
        PyRep::Integer(1_000_000_000_601)
    );

    // 3) release_bound_object rejects wrong user/session and emits OnMachoObjectDisconnect on success
    let mut wrong_user = sess.clone();
    wrong_user.user_id = Some(999);
    assert_eq!(
        ctx.release_bound_object(&wrong_user, b_repair)
            .unwrap_err()
            .code,
        "BoundObjectOwnershipViolation"
    );
    ctx.release_bound_object(&sess, b_repair)
        .expect("release b_repair");
    assert!(!ctx.bound_objects.read().unwrap().contains_key(&b_repair));
}

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
