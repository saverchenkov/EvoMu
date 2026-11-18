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

fn seed_workforce_skyhook(
    snap: &mut eve_db::WorldStateSnapshot,
    sde: &SdeRegistry,
    corp_id: i64,
    sys_id: u32,
    skyhook_id: i64,
) -> (i64, u64) {
    let (planet_id, wf) = sde
        .celestials_in_system(sys_id)
        .into_iter()
        .filter(|c| c.group_id == 7)
        .filter_map(|c| {
            let (_, wf) = super::super::quasar_sov_hub::planet_yield_for_planet(c.type_id, c.radius);
            (wf > 0).then_some((c.celestial_id, wf))
        })
        .max_by_key(|&(_, wf)| wf)
        .expect("system must have at least one workforce planet");
    snap.structures.insert(
        skyhook_id,
        UpwellStructureRecord {
            structure_id: skyhook_id,
            type_id: super::super::sov_services::TYPE_ORBITAL_SKYHOOK,
            owner_corp_id: corp_id,
            solar_system_id: sys_id,
            name: format!("Workforce Skyhook {skyhook_id}"),
            state: "ShieldVulnerable".into(),
            shield_hp: crate::services::sov_gameplay::SKYHOOK_BASE_SHIELD_HP,
            armor_hp: crate::services::sov_gameplay::SKYHOOK_BASE_ARMOR_HP,
            hull_hp: crate::services::sov_gameplay::SKYHOOK_BASE_HULL_HP,
            ..Default::default()
        },
    );
    snap.sov_runtime.skyhook_planets.insert(skyhook_id, planet_id);
    snap.sov_runtime.skyhook_active.insert(skyhook_id, true);
    (planet_id, wf)
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
        // Give Hub A an active workforce Skyhook (for exportable workforce) + upgrade 87_710
        seed_workforce_skyhook(snap, &sde, corp_id, sys_a, hub_a + 100);
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
                && super::super::quasar_sov_hub::star_power_for_system(&sde, s) >= 250
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

        // Hub A harvests planetary workforce via active Skyhook so it can export 2,000, plus 87_710
        seed_workforce_skyhook(snap, &sde, corp_a, sys_a, hub_a + 100);
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

#[test]
fn multi_hop_cyclic_transit_relays_and_import_validation_guards() {
    let sde = shared_sde();
    let db = Arc::new(WorldRepository::with_demo_seed());
    let descriptors = Arc::new(QuasarDescriptorRegistry::empty_with_builtins());
    let router = PublicGatewayRouter::new(descriptors, Arc::clone(&db), Arc::clone(&sde));
    register_handlers(&router, Arc::clone(&db), Arc::clone(&sde));

    // Real 6-hop claimable null-sec path in SDE:
    // N3-JBX(0) -> GN-PDU(1) -> 8Q-T7B(2) -> HJ-BCH(3) -> QPTT-F(4) -> PU-UMM(5) -> 6-KPAB(6)
    let sys_0 = 30_000_669_u32;
    let sys_1 = 30_000_671_u32;
    let sys_2 = 30_000_697_u32;
    let sys_4 = 30_000_722_u32;
    let sys_6 = 30_000_728_u32;
    let sys_branch = 30_000_670_u32; // extra 1j neighbor for >3 import check

    let cid = 90_000_001_i64;
    let corp_id = 1_000_009_i64;
    let (hub_0, hub_1, hub_2, hub_4, hub_6, hub_b) = (
        82_000_000_i64,
        82_000_001_i64,
        82_000_002_i64,
        82_000_004_i64,
        82_000_006_i64,
        82_000_007_i64,
    );

    db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.corporation_id = corp_id;
            ch.solar_system_id = sys_0;
        }
        snap.corporations.insert(corp_id, {
            let mut c = eve_db::CorporationRecord::default();
            c.corporation_id = corp_id;
            c.ceo_id = cid;
            c.ensure_wallet_divisions();
            c
        });
        for (hid, sys) in [
            (hub_0, sys_0),
            (hub_1, sys_1),
            (hub_2, sys_2),
            (hub_4, sys_4),
            (hub_6, sys_6),
            (hub_b, sys_branch),
        ] {
            snap.structures.insert(
                hid,
                UpwellStructureRecord {
                    structure_id: hid,
                    type_id: TYPE_SOVEREIGNTY_HUB,
                    owner_corp_id: corp_id,
                    solar_system_id: sys,
                    name: format!("Hub {hid}"),
                    state: "anchor_vulnerable".into(),
                    ..Default::default()
                },
            );
        }
        // Hub 0 has active workforce Skyhook + upgrade 87_710 so it can export 1,000 wf.
        seed_workforce_skyhook(snap, &sde, corp_id, sys_0, hub_0 + 100);
        snap.sov_runtime.sov_hubs.insert(
            hub_0,
            PersistedSovHubData {
                hub_id: hub_0,
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
    let any = GoogleAny::default();
    let dir_ctx = AuthoritativeContext::for_session(
        Some(1),
        Some(cid as u32),
        Some(corp_id as u32),
        corp_roles::ROLE_DIRECTOR,
        "tranquility",
        vec![1],
    );

    // Put Hub 1, Hub 2, and Hub 4 in Transit (forming bidirectional cycle sys_1 <-> sys_2 through shared relay)
    for tid in [hub_1, hub_2, hub_4] {
        let mut tcfg = DynamicQuasarMessage::new("Cfg");
        tcfg.fields.insert(4, DynamicValue::Bool(true));
        let mut req = id_msg(1, tid);
        req.fields.insert(2, DynamicValue::Message(Box::new(tcfg)));
        handlers["eve_public.sovereignty.hub.workforce.api.ConfigureRequest"](&dir_ctx, &any, &req)
            .expect("configure Transit relay");
    }

    // Hub 6 (> 5 jumps from sys_0!) is reachable via multi-hop Transit relays and does not loop!
    db.with_snapshot(|snap| {
        let net = networkable_hubs(snap, &sde, hub_0);
        assert!(
            net.iter().any(|(hid, _)| *hid == hub_6),
            "6-hop destination hub must be reachable via chained Transit relays"
        );
    });

    // Export from hub_0 -> sys_6 and import on hub_6 from sys_0 across cyclic relays
    let mut exp_0 = id_msg(1, sys_6 as i64);
    exp_0.fields.insert(2, DynamicValue::Int64(1_000));
    let mut cfg_0 = DynamicQuasarMessage::new("Cfg");
    cfg_0.fields.insert(3, DynamicValue::Message(Box::new(exp_0)));
    let mut req_0 = id_msg(1, hub_0);
    req_0.fields.insert(2, DynamicValue::Message(Box::new(cfg_0)));
    handlers["eve_public.sovereignty.hub.workforce.api.ConfigureRequest"](&dir_ctx, &any, &req_0)
        .expect("export to 6-hop destination");

    let mut imp_6 = DynamicQuasarMessage::new("Imp");
    imp_6.fields.insert(1, DynamicValue::Repeated(vec![DynamicValue::Message(Box::new(id_msg(1, sys_0 as i64)))]));
    let mut cfg_6 = DynamicQuasarMessage::new("Cfg");
    cfg_6.fields.insert(2, DynamicValue::Message(Box::new(imp_6)));
    let mut req_6 = id_msg(1, hub_6);
    req_6.fields.insert(2, DynamicValue::Message(Box::new(cfg_6)));
    handlers["eve_public.sovereignty.hub.workforce.api.ConfigureRequest"](&dir_ctx, &any, &req_6)
        .expect("import from 6-hop source across Transit cycle");

    db.with_snapshot(|snap| {
        assert_eq!(export_connected(snap, &sde, hub_0), Some(sys_6));
        assert_eq!(imports(snap, &sde, hub_6), vec![(sys_0, 1_000)]);
    });

    // Negative import guards: self-import, duplicate source, and > MAX_IMPORT_SOURCES (4 > 3)
    for sources in [
        vec![sys_0],                        // self-import on hub_0
        vec![sys_1, sys_1],                 // duplicate source on hub_0
        vec![sys_1, sys_2, sys_4, sys_branch], // 4 sources (> MAX_IMPORT_SOURCES=3)
    ] {
        let mut imp = DynamicQuasarMessage::new("Imp");
        imp.fields.insert(
            1,
            DynamicValue::Repeated(
                sources
                    .into_iter()
                    .map(|s| DynamicValue::Message(Box::new(id_msg(1, i64::from(s)))))
                    .collect(),
            ),
        );
        let mut cfg = DynamicQuasarMessage::new("Cfg");
        cfg.fields.insert(2, DynamicValue::Message(Box::new(imp)));
        let mut req = id_msg(1, hub_0);
        req.fields.insert(2, DynamicValue::Message(Box::new(cfg)));
        assert_eq!(
            handlers["eve_public.sovereignty.hub.workforce.api.ConfigureRequest"](&dir_ctx, &any, &req)
                .unwrap_err()
                .0,
            STATUS_BAD_REQUEST
        );
    }

    // Valid import with MAX_IMPORT_SOURCES = 3 succeeds on hub_0
    let mut imp_3 = DynamicQuasarMessage::new("Imp");
    imp_3.fields.insert(
        1,
        DynamicValue::Repeated(
            [sys_1, sys_2, sys_branch]
                .into_iter()
                .map(|s| DynamicValue::Message(Box::new(id_msg(1, i64::from(s)))))
                .collect(),
        ),
    );
    let mut cfg_3 = DynamicQuasarMessage::new("Cfg");
    cfg_3.fields.insert(2, DynamicValue::Message(Box::new(imp_3)));
    let mut req_3 = id_msg(1, hub_0);
    req_3.fields.insert(2, DynamicValue::Message(Box::new(cfg_3)));
    handlers["eve_public.sovereignty.hub.workforce.api.ConfigureRequest"](&dir_ctx, &any, &req_3)
        .expect("import with exact MAX_IMPORT_SOURCES = 3 succeeds");
}

#[test]
fn workforce_mecha_tooling_local_only_and_mutation_reconcile_clamping() {
    use crate::services::quasar_sov_hub::{
        compute_hub_resources, enforce_hub_power_overload, max_exportable_workforce,
        planet_yield_for_planet, register_handlers as register_hub_handlers, run_simulation,
        star_power_for_system,
    };
    use crate::services::sov_services::{TYPE_ORBITAL_SKYHOOK, TYPE_SUPERIONIC_ICE};
    use eve_db::{InventoryFlag, InventoryItemRecord};
    use eve_proto::quasar::GoogleTimestamp;

    let sde = shared_sde();
    let db = Arc::new(WorldRepository::with_demo_seed());
    let descriptors = Arc::new(QuasarDescriptorRegistry::empty_with_builtins());
    let router = PublicGatewayRouter::new(descriptors, Arc::clone(&db), Arc::clone(&sde));
    register_handlers(&router, Arc::clone(&db), Arc::clone(&sde));
    register_hub_handlers(&router, Arc::clone(&db), Arc::clone(&sde));

    let cid = 90_000_001_i64;
    let corp_id = 1_000_009_i64;
    let sys_a = 30_000_142_u32;
    let sys_b = *systems_within_jumps(&sde, sys_a, WORKFORCE_TRANSFER_MAX_JUMPS)
        .keys()
        .find(|&&s| s != sys_a && star_power_for_system(&sde, s) >= 200)
        .expect("neighbor system with >= 200 star power within jump range");
    let gas_planet = sde
        .celestials_in_system(sys_a)
        .into_iter()
        .filter(|c| c.group_id == 7 && c.type_id == 13)
        .max_by_key(|c| planet_yield_for_planet(c.type_id, c.radius).0)
        .map(|c| c.celestial_id)
        .expect("gas planet in Jita");

    let hub_a = 77_700_001_i64;
    let hub_b = 77_700_002_i64;
    let skyhook_temp = 77_700_010_i64;
    let skyhook_gas = 77_700_011_i64;
    let item_tooling = 88_700_001_i64;
    let item_threat2 = 88_700_002_i64;
    let item_threat1_b = 88_700_003_i64;
    let item_ice = 88_700_004_i64;

    let mut local_wf = 0_u64;
    db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.corporation_id = corp_id;
            ch.solar_system_id = sys_a;
        }
        snap.corporations.insert(corp_id, {
            let mut corp = eve_db::CorporationRecord::default();
            corp.corporation_id = corp_id;
            corp.ceo_id = cid;
            corp
        });
        for (hid, sys) in [(hub_a, sys_a), (hub_b, sys_b)] {
            snap.structures.insert(
                hid,
                UpwellStructureRecord {
                    structure_id: hid,
                    type_id: TYPE_SOVEREIGNTY_HUB,
                    owner_corp_id: corp_id,
                    solar_system_id: sys,
                    name: format!("Hub {hid}"),
                    state: "anchor_vulnerable".into(),
                    ..Default::default()
                },
            );
            snap.sov_runtime.sov_hubs.insert(
                hid,
                PersistedSovHubData {
                    hub_id: hid,
                    last_updated_unix: 10_000,
                    ..Default::default()
                },
            );
        }
        let (_temp_planet, wf) = seed_workforce_skyhook(snap, &sde, corp_id, sys_a, skyhook_temp);
        local_wf = wf;
        snap.structures.insert(
            skyhook_gas,
            UpwellStructureRecord {
                structure_id: skyhook_gas,
                type_id: TYPE_ORBITAL_SKYHOOK,
                owner_corp_id: corp_id,
                solar_system_id: sys_a,
                name: "Gas Skyhook".into(),
                state: "Online".into(),
                ..Default::default()
            },
        );
        snap.sov_runtime.skyhook_planets.insert(skyhook_gas, gas_planet);
        snap.sov_runtime.skyhook_active.insert(skyhook_gas, true);

        for (item_id, type_id, qty, loc) in [
            (item_tooling, 87_710_u32, 1_i32, hub_a),
            (item_threat2, 82_500_u32, 1_i32, hub_a),
            (item_threat1_b, 82_499_u32, 1_i32, hub_b),
            (item_ice, TYPE_SUPERIONIC_ICE, 200_i32, hub_a),
        ] {
            snap.items.insert(
                item_id,
                InventoryItemRecord {
                    item_id,
                    type_id,
                    owner_id: cid,
                    location_id: loc,
                    flag: InventoryFlag::Hangar,
                    quantity: qty,
                    singleton: false,
                    version: 1,
                    ..Default::default()
                },
            );
        }
    });
    assert!(local_wf >= 5_000, "Temperate skyhook expected >= 5_000 workforce, got {local_wf}");
    let export_nominal = 4_500_u64;
    let expected_clamped = local_wf - 3_800;
    assert!(expected_clamped < 1_800, "clamped export {expected_clamped} must starve 82_499 (1_800)");

    let handlers = router.handlers.read().unwrap();
    let any = GoogleAny::default();
    let mut dir_ctx = AuthoritativeContext::for_session(
        Some(1),
        Some(cid as u32),
        Some(corp_id as u32),
        corp_roles::ROLE_DIRECTOR,
        "tranquility",
        vec![1],
    );
    dir_ctx.received = Some(GoogleTimestamp::from_unix(10_000, 0));

    let mk_export_req = |hub_id: i64, dest: u32, amt: i64| {
        let mut exp = id_msg(1, i64::from(dest));
        exp.fields.insert(2, DynamicValue::Int64(amt));
        let mut cfg = DynamicQuasarMessage::new("Cfg");
        cfg.fields.insert(3, DynamicValue::Message(Box::new(exp)));
        let mut req = id_msg(1, hub_id);
        req.fields.insert(2, DynamicValue::Message(Box::new(cfg)));
        req
    };
    let mk_import_req = |hub_id: i64, sources: &[u32]| {
        let srcs = sources
            .iter()
            .map(|&s| DynamicValue::Message(Box::new(id_msg(1, i64::from(s)))))
            .collect();
        let mut imp = DynamicQuasarMessage::new("Imp");
        imp.fields.insert(1, DynamicValue::Repeated(srcs));
        let mut cfg = DynamicQuasarMessage::new("Cfg");
        cfg.fields.insert(2, DynamicValue::Message(Box::new(imp)));
        let mut req = id_msg(1, hub_id);
        req.fields.insert(2, DynamicValue::Message(Box::new(cfg)));
        req
    };
    let mk_proc_cfg_req = |hub_id: i64, new_items: &[i64], states: &[(u32, bool)]| {
        let new_msgs = new_items
            .iter()
            .map(|&id| {
                let mut nu = DynamicQuasarMessage::new("NewUpgrade");
                nu.fields.insert(1, DynamicValue::Int64(id));
                DynamicValue::Message(Box::new(nu))
            })
            .collect();
        let state_msgs = states
            .iter()
            .map(|&(tid, online)| {
                let mut e = id_msg(1, i64::from(tid));
                e.fields.insert(2, DynamicValue::Bool(online));
                DynamicValue::Message(Box::new(e))
            })
            .collect();
        let mut req = id_msg(3, hub_id);
        if !new_items.is_empty() {
            req.fields.insert(1, DynamicValue::Repeated(new_msgs));
        }
        req.fields.insert(2, DynamicValue::Repeated(state_msgs));
        req
    };
    let mk_add_fuel_req = |hub_id: i64, fuel_item: i64, amt: i64| {
        let mut inner = DynamicQuasarMessage::new("Identifier");
        inner.fields.insert(1, DynamicValue::Int64(hub_id));
        let mut req = id_msg(1, fuel_item);
        req.fields.insert(2, DynamicValue::Int64(amt));
        req.fields.insert(3, DynamicValue::Message(Box::new(inner)));
        req
    };
    let mk_uninstall_req = |hub_id: i64, type_id: u32| {
        let mut upg_id = id_msg(1, hub_id);
        upg_id.fields.insert(2, DynamicValue::Int64(i64::from(type_id)));
        let mut req = DynamicQuasarMessage::new("Uninstall");
        req.fields.insert(1, DynamicValue::Message(Box::new(upg_id)));
        req
    };

    // 1. Add Superionic Ice and install/online [87_710 (+5_000 wf), 82_500 (-3_800 wf)] on Hub A.
    handlers["eve_public.sovereignty.hub.fuel.api.AddRequest"](
        &dir_ctx,
        &any,
        &mk_add_fuel_req(hub_a, item_ice, 50),
    )
    .expect("add ice");
    handlers["eve_public.sovereignty.hub.upgrade.api.ProcessConfigurationRequest"](
        &dir_ctx,
        &any,
        &mk_proc_cfg_req(hub_a, &[item_tooling, item_threat2], &[(87_710, true), (82_500, true)]),
    )
    .expect("install and online Mecha-Tooling 1 + Minor Threat 2 on Hub A");

    // 2. Mecha-Tooling 1's +5_000 workforce is strictly local-only: exporting > local_wf must fail.
    let err_export = handlers["eve_public.sovereignty.hub.workforce.api.ConfigureRequest"](
        &dir_ctx,
        &any,
        &mk_export_req(hub_a, sys_b, (local_wf + 500) as i64),
    )
    .unwrap_err();
    assert_eq!(err_export.0, STATUS_BAD_REQUEST);

    // 3. Forced legacy excess-export invariant: compute_hub_resources, imports(), and enforce_hub_power_overload
    //    must deduct uncompensated excess export from produced_w and clamp received imports cleanly.
    db.with_mut_snapshot(|snap| {
        let hub = snap.sov_runtime.sov_hubs.get_mut(&hub_a).unwrap();
        hub.workforce = PersistedWorkforceMode::Export {
            destination: Some(sys_b),
            amount: local_wf + 2_000,
        };
        let hub_b_data = snap.sov_runtime.sov_hubs.get_mut(&hub_b).unwrap();
        hub_b_data.workforce = PersistedWorkforceMode::Import { sources: vec![sys_a] };

        let res_a = compute_hub_resources(snap, &sde, hub_a).unwrap();
        assert_eq!(res_a.workforce_local, local_wf);
        assert_eq!(
            res_a.workforce_available, 3_000,
            "excess export above local Skyhook pool (2_000) must be deducted from 87_710's 5_000 output"
        );
        assert_eq!(
            max_exportable_workforce(snap, &sde, hub_a),
            Some(local_wf),
            "Mecha-Tooling before 82_500 covers 82_500 locally without entering exportable pool"
        );
        assert_eq!(
            imports(snap, &sde, hub_b),
            vec![(sys_a, local_wf)],
            "importer must never receive phantom workforce beyond exporter's max_exportable_workforce"
        );
        assert!(
            enforce_hub_power_overload(snap, &sde, hub_a),
            "3_800 demand > 3_000 remaining local+produced workforce must drop 82_500 to low_power"
        );
        assert!(snap.sov_runtime.sov_hubs[&hub_a].upgrades[1].low_power);
        // Restore clean active state for RPC cascade tests below.
        let hub = snap.sov_runtime.sov_hubs.get_mut(&hub_a).unwrap();
        hub.upgrades[1].low_power = false;
        hub.workforce = PersistedWorkforceMode::Inactive;
        snap.sov_runtime.sov_hubs.get_mut(&hub_b).unwrap().workforce = PersistedWorkforceMode::Inactive;
    });

    // 4. Configure valid export 4_500 (<= local_wf) on Hub A -> Hub B (running 82_499 requiring 1_800).
    handlers["eve_public.sovereignty.hub.workforce.api.ConfigureRequest"](
        &dir_ctx,
        &any,
        &mk_export_req(hub_a, sys_b, export_nominal as i64),
    )
    .expect("valid export <= local_wf succeeds");
    handlers["eve_public.sovereignty.hub.workforce.api.ConfigureRequest"](
        &dir_ctx,
        &any,
        &mk_import_req(hub_b, &[sys_a]),
    )
    .expect("Hub B import succeeds");
    handlers["eve_public.sovereignty.hub.upgrade.api.ProcessConfigurationRequest"](
        &dir_ctx,
        &any,
        &mk_proc_cfg_req(hub_b, &[item_threat1_b], &[(82_499, true)]),
    )
    .expect("Hub B installs and onlines 82_499");
    db.with_snapshot(|snap| {
        assert_eq!(exported_amount(snap, hub_a), export_nominal);
        assert_eq!(imports(snap, &sde, hub_b), vec![(sys_a, export_nominal)]);
        assert!(!snap.sov_runtime.sov_hubs[&hub_b].upgrades[0].low_power);
    });

    let take_notice_urls = || -> Vec<String> {
        let mut guard = router.pending_notices.write().unwrap();
        guard.drain(..).map(|(_, a)| a.type_url).collect()
    };

    // 5a. ProcessConfigurationRequest offlining 87_710 on Hub A reconciles network:
    //     Hub A export clamps 4_500 -> local_wf - 3_800, Hub B 82_499 drops to low_power = true.
    let _ = take_notice_urls();
    handlers["eve_public.sovereignty.hub.upgrade.api.ProcessConfigurationRequest"](
        &dir_ctx,
        &any,
        &mk_proc_cfg_req(hub_a, &[], &[(87_710, false)]),
    )
    .expect("offline 87_710");
    let notices_offline = take_notice_urls();
    assert!(notices_offline
        .iter()
        .any(|u| u.ends_with("workforce.api.StateChangedNotice")));
    assert!(notices_offline
        .iter()
        .any(|u| u.ends_with("hub.api.ResourcesSimulatedNotice")));
    db.with_snapshot(|snap| {
        assert_eq!(exported_amount(snap, hub_a), expected_clamped);
        assert_eq!(imports(snap, &sde, hub_b), vec![(sys_a, expected_clamped)]);
        assert!(snap.sov_runtime.sov_hubs[&hub_b].upgrades[0].low_power);
    });

    // 5b. Re-online 87_710, restore Export 4_500 + Hub B 82_499, set Ice to 8 (1 hour), then run_simulation(+7200s) -> starvation!
    handlers["eve_public.sovereignty.hub.upgrade.api.ProcessConfigurationRequest"](
        &dir_ctx,
        &any,
        &mk_proc_cfg_req(hub_a, &[], &[(87_710, true)]),
    )
    .expect("re-online 87_710");
    handlers["eve_public.sovereignty.hub.workforce.api.ConfigureRequest"](
        &dir_ctx,
        &any,
        &mk_export_req(hub_a, sys_b, export_nominal as i64),
    )
    .expect("restore export 4_500");
    handlers["eve_public.sovereignty.hub.upgrade.api.ProcessConfigurationRequest"](
        &dir_ctx,
        &any,
        &mk_proc_cfg_req(hub_b, &[], &[(82_499, true)]),
    )
    .expect("re-online Hub B 82_499");
    db.with_mut_snapshot(|snap| {
        snap.sov_runtime
            .sov_hubs
            .get_mut(&hub_a)
            .unwrap()
            .fuel
            .insert(TYPE_SUPERIONIC_ICE, 8);
    });

    let _ = take_notice_urls();
    db.with_mut_snapshot(|snap| {
        run_simulation(snap, &sde, &router.pending_notices, hub_a, 17_200);
        assert!(snap.sov_runtime.sov_hubs[&hub_a].upgrades[0].low_power);
        assert_eq!(exported_amount(snap, hub_a), expected_clamped);
        assert_eq!(imports(snap, &sde, hub_b), vec![(sys_a, expected_clamped)]);
        assert!(snap.sov_runtime.sov_hubs[&hub_b].upgrades[0].low_power);
    });
    let notices_starve = take_notice_urls();
    assert!(notices_starve
        .iter()
        .any(|u| u.ends_with("workforce.api.StateChangedNotice")));
    assert!(notices_starve
        .iter()
        .any(|u| u.ends_with("hub.api.ResourcesSimulatedNotice")));

    // 5c. hub.fuel.api.AddRequest restores starved 87_710 and reconciles network resources/notices.
    dir_ctx.received = Some(GoogleTimestamp::from_unix(17_300, 0));
    handlers["eve_public.sovereignty.hub.fuel.api.AddRequest"](
        &dir_ctx,
        &any,
        &mk_add_fuel_req(hub_a, item_ice, 50),
    )
    .expect("refuel Hub A with Superionic Ice");
    let notices_refuel = take_notice_urls();
    assert!(notices_refuel
        .iter()
        .any(|u| u.ends_with("hub.api.ResourcesSimulatedNotice")));
    db.with_snapshot(|snap| {
        assert!(!snap.sov_runtime.sov_hubs[&hub_a].upgrades[0].low_power);
    });

    // 5d. hub.upgrade.api.UninstallRequest removing 87_710 while exporting 4_500 clamps Hub A and drops Hub B.
    handlers["eve_public.sovereignty.hub.workforce.api.ConfigureRequest"](
        &dir_ctx,
        &any,
        &mk_export_req(hub_a, sys_b, export_nominal as i64),
    )
    .expect("restore export 4_500 before uninstall test");
    handlers["eve_public.sovereignty.hub.upgrade.api.ProcessConfigurationRequest"](
        &dir_ctx,
        &any,
        &mk_proc_cfg_req(hub_b, &[], &[(82_499, true)]),
    )
    .expect("re-online Hub B 82_499 before uninstall test");
    let _ = take_notice_urls();
    handlers["eve_public.sovereignty.hub.upgrade.api.UninstallRequest"](
        &dir_ctx,
        &any,
        &mk_uninstall_req(hub_a, 87_710),
    )
    .expect("uninstall 87_710 reconciles network");
    let notices_uninstall = take_notice_urls();
    assert!(notices_uninstall
        .iter()
        .any(|u| u.ends_with("workforce.api.StateChangedNotice")));
    assert!(notices_uninstall
        .iter()
        .any(|u| u.ends_with("hub.api.ResourcesSimulatedNotice")));
    db.with_snapshot(|snap| {
        assert_eq!(snap.sov_runtime.sov_hubs[&hub_a].upgrades.len(), 1);
        assert_eq!(exported_amount(snap, hub_a), expected_clamped);
        assert!(snap.sov_runtime.sov_hubs[&hub_b].upgrades[0].low_power);
    });
}
