//! EVE mail notification emission: `OnMailSent` from the game-client `mailMgr.SendMail` path and
//! from ESI `POST /characters/{id}/mail`, plus `OnMailUpdatedByExternal` / `OnMailDeleted` from the
//! ESI organise / delete routes. Layouts follow the V24 client's
//! `eve/client/script/ui/services/mail/mailSvc.pyj` (`mailSvc.OnMailSent`, `OnMailUpdatedByExternal`,
//! `OnMailDeleted`); every assertion checks exact values on the recipient and absence elsewhere.

use crate::common;

use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::esi::{EsiHttpRequest, EsiRouter};
use eve_server::router::ServerContext;
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;

const P1: i64 = 90_000_001;
const P2: i64 = 90_000_002;
const MINUTE_TICKS: i64 = 600_000_000;
const PKCE_VERIFIER: &str = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
const PKCE_CHALLENGE: &str = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";

async fn select(ctx: &ServerContext, sid: u64, user: &str, pass: &str, char_id: i64) -> SessionState {
    let mut s = SessionState::new(sid);
    common::login_pilot(ctx, &mut s, user, pass).await;
    ctx.dispatch_rpc(&mut s, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char_id)], &HashMap::new(), None)
        .await
        .expect("SelectCharacterID");
    s
}

fn events(ctx: &ServerContext, char_id: i64, event: &str) -> Vec<Vec<PyRep>> {
    ctx.drain_notifications_for_char(char_id).into_iter().filter(|n| n.event == event).map(|n| n.args).collect()
}

/// ESI token for P1 (account `demo`, which already owns P1).
fn p1_token(ctx: &ServerContext) -> String {
    let now = ctx.now_unix();
    let scopes = ["esi-mail.read_mail.v1", "esi-mail.send_mail.v1", "esi-mail.organize_mail.v1"];
    let code = ctx
        .db
        .authorize_pkce("demo", "demo-secret", P1, "evomu-notify-mail-test", "https://localhost/callback",
            scopes.iter().map(|s| s.to_string()).collect(), PKCE_CHALLENGE, "S256", now)
        .expect("authorize_pkce");
    ctx.db.exchange_pkce_code(&code, "evomu-notify-mail-test", PKCE_VERIFIER, now).expect("exchange").access_token
}

/// `sentDate` must be the send time truncated to the minute (as `mailSvc.SendMail` computes it).
fn assert_sent_date(args: &[PyRep], before: i64, after: i64) {
    let PyRep::Integer(d) = args[2] else { panic!("sentDate must be an integer FILETIME: {:?}", args[2]) };
    assert_eq!(d % MINUTE_TICKS, 0, "sentDate is minute-aligned");
    assert!(d >= before / MINUTE_TICKS * MINUTE_TICKS && d <= after, "sentDate {d} outside [{before}, {after}]");
}

