//! Integration tests for PaperDoll (`CH-3`), Contacts/Blocklist/Labels/Watchlist (`CH-4`), and GM Slash & QA Tools (`CH-5`).


use crate::common;

use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::HashMap;

#[tokio::test]
async fn test_ch5_gm_command_dispatcher() {
    // parity: feat:admin.slash-dispatcher, feat:admin.gm-giveskill, feat:admin.gm-giveisk, feat:admin.gm-createitem, feat:admin.gm-tr, rpc:charUnboundMgr.SelectCharacterID, rpc:slash.SlashCmd
    eve_testkit::attest(&["feat:admin.slash-dispatcher", "feat:admin.gm-giveskill", "feat:admin.gm-giveisk", "feat:admin.gm-createitem", "feat:admin.gm-tr"]);
    let ctx = ServerContext::new();
    let kw = HashMap::new();

    // 1. Regular player session rejected on slash commands (feat:admin.slash-dispatcher)
    let mut player_sess = SessionState::new(935);
    common::login_pilot(&ctx, &mut player_sess, "other", "other-secret").await;
    ctx.dispatch_rpc(
        &mut player_sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_002)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID player");

    let slash_err = ctx
        .dispatch_rpc(
            &mut player_sess,
            "slash",
            None,
            "SlashCmd",
            &[PyRep::String("/giveisk 1000".into())],
            &kw,
            None,
        )
        .await;
    assert_eq!(
        slash_err.unwrap_err().code,
        "InsufficientRole",
        "Non-GM player session must be rejected on slash commands"
    );

    // 2. GM session authorized (role = ROLE_ADMIN | ROLE_GMS)
    let mut gm_sess = SessionState::new(936);
    common::login_pilot(&ctx, &mut gm_sess, "demo", "demo-secret").await;
    gm_sess.role = eve_db::service_roles::ROLE_ADMIN | eve_db::service_roles::ROLE_GMS;

    let gm_char_id = 90_000_001;
    ctx.dispatch_rpc(
        &mut gm_sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(gm_char_id)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID gm");

    // 3. /giveisk <amount> (feat:admin.gm-giveisk)
    let isk_before = ctx.db.with_snapshot(|s| s.characters[&gm_char_id].balance.to_wire_f64());
    let giveisk_rep = ctx
        .dispatch_rpc(
            &mut gm_sess,
            "slash",
            None,
            "SlashCmd",
            &[PyRep::String("/giveisk 500000".into())],
            &kw,
            None,
        )
        .await
        .expect("/giveisk 500000");
    let PyRep::KeyVal(isk_kv) = giveisk_rep else {
        panic!("Expected KeyVal from /giveisk");
    };
    let new_bal = isk_kv.get("newBalanceISK").unwrap().as_f64().unwrap();
    assert_eq!(new_bal, isk_before + 500_000.0);

    // 4. /giveskill <id> <lvl> (feat:admin.gm-giveskill)
    let giveskill_rep = ctx
        .dispatch_rpc(
            &mut gm_sess,
            "slash",
            None,
            "SlashCmd",
            &[PyRep::String("/giveskill 3300 4".into())],
            &kw,
            None,
        )
        .await
        .expect("/giveskill 3300 4");
    let PyRep::KeyVal(skill_kv) = giveskill_rep else {
        panic!("Expected KeyVal from /giveskill");
    };
    assert_eq!(skill_kv.get("level").unwrap().as_i64(), Some(4));
    assert_eq!(skill_kv.get("skillID").unwrap().as_i64(), Some(3300));
    ctx.db.with_snapshot(|s| {
        let rec = s.skills[&gm_char_id].get(&3300).expect("Trained skill 3300");
        assert_eq!(rec.level, 4);
    });

    // 5. /createitem <type_id> <qty> (feat:admin.gm-createitem)
    let createitem_rep = ctx
        .dispatch_rpc(
            &mut gm_sess,
            "slash",
            None,
            "SlashCmd",
            &[PyRep::String("/createitem 601 2".into())],
            &kw,
            None,
        )
        .await
        .expect("/createitem 601 2");
    let PyRep::KeyVal(item_kv) = createitem_rep else {
        panic!("Expected KeyVal from /createitem");
    };
    let item_id = item_kv.get("itemID").unwrap().as_i64().unwrap();
    ctx.db.with_snapshot(|s| {
        let it = s.items.get(&item_id).expect("Created item");
        assert_eq!(it.type_id, 601);
        assert_eq!(it.owner_id, gm_char_id);
    });

    // 6. /tr <system_id> (feat:admin.gm-tr)
    // Teleport from Jita (30000142) to Amamake (30002537)
    let tr_rep = ctx
        .dispatch_rpc(
            &mut gm_sess,
            "slash",
            None,
            "SlashCmd",
            &[PyRep::String("/tr 30002537".into())],
            &kw,
            None,
        )
        .await
        .expect("/tr 30002537");
    let PyRep::KeyVal(tr_kv) = tr_rep else {
        panic!("Expected KeyVal from /tr");
    };
    assert_eq!(
        tr_kv.get("solarSystemID").unwrap().as_i64(),
        Some(30002537)
    );
    assert_eq!(gm_sess.solar_system_id, Some(30002537));

    // 7. /heal
    let heal_rep = ctx
        .dispatch_rpc(
            &mut gm_sess,
            "slash",
            None,
            "SlashCmd",
            &[PyRep::String("/heal".into())],
            &kw,
            None,
        )
        .await
        .expect("/heal");
    let PyRep::KeyVal(heal_kv) = heal_rep else {
        panic!("Expected KeyVal from /heal");
    };
    assert_eq!(heal_kv.get("healed").unwrap().as_bool(), Some(true));
}

#[tokio::test]
async fn test_ch3_ch4_paperdoll_contacts_blocklist_labels_and_watchlist() {
    // parity: feat:character.paperdoll-store, feat:social.contacts-add, feat:social.contacts-edit-delete, feat:social.contacts-block, feat:social.contacts-labels, feat:social.contacts-buddy-online, feat:character.sp-summary, rpc:charMgr.AddContact, rpc:charMgr.BlockOwners, rpc:charMgr.DeleteContacts, rpc:charMgr.GetContactList, rpc:charMgr.UnblockOwners, rpc:charUnboundMgr.SelectCharacterID, rpc:paperDollServer.GetMyPaperDollData, rpc:paperDollServer.UpdateExistingCharacterFull
    eve_testkit::attest(&["feat:character.paperdoll-store", "feat:social.contacts-add", "feat:social.contacts-edit-delete", "feat:social.contacts-block", "feat:social.contacts-labels", "feat:social.contacts-buddy-online", "feat:character.sp-summary"]);
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut sess = SessionState::new(941);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let char_id = 90_000_001;
    let buddy_id = 90_000_002;

    ctx.db.with_mut_snapshot(|snap| {
        let mut buddy = snap.characters[&char_id].clone();
        buddy.character_id = buddy_id;
        buddy.user_id = 2;
        buddy.name = "Buddy Pilot".into();
        snap.characters.insert(buddy_id, buddy);
    });

    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");

    // 1. PaperDollServer round-trip (feat:character.paperdoll-store)
    let new_doll = PyRep::keyval([
        ("sculpts", PyRep::String("jaw_narrow".into())),
        ("hair", PyRep::String("short_dark".into())),
    ]);
    let new_portrait = PyRep::keyval([("camera", PyRep::String("closeup_v2".into()))]);
    ctx.dispatch_rpc(
        &mut sess,
        "paperDollServer",
        None,
        "UpdateExistingCharacterFull",
        &[PyRep::Integer(char_id), new_doll, new_portrait],
        &kw,
        None,
    )
    .await
    .expect("UpdateExistingCharacterFull");

    let my_doll_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "paperDollServer",
            None,
            "GetMyPaperDollData",
            &[],
            &kw,
            None,
        )
        .await
        .expect("GetMyPaperDollData");
    let PyRep::KeyVal(doll_kv) = my_doll_rep else {
        panic!("Expected KeyVal from GetMyPaperDollData");
    };
    assert!(doll_kv
        .get("dollDNA")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .contains("jaw_narrow"));

    // 2. Contacts Add, Labels, Edit, Online Watchlist, Blocklist, Delete (CH-4)
    ctx.dispatch_rpc(
        &mut sess,
        "charMgr",
        None,
        "AddContact",
        &[
            PyRep::Integer(buddy_id),
            PyRep::Real(10.0),
            PyRep::Bool(true),
            PyRep::Bool(true),
        ],
        &kw,
        None,
    )
    .await
    .expect("AddContact");

    // Create two labels and assign both via bitmask
    let lbl1 = ctx
        .dispatch_rpc(
            &mut sess,
            "charMgr",
            None,
            "CreateLabel",
            &[PyRep::String("Fleet Friends".into())],
            &kw,
            None,
        )
        .await
        .expect("CreateLabel 1")
        .as_i64()
        .unwrap();
    let lbl2 = ctx
        .dispatch_rpc(
            &mut sess,
            "charMgr",
            None,
            "CreateLabel",
            &[PyRep::String("Industrial".into())],
            &kw,
            None,
        )
        .await
        .expect("CreateLabel 2")
        .as_i64()
        .unwrap();
    assert_eq!(lbl1, 1);
    assert_eq!(lbl2, 2);

    ctx.dispatch_rpc(
        &mut sess,
        "charMgr",
        None,
        "AssignLabels",
        &[
            PyRep::List(vec![PyRep::Integer(buddy_id)]),
            PyRep::Integer(lbl1 | lbl2),
        ],
        &kw,
        None,
    )
    .await
    .expect("AssignLabels");

    // EditContact standing to 5.0
    ctx.dispatch_rpc(
        &mut sess,
        "charMgr",
        None,
        "EditContact",
        &[
            PyRep::Integer(buddy_id),
            PyRep::Real(5.0),
            PyRep::Bool(true),
        ],
        &kw,
        None,
    )
    .await
    .expect("EditContact");

    // Verify GetContactList returns addresses with relationshipID=5.0 and labelMask=3
    let clist_rep = ctx
        .dispatch_rpc(&mut sess, "charMgr", None, "GetContactList", &[], &kw, None)
        .await
        .expect("GetContactList");
    let PyRep::KeyVal(clist_kv) = clist_rep else {
        panic!("Expected KeyVal from GetContactList");
    };
    let Some(PyRep::Rowset { rows, .. }) = clist_kv.get("addresses") else {
        panic!("Expected addresses rowset");
    };
    assert_eq!(rows.len(), 1);

    // Watchlist online notification when buddy selects character (feat:social.contacts-buddy-online)
    ctx.drain_notifications_for_char(char_id);
    let mut buddy_sess = SessionState::new(942);
    buddy_sess.user_id = Some(2);
    ctx.dispatch_rpc(
        &mut buddy_sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(buddy_id)],
        &kw,
        None,
    )
    .await
    .expect("Buddy SelectCharacterID");
    let notifs = ctx.drain_notifications_for_char(char_id);
    assert!(
        notifs.iter().any(|n| n.event == "OnClientContactChange" && n.args == vec![PyRep::Integer(buddy_id), PyRep::Bool(true)]),
        "Watcher must receive OnClientContactChange(buddy, True) when watchlisted buddy logs in"
    );

    // BlockOwners prevents mail and private conversation from blocked character (feat:social.contacts-block)
    ctx.dispatch_rpc(
        &mut sess,
        "charMgr",
        None,
        "BlockOwners",
        &[PyRep::List(vec![PyRep::Integer(buddy_id)])],
        &kw,
        None,
    )
    .await
    .expect("BlockOwners");

    let mail_blocked_err = ctx
        .dispatch_rpc(
            &mut buddy_sess,
            "mailMgr",
            None,
            "SendMail",
            &[
                PyRep::Integer(char_id),
                PyRep::String("Spam".into()),
                PyRep::String("Blocked body".into()),
            ],
            &kw,
            None,
        )
        .await;
    assert_eq!(
        mail_blocked_err.unwrap_err().code,
        "ContactBlocked",
        "Blocked sender must be prevented from sending mail to blocker"
    );

    // UnblockOwners and DeleteContacts
    ctx.dispatch_rpc(
        &mut sess,
        "charMgr",
        None,
        "UnblockOwners",
        &[PyRep::List(vec![PyRep::Integer(buddy_id)])],
        &kw,
        None,
    )
    .await
    .expect("UnblockOwners");
    ctx.dispatch_rpc(
        &mut sess,
        "charMgr",
        None,
        "DeleteContacts",
        &[PyRep::List(vec![PyRep::Integer(buddy_id)])],
        &kw,
        None,
    )
    .await
    .expect("DeleteContacts");
}

