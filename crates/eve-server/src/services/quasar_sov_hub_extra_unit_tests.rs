//! Extra branch-coverage and mutation-killing unit tests for `quasar_sov_hub.rs`.

#![forbid(unsafe_code)]

use crate::quasar_gateway::{
    PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_FORBIDDEN, STATUS_UNAUTHORIZED,
};
use crate::services::quasar_sov_hub::*;
use crate::services::sov_services::{
    TYPE_MAGMATIC_GAS, TYPE_ORBITAL_SKYHOOK, TYPE_SOVEREIGNTY_HUB, TYPE_SUPERIONIC_ICE,
};
use eve_db::{
    corp_roles, CorporationRecord, InventoryFlag, InventoryItemRecord, PersistedHubUpgrade,
    PersistedMercenaryDen, PersistedSovHubData, UpwellStructureRecord, WorldRepository,
};
use eve_proto::quasar::{
    AuthoritativeContext, DynamicQuasarMessage, DynamicValue, GoogleAny, QuasarDescriptorRegistry,
};
use eve_sde::SdeRegistry;
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
fn all_upgrade_groups_star_classes_planet_yields_and_fuel_clock_edges() {
    let sde = shared_sde();

    for (tid, exp_p_req, exp_w_req, exp_fuel, exp_rate, exp_p_prod, exp_w_prod) in [
        (81_623, 1_750, 17_500, TYPE_SUPERIONIC_ICE, 90, 0, 0),
        (81_621, 500, 18_100, TYPE_SUPERIONIC_ICE, 25, 0, 0),
        (81_619, 250, 4_500, TYPE_MAGMATIC_GAS, 205, 0, 0),
        (81_615, 250, 1_500, TYPE_MAGMATIC_GAS, 205, 0, 0),
        (82_499, 200, 1_800, 0, 0, 0, 0),
        (82_500, 500, 3_800, 0, 0, 0, 0),
        (82_492, 700, 5_400, 0, 0, 0, 0),
        (82_496, 400, 2_700, 0, 0, 0, 0),
        (82_497, 900, 5_400, 0, 0, 0, 0),
        (82_498, 1_300, 7_300, 0, 0, 0, 0),
        (82_579, 450, 6_400, 0, 0, 0, 0),
        (82_580, 1_220, 12_700, 0, 0, 0, 0),
        (87_702, 1_800, 18_100, 0, 0, 0, 0),
        (87_948, 400, 4_000, 0, 0, 0, 0),
        (87_953, 850, 8_000, 0, 0, 0, 0),
        (87_954, 1_300, 12_000, 0, 0, 0, 0),
        (87_815, 200, 2_700, TYPE_MAGMATIC_GAS, 7, 0, 0),
        (87_703, 0, 2_500, TYPE_SUPERIONIC_ICE, 8, 200, 0),
        (88_221, 0, 10_000, TYPE_SUPERIONIC_ICE, 25, 500, 0),
        (88_227, 0, 25_000, TYPE_SUPERIONIC_ICE, 42, 1_000, 0),
        (87_710, 200, 0, TYPE_SUPERIONIC_ICE, 8, 0, 5_000),
        (88_228, 500, 0, TYPE_SUPERIONIC_ICE, 25, 0, 10_000),
        (88_229, 800, 0, TYPE_SUPERIONIC_ICE, 42, 0, 15_000),
    ] {
        let d = upgrade_definition(&sde, tid).unwrap_or_else(|| panic!("upgrade {tid}"));
        assert_eq!(d.power_required, exp_p_req);
        assert_eq!(d.workforce_required, exp_w_req);
        assert_eq!(d.fuel_type, exp_fuel);
        assert_eq!(d.fuel_per_hour, exp_rate);
        assert_eq!(d.power_produced, exp_p_prod);
        assert_eq!(d.workforce_produced, exp_w_prod);
    }
    assert!(upgrade_definition(&sde, 34).is_none());

    for &(ptid, _name, p, w) in &PLANET_RESOURCE_TABLE {
        assert_eq!(planet_yield(ptid), (p, w));
    }
    assert_eq!(star_power(&sde, 999_999), 300);

    // simulate_fuel edge cases: last_updated_unix == 0, now <= last_updated, sub-hour elapsed
    let mut hub = PersistedSovHubData {
        hub_id: 1,
        upgrades: vec![PersistedHubUpgrade {
            type_id: 81_623,
            online: true,
            low_power: false,
        }],
        fuel: [(TYPE_SUPERIONIC_ICE, 90)].into_iter().collect(),
        last_updated_unix: 0,
        ..Default::default()
    };
    assert!(!simulate_fuel(&sde, &mut hub, 5_000));
    assert_eq!(hub.last_updated_unix, 5_000);
    assert!(!simulate_fuel(&sde, &mut hub, 5_000));
    assert!(!simulate_fuel(&sde, &mut hub, 8_599)); // 3599s < 1h
    assert_eq!(hub.fuel.get(&TYPE_SUPERIONIC_ICE), Some(&90));

    // Exact 1h burns all 90 units and removes 0-balance entry while keeping upgrade online
    assert!(!simulate_fuel(&sde, &mut hub, 8_600));
    assert!(!hub.fuel.contains_key(&TYPE_SUPERIONIC_ICE));
    assert!(!hub.upgrades[0].low_power);
}

