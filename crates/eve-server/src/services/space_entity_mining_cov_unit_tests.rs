//! Focused unit tests for uncovered branches in `space_entity_mining`
//! (`reprocessingSvc.Reprocess` single/batch/corp/structure/remainder/errors,
//! `reprocessingSvc.GetQuotes` summary/list/tuple, and `handle_mine`).

use super::*;
use crate::router::ServerContext;
use crate::services::station_structure::structure_upwell_ops::{self, save_ext_state, StructureAccessProfile};
use eve_db::clock::VirtualServerClock;
use eve_db::types::UpwellStructureRecord;
use eve_db::{CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents, WorldRepository};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use glam::DVec3;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};

fn full_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::clone(&ServerContext::new().sde)))
}

#[tokio::test]
async fn reprocessing_quotes_and_reprocess_npc_structure_and_error_paths() {
    let ctx = ServerContext::with_sde_and_repository(
        full_sde(),
        Arc::new(WorldRepository::with_demo_seed()),
    );
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_700_000_000)));

    let char_id = 96_940_001i64;
    let corp_id = 98_940_001i64;
    let station_id = 60_003_760i64;
    let ship_id = 96_940_500i64;
    let ore_stack_id = 96_940_101i64;
    let small_stack_id = 96_940_102i64;
    let singleton_item_id = 96_940_103i64;
    let structure_id = 1_040_000_000_999i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().expect("seed char");
        ch.character_id = char_id;
        ch.user_id = 9601;
        ch.corporation_id = corp_id;
        ch.station_id = Some(station_id);
        ch.solar_system_id = 30_000_142;
        ch.active_ship_id = ship_id;
        snap.characters.insert(char_id, ch);

        let mut corp = { let mut __corp = CorporationRecord::default();
            __corp.corporation_id = corp_id;
            __corp.name = "Reproc Corp".into();
            __corp.ticker = "RPRC".into();
            __corp.ceo_id = char_id;
            __corp.set_division_balance(1, IskCents::from_f64_isk(50_000_000.0));
            __corp };
        corp.ensure_wallet_divisions();
        snap.corporations.insert(corp_id, corp);

        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        // 350 Veldspar (portion_size 100 -> 3 portions + 50 remainder!)
        snap.items.insert(
            ore_stack_id,
            InventoryItemRecord {
                item_id: ore_stack_id,
                type_id: 1230, // Veldspar
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 350,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        // 25 Veldspar (< 1 portion -> ItemQuantityTooLow)
        snap.items.insert(
            small_stack_id,
            InventoryItemRecord {
                item_id: small_stack_id,
                type_id: 1230,
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 25,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        // Unrepackaged singleton Veldspar -> ItemMustBeRepackaged
        snap.items.insert(
            singleton_item_id,
            InventoryItemRecord {
                item_id: singleton_item_id,
                type_id: 1230,
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 100,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );

        // Online Athanor refinery (type 35835) with offline Standup Reprocessing service (35899)
        snap.structures.insert(
            structure_id,
            UpwellStructureRecord {
                structure_id,
                solar_system_id: 30_000_142,
                type_id: 35835,
                owner_corp_id: corp_id,
                name: "Refinery Test".into(),
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 200,
                ..Default::default()
            },
        );
        snap.items.insert(
            96_940_801,
            InventoryItemRecord {
                item_id: 96_940_801,
                type_id: 35899,
                owner_id: corp_id,
                location_id: structure_id,
                flag: InventoryFlag::ServiceSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(96_940_801, false);
        let mut ext = structure_upwell_ops::load_ext_state(snap);
        ext.access_profiles.insert(
            structure_id,
            StructureAccessProfile {
                profile_id: 7200,
                owner_corp_id: corp_id,
                name: "RefineryACL".into(),
                allowed_corporations: HashSet::from([corp_id]),
                ..Default::default()
            },
        );
        save_ext_state(snap, &ext);
    });

    let mut sess = SessionState {
        user_id: Some(9601),
        character_id: Some(char_id),
        corporation_id: Some(corp_id),
        station_id: Some(station_id),
        solar_system_id: Some(30_000_142),
        ship_id: Some(ship_id),
        ..Default::default()
    };

    // 1. GetQuotes empty summary dict and 2-arg tuple quote
    let summary = handle(&ctx, &mut sess, "reprocessingSvc", "GetQuotes", &[], None)
        .await
        .expect("handled")
        .expect("summary quote");
    assert!(matches!(summary, PyRep::KeyVal(_)));

    let tuple_quote = handle(
        &ctx,
        &mut sess,
        "reprocessingSvc",
        "GetQuotes",
        &[PyRep::List(vec![PyRep::Integer(ore_stack_id)]), PyRep::Integer(ship_id)],
        None,
    )
    .await
    .expect("handled")
    .expect("tuple quote");
    assert!(matches!(tuple_quote, PyRep::Tuple(ref t) if t.len() == 3));

    // V24 client sends Dict {itemID: invItem} to GetQuotes; quote must return item typeID and outputs
    let dict_quote = handle(
        &ctx,
        &mut sess,
        "reprocessingSvc",
        "GetQuotes",
        &[
            PyRep::Dict(vec![(
                PyRep::Integer(ore_stack_id),
                PyRep::keyval([("itemID", PyRep::Integer(ore_stack_id))]),
            )]),
            PyRep::Integer(ship_id),
        ],
        None,
    )
    .await
    .expect("handled")
    .expect("dict quote");
    let PyRep::Tuple(ref dq_parts) = dict_quote else {
        panic!("expected 3-tuple from GetQuotes Dict, got {dict_quote:?}");
    };
    let PyRep::List(ref dq_list) = dq_parts[2] else {
        panic!("expected quote list");
    };
    assert_eq!(dq_list.len(), 1);
    assert_eq!(dq_list[0].dict_get("itemID").and_then(|v| v.as_i64()), Some(ore_stack_id));
    assert_eq!(dq_list[0].dict_get("typeID").and_then(|v| v.as_i64()), Some(1230));
    assert!(!matches!(dq_list[0].dict_get("outputs"), Some(PyRep::List(o)) if o.is_empty()));

    // 1-arg Dict (`activeShipID` omitted, `< 2` args) returns V24.01 3-tuple `(tax, eff_dict, {itemID: KeyVal})`
    let single_arg_dict_quote = handle(
        &ctx,
        &mut sess,
        "reprocessingSvc",
        "GetQuotes",
        &[PyRep::Dict(vec![(
            PyRep::Integer(ore_stack_id),
            PyRep::keyval([("itemID", PyRep::Integer(ore_stack_id))]),
        )])],
        None,
    )
    .await
    .expect("handled")
    .expect("1-arg dict quote");
    let PyRep::Tuple(ref sdq_parts) = single_arg_dict_quote else {
        panic!("expected 3-tuple from 1-arg Dict GetQuotes, got {single_arg_dict_quote:?}");
    };
    let PyRep::Dict(ref sdq_entries) = sdq_parts[2] else {
        panic!("expected {{itemID: KeyVal}} Dict in 1-arg Dict GetQuotes, got {sdq_parts:?}");
    };
    assert_eq!(sdq_entries.len(), 1);
    assert_eq!(sdq_entries[0].0, PyRep::Integer(ore_stack_id));
    assert_eq!(sdq_entries[0].1.dict_get("typeID").and_then(PyRep::as_i64), Some(1230));
    assert!(sdq_entries[0].1.dict_get("recoverables").is_some_and(PyRep::is_crowset));

    // 1-arg List and 1-arg Integer (`< 2` args) also return V24.01 3-tuple preserving `typeID` (`#723`)
    for single_arg in [
        PyRep::List(vec![PyRep::Integer(ore_stack_id)]),
        PyRep::Integer(ore_stack_id),
    ] {
        let single_res = handle(
            &ctx,
            &mut sess,
            "reprocessingSvc",
            "GetQuotes",
            &[single_arg],
            None,
        )
        .await
        .expect("handled")
        .expect("1-arg quote");
        let PyRep::Tuple(ref parts) = single_res else {
            panic!("expected 3-tuple from 1-arg GetQuotes, got {single_res:?}");
        };
        assert_eq!(parts.len(), 3);
        let PyRep::Dict(ref entries) = parts[2] else {
            panic!("expected rawQuotes Dict in 1-arg GetQuotes, got {parts:?}");
        };
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].0, PyRep::Integer(ore_stack_id));
        assert_eq!(entries[0].1.dict_get("typeID").and_then(PyRep::as_i64), Some(1230));
        assert!(entries[0].1.dict_get("recoverables").is_some_and(PyRep::is_crowset));
    }

    // 2. Reprocess active ship rejected
    let err = handle(&ctx, &mut sess, "reprocessingSvc", "Reprocess", &[PyRep::Integer(ship_id)], None)
        .await
        .expect("handled")
        .expect_err("cannot reprocess active ship");
    assert_eq!(err.code, "CannotReprocessActiveShip");

    // 3. Reprocess singleton item rejected
    let err = handle(&ctx, &mut sess, "reprocessingSvc", "Reprocess", &[PyRep::Integer(singleton_item_id)], None)
        .await
        .expect("handled")
        .expect_err("unrepackaged");
    assert_eq!(err.code, "ItemMustBeRepackaged");

    // 4. Reprocess insufficient portion rejected
    let err = handle(&ctx, &mut sess, "reprocessingSvc", "Reprocess", &[PyRep::Integer(small_stack_id)], None)
        .await
        .expect("handled")
        .expect_err("portion too small");
    assert_eq!(err.code, "InsufficientBatchPortion");

    // 5. Reprocess 350 Veldspar -> leaves 50 remainder in stack and outputs Tritanium (34)
    let out = handle(
        &ctx,
        &mut sess,
        "reprocessingSvc",
        "Reprocess",
        &[PyRep::Integer(ore_stack_id)],
        None,
    )
    .await
    .expect("handled")
    .expect("reprocess succeeds");
    assert!(matches!(out, PyRep::KeyVal(_)));
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&ore_stack_id].quantity, 50);
        assert!(snap.items.values().any(|i| i.owner_id == char_id && i.type_id == 34 && i.quantity > 0));
    });

    // 6. Undocked Reprocess rejected
    let mut space_sess = sess.clone();
    space_sess.station_id = None;
    let err = handle(&ctx, &mut space_sess, "reprocessingSvc", "Reprocess", &[PyRep::Integer(ore_stack_id)], None)
        .await
        .expect("handled")
        .expect_err("must be docked");
    assert_eq!(err.code, "NotDocked");

    // 7. Structure with offline reprocessing service -> ReprocessingServiceOffline
    let mut struct_sess = SessionState {
        user_id: Some(9601),
        character_id: Some(char_id),
        corporation_id: Some(corp_id),
        station_id: None,
        structure_id: Some(structure_id),
        solar_system_id: Some(30_000_142),
        ship_id: Some(ship_id),
        ..Default::default()
    };
    let err = handle(
        &ctx,
        &mut struct_sess,
        "reprocessingSvc",
        "Reprocess",
        &[PyRep::List(vec![PyRep::Integer(ore_stack_id)])],
        None,
    )
    .await
    .expect("handled")
    .expect_err("service offline");
    assert_eq!(err.code, "ReprocessingServiceOffline");

    // 8. CmdMineRepeatedly validation: missing asteroidID rejected
    let err = handle(
        &ctx,
        &mut space_sess,
        "beyonce",
        "CmdMineRepeatedly",
        &[PyRep::Integer(0)],
        None,
    )
    .await
    .expect("handled")
    .expect_err("invalid target asteroid");
    assert_eq!(err.code, "InvalidArgument");
    let _ = (DVec3::ZERO, HashMap::<String, PyRep>::new());
}

