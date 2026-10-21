//! Regression tests for the RPCs the real V24.01 client calls between the handshake and the
//! character selection screen (T-6 `journey:launch_login`), plus the V24 `ErrorResponse` body.
//! Each one was an `UnimplementedRPC`, `CharacterNotSelected` or unmarshal error in a captured run.

use crate::common::*;
use eve_machonet::{wrapped_runtime_error_body, SessionState, MACHONETERR_WRAPPEDEXCEPTION};
use eve_proto::{marshal, PyRep};
use eve_server::router::ServerContext;
use std::collections::HashMap;

fn kv_get<'a>(rep: &'a PyRep, key: &str) -> Option<&'a PyRep> {
    match rep {
        PyRep::KeyVal(m) => m.get(key),
        _ => None,
    }
}

#[test]
fn error_response_body_wraps_a_pickled_runtime_error() {
    // machoNet: ErrorResponse(originalCommand, code, payload); ExceptionWrapperGPCS re-raises
    // LoadsSanitized(payload[0]) when code == MACHONETERR_WRAPPEDEXCEPTION.
    let body = wrapped_runtime_error_body(6, "UnimplementedRPC: config.X is not yet implemented");
    let PyRep::Tuple(items) = &body else { panic!("body must be a tuple") };
    assert_eq!(items[0], PyRep::Integer(6));
    assert_eq!(items[1], PyRep::Integer(MACHONETERR_WRAPPEDEXCEPTION));
    assert_eq!(MACHONETERR_WRAPPEDEXCEPTION, 2);
    let PyRep::Tuple(payload) = &items[2] else { panic!("payload must be a tuple") };
    let PyRep::Bytes(pickled) = &payload[0] else { panic!("payload[0] must be a marshal string") };
    let exc = marshal::decode(pickled).expect("pickled exception decodes");
    let PyRep::ObjectEx { is_type_2, header, .. } = exc else { panic!("ObjectEx1 expected") };
    assert!(!is_type_2);
    let PyRep::Tuple(h) = *header else { panic!("header tuple") };
    assert_eq!(h[0].as_str(), Some("exceptions.RuntimeError"));
    assert_eq!(
        h[1],
        PyRep::Tuple(vec![PyRep::ascii_bytes("UnimplementedRPC: config.X is not yet implemented")])
    );
}

#[tokio::test]
async fn login_path_rpcs_answer_before_character_selection() {
    let ctx = ServerContext::new();
    let mut sess = SessionState::new(9_101);
    login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    assert!(sess.character_id.is_none(), "these calls happen before SelectCharacterID");
    let kw = HashMap::new();
    let call = |svc: &'static str, m: &'static str, a: Vec<PyRep>| (svc, m, a);

    // alert telemetry: unknown hash -> (None, None); after a trace is sent the hash maps to an errorID.
    let hash = 0x5EED_1234_i64;
    for (svc, m, a) in [call("alert", "BeanCount", vec![PyRep::Integer(hash)])] {
        let r = ctx.dispatch_rpc(&mut sess, svc, None, m, &a, &kw, None).await.unwrap();
        assert_eq!(r, PyRep::Tuple(vec![PyRep::None, PyRep::None]));
    }
    let stack_id = PyRep::Tuple(vec![PyRep::Integer(hash), PyRep::ascii_bytes("k")]);
    let sent = ctx
        .dispatch_rpc(&mut sess, "alert", None, "SendClientStackTraceAlert",
            &[stack_id, PyRep::ascii_bytes("EXCEPTION #1 ...\nline2"), PyRep::ascii_bytes("Error"), PyRep::None],
            &kw, None)
        .await
        .unwrap();
    let PyRep::Tuple(t) = &sent else { panic!("(errorID, logMode)") };
    let error_id = t[0].as_i64().expect("errorID assigned");
    assert_eq!(t[1].as_str(), Some("B"));
    let again = ctx
        .dispatch_rpc(&mut sess, "alert", None, "BeanCount", &[PyRep::Integer(hash)], &kw, None)
        .await
        .unwrap();
    assert_eq!(again, sent, "BeanCount now returns the registered (errorID, logMode)");
    let before = eve_server::services::client_login::delivered_bean_count(error_id);
    let beans = PyRep::Dict(vec![(PyRep::Integer(error_id), PyRep::Tuple(vec![PyRep::Integer(3), PyRep::Integer(0)]))]);
    let d = ctx.dispatch_rpc(&mut sess, "alert", None, "BeanDelivery", &[beans], &kw, None).await.unwrap();
    assert_eq!(d, PyRep::None);
    assert_eq!(eve_server::services::client_login::delivered_bean_count(error_id), before + 3);

    let planets = ctx.dispatch_rpc(&mut sess, "config", None, "GetBlackListedPlanets", &[], &kw, None).await.unwrap();
    assert_eq!(planets, PyRep::List(Vec::new()));
    let old = ctx.dispatch_rpc(&mut sess, "config", None, "GetOldStationData", &[], &kw, None).await.unwrap();
    assert_eq!(old, PyRep::None);
    let npe = ctx.dispatch_rpc(&mut sess, "air_npe", None, "is_air_npe_enabled", &[], &kw, None).await.unwrap();
    assert_eq!(npe, PyRep::Bool(!ctx.sde.node_graphs.is_empty()));
    assert_eq!(npe, PyRep::Bool(true), "the real SDE ships AIR NPE node graphs");
    let desc = ctx.dispatch_rpc(&mut sess, "invbroker", None, "GetItemDescriptor", &[], &kw, None).await.unwrap();
    assert_eq!(desc, eve_proto::DBRowDescriptor::inv_item_header().to_pyrep());
}

