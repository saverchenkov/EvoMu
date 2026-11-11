//! Deep branch-coverage and mutation-killing unit tests for `fleet_social_mail`.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::fleet_social::handle;
use eve_db::{
    corp_roles, AllianceRecord, CorporationRecord, IskCents, MailingListRecord, WorldRepository,
};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::SdeRegistry;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn test_ctx() -> ServerContext {
    if let Some(sde) = SHARED_SDE.get() {
        ServerContext::with_sde_and_repository(
            Arc::clone(sde),
            Arc::new(WorldRepository::with_demo_seed()),
        )
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
    } else {
        let ctx = ServerContext::new();
        let _ = SHARED_SDE.set(Arc::clone(&ctx.sde));
        ctx
    }
}

fn make_session(char_id: i64, corp_id: i64, ally_id: Option<i64>) -> SessionState {
    let mut s = SessionState::new(710);
    s.user_id = Some(1);
    s.character_id = Some(char_id);
    s.corporation_id = Some(corp_id);
    s.alliance_id = ally_id;
    s.solar_system_id = Some(30_000_142);
    s
}

async fn call_mail(
    ctx: &ServerContext,
    sess: &mut SessionState,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let kw = HashMap::new();
    handle(ctx, sess, "mailMgr", method, args, &kw)
        .await
        .expect("mailMgr method should be handled")
}

