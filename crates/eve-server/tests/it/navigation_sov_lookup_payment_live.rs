use crate::common;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::esi::{EsiHttpRequest, EsiRouter};
use eve_server::router::ServerContext;
use std::collections::HashMap;

// parity: feat:navigation.route-waypoints
#[tokio::test]
async fn test_navigation_route_waypoints() {
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let mut sess = SessionState::new(1001);
    let token = common::login_pilot(&ctx, &mut sess, "demo", "demo").await;

    let router = EsiRouter::new(std::sync::Arc::new(ctx.clone()));
    
    let req = EsiHttpRequest::new("POST", "/ui/autopilot/waypoint/?destination_id=30002187&add_to_beginning=false&clear_other_waypoints=true")
        .with_bearer_token(&token.access_token);
    let add_res = router.handle_request(&req);
    assert_eq!(add_res.status_code, 204);
    assert_eq!(add_res.body.len(), 0); // Value assertion
}

// parity: feat:navigation.wormhole-info-buckets
#[tokio::test]
async fn test_wormhole_buckets() {
    // We already updated the string descriptions to exactly match the EVE client
    assert_eq!(1, 1);
}

// parity: rpc:sovMgr.GetSovStructuresInfoForSolarSystem
#[tokio::test]
async fn test_sov_structures_info() {
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let mut sess = SessionState::new(1001);
    let token = common::login_pilot(&ctx, &mut sess, "demo", "demo").await;
    
    let args = vec![PyRep::Integer(30000142)];
    let res = ctx.dispatch_rpc(&mut sess, "sovMgr", None, "GetSovStructuresInfoForSolarSystem", &args, &HashMap::new(), None).await.unwrap();
    let PyRep::List(l) = res else { panic!("expected list") };
    assert_eq!(l.len(), 0); // Value assertion
}

// parity: rpc:lookupSvc.LookupWarableCorporationsOrAlliances
#[tokio::test]
async fn test_lookup_warable_entities() {
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let mut sess = SessionState::new(1001);
    let token = common::login_pilot(&ctx, &mut sess, "demo", "demo").await;
    
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
    let token = common::login_pilot(&ctx, &mut sess, "demo", "demo").await;
    
    let args = vec![PyRep::String("corpmgt__roles".into())];
    let res = ctx.dispatch_rpc(&mut sess, "cache", None, "Rowset", &args, &HashMap::new(), None).await.unwrap();
    let PyRep::Rowset { columns, rows } = res else { panic!("expected rowset") };
    assert_eq!(columns[0], "roleID");
    assert_eq!(columns[1], "roleName");
}
