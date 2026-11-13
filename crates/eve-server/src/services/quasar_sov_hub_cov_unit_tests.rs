//! Comprehensive unit tests for `quasar_sov_hub` covering spectral star power, Mercenary Den
//! workforce siphoning, startup fuel & hack boundaries, corp hangar item access, and simulation notices.

#![forbid(unsafe_code)]

use super::*;
use crate::quasar_gateway::{
    PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_FORBIDDEN, STATUS_INTERNAL_ERROR,
    STATUS_UNAUTHORIZED,
};
use eve_db::durable_domain_types::PersistedMercenaryDen;
use eve_db::{CorporationRecord, InventoryFlag, InventoryItemRecord, PersistedHubUpgrade, PersistedSovHubData, UpwellStructureRecord};
use eve_proto::quasar::{AuthoritativeContext, DynamicQuasarMessage, DynamicValue, QuasarDescriptorRegistry};
use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock, RwLock};

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
fn tx_error_spectral_star_power_upgrade_defs_and_proto_field_extractors() {
    let tx_err = TxError::from_wal_error("disk_full".into());
    assert_eq!(tx_err.0, STATUS_INTERNAL_ERROR);
    assert_eq!(tx_err.1, "disk_full");

    assert_eq!(split_tier("Major Threat Detection Array 3"), ("Major Threat Detection Array", 3));
    assert_eq!(split_tier("Trailing Space "), ("Trailing Space ", 1));
    assert_eq!(split_tier("Alpha Beta"), ("Alpha Beta", 1));

    assert_eq!(power_state(&PersistedHubUpgrade { type_id: 1, online: false, low_power: true }), 1);
    assert_eq!(power_state(&PersistedHubUpgrade { type_id: 1, online: true, low_power: false }), 2);
    assert_eq!(power_state(&PersistedHubUpgrade { type_id: 1, online: true, low_power: true }), 3);

    // star_power for all spectral classes O/B/A/F/G/K/M + non-sun fallback (300)
    let mut sde = (*shared_sde()).clone();
    let base_type = sde.types.values().next().unwrap().clone();
    for (idx, (name, exp)) in [
        ("Sun O1 (Blue)", 800_u64),
        ("Sun B0 (Blue)", 700),
        ("Sun A0 (Blue-White)", 600),
        ("Sun F0 (White)", 500),
        ("Sun G5 (Yellow)", 400),
        ("Sun K5 (Orange)", 300),
        ("Sun M0 (Red)", 200),
        ("Sun X9 (Exotic)", 300),
        ("Not A Sun", 300),
    ]
    .into_iter()
    .enumerate()
    {
        let tid = 996_000 + idx as u32;
        let mut t = base_type.clone();
        t.type_id = tid;
        t.name = name.into();
        sde.types.insert(tid, t);
        assert_eq!(star_power(&sde, tid), exp);
    }
    assert_eq!(star_power(&sde, 999_999_999), 300);

    // upgrade_definition: unpublished or unmapped type in SOV_HUB_UPGRADE_GROUPS returns None;
    // resource management & system effect generator groups return expected startup/exclusivity.
    let mut unpub = sde.types.get(&81_623).unwrap().clone();
    unpub.type_id = 996_101;
    unpub.published = false;
    sde.types.insert(996_101, unpub);
    assert!(upgrade_definition(&sde, 996_101).is_none());

    let mut unmapped = sde.types.get(&81_623).unwrap().clone();
    unmapped.type_id = 996_102;
    unmapped.group_id = 4772;
    sde.types.insert(996_102, unmapped);
    assert!(upgrade_definition(&sde, 996_102).is_none());

    for tid in [81_621, 81_619, 81_615, 82_500, 82_579, 82_580, 87_702, 87_948, 87_815, 87_703, 87_710, 88_229] {
        let d = upgrade_definition(&sde, tid).expect("canonical upgrade def");
        if d.fuel_type > 0 {
            assert_eq!(d.fuel_startup, d.fuel_per_hour * 2);
        } else {
            assert_eq!(d.fuel_startup, 0);
        }
    }

    // upgrade_identifier & repeated helper edge cases
    assert_eq!(upgrade_identifier(&DynamicQuasarMessage::new("Empty"), 1).unwrap_err().0, STATUS_BAD_REQUEST);
    let mut missing_type = DynamicQuasarMessage::new("Outer");
    missing_type.fields.insert(1, DynamicValue::Message(Box::new(id_msg(1, 500))));
    assert_eq!(upgrade_identifier(&missing_type, 1).unwrap_err().0, STATUS_BAD_REQUEST);

    // Field 3 fallback on upgrade_identifier
    let mut f3_id = id_msg(1, 500);
    f3_id.fields.insert(3, DynamicValue::Int64(82_499));
    let mut f3_outer = DynamicQuasarMessage::new("Outer");
    f3_outer.fields.insert(1, DynamicValue::Message(Box::new(f3_id)));
    assert_eq!(upgrade_identifier(&f3_outer, 1).unwrap(), (500, 82_499));

    let single_msg = id_msg(1, 77);
    assert_eq!(repeated(&single_msg, 1).len(), 1);
    assert!(repeated(&single_msg, 9).is_empty());
    let mut mixed_rep = DynamicQuasarMessage::new("Mix");
    mixed_rep.fields.insert(
        2,
        DynamicValue::Repeated(vec![
            DynamicValue::Int64(1),
            DynamicValue::Message(Box::new(DynamicQuasarMessage::new("Item"))),
        ]),
    );
    assert_eq!(repeated(&mixed_rep, 2).len(), 1);
}

