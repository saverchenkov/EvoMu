//! Helper functions and `publicGatewaySvc` / `serviceGatewayMgr` RPC dispatchers for [`super::PublicGatewayRouter`].

#![forbid(unsafe_code)]

use super::{
    PublicGatewayRouter, SERVICE_GATEWAY_MGR_NAME_STR, STATUS_BAD_REQUEST, STATUS_FORBIDDEN,
    STATUS_GATEWAY_TIMEOUT,
};
use eve_machonet::SessionState;
use eve_proto::quasar::{
    AuthoritativeContext, EvePublicEvent, EvePublicNotice, EvePublicRequest, EvePublicResponse,
    GoogleAny, GoogleTimestamp, QuasarError, QuasarMessage, WireMessage,
};
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::HashMap;

pub const SERVICE_GATEWAY_MGR_NAME: &str = SERVICE_GATEWAY_MGR_NAME_STR;

pub fn response_to_pyrep(rsp: &EvePublicResponse) -> PyRep {
    let raw = rsp.encode_proto();
    let payload_type_url = rsp.payload.as_ref().map(|p| p.type_url.clone()).unwrap_or_default();
    let payload_bytes = rsp.payload.as_ref().map(|p| p.value.clone()).unwrap_or_default();
    PyRep::keyval([
        ("status_code", PyRep::Integer(rsp.status_code as i64)),
        ("status_message", PyRep::String(rsp.status_message.clone())),
        ("correlation_uuid", PyRep::Bytes(rsp.correlation_uuid.clone())),
        ("type_url", PyRep::String(payload_type_url)),
        ("payload", PyRep::Bytes(payload_bytes)),
        ("response_proto", PyRep::Bytes(raw)),
    ])
}

pub fn notice_to_pyrep(notice: &EvePublicNotice) -> PyRep {
    let raw = notice.encode_proto();
    let payload_type_url = notice.payload.as_ref().map(|p| p.type_url.clone()).unwrap_or_default();
    let payload_bytes = notice.payload.as_ref().map(|p| p.value.clone()).unwrap_or_default();
    let target_group = notice
        .target_group
        .as_ref()
        .map(|tg| tg.target_group_name().to_string())
        .unwrap_or_default();
    PyRep::keyval([
        ("uuid", PyRep::Bytes(notice.uuid.clone())),
        ("tenant", PyRep::String(notice.tenant.clone())),
        ("target_group", PyRep::String(target_group)),
        ("type_url", PyRep::String(payload_type_url)),
        ("payload", PyRep::Bytes(payload_bytes)),
        ("notice_proto", PyRep::Bytes(raw)),
    ])
}

pub fn require_admin_role(ctx: &AuthoritativeContext) -> Result<(), (u32, String)> {
    if (ctx.service_role_mask & eve_machonet::service_roles::ROLE_GM_MASK) == 0 {
        return Err((STATUS_FORBIDDEN, "Admin/GM role required".into()));
    }
    Ok(())
}

/// Validates raw `WireMessage` sanity when decoding untrusted frames.
pub fn validate_raw_wire_frame(buf: &[u8]) -> Result<WireMessage, QuasarError> {
    WireMessage::decode(buf)
}

