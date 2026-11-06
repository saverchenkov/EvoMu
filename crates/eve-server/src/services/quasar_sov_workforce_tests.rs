//! Unit tests for `quasar_sov_workforce` (Equinox Sovereignty Hub workforce transfer network, relays, configuration).

#![forbid(unsafe_code)]

use super::*;
use crate::quasar_gateway::PublicGatewayRouter;
use eve_db::{corp_roles, PersistedHubUpgrade, UpwellStructureRecord};
use eve_proto::quasar::{AuthoritativeContext, DynamicQuasarMessage, DynamicValue, GoogleAny, QuasarDescriptorRegistry};
use std::sync::{Arc, OnceLock};

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn shared_sde() -> Arc<SdeRegistry> {
    Arc::clone(SHARED_SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn id_msg(field: u32, id: i64) -> DynamicQuasarMessage {
    let mut inner = DynamicQuasarMessage::new("Identifier");
    inner.fields.insert(1, DynamicValue::Int64(id));
    let mut outer = DynamicQuasarMessage::new("Req");
    outer.fields.insert(field, DynamicValue::Message(Box::new(inner)));
    outer
}

#[test]
fn workforce_jump_graph_and_configuration_codecs() {
    let sde = shared_sde();
    let reach = systems_within_jumps(&sde, 30_000_142, 2);
    assert_eq!(reach.get(&30_000_142), Some(&0));
    assert!(reach.len() > 1);

    for m in [
        PersistedWorkforceMode::Inactive,
        PersistedWorkforceMode::Transit,
        PersistedWorkforceMode::Import {
            sources: vec![30_000_142],
        },
        PersistedWorkforceMode::Export {
            destination: Some(30_000_144),
            amount: 500,
        },
        PersistedWorkforceMode::Export {
            destination: None,
            amount: 250,
        },
    ] {
        assert!(!encode_configuration(&m).is_empty());
    }

    let mut cfg_transit = DynamicQuasarMessage::new("Cfg");
    cfg_transit.fields.insert(4, DynamicValue::Bool(true));
    assert_eq!(
        decode_configuration(&cfg_transit).unwrap(),
        PersistedWorkforceMode::Transit
    );

    let mut exp_neg = DynamicQuasarMessage::new("Exp");
    exp_neg.fields.insert(2, DynamicValue::Int64(-10));
    let mut cfg_bad = DynamicQuasarMessage::new("Cfg");
    cfg_bad
        .fields
        .insert(3, DynamicValue::Message(Box::new(exp_neg)));
    assert_eq!(decode_configuration(&cfg_bad).unwrap_err().0, STATUS_BAD_REQUEST);
}

#[test]
fn workforce_network_and_rpc_handlers() {
    let sde = shared_sde();
    let db = Arc::new(WorldRepository::with_demo_seed());
    let descriptors = Arc::new(QuasarDescriptorRegistry::empty_with_builtins());
    let router = PublicGatewayRouter::new(descriptors, Arc::clone(&db), Arc::clone(&sde));
    register_handlers(&router, Arc::clone(&db), Arc::clone(&sde));

    let cid = 90_000_001_i64;
    let corp_id = 1_000_009_i64;
    let sys_a = 30_000_142_u32;
    let sys_b = *systems_within_jumps(&sde, sys_a, 1)
        .keys()
        .find(|&&s| s != sys_a)
        .expect("adjacent system");
    let hub_a = 80_000_001_i64;
    let hub_b = 80_000_002_i64;

    db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.corporation_id = corp_id;
            ch.solar_system_id = sys_a;
        }
        snap.corporations.insert(
            corp_id,
            eve_db::CorporationRecord {
                corporation_id: corp_id,
                ceo_id: cid,
                ..Default::default()
            },
        );
        snap.structures.insert(
            hub_a,
            UpwellStructureRecord {
                structure_id: hub_a,
                type_id: TYPE_SOVEREIGNTY_HUB,
                owner_corp_id: corp_id,
                solar_system_id: sys_a,
                name: "Hub A".into(),
                state: "anchor_vulnerable".into(),
                ..Default::default()
            },
        );
        snap.structures.insert(
            hub_b,
            UpwellStructureRecord {
                structure_id: hub_b,
                type_id: TYPE_SOVEREIGNTY_HUB,
                owner_corp_id: corp_id,
                solar_system_id: sys_b,
                name: "Hub B".into(),
                state: "anchor_vulnerable".into(),
                ..Default::default()
            },
        );
        // Give Hub A an active workforce-producing upgrade (87_710 produces 5,000 workforce, needs 200 power)
        snap.sov_runtime.sov_hubs.insert(
            hub_a,
            PersistedSovHubData {
                hub_id: hub_a,
                upgrades: vec![PersistedHubUpgrade {
                    type_id: 87_710,
                    online: true,
                    low_power: false,
                }],
                ..Default::default()
            },
        );
    });

    let handlers = router.handlers.read().unwrap();
    let dummy_any = GoogleAny::default();
    let dir_ctx = AuthoritativeContext::for_session(
        Some(1),
        Some(cid as u32),
        Some(corp_id as u32),
        corp_roles::ROLE_DIRECTOR,
        "tranquility",
        vec![1],
    );

    // GetNetworkableHubsRequest on Hub A finds Hub B
    let net_resp = handlers["eve_public.sovereignty.hub.workforce.api.GetNetworkableHubsRequest"](
        &dir_ctx,
        &dummy_any,
        &id_msg(1, hub_a),
    )
    .expect("GetNetworkableHubsRequest");
    assert!(net_resp
        .type_url
        .ends_with("eve_public.sovereignty.hub.workforce.api.GetNetworkableHubsResponse"));
    assert!(!net_resp.value.is_empty());

    // Configure Hub A to Export 1,000 workforce to sys_b
    let mut exp_msg = id_msg(1, sys_b as i64);
    exp_msg.fields.insert(2, DynamicValue::Int64(1_000));
    let mut cfg_a = DynamicQuasarMessage::new("Cfg");
    cfg_a.fields.insert(3, DynamicValue::Message(Box::new(exp_msg)));
    let mut req_a = id_msg(1, hub_a);
    req_a.fields.insert(2, DynamicValue::Message(Box::new(cfg_a)));

    handlers["eve_public.sovereignty.hub.workforce.api.ConfigureRequest"](&dir_ctx, &dummy_any, &req_a)
        .expect("Configure Hub A Export");

    // Configure Hub B to Import from sys_a
    let src_msg = id_msg(1, sys_a as i64);
    let mut imp_msg = DynamicQuasarMessage::new("Imp");
    imp_msg
        .fields
        .insert(1, DynamicValue::Repeated(vec![DynamicValue::Message(Box::new(src_msg))]));
    let mut cfg_b = DynamicQuasarMessage::new("Cfg");
    cfg_b.fields.insert(2, DynamicValue::Message(Box::new(imp_msg)));
    let mut req_b = id_msg(1, hub_b);
    req_b.fields.insert(2, DynamicValue::Message(Box::new(cfg_b)));

    handlers["eve_public.sovereignty.hub.workforce.api.ConfigureRequest"](&dir_ctx, &dummy_any, &req_b)
        .expect("Configure Hub B Import");

    db.with_snapshot(|snap| {
        assert_eq!(exported_amount(snap, hub_a), 1_000);
        assert_eq!(imports(snap, &sde, hub_b), vec![(sys_a, 1_000)]);
    });

    // GetStateRequest & GetConfigurationRequest on both hubs
    for hid in [hub_a, hub_b] {
        let st = handlers["eve_public.sovereignty.hub.workforce.api.GetStateRequest"](
            &dir_ctx,
            &dummy_any,
            &id_msg(1, hid),
        )
        .expect("GetStateRequest");
        assert!(st
            .type_url
            .ends_with("eve_public.sovereignty.hub.workforce.api.GetStateResponse"));

        let cf = handlers["eve_public.sovereignty.hub.workforce.api.GetConfigurationRequest"](
            &dir_ctx,
            &dummy_any,
            &id_msg(1, hid),
        )
        .expect("GetConfigurationRequest");
        assert!(cf
            .type_url
            .ends_with("eve_public.sovereignty.hub.workforce.api.GetConfigurationResponse"));
    }
}