#[test]
fn caller_ownership_item_access_mercden_siphon_and_process_configuration_boundaries() {
    let sde = shared_sde();
    let db = WorldRepository::with_demo_seed();
    let cid = 90_000_001_i64;
    let corp_id = 1_000_009_i64;
    let alt_corp_id = 98_500_001_i64;
    let alliance_id = 99_500_001_i64;
    let hub_id = 78_000_001_i64;
    let skyhook_id = 78_000_002_i64;
    let sys_id = 30_000_142_u32;

    let temp_planet = sde
        .celestials_in_system(sys_id)
        .into_iter()
        .find(|c| c.group_id == GROUP_PLANET && planet_yield(c.type_id).1 > 0)
        .unwrap_or_else(|| {
            sde.celestials_in_system(sys_id)
                .into_iter()
                .find(|c| c.group_id == GROUP_PLANET)
                .unwrap()
        });

    // Unauthenticated & corp-less caller checks
    let unauth = AuthoritativeContext::default();
    db.with_snapshot(|snap| {
        assert_eq!(caller(&unauth, snap).unwrap_err().0, STATUS_UNAUTHORIZED);
    });
    let no_corp_ctx = AuthoritativeContext::for_session(Some(1), Some(77_888_999), None, 0, "tq", vec![1]);
    db.with_snapshot(|snap| {
        assert_eq!(caller(&no_corp_ctx, snap).unwrap_err().0, STATUS_FORBIDDEN);
    });

    db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&cid).unwrap();
        ch.corporation_id = corp_id;
        ch.alliance_id = Some(alliance_id);
        ch.solar_system_id = sys_id;
        ch.active_ship_id = 88_100;
        let mut corp_rec = snap.corporations.values().next().unwrap().clone();
        corp_rec.corporation_id = alt_corp_id;
        corp_rec.alliance_id = Some(alliance_id);
        snap.corporations.insert(alt_corp_id, corp_rec);

        snap.structures.insert(
            hub_id,
            UpwellStructureRecord {
                structure_id: hub_id,
                type_id: TYPE_SOVEREIGNTY_HUB,
                owner_corp_id: corp_id,
                solar_system_id: sys_id,
                state: "anchor_vulnerable".into(),
                ..Default::default()
            },
        );
        snap.structures.insert(
            skyhook_id,
            UpwellStructureRecord {
                structure_id: skyhook_id,
                type_id: TYPE_ORBITAL_SKYHOOK,
                owner_corp_id: corp_id,
                solar_system_id: sys_id,
                state: "anchor_vulnerable".into(),
                ..Default::default()
            },
        );
        snap.sov_runtime.skyhook_planets.insert(skyhook_id, temp_planet.celestial_id);
        snap.sov_runtime.skyhook_active.insert(skyhook_id, true);

        // Mercenary Den with high anarchy_level siphons planet workforce!
        let full_res = compute_hub_resources(snap, &sde, hub_id).unwrap();
        snap.sov_runtime.mercenary_dens.insert(
            501,
            PersistedMercenaryDen {
                den_id: 501,
                type_id: 85_230,
                owner_char_id: cid,
                skyhook_id,
                solar_system_id: sys_id,
                planet_id: temp_planet.celestial_id,
                enabled: true,
                anarchy_level: 500_000,
                ..Default::default()
            },
        );
        let siphoned_res = compute_hub_resources(snap, &sde, hub_id).unwrap();
        if full_res.workforce_local > 0 {
            assert!(siphoned_res.workforce_local <= full_res.workforce_local);
        }

        // Alt-corp structure in same alliance passes `owned_structure` but fails `corp_owned_structure`!
        let alt_hub = hub_id + 99;
        snap.structures.insert(
            alt_hub,
            UpwellStructureRecord {
                structure_id: alt_hub,
                type_id: TYPE_SOVEREIGNTY_HUB,
                owner_corp_id: alt_corp_id,
                solar_system_id: sys_id,
                state: "anchor_vulnerable".into(),
                ..Default::default()
            },
        );
        let c = Caller {
            cid,
            corp: corp_id,
            alliance: Some(alliance_id),
            roles: corp_roles::ROLE_DIRECTOR,
        };
        assert!(owned_structure(snap, &c, alt_hub, &[TYPE_SOVEREIGNTY_HUB], "Hub").is_ok());
        assert_eq!(
            corp_owned_structure(snap, &c, alt_hub, &[TYPE_SOVEREIGNTY_HUB], "Hub").unwrap_err().0,
            STATUS_FORBIDDEN
        );

        // item_at_hub: CorpSAG1 owned by corp + SpecializedAsteroidHold in active ship + wrong-flag rejection
        let corp_it = InventoryItemRecord {
            item_id: 88_201,
            type_id: 87_710, // Colony Resource Management (200 power -> +5,000 workforce, 8 ice/hr, 16 startup)
            owner_id: corp_id,
            location_id: hub_id,
            flag: InventoryFlag::CorpSAG1,
            quantity: 2,
            singleton: false,
            version: 1,
            ..Default::default()
        };
        assert!(item_at_hub(snap, &c, &corp_it, hub_id, sys_id));
        let ship_it = InventoryItemRecord {
            item_id: 88_202,
            type_id: TYPE_SUPERIONIC_ICE,
            owner_id: cid,
            location_id: 88_100,
            flag: InventoryFlag::SpecializedAsteroidHold,
            quantity: 50,
            singleton: false,
            version: 1,
            ..Default::default()
        };
        assert!(item_at_hub(snap, &c, &ship_it, hub_id, sys_id));
        let bad_owner_it = InventoryItemRecord {
            owner_id: 777_777,
            ..ship_it.clone()
        };
        assert!(!item_at_hub(snap, &c, &bad_owner_it, hub_id, sys_id));
        snap.items.insert(88_201, corp_it);

        // Startup fuel boundary on 87_710 (fuel_per_hour = 8, fuel_startup = 16 -> required = 24):
        // At 23 units (< 24), onlining is rejected with STATUS_BAD_REQUEST; at 24 units, 16 startup fuel IS deducted -> 8 left!
        snap.sov_runtime.sov_hubs.insert(
            hub_id,
            PersistedSovHubData {
                hub_id,
                fuel: [(TYPE_SUPERIONIC_ICE, 23)].into_iter().collect(),
                ..Default::default()
            },
        );
        assert_eq!(
            process_configuration(snap, &sde, &c, hub_id, &[88_201], &[(87_710, true)], 1_000)
                .unwrap_err()
                .0,
            STATUS_BAD_REQUEST
        );
        snap.sov_runtime.sov_hubs.insert(
            hub_id,
            PersistedSovHubData {
                hub_id,
                fuel: [(TYPE_SUPERIONIC_ICE, 24)].into_iter().collect(),
                ..Default::default()
            },
        );
        process_configuration(snap, &sde, &c, hub_id, &[88_201], &[(87_710, true)], 1_000).unwrap();
        assert_eq!(snap.items[&88_201].quantity, 1); // partial consume_items left 1 unit
        assert_eq!(snap.sov_runtime.sov_hubs[&hub_id].fuel[&TYPE_SUPERIONIC_ICE], 8);

        // Offline, top up to 40, and re-online -> deducts 16 startup fuel, leaving 24!
        process_configuration(snap, &sde, &c, hub_id, &[], &[(87_710, false)], 1_001).unwrap();
        snap.sov_runtime
            .sov_hubs
            .get_mut(&hub_id)
            .unwrap()
            .fuel
            .insert(TYPE_SUPERIONIC_ICE, 40);
        process_configuration(snap, &sde, &c, hub_id, &[], &[(87_710, true)], 1_002).unwrap();
        assert_eq!(snap.sov_runtime.sov_hubs[&hub_id].fuel[&TYPE_SUPERIONIC_ICE], 24);

        // Hacked hub boundary (`now < until` keeps `low_power = true`, `now == until` clears `low_power`)
        snap.sov_runtime.sov_hubs.get_mut(&hub_id).unwrap().upgrades[0].low_power = true;
        snap.sov_runtime.hacked_hubs.insert(hub_id, 2_000);
        process_configuration(snap, &sde, &c, hub_id, &[], &[], 1_999).unwrap();
        assert!(snap.sov_runtime.sov_hubs[&hub_id].upgrades[0].low_power);
        process_configuration(snap, &sde, &c, hub_id, &[], &[], 2_000).unwrap();
        assert!(!snap.sov_runtime.sov_hubs[&hub_id].upgrades[0].low_power);
    });
}

