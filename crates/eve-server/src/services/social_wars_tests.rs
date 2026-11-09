use super::*;
use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::make_fighter_ctx_and_session;
use eve_db::{CorporationRecord, IskCents, KillmailRecord, UpwellStructureRecord};

fn dict_i64(r: &PyRep, k: &str) -> Option<i64> {
    r.dict_get(k).and_then(|v| v.as_i64())
}
fn dict_str<'a>(r: &'a PyRep, k: &str) -> Option<&'a str> {
    r.dict_get(k).and_then(|v| v.as_str())
}
fn dict_bool(r: &PyRep, k: &str) -> Option<bool> {
    r.dict_get(k).and_then(|v| v.as_bool())
}

fn setup_wars_ctx() -> (
    ServerContext,
    SessionState,
    i64,
    SessionState,
    i64,
    SessionState,
    i64,
    i64,
) {
    let (ctx, mut agg_sess, agg_char, _ship_id, sys_id) = make_fighter_ctx_and_session();
    let agg_corp = 98_000_101_i64;
    let def_corp = 98_000_102_i64;
    let ally_corp = 98_000_103_i64;
    let def_char = 90_000_102_i64;
    let ally_char = 90_000_103_i64;
    let agg_hq_id = 1_000_000_100_001_i64;
    let def_st_id = 1_000_000_100_002_i64;

    agg_sess.corporation_id = Some(agg_corp);
    agg_sess.corp_role = u64::MAX;
    let mut def_sess = agg_sess.clone();
    def_sess.character_id = Some(def_char);
    def_sess.corporation_id = Some(def_corp);
    let mut ally_sess = agg_sess.clone();
    ally_sess.character_id = Some(ally_char);
    ally_sess.corporation_id = Some(ally_corp);

    ctx.db.with_mut_snapshot(|snap| {
        let base_char = snap.characters.get(&agg_char).cloned().unwrap();
        if let Some(ch) = snap.characters.get_mut(&agg_char) {
            ch.corporation_id = agg_corp;
        }
        for (cid, uid, corp, name) in [
            (def_char, 102, def_corp, "DefCEO"),
            (ally_char, 103, ally_corp, "AllyCEO"),
        ] {
            let mut ch = base_char.clone();
            ch.character_id = cid;
            ch.user_id = uid;
            ch.name = name.into();
            ch.corporation_id = corp;
            ch.solar_system_id = sys_id;
            ch.active_ship_id = 100_000_100 + uid as i64;
            ch.balance = IskCents::from_isk(100_000_000);
            snap.characters.insert(cid, ch);
        }
        for (corp_id, ceo_id, ticker) in [
            (agg_corp, agg_char, "AGGR"),
            (def_corp, def_char, "DEFD"),
            (ally_corp, ally_char, "ALLY"),
        ] {
            let mut c = { let mut __corp = CorporationRecord::default();
                __corp.corporation_id = corp_id;
                __corp.name = format!("Corp {ticker}");
                __corp.ticker = ticker.into();
                __corp.ceo_id = ceo_id;
                __corp.alliance_id = None;
                __corp.tax_rate = 0.05;
                __corp.members = vec![ceo_id].into_iter().collect();
                __corp.member_roles = [(ceo_id, u64::MAX)].into_iter().collect();
                __corp.hq_station_id = Some(60_003_760);
                __corp.set_division_balance(1, IskCents::from_isk(500_000_000));
                __corp };
            c.ensure_wallet_divisions();
            snap.corporations.insert(corp_id, c);
        }
        for (sid, owner) in [(agg_hq_id, agg_corp), (def_st_id, def_corp)] {
            snap.structures.insert(
                sid,
                UpwellStructureRecord {
                    structure_id: sid,
                    type_id: 35832,
                    owner_corp_id: owner,
                    solar_system_id: sys_id,
                    name: format!("HQ {sid}"),
                    state: "ShieldVulnerable".into(),
                    position: [0.0; 3],
                    quantum_core_installed: true,
                    ..Default::default()
                },
            );
        }
    });

    (
        ctx, agg_sess, agg_corp, def_sess, def_corp, ally_sess, ally_corp, agg_hq_id,
    )
}

