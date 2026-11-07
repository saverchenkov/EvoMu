//! Integration tests for Sovereignty Mercenary Dens and Skyhook Reagents (`feat:sovereignty.*`).

#![forbid(unsafe_code)]

use crate::quasar_sov_equinox_hub::{field, fields, id_req, rpc, structure, subscribe, varint};
use crate::sovereignty_equinox_gameplay::{
    drain_notif_events, drain_quasar, find_nullsec_planets, fit_siege_artillery,
    lock_target_via_rpc, make_ball, register_online_pilot, rpc_call, step_weapon_ticks,
    ALLIANCE, CORP, PILOT, PILOT_SHIP,
};
use eve_db::{corp_roles, InventoryFlag, InventoryItemRecord, TrainedSkillRecord};
use eve_proto::quasar::encode_bytes_field;
use eve_proto::PyRep;
use eve_server::quasar_gateway::{STATUS_CONFLICT, STATUS_OK};
use eve_server::router::ServerContext;
use eve_server::services::quasar_sov_hub::compute_hub_resources;
use eve_server::services::quasar_sov_mercden::TYPE_MERCENARY_DEN_MANAGEMENT;
use eve_server::services::sov_gameplay::{
    MERC_DEN_REINFORCEMENT_SECS, MERC_DEN_VULNERABILITY_WINDOW_SECS, SKYHOOK_MATURITY_CYCLE_SECS,
    SKYHOOK_SILO_LINK_DURATION_SECS, TYPE_INFOMORPH_DECRYPTION_KEY, TYPE_MERCENARY_DEN,
};
use eve_server::services::sov_services::{
    TYPE_MAGMATIC_GAS, TYPE_ORBITAL_SKYHOOK, TYPE_SOVEREIGNTY_HUB, TYPE_SUPERIONIC_ICE,
};
use glam::DVec3;

