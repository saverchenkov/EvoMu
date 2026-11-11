//! Equinox Sovereignty Hub Workforce `Transit` relay traversal and Sovereignty/MercenaryDen
//! descriptor closure tests.

#![forbid(unsafe_code)]

use crate::quasar_sov_equinox_hub::{drain, field, fields, id_req, idm, rpc, sess, structure, subscribe, varint};
use eve_db::corp_roles;
use eve_proto::quasar::{encode_bytes_field, encode_varint_field};
use eve_server::quasar_gateway::STATUS_OK;
use eve_server::router::ServerContext;
use eve_server::services::quasar_sov_workforce::{systems_within_jumps, WORKFORCE_TRANSFER_MAX_JUMPS};
use eve_server::services::sov_services::TYPE_SOVEREIGNTY_HUB;

const PILOT: i64 = 90_000_001;
const CORP: i64 = 98_000_001;
const HUB_A: i64 = 1_020_000_101;
const HUB_B: i64 = 1_020_000_102;
const HUB_C: i64 = 1_020_000_103;
const DEN_ID: i64 = 1_020_000_201;

fn seed_hub(ctx: &ServerContext, hub_id: i64, sys: u32) {
    let planets: Vec<i64> = ctx
        .sde
        .celestials_in_system(sys)
        .into_iter()
        .filter(|c| c.group_id == 7)
        .map(|c| c.celestial_id)
        .collect();
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(hub_id, structure(hub_id, CORP, sys, TYPE_SOVEREIGNTY_HUB));
        for (idx, pid) in planets.into_iter().enumerate() {
            let sh_id = hub_id * 100 + (idx as i64) + 1;
            snap.structures.insert(sh_id, structure(sh_id, CORP, sys, 81_080));
            snap.sov_runtime.skyhook_planets.insert(sh_id, pid);
        }
    });
}

fn find_transit_chain(ctx: &ServerContext) -> (u32, u32, u32) {
    let mut sys_ids: Vec<u32> = ctx.sde.solar_systems.keys().copied().collect();
    sys_ids.sort_unstable();
    for &a in &sys_ids {
        if eve_server::services::quasar_sov_hub::system_yield(&ctx.sde, a).planet_workforce < 2_000 {
            continue;
        }
        let reach_a = systems_within_jumps(&ctx.sde, a, WORKFORCE_TRANSFER_MAX_JUMPS);
        if reach_a.len() < 2 {
            continue;
        }
        for (&b, &dab) in &reach_a {
            if dab == 0 || b == a {
                continue;
            }
            let reach_b = systems_within_jumps(&ctx.sde, b, WORKFORCE_TRANSFER_MAX_JUMPS);
            for (&c, &dbc) in &reach_b {
                if dbc > 0 && c != a && c != b && !reach_a.contains_key(&c) {
                    return (a, b, c);
                }
            }
        }
    }
    panic!("Expected a 3-system chain (a -> b <= 5 jumps, b -> c <= 5 jumps, a -> c > 5 jumps) in SDE");
}

fn encode_configure_req(hub_id: i64, cfg_bytes: &[u8]) -> Vec<u8> {
    let mut out = id_req(1, hub_id as u64);
    encode_bytes_field(2, cfg_bytes, &mut out);
    out
}

