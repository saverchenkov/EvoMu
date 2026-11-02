//! End-to-end integration tests for `work/open-feats-and-rpcs-batch2` (part 2).

use std::collections::HashMap;

use eve_db::{
    AllianceRecord, CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents,
    UpwellStructureRecord,
};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;

fn make_gm_session(char_id: i64, corp_id: i64, alliance_id: Option<i64>, sys_id: u32) -> SessionState {
    let mut sess = make_player_session(51_001, 1, char_id, corp_id, alliance_id, sys_id);
    sess.role = eve_db::service_roles::ROLE_ADMIN | eve_db::service_roles::ROLE_GMH
        | eve_machonet::service_roles::ROLE_QA | eve_machonet::service_roles::ROLE_PROGRAMMER;
    sess
}

fn make_player_session(sess_id: u64, user_id: u64, char_id: i64, corp_id: i64, alliance_id: Option<i64>, sys_id: u32) -> SessionState {
    let mut sess = SessionState::new(sess_id);
    sess.user_id = Some(user_id);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(corp_id);
    sess.alliance_id = alliance_id;
    sess.solar_system_id = Some(sys_id);
    sess.station_id = Some(60_003_760);
    sess
}

fn keyval_get<'a>(rep: &'a PyRep, key: &str) -> Option<&'a PyRep> {
    match rep {
        PyRep::KeyVal(entries) => entries.get(key),
        PyRep::Dict(entries) => entries.iter().find(|(k, _)| k.as_str() == Some(key)).map(|(_, v)| v),
        _ => None,
    }
}

