//! Batch `DoDestinyUpdate` wire encoding helpers (`P-7`).

use crate::wire::encode_destiny_event;
use crate::{BallId, Ballpark, DestinyEvent};
use eve_proto::PyRep;
use std::collections::HashSet;

/// Returns the canonical wire-ordered indices into `events` for a `DoDestinyUpdate` batch (`P-7`).
///
/// DERIVED: V24.01 `eve/client/script/remote/michelle.pyj` (`Park.FlushState` & `Park.RealFlushState`)
/// requires `SetState` to appear at index `0` (`state[0][1][0] == 'SetState'`); any `SetState` at index `> 0`
/// is ignored by `RealFlushState`, leaving `Park.validState == False` and hanging `GetBallWaitForModel`.
///
/// Also omits:
/// - `TargetLockFailed` (internal lock failure notification, not emitted on V24 Destiny wire);
/// - redundant `OnSlimItemChange { ball_id }` for balls already carrying their latest `SlimItem` in
///   same-batch `SetState` (all balls) or `AddBalls` / `AddBalls2` (`added_ball_ids`), while
///   preserving `OnSlimItemChange` for unrelated resident balls (e.g. vacated ship on `ship.Eject`);
/// - `SetBallInteractive` and `PackagedAction` when `SetState` is present in the same batch.
pub fn ordered_batch_indices(events: &[DestinyEvent]) -> Vec<usize> {
    let has_set_state = events
        .iter()
        .any(|ev| matches!(ev, DestinyEvent::SetState { .. }));
    let mut added_ball_ids: HashSet<BallId> = HashSet::new();
    if !has_set_state {
        for ev in events {
            if let DestinyEvent::AddBalls { balls } | DestinyEvent::AddBalls2 { balls } = ev {
                added_ball_ids.extend(balls.iter().map(|b| b.id));
            }
        }
    }
    events
        .iter()
        .enumerate()
        .filter(|(_, ev)| matches!(ev, DestinyEvent::SetState { .. }))
        .chain(events.iter().enumerate().filter(|(_, ev)| {
            let omit_slim = matches!(
                ev,
                DestinyEvent::OnSlimItemChange { ball_id }
                    if has_set_state || added_ball_ids.contains(ball_id)
            );
            !matches!(
                ev,
                DestinyEvent::SetState { .. } | DestinyEvent::TargetLockFailed { .. }
            ) && !omit_slim
                && !(has_set_state
                    && matches!(
                        ev,
                        DestinyEvent::SetBallInteractive { .. }
                            | DestinyEvent::PackagedAction { .. }
                    ))
        }))
        .map(|(idx, _)| idx)
        .collect()
}

/// Encodes a slice of [`DestinyEvent`]s into a `DoDestinyUpdate` `PyRep::List` of `(stamp, (action, args))` tuples.
pub fn encode_destiny_updates(
    stamp: u32,
    events: &[DestinyEvent],
    park: Option<&Ballpark>,
) -> PyRep {
    let encoded: Vec<PyRep> = ordered_batch_indices(events)
        .into_iter()
        .map(|idx| encode_destiny_event(stamp, &events[idx], park))
        .collect();
    PyRep::List(encoded)
}
