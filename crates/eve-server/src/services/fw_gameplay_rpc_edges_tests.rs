//! Behavioral edge-case tests for `fw_gameplay_rpc.rs`.

#![forbid(unsafe_code)]

use super::*;
use crate::router::ServerContext;
use eve_db::snapshot_ext::{FwCharacterStatsRecord, FwSystemOccupationRecord, FwWindowedStat};
use eve_db::{
    AllianceRecord, CharacterRecord, CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents,
    ServerClock, UpwellStructureRecord, VirtualServerClock,
};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn test_ctx() -> (ServerContext, Arc<VirtualServerClock>) {
    let clock = Arc::new(VirtualServerClock::with_fixed_base(1_750_000_000));
    let ctx = if let Some(sde) = SHARED_SDE.get() {
        ServerContext::with_sde_and_repository(
            Arc::clone(sde),
            Arc::new(eve_db::WorldRepository::with_demo_seed()),
        )
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
    } else {
        let c = ServerContext::new();
        let _ = SHARED_SDE.set(Arc::clone(&c.sde));
        c
    };
    ctx.set_clock(clock.clone());
    (ctx, clock)
}

async fn call_fw(
    ctx: &ServerContext,
    sess: &mut SessionState,
    method: &str,
    args: Vec<PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    handle(ctx, sess, "facWarMgr", method, &args, &HashMap::new())
        .await
        .unwrap_or_else(|| panic!("unhandled facWarMgr.{method}"))
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
    c.name = format!("FwEdge_{char_id}");
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

#[tokio::test(flavor = "multi_thread")]
async fn corp_and_alliance_enlist_leave_cooldowns_and_withdraw_guards() {
    let (ctx, clock) = test_ctx();
    let sys_id = *ctx.sde.solar_systems.keys().next().unwrap_or(&30_000_142);

    let solo_corp = 98_600_001_i64;
    let solo_ceo = 91_600_001_i64;
    let solo_line = 91_600_002_i64;
    let exec_corp = 98_600_002_i64;
    let exec_ceo = 91_600_003_i64;
    let memb_corp = 98_600_003_i64;
    let memb_ceo = 91_600_004_i64;
    let aid = 99_600_001_i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.characters
            .insert(solo_ceo, fw_char(snap, solo_ceo, solo_corp, None, None, sys_id));
        snap.characters
            .insert(solo_line, fw_char(snap, solo_line, solo_corp, None, None, sys_id));
        snap.characters
            .insert(exec_ceo, fw_char(snap, exec_ceo, exec_corp, Some(aid), None, sys_id));
        snap.characters
            .insert(memb_ceo, fw_char(snap, memb_ceo, memb_corp, Some(aid), None, sys_id));

        let mut sc = make_corp(solo_corp, solo_ceo, None);
        sc.members = HashSet::from([solo_ceo, solo_line]);
        sc.member_roles = HashMap::from([(solo_ceo, u64::MAX), (solo_line, 0u64)]);
        snap.corporations.insert(solo_corp, sc);

        let mut ec = make_corp(exec_corp, exec_ceo, Some(aid));
        ec.members = HashSet::from([exec_ceo]);
        ec.member_roles = HashMap::from([(exec_ceo, u64::MAX)]);
        snap.corporations.insert(exec_corp, ec);

        let mut mc = make_corp(memb_corp, memb_ceo, Some(aid));
        mc.members = HashSet::from([memb_ceo]);
        mc.member_roles = HashMap::from([(memb_ceo, u64::MAX)]);
        snap.corporations.insert(memb_corp, mc);

        snap.alliances.insert(
            aid,
            AllianceRecord {
                alliance_id: aid,
                name: "FW Edge Alliance".into(),
                short_name: "FWEA".into(),
                executor_corp_id: exec_corp,
                member_corp_ids: HashSet::from([exec_corp, memb_corp]),
            },
        );
    });

    let make_sess = |sid: u64, cid: i64, corp_id: i64, ally_id: Option<i64>, role: u64| {
        let mut s = SessionState::new(sid);
        s.user_id = Some(cid as u64);
        s.character_id = Some(cid);
        s.corporation_id = Some(corp_id);
        s.alliance_id = ally_id;
        s.corp_role = role;
        s.solar_system_id = Some(sys_id);
        s
    };

    let mut solo_ceo_s = make_sess(1, solo_ceo, solo_corp, None, u64::MAX);
    let mut solo_line_s = make_sess(2, solo_line, solo_corp, None, 0);
    let mut exec_ceo_s = make_sess(3, exec_ceo, exec_corp, Some(aid), u64::MAX);
    let mut memb_ceo_s = make_sess(4, memb_ceo, memb_corp, Some(aid), u64::MAX);

    // 1. LeaveFactionAsCorporation error guards: AccessDenied, CorporationNotEnlisted, MilitiaCooldownActive
    assert_eq!(
        call_fw(&ctx, &mut solo_line_s, "LeaveFactionAsCorporation", vec![])
            .await
            .unwrap_err()
            .code,
        "AccessDenied"
    );
    assert_eq!(
        call_fw(&ctx, &mut solo_ceo_s, "LeaveFactionAsCorporation", vec![])
            .await
            .unwrap_err()
            .code,
        "CorporationNotEnlisted"
    );

    // Enlist solo corp, then immediate leave fails with MilitiaCooldownActive
    call_fw(
        &ctx,
        &mut solo_ceo_s,
        "JoinFactionAsCorporation",
        vec![PyRep::Integer(500_001)],
    )
    .await
    .unwrap();
    assert_eq!(
        call_fw(&ctx, &mut solo_ceo_s, "LeaveFactionAsCorporation", vec![])
            .await
            .unwrap_err()
            .code,
        "MilitiaCooldownActive"
    );

    // Advance 24h -> LeaveFactionAsCorporation succeeds, then immediate re-join fails due to leave cooldown
    clock.advance_secs(86_401);
    assert_eq!(
        call_fw(&ctx, &mut solo_ceo_s, "LeaveFactionAsCorporation", vec![])
            .await
            .unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(
        call_fw(
            &ctx,
            &mut solo_ceo_s,
            "JoinFactionAsCorporation",
            vec![PyRep::Integer(500_001)],
        )
        .await
        .unwrap_err()
        .code,
        "MilitiaCooldownActive"
    );
    clock.advance_secs(86_401);
    assert_eq!(
        call_fw(
            &ctx,
            &mut solo_ceo_s,
            "JoinFactionAsCorporation",
            vec![PyRep::Integer(500_001)],
        )
        .await
        .unwrap(),
        PyRep::Bool(true)
    );

    // 2. LeaveFactionAsAlliance & member corp LeaveFactionAsCorporation blocked by AllianceEnlisted
    assert_eq!(
        call_fw(&ctx, &mut solo_ceo_s, "LeaveFactionAsAlliance", vec![])
            .await
            .unwrap_err()
            .code,
        "NotInAlliance"
    );
    assert_eq!(
        call_fw(&ctx, &mut memb_ceo_s, "LeaveFactionAsAlliance", vec![])
            .await
            .unwrap_err()
            .code,
        "AccessDenied"
    );
    assert_eq!(
        call_fw(&ctx, &mut exec_ceo_s, "LeaveFactionAsAlliance", vec![])
            .await
            .unwrap_err()
            .code,
        "AllianceNotEnlisted"
    );

    call_fw(
        &ctx,
        &mut exec_ceo_s,
        "JoinFactionAsAlliance",
        vec![PyRep::Integer(500_004)],
    )
    .await
    .unwrap();
    assert_eq!(
        call_fw(&ctx, &mut memb_ceo_s, "LeaveFactionAsCorporation", vec![])
            .await
            .unwrap_err()
            .code,
        "AllianceEnlisted"
    );
    assert_eq!(
        call_fw(&ctx, &mut exec_ceo_s, "LeaveFactionAsAlliance", vec![])
            .await
            .unwrap_err()
            .code,
        "MilitiaCooldownActive"
    );

    // 3. Withdraw* guards and mismatched vs matching faction IDs
    assert_eq!(
        call_fw(&ctx, &mut solo_line_s, "WithdrawJoinFactionAsCorporation", vec![])
            .await
            .unwrap_err()
            .code,
        "AccessDenied"
    );
    assert_eq!(
        call_fw(&ctx, &mut solo_ceo_s, "WithdrawJoinFactionAsAlliance", vec![])
            .await
            .unwrap_err()
            .code,
        "NotInAlliance"
    );
    assert_eq!(
        call_fw(&ctx, &mut memb_ceo_s, "WithdrawLeaveFactionAsAlliance", vec![])
            .await
            .unwrap_err()
            .code,
        "AccessDenied"
    );

    // Seed pending queue entries and test mismatch -> false, match -> true
    ctx.db.with_mut_snapshot(|snap| {
        let mut q = FwPendingQueue::default();
        q.corp_joins.insert(solo_corp, 500_001);
        q.corp_leaves.insert(solo_corp, 500_001);
        q.alliance_joins.insert(aid, 500_004);
        q.alliance_leaves.insert(aid, 500_004);
        save_fw_pending(snap, &q);
    });

    assert_eq!(
        call_fw(
            &ctx,
            &mut solo_ceo_s,
            "WithdrawJoinFactionAsCorporation",
            vec![PyRep::Integer(500_002)],
        )
        .await
        .unwrap(),
        PyRep::Bool(false)
    );
    assert_eq!(
        call_fw(
            &ctx,
            &mut solo_ceo_s,
            "WithdrawJoinFactionAsCorporation",
            vec![PyRep::Integer(500_001)],
        )
        .await
        .unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(
        call_fw(&ctx, &mut solo_ceo_s, "WithdrawLeaveFactionAsCorporation", vec![])
            .await
            .unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(
        call_fw(
            &ctx,
            &mut exec_ceo_s,
            "WithdrawJoinFactionAsAlliance",
            vec![PyRep::Integer(500_001)],
        )
        .await
        .unwrap(),
        PyRep::Bool(false)
    );
    assert_eq!(
        call_fw(
            &ctx,
            &mut exec_ceo_s,
            "WithdrawJoinFactionAsAlliance",
            vec![PyRep::Integer(500_004)],
        )
        .await
        .unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(
        call_fw(&ctx, &mut exec_ceo_s, "WithdrawLeaveFactionAsAlliance", vec![])
            .await
            .unwrap(),
        PyRep::Bool(true)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn bunker_lookup_donate_errors_refresh_corps_status_and_faction_info() {
    let (ctx, _clock) = test_ctx();
    let sys_id = *ctx.sde.solar_systems.keys().next().unwrap_or(&30_000_142);
    let char_id = 91_600_101_i64;
    let exec_corp = 98_600_101_i64;
    let member_corp = 98_600_102_i64;
    let aid = 99_600_101_i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.characters
            .insert(char_id, fw_char(snap, char_id, exec_corp, Some(aid), Some(500_002), sys_id));
        snap.corporations
            .insert(exec_corp, make_corp(exec_corp, char_id, Some(aid)));
        snap.corporations
            .insert(member_corp, make_corp(member_corp, char_id, Some(aid)));
        snap.alliances.insert(
            aid,
            AllianceRecord {
                alliance_id: aid,
                name: "Refresh Ally".into(),
                short_name: "RFRA".into(),
                executor_corp_id: exec_corp,
                member_corp_ids: HashSet::from([exec_corp, member_corp]),
            },
        );
        snap.fw_runtime.corp_war_factions.insert(exec_corp, 500_002);
        snap.fw_character_stats.insert(
            char_id,
            FwCharacterStatsRecord {
                character_id: char_id,
                faction_id: Some(500_002),
                enlisted_on_unix: Some(1_750_000_000),
                current_rank: 4,
                highest_rank: 6,
                kills: FwWindowedStat::default(),
                victory_points: FwWindowedStat::default(),
            },
        );
        snap.fw_system_occupations.clear();
        snap.fw_system_occupations.insert(
            sys_id,
            FwSystemOccupationRecord {
                solar_system_id: sys_id,
                owner_faction_id: 500_002,
                occupier_faction_id: 500_002,
                victory_points: 0,
                victory_points_threshold: 100,
            },
        );
    });

    let mut sess = SessionState::new(10);
    sess.user_id = Some(char_id as u64);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(exec_corp);
    sess.alliance_id = Some(aid);
    sess.solar_system_id = Some(sys_id);

    // 1. DonateLPsToSolarSystem validation: NoSolarSystem, InvalidLpDonation
    let mut no_sys = sess.clone();
    no_sys.solar_system_id = None;
    assert_eq!(
        call_fw(
            &ctx,
            &mut no_sys,
            "DonateLPsToSolarSystem",
            vec![PyRep::None, PyRep::Integer(100)],
        )
        .await
        .unwrap_err()
        .code,
        "NoSolarSystem"
    );
    assert_eq!(
        call_fw(
            &ctx,
            &mut sess,
            "DonateLPsToSolarSystem",
            vec![PyRep::Integer(i64::from(sys_id)), PyRep::Integer(0)],
        )
        .await
        .unwrap_err()
        .code,
        "InvalidLpDonation"
    );

    // 2. RefreshCorps propagates executor faction to member_corp
    assert_eq!(
        call_fw(&ctx, &mut sess, "RefreshCorps", vec![]).await.unwrap(),
        PyRep::None
    );
    assert_eq!(
        ctx.db
            .with_snapshot(|s| s.fw_runtime.corp_war_factions.get(&member_corp).copied()),
        Some(500_002)
    );

    // 3. GetCorpFactionalWarStatus for enlisted vs non-enlisted
    let st_enlisted = call_fw(
        &ctx,
        &mut sess,
        "GetCorpFactionalWarStatus",
        vec![PyRep::Integer(exec_corp)],
    )
    .await
    .unwrap();
    assert_eq!(
        st_enlisted.dict_get("factionID").and_then(|v| v.as_i64()),
        Some(500_002)
    );
    assert_eq!(st_enlisted.dict_get("status").and_then(|v| v.as_i64()), Some(2));

    let st_none = call_fw(
        &ctx,
        &mut sess,
        "GetCorpFactionalWarStatus",
        vec![PyRep::Integer(98_999_999)],
    )
    .await
    .unwrap();
    assert_eq!(st_none.dict_get("factionID"), Some(&PyRep::None));
    assert_eq!(st_none.dict_get("status").and_then(|v| v.as_i64()), Some(0));

    // 4. GetControlBunkerIDInSystem priority chain: fw_system_occupations -> space item -> Upwell structure
    assert_eq!(
        call_fw(&ctx, &mut sess, "GetControlBunkerIDInSystem", vec![])
            .await
            .unwrap(),
        PyRep::Integer(1_000_000_000 + i64::from(sys_id))
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            77_700_001,
            InventoryItemRecord {
                item_id: 77_700_001,
                type_id: crate::services::fw_gameplay::TYPE_FW_IHUB,
                owner_id: 500_002,
                location_id: i64::from(sys_id),
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    assert_eq!(
        call_fw(&ctx, &mut sess, "GetControlBunkerIDInSystem", vec![])
            .await
            .unwrap(),
        PyRep::Integer(77_700_001)
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            77_800_001,
            UpwellStructureRecord {
                structure_id: 77_800_001,
                type_id: crate::services::fw_gameplay::TYPE_FW_IHUB,
                owner_corp_id: exec_corp,
                solar_system_id: sys_id,
                name: "FW Hub".into(),
                position: [0.0; 3],
                state: "ShieldVulnerable".into(),
                ..Default::default()
            },
        );
    });
    assert_eq!(
        call_fw(&ctx, &mut sess, "GetControlBunkerIDInSystem", vec![])
            .await
            .unwrap(),
        PyRep::Integer(77_800_001)
    );

    // 5. CheckForRankChange, GetCharacterRankInfo, GetMyCharacterRankOverview, GetStats_FactionInfo
    assert_eq!(
        call_fw(&ctx, &mut sess, "CheckForRankChange", vec![]).await.unwrap(),
        PyRep::Integer(4)
    );
    let rinfo = call_fw(&ctx, &mut sess, "GetCharacterRankInfo", vec![])
        .await
        .unwrap();
    assert_eq!(rinfo.dict_get("currentRank").and_then(|v| v.as_i64()), Some(4));
    assert_eq!(rinfo.dict_get("highestRank").and_then(|v| v.as_i64()), Some(6));

    let finfo = call_fw(&ctx, &mut sess, "GetStats_FactionInfo", vec![])
        .await
        .unwrap();
    let PyRep::Dict(entries) = finfo else { panic!("expected dict") };
    let min_entry = entries
        .iter()
        .find(|(k, _)| k.as_i64() == Some(500_002))
        .map(|(_, v)| v)
        .unwrap();
    assert_eq!(
        min_entry.dict_get("warzoneControlled").and_then(|v| v.as_f64()),
        Some(1.0)
    );
    let cal_entry = entries
        .iter()
        .find(|(k, _)| k.as_i64() == Some(500_001))
        .map(|(_, v)| v)
        .unwrap();
    assert_eq!(
        cal_entry.dict_get("warzoneControlled").and_then(|v| v.as_f64()),
        Some(0.0)
    );
}
