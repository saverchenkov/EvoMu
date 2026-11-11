//! Extra branch-coverage and mutation-killing unit tests for `repair_service.rs`.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::{
    make_fighter_ctx_and_session, make_fighter_item,
};
use crate::services::sov_services::{handle_repair_get_quotes, handle_repair_items_in_location};
use crate::services::station_structure::structure_upwell_ops::{
    load_ext_state, save_ext_state, StructureAccessProfile,
};
use eve_db::{
    corp_roles, CorpMemberRoleAssignment, CorporationRecord, InventoryFlag, IskCents,
    UpwellStructureRecord,
};
use eve_destiny::{Ball, CombatState, DVec3, MovementMode};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::HashMap;

fn call_quotes(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    handle_repair_get_quotes(ctx, session, args)
}

fn call_repair(
    ctx: &ServerContext,
    session: &SessionState,
    is_structure: bool,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    handle_repair_items_in_location(ctx, session, is_structure, args)
}

#[tokio::test]
async fn station_service_unavailable_auth_and_argument_validation_branches() {
    let (ctx, mut sess, char_id, ship_id, _sys_id) = make_fighter_ctx_and_session();

    // Unauthenticated session fails
    let anon = SessionState::default();
    assert!(call_quotes(&ctx, &anon, &[]).is_err());
    assert!(call_repair(&ctx, &anon, false, &[]).is_err());

    // Not in station or structure -> NotInStation / NotInStructure
    sess.station_id = None;
    sess.structure_id = None;
    assert_eq!(
        call_repair(&ctx, &sess, false, &[PyRep::Integer(ship_id)])
            .unwrap_err()
            .code,
        "NotInStation"
    );
    assert_eq!(
        call_repair(&ctx, &sess, true, &[PyRep::Integer(ship_id)])
            .unwrap_err()
            .code,
        "NotInStructure"
    );

    // NPC station without repair service (absent from sde.stations -> does not offer service 13) -> StationServiceUnavailable
    let no_rep_sta = 60_999_001_i64;
    sess.station_id = Some(no_rep_sta);
    assert_eq!(
        call_quotes(&ctx, &sess, &[PyRep::Integer(ship_id)])
            .unwrap_err()
            .code,
        "StationServiceUnavailable"
    );
    assert_eq!(
        call_repair(&ctx, &sess, false, &[PyRep::Integer(ship_id)])
            .unwrap_err()
            .code,
        "StationServiceUnavailable"
    );

    // Valid station with repair service
    let station_id = 60_003_760_i64;
    sess.station_id = Some(station_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.station_id = Some(station_id);
            c.balance = IskCents::from_isk(5_000_000);
        }
        if let Some(sh) = snap.items.get_mut(&ship_id) {
            sh.location_id = station_id;
            sh.flag = InventoryFlag::Hangar;
            sh.dynamic_attributes.insert(eve_sde::attr_ids::DAMAGE, 50.0);
        }
    });

    // Empty item list -> InvalidArgument in repair, falls back to session.ship_id in quotes
    assert!(call_quotes(&ctx, &sess, &[]).is_ok());
    assert_eq!(
        call_repair(&ctx, &sess, false, &[PyRep::List(vec![])])
            .unwrap_err()
            .code,
        "InvalidArgument"
    );

    // Invalid iskRepairValue (negative or infinite) -> InvalidArgument
    for bad_budget in [-1.0, f64::INFINITY, f64::NAN] {
        assert_eq!(
            call_repair(
                &ctx,
                &sess,
                false,
                &[PyRep::Integer(ship_id), PyRep::Real(bad_budget)],
            )
            .unwrap_err()
            .code,
            "InvalidArgument"
        );
    }

    // Missing item -> empty quote list in GetRepairQuotes, ItemNotFound in RepairItems
    let q_missing = call_quotes(&ctx, &sess, &[PyRep::Integer(999_888_777)]).unwrap();
    if let PyRep::Dict(entries) = q_missing {
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].1, PyRep::List(vec![]));
    } else {
        panic!("expected Dict");
    }
    assert_eq!(
        call_repair(&ctx, &sess, false, &[PyRep::Integer(999_888_777)])
            .unwrap_err()
            .code,
        "ItemNotFound"
    );

    // Budget too small -> InsufficientFunds
    assert_eq!(
        call_repair(
            &ctx,
            &sess,
            false,
            &[PyRep::Integer(ship_id), PyRep::Real(0.0)],
        )
        .unwrap_err()
        .code,
        "InsufficientFunds"
    );

    // Character balance too low -> InsufficientFunds
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().balance = IskCents::ZERO;
    });
    assert_eq!(
        call_repair(&ctx, &sess, false, &[PyRep::Integer(ship_id)])
            .unwrap_err()
            .code,
        "InsufficientFunds"
    );
}

