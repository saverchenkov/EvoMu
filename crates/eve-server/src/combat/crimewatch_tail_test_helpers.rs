//! Test-only façade over CrimeWatch/storage tail state (`#1541` Track A7a/A7b).
//!
//! Compiled under `#[cfg(any(test, feature = "testkit"))]` so both unit tests (`--lib`) and
//! integration tests (`--test it`) seed, mutate, and inspect security status transactions,
//! disapproval flags, and authorized shipping storage without reaching directly into
//! `CombatRuntimeState`.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::combat_crimewatch::{
    with_combat_runtime, with_mut_combat_runtime, SecStatusTransaction,
};

/// Replaces the persisted security status transaction list for `char_id` (clears entry when `txs` is empty).
pub fn set_sec_status_txs(ctx: &ServerContext, char_id: i64, txs: Vec<SecStatusTransaction>) {
    ctx.db.with_mut_snapshot(|snap| {
        let map = &mut snap.combat_crimewatch_state.sec_status_transactions;
        if txs.is_empty() {
            map.remove(&char_id);
        } else {
            map.insert(char_id, txs);
        }
    });
}

/// Appends a [`SecStatusTransaction`] entry for `char_id` in persisted snapshot storage.
pub fn push_sec_status_tx(ctx: &ServerContext, char_id: i64, tx: SecStatusTransaction) {
    ctx.db.with_mut_snapshot(|snap| {
        snap.combat_crimewatch_state
            .sec_status_transactions
            .entry(char_id)
            .or_default()
            .push(tx);
    });
}

/// Returns a cloned snapshot of persisted security status transactions recorded for `char_id`.
pub fn sec_status_txs(ctx: &ServerContext, char_id: i64) -> Vec<SecStatusTransaction> {
    crate::services::combat_crimewatch::char_sec_status_transactions(ctx, char_id)
}

/// Sets the persisted CrimeWatch / Zarzakh disapproval expiration timestamp (Unix seconds) for `char_id`.
pub fn set_disapproval_flag(ctx: &ServerContext, char_id: i64, expires_unix: u64) {
    crate::services::combat_crimewatch::set_char_disapproval_flag(ctx, char_id, expires_unix);
}

/// Clears any persisted disapproval flag entry for `char_id`, returning its prior expiration timestamp.
pub fn clear_disapproval_flag(ctx: &ServerContext, char_id: i64) -> Option<u64> {
    ctx.db.with_mut_snapshot(|snap| {
        snap.combat_crimewatch_state
            .disapproval_flags
            .remove(&char_id)
    })
}

/// Returns `true` if `char_id` currently has a stored disapproval flag entry in DB snapshot state.
pub fn has_disapproval_flag(ctx: &ServerContext, char_id: i64) -> bool {
    ctx.db.with_snapshot(|snap| {
        snap.combat_crimewatch_state
            .disapproval_flags
            .contains_key(&char_id)
    })
}

/// Returns the stored disapproval expiration timestamp (Unix seconds) for `char_id`, if any.
pub fn disapproval_flag_until(ctx: &ServerContext, char_id: i64) -> Option<u64> {
    crate::services::combat_crimewatch::char_disapproval_flag_until(ctx, char_id)
}

/// Authorizes `char_id` to access shipping/audit-log storage container `container_id`.
pub fn authorize_shipping_storage(ctx: &ServerContext, container_id: i64, char_id: i64) {
    with_mut_combat_runtime(ctx, |rt| {
        rt.authorized_shipping_storage
            .insert((container_id, char_id));
    });
}

/// Revokes `char_id`'s authorization for shipping/audit-log storage container `container_id`.
pub fn revoke_shipping_storage(ctx: &ServerContext, container_id: i64, char_id: i64) -> bool {
    with_mut_combat_runtime(ctx, |rt| {
        rt.authorized_shipping_storage
            .remove(&(container_id, char_id))
    })
}

/// Returns `true` if `char_id` is authorized for shipping/audit-log storage container `container_id`.
pub fn is_shipping_storage_authorized(
    ctx: &ServerContext,
    container_id: i64,
    char_id: i64,
) -> bool {
    with_combat_runtime(ctx, |rt| {
        rt.authorized_shipping_storage
            .contains(&(container_id, char_id))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn crimewatch_tail_test_helpers_round_trip() {
        let ctx = ServerContext::with_sde_and_repository(
            Arc::new(eve_sde::SdeRegistry::default()),
            Arc::new(eve_db::WorldRepository::new_empty()),
        );
        let char_id = 90_000_001_i64;
        let container_id = 100_000_001_i64;

        let tx1 = SecStatusTransaction {
            timestamp_unix: 1_800_000_000,
            old_sec: 1.0,
            new_sec: 0.8,
            delta: -0.2,
            reason: "Illegal Aggression".into(),
            solar_system_id: 30_000_142,
            event_type_id: 1,
            other_owner_id: Some(90_000_002),
            other_type_id: None,
        };
        let tx2 = SecStatusTransaction {
            timestamp_unix: 1_800_000_010,
            reason: "NPC Pirate Bounty".into(),
            ..tx1.clone()
        };

        assert!(sec_status_txs(&ctx, char_id).is_empty());
        set_sec_status_txs(&ctx, char_id, vec![tx1.clone()]);
        push_sec_status_tx(&ctx, char_id, tx2.clone());
        assert_eq!(sec_status_txs(&ctx, char_id), vec![tx1, tx2]);
        set_sec_status_txs(&ctx, char_id, Vec::new());
        assert!(sec_status_txs(&ctx, char_id).is_empty());

        assert!(!has_disapproval_flag(&ctx, char_id));
        assert_eq!(disapproval_flag_until(&ctx, char_id), None);
        set_disapproval_flag(&ctx, char_id, 1_800_000_300);
        assert!(has_disapproval_flag(&ctx, char_id));
        assert_eq!(disapproval_flag_until(&ctx, char_id), Some(1_800_000_300));
        assert_eq!(clear_disapproval_flag(&ctx, char_id), Some(1_800_000_300));
        assert_eq!(clear_disapproval_flag(&ctx, char_id), None);
        assert!(!has_disapproval_flag(&ctx, char_id));

        assert!(!is_shipping_storage_authorized(&ctx, container_id, char_id));
        authorize_shipping_storage(&ctx, container_id, char_id);
        assert!(is_shipping_storage_authorized(&ctx, container_id, char_id));
        assert!(revoke_shipping_storage(&ctx, container_id, char_id));
        assert!(!revoke_shipping_storage(&ctx, container_id, char_id));
        assert!(!is_shipping_storage_authorized(&ctx, container_id, char_id));
    }
}
