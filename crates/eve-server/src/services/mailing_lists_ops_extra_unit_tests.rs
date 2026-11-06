//! Deep branch-coverage and mutation-killing unit tests for `mailing_lists_ops` (`mailMgr` & `mailingListsMgr`).

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::fleet_social::fleet_social_persistence::{
    sync_mailing_lists_to_db, MailingListState,
};
use crate::services::fleet_social::handle;
use eve_db::clock::VirtualServerClock;
use eve_db::{IskCents, MailRecord, WorldRepository};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::SdeRegistry;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn make_ctx(base_unix: u64) -> ServerContext {
    let db = Arc::new(WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(shared_sde(), db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(base_unix)));
    ctx
}

fn char_sess(uid: u64, char_id: i64, corp_id: i64) -> SessionState {
    let mut s = SessionState::new(uid);
    s.user_id = Some(uid);
    s.character_id = Some(char_id);
    s.corporation_id = Some(corp_id);
    s.solar_system_id = Some(30_000_142);
    s.station_id = Some(60_003_760);
    s
}

fn call_mail_mgr(
    ctx: &ServerContext,
    sess: &mut SessionState,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let kw = HashMap::new();
    tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(handle(ctx, sess, "mailMgr", method, args, &kw))
        .expect("mailMgr handled")
}

fn call_ml_mgr(
    ctx: &ServerContext,
    sess: &mut SessionState,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(handle(ctx, sess, "mailingListsMgr", method, args, kwargs))
        .expect("mailingListsMgr handled")
}

#[test]
fn mail_mgr_list_vs_id_array_resolution_ownership_and_default_label_trashing() {
    let ctx = make_ctx(1_775_400_000);
    let char_a = 90_000_001_i64;
    let char_b = 90_000_002_i64;
    let mut sess_a = char_sess(1, char_a, 98_000_001);
    let mut unauth = SessionState::new(999);

    // 1. Unauthenticated rejected on mailMgr methods
    for m in [
        "MarkAsReadByList",
        "MarkAsUnreadByList",
        "MoveToTrash",
        "MoveToTrashByLabel",
        "MoveToTrashByList",
        "MoveAllToTrash",
    ] {
        assert!(call_mail_mgr(&ctx, &mut unauth, m, &[]).is_err());
    }

    // 2. Seed mails:
    // - 201: inbox mail to char_a (no explicit label -> defaults to label 1)
    // - 202: sent mail from char_a to char_b (no explicit label -> defaults to label 2 for char_a)
    // - 203: foreign mail between char_b and 90_000_003 (not owned by char_a)
    // - 204: deleted mail for char_a
    ctx.db.with_mut_snapshot(|snap| {
        snap.mails.push(MailRecord {
            message_id: 201,
            sender_id: char_b,
            recipient_id: char_a,
            subject: "Inbox Default".into(),
            body: "Hi".into(),
        });
        snap.mails.push(MailRecord {
            message_id: 202,
            sender_id: char_a,
            recipient_id: char_b,
            subject: "Sent Default".into(),
            body: "Yo".into(),
        });
        snap.mails.push(MailRecord {
            message_id: 203,
            sender_id: char_b,
            recipient_id: 90_000_003,
            subject: "Foreign".into(),
            body: "Secret".into(),
        });
        snap.mails.push(MailRecord {
            message_id: 204,
            sender_id: char_b,
            recipient_id: char_a,
            subject: "Deleted".into(),
            body: "Gone".into(),
        });
        snap.deleted_mails.insert((char_a, 204));
    });

    // 3. MoveToTrash rejects foreign mail with AccessDenied
    let err_foreign = call_mail_mgr(
        &ctx,
        &mut sess_a,
        "MoveToTrash",
        &[PyRep::Integer(203)], // single integer arg extracted via extract_id_list
    )
    .unwrap_err();
    assert_eq!(err_foreign.code, "AccessDenied");

    // 4. MarkAsReadByList / MarkAsUnreadByList with Tuple of message IDs (non-list-ID branch)
    let read_tup = call_mail_mgr(
        &ctx,
        &mut sess_a,
        "MarkAsReadByList",
        &[PyRep::Tuple(vec![
            PyRep::Integer(201),
            PyRep::Integer(202),
            PyRep::Integer(203), // filtered out (foreign)
            PyRep::Integer(204), // filtered out (deleted)
        ])],
    )
    .unwrap();
    assert_eq!(read_tup.dict_get("messageID"), Some(&PyRep::Integer(201)));
    assert_eq!(read_tup.dict_get("read"), Some(&PyRep::Bool(true)));
    assert!(ctx.esi_state.get_mail_meta(char_a, 201, false).is_read);
    assert!(ctx.esi_state.get_mail_meta(char_a, 202, false).is_read);

    // Unknown mailing list ID -> empty messageIDs, first_mid == 0
    let read_unknown_ml =
        call_mail_mgr(&ctx, &mut sess_a, "MarkAsReadByList", &[PyRep::Integer(999_999)]).unwrap();
    assert_eq!(
        read_unknown_ml.dict_get("messageID"),
        Some(&PyRep::Integer(0))
    );

    // 5. MoveToTrashByLabel with label 2 trashes sent mail (202) using default sent-label fallback
    let trash_sent =
        call_mail_mgr(&ctx, &mut sess_a, "MoveToTrashByLabel", &[PyRep::Integer(2)]).unwrap();
    assert_eq!(trash_sent.dict_get("trashed"), Some(&PyRep::Bool(true)));
    // Second call on same label 2 finds no untrashed mails -> trashed == false
    let trash_sent_again =
        call_mail_mgr(&ctx, &mut sess_a, "MoveToTrashByLabel", &[PyRep::Integer(2)]).unwrap();
    assert_eq!(
        trash_sent_again.dict_get("trashed"),
        Some(&PyRep::Bool(false))
    );

    // Default label (no args -> label 1) trashes inbox mail (201)
    let trash_inbox_default = call_mail_mgr(&ctx, &mut sess_a, "MoveToTrashByLabel", &[]).unwrap();
    assert_eq!(
        trash_inbox_default.dict_get("trashed"),
        Some(&PyRep::Bool(true))
    );
}

