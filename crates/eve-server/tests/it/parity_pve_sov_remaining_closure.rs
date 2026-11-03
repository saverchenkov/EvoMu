//! End-to-end parity closure tests (Part 1) for Equinox Sovereignty Hub
//! anchoring, fuel, upgrades, power, workforce, reagents, and Mercenary Dens.

#![forbid(unsafe_code)]

use crate::quasar_sov_equinox_hub::{field, id_req, idm, rpc, structure, subscribe, varint};
use crate::sovereignty_equinox_gameplay_closure::{
    drain_quasar, find_nullsec_planets, fit_subcap_blasters, lock_target_via_rpc, make_ball,
    open_sqlite_ctx, place_ship_ball, register_online_pilot, rpc_call, set_pilot_location,
    step_weapon_ticks, temp_sqlite_path, ALLIANCE, CORP, PILOT, PILOT_SHIP,
};

fn err_code<T: std::fmt::Debug>(res: Result<T, eve_proto::BridgeRpcError>) -> String {
    res.unwrap_err().code
}
use eve_db::{corp_roles, InventoryFlag, InventoryItemRecord, TrainedSkillRecord};
use eve_proto::quasar::{encode_bytes_field, encode_varint_field};
use eve_proto::PyRep;
use eve_server::quasar_gateway::STATUS_OK;
use eve_server::services::quasar_sov_hub::{compute_hub_resources, simulate_fuel, star_power};
use eve_server::services::quasar_sov_mercden::TYPE_MERCENARY_DEN_MANAGEMENT;
use eve_server::services::sov_gameplay::{
    TYPE_MERCENARY_DEN, UPGRADE_MAJOR_THREAT_DETECTION, UPGRADE_PROSPECTING_ARRAY,
};
use eve_server::services::sov_services::{
    claim_system_sovereignty, TYPE_ORBITAL_SKYHOOK, TYPE_SOVEREIGNTY_HUB, TYPE_SUPERIONIC_ICE,
};
use eve_server::services::station_structure::structure_lifecycle as life;
use glam::DVec3;