#[test]
fn skyhook_harvest_mercden_siphon_and_priority_slot_power_overload() {
    let sde = shared_sde();
    let db = WorldRepository::with_demo_seed();
    let sys_id = 30_000_142_u32;
    let hub_id = 78_000_001_i64;
    let sh_id = 78_000_002_i64;

    // Find a planet in sys_id that yields workforce or test with any planet
    let planet_id = sde
        .celestials_in_system(sys_id)
        .iter()
        .find(|c| c.group_id == 7 && planet_yield(c.type_id).1 > 0)
        .map(|c| c.celestial_id)
        .unwrap_or_else(|| {
            sde.celestials_in_system(sys_id)
                .iter()
                .find(|c| c.group_id == 7)
                .unwrap()
                .celestial_id
        });

    db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            hub_id,
            UpwellStructureRecord {
                structure_id: hub_id,
                type_id: TYPE_SOVEREIGNTY_HUB,
                owner_corp_id: 1_000_009,
                solar_system_id: sys_id,
                state: "anchor_vulnerable".into(),
                ..Default::default()
            },
        );
        snap.structures.insert(
            sh_id,
            UpwellStructureRecord {
                structure_id: sh_id,
                type_id: TYPE_ORBITAL_SKYHOOK,
                owner_corp_id: 1_000_009,
                solar_system_id: sys_id,
                state: "anchor_vulnerable".into(),
                ..Default::default()
            },
        );
        snap.sov_runtime.skyhook_planets.insert(sh_id, planet_id);
        snap.sov_runtime.skyhook_active.insert(sh_id, true);
        assert!(is_planet_harvested_by_active_skyhook(snap, sys_id, planet_id));

        // Inactive or destroyed skyhook does not harvest planet
        snap.sov_runtime.skyhook_active.insert(sh_id, false);
        assert!(!is_planet_harvested_by_active_skyhook(snap, sys_id, planet_id));
        snap.sov_runtime.skyhook_active.insert(sh_id, true);
        snap.structures.get_mut(&sh_id).unwrap().state = "destroyed".into();
        assert!(!is_planet_harvested_by_active_skyhook(snap, sys_id, planet_id));
        snap.structures.get_mut(&sh_id).unwrap().state = "anchor_vulnerable".into();

        // Seed Mercenary Den on planet_id with high anarchy_level to exercise siphon calculation
        snap.sov_runtime.mercenary_dens.insert(
            78_000_099,
            PersistedMercenaryDen {
                den_id: 78_000_099,
                owner_char_id: 90_000_001,
                solar_system_id: sys_id,
                planet_id,
                skyhook_id: sh_id,
                anarchy_level: 5,
                enabled: true,
                ..Default::default()
            },
        );

        // Priority overload: slot 0 fits in star power (82_499 needs 200p/1800w), slot 1 exceeds workforce/power
        snap.sov_runtime.sov_hubs.insert(
            hub_id,
            PersistedSovHubData {
                hub_id,
                upgrades: vec![
                    PersistedHubUpgrade {
                        type_id: 87_710, // needs 200 power, 0 workforce, produces +5000 workforce
                        online: true,
                        low_power: false,
                    },
                    PersistedHubUpgrade {
                        type_id: 82_499, // needs 200 power, 1800 workforce (covered by 87_710!)
                        online: true,
                        low_power: false,
                    },
                    PersistedHubUpgrade {
                        type_id: 87_702, // needs 1800 power, 18100 workforce (exceeds pool -> low_power!)
                        online: true,
                        low_power: false,
                    },
                ],
                ..Default::default()
            },
        );
        assert!(enforce_hub_power_overload(snap, &sde, hub_id));
        let u = &snap.sov_runtime.sov_hubs[&hub_id].upgrades;
        assert!(!u[0].low_power);
        assert!(!u[1].low_power);
        assert!(u[2].low_power);

        let res = compute_hub_resources(snap, &sde, hub_id).unwrap();
        assert_eq!(res.power_allocated, 400);
        assert_eq!(res.workforce_allocated, 1_800);
    });
}

