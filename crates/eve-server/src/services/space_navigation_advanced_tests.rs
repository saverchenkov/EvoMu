//! Unit tests for `space_navigation_advanced` (immobilizing modules, decloak delay, supercap NPC docking, Zarzakh toll & gravity hazard).

#![forbid(unsafe_code)]

use super::*;
use crate::services::fleet_social::fleet_social_fleet_ops::fleet_social_fleet_ops_tests::{
    clone_char_with, test_ctx_with_clock,
};
use eve_db::{InventoryFlag, InventoryItemRecord, IskCents};
use eve_destiny::{Ball, Ballpark, MovementMode};
use glam::DVec3;

#[test]
fn attr_lookup_and_supercap_npc_station_docking_validation() {
    let ctx = test_ctx_with_clock(1_800_000_000);
    assert!(find_attr_id_by_name(&ctx.sde, "maxVelocity").is_some());
    assert_eq!(find_attr_id_by_name(&ctx.sde, "nonExistentAttrName_XYZ"), None);
    let _ = max_velocity_cloak_attr_id(&ctx.sde);

    // Subcapitals and regular capitals can dock at NPC stations
    assert!(validate_npc_station_docking(25, true).is_ok());
    assert!(validate_npc_station_docking(547, true).is_ok());

    // Titans (30) and Supercarriers (659) are refused at NPC stations, allowed at Upwell structures
    assert_eq!(
        validate_npc_station_docking(30, true).unwrap_err().code,
        "DockingRequestDenied"
    );
    assert_eq!(
        validate_npc_station_docking(659, true).unwrap_err().code,
        "DockingRequestDenied"
    );
    assert!(validate_npc_station_docking(30, false).is_ok());
    assert!(validate_npc_station_docking(659, false).is_ok());
}

#[test]
fn immobilizing_module_active_blocks_dock_jump_warp_and_tether() {
    let ctx = test_ctx_with_clock(1_800_000_000);
    let ship_id = 410_001_i64;
    let mod_id = 410_002_i64;

    // Find a type in SDE that has siegeModeWarpStatus, disallowDocking, or disallowTethering
    let ids = nav_attr_ids(&ctx.sde);
    let siege_type_id = ctx
        .sde
        .types
        .values()
        .find(|t| {
            ids.siege_warp_status
                .and_then(|aid| t.default_attributes.get(&aid).copied())
                .unwrap_or(0.0)
                != 0.0
        })
        .map(|t| t.type_id)
        .unwrap_or(33_400);

    // No modules fitted -> all actions allowed
    assert!(check_immobilizing_module_active(&ctx, ship_id, "dock").is_ok());
    assert!(check_immobilizing_module_active(&ctx, ship_id, "jump").is_ok());
    assert!(check_immobilizing_module_active(&ctx, ship_id, "warp").is_ok());
    assert!(check_immobilizing_module_active(&ctx, ship_id, "tether").is_ok());

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            mod_id,
            InventoryItemRecord {
                item_id: mod_id,
                type_id: siege_type_id,
                owner_id: 90_000_001,
                location_id: ship_id,
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
    });

    // Fitted but inactive -> allowed
    assert!(check_immobilizing_module_active(&ctx, ship_id, "dock").is_ok());

    // Activate module -> blocks dock, jump, warp, tether, and fallback actions
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_active_states.insert(mod_id);
        snap.module_active_charges.insert(mod_id, 999);
    });
    assert_eq!(
        check_immobilizing_module_active(&ctx, ship_id, "dock").unwrap_err().code,
        "CantDockWhileModuleActive"
    );
    assert_eq!(
        check_immobilizing_module_active(&ctx, ship_id, "jump").unwrap_err().code,
        "CantJumpWhileModuleActive"
    );
    assert_eq!(
        check_immobilizing_module_active(&ctx, ship_id, "warp").unwrap_err().code,
        "WarpDisrupted"
    );
    assert_eq!(
        check_immobilizing_module_active(&ctx, ship_id, "tether").unwrap_err().code,
        "DeniedTethered"
    );
    assert_eq!(
        check_immobilizing_module_active(&ctx, ship_id, "gate").unwrap_err().code,
        "CantJumpWhileModuleActive"
    );

    // Deactivate on leave space clears active state and active charges
    deactivate_ship_modules_on_leave_space(&ctx, 90_000_001, ship_id);
    assert!(check_immobilizing_module_active(&ctx, ship_id, "dock").is_ok());
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.module_active_states.contains(&mod_id));
        assert!(!snap.module_active_charges.contains_key(&mod_id));
    });
}

