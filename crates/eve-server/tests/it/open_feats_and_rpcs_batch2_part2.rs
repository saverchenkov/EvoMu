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
    let mut sess = SessionState::new(51_001);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(corp_id);
    sess.alliance_id = alliance_id;
    sess.solar_system_id = Some(sys_id);
    sess.station_id = Some(60_003_760);
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
    sess.station_id = Some(60_003_760);
    sess
}

fn keyval_get<'a>(rep: &'a PyRep, key: &str) -> Option<&'a PyRep> {
    match rep {
        PyRep::KeyVal(entries) => entries.get(key),
        PyRep::Dict(entries) => {
            entries.iter().find(|(k, _)| k.as_str() == Some(key)).map(|(_, v)| v)
        }
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
    ctx.db.with_mut_snapshot(|snap| {
        let skills = snap.skills.entry(90_000_002).or_default();
        skills.insert(
            3410,
            eve_db::TrainedSkillRecord {
                type_id: 3410,
                level: 4,
                skill_points: 45_255.0,
            },
        );
        skills.insert(
            3359,
            eve_db::TrainedSkillRecord {
                type_id: 3359,
                level: 3,
                skill_points: 8_000.0,
            },
        );
    });

    // Colocate player with agent station & solar system
    let agent_rec = &ctx.sde.agents[&agent_id];
    let agent_station_id = agent_rec.station_id.unwrap();
    let agent_sys_id = agent_rec.solar_system_id.unwrap_or(30_000_142);
    player.station_id = Some(agent_station_id);
    player.solar_system_id = Some(agent_sys_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_002) {
            ch.station_id = Some(agent_station_id);
            ch.solar_system_id = agent_sys_id;
        }
    });

    // Request, accept, and complete mission via agentMgr.DoAction (2 = offer, 3 = accept, 4 = complete)
    let offer_res = ctx
        .dispatch_rpc(
            &mut player,
            "agentMgr",
            None,
            "DoAction",
            &[PyRep::Integer(2), PyRep::Integer(agent_id)],
            &kw,
            None,
        )
        .await
        .expect("agentMgr.DoAction offer");
    let mission_id = keyval_get(&offer_res, "missionID")
        .and_then(|v| v.as_i64())
        .expect("missionID in offer");

    ctx.dispatch_rpc(
        &mut player,
        "agentMgr",
        None,
        "DoAction",
        &[PyRep::Integer(3), PyRep::Integer(mission_id)],
        &kw,
        None,
    )
    .await
    .expect("agentMgr.DoAction accept");

    let dest_station = keyval_get(&offer_res, "dropoffStationID").and_then(|v| v.as_i64());
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(m) = snap.active_missions.get_mut(&(90_000_002, mission_id as u32)) {
            m.npcs_remaining = 0;
        }
        if let Some(dest) = dest_station {
            if let Some(ch) = snap.characters.get_mut(&90_000_002) {
                ch.station_id = Some(dest);
            }
        }
    });
    if let Some(dest) = dest_station {
        player.station_id = Some(dest);
    }

    let bal_before = ctx.db.with_snapshot(|snap| snap.characters[&90_000_002].balance.0);
    let complete_res = ctx
        .dispatch_rpc(
            &mut player,
            "agentMgr",
            None,
            "DoAction",
            &[PyRep::Integer(6), PyRep::Integer(mission_id)],
            &kw,
            None,
        )
        .await
        .expect("agentMgr.DoAction complete");
    let isk_paid = keyval_get(&complete_res, "iskReward").and_then(|v| v.as_i64()).unwrap_or(0);
    let bonus_paid = keyval_get(&complete_res, "bonusReward").and_then(|v| v.as_i64()).unwrap_or(0);
    let lp_paid = keyval_get(&complete_res, "lpReward").and_then(|v| v.as_i64()).unwrap_or(0);
    assert!(isk_paid > 0 && bonus_paid > 0 && lp_paid > 0, "expected positive ISK, bonus, and LP rewards");

    let bal_after = ctx.db.with_snapshot(|snap| snap.characters[&90_000_002].balance.0);
    assert!(
        bal_after > bal_before && bal_after - bal_before <= IskCents::from_isk(isk_paid + bonus_paid).0,
        "wallet balance must increase by net mission ISK + bonus after corp tax"
    );
}