#[tokio::test]
async fn reprocess_stale_station_or_structure_output_hangar_and_guards_1563() {
    use crate::router::{client_wire::ClientWireSession, TestContext};
    use eve_db::{
        CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents, UpwellStructureRecord,
        WorldRepository,
    };
    use eve_proto::PyRep;
    use eve_sde::SdeRegistry;
    use std::collections::HashMap;
    use std::sync::Arc;

    const JITA_STATION_ID: i64 = 60_003_760;
    const PERIMETER_STATION_ID: i64 = 60_002_956;
    const JITA_SYSTEM_ID: u32 = 30_000_142;
    const PERIMETER_SYSTEM_ID: u32 = 30_000_144;
    const THE_FORGE_REGION_ID: u32 = 10_000_002;

    let sde = Arc::new(SdeRegistry::with_canonical_seed());
    let db = Arc::new(WorldRepository::with_demo_seed());
    let ctx = TestContext::with_sde_and_repository(sde, db).with_rpc_rate_budget_disabled();

    let uid = ctx.db.provision_player_account("reproc1563", "reproc1563-secret");
    let char_id = 940_001;
    let other_char_id = 940_002;
    let corp_id = 98_940_001;
    let ship_id = 940_500;
    let prev_struct_id = 1_000_000_940_001;
    let cur_struct_id = 1_000_000_940_002;
    let service_mod_id = 940_501;
    let ore_npc_id = 940_101;
    let ore_prev_struct_id = 940_102;
    let ore_cur_struct_id = 940_103;
    let ore_guard_id = 940_104;
    let stale_office_id = 940_801;

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().unwrap().clone();
        ch.character_id = char_id;
        ch.user_id = uid;
        ch.name = "Jita Reprocessor".into();
        ch.corporation_id = corp_id;
        ch.station_id = Some(JITA_STATION_ID);
        ch.structure_id = None;
        ch.solar_system_id = JITA_SYSTEM_ID;
        ch.constellation_id = 20_000_020;
        ch.region_id = THE_FORGE_REGION_ID;
        ch.active_ship_id = ship_id;
        ch.balance = IskCents::from_isk(10_000_000);
        snap.characters.insert(char_id, ch);

        let mut corp = CorporationRecord::default();
        corp.corporation_id = corp_id;
        corp.name = "Refining Test Corp".into();
        corp.ticker = "RF15".into();
        corp.ceo_id = char_id;
        snap.corporations.insert(corp_id, corp);

        for (sid, sys_id, name) in [
            (prev_struct_id, PERIMETER_SYSTEM_ID, "Prev Stale Raitaru"),
            (cur_struct_id, JITA_SYSTEM_ID, "Current Online Raitaru"),
        ] {
            snap.structures.insert(
                sid,
                UpwellStructureRecord {
                    structure_id: sid,
                    type_id: 35825,
                    owner_corp_id: corp_id,
                    solar_system_id: sys_id,
                    name: name.into(),
                    state: "ShieldVulnerable".into(),
                    fuel_blocks: 500,
                    quantum_core_installed: true,
                    ..Default::default()
                },
            );
        }

        // Real deployed structures can also exist in `snap.items` in space (`solar_system_id`).
        snap.items.insert(
            prev_struct_id,
            InventoryItemRecord {
                item_id: prev_struct_id,
                type_id: 35825,
                owner_id: corp_id,
                location_id: PERIMETER_SYSTEM_ID as i64,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            service_mod_id,
            InventoryItemRecord {
                item_id: service_mod_id,
                type_id: 35899,
                owner_id: corp_id,
                location_id: cur_struct_id,
                flag: InventoryFlag::ServiceSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(service_mod_id, true);

        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: char_id,
                location_id: JITA_STATION_ID,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );

        for (oid, loc) in [
            (ore_npc_id, JITA_STATION_ID),
            (ore_prev_struct_id, JITA_STATION_ID),
            (ore_guard_id, JITA_STATION_ID),
            (ore_cur_struct_id, cur_struct_id),
        ] {
            snap.items.insert(
                oid,
                InventoryItemRecord {
                    item_id: oid,
                    type_id: 1230, // Veldspar
                    owner_id: char_id,
                    location_id: loc,
                    flag: InventoryFlag::Hangar,
                    quantity: 200,
                    singleton: false,
                    ..Default::default()
                },
            );
        }

        snap.items.insert(
            stale_office_id,
            InventoryItemRecord {
                item_id: stale_office_id,
                type_id: 27,
                owner_id: corp_id,
                location_id: PERIMETER_STATION_ID,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });

    let kw = HashMap::new();
    let mut client = ClientWireSession::new(&ctx, char_id as u64);
    let login_out = client
        .call_wire(
            "authentication",
            None,
            "Login",
            &[
                PyRep::String("reproc1563".into()),
                PyRep::String("reproc1563-secret".into()),
            ],
            &kw,
        )
        .await;
    assert!(login_out.marshal_errors.is_empty());
    let login_rep = login_out.rpc_result.expect("Login must succeed");
    assert!(!matches!(login_rep, PyRep::None));
    let select_out = client
        .call_wire(
            "charUnboundMgr",
            None,
            "SelectCharacterID",
            &[PyRep::Integer(char_id)],
            &kw,
        )
        .await;
    assert!(select_out.marshal_errors.is_empty());
    let select_rep = select_out.rpc_result.expect("SelectCharacterID must succeed");
    assert!(!matches!(select_rep, PyRep::None));
    assert_eq!(client.session.station_id, Some(JITA_STATION_ID));

    // 1. Exact #1563 V24 call after undocking from Perimeter (60002956) and redocking at Jita IV-4 (60003760).
    let wire_out = client
        .call_wire(
            "reprocessingSvc",
            None,
            "Reprocess",
            &[
                PyRep::List(vec![PyRep::Integer(ore_npc_id)]),
                PyRep::Integer(JITA_STATION_ID),
                PyRep::None,
                PyRep::Integer(PERIMETER_STATION_ID),
                PyRep::Integer(InventoryFlag::Hangar as i64),
            ],
            &kw,
        )
        .await;
    assert!(wire_out.marshal_errors.is_empty());
    let wire_res = wire_out
        .rpc_result
        .expect("stale NPC station Item hangar output location must resolve to current docked station");
    let PyRep::Tuple(ref tuple_parts) = wire_res else {
        panic!("expected 2-tuple MachoNet reply, got {wire_res:?}");
    };
    assert_eq!(tuple_parts.len(), 2);
    assert_eq!(tuple_parts[0], PyRep::List(vec![PyRep::Integer(ore_npc_id)]));
    let PyRep::Dict(ref out_map) = tuple_parts[1] else {
        panic!("expected outputByTypeID dict, got {:?}", tuple_parts[1]);
    };
    let trit_wire_qty = out_map
        .iter()
        .find_map(|(k, v)| match (k, v) {
            (PyRep::Integer(34), PyRep::Integer(q)) => Some(*q),
            _ => None,
        })
        .expect("expected Tritanium (34) output on MachoNet reply");
    assert!(trit_wire_qty > 0);

    // 2. Stale Upwell structure output hangar -> current NPC station Item hangar.
    let prev_struct_out = client
        .call_wire(
            "reprocessingSvc",
            None,
            "Reprocess",
            &[
                PyRep::List(vec![PyRep::Integer(ore_prev_struct_id)]),
                PyRep::Integer(JITA_STATION_ID),
                PyRep::None,
                PyRep::Integer(prev_struct_id),
                PyRep::Integer(InventoryFlag::Hangar as i64),
            ],
            &kw,
        )
        .await;
    assert!(prev_struct_out.marshal_errors.is_empty());
    let prev_struct_rep = prev_struct_out
        .rpc_result
        .expect("stale Upwell structure Item hangar output location must resolve to current station");
    assert!(matches!(prev_struct_rep, PyRep::Tuple(_)));

    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&ore_npc_id));
        assert!(!snap.items.contains_key(&ore_prev_struct_id));
        let trit_total_at_jita: i64 = snap
            .items
            .values()
            .filter(|i| {
                i.type_id == 34
                    && i.owner_id == char_id
                    && i.location_id == JITA_STATION_ID
                    && i.flag == InventoryFlag::Hangar
            })
            .map(|i| i64::from(i.quantity))
            .sum();
        assert_eq!(trit_total_at_jita, trit_wire_qty * 2);
        assert!(
            !snap.items.values().any(|i| i.type_id == 34 && i.location_id == PERIMETER_STATION_ID),
            "reprocessed minerals must never land in the stale station"
        );
        assert!(
            !snap.items.values().any(|i| i.type_id == 34 && i.location_id == prev_struct_id),
            "reprocessed minerals must never land in the stale structure"
        );
    });

    // 3. Stale NPC station output hangar -> current docked online Upwell structure.
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.structure_id = Some(cur_struct_id);
        }
    });
    ctx.sync_session(&mut client.session);
    assert_eq!(client.session.structure_id, Some(cur_struct_id));
    let cur_struct_out = client
        .call_wire(
            "reprocessingSvc",
            None,
            "Reprocess",
            &[
                PyRep::List(vec![PyRep::Integer(ore_cur_struct_id)]),
                PyRep::Integer(cur_struct_id),
                PyRep::None,
                PyRep::Integer(PERIMETER_STATION_ID),
                PyRep::Integer(InventoryFlag::Hangar as i64),
            ],
            &kw,
        )
        .await;
    assert!(cur_struct_out.marshal_errors.is_empty());
    let cur_struct_rep = cur_struct_out
        .rpc_result
        .expect("stale NPC station Item hangar must resolve to current docked structure hangar");
    assert!(matches!(cur_struct_rep, PyRep::Tuple(_)));
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&ore_cur_struct_id));
        assert!(snap.items.values().any(|i| {
            i.type_id == 34
                && i.owner_id == char_id
                && i.location_id == cur_struct_id
                && i.flag == InventoryFlag::Hangar
                && i.quantity > 0
        }));
    });

    // Restore Jita character docking for guard assertions.
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = Some(JITA_STATION_ID);
            ch.structure_id = None;
        }
    });
    ctx.sync_session(&mut client.session);
    assert_eq!(client.session.station_id, Some(JITA_STATION_ID));

    // 4. Security guards must still reject non-existent stations, mismatched owners, unrented offices, and non-hangar flags.
    let guard_cases: Vec<(PyRep, i64, InventoryFlag, &str)> = vec![
        (PyRep::None, 60_999_999, InventoryFlag::Hangar, "InvalidOutputLocation"),
        (PyRep::Integer(corp_id), PERIMETER_STATION_ID, InventoryFlag::Hangar, "InvalidOutputLocation"),
        (PyRep::Integer(other_char_id), PERIMETER_STATION_ID, InventoryFlag::Hangar, "OwnershipOrLocationViolation"),
        (PyRep::None, PERIMETER_STATION_ID, InventoryFlag::CorpSAG1, "InvalidOutputLocation"),
        (PyRep::None, PERIMETER_STATION_ID, InventoryFlag::Cargo, "InvalidOutputLocation"),
        (PyRep::None, PERIMETER_STATION_ID, InventoryFlag::HiSlot0, "InvalidOutputLocation"),
        (PyRep::None, stale_office_id, InventoryFlag::CorpSAG1, "InvalidOutputLocation"),
    ];
    for (owner_rep, out_loc, out_flag, expected_code) in guard_cases {
        let err = client
            .call_wire(
                "reprocessingSvc",
                None,
                "Reprocess",
                &[
                    PyRep::List(vec![PyRep::Integer(ore_guard_id)]),
                    PyRep::Integer(JITA_STATION_ID),
                    owner_rep.clone(),
                    PyRep::Integer(out_loc),
                    PyRep::Integer(out_flag as i64),
                ],
                &kw,
            )
            .await
            .rpc_result
            .expect_err("negative security output-location guard must still reject");
        assert_eq!(
            err.code, expected_code,
            "unexpected error for ({owner_rep:?}, {out_loc}, {out_flag:?}): {err:?}"
        );
    }
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items.get(&ore_guard_id).map(|i| i.quantity), Some(200));
    });
}
