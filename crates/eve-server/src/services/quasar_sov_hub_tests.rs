//! Unit tests for `quasar_sov_hub` (Equinox Sovereignty Hub resources, fuel simulation, upgrade configuration).

#![forbid(unsafe_code)]

use super::*;
use crate::quasar_gateway::PublicGatewayRouter;
use eve_db::{InventoryFlag, InventoryItemRecord, UpwellStructureRecord};
use eve_proto::quasar::{AuthoritativeContext, DynamicQuasarMessage, DynamicValue, QuasarDescriptorRegistry};
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
fn upgrade_definitions_celestial_yields_and_fuel_simulation() {
    let sde = shared_sde();

    assert_eq!(split_tier("Minor Threat Detection Array 2"), ("Minor Threat Detection Array", 2));
    assert_eq!(split_tier("Cynosural Navigation"), ("Cynosural Navigation", 1));

    assert!(is_hub_fuel_type(TYPE_MAGMATIC_GAS));
    assert!(is_hub_fuel_type(TYPE_SUPERIONIC_ICE));
    assert!(!is_hub_fuel_type(34));

    assert_eq!(planet_yield(11), (0, 1_500));
    assert_eq!(planet_yield(2063), (200, 0));
    assert_eq!(planet_yield(999_999), (0, 0));

    let sys_y = system_yield(&sde, 30_000_142);
    assert!(!sys_y.stars.is_empty());

    let defs = all_upgrade_definitions(&sde);
    assert!(!defs.is_empty());
    let d81623 = upgrade_definition(&sde, 81_623).expect("81623 upgrade definition");
    assert_eq!(d81623.fuel_type, TYPE_SUPERIONIC_ICE);
    assert_eq!(d81623.fuel_per_hour, 90);
    assert!(!encode_definition(&d81623).is_empty());

    // Fuel simulation: sufficient fuel vs starvation -> low_power
    let mut hub = PersistedSovHubData {
        hub_id: 900_001,
        upgrades: vec![PersistedHubUpgrade {
            type_id: 81_623,
            online: true,
            low_power: false,
        }],
        fuel: [(TYPE_SUPERIONIC_ICE, 100)].into_iter().collect(),
        last_updated_unix: 1_000,
        ..Default::default()
    };
    // 1 hour elapsed burns 90 units, leaves 10 units, stays online
    assert!(!simulate_fuel(&sde, &mut hub, 4_600));
    assert_eq!(hub.fuel.get(&TYPE_SUPERIONIC_ICE), Some(&10));
    assert!(!hub.upgrades[0].low_power);

    // Next hour starves upgrade -> low_power = true
    assert!(simulate_fuel(&sde, &mut hub, 8_200));
    assert!(hub.upgrades[0].low_power);
}

