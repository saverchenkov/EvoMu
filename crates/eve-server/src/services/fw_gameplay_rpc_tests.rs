//! Unit tests for `fw_gameplay_rpc` (`facWarMgr` enlistment, LP donations, leaderboards, combat kills, War HQ).

#![forbid(unsafe_code)]

use super::*;
use crate::router::ServerContext;
use eve_db::snapshot_ext::{FwCharacterStatsRecord, FwSystemOccupationRecord, FwWindowedStat};
use eve_db::{AllianceRecord, CharacterRecord, CorporationRecord, IskCents};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn test_ctx() -> ServerContext {
    if let Some(sde) = SHARED_SDE.get() {
        ServerContext::with_sde_and_repository(
            Arc::clone(sde),
            Arc::new(eve_db::WorldRepository::with_demo_seed()),
        )
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
    } else {
        let ctx = ServerContext::new();
        let _ = SHARED_SDE.set(Arc::clone(&ctx.sde));
        ctx
    }
}

fn fw_char(
    snap: &eve_db::WorldStateSnapshot,
    char_id: i64,
    corp_id: i64,
    alliance_id: Option<i64>,
    fac_id: Option<i64>,
    sys_id: u32,
) -> CharacterRecord {
    let mut c = snap.characters.values().next().unwrap().clone();
    c.character_id = char_id;
    c.name = format!("FwPilot_{char_id}");
    c.corporation_id = corp_id;
    c.alliance_id = alliance_id;
    c.war_faction_id = fac_id;
    c.solar_system_id = sys_id;
    c.station_id = None;
    c.active_ship_id = 0;
    c.balance = IskCents(20_000_000_00);
    c.loyalty_points.clear();
    c
}

fn make_corp(corp_id: i64, ceo_id: i64, alliance_id: Option<i64>) -> CorporationRecord {
    let mut c = CorporationRecord::default();
    c.corporation_id = corp_id;
    c.ceo_id = ceo_id;
    c.alliance_id = alliance_id;
    c.name = format!("Corp_{corp_id}");
    c.ticker = format!("C{}", corp_id % 100);
    c.ensure_wallet_divisions();
    c
}

#[test]
fn war_hq_services_combat_kills_and_ihub_lp_donations() {
    let ctx = test_ctx();
    let (name, svcs) = get_war_hq_services(HQ_SYSTEM_MEHATOOR).expect("Mehatoor War HQ");
    assert!(name.contains("Mehatoor"));
    assert_eq!(svcs.len(), 3);
    assert!(get_war_hq_services(HQ_SYSTEM_ONNAMON).is_some());
    assert!(get_war_hq_services(HQ_SYSTEM_INTAKI).is_some());
    assert!(get_war_hq_services(HQ_SYSTEM_AMO).is_some());
    assert!(get_war_hq_services(30000142).is_none());

    let sys_id = 30002505_u32;
    let killer_id = 920_001_i64;
    let victim_id = 920_002_i64;
    let cal_lp_corp = militia_lp_corp_id(&ctx.sde, 500_001);

    ctx.db.with_mut_snapshot(|snap| {
        snap.fw_system_occupations.insert(
            sys_id,
            FwSystemOccupationRecord {
                solar_system_id: sys_id,
                owner_faction_id: 500_001,
                occupier_faction_id: 500_001,
                victory_points: 100,
                victory_points_threshold: 3_000,
            },
        );
        let mut k = fw_char(snap, killer_id, 98_000_001, None, Some(500_001), sys_id);
        k.loyalty_points.insert(cal_lp_corp, 100_000);
        let v = fw_char(snap, victim_id, 98_000_002, None, Some(500_004), sys_id);
        snap.characters.insert(killer_id, k);
        snap.characters.insert(victim_id, v);
    });

    on_player_ship_destroyed_in_combat(&ctx, sys_id, killer_id, killer_id, 587);
    on_player_ship_destroyed_in_combat(&ctx, sys_id, killer_id, victim_id, 670);
    let lp_before = ctx.db.with_snapshot(|s| s.characters[&killer_id].loyalty_points[&cal_lp_corp]);
    on_player_ship_destroyed_in_combat_detailed(&ctx, sys_id, killer_id, victim_id, 587, 5_000_000.0, 0);
    let lp_after = ctx.db.with_snapshot(|s| s.characters[&killer_id].loyalty_points[&cal_lp_corp]);
    assert!(lp_after > lp_before);

    assert_eq!(
        donate_lp_to_fw_ihub(&ctx, killer_id, sys_id, 0).unwrap_err().code,
        "InvalidLpDonation"
    );
    assert_eq!(
        donate_lp_to_fw_ihub(&ctx, 999_999, sys_id, 100).unwrap_err().code,
        "CharacterNotFound"
    );
    assert_eq!(
        donate_lp_to_fw_ihub(&ctx, victim_id, sys_id, 100).unwrap_err().code,
        "NotEnlistedInOccupyingMilitia"
    );
    let (total_lp, tier, tax, fac_bonus) =
        donate_lp_to_fw_ihub(&ctx, killer_id, sys_id, 60_000).expect("LP donation succeeds");
    assert_eq!(total_lp, 60_000);
    assert!(tier >= 1);
    assert!(tax >= 0.0 && fac_bonus >= 0.0);
}