#[tokio::test]
async fn test_milestones_seasons_and_cache_setting_e2e() {
    // parity: rpc:milestoneMgr.ProcessCharacterLogon, rpc:seasonManager.fill_up_and_return_challenges_for_character, rpc:cache.Setting
    let ctx = ServerContext::new();
    ctx.seed_demo_world_fixtures();
    let kw = HashMap::new();

    let mut unauth = SessionState::new(51_099);
    let mut gm = make_gm_session(90_000_001, 98_000_001, Some(99_000_001), 30_000_142);
    let mut player = make_player_session(51_002, 2, 90_000_002, 98_000_001, Some(99_000_001), 30_000_142);

    // 1. milestoneMgr.ProcessCharacterLogon & ClaimRewards negative/auth + state mutation
    assert!(
        ctx.dispatch_rpc(&mut unauth, "milestoneMgr", None, "ProcessCharacterLogon", &[], &kw, None)
            .await
            .is_err(),
        "unauthenticated ProcessCharacterLogon must fail"
    );
    let prereq_err = ctx
        .dispatch_rpc(&mut player, "milestoneMgr", None, "ClaimRewards", &[PyRep::Integer(1)], &kw, None)
        .await
        .expect_err("ClaimRewards before ProcessCharacterLogon must fail");
    assert_eq!(prereq_err.code, "MilestonePrerequisiteNotMet");

    let logon_res = ctx
        .dispatch_rpc(&mut player, "milestoneMgr", None, "ProcessCharacterLogon", &[], &kw, None)
        .await
        .expect("ProcessCharacterLogon");
    assert_eq!(logon_res, PyRep::None);

    // Calling ClaimRewards(0) after ProcessCharacterLogon now reaches milestone_id validation
    let invalid_id_err = ctx
        .dispatch_rpc(&mut player, "milestoneMgr", None, "ClaimRewards", &[PyRep::Integer(0)], &kw, None)
        .await
        .expect_err("ClaimRewards(0) must fail with InvalidMilestoneID");
    assert_eq!(invalid_id_err.code, "InvalidMilestoneID");

    // 2. seasonManager.fill_up_and_return_challenges_for_character & challenge lifecycle
    assert!(
        ctx.dispatch_rpc(
            &mut unauth,
            "seasonManager",
            None,
            "fill_up_and_return_challenges_for_character",
            &[],
            &kw,
            None,
        )
        .await
        .is_err(),
        "unauthenticated fill_up_and_return_challenges_for_character must fail"
    );

    let filled = ctx
        .dispatch_rpc(
            &mut player,
            "seasonManager",
            None,
            "fill_up_and_return_challenges_for_character",
            &[],
            &kw,
            None,
        )
        .await
        .expect("fill_up_and_return_challenges_for_character");
    let challenges = match filled {
        PyRep::List(list) => list,
        other => panic!("expected List of challenges, got {other:?}"),
    };
    assert!(!challenges.is_empty(), "expected non-empty active challenges");
    let first_ch_id = keyval_get(&challenges[0], "challengeID")
        .and_then(|v| v.as_i64())
        .expect("challengeID");

    // Claiming uncompleted challenge must fail
    let uncompleted_err = ctx
        .dispatch_rpc(
            &mut player,
            "seasonManager",
            None,
            "claim_challenge_rewards",
            &[PyRep::Integer(first_ch_id)],
            &kw,
            None,
        )
        .await
        .expect_err("uncompleted challenge claim must fail");
    assert_eq!(uncompleted_err.code, "ChallengeNotCompleted");

    // 3. cache.Setting auth, persistence, and default lookup
    let unauth_write = ctx
        .dispatch_rpc(
            &mut player,
            "cache",
            None,
            "Setting",
            &[
                PyRep::String("zsystem".into()),
                PyRep::String("Batch2Key".into()),
                PyRep::String("Batch2Val".into()),
            ],
            &kw,
            None,
        )
        .await
        .expect_err("non-GM cache.Setting write must fail");
    assert_eq!(unauth_write.code, "UserError");

    let gm_write = ctx
        .dispatch_rpc(
            &mut gm,
            "cache",
            None,
            "Setting",
            &[
                PyRep::String("zsystem".into()),
                PyRep::String("Batch2Key".into()),
                PyRep::String("Batch2Val".into()),
            ],
            &kw,
            None,
        )
        .await
        .expect("GM cache.Setting write");
    assert_eq!(gm_write, PyRep::String("Batch2Val".into()));

    let read_back = ctx
        .dispatch_rpc(
            &mut player,
            "cache",
            None,
            "Setting",
            &[PyRep::String("zsystem".into()), PyRep::String("Batch2Key".into())],
            &kw,
            None,
        )
        .await
        .expect("cache.Setting read");
    assert_eq!(read_back, PyRep::String("Batch2Val".into()));

    let default_ext = ctx
        .dispatch_rpc(
            &mut player,
            "cache",
            None,
            "Setting",
            &[PyRep::String("SP-Allowed-Extensions".into())],
            &kw,
            None,
        )
        .await
        .expect("cache.Setting default");
    assert_eq!(
        default_ext,
        PyRep::String("txt,csv,log,png,jpg,dds,json,yaml".into())
    );
}

