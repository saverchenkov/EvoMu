use super::*;
use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::make_fighter_ctx_and_session;
use eve_db::{
    CorporationRecord, IskCents, KillmailRecord, PersistedWarNegotiation as Negotiation,
    UpwellStructureRecord, WarRecord,
};

#[cfg(test)]
#[path = "social_wars_extra_unit_tests.rs"]
mod extra_unit_tests;

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

    let PyRep::List(pub_items) = handle_wars(
        &ctx,
        &mut agg_sess,
        "warsInfoMgr",
        "GetPublicWarInfo",
        &[PyRep::Integer(war_id)],
        None,
    )
    .unwrap()
    .unwrap()
    else {
        panic!("expected List from GetPublicWarInfo");
    };
    assert_eq!(pub_items.len(), 1);
    assert_eq!(dict_i64(&pub_items[0], "warID"), Some(war_id));

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
    let PyRep::Tuple(base_parts) = handle_wars(
        &ctx,
        &mut agg_sess,
        "warStatisticMgr",
        "GetBaseInfo",
        &[PyRep::Integer(war_id)],
        Some(war_id),
    )
    .unwrap()
    .unwrap()
    else {
        panic!("expected 4-tuple from GetBaseInfo");
    };
    assert_eq!(base_parts.len(), 4);
    assert_eq!(dict_i64(&base_parts[0], "killsForAggressor"), Some(1));

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
    let PyRep::Tuple(ref parts) = treaties else {
        panic!("expected (outgoing, incoming) 2-tuple from GetPeaceTreatiesForSession");
    };
    assert_eq!(parts.len(), 2);
    let PyRep::List(ref outgoing) = parts[0] else {
        panic!("expected outgoing List");
    };
    let PyRep::List(ref incoming) = parts[1] else {
        panic!("expected incoming List");
    };
    assert_eq!(outgoing.len(), 1);
    assert!(incoming.is_empty());
    assert_eq!(dict_i64(&outgoing[0], "otherOwnerID"), Some(def_corp));
    assert_eq!(dict_i64(&outgoing[0], "peaceReason"), Some(4));
    assert!(dict_i64(&outgoing[0], "expiryDate").unwrap_or(0) > 100_000_000_000_000_000);
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

#[test]
fn war_ally_24h_effective_delay_and_sync_wars_enforced() {
    let (ctx, mut agg_sess, _agg_char, _def_sess, def_corp, _ally_sess, ally_corp, agg_hq_id) =
        setup_wars_ctx();
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
    let now = 1_750_000_000_u64;
    let eff = now + 86_400;

    ctx.db.with_mut_snapshot(|snap| {
        snap.war_state.negotiations.push(eve_db::PersistedWarNegotiation {
            id: 9_901,
            war_id,
            kind: eve_db::PersistedNegKind::Ally,
            from_corp: ally_corp,
            isk: IskCents(0),
            message: "Ally help".into(),
            status: "Accepted".into(),
            effective_unix: Some(eff),
        });
    });

    ctx.db.with_snapshot(|snap| {
        assert!(
            !social_wars_helpers::is_war_participant_corp(snap, war_id, ally_corp, eff - 1),
            "ally must NOT be a war participant before effective_unix (24h spool-up)"
        );
        assert!(
            social_wars_helpers::is_war_participant_corp(snap, war_id, ally_corp, eff),
            "ally becomes active war participant once now >= effective_unix"
        );
    });
}

#[test]
fn test_social_wars_helpers_predicates_sync_notifications_and_resolution() {
    use super::social_wars_helpers::*;
    use eve_db::{PersistedNegKind, PersistedWarMeta, PersistedWarNegotiation, WarRecord};

    let (ctx, agg_sess, agg_corp, _def_sess, def_corp, _ally_sess, ally_corp, agg_hq_id) =
        setup_wars_ctx();
    let other_corp = 98_000_199_i64;
    let lowsec_sys = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| s.solar_system_id < 31_000_000 && s.security_status < 0.45)
        .map(|s| s.solar_system_id)
        .unwrap_or(30_002_813);

    // 1. err() helper
    let e = err("TestErr", "bad input").unwrap().unwrap_err();
    assert_eq!(e.code, "TestErr");
    assert_eq!(e.message, "bad input");

    // 2. is_valid_war_hq exhaustive matrix
    ctx.db.with_snapshot(|snap| {
        let base = snap.structures[&agg_hq_id].clone();
        assert!(is_valid_war_hq(&ctx, &snap.corporations, agg_corp, &base));

        // Wrong owner
        assert!(!is_valid_war_hq(&ctx, &snap.corporations, def_corp, &base));

        // No quantum core
        let mut no_core = base.clone();
        no_core.quantum_core_installed = false;
        assert!(!is_valid_war_hq(&ctx, &snap.corporations, agg_corp, &no_core));

        // Allowed vs disallowed states
        for valid_state in [
            "ShieldVulnerable",
            "ArmorReinforced",
            "ArmorVulnerable",
            "HullReinforced",
            "HullVulnerable",
        ] {
            let mut st = base.clone();
            st.state = valid_state.into();
            assert!(is_valid_war_hq(&ctx, &snap.corporations, agg_corp, &st), "state {valid_state} should be valid");
        }
        for bad_state in ["Anchoring", "Unanchored", "Destroyed"] {
            let mut st = base.clone();
            st.state = bad_state.into();
            assert!(!is_valid_war_hq(&ctx, &snap.corporations, agg_corp, &st), "state {bad_state} should be invalid");
        }

        // Wormhole solar system >= 31_000_000
        let mut wh = base.clone();
        wh.solar_system_id = 31_000_005;
        assert!(!is_valid_war_hq(&ctx, &snap.corporations, agg_corp, &wh));

        // Low/null-sec solar system (< 0.45)
        let mut low = base.clone();
        low.solar_system_id = lowsec_sys;
        assert!(!is_valid_war_hq(&ctx, &snap.corporations, agg_corp, &low));

        // FLEX structure (35841 Ansiblex Jump Gate)
        let mut flex = base.clone();
        flex.type_id = 35841;
        assert!(!is_valid_war_hq(&ctx, &snap.corporations, agg_corp, &flex));
    });

    // 3. same_alliance, already_at_war, war_eligible, debit_corp, credit_corp
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.insert(
            other_corp,
            { let mut __corp = CorporationRecord::default(); __corp.corporation_id = other_corp; __corp.name = "Other".into(); __corp.ticker = "OTHR".into(); __corp.ceo_id = 90_000_199; __corp.alliance_id = Some(99_000_555); __corp.ensure_wallet_divisions(); __corp },
        );
        // None / Some(0) / mismatched / matched alliances
        assert!(!same_alliance(snap, agg_corp, def_corp));
        snap.corporations.get_mut(&agg_corp).unwrap().alliance_id = Some(0);
        snap.corporations.get_mut(&def_corp).unwrap().alliance_id = Some(0);
        assert!(!same_alliance(snap, agg_corp, def_corp));
        snap.corporations.get_mut(&agg_corp).unwrap().alliance_id = Some(99_000_100);
        snap.corporations.get_mut(&def_corp).unwrap().alliance_id = Some(99_000_200);
        assert!(!same_alliance(snap, agg_corp, def_corp));
        snap.corporations.get_mut(&ally_corp).unwrap().alliance_id = Some(99_000_100);
        assert!(same_alliance(snap, agg_corp, ally_corp));

        // war_eligible: Destroyed structure doesn't count
        assert!(!war_eligible(snap, ally_corp));
        snap.structures.insert(
            1_000_000_100_099,
            UpwellStructureRecord {
                structure_id: 1_000_000_100_099,
                type_id: 35832,
                owner_corp_id: ally_corp,
                solar_system_id: 30_000_142,
                state: "Destroyed".into(),
                ..Default::default()
            },
        );
        assert!(!war_eligible(snap, ally_corp));

        // debit_corp errors & credit_corp
        assert_eq!(
            debit_corp(snap, 99_999_999, IskCents::from_isk(100))
                .unwrap_err()
                .code,
            "CorporationNotFound"
        );
        assert_eq!(
            debit_corp(snap, other_corp, IskCents::from_isk(100))
                .unwrap_err()
                .code,
            "InsufficientFunds"
        );
        let before = snap.corporations[&agg_corp].division_balance(1);
        credit_corp(snap, agg_corp, IskCents::from_isk(5_000));
        assert_eq!(
            snap.corporations[&agg_corp].division_balance(1),
            IskCents(before.0 + 5_000 * 100)
        );
        // credit_corp on missing corp is a safe no-op
        credit_corp(snap, 99_999_999, IskCents::from_isk(500));

        // Wars for already_at_war and is_war_participant_corp
        snap.wars.insert(
            5001,
            WarRecord {
                war_id: 5001,
                aggressor_corp_id: agg_corp,
                defender_corp_id: def_corp,
                mutual: false,
                started_unix: 1_700_000_000,
                active: false,
            },
        );
        assert!(!already_at_war(snap, agg_corp, def_corp));
        snap.wars.get_mut(&5001).unwrap().active = true;
        assert!(already_at_war(snap, agg_corp, def_corp));
        assert!(already_at_war(snap, def_corp, agg_corp));

        // is_war_participant_corp
        assert!(!is_war_participant_corp(snap, 99_999, agg_corp, 2_000_000_000));
        assert!(is_war_participant_corp(snap, 5001, agg_corp, 2_000_000_000));
        assert!(is_war_participant_corp(snap, 5001, def_corp, 2_000_000_000));
        assert!(is_war_participant_corp(snap, 5001, ally_corp, 2_000_000_000)); // same alliance as agg_corp
        assert!(!is_war_participant_corp(snap, 5001, other_corp, 2_000_000_000));

        // Pending ally neg doesn't make other_corp participant; Accepted does
        snap.war_state.negotiations.push(PersistedWarNegotiation {
            id: 7001,
            war_id: 5001,
            kind: PersistedNegKind::Ally,
            from_corp: other_corp,
            isk: IskCents::from_isk(1_000),
            message: "pending".into(),
            status: "Pending".into(),
            effective_unix: None,
        });
        assert!(!is_war_participant_corp(snap, 5001, other_corp, 2_000_000_000));
        snap.war_state.negotiations.push(PersistedWarNegotiation {
            id: 7002,
            war_id: 5001,
            kind: PersistedNegKind::Ally,
            from_corp: other_corp,
            isk: IskCents::from_isk(2_000),
            message: "accepted".into(),
            status: "Accepted".into(),
            effective_unix: Some(1_700_001_000),
        });
        assert!(is_war_participant_corp(snap, 5001, other_corp, 2_000_000_000));

        // Third corp (not participant) with its own ally offer for neg_rows_for_caller
        snap.war_state.negotiations.push(PersistedWarNegotiation {
            id: 7003,
            war_id: 5001,
            kind: PersistedNegKind::Ally,
            from_corp: 98_000_888,
            isk: IskCents::from_isk(3_000),
            message: "third".into(),
            status: "Pending".into(),
            effective_unix: None,
        });
    });

    // 4. neg_rows_for_caller ACL & filtering
    assert_eq!(
        neg_rows_for_caller(&ctx, 5001, PersistedNegKind::Surrender, 98_000_888)
            .unwrap_err()
            .code,
        "NotWarParticipant"
    );
    let non_part_ally_rows =
        neg_rows_for_caller(&ctx, 5001, PersistedNegKind::Ally, 98_000_888).unwrap();
    assert_eq!(non_part_ally_rows.len(), 1);
    assert_eq!(non_part_ally_rows[0][0], PyRep::Integer(7003));
    let part_ally_rows =
        neg_rows_for_caller(&ctx, 5001, PersistedNegKind::Ally, def_corp).unwrap();
    assert_eq!(part_ally_rows.len(), 3);

    // 5. war_row combatActive before vs after WAR_TIMER_SECS
    let w_sample = WarRecord {
        war_id: 5001,
        aggressor_corp_id: agg_corp,
        defender_corp_id: def_corp,
        mutual: false,
        started_unix: 1_000,
        active: true,
    };
    let meta_sample = PersistedWarMeta {
        war_hq_id: Some(agg_hq_id),
        open_for_allies: true,
        finish_at_unix: Some(100_000),
    };
    let row_before = war_row(&w_sample, &meta_sample, 1_000 + WAR_TIMER_SECS - 1);
    assert_eq!(row_before[10], PyRep::Bool(false));
    let row_at = war_row(&w_sample, &meta_sample, 1_000 + WAR_TIMER_SECS);
    assert_eq!(row_at[10], PyRep::Bool(true));

    // 6. resolve_war_stat_id priority: arg0 > bound_obj_id > session bound_objects > DB fallback > 0
    assert_eq!(resolve_war_stat_id(&ctx, &agg_sess, Some(11), Some(22), agg_corp), 11);
    assert_eq!(resolve_war_stat_id(&ctx, &agg_sess, Some(0), Some(22), agg_corp), 22);
    ctx.bound_objects.write().unwrap().insert(
        901,
        crate::router::BoundObjectEntry {
            bound_id: 901,
            session_id: agg_sess.session_id,
            user_id: 1,
            character_id: agg_sess.character_id.unwrap(),
            service: "warStatisticMgr".into(),
            object_id: 33,
            oid_str: String::new(),
            ref_id: 1,
        },
    );
    assert_eq!(resolve_war_stat_id(&ctx, &agg_sess, None, None, agg_corp), 33);
    ctx.bound_objects.write().unwrap().clear();
    assert_eq!(resolve_war_stat_id(&ctx, &agg_sess, None, None, agg_corp), 5001);
    assert_eq!(resolve_war_stat_id(&ctx, &agg_sess, None, None, 98_999_999), 0);

    // 7. emit_war_changed & emit_mutual_wars_updated notifications
    let agg_char = agg_sess.character_id.unwrap();
    let agg_uid = ctx.db.with_snapshot(|s| s.characters[&agg_char].user_id);
    ctx.db
        .acquire_online_lock(agg_uid, 701, Some(agg_char), true, ctx.now_unix())
        .unwrap();
    let _ = ctx.drain_notifications_for_char(agg_char);
    emit_war_changed(&ctx, 999_999, vec![]); // non-existent war_id -> no-op
    emit_war_changed(&ctx, 5001, vec![("active", PyRep::Bool(true), PyRep::Bool(false))]);
    emit_mutual_wars_updated(&ctx, agg_char, agg_corp, def_corp);
    let notifs = ctx.drain_notifications_for_char(agg_char);
    assert!(notifs.iter().any(|n| n.event == "OnWarChanged"));
    assert!(notifs.iter().any(|n| n.event == "OnMutualWarsUpdated_Remote"));

    // 8. sync_wars: finish_at_unix <= now ends war; mutual war without defender structures stays active; non-mutual ends
    ctx.db.with_mut_snapshot(|snap| {
        snap.wars.insert(
            5002,
            WarRecord {
                war_id: 5002,
                aggressor_corp_id: agg_corp,
                defender_corp_id: other_corp, // has no structures
                mutual: true,
                started_unix: 100,
                active: true,
            },
        );
        snap.wars.insert(
            5003,
            WarRecord {
                war_id: 5003,
                aggressor_corp_id: agg_corp,
                defender_corp_id: other_corp, // has no structures, not mutual -> ends!
                mutual: false,
                started_unix: 100,
                active: true,
            },
        );
        snap.war_state.wars.insert(
            5001,
            PersistedWarMeta {
                war_hq_id: Some(agg_hq_id),
                open_for_allies: false,
                finish_at_unix: Some(1), // already past ctx.now_unix()
            },
        );
    });
    sync_wars(&ctx);
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.wars[&5001].active);
        assert!(snap.wars[&5002].active);
        assert!(!snap.wars[&5003].active);
        assert!(snap.war_state.wars[&5003].finish_at_unix.is_some());
    });
}

