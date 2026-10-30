//! Comprehensive remediation tests for Metaliminal Roaming Storms,
//! Roaming Weather Map RPC, and Homefront Operation Instance Caching
//! (`feat:pve.metaliminal-storms`, `feat:pve.homefront-ops`, `rpc:map.GetAllRoamingWeatherSystems`, `rpc:dungeonInstanceCacheMgr.GetHomefrontSiteInstances`).

use eve_destiny::DamageVector;
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
    assert_eq!(initial_centers.len(), 4, "Must have 4 roaming metaliminal storms");
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
    // parity: feat:pve.homefront-ops, rpc:dungeonInstanceCacheMgr.GetHomefrontSiteInstances
    eve_testkit::attest(&["feat:pve.homefront-ops"]);

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

    let Some((_, PyRep::Dict(jita_dungeons))) = hf_dict.iter().find(|(k, _)| k.as_i64() == Some(jita as i64)) else {
        panic!("Dict must contain entry for queried solar system {jita}");
    };
    assert!(!jita_dungeons.is_empty(), "Jita must have seeded Homefront dungeons");

    let (did_rep, list_rep) = &jita_dungeons[0];
    let did = did_rep.as_i64().expect("dungeonID") as u32;
    let PyRep::List(inst_list) = list_rep else {
        panic!("Expected List of instance IDs");
    };
    assert!(!inst_list.is_empty(), "Dungeon must have active instance IDs");
    let target_instance_id = inst_list[0].as_i64().expect("instanceID");

    // SDE Archetype 70 verification: verify dungeon ID is from SDE archetype 70
    let d_archetype = ctx.sde.dungeons.get(&did).and_then(|d| d.archetype_id);
    assert_eq!(d_archetype, Some(70), "Homefront dungeon must belong to SDE archetype 70");

    // 2. Complete the Homefront site via in-space combat destruction of the SDE-backed boss NPC
    let (boss_bid, boss_tid) = with_pve_special_reg(&ctx, |reg| {
        let s = reg.homefront_sites[&jita]
            .iter()
            .find(|s| s.instance_id == target_instance_id)
            .unwrap();
        (s.boss_npc_ball_id, s.boss_type_id)
    });
    assert!(ctx.sde.types.contains_key(&boss_tid), "Homefront boss must use a real SDE NPC type");

    let jita_sys = ctx.systems.get_or_spawn(jita);
    let boss_snap = jita_sys
        .get_ball_snapshot(boss_bid)
        .await
        .expect("get_ball_snapshot")
        .expect("Homefront boss ball must be spawned");
    assert_eq!(boss_snap.1.type_id, boss_tid);

    let boss_combat = jita_sys
        .get_combat_state(boss_bid)
        .await
        .expect("get_combat_state")
        .expect("Homefront boss ball must have combat state");
    assert!(
        boss_combat.max_shield_hp > 0.0
            && boss_combat.max_armor_hp > 0.0
            && boss_combat.max_structure_hp > 0.0,
        "Homefront boss ball must have SDE-backed CombatState HP"
    );

    let init_balance = ctx.db.with_snapshot(|s| s.characters[&90_000_002].balance.0);
    let init_journal_len = ctx.db.with_snapshot(|s| s.wallet_journal.len());
    let _ = ctx.drain_notifications_for_char(90_000_002);

    let kill_evt = jita_sys
        .apply_damage_with_attacker(
            boss_bid,
            DamageVector {
                em: 50_000.0,
                thermal: 50_000.0,
                kinetic: 50_000.0,
                explosive: 50_000.0,
            },
            Some(90_000_002),
        )
        .await
        .expect("apply_damage_with_attacker")
        .expect("Homefront boss hit result");
    assert!(kill_evt.1, "Homefront boss must be destroyed by combat damage");

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
    let Some((_, PyRep::Dict(jita_after))) = hf_after_dict.iter().find(|(k, _)| k.as_i64() == Some(jita as i64)) else {
        panic!("Missing Jita in after dict");
    };
    let active_remaining: Vec<i64> = jita_after
        .iter()
        .flat_map(|(_, l)| match l {
            PyRep::List(ids) => ids.iter().filter_map(|i| i.as_i64()).collect::<Vec<_>>(),
            _ => Vec::new(),
        })
        .collect();
    assert!(
        !active_remaining.contains(&target_instance_id),
        "Completed instance {target_instance_id} must not appear in active registry instances"
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
    let (_, PyRep::Dict(ns_dungeons)) = &ns_dict[0] else { panic!("Expected Dict") };
    assert!(ns_dungeons.is_empty(), "Nullsec system must not have Homefront operations");
}
