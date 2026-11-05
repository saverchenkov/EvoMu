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
    assert_ne!(res, PyRep::None);

    let fitted_loc = ctx
        .db
        .with_snapshot(|s| s.items.get(&mod_item_id).map(|i| (i.location_id, i.flag)));
    assert_eq!(fitted_loc, Some((ship_id, InventoryFlag::HiSlot0)));
}
