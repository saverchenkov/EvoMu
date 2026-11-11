//! Additional branch-coverage and mutation-killing unit tests for `quasar_corp_structure_services.rs`.

#![forbid(unsafe_code)]

use crate::quasar_gateway::{STATUS_BAD_REQUEST, STATUS_NOT_FOUND};
use crate::services::fleet_social::fleet_social_fleet_ops::fleet_social_fleet_ops_tests::test_ctx_and_clock;
use crate::services::quasar_corp_structure_services::*;
use crate::services::quasar_cosmetics_market_types::{
    CosmeticsMarketData, ListingTargetRecord, SkinListingRecord,
};
use eve_proto::quasar::{AuthoritativeContext, DynamicQuasarMessage, DynamicValue, GoogleAny};

fn make_tag_msg(tag: &str) -> DynamicValue {
    let mut m = DynamicQuasarMessage::new("Tag");
    m.fields.insert(1, DynamicValue::String(tag.to_string()));
    DynamicValue::Message(Box::new(m))
}

#[test]
fn semver_encoding_dyn_extractors_and_dry_run_ruleset_tags_kill_mutations() {
    // 1. SemVerSpec::default, encode with 0 vs >0 fields, and from_dyn across integer variants
    let def = SemVerSpec::default();
    assert_eq!(
        def,
        SemVerSpec {
            major: 1,
            minor: 0,
            patch: 0
        }
    );
    let zero_ver = SemVerSpec {
        major: 0,
        minor: 0,
        patch: 0,
    };
    assert!(zero_ver.encode().is_empty());

    let minor_only = SemVerSpec {
        major: 0,
        minor: 3,
        patch: 0,
    };
    assert!(!minor_only.encode().is_empty());

    let patch_only = SemVerSpec {
        major: 0,
        minor: 0,
        patch: 5,
    };
    assert!(!patch_only.encode().is_empty());

    let mut dyn_ver = DynamicQuasarMessage::new("SemVer");
    dyn_ver.fields.insert(1, DynamicValue::Int64(4));
    dyn_ver.fields.insert(2, DynamicValue::Sint32(7));
    let mut nested = DynamicQuasarMessage::new("Sub");
    nested.fields.insert(1, DynamicValue::Enum(9));
    dyn_ver
        .fields
        .insert(3, DynamicValue::Message(Box::new(nested)));
    assert_eq!(
        SemVerSpec::from_dyn(&dyn_ver),
        SemVerSpec {
            major: 4,
            minor: 7,
            patch: 9
        }
    );

    let (ctx, _clock) = test_ctx_and_clock(1_700_000_000);
    let handlers = ctx.quasar_gateway.handlers.read().unwrap();
    let admin_ctx = AuthoritativeContext::for_session(
        Some(1),
        Some(100_000_001),
        Some(1_000_009),
        u64::MAX,
        "tranquility",
        vec![1, 2],
    );
    let dummy_any = GoogleAny::default();
    let empty_msg = DynamicQuasarMessage::new("Empty");

    // 2. SetDefaultTenantVersionRequest & SetTenantVersionRequest error and use_default branches
    let set_def_fn =
        &handlers["eve.content.orchestrator.admin.api.SetDefaultTenantVersionRequest"];
    assert_eq!(
        set_def_fn(&admin_ctx, &dummy_any, &empty_msg)
            .unwrap_err()
            .0,
        STATUS_BAD_REQUEST
    );

    let set_ten_fn = &handlers["eve.content.orchestrator.admin.api.SetTenantVersionRequest"];
    assert_eq!(
        set_ten_fn(&admin_ctx, &dummy_any, &empty_msg)
            .unwrap_err()
            .0,
        STATUS_BAD_REQUEST
    );

    // Set custom tenant version, then reset to default via field 1 = Uint64(1)
    let mut set_custom = DynamicQuasarMessage::new("SetTen");
    set_custom
        .fields
        .insert(2, DynamicValue::Message(Box::new(dyn_ver)));
    assert!(set_ten_fn(&admin_ctx, &dummy_any, &set_custom).is_ok());

    let get_ten_fn = &handlers["eve.content.orchestrator.api.GetTenantVersionRequest"];
    let custom_resp = get_ten_fn(&admin_ctx, &dummy_any, &empty_msg).unwrap();

    let mut set_use_def = DynamicQuasarMessage::new("SetTenDef");
    set_use_def.fields.insert(1, DynamicValue::Uint64(1));
    assert!(set_ten_fn(&admin_ctx, &dummy_any, &set_use_def).is_ok());
    let def_resp = get_ten_fn(&admin_ctx, &dummy_any, &empty_msg).unwrap();
    assert_ne!(custom_resp.value, def_resp.value);

    // 3. DryRunRulesetRequest validation & tag matchers (highsec, lowsec, nullsec, wormhole, region:, constellation:, sid:, raw id)
    let dry_fn = &handlers["eve.content.orchestrator.api.DryRunRulesetRequest"];
    assert_eq!(
        dry_fn(&admin_ctx, &dummy_any, &empty_msg).unwrap_err().0,
        STATUS_BAD_REQUEST
    );

    let mut empty_rs_req = DynamicQuasarMessage::new("DryRun");
    empty_rs_req
        .fields
        .insert(1, DynamicValue::Message(Box::new(empty_msg.clone())));
    assert_eq!(
        dry_fn(&admin_ctx, &dummy_any, &empty_rs_req).unwrap_err().0,
        STATUS_BAD_REQUEST
    );

    // Non-matching tag -> STATUS_NOT_FOUND
    let mut unmatched_rs = DynamicQuasarMessage::new("Ruleset");
    unmatched_rs
        .fields
        .insert(1, make_tag_msg("sid:999999999"));
    let mut unmatched_req = DynamicQuasarMessage::new("DryRun");
    unmatched_req
        .fields
        .insert(1, DynamicValue::Message(Box::new(unmatched_rs)));
    assert_eq!(
        dry_fn(&admin_ctx, &dummy_any, &unmatched_req).unwrap_err().0,
        STATUS_NOT_FOUND
    );

    // Repeated inc_any (field 2) + exc_all (field 3) + exc_any (field 4) covering all tag formats
    let mut multi_rs = DynamicQuasarMessage::new("Ruleset");
    multi_rs.fields.insert(
        2,
        DynamicValue::Repeated(vec![
            make_tag_msg("lowsec"),
            make_tag_msg("nullsec"),
            make_tag_msg("wormhole"),
            make_tag_msg("region:10000002"),
            make_tag_msg("constellation:20000020"),
            make_tag_msg("sid:30000142"),
            make_tag_msg("30000142"),
            make_tag_msg("   "), // empty tag ignored
        ]),
    );
    multi_rs
        .fields
        .insert(3, make_tag_msg("nonexistent_exc_all"));
    multi_rs
        .fields
        .insert(4, make_tag_msg("nonexistent_exc_any"));
    let mut multi_req = DynamicQuasarMessage::new("DryRun");
    multi_req
        .fields
        .insert(1, DynamicValue::Message(Box::new(multi_rs)));
    let dry_ok = dry_fn(&admin_ctx, &dummy_any, &multi_req).unwrap();
    assert!(!dry_ok.value.is_empty());
}

