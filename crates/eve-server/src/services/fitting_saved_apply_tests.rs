//! Unit tests for `fitting_saved_apply` DNA encoding/decoding, hold capacity/type helpers, and `FitShips` (`handle_fit_ships`).

use super::*;
use eve_db::{InventoryFlag, InventoryItemRecord, SavedFittingRecord, TrainedSkillRecord, WorldRepository};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::{InvType, SdeRegistry};
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

#[test]
fn dna_roundtrip_and_hold_capacity_and_type_checks() {
    let dna = fitting_to_dna(587, &[(484, 2), (178, 50), (484, 1), (999, 0)]);
    assert_eq!(dna, "587:484;3:178;50::");

    let saved = SavedFittingRecord {
        fitting_id: 10,
        owner_id: 90_000_001,
        ship_type_id: 587,
        name: "Rifter Fit".into(),
        description: "Test".into(),
        items: vec![
            (484, InventoryFlag::HiSlot0, 1),
            (484, InventoryFlag::HiSlot1, 1),
            (178, InventoryFlag::Cargo, 50),
        ],
    };
    let saved_dna = saved_fitting_to_dna(&saved);
    assert_eq!(saved_dna, "587:484;2:178;50::");

    let (parsed_ship, parsed_items) = dna_to_fitting(&saved_dna).expect("valid DNA");
    assert_eq!(parsed_ship, 587);
    assert_eq!(parsed_items, vec![(484, 2), (178, 50)]);

    assert!(dna_to_fitting("").is_err());
    assert!(dna_to_fitting("not_a_number:484;1::").is_err());

    let sde = shared_sde();
    let mut ship_type: InvType = sde.types.get(&587).expect("seed type").clone();
    ship_type.capacity = 140.0;
    ship_type.default_attributes = HashMap::from([
        (eve_sde::attr_ids::DRONE_CAPACITY, 25.0),
        (2055, 1000.0),
        (1549, 500.0),
        (1556, 5000.0),
        (3020, 1.0),
    ]);
    assert_eq!(get_ship_hold_capacity(5, &ship_type, Some(180.0)), 180.0);
    assert_eq!(get_ship_hold_capacity(5, &ship_type, None), 140.0);
    assert_eq!(get_ship_hold_capacity(87, &ship_type, None), 25.0);
    assert_eq!(get_ship_hold_capacity(158, &ship_type, None), 1000.0);
    assert_eq!(get_ship_hold_capacity(133, &ship_type, None), 500.0);
    assert_eq!(get_ship_hold_capacity(134, &ship_type, None), 5000.0);

    let mut drone_type = ship_type.clone();
    drone_type.category_id = 18;
    assert!(is_item_allowed_in_ship_hold(87, &drone_type, &ship_type));
    assert!(!is_item_allowed_in_ship_hold(87, &ship_type, &ship_type));

    // Frigate escape bay allows frigates (group 25) when ship has attr 3020 > 0
    assert!(is_item_allowed_in_ship_hold(177, &ship_type, &ship_type));
    ship_type.default_attributes.remove(&3020);
    assert!(!is_item_allowed_in_ship_hold(177, &ship_type, &ship_type));
}

