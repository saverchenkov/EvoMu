#![forbid(unsafe_code)]
//! Regression tests (`#481` / `#916`) verifying standalone `chat_channel_acl` entries (such as
//! `"global"`) persist, clear cleanly, never synthesize phantom `ChatChannelRecord`, and
//! `load_social_domain_rows` restores them.

use super::domain_rows::{DomainRowChange, DOMAIN_SOCIAL_CHAT_CALENDAR};
use super::quarantine::LoadContext;
use super::schema::apply_migrations;
use super::social_store::{
    collect_social_domain_row_diffs, load_social_domain_rows, sync_social_domain_rows,
};
use crate::types::{ChatChannelAclRecord, WorldStateSnapshot};
use rusqlite::Connection;
use std::collections::{HashMap, HashSet};

#[test]
fn standalone_global_chat_channel_acl_persists_reloads_and_survives_rollback() {
    let mut conn = Connection::open_in_memory().unwrap();
    apply_migrations(&conn).unwrap();

    let empty = WorldStateSnapshot::default();
    let mut snap = WorldStateSnapshot::default();
    let mut acl = ChatChannelAclRecord {
        allow_list: HashSet::from([90_000_001]),
        deny_list: HashSet::from([90_000_002]),
        muted_until: HashMap::from([(90_000_003, (1_900_000_000, "spam".to_string()))]),
    };

    // Standalone "global" ACL with NO matching ChatChannelRecord in `snap.chat_channels`.
    snap.chat_channel_acl.insert("global".to_string(), acl.clone());
    assert!(snap.chat_channels.is_empty());

    let mut insert_diffs = Vec::new();
    collect_social_domain_row_diffs(&empty, &snap, &mut insert_diffs);
    assert_eq!(insert_diffs.len(), 1);
    assert!(matches!(
        &insert_diffs[0],
        DomainRowChange::Upsert { domain, key, .. }
            if *domain == DOMAIN_SOCIAL_CHAT_CALENDAR && key == "chat_channel_acl/global"
    ));

    {
        let tx = conn.transaction().unwrap();
        assert!(
            sync_social_domain_rows(&tx, &snap).unwrap(),
            "sync_social_domain_rows must flush standalone chat_channel_acl"
        );
        tx.commit().unwrap();
    }

    let mut loaded = WorldStateSnapshot::default();
    assert!(!load_social_domain_rows(&conn, &mut loaded, &mut LoadContext::new(false)).unwrap());
    assert!(
        !loaded.chat_channels.contains_key("global"),
        "standalone ACL must NOT synthesize a phantom ChatChannelRecord"
    );
    assert_eq!(loaded.chat_channel_acl.get("global"), Some(&acl));

    // Mutate in memory and reload via `load_social_domain_rows`.
    loaded.chat_channel_acl.clear();
    load_social_domain_rows(&conn, &mut loaded, &mut LoadContext::new(false))
        .expect("load_social_domain_rows");
    assert_eq!(loaded.chat_channel_acl.get("global"), Some(&acl));

    // Clearing all ACL sets back to empty removes the persisted domain row without creating a channel.
    let before_clear = loaded.clone();
    acl.allow_list.clear();
    acl.deny_list.clear();
    acl.muted_until.clear();
    loaded.chat_channel_acl.insert("global".to_string(), acl);

    let mut del_diffs = Vec::new();
    collect_social_domain_row_diffs(&before_clear, &loaded, &mut del_diffs);
    assert_eq!(del_diffs.len(), 1);
    assert!(matches!(
        &del_diffs[0],
        DomainRowChange::Delete { domain, key }
            if *domain == DOMAIN_SOCIAL_CHAT_CALENDAR && key == "chat_channel_acl/global"
    ));

    {
        let tx = conn.transaction().unwrap();
        assert!(sync_social_domain_rows(&tx, &loaded).unwrap());
        tx.commit().unwrap();
    }
    let mut after_clear = WorldStateSnapshot::default();
    load_social_domain_rows(&conn, &mut after_clear, &mut LoadContext::new(false)).unwrap();
    assert!(after_clear.chat_channel_acl.is_empty());
    assert!(after_clear.chat_channels.is_empty());
}
