//! Edge-case behavioral tests for `fitting_containers_ships_ops`.

use super::*;
use crate::router::BoundObjectEntry;
use crate::services::fitting_containers_ships::write_container_ship_state_snap;
use crate::system_actor::ActiveModuleCycle;
use eve_db::{
    corp_roles, CustomsOfficeRecord, InventoryFlag, InventoryItemRecord, IskCents,
    PlanetaryColonyLayoutRecord, PlanetaryPinContentRecord, PlanetaryPinRecord,
    SavedFittingRecord, WorldRepository,
};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::{InvType, SdeRegistry};
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn make_sde_with_types(extra: Vec<InvType>) -> Arc<SdeRegistry> {
    let mut sde = (*shared_sde()).clone();
    for t in extra {
        sde.types.insert(t.type_id, t);
    }
    Arc::new(sde)
}

fn test_inv_type(
    type_id: u32,
    group_id: u32,
    category_id: u32,
    name: &str,
    volume: f64,
    attrs: &[(u16, f64)],
) -> InvType {
    let mut t = shared_sde()
        .types
        .get(&587)
        .expect("canonical Rifter type")
        .clone();
    t.type_id = type_id;
    t.group_id = group_id;
    t.category_id = category_id;
    t.name = name.to_string();
    t.volume = volume;
    t.capacity = 1000.0;
    t.default_attributes.clear();
    for &(k, v) in attrs {
        t.default_attributes.insert(k, v);
    }
    t
}