#[tokio::test]
async fn character_selection_data_lists_the_accounts_characters() {
    let ctx = ServerContext::new();
    let mut sess = SessionState::new(9_102);
    login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let user_id = sess.user_id.expect("logged in");
    let expected = ctx.db.get_characters_for_user(user_id);
    assert!(!expected.is_empty(), "demo fixture owns a character");
    let kw = HashMap::new();
    let rep = ctx
        .dispatch_rpc(&mut sess, "charUnboundMgr", None, "GetCharacterSelectionData", &[], &kw, None)
        .await
        .unwrap();
    let PyRep::Tuple(parts) = &rep else { panic!("4-tuple") };
    assert_eq!(parts.len(), 4);
    let PyRep::List(user_rows) = &parts[0] else { panic!("userDetails list") };
    assert_eq!(kv_get(&user_rows[0], "characterSlots"), Some(&PyRep::Integer(3)));
    assert_eq!(kv_get(&user_rows[0], "userName"), Some(&PyRep::String("demo".into())));
    assert_eq!(parts[1], PyRep::Tuple(vec![PyRep::None, PyRep::None]));
    let PyRep::List(chars) = &parts[2] else { panic!("characterDetails list") };
    assert_eq!(chars.len(), expected.len());
    let c0 = &expected[0];
    let row = chars
        .iter()
        .find(|r| kv_get(r, "characterID") == Some(&PyRep::Integer(c0.character_id)))
        .expect("row for the first character");
    assert_eq!(kv_get(row, "corporationID"), Some(&PyRep::Integer(c0.corporation_id)));
    assert_eq!(kv_get(row, "solarSystemID"), Some(&PyRep::Integer(i64::from(c0.solar_system_id))));
    assert_eq!(kv_get(row, "balance"), Some(&PyRep::Real(c0.balance.to_wire_f64())));
    assert_eq!(kv_get(row, "raceID"), Some(&PyRep::Integer(i64::from(c0.race_id))));
    assert!(matches!(parts[3], PyRep::List(_)));
}

#[test]
fn keyval_and_rowsets_use_v24_module_paths() {
    // The V24 client has no `util` module; blue imports these classes by module path.
    let enc = marshal::encode(&PyRep::keyval([("a", PyRep::Integer(1))]));
    let needle = b"utillib.KeyVal";
    assert!(enc.windows(needle.len()).any(|w| w == needle));
    assert!(!enc.windows(11).any(|w| w == b"util.KeyVal"));
    let back = marshal::decode(&enc).unwrap();
    assert_eq!(kv_get(&back, "a"), Some(&PyRep::Integer(1)));
    let d = eve_proto::DBRowDescriptor::inv_item_header().to_pyrep();
    let denc = marshal::encode(&d);
    let ss = b"eve.common.script.sys.eveCfg.StackSize";
    assert!(denc.windows(ss.len()).any(|w| w == ss));
}
