//! Behavioral edge-case unit tests for `esi::runtime_state`.

#![forbid(unsafe_code)]

use super::*;
use serde_json::json;
use std::collections::HashSet;
use std::sync::Arc;

#[test]
fn alloc_id_is_monotonically_unique_across_threads() {
    let state = Arc::new(EsiRuntimeState::new());
    let mut handles = Vec::new();
    for _ in 0..4 {
        let st = Arc::clone(&state);
        handles.push(std::thread::spawn(move || {
            (0..25).map(|_| st.alloc_id()).collect::<Vec<_>>()
        }));
    }
    let mut seen = HashSet::new();
    for h in handles {
        for id in h.join().expect("thread join") {
            assert!(id >= 16);
            assert!(seen.insert(id), "duplicate alloc_id {id}");
        }
    }
    assert_eq!(seen.len(), 100);
}

#[test]
fn mail_deletion_and_metadata_edge_cases_and_overwrites() {
    let state = EsiRuntimeState::new();
    let char_a = 90_001_001;
    let char_b = 90_001_002;
    let mail_id = 777;

    // Repeated mark_mail_deleted is idempotent
    state.mark_mail_deleted(char_a, mail_id);
    state.mark_mail_deleted(char_a, mail_id);
    assert!(state.is_mail_deleted(char_a, mail_id));
    assert!(!state.is_mail_deleted(char_b, mail_id));

    // Sender vs recipient default meta differs for unset entry
    let default_sender = state.get_mail_meta(char_b, mail_id, true);
    assert!(default_sender.is_read);
    assert_eq!(default_sender.labels, vec![2]);
    let default_recip = state.get_mail_meta(char_b, mail_id, false);
    assert!(!default_recip.is_read);
    assert_eq!(default_recip.labels, vec![1]);

    // Overwriting existing mail_meta replaces read flag and labels
    state.set_mail_meta(
        char_a,
        mail_id,
        EsiMailMeta {
            is_read: false,
            labels: vec![1, 4],
        },
    );
    state.set_mail_meta(
        char_a,
        mail_id,
        EsiMailMeta {
            is_read: true,
            labels: vec![8],
        },
    );
    let after = state.get_mail_meta(char_a, mail_id, false);
    assert!(after.is_read);
    assert_eq!(after.labels, vec![8]);
    // Other character still gets default
    assert!(!state.get_mail_meta(char_b, mail_id, false).is_read);
}

