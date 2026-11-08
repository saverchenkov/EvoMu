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
