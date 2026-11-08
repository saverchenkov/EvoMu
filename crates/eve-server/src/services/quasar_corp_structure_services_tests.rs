use super::*;
use crate::services::fleet_social::fleet_social_fleet_ops::fleet_social_fleet_ops_tests::test_ctx_and_clock;
use eve_proto::quasar::AuthoritativeContext;

#[test]
fn semver_orchestrator_and_commerce_admin_handlers() {
    let (ctx, _clock) = test_ctx_and_clock(1_700_000_000);
    let router = &ctx.quasar_gateway;
    let db = &ctx.db;

    let mut ver_msg = DynamicQuasarMessage::new("SemVer");
    ver_msg.fields.insert(1, DynamicValue::Uint32(2));
    ver_msg.fields.insert(2, DynamicValue::Uint32(4));
    ver_msg.fields.insert(3, DynamicValue::Uint32(1));
    let spec = SemVerSpec::from_dyn(&ver_msg);
    assert_eq!(
        spec,
        SemVerSpec {
            major: 2,
            minor: 4,
            patch: 1
        }
    );
    assert!(!spec.encode().is_empty());

    let mut init_orch = OrchestratorState::default();
    init_orch.deployments.insert(
        10,
        DeploymentRecord {
            id: 10,
            name: "Pirate Insurgency".into(),
            short_description: "Vanguard".into(),
            content_group_id: 20,
            enabled: false,
            current_content_count: 3,
            no_region_limit: true,
        },
    );
    init_orch.content_groups.insert(
        20,
        ContentGroupRecord {
            id: 20,
            name: "Group 20".into(),
            content_type_id: 30,
            weight: 1.5,
        },
    );
    init_orch.content_types.insert(
        30,
        ContentTypeRecord {
            id: 30,
            name: "Type 30".into(),
            short_description: "Dungeon".into(),
            dungeon_id: 400,
        },
    );
    init_orch.instances.push(DeploymentInstanceRecord {
        uuid: vec![1, 2, 3, 4],
        deployment_id: 10,
        content_type_id: 30,
        solar_system_id: 30_000_142,
        constellation_id: 20_000_020,
        created_unix: 1_700_000_000,
    });
    save_orch_state(db, &init_orch);

    db.with_mut_snapshot(|snap| {
        snap.quasar_commerce.failed_fulfillments.push(
            FailedFulfillmentRecord {
                id: 1,
                user_id: 1,
                plex_units: 500,
                retries: 0,
                fulfilled: false,
            },
        );
        snap.quasar_commerce.failed_fulfillments.push(
            FailedFulfillmentRecord {
                id: 2,
                user_id: 1,
                plex_units: 250,
                retries: 0,
                fulfilled: false,
            },
        );
    });

    let admin_ctx = AuthoritativeContext::for_session(
        Some(1),
        Some(100_000_001),
        Some(1_000_009),
        u64::MAX,
        "tranquility",
        vec![1, 2],
    );
    let dummy_any = GoogleAny::default();
    let handlers = router.handlers.read().unwrap();
    let empty_msg = DynamicQuasarMessage::new("Empty");

    let mut set_def = DynamicQuasarMessage::new("SetDef");
    set_def.fields.insert(1, DynamicValue::Message(Box::new(ver_msg.clone())));
    assert!(handlers["eve.content.orchestrator.admin.api.SetDefaultTenantVersionRequest"](&admin_ctx, &dummy_any, &set_def).is_ok());

    let mut set_ten = DynamicQuasarMessage::new("SetTen");
    set_ten.fields.insert(2, DynamicValue::Message(Box::new(ver_msg)));
    assert!(handlers["eve.content.orchestrator.admin.api.SetTenantVersionRequest"](&admin_ctx, &dummy_any, &set_ten).is_ok());

    assert!(handlers["eve.content.orchestrator.api.GetDefaultTenantVersionRequest"](&admin_ctx, &dummy_any, &empty_msg).is_ok());
    assert!(handlers["eve.content.orchestrator.api.GetTenantVersionRequest"](&admin_ctx, &dummy_any, &empty_msg).is_ok());

    let mut tag_msg = DynamicQuasarMessage::new("Tag");
    tag_msg.fields.insert(1, DynamicValue::String("highsec".into()));
    let mut rs_msg = DynamicQuasarMessage::new("Ruleset");
    rs_msg.fields.insert(1, DynamicValue::Message(Box::new(tag_msg)));
    let mut dry_msg = DynamicQuasarMessage::new("DryRun");
    dry_msg.fields.insert(1, DynamicValue::Message(Box::new(rs_msg)));
    assert!(handlers["eve.content.orchestrator.api.DryRunRulesetRequest"](&admin_ctx, &dummy_any, &dry_msg).is_ok());

    assert!(handlers["eve.content.orchestrator.deployment.api.ListRequest"](&admin_ctx, &dummy_any, &empty_msg).is_ok());

    let mut dep_msg = DynamicQuasarMessage::new("Dep");
    dep_msg.fields.insert(1, DynamicValue::Uint64(10));
    assert!(handlers["eve.content.orchestrator.deployment.api.GetRequest"](&admin_ctx, &dummy_any, &dep_msg).is_ok());
    assert!(handlers["eve.content.orchestrator.deployment.api.EnableRequest"](&admin_ctx, &dummy_any, &dep_msg).is_ok());
    assert!(handlers["eve.content.orchestrator.deployment.api.DisableRequest"](&admin_ctx, &dummy_any, &dep_msg).is_ok());

    let mut cg_msg = DynamicQuasarMessage::new("Cg");
    cg_msg.fields.insert(1, DynamicValue::Uint64(20));
    assert!(handlers["eve.content.orchestrator.deployment.contentgroup.api.GetRequest"](&admin_ctx, &dummy_any, &cg_msg).is_ok());

    let mut ct_msg = DynamicQuasarMessage::new("Ct");
    ct_msg.fields.insert(1, DynamicValue::Uint64(30));
    assert!(handlers["eve.content.orchestrator.deployment.contenttype.api.GetRequest"](&admin_ctx, &dummy_any, &ct_msg).is_ok());

    assert!(handlers["eve.content.orchestrator.deployment.instance.api.ListRequest"](&admin_ctx, &dummy_any, &dep_msg).is_ok());

    let mut inst_msg = DynamicQuasarMessage::new("Inst");
    inst_msg.fields.insert(1, DynamicValue::Bytes(vec![1, 2, 3, 4]));
    assert!(handlers["eve.content.orchestrator.deployment.instance.api.GetRequest"](&admin_ctx, &dummy_any, &inst_msg).is_ok());
    assert!(handlers["eve.content.orchestrator.deployment.instance.admin.api.RemoveRequest"](&admin_ctx, &dummy_any, &inst_msg).is_ok());

    assert!(handlers["eve.cosmetic.market.component.listing.api.GetAllRequest"](&admin_ctx, &dummy_any, &empty_msg).is_ok());

    let mut creator_msg = DynamicQuasarMessage::new("Creator");
    creator_msg.fields.insert(1, DynamicValue::Uint64(100_000_001));
    assert!(handlers["eve.cosmetic.market.skin.listing.api.admin.GetAllListedByCharacterRequest"](&admin_ctx, &dummy_any, &creator_msg).is_ok());

    let mut fid1 = DynamicQuasarMessage::new("Fid1");
    fid1.fields.insert(1, DynamicValue::Uint64(1));
    assert!(handlers["eve.store.sale.admin.RetryFailedFulfillmentRequest"](&admin_ctx, &dummy_any, &fid1).is_ok());
    assert!(handlers["eve.store.sale.admin.RetryAllFailedFulfillmentsRequest"](&admin_ctx, &dummy_any, &empty_msg).is_ok());
    assert!(handlers["eve.store.sale.admin.DropFailedFulfillmentRequest"](&admin_ctx, &dummy_any, &fid1).is_ok());

    let mut offer_id = DynamicQuasarMessage::new("OfferId");
    offer_id.fields.insert(1, DynamicValue::Uint64(1));
    offer_id.fields.insert(2, DynamicValue::String("starter_pack".into()));
    let mut offer_req = DynamicQuasarMessage::new("OfferReq");
    offer_req.fields.insert(1, DynamicValue::Message(Box::new(offer_id.clone())));
    assert!(handlers["eve.store.targetaudience.offer.api.admin.PromoteRequest"](&admin_ctx, &dummy_any, &offer_req).is_ok());
    assert!(handlers["eve.store.targetaudience.offer.api.admin.RetractRequest"](&admin_ctx, &dummy_any, &offer_req).is_ok());

    let mut bf_req = DynamicQuasarMessage::new("Backfill");
    bf_req.fields.insert(1, DynamicValue::Uint64(1));
    bf_req.fields.insert(3, DynamicValue::Message(Box::new(offer_id)));
    bf_req.fields.insert(4, DynamicValue::String("PARTNER2026".into()));
    assert!(handlers["eve.store.targetaudience.offer.api.admin.BackfillPartnerCodeRequest"](&admin_ctx, &dummy_any, &bf_req).is_ok());
}