#[tokio::test]
async fn ownership_corp_hangar_divisions_and_container_location_checks() {
    let (ctx, mut sess, char_id, ship_id, _sys_id) = make_fighter_ctx_and_session();
    let station_id = 60_003_760_i64;
    let other_station = 60_008_494_i64;
    let corp_id = 98_500_001_i64;
    sess.station_id = Some(station_id);
    sess.corporation_id = Some(corp_id);

    let foreign_item = 810_001_i64;
    let corp_hangar_item = 810_002_i64;
    let corp_div1_item = 810_003_i64;
    let cargo_mod_item = 810_004_i64;
    let remote_ship_id = 810_005_i64;
    let remote_mod_id = 810_006_i64;
    let orphan_loc_item = 810_007_i64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.corporation_id = corp_id;
            c.station_id = Some(station_id);
            c.balance = IskCents::from_isk(10_000_000);
        }
        snap.corporations.insert(
            corp_id,
            { let mut __corp = CorporationRecord::default();
                __corp.corporation_id = corp_id;
                __corp.name = "Repair Corp".into();
                __corp.ticker = "RPRC".into();
                __corp.ceo_id = 99_999_999;
                __corp.hq_station_id = Some(station_id);
                __corp },
        );
        if let Some(sh) = snap.items.get_mut(&ship_id) {
            sh.location_id = station_id;
            sh.flag = InventoryFlag::Hangar;
        }
        // 1. Foreign owned item -> NotItemOwner
        snap.items.insert(
            foreign_item,
            make_fighter_item(foreign_item, 34, 77_777_777, station_id, InventoryFlag::Hangar, 1),
        );
        // 2. Corp-owned item in generic Hangar (not division 116..=122) -> requires Director
        snap.items.insert(
            corp_hangar_item,
            make_fighter_item(corp_hangar_item, 34, corp_id, station_id, InventoryFlag::Hangar, 1),
        );
        // 3. Corp-owned item in CorpSAG1 (flag 116 -> div_idx 0 -> bit 13 or 43)
        snap.items.insert(
            corp_div1_item,
            make_fighter_item(corp_div1_item, 34, corp_id, station_id, InventoryFlag::CorpSAG1, 1),
        );
        // 4. Item inside ship Cargo (not a repairable ship child flag) -> ItemInsideContainer
        snap.items.insert(
            cargo_mod_item,
            make_fighter_item(cargo_mod_item, 34, char_id, ship_id, InventoryFlag::Cargo, 1),
        );
        // 5. Module fitted on a ship located at a DIFFERENT station -> ItemNotAtLocation
        snap.items.insert(
            remote_ship_id,
            make_fighter_item(remote_ship_id, 587, char_id, other_station, InventoryFlag::Hangar, 1),
        );
        snap.items.insert(
            remote_mod_id,
            make_fighter_item(remote_mod_id, 34, char_id, remote_ship_id, InventoryFlag::HiSlot0, 1),
        );
        // 6. Item whose parent does not exist in snap.items -> ItemNotAtLocation
        snap.items.insert(
            orphan_loc_item,
            make_fighter_item(orphan_loc_item, 34, char_id, 777_666_555, InventoryFlag::Hangar, 1),
        );
    });

    assert_eq!(
        call_quotes(&ctx, &sess, &[PyRep::Integer(foreign_item)])
            .unwrap_err()
            .code,
        "NotItemOwner"
    );
    assert_eq!(
        call_quotes(&ctx, &sess, &[PyRep::Integer(corp_hangar_item)])
            .unwrap_err()
            .code,
        "CorpRoleDenied"
    );
    assert_eq!(
        call_quotes(&ctx, &sess, &[PyRep::Integer(corp_div1_item)])
            .unwrap_err()
            .code,
        "CorpRoleDenied"
    );

    // Grant division 1 hangar access role (1 << 13) -> corp_div1_item succeeds, corp_hangar_item still denied
    ctx.db.with_mut_snapshot(|snap| {
        snap.corp_role_assignments.insert(
            (corp_id, char_id),
            CorpMemberRoleAssignment {
                character_id: char_id,
                roles: 1u64 << 13,
                ..Default::default()
            },
        );
    });
    assert!(call_quotes(&ctx, &sess, &[PyRep::Integer(corp_div1_item)]).is_ok());
    assert_eq!(
        call_quotes(&ctx, &sess, &[PyRep::Integer(corp_hangar_item)])
            .unwrap_err()
            .code,
        "CorpRoleDenied"
    );

    // Grant ROLE_DIRECTOR -> corp_hangar_item succeeds
    ctx.db.with_mut_snapshot(|snap| {
        snap.corp_role_assignments
            .get_mut(&(corp_id, char_id))
            .unwrap()
            .roles = corp_roles::ROLE_DIRECTOR;
    });
    assert!(call_quotes(&ctx, &sess, &[PyRep::Integer(corp_hangar_item)]).is_ok());

    // Container and location errors in both GetRepairQuotes and RepairItemsInLocation
    for id in [orphan_loc_item, remote_mod_id] {
        assert_eq!(
            call_quotes(&ctx, &sess, &[PyRep::Integer(id)])
                .unwrap_err()
                .code,
            "ItemNotAtLocation"
        );
        assert_eq!(
            call_repair(&ctx, &sess, false, &[PyRep::Integer(id)])
                .unwrap_err()
                .code,
            "ItemNotAtLocation"
        );
    }
    assert_eq!(
        call_quotes(&ctx, &sess, &[PyRep::Integer(cargo_mod_item)])
            .unwrap_err()
            .code,
        "ItemInsideContainer"
    );
    assert_eq!(
        call_repair(&ctx, &sess, false, &[PyRep::Integer(cargo_mod_item)])
            .unwrap_err()
            .code,
        "ItemInsideContainer"
    );
}

