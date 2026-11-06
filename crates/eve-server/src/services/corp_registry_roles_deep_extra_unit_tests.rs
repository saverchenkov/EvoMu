//! Deep branch-coverage and mutation-killing unit tests for corporation registry,
//! role bitmasks, hangar divisions, share/dividend governance, votes, and member lifecycle.

#![forbid(unsafe_code)]

use crate::router::{BoundObjectEntry, ServerContext};
use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::{
    make_fighter_ctx_and_session, make_fighter_item,
};
use crate::services::corp_registry_ops::{handle as handle_corp_reg, handle_corp_hangar_invbroker};
use crate::services::fleet_social::{
    execute_pending_auto_kicks_for_docked_char, execute_pending_auto_kicks_on_downtime,
    handle as handle_social,
};
use eve_db::{corp_roles, CorporationRecord, InventoryFlag, IskCents, WarRecord};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::HashMap;

async fn social_call(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    handle_social(ctx, sess, svc, method, args, &HashMap::new())
        .await
        .expect("handled")
}

fn reg_call(
    ctx: &ServerContext,
    sess: &mut SessionState,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    handle_corp_reg(ctx, sess, "corpRegistry", method, args, &HashMap::new()).expect("handled")
}

fn setup_corp_env() -> (ServerContext, SessionState, i64, i64, i64, i64) {
    let (ctx, mut ceo_sess, ceo_id, _ship_id, _sys_id) = make_fighter_ctx_and_session();
    let corp_id = 98_200_100_i64;
    let member_id = 90_000_002_i64;
    let third_id = 90_000_003_i64;
    let station_id = 60_003_760_i64;
    ceo_sess.corporation_id = Some(corp_id);
    ceo_sess.station_id = Some(station_id);
    ceo_sess.corp_role = u64::MAX;

    ctx.db.with_mut_snapshot(|snap| {
        let base_char = snap.characters.values().next().expect("demo char").clone();
        for &cid in &[ceo_id, member_id, third_id] {
            let mut ch = snap.characters.get(&cid).cloned().unwrap_or_else(|| {
                let mut c = base_char.clone();
                c.character_id = cid;
                c.user_id = cid as u64;
                c.name = format!("Pilot {cid}");
                c
            });
            ch.corporation_id = corp_id;
            ch.station_id = Some(station_id);
            ch.structure_id = None;
            ch.balance = IskCents::from_isk(1_000_000);
            snap.characters.insert(cid, ch);
        }
        let mut corp = CorporationRecord {
            corporation_id: corp_id,
            name: "Deep Coverage Corp".into(),
            ticker: "DCVR".into(),
            ceo_id,
            master_wallet: IskCents::from_isk(100_000_000),
            members: [ceo_id, member_id, third_id].into_iter().collect(),
            member_roles: [(ceo_id, u64::MAX), (member_id, 0), (third_id, 0)]
                .into_iter()
                .collect(),
            hq_station_id: Some(station_id),
            tax_rate: 0.05,
            ..Default::default()
        };
        corp.ensure_wallet_divisions();
        for div in &mut corp.wallet_divisions {
            div.balance = IskCents::from_isk(10_000_000);
        }
        snap.corporations.insert(corp_id, corp);
        let gov = snap.corp_governance.entry(corp_id).or_default();
        gov.shares.insert(corp_id, 1_000);
        gov.company_shares = 1_000;
    });
    (ctx, ceo_sess, ceo_id, member_id, third_id, corp_id)
}

fn make_member_sess(char_id: i64, corp_id: i64, station_id: Option<i64>, role: u64) -> SessionState {
    let mut s = SessionState::new(7700 + (char_id as u64 % 1000));
    s.user_id = Some(char_id as u64);
    s.character_id = Some(char_id);
    s.corporation_id = Some(corp_id);
    s.station_id = station_id;
    s.solar_system_id = Some(30_000_142);
    s.corp_role = role;
    s
}