#[tokio::test]
async fn test_sov_hub_anchor_upgrades_power_and_fuel_closure() {
    // parity: feat:sovereignty.sov-hub-anchor
    // parity: feat:sovereignty.sov-hub-fuel
    // parity: feat:sovereignty.sov-hub-upgrades
    // parity: feat:sovereignty.sov-power
    eve_testkit::attest(&[
        "feat:sovereignty.sov-hub-anchor",
        "feat:sovereignty.sov-hub-fuel",
        "feat:sovereignty.sov-hub-upgrades",
        "feat:sovereignty.sov-power",
    ]);

    let db_path = temp_sqlite_path("parity_sov_hub_core");
    let ctx = open_sqlite_ctx(&db_path, true);
    crate::common::pin_clock(&ctx);
    let null_sys = ctx
        .sde
        .solar_systems
        .values()
        .filter(|s| {
            s.security_status <= 0.0
                && s.region_id < 11_000_000
                && ctx.sde.regions.get(&s.region_id).is_none_or(|r| r.faction_id.is_none())
        })
        .find(|s| eve_server::services::quasar_sov_hub::system_yield(&ctx.sde, s.solar_system_id).planet_workforce >= 2_500)
        .map(|s| s.solar_system_id)
        .unwrap_or_else(|| find_nullsec_planets(&ctx).0);
    let hub_pos = DVec3::new(150_000.0, 0.0, 0.0);

    let mut s_dir = register_online_pilot(
        &ctx,
        88_001,
        "demo",
        "demo-secret",
        PILOT,
        CORP,
        Some(ALLIANCE),
        30_000_142,
        PILOT_SHIP,
        639,
        hub_pos,
        corp_roles::ROLE_DIRECTOR,
    )
    .await;

    // Negative: high-sec anchoring fails closed.
    assert_eq!(
        err_code(
            rpc_call(
                &ctx,
                &mut s_dir,
                "structureDeployment",
                "Anchor",
                &[PyRep::Integer(TYPE_SOVEREIGNTY_HUB as i64)],
            )
            .await
        ),
        "CannotAnchorSovHubOutsideNullSec"
    );

    // Move to unclaimed null-sec, anchor Sovereignty Hub from cargo, and complete onlining.
    let _ = ctx.systems.get_or_spawn(30_000_142).remove_ball(PILOT_SHIP).await;
    set_pilot_location(&ctx, &mut s_dir, null_sys, None);
    place_ship_ball(&ctx, null_sys, PILOT, PILOT_SHIP, 639, hub_pos).await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            988_001,
            InventoryItemRecord {
                item_id: 988_001,
                type_id: TYPE_SOVEREIGNTY_HUB,
                owner_id: PILOT,
                location_id: PILOT_SHIP,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                ..Default::default()
            },
        );
    });

    let anchor_rep = rpc_call(
        &ctx,
        &mut s_dir,
        "structureDeployment",
        "Anchor",
        &[PyRep::Integer(988_001), PyRep::String("Equinox Hub".into())],
    )
    .await
    .expect("anchor Sovereignty Hub");
    let hub_id = anchor_rep.dict_get("structureID").and_then(|v| v.as_i64()).unwrap();
    assert_eq!(
        ctx.db.with_snapshot(|s| s.system_states.get(&null_sys).and_then(|st| st.sov_alliance_id)),
        None
    );

    let anchor_end = ctx.db.with_snapshot(|s| s.structures.get(&hub_id).unwrap().reinforced_until_unix);
    ctx.set_clock(std::sync::Arc::new(eve_db::VirtualServerClock::with_fixed_base(anchor_end + 1)));
    life::sync_all_structures(&ctx);
    assert_eq!(
        ctx.db.with_snapshot(|s| s.system_states.get(&null_sys).and_then(|st| st.sov_alliance_id)),
        Some(ALLIANCE)
    );

    // Fuel Access Group via SetSovHubFuelAccessGroup.
    assert_eq!(
        rpc_call(&ctx, &mut s_dir, "sovMgr", "SetSovHubFuelAccessGroup", &[PyRep::Integer(hub_id), PyRep::Integer(55_001)])
            .await
            .expect("SetSovHubFuelAccessGroup"),
        PyRep::Integer(55_001)
    );
    assert_eq!(
        rpc_call(&ctx, &mut s_dir, "sovMgr", "IsOnLocalSovHubFuelAccessGroup", &[PyRep::Integer(hub_id)])
            .await
            .unwrap(),
        PyRep::Bool(true)
    );

    // Anchor Skyhooks on all planets so system power/workforce are harvested, and verify star power.
    let expected_star_power: u64 = ctx
        .sde
        .celestials_in_system(null_sys)
        .into_iter()
        .filter(|c| c.group_id == 6)
        .map(|c| star_power(&ctx.sde, c.type_id))
        .sum();
    ctx.db.with_mut_snapshot(|snap| {
        for (idx, cel) in ctx.sde.celestials_in_system(null_sys).into_iter().filter(|c| c.group_id == 7).enumerate() {
            let sh_id = 1_098_000_100 + idx as i64;
            snap.structures.insert(sh_id, structure(sh_id, CORP, null_sys, TYPE_ORBITAL_SKYHOOK));
            snap.sov_runtime.skyhook_planets.insert(sh_id, cel.celestial_id);
        }
        for (iid, tid, qty) in [
            (988_010, TYPE_SUPERIONIC_ICE, 100),
            (988_011, 87_703, 1), // Power Monitoring Division 1 (+200 power, 2,500 wf, 8 Ice/hr)
            (988_012, UPGRADE_MAJOR_THREAT_DETECTION, 1), // 400 power, 2,700 wf
            (988_013, UPGRADE_PROSPECTING_ARRAY, 1),      // 450 power, 6,400 wf
            (988_014, 87_948, 1),                         // Minor Threat Detection: 400 power, 4,000 wf (overloads power)
        ] {
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: tid,
                    owner_id: PILOT,
                    location_id: hub_id,
                    flag: InventoryFlag::Cargo,
                    quantity: qty,
                    ..Default::default()
                },
            );
        }
    });

    let mut rx = subscribe(&ctx, &mut s_dir).await;
    let base_power = ctx.db.with_snapshot(|s| compute_hub_resources(s, &ctx.sde, hub_id).unwrap().power_available);
    assert!(base_power >= expected_star_power);

    // Add 16 Superionic Ice (enough for 2 hours of Power Monitoring Division 1 at 8 Ice/hr).
    let mut add_fuel = id_req(1, 988_010);
    encode_varint_field(2, 16, &mut add_fuel);
    encode_bytes_field(3, &idm(hub_id as u64), &mut add_fuel);
    assert_eq!(rpc(&ctx, &mut s_dir, "eve_public.sovereignty.hub.fuel.api.AddRequest", add_fuel).await.status_code, STATUS_OK);

    // Configure Power Monitoring Division 1 + Major Threat Detection + 2x Prospecting Array (overloads power).
    let mut proc_cfg = Vec::new();
    for iid in [988_011u64, 988_012, 988_013, 988_014] {
        encode_bytes_field(1, &idm(iid), &mut proc_cfg);
    }
    for tid in [87_703u64, UPGRADE_MAJOR_THREAT_DETECTION as u64, UPGRADE_PROSPECTING_ARRAY as u64, 87_948u64] {
        let mut cfg = Vec::new();
        encode_bytes_field(1, &idm(tid), &mut cfg);
        encode_varint_field(2, 1, &mut cfg);
        encode_bytes_field(2, &cfg, &mut proc_cfg);
    }
    encode_bytes_field(3, &idm(hub_id as u64), &mut proc_cfg);
    assert_eq!(
        rpc(&ctx, &mut s_dir, "eve_public.sovereignty.hub.upgrade.api.ProcessConfigurationRequest", proc_cfg)
            .await
            .status_code,
        STATUS_OK
    );
    let _ = drain_quasar(&mut rx);

    let res_after = ctx.db.with_snapshot(|s| compute_hub_resources(s, &ctx.sde, hub_id).unwrap());
    assert_eq!(res_after.power_available, base_power + 200);
    assert!(res_after.power_allocated <= res_after.power_available, "power overload must take excess upgrades offline");

    // Hourly fuel burn: 2 hours burns 16 Ice, 3rd hour empties bay and offlines fuel-burning upgrade.
    let now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        let hub = snap.sov_runtime.sov_hubs.get_mut(&hub_id).unwrap();
        hub.last_updated_unix = now;
        let _ = simulate_fuel(&ctx.sde, hub, now + 7_200);
        assert_eq!(hub.fuel.get(&TYPE_SUPERIONIC_ICE).copied().unwrap_or(0), 0);
        assert!(simulate_fuel(&ctx.sde, hub, now + 10_800));
        let pwr_mon = hub.upgrades.iter().find(|u| u.type_id == 87_703).unwrap();
        assert!(!pwr_mon.online || pwr_mon.low_power, "empty fuel bay must offline fuel-consuming upgrade");
    });

    drop(ctx);
    let _ = std::fs::remove_file(&db_path);
}

