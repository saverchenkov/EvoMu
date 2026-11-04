//! Integration tests for Social Chat, Mail, and Presence.
//! Covers:
//! - `feat:social.chat-item-fitting-hyperlinks`
//! - `feat:social.mail-mailbox-limits`
//! - `feat:social.wormhole-local-chat-delayed`

#![forbid(unsafe_code)]

use crate::common;
use eve_db::{MailRecord, MailingListRecord};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::{NotificationRecord, ServerContext};
use std::collections::HashMap;

const JITA_SYSTEM_ID: u32 = 30_000_142;
const WORMHOLE_SYSTEM_ID: u32 = 31_000_001;
const P1: i64 = 90_000_001;
const P2: i64 = 90_000_002;
const P3: i64 = 90_000_003;

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, eve_proto::BridgeRpcError> {
    let kw = HashMap::new();
    ctx.dispatch_rpc(sess, service, None, method, args, &kw, None).await
}

#[tokio::test]
async fn test_social_chat_item_fitting_hyperlinks() {
    // parity:feat:social.chat-item-fitting-hyperlinks
    eve_testkit::attest(&["feat:social.chat-item-fitting-hyperlinks"]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let mut s1 = SessionState::new(8401);
    common::login_pilot(&ctx, &mut s1, "demo", "demo-secret").await;
    rpc(&ctx, &mut s1, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(P1)])
        .await
        .expect("SelectCharacterID s1");

    let mut s2 = SessionState::new(8402);
    common::login_pilot(&ctx, &mut s2, "other", "other-secret").await;
    rpc(&ctx, &mut s2, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(P2)])
        .await
        .expect("SelectCharacterID s2");

    // 1. Prepare target objects for each of the 5 hyperlink kinds
    let ship_item_id = s1.ship_id.expect("P1 active ship item ID");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ship_it) = snap.items.get_mut(&ship_item_id) {
            ship_it.type_id = 587; // Rifter
        }
    });

    // (b) Fitting saved by sender P1
    let fit_rep = rpc(
        &ctx,
        &mut s1,
        "charFittingMgr",
        "SaveFitting",
        &[
            PyRep::Integer(P1),
            PyRep::keyval([
                ("name", PyRep::String("Fleet Rifter".into())),
                ("shipTypeID", PyRep::Integer(587)),
                ("description", PyRep::String("Fit link".into())),
                (
                    "fitData",
                    PyRep::List(vec![PyRep::Tuple(vec![
                        PyRep::Integer(434),
                        PyRep::Integer(19),
                        PyRep::Integer(1),
                    ])]),
                ),
            ]),
        ],
    )
    .await
    .expect("SaveFitting");
    let fit_id = fit_rep.as_i64().expect("SaveFitting must return integer fit_id");

    // (c) Overview preset saved by P1
    let ov_rep = rpc(
        &ctx,
        &mut s1,
        "overviewPresetMgr",
        "StoreLinkAndGetID",
        &[PyRep::String("PvP Focus".into()), PyRep::String("{\"tab\":\"pvp\"}".into())],
    )
    .await
    .expect("StoreLinkAndGetID");
    let ov_sqid = ov_rep
        .dict_get("sqID")
        .and_then(|v| v.as_i64())
        .expect("StoreLinkAndGetID must return sqID");

    // (d) Killmail
    let kill_id = 8881i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.killmails.insert(
            kill_id,
            eve_db::KillmailRecord {
                kill_id,
                victim_character_id: P2,
                victim_corp_id: 2001,
                attacker_character_id: P1,
                solar_system_id: JITA_SYSTEM_ID,
                ship_type_id: 587,
                killed_unix: 1_700_000_000,
                ..Default::default()
            },
        );
    });

    // (e) Bookmark folder with recipient P2 in accessGroupIDs
    let bm_rep = rpc(
        &ctx,
        &mut s1,
        "bookmark",
        "AddFolder",
        &[
            PyRep::String("HQ Bookmarks".into()),
            PyRep::Bool(false),
            PyRep::List(vec![PyRep::Integer(P2)]),
        ],
    )
    .await
    .expect("AddFolder");
    let folder_id = bm_rep
        .dict_get("folderID")
        .and_then(|v| v.as_i64())
        .expect("AddFolder must return folderID");

    // 2. Rich message containing all 5 hyperlink schemes
    let rich_msg = format!(
        "<url=showinfo:587//{ship_item_id}>Rifter</url> <url=fitting:587:434;1::>Fit #{fit_id}</url> <url=killReport:{kill_id}:abcdef>KM {kill_id}</url> <url=bookmarkFolder:{folder_id}>HQ BM</url> <url=overviewPreset:{ov_sqid}>PvP OV</url>"
    );

    // Join chat channel, send rich_msg, verify delivery to P2
    let chan_id = "player_channel:social_hyperlinks".to_string();
    rpc(&ctx, &mut s1, "XmppChatMgr", "AddCharacterToChannel", &[PyRep::String(chan_id.clone())])
        .await
        .expect("s1 join channel");
    rpc(&ctx, &mut s2, "XmppChatMgr", "AddCharacterToChannel", &[PyRep::String(chan_id.clone())])
        .await
        .expect("s2 join channel");

    let _ = ctx.drain_notifications_for_char(P2);

    let chat_rep = rpc(
        &ctx,
        &mut s1,
        "LSC",
        "SendMessage",
        &[PyRep::String(chan_id.clone()), PyRep::String(rich_msg.clone())],
    )
    .await
    .expect("LSC.SendMessage");
    assert_eq!(chat_rep.dict_get("text"), Some(&PyRep::String(rich_msg.clone())));

    let p2_chat_notifs = ctx.drain_notifications_for_char(P2);
    let chat_notif = p2_chat_notifs
        .iter()
        .find(|n| n.event == "OnClientEvent_ChatMessageSent")
        .expect("Recipient P2 must receive OnClientEvent_ChatMessageSent");
    let delivered_chat_text = chat_notif
        .args
        .get(3)
        .and_then(|v| v.as_str())
        .expect("delivered chat text in OnClientEvent_ChatMessageSent");
    assert_eq!(delivered_chat_text, rich_msg);

    // Send through mailMgr and verify recipient P2 receives identical hyperlink text via GetBody
    let mail_id_rep = rpc(
        &ctx,
        &mut s1,
        "mailMgr",
        "SendMail",
        &[
            PyRep::List(vec![PyRep::Integer(P2)]),
            PyRep::None,
            PyRep::None,
            PyRep::String("Hyperlink Subject".into()),
            PyRep::String(rich_msg.clone()),
        ],
    )
    .await
    .expect("SendMail with hyperlinks");
    let mail_id = mail_id_rep.as_i64().expect("SendMail must return integer mail_id");

    let body_rep = rpc(&ctx, &mut s2, "mailMgr", "GetBody", &[PyRep::Integer(mail_id)])
        .await
        .expect("mailMgr.GetBody");
    let delivered_mail_text = match &body_rep {
        PyRep::Buffer(b) => {
            String::from_utf8(miniz_oxide::inflate::decompress_to_vec_zlib(b).expect("decompress mail body"))
                .expect("utf8 mail body")
        }
        _ => body_rep.as_str().expect("string mail body").to_string(),
    };
    assert_eq!(delivered_mail_text, rich_msg);

    // 3. Resolve all 5 hyperlinks via recipient session s2
    // (a) showinfo: dogmaIM.ItemGetInfo
    let item_info = rpc(&ctx, &mut s2, "dogmaIM", "ItemGetInfo", &[PyRep::Integer(ship_item_id)])
        .await
        .expect("Recipient s2 resolves showinfo itemID via dogmaIM.ItemGetInfo");
    assert_eq!(item_info.dict_get("typeID").and_then(|v| v.as_i64()), Some(587));
    assert_eq!(item_info.dict_get("ownerID").and_then(|v| v.as_i64()), Some(P1));

    // (b) fitting: charFittingMgr.SaveFitting
    let s2_saved_fit = rpc(
        &ctx,
        &mut s2,
        "charFittingMgr",
        "SaveFitting",
        &[
            PyRep::Integer(P2),
            PyRep::keyval([
                ("name", PyRep::String("Fleet Rifter".into())),
                ("shipTypeID", PyRep::Integer(587)),
                ("description", PyRep::String("Imported from chat link".into())),
                (
                    "fitData",
                    PyRep::List(vec![PyRep::Tuple(vec![
                        PyRep::Integer(434),
                        PyRep::Integer(19),
                        PyRep::Integer(1),
                    ])]),
                ),
            ]),
        ],
    )
    .await
    .expect("Recipient s2 saves fitting from chat hyperlink DNA");
    let s2_fit_id = s2_saved_fit.as_i64().expect("s2_fit_id");
    let s2_fittings = rpc(&ctx, &mut s2, "charFittingMgr", "GetFittings", &[PyRep::Integer(P2)])
        .await
        .expect("Recipient s2 GetFittings");
    let fits_debug = format!("{s2_fittings:?}");
    assert!(fits_debug.contains("Fleet Rifter") && fits_debug.contains(&s2_fit_id.to_string()));

    // (c) killReport: warStatisticMgr.GetKillMail
    let km = rpc(&ctx, &mut s2, "warStatisticMgr", "GetKillMail", &[PyRep::Integer(kill_id)])
        .await
        .expect("Recipient s2 GetKillMail");
    assert_eq!(km.dict_get("killID").and_then(|v| v.as_i64()), Some(kill_id));
    assert_eq!(km.dict_get("victimCharacterID").and_then(|v| v.as_i64()), Some(P2));

    // (d) bookmarkFolder: bookmark.GetFolders
    let folders = rpc(&ctx, &mut s2, "bookmark", "GetFolders", &[])
        .await
        .expect("Recipient s2 GetFolders");
    let folders_debug = format!("{folders:?}");
    assert!(folders_debug.contains("HQ Bookmarks") && folders_debug.contains(&folder_id.to_string()));

    // (e) overviewPreset: overviewPresetMgr.GetStoredPreset
    let preset = rpc(&ctx, &mut s2, "overviewPresetMgr", "GetStoredPreset", &[PyRep::Integer(ov_sqid)])
        .await
        .expect("Recipient s2 GetStoredPreset");
    assert_eq!(preset.as_str(), Some("{\"tab\":\"pvp\"}"));
}