#[tokio::test]
async fn test_search_and_paperdoll_apparel_e2e() {
    // parity: feat:ui.search, feat:cosmetics.apparel
    let ctx = ServerContext::new();
    ctx.seed_demo_world_fixtures();
    let kw = HashMap::new();

    let mut player = make_player_session(51_010, 2, 90_000_002, 98_000_001, Some(99_000_001), 30_000_142);

    // 1. search.Query & QuickQuery validation, exact/partial matching, and structure/system lookup
    let short_err = ctx
        .dispatch_rpc(&mut player, "search", None, "Query", &[PyRep::String("   ".into())], &kw, None)
        .await
        .expect_err("empty search query must fail");
    assert_eq!(short_err.code, "SearchStringTooShort");

    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.insert(
            98_000_555,
            CorporationRecord {
                corporation_id: 98_000_555,
                name: "Vanguard Horizon".into(),
                ticker: "VGHR".into(),
                ceo_id: 90_000_002,
                hq_station_id: Some(60_003_760),
                alliance_id: Some(99_000_555),
                ..Default::default()
            },
        );
        snap.alliances.insert(
            99_000_555,
            AllianceRecord {
                alliance_id: 99_000_555,
                name: "Horizon Coalition".into(),
                short_name: "HRZN".into(),
                executor_corp_id: 98_000_555,
                member_corp_ids: [98_000_555].into_iter().collect(),
            },
        );
        snap.structures.insert(
            1_000_000_000_555,
            UpwellStructureRecord {
                structure_id: 1_000_000_000_555,
                type_id: 35_832,
                owner_corp_id: 98_000_555,
                solar_system_id: 30_000_142,
                name: "Jita Horizon Astrahus".into(),
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
    });

    // Partial match on "Horizon" across corp (3), alliance (4), structure (11)
    let partial_res = ctx
        .dispatch_rpc(
            &mut player,
            "search",
            None,
            "Query",
            &[
                PyRep::String("Horizon".into()),
                PyRep::List(vec![PyRep::Integer(3), PyRep::Integer(4), PyRep::Integer(11)]),
                PyRep::Bool(false),
            ],
            &kw,
            None,
        )
        .await
        .expect("search.Query partial");
    let PyRep::Dict(groups) = partial_res else {
        panic!("expected Dict from search.Query");
    };
    let corp_hits = groups
        .iter()
        .find(|(k, _)| k.as_i64() == Some(3))
        .and_then(|(_, v)| match v {
            PyRep::List(l) => Some(l.iter().filter_map(|x| x.as_i64()).collect::<Vec<_>>()),
            _ => None,
        })
        .unwrap_or_default();
    assert!(corp_hits.contains(&98_000_555), "expected corp 98000555 in partial hits");

    // Exact match on "Horizon" should NOT match "Vanguard Horizon"
    let exact_res = ctx
        .dispatch_rpc(
            &mut player,
            "search",
            None,
            "Query",
            &[
                PyRep::String("Horizon".into()),
                PyRep::List(vec![PyRep::Integer(3)]),
                PyRep::Bool(true),
            ],
            &kw,
            None,
        )
        .await
        .expect("search.Query exact");
    let PyRep::Dict(exact_groups) = exact_res else {
        panic!("expected Dict from search.Query exact");
    };
    let exact_corps = exact_groups
        .iter()
        .find(|(k, _)| k.as_i64() == Some(3))
        .and_then(|(_, v)| match v {
            PyRep::List(l) => Some(l.iter().filter_map(|x| x.as_i64()).collect::<Vec<_>>()),
            _ => None,
        })
        .unwrap_or_default();
    assert!(!exact_corps.contains(&98_000_555), "exact match must exclude partial corp name");

    // QuickQuery for Jita solar system (group 7)
    let quick_jita = ctx
        .dispatch_rpc(
            &mut player,
            "search",
            None,
            "QuickQuery",
            &[PyRep::String("Jita".into()), PyRep::List(vec![PyRep::Integer(7)]), PyRep::Bool(true)],
            &kw,
            None,
        )
        .await
        .expect("search.QuickQuery Jita");
    let PyRep::List(jita_ids) = quick_jita else {
        panic!("expected List from QuickQuery");
    };
    assert!(jita_ids.iter().any(|x| x.as_i64() == Some(30_000_142)));

    // 2. paperDollServer apparel wear & persistence
    let apparel_type_id = ctx
        .sde
        .types
        .values()
        .find(|t| t.category_id == 30 && t.published)
        .map(|t| t.type_id)
        .expect("published category 30 apparel type in SDE");

    // Unowned apparel must fail
    let unowned_doll = PyRep::keyval([
        ("dollDNA", PyRep::String("dna:test".into())),
        ("apparel", PyRep::List(vec![PyRep::Integer(i64::from(apparel_type_id))])),
    ]);
    let unowned_err = ctx
        .dispatch_rpc(
            &mut player,
            "paperDollServer",
            None,
            "UpdateExistingCharacterLimited",
            &[PyRep::Integer(90_000_002), unowned_doll],
            &kw,
            None,
        )
        .await
        .expect_err("unowned apparel must fail");
    assert_eq!(unowned_err.code, "InvalidApparelItem");

    // Grant owned apparel item in Hangar and wear it
    let apparel_item_id = 880_999_001;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            apparel_item_id,
            InventoryItemRecord {
                item_id: apparel_item_id,
                type_id: apparel_type_id,
                owner_id: 90_000_002,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });

    let owned_doll = PyRep::keyval([
        ("dollDNA", PyRep::String("dna:worn".into())),
        ("apparel", PyRep::List(vec![PyRep::Integer(apparel_item_id)])),
    ]);
    let update_res = ctx
        .dispatch_rpc(
            &mut player,
            "paperDollServer",
            None,
            "UpdateExistingCharacterLimited",
            &[PyRep::Integer(90_000_002), owned_doll],
            &kw,
            None,
        )
        .await
        .expect("UpdateExistingCharacterLimited");
    assert_eq!(keyval_get(&update_res, "updated"), Some(&PyRep::Bool(true)));

    ctx.db.with_snapshot(|snap| {
        let it = snap.items.get(&apparel_item_id).expect("apparel item");
        assert_eq!(it.flag, InventoryFlag::Wardrobe, "worn apparel must move to Wardrobe flag");
    });

    let doll_data = ctx
        .dispatch_rpc(&mut player, "paperDollServer", None, "GetMyPaperDollData", &[], &kw, None)
        .await
        .expect("GetMyPaperDollData");
    let worn = match keyval_get(&doll_data, "wornApparel") {
        Some(PyRep::List(l)) => l.iter().filter_map(|v| v.as_i64()).collect::<Vec<_>>(),
        other => panic!("expected wornApparel List, got {other:?}"),
    };
    assert_eq!(worn, vec![i64::from(apparel_type_id)]);
}