#[tokio::test]
async fn test_mail_sent_updated_deleted_notifications() {
    // parity: notify:OnMailSent, notify:OnMailUpdatedByExternal, notify:OnMailDeleted
    eve_testkit::attest(&["notify:OnMailSent", "notify:OnMailUpdatedByExternal", "notify:OnMailDeleted"]);
    let ctx = Arc::new(ServerContext::new());
    let mut s1 = select(&ctx, 701, "demo", "demo-secret", P1).await;
    let _s2 = select(&ctx, 702, "other", "other-secret", P2).await;
    let uid3 = ctx.db.provision_player_account("mailp3", "mailp3-secret");
    let p3 = ctx.db.create_character(uid3, "Mail Pilot Three", 1, 1, 11, 17, &ctx.sde).expect("create p3").character_id;
    let _s3 = select(&ctx, 703, "mailp3", "mailp3-secret", p3).await;
    for c in [P1, P2, p3] {
        ctx.drain_notifications_for_char(c);
    }

    // Game client: mailMgr.SendMail(toCharacterIDs, toListID, toCorpOrAllianceID, title, body,
    // isReplyTo, isForwardedFrom). The recipient gets OnMailSent(messageID, senderID, sentDate,
    // toCharacterIDs, toListID, toCorpOrAllianceID, title, statusMask); the sender's own client
    // already ran OnMailSent locally, and the uninvolved P3 gets nothing.
    let before = ctx.now_blueos_filetime_ticks();
    let mid = ctx
        .dispatch_rpc(&mut s1, "mailMgr", None, "SendMail",
            &[PyRep::List(vec![PyRep::Integer(P2)]), PyRep::None, PyRep::None, PyRep::String("Op tonight".into()),
              PyRep::String("Form up at 20:00".into()), PyRep::None, PyRep::None],
            &HashMap::new(), None)
        .await
        .expect("SendMail")
        .as_i64()
        .expect("messageID");
    let after = ctx.now_blueos_filetime_ticks();
    let got = events(&ctx, P2, "OnMailSent");
    assert_eq!(got.len(), 1);
    assert_sent_date(&got[0], before, after);
    let mut expected = vec![
        PyRep::Integer(mid),
        PyRep::Integer(P1),
        got[0][2].clone(),
        PyRep::List(vec![PyRep::Integer(P2)]),
        PyRep::None,
        PyRep::None,
        PyRep::String("Op tonight".into()),
        PyRep::Integer(0),
    ];
    assert_eq!(got[0], expected);
    assert!(events(&ctx, P1, "OnMailSent").is_empty(), "game-client sender is not re-notified");
    assert!(events(&ctx, p3, "OnMailSent").is_empty());
    assert!(events(&ctx, P2, "OnNewMailReceived").is_empty(), "OnNewMailReceived is client-local");

    // ESI POST /characters/{P1}/mail: sent outside the game client, so sender and recipient both get it.
    let router = EsiRouter::new(Arc::clone(&ctx));
    let token = p1_token(&ctx);
    let body = json!({"approved_cost": 0, "subject": "ESI op", "body": "x",
        "recipients": [{"recipient_id": P2, "recipient_type": "character"}]});
    let before = ctx.now_blueos_filetime_ticks();
    let resp = router.handle_request(
        &EsiHttpRequest::new("POST", format!("/v1/characters/{P1}/mail/")).with_bearer_token(&token).with_body(serde_json::to_vec(&body).unwrap()),
    );
    assert_eq!(resp.status_code, 201, "{}", String::from_utf8_lossy(&resp.body));
    let esi_mid: i64 = serde_json::from_slice(&resp.body).expect("mail id");
    let after = ctx.now_blueos_filetime_ticks();
    for c in [P1, P2] {
        let got = events(&ctx, c, "OnMailSent");
        assert_eq!(got.len(), 1, "char {c}");
        assert_sent_date(&got[0], before, after);
        expected = vec![
            PyRep::Integer(esi_mid),
            PyRep::Integer(P1),
            got[0][2].clone(),
            PyRep::List(vec![PyRep::Integer(P2)]),
            PyRep::None,
            PyRep::None,
            PyRep::String("ESI op".into()),
            PyRep::Integer(0),
        ];
        assert_eq!(got[0], expected);
    }
    assert!(events(&ctx, p3, "OnMailSent").is_empty());

    // ESI PUT /characters/{P1}/mail/{id}: OnMailUpdatedByExternal(messageIDs, is_read, new_label);
    // the label mask is the OR of the label IDs, and an omitted field is None.
    let put = |payload: serde_json::Value| {
        router.handle_request(
            &EsiHttpRequest::new("PUT", format!("/v1/characters/{P1}/mail/{esi_mid}/")).with_bearer_token(&token).with_body(serde_json::to_vec(&payload).unwrap()),
        )
    };
    assert_eq!(put(json!({"read": false, "labels": [2, 16]})).status_code, 204);
    assert_eq!(
        events(&ctx, P1, "OnMailUpdatedByExternal"),
        vec![vec![PyRep::List(vec![PyRep::Integer(esi_mid)]), PyRep::Bool(false), PyRep::Integer(18)]]
    );
    assert_eq!(put(json!({"read": true})).status_code, 204);
    assert_eq!(
        events(&ctx, P1, "OnMailUpdatedByExternal"),
        vec![vec![PyRep::List(vec![PyRep::Integer(esi_mid)]), PyRep::Bool(true), PyRep::None]]
    );
    assert!(events(&ctx, P2, "OnMailUpdatedByExternal").is_empty(), "only the owner of the mailbox");
    // A rejected update (empty body) changes nothing and notifies nobody.
    assert_eq!(put(json!({})).status_code, 400);
    assert!(events(&ctx, P1, "OnMailUpdatedByExternal").is_empty());

    // ESI DELETE: OnMailDeleted(messageIDs) to the mailbox owner only; deleting again is a 404, no event.
    let del = || router.handle_request(&EsiHttpRequest::new("DELETE", format!("/v1/characters/{P1}/mail/{esi_mid}/")).with_bearer_token(&token));
    assert_eq!(del().status_code, 204);
    assert_eq!(events(&ctx, P1, "OnMailDeleted"), vec![vec![PyRep::List(vec![PyRep::Integer(esi_mid)])]]);
    assert!(events(&ctx, P2, "OnMailDeleted").is_empty());
    assert_eq!(del().status_code, 404);
    assert!(events(&ctx, P1, "OnMailDeleted").is_empty());
}