#[test]
fn mailing_lists_create_kwargs_join_by_name_fee_transfer_and_access_precedence() {
    let ctx = make_ctx(1_775_400_000);
    let owner_id = 90_000_001_i64;
    let joiner_id = 90_000_002_i64;
    let joiner_corp = 98_000_002_i64;
    let joiner_alliance = 99_000_002_i64;
    let mut owner_sess = char_sess(1, owner_id, 98_000_001);
    let mut joiner_sess = char_sess(2, joiner_id, joiner_corp);
    let kw_empty = HashMap::new();

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&owner_id) {
            ch.balance = IskCents::from_isk(1_000);
        }
        if let Some(ch) = snap.characters.get_mut(&joiner_id) {
            ch.corporation_id = joiner_corp;
            ch.alliance_id = Some(joiner_alliance);
            ch.balance = IskCents::from_isk(500);
        }
    });

    // 1. Create via kwargs with cost clamped to 1_000_000_000, then adjust via SetDefaultAccess to 1,000 ISK
    let mut create_kw = HashMap::new();
    create_kw.insert("name".into(), PyRep::String("Fleet Broadcasts".into()));
    create_kw.insert("defaultAccess".into(), PyRep::Integer(0)); // blocked by default
    create_kw.insert("defaultMemberAccess".into(), PyRep::Integer(0)); // muted by default
    create_kw.insert("mailCost".into(), PyRep::Integer(5_000_000_000)); // clamped to 1_000_000_000
    let lid = call_ml_mgr(&ctx, &mut owner_sess, "Create", &[], &create_kw)
        .unwrap()
        .as_i64()
        .unwrap();

    let info = call_ml_mgr(
        &ctx,
        &mut owner_sess,
        "GetInfo",
        &[PyRep::Integer(lid)],
        &kw_empty,
    )
    .unwrap();
    assert_eq!(info.dict_get("cost"), Some(&PyRep::Integer(1_000_000_000)));
    assert!(call_ml_mgr(
        &ctx,
        &mut owner_sess,
        "GetInfo",
        &[PyRep::Integer(999_999)],
        &kw_empty
    )
    .is_err());

    // Set cost to 1,000 ISK and save a welcome mail
    call_ml_mgr(
        &ctx,
        &mut owner_sess,
        "SaveSettings",
        &[
            PyRep::Integer(lid),
            PyRep::Integer(0),
            PyRep::Integer(0),
            PyRep::Integer(1_000),
        ],
        &kw_empty,
    )
    .unwrap();
    call_ml_mgr(
        &ctx,
        &mut owner_sess,
        "SaveWelcomeMail",
        &[
            PyRep::Integer(lid),
            PyRep::String("Welcome!".into()),
            PyRep::String("Fly safe".into()),
        ],
        &kw_empty,
    )
    .unwrap();

    // 2. Joiner blocked because defaultAccess == 0 and no entity_access entry
    assert!(call_ml_mgr(
        &ctx,
        &mut joiner_sess,
        "Join",
        &[PyRep::String("fleet broadcasts".into())],
        &kw_empty
    )
    .is_err());
    assert!(call_ml_mgr(
        &ctx,
        &mut joiner_sess,
        "GetMembers",
        &[PyRep::Integer(lid)],
        &kw_empty
    )
    .is_err());
    assert!(call_ml_mgr(
        &ctx,
        &mut joiner_sess,
        "GetWelcomeMail",
        &[PyRep::Integer(lid)],
        &kw_empty
    )
    .is_err());
    assert!(call_ml_mgr(
        &ctx,
        &mut joiner_sess,
        "GetSettings",
        &[PyRep::Integer(lid)],
        &kw_empty
    )
    .is_err());

    // 3. Grant alliance access (level 1) via Dict SetEntitiesAccess -> joiner can now pass access check,
    //    but fails join fee (has 500 ISK < 1,000 ISK cost) with NotEnoughISK!
    call_ml_mgr(
        &ctx,
        &mut owner_sess,
        "SetEntitiesAccess",
        &[
            PyRep::Integer(lid),
            PyRep::Dict(vec![
                (PyRep::Integer(-1), PyRep::Integer(2)), // ignored (eid <= 0)
                (PyRep::Integer(joiner_alliance), PyRep::Integer(99)), // clamped to 2
            ]),
        ],
        &kw_empty,
    )
    .unwrap();

    let err_isk = call_ml_mgr(
        &ctx,
        &mut joiner_sess,
        "Join",
        &[PyRep::String("FLEET BROADCASTS".into())], // case-insensitive name lookup
        &kw_empty,
    )
    .unwrap_err();
    assert_eq!(err_isk.code, "NotEnoughISK");

    // 4. Fund joiner with 1,500 ISK -> Join succeeds, transfers 1,000 ISK to owner, delivers welcome mail
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&joiner_id).unwrap().balance = IskCents::from_isk(1_500);
    });
    let join_rep = call_ml_mgr(
        &ctx,
        &mut joiner_sess,
        "Join",
        &[PyRep::String("fleet broadcasts".into())],
        &kw_empty,
    )
    .unwrap();
    assert_eq!(join_rep.dict_get("isMuted"), Some(&PyRep::Bool(true)));
    assert_eq!(join_rep.dict_get("access"), Some(&PyRep::Integer(2)));

    // Re-joining when already a member does not deduct fee a second time
    call_ml_mgr(
        &ctx,
        &mut joiner_sess,
        "Join",
        &[PyRep::Integer(lid)],
        &kw_empty,
    )
    .unwrap();

    ctx.db.with_snapshot(|snap| {
        assert_eq!(
            snap.characters[&joiner_id].balance,
            IskCents::from_isk(500)
        );
        assert_eq!(
            snap.characters[&owner_id].balance,
            IskCents::from_isk(2_000)
        );
        assert!(snap
            .mails
            .iter()
            .any(|m| m.recipient_id == joiner_id && m.subject == "Welcome!"));
    });

    // GetJoinedLists reflects muted role and alliance-derived access
    let joined = call_ml_mgr(&ctx, &mut joiner_sess, "GetJoinedLists", &[], &kw_empty).unwrap();
    let PyRep::Dict(j_entries) = joined else {
        panic!("expected dict");
    };
    assert_eq!(j_entries.len(), 1);
    assert_eq!(
        j_entries[0].1.dict_get("isMuted"),
        Some(&PyRep::Bool(true))
    );

    // Leave removes joiner
    assert_eq!(
        call_ml_mgr(
            &ctx,
            &mut joiner_sess,
            "Leave",
            &[PyRep::Integer(lid)],
            &kw_empty
        )
        .unwrap(),
        PyRep::Bool(true)
    );
}