#[tokio::test]
async fn handle_fit_ships_applies_saved_fitting_and_reports_missing_items() {
    let ctx = ServerContext::with_sde_and_repository(
        shared_sde(),
        Arc::new(WorldRepository::with_demo_seed()),
    );
    let char_id = 96_000_001i64;
    let station_id = 60_003_760i64;
    let ship_id = 96_000_100i64;
    let mod_item_id = 96_000_200i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().expect("seed char");
        ch.character_id = char_id;
        ch.user_id = 1;
        ch.name = "SavedFitPilot".into();
        ch.corporation_id = 1_000_009;
        ch.station_id = Some(station_id);
        ch.solar_system_id = 30_000_142;
        ch.active_ship_id = ship_id;
        snap.characters.insert(char_id, ch);

        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587, // Rifter
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            mod_item_id,
            InventoryItemRecord {
                item_id: mod_item_id,
                type_id: 484, // 125mm Gatling AutoCannon I (high slot)
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        for &sk in &[3300u32, 3318, 3392, 3426] {
            snap.skills.entry(char_id).or_default().insert(
                sk,
                TrainedSkillRecord {
                    type_id: sk,
                    level: 5,
                    skill_points: 256_000.0,
                },
            );
        }
        snap.saved_fittings.insert(
            501,
            SavedFittingRecord {
                fitting_id: 501,
                owner_id: char_id,
                ship_type_id: 587,
                name: "Rifter Fit".into(),
                description: "Saved fit".into(),
                items: vec![
                    (484, InventoryFlag::HiSlot0, 1),
                    (178, InventoryFlag::Cargo, 50), // Nuclear S missing from hangar
                ],
            },
        );
    });

    let mut session = SessionState::default();
    session.user_id = Some(1);
    session.character_id = Some(char_id);
    session.corporation_id = Some(1_000_009);
    session.station_id = Some(station_id);
    session.solar_system_id = Some(30_000_142);
    session.ship_id = Some(ship_id);

    let res = handle_fit_ships(
        &ctx,
        &mut session,
        &[
            PyRep::List(vec![PyRep::Integer(ship_id)]),
            PyRep::Integer(501),
        ],
        &HashMap::new(),
    )
    .await
    .expect("handled")
    .expect("FitShips ok");
    assert_eq!(
        res,
        PyRep::Tuple(vec![
            PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::Integer(178),
                PyRep::Integer(50),
            ])]),
            PyRep::Integer(ship_id),
        ])
    );

    let fitted_loc = ctx
        .db
        .with_snapshot(|s| s.items.get(&mod_item_id).map(|i| (i.location_id, i.flag)));
    assert_eq!(fitted_loc, Some((ship_id, InventoryFlag::HiSlot0)));
}