#[test]
fn decloak_targeting_delay_skill_and_hull_role_bonuses() {
    let ctx = test_ctx_with_clock(1_800_000_000);
    // Standard frigate (Rifter 587): 30s at lvl 0, 21s at lvl 3, 15s at lvl 5 (and clamped at lvl 7)
    let d0 = compute_decloak_targeting_delay_s(&ctx.sde, 587, 0, 30.0);
    let d3 = compute_decloak_targeting_delay_s(&ctx.sde, 587, 3, 30.0);
    let d5 = compute_decloak_targeting_delay_s(&ctx.sde, 587, 5, 30.0);
    let d7 = compute_decloak_targeting_delay_s(&ctx.sde, 587, 7, 30.0);
    assert!((d0 - 30.0).abs() < 1e-6);
    assert!((d3 - 21.0).abs() < 1e-6);
    assert!((d5 - 15.0).abs() < 1e-6);
    assert!((d7 - 15.0).abs() < 1e-6);

    // Stealth Bomber (Manticore 12032) has 0ms targeting delay override
    let sb_delay = compute_decloak_targeting_delay_s(&ctx.sde, 12_032, 0, 30.0);
    assert!((sb_delay - 0.0).abs() < 1e-6);

    // Covert Ops frigate (Anathema 11188) with Covert Ops Cloaking Device II (11578) -> 0.0s
    let covops_delay = compute_decloak_targeting_delay_for_module(
        &ctx,
        90_000_001,
        100_000_001,
        11_188,
        None,
        11_578,
        4,
    );
    assert!((covops_delay - 0.0).abs() < 1e-6);

    // Standard ship (587) with Prototype Cloaking Device I (11370) -> positive delay
    let proto_delay = compute_decloak_targeting_delay_for_module(
        &ctx,
        90_000_001,
        999_999_999,
        587,
        None,
        11_370,
        5,
    );
    assert!(proto_delay > 0.0);
}

