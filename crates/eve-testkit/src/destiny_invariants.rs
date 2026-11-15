//! Destiny update extraction, wire descriptor verification, and two-session owner/observer
//! invariant helpers for integration tests (`GH #1077 §C`).

use crate::client_harness::{
    ClientCallOutput, ClientHarness, ClientWireFrame, ClientWireFrameKind, ClientWireTransport,
};
pub use crate::notify_ratchet::{assert_no_notification, assert_no_notification_tuples};
use eve_proto::dbrow::extract_dbrow_header_col_defs;
use eve_proto::{marshal, PyRep};
use std::collections::HashMap;

/// Flattens every `DoDestinyUpdate` frame in `frames` into ordered `(stamp, action, args)` tuples
/// (`GH #1077 §C`).
pub fn destiny_updates(frames: &[ClientWireFrame]) -> Vec<(i64, String, PyRep)> {
    let mut out = Vec::new();
    for frame in frames {
        if let ClientWireFrameKind::DoDestinyUpdate { updates, .. } = &frame.kind {
            out.extend(destiny_updates_from_batch(updates));
        }
    }
    out
}

/// Extracts `(stamp, action, args)` tuples from a single `DoDestinyUpdate.updates` `PyRep` list.
pub fn destiny_updates_from_batch(updates: &PyRep) -> Vec<(i64, String, PyRep)> {
    let items = match updates {
        PyRep::List(items) | PyRep::Tuple(items) => items.as_slice(),
        _ => return Vec::new(),
    };
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        let PyRep::Tuple(outer) = item else {
            continue;
        };
        if outer.len() < 2 {
            continue;
        }
        let stamp = outer[0].as_i64().unwrap_or(0);
        let Some(PyRep::Tuple(action_tuple)) = outer.get(1) else {
            continue;
        };
        let Some(action_name) = action_tuple.first().and_then(PyRep::as_str) else {
            continue;
        };
        let args = action_tuple
            .get(1)
            .cloned()
            .unwrap_or_else(|| PyRep::Tuple(Vec::new()));
        out.push((stamp, action_name.to_string(), args));
    }
    out
}

/// Asserts that the ordered destiny update action names in `frames` match `expected` exactly
/// (`GH #1077 §C`).
pub fn assert_update_names(frames: &[ClientWireFrame], expected: &[&str]) {
    let extracted = destiny_updates(frames);
    let actual: Vec<&str> = extracted.iter().map(|(_, name, _)| name.as_str()).collect();
    assert_eq!(
        actual.as_slice(),
        expected,
        "destiny update action sequence mismatch; full updates: {extracted:?}"
    );
}

fn unwrap_descriptor_node(rep: &PyRep) -> &PyRep {
    match rep {
        PyRep::DBRow { header, .. } => header.as_ref(),
        PyRep::Object { args, .. } => {
            if let Some(h) = args.dict_get("header") {
                return h;
            }
            rep
        }
        PyRep::ObjectEx {
            is_type_2: true,
            header,
            ..
        } => {
            if let Some(h) = header.dict_get("header") {
                return h;
            }
            rep
        }
        _ => rep,
    }
}

/// Verifies that `rep` (either a bare `DBRowDescriptor`, `PyRep::DBRow`, `dbutil.CRowset`, or
/// `dbutil.CIndexedRowset`) exposes exact `(column_name, db_type_code)` definitions AND survives
/// a `blue.marshal` encode/decode round-trip intact (`GH #1077 §C`).
pub fn assert_dbrow_descriptor(rep: &PyRep, expected_cols: &[(&str, i64)]) {
    let desc_rep = unwrap_descriptor_node(rep);
    let cols = extract_dbrow_header_col_defs(desc_rep).unwrap_or_else(|| {
        panic!("expected PyRep carrying a DBRowDescriptor, got: {rep:?}");
    });
    let expected: Vec<(String, i64)> = expected_cols
        .iter()
        .map(|(name, code)| ((*name).to_string(), *code))
        .collect();
    assert_eq!(
        cols, expected,
        "DBRowDescriptor column definitions mismatch before marshal round-trip"
    );

    let encoded = marshal::encode(rep);
    let decoded = marshal::decode(&encoded).unwrap_or_else(|err| {
        panic!("DBRowDescriptor failed blue.marshal round-trip decode: {err}; rep={rep:?}");
    });
    let decoded_desc = unwrap_descriptor_node(&decoded);
    let decoded_cols = extract_dbrow_header_col_defs(decoded_desc).unwrap_or_else(|| {
        panic!(
            "DBRowDescriptor lost header column defs after blue.marshal round-trip: {decoded:?}"
        );
    });
    assert_eq!(
        decoded_cols, expected,
        "DBRowDescriptor column definitions changed across blue.marshal round-trip"
    );
}

