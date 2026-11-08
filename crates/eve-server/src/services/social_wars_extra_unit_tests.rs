//! Extra unit and mutation-killing tests for `social_wars` via `social_corp_fleet::handle`.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::make_fighter_ctx_and_session;
use crate::services::social_corp_fleet::handle;
use eve_db::{CorporationRecord, IskCents, KillmailRecord, UpwellStructureRecord};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::HashMap;

async fn call_wars(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    bound_obj_id: Option<i64>,
) -> Result<PyRep, BridgeRpcError> {
    let kw = HashMap::new();
    handle(ctx, session, service, method, args, &kw, bound_obj_id)
        .await
        .expect("handled war rpc")
}

fn setup_extra_wars_ctx() -> (
    ServerContext,
    SessionState,
    i64,
    SessionState,
    i64,
    SessionState,
    i64,
    i64,
    i64,
) {
    let (ctx, mut agg_sess, agg_char, _ship_id, sys_id) = make_fighter_ctx_and_session();
    let agg_corp = 98_500_101i64;
    let def_corp = 98_500_102i64;
    let ally_corp = 98_500_103i64;
    let def_char = 90_500_102i64;
    let ally_char = 90_500_103i64;
    let agg_hq_id = 1_000_500_100_001i64;
    let def_st_id = 1_000_500_100_002i64;

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
            (def_char, 502, def_corp, "DefCEO"),
            (ally_char, 503, ally_corp, "AllyCEO"),
        ] {
            let mut ch = base_char.clone();
            ch.character_id = cid;
            ch.user_id = uid;
            ch.name = name.into();
            ch.corporation_id = corp;
            ch.solar_system_id = sys_id;
            ch.active_ship_id = 100_500_100 + uid as i64;
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
                    hull_hp: 500_000.0,
                    ..Default::default()
                },
            );
        }
    });

    (
        ctx, agg_sess, agg_corp, def_sess, def_corp, ally_sess, ally_corp, agg_hq_id, def_st_id,
    )
}

