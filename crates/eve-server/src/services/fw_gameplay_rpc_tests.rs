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
