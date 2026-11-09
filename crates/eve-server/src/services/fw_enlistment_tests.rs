//! Unit tests for `fw_enlistment` (`fwCharacterEnlistmentMgr`).

#![forbid(unsafe_code)]

use super::*;
use crate::services::account_services::account_services_tests::{auth_session, test_ctx};
use eve_db::{corp_roles, CorporationRecord};
use eve_proto::PyRep;

#[test]
fn fw_enlistment_direct_join_leave_cooldown_and_corp_allowed_factions() {
    assert_eq!(unix_to_filetime(0), 116_444_736_000_000_000);

    let ctx = test_ctx();
    assert!(is_militia_faction(&ctx, 500_001));
    assert!(is_militia_faction(&ctx, 500_004));
    assert!(!is_militia_faction(&ctx, 999_999));

    let cid = 90_000_001_i64;
    let player_corp = 98_000_001_i64;
    let mut sess = auth_session(1, cid, player_corp, 30_000_142);

    let other_svc = "other";
    let unknown_method = "Unknown";
    assert!(handle(&ctx, &mut sess, other_svc, "GetMyEnlistment", &[]).is_none());
    assert!(handle(&ctx, &mut sess, "fwCharacterEnlistmentMgr", unknown_method, &[]).is_none());

    // Initial state: not enlisted
    assert_eq!(
        handle(&ctx, &mut sess, "fwCharacterEnlistmentMgr", "GetMyEnlistment", &[])
            .unwrap()
            .unwrap(),
        PyRep::Tuple(vec![PyRep::None, PyRep::None, PyRep::None])
    );

    // Ensure player_corp exists with cid as CEO and configure corp allowed militia factions
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.corporation_id = player_corp;
            ch.war_faction_id = None;
        }
        snap.corporations.insert(
            player_corp,
            { let mut __corp = CorporationRecord::default();
                __corp.corporation_id = player_corp;
                __corp.ceo_id = cid;
                __corp },
        );
    });

    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "fwCharacterEnlistmentMgr",
            "GetCorpAllowedEnlistmentFactions",
            &[PyRep::Integer(player_corp)],
            )
            .unwrap()
            .unwrap(),
        PyRep::List(Vec::new())
    );

    let set_allowed = handle(
        &ctx,
        &mut sess,
        "fwCharacterEnlistmentMgr",
        "SetMyCorpAllowedEnlistmentFactions",
        &[PyRep::List(vec![
            PyRep::Integer(500_004),
            PyRep::Integer(500_001),
            PyRep::Integer(500_001),
        ])],
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        set_allowed,
        PyRep::List(vec![PyRep::Integer(500_001), PyRep::Integer(500_004)])
    );

    // Non-director cannot set allowed factions
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.get_mut(&player_corp).unwrap().ceo_id = 90_000_002;
    });
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "fwCharacterEnlistmentMgr",
            "SetMyCorpAllowedEnlistmentFactions",
            &[PyRep::List(vec![PyRep::Integer(500_001)])],
        )
        .unwrap()
        .unwrap_err()
        .code,
        "CorpRoleDenied"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations
            .get_mut(&player_corp)
            .unwrap()
            .member_roles
            .insert(cid, corp_roles::ROLE_DIRECTOR);
    });

    // Enlistment blocked by corp allowed list (500_002 not in [500_001, 500_004])
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "fwCharacterEnlistmentMgr",
            "CreateMyDirectEnlistment",
            &[PyRep::Integer(500_002)],
        )
        .unwrap()
        .unwrap_err()
        .code,
        "FwFactionNotAllowedByCorp"
    );

    // Negative standing blocks enlistment
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters
            .get_mut(&cid)
            .unwrap()
            .standings
            .insert(500_001, -0.5);
    });
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "fwCharacterEnlistmentMgr",
            "CreateMyDirectEnlistment",
            &[PyRep::Integer(500_001)],
        )
        .unwrap()
        .unwrap_err()
        .code,
        "FwStandingTooLow"
    );

    // Valid direct enlistment
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters
            .get_mut(&cid)
            .unwrap()
            .standings
            .insert(500_001, 1.0);
    });
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "fwCharacterEnlistmentMgr",
            "CreateMyDirectEnlistment",
            &[PyRep::Integer(500_001)],
        )
        .unwrap()
        .unwrap(),
        PyRep::None
    );
    assert_eq!(sess.war_faction_id, Some(500_001));
    assert_eq!(
        handle(&ctx, &mut sess, "fwCharacterEnlistmentMgr", "GetMyEnlistment", &[])
            .unwrap()
            .unwrap(),
        PyRep::Tuple(vec![
            PyRep::Integer(500_001),
            PyRep::Integer(500_001),
            PyRep::None,
        ])
    );

    // Duplicate enlistment rejected
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "fwCharacterEnlistmentMgr",
            "CreateMyDirectEnlistment",
            &[PyRep::Integer(500_001)],
        )
        .unwrap()
        .unwrap_err()
        .code,
        "FwAlreadyEnlisted"
    );

    // Leave militia -> sets 24h cooldown
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "fwCharacterEnlistmentMgr",
            "RemoveMyDirectEnlistment",
            &[],
        )
        .unwrap()
        .unwrap(),
        PyRep::None
    );
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "fwCharacterEnlistmentMgr",
            "RemoveMyDirectEnlistment",
            &[],
        )
        .unwrap()
        .unwrap_err()
        .code,
        "FwNotEnlisted"
    );

    let cd_rep = handle(
        &ctx,
        &mut sess,
        "fwCharacterEnlistmentMgr",
        "GetMyDirectEnlistmentCooldownTimestamp",
        &[],
    )
    .unwrap()
    .unwrap();
    assert!(cd_rep.as_i64().unwrap() > 0);
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "fwCharacterEnlistmentMgr",
            "CreateMyDirectEnlistment",
            &[PyRep::Integer(500_001)],
        )
        .unwrap()
        .unwrap_err()
        .code,
        "FwEnlistmentCooldown"
    );

    // Expire cooldown and verify it clears
    ctx.db.with_mut_snapshot(|snap| {
        let mut st = load_store(snap);
        st.cooldown_until.insert(cid, 1);
        save_store(snap, &st);
    });
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "fwCharacterEnlistmentMgr",
            "GetMyDirectEnlistmentCooldownTimestamp",
            &[],
        )
        .unwrap()
        .unwrap(),
        PyRep::None
    );
}
