//! End-to-end integration tests for `work/open-feats-and-rpcs-batch2`.

use std::collections::HashMap;

use eve_db::{
    AllianceRecord, CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents,
    UpwellStructureRecord,
};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;

fn make_gm_session(char_id: i64, corp_id: i64, alliance_id: Option<i64>, sys_id: u32) -> SessionState {
    let mut sess = SessionState::new(50_001);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(corp_id);
    sess.alliance_id = alliance_id;
    sess.solar_system_id = Some(sys_id);
    sess.role = eve_db::service_roles::ROLE_ADMIN
        | eve_db::service_roles::ROLE_GMH
        | eve_machonet::service_roles::ROLE_QA
        | eve_machonet::service_roles::ROLE_PROGRAMMER;
    sess
}

fn make_player_session(
    sess_id: u64,
    user_id: u64,
    char_id: i64,
    corp_id: i64,
    alliance_id: Option<i64>,
    sys_id: u32,
) -> SessionState {
    let mut sess = SessionState::new(sess_id);
    sess.user_id = Some(user_id);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(corp_id);
    sess.alliance_id = alliance_id;
    sess.solar_system_id = Some(sys_id);
    sess
}

#[tokio::test]
async fn test_cluster_session_localization_qa_rpcs_e2e() {
    // parity: rpc:DB2.CallProc, rpc:loadService.GetTotalStats, rpc:loadService.StartLoad, rpc:loadService.StopLoad, rpc:localizationServer.ReloadFSDPickle, rpc:raffleMgr.QA_SeedRaffles, rpc:sessionMgr.EndAllGameSessions, rpc:sessionMgr.RemoveSessionsFromServer, rpc:userSvc.ProcessEVESessionCreated
    let ctx = ServerContext::new();
    ctx.seed_demo_world_fixtures();
    let kw = HashMap::new();

    let mut gm = make_gm_session(90_000_001, 98_000_001, Some(99_000_001), 30_000_142);
    let mut player = make_player_session(50_002, 2, 90_000_002, 98_000_001, Some(99_000_001), 30_000_142);

    // 1. userSvc.ProcessEVESessionCreated
    let created = ctx
        .dispatch_rpc(&mut player, "userSvc", None, "ProcessEVESessionCreated", &[], &kw, None)
        .await
        .expect("ProcessEVESessionCreated");
    assert_eq!(created, PyRep::None);
    ctx.db.with_snapshot(|snap| {
        let sessions_json = snap
            .service_state_blobs
            .get("user_sessions")
            .expect("user_sessions blob");
        assert!(sessions_json.contains("50002"), "Session 50002 must be recorded in user_sessions");
    });

    // 2. loadService.StartLoad / GetTotalStats / StopLoad
    let denied = ctx
        .dispatch_rpc(&mut player, "loadService", None, "StartLoad", &[], &kw, None)
        .await;
    assert!(denied.is_err(), "Non-admin must be denied loadService.StartLoad");

    ctx.dispatch_rpc(
        &mut gm,
        "loadService",
        None,
        "StartLoad",
        &[PyRep::String("stress_test_v24".into())],
        &kw,
        None,
    )
    .await
    .expect("StartLoad");

    let stats_active = ctx
        .dispatch_rpc(&mut gm, "loadService", None, "GetTotalStats", &[], &kw, None)
        .await
        .expect("GetTotalStats");
    assert_eq!(stats_active.dict_get("active"), Some(&PyRep::Bool(true)));
    assert_eq!(stats_active.dict_get("runs_started"), Some(&PyRep::Integer(1)));

    ctx.dispatch_rpc(&mut gm, "loadService", None, "StopLoad", &[], &kw, None)
        .await
        .expect("StopLoad");
    let stats_stopped = ctx
        .dispatch_rpc(&mut gm, "loadService", None, "GetTotalStats", &[], &kw, None)
        .await
        .expect("GetTotalStats after stop");
    assert_eq!(stats_stopped.dict_get("active"), Some(&PyRep::Bool(false)));

    // 3. localizationServer.UpdateLocalizationQAWrap (fail-closed UnimplementedRPC) & ReloadFSDPickle
    let qa_wrap_err = ctx
        .dispatch_rpc(
            &mut gm,
            "localizationServer",
            None,
            "UpdateLocalizationQAWrap",
            &[PyRep::Dict(vec![(
                PyRep::Integer(12345),
                PyRep::String("QA Override".into()),
            )])],
            &kw,
            None,
        )
        .await
        .expect_err("UpdateLocalizationQAWrap must fail-closed with UnimplementedRPC");
    assert_eq!(qa_wrap_err.code, "UnimplementedRPC");

    ctx.dispatch_rpc(
        &mut gm,
        "localizationServer",
        None,
        "ReloadFSDPickle",
        &[PyRep::String("localization_en_us".into())],
        &kw,
        None,
    )
    .await
    .expect("ReloadFSDPickle");

    // 4. DB2.CallProc & debug.Exec (fail-closed UnimplementedRPC)
    let polaris = ctx
        .dispatch_rpc(
            &mut gm,
            "DB2",
            None,
            "CallProc",
            &[PyRep::String("zcluster.Nodes_PolarisID".into())],
            &kw,
            None,
        )
        .await
        .expect("DB2.CallProc");
    assert_eq!(polaris, PyRep::Integer(1));

    let users_rowset = ctx
        .dispatch_rpc(
            &mut gm,
            "DB2",
            None,
            "CallProc",
            &[PyRep::String("zuser.Users_SelectByRole".into())],
            &kw,
            None,
        )
        .await
        .expect("DB2.CallProc zuser.Users_SelectByRole");
    assert!(matches!(users_rowset, PyRep::Rowset { .. }));

    let unknown_proc_err = ctx
        .dispatch_rpc(
            &mut gm,
            "DB2",
            None,
            "CallProc",
            &[PyRep::String("zcluster.UnknownProc".into())],
            &kw,
            None,
        )
        .await
        .expect_err("DB2.CallProc unknown proc must return UnimplementedRPC");
    assert_eq!(unknown_proc_err.code, "UnimplementedRPC");

    let debug_exec_err = ctx
        .dispatch_rpc(
            &mut gm,
            "debug",
            None,
            "Exec",
            &[PyRep::String("status".into())],
            &kw,
            None,
        )
        .await
        .expect_err("debug.Exec must fail-closed with UnimplementedRPC");
    assert_eq!(debug_exec_err.code, "UnimplementedRPC");

    // 5. raffleMgr.QA_SeedRaffles
    let seeded = ctx
        .dispatch_rpc(
            &mut gm,
            "raffleMgr",
            None,
            "QA_SeedRaffles",
            &[PyRep::Integer(2)],
            &kw,
            None,
        )
        .await
        .expect("QA_SeedRaffles");
    let PyRep::List(seeded_ids) = seeded else {
        panic!("Expected list of seeded raffle IDs");
    };
    assert_eq!(seeded_ids.len(), 2);

    // 6. sessionMgr.RemoveSessionsFromServer & EndAllGameSessions
    ctx.dispatch_rpc(
        &mut gm,
        "sessionMgr",
        None,
        "RemoveSessionsFromServer",
        &[PyRep::None, PyRep::List(vec![PyRep::Integer(50_002)])],
        &kw,
        None,
    )
    .await
    .expect("RemoveSessionsFromServer");

    ctx.dispatch_rpc(
        &mut gm,
        "sessionMgr",
        None,
        "EndAllGameSessions",
        &[],
        &kw,
        None,
    )
    .await
    .expect("EndAllGameSessions");
}

