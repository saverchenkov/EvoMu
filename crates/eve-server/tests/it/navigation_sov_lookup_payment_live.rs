use crate::common;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::HashMap;

#[tokio::test]
async fn test_sov_structures_info() {
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let mut sess = SessionState::new(1001);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    common::borrow_wire_harness(&ctx, &mut sess)
        .call_ok(
            "charUnboundMgr",
            None,
            "SelectCharacterID",
            &[PyRep::Integer(90_000_001)],
            &HashMap::new(),
        )
        .await;
    
    let args = vec![PyRep::Integer(30000142)];
    let res = common::borrow_wire_harness(&ctx, &mut sess)
        .call_ok("sovMgr", None, "GetSovStructuresInfoForSolarSystem", &args, &HashMap::new())
        .await;
    let PyRep::List(l) = res else { panic!("expected list") };
    assert_eq!(l.len(), 0); // Value assertion
}

#[tokio::test]
async fn test_lookup_warable_entities() {
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let mut sess = SessionState::new(1001);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    
    let args = vec![PyRep::String("WarCorp".into()), PyRep::Bool(true)];
    let res = common::borrow_wire_harness(&ctx, &mut sess)
        .call_ok("lookupSvc", None, "LookupWarableCorporationsOrAlliances", &args, &HashMap::new())
        .await;
    let PyRep::List(l) = res else { panic!("expected list") };
    assert_eq!(l.len(), 0); // Value assertion
}

// parity: rpc:cache.Rowset
#[tokio::test]
async fn test_cache_rowset_corpmgt_roles() {
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let mut sess = SessionState::new(1001);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    
    let args = vec![PyRep::String("corpmgt__roles".into())];
    let res = common::borrow_wire_harness(&ctx, &mut sess)
        .call_ok("cache", None, "Rowset", &args, &HashMap::new())
        .await;
    let PyRep::Rowset { columns, rows: _ } = res else { panic!("expected rowset") };
    assert_eq!(columns[0], "roleID");
    assert_eq!(columns[1], "roleName");
}