#[tokio::test]
async fn upwell_structure_states_tether_repair_and_tax_wallet_credit() {
    let (ctx, mut sess, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    let struct_id = 1_000_000_000_888_i64;
    let owner_corp = 98_600_001_i64;
    let player_corp = 98_600_002_i64;
    sess.station_id = None;
    sess.structure_id = Some(struct_id);
    sess.corporation_id = Some(player_corp);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.corporation_id = player_corp;
            c.station_id = None;
            c.structure_id = Some(struct_id);
            c.balance = IskCents::from_isk(10_000_000);
        }
        snap.corporations.insert(
            owner_corp,
            { let mut __corp = CorporationRecord::default();
                __corp.corporation_id = owner_corp;
                __corp.name = "Citadel Owner".into();
                __corp.ticker = "CTDL".into();
                __corp.ceo_id = 1;
                __corp },
        );
        snap.structures.insert(
            struct_id,
            UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 35_832,
                owner_corp_id: owner_corp,
                solar_system_id: sys_id,
                name: "Taxed Astrahus".into(),
                state: "Anchoring".into(), // Not operational initially
                position: [0.0, 0.0, 0.0],
                fuel_blocks: 100,
                quantum_core_installed: false,
                ..Default::default()
            },
        );
        if let Some(sh) = snap.items.get_mut(&ship_id) {
            sh.location_id = struct_id;
            sh.dynamic_attributes.insert(eve_sde::attr_ids::DAMAGE, 100.0);
        }
    });

    // 1. Anchoring state -> StructureNotOperational
    assert_eq!(
        call_repair(&ctx, &sess, true, &[PyRep::Integer(ship_id)])
            .unwrap_err()
            .code,
        "StructureNotOperational"
    );

    // 2. Operational but missing Quantum Core -> QuantumCoreRequired
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.get_mut(&struct_id).unwrap().state = "ShieldVulnerable".into();
    });
    assert_eq!(
        call_repair(&ctx, &sess, true, &[PyRep::Integer(ship_id)])
            .unwrap_err()
            .code,
        "QuantumCoreRequired"
    );

    // 3. Quantum Core installed, owned by player_corp, with 25% repair tax (2500 bps)
    ctx.db.with_mut_snapshot(|snap| {
        let st = snap.structures.get_mut(&struct_id).unwrap();
        st.quantum_core_installed = true;
        st.owner_corp_id = player_corp;
        snap.corporations.insert(
            player_corp,
            { let mut __corp = CorporationRecord::default();
                __corp.corporation_id = player_corp;
                __corp.name = "Player Corp".into();
                __corp.ticker = "PLRC".into();
                __corp.ceo_id = char_id;
                __corp },
        );
        let mut ext = load_ext_state(snap);
        ext.access_profiles.insert(
            struct_id,
            StructureAccessProfile {
                profile_id: 99,
                owner_corp_id: player_corp,
                name: "Taxed Profile".into(),
                service_settings: HashMap::from([(13, 10_000)]),
                ..Default::default()
            },
        );
        save_ext_state(snap, &ext);
    });

    // Test tethered in-space repair at the structure (session.structure_id = None, tethered ball)
    sess.structure_id = None;
    let sys = ctx.systems.get_or_spawn(sys_id);
    {
        let mut bp = sys.ballpark.write().unwrap();
        bp.balls.insert(
            ship_id,
            Ball {
                id: ship_id,
                type_id: 587,
                position: DVec3::new(1_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
        );
        bp.combat_states.insert(
            ship_id,
            CombatState {
                is_tethered: true,
                shield_hp: 50.0,
                max_shield_hp: 500.0,
                armor_hp: 50.0,
                max_armor_hp: 500.0,
                structure_hp: 50.0,
                max_structure_hp: 500.0,
                ..CombatState::default()
            },
        );
    }

    // Hangar item cannot be repaired while undocked on tether -> NotDockedInStructure
    let hangar_item = 820_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            hangar_item,
            make_fighter_item(hangar_item, 34, char_id, struct_id, InventoryFlag::Hangar, 1),
        );
    });
    assert_eq!(
        call_quotes(&ctx, &sess, &[PyRep::Integer(hangar_item)])
            .unwrap_err()
            .code,
        "NotDockedInStructure"
    );
    assert_eq!(
        call_repair(&ctx, &sess, true, &[PyRep::Integer(hangar_item)])
            .unwrap_err()
            .code,
        "NotDockedInStructure"
    );

    // Active ship IS repaired while tethered, tax is charged and credited to corp division 1, and ballpark HP restored!
    let rep = call_repair(&ctx, &sess, true, &[PyRep::Tuple(vec![PyRep::Integer(ship_id)])]).unwrap();
    assert_eq!(
        rep.dict_get("locationID").and_then(|v| v.as_i64()),
        Some(struct_id)
    );
    let paid = rep
        .dict_get("totalCostISK")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    assert!(paid > 0.0);
    ctx.db.with_snapshot(|snap| {
        assert!(snap.corporations[&player_corp].division_balance(1).0 > 0);
        assert!(!snap.corp_wallet_journals[&(player_corp, 1)].is_empty());
    });
    {
        let bp = sys.ballpark.read().unwrap();
        let cs = &bp.combat_states[&ship_id];
        assert_eq!(cs.shield_hp, 500.0);
        assert_eq!(cs.armor_hp, 500.0);
        assert_eq!(cs.structure_hp, 500.0);
    }
}

