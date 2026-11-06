//! Batch `DoDestinyUpdate` wire encoding helpers (`P-7`).

use crate::wire::encode_destiny_event;
use crate::{Ballpark, DestinyEvent};
use eve_proto::PyRep;

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
    let encoded: Vec<PyRep> = events
        .iter()
        .filter(|ev| matches!(ev, DestinyEvent::SetState { .. }))
        .chain(events.iter().filter(|ev| !matches!(ev, DestinyEvent::SetState { .. })))
        .map(|ev| encode_destiny_event(stamp, ev, park))
        .collect();
    PyRep::List(encoded)
}