fn make_ctx_and_session(char_id: i64, ship_id: i64, sde: Arc<SdeRegistry>) -> (ServerContext, SessionState) {
    let ctx = ServerContext::with_sde_and_repository(sde, Arc::new(WorldRepository::with_demo_seed()));
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = 98_000_001;
            ch.station_id = Some(60_003_760);
            ch.solar_system_id = 30_000_142;
            ch.balance = IskCents::from_isk(100_000_000);
        }
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: char_id,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::from([(38u16, 100.0)]),
                ..Default::default()
            },
        );
    });
    let mut sess = SessionState::new(97_300);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(98_000_001);
    sess.station_id = Some(60_003_760);
    sess.solar_system_id = Some(30_000_142);
    sess.ship_id = Some(ship_id);
    (ctx, sess)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn clear_charges_rejects_invalid_target_active_module_cycle_and_capacity_exceeded() {
    let char_id = 90_000_001i64;
    let ship_id = 97_300_100i64;
    let charge_type = test_inv_type(950_001, 83, 8, "Test Ammo", 10.0, &[]);
    let mod_type = test_inv_type(950_002, 53, 7, "Test Blaster", 5.0, &[]);
    let sde = make_sde_with_types(vec![charge_type, mod_type]);
    let (ctx, sess) = make_ctx_and_session(char_id, ship_id, sde);

    // Invalid ship_id <= 0 without fallback session ship
    let mut no_ship_sess = sess.clone();
    no_ship_sess.ship_id = None;
    let inv_err = handle_clear_charges(&ctx, &no_ship_sess, None, &[PyRep::Integer(0)])
        .await
        .unwrap_err();
    assert_eq!(inv_err.code, "InvalidFittingTarget");

    // Non-ship item -> InvalidFittingTarget
    let non_ship_id = 97_300_101i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(corp) = snap.corporations.get_mut(&98_000_001) {
            corp.ceo_id = 90_000_099;
            corp.member_roles.remove(&char_id);
        }
        snap.items.insert(
            non_ship_id,
            InventoryItemRecord {
                item_id: non_ship_id,
                type_id: 950_002,
                owner_id: char_id,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });
    let wrong_target = handle_clear_charges(&ctx, &sess, None, &[PyRep::Integer(non_ship_id)])
        .await
        .unwrap_err();
    assert_eq!(wrong_target.code, "InvalidFittingTarget");

    // Active module in snap.module_active_states -> ModuleIsActive
    let mod_id = 97_300_102i64;
    let charge_id = 97_300_103i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(corp) = snap.corporations.get_mut(&98_000_001) {
            corp.ceo_id = 90_000_099;
            corp.member_roles.remove(&char_id);
        }
        snap.items.insert(
            mod_id,
            InventoryItemRecord {
                item_id: mod_id,
                type_id: 950_002,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            charge_id,
            InventoryItemRecord {
                item_id: charge_id,
                type_id: 950_001,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 20, // 20 * 10m3 = 200m3 > cargo capacity 100m3
                singleton: false,
                ..Default::default()
            },
        );
        snap.module_active_states.insert(mod_id);
    });

    let active_err = handle_clear_charges(&ctx, &sess, None, &[PyRep::Integer(ship_id)])
        .await
        .unwrap_err();
    assert_eq!(active_err.code, "ModuleIsActive");

    // Clear active state -> hits CapacityExceeded (200m3 > 100m3)
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_active_states.remove(&mod_id);
    });
    let cap_err = handle_clear_charges(&ctx, &sess, None, &[PyRep::Integer(ship_id)])
        .await
        .unwrap_err();
    assert_eq!(cap_err.code, "CapacityExceeded");

    // Shrink charge volume so capacity check passes, then active module cycle triggers ModuleIsActive
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&charge_id).unwrap().quantity = 5;
    });
    let sys = ctx.systems.get_or_spawn(30_000_142);
    sys.add_module_cycle(ActiveModuleCycle {
        ship_id,
        module_id: mod_id,
        module_type_id: 950_002,
        owner_char_id: Some(char_id),
        duration_s: 5.0,
        ..Default::default()
    })
    .await
    .unwrap();
    let cycle_err = handle_clear_charges(&ctx, &sess, None, &[PyRep::Integer(ship_id)])
        .await
        .unwrap_err();
    assert_eq!(cycle_err.code, "ModuleIsActive");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fit_fitting_validates_docked_permissions_flags_slots_and_reports_missing_sorted() {
    let char_id = 90_000_001i64;
    let ship_id = 97_300_200i64;
    let mut rifter = test_inv_type(587, 25, 6, "Rifter", 28_000.0, &[(14, 1.0), (13, 1.0), (12, 1.0), (283, 50.0)]);
    rifter.capacity = 200.0;
    let hi_mod = test_inv_type(950_101, 53, 7, "High Gun", 5.0, &[]);
    let drone_t = test_inv_type(950_102, 100, 18, "Hobgoblin", 5.0, &[]);
    let charge_t = test_inv_type(950_103, 83, 8, "Charge", 0.01, &[]);
    let sde = make_sde_with_types(vec![rifter, hi_mod, drone_t, charge_t]);
    let (ctx, mut sess) = make_ctx_and_session(char_id, ship_id, sde);
    let kw = HashMap::new();

    // NotDocked error
    sess.station_id = None;
    let undocked = handle_ext_tail(&ctx, &mut sess, None, "invbroker", "FitFitting", &[], &kw)
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(undocked.code, "NotDocked");
    sess.station_id = Some(60_003_760);

    // FittingID not found (scalar fittingID arg)
    let missing_fit = handle_ext_tail(
        &ctx,
        &mut sess,
        None,
        "invbroker",
        "FitFitting",
        &[PyRep::Integer(ship_id), PyRep::Integer(99_999)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(missing_fit.code, "FittingNotFound");

    // Unauthorized owner on saved fitting
    ctx.db.with_mut_snapshot(|snap| {
        snap.saved_fittings.insert(
            77_001,
            SavedFittingRecord {
                fitting_id: 77_001,
                owner_id: 90_999_999,
                name: "Foreign".into(),
                description: String::new(),
                ship_type_id: 587,
                items: vec![],
            },
        );
    });
    let denied_fit = handle_ext_tail(
        &ctx,
        &mut sess,
        None,
        "invbroker",
        "FitFitting",
        &[PyRep::Integer(ship_id), PyRep::Integer(77_001)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(denied_fit.code, "AccessDenied");

    // Invalid flag 999 -> InvalidFittingFlag
    let bad_flag_fit = PyRep::List(vec![PyRep::Tuple(vec![
        PyRep::Integer(950_101),
        PyRep::Integer(999),
        PyRep::Integer(1),
    ])]);
    let bad_flag_err = handle_ext_tail(
        &ctx,
        &mut sess,
        None,
        "invbroker",
        "FitFitting",
        &[PyRep::Integer(ship_id), bad_flag_fit],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(bad_flag_err.code, "InvalidFittingFlag");

    // Invalid qty 0 -> InvalidQuantity
    let bad_qty_fit = PyRep::List(vec![PyRep::Tuple(vec![
        PyRep::Integer(950_101),
        PyRep::Integer(27),
        PyRep::Integer(0),
    ])]);
    let bad_qty_err = handle_ext_tail(
        &ctx,
        &mut sess,
        None,
        "invbroker",
        "FitFitting",
        &[PyRep::Integer(ship_id), bad_qty_fit],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(bad_qty_err.code, "InvalidQuantity");

    // SlotIndexExceedsHullSlots: hull has 1 hiSlot, requesting HiSlot1 (flag 28) fails
    let slot_overflow_fit = PyRep::List(vec![PyRep::Tuple(vec![
        PyRep::Integer(950_101),
        PyRep::Integer(28),
        PyRep::Integer(1),
    ])]);
    let overflow_err = handle_ext_tail(
        &ctx,
        &mut sess,
        None,
        "invbroker",
        "FitFitting",
        &[PyRep::Integer(ship_id), slot_overflow_fit],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(overflow_err.code, "UserError");
    assert_eq!(overflow_err.message, "SlotIndexExceedsHullSlots");

    // Valid fitting using 2-tuples + drone + charge
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(corp) = snap.corporations.get_mut(&98_000_001) {
            corp.ceo_id = 90_000_099;
            corp.member_roles.remove(&char_id);
        }
        snap.items.insert(
            97_300_201,
            InventoryItemRecord {
                item_id: 97_300_201,
                type_id: 950_102,
                owner_id: char_id,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: false,
                ..Default::default()
            },
        );
    });
    let partial_fit = PyRep::List(vec![
        PyRep::Tuple(vec![PyRep::Integer(950_103), PyRep::Integer(5)]),
        PyRep::Tuple(vec![PyRep::Integer(950_102), PyRep::Integer(3)]),
    ]);
    let res = handle_ext_tail(
        &ctx,
        &mut sess,
        None,
        "invbroker",
        "FitFitting",
        &[PyRep::Integer(ship_id), partial_fit],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        res,
        PyRep::List(vec![
            PyRep::Tuple(vec![PyRep::Integer(950_102), PyRep::Integer(2)]),
            PyRep::Tuple(vec![PyRep::Integer(950_103), PyRep::Integer(5)]),
        ])
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn import_export_with_planet_covers_validations_capacity_tax_and_roundtrip() {
    let char_id = 90_000_001i64;
    let ship_id = 97_300_300i64;
    let co_id = 40_000_100i64;
    let planet_id = 40_000_005i64;
    let spaceport_pin_id = 5_555_001i64;
    let co_type = test_inv_type(2233, 1025, 40, "Customs Office", 100.0, &[]);
    let lp_type = test_inv_type(2256, 1030, 41, "Barren Launchpad", 100.0, &[]);
    let comm_a = test_inv_type(950_201, 1035, 42, "Bacteria", 1.0, &[]);
    let comm_b = test_inv_type(950_202, 1035, 42, "Biofuels", 2.0, &[]);
    let big_comm = test_inv_type(950_203, 1035, 42, "Heavy Commodity", 6_000.0, &[]);
    let sde = make_sde_with_types(vec![co_type, lp_type, comm_a, comm_b, big_comm]);
    let (ctx, mut sess) = make_ctx_and_session(char_id, ship_id, sde);
    sess.station_id = None;
    let kw = HashMap::new();

    assert_eq!(
        parse_pi_xfer_entries(Some(&PyRep::Integer(1)), "import").unwrap_err().code,
        "InvalidTransferArguments"
    );
    assert_eq!(
        parse_pi_xfer_entries(Some(&PyRep::Dict(vec![(PyRep::Integer(0), PyRep::Integer(1))])), "import")
            .unwrap_err()
            .code,
        "InvalidCommodity"
    );
    assert_eq!(
        parse_pi_xfer_entries(Some(&PyRep::Dict(vec![(PyRep::Integer(10), PyRep::Integer(0))])), "import")
            .unwrap_err()
            .code,
        "InvalidCommodityQuantity"
    );

    let bound_co = BoundObjectEntry {
        bound_id: 1,
        session_id: sess.session_id,
        user_id: 1,
        character_id: char_id,
        service: "invbroker".into(),
        object_id: co_id,
        oid_str: co_id.to_string(),
        ref_id: 1,
    };

    // Empty import & export -> InvalidTransferArguments
    let empty_err = handle_import_export_with_planet(
        &ctx,
        &sess,
        Some(&bound_co),
        &[
            PyRep::Integer(spaceport_pin_id),
            PyRep::Dict(vec![]),
            PyRep::Dict(vec![]),
            PyRep::Real(0.0),
        ],
        &kw,
    )
    .unwrap_err();
    assert_eq!(empty_err.code, "InvalidTransferArguments");

    let imp_item_id = 97_300_301i64;
    let exp_existing_co_id = 97_300_302i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(corp) = snap.corporations.get_mut(&98_000_001) {
            corp.ceo_id = 90_000_099;
            corp.member_roles.remove(&char_id);
        }
        snap.items.insert(
            co_id,
            InventoryItemRecord {
                item_id: co_id,
                type_id: 2233,
                owner_id: 98_000_002,
                location_id: 30_000_142,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.structures.insert(
            co_id,
            eve_db::types::UpwellStructureRecord {
                structure_id: co_id,
                type_id: 2233,
                owner_corp_id: 98_000_002,
                solar_system_id: 30_000_142,
                name: format!("Orbital:{planet_id}"),
                position: [0.0; 3],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: false,
                fuel_blocks: 0,
                shield_hp: 100.0,
                armor_hp: 100.0,
                hull_hp: 100.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
        snap.corp_customs_offices.insert(
            98_000_002,
            vec![CustomsOfficeRecord {
                office_id: co_id,
                system_id: 30_000_142,
                reinforce_exit_start: 0,
                reinforce_exit_end: 1,
                allow_alliance_access: true,
                allow_access_with_standings: true,
                standing_level: None,
                corporation_tax_rate: Some(0.10),
                alliance_tax_rate: None,
                excellent_standing_tax_rate: None,
                good_standing_tax_rate: None,
                neutral_standing_tax_rate: Some(0.10),
                bad_standing_tax_rate: None,
                terrible_standing_tax_rate: None,
            }],
        );
        snap.planetary_colonies.insert(
            (char_id, planet_id),
            eve_db::types::PlanetaryColonyRecord {
                planet_id,
                owner_id: char_id,
                solar_system_id: 30_000_142,
                command_center_level: 1,
                pins_count: 1,
                extracted_commodities: HashMap::new(),
                last_extracted_unix: 0,
            },
        );
        snap.planetary_colony_layouts.insert(
            (char_id, planet_id),
            PlanetaryColonyLayoutRecord {
                pins: vec![PlanetaryPinRecord {
                    pin_id: spaceport_pin_id,
                    type_id: 2256,
                    latitude: 0.0,
                    longitude: 0.0,
                    schematic_id: None,
                    install_time_unix: None,
                    expiry_time_unix: None,
                    last_cycle_start_unix: None,
                    contents: vec![PlanetaryPinContentRecord {
                        type_id: 950_202,
                        amount: 10,
                    }],
                }],
                links: vec![],
                routes: vec![],
            },
        );
        snap.items.insert(
            imp_item_id,
            InventoryItemRecord {
                item_id: imp_item_id,
                type_id: 950_201,
                owner_id: char_id,
                location_id: co_id,
                flag: InventoryFlag::Hangar,
                quantity: 5,
                singleton: false,
                ..Default::default()
            },
        );
        snap.items.insert(
            exp_existing_co_id,
            InventoryItemRecord {
                item_id: exp_existing_co_id,
                type_id: 950_202,
                owner_id: char_id,
                location_id: co_id,
                flag: InventoryFlag::Hangar,
                quantity: 4,
                singleton: false,
                ..Default::default()
            },
        );
        snap.items.insert(
            97_300_303,
            InventoryItemRecord {
                item_id: 97_300_303,
                type_id: 950_203,
                owner_id: char_id,
                location_id: co_id,
                flag: InventoryFlag::Hangar,
                quantity: 2,
                singleton: false,
                ..Default::default()
            },
        );
    });

    // Launchpad capacity exceeded (2 * 6000m3 = 12,000 > 10,000)
    let lp_cap = handle_import_export_with_planet(
        &ctx,
        &sess,
        Some(&bound_co),
        &[
            PyRep::Integer(spaceport_pin_id),
            PyRep::Dict(vec![(PyRep::Integer(97_300_303), PyRep::Integer(2))]),
            PyRep::Dict(vec![]),
            PyRep::Real(0.0),
        ],
        &kw,
    )
    .unwrap_err();
    assert_eq!(lp_cap.code, "LaunchpadCapacityExceeded");

    // Roundtrip: import 3 of comm_a (by type_id) & export 4 of comm_b (merges into exp_existing_co_id)
    let res = handle_ext_tail(
        &ctx,
        &mut sess,
        Some(&bound_co),
        "invbroker",
        "ImportExportWithPlanet",
        &[
            PyRep::Integer(spaceport_pin_id),
            PyRep::Dict(vec![(PyRep::Integer(950_201), PyRep::Integer(3))]),
            PyRep::Dict(vec![(PyRep::Integer(950_202), PyRep::Integer(4))]),
            PyRep::Real(0.10),
        ],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(res, PyRep::None);

    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&imp_item_id].quantity, 2);
        assert_eq!(snap.items[&exp_existing_co_id].quantity, 8);
        let pin = &snap.planetary_colony_layouts[&(char_id, planet_id)].pins[0];
        let a_amt = pin.contents.iter().find(|c| c.type_id == 950_201).unwrap().amount;
        let b_amt = pin.contents.iter().find(|c| c.type_id == 950_202).unwrap().amount;
        assert_eq!(a_amt, 3);
        assert_eq!(b_amt, 6);
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retrieve_password_validates_type_roles_container_group_and_unlocks() {
    let char_id = 90_000_001i64;
    let ship_id = 97_300_400i64;
    let alsc_type = test_inv_type(17366, 448, 2, "Audit Log Secure Container", 100.0, &[]);
    let plain_type = test_inv_type(950_401, 649, 2, "Freight Container", 100.0, &[]);
    let sde = make_sde_with_types(vec![alsc_type, plain_type]);
    let (ctx, mut sess) = make_ctx_and_session(char_id, ship_id, sde);
    let kw = HashMap::new();

    // Invalid password type which = 0
    assert_eq!(
        handle_retrieve_password(&ctx, &sess, None, &[PyRep::Integer(1), PyRep::Integer(0)], &kw)
            .unwrap_err()
            .code,
        "InvalidPasswordType"
    );

    let cont_id = 97_300_410i64;
    let plain_id = 97_300_411i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(corp) = snap.corporations.get_mut(&98_000_001) {
            corp.ceo_id = 90_000_099;
            corp.member_roles.remove(&char_id);
        }
        snap.items.insert(
            cont_id,
            InventoryItemRecord {
                item_id: cont_id,
                type_id: 17366,
                owner_id: 98_000_001, // corp-owned
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            plain_id,
            InventoryItemRecord {
                item_id: plain_id,
                type_id: 950_401,
                owner_id: char_id,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        write_container_ship_state_snap(snap, |st| {
            st.set_container_password(cont_id, 1, "sec1".into());
            st.set_container_password(cont_id, 2, "cfg1".into());
        });
    });

    // Corp-owned container rejected without EquipmentConfig/Director role
    sess.corp_role = 0;
    assert_eq!(
        handle_retrieve_password(&ctx, &sess, None, &[PyRep::Integer(cont_id), PyRep::Integer(1)], &kw)
            .unwrap_err()
            .code,
        "CorpRoleRequired"
    );

    // Non-password container group rejected
    assert_eq!(
        handle_retrieve_password(&ctx, &sess, None, &[PyRep::Integer(plain_id), PyRep::Integer(1)], &kw)
            .unwrap_err()
            .code,
        "ContainerDoesNotSupportPassword"
    );

    // Authorized corp EquipmentConfig unlocks container and logs action
    sess.corp_role = corp_roles::ROLE_EQUIPMENT_CONFIG;
    let ok = handle_retrieve_password(
        &ctx,
        &sess,
        None,
        &[PyRep::Integer(cont_id), PyRep::Integer(1)],
        &kw,
    )
    .unwrap();
    assert_eq!(ok, PyRep::String("sec1".into()));
    ctx.db.with_snapshot(|snap| {
        assert!(snap
            .corp_container_logs
            .get(&98_000_001)
            .is_some_and(|v| v.iter().any(|l| l.container_id == cont_id)));
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn import_by_item_id_multistack_by_type_id_retrieve_pw_or_role_and_clear_charges_ok() {
    let char_id = 90_000_001i64;
    let ship_id = 97_300_500i64;
    let co_id = 40_000_500i64;
    let planet_id = 40_000_505i64;
    let spaceport_pin_id = 5_555_501i64;
    let co_type = test_inv_type(2233, 1025, 40, "Customs Office", 100.0, &[]);
    let lp_type = test_inv_type(2256, 1030, 41, "Barren Launchpad", 100.0, &[]);
    let comm_a = test_inv_type(950_501, 1035, 42, "Bacteria", 1.0, &[]);
    let alsc_type = test_inv_type(17366, 448, 2, "Audit Log Secure Container", 100.0, &[]);
    let charge_t = test_inv_type(950_502, 85, 8, "Charge", 5.0, &[]);
    let mut mod_t = test_inv_type(950_503, 53, 7, "Gun", 5.0, &[]);
    mod_t.default_attributes.insert(137, 1.0);
    let mut ship_t = test_inv_type(587, 25, 6, "Rifter", 1000.0, &[]);
    ship_t.default_attributes.insert(38, 100.0);
    let sde = make_sde_with_types(vec![co_type, lp_type, comm_a, alsc_type, charge_t, mod_t, ship_t]);
    let (ctx, mut sess) = make_ctx_and_session(char_id, ship_id, sde);
    sess.station_id = None;
    let kw = HashMap::new();

    let item_by_id = 97_300_510i64;
    let stack1 = 97_300_511i64;
    let stack2 = 97_300_512i64;
    let stack3 = 97_300_513i64;
    let cont_id = 97_300_520i64;
    let mod_id = 97_300_530i64;
    let charge_id = 97_300_531i64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(corp) = snap.corporations.get_mut(&98_000_001) {
            corp.ceo_id = 90_000_099;
            corp.member_roles.insert(char_id, corp_roles::ROLE_DIRECTOR);
        }
        snap.items.insert(
            co_id,
            InventoryItemRecord {
                item_id: co_id,
                type_id: 2233,
                owner_id: 98_000_002,
                location_id: 30_000_142,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.structures.insert(
            co_id,
            eve_db::types::UpwellStructureRecord {
                structure_id: co_id,
                type_id: 2233,
                owner_corp_id: 98_000_002,
                solar_system_id: 30_000_142,
                name: format!("Orbital:{planet_id}"),
                position: [0.0; 3],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: false,
                fuel_blocks: 0,
                shield_hp: 100.0,
                armor_hp: 100.0,
                hull_hp: 100.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
        snap.corp_customs_offices.insert(
            98_000_002,
            vec![CustomsOfficeRecord {
                office_id: co_id,
                system_id: 30_000_142,
                reinforce_exit_start: 0,
                reinforce_exit_end: 1,
                allow_alliance_access: true,
                allow_access_with_standings: true,
                standing_level: None,
                corporation_tax_rate: Some(0.05),
                alliance_tax_rate: None,
                excellent_standing_tax_rate: None,
                good_standing_tax_rate: None,
                neutral_standing_tax_rate: Some(0.05),
                bad_standing_tax_rate: None,
                terrible_standing_tax_rate: None,
            }],
        );
        snap.planetary_colonies.insert(
            (char_id, planet_id),
            eve_db::types::PlanetaryColonyRecord {
                planet_id,
                owner_id: char_id,
                solar_system_id: 30_000_142,
                command_center_level: 1,
                pins_count: 1,
                extracted_commodities: HashMap::new(),
                last_extracted_unix: 0,
            },
        );
        snap.planetary_colony_layouts.insert(
            (char_id, planet_id),
            PlanetaryColonyLayoutRecord {
                pins: vec![PlanetaryPinRecord {
                    pin_id: spaceport_pin_id,
                    type_id: 2256,
                    latitude: 0.0,
                    longitude: 0.0,
                    schematic_id: None,
                    install_time_unix: None,
                    expiry_time_unix: None,
                    last_cycle_start_unix: None,
                    contents: vec![PlanetaryPinContentRecord {
                        type_id: 950_501,
                        amount: 10,
                    }],
                }],
                links: vec![],
                routes: vec![],
            },
        );
        for (iid, qty) in [(item_by_id, 5), (stack1, 10), (stack2, 20), (stack3, 15)] {
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: 950_501,
                    owner_id: char_id,
                    location_id: co_id,
                    flag: InventoryFlag::Hangar,
                    quantity: qty,
                    singleton: false,
                    ..Default::default()
                },
            );
        }
        snap.items.insert(
            cont_id,
            InventoryItemRecord {
                item_id: cont_id,
                type_id: 17366,
                owner_id: 98_000_001,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        write_container_ship_state_snap(snap, |st| {
            st.set_container_password(cont_id, 1, "pw_or_test".into());
        });
        snap.items.insert(
            mod_id,
            InventoryItemRecord {
                item_id: mod_id,
                type_id: 950_503,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            charge_id,
            InventoryItemRecord {
                item_id: charge_id,
                type_id: 950_502,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 5,
                singleton: false,
                ..Default::default()
            },
        );
    });

    let bound_co = BoundObjectEntry {
        bound_id: 1,
        session_id: sess.session_id,
        user_id: 1,
        character_id: char_id,
        service: "invbroker".into(),
        object_id: co_id,
        oid_str: co_id.to_string(),
        ref_id: 1,
    };

    // 1. Import ByItemId into pin already holding 10 units -> 10 + 3 = 13 (kills `+=` -> `-=` / `*=`)
    handle_import_export_with_planet(
        &ctx,
        &sess,
        Some(&bound_co),
        &[
            PyRep::Integer(spaceport_pin_id),
            PyRep::Dict(vec![(PyRep::Integer(item_by_id), PyRep::Integer(3))]),
            PyRep::Dict(vec![]),
        ],
        &kw,
    )
    .unwrap();
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&item_by_id].quantity, 2);
        let pin = &snap.planetary_colony_layouts[&(char_id, planet_id)].pins[0];
        let amt = pin.contents.iter().find(|c| c.type_id == 950_501).unwrap().amount;
        assert_eq!(amt, 13);
    });

    // 2. Import ByTypeId qty=18 across sorted stacks [item_by_id=2, stack1=10, stack2=20, stack3=15]:
    // Consumes item_by_id (2), stack1 (10), and exact rem=6 from stack2 -> stack2=14, stack3=15 untouched.
    // Kills `take <= 0 -> >`, `rem -= take -> +=`, and `rem -= take -> /=`.
    handle_import_export_with_planet(
        &ctx,
        &sess,
        Some(&bound_co),
        &[
            PyRep::Integer(spaceport_pin_id),
            PyRep::Dict(vec![(PyRep::Integer(950_501), PyRep::Integer(18))]),
            PyRep::Dict(vec![]),
        ],
        &kw,
    )
    .unwrap();
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&item_by_id));
        assert!(!snap.items.contains_key(&stack1));
        assert_eq!(snap.items[&stack2].quantity, 14);
        assert_eq!(snap.items[&stack3].quantity, 15);
        let pin = &snap.planetary_colony_layouts[&(char_id, planet_id)].pins[0];
        let amt = pin.contents.iter().find(|c| c.type_id == 950_501).unwrap().amount;
        assert_eq!(amt, 31);
    });

    // 3. handle_retrieve_password bitwise OR vs AND/XOR:
    // Case A: sess.corp_role = 0 while DB corp_roles = ROLE_DIRECTOR -> kills `|` -> `&`
    sess.corp_role = 0;
    assert_eq!(
        handle_retrieve_password(&ctx, &sess, None, &[PyRep::Integer(cont_id), PyRep::Integer(1)], &kw)
            .unwrap(),
        PyRep::String("pw_or_test".into())
    );
    // Case B: sess.corp_role = ROLE_DIRECTOR while DB corp_roles = ROLE_DIRECTOR -> kills `|` -> `^`
    sess.corp_role = corp_roles::ROLE_DIRECTOR;
    assert_eq!(
        handle_retrieve_password(&ctx, &sess, None, &[PyRep::Integer(cont_id), PyRep::Integer(1)], &kw)
            .unwrap(),
        PyRep::String("pw_or_test".into())
    );

    // 4. handle_clear_charges happy path moves charge from HiSlot0 to Cargo
    let clr = handle_clear_charges(&ctx, &sess, None, &[PyRep::Integer(ship_id)])
        .await
        .unwrap();
    assert_eq!(clr, PyRep::None);
    assert_eq!(
        ctx.db.with_snapshot(|s| s.items[&charge_id].flag),
        InventoryFlag::Cargo
    );
}