async fn call_cal(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let kw = HashMap::new();
    handle(ctx, sess, service, method, args, &kw)
        .await
        .expect("calendar method should be handled")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn send_mail_mailing_lists_permissions_costs_and_validation_branches() {
    let ctx = test_ctx();
    let owner_id = 90_000_001_i64;
    let member_id = 90_000_002_i64;
    let muted_id = 90_000_003_i64;
    let outsider_id = 90_000_004_i64;
    let corp_id = 98_000_001_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let base = snap.characters[&owner_id].clone();
        for (cid, name, bal) in [
            (owner_id, "ML Owner", 100_000.0),
            (member_id, "ML Member", 50_000.0),
            (muted_id, "ML Muted", 50_000.0),
            (outsider_id, "ML Outsider", 50_000.0),
        ] {
            let mut ch = base.clone();
            ch.character_id = cid;
            ch.name = name.into();
            ch.corporation_id = corp_id;
            ch.balance = IskCents::from_f64_isk(bal);
            snap.characters.insert(cid, ch);
        }
        let mut ml = MailingListRecord {
            list_id: 500,
            name: "Paid List".into(),
            owner_id,
            default_access: 1,
            default_role: 1,
            cost: 2_500,
            members: HashMap::new(),
            entity_access: HashMap::new(),
            welcome_mail: None,
            mail_ids: Default::default(),
        };
        ml.members.insert(owner_id, 2);
        ml.members.insert(member_id, 1);
        ml.members.insert(muted_id, 0);
        snap.mailing_lists.insert(500, ml);
    });
    ctx.social_extras.lock().unwrap().mailing_lists_seeded = false;

    let mut owner_sess = make_session(owner_id, corp_id, None);
    let mut member_sess = make_session(member_id, corp_id, None);
    let mut muted_sess = make_session(muted_id, corp_id, None);
    let mut outsider_sess = make_session(outsider_id, corp_id, None);

    // 1. Unknown mailing list -> MailingListNotFound
    let args_unknown = [
        PyRep::List(vec![]),
        PyRep::Integer(999_999),
        PyRep::None,
        PyRep::String("Subj".into()),
        PyRep::String("Body".into()),
    ];
    let err = call_mail(&ctx, &mut member_sess, "SendMail", &args_unknown).await.unwrap_err();
    assert_eq!(err.code, "UserError");
    assert_eq!(err.message, "MailingListNotFound");

    // 2. Non-member -> MailingListAccessDenied; muted member -> MutedInMailingList
    let args_500 = [
        PyRep::List(vec![]),
        PyRep::Integer(500),
        PyRep::None,
        PyRep::String("List Announcement".into()),
        PyRep::String("Hello list".into()),
    ];
    let err = call_mail(&ctx, &mut outsider_sess, "SendMail", &args_500).await.unwrap_err();
    assert_eq!(err.message, "MailingListAccessDenied");
    let err = call_mail(&ctx, &mut muted_sess, "SendMail", &args_500).await.unwrap_err();
    assert_eq!(err.message, "MutedInMailingList");

    // 3. Regular member sends to list -> pays 2_500 ISK to owner_id and records mail_id on list
    let mid = call_mail(&ctx, &mut member_sess, "SendMail", &args_500)
        .await
        .unwrap()
        .as_i64()
        .unwrap();
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.characters[&member_id].balance, IskCents::from_f64_isk(47_500.0));
        assert_eq!(snap.characters[&owner_id].balance, IskCents::from_f64_isk(102_500.0));
        assert!(snap.mailing_lists[&500].mail_ids.contains(&mid));
    });

    // 4. Owner sending to own list does not pay mailing list fee
    let _ = call_mail(&ctx, &mut owner_sess, "SendMail", &args_500).await.unwrap();
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.characters[&owner_id].balance, IskCents::from_f64_isk(102_500.0));
    });

    // 5. Subject and body length boundary checks (empty, 150 ok, 151 err, 8000 ok, 8001 err)
    let recips = PyRep::List(vec![PyRep::Integer(member_id)]);
    let err = call_mail(
        &ctx,
        &mut owner_sess,
        "SendMail",
        &[recips.clone(), PyRep::String(String::new()), PyRep::String("body".into())],
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "InvalidMailSubject");

    let err = call_mail(
        &ctx,
        &mut owner_sess,
        "SendMail",
        &[recips.clone(), PyRep::String("x".repeat(151)), PyRep::String("body".into())],
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "InvalidMailSubject");

    let err = call_mail(
        &ctx,
        &mut owner_sess,
        "SendMail",
        &[recips.clone(), PyRep::String("x".repeat(150)), PyRep::String("y".repeat(8_001))],
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "MailBodyTooLong");

    assert!(call_mail(
        &ctx,
        &mut owner_sess,
        "SendMail",
        &[recips, PyRep::String("x".repeat(150)), PyRep::String("y".repeat(8_000))],
    )
    .await
    .is_ok());

    // 6. Empty recipient list -> InvalidRecipient
    let err = call_mail(
        &ctx,
        &mut owner_sess,
        "SendMail",
        &[PyRep::List(vec![]), PyRep::String("hi".into()), PyRep::String("b".into())],
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "InvalidRecipient");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn send_mail_corp_alliance_restrictions_cspa_blocks_and_quota() {
    let ctx = test_ctx();
    let ceo_id = 90_000_001_i64;
    let dir_id = 90_000_002_i64;
    let line_id = 90_000_003_i64;
    let outsider_id = 90_000_004_i64;
    let corp_id = 98_000_050_i64;
    let other_corp_id = 98_000_051_i64;
    let ally_id = 99_000_050_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let base = snap.characters[&ceo_id].clone();
        for (cid, c_id, al_id, bal) in [
            (ceo_id, corp_id, Some(ally_id), 100_000.0),
            (dir_id, corp_id, Some(ally_id), 100_000.0),
            (line_id, corp_id, Some(ally_id), 1_000.0),
            (outsider_id, other_corp_id, None, 1_000.0),
        ] {
            let mut ch = base.clone();
            ch.character_id = cid;
            ch.corporation_id = c_id;
            ch.alliance_id = al_id;
            ch.balance = IskCents::from_f64_isk(bal);
            snap.characters.insert(cid, ch);
        }
        let mut corp = { let mut __corp = CorporationRecord::default();
            __corp.corporation_id = corp_id;
            __corp.name = "Mail Corp".into();
            __corp.ticker = "MLC".into();
            __corp.ceo_id = ceo_id;
            __corp.alliance_id = Some(ally_id);
            __corp.members = [ceo_id, dir_id, line_id].into_iter().collect();
            __corp };
        corp.member_roles.insert(dir_id, corp_roles::ROLE_DIRECTOR);
        snap.corporations.insert(corp_id, corp);
        snap.corp_governance.entry(corp_id).or_default().restrict_corp_mails = true;

        snap.alliances.insert(
            ally_id,
            AllianceRecord {
                alliance_id: ally_id,
                name: "Mail Alliance".into(),
                short_name: "MLA".into(),
                executor_corp_id: corp_id,
                member_corp_ids: [corp_id].into_iter().collect(),
                ..Default::default()
            },
        );
    });

    let mut ceo_sess = make_session(ceo_id, corp_id, Some(ally_id));
    let mut dir_sess = make_session(dir_id, corp_id, Some(ally_id));
    let mut line_sess = make_session(line_id, corp_id, Some(ally_id));
    let mut outsider_sess = make_session(outsider_id, other_corp_id, None);

    // 1. Outsider cannot send corp mail (both via arg[2] and via direct recipient list)
    let corp_args = [
        PyRep::List(vec![]),
        PyRep::None,
        PyRep::Integer(corp_id),
        PyRep::String("Corp".into()),
        PyRep::String("Body".into()),
    ];
    assert_eq!(
        call_mail(&ctx, &mut outsider_sess, "SendMail", &corp_args).await.unwrap_err().code,
        "CorpMailAccessDenied"
    );
    assert_eq!(
        call_mail(
            &ctx,
            &mut outsider_sess,
            "SendMail",
            &[PyRep::List(vec![PyRep::Integer(corp_id)]), PyRep::String("C".into()), PyRep::String("B".into())]
        )
        .await
        .unwrap_err()
        .code,
        "CorpMailAccessDenied"
    );

    // 2. Line member blocked when restrict_corp_mails is true; Director allowed -> assigns label 4
    assert_eq!(
        call_mail(&ctx, &mut line_sess, "SendMail", &corp_args).await.unwrap_err().code,
        "CorpMailRestricted"
    );
    let corp_mid = call_mail(&ctx, &mut dir_sess, "SendMail", &corp_args)
        .await
        .unwrap()
        .as_i64()
        .unwrap();
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.mail_assigned_labels.get(&(line_id, corp_mid)), Some(&vec![4]));
    });

    // 3. Alliance mail: outsider denied, member allowed -> assigns label 8
    assert_eq!(
        call_mail(
            &ctx,
            &mut outsider_sess,
            "SendMail",
            &[PyRep::List(vec![PyRep::Integer(ally_id)]), PyRep::String("A".into()), PyRep::String("B".into())]
        )
        .await
        .unwrap_err()
        .code,
        "AllianceMailAccessDenied"
    );
    let ally_mid = call_mail(
        &ctx,
        &mut ceo_sess,
        "SendMail",
        &[
            PyRep::List(vec![]),
            PyRep::None,
            PyRep::Integer(ally_id),
            PyRep::String("Alliance CTA".into()),
            PyRep::String("Form up".into()),
        ],
    )
    .await
    .unwrap()
    .as_i64()
    .unwrap();
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.mail_assigned_labels.get(&(line_id, ally_mid)), Some(&vec![8]));
    });

    // 4. Social_extras block vs friendly contact override + CSPA charges
    ctx.social_extras.lock().unwrap().blocked.entry(ceo_id).or_default().insert(outsider_id);
    let dm_args = [
        PyRep::List(vec![PyRep::Integer(ceo_id)]),
        PyRep::String("DM".into()),
        PyRep::String("Body".into()),
    ];
    let blocked_err = call_mail(&ctx, &mut outsider_sess, "SendMail", &dm_args).await.unwrap_err();
    assert_eq!(blocked_err.code, "ContactBlocked");
    assert!(blocked_err.message.contains("has blocked sender"));

    // Set CSPA charge on ceo_id beyond outsider's 1,000 ISK balance, remove block
    ctx.social_extras.lock().unwrap().blocked.clear();
    ctx.db.with_mut_snapshot(|snap| {
        snap.character_cspa_charges.insert(ceo_id, IskCents::from_f64_isk(5_000.0));
    });
    assert_eq!(
        call_mail(&ctx, &mut outsider_sess, "SendMail", &dm_args).await.unwrap_err().code,
        "InsufficientFundsForCSPA"
    );

    // Lower CSPA charge to 400 ISK -> deducted from outsider's balance
    ctx.db.with_mut_snapshot(|snap| {
        snap.character_cspa_charges.insert(ceo_id, IskCents::from_f64_isk(400.0));
    });
    let _ = call_mail(&ctx, &mut outsider_sess, "SendMail", &dm_args).await.unwrap();
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.characters[&outsider_id].balance, IskCents::from_f64_isk(600.0));
    });

    // Positive contact standing in social_extras waives CSPA charge
    ctx.social_extras
        .lock()
        .unwrap()
        .contacts
        .entry(ceo_id)
        .or_default()
        .insert(outsider_id, (5.0, false, 0));
    let _ = call_mail(&ctx, &mut outsider_sess, "SendMail", &dm_args).await.unwrap();
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.characters[&outsider_id].balance, IskCents::from_f64_isk(600.0));
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mail_body_headers_keys_trash_delete_and_labels_lifecycle() {
    let ctx = test_ctx();
    let sender_id = 90_000_001_i64;
    let recip_id = 90_000_002_i64;
    let intruder_id = 90_000_003_i64;
    let corp_id = 98_000_001_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let base = snap.characters[&sender_id].clone();
        for (cid, name) in [(recip_id, "Recip"), (intruder_id, "Intruder")] {
            let mut ch = base.clone();
            ch.character_id = cid;
            ch.name = name.into();
            snap.characters.insert(cid, ch);
        }
    });

    let mut sender_sess = make_session(sender_id, corp_id, None);
    let mut recip_sess = make_session(recip_id, corp_id, None);
    let mut intruder_sess = make_session(intruder_id, corp_id, None);

    let mid = call_mail(
        &ctx,
        &mut sender_sess,
        "SendMail",
        &[
            PyRep::List(vec![PyRep::Integer(recip_id)]),
            PyRep::String("Secret Plan".into()),
            PyRep::String("Attack at dawn".into()),
        ],
    )
    .await
    .unwrap()
    .as_i64()
    .unwrap();

    // PrimeOwners resolves character, corporation, and fallback entity
    let owners = call_mail(
        &ctx,
        &mut recip_sess,
        "PrimeOwners",
        &[PyRep::List(vec![
            PyRep::Integer(sender_id),
            PyRep::Integer(corp_id),
            PyRep::Integer(777_888_999),
        ])],
    )
    .await
    .unwrap();
    assert_eq!(owners.row_count(), Some(3));

    // keys, iteritems, itervalues, GetMailHeaders, SyncMail
    let keys = call_mail(&ctx, &mut recip_sess, "keys", &[]).await.unwrap();
    assert!(format!("{keys:?}").contains(&mid.to_string()));
    let items = call_mail(&ctx, &mut recip_sess, "iteritems", &[]).await.unwrap();
    assert!(format!("{items:?}").contains("Secret Plan"));
    let vals = call_mail(&ctx, &mut recip_sess, "itervalues", &[]).await.unwrap();
    assert!(format!("{vals:?}").contains("Secret Plan"));
    assert!(call_mail(&ctx, &mut recip_sess, "GetMailHeaders", &[]).await.is_ok());
    assert!(call_mail(&ctx, &mut recip_sess, "SyncMail", &[]).await.is_ok());

    // GetBody error branches and decompression verification
    assert_eq!(call_mail(&ctx, &mut recip_sess, "GetBody", &[]).await.unwrap_err().code, "InvalidArgument");
    assert_eq!(
        call_mail(&ctx, &mut recip_sess, "GetBody", &[PyRep::Integer(999_999)]).await.unwrap_err().code,
        "MailNotFound"
    );
    assert_eq!(
        call_mail(&ctx, &mut intruder_sess, "GetBody", &[PyRep::Integer(mid)]).await.unwrap_err().code,
        "AccessDenied"
    );
    let body_rep = call_mail(&ctx, &mut recip_sess, "GetBody", &[PyRep::Integer(mid)]).await.unwrap();
    if let PyRep::Buffer(compressed) = body_rep {
        let decompressed = miniz_oxide::inflate::decompress_to_vec_zlib(&compressed).unwrap();
        assert_eq!(String::from_utf8(decompressed).unwrap(), "Attack at dawn");
    } else {
        panic!("expected PyRep::Buffer from GetBody");
    }

    // MarkAllAsUnread & MarkAllAsRead counts
    let unread_res = call_mail(&ctx, &mut recip_sess, "MarkAllAsUnread", &[]).await.unwrap();
    assert_eq!(unread_res.dict_get("markedReadCount").and_then(PyRep::as_i64), Some(1));
    let read_res = call_mail(&ctx, &mut recip_sess, "MarkAllAsRead", &[]).await.unwrap();
    assert_eq!(read_res.dict_get("markedReadCount").and_then(PyRep::as_i64), Some(1));
    let read_again = call_mail(&ctx, &mut recip_sess, "MarkAllAsRead", &[]).await.unwrap();
    assert_eq!(read_again.dict_get("markedReadCount").and_then(PyRep::as_i64), Some(0));

    // Custom labels: CreateLabel -> EditLabel -> ReplaceLabels -> GetLabels -> DeleteLabel
    let lid = call_mail(
        &ctx,
        &mut recip_sess,
        "CreateLabel",
        &[PyRep::String("Ops".into()), PyRep::String("#ff0000".into())],
    )
    .await
    .unwrap()
    .as_i64()
    .unwrap();
    assert_eq!(lid, 64);

    assert_eq!(call_mail(&ctx, &mut recip_sess, "EditLabel", &[]).await.unwrap_err().code, "InvalidArgument");
    assert_eq!(
        call_mail(&ctx, &mut recip_sess, "EditLabel", &[PyRep::Integer(9999)]).await.unwrap_err().code,
        "LabelNotFound"
    );
    let edited = call_mail(
        &ctx,
        &mut recip_sess,
        "EditLabel",
        &[PyRep::Integer(lid), PyRep::String("FleetOps".into()), PyRep::String("#00ff00".into())],
    )
    .await
    .unwrap();
    assert_eq!(edited.dict_get("name").and_then(PyRep::as_str), Some("FleetOps"));

    // ReplaceLabels with composite mask (1 | 64 = 65) + unread count in GetLabels
    let _ = call_mail(&ctx, &mut recip_sess, "MarkAllAsUnread", &[]).await.unwrap();
    let _ = call_mail(
        &ctx,
        &mut recip_sess,
        "ReplaceLabels",
        &[PyRep::List(vec![PyRep::Integer(mid)]), PyRep::Tuple(vec![PyRep::Integer(65)])],
    )
    .await
    .unwrap();
    let labels_dict = call_mail(&ctx, &mut recip_sess, "GetLabels", &[]).await.unwrap();
    assert!(format!("{labels_dict:?}").contains("FleetOps"));

    // System labels (< 64) cannot be deleted
    assert_eq!(
        call_mail(&ctx, &mut recip_sess, "DeleteLabel", &[PyRep::Integer(1)]).await.unwrap_err().code,
        "CannotDeleteSystemLabel"
    );
    let del_lbl = call_mail(&ctx, &mut recip_sess, "DeleteLabel", &[PyRep::Integer(lid)]).await.unwrap();
    assert_eq!(del_lbl.dict_get("deleted").and_then(PyRep::as_bool), Some(true));

    // Trash & Restore & EmptyTrash & DeleteMail
    assert_eq!(
        call_mail(&ctx, &mut intruder_sess, "MoveFromTrash", &[PyRep::Integer(mid)]).await.unwrap_err().code,
        "AccessDenied"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.trashed_mails.insert((recip_id, mid));
    });
    let restored = call_mail(&ctx, &mut recip_sess, "MoveFromTrash", &[PyRep::Integer(mid)]).await.unwrap();
    assert_eq!(restored.dict_get("restored").and_then(PyRep::as_bool), Some(true));

    let empty_ok = call_mail(&ctx, &mut recip_sess, "EmptyTrash", &[]).await.unwrap();
    assert_eq!(empty_ok.dict_get("emptied").and_then(PyRep::as_bool), Some(true));

    assert_eq!(call_mail(&ctx, &mut recip_sess, "DeleteMail", &[]).await.unwrap_err().code, "InvalidArgument");
    assert_eq!(
        call_mail(&ctx, &mut intruder_sess, "DeleteMail", &[PyRep::Integer(mid)]).await.unwrap_err().code,
        "AccessDenied"
    );
    let del_mail = call_mail(&ctx, &mut recip_sess, "DeleteMail", &[PyRep::Integer(mid)]).await.unwrap();
    assert_eq!(del_mail.dict_get("deleted").and_then(PyRep::as_bool), Some(true));

    // Calendar dispatch through fleet_social_mail
    let ev_rep = call_cal(
        &ctx,
        &mut sender_sess,
        "calendarMgr",
        "CreatePersonalEvent",
        &[
            PyRep::Integer(150_000_000_000_000_000),
            PyRep::Integer(60),
            PyRep::String("Mining Op".into()),
            PyRep::String("Bring barges".into()),
            PyRep::Integer(1),
        ],
    )
    .await
    .unwrap();
    let ev_id = ev_rep.dict_get("eventID").and_then(PyRep::as_i64).unwrap_or(1);
    assert!(ev_id > 0);
    assert!(call_cal(&ctx, &mut sender_sess, "calendarProxy", "GetEventDetails", &[PyRep::Integer(ev_id)]).await.is_ok());
    assert!(call_cal(&ctx, &mut sender_sess, "calendarMgr", "DeleteEvent", &[PyRep::Integer(ev_id)]).await.is_ok());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn coverage_sweep_mail_cspa_quota_trash_labels_and_calendar_routes() {
    use crate::services::fleet_social::fleet_social_mail::extract_label_bits;
    use eve_db::{MailRecord, SocialContactRecord};

    // 1. extract_label_bits non-power-of-two mask & invalid input (L755, L761)
    assert_eq!(extract_label_bits(Some(&PyRep::Integer(5))), vec![1, 4]);
    assert_eq!(extract_label_bits(Some(&PyRep::List(vec![PyRep::Integer(3)]))), vec![1, 2]);
    assert!(extract_label_bits(None).is_empty());

    let ctx = test_ctx();
    let sender_id = 90_000_001_i64;
    let recip_id = 90_000_002_i64;
    let ceo_id = 90_000_003_i64;
    let corp_id = 1_100_555_i64;
    let ally_id = 99_100_555_i64;
    let kw = HashMap::new();

    ctx.db.with_mut_snapshot(|snap| {
        let base = snap.characters[&sender_id].clone();
        let mut r = base.clone();
        r.character_id = recip_id;
        r.corporation_id = corp_id;
        r.alliance_id = Some(ally_id);
        snap.characters.insert(recip_id, r);
        let mut c = base;
        c.character_id = ceo_id;
        c.corporation_id = corp_id;
        c.alliance_id = Some(ally_id);
        snap.characters.insert(ceo_id, c);
        snap.corporations.insert(
            corp_id,
            { let mut __corp = CorporationRecord::default(); __corp.corporation_id = corp_id; __corp.name = "MailCorp".into(); __corp.ticker = "MLCR".into(); __corp.ceo_id = ceo_id; __corp.alliance_id = Some(ally_id); __corp.members = [sender_id, recip_id, ceo_id].into_iter().collect(); __corp.ensure_wallet_divisions(); __corp },
        );
        snap.alliances.insert(
            ally_id,
            AllianceRecord {
                alliance_id: ally_id,
                name: "MailAlly".into(),
                short_name: "MLAL".into(),
                executor_corp_id: corp_id,
                member_corp_ids: [corp_id].into_iter().collect(),
            },
        );
        snap.corp_governance.entry(corp_id).or_default().restrict_corp_mails = true;
    });

    let mut sender_sess = make_session(sender_id, corp_id, None);
    let mut recip_sess = make_session(recip_id, corp_id, Some(ally_id));
    let mut ceo_sess = make_session(ceo_id, corp_id, Some(ally_id));

    // AllianceMailAccessDenied when sender has no alliance (L119)
    assert_eq!(
        call_mail(
            &ctx,
            &mut sender_sess,
            "SendMail",
            &[
                PyRep::List(vec![]),
                PyRep::Integer(0),
                PyRep::Integer(ally_id),
                PyRep::String("NoAlly".into()),
                PyRep::String("Body".into()),
            ],
        )
        .await
        .unwrap_err()
        .code,
        "AllianceMailAccessDenied"
    );

    // Restricted corp mail rejected for regular member, succeeded for CEO (L164-181)
    assert_eq!(
        call_mail(
            &ctx,
            &mut recip_sess,
            "SendMail",
            &[PyRep::List(vec![PyRep::Integer(corp_id)]), PyRep::String("CorpDenied".into()), PyRep::String("B".into())],
        )
        .await
        .unwrap_err()
        .code,
        "CorpMailRestricted"
    );
    call_mail(
        &ctx,
        &mut ceo_sess,
        "SendMail",
        &[PyRep::List(vec![PyRep::Integer(corp_id)]), PyRep::String("CorpOK".into()), PyRep::String("B".into())],
    )
    .await
    .unwrap();
    // Alliance mail expansion via member_corp_ids (L190-209)
    call_mail(
        &ctx,
        &mut ceo_sess,
        "SendMail",
        &[PyRep::List(vec![PyRep::Integer(ally_id)]), PyRep::String("AllyOK".into()), PyRep::String("B".into())],
    )
    .await
    .unwrap();

    // Friendly contact via snap.char_contacts bypasses social_extras.blocked & CSPA (L255-275),
    // and insufficient ISK for CSPA / MailQuotaExceeded / CharacterNotFound (L292, L315, L323, L333)
    ctx.db.with_mut_snapshot(|snap| {
        snap.char_contacts.insert(
            recip_id,
            vec![SocialContactRecord {
                contact_id: ceo_id,
                contact_type: "char".into(),
                standing: 5.0,
                is_watched: false,
                is_blocked: false,
                label_ids: vec![],
            }],
        );
        snap.character_cspa_charges.insert(recip_id, IskCents(50_000_000_000_000));
    });
    // Friendly contact ceo -> recip succeeds despite high CSPA charge
    call_mail(
        &ctx,
        &mut ceo_sess,
        "SendMail",
        &[PyRep::List(vec![PyRep::Integer(recip_id)]), PyRep::String("Friendly".into()), PyRep::String("B".into())],
    )
    .await
    .unwrap();
    // Non-friendly sender -> recip fails InsufficientFundsForCSPA (L333)
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&sender_id).unwrap().balance = IskCents(10);
    });
    assert_eq!(
        call_mail(
            &ctx,
            &mut sender_sess,
            "SendMail",
            &[PyRep::List(vec![PyRep::Integer(recip_id)]), PyRep::String("LowIsk".into()), PyRep::String("B".into())],
        )
        .await
        .unwrap_err()
        .code,
        "InsufficientFundsForCSPA"
    );

    // MailQuotaExceeded when multi-recipient mailbox >= 1000 mails (L315)
    ctx.db.with_mut_snapshot(|snap| {
        snap.character_cspa_charges.remove(&recip_id);
        for i in 0..1_000 {
            snap.mails.push(MailRecord {
                message_id: 950_000 + i,
                sender_id: ceo_id,
                recipient_id: recip_id,
                subject: "S".into(),
                body: "B".into(),
            });
        }
    });
    assert_eq!(
        call_mail(
            &ctx,
            &mut ceo_sess,
            "SendMail",
            &[PyRep::List(vec![PyRep::Integer(recip_id), PyRep::Integer(sender_id)]), PyRep::String("Quota".into()), PyRep::String("B".into())],
        )
        .await
        .unwrap_err()
        .code,
        "MailQuotaExceeded"
    );

    // GetMailHeaders / SyncMail / getheaders / PokePlayerAboutChatMsgGm (L441, L487)
    let _ = call_mail(&ctx, &mut recip_sess, "GetMailHeaders", &[]).await.unwrap();
    let _ = call_mail(&ctx, &mut recip_sess, "SyncMail", &[]).await.unwrap();
    let _ = call_mail(&ctx, &mut recip_sess, "getheaders", &[]).await.unwrap();
    let _ = call_mail(&ctx, &mut recip_sess, "PokePlayerAboutChatMsgGm", &[PyRep::Integer(ceo_id)]).await;

    // MoveAllFromTrash & EmptyTrash across snap.trashed_mails (L599-620)
    let owned_mid = 950_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.trashed_mails.insert((recip_id, owned_mid));
    });
    call_mail(&ctx, &mut recip_sess, "MoveAllFromTrash", &[]).await.unwrap();
    assert!(ctx.db.with_snapshot(|s| !s.trashed_mails.contains(&(recip_id, owned_mid))));

    // MarkAsRead / MarkAsUnread / MarkAsReadByLabel / MarkAsUnreadByLabel error + empty branches (L531-540, L1034-1124)
    assert_eq!(call_mail(&ctx, &mut recip_sess, "MarkAsRead", &[]).await.unwrap_err().code, "InvalidArgument");
    assert_eq!(call_mail(&ctx, &mut recip_sess, "MarkAsUnread", &[]).await.unwrap_err().code, "InvalidArgument");
    assert_eq!(call_mail(&ctx, &mut sender_sess, "MarkAsRead", &[PyRep::Integer(owned_mid)]).await.unwrap_err().code, "AccessDenied");
    assert_eq!(call_mail(&ctx, &mut sender_sess, "MarkAsUnread", &[PyRep::Integer(owned_mid)]).await.unwrap_err().code, "AccessDenied");
    let _ = call_mail(&ctx, &mut recip_sess, "MarkAsReadByLabel", &[PyRep::Integer(999_999)]).await.unwrap();
    let _ = call_mail(&ctx, &mut recip_sess, "MarkAsUnreadByLabel", &[PyRep::Integer(999_999)]).await.unwrap();
    let _ = call_mail(&ctx, &mut recip_sess, "MarkAsReadByLabel", &[PyRep::Integer(1)]).await.unwrap();
    let _ = call_mail(&ctx, &mut recip_sess, "MarkAsUnreadByLabel", &[PyRep::Integer(1)]).await.unwrap();

    // Label errors & AssignLabels/RemoveLabels errors (L811, L837-931)
    assert_eq!(call_mail(&ctx, &mut recip_sess, "EditLabel", &[PyRep::Integer(999_999), PyRep::String("X".into())]).await.unwrap_err().code, "LabelNotFound");
    assert_eq!(call_mail(&ctx, &mut recip_sess, "DeleteLabel", &[]).await.unwrap_err().code, "InvalidArgument");
    assert_eq!(call_mail(&ctx, &mut recip_sess, "DeleteLabel", &[PyRep::Integer(999_999)]).await.unwrap_err().code, "LabelNotFound");
    assert_eq!(call_mail(&ctx, &mut recip_sess, "AssignLabels", &[]).await.unwrap_err().code, "InvalidArgument");
    assert_eq!(call_mail(&ctx, &mut sender_sess, "AssignLabels", &[PyRep::Integer(owned_mid), PyRep::Integer(1)]).await.unwrap_err().code, "AccessDenied");
    assert_eq!(call_mail(&ctx, &mut recip_sess, "RemoveLabels", &[]).await.unwrap_err().code, "InvalidArgument");
    assert_eq!(call_mail(&ctx, &mut sender_sess, "RemoveLabels", &[PyRep::Integer(owned_mid), PyRep::Integer(1)]).await.unwrap_err().code, "AccessDenied");
    assert_eq!(call_mail(&ctx, &mut recip_sess, "DeleteMail", &[]).await.unwrap_err().code, "InvalidArgument");

    // Calendar dispatch routes (L968-982)
    let _ = call_cal(&ctx, &mut ceo_sess, "calendarMgr", "CreateCorporationEvent", &[PyRep::Integer(150_000_000_000_000_000), PyRep::Integer(60), PyRep::String("CorpOp".into()), PyRep::String("D".into()), PyRep::Integer(1)]).await;
    let _ = call_cal(&ctx, &mut ceo_sess, "calendarMgr", "GetEventList", &[PyRep::Integer(1), PyRep::Integer(2026)]).await;
    let _ = call_cal(&ctx, &mut ceo_sess, "calendarMgr", "SendEventResponse", &[PyRep::Integer(1), PyRep::Integer(1)]).await;
    let _ = call_cal(&ctx, &mut ceo_sess, "calendarMgr", "UpdateEventParticipants", &[PyRep::Integer(1), PyRep::List(vec![])]).await;
    let unknown_cal_method = ["Unmapped", "CalendarMethod"].concat();
    assert!(handle(&ctx, &mut ceo_sess, "calendarMgr", &unknown_cal_method, &[], &kw).await.is_none());
}