#[tokio::test]
async fn handle_fit_ships_complete_and_partial_multifit_tuple_contracts() {
    let ctx = ServerContext::with_sde_and_repository(
        shared_sde(),
        Arc::new(WorldRepository::with_demo_seed()),
    );
    let char_id = 96_000_002i64;
    let station_id = 60_003_760i64;
    let ship_a = 96_000_110i64;
    let ship_b = 96_000_111i64;
    let single_ship = 96_000_112i64;
    let gun_a = 96_000_210i64;
    let gun_b = 96_000_211i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().expect("seed char");
        ch.character_id = char_id;
        ch.user_id = 1;
        ch.name = "MultiFitPilot".into();
        ch.corporation_id = 1_000_009;
        ch.station_id = Some(station_id);
        ch.solar_system_id = 30_000_142;
        ch.active_ship_id = single_ship;
        snap.characters.insert(char_id, ch);

        for (sid, singleton) in [(single_ship, true), (ship_a, false), (ship_b, false)] {
            snap.items.insert(
                sid,
                InventoryItemRecord {
                    item_id: sid,
                    type_id: 587, // Rifter
                    owner_id: char_id,
                    location_id: station_id,
                    flag: InventoryFlag::Hangar,
                    quantity: 1,
                    singleton,
                    version: 1,
                    ..Default::default()
                },
            );
        }
        // Seed two guns: one consumed by complete single_ship fit, one left for ship_a (leaving ship_b short).
        for gid in [gun_a, gun_b] {
            snap.items.insert(
                gid,
                InventoryItemRecord {
                    item_id: gid,
                    type_id: 484,
                    owner_id: char_id,
                    location_id: station_id,
                    flag: InventoryFlag::Hangar,
                    quantity: 1,
                    singleton: false,
                    version: 1,
                    ..Default::default()
                },
            );
        }
        for &sk in &[3300u32, 3318, 3392, 3426] {
            snap.skills.entry(char_id).or_default().insert(
                sk,
                TrainedSkillRecord {
                    type_id: sk,
                    level: 5,
                    skill_points: 256_000.0,
                },
            );
        }
        snap.saved_fittings.insert(
            502,
            SavedFittingRecord {
                fitting_id: 502,
                owner_id: char_id,
                ship_type_id: 587,
                name: "Single Gun Rifter".into(),
                description: "Single high-slot fit".into(),
                items: vec![(484, InventoryFlag::HiSlot0, 1)],
            },
        );
    });

    let mut session = SessionState::default();
    session.user_id = Some(1);
    session.character_id = Some(char_id);
    session.corporation_id = Some(1_000_009);
    session.station_id = Some(station_id);
    session.solar_system_id = Some(30_000_142);
    session.ship_id = Some(single_ship);

    // 1. Complete fit returns ([], None)
    let clean_res = handle_fit_ships(
        &ctx,
        &mut session,
        &[PyRep::List(vec![PyRep::Integer(single_ship)]), PyRep::Integer(502)],
        &HashMap::new(),
    )
    .await
    .expect("handled")
    .expect("clean FitShips ok");
    assert_eq!(clean_res, PyRep::Tuple(vec![PyRep::List(vec![]), PyRep::None]));

    // 2. Partial multifit: ship_a succeeds cleanly with remaining gun_b; ship_b fails -> ([(484, 1)], ship_b)
    let partial_res = handle_fit_ships(
        &ctx,
        &mut session,
        &[
            PyRep::List(vec![PyRep::Integer(ship_a), PyRep::Integer(ship_b)]),
            PyRep::Integer(502),
        ],
        &HashMap::new(),
    )
    .await
    .expect("handled")
    .expect("partial multifit returns 2-tuple");
    assert_eq!(
        partial_res,
        PyRep::Tuple(vec![
            PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::Integer(484),
                PyRep::Integer(1),
            ])]),
            PyRep::Integer(ship_b),
        ])
    );
    ctx.db.with_snapshot(|s| {
        assert_eq!(
            s.items
                .values()
                .filter(|i| i.location_id == single_ship && i.type_id == 484 && i.flag == InventoryFlag::HiSlot0)
                .count(),
            1
        );
        assert_eq!(
            s.items
                .values()
                .filter(|i| i.location_id == ship_a && i.type_id == 484 && i.flag == InventoryFlag::HiSlot0)
                .count(),
            1
        );
        assert!(!s.items.values().any(|i| i.location_id == ship_b && i.type_id == 484));
    });
}

