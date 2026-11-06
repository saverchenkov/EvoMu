//! Regression tests for `#481`: standalone `chat_channel_acl` dirty fingerprinting,
//! SQLite persistence without a `chat_channels` row, reload without creating a phantom
//! `ChatChannelRecord`, and `rollback_d6_social` preservation.

use super::fingerprints::TableFingerprints;
use super::schema::apply_migrations;
use super::social_store::{
    compute_d6_social_fp, load_d6_social_into_snapshot, rollback_d6_social, sync_d6_social,
};
use crate::snapshot_ext::{ChatChannelAclRecord, WorldStateSnapshot};
use rusqlite::Connection;
use std::collections::{HashMap, HashSet};

#[test]
fn standalone_global_chat_channel_acl_persists_reloads_and_survives_rollback() {
    let mut conn = Connection::open_in_memory().unwrap();
    apply_migrations(&conn).unwrap();

    let mut snap = WorldStateSnapshot::default();
    let mut fp = TableFingerprints::default();
    fp.d6_social = compute_d6_social_fp(&snap);

    // Mutate ONLY standalone `chat_channel_acl["global"]` with no entry in `snap.chat_channels`.
    snap.chat_channel_acl.insert(
        "global".into(),
        ChatChannelAclRecord {
            allow_list: HashSet::new(),
            deny_list: HashSet::from([90_000_077]),
            muted_until: HashMap::from([(90_000_088, (2_100_000_000, "RMT spam".into()))]),
        },
    );
    assert_ne!(
        compute_d6_social_fp(&snap),
        fp.d6_social,
        "standalone chat_channel_acl mutation must dirty d6_social fingerprint"
    );

    {
        let tx = conn.transaction().unwrap();
        assert!(
            sync_d6_social(&tx, &snap, &mut fp).unwrap(),
            "sync_d6_social must flush standalone chat_channel_acl"
        );
        tx.commit().unwrap();
    }

    // Reload into a fresh snapshot: ACL must be restored WITHOUT synthesizing a phantom ChatChannelRecord.
    let mut loaded = WorldStateSnapshot::default();
    load_d6_social_into_snapshot(&conn, &mut loaded).unwrap();
    assert!(
        !loaded.chat_channels.contains_key("global"),
        "standalone ACL reload must not materialize a phantom ChatChannelRecord"
    );
    let global_acl = loaded
        .chat_channel_acl
        .get("global")
        .expect("standalone global ACL must reload from SQLite");
    assert!(global_acl.deny_list.contains(&90_000_077));
    assert_eq!(
        global_acl.muted_until.get(&90_000_088),
        Some(&(2_100_000_000, "RMT spam".into()))
    );

    // Simulate an unrelated aborted social mutation followed by `rollback_d6_social`:
    // standalone `"global"` ACL must survive intact, and clearing it in-memory must roll back from SQLite.
    let synced_fp = compute_d6_social_fp(&loaded);
    fp.d6_social = synced_fp;
    loaded.read_mails.insert((90_000_001, 555));
    loaded.chat_channel_acl.clear();
    rollback_d6_social(&conn, &mut loaded, &fp);
    assert!(!loaded.read_mails.contains(&(90_000_001, 555)));
    assert_eq!(
        loaded.chat_channel_acl["global"].muted_until.get(&90_000_088),
        Some(&(2_100_000_000, "RMT spam".into()))
    );

    // Unmuting / clearing standalone ACL must also delete the durable row on next sync.
    loaded.chat_channel_acl.remove("global");
    {
        let tx = conn.transaction().unwrap();
        assert!(sync_d6_social(&tx, &loaded, &mut fp).unwrap());
        tx.commit().unwrap();
    }
    let mut after_clear = WorldStateSnapshot::default();
    load_d6_social_into_snapshot(&conn, &mut after_clear).unwrap();
    assert!(!after_clear.chat_channel_acl.contains_key("global"));
}
