//! End-to-end parity closure tests (Part 3) for Sovereignty Hub Power, Workforce,
//! Fuel, Upgrades, Skyhook Reagents & Sovereignty Upgrade Sites.

#![forbid(unsafe_code)]

use crate::quasar_sov_equinox_hub::{field, fields, id_req, idm, rpc, structure, subscribe, varint};
use crate::sovereignty_equinox_gameplay_closure::{
    drain_quasar, find_nullsec_planets, fit_siege_artillery, lock_target_via_rpc, make_ball,
    open_sqlite_ctx, place_ship_ball, register_online_pilot, rpc_call, step_weapon_ticks,
    temp_sqlite_path, ALLIANCE, CORP, PILOT, PILOT_SHIP,
};
use eve_db::{corp_roles, InventoryFlag, InventoryItemRecord};
use eve_proto::quasar::{encode_bytes_field, encode_varint_field};
use eve_proto::PyRep;
use eve_server::services::sov_gameplay::{UPGRADE_MAJOR_THREAT_DETECTION, UPGRADE_PROSPECTING_ARRAY};
use eve_server::services::sov_services::{
    claim_system_sovereignty, TYPE_MAGMATIC_GAS, TYPE_ORBITAL_SKYHOOK, TYPE_SOVEREIGNTY_HUB,
    TYPE_SUPERIONIC_ICE,
};
use glam::DVec3;

