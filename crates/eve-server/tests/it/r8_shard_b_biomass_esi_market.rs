//! Round-8 Shard-B integration regression tests for `#519` and `#524`.

#![forbid(unsafe_code)]

use eve_db::{
    CorporationRecord, IndustryJobRecord, InventoryFlag, InventoryItemRecord, IskCents,
    UpwellStructureRecord,
};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::esi::{EsiHttpRequest, EsiRouter};
use eve_server::router::ServerContext;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

async fn call(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None)
        .await
}

#[tokio::test]
async fn r8b_519_524_biomass_docked_corp_job_asset_safety_and_esi_private_market_filter() {
    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);

    let victim_id = 90_519_001_i64;
    let heir_id = 90_000_001_i64;
    let corp_id = 98_519_001_i64;
    let jita_station = 60_003_760_i64;
    let amarr_station = 60_008_494_i64;
    let safety_item_id = 880_519_002_i64;
    let remote_item_id = 880_519_003_i64;
    let corp_bp_id = 880_519_004_i64;
    let personal_bp_id = 880_519_099_i64;
    let personal_job_id = 880_519_101_i64;
    let corp_job_id = 880_519_102_i64;
    let victim_ship_id = 880_519_010_i64;
    let private_struct_id = 1_030_524_001_001_i64;

    ctx.db.with_mut_snapshot(|s| {
        let mut v = s.characters[&heir_id].clone();
        v.character_id = victim_id;
        v.user_id = 1;
        v.name = "Biomass Runner".into();
        v.corporation_id = corp_id;
        v.station_id = None; // undocked in space initially!
        v.solar_system_id = 30_000_142;
        v.home_station_id = jita_station;
        v.active_ship_id = victim_ship_id;
        v.balance = IskCents::from_isk(5_000_000);
        s.characters.insert(victim_id, v);
        s.corporations.insert(
            corp_id,
            { let mut __corp = CorporationRecord::default();
                __corp.corporation_id = corp_id;
                __corp.name = "Biomass Test Corp".into();
                __corp.ticker = "BMTC".into();
                __corp.ceo_id = heir_id;
                __corp.members = HashSet::from([heir_id, victim_id]);
                __corp },
        );
        for (id, tid, owner, loc, flag) in [
            (880_519_000, 27, corp_id, jita_station, InventoryFlag::None),
            (victim_ship_id, 587, victim_id, 30_000_142, InventoryFlag::None),
            (safety_item_id, 34, victim_id, private_struct_id, InventoryFlag::Hangar),
            (remote_item_id, 35, victim_id, amarr_station, InventoryFlag::Hangar),
            (personal_bp_id, 688, victim_id, jita_station, InventoryFlag::Locked),
            (corp_bp_id, 688, corp_id, jita_station, InventoryFlag::Locked),
        ] {
            s.items.insert(
                id,
                InventoryItemRecord {
                    item_id: id,
                    type_id: tid,
                    owner_id: owner,
                    location_id: loc,
                    flag,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
        }
        let now = ctx.now_unix();
        s.industry_jobs.insert(
            personal_job_id,
            IndustryJobRecord {
                job_id: personal_job_id,
                installer_id: victim_id,
                facility_id: jita_station,
                activity_id: 1,
                blueprint_item_id: personal_bp_id,
                blueprint_type_id: 688,
                product_type_id: 587,
                runs: 1,
                status: "Active".into(),
                start_unix: now,
                duration_seconds: 3600,
                ..Default::default()
            },
        );
        s.industry_jobs.insert(
            corp_job_id,
            IndustryJobRecord {
                job_id: corp_job_id,
                installer_id: victim_id,
                facility_id: jita_station,
                activity_id: 1,
                blueprint_item_id: corp_bp_id,
                blueprint_type_id: 688,
                product_type_id: 587,
                runs: 1,
                status: "Active".into(),
                start_unix: now,
                duration_seconds: 3600,
                ..Default::default()
            },
        );
        s.structures.insert(
            private_struct_id,
            UpwellStructureRecord {
                structure_id: private_struct_id,
                type_id: 35_833,
                owner_corp_id: corp_id,
                solar_system_id: 30_000_142,
                name: "Private Citadel Market".into(),
                position: [1000.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 500,
                shield_hp: 10_000.0,
                armor_hp: 10_000.0,
                hull_hp: 10_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
        s.items.insert(
            880_524_001,
            InventoryItemRecord {
                item_id: 880_524_001,
                type_id: 35_892,
                owner_id: corp_id,
                location_id: private_struct_id,
                flag: InventoryFlag::ServiceSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });

    let mut victim_sess = SessionState::new(8400);
    victim_sess.user_id = Some(1);
    victim_sess.character_id = Some(victim_id);
    victim_sess.corporation_id = Some(corp_id);
    victim_sess.solar_system_id = Some(30_000_142);
    let wrap_rep = call(
        &ctx,
        &mut victim_sess,
        "structureAssetSafety",
        "MovePersonalAssetsToSafety",
        &[PyRep::Integer(30_000_142), PyRep::Integer(private_struct_id)],
    )
    .await
    .expect("create personal Asset Safety wrap");
    let wrap_id = wrap_rep.dict_get("wrapID").and_then(|v| v.as_i64()).expect("wrapID");
    // Ensure victim has no active bound in-space session when testing docked biomass.
    victim_sess.character_id = None;

    let mut unbound_sess = SessionState::new(8401);
    unbound_sess.user_id = Some(1);
    let prep_err = call(
        &ctx,
        &mut unbound_sess,
        "charUnboundMgr",
        "PrepareCharacterForDelete",
        &[PyRep::Integer(victim_id)],
    )
    .await
    .unwrap_err();
    // rpc-lint: allow error code check
    assert_eq!(prep_err.code, "CharacterMustBeDocked");

    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&victim_id).unwrap().station_id = Some(jita_station);
        s.items.get_mut(&victim_ship_id).unwrap().location_id = jita_station;
        s.items.get_mut(&victim_ship_id).unwrap().flag = InventoryFlag::Hangar;
    });
    call(
        &ctx,
        &mut unbound_sess,
        "charUnboundMgr",
        "PrepareCharacterForDelete",
        &[PyRep::Integer(victim_id)],
    )
    .await
    .expect("PrepareCharacterForDelete while docked");
    ctx.advance_virtual_clock(36_001);

    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&victim_id).unwrap().station_id = None;
    });
    let del_err = call(&ctx, &mut unbound_sess, "charUnboundMgr", "DeleteCharacter", &[PyRep::Integer(victim_id)])
        .await
        .unwrap_err();
    // rpc-lint: allow error code check
    assert_eq!(del_err.code, "CharacterMustBeDocked");

    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&victim_id).unwrap().station_id = Some(jita_station);
    });
    call(&ctx, &mut unbound_sess, "charUnboundMgr", "DeleteCharacter", &[PyRep::Integer(victim_id)])
        .await
        .expect("DeleteCharacter while docked");

    ctx.db.with_snapshot(|s| {
        assert_eq!(s.characters[&victim_id].corporation_id, 1_000_001);
        assert_eq!(s.characters[&victim_id].user_id, 0);
        assert!(!s.industry_jobs.contains_key(&personal_job_id));
        assert!(s.industry_jobs.contains_key(&corp_job_id));
        assert_eq!(s.items[&corp_bp_id].flag, InventoryFlag::Locked);
        assert_eq!(s.items[&remote_item_id].location_id, amarr_station);
        assert_eq!(s.items[&remote_item_id].flag, InventoryFlag::Impounded);
        assert_eq!(s.items[&safety_item_id].location_id, wrap_id);
        assert_eq!(s.items[&safety_item_id].flag, InventoryFlag::Locked);
        assert_eq!(s.items[&safety_item_id].owner_id, corp_id);
    });

    ctx.db.with_mut_snapshot(|s| {
        s.market_orders.clear();
        let h = s.characters.get_mut(&heir_id).unwrap();
        h.corporation_id = corp_id;
        h.station_id = Some(jita_station);
        h.solar_system_id = 30_000_142;
        h.balance = IskCents::from_isk(100_000_000);
    });
    let mut market_sess = SessionState::new(8402);
    market_sess.user_id = Some(1);
    market_sess.character_id = Some(heir_id);
    market_sess.corporation_id = Some(corp_id);
    market_sess.station_id = Some(jita_station);
    market_sess.solar_system_id = Some(30_000_142);
    market_sess.region_id = Some(10_000_002);

    call(
        &ctx,
        &mut market_sess,
        "marketProxy",
        "PlaceBuyOrder",
        &[
            PyRep::Integer(jita_station),
            PyRep::Integer(36),
            PyRep::Real(50.0),
            PyRep::Integer(10),
            PyRep::Integer(-1),
            PyRep::Integer(1),
            PyRep::Integer(14),
            PyRep::Bool(false),
        ],
    )
    .await
    .expect("NPC station buy order");

    ctx.db.with_mut_snapshot(|s| {
        let h = s.characters.get_mut(&heir_id).unwrap();
        h.station_id = None;
        h.structure_id = Some(private_struct_id);
    });
    market_sess.station_id = None;
    market_sess.structure_id = Some(private_struct_id);
    call(
        &ctx,
        &mut market_sess,
        "marketProxy",
        "PlaceBuyOrder",
        &[
            PyRep::Integer(private_struct_id),
            PyRep::Integer(11_399),
            PyRep::Real(5_000.0),
            PyRep::Integer(5),
            PyRep::Integer(-1),
            PyRep::Integer(1),
            PyRep::Integer(14),
            PyRep::Bool(false),
        ],
    )
    .await
    .expect("private citadel buy order");

    let router = EsiRouter::new(Arc::clone(&ctx));
    let orders_resp =
        router.handle_request(&EsiHttpRequest::new("GET", "/v1/markets/10000002/orders/?order_type=all"));
    assert_eq!(orders_resp.status_code, 200);
    let orders_json: Value = serde_json::from_slice(&orders_resp.body).unwrap();
    let arr = orders_json.as_array().unwrap();
    assert!(arr.iter().any(|o| o["location_id"] == json!(jita_station) && o["type_id"] == json!(36)));
    assert!(
        !arr.iter().any(|o| o["location_id"] == json!(private_struct_id) || o["type_id"] == json!(11_399)),
        "unauthenticated region /orders must never leak private Upwell structure orders"
    );

    let types_resp = router.handle_request(&EsiHttpRequest::new("GET", "/v1/markets/10000002/types/"));
    assert_eq!(types_resp.status_code, 200);
    let types_json: Value = serde_json::from_slice(&types_resp.body).unwrap();
    let t_arr = types_json.as_array().unwrap();
    assert!(t_arr.contains(&json!(36)));
    assert!(!t_arr.contains(&json!(11_399)));
}
