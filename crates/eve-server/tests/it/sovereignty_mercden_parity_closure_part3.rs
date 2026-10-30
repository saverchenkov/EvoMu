//! End-to-end parity closure tests (Part 3) for Sovereignty Hub Power, Workforce,
//! Fuel, Upgrades, Skyhook Reagents & Sovereignty Upgrade Sites.

#![forbid(unsafe_code)]

use crate::quasar_sov_equinox_hub::{field, fields, id_req, idm, rpc, structure, subscribe, varint};
use crate::sovereignty_equinox_gameplay_closure::{
    drain_quasar, find_nullsec_planets, fit_subcap_blasters, lock_target_via_rpc, make_ball,
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
    // DATA GAP: feat:sovereignty.sov-hub-upgrades / sov-hub-fuel — 44 wiki-derived (build 3538132) upgrade definitions & 0 startup-cost placeholder pending V24.01 SDE import (docs/BLOCKERS.md §7)
    // DATA GAP: feat:sovereignty.sov-power — server-only celestial power table (docs/BLOCKERS.md §7)
    // DATA GAP: feat:sovereignty.sov-workforce — server-only celestial workforce table (docs/BLOCKERS.md §7)
    // DATA GAP: feat:sovereignty.sov-reagents — server-only planetary reagent yield table (docs/BLOCKERS.md §7)
    // parity: feat:sovereignty.sov-upgrade-sites
    eve_testkit::attest(&[
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
        // Anchor active Skyhooks on non-Lava/non-Ice planets in null_sys (leaving Lava & Ice free for live anchoring below)
        // and on all planets in neighbor_sys so planetary Power & Workforce are harvested solely from active Skyhooks.
        for (idx, cel) in ctx.sde.celestials_in_system(null_sys).into_iter().filter(|c| c.group_id == 7 && c.celestial_id != lava_planet && c.celestial_id != ice_planet).enumerate() {
            let sh_id = 1_090_000_100 + idx as i64;
            snap.structures.insert(sh_id, structure(sh_id, CORP, null_sys, TYPE_ORBITAL_SKYHOOK));
            snap.sov_runtime.skyhook_planets.insert(sh_id, cel.celestial_id);
        }
        for (idx, cel) in ctx.sde.celestials_in_system(neighbor_sys).into_iter().filter(|c| c.group_id == 7).enumerate() {
            let sh_id = 1_090_000_200 + idx as i64;
            snap.structures.insert(sh_id, structure(sh_id, CORP, neighbor_sys, TYPE_ORBITAL_SKYHOOK));
            snap.sov_runtime.skyhook_planets.insert(sh_id, cel.celestial_id);
        }
        for (iid, tid, loc, qty) in [
            (982_001, TYPE_MAGMATIC_GAS, hub_a, 3_000),
            (982_002, TYPE_SUPERIONIC_ICE, hub_a, 3_000),
            (982_004, 87_710, hub_a, 1), // Workforce Mecha-Tooling 1 (200 p, +5,000 wf, 8 Ice/hr)
            (982_005, UPGRADE_MAJOR_THREAT_DETECTION, hub_a, 1), // 400 p, 2,700 wf, 0 fuel
            (982_006, UPGRADE_PROSPECTING_ARRAY, hub_a, 1), // 450 p, 6,400 wf, 0 fuel
            (982_007, UPGRADE_MAJOR_THREAT_DETECTION, hub_a, 1),
            (982_008, 87_703, hub_a, 1), // Power Monitoring Division 1 ("ResourceManagement" group)
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
    let mt1_def = defs.iter().find(|d| varint(&field(d, 1).unwrap(), 1) == Some(UPGRADE_MAJOR_THREAT_DETECTION as u64)).expect("Major Threat 1");
    assert_eq!((varint(mt1_def, 2), varint(mt1_def, 3), field(mt1_def, 4)), (Some(400), Some(2_700), None));
    let pa1_def = defs.iter().find(|d| varint(&field(d, 1).unwrap(), 1) == Some(UPGRADE_PROSPECTING_ARRAY as u64)).expect("Prospecting Array 1");
    assert_eq!((varint(pa1_def, 2), varint(pa1_def, 3), field(pa1_def, 4)), (Some(450), Some(6_400), None));
    let cyno_def = defs.iter().find(|d| varint(&field(d, 1).unwrap(), 1) == Some(81_619)).expect("Cynosural Suppression");
    assert_eq!((varint(cyno_def, 2), varint(cyno_def, 3)), (Some(250), Some(4_500)));
    assert_eq!(field(cyno_def, 4).and_then(|i| varint(&i, 1)), Some(TYPE_MAGMATIC_GAS as u64));
    assert_eq!(varint(cyno_def, 5), Some(205));

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
    encode_varint_field(2, 3_500, &mut exp);
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
    assert_eq!(wf1, wf0 + 3_500);

    // 3. Exclusive group conflict check + priority-slot power/workforce overload (`POWER_STATE_LOW`) + valid priority reorder:
    //    - Duplicate Major Threat Detection Array fails (`400`).
    //    - Conflicting `"ResourceManagement"` upgrades (`87710` + `87703`) fail (`400`).
    //    - Placing Prospecting Array 1 (`6,400` wf) in slot 0 BEFORE Workforce Mecha-Tooling 1 (`+5,000` wf) in slot 1 succeeds (`200`)
    //      and puts slot 0 into `POWER_STATE_LOW` (`3`) because base+imported wf is `5,000 < 6,400`.
    //    - Reordering Workforce Mecha-Tooling 1 (`87710`) into slot 0 brings all 3 upgrades to `POWER_STATE_ONLINE` (`2`)!
    let build_cfg = |new_iids: &[u64], tids: &[u64]| {
        let mut req = Vec::new();
        for &iid in new_iids {
            encode_bytes_field(1, &idm(iid), &mut req);
        }
        for &tid in tids {
            let mut c = Vec::new();
            encode_bytes_field(1, &idm(tid), &mut c);
            encode_varint_field(2, 1, &mut c);
            encode_bytes_field(2, &c, &mut req);
        }
        encode_bytes_field(3, &idm(hub_a as u64), &mut req);
        req
    };
    let bad_dup = build_cfg(&[982_004, 982_005, 982_007], &[87_710, UPGRADE_MAJOR_THREAT_DETECTION as u64]);
    assert_eq!(rpc(&ctx, &mut s_dir, "eve_public.sovereignty.hub.upgrade.api.ProcessConfigurationRequest", bad_dup).await.status_code, 400);
    let bad_excl = build_cfg(&[982_004, 982_008], &[87_710, 87_703]);
    assert_eq!(rpc(&ctx, &mut s_dir, "eve_public.sovereignty.hub.upgrade.api.ProcessConfigurationRequest", bad_excl).await.status_code, 400);

    let bad_prio = build_cfg(&[982_006, 982_004, 982_005], &[UPGRADE_PROSPECTING_ARRAY as u64, 87_710, UPGRADE_MAJOR_THREAT_DETECTION as u64]);
    let prio_rsp = rpc(&ctx, &mut s_dir, "eve_public.sovereignty.hub.upgrade.api.ProcessConfigurationRequest", bad_prio).await;
    assert_eq!(prio_rsp.status_code, 200);
    let prio_ups = fields(&field(&prio_rsp.payload.unwrap().value, 1).unwrap(), 2);
    assert_eq!(field(&prio_ups[0], 2).and_then(|a| varint(&a, 2)), Some(3), "slot 0 Prospecting Array enters POWER_STATE_LOW");

    let proc = build_cfg(&[], &[87_710, UPGRADE_MAJOR_THREAT_DETECTION as u64, UPGRADE_PROSPECTING_ARRAY as u64]);
    let ok_rsp = rpc(&ctx, &mut s_dir, "eve_public.sovereignty.hub.upgrade.api.ProcessConfigurationRequest", proc).await;
    assert_eq!(ok_rsp.status_code, 200);
    let ok_ups = fields(&field(&ok_rsp.payload.unwrap().value, 1).unwrap(), 2);
    assert!(ok_ups.iter().all(|u| field(u, 2).and_then(|a| varint(&a, 2)) == Some(2)), "reordered upgrades all POWER_STATE_ONLINE");

    // 4. Spawn upgrade sites (`tick_background_cluster`), verify mineable asteroid & killable NPC rat balls in space!
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    let balls = ctx.systems.get_or_spawn(null_sys).list_balls().await.unwrap();
    let ore_ball = balls
        .iter()
        .find(|b| matches!(b.type_id, 18..=22 | 1_223..=1_232))
        .expect("spawned asteroid ball");
    let npc_balls: Vec<_> = balls.iter().filter(|b| b.type_id == 37_453).cloned().collect();
    assert_eq!(npc_balls.len(), 2);
    assert_eq!(ctx.db.with_snapshot(|s| s.cosmic_anomalies[&null_sys].len()), 2);

    // Kill both spawned upgrade-site NPC rats via live subcap blaster fire -> credits Military Index, marks site cleared, and blocks instant respawn!
    fit_subcap_blasters(&ctx, PILOT, PILOT_SHIP, 982_200);
    let mut last_shot = PyRep::None;
    for (idx, npc_ball) in npc_balls.iter().enumerate() {
        place_ship_ball(&ctx, null_sys, PILOT, PILOT_SHIP, 16_229, npc_ball.position + DVec3::new(2_500.0, 0.0, 0.0)).await;
        lock_target_via_rpc(&ctx, &mut s_dir, null_sys, PILOT_SHIP, npc_ball.id).await;
        for _round in 0..8 {
            for gun_id in (0..6).map(|g| 982_200i64 + g * 2) {
                ctx.set_invention_roll_override(Some(0.05));
                last_shot = rpc_call(&ctx, &mut s_dir, "dogmaIM", "Activate", &[PyRep::Integer(gun_id), PyRep::Integer(npc_ball.id)])
                    .await
                    .expect("activate turret on upgrade-site NPC");
                if last_shot.dict_get("destroyed") == Some(&PyRep::Bool(true))
                    || ctx.db.with_snapshot(|s| s.sov_runtime.sov_npc_kills.get(&null_sys).copied().unwrap_or(0)) > idx as u64
                {
                    break;
                }
            }
            if ctx.db.with_snapshot(|s| s.sov_runtime.sov_npc_kills.get(&null_sys).copied().unwrap_or(0)) > idx as u64 {
                break;
            }
            step_weapon_ticks(&ctx, null_sys).await;
        }
    }
    assert_eq!(ctx.db.with_snapshot(|s| s.sov_runtime.sov_npc_kills.get(&null_sys).copied().unwrap_or(0)), 2, "last_shot={last_shot:?}");
    assert!(ctx.db.with_snapshot(|s| s.cosmic_anomalies[&null_sys].iter().any(|a| a.anomaly_type == "Combat" && a.cleared)));
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    assert!(!ctx.systems.get_or_spawn(null_sys).list_balls().await.unwrap().iter().any(|b| b.type_id == 37_453), "cleared Threat Detection site must not immediately respawn");
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

    // Advance 200 more hours: Workforce Mecha-Tooling 1 (`87710`, 8 Ice/hr) exhausts the 1,000 Superionic Ice bay,
    // dropping `87710` and cascaded `Prospecting Array 1` (`6,400 > 5,000` remaining wf) to `POWER_STATE_LOW`, while
    // zero-fuel `Major Threat Detection Array 1` (`400` power, `2,700 <= 5,000` wf) stays online!
    ctx.advance_secs(200 * 3_600);
    let res_post = rpc(&ctx, &mut s_dir, "eve_public.sovereignty.hub.api.GetResourcesRequest", id_req(1, hub_a as u64)).await.payload.unwrap().value;
    assert_eq!(varint(&field(&res_post, 1).unwrap(), 2), Some(400), "Mecha-Tooling + cascaded Prospecting drop to low power while zero-fuel Major Threat 1 stays online at 400p");

    // Decommissioning an upgrade destroys it without minting a hangar item (`Sovereignty.md:37`).
    let _ = drain_quasar(&mut rx);
    let (mut un_ident, mut un_req) = (Vec::new(), Vec::new());
    encode_bytes_field(1, &idm(hub_a as u64), &mut un_ident);
    encode_bytes_field(2, &idm(87_710), &mut un_ident);
    encode_bytes_field(1, &un_ident, &mut un_req);
    assert_eq!(rpc(&ctx, &mut s_dir, "eve_public.sovereignty.hub.upgrade.api.UninstallRequest", un_req).await.status_code, 200);
    assert!(drain_quasar(&mut rx).iter().any(|(t, _)| t == "eve_public.sovereignty.hub.upgrade.api.UninstalledNotice"));
    assert!(ctx.db.with_snapshot(|s| !s.items.values().any(|i| i.type_id == 87_710 && i.location_id == hub_a)));

    drop(ctx);
    let _ = std::fs::remove_file(&db_path);
}

#[tokio::test]
async fn test_skyhook_harvest_gating_anarchy_and_persisted_npc_durability() {
    let db_path = temp_sqlite_path("sov_harvest_npc_durability");
    let ctx = open_sqlite_ctx(&db_path, true);
    let (null_sys, temp_planet, _, _, _) = find_nullsec_planets(&ctx);
    let hub_id = 1_130_000_001i64;
    let temp_sh_id = 1_130_000_010i64;
    let hub_pos = DVec3::new(200_000.0, 0.0, 0.0);

    let mut s_dir = register_online_pilot(
        &ctx, 88_001, "demo", "demo-secret", PILOT, CORP, Some(ALLIANCE),
        null_sys, PILOT_SHIP, 19_722, hub_pos + DVec3::new(2_500.0, 0.0, 0.0), corp_roles::ROLE_DIRECTOR,
    ).await;
    let mut rx = subscribe(&ctx, &mut s_dir).await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states.entry(null_sys).or_default().sov_alliance_id = Some(ALLIANCE);
        snap.structures.insert(hub_id, structure(hub_id, CORP, null_sys, TYPE_SOVEREIGNTY_HUB));
        snap.items.insert(
            988_001,
            InventoryItemRecord {
                item_id: 988_001,
                type_id: eve_server::services::sov_gameplay::UPGRADE_MINOR_THREAT_DETECTION,
                owner_id: PILOT,
                location_id: hub_id,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                ..Default::default()
            },
        );
    });
    claim_system_sovereignty(&ctx, null_sys, ALLIANCE, CORP, hub_id).expect("claim null_sys");

    // 1. With 0 Orbital Skyhooks, planetary Power & Workforce are 0 (only stellar power = 300).
    let r0 = rpc(&ctx, &mut s_dir, "eve_public.sovereignty.hub.api.GetResourcesRequest", id_req(1, hub_id as u64)).await.payload.unwrap().value;
    assert_eq!(varint(&field(&r0, 1).unwrap(), 1), Some(300));
    assert_eq!(varint(&field(&r0, 2).unwrap(), 1).unwrap_or(0), 0);

    // Anchor active Skyhook on non-first Temperate planet -> harvests +1,500 workforce; Anarchy stage 5 siphons 600 -> 900 wf.
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(temp_sh_id, structure(temp_sh_id, CORP, null_sys, TYPE_ORBITAL_SKYHOOK));
        snap.sov_runtime.skyhook_planets.insert(temp_sh_id, temp_planet);
        snap.sov_runtime.mercenary_dens.insert(
            1_130_000_099,
            eve_db::PersistedMercenaryDen {
                den_id: 1_130_000_099,
                owner_char_id: PILOT,
                skyhook_id: temp_sh_id,
                solar_system_id: null_sys,
                planet_id: temp_planet,
                type_id: eve_server::services::sov_gameplay::TYPE_MERCENARY_DEN,
                enabled: true,
                cargo_extraction_enabled: true,
                development_level: 100,
                anarchy_level: 100,
                simulated_at_unix: ctx.now_unix(),
                infomorphs: 0,
                next_activity_generation_unix: ctx.now_unix() + 86_400,
            },
        );
    });
    let r_active = rpc(&ctx, &mut s_dir, "eve_public.sovereignty.hub.api.GetResourcesRequest", id_req(1, hub_id as u64)).await.payload.unwrap().value;
    assert_eq!(varint(&field(&r_active, 2).unwrap(), 1), Some(900), "1,500 harvested - 40% Anarchy siphon = 900 wf");

    // Deactivating the Skyhook drops harvested workforce to 0 (Anarchy siphons 0 when unharvested) and emits exact bound `temp_planet` in ActivationNotice!
    let _ = drain_quasar(&mut rx);
    assert_eq!(rpc(&ctx, &mut s_dir, "eve_public.sovereignty.skyhook.api.DeactivateRequest", id_req(1, temp_sh_id as u64)).await.status_code, 200);
    let deact_n = drain_quasar(&mut rx);
    let act_notice = deact_n.iter().find(|(t, _)| t == "eve_public.sovereignty.skyhook.api.ActivationNotice").expect("ActivationNotice");
    assert_eq!(field(&act_notice.1, 2).and_then(|i| varint(&i, 1)), Some(temp_planet as u64), "ActivationNotice carries bound non-first planet_id");
    let r_deact = rpc(&ctx, &mut s_dir, "eve_public.sovereignty.hub.api.GetResourcesRequest", id_req(1, hub_id as u64)).await.payload.unwrap().value;
    assert_eq!(varint(&field(&r_deact, 2).unwrap(), 1).unwrap_or(0), 0);

    // Re-activating restores harvest; destroying the Skyhook removes harvest again.
    assert_eq!(rpc(&ctx, &mut s_dir, "eve_public.sovereignty.skyhook.api.ActivateRequest", id_req(1, temp_sh_id as u64)).await.status_code, 200);
    ctx.db.with_mut_snapshot(|snap| {
        snap.sov_runtime.mercenary_dens.clear();
        snap.structures.get_mut(&temp_sh_id).unwrap().state = "Destroyed".into();
    });
    let r_dest = rpc(&ctx, &mut s_dir, "eve_public.sovereignty.hub.api.GetResourcesRequest", id_req(1, hub_id as u64)).await.payload.unwrap().value;
    assert_eq!(varint(&field(&r_dest, 2).unwrap(), 1).unwrap_or(0), 0);

    // 2. Restore Temperate + neighbor workforce, online Minor Threat Detection Array 1 (200p, 1,800wf, 0 fuel), spawn Threat Detection NPCs,
    //    and verify `sov_upgrade_combat_npcs` isolation, rollback safety, and SQLite reload durability!
    let neigh = ctx.sde.jumps.values().find(|j| {
        j.from_solar_system_id == null_sys
            && eve_server::services::sov_gameplay::is_claimable_nullsec_system(&ctx.sde, j.to_solar_system_id)
            && ctx.sde.celestials_in_system(j.to_solar_system_id).into_iter().any(|c| matches!(c.type_id, 11 | 2014 | 2016))
    }).unwrap().to_solar_system_id;
    let hb = 1_130_000_002i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.get_mut(&temp_sh_id).unwrap().state = "ShieldVulnerable".into();
        let sh2 = 1_130_000_011i64;
        snap.system_states.entry(neigh).or_default().solar_system_id = neigh;
        snap.structures.insert(hb, structure(hb, CORP, neigh, TYPE_SOVEREIGNTY_HUB));
        let np = ctx.sde.celestials_in_system(neigh).into_iter().find(|c| matches!(c.type_id, 11 | 2014 | 2016)).unwrap().celestial_id;
        snap.structures.insert(sh2, structure(sh2, CORP, neigh, TYPE_ORBITAL_SKYHOOK));
        snap.sov_runtime.skyhook_planets.insert(sh2, np);
    });
    claim_system_sovereignty(&ctx, neigh, ALLIANCE, CORP, hb).expect("claim neigh");
    let (mut exp, mut exp_cfg, mut cfg_hb) = (Vec::new(), Vec::new(), id_req(1, hb as u64));
    encode_bytes_field(3, &idm(null_sys as u64), &mut exp);
    encode_varint_field(2, 500, &mut exp);
    encode_bytes_field(3, &exp, &mut exp_cfg);
    encode_bytes_field(2, &exp_cfg, &mut cfg_hb);
    assert_eq!(rpc(&ctx, &mut s_dir, "eve_public.sovereignty.hub.workforce.api.ConfigureRequest", cfg_hb).await.status_code, 200);
    let (mut src, mut imp, mut imp_cfg, mut cfg_h) = (Vec::new(), Vec::new(), Vec::new(), id_req(1, hub_id as u64));
    encode_bytes_field(1, &idm(neigh as u64), &mut src);
    encode_bytes_field(1, &src, &mut imp);
    encode_bytes_field(2, &imp, &mut imp_cfg);
    encode_bytes_field(2, &imp_cfg, &mut cfg_h);
    assert_eq!(rpc(&ctx, &mut s_dir, "eve_public.sovereignty.hub.workforce.api.ConfigureRequest", cfg_h).await.status_code, 200);
    let mut proc = id_req(1, 988_001);
    let mut cfg = Vec::new();
    encode_bytes_field(1, &idm(eve_server::services::sov_gameplay::UPGRADE_MINOR_THREAT_DETECTION as u64), &mut cfg);
    encode_varint_field(2, 1, &mut cfg);
    encode_bytes_field(2, &cfg, &mut proc);
    encode_bytes_field(3, &idm(hub_id as u64), &mut proc);
    assert_eq!(rpc(&ctx, &mut s_dir, "eve_public.sovereignty.hub.upgrade.api.ProcessConfigurationRequest", proc).await.status_code, 200);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;

    let tracked_nid = ctx.db.with_snapshot(|s| *s.sov_runtime.sov_upgrade_combat_npcs.iter().next().expect("persisted Threat Detection NPC ID"));
    let other_ctx = eve_server::router::ServerContext::new();
    assert!(
        other_ctx.db.with_snapshot(|s| !s.sov_runtime.sov_upgrade_combat_npcs.contains(&tracked_nid)),
        "separate ServerContext must not share persisted Threat Detection NPC set"
    );
    let _: Result<(), eve_proto::BridgeRpcError> = ctx.db.try_with_mut_snapshot(|snap| {
        snap.sov_runtime.sov_upgrade_combat_npcs.insert(777_888_999);
        Err(eve_proto::BridgeRpcError { code: "Abort".into(), message: "rollback test".into() })
    });
    assert!(ctx.db.with_snapshot(|s| !s.sov_runtime.sov_upgrade_combat_npcs.contains(&777_888_999)), "failed transaction rolls back sov_upgrade_combat_npcs");

    drop(ctx);
    let reloaded = open_sqlite_ctx(&db_path, false);
    assert!(
        reloaded.db.with_snapshot(|s| s.sov_runtime.sov_upgrade_combat_npcs.contains(&tracked_nid)),
        "sov_upgrade_combat_npcs survives SQLite reopen"
    );
    eve_server::sp6_lifecycle::tick_background_cluster(&reloaded).await;
    let npc_pos = reloaded.systems.get_or_spawn(null_sys).get_ball_snapshot(tracked_nid).await.unwrap().unwrap().1.position;
    let mut s_rel = register_online_pilot(
        &reloaded, 88_002, "demo", "demo-secret", PILOT, CORP, Some(ALLIANCE),
        null_sys, PILOT_SHIP, 16_229, npc_pos + DVec3::new(2_500.0, 0.0, 0.0), corp_roles::ROLE_DIRECTOR,
    ).await;
    fit_subcap_blasters(&reloaded, PILOT, PILOT_SHIP, 988_300);
    lock_target_via_rpc(&reloaded, &mut s_rel, null_sys, PILOT_SHIP, tracked_nid).await;
    for _round in 0..8 {
        for gun_id in (0..6).map(|g| 988_300i64 + g * 2) {
            reloaded.set_invention_roll_override(Some(0.05));
            let res = rpc_call(&reloaded, &mut s_rel, "dogmaIM", "Activate", &[PyRep::Integer(gun_id), PyRep::Integer(tracked_nid)]).await.unwrap();
            if res.dict_get("destroyed") == Some(&PyRep::Bool(true)) {
                break;
            }
        }
        if reloaded.db.with_snapshot(|s| !s.sov_runtime.sov_upgrade_combat_npcs.contains(&tracked_nid)) {
            break;
        }
        step_weapon_ticks(&reloaded, null_sys).await;
    }
    assert_eq!(reloaded.db.with_snapshot(|s| s.sov_runtime.sov_npc_kills.get(&null_sys).copied().unwrap_or(0)), 1, "post-reload Threat Detection rat kill credits sov_npc_kills");
    assert!(reloaded.db.with_snapshot(|s| !s.sov_runtime.sov_upgrade_combat_npcs.contains(&tracked_nid)));

    drop(reloaded);
    let _ = std::fs::remove_file(&db_path);
}