#[test]
fn war_registry_gm_gates_and_war_stat_bind_enforced() {
    use eve_db::PersistedWarMeta;
    use eve_machonet::service_roles::{ROLE_ADMIN, ROLE_GMH, ROLE_GML, ROLE_PLAYER_DEFAULT, ROLE_QA};

    let (ctx, mut agg_sess, _agg_char, mut def_sess, def_corp, _ally_sess, ally_corp, agg_hq_id) =
        setup_wars_ctx();
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

    // Seed an expired side war to prove rejected GM* and Bind calls are read-pure (never trigger sync_wars).
    let expired_side_war = 5_099_i64;
    let outsider_corp = 98_000_999_i64;
    let outsider_char = 90_000_999_i64;
    ctx.db.with_mut_snapshot(|snap| {
        let mut out_ch = snap.characters[&90_000_001].clone();
        out_ch.character_id = outsider_char;
        out_ch.user_id = 999;
        out_ch.corporation_id = outsider_corp;
        snap.characters.insert(outsider_char, out_ch);
        snap.corporations.insert(
            outsider_corp,
            {
                let mut c = CorporationRecord::default();
                c.corporation_id = outsider_corp;
                c.name = "Outsider".into();
                c.ticker = "OUT".into();
                c.ceo_id = outsider_char;
                c
            },
        );
        snap.wars.insert(
            expired_side_war,
            WarRecord {
                war_id: expired_side_war,
                aggressor_corp_id: 98_000_101,
                defender_corp_id: def_corp,
                mutual: true,
                started_unix: 100,
                active: true,
            },
        );
        snap.war_state.wars.insert(
            expired_side_war,
            PersistedWarMeta {
                war_hq_id: Some(agg_hq_id),
                open_for_allies: false,
                finish_at_unix: Some(1),
            },
        );
    });

    let expected_err_code = "InsufficientRole";
    let before_rejects = ctx.db.snapshot_tables();
    for bad_role in [0u64, ROLE_PLAYER_DEFAULT, ROLE_QA] {
        agg_sess.role = bad_role;
        for method in [
            "GMActivateDefender",
            "GMDeactivateDefender",
            "GMExtendAllyContract",
            "GMJoinDefender",
            "GMSetDeclareTime",
            "GMSetWarFinishTime",
        ] {
            let err = handle_wars(&ctx, &mut agg_sess, "warRegistry", method, &[], None)
                .unwrap()
                .expect_err(&format!("warRegistry.{method} must deny role {bad_role:#x}"));
            assert_eq!(err.code, expected_err_code);
        }
    }
    assert_eq!(ctx.db.snapshot_tables(), before_rejects);

    for ok_role in [
        ROLE_GMH,
        ROLE_GML,
        eve_machonet::service_roles::ROLE_GMS,
        ROLE_ADMIN,
    ] {
        agg_sess.role = ok_role;
        let now_u = ctx.now_unix();
        assert_eq!(
            handle_wars(
                &ctx,
                &mut agg_sess,
                "warRegistry",
                "GMJoinDefender",
                &[PyRep::Integer(war_id), PyRep::Integer(ally_corp)],
                None
            )
            .unwrap()
            .unwrap(),
            PyRep::Bool(true)
        );
        assert_eq!(
            handle_wars(
                &ctx,
                &mut agg_sess,
                "warRegistry",
                "GMExtendAllyContract",
                &[
                    PyRep::Integer(war_id),
                    PyRep::Integer(ally_corp),
                    PyRep::Integer(86_400),
                ],
                None
            )
            .unwrap()
            .unwrap(),
            PyRep::Bool(true)
        );
        assert_eq!(
            handle_wars(
                &ctx,
                &mut agg_sess,
                "warRegistry",
                "GMActivateDefender",
                &[PyRep::Integer(war_id), PyRep::Integer(ally_corp)],
                None
            )
            .unwrap()
            .unwrap(),
            PyRep::Bool(true)
        );
        assert_eq!(
            handle_wars(
                &ctx,
                &mut agg_sess,
                "warRegistry",
                "GMDeactivateDefender",
                &[PyRep::Integer(war_id), PyRep::Integer(ally_corp)],
                None
            )
            .unwrap()
            .unwrap(),
            PyRep::Bool(true)
        );
        assert_eq!(
            handle_wars(
                &ctx,
                &mut agg_sess,
                "warRegistry",
                "GMSetDeclareTime",
                &[
                    PyRep::Integer(war_id),
                    PyRep::Integer(now_u.saturating_sub(1_000) as i64),
                ],
                None
            )
            .unwrap()
            .unwrap(),
            PyRep::Bool(true)
        );
        let future_finish = now_u + 14_400;
        assert_eq!(
            handle_wars(
                &ctx,
                &mut agg_sess,
                "warRegistry",
                "GMSetWarFinishTime",
                &[
                    PyRep::Integer(war_id),
                    PyRep::Integer(future_finish as i64),
                ],
                None
            )
            .unwrap()
            .unwrap(),
            PyRep::Bool(true)
        );
        ctx.db.with_snapshot(|snap| {
            assert_eq!(snap.wars[&war_id].started_unix, now_u.saturating_sub(1_000));
            assert!(snap.wars[&war_id].active);
            assert_eq!(snap.war_state.wars[&war_id].finish_at_unix, Some(future_finish));
        });
    }
    let before_missing_war = ctx.db.snapshot_tables();
    assert_eq!(
        handle_wars(
            &ctx,
            &mut agg_sess,
            "warRegistry",
            "GMActivateDefender",
            &[PyRep::Integer(war_id + 777)],
            None
        )
        .unwrap()
        .unwrap_err()
        .code,
        "WarNotFound"
    );
    assert_eq!(ctx.db.snapshot_tables(), before_missing_war);

    // warStatisticMgr.Bind rejects outsiders and non-existent wars without mutating snapshot or bound_objects
    let before_rejects = ctx.db.snapshot_tables();
    let mut outsider_sess = agg_sess.clone();
    outsider_sess.user_id = Some(999);
    outsider_sess.character_id = Some(outsider_char);
    outsider_sess.corporation_id = Some(outsider_corp);
    outsider_sess.role = ROLE_PLAYER_DEFAULT;
    assert_eq!(
        handle_wars(
            &ctx,
            &mut outsider_sess,
            "warStatisticMgr",
            "Bind",
            &[PyRep::Integer(war_id)],
            None
        )
        .unwrap()
        .unwrap_err()
        .code,
        "BindAuthorizationDenied"
    );
    assert_eq!(
        handle_wars(
            &ctx,
            &mut agg_sess,
            "warStatisticMgr",
            "Bind",
            &[PyRep::Integer(war_id + 777)],
            None
        )
        .unwrap()
        .unwrap_err()
        .code,
        "BindAuthorizationDenied"
    );
    assert_eq!(ctx.db.snapshot_tables(), before_rejects);
    assert!(ctx.bound_objects.read().unwrap().is_empty());

    // Involved defender binds cleanly
    def_sess.role = ROLE_PLAYER_DEFAULT;
    let bound_id = handle_wars(
        &ctx,
        &mut def_sess,
        "warStatisticMgr",
        "Bind",
        &[PyRep::Integer(war_id)],
        None,
    )
    .unwrap()
    .unwrap();
    let bid = bound_id.as_i64().unwrap();
    assert!(bid > 0);
    assert_eq!(
        ctx.bound_objects
            .read()
            .unwrap()
            .get(&(bid as u64))
            .map(|e| (e.service.as_str(), e.object_id)),
        Some(("warStatisticMgr", war_id))
    );
}