#[tokio::test]
async fn test_sov_workforce_transit_relay_traversal_and_notices() {
    let ctx = ServerContext::new();
    let (sys_a, sys_b, sys_c) = find_transit_chain(&ctx);
    seed_hub(&ctx, HUB_A, sys_a);
    seed_hub(&ctx, HUB_B, sys_b);
    seed_hub(&ctx, HUB_C, sys_c);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT) {
            ch.solar_system_id = sys_a;
        }
    });

    let mut s = sess(9101, PILOT, CORP, sys_a, corp_roles::ROLE_DIRECTOR);
    let mut rx = subscribe(&ctx, &mut s).await;

    // 1. Before HUB_B is Transit, HUB_C (> 5 jumps from HUB_A) is NOT networkable from HUB_A.
    let net_before = rpc(&ctx, &mut s, "eve_public.sovereignty.hub.workforce.api.GetNetworkableHubsRequest", id_req(1, HUB_A as u64)).await;
    assert_eq!(net_before.status_code, STATUS_OK);
    let ids_before: Vec<u64> = fields(&net_before.payload.unwrap().value, 1)
        .into_iter()
        .filter_map(|h| field(&h, 1).and_then(|id| varint(&id, 1)))
        .collect();
    assert!(ids_before.contains(&(HUB_B as u64)));
    assert!(!ids_before.contains(&(HUB_C as u64)));

    // 2. Configure HUB_B to Transit mode (Configuration field 4 = true).
    let mut transit_cfg = Vec::new();
    encode_varint_field(4, 1, &mut transit_cfg);
    let rsp_b = rpc(&ctx, &mut s, "eve_public.sovereignty.hub.workforce.api.ConfigureRequest", encode_configure_req(HUB_B, &transit_cfg)).await;
    assert_eq!(rsp_b.status_code, STATUS_OK);

    // 3. Now HUB_C IS networkable from HUB_A via Transit relay HUB_B.
    let net_after = rpc(&ctx, &mut s, "eve_public.sovereignty.hub.workforce.api.GetNetworkableHubsRequest", id_req(1, HUB_A as u64)).await;
    assert_eq!(net_after.status_code, STATUS_OK);
    let ids_after: Vec<u64> = fields(&net_after.payload.unwrap().value, 1)
        .into_iter()
        .filter_map(|h| field(&h, 1).and_then(|id| varint(&id, 1)))
        .collect();
    assert!(ids_after.contains(&(HUB_C as u64)), "HUB_C must be reachable via Transit HUB_B");

    // 4. Configure HUB_A to Export 200 to sys_c, and HUB_C to Import from sys_a.
    let mut exp_inner = id_req(3, u64::from(sys_c));
    encode_varint_field(2, 200, &mut exp_inner);
    let mut exp_cfg = Vec::new();
    encode_bytes_field(3, &exp_inner, &mut exp_cfg);
    let rsp_a = rpc(&ctx, &mut s, "eve_public.sovereignty.hub.workforce.api.ConfigureRequest", encode_configure_req(HUB_A, &exp_cfg)).await;
    assert_eq!(rsp_a.status_code, STATUS_OK);

    let mut src = Vec::new();
    encode_bytes_field(1, &idm(u64::from(sys_a)), &mut src);
    let mut imp_inner = Vec::new();
    encode_bytes_field(1, &src, &mut imp_inner);
    let mut imp_cfg = Vec::new();
    encode_bytes_field(2, &imp_inner, &mut imp_cfg);
    let rsp_c = rpc(&ctx, &mut s, "eve_public.sovereignty.hub.workforce.api.ConfigureRequest", encode_configure_req(HUB_C, &imp_cfg)).await;
    assert_eq!(rsp_c.status_code, STATUS_OK);

    // Verify HUB_C imports 200 workforce from sys_a.
    let st_c = rpc(&ctx, &mut s, "eve_public.sovereignty.hub.workforce.api.GetStateRequest", id_req(1, HUB_C as u64)).await;
    assert_eq!(st_c.status_code, STATUS_OK);
    let state_msg = field(&st_c.payload.unwrap().value, 1).unwrap();
    let imp_state = field(&state_msg, 2).unwrap();
    let src_entry = field(&imp_state, 1).unwrap();
    assert_eq!(varint(&src_entry, 2), Some(200));

    // 5. Switch HUB_B back to Inactive; verify StateChangedNotice is emitted and HUB_C import drops to 0.
    let _ = drain(&mut rx);
    let mut inactive_cfg = Vec::new();
    encode_varint_field(1, 1, &mut inactive_cfg);
    let rsp_off = rpc(&ctx, &mut s, "eve_public.sovereignty.hub.workforce.api.ConfigureRequest", encode_configure_req(HUB_B, &inactive_cfg)).await;
    assert_eq!(rsp_off.status_code, STATUS_OK);

    let notices = drain(&mut rx);
    assert!(
        notices.iter().any(|(t, _)| t == "eve_public.sovereignty.resource.transfer.workforce.api.StateChangedNotice"),
        "Breaking Transit relay must emit StateChangedNotice"
    );

    let st_c_after = rpc(&ctx, &mut s, "eve_public.sovereignty.hub.workforce.api.GetStateRequest", id_req(1, HUB_C as u64)).await;
    let state_msg_after = field(&st_c_after.payload.unwrap().value, 1).unwrap();
    let imp_state_after = field(&state_msg_after, 2).unwrap();
    let src_entry_after = field(&imp_state_after, 1).unwrap();
    assert_eq!(varint(&src_entry_after, 2), Some(0));
}