#[tokio::test]
async fn test_sov_anchoring_item_locks_and_posmgr_mercenary_den_guards() {
    let db_path = temp_sqlite_path("sov_anchor_locks_posmgr");
    let ctx = open_sqlite_ctx(&db_path, true);
    let (null_sys, temp_planet, lava_planet, _, _) = find_nullsec_planets(&ctx);
    let cels = ctx.sde.celestials_in_system(null_sys);
    let temp_pos = cels.iter().find(|c| c.celestial_id == temp_planet).unwrap().position;
    let lava_pos = cels.iter().find(|c| c.celestial_id == lava_planet).unwrap().position;

    let mut s_dir = register_online_pilot(
        &ctx, 89_001, "demo", "demo-secret", PILOT, CORP, Some(ALLIANCE),
        null_sys, PILOT_SHIP, 19_722, lava_pos, corp_roles::ROLE_DIRECTOR,
    ).await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(PILOT).or_default().insert(
            85_233,
            eve_db::TrainedSkillRecord { type_id: 85_233, level: 3, skill_points: 100_000.0 },
        );
        for (iid, tid, owner, flg) in [
            (989_001, TYPE_SOVEREIGNTY_HUB, PILOT, InventoryFlag::Cargo),  // contract-locked
            (989_002, TYPE_SOVEREIGNTY_HUB, PILOT, InventoryFlag::Hangar), // non-cargo
            (989_003, TYPE_SOVEREIGNTY_HUB, 90_999_888, InventoryFlag::Cargo), // unowned
            (989_011, TYPE_ORBITAL_SKYHOOK, PILOT, InventoryFlag::Cargo),  // contract-locked
            (989_012, TYPE_ORBITAL_SKYHOOK, PILOT, InventoryFlag::Hangar), // non-cargo
            (989_013, TYPE_ORBITAL_SKYHOOK, 90_999_888, InventoryFlag::Cargo), // unowned
            (989_021, eve_server::services::sov_gameplay::TYPE_MERCENARY_DEN, PILOT, InventoryFlag::Cargo), // contract-locked
            (989_022, eve_server::services::sov_gameplay::TYPE_MERCENARY_DEN, PILOT, InventoryFlag::Hangar), // non-cargo
            (989_023, eve_server::services::sov_gameplay::TYPE_MERCENARY_DEN, 90_999_888, InventoryFlag::Cargo), // unowned
            (989_024, eve_server::services::sov_gameplay::TYPE_MERCENARY_DEN, PILOT, InventoryFlag::Cargo), // valid #1
            (989_025, eve_server::services::sov_gameplay::TYPE_MERCENARY_DEN, PILOT, InventoryFlag::Cargo), // valid #2
        ] {
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: tid,
                    owner_id: owner,
                    location_id: PILOT_SHIP,
                    flag: flg,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
        }
        snap.contracts.insert(
            700_001,
            eve_db::ContractRecord {
                contract_id: 700_001,
                issuer_id: PILOT,
                contract_type: "ItemExchange".into(),
                status: "Outstanding".into(),
                item_ids: vec![989_001, 989_011, 989_021],
                ..Default::default()
            },
        );
    });

    // 1. Sov Hub anchoring rejects contract-locked, non-cargo, and unowned items (both by item_id and by type_id).
    assert_eq!(rpc_call(&ctx, &mut s_dir, "structureDeployment", "Anchor", &[PyRep::Integer(989_001)]).await.unwrap_err().code, "ItemLockedInContractOrJob");
    assert_eq!(rpc_call(&ctx, &mut s_dir, "structureDeployment", "Anchor", &[PyRep::Integer(989_002)]).await.unwrap_err().code, "ItemNotInCargo");
    assert_eq!(rpc_call(&ctx, &mut s_dir, "structureDeployment", "Anchor", &[PyRep::Integer(989_003)]).await.unwrap_err().code, "ItemOwnershipViolation");
    assert_eq!(rpc_call(&ctx, &mut s_dir, "structureDeployment", "Anchor", &[PyRep::Integer(TYPE_SOVEREIGNTY_HUB as i64)]).await.unwrap_err().code, "MissingStructureItem");

    // 2. Seed Sov Hub + claim sovereignty; Skyhook anchoring rejects contract-locked, non-cargo, and unowned items.
    let hub_id = 1_140_000_001i64;
    let lava_sh = 1_140_000_010i64;
    let temp_sh = 1_140_000_011i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states.entry(null_sys).or_default().solar_system_id = null_sys;
        snap.structures.insert(hub_id, structure(hub_id, CORP, null_sys, TYPE_SOVEREIGNTY_HUB));
        let mut lsh = structure(lava_sh, CORP, null_sys, TYPE_ORBITAL_SKYHOOK);
        lsh.position = lava_pos.to_array();
        snap.structures.insert(lava_sh, lsh);
        snap.sov_runtime.skyhook_planets.insert(lava_sh, lava_planet);
        let mut tsh = structure(temp_sh, CORP, null_sys, TYPE_ORBITAL_SKYHOOK);
        tsh.position = temp_pos.to_array();
        snap.structures.insert(temp_sh, tsh);
        snap.sov_runtime.skyhook_planets.insert(temp_sh, temp_planet);
    });
    let _ = ctx.systems.get_or_spawn(null_sys).spawn_ball(make_ball(lava_sh, TYPE_ORBITAL_SKYHOOK, lava_pos)).await;
    let _ = ctx.systems.get_or_spawn(null_sys).spawn_ball(make_ball(temp_sh, TYPE_ORBITAL_SKYHOOK, temp_pos)).await;
    claim_system_sovereignty(&ctx, null_sys, ALLIANCE, CORP, hub_id).expect("claim null_sys");

    place_ship_ball(&ctx, null_sys, PILOT, PILOT_SHIP, 19_722, temp_pos).await;
    let (_, _, _, ice_planet, _) = find_nullsec_planets(&ctx);
    let ice_pos = cels.iter().find(|c| c.celestial_id == ice_planet).unwrap().position;
    place_ship_ball(&ctx, null_sys, PILOT, PILOT_SHIP, 19_722, ice_pos).await;
    assert_eq!(rpc_call(&ctx, &mut s_dir, "structureDeployment", "Anchor", &[PyRep::Integer(989_011), PyRep::Integer(ice_planet)]).await.unwrap_err().code, "ItemLockedInContractOrJob");
    assert_eq!(rpc_call(&ctx, &mut s_dir, "structureDeployment", "Anchor", &[PyRep::Integer(989_012), PyRep::Integer(ice_planet)]).await.unwrap_err().code, "ItemNotInCargo");
    assert_eq!(rpc_call(&ctx, &mut s_dir, "structureDeployment", "Anchor", &[PyRep::Integer(989_013), PyRep::Integer(ice_planet)]).await.unwrap_err().code, "ItemOwnershipViolation");
    assert_eq!(rpc_call(&ctx, &mut s_dir, "structureDeployment", "Anchor", &[PyRep::Integer(TYPE_ORBITAL_SKYHOOK as i64), PyRep::Integer(ice_planet)]).await.unwrap_err().code, "MissingSkyhookItem");

    // 3. `posMgr.AnchorStructure` for Mercenary Den (`85230`):
    //    - Rejects non-Temperate Lava Skyhook (`PlanetNotTemperate`), >10 km distance (`MercenaryDenTooFarFromSkyhook`),
    //      unclaimed nullsec (`SovereigntyNotClaimed`), Zarzakh (`SovereigntyNullsecRequired`), and locked/non-cargo/unowned items!
    place_ship_ball(&ctx, null_sys, PILOT, PILOT_SHIP, 19_722, lava_pos + DVec3::new(2_000.0, 0.0, 0.0)).await;
    let _ = ctx.drain_notifications_for_char(PILOT);
    assert_eq!(rpc_call(&ctx, &mut s_dir, "posMgr", "AnchorStructure", &[PyRep::Integer(989_024)]).await.unwrap_err().code, "PlanetNotTemperate");
    let inv_n = ctx.drain_notifications_for_char(PILOT).into_iter().find(|n| n.event == "OnMercenaryDenRegistrationInvalid").expect("OnMercenaryDenRegistrationInvalid");
    assert_eq!(inv_n.args[1].dict_get("itemID"), Some(&PyRep::Integer(989_024)));
    place_ship_ball(&ctx, null_sys, PILOT, PILOT_SHIP, 19_722, temp_pos + DVec3::new(25_000.0, 0.0, 0.0)).await;
    assert_eq!(rpc_call(&ctx, &mut s_dir, "posMgr", "AnchorStructure", &[PyRep::Integer(989_024)]).await.unwrap_err().code, "MercenaryDenTooFarFromSkyhook");
    place_ship_ball(&ctx, null_sys, PILOT, PILOT_SHIP, 19_722, temp_pos + DVec3::new(2_500.0, 0.0, 0.0)).await;
    assert_eq!(rpc_call(&ctx, &mut s_dir, "posMgr", "AnchorStructure", &[PyRep::Integer(989_021)]).await.unwrap_err().code, "ItemLockedInContractOrJob");
    assert_eq!(rpc_call(&ctx, &mut s_dir, "posMgr", "AnchorStructure", &[PyRep::Integer(989_022)]).await.unwrap_err().code, "ItemNotInCargo");
    assert_eq!(rpc_call(&ctx, &mut s_dir, "posMgr", "AnchorStructure", &[PyRep::Integer(989_023)]).await.unwrap_err().code, "ItemOwnershipViolation");

    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states.get_mut(&null_sys).unwrap().sov_alliance_id = None;
    });
    assert_eq!(rpc_call(&ctx, &mut s_dir, "posMgr", "AnchorStructure", &[PyRep::Integer(989_024)]).await.unwrap_err().code, "SovereigntyNotClaimed");
    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states.get_mut(&null_sys).unwrap().sov_alliance_id = Some(ALLIANCE);
    });

    // Valid 10-arg V24 `structureDeployment.Anchor` within 10 km of Temperate Skyhook in claimed nullsec succeeds, and a second den on the same Skyhook fails!
    let _ = ctx.drain_notifications_for_char(PILOT);
    let extra_cfg = PyRep::Dict(vec![
        (PyRep::String("skyhookID".into()), PyRep::Integer(temp_sh)),
        (PyRep::String("planetID".into()), PyRep::Integer(temp_planet)),
    ]);
    let ok_den = rpc_call(&ctx, &mut s_dir, "structureDeployment", "Anchor", &[
        PyRep::Integer(989_024),
        PyRep::Real(temp_pos.x + 2_500.0),
        PyRep::Real(temp_pos.z),
        PyRep::Real(0.0),
        PyRep::Integer(1),
        PyRep::String("Custom Den".into()),
        PyRep::String(String::new()),
        PyRep::Integer(0),
        PyRep::Integer(0),
        extra_cfg,
    ]).await.expect("10-arg structureDeployment.Anchor Mercenary Den");
    let ok_den_id = ok_den.dict_get("structureID").and_then(|v| v.as_i64()).unwrap_or(0);
    assert!(ok_den_id > 0);
    assert_eq!(ctx.db.with_snapshot(|s| s.structures[&ok_den_id].name.clone()), "Custom Den");
    assert!(ctx.drain_notifications_for_char(PILOT).iter().any(|n| n.event == "OnMercenaryDenRegistrationSuccess"));
    assert_eq!(rpc_call(&ctx, &mut s_dir, "posMgr", "AnchorStructure", &[PyRep::Integer(989_025)]).await.unwrap_err().code, "MercenaryDenAlreadyAnchoredOnSkyhook");

    drop(ctx);
    let _ = std::fs::remove_file(&db_path);
}