#[test]
fn war_helpers_resolve_entity_alliance_and_ally_expiry_boundaries() {
    use super::social_wars_helpers::{
        is_ally_neg_active_at, is_war_participant_corp, resolve_entity_alliance_id, same_alliance,
    };
    use eve_db::{
        AllianceRecord, PersistedNegKind, PersistedWarNegotiation, WarRecord, WorldStateSnapshot,
    };

    let mut snap = WorldStateSnapshot::default();
    let agg_corp = 98_910_001_i64;
    let def_corp = 98_910_002_i64;
    let corp_ally = 98_910_003_i64;
    let corp_ally_sibling = 98_910_004_i64;
    let no_all_corp = 98_910_005_i64;
    let zero_all_corp = 98_910_006_i64;
    let alliance_ally_member = 98_910_007_i64;

    let agg_alliance = 99_910_001_i64;
    let shared_corp_ally_alliance = 99_910_003_i64;
    let ally_alliance = 99_910_007_i64;
    let empty_alliance = 99_910_099_i64;

    for (cid, all) in [
        (agg_corp, Some(agg_alliance)),
        (def_corp, None),
        (corp_ally, Some(shared_corp_ally_alliance)),
        (corp_ally_sibling, Some(shared_corp_ally_alliance)),
        (no_all_corp, None),
        (zero_all_corp, Some(0)),
        (alliance_ally_member, Some(ally_alliance)),
    ] {
        let mut c = CorporationRecord::default();
        c.corporation_id = cid;
        c.name = format!("Corp {cid}");
        c.ticker = format!("C{}", cid % 100);
        c.ceo_id = 90_000_001;
        c.alliance_id = all;
        snap.corporations.insert(cid, c);
    }
    snap.alliances.insert(
        empty_alliance,
        AllianceRecord {
            alliance_id: empty_alliance,
            name: "Empty Alliance".into(),
            short_name: "EMPA".into(),
            executor_corp_id: 98_910_099,
            member_corp_ids: Default::default(),
        },
    );

    // resolve_entity_alliance_id across corp, explicit alliance, inferred alliance, non-positive & unknown IDs
    assert_eq!(resolve_entity_alliance_id(&snap, 0), None);
    assert_eq!(resolve_entity_alliance_id(&snap, -42), None);
    assert_eq!(resolve_entity_alliance_id(&snap, no_all_corp), None);
    assert_eq!(resolve_entity_alliance_id(&snap, zero_all_corp), None);
    assert_eq!(resolve_entity_alliance_id(&snap, agg_corp), Some(agg_alliance));
    assert_eq!(resolve_entity_alliance_id(&snap, agg_alliance), Some(agg_alliance));
    assert_eq!(resolve_entity_alliance_id(&snap, empty_alliance), Some(empty_alliance));
    assert_eq!(resolve_entity_alliance_id(&snap, 99_999_888), None);

    // same_alliance covers corp-vs-corp, corp-vs-alliance, alliance-vs-corp, and alliance-vs-alliance
    assert!(same_alliance(&snap, corp_ally, corp_ally_sibling));
    assert!(same_alliance(&snap, agg_corp, agg_alliance));
    assert!(same_alliance(&snap, agg_alliance, agg_corp));
    assert!(same_alliance(&snap, agg_alliance, agg_alliance));
    assert!(!same_alliance(&snap, agg_corp, ally_alliance));
    assert!(!same_alliance(&snap, def_corp, no_all_corp));
    assert!(!same_alliance(&snap, 99_999_888, 99_999_888));

    let t0 = 1_700_000_000_u64;
    let war_corp = 700_910_001_i64;
    let war_alliance_agg = 700_910_002_i64;
    snap.wars.insert(
        war_corp,
        WarRecord {
            war_id: war_corp,
            aggressor_corp_id: agg_corp,
            defender_corp_id: def_corp,
            mutual: false,
            started_unix: t0,
            active: true,
        },
    );
    snap.wars.insert(
        war_alliance_agg,
        WarRecord {
            war_id: war_alliance_agg,
            aggressor_corp_id: agg_alliance,
            defender_corp_id: def_corp,
            mutual: false,
            started_unix: t0,
            active: true,
        },
    );
    let corp_neg = PersistedWarNegotiation {
        id: 1,
        war_id: war_corp,
        kind: PersistedNegKind::Ally,
        from_corp: corp_ally,
        isk: IskCents::ZERO,
        message: "corp ally".into(),
        status: "Accepted".into(),
        effective_unix: Some(t0),
    };
    let alliance_neg = PersistedWarNegotiation {
        id: 2,
        war_id: war_corp,
        kind: PersistedNegKind::Ally,
        from_corp: ally_alliance,
        isk: IskCents::ZERO,
        message: "alliance ally".into(),
        status: "Accepted".into(),
        effective_unix: Some(t0 + 100),
    };
    snap.war_state.negotiations.push(corp_neg.clone());
    snap.war_state.negotiations.push(alliance_neg.clone());
    snap.war_state
        .ally_expires_unix
        .insert((war_corp, corp_ally), t0 + 500);
    snap.war_state
        .ally_expires_unix
        .insert((war_corp, ally_alliance), t0 + 900);

    // Effective start & expiry boundaries
    assert!(is_ally_neg_active_at(&snap, &corp_neg, t0));
    assert!(is_ally_neg_active_at(&snap, &corp_neg, t0 + 499));
    assert!(!is_ally_neg_active_at(&snap, &corp_neg, t0 + 500));
    assert!(!is_ally_neg_active_at(&snap, &alliance_neg, t0 + 99));
    assert!(is_ally_neg_active_at(&snap, &alliance_neg, t0 + 100));
    assert!(is_ally_neg_active_at(&snap, &alliance_neg, t0 + 899));
    assert!(!is_ally_neg_active_at(&snap, &alliance_neg, t0 + 900));

    // is_war_participant_corp respects corp ally isolation, alliance ally membership, expiry, and alliance war parties
    assert!(is_war_participant_corp(&snap, war_corp, corp_ally, t0 + 499));
    assert!(!is_war_participant_corp(&snap, war_corp, corp_ally, t0 + 500));
    assert!(!is_war_participant_corp(&snap, war_corp, corp_ally_sibling, t0 + 100));
    assert!(is_war_participant_corp(&snap, war_corp, alliance_ally_member, t0 + 100));
    assert!(is_war_participant_corp(&snap, war_corp, alliance_ally_member, t0 + 899));
    assert!(!is_war_participant_corp(&snap, war_corp, alliance_ally_member, t0 + 900));
    assert!(is_war_participant_corp(&snap, war_alliance_agg, agg_corp, t0));
}

#[test]
fn war_extend_ally_contract_preserves_effective_unix_and_enforces_expiry() {
    use super::social_wars_helpers::is_war_participant_corp;
    use eve_db::clock::VirtualServerClock;
    use eve_machonet::service_roles::{ROLE_GMH, ROLE_PLAYER_DEFAULT};
    use std::sync::Arc;

    let (ctx, mut agg_sess, agg_corp, mut def_sess, def_corp, mut ally_sess, ally_corp, agg_hq_id) =
        setup_wars_ctx();
    let t0 = 1_700_000_000_u64;
    let clock = Arc::new(VirtualServerClock::with_fixed_base(t0));
    ctx.set_clock(clock.clone());

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

    agg_sess.role = ROLE_GMH;
    assert_eq!(
        handle_wars(
            &ctx,
            &mut agg_sess,
            "warRegistry",
            "GMJoinDefender",
            &[PyRep::Integer(war_id), PyRep::Integer(ally_corp)],
            None,
        )
        .unwrap()
        .unwrap(),
        PyRep::Bool(true)
    );

    // Seed a killmail for the active ally attacking the aggressor at t0.
    let agg_char = agg_sess.character_id.unwrap();
    let ally_char = ally_sess.character_id.unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        snap.killmails.insert(
            81_001,
            KillmailRecord {
                kill_id: 81_001,
                solar_system_id: 30_000_142,
                victim_character_id: agg_char,
                victim_corp_id: agg_corp,
                attacker_character_id: ally_char,
                attacker_corp_id: ally_corp,
                ship_type_id: 587,
                total_value_isk: 2_000_000.0,
                killed_unix: t0,
                ..Default::default()
            },
        );
    });

    // Extend twice (+3_600s then +1_800s) -> effective_unix stays t0, ally_expires_unix becomes t0 + 5_400.
    assert_eq!(
        handle_wars(
            &ctx,
            &mut agg_sess,
            "warRegistry",
            "GMExtendAllyContract",
            &[
                PyRep::Integer(war_id),
                PyRep::Integer(ally_corp),
                PyRep::Integer(3_600),
            ],
            None,
        )
        .unwrap()
        .unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(
        handle_wars(
            &ctx,
            &mut agg_sess,
            "warRegistry",
            "GMExtendAllyContract",
            &[
                PyRep::Integer(war_id),
                PyRep::Integer(ally_corp),
                PyRep::Integer(1_800),
            ],
            None,
        )
        .unwrap()
        .unwrap(),
        PyRep::Bool(true)
    );

    ctx.db.with_snapshot(|snap| {
        let neg = snap
            .war_state
            .negotiations
            .iter()
            .find(|n| n.war_id == war_id && n.from_corp == ally_corp && n.status == "Accepted")
            .expect("accepted ally neg");
        assert_eq!(neg.effective_unix, Some(t0));
        assert_eq!(
            snap.war_state.ally_expires_unix.get(&(war_id, ally_corp)),
            Some(&(t0 + 5_400))
        );
        assert!(is_war_participant_corp(snap, war_id, ally_corp, t0));
        let summary = war_kills_in_snap(&ctx, snap, war_id).expect("war_kills_in_snap");
        assert_eq!(summary.ally_corp_ids, vec![ally_corp]);
        assert_eq!(summary.kills.len(), 1);
    });

    // Immediately after GMExtendAllyContract, ally stays active across GetAllyRowsByWarID, GetBaseInfo, and warStatisticMgr.Bind.
    let rows_active = handle_wars(
        &ctx,
        &mut def_sess,
        "warRegistry",
        "GetAllyRowsByWarID",
        &[PyRep::Integer(war_id)],
        None,
    )
    .unwrap()
    .unwrap();
    match rows_active {
        PyRep::Rowset { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0][0], PyRep::Integer(ally_corp));
            assert_eq!(rows[0][1], PyRep::Integer(crate::domain::wars::unix_to_filetime(t0)));
            assert_eq!(rows[0][3], PyRep::Bool(true));
        }
        other => panic!("expected Rowset, got {other:?}"),
    }
    let PyRep::Tuple(base_parts) = handle_wars(
        &ctx,
        &mut def_sess,
        "warStatisticMgr",
        "GetBaseInfo",
        &[PyRep::Integer(war_id)],
        None,
    )
    .unwrap()
    .unwrap()
    else {
        panic!("expected 4-tuple from GetBaseInfo");
    };
    assert_eq!(base_parts.len(), 4);
    let base_info = &base_parts[0];
    let allies_dict = base_info.dict_get("allies").expect("allies dict");
    match allies_dict {
        PyRep::Dict(entries) => {
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0].0, PyRep::Integer(ally_corp));
            assert_eq!(dict_i64(&entries[0].1, "allyID"), Some(ally_corp));
            assert_eq!(
                dict_i64(&entries[0].1, "timeStarted"),
                Some(crate::domain::wars::unix_to_filetime(t0))
            );
            assert_eq!(
                dict_i64(&entries[0].1, "timeFinished"),
                Some(crate::domain::wars::unix_to_filetime(t0 + 5_400))
            );
        }
        other => panic!("expected Dict, got {other:?}"),
    }

    ally_sess.role = ROLE_PLAYER_DEFAULT;
    let bound_rep = handle_wars(
        &ctx,
        &mut ally_sess,
        "warStatisticMgr",
        "Bind",
        &[PyRep::Integer(war_id)],
        None,
    )
    .unwrap()
    .unwrap();
    assert!(bound_rep.as_i64().unwrap() > 0);

    // At exp - 1 (t0 + 5_399) ally is still active; at exp (t0 + 5_400) ally becomes inactive across all surfaces.
    clock.advance_secs_and_read_offset(5_399);
    assert!(ctx
        .db
        .with_snapshot(|snap| is_war_participant_corp(snap, war_id, ally_corp, ctx.now_unix())));
    clock.advance_secs_and_read_offset(1);
    let expired_at = ctx.now_unix();
    assert_eq!(expired_at, t0 + 5_400);

    ctx.db.with_mut_snapshot(|snap| {
        snap.killmails.insert(
            81_002,
            KillmailRecord {
                kill_id: 81_002,
                solar_system_id: 30_000_142,
                victim_character_id: agg_char,
                victim_corp_id: agg_corp,
                attacker_character_id: ally_char,
                attacker_corp_id: ally_corp,
                ship_type_id: 587,
                total_value_isk: 3_000_000.0,
                killed_unix: expired_at,
                ..Default::default()
            },
        );
    });

    ctx.db.with_snapshot(|snap| {
        assert!(!is_war_participant_corp(snap, war_id, ally_corp, expired_at));
        let summary = war_kills_in_snap(&ctx, snap, war_id).expect("war_kills_in_snap expired");
        assert!(summary.ally_corp_ids.is_empty());
        // Pre-expiry kill 81_001 counts, post-expiry kill 81_002 does not.
        assert_eq!(summary.kills.iter().map(|k| k.kill_id).collect::<Vec<_>>(), vec![81_001]);
    });
    let rows_expired = handle_wars(
        &ctx,
        &mut def_sess,
        "warRegistry",
        "GetAllyRowsByWarID",
        &[PyRep::Integer(war_id)],
        None,
    )
    .unwrap()
    .unwrap();
    match rows_expired {
        PyRep::Rowset { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0][3], PyRep::Bool(false));
        }
        other => panic!("expected Rowset, got {other:?}"),
    }
    assert_eq!(
        handle_wars(
            &ctx,
            &mut ally_sess,
            "warStatisticMgr",
            "Bind",
            &[PyRep::Integer(war_id)],
            None,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "BindAuthorizationDenied"
    );

    // Re-joining via GMJoinDefender clears stale ally_expires_unix and restores active status.
    assert_eq!(
        handle_wars(
            &ctx,
            &mut agg_sess,
            "warRegistry",
            "GMJoinDefender",
            &[PyRep::Integer(war_id), PyRep::Integer(ally_corp)],
            None,
        )
        .unwrap()
        .unwrap(),
        PyRep::Bool(true)
    );
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.war_state.ally_expires_unix.contains_key(&(war_id, ally_corp)));
        assert!(is_war_participant_corp(snap, war_id, ally_corp, ctx.now_unix()));
    });
    assert!(
        handle_wars(
            &ctx,
            &mut ally_sess,
            "warStatisticMgr",
            "Bind",
            &[PyRep::Integer(war_id)],
            None,
        )
        .unwrap()
        .unwrap()
        .as_i64()
        .unwrap()
            > 0
    );

    // AcceptAllyNegotiation likewise clears stale ally_expires_unix on fresh acceptance.
    let ally_corp_2 = 98_000_199_i64;
    let ally_char_2 = 90_000_199_i64;
    let mut ally_sess_2 = ally_sess.clone();
    ally_sess_2.character_id = Some(ally_char_2);
    ally_sess_2.corporation_id = Some(ally_corp_2);
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters[&ally_char].clone();
        ch.character_id = ally_char_2;
        ch.user_id = 199;
        ch.corporation_id = ally_corp_2;
        snap.characters.insert(ally_char_2, ch);
        let mut corp = snap.corporations[&ally_corp].clone();
        corp.corporation_id = ally_corp_2;
        corp.ceo_id = ally_char_2;
        corp.members = [ally_char_2].into_iter().collect();
        corp.member_roles = [(ally_char_2, u64::MAX)].into_iter().collect();
        snap.corporations.insert(ally_corp_2, corp);
        snap.war_state
            .ally_expires_unix
            .insert((war_id, ally_corp_2), ctx.now_unix());
    });
    let offer = handle_wars(
        &ctx,
        &mut ally_sess_2,
        "warsInfoMgr",
        "CreateWarAllyOffer",
        &[PyRep::Integer(war_id), PyRep::Real(0.0), PyRep::String("join".into())],
        None,
    )
    .unwrap()
    .unwrap();
    let offer_id = dict_i64(&offer, "negotiationID").unwrap();
    let acc = handle_wars(
        &ctx,
        &mut def_sess,
        "warsInfoMgr",
        "AcceptAllyNegotiation",
        &[PyRep::Integer(offer_id)],
        None,
    )
    .unwrap()
    .unwrap();
    assert_eq!(dict_i64(&acc, "allyID"), Some(ally_corp_2));
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.war_state.ally_expires_unix.contains_key(&(war_id, ally_corp_2)));
    });
}

