#![forbid(unsafe_code)]

use super::*;
use eve_db::{
    InventoryFlag, InventoryItemRecord, StarbaseDetailSettings, VirtualServerClock, WorldRepository,
};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::{InvType, SdeRegistry};
use glam::DVec3;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn custom_inv_type(
    sde: &SdeRegistry,
    type_id: u32,
    group_id: u32,
    category_id: u32,
    name: &str,
    attrs: &[(u16, f64)],
) -> InvType {
    let mut t = sde.types.get(&587).expect("seed type").clone();
    t.type_id = type_id;
    t.group_id = group_id;
    t.category_id = category_id;
    t.name = name.into();
    t.default_attributes.clear();
    for &(k, v) in attrs {
        t.default_attributes.insert(k, v);
    }
    t
}

fn test_ctx_with_custom<F: FnOnce(&mut SdeRegistry)>(mutate_sde: F) -> ServerContext {
    let mut sde = (*shared_sde()).clone();
    mutate_sde(&mut sde);
    let db = Arc::new(WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(Arc::new(sde), db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_800_000_000)));
    ctx
}

#[test]
fn deployable_infrastructure_and_mobile_depot_classification() {
    let ctx = test_ctx_with_custom(|sde| {
        sde.types.insert(991_001, custom_inv_type(sde, 991_001, 1246, 22, "Mobile Depot", &[]));
        sde.types.insert(991_002, custom_inv_type(sde, 991_002, 361, 22, "Mobile Warp Disruptor I", &[]));
        sde.types.insert(991_003, custom_inv_type(sde, 991_003, 186, 2, "Ship Wreck", &[]));
        sde.types.insert(991_004, custom_inv_type(sde, 991_004, 25, 6, "Rifter", &[]));
    });

    assert!(is_deployable_item(&ctx.sde, 991_001));
    assert!(is_infrastructure_item(&ctx.sde, 991_001));
    assert!(is_mobile_depot(&ctx.sde, 991_001));

    assert!(is_deployable_item(&ctx.sde, 991_002));
    assert!(is_infrastructure_item(&ctx.sde, 991_002));
    assert!(!is_mobile_depot(&ctx.sde, 991_002));

    assert!(!is_deployable_item(&ctx.sde, 991_003));
    assert!(!is_deployable_item(&ctx.sde, 991_004));
    assert!(!is_deployable_item(&ctx.sde, 999_999_999));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn forcefield_entry_permissions_and_ship_configuration_password_flow() {
    let ctx = test_ctx_with_custom(|sde| {
        sde.types.insert(
            12235,
            custom_inv_type(sde, 12235, 365, 23, "Amarr Control Tower", &[(680, 30_000.0)]),
        );
    });

    let sys_id = 30_002_505u32;
    let owner_char = 95_100_001;
    let outsider_char = 95_100_002;
    let ally_char = 95_100_003;
    let owner_corp = 98_100_010;
    let outsider_corp = 98_100_020;
    let alliance_id = 99_100_001;
    let tower_id = 1_000_951_001;
    let outsider_ship = 1_000_951_002;

    ctx.db.with_mut_snapshot(|snap| {
        let base_ch = snap.characters.values().next().cloned().expect("seed char");
        for (cid, corp, ally) in [
            (owner_char, owner_corp, Some(alliance_id)),
            (outsider_char, outsider_corp, None),
            (ally_char, outsider_corp, Some(alliance_id)),
        ] {
            let mut ch = base_ch.clone();
            ch.character_id = cid;
            ch.user_id = cid as u64;
            ch.corporation_id = corp;
            ch.alliance_id = ally;
            ch.solar_system_id = sys_id;
            ch.active_ship_id = outsider_ship;
            snap.characters.insert(cid, ch);
        }
        snap.items.insert(
            tower_id,
            InventoryItemRecord {
                item_id: tower_id,
                type_id: 12235,
                owner_id: owner_char,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.item_positions.insert(tower_id, [10_000.0, 0.0, 0.0]);
        snap.items.insert(
            outsider_ship,
            InventoryItemRecord {
                item_id: outsider_ship,
                type_id: 587,
                owner_id: outsider_char,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.starbase_settings.insert(
            tower_id,
            StarbaseDetailSettings {
                allow_corporation_members: true,
                allow_alliance_members: true,
                password: Some("opensesame".into()),
                ..Default::default()
            },
        );
    });

    // Owner and alliance member can enter immediately; outsider without password cannot
    assert!(can_enter_forcefield(&ctx, owner_char, outsider_ship, tower_id));
    assert!(can_enter_forcefield(&ctx, ally_char, outsider_ship, tower_id));
    assert!(!can_enter_forcefield(&ctx, outsider_char, outsider_ship, tower_id));

    // check_pos_forcefield_coverage denies outsider inside 30km shield radius, allows outside
    assert_eq!(
        check_pos_forcefield_coverage(
            &ctx,
            sys_id,
            outsider_char,
            outsider_ship,
            &[DVec3::new(15_000.0, 0.0, 0.0)],
        )
        .await
        .unwrap_err()
        .code,
        "ForcefieldAccessDenied"
    );
    assert!(check_pos_forcefield_coverage(
        &ctx,
        sys_id,
        outsider_char,
        outsider_ship,
        &[DVec3::new(80_000.0, 0.0, 0.0)],
    )
    .await
    .is_ok());

    // Outsider sets matching ship password via ship_configuration("SetShipPassword") -> now allowed!
    let outsider_sess = SessionState {
        user_id: Some(outsider_char as u64),
        character_id: Some(outsider_char),
        corporation_id: Some(outsider_corp),
        solar_system_id: Some(sys_id),
        ship_id: Some(outsider_ship),
        ..Default::default()
    };
    let pw_res = ship_configuration(
        &ctx,
        &outsider_sess,
        "SetShipPassword",
        &[PyRep::Integer(outsider_ship), PyRep::String("opensesame".into())],
    )
    .expect("set ship password");
    assert_eq!(pw_res, PyRep::None);
    assert!(can_enter_forcefield(&ctx, outsider_char, outsider_ship, tower_id));
    assert!(check_pos_forcefield_coverage(
        &ctx,
        sys_id,
        outsider_char,
        outsider_ship,
        &[DVec3::new(15_000.0, 0.0, 0.0)],
    )
    .await
    .is_ok());

    // ConfigureShip & GetShipConfiguration & OwnershipViolation guard
    let cfg_updated = ship_configuration(
        &ctx,
        &outsider_sess,
        "ConfigureShip",
        &[
            PyRep::Integer(outsider_ship),
            PyRep::Dict(vec![
                (PyRep::String(CFG_FLEET_HANGAR_FLEET.into()), PyRep::Bool(true)),
                (PyRep::String(CFG_SMB_CORP.into()), PyRep::Integer(1)),
            ]),
        ],
    )
    .expect("ConfigureShip");
    assert_eq!(cfg_updated.dict_get(CFG_FLEET_HANGAR_FLEET).and_then(|v| v.as_bool()), Some(true));
    assert_eq!(cfg_updated.dict_get(CFG_SMB_CORP).and_then(|v| v.as_bool()), Some(true));

    let fetched = ship_configuration(&ctx, &outsider_sess, "GetShipConfiguration", &[PyRep::Integer(outsider_ship)])
        .expect("GetShipConfiguration");
    assert_eq!(fetched.dict_get(CFG_FLEET_HANGAR_FLEET).and_then(|v| v.as_bool()), Some(true));

    let owner_sess = SessionState {
        user_id: Some(owner_char as u64),
        character_id: Some(owner_char),
        ship_id: Some(outsider_ship),
        ..Default::default()
    };
    assert_eq!(
        ship_configuration(&ctx, &owner_sess, "SetShipPassword", &[PyRep::String("bad".into())])
            .unwrap_err()
            .code,
        "OwnershipViolation"
    );
}

#[test]
fn set_ship_password_rejects_missing_or_integer_password_without_mutating() {
    let ctx = test_ctx_with_custom(|_| {});
    let char_id = 90_000_001_i64;
    let foreign_id = 90_000_002_i64;
    let ship_id = 77_700_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
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
                ..Default::default()
            },
        );
    });
    let mut owner_sess = SessionState::default();
    owner_sess.user_id = Some(char_id as u64);
    owner_sess.character_id = Some(char_id);
    owner_sess.ship_id = Some(ship_id);
    let mut foreign_sess = SessionState::default();
    foreign_sess.user_id = Some(foreign_id as u64);
    foreign_sess.character_id = Some(foreign_id);
    foreign_sess.ship_id = Some(ship_id);

    ship_configuration(
        &ctx,
        &owner_sess,
        "SetShipPassword",
        &[PyRep::String("initial-pw".into())],
    )
    .expect("valid single-string SetShipPassword succeeds");

    let commits_before = ctx.db.tx_commit_count();
    for bad_args in [
        vec![],
        vec![PyRep::Integer(ship_id)],
        vec![PyRep::Integer(ship_id), PyRep::Integer(123)],
    ] {
        assert_eq!(
            ship_configuration(&ctx, &owner_sess, "SetShipPassword", &bad_args)
                .unwrap_err()
                .code,
            "InvalidArgument"
        );
    }
    assert_eq!(
        ship_configuration(
            &ctx,
            &foreign_sess,
            "SetShipPassword",
            &[PyRep::Integer(ship_id), PyRep::String("hack".into())],
        )
        .unwrap_err()
        .code,
        "OwnershipViolation"
    );
    assert_eq!(ctx.db.tx_commit_count(), commits_before);
    assert_eq!(
        ctx.db
            .with_snapshot(|snap| fitting_containers_ships::ship_password_snap(snap, ship_id))
            .as_deref(),
        Some("initial-pw")
    );

    ship_configuration(
        &ctx,
        &owner_sess,
        "SetShipPassword",
        &[PyRep::Integer(ship_id), PyRep::String("updated-pw".into())],
    )
    .expect("explicit (ship_id, password) succeeds");
    assert_eq!(
        ctx.db
            .with_snapshot(|snap| fitting_containers_ships::ship_password_snap(snap, ship_id))
            .as_deref(),
        Some("updated-pw")
    );
}

#[test]
fn ship_configuration_enforces_ship_validation_and_tx_atomicity() {
    let ctx = test_ctx_with_custom(|_| {});
    let char_id = 90_000_001_i64;
    let other_id = 90_000_002_i64;
    let ship_id = 77_700_101_i64;
    let foreign_ship_id = 77_700_102_i64;
    let non_ship_id = 77_700_103_i64;
    ctx.db.with_mut_snapshot(|snap| {
        for (item_id, type_id, owner_id) in [
            (ship_id, 587, char_id),
            (foreign_ship_id, 587, other_id),
            (non_ship_id, 34, char_id),
        ] {
            snap.items.insert(
                item_id,
                InventoryItemRecord {
                    item_id,
                    type_id,
                    owner_id,
                    location_id: 60_003_760,
                    flag: InventoryFlag::Hangar,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
        }
    });

    let mut owner_sess = SessionState::default();
    owner_sess.user_id = Some(char_id as u64);
    owner_sess.character_id = Some(char_id);
    owner_sess.ship_id = Some(ship_id);

    let commits_before = ctx.db.tx_commit_count();
    assert_eq!(
        ship_configuration(
            &ctx,
            &owner_sess,
            "GetShipConfiguration",
            &[PyRep::Integer(99_999_999)],
        )
        .unwrap_err()
        .code,
        "ShipNotFound"
    );
    for bad_target in [99_999_999_i64, foreign_ship_id, non_ship_id] {
        let expected_err = if bad_target == 99_999_999 {
            "ShipNotFound"
        } else {
            "OwnershipViolation"
        };
        assert_eq!(
            ship_configuration(
                &ctx,
                &owner_sess,
                "SetShipPassword",
                &[PyRep::Integer(bad_target), PyRep::String("pw".into())],
            )
            .unwrap_err()
            .code,
            expected_err
        );
        assert_eq!(
            ship_configuration(
                &ctx,
                &owner_sess,
                "ConfigureShip",
                &[
                    PyRep::Integer(bad_target),
                    PyRep::Dict(vec![(
                        PyRep::String(CFG_FLEET_HANGAR_FLEET.into()),
                        PyRep::Bool(true),
                    )]),
                ],
            )
            .unwrap_err()
            .code,
            expected_err
        );
    }
    assert_eq!(
        ship_configuration(
            &ctx,
            &owner_sess,
            "ConfigureShip",
            &[
                PyRep::Integer(ship_id),
                PyRep::Dict(vec![(PyRep::String("unknownFlag".into()), PyRep::Bool(true))]),
            ],
        )
        .unwrap_err()
        .code,
        "InvalidArgument"
    );
    assert_eq!(ctx.db.tx_commit_count(), commits_before);

    let cfg_reply = ship_configuration(
        &ctx,
        &owner_sess,
        "ConfigureShip",
        &[
            PyRep::Integer(ship_id),
            PyRep::Dict(vec![(
                PyRep::String(CFG_FLEET_HANGAR_FLEET.into()),
                PyRep::Bool(true),
            )]),
        ],
    )
    .expect("owner ConfigureShip succeeds");
    assert_eq!(
        cfg_reply.dict_get(CFG_FLEET_HANGAR_FLEET).and_then(|v| v.as_bool()),
        Some(true)
    );
    assert_eq!(ctx.db.tx_commit_count(), commits_before + 1);

    ship_configuration(
        &ctx,
        &owner_sess,
        "SetShipPassword",
        &[PyRep::Integer(ship_id), PyRep::String("vault-ship".into())],
    )
    .expect("owner SetShipPassword succeeds");
    assert_eq!(ctx.db.tx_commit_count(), commits_before + 2);
    ctx.db.with_snapshot(|snap| {
        let cfg = fitting_containers_ships::ship_access_config_snap(snap, ship_id);
        assert!(cfg.allow_fleet_hangar);
        assert_eq!(cfg.password.as_deref(), Some("vault-ship"));
    });
}

#[cfg(test)]
#[path = "fitting_ship_forcefield_cov_edges_tests.rs"]
mod fitting_ship_forcefield_cov_edges_tests;