#[test]
fn rpc_corp_and_alliance_enlistment_and_withdrawal() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let ctx = test_ctx();
    let ceo_id = 920_010_i64;
    let member_id = 920_011_i64;
    let corp_id = 98_000_010_i64;
    let extra_corp = 98_000_011_i64;
    let ally_id = 99_000_010_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let c1 = fw_char(snap, ceo_id, corp_id, Some(ally_id), None, 30002505);
        let c2 = fw_char(snap, member_id, corp_id, Some(ally_id), None, 30002505);
        snap.characters.insert(ceo_id, c1);
        snap.characters.insert(member_id, c2);
        snap.corporations
            .insert(corp_id, make_corp(corp_id, ceo_id, Some(ally_id)));
        snap.corporations
            .insert(extra_corp, make_corp(extra_corp, 999, Some(ally_id)));
        let mut members = HashSet::new();
        members.insert(corp_id);
        members.insert(extra_corp);
        snap.alliances.insert(
            ally_id,
            AllianceRecord {
                alliance_id: ally_id,
                name: "Test FW Alliance".into(),
                short_name: "TFWA".into(),
                executor_corp_id: corp_id,
                member_corp_ids: members,
                ..Default::default()
            },
        );
    });

    let mut member_sess = SessionState::default();
    member_sess.user_id = Some(2);
    member_sess.character_id = Some(member_id);
    member_sess.corporation_id = Some(corp_id);
    member_sess.alliance_id = Some(ally_id);

    let empty_kw = HashMap::new();
    let denied = rt
        .block_on(handle(
            &ctx,
            &mut member_sess,
            "facWarMgr",
            "JoinFactionAsCorporation",
            &[PyRep::Integer(500_001)],
            &empty_kw,
        ))
        .unwrap()
        .unwrap_err();
    assert_eq!(denied.code, "AccessDenied");

    let mut ceo_sess = SessionState::default();
    ceo_sess.user_id = Some(1);
    ceo_sess.character_id = Some(ceo_id);
    ceo_sess.corporation_id = Some(corp_id);
    ceo_sess.alliance_id = Some(ally_id);

    let bad_fac = rt
        .block_on(handle(
            &ctx,
            &mut ceo_sess,
            "facWarMgr",
            "JoinFactionAsCorporation",
            &[PyRep::Integer(999_999)],
            &empty_kw,
        ))
        .unwrap()
        .unwrap_err();
    assert_eq!(bad_fac.code, "InvalidFWMilitiaFaction");

    let joined_corp = rt
        .block_on(handle(
            &ctx,
            &mut ceo_sess,
            "facWarMgr",
            "JoinFactionAsCorporation",
            &[PyRep::Integer(500_001)],
            &empty_kw,
        ))
        .unwrap()
        .expect("enlist corp");
    assert_eq!(joined_corp, PyRep::Bool(true));

    let corp_cooldown = rt
        .block_on(handle(
            &ctx,
            &mut ceo_sess,
            "facWarMgr",
            "LeaveFactionAsCorporation",
            &[],
            &empty_kw,
        ))
        .unwrap()
        .unwrap_err();
    assert_eq!(corp_cooldown.code, "MilitiaCooldownActive");

    ctx.advance_secs(90_000);
    assert_eq!(
        rt.block_on(handle(
            &ctx,
            &mut ceo_sess,
            "facWarMgr",
            "LeaveFactionAsCorporation",
            &[],
            &empty_kw,
        ))
        .unwrap()
        .unwrap(),
        PyRep::Bool(true)
    );

    ctx.advance_secs(90_000);
    let joined_ally = rt
        .block_on(handle(
            &ctx,
            &mut ceo_sess,
            "facWarMgr",
            "JoinFactionAsAlliance",
            &[PyRep::Integer(500_004)],
            &empty_kw,
        ))
        .unwrap()
        .expect("enlist alliance");
    assert_eq!(joined_ally, PyRep::Bool(true));
    assert_eq!(ceo_sess.war_faction_id, Some(500_004));
    ctx.db.with_snapshot(|s| {
        assert_eq!(s.fw_runtime.corp_war_factions.get(&corp_id), Some(&500_004));
        assert_eq!(s.fw_runtime.corp_war_factions.get(&extra_corp), Some(&500_004));
    });

    ctx.advance_secs(90_000);
    let left_ally = rt
        .block_on(handle(
            &ctx,
            &mut ceo_sess,
            "facWarMgr",
            "LeaveFactionAsAlliance",
            &[],
            &empty_kw,
        ))
        .unwrap()
        .expect("withdraw alliance");
    assert_eq!(left_ally, PyRep::Bool(true));
    assert_eq!(ceo_sess.war_faction_id, None);
    ctx.db.with_snapshot(|s| {
        assert!(!s.fw_runtime.corp_war_factions.contains_key(&corp_id));
        assert!(!s.fw_runtime.corp_war_factions.contains_key(&extra_corp));
    });
}

