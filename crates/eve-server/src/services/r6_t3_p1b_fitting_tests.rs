//! Round-6 Track-3 P1 Fitting & Inventory Security Tests (`#393`, `#394`).

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use eve_db::{
    InventoryFlag, InventoryItemRecord, SavedFittingRecord, WorldRepository,
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

#[allow(clippy::too_many_arguments)]
fn custom_type(
    sde: &SdeRegistry,
    type_id: u32,
    group_id: u32,
    category_id: u32,
    volume: f64,
    capacity: f64,
    name: &str,
    attrs: &[(u16, f64)],
    effects: &[u16],
) -> InvType {
    let mut t = sde.types.get(&587).expect("seed type").clone();
    t.type_id = type_id;
    t.group_id = group_id;
    t.category_id = category_id;
    t.volume = volume;
    t.capacity = capacity;
    t.name = name.into();
    t.published = true;
    t.effect_ids = effects.to_vec();
    if category_id != 6 {
        t.default_attributes.clear();
        t.mass = 1.0;
    } else {
        t.default_attributes.insert(eve_sde::attr_ids::TURRET_SLOTS_LEFT, 4.0);
        t.default_attributes.insert(eve_sde::attr_ids::LAUNCHER_SLOTS_LEFT, 4.0);
    }
    for &(k, v) in attrs {
        t.default_attributes.insert(k, v);
    }
    t
}

pub(super) fn build_r6_fitting_ctx() -> Arc<ServerContext> {
    let mut sde = (*shared_sde()).clone();

    let ship = custom_type(
        &sde,
        587,
        25,
        6,
        2500.0,
        140.0,
        "Rifter",
        &[
            (eve_sde::attr_ids::HI_SLOTS, 4.0),
            (eve_sde::attr_ids::MED_SLOTS, 4.0),
            (eve_sde::attr_ids::LOW_SLOTS, 4.0),
            (eve_sde::attr_ids::RIG_SLOTS, 3.0),
            (eve_sde::attr_ids::CPU_OUTPUT, 500.0),
            (eve_sde::attr_ids::POWER_OUTPUT, 500.0),
            (eve_sde::attr_ids::CAPACITOR_CAPACITY, 1000.0),
            (eve_sde::attr_ids::MAX_TARGET_RANGE, 40_000.0),
            (eve_sde::attr_ids::MAX_LOCKED_TARGETS, 2.0),
            (eve_sde::attr_ids::SCAN_RESOLUTION, 250.0),
            (eve_sde::attr_ids::SIGNATURE_RADIUS, 40.0),
        ],
        &[],
    );
    sde.types.insert(587, ship);

    sde.types.insert(
        440,
        custom_type(
            &sde,
            440,
            55,
            7,
            5.0,
            5.0,
            "200mm AutoCannon I",
            &[(604, 83.0), (128, 1.0)],
            &[12],
        ),
    );
    sde.types.insert(
        178,
        custom_type(&sde, 178, 83, 8, 0.05, 0.0, "Phased Plasma S", &[(128, 1.0)], &[]),
    );
    sde.types.insert(
        179,
        custom_type(&sde, 179, 83, 8, 0.25, 0.0, "Phased Plasma L", &[(128, 3.0)], &[]),
    );
    sde.types.insert(
        248,
        custom_type(&sde, 248, 86, 8, 0.05, 0.0, "Multifrequency S", &[], &[]),
    );
    sde.types.insert(
        30090,
        custom_type(&sde, 30090, 954, 32, 10.0, 0.0, "Tengu Core", &[], &[3772]),
    );
    sde.types.insert(
        1952,
        custom_type(
            &sde,
            1952,
            82,
            7,
            5.0,
            0.0,
            "Passive Targeter I",
            &[
                (eve_sde::attr_ids::CAPACITOR_NEED, 25.0),
                (eve_sde::attr_ids::DURATION, 5000.0),
            ],
            &[13],
        ),
    );
    sde.types.insert(
        40635,
        custom_type(
            &sde,
            40635,
            842,
            7,
            50.0,
            0.0,
            "Energy Neutralization Burst Projector",
            &[
                (eve_sde::attr_ids::CAPACITOR_NEED, 200.0),
                (eve_sde::attr_ids::DURATION, 10_000.0),
                (2279, 25_000.0),
                (97, 500.0),
            ],
            &[12],
        ),
    );
    sde.types.insert(
        20280,
        custom_type(
            &sde,
            20280,
            515,
            7,
            100.0,
            0.0,
            "Siege Module I",
            &[
                (eve_sde::attr_ids::CAPACITOR_NEED, 150.0),
                (eve_sde::attr_ids::DURATION, 60_000.0),
                (713, 16275.0),
                (714, 25.0),
            ],
            &[12],
        ),
    );
    sde.types.insert(
        19720,
        custom_type(
            &sde,
            19720,
            485,
            6,
            100_000.0,
            10_000.0,
            "Revelation",
            &[
                (eve_sde::attr_ids::HI_SLOTS, 4.0),
                (eve_sde::attr_ids::CAPACITOR_CAPACITY, 500.0),
                (eve_sde::attr_ids::MAX_LOCKED_TARGETS, 5.0),
            ],
            &[],
        ),
    );
    sde.types.insert(
        16275,
        custom_type(&sde, 16275, 423, 4, 3.0, 0.0, "Strontium Clathrates", &[], &[]),
    );

    let mut base_char = WorldRepository::with_demo_seed()
        .with_snapshot(|s| s.characters.values().next().expect("demo character").clone());
    base_char.structure_id = None;
    base_char.alliance_id = None;
    base_char.war_faction_id = None;

    let ctx = Arc::new(ServerContext::with_sde_and_repository(
        Arc::new(sde),
        Arc::new(WorldRepository::new_empty()),
    ));
    ctx.db.with_mut_snapshot(|snap| {
        snap.next_entity_id = 1_000_900_000;
        let mut ch_a = base_char.clone();
        ch_a.character_id = 9001;
        ch_a.user_id = 9001;
        ch_a.name = "PilotA".into();
        ch_a.corporation_id = 98_000_001;
        ch_a.station_id = Some(60_003_760);
        ch_a.solar_system_id = 30_000_142;
        ch_a.active_ship_id = 1_000_800_001;
        snap.characters.insert(9001, ch_a);

        let mut ch_b = base_char;
        ch_b.character_id = 9002;
        ch_b.user_id = 9002;
        ch_b.name = "PilotB".into();
        ch_b.corporation_id = 98_000_002;
        ch_b.station_id = None;
        ch_b.solar_system_id = 30_000_142;
        ch_b.active_ship_id = 1_000_800_002;
        snap.characters.insert(9002, ch_b);

        snap.items.insert(
            1_000_800_001,
            InventoryItemRecord {
                item_id: 1_000_800_001,
                type_id: 587,
                owner_id: 9001,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            1_000_800_002,
            InventoryItemRecord {
                item_id: 1_000_800_002,
                type_id: 587,
                owner_id: 9002,
                location_id: 30_000_142,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });
    ctx
}

pub(super) fn make_session(
    char_id: i64,
    corp_id: i64,
    ship_id: i64,
    station_id: Option<i64>,
    sys_id: Option<u32>,
) -> SessionState {
    SessionState {
        user_id: Some(char_id as u64),
        character_id: Some(char_id),
        corporation_id: Some(corp_id),
        ship_id: Some(ship_id),
        station_id,
        solar_system_id: sys_id,
        ..Default::default()
    }
}

#[tokio::test]
async fn r6_t3_10_fit_ships_auth_and_charge_validation() {
    let ctx = build_r6_fitting_ctx();
    let ship_id = 1_000_800_001;
    let station_id = 60_003_760;
    let char_id = 9001;

    ctx.db.with_mut_snapshot(|snap| {
        snap.saved_fittings.insert(
            777,
            SavedFittingRecord {
                fitting_id: 777,
                owner_id: 9002,
                name: "SecretFit".into(),
                description: String::new(),
                ship_type_id: 587,
                items: vec![(440, InventoryFlag::HiSlot0, 1)],
            },
        );
        for (iid, tid, qty) in [
            (1_000_800_010, 440, 5),
            (1_000_800_011, 178, 500),
            (1_000_800_012, 248, 50),
            (1_000_800_013, 179, 10),
        ] {
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: tid,
                    owner_id: char_id,
                    location_id: station_id,
                    flag: InventoryFlag::Hangar,
                    quantity: qty,
                    singleton: false,
                    ..Default::default()
                },
            );
        }
    });

    let mut sess = make_session(char_id, 98_000_001, ship_id, Some(station_id), Some(30_000_142));
    let kw = HashMap::new();

    // 1. Unauthorized saved fitting access must fail with AccessDenied
    let err = ctx
        .dispatch_rpc(
            &mut sess,
            "ship",
            None,
            "FitShips",
            &[PyRep::List(vec![PyRep::Integer(ship_id)]), PyRep::Integer(777)],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(err.code, "AccessDenied");

    // 2. Loading charge into empty slot without module fails
    let no_mod = PyRep::List(vec![PyRep::Tuple(vec![
        PyRep::Integer(178),
        PyRep::Integer(27),
        PyRep::Integer(10),
    ])]);
    assert!(ctx.dispatch_rpc(&mut sess, "ship", None, "FitShips", &[no_mod], &kw, None).await.is_err());

    // 3. Wrong charge group fails
    let bad_group = PyRep::List(vec![
        PyRep::Tuple(vec![PyRep::Integer(440), PyRep::Integer(27), PyRep::Integer(1)]),
        PyRep::Tuple(vec![PyRep::Integer(248), PyRep::Integer(27), PyRep::Integer(10)]),
    ]);
    assert!(ctx.dispatch_rpc(&mut sess, "ship", None, "FitShips", &[bad_group], &kw, None).await.is_err());

    // 4. Wrong charge size fails
    let bad_size = PyRep::List(vec![
        PyRep::Tuple(vec![PyRep::Integer(440), PyRep::Integer(27), PyRep::Integer(1)]),
        PyRep::Tuple(vec![PyRep::Integer(179), PyRep::Integer(27), PyRep::Integer(5)]),
    ]);
    assert!(ctx.dispatch_rpc(&mut sess, "ship", None, "FitShips", &[bad_size], &kw, None).await.is_err());

    // 5. Over-capacity charge fails (440 capacity = 5.0 m3, 178 volume = 0.05 -> max 100, 150 fails)
    let over_cap = PyRep::List(vec![
        PyRep::Tuple(vec![PyRep::Integer(440), PyRep::Integer(27), PyRep::Integer(1)]),
        PyRep::Tuple(vec![PyRep::Integer(178), PyRep::Integer(27), PyRep::Integer(150)]),
    ]);
    assert!(ctx.dispatch_rpc(&mut sess, "ship", None, "FitShips", &[over_cap], &kw, None).await.is_err());

    // 6. Valid module + within-capacity charge succeeds
    let valid_fit = PyRep::List(vec![
        PyRep::Tuple(vec![PyRep::Integer(440), PyRep::Integer(27), PyRep::Integer(1)]),
        PyRep::Tuple(vec![PyRep::Integer(178), PyRep::Integer(27), PyRep::Integer(80)]),
    ]);
    assert!(ctx.dispatch_rpc(&mut sess, "ship", None, "FitShips", &[valid_fit], &kw, None).await.is_ok());
}

#[tokio::test]
async fn r6_t3_11_invbroker_destroy_fitting_and_clear_charges() {
    let ctx = build_r6_fitting_ctx();
    let ship_id = 1_000_800_001;
    let station_id = 60_003_760;
    let char_id = 9001;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            1_000_800_020,
            InventoryItemRecord {
                item_id: 1_000_800_020,
                type_id: 30090,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::SubSystemSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            1_000_800_021,
            InventoryItemRecord {
                item_id: 1_000_800_021,
                type_id: 440,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            1_000_800_022,
            InventoryItemRecord {
                item_id: 1_000_800_022,
                type_id: 178,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 40,
                singleton: false,
                ..Default::default()
            },
        );
        snap.items.insert(
            1_000_800_023,
            InventoryItemRecord {
                item_id: 1_000_800_023,
                type_id: 178,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 20,
                singleton: false,
                ..Default::default()
            },
        );
        snap.module_active_states.insert(1_000_800_021);
    });

    let mut sess = make_session(char_id, 98_000_001, ship_id, Some(station_id), Some(30_000_142));
    let kw = HashMap::new();

    assert_eq!(
        ctx.dispatch_rpc(&mut sess, "invbroker", None, "ClearCharges", &[PyRep::Integer(ship_id)], &kw, None)
            .await
            .unwrap_err()
            .code,
        "ModuleIsActive"
    );
    assert_eq!(
        ctx.dispatch_rpc(&mut sess, "invbroker", None, "DestroyFitting", &[PyRep::Integer(1_000_800_021)], &kw, None)
            .await
            .unwrap_err()
            .code,
        "ModuleIsActive"
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.module_active_states.remove(&1_000_800_021);
    });

    ctx.dispatch_rpc(&mut sess, "invbroker", None, "ClearCharges", &[PyRep::Integer(ship_id)], &kw, None)
        .await
        .unwrap();
    let cargo_qty = ctx
        .db
        .with_snapshot(|snap| snap.items.get(&1_000_800_023).map(|i| i.quantity))
        .unwrap();
    assert_eq!(cargo_qty, 60);

    assert_eq!(
        ctx.dispatch_rpc(&mut sess, "invbroker", None, "DestroyFitting", &[PyRep::Integer(1_000_800_020)], &kw, None)
            .await
            .unwrap_err()
            .code,
        "SubsystemHasFittedModules"
    );

    ctx.dispatch_rpc(&mut sess, "invbroker", None, "DestroyFitting", &[PyRep::Integer(1_000_800_021)], &kw, None)
        .await
        .unwrap();
    assert!(ctx.db.with_snapshot(|snap| !snap.items.contains_key(&1_000_800_021)));
}
