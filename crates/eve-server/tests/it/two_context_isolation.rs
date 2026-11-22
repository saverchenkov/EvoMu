//! Smoke tests for side-by-side [`crate::common::TwoServerContexts`] isolation (`#1583` Track 1).

use crate::common::TwoServerContexts;
use eve_sde::SdeRegistry;
use std::sync::Arc;

#[test]
fn two_server_contexts_isolate_repositories_and_sdes() {
    let contexts = TwoServerContexts::new();
    let shared_char_id = 90_000_001_i64;

    let right_sec_before = contexts
        .right()
        .db
        .with_snapshot(|s| s.characters.get(&shared_char_id).map(|c| c.security_status));
    let right_epoch_before = contexts.right().db.state_epoch();

    contexts
        .left()
        .db
        .tx("two_context_left_isolation", |tx| {
            let ch = tx
                .snap
                .characters
                .get_mut(&shared_char_id)
                .expect("demo seed character 90_000_001 must exist");
            ch.security_status = 4.25;
            Ok::<(), eve_db::TxError>(())
        })
        .expect("left context tx commit");

    assert_ne!(right_sec_before, Some(4.25));

    assert_eq!(
        contexts
            .left()
            .db
            .with_snapshot(|s| s.characters.get(&shared_char_id).map(|c| c.security_status)),
        Some(4.25)
    );
    assert_eq!(
        contexts
            .right()
            .db
            .with_snapshot(|s| s.characters.get(&shared_char_id).map(|c| c.security_status)),
        right_sec_before
    );
    assert_eq!(contexts.right().db.state_epoch(), right_epoch_before);
    assert!(contexts.left().db.state_epoch() > right_epoch_before);
    contexts.assert_teardown_invariants();

    let empty = TwoServerContexts::new_empty();
    assert_ne!(empty.left().instance_id, empty.right().instance_id);
    assert_ne!(
        empty.left().db.instance_id(),
        empty.right().db.instance_id()
    );

    let custom = TwoServerContexts::with_sdes(
        Arc::new(SdeRegistry::default()),
        Arc::new(SdeRegistry::default()),
    );
    assert!(!Arc::ptr_eq(&custom.left().sde, &custom.right().sde));
    assert!(!Arc::ptr_eq(&custom.left().db, &custom.right().db));
}
