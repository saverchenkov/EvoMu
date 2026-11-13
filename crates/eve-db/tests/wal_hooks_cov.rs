//! Integration test for [`WorldRepository::tx`] fault isolation and post-commit effects (`#915`).

use eve_db::persistence::WorldRepository;
use eve_db::{TxError, TxFaultPlan};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

#[test]
fn tx_fault_plan_restores_before_and_drops_effects_without_poison() {
    let repo = WorldRepository::with_demo_seed();
    let before = repo.with_snapshot(|s| s.clone());

    let effect_count = Arc::new(AtomicUsize::new(0));
    let effect_hook = Arc::clone(&effect_count);
    repo.register_post_commit_listener(Arc::new(move |_snap, _ch, eff| {
        effect_hook.fetch_add(eff.notifications.len(), Ordering::SeqCst);
    }));

    repo.set_fault_plan_for_test(TxFaultPlan::FailAtCommit);
    let err_res: Result<(), TxError> = repo.tx("fault_at_commit", |wtx| {
        wtx.snap.next_entity_id = 200_999_999;
        wtx.effects.notify_character(90_000_001, "Dropped", "charid", vec![]);
        Ok(())
    });
    assert!(err_res.is_err());
    assert_eq!(effect_count.load(Ordering::SeqCst), 0);
    assert_eq!(repo.with_snapshot(|s| s.clone()), before);

    // Subsequent transaction succeeds normally.
    repo.tx("commit_after_fault", |wtx| -> Result<(), TxError> {
        wtx.effects.notify_character(90_000_001, "Delivered", "charid", vec![]);
        Ok(())
    })
    .expect("commit_after_fault");
    assert_eq!(effect_count.load(Ordering::SeqCst), 1);
}