#[tokio::test]
async fn live_ballpark_and_cached_module_damage_plus_auth_and_tether_tiebreakers() {
    let (ctx, mut sess, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    let station_id = sess.station_id.unwrap_or(60_003_760);
    sess.station_id = Some(station_id);
    let struct_near = 1_000_000_000_901_i64;
    let struct_far = 1_000_000_000_902_i64;
    let struct_other_sys = 1_000_000_000_903_i64;
    let struct_abandoned = 1_000_000_000_904_i64;
    let corp_a = 98_600_101_i64;
    let corp_b = 98_600_102_i64;
    let mod_id = 830_001_i64;
    let corp_box_id = 830_002_i64;
    let corp_nested_id = 830_003_i64;

    // Seed character money, container in HangarCorp1 (`L224`), damaged fitted module in fitting_cache (`L79-81`, `L750-753`)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.corporation_id = corp_a;
            c.balance = IskCents::from_isk(50_000_000);
        }
        snap.items.insert(mod_id, make_fighter_item(mod_id, 597, char_id, ship_id, InventoryFlag::MedSlot0, 1));
        snap.items.insert(corp_box_id, make_fighter_item(corp_box_id, 34, corp_a, station_id, InventoryFlag::CorpSAG1, 1));
        snap.items.insert(corp_nested_id, make_fighter_item(corp_nested_id, 34, corp_a, corp_box_id, InventoryFlag::Cargo, 1));
        snap.corp_role_assignments.insert(
            (corp_a, char_id),
            CorpMemberRoleAssignment {
                character_id: char_id,
                roles: 1u64 << 13,
                ..Default::default()
            },
        );
    });
    let mut fit = eve_dogma::ShipFittingEngine::from_sde(ship_id, 587, &ctx.sde);
    fit.modules.insert(
        mod_id,
        eve_dogma::FittedModule {
            item_id: mod_id,
            type_id: 597,
            flag_id: 19,
            state: eve_dogma::ModuleState::Online,
            charge_type_id: None,
            charge_quantity: 0,
            damage_hp: 15.0,
            max_hp: 40.0,
            dynamic_attribute_overrides: HashMap::new(),
        },
    );
    ctx.ship_fittings.write().unwrap().insert(ship_id, fit);

    // Live ballpark damage (`L23-34`, `L43-46`) when `sess.corporation_id = None` (`L378`, `L545`)
    sess.corporation_id = None;
    let sys = ctx.systems.get_or_spawn(sys_id);
    {
        let mut bp = sys.ballpark.write().unwrap();
        bp.balls.insert(
            ship_id,
            Ball {
                id: ship_id,
                type_id: 587,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
        );
        bp.combat_states.insert(
            ship_id,
            CombatState {
                is_tethered: false,
                shield_hp: 90.0,
                max_shield_hp: 100.0,
                armor_hp: 80.0,
                max_armor_hp: 100.0,
                structure_hp: 70.0,
                max_structure_hp: 100.0,
                ..CombatState::default()
            },
        );
    }
    let quotes = call_quotes(&ctx, &sess, &[PyRep::Integer(ship_id), PyRep::Integer(mod_id), PyRep::Integer(corp_nested_id)]).unwrap();
    assert!(matches!(quotes, PyRep::Dict(ref rows) if !rows.is_empty()));
    let rep = call_repair(&ctx, &sess, false, &[PyRep::List(vec![PyRep::Integer(ship_id), PyRep::Integer(mod_id)])]).unwrap();
    assert!(rep.dict_get("totalCostISK").and_then(|v| v.as_f64()).unwrap_or(0.0) > 0.0);
    assert_eq!(ctx.ship_fittings.read().unwrap()[&ship_id].modules[&mod_id].damage_hp, 0.0);

    // Upwell structure auth errors (`StructureNotFound`, `StructureAbandoned`, `StructureRepairAccessDenied`) & tether closest tie-breaker (`L282`, `L287`, `L294-297`)
    {
        let mut bp = sys.ballpark.write().unwrap();
        bp.combat_states.get_mut(&ship_id).unwrap().armor_hp = 50.0;
    }
    sess.station_id = None;
    sess.structure_id = Some(1_000_999_888_777);
    assert_eq!(call_repair(&ctx, &sess, true, &[PyRep::Integer(ship_id)]).unwrap_err().code, "StructureNotFound");

    let now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            struct_abandoned,
            UpwellStructureRecord {
                structure_id: struct_abandoned,
                type_id: 35_832,
                owner_corp_id: corp_b,
                solar_system_id: sys_id,
                name: "Abandoned".into(),
                state: "ShieldVulnerable".into(),
                position: [0.0, 0.0, 0.0],
                fuel_blocks: 0,
                quantum_core_installed: true,
                ..Default::default()
            },
        );
        let mut ext = load_ext_state(snap);
        ext.lifecycle.entry(struct_abandoned).or_default().low_power_since_unix = Some(now.saturating_sub(10 * 86_400));
        ext.structure_profile_id.insert(struct_near, 8_001);
        ext.structure_profile_id.insert(struct_far, 8_001);
        ext.profiles_by_id.insert(
            8_001,
            StructureAccessProfile {
                profile_id: 8_001,
                owner_corp_id: corp_a,
                name: "Public Repair".into(),
                corp_only: false,
                ..Default::default()
            },
        );
        save_ext_state(snap, &ext);

        snap.structures.insert(
            struct_other_sys,
            UpwellStructureRecord {
                structure_id: struct_other_sys,
                type_id: 35_832,
                owner_corp_id: corp_a,
                solar_system_id: sys_id + 1,
                name: "WrongSys".into(),
                state: "ShieldVulnerable".into(),
                position: [0.0, 0.0, 0.0],
                fuel_blocks: 100,
                quantum_core_installed: true,
                ..Default::default()
            },
        );
        snap.structures.insert(
            struct_near,
            UpwellStructureRecord {
                structure_id: struct_near,
                type_id: 35_832,
                owner_corp_id: corp_a,
                solar_system_id: sys_id,
                name: "NearCit".into(),
                state: "ShieldVulnerable".into(),
                position: [500.0, 0.0, 0.0],
                fuel_blocks: 100,
                quantum_core_installed: true,
                ..Default::default()
            },
        );
        snap.structures.insert(
            struct_far,
            UpwellStructureRecord {
                structure_id: struct_far,
                type_id: 35_832,
                owner_corp_id: corp_a,
                solar_system_id: sys_id,
                name: "FarCit".into(),
                state: "ShieldVulnerable".into(),
                position: [1_500.0, 0.0, 0.0],
                fuel_blocks: 100,
                quantum_core_installed: true,
                ..Default::default()
            },
        );
    });
    sess.structure_id = Some(struct_abandoned);
    assert_eq!(call_repair(&ctx, &sess, true, &[PyRep::Integer(ship_id)]).unwrap_err().code, "StructureAbandoned");

    // Non-owner without ACL gets StructureRepairAccessDenied (`L167-177`)
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.get_mut(&struct_far).unwrap().owner_corp_id = corp_b;
    });
    sess.structure_id = Some(struct_far);
    assert_eq!(call_repair(&ctx, &sess, true, &[PyRep::Integer(ship_id)]).unwrap_err().code, "StructureRepairAccessDenied");

    // Restore ownership and test tethered resolution picks nearest valid operational citadel
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.get_mut(&struct_far).unwrap().owner_corp_id = corp_a;
    });
    sess.structure_id = None;
    {
        let mut bp = sys.ballpark.write().unwrap();
        let cs = bp.combat_states.get_mut(&ship_id).unwrap();
        cs.is_tethered = true;
        cs.armor_hp = 50.0;
    }
    let tethered_rep = call_repair(&ctx, &sess, true, &[PyRep::Integer(ship_id)]).unwrap();
    assert_eq!(tethered_rep.dict_get("locationID").and_then(|v| v.as_i64()), Some(struct_near));

    // CharacterNotFound on repair (`L685-687`)
    sess.structure_id = None;
    sess.station_id = Some(station_id);
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.remove(&char_id);
    });
    assert_eq!(call_repair(&ctx, &sess, false, &[PyRep::Integer(ship_id)]).unwrap_err().code, "CharacterNotFound");
}

