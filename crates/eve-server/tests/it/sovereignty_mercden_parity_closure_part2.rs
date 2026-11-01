//! End-to-end parity closure tests (Part 2) for Tenebrex Cyno Jammer upgrade gating,
//! Sovereignty Hub hacking & `OnLocalHackProgressUpdated`, Mercenary Den Development/Anarchy
//! evolution & workforce disruption, and all 7 Quasar Sovereignty protobuf APIs & notices.

#![forbid(unsafe_code)]

use crate::quasar_sov_equinox_hub::{field, fields, id_req, idm, rpc, structure, subscribe, varint};
use crate::sovereignty_equinox_gameplay_closure::{
    drain_quasar, find_nullsec_planets, fit_subcap_blasters, lock_target_via_rpc, make_ball,
    open_sqlite_ctx, place_ship_ball, register_online_pilot, rpc_call, step_weapon_ticks,
    temp_sqlite_path, ALLIANCE, CORP, PILOT, PILOT_SHIP,
};
use eve_db::{corp_roles, InventoryFlag, InventoryItemRecord, TrainedSkillRecord};
use eve_proto::quasar::{encode_bytes_field, encode_varint_field};
use eve_proto::PyRep;
use eve_server::services::quasar_sov_hub::compute_hub_resources;
use eve_server::services::quasar_sov_mercden::TYPE_MERCENARY_DEN_MANAGEMENT;
use eve_server::services::sov_gameplay::{
    TENEBREX_SPOOL_UP_SECS, TYPE_MERCENARY_DEN, TYPE_TENEBREX_CYNO_JAMMER,
    UPGRADE_CYNOSURAL_SUPPRESSION,
};
use eve_server::services::sov_services::{
    claim_system_sovereignty, is_hub_hacked, TYPE_MAGMATIC_GAS, TYPE_ORBITAL_SKYHOOK,
    TYPE_SOVEREIGNTY_HUB, TYPE_SUPERIONIC_ICE,
};
use eve_server::services::station_structure::structure_lifecycle as life;
use glam::DVec3;

