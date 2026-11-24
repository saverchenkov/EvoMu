//! Task-local tracker for the active character in an in-flight `dispatch_rpc` call (`#850`, `#931`).

tokio::task_local! {
    static DISPATCH_ACTIVE_CHAR_ID: std::cell::Cell<Option<i64>>;
}

/// Returns the active character ID inside the current in-flight `dispatch_rpc` call, if any.
pub fn active_dispatch_char() -> Option<i64> {
    DISPATCH_ACTIVE_CHAR_ID.try_with(|c| c.get()).ok().flatten()
}

/// Returns `true` if `char_id` is the character executing the current in-flight `dispatch_rpc` call (`#850`).
pub fn is_active_dispatch_char(char_id: i64) -> bool {
    active_dispatch_char() == Some(char_id)
}

/// Updates the active character ID inside an in-flight `dispatch_rpc` scope (e.g. during `SelectCharacterID`).
pub fn set_active_dispatch_char(char_id: Option<i64>) {
    let _ = DISPATCH_ACTIVE_CHAR_ID.try_with(|c| c.set(char_id));
}

/// Runs `fut` with `char_id` registered as the active `dispatch_rpc` caller so bulk RPCs (`MultiAdd`, `FitMany`)
/// do not evict their own in-flight notifications at `MAX_PENDING_NOTIFICATIONS_PER_CHAR` (`#850` / `B12`).
pub async fn with_active_dispatch_char<F, R>(char_id: Option<i64>, fut: F) -> R
where
    F: std::future::Future<Output = R>,
{
    DISPATCH_ACTIVE_CHAR_ID
        .scope(std::cell::Cell::new(char_id), fut)
        .await
}
