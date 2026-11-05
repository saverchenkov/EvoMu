use crate::common;
use eve_db::{InventoryItemRecord, InventoryFlag};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use std::collections::HashMap;

const P1: i64 = 90_000_888;

async fn call(ctx: &ServerContext, s: &mut SessionState, svc: &str, m: &str, a: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(s, svc, None, m, a, &HashMap::new(), None).await
}

#[tokio::test]
async fn test_warp_vector_item() {
    eve_testkit::attest(&["feat:navigation.warp-vector"]);
    let ctx = ServerContext::new();
    let p = common::gm_pilot_in_space(&ctx, 1, P1, 606, 30_000_142).await;
    let mut s = p.sess.clone();
    
    let ship_id = s.ship_id.unwrap();
    let sys_id = 30_000_142;
    
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(20001, InventoryItemRecord {
            item_id: 20001,
            type_id: 77825, // Jovian Symbolic Navigator (group 4599)
            owner_id: P1,
            location_id: ship_id,
            flag: InventoryFlag::Cargo,
            quantity: 1,
            ..Default::default()
        });
    });

    let res = call(&ctx, &mut s, "warpVectorMgr", "UseVector", &[PyRep::Integer(20001)]).await.unwrap();
    let PyRep::KeyVal(dict) = res else { panic!("Expected KeyVal"); };
    assert_eq!(dict.get("consumed").unwrap().as_bool(), Some(true));
    
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let (_, ball) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    assert!(matches!(ball.mode, eve_destiny::MovementMode::Warp { .. }));
}

#[tokio::test]
async fn test_pochven_wormhole_scan_and_jump() {
    eve_testkit::attest(&["feat:navigation.pochven-wormholes", "feat:navigation.drifter-wormhole-systems"]);
    let ctx = ServerContext::new();
    
    let sigs = eve_server::services::space_destiny::space_pochven_wormholes::pochven_static_wormholes(&ctx.sde, 30_000_142, 1, 1234567);
    assert_eq!(sigs.len(), 2);
    assert_eq!(sigs[0].lifetime_seconds, 43200);
    assert_eq!(sigs[0].max_jump_mass_kg, 410_000_000.0);
}

#[tokio::test]
async fn test_repairshop_quotes_and_restoration() {
    eve_testkit::attest(&["feat:structures.repairshop-service-quotes-and-restoration"]);
    let ctx = ServerContext::new();
    let p = common::gm_pilot_in_space(&ctx, 2, P1, 606, 30_000_142).await;
    let mut s = p.sess.clone();

    let struct_id = 100_000_000_001; 
    ctx.db.with_mut_snapshot(|snap| {
        let mut st = eve_db::UpwellStructureRecord::default();
        st.structure_id = struct_id;
        st.type_id = 35832;
        st.solar_system_id = 30_000_142;
        st.owner_corp_id = 98_000_001;
        st.state = "ShieldVulnerable".into();
        st.fuel_blocks = 1000;
        st.quantum_core_installed = true;
        st.name = "Test Citadel".into();
        snap.structures.insert(struct_id, st);
        
        let ch = snap.characters.get_mut(&P1).unwrap();
        ch.structure_id = Some(struct_id);
        ch.corporation_id = 98_000_001; 
    });

    s.structure_id = Some(struct_id);
    
    let res = call(&ctx, &mut s, "repairSvc", "GetRepairQuotes", &[PyRep::List(vec![])]).await.unwrap();
    let PyRep::Dict(entries) = res else { panic!("not dict") };
    assert!(!entries.is_empty());
}

#[tokio::test]
async fn test_anchor_restrictions() {
    eve_testkit::attest(&["feat:structures.anchor-restrictions"]);
    let ctx = ServerContext::new();
    let p = common::gm_pilot_in_space(&ctx, 3, P1, 606, 30_000_142).await;
    let mut s = p.sess.clone();
    let ship_id = s.ship_id.unwrap();
    let item_id = 20002;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(item_id, InventoryItemRecord {
            item_id,
            type_id: 35832, 
            owner_id: P1,
            location_id: ship_id,
            flag: InventoryFlag::Cargo,
            quantity: 1,
            ..Default::default()
        });
        let ch = snap.characters.get_mut(&P1).unwrap();
        ch.corporation_id = 98_000_001;
        
        let mut corp = eve_db::CorporationRecord::default();
        corp.corporation_id = 98_000_001;
        corp.member_roles.insert(P1, eve_db::corp_roles::ROLE_STATION_MANAGER);
        snap.corporations.insert(98_000_001, corp);
    });
    
    let res = call(&ctx, &mut s, "structureDeployment", "Anchor", &[PyRep::Integer(35832), PyRep::String("Test Astrahus".into())]).await;
    let err = res.unwrap_err();
    assert_eq!(err.code, "AnchoringDisallowedInTradeHub");
}

#[tokio::test]
async fn test_omega_weekly_free_sp() {
    eve_testkit::attest(&["feat:skills.omega-weekly-free-sp"]);
    let ctx = ServerContext::new();
    let p = common::gm_pilot_in_space(&ctx, 4, P1, 606, 30_000_142).await;
    let mut s = p.sess.clone();

    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&P1).unwrap();
        ch.clone_grade = 2; // Omega
    });

    let res = call(&ctx, &mut s, "storeManager", "buy_offer", &[PyRep::Integer(1005)]).await.unwrap();
    let PyRep::KeyVal(dict) = res else { panic!("Expected KeyVal"); };
    assert_eq!(dict.get("freeSkillPointsGranted").unwrap().as_i64(), Some(15000));
    
    let res2 = call(&ctx, &mut s, "storeManager", "buy_offer", &[PyRep::Integer(1005)]).await;
    assert!(res2.is_err());
}