#[test]
fn war_gm_ally_corp_vs_alliance_cannot_ally_own_war_and_alliance_ally_stats_and_rollback() {
    use super::social_wars_helpers::is_war_participant_corp;
    use eve_db::{clock::VirtualServerClock, AllianceRecord, PersistedWarMeta};
    use eve_machonet::service_roles::{ROLE_GMH, ROLE_PLAYER_DEFAULT};
    use std::sync::Arc;

    let (ctx, mut agg_sess, agg_corp, mut def_sess, def_corp, _ally_sess, _ally_corp, _agg_hq_id) =
        setup_wars_ctx();
    let t0 = 1_700_000_000_u64;
    let clock = Arc::new(VirtualServerClock::with_fixed_base(t0));
    ctx.set_clock(clock.clone());

    let agg_alliance = 99_920_001_i64;
    let def_alliance = 99_920_002_i64;
    let ext_alliance = 99_920_003_i64;
    let ext_member_corp = 98_920_003_i64;
    let ext_member_char = 90_920_003_i64;
    let war_corp_vs_corp = 700_920_001_i64;
    let war_alliance_vs_corp = 700_920_002_i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.get_mut(&agg_corp).unwrap().alliance_id = Some(agg_alliance);
        snap.corporations.get_mut(&def_corp).unwrap().alliance_id = Some(def_alliance);
        let mut ext_ch = snap.characters[&agg_sess.character_id.unwrap()].clone();
        ext_ch.character_id = ext_member_char;
        ext_ch.user_id = 920;
        ext_ch.corporation_id = ext_member_corp;
        snap.characters.insert(ext_member_char, ext_ch);
        let mut ext_c = CorporationRecord::default();
        ext_c.corporation_id = ext_member_corp;
        ext_c.name = "Ext Member".into();
        ext_c.ticker = "EXTM".into();
        ext_c.ceo_id = ext_member_char;
        ext_c.alliance_id = Some(ext_alliance);
        snap.corporations.insert(ext_member_corp, ext_c);

        snap.alliances.insert(
            def_alliance,
            AllianceRecord {
                alliance_id: def_alliance,
                name: "Def Alliance".into(),
                short_name: "DEFA".into(),
                executor_corp_id: def_corp,
                member_corp_ids: [def_corp].into_iter().collect(),
            },
        );
        snap.alliances.insert(
            ext_alliance,
            AllianceRecord {
                alliance_id: ext_alliance,
                name: "Ext Alliance".into(),
                short_name: "EXTA".into(),
                executor_corp_id: ext_member_corp,
                member_corp_ids: [ext_member_corp].into_iter().collect(),
            },
        );
        snap.wars.insert(
            war_corp_vs_corp,
            WarRecord {
                war_id: war_corp_vs_corp,
                aggressor_corp_id: agg_corp,
                defender_corp_id: def_corp,
                mutual: false,
                started_unix: t0,
                active: true,
            },
        );
        snap.war_state
            .wars
            .insert(war_corp_vs_corp, PersistedWarMeta { open_for_allies: true, ..Default::default() });
        snap.wars.insert(
            war_alliance_vs_corp,
            WarRecord {
                war_id: war_alliance_vs_corp,
                aggressor_corp_id: agg_alliance,
                defender_corp_id: def_corp,
                mutual: false,
                started_unix: t0,
                active: true,
            },
        );
    });

    agg_sess.role = ROLE_GMH;
    let snap_before_rejects = ctx.db.snapshot_tables();

    // Corp-vs-alliance & alliance-vs-corp CannotAllyOwnWar rejections leave snapshot untouched.
    let expected_own_war_err = "CannotAllyOwnWar";
    for (method, wid, target) in [
        ("GMJoinDefender", war_corp_vs_corp, agg_alliance),
        ("GMJoinDefender", war_corp_vs_corp, def_alliance),
        ("GMJoinDefender", war_alliance_vs_corp, agg_corp),
        ("GMActivateDefender", war_corp_vs_corp, agg_alliance),
    ] {
        let err = handle_wars(
            &ctx,
            &mut agg_sess,
            "warRegistry",
            method,
            &[PyRep::Integer(wid), PyRep::Integer(target)],
            None,
        )
        .unwrap()
        .unwrap_err();
        assert_eq!(err.code, expected_own_war_err, "{method}({wid}, {target})");
    }
    // Non-matching ally & bad args on GM methods likewise leave snapshot unchanged.
    assert_eq!(
        handle_wars(
            &ctx,
            &mut agg_sess,
            "warRegistry",
            "GMExtendAllyContract",
            &[PyRep::Integer(war_corp_vs_corp), PyRep::Integer(ext_alliance), PyRep::Integer(600)],
            None,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "AllyNotFound"
    );
    assert_eq!(
        handle_wars(
            &ctx,
            &mut agg_sess,
            "warRegistry",
            "GMDeactivateDefender",
            &[PyRep::Integer(war_corp_vs_corp), PyRep::Integer(ext_alliance)],
            None,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "AllyNotFound"
    );
    assert_eq!(ctx.db.snapshot_tables(), snap_before_rejects);

    // Direct try_with_mut_snapshot rollback guard matching handle_war_registry_gm_rpc:
    let mut_tx_err = ctx.db.try_with_mut_snapshot(|snap| -> Result<(), BridgeRpcError> {
        snap.war_state.next_negotiation_id += 99;
        snap.war_state.ally_expires_unix.insert((war_corp_vs_corp, ext_alliance), t0 + 1);
        Err(BridgeRpcError {
            code: "SimulatedFailure".into(),
            message: "rollback check".into(),
            client_error: None,
        })
    });
    assert_eq!(mut_tx_err.unwrap_err().code, "SimulatedFailure");
    assert_eq!(ctx.db.snapshot_tables(), snap_before_rejects);

    // Alliance-ID ally joins cleanly via GMJoinDefender and resolves kills + participation + binding + expiry.
    assert_eq!(
        handle_wars(
            &ctx,
            &mut agg_sess,
            "warRegistry",
            "GMJoinDefender",
            &[PyRep::Integer(war_corp_vs_corp), PyRep::Integer(ext_alliance)],
            None,
        )
        .unwrap()
        .unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(
        handle_wars(
            &ctx,
            &mut agg_sess,
            "warRegistry",
            "GMExtendAllyContract",
            &[PyRep::Integer(war_corp_vs_corp), PyRep::Integer(ext_alliance), PyRep::Integer(600)],
            None,
        )
        .unwrap()
        .unwrap(),
        PyRep::Bool(true)
    );
    let agg_char = agg_sess.character_id.unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        let lm = snap.war_lifecycle_meta.get(&war_corp_vs_corp).expect("war lifecycle meta");
        assert!(lm.allies.iter().any(|a| a.alliance_id == Some(ext_alliance) && a.corporation_id.is_none()));
        snap.killmails.insert(
            82_001,
            KillmailRecord {
                kill_id: 82_001,
                solar_system_id: 30_000_142,
                victim_character_id: agg_char,
                victim_corp_id: agg_corp,
                attacker_character_id: ext_member_char,
                attacker_corp_id: ext_member_corp,
                attacker_alliance_id: None,
                ship_type_id: 587,
                total_value_isk: 4_000_000.0,
                killed_unix: t0 + 10,
                ..Default::default()
            },
        );
    });
    ctx.db.with_snapshot(|snap| {
        assert!(is_war_participant_corp(snap, war_corp_vs_corp, ext_member_corp, t0));
        let summary = war_kills_in_snap(&ctx, snap, war_corp_vs_corp).expect("war_kills_in_snap");
        assert_eq!(summary.ally_corp_ids, vec![ext_alliance]);
        assert_eq!(summary.kills.len(), 1);
    });

    def_sess.role = ROLE_PLAYER_DEFAULT;
    def_sess.character_id = Some(ext_member_char);
    def_sess.corporation_id = Some(ext_member_corp);
    def_sess.alliance_id = None;
    assert!(
        handle_wars(
            &ctx,
            &mut def_sess,
            "warStatisticMgr",
            "Bind",
            &[PyRep::Integer(war_corp_vs_corp)],
            None,
        )
        .unwrap()
        .unwrap()
        .as_i64()
        .unwrap()
            > 0
    );

    // Advance to expiry (t0 + 600): alliance member corp loses participation and warStatisticMgr.Bind access.
    clock.advance_secs_and_read_offset(600);
    ctx.db.with_snapshot(|snap| {
        assert!(!is_war_participant_corp(snap, war_corp_vs_corp, ext_member_corp, ctx.now_unix()));
        assert!(war_kills_in_snap(&ctx, snap, war_corp_vs_corp)
            .unwrap()
            .ally_corp_ids
            .is_empty());
    });
    assert_eq!(
        handle_wars(
            &ctx,
            &mut def_sess,
            "warStatisticMgr",
            "Bind",
            &[PyRep::Integer(war_corp_vs_corp)],
            None,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "BindAuthorizationDenied"
    );
}

#[test]
fn gm_extend_ally_contract_rejects_zero_and_anchors_warmup_on_effective_unix() {
    use super::social_wars_helpers::is_war_participant_corp;
    use eve_db::clock::VirtualServerClock;
    use eve_machonet::service_roles::ROLE_GMH;
    use std::sync::Arc;

    let (ctx, mut agg_sess, _agg_corp, mut def_sess, def_corp, mut ally_sess, ally_corp, _hq_id) =
        setup_wars_ctx();
    let t0 = 1_700_000_000_u64;
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(t0)));

    let decl = handle_wars(
        &ctx,
        &mut agg_sess,
        "corpRegistry",
        "DeclareWarAgainst",
        &[PyRep::Integer(def_corp)],
        None,
    )
    .unwrap()
    .unwrap();
    let war_id = dict_i64(&decl, "warID").unwrap();
    handle_wars(
        &ctx,
        &mut def_sess,
        "warsInfoMgr",
        "SetOpenForAllies",
        &[PyRep::Integer(war_id), PyRep::Bool(true)],
        None,
    )
    .unwrap()
    .unwrap();
    let offer = handle_wars(
        &ctx,
        &mut ally_sess,
        "warsInfoMgr",
        "CreateWarAllyOffer",
        &[
            PyRep::Integer(war_id),
            PyRep::Real(0.0),
            PyRep::String("warmup ally".into()),
        ],
        None,
    )
    .unwrap()
    .unwrap();
    let offer_id = dict_i64(&offer, "negotiationID").unwrap();
    handle_wars(
        &ctx,
        &mut def_sess,
        "warsInfoMgr",
        "AcceptAllyNegotiation",
        &[PyRep::Integer(offer_id)],
        None,
    )
    .unwrap()
    .unwrap();
    let effective_unix = t0 + WAR_TIMER_SECS;

    agg_sess.role = ROLE_GMH;
    let snap_before_zero = ctx.db.snapshot_tables();
    // ts_or_delta == 0 (raw 0 and FILETIME Unix epoch 0) must fail with InvalidArgument without mutating state.
    for zero_rep in [PyRep::Integer(0), PyRep::Integer(116_444_736_000_000_000)] {
        assert_eq!(
            handle_wars(
                &ctx,
                &mut agg_sess,
                "warRegistry",
                "GMExtendAllyContract",
                &[PyRep::Integer(war_id), PyRep::Integer(ally_corp), zero_rep],
                None,
            )
            .unwrap()
            .unwrap_err()
            .code,
            "InvalidArgument"
        );
    }
    assert_eq!(ctx.db.snapshot_tables(), snap_before_zero);

    // Extending during the 24h warm-up anchors on effective_unix so updated_exp >= effective_unix + delta.
    assert_eq!(
        handle_wars(
            &ctx,
            &mut agg_sess,
            "warRegistry",
            "GMExtendAllyContract",
            &[
                PyRep::Integer(war_id),
                PyRep::Integer(ally_corp),
                PyRep::Integer(3_600),
            ],
            None,
        )
        .unwrap()
        .unwrap(),
        PyRep::Bool(true)
    );
    ctx.db.with_snapshot(|snap| {
        assert_eq!(
            snap.war_state.ally_expires_unix.get(&(war_id, ally_corp)),
            Some(&(effective_unix + 3_600))
        );
    });

    // A second relative extension during warm-up stacks onto current_exp.
    assert_eq!(
        handle_wars(
            &ctx,
            &mut agg_sess,
            "warRegistry",
            "GMExtendAllyContract",
            &[
                PyRep::Integer(war_id),
                PyRep::Integer(ally_corp),
                PyRep::Integer(1_800),
            ],
            None,
        )
        .unwrap()
        .unwrap(),
        PyRep::Bool(true)
    );
    ctx.db.with_snapshot(|snap| {
        assert_eq!(
            snap.war_state.ally_expires_unix.get(&(war_id, ally_corp)),
            Some(&(effective_unix + 5_400))
        );
        assert!(!is_war_participant_corp(snap, war_id, ally_corp, effective_unix - 1));
        assert!(is_war_participant_corp(snap, war_id, ally_corp, effective_unix));
        assert!(is_war_participant_corp(
            snap,
            war_id,
            ally_corp,
            effective_unix + 5_399
        ));
        assert!(!is_war_participant_corp(
            snap,
            war_id,
            ally_corp,
            effective_unix + 5_400
        ));
    });
}