fn parse_hijack_args(
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Result<(Option<u32>, Option<f64>), BridgeRpcError> {
    let err_rep = args.first().or_else(|| kwargs.get("error"));
    let error = match err_rep {
        None | Some(PyRep::None) => None,
        Some(PyRep::String(s)) | Some(PyRep::Token(s)) => match s.as_str() {
            "None" | "" => None,
            "TimeoutException" => Some(STATUS_GATEWAY_TIMEOUT),
            "UnpackException" => Some(STATUS_BAD_REQUEST),
            other => {
                return Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: format!("Unsupported Quasar hijack error option '{other}'"),
                    client_error: None,
                });
            }
        },
        Some(PyRep::Integer(code)) => match *code {
            400 | 401 | 403 | 404 | 405 | 409 | 412 | 415 | 500 | 501 | 503 | 504 => Some(*code as u32),
            other => {
                return Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: format!("Unsupported Quasar hijack HTTP status code {other} (must be V24 ERROR_OPTIONS)"),
                    client_error: None,
                });
            }
        },
        Some(other) => {
            return Err(BridgeRpcError {
                code: "InvalidArgument".into(),
                message: format!("Invalid Quasar hijack error argument: {other:?}"),
                client_error: None,
            });
        }
    };

    let lat_rep = args.get(1).or_else(|| kwargs.get("latency"));
    let latency = match lat_rep {
        None | Some(PyRep::None) => None,
        Some(PyRep::Real(f)) => {
            if !f.is_finite() || *f < 0.0 || *f > 300.0 {
                return Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: format!("Invalid Quasar hijack latency {f} (must be 0.0..=300.0 sec)"),
                    client_error: None,
                });
            }
            Some(*f)
        }
        Some(PyRep::Integer(i)) => {
            if *i < 0 || *i > 300 {
                return Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: format!("Invalid Quasar hijack latency {i} (must be 0..=300 sec)"),
                    client_error: None,
                });
            }
            Some(*i as f64)
        }
        Some(other) => {
            return Err(BridgeRpcError {
                code: "InvalidArgument".into(),
                message: format!("Invalid Quasar hijack latency argument: {other:?}"),
                client_error: None,
            });
        }
    };

    Ok((error, latency))
}

impl PublicGatewayRouter {
    /// Resolves live `(corporation_id, corp_role_mask)` for `session` from `WorldRepository`
    /// rather than trusting potentially stale `session.corporation_id` / `session.corp_role`.
    pub(crate) fn resolve_live_session_corp_and_roles(
        &self,
        session: &SessionState,
        now_unix: u64,
    ) -> (Option<u32>, u64) {
        let Some(cid) = session.character_id else {
            return (session.corporation_id.and_then(|c| u32::try_from(c).ok()), 0);
        };
        self.db.with_snapshot(|snap| {
            let db_corp = snap.characters.get(&cid).map(|c| c.corporation_id);
            let active_corp = match (db_corp, session.corporation_id) {
                (Some(dc), Some(sc)) if sc != dc => {
                    if (1_000_000..2_000_000).contains(&dc)
                        && snap
                            .corporations
                            .get(&sc)
                            .map_or(true, |c| c.ceo_id == cid || c.members.contains(&cid))
                    {
                        Some(sc)
                    } else {
                        Some(dc)
                    }
                }
                (Some(dc), _) => Some(dc),
                (None, sc) => sc,
            };
            let Some(corp_id) = active_corp else {
                return (None, 0);
            };
            let (is_ceo, eff_roles) =
                crate::services::social_corp_fleet::social_corp_projects::effective_corp_member_roles(
                    snap, corp_id, cid, 0, now_unix,
                );
            let mask = if is_ceo { u64::MAX } else { eff_roles };
            (u32::try_from(corp_id).ok(), mask)
        })
    }

    pub fn dispatch_service_gateway_mgr_rpc(
        &self,
        method: &str,
        args: &[PyRep],
        kwargs: &HashMap<String, PyRep>,
    ) -> Option<Result<PyRep, BridgeRpcError>> {
        match method {
            "qa_enable_hijack_mode" => {
                let (error, latency) = match parse_hijack_args(args, kwargs) {
                    Ok(v) => v,
                    Err(e) => return Some(Err(e)),
                };
                self.set_server_hijack_mode(true, error, latency);
                Some(Ok(PyRep::None))
            }
            "qa_disable_hijack_mode" => {
                self.set_server_hijack_mode(false, None, None);
                Some(Ok(PyRep::None))
            }
            "qa_is_hijack_mode_enabled" => Some(Ok(PyRep::Bool(self.is_server_hijack_mode_enabled()))),
            _ => None,
        }
    }