#[test]
fn mailing_lists_operator_hierarchy_protections_and_permission_gates() {
    let ctx = make_ctx(1_775_400_000);
    let owner_id = 90_000_001_i64;
    let op_a = 90_000_002_i64;
    let op_b = 90_000_003_i64;
    let regular = 90_000_004_i64;

    let mut owner_sess = char_sess(1, owner_id, 98_000_001);
    let mut op_a_sess = char_sess(2, op_a, 98_000_001);
    let mut reg_sess = char_sess(4, regular, 98_000_001);
    let kw = HashMap::new();

    let lid = 888_i64;
    {
        let mut s = ctx.social_extras.lock().unwrap();
        s.mailing_lists.insert(
            lid,
            MailingListState {
                list_id: lid,
                name: "Hierarchy List".into(),
                owner_id,
                default_access: 1,
                default_role: 1,
                cost: 0,
                members: [
                    (owner_id, 3),
                    (op_a, 2),
                    (op_b, 2),
                    (regular, 1),
                ]
                .into_iter()
                .collect(),
                entity_access: Default::default(),
                welcome_mail: None,
                mail_ids: Default::default(),
            },
        );
    }
    sync_mailing_lists_to_db(&ctx);

    // 1. Regular member (role 1) is rejected on all operator/owner management RPCs
    for method in [
        "ClearEntityAccess",
        "ClearWelcomeMail",
        "SaveWelcomeMail",
        "SendWelcomeMail",
        "SetDefaultAccess",
        "SetEntitiesAccess",
        "SetMembersMuted",
        "SetMembersClear",
        "SetMembersOperator",
        "KickMembers",
        "Delete",
    ] {
        assert!(
            call_ml_mgr(&ctx, &mut reg_sess, method, &[PyRep::Integer(lid)], &kw).is_err(),
            "regular member should be denied on {method}"
        );
    }

    // 2. SendWelcomeMail missing title/body -> InvalidArgument
    let err_arg = call_ml_mgr(
        &ctx,
        &mut op_a_sess,
        "SendWelcomeMail",
        &[PyRep::Integer(lid)],
        &kw,
    )
    .unwrap_err();
    assert_eq!(err_arg.code, "InvalidArgument");

    // SetEntityAccess with invalid entity_id <= 0 -> UserError
    assert!(call_ml_mgr(
        &ctx,
        &mut op_a_sess,
        "SetEntityAccess",
        &[PyRep::Integer(lid), PyRep::Integer(0), PyRep::Integer(1)],
        &kw
    )
    .is_err());

    // 3. Operator (op_a) CANNOT mute, clear, or kick owner_id or fellow operator (op_b),
    //    and CANNOT Delete the mailing list!
    call_ml_mgr(
        &ctx,
        &mut op_a_sess,
        "SetMembersMuted",
        &[
            PyRep::Integer(lid),
            PyRep::List(vec![
                PyRep::Integer(owner_id),
                PyRep::Integer(op_b),
                PyRep::Integer(regular),
            ]),
        ],
        &kw,
    )
    .unwrap();
    call_ml_mgr(
        &ctx,
        &mut op_a_sess,
        "KickMembers",
        &[
            PyRep::Integer(lid),
            PyRep::List(vec![PyRep::Integer(owner_id), PyRep::Integer(op_b)]),
        ],
        &kw,
    )
    .unwrap();
    assert!(call_ml_mgr(&ctx, &mut op_a_sess, "Delete", &[PyRep::Integer(lid)], &kw).is_err());

    {
        let s = ctx.social_extras.lock().unwrap();
        let ml = &s.mailing_lists[&lid];
        assert_eq!(ml.members.get(&owner_id), Some(&3)); // owner untouched
        assert_eq!(ml.members.get(&op_b), Some(&2)); // fellow operator untouched
        assert_eq!(ml.members.get(&regular), Some(&0)); // regular member muted
    }

    // 4. Owner CAN demote/mute and kick operators, but still cannot kick or demote themselves
    call_ml_mgr(
        &ctx,
        &mut owner_sess,
        "SetMembersClear",
        &[
            PyRep::Integer(lid),
            PyRep::List(vec![PyRep::Integer(owner_id), PyRep::Integer(op_b)]),
        ],
        &kw,
    )
    .unwrap();
    {
        let s = ctx.social_extras.lock().unwrap();
        let ml = &s.mailing_lists[&lid];
        assert_eq!(ml.members.get(&owner_id), Some(&3));
        assert_eq!(ml.members.get(&op_b), Some(&1)); // op_b demoted to clear (1) by owner
    }
    call_ml_mgr(
        &ctx,
        &mut owner_sess,
        "KickMembers",
        &[
            PyRep::Integer(lid),
            PyRep::List(vec![PyRep::Integer(owner_id), PyRep::Integer(op_a)]),
        ],
        &kw,
    )
    .unwrap();
    {
        let s = ctx.social_extras.lock().unwrap();
        let ml = &s.mailing_lists[&lid];
        assert!(ml.members.contains_key(&owner_id));
        assert!(!ml.members.contains_key(&op_a));
    }
}