#[test]
fn orchestrator_deployments_instances_and_commerce_admin_error_and_retry_paths_kill_mutations() {
    let (ctx, _clock) = test_ctx_and_clock(1_700_000_000);
    let handlers = ctx.quasar_gateway.handlers.read().unwrap();
    let db = &ctx.db;
    let admin_ctx = AuthoritativeContext::for_session(
        Some(1),
        Some(100_000_001),
        Some(1_000_009),
        u64::MAX,
        "tranquility",
        vec![1, 2],
    );
    let dummy_any = GoogleAny::default();
    let empty_msg = DynamicQuasarMessage::new("Empty");

    // 1. Deployment / ContentGroup / ContentType / Instance NOT_FOUND and BAD_REQUEST paths
    for name in [
        "eve.content.orchestrator.deployment.api.GetRequest",
        "eve.content.orchestrator.deployment.api.EnableRequest",
        "eve.content.orchestrator.deployment.api.DisableRequest",
        "eve.content.orchestrator.deployment.contentgroup.api.GetRequest",
        "eve.content.orchestrator.deployment.contenttype.api.GetRequest",
    ] {
        let mut req = DynamicQuasarMessage::new("Req");
        req.fields.insert(1, DynamicValue::Uint64(999_999));
        assert_eq!(
            handlers[name](&admin_ctx, &dummy_any, &req).unwrap_err().0,
            STATUS_NOT_FOUND,
            "{name} should return NOT_FOUND"
        );
    }

    for name in [
        "eve.content.orchestrator.deployment.instance.api.GetRequest",
        "eve.content.orchestrator.deployment.instance.admin.api.RemoveRequest",
    ] {
        assert_eq!(
            handlers[name](&admin_ctx, &dummy_any, &empty_msg)
                .unwrap_err()
                .0,
            STATUS_BAD_REQUEST,
            "{name} with empty uuid should return BAD_REQUEST"
        );
        let mut unknown_uuid = DynamicQuasarMessage::new("Uuid");
        unknown_uuid
            .fields
            .insert(1, DynamicValue::Bytes(vec![9, 9, 9]));
        assert_eq!(
            handlers[name](&admin_ctx, &dummy_any, &unknown_uuid)
                .unwrap_err()
                .0,
            STATUS_NOT_FOUND,
            "{name} with unknown uuid should return NOT_FOUND"
        );
    }

    // Seed two instances on different deployments and test ListRequest with dep_id == 0 vs dep_id == 10
    let mut orch = OrchestratorState::default();
    orch.instances.push(DeploymentInstanceRecord {
        uuid: vec![10, 1],
        deployment_id: 10,
        content_type_id: 1,
        solar_system_id: 30_000_142,
        constellation_id: 20_000_020,
        created_unix: 1_700_000_000,
    });
    orch.instances.push(DeploymentInstanceRecord {
        uuid: vec![20, 2],
        deployment_id: 20,
        content_type_id: 1,
        solar_system_id: 30_000_142,
        constellation_id: 20_000_020,
        created_unix: 1_700_000_000,
    });
    db.with_mut_snapshot(|snap| {
        snap.quasar_commerce.orchestrator = orch;
    });
    let list_inst_fn = &handlers["eve.content.orchestrator.deployment.instance.api.ListRequest"];
    let all_inst = list_inst_fn(&admin_ctx, &dummy_any, &empty_msg).unwrap();
    let mut dep10_req = DynamicQuasarMessage::new("Dep10");
    dep10_req.fields.insert(1, DynamicValue::Uint64(10));
    let filtered_inst = list_inst_fn(&admin_ctx, &dummy_any, &dep10_req).unwrap();
    assert!(all_inst.value.len() > filtered_inst.value.len());

    // 2. GetAllListedByCharacterRequest: creator_id == 0 -> BAD_REQUEST; seeded skin listing filters by seller_id
    let skin_admin_fn =
        &handlers["eve.cosmetic.market.skin.listing.api.admin.GetAllListedByCharacterRequest"];
    assert_eq!(
        skin_admin_fn(&admin_ctx, &dummy_any, &empty_msg)
            .unwrap_err()
            .0,
        STATUS_BAD_REQUEST
    );

    let seller_cid = 90_000_001_u32;
    db.with_mut_snapshot(|snap| {
        let mut mkt = CosmeticsMarketData::default();
        mkt.skin_listings.insert(
            "07".repeat(16),
            SkinListingRecord {
                uuid: [7u8; 16],
                seller_id: seller_cid,
                created_unix: 1_700_000_000,
                expires_unix: 1_700_086_400,
                price_isk: Some(10_000_000),
                price_plex_cents: None,
                skin_hex: "deadbeef".into(),
                quantity: 3,
                target: ListingTargetRecord::Public,
                branded: false,
            },
        );
        snap.quasar_state_blobs.insert(
            "quasar_cosmetics_market".into(),
            serde_json::to_string(&mkt).unwrap(),
        );
    });
    let mut seller_req = DynamicQuasarMessage::new("Seller");
    seller_req
        .fields
        .insert(1, DynamicValue::Uint64(u64::from(seller_cid)));
    let skin_resp = skin_admin_fn(&admin_ctx, &dummy_any, &seller_req).unwrap();
    assert!(!skin_resp.value.is_empty());

    // 3. RetryFailedFulfillmentRequest & DropFailedFulfillmentRequest: NOT_FOUND and idempotent retry (no double PLEX credit)
    let retry_fn = &handlers["eve.store.sale.admin.RetryFailedFulfillmentRequest"];
    let drop_fn = &handlers["eve.store.sale.admin.DropFailedFulfillmentRequest"];
    let mut missing_fid = DynamicQuasarMessage::new("Fid");
    missing_fid.fields.insert(1, DynamicValue::Uint64(404));
    assert_eq!(
        retry_fn(&admin_ctx, &dummy_any, &missing_fid)
            .unwrap_err()
            .0,
        STATUS_NOT_FOUND
    );
    assert_eq!(
        drop_fn(&admin_ctx, &dummy_any, &missing_fid).unwrap_err().0,
        STATUS_NOT_FOUND
    );

    let test_uid = 777_u64;
    db.with_mut_snapshot(|snap| {
        snap.account_plex_vault.insert(test_uid, 100);
        snap.quasar_commerce.failed_fulfillments = vec![FailedFulfillmentRecord {
            id: 5,
            user_id: test_uid,
            plex_units: 300,
            retries: 0,
            fulfilled: false,
        }];
    });
    let mut fid5 = DynamicQuasarMessage::new("Fid5");
    fid5.fields.insert(1, DynamicValue::Uint64(5));
    assert!(retry_fn(&admin_ctx, &dummy_any, &fid5).is_ok());
    // Second retry increments retries to 2 but does NOT add another 300 PLEX
    assert!(retry_fn(&admin_ctx, &dummy_any, &fid5).is_ok());
    db.with_snapshot(|snap| {
        assert_eq!(snap.account_plex_vault[&test_uid], 400);
        let ff5 = snap
            .quasar_commerce
            .failed_fulfillments
            .iter()
            .find(|f| f.id == 5)
            .unwrap();
        assert_eq!(ff5.retries, 2);
        assert!(ff5.fulfilled);
    });

    // 4. PromoteRequest, RetractRequest, BackfillPartnerCodeRequest validation & state checks
    let promote_fn = &handlers["eve.store.targetaudience.offer.api.admin.PromoteRequest"];
    let retract_fn = &handlers["eve.store.targetaudience.offer.api.admin.RetractRequest"];
    let backfill_fn =
        &handlers["eve.store.targetaudience.offer.api.admin.BackfillPartnerCodeRequest"];

    assert_eq!(
        promote_fn(&admin_ctx, &dummy_any, &empty_msg)
            .unwrap_err()
            .0,
        STATUS_BAD_REQUEST
    );
    let mut empty_id_req = DynamicQuasarMessage::new("Promote");
    empty_id_req
        .fields
        .insert(1, DynamicValue::Message(Box::new(empty_msg.clone())));
    assert_eq!(
        promote_fn(&admin_ctx, &dummy_any, &empty_id_req)
            .unwrap_err()
            .0,
        STATUS_BAD_REQUEST
    );

    assert_eq!(
        retract_fn(&admin_ctx, &dummy_any, &empty_msg)
            .unwrap_err()
            .0,
        STATUS_BAD_REQUEST
    );
    assert_eq!(
        retract_fn(&admin_ctx, &dummy_any, &empty_id_req)
            .unwrap_err()
            .0,
        STATUS_NOT_FOUND
    );

    assert_eq!(
        backfill_fn(&admin_ctx, &dummy_any, &empty_msg)
            .unwrap_err()
            .0,
        STATUS_BAD_REQUEST
    );
}