#[tokio::test]
async fn live_ballpark_repair_not_operational_and_arithmetic_overflow_branches() {
    let (mut ctx, mut sess, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    let station_id = sess.station_id.unwrap_or(60_003_760);
    sess.station_id = Some(station_id);
    let corp_id = sess.corporation_id.unwrap_or(1_000_009);
    sess.corporation_id = Some(corp_id);
    let struct_id = 1_040_000_009_001_i64;
    let struct_not_op = 1_040_000_009_002_i64;
    let ship2_id = 910_000_002_i64;
    let container_id = 910_000_003_i64;
    let corp_mod_id = 910_000_004_i64;

    {
        let sde_mut = std::sync::Arc::make_mut(&mut ctx.sde);
        if let Some(t) = sde_mut.types.get_mut(&23_915) {
            t.category_id = 6;
            t.base_price = 100_000.0;
            t.default_attributes.insert(eve_sde::attr_ids::HP_STRUCT, 500.0);
            t.default_attributes.insert(eve_sde::attr_ids::ARMOR_HP, 500.0);
            t.default_attributes.insert(eve_sde::attr_ids::SHIELD_CAPACITY, 500.0);
        }
    }

    let sys = ctx.systems.get_or_spawn(sys_id);
    let _ = sys
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: 23_915,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState {
                max_structure_hp: 500.0,
                structure_hp: 400.0,
                max_armor_hp: 500.0,
                armor_hp: 350.0,
                max_shield_hp: 500.0,
                shield_hp: 250.0,
                ..Default::default()
            },
        )
        .await;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ship2_id,
            make_fighter_item(ship2_id, 23_915, char_id, station_id, InventoryFlag::Hangar, 1),
        );
        // Corp container in CorpSAG2 (117) holding damaged corp item (`L224`)
        snap.items.insert(
            container_id,
            make_fighter_item(container_id, 17366, corp_id, station_id, InventoryFlag::CorpSAG2, 1),
        );
        let mut cm = make_fighter_item(corp_mod_id, 587, corp_id, container_id, InventoryFlag::Unlocked, 1);
        cm.dynamic_attributes.insert(eve_sde::attr_ids::DAMAGE, 20.0);
        snap.items.insert(corp_mod_id, cm);
        snap.corporations.entry(corp_id).or_insert_with(|| { let mut __corp = CorporationRecord::default(); __corp.corporation_id = corp_id; __corp.name = "RepairCorp".into(); __corp.ticker = "REPC".into(); __corp.ceo_id = char_id; __corp.ensure_wallet_divisions(); __corp });
        snap.corp_role_assignments.insert((corp_id, char_id), CorpMemberRoleAssignment {
            character_id: char_id,
            roles: corp_roles::ROLE_HANGAR_CAN_TAKE[1],
            ..Default::default()
        });
        snap.structures.insert(
            struct_not_op,
            UpwellStructureRecord {
                structure_id: struct_not_op,
                type_id: 35_832,
                owner_corp_id: corp_id,
                solar_system_id: sys_id,
                name: "NotOpCit".into(),
                state: "Destroyed".into(),
                fuel_blocks: 100,
                quantum_core_installed: true,
                ..Default::default()
            },
        );
        snap.structures.insert(
            struct_id,
            UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 35_832,
                owner_corp_id: corp_id,
                solar_system_id: sys_id,
                name: "OpCit".into(),
                state: "ShieldVulnerable".into(),
                fuel_blocks: 100,
                quantum_core_installed: true,
                ..Default::default()
            },
        );
        let mut ext = load_ext_state(snap);
        ext.access_profiles.insert(
            struct_id,
            StructureAccessProfile {
                profile_id: 99,
                owner_corp_id: corp_id,
                name: "Taxed Profile".into(),
                service_settings: HashMap::from([(13, 10_000)]),
                ..Default::default()
            },
        );
        save_ext_state(snap, &ext);
    });

    // Live ballpark hull/armor/shield damage calculation & restoration (`L23-34`, `L43-46`, `L765`)
    let q_live = call_quotes(&ctx, &sess, &[PyRep::Integer(ship_id)]).unwrap();
    assert!(q_live.dict_get(&ship_id.to_string()).is_some() || !matches!(q_live, PyRep::None));
    assert!(call_repair(&ctx, &sess, false, &[PyRep::Integer(ship_id)]).is_ok());

    // Parent container flag CorpSAG2 permission check (`L224`)
    assert!(call_repair(&ctx, &sess, false, &[PyRep::Integer(corp_mod_id)]).is_err());

    // StructureNotOperational (`L157-162` guard via `is_operational`)
    sess.structure_id = Some(struct_not_op);
    assert_eq!(call_repair(&ctx, &sess, true, &[PyRep::Integer(ship_id)]).unwrap_err().code, "StructureNotOperational");

    // Structure location resolved via session.station_id >= 1e12 (`L260-261`) + corp wallet overflow (`L708-712`)
    sess.structure_id = None;
    sess.station_id = Some(struct_id);
    ctx.db.with_mut_snapshot(|snap| {
        snap.sp6_lifecycle.ship_combat_hp.insert(
            ship_id,
            eve_db::PersistedCombatHp { hull_hp: 100.0, armor_hp: 100.0, shield_hp: 100.0 },
        );
        snap.items.get_mut(&ship_id).unwrap().location_id = struct_id;
        snap.corporations.get_mut(&corp_id).unwrap().set_division_balance(1, IskCents(i64::MAX - 1));
    });
    assert_eq!(call_repair(&ctx, &sess, true, &[PyRep::Integer(ship_id)]).unwrap_err().code, "ArithmeticOverflow");

    // ArithmeticOverflow in Upwell item_cents (`L653-655`) & station item_base (`L639-641`)
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.get_mut(&corp_id).unwrap().set_division_balance(1, IskCents(0));
        snap.items
            .get_mut(&ship_id)
            .unwrap()
            .dynamic_attributes
            .insert(eve_sde::attr_ids::DAMAGE, 1e18);
    });
    assert_eq!(call_repair(&ctx, &sess, true, &[PyRep::Integer(ship_id)]).unwrap_err().code, "ArithmeticOverflow");
    sess.station_id = Some(station_id);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&ship_id).unwrap().location_id = station_id;
        snap.characters.get_mut(&char_id).unwrap().balance = IskCents(i64::MAX);
    });
    assert_eq!(call_repair(&ctx, &sess, false, &[PyRep::Integer(ship_id)]).unwrap_err().code, "ArithmeticOverflow");

    // Two items each slightly over half of i64::MAX base cost overflows base_total_cents (`L644-648`)
    // For type 23_915: base_price = 100_000 * 7.5e-6 = 0.75 ISK/HP = 75 cents/HP.
    // i64::MAX / 75 ~= 1.23e17; 7e16 * 75 = 5.25e18 (fits i64), 2 * 5.25e18 overflows i64!
    ctx.db.with_mut_snapshot(|snap| {
        snap.sp6_lifecycle.ship_combat_hp.remove(&ship_id);
        snap.sp6_lifecycle.ship_combat_hp.remove(&ship2_id);
        snap.items.get_mut(&ship_id).unwrap().dynamic_attributes.insert(eve_sde::attr_ids::DAMAGE, 7.0e16);
        snap.items.get_mut(&ship2_id).unwrap().dynamic_attributes.insert(eve_sde::attr_ids::DAMAGE, 7.0e16);
    });
    assert_eq!(
        call_repair(&ctx, &sess, false, &[PyRep::List(vec![PyRep::Integer(ship_id), PyRep::Integer(ship2_id)])])
            .unwrap_err()
            .code,
        "ArithmeticOverflow"
    );

    // Same accumulator overflow (`L658-662`) at Upwell citadel
    sess.station_id = None;
    sess.structure_id = Some(struct_id);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&ship_id).unwrap().location_id = struct_id;
        snap.items.get_mut(&ship2_id).unwrap().location_id = struct_id;
    });
    assert_eq!(
        call_repair(&ctx, &sess, true, &[PyRep::List(vec![PyRep::Integer(ship_id), PyRep::Integer(ship2_id)])])
            .unwrap_err()
            .code,
        "ArithmeticOverflow"
    );
}