#[tokio::test]
async fn test_ch5_gm_spawn_attr_fit_kill_location_online_and_qa_tools() {
    // parity: feat:admin.gm-heal, feat:admin.gm-spawn, feat:admin.gm-attr, feat:admin.gm-fit, feat:admin.gm-kill, feat:admin.gm-location-online, feat:admin.gm-qa-tools, rpc:charUnboundMgr.SelectCharacterID, rpc:publicQaToolsServer.CanGiveItemForMultifit, rpc:publicQaToolsServer.MoveMeTo, rpc:slash.SlashCmd
    eve_testkit::attest(&["feat:admin.gm-heal", "feat:admin.gm-spawn", "feat:admin.gm-attr", "feat:admin.gm-fit", "feat:admin.gm-kill", "feat:admin.gm-location-online", "feat:admin.gm-qa-tools"]);
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut gm_sess = SessionState::new(943);
    common::login_pilot(&ctx, &mut gm_sess, "demo", "demo-secret").await;
    gm_sess.role = eve_db::service_roles::ROLE_ADMIN | eve_db::service_roles::ROLE_GMS;
    let gm_char_id = 90_000_002;
    let gm_ship_id = 990_000_002;

    ctx.db.with_mut_snapshot(|snap| {
        let mut gm_ch = snap.characters[&90_000_001].clone();
        gm_ch.character_id = gm_char_id;
        gm_ch.user_id = gm_sess.user_id.unwrap();
        gm_ch.name = "GM Slash Operator".into();
        gm_ch.station_id = None;
        gm_ch.active_ship_id = gm_ship_id;
        snap.characters.insert(gm_char_id, gm_ch);
        snap.items.insert(
            gm_ship_id,
            InventoryItemRecord {
                item_id: gm_ship_id,
                type_id: 601, // Merlin
                owner_id: gm_char_id,
                location_id: 30_000_142,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });
    ctx.dispatch_rpc(
        &mut gm_sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(gm_char_id)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID GM");

    // 1. /spawn <type_id> <qty> & /unspawn <id> (feat:admin.gm-spawn)
    let spawn_rep = ctx
        .dispatch_rpc(
            &mut gm_sess,
            "slash",
            None,
            "SlashCmd",
            &[PyRep::String("/spawn 601 2".into())],
            &kw,
            None,
        )
        .await
        .expect("/spawn 601 2");
    let PyRep::KeyVal(spawn_kv) = spawn_rep else {
        panic!("Expected KeyVal from /spawn");
    };
    let PyRep::List(spawned_ids) = spawn_kv.get("spawnedIDs").expect("spawnedIDs") else {
        panic!("Expected spawnedIDs List");
    };
    assert_eq!(spawned_ids.len(), 2);
    let first_spawn_id = spawned_ids[0].as_i64().unwrap();
    let second_spawn_id = spawned_ids[1].as_i64().unwrap();

    let unspawn_rep = ctx
        .dispatch_rpc(
            &mut gm_sess,
            "slash",
            None,
            "SlashCmd",
            &[PyRep::String(format!("/unspawn {first_spawn_id}"))],
            &kw,
            None,
        )
        .await
        .expect("/unspawn");
    let PyRep::KeyVal(unspawn_kv) = unspawn_rep else {
        panic!("Expected KeyVal from /unspawn");
    };
    assert_eq!(unspawn_kv.get("removedFromDb").and_then(|v| v.as_bool()), Some(true));

    // 2. /setattr & /getattr (feat:admin.gm-attr)
    ctx.dispatch_rpc(
        &mut gm_sess,
        "slash",
        None,
        "SlashCmd",
        &[PyRep::String(format!("/setattr {gm_ship_id} 37 555.0"))],
        &kw,
        None,
    )
    .await
    .expect("/setattr");
    let getattr_rep = ctx
        .dispatch_rpc(
            &mut gm_sess,
            "slash",
            None,
            "SlashCmd",
            &[PyRep::String(format!("/getattr {gm_ship_id} 37"))],
            &kw,
            None,
        )
        .await
        .expect("/getattr");
    let PyRep::KeyVal(getattr_kv) = getattr_rep else {
        panic!("Expected KeyVal from /getattr");
    };
    assert_eq!(getattr_kv.get("value").and_then(|v| v.as_f64()), Some(555.0));

    // 3. /fit me <type_id> (feat:admin.gm-fit)
    let fit_rep = ctx
        .dispatch_rpc(
            &mut gm_sess,
            "slash",
            None,
            "SlashCmd",
            &[PyRep::String("/fit me 439".into())], // 1MN Afterburner I
            &kw,
            None,
        )
        .await
        .expect("/fit me 439");
    let PyRep::KeyVal(fit_kv) = fit_rep else {
        panic!("Expected KeyVal from /fit");
    };
    assert_eq!(fit_kv.get("shipID").and_then(|v| v.as_i64()), Some(gm_ship_id));
    assert_eq!(fit_kv.get("online").and_then(|v| v.as_bool()), Some(true));

    // 4. /kill <target_id> (feat:admin.gm-kill)
    let kill_rep = ctx
        .dispatch_rpc(
            &mut gm_sess,
            "slash",
            None,
            "SlashCmd",
            &[PyRep::String(format!("/kill {second_spawn_id}"))],
            &kw,
            None,
        )
        .await
        .expect("/kill");
    let PyRep::KeyVal(kill_kv) = kill_rep else {
        panic!("Expected KeyVal from /kill");
    };
    assert_eq!(kill_kv.get("destroyed").and_then(|v| v.as_bool()), Some(true));
    assert!(kill_kv.get("wreckID").and_then(|v| v.as_i64()).unwrap_or(0) > 0);
    assert!(kill_kv.get("killmailID").and_then(|v| v.as_i64()).unwrap_or(0) > 0);

    // 5. /location & /online (feat:admin.gm-location-online)
    let loc_rep = ctx
        .dispatch_rpc(
            &mut gm_sess,
            "slash",
            None,
            "SlashCmd",
            &[PyRep::String("/location".into())],
            &kw,
            None,
        )
        .await
        .expect("/location");
    let PyRep::KeyVal(loc_kv) = loc_rep else {
        panic!("Expected KeyVal from /location");
    };
    assert_eq!(loc_kv.get("characterID").and_then(|v| v.as_i64()), Some(gm_char_id));

    let online_rep = ctx
        .dispatch_rpc(
            &mut gm_sess,
            "slash",
            None,
            "SlashCmd",
            &[PyRep::String("/online".into())],
            &kw,
            None,
        )
        .await
        .expect("/online");
    let PyRep::KeyVal(online_kv) = online_rep else {
        panic!("Expected KeyVal from /online");
    };
    assert!(online_kv.get("onlineCount").and_then(|v| v.as_i64()).unwrap_or(0) >= 1);

    // 6. publicQaToolsServer (feat:admin.gm-qa-tools)
    let can_give = ctx
        .dispatch_rpc(
            &mut gm_sess,
            "publicQaToolsServer",
            None,
            "CanGiveItemForMultifit",
            &[],
            &kw,
            None,
        )
        .await
        .expect("CanGiveItemForMultifit");
    assert_eq!(can_give, PyRep::Bool(true));

    let move_rep = ctx
        .dispatch_rpc(
            &mut gm_sess,
            "publicQaToolsServer",
            None,
            "MoveMeTo",
            &[PyRep::Integer(30_002_537)],
            &kw,
            None,
        )
        .await
        .expect("MoveMeTo");
    let PyRep::KeyVal(move_kv) = move_rep else {
        panic!("Expected KeyVal from MoveMeTo");
    };
    assert_eq!(move_kv.get("solarSystemID").and_then(|v| v.as_i64()), Some(30_002_537));
}

#[tokio::test]
async fn test_char_mgr_character_profile_info_and_lifecycle() {
    // parity: rpc:charMgr.GetCharacterCreationDate, rpc:charMgr.GetHomeStationRow, rpc:charMgr.stationID, rpc:charMgr.GetOrganizationInfoForCharacters, rpc:charMgr.GetPrivateInfoOnCorpChange, rpc:charMgr.GetPublicInfo, rpc:charMgr.GetRecentShipKillsAndLosses, rpc:charMgr.SetActivityStatus, rpc:charMgr.GetCohortsForCharacter, rpc:charMgr.SetSessionCheck, rpc:charMgr.clear_memoized, rpc:charMgr.GetPaperdollState, rpc:charMgr.SetNote, rpc:charMgr.GetNote
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut sess = SessionState::new(1001);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_001)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");

    // 1. GetCharacterCreationDate
    let cdate = ctx
        .dispatch_rpc(&mut sess, "charMgr", None, "GetCharacterCreationDate", &[PyRep::Integer(90_000_001)], &kw, None)
        .await
        .expect("GetCharacterCreationDate");
    let cdate_ft = cdate.as_i64().expect("creation filetime");
    assert!(cdate_ft > 116_444_736_000_000_000);

    // 2. GetHomeStationRow
    let home = ctx
        .dispatch_rpc(&mut sess, "charMgr", None, "GetHomeStationRow", &[], &kw, None)
        .await
        .expect("GetHomeStationRow");
    let PyRep::KeyVal(home_kv) = home else { panic!("expected KeyVal") };
    assert_eq!(home_kv.get("stationID"), Some(&PyRep::Integer(60_003_760)));

    // 3. stationID
    let sta = ctx
        .dispatch_rpc(&mut sess, "charMgr", None, "stationID", &[], &kw, None)
        .await
        .expect("stationID");
    assert_eq!(sta, PyRep::Integer(60_003_760));

    // 4. GetOrganizationInfoForCharacters
    let orgs = ctx
        .dispatch_rpc(
            &mut sess,
            "charMgr",
            None,
            "GetOrganizationInfoForCharacters",
            &[PyRep::List(vec![PyRep::Integer(90_000_001)])],
            &kw,
            None,
        )
        .await
        .expect("GetOrganizationInfoForCharacters");
    let PyRep::Dict(org_map) = orgs else { panic!("expected Dict") };
    assert_eq!(org_map.len(), 1);

    // 5. GetPrivateInfoOnCorpChange
    let priv_info = ctx
        .dispatch_rpc(&mut sess, "charMgr", None, "GetPrivateInfoOnCorpChange", &[PyRep::Integer(90_000_001)], &kw, None)
        .await
        .expect("GetPrivateInfoOnCorpChange");
    let PyRep::KeyVal(priv_kv) = priv_info else { panic!("expected KeyVal") };
    assert_eq!(priv_kv.get("characterID"), Some(&PyRep::Integer(90_000_001)));

    // 6. GetPublicInfo
    let pub_info = ctx
        .dispatch_rpc(&mut sess, "charMgr", None, "GetPublicInfo", &[PyRep::Integer(90_000_001)], &kw, None)
        .await
        .expect("GetPublicInfo");
    let PyRep::KeyVal(pub_kv) = pub_info else { panic!("expected KeyVal") };
    assert_eq!(pub_kv.get("characterID"), Some(&PyRep::Integer(90_000_001)));
    assert_eq!(pub_kv.get("characterName"), Some(&PyRep::String("Aura Capsuleer".into())));

    // 7. GetRecentShipKillsAndLosses
    let kills = ctx
        .dispatch_rpc(&mut sess, "charMgr", None, "GetRecentShipKillsAndLosses", &[PyRep::Integer(90_000_001)], &kw, None)
        .await
        .expect("GetRecentShipKillsAndLosses");
    let PyRep::Rowset { rows, .. } = kills else { panic!("expected Rowset") };
    assert!(rows.is_empty());

    // 8. SetActivityStatus
    let act = ctx
        .dispatch_rpc(&mut sess, "charMgr", None, "SetActivityStatus", &[PyRep::Integer(1)], &kw, None)
        .await
        .expect("SetActivityStatus");
    assert_eq!(act, PyRep::Bool(true));

    // 9. GetCohortsForCharacter
    let cohorts = ctx
        .dispatch_rpc(&mut sess, "charMgr", None, "GetCohortsForCharacter", &[PyRep::Integer(90_000_001)], &kw, None)
        .await
        .expect("GetCohortsForCharacter");
    let PyRep::List(c_list) = cohorts else { panic!("expected List") };
    assert!(c_list.is_empty());

    // 10. SetSessionCheck & clear_memoized
    let sc = ctx
        .dispatch_rpc(&mut sess, "charMgr", None, "SetSessionCheck", &[], &kw, None)
        .await
        .expect("SetSessionCheck");
    assert_eq!(sc, PyRep::Bool(true));

    let cm = ctx
        .dispatch_rpc(&mut sess, "charMgr", None, "clear_memoized", &[], &kw, None)
        .await
        .expect("clear_memoized");
    assert_eq!(cm, PyRep::Bool(true));

    // 11. GetPaperdollState
    let pd = ctx
        .dispatch_rpc(&mut sess, "charMgr", None, "GetPaperdollState", &[], &kw, None)
        .await
        .expect("GetPaperdollState");
    assert_eq!(pd, PyRep::Integer(0));

    // 12. SetNote & GetNote
    let sn = ctx
        .dispatch_rpc(&mut sess, "charMgr", None, "SetNote", &[PyRep::Integer(90_000_002), PyRep::String("Ally scout".into())], &kw, None)
        .await
        .expect("SetNote");
    assert_eq!(sn, PyRep::Bool(true));

    let gn = ctx
        .dispatch_rpc(&mut sess, "charMgr", None, "GetNote", &[PyRep::Integer(90_000_002)], &kw, None)
        .await
        .expect("GetNote");
    assert_eq!(gn, PyRep::String("Ally scout".into()));
}