#[tokio::test]
async fn corp_hangar_divisions_account_keys_and_delayed_role_grants_kill_mutations() {
    let (ctx, mut ceo_sess, ceo_id, member_id, _third_id, corp_id) = setup_corp_env();
    let station_id = 60_003_760_i64;
    let kw = HashMap::new();

    // Test all 7 hangar division flags (116..=122): query role = 1 << (20 + div_idx), take role = 1 << (13 + div_idx)
    let bound_member = BoundObjectEntry {
        bound_id: 10,
        session_id: 1,
        user_id: member_id as u64,
        character_id: member_id,
        service: "invbroker".into(),
        object_id: station_id,
        oid_str: "oid-10".into(),
        ref_id: 1,
    };
    for div_idx in 0..7u64 {
        let flag = 116 + div_idx as i64;
        let query_bit = 1u64 << (20 + div_idx);
        let take_bit = 1u64 << (13 + div_idx);
        let wrong_flag = 116 + ((div_idx + 1) % 7) as i64;

        ctx.db.with_mut_snapshot(|snap| {
            snap.corporations
                .get_mut(&corp_id)
                .unwrap()
                .member_roles
                .insert(member_id, query_bit);
        });
        let mut mem_sess = make_member_sess(member_id, corp_id, Some(station_id), query_bit);

        let list_ok = handle_corp_hangar_invbroker(
            &ctx,
            &mut mem_sess,
            Some(&bound_member),
            "invbroker",
            "List",
            &[PyRep::Integer(flag)],
            &kw,
        )
        .await
        .unwrap();
        assert!(list_ok.is_ok(), "div_idx={div_idx} query_bit should allow List");

        let list_err = handle_corp_hangar_invbroker(
            &ctx,
            &mut mem_sess,
            Some(&bound_member),
            "invbroker",
            "List",
            &[PyRep::Integer(wrong_flag)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err();
        assert_eq!(list_err.code, "CorpRoleDenied");

        let item_id = 950_100 + div_idx as i64;
        let div_flag = InventoryFlag::from_u16(flag as u16);
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                item_id,
                make_fighter_item(item_id, 34, corp_id, station_id, div_flag, 50),
            );
        });
        let add_args = [
            PyRep::Integer(item_id),
            PyRep::Integer(station_id),
            PyRep::Integer(4),
        ];
        let add_denied = handle_corp_hangar_invbroker(
            &ctx,
            &mut mem_sess,
            Some(&bound_member),
            "invbroker",
            "Add",
            &add_args,
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err();
        assert_eq!(add_denied.code, "CorpRoleDenied");

        ctx.db.with_mut_snapshot(|snap| {
            snap.corporations
                .get_mut(&corp_id)
                .unwrap()
                .member_roles
                .insert(member_id, query_bit | take_bit);
        });
        mem_sess.corp_role = query_bit | take_bit;
        let add_gate = handle_corp_hangar_invbroker(
            &ctx,
            &mut mem_sess,
            Some(&bound_member),
            "invbroker",
            "Add",
            &add_args,
            &kw,
        )
        .await;
        assert!(add_gate.is_none(), "div_idx={div_idx} take_bit should pass corp hangar gate");
    }

    // BoundObjectOwnershipViolation & InvalidLocation checks
    let mut wrong_sess = make_member_sess(ceo_id, corp_id, None, u64::MAX);
    wrong_sess.ship_id = None;
    let own_err = handle_corp_hangar_invbroker(
        &ctx,
        &mut wrong_sess,
        Some(&bound_member),
        "invbroker",
        "List",
        &[PyRep::Integer(116)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(own_err.code, "BoundObjectOwnershipViolation");

    wrong_sess.station_id = None;
    wrong_sess.structure_id = None;
    wrong_sess.ship_id = None;
    let loc_err = handle_corp_hangar_invbroker(
        &ctx,
        &mut wrong_sess,
        None,
        "invbroker",
        "List",
        &[PyRep::Integer(116)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(loc_err.code, "InvalidLocation");

    // SetAccountKey boundary (999 invalid, 1007 invalid, 1000..=1006 valid with ROLE_ACCOUNT_CAN_TAKE 1 << (div_idx + 27))
    assert_eq!(
        reg_call(&ctx, &mut ceo_sess, "SetAccountKey", &[PyRep::Integer(999)]).unwrap_err().code,
        "InvalidAccountKey"
    );
    assert_eq!(
        reg_call(&ctx, &mut ceo_sess, "SetAccountKey", &[PyRep::Integer(1007)]).unwrap_err().code,
        "InvalidAccountKey"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.get_mut(&corp_id).unwrap().member_roles.insert(member_id, 1u64 << 30);
    });
    let mut mem_sess = make_member_sess(member_id, corp_id, Some(station_id), 1u64 << 30);
    assert_eq!(
        reg_call(&ctx, &mut mem_sess, "SetAccountKey", &[PyRep::Integer(1002)]).unwrap_err().code,
        "CorpRoleDenied"
    );
    assert!(reg_call(&ctx, &mut mem_sess, "SetAccountKey", &[PyRep::Integer(1003)]).is_ok());
    assert_eq!(mem_sess.corp_account_key, 1003);

    // ExecuteActions: 24h delayed division roles vs immediate roles
    let immediate_bit = corp_roles::ROLE_PERSONNEL_MANAGER;
    let delayed_bit = corp_roles::ROLE_HANGAR_CAN_TAKE[0] | corp_roles::ROLE_HANGAR_CAN_QUERY[0];
    let combined = (immediate_bit | delayed_bit) as i64;
    let action_res = reg_call(
        &ctx,
        &mut ceo_sess,
        "ExecuteActions",
        &[
            PyRep::List(vec![PyRep::Integer(member_id)]),
            PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::String("set".into()),
                PyRep::String("roles".into()),
                PyRep::Integer(combined),
            ])]),
        ],
    )
    .unwrap();
    assert_eq!(action_res, PyRep::Bool(true));
    let (eff_now, pending_full) = ctx.db.with_snapshot(|snap| {
        let eff = snap.corporations.get(&corp_id).unwrap().member_roles[&member_id];
        let pend = snap.corp_governance.get(&corp_id).unwrap().pending_role_grants[&member_id];
        (eff, pend)
    });
    assert_eq!(eff_now, immediate_bit);
    assert_eq!(pending_full.0, immediate_bit | delayed_bit);

    // Non-CEO Director cannot grant or strip ROLE_DIRECTOR (CrpAccessDenied)
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations
            .get_mut(&corp_id)
            .unwrap()
            .member_roles
            .insert(member_id, corp_roles::ROLE_DIRECTOR);
    });
    let mut dir_sess = make_member_sess(member_id, corp_id, Some(station_id), corp_roles::ROLE_DIRECTOR);
    let dir_err = reg_call(
        &ctx,
        &mut dir_sess,
        "ExecuteActions",
        &[
            PyRep::List(vec![PyRep::Integer(90_000_003)]),
            PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::String("add".into()),
                PyRep::String("roles".into()),
                PyRep::Integer(corp_roles::ROLE_DIRECTOR as i64),
            ])]),
        ],
    )
    .unwrap_err();
    assert!(dir_err.message.contains("CrpAccessDenied"));
}