#[tokio::test]
async fn test_insurance_void_expiry_and_mission_rewards_e2e() {
    // parity: feat:combat.insurance-void-and-expiry, feat:pve.mission-lp-isk-reward-formula, feat:pve.mission-complete
    let ctx = ServerContext::new();
    ctx.seed_demo_world_fixtures();
    let kw = HashMap::new();

    let mut player = make_player_session(51_020, 2, 90_000_002, 98_000_001, Some(99_000_001), 30_000_142);

    // 1. Insurance 12-week contract, repackage voiding, and destruction payout + EVE-mail
    let spare_ship_id = 880_999_101;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_002) {
            ch.balance = IskCents::from_isk(50_000_000);
        }
        snap.items.insert(
            spare_ship_id,
            InventoryItemRecord {
                item_id: spare_ship_id,
                type_id: 587, // Rifter
                owner_id: 90_000_002,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });

    // Insure at Platinum (1.0 fraction)
    let insure_res = ctx
        .dispatch_rpc(
            &mut player,
            "insuranceSvc",
            None,
            "InsureShip",
            &[PyRep::Integer(spare_ship_id), PyRep::Real(1.0)],
            &kw,
            None,
        )
        .await
        .expect("InsureShip");
    assert_eq!(
        keyval_get(&insure_res, "levelName"),
        Some(&PyRep::String("Platinum".into()))
    );

    let contract = ctx
        .dispatch_rpc(
            &mut player,
            "insuranceSvc",
            None,
            "GetContractForShip",
            &[PyRep::Integer(spare_ship_id)],
            &kw,
            None,
        )
        .await
        .expect("GetContractForShip");
    let start_date = keyval_get(&contract, "startDate").and_then(|v| v.as_i64()).unwrap_or(0);
    let end_date = keyval_get(&contract, "endDate").and_then(|v| v.as_i64()).unwrap_or(0);
    let twelve_weeks_ticks = 12 * 7 * 86_400 * 10_000_000_i64;
    assert_eq!(end_date - start_date, twelve_weeks_ticks, "insurance must last 12 weeks");

    // Repackaging voids insurance
    ctx.dispatch_rpc(
        &mut player,
        "repackagingSvc",
        None,
        "RepackageItems",
        &[PyRep::List(vec![PyRep::Integer(spare_ship_id)])],
        &kw,
        None,
    )
    .await
    .expect("RepackageItems");

    let after_repack = ctx
        .dispatch_rpc(
            &mut player,
            "insuranceSvc",
            None,
            "GetContractForShip",
            &[PyRep::Integer(spare_ship_id)],
            &kw,
            None,
        )
        .await
        .expect("GetContractForShip after repackage");
    assert_eq!(after_repack, PyRep::None, "repackaging must void insurance contract");

    // 2. Mission LP/ISK reward formula & completion payout
    let agent_id = ctx
        .sde
        .agents
        .values()
        .find(|a| a.level == 1 && a.agent_type_id == 2 && a.division_id == 24 && a.station_id.unwrap_or(0) > 0)
        .or_else(|| {
            ctx.sde
                .agents
                .values()
                .find(|a| a.level == 1 && a.agent_type_id == 2 && a.station_id.unwrap_or(0) > 0)
        })
        .map(|a| a.agent_id)
        .expect("level 1 station agent in SDE");

    // Grant Negotiation (3410) level 4 and Connections (3359) level 3
    let agent_rec = &ctx.sde.agents[&agent_id];
    let agent_station_id = agent_rec.station_id.unwrap();
    let agent_sys_id = agent_rec.solar_system_id.unwrap_or(30_000_142);
    player.station_id = Some(agent_station_id);
    player.solar_system_id = Some(agent_sys_id);
    ctx.db.with_mut_snapshot(|snap| {
        let skills = snap.skills.entry(90_000_002).or_default();
        skills.insert(3410, eve_db::TrainedSkillRecord { type_id: 3410, level: 4, skill_points: 45_255.0 });
        skills.insert(3359, eve_db::TrainedSkillRecord { type_id: 3359, level: 3, skill_points: 8_000.0 });
        if let Some(ch) = snap.characters.get_mut(&90_000_002) {
            ch.station_id = Some(agent_station_id);
            ch.solar_system_id = agent_sys_id;
        }
    });

    let offer_res = ctx.dispatch_rpc(&mut player, "agentMgr", None, "DoAction", &[PyRep::Integer(2), PyRep::Integer(agent_id)], &kw, None).await.expect("offer");
    let mission_id = keyval_get(&offer_res, "missionID").and_then(|v| v.as_i64()).expect("missionID");
    ctx.dispatch_rpc(&mut player, "agentMgr", None, "DoAction", &[PyRep::Integer(3), PyRep::Integer(mission_id)], &kw, None).await.expect("accept");
    let dest_station = keyval_get(&offer_res, "dropoffStationID").and_then(|v| v.as_i64());
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(m) = snap.active_missions.get_mut(&(90_000_002, mission_id as u32)) { m.npcs_remaining = 0; }
        if let Some(dest) = dest_station { if let Some(ch) = snap.characters.get_mut(&90_000_002) { ch.station_id = Some(dest); } }
    });
    if let Some(dest) = dest_station { player.station_id = Some(dest); }
    let bal_before = ctx.db.with_snapshot(|snap| snap.characters[&90_000_002].balance.0);
    let complete_res = ctx.dispatch_rpc(&mut player, "agentMgr", None, "DoAction", &[PyRep::Integer(6), PyRep::Integer(mission_id)], &kw, None).await.expect("complete");
    let isk_paid = keyval_get(&complete_res, "iskReward").and_then(|v| v.as_i64()).unwrap_or(0);
    let bonus_paid = keyval_get(&complete_res, "bonusReward").and_then(|v| v.as_i64()).unwrap_or(0);
    let lp_paid = keyval_get(&complete_res, "lpReward").and_then(|v| v.as_i64()).unwrap_or(0);
    assert!(isk_paid > 0 && bonus_paid > 0 && lp_paid > 0);
    let bal_after = ctx.db.with_snapshot(|snap| snap.characters[&90_000_002].balance.0);
    assert!(bal_after > bal_before && bal_after - bal_before <= IskCents::from_isk(isk_paid + bonus_paid).0);
}