#[test]
fn zarzakh_gate_toll_pirate_exemption_confirmation_and_wallet_deduction() {
    let ctx = test_ctx_with_clock(1_800_000_000);
    let char_id = 910_101_i64;
    let ship_id = 910_102_i64;
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = clone_char_with(snap, char_id, "Zarzakh Traveler", 98_000_001, 30_100_000, ship_id);
        ch.balance = IskCents::from_isk(5_000);
        snap.characters.insert(char_id, ch);
    });
    let small_ball = Ball {
        id: ship_id,
        type_id: 587,
        position: DVec3::ZERO,
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 2.0,
        mass: 1_000_000.0,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };

    //Declining toll returns ZarzakhGateTollDeclined
    let mut kw_decline = HashMap::new();
    kw_decline.insert("confirmToll".to_string(), PyRep::Bool(false));
    assert_eq!(
        process_zarzakh_gate_toll(&ctx, char_id, ship_id, 50_001, 50_002, &small_ball, &[], &kw_decline)
            .unwrap_err()
            .code,
        "ZarzakhGateTollDeclined"
    );

    // Prompt required stores quote and returns AskConfirmZarzakhGateToll
    let mut kw_prompt = HashMap::new();
    kw_prompt.insert("requireConfirm".to_string(), PyRep::Bool(true));
    assert_eq!(
        process_zarzakh_gate_toll(&ctx, char_id, ship_id, 50_001, 50_002, &small_ball, &[], &kw_prompt)
            .unwrap_err()
            .code,
        "AskConfirmZarzakhGateToll"
    );

    // Second call uses stored quote, fails on insufficient balance (5,000 < 10,000 ISK floor)
    assert_eq!(
        process_zarzakh_gate_toll(&ctx, char_id, ship_id, 50_001, 50_002, &small_ball, &[], &kw_prompt)
            .unwrap_err()
            .code,
        "InsufficientFundsForZarzakhGateToll"
    );

    // Fund character with 100,000 ISK and confirm toll -> deducts 10,000 ISK floor
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().balance = IskCents::from_isk(100_000);
    });
    assert!(
        process_zarzakh_gate_toll(&ctx, char_id, ship_id, 50_001, 50_002, &small_ball, &[], &HashMap::new())
            .is_ok()
    );
    ctx.db.with_snapshot(|snap| {
        assert_eq!(
            snap.characters[&char_id].balance,
            IskCents::from_isk(90_000)
        );
        assert!(snap
            .wallet_journal
            .iter()
            .any(|j| j.character_id == char_id && j.entry_type == "zarzakh_gate_toll"));
    });

    // Pirate insurgency enlisted pilot is exempt from Zarzakh gate toll
    ctx.db.with_mut_snapshot(|snap| {
        snap.fw_runtime.direct_enlisted_chars.insert(char_id, 500_010);
    });
    assert!(
        process_zarzakh_gate_toll(&ctx, char_id, ship_id, 50_001, 50_002, &small_ball, &[], &kw_prompt)
            .is_ok()
    );
    ctx.db.with_snapshot(|snap| {
        assert_eq!(
            snap.characters[&char_id].balance,
            IskCents::from_isk(90_000)
        );
    });
}

#[test]
fn zarzakh_gravity_hazard_respects_safe_zones_and_damages_exposed_ships() {
    let ctx = test_ctx_with_clock(1_800_000_000);
    let mut bp = Ballpark::new(30_100_000);

    // Anchor a safe zone stargate (group 10, type 16) at origin
    let gate_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.group_id == 10)
        .map(|t| t.type_id)
        .unwrap_or(16);
    bp.add_ball(Ball {
        id: 70_001,
        type_id: gate_tid,
        position: DVec3::ZERO,
        velocity: DVec3::ZERO,
        max_velocity: 0.0,
        inertia: 1.0,
        mass: 1e9,
        radius: 5000.0,
        speed_fraction: 0.0,
        mode: MovementMode::Rigid,
    });

    // Ship inside 150 km safe zone (at 50 km) vs ship outside safe zone (at 500 km) with 40 hull HP
    bp.add_ball(Ball {
        id: 80_001,
        type_id: 587,
        position: DVec3::new(50_000.0, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 2.0,
        mass: 1e6,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    });
    bp.combat_states.get_mut(&80_001).unwrap().structure_hp = 100.0;
    bp.add_ball(Ball {
        id: 80_002,
        type_id: 587,
        position: DVec3::new(500_000.0, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 2.0,
        mass: 1e6,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    });
    bp.combat_states.get_mut(&80_002).unwrap().structure_hp = 40.0;

    // Non-Zarzakh system does nothing
    assert!(step_zarzakh_gravity_hazard_in_ballpark(&mut bp, Some(&ctx.sde), 30_000_142).is_empty());

    // Zarzakh system destroys exposed 40 HP hull and leaves safe-zone ship unharmed
    let destroyed = step_zarzakh_gravity_hazard_in_ballpark(&mut bp, Some(&ctx.sde), 30_100_000);
    assert_eq!(destroyed, vec![80_002]);
    assert!((bp.combat_states[&80_001].structure_hp - 100.0).abs() < 1e-6);
}