fn slim_item_id(slim: &PyRep) -> Option<i64> {
    slim.dict_get("itemID").and_then(PyRep::as_i64)
}

/// Extracts entity/ball IDs targeted by a single `(action, args)` Destiny update.
pub fn extract_destiny_entity_ids(action: &str, args: &PyRep) -> Vec<i64> {
    match action {
        "SetState" => {
            let Some(kv) = (match args {
                PyRep::Tuple(items) => items.first(),
                other => Some(other),
            }) else {
                return Vec::new();
            };
            let mut ids = Vec::new();
            if let Some(PyRep::List(slims)) = kv.dict_get("slims") {
                for entry in slims {
                    let slim = match entry {
                        PyRep::Tuple(pair) if !pair.is_empty() => &pair[0],
                        other => other,
                    };
                    if let Some(id) = slim_item_id(slim) {
                        ids.push(id);
                    }
                }
            }
            if ids.is_empty() {
                if let Some(ego) = kv.dict_get("ego").and_then(PyRep::as_i64) {
                    ids.push(ego);
                }
            }
            ids
        }
        "AddBalls2" => {
            // args = ((state_bytes, [(slim, dmg), ...]),)
            let PyRep::Tuple(outer) = args else {
                return Vec::new();
            };
            let Some(PyRep::Tuple(inner)) = outer.first() else {
                return Vec::new();
            };
            let Some(PyRep::List(entries)) = inner.get(1) else {
                return Vec::new();
            };
            entries
                .iter()
                .filter_map(|entry| {
                    let slim = match entry {
                        PyRep::Tuple(pair) => pair.first()?,
                        other => other,
                    };
                    slim_item_id(slim)
                })
                .collect()
        }
        "AddBalls" => {
            // args = ((state_bytes, [slim, ...], dmg_dict),)
            let PyRep::Tuple(outer) = args else {
                return Vec::new();
            };
            let Some(PyRep::Tuple(inner)) = outer.first() else {
                return Vec::new();
            };
            let Some(PyRep::List(slims)) = inner.get(1) else {
                return Vec::new();
            };
            slims.iter().filter_map(slim_item_id).collect()
        }
        "AddBall" => {
            // args = ((slim, dmg), is_global)
            let PyRep::Tuple(outer) = args else {
                return Vec::new();
            };
            let slim = match outer.first() {
                Some(PyRep::Tuple(pair)) => pair.first(),
                other => other,
            };
            slim.and_then(slim_item_id).into_iter().collect()
        }
        "RemoveBalls" => {
            let PyRep::Tuple(outer) = args else {
                return Vec::new();
            };
            let Some(PyRep::List(ids)) = outer.first() else {
                return Vec::new();
            };
            ids.iter().filter_map(PyRep::as_i64).collect()
        }
        "OnTarget" => {
            // args = (mode, source_id, target_id)
            if let PyRep::Tuple(items) = args {
                if let Some(target_id) = items.get(2).and_then(PyRep::as_i64) {
                    return vec![target_id];
                }
                if let Some(source_id) = items.get(1).and_then(PyRep::as_i64) {
                    return vec![source_id];
                }
            }
            Vec::new()
        }
        _ => {
            if let PyRep::Tuple(items) = args {
                if let Some(id) = items.first().and_then(PyRep::as_i64) {
                    return vec![id];
                }
            } else if let Some(id) = args.as_i64() {
                return vec![id];
            }
            Vec::new()
        }
    }
}