#[tokio::test]
async fn test_sov_hub_mercenaryden_and_star_resources_descriptors() {
    // parity: quasar:eveProto/generated/eve_public/sovereignty/hub/api/requests_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/sovereignty/resource/star/api/requests_pb2.pyj
    let ctx = ServerContext::new();
    let sys_a = 30_000_142u32;
    seed_hub(&ctx, HUB_A, sys_a);

    let mut s = sess(9102, PILOT, CORP, sys_a, corp_roles::ROLE_DIRECTOR);
    s.role = eve_machonet::service_roles::ROLE_PLAYER_DEFAULT | eve_machonet::service_roles::ROLE_ADMIN;
    let mut rx = subscribe(&ctx, &mut s).await;

    // 1. sovereignty.hub.api.GetResourcesRequest
    let res = rpc(&ctx, &mut s, "eve_public.sovereignty.hub.api.GetResourcesRequest", id_req(1, HUB_A as u64)).await;
    assert_eq!(res.status_code, STATUS_OK);
    let res_bytes = res.payload.unwrap().value;
    let power_pair = field(&res_bytes, 1).expect("power pair");
    assert!(varint(&power_pair, 1).unwrap_or(0) > 0, "Jita hub must have positive available power");

    // 2. sovereignty.resource.star.api.GetAllConfigurationsRequest
    let expected_star_id = ctx
        .sde
        .celestials_in_system(sys_a)
        .into_iter()
        .find(|c| c.group_id == 6)
        .expect("SDE star in system")
        .celestial_id as u64;
    let stars = rpc(&ctx, &mut s, "eve_public.sovereignty.resource.star.api.GetAllConfigurationsRequest", Vec::new()).await;
    assert_eq!(stars.status_code, STATUS_OK);
    let star_cfgs = fields(&stars.payload.unwrap().value, 1);
    assert!(!star_cfgs.is_empty(), "Must return SDE star configurations");
    let jita_star_cfg = star_cfgs
        .iter()
        .find(|cfg| field(cfg, 1).and_then(|id| varint(&id, 1)) == Some(expected_star_id))
        .expect("GetAllConfigurationsResponse must include Jita SDE star");
    assert!(varint(jita_star_cfg, 2).unwrap_or(0) > 0);

    // 3. Seed Mercenary Den and verify EvolutionStateChangedNotice + Activity requests/notices.
    let planet_id = 40_009_048i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.sov_runtime.mercenary_dens.insert(
            DEN_ID,
            eve_db::PersistedMercenaryDen {
                den_id: DEN_ID,
                owner_char_id: PILOT,
                solar_system_id: sys_a,
                planet_id,
                type_id: 85_230,
                enabled: true,
                ..Default::default()
            },
        );
    });

    let mut mod_dev = id_req(1, DEN_ID as u64);
    encode_varint_field(2, 2, &mut mod_dev);
    let dev_rsp = rpc(&ctx, &mut s, "eve_public.sovereignty.mercenaryden.api.admin.ModifyDevelopmentLevelRequest", mod_dev).await;
    assert_eq!(dev_rsp.status_code, STATUS_OK);
    let notices = drain(&mut rx);
    assert!(
        notices.iter().any(|(t, _)| t == "eve_public.sovereignty.mercenaryden.api.EvolutionStateChangedNotice"),
        "ModifyDevelopmentLevelRequest must emit EvolutionStateChangedNotice"
    );

    let acts = rpc(&ctx, &mut s, "eve_public.sovereignty.mercenaryden.activity.api.GetForMercenaryDenRequest", id_req(1, DEN_ID as u64)).await;
    assert_eq!(acts.status_code, STATUS_OK);
    let act_list = fields(&acts.payload.unwrap().value, 1);
    assert!(!act_list.is_empty(), "Must return generated mercenary den activities");
    let cap = rpc(&ctx, &mut s, "eve_public.sovereignty.mercenaryden.activity.api.GetCapacityRequest", Vec::new()).await;
    assert_eq!(cap.status_code, STATUS_OK);
    let cap_bytes = cap.payload.unwrap().value;
    assert!(varint(&cap_bytes, 1).unwrap_or(0) >= 1, "Must return positive activity capacity");
}

