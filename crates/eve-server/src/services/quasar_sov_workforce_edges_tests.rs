//! Edge-case unit tests for `quasar_sov_workforce`.

#![forbid(unsafe_code)]

use super::*;
use crate::quasar_gateway::{PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_FORBIDDEN, STATUS_NOT_FOUND};
use crate::services::quasar_sov_hub::upgrade_definition;
use eve_db::{corp_roles, PersistedHubUpgrade, PersistedSovHubData, PersistedWorkforceMode, UpwellStructureRecord};
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
fn relay_tiebreakers_destroyed_hubs_alliance_sharing_and_codecs() {
    let sde = shared_sde();
    let sys_a = 30_000_142_u32;
    let reach1 = systems_within_jumps(&sde, sys_a, 1);
    let reach5 = systems_within_jumps(&sde, sys_a, WORKFORCE_TRANSFER_MAX_JUMPS);
    let reach6 = systems_within_jumps(&sde, sys_a, WORKFORCE_TRANSFER_MAX_JUMPS + 1);
    let sys_b = *reach1.keys().find(|&&s| s != sys_a).expect("1-jump sys");
    let sys_far = *reach6.keys().find(|s| !reach5.contains_key(s)).expect("6-jump sys");
    // Find a midpoint system within 5 jumps of sys_a AND within 5 jumps of sys_far
    let reach_far_5 = systems_within_jumps(&sde, sys_far, WORKFORCE_TRANSFER_MAX_JUMPS);
    let sys_mid = *reach5
        .keys()
        .find(|s| **s != sys_a && **s != sys_far && reach_far_5.contains_key(s))
        .expect("relay midpoint within 5 jumps of both");

    let corp1 = 98_100_001_i64;
    let corp2 = 98_100_002_i64;
    let corp3 = 98_100_003_i64;
    let alliance_shared = 99_100_001_i64;

    let db = WorldRepository::with_demo_seed();
    let hub_a = 80_100_001_i64;
    let hub_t = 80_100_002_i64;
    let hub_c_relay = 80_100_008_i64;
    let hub_c_direct_hi = 80_100_005_i64;
    let hub_c_direct_lo = 80_100_004_i64;
    let hub_destroyed = 80_100_006_i64;
    let hub_foreign = 80_100_007_i64;

    db.with_mut_snapshot(|snap| {
        snap.corporations.insert(
            corp1,
            { let mut __corp = eve_db::CorporationRecord::default(); __corp.corporation_id = corp1; __corp.alliance_id = Some(alliance_shared); __corp.ensure_wallet_divisions(); __corp },
        );
        snap.corporations.insert(
            corp2,
            { let mut __corp = eve_db::CorporationRecord::default(); __corp.corporation_id = corp2; __corp.alliance_id = Some(alliance_shared); __corp.ensure_wallet_divisions(); __corp },
        );
        snap.corporations.insert(
            corp3,
            { let mut __corp = eve_db::CorporationRecord::default(); __corp.corporation_id = corp3; __corp.alliance_id = None; __corp.ensure_wallet_divisions(); __corp },
        );

        let mk_hub = |id: i64, owner: i64, sys: u32, state: &str| UpwellStructureRecord {
            structure_id: id,
            type_id: TYPE_SOVEREIGNTY_HUB,
            owner_corp_id: owner,
            solar_system_id: sys,
            name: format!("Hub {id}"),
            state: state.into(),
            ..Default::default()
        };
        snap.structures.insert(hub_a, mk_hub(hub_a, corp1, sys_a, "anchor_vulnerable"));
        snap.structures.insert(hub_t, mk_hub(hub_t, corp2, sys_mid, "anchor_vulnerable"));
        snap.structures.insert(hub_c_relay, mk_hub(hub_c_relay, corp2, sys_far, "anchor_vulnerable"));
        snap.structures.insert(hub_c_direct_hi, mk_hub(hub_c_direct_hi, corp2, sys_far, "anchor_vulnerable"));
        snap.structures.insert(hub_c_direct_lo, mk_hub(hub_c_direct_lo, corp2, sys_far, "anchor_vulnerable"));
        snap.structures.insert(hub_destroyed, mk_hub(hub_destroyed, corp1, sys_b, "destroyed"));
        snap.structures.insert(hub_foreign, mk_hub(hub_foreign, corp3, sys_b, "anchor_vulnerable"));

        snap.sov_runtime.sov_hubs.insert(
            hub_a,
            PersistedSovHubData {
                hub_id: hub_a,
                workforce: PersistedWorkforceMode::Export {
                    destination: Some(sys_far),
                    amount: 400,
                },
                ..Default::default()
            },
        );
        snap.sov_runtime.sov_hubs.insert(
            hub_t,
            PersistedSovHubData {
                hub_id: hub_t,
                workforce: PersistedWorkforceMode::Transit,
                ..Default::default()
            },
        );
        snap.sov_runtime.sov_hubs.insert(
            hub_c_relay,
            PersistedSovHubData {
                hub_id: hub_c_relay,
                workforce: PersistedWorkforceMode::Import { sources: vec![sys_a] },
                ..Default::default()
            },
        );
        snap.sov_runtime.sov_hubs.insert(
            hub_c_direct_hi,
            PersistedSovHubData {
                hub_id: hub_c_direct_hi,
                workforce: PersistedWorkforceMode::Import { sources: vec![sys_a] },
                ..Default::default()
            },
        );
        snap.sov_runtime.sov_hubs.insert(
            hub_c_direct_lo,
            PersistedSovHubData {
                hub_id: hub_c_direct_lo,
                workforce: PersistedWorkforceMode::Import { sources: vec![sys_a] },
                ..Default::default()
            },
        );
    });

    db.with_snapshot(|snap| {
        assert_eq!(hub_sys(snap, hub_destroyed), None);
        assert!(linked_owners(snap, hub_a, hub_t));
        assert!(!linked_owners(snap, hub_a, hub_foreign));
        assert!(!linked_owners(snap, hub_a, 999_999));

        // Direct importer preferred over relay importer, lowest hub id wins tiebreak
        assert_eq!(assigned_importer(snap, &sde, hub_a, sys_far), Some(hub_c_direct_lo));
        assert_eq!(imports(snap, &sde, hub_c_direct_lo), vec![(sys_a, 400)]);
        assert_eq!(imports(snap, &sde, hub_c_direct_hi), vec![(sys_a, 0)]);

        // When export destination == hub_t's system (sys_mid), can_relay_export direct / relay paths
        assert!(can_relay_export(snap, &sde, hub_a, sys_far, hub_c_direct_lo));
        assert!(!can_relay_export(snap, &sde, hub_a, sys_far, hub_a));
        assert!(!can_relay_export(snap, &sde, hub_a, sys_b, hub_foreign));
        assert!(!can_relay_export(snap, &sde, hub_a, sys_far, hub_destroyed));
        // Unreachable destination (>5 jumps from hub_a) returns false
        assert!(!can_relay_export(snap, &sde, hub_a, sys_far, hub_destroyed));

        // encode_state covers Inactive, Transit, Export connected & reserve, Import
        assert!(!encode_state(snap, &sde, hub_foreign).is_empty());
        assert!(!encode_state(snap, &sde, hub_t).is_empty());
        assert!(!encode_state(snap, &sde, hub_a).is_empty());
        assert!(!encode_state(snap, &sde, hub_c_direct_lo).is_empty());

        let net = networkable_hubs(snap, &sde, hub_a);
        assert!(net.iter().any(|&(id, _)| id == hub_c_direct_lo));
        assert!(!net.iter().any(|&(id, _)| id == hub_destroyed || id == hub_foreign));
        assert!(networkable_hubs(snap, &sde, hub_destroyed).is_empty());
    });

    // Export with destination = sys_b (1 jump) to hub in sys_mid (via direct/relay dest_sys != imp_sys)
    // and reserve export (destination = None / no matching importer)
    db.with_mut_snapshot(|snap| {
        snap.sov_runtime.sov_hubs.get_mut(&hub_a).unwrap().workforce =
            PersistedWorkforceMode::Export {
                destination: Some(sys_far),
                amount: 100,
            };
        // Remove importers in sys_far -> only relay hub_c_relay remains, then test reserve
        snap.sov_runtime.sov_hubs.remove(&hub_c_direct_lo);
        snap.sov_runtime.sov_hubs.remove(&hub_c_direct_hi);
    });
    db.with_snapshot(|snap| {
        assert_eq!(assigned_importer(snap, &sde, hub_a, sys_far), Some(hub_c_relay));
        // Exporting to unreachable sys_far directly fails when dest_sys itself >5 jumps away
        assert!(!can_relay_export(snap, &sde, hub_a, sys_far, hub_a));
        // Reserve export state encoding
        assert_eq!(export_connected(snap, &sde, hub_t), None);
    });

    // Test decode_configuration variants: Import with bad entry, Export via field 1, Inactive fallback.
    let mut imp_bad = DynamicQuasarMessage::new("Imp");
    imp_bad
        .fields
        .insert(1, DynamicValue::Repeated(vec![DynamicValue::Int64(123)]));
    let mut cfg_imp = DynamicQuasarMessage::new("Cfg");
    cfg_imp.fields.insert(2, DynamicValue::Message(Box::new(imp_bad)));
    assert_eq!(
        decode_configuration(&cfg_imp).unwrap(),
        PersistedWorkforceMode::Import { sources: Vec::new() }
    );

    let mut imp_non_rep = DynamicQuasarMessage::new("Imp");
    imp_non_rep.fields.insert(1, DynamicValue::Int64(1));
    let mut cfg_imp2 = DynamicQuasarMessage::new("Cfg");
    cfg_imp2
        .fields
        .insert(2, DynamicValue::Message(Box::new(imp_non_rep)));
    assert_eq!(
        decode_configuration(&cfg_imp2).unwrap(),
        PersistedWorkforceMode::Import { sources: Vec::new() }
    );

    let mut exp_f1 = id_msg(1, sys_b as i64);
    exp_f1.fields.insert(2, DynamicValue::Int64(50));
    let mut cfg_exp = DynamicQuasarMessage::new("Cfg");
    cfg_exp.fields.insert(3, DynamicValue::Message(Box::new(exp_f1)));
    assert_eq!(
        decode_configuration(&cfg_exp).unwrap(),
        PersistedWorkforceMode::Export {
            destination: Some(sys_b),
            amount: 50,
        }
    );

    let cfg_inactive = DynamicQuasarMessage::new("Cfg");
    assert_eq!(
        decode_configuration(&cfg_inactive).unwrap(),
        PersistedWorkforceMode::Inactive
    );

    for transit_val in [
        DynamicValue::Bool(true),
        DynamicValue::Int64(1),
        DynamicValue::Uint64(1),
        DynamicValue::Message(Box::new(DynamicQuasarMessage::new("Transit"))),
        DynamicValue::Bytes(vec![1]),
    ] {
        let mut cfg_tr = DynamicQuasarMessage::new("Cfg");
        cfg_tr.fields.insert(4, transit_val);
        assert_eq!(
            decode_configuration(&cfg_tr).unwrap(),
            PersistedWorkforceMode::Transit
        );
    }
    for non_transit_val in [
        DynamicValue::Bool(false),
        DynamicValue::Int64(0),
        DynamicValue::Uint64(0),
    ] {
        let mut cfg_ntr = DynamicQuasarMessage::new("Cfg");
        cfg_ntr.fields.insert(4, non_transit_val);
        assert_eq!(
            decode_configuration(&cfg_ntr).unwrap(),
            PersistedWorkforceMode::Inactive
        );
    }
}

