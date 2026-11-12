//! Unit and mutation-killing tests for `services::character_contacts`.

use super::*;
use crate::router::ServerContext;
use eve_db::{InventoryFlag, InventoryItemRecord, WorldRepository};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

#[cfg(test)]
#[path = "character_contacts_extra_unit_tests.rs"]
mod extra_unit_tests;

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn rowset_len(rep: &PyRep) -> usize {
    match rep {
        PyRep::Rowset { rows, .. } | PyRep::TupleSet { rows, .. } => rows.len(),
        PyRep::IndexRowset { items, .. } => items.len(),
        PyRep::List(items) | PyRep::Tuple(items) => items.len(),
        _ => rep.crowset_rows().map(|r| r.len()).unwrap_or(0),
    }
}

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

fn char_session(char_id: i64, station_id: Option<i64>) -> SessionState {
    SessionState {
        user_id: Some(1),
        character_id: Some(char_id),
        corporation_id: Some(1_000_009),
        solar_system_id: Some(30_000_142),
        station_id,
        ship_id: Some(100_000_001),
        ..Default::default()
    }
}

#[test]
fn public_private_info_description_notes_and_respec_cover_branches() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let other_id = 90_000_002_i64;
    let mut sess = char_session(char_id, Some(60_003_760));
    let kw = HashMap::new();

    assert_eq!(
        parse_id_list(Some(&PyRep::List(vec![PyRep::Integer(10), PyRep::Integer(20)]))),
        vec![10, 20]
    );
    assert_eq!(parse_id_list(Some(&PyRep::Integer(42))), vec![42]);
    assert!(parse_id_list(None).is_empty());

    let pub_info = handle_contacts(&ctx, &mut sess, "charMgr", "GetPublicInfo3", &[], &kw)
        .unwrap()
        .unwrap();
    assert_eq!(pub_info.dict_get("characterID"), Some(&PyRep::Integer(char_id)));
    assert_eq!(
        handle_contacts(&ctx, &mut sess, "charMgr", "GetPublicInfo3", &[PyRep::Integer(999_999_999)], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "CharacterNotFound"
    );

    let priv_info = handle_contacts(&ctx, &mut sess, "charMgr", "GetPrivateInfo", &[], &kw)
        .unwrap()
        .unwrap();
    assert_eq!(priv_info.dict_get("characterID"), Some(&PyRep::Integer(char_id)));
    assert_eq!(
        handle_contacts(&ctx, &mut sess, "charMgr", "GetPrivateInfo", &[PyRep::Integer(other_id)], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "CharacterOwnershipMismatch"
    );

    // Description & employment history
    let _ = handle_contacts(
        &ctx,
        &mut sess,
        "charMgr",
        "SetCharacterDescription",
        &[PyRep::String("Veteran pilot".into())],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        handle_contacts(&ctx, &mut sess, "charMgr", "GetCharacterDescription", &[], &kw)
            .unwrap()
            .unwrap(),
        PyRep::String("Veteran pilot".into())
    );
    assert_eq!(
        handle_contacts(
            &ctx,
            &mut sess,
            "charMgr",
            "SetCharacterDescription",
            &[PyRep::String("x".repeat(5001))],
            &kw
        )
        .unwrap()
        .unwrap_err()
        .code,
        "DescriptionTooLong"
    );

    let emp = handle_contacts(&ctx, &mut sess, "charMgr", "GetEmployementRecordAndCharacterTransfers", &[], &kw)
        .unwrap()
        .unwrap();
    assert!(rowset_len(&emp) > 0);
    if let PyRep::Rowset { columns, rows } = &emp {
        assert_eq!(columns, &vec!["recordID", "corporationID", "startDate", "deleted"]);
        let start_ft = rows[0][2].as_i64().expect("startDate filetime");
        assert!(start_ft >= 116_444_736_000_000_000, "startDate must be 64-bit Win32 FILETIME ticks, got {start_ft}");
        assert_eq!(rows[0][3], PyRep::Integer(0));
    } else {
        panic!("expected Rowset from GetEmployementRecordAndCharacterTransfers");
    }

    // Owner notes CRUD & labels
    let nid = handle_contacts(
        &ctx,
        &mut sess,
        "charMgr",
        "AddOwnerNote",
        &[
            PyRep::Integer(char_id),
            PyRep::String("Tactical".into()),
            PyRep::String("Safe spot".into()),
        ],
        &kw,
    )
    .unwrap()
    .unwrap()
    .as_i64()
    .unwrap();

    let notes = handle_contacts(&ctx, &mut sess, "charMgr", "GetOwnerNote", &[PyRep::Integer(char_id)], &kw)
        .unwrap()
        .unwrap();
    assert_eq!(rowset_len(&notes), 1);

    let labels = handle_contacts(&ctx, &mut sess, "charMgr", "GetOwnerNoteLabels", &[], &kw)
        .unwrap()
        .unwrap();
    assert_eq!(labels, PyRep::List(vec![PyRep::String("Tactical".into())]));

    assert_eq!(
        handle_contacts(
            &ctx,
            &mut sess,
            "charMgr",
            "EditOwnerNote",
            &[PyRep::Integer(nid), PyRep::String("Updated".into()), PyRep::String("New body".into())],
            &kw,
        )
        .unwrap()
        .unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(
        handle_contacts(&ctx, &mut sess, "charMgr", "RemoveOwnerNote", &[PyRep::Integer(nid)], &kw)
            .unwrap()
            .unwrap(),
        PyRep::Bool(true)
    );

    // RespecCharacter
    assert_eq!(
        handle_contacts(&ctx, &mut sess, "charMgr", "RespecCharacter", &[], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidRemapArguments"
    );
    let remap_kv = PyRep::keyval([
        ("charisma", PyRep::Integer(17)),
        ("intelligence", PyRep::Integer(27)),
        ("memory", PyRep::Integer(21)),
        ("perception", PyRep::Integer(17)),
        ("willpower", PyRep::Integer(17)),
    ]);
    let remapped = handle_contacts(&ctx, &mut sess, "charMgr", "RespecCharacter", &[remap_kv], &kw)
        .unwrap()
        .unwrap();
    assert_eq!(remapped.dict_get("intelligence"), Some(&PyRep::Integer(27)));
}

#[test]
fn contacts_labels_blocklist_online_status_and_watchlist_work() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let other_id = 90_000_002_i64;
    let mut sess = char_session(char_id, Some(60_003_760));
    let kw = HashMap::new();

    assert_eq!(
        handle_contacts(&ctx, &mut sess, "charMgr", "AddContact", &[PyRep::Integer(char_id)], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidContactID"
    );
    let added = handle_contacts(
        &ctx,
        &mut sess,
        "charMgr",
        "AddContact",
        &[PyRep::Integer(other_id), PyRep::Real(10.0), PyRep::Bool(true)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(added.dict_get("standing"), Some(&PyRep::Real(10.0)));
    assert_eq!(added.dict_get("inWatchlist"), Some(&PyRep::Bool(true)));

    // CreateLabel, EditLabel, AssignLabels, RemoveLabels, DeleteLabel
    let lid = handle_contacts(&ctx, &mut sess, "charMgr", "CreateLabel", &[PyRep::String("Fleet Friends".into())], &kw)
        .unwrap()
        .unwrap()
        .as_i64()
        .unwrap();
    assert!(lid > 0);
    let _ = handle_contacts(&ctx, &mut sess, "charMgr", "EditLabel", &[PyRep::Integer(lid), PyRep::String("Allies".into())], &kw)
        .unwrap()
        .unwrap();
    let assigned = handle_contacts(
        &ctx,
        &mut sess,
        "charMgr",
        "AssignLabels",
        &[PyRep::List(vec![PyRep::Integer(other_id)]), PyRep::Integer(lid)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(assigned.dict_get("updatedCount"), Some(&PyRep::Integer(1)));

    let removed_lbl = handle_contacts(
        &ctx,
        &mut sess,
        "charMgr",
        "RemoveLabels",
        &[PyRep::List(vec![PyRep::Integer(other_id)]), PyRep::Integer(lid)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(removed_lbl.dict_get("updatedCount"), Some(&PyRep::Integer(1)));

    let del_lbl = handle_contacts(&ctx, &mut sess, "charMgr", "DeleteLabel", &[PyRep::Integer(lid)], &kw)
        .unwrap()
        .unwrap();
    assert_eq!(del_lbl.dict_get("deleted"), Some(&PyRep::Bool(true)));

    let edited = handle_contacts(
        &ctx,
        &mut sess,
        "charMgr",
        "EditContact",
        &[PyRep::List(vec![PyRep::Integer(other_id)]), PyRep::Real(5.0), PyRep::Bool(true)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(edited.dict_get("standing"), Some(&PyRep::Real(5.0)));

    let clist = handle_contacts(&ctx, &mut sess, "charMgr", "GetContactList", &[], &kw)
        .unwrap()
        .unwrap();
    assert!(clist.dict_get("addresses").is_some());

    // Online status & watchlist logon notification
    let other_uid = ctx.db.with_snapshot(|s| s.characters[&other_id].user_id);
    let _ = ctx
        .db
        .acquire_online_lock(other_uid, 999, Some(other_id), false, ctx.now_unix());
    assert!(is_character_online(&ctx, other_id));
    notify_watchlist_logon(&ctx, other_id);
    assert_eq!(
        handle_contacts(&ctx, &mut sess, "onlineStatus", "GetOnlineStatus", &[PyRep::Integer(other_id)], &kw)
            .unwrap()
            .unwrap(),
        PyRep::Bool(true)
    );
    let init_st = handle_contacts(&ctx, &mut sess, "onlineStatus", "GetInitialState", &[], &kw)
        .unwrap()
        .unwrap();
    assert_eq!(rowset_len(&init_st), 1);

    // Also verify DB-persisted watched contact fallback when esi_state is empty
    let saved_esi = ctx.esi_state.char_contacts.write().unwrap().remove(&char_id);
    ctx.db.with_mut_snapshot(|snap| {
        snap.char_contacts.insert(
            char_id,
            vec![eve_db::SocialContactRecord {
                contact_id: other_id,
                contact_type: "character".into(),
                standing: 5.0,
                is_watched: true,
                is_blocked: false,
                label_ids: Vec::new(),
            }],
        );
    });
    let init_db = handle_contacts(&ctx, &mut sess, "onlineStatus", "GetInitialState", &[], &kw)
        .unwrap()
        .unwrap();
    assert_eq!(rowset_len(&init_db), 1);
    if let Some(prev) = saved_esi {
        ctx.esi_state.char_contacts.write().unwrap().insert(char_id, prev);
    }

    // BlockOwners, SendMail blocked check, UnblockOwners, DeleteContacts
    let blocked = handle_contacts(&ctx, &mut sess, "charMgr", "BlockOwners", &[PyRep::List(vec![PyRep::Integer(other_id)])], &kw)
        .unwrap()
        .unwrap();
    assert_eq!(blocked.dict_get("blockedCount"), Some(&PyRep::Integer(1)));

    let mut other_sess = char_session(other_id, Some(60_003_760));
    assert_eq!(
        handle_contacts(&ctx, &mut other_sess, "mailMgr", "SendMail", &[PyRep::List(vec![PyRep::Integer(char_id)])], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "ContactBlocked"
    );

    let unblocked = handle_contacts(&ctx, &mut sess, "charMgr", "UnblockOwners", &[PyRep::List(vec![PyRep::Integer(other_id)])], &kw)
        .unwrap()
        .unwrap();
    assert_eq!(unblocked.dict_get("unblockedCount"), Some(&PyRep::Integer(1)));

    let deleted = handle_contacts(&ctx, &mut sess, "charMgr", "DeleteContacts", &[PyRep::List(vec![PyRep::Integer(other_id)])], &kw)
        .unwrap()
        .unwrap();
    assert_eq!(deleted.dict_get("deletedCount"), Some(&PyRep::Integer(1)));
}

#[test]
fn home_station_assets_and_self_destruct_clone_enforce_rules() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let st_id = 60_003_760_i64;
    let mut sess = char_session(char_id, Some(st_id));
    let kw = HashMap::new();

    let home = handle_contacts(&ctx, &mut sess, "charMgr", "GetHomeStation", &[], &kw)
        .unwrap()
        .unwrap();
    assert!(home.as_i64().unwrap() > 0);
    assert!(
        handle_contacts(&ctx, &mut sess, "home_station", "get_home_station", &[], &kw)
            .unwrap()
            .unwrap()
            .dict_get("station_id")
            .is_some()
    );
    let cands_rep = handle_contacts(&ctx, &mut sess, "home_station", "get_home_station_candidates", &[], &kw)
        .unwrap()
        .unwrap();
    let PyRep::List(ref l) = cands_rep else {
        panic!("expected List from get_home_station_candidates (#944)");
    };
    assert!(!l.is_empty());
    assert!(l[0].dict_get("type_id").is_some());
    assert!(l[0].dict_get("solar_system_id").is_some());
    assert!(l[0].dict_get("is_current_station").is_some());
    assert!(l[0].dict_get("is_school_hq").is_some());
    assert_eq!(l[0].dict_get("errors"), Some(&PyRep::List(Vec::new())));
    assert_eq!(
        handle_contacts(&ctx, &mut sess, "home_station", "get_next_remote_change_time", &[], &kw)
            .unwrap()
            .unwrap(),
        PyRep::None
    );

    let set_res = handle_contacts(&ctx, &mut sess, "charMgr", "SetHomeStation", &[PyRep::Integer(st_id)], &kw)
        .unwrap()
        .unwrap();
    assert_eq!(set_res, PyRep::Integer(st_id));

    let reset_err = handle_contacts(&ctx, &mut sess, "home_station", "reset_remote_change_time", &[], &kw)
        .unwrap()
        .unwrap_err();
    assert_eq!(reset_err.code.to_ascii_lowercase(), "accessdenied");
    sess.role = eve_machonet::service_roles::ROLE_GMH;
    assert_eq!(
        handle_contacts(&ctx, &mut sess, "home_station", "reset_remote_change_time", &[], &kw)
            .unwrap()
            .unwrap(),
        PyRep::Integer(0)
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            777_901,
            InventoryItemRecord {
                item_id: 777_901,
                type_id: 44_992,
                owner_id: char_id,
                location_id: st_id,
                flag: InventoryFlag::Hangar,
                quantity: 10,
                singleton: false,
                ..Default::default()
            },
        );
    });
    assert!(matches!(
        handle_contacts(&ctx, &mut sess, "charMgr", "GetAssetWorth", &[], &kw)
            .unwrap()
            .unwrap(),
        PyRep::Tuple(_)
    ));
    let list_res = handle_contacts(&ctx, &mut sess, "charMgr", "ListIncludingContainers", &[PyRep::Integer(st_id)], &kw)
        .unwrap()
        .unwrap();
    assert!(rowset_len(&list_res) > 0);

    sess.station_id = None;
    assert!(handle_contacts(&ctx, &mut sess, "home_station", "self_destruct_clone", &[], &kw).unwrap().is_err());
    sess.station_id = Some(st_id);
    let pod_id = 777_999_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            pod_id,
            InventoryItemRecord {
                item_id: pod_id,
                type_id: 670,
                owner_id: char_id,
                location_id: st_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });
    sess.ship_id = Some(pod_id);
    assert_eq!(
        handle_contacts(&ctx, &mut sess, "home_station", "self_destruct_clone", &[], &kw)
            .unwrap()
            .unwrap(),
        PyRep::Integer(st_id)
    );
}
