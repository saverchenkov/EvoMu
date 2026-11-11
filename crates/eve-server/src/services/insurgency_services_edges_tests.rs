//! Edge-case tests for `insurgency_services`: campaign phase transitions, outcome resolution,
//! suppression/corruption modifiers, FOB invulnerability/destruction, FW complex ticks, and RPC errors.

#![forbid(unsafe_code)]

use super::*;
use crate::services::account_services::account_services_tests::{auth_session, test_ctx};
use eve_db::{CharacterRecord, FwSystemOccupationRecord, InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, MovementMode};
use glam::DVec3;
use std::collections::HashMap;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn insurgency_phases_modifiers_outcomes_and_stronghold_destruction() {
    let ctx = test_ctx();
    let now = ctx.now_unix();
    let camp_id = 91_001_i64;
    let fob_sys = 30_002_537_u32; // Lowsec system if present, or fallback
    let second_sys = 30_002_538_u32;

    // 1. Angel Cartel campaign auto-pushes fob_sys into systems, uses Angel FOB type (78260), warzone 2
    init_insurgency_campaign(
        &ctx,
        camp_id,
        FACTION_ANGEL_CARTEL,
        500_002,
        fob_sys,
        vec![second_sys],
    );
    let fob_id = spawn_pirate_stronghold(&ctx, camp_id).unwrap();
    assert_eq!(fob_id, unified_fob_id(camp_id));
    ctx.db.with_snapshot(|snap| {
        let st = snap.structures.get(&fob_id).unwrap();
        assert_eq!(st.type_id, PIRATE_STRONGHOLD_ANGEL_TYPE);
        let c = snap.insurgency_runtime.campaigns.get(&camp_id).unwrap();
        assert!(c.systems.contains(&fob_sys));
        let rep = build_campaign_snapshot_for_system(c, second_sys, now, 0);
        assert_eq!(rep.dict_get("warzoneID").and_then(|v| v.as_i64()), Some(2));
        assert_eq!(rep.dict_get("isFobSystem").and_then(|v| v.as_bool()), Some(false));
        assert_eq!(rep.dict_get("phase").and_then(|v| v.as_str()), Some(PHASE_FORECASTING));
    });

    // Forecasting phase: insurgency_system_corruption_stage returns 0 even if points exist
    assert_eq!(insurgency_system_corruption_stage(&ctx, fob_sys), 0);

    // Unknown campaign_id add_system_corruption / add_system_suppression return (0, 0)
    assert_eq!(add_system_corruption(&ctx, 777_777, fob_sys, 100), (0, 0));
    assert_eq!(add_system_suppression(&ctx, 777_777, fob_sys, 100), (0, 0));

    // Switch campaign to Active phase
    with_insurgency_runtime(&ctx, |rt| {
        if let Some(c) = rt.campaigns.get_mut(&camp_id) {
            c.forecasting_end_unix = now.saturating_sub(100);
            c.active_end_unix = now + 10_000;
            c.winddown_end_unix = now + 20_000;
        }
    });

    // Reach suppression stage 3 (450 pts) -> positive corruption points are reduced by 30%
    add_system_suppression(&ctx, camp_id, fob_sys, 450);
    let (c_pts, _) = add_system_corruption(&ctx, camp_id, fob_sys, 100);
    assert_eq!(c_pts, 70);

    // Reach suppression stage 5 (1000 pts total) -> corruption capped below 800 when advancing from <5
    add_system_suppression(&ctx, camp_id, fob_sys, 550);
    let (c_capped, c_stg) = add_system_corruption(&ctx, camp_id, fob_sys, 1_500);
    assert_eq!(c_capped, 800);
    assert_eq!(c_stg, 4);
    assert_eq!(insurgency_system_corruption_stage(&ctx, fob_sys), 4);

    // Negative adjustment emits SetNotice path cleanly
    let (c_dec, _) = add_system_corruption(&ctx, camp_id, fob_sys, -200);
    assert_eq!(c_dec, 600);
    let (s_dec, _) = add_system_suppression(&ctx, camp_id, fob_sys, -100);
    assert_eq!(s_dec, 900);

    // Ice heist cooldown prevents duplicate spawn; clearing cooldown and existing allows spawn
    let heist_id = spawn_ice_heist(&ctx, second_sys);
    assert!(heist_id >= 800_001);

    // FOB invulnerability edges: fob_armor_reinforced_until & structure ArmorReinforced
    with_insurgency_runtime(&ctx, |rt| {
        if let Some(c) = rt.campaigns.get_mut(&camp_id) {
            for off in 0..5u32 {
                c.suppression_points.insert(fob_sys + 10 + off, 1_000);
            }
        }
        rt.fob_armor_reinforced_until.insert(camp_id, now + 3600);
    });
    assert!(is_insurgency_stronghold_invulnerable(&ctx, fob_sys, fob_id));
    with_insurgency_runtime(&ctx, |rt| {
        rt.fob_armor_reinforced_until.insert(camp_id, now.saturating_sub(1));
    });
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(st) = snap.structures.get_mut(&fob_id) {
            st.state = "ArmorReinforced".into();
            st.reinforced_until_unix = now + 500;
        }
    });
    assert!(is_insurgency_stronghold_invulnerable(&ctx, fob_sys, fob_id));

    // Restore ShieldVulnerable and destroy stronghold via on_insurgency_stronghold_destroyed_with_sde
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(st) = snap.structures.get_mut(&fob_id) {
            st.state = "ShieldVulnerable".into();
            st.reinforced_until_unix = 0;
        }
    });
    assert!(!is_insurgency_stronghold_invulnerable(&ctx, fob_sys, fob_id));

    let destroyed = on_insurgency_stronghold_destroyed_with_sde(
        &ctx.systems,
        &ctx.db,
        Some(&ctx.sde),
        fob_sys,
        fob_id,
        Some(90_000_001),
    )
    .await;
    assert!(destroyed.is_some());
    let (d_info, _) = destroyed.unwrap();
    assert_eq!(d_info.campaign_id, camp_id);
    assert!(is_stronghold_invulnerable_in_db(&ctx.db, fob_sys, fob_id));

    // Second destruction attempt while in WindDown / resolved returns None
    assert!(on_insurgency_stronghold_destroyed_core(
        &ctx.systems,
        &ctx.db,
        fob_sys,
        fob_id,
        Some(90_000_001),
    )
    .await
    .is_none());

    // notify_stronghold_destruction with pending map, broadcast tx, and missing campaign fallback
    let pend = std::sync::RwLock::new(HashMap::new());
    let (ntx, mut nrx) = tokio::sync::broadcast::channel(16);
    notify_stronghold_destruction(
        &ctx.db,
        Some(&ctx.quasar_gateway),
        Some(&pend),
        Some(&ntx),
        &StrongholdDestructionInfo {
            campaign_id: 777_888, // absent campaign triggers fallback snapshot
            solar_system_id: fob_sys,
            killer_char_id: Some(90_000_001),
            old_suppression: 500,
            new_suppression: 1_000,
            new_suppression_stage: 5,
        },
    );
    assert!(pend.read().unwrap().contains_key(&90_000_001));
    assert!(nrx.try_recv().is_ok());

    // end_insurgency_campaign outcomes: PirateWin, EmpireWin, NoWinner, and missing
    with_insurgency_runtime(&ctx, |rt| {
        rt.campaign_outcomes.insert(camp_id, "PirateWin".into());
    });
    end_insurgency_campaign(&ctx, camp_id);
    end_insurgency_campaign(&ctx, camp_id); // missing no-op

    let camp_emp = 91_002_i64;
    init_insurgency_campaign(&ctx, camp_emp, FACTION_GURISTAS, 500_001, fob_sys, vec![fob_sys]);
    with_insurgency_runtime(&ctx, |rt| {
        rt.campaign_outcomes.insert(camp_emp, "EmpireWin".into());
    });
    end_insurgency_campaign(&ctx, camp_emp);

    let camp_none = 91_003_i64;
    init_insurgency_campaign(&ctx, camp_none, FACTION_GURISTAS, 500_001, fob_sys, vec![fob_sys]);
    end_insurgency_campaign(&ctx, camp_none);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn insurgency_fw_complex_tick_and_rpc_error_edges() {
    let ctx = test_ctx();
    let now = ctx.now_unix();
    let kw = HashMap::new();

    // Pick a real lowsec system from SDE (0.0 < sec < 0.45)
    let lowsec_sys = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| s.security_status > 0.0 && s.security_status < 0.45)
        .map(|s| s.solar_system_id)
        .unwrap_or(30_002_537);

    let mut sess = auth_session(1, 90_000_001, 98_000_001, lowsec_sys);
    let mut sess_no_sys = sess.clone();
    sess_no_sys.solar_system_id = None;

    // GetLocalCampaignClientSnapshot errors & None snapshot when no campaign covers system
    assert_eq!(
        handle(&ctx, &mut sess_no_sys, "insurgencySolarsystem", "GetLocalCampaignClientSnapshot", &[], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "NoSolarSystem"
    );
    let local_none = handle(&ctx, &mut sess, "insurgencySolarsystem", "GetLocalCampaignClientSnapshot", &[], &kw)
        .await
        .unwrap()
        .unwrap();
    let PyRep::Tuple(parts) = local_none else { panic!("tuple expected") };
    assert_eq!(parts[1], PyRep::None);

    // Invalid solarSystemID on GetPirateInsurgencyInstances & GetPirateInsurgencyIceHeistInstances
    for method in ["GetPirateInsurgencyInstances", "GetPirateInsurgencyIceHeistInstances"] {
        let err = handle(&ctx, &mut sess, "dungeonInstanceCacheMgr", method, &[PyRep::Integer(999_999_999)], &kw)
            .await
            .unwrap()
            .unwrap_err();
        assert!(err.code.eq_ignore_ascii_case("invalidsolarsystem"), "got {}", err.code);
        // PyRep::None falls back to session solar_system_id (or None if unset)
        assert!(
            handle(&ctx, &mut sess_no_sys, "dungeonInstanceCacheMgr", method, &[PyRep::None], &kw)
                .await
                .unwrap()
                .is_ok()
        );
    }

    // FW/insurgency complex tick evaluation:
    // 1. fid == 0 -> false
    assert!(!step_system_insurgency_capture_tick(&ctx, lowsec_sys, 98_000_001, 0).await);
    // 2. No active complex -> no-op
    step_system_insurgency_tick(&ctx, lowsec_sys).await;

    // Seed online pilot + ship on complex beacon in lowsec system
    let ship_id = 98_500_001_i64;
    let char_id = 90_000_001_i64;
    let enemy_char = 90_000_055_i64;
    let enemy_ship = 98_500_002_i64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = lowsec_sys;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = ship_id;
            ch.war_faction_id = Some(500_001);
        }
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: char_id,
                location_id: i64::from(lowsec_sys),
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        let st = snap.system_states.entry(lowsec_sys).or_default();
        st.solar_system_id = lowsec_sys;
        st.fw_complex_position = Some([1_000.0, 0.0, 0.0]);
        st.fw_capture_duration_seconds = 600;
        st.fw_capture_seconds_elapsed = 590;
        snap.fw_system_occupations.insert(
            lowsec_sys,
            FwSystemOccupationRecord {
                solar_system_id: lowsec_sys,
                owner_faction_id: 500_002,
                occupier_faction_id: 500_002,
                victory_points: 100,
                victory_points_threshold: 1000,
            },
        );
    });
    let _ = ctx.db.acquire_online_lock(1, 501, Some(char_id), true, now);

    let sys_handle = ctx.systems.get_or_spawn(lowsec_sys);
    let _ = sys_handle
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 587,
            position: DVec3::new(1_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;

    // 3. Complete lowsec FW complex capture with forced_delta = Some(30) -> awards LP and clears fw_complex_position
    step_system_insurgency_tick_delta(&ctx, lowsec_sys, Some(30)).await;
    ctx.db.with_snapshot(|snap| {
        assert!(snap.system_states[&lowsec_sys].fw_complex_position.is_none());
        assert_eq!(snap.fw_system_occupations[&lowsec_sys].victory_points, 120);
    });

    // 4. Re-seed complex under an active insurgency campaign with opposing militia -> contested (returns false)
    let camp_id = 91_004_i64;
    init_insurgency_campaign(&ctx, camp_id, FACTION_GURISTAS, 500_001, lowsec_sys, vec![lowsec_sys]);
    ctx.db.with_mut_snapshot(|snap| {
        let st = snap.system_states.get_mut(&lowsec_sys).unwrap();
        st.fw_complex_position = Some([1_000.0, 0.0, 0.0]);
        let mut enemy_acc = snap.accounts.values().next().unwrap().clone();
        enemy_acc.user_id = 55;
        enemy_acc.username = "enemy_acc".into();
        enemy_acc.password_token = "h".into();
        snap.accounts.insert("enemy_acc".into(), enemy_acc);

        let mut enemy_rec: CharacterRecord = snap.characters[&90_000_001].clone();
        enemy_rec.character_id = enemy_char;
        enemy_rec.user_id = 55;
        enemy_rec.name = "PiratePilot".into();
        enemy_rec.solar_system_id = lowsec_sys;
        enemy_rec.active_ship_id = enemy_ship;
        enemy_rec.war_faction_id = Some(FACTION_GURISTAS);
        snap.characters.insert(enemy_char, enemy_rec);
        snap.items.insert(
            enemy_ship,
            InventoryItemRecord {
                item_id: enemy_ship,
                type_id: 587,
                owner_id: enemy_char,
                location_id: i64::from(lowsec_sys),
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    let _ = ctx.db.acquire_online_lock(55, 502, Some(enemy_char), true, now);
    let _ = sys_handle
        .spawn_ball(Ball {
            id: enemy_ship,
            type_id: 587,
            position: DVec3::new(1_200.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;

    // Opposing sides on grid -> evaluate returns false
    assert!(!step_system_insurgency_capture_tick(&ctx, lowsec_sys, ship_id, 500_001).await);

    // Move empire ship > 30 km away and advance tick so only pirate ship is on grid -> advances corruption!
    let _ = sys_handle.remove_ball(ship_id).await;
    {
        let mut bp = sys_handle.ballpark.write().unwrap();
        for _ in 0..5 {
            bp.step(1.0);
        }
    }
    assert!(step_system_insurgency_capture_tick(&ctx, lowsec_sys, enemy_ship, FACTION_GURISTAS).await);
    let corr_pts = read_insurgency_runtime(&ctx, |rt| {
        rt.campaigns[&camp_id].corruption_points.get(&lowsec_sys).copied().unwrap_or(0)
    });
    assert!(corr_pts > 0);
}