#[test]
fn custom_item_names_and_all_runtime_tables_support_full_crud() {
    let state = EsiRuntimeState::default();
    assert_eq!(state.get_custom_item_name(0), None);
    state.set_custom_item_name(-42, "Wormhole Depot");
    assert_eq!(
        state.get_custom_item_name(-42).as_deref(),
        Some("Wormhole Depot")
    );
    state.set_custom_item_name(-42, String::from("Updated Depot"));
    assert_eq!(
        state.get_custom_item_name(-42).as_deref(),
        Some("Updated Depot")
    );

    let label = EsiMailLabel {
        label_id: 32,
        name: "Ops".into(),
        color: "#112233".into(),
    };
    state
        .mail_labels
        .write()
        .unwrap()
        .insert(9001, vec![label.clone()]);
    assert_eq!(state.mail_labels.read().unwrap()[&9001][0].label_id, 32);

    state
        .mail_lists
        .write()
        .unwrap()
        .insert(9001, vec![(101, "Ops List".into())]);
    assert_eq!(state.mail_lists.read().unwrap()[&9001][0].1, "Ops List");

    state
        .char_contact_labels
        .write()
        .unwrap()
        .insert(9001, vec![(1, "Friends".into())]);
    state.corp_contacts.write().unwrap().insert(
        98_000_001,
        vec![EsiContactEntry {
            contact_id: 9002,
            contact_type: "character".into(),
            standing: -5.0,
            is_watched: false,
            is_blocked: true,
            label_ids: vec![2],
        }],
    );
    state
        .corp_contact_labels
        .write()
        .unwrap()
        .insert(98_000_001, vec![(2, "Hostiles".into())]);
    state.alliance_contacts.write().unwrap().insert(
        99_000_001,
        vec![EsiContactEntry {
            contact_id: 98_000_002,
            contact_type: "corporation".into(),
            standing: 5.0,
            is_watched: false,
            is_blocked: false,
            label_ids: vec![],
        }],
    );
    state
        .alliance_contact_labels
        .write()
        .unwrap()
        .insert(99_000_001, vec![(3, "Allies".into())]);

    let ev = EsiCalendarEvent {
        event_id: 5001,
        event_date_unix: 1_700_000_000,
        title: "CTA".into(),
        text: "Form up".into(),
        duration_minutes: 60,
        importance: 1,
        owner_id: 9001,
        owner_name: "FC".into(),
        owner_type: "character".into(),
        attendees: vec![9001, 9002],
    };
    state.calendar_events.write().unwrap().insert(5001, ev.clone());
    state
        .calendar_responses
        .write()
        .unwrap()
        .insert((9002, 5001), "accepted".into());
    assert_eq!(state.calendar_events.read().unwrap()[&5001].attendees.len(), 2);
    assert_eq!(state.calendar_responses.read().unwrap()[&(9002, 5001)], "accepted");

    let notif = EsiContactNotification {
        notification_id: 88,
        sender_character_id: 9002,
        send_date_unix: 1_700_000_123,
        standing_level: 10.0,
        message: "o7".into(),
    };
    state
        .contact_notifications
        .write()
        .unwrap()
        .insert(9001, vec![notif.clone()]);
    assert_eq!(state.contact_notifications.read().unwrap()[&9001][0].message, "o7");

    let hist = EsiCorpHistoryEntry {
        record_id: 1,
        corporation_id: 1_000_009,
        start_date_unix: 1_600_000_000,
        is_deleted: false,
    };
    state
        .char_corp_history
        .write()
        .unwrap()
        .insert(9001, vec![hist.clone()]);
    state
        .char_titles
        .write()
        .unwrap()
        .insert(9001, vec![(1, "Director".into())]);
    state
        .fleet_wing_names
        .write()
        .unwrap()
        .insert((10, 20), "Logi".into());
    state
        .fleet_squads
        .write()
        .unwrap()
        .insert((10, 30), (20, "Guardians".into()));
    state
        .fleet_member_meta
        .write()
        .unwrap()
        .insert((10, 9001), ("fleet_commander".into(), 20, 30));
    state
        .war_allies
        .write()
        .unwrap()
        .insert(55, vec![json!({"ally_id": 98_000_003})]);

    assert!(!format!("{state:?}").is_empty());
    assert_eq!(state.char_corp_history.read().unwrap()[&9001][0].record_id, 1);
    assert_eq!(state.fleet_wing_names.read().unwrap()[&(10, 20)], "Logi");
    assert_eq!(state.fleet_squads.read().unwrap()[&(10, 30)].1, "Guardians");
    assert_eq!(state.fleet_member_meta.read().unwrap()[&(10, 9001)].0, "fleet_commander");
}

#[test]
fn poisoned_rwlocks_recover_gracefully_in_helper_methods() {
    let state = EsiRuntimeState::new();

    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _guard = state.deleted_mails.write().unwrap();
        panic!("poison deleted_mails");
    }));
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _guard = state.mail_meta.write().unwrap();
        panic!("poison mail_meta");
    }));
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _guard = state.custom_item_names.write().unwrap();
        panic!("poison custom_item_names");
    }));

    state.mark_mail_deleted(42, 99);
    assert!(state.is_mail_deleted(42, 99));

    state.set_mail_meta(
        42,
        99,
        EsiMailMeta {
            is_read: true,
            labels: vec![4],
        },
    );
    let meta = state.get_mail_meta(42, 99, false);
    assert!(meta.is_read);
    assert_eq!(meta.labels, vec![4]);

    state.set_custom_item_name(500, "Recovered Item");
    assert_eq!(
        state.get_custom_item_name(500).as_deref(),
        Some("Recovered Item")
    );
}