#[tokio::test]
async fn test_sov_workforce_reagents_and_mercenary_den_closure() {
    // parity: feat:sovereignty.sov-workforce
    // parity: feat:sovereignty.sov-reagents
    // parity: feat:sovereignty.mercenary-den
    eve_testkit::attest(&[
        "feat:sovereignty.sov-workforce",
        "feat:sovereignty.sov-reagents",
        "feat:sovereignty.mercenary-den",
    ]);

    let db_path = temp_sqlite_path("parity_sov_wf_reagents_den");
    let ctx = open_sqlite_ctx(&db_path, true);
    crate::common::pin_clock(&ctx);
    let (null_sys, temp_planet, lava_planet, ice_planet, _) = find_nullsec_planets(&ctx);
    let mut adj_null: Vec<u32> = ctx
        .sde
        .jumps
        .values()
        .filter(|j| {
            j.from_solar_system_id == null_sys
                && ctx.sde.solar_systems.get(&j.to_solar_system_id).is_some_and(|s| {
                    s.security_status <= 0.0
                        && ctx.sde.constellations.get(&s.constellation_id).and_then(|c| c.faction_id).is_none()
                        && ctx.sde.regions.get(&s.region_id).and_then(|r| r.faction_id).is_none()
                })
        })
        .map(|j| j.to_solar_system_id)
        .filter(|&sys| {
            ctx.sde
                .celestials_in_system(sys)
                .into_iter()
                .filter(|c| c.group_id == 7)
                .map(|c| eve_server::services::quasar_sov_hub::planet_yield(c.type_id).1)
                .sum::<u64>()
                >= 2_000
        })
        .collect();
    adj_null.sort_unstable();
    let neighbor_sys = adj_null.into_iter().next().expect("adjacent null-sec system with >= 2000 workforce");

    let cels = ctx.sde.celestials_in_system(null_sys);
    let lava_pos = cels.iter().find(|c| c.celestial_id == lava_planet).unwrap().position;
    let ice_pos = cels.iter().find(|c| c.celestial_id == ice_planet).unwrap().position;
    let temp_pos = cels.iter().find(|c| c.celestial_id == temp_planet).unwrap().position;
    let (hub_a, hub_b, sh_temp) = (1_099_000_001i64, 1_099_000_002i64, 1_099_000_003i64);

    let mut s_dir = register_online_pilot(
        &ctx,
        88_002,
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
        snap.structures.insert(sh_temp, structure(sh_temp, CORP, null_sys, TYPE_ORBITAL_SKYHOOK));
        snap.sov_runtime.skyhook_planets.insert(sh_temp, temp_planet);
        for (idx, cel) in ctx.sde.celestials_in_system(neighbor_sys).into_iter().filter(|c| c.group_id == 7).enumerate() {
            let sh_id = 1_099_000_200 + idx as i64;
            snap.structures.insert(sh_id, structure(sh_id, CORP, neighbor_sys, TYPE_ORBITAL_SKYHOOK));
            snap.sov_runtime.skyhook_planets.insert(sh_id, cel.celestial_id);
        }
        snap.skills.entry(PILOT).or_default().insert(
            TYPE_MERCENARY_DEN_MANAGEMENT,
            TrainedSkillRecord { type_id: TYPE_MERCENARY_DEN_MANAGEMENT, level: 2, skill_points: 50_000.0 },
        );
        for (iid, tid, qty) in [
            (988_101, TYPE_ORBITAL_SKYHOOK, 2),
            (988_102, TYPE_MERCENARY_DEN, 1),
        ] {
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: tid,
                    owner_id: PILOT,
                    location_id: PILOT_SHIP,
                    flag: InventoryFlag::Cargo,
                    quantity: qty,
                    ..Default::default()
                },
            );
        }
    });
    claim_system_sovereignty(&ctx, null_sys, ALLIANCE, CORP, hub_a).expect("claim null_sys");
    claim_system_sovereignty(&ctx, neighbor_sys, ALLIANCE, CORP, hub_b).expect("claim neighbor_sys");
    let _ = ctx.systems.get_or_spawn(null_sys).spawn_ball(make_ball(sh_temp, TYPE_ORBITAL_SKYHOOK, temp_pos)).await;

    // 1. Anchor Skyhooks on Lava & Ice planets and verify Magmatic Gas / Superionic Ice accumulation.
    let sh_lava = rpc_call(
        &ctx,
        &mut s_dir,
        "structureDeployment",
        "Anchor",
        &[PyRep::Integer(TYPE_ORBITAL_SKYHOOK as i64), PyRep::Integer(lava_planet)],
    )
    .await
    .expect("anchor Lava Skyhook")
    .dict_get("structureID")
    .and_then(|v| v.as_i64())
    .unwrap();

    let _ = ctx.systems.get_or_spawn(null_sys).remove_ball(PILOT_SHIP).await;
    place_ship_ball(&ctx, null_sys, PILOT, PILOT_SHIP, 19_722, ice_pos).await;
    let sh_ice = rpc_call(
        &ctx,
        &mut s_dir,
        "structureDeployment",
        "Anchor",
        &[PyRep::Integer(TYPE_ORBITAL_SKYHOOK as i64), PyRep::Integer(ice_planet)],
    )
    .await
    .expect("anchor Ice Skyhook")
    .dict_get("structureID")
    .and_then(|v| v.as_i64())
    .unwrap();

    let _ = ctx.systems.get_or_spawn(null_sys).remove_ball(PILOT_SHIP).await;
    place_ship_ball(&ctx, null_sys, PILOT, PILOT_SHIP, 19_722, DVec3::new(200_000.0, 0.0, 0.0)).await;
    let _ = rpc_call(&ctx, &mut s_dir, "sovMgr", "AcquireSkyhooks", &[PyRep::Integer(null_sys as i64)])
        .await
        .expect("AcquireSkyhooks");

    let ready_unix = ctx.db.with_snapshot(|s| s.structures.get(&sh_ice).unwrap().reinforced_until_unix);
    let now_unix = ctx.now_unix();
    if ready_unix + 7_200 > now_unix {
        ctx.advance_secs(ready_unix + 7_200 - now_unix);
    } else {
        ctx.advance_secs(7_200);
    }
    life::sync_all_structures(&ctx);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;

    let (gas_qty, ice_qty) = ctx.db.with_snapshot(|s| {
        let g = s.sov_runtime.skyhook_reagent_bays.get(&sh_lava).map(|b| b.1 + b.2).unwrap_or(0);
        let i = s.sov_runtime.skyhook_reagent_bays.get(&sh_ice).map(|b| b.1 + b.2).unwrap_or(0);
        (g, i)
    });
    assert!(gas_qty > 0 && ice_qty > 0, "Lava ({gas_qty}) and Ice ({ice_qty}) Skyhooks must accumulate reagents");

    // 2. Cross-system Workforce export/import via hub.workforce.api.ConfigureRequest.
    let wf_before = varint(
        &field(
            &rpc(&ctx, &mut s_dir, "eve_public.sovereignty.hub.api.GetResourcesRequest", id_req(1, hub_a as u64))
                .await
                .payload
                .unwrap()
                .value,
            2,
        )
        .unwrap(),
        1,
    )
    .unwrap_or(0);

    let (mut exp, mut exp_cfg, mut cfg_b) = (Vec::new(), Vec::new(), id_req(1, hub_b as u64));
    encode_bytes_field(3, &idm(null_sys as u64), &mut exp);
    encode_varint_field(2, 1_500, &mut exp);
    encode_bytes_field(3, &exp, &mut exp_cfg);
    encode_bytes_field(2, &exp_cfg, &mut cfg_b);
    assert_eq!(rpc(&ctx, &mut s_dir, "eve_public.sovereignty.hub.workforce.api.ConfigureRequest", cfg_b).await.status_code, STATUS_OK);

    let (mut src, mut imp, mut imp_cfg, mut cfg_a) = (Vec::new(), Vec::new(), Vec::new(), id_req(1, hub_a as u64));
    encode_bytes_field(1, &idm(neighbor_sys as u64), &mut src);
    encode_bytes_field(1, &src, &mut imp);
    encode_bytes_field(2, &imp, &mut imp_cfg);
    encode_bytes_field(2, &imp_cfg, &mut cfg_a);
    assert_eq!(rpc(&ctx, &mut s_dir, "eve_public.sovereignty.hub.workforce.api.ConfigureRequest", cfg_a).await.status_code, STATUS_OK);

    let wf_after = varint(
        &field(
            &rpc(&ctx, &mut s_dir, "eve_public.sovereignty.hub.api.GetResourcesRequest", id_req(1, hub_a as u64))
                .await
                .payload
                .unwrap()
                .value,
            2,
        )
        .unwrap(),
        1,
    )
    .unwrap_or(0);
    assert_eq!(wf_after, wf_before + 1_500);

    // 3. Anchor Mercenary Den on Temperate Skyhook, generate Infomorphs, and attack it.
    let _ = ctx.systems.get_or_spawn(null_sys).remove_ball(PILOT_SHIP).await;
    place_ship_ball(&ctx, null_sys, PILOT, PILOT_SHIP, 19_722, temp_pos + DVec3::new(3_000.0, 0.0, 0.0)).await;
    let den_id = rpc_call(
        &ctx,
        &mut s_dir,
        "structureDeployment",
        "Anchor",
        &[PyRep::Integer(TYPE_MERCENARY_DEN as i64), PyRep::Integer(sh_temp), PyRep::Integer(temp_planet)],
    )
    .await
    .expect("anchor Mercenary Den")
    .dict_get("structureID")
    .and_then(|v| v.as_i64())
    .unwrap();

    let den_ready = ctx.db.with_snapshot(|s| s.structures.get(&den_id).unwrap().reinforced_until_unix);
    let now_u = ctx.now_unix();
    if den_ready + 3_600 > now_u {
        ctx.advance_secs(den_ready + 3_600 - now_u);
    } else {
        ctx.advance_secs(3_600);
    }
    life::sync_all_structures(&ctx);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;

    let idks = ctx.db.with_snapshot(|s| {
        s.sov_runtime
            .mercenary_dens
            .get(&den_id)
            .map(|d| d.infomorphs)
            .unwrap_or(0)
    });
    assert!(idks > 0, "Mercenary Den must generate Infomorph Decryption Keys over time");

    // Attack Mercenary Den with blasters and verify shield HP drops.
    let blaster_base = 988_900i64;
    fit_subcap_blasters(&ctx, PILOT, PILOT_SHIP, blaster_base);
    lock_target_via_rpc(&ctx, &mut s_dir, null_sys, PILOT_SHIP, den_id).await;
    let shield_before = ctx.db.with_snapshot(|s| s.structures.get(&den_id).unwrap().shield_hp);
    let _ = rpc_call(
        &ctx,
        &mut s_dir,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(blaster_base), PyRep::String("effects.ProjectileFired".into()), PyRep::Integer(den_id)],
    )
    .await;
    step_weapon_ticks(&ctx, null_sys).await;
    let shield_after = ctx.db.with_snapshot(|s| s.structures.get(&den_id).unwrap().shield_hp);
    assert!(shield_after < shield_before, "attacking Mercenary Den must reduce its shield HP");
    let _ = drain_quasar(&mut rx);

    drop(ctx);
    let _ = std::fs::remove_file(&db_path);
}
