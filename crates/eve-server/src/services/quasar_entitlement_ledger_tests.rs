//! Unit and mutation-killing tests for `quasar_entitlement_ledger` (`Q-6`).

use super::*;
use crate::quasar_gateway::{
    PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_FORBIDDEN,
};
use eve_db::{InventoryFlag, WorldRepository};
use eve_proto::pb;
use eve_proto::quasar::{
    AuthoritativeContext, GoogleAny, QuasarDescriptorRegistry, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_sde::SdeRegistry;
use std::sync::{Arc, OnceLock};

const ROLE_PROGRAMMER: u64 = eve_machonet::service_roles::ROLE_GM_MASK;

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

trait TestRouterExt {
    fn dispatch(
        &self,
        ctx: &AuthoritativeContext,
        req: &GoogleAny,
    ) -> Result<GoogleAny, (u32, String)>;
}
impl TestRouterExt for PublicGatewayRouter {
    fn dispatch(
        &self,
        ctx: &AuthoritativeContext,
        req: &GoogleAny,
    ) -> Result<GoogleAny, (u32, String)> {
        let norm = req.normalized_type_name();
        let handler = self
            .handlers
            .read()
            .unwrap()
            .get(norm)
            .cloned()
            .expect("registered handler");
        let dyn_msg = self
            .descriptors
            .decode_dynamic_or_wire(norm, &req.value)
            .unwrap_or_else(|_| {
                crate::services::quasar_char_skill_services::decode_wire_dynamic(norm, &req.value)
            });
        handler(ctx, req, &dyn_msg)
    }
}

fn make_router_and_db() -> (Arc<PublicGatewayRouter>, Arc<WorldRepository>, Arc<SdeRegistry>) {
    let db = Arc::new(WorldRepository::with_demo_seed());
    let sde = shared_sde();
    let router = Arc::new(PublicGatewayRouter::new(
        QuasarDescriptorRegistry::open_default_or_empty(),
        Arc::clone(&db),
        Arc::clone(&sde),
    ));
    (router, db, sde)
}

fn seed_char(db: &WorldRepository, char_id: i64) {
    db.with_mut_snapshot(|s| {
        let mut rec = s.characters[&90_000_001].clone();
        rec.character_id = char_id;
        s.characters.insert(char_id, rec);
    });
}

fn make_ctx(char_id: Option<u32>, role: u64) -> AuthoritativeContext {
    AuthoritativeContext::for_session_at(
        Some(1),
        char_id,
        None,
        0,
        "tranquility".to_string(),
        b"evomu-quasar-gw1".to_vec(),
        1_760_000_000,
    )
    .with_service_roles(role)
}

fn encode_entitlement_id(holder: u32, asset_uuid: &[u8; 16], group_name: &str) -> Vec<u8> {
    pb::message("eve_public.assetholding.entitlement.Identifier")
        .set("holder.character.sequential", holder)
        .set("asset.uuid", asset_uuid)
        .set("group_by.name", group_name)
        .must_encode()
}

fn extract_spawned_uuid(resp: &GoogleAny) -> [u8; 16] {
    let dyn_msg =
        crate::services::quasar_char_skill_services::decode_wire_dynamic("test.Msg", &resp.value);
    bytes16(sub(&dyn_msg, 1), 1).expect("uuid in field 1.1")
}

#[test]
fn spawn_hold_create_and_encode_asset_attributes() {
    // parity: feat:quasar.assetholding
    let (router, _db, _sde) = make_router_and_db();
    let cid = 211200200u32;
    let user = make_ctx(Some(cid), 0);
    let admin = make_ctx(Some(cid), ROLE_PROGRAMMER);

    // Direct test of encode_asset_attributes for public & internal across all variants
    let a_stack = AssetRecord {
        uuid: [1u8; 16],
        name: "stack_asset".to_string(),
        stacks: vec![(34, 100)],
        character_title_id: None,
        ship_skin_license_hex: None,
        capacity: Some((10, 8)),
        benefactor_char_id: Some(cid),
    };
    assert!(!encode_asset_attributes(&a_stack, true).is_empty());
    assert!(!encode_asset_attributes(&a_stack, false).is_empty());

    let a_skin = AssetRecord {
        uuid: [2u8; 16],
        name: "skin_asset".to_string(),
        stacks: vec![],
        character_title_id: None,
        ship_skin_license_hex: Some("deadbeef".to_string()),
        capacity: None,
        benefactor_char_id: None,
    };
    assert!(!encode_asset_attributes(&a_skin, true).is_empty());
    assert!(!encode_asset_attributes(&a_skin, false).is_empty());

    let a_title = AssetRecord {
        uuid: [3u8; 16],
        name: "title_asset".to_string(),
        stacks: vec![],
        character_title_id: Some(77),
        ship_skin_license_hex: None,
        capacity: None,
        benefactor_char_id: None,
    };
    assert!(!encode_asset_attributes(&a_title, false).is_empty());

    // 1. SpawnRequest validation & happy paths (itemstack + character_title)
    let spawn_type = format!("{TYPE_GOOGLEAPIS_PREFIX}eve.assetholding.asset.api.SpawnRequest");
    assert_eq!(
        router
            .dispatch(&user, &GoogleAny { type_url: spawn_type.clone(), value: vec![] })
            .unwrap_err()
            .0,
        STATUS_FORBIDDEN
    );
    assert_eq!(
        router
            .dispatch(&admin, &GoogleAny { type_url: spawn_type.clone(), value: vec![] })
            .unwrap_err()
            .0,
        STATUS_BAD_REQUEST
    );

    // Spawn itemstack (type 34, qty 500)
    let spawn_stack_bytes = pb::message("eve.assetholding.asset.api.SpawnRequest")
        .set("internal_name", "tritanium_pack")
        .add_with("itemstack.stacks", |s| {
            s.set("type.sequential", 34u64).set("quantity", 500u32)
        })
        .must_encode();
    let spawn_resp1 = router
        .dispatch(
            &admin,
            &GoogleAny {
                type_url: spawn_type.clone(),
                value: spawn_stack_bytes,
            },
        )
        .expect("Spawn itemstack");
    let uuid1 = extract_spawned_uuid(&spawn_resp1);
    assert_eq!(&uuid1[0..8], b"ASSETHLD");

    // Spawn character_title with finite capacity = 5
    let mut title_uuid = [0u8; 16];
    title_uuid[8..16].copy_from_slice(&42u64.to_be_bytes());
    let spawn_title_bytes = pb::message("eve.assetholding.asset.api.SpawnRequest")
        .set("character_title.title.uuid", title_uuid)
        .set("character_title.finite", 5u64)
        .must_encode();
    let spawn_resp2 = router
        .dispatch(
            &admin,
            &GoogleAny {
                type_url: spawn_type,
                value: spawn_title_bytes,
            },
        )
        .expect("Spawn character_title");
    let _uuid2 = extract_spawned_uuid(&spawn_resp2);

    // 2. HoldRequest (ship_skin_license and itemstack)
    let hold_type = format!("{TYPE_GOOGLEAPIS_PREFIX}eve.assetholding.asset.api.HoldRequest");
    assert_eq!(
        router
            .dispatch(&admin, &GoogleAny { type_url: hold_type.clone(), value: vec![] })
            .unwrap_err()
            .0,
        STATUS_BAD_REQUEST
    );

    let hold_skin_bytes = pb::message("eve.assetholding.asset.api.HoldRequest")
        .set("ship_skin_license.license.skin.hex", "skin_hex_01")
        .set("ship_skin_license.capacity", 3u64)
        .must_encode();
    let hold_resp1 = router
        .dispatch(
            &admin,
            &GoogleAny {
                type_url: hold_type.clone(),
                value: hold_skin_bytes,
            },
        )
        .expect("Hold skin license");
    let _uuid3 = extract_spawned_uuid(&hold_resp1);

    let hold_stack_bytes = pb::message("eve.assetholding.asset.api.HoldRequest")
        .add_with("itemstack.by_typed_quantities.items", |s| {
            s.set("type.sequential", 34u64).set("quantity", 500u32)
        })
        .must_encode();
    let hold_resp2 = router
        .dispatch(
            &admin,
            &GoogleAny {
                type_url: hold_type,
                value: hold_stack_bytes,
            },
        )
        .expect("Hold itemstack");
    let _uuid4 = extract_spawned_uuid(&hold_resp2);

    // 3. CreateRequest (unit 8 itemstack, unit 10 skin, unit 12 title)
    let create_type = format!("{TYPE_GOOGLEAPIS_PREFIX}eve.assetholding.asset.api.CreateRequest");
    assert_eq!(
        router
            .dispatch(&admin, &GoogleAny { type_url: create_type.clone(), value: vec![] })
            .unwrap_err()
            .0,
        STATUS_BAD_REQUEST
    );

    let create8 = pb::message("eve.assetholding.asset.api.CreateRequest")
        .add_with("unit.unit_inventory_stack.stacks", |s| {
            s.set("type.sequential", 34u64).set("quantity", 500u32)
        })
        .set("finite", 10u64)
        .must_encode();
    assert!(router
        .dispatch(&admin, &GoogleAny { type_url: create_type.clone(), value: create8 })
        .is_ok());

    let create10 = pb::message("eve.assetholding.asset.api.CreateRequest")
        .set("unit.unit_ship_skin_license.skin.hex", "skin_hex_01")
        .must_encode();
    assert!(router
        .dispatch(&admin, &GoogleAny { type_url: create_type.clone(), value: create10 })
        .is_ok());

    let create12 = pb::message("eve.assetholding.asset.api.CreateRequest")
        .set("unit.unit_character_title.title.uuid", title_uuid)
        .must_encode();
    assert!(router
        .dispatch(&admin, &GoogleAny { type_url: create_type, value: create12 })
        .is_ok());
}

#[test]
fn direct_delivery_get_redeem_delete_expire_autoredeem_and_redeliver() {
    // parity: feat:quasar.assetholding
    let (router, db, _sde) = make_router_and_db();
    let holder = 211200210u32;
    let other = 211200211u32;
    seed_char(&db, holder as i64);
    seed_char(&db, other as i64);

    let holder_ctx = make_ctx(Some(holder), 0);
    let other_ctx = make_ctx(Some(other), 0);
    let admin = make_ctx(Some(holder), ROLE_PROGRAMMER);

    // Spawn 3 assets: (a) itemstack, (b) character_title with finite cap 2, (c) skin_license
    let spawn_type = format!("{TYPE_GOOGLEAPIS_PREFIX}eve.assetholding.asset.api.SpawnRequest");
    let sp_stack = pb::message("eve.assetholding.asset.api.SpawnRequest")
        .add_with("itemstack.stacks", |s| {
            s.set("type.sequential", 34u64).set("quantity", 250u32)
        })
        .must_encode();
    let asset_stack = extract_spawned_uuid(
        &router
            .dispatch(&admin, &GoogleAny { type_url: spawn_type.clone(), value: sp_stack })
            .unwrap(),
    );

    let mut title_uuid = [0u8; 16];
    title_uuid[8..16].copy_from_slice(&99u64.to_be_bytes());
    let sp_title = pb::message("eve.assetholding.asset.api.SpawnRequest")
        .set("character_title.title.uuid", title_uuid)
        .set("character_title.finite", 2u64)
        .must_encode();
    let asset_title = extract_spawned_uuid(
        &router
            .dispatch(&admin, &GoogleAny { type_url: spawn_type, value: sp_title })
            .unwrap(),
    );

    let hold_type = format!("{TYPE_GOOGLEAPIS_PREFIX}eve.assetholding.asset.api.HoldRequest");
    let hold_skin = pb::message("eve.assetholding.asset.api.HoldRequest")
        .set("ship_skin_license.license.skin.hex", "skin_abc")
        .set("ship_skin_license.capacity", 5u64)
        .must_encode();
    let asset_skin = extract_spawned_uuid(
        &router
            .dispatch(&admin, &GoogleAny { type_url: hold_type, value: hold_skin })
            .unwrap(),
    );

    // DirectDeliveryRequest
    let dd_type =
        format!("{TYPE_GOOGLEAPIS_PREFIX}eve.assetholding.entitlement.api.DirectDeliveryRequest");
    assert_eq!(
        router
            .dispatch(&admin, &GoogleAny { type_url: dd_type.clone(), value: vec![] })
            .unwrap_err()
            .0,
        STATUS_BAD_REQUEST
    );

    let deliver_one = |asset: &[u8; 16], qty: u64| -> Vec<u8> {
        let ent_id = encode_entitlement_id(holder, asset, "grp1");
        pb::message("eve.assetholding.entitlement.api.DirectDeliveryRequest")
            .add_with("entitlements", |e| {
                e.set_raw_message("entitlement", ent_id)
                    .set("quantity", qty)
            })
            .must_encode()
    };

    // Deliver exceeding finite capacity (asset_title has finite cap 2; delivering 3 -> BAD_REQUEST)
    assert_eq!(
        router
            .dispatch(
                &admin,
                &GoogleAny {
                    type_url: dd_type.clone(),
                    value: deliver_one(&asset_title, 3),
                }
            )
            .unwrap_err()
            .0,
        STATUS_BAD_REQUEST
    );

    // Deliver valid quantities for all 3 assets
    for (asset, qty) in [(&asset_stack, 2u64), (&asset_title, 2u64), (&asset_skin, 3u64)] {
        router
            .dispatch(
                &admin,
                &GoogleAny {
                    type_url: dd_type.clone(),
                    value: deliver_one(asset, qty),
                },
            )
            .expect("DirectDelivery");
    }

    // GetRequest (public and internal)
    let get_stack_bytes = pb::message("eve_public.assetholding.entitlement.api.GetRequest")
        .set_raw_message("entitlement", encode_entitlement_id(holder, &asset_stack, "grp1"))
        .must_encode();
    let pub_get = GoogleAny {
        type_url: format!(
            "{TYPE_GOOGLEAPIS_PREFIX}eve_public.assetholding.entitlement.api.GetRequest"
        ),
        value: get_stack_bytes.clone(),
    };
    let int_get = GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.assetholding.entitlement.api.GetRequest"),
        value: get_stack_bytes.clone(),
    };
    assert_eq!(router.dispatch(&other_ctx, &pub_get).unwrap_err().0, STATUS_FORBIDDEN);
    assert!(!router.dispatch(&holder_ctx, &pub_get).unwrap().value.is_empty());
    assert!(!router.dispatch(&holder_ctx, &int_get).unwrap().value.is_empty());

    // RedeliverStaleEntitlementsRequest
    let redeliv = GoogleAny {
        type_url: format!(
            "{TYPE_GOOGLEAPIS_PREFIX}eve.assetholding.entitlement.api.admin.RedeliverStaleEntitlementsRequest"
        ),
        value: vec![],
    };
    assert!(router.dispatch(&admin, &redeliv).is_ok());

    // RedeemRequest on asset_stack -> materializes 2 * 250 = 500 Tritanium in holder's hangar
    let pub_red = GoogleAny {
        type_url: format!(
            "{TYPE_GOOGLEAPIS_PREFIX}eve_public.assetholding.entitlement.api.RedeemRequest"
        ),
        value: get_stack_bytes.clone(),
    };
    router.dispatch(&holder_ctx, &pub_red).expect("Redeem stack");
    // Redeeming again -> BAD_REQUEST (nothing left to redeem)
    assert_eq!(
        router.dispatch(&holder_ctx, &pub_red).unwrap_err().0,
        STATUS_BAD_REQUEST
    );

    let trit_qty: i32 = db.with_snapshot(|snap| {
        snap.items
            .values()
            .filter(|i| i.owner_id == holder as i64 && i.type_id == 34 && i.flag == InventoryFlag::Hangar)
            .map(|i| i.quantity)
            .sum()
    });
    assert_eq!(trit_qty, 500);

    // Non-admin DeleteRequest on already-redeemed entitlement -> FORBIDDEN
    let del_req = GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.assetholding.entitlement.api.DeleteRequest"),
        value: get_stack_bytes.clone(),
    };
    assert_eq!(router.dispatch(&holder_ctx, &del_req).unwrap_err().0, STATUS_FORBIDDEN);
    // Admin ExpireRequest succeeds
    let exp_req = GoogleAny {
        type_url: format!(
            "{TYPE_GOOGLEAPIS_PREFIX}eve.assetholding.entitlement.api.admin.ExpireRequest"
        ),
        value: get_stack_bytes,
    };
    assert!(router.dispatch(&admin, &exp_req).is_ok());

    // AutoRedeemRequest redeems remaining pending entitlements (asset_title + asset_skin)
    let auto_req = GoogleAny {
        type_url: format!(
            "{TYPE_GOOGLEAPIS_PREFIX}eve_public.assetholding.entitlement.api.admin.AutoRedeemRequest"
        ),
        value: vec![],
    };
    assert!(router.dispatch(&admin, &auto_req).is_ok());

    assert_eq!(redeemed_titles_for_char(&db, holder), vec![99]);
    assert_eq!(
        redeemed_skin_licenses_for_char(&db, holder),
        vec![("skin_abc".to_string(), 3)]
    );
}

#[cfg(test)]
#[path = "quasar_entitlement_ledger_extra_unit_tests.rs"]
mod quasar_entitlement_ledger_extra_unit_tests;