#[tokio::test]
async fn test_sov_cyno_jammer_upgrade_and_hub_hacking_parity() {
    // parity: feat:sovereignty.sov-cyno-jammer-upgrade
    // parity: feat:sovereignty.sov-hub-hacking
    // parity: notify:OnLocalHackProgressUpdated
    eve_testkit::attest(&[
        "feat:sovereignty.sov-cyno-jammer-upgrade",
        "feat:sovereignty.sov-hub-hacking",
        "notify:OnLocalHackProgressUpdated",
    ]);

    let db_path = temp_sqlite_path("sov_cyno_hack");
    let ctx = open_sqlite_ctx(&db_path, true);
    let (null_sys, _, _, _, _) = find_nullsec_planets(&ctx);
    let hub_id = 1_100_000_001i64;
    let hub_pos = DVec3::new(200_000.0, 0.0, 0.0);

    let mut s_dir = register_online_pilot(
        &ctx,
        84_001,
        "demo",
        "demo-secret",
        PILOT,
        CORP,
        Some(ALLIANCE),
        null_sys,
        PILOT_SHIP,
        11_957,
        hub_pos + DVec3::new(35_000.0, 0.0, 0.0),
        corp_roles::ROLE_DIRECTOR,
    )
    .await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states.entry(null_sys).or_default().solar_system_id = null_sys;
        snap.structures.insert(hub_id, structure(hub_id, CORP, null_sys, TYPE_SOVEREIGNTY_HUB));
        for (idx, cel) in ctx.sde.celestials_in_system(null_sys).into_iter().filter(|c| c.group_id == 7).enumerate() {
            let sh_id = 1_100_000_100 + idx as i64;
            snap.structures.insert(sh_id, structure(sh_id, CORP, null_sys, TYPE_ORBITAL_SKYHOOK));
            snap.sov_runtime.skyhook_planets.insert(sh_id, cel.celestial_id);
        }
        for (iid, tid, loc, flg, qty) in [
            (984_001, TYPE_MAGMATIC_GAS, hub_id, InventoryFlag::Cargo, 5_000),
            (984_002, TYPE_SUPERIONIC_ICE, hub_id, InventoryFlag::Cargo, 5_000),
            (984_004, 88_228, hub_id, InventoryFlag::Cargo, 1),
            (984_005, UPGRADE_CYNOSURAL_SUPPRESSION, hub_id, InventoryFlag::Cargo, 1),
            (984_010, TYPE_TENEBREX_CYNO_JAMMER, PILOT_SHIP, InventoryFlag::Cargo, 1),
            (984_011, 4_247, PILOT_SHIP, InventoryFlag::Cargo, 500),
            (984_020, 21_096, PILOT_SHIP, InventoryFlag::HiSlot0, 1),
            (984_021, 16_273, PILOT_SHIP, InventoryFlag::Cargo, 2_000),
        ] {
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: tid,
                    owner_id: PILOT,
                    location_id: loc,
                    flag: flg,
                    quantity: qty,
                    singleton: qty == 1,
                    ..Default::default()
                },
            );
        }
        snap.module_online_states.insert(984_020, true);
        let sk = snap.skills.entry(PILOT).or_default();
        for sid in [21_603u32, 3_335, 12_092, 22_761] {
            sk.insert(sid, TrainedSkillRecord { type_id: sid, level: 5, skill_points: 256_000.0 });
        }
    });
    ctx.invalidate_ship_fitting(PILOT_SHIP);
    let _ = ctx.systems.get_or_spawn(null_sys).spawn_ball(make_ball(hub_id, TYPE_SOVEREIGNTY_HUB, hub_pos)).await;
    claim_system_sovereignty(&ctx, null_sys, ALLIANCE, CORP, hub_id).expect("claim null_sys");

    // 1. Anchoring Tenebrex fails closed until Cynosural Suppression (81619) is installed & onlined.
    let err_offline = rpc_call(
        &ctx,
        &mut s_dir,
        "structureDeployment",
        "Anchor",
        &[PyRep::Integer(984_010), PyRep::String("Tenebrex".into())],
    )
    .await
    .unwrap_err();
    assert_eq!(err_offline.code, "RequiredSovUpgradeOffline");

    for (fuel_iid, amt) in [(984_001i64, 2_000u64), (984_002i64, 2_000u64)] {
        let mut add = id_req(1, fuel_iid as u64);
        encode_varint_field(2, amt, &mut add);
        encode_bytes_field(3, &idm(hub_id as u64), &mut add);
        assert_eq!(rpc(&ctx, &mut s_dir, "eve_public.sovereignty.hub.fuel.api.AddRequest", add).await.status_code, 200);
    }
    let mut proc = Vec::new();
    for iid in [984_004u64, 984_005] {
        encode_bytes_field(1, &idm(iid), &mut proc);
    }
    for tid in [88_228u64, UPGRADE_CYNOSURAL_SUPPRESSION as u64] {
        let mut c = Vec::new();
        encode_bytes_field(1, &idm(tid), &mut c);
        encode_varint_field(2, 1, &mut c);
        encode_bytes_field(2, &c, &mut proc);
    }
    encode_bytes_field(3, &idm(hub_id as u64), &mut proc);
    assert_eq!(rpc(&ctx, &mut s_dir, "eve_public.sovereignty.hub.upgrade.api.ProcessConfigurationRequest", proc).await.status_code, 200);

    // Anchor Tenebrex, fuel it, advance through anchoring + onlining + 300s spool-up -> system jammed!
    let deep = DVec3::new(40_000_000.0, 40_000_000.0, 40_000_000.0);
    place_ship_ball(&ctx, null_sys, PILOT, PILOT_SHIP, 11_957, deep).await;
    let tenebrex_id = rpc_call(&ctx, &mut s_dir, "structureDeployment", "Anchor", &[PyRep::Integer(984_010), PyRep::String("Tenebrex".into())])
        .await.unwrap().dict_get("structureID").and_then(|v| v.as_i64()).unwrap();
    place_ship_ball(&ctx, null_sys, PILOT, PILOT_SHIP, 11_957, deep + DVec3::new(60_000.0, 0.0, 0.0)).await;
    rpc_call(&ctx, &mut s_dir, "invbroker", "Add", &[PyRep::Integer(984_011), PyRep::Integer(tenebrex_id), PyRep::Integer(133), PyRep::Integer(500)])
        .await.expect("fuel Tenebrex");
    ctx.advance_secs(15 * 60 + 24 * 3_600 + 15 * 60 + 10);
    life::sync_all_structures(&ctx);
    ctx.advance_secs(TENEBREX_SPOOL_UP_SECS + 5);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;

    let jam_on = rpc_call(&ctx, &mut s_dir, "beyonce", "GetCynoJammerState", &[]).await.expect("GetCynoJammerState");
    assert_eq!(jam_on.dict_get("isJammed"), Some(&PyRep::Bool(true)));
    ctx.db.with_mut_snapshot(|s| {
        s.fleets.insert(5001, eve_db::FleetRecord { fleet_id: 5001, boss_id: PILOT, members: vec![PILOT], ..Default::default() });
    });
    let cyno_err = rpc_call(&ctx, &mut s_dir, "dogmaIM", "Activate", &[PyRep::Integer(984_020), PyRep::String("cynosuralGeneration".into())])
        .await.unwrap_err();
    assert_eq!(cyno_err.code, "CynoJammed");

    // 2. Hostile Data Analyzer I hacking minigame on Sov Hub emits OnLocalHackProgressUpdated + OnSovHubHacked and disables jammer!
    let hacker_cid = 90_000_301i64;
    let hacker_ship = 100_000_301i64;
    let mut s_hacker = register_online_pilot(
        &ctx,
        84_010,
        "hacker",
        "sec-h",
        hacker_cid,
        98_000_301,
        Some(99_000_301),
        null_sys,
        hacker_ship,
        621,
        hub_pos + DVec3::new(2_500.0, 0.0, 0.0),
        0,
    )
    .await;
    ctx.db.with_mut_snapshot(|snap| {
        let sk = snap.skills.entry(hacker_cid).or_default();
        for sid in [3_405u32, 21_718] {
            sk.insert(sid, TrainedSkillRecord { type_id: sid, level: 5, skill_points: 256_000.0 });
        }
        snap.items.insert(
            984_100,
            InventoryItemRecord {
                item_id: 984_100,
                type_id: 22_175,
                owner_id: hacker_cid,
                location_id: hacker_ship,
                flag: InventoryFlag::MedSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(984_100, true);
    });
    ctx.invalidate_ship_fitting(hacker_ship);
    lock_target_via_rpc(&ctx, &mut s_hacker, null_sys, hacker_ship, hub_id).await;
    let _ = ctx.drain_notifications_for_char(PILOT);
    let _ = ctx.drain_notifications_for_char(hacker_cid);

    rpc_call(&ctx, &mut s_hacker, "dogmaIM", "Activate", &[PyRep::Integer(984_100), PyRep::String("dataHacking".into()), PyRep::Integer(hub_id)])
        .await.expect("start Data Analyzer on Sov Hub");
    rpc_call(&ctx, &mut s_hacker, "hackingMgr", "StartNewGameInstance", &[PyRep::Integer(hub_id)])
        .await.expect("start hacking board");

    let mut progress_values = Vec::new();
    let mut won = false;
    for _pass in 0..8 {
        for tile in 1..24i64 {
            for _ in 0..5 {
                if let Ok(step) = rpc_call(&ctx, &mut s_hacker, "hackingMgr", "ClickedOnTile", &[PyRep::Integer(hub_id), PyRep::Integer(tile)]).await {
                    for n in ctx.drain_notifications_for_char(PILOT) {
                        if n.event == "OnLocalHackProgressUpdated" {
                            assert_eq!(n.args[0].as_i64(), Some(hub_id));
                            if let Some(p) = n.args[1].as_f64() {
                                progress_values.push(p);
                            }
                        } else if n.event == "OnSovHubHacked" {
                            assert_eq!(n.args[0].as_i64(), Some(hub_id));
                        }
                    }
                    if step.dict_get("status").and_then(|v| v.as_str()) == Some("Won") {
                        won = true;
                        break;
                    }
                } else {
                    break;
                }
            }
            if won {
                break;
            }
        }
        if won {
            break;
        }
    }
    assert_eq!(won, true);
    assert_eq!(is_hub_hacked(&ctx, hub_id), true);
    assert_eq!(progress_values.iter().any(|&p| p > 0.0 && p < 1.0), true, "intermediate OnLocalHackProgressUpdated values emitted");
    assert_eq!(progress_values.last().copied(), Some(1.0), "final OnLocalHackProgressUpdated reaches 1.0");

    // While hacked, Cynosural Suppression upgrade is disabled so Tenebrex immediately stops jamming!
    let jam_hacked = rpc_call(&ctx, &mut s_dir, "beyonce", "GetCynoJammerState", &[]).await.expect("GetCynoJammerState while hacked");
    assert_eq!(jam_hacked.dict_get("isJammed"), Some(&PyRep::Bool(false)));
    let cyno_ok = rpc_call(&ctx, &mut s_dir, "dogmaIM", "Activate", &[PyRep::Integer(984_020), PyRep::String("cynosuralGeneration".into())]).await;
    assert_eq!(cyno_ok.unwrap().dict_get("solarSystemID").and_then(|v| v.as_i64()), Some(null_sys as i64));

    // Once the 1h hack expires, Cynosural Suppression recovers and Tenebrex re-spools.
    ctx.advance_secs(3_605);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    assert_eq!(is_hub_hacked(&ctx, hub_id), false);
    ctx.advance_secs(TENEBREX_SPOOL_UP_SECS + 5);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    assert_eq!(rpc_call(&ctx, &mut s_dir, "beyonce", "GetCynoJammerState", &[]).await.unwrap().dict_get("isJammed"), Some(&PyRep::Bool(true)));

    drop(ctx);
    let _ = std::fs::remove_file(&db_path);
}

#[tokio::test]
async fn test_mercenary_den_lifecycle_evolution_and_anarchy_parity() {
    // DATA GAP: feat:sovereignty.mercenary-den / mercenary-den-development-anarchy / mercenary-den-anarchy-evolution — synthetic yield/capacity/anarchy values pending V24.01 client/SDE tables (docs/BLOCKERS.md §10)

    let db_path = temp_sqlite_path("sov_mercden_evo");
    let ctx = open_sqlite_ctx(&db_path, true);
    let (null_sys, temp_planet, _, _, _) = find_nullsec_planets(&ctx);
    let (hub_id, skyhook_id) = (1_110_000_001i64, 1_110_000_002i64);
    let sh_pos = DVec3::new(200_000.0, 0.0, 0.0);

    let mut s_owner = register_online_pilot(
        &ctx,
        85_001,
        "demo",
        "demo-secret",
        PILOT,
        CORP,
        Some(ALLIANCE),
        null_sys,
        PILOT_SHIP,
        19_722,
        sh_pos + DVec3::new(3_000.0, 0.0, 0.0),
        corp_roles::ROLE_DIRECTOR,
    )
    .await;
    let mut rx = subscribe(&ctx, &mut s_owner).await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states.entry(null_sys).or_default().sov_alliance_id = Some(ALLIANCE);
        snap.structures.insert(hub_id, structure(hub_id, CORP, null_sys, TYPE_SOVEREIGNTY_HUB));
        snap.structures.insert(skyhook_id, structure(skyhook_id, CORP, null_sys, TYPE_ORBITAL_SKYHOOK));
        snap.sov_runtime.skyhook_planets.insert(skyhook_id, temp_planet);
        snap.skills.entry(PILOT).or_default().insert(
            TYPE_MERCENARY_DEN_MANAGEMENT,
            TrainedSkillRecord { type_id: TYPE_MERCENARY_DEN_MANAGEMENT, level: 2, skill_points: 50_000.0 },
        );
        snap.items.insert(
            985_001,
            InventoryItemRecord {
                item_id: 985_001,
                type_id: TYPE_MERCENARY_DEN,
                owner_id: PILOT,
                location_id: PILOT_SHIP,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                ..Default::default()
            },
        );
    });
    let _ = ctx.systems.get_or_spawn(null_sys).spawn_ball(make_ball(skyhook_id, TYPE_ORBITAL_SKYHOOK, sh_pos)).await;

    let base_wf = ctx.db.with_snapshot(|s| compute_hub_resources(s, &ctx.sde, hub_id).unwrap().workforce_available);
    let den_id = rpc_call(&ctx, &mut s_owner, "structureDeployment", "Anchor", &[PyRep::Integer(TYPE_MERCENARY_DEN as i64), PyRep::Integer(skyhook_id), PyRep::Integer(temp_planet)])
        .await.unwrap().dict_get("structureID").and_then(|v| v.as_i64()).unwrap();
    let _ = drain_quasar(&mut rx);

    // Stage 1 (0..19): 65 IDK/hr, 0% workforce disruption.
    ctx.advance_secs(2 * 3_600);
    let d_2h = rpc(&ctx, &mut s_owner, "eve_public.sovereignty.mercenaryden.api.GetAsOwnerRequest", id_req(1, den_id as u64)).await.payload.unwrap().value;
    let info_2h = field(&field(&d_2h, 4).unwrap(), 2).unwrap();
    assert_eq!(varint(&info_2h, 1), Some(130), "2h at stage 1 yields 2 * 65 = 130 IDK");
    assert_eq!(ctx.db.with_snapshot(|s| compute_hub_resources(s, &ctx.sde, hub_id).unwrap().workforce_available), base_wf);

    // Advance to Day 4 (Dev 20, Anarchy 20 -> Stage 2): 68 IDK/hr, still 0% workforce disruption.
    ctx.advance_secs(4 * 86_400 - 2 * 3_600);
    let d_day4 = rpc(&ctx, &mut s_owner, "eve_public.sovereignty.mercenaryden.api.GetAsOwnerRequest", id_req(1, den_id as u64)).await.payload.unwrap().value;
    let evo4 = field(&d_day4, 3).unwrap();
    let dev_sim4 = field(&field(&evo4, 2).unwrap(), 3).unwrap();
    assert_eq!(varint(&dev_sim4, 1), Some(20));
    assert_eq!(varint(&dev_sim4, 2), Some(2));
    assert_eq!(ctx.db.with_snapshot(|s| compute_hub_resources(s, &ctx.sde, hub_id).unwrap().workforce_available), base_wf);

    // Advance to Day 8 (Dev 40, Anarchy 40 -> Stage 3): -10% of Temperate Skyhook workforce (150 of 1,500)!
    ctx.advance_secs(4 * 86_400);
    let _ = rpc(&ctx, &mut s_owner, "eve_public.sovereignty.mercenaryden.api.GetAsOwnerRequest", id_req(1, den_id as u64)).await;
    assert_eq!(ctx.db.with_snapshot(|s| compute_hub_resources(s, &ctx.sde, hub_id).unwrap().workforce_available) + 150, base_wf);

    // Advance to Day 14 (Dev 70, Anarchy 70 -> Stage 4): -20% of Temperate Skyhook workforce (300 of 1,500)!
    ctx.advance_secs(6 * 86_400);
    let _ = rpc(&ctx, &mut s_owner, "eve_public.sovereignty.mercenaryden.api.GetAsOwnerRequest", id_req(1, den_id as u64)).await;
    assert_eq!(ctx.db.with_snapshot(|s| compute_hub_resources(s, &ctx.sde, hub_id).unwrap().workforce_available) + 300, base_wf);

    // Advance to Day 20 (Dev 100, Anarchy 100 -> Stage 5): 78 IDK/hr, -40% of Temperate Skyhook workforce (600 of 1,500)!
    ctx.advance_secs(6 * 86_400);
    let _ = rpc(&ctx, &mut s_owner, "eve_public.sovereignty.mercenaryden.api.GetAsOwnerRequest", id_req(1, den_id as u64)).await;
    assert_eq!(ctx.db.with_snapshot(|s| compute_hub_resources(s, &ctx.sde, hub_id).unwrap().workforce_available) + 600, base_wf);

    // Extract stored infomorphs with 25% sov tax, then advance 1h at Stage 5 to verify 78 IDK/hr rate!
    rpc_call(&ctx, &mut s_owner, "sovMgr", "ConfirmSkyhookSovManagement", &[PyRep::Integer(skyhook_id), PyRep::Integer(25)])
        .await.expect("set 25% Mercenary Den tax");
    let den_pos = ctx.systems.get_or_spawn(null_sys).get_ball_snapshot(den_id).await.unwrap().unwrap().1.position;
    place_ship_ball(&ctx, null_sys, PILOT, PILOT_SHIP, 16_229, den_pos + DVec3::new(2_000.0, 0.0, 0.0)).await;
    let ext = rpc_call(&ctx, &mut s_owner, "beyonce", "CallComponentFromClient", &[PyRep::Integer(den_id), PyRep::String("mercenaryDen".into()), PyRep::String("extract_infomorphs_to_ship".into())])
        .await.expect("extract infomorphs");
    assert_eq!(ext.dict_get("ownerShare").and_then(|v| v.as_i64()), Some(2_250));
    assert_eq!(ext.dict_get("taxShare").and_then(|v| v.as_i64()), Some(750));

    ctx.advance_secs(3_600);
    let d_s5 = rpc(&ctx, &mut s_owner, "eve_public.sovereignty.mercenaryden.api.GetAsOwnerRequest", id_req(1, den_id as u64)).await.payload.unwrap().value;
    let info_s5 = field(&field(&d_s5, 4).unwrap(), 2).unwrap();
    assert_eq!(varint(&info_s5, 1), Some(78), "1h at stage 5 yields 78 IDK");

    // Completing an MTO at max Development (100) grants +350 bonus infomorphs (78 -> 428) and lowers Anarchy 100 -> 90 (stage 4: -300 wf)!
    let acts = rpc(&ctx, &mut s_owner, "eve_public.sovereignty.mercenaryden.activity.api.GetForMercenaryDenRequest", id_req(1, den_id as u64)).await.payload.unwrap().value;
    let first_act = fields(&acts, 1).into_iter().next().unwrap();
    let mut start_req = Vec::new();
    encode_bytes_field(1, &field(&first_act, 1).unwrap(), &mut start_req);
    assert_eq!(rpc(&ctx, &mut s_owner, "eve_public.sovereignty.mercenaryden.activity.api.StartRequest", start_req).await.status_code, 200);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    let mto_ball = ctx.db.with_snapshot(|s| s.sov_runtime.mto_site_balls.keys().copied().next().unwrap());
    let mto_pos = ctx.systems.get_or_spawn(null_sys).get_ball_snapshot(mto_ball).await.unwrap().unwrap().1.position;
    fit_subcap_blasters(&ctx, PILOT, PILOT_SHIP, 985_200);
    place_ship_ball(&ctx, null_sys, PILOT, PILOT_SHIP, 16_229, mto_pos + DVec3::new(1_500.0, 0.0, 0.0)).await;
    lock_target_via_rpc(&ctx, &mut s_owner, null_sys, PILOT_SHIP, mto_ball).await;
    for round in 0..55 {
        for gun in (0..6).map(|g| 985_200i64 + g * 2) {
            ctx.set_invention_roll_override(Some(0.45));
            let _ = rpc_call(&ctx, &mut s_owner, "dogmaIM", "Activate", &[PyRep::Integer(gun), PyRep::Integer(mto_ball)]).await;
            if ctx.db.with_snapshot(|s| !s.sov_runtime.mto_site_balls.contains_key(&mto_ball)) {
                break;
            }
        }
        if ctx.db.with_snapshot(|s| !s.sov_runtime.mto_site_balls.contains_key(&mto_ball)) {
            break;
        }
        if round % 10 == 9 {
            step_weapon_ticks(&ctx, null_sys).await;
        }
    }
    assert_eq!(ctx.db.with_snapshot(|s| s.sov_runtime.mercenary_dens[&den_id].infomorphs), 428);
    assert_eq!(ctx.db.with_snapshot(|s| s.sov_runtime.mercenary_dens[&den_id].anarchy_level), 90);
    assert_eq!(ctx.db.with_snapshot(|s| compute_hub_resources(s, &ctx.sde, hub_id).unwrap().workforce_available) + 300, base_wf);

    drop(ctx);
    let _ = std::fs::remove_file(&db_path);
}