#[test]
fn process_configuration_reordering_fuel_restore_hacked_hub_and_uninstall_recovery() {
    let sde = shared_sde();
    let db = Arc::new(WorldRepository::with_demo_seed());
    let descriptors = Arc::new(QuasarDescriptorRegistry::empty_with_builtins());
    let router = PublicGatewayRouter::new(descriptors, Arc::clone(&db), Arc::clone(&sde));
    register_handlers(&router, Arc::clone(&db), Arc::clone(&sde));

    let cid = 90_000_001_i64;
    let corp_a = 98_000_301_i64;
    let sister_corp = 98_000_302_i64;
    let enemy_corp = 98_000_303_i64;
    let ally_id = 99_000_301_i64;
    let hub_id = 79_000_001_i64;
    let sys_id = 30_000_142_u32;
    let ship_id = 79_000_010_i64;

    db.with_mut_snapshot(|snap| {
        let base_ch = snap.characters[&cid].clone();
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.corporation_id = corp_a;
            ch.alliance_id = Some(ally_id);
            ch.solar_system_id = sys_id;
            ch.active_ship_id = ship_id;
        }
        let mut grunt_ch = base_ch.clone();
        grunt_ch.character_id = 90_000_003;
        grunt_ch.corporation_id = corp_a;
        grunt_ch.alliance_id = Some(ally_id);
        grunt_ch.solar_system_id = sys_id;
        snap.characters.insert(90_000_003, grunt_ch);

        let mut sis_ch = base_ch.clone();
        sis_ch.character_id = 90_000_004;
        sis_ch.corporation_id = sister_corp;
        sis_ch.alliance_id = Some(ally_id);
        sis_ch.solar_system_id = sys_id;
        snap.characters.insert(90_000_004, sis_ch);

        if let Some(ech) = snap.characters.get_mut(&90_000_002) {
            ech.corporation_id = enemy_corp;
            ech.alliance_id = None;
        }

        let mut ca = { let mut __corp = CorporationRecord::default();
            __corp.corporation_id = corp_a;
            __corp.alliance_id = Some(ally_id);
            __corp.ceo_id = 99_999_001;
            __corp };
        ca.members.extend([cid, 90_000_003]);
        ca.member_roles.insert(cid, corp_roles::ROLE_DIRECTOR);
        snap.corporations.insert(corp_a, ca);

        snap.corporations.insert(
            sister_corp,
            { let mut __corp = CorporationRecord::default();
                __corp.corporation_id = sister_corp;
                __corp.alliance_id = Some(ally_id);
                __corp.ceo_id = 90_000_004;
                __corp },
        );
        snap.corporations.insert(
            enemy_corp,
            { let mut __corp = CorporationRecord::default();
                __corp.corporation_id = enemy_corp;
                __corp.alliance_id = None;
                __corp.ceo_id = 90_000_002;
                __corp },
        );
        snap.structures.insert(
            hub_id,
            UpwellStructureRecord {
                structure_id: hub_id,
                type_id: TYPE_SOVEREIGNTY_HUB,
                owner_corp_id: corp_a,
                solar_system_id: sys_id,
                name: "Test Sov Hub".into(),
                state: "anchor_vulnerable".into(),
                ..Default::default()
            },
        );
        // Seed upgrade items in active ship cargo (87_710 Workforce Generator & 81_615 Service Upgrade) + fuel
        for (iid, tid, qty) in [
            (89_001_i64, 87_710_u32, 1),
            (89_002_i64, 88_228_u32, 1), // Same ResourceManagement exclusive_group as 87_710!
            (89_003_i64, 82_499_u32, 1),
            (89_004_i64, TYPE_SUPERIONIC_ICE, 500),
            (89_005_i64, 34_u32, 100), // Not fuel / not upgrade
        ] {
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: tid,
                    owner_id: cid,
                    location_id: ship_id,
                    flag: InventoryFlag::Cargo,
                    quantity: qty,
                    singleton: false,
                    version: 1,
                    ..Default::default()
                },
            );
        }
    });

    let handlers = router.handlers.read().unwrap();
    let dummy_any = GoogleAny::default();
    let unauth_ctx = AuthoritativeContext::for_session(None, None, None, 0, "tranquility", vec![]);
    let no_role_ctx = AuthoritativeContext::for_session(
        Some(1),
        Some(90_000_003),
        Some(corp_a as u32),
        0,
        "tranquility",
        vec![1],
    );
    let mgr_ctx = AuthoritativeContext::for_session(
        Some(1),
        Some(cid as u32),
        Some(corp_a as u32),
        corp_roles::ROLE_DIRECTOR,
        "tranquility",
        vec![1],
    );
    let sister_mgr_ctx = AuthoritativeContext::for_session(
        Some(2),
        Some(90_000_004),
        Some(sister_corp as u32),
        corp_roles::ROLE_DIRECTOR,
        "tranquility",
        vec![1],
    );
    let enemy_ctx = AuthoritativeContext::for_session(
        Some(3),
        Some(90_000_002),
        Some(enemy_corp as u32),
        corp_roles::ROLE_DIRECTOR,
        "tranquility",
        vec![1],
    );

    // Auth & ownership guards
    assert_eq!(
        handlers["eve_public.sovereignty.hub.api.GetResourcesRequest"](
            &unauth_ctx,
            &dummy_any,
            &id_msg(1, hub_id),
        )
        .unwrap_err()
        .0,
        STATUS_UNAUTHORIZED
    );
    assert_eq!(
        handlers["eve_public.sovereignty.hub.api.GetResourcesRequest"](
            &enemy_ctx,
            &dummy_any,
            &id_msg(1, hub_id),
        )
        .unwrap_err()
        .0,
        STATUS_FORBIDDEN
    );
    // Sister corp in same alliance CAN read hub resources
    assert!(handlers["eve_public.sovereignty.hub.api.GetResourcesRequest"](
        &sister_mgr_ctx,
        &dummy_any,
        &id_msg(1, hub_id),
    )
    .is_ok());

    // ProcessConfigurationRequest without HUB_MANAGER_ROLES -> STATUS_FORBIDDEN
    assert_eq!(
        handlers["eve_public.sovereignty.hub.upgrade.api.ProcessConfigurationRequest"](
            &no_role_ctx,
            &dummy_any,
            &id_msg(3, hub_id),
        )
        .unwrap_err()
        .0,
        STATUS_FORBIDDEN
    );

    // Trying to online 87_710 before adding fuel -> STATUS_BAD_REQUEST (Insufficient fuel)
    let mut new1 = DynamicQuasarMessage::new("New");
    new1.fields.insert(1, DynamicValue::Int64(89_001));
    let mut new3 = DynamicQuasarMessage::new("New");
    new3.fields.insert(1, DynamicValue::Int64(89_003));
    let mut cfg_online = id_msg(1, 87_710);
    cfg_online.fields.insert(2, DynamicValue::Bool(true));
    let mut proc_no_fuel = id_msg(3, hub_id);
    proc_no_fuel
        .fields
        .insert(1, DynamicValue::Repeated(vec![DynamicValue::Message(Box::new(new1.clone()))]));
    proc_no_fuel
        .fields
        .insert(2, DynamicValue::Repeated(vec![DynamicValue::Message(Box::new(cfg_online))]));
    assert_eq!(
        handlers["eve_public.sovereignty.hub.upgrade.api.ProcessConfigurationRequest"](
            &mgr_ctx,
            &dummy_any,
            &proc_no_fuel,
        )
        .unwrap_err()
        .0,
        STATUS_BAD_REQUEST
    );

    // Install 87_710 and 82_499 offline first
    let mut proc_install = id_msg(3, hub_id);
    proc_install.fields.insert(
        1,
        DynamicValue::Repeated(vec![
            DynamicValue::Message(Box::new(new1)),
            DynamicValue::Message(Box::new(new3)),
        ]),
    );
    handlers["eve_public.sovereignty.hub.upgrade.api.ProcessConfigurationRequest"](
        &mgr_ctx,
        &dummy_any,
        &proc_install,
    )
    .expect("install 87_710 and 82_499 from ship cargo");

    // Installing second ResourceManagement upgrade (88_228) fails exclusive_group check
    let mut new_dup = DynamicQuasarMessage::new("New");
    new_dup.fields.insert(1, DynamicValue::Int64(89_002));
    let mut proc_dup = id_msg(3, hub_id);
    proc_dup
        .fields
        .insert(1, DynamicValue::Repeated(vec![DynamicValue::Message(Box::new(new_dup))]));
    assert_eq!(
        handlers["eve_public.sovereignty.hub.upgrade.api.ProcessConfigurationRequest"](
            &mgr_ctx, &dummy_any, &proc_dup
        )
        .unwrap_err()
        .0,
        STATUS_BAD_REQUEST
    );

    // Fuel AddRequest validation: non-positive amount, non-fuel item (89_005 = Tritanium)
    let mut bad_fuel = id_msg(1, 89_005);
    bad_fuel.fields.insert(2, DynamicValue::Int64(10));
    let mut hub_inner = DynamicQuasarMessage::new("Identifier");
    hub_inner.fields.insert(1, DynamicValue::Int64(hub_id));
    bad_fuel
        .fields
        .insert(3, DynamicValue::Message(Box::new(hub_inner.clone())));
    assert_eq!(
        handlers["eve_public.sovereignty.hub.fuel.api.AddRequest"](&mgr_ctx, &dummy_any, &bad_fuel)
            .unwrap_err()
            .0,
        STATUS_BAD_REQUEST
    );

    // Add valid Superionic Ice fuel from ship cargo, then reorder & online [82_499, 87_710]
    let mut good_fuel = id_msg(1, 89_004);
    good_fuel.fields.insert(2, DynamicValue::Int64(100));
    good_fuel
        .fields
        .insert(3, DynamicValue::Message(Box::new(hub_inner)));
    handlers["eve_public.sovereignty.hub.fuel.api.AddRequest"](&mgr_ctx, &dummy_any, &good_fuel)
        .expect("add fuel from ship cargo");

    let mut c1 = id_msg(1, 87_710);
    c1.fields.insert(2, DynamicValue::Bool(true));
    let mut c2 = id_msg(1, 82_499);
    c2.fields.insert(2, DynamicValue::Bool(true));
    let mut proc_reorder = id_msg(3, hub_id);
    proc_reorder.fields.insert(
        2,
        DynamicValue::Repeated(vec![
            DynamicValue::Message(Box::new(c1)),
            DynamicValue::Message(Box::new(c2)),
        ]),
    );
    handlers["eve_public.sovereignty.hub.upgrade.api.ProcessConfigurationRequest"](
        &mgr_ctx,
        &dummy_any,
        &proc_reorder,
    )
    .expect("online and reorder upgrades");

    // Sister corp in same alliance CANNOT uninstall upgrades (must be exact owning corporation)
    let mut upg_id_inner = id_msg(1, hub_id);
    upg_id_inner.fields.insert(2, DynamicValue::Int64(82_499));
    let mut uninst_req = DynamicQuasarMessage::new("Uninst");
    uninst_req
        .fields
        .insert(1, DynamicValue::Message(Box::new(upg_id_inner)));
    assert_eq!(
        handlers["eve_public.sovereignty.hub.upgrade.api.UninstallRequest"](
            &sister_mgr_ctx,
            &dummy_any,
            &uninst_req,
        )
        .unwrap_err()
        .0,
        STATUS_FORBIDDEN
    );
    handlers["eve_public.sovereignty.hub.upgrade.api.UninstallRequest"](
        &mgr_ctx,
        &dummy_any,
        &uninst_req,
    )
    .expect("owning corp station manager uninstalls upgrade");
}

