//! Extra branch-coverage and mutation-killing unit tests for `fitting_mgr_service`.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::account_services::account_services_tests::PyRepTestExt;
use crate::services::inventory_fitting::fitting_mgr_service::handle_fitting_mgr;
use eve_db::{
    CorporationRecord, InventoryFlag, InventoryItemRecord, SavedFittingRecord, VirtualServerClock,
    WorldRepository,
};
use eve_machonet::{service_roles, SessionState};
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::SdeRegistry;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn make_ctx() -> ServerContext {
    let db = Arc::new(WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(shared_sde(), db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_800_000_000)));
    ctx
}

async fn call_fit(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    method: &str,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    let kw = HashMap::new();
    handle_fitting_mgr(ctx, sess, None, svc, method, args, &kw).await
}

async fn fit_ok(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    method: &str,
    args: &[PyRep],
) -> PyRep {
    call_fit(ctx, sess, svc, method, args)
        .await
        .expect("handled")
        .expect("ok")
}

async fn fit_err(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    method: &str,
    args: &[PyRep],
) -> String {
    call_fit(ctx, sess, svc, method, args)
        .await
        .expect("handled")
        .expect_err("err")
        .code
}

#[tokio::test]
async fn save_fitting_item_validation_positional_signatures_and_active_ship_extraction() {
    let ctx = make_ctx();
    let cid = 90_000_001_i64;
    let corp_id = 98_000_001_i64;
    let ship_id = 100_000_001_i64;
    let mut sess = SessionState {
        user_id: Some(1),
        character_id: Some(cid),
        corporation_id: Some(corp_id),
        ship_id: Some(ship_id),
        ..Default::default()
    };

    // Unhandled service/method returns None
    assert!(call_fit(&ctx, &mut sess, "dogmaIM", "GetAllInfo", &[])
        .await
        .is_none());

    // Unauthenticated session returns error
    let mut unauth = SessionState::default();
    assert!(call_fit(&ctx, &mut unauth, "charFittingMgr", "GetFittings", &[])
        .await
        .unwrap()
        .is_err());

    // SaveFitting & SaveManyFittings argument errors
    assert_eq!(
        fit_err(
            &ctx,
            &mut sess,
            "charFittingMgr",
            "SaveFitting",
            &[PyRep::Integer(cid)]
        )
        .await,
        "InvalidArgument"
    );
    assert_eq!(
        fit_err(&ctx, &mut sess, "charFittingMgr", "SaveManyFittings", &[]).await,
        "InvalidArgument"
    );
    assert_eq!(
        fit_err(
            &ctx,
            &mut sess,
            "charFittingMgr",
            "SaveManyFittings",
            &[PyRep::Integer(cid), PyRep::String("not-a-collection".into())]
        )
        .await,
        "InvalidArgument"
    );
    // Missing shipTypeID in object or positional
    assert_eq!(
        fit_err(
            &ctx,
            &mut sess,
            "charFittingMgr",
            "SaveFitting",
            &[PyRep::Integer(cid), PyRep::keyval([("name", PyRep::String("NoHull".into()))])]
        )
        .await,
        "InvalidShipType"
    );
    assert_eq!(
        fit_err(
            &ctx,
            &mut sess,
            "charFittingMgr",
            "SaveFitting",
            &[PyRep::String("LegacyNameOnly".into())]
        )
        .await,
        "InvalidShipType"
    );
    // Non-ship typeID (34 = Tritanium)
    assert_eq!(
        fit_err(
            &ctx,
            &mut sess,
            "charFittingMgr",
            "SaveFitting",
            &[PyRep::String("BadHull".into()), PyRep::Integer(34)]
        )
        .await,
        "InvalidShipType"
    );

    // parse_fit_items validation branches
    let make_fit_with_items = |items: PyRep| -> PyRep {
        PyRep::keyval([
            ("name", PyRep::String("TestFit".into())),
            ("shipTypeID", PyRep::Integer(587)),
            ("items", items),
        ])
    };
    assert_eq!(
        fit_err(
            &ctx,
            &mut sess,
            "charFittingMgr",
            "SaveFitting",
            &[make_fit_with_items(PyRep::Integer(123))]
        )
        .await,
        "InvalidFittingItems"
    );
    assert_eq!(
        fit_err(
            &ctx,
            &mut sess,
            "charFittingMgr",
            "SaveFitting",
            &[make_fit_with_items(PyRep::List(vec![PyRep::Integer(1)]))]
        )
        .await,
        "InvalidFittingItem"
    );
    assert_eq!(
        fit_err(
            &ctx,
            &mut sess,
            "charFittingMgr",
            "SaveFitting",
            &[make_fit_with_items(PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::Integer(0),
                PyRep::Integer(11),
                PyRep::Integer(1),
            ])]))]
        )
        .await,
        "InvalidFittingItem"
    );
    assert_eq!(
        fit_err(
            &ctx,
            &mut sess,
            "charFittingMgr",
            "SaveFitting",
            &[make_fit_with_items(PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::Integer(439),
                PyRep::Integer(-1),
                PyRep::Integer(1),
            ])]))]
        )
        .await,
        "InvalidFittingItem"
    );
    assert_eq!(
        fit_err(
            &ctx,
            &mut sess,
            "charFittingMgr",
            "SaveFitting",
            &[make_fit_with_items(PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::Integer(439),
                PyRep::Integer(11),
                PyRep::Integer(i64::MAX),
            ])]))]
        )
        .await,
        "InvalidFittingItem"
    );
    assert_eq!(
        fit_err(
            &ctx,
            &mut sess,
            "charFittingMgr",
            "SaveFitting",
            &[make_fit_with_items(PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::Integer(439),
                PyRep::Integer(11),
                PyRep::Integer(0),
            ])]))]
        )
        .await,
        "InvalidFittingItem"
    );
    // Invalid flag (Hangar = 4)
    assert_eq!(
        fit_err(
            &ctx,
            &mut sess,
            "charFittingMgr",
            "SaveFitting",
            &[make_fit_with_items(PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::Integer(439),
                PyRep::Integer(4),
                PyRep::Integer(1),
            ])]))]
        )
        .await,
        "InvalidFittingFlag"
    );
    // Fitted slot with quantity != 1 -> InvalidFittingQuantity
    assert_eq!(
        fit_err(
            &ctx,
            &mut sess,
            "charFittingMgr",
            "SaveFitting",
            &[make_fit_with_items(PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::Integer(439),
                PyRep::Integer(InventoryFlag::MedSlot0 as i64),
                PyRep::Integer(2),
            ])]))]
        )
        .await,
        "InvalidFittingQuantity"
    );
    // Fitted slot with wrong slot type (putting MedSlot module 439 into LoSlot0 = 11) -> InvalidFittingModule
    assert_eq!(
        fit_err(
            &ctx,
            &mut sess,
            "charFittingMgr",
            "SaveFitting",
            &[make_fit_with_items(PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::Integer(439),
                PyRep::Integer(InventoryFlag::LoSlot0 as i64),
                PyRep::Integer(1),
            ])]))]
        )
        .await,
        "InvalidFittingModule"
    );
    // Cargo flag (5) with unknown item type -> UnknownItemType
    assert_eq!(
        fit_err(
            &ctx,
            &mut sess,
            "charFittingMgr",
            "SaveFitting",
            &[make_fit_with_items(PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::Integer(999_999_999),
                PyRep::Integer(InventoryFlag::Cargo as i64),
                PyRep::Integer(10),
            ])]))]
        )
        .await,
        "UnknownItemType"
    );

    // Active ship module/drone extraction when items_arg is None and active ship matches shipTypeID (587)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(sh) = snap.items.get_mut(&ship_id) {
            sh.owner_id = cid;
            sh.type_id = 587;
        }
        snap.items.insert(
            960_001,
            InventoryItemRecord {
                item_id: 960_001,
                type_id: 439, // 1MN Afterburner I (category 7)
                owner_id: cid,
                location_id: ship_id,
                flag: InventoryFlag::MedSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });
    let fid_from_ship = fit_ok(
        &ctx,
        &mut sess,
        "charFittingMgr",
        "SaveFitting",
        &[
            PyRep::String("FromActiveShip".into()),
            PyRep::Integer(587),
            PyRep::String("Auto-extracted".into()),
        ],
    )
    .await
    .as_i64()
    .unwrap();
    assert!(ctx
        .db
        .with_snapshot(|s| !s.saved_fittings[&fid_from_ship].items.is_empty()));

    // Legacy positional signature with Tuple items in args[2]
    let fid_pos_tuple = fit_ok(
        &ctx,
        &mut sess,
        "charFittingMgr",
        "SaveFitting",
        &[
            PyRep::String("LegacyTupleItems".into()),
            PyRep::Integer(587),
            PyRep::Tuple(vec![PyRep::Tuple(vec![
                PyRep::Integer(34),
                PyRep::Integer(InventoryFlag::Cargo as i64),
                PyRep::Integer(50),
            ])]),
        ],
    )
    .await
    .as_i64()
    .unwrap();
    assert_eq!(
        ctx.db.with_snapshot(|s| s.saved_fittings[&fid_pos_tuple].description.clone()),
        "Saved via charFittingMgr"
    );
}

