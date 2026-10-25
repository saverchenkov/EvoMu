//! Comprehensive integration tests for User Services (`userSvc`), Mail Manager (`mailMgr`),
//! and Direct Trade / Contacts (`trademgr`, `charMgr`).

use crate::common;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::HashMap;

const P1: i64 = 90_000_001;
const P2: i64 = 90_000_002;
const JITA_STATION_ID: i64 = 60003760;

async fn select(
    ctx: &ServerContext,
    sid: u64,
    user: &str,
    pass: &str,
    char_id: i64,
) -> SessionState {
    let mut s = SessionState::new(sid);
    common::login_pilot(ctx, &mut s, user, pass).await;
    ctx.dispatch_rpc(
        &mut s,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &HashMap::new(),
        None,
    )
    .await
    .expect("SelectCharacterID");
    s
}

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, eve_proto::BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None)
        .await
}

fn seed_item(ctx: &ServerContext, type_id: u32, owner_id: i64, qty: i32) -> i64 {
    ctx.db.with_mut_snapshot(|snap| {
        snap.next_entity_id += 1;
        let id = snap.next_entity_id;
        snap.items.insert(
            id,
            eve_db::InventoryItemRecord {
                item_id: id,
                type_id,
                owner_id,
                location_id: JITA_STATION_ID,
                flag: eve_db::InventoryFlag::Hangar,
                quantity: qty,
                singleton: false,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        id
    })
}

async fn send_test_mail(
    ctx: &ServerContext,
    s: &mut SessionState,
    to: i64,
    title: &str,
    body: &str,
) -> i64 {
    let res = rpc(
        ctx,
        s,
        "mailMgr",
        "SendMail",
        &[
            PyRep::List(vec![PyRep::Integer(to)]),
            PyRep::None,
            PyRep::None,
            PyRep::String(title.into()),
            PyRep::String(body.into()),
        ],
    )
    .await
    .expect("SendMail");
    let PyRep::Integer(mid) = res else {
        panic!("expected Integer mail_id");
    };
    mid
}

#[tokio::test]
async fn test_usersvc_account_profile_and_tokens() {
    // parity: rpc:userSvc.GetUserName, rpc:userSvc.GetCreateDate, rpc:userSvc.GetUserToken, rpc:userSvc.GetSessionPlaytimeMinutes, rpc:userSvc.UserLogOffCharacter, rpc:userSvc.CreateRedeemTokenQA, rpc:userSvc.GetRedeemTokens, rpc:userSvc.ClaimRedeemTokens, rpc:userSvc.TrashRedeemTokens
    eve_testkit::attest(&[
        "rpc:userSvc.GetUserName",
        "rpc:userSvc.GetCreateDate",
        "rpc:userSvc.GetUserToken",
        "rpc:userSvc.GetSessionPlaytimeMinutes",
        "rpc:userSvc.UserLogOffCharacter",
        "rpc:userSvc.CreateRedeemTokenQA",
        "rpc:userSvc.GetRedeemTokens",
        "rpc:userSvc.ClaimRedeemTokens",
        "rpc:userSvc.TrashRedeemTokens",
    ]);

    let ctx = ServerContext::new();
    let mut s = select(&ctx, 1001, "demo", "demo-secret", P1).await;

    // 1. GetUserName
    let name_rep = rpc(&ctx, &mut s, "userSvc", "GetUserName", &[])
        .await
        .expect("GetUserName");
    assert_eq!(name_rep, PyRep::String("demo".into()));

    // 2. GetCreateDate
    let cd_rep = rpc(&ctx, &mut s, "userSvc", "GetCreateDate", &[])
        .await
        .expect("GetCreateDate");
    let PyRep::Integer(cd1) = cd_rep else {
        panic!("expected Integer createDate");
    };
    assert!(cd1 > 130_000_000_000_000_000);
    let cd_rep2 = rpc(&ctx, &mut s, "userSvc", "GetCreateDate", &[])
        .await
        .expect("GetCreateDate 2");
    assert_eq!(cd_rep2, PyRep::Integer(cd1));

    // 3. GetUserToken
    let tok_rep = rpc(&ctx, &mut s, "userSvc", "GetUserToken", &[])
        .await
        .expect("GetUserToken");
    let PyRep::String(tok) = tok_rep else {
        panic!("expected String token");
    };
    assert!(tok.starts_with("sso-tok-"));

    // 4. GetSessionPlaytimeMinutes
    let pt_rep = rpc(&ctx, &mut s, "userSvc", "GetSessionPlaytimeMinutes", &[])
        .await
        .expect("GetSessionPlaytimeMinutes");
    let PyRep::Integer(pt) = pt_rep else {
        panic!("expected Integer playtime");
    };
    assert!(pt >= 0);

    // 5. CreateRedeemTokenQA (requires GM role)
    s.role = eve_db::service_roles::ROLE_ADMIN | eve_db::service_roles::ROLE_GMS;
    let cr1 = rpc(
        &ctx,
        &mut s,
        "userSvc",
        "CreateRedeemTokenQA",
        &[
            PyRep::Integer(34),
            PyRep::Integer(500),
            PyRep::String("Tritanium Gift".into()),
        ],
    )
    .await
    .expect("CreateRedeemTokenQA 1");
    let PyRep::KeyVal(cr1_kv) = cr1 else {
        panic!("expected KeyVal");
    };
    let tok1_id = cr1_kv
        .get("tokenID")
        .and_then(|v| v.as_i64())
        .expect("tok1_id");
    assert_eq!(cr1_kv.get("quantity"), Some(&PyRep::Integer(500)));

    let cr2 = rpc(
        &ctx,
        &mut s,
        "userSvc",
        "CreateRedeemTokenQA",
        &[
            PyRep::Integer(35),
            PyRep::Integer(200),
            PyRep::String("Pyerite Gift".into()),
        ],
    )
    .await
    .expect("CreateRedeemTokenQA 2");
    let PyRep::KeyVal(cr2_kv) = cr2 else {
        panic!("expected KeyVal");
    };
    let tok2_id = cr2_kv
        .get("tokenID")
        .and_then(|v| v.as_i64())
        .expect("tok2_id");

    // 6. GetRedeemTokens
    let gr_rep = rpc(&ctx, &mut s, "userSvc", "GetRedeemTokens", &[])
        .await
        .expect("GetRedeemTokens");
    let PyRep::List(toks) = gr_rep else {
        panic!("expected List");
    };
    assert!(toks.iter().any(|t| matches!(t, PyRep::KeyVal(kv) if kv.get("tokenID").and_then(|v| v.as_i64()) == Some(tok1_id))));
    assert!(toks.iter().any(|t| matches!(t, PyRep::KeyVal(kv) if kv.get("tokenID").and_then(|v| v.as_i64()) == Some(tok2_id))));

    // 7. ClaimRedeemTokens
    let claim_rep = rpc(
        &ctx,
        &mut s,
        "userSvc",
        "ClaimRedeemTokens",
        &[
            PyRep::List(vec![PyRep::Integer(tok1_id)]),
            PyRep::Integer(P1),
        ],
    )
    .await
    .expect("ClaimRedeemTokens");
    let PyRep::KeyVal(claim_kv) = claim_rep else {
        panic!("expected KeyVal");
    };
    assert_eq!(
        claim_kv.get("claimedTokenIDs"),
        Some(&PyRep::List(vec![PyRep::Integer(tok1_id)]))
    );
    let has_claimed_item = ctx.db.with_snapshot(|snap| {
        snap.items
            .values()
            .any(|it| it.owner_id == P1 && it.type_id == 34 && it.quantity == 500)
    });
    assert!(has_claimed_item);

    // 8. TrashRedeemTokens
    let trash_rep = rpc(
        &ctx,
        &mut s,
        "userSvc",
        "TrashRedeemTokens",
        &[PyRep::List(vec![PyRep::Integer(tok2_id)])],
    )
    .await
    .expect("TrashRedeemTokens");
    assert_eq!(trash_rep, PyRep::Bool(true));
    let gr_after = rpc(&ctx, &mut s, "userSvc", "GetRedeemTokens", &[])
        .await
        .expect("GetRedeemTokens after");
    let PyRep::List(toks_after) = gr_after else {
        panic!("expected List");
    };
    assert!(toks_after.iter().all(|t| matches!(t, PyRep::KeyVal(kv) if kv.get("tokenID").and_then(|v| v.as_i64()) != Some(tok2_id))));

    // 9. UserLogOffCharacter
    let logoff_rep = rpc(&ctx, &mut s, "userSvc", "UserLogOffCharacter", &[])
        .await
        .expect("UserLogOffCharacter");
    assert_eq!(logoff_rep, PyRep::Bool(true));
    assert_eq!(s.character_id, None);
}

#[tokio::test]
async fn test_usersvc_mct_resculpt_and_moderation() {
    // parity: rpc:userSvc.ReverseRedeem, rpc:userSvc.CastUpvote, rpc:userSvc.CastDownvote, rpc:userSvc.ReportBot, rpc:userSvc.ReportISKSpammer, rpc:userSvc.ActivateMultiTraining, rpc:userSvc.ActivateSoulboundMultiTraining, rpc:userSvc.GetMultiCharactersTrainingSlots, rpc:userSvc.GetUpdatedMultiCharacterTraining, rpc:userSvc.InvalidateMultiCharacterTrainingCache, rpc:userSvc.ActivateCharacterReSculpt
    eve_testkit::attest(&[
        "rpc:userSvc.ReverseRedeem",
        "rpc:userSvc.CastUpvote",
        "rpc:userSvc.CastDownvote",
        "rpc:userSvc.ReportBot",
        "rpc:userSvc.ReportISKSpammer",
        "rpc:userSvc.ActivateMultiTraining",
        "rpc:userSvc.ActivateSoulboundMultiTraining",
        "rpc:userSvc.GetMultiCharactersTrainingSlots",
        "rpc:userSvc.GetUpdatedMultiCharacterTraining",
        "rpc:userSvc.InvalidateMultiCharacterTrainingCache",
        "rpc:userSvc.ActivateCharacterReSculpt",
    ]);

    let ctx = ServerContext::new();
    let mut s = select(&ctx, 1002, "demo", "demo-secret", P1).await;
    s.role = eve_db::service_roles::ROLE_ADMIN | eve_db::service_roles::ROLE_GMS;

    // 1. ReverseRedeem
    let cr = rpc(
        &ctx,
        &mut s,
        "userSvc",
        "CreateRedeemTokenQA",
        &[
            PyRep::Integer(36),
            PyRep::Integer(100),
            PyRep::String("Mexallon Gift".into()),
        ],
    )
    .await
    .expect("CreateRedeemTokenQA");
    let PyRep::KeyVal(kv) = cr else {
        panic!("expected KeyVal");
    };
    let rev_tok_id = kv.get("tokenID").and_then(|v| v.as_i64()).unwrap();
    let rev = rpc(
        &ctx,
        &mut s,
        "userSvc",
        "ReverseRedeem",
        &[PyRep::Integer(rev_tok_id)],
    )
    .await
    .expect("ReverseRedeem");
    assert_eq!(rev, PyRep::Bool(true));

    // 2. CastUpvote & CastDownvote
    let up = rpc(
        &ctx,
        &mut s,
        "userSvc",
        "CastUpvote",
        &[PyRep::String("survey-q1".into())],
    )
    .await
    .expect("CastUpvote");
    assert_eq!(up, PyRep::Bool(true));
    let down = rpc(
        &ctx,
        &mut s,
        "userSvc",
        "CastDownvote",
        &[PyRep::String("survey-q1".into())],
    )
    .await
    .expect("CastDownvote");
    assert_eq!(down, PyRep::Bool(true));

    // 3. ReportBot & ReportISKSpammer
    let bot = rpc(
        &ctx,
        &mut s,
        "userSvc",
        "ReportBot",
        &[PyRep::Integer(P2), PyRep::String("Bot mining in Belts".into())],
    )
    .await
    .expect("ReportBot");
    let PyRep::Integer(bot_nid) = bot else {
        panic!("expected Integer noteID");
    };
    assert!(bot_nid > 0);

    let spam = rpc(
        &ctx,
        &mut s,
        "userSvc",
        "ReportISKSpammer",
        &[
            PyRep::Integer(P2),
            PyRep::String("Spamming HyperNet in Local".into()),
        ],
    )
    .await
    .expect("ReportISKSpammer");
    let PyRep::Integer(spam_nid) = spam else {
        panic!("expected Integer noteID");
    };
    assert!(spam_nid > 0);

    ctx.db.with_snapshot(|snap| {
        assert!(snap.owner_notes.contains_key(&bot_nid));
        assert!(snap.owner_notes.contains_key(&spam_nid));
    });

    // 4. GetMultiCharactersTrainingSlots (initially empty)
    let mct_init = rpc(&ctx, &mut s, "userSvc", "GetMultiCharactersTrainingSlots", &[])
        .await
        .expect("GetMultiCharactersTrainingSlots init");
    assert_eq!(mct_init, PyRep::Dict(Vec::new()));

    // 5. ActivateMultiTraining
    let mct_item = seed_item(&ctx, 34189, P1, 1);
    let act1 = rpc(&ctx, &mut s, "userSvc", "ActivateMultiTraining", &[])
        .await
        .expect("ActivateMultiTraining");
    let PyRep::KeyVal(act1_kv) = act1 else {
        panic!("expected KeyVal");
    };
    assert_eq!(act1_kv.get("slotActivated"), Some(&PyRep::Bool(true)));
    assert!(ctx.db.with_snapshot(|snap| !snap.items.contains_key(&mct_item)));

    // 6. GetMultiCharactersTrainingSlots (now has 1 slot)
    let mct_now = rpc(&ctx, &mut s, "userSvc", "GetMultiCharactersTrainingSlots", &[])
        .await
        .expect("GetMultiCharactersTrainingSlots active");
    let PyRep::Dict(slots) = mct_now else {
        panic!("expected Dict");
    };
    assert_eq!(slots.len(), 1);

    // 7. ActivateSoulboundMultiTraining
    let sbmct_item = seed_item(&ctx, 48582, P1, 1);
    let act2 = rpc(&ctx, &mut s, "userSvc", "ActivateSoulboundMultiTraining", &[])
        .await
        .expect("ActivateSoulboundMultiTraining");
    let PyRep::KeyVal(act2_kv) = act2 else {
        panic!("expected KeyVal");
    };
    assert_eq!(act2_kv.get("slotActivated"), Some(&PyRep::Bool(true)));
    assert!(ctx.db.with_snapshot(|snap| !snap.items.contains_key(&sbmct_item)));

    // 8. GetUpdatedMultiCharacterTraining & InvalidateMultiCharacterTrainingCache
    let upd = rpc(
        &ctx,
        &mut s,
        "userSvc",
        "GetUpdatedMultiCharacterTraining",
        &[],
    )
    .await
    .expect("GetUpdatedMultiCharacterTraining");
    let PyRep::List(upd_slots) = upd else {
        panic!("expected List");
    };
    assert_eq!(upd_slots.len(), 2);

    let inval = rpc(
        &ctx,
        &mut s,
        "userSvc",
        "InvalidateMultiCharacterTrainingCache",
        &[],
    )
    .await
    .expect("InvalidateMultiCharacterTrainingCache");
    let PyRep::List(inval_slots) = inval else {
        panic!("expected List");
    };
    assert_eq!(inval_slots.len(), 2);

    // 9. ActivateCharacterReSculpt
    let resc_item = seed_item(&ctx, 28662, P1, 1);
    let resc = rpc(&ctx, &mut s, "userSvc", "ActivateCharacterReSculpt", &[])
        .await
        .expect("ActivateCharacterReSculpt");
    assert_eq!(resc, PyRep::Bool(true));
    assert!(ctx.db.with_snapshot(|snap| !snap.items.contains_key(&resc_item)));
}

#[tokio::test]
async fn test_mailmgr_labels_and_headers() {
    // parity: rpc:mailMgr.GetMailHeaders, rpc:mailMgr.GetBody, rpc:mailMgr.GetLabels, rpc:mailMgr.CreateLabel, rpc:mailMgr.EditLabel, rpc:mailMgr.DeleteLabel, rpc:mailMgr.AssignLabels, rpc:mailMgr.RemoveLabels
    eve_testkit::attest(&[
        "rpc:mailMgr.GetMailHeaders",
        "rpc:mailMgr.GetBody",
        "rpc:mailMgr.GetLabels",
        "rpc:mailMgr.CreateLabel",
        "rpc:mailMgr.EditLabel",
        "rpc:mailMgr.DeleteLabel",
        "rpc:mailMgr.AssignLabels",
        "rpc:mailMgr.RemoveLabels",
    ]);

    let ctx = ServerContext::new();
    let mut s = select(&ctx, 1003, "demo", "demo-secret", P1).await;
    let mut s2 = select(&ctx, 1004, "other", "other-secret", P2).await;

    // 1. PokePlayerAboutChatMsgGm (Non-GM must fail)
    let poke_err = rpc(&ctx, &mut s, "mailMgr", "PokePlayerAboutChatMsgGm", &[])
        .await
        .expect_err("non-GM PokePlayerAboutChatMsgGm must fail");
    assert_eq!(poke_err.code, "UserError");

    // 2. GetLabels
    let gl = rpc(&ctx, &mut s, "mailMgr", "GetLabels", &[])
        .await
        .expect("GetLabels");
    let PyRep::Dict(dict) = gl else {
        panic!("expected Dict");
    };
    assert!(dict.len() >= 4);

    // 3. CreateLabel
    let cl = rpc(
        &ctx,
        &mut s,
        "mailMgr",
        "CreateLabel",
        &[
            PyRep::String("Combat Ops".into()),
            PyRep::String("#ff5500".into()),
        ],
    )
    .await
    .expect("CreateLabel");
    let custom_lid = cl.as_i64().expect("labelID");
    assert!(custom_lid >= 16);

    // 4. EditLabel
    let el = rpc(
        &ctx,
        &mut s,
        "mailMgr",
        "EditLabel",
        &[
            PyRep::Integer(custom_lid),
            PyRep::String("Tactical Ops".into()),
            PyRep::String("#00ff55".into()),
        ],
    )
    .await
    .expect("EditLabel");
    let PyRep::KeyVal(el_kv) = el else {
        panic!("expected KeyVal");
    };
    assert_eq!(
        el_kv.get("name"),
        Some(&PyRep::String("Tactical Ops".into()))
    );
    assert_eq!(
        el_kv.get("color"),
        Some(&PyRep::String("#00ff55".into()))
    );

    // 5. Send a mail from P2 to P1
    let sm = rpc(
        &ctx,
        &mut s2,
        "mailMgr",
        "SendMail",
        &[
            PyRep::List(vec![PyRep::Integer(P1)]),
            PyRep::None,
            PyRep::None,
            PyRep::String("Intel Report".into()),
            PyRep::String("Enemy fleet in Jita 4-4".into()),
        ],
    )
    .await
    .expect("SendMail");
    let PyRep::Integer(mail_id) = sm else {
        panic!("expected Integer mail_id");
    };
    assert!(mail_id > 0);

    // 6. GetMailHeaders
    let headers_rep = rpc(&ctx, &mut s, "mailMgr", "GetMailHeaders", &[])
        .await
        .expect("GetMailHeaders");
    let PyRep::Rowset {
        rows: h_rows, ..
    } = headers_rep
    else {
        panic!("expected Rowset");
    };
    assert!(h_rows
        .iter()
        .any(|r| r.first() == Some(&PyRep::Integer(mail_id))));

    // 7. GetBody
    let gb = rpc(&ctx, &mut s, "mailMgr", "GetBody", &[PyRep::Integer(mail_id)])
        .await
        .expect("GetBody");
    let PyRep::Buffer(comp_bytes) = gb else {
        panic!("expected Buffer");
    };
    let decomp = miniz_oxide::inflate::decompress_to_vec_zlib(&comp_bytes).expect("decompress zlib");
    assert_eq!(
        String::from_utf8(decomp).unwrap(),
        "Enemy fleet in Jita 4-4"
    );
    assert!(ctx
        .db
        .with_snapshot(|snap| snap.read_mails.contains(&(P1, mail_id))));

    // 8. AssignLabels
    let al = rpc(
        &ctx,
        &mut s,
        "mailMgr",
        "AssignLabels",
        &[
            PyRep::List(vec![PyRep::Integer(mail_id)]),
            PyRep::List(vec![PyRep::Integer(custom_lid)]),
        ],
    )
    .await
    .expect("AssignLabels");
    let PyRep::KeyVal(al_kv) = al else {
        panic!("expected KeyVal");
    };
    assert_eq!(al_kv.get("assigned"), Some(&PyRep::Bool(true)));
    ctx.db.with_snapshot(|snap| {
        let labels = snap
            .mail_assigned_labels
            .get(&(P1, mail_id))
            .expect("assigned labels");
        assert!(labels.contains(&custom_lid));
    });

    // 9. RemoveLabels
    let rl = rpc(
        &ctx,
        &mut s,
        "mailMgr",
        "RemoveLabels",
        &[
            PyRep::List(vec![PyRep::Integer(mail_id)]),
            PyRep::List(vec![PyRep::Integer(custom_lid)]),
        ],
    )
    .await
    .expect("RemoveLabels");
    let PyRep::KeyVal(rl_kv) = rl else {
        panic!("expected KeyVal");
    };
    assert_eq!(rl_kv.get("removed"), Some(&PyRep::Bool(true)));
    ctx.db.with_snapshot(|snap| {
        let labels = snap
            .mail_assigned_labels
            .get(&(P1, mail_id))
            .expect("assigned labels");
        assert!(!labels.contains(&custom_lid));
    });

    // 10. DeleteLabel
    let dl = rpc(
        &ctx,
        &mut s,
        "mailMgr",
        "DeleteLabel",
        &[PyRep::Integer(custom_lid)],
    )
    .await
    .expect("DeleteLabel");
    let PyRep::KeyVal(dl_kv) = dl else {
        panic!("expected KeyVal");
    };
    assert_eq!(dl_kv.get("deleted"), Some(&PyRep::Bool(true)));
    ctx.db.with_snapshot(|snap| {
        let list = snap.mail_labels.get(&P1).cloned().unwrap_or_default();
        assert!(!list.iter().any(|l| l.label_id == custom_lid));
    });
}

#[tokio::test]
async fn test_mailmgr_read_states_and_trash() {
    // parity: rpc:mailMgr.MarkAsRead, rpc:mailMgr.MarkAsReadByLabel, rpc:mailMgr.MarkAsReadByList, rpc:mailMgr.MarkAsUnread, rpc:mailMgr.MarkAsUnreadByLabel, rpc:mailMgr.MarkAsUnreadByList, rpc:mailMgr.MarkAllAsRead, rpc:mailMgr.MarkAllAsUnread, rpc:mailMgr.MoveToTrash, rpc:mailMgr.MoveToTrashByLabel, rpc:mailMgr.MoveToTrashByList, rpc:mailMgr.MoveAllToTrash, rpc:mailMgr.MoveFromTrash, rpc:mailMgr.MoveAllFromTrash, rpc:mailMgr.EmptyTrash
    eve_testkit::attest(&[
        "rpc:mailMgr.MarkAsRead",
        "rpc:mailMgr.MarkAsReadByLabel",
        "rpc:mailMgr.MarkAsReadByList",
        "rpc:mailMgr.MarkAsUnread",
        "rpc:mailMgr.MarkAsUnreadByLabel",
        "rpc:mailMgr.MarkAsUnreadByList",
        "rpc:mailMgr.MarkAllAsRead",
        "rpc:mailMgr.MarkAllAsUnread",
        "rpc:mailMgr.MoveToTrash",
        "rpc:mailMgr.MoveToTrashByLabel",
        "rpc:mailMgr.MoveToTrashByList",
        "rpc:mailMgr.MoveAllToTrash",
        "rpc:mailMgr.MoveFromTrash",
        "rpc:mailMgr.MoveAllFromTrash",
        "rpc:mailMgr.EmptyTrash",
    ]);

    let ctx = ServerContext::new();
    let mut s = select(&ctx, 1005, "demo", "demo-secret", P1).await;
    let mut s2 = select(&ctx, 1006, "other", "other-secret", P2).await;

    // Send 3 mails from P2 to P1
    let m1 = send_test_mail(&ctx, &mut s2, P1, "Msg 1", "Body 1").await;
    let m2 = send_test_mail(&ctx, &mut s2, P1, "Msg 2", "Body 2").await;
    let m3 = send_test_mail(&ctx, &mut s2, P1, "Msg 3", "Body 3").await;

    // Create custom label and assign to m2
    let cl = rpc(
        &ctx,
        &mut s,
        "mailMgr",
        "CreateLabel",
        &[PyRep::String("Special".into()), PyRep::String("#112233".into())],
    )
    .await
    .expect("CreateLabel");
    let special_lid = cl.as_i64().expect("special_lid");
    rpc(
        &ctx,
        &mut s,
        "mailMgr",
        "AssignLabels",
        &[
            PyRep::List(vec![PyRep::Integer(m2)]),
            PyRep::List(vec![PyRep::Integer(special_lid)]),
        ],
    )
    .await
    .expect("AssignLabels");

    // 1. MarkAsRead & MarkAsUnread
    rpc(
        &ctx,
        &mut s,
        "mailMgr",
        "MarkAsRead",
        &[PyRep::List(vec![PyRep::Integer(m1)])],
    )
    .await
    .expect("MarkAsRead");
    assert!(ctx.db.with_snapshot(|snap| snap.read_mails.contains(&(P1, m1))));

    rpc(
        &ctx,
        &mut s,
        "mailMgr",
        "MarkAsUnread",
        &[PyRep::List(vec![PyRep::Integer(m1)])],
    )
    .await
    .expect("MarkAsUnread");
    assert!(!ctx.db.with_snapshot(|snap| snap.read_mails.contains(&(P1, m1))));

    // 2. MarkAsReadByLabel & MarkAsUnreadByLabel
    rpc(
        &ctx,
        &mut s,
        "mailMgr",
        "MarkAsReadByLabel",
        &[PyRep::Integer(special_lid)],
    )
    .await
    .expect("MarkAsReadByLabel");
    assert!(ctx.db.with_snapshot(|snap| snap.read_mails.contains(&(P1, m2))));

    rpc(
        &ctx,
        &mut s,
        "mailMgr",
        "MarkAsUnreadByLabel",
        &[PyRep::Integer(special_lid)],
    )
    .await
    .expect("MarkAsUnreadByLabel");
    assert!(!ctx.db.with_snapshot(|snap| snap.read_mails.contains(&(P1, m2))));

    // 3. MarkAsReadByList & MarkAsUnreadByList
    let ml = rpc(
        &ctx,
        &mut s,
        "mailingListsMgr",
        "Create",
        &[PyRep::String("TradeList".into())],
    )
    .await
    .expect("Create");
    let PyRep::KeyVal(ml_kv) = ml else {
        panic!("expected KeyVal");
    };
    let trade_lid = ml_kv.get("listID").and_then(|v| v.as_i64()).unwrap();
    rpc(&ctx, &mut s2, "mailingListsMgr", "Join", &[PyRep::Integer(trade_lid)]).await.expect("Join");
    let m_list = rpc(
        &ctx,
        &mut s2,
        "mailMgr",
        "SendMail",
        &[
            PyRep::List(vec![]),
            PyRep::Integer(trade_lid),
            PyRep::None,
            PyRep::String("ListMsg".into()),
            PyRep::String("ListBody".into()),
        ],
    )
    .await
    .expect("SendMail to list")
    .as_i64()
    .unwrap();

    rpc(
        &ctx,
        &mut s,
        "mailMgr",
        "MarkAsReadByList",
        &[PyRep::Integer(trade_lid)],
    )
    .await
    .expect("MarkAsReadByList");
    assert!(ctx.db.with_snapshot(|snap| {
        snap.read_mails.contains(&(P1, m_list))
    }));

    rpc(
        &ctx,
        &mut s,
        "mailMgr",
        "MarkAsUnreadByList",
        &[PyRep::Integer(trade_lid)],
    )
    .await
    .expect("MarkAsUnreadByList");
    assert!(ctx.db.with_snapshot(|snap| {
        !snap.read_mails.contains(&(P1, m_list))
    }));

    // 4. MarkAllAsRead & MarkAllAsUnread
    rpc(&ctx, &mut s, "mailMgr", "MarkAllAsRead", &[])
        .await
        .expect("MarkAllAsRead");
    assert!(ctx.db.with_snapshot(|snap| {
        snap.read_mails.contains(&(P1, m1))
            && snap.read_mails.contains(&(P1, m2))
            && snap.read_mails.contains(&(P1, m3))
    }));

    rpc(&ctx, &mut s, "mailMgr", "MarkAllAsUnread", &[])
        .await
        .expect("MarkAllAsUnread");
    assert!(ctx.db.with_snapshot(|snap| {
        !snap.read_mails.contains(&(P1, m1))
            && !snap.read_mails.contains(&(P1, m2))
            && !snap.read_mails.contains(&(P1, m3))
    }));

    // 5. MoveToTrash & MoveFromTrash
    let mt_rep = rpc(
        &ctx,
        &mut s,
        "mailMgr",
        "MoveToTrash",
        &[PyRep::List(vec![PyRep::Integer(m1)])],
    )
    .await
    .expect("MoveToTrash");
    let PyRep::KeyVal(mt_kv) = mt_rep else { panic!("expected KeyVal") };
    assert_eq!(mt_kv.get("inTrash"), Some(&PyRep::Bool(true)));
    assert!(eve_server::services::fleet_social::get_trashed_mails(&ctx).contains(&(P1, m1)));

    let mf_rep = rpc(
        &ctx,
        &mut s,
        "mailMgr",
        "MoveFromTrash",
        &[PyRep::List(vec![PyRep::Integer(m1)])],
    )
    .await
    .expect("MoveFromTrash");
    let PyRep::KeyVal(mf_kv) = mf_rep else { panic!("expected KeyVal") };
    assert_eq!(mf_kv.get("inTrash"), Some(&PyRep::Bool(false)));
    assert!(!eve_server::services::fleet_social::get_trashed_mails(&ctx).contains(&(P1, m1)));

    // 6. MoveToTrashByLabel
    let mtl_rep = rpc(
        &ctx,
        &mut s,
        "mailMgr",
        "MoveToTrashByLabel",
        &[PyRep::Integer(special_lid)],
    )
    .await
    .expect("MoveToTrashByLabel");
    let PyRep::KeyVal(mtl_kv) = mtl_rep else { panic!("expected KeyVal") };
    assert_eq!(mtl_kv.get("inTrash"), Some(&PyRep::Bool(true)));
    assert!(eve_server::services::fleet_social::get_trashed_mails(&ctx).contains(&(P1, m2)));

    // 7. MoveToTrashByList
    let mtlist_rep = rpc(
        &ctx,
        &mut s,
        "mailMgr",
        "MoveToTrashByList",
        &[PyRep::List(vec![PyRep::Integer(m3)])],
    )
    .await
    .expect("MoveToTrashByList");
    let PyRep::KeyVal(mtlist_kv) = mtlist_rep else { panic!("expected KeyVal") };
    assert_eq!(mtlist_kv.get("inTrash"), Some(&PyRep::Bool(true)));
    assert!(eve_server::services::fleet_social::get_trashed_mails(&ctx).contains(&(P1, m3)));

    // 8. MoveAllFromTrash
    let mafr_rep = rpc(&ctx, &mut s, "mailMgr", "MoveAllFromTrash", &[])
        .await
        .expect("MoveAllFromTrash");
    let PyRep::KeyVal(mafr_kv) = mafr_rep else { panic!("expected KeyVal") };
    assert_eq!(mafr_kv.get("inTrash"), Some(&PyRep::Bool(false)));
    assert!(!eve_server::services::fleet_social::get_trashed_mails(&ctx).iter().any(|&(cid, _)| cid == P1));

    // 9. MoveAllToTrash
    let matr_rep = rpc(&ctx, &mut s, "mailMgr", "MoveAllToTrash", &[])
        .await
        .expect("MoveAllToTrash");
    let PyRep::KeyVal(matr_kv) = matr_rep else { panic!("expected KeyVal") };
    assert_eq!(matr_kv.get("inTrash"), Some(&PyRep::Bool(true)));
    let trashed_all = eve_server::services::fleet_social::get_trashed_mails(&ctx);
    assert!(trashed_all.contains(&(P1, m1)));
    assert!(trashed_all.contains(&(P1, m2)));
    assert!(trashed_all.contains(&(P1, m3)));

    // 10. EmptyTrash
    let et_rep = rpc(&ctx, &mut s, "mailMgr", "EmptyTrash", &[])
        .await
        .expect("EmptyTrash");
    let PyRep::KeyVal(et_kv) = et_rep else { panic!("expected KeyVal") };
    assert_eq!(et_kv.get("deleted"), Some(&PyRep::Bool(true)));
    assert!(!eve_server::services::fleet_social::get_trashed_mails(&ctx).iter().any(|&(cid, _)| cid == P1));
    assert!(ctx.db.with_snapshot(|snap| {
        snap.deleted_mails.contains(&(P1, m1))
            && snap.deleted_mails.contains(&(P1, m2))
            && snap.deleted_mails.contains(&(P1, m3))
    }));
}

#[tokio::test]
async fn test_trade_and_contact_blocklist() {
    // parity: rpc:trademgr.InitiateTrade, rpc:charMgr.BlockOwners, rpc:charMgr.UnblockOwners
    eve_testkit::attest(&[
        "rpc:trademgr.InitiateTrade",
        "rpc:charMgr.BlockOwners",
        "rpc:charMgr.UnblockOwners",
    ]);

    let ctx = ServerContext::new();
    let mut s1 = select(&ctx, 1007, "demo", "demo-secret", P1).await;
    let mut s2 = select(&ctx, 1008, "other", "other-secret", P2).await;

    // Both pilots docked at Jita 4-4
    s1.station_id = Some(JITA_STATION_ID);
    s2.station_id = Some(JITA_STATION_ID);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&P1) {
            ch.station_id = Some(JITA_STATION_ID);
        }
        if let Some(ch) = snap.characters.get_mut(&P2) {
            ch.station_id = Some(JITA_STATION_ID);
        }
    });

    // 1. InitiateTrade
    let init = rpc(
        &ctx,
        &mut s1,
        "trademgr",
        "InitiateTrade",
        &[PyRep::Integer(P2)],
    )
    .await
    .expect("InitiateTrade");
    let PyRep::KeyVal(init_kv) = init else {
        panic!("expected KeyVal");
    };
    let trade_id = init_kv
        .get("tradeContainerID")
        .and_then(|v| v.as_i64())
        .expect("tradeContainerID");
    assert!(trade_id > 0);
    assert_eq!(init_kv.get("myID"), Some(&PyRep::Integer(P1)));
    assert_eq!(init_kv.get("herID"), Some(&PyRep::Integer(P2)));
    assert_eq!(
        init_kv.get("stationID"),
        Some(&PyRep::Integer(JITA_STATION_ID))
    );

    let n1 = ctx.drain_notifications_for_char(P1);
    assert!(n1.iter().any(|n| n.event == "OnTradeInitiate"));
    let n2 = ctx.drain_notifications_for_char(P2);
    assert!(n2.iter().any(|n| n.event == "OnTradeInitiate"));

    // 2. BlockOwners
    let bo = rpc(
        &ctx,
        &mut s1,
        "charMgr",
        "BlockOwners",
        &[PyRep::List(vec![PyRep::Integer(P2)])],
    )
    .await
    .expect("BlockOwners");
    let PyRep::KeyVal(bo_kv) = bo else {
        panic!("expected KeyVal");
    };
    assert_eq!(bo_kv.get("blockedCount"), Some(&PyRep::Integer(1)));
    assert_eq!(
        bo_kv.get("blockedIDs"),
        Some(&PyRep::List(vec![PyRep::Integer(P2)]))
    );

    let is_blocked = ctx
        .esi_state
        .char_contacts
        .read()
        .unwrap()
        .get(&P1)
        .and_then(|c| c.iter().find(|e| e.contact_id == P2).map(|e| e.is_blocked))
        .unwrap_or(false);
    assert!(is_blocked);

    // 3. UnblockOwners
    let ubo = rpc(
        &ctx,
        &mut s1,
        "charMgr",
        "UnblockOwners",
        &[PyRep::List(vec![PyRep::Integer(P2)])],
    )
    .await
    .expect("UnblockOwners");
    let PyRep::KeyVal(ubo_kv) = ubo else {
        panic!("expected KeyVal");
    };
    assert_eq!(ubo_kv.get("unblockedCount"), Some(&PyRep::Integer(1)));
    assert_eq!(
        ubo_kv.get("unblockedIDs"),
        Some(&PyRep::List(vec![PyRep::Integer(P2)]))
    );

    let is_blocked_after = ctx
        .esi_state
        .char_contacts
        .read()
        .unwrap()
        .get(&P1)
        .and_then(|c| c.iter().find(|e| e.contact_id == P2).map(|e| e.is_blocked))
        .unwrap_or(true);
    assert!(!is_blocked_after);
}