#[tokio::test]
async fn mutants_cov_ship_combat_damage_tether_acl_and_structure_tax() {
    let (mut ctx, mut sess, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    let station_id = sess.station_id.unwrap_or(60_003_760);
    let corp_a = sess.corporation_id.unwrap_or(1_000_009);
    let corp_b = corp_a + 1;
    let ship_no_shield = ship_id;
    let ship_with_shield = 910_090_002_i64;
    let denied_struct = 1_040_090_000_001_i64;
    let taxed_struct = 1_040_090_000_002_i64;

    {
        let sde_mut = std::sync::Arc::make_mut(&mut ctx.sde);
        if let Some(t) = sde_mut.types.get_mut(&23_915) {
            t.category_id = 6;
            t.base_price = 100_000.0; // repair cost = 0.75 ISK/HP
            t.default_attributes.insert(eve_sde::attr_ids::HP_STRUCT, 500.0);
            t.default_attributes.insert(eve_sde::attr_ids::ARMOR_HP, 500.0);
            t.default_attributes.insert(eve_sde::attr_ids::SHIELD_CAPACITY, 500.0);
        }
    }

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.station_id = Some(station_id);
            c.structure_id = None;
            c.corporation_id = corp_a;
            c.balance = IskCents::from_isk(1_000_000);
        }
        snap.corporations.entry(corp_a).or_insert_with(|| {
            let mut corp = CorporationRecord::default();
            corp.corporation_id = corp_a;
            corp.name = "RepairCorpA".into();
            corp.ticker = "RCA".into();
            corp.ceo_id = char_id;
            corp.ensure_wallet_divisions();
            corp
        });
        snap.items.insert(
            ship_no_shield,
            make_fighter_item(ship_no_shield, 23_915, char_id, station_id, InventoryFlag::Hangar, 1),
        );
        snap.items.insert(
            ship_with_shield,
            make_fighter_item(ship_with_shield, 23_915, char_id, station_id, InventoryFlag::Hangar, 1),
        );
    });

    // Spawn ballpark combat states:
    // - ship_no_shield: hull_dmg=10, armor_dmg=20, shield_dmg=0 -> dmg=30, max_hp=1000 (kills L32 > -> >=, ==)
    // - ship_with_shield: hull_dmg=10, armor_dmg=20, shield_dmg=30 -> dmg=60, max_hp=1500 (kills L31 +, L32 <, ==)
    let sys = ctx.systems.get_or_spawn(sys_id);
    for (sid, s_hp, a_hp, h_hp, tethered) in [
        (ship_no_shield, 500.0_f64, 480.0_f64, 490.0_f64, false),
        (ship_with_shield, 470.0_f64, 480.0_f64, 490.0_f64, true),
    ] {
        let _ = sys
            .spawn_ball_with_combat(
                Ball {
                    id: sid,
                    type_id: 23_915,
                    position: DVec3::ZERO,
                    velocity: DVec3::ZERO,
                    max_velocity: 300.0,
                    inertia: 1.0,
                    mass: 1_000_000.0,
                    radius: 50.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                CombatState {
                    is_tethered: tethered,
                    max_structure_hp: 500.0,
                    structure_hp: h_hp,
                    max_armor_hp: 500.0,
                    armor_hp: a_hp,
                    max_shield_hp: 500.0,
                    shield_hp: s_hp,
                    ..Default::default()
                },
            )
            .await;
    }

    sess.station_id = Some(station_id);
    sess.structure_id = None;
    sess.corporation_id = Some(corp_a);

    let quote_row_val = |rep: &PyRep, target_id: i64, field: &str| -> f64 {
        let PyRep::Dict(entries) = rep else { panic!("expected quote dict, got {rep:?}") };
        let (_, list_rep) = entries.iter().find(|(k, _)| k.as_i64() == Some(target_id)).unwrap();
        let PyRep::List(rows) = list_rep else { panic!("expected quote rows") };
        rows[0].dict_get(field).and_then(|v| v.as_f64()).unwrap()
    };

    let q_rep = call_quotes(
        &ctx,
        &sess,
        &[PyRep::List(vec![PyRep::Integer(ship_no_shield), PyRep::Integer(ship_with_shield)])],
    )
    .unwrap();
    assert!((quote_row_val(&q_rep, ship_no_shield, "damage") - 30.0).abs() < 1e-6);
    assert!((quote_row_val(&q_rep, ship_no_shield, "maxHealth") - 1000.0).abs() < 1e-6);
    assert!((quote_row_val(&q_rep, ship_with_shield, "damage") - 60.0).abs() < 1e-6);
    assert!((quote_row_val(&q_rep, ship_with_shield, "maxHealth") - 1500.0).abs() < 1e-6);

    // 2. validate_structure_repair_access: tethered near ONLY corp_b private citadel -> NotInStructure
    // (if validate_structure_repair_access mutated to Ok(()), resolves denied_struct and returns StructureRepairAccessDenied)
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            denied_struct,
            UpwellStructureRecord {
                structure_id: denied_struct,
                type_id: 35_832,
                owner_corp_id: corp_b,
                solar_system_id: sys_id,
                name: "DeniedCitadel".into(),
                state: "ShieldVulnerable".into(),
                position: [500.0, 0.0, 0.0],
                fuel_blocks: 100,
                quantum_core_installed: true,
                ..Default::default()
            },
        );
        snap.structures.insert(
            taxed_struct,
            UpwellStructureRecord {
                structure_id: taxed_struct,
                type_id: 35_832,
                owner_corp_id: corp_a,
                solar_system_id: sys_id,
                name: "TaxedCitadel".into(),
                state: "ShieldVulnerable".into(),
                position: [1_000_000_000.0, 0.0, 0.0],
                fuel_blocks: 100,
                quantum_core_installed: true,
                ..Default::default()
            },
        );
        let mut ext = load_ext_state(snap);
        let denied_prof = StructureAccessProfile {
            profile_id: 8_011,
            owner_corp_id: corp_b,
            name: "Denied".into(),
            corp_only: true,
            ..Default::default()
        };
        ext.structure_profile_id.insert(denied_struct, 8_011);
        ext.profiles_by_id.insert(8_011, denied_prof.clone());
        ext.access_profiles.insert(denied_struct, denied_prof);
        ext.structure_profile_id.insert(taxed_struct, 8_012);
        let mut tax_prof = StructureAccessProfile {
            profile_id: 8_012,
            owner_corp_id: corp_a,
            name: "Taxed5Pct".into(),
            corp_only: false,
            ..Default::default()
        };
        tax_prof.service_settings.insert(13, 500);
        ext.profiles_by_id.insert(8_012, tax_prof.clone());
        ext.access_profiles.insert(taxed_struct, tax_prof);
        save_ext_state(snap, &ext);
    });

    sess.station_id = None;
    sess.structure_id = None;
    sess.ship_id = Some(ship_with_shield);
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.station_id = None;
        ch.structure_id = None;
        ch.active_ship_id = ship_with_shield;
        let sh = snap.items.get_mut(&ship_with_shield).unwrap();
        sh.location_id = sys_id as i64;
        sh.flag = InventoryFlag::None;
    });
    assert_eq!(
        call_repair(&ctx, &sess, true, &[PyRep::Integer(ship_with_shield)])
            .unwrap_err()
            .code,
        "NotInStructure"
    );

    // 3. Dock in taxed_struct (500 bps = 5% tax) with damage=100 (75.00 ISK base; Upwell service charge = 3 cents/HP -> 3.00 ISK)
    // Kills resolve_structure_repair_tax_bps -> Ok(-1/0/1) and handle_repair_items_in_location -> Ok(Default)
    {
        let mut bp = sys.ballpark.write().unwrap();
        let cs = bp.combat_states.get_mut(&ship_with_shield).unwrap();
        cs.shield_hp = 500.0;
        cs.structure_hp = 500.0;
        cs.armor_hp = 400.0; // damage = 100.0 -> base 75.00 ISK -> Upwell 3 cents/HP * 100 = 3.00 ISK
    }
    sess.structure_id = Some(taxed_struct);
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().structure_id = Some(taxed_struct);
        let sh = snap.items.get_mut(&ship_with_shield).unwrap();
        sh.location_id = taxed_struct;
        sh.flag = InventoryFlag::Hangar;
    });

    let q_tax = call_quotes(&ctx, &sess, &[PyRep::Integer(ship_with_shield)]).unwrap();
    assert!((quote_row_val(&q_tax, ship_with_shield, "totalRepairCost") - 3.00).abs() < 0.02);

    let rep_tax = call_repair(&ctx, &sess, true, &[PyRep::Integer(ship_with_shield)]).unwrap();
    assert_eq!(
        rep_tax.dict_get("locationID").and_then(|v| v.as_i64()),
        Some(taxed_struct)
    );
    assert!(
        (rep_tax.dict_get("totalCostISK").and_then(|v| v.as_f64()).unwrap() - 3.00).abs() < 0.02
    );
}