#[tokio::test]
async fn alliance_fittings_community_content_role_update_delete_and_max_cap() {
    let ctx = make_ctx();
    let cid = 95_700_001_i64;
    let corp_id = 98_700_001_i64;
    let alliance_id = 99_700_001_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().unwrap();
        ch.character_id = cid;
        ch.user_id = cid as u64;
        ch.corporation_id = corp_id;
        ch.alliance_id = Some(alliance_id);
        snap.characters.insert(cid, ch);
        snap.corporations.insert(
            corp_id,
            CorporationRecord {
                corporation_id: corp_id,
                ceo_id: cid,
                alliance_id: Some(alliance_id),
                ..Default::default()
            },
        );
    });

    let mut sess = SessionState {
        user_id: Some(cid as u64),
        character_id: Some(cid),
        corporation_id: Some(corp_id),
        alliance_id: Some(alliance_id),
        ..Default::default()
    };

    let fit_obj = PyRep::keyval([
        ("name", PyRep::String("Alliance Doctrine".into())),
        ("shipTypeID", PyRep::Integer(587)),
        ("description", PyRep::String("Fleet tackle".into())),
        ("fitData", PyRep::List(vec![])),
    ]);

    // 1. allianceFittingMgr SaveManyFittings (Dict form, 1-arg), GetFittings, UpdateFitting (3-arg), DeleteFitting (1-arg)
    let saved_all = fit_ok(
        &ctx,
        &mut sess,
        "allianceFittingMgr",
        "SaveManyFittings",
        &[PyRep::Dict(vec![(PyRep::Integer(1), fit_obj.clone())])],
    )
    .await;
    let all_fid = saved_all.as_list().unwrap()[0].as_i64().unwrap();

    // FittingOwnerMismatch when querying GetFittings with wrong owner
    assert_eq!(
        fit_err(
            &ctx,
            &mut sess,
            "allianceFittingMgr",
            "GetFittings",
            &[PyRep::Integer(cid)]
        )
        .await,
        "UserError"
    );
    let all_fits = fit_ok(&ctx, &mut sess, "allianceFittingMgr", "GetFittings", &[]).await;
    assert_eq!(all_fits.as_dict().unwrap().len(), 1);

    // UpdateFitting error branches: empty name, not found, ownership violation
    assert_eq!(
        fit_err(
            &ctx,
            &mut sess,
            "allianceFittingMgr",
            "UpdateFitting",
            &[PyRep::Integer(all_fid), PyRep::String("   ".into()), PyRep::String("d".into())]
        )
        .await,
        "InvalidFittingName"
    );
    assert_eq!(
        fit_err(
            &ctx,
            &mut sess,
            "allianceFittingMgr",
            "UpdateFitting",
            &[PyRep::Integer(999_999), PyRep::String("Valid".into()), PyRep::String("d".into())]
        )
        .await,
        "FittingNotFound"
    );
    assert_eq!(
        fit_err(
            &ctx,
            &mut sess,
            "charFittingMgr",
            "UpdateFitting",
            &[PyRep::Integer(all_fid), PyRep::String("Valid".into()), PyRep::String("d".into())]
        )
        .await,
        "FittingOwnershipViolation"
    );

    // 4-arg UpdateFitting with (ownerID, fittingID, name, desc) order
    let upd_all = fit_ok(
        &ctx,
        &mut sess,
        "allianceFittingMgr",
        "UpdateFitting",
        &[
            PyRep::Integer(alliance_id),
            PyRep::Integer(all_fid),
            PyRep::String("Renamed Doctrine".into()),
            PyRep::String("New Desc".into()),
        ],
    )
    .await;
    assert_eq!(
        upd_all.dict_get("name"),
        Some(&PyRep::String("Renamed Doctrine".into()))
    );

    // DeleteFitting on non-existent fitting returns Bool(false); 2-arg reversed (fittingID, ownerID) returns Bool(true)
    assert_eq!(
        fit_ok(
            &ctx,
            &mut sess,
            "allianceFittingMgr",
            "DeleteFitting",
            &[PyRep::Integer(999_999)]
        )
        .await,
        PyRep::Bool(false)
    );
    assert_eq!(
        fit_ok(
            &ctx,
            &mut sess,
            "allianceFittingMgr",
            "DeleteFitting",
            &[PyRep::Integer(all_fid), PyRep::Integer(alliance_id)]
        )
        .await,
        PyRep::Bool(true)
    );

    // 2. Community fittings (owner 1 and 1_000_282) with ROLE_CONTENT (non-GM)
    sess.role = service_roles::ROLE_CONTENT;
    let comm_fid = fit_ok(
        &ctx,
        &mut sess,
        "corpFittingMgr",
        "SaveFitting",
        &[PyRep::Integer(1), fit_obj.clone()],
    )
    .await
    .as_i64()
    .unwrap();
    // 3-arg UpdateNameAndDescription on community fitting infers community owner
    fit_ok(
        &ctx,
        &mut sess,
        "corpFittingMgr",
        "UpdateNameAndDescription",
        &[
            PyRep::Integer(comm_fid),
            PyRep::String("Curated Community Fit".into()),
            PyRep::String("Official".into()),
        ],
    )
    .await;
    // 1-arg DeleteFitting on community fitting infers community owner
    assert_eq!(
        fit_ok(
            &ctx,
            &mut sess,
            "corpFittingMgr",
            "DeleteFitting",
            &[PyRep::Integer(comm_fid)]
        )
        .await,
        PyRep::Bool(true)
    );

    // 3. OwnerMaxFittings cap (250 for character)
    sess.role = 0;
    ctx.db.with_mut_snapshot(|snap| {
        for i in 1..=250 {
            let id = 800_000 + i;
            snap.saved_fittings.insert(
                id,
                SavedFittingRecord {
                    fitting_id: id,
                    owner_id: cid,
                    name: format!("Fit {i}"),
                    ship_type_id: 587,
                    description: String::new(),
                    items: Vec::new(),
                },
            );
        }
    });
    assert_eq!(
        fit_err(
            &ctx,
            &mut sess,
            "charFittingMgr",
            "SaveFitting",
            &[PyRep::Integer(cid), fit_obj]
        )
        .await,
        "OwnerMaxFittings"
    );

    // 1-arg DeleteManyFittings cleans up character fittings
    let del_one = fit_ok(
        &ctx,
        &mut sess,
        "charFittingMgr",
        "DeleteManyFittings",
        &[PyRep::Tuple(vec![PyRep::Integer(800_001)])],
    )
    .await;
    assert_eq!(del_one, PyRep::List(vec![PyRep::Integer(800_001)]));
}