#[test]
fn mutant_kill_orch_state_and_failed_fulfillment_admin_mutations() {
    use crate::services::quasar_payment_store_types::get_plex_transactions;
    use eve_db::FailedFulfillmentRecord;

    let (ctx, _clock) = test_ctx_and_clock(1_700_000_000);
    let handlers = ctx.quasar_gateway.handlers.read().unwrap();
    let db = &ctx.db;
    let admin_ctx = AuthoritativeContext::for_session(
        Some(1),
        Some(100_000_001),
        Some(1_000_009),
        u64::MAX,
        "tranquility",
        vec![1, 2],
    );
    let dummy_any = GoogleAny::default();
    let empty_msg = DynamicQuasarMessage::new("Empty");

    // 1. SetDefaultTenantVersionRequest & GetDefaultTenantVersionRequest roundtrip through orch state
    let mut ver_msg = DynamicQuasarMessage::new("SemVer");
    ver_msg.fields.insert(1, DynamicValue::Uint64(3));
    ver_msg.fields.insert(2, DynamicValue::Uint64(4));
    ver_msg.fields.insert(3, DynamicValue::Uint64(5));
    let mut set_def_req = DynamicQuasarMessage::new("SetDef");
    set_def_req
        .fields
        .insert(1, DynamicValue::Message(Box::new(ver_msg)));

    let get_def_fn = &handlers["eve.content.orchestrator.api.GetDefaultTenantVersionRequest"];
    let before_def = get_def_fn(&admin_ctx, &dummy_any, &empty_msg).unwrap();
    handlers["eve.content.orchestrator.admin.api.SetDefaultTenantVersionRequest"](
        &admin_ctx,
        &dummy_any,
        &set_def_req,
    )
    .unwrap();
    let after_def = get_def_fn(&admin_ctx, &dummy_any, &empty_msg).unwrap();
    assert_ne!(before_def.value, after_def.value);
    assert_eq!(
        db.with_snapshot(|s| s.quasar_commerce.orchestrator.default_tenant_version.clone()),
        SemVerSpec {
            major: 3,
            minor: 4,
            patch: 5
        }
    );

    // 2. Component listing GetAllRequest is registered by register_commerce_admin_handlers
    let comp_all = handlers["eve.cosmetic.market.component.listing.api.GetAllRequest"](
        &admin_ctx, &dummy_any, &empty_msg,
    )
    .unwrap();
    assert!(!comp_all.value.is_empty());

    // 3. RetryFailedFulfillmentRequest boundary cases (fulfilled, plex=0, plex<0, eligible non-first, retries++, next_entity_id++)
    let uid = 888_001_u64;
    db.with_mut_snapshot(|snap| {
        snap.account_plex_vault.insert(uid, 100);
        snap.quasar_commerce.failed_fulfillments = vec![
            FailedFulfillmentRecord {
                id: 4,
                user_id: uid,
                plex_units: 200,
                retries: 0,
                fulfilled: true,
            },
            FailedFulfillmentRecord {
                id: 6,
                user_id: uid,
                plex_units: 0,
                retries: 0,
                fulfilled: false,
            },
            FailedFulfillmentRecord {
                id: 7,
                user_id: uid,
                plex_units: -25,
                retries: 0,
                fulfilled: false,
            },
            FailedFulfillmentRecord {
                id: 5,
                user_id: uid,
                plex_units: 300,
                retries: 0,
                fulfilled: false,
            },
        ];
    });
    let retry_fn = &handlers["eve.store.sale.admin.RetryFailedFulfillmentRequest"];
    let mk_fid = |id: u64| {
        let mut m = DynamicQuasarMessage::new("Fid");
        m.fields.insert(1, DynamicValue::Uint64(id));
        m
    };

    let (eid_0, tx_0) = db.with_snapshot(|s| (s.next_entity_id, get_plex_transactions(s).len()));

    // Already fulfilled, 0 units, and negative units must NOT credit PLEX or touch next_entity_id
    for id in [4_u64, 6, 7] {
        assert!(retry_fn(&admin_ctx, &dummy_any, &mk_fid(id)).is_ok());
    }
    db.with_snapshot(|s| {
        assert_eq!(s.account_plex_vault[&uid], 100);
        assert_eq!(s.next_entity_id, eid_0);
        for id in [4_u32, 6, 7] {
            let r = s
                .quasar_commerce
                .failed_fulfillments
                .iter()
                .find(|r| r.id == id)
                .unwrap();
            assert_eq!(r.retries, 1);
            if id != 4 {
                assert!(!r.fulfilled);
            }
        }
    });

    // Eligible record fid=5 (non-first): credits 300 once, retries goes 0 -> 1 -> 2, next_entity_id increments by 1
    assert!(retry_fn(&admin_ctx, &dummy_any, &mk_fid(5)).is_ok());
    db.with_snapshot(|s| {
        assert_eq!(s.account_plex_vault[&uid], 400);
        assert_eq!(s.next_entity_id, eid_0 + 1);
        let txs = get_plex_transactions(s);
        assert_eq!(txs.len(), tx_0 + 1);
        let r5 = s
            .quasar_commerce
            .failed_fulfillments
            .iter()
            .find(|r| r.id == 5)
            .unwrap();
        assert_eq!(r5.retries, 1);
        assert!(r5.fulfilled);
    });
    assert!(retry_fn(&admin_ctx, &dummy_any, &mk_fid(5)).is_ok());
    db.with_snapshot(|s| {
        assert_eq!(s.account_plex_vault[&uid], 400);
        assert_eq!(s.next_entity_id, eid_0 + 1);
        let r5 = s
            .quasar_commerce
            .failed_fulfillments
            .iter()
            .find(|r| r.id == 5)
            .unwrap();
        assert_eq!(r5.retries, 2);
    });

    // 4. RetryAllFailedFulfillmentsRequest: only eligible (!fulfilled && plex_units > 0) credited; DropFailedFulfillmentRequest retains non-matching IDs
    db.with_mut_snapshot(|snap| {
        snap.quasar_commerce.failed_fulfillments = vec![
            FailedFulfillmentRecord {
                id: 11,
                user_id: uid,
                plex_units: 120,
                retries: 0,
                fulfilled: true,
            },
            FailedFulfillmentRecord {
                id: 12,
                user_id: uid,
                plex_units: 0,
                retries: 0,
                fulfilled: false,
            },
            FailedFulfillmentRecord {
                id: 13,
                user_id: uid,
                plex_units: -10,
                retries: 0,
                fulfilled: false,
            },
            FailedFulfillmentRecord {
                id: 14,
                user_id: uid,
                plex_units: 150,
                retries: 0,
                fulfilled: false,
            },
        ];
    });
    let eid_1 = db.with_snapshot(|s| s.next_entity_id);
    handlers["eve.store.sale.admin.RetryAllFailedFulfillmentsRequest"](
        &admin_ctx, &dummy_any, &empty_msg,
    )
    .unwrap();
    db.with_snapshot(|s| {
        assert_eq!(s.account_plex_vault[&uid], 550);
        assert_eq!(s.next_entity_id, eid_1 + 1);
        assert!(
            s.quasar_commerce
                .failed_fulfillments
                .iter()
                .find(|r| r.id == 14)
                .unwrap()
                .fulfilled
        );
        assert!(
            !s.quasar_commerce
                .failed_fulfillments
                .iter()
                .find(|r| r.id == 12)
                .unwrap()
                .fulfilled
        );
        assert!(
            !s.quasar_commerce
                .failed_fulfillments
                .iter()
                .find(|r| r.id == 13)
                .unwrap()
                .fulfilled
        );
    });

    let drop_fn = &handlers["eve.store.sale.admin.DropFailedFulfillmentRequest"];
    assert!(drop_fn(&admin_ctx, &dummy_any, &mk_fid(12)).is_ok());
    db.with_snapshot(|s| {
        let remaining_ids: Vec<u32> = s
            .quasar_commerce
            .failed_fulfillments
            .iter()
            .map(|r| r.id)
            .collect();
        assert_eq!(remaining_ids, vec![11, 13, 14]);
    });
    assert_eq!(
        drop_fn(&admin_ctx, &dummy_any, &mk_fid(99_999))
            .unwrap_err()
            .0,
        STATUS_NOT_FOUND
    );
}
