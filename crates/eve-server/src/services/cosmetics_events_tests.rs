//! Unit tests for `cosmetics_events` (`materialize_system_lod2`, `interdict_convoy`, `shipCosmeticsMgr`).

#![forbid(unsafe_code)]

use super::*;
use crate::services::account_services::account_services_tests::{auth_session, test_ctx};
use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_machonet::service_roles;
use eve_proto::PyRep;
use std::collections::HashMap;

#[tokio::test]
async fn lod2_materialization_convoy_interdiction_and_skin_grants() {
    let ctx = test_ctx();
    let uid = 1_u64;
    let cid = 90_000_001_i64;
    let sys_id = 30_000_142_u32;
    let station_id = 60_003_760_i64;
    let mut sess = auth_session(uid, cid, 98_000_001, sys_id);
    let kw = HashMap::new();

    // Non-GM rejected on materialize_system_lod2, interdict_convoy, GiveSkin
    assert_eq!(
        materialize_system_lod2(&ctx, &sess, &[]).await.unwrap_err().code,
        "InsufficientRole"
    );
    assert_eq!(
        interdict_convoy(&ctx, &sess, &[]).await.unwrap_err().code,
        "InsufficientRole"
    );
    assert_eq!(
        handle(&ctx, &mut sess, "shipCosmeticsMgr", "GiveSkin", &[], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "InsufficientRole"
    );

    sess.role = service_roles::ROLE_GMH;

    // materialize_system_lod2 validation & execution
    assert_eq!(
        materialize_system_lod2(&ctx, &sess, &[PyRep::Integer(-1)])
            .await
            .unwrap_err()
            .code,
        "UnknownSolarSystem"
    );
    assert_eq!(
        materialize_system_lod2(&ctx, &sess, &[PyRep::Integer(30_000_144)])
            .await
            .unwrap_err()
            .code,
        "CannotMaterializeRemoteSystem"
    );
    assert!(materialize_system_lod2(&ctx, &sess, &[PyRep::Integer(sys_id as i64)])
        .await
        .is_ok());

    // interdict_convoy validation branches
    sess.station_id = Some(station_id);
    assert_eq!(
        interdict_convoy(&ctx, &sess, &[PyRep::Integer(1)])
            .await
            .unwrap_err()
            .code,
        "ShipDocked"
    );
    sess.station_id = None;
    sess.ship_id = None;
    assert_eq!(
        interdict_convoy(&ctx, &sess, &[PyRep::Integer(1)])
            .await
            .unwrap_err()
            .code,
        "NoActiveShip"
    );
    sess.ship_id = Some(100_000_001);
    assert_eq!(
        interdict_convoy(&ctx, &sess, &[]).await.unwrap_err().code,
        "InvalidArgument"
    );
    assert_eq!(
        interdict_convoy(&ctx, &sess, &[PyRep::Integer(999_999_999)])
            .await
            .unwrap_err()
            .code,
        "ConvoyNotFound"
    );

    // Find a valid permanent SKIN license in SDE (category 91)
    let (lic_tid, skin_id, valid_ship_tid) = ctx
        .sde
        .skin_licenses
        .iter()
        .find_map(|(&ltid, lic)| {
            let st = ctx.sde.types.get(&ltid).filter(|t| t.category_id == 91)?;
            let sk = ctx.sde.skins.get(&lic.skin_id).filter(|s| !s.types.is_empty())?;
            Some((st.type_id, sk.skin_id, sk.types[0]))
        })
        .expect("SDE must contain at least one category 91 skin license");

    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "shipCosmeticsMgr",
            "GiveSkin",
            &[PyRep::Integer(skin_id as i64), PyRep::Integer(34)], // 34 is Tritanium, not a compatible ship
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidShipTypeForSkin"
    );

    let give_rep = handle(
        &ctx,
        &mut sess,
        "shipCosmeticsMgr",
        "GiveSkin",
        &[
            PyRep::Integer(skin_id as i64),
            PyRep::Integer(valid_ship_tid as i64),
        ],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(give_rep.dict_get("licensed"), Some(&PyRep::Bool(true)));

    // ActivateSkinLicense
    sess.station_id = Some(station_id);
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "shipCosmeticsMgr",
            "ActivateSkinLicense",
            &[PyRep::Integer(34)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidSkinLicenseType"
    );
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "shipCosmeticsMgr",
            "ActivateSkinLicense",
            &[PyRep::Integer(lic_tid as i64)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "SkinLicenseItemNotFound"
    );

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.station_id = Some(station_id);
        }
        snap.items.insert(
            920_001,
            InventoryItemRecord {
                item_id: 920_001,
                type_id: lic_tid,
                owner_id: cid,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 2,
                ..Default::default()
            },
        );
    });

    let act_rep = handle(
        &ctx,
        &mut sess,
        "shipCosmeticsMgr",
        "ActivateSkinLicense",
        &[PyRep::Integer(lic_tid as i64)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(act_rep.dict_get("licensed"), Some(&PyRep::Bool(true)));
    assert_eq!(
        ctx.db.with_snapshot(|s| s.items.get(&920_001).map(|i| i.quantity)),
        Some(1)
    );
}