#[tokio::test]
async fn test_sov_power_workforce_fuel_upgrades_reagents_and_sites_parity() {
    // parity: feat:sovereignty.sov-hub-upgrades
    // parity: feat:sovereignty.sov-power
    // parity: feat:sovereignty.sov-workforce
    // parity: feat:sovereignty.sov-reagents
    // parity: feat:sovereignty.sov-hub-fuel
    // parity: feat:sovereignty.sov-upgrade-sites
    eve_testkit::attest(&[
        "feat:sovereignty.sov-hub-upgrades",
        "feat:sovereignty.sov-power",
        "feat:sovereignty.sov-workforce",
        "feat:sovereignty.sov-reagents",
        "feat:sovereignty.sov-hub-fuel",
        "feat:sovereignty.sov-upgrade-sites",
    ]);

    let db_path = temp_sqlite_path("sov_power_wf_sites");
    let ctx = open_sqlite_ctx(&db_path, true);
    let (null_sys, _, lava_planet, ice_planet, _) = find_nullsec_planets(&ctx);
    let neighbor_sys = ctx
        .sde
        .jumps
        .values()
        .find(|j| {
            j.from_solar_system_id == null_sys
                && ctx.sde.solar_systems.get(&j.to_solar_system_id).is_some_and(|s| {
                    s.security_status <= 0.0
                        && ctx.sde.constellations.get(&s.constellation_id).and_then(|c| c.faction_id).is_none()
                        && ctx.sde.regions.get(&s.region_id).and_then(|r| r.faction_id).is_none()
                })
        })
        .map(|j| j.to_solar_system_id)
        .expect("adjacent null-sec system");
    let cels = ctx.sde.celestials_in_system(null_sys);
    let lava_pos = cels.iter().find(|c| c.celestial_id == lava_planet).unwrap().position;
    let ice_pos = cels.iter().find(|c| c.celestial_id == ice_planet).unwrap().position;
    let (hub_a, hub_b) = (1_090_000_001i64, 1_090_000_002i64);

    let mut s_dir = register_online_pilot(
        &ctx,
        83_001,
        "demo",
        "demo-secret",
        PILOT,
        CORP,
        Some(ALLIANCE),
        null_sys,
        PILOT_SHIP,
        19_722,
        lava_pos,
        corp_roles::ROLE_DIRECTOR,
    )
    .await;
    let mut rx = subscribe(&ctx, &mut s_dir).await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states.entry(null_sys).or_default().solar_system_id = null_sys;
        snap.system_states.entry(neighbor_sys).or_default().solar_system_id = neighbor_sys;
        snap.structures.insert(hub_a, structure(hub_a, CORP, null_sys, TYPE_SOVEREIGNTY_HUB));
        snap.structures.insert(hub_b, structure(hub_b, CORP, neighbor_sys, TYPE_SOVEREIGNTY_HUB));
        for (iid, tid, loc, qty) in [
            (982_001, TYPE_MAGMATIC_GAS, hub_a, 3_000),
            (982_002, TYPE_SUPERIONIC_ICE, hub_a, 3_000),
            (982_003, 88_227, hub_a, 1), // Power Monitoring Division 3 (+750 power)
            (982_004, 88_229, hub_a, 1), // Workforce Mecha-Tooling 3 (+6000 workforce)
            (982_005, UPGRADE_MAJOR_THREAT_DETECTION, hub_a, 1),
            (982_006, UPGRADE_PROSPECTING_ARRAY, hub_a, 1),
            (982_007, UPGRADE_MAJOR_THREAT_DETECTION, hub_a, 1),
            (982_010, TYPE_ORBITAL_SKYHOOK, PILOT_SHIP, 2),
        ] {
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: tid,
                    owner_id: PILOT,
                    location_id: loc,
                    flag: InventoryFlag::Cargo,
                    quantity: qty,
                    ..Default::default()
                },
            );
        }
    });
    let _ = ctx.systems.get_or_spawn(null_sys).spawn_ball(make_ball(hub_a, TYPE_SOVEREIGNTY_HUB, DVec3::new(200_000.0, 0.0, 0.0))).await;
    claim_system_sovereignty(&ctx, null_sys, ALLIANCE, CORP, hub_a).expect("claim null_sys");
    claim_system_sovereignty(&ctx, neighbor_sys, ALLIANCE, CORP, hub_b).expect("claim neighbor_sys");

    // 1. Upgrade definitions from SDE (`GetDefinitionsRequest`) & Fuel deposit (`fuel.api.AddRequest` / `GetRequest`).
    let defs_rsp = rpc(&ctx, &mut s_dir, "eve_public.sovereignty.hub.upgrade.api.GetDefinitionsRequest", Vec::new()).await;
    let defs = fields(&defs_rsp.payload.unwrap().value, 1);
    assert_eq!(defs.iter().any(|d| varint(&field(d, 1).unwrap(), 1) == Some(UPGRADE_MAJOR_THREAT_DETECTION as u64)), true);
    assert_eq!(defs.iter().any(|d| varint(&field(d, 1).unwrap(), 1) == Some(UPGRADE_PROSPECTING_ARRAY as u64)), true);

    for (fuel_iid, amt) in [(982_001i64, 1_000u64), (982_002i64, 1_000u64)] {
        let mut add = id_req(1, fuel_iid as u64);
        encode_varint_field(2, amt, &mut add);
        encode_bytes_field(3, &idm(hub_a as u64), &mut add);
        let r = rpc(&ctx, &mut s_dir, "eve_public.sovereignty.hub.fuel.api.AddRequest", add).await;
        assert_eq!(r.status_code, 200);
    }
    let _ = drain_quasar(&mut rx);
    let fuel_get = rpc(&ctx, &mut s_dir, "eve_public.sovereignty.hub.fuel.api.GetRequest", id_req(1, hub_a as u64)).await.payload.unwrap().value;
    let fuel_entries = fields(&fuel_get, 1);
    assert_eq!(fuel_entries.len(), 2);
    assert_eq!(fuel_entries.iter().all(|f| varint(f, 2) == Some(1_000)), true);
    assert_eq!(ctx.db.with_snapshot(|s| s.items[&982_001].quantity), 2_000);

    // 2. Cross-system Workforce transfer (`GetNetworkableHubsRequest` + `ConfigureRequest`).
    let net_rsp = rpc(&ctx, &mut s_dir, "eve_public.sovereignty.hub.workforce.api.GetNetworkableHubsRequest", id_req(1, hub_a as u64)).await.payload.unwrap().value;
    assert_eq!(fields(&net_rsp, 1).iter().any(|b| field(b, 1).and_then(|h| varint(&h, 1)) == Some(hub_b as u64)), true);
    let wf0 = varint(&field(&rpc(&ctx, &mut s_dir, "eve_public.sovereignty.hub.api.GetResourcesRequest", id_req(1, hub_a as u64)).await.payload.unwrap().value, 2).unwrap(), 1).unwrap();
    let (mut exp, mut exp_cfg, mut cfg_b) = (Vec::new(), Vec::new(), id_req(1, hub_b as u64));
    encode_bytes_field(3, &idm(null_sys as u64), &mut exp);
    encode_varint_field(2, 400, &mut exp);
    encode_bytes_field(3, &exp, &mut exp_cfg);
    encode_bytes_field(2, &exp_cfg, &mut cfg_b);
    assert_eq!(rpc(&ctx, &mut s_dir, "eve_public.sovereignty.hub.workforce.api.ConfigureRequest", cfg_b).await.status_code, 200);
    let (mut src, mut imp, mut imp_cfg, mut cfg_a) = (Vec::new(), Vec::new(), Vec::new(), id_req(1, hub_a as u64));
    encode_bytes_field(1, &idm(neighbor_sys as u64), &mut src);
    encode_bytes_field(1, &src, &mut imp);
    encode_bytes_field(2, &imp, &mut imp_cfg);
    encode_bytes_field(2, &imp_cfg, &mut cfg_a);
    assert_eq!(rpc(&ctx, &mut s_dir, "eve_public.sovereignty.hub.workforce.api.ConfigureRequest", cfg_a).await.status_code, 200);
    let wf1 = varint(&field(&rpc(&ctx, &mut s_dir, "eve_public.sovereignty.hub.api.GetResourcesRequest", id_req(1, hub_a as u64)).await.payload.unwrap().value, 2).unwrap(), 1).unwrap();
    assert_eq!(wf1, wf0 + 400);

    // 3. Install & online upgrades + exclusive group conflict check (`Major Threat Detection Array`).
    let mut bad_proc = Vec::new();
    for iid in [982_003u64, 982_004, 982_005, 982_007] {
        encode_bytes_field(1, &idm(iid), &mut bad_proc);
    }
    for tid in [88_227u64, 88_229, UPGRADE_MAJOR_THREAT_DETECTION as u64] {
        let mut c = Vec::new();
        encode_bytes_field(1, &idm(tid), &mut c);
        encode_varint_field(2, 1, &mut c);
        encode_bytes_field(2, &c, &mut bad_proc);
    }
    encode_bytes_field(3, &idm(hub_a as u64), &mut bad_proc);
    assert_eq!(rpc(&ctx, &mut s_dir, "eve_public.sovereignty.hub.upgrade.api.ProcessConfigurationRequest", bad_proc).await.status_code, 400);

    let mut proc = Vec::new();
    for iid in [982_003u64, 982_004, 982_005, 982_006] {
        encode_bytes_field(1, &idm(iid), &mut proc);
    }
    for tid in [88_227u64, 88_229, UPGRADE_MAJOR_THREAT_DETECTION as u64, UPGRADE_PROSPECTING_ARRAY as u64] {
        let mut c = Vec::new();
        encode_bytes_field(1, &idm(tid), &mut c);
        encode_varint_field(2, 1, &mut c);
        encode_bytes_field(2, &c, &mut proc);
    }
    encode_bytes_field(3, &idm(hub_a as u64), &mut proc);
    assert_eq!(rpc(&ctx, &mut s_dir, "eve_public.sovereignty.hub.upgrade.api.ProcessConfigurationRequest", proc).await.status_code, 200);

    // 4. Spawn upgrade sites (`tick_background_cluster`), verify mineable asteroid & killable NPC rat balls in space!
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    let balls = ctx.systems.get_or_spawn(null_sys).list_balls().await.unwrap();
    let ore_ball = balls
        .iter()
        .find(|b| matches!(b.type_id, 18..=22 | 1_223..=1_232))
        .expect("spawned asteroid ball");
    let npc_ball = balls.iter().find(|b| b.type_id == 37_453).expect("spawned combat NPC ball");
    assert_eq!(ctx.db.with_snapshot(|s| s.cosmic_anomalies[&null_sys].len()), 2);

    // Kill the spawned upgrade-site NPC rat via live weapon fire -> credits sovereign Military Index!
    fit_siege_artillery(&ctx, PILOT, PILOT_SHIP, 982_200);
    ctx.invalidate_ship_fitting(PILOT_SHIP);
    place_ship_ball(&ctx, null_sys, PILOT, PILOT_SHIP, 19_722, npc_ball.position + DVec3::new(2_500.0, 0.0, 0.0)).await;
    lock_target_via_rpc(&ctx, &mut s_dir, null_sys, PILOT_SHIP, npc_ball.id).await;
    let mut last_shot = PyRep::None;
    for _round in 0..4 {
        for gun_id in [982_200i64, 982_202, 982_204] {
            ctx.set_invention_roll_override(Some(0.05));
            last_shot = rpc_call(&ctx, &mut s_dir, "dogmaIM", "Activate", &[PyRep::Integer(gun_id), PyRep::Integer(npc_ball.id)])
                .await
                .expect("activate turret on upgrade-site NPC");
            if last_shot.dict_get("destroyed") == Some(&PyRep::Bool(true))
                || ctx.db.with_snapshot(|s| s.sov_runtime.sov_npc_kills.get(&null_sys).copied().unwrap_or(0)) >= 1
            {
                break;
            }
        }
        if ctx.db.with_snapshot(|s| s.sov_runtime.sov_npc_kills.get(&null_sys).copied().unwrap_or(0)) >= 1 {
            break;
        }
        step_weapon_ticks(&ctx, null_sys).await;
    }
    assert_eq!(
        ctx.db.with_snapshot(|s| s.sov_runtime.sov_npc_kills.get(&null_sys).copied().unwrap_or(0)),
        1,
        "last_shot={last_shot:?}"
    );
    assert_eq!(matches!(ore_ball.type_id, 18..=22 | 1_223..=1_232), true);

    // 5. Skyhook reagents on Lava (81143 Magmatic Gas) & Ice (81144 Superionic Ice) + extraction to ship cargo.
    place_ship_ball(&ctx, null_sys, PILOT, PILOT_SHIP, 19_722, lava_pos).await;
    let lava_sh = rpc_call(&ctx, &mut s_dir, "structureDeployment", "Anchor", &[PyRep::Integer(TYPE_ORBITAL_SKYHOOK as i64), PyRep::Integer(lava_planet)])
        .await.unwrap().dict_get("structureID").and_then(|v| v.as_i64()).unwrap();
    place_ship_ball(&ctx, null_sys, PILOT, PILOT_SHIP, 19_722, ice_pos).await;
    let ice_sh = rpc_call(&ctx, &mut s_dir, "structureDeployment", "Anchor", &[PyRep::Integer(TYPE_ORBITAL_SKYHOOK as i64), PyRep::Integer(ice_planet)])
        .await.unwrap().dict_get("structureID").and_then(|v| v.as_i64()).unwrap();

    // Advance 1h to harvest initial insecure batch, then 24h to mature secured reagents, then extract Magmatic Gas to ship cargo.
    ctx.advance_secs(3_600);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    ctx.advance_secs(24 * 3_600);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    let (lt, lsec, _, _) = ctx.db.with_snapshot(|s| s.sov_runtime.skyhook_reagent_bays[&lava_sh]);
    let (it, isec, _, _) = ctx.db.with_snapshot(|s| s.sov_runtime.skyhook_reagent_bays[&ice_sh]);
    assert_eq!(lt, TYPE_MAGMATIC_GAS);
    assert_eq!(it, TYPE_SUPERIONIC_ICE);
    assert_eq!(lsec > 0 && isec > 0, true);

    let lava_sh_pos = ctx.db.with_snapshot(|s| DVec3::from_array(s.structures[&lava_sh].position));
    place_ship_ball(&ctx, null_sys, PILOT, PILOT_SHIP, 19_722, lava_sh_pos + DVec3::new(2_500.0, 0.0, 0.0)).await;
    let ext = rpc_call(&ctx, &mut s_dir, "beyonce", "CallComponentFromClient", &[PyRep::Integer(lava_sh), PyRep::String("orbitalSkyhook".into()), PyRep::String("ExtractReagentsToShip".into())])
        .await.expect("extract Magmatic Gas");
    assert_eq!(ext.dict_get("extractedQuantity").and_then(|v| v.as_i64()), Some(lsec as i64));

    // Advance 200 more hours so Hub burns through all remaining fuel -> all upgrades drop to POWER_STATE_LOW (0 allocated power).
    ctx.advance_secs(200 * 3_600);
    let res_post = rpc(&ctx, &mut s_dir, "eve_public.sovereignty.hub.api.GetResourcesRequest", id_req(1, hub_a as u64)).await.payload.unwrap().value;
    assert_eq!(varint(&field(&res_post, 1).unwrap(), 2), Some(0), "fuel exhaustion drops upgrades to low power (0 allocated)");

    drop(ctx);
    let _ = std::fs::remove_file(&db_path);
}