#[test]
fn rpc_leaderboards_rank_status_withdraw_stubs_and_unknown() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let ctx = test_ctx();
    let cid = 920_020_i64;
    let corp_id = 98_000_020_i64;
    let ally_id = 99_000_020_i64;
    let sys_id = 30002505_u32;

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = fw_char(snap, cid, corp_id, Some(ally_id), Some(500_002), sys_id);
        ch.loyalty_points.insert(1_000_182, 10_000);
        snap.characters.insert(cid, ch);
        snap.fw_runtime.corp_war_factions.insert(corp_id, 500_002);
        snap.fw_character_stats.insert(
            cid,
            FwCharacterStatsRecord {
                character_id: cid,
                faction_id: Some(500_002),
                enlisted_on_unix: Some(1_000_000),
                current_rank: 4,
                highest_rank: 5,
                kills: FwWindowedStat {
                    yesterday: 2,
                    last_week: 7,
                    total: 25,
                },
                victory_points: FwWindowedStat {
                    yesterday: 100,
                    last_week: 500,
                    total: 2_500,
                },
            },
        );
    });

    let mut sess = SessionState::default();
    sess.user_id = Some(1);
    sess.character_id = Some(cid);
    sess.corporation_id = Some(corp_id);
    sess.alliance_id = Some(ally_id);
    sess.war_faction_id = Some(500_002);
    sess.solar_system_id = Some(sys_id);
    let kw = HashMap::new();

    let top = rt
        .block_on(handle(&ctx, &mut sess, "facWarMgr", "GetStats_TopAndAllKillsAndVPs", &[], &kw))
        .unwrap()
        .unwrap();
    assert!(top.dict_get("characters").is_some());
    assert!(top.dict_get("corporations").is_some());

    assert!(rt
        .block_on(handle(&ctx, &mut sess, "facWarMgr", "GetStats_Character", &[], &kw))
        .unwrap()
        .is_ok());
    assert!(rt
        .block_on(handle(&ctx, &mut sess, "facWarMgr", "GetStats_Corp", &[], &kw))
        .unwrap()
        .is_ok());
    assert!(rt
        .block_on(handle(&ctx, &mut sess, "facWarMgr", "GetStats_Alliance", &[], &kw))
        .unwrap()
        .is_ok());
    assert!(rt
        .block_on(handle(&ctx, &mut sess, "facWarMgr", "GetStats_CorpPilots", &[], &kw))
        .unwrap()
        .is_ok());
    assert!(rt
        .block_on(handle(&ctx, &mut sess, "facWarMgr", "GetStats_FactionInfo", &[], &kw))
        .unwrap()
        .is_ok());
    assert!(rt
        .block_on(handle(&ctx, &mut sess, "facWarMgr", "GetStats_Militia", &[], &kw))
        .unwrap()
        .is_ok());

    let rank_info = rt
        .block_on(handle(&ctx, &mut sess, "facWarMgr", "GetCharacterRankInfo", &[], &kw))
        .unwrap()
        .unwrap();
    assert_eq!(rank_info.dict_get("currentRank").and_then(|v| v.as_i64()), Some(4));
    assert_eq!(rank_info.dict_get("highestRank").and_then(|v| v.as_i64()), Some(5));

    assert_eq!(
        rt.block_on(handle(&ctx, &mut sess, "facWarMgr", "CheckForRankChange", &[], &kw))
            .unwrap()
            .unwrap(),
        PyRep::Integer(4)
    );

    assert_eq!(
        rt.block_on(handle(
            &ctx,
            &mut sess,
            "facWarMgr",
            "GetFactionMilitiaCorporation",
            &[PyRep::Integer(500_002)],
            &kw,
        ))
        .unwrap()
        .unwrap(),
        PyRep::Integer(militia_lp_corp_id(&ctx.sde, 500_002))
    );

    assert_eq!(
        rt.block_on(handle(&ctx, &mut sess, "facWarMgr", "RefreshCorps", &[], &kw))
            .unwrap()
            .unwrap(),
        PyRep::None
    );
    assert_eq!(
        rt.block_on(handle(
            &ctx,
            &mut sess,
            "facWarMgr",
            "WithdrawJoinFactionAsCorporation",
            &[],
            &kw,
        ))
        .unwrap()
        .unwrap_err()
        .code,
        "AccessDenied"
    );
    sess.corp_role = eve_db::corp_roles::ROLE_DIRECTOR;
    assert_eq!(
        rt.block_on(handle(
            &ctx,
            &mut sess,
            "facWarMgr",
            "WithdrawJoinFactionAsCorporation",
            &[],
            &kw,
        ))
        .unwrap()
        .unwrap(),
        PyRep::Bool(false)
    );
    let corp_id = sess.corporation_id.unwrap_or(98_000_001);
    ctx.db.with_mut_snapshot(|snap| {
        let mut q = FwPendingQueue::default();
        q.corp_joins.insert(corp_id, 500_001);
        save_fw_pending(snap, &q);
    });
    assert_eq!(
        rt.block_on(handle(
            &ctx,
            &mut sess,
            "facWarMgr",
            "WithdrawJoinFactionAsCorporation",
            &[PyRep::Integer(500_001)],
            &kw,
        ))
        .unwrap()
        .unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(
        rt.block_on(handle(
            &ctx,
            &mut sess,
            "facWarMgr",
            "WithdrawJoinFactionAsCorporation",
            &[],
            &kw,
        ))
        .unwrap()
        .unwrap(),
        PyRep::Bool(false)
    );
    assert!(rt
        .block_on(handle(&ctx, &mut sess, "facWarMgr", "", &[], &kw))
        .is_none());
}

