//! Integration tests closing canonical plan rows SO-1 and SO-7
//! and their blocking lifecycle and social features.

#![forbid(unsafe_code)]

use crate::common;
use eve_db::MailRecord;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::{NotificationRecord, ServerContext};
use std::collections::HashMap;

const JITA_SYSTEM_ID: u32 = 30_000_142;
const P1: i64 = 90_000_001;
const P2: i64 = 90_000_002;

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
async fn test_so1_mail_no_global_cap() {
    // parity: plan:SO-1, feat:lifecycle.mail-no-global-cap
    eve_testkit::attest(&["plan:SO-1", "feat:lifecycle.mail-no-global-cap"]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    // 1. Seed 3,001 dummy mails server-wide across many recipients (simulating high server load)
    ctx.db.with_mut_snapshot(|snap| {
        for i in 1..=3001 {
            snap.mails.push(MailRecord {
                message_id: 100_000 + i,
                sender_id: P1,
                recipient_id: 80_000_000 + (i % 50),
                subject: format!("Mail {i}"),
                body: format!("Body {i}"),
            });
        }
    });

    assert!(ctx.db.with_snapshot(|snap| snap.mails.len()) >= 3001);

    // Sender logs in
    let mut s1 = SessionState::new(8001);
    common::login_pilot(&ctx, &mut s1, "demo", "demo-secret").await;
    rpc(&ctx, &mut s1, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(P1)])
        .await
        .expect("SelectCharacterID s1");

    // Recipient P2 is offline. Send mail number 3,002 to offline character P2.
    let send_rep = rpc(
        &ctx,
        &mut s1,
        "mailMgr",
        "SendMail",
        &[
            PyRep::List(vec![PyRep::Integer(P2)]),
            PyRep::None,
            PyRep::None,
            PyRep::String("Mail 3002 Subject".into()),
            PyRep::String("Mail 3002 Body for offline recipient".into()),
        ],
    )
    .await
    .expect("SendMail 3002 must succeed despite >3,000 global mails");

    let mail_id = send_rep.as_i64().expect("mail_id");
    assert!(mail_id > 0);

    // Recipient logs in
    let mut s2 = SessionState::new(8002);
    common::login_pilot(&ctx, &mut s2, "other", "other-secret").await;
    rpc(&ctx, &mut s2, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(P2)])
        .await
        .expect("SelectCharacterID s2");

    // Mail 3,002 appears in offline recipient's SyncMail at next login
    let sync_rep = rpc(&ctx, &mut s2, "mailMgr", "SyncMail", &[PyRep::None, PyRep::None])
        .await
        .expect("SyncMail");
    let sync_text = format!("{sync_rep:?}");
    assert!(sync_text.contains(&mail_id.to_string()) || sync_text.contains("Mail 3002 Subject"));

    // Recipient reads body
    let body_rep = rpc(&ctx, &mut s2, "mailMgr", "GetBody", &[PyRep::Integer(mail_id)])
        .await
        .expect("GetBody");
    let body_str = match &body_rep {
        PyRep::Buffer(b) => {
            String::from_utf8(miniz_oxide::inflate::decompress_to_vec_zlib(b).unwrap_or_default())
                .unwrap_or_default()
        }
        _ => body_rep.as_str().unwrap_or("").to_string(),
    };
    assert!(body_str.contains("Mail 3002 Body for offline recipient"));

    // 2. Per-recipient mailbox quota (1,000 mails) is enforced across ALL recipients in a multi-recipient SendMail
    let full_recipient = 80_000_999i64;
    ctx.db.with_mut_snapshot(|snap| {
        for i in 1..=1_000 {
            snap.mails.push(MailRecord {
                message_id: 200_000 + i,
                sender_id: P1,
                recipient_id: full_recipient,
                subject: format!("Quota {i}"),
                body: "Quota body".into(),
            });
        }
    });
    let quota_err = rpc(
        &ctx,
        &mut s1,
        "mailMgr",
        "SendMail",
        &[
            PyRep::List(vec![PyRep::Integer(P2), PyRep::Integer(full_recipient)]),
            PyRep::None,
            PyRep::None,
            PyRep::String("Multi Recipient Quota Check".into()),
            PyRep::String("Second recipient is at 1,000 mailbox cap".into()),
        ],
    )
    .await
    .expect_err("Multi-recipient SendMail must reject when any recipient exceeds 1,000-mail quota");
    assert_eq!(quota_err.code, "MailQuotaExceeded");

    // 3. Mail read/unread state, custom labels, trash, and deletion (plan:SO-1)
    rpc(&ctx, &mut s2, "mailMgr", "MarkAsRead", &[PyRep::List(vec![PyRep::Integer(mail_id)])])
        .await
        .expect("MarkAsRead");
    assert!(ctx.db.with_snapshot(|s| s.read_mails.contains(&(P2, mail_id))));

    let label_rep = rpc(
        &ctx,
        &mut s2,
        "mailMgr",
        "CreateLabel",
        &[PyRep::String("Priority".into()), PyRep::String("#ff0000".into())],
    )
    .await
    .expect("CreateLabel");
    let custom_label_id = label_rep.as_i64().expect("custom_label_id");
    assert!(custom_label_id >= 64);

    rpc(
        &ctx,
        &mut s2,
        "mailMgr",
        "AssignLabels",
        &[PyRep::List(vec![PyRep::Integer(mail_id)]), PyRep::Integer(custom_label_id)],
    )
    .await
    .expect("AssignLabels");
    let labels_rep = rpc(&ctx, &mut s2, "mailMgr", "GetLabels", &[])
        .await
        .expect("GetLabels");
    assert!(format!("{labels_rep:?}").contains("Priority"));

    rpc(&ctx, &mut s2, "mailMgr", "MoveToTrash", &[PyRep::List(vec![PyRep::Integer(mail_id)])])
        .await
        .expect("MoveToTrash");
    assert!(ctx.db.with_snapshot(|s| s.trashed_mails.contains(&(P2, mail_id))));

    rpc(&ctx, &mut s2, "mailMgr", "MoveFromTrash", &[PyRep::List(vec![PyRep::Integer(mail_id)])])
        .await
        .expect("MoveFromTrash");
    assert!(!ctx.db.with_snapshot(|s| s.trashed_mails.contains(&(P2, mail_id))));

    rpc(&ctx, &mut s2, "mailMgr", "DeleteMail", &[PyRep::List(vec![PyRep::Integer(mail_id)])])
        .await
        .expect("DeleteMail");
    assert!(ctx.db.with_snapshot(|s| s.deleted_mails.contains(&(P2, mail_id))));

    // 4. Mailing lists (Create, Join, SendMail to list, Leave) and Notification center (plan:SO-1)
    let ml_rep = rpc(
        &ctx,
        &mut s1,
        "mailingListsMgr",
        "Create",
        &[PyRep::String("Fleet Alerts".into())],
    )
    .await
    .expect("mailingListsMgr.Create");
    let list_id = ml_rep
        .as_i64()
        .or_else(|| ml_rep.dict_get("listID").and_then(|v| v.as_i64()))
        .expect("mailing list ID");
    rpc(&ctx, &mut s2, "mailingListsMgr", "Join", &[PyRep::Integer(list_id)])
        .await
        .expect("mailingListsMgr.Join");
    let list_mail_rep = rpc(
        &ctx,
        &mut s1,
        "mailMgr",
        "SendMail",
        &[
            PyRep::List(vec![]),
            PyRep::Integer(list_id),
            PyRep::None,
            PyRep::String("List Broadcast".into()),
            PyRep::String("Broadcast to mailing list members".into()),
        ],
    )
    .await
    .expect("SendMail to mailing list");
    assert!(list_mail_rep.as_i64().unwrap_or(0) > 0);
    rpc(&ctx, &mut s2, "mailingListsMgr", "Leave", &[PyRep::Integer(list_id)])
        .await
        .expect("mailingListsMgr.Leave");

    // Seed a notification record and verify GetUnprocessedNotifications + MarkAsProcessed
    let notif_id = 990_001i64;
    ctx.notification_records.write().unwrap().push(NotificationRecord {
        notification_id: notif_id,
        type_id: 101,
        sender_id: 1_000_125,
        receiver_id: P2,
        processed: false,
        group_id: 12,
        created: ctx.now_unix() as i64,
        data: "planetID: 40009082\n".into(),
        deleted: false,
        interacted: false,
    });
    let unproc = rpc(&ctx, &mut s2, "notificationMgr", "GetUnprocessed", &[])
        .await
        .expect("GetUnprocessed");
    assert!(format!("{unproc:?}").contains(&notif_id.to_string()));
    rpc(
        &ctx,
        &mut s2,
        "notificationMgr",
        "MarkAsProcessed",
        &[PyRep::List(vec![PyRep::Integer(notif_id)])],
    )
    .await
    .expect("MarkAsProcessed");
    let unproc_after = rpc(&ctx, &mut s2, "notificationMgr", "GetUnprocessed", &[])
        .await
        .expect("GetUnprocessed after MarkAsProcessed");
    assert!(!format!("{unproc_after:?}").contains(&notif_id.to_string()));
}