#[test]
fn create_war_ally_offer_and_accept_enforce_same_alliance_and_allow_reoffer_after_expiry() {
    use super::social_wars_helpers::is_war_participant_corp;
    use eve_db::{clock::VirtualServerClock, AllianceRecord, PersistedWarMeta};
    use eve_machonet::service_roles::ROLE_GMH;
    use std::sync::Arc;

    let (ctx, mut agg_sess, agg_corp, mut def_sess, def_corp, mut ally_sess, ally_corp, _hq_id) =
        setup_wars_ctx();
    let t0 = 1_700_000_000_u64;
    let clock = Arc::new(VirtualServerClock::with_fixed_base(t0));
    ctx.set_clock(clock.clone());

    let agg_alliance = 99_930_001_i64;
    let sister_corp = 98_930_002_i64;
    let sister_char = 90_930_002_i64;
    let war_alliance_vs_corp = 700_930_001_i64;

    let mut sister_sess = agg_sess.clone();
    sister_sess.character_id = Some(sister_char);
    sister_sess.corporation_id = Some(sister_corp);
    sister_sess.alliance_id = Some(agg_alliance);

    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.get_mut(&agg_corp).unwrap().alliance_id = Some(agg_alliance);
        let mut s_ch = snap.characters[&agg_sess.character_id.unwrap()].clone();
        s_ch.character_id = sister_char;
        s_ch.user_id = 930;
        s_ch.corporation_id = sister_corp;
        s_ch.alliance_id = Some(agg_alliance);
        snap.characters.insert(sister_char, s_ch);

        let mut s_corp = CorporationRecord::default();
        s_corp.corporation_id = sister_corp;
        s_corp.name = "Agg Sister Corp".into();
        s_corp.ticker = "ASIS".into();
        s_corp.ceo_id = sister_char;
        s_corp.alliance_id = Some(agg_alliance);
        s_corp.members = [sister_char].into_iter().collect();
        s_corp.member_roles = [(sister_char, u64::MAX)].into_iter().collect();
        snap.corporations.insert(sister_corp, s_corp);

        snap.alliances.insert(
            agg_alliance,
            AllianceRecord {
                alliance_id: agg_alliance,
                name: "Agg Alliance".into(),
                short_name: "AGGA".into(),
                executor_corp_id: agg_corp,
                member_corp_ids: [agg_corp, sister_corp].into_iter().collect(),
            },
        );
        snap.wars.insert(
            war_alliance_vs_corp,
            WarRecord {
                war_id: war_alliance_vs_corp,
                aggressor_corp_id: agg_alliance,
                defender_corp_id: def_corp,
                mutual: false,
                started_unix: t0.saturating_sub(WAR_TIMER_SECS),
                active: true,
            },
        );
        snap.war_state.wars.insert(
            war_alliance_vs_corp,
            PersistedWarMeta {
                open_for_allies: true,
                ..Default::default()
            },
        );
    });

    // Non-executor member corp of an alliance at war cannot bypass CannotAllyOwnWar via CreateWarAllyOffer
    // and cannot submit surrender negotiations (only the war owner or alliance executor may).
    let snap_before_own_war = ctx.db.snapshot_tables();
    assert_eq!(
        handle_wars(
            &ctx,
            &mut sister_sess,
            "warsInfoMgr",
            "CreateWarAllyOffer",
            &[
                PyRep::Integer(war_alliance_vs_corp),
                PyRep::Real(0.0),
                PyRep::String("sneaky".into()),
            ],
            None,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "CannotAllyOwnWar"
    );
    assert_eq!(
        handle_wars(
            &ctx,
            &mut sister_sess,
            "warsInfoMgr",
            "CreateSurrenderNegotiation",
            &[
                PyRep::Integer(war_alliance_vs_corp),
                PyRep::Real(0.0),
                PyRep::String("unauthorized surrender".into()),
            ],
            None,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "NotWarParticipant"
    );
    assert_eq!(ctx.db.snapshot_tables(), snap_before_own_war);

    // Alliance executor_corp_id CAN submit a surrender offer on the alliance war, cannot self-accept/decline,
    // non-executor sister corp cannot accept/decline, and opposing defender CAN decline.
    let surr = handle_wars(
        &ctx,
        &mut agg_sess,
        "warsInfoMgr",
        "CreateSurrenderNegotiation",
        &[
            PyRep::Integer(war_alliance_vs_corp),
            PyRep::Real(0.0),
            PyRep::String("peace".into()),
        ],
        None,
    )
    .unwrap()
    .unwrap();
    assert_eq!(dict_str(&surr, "status"), Some("Offered"));
    let surr_id = dict_i64(&surr, "negotiationID").unwrap();
    let expected_counterparty_err = "NotNegotiationCounterparty";
    for method in ["AcceptSurrender", "DeclineSurrender"] {
        assert_eq!(
            handle_wars(
                &ctx,
                &mut agg_sess,
                "warsInfoMgr",
                method,
                &[PyRep::Integer(surr_id)],
                None,
            )
            .unwrap()
            .unwrap_err()
            .code,
            expected_counterparty_err
        );
        assert_eq!(
            handle_wars(
                &ctx,
                &mut sister_sess,
                "warsInfoMgr",
                method,
                &[PyRep::Integer(surr_id)],
                None,
            )
            .unwrap()
            .unwrap_err()
            .code,
            expected_counterparty_err
        );
    }
    let declined = handle_wars(
        &ctx,
        &mut def_sess,
        "warsInfoMgr",
        "DeclineSurrender",
        &[PyRep::Integer(surr_id)],
        None,
    )
    .unwrap()
    .unwrap();
    assert_eq!(dict_str(&declined, "status"), Some("Declined"));

    // External ally joins, receives a finite expiry via GMExtendAllyContract, and cannot re-offer while unexpired.
    agg_sess.role = ROLE_GMH;
    handle_wars(
        &ctx,
        &mut agg_sess,
        "warRegistry",
        "GMJoinDefender",
        &[PyRep::Integer(war_alliance_vs_corp), PyRep::Integer(ally_corp)],
        None,
    )
    .unwrap()
    .unwrap();
    handle_wars(
        &ctx,
        &mut agg_sess,
        "warRegistry",
        "GMExtendAllyContract",
        &[
            PyRep::Integer(war_alliance_vs_corp),
            PyRep::Integer(ally_corp),
            PyRep::Integer(600),
        ],
        None,
    )
    .unwrap()
    .unwrap();

    // Before expiry (t0 + 599): duplicate CreateWarAllyOffer is blocked with AllyOfferAlreadyExists,
    // and AcceptAllyNegotiation rejects an existing pending offer with AlreadyWarAlly.
    clock.advance_secs_and_read_offset(599);
    assert_eq!(
        handle_wars(
            &ctx,
            &mut ally_sess,
            "warsInfoMgr",
            "CreateWarAllyOffer",
            &[
                PyRep::Integer(war_alliance_vs_corp),
                PyRep::Real(0.0),
                PyRep::String("too early".into()),
            ],
            None,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "AllyOfferAlreadyExists"
    );
    let pre_expiry_pending_id = ctx.db.with_mut_snapshot(|snap| {
        snap.war_state.next_negotiation_id += 1;
        let id = snap.war_state.next_negotiation_id;
        snap.war_state.negotiations.push(Negotiation {
            id,
            war_id: war_alliance_vs_corp,
            kind: NegKind::Ally,
            from_corp: ally_corp,
            isk: IskCents::ZERO,
            message: "pending before expiry".into(),
            status: "Offered".into(),
            effective_unix: None,
        });
        id
    });
    assert_eq!(
        handle_wars(
            &ctx,
            &mut def_sess,
            "warsInfoMgr",
            "AcceptAllyNegotiation",
            &[PyRep::Integer(pre_expiry_pending_id)],
            None,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "AlreadyWarAlly"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.war_state
            .negotiations
            .retain(|n| n.id != pre_expiry_pending_id);
    });

    // At expiry (t0 + 600): expired ally can re-offer and defender can accept again cleanly.
    clock.advance_secs_and_read_offset(1);
    let reoffer_now = ctx.now_unix();
    assert_eq!(reoffer_now, t0 + 600);
    let reoffer = handle_wars(
        &ctx,
        &mut ally_sess,
        "warsInfoMgr",
        "CreateWarAllyOffer",
        &[
            PyRep::Integer(war_alliance_vs_corp),
            PyRep::Real(0.0),
            PyRep::String("re-join".into()),
        ],
        None,
    )
    .unwrap()
    .unwrap();
    let reoffer_id = dict_i64(&reoffer, "negotiationID").unwrap();
    let accepted = handle_wars(
        &ctx,
        &mut def_sess,
        "warsInfoMgr",
        "AcceptAllyNegotiation",
        &[PyRep::Integer(reoffer_id)],
        None,
    )
    .unwrap()
    .unwrap();
    let new_eff = reoffer_now + WAR_TIMER_SECS;
    assert_eq!(dict_i64(&accepted, "allyID"), Some(ally_corp));
    assert_eq!(
        dict_i64(&accepted, "timeStarted"),
        Some(crate::domain::wars::unix_to_filetime(new_eff))
    );

    ctx.db.with_snapshot(|snap| {
        assert!(!snap
            .war_state
            .ally_expires_unix
            .contains_key(&(war_alliance_vs_corp, ally_corp)));
        // Old expired contract must not bypass the new contract's 24h warm-up.
        assert!(!is_war_participant_corp(
            snap,
            war_alliance_vs_corp,
            ally_corp,
            new_eff - 1
        ));
        assert!(is_war_participant_corp(
            snap,
            war_alliance_vs_corp,
            ally_corp,
            new_eff
        ));
        let accepted_count = snap
            .war_state
            .negotiations
            .iter()
            .filter(|n| {
                n.war_id == war_alliance_vs_corp
                    && n.kind == NegKind::Ally
                    && n.from_corp == ally_corp
                    && n.status == "Accepted"
            })
            .count();
        assert_eq!(accepted_count, 1);
        let lm_count = snap.war_lifecycle_meta[&war_alliance_vs_corp]
            .allies
            .iter()
            .filter(|a| a.corporation_id == Some(ally_corp))
            .count();
        assert_eq!(lm_count, 1);
    });
}

#[test]
fn gm_activate_defender_without_ally_id_clears_expired_ally_expiries_and_preserves_unexpired() {
    use super::social_wars_helpers::is_war_participant_corp;
    use eve_db::clock::VirtualServerClock;
    use eve_machonet::service_roles::ROLE_GMH;
    use std::sync::Arc;

    let (ctx, mut agg_sess, _agg_corp, _def_sess, def_corp, _ally_sess, ally_corp, _hq_id) =
        setup_wars_ctx();
    let t0 = 1_700_000_000_u64;
    let clock = Arc::new(VirtualServerClock::with_fixed_base(t0));
    ctx.set_clock(clock.clone());

    let decl = handle_wars(
        &ctx,
        &mut agg_sess,
        "corpRegistry",
        "DeclareWarAgainst",
        &[PyRep::Integer(def_corp)],
        None,
    )
    .unwrap()
    .unwrap();
    let war_id = dict_i64(&decl, "warID").unwrap();
    let other_war_id = war_id + 100;
    let ally_corp_2 = 98_000_299_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut c2 = snap.corporations[&ally_corp].clone();
        c2.corporation_id = ally_corp_2;
        snap.corporations.insert(ally_corp_2, c2);
        snap.war_state
            .ally_expires_unix
            .insert((other_war_id, ally_corp), t0);
    });

    agg_sess.role = ROLE_GMH;
    for aid in [ally_corp, ally_corp_2] {
        handle_wars(
            &ctx,
            &mut agg_sess,
            "warRegistry",
            "GMJoinDefender",
            &[PyRep::Integer(war_id), PyRep::Integer(aid)],
            None,
        )
        .unwrap()
        .unwrap();
    }
    // ally_corp expires at t0 + 300; ally_corp_2 expires at t0 + 3_600.
    handle_wars(
        &ctx,
        &mut agg_sess,
        "warRegistry",
        "GMExtendAllyContract",
        &[
            PyRep::Integer(war_id),
            PyRep::Integer(ally_corp),
            PyRep::Integer(300),
        ],
        None,
    )
    .unwrap()
    .unwrap();
    handle_wars(
        &ctx,
        &mut agg_sess,
        "warRegistry",
        "GMExtendAllyContract",
        &[
            PyRep::Integer(war_id),
            PyRep::Integer(ally_corp_2),
            PyRep::Integer(3_600),
        ],
        None,
    )
    .unwrap()
    .unwrap();

    clock.advance_secs_and_read_offset(300);
    let now = ctx.now_unix();
    ctx.db.with_snapshot(|snap| {
        assert!(!is_war_participant_corp(snap, war_id, ally_corp, now));
        assert!(is_war_participant_corp(snap, war_id, ally_corp_2, now));
    });

    // GMActivateDefender(war_id) without allyID clears expired ally_corp expiry while keeping unexpired ally_corp_2 and other wars intact.
    assert_eq!(
        handle_wars(
            &ctx,
            &mut agg_sess,
            "warRegistry",
            "GMActivateDefender",
            &[PyRep::Integer(war_id)],
            None,
        )
        .unwrap()
        .unwrap(),
        PyRep::Bool(true)
    );
    ctx.db.with_snapshot(|snap| {
        assert!(!snap
            .war_state
            .ally_expires_unix
            .contains_key(&(war_id, ally_corp)));
        assert_eq!(
            snap.war_state.ally_expires_unix.get(&(war_id, ally_corp_2)),
            Some(&(t0 + 3_600))
        );
        assert_eq!(
            snap.war_state
                .ally_expires_unix
                .get(&(other_war_id, ally_corp)),
            Some(&t0)
        );
        assert!(is_war_participant_corp(snap, war_id, ally_corp, now));
        assert!(is_war_participant_corp(snap, war_id, ally_corp_2, now));
    });
}

