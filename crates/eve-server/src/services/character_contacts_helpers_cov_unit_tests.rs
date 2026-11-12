//! Unit tests covering `character_contacts_helpers` (`parse_id_list`,
//! `is_character_online`, `notify_watchlist_logon`, `notify_watchlist_presence`,
//! `handle_home_station_candidates`, `handle_self_destruct_clone`,
//! `check_mail_recipient_blocks`).

use super::*;
use crate::esi::runtime_state::EsiContactEntry;
use crate::router::ServerContext;
use eve_db::{InventoryFlag, InventoryItemRecord, WorldRepository};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

#[test]
fn contacts_watchlist_online_status_and_mail_blocks_cover_all_paths() {
    let ctx = ServerContext::with_sde_and_repository(
        shared_sde(),
        Arc::new(WorldRepository::with_demo_seed()),
    );

    assert_eq!(parse_id_list(None), Vec::<i64>::new());
    assert_eq!(parse_id_list(Some(&PyRep::Integer(42))), vec![42]);
    assert_eq!(
        parse_id_list(Some(&PyRep::List(vec![PyRep::Integer(1), PyRep::Integer(2)]))),
        vec![1, 2]
    );

    let char1 = 90_000_001i64;
    let char2 = 90_000_002i64;
    assert!(!is_character_online(&ctx, char1));

    ctx.db
        .acquire_online_lock_with_computer_hash(1, 98_701, Some(char1), None, true, 100)
        .expect("online lock");
    ctx.db
        .acquire_online_lock_with_computer_hash(2, 98_702, Some(char2), None, true, 100)
        .expect("watcher online lock");
    assert!(is_character_online(&ctx, char1));

    // Add watchlist + block entries in ESI contact state
    {
        let mut contacts = ctx.esi_state.char_contacts.write().unwrap();
        contacts
            .entry(char2)
            .or_default()
            .push(EsiContactEntry {
                contact_id: char1,
                contact_type: "character".into(),
                standing: 10.0,
                is_watched: true,
                is_blocked: false,
                label_ids: Vec::new(),
            });
    }
    notify_watchlist_logon(&ctx, char1);
    let notifs = ctx.drain_notifications_for_char(char2);
    assert!(notifs.iter().any(|n| n.event == "OnClientContactChange"));

    // Blocking sender rejects mail
    {
        let mut contacts = ctx.esi_state.char_contacts.write().unwrap();
        if let Some(entry) = contacts
            .entry(char2)
            .or_default()
            .iter_mut()
            .find(|e| e.contact_id == char1)
        {
            entry.is_blocked = true;
        }
    }
    let mut sess = SessionState::new(98_701);
    sess.user_id = Some(1);
    sess.character_id = Some(char1);
    assert_eq!(
        check_mail_recipient_blocks(&ctx, &sess, &[PyRep::List(vec![PyRep::Integer(char2)])])
            .expect_err("blocked sender")
            .code,
        "ContactBlocked"
    );

    // home_station_candidates
    sess.station_id = Some(60_003_760);
    let cands = handle_home_station_candidates(&ctx, &sess).expect("home station candidates");
    assert!(matches!(cands, PyRep::List(l) if !l.is_empty()));

    // handle_self_destruct_clone: in space rejected, non-capsule rejected, capsule in station succeeds
    sess.station_id = None;
    assert!(handle_self_destruct_clone(&ctx, &mut sess).is_err());

    sess.station_id = Some(60_003_760);
    sess.ship_id = Some(0);
    assert!(handle_self_destruct_clone(&ctx, &mut sess).is_err());

    let pod_id = 98_700_333i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            pod_id,
            InventoryItemRecord {
                item_id: pod_id,
                type_id: 670,
                owner_id: char1,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    sess.ship_id = Some(pod_id);
    let home = handle_self_destruct_clone(&ctx, &mut sess).expect("capsule self destruct ok");
    assert!(matches!(home, PyRep::Integer(_)));
}

fn seeded_contacts_ctx() -> ServerContext {
    ServerContext::with_sde_and_repository(
        shared_sde(),
        Arc::new(WorldRepository::with_demo_seed()),
    )
    .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
}

#[test]
fn parse_id_list_and_is_character_online_all_variants() {
    let ctx = seeded_contacts_ctx();

    assert_eq!(parse_id_list(Some(&PyRep::String("abc".into()))), Vec::<i64>::new());
    assert_eq!(parse_id_list(Some(&PyRep::Real(1.5))), Vec::<i64>::new());
    assert_eq!(parse_id_list(None), Vec::<i64>::new());
    assert_eq!(parse_id_list(Some(&PyRep::Bool(true))), vec![1]);
    assert_eq!(
        parse_id_list(Some(&PyRep::List(vec![
            PyRep::Integer(10),
            PyRep::String("skip".into()),
            PyRep::Integer(20),
        ]))),
        vec![10, 20]
    );
    assert_eq!(
        parse_id_list(Some(&PyRep::Tuple(vec![
            PyRep::Integer(31),
            PyRep::None,
            PyRep::Integer(-5),
        ]))),
        vec![31, -5]
    );

    // Nonexistent character has no user_id -> false
    assert!(!is_character_online(&ctx, 999_888_777));

    let char1 = 90_000_001i64;
    let char2 = 90_000_002i64;
    assert!(!is_character_online(&ctx, char1));

    // User online with character_id = None -> false
    ctx.db
        .acquire_online_lock_with_computer_hash(1, 51_001, None, None, true, 100)
        .expect("lock with none char");
    assert!(!is_character_online(&ctx, char1));
    ctx.db.release_online_lock(1, 51_001);

    // User 1 online on different character_id -> false for char1 and false for char2 (char2 belongs to another user)
    ctx.db
        .acquire_online_lock_with_computer_hash(1, 51_002, Some(char2), None, true, 100)
        .expect("lock uid1 with char2");
    assert!(!is_character_online(&ctx, char1));
    assert!(!is_character_online(&ctx, char2));
    ctx.db.release_online_lock(1, 51_002);

    // Online on char2's own user account -> true for char2
    let char2_uid = ctx.db.with_snapshot(|snap| snap.characters[&char2].user_id);
    ctx.db
        .acquire_online_lock_with_computer_hash(char2_uid, 51_003, Some(char2), None, true, 100)
        .expect("lock char2 account");
    assert!(is_character_online(&ctx, char2));
}

#[test]
fn watchlist_presence_filters_unwatched_self_watch_and_target_block() {
    let ctx = seeded_contacts_ctx();
    let target = 90_000_001i64;
    let watcher_ok = 90_000_002i64;
    let unwatched = 90_000_003i64;
    let watcher_blocked_by_target = 90_000_004i64;

    ctx.db.with_mut_snapshot(|snap| {
        let base_ch = snap.characters[&target].clone();
        for (idx, cid) in [
            target,
            watcher_ok,
            unwatched,
            watcher_blocked_by_target,
        ]
        .into_iter()
        .enumerate()
        {
            let uid = 200 + idx as u64;
            let uname = format!("wl_u{uid}");
            snap.accounts
                .entry(uname.clone())
                .or_insert_with(|| eve_db::AccountRecord {
                    user_id: uid,
                    username: uname,
                    password_token: String::new(),
                    role: 1,
                    banned_until_unix: None,
                    ban_reason: None,
                });
            snap.characters
                .entry(cid)
                .and_modify(|c| c.user_id = uid)
                .or_insert_with(|| {
                    let mut c = base_ch.clone();
                    c.character_id = cid;
                    c.user_id = uid;
                    c.name = format!("Watcher {cid}");
                    c
                });
        }
    });
    for (idx, cid) in [
        target,
        watcher_ok,
        unwatched,
        watcher_blocked_by_target,
    ]
    .into_iter()
    .enumerate()
    {
        let uid = 200 + idx as u64;
        ctx.db
            .acquire_online_lock_with_computer_hash(uid, 52_000 + uid, Some(cid), None, true, 100)
            .expect("bring watchlist participant online");
    }

    ctx.esi_state.char_contacts.write().unwrap().insert(
        target,
        vec![
            // Self-entry with is_watched = true must NOT self-notify
            EsiContactEntry {
                contact_id: target,
                contact_type: "character".into(),
                standing: 10.0,
                is_watched: true,
                is_blocked: false,
                label_ids: Vec::new(),
            },
            // Target has blocked watcher_blocked_by_target -> suppresses notification to them
            EsiContactEntry {
                contact_id: watcher_blocked_by_target,
                contact_type: "character".into(),
                standing: -10.0,
                is_watched: false,
                is_blocked: true,
                label_ids: Vec::new(),
            },
        ],
    );
    ctx.esi_state.char_contacts.write().unwrap().insert(
        watcher_ok,
        vec![EsiContactEntry {
            contact_id: target,
            contact_type: "character".into(),
            standing: 5.0,
            is_watched: true,
            is_blocked: false,
            label_ids: Vec::new(),
        }],
    );
    ctx.esi_state.char_contacts.write().unwrap().insert(
        unwatched,
        vec![EsiContactEntry {
            contact_id: target,
            contact_type: "character".into(),
            standing: 5.0,
            is_watched: false,
            is_blocked: false,
            label_ids: Vec::new(),
        }],
    );
    ctx.esi_state.char_contacts.write().unwrap().insert(
        watcher_blocked_by_target,
        vec![EsiContactEntry {
            contact_id: target,
            contact_type: "character".into(),
            standing: 5.0,
            is_watched: true,
            is_blocked: false,
            label_ids: Vec::new(),
        }],
    );

    notify_watchlist_presence(&ctx, target, false);

    let ok_notifs = ctx.drain_notifications_for_char(watcher_ok);
    assert_eq!(ok_notifs.len(), 2);
    assert_eq!(ok_notifs[0].event, "OnClientContactChange");
    assert_eq!(
        ok_notifs[0].args,
        vec![PyRep::Integer(target), PyRep::Bool(false)]
    );
    assert_eq!(ok_notifs[1].event, "OnContactLoggedOff");
    assert_eq!(ok_notifs[1].args, vec![PyRep::Integer(target)]);

    assert!(ctx.drain_notifications_for_char(target).is_empty());
    assert!(ctx.drain_notifications_for_char(unwatched).is_empty());
    assert!(
        ctx.drain_notifications_for_char(watcher_blocked_by_target)
            .is_empty(),
        "blocked watcher must not receive presence update"
    );

    // Logon helper delegates with online = true
    notify_watchlist_logon(&ctx, target);
    let logon_notifs = ctx.drain_notifications_for_char(watcher_ok);
    assert_eq!(logon_notifs.len(), 2);
    assert_eq!(
        logon_notifs[0].args,
        vec![PyRep::Integer(target), PyRep::Bool(true)]
    );
    assert_eq!(logon_notifs[1].event, "OnContactLoggedOn");
    assert_eq!(logon_notifs[1].args, vec![PyRep::Integer(target)]);
}

#[test]
fn home_station_candidates_self_destruct_in_structure_and_mail_blocks_cover_boundaries() {
    let ctx = seeded_contacts_ctx();
    let char1 = 90_000_001i64;
    let char2 = 90_000_002i64;
    let mut sess = SessionState::new(98_750);

    // require_character error when character_id is None
    assert_eq!(
        handle_home_station_candidates(&ctx, &sess)
            .expect_err("missing char")
            .code,
        "Unauthorized"
    );
    assert_eq!(
        handle_self_destruct_clone(&ctx, &mut sess)
            .expect_err("missing char self destruct")
            .code,
        "Unauthorized"
    );
    // mail check with None character_id returns Ok(())
    assert!(check_mail_recipient_blocks(&ctx, &sess, &[PyRep::Integer(char2)]).is_ok());

    // Missing character in DB -> CharacterNotFound
    sess.user_id = Some(1);
    sess.character_id = Some(999_777_666);
    assert_eq!(
        handle_home_station_candidates(&ctx, &sess)
            .expect_err("unknown char")
            .code,
        "CharacterNotFound"
    );

    // Valid char with school > 0, corp HQ > 0, duplicate station/structure filtering
    let school_station_id = 60_001_001i64;
    let hq_station_id = 60_002_002i64;
    let docked_structure_id = 1_030_000_001_001i64;
    let char1_school_id = ctx.db.with_snapshot(|snap| snap.characters[&char1].school_id);
    let mut sde_with_school = (*shared_sde()).clone();
    if let Some(sch) = sde_with_school.schools.get_mut(&char1_school_id) {
        sch.station_id = school_station_id;
    }
    let ctx_school = ServerContext::with_sde_and_repository(
        Arc::new(sde_with_school),
        Arc::new(WorldRepository::with_demo_seed()),
    )
    .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED);

    sess.character_id = Some(char1);
    let home_station_id = ctx_school.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get(&char1).expect("char1");
        let corp_id = ch.corporation_id;
        let home = ch.home_station_id;
        snap.corporations
            .entry(corp_id)
            .and_modify(|c| c.hq_station_id = Some(hq_station_id))
            .or_insert_with(|| {
                let mut corp = eve_db::CorporationRecord::new(
                    corp_id,
                    "School Corp",
                    "SCHL",
                    char1,
                );
                corp.hq_station_id = Some(hq_station_id);
                corp
            });
        home
    });
    // Include duplicate home_station_id in session.station_id and distinct structure_id
    sess.station_id = Some(home_station_id);
    sess.structure_id = Some(docked_structure_id);
    let PyRep::List(cands) =
        handle_home_station_candidates(&ctx_school, &sess).expect("candidates list")
    else {
        panic!("expected PyRep::List");
    };
    let cand_ids: Vec<i64> = cands
        .iter()
        .filter_map(|v| v.dict_get("id").and_then(|id| id.as_i64()))
        .collect();
    assert_eq!(
        cand_ids,
        vec![
            home_station_id,
            school_station_id,
            hq_station_id,
            docked_structure_id
        ]
    );

    // When school_station <= 0 and hq <= 0 and station_id == None, skips <= 0
    let mut sde_zero_school = (*shared_sde()).clone();
    if let Some(sch) = sde_zero_school.schools.get_mut(&char1_school_id) {
        sch.station_id = 0;
    }
    let ctx_no_school = ServerContext::with_sde_and_repository(
        Arc::new(sde_zero_school),
        Arc::new(WorldRepository::with_demo_seed()),
    )
    .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED);
    ctx_no_school.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get(&char1).unwrap().clone();
        if let Some(corp) = snap.corporations.get_mut(&ch.corporation_id) {
            corp.hq_station_id = Some(-1);
        }
    });
    sess.station_id = None;
    sess.structure_id = Some(docked_structure_id);
    let PyRep::List(cands2) =
        handle_home_station_candidates(&ctx_no_school, &sess).expect("candidates filtered")
    else {
        panic!("expected list");
    };
    let cand2_ids: Vec<i64> = cands2
        .iter()
        .filter_map(|v| v.dict_get("id").and_then(|id| id.as_i64()))
        .collect();
    assert_eq!(cand2_ids, vec![home_station_id, docked_structure_id]);

    // Self destruct in Upwell structure (station_id = None, structure_id = Some)
    // Frigate ship returns SelfDestructOnlyInCapsule
    let frig_id = 98_750_587i64;
    let pod33328_id = 98_750_333i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            frig_id,
            InventoryItemRecord {
                item_id: frig_id,
                type_id: 587,
                owner_id: char1,
                location_id: docked_structure_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            pod33328_id,
            InventoryItemRecord {
                item_id: pod33328_id,
                type_id: 33328,
                owner_id: char1,
                location_id: docked_structure_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    sess.ship_id = Some(frig_id);
    assert_eq!(
        handle_self_destruct_clone(&ctx, &mut sess)
            .expect_err("frigate cannot self destruct clone")
            .code,
        "SelfDestructOnlyInCapsule"
    );

    // Capsule type 33328 in structure succeeds, emits OnCloneDestruction, sets home ship
    let c1_uid = ctx.db.with_snapshot(|s| s.characters[&char1].user_id);
    ctx.db
        .acquire_online_lock_with_computer_hash(c1_uid, 98_750, Some(char1), None, true, 100)
        .expect("bring pilot online for clone destruction notif");
    sess.ship_id = Some(pod33328_id);
    let _ = ctx.drain_notifications_for_char(char1);
    let res = handle_self_destruct_clone(&ctx, &mut sess).expect("33328 pod in structure ok");
    assert_eq!(res, PyRep::Integer(home_station_id));
    let notifs = ctx.drain_notifications_for_char(char1);
    assert!(notifs.iter().any(|n| n.event == "OnCloneDestruction"));

    // Mail blocks: unblocked ok, Tuple with second item blocked rejects, empty args ok
    assert!(check_mail_recipient_blocks(&ctx, &sess, &[]).is_ok());
    ctx.esi_state.char_contacts.write().unwrap().insert(
        char2,
        vec![EsiContactEntry {
            contact_id: char1,
            contact_type: "character".into(),
            standing: 0.0,
            is_watched: false,
            is_blocked: false,
            label_ids: Vec::new(),
        }],
    );
    assert!(check_mail_recipient_blocks(&ctx, &sess, &[PyRep::Integer(char2)]).is_ok());

    let char3 = 90_000_003i64;
    ctx.esi_state.char_contacts.write().unwrap().insert(
        char3,
        vec![EsiContactEntry {
            contact_id: char1,
            contact_type: "character".into(),
            standing: -10.0,
            is_watched: false,
            is_blocked: true,
            label_ids: Vec::new(),
        }],
    );
    let err = check_mail_recipient_blocks(
        &ctx,
        &sess,
        &[PyRep::Tuple(vec![PyRep::Integer(char2), PyRep::Integer(char3)])],
    )
    .expect_err("char3 blocks char1");
    assert_eq!(err.code, "ContactBlocked");
}

#[test]
fn character_contacts_extra_validation_and_list_station_items_branches() {
    let ctx = seeded_contacts_ctx();
    let char1 = 90_000_001i64;
    let empty_kwargs = std::collections::HashMap::new();
    let mut sess = SessionState::new(98_751);

    // GetPublicInfo3 with no charID and no session character -> CharacterNotSelected
    assert_eq!(
        super::super::handle_contacts(&ctx, &mut sess, "charMgr", "GetPublicInfo3", &[], &empty_kwargs)
            .expect("handled")
            .expect_err("not selected")
            .code,
        "CharacterNotSelected"
    );
    assert_eq!(
        super::super::handle_contacts(
            &ctx,
            &mut sess,
            "charMgr",
            "GetPublicInfo3",
            &[PyRep::Integer(999_555_444)],
            &empty_kwargs
        )
        .expect("handled")
        .expect_err("not found")
        .code,
        "CharacterNotFound"
    );

    sess.user_id = Some(1);
    sess.character_id = Some(char1);

    // GetPrivateInfo mismatch
    assert_eq!(
        super::super::handle_contacts(
            &ctx,
            &mut sess,
            "charMgr",
            "GetPrivateInfo",
            &[PyRep::Integer(90_000_002)],
            &empty_kwargs
        )
        .expect("handled")
        .expect_err("mismatch")
        .code,
        "CharacterOwnershipMismatch"
    );

    // AddContact validation errors
    assert_eq!(
        super::super::handle_contacts(&ctx, &mut sess, "charMgr", "AddContact", &[], &empty_kwargs)
            .expect("handled")
            .expect_err("missing contactID")
            .code,
        "InvalidArgument"
    );
    assert_eq!(
        super::super::handle_contacts(
            &ctx,
            &mut sess,
            "charMgr",
            "AddContact",
            &[PyRep::Integer(char1)],
            &empty_kwargs
        )
        .expect("handled")
        .expect_err("self contact")
        .code,
        "InvalidContactID"
    );
    assert_eq!(
        super::super::handle_contacts(
            &ctx,
            &mut sess,
            "charMgr",
            "AddContact",
            &[PyRep::Integer(500)],
            &empty_kwargs
        )
        .expect("handled")
        .expect_err("unknown small contact")
        .code,
        "ContactNotFound"
    );
    assert_eq!(
        super::super::handle_contacts(
            &ctx,
            &mut sess,
            "charMgr",
            "AddContact",
            &[
                PyRep::Integer(90_000_002),
                PyRep::Real(5.0),
                PyRep::String("bad_bool".into()),
            ],
            &empty_kwargs
        )
        .expect("handled")
        .expect_err("bad inWatchlist")
        .code,
        "InvalidArgument"
    );

    // MaxContactsExceeded (1024 entries)
    {
        let mut guard = ctx.esi_state.char_contacts.write().unwrap();
        let list = guard.entry(char1).or_default();
        list.clear();
        for idx in 0..1_024i64 {
            list.push(EsiContactEntry {
                contact_id: 95_000_000 + idx,
                contact_type: "character".into(),
                standing: 0.0,
                is_watched: false,
                is_blocked: false,
                label_ids: Vec::new(),
            });
        }
    }
    assert_eq!(
        super::super::handle_contacts(
            &ctx,
            &mut sess,
            "charMgr",
            "AddContact",
            &[PyRep::Integer(90_000_002), PyRep::Real(5.0), PyRep::Integer(1)],
            &empty_kwargs
        )
        .expect("handled")
        .expect_err("max contacts")
        .code,
        "MaxContactsExceeded"
    );

    // DeleteContacts with empty list -> InvalidArgument
    assert_eq!(
        super::super::handle_contacts(
            &ctx,
            &mut sess,
            "charMgr",
            "DeleteContacts",
            &[PyRep::List(Vec::new())],
            &empty_kwargs
        )
        .expect("handled")
        .expect_err("empty delete list")
        .code,
        "InvalidArgument"
    );

    // ListStationItems returns ok
    assert!(
        super::super::handle_contacts(
            &ctx,
            &mut sess,
            "charMgr",
            "ListStationItems",
            &[PyRep::Integer(60_003_760)],
            &empty_kwargs
        )
        .expect("handled")
        .is_ok()
    );
}

