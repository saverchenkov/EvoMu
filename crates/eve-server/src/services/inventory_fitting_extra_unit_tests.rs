//! Extra unit and mutation-killing tests for `services::inventory_fitting`.

use crate::router::{BoundObjectEntry, ServerContext};
use crate::services::inventory_fitting::{csprng_unit_f64, handle};
use eve_db::{
    corp_roles, CorporationRecord, InventoryFlag, InventoryItemRecord, TrainedSkillRecord,
    UpwellStructureRecord, WorldRepository,
};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::SdeRegistry;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

#[cfg(test)]
#[path = "inventory_fitting_moves_and_stacks_unit_tests.rs"]
mod inventory_fitting_moves_and_stacks_unit_tests;

pub(super) fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

pub(super) fn mk_item(
    item_id: i64,
    type_id: u32,
    owner_id: i64,
    location_id: i64,
    flag: InventoryFlag,
    quantity: i32,
    singleton: bool,
) -> InventoryItemRecord {
    InventoryItemRecord {
        item_id,
        type_id,
        owner_id,
        location_id,
        flag,
        quantity,
        singleton,
        version: 1,
        ..Default::default()
    }
}

pub(super) async fn call_add(
    ctx: &ServerContext,
    session: &mut SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    handle(ctx, session, None, "invbroker", "Add", args, &HashMap::new())
        .await
        .unwrap()
}

pub(super) fn setup_inv_ctx() -> (ServerContext, SessionState, i64, i64, i64, i64) {
    let ctx = ServerContext::with_sde_and_repository(
        shared_sde(),
        Arc::new(WorldRepository::with_demo_seed()),
    );
    let char_id = 96_600_001i64;
    let corp_id = 98_600_001i64;
    let station_id = 60_003_760i64;
    let ship_id = 96_600_100i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().expect("seed char");
        ch.character_id = char_id;
        ch.user_id = 1;
        ch.name = "InvExtraPilot".into();
        ch.corporation_id = corp_id;
        ch.station_id = Some(station_id);
        ch.structure_id = None;
        ch.solar_system_id = 30_000_142;
        ch.active_ship_id = ship_id;
        snap.characters.insert(char_id, ch);

        snap.corporations.insert(
            corp_id,
            CorporationRecord {
                corporation_id: corp_id,
                name: "InvExtra Corp".into(),
                ticker: "IEXC".into(),
                ceo_id: 96_699_999,
                members: [char_id, 96_699_999].into_iter().collect(),
                ..Default::default()
            },
        );

        snap.items.insert(
            ship_id,
            mk_item(ship_id, 587, char_id, station_id, InventoryFlag::Hangar, 1, true),
        );

        for &sk in &[3300u32, 3302, 3318, 3392, 3426, 26252, 26253] {
            snap.skills.entry(char_id).or_default().insert(
                sk,
                TrainedSkillRecord {
                    type_id: sk,
                    level: 5,
                    skill_points: 256_000.0,
                },
            );
        }
    });

    let mut session = SessionState::default();
    session.user_id = Some(1);
    session.character_id = Some(char_id);
    session.corporation_id = Some(corp_id);
    session.station_id = Some(station_id);
    session.solar_system_id = Some(30_000_142);
    session.ship_id = Some(ship_id);
    (ctx, session, char_id, corp_id, station_id, ship_id)
}

#[tokio::test]
async fn invbroker_add_argument_flag_quantity_and_corp_role_validations() {
    let (ctx, mut session, char_id, corp_id, station_id, ship_id) = setup_inv_ctx();
    let kw = HashMap::new();
    assert!((0.0..1.0).contains(&csprng_unit_f64(&ctx)));

    // 1. Unauthenticated GetItemDescriptor & Add rejected
    let mut unauth = SessionState::default();
    assert_eq!(
        handle(&ctx, &mut unauth, None, "invbroker", "GetItemDescriptor", &[], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "Unauthorized"
    );
    assert_eq!(call_add(&ctx, &mut unauth, &[]).await.unwrap_err().code, "Unauthorized");

    // 2. Missing item_id -> InvalidArgument
    assert_eq!(call_add(&ctx, &mut session, &[]).await.unwrap_err().code, "InvalidArgument");

    // 3. Missing target location when session has no ship/station/structure -> InvalidLocation
    let mut bare_sess = SessionState::default();
    bare_sess.user_id = Some(1);
    bare_sess.character_id = Some(char_id);
    assert_eq!(
        call_add(&ctx, &mut bare_sess, &[PyRep::Integer(ship_id)])
            .await
            .unwrap_err()
            .code,
        "InvalidLocation"
    );

    // 4. Invalid inventory flag (999) and Fighter Bay (158) with non-fighter item (Tritanium 34)
    let trit_id = 96_600_201i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            trit_id,
            mk_item(trit_id, 34, char_id, station_id, InventoryFlag::Hangar, 100, false),
        );
    });
    for bad_flag in [999_i64, 158_i64] {
        assert_eq!(
            call_add(
                &ctx,
                &mut session,
                &[PyRep::Integer(trit_id), PyRep::Integer(ship_id), PyRep::Integer(bad_flag)],
            )
            .await
            .unwrap_err()
            .code,
            "InvalidInventoryFlag"
        );
    }

    // 5. InvalidQuantity (<= 0 or overflow i32)
    let mut bad_kw = HashMap::new();
    bad_kw.insert("quantity".into(), PyRep::Integer(0));
    assert_eq!(
        handle(
            &ctx,
            &mut session,
            None,
            "invbroker",
            "Add",
            &[PyRep::Integer(trit_id), PyRep::Integer(ship_id), PyRep::Integer(5)],
            &bad_kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidQuantity"
    );
    assert_eq!(
        call_add(
            &ctx,
            &mut session,
            &[
                PyRep::Integer(trit_id),
                PyRep::Integer(ship_id),
                PyRep::Integer(5),
                PyRep::Integer(i64::from(i32::MAX) + 10),
            ],
        )
        .await
        .unwrap_err()
        .code,
        "InvalidQuantity"
    );

    // 6. Taking from CorpSAG1 (116) without division 1 take role -> CorpRoleDenied, then with role -> succeeds
    let corp_item_id = 96_600_202i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            corp_item_id,
            mk_item(corp_item_id, 34, corp_id, station_id, InventoryFlag::CorpSAG1, 50, false),
        );
    });
    assert_eq!(
        call_add(
            &ctx,
            &mut session,
            &[PyRep::Integer(corp_item_id), PyRep::Integer(ship_id), PyRep::Integer(5)],
        )
        .await
        .unwrap_err()
        .code,
        "CorpRoleDenied"
    );

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(corp) = snap.corporations.get_mut(&corp_id) {
            corp.member_roles
                .insert(char_id, corp_roles::ROLE_DIRECTOR | (1u64 << 13));
        }
    });
    let bound = BoundObjectEntry {
        bound_id: 1,
        session_id: 1,
        user_id: 1,
        character_id: char_id,
        service: "invbroker".into(),
        object_id: ship_id,
        oid_str: "N=1:1".into(),
        ref_id: 1,
    };
    let moved_corp = handle(
        &ctx,
        &mut session,
        Some(&bound),
        "invbroker",
        "Add",
        &[PyRep::Integer(corp_item_id), PyRep::Integer(station_id), PyRep::Integer(5)],
        &kw,
    )
    .await
    .unwrap()
    .expect("director can take from CorpSAG1 into bound ship");
    assert_eq!(moved_corp.dict_get("locationID").and_then(|v| v.as_i64()), Some(ship_id));
}