#[test]
fn fw_gameplay_rpc_stats_alliance_errors_supply_defense_and_ihub_regen() {
    use eve_db::PersistedFwStatEvent;

    let ctx = test_ctx();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let kw = HashMap::new();
    let sys_id = 30_002_505_u32;
    let exec_cid = 940_001_i64;
    let mem_cid = 940_002_i64;
    let unenl_cid = 940_003_i64;
    let exec_corp = 98_444_001_i64;
    let mem_corp = 98_444_002_i64;
    let ally_id = 99_444_001_i64;
    let ship_id = 100_940_001_i64;
    let supply_id = 555_101_i64;
    let complex_gid = 777_i64;
    let now = ctx.now_unix();

    ctx.db.with_mut_snapshot(|snap| {
        let mut c1 = fw_char(snap, exec_cid, exec_corp, Some(ally_id), None, sys_id);
        c1.active_ship_id = ship_id;
        c1.loyalty_points.insert(militia_lp_corp_id(&ctx.sde, 500_001), 10_000);
        let c2 = fw_char(snap, mem_cid, mem_corp, Some(ally_id), None, sys_id);
        let c3 = fw_char(snap, unenl_cid, 1_000_125, None, None, sys_id);
        snap.characters.insert(exec_cid, c1);
        snap.characters.insert(mem_cid, c2);
        snap.characters.insert(unenl_cid, c3);
        snap.corporations.insert(exec_corp, make_corp(exec_corp, exec_cid, Some(ally_id)));
        snap.corporations.insert(mem_corp, make_corp(mem_corp, mem_cid, Some(ally_id)));
        snap.alliances.insert(
            ally_id,
            AllianceRecord {
                alliance_id: ally_id,
                name: "Fw Rpc Ally".into(),
                short_name: "FWR".into(),
                executor_corp_id: exec_corp,
                member_corp_ids: [exec_corp, mem_corp].into_iter().collect(),
            },
        );
        snap.fw_system_occupations.insert(
            sys_id,
            FwSystemOccupationRecord {
                solar_system_id: sys_id,
                owner_faction_id: 500_001,
                occupier_faction_id: 500_001,
                victory_points: 1000,
                victory_points_threshold: 3000,
            },
        );
        snap.fw_runtime.supply_sites.insert(supply_id, (sys_id, "SupplyDepot".into(), 500_001));
        snap.fw_runtime.fw_complexes.insert(complex_gid, Default::default());
        snap.fw_runtime.historical_char_totals.insert(exec_cid, (2, 200));
        snap.fw_runtime.historical_corp_totals.insert(exec_corp, (3, 300));
        snap.fw_runtime.historical_fac_totals.insert(500_001, (4, 400));
        snap.fw_runtime.historical_ally_totals.insert(ally_id, (5, 500));
        snap.fw_runtime.stat_events.push(PersistedFwStatEvent {
            timestamp_unix: now.saturating_sub(100),
            character_id: exec_cid,
            corporation_id: exec_corp,
            alliance_id: Some(ally_id),
            faction_id: 500_001,
            kills: 1,
            victory_points: 50,
        });
    });

    // 1. on_space_entity_destroyed & resolve_friendly_supply_site_defense & regenerate_unattacked_fw_ihub
    rt.block_on(on_space_entity_destroyed(&ctx, sys_id, Some(ship_id), 500_000_000 + complex_gid, 587));
    assert!(ctx.db.with_snapshot(|s| s.fw_runtime.complex_guards_cleared.get(&complex_gid) == Some(&true)));
    rt.block_on(on_space_entity_destroyed(&ctx, sys_id, Some(ship_id), supply_id, 587));
    rt.block_on(on_space_entity_destroyed(&ctx, sys_id, None, supply_id, 587));

    assert_eq!(resolve_friendly_supply_site_defense(&ctx, exec_cid, 999_888).unwrap_err().code, "StructureNotFound");
    assert_eq!(resolve_friendly_supply_site_defense(&ctx, unenl_cid, supply_id).unwrap_err().code, "NotEnlistedInMilitia");
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&mem_cid).unwrap().war_faction_id = Some(500_004);
        snap.characters.get_mut(&exec_cid).unwrap().war_faction_id = Some(500_001);
    });
    assert_eq!(resolve_friendly_supply_site_defense(&ctx, mem_cid, supply_id).unwrap_err().code, "CannotDefendEnemyStructure");
    let def_res = resolve_friendly_supply_site_defense(&ctx, exec_cid, supply_id).unwrap();
    assert_eq!(def_res.dict_get("defended"), Some(&PyRep::Bool(true)));

    assert_eq!(regenerate_unattacked_fw_ihub(&ctx, sys_id), 80.0);

    // 2. GetStats_TopAndAllKillsAndVPs with stat_events & historical totals + legacy fallback branch when empty
    let stats_ev = rt.block_on(handle(&ctx, &mut SessionState::default(), "facWarMgr", "GetStats_TopAndAllKillsAndVPs", &[], &kw)).unwrap().unwrap();
    assert!(matches!(stats_ev, PyRep::Dict(_)));
    ctx.db.with_mut_snapshot(|snap| {
        snap.fw_runtime.stat_events.clear();
        snap.fw_runtime.historical_char_totals.clear();
        snap.fw_character_stats.insert(
            exec_cid,
            FwCharacterStatsRecord {
                character_id: exec_cid,
                faction_id: Some(500_001),
                enlisted_on_unix: Some(now),
                current_rank: 2,
                highest_rank: 3,
                kills: FwWindowedStat { yesterday: 1, last_week: 2, total: 5 },
                victory_points: FwWindowedStat { yesterday: 10, last_week: 20, total: 50 },
            },
        );
    });
    let stats_leg = rt.block_on(handle(&ctx, &mut SessionState::default(), "facWarMgr", "GetStats_TopAndAllKillsAndVPs", &[], &kw)).unwrap().unwrap();
    assert!(matches!(stats_leg, PyRep::Dict(_)));

    // 3. Join/LeaveFactionAsAlliance error paths + WithdrawJoin/LeaveFactionAsAlliance + RefreshCorporations + queries
    let mut sess_no_ally = SessionState::default();
    sess_no_ally.user_id = Some(1);
    sess_no_ally.character_id = Some(unenl_cid);
    sess_no_ally.corporation_id = Some(1_000_125);
    assert_eq!(rt.block_on(handle(&ctx, &mut sess_no_ally, "facWarMgr", "JoinFactionAsAlliance", &[PyRep::Integer(500_001)], &kw)).unwrap().unwrap_err().code, "NotInAlliance");
    assert_eq!(rt.block_on(handle(&ctx, &mut sess_no_ally, "facWarMgr", "LeaveFactionAsAlliance", &[], &kw)).unwrap().unwrap_err().code, "NotInAlliance");

    let mut sess_exec = SessionState::default();
    sess_exec.user_id = Some(1);
    sess_exec.character_id = Some(exec_cid);
    sess_exec.corporation_id = Some(exec_corp);
    sess_exec.alliance_id = Some(ally_id);
    sess_exec.solar_system_id = Some(sys_id);

    let mut sess_mem = SessionState::default();
    sess_mem.user_id = Some(2);
    sess_mem.character_id = Some(mem_cid);
    sess_mem.corporation_id = Some(mem_corp);
    sess_mem.alliance_id = Some(ally_id);

    // Invalid args / invalid faction / access denied / already enlisted / cooldown / not enlisted
    assert_eq!(rt.block_on(handle(&ctx, &mut sess_exec, "facWarMgr", "JoinFactionAsAlliance", &[], &kw)).unwrap().unwrap_err().code, "InvalidArgs");
    assert_eq!(rt.block_on(handle(&ctx, &mut sess_exec, "facWarMgr", "JoinFactionAsAlliance", &[PyRep::Integer(999_999)], &kw)).unwrap().unwrap_err().code, "InvalidFWMilitiaFaction");
    assert_eq!(rt.block_on(handle(&ctx, &mut sess_mem, "facWarMgr", "JoinFactionAsAlliance", &[PyRep::Integer(500_001)], &kw)).unwrap().unwrap_err().code, "AccessDenied");
    assert_eq!(rt.block_on(handle(&ctx, &mut sess_mem, "facWarMgr", "LeaveFactionAsAlliance", &[], &kw)).unwrap().unwrap_err().code, "AccessDenied");
    assert_eq!(rt.block_on(handle(&ctx, &mut sess_exec, "facWarMgr", "LeaveFactionAsAlliance", &[], &kw)).unwrap().unwrap_err().code, "AllianceNotEnlisted");

    // Pre-enlist mem_corp -> CorporationAlreadyEnlisted; pre-enlist exec_corp -> AllianceAlreadyEnlisted; negative standing -> InsufficientStanding
    ctx.db.with_mut_snapshot(|snap| {
        snap.fw_runtime.corp_war_factions.insert(mem_corp, 500_001);
    });
    assert_eq!(rt.block_on(handle(&ctx, &mut sess_exec, "facWarMgr", "JoinFactionAsAlliance", &[PyRep::Integer(500_001)], &kw)).unwrap().unwrap_err().code, "CorporationAlreadyEnlisted");
    ctx.db.with_mut_snapshot(|snap| {
        snap.fw_runtime.corp_war_factions.remove(&mem_corp);
        snap.fw_runtime.corp_war_factions.insert(exec_corp, 500_001);
    });
    assert_eq!(rt.block_on(handle(&ctx, &mut sess_exec, "facWarMgr", "JoinFactionAsAlliance", &[PyRep::Integer(500_001)], &kw)).unwrap().unwrap_err().code, "AllianceAlreadyEnlisted");
    // RefreshCorps propagates executor faction to member corp!
    assert!(rt.block_on(handle(&ctx, &mut sess_exec, "facWarMgr", "RefreshCorps", &[], &kw)).unwrap().is_ok());
    assert_eq!(ctx.db.with_snapshot(|s| s.fw_runtime.corp_war_factions.get(&mem_corp).copied()), Some(500_001));
    // LeaveFactionAsAlliance cooldown when fw_corp_enlisted_on is recent
    ctx.db.with_mut_snapshot(|snap| {
        snap.fw_corp_enlisted_on.insert(exec_corp, now);
    });
    assert_eq!(rt.block_on(handle(&ctx, &mut sess_exec, "facWarMgr", "LeaveFactionAsAlliance", &[], &kw)).unwrap().unwrap_err().code, "MilitiaCooldownActive");
    ctx.db.with_mut_snapshot(|snap| {
        snap.fw_runtime.corp_war_factions.remove(&exec_corp);
        snap.fw_runtime.corp_war_factions.remove(&mem_corp);
    });
    assert_eq!(rt.block_on(handle(&ctx, &mut sess_exec, "facWarMgr", "JoinFactionAsAlliance", &[PyRep::Integer(500_001)], &kw)).unwrap().unwrap_err().code, "MilitiaCooldownActive");

    // 4. Queries: GetAllianceWarFactionID, GetCharacterRankInfo, GetMyCharacterRankOverview, GetControlBunkerIDInSystem, GetCorpFactionalWarStatus, GetFactionCorporations, GetSolarSystemLPs, DonateLPsToSolarSystem, Withdraw*Alliance
    let _ = rt.block_on(handle(&ctx, &mut sess_exec, "facWarMgr", "GetAllianceWarFactionID", &[PyRep::Integer(ally_id)], &kw)).unwrap().unwrap();
    let rank_info = rt.block_on(handle(&ctx, &mut sess_exec, "facWarMgr", "GetCharacterRankInfo", &[PyRep::Integer(exec_cid)], &kw)).unwrap().unwrap();
    assert_eq!(rank_info.dict_get("currentRank"), Some(&PyRep::Integer(2)));
    let rank_ov = rt.block_on(handle(&ctx, &mut sess_exec, "facWarMgr", "GetMyCharacterRankOverview", &[PyRep::Integer(exec_cid)], &kw)).unwrap().unwrap();
    assert!(matches!(rank_ov, PyRep::Rowset { .. }));
    let bunker = rt.block_on(handle(&ctx, &mut sess_exec, "facWarMgr", "GetControlBunkerIDInSystem", &[], &kw)).unwrap().unwrap();
    assert!(bunker.as_i64().is_some());
    assert!(rt.block_on(handle(&ctx, &mut sess_no_ally, "facWarMgr", "GetCorpFactionalWarStatus", &[PyRep::Integer(1_000_125)], &kw)).unwrap().is_ok());
    assert!(rt.block_on(handle(&ctx, &mut sess_exec, "facWarMgr", "GetCorpFactionalWarStatus", &[PyRep::Integer(exec_corp)], &kw)).unwrap().is_ok());
    assert!(rt.block_on(handle(&ctx, &mut sess_exec, "facWarMgr", "GetFactionCorporations", &[PyRep::Integer(500_001)], &kw)).unwrap().is_ok());
    assert!(rt.block_on(handle(&ctx, &mut sess_exec, "facWarMgr", "GetSolarSystemLPs", &[PyRep::Integer(sys_id as i64)], &kw)).unwrap().is_ok());
    assert_eq!(rt.block_on(handle(&ctx, &mut sess_exec, "facWarMgr", "DonateLPsToSolarSystem", &[PyRep::Integer(sys_id as i64), PyRep::Integer(0)], &kw)).unwrap().unwrap_err().code, "InvalidLpDonation");
    assert!(rt.block_on(handle(&ctx, &mut sess_exec, "facWarMgr", "DonateLPsToSolarSystem", &[PyRep::Integer(sys_id as i64), PyRep::Integer(500)], &kw)).unwrap().is_ok());
    sess_exec.solar_system_id = None;
    assert_eq!(rt.block_on(handle(&ctx, &mut sess_exec, "facWarMgr", "DonateLPsToSolarSystem", &[], &kw)).unwrap().unwrap_err().code, "NoSolarSystem");

    // WithdrawJoinFactionAsAlliance / WithdrawLeaveFactionAsAlliance
    ctx.db.with_mut_snapshot(|snap| {
        let c = snap.corporations.entry(1_000_125).or_default();
        c.corporation_id = 1_000_125;
        c.ceo_id = unenl_cid;
    });
    assert_eq!(rt.block_on(handle(&ctx, &mut sess_no_ally, "facWarMgr", "WithdrawJoinFactionAsAlliance", &[], &kw)).unwrap().unwrap_err().code, "NotInAlliance");
    assert_eq!(rt.block_on(handle(&ctx, &mut sess_mem, "facWarMgr", "WithdrawJoinFactionAsAlliance", &[], &kw)).unwrap().unwrap_err().code, "AccessDenied");
    ctx.db.with_mut_snapshot(|snap| {
        let mut q = FwPendingQueue::default();
        q.alliance_joins.insert(ally_id, 500_001);
        q.alliance_leaves.insert(ally_id, 500_001);
        save_fw_pending(snap, &q);
    });
    assert_eq!(rt.block_on(handle(&ctx, &mut sess_exec, "facWarMgr", "WithdrawJoinFactionAsAlliance", &[PyRep::Integer(500_001)], &kw)).unwrap().unwrap(), PyRep::Bool(true));
    assert_eq!(rt.block_on(handle(&ctx, &mut sess_exec, "facWarMgr", "WithdrawLeaveFactionAsAlliance", &[], &kw)).unwrap().unwrap(), PyRep::Bool(true));
    assert_eq!(rt.block_on(handle(&ctx, &mut sess_exec, "facWarMgr", "WithdrawLeaveFactionAsAlliance", &[], &kw)).unwrap().unwrap(), PyRep::Bool(false));

    // Cover DonateLPsToSolarSystem NotInWarzone & NotInOrNearSystem, GetCorpFactionalWarStatus enlisted,
    // GetControlBunkerIDInSystem item fallback, AllianceNotFound, and standing_transactions / member standings fallback
    assert_eq!(rt.block_on(handle(&ctx, &mut sess_exec, "facWarMgr", "DonateLPsToSolarSystem", &[PyRep::Integer(30_000_142), PyRep::Integer(100)], &kw)).unwrap().unwrap_err().code, "NotInWarzone");
    sess_exec.solar_system_id = Some(sys_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&exec_cid) {
            ch.solar_system_id = 30_000_142;
            ch.war_faction_id = Some(500_001);
        }
        snap.fw_runtime.corp_war_factions.insert(exec_corp, 500_001);
        snap.fw_corp_enlisted_on.insert(exec_corp, ctx.now_unix());
        snap.structures.retain(|_, st| !(st.solar_system_id == sys_id && st.type_id == crate::services::fw_gameplay::TYPE_FW_IHUB));
        snap.items.insert(888_100_001, eve_db::InventoryItemRecord {
            item_id: 888_100_001,
            type_id: crate::services::fw_gameplay::TYPE_FW_IHUB,
            owner_id: exec_corp,
            location_id: i64::from(sys_id),
            quantity: 1,
            ..Default::default()
        });
    });
    assert_eq!(rt.block_on(handle(&ctx, &mut sess_exec, "facWarMgr", "DonateLPsToSolarSystem", &[PyRep::Integer(sys_id as i64), PyRep::Integer(100)], &kw)).unwrap().unwrap_err().code, "NotInOrNearSystem");
    assert_eq!(rt.block_on(handle(&ctx, &mut sess_exec, "facWarMgr", "GetControlBunkerIDInSystem", &[], &kw)).unwrap().unwrap(), PyRep::Integer(888_100_001));
    let enl_st = rt.block_on(handle(&ctx, &mut sess_exec, "facWarMgr", "GetCorpFactionalWarStatus", &[PyRep::Integer(exec_corp)], &kw)).unwrap().unwrap();
    assert_eq!(enl_st.dict_get("status"), Some(&PyRep::Integer(2)));

    // Standing via member standings, plus AllianceNotFound
    ctx.db.with_mut_snapshot(|snap| {
        snap.fw_runtime.corp_war_factions.remove(&exec_corp);
        snap.fw_runtime.corp_war_factions.remove(&mem_corp);
        snap.fw_corp_enlisted_on.remove(&exec_corp);
        if let Some(c) = snap.corporations.get_mut(&exec_corp) {
            c.members.insert(exec_cid);
        }
        snap.characters.get_mut(&exec_cid).unwrap().standings.insert(500_001, -3.0);
    });
    assert_eq!(rt.block_on(handle(&ctx, &mut sess_exec, "facWarMgr", "JoinFactionAsAlliance", &[PyRep::Integer(500_001)], &kw)).unwrap().unwrap_err().code, "InsufficientStanding");
    sess_exec.alliance_id = Some(99_999_888);
    assert_eq!(rt.block_on(handle(&ctx, &mut sess_exec, "facWarMgr", "JoinFactionAsAlliance", &[PyRep::Integer(500_001)], &kw)).unwrap().unwrap_err().code, "AllianceNotFound");
    assert_eq!(rt.block_on(handle(&ctx, &mut sess_exec, "facWarMgr", "LeaveFactionAsAlliance", &[], &kw)).unwrap().unwrap_err().code, "AllianceNotFound");
}

#[cfg(test)]
#[path = "fw_gameplay_rpc_extra_unit_tests.rs"]
mod fw_gameplay_rpc_extra_unit_tests;