#[test]
fn fuel_add_validation_starvation_simulation_and_uninstall_overload_recovery_notices() {
    let sde = shared_sde();
    let db = Arc::new(WorldRepository::with_demo_seed());
    let router = PublicGatewayRouter::new(
        Arc::new(QuasarDescriptorRegistry::empty_with_builtins()),
        Arc::clone(&db),
        Arc::clone(&sde),
    );
    register_handlers(&router, Arc::clone(&db), Arc::clone(&sde));

    let cid = 90_000_001_i64;
    let corp_id = 1_000_009_i64;
    let hub_id = 79_000_001_i64;
    let sys_id = 30_000_142_u32;

    db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&cid).unwrap();
        ch.corporation_id = corp_id;
        ch.solar_system_id = sys_id;
        if let Some(ch2) = snap.characters.get_mut(&90_000_002) {
            ch2.corporation_id = corp_id;
            ch2.solar_system_id = sys_id;
        }
        let mut corp_rec = { let mut __corp = CorporationRecord::default();
            __corp.corporation_id = corp_id;
            __corp.ceo_id = cid;
            __corp };
        corp_rec.members.extend([cid, 90_000_002]);
        snap.corporations.insert(corp_id, corp_rec);
        snap.structures.insert(
            hub_id,
            UpwellStructureRecord {
                structure_id: hub_id,
                type_id: TYPE_SOVEREIGNTY_HUB,
                owner_corp_id: corp_id,
                solar_system_id: sys_id,
                state: "anchor_vulnerable".into(),
                ..Default::default()
            },
        );
        // Install 87_710 (8 ice/hr, +5,000 workforce) + 82_499 (200 power, 1,800 workforce, 0 fuel)
        // with only 5 ice (< 8) and last_updated_unix = 1_000 so t = 4_600 starves 87_710 AND cascades 82_499 into overload!
        snap.sov_runtime.sov_hubs.insert(
            hub_id,
            PersistedSovHubData {
                hub_id,
                upgrades: vec![
                    PersistedHubUpgrade {
                        type_id: 87_710,
                        online: true,
                        low_power: false,
                    },
                    PersistedHubUpgrade {
                        type_id: 82_499,
                        online: true,
                        low_power: false,
                    },
                ],
                fuel: [(TYPE_SUPERIONIC_ICE, 5)].into_iter().collect(),
                last_updated_unix: 1_000,
                ..Default::default()
            },
        );
        snap.items.insert(
            91_001,
            InventoryItemRecord {
                item_id: 91_001,
                type_id: TYPE_SUPERIONIC_ICE,
                owner_id: cid,
                location_id: hub_id,
                flag: InventoryFlag::Hangar,
                quantity: 50,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            91_002,
            InventoryItemRecord {
                item_id: 91_002,
                type_id: 34, // Tritanium -> not hub fuel!
                owner_id: cid,
                location_id: hub_id,
                flag: InventoryFlag::Hangar,
                quantity: 50,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    let handlers = router.handlers.read().unwrap();
    let any = GoogleAny::default();
    let dir_4600 = AuthoritativeContext::for_session_at(
        Some(1),
        Some(cid as u32),
        Some(corp_id as u32),
        corp_roles::ROLE_DIRECTOR,
        "tranquility",
        vec![1],
        4_600,
    );

    // 1. GetResourcesRequest at t = 4_600 triggers `run_simulation` -> starves 87_710, overloads 82_499,
    // and emits `HubUpgradesSimulatedNotice` + `ResourcesSimulatedNotice`!
    router.pending_notices.write().unwrap().clear();
    handlers["eve_public.sovereignty.hub.api.GetResourcesRequest"](&dir_4600, &any, &id_msg(1, hub_id)).unwrap();
    {
        let notices = router.pending_notices.read().unwrap();
        assert!(notices.iter().any(|(_, a)| a.type_url.ends_with("HubUpgradesSimulatedNotice")));
        assert!(notices.iter().any(|(_, a)| a.type_url.ends_with("ResourcesSimulatedNotice")));
    }

    // 2. Fuel AddRequest validation errors: amount <= 0, non-fuel item (34), insufficient stack (100 > 50), unauthorized role
    let mk_fuel = |item: i64, amt: i64| {
        let mut m = id_msg(1, item);
        m.fields.insert(2, DynamicValue::Int64(amt));
        m.fields.insert(3, id_msg(1, hub_id).fields.remove(&1).unwrap());
        m
    };
    assert_eq!(
        handlers["eve_public.sovereignty.hub.fuel.api.AddRequest"](&dir_4600, &any, &mk_fuel(91_001, 0))
            .unwrap_err()
            .0,
        STATUS_BAD_REQUEST
    );
    assert_eq!(
        handlers["eve_public.sovereignty.hub.fuel.api.AddRequest"](&dir_4600, &any, &mk_fuel(91_002, 10))
            .unwrap_err()
            .0,
        STATUS_BAD_REQUEST
    );
    assert_eq!(
        handlers["eve_public.sovereignty.hub.fuel.api.AddRequest"](&dir_4600, &any, &mk_fuel(91_001, 100))
            .unwrap_err()
            .0,
        STATUS_BAD_REQUEST
    );
    let line_member = AuthoritativeContext::for_session_at(
        Some(2),
        Some(90_000_002),
        Some(corp_id as u32),
        0,
        "tranquility",
        vec![1],
        4_600,
    );
    assert_eq!(
        handlers["eve_public.sovereignty.hub.fuel.api.AddRequest"](&line_member, &any, &mk_fuel(91_001, 10))
            .unwrap_err()
            .0,
        STATUS_FORBIDDEN
    );

    // Valid refueling restores starved 87_710 (`low_power = false`) and emits `HubUpgradesSimulatedNotice`!
    router.pending_notices.write().unwrap().clear();
    handlers["eve_public.sovereignty.hub.fuel.api.AddRequest"](&dir_4600, &any, &mk_fuel(91_001, 20)).unwrap();
    assert!(router
        .pending_notices
        .read()
        .unwrap()
        .iter()
        .any(|(_, a)| a.type_url.ends_with("HubUpgradesSimulatedNotice")));

    // 3. UninstallRequest overload recovery: install two power-hungry upgrades so slot 1 is overloaded (`low_power = true`),
    // then uninstall slot 0 -> slot 1 recovers (`prev_states != next_states`) and emits `HubUpgradesSimulatedNotice`!
    db.with_mut_snapshot(|snap| {
        let h = snap.sov_runtime.sov_hubs.get_mut(&hub_id).unwrap();
        h.upgrades = vec![
            PersistedHubUpgrade {
                type_id: 87_954, // 1,300 power
                online: true,
                low_power: false,
            },
            PersistedHubUpgrade {
                type_id: 82_499, // 200 power, currently forced low_power
                online: true,
                low_power: true,
            },
        ];
    });
    // Also exercise `emit_hub_diffs` workforce `StateChangedNotice` directly when `state` bytes differ
    let pending: PendingNotices = Arc::new(RwLock::new(Vec::new()));
    let b_map: BTreeMap<i64, HubView> = [(
        hub_id,
        HubView {
            sys: sys_id,
            resources: HubResources::default(),
            state: pb::message("eve_public.sovereignty.hub.workforce.State")
                .set("inactive", true)
                .must_encode(),
        },
    )]
    .into_iter()
    .collect();
    let a_map: BTreeMap<i64, HubView> = [(
        hub_id,
        HubView {
            sys: sys_id,
            resources: HubResources::default(),
            state: pb::message("eve_public.sovereignty.hub.workforce.State")
                .set_with("transit", |t| t)
                .must_encode(),
        },
    )]
    .into_iter()
    .collect();
    emit_hub_diffs(&pending, &b_map, &a_map);
    assert!(pending.read().unwrap().iter().any(|(_, a)| a.type_url.ends_with("StateChangedNotice")));

    // Uninstall 87_954 -> 82_499 transitions from low_power (3) to online (2) when workforce allows!
    db.with_mut_snapshot(|snap| {
        let h = snap.sov_runtime.sov_hubs.get_mut(&hub_id).unwrap();
        h.upgrades.insert(
            0,
            PersistedHubUpgrade {
                type_id: 87_710, // +5,000 workforce so 82_499 has enough workforce once 87_954 is removed
                online: true,
                low_power: false,
            },
        );
    });
    router.pending_notices.write().unwrap().clear();
    let mut uninst_id = id_msg(1, hub_id);
    uninst_id.fields.insert(2, DynamicValue::Int64(87_954));
    let mut uninst_req = DynamicQuasarMessage::new("Uninst");
    uninst_req.fields.insert(1, DynamicValue::Message(Box::new(uninst_id)));
    handlers["eve_public.sovereignty.hub.upgrade.api.UninstallRequest"](&dir_4600, &any, &uninst_req).unwrap();
    {
        let notices = router.pending_notices.read().unwrap();
        assert!(notices.iter().any(|(_, a)| a.type_url.ends_with("UninstalledNotice")));
        assert!(notices.iter().any(|(_, a)| a.type_url.ends_with("HubUpgradesSimulatedNotice")));
    }
}