#[tokio::test]
async fn test_sov_workforce_transit_disconnect_drops_starved_upgrades_to_low_power() {
    // Regression test for B-03: severing a Transit relay must immediately drop starved zero-fuel
    // downstream upgrades into POWER_STATE_LOW (3) and emit HubUpgradesSimulatedNotice.
    const MINOR_THREAT_1: u32 = 82_499; // 200 power, 1_800 workforce, 0 fuel/hr
    const UPGRADE_ITEM_ID: i64 = 995_001;

    let ctx = ServerContext::new();
    let (sys_a, sys_b, sys_c) = find_transit_chain(&ctx);
    seed_hub(&ctx, HUB_A, sys_a);
    seed_hub(&ctx, HUB_B, sys_b);
    // Seed HUB_C WITHOUT Orbital Skyhooks so HUB_C has 0 local workforce and depends entirely on imported workforce.
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.entry(CORP).or_default().ceo_id = PILOT;
        snap.corporations.entry(CORP).or_default().members.insert(PILOT);
        snap.corporations.entry(CORP).or_default().member_roles.insert(PILOT, corp_roles::ROLE_DIRECTOR);
        snap.structures.insert(HUB_C, structure(HUB_C, CORP, sys_c, TYPE_SOVEREIGNTY_HUB));
        snap.items.insert(
            UPGRADE_ITEM_ID,
            eve_db::InventoryItemRecord {
                item_id: UPGRADE_ITEM_ID,
                type_id: MINOR_THREAT_1,
                owner_id: CORP,
                location_id: HUB_C,
                flag: eve_db::InventoryFlag::CorpSAG1,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        if let Some(ch) = snap.characters.get_mut(&PILOT) {
            ch.solar_system_id = sys_c;
        }
    });

    let mut sc = sess(9103, PILOT, CORP, sys_c, corp_roles::ROLE_DIRECTOR);
    let mut rx_c = subscribe(&ctx, &mut sc).await;

    // 1. Enable Transit on HUB_B, export 2_000 workforce from HUB_A -> sys_c, and import from sys_a on HUB_C.
    let mut transit_cfg = Vec::new();
    encode_varint_field(4, 1, &mut transit_cfg);
    assert_eq!(
        rpc(&ctx, &mut sc, "eve_public.sovereignty.hub.workforce.api.ConfigureRequest", encode_configure_req(HUB_B, &transit_cfg)).await.status_code,
        STATUS_OK
    );

    let mut exp_inner = id_req(3, u64::from(sys_c));
    encode_varint_field(2, 2_000, &mut exp_inner);
    let mut exp_cfg = Vec::new();
    encode_bytes_field(3, &exp_inner, &mut exp_cfg);
    assert_eq!(
        rpc(&ctx, &mut sc, "eve_public.sovereignty.hub.workforce.api.ConfigureRequest", encode_configure_req(HUB_A, &exp_cfg)).await.status_code,
        STATUS_OK
    );

    let mut src = Vec::new();
    encode_bytes_field(1, &idm(u64::from(sys_a)), &mut src);
    let mut imp_inner = Vec::new();
    encode_bytes_field(1, &src, &mut imp_inner);
    let mut imp_cfg = Vec::new();
    encode_bytes_field(2, &imp_inner, &mut imp_cfg);
    assert_eq!(
        rpc(&ctx, &mut sc, "eve_public.sovereignty.hub.workforce.api.ConfigureRequest", encode_configure_req(HUB_C, &imp_cfg)).await.status_code,
        STATUS_OK
    );

    // 2. Install & online zero-fuel Minor Threat Detection Array 1 (82499) on HUB_C; verify POWER_STATE_ONLINE (2).
    let mut install_req = Vec::new();
    encode_bytes_field(1, &idm(UPGRADE_ITEM_ID as u64), &mut install_req);
    let mut cfg_entry = Vec::new();
    encode_bytes_field(1, &idm(u64::from(MINOR_THREAT_1)), &mut cfg_entry);
    encode_varint_field(2, 1, &mut cfg_entry);
    encode_bytes_field(2, &cfg_entry, &mut install_req);
    encode_bytes_field(3, &idm(HUB_C as u64), &mut install_req);
    let install_rsp = rpc(&ctx, &mut sc, "eve_public.sovereignty.hub.upgrade.api.ProcessConfigurationRequest", install_req).await;
    assert_eq!(install_rsp.status_code, STATUS_OK);
    let up_before = field(&field(&install_rsp.payload.unwrap().value, 1).unwrap(), 2).unwrap();
    assert_eq!(field(&up_before, 2).and_then(|a| varint(&a, 2)), Some(2), "POWER_STATE_ONLINE while Transit supplies workforce");

    // 3. Disable Transit on HUB_B (Transit -> Inactive) and verify HUB_C upgrade immediately drops to POWER_STATE_LOW (3) with HubUpgradesSimulatedNotice.
    let _ = drain(&mut rx_c);
    let mut inactive_cfg = Vec::new();
    encode_varint_field(1, 1, &mut inactive_cfg);
    let off_rsp = rpc(&ctx, &mut sc, "eve_public.sovereignty.hub.workforce.api.ConfigureRequest", encode_configure_req(HUB_B, &inactive_cfg)).await;
    assert_eq!(off_rsp.status_code, STATUS_OK);

    let notices = drain(&mut rx_c);
    let sim_notice = notices
        .iter()
        .find(|(t, _)| t == "eve_public.sovereignty.hub.upgrade.api.HubUpgradesSimulatedNotice")
        .expect("Disabling Transit relay must emit HubUpgradesSimulatedNotice on starved HUB_C");
    let sim_up = field(&field(&sim_notice.1, 1).unwrap(), 2).unwrap();
    assert_eq!(field(&sim_up, 2).and_then(|a| varint(&a, 2)), Some(3), "Notice must report POWER_STATE_LOW (3)");

    let res_notice = notices
        .iter()
        .find(|(t, _)| t == "eve_public.sovereignty.hub.api.ResourcesSimulatedNotice")
        .expect("Disabling Transit relay must emit post-overload ResourcesSimulatedNotice on HUB_C");
    assert_eq!(field(&res_notice.1, 4).and_then(|w| varint(&w, 1)), Some(0), "HUB_C available workforce must drop to 0");
    assert_eq!(field(&res_notice.1, 4).and_then(|w| varint(&w, 2)), Some(0), "Low-power upgrade must release workforce allocation");

    let get_hu = rpc(&ctx, &mut sc, "eve_public.sovereignty.hub.upgrade.api.GetHubUpgradesRequest", id_req(1, HUB_C as u64)).await;
    assert_eq!(get_hu.status_code, STATUS_OK);
    let up_after = field(&field(&get_hu.payload.unwrap().value, 1).unwrap(), 2).unwrap();
    assert_eq!(field(&up_after, 2).and_then(|a| varint(&a, 2)), Some(3), "GetHubUpgradesRequest must persist POWER_STATE_LOW (3)");

    // 4. Destroyed structures in `snap.structures` must be excluded from `GetNetworkableHubsRequest`.
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(s) = snap.structures.get_mut(&HUB_B) {
            s.state = "destroyed".to_string();
        }
    });
    let net_after_destroy = rpc(&ctx, &mut sc, "eve_public.sovereignty.hub.workforce.api.GetNetworkableHubsRequest", id_req(1, HUB_C as u64)).await;
    assert_eq!(net_after_destroy.status_code, STATUS_OK);
    assert!(
        fields(&net_after_destroy.payload.unwrap().value, 1).is_empty(),
        "Destroyed sovereignty hub must not appear in networkable_hubs"
    );
}
