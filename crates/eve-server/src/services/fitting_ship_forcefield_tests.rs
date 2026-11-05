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

#[tokio::test]
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