#[test]
fn sov_hub_resources_fuel_and_upgrade_rpc_handlers() {
    let sde = shared_sde();
    let db = Arc::new(WorldRepository::with_demo_seed());
    let descriptors = Arc::new(QuasarDescriptorRegistry::empty_with_builtins());
    let router = PublicGatewayRouter::new(descriptors, Arc::clone(&db), Arc::clone(&sde));
    register_handlers(&router, Arc::clone(&db), Arc::clone(&sde));

    let cid = 90_000_001_i64;
    let corp_id = 1_000_009_i64;
    let hub_id = 77_000_001_i64;
    let skyhook_id = 77_000_002_i64;
    let sys_id = 30_000_142_u32;
    let fuel_item_id = 88_000_001_i64;
    let upg_item_id = 88_000_002_i64;

    let planet_id = sde
        .celestials_in_system(sys_id)
        .iter()
        .find(|c| c.group_id == GROUP_PLANET)
        .map(|c| c.celestial_id)
        .unwrap_or(40_009_080);

    db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.corporation_id = corp_id;
            ch.solar_system_id = sys_id;
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
            hub_id,
            UpwellStructureRecord {
                structure_id: hub_id,
                type_id: TYPE_SOVEREIGNTY_HUB,
                owner_corp_id: corp_id,
                solar_system_id: sys_id,
                name: "Equinox Hub".into(),
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
                name: "Orbital Skyhook".into(),
                state: "anchor_vulnerable".into(),
                ..Default::default()
            },
        );
        snap.sov_runtime.skyhook_planets.insert(skyhook_id, planet_id);
        snap.sov_runtime.skyhook_active.insert(skyhook_id, true);

        snap.items.insert(
            fuel_item_id,
            InventoryItemRecord {
                item_id: fuel_item_id,
                type_id: TYPE_SUPERIONIC_ICE,
                owner_id: cid,
                location_id: hub_id,
                flag: InventoryFlag::Hangar,
                quantity: 500,
                singleton: false,
                version: 1,
                dynamic_attributes: Default::default(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        snap.items.insert(
            upg_item_id,
            InventoryItemRecord {
                item_id: upg_item_id,
                type_id: 82_499, // zero-fuel site detection upgrade
                owner_id: cid,
                location_id: hub_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: false,
                version: 1,
                dynamic_attributes: Default::default(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
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

    // GetDefinitionsRequest
    let defs_resp = handlers["eve_public.sovereignty.hub.upgrade.api.GetDefinitionsRequest"](
        &dir_ctx,
        &dummy_any,
        &DynamicQuasarMessage::new("Req"),
    )
    .expect("GetDefinitionsRequest");
    assert!(defs_resp
        .type_url
        .ends_with("eve_public.sovereignty.hub.upgrade.api.GetDefinitionsResponse"));
    assert!(!defs_resp.value.is_empty());

    // GetResourcesRequest
    let res_resp = handlers["eve_public.sovereignty.hub.api.GetResourcesRequest"](
        &dir_ctx,
        &dummy_any,
        &id_msg(1, hub_id),
    )
    .expect("GetResourcesRequest");
    assert!(res_resp
        .type_url
        .ends_with("eve_public.sovereignty.hub.api.GetResourcesResponse"));

    // Fuel AddRequest & GetRequest
    let mut add_fuel = id_msg(1, fuel_item_id);
    add_fuel.fields.insert(2, DynamicValue::Int64(200));
    let mut hub_inner = DynamicQuasarMessage::new("Identifier");
    hub_inner.fields.insert(1, DynamicValue::Int64(hub_id));
    add_fuel.fields.insert(3, DynamicValue::Message(Box::new(hub_inner)));
    handlers["eve_public.sovereignty.hub.fuel.api.AddRequest"](&dir_ctx, &dummy_any, &add_fuel)
        .expect("AddRequest fuel");

    let fuel_resp = handlers["eve_public.sovereignty.hub.fuel.api.GetRequest"](
        &dir_ctx,
        &dummy_any,
        &id_msg(1, hub_id),
    )
    .expect("fuel GetRequest");
    assert!(fuel_resp
        .type_url
        .ends_with("eve_public.sovereignty.hub.fuel.api.GetResponse"));

    // ProcessConfigurationRequest: install 82_499 and set online = true
    let mut new_up_msg = DynamicQuasarMessage::new("NewUpgrade");
    new_up_msg.fields.insert(1, DynamicValue::Int64(upg_item_id));
    let mut cfg_entry = id_msg(1, 82_499);
    cfg_entry.fields.insert(2, DynamicValue::Bool(true));

    let mut proc_msg = id_msg(3, hub_id);
    proc_msg
        .fields
        .insert(1, DynamicValue::Repeated(vec![DynamicValue::Message(Box::new(new_up_msg))]));
    proc_msg
        .fields
        .insert(2, DynamicValue::Repeated(vec![DynamicValue::Message(Box::new(cfg_entry))]));

    let proc_resp = handlers["eve_public.sovereignty.hub.upgrade.api.ProcessConfigurationRequest"](
        &dir_ctx,
        &dummy_any,
        &proc_msg,
    )
    .expect("ProcessConfigurationRequest");
    assert!(proc_resp
        .type_url
        .ends_with("eve_public.sovereignty.hub.upgrade.api.ProcessConfigurationResponse"));

    // GetHubUpgradesRequest & upgrade.api.GetRequest & UninstallRequest
    let hub_upgs = handlers["eve_public.sovereignty.hub.upgrade.api.GetHubUpgradesRequest"](
        &dir_ctx,
        &dummy_any,
        &id_msg(1, hub_id),
    )
    .expect("GetHubUpgradesRequest");
    assert!(hub_upgs
        .type_url
        .ends_with("eve_public.sovereignty.hub.upgrade.api.GetHubUpgradesResponse"));

    let mut upg_id_inner = id_msg(1, hub_id);
    upg_id_inner.fields.insert(2, DynamicValue::Int64(82_499));
    let mut upg_req = DynamicQuasarMessage::new("UpgReq");
    upg_req
        .fields
        .insert(1, DynamicValue::Message(Box::new(upg_id_inner)));

    let get_upg = handlers["eve_public.sovereignty.hub.upgrade.api.GetRequest"](&dir_ctx, &dummy_any, &upg_req)
        .expect("upgrade GetRequest");
    assert!(get_upg
        .type_url
        .ends_with("eve_public.sovereignty.hub.upgrade.api.GetResponse"));

    let uninst = handlers["eve_public.sovereignty.hub.upgrade.api.UninstallRequest"](&dir_ctx, &dummy_any, &upg_req)
        .expect("UninstallRequest");
    assert!(uninst
        .type_url
        .ends_with("eve_public.sovereignty.hub.upgrade.api.UninstallResponse"));
}
