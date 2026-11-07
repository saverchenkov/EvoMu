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
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_001)],
        &HashMap::new(),
        None,
    )
    .await
    .unwrap();
    
    let args = vec![PyRep::Integer(30000142)];
    let res = ctx.dispatch_rpc(&mut sess, "sovMgr", None, "GetSovStructuresInfoForSolarSystem", &args, &HashMap::new(), None).await.unwrap();
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
    let res = ctx.dispatch_rpc(&mut sess, "lookupSvc", None, "LookupWarableCorporationsOrAlliances", &args, &HashMap::new(), None).await.unwrap();
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
    let res = ctx.dispatch_rpc(&mut sess, "cache", None, "Rowset", &args, &HashMap::new(), None).await.unwrap();
    let PyRep::Rowset { columns, rows: _ } = res else { panic!("expected rowset") };
    assert_eq!(columns[0], "roleID");
    assert_eq!(columns[1], "roleName");
}