#[test]
fn test_declare_war_allies_surrender_peace_treaty_and_statistics() {
    let (ctx, mut agg_sess, agg_corp, mut def_sess, def_corp, mut ally_sess, ally_corp, agg_hq_id) =
        setup_wars_ctx();

    // 1. DeclareWarAgainst
    let declared = handle_wars(
        &ctx,
        &mut agg_sess,
        "warRegistry",
        "DeclareWarAgainst",
        &[PyRep::Integer(def_corp), PyRep::Integer(agg_hq_id)],
        None,
    )
    .unwrap()
    .unwrap();
    let war_id = dict_i64(&declared, "warID").unwrap();
    assert_eq!(dict_i64(&declared, "warHQ"), Some(agg_hq_id));

    // GetWars, GetWarsByOwnerID, GetTop50, GetWarsForStructure, GetPublicWarInfo
    let wars = handle_wars(&ctx, &mut agg_sess, "warRegistry", "GetWars", &[], None)
        .unwrap()
        .unwrap();
    assert!(matches!(wars, PyRep::Dict(ref d) if !d.is_empty()));

    let by_owner = handle_wars(
        &ctx,
        &mut agg_sess,
        "warsInfoMgr",
        "GetWarsByOwnerID",
        &[PyRep::Integer(agg_corp)],
        None,
    )
    .unwrap()
    .unwrap();
    assert!(by_owner.is_crowset(), "GetWarsByOwnerID must return dbutil.CRowset");
    assert_eq!(by_owner.row_count(), Some(1));

    // A non-participant corporation calling warRegistry.GetNegotiations() must get an empty result,
    // not NotWarParticipant (#760).
    let mut non_party_sess = SessionState::new(719);
    non_party_sess.user_id = Some(999);
    non_party_sess.character_id = Some(90_000_003);
    non_party_sess.corporation_id = Some(98_099_999);
    let empty_negs = handle_wars(
        &ctx,
        &mut non_party_sess,
        "warRegistry",
        "GetNegotiations",
        &[],
        Some(98_099_999),
    )
    .unwrap()
    .unwrap();
    assert_eq!(empty_negs.row_count(), Some(0));

    let top50 = handle_wars(&ctx, &mut agg_sess, "warsInfoMgr", "GetTop50", &[], None)
        .unwrap()
        .unwrap();
    assert!(!matches!(top50, PyRep::None));

    let for_st = handle_wars(
        &ctx,
        &mut agg_sess,
        "warsInfoMgr",
        "GetWarsForStructure",
        &[PyRep::Integer(agg_hq_id)],
        None,
    )
    .unwrap()
    .unwrap();
    assert!(!matches!(for_st, PyRep::None));

    let pub_info = handle_wars(
        &ctx,
        &mut agg_sess,
        "warsInfoMgr",
        "GetPublicWarInfo",
        &[PyRep::Integer(war_id)],
        None,
    )
    .unwrap()
    .unwrap();
    assert_eq!(dict_i64(&pub_info, "warID"), Some(war_id));

    // 2. Open for allies, ally offer, accept ally
    let opened = handle_wars(
        &ctx,
        &mut def_sess,
        "warsInfoMgr",
        "SetOpenForAllies",
        &[PyRep::Integer(war_id), PyRep::Bool(true)],
        None,
    )
    .unwrap()
    .unwrap();
    assert_eq!(dict_bool(&opened, "openForAllies"), Some(true));

    let ally_offer = handle_wars(
        &ctx,
        &mut ally_sess,
        "warsInfoMgr",
        "CreateWarAllyOffer",
        &[
            PyRep::Integer(war_id),
            PyRep::Real(5_000_000.0),
            PyRep::String("Reinforcements".into()),
        ],
        None,
    )
    .unwrap()
    .unwrap();
    let ally_neg_id = dict_i64(&ally_offer, "negotiationID").unwrap();

    let ally_acc = handle_wars(
        &ctx,
        &mut def_sess,
        "warsInfoMgr",
        "AcceptAllyNegotiation",
        &[PyRep::Integer(ally_neg_id)],
        None,
    )
    .unwrap()
    .unwrap();
    assert_eq!(dict_i64(&ally_acc, "allyID"), Some(ally_corp));

    // 3. Seed a war killmail and test warStatisticMgr
    let now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        snap.killmails.insert(
            601,
            KillmailRecord {
                kill_id: 601,
                solar_system_id: 30_000_142,
                killed_unix: now,
                victim_character_id: 90_000_102,
                victim_corp_id: def_corp,
                victim_alliance_id: None,
                ship_type_id: 600,
                attacker_character_id: 90_000_001,
                attacker_corp_id: agg_corp,
                attacker_alliance_id: None,
                total_value_isk: 25_000_000.0,
            },
        );
    });
    let base_info = handle_wars(
        &ctx,
        &mut agg_sess,
        "warStatisticMgr",
        "GetBaseInfo",
        &[PyRep::Integer(war_id)],
        Some(war_id),
    )
    .unwrap()
    .unwrap();
    assert_eq!(dict_i64(&base_info, "killsForAggressor"), Some(1));

    let kills = handle_wars(
        &ctx,
        &mut agg_sess,
        "warStatisticMgr",
        "GetKills",
        &[PyRep::Integer(war_id)],
        Some(war_id),
    )
    .unwrap()
    .unwrap();
    assert!(!matches!(kills, PyRep::None));

    let km = handle_wars(
        &ctx,
        &mut agg_sess,
        "warStatisticMgr",
        "GetKillMail",
        &[PyRep::Integer(601)],
        Some(war_id),
    )
    .unwrap()
    .unwrap();
    assert_eq!(dict_i64(&km, "killID"), Some(601));

    // 4. Surrender negotiation & accept -> creates peace treaty
    let surr = handle_wars(
        &ctx,
        &mut def_sess,
        "warsInfoMgr",
        "CreateSurrenderNegotiation",
        &[
            PyRep::Integer(war_id),
            PyRep::Real(10_000_000.0),
            PyRep::String("GG".into()),
        ],
        None,
    )
    .unwrap()
    .unwrap();
    let surr_id = dict_i64(&surr, "negotiationID").unwrap();
    let acc_surr = handle_wars(
        &ctx,
        &mut agg_sess,
        "warsInfoMgr",
        "AcceptSurrender",
        &[PyRep::Integer(surr_id)],
        None,
    )
    .unwrap()
    .unwrap();
    assert_eq!(dict_str(&acc_surr, "status"), Some("Accepted"));

    let treaties = handle_wars(
        &ctx,
        &mut agg_sess,
        "peaceTreatyMgr",
        "GetPeaceTreatiesForSession",
        &[],
        None,
    )
    .unwrap()
    .unwrap();
    assert!(!matches!(treaties, PyRep::None));
}