#[tokio::test]
async fn declare_war_validation_matrix_and_sync_wars_hq_defender_loss() {
    let (ctx, mut agg_sess, agg_corp, mut def_sess, def_corp, _, _, agg_hq_id, def_st_id) =
        setup_extra_wars_ctx();

    // NPC corporation cannot declare war
    let agg_char = agg_sess.character_id.unwrap();
    let mut npc_sess = agg_sess.clone();
    npc_sess.corporation_id = Some(1_000_125);
    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&agg_char).unwrap().corporation_id = 1_000_125;
    });
    assert_eq!(
        call_wars(&ctx, &mut npc_sess, "corpRegistry", "DeclareWarAgainst", &[PyRep::Integer(def_corp)], None).await.unwrap_err().code,
        "NpcCorporationCannotDeclareWar"
    );
    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&agg_char).unwrap().corporation_id = agg_corp;
    });

    // Non-CEO/Director cannot declare war
    let mut low_role = agg_sess.clone();
    low_role.character_id = Some(99_999_999);
    low_role.corp_role = 0;
    assert_eq!(
        call_wars(&ctx, &mut low_role, "warRegistry", "DeclareWarAgainst", &[PyRep::Integer(def_corp)], None).await.unwrap_err().code,
        "CeoRoleRequired"
    );

    // InvalidWarTarget (self or NPC target)
    assert_eq!(
        call_wars(&ctx, &mut agg_sess, "warRegistry", "DeclareWarAgainst", &[PyRep::Integer(agg_corp)], None).await.unwrap_err().code,
        "InvalidWarTarget"
    );
    assert_eq!(
        call_wars(&ctx, &mut agg_sess, "warRegistry", "DeclareWarAgainst", &[PyRep::Integer(1_000_125)], None).await.unwrap_err().code,
        "InvalidWarTarget"
    );

    // WarHqRequired when invalid HQ specified
    assert_eq!(
        call_wars(&ctx, &mut agg_sess, "warRegistry", "DeclareWarAgainst", &[PyRep::Integer(def_corp), PyRep::Integer(def_st_id)], None).await.unwrap_err().code,
        "WarHqRequired"
    );

    // DefenderNotWarEligible when defender has no structures
    let saved_def_st = ctx.db.with_mut_snapshot(|s| s.structures.remove(&def_st_id).unwrap());
    assert_eq!(
        call_wars(&ctx, &mut agg_sess, "warRegistry", "DeclareWarAgainst", &[PyRep::Integer(def_corp)], None).await.unwrap_err().code,
        "DefenderNotWarEligible"
    );
    ctx.db.with_mut_snapshot(|s| {
        s.structures.insert(def_st_id, saved_def_st);
    });

    // InsufficientFunds when master_wallet < 100M ISK
    ctx.db.with_mut_snapshot(|s| {
        let c = s.corporations.get_mut(&agg_corp).unwrap();
        c.set_division_balance(1, IskCents::from_isk(1_000));
        c.wallet_divisions.clear();
        c.ensure_wallet_divisions();
    });
    assert_eq!(
        call_wars(&ctx, &mut agg_sess, "warRegistry", "DeclareWarAgainst", &[PyRep::Integer(def_corp)], None).await.unwrap_err().code,
        "InsufficientFunds"
    );
    ctx.db.with_mut_snapshot(|s| {
        let c = s.corporations.get_mut(&agg_corp).unwrap();
        c.set_division_balance(1, IskCents::from_isk(500_000_000));
        c.wallet_divisions.clear();
        c.ensure_wallet_divisions();
    });

    // Declare war without explicit HQ arg -> automatically selects lowest structure ID (`agg_hq_id`)
    let war = call_wars(&ctx, &mut agg_sess, "corpRegistry", "DeclareWarAgainst", &[PyRep::Integer(def_corp)], None).await.unwrap();
    let war_id = war.dict_get("warID").and_then(|v| v.as_i64()).unwrap();
    assert_eq!(war.dict_get("warHQ").and_then(|v| v.as_i64()), Some(agg_hq_id));

    // Duplicate war declaration fails AlreadyAtWar
    assert_eq!(
        call_wars(&ctx, &mut agg_sess, "warRegistry", "DeclareWarAgainst", &[PyRep::Integer(def_corp)], None).await.unwrap_err().code,
        "AlreadyAtWar"
    );

    // War info & timestamp methods: GetPublicWarInfo, GetTop50, GetWarsForStructure, timeDeclared/timeStarted/timeFinished
    let _ = call_wars(&ctx, &mut agg_sess, "warsInfoMgr", "GetPublicWarInfo", &[PyRep::Integer(war_id)], None).await.unwrap();
    let _ = call_wars(&ctx, &mut agg_sess, "warsInfoMgr", "GetTop50", &[], None).await.unwrap();
    let _ = call_wars(&ctx, &mut agg_sess, "warsInfoMgr", "GetWarsForStructure", &[PyRep::Integer(agg_hq_id)], None).await.unwrap();
    for m in ["timeDeclared", "timeStarted", "timeFinished"] {
        let _ = call_wars(&ctx, &mut agg_sess, "warRegistry", m, &[PyRep::Integer(war_id)], None).await.unwrap();
    }

    // sync_wars: destroying war HQ ends the war; then surrender + AcceptSurrender creates a peace treaty -> DeclareWarAgainst fails PeaceTreatyActive
    ctx.db.with_mut_snapshot(|s| {
        s.structures.get_mut(&agg_hq_id).unwrap().state = "Destroyed".into();
    });
    let after_hq_loss = call_wars(&ctx, &mut def_sess, "warsInfoMgr", "GetPublicWarInfo", &[PyRep::Integer(war_id)], None).await.unwrap();
    assert_eq!(after_hq_loss.dict_get("active").and_then(|v| v.as_bool()), Some(false));
    assert!(after_hq_loss.dict_get("timeFinished").is_some_and(|v| !matches!(v, PyRep::None)));
    ctx.db.with_mut_snapshot(|s| {
        s.structures.get_mut(&agg_hq_id).unwrap().state = "ShieldVulnerable".into();
    });
    let war2 = call_wars(&ctx, &mut agg_sess, "warRegistry", "DeclareWarAgainst", &[PyRep::Integer(def_corp)], None).await.unwrap();
    let war2_id = war2.dict_get("warID").and_then(|v| v.as_i64()).unwrap();
    let sur = call_wars(&ctx, &mut def_sess, "warsInfoMgr", "CreateSurrenderNegotiation", &[PyRep::Integer(war2_id), PyRep::Real(1_000_000.0), PyRep::String("Surrender".into())], None).await.unwrap();
    let sur_id = sur.dict_get("negotiationID").and_then(|v| v.as_i64()).unwrap();
    let _ = call_wars(&ctx, &mut agg_sess, "warsInfoMgr", "AcceptSurrender", &[PyRep::Integer(sur_id)], None).await.unwrap();
    let treaties = call_wars(&ctx, &mut agg_sess, "peaceTreatyMgr", "GetPeaceTreatiesForSession", &[], None).await.unwrap();
    assert!(matches!(treaties, PyRep::Rowset { .. }));
    assert_eq!(
        call_wars(&ctx, &mut agg_sess, "warRegistry", "DeclareWarAgainst", &[PyRep::Integer(def_corp)], None).await.unwrap_err().code,
        "PeaceTreatyActive"
    );
}