#[tokio::test]
async fn test_social_mail_mailbox_and_notification_limits() {
    // parity:feat:social.mail-mailbox-limits
    eve_testkit::attest(&["feat:social.mail-mailbox-limits"]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let mut s1 = SessionState::new(8411);
    common::login_pilot(&ctx, &mut s1, "demo", "demo-secret").await;
    rpc(&ctx, &mut s1, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(P1)])
        .await
        .expect("SelectCharacterID s1");

    let mut s2 = SessionState::new(8412);
    common::login_pilot(&ctx, &mut s2, "other", "other-secret").await;
    rpc(&ctx, &mut s2, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(P2)])
        .await
        .expect("SelectCharacterID s2");

    // 1. Mail body length limit: 8,000 chars is allowed, 8,001 chars is rejected with MailBodyTooLong
    let exact_8000_body = "x".repeat(8_000);
    let ok_send = rpc(
        &ctx,
        &mut s1,
        "mailMgr",
        "SendMail",
        &[
            PyRep::List(vec![PyRep::Integer(P2)]),
            PyRep::None,
            PyRep::None,
            PyRep::String("Exact 8000".into()),
            PyRep::String(exact_8000_body),
        ],
    )
    .await
    .expect("Mail body of exactly 8,000 characters must succeed");
    assert!(ok_send.as_i64().unwrap_or(0) > 0);

    let over_8000_body = "x".repeat(8_001);
    let body_err = rpc(
        &ctx,
        &mut s1,
        "mailMgr",
        "SendMail",
        &[
            PyRep::List(vec![PyRep::Integer(P2)]),
            PyRep::None,
            PyRep::None,
            PyRep::String("Over 8000".into()),
            PyRep::String(over_8000_body),
        ],
    )
    .await
    .expect_err("Mail body > 8,000 characters must be rejected");
    assert_eq!(body_err.code, "MailBodyTooLong");

    // 2. Mailbox limit: character keeps at most 3,000 player mails counting mailing-list and Trash mails.
    // Beyond that, older mails are no longer listed until some are deleted.
    ctx.db.with_mut_snapshot(|snap| {
        snap.mails.clear();
        snap.mail_timestamps.clear();
        snap.read_mails.clear();
        snap.trashed_mails.clear();
        snap.deleted_mails.clear();

        // Mail #1 belongs to mailing list 555 where P2 is a member
        let mut ml = MailingListRecord {
            list_id: 555,
            name: "CorpAnnounce".into(),
            owner_id: P1,
            default_access: 1,
            default_role: 1,
            cost: 0,
            members: HashMap::new(),
            entity_access: Default::default(),
            mail_ids: [1].into_iter().collect(),
            welcome_mail: None,
        };
        ml.members.insert(P2, 1);
        snap.mailing_lists.insert(555, ml);

        for i in 1..=3_000 {
            snap.mails.push(MailRecord {
                message_id: i,
                sender_id: P1,
                recipient_id: if i == 1 { 555 } else { P2 },
                subject: format!("Mail {i}"),
                body: "Body text".into(),
            });
            snap.mail_timestamps.insert(i, 1_000_000 + (i as u64));
        }
    });

    // Move mail #2 to Trash
    rpc(&ctx, &mut s2, "mailMgr", "MoveToTrash", &[PyRep::List(vec![PyRep::Integer(2)])])
        .await
        .expect("MoveToTrash");

    // Now send mail #3,001. P2 now has 3,001 mails (including mailing-list #1 and Trash #2).
    let m3001 = rpc(
        &ctx,
        &mut s1,
        "mailMgr",
        "SendMail",
        &[
            PyRep::List(vec![PyRep::Integer(P2)]),
            PyRep::None,
            PyRep::None,
            PyRep::String("Mail 3001".into()),
            PyRep::String("Newest mail".into()),
        ],
    )
    .await
    .expect("3,001st mail must succeed");
    let m3001_id = m3001.as_i64().expect("m3001_id");

    // SyncMail / GetMailHeaders must list the newest 3,000 mails (2..=3001) and exclude the oldest (#1).
    // Because mail #2 is in Trash, the active inbox listing contains 2,999 mails (and 1 in Trash).
    let hdrs_rep = rpc(&ctx, &mut s2, "mailMgr", "GetMailHeaders", &[])
        .await
        .expect("GetMailHeaders");
    let PyRep::Rowset { rows: h_rows, .. } = hdrs_rep else { panic!("expected Rowset") };
    let listed_ids: Vec<i64> = h_rows.iter().filter_map(|r| r[0].as_i64()).collect();
    assert_eq!(listed_ids.len(), 2_999, "Active inbox headers exclude trashed mail");
    assert!(listed_ids.contains(&m3001_id), "Newest mail 3,001 is listed");
    assert!(!listed_ids.contains(&1), "Oldest mail #1 is omitted beyond the 3,000-mail window");

    // EmptyTrash permanently deletes mail #2 -> oldest mail #1 re-enters the active 3,000-mail window!
    rpc(&ctx, &mut s2, "mailMgr", "EmptyTrash", &[])
        .await
        .expect("EmptyTrash");
    let hdrs_after = rpc(&ctx, &mut s2, "mailMgr", "GetMailHeaders", &[])
        .await
        .expect("GetMailHeaders after EmptyTrash");
    let PyRep::Rowset { rows: h_rows_after, .. } = hdrs_after else { panic!("expected Rowset") };
    let listed_after: Vec<i64> = h_rows_after.iter().filter_map(|r| r[0].as_i64()).collect();
    assert_eq!(listed_after.len(), 3_000);
    assert!(listed_after.contains(&1), "Mail #1 re-enters the listed window once Trash is deleted");

    // 3. Notification limit: notifications are capped at 500
    {
        let mut notifs = ctx.notification_records.write().unwrap();
        notifs.clear();
        for i in 1..=510 {
            notifs.push(NotificationRecord {
                notification_id: 900_000 + i,
                type_id: 10,
                sender_id: P1,
                receiver_id: P2,
                processed: false,
                group_id: 1,
                created: 132_000_000_000_000_000 + i,
                data: "{}".into(),
                deleted: false,
                interacted: false,
            });
        }
    }
    let all_notifs = rpc(&ctx, &mut s2, "notificationMgr", "GetAllNotifications", &[])
        .await
        .expect("GetAllNotifications");
    let PyRep::ObjectEx { list: all_rows, .. } = all_notifs else { panic!("expected CRowset ObjectEx") };
    assert_eq!(all_rows.len(), 500, "GetAllNotifications capped at 500");

    let unproc_notifs = rpc(&ctx, &mut s2, "notificationMgr", "GetUnprocessed", &[])
        .await
        .expect("GetUnprocessed");
    let PyRep::ObjectEx { list: unproc_rows, .. } = unproc_notifs else { panic!("expected CRowset ObjectEx") };
    assert_eq!(unproc_rows.len(), 500, "GetUnprocessed capped at 500");
}