#[tokio::test]
async fn corp_shares_dividends_titles_and_sanctioned_votes_kill_mutations() {
    let (ctx, mut ceo_sess, ceo_id, member_id, third_id, corp_id) = setup_corp_env();
    let station_id = 60_003_760_i64;

    let mut mem_sess = make_member_sess(member_id, corp_id, Some(station_id), 0);
    assert_eq!(
        social_call(&ctx, &mut mem_sess, "corpRegistry", "MoveCompanyShares", &[PyRep::Integer(corp_id), PyRep::Integer(member_id), PyRep::Integer(100)]).await.unwrap_err().code,
        "CorpRoleDenied"
    );
    assert_eq!(
        social_call(&ctx, &mut ceo_sess, "corpRegistry", "MoveCompanyShares", &[PyRep::Integer(corp_id), PyRep::Integer(member_id), PyRep::Integer(0)]).await.unwrap_err().code,
        "InvalidArgument"
    );
    assert_eq!(
        social_call(&ctx, &mut ceo_sess, "corpRegistry", "MoveCompanyShares", &[PyRep::Integer(corp_id), PyRep::Integer(member_id), PyRep::Integer(1001)]).await.unwrap_err().code,
        "InsufficientShares"
    );

    // Move 600 shares to ceo_id and 400 shares to member_id, then 100 private shares to third_id
    social_call(&ctx, &mut ceo_sess, "corpRegistry", "MoveCompanyShares", &[PyRep::Integer(corp_id), PyRep::Integer(ceo_id), PyRep::Integer(600)]).await.unwrap();
    social_call(&ctx, &mut ceo_sess, "corpRegistry", "MoveCompanyShares", &[PyRep::Integer(corp_id), PyRep::Integer(member_id), PyRep::Integer(400)]).await.unwrap();
    social_call(&ctx, &mut mem_sess, "corpRegistry", "MovePrivateShares", &[PyRep::Integer(corp_id), PyRep::Integer(third_id), PyRep::Integer(100)]).await.unwrap();

    let above_avg = social_call(&ctx, &mut ceo_sess, "corpRegistry", "GetMemberIDsWithMoreThanAvgShares", &[PyRep::Integer(corp_id)]).await.unwrap();
    assert_eq!(above_avg, PyRep::List(vec![PyRep::Integer(ceo_id)]));

    // PayoutDividend to shareholders (600 / 300 / 100 out of 1000) from division 2 (account_key 1001)
    let (b_ceo, b_mem, b_third) = ctx.db.with_snapshot(|s| {
        (
            s.characters[&ceo_id].balance,
            s.characters[&member_id].balance,
            s.characters[&third_id].balance,
        )
    });
    let div_res = social_call(
        &ctx,
        &mut ceo_sess,
        "corpRegistry",
        "PayoutDividend",
        &[PyRep::Real(1_000_000.0), PyRep::Bool(true), PyRep::Integer(2)],
    )
    .await
    .unwrap();
    assert_eq!(div_res.dict_get("recipientCount").and_then(|v| v.as_i64()), Some(3));
    let (a_ceo, a_mem, a_third, div2_bal) = ctx.db.with_snapshot(|s| {
        (
            s.characters[&ceo_id].balance,
            s.characters[&member_id].balance,
            s.characters[&third_id].balance,
            s.corporations[&corp_id].wallet_divisions[1].balance,
        )
    });
    assert_eq!(a_ceo.0 - b_ceo.0, IskCents::from_isk(600_000).0);
    assert_eq!(a_mem.0 - b_mem.0, IskCents::from_isk(300_000).0);
    assert_eq!(a_third.0 - b_third.0, IskCents::from_isk(100_000).0);
    assert_eq!(div2_bal, IskCents::from_isk(9_000_000));

    // UpdateTitle & DeleteTitle role checks
    assert_eq!(
        social_call(
            &ctx,
            &mut mem_sess,
            "corpRegistry",
            "UpdateTitle",
            &[PyRep::Integer(2), PyRep::String("Bad".into()), PyRep::Integer(0)],
        )
        .await
        .unwrap_err()
        .code,
        "CorpRoleDenied"
    );
    social_call(
        &ctx,
        &mut ceo_sess,
        "corpRegistry",
        "UpdateTitle",
        &[PyRep::Integer(2), PyRep::String("Fleet Officer".into()), PyRep::Integer(1024)],
    )
    .await
    .unwrap();
    social_call(&ctx, &mut ceo_sess, "corpRegistry", "DeleteTitle", &[PyRep::Integer(2)])
        .await
        .unwrap();

    // Vote type 5 (Share creation) and Vote type 0 (CEO election)
    let share_vote = social_call(
        &ctx,
        &mut ceo_sess,
        "voteManager",
        "InsertVoteCase",
        &[
            PyRep::String("Create 500 Shares".into()),
            PyRep::String("Expand equity".into()),
            PyRep::Integer(5),
            PyRep::Integer(500),
        ],
    )
    .await
    .unwrap();
    let share_case_id = share_vote.dict_get("voteCaseID").and_then(|v| v.as_i64()).unwrap();
    social_call(
        &ctx,
        &mut ceo_sess,
        "voteManager",
        "InsertVote",
        &[PyRep::Integer(corp_id), PyRep::Integer(share_case_id), PyRep::Integer(1)],
    )
    .await
    .unwrap();
    assert_eq!(
        social_call(&ctx, &mut ceo_sess, "voteManager", "ActivateSanctionedAction", &[PyRep::Integer(share_case_id)])
            .await
            .unwrap_err()
            .code,
        "VoteStillOpen"
    );
    ctx.db.with_mut_snapshot(|s| {
        s.corp_vote_cases.get_mut(&share_case_id).unwrap().end_ts = 0;
    });
    social_call(&ctx, &mut ceo_sess, "voteManager", "ActivateSanctionedAction", &[PyRep::Integer(share_case_id)])
        .await
        .unwrap();
    assert_eq!(ctx.db.with_snapshot(|s| s.corp_governance[&corp_id].shares[&corp_id]), 500);

    let ceo_vote = social_call(
        &ctx,
        &mut mem_sess,
        "voteManager",
        "InsertVoteCase",
        &[
            PyRep::String("Elect New CEO".into()),
            PyRep::String("Change leadership".into()),
            PyRep::Integer(0),
            PyRep::Integer(member_id),
        ],
    )
    .await
    .unwrap();
    let ceo_case_id = ceo_vote.dict_get("voteCaseID").and_then(|v| v.as_i64()).unwrap();
    social_call(
        &ctx,
        &mut ceo_sess,
        "voteManager",
        "InsertVote",
        &[PyRep::Integer(corp_id), PyRep::Integer(ceo_case_id), PyRep::Integer(1)],
    )
    .await
    .unwrap();
    ctx.db.with_mut_snapshot(|s| {
        s.corp_vote_cases.get_mut(&ceo_case_id).unwrap().end_ts = 0;
    });
    social_call(&ctx, &mut ceo_sess, "voteManager", "ActivateSanctionedAction", &[PyRep::Integer(ceo_case_id)])
        .await
        .unwrap();
    assert_eq!(ctx.db.with_snapshot(|s| s.corporations[&corp_id].ceo_id), member_id);
}