#[tokio::test]
async fn war_negotiations_decline_retract_mutual_invites_and_statistics_groups() {
    let (ctx, mut agg_sess, agg_corp, mut def_sess, def_corp, mut ally_sess, ally_corp, agg_hq_id, _) =
        setup_extra_wars_ctx();

    let war = call_wars(
        &ctx,
        &mut agg_sess,
        "warRegistry",
        "DeclareWarAgainst",
        &[PyRep::Integer(def_corp), PyRep::Integer(agg_hq_id)],
        None,
    )
    .await
    .unwrap();
    let war_id = war.dict_get("warID").and_then(|v| v.as_i64()).unwrap();

    // Bind/UnBind/GetWarsByOwners/GetWarsRequiringAssistance
    let _ = call_wars(&ctx, &mut agg_sess, "warRegistry", "Bind", &[PyRep::Integer(agg_corp)], None).await.unwrap();
    let _ = call_wars(&ctx, &mut agg_sess, "warRegistry", "UnBind", &[], None).await.unwrap();
    let by_owners = call_wars(
        &ctx,
        &mut agg_sess,
        "warsInfoMgr",
        "GetWarsByOwners",
        &[PyRep::List(vec![PyRep::Integer(agg_corp), PyRep::Integer(def_corp)])],
        None,
    )
    .await
    .unwrap();
    assert!(matches!(by_owners, PyRep::Rowset { .. }));
    let req_assist = call_wars(&ctx, &mut agg_sess, "warsInfoMgr", "GetWarsRequiringAssistance", &[], None).await.unwrap();
    assert!(matches!(req_assist, PyRep::Rowset { .. }));

    // SetOpenForAllies: aggressor fails NotWarDefender; defender can toggle
    assert_eq!(
        call_wars(&ctx, &mut agg_sess, "warsInfoMgr", "SetOpenForAllies", &[PyRep::Integer(war_id), PyRep::Bool(false)], None).await.unwrap_err().code,
        "NotWarDefender"
    );
    let _ = call_wars(&ctx, &mut def_sess, "warRegistry", "SetOpenForAllies", &[PyRep::Bool(false)], Some(war_id)).await.unwrap();
    assert_eq!(
        call_wars(
            &ctx,
            &mut ally_sess,
            "warsInfoMgr",
            "CreateWarAllyOffer",
            &[PyRep::Integer(war_id), PyRep::Real(1_000_000.0), PyRep::String("No".into())],
            None
        )
        .await
        .unwrap_err()
        .code,
        "WarNotOpenForAllies"
    );
    let _ = call_wars(&ctx, &mut def_sess, "warsInfoMgr", "SetOpenForAllies", &[PyRep::Integer(war_id), PyRep::Bool(true)], None).await.unwrap();

    // Ally offer -> RetractWarAllyOffer by ally; second ally offer -> DeclineAllyOffer by defender
    let a1 = call_wars(
        &ctx,
        &mut ally_sess,
        "warsInfoMgr",
        "CreateWarAllyOffer",
        &[PyRep::Integer(war_id), PyRep::Real(2_000_000.0), PyRep::String("Offer1".into())],
        None,
    )
    .await
    .unwrap();
    let a1_id = a1.dict_get("negotiationID").and_then(|v| v.as_i64()).unwrap();
    let ret_a1 = call_wars(&ctx, &mut ally_sess, "warsInfoMgr", "RetractWarAllyOffer", &[PyRep::Integer(a1_id)], None).await.unwrap();
    assert_eq!(ret_a1.dict_get("status").and_then(|v| v.as_str()), Some("Retracted"));

    let a2 = call_wars(
        &ctx,
        &mut ally_sess,
        "warsInfoMgr",
        "CreateWarAllyOffer",
        &[PyRep::Integer(war_id), PyRep::Real(2_000_000.0), PyRep::String("Offer2".into())],
        None,
    )
    .await
    .unwrap();
    let a2_id = a2.dict_get("negotiationID").and_then(|v| v.as_i64()).unwrap();
    let dec_a2 = call_wars(&ctx, &mut def_sess, "warsInfoMgr", "DeclineAllyOffer", &[PyRep::Integer(a2_id)], None).await.unwrap();
    assert_eq!(dec_a2.dict_get("status").and_then(|v| v.as_str()), Some("Declined"));

    // Surrender offer -> DeclineSurrender + GetSurrenderNegotiations + GetAllyNegotiations + GetWarNegotiation
    let s1 = call_wars(
        &ctx,
        &mut def_sess,
        "warsInfoMgr",
        "CreateSurrenderNegotiation",
        &[PyRep::Integer(war_id), PyRep::Real(5_000_000.0), PyRep::String("Peace?".into())],
        None,
    )
    .await
    .unwrap();
    let s1_id = s1.dict_get("negotiationID").and_then(|v| v.as_i64()).unwrap();
    let _ = call_wars(&ctx, &mut def_sess, "warRegistry", "GetWarNegotiation", &[PyRep::Integer(s1_id)], Some(war_id)).await.unwrap();
    let _ = call_wars(&ctx, &mut def_sess, "warRegistry", "GetNegotiations", &[PyRep::Integer(war_id)], Some(war_id)).await.unwrap();
    let _ = call_wars(&ctx, &mut def_sess, "warsInfoMgr", "GetSurrenderNegotiations", &[PyRep::Integer(war_id)], None).await.unwrap();
    let _ = call_wars(&ctx, &mut def_sess, "warsInfoMgr", "GetAllyNegotiations", &[PyRep::Integer(war_id)], None).await.unwrap();
    let dec_s1 = call_wars(&ctx, &mut agg_sess, "warsInfoMgr", "DeclineSurrender", &[PyRep::Integer(s1_id)], None).await.unwrap();
    assert_eq!(dec_s1.dict_get("status").and_then(|v| v.as_str()), Some("Declined"));

    // mutualWarInviteMgr: block invites, check blocked, unblock, send, pending, withdraw
    let _ = call_wars(&ctx, &mut ally_sess, "mutualWarInviteMgr", "SetInvitesBlockedByPlayer", &[PyRep::Bool(true)], None).await.unwrap();
    assert_eq!(
        call_wars(&ctx, &mut ally_sess, "mutualWarInviteMgr", "IsCorpInvitesBlockedPlayer", &[], None).await.unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(
        call_wars(&ctx, &mut agg_sess, "mutualWarInviteMgr", "SendInviteByPlayer", &[PyRep::Integer(ally_corp)], None).await.unwrap_err().code,
        "CorpInvitesBlocked"
    );
    let _ = call_wars(&ctx, &mut ally_sess, "mutualWarInviteMgr", "SetInvitesBlockedByPlayer", &[PyRep::Bool(false)], None).await.unwrap();
    let _ = call_wars(&ctx, &mut agg_sess, "mutualWarInviteMgr", "SendInviteByPlayer", &[PyRep::Integer(ally_corp)], None).await.unwrap();
    let pending = call_wars(&ctx, &mut ally_sess, "mutualWarInviteMgr", "GetPendingInvitesForSession", &[], None).await.unwrap();
    assert!(matches!(pending, PyRep::Rowset { .. }));
    let _ = call_wars(&ctx, &mut agg_sess, "mutualWarInviteMgr", "WithdrawInviteByPlayer", &[PyRep::Integer(ally_corp)], None).await.unwrap();

    // warStatisticMgr: Bind/SetSessionCheck/GetBaseInfo/GetKills/GetKillsByGroup/GetKillMail & defender killmail
    let now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        snap.killmails.insert(
            701,
            KillmailRecord {
                kill_id: 701,
                solar_system_id: 30_000_142,
                killed_unix: now,
                victim_character_id: 90_000_001,
                victim_corp_id: agg_corp,
                victim_alliance_id: None,
                ship_type_id: 600,
                attacker_character_id: 90_500_102,
                attacker_corp_id: def_corp,
                attacker_alliance_id: None,
                total_value_isk: 15_000_000.0,
            },
        );
    });
    let _ = call_wars(&ctx, &mut agg_sess, "warStatisticMgr", "Bind", &[PyRep::Integer(war_id)], None).await.unwrap();
    let _ = call_wars(&ctx, &mut agg_sess, "warStatisticMgr", "SetSessionCheck", &[], Some(war_id)).await.unwrap();
    let base_info = call_wars(&ctx, &mut agg_sess, "warStatisticMgr", "GetBaseInfo", &[PyRep::Integer(war_id)], Some(war_id)).await.unwrap();
    assert_eq!(base_info.dict_get("killsForDefender").and_then(|v| v.as_i64()), Some(1));
    let kills = call_wars(&ctx, &mut agg_sess, "warStatisticMgr", "GetKills", &[PyRep::Integer(war_id)], Some(war_id)).await.unwrap();
    assert!(matches!(kills, PyRep::Rowset { .. }));
    let by_grp = call_wars(&ctx, &mut agg_sess, "warStatisticMgr", "GetKillsByGroup", &[PyRep::Integer(war_id)], Some(war_id)).await.unwrap();
    assert!(matches!(by_grp, PyRep::Rowset { .. }));
    let km = call_wars(&ctx, &mut agg_sess, "warStatisticMgr", "GetKillMail", &[PyRep::Integer(701)], Some(war_id)).await.unwrap();
    assert_eq!(km.dict_get("killID").and_then(|v| v.as_i64()), Some(701));
    assert_eq!(
        call_wars(&ctx, &mut agg_sess, "warStatisticMgr", "GetKillMail", &[PyRep::Integer(999_999)], Some(war_id)).await.unwrap_err().code,
        "KillmailNotFound"
    );
}
