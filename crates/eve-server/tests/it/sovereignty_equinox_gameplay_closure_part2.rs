//! Part 2 of Equinox Sovereignty closure tests: Mercenary Den Anchoring, Anarchy, MTO Combat, Repair & Loot (`SO-5`, `Q-8`).

#![forbid(unsafe_code)]

use crate::quasar_sov_equinox_hub::{field, fields, id_req, rpc, structure, subscribe};
use crate::sovereignty_equinox_gameplay_closure::{
    drain_notif_events, drain_quasar, find_nullsec_planets, fit_siege_artillery,
    lock_target_via_rpc, make_ball, register_online_pilot, rpc_call, step_weapon_ticks, ALLIANCE,
    CORP, PILOT, PILOT_SHIP,
};
use eve_db::{corp_roles, InventoryFlag, InventoryItemRecord, TrainedSkillRecord};
use eve_proto::quasar::encode_bytes_field;
use eve_proto::PyRep;
use eve_server::quasar_gateway::{STATUS_CONFLICT, STATUS_OK};
use eve_server::router::ServerContext;
use eve_server::services::quasar_sov_hub::compute_hub_resources;
use eve_server::services::quasar_sov_mercden::TYPE_MERCENARY_DEN_MANAGEMENT;
use eve_server::services::sov_gameplay::{
    MERC_DEN_REINFORCEMENT_SECS, MERC_DEN_VULNERABILITY_WINDOW_SECS, TYPE_INFOMORPH_DECRYPTION_KEY,
    TYPE_MERCENARY_DEN,
};
use eve_server::services::sov_services::{TYPE_ORBITAL_SKYHOOK, TYPE_SOVEREIGNTY_HUB};
use glam::DVec3;

