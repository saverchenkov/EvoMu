//! Batch `DoDestinyUpdate` wire encoding helpers (`P-7`).

use crate::wire::encode_destiny_event;
use crate::{BallId, Ballpark, DestinyEvent};
use eve_proto::PyRep;
use std::collections::HashSet;

/// Encodes a slice of [`DestinyEvent`]s into a `DoDestinyUpdate` `PyRep::List` of `(stamp, (action, args))` tuples.
///
/// DERIVED: V24.01 `eve/client/script/remote/michelle.pyj` (`Park.FlushState` & `Park.RealFlushState`)
/// requires `SetState` to appear at index `0` (`state[0][1][0] == 'SetState'`); any `SetState` at index `> 0`
/// is ignored by `RealFlushState`, leaving `Park.validState == False` and hanging `GetBallWaitForModel`.
pub fn encode_destiny_updates(
    stamp: u32,
    events: &[DestinyEvent],
    park: Option<&Ballpark>,
) -> PyRep {
    let has_set_state = events
        .iter()
        .any(|ev| matches!(ev, DestinyEvent::SetState { .. }));
    // Omit redundant `OnSlimItemChange` for balls already carrying their latest `SlimItem`
    // in `SetState` (all balls) or same-batch `AddBalls` / `AddBalls2` (`added_ball_ids`),
    // while preserving `OnSlimItemChange` for unrelated resident balls (e.g. vacated ship on `ship.Eject`).
    let mut added_ball_ids: HashSet<BallId> = HashSet::new();
    if !has_set_state {
        for ev in events {
            if let DestinyEvent::AddBalls { balls } | DestinyEvent::AddBalls2 { balls } = ev {
                added_ball_ids.extend(balls.iter().map(|b| b.id));
            }
        }
    }
    let encoded: Vec<PyRep> = events
        .iter()
        .filter(|ev| matches!(ev, DestinyEvent::SetState { .. }))
        .chain(events.iter().filter(|ev| {
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
        .map(|ev| encode_destiny_event(stamp, ev, park))
        .collect();
    PyRep::List(encoded)
}
