//! Unit and mutation-killing tests for `esi::runtime_state`.

use super::*;
use serde_json::json;

#[test]
fn new_and_default_initialize_empty_tables_and_id_counter_at_16() {
    let state = EsiRuntimeState::default();
    assert_eq!(state.alloc_id(), 16);
    assert_eq!(state.alloc_id(), 17);
    assert_eq!(state.alloc_id(), 18);

    let state2 = EsiRuntimeState::new();
    assert_eq!(state2.alloc_id(), 16);
    assert!(state2.deleted_mails.read().unwrap().is_empty());
    assert!(state2.mail_meta.read().unwrap().is_empty());
    assert!(state2.mail_labels.read().unwrap().is_empty());
    assert!(state2.char_contacts.read().unwrap().is_empty());
    assert!(state2.calendar_events.read().unwrap().is_empty());
    assert!(state2.custom_item_names.read().unwrap().is_empty());
}

#[test]
fn default_mail_labels_returns_four_canonical_system_labels() {
    let labels = EsiRuntimeState::default_mail_labels();
    assert_eq!(labels.len(), 4);
    assert_eq!(labels[0].label_id, 1);
    assert_eq!(labels[0].name, "Inbox");
    assert_eq!(labels[0].color, "#ffffff");
    assert_eq!(labels[1].label_id, 2);
    assert_eq!(labels[1].name, "Sent");
    assert_eq!(labels[1].color, "#ffffff");
    assert_eq!(labels[2].label_id, 4);
    assert_eq!(labels[2].name, "Corp");
    assert_eq!(labels[2].color, "#0099ff");
    assert_eq!(labels[3].label_id, 8);
    assert_eq!(labels[3].name, "Alliance");
    assert_eq!(labels[3].color, "#00ff33");
}

#[test]
fn mail_deletion_and_metadata_distinguish_sender_recipient_and_custom_override() {
    let state = EsiRuntimeState::new();
    let char_id = 90_000_001;
    let other_char = 90_000_002;
    let mail_id = 500;

    assert!(!state.is_mail_deleted(char_id, mail_id));
    state.mark_mail_deleted(char_id, mail_id);
    assert!(state.is_mail_deleted(char_id, mail_id));
    assert!(!state.is_mail_deleted(other_char, mail_id));
    assert!(!state.is_mail_deleted(char_id, mail_id + 1));

    // Default recipient metadata: unread, label [1] (Inbox)
    let recip_meta = state.get_mail_meta(char_id, mail_id, false);
    assert!(!recip_meta.is_read);
    assert_eq!(recip_meta.labels, vec![1]);

    // Default sender metadata: read, label [2] (Sent)
    let sender_meta = state.get_mail_meta(char_id, mail_id, true);
    assert!(sender_meta.is_read);
    assert_eq!(sender_meta.labels, vec![2]);

    // Stored override takes precedence over both sender and recipient defaults
    state.set_mail_meta(
        char_id,
        mail_id,
        EsiMailMeta {
            is_read: true,
            labels: vec![1, 4, 8],
        },
    );
    let overridden = state.get_mail_meta(char_id, mail_id, false);
    assert!(overridden.is_read);
    assert_eq!(overridden.labels, vec![1, 4, 8]);
}

#[test]
fn custom_item_names_and_auxiliary_runtime_maps_roundtrip() {
    let state = EsiRuntimeState::new();
    assert_eq!(state.get_custom_item_name(100_001), None);

    state.set_custom_item_name(100_001, "Mobile Tractor alpha");
    assert_eq!(
        state.get_custom_item_name(100_001).as_deref(),
        Some("Mobile Tractor alpha")
    );
    state.set_custom_item_name(100_001, "Renamed MTU");
    assert_eq!(
        state.get_custom_item_name(100_001).as_deref(),
        Some("Renamed MTU")
    );
    assert_eq!(state.get_custom_item_name(100_002), None);

    // Auxiliary maps (contacts, history, fleet_motd, war_allies) store and retrieve records cleanly
    state.char_contacts.write().unwrap().insert(
        90_000_001,
        vec![EsiContactEntry {
            contact_id: 90_000_002,
            contact_type: "character".to_string(),
            standing: 10.0,
            is_watched: true,
            is_blocked: false,
            label_ids: vec![1],
        }],
    );
    assert_eq!(
        state.char_contacts.read().unwrap()[&90_000_001][0].standing,
        10.0
    );

    state
        .fleet_motd
        .write()
        .unwrap()
        .insert(1, "Fly safe".to_string());
    assert_eq!(state.fleet_motd.read().unwrap()[&1], "Fly safe");

    state
        .war_allies
        .write()
        .unwrap()
        .insert(7, vec![json!({"ally_id": 99000001})]);
    assert_eq!(state.war_allies.read().unwrap()[&7].len(), 1);
}