#[tokio::test]
async fn test_mercenary_den_live_anchoring_anarchy_mto_combat_repair_and_loot() {
    // DATA GAP: feat:sovereignty.mercenary-den (open until real SDE tables imported)
    // parity: feat:sovereignty.mercenary-den-anchoring
    // DATA GAP: feat:sovereignty.mercenary-den-development-anarchy (open until real SDE tables imported)
    // DATA GAP: feat:sovereignty.mercenary-den-anarchy-evolution (open until real SDE tables imported)
    // DATA GAP: feat:pve.mercenary-tactical-ops (open until real SDE tables imported)
    // parity: feat:sovereignty.mercenary-den-reinforcement-loot
    // DATA GAP: quasar:eveProto/generated/eve_public/sovereignty/mercenaryden/activity/api/requests_pb2.pyj (open until real SDE tables imported)
    // DATA GAP: quasar:eveProto/generated/eve_public/sovereignty/mercenaryden/activity/api/notices_pb2.pyj (open until real SDE tables imported)
    // DATA GAP: quasar:eveProto/generated/eve_public/sovereignty/mercenaryden/api/notices_pb2.pyj (open until real SDE tables imported)
    // parity: notify:OnMercenaryDenRegistrationInvalid
    // parity: notify:OnMercenaryDenRegistrationSuccess
    // parity: notify:OnMyMercenaryDenRemoved
    eve_testkit::attest(&[
        "feat:sovereignty.mercenary-den-anchoring",
        "feat:sovereignty.mercenary-den-reinforcement-loot",
        "notify:OnMercenaryDenRegistrationInvalid",
        "notify:OnMercenaryDenRegistrationSuccess",
        "notify:OnMyMercenaryDenRemoved",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let (null_sys, temp_planet, _, _, non_temp_planet) = find_nullsec_planets(&ctx);
    let hub_id = 1_050_000_001i64;
    let skyhook_id = 1_050_000_002i64;
    let sh_pos = DVec3::new(200_000.0, 0.0, 0.0);

    let mut s_owner = register_online_pilot(
        &ctx, 72_001, "demo", "demo-secret", PILOT, CORP, Some(ALLIANCE),
        null_sys, PILOT_SHIP, 639, DVec3::new(50_000.0, 0.0, 0.0), corp_roles::ROLE_DIRECTOR,
    ).await;
    let mut rx = subscribe(&ctx, &mut s_owner).await;

    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states.entry(null_sys).or_default().sov_alliance_id = Some(ALLIANCE);
        snap.structures.insert(hub_id, structure(hub_id, CORP, null_sys, TYPE_SOVEREIGNTY_HUB));
        snap.structures.insert(skyhook_id, structure(skyhook_id, CORP, null_sys, TYPE_ORBITAL_SKYHOOK));
        snap.sov_runtime.skyhook_planets.insert(skyhook_id, temp_planet);
        snap.skills.entry(PILOT).or_default().insert(
            TYPE_MERCENARY_DEN_MANAGEMENT,
            TrainedSkillRecord { type_id: TYPE_MERCENARY_DEN_MANAGEMENT, level: 1, skill_points: 25_000.0 },
        );
        snap.items.insert(
            990_201,
            InventoryItemRecord {
                item_id: 990_201,
                type_id: TYPE_MERCENARY_DEN,
                owner_id: PILOT,
                location_id: PILOT_SHIP,
                flag: InventoryFlag::Cargo,
                quantity: 2,
                ..Default::default()
            },
        );
    });
    let _ = ctx.systems.get_or_spawn(null_sys).spawn_ball(make_ball(skyhook_id, TYPE_ORBITAL_SKYHOOK, sh_pos)).await;

    // Sovereign owner configures Skyhook Mercenary Den tax via authorized ConfirmSkyhookSovManagement RPC
    let tax_rep = rpc_call(&ctx, &mut s_owner, "sovMgr", "ConfirmSkyhookSovManagement", &[PyRep::Integer(skyhook_id), PyRep::Integer(20)])
        .await.expect("owner sets skyhook mercenary den tax");
    assert_eq!(tax_rep.dict_get("taxPercent").and_then(|v| v.as_i64()), Some(20));

    // 1. Live Mercenary Den anchoring validation (>10km, non-Temperate, skill cap) & notifications.
    let _ = drain_notif_events(&ctx, PILOT);
    assert_eq!(
        rpc_call(&ctx, &mut s_owner, "structureDeployment", "Anchor", &[PyRep::Integer(TYPE_MERCENARY_DEN as i64), PyRep::Integer(skyhook_id), PyRep::Integer(temp_planet)])
            .await.unwrap_err().code,
        "MercenaryDenTooFarFromSkyhook"
    );
    let notifs_inv = ctx.drain_notifications_for_char(PILOT);
    let inv_ev = notifs_inv.iter().find(|n| n.event == "OnMercenaryDenRegistrationInvalid").expect("OnMercenaryDenRegistrationInvalid");
    assert_eq!(inv_ev.args.first().and_then(|a| a.as_str()), Some("MercenaryDenTooFarFromSkyhook"));
    assert_eq!(inv_ev.args.get(1).and_then(|a| a.dict_get("itemID")).and_then(|v| v.as_i64()), Some(skyhook_id));

    let _ = ctx.systems.get_or_spawn(null_sys).spawn_ball(make_ball(PILOT_SHIP, 639, sh_pos + DVec3::new(3_000.0, 0.0, 0.0))).await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.sov_runtime.skyhook_planets.insert(skyhook_id, non_temp_planet);
    });
    assert_eq!(
        rpc_call(&ctx, &mut s_owner, "structureDeployment", "Anchor", &[PyRep::Integer(TYPE_MERCENARY_DEN as i64), PyRep::Integer(skyhook_id), PyRep::Integer(non_temp_planet)])
            .await.unwrap_err().code,
        "PlanetNotTemperate"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.sov_runtime.skyhook_planets.insert(skyhook_id, temp_planet);
    });

    let _ = drain_notif_events(&ctx, PILOT);
    let _ = drain_quasar(&mut rx);
    let den_rep = rpc_call(
        &ctx, &mut s_owner, "structureDeployment", "Anchor",
        &[PyRep::Integer(TYPE_MERCENARY_DEN as i64), PyRep::Integer(skyhook_id), PyRep::Integer(temp_planet)],
    ).await.expect("anchor Mercenary Den at Temperate Skyhook <=10km");
    let den_id = den_rep.dict_get("structureID").and_then(|v| v.as_i64()).unwrap();
    let notifs_succ = ctx.drain_notifications_for_char(PILOT);
    let succ_ev = notifs_succ.iter().find(|n| n.event == "OnMercenaryDenRegistrationSuccess").expect("OnMercenaryDenRegistrationSuccess");
    assert_eq!(succ_ev.args.first().and_then(|a| a.as_str()), Some("MercenaryDenRegistered"));
    assert_eq!(succ_ev.args.get(1).and_then(|a| a.dict_get("itemID")).and_then(|v| v.as_i64()), Some(den_id));
    let anchor_q = drain_quasar(&mut rx);
    assert!(anchor_q.iter().any(|(t, _)| t == "eve_public.sovereignty.mercenaryden.api.DefinitionsUpdatedNotice"));
    assert!(anchor_q.iter().any(|(t, _)| t == "eve_public.sovereignty.mercenaryden.activity.api.AddedNotice"));

    assert_eq!(
        rpc_call(&ctx, &mut s_owner, "structureDeployment", "Anchor", &[PyRep::Integer(TYPE_MERCENARY_DEN as i64), PyRep::Integer(skyhook_id), PyRep::Integer(temp_planet)])
            .await.unwrap_err().code,
        "MercenaryDenCapExceeded"
    );

    // 2. Anarchy rises over 25h without MTO completion, siphoning 500 workforce from host Hub/Skyhook.
    let base_wf = ctx.db.with_snapshot(|s| compute_hub_resources(s, &ctx.sde, hub_id).unwrap().workforce_available);
    ctx.advance_secs(25 * 3_600);
    assert_eq!(
        rpc(&ctx, &mut s_owner, "eve_public.sovereignty.mercenaryden.api.GetAsOwnerRequest", id_req(1, den_id as u64)).await.status_code,
        STATUS_OK
    );
    let siphoned_wf = ctx.db.with_snapshot(|s| compute_hub_resources(s, &ctx.sde, hub_id).unwrap().workforce_available);
    assert_eq!(siphoned_wf + 500, base_wf);

    // 3. Quasar MTO requests spawn live MTO combat target ball (`24690`), completed via live `dogmaIM.AddTarget` + `Activate`!
    assert_eq!(rpc(&ctx, &mut s_owner, "eve_public.sovereignty.mercenaryden.activity.api.GetCapacityRequest", Vec::new()).await.status_code, STATUS_OK);
    assert_eq!(rpc(&ctx, &mut s_owner, "eve_public.sovereignty.mercenaryden.activity.api.GetAllRequest", Vec::new()).await.status_code, STATUS_OK);
    let acts = rpc(&ctx, &mut s_owner, "eve_public.sovereignty.mercenaryden.activity.api.GetForMercenaryDenRequest", id_req(1, den_id as u64))
        .await.payload.unwrap().value;
    let first_act = fields(&acts, 1).last().unwrap().clone();
    let mut start_req = Vec::new();
    encode_bytes_field(1, &field(&first_act, 1).unwrap(), &mut start_req);
    let _ = drain_quasar(&mut rx);
    assert_eq!(rpc(&ctx, &mut s_owner, "eve_public.sovereignty.mercenaryden.activity.api.StartRequest", start_req).await.status_code, STATUS_OK);
    assert!(drain_quasar(&mut rx).iter().any(|(t, _)| t == "eve_public.sovereignty.mercenaryden.activity.api.StartedNotice"));

    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    let mto_ball_id = ctx.db.with_snapshot(|s| s.sov_runtime.mto_site_balls.keys().copied().next().expect("MTO ball"));
    let mto_pos = ctx.systems.get_or_spawn(null_sys).get_ball_snapshot(mto_ball_id).await.unwrap().unwrap().1.position;

    fit_siege_artillery(&ctx, PILOT, PILOT_SHIP, 992_000);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            992_050,
            InventoryItemRecord {
                item_id: 992_050,
                type_id: 3_616, // Capital Remote Shield Booster I (960 HP/cycle)
                owner_id: PILOT,
                location_id: PILOT_SHIP,
                flag: InventoryFlag::HiSlot3,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(992_050, true);
    });
    ctx.invalidate_ship_fitting(PILOT_SHIP);
    let cs_owner = ctx.build_ship_combat_state(PILOT, PILOT_SHIP, 19_722);
    let _ = ctx.systems.get_or_spawn(null_sys).spawn_ball_with_combat(make_ball(PILOT_SHIP, 19_722, mto_pos + DVec3::new(2_500.0, 0.0, 0.0)), cs_owner).await;
    lock_target_via_rpc(&ctx, &mut s_owner, null_sys, PILOT_SHIP, mto_ball_id).await;
    let _ = drain_quasar(&mut rx);
    for gun in [992_000i64, 992_002, 992_004] {
        ctx.set_invention_roll_override(Some(0.05));
        let shot = rpc_call(&ctx, &mut s_owner, "dogmaIM", "Activate", &[PyRep::Integer(gun), PyRep::Integer(mto_ball_id)])
            .await
            .expect("dogmaIM.Activate turret on MTO ball");
        if shot.dict_get("destroyed") == Some(&PyRep::Bool(true))
            || ctx.db.with_snapshot(|s| !s.sov_runtime.mto_site_balls.contains_key(&mto_ball_id))
        {
            break;
        }
    }
    let mto_q = drain_quasar(&mut rx);
    assert!(mto_q.iter().any(|(t, _)| t == "eve_public.sovereignty.mercenaryden.activity.api.CompletedNotice"));
    assert!(mto_q.iter().any(|(t, _)| t == "eve_public.sovereignty.mercenaryden.api.EvolutionStateChangedNotice"));
    let restored_wf = ctx.db.with_snapshot(|s| compute_hub_resources(s, &ctx.sde, hub_id).unwrap().workforce_available);
    assert_eq!(restored_wf, base_wf);

    // 4. Live `beyonce.CallComponentFromClient` (`mercenaryDen.extract_infomorphs_to_ship`) deducts 20% sov tax.
    let den_pos = ctx.systems.get_or_spawn(null_sys).get_ball_snapshot(den_id).await.unwrap().unwrap().1.position;
    let cs_near = ctx.build_ship_combat_state(PILOT, PILOT_SHIP, 19_722);
    let _ = ctx.systems.get_or_spawn(null_sys).spawn_ball_with_combat(make_ball(PILOT_SHIP, 19_722, den_pos + DVec3::new(2_000.0, 0.0, 0.0)), cs_near).await;
    let ext_rep = rpc_call(
        &ctx, &mut s_owner, "beyonce", "CallComponentFromClient",
        &[PyRep::Integer(den_id), PyRep::String("mercenaryDen".into()), PyRep::String("extract_infomorphs_to_ship".into())],
    ).await.expect("extract_infomorphs_to_ship");
    let owner_share = ext_rep.dict_get("ownerShare").and_then(|v| v.as_i64()).unwrap();
    let tax_share = ext_rep.dict_get("taxShare").and_then(|v| v.as_i64()).unwrap();
    assert!(owner_share > 0 && tax_share > 0 && tax_share * 4 == owner_share);

    // 5. Hostile live combat weapon fire reinforces Den (24h + 6h window), Remote Shield Booster repairs <50% then >=50%, second kill drops ~50% stacks!
    let atk_cid = 90_000_088i64;
    let atk_ship = 100_000_088i64;
    let mut s_atk = register_online_pilot(
        &ctx, 72_010, "demo2", "demo2-secret", atk_cid, 98_000_088, Some(99_000_088),
        null_sys, atk_ship, 639, den_pos + DVec3::new(3_000.0, 0.0, 0.0), 0,
    ).await;
    fit_siege_artillery(&ctx, atk_cid, atk_ship, 993_000);
    lock_target_via_rpc(&ctx, &mut s_atk, null_sys, atk_ship, den_id).await;
    for _ in 0..6 {
        for gun_id in [993_000, 993_002, 993_004] {
            let _ = rpc_call(&ctx, &mut s_atk, "dogmaIM", "Activate", &[PyRep::Integer(gun_id), PyRep::String("targetAttack".into()), PyRep::Integer(den_id)]).await;
        }
        step_weapon_ticks(&ctx, null_sys).await;
        if ctx.db.with_snapshot(|s| s.structures[&den_id].state == "HullReinforced") {
            break;
        }
        ctx.advance_secs(25);
    }
    assert_eq!(ctx.db.with_snapshot(|s| s.structures[&den_id].state.clone()), "HullReinforced");
    let (reinf_until, vuln_ends) = ctx.db.with_snapshot(|snap| {
        let (r, v, _) = snap.sov_runtime.mercenary_den_reinforcement[&den_id];
        (r, v)
    });
    assert_eq!(reinf_until - ctx.now_unix(), MERC_DEN_REINFORCEMENT_SECS);
    assert_eq!(vuln_ends - reinf_until, MERC_DEN_VULNERABILITY_WINDOW_SECS);
    assert_eq!(rpc(&ctx, &mut s_owner, "eve_public.sovereignty.mercenaryden.api.GetAsOwnerRequest", id_req(1, den_id as u64)).await.status_code, STATUS_CONFLICT);

    ctx.advance_secs(MERC_DEN_REINFORCEMENT_SECS + 60);
    lock_target_via_rpc(&ctx, &mut s_owner, null_sys, PILOT_SHIP, den_id).await;
    for step_idx in 0..2 {
        let rep = rpc_call(&ctx, &mut s_owner, "dogmaIM", "Activate", &[PyRep::Integer(992_050), PyRep::String("shieldTransfer".into()), PyRep::Integer(den_id)])
            .await.expect("Remote Shield Booster cycle <50%");
        if step_idx == 1 {
            assert_eq!(rep.dict_get("denEnabled"), Some(&PyRep::Bool(false)));
        }
        ctx.advance_secs(25);
    }
    let rep_ok = rpc_call(&ctx, &mut s_owner, "dogmaIM", "Activate", &[PyRep::Integer(992_050), PyRep::String("shieldTransfer".into()), PyRep::Integer(den_id)])
        .await.expect("Remote Shield Booster cycle >=50%");
    assert_eq!(rep_ok.dict_get("denEnabled"), Some(&PyRep::Bool(true)));

    ctx.db.with_mut_snapshot(|snap| {
        for idx in 0..8i64 {
            snap.items.insert(
                994_000 + idx,
                InventoryItemRecord {
                    item_id: 994_000 + idx,
                    type_id: TYPE_INFOMORPH_DECRYPTION_KEY,
                    owner_id: PILOT,
                    location_id: den_id,
                    flag: InventoryFlag::Hangar,
                    quantity: 10,
                    ..Default::default()
                },
            );
        }
    });
    step_weapon_ticks(&ctx, null_sys).await;
    lock_target_via_rpc(&ctx, &mut s_atk, null_sys, atk_ship, den_id).await;
    for _ in 0..6 {
        for gun_id in [993_000, 993_002, 993_004] {
            let _ = rpc_call(&ctx, &mut s_atk, "dogmaIM", "Activate", &[PyRep::Integer(gun_id), PyRep::String("targetAttack".into()), PyRep::Integer(den_id)]).await;
        }
        step_weapon_ticks(&ctx, null_sys).await;
        if ctx.db.with_snapshot(|s| s.structures[&den_id].state == "HullReinforced") {
            break;
        }
        ctx.advance_secs(25);
    }
    assert_eq!(ctx.db.with_snapshot(|s| s.structures[&den_id].state.clone()), "HullReinforced");
    ctx.advance_secs(MERC_DEN_REINFORCEMENT_SECS + 120);
    let _ = drain_notif_events(&ctx, PILOT);
    lock_target_via_rpc(&ctx, &mut s_atk, null_sys, atk_ship, den_id).await;
    for _ in 0..6 {
        for gun_id in [993_000, 993_002, 993_004] {
            let _ = rpc_call(&ctx, &mut s_atk, "dogmaIM", "Activate", &[PyRep::Integer(gun_id), PyRep::String("targetAttack".into()), PyRep::Integer(den_id)]).await;
        }
        step_weapon_ticks(&ctx, null_sys).await;
        if ctx.db.with_snapshot(|s| s.structures[&den_id].state == "Destroyed") {
            break;
        }
        ctx.advance_secs(25);
    }
    assert_eq!(ctx.db.with_snapshot(|s| s.structures[&den_id].state.clone()), "Destroyed");
    let notifs_rem = ctx.drain_notifications_for_char(PILOT);
    let rem_ev = notifs_rem.iter().find(|n| n.event == "OnMyMercenaryDenRemoved").expect("OnMyMercenaryDenRemoved");
    assert_eq!(rem_ev.args.first().and_then(|a| a.as_str()), Some("MercenaryDenRemoved"));
    assert_eq!(rem_ev.args.get(1).and_then(|a| a.dict_get("itemID")).and_then(|v| v.as_i64()), Some(den_id));
    let dropped_stacks = ctx.db.with_snapshot(|snap| {
        snap.items.values().filter(|i| i.type_id == TYPE_INFOMORPH_DECRYPTION_KEY && i.quantity > 0 && i.location_id != den_id).count()
    });
    assert!((1..=7).contains(&dropped_stacks));
}