#[tokio::test]
async fn test_jump_fatigue_decay_bridges_and_hulls_e2e() {
    // parity: feat:navigation.jump-fatigue-formula
    let ctx = ServerContext::new();
    ctx.seed_demo_world_fixtures();
    for tid in [28606_u32, 42244, 670, 672, 648, 12729, 12753, 20185, 28846, 28352] {
        assert!((eve_server::services::space_destiny::space_nav_travel::ship_fatigue_reduction_bonus(&ctx, tid) - 0.90).abs() < 1e-6);
    }
    assert!((eve_server::services::space_destiny::space_nav_travel::ship_fatigue_reduction_bonus(&ctx, 22428) - 0.75).abs() < 1e-6);

    let kw = HashMap::new();
    let (cyno_char, bridger_char, passenger_char) = (90_000_002_i64, 90_000_003_i64, 90_000_001_i64);
    let (src_sys, dst_sys) = (30_000_142_u32, 30_000_204_u32);
    let (cyno_ship, cyno_mod, bridger_ship, portal_mod, bridger_fuel, passenger_ship) =
        (990_010_001_i64, 990_010_002, 990_010_011, 990_010_012, 990_010_013, 990_010_021);

    ctx.db.with_mut_snapshot(|snap| {
        if !snap.characters.contains_key(&bridger_char) {
            if let Some(mut c3) = snap.characters.get(&passenger_char).cloned() {
                c3.character_id = bridger_char;
                snap.characters.insert(bridger_char, c3);
            }
        }
        for (cid, sid, sys) in [
            (cyno_char, cyno_ship, dst_sys),
            (bridger_char, bridger_ship, src_sys),
            (passenger_char, passenger_ship, src_sys),
        ] {
            if let Some(ch) = snap.characters.get_mut(&cid) {
                ch.solar_system_id = sys;
                ch.station_id = None;
                ch.structure_id = None;
                ch.active_ship_id = sid;
                ch.jump_cooldown_until_unix = 0;
                ch.jump_fatigue_until_unix = 0;
                ch.jump_fatigue_minutes = 0.0;
            }
            let sk = snap.skills.entry(cid).or_default();
            for stid in [21603_u32, 24562, 3456, 21610, 21611] {
                sk.insert(stid, eve_db::TrainedSkillRecord { type_id: stid, level: 5, skill_points: 256_000.0 });
            }
        }
        let mut member_details = HashMap::new();
        for cid in [cyno_char, bridger_char, passenger_char] {
            member_details.insert(cid, eve_db::FleetMemberRecord {
                character_id: cid, role: eve_db::FleetRole::SquadMember,
                wing_id: Some(1), squad_id: Some(1), takes_fleet_warp: true,
                accepts_regroup: true, accepts_conduit_jumps: true, job: 0,
                role_name: String::new(), join_time_unix: 0,
            });
        }
        snap.fleets.insert(77_001, eve_db::FleetRecord {
            fleet_id: 77_001, boss_id: bridger_char, free_move: true,
            wings: vec![1], squads: vec![1],
            members: vec![cyno_char, bridger_char, passenger_char],
            member_details, ..Default::default()
        });
        for (id, tid, owner, loc, flag, qty) in [
            (cyno_ship, 12032_u32, cyno_char, i64::from(dst_sys), InventoryFlag::None, 1_i32),
            (cyno_mod, 28646, cyno_char, cyno_ship, InventoryFlag::HiSlot0, 1),
            (990_010_003, 16273, cyno_char, cyno_ship, InventoryFlag::Cargo, 1_000),
            (bridger_ship, 22428, bridger_char, i64::from(src_sys), InventoryFlag::None, 1),
            (portal_mod, 28652, bridger_char, bridger_ship, InventoryFlag::HiSlot0, 1),
            (bridger_fuel, 16274, bridger_char, bridger_ship, InventoryFlag::Cargo, 50_000),
            (passenger_ship, 12032, passenger_char, i64::from(src_sys), InventoryFlag::None, 1),
        ] {
            snap.items.insert(id, InventoryItemRecord {
                item_id: id, type_id: tid, owner_id: owner, location_id: loc,
                flag, quantity: qty, singleton: qty == 1, ..Default::default()
            });
        }
        snap.module_online_states.insert(cyno_mod, true);
        snap.module_online_states.insert(portal_mod, true);
    });
    for (cid, sid, tid, sys) in [
        (cyno_char, cyno_ship, 12032_u32, dst_sys),
        (bridger_char, bridger_ship, 22428, src_sys),
        (passenger_char, passenger_ship, 12032, src_sys),
    ] {
        let ball = eve_destiny::Ball {
            id: sid, type_id: tid, position: glam::DVec3::ZERO, velocity: glam::DVec3::ZERO,
            max_velocity: 150.0, inertia: 1.0, mass: 1_000_000.0, radius: 50.0,
            speed_fraction: 0.0, mode: eve_destiny::MovementMode::Stop,
        };
        let cs = ctx.build_ship_combat_state(cid, sid, tid);
        let meta = ctx.build_ship_slim_meta(cid, tid);
        ctx.systems.get_or_spawn(sys).spawn_ball_with_meta(ball, cs, meta).await.expect("spawn ship");
    }

    let mut cyno_sess = make_player_session(51_101, 101, cyno_char, 98_000_001, None, dst_sys);
    cyno_sess.ship_id = Some(cyno_ship); cyno_sess.station_id = None;
    let mut bridger_sess = make_player_session(51_102, 102, bridger_char, 98_000_001, None, src_sys);
    bridger_sess.ship_id = Some(bridger_ship); bridger_sess.station_id = None;
    let mut pass_sess = make_player_session(51_103, 103, passenger_char, 98_000_001, None, src_sys);
    pass_sess.ship_id = Some(passenger_ship); pass_sess.station_id = None;

    let cyno_res = ctx
        .dispatch_rpc(&mut cyno_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(cyno_mod), PyRep::String("cynosuralGeneration".into())], &kw, None)
        .await
        .expect("activate covert cyno");
    let beacon_id = if let PyRep::KeyVal(entries) = &cyno_res {
        entries.iter().find(|(k, _)| *k == "beaconID").and_then(|(_, v)| match v { PyRep::Integer(n) => Some(*n), _ => None }).unwrap_or(cyno_ship)
    } else {
        cyno_ship
    };

    ctx.dispatch_rpc(&mut bridger_sess, "beyonce", None, "CmdBridgeToFleetModuleBeacon", &[PyRep::Integer(cyno_char), PyRep::Integer(i64::from(dst_sys)), PyRep::Integer(beacon_id)], &kw, None)
        .await
        .expect("open covert bridge");

    eve_server::services::space_destiny::clear_session_change_timer(&ctx, passenger_char);
    ctx.dispatch_rpc(&mut pass_sess, "beyonce", None, "CmdJumpThroughFleet", &[PyRep::Integer(bridger_char), PyRep::Integer(bridger_ship)], &kw, None)
        .await
        .expect("passenger jump through covert portal");
    assert_eq!(pass_sess.solar_system_id, Some(dst_sys));

    let timers = ctx.dispatch_rpc(&mut pass_sess, "jumpTimers", None, "GetTimers", &[], &kw, None).await.expect("GetTimers");
    assert!(!matches!(timers, PyRep::None));
    assert!(ctx.db.with_snapshot(|s| s.characters[&passenger_char].jump_fatigue_minutes > 0.0));
    let (cd1, fat1) = eve_server::formulas::compute_jump_fatigue_and_cooldown_minutes(0.0, 5.0, 0.0);
    let (cd2, fat2) = eve_server::formulas::compute_jump_fatigue_and_cooldown_minutes(50.0, 5.0, 0.0);
    let (cd3, fat3) = eve_server::formulas::compute_jump_fatigue_and_cooldown_minutes(270.0, 5.0, 0.0);
    assert!((cd1 - 6.0).abs() < 1e-6 && (fat1 - 60.0).abs() < 1e-6 && (cd2 - 6.0).abs() < 1e-6 && (fat2 - 300.0).abs() < 1e-6 && (cd3 - 27.0).abs() < 1e-6 && (fat3 - 300.0).abs() < 1e-6);
}