#[test]
fn rpc_errors_validation_overload_and_export_clamping() {
    let sde = shared_sde();
    let db = Arc::new(WorldRepository::with_demo_seed());
    let descriptors = Arc::new(QuasarDescriptorRegistry::empty_with_builtins());
    let router = PublicGatewayRouter::new(descriptors, Arc::clone(&db), Arc::clone(&sde));
    register_handlers(&router, Arc::clone(&db), Arc::clone(&sde));

    let cid = 90_000_001_i64;
    let corp_id = 98_200_001_i64;
    let sys_a = 30_000_142_u32;
    let reach1 = systems_within_jumps(&sde, sys_a, 1);
    let reach5 = systems_within_jumps(&sde, sys_a, WORKFORCE_TRANSFER_MAX_JUMPS);
    let reach6 = systems_within_jumps(&sde, sys_a, WORKFORCE_TRANSFER_MAX_JUMPS + 1);
    let sys_b = *reach1.keys().find(|&&s| s != sys_a).expect("adjacent sys");
    let sys_far = *reach6.keys().find(|s| !reach5.contains_key(s)).expect("6-jump sys");
    let hub_a = 80_200_001_i64;
    let hub_b = 80_200_002_i64;

    // Pick an upgrade consuming workforce > 0 so severing workforce triggers low_power = true.
    let wf_consumer_tid = sde
        .types
        .keys()
        .copied()
        .find(|&tid| {
            upgrade_definition(&sde, tid)
                .is_some_and(|d| d.workforce_required > 0 && d.power_required == 0)
        })
        .or_else(|| {
            sde.types
                .keys()
                .copied()
                .find(|&tid| upgrade_definition(&sde, tid).is_some_and(|d| d.workforce_required > 0))
        })
        .expect("SDE has a workforce-requiring hub upgrade");

    db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.corporation_id = corp_id;
            ch.solar_system_id = sys_a;
        }
        snap.corporations.insert(
            corp_id,
            { let mut __corp = eve_db::CorporationRecord::default(); __corp.corporation_id = corp_id; __corp.ceo_id = cid; __corp.ensure_wallet_divisions(); __corp },
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
        snap.sov_runtime.sov_hubs.insert(
            hub_a,
            PersistedSovHubData {
                hub_id: hub_a,
                workforce: PersistedWorkforceMode::Export {
                    destination: Some(sys_b),
                    amount: 99_999, // deliberately above max_exportable to test fixed-point clamp
                },
                upgrades: vec![PersistedHubUpgrade {
                    type_id: 87_710,
                    online: true,
                    low_power: false,
                }],
                ..Default::default()
            },
        );
        snap.sov_runtime.sov_hubs.insert(
            hub_b,
            PersistedSovHubData {
                hub_id: hub_b,
                workforce: PersistedWorkforceMode::Import { sources: vec![sys_a] },
                upgrades: vec![PersistedHubUpgrade {
                    type_id: wf_consumer_tid,
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
    let non_dir_ctx = AuthoritativeContext::for_session(
        Some(1),
        Some(cid as u32),
        Some(corp_id as u32),
        0,
        "tranquility",
        vec![1],
    );

    // Not found hub on GetStateRequest / GetConfigurationRequest / GetNetworkableHubsRequest
    assert_eq!(
        handlers["eve_public.sovereignty.hub.workforce.api.GetStateRequest"](
            &dir_ctx,
            &dummy_any,
            &id_msg(1, 89_999_999),
        )
        .unwrap_err()
        .0,
        STATUS_NOT_FOUND
    );
    assert_eq!(
        handlers["eve_public.sovereignty.hub.workforce.api.GetConfigurationRequest"](
            &dir_ctx,
            &dummy_any,
            &id_msg(1, 89_999_999),
        )
        .unwrap_err()
        .0,
        STATUS_NOT_FOUND
    );
    assert_eq!(
        handlers["eve_public.sovereignty.hub.workforce.api.GetNetworkableHubsRequest"](
            &dir_ctx,
            &dummy_any,
            &id_msg(1, 89_999_999),
        )
        .unwrap_err()
        .0,
        STATUS_NOT_FOUND
    );

    // ConfigureRequest without field 2 -> STATUS_BAD_REQUEST
    assert_eq!(
        handlers["eve_public.sovereignty.hub.workforce.api.ConfigureRequest"](
            &dir_ctx,
            &dummy_any,
            &id_msg(1, hub_a),
        )
        .unwrap_err()
        .0,
        STATUS_BAD_REQUEST
    );

    // Non-director cannot ConfigureRequest
    let mut cfg_inactive = DynamicQuasarMessage::new("Cfg");
    cfg_inactive.fields.insert(1, DynamicValue::Bool(true));
    let mut req_inactive = id_msg(1, hub_a);
    req_inactive
        .fields
        .insert(2, DynamicValue::Message(Box::new(cfg_inactive)));
    // Wait: Caller resolves CEO as u64::MAX roles, so change CEO to someone else first.
    db.with_mut_snapshot(|snap| {
        snap.corporations.get_mut(&corp_id).unwrap().ceo_id = 90_000_002;
    });
    assert_eq!(
        handlers["eve_public.sovereignty.hub.workforce.api.ConfigureRequest"](
            &non_dir_ctx,
            &dummy_any,
            &req_inactive,
        )
        .unwrap_err()
        .0,
        STATUS_FORBIDDEN
    );
    db.with_mut_snapshot(|snap| {
        snap.corporations.get_mut(&corp_id).unwrap().ceo_id = cid;
    });

    // Import from non-networkable system sys_far -> STATUS_BAD_REQUEST
    let mut imp_bad = DynamicQuasarMessage::new("Imp");
    imp_bad.fields.insert(
        1,
        DynamicValue::Repeated(vec![DynamicValue::Message(Box::new(id_msg(1, sys_far as i64)))]),
    );
    let mut cfg_bad_imp = DynamicQuasarMessage::new("Cfg");
    cfg_bad_imp
        .fields
        .insert(2, DynamicValue::Message(Box::new(imp_bad)));
    let mut req_bad_imp = id_msg(1, hub_b);
    req_bad_imp
        .fields
        .insert(2, DynamicValue::Message(Box::new(cfg_bad_imp)));
    assert_eq!(
        handlers["eve_public.sovereignty.hub.workforce.api.ConfigureRequest"](
            &dir_ctx,
            &dummy_any,
            &req_bad_imp,
        )
        .unwrap_err()
        .0,
        STATUS_BAD_REQUEST
    );

    // Export to non-networkable system sys_far -> STATUS_BAD_REQUEST
    let mut exp_bad_dest = id_msg(1, sys_far as i64);
    exp_bad_dest.fields.insert(2, DynamicValue::Int64(100));
    let mut cfg_bad_dest = DynamicQuasarMessage::new("Cfg");
    cfg_bad_dest
        .fields
        .insert(3, DynamicValue::Message(Box::new(exp_bad_dest)));
    let mut req_bad_dest = id_msg(1, hub_b);
    req_bad_dest
        .fields
        .insert(2, DynamicValue::Message(Box::new(cfg_bad_dest)));
    assert_eq!(
        handlers["eve_public.sovereignty.hub.workforce.api.ConfigureRequest"](
            &dir_ctx,
            &dummy_any,
            &req_bad_dest,
        )
        .unwrap_err()
        .0,
        STATUS_BAD_REQUEST
    );

    // Export amount exceeding spare workforce rolls back old mode and returns STATUS_BAD_REQUEST
    let mut exp_too_much = id_msg(1, sys_b as i64);
    exp_too_much.fields.insert(2, DynamicValue::Int64(9_999_999));
    let mut cfg_too_much = DynamicQuasarMessage::new("Cfg");
    cfg_too_much
        .fields
        .insert(3, DynamicValue::Message(Box::new(exp_too_much)));
    let mut req_too_much = id_msg(1, hub_a);
    req_too_much
        .fields
        .insert(2, DynamicValue::Message(Box::new(cfg_too_much)));
    assert_eq!(
        handlers["eve_public.sovereignty.hub.workforce.api.ConfigureRequest"](
            &dir_ctx,
            &dummy_any,
            &req_too_much,
        )
        .unwrap_err()
        .0,
        STATUS_BAD_REQUEST
    );
    db.with_snapshot(|snap| {
        assert_eq!(exported_amount(snap, hub_a), 99_999);
    });

    // Reconfigure Hub B to Transit -> runs fixed-point overload across Hub A (clamps export 99_999 -> max_exportable)
    // and Hub B, emitting HubUpgradesSimulatedNotice if upgrades changed low_power state.
    let mut cfg_b_transit = DynamicQuasarMessage::new("Cfg");
    cfg_b_transit.fields.insert(4, DynamicValue::Bool(true));
    let mut req_b_transit = id_msg(1, hub_b);
    req_b_transit
        .fields
        .insert(2, DynamicValue::Message(Box::new(cfg_b_transit)));
    handlers["eve_public.sovereignty.hub.workforce.api.ConfigureRequest"](
        &dir_ctx,
        &dummy_any,
        &req_b_transit,
    )
    .expect("Configure Hub B Transit triggers fixed point clamp on Hub A");

    db.with_snapshot(|snap| {
        assert!(exported_amount(snap, hub_a) < 99_999);
        assert!(snap.sov_runtime.sov_hubs[&hub_b].upgrades[0].low_power);
    });
}

#[test]
fn decode_configuration_direct_returns_non_default_variants() {
    let mut src_msg = DynamicQuasarMessage::new("Identifier");
    src_msg.fields.insert(1, DynamicValue::Int64(30_000_142));
    let mut imp_msg = DynamicQuasarMessage::new("Import");
    imp_msg.fields.insert(1, DynamicValue::Repeated(vec![DynamicValue::Message(Box::new(src_msg))]));
    let mut cfg_imp = DynamicQuasarMessage::new("Configuration");
    cfg_imp.fields.insert(2, DynamicValue::Message(Box::new(imp_msg)));
    assert_eq!(
        decode_configuration(&cfg_imp).unwrap(),
        PersistedWorkforceMode::Import { sources: vec![30_000_142] }
    );

    let mut exp_msg = id_msg(1, 30_000_144);
    exp_msg.fields.insert(2, DynamicValue::Int64(250));
    let mut cfg_exp = DynamicQuasarMessage::new("Configuration");
    cfg_exp.fields.insert(3, DynamicValue::Message(Box::new(exp_msg)));
    assert_eq!(
        decode_configuration(&cfg_exp).unwrap(),
        PersistedWorkforceMode::Export { destination: Some(30_000_144), amount: 250 }
    );

    let mut bad_exp = id_msg(1, 30_000_144);
    bad_exp.fields.insert(2, DynamicValue::Int64(-1));
    let mut cfg_bad_exp = DynamicQuasarMessage::new("Configuration");
    cfg_bad_exp.fields.insert(3, DynamicValue::Message(Box::new(bad_exp)));
    assert_eq!(decode_configuration(&cfg_bad_exp).unwrap_err().0, STATUS_BAD_REQUEST);

    let mut cfg_transit = DynamicQuasarMessage::new("Configuration");
    cfg_transit.fields.insert(4, DynamicValue::Bool(true));
    assert_eq!(decode_configuration(&cfg_transit).unwrap(), PersistedWorkforceMode::Transit);

    let cfg_empty = DynamicQuasarMessage::new("Configuration");
    assert_eq!(decode_configuration(&cfg_empty).unwrap(), PersistedWorkforceMode::Inactive);
}