/// Recorded `(session_id, stamp, action, entity_id)` tuple for every observed `DoDestinyUpdate`
/// (`GH #1077 §C`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DestinyRecord {
    pub session_id: u64,
    pub stamp: i64,
    pub action: String,
    pub entity_id: Option<i64>,
}

impl DestinyRecord {
    pub fn as_tuple(&self) -> (u64, i64, &str, Option<i64>) {
        (self.session_id, self.stamp, self.action.as_str(), self.entity_id)
    }
}

/// Two-session **owner/observer** fixture recording `(session_id, stamp, action, entity_id)` for
/// every `DoDestinyUpdate` received by either session (`GH #1077 §C`).
pub struct OwnerObserverFixture<T: ClientWireTransport> {
    pub owner_session_id: u64,
    pub observer_session_id: u64,
    pub owner: ClientHarness<T>,
    pub observer: ClientHarness<T>,
    pub records: Vec<DestinyRecord>,
}

impl<T: ClientWireTransport> OwnerObserverFixture<T> {
    /// Creates a new two-session owner/observer fixture.
    pub fn new(
        owner_session_id: u64,
        owner: ClientHarness<T>,
        observer_session_id: u64,
        observer: ClientHarness<T>,
    ) -> Self {
        Self {
            owner_session_id,
            observer_session_id,
            owner,
            observer,
            records: Vec::new(),
        }
    }

    /// Appends `(session_id, stamp, action, entity_id)` records for all `DoDestinyUpdate`
    /// frames in `frames`.
    pub fn record_frames(&mut self, session_id: u64, frames: &[ClientWireFrame]) {
        for (stamp, action, args) in destiny_updates(frames) {
            let entity_ids = extract_destiny_entity_ids(&action, &args);
            if entity_ids.is_empty() {
                self.records.push(DestinyRecord {
                    session_id,
                    stamp,
                    action,
                    entity_id: None,
                });
            } else {
                for entity_id in entity_ids {
                    self.records.push(DestinyRecord {
                        session_id,
                        stamp,
                        action: action.clone(),
                        entity_id: Some(entity_id),
                    });
                }
            }
        }
    }

    /// Drains any pending asynchronous frames from both owner and observer sessions and records
    /// all `DoDestinyUpdate` items, returning `(owner_frames, observer_frames)`.
    pub fn drain_pending(&mut self) -> (Vec<ClientWireFrame>, Vec<ClientWireFrame>) {
        let owner_frames = self.owner.drain_pending_frames();
        self.record_frames(self.owner_session_id, &owner_frames);
        let observer_frames = self.observer.drain_pending_frames();
        self.record_frames(self.observer_session_id, &observer_frames);
        (owner_frames, observer_frames)
    }

    /// Executes an RPC on the owner session, records owner destiny updates, and immediately drains
    /// and records any peer updates broadcast to the observer session.
    pub async fn call_owner(
        &mut self,
        service: &str,
        bound_id: Option<u64>,
        method: &str,
        args: &[PyRep],
        kwargs: &HashMap<String, PyRep>,
    ) -> ClientCallOutput {
        let out = self
            .owner
            .call_exchange(service, bound_id, method, args, kwargs)
            .await;
        self.record_frames(self.owner_session_id, &out.frames);
        let peer_frames = self.observer.drain_pending_frames();
        self.record_frames(self.observer_session_id, &peer_frames);
        out
    }