#[tokio::test]
async fn test_so7_chat_item_fitting_hyperlinks_resolution() {
    // parity: plan:SO-7, feat:social.chat-item-fitting-hyperlinks
    eve_testkit::attest(&["plan:SO-7", "feat:social.chat-item-fitting-hyperlinks"]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let mut s1 = SessionState::new(8201);
    common::login_pilot(&ctx, &mut s1, "demo", "demo-secret").await;
    rpc(&ctx, &mut s1, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(P1)])
        .await
        .expect("SelectCharacterID s1");

    let mut s2 = SessionState::new(8202);
    common::login_pilot(&ctx, &mut s2, "other", "other-secret").await;
    rpc(&ctx, &mut s2, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(P2)])
        .await
        .expect("SelectCharacterID s2");

    // 1. Prepare target objects for each of the 5 hyperlink kinds (no .unwrap_or(1) fallbacks)
    let ship_item_id = s1.ship_id.expect("P1 active ship item ID");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ship_it) = snap.items.get_mut(&ship_item_id) {
            ship_it.type_id = 587; // Rifter
        }
    });

    // Fitting created by sender P1
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

    // Overview preset
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

    // Killmail
    let kill_id = 7771i64;
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

    // Bookmark folder with recipient P2 in accessGroupIDs ACL so recipient s2 can resolve it
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

    // Both s1 (P1) and s2 (P2) join the chat channel; s1 configures MOTD/ACL and sends rich_msg; s2 receives OnClientEvent_ChatMessageSent
    let chan_id = "player_channel:hyperlinks".to_string();
    rpc(&ctx, &mut s1, "XmppChatMgr", "AddCharacterToChannel", &[PyRep::String(chan_id.clone())])
        .await
        .expect("s1 join channel");
    rpc(&ctx, &mut s2, "XmppChatMgr", "AddCharacterToChannel", &[PyRep::String(chan_id.clone())])
        .await
        .expect("s2 join channel");
    rpc(
        &ctx,
        &mut s1,
        "XmppChatMgr",
        "SetMotd",
        &[PyRep::String(chan_id.clone()), PyRep::String("Welcome to Hyperlinks Channel".into())],
    )
    .await
    .expect("XmppChatMgr.SetMotd");
    let chans_rep = rpc(&ctx, &mut s2, "LSC", "GetChannels", &[])
        .await
        .expect("LSC.GetChannels");
    assert!(format!("{chans_rep:?}").contains("solarsystemid2"));

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
    assert_eq!(delivered_chat_text, rich_msg, "Chat hyperlink text delivered intact to recipient P2");

    // Local LSC channel delivery between P1 and P2 in Jita (30000142)
    let _ = ctx.drain_notifications_for_char(P2);
    rpc(
        &ctx,
        &mut s1,
        "LSC",
        "SendMessage",
        &[
            PyRep::String(format!("solarsystemid2:{JITA_SYSTEM_ID}")),
            PyRep::String("Local Intel Ping".into()),
        ],
    )
    .await
    .expect("LSC.SendMessage on local channel");
    let local_notifs = ctx.drain_notifications_for_char(P2);
    assert!(
        local_notifs.iter().any(|n| format!("{:?}", n.args).contains("Local Intel Ping")),
        "P2 in same solar system receives local LSC message"
    );

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
    assert_eq!(delivered_mail_text, rich_msg, "Mail hyperlink text delivered intact to recipient P2");

    // 3. Parse target IDs directly from the delivered message text and resolve ALL 5 via recipient session `s2` (`R-SO7-RECIPIENT`)
    let extract_between = |text: &str, prefix: &str, suffix: &str| -> String {
        let after = text.split_once(prefix).expect("hyperlink prefix present").1;
        after.split_once(suffix).expect("hyperlink suffix present").0.to_string()
    };

    // (a) showinfo:<typeID>//<itemID> -> recipient `s2` resolves typeID via SDE and itemID via dogmaIM.ItemGetInfo
    let showinfo_target = extract_between(delivered_chat_text, "<url=showinfo:", ">Rifter</url>");
    let (type_str, item_str) = showinfo_target.split_once("//").expect("showinfo typeID//itemID");
    let parsed_type_id: u32 = type_str.parse().expect("parsed showinfo typeID");
    let parsed_item_id: i64 = item_str.parse().expect("parsed showinfo itemID");
    assert_eq!(ctx.sde.types.get(&parsed_type_id).expect("SDE type").name, "Rifter");
    let item_info = rpc(&ctx, &mut s2, "dogmaIM", "ItemGetInfo", &[PyRep::Integer(parsed_item_id)])
        .await
        .expect("Recipient s2 resolves showinfo itemID via dogmaIM.ItemGetInfo");
    assert_eq!(item_info.dict_get("typeID").and_then(|v| v.as_i64()), Some(parsed_type_id as i64));
    assert_eq!(item_info.dict_get("ownerID").and_then(|v| v.as_i64()), Some(P1));

    // (b) fitting:<shipTypeID>:<modTypeID>;<qty>:: -> recipient `s2` parses fitting DNA from hyperlink and resolves/imports via charFittingMgr
    let fitting_dna = extract_between(delivered_chat_text, "<url=fitting:", ">Fit #");
    let (dna_ship_str, dna_rest) = fitting_dna.split_once(':').expect("fitting DNA ship:modules");
    let parsed_fit_ship_tid: i64 = dna_ship_str.parse().expect("parsed fitting shipTypeID");
    let (mod_str, qty_rest) = dna_rest.split_once(';').expect("mod;qty");
    let parsed_fit_mod_tid: i64 = mod_str.parse().expect("parsed modTypeID");
    let parsed_fit_mod_qty: i64 = qty_rest.trim_end_matches(':').parse().expect("parsed modQty");
    let parsed_fit_id: i64 = extract_between(delivered_chat_text, ">Fit #", "</url> <url=killReport:")
        .parse()
        .expect("parsed fit_id");
    assert_eq!(parsed_fit_id, fit_id);
    assert_eq!((parsed_fit_ship_tid, parsed_fit_mod_tid, parsed_fit_mod_qty), (587, 434, 1));
    let s2_saved_fit = rpc(
        &ctx,
        &mut s2,
        "charFittingMgr",
        "SaveFitting",
        &[
            PyRep::Integer(P2),
            PyRep::keyval([
                ("name", PyRep::String("Fleet Rifter".into())),
                ("shipTypeID", PyRep::Integer(parsed_fit_ship_tid)),
                ("description", PyRep::String("Imported from chat link".into())),
                (
                    "fitData",
                    PyRep::List(vec![PyRep::Tuple(vec![
                        PyRep::Integer(parsed_fit_mod_tid),
                        PyRep::Integer(19),
                        PyRep::Integer(parsed_fit_mod_qty),
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

    // (c) killReport:<killID>:<hash> -> recipient `s2` resolves killID via warStatisticMgr.GetKillMail
    let kr_target = extract_between(&delivered_mail_text, "<url=killReport:", ">KM ");
    let (kr_id_str, _kr_hash) = kr_target.split_once(':').expect("killID:hash");
    let parsed_kill_id: i64 = kr_id_str.parse().expect("parsed killID");
    let km = rpc(&ctx, &mut s2, "warStatisticMgr", "GetKillMail", &[PyRep::Integer(parsed_kill_id)])
        .await
        .expect("Recipient s2 GetKillMail");
    assert_eq!(km.dict_get("killID").and_then(|v| v.as_i64()), Some(parsed_kill_id));
    assert_eq!(km.dict_get("victimCharacterID").and_then(|v| v.as_i64()), Some(P2));

    // (d) bookmarkFolder:<folderID> -> recipient `s2` resolves folderID via bookmark.GetFolders
    let parsed_folder_id: i64 = extract_between(&delivered_mail_text, "<url=bookmarkFolder:", ">HQ BM</url>")
        .parse()
        .expect("parsed folderID");
    let folders = rpc(&ctx, &mut s2, "bookmark", "GetFolders", &[])
        .await
        .expect("Recipient s2 GetFolders");
    let folders_debug = format!("{folders:?}");
    assert!(
        folders_debug.contains("HQ Bookmarks") && folders_debug.contains(&parsed_folder_id.to_string()),
        "Recipient s2 must resolve shared bookmarkFolder hyperlink via bookmark.GetFolders"
    );

    // (e) overviewPreset:<sqID> -> recipient `s2` resolves sqID via overviewPresetMgr.GetStoredPreset
    let parsed_ov_sqid: i64 = extract_between(&delivered_mail_text, "<url=overviewPreset:", ">PvP OV</url>")
        .parse()
        .expect("parsed overviewPreset sqID");
    let preset = rpc(&ctx, &mut s2, "overviewPresetMgr", "GetStoredPreset", &[PyRep::Integer(parsed_ov_sqid)])
        .await
        .expect("Recipient s2 GetStoredPreset");
    assert_eq!(preset.as_str(), Some("{\"tab\":\"pvp\"}"));
}