#[test]
fn test_mutual_war_invites_and_retraction() {
    let (ctx, mut agg_sess, agg_corp, _def_sess, _def_corp, mut ally_sess, ally_corp, _) =
        setup_wars_ctx();

    let sent = handle_wars(
        &ctx,
        &mut agg_sess,
        "mutualWarInviteMgr",
        "SendInviteByPlayer",
        &[PyRep::Integer(ally_corp)],
        None,
    )
    .unwrap()
    .unwrap();
    assert_eq!(dict_i64(&sent, "toID"), Some(ally_corp));

    let resp = handle_wars(
        &ctx,
        &mut ally_sess,
        "mutualWarInviteMgr",
        "RespondToInviteByPlayer",
        &[PyRep::Integer(agg_corp), PyRep::Bool(true)],
        None,
    )
    .unwrap()
    .unwrap();
    let wid = dict_i64(&resp, "warID").unwrap();
    assert_eq!(dict_bool(&resp, "mutual"), Some(true));

    let retracted = handle_wars(
        &ctx,
        &mut agg_sess,
        "warsInfoMgr",
        "RetractMutualWar",
        &[PyRep::Integer(wid)],
        None,
    )
    .unwrap()
    .unwrap();
    assert_eq!(dict_i64(&retracted, "warID"), Some(wid));
}

#[test]
fn war_statistic_get_killmail_denies_uninvolved_without_hash() {
    let (ctx, agg_sess, agg_char, _def_sess, def_char, ally_sess, _ally_char, _war_id) =
        setup_wars_ctx();
    let kill_id = 602_i64;
    let killed_unix = 1_750_000_000_u64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.killmails.insert(
            kill_id,
            KillmailRecord {
                kill_id,
                solar_system_id: 30_000_142,
                victim_character_id: def_char,
                victim_corp_id: 98_000_102,
                victim_alliance_id: None,
                attacker_character_id: agg_char,
                attacker_corp_id: 98_000_101,
                attacker_alliance_id: None,
                ship_type_id: 587,
                killed_unix,
                total_value_isk: 500_000.0,
            },
        );
    });

    // Uninvolved outsider corp (ally_sess before any war match for this non-war kill)
    let mut bystander = ally_sess;
    bystander.corporation_id = Some(98_000_999);
    bystander.character_id = Some(90_000_999);
    assert_eq!(
        handle_wars(
            &ctx,
            &mut bystander,
            "warStatisticMgr",
            "GetKillMail",
            &[PyRep::Integer(kill_id)],
            None,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "KillmailAccessDenied"
    );

    let mut att = agg_sess;
    assert!(
        handle_wars(
            &ctx,
            &mut att,
            "warStatisticMgr",
            "GetKillMail",
            &[PyRep::Integer(kill_id)],
            None,
        )
        .unwrap()
        .is_ok()
    );
}