#[tokio::test]
async fn test_social_wormhole_local_chat_delayed() {
    // parity:feat:social.wormhole-local-chat-delayed
    eve_testkit::attest(&["feat:social.wormhole-local-chat-delayed"]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    ctx.db.with_mut_snapshot(|snap| {
        snap.accounts.insert(
            "pilot3".to_string(),
            eve_db::AccountRecord {
                user_id: 3,
                username: "pilot3".to_string(),
                password_token: eve_db::WorldRepository::hash_password_token("pilot3", "pilot3-secret"),
                role: eve_db::service_roles::ROLE_PLAYER_DEFAULT,
                banned_until_unix: None,
                ban_reason: None,
            },
        );
        let base_char = snap.characters.get(&P1).cloned().unwrap();
        snap.characters.insert(
            P3,
            eve_db::CharacterRecord {
                character_id: P3,
                user_id: 3,
                name: "Third Pilot".to_string(),
                corporation_id: 1_000_045,
                solar_system_id: JITA_SYSTEM_ID,
                ..base_char
            },
        );
    });

    let mut s1 = SessionState::new(8421);
    common::login_pilot(&ctx, &mut s1, "demo", "demo-secret").await;
    rpc(&ctx, &mut s1, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(P1)])
        .await
        .expect("SelectCharacterID s1");

    let mut s2 = SessionState::new(8422);
    common::login_pilot(&ctx, &mut s2, "other", "other-secret").await;
    rpc(&ctx, &mut s2, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(P2)])
        .await
        .expect("SelectCharacterID s2");

    let mut s3 = SessionState::new(8423);
    common::login_pilot(&ctx, &mut s3, "pilot3", "pilot3-secret").await;
    rpc(&ctx, &mut s3, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(P3)])
        .await
        .expect("SelectCharacterID s3");

    // 1. K-space Local is unchanged: simply entering/joining immediately lists members
    let k_chan = format!("solarsystemid2:{JITA_SYSTEM_ID}");
    rpc(&ctx, &mut s1, "LSC", "JoinChannel", &[PyRep::String(k_chan.clone())])
        .await
        .expect("s1 join K-space local");
    rpc(&ctx, &mut s2, "LSC", "JoinChannel", &[PyRep::String(k_chan.clone())])
        .await
        .expect("s2 join K-space local");

    let k_members = rpc(&ctx, &mut s1, "LSC", "GetMembers", &[PyRep::String(k_chan.clone())])
        .await
        .expect("GetMembers K-space");
    let Some(PyRep::Rowset { rows: k_rows, .. }) = k_members.dict_get("members") else { panic!("expected Rowset") };
    let k_mids: Vec<i64> = k_rows.iter().filter_map(|r| r[0].as_i64()).collect();
    assert!(k_mids.contains(&P1) && k_mids.contains(&P2), "K-space Local lists pilots immediately on join without speaking");

    // 2. J-space delayed Local:
    // Move pilots to wormhole system WORMHOLE_SYSTEM_ID (31000001)
    let wh_chan = format!("solarsystemid2:{WORMHOLE_SYSTEM_ID}");
    s1.solar_system_id = Some(WORMHOLE_SYSTEM_ID);
    s2.solar_system_id = Some(WORMHOLE_SYSTEM_ID);
    s3.solar_system_id = Some(WORMHOLE_SYSTEM_ID);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&P1) { c.solar_system_id = WORMHOLE_SYSTEM_ID; }
        if let Some(c) = snap.characters.get_mut(&P2) { c.solar_system_id = WORMHOLE_SYSTEM_ID; }
        if let Some(c) = snap.characters.get_mut(&P3) { c.solar_system_id = WORMHOLE_SYSTEM_ID; }
    });

    // P1 and P2 enter wormhole and join channel
    rpc(&ctx, &mut s1, "LSC", "JoinChannel", &[PyRep::String(wh_chan.clone())])
        .await
        .expect("s1 join wh");
    rpc(&ctx, &mut s2, "LSC", "JoinChannel", &[PyRep::String(wh_chan.clone())])
        .await
        .expect("s2 join wh");

    // (a) Simply entering does NOT list pilots in wormhole Local
    let wh_init = rpc(&ctx, &mut s2, "LSC", "GetMembers", &[PyRep::String(wh_chan.clone())])
        .await
        .expect("wh GetMembers initial");
    let Some(PyRep::Rowset { rows: wh_r0, .. }) = wh_init.dict_get("members") else { panic!("expected Rowset") };
    assert!(wh_r0.is_empty(), "In J-space, Local lists no pilots before they speak");

    // (b) Pilot P1 speaks in Local -> P1 is now listed; P2 (who has not spoken) is still not listed
    rpc(
        &ctx,
        &mut s1,
        "LSC",
        "SendMessage",
        &[PyRep::String(wh_chan.clone()), PyRep::String("First wormhole contact".into())],
    )
    .await
    .expect("s1 speak in wh local");

    let wh_spoke = rpc(&ctx, &mut s2, "LSC", "GetMembers", &[PyRep::String(wh_chan.clone())])
        .await
        .expect("wh GetMembers after speaking");
    let Some(PyRep::Rowset { rows: wh_r1, .. }) = wh_spoke.dict_get("members") else { panic!("expected Rowset") };
    assert_eq!(wh_r1.len(), 1, "Only the pilot who spoke is listed in J-space Local");
    assert_eq!(wh_r1[0][0], PyRep::Integer(P1));

    // (c) Pilot P1 leaves wormhole while witness P2 is still present -> P1 stays listed!
    rpc(&ctx, &mut s1, "LSC", "LeaveChannel", &[PyRep::String(wh_chan.clone())])
        .await
        .expect("s1 leave wh channel");

    let wh_p1_left = rpc(&ctx, &mut s2, "LSC", "GetMembers", &[PyRep::String(wh_chan.clone())])
        .await
        .expect("wh GetMembers after P1 leaves");
    let Some(PyRep::Rowset { rows: wh_r2, .. }) = wh_p1_left.dict_get("members") else { panic!("expected Rowset") };
    assert_eq!(wh_r2.len(), 1, "Speaker P1 stays listed as long as witness P2 is still in the system");
    assert_eq!(wh_r2[0][0], PyRep::Integer(P1));

    // (d) Pilot P3 enters wormhole: entering does NOT add P3, and does NOT remove P1
    rpc(&ctx, &mut s3, "LSC", "JoinChannel", &[PyRep::String(wh_chan.clone())])
        .await
        .expect("s3 join wh channel");

    let wh_p3_entered = rpc(&ctx, &mut s2, "LSC", "GetMembers", &[PyRep::String(wh_chan.clone())])
        .await
        .expect("wh GetMembers after P3 entered");
    let Some(PyRep::Rowset { rows: wh_r3, .. }) = wh_p3_entered.dict_get("members") else { panic!("expected Rowset") };
    assert_eq!(wh_r3.len(), 1, "Entering did not add P3 nor remove P1");
    assert_eq!(wh_r3[0][0], PyRep::Integer(P1));

    // (e) Witness P2 leaves the system / logs off -> every pilot who saw P1 speak has left!
    // P1 is now removed from Local, leaving the member list empty!
    rpc(&ctx, &mut s2, "LSC", "LeaveChannel", &[PyRep::String(wh_chan.clone())])
        .await
        .expect("s2 witness leaves");

    let wh_empty = rpc(&ctx, &mut s3, "LSC", "GetMembers", &[PyRep::String(wh_chan)])
        .await
        .expect("wh GetMembers after all witnesses left");
    let Some(PyRep::Rowset { rows: wh_r4, .. }) = wh_empty.dict_get("members") else { panic!("expected Rowset") };
    assert!(wh_r4.is_empty(), "Speaker P1 is removed once every witness who saw them has left the system");
}