#[tokio::test]
async fn test_quasar_sovereignty_all_seven_protos_parity() {
    // DATA GAP: feat:quasar.sovereignty — server-only celestial resource tables (docs/BLOCKERS.md §9)
    // parity: quasar:eveProto/generated/eve_public/sovereignty/hub/api/requests_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/sovereignty/mercenaryden/activity/api/notices_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/sovereignty/mercenaryden/activity/api/requests_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/sovereignty/mercenaryden/api/notices_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/sovereignty/mercenaryden/api/requests_pb2.pyj
    // DATA GAP: quasar:eveProto/generated/eve_public/sovereignty/resource/planet/api/requests_pb2.pyj — server-only planet resource table (docs/BLOCKERS.md §9)
    // DATA GAP: quasar:eveProto/generated/eve_public/sovereignty/resource/star/api/requests_pb2.pyj — server-only star power table (docs/BLOCKERS.md §9)
    eve_testkit::attest(&[
        "quasar:eveProto/generated/eve_public/sovereignty/hub/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/sovereignty/mercenaryden/activity/api/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/sovereignty/mercenaryden/activity/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/sovereignty/mercenaryden/api/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/sovereignty/mercenaryden/api/requests_pb2.pyj",
    ]);

    let db_path = temp_sqlite_path("quasar_sov_protos");
    let ctx = open_sqlite_ctx(&db_path, true);
    let (null_sys, temp_planet, _, _, _) = find_nullsec_planets(&ctx);
    let (hub_id, skyhook_id) = (1_120_000_001i64, 1_120_000_002i64);
    let sh_pos = DVec3::new(200_000.0, 0.0, 0.0);

    let mut s_owner = register_online_pilot(
        &ctx,
        86_001,
        "demo",
        "demo-secret",
        PILOT,
        CORP,
        Some(ALLIANCE),
        null_sys,
        PILOT_SHIP,
        639,
        sh_pos + DVec3::new(2_500.0, 0.0, 0.0),
        corp_roles::ROLE_DIRECTOR,
    )
    .await;
    let mut rx = subscribe(&ctx, &mut s_owner).await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states.entry(null_sys).or_default().sov_alliance_id = Some(ALLIANCE);
        snap.structures.insert(hub_id, structure(hub_id, CORP, null_sys, TYPE_SOVEREIGNTY_HUB));
        snap.structures.insert(skyhook_id, structure(skyhook_id, CORP, null_sys, TYPE_ORBITAL_SKYHOOK));
        snap.sov_runtime.skyhook_planets.insert(skyhook_id, temp_planet);
        snap.skills.entry(PILOT).or_default().insert(
            TYPE_MERCENARY_DEN_MANAGEMENT,
            TrainedSkillRecord { type_id: TYPE_MERCENARY_DEN_MANAGEMENT, level: 4, skill_points: 180_000.0 },
        );
        snap.items.insert(
            986_001,
            InventoryItemRecord {
                item_id: 986_001,
                type_id: TYPE_MERCENARY_DEN,
                owner_id: PILOT,
                location_id: PILOT_SHIP,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                ..Default::default()
            },
        );
    });
    let _ = ctx.systems.get_or_spawn(null_sys).spawn_ball(make_ball(skyhook_id, TYPE_ORBITAL_SKYHOOK, sh_pos)).await;

    // 1. resource.planet.api (GetAllDefinitions, GetDefinitionsVersion, GetAllConfigurations) + invalid version negative check.
    let all_defs = rpc(&ctx, &mut s_owner, "eve_public.sovereignty.resource.planet.api.GetAllDefinitionsRequest", Vec::new()).await;
    let defs_list = fields(&all_defs.payload.unwrap().value, 1);
    assert_eq!(defs_list.len(), 8);
    assert_eq!(defs_list.iter().filter(|d| field(d, 4).is_some()).count(), 3);
    let lava_def = defs_list.iter().find(|d| varint(&field(d, 1).unwrap(), 1) == Some(2015)).expect("Lava planet definition");
    let lava_reagent_wrapper = field(lava_def, 4).expect("Lava reagent_definition");
    let lava_reagent_inner = field(&lava_reagent_wrapper, 2).expect("Lava inner ReagentDefinition");
    assert_eq!(varint(&lava_reagent_inner, 1), Some(50));
    assert_eq!(varint(&lava_reagent_inner, 2), Some(3_600));
    assert_eq!(varint(&lava_reagent_inner, 3), Some(50));
    assert_eq!(varint(&lava_reagent_inner, 4), Some(25_000));
    assert_eq!(varint(&lava_reagent_inner, 5), Some(25_000));
    assert_eq!(varint(&field(&lava_reagent_wrapper, 3).unwrap(), 1), Some(TYPE_MAGMATIC_GAS as u64));

    let ver_defs = rpc(&ctx, &mut s_owner, "eve_public.sovereignty.resource.planet.api.GetDefinitionsVersionRequest", Vec::new()).await;
    assert_eq!(fields(&ver_defs.payload.unwrap().value, 1).len(), 8);
    let mut bad_ver = Vec::new();
    let mut spec = Vec::new();
    encode_varint_field(1, 99, &mut spec);
    encode_bytes_field(1, &spec, &mut bad_ver);
    assert_eq!(rpc(&ctx, &mut s_owner, "eve_public.sovereignty.resource.planet.api.GetDefinitionsVersionRequest", bad_ver).await.status_code, 404);

    let planet_cfgs = rpc(&ctx, &mut s_owner, "eve_public.sovereignty.resource.planet.api.GetAllConfigurationsRequest", Vec::new()).await;
    let cfg_list = fields(&planet_cfgs.payload.unwrap().value, 1);
    let expected_planets = ctx.sde.celestials_in_system(null_sys).into_iter().filter(|c| c.group_id == 7).count();
    assert_eq!(cfg_list.len(), expected_planets);
    let reagent_cfgs: Vec<Vec<u8>> = cfg_list.iter().filter_map(|c| field(c, 4)).collect();
    assert!(reagent_cfgs.len() >= 2);
    let gas_cfg = reagent_cfgs.iter().find(|c| varint(&field(c, 1).unwrap(), 1) == Some(TYPE_MAGMATIC_GAS as u64)).expect("Magmatic Gas config");
    assert_eq!(varint(&field(gas_cfg, 2).unwrap(), 4), Some(25_000));
    assert_eq!(varint(&field(gas_cfg, 3).unwrap(), 2), Some(86_400));
    let ice_cfg = reagent_cfgs.iter().find(|c| varint(&field(c, 1).unwrap(), 1) == Some(TYPE_SUPERIONIC_ICE as u64)).expect("Superionic Ice config");
    assert_eq!(varint(&field(ice_cfg, 2).unwrap(), 4), Some(25_000));
    assert_eq!(varint(&field(ice_cfg, 3).unwrap(), 2), Some(86_400));

    // 2. resource.star.api (GetAllConfigurationsRequest)
    let star_cfgs = rpc(&ctx, &mut s_owner, "eve_public.sovereignty.resource.star.api.GetAllConfigurationsRequest", Vec::new()).await;
    let stars = fields(&star_cfgs.payload.unwrap().value, 1);
    assert_eq!(stars.is_empty(), false);
    assert_eq!(varint(&stars[0], 2).unwrap_or(0) >= 200, true);

    // 3. hub.api (GetResourcesRequest + non-owner forbidden check)
    let hub_res = rpc(&ctx, &mut s_owner, "eve_public.sovereignty.hub.api.GetResourcesRequest", id_req(1, hub_id as u64)).await.payload.unwrap().value;
    assert_eq!(varint(&field(&hub_res, 1).unwrap(), 1).unwrap_or(0) > 0, true);
    assert_eq!(varint(&field(&hub_res, 2).unwrap(), 1).unwrap_or(0) > 0, true);

    let mut s_other = register_online_pilot(&ctx, 86_002, "other2", "sec-o", 90_000_310, 98_000_310, Some(99_000_310), null_sys, 100_000_310, 639, sh_pos, 0).await;
    assert_eq!(rpc(&ctx, &mut s_other, "eve_public.sovereignty.hub.api.GetResourcesRequest", id_req(1, hub_id as u64)).await.status_code, 403);

    // 4. mercenaryden.api + notices + mercenaryden.activity.api + notices
    let den_id = rpc_call(&ctx, &mut s_owner, "structureDeployment", "Anchor", &[PyRep::Integer(TYPE_MERCENARY_DEN as i64), PyRep::Integer(skyhook_id), PyRep::Integer(temp_planet)])
        .await.unwrap().dict_get("structureID").and_then(|v| v.as_i64()).unwrap();
    let anchor_notices = drain_quasar(&mut rx);
    assert_eq!(anchor_notices.iter().any(|(t, _)| t == "eve_public.sovereignty.mercenaryden.api.DefinitionsUpdatedNotice"), true);
    assert_eq!(anchor_notices.iter().filter(|(t, _)| t == "eve_public.sovereignty.mercenaryden.activity.api.AddedNotice").count(), 3);

    let max_rsp = rpc(&ctx, &mut s_owner, "eve_public.sovereignty.mercenaryden.api.GetMaximumForCharacterRequest", Vec::new()).await.payload.unwrap().value;
    assert_eq!(varint(&max_rsp, 1), Some(4));
    assert_eq!(varint(&max_rsp, 2), Some(5));

    let owned_rsp = rpc(&ctx, &mut s_owner, "eve_public.sovereignty.mercenaryden.api.GetAllOwnedRequest", Vec::new()).await.payload.unwrap().value;
    assert_eq!(fields(&owned_rsp, 1).len(), 1);

    let owner_rsp = rpc(&ctx, &mut s_owner, "eve_public.sovereignty.mercenaryden.api.GetAsOwnerRequest", id_req(1, den_id as u64)).await.payload.unwrap().value;
    let owner_attrs = field(&owner_rsp, 1).unwrap();
    assert_eq!(varint(&field(&owner_attrs, 1).unwrap(), 1), Some(PILOT as u64));
    assert_eq!(varint(&field(&owner_attrs, 2).unwrap(), 1), Some(skyhook_id as u64));
    assert_eq!(rpc(&ctx, &mut s_other, "eve_public.sovereignty.mercenaryden.api.GetAsOwnerRequest", id_req(1, den_id as u64)).await.status_code, 403);

    let cap_rsp = rpc(&ctx, &mut s_owner, "eve_public.sovereignty.mercenaryden.activity.api.GetCapacityRequest", Vec::new()).await.payload.unwrap().value;
    assert_eq!(varint(&cap_rsp, 1), Some(3));
    let all_acts = rpc(&ctx, &mut s_owner, "eve_public.sovereignty.mercenaryden.activity.api.GetAllRequest", Vec::new()).await.payload.unwrap().value;
    assert_eq!(fields(&all_acts, 1).len(), 3);
    let den_acts = rpc(&ctx, &mut s_owner, "eve_public.sovereignty.mercenaryden.activity.api.GetForMercenaryDenRequest", id_req(1, den_id as u64)).await.payload.unwrap().value;
    let act_list = fields(&den_acts, 1);
    assert_eq!(act_list.len(), 3);

    let pre_mto_bal = ctx.db.with_snapshot(|s| s.characters[&PILOT].balance.0);
    let mut start_req = Vec::new();
    encode_bytes_field(1, &field(&act_list[0], 1).unwrap(), &mut start_req);
    assert_eq!(rpc(&ctx, &mut s_owner, "eve_public.sovereignty.mercenaryden.activity.api.StartRequest", start_req).await.status_code, 200);
    let start_notices = drain_quasar(&mut rx);
    assert_eq!(start_notices.iter().any(|(t, _)| t == "eve_public.sovereignty.mercenaryden.activity.api.StartedNotice"), true);
    assert_eq!(start_notices.iter().any(|(t, _)| t == "eve_public.sovereignty.mercenaryden.activity.api.ExpiryChangedNotice"), true);

    // Spawn the live MTO target ball via cluster tick and destroy it through real `dogmaIM.AddTarget` + `dogmaIM.Activate` weapon fire!
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    let site_ball = ctx.db.with_snapshot(|s| s.sov_runtime.mto_site_balls.keys().copied().next().unwrap());
    let site_pos = ctx.systems.get_or_spawn(null_sys).get_ball_snapshot(site_ball).await.unwrap().unwrap().1.position;
    fit_subcap_blasters(&ctx, PILOT, PILOT_SHIP, 986_200);
    place_ship_ball(&ctx, null_sys, PILOT, PILOT_SHIP, 16_229, site_pos + DVec3::new(1_500.0, 0.0, 0.0)).await;
    lock_target_via_rpc(&ctx, &mut s_owner, null_sys, PILOT_SHIP, site_ball).await;
    for round in 0..55 {
        for gun_id in (0..6).map(|g| 986_200i64 + g * 2) {
            ctx.set_invention_roll_override(Some(0.45));
            let _ = rpc_call(&ctx, &mut s_owner, "dogmaIM", "Activate", &[PyRep::Integer(gun_id), PyRep::Integer(site_ball)]).await;
            if ctx.db.with_snapshot(|s| !s.sov_runtime.mto_site_balls.contains_key(&site_ball)) {
                break;
            }
        }
        if ctx.db.with_snapshot(|s| !s.sov_runtime.mto_site_balls.contains_key(&site_ball)) {
            break;
        }
        if round % 10 == 9 {
            step_weapon_ticks(&ctx, null_sys).await;
        }
    }
    assert!(ctx.db.with_snapshot(|s| !s.sov_runtime.mto_site_balls.contains_key(&site_ball)));
    assert_eq!(ctx.db.with_snapshot(|s| s.characters[&PILOT].balance.0), pre_mto_bal, "MTO completion mints 0 synthetic ISK");
    let comp_notices = drain_quasar(&mut rx);
    assert_eq!(comp_notices.iter().any(|(t, _)| t == "eve_public.sovereignty.mercenaryden.activity.api.CompletedNotice"), true);
    assert_eq!(comp_notices.iter().any(|(t, _)| t == "eve_public.sovereignty.mercenaryden.api.EvolutionStateChangedNotice"), true);

    // Advance 4 days past unstarted activity expiry: `GetAllRequest` is strictly read-only (filters expired activities,
    // emits 0 notices, and does NOT mutate `mercenary_activities` or `simulated_at_unix` until tick/refresh runs).
    ctx.advance_secs(4 * 86_400);
    let snap_before_get_all = ctx.db.with_snapshot(|s| (s.sov_runtime.mercenary_activities.len(), s.sov_runtime.mercenary_dens[&den_id].simulated_at_unix));
    let get_all_expired = rpc(&ctx, &mut s_owner, "eve_public.sovereignty.mercenaryden.activity.api.GetAllRequest", Vec::new()).await.payload.unwrap().value;
    assert!(fields(&get_all_expired, 1).is_empty(), "GetAllRequest filters out expired activities without mutating state");
    assert!(drain_quasar(&mut rx).is_empty(), "GetAllRequest must not emit notices");
    assert_eq!(
        ctx.db.with_snapshot(|s| (s.sov_runtime.mercenary_activities.len(), s.sov_runtime.mercenary_dens[&den_id].simulated_at_unix)),
        snap_before_get_all,
        "GetAllRequest must not mutate persisted mercenary_activities or simulated_at_unix"
    );

    let _ = rpc(&ctx, &mut s_owner, "eve_public.sovereignty.mercenaryden.activity.api.GetForMercenaryDenRequest", id_req(1, den_id as u64)).await;
    let exp_notices = drain_quasar(&mut rx);
    assert_eq!(exp_notices.iter().filter(|(t, _)| t == "eve_public.sovereignty.mercenaryden.activity.api.RemovedNotice").count(), 2);

    drop(ctx);
    let _ = std::fs::remove_file(&db_path);
}
