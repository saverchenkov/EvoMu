use super::*;
use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::make_fighter_ctx_and_session;
use eve_db::{CorporationRecord, IskCents, KillmailRecord, UpwellStructureRecord};

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
