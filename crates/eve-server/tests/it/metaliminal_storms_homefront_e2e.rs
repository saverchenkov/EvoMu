//! Comprehensive remediation tests for Metaliminal Roaming Storms,
//! Roaming Weather Map RPC, and Homefront Operation Instance Caching
//! (`feat:pve.metaliminal-storms`, `rpc:map.GetAllRoamingWeatherSystems`, `rpc:dungeonInstanceCacheMgr.GetHomefrontSiteInstances`).

use eve_destiny::{Ball, DVec3, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::pve_special_sites::{
    advance_pve_special_clock, ensure_special_sites_spawned, with_pve_special_reg,
};
use std::collections::HashMap;

async fn setup_pilot_session(
    ctx: &ServerContext,
    session_id: u64,
    solar_system_id: u32,
    char_id: i64,
) -> SessionState {
    let kw = HashMap::new();
    let mut sess = SessionState::new(session_id);
    let (user, secret) = if char_id == 90_000_002 {
        ("other", "other-secret")
    } else {
        ("demo", "demo-secret")
    };
    ctx.dispatch_rpc(
        &mut sess,
        "authentication",
        None,
        "Login",
        &[PyRep::String(user.into()), PyRep::String(secret.into())],
        &kw,
        None,
    )
    .await
    .expect("Login");
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");

    sess.solar_system_id = Some(solar_system_id);
    sess.station_id = None;
    sess.structure_id = None;
    sess.ship_id = Some(char_id + 10_000_000);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = solar_system_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = char_id + 10_000_000;
        }
    });

    sess
}

#[tokio::test]
async fn test_metaliminal_storms_roaming_dogma_effects_and_sites() {
    // parity: feat:pve.metaliminal-storms, rpc:map.GetAllRoamingWeatherSystems
    eve_testkit::attest(&["feat:pve.metaliminal-storms"]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    ensure_special_sites_spawned(&ctx).await;

    let kw = HashMap::new();
    let mut sess = setup_pilot_session(&ctx, 8911, 30_002_904, 90_000_001).await;

    // 1. Verify map.GetAllRoamingWeatherSystems returns V24 shape with locationID and sceneType
    let storms_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "map",
            None,
            "GetAllRoamingWeatherSystems",
            &[],
            &kw,
            None,
        )
        .await
        .expect("GetAllRoamingWeatherSystems");
    let PyRep::List(storms) = &storms_rep else {
        panic!("Expected List from GetAllRoamingWeatherSystems");
    };
    assert!(!storms.is_empty(), "Must return active roaming weather systems");

    for entry in storms {
        let loc = entry.dict_get("locationID").and_then(|v| v.as_i64()).expect("locationID");
        let scene = entry.dict_get("sceneType").and_then(|v| v.as_i64()).expect("sceneType");
        assert!(loc > 0, "Location ID must be valid");
        assert!(scene == 1 || scene == 4, "sceneType must be 4 (core) or 1 (periphery)");
        let sec = ctx.sde.solar_systems.get(&(loc as u32)).map(|s| s.security_status).unwrap_or(1.0);
        assert!(sec < 0.45, "Metaliminal storm systems must only be lowsec/nullsec, got sec={sec}");
    }

    // 2. Verify initial storm centers are in nullsec/lowsec, not Jita (30000142)
    let initial_centers = with_pve_special_reg(&ctx, |reg| {
        reg.roaming_storms.iter().map(|s| (s.storm_id, s.center_solar_system_id, s.weather)).collect::<Vec<_>>()
    });
    assert_eq!(initial_centers.len(), 8, "Must have 8 roaming metaliminal storms");
    for (_, center, _) in &initial_centers {
        let sec = ctx.sde.solar_systems.get(center).map(|s| s.security_status).unwrap_or(1.0);
        assert!(sec < 0.45, "Storm center must be nullsec/lowsec, not highsec");
        assert_ne!(*center, 30_000_142, "Storm center must never be highsec Jita");
    }

    // 3. Verify system-wide environment dogma effects and bonus mining sites are applied
    let elec_center = initial_centers.iter().find(|(_, _, w)| *w == eve_server::services::pve_special_sites::AbyssalWeatherKind::Electrical).unwrap().1;
    let has_strong_beacon = ctx.db.with_snapshot(|snap| {
        snap.items.values().any(|it| it.location_id == elec_center as i64 && it.type_id == 47382)
    });
    assert!(has_strong_beacon, "Core electrical system must have strong weather beacon 47382");

    let has_volatile_ice = ctx.db.with_snapshot(|snap| {
        snap.cosmic_anomalies.get(&elec_center).into_iter().flatten().any(|a| a.anomaly_id.starts_with("DNG-9042-"))
    });
    assert!(has_volatile_ice, "Core storm system must spawn bonus Fresh Volatile Ice Field (9042)");

    // 4. Advance clock by 48h (172,800s): storms roam 1 jump along connected nullsec stargates
    advance_pve_special_clock(&ctx, 172_800);

    let storms_rep_after = ctx
        .dispatch_rpc(
            &mut sess,
            "map",
            None,
            "GetAllRoamingWeatherSystems",
            &[],
            &kw,
            None,
        )
        .await
        .expect("GetAllRoamingWeatherSystems after roam");
    let PyRep::List(storms_after) = &storms_rep_after else {
        panic!("Expected List");
    };
    assert!(!storms_after.is_empty());

    let moved_centers = with_pve_special_reg(&ctx, |reg| {
        reg.roaming_storms.iter().map(|s| (s.storm_id, s.center_solar_system_id)).collect::<Vec<_>>()
    });
    assert!(
        moved_centers.iter().any(|(id, new_c)| {
            let old_c = initial_centers.iter().find(|(sid, _, _)| sid == id).unwrap().1;
            *new_c != old_c
        }),
        "At least one storm center must have moved along stargates"
    );
}