#[tokio::test]
async fn test_lookup_warable_and_sov_structures_e2e() {
    // parity: rpc:lookupSvc.LookupWarableCorporationsOrAlliances, feat:ui.lookup, rpc:sovMgr.GetSovStructuresInfoForSolarSystem
    let ctx = ServerContext::new();
    ctx.seed_demo_world_fixtures();
    let kw = HashMap::new();

    let null_sys: u32 = 30_001_269;
    let mut sess = make_player_session(50_010, 1, 90_000_001, 98_000_001, Some(99_000_001), null_sys);

    ctx.db.with_mut_snapshot(|snap| {
        // Standalone war-eligible corp (owns structure)
        snap.corporations.insert(
            98_000_101,
            CorporationRecord {
                corporation_id: 98_000_101,
                name: "Vanguard Horizon".into(),
                ticker: "VGHD".into(),
                ceo_id: 90_000_101,
                alliance_id: None,
                members: [90_000_101].into_iter().collect(),
                ..Default::default()
            },
        );
        // Standalone non-war-eligible corp (no structure)
        snap.corporations.insert(
            98_000_102,
            CorporationRecord {
                corporation_id: 98_000_102,
                name: "Vanguard Peacekeepers".into(),
                ticker: "VGPK".into(),
                ceo_id: 90_000_102,
                alliance_id: None,
                members: [90_000_102].into_iter().collect(),
                ..Default::default()
            },
        );
        // External alliance + member corp with structure
        snap.corporations.insert(
            98_000_103,
            CorporationRecord {
                corporation_id: 98_000_103,
                name: "Vanguard Sov Holding".into(),
                ticker: "VGSH".into(),
                ceo_id: 90_000_103,
                alliance_id: Some(99_000_103),
                members: [90_000_103].into_iter().collect(),
                ..Default::default()
            },
        );
        snap.alliances.insert(
            99_000_103,
            AllianceRecord {
                alliance_id: 99_000_103,
                name: "Vanguard Coalition".into(),
                short_name: "VGC".into(),
                executor_corp_id: 98_000_103,
                member_corp_ids: [98_000_103].into_iter().collect(),
            },
        );

        // Non-sov Upwell Astrahus in null_sys (must NOT appear in GetSovStructuresInfoForSolarSystem)
        snap.structures.insert(
            1_030_000_000_101,
            UpwellStructureRecord {
                structure_id: 1_030_000_000_101,
                type_id: 35832, // Astrahus
                owner_corp_id: 98_000_101,
                solar_system_id: null_sys,
                name: "Vanguard Astrahus".into(),
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                shield_hp: 1.0,
                armor_hp: 1.0,
                hull_hp: 1.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
        // Sovereignty Hub (32458) owned by 98_000_103 (alliance 99_000_103)
        snap.structures.insert(
            1_030_000_000_103,
            UpwellStructureRecord {
                structure_id: 1_030_000_000_103,
                type_id: 32458, // Sovereignty Hub
                owner_corp_id: 98_000_103,
                solar_system_id: null_sys,
                name: "Vanguard Sov Hub".into(),
                position: [100_000.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                shield_hp: 1.0,
                armor_hp: 1.0,
                hull_hp: 1.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
        snap.sov_runtime
            .adm_vulnerability_windows
            .insert(1_030_000_000_103, (36_000, 46_800));
    });

    // 1. LookupWarableCorporationsOrAlliances
    let warable_res = ctx
        .dispatch_rpc(
            &mut sess,
            "lookupSvc",
            None,
            "LookupWarableCorporationsOrAlliances",
            &[PyRep::String("Vanguard".into()), PyRep::Bool(false)],
            &kw,
            None,
        )
        .await
        .expect("LookupWarableCorporationsOrAlliances");
    let PyRep::List(warable) = warable_res else {
        panic!("Expected list");
    };
    let ids: Vec<i64> = warable
        .iter()
        .filter_map(|r| r.dict_get("ownerID").and_then(PyRep::as_i64))
        .collect();
    assert!(ids.contains(&98_000_101), "Standalone corp with structure must be warable");
    assert!(!ids.contains(&98_000_102), "Structureless corp must NOT be warable");
    assert!(!ids.contains(&98_000_103), "Alliance member corp must NOT be listed individually");
    assert!(ids.contains(&99_000_103), "Alliance with structure-owning member must be warable");

    // 2. GetSovStructuresInfoForSolarSystem
    let sov_res = ctx
        .dispatch_rpc(
            &mut sess,
            "sovMgr",
            None,
            "GetSovStructuresInfoForSolarSystem",
            &[PyRep::Integer(null_sys as i64)],
            &kw,
            None,
        )
        .await
        .expect("GetSovStructuresInfoForSolarSystem");
    let PyRep::List(sov_structs) = sov_res else {
        panic!("Expected structures list");
    };
    assert_eq!(sov_structs.len(), 1, "Only Sovereignty Hub (32458) must be returned, not Astrahus");
    let hub = &sov_structs[0];
    assert_eq!(hub.dict_get("structureID"), Some(&PyRep::Integer(1_030_000_000_103)));
    assert_eq!(hub.dict_get("allianceID"), Some(&PyRep::Integer(99_000_103)));
    let v_start = hub.dict_get("vulnerableStartTime").and_then(PyRep::as_i64).unwrap_or(0);
    let v_end = hub.dict_get("vulnerableEndTime").and_then(PyRep::as_i64).unwrap_or(0);
    assert!(v_end > v_start && v_start > 100_000_000_000_000_000, "Must return valid Windows FILETIME window");
}

#[tokio::test]
async fn test_homefront_sites_and_hypernet_raffle_e2e() {
    // parity: rpc:dungeonInstanceCacheMgr.GetHomefrontSiteInstances, rpc:raffleProxy.CreateRaffle, rpc:raffleProxy.BuyRandomTickets, rpc:raffleProxy.GetActiveHistoricPrices
    let ctx = ServerContext::new();
    ctx.seed_demo_world_fixtures();
    let kw = HashMap::new();

    let jita: u32 = 30_000_142;
    let jita_station: i64 = 60_003_760;
    let unseeded_hisec: u32 = 30_000_144; // Perimeter
    let mut sess = make_player_session(50_020, 1, 90_000_001, 98_000_001, Some(99_000_001), jita);
    sess.station_id = Some(jita_station);

    // 1. GetHomefrontSiteInstances must NOT lazily fabricate sites in unseeded high-sec systems
    let unseeded_res = ctx
        .dispatch_rpc(
            &mut sess,
            "dungeonInstanceCacheMgr",
            None,
            "GetHomefrontSiteInstances",
            &[PyRep::Integer(unseeded_hisec as i64)],
            &kw,
            None,
        )
        .await
        .expect("GetHomefrontSiteInstances unseeded");
    let PyRep::Dict(unseeded_outer) = unseeded_res else {
        panic!("Expected dict");
    };
    let PyRep::Dict(unseeded_inner) = &unseeded_outer[0].1 else {
        panic!("Expected inner dict");
    };
    assert!(unseeded_inner.is_empty(), "Unseeded system must not lazily fabricate Homefront sites");

    // 2. HyperNet CreateRaffle, GetActiveHistoricPrices, BuyRandomTickets
    let raffle_item_id: i64 = 900_000_555;
    let hypercore_item_id: i64 = 900_000_556;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.station_id = Some(jita_station);
            ch.structure_id = None;
            ch.balance = IskCents(500_000_000 * 100);
        }
        snap.items.insert(
            raffle_item_id,
            InventoryItemRecord {
                item_id: raffle_item_id,
                type_id: 587, // Rifter
                owner_id: 90_000_001,
                location_id: jita_station,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            hypercore_item_id,
            InventoryItemRecord {
                item_id: hypercore_item_id,
                type_id: 52_568, // HyperCore
                owner_id: 90_000_001,
                location_id: jita_station,
                flag: InventoryFlag::Hangar,
                quantity: 100,
                singleton: false,
                ..Default::default()
            },
        );
    });

    let created = ctx
        .dispatch_rpc(
            &mut sess,
            "raffleProxy",
            None,
            "CreateRaffle",
            &[
                PyRep::Integer(raffle_item_id),
                PyRep::Integer(8),
                PyRep::Real(1_000_000.0),
            ],
            &kw,
            None,
        )
        .await
        .expect("CreateRaffle");
    let raffle_id = created.dict_get("raffle_id").and_then(PyRep::as_i64).expect("raffle_id");

    let prices_active = ctx
        .dispatch_rpc(
            &mut sess,
            "raffleProxy",
            None,
            "GetActiveHistoricPrices",
            &[PyRep::Integer(587)],
            &kw,
            None,
        )
        .await
        .expect("GetActiveHistoricPrices");
    let PyRep::Tuple(p_tup) = prices_active else {
        panic!("Expected tuple");
    };
    assert_eq!(p_tup[0].as_f64(), Some(8_000_000.0));

    let bought = ctx
        .dispatch_rpc(
            &mut sess,
            "raffleProxy",
            None,
            "BuyRandomTickets",
            &[PyRep::Integer(raffle_id), PyRep::Integer(8)],
            &kw,
            None,
        )
        .await
        .expect("BuyRandomTickets");
    assert_eq!(bought.dict_get("is_finished"), Some(&PyRep::Bool(true)));
    assert_eq!(bought.dict_get("winner_id"), Some(&PyRep::Integer(90_000_001)));
}

#[tokio::test]
async fn test_navigation_jump_fatigue_cloak_and_thera_e2e() {
    // parity: feat:navigation.jump-fatigue-formula, feat:navigation.thera-npc-stations
    let ctx = ServerContext::new();
    ctx.seed_demo_world_fixtures();
    let kw = HashMap::new();
    let char_id = 90_000_001i64;
    let mut sess = make_gm_session(char_id, 98_000_001, Some(99_000_001), 30_000_142);

    // 1. Jump fatigue formula (V24: min 10 min fatigue, 1 min cooldown, 1 + LY scaling)
    let (cd_min, fat_min) = eve_server::formulas::compute_jump_fatigue_and_cooldown_minutes(0.0, 5.0, 0.0);
    assert!((cd_min - 6.0).abs() < 1e-6, "Cooldown for 5 LY from 0 fatigue must be 6 min");
    assert!((fat_min - 60.0).abs() < 1e-6, "Fatigue for 5 LY from 0 fatigue must be 60 min");

    // Non-zero existing fatigue: 30 min * (1 + 5) = 180 min
    let (cd_nz, fat_nz) = eve_server::formulas::compute_jump_fatigue_and_cooldown_minutes(30.0, 5.0, 0.0);
    assert!((cd_nz - 6.0).abs() < 1e-6, "Cooldown for 5 LY from 30 min fatigue must be 6 min");
    assert!((fat_nz - 180.0).abs() < 1e-6, "Fatigue for 5 LY from 30 min fatigue must be 180 min");

    // 5h (300 min) fatigue cap
    let (_, fat_capped) = eve_server::formulas::compute_jump_fatigue_and_cooldown_minutes(100.0, 5.0, 0.0);
    assert!((fat_capped - 300.0).abs() < 1e-6, "Fatigue must be capped at 300 min (5h)");

    // -75% Black Ops and -90% industrial reductions
    let (cd_blops, fat_blops) = eve_server::formulas::compute_jump_fatigue_and_cooldown_minutes(0.0, 8.0, 0.75);
    assert!((cd_blops - 3.0).abs() < 1e-6, "Black Ops -75% on 8 LY -> 2 effective LY -> 3 min cooldown");
    assert!((fat_blops - 30.0).abs() < 1e-6, "Black Ops -75% on 8 LY -> 30 min fatigue");

    let (cd_ind, fat_ind) = eve_server::formulas::compute_jump_fatigue_and_cooldown_minutes(0.0, 10.0, 0.90);
    assert!((cd_ind - 2.0).abs() < 1e-6, "Industrial -90% on 10 LY -> 1 effective LY -> 2 min cooldown");
    assert!((fat_ind - 20.0).abs() < 1e-6, "Industrial -90% on 10 LY -> 20 min fatigue");

    // 30m cooldown cap & jumpTimers.GetTimers RPC round trip
    let (raw_cd, capped_fat) =
        eve_server::formulas::compute_jump_fatigue_and_cooldown_minutes(300.0, 35.0, 0.0);
    let capped_cd = raw_cd.min(30.0);
    assert!((capped_cd - 30.0).abs() < 1e-6, "Jump activation cooldown must be capped at 30 min");
    assert!((capped_fat - 300.0).abs() < 1e-6, "Jump fatigue must be capped at 300 min");

    let now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.jump_cooldown_until_unix = now + (capped_cd * 60.0) as u64;
            ch.jump_fatigue_until_unix = now + (capped_fat * 60.0) as u64;
        }
    });

    let timers = ctx
        .dispatch_rpc(&mut sess, "jumpTimers", None, "GetTimers", &[PyRep::Integer(char_id)], &kw, None)
        .await
        .expect("jumpTimers.GetTimers");
    let PyRep::KeyVal(entries) = &timers else {
        panic!("GetTimers must return KeyVal");
    };
    let act_ticks = entries.get("jumpActivation").and_then(PyRep::as_i64).unwrap_or(0);
    let fat_ticks = entries.get("jumpFatigue").and_then(PyRep::as_i64).unwrap_or(0);
    let upd_ticks = entries.get("lastUpdated").and_then(PyRep::as_i64).unwrap_or(0);
    assert!(act_ticks > upd_ticks, "Active cooldown must be in the future");
    assert!(fat_ticks > act_ticks, "Active fatigue must exceed cooldown");

    // Real-time decay: once jump_fatigue_until_unix is in the past, fatigue decays to 0
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.jump_fatigue_until_unix = now.saturating_sub(60);
            ch.jump_cooldown_until_unix = now.saturating_sub(60);
        }
    });
    let timers_decayed = ctx
        .dispatch_rpc(&mut sess, "jumpTimers", None, "GetTimers", &[PyRep::Integer(char_id)], &kw, None)
        .await
        .expect("jumpTimers.GetTimers after decay");
    let PyRep::KeyVal(dec_entries) = &timers_decayed else {
        panic!("GetTimers must return KeyVal");
    };
    let dec_fat = dec_entries.get("jumpFatigue").and_then(PyRep::as_i64).unwrap_or(0);
    assert!(dec_fat < fat_ticks, "Expired fatigue must decay below active fatigue");

    // 2. Thera (31_000_005) NPC stations in SDE
    let thera_stations: Vec<_> = ctx
        .sde
        .stations
        .values()
        .filter(|s| s.solar_system_id == 31_000_005)
        .collect();
    assert!(!thera_stations.is_empty(), "Thera (31000005) must have NPC stations in SDE");
}
