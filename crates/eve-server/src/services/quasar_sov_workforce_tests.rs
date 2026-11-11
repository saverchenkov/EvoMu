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
            { let mut __corp = eve_db::CorporationRecord::default();
                __corp.corporation_id = corp_id;
                __corp.ceo_id = cid;
                __corp },
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

#[test]
fn workforce_relay_alliance_permissions_overload_and_error_paths() {
    let sde = shared_sde();
    let db = Arc::new(WorldRepository::with_demo_seed());
    let descriptors = Arc::new(QuasarDescriptorRegistry::empty_with_builtins());
    let router = PublicGatewayRouter::new(descriptors, Arc::clone(&db), Arc::clone(&sde));
    register_handlers(&router, Arc::clone(&db), Arc::clone(&sde));

    let cid = 90_000_001_i64;
    let non_dir_cid = 90_000_002_i64;
    let corp_a = 1_000_009_i64;
    let corp_b = 1_000_010_i64;
    let corp_foreign = 1_000_011_i64;
    let alliance_id = 99_000_001_i64;

    // Pick a chain of 3 distinct systems: sys_a -> sys_b -> sys_c
    let sys_a = 30_000_142_u32;
    let reach_a = systems_within_jumps(&sde, sys_a, 1);
    let sys_b = *reach_a.keys().find(|&&s| s != sys_a).expect("sys_b");
    let reach_b = systems_within_jumps(&sde, sys_b, 1);
    let sys_c = *reach_b
        .keys()
        .find(|&&s| {
            s != sys_a
                && s != sys_b
                && sde
                    .celestials_in_system(s)
                    .iter()
                    .filter(|c| c.group_id == 6)
                    .map(|c| super::super::quasar_sov_hub::star_power(&sde, c.type_id))
                    .sum::<u64>()
                    >= 250
        })
        .unwrap_or(&sys_b);

    let hub_a = 81_000_001_i64;
    let hub_b_transit = 81_000_002_i64;
    let hub_c_import = 81_000_003_i64;
    let hub_foreign = 81_000_004_i64;
    let hub_destroyed = 81_000_005_i64;

    db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.corporation_id = corp_a;
            ch.alliance_id = Some(alliance_id);
            ch.solar_system_id = sys_a;
        }
        let mut non_dir_ch = snap.characters[&cid].clone();
        non_dir_ch.character_id = non_dir_cid;
        non_dir_ch.user_id = 2;
        non_dir_ch.corporation_id = corp_a;
        non_dir_ch.alliance_id = Some(alliance_id);
        snap.characters.insert(non_dir_cid, non_dir_ch);
        snap.alliances.insert(
            alliance_id,
            eve_db::AllianceRecord {
                alliance_id,
                name: "Test Alliance".into(),
                short_name: "TSTA".into(),
                executor_corp_id: corp_a,
                member_corp_ids: [corp_a, corp_b].into_iter().collect(),
            },
        );
        for (c_id, ally) in [(corp_a, Some(alliance_id)), (corp_b, Some(alliance_id)), (corp_foreign, None)] {
            snap.corporations.insert(
                c_id,
                { let mut __corp = eve_db::CorporationRecord::default(); __corp.corporation_id = c_id; __corp.ceo_id = cid; __corp.alliance_id = ally; __corp.ensure_wallet_divisions(); __corp },
            );
        }
        let mk_hub = |id: i64, owner: i64, sys: u32, state: &str| UpwellStructureRecord {
            structure_id: id,
            type_id: TYPE_SOVEREIGNTY_HUB,
            owner_corp_id: owner,
            solar_system_id: sys,
            name: format!("Hub {id}"),
            state: state.into(),
            ..Default::default()
        };
        snap.structures.insert(hub_a, mk_hub(hub_a, corp_a, sys_a, "anchor_vulnerable"));
        snap.structures.insert(hub_b_transit, mk_hub(hub_b_transit, corp_b, sys_b, "anchor_vulnerable"));
        snap.structures.insert(hub_c_import, mk_hub(hub_c_import, corp_b, sys_c, "anchor_vulnerable"));
        snap.structures.insert(hub_foreign, mk_hub(hub_foreign, corp_foreign, sys_b, "anchor_vulnerable"));
        snap.structures.insert(hub_destroyed, mk_hub(hub_destroyed, corp_a, sys_b, "destroyed"));

        // Hub A produces 5,000 workforce via 87_710
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
        // Hub C has upgrade 81_615 (needs 1,500 workforce)
        snap.sov_runtime.sov_hubs.insert(
            hub_c_import,
            PersistedSovHubData {
                hub_id: hub_c_import,
                upgrades: vec![PersistedHubUpgrade {
                    type_id: 81_615,
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
        Some(corp_a as u32),
        corp_roles::ROLE_DIRECTOR,
        "tranquility",
        vec![1],
    );
    let non_dir_ctx = AuthoritativeContext::for_session(
        Some(2),
        Some(non_dir_cid as u32),
        Some(corp_a as u32),
        0,
        "tranquility",
        vec![1],
    );

    // 1. Error paths: missing hub id, missing config, non-director role, foreign hub, bad export/import
    assert_eq!(
        handlers["eve_public.sovereignty.hub.workforce.api.GetStateRequest"](&dir_ctx, &dummy_any, &DynamicQuasarMessage::new("Empty"))
            .unwrap_err()
            .0,
        STATUS_BAD_REQUEST
    );
    assert_eq!(
        handlers["eve_public.sovereignty.hub.workforce.api.ConfigureRequest"](&dir_ctx, &dummy_any, &id_msg(1, hub_a))
            .unwrap_err()
            .0,
        STATUS_BAD_REQUEST
    );
    let mut cfg_inactive = DynamicQuasarMessage::new("Cfg");
    cfg_inactive.fields.insert(1, DynamicValue::Bool(true));
    let mut req_inactive_a = id_msg(1, hub_a);
    req_inactive_a.fields.insert(2, DynamicValue::Message(Box::new(cfg_inactive.clone())));
    assert!(
        handlers["eve_public.sovereignty.hub.workforce.api.ConfigureRequest"](&non_dir_ctx, &dummy_any, &req_inactive_a)
            .is_err()
    );
    assert!(
        handlers["eve_public.sovereignty.hub.workforce.api.GetStateRequest"](&dir_ctx, &dummy_any, &id_msg(1, hub_foreign))
            .is_err()
    );

    // Export more workforce than available rolls back and errors
    let mut exp_too_much = id_msg(1, sys_b as i64);
    exp_too_much.fields.insert(2, DynamicValue::Int64(99_999));
    let mut cfg_too_much = DynamicQuasarMessage::new("Cfg");
    cfg_too_much.fields.insert(3, DynamicValue::Message(Box::new(exp_too_much)));
    let mut req_too_much = id_msg(1, hub_a);
    req_too_much.fields.insert(2, DynamicValue::Message(Box::new(cfg_too_much)));
    assert_eq!(
        handlers["eve_public.sovereignty.hub.workforce.api.ConfigureRequest"](&dir_ctx, &dummy_any, &req_too_much)
            .unwrap_err()
            .0,
        STATUS_BAD_REQUEST
    );
    db.with_snapshot(|snap| assert_eq!(mode(snap, hub_a), PersistedWorkforceMode::Inactive));

    // Import from unreachable system -> STATUS_BAD_REQUEST
    let mut imp_bad = DynamicQuasarMessage::new("Imp");
    imp_bad.fields.insert(1, DynamicValue::Repeated(vec![DynamicValue::Message(Box::new(id_msg(1, 30_009_999)))]));
    let mut cfg_imp_bad = DynamicQuasarMessage::new("Cfg");
    cfg_imp_bad.fields.insert(2, DynamicValue::Message(Box::new(imp_bad)));
    let mut req_imp_bad = id_msg(1, hub_a);
    req_imp_bad.fields.insert(2, DynamicValue::Message(Box::new(cfg_imp_bad)));
    assert_eq!(
        handlers["eve_public.sovereignty.hub.workforce.api.ConfigureRequest"](&dir_ctx, &dummy_any, &req_imp_bad)
            .unwrap_err()
            .0,
        STATUS_BAD_REQUEST
    );

    // 2. Relay chain: Hub B (Transit) -> Hub A exports 2,000 to sys_b -> Hub C (same alliance!) imports from sys_a
    let dir_b_ctx = AuthoritativeContext::for_session(
        Some(2),
        Some(cid as u32),
        Some(corp_b as u32),
        corp_roles::ROLE_DIRECTOR,
        "tranquility",
        vec![1],
    );
    let mut cfg_transit = DynamicQuasarMessage::new("Cfg");
    cfg_transit.fields.insert(4, DynamicValue::Bool(true));
    let mut req_b = id_msg(1, hub_b_transit);
    req_b.fields.insert(2, DynamicValue::Message(Box::new(cfg_transit)));
    // Set corp_b character membership for caller check
    db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&cid).unwrap().corporation_id = corp_b;
    });
    handlers["eve_public.sovereignty.hub.workforce.api.ConfigureRequest"](&dir_b_ctx, &dummy_any, &req_b)
        .expect("Configure Hub B Transit");

    // Verify networkable_hubs excludes foreign and destroyed hubs, includes alliance hubs
    db.with_snapshot(|snap| {
        let net = networkable_hubs(snap, &sde, hub_a);
        assert!(net.iter().any(|(id, _)| *id == hub_b_transit));
        assert!(!net.iter().any(|(id, _)| *id == hub_foreign || *id == hub_destroyed));
        assert!(!encode_state(snap, &sde, hub_b_transit).is_empty());
    });

    // Hub A exports 2,000 to sys_b (before C imports -> unconnected field 6 in encode_state)
    db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&cid).unwrap().corporation_id = corp_a;
    });
    let mut exp_a = id_msg(1, sys_b as i64);
    exp_a.fields.insert(2, DynamicValue::Int64(2_000));
    let mut cfg_a = DynamicQuasarMessage::new("Cfg");
    cfg_a.fields.insert(3, DynamicValue::Message(Box::new(exp_a)));
    let mut req_a = id_msg(1, hub_a);
    req_a.fields.insert(2, DynamicValue::Message(Box::new(cfg_a)));
    handlers["eve_public.sovereignty.hub.workforce.api.ConfigureRequest"](&dir_ctx, &dummy_any, &req_a)
        .expect("Configure Hub A Export");

    db.with_snapshot(|snap| {
        assert_eq!(export_connected(snap, &sde, hub_a), None);
        assert!(!encode_state(snap, &sde, hub_a).is_empty());
    });

    // Hub C imports from sys_a via Hub B Transit relay
    db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&cid).unwrap().corporation_id = corp_b;
    });
    let mut imp_c = DynamicQuasarMessage::new("Imp");
    imp_c.fields.insert(1, DynamicValue::Repeated(vec![DynamicValue::Message(Box::new(id_msg(1, sys_a as i64)))]));
    let mut cfg_c = DynamicQuasarMessage::new("Cfg");
    cfg_c.fields.insert(2, DynamicValue::Message(Box::new(imp_c)));
    let mut req_c = id_msg(1, hub_c_import);
    req_c.fields.insert(2, DynamicValue::Message(Box::new(cfg_c)));
    handlers["eve_public.sovereignty.hub.workforce.api.ConfigureRequest"](&dir_b_ctx, &dummy_any, &req_c)
        .expect("Configure Hub C Import via relay");

    db.with_snapshot(|snap| {
        assert_eq!(export_connected(snap, &sde, hub_a), Some(sys_b));
        assert_eq!(imports(snap, &sde, hub_c_import), vec![(sys_a, 2_000)]);
        assert!(!encode_state(snap, &sde, hub_a).is_empty());
        assert!(!encode_state(snap, &sde, hub_c_import).is_empty());
    });

    // 3. Severing Hub A export to Inactive starves Hub C's upgrade 81_615 -> drops to low_power = true and emits notices
    router.pending_notices.write().unwrap().clear();
    db.with_mut_snapshot(|snap| {
        snap.sov_runtime
            .sov_hubs
            .get_mut(&hub_c_import)
            .unwrap()
            .upgrades[0]
            .low_power = false;
        snap.characters.get_mut(&cid).unwrap().corporation_id = corp_a;
    });
    handlers["eve_public.sovereignty.hub.workforce.api.ConfigureRequest"](&dir_ctx, &dummy_any, &req_inactive_a)
        .expect("Sever Hub A Export");

    db.with_snapshot(|snap| {
        assert_eq!(exported_amount(snap, hub_a), 0);
        assert!(snap.sov_runtime.sov_hubs[&hub_c_import].upgrades[0].low_power);
    });
    let notices = router.pending_notices.read().unwrap();
    assert!(notices.iter().any(|(_, a)| a.type_url.ends_with(CONFIGURED_NOTICE)));
    assert!(notices
        .iter()
        .any(|(_, a)| a.type_url.ends_with("HubUpgradesSimulatedNotice")));
}