#[test]
fn surrender_negotiation_rejects_non_executor_sister_corps_and_same_side_self_accept() {
    use eve_db::{clock::VirtualServerClock, AllianceRecord, PersistedWarMeta};
    use std::sync::Arc;

    let (ctx, mut agg_sess, agg_corp, mut def_sess, def_corp, _ally_sess, _ally_corp, _hq_id) =
        setup_wars_ctx();
    let t0 = 1_700_000_000_u64;
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(t0)));

    let agg_alliance = 99_940_001_i64;
    let def_alliance = 99_940_002_i64;
    let agg_sister_corp = 98_940_011_i64;
    let agg_sister_char = 90_940_011_i64;
    let def_sister_corp = 98_940_022_i64;
    let def_sister_char = 90_940_022_i64;
    let war_corp_vs_corp = 700_940_001_i64;
    let war_alliance_vs_alliance = 700_940_002_i64;

    let mut agg_sister_sess = agg_sess.clone();
    agg_sister_sess.character_id = Some(agg_sister_char);
    agg_sister_sess.corporation_id = Some(agg_sister_corp);
    agg_sister_sess.alliance_id = Some(agg_alliance);

    let mut def_sister_sess = def_sess.clone();
    def_sister_sess.character_id = Some(def_sister_char);
    def_sister_sess.corporation_id = Some(def_sister_corp);
    def_sister_sess.alliance_id = Some(def_alliance);

    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.get_mut(&agg_corp).unwrap().alliance_id = Some(agg_alliance);
        snap.corporations.get_mut(&def_corp).unwrap().alliance_id = Some(def_alliance);
        let base_ch = snap.characters[&agg_sess.character_id.unwrap()].clone();
        for (cid, uid, corp_id, ally_id, ticker) in [
            (agg_sister_char, 941, agg_sister_corp, agg_alliance, "ASIS"),
            (def_sister_char, 942, def_sister_corp, def_alliance, "DSIS"),
        ] {
            let mut ch = base_ch.clone();
            ch.character_id = cid;
            ch.user_id = uid;
            ch.corporation_id = corp_id;
            ch.alliance_id = Some(ally_id);
            snap.characters.insert(cid, ch);

            let mut corp = CorporationRecord::default();
            corp.corporation_id = corp_id;
            corp.name = format!("Sister {ticker}");
            corp.ticker = ticker.into();
            corp.ceo_id = cid;
            corp.alliance_id = Some(ally_id);
            corp.members = [cid].into_iter().collect();
            corp.member_roles = [(cid, u64::MAX)].into_iter().collect();
            corp.set_division_balance(1, IskCents::from_isk(50_000_000));
            snap.corporations.insert(corp_id, corp);
        }

        snap.alliances.insert(
            agg_alliance,
            AllianceRecord {
                alliance_id: agg_alliance,
                name: "Agg Alliance".into(),
                short_name: "AGGA".into(),
                executor_corp_id: agg_corp,
                member_corp_ids: [agg_corp, agg_sister_corp].into_iter().collect(),
            },
        );
        snap.alliances.insert(
            def_alliance,
            AllianceRecord {
                alliance_id: def_alliance,
                name: "Def Alliance".into(),
                short_name: "DEFA".into(),
                executor_corp_id: def_corp,
                member_corp_ids: [def_corp, def_sister_corp].into_iter().collect(),
            },
        );

        // 1. Corp-vs-corp war where defender belongs to def_alliance alongside def_sister_corp.
        snap.wars.insert(
            war_corp_vs_corp,
            WarRecord {
                war_id: war_corp_vs_corp,
                aggressor_corp_id: agg_corp,
                defender_corp_id: def_corp,
                mutual: false,
                started_unix: t0.saturating_sub(WAR_TIMER_SECS),
                active: true,
            },
        );
        snap.war_state.wars.insert(
            war_corp_vs_corp,
            PersistedWarMeta {
                open_for_allies: true,
                ..Default::default()
            },
        );

        // 2. Alliance-vs-alliance war between agg_alliance (executor agg_corp) and def_alliance (executor def_corp).
        snap.wars.insert(
            war_alliance_vs_alliance,
            WarRecord {
                war_id: war_alliance_vs_alliance,
                aggressor_corp_id: agg_alliance,
                defender_corp_id: def_alliance,
                mutual: true,
                started_unix: t0.saturating_sub(WAR_TIMER_SECS),
                active: true,
            },
        );
        snap.war_state
            .wars
            .insert(war_alliance_vs_alliance, PersistedWarMeta::default());
    });

    // A. In corp-vs-corp war, defender's sister corp cannot ally its own war and cannot create surrender offers.
    let snap_before_sister = ctx.db.snapshot_tables();
    assert_eq!(
        handle_wars(
            &ctx,
            &mut def_sister_sess,
            "warsInfoMgr",
            "CreateWarAllyOffer",
            &[PyRep::Integer(war_corp_vs_corp), PyRep::Real(0.0)],
            None,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "CannotAllyOwnWar"
    );
    assert_eq!(
        handle_wars(
            &ctx,
            &mut def_sister_sess,
            "warsInfoMgr",
            "CreateSurrenderNegotiation",
            &[PyRep::Integer(war_corp_vs_corp), PyRep::Real(0.0)],
            None,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "NotWarParticipant"
    );
    assert_eq!(ctx.db.snapshot_tables(), snap_before_sister);

    // Even if a same-side sister-corp surrender row were injected into state, neither defender nor aggressor may accept it.
    let injected_id = ctx.db.with_mut_snapshot(|snap| {
        snap.war_state.next_negotiation_id += 1;
        let id = snap.war_state.next_negotiation_id;
        snap.war_state.negotiations.push(Negotiation {
            id,
            war_id: war_corp_vs_corp,
            kind: NegKind::Surrender,
            from_corp: def_sister_corp,
            isk: IskCents::ZERO,
            message: "same-side exploit".into(),
            status: "Offered".into(),
            effective_unix: None,
        });
        id
    });
    for sess in [&mut def_sess, &mut agg_sess] {
        assert_eq!(
            handle_wars(
                &ctx,
                sess,
                "warsInfoMgr",
                "AcceptSurrender",
                &[PyRep::Integer(injected_id)],
                None,
            )
            .unwrap()
            .unwrap_err()
            .code,
            "NotNegotiationCounterparty"
        );
    }

    // B. In alliance-vs-alliance war, non-executor sister corps cannot submit surrender offers.
    for sess in [&mut agg_sister_sess, &mut def_sister_sess] {
        assert_eq!(
            handle_wars(
                &ctx,
                sess,
                "warsInfoMgr",
                "CreateSurrenderNegotiation",
                &[PyRep::Integer(war_alliance_vs_alliance), PyRep::Real(0.0)],
                None,
            )
            .unwrap()
            .unwrap_err()
            .code,
            "NotWarParticipant"
        );
    }

    // Aggressor alliance executor submits surrender offer; only opposing defender alliance executor may decline/accept.
    let s1 = handle_wars(
        &ctx,
        &mut agg_sess,
        "warsInfoMgr",
        "CreateSurrenderNegotiation",
        &[
            PyRep::Integer(war_alliance_vs_alliance),
            PyRep::Real(1_000_000.0),
            PyRep::String("truce?".into()),
        ],
        None,
    )
    .unwrap()
    .unwrap();
    let s1_id = dict_i64(&s1, "negotiationID").unwrap();
    for sess in [&mut agg_sess, &mut agg_sister_sess, &mut def_sister_sess] {
        assert_eq!(
            handle_wars(
                &ctx,
                sess,
                "warsInfoMgr",
                "AcceptSurrender",
                &[PyRep::Integer(s1_id)],
                None,
            )
            .unwrap()
            .unwrap_err()
            .code,
            "NotNegotiationCounterparty"
        );
    }
    let decl_s1 = handle_wars(
        &ctx,
        &mut def_sess,
        "warsInfoMgr",
        "DeclineSurrender",
        &[PyRep::Integer(s1_id)],
        None,
    )
    .unwrap()
    .unwrap();
    assert_eq!(dict_str(&decl_s1, "status"), Some("Declined"));

    // Defender alliance executor submits surrender offer; opposing aggressor alliance executor accepts cleanly.
    let s2 = handle_wars(
        &ctx,
        &mut def_sess,
        "warsInfoMgr",
        "CreateSurrenderNegotiation",
        &[
            PyRep::Integer(war_alliance_vs_alliance),
            PyRep::Real(2_000_000.0),
            PyRep::String("we surrender".into()),
        ],
        None,
    )
    .unwrap()
    .unwrap();
    let s2_id = dict_i64(&s2, "negotiationID").unwrap();
    assert_eq!(
        handle_wars(
            &ctx,
            &mut def_sess,
            "warsInfoMgr",
            "AcceptSurrender",
            &[PyRep::Integer(s2_id)],
            None,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "NotNegotiationCounterparty"
    );
    let acc_s2 = handle_wars(
        &ctx,
        &mut agg_sess,
        "warsInfoMgr",
        "AcceptSurrender",
        &[PyRep::Integer(s2_id)],
        None,
    )
    .unwrap()
    .unwrap();
    assert_eq!(dict_str(&acc_s2, "status"), Some("Accepted"));
    assert_eq!(
        dict_i64(&acc_s2, "timeFinished"),
        Some(crate::domain::wars::unix_to_filetime(t0 + WAR_TIMER_SECS))
    );
}

#[test]
fn war_timestamps_use_windows_filetime_for_all_war_ally_neg_invite_and_treaty_fields() {
    use crate::domain::wars::{unix_to_filetime, PEACE_TREATY_SECS, WAR_TIMER_SECS};
    use eve_db::clock::VirtualServerClock;
    use eve_machonet::service_roles::ROLE_GMH;
    use std::sync::Arc;

    let (ctx, mut agg_sess, agg_corp, mut def_sess, def_corp, mut ally_sess, ally_corp, agg_hq_id) =
        setup_wars_ctx();
    let t0 = 1_750_000_000_u64;
    let clock = Arc::new(VirtualServerClock::with_fixed_base(t0));
    ctx.set_clock(clock.clone());

    let agg_char = agg_sess.character_id.unwrap();
    let agg_uid = ctx.db.with_snapshot(|snap| snap.characters[&agg_char].user_id);
    ctx.db
        .acquire_online_lock(agg_uid, 777, Some(agg_char), true, t0)
        .unwrap();
    let _ = ctx.drain_notifications_for_char(agg_char);

    // 1. DeclareWarAgainst + warRegistry.{timeDeclared,timeStarted,timeFinished} + GetWars / GetWarsByOwnerID / GetPublicWarInfo
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
    let ft_t0 = unix_to_filetime(t0);
    let ft_t0_start = unix_to_filetime(t0 + WAR_TIMER_SECS);
    assert_eq!(dict_i64(&declared, "timeDeclared"), Some(ft_t0));
    assert_eq!(dict_i64(&declared, "timeStarted"), Some(ft_t0_start));

    assert_eq!(
        handle_wars(&ctx, &mut agg_sess, "warRegistry", "timeDeclared", &[PyRep::Integer(war_id)], None)
            .unwrap()
            .unwrap(),
        PyRep::Integer(ft_t0)
    );
    assert_eq!(
        handle_wars(&ctx, &mut agg_sess, "warRegistry", "timeStarted", &[PyRep::Integer(war_id)], None)
            .unwrap()
            .unwrap(),
        PyRep::Integer(ft_t0_start)
    );
    assert_eq!(
        handle_wars(&ctx, &mut agg_sess, "warRegistry", "timeFinished", &[PyRep::Integer(war_id)], None)
            .unwrap()
            .unwrap(),
        PyRep::None
    );

    let PyRep::Dict(wars_map) = handle_wars(&ctx, &mut agg_sess, "warRegistry", "GetWars", &[], None)
        .unwrap()
        .unwrap()
    else {
        panic!("expected Dict from GetWars");
    };
    let war_kv = wars_map
        .iter()
        .find_map(|(k, v)| (k == &PyRep::Integer(war_id)).then_some(v))
        .expect("war in GetWars");
    assert_eq!(dict_i64(war_kv, "timeDeclared"), Some(ft_t0));
    assert_eq!(dict_i64(war_kv, "timeStarted"), Some(ft_t0_start));
    assert_eq!(war_kv.dict_get("timeFinished"), Some(&PyRep::None));

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
    let crow_rows = by_owner.crowset_rows().expect("CRowset rows");
    assert_eq!(crow_rows.len(), 1);
    assert_eq!(crow_rows[0].dbrow_get("timeDeclared"), Some(&PyRep::Integer(ft_t0)));
    assert_eq!(crow_rows[0].dbrow_get("timeStarted"), Some(&PyRep::Integer(ft_t0_start)));
    assert_eq!(crow_rows[0].dbrow_get("timeFinished"), Some(&PyRep::None));

    // 2. GMSetDeclareTime / GMSetWarFinishTime emit FILETIME OnWarChanged and update GetPublicWarInfo.
    let _ = ctx.drain_notifications_for_char(agg_char);
    agg_sess.role |= ROLE_GMH;
    let new_decl = t0 + 1_200;
    let ft_new_decl = unix_to_filetime(new_decl);
    let ft_new_start = unix_to_filetime(new_decl + WAR_TIMER_SECS);
    assert_eq!(
        handle_wars(
            &ctx,
            &mut agg_sess,
            "warRegistry",
            "GMSetDeclareTime",
            &[PyRep::Integer(war_id), PyRep::Integer(new_decl as i64)],
            None,
        )
        .unwrap()
        .unwrap(),
        PyRep::Bool(true)
    );
    let decl_notif = ctx
        .drain_notifications_for_char(agg_char)
        .into_iter()
        .find(|n| n.event == "OnWarChanged")
        .expect("GMSetDeclareTime must emit OnWarChanged");
    assert_eq!(dict_i64(&decl_notif.args[0], "timeDeclared"), Some(ft_new_decl));
    assert_eq!(dict_i64(&decl_notif.args[0], "timeStarted"), Some(ft_new_start));
    assert_eq!(
        decl_notif.args[2].dict_get("timeDeclared"),
        Some(&PyRep::Tuple(vec![PyRep::Integer(ft_t0), PyRep::Integer(ft_new_decl)]))
    );
    assert_eq!(
        decl_notif.args[2].dict_get("timeStarted"),
        Some(&PyRep::Tuple(vec![PyRep::Integer(ft_t0_start), PyRep::Integer(ft_new_start)]))
    );

    let fin_unix = t0 + 20_000;
    let ft_fin = unix_to_filetime(fin_unix);
    assert_eq!(
        handle_wars(
            &ctx,
            &mut agg_sess,
            "warRegistry",
            "GMSetWarFinishTime",
            &[PyRep::Integer(war_id), PyRep::Integer(fin_unix as i64)],
            None,
        )
        .unwrap()
        .unwrap(),
        PyRep::Bool(true)
    );
    let fin_notif = ctx
        .drain_notifications_for_char(agg_char)
        .into_iter()
        .find(|n| n.event == "OnWarChanged")
        .expect("GMSetWarFinishTime must emit OnWarChanged");
    assert_eq!(dict_i64(&fin_notif.args[0], "timeFinished"), Some(ft_fin));
    assert_eq!(
        fin_notif.args[2].dict_get("timeFinished"),
        Some(&PyRep::Tuple(vec![PyRep::None, PyRep::Integer(ft_fin)]))
    );
    let PyRep::List(pub_items) = handle_wars(
        &ctx,
        &mut agg_sess,
        "warsInfoMgr",
        "GetPublicWarInfo",
        &[PyRep::Integer(war_id)],
        None,
    )
    .unwrap()
    .unwrap()
    else {
        panic!("expected List from GetPublicWarInfo");
    };
    assert_eq!(dict_i64(&pub_items[0], "timeDeclared"), Some(ft_new_decl));
    assert_eq!(dict_i64(&pub_items[0], "timeStarted"), Some(ft_new_start));
    assert_eq!(dict_i64(&pub_items[0], "timeFinished"), Some(ft_fin));
    assert_eq!(
        handle_wars(
            &ctx,
            &mut agg_sess,
            "warRegistry",
            "GMSetWarFinishTime",
            &[PyRep::Integer(war_id), PyRep::None],
            None,
        )
        .unwrap()
        .unwrap(),
        PyRep::Bool(true)
    );

    // 3. GetNegotiations timeEffective (None while Offered, FILETIME once Accepted) & expired-ally query exclusion.
    assert_eq!(
        dict_bool(
            &handle_wars(
                &ctx,
                &mut def_sess,
                "warsInfoMgr",
                "SetOpenForAllies",
                &[PyRep::Integer(war_id), PyRep::Bool(true)],
                None,
            )
            .unwrap()
            .unwrap(),
            "openForAllies"
        ),
        Some(true)
    );
    let offer = handle_wars(
        &ctx,
        &mut ally_sess,
        "warsInfoMgr",
        "CreateWarAllyOffer",
        &[PyRep::Integer(war_id), PyRep::Real(1_000_000.0), PyRep::String("Help".into())],
        None,
    )
    .unwrap()
    .unwrap();
    let neg_id = dict_i64(&offer, "negotiationID").unwrap();
    let PyRep::Rowset {
        columns: neg_cols_pre,
        rows: neg_rows_pre,
    } = handle_wars(&ctx, &mut def_sess, "warRegistry", "GetNegotiations", &[PyRep::Integer(war_id)], None)
        .unwrap()
        .unwrap()
    else {
        panic!("expected Rowset from GetNegotiations");
    };
    let eff_idx = neg_cols_pre.iter().position(|c| c == "timeEffective").expect("timeEffective col");
    assert_eq!(neg_rows_pre.len(), 1);
    assert_eq!(neg_rows_pre[0][eff_idx], PyRep::None);

    let ally_acc = handle_wars(
        &ctx,
        &mut def_sess,
        "warsInfoMgr",
        "AcceptAllyNegotiation",
        &[PyRep::Integer(neg_id)],
        None,
    )
    .unwrap()
    .unwrap();
    let ally_eff = t0 + WAR_TIMER_SECS;
    let ft_ally_eff = unix_to_filetime(ally_eff);
    assert_eq!(dict_i64(&ally_acc, "timeStarted"), Some(ft_ally_eff));

    let PyRep::Rowset {
        columns: neg_cols_post,
        rows: neg_rows_post,
    } = handle_wars(&ctx, &mut def_sess, "warRegistry", "GetNegotiations", &[PyRep::Integer(war_id)], None)
        .unwrap()
        .unwrap()
    else {
        panic!("expected Rowset from GetNegotiations");
    };
    let eff_idx_post = neg_cols_post.iter().position(|c| c == "timeEffective").expect("timeEffective col");
    assert_eq!(neg_rows_post[0][eff_idx_post], PyRep::Integer(ft_ally_eff));

    assert_eq!(
        handle_wars(
            &ctx,
            &mut agg_sess,
            "warRegistry",
            "GMExtendAllyContract",
            &[PyRep::Integer(war_id), PyRep::Integer(ally_corp), PyRep::Integer(5_400)],
            None,
        )
        .unwrap()
        .unwrap(),
        PyRep::Bool(true)
    );
    let ally_exp = ally_eff + 5_400;
    let ft_ally_exp = unix_to_filetime(ally_exp);

    clock.advance_secs_and_read_offset(WAR_TIMER_SECS);
    assert_eq!(ctx.now_unix(), ally_eff);
    let PyRep::Tuple(base_active) = handle_wars(
        &ctx,
        &mut agg_sess,
        "warStatisticMgr",
        "GetBaseInfo",
        &[PyRep::Integer(war_id)],
        Some(war_id),
    )
    .unwrap()
    .unwrap()
    else {
        panic!("expected 4-tuple from GetBaseInfo");
    };
    assert_eq!(base_active[0].dict_get("retracted"), Some(&PyRep::None));
    let Some(PyRep::Dict(active_allies)) = base_active[0].dict_get("allies") else {
        panic!("expected allies dict");
    };
    assert_eq!(active_allies.len(), 1);
    assert_eq!(dict_i64(&active_allies[0].1, "timeStarted"), Some(ft_ally_eff));
    assert_eq!(dict_i64(&active_allies[0].1, "timeFinished"), Some(ft_ally_exp));

    clock.advance_secs_and_read_offset(5_400);
    assert_eq!(ctx.now_unix(), ally_exp);
    let PyRep::Tuple(base_expired) = handle_wars(
        &ctx,
        &mut agg_sess,
        "warStatisticMgr",
        "GetBaseInfo",
        &[PyRep::Integer(war_id)],
        Some(war_id),
    )
    .unwrap()
    .unwrap()
    else {
        panic!("expected 4-tuple from GetBaseInfo");
    };
    let Some(PyRep::Dict(expired_allies)) = base_expired[0].dict_get("allies") else {
        panic!("expected allies dict");
    };
    assert!(expired_allies.is_empty(), "expired ally must be excluded from GetBaseInfo.allies");
    let PyRep::Rowset { rows: ally_rows_exp, .. } = handle_wars(
        &ctx,
        &mut def_sess,
        "warRegistry",
        "GetAllyRowsByWarID",
        &[PyRep::Integer(war_id)],
        None,
    )
    .unwrap()
    .unwrap()
    else {
        panic!("expected Rowset from GetAllyRowsByWarID");
    };
    assert_eq!(ally_rows_exp[0][1], PyRep::Integer(ft_ally_eff));
    assert_eq!(ally_rows_exp[0][3], PyRep::Bool(false));

    // Verify `warStatisticMgr.GetKills` emits Windows FILETIME on `"killTime"`.
    let def_char = def_sess.character_id.unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        snap.killmails.insert(
            888_777,
            eve_db::KillmailRecord {
                kill_id: 888_777,
                victim_character_id: def_char,
                attacker_character_id: agg_char,
                solar_system_id: 30_000_142,
                ship_type_id: 587,
                total_value_isk: 1_234_567.0,
                killed_unix: ally_eff,
                victim_corp_id: def_corp,
                victim_alliance_id: None,
                attacker_corp_id: agg_corp,
                attacker_alliance_id: None,
            },
        );
    });
    let PyRep::Rowset {
        columns: kill_cols,
        rows: kill_rows,
    } = handle_wars(
        &ctx,
        &mut agg_sess,
        "warStatisticMgr",
        "GetKills",
        &[PyRep::Integer(war_id)],
        Some(war_id),
    )
    .unwrap()
    .unwrap()
    else {
        panic!("expected Rowset from GetKills");
    };
    let kt_idx = kill_cols.iter().position(|c| c == "killTime").expect("killTime col");
    assert_eq!(kill_rows.len(), 1);
    assert_eq!(kill_rows[0][kt_idx], PyRep::Integer(ft_ally_eff));

    // 4. Surrender + GetPeaceTreatiesForSession expiryDate + Mutual invite sentDate & RetractMutualWar retracted FILETIME.
    let surr = handle_wars(
        &ctx,
        &mut def_sess,
        "warsInfoMgr",
        "CreateSurrenderNegotiation",
        &[PyRep::Integer(war_id), PyRep::Real(1_000_000.0), PyRep::String("Peace".into())],
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
    let surr_fin = ally_exp + WAR_TIMER_SECS;
    let treaty_exp = surr_fin + PEACE_TREATY_SECS;
    assert_eq!(dict_i64(&acc_surr, "timeFinished"), Some(unix_to_filetime(surr_fin)));
    assert_eq!(dict_i64(&acc_surr, "peaceTreatyExpiry"), Some(unix_to_filetime(treaty_exp)));

    let PyRep::Tuple(treaties) = handle_wars(
        &ctx,
        &mut agg_sess,
        "peaceTreatyMgr",
        "GetPeaceTreatiesForSession",
        &[],
        None,
    )
    .unwrap()
    .unwrap()
    else {
        panic!("expected Tuple from GetPeaceTreatiesForSession");
    };
    let PyRep::List(ref outgoing_treaties) = treaties[0] else {
        panic!("expected outgoing treaties List");
    };
    assert_eq!(
        dict_i64(&outgoing_treaties[0], "expiryDate"),
        Some(unix_to_filetime(treaty_exp))
    );

    let sent_invite = handle_wars(
        &ctx,
        &mut agg_sess,
        "mutualWarInviteMgr",
        "SendInviteByPlayer",
        &[PyRep::Integer(ally_corp)],
        None,
    )
    .unwrap()
    .unwrap();
    let ft_invite = unix_to_filetime(ally_exp);
    assert_eq!(dict_i64(&sent_invite, "sentDate"), Some(ft_invite));
    let PyRep::Rowset { rows: pend_rows, .. } = handle_wars(
        &ctx,
        &mut ally_sess,
        "mutualWarInviteMgr",
        "GetPendingInvitesForSession",
        &[],
        None,
    )
    .unwrap()
    .unwrap()
    else {
        panic!("expected Rowset from GetPendingInvitesForSession");
    };
    assert_eq!(pend_rows, vec![vec![PyRep::Integer(agg_corp), PyRep::Integer(ally_corp), PyRep::Integer(ft_invite)]]);

    let mresp = handle_wars(
        &ctx,
        &mut ally_sess,
        "mutualWarInviteMgr",
        "RespondToInviteByPlayer",
        &[PyRep::Integer(agg_corp), PyRep::Bool(true)],
        None,
    )
    .unwrap()
    .unwrap();
    let mwar_id = dict_i64(&mresp, "warID").unwrap();
    let retracted = handle_wars(
        &ctx,
        &mut agg_sess,
        "warsInfoMgr",
        "RetractMutualWar",
        &[PyRep::Integer(mwar_id)],
        None,
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        dict_i64(&retracted, "timeFinished"),
        Some(unix_to_filetime(ally_exp + WAR_TIMER_SECS))
    );
    let PyRep::Tuple(retracted_base) = handle_wars(
        &ctx,
        &mut agg_sess,
        "warStatisticMgr",
        "GetBaseInfo",
        &[PyRep::Integer(mwar_id)],
        Some(mwar_id),
    )
    .unwrap()
    .unwrap()
    else {
        panic!("expected 4-tuple from GetBaseInfo for retracted mutual war");
    };
    assert_eq!(dict_i64(&retracted_base[0], "retracted"), Some(ft_invite));
}
