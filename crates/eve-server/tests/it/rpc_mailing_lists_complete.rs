//! Complete integration tests for `mailingListsMgr` RPC methods (`SO-1`).
//!
//! Validates all 19 open `mailingListsMgr` methods:
//! - Lifecycle, Membership, Info & Settings:
//!   `Create`, `Delete`, `GetInfo`, `GetJoinedLists`, `GetMembers`, `GetSettings`,
//!   `Join`, `KickMembers`, `Leave`, `SetDefaultAccess`
//! - Welcome Mail, Entity Access & Member Roles:
//!   `ClearEntityAccess`, `ClearWelcomeMail`, `GetWelcomeMail`, `SaveWelcomeMail`,
//!   `SendWelcomeMail`, `SetEntitiesAccess`, `SetMembersClear`, `SetMembersMuted`,
//!   `SetMembersOperator`

use crate::common;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::HashMap;

const P1: i64 = 90_000_001;
const P2: i64 = 90_000_002;

async fn select_pilot(
    ctx: &ServerContext,
    sid: u64,
    user: &str,
    pass: &str,
    char_id: i64,
) -> SessionState {
    let mut s = SessionState::new(sid);
    common::login_pilot(ctx, &mut s, user, pass).await;
    let kw = HashMap::new();
    ctx.dispatch_rpc(
        &mut s,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");
    s
}

#[tokio::test]
async fn test_mailing_lists_lifecycle_membership_and_settings() {
    // parity: rpc:mailingListsMgr.Create, rpc:mailingListsMgr.Delete, rpc:mailingListsMgr.GetInfo, rpc:mailingListsMgr.GetJoinedLists, rpc:mailingListsMgr.GetMembers, rpc:mailingListsMgr.GetSettings, rpc:mailingListsMgr.Join, rpc:mailingListsMgr.KickMembers, rpc:mailingListsMgr.Leave, rpc:mailingListsMgr.SetDefaultAccess
    let ctx = ServerContext::new();
    ctx.seed_demo_world_fixtures();
    let mut s1 = select_pilot(&ctx, 831, "demo", "demo-secret", P1).await;
    let mut s2 = select_pilot(&ctx, 832, "other", "other-secret", P2).await;
    let uid3 = ctx.db.provision_player_account("ml_p3_1", "ml_p3_1-secret");
    let p3 = ctx.db.create_character(uid3, "Mailing List Pilot Three", 1, 1, 11, 17, &ctx.sde).expect("create p3").character_id;
    let mut s3 = select_pilot(&ctx, 833, "ml_p3_1", "ml_p3_1-secret", p3).await;
    let kw = HashMap::new();

    // 1. Create mailing list
    let create_rep = ctx
        .dispatch_rpc(
            &mut s1,
            "mailingListsMgr",
            None,
            "Create",
            &[
                PyRep::String("Strategic Wing".into()),
                PyRep::Integer(1), // default_access: 1 (open)
                PyRep::Integer(1), // default_role: 1 (member)
                PyRep::Integer(100), // cost: 100 ISK
            ],
            &kw,
            None,
        )
        .await
        .expect("Create");
    let PyRep::KeyVal(cr_kv) = &create_rep else { panic!("Create must return KeyVal"); };
    let list_id = cr_kv.get("listID").and_then(|v| v.as_i64()).expect("listID");
    assert_eq!(cr_kv.get("name"), Some(&PyRep::String("Strategic Wing".into())));
    assert_eq!(cr_kv.get("ownerID"), Some(&PyRep::Integer(P1)));

    // 2. GetInfo
    let info_rep = ctx
        .dispatch_rpc(&mut s2, "mailingListsMgr", None, "GetInfo", &[PyRep::Integer(list_id)], &kw, None)
        .await
        .expect("GetInfo");
    let PyRep::KeyVal(info_kv) = &info_rep else { panic!("GetInfo must return KeyVal"); };
    assert_eq!(info_kv.get("listID"), Some(&PyRep::Integer(list_id)));
    assert_eq!(info_kv.get("displayName"), Some(&PyRep::String("Strategic Wing".into())));
    assert_eq!(info_kv.get("ownerID"), Some(&PyRep::Integer(P1)));

    // 3. GetSettings
    let settings_rep = ctx
        .dispatch_rpc(&mut s1, "mailingListsMgr", None, "GetSettings", &[PyRep::Integer(list_id)], &kw, None)
        .await
        .expect("GetSettings");
    let PyRep::KeyVal(set_kv) = &settings_rep else { panic!("GetSettings must return KeyVal"); };
    assert_eq!(set_kv.get("listID"), Some(&PyRep::Integer(list_id)));
    assert_eq!(set_kv.get("defaultAccess"), Some(&PyRep::Integer(1)));
    assert_eq!(set_kv.get("cost"), Some(&PyRep::Integer(100)));

    // 4. SetDefaultAccess
    let updated_set = ctx
        .dispatch_rpc(
            &mut s1,
            "mailingListsMgr",
            None,
            "SetDefaultAccess",
            &[
                PyRep::Integer(list_id),
                PyRep::Integer(2), // restricted default access
                PyRep::Integer(1), // default role
                PyRep::Integer(250), // updated cost
            ],
            &kw,
            None,
        )
        .await
        .expect("SetDefaultAccess");
    assert_eq!(updated_set.dict_get("defaultAccess"), Some(&PyRep::Integer(2)));
    assert_eq!(updated_set.dict_get("cost"), Some(&PyRep::Integer(250)));

    // Non-operator cannot modify default access
    let denied = ctx
        .dispatch_rpc(
            &mut s2,
            "mailingListsMgr",
            None,
            "SetDefaultAccess",
            &[PyRep::Integer(list_id), PyRep::Integer(1), PyRep::Integer(1), PyRep::Integer(0)],
            &kw,
            None,
        )
        .await
        .expect_err("Non-operator cannot SetDefaultAccess");
    assert_eq!(denied.code, "UserError");

    // 5. Join list
    let join_rep = ctx
        .dispatch_rpc(&mut s2, "mailingListsMgr", None, "Join", &[PyRep::Integer(list_id)], &kw, None)
        .await
        .expect("Join");
    let PyRep::KeyVal(join_kv) = &join_rep else { panic!("Join must return KeyVal"); };
    assert_eq!(join_kv.get("id"), Some(&PyRep::Integer(list_id)));
    assert_eq!(join_kv.get("joined"), Some(&PyRep::Bool(true)));

    // 6. GetJoinedLists
    let joined_rep = ctx
        .dispatch_rpc(&mut s2, "mailingListsMgr", None, "GetJoinedLists", &[], &kw, None)
        .await
        .expect("GetJoinedLists");
    let PyRep::Rowset { rows: j_rows, .. } = &joined_rep else { panic!("GetJoinedLists rowset"); };
    assert!(
        j_rows.iter().any(|r| r[0] == PyRep::Integer(list_id) && r[1] == PyRep::String("Strategic Wing".into())),
        "Joined list must appear in GetJoinedLists"
    );

    // 7. GetMembers
    let mems_rep = ctx
        .dispatch_rpc(&mut s1, "mailingListsMgr", None, "GetMembers", &[PyRep::Integer(list_id)], &kw, None)
        .await
        .expect("GetMembers");
    let PyRep::Rowset { rows: mem_rows, .. } = &mems_rep else { panic!("GetMembers rowset"); };
    assert!(mem_rows.iter().any(|r| r[0] == PyRep::Integer(P1) && r[1] == PyRep::Integer(3)));
    assert!(mem_rows.iter().any(|r| r[0] == PyRep::Integer(P2) && r[1] == PyRep::Integer(1)));

    // 8. Leave list
    let leave_rep = ctx
        .dispatch_rpc(&mut s2, "mailingListsMgr", None, "Leave", &[PyRep::Integer(list_id)], &kw, None)
        .await
        .expect("Leave");
    assert_eq!(leave_rep, PyRep::Bool(true));

    let mems_after_leave = ctx
        .dispatch_rpc(&mut s1, "mailingListsMgr", None, "GetMembers", &[PyRep::Integer(list_id)], &kw, None)
        .await
        .expect("GetMembers after leave");
    let PyRep::Rowset { rows: mems_leave_rows, .. } = &mems_after_leave else { panic!("rowset"); };
    assert!(!mems_leave_rows.iter().any(|r| r[0] == PyRep::Integer(P2)));

    // 9. KickMembers
    // P2 and P3 rejoin
    ctx.dispatch_rpc(&mut s2, "mailingListsMgr", None, "Join", &[PyRep::Integer(list_id)], &kw, None)
        .await
        .expect("P2 rejoin");
    ctx.dispatch_rpc(&mut s3, "mailingListsMgr", None, "Join", &[PyRep::Integer(list_id)], &kw, None)
        .await
        .expect("P3 join");

    // Non-operator cannot kick
    let kick_denied = ctx
        .dispatch_rpc(
            &mut s3,
            "mailingListsMgr",
            None,
            "KickMembers",
            &[PyRep::Integer(list_id), PyRep::List(vec![PyRep::Integer(P2)])],
            &kw,
            None,
        )
        .await
        .expect_err("Non-operator cannot kick");
    assert_eq!(kick_denied.code, "UserError");

    // Owner kicks P3
    ctx.drain_notifications_for_char(p3);
    let kick_rep = ctx
        .dispatch_rpc(
            &mut s1,
            "mailingListsMgr",
            None,
            "KickMembers",
            &[PyRep::Integer(list_id), PyRep::List(vec![PyRep::Integer(p3)])],
            &kw,
            None,
        )
        .await
        .expect("Owner KickMembers");
    assert_eq!(kick_rep, PyRep::Bool(true));

    let p3_leave_notif = ctx.drain_notifications_for_char(p3);
    assert!(p3_leave_notif.iter().any(|n| n.event == "OnMailingListLeave"));

    // 10. Delete mailing list
    let del_denied = ctx
        .dispatch_rpc(&mut s2, "mailingListsMgr", None, "Delete", &[PyRep::Integer(list_id)], &kw, None)
        .await
        .expect_err("Non-owner cannot delete list");
    assert_eq!(del_denied.code, "UserError");

    ctx.drain_notifications_for_char(P2);
    let del_rep = ctx
        .dispatch_rpc(&mut s1, "mailingListsMgr", None, "Delete", &[PyRep::Integer(list_id)], &kw, None)
        .await
        .expect("Owner Delete list");
    assert_eq!(del_rep, PyRep::Bool(true));

    let p2_del_notif = ctx.drain_notifications_for_char(P2);
    assert!(p2_del_notif.iter().any(|n| n.event == "OnMailingListDeleted"));

    let not_found = ctx
        .dispatch_rpc(&mut s1, "mailingListsMgr", None, "GetInfo", &[PyRep::Integer(list_id)], &kw, None)
        .await
        .expect_err("Deleted list info must fail");
    assert_eq!(not_found.code, "UserError");
}

#[tokio::test]
async fn test_mailing_lists_welcome_mail_and_access_roles() {
    // parity: rpc:mailingListsMgr.ClearEntityAccess, rpc:mailingListsMgr.ClearWelcomeMail, rpc:mailingListsMgr.GetWelcomeMail, rpc:mailingListsMgr.SaveWelcomeMail, rpc:mailingListsMgr.SendWelcomeMail, rpc:mailingListsMgr.SetEntitiesAccess, rpc:mailingListsMgr.SetMembersClear, rpc:mailingListsMgr.SetMembersMuted, rpc:mailingListsMgr.SetMembersOperator
    let ctx = ServerContext::new();
    ctx.seed_demo_world_fixtures();
    let mut s1 = select_pilot(&ctx, 841, "demo", "demo-secret", P1).await;
    let mut s2 = select_pilot(&ctx, 842, "other", "other-secret", P2).await;
    let uid3 = ctx.db.provision_player_account("ml_p3_2", "ml_p3_2-secret");
    let p3 = ctx.db.create_character(uid3, "Mailing List Pilot Three", 1, 1, 11, 17, &ctx.sde).expect("create p3").character_id;
    let mut s3 = select_pilot(&ctx, 843, "ml_p3_2", "ml_p3_2-secret", p3).await;
    let kw = HashMap::new();

    // Create base list
    let cr = ctx
        .dispatch_rpc(
            &mut s1,
            "mailingListsMgr",
            None,
            "Create",
            &[
                PyRep::String("Intel & Recon".into()),
                PyRep::Integer(1),
                PyRep::Integer(1),
                PyRep::Integer(0),
            ],
            &kw,
            None,
        )
        .await
        .expect("Create");
    let list_id = cr.dict_get("listID").and_then(|v| v.as_i64()).unwrap();

    ctx.dispatch_rpc(&mut s2, "mailingListsMgr", None, "Join", &[PyRep::Integer(list_id)], &kw, None)
        .await
        .expect("P2 join");
    ctx.dispatch_rpc(&mut s3, "mailingListsMgr", None, "Join", &[PyRep::Integer(list_id)], &kw, None)
        .await
        .expect("P3 join");

    // 1. SaveWelcomeMail & GetWelcomeMail
    let save_wm = ctx
        .dispatch_rpc(
            &mut s1,
            "mailingListsMgr",
            None,
            "SaveWelcomeMail",
            &[
                PyRep::Integer(list_id),
                PyRep::String("Welcome to Intel & Recon".into()),
                PyRep::String("Rules: Always report hostiles with dotlan links.".into()),
            ],
            &kw,
            None,
        )
        .await
        .expect("SaveWelcomeMail");
    assert_eq!(save_wm.dict_get("title"), Some(&PyRep::String("Welcome to Intel & Recon".into())));

    let get_wm = ctx
        .dispatch_rpc(&mut s2, "mailingListsMgr", None, "GetWelcomeMail", &[PyRep::Integer(list_id)], &kw, None)
        .await
        .expect("GetWelcomeMail");
    let PyRep::List(wm_items) = &get_wm else { panic!("GetWelcomeMail must return List"); };
    assert_eq!(wm_items.len(), 1);
    let wm_item = &wm_items[0];
    assert_eq!(wm_item.dict_get("title"), Some(&PyRep::String("Welcome to Intel & Recon".into())));

    // 2. SendWelcomeMail
    ctx.drain_notifications_for_char(P2);
    ctx.drain_notifications_for_char(p3);
    let send_wm = ctx
        .dispatch_rpc(
            &mut s1,
            "mailingListsMgr",
            None,
            "SendWelcomeMail",
            &[
                PyRep::Integer(list_id),
                PyRep::String("Broadcast Intel Welcome".into()),
                PyRep::String("Welcome everyone to the channel.".into()),
            ],
            &kw,
            None,
        )
        .await
        .expect("SendWelcomeMail");
    assert_eq!(send_wm, PyRep::Bool(true));
    assert!(ctx.drain_notifications_for_char(P2).iter().any(|n| n.event == "OnMailSent"));
    assert!(ctx.drain_notifications_for_char(p3).iter().any(|n| n.event == "OnMailSent"));

    // 3. ClearWelcomeMail
    let clear_wm = ctx
        .dispatch_rpc(&mut s1, "mailingListsMgr", None, "ClearWelcomeMail", &[PyRep::Integer(list_id)], &kw, None)
        .await
        .expect("ClearWelcomeMail");
    assert_eq!(clear_wm, PyRep::Bool(true));

    let wm_after_clear = ctx
        .dispatch_rpc(&mut s2, "mailingListsMgr", None, "GetWelcomeMail", &[PyRep::Integer(list_id)], &kw, None)
        .await
        .expect("GetWelcomeMail after clear");
    let PyRep::List(empty_items) = &wm_after_clear else { panic!("List expected"); };
    assert!(empty_items.is_empty(), "Welcome mail list must be empty after clear");

    // 4. SetMembersOperator, SetMembersMuted, SetMembersClear
    ctx.drain_notifications_for_char(P2);
    let set_op = ctx
        .dispatch_rpc(
            &mut s1,
            "mailingListsMgr",
            None,
            "SetMembersOperator",
            &[PyRep::Integer(list_id), PyRep::List(vec![PyRep::Integer(P2)])],
            &kw,
            None,
        )
        .await
        .expect("SetMembersOperator");
    assert_eq!(set_op, PyRep::Bool(true));
    assert!(ctx.drain_notifications_for_char(P2).iter().any(|n| n.event == "OnMailingListSetOperator"));

    ctx.drain_notifications_for_char(p3);
    let set_mute = ctx
        .dispatch_rpc(
            &mut s2, // P2 as operator can mute P3
            "mailingListsMgr",
            None,
            "SetMembersMuted",
            &[PyRep::Integer(list_id), PyRep::List(vec![PyRep::Integer(p3)])],
            &kw,
            None,
        )
        .await
        .expect("SetMembersMuted");
    assert_eq!(set_mute, PyRep::Bool(true));
    assert!(ctx.drain_notifications_for_char(p3).iter().any(|n| n.event == "OnMailingListSetMuted"));

    ctx.drain_notifications_for_char(p3);
    let set_clear = ctx
        .dispatch_rpc(
            &mut s1,
            "mailingListsMgr",
            None,
            "SetMembersClear",
            &[PyRep::Integer(list_id), PyRep::List(vec![PyRep::Integer(p3)])],
            &kw,
            None,
        )
        .await
        .expect("SetMembersClear");
    assert_eq!(set_clear, PyRep::Bool(true));
    assert!(ctx.drain_notifications_for_char(p3).iter().any(|n| n.event == "OnMailingListSetClear"));

    // 5. SetEntitiesAccess & ClearEntityAccess
    let set_ent = ctx
        .dispatch_rpc(
            &mut s1,
            "mailingListsMgr",
            None,
            "SetEntitiesAccess",
            &[
                PyRep::Integer(list_id),
                PyRep::Dict(vec![(PyRep::Integer(p3), PyRep::Integer(0))]), // block P3
            ],
            &kw,
            None,
        )
        .await
        .expect("SetEntitiesAccess");
    assert_eq!(set_ent.dict_get("entityAccessCount"), Some(&PyRep::Integer(1)));

    // Verify P3 access is blocked
    let settings = ctx
        .dispatch_rpc(&mut s1, "mailingListsMgr", None, "GetSettings", &[PyRep::Integer(list_id)], &kw, None)
        .await
        .expect("GetSettings");
    let PyRep::KeyVal(skv) = &settings else { panic!("settings KeyVal"); };
    let PyRep::Dict(acc_dict) = skv.get("access").unwrap() else { panic!("access Dict"); };
    assert!(acc_dict.iter().any(|(k, v)| k == &PyRep::Integer(p3) && v == &PyRep::Integer(0)));

    // ClearEntityAccess
    let clear_ent = ctx
        .dispatch_rpc(
            &mut s1,
            "mailingListsMgr",
            None,
            "ClearEntityAccess",
            &[PyRep::Integer(list_id), PyRep::Integer(p3)],
            &kw,
            None,
        )
        .await
        .expect("ClearEntityAccess");
    assert_eq!(clear_ent, PyRep::Bool(true));

    let settings_after = ctx
        .dispatch_rpc(&mut s1, "mailingListsMgr", None, "GetSettings", &[PyRep::Integer(list_id)], &kw, None)
        .await
        .expect("GetSettings after clear");
    let PyRep::KeyVal(skv2) = &settings_after else { panic!("settings KeyVal"); };
    let PyRep::Dict(acc_dict2) = skv2.get("access").unwrap() else { panic!("access Dict"); };
    assert!(!acc_dict2.iter().any(|(k, _)| k == &PyRep::Integer(p3)));
}