#[test]
fn mutant_kill_upgrade_definition_item_at_hub_boundaries_and_consume_items() {
    let sde = shared_sde();
    let db = WorldRepository::with_demo_seed();

    // 1. upgrade_definition full field verification & published / group checks
    let svc_def = upgrade_definition(&sde, 81_623).expect("81_623 definition");
    assert_eq!(svc_def.type_id, 81_623);
    assert_eq!(svc_def.fuel_startup, 180);
    assert!(!svc_def.exclusive_group.is_empty());

    let seg_def = upgrade_definition(&sde, 87_815).expect("87_815 definition");
    assert_eq!(seg_def.exclusive_group, "SystemEffectGenerator");
    assert_eq!(seg_def.fuel_startup, 14);

    let res_def = upgrade_definition(&sde, 87_710).expect("87_710 definition");
    assert_eq!(res_def.exclusive_group, "ResourceManagement");
    assert_eq!(res_def.fuel_startup, 16);

    assert!(!all_upgrade_definitions(&sde).is_empty());

    let mut unpub_sde = (*sde).clone();
    unpub_sde.types.get_mut(&81_623).unwrap().published = false;
    assert!(upgrade_definition(&unpub_sde, 81_623).is_none());

    // 2. item_at_hub: owner, qty >= 1, unlocked, non-fitted, hub/active-ship-system boundaries
    let cid = 90_000_001_i64;
    let other_cid = 90_000_099_i64;
    let corp_id = 98_000_401_i64;
    let hub_id = 79_500_001_i64;
    let ship_id = 79_500_010_i64;
    let other_ship_id = 79_500_011_i64;
    let hub_sys = 30_000_142_u32;
    let other_sys = 30_000_144_u32;

    let dir_cid = cid + 1;
    db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.corporation_id = corp_id;
            ch.solar_system_id = hub_sys;
            ch.active_ship_id = ship_id;
        }
        let mut dir_ch = snap.characters[&cid].clone();
        dir_ch.character_id = dir_cid;
        dir_ch.user_id += 1;
        dir_ch.name = "ItemAtHubDir".into();
        snap.characters.insert(dir_cid, dir_ch);
        let mut corp = CorporationRecord::default();
        corp.corporation_id = corp_id;
        corp.name = "ItemAtHubCorp".into();
        corp.ticker = "IAH".into();
        corp.ceo_id = 99_999_401;
        corp.members.insert(cid);
        corp.members.insert(dir_cid);
        corp.member_roles.insert(dir_cid, corp_roles::ROLE_DIRECTOR);
        corp.ensure_wallet_divisions();
        snap.corporations.insert(corp_id, corp);

        // Insert active ship item so ancestor lock can be toggled on it cleanly
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: cid,
                location_id: hub_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            91_001,
            InventoryItemRecord {
                item_id: 91_001,
                type_id: TYPE_SUPERIONIC_ICE,
                owner_id: cid,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 10,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            91_002,
            InventoryItemRecord {
                item_id: 91_002,
                type_id: 82_499,
                owner_id: cid,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            91_003,
            InventoryItemRecord {
                item_id: 91_003,
                type_id: TYPE_SUPERIONIC_ICE,
                owner_id: cid,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 0,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            91_004,
            InventoryItemRecord {
                item_id: 91_004,
                type_id: TYPE_SUPERIONIC_ICE,
                owner_id: other_cid,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 5,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            91_005,
            InventoryItemRecord {
                item_id: 91_005,
                type_id: TYPE_SUPERIONIC_ICE,
                owner_id: cid,
                location_id: hub_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            91_006,
            InventoryItemRecord {
                item_id: 91_006,
                type_id: TYPE_SUPERIONIC_ICE,
                owner_id: corp_id,
                location_id: hub_id,
                flag: InventoryFlag::CorpSAG1,
                quantity: 5,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );

        let caller_plain = Caller {
            cid,
            corp: corp_id,
            alliance: None,
            roles: 0,
        };
        let caller_dir = Caller {
            cid: dir_cid,
            corp: corp_id,
            alliance: None,
            roles: corp_roles::ROLE_DIRECTOR,
        };

        // Valid cargo item in active ship in hub_sys -> true
        assert!(item_at_hub(snap, &caller_plain, &snap.items[&91_001], hub_id, hub_sys));
        // Exact min quantity == 1 in hub Hangar -> true
        assert!(item_at_hub(snap, &caller_plain, &snap.items[&91_005], hub_id, hub_sys));
        // Fitted module (HiSlot0) -> false (kills delete !is_fitted_slot_flag & ||)
        assert!(!item_at_hub(snap, &caller_plain, &snap.items[&91_002], hub_id, hub_sys));
        // Quantity == 0 -> false (kills quantity < 1 & ||)
        assert!(!item_at_hub(snap, &caller_plain, &snap.items[&91_003], hub_id, hub_sys));
        // Wrong owner (valid_owner == false while qty > 0, unlocked, Cargo, right ship) -> false
        assert!(!item_at_hub(snap, &caller_plain, &snap.items[&91_004], hub_id, hub_sys));

        // CorpSAG1 requires corp hangar take role
        assert!(!item_at_hub(snap, &caller_plain, &snap.items[&91_006], hub_id, hub_sys));
        assert!(item_at_hub(snap, &caller_dir, &snap.items[&91_006], hub_id, hub_sys));

        // Locked ancestor ship blocks cargo item -> false (kills delete !is_item_or_ancestor_locked & ||)
        snap.items.get_mut(&ship_id).unwrap().flag = InventoryFlag::Locked;
        assert!(!item_at_hub(snap, &caller_plain, &snap.items[&91_001], hub_id, hub_sys));
        snap.items.get_mut(&ship_id).unwrap().flag = InventoryFlag::Hangar;

        // Caller outside hub_sys still accesses hub Hangar, but NOT active ship cargo
        snap.characters.get_mut(&cid).unwrap().solar_system_id = other_sys;
        assert!(!item_at_hub(snap, &caller_plain, &snap.items[&91_001], hub_id, hub_sys));
        assert!(item_at_hub(snap, &caller_plain, &snap.items[&91_005], hub_id, hub_sys));
        snap.characters.get_mut(&cid).unwrap().solar_system_id = hub_sys;
        snap.characters.get_mut(&cid).unwrap().active_ship_id = other_ship_id;
        assert!(!item_at_hub(snap, &caller_plain, &snap.items[&91_001], hub_id, hub_sys));

        // 3. consume_items decrements partial stacks and removes exhausted items (even when singleton == true)
        snap.items.insert(
            91_007,
            InventoryItemRecord {
                item_id: 91_007,
                type_id: TYPE_SUPERIONIC_ICE,
                owner_id: cid,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        consume_items(snap, 91_001, 4);
        assert_eq!(snap.items[&91_001].quantity, 6);
        consume_items(snap, 91_007, 1);
        assert!(!snap.items.contains_key(&91_007));
    });
}
