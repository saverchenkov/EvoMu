//! Contact / address-book notification emission: each notification is produced by the real
//! `charMgr` / `authentication` / `charUnboundMgr` RPC and asserted with the argument layout of the
//! V24 client handler (`eve/client/script/ui/shared/neocom/addressBook/addressbookService.pyj`
//! `AddressBookSvc.On…`, `eve/client/script/ui/shared/comtool/onlineStatus.pyj`
//! `OnlineStatus.OnClientContactChange`), plus absence for non-recipients.

use crate::common;

use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::HashMap;

const P1: i64 = 90_000_001;
const P2: i64 = 90_000_002;

async fn select(ctx: &ServerContext, sid: u64, user: &str, pass: &str, char_id: i64) -> SessionState {
    let mut s = SessionState::new(sid);
    common::login_pilot(ctx, &mut s, user, pass).await;
    call(ctx, &mut s, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)]).await;
    s
}

async fn call(ctx: &ServerContext, s: &mut SessionState, svc: &str, method: &str, args: &[PyRep]) -> PyRep {
    ctx.dispatch_rpc(s, svc, None, method, args, &HashMap::new(), None)
        .await
        .unwrap_or_else(|e| panic!("{svc}.{method} failed: {} {}", e.code, e.message))
}

fn events(ctx: &ServerContext, char_id: i64, event: &str) -> Vec<Vec<PyRep>> {
    ctx.drain_notifications_for_char(char_id).into_iter().filter(|n| n.event == event).map(|n| n.args).collect()
}

#[tokio::test]
async fn test_personal_contacts_and_watchlist_presence_notifications() {
    // parity: notify:OnPersonalContactsAdded, notify:OnPersonalContactsUpdated, notify:OnPersonalContactsDeleted, notify:OnClientContactChange
    eve_testkit::attest(&[
        "notify:OnPersonalContactsAdded",
        "notify:OnPersonalContactsUpdated",
        "notify:OnPersonalContactsDeleted",
        "notify:OnClientContactChange",
    ]);
    let ctx = ServerContext::new();
    let mut s1 = select(&ctx, 601, "demo", "demo-secret", P1).await;
    let mut s2 = select(&ctx, 602, "other", "other-secret", P2).await;
    let uid3 = ctx.db.provision_player_account("contactp3", "contactp3-secret");
    let p3 = ctx.db.create_character(uid3, "Contact Pilot Three", 1, 1, 11, 17, &ctx.sde).expect("create p3").character_id;
    let _s3 = select(&ctx, 603, "contactp3", "contactp3-secret", p3).await;
    for c in [P1, P2, p3] {
        ctx.drain_notifications_for_char(c);
    }

    // AddContact(contactID, relationshipID, inWatchlist, sendNotification, message) — the client's
    // AddressBookSvc.AddToAddressBook call. OnPersonalContactsAdded(contacts, relationshipID, watchlist,
    // labelMask) goes to the owner only; each contact exposes `contactID` and `isOnline`.
    call(&ctx, &mut s1, "charMgr", "AddContact", &[PyRep::Integer(P2), PyRep::Real(5.0), PyRep::Bool(true), PyRep::Bool(false), PyRep::String(String::new())]).await;
    let added = PyRep::keyval([("contactID", PyRep::Integer(P2)), ("isOnline", PyRep::Bool(true))]);
    assert_eq!(
        events(&ctx, P1, "OnPersonalContactsAdded"),
        vec![vec![PyRep::List(vec![added]), PyRep::Real(5.0), PyRep::Bool(true), PyRep::Integer(0)]]
    );
    assert!(events(&ctx, P2, "OnPersonalContactsAdded").is_empty(), "the added character is not notified");

    // Watchlist presence: P2 logging out / back in sends OnClientContactChange(charID, online) to the
    // watcher P1 only (P3 does not watch P2; P2 never gets its own presence).
    call(&ctx, &mut s2, "authentication", "Logout", &[]).await;
    assert_eq!(events(&ctx, P1, "OnClientContactChange"), vec![vec![PyRep::Integer(P2), PyRep::Bool(false)]]);
    assert!(events(&ctx, p3, "OnClientContactChange").is_empty());
    let mut s2 = select(&ctx, 604, "other", "other-secret", P2).await;
    assert_eq!(events(&ctx, P1, "OnClientContactChange"), vec![vec![PyRep::Integer(P2), PyRep::Bool(true)]]);
    assert!(events(&ctx, p3, "OnClientContactChange").is_empty());
    assert!(events(&ctx, P2, "OnClientContactChange").is_empty());

    // A watcher that P2 has blocked gets no presence updates.
    call(&ctx, &mut s2, "charMgr", "BlockOwners", &[PyRep::List(vec![PyRep::Integer(P1)])]).await;
    call(&ctx, &mut s2, "authentication", "Logout", &[]).await;
    assert!(events(&ctx, P1, "OnClientContactChange").is_empty(), "blocked watcher must not see presence");

    // EditContact(contactID, relationshipID, inWatchlist): OnPersonalContactsUpdated(contactIDs,
    // standing, inWatchlist). EditContactsRelationshipID leaves the watchlist as None (keep current).
    call(&ctx, &mut s1, "charMgr", "EditContact", &[PyRep::Integer(P2), PyRep::Real(-5.0), PyRep::Bool(false)]).await;
    assert_eq!(
        events(&ctx, P1, "OnPersonalContactsUpdated"),
        vec![vec![PyRep::List(vec![PyRep::Integer(P2)]), PyRep::Real(-5.0), PyRep::Bool(false)]]
    );
    call(&ctx, &mut s1, "charMgr", "EditContactsRelationshipID", &[PyRep::List(vec![PyRep::Integer(P2)]), PyRep::Real(10.0)]).await;
    assert_eq!(
        events(&ctx, P1, "OnPersonalContactsUpdated"),
        vec![vec![PyRep::List(vec![PyRep::Integer(P2)]), PyRep::Real(10.0), PyRep::None]]
    );
    assert!(events(&ctx, p3, "OnPersonalContactsUpdated").is_empty());

    // DeleteContacts(contactIDs): OnPersonalContactsDeleted(contacts) with `contactID` entries; the
    // contact list is empty afterwards.
    call(&ctx, &mut s1, "charMgr", "DeleteContacts", &[PyRep::List(vec![PyRep::Integer(P2)])]).await;
    let deleted = PyRep::keyval([("contactID", PyRep::Integer(P2))]);
    assert_eq!(events(&ctx, P1, "OnPersonalContactsDeleted"), vec![vec![PyRep::List(vec![deleted])]]);
    assert!(events(&ctx, p3, "OnPersonalContactsDeleted").is_empty());
    let list = call(&ctx, &mut s1, "charMgr", "GetContactList", &[]).await;
    let Some(PyRep::Rowset { rows, .. }) = list.dict_get("addresses") else { panic!("addresses rowset") };
    assert!(rows.is_empty(), "deleted contact must be gone: {rows:?}");
}