#[tokio::test]
async fn test_mercenary_den_anchoring_skill_cap_distance_and_sov_tax() {
    // parity: feat:sovereignty.mercenary-den-anchoring
    eve_testkit::attest(&["feat:sovereignty.mercenary-den-anchoring"]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let (null_sys, temp_planet, _, _, non_temp_planet) = find_nullsec_planets(&ctx);
    let hub_id = 1_050_000_001i64;
    let skyhook_id = 1_050_000_002i64;
    let sh_pos = DVec3::new(200_000.0, 0.0, 0.0);

    let mut s_owner = register_online_pilot(
        &ctx, 72_001, "demo", "demo-secret", PILOT, CORP, Some(ALLIANCE),
        null_sys, PILOT_SHIP, 639, sh_pos + DVec3::new(3_000.0, 0.0, 0.0), corp_roles::ROLE_DIRECTOR,
    ).await;
    let _rx = subscribe(&ctx, &mut s_owner).await;

    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states.entry(null_sys).or_default().sov_alliance_id = Some(ALLIANCE);
        snap.structures.insert(hub_id, structure(hub_id, CORP, null_sys, TYPE_SOVEREIGNTY_HUB));
        snap.structures.insert(skyhook_id, structure(skyhook_id, CORP, null_sys, TYPE_ORBITAL_SKYHOOK));
        snap.sov_runtime.skyhook_planets.insert(skyhook_id, temp_planet);
        snap.skills.entry(PILOT).or_default().remove(&TYPE_MERCENARY_DEN_MANAGEMENT);
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

    // 1. Without Mercenary Den Management skill, anchoring fails with MercenaryDenCapExceeded
    assert_eq!(
        rpc_call(&ctx, &mut s_owner, "structureDeployment", "Anchor", &[
            PyRep::Integer(TYPE_MERCENARY_DEN as i64),
            PyRep::Integer(skyhook_id),
            PyRep::Integer(temp_planet),
        ]).await.unwrap_err().code,
        "MercenaryDenCapExceeded"
    );

    // Train Mercenary Den Management level 1 (allows 1 den max)
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(PILOT).or_default().insert(
            TYPE_MERCENARY_DEN_MANAGEMENT,
            TrainedSkillRecord { type_id: TYPE_MERCENARY_DEN_MANAGEMENT, level: 1, skill_points: 25_000.0 },
        );
    });

    // 2. Distance check: ship > 10 km from Skyhook fails MercenaryDenTooFarFromSkyhook and emits OnMercenaryDenRegistrationInvalid
    let _ = ctx.systems.get_or_spawn(null_sys).spawn_ball(make_ball(PILOT_SHIP, 639, sh_pos + DVec3::new(50_000.0, 0.0, 0.0))).await;
    let _ = drain_notif_events(&ctx, PILOT);
    assert_eq!(
        rpc_call(&ctx, &mut s_owner, "structureDeployment", "Anchor", &[
            PyRep::Integer(TYPE_MERCENARY_DEN as i64),
            PyRep::Integer(skyhook_id),
            PyRep::Integer(temp_planet),
        ]).await.unwrap_err().code,
        "MercenaryDenTooFarFromSkyhook"
    );
    let notifs_inv = ctx.drain_notifications_for_char(PILOT);
    let inv_ev = notifs_inv.iter().find(|n| n.event == "OnMercenaryDenRegistrationInvalid").expect("OnMercenaryDenRegistrationInvalid");
    assert_eq!(inv_ev.args.first().and_then(|a| a.as_str()), Some("MercenaryDenTooFarFromSkyhook"));

    // Move ship within 3 km of Skyhook
    let _ = ctx.systems.get_or_spawn(null_sys).spawn_ball(make_ball(PILOT_SHIP, 639, sh_pos + DVec3::new(3_000.0, 0.0, 0.0))).await;

    // 3. Planet check: explicit planet mismatch fails with DenPlanetMismatch
    assert_eq!(
        rpc_call(&ctx, &mut s_owner, "structureDeployment", "Anchor", &[
            PyRep::Integer(TYPE_MERCENARY_DEN as i64),
            PyRep::Integer(skyhook_id),
            PyRep::Integer(non_temp_planet),
        ]).await.unwrap_err().code,
        "DenPlanetMismatch"
    );

    // Bound non-temperate planet fails with PlanetNotTemperate
    ctx.db.with_mut_snapshot(|snap| {
        snap.sov_runtime.skyhook_planets.insert(skyhook_id, non_temp_planet);
    });
    assert_eq!(
        rpc_call(&ctx, &mut s_owner, "structureDeployment", "Anchor", &[
            PyRep::Integer(TYPE_MERCENARY_DEN as i64),
            PyRep::Integer(skyhook_id),
            PyRep::Integer(non_temp_planet),
        ]).await.unwrap_err().code,
        "PlanetNotTemperate"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.sov_runtime.skyhook_planets.insert(skyhook_id, temp_planet);
    });

    // 4. Valid anchoring within 10 km of Temperate Skyhook succeeds
    let den_rep = rpc_call(
        &ctx, &mut s_owner, "structureDeployment", "Anchor",
        &[PyRep::Integer(TYPE_MERCENARY_DEN as i64), PyRep::Integer(skyhook_id), PyRep::Integer(temp_planet)],
    ).await.expect("anchor Mercenary Den");
    let den_id = den_rep.dict_get("structureID").and_then(|v| v.as_i64()).unwrap();

    // Verify personal character ownership (owned by character, not corporation)
    let (owner_char, owner_corp) = ctx.db.with_snapshot(|snap| {
        let den = snap.sov_runtime.mercenary_dens.get(&den_id).expect("den in sov_runtime");
        let st = snap.structures.get(&den_id).expect("structure record");
        (den.owner_char_id, st.owner_corp_id)
    });
    assert_eq!(owner_char, PILOT);
    assert_eq!(owner_corp, 0);

    let notifs_succ = ctx.drain_notifications_for_char(PILOT);
    assert!(notifs_succ.iter().any(|n| n.event == "OnMercenaryDenRegistrationSuccess"));

    // 5. Attempting to anchor a second den when skill level is 1 fails with MercenaryDenCapExceeded
    assert_eq!(
        rpc_call(&ctx, &mut s_owner, "structureDeployment", "Anchor", &[
            PyRep::Integer(TYPE_MERCENARY_DEN as i64),
            PyRep::Integer(skyhook_id),
            PyRep::Integer(temp_planet),
        ]).await.unwrap_err().code,
        "MercenaryDenCapExceeded"
    );

    // 6. Sov owner charges tax on Skyhook Mercenary Den
    let tax_rep = rpc_call(&ctx, &mut s_owner, "sovMgr", "ConfirmSkyhookSovManagement", &[
        PyRep::Integer(skyhook_id),
        PyRep::Integer(20),
    ]).await.expect("set tax");
    assert_eq!(tax_rep.dict_get("taxPercent").and_then(|v| v.as_i64()), Some(20));

    // Place ship near den and extract infomorphs: sov tax is deducted
    let den_pos = ctx.systems.get_or_spawn(null_sys).get_ball_snapshot(den_id).await.unwrap().unwrap().1.position;
    let _ = ctx.systems.get_or_spawn(null_sys).spawn_ball(make_ball(PILOT_SHIP, 19_722, den_pos + DVec3::new(2_000.0, 0.0, 0.0))).await;

    ctx.advance_secs(3_600); // 1 hour -> generates infomorphs
    let ext_rep = rpc_call(
        &ctx, &mut s_owner, "beyonce", "CallComponentFromClient",
        &[PyRep::Integer(den_id), PyRep::String("mercenaryDen".into()), PyRep::String("extract_infomorphs_to_ship".into())],
    ).await.expect("extract_infomorphs_to_ship");
    let owner_share = ext_rep.dict_get("ownerShare").and_then(|v| v.as_i64()).unwrap();
    let tax_share = ext_rep.dict_get("taxShare").and_then(|v| v.as_i64()).unwrap();
    assert!(owner_share > 0);
    assert_eq!(tax_share * 4, owner_share);
}