    pub fn dispatch_public_gateway_rpc(
        &self,
        session: &SessionState,
        method: &str,
        args: &[PyRep],
        kwargs: &HashMap<String, PyRep>,
        now_unix: u64,
    ) -> Option<Result<PyRep, BridgeRpcError>> {
        match method {
            "Subscribe" | "SubscribeToNotice" | "ConsumeNotices" => {
                if session.user_id.is_none() {
                    return Some(Err(BridgeRpcError {
                        code: "AuthenticationRequired".into(),
                        message: "publicGatewaySvc subscription requires an authenticated session".into(),
                        client_error: None,
                    }));
                }
                let mut notice_types = Vec::new();
                for arg in args {
                    match arg {
                        PyRep::String(s) => notice_types.push(s.clone()),
                        PyRep::List(items) | PyRep::Tuple(items) => {
                            for it in items {
                                if let Some(s) = it.as_str() {
                                    notice_types.push(s.to_string());
                                }
                            }
                        }
                        _ => {}
                    }
                }
                let rx = self.subscribe_session(session, notice_types, now_unix);
                self.stream_receivers.write().unwrap_or_else(|e| e.into_inner()).insert(session.session_id, rx);
                let app_uuid = Self::default_app_instance_uuid(session.session_id);
                Some(Ok(PyRep::keyval([
                    ("application_instance_uuid", PyRep::Bytes(app_uuid)),
                    ("subscribed", PyRep::Bool(true)),
                ])))
            }
            "PollNotices" => {
                let notices = self.poll_notices(session.session_id);
                Some(Ok(PyRep::List(notices.iter().map(notice_to_pyrep).collect())))
            }
            "PublishEvent" | "PublishEventPayload" => {
                if let Some(PyRep::Bytes(raw_event)) | Some(PyRep::Buffer(raw_event)) = args.first() {
                    if session.user_id.is_none() {
                        return Some(Err(BridgeRpcError {
                            code: "AuthenticationRequired".into(),
                            message: "publicGatewaySvc.PublishEvent requires authentication".into(),
                            client_error: None,
                        }));
                    }
                    let event = if method == "PublishEventPayload" {
                        if let Some(any) = GoogleAny::decode_proto(raw_event)
                            .ok()
                            .filter(|a| eve_proto::quasar::has_quasar_type_url_prefix(&a.type_url))
                        {
                            EvePublicEvent { payload: Some(any), ..Default::default() }
                        } else {
                            match EvePublicEvent::decode_proto(raw_event) {
                                Ok(ev) => ev,
                                Err(e) => return Some(Err(BridgeRpcError { code: "InvalidProtobuf".into(), message: e.to_string(), client_error: None })),
                            }
                        }
                    } else {
                        match EvePublicEvent::decode_proto(raw_event) {
                            Ok(ev) => ev,
                            Err(e) => return Some(Err(BridgeRpcError { code: "InvalidProtobuf".into(), message: e.to_string(), client_error: None })),
                        }
                    };
                    self.publish_event(session, event, now_unix);
                    return Some(Ok(PyRep::Bool(true)));
                }
                None
            }
            "SendRequest" | "CallQuasar" | "Send" => {
                let explicit_idem = kwargs.get("idempotency_key").and_then(|v| match v {
                    PyRep::Bytes(b) | PyRep::Buffer(b) => Some(b.clone()),
                    PyRep::String(s) => Some(s.as_bytes().to_vec()),
                    _ => None,
                });
                if let Some(PyRep::Bytes(raw_req)) | Some(PyRep::Buffer(raw_req)) =
                    args.first().or_else(|| kwargs.get("request"))
                {
                    let rsp = match EvePublicRequest::decode_proto(raw_req) {
                        Ok(req) => self.handle_public_request(session, &req, explicit_idem, false, now_unix),
                        Err(e) => EvePublicResponse {
                            dispatched: Some(GoogleTimestamp { seconds: now_unix as i64, nanos: 0 }),
                            status_code: STATUS_BAD_REQUEST,
                            status_message: format!("Malformed eve_public.Request wire frame: {e}"),
                            internal_origin: "evomu.quasar.public_gateway".into(),
                            application_instance_uuid: Self::default_app_instance_uuid(session.session_id),
                            gateway_instance_uuid: self.gateway_instance_uuid.clone(),
                            ..Default::default()
                        },
                    };
                    return Some(Ok(response_to_pyrep(&rsp)));
                }
                None
            }
            _ => None,
        }
    }
}

#[cfg(test)]
#[path = "quasar_gateway_helpers_edges_tests.rs"]
mod quasar_gateway_helpers_edges_tests;