#[tokio::test]
async fn handle_fit_ships_rejected_whole_stack_transfer_reports_missing_item_and_failed_ship() {
    let ctx = ServerContext::with_sde_and_repository(
        shared_sde(),
        Arc::new(WorldRepository::with_demo_seed()),
    );
    let char_id = 96_000_003i64;
    let struct_id = 1_000_000_000_901i64;
    let ship_id = 96_000_301i64;
    let gun_item_id = 96_000_302i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            struct_id,
            eve_db::UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 35_832, // Astrahus
                owner_corp_id: 98_000_999,
                solar_system_id: 30_000_142,
                name: "Revoked ACL Astrahus".into(),
                position: [10_000.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                ..Default::default()
            },
        );
        let mut ch = snap.characters.values().next().cloned().expect("seed char");
        ch.character_id = char_id;
        ch.user_id = 3;
        ch.name = "AclRevokedFitPilot".into();
        ch.corporation_id = 1_000_009;
        ch.station_id = None;
        ch.structure_id = Some(struct_id);
        ch.solar_system_id = 30_000_142;
        ch.active_ship_id = ship_id;
        snap.characters.insert(char_id, ch);

        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587, // Rifter
                owner_id: char_id,
                location_id: struct_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            gun_item_id,
            InventoryItemRecord {
                item_id: gun_item_id,
                type_id: 484, // 125mm Gatling AutoCannon I (1-unit stack -> whole-stack transfer)
                owner_id: char_id,
                location_id: struct_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        for &sk in &[3300u32, 3318, 3392, 3426] {
            snap.skills.entry(char_id).or_default().insert(
                sk,
                TrainedSkillRecord {
                    type_id: sk,
                    level: 5,
                    skill_points: 256_000.0,
                },
            );
        }
        snap.saved_fittings.insert(
            503,
            SavedFittingRecord {
                fitting_id: 503,
                owner_id: char_id,
                ship_type_id: 587,
                name: "ACL Rejected Fit".into(),
                description: "Single high-slot fit".into(),
                items: vec![(484, InventoryFlag::HiSlot0, 1)],
            },
        );
        snap.structure_docking_acl_denied
            .entry(struct_id)
            .or_default()
            .insert(char_id);
    });

    let mut session = SessionState::default();
    session.user_id = Some(3);
    session.character_id = Some(char_id);
    session.corporation_id = Some(1_000_009);
    session.station_id = None;
    session.structure_id = Some(struct_id);
    session.solar_system_id = Some(30_000_142);
    session.ship_id = Some(ship_id);

    let res = handle_fit_ships(
        &ctx,
        &mut session,
        &[PyRep::List(vec![PyRep::Integer(ship_id)]), PyRep::Integer(503)],
        &HashMap::new(),
    )
    .await
    .expect("handled")
    .expect("rejected whole-stack transfer still returns 2-tuple");
    assert_eq!(
        res,
        PyRep::Tuple(vec![
            PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::Integer(484),
                PyRep::Integer(1),
            ])]),
            PyRep::Integer(ship_id),
        ])
    );

    ctx.db.with_snapshot(|s| {
        assert_eq!(
            s.items.get(&gun_item_id).map(|i| (i.location_id, i.flag)),
            Some((struct_id, InventoryFlag::Hangar))
        );
        assert!(!s.items.values().any(|i| i.location_id == ship_id && i.type_id == 484));
    });
}

