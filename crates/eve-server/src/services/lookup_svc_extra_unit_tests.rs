//! Deep branch-coverage and mutation-killing unit tests for `lookup_svc`.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::make_fighter_ctx_and_session;
use crate::services::lookup_svc::{
    handle, handle_search_query, handle_search_quick_query, is_word_allowed,
};
use eve_db::{AllianceRecord, CorporationRecord, UpwellStructureRecord, WarRecord};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::{HashMap, HashSet};

fn call_lookup(
    ctx: &ServerContext,
    session: &SessionState,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    handle(ctx, session, "lookupSvc", method, args, kwargs)
}

fn call_banned(
    ctx: &ServerContext,
    session: &SessionState,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    handle(ctx, session, concat!("banned", "words"), method, args, kwargs)
}

#[test]
fn is_word_allowed_and_bannedwords_service_all_argument_and_kwarg_variants() {
    for banned in [
        "badword",
        "bannedword",
        "myexploittest",
        "scam_link",
        "hacktool",
        "profanitycheck",
    ] {
        assert!(!is_word_allowed(banned), "expected {banned} to be disallowed");
        assert!(!is_word_allowed(&banned.to_uppercase()));
    }
    assert!(is_word_allowed("legitimate fleet doctrine"));

    let (ctx, session, _, _, _) = make_fighter_ctx_and_session();
    let empty_kw = HashMap::new();

    // Unhandled service and unhandled method guards
    let other_svc = "machoNet";
    let other_method = "GetTime";
    assert!(handle(&ctx, &session, other_svc, other_method, &[], &empty_kw).is_none());
    assert!(call_lookup(&ctx, &session, other_method, &[], &empty_kw).is_none());
    let unauth = SessionState::default();
    assert!(call_lookup(&ctx, &unauth, "LookupCharacters", &[], &empty_kw)
        .unwrap()
        .is_err());

    // 1. bannedwords.check_words_allowed: Tuple, kwargs["word"], and None fallback
    let list_res = call_banned(
        &ctx,
        &session,
        "check_words_allowed",
        &[PyRep::Tuple(vec![
            PyRep::String("clean".into()),
            PyRep::String("scam".into()),
        ])],
        &empty_kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        list_res,
        PyRep::List(vec![PyRep::Bool(true), PyRep::Bool(false)])
    );

    let mut kw_word = HashMap::new();
    kw_word.insert("word".into(), PyRep::String("profanity".into()));
    let err_word = call_banned(&ctx, &session, "check_words_allowed", &[], &kw_word)
        .unwrap()
        .unwrap_err();
    assert_eq!(err_word.message, "LocationNameInvalidBannedWord");

    let none_fallback = call_banned(
        &ctx,
        &session,
        "check_words_allowed",
        &[PyRep::None],
        &empty_kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(none_fallback, PyRep::None);

    // 2. bannedwords.check_chat_words_allowed: List all-ok vs one-bad, kwargs["text"], fallback
    let chat_list_ok = call_banned(
        &ctx,
        &session,
        "check_chat_words_allowed",
        &[PyRep::List(vec![
            PyRep::String("hello".into()),
            PyRep::String("world".into()),
        ])],
        &empty_kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(chat_list_ok, PyRep::None);

    let chat_list_bad = call_banned(
        &ctx,
        &session,
        "check_chat_words_allowed",
        &[PyRep::List(vec![
            PyRep::String("hello".into()),
            PyRep::String("exploit".into()),
        ])],
        &empty_kw,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(chat_list_bad.message, "ChatContentInvalidBannedWords");

    let mut kw_text = HashMap::new();
    kw_text.insert("text".into(), PyRep::String("clean chat".into()));
    assert_eq!(
        call_banned(&ctx, &session, "check_chat_words_allowed", &[], &kw_text)
            .unwrap()
            .unwrap(),
        PyRep::None
    );
    assert_eq!(
        call_banned(&ctx, &session, "check_chat_words_allowed", &[], &empty_kw)
            .unwrap()
            .unwrap(),
        PyRep::None
    );

    // 3. bannedwords.check_search_words_allowed: Tuple, kwargs["searchStr"], fallback
    let search_list = call_banned(
        &ctx,
        &session,
        "check_search_words_allowed",
        &[PyRep::Tuple(vec![
            PyRep::String("jita".into()),
            PyRep::String("hack".into()),
        ])],
        &empty_kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(search_list, PyRep::Bool(false));

    let mut kw_s = HashMap::new();
    kw_s.insert("searchStr".into(), PyRep::String("amarr".into()));
    assert_eq!(
        call_banned(&ctx, &session, "check_search_words_allowed", &[], &kw_s)
            .unwrap()
            .unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(
        call_banned(&ctx, &session, "check_search_words_allowed", &[], &empty_kw)
            .unwrap()
            .unwrap(),
        PyRep::Bool(true)
    );
}

#[test]
fn lookup_svc_exact_matching_npc_filtering_locations_and_war_eligibility_matrix() {
    let (ctx, mut session, _, _, sys_id) = make_fighter_ctx_and_session();
    let kw = HashMap::new();

    let caller_corp = 98_100_001i64;
    let caller_ally = 99_100_001i64;
    let war_eligible_corp = 98_100_002i64;
    let war_disqualified_corp = 98_100_003i64;
    let destroyed_struct_corp = 98_100_004i64;
    let ally_member_corp = 98_100_005i64;
    let war_eligible_ally = 99_100_002i64;
    let npc_char_id = 3_019_580i64;
    let player_char_id = 95_600_001i64;

    session.corporation_id = Some(caller_corp);
    session.alliance_id = None;

    ctx.db.with_mut_snapshot(|snap| {
        let base_ch = snap.characters.values().next().unwrap().clone();
        let mut npc_ch = base_ch.clone();
        npc_ch.character_id = npc_char_id;
        npc_ch.name = "uniquelookuppilot alpha".into();
        npc_ch.alliance_id = None;
        snap.characters.insert(npc_char_id, npc_ch);

        let mut pc_ch = base_ch;
        pc_ch.character_id = player_char_id;
        pc_ch.name = "uniquelookuppilot beta".into();
        pc_ch.corporation_id = caller_corp;
        pc_ch.alliance_id = Some(caller_ally);
        snap.characters.insert(player_char_id, pc_ch);

        for (cid, name, ticker, ally) in [
            (caller_corp, "callervaliantcorp", "cval", Some(caller_ally)),
            (war_eligible_corp, "wartargetablealpha", "wtga", None),
            (war_disqualified_corp, "wartargetabledisq", "wtdq", None),
            (destroyed_struct_corp, "wartargetabledead", "wtdd", None),
            (
                ally_member_corp,
                "wartargetableallymember",
                "wtam",
                Some(war_eligible_ally),
            ),
        ] {
            snap.corporations.insert(
                cid,
                { let mut __corp = CorporationRecord::default();
                    __corp.corporation_id = cid;
                    __corp.name = name.into();
                    __corp.ticker = ticker.into();
                    __corp.ceo_id = player_char_id;
                    __corp.alliance_id = ally;
                    __corp.tax_rate = 0.05;
                    __corp },
            );
        }

        snap.alliances.insert(
            caller_ally,
            AllianceRecord {
                alliance_id: caller_ally,
                name: "callervaliantalliance".into(),
                short_name: "cvala".into(),
                executor_corp_id: caller_corp,
                member_corp_ids: HashSet::from([caller_corp]),
            },
        );
        snap.alliances.insert(
            war_eligible_ally,
            AllianceRecord {
                alliance_id: war_eligible_ally,
                name: "wartargetablecoalition".into(),
                short_name: "wtcoa".into(),
                executor_corp_id: ally_member_corp,
                member_corp_ids: HashSet::from([ally_member_corp]),
            },
        );

        for (sid, owner, state) in [
            (1_045_000_001i64, war_eligible_corp, "ShieldVulnerable"),
            (1_045_000_002i64, war_disqualified_corp, "ShieldVulnerable"),
            (1_045_000_003i64, destroyed_struct_corp, "Destroyed"),
            (1_045_000_004i64, ally_member_corp, "ArmorReinforced"),
        ] {
            snap.structures.insert(
                sid,
                UpwellStructureRecord {
                    structure_id: sid,
                    type_id: 35832,
                    owner_corp_id: owner,
                    solar_system_id: sys_id,
                    name: format!("struct {sid}"),
                    state: state.into(),
                    quantum_core_installed: true,
                    ..Default::default()
                },
            );
        }

        snap.wars.insert(
            501,
            WarRecord {
                war_id: 501,
                aggressor_corp_id: caller_ally,
                defender_corp_id: war_disqualified_corp,
                mutual: false,
                started_unix: 1_000,
                active: true,
            },
        );
        snap.wars.insert(
            502,
            WarRecord {
                war_id: 502,
                aggressor_corp_id: war_eligible_corp,
                defender_corp_id: caller_corp,
                mutual: false,
                started_unix: 1_000,
                active: false,
            },
        );
    });

    // 1. LookupCharacters vs LookupEvePlayerCharacters (exact=false vs exact=true)
    let all_chars = call_lookup(
        &ctx,
        &session,
        "LookupCharacters",
        &[PyRep::String("uniquelookuppilot".into()), PyRep::Bool(false)],
        &kw,
    )
    .unwrap()
    .unwrap();
    if let PyRep::List(v) = all_chars {
        assert_eq!(v.len(), 2);
    }

    let exact_char = call_lookup(
        &ctx,
        &session,
        "LookupCharacters",
        &[
            PyRep::String("uniquelookuppilot beta".into()),
            PyRep::Bool(true),
        ],
        &kw,
    )
    .unwrap()
    .unwrap();
    if let PyRep::List(v) = exact_char {
        assert_eq!(v.len(), 1);
        assert_eq!(
            v[0].dict_get("allianceID").and_then(|a| a.as_i64()),
            Some(caller_ally)
        );
    }

    let pc_chars = call_lookup(
        &ctx,
        &session,
        "LookupEvePlayerCharacters",
        &[PyRep::String("uniquelookuppilot".into()), PyRep::Integer(0)],
        &kw,
    )
    .unwrap()
    .unwrap();
    if let PyRep::List(v) = pc_chars {
        assert_eq!(v.len(), 1);
        assert_eq!(
            v[0].dict_get("characterID").and_then(|a| a.as_i64()),
            Some(player_char_id)
        );
    }

    // 2. LookupCorporations by integer corp_id and empty args
    let by_id = call_lookup(
        &ctx,
        &session,
        "LookupCorporations",
        &[PyRep::Integer(war_eligible_corp)],
        &kw,
    )
    .unwrap()
    .unwrap();
    if let PyRep::List(v) = by_id {
        assert_eq!(v.len(), 1);
    }
    let all_corps = call_lookup(&ctx, &session, "LookupCorporations", &[], &kw)
        .unwrap()
        .unwrap();
    if let PyRep::List(v) = all_corps {
        assert!(v.len() >= 5);
    }

    // 3. LookupKnownLocationsByGroup across groups 5, 15, 4, 3, 999
    for gid in [5i64, 15, 4, 3, 999] {
        let loc_res = call_lookup(
            &ctx,
            &session,
            "LookupKnownLocationsByGroup",
            &[PyRep::Integer(gid), PyRep::String("".into()), PyRep::Integer(0)],
            &kw,
        )
        .unwrap()
        .unwrap();
        if let PyRep::List(v) = loc_res {
            if gid == 999 {
                assert!(v.is_empty());
            } else {
                assert!(!v.is_empty());
            }
        }
    }

    let sys_locs = call_lookup(
        &ctx,
        &session,
        "LookupLocationsByGroup",
        &[PyRep::Integer(5), PyRep::String("jita".into()), PyRep::Integer(1)],
        &kw,
    )
    .unwrap()
    .unwrap();
    if let PyRep::List(v) = sys_locs {
        assert_eq!(v.len(), 1);
    }

    // 4. LookupWarableCorporationsOrAlliances
    let warable = call_lookup(
        &ctx,
        &session,
        "LookupWarableCorporationsOrAlliances",
        &[PyRep::String("wartargetable".into()), PyRep::Integer(0)],
        &kw,
    )
    .unwrap()
    .unwrap();
    if let PyRep::List(v) = warable {
        let ids: Vec<i64> = v
            .iter()
            .filter_map(|r| r.dict_get("ownerID").and_then(|x| x.as_i64()))
            .collect();
        assert_eq!(ids, vec![war_eligible_corp, war_eligible_ally]);
    }
}

#[test]
fn search_query_and_quick_query_groups_flags_and_error_branches() {
    let (ctx, session, char_id, _, sys_id) = make_fighter_ctx_and_session();
    let empty_kw = HashMap::new();

    let err_short = handle_search_query(&ctx, &session, &[PyRep::String("   ".into())], &empty_kw)
        .unwrap_err();
    assert_eq!(err_short.code, "SearchStringTooShort");

    let err_ban =
        handle_search_quick_query(&ctx, &session, &[PyRep::String("badword".into())], &empty_kw)
            .unwrap_err();
    assert_eq!(err_ban.message, "LocationNameInvalidBannedWord");

    let struct_id = 1_046_000_001i64;
    let char_corp = ctx.db.with_snapshot(|s| s.characters[&char_id].corporation_id);
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            struct_id,
            UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 35832,
                owner_corp_id: char_corp,
                solar_system_id: sys_id,
                name: "citadelsearchhub".into(),
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                ..Default::default()
            },
        );
        snap.corporations.insert(
            98_200_001,
            { let mut __corp = CorporationRecord::default();
                __corp.corporation_id = 98_200_001;
                __corp.name = "fullcorpnamesearch".into();
                __corp.ticker = "tckrz".into();
                __corp.ceo_id = char_id;
                __corp },
        );
        snap.alliances.insert(
            99_200_001,
            AllianceRecord {
                alliance_id: 99_200_001,
                name: "fullalliancenamesearch".into(),
                short_name: "alshz".into(),
                executor_corp_id: 98_200_001,
                member_corp_ids: HashSet::from([98_200_001]),
            },
        );
    });

    let q_res = handle_search_query(
        &ctx,
        &session,
        &[
            PyRep::String("a".into()),
            PyRep::List(vec![]),
            PyRep::Integer(0),
            PyRep::Integer(1),
            PyRep::Integer(0),
        ],
        &empty_kw,
    )
    .unwrap();
    if let PyRep::Dict(entries) = q_res {
        assert_eq!(entries.len(), 11);
        let g1 = entries.iter().find(|(k, _)| k.as_i64() == Some(1)).map(|(_, v)| v).unwrap();
        let g5 = entries.iter().find(|(k, _)| k.as_i64() == Some(5)).map(|(_, v)| v).unwrap();
        assert_eq!(*g1, PyRep::List(vec![]));
        assert_eq!(*g5, PyRep::List(vec![]));
    }

    let alt_miss = handle_search_quick_query(
        &ctx,
        &session,
        &[
            PyRep::String("fullcorpnamesearch".into()),
            PyRep::Integer(3),
            PyRep::Integer(1),
            PyRep::Integer(1),
            PyRep::Integer(1),
        ],
        &empty_kw,
    )
    .unwrap();
    assert_eq!(alt_miss, PyRep::List(vec![]));

    let alt_hit = handle_search_quick_query(
        &ctx,
        &session,
        &[
            PyRep::String("tckrz".into()),
            PyRep::Integer(3),
            PyRep::Integer(1),
            PyRep::Integer(1),
            PyRep::Integer(1),
        ],
        &empty_kw,
    )
    .unwrap();
    assert_eq!(alt_hit, PyRep::List(vec![PyRep::Integer(98_200_001)]));

    let struct_hit = handle_search_quick_query(
        &ctx,
        &session,
        &[
            PyRep::String("citadelsearchhub".into()),
            PyRep::Tuple(vec![
                PyRep::Integer(11),
                PyRep::Integer(1404),
                PyRep::Integer(999),
            ]),
        ],
        &empty_kw,
    )
    .unwrap();
    assert_eq!(struct_hit, PyRep::List(vec![PyRep::Integer(struct_id)]));
}