    /// Executes an RPC on the observer session, records observer destiny updates, and immediately
    /// drains and records any peer updates broadcast to the owner session.
    pub async fn call_observer(
        &mut self,
        service: &str,
        bound_id: Option<u64>,
        method: &str,
        args: &[PyRep],
        kwargs: &HashMap<String, PyRep>,
    ) -> ClientCallOutput {
        let out = self
            .observer
            .call_exchange(service, bound_id, method, args, kwargs)
            .await;
        self.record_frames(self.observer_session_id, &out.frames);
        let peer_frames = self.owner.drain_pending_frames();
        self.record_frames(self.owner_session_id, &peer_frames);
        out
    }

    /// Returns all recorded tuples in `(session_id, stamp, action, entity_id)` form.
    pub fn tuples(&self) -> Vec<(u64, i64, String, Option<i64>)> {
        self.records
            .iter()
            .map(|r| (r.session_id, r.stamp, r.action.clone(), r.entity_id))
            .collect()
    }

    /// Returns all recorded updates observed by `session_id`.
    pub fn records_for(&self, session_id: u64) -> Vec<&DestinyRecord> {
        self.records
            .iter()
            .filter(|r| r.session_id == session_id)
            .collect()
    }

    /// Returns all recorded updates referencing `entity_id`.
    pub fn records_for_entity(&self, entity_id: i64) -> Vec<&DestinyRecord> {
        self.records
            .iter()
            .filter(|r| r.entity_id == Some(entity_id))
            .collect()
    }