#[tokio::test]
async fn test_homefront_site_instances_v24_registry_and_completion() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    ensure_special_sites_spawned(&ctx).await;

    let kw = HashMap::new();
    let jita = 30_000_142_u32;
    let mut sess = setup_pilot_session(&ctx, 8912, jita, 90_000_002).await;

    // 1. Query dungeonInstanceCacheMgr.GetHomefrontSiteInstances with V24 dict return shape
    let hf_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "dungeonInstanceCacheMgr",
            None,
            "GetHomefrontSiteInstances",
            &[PyRep::Integer(jita as i64)],
            &kw,
            None,
        )
        .await
        .expect("GetHomefrontSiteInstances for Jita");
    let PyRep::Dict(hf_dict) = &hf_rep else {
        panic!("Expected Dict from GetHomefrontSiteInstances");
    };

    let Some((_, PyRep::List(jita_dungeons))) = hf_dict.iter().find(|(k, _)| k.as_i64() == Some(jita as i64)) else {
        panic!("Dict must contain entry for queried solar system {jita}");
    };
    assert!(!jita_dungeons.is_empty(), "Jita must have seeded Homefront dungeons");

    let did = jita_dungeons[0].dict_get("dungeonID").and_then(|v| v.as_i64()).expect("dungeonID") as u32;
    let target_instance_id = jita_dungeons[0].dict_get("instanceID").and_then(|v| v.as_i64()).expect("instanceID");

    // SDE Archetype 70 verification: verify dungeon ID is from SDE archetype 70
    let d_archetype = ctx.sde.dungeons.get(&did).and_then(|d| d.archetype_id);
    assert_eq!(d_archetype, Some(70), "Homefront dungeon must belong to SDE archetype 70");

    // 2. Verify lazy outer-entry spawn + activate Homefront gate on 10_387, mine Mutanite via dogmaIM.Mine & complete
    eve_server::services::pve_special_sites::on_warp_to_homefront_site(&ctx, jita, target_instance_id).await;
    let seeded_st = eve_server::services::pve_homefront_sites::get_homefront_runtime_state(&ctx, target_instance_id)
        .expect("seeded Homefront runtime state");
    assert!(!seeded_st.pocket_spawned, "Seeded Homefront site must not spawn pocket NPCs before gate entry");

    let hf_st = eve_server::services::pve_homefront_sites::spawn_homefront_site_for_dungeon(&ctx, jita, 10_387)
        .expect("spawn Homefront 10_387");
    let hf_inst_id = hf_st.instance_id;
    let ship_id = 910_599_i64;
    let miner_mod_id = 924_599_i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_002) {
            ch.active_ship_id = ship_id;
        }
        snap.skills.entry(90_000_002).or_default().insert(3386, eve_db::TrainedSkillRecord { type_id: 3386, level: 5, skill_points: 256_000.0 });
        snap.items.insert(
            ship_id,
            eve_db::InventoryItemRecord {
                item_id: ship_id,
                type_id: 32_880,
                owner_id: 90_000_002,
                location_id: jita as i64,
                flag: eve_db::InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            miner_mod_id,
            eve_db::InventoryItemRecord {
                item_id: miner_mod_id,
                type_id: 483,
                owner_id: 90_000_002,
                location_id: ship_id,
                flag: eve_db::InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(miner_mod_id, true);
    });
    ctx.invalidate_ship_fitting(ship_id);
    sess.ship_id = Some(ship_id);

    let jita_sys = ctx.systems.get_or_spawn(jita);
    let _ = jita_sys
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: 32_880,
                position: hf_st.site_position + DVec3::new(500.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 335.0,
                inertia: 1.0,
                mass: 1_200_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_ship_combat_state(90_000_002, ship_id, 32_880),
        )
        .await;
    let PyRep::Tuple(bound_jita) = ctx
        .dispatch_rpc(&mut sess, "beyonce", None, "MachoBindObject", &[PyRep::Integer(jita as i64), PyRep::None], &kw, None)
        .await
        .expect("bind beyonce")
    else {
        panic!("expected tuple from MachoBindObject");
    };
    let jita_beyonce = bound_jita[0].as_i64().unwrap() as u64;

    ctx.dispatch_rpc(
        &mut sess,
        "beyonce",
        Some(jita_beyonce),
        "ActivateAccelerationGate",
        &[PyRep::Integer(hf_st.outer_gate_ball_id)],
        &kw,
        None,
    )
    .await
    .expect("ActivateAccelerationGate in Venture");

    let rock_id = match eve_server::services::pve_homefront_sites::get_homefront_runtime_state(&ctx, hf_inst_id).unwrap().progress {
        eve_server::services::pve_homefront_sites::HomefrontArchetypeProgress::MetaliminalMeteoroid { meteoroid_ball_id, .. } => meteoroid_ball_id,
        other => panic!("expected MetaliminalMeteoroid, got {other:?}"),
    };
    let rock_pos = jita_sys.get_ball_snapshot(rock_id).await.unwrap().expect("Mutanite rock ball").1.position;
    jita_sys.set_ball_position(ship_id, rock_pos + DVec3::new(1_000.0, 0.0, 0.0)).await;

    let init_balance = ctx.db.with_snapshot(|s| s.characters[&90_000_002].balance.0);
    let init_journal_len = ctx.db.with_snapshot(|s| s.wallet_journal.len());
    let _ = ctx.drain_notifications_for_char(90_000_002);

    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Mine", &[PyRep::Integer(rock_id)], &kw, None)
        .await
        .expect("dogmaIM.Mine Mutanite");
    let rem = match eve_server::services::pve_homefront_sites::get_homefront_runtime_state(&ctx, hf_inst_id).unwrap().progress {
        eve_server::services::pve_homefront_sites::HomefrontArchetypeProgress::MetaliminalMeteoroid { remaining_units, .. } => remaining_units,
        _ => unreachable!(),
    };
    if rem > 0 {
        eve_server::services::pve_homefront_sites::metaliminal_mine_mutanite(&ctx, &sess, hf_inst_id, rem, 0).unwrap();
    }
    if !eve_server::services::pve_homefront_sites::get_homefront_runtime_state(&ctx, hf_inst_id).unwrap().completed {
        eve_server::services::pve_homefront_sites::complete_homefront_operation_and_pay(&ctx, &sess, hf_inst_id)
            .await
            .expect("complete Homefront 10_387");
    }

    let final_balance = ctx.db.with_snapshot(|s| s.characters[&90_000_002].balance.0);
    assert!(final_balance > init_balance, "Pilot must receive ISK payout from in-space Homefront completion");

    let journal_after = ctx.db.with_snapshot(|s| s.wallet_journal.clone());
    assert!(
        journal_after.len() > init_journal_len
            && journal_after.iter().any(|j| {
                j.character_id == 90_000_002
                    && j.entry_type == "CorporateRewardPayout"
                    && j.reason.contains("Homefront Operation Payout")
            }),
        "Homefront completion must record a CorporateRewardPayout wallet journal entry"
    );

    let notifs = ctx.drain_notifications_for_char(90_000_002);
    assert!(
        notifs.iter().any(|n| n.event == "OnAccountChange"),
        "Homefront payout must emit OnAccountChange notification"
    );

    // 3. Query GetHomefrontSiteInstances again: the completed instance is no longer returned in the active registry list
    let hf_after_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "dungeonInstanceCacheMgr",
            None,
            "GetHomefrontSiteInstances",
            &[PyRep::Integer(jita as i64)],
            &kw,
            None,
        )
        .await
        .expect("GetHomefrontSiteInstances after completion");
    let PyRep::Dict(hf_after_dict) = &hf_after_rep else {
        panic!("Expected Dict");
    };
    let Some((_, PyRep::List(jita_after))) = hf_after_dict.iter().find(|(k, _)| k.as_i64() == Some(jita as i64)) else {
        panic!("Missing Jita in after dict");
    };
    let active_remaining: Vec<i64> = jita_after
        .iter()
        .filter_map(|e| e.dict_get("instanceID").and_then(|v| v.as_i64()))
        .collect();
    assert!(
        !active_remaining.contains(&hf_inst_id),
        "Completed instance {hf_inst_id} must not appear in active registry instances"
    );

    // 4. Lowsec/nullsec query returns empty (Homefront is highsec only)
    let nullsec_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "dungeonInstanceCacheMgr",
            None,
            "GetHomefrontSiteInstances",
            &[PyRep::Integer(30_002_904)],
            &kw,
            None,
        )
        .await
        .expect("GetHomefrontSiteInstances for nullsec");
    let PyRep::Dict(ns_dict) = &nullsec_rep else { panic!("Expected Dict") };
    let (_, PyRep::List(ns_dungeons)) = &ns_dict[0] else { panic!("Expected List") };
    assert!(ns_dungeons.is_empty(), "Nullsec system must not have Homefront operations");
}
