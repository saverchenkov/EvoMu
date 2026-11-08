//! Integration regression tests for Upwell market ACLs (`#553`).

#![forbid(unsafe_code)]

use eve_db::{
    CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents, MarketOrderRecord,
    UpwellStructureRecord, WarRecord,
};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use eve_server::services::station_structure::structure_upwell_ops::{
    load_ext_state, save_ext_state, StructureAccessProfile,
};
use std::collections::HashMap;

const JITA_44: i64 = 60_003_760;
const JITA_SYS: u32 = 30_000_142;
const FORGE_REGION: u32 = 10_000_002;
const P1: i64 = 90_000_001;
const P2: i64 = 90_000_002;

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, svc, None, method, args, &HashMap::new(), None)
        .await
}

async fn login_char(
    ctx: &ServerContext,
    sess: &mut SessionState,
    username: &str,
    password: &str,
    char_id: i64,
) {
    crate::common::login_pilot(ctx, sess, username, password).await;
    rpc(ctx, sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)])
        .await
        .expect("SelectCharacterID");
}

#[tokio::test]
async fn issue_553_market_acl_hides_denied_upwell_orders() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut s1 = SessionState::new(1);
    let mut s2 = SessionState::new(2);
    login_char(&ctx, &mut s1, "demo", "demo-secret", P1).await;
    login_char(&ctx, &mut s2, "other", "other-secret", P2).await;

    let (owner_corp, enemy_corp, fort_id, mod_id, fort_order, npc_order) = (
        98_553_001_i64,
        98_553_002_i64,
        1_000_000_553_001_i64,
        955_300_001_i64,
        955_300_002_i64,
        955_300_003_i64,
    );
    let now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.insert(owner_corp, CorporationRecord {
            corporation_id: owner_corp, name: "Fort Owner".into(), ticker: "FORT".into(), ceo_id: P1, ..Default::default()
        });
        snap.corporations.insert(enemy_corp, CorporationRecord {
            corporation_id: enemy_corp, name: "Guest Corp".into(), ticker: "GST".into(), ceo_id: P2, ..Default::default()
        });
        snap.characters.get_mut(&P1).unwrap().corporation_id = owner_corp;
        snap.characters.get_mut(&P2).unwrap().corporation_id = enemy_corp;

        snap.structures.insert(fort_id, UpwellStructureRecord {
            structure_id: fort_id, type_id: 35833, owner_corp_id: owner_corp,
            solar_system_id: JITA_SYS, name: "Private Market Fortizar".into(),
            state: "ShieldVulnerable".into(), quantum_core_installed: true, fuel_blocks: 100,
            ..Default::default()
        });
        snap.items.insert(mod_id, InventoryItemRecord {
            item_id: mod_id, type_id: 35_892, owner_id: owner_corp, location_id: fort_id,
            flag: InventoryFlag::ServiceSlot0, quantity: 1, singleton: true, ..Default::default()
        });
        snap.module_online_states.insert(mod_id, true);

        snap.market_orders.insert(fort_order, MarketOrderRecord {
            order_id: fort_order, owner_id: P1, station_id: fort_id, region_id: FORGE_REGION,
            type_id: 34, is_bid: false, price: IskCents::from_isk(4), vol_entered: 1_000, vol_remaining: 1_000,
            min_volume: 1, is_npc: false, escrow: IskCents(0),
        });
        snap.market_orders.insert(npc_order, MarketOrderRecord {
            order_id: npc_order, owner_id: P1, station_id: JITA_44, region_id: FORGE_REGION,
            type_id: 34, is_bid: false, price: IskCents::from_isk(6), vol_entered: 1_000, vol_remaining: 1_000,
            min_volume: 1, is_npc: false, escrow: IskCents(0),
        });
        let mut ext = load_ext_state(snap);
        let mut gen = HashMap::new();
        gen.insert(4, true);
        ext.access_profiles.insert(fort_id, StructureAccessProfile {
            profile_id: fort_id, owner_corp_id: owner_corp, name: "Public Market".into(),
            corp_only: false, general_settings: gen, ..Default::default()
        });
        save_ext_state(snap, &ext);
        for cid in [P1, P2] {
            if let Some(ch) = snap.characters.get_mut(&cid) {
                ch.station_id = None;
                ch.structure_id = Some(fort_id);
            }
        }
    });

    for s in [&mut s1, &mut s2] {
        s.region_id = Some(FORGE_REGION);
        s.solar_system_id = Some(JITA_SYS);
        s.station_id = None;
        s.structure_id = Some(fort_id);
    }

    // Initially P2 is neutral and structure is public -> P2 sees both Fortizar and NPC Jita orders
    let count_get_orders = |rep: PyRep| -> Vec<i64> {
        let PyRep::Tuple(mut parts) = rep else { panic!("expected Tuple: {rep:?}") };
        let sells = parts.remove(0);
        let mut ids: Vec<i64> = sells
            .crowset_rows()
            .expect("expected CRowset for sells")
            .into_iter()
            .filter_map(|row| row.dbrow_get("orderID").and_then(|v| v.as_i64()))
            .filter(|id| *id == fort_order || *id == npc_order)
            .collect();
        ids.sort_unstable();
        ids
    };
    let entry_34 = |rep: PyRep| -> Option<(f64, i64)> {
        let PyRep::ObjectEx { dict, .. } = rep else { return None };
        dict.into_iter()
            .find(|(k, _)| k.as_i64() == Some(34))
            .map(|(_, row)| (
                row.dbrow_get("price").and_then(|v| v.as_f64()).unwrap_or(0.0),
                row.dbrow_get("stationID").and_then(|v| v.as_i64()).unwrap_or(0),
            ))
    };
    let pub_ids = count_get_orders(rpc(&ctx, &mut s2, "marketProxy", "GetOrders", &[PyRep::Integer(34)]).await.unwrap());
    assert_eq!(pub_ids, vec![fort_order, npc_order]);
    assert_eq!(entry_34(rpc(&ctx, &mut s2, "marketProxy", "GetStationAsks", &[]).await.unwrap()), Some((4.0, fort_id)));

    // Case A: docking ACL denied blocks GetOrders, GetStationAsks, GetRegionBest, GetSystemAsks for P2 (P1 still sees!)
    ctx.db.with_mut_snapshot(|snap| {
        snap.structure_docking_acl_denied.entry(fort_id).or_default().insert(P2);
    });
    assert_eq!(count_get_orders(rpc(&ctx, &mut s1, "marketProxy", "GetOrders", &[PyRep::Integer(34)]).await.unwrap()), vec![fort_order, npc_order]);
    assert_eq!(count_get_orders(rpc(&ctx, &mut s2, "marketProxy", "GetOrders", &[PyRep::Integer(34)]).await.unwrap()), vec![npc_order]);
    assert_eq!(entry_34(rpc(&ctx, &mut s2, "marketProxy", "GetStationAsks", &[]).await.unwrap()), None);
    let reg_best = entry_34(rpc(&ctx, &mut s2, "marketProxy", "GetRegionBest", &[]).await.unwrap());
    assert!(matches!(reg_best, Some((p, st)) if st == JITA_44 && p >= 5.0), "GetRegionBest must exclude denied Fortizar 4.0 ask: {reg_best:?}");
    let sys_asks = entry_34(rpc(&ctx, &mut s2, "marketProxy", "GetSystemAsks", &[PyRep::Integer(i64::from(JITA_SYS))]).await.unwrap());
    assert!(matches!(sys_asks, Some((p, st)) if st == JITA_44 && p >= 5.0), "GetSystemAsks must exclude denied Fortizar 4.0 ask: {sys_asks:?}");

    // Case B: negative standing blocks P2
    ctx.db.with_mut_snapshot(|snap| {
        snap.structure_docking_acl_denied.clear();
        snap.characters.get_mut(&P2).unwrap().standings.insert(owner_corp, -5.0);
    });
    assert_eq!(count_get_orders(rpc(&ctx, &mut s2, "marketProxy", "GetOrders", &[PyRep::Integer(34)]).await.unwrap()), vec![npc_order]);

    // Case C: active war blocks P2
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&P2).unwrap().standings.clear();
        snap.wars.insert(700_553, WarRecord {
            war_id: 700_553, aggressor_corp_id: enemy_corp, defender_corp_id: owner_corp,
            mutual: false, started_unix: now, active: true,
        });
    });
    assert_eq!(count_get_orders(rpc(&ctx, &mut s2, "marketProxy", "GetOrders", &[PyRep::Integer(34)]).await.unwrap()), vec![npc_order]);

    // Case D: Upwell StructureAccessProfile denying market service (SETTING_MARKET_TAX = 4 -> false) blocks P2
    ctx.db.with_mut_snapshot(|snap| {
        snap.wars.clear();
        let mut ext = load_ext_state(snap);
        let mut gen = HashMap::new();
        gen.insert(4, false);
        ext.access_profiles.insert(fort_id, StructureAccessProfile {
            profile_id: fort_id, owner_corp_id: owner_corp, name: "No Market".into(),
            general_settings: gen, ..Default::default()
        });
        save_ext_state(snap, &ext);
    });
    assert_eq!(count_get_orders(rpc(&ctx, &mut s2, "marketProxy", "GetOrders", &[PyRep::Integer(34)]).await.unwrap()), vec![npc_order]);
}