    /// Asserts that `stamp` values never decrease within either session outside a `SetState`
    /// epoch resync.
    pub fn assert_monotonic_stamps(&self) {
        for sid in [self.owner_session_id, self.observer_session_id] {
            let mut last_stamp: Option<i64> = None;
            for rec in self.records_for(sid) {
                if rec.action == "SetState" {
                    last_stamp = Some(rec.stamp);
                    continue;
                }
                if let Some(prev) = last_stamp {
                    assert!(
                        rec.stamp >= prev,
                        "non-monotonic stamp on session {sid}: {} after {prev} (record={rec:?})",
                        rec.stamp
                    );
                }
                last_stamp = Some(rec.stamp);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client_harness::{ClientWireExchange, ContractValidationMode};
    use eve_proto::dbrow::{DBRowDescriptor, DbColumnType};
    use std::collections::VecDeque;
    use std::future::Future;
    use std::pin::Pin;

    #[test]
    fn destiny_updates_and_assert_update_names_extract_batch() {
        let frames = vec![ClientWireFrame {
            kind: ClientWireFrameKind::DoDestinyUpdate {
                epoch: 1,
                stamp: 100,
                has_set_state: false,
                updates: PyRep::List(vec![
                    PyRep::Tuple(vec![
                        PyRep::Integer(100),
                        PyRep::Tuple(vec![
                            PyRep::String("GotoDirection".into()),
                            PyRep::Tuple(vec![
                                PyRep::Integer(5001),
                                PyRep::Real(1.0),
                                PyRep::Real(0.0),
                                PyRep::Real(0.0),
                            ]),
                        ]),
                    ]),
                    PyRep::Tuple(vec![
                        PyRep::Integer(101),
                        PyRep::Tuple(vec![
                            PyRep::String("RemoveBall".into()),
                            PyRep::Tuple(vec![PyRep::Integer(5001)]),
                        ]),
                    ]),
                ]),
                wait_for_bubble: false,
            },
            wire_packet: PyRep::None,
        }];
        let extracted = destiny_updates(&frames);
        assert_eq!(extracted.len(), 2);
        assert_eq!(extracted[0].0, 100);
        assert_eq!(extracted[0].1, "GotoDirection");
        assert_eq!(extracted[1].0, 101);
        assert_eq!(extracted[1].1, "RemoveBall");
        assert_update_names(&frames, &["GotoDirection", "RemoveBall"]);
    }

    #[test]
    fn assert_dbrow_descriptor_verifies_crowset_and_marshal_round_trip() {
        let desc = DBRowDescriptor::new(vec![
            ("orderID", DbColumnType::I8),
            ("typeID", DbColumnType::I4),
            ("price", DbColumnType::Cy),
        ]);
        let crowset = desc.build_crowset(vec![vec![
            PyRep::Integer(7001),
            PyRep::Integer(34),
            PyRep::Integer(42_500),
        ]]);
        assert_dbrow_descriptor(&crowset, &[("orderID", 20), ("typeID", 3), ("price", 6)]);
        assert_dbrow_descriptor(
            &desc.to_pyrep(),
            &[("orderID", 20), ("typeID", 3), ("price", 6)],
        );
    }

    struct ScriptedTransport {
        next_call_reply: PyRep,
        pending: VecDeque<ClientWireFrame>,
    }

    impl ClientWireTransport for ScriptedTransport {
        fn send_wire_call<'a>(
            &'a mut self,
            _service: &'a str,
            _bound_id: Option<u64>,
            _method: &'a str,
            _args: &'a [PyRep],
            _kwargs: &'a HashMap<String, PyRep>,
        ) -> Pin<Box<dyn Future<Output = ClientWireExchange> + Send + 'a>> {
            let frame = self.pending.pop_front();
            let frames = frame.into_iter().collect();
            let reply = self.next_call_reply.clone();
            Box::pin(async move {
                ClientWireExchange {
                    rpc_result: Ok(reply),
                    frames,
                    marshal_errors: Vec::new(),
                }
            })
        }

        fn drain_pending_frames(&mut self) -> Vec<ClientWireFrame> {
            self.pending.drain(..).collect()
        }
    }

    #[test]
    fn owner_observer_fixture_records_entity_ids_and_checks_monotonic_stamps() {
        let owner_update = ClientWireFrame {
            kind: ClientWireFrameKind::DoDestinyUpdate {
                epoch: 1,
                stamp: 50,
                has_set_state: false,
                updates: PyRep::List(vec![PyRep::Tuple(vec![
                    PyRep::Integer(50),
                    PyRep::Tuple(vec![
                        PyRep::String("WarpTo".into()),
                        PyRep::Tuple(vec![PyRep::Integer(9001)]),
                    ]),
                ])]),
                wait_for_bubble: false,
            },
            wire_packet: PyRep::None,
        };
        let observer_update = ClientWireFrame {
            kind: ClientWireFrameKind::DoDestinyUpdate {
                epoch: 1,
                stamp: 50,
                has_set_state: false,
                updates: PyRep::List(vec![PyRep::Tuple(vec![
                    PyRep::Integer(50),
                    PyRep::Tuple(vec![
                        PyRep::String("AddBalls2".into()),
                        PyRep::Tuple(vec![PyRep::Tuple(vec![
                            PyRep::Bytes(Vec::new()),
                            PyRep::List(vec![PyRep::Tuple(vec![
                                PyRep::Dict(vec![(
                                    PyRep::String("itemID".into()),
                                    PyRep::Integer(9001),
                                )]),
                                PyRep::None,
                            ])]),
                        ])]),
                    ]),
                ])]),
                wait_for_bubble: false,
            },
            wire_packet: PyRep::None,
        };

        let owner = ClientHarness::new(ScriptedTransport {
            next_call_reply: PyRep::None,
            pending: VecDeque::from([owner_update]),
        })
        .with_mode(ContractValidationMode::ReportOnly);
        let observer = ClientHarness::new(ScriptedTransport {
            next_call_reply: PyRep::None,
            pending: VecDeque::from([observer_update]),
        })
        .with_mode(ContractValidationMode::ReportOnly);

        let mut fixture = OwnerObserverFixture::new(11, owner, 22, observer);
        let (o_frames, obs_frames) = fixture.drain_pending();
        assert_eq!(o_frames.len(), 1);
        assert_eq!(obs_frames.len(), 1);

        assert_eq!(
            fixture.tuples(),
            vec![
                (11, 50, "WarpTo".to_string(), Some(9001)),
                (22, 50, "AddBalls2".to_string(), Some(9001)),
            ]
        );
        fixture.assert_monotonic_stamps();
    }
}