#[tokio::test]
async fn corp_kick_blockers_auto_kick_queue_and_war_allow_kill_mutations() {
    let (ctx, mut ceo_sess, _ceo_id, member_id, third_id, corp_id) = setup_corp_env();
    let station_id = 60_003_760_i64;

    let upd = social_call(
        &ctx,
        &mut ceo_sess,
        "corpRegistry",
        "UpdateCorporation",
        &[
            PyRep::String("Updated Desc".into()),
            PyRep::String("https://evomu.test".into()),
            PyRep::Real(0.10),
            PyRep::Bool(false),
        ],
    )
    .await
    .unwrap();
    assert_eq!(upd.dict_get("description"), Some(&PyRep::String("Updated Desc".into())));
    let corp_info = social_call(&ctx, &mut ceo_sess, "corpRegistry", "GetCorporation", &[PyRep::Integer(corp_id)])
        .await
        .unwrap();
    assert_eq!(corp_info.dict_get("allowWar"), Some(&PyRep::Bool(false)));

    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations
            .get_mut(&corp_id)
            .unwrap()
            .member_roles
            .insert(member_id, corp_roles::ROLE_JUNIOR_ACCOUNTANT);
    });
    let mut mem_sess = make_member_sess(member_id, corp_id, Some(station_id), corp_roles::ROLE_JUNIOR_ACCOUNTANT);
    let can_kick = social_call(&ctx, &mut ceo_sess, "corpRegistry", "CanBeKickedOut", &[PyRep::Integer(member_id)])
        .await
        .unwrap();
    assert_eq!(can_kick, PyRep::Bool(false));
    assert_eq!(
        social_call(&ctx, &mut ceo_sess, "corpRegistry", "KickOutMember", &[PyRep::Integer(member_id)])
            .await
            .unwrap_err()
            .code,
        "MemberHasRoles"
    );

    let now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.get_mut(&corp_id).unwrap().member_roles.insert(member_id, 0);
        snap.corp_governance
            .entry(corp_id)
            .or_default()
            .role_drop_timestamps
            .insert(member_id, now - 3600);
    });
    assert_eq!(
        social_call(&ctx, &mut ceo_sess, "corpRegistry", "KickOutMember", &[PyRep::Integer(member_id)])
            .await
            .unwrap_err()
            .code,
        "RoleDropCooldownActive"
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.corp_governance
            .entry(corp_id)
            .or_default()
            .role_drop_timestamps
            .insert(member_id, now - 90_000);
        snap.characters.get_mut(&member_id).unwrap().station_id = None;
        snap.characters.get_mut(&third_id).unwrap().station_id = None;
    });
    let kick_space = social_call(
        &ctx,
        &mut ceo_sess,
        "corpRegistry",
        "KickOutMembers",
        &[PyRep::List(vec![PyRep::Integer(member_id), PyRep::Integer(third_id)])],
    )
    .await
    .unwrap();
    assert_eq!(kick_space.dict_get("queuedAutoKick"), Some(&PyRep::Bool(true)));
    assert_eq!(kick_space.dict_get("removed"), Some(&PyRep::Bool(false)));

    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&member_id).unwrap().station_id = Some(station_id);
    });
    execute_pending_auto_kicks_for_docked_char(&ctx, &mut mem_sess, member_id);
    assert!(!ctx.db.with_snapshot(|s| s.corporations[&corp_id].members.contains(&member_id)));

    let dt_kicked = execute_pending_auto_kicks_on_downtime(&ctx);
    assert_eq!(dt_kicked, 1);
    assert!(!ctx.db.with_snapshot(|s| s.corporations[&corp_id].members.contains(&third_id)));

    ctx.db.with_mut_snapshot(|snap| {
        snap.wars.insert(
            701,
            WarRecord {
                war_id: 701,
                aggressor_corp_id: corp_id,
                defender_corp_id: 98_999_999,
                mutual: false,
                started_unix: now - 5_000,
                active: true,
            },
        );
    });
    assert_eq!(
        social_call(&ctx, &mut ceo_sess, "corpRegistry", "KickOutMember", &[])
            .await
            .unwrap_err()
            .code,
        "ActiveWarsPreventClosure"
    );
}
