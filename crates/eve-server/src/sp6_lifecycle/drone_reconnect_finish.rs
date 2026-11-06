//! Drone reconnect ownership persistence and Destiny state emission helpers (`SP-6`).

use crate::router::ServerContext;
use crate::system_actor::SystemHandle;

pub(super) async fn finish_reconnect_drones(
    ctx: &ServerContext,
    sys_handle: &SystemHandle,
    char_id: i64,
    ship_id: i64,
    reconnected: &[(i64, i64, u32)],
) {
    if !reconnected.is_empty() {
        let _ = ctx.db.try_with_mut_snapshot(|snap| -> Result<(), String> {
            for &(_, did, _) in reconnected {
                if let Some(it) = snap.items.get_mut(&did) {
                    it.owner_id = char_id;
                    it.version += 1;
                }
            }
            Ok(())
        });
    }

    for &(bid, _, tid) in reconnected {
        let _ = sys_handle
            .emit_destiny_event(
                bid,
                eve_destiny::DestinyEvent::OnDroneStateChange {
                    drone_id: bid,
                    owner_id: char_id,
                    controller_id: ship_id,
                    activity_state: 0,
                    type_id: tid,
                    controller_owner_id: char_id,
                    target_id: None,
                },
            )
            .await;
    }
}