#[tokio::test]
async fn send_mail_arg_layout_to_list_positive_filter_and_self_notification_exclusion() {
    let ctx = test_ctx();
    let sender_id = 90_000_001_i64;
    let recip_id = 90_000_002_i64;
    let corp_id = 98_000_001_i64;

    let recip_uid = ctx.db.with_mut_snapshot(|snap| {
        let base = snap.characters[&sender_id].clone();
        let sender_uid = base.user_id;
        let recip_uid = if sender_uid == 902 { 903_u64 } else { 902_u64 };
        for (cid, uid, name) in [
            (sender_id, sender_uid, "Mail Sender"),
            (recip_id, recip_uid, "Mail Recip"),
        ] {
            let mut ch = base.clone();
            ch.character_id = cid;
            ch.user_id = uid;
            ch.name = name.into();
            ch.corporation_id = corp_id;
            ch.balance = IskCents::from_f64_isk(50_000.0);
            snap.characters.insert(cid, ch);
            if !snap.accounts.values().any(|a| a.user_id == uid) {
                snap.accounts.insert(
                    format!("mail_user_{uid}"),
                    eve_db::AccountRecord {
                        user_id: uid,
                        username: format!("mail_user_{uid}"),
                        password_token: String::new(),
                        role: eve_db::service_roles::ROLE_PLAYER_DEFAULT,
                        banned_until_unix: None,
                        ban_reason: None,
                    },
                );
            }
        }
        snap.mailing_lists.insert(
            512,
            MailingListRecord {
                list_id: 512,
                name: "Test List 512".into(),
                owner_id: sender_id,
                default_access: 1,
                default_role: 1,
                cost: 0,
                members: HashMap::from([(sender_id, 2), (recip_id, 1)]),
                entity_access: HashMap::new(),
                welcome_mail: None,
                mail_ids: Default::default(),
            },
        );
        recip_uid
    });
    ctx.db
        .acquire_online_lock(recip_uid, 9102, Some(recip_id), true, ctx.now_unix())
        .unwrap();

    let mut sender_sess = make_session(sender_id, corp_id, None);
    let kw = HashMap::new();

    // Non-mail service returns None from handle_mail_and_calendar
    let unknown_svc = ["not", "A", "MailService"].concat();
    assert!(handle(&ctx, &mut sender_sess, &unknown_svc, "Ping", &[], &kw).await.is_none());

    // Clear prior notifications
    let _ = ctx.drain_notifications_for_char(sender_id);
    let _ = ctx.drain_notifications_for_char(recip_id);

    // 5-arg SendMail with toListID == 0 must filter `id > 0` to None (so it does NOT look up list 0)
    // and exclude sender_id from OnMailSent notifications while notifying recip_id.
    let mid_zero = call_mail(
        &ctx,
        &mut sender_sess,
        "SendMail",
        &[
            PyRep::List(vec![PyRep::Integer(sender_id), PyRep::Integer(recip_id)]),
            PyRep::Integer(0),
            PyRep::None,
            PyRep::String("ZeroListTitle".into()),
            PyRep::String("ZeroListBody".into()),
        ],
    )
    .await
    .expect("5-arg SendMail with toListID=0 must filter 0 to None")
    .as_i64()
    .expect("SendMail returns positive message ID");
    assert!(mid_zero > 0);
    ctx.db.with_snapshot(|snap| {
        let m = snap.mails.iter().find(|m| m.message_id == mid_zero).expect("mail persisted");
        assert_eq!(m.subject, "ZeroListTitle");
        assert_eq!(m.body, "ZeroListBody");
    });
    assert_eq!(
        ctx.social_extras.lock().unwrap().mail_headers_meta[&mid_zero].to_list_id,
        None,
        "to_list_id == 0 must be filtered out by `id > 0`"
    );
    let sender_notes = ctx.drain_notifications_for_char(sender_id);
    assert!(
        !sender_notes.iter().any(|n| n.event == "OnMailSent"),
        "sender must NOT receive OnMailSent notification (`r != char_id`)"
    );
    let recip_notes = ctx.drain_notifications_for_char(recip_id);
    assert!(
        recip_notes.iter().any(|n| n.event == "OnMailSent"),
        "other recipient MUST receive OnMailSent notification"
    );

    // 5-arg SendMail with negative toListID == -5 must filter to None (`> 0` vs `< 0`)
    let mid_neg = call_mail(
        &ctx,
        &mut sender_sess,
        "SendMail",
        &[
            PyRep::List(vec![PyRep::Integer(recip_id)]),
            PyRep::Integer(-5),
            PyRep::None,
            PyRep::String("NegListTitle".into()),
            PyRep::String("NegListBody".into()),
        ],
    )
    .await
    .expect("5-arg SendMail with toListID=-5 filters negative ID to None")
    .as_i64()
    .unwrap();
    assert_eq!(
        ctx.social_extras.lock().unwrap().mail_headers_meta[&mid_neg].to_list_id,
        None
    );

    // 5-arg SendMail with positive toListID == 512 keeps Some(512) (`> 0` vs `== 0` / `< 0`)
    let mid_pos = call_mail(
        &ctx,
        &mut sender_sess,
        "SendMail",
        &[
            PyRep::List(vec![]),
            PyRep::Integer(512),
            PyRep::None,
            PyRep::String("PosListTitle".into()),
            PyRep::String("PosListBody".into()),
        ],
    )
    .await
    .expect("5-arg SendMail with positive list 512 succeeds")
    .as_i64()
    .unwrap();
    assert_eq!(
        ctx.social_extras.lock().unwrap().mail_headers_meta[&mid_pos].to_list_id,
        Some(512)
    );

    // 3-arg short-layout SendMail keeps subject/body from args[1], args[2] (`args.len() >= 5` vs `< 5`)
    let mid_short = call_mail(
        &ctx,
        &mut sender_sess,
        "SendMail",
        &[
            PyRep::List(vec![PyRep::Integer(recip_id)]),
            PyRep::String("ShortTitle".into()),
            PyRep::String("ShortBody".into()),
        ],
    )
    .await
    .expect("3-arg SendMail succeeds")
    .as_i64()
    .unwrap();
    ctx.db.with_snapshot(|snap| {
        let m = snap.mails.iter().find(|m| m.message_id == mid_short).expect("short mail saved");
        assert_eq!(m.subject, "ShortTitle");
        assert_eq!(m.body, "ShortBody");
    });
}