#[tokio::test]
async fn test_mercenary_den_development_anarchy_mto_workforce_siphon() {
    // parity: feat:sovereignty.mercenary-den-development-anarchy
    // parity: feat:sovereignty.mercenary-den-anarchy-evolution
    eve_testkit::attest(&[
        "feat:sovereignty.mercenary-den-development-anarchy",
        "feat:sovereignty.mercenary-den-anarchy-evolution",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let (null_sys, temp_planet, _, _, _) = find_nullsec_planets(&ctx);
    let hub_id = 1_060_000_001i64;
    let skyhook_id = 1_060_000_002i64;
    let sh_pos = DVec3::new(200_000.0, 0.0, 0.0);

    let mut s_owner = register_online_pilot(
        &ctx, 73_001, "demo", "demo-secret", PILOT, CORP, Some(ALLIANCE),
        null_sys, PILOT_SHIP, 639, sh_pos + DVec3::new(3_000.0, 0.0, 0.0), corp_roles::ROLE_DIRECTOR,
    ).await;
    let mut rx = subscribe(&ctx, &mut s_owner).await;

    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states.entry(null_sys).or_default().sov_alliance_id = Some(ALLIANCE);
        snap.structures.insert(hub_id, structure(hub_id, CORP, null_sys, TYPE_SOVEREIGNTY_HUB));
        snap.structures.insert(skyhook_id, structure(skyhook_id, CORP, null_sys, TYPE_ORBITAL_SKYHOOK));
        snap.sov_runtime.skyhook_planets.insert(skyhook_id, temp_planet);
        snap.skills.entry(PILOT).or_default().insert(
            TYPE_MERCENARY_DEN_MANAGEMENT,
            TrainedSkillRecord { type_id: TYPE_MERCENARY_DEN_MANAGEMENT, level: 3, skill_points: 100_000.0 },
        );
        snap.items.insert(
            990_301,
            InventoryItemRecord {
                item_id: 990_301,
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
    let _ = ctx.systems.get_or_spawn(null_sys).spawn_ball(make_ball(PILOT_SHIP, 639, sh_pos + DVec3::new(3_000.0, 0.0, 0.0))).await;

    let base_wf = ctx.db.with_snapshot(|s| compute_hub_resources(s, &ctx.sde, hub_id).unwrap().workforce_available);
    assert!(base_wf > 0);

    let den_rep = rpc_call(
        &ctx, &mut s_owner, "structureDeployment", "Anchor",
        &[PyRep::Integer(TYPE_MERCENARY_DEN as i64), PyRep::Integer(skyhook_id), PyRep::Integer(temp_planet)],
    ).await.expect("anchor Mercenary Den");
    let den_id = den_rep.dict_get("structureID").and_then(|v| v.as_i64()).unwrap();
    let _ = drain_quasar(&mut rx);

    // 1. Stage 1 (Development 0): planet-scaled IDK/hr, 0% workforce disruption
    let exp_rate1 = eve_server::services::quasar_sov_mercden::planet_scaled_infomorphs_per_hour(
        Some(&ctx.sde),
        null_sys,
        temp_planet,
        1,
    );
    ctx.advance_secs(3_600);
    let d_1h = rpc(&ctx, &mut s_owner, "eve_public.sovereignty.mercenaryden.api.GetAsOwnerRequest", id_req(1, den_id as u64))
        .await.payload.unwrap().value;
    let info_1h = field(&field(&d_1h, 4).unwrap(), 2).unwrap();
    assert_eq!(varint(&info_1h, 1), Some(u64::from(exp_rate1)));
    assert_eq!(ctx.db.with_snapshot(|s| compute_hub_resources(s, &ctx.sde, hub_id).unwrap().workforce_available), base_wf);

    // 2. Advance 8 days: Development +40, Anarchy +40 (UI Anarchy level 2 / Stage 3)
    // Den siphons 10% of host Skyhook's planetary workforce (150 of 1,500)
    ctx.advance_secs(8 * 86_400 - 3_600);
    let _ = rpc(&ctx, &mut s_owner, "eve_public.sovereignty.mercenaryden.api.GetAsOwnerRequest", id_req(1, den_id as u64)).await;
    let siphoned_wf = ctx.db.with_snapshot(|s| compute_hub_resources(s, &ctx.sde, hub_id).unwrap().workforce_available);
    assert_eq!(siphoned_wf + 150, base_wf);

    // Higher development raises hourly IDK yield (Stage 3 = 71 IDK/hr)
    let d_day8 = rpc(&ctx, &mut s_owner, "eve_public.sovereignty.mercenaryden.api.GetAsOwnerRequest", id_req(1, den_id as u64))
        .await.payload.unwrap().value;
    let evo_day8 = field(&d_day8, 3).unwrap();
    let sim_day8 = field(&evo_day8, 2).unwrap();
    assert_eq!(varint(&field(&sim_day8, 3).unwrap(), 1), Some(40));
    assert_eq!(varint(&field(&sim_day8, 4).unwrap(), 1), Some(40));

    // 3. Completing MTO raises Development (+10 -> 50) and lowers Anarchy (-10 -> 30, Stage 2 = 0% disruption)
    let acts = rpc(&ctx, &mut s_owner, "eve_public.sovereignty.mercenaryden.activity.api.GetForMercenaryDenRequest", id_req(1, den_id as u64))
        .await.payload.unwrap().value;
    let first_act = fields(&acts, 1).into_iter().next().unwrap();
    let mut start_req = Vec::new();
    encode_bytes_field(1, &field(&first_act, 1).unwrap(), &mut start_req);
    assert_eq!(
        rpc(&ctx, &mut s_owner, "eve_public.sovereignty.mercenaryden.activity.api.StartRequest", start_req).await.status_code,
        STATUS_OK
    );

    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    let mto_ball_id = ctx.db.with_snapshot(|s| s.sov_runtime.mto_site_balls.keys().copied().next().expect("MTO site ball"));
    let mto_pos = ctx.systems.get_or_spawn(null_sys).get_ball_snapshot(mto_ball_id).await.unwrap().unwrap().1.position;

    fit_siege_artillery(&ctx, PILOT, PILOT_SHIP, 992_000);
    let cs_owner = ctx.build_ship_combat_state(PILOT, PILOT_SHIP, 19_722);
    let _ = ctx.systems.get_or_spawn(null_sys).spawn_ball_with_combat(make_ball(PILOT_SHIP, 19_722, mto_pos + DVec3::new(2_500.0, 0.0, 0.0)), cs_owner).await;
    lock_target_via_rpc(&ctx, &mut s_owner, null_sys, PILOT_SHIP, mto_ball_id).await;

    for _ in 0..8 {
        for gun in [992_000i64, 992_002, 992_004] {
            ctx.set_invention_roll_override(Some(0.005));
            let shot = rpc_call(&ctx, &mut s_owner, "dogmaIM", "Activate", &[PyRep::Integer(gun), PyRep::Integer(mto_ball_id)]).await.unwrap();
            if shot.dict_get("destroyed") == Some(&PyRep::Bool(true))
                || ctx.db.with_snapshot(|s| !s.sov_runtime.mto_site_balls.contains_key(&mto_ball_id))
            {
                break;
            }
        }
        if ctx.db.with_snapshot(|s| !s.sov_runtime.mto_site_balls.contains_key(&mto_ball_id)) {
            break;
        }
        step_weapon_ticks(&ctx, null_sys).await;
    }

    let mto_notifs = drain_quasar(&mut rx);
    assert!(mto_notifs.iter().any(|(t, _)| t == "eve_public.sovereignty.mercenaryden.activity.api.CompletedNotice"));

    // Verify development raised (+10 -> 50) and anarchy lowered (-10 -> 30)
    let d_post_mto = rpc(&ctx, &mut s_owner, "eve_public.sovereignty.mercenaryden.api.GetAsOwnerRequest", id_req(1, den_id as u64))
        .await.payload.unwrap().value;
    let evo_post = field(&d_post_mto, 3).unwrap();
    let sim_post = field(&evo_post, 2).unwrap();
    assert_eq!(varint(&field(&sim_post, 3).unwrap(), 1), Some(50));
    assert_eq!(varint(&field(&sim_post, 4).unwrap(), 1), Some(30));

    // Workforce output fully restored to sov owner!
    let restored_wf = ctx.db.with_snapshot(|s| compute_hub_resources(s, &ctx.sde, hub_id).unwrap().workforce_available);
    assert_eq!(restored_wf, base_wf);
}

#[tokio::test]
async fn test_mercenary_den_reinforcement_repair_and_destruction_loot() {
    // parity: feat:sovereignty.mercenary-den-reinforcement-loot
    eve_testkit::attest(&["feat:sovereignty.mercenary-den-reinforcement-loot"]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let (null_sys, temp_planet, _, _, _) = find_nullsec_planets(&ctx);
    let hub_id = 1_070_000_001i64;
    let skyhook_id = 1_070_000_002i64;
    let sh_pos = DVec3::new(200_000.0, 0.0, 0.0);

    let mut s_owner = register_online_pilot(
        &ctx, 74_001, "demo", "demo-secret", PILOT, CORP, Some(ALLIANCE),
        null_sys, PILOT_SHIP, 19_722, sh_pos + DVec3::new(3_000.0, 0.0, 0.0), corp_roles::ROLE_DIRECTOR,
    ).await;
    let _rx = subscribe(&ctx, &mut s_owner).await;

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
            990_401,
            InventoryItemRecord {
                item_id: 990_401,
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
    let _ = ctx.systems.get_or_spawn(null_sys).spawn_ball(make_ball(PILOT_SHIP, 639, sh_pos + DVec3::new(3_000.0, 0.0, 0.0))).await;

    let den_rep = rpc_call(
        &ctx, &mut s_owner, "structureDeployment", "Anchor",
        &[PyRep::Integer(TYPE_MERCENARY_DEN as i64), PyRep::Integer(skyhook_id), PyRep::Integer(temp_planet)],
    ).await.expect("anchor Mercenary Den");
    let den_id = den_rep.dict_get("structureID").and_then(|v| v.as_i64()).unwrap();
    let den_pos = ctx.systems.get_or_spawn(null_sys).get_ball_snapshot(den_id).await.unwrap().unwrap().1.position;

    // 1. Attacking the den: hostile weapon fire reduces shield to 0 -> HullReinforced
    let atk_cid = 90_000_089i64;
    let atk_ship = 100_000_089i64;
    let mut s_atk = register_online_pilot(
        &ctx, 74_010, "demo2", "demo2-secret", atk_cid, 98_000_089, Some(99_000_089),
        null_sys, atk_ship, 639, den_pos + DVec3::new(3_000.0, 0.0, 0.0), 0,
    ).await;
    fit_siege_artillery(&ctx, atk_cid, atk_ship, 993_100);
    lock_target_via_rpc(&ctx, &mut s_atk, null_sys, atk_ship, den_id).await;

    for _ in 0..6 {
        for gun in [993_100i64, 993_102, 993_104] {
            let _ = rpc_call(&ctx, &mut s_atk, "dogmaIM", "Activate", &[PyRep::Integer(gun), PyRep::String("targetAttack".into()), PyRep::Integer(den_id)]).await;
        }
        step_weapon_ticks(&ctx, null_sys).await;
        if ctx.db.with_snapshot(|s| s.structures[&den_id].state == "HullReinforced") {
            break;
        }
        ctx.advance_secs(25);
    }
    assert_eq!(ctx.db.with_snapshot(|s| s.structures[&den_id].state.clone()), "HullReinforced");

    // Reinforcement timer is 24h with 6h vulnerability window
    let (reinf_until, vuln_ends) = ctx.db.with_snapshot(|snap| {
        let (r, v, _) = snap.sov_runtime.mercenary_den_reinforcement[&den_id];
        (r, v)
    });
    assert_eq!(reinf_until - ctx.now_unix(), MERC_DEN_REINFORCEMENT_SECS);
    assert_eq!(vuln_ends - reinf_until, MERC_DEN_VULNERABILITY_WINDOW_SECS);

    // 2. While reinforced, den is inaccessible (GetAsOwnerRequest returns STATUS_CONFLICT)
    assert_eq!(
        rpc(&ctx, &mut s_owner, "eve_public.sovereignty.mercenaryden.api.GetAsOwnerRequest", id_req(1, den_id as u64)).await.status_code,
        STATUS_CONFLICT
    );

    // 3. Post-reinforcement: shields must be repaired to 50%
    ctx.advance_secs(MERC_DEN_REINFORCEMENT_SECS + 60);

    // Equip remote shield booster on pilot ship
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            992_150,
            InventoryItemRecord {
                item_id: 992_150,
                type_id: 3_616, // Capital Remote Shield Booster I
                owner_id: PILOT,
                location_id: PILOT_SHIP,
                flag: InventoryFlag::HiSlot3,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(992_150, true);
    });
    ctx.invalidate_ship_fitting(PILOT_SHIP);
    let cs_owner = ctx.build_ship_combat_state(PILOT, PILOT_SHIP, 19_722);
    let _ = ctx.systems.get_or_spawn(null_sys).set_combat_state(PILOT_SHIP, cs_owner).await;
    let _ = ctx.systems.get_or_spawn(null_sys).set_capacitor(PILOT_SHIP, 50_000.0).await;

    lock_target_via_rpc(&ctx, &mut s_owner, null_sys, PILOT_SHIP, den_id).await;
    // Step 1: Repair < 50% keeps denEnabled == false
    for step_idx in 0..2 {
        let rep = rpc_call(&ctx, &mut s_owner, "dogmaIM", "Activate", &[PyRep::Integer(992_150), PyRep::String("shieldTransfer".into()), PyRep::Integer(den_id)])
            .await.expect("Remote Shield Booster cycle <50%");
        if step_idx == 1 {
            assert_eq!(rep.dict_get("denEnabled"), Some(&PyRep::Bool(false)));
        }
        ctx.advance_secs(25);
    }

    // Step 2: Repair >= 50% re-enables den
    let rep_ok = rpc_call(&ctx, &mut s_owner, "dogmaIM", "Activate", &[PyRep::Integer(992_150), PyRep::String("shieldTransfer".into()), PyRep::Integer(den_id)])
        .await.expect("Remote Shield Booster cycle >=50%");
    assert_eq!(rep_ok.dict_get("denEnabled"), Some(&PyRep::Bool(true)));

    // 4. Stored Infomorph Decryption Keys drop rate on destruction (50% drop chance per stack)
    ctx.db.with_mut_snapshot(|snap| {
        for idx in 0..8i64 {
            snap.items.insert(
                994_500 + idx,
                InventoryItemRecord {
                    item_id: 994_500 + idx,
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
        for gun in [993_100i64, 993_102, 993_104] {
            let _ = rpc_call(&ctx, &mut s_atk, "dogmaIM", "Activate", &[PyRep::Integer(gun), PyRep::String("targetAttack".into()), PyRep::Integer(den_id)]).await;
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
        for gun in [993_100i64, 993_102, 993_104] {
            let _ = rpc_call(&ctx, &mut s_atk, "dogmaIM", "Activate", &[PyRep::Integer(gun), PyRep::String("targetAttack".into()), PyRep::Integer(den_id)]).await;
        }
        step_weapon_ticks(&ctx, null_sys).await;
        if ctx.db.with_snapshot(|s| s.structures[&den_id].state == "Destroyed") {
            break;
        }
        ctx.advance_secs(25);
    }
    assert_eq!(ctx.db.with_snapshot(|s| s.structures[&den_id].state.clone()), "Destroyed");

    let notifs_rem = ctx.drain_notifications_for_char(PILOT);
    assert!(notifs_rem.iter().any(|n| n.event == "OnMyMercenaryDenRemoved"));

    let dropped_stacks = ctx.db.with_snapshot(|snap| {
        snap.items.values().filter(|i| i.type_id == TYPE_INFOMORPH_DECRYPTION_KEY && i.quantity > 0 && i.location_id != den_id).count()
    });
    assert!(dropped_stacks > 0);
}

#[tokio::test]
async fn test_skyhook_reagent_maturity_split_and_theft() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let (sys_id, _, lava_planet, ice_planet, _) = find_nullsec_planets(&ctx);
    let cels = ctx.sde.celestials_in_system(sys_id);
    let lava_pos = cels.iter().find(|c| c.celestial_id == lava_planet).unwrap().position;
    let ice_pos = cels.iter().find(|c| c.celestial_id == ice_planet).unwrap().position;

    let mut s_dir = register_online_pilot(
        &ctx, 75_001, "demo", "demo-secret", PILOT, CORP, Some(ALLIANCE),
        sys_id, PILOT_SHIP, 621, lava_pos, corp_roles::ROLE_DIRECTOR,
    ).await;
    let _rx = subscribe(&ctx, &mut s_dir).await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states.entry(sys_id).or_default().sov_alliance_id = Some(ALLIANCE);
        snap.items.insert(
            995_001,
            InventoryItemRecord {
                item_id: 995_001,
                type_id: TYPE_ORBITAL_SKYHOOK,
                owner_id: PILOT,
                location_id: PILOT_SHIP,
                flag: InventoryFlag::Cargo,
                quantity: 2,
                ..Default::default()
            },
        );
    });

    // 1. Anchor Lava Skyhook and Ice Skyhook via live structureDeployment.Anchor RPC
    let lava_rep = rpc_call(&ctx, &mut s_dir, "structureDeployment", "Anchor", &[
        PyRep::Integer(TYPE_ORBITAL_SKYHOOK as i64),
        PyRep::Integer(lava_planet),
    ]).await.expect("anchor Lava Skyhook");
    let lava_skyhook = lava_rep.dict_get("structureID").and_then(|v| v.as_i64()).unwrap();

    let _ = ctx.systems.get_or_spawn(sys_id).spawn_ball(make_ball(PILOT_SHIP, 621, ice_pos + DVec3::new(150_000.0, 0.0, 0.0))).await;
    let ice_rep = rpc_call(&ctx, &mut s_dir, "structureDeployment", "Anchor", &[
        PyRep::Integer(TYPE_ORBITAL_SKYHOOK as i64),
        PyRep::Integer(ice_planet),
    ]).await.expect("anchor Ice Skyhook");
    let ice_skyhook = ice_rep.dict_get("structureID").and_then(|v| v.as_i64()).unwrap();

    // 2. Extracted Magmatic Gas and Superionic Ice are split 50/50 between secure and insecure (maturing) bays
    let lava_rate = eve_server::services::quasar_sov_skyhook::planet_hourly_reagent_yield(
        &ctx.sde, Some(lava_planet), Some(sys_id), TYPE_MAGMATIC_GAS,
    );
    let ice_rate = eve_server::services::quasar_sov_skyhook::planet_hourly_reagent_yield(
        &ctx.sde, Some(ice_planet), Some(sys_id), TYPE_SUPERIONIC_ICE,
    );
    ctx.advance_secs(10 * 3_600);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;

    let (lava_sec_10h, lava_insec_10h) = (10 * lava_rate / 2, 10 * lava_rate - 10 * lava_rate / 2);
    let (ice_sec_10h, ice_insec_10h) = (10 * ice_rate / 2, 10 * ice_rate - 10 * ice_rate / 2);
    let (lava_type, lava_sec, lava_insec, _) = ctx.db.with_snapshot(|s| s.sov_runtime.skyhook_reagent_bays[&lava_skyhook]);
    let (ice_type, ice_sec, ice_insec, _) = ctx.db.with_snapshot(|s| s.sov_runtime.skyhook_reagent_bays[&ice_skyhook]);
    assert_eq!((lava_type, lava_sec, lava_insec), (TYPE_MAGMATIC_GAS, lava_sec_10h, lava_insec_10h));
    assert_eq!((ice_type, ice_sec, ice_insec), (TYPE_SUPERIONIC_ICE, ice_sec_10h, ice_insec_10h));

    // 3. Over the maturity cycle, insecure bay matures into secure bay
    // Deactivate Skyhook via Quasar RPC to isolate linear maturity without new harvest
    let deact_resp = rpc(&ctx, &mut s_dir, "eve_public.sovereignty.skyhook.api.DeactivateRequest", id_req(1, lava_skyhook as u64)).await;
    assert_eq!(deact_resp.status_code, STATUS_OK);
    let _ = rpc(&ctx, &mut s_dir, "eve_public.sovereignty.skyhook.api.DeactivateRequest", id_req(1, ice_skyhook as u64)).await;

    ctx.advance_secs(SKYHOOK_MATURITY_CYCLE_SECS / 2); // 12 hours -> half of maturing units transfer to secure
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;

    let lava_mat = lava_insec_10h / 2;
    let (_, lava_sec_mat, lava_insec_mat, _) = ctx.db.with_snapshot(|s| s.sov_runtime.skyhook_reagent_bays[&lava_skyhook]);
    assert_eq!((lava_sec_mat, lava_insec_mat), (lava_sec_10h + lava_mat, lava_insec_10h - lava_mat));

    // 4. Reagent theft: during theft vulnerability window with shield <= 10%, thief links and steals insecure bay
    let thief_cid = 90_000_078i64;
    let thief_ship = 100_000_078i64;
    let sky_ball_pos = ctx.systems.get_or_spawn(sys_id).get_ball_snapshot(lava_skyhook).await.unwrap().unwrap().1.position;
    let mut s_thief = register_online_pilot(
        &ctx, 75_010, "demo2", "demo2-secret", thief_cid, 98_000_078, Some(99_000_078),
        sys_id, thief_ship, 621, sky_ball_pos + DVec3::new(4_000.0, 0.0, 0.0), 0,
    ).await;
    fit_siege_artillery(&ctx, thief_cid, thief_ship, 991_000);

    // Full shield rejects link
    assert_eq!(
        rpc_call(&ctx, &mut s_thief, "beyonce", "CallComponentFromClient", &[
            PyRep::Integer(lava_skyhook),
            PyRep::String("linkWithShip".into()),
            PyRep::String("InitiateLink".into()),
        ]).await.unwrap_err().code,
        "SkyhookSiloShieldTooHigh"
    );

    // Damage skyhook shield to <= 10%
    lock_target_via_rpc(&ctx, &mut s_thief, sys_id, thief_ship, lava_skyhook).await;
    for _ in 0..6 {
        for gun in [991_000i64, 991_002, 991_004] {
            let _ = rpc_call(&ctx, &mut s_thief, "dogmaIM", "Activate", &[
                PyRep::Integer(gun),
                PyRep::String("targetAttack".into()),
                PyRep::Integer(lava_skyhook),
            ]).await;
        }
        step_weapon_ticks(&ctx, sys_id).await;
        if ctx.db.with_snapshot(|s| s.structures[&lava_skyhook].shield_hp) <= 1_000.0 {
            break;
        }
        ctx.advance_secs(25);
    }
    let notifs = ctx.drain_notifications_for_char(PILOT);
    assert!(notifs.iter().any(|n| n.event == "OnSkyhookCanBeHacked"));

    // Initiate link with ship
    let link_rep = rpc_call(
        &ctx, &mut s_thief, "beyonce", "CallComponentFromClient",
        &[PyRep::Integer(lava_skyhook), PyRep::String("linkWithShip".into()), PyRep::String("InitiateLink".into())],
    ).await.expect("InitiateLink");
    assert_eq!(link_rep.dict_get("linked"), Some(&PyRep::Bool(true)));

    let before_theft_sec = ctx.db.with_snapshot(|s| s.sov_runtime.skyhook_reagent_bays[&lava_skyhook].1);
    let before_theft_insec = ctx.db.with_snapshot(|s| s.sov_runtime.skyhook_reagent_bays[&lava_skyhook].2);
    assert_eq!(before_theft_sec + before_theft_insec, lava_sec_mat + lava_insec_mat);
    assert!(before_theft_sec >= lava_sec_mat);

    // Complete 10-minute link
    ctx.advance_secs(SKYHOOK_SILO_LINK_DURATION_SECS + 1);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;

    // Insecure bay is 100% stolen into in-space Cargo Container, secure bay is UNTOUCHED
    let (_, after_theft_sec, after_theft_insec, _) = ctx.db.with_snapshot(|s| s.sov_runtime.skyhook_reagent_bays[&lava_skyhook]);
    assert_eq!(after_theft_insec, 0, "100% of insecure bay stolen");
    let total_before = before_theft_sec + before_theft_insec;
    assert!(
        after_theft_sec >= before_theft_sec && after_theft_sec < total_before,
        "secure bay retains matured reagents ({before_theft_sec} <= {after_theft_sec} < {total_before})"
    );

    let balls = ctx.systems.get_or_spawn(sys_id).list_balls().await.unwrap();
    let can_ball = balls.iter().find(|b| b.type_id == 23).expect("Cargo Container 23 spawned in space");
    let stolen_qty: i32 = ctx.db.with_snapshot(|s| {
        s.items.values().filter(|i| i.location_id == can_ball.id && i.type_id == TYPE_MAGMATIC_GAS).map(|i| i.quantity).sum()
    });
    assert_eq!(stolen_qty, (total_before - after_theft_sec) as i32);

    // 5. Corporation Director extracts secured reagents to active ship cargo (within 15 km)
    let _ = ctx.systems.get_or_spawn(sys_id).spawn_ball(make_ball(PILOT_SHIP, 639, sky_ball_pos + DVec3::new(3_000.0, 0.0, 0.0))).await;
    let ext_rep = rpc_call(
        &ctx, &mut s_dir, "beyonce", "CallComponentFromClient",
        &[PyRep::Integer(lava_skyhook), PyRep::String("orbitalSkyhook".into()), PyRep::String("ExtractReagentsToShip".into())],
    ).await.expect("ExtractReagentsToShip");
    let ext_qty = ext_rep.dict_get("extractedQuantity").and_then(|v| v.as_i64()).unwrap();
    assert_eq!(ext_qty, after_theft_sec as i64);
    let ship_reagents: i32 = ctx.db.with_snapshot(|s| {
        s.items.values().filter(|i| i.location_id == PILOT_SHIP && i.type_id == TYPE_MAGMATIC_GAS).map(|i| i.quantity).sum()
    });
    assert_eq!(ship_reagents as i64, ext_qty);
}