#[tokio::test]
async fn handle_fit_ships_partial_multistack_emits_notifications_and_reports_remaining_shortfall() {
    let ctx = ServerContext::with_sde_and_repository(
        shared_sde(),
        Arc::new(WorldRepository::with_demo_seed()),
    );
    let char_id = 96_000_004i64;
    let station_id = 60_003_760i64;
    let charon_id = 96_000_401i64;
    let good_rifter_1 = 96_000_402i64;
    let good_rifter_2 = 96_000_403i64;
    let boarded_rifter_id = 96_000_404i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().expect("seed char");
        ch.character_id = char_id;
        ch.user_id = 4;
        ch.name = "PartialMultiStackPilot".into();
        ch.corporation_id = 1_000_009;
        ch.station_id = Some(station_id);
        ch.structure_id = None;
        ch.solar_system_id = 30_000_142;
        ch.active_ship_id = boarded_rifter_id;
        snap.characters.insert(char_id, ch);
        snap.accounts.insert(
            format!("user_{char_id}"),
            eve_db::AccountRecord {
                user_id: 4,
                username: format!("user_{char_id}"),
                password_token: String::new(),
                role: 0,
                banned_until_unix: None,
                ban_reason: None,
            },
        );

        for &(item_id, type_id, singleton) in &[
            (charon_id, 20_185u32, true),       // Charon (Cargo capacity 465,000 m3)
            (good_rifter_1, 587u32, false),     // Transferable packaged Rifter #1
            (boarded_rifter_id, 587u32, true),  // Currently boarded Rifter (rejected by transfer_inventory_item)
        ] {
            snap.items.insert(
                item_id,
                InventoryItemRecord {
                    item_id,
                    type_id,
                    owner_id: char_id,
                    location_id: station_id,
                    flag: InventoryFlag::Hangar,
                    quantity: 1,
                    singleton,
                    version: 1,
                    ..Default::default()
                },
            );
        }
        snap.saved_fittings.insert(
            504,
            SavedFittingRecord {
                fitting_id: 504,
                owner_id: char_id,
                ship_type_id: 20_185,
                name: "Charon With 2 Rifters".into(),
                description: "Partial multi-stack cargo test".into(),
                items: vec![(587, InventoryFlag::Cargo, 2)],
            },
        );
    });

    let mut session = SessionState::default();
    session.user_id = Some(4);
    session.character_id = Some(char_id);
    session.corporation_id = Some(1_000_009);
    session.station_id = Some(station_id);
    session.solar_system_id = Some(30_000_142);
    session.ship_id = Some(boarded_rifter_id);
    ctx.db
        .acquire_online_lock(4, 4, Some(char_id), true, ctx.now_unix())
        .expect("register online session for OnItemChange/OnMultipleItemChange delivery");
    let _ = ctx.drain_notifications_for_char(char_id);

    // 1. handle_fit_ships fits 1 transferable Rifter and skips the boarded Rifter:
    //    returns ([(587, 1)], charon_id) AND emits OnMultipleItemChange for the moved stack.
    let res = handle_fit_ships(
        &ctx,
        &mut session,
        &[PyRep::List(vec![PyRep::Integer(charon_id)]), PyRep::Integer(504)],
        &HashMap::new(),
    )
    .await
    .expect("handled")
    .expect("partial multi-stack FitShips ok");
    assert_eq!(
        res,
        PyRep::Tuple(vec![
            PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::Integer(587),
                PyRep::Integer(1),
            ])]),
            PyRep::Integer(charon_id),
        ])
    );

    let notifs = ctx.drain_notifications_for_char(char_id);
    let multi = notifs
        .iter()
        .find(|n| n.event == "OnMultipleItemChange")
        .expect("OnMultipleItemChange must be emitted when partial multi-stack moves >= 1 item");
    let PyRep::List(ref moved_rows) = multi.args[0] else {
        panic!("expected OnMultipleItemChange items list");
    };
    assert_eq!(moved_rows.len(), 1);
    assert_eq!(
        moved_rows[0].dict_get("itemID").and_then(|v| v.as_i64()),
        Some(good_rifter_1)
    );
    assert_eq!(
        moved_rows[0].dict_get("locationID").and_then(|v| v.as_i64()),
        Some(charon_id)
    );
    assert_eq!(
        moved_rows[0].dict_get("flagID").and_then(|v| v.as_i64()),
        Some(InventoryFlag::Cargo as i64)
    );

    ctx.db.with_snapshot(|s| {
        assert_eq!(
            s.items.get(&good_rifter_1).map(|i| (i.location_id, i.flag)),
            Some((charon_id, InventoryFlag::Cargo))
        );
        assert_eq!(
            s.items.get(&boarded_rifter_id).map(|i| (i.location_id, i.flag)),
            Some((station_id, InventoryFlag::Hangar))
        );
    });

    // 2. Direct validate_and_apply_fitting_with_failed_ship (emit_single_item_change = true)
    //    verifies exact (fitted_count == 1, moved_items.len() == 1, missing[587] == 1, failed_ship == Some(charon_id))
    //    and OnItemChange emission on partial multi-stack consumption.
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            good_rifter_2,
            InventoryItemRecord {
                item_id: good_rifter_2,
                type_id: 587,
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });
    let (fitted_count, missing, moved_items, failed_ship_id) =
        validate_and_apply_fitting_with_failed_ship(
            &ctx,
            char_id,
            station_id,
            &[charon_id],
            Some(20_185),
            &[(587, InventoryFlag::Cargo, 2)],
            true,
        )
        .expect("direct validate_and_apply_fitting_with_failed_ship ok");
    assert_eq!(fitted_count, 1);
    assert_eq!(missing.get(&587).copied(), Some(1));
    assert_eq!(moved_items.len(), 1);
    assert_eq!(moved_items[0].item_id, good_rifter_2);
    assert_eq!(failed_ship_id, Some(charon_id));

    let single_notifs = ctx.drain_notifications_for_char(char_id);
    let item_notif = single_notifs
        .iter()
        .find(|n| n.event == "OnItemChange")
        .expect("OnItemChange must be emitted when emit_single_item_change=true");
    assert_eq!(
        item_notif.args[0].dict_get("itemID").and_then(|v| v.as_i64()),
        Some(good_rifter_2)
    );
}
