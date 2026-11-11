//! Additional edge-case and branch-coverage tests for `social_wars`.

#![forbid(unsafe_code)]

use super::*;
use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::make_fighter_ctx_and_session;
use eve_db::{CorporationRecord, IskCents, KillmailRecord, UpwellStructureRecord, WarRecord};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::HashMap;

const ERR_WAR_NOT_FOUND: &str = "WarNotFound";

async fn call_wars(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    bound_obj_id: Option<i64>,
) -> Result<PyRep, BridgeRpcError> {
    let kw = HashMap::new();
    crate::services::social_corp_fleet::handle(ctx, session, service, method, args, &kw, bound_obj_id)
        .await
        .expect("handled war rpc")
}

fn setup_wars_edge_ctx() -> (
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
    ctx.set_clock(std::sync::Arc::new(
        eve_db::VirtualServerClock::with_fixed_base(1_800_000_000),
    ));
    let agg_corp = 98_600_101_i64;
    let def_corp = 98_600_102_i64;
    let ally_corp = 98_600_103_i64;
    let def_char = 90_600_102_i64;
    let ally_char = 90_600_103_i64;
    let agg_hq_id = 1_000_600_100_001_i64;
    let def_st_id = 1_000_600_100_002_i64;

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
            (def_char, 602, def_corp, "DefCEO"),
            (ally_char, 603, ally_corp, "AllyCEO"),
        ] {
            let mut ch = base_char.clone();
            ch.character_id = cid;
            ch.user_id = uid;
            ch.name = name.into();
            ch.corporation_id = corp;
            ch.solar_system_id = sys_id;
            ch.active_ship_id = 100_600_100 + uid as i64;
            ch.balance = IskCents::from_isk(100_000_000);
            snap.characters.insert(cid, ch);
        }
        for (corp_id, ceo_id, ticker) in [
            (agg_corp, agg_char, "AG2"),
            (def_corp, def_char, "DF2"),
            (ally_corp, ally_char, "AL2"),
        ] {
            let mut c = { let mut __corp = CorporationRecord::default(); __corp.corporation_id = corp_id; __corp.name = format!("Corp {ticker}"); __corp.ticker = ticker.into(); __corp.ceo_id = ceo_id; __corp.alliance_id = None; __corp.tax_rate = 0.05; __corp.members = vec![ceo_id].into_iter().collect(); __corp.member_roles = [(ceo_id, u64::MAX)].into_iter().collect(); __corp.hq_station_id = Some(60_003_760); let __mw = IskCents::from_isk(1_000_000_000); if __mw.0 != 0 { __corp.set_division_balance(1, __mw); } else { __corp.ensure_wallet_divisions(); } __corp };
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
async fn social_wars_dispatch_unauth_unbind_timestamps_gm_and_get_top50_edges() {
    let (ctx, mut agg_sess, agg_corp, mut def_sess, def_corp, _, _, agg_hq_id, _) =
        setup_wars_edge_ctx();

    // 1. Non-war RPC returns None from handle_wars.
    assert!(handle_wars(&ctx, &mut agg_sess, "corpRegistry", "GetCorporation", &[], None).is_none());

    // 2. Unauthenticated session fails with error.
    let mut unauth = SessionState::new(0);
    assert!(handle_wars(&ctx, &mut unauth, "warRegistry", "GetWars", &[], None)
        .unwrap()
        .is_err());

    // 3. Same-alliance DeclareWarAgainst -> InvalidWarTarget.
    ctx.db.with_mut_snapshot(|s| {
        s.corporations.get_mut(&agg_corp).unwrap().alliance_id = Some(99_600_001);
        s.corporations.get_mut(&def_corp).unwrap().alliance_id = Some(99_600_001);
    });
    assert_eq!(
        call_wars(
            &ctx,
            &mut agg_sess,
            "warRegistry",
            "DeclareWarAgainst",
            &[PyRep::Integer(def_corp)],
            None,
        )
        .await
        .unwrap_err()
        .code,
        "InvalidWarTarget"
    );
    ctx.db.with_mut_snapshot(|s| {
        s.corporations.get_mut(&agg_corp).unwrap().alliance_id = None;
        s.corporations.get_mut(&def_corp).unwrap().alliance_id = None;
    });

    // 4. timeDeclared / timeStarted / timeFinished when corp has no wars -> WarNotFound.
    for m in ["timeDeclared", "timeStarted", "timeFinished"] {
        assert_eq!(
            call_wars(&ctx, &mut agg_sess, "warRegistry", m, &[], None)
                .await
                .unwrap_err()
                .code,
            ERR_WAR_NOT_FOUND
        );
        assert_eq!(
            call_wars(&ctx, &mut agg_sess, "warRegistry", m, &[PyRep::Integer(999_999)], None)
                .await
                .unwrap_err()
                .code,
            ERR_WAR_NOT_FOUND
        );
    }

    // 5. Declare war, test timeFinished == None before surrender, and default filter on GetWars & GetWarsByOwnerID.
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

    assert_eq!(
        call_wars(&ctx, &mut agg_sess, "warRegistry", "timeFinished", &[], None)
            .await
            .unwrap(),
        PyRep::None
    );
    assert!(matches!(
        call_wars(&ctx, &mut agg_sess, "warRegistry", "GetWars", &[], None)
            .await
            .unwrap(),
        PyRep::Dict(_)
    ));
    assert!(matches!(
        call_wars(&ctx, &mut agg_sess, "warsInfoMgr", "GetWarsByOwnerID", &[], None)
            .await
            .unwrap(),
        PyRep::ObjectEx { .. }
    ));

    // 6. warRegistry Bind / Unbind (lowercase 'b') with valid & invalid bound IDs,
    // plus warStatisticMgr Bind / UnBind / Unbind / SetSessionCheck.
    let wr_bid = call_wars(&ctx, &mut agg_sess, "warRegistry", "Bind", &[PyRep::Integer(agg_corp)], None)
        .await
        .unwrap()
        .as_i64()
        .unwrap();
    assert_eq!(
        call_wars(&ctx, &mut agg_sess, "warRegistry", "Unbind", &[PyRep::Integer(wr_bid)], None)
            .await
            .unwrap(),
        PyRep::None
    );
    assert!(call_wars(&ctx, &mut agg_sess, "warRegistry", "UnBind", &[PyRep::Integer(999_999)], None)
        .await
        .is_err());

    let ws_bid = call_wars(&ctx, &mut agg_sess, "warStatisticMgr", "Bind", &[PyRep::Integer(war_id)], None)
        .await
        .unwrap()
        .as_i64()
        .unwrap();
    assert_eq!(
        call_wars(&ctx, &mut agg_sess, "warStatisticMgr", "SetSessionCheck", &[], None)
            .await
            .unwrap(),
        PyRep::None
    );
    assert_eq!(
        call_wars(&ctx, &mut agg_sess, "warStatisticMgr", "UnBind", &[PyRep::Integer(ws_bid)], None)
            .await
            .unwrap(),
        PyRep::None
    );
    assert_eq!(
        call_wars(&ctx, &mut agg_sess, "warStatisticMgr", "Unbind", &[], None)
            .await
            .unwrap(),
        PyRep::None
    );
    assert!(call_wars(&ctx, &mut agg_sess, "warStatisticMgr", "Unbind", &[PyRep::Integer(999_999)], None)
        .await
        .is_err());

    // 7. GM warRegistry commands all return Bool(true).
    for gm in [
        "GMActivateDefender",
        "GMDeactivateDefender",
        "GMExtendAllyContract",
        "GMJoinDefender",
        "GMSetDeclareTime",
        "GMSetWarFinishTime",
    ] {
        assert_eq!(
            call_wars(&ctx, &mut agg_sess, "warRegistry", gm, &[], None)
                .await
                .unwrap(),
            PyRep::Bool(true)
        );
    }

    // 8. GetPublicWarInfo / GetBaseInfo / GetKills / GetKillsByGroup with unknown war_id -> WarNotFound.
    for (svc, meth) in [
        ("warsInfoMgr", "GetPublicWarInfo"),
        ("warStatisticMgr", "GetBaseInfo"),
        ("warStatisticMgr", "GetKills"),
        ("warStatisticMgr", "GetKillsByGroup"),
    ] {
        assert_eq!(
            call_wars(&ctx, &mut def_sess, svc, meth, &[PyRep::Integer(888_888)], None)
                .await
                .unwrap_err()
                .code,
            "WarNotFound"
        );
    }

    // 9. GetTop50 truncates to 50 when > 50 active wars exist.
    ctx.db.with_mut_snapshot(|s| {
        for i in 1..=55_i64 {
            let wid = 770_000 + i;
            s.wars.insert(
                wid,
                WarRecord {
                    war_id: wid,
                    aggressor_corp_id: agg_corp,
                    defender_corp_id: def_corp,
                    mutual: true,
                    started_unix: 100,
                    active: true,
                },
            );
        }
    });
    let top50 = call_wars(&ctx, &mut agg_sess, "warsInfoMgr", "GetTop50", &[], None)
        .await
        .unwrap();
    if let PyRep::Rowset { rows, .. } = top50 {
        assert_eq!(rows.len(), 50);
    } else {
        panic!("expected Rowset from GetTop50");
    }
}

#[tokio::test]
async fn social_wars_negotiation_and_mutual_war_error_paths_plus_kill_filtering() {
    let (ctx, mut agg_sess, agg_corp, mut def_sess, def_corp, mut ally_sess, ally_corp, agg_hq_id, _) =
        setup_wars_edge_ctx();
    let now = ctx.now_unix();

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

    // Non-director character
    let mut no_role_sess = def_sess.clone();
    no_role_sess.character_id = Some(99_600_999);
    no_role_sess.corp_role = 0;

    // 1. CreateSurrenderNegotiation / CreateWarAllyOffer validation errors:
    // - CorpRoleDenied
    assert_eq!(
        call_wars(&ctx, &mut no_role_sess, "warsInfoMgr", "CreateSurrenderNegotiation", &[PyRep::Integer(war_id), PyRep::Real(0.0)], None)
            .await
            .unwrap_err()
            .code,
        "CorpRoleDenied"
    );
    // - InvalidIskValue (negative and NaN)
    for bad_isk in [-5.0, f64::NAN] {
        assert_eq!(
            call_wars(&ctx, &mut def_sess, "warsInfoMgr", "CreateSurrenderNegotiation", &[PyRep::Integer(war_id), PyRep::Real(bad_isk)], None)
                .await
                .unwrap_err()
                .code,
            "InvalidIskValue"
        );
    }
    // - WarNotFound
    assert_eq!(
        call_wars(&ctx, &mut def_sess, "warsInfoMgr", "CreateSurrenderNegotiation", &[PyRep::Integer(999_999), PyRep::Real(0.0)], None)
            .await
            .unwrap_err()
            .code,
        "WarNotFound"
    );
    // - NotWarParticipant (ally tries surrender)
    assert_eq!(
        call_wars(&ctx, &mut ally_sess, "warsInfoMgr", "CreateSurrenderNegotiation", &[PyRep::Integer(war_id), PyRep::Real(0.0)], None)
            .await
            .unwrap_err()
            .code,
        "NotWarParticipant"
    );
    // - CannotAllyOwnWar (defender tries ally offer)
    assert_eq!(
        call_wars(&ctx, &mut def_sess, "warsInfoMgr", "CreateWarAllyOffer", &[PyRep::Integer(war_id), PyRep::Real(0.0)], None)
            .await
            .unwrap_err()
            .code,
        "CannotAllyOwnWar"
    );
    // - CorpRoleDenied when non-registered / NPC corporation attempts CreateWarAllyOffer
    let mut npc_ally = ally_sess.clone();
    let ally_char = ally_sess.character_id.unwrap();
    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&ally_char).unwrap().corporation_id = 1_000_125;
        s.war_state.wars.entry(war_id).or_default().open_for_allies = true;
    });
    npc_ally.corporation_id = Some(1_000_125);
    assert_eq!(
        call_wars(&ctx, &mut npc_ally, "warsInfoMgr", "CreateWarAllyOffer", &[PyRep::Integer(war_id), PyRep::Real(0.0)], None)
            .await
            .unwrap_err()
            .code,
        "CorpRoleDenied"
    );
    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&ally_char).unwrap().corporation_id = ally_corp;
    });

    // 2. Duplicate ally offer -> AllyOfferAlreadyExists; wrong kind & counterparty checks on Accept/Decline/Retract.
    let ally_neg = call_wars(
        &ctx,
        &mut ally_sess,
        "warsInfoMgr",
        "CreateWarAllyOffer",
        &[PyRep::Integer(war_id), PyRep::Real(1_000_000.0), PyRep::String("Help".into())],
        None,
    )
    .await
    .unwrap();
    let ally_neg_id = ally_neg.dict_get("negotiationID").and_then(|v| v.as_i64()).unwrap();
    assert_eq!(
        call_wars(
            &ctx,
            &mut ally_sess,
            "warsInfoMgr",
            "CreateWarAllyOffer",
            &[PyRep::Integer(war_id), PyRep::Real(1_000_000.0)],
            None,
        )
        .await
        .unwrap_err()
        .code,
        "AllyOfferAlreadyExists"
    );

    // GetWarNegotiation for Ally neg returns warNegotiationTypeID == 2; outsider gets NotWarParticipant;
    // missing neg_id for participant returns fallback keyval with iskValue 0.0.
    let gn_ally = call_wars(&ctx, &mut ally_sess, "warRegistry", "GetWarNegotiation", &[PyRep::Integer(ally_neg_id)], Some(war_id))
        .await
        .unwrap();
    assert_eq!(gn_ally.dict_get("warNegotiationTypeID").and_then(|v| v.as_i64()), Some(2));
    let gn_fallback = call_wars(&ctx, &mut agg_sess, "warRegistry", "GetWarNegotiation", &[PyRep::Integer(999_999)], Some(war_id))
        .await
        .unwrap();
    assert_eq!(gn_fallback.dict_get("iskValue").and_then(|v| v.as_f64()), Some(0.0));
    let mut outsider_sess = agg_sess.clone();
    outsider_sess.character_id = Some(90_600_998);
    outsider_sess.corporation_id = Some(98_600_999);
    assert_eq!(
        call_wars(&ctx, &mut outsider_sess, "warRegistry", "GetWarNegotiation", &[PyRep::Integer(ally_neg_id)], Some(war_id))
            .await
            .unwrap_err()
            .code,
        "NotWarParticipant"
    );

    // Passing ally_neg_id to AcceptSurrender -> NegotiationNotFound (wrong kind);
    // Aggressor trying AcceptAllyNegotiation -> NotNegotiationCounterparty;
    // Defender trying RetractWarAllyOffer -> NotNegotiationCounterparty;
    // Non-director trying AcceptSurrender / AcceptAllyNegotiation -> CorpRoleDenied.
    assert_eq!(
        call_wars(&ctx, &mut no_role_sess, "warsInfoMgr", "AcceptSurrender", &[PyRep::Integer(ally_neg_id)], None)
            .await
            .unwrap_err()
            .code,
        "CorpRoleDenied"
    );
    assert_eq!(
        call_wars(&ctx, &mut no_role_sess, "warsInfoMgr", "AcceptAllyNegotiation", &[PyRep::Integer(ally_neg_id)], None)
            .await
            .unwrap_err()
            .code,
        "CorpRoleDenied"
    );
    assert_eq!(
        call_wars(&ctx, &mut agg_sess, "warsInfoMgr", "AcceptSurrender", &[PyRep::Integer(ally_neg_id)], None)
            .await
            .unwrap_err()
            .code,
        "NegotiationNotFound"
    );
    assert_eq!(
        call_wars(&ctx, &mut agg_sess, "warsInfoMgr", "AcceptAllyNegotiation", &[PyRep::Integer(ally_neg_id)], None)
            .await
            .unwrap_err()
            .code,
        "NotNegotiationCounterparty"
    );
    assert_eq!(
        call_wars(&ctx, &mut def_sess, "warsInfoMgr", "RetractWarAllyOffer", &[PyRep::Integer(ally_neg_id)], None)
            .await
            .unwrap_err()
            .code,
        "NotNegotiationCounterparty"
    );

    // Accept ally offer -> GetAllyRowsByWarID shows 1 active ally.
    let acc_ally = call_wars(&ctx, &mut def_sess, "warsInfoMgr", "AcceptAllyNegotiation", &[PyRep::Integer(ally_neg_id)], None)
        .await
        .unwrap();
    assert_eq!(acc_ally.dict_get("allyID").and_then(|v| v.as_i64()), Some(ally_corp));
    let ally_rows = call_wars(&ctx, &mut def_sess, "warsInfoMgr", "GetAllyRowsByWarID", &[PyRep::Integer(war_id)], None)
        .await
        .unwrap();
    if let PyRep::Rowset { rows, .. } = ally_rows {
        assert_eq!(rows.len(), 1);
    } else {
        panic!("expected Rowset");
    }

    // 3. Killmail window & ally side filtering in war_kills_in_snap + GetKillMail missing killID & KillmailNotFound.
    assert_eq!(
        call_wars(&ctx, &mut agg_sess, "warStatisticMgr", "GetKillMail", &[], None)
            .await
            .unwrap_err()
            .code,
        "InvalidArgument"
    );
    assert_eq!(
        call_wars(&ctx, &mut agg_sess, "warStatisticMgr", "GetKillMail", &[PyRep::Integer(999_999)], None)
            .await
            .unwrap_err()
            .code,
        "KillmailNotFound"
    );
    ctx.db.with_mut_snapshot(|s| {
        s.corporations.get_mut(&agg_corp).unwrap().alliance_id = Some(99_600_111);
        s.corporations.get_mut(&def_corp).unwrap().alliance_id = Some(99_600_222);
        // Before war started -> excluded
        s.killmails.insert(
            600_001,
            KillmailRecord {
                kill_id: 600_001,
                solar_system_id: 30_000_142,
                victim_character_id: def_sess.character_id.unwrap(),
                victim_corp_id: def_corp,
                attacker_character_id: agg_sess.character_id.unwrap(),
                attacker_corp_id: agg_corp,
                ship_type_id: 587,
                total_value_isk: 1_000_000.0,
                killed_unix: now.saturating_sub(100),
                ..Default::default()
            },
        );
        // Accepted ally killing aggressor (with explicit corp == 0 -> resolved from character!) -> counted on defender side!
        s.killmails.insert(
            600_002,
            KillmailRecord {
                kill_id: 600_002,
                solar_system_id: 30_000_142,
                victim_character_id: agg_sess.character_id.unwrap(),
                victim_corp_id: 0,
                attacker_character_id: ally_sess.character_id.unwrap(),
                attacker_corp_id: 0,
                ship_type_id: 587,
                total_value_isk: 2_500_000.0,
                killed_unix: now + 86_400,
                ..Default::default()
            },
        );
    });
    let base_info = call_wars(&ctx, &mut agg_sess, "warStatisticMgr", "GetBaseInfo", &[PyRep::Integer(war_id)], None)
        .await
        .unwrap();
    assert_eq!(base_info.dict_get("killsForDefender").and_then(|v| v.as_i64()), Some(1));
    assert_eq!(base_info.dict_get("killsForAggressor").and_then(|v| v.as_i64()), Some(0));

    // 4. Multiple surrender offers: second CreateSurrenderNegotiation cancels first;
    // accepting surrender sets WarAlreadyEnding on subsequent offers/accepts.
    let s_old = call_wars(&ctx, &mut def_sess, "warsInfoMgr", "CreateSurrenderNegotiation", &[PyRep::Integer(war_id), PyRep::Real(100.0)], None)
        .await
        .unwrap();
    let s_old_id = s_old.dict_get("negotiationID").and_then(|v| v.as_i64()).unwrap();
    let s_new = call_wars(&ctx, &mut def_sess, "warsInfoMgr", "CreateSurrenderNegotiation", &[PyRep::Integer(war_id), PyRep::Real(200.0)], None)
        .await
        .unwrap();
    let s_new_id = s_new.dict_get("negotiationID").and_then(|v| v.as_i64()).unwrap();
    // Old surrender was cancelled, so AcceptSurrender on s_old_id fails NegotiationNotFound.
    assert_eq!(
        call_wars(&ctx, &mut agg_sess, "warsInfoMgr", "AcceptSurrender", &[PyRep::Integer(s_old_id)], None)
            .await
            .unwrap_err()
            .code,
        "NegotiationNotFound"
    );
    // Same-corp trying to accept own surrender -> NotNegotiationCounterparty.
    assert_eq!(
        call_wars(&ctx, &mut def_sess, "warsInfoMgr", "AcceptSurrender", &[PyRep::Integer(s_new_id)], None)
            .await
            .unwrap_err()
            .code,
        "NotNegotiationCounterparty"
    );
    call_wars(&ctx, &mut agg_sess, "warsInfoMgr", "AcceptSurrender", &[PyRep::Integer(s_new_id)], None)
        .await
        .unwrap();
    assert_eq!(
        call_wars(&ctx, &mut def_sess, "warsInfoMgr", "CreateSurrenderNegotiation", &[PyRep::Integer(war_id), PyRep::Real(300.0)], None)
            .await
            .unwrap_err()
            .code,
        "WarAlreadyEnding"
    );

    // Mark war inactive -> WarNotActive on CreateSurrenderNegotiation.
    ctx.db.with_mut_snapshot(|s| {
        s.wars.get_mut(&war_id).unwrap().active = false;
    });
    assert_eq!(
        call_wars(&ctx, &mut def_sess, "warsInfoMgr", "CreateSurrenderNegotiation", &[PyRep::Integer(war_id), PyRep::Real(300.0)], None)
            .await
            .unwrap_err()
            .code,
        "WarNotActive"
    );

    // 5. RetractMutualWar errors: CorpRoleDenied, WarNotFound, NotActiveMutualWar, NotWarParticipant.
    assert_eq!(
        call_wars(&ctx, &mut no_role_sess, "warsInfoMgr", "RetractMutualWar", &[PyRep::Integer(war_id)], None)
            .await
            .unwrap_err()
            .code,
        "CorpRoleDenied"
    );
    assert_eq!(
        call_wars(&ctx, &mut agg_sess, "warsInfoMgr", "RetractMutualWar", &[PyRep::Integer(999_999)], None)
            .await
            .unwrap_err()
            .code,
        "WarNotFound"
    );
    assert_eq!(
        call_wars(&ctx, &mut agg_sess, "warsInfoMgr", "RetractMutualWar", &[PyRep::Integer(war_id)], None)
            .await
            .unwrap_err()
            .code,
        "NotActiveMutualWar"
    );

    // 6. mutualWarInviteMgr role/target/decline/missing-invite edges:
    assert_eq!(
        call_wars(&ctx, &mut no_role_sess, "mutualWarInviteMgr", "SetInvitesBlockedByPlayer", &[PyRep::Integer(1)], None)
            .await
            .unwrap_err()
            .code,
        "CorpRoleDenied"
    );
    assert_eq!(
        call_wars(&ctx, &mut no_role_sess, "mutualWarInviteMgr", "SendInviteByPlayer", &[PyRep::Integer(agg_corp)], None)
            .await
            .unwrap_err()
            .code,
        "CorpRoleDenied"
    );
    assert_eq!(
        call_wars(&ctx, &mut agg_sess, "mutualWarInviteMgr", "SendInviteByPlayer", &[PyRep::Integer(agg_corp)], None)
            .await
            .unwrap_err()
            .code,
        "InvalidWarTarget"
    );
    assert_eq!(
        call_wars(&ctx, &mut no_role_sess, "mutualWarInviteMgr", "WithdrawInviteByPlayer", &[PyRep::Integer(agg_corp)], None)
            .await
            .unwrap_err()
            .code,
        "CorpRoleDenied"
    );
    assert_eq!(
        call_wars(&ctx, &mut agg_sess, "mutualWarInviteMgr", "WithdrawInviteByPlayer", &[PyRep::Integer(ally_corp)], None)
            .await
            .unwrap(),
        PyRep::Bool(false)
    );
    assert_eq!(
        call_wars(&ctx, &mut no_role_sess, "mutualWarInviteMgr", "RespondToInviteByPlayer", &[PyRep::Integer(agg_corp), PyRep::Bool(false)], None)
            .await
            .unwrap_err()
            .code,
        "CorpRoleDenied"
    );
    assert_eq!(
        call_wars(&ctx, &mut ally_sess, "mutualWarInviteMgr", "RespondToInviteByPlayer", &[PyRep::Integer(agg_corp), PyRep::Bool(true)], None)
            .await
            .unwrap_err()
            .code,
        "MutualWarInviteNotFound"
    );

    // Send invite and decline with Integer(0) -> accepted: false, warID: None.
    call_wars(&ctx, &mut agg_sess, "mutualWarInviteMgr", "SendInviteByPlayer", &[PyRep::Integer(ally_corp)], None)
        .await
        .unwrap();
    let declined = call_wars(&ctx, &mut ally_sess, "mutualWarInviteMgr", "RespondToInviteByPlayer", &[PyRep::Integer(agg_corp), PyRep::Integer(0)], None)
        .await
        .unwrap();
    assert_eq!(declined.dict_get("accepted").and_then(|v| v.as_bool()), Some(false));
    assert_eq!(declined.dict_get("warID"), Some(&PyRep::None));

    // Send invite, then both corps join same alliance before accept -> InvalidWarTarget & invite cleaned.
    call_wars(&ctx, &mut agg_sess, "mutualWarInviteMgr", "SendInviteByPlayer", &[PyRep::Integer(ally_corp)], None)
        .await
        .unwrap();
    ctx.db.with_mut_snapshot(|s| {
        s.corporations.get_mut(&agg_corp).unwrap().alliance_id = Some(99_600_777);
        s.corporations.get_mut(&ally_corp).unwrap().alliance_id = Some(99_600_777);
    });
    assert_eq!(
        call_wars(&ctx, &mut ally_sess, "mutualWarInviteMgr", "RespondToInviteByPlayer", &[PyRep::Integer(agg_corp), PyRep::Integer(1)], None)
            .await
            .unwrap_err()
            .code,
        "InvalidWarTarget"
    );

    // 7. `warRegistry.GetNegotiations` (`handle_get_negotiations`) and `warStatisticMgr.GetKills` / `GetKillsByGroup` (`handle_war_statistic_kills`)
    let neg_rows = call_wars(&ctx, &mut agg_sess, "warRegistry", "GetNegotiations", &[PyRep::Integer(war_id)], None)
        .await
        .unwrap();
    assert!(matches!(neg_rows, PyRep::Rowset { ref rows, .. } if !rows.is_empty()));

    let kills_by_grp = call_wars(&ctx, &mut agg_sess, "warStatisticMgr", "GetKillsByGroup", &[PyRep::Integer(war_id)], None)
        .await
        .unwrap();
    assert!(matches!(
        kills_by_grp,
        PyRep::Rowset { ref columns, ref rows }
            if columns.iter().map(String::as_str).eq(["groupID", "kills", "iskDestroyed"])
                && !rows.is_empty()
    ));

    let kills_rows = call_wars(&ctx, &mut agg_sess, "warStatisticMgr", "GetKills", &[PyRep::Integer(war_id)], None)
        .await
        .unwrap();
    assert!(matches!(kills_rows, PyRep::Rowset { ref rows, .. } if rows.len() == 1));

    let custom_war_crowset =
        crate::services::social_corp_fleet::social_wars::social_wars_helpers::war_crowset(vec![vec![
            PyRep::Integer(war_id),
            PyRep::Integer(agg_corp),
            PyRep::Integer(def_corp),
            PyRep::Integer(now as i64),
            PyRep::Integer(now as i64),
            PyRep::Integer(0),
            PyRep::Bool(false),
            PyRep::Bool(false),
            PyRep::Integer(0),
            PyRep::Bool(true),
            PyRep::Bool(true),
        ]]);
    assert!(matches!(custom_war_crowset, PyRep::ObjectEx { ref list, .. } if list.len() == 1));
}
