//! Upwell Structure Lifecycle, Asset Safety, Reinforcement, Repair & Hack Reveal
//! parity tests (`plan_upwell_structures_closure.rs`).

use crate::esi_routes::issue_token_with_scopes;
use eve_db::{InventoryFlag, InventoryItemRecord, UpwellStructureRecord};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::esi::{EsiHttpRequest, EsiRouter};
use eve_server::router::ServerContext;
use eve_server::services::station_structure::structure_lifecycle as life;
use eve_server::services::station_structure::structure_upwell_ops::{load_ext_state, save_ext_state};
use glam::DVec3;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;

const OWNER: i64 = 90_000_001; const HACKER: i64 = 90_000_002; const BYSTANDER: i64 = 90_000_003; const RIVAL_CORP: i64 = 98_999_999;
const JITA: u32 = 30_000_142; const RANCER: u32 = 30_002_718; const HED_GP: u32 = 30_001_161;

fn prepare_siege(ctx: &ServerContext, id: i64, charge_dmg_em: f64) -> i64 {
    ctx.db.with_mut_snapshot(|snap| {
        let my_corp = snap.characters.get(&OWNER).map(|c| c.corporation_id).expect("corp");
        if let Some(s) = snap.structures.get_mut(&id) { s.owner_corp_id = RIVAL_CORP; }
        snap.wars.insert(77, eve_db::WarRecord { war_id: 77, aggressor_corp_id: my_corp, defender_corp_id: RIVAL_CORP, mutual: true, started_unix: 0, active: true });
        if let Some(w) = snap.items.get_mut(&100_000_005) {
            w.location_id = 100_000_001; w.flag = InventoryFlag::HiSlot0; w.singleton = true;
            if charge_dmg_em > 0.0 { w.dynamic_attributes.insert(64, 5_000.0); } else { w.dynamic_attributes.remove(&64); }
        }
        let old_ammo: Vec<i64> = snap.items.values().filter(|i| i.location_id == 100_000_001 && i.flag == InventoryFlag::HiSlot0 && i.item_id != 100_000_005).map(|i| i.item_id).collect();
        for aid in old_ammo { snap.items.remove(&aid); }
    });
    let ammo_id = grant(ctx, OWNER, 100_000_001, 185, InventoryFlag::HiSlot0, 10_000);
    ctx.invalidate_ship_fitting(100_000_001);
    ammo_id
}

async fn rpc(ctx: &ServerContext, sess: &mut SessionState, service: &str, method: &str, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None).await
}
fn kv_i64(rep: &PyRep, key: &str) -> i64 { rep.dict_get(key).and_then(|v| v.as_i64()).unwrap_or(0) }
fn kv_str(rep: &PyRep, key: &str) -> String { rep.dict_get(key).and_then(|v| v.as_str()).unwrap_or("").to_string() }

async fn fire_weapon(ctx: &ServerContext, sess: &mut SessionState, target_id: i64) -> Result<PyRep, BridgeRpcError> {
    let (sys_id, ship_id) = (sess.solar_system_id.expect("sys"), sess.ship_id.expect("ship"));
    let sys = ctx.systems.get_or_spawn(sys_id);
    if !sys.get_target_locks(ship_id).await.unwrap_or_default().iter().any(|l| l.target_id == target_id && l.locked) {
        rpc(ctx, sess, "dogmaIM", "AddTarget", &[PyRep::Integer(target_id)]).await?;
        let _ = sys.step_manual(12.0).await;
    }
    rpc(ctx, sess, "dogmaIM", "Activate", &[PyRep::Integer(100_000_005), PyRep::String("projectileFired".into()), PyRep::Integer(target_id)]).await
}

fn grant(ctx: &ServerContext, owner: i64, location: i64, type_id: u32, flag: InventoryFlag, qty: i32) -> i64 {
    ctx.db.with_mut_snapshot(|snap| {
        snap.next_entity_id += 1;
        let id = snap.next_entity_id;
        snap.items.insert(id, InventoryItemRecord { item_id: id, type_id, owner_id: owner, location_id: location, flag, quantity: qty, singleton: qty == 1, version: 1, ..Default::default() });
        id
    })
}

fn attr(ctx: &ServerContext, type_id: u32, attr_id: u16) -> f64 {
    ctx.sde.types.get(&type_id).and_then(|t| t.default_attributes.get(&attr_id).copied()).unwrap_or_else(|| panic!("SDE type {type_id} lacks attribute {attr_id}"))
}

fn new_test_ctx() -> Arc<ServerContext> {
    let ctx = Arc::new(ServerContext::new());
    ctx.set_clock(Arc::new(eve_db::VirtualServerClock::with_fixed_base(1_800_000_000)));
    ctx
}

async fn pilot_in_space(ctx: &ServerContext, sess_id: u64) -> SessionState {
    let mut sess = SessionState::new(sess_id);
    rpc(ctx, &mut sess, "authentication", "Login", &[PyRep::String("demo".into()), PyRep::String("demo-secret".into())]).await.expect("Login");
    rpc(ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(OWNER)]).await.expect("Select");
    ctx.advance_secs(12);
    rpc(ctx, &mut sess, "corpRegistry", "AddCorporation", &[PyRep::String("Upwell Closure Corp".into()), PyRep::String("UPWL".into())]).await.expect("AddCorporation");
    ctx.advance_secs(12);
    rpc(ctx, &mut sess, "structureDocking", "Undock", &[]).await.expect("Undock");
    sess
}

async fn move_ship(ctx: &ServerContext, sess: &SessionState, pos: DVec3) {
    assert!(ctx.systems.get_or_spawn(sess.solar_system_id.expect("sys")).set_ball_position(sess.ship_id.expect("ship"), pos).await);
}

fn open_space(ctx: &ServerContext, sys_id: u32, k: f64) -> DVec3 {
    let base = ctx.sde.stations.values().find(|s| s.solar_system_id == sys_id).map(|s| s.position).unwrap_or(DVec3::ZERO);
    base + DVec3::new(0.0, 5.0e9 + k * 3.0e9, 0.0)
}

async fn anchor(ctx: &ServerContext, sess: &mut SessionState, type_id: u32, name: &str) -> Result<PyRep, BridgeRpcError> {
    let ship = sess.ship_id.expect("ship");
    grant(ctx, OWNER, ship, type_id, InventoryFlag::Cargo, 1);
    rpc(ctx, sess, "structureDeployment", "Anchor", &[PyRep::Integer(i64::from(type_id)), PyRep::String(name.into())]).await
}

async fn observe(ctx: &ServerContext, sess: &mut SessionState, id: i64) -> Option<UpwellStructureRecord> {
    let _ = rpc(ctx, sess, "structureGuests", "GetGuests", &[PyRep::Integer(id)]).await;
    ctx.db.with_snapshot(|snap| snap.structures.get(&id).cloned())
}

async fn online_structure(ctx: &ServerContext, sess: &mut SessionState, type_id: u32, name: &str) -> i64 {
    let id = kv_i64(&anchor(ctx, sess, type_id, name).await.expect("Anchor"), "structureID");
    ctx.advance_virtual_clock(life::ANCHOR_VULNERABLE_SECS + life::ANCHORING_SECS);
    assert_eq!(observe(ctx, sess, id).await.expect("st").state, life::STATE_NO_CORE);
    let core = attr(ctx, type_id, life::ATTR_REQUIRES_DEED_TYPE) as u32;
    rpc(ctx, sess, "structureDocking", "Dock", &[PyRep::Integer(id)]).await.expect("Dock");
    let core_item = grant(ctx, OWNER, sess.ship_id.expect("ship"), core, InventoryFlag::Cargo, 1);
    rpc(ctx, sess, "invbroker", "Add", &[PyRep::Integer(core_item), PyRep::Integer(id), PyRep::Integer(172)]).await.expect("move core");
    assert_eq!(observe(ctx, sess, id).await.expect("st").state, life::STATE_ONLINING);
    ctx.advance_virtual_clock(life::ONLINING_SECS);
    assert_eq!(observe(ctx, sess, id).await.expect("st").state, life::STATE_SHIELD_VULNERABLE);
    id
}

#[tokio::test]
async fn test_upwell_lifecycle_asset_safety_abandoned_and_leave_space() {
    // parity: feat:lifecycle.asset_safety-auto_delivery, feat:structures.asset-safety-recovery-delay-and-fee, feat:lifecycle.structure-abandoned_destroyed, feat:lifecycle.structure-leave_space
    eve_testkit::attest(&[
        "feat:lifecycle.asset_safety-auto_delivery", "feat:structures.asset-safety-recovery-delay-and-fee",
        "feat:lifecycle.structure-abandoned_destroyed", "feat:lifecycle.structure-leave_space",
    ]);

    let ctx = new_test_ctx();
    let mut sess = pilot_in_space(&ctx, 7101).await;

    // 1. Security band mapping & same-system Euclidean AU distance for nearest_npc_station_from_pos.
    let mut jita_stations: Vec<_> = ctx.sde.stations.values().filter(|s| s.solar_system_id == JITA).collect();
    jita_stations.sort_by_key(|s| s.station_id);
    let (st1, st2) = (jita_stations[0], jita_stations[1]);
    assert_eq!(life::nearest_npc_station_from_pos(&ctx, JITA, st1.position + DVec3::new(1000.0, 0.0, 0.0)), Some(st1.station_id));
    assert_eq!(life::nearest_npc_station_from_pos(&ctx, JITA, st2.position + DVec3::new(1000.0, 0.0, 0.0)), Some(st2.station_id));
    let pos_jita = open_space(&ctx, JITA, 1.0);
    let st_high = life::nearest_npc_station_from_pos(&ctx, JITA, pos_jita).expect("high-sec station");
    assert_eq!(ctx.sde.stations.get(&st_high).expect("station").solar_system_id, JITA);
    let st_low = life::nearest_npc_station_from_pos(&ctx, RANCER, DVec3::ZERO).expect("low-sec station");
    assert!((0.0..0.45).contains(&ctx.sde.solar_systems.get(&ctx.sde.stations.get(&st_low).unwrap().solar_system_id).unwrap().security_status));
    let st_null = life::nearest_npc_station_from_pos(&ctx, HED_GP, DVec3::ZERO).expect("null-sec -> low-sec station");
    assert!((0.0..0.45).contains(&ctx.sde.solar_systems.get(&ctx.sde.stations.get(&st_null).unwrap().solar_system_id).unwrap().security_status));

    // 2. Anchor two Astrahus structures in Jita: one source, one same-system delivery destination.
    move_ship(&ctx, &sess, pos_jita).await;
    let src_id = online_structure(&ctx, &mut sess, 35832, "Safety Source Astrahus").await;
    let item_5d = grant(&ctx, OWNER, src_id, 34, InventoryFlag::Hangar, 10_000);
    assert_eq!(rpc(&ctx, &mut sess, "structureAssetSafety", "MovePersonalAssetsToSafety", &[PyRep::Integer(i64::from(JITA)), PyRep::Integer(src_id)]).await.expect_err("docked in target").code, "CannotAssetSafetyWhileDockedInTargetStructure");

    rpc(&ctx, &mut sess, "structureDocking", "Undock", &[]).await.expect("Undock");
    move_ship(&ctx, &sess, open_space(&ctx, JITA, 2.0)).await;
    let dst_id = online_structure(&ctx, &mut sess, 35832, "Safety Dest Astrahus").await;

    // 3. Manual MovePersonalAssetsToSafety & MoveCorpAssetsToSafety (skipping boarded ship).
    let wrap_5d = kv_i64(&rpc(&ctx, &mut sess, "structureAssetSafety", "MovePersonalAssetsToSafety", &[PyRep::Integer(i64::from(JITA)), PyRep::Integer(src_id)]).await.expect("MovePersonalAssetsToSafety"), "wrapID");
    assert!(wrap_5d > 0);

    let corp_id = ctx.db.with_snapshot(|snap| snap.characters.get(&OWNER).unwrap().corporation_id);
    let boarded_corp_ship = grant(&ctx, corp_id, src_id, 606, InventoryFlag::CorpSAG1, 1);
    let unboarded_corp_ore = grant(&ctx, corp_id, src_id, 34, InventoryFlag::CorpSAG1, 2_500);
    ctx.db.with_mut_snapshot(|snap| {
        let mut mate = snap.characters.get(&OWNER).unwrap().clone();
        mate.character_id = HACKER; mate.user_id = 2; mate.active_ship_id = boarded_corp_ship;
        snap.characters.insert(HACKER, mate);
    });
    let corp_wrap_live = kv_i64(&rpc(&ctx, &mut sess, "structureAssetSafety", "MoveCorpAssetsToSafety", &[PyRep::Integer(i64::from(JITA)), PyRep::Integer(src_id)]).await.expect("MoveCorpAssetsToSafety"), "wrapID");
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items.get(&boarded_corp_ship).map(|i| i.location_id), Some(src_id));
        assert_eq!(snap.items.get(&unboarded_corp_ore).map(|i| i.location_id), Some(corp_wrap_live));
    });

    // Wormhole manual asset safety rejected; Faction Fortizar (47512) never becomes Abandoned.
    let (wh_sys_id, wh_manual_st) = (31_000_001u32, 990_000_099i64);
    let wh_hangar_p = grant(&ctx, OWNER, wh_manual_st, 34, InventoryFlag::Hangar, 100);
    let wh_hangar_c = grant(&ctx, corp_id, wh_manual_st, 34, InventoryFlag::CorpSAG1, 100);
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(wh_manual_st, UpwellStructureRecord {
            structure_id: wh_manual_st, owner_corp_id: corp_id, type_id: 35832, solar_system_id: wh_sys_id,
            name: "WH Manual Safety".into(), state: life::STATE_SHIELD_VULNERABLE.into(), position: [0.0, 0.0, 0.0],
            shield_hp: 10_000.0, armor_hp: 10_000.0, hull_hp: 10_000.0, fuel_blocks: 1_000, reinforced_until_unix: 0,
            last_damage_tick: 0, last_damage_unix: 0, self_destruct_at_unix: None, quantum_core_installed: true,
        });
        let ffort_id = 990_000_088i64;
        snap.structures.insert(ffort_id, UpwellStructureRecord {
            structure_id: ffort_id, owner_corp_id: corp_id, type_id: 47512, solar_system_id: JITA,
            name: "Moreau Fortizar".into(), state: life::STATE_SHIELD_VULNERABLE.into(), position: [0.0, 0.0, 0.0],
            shield_hp: 10_125_000.0, armor_hp: 8_100_000.0, hull_hp: 32_400_000.0, fuel_blocks: 0, reinforced_until_unix: 0,
            last_damage_tick: 0, last_damage_unix: 0, self_destruct_at_unix: None, quantum_core_installed: true,
        });
        let mut ext = load_ext_state(snap);
        ext.lifecycle.entry(ffort_id).or_default().low_power_since_unix = Some(1);
        assert_eq!(life::power_state(&ctx, snap, &ext, snap.structures.get(&ffort_id).unwrap(), ctx.now_unix()), life::PowerState::LowPower);
        save_ext_state(snap, &ext);
    });
    assert_eq!(rpc(&ctx, &mut sess, "structureAssetSafety", "MovePersonalAssetsToSafety", &[PyRep::Integer(i64::from(wh_sys_id)), PyRep::Integer(wh_manual_st)]).await.expect_err("wh personal").code, "AssetSafetyUnavailableInWormhole");
    assert_eq!(rpc(&ctx, &mut sess, "structureAssetSafety", "MoveCorpAssetsToSafety", &[PyRep::Integer(i64::from(wh_sys_id)), PyRep::Integer(wh_manual_st)]).await.expect_err("wh corp").code, "AssetSafetyUnavailableInWormhole");

    // Wormhole destruction drops hangar contents as space loot; NO Asset Safety wrap created (feat:structures.asset-safety-recovery-delay-and-fee).
    let wh_disposal = ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        life::destroy_structure(&ctx, snap, &mut ext, wh_manual_st, OWNER, ctx.now_unix())
    });
    assert!(wh_disposal.asset_safety_wrap_ids.is_empty(), "Wormhole destruction must create zero Asset Safety wraps");
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items.get(&wh_hangar_p).map(|i| i.location_id), Some(i64::from(wh_sys_id)), "Personal items drop directly in wormhole space");
        assert_eq!(snap.items.get(&wh_hangar_c).map(|i| i.location_id), Some(i64::from(wh_sys_id)), "Corp items drop directly in wormhole space");
    });

    // Before 5 days: rejected; at 6 days (< 20 days): same-system structure delivery charges 0.5%.
    ctx.advance_virtual_clock(4 * 86_400);
    assert_eq!(rpc(&ctx, &mut sess, "structureAssetSafety", "MoveSafetyWrapToStructure", &[PyRep::Integer(wrap_5d), PyRep::Integer(dst_id)]).await.expect_err("early").code, "AssetSafetyCooldownActive");
    ctx.advance_virtual_clock(2 * 86_400);
    let est_val = ctx.sde.types.get(&34).unwrap().base_price * 10_000.0;
    let deliv_5d = rpc(&ctx, &mut sess, "structureAssetSafety", "MoveSafetyWrapToStructure", &[PyRep::Integer(wrap_5d), PyRep::Integer(dst_id)]).await.expect("deliver 5d");
    assert!((deliv_5d.dict_get("feePaidISK").and_then(|v| v.as_f64()).unwrap_or(-1.0) - est_val * 0.005).abs() < 1.0);
    ctx.db.with_snapshot(|snap| assert_eq!(snap.items.get(&item_5d).map(|i| (i.location_id, i.flag)), Some((dst_id, InventoryFlag::Deliveries))));

    // 4. Lazy 20-day auto-delivery triggered directly via ESI GET /characters/{id}/assets/ & GET /corporations/{id}/assets/ (with NO prior structure RPC!).
    // Destroying a structure in stationless null-sec (HED-GP) triggers cross-system auto-delivery to the nearest lowsec NPC station (st_null) at 15% fee.
    let (null_sys_id, null_st_id) = (HED_GP, 990_000_077i64);
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(null_st_id, UpwellStructureRecord {
            structure_id: null_st_id, owner_corp_id: corp_id, type_id: 35832, solar_system_id: null_sys_id,
            name: "Null Safety Source".into(), state: life::STATE_SHIELD_VULNERABLE.into(), position: [0.0, 0.0, 0.0],
            shield_hp: 10_000.0, armor_hp: 10_000.0, hull_hp: 10_000.0, fuel_blocks: 100, reinforced_until_unix: 0,
            last_damage_tick: 0, last_damage_unix: 0, self_destruct_at_unix: None, quantum_core_installed: true,
        });
    });
    let item_20d = grant(&ctx, OWNER, null_st_id, 34, InventoryFlag::Hangar, 20_000);
    let wrap_20d = kv_i64(&rpc(&ctx, &mut sess, "structureAssetSafety", "MovePersonalAssetsToSafety", &[PyRep::Integer(i64::from(null_sys_id)), PyRep::Integer(null_st_id)]).await.expect("wrap 20d"), "wrapID");
    let corp_item_20d = grant(&ctx, corp_id, null_st_id, 34, InventoryFlag::CorpSAG1, 10_000);
    ctx.db.with_mut_snapshot(|snap| { if let Some(m) = snap.characters.get_mut(&HACKER) { m.active_ship_id = 0; } });
    let corp_wrap_20d = kv_i64(&rpc(&ctx, &mut sess, "structureAssetSafety", "MoveCorpAssetsToSafety", &[PyRep::Integer(i64::from(null_sys_id)), PyRep::Integer(null_st_id)]).await.expect("corp wrap 20d"), "wrapID");

    ctx.advance_virtual_clock(life::ASSET_SAFETY_AUTO_DELIVERY_SECS + 10);
    let esi_router = EsiRouter::new(Arc::clone(&ctx));
    let token = issue_token_with_scopes(&ctx, OWNER, vec!["esi-assets.read_assets.v1".into(), "esi-assets.read_corporation_assets.v1".into()]);
    let char_resp = esi_router.handle_request(&EsiHttpRequest::new("GET", format!("/v5/characters/{OWNER}/assets/")).with_bearer_token(&token));
    assert_eq!(char_resp.status_code, 200);
    let char_json: Value = serde_json::from_slice(&char_resp.body).unwrap();
    assert!(char_json.as_array().unwrap().iter().any(|a| a["item_id"].as_i64() == Some(wrap_20d) && a["location_id"].as_i64() == Some(st_null)), "ESI character assets must lazily auto-deliver 20d wrap to nearest NPC station");
    let corp_resp = esi_router.handle_request(&EsiHttpRequest::new("GET", format!("/v5/corporations/{corp_id}/assets/")).with_bearer_token(&token));
    assert_eq!(corp_resp.status_code, 200);
    let corp_json: Value = serde_json::from_slice(&corp_resp.body).unwrap();
    assert!(corp_json.as_array().unwrap().iter().any(|a| a["item_id"].as_i64() == Some(corp_wrap_20d) && a["location_id"].as_i64() == Some(st_null)), "ESI corporation assets must lazily auto-deliver 20d corp wrap to nearest NPC station");

    let est_val_20d = ctx.sde.types.get(&34).unwrap().base_price * 20_000.0;
    let deliv_20d = rpc(&ctx, &mut sess, "structureAssetSafety", "MoveSafetyWrapToStructure", &[PyRep::Integer(wrap_20d), PyRep::Integer(st_null)]).await.expect("retrieve 20d");
    assert!((deliv_20d.dict_get("feePaidISK").and_then(|v| v.as_f64()).unwrap_or(-1.0) - est_val_20d * 0.15).abs() < 1.0);
    ctx.db.with_snapshot(|snap| assert_eq!(snap.items.get(&item_20d).map(|i| (i.location_id, i.flag)), Some((st_null, InventoryFlag::Deliveries))));

    // Corporate asset safety retrieval debits corp wallet division 1 and records BOTH wallet_journal and corp_wallet_journals.
    ctx.db.with_mut_snapshot(|snap| snap.corporations.get_mut(&corp_id).unwrap().set_division_balance(1, eve_db::IskCents::from_f64_isk(50_000_000.0)));
    let corp_deliv = rpc(&ctx, &mut sess, "structureAssetSafety", "MoveSafetyWrapToStructure", &[PyRep::Integer(corp_wrap_20d), PyRep::Integer(st_null)]).await.expect("retrieve corp 20d");
    let corp_fee = corp_deliv.dict_get("feePaidISK").and_then(|v| v.as_f64()).unwrap_or(0.0);
    assert!(corp_fee > 0.0);
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items.get(&corp_item_20d).map(|i| i.location_id), Some(st_null));
        let cj = snap.corp_wallet_journals.get(&(corp_id, 1)).expect("corp_wallet_journals division 1");
        assert!(cj.iter().any(|e| e.ref_type == "AssetSafetyRecoveryTax" && (e.amount + corp_fee).abs() < 1e-2));
    });

    // 5. Unanchor completion (leave_space): wraps SpecializedFuelBay items into corp Asset Safety, removes structure ball, spawns packaged structure & dropped core balls, emits notifications.
    let dst_ship = sess.ship_id.unwrap();
    let dst_fb = grant(&ctx, OWNER, dst_ship, 4247, InventoryFlag::Cargo, 250);
    let dst_oz = grant(&ctx, OWNER, dst_ship, 16273, InventoryFlag::Cargo, 400);
    rpc(&ctx, &mut sess, "invbroker", "Add", &[PyRep::Integer(dst_fb), PyRep::Integer(dst_id), PyRep::Integer(133), PyRep::Integer(250)]).await.expect("dst fuel blocks");
    rpc(&ctx, &mut sess, "invbroker", "Add", &[PyRep::Integer(dst_oz), PyRep::Integer(dst_id), PyRep::Integer(133), PyRep::Integer(400)]).await.expect("dst ozone");
    assert_eq!(observe(&ctx, &mut sess, dst_id).await.unwrap().fuel_blocks, 250, "Ozone must not inflate fuel_blocks");
    rpc(&ctx, &mut sess, "structureDocking", "Undock", &[]).await.expect("Undock");
    let _ = ctx.drain_notifications_for_char(OWNER);
    rpc(&ctx, &mut sess, "structureDeployment", "Unanchor", &[PyRep::Integer(dst_id)]).await.expect("Unanchor");
    assert!(!ctx.drain_notifications_for_char(OWNER).iter().any(|n| n.event == "OnStructureRemoved"));
    ctx.advance_virtual_clock(7 * 86_400 + 5);
    assert!(observe(&ctx, &mut sess, dst_id).await.is_none());
    let unanchor_notifs = ctx.drain_notifications_for_char(OWNER);
    assert!(unanchor_notifs.iter().any(|n| n.event == "OnStructureRemoved") && unanchor_notifs.iter().any(|n| n.event == "OnStructureRemovedFromSpace"));
    let jita_sys = ctx.systems.get_or_spawn(JITA);
    assert!(jita_sys.get_ball_snapshot(dst_id).await.unwrap().is_none());
    ctx.db.with_snapshot(|snap| {
        let ext = load_ext_state(snap);
        let dst_wrap = ext.asset_safety_wraps.values().find(|w| w.is_corp && w.source_structure_id == dst_id).expect("dst unanchor corp wrap");
        assert!(dst_wrap.item_ids.iter().any(|id| snap.items.get(id).is_some_and(|i| i.type_id == 4247 && i.quantity == 250)), "Unanchor must wrap remaining Fuel Blocks into corp Asset Safety");
        assert!(dst_wrap.item_ids.iter().any(|id| snap.items.get(id).is_some_and(|i| i.type_id == 16273 && i.quantity == 400)), "Unanchor must wrap Liquid Ozone into corp Asset Safety");
    });

    // 6. Abandoned K-space structure destruction via live weapon combat across all 3 layers (Shield -> Armor -> Hull -> Destroyed), dropping SpecializedFuelBay fuel & reagents in space.
    ctx.advance_virtual_clock(life::ABANDON_AFTER_LOW_POWER_SECS + 60);
    let _ = observe(&ctx, &mut sess, src_id).await;
    move_ship(&ctx, &sess, pos_jita).await;
    let src_fb_in = grant(&ctx, OWNER, dst_ship, 4247, InventoryFlag::Cargo, 180);
    let src_gas_in = grant(&ctx, OWNER, dst_ship, 81143, InventoryFlag::Cargo, 320);
    rpc(&ctx, &mut sess, "invbroker", "Add", &[PyRep::Integer(src_fb_in), PyRep::Integer(src_id), PyRep::Integer(133), PyRep::Integer(180)]).await.expect("src fuel blocks");
    rpc(&ctx, &mut sess, "invbroker", "Add", &[PyRep::Integer(src_gas_in), PyRep::Integer(src_id), PyRep::Integer(133), PyRep::Integer(320)]).await.expect("src magmatic gas");
    let abandoned_hangar = grant(&ctx, OWNER, src_id, 35, InventoryFlag::Hangar, 7_777);
    let abandoned_rig = grant(&ctx, OWNER, src_id, 37146, InventoryFlag::RigSlot0, 1);
    let abandoned_hi = grant(&ctx, OWNER, src_id, 3170, InventoryFlag::HiSlot0, 1);
    prepare_siege(&ctx, src_id, 200_000.0);
    ctx.db.with_mut_snapshot(|snap| {
        let s = snap.structures.get_mut(&src_id).unwrap();
        s.shield_hp = 10.0; s.armor_hp = 10.0; s.hull_hp = 10.0;
    });
    fire_weapon(&ctx, &mut sess, src_id).await.expect("abandoned shield -> armor");
    assert_eq!(ctx.db.with_snapshot(|s| s.structures[&src_id].state.clone()), life::STATE_ARMOR_VULNERABLE);
    fire_weapon(&ctx, &mut sess, src_id).await.expect("abandoned armor -> hull");
    assert_eq!(ctx.db.with_snapshot(|s| s.structures[&src_id].state.clone()), life::STATE_HULL_VULNERABLE);
    fire_weapon(&ctx, &mut sess, src_id).await.expect("abandoned hull -> destroyed");
    assert_eq!(ctx.db.with_snapshot(|s| s.structures[&src_id].state.clone()), life::STATE_DESTROYED);
    assert!(jita_sys.get_ball_snapshot(src_id).await.unwrap().is_none());
    assert!(jita_sys.get_ball_snapshot(abandoned_hangar).await.unwrap().is_some());
    assert!(jita_sys.get_ball_snapshot(abandoned_hi).await.unwrap().is_some());
    let (dropped_fb_id, dropped_gas_id) = ctx.db.with_snapshot(|snap| {
        let ext = load_ext_state(snap);
        assert!(!ext.asset_safety_wraps.values().any(|w| w.source_structure_id == src_id && w.item_ids.contains(&abandoned_hangar)));
        assert!(!snap.items.contains_key(&abandoned_rig));
        assert_eq!(snap.items.get(&abandoned_hangar).map(|i| i.location_id), Some(i64::from(JITA)));
        let fb = snap.items.values().find(|i| i.location_id == i64::from(JITA) && i.type_id == 4247 && i.quantity == 180).expect("dropped fuel blocks").item_id;
        let gas = snap.items.values().find(|i| i.location_id == i64::from(JITA) && i.type_id == 81143 && i.quantity == 320).expect("dropped magmatic gas").item_id;
        (fb, gas)
    });
    assert!(jita_sys.get_ball_snapshot(dropped_fb_id).await.unwrap().is_some());
    assert!(jita_sys.get_ball_snapshot(dropped_gas_id).await.unwrap().is_some());

    // Destroyed structure rejects management, control, boarding, and manual asset safety RPCs.
    ctx.db.with_mut_snapshot(|snap| snap.structures.get_mut(&src_id).unwrap().owner_corp_id = corp_id);
    assert_eq!(rpc(&ctx, &mut sess, "structureAssetSafety", "MovePersonalAssetsToSafety", &[PyRep::Integer(i64::from(JITA)), PyRep::Integer(src_id)]).await.expect_err("destroyed safety").code, "StructureDestroyed");
    assert_eq!(rpc(&ctx, &mut sess, "structureAssetSafety", "MoveCorpAssetsToSafety", &[PyRep::Integer(i64::from(JITA)), PyRep::Integer(src_id)]).await.expect_err("destroyed corp safety").code, "StructureDestroyed");
    assert_eq!(rpc(&ctx, &mut sess, "structureControl", "TakeControl", &[PyRep::Integer(src_id)]).await.expect_err("destroyed take control").code, "StructureDestroyed");
    assert_eq!(rpc(&ctx, &mut sess, "structureControl", "BoardStructure", &[PyRep::Integer(src_id)]).await.expect_err("destroyed board").code, "StructureDestroyed");
    assert_eq!(rpc(&ctx, &mut sess, "structureDirectory", "SetStructureDescription", &[PyRep::Integer(src_id), PyRep::String("Dead".into())]).await.expect_err("destroyed desc").code, "StructureDestroyed");

    // 7. Lowsec transit recovery charges 15% fee.
    let lowsec_st = ctx.sde.stations.values().find(|s| ctx.sde.solar_systems.get(&s.solar_system_id).is_some_and(|sy| sy.security_status < 0.45 && sy.security_status > 0.0)).map(|s| s.station_id).unwrap();
    let lowsec_wrap_id = grant(&ctx, OWNER, lowsec_st, 34, InventoryFlag::Hangar, 10_000);
    let lowsec_val = ctx.sde.types.get(&34).unwrap().base_price * 10_000.0;
    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        ext.asset_safety_wraps.insert(lowsec_wrap_id, eve_server::services::station_structure::structure_upwell_ops::AssetSafetyWrapRecord {
            wrap_id: lowsec_wrap_id, owner_id: OWNER, is_corp: false, source_structure_id: src_id, source_structure_name: "Source".into(),
            solar_system_id: JITA, created_unix: ctx.now_unix(), deliverable_after_unix: 0, delivered_to_id: None, estimated_value_isk: lowsec_val,
            item_ids: vec![lowsec_wrap_id], nearest_npc_station_id: Some(lowsec_st), auto_delivered: false, retrieval_fee_paid: false,
        });
        save_ext_state(snap, &ext);
    });
    let lowsec_deliv = rpc(&ctx, &mut sess, "structureAssetSafety", "MoveSafetyWrapToStructure", &[PyRep::Integer(lowsec_wrap_id), PyRep::Integer(lowsec_st)]).await.expect("lowsec transit");
    assert!((lowsec_deliv.dict_get("feePaidISK").and_then(|v| v.as_f64()).unwrap_or(-1.0) - lowsec_val * 0.15).abs() < 1.0);
}

#[tokio::test]
async fn test_upwell_reinforcement_durations_exit_hack_and_repair() {
    // parity: feat:structures.armor-reinforce-durations, feat:structures.hull-reinforce-durations, feat:structures.reinforce-exit-time-rules, feat:structures.reinforce-hour-hack-reveal, feat:structures.repair-timer-pause-threshold, feat:structures.vulnerability-repair-window
    eve_testkit::attest(&[
        "feat:structures.armor-reinforce-durations", "feat:structures.hull-reinforce-durations",
        "feat:structures.reinforce-exit-time-rules", "feat:structures.reinforce-hour-hack-reveal",
        "feat:structures.repair-timer-pause-threshold", "feat:structures.vulnerability-repair-window",
    ]);

    let ctx = new_test_ctx();
    let mut sess = pilot_in_space(&ctx, 7103).await;
    let pos4 = open_space(&ctx, JITA, 4.0);
    move_ship(&ctx, &sess, pos4).await;
    let st_id = online_structure(&ctx, &mut sess, 35832, "Reinforce Test Astrahus").await;
    rpc(&ctx, &mut sess, "structureDocking", "Undock", &[]).await.expect("Undock");
    move_ship(&ctx, &sess, pos4).await;

    // 1. Armor & Hull reinforcement duration tables & jitter window rules.
    use life::{armor_reinforcement_secs as a_secs, hull_reinforcement_secs as h_secs, HullClass::*, PowerState::*, SpaceClass::*};
    assert_eq!(a_secs(Medium, HighSec, FullPower), Some(108 * 3600)); assert_eq!(a_secs(Medium, LowNullSec, FullPower), Some(84 * 3600)); assert_eq!(a_secs(Medium, Wormhole, FullPower), Some(60 * 3600));
    assert_eq!(a_secs(Medium, WarHq, FullPower), Some(24 * 3600)); assert_eq!(a_secs(Large, HighSec, FullPower), Some(24 * 3600)); assert_eq!(a_secs(XLarge, LowNullSec, FullPower), Some(24 * 3600));
    assert_eq!(a_secs(Large, HighSec, LowPower), None); assert_eq!(a_secs(Flex, LowNullSec, FullPower), None); assert_eq!(a_secs(Medium, HighSec, Abandoned), None);
    assert_eq!(h_secs(Medium, HighSec, FullPower), None); assert_eq!(h_secs(Large, HighSec, FullPower), Some(108 * 3600)); assert_eq!(h_secs(Large, LowNullSec, FullPower), Some(60 * 3600));
    assert_eq!(h_secs(XLarge, Wormhole, FullPower), Some(36 * 3600)); assert_eq!(h_secs(XLarge, WarHq, FullPower), Some(24 * 3600)); assert_eq!(h_secs(Flex, LowNullSec, FullPower), Some(1800)); assert_eq!(h_secs(Large, HighSec, Abandoned), None);
    assert_eq!(life::reinforce_jitter_window_secs(Medium, true), 5_400); assert_eq!(life::reinforce_jitter_window_secs(Large, true), 10_800);
    assert_eq!(life::REPAIR_HULL_SECS, 30 * 60, "HullVulnerable repair window must be 30 minutes (1800s)");

    // 2. Post-resist repair timer pause threshold (10% of layer damage cap) & 15m shield / 30m hull repair (including Abandoned structures) via live dogmaIM.Activate.
    // Astrahus shield pause threshold = 500 DPS, EM resonance = 0.8 (20% resist), 200mm AutoCannon II cycle = 1.875s / dmg_mult:
    prepare_siege(&ctx, st_id, 0.0);
    fire_weapon(&ctx, &mut sess, st_id).await.expect("light hit");
    let rep_after_light = ctx.db.with_snapshot(|s| load_ext_state(s).lifecycle.get(&st_id).and_then(|l| l.repair_until_unix)).expect("repair timer started");
    ctx.advance_virtual_clock(5);
    prepare_siege(&ctx, st_id, 20_000.0);
    fire_weapon(&ctx, &mut sess, st_id).await.expect("qualifying heavy hit");
    let rep_after_heavy = ctx.db.with_snapshot(|s| load_ext_state(s).lifecycle.get(&st_id).and_then(|l| l.repair_until_unix)).expect("repair timer paused");
    assert!(rep_after_heavy > rep_after_light, "qualifying post-resist DPS must advance repair_until_unix");
    ctx.advance_virtual_clock(life::REPAIR_SHIELD_ARMOR_SECS + 30);
    let prof = life::structure_profile(&ctx, 35832).unwrap();
    assert!((observe(&ctx, &mut sess, st_id).await.unwrap().shield_hp - prof.shield_hp).abs() < 1.0);

    // Abandoned structure also pauses its repair timer on qualifying DPS and heals after uncontested 30m hull window:
    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        ext.lifecycle.entry(st_id).or_default().low_power_since_unix = Some(1);
        save_ext_state(snap, &ext);
        let s = snap.structures.get_mut(&st_id).unwrap();
        s.shield_hp = 10.0; s.armor_hp = 10.0; s.hull_hp = 50_000.0;
    });
    fire_weapon(&ctx, &mut sess, st_id).await.expect("abandoned shield -> armor");
    fire_weapon(&ctx, &mut sess, st_id).await.expect("abandoned armor -> hull");
    let ab_hull_rep0 = ctx.db.with_snapshot(|s| load_ext_state(s).lifecycle.get(&st_id).and_then(|l| l.repair_until_unix)).expect("abandoned hull repair timer");
    ctx.advance_virtual_clock(10);
    fire_weapon(&ctx, &mut sess, st_id).await.expect("pause abandoned hull repair timer");
    let ab_hull_rep1 = ctx.db.with_snapshot(|s| load_ext_state(s).lifecycle.get(&st_id).and_then(|l| l.repair_until_unix)).unwrap();
    assert!(ab_hull_rep1 > ab_hull_rep0, "Abandoned HullVulnerable repair timer must pause on qualifying DPS");
    ctx.advance_virtual_clock(life::REPAIR_HULL_SECS + 5);
    let ab_healed = observe(&ctx, &mut sess, st_id).await.unwrap();
    assert_eq!(ab_healed.state, life::STATE_SHIELD_VULNERABLE, "Abandoned HullVulnerable heals after 30m");
    assert!((ab_healed.hull_hp - prof.hull_hp).abs() < 1.0, "Abandoned structure hull fully restored");
    assert!((ab_healed.armor_hp - prof.armor_hp).abs() < 1.0, "Abandoned structure armor fully restored");
    assert!((ab_healed.shield_hp - prof.shield_hp).abs() < 1.0, "Abandoned structure shield fully restored");
    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        ext.lifecycle.entry(st_id).or_default().low_power_since_unix = None;
        save_ext_state(snap, &ext);
    });

    // 3. Multi-timer High-Sec FullPower Fortizar (1.0d + 4.5d anchored to shield_fight_started_unix) + 30-minute HullVulnerable repair window via live RPCs.
    let my_corp = ctx.db.with_snapshot(|snap| snap.characters.get(&OWNER).unwrap().corporation_id);
    ctx.db.with_mut_snapshot(|snap| snap.structures.get_mut(&st_id).unwrap().owner_corp_id = my_corp);
    let pos_fort = open_space(&ctx, JITA, 6.0);
    move_ship(&ctx, &sess, pos_fort).await;
    let fort_id = online_structure(&ctx, &mut sess, 35833, "MultiTimer Fortizar").await;
    let ship_id = sess.ship_id.unwrap();
    let fuel_item = grant(&ctx, OWNER, ship_id, 4247, InventoryFlag::Cargo, 25_000);
    let svc_item = grant(&ctx, OWNER, ship_id, 35894, InventoryFlag::Cargo, 1);
    rpc(&ctx, &mut sess, "invbroker", "Add", &[PyRep::Integer(fuel_item), PyRep::Integer(fort_id), PyRep::Integer(133), PyRep::Integer(25_000)]).await.expect("fuel fort");
    let mut kw_on = HashMap::new();
    kw_on.insert("online".into(), PyRep::Bool(true));
    ctx.dispatch_rpc(&mut sess, "invbroker", None, "Add", &[PyRep::Integer(svc_item), PyRep::Integer(fort_id), PyRep::Integer(InventoryFlag::ServiceSlot0 as i64)], &kw_on, None).await.expect("online service");
    rpc(&ctx, &mut sess, "structureDocking", "Undock", &[]).await.expect("Undock fort");
    move_ship(&ctx, &sess, pos_fort).await;

    // Align clock to exact 00:00 UTC with reinforce_hour = 18 so armor unjittered exit = t0 + 42h (18:00 Day 1)
    // and multi-timer hull unjittered exit = next_reinforce_exit(t0 + (24 + 108)*3600, 18) = t0 + 138h (18:00 Day 5, NOT Day 6!).
    let cur = ctx.now_unix();
    let next_midnight = ((cur / 86_400) + 1) * 86_400;
    ctx.advance_virtual_clock(next_midnight - cur);
    let t0 = ctx.now_unix();
    let fort_prof = life::structure_profile(&ctx, 35833).unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        snap.structure_vulnerability_windows.insert(fort_id, (18, 3));
        let s = snap.structures.get_mut(&fort_id).unwrap();
        s.shield_hp = 10.0; s.armor_hp = 50_000.0; s.hull_hp = fort_prof.hull_hp;
    });
    prepare_siege(&ctx, fort_id, 20_000.0);
    fire_weapon(&ctx, &mut sess, fort_id).await.expect("strip fort shield");
    let info_ar = rpc(&ctx, &mut sess, "structureDirectory", "GetStructureInfo", &[PyRep::Integer(fort_id)]).await.expect("info ar");
    assert_eq!(kv_str(&info_ar, "state"), life::STATE_ARMOR_REINFORCED);
    let ar_exit = observe(&ctx, &mut sess, fort_id).await.unwrap().reinforced_until_unix;
    let exp_ar_base = t0 + 42 * 3_600;
    assert!((ar_exit as i64 - exp_ar_base as i64).abs() <= 10_800, "Fortizar armor exit within ±3h of Day 1 18:00");

    // Advance into ArmorVulnerable, pause armor repair timer with qualifying damage, then strip armor -> HullReinforced.
    ctx.advance_virtual_clock((ar_exit + 5).saturating_sub(ctx.now_unix()));
    assert_eq!(kv_str(&rpc(&ctx, &mut sess, "structureDirectory", "GetStructureInfo", &[PyRep::Integer(fort_id)]).await.unwrap(), "state"), life::STATE_ARMOR_VULNERABLE);
    fire_weapon(&ctx, &mut sess, fort_id).await.expect("pause armor repair timer");
    ctx.advance_virtual_clock(15);
    ctx.db.with_mut_snapshot(|snap| snap.structures.get_mut(&fort_id).unwrap().armor_hp = 10.0);
    fire_weapon(&ctx, &mut sess, fort_id).await.expect("strip fort armor");
    assert_eq!(kv_str(&rpc(&ctx, &mut sess, "structureDirectory", "GetStructureInfo", &[PyRep::Integer(fort_id)]).await.unwrap(), "state"), life::STATE_HULL_REINFORCED);
    let hu_exit = observe(&ctx, &mut sess, fort_id).await.unwrap().reinforced_until_unix;
    let exp_hu_base = life::next_reinforce_exit(t0 + (24 + 108) * 3_600, 18);
    assert_eq!(exp_hu_base, t0 + 138 * 3_600);
    assert!((hu_exit as i64 - exp_hu_base as i64).abs() <= 10_800, "Multi-timer hull exit must anchor to t0 + 24h + 108h (Day 5 18:00), not armor exit + 108h");

    // Advance into HullVulnerable and verify exact 30-minute (1800s) repair window via GetStructureInfo.
    ctx.advance_virtual_clock(hu_exit.saturating_sub(ctx.now_unix()));
    assert_eq!(kv_str(&rpc(&ctx, &mut sess, "structureDirectory", "GetStructureInfo", &[PyRep::Integer(fort_id)]).await.unwrap(), "state"), life::STATE_HULL_VULNERABLE);
    ctx.advance_virtual_clock(1_799);
    assert_eq!(kv_str(&rpc(&ctx, &mut sess, "structureDirectory", "GetStructureInfo", &[PyRep::Integer(fort_id)]).await.unwrap(), "state"), life::STATE_HULL_VULNERABLE, "at 1799s structure must still be in HullVulnerable");
    ctx.advance_virtual_clock(1);
    assert_eq!(kv_str(&rpc(&ctx, &mut sess, "structureDirectory", "GetStructureInfo", &[PyRep::Integer(fort_id)]).await.unwrap(), "state"), life::STATE_SHIELD_VULNERABLE, "at 1800s HullVulnerable 30m repair completes to ShieldVulnerable");
    let healed_fort = observe(&ctx, &mut sess, fort_id).await.unwrap();
    assert!(healed_fort.shield_hp >= fort_prof.shield_hp && (healed_fort.armor_hp - fort_prof.armor_hp).abs() < 1.0 && (healed_fort.hull_hp - fort_prof.hull_hp).abs() < 1.0);

    // 4. FLEX negative-jitter clamp: Pharolux Cyno Beacon (35840) reinforced 25m before reinforce_hour never gets reinforced_until_unix <= now.
    let flex_day = ((ctx.now_unix() / 86_400) + 1) * 86_400;
    let target_t = flex_day + 17 * 3_600 + 35 * 60;
    ctx.advance_virtual_clock(target_t - ctx.now_unix());
    sess.solar_system_id = Some(RANCER);
    ctx.db.with_mut_snapshot(|snap| snap.characters.get_mut(&OWNER).unwrap().solar_system_id = RANCER);
    let rancer_pos = open_space(&ctx, RANCER, 1.0);
    let rancer_sys = ctx.systems.get_or_spawn(RANCER);
    let _ = rancer_sys.spawn_ball_with_combat(eve_destiny::Ball { id: ship_id, type_id: 587, position: rancer_pos, velocity: DVec3::ZERO, radius: 40.0, mass: 1.0e6, mode: eve_destiny::MovementMode::Stop, speed_fraction: 0.0, max_velocity: 300.0, inertia: 1.0 }, ctx.build_type_combat_state(587)).await;
    let flex_id = kv_i64(&anchor(&ctx, &mut sess, 35840, "Rancer Flex Beacon").await.expect("anchor flex"), "structureID");
    ctx.db.with_mut_snapshot(|snap| {
        snap.structure_vulnerability_windows.insert(flex_id, (18, 3));
        let f = snap.structures.get_mut(&flex_id).unwrap();
        f.state = life::STATE_SHIELD_VULNERABLE.into(); f.quantum_core_installed = true; f.fuel_blocks = 2_000;
        f.shield_hp = 10.0; f.armor_hp = 10.0; f.hull_hp = 50_000.0; f.reinforced_until_unix = 0;
    });
    let flex_svc = grant(&ctx, OWNER, flex_id, 35894, InventoryFlag::ServiceSlot0, 1);
    ctx.db.with_mut_snapshot(|snap| snap.module_online_states.insert(flex_svc, true));
    prepare_siege(&ctx, flex_id, 20_000.0);
    let flex_now = ctx.now_unix();
    fire_weapon(&ctx, &mut sess, flex_id).await.expect("flex shield -> armor");
    fire_weapon(&ctx, &mut sess, flex_id).await.expect("flex armor -> hull reinforced");
    let flex_st = observe(&ctx, &mut sess, flex_id).await.unwrap();
    assert_eq!(flex_st.state, life::STATE_HULL_REINFORCED);
    assert!(flex_st.reinforced_until_unix > flex_now, "FLEX negative jitter must never place reinforced_until_unix in the past");

    // Return session to JITA for section 5 Data Analyzer hack reveal test.
    sess.solar_system_id = Some(JITA);
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&OWNER).unwrap().solar_system_id = JITA;
        snap.structures.get_mut(&st_id).unwrap().owner_corp_id = my_corp;
        snap.structure_vulnerability_windows.insert(st_id, (21, 3));
        let mut ext = load_ext_state(snap);
        ext.pending_reinforce_schedule.insert(st_id, (14, 2, 1_850_000_000));
        let prof_rec = ext.access_profiles.entry(st_id).or_default();
        prof_rec.corp_only = false;
        prof_rec.allowed_characters.extend([HACKER, BYSTANDER]);
        save_ext_state(snap, &ext);
    });
    let owner_info = rpc(&ctx, &mut sess, "structureDirectory", "GetStructureInfo", &[PyRep::Integer(st_id)]).await.expect("owner info");
    assert_eq!(owner_info.dict_get("reinforceHour"), Some(&PyRep::Integer(21)));
    assert_eq!(owner_info.dict_get("nextReinforceHour"), Some(&PyRep::Integer(14)));

    let mut hacker_sess = SessionState::new(7104);
    hacker_sess.user_id = Some(2); hacker_sess.character_id = Some(HACKER); hacker_sess.corporation_id = Some(1000009); hacker_sess.solar_system_id = Some(JITA);
    let hacker_ship = grant(&ctx, HACKER, i64::from(JITA), 606, InventoryFlag::None, 1);
    hacker_sess.ship_id = Some(hacker_ship);
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.get(&OWNER).unwrap().clone();
        ch.character_id = HACKER; ch.user_id = 2; ch.name = "Structure Hacker".into(); ch.corporation_id = 1000009; ch.solar_system_id = JITA; ch.station_id = None; ch.structure_id = None; ch.active_ship_id = hacker_ship;
        snap.characters.insert(HACKER, ch);
        let mut cb = snap.characters.get(&OWNER).unwrap().clone();
        cb.character_id = BYSTANDER; cb.user_id = 3; cb.name = "Structure Bystander".into(); cb.corporation_id = 1000009; cb.solar_system_id = JITA; cb.station_id = None; cb.structure_id = None;
        snap.characters.insert(BYSTANDER, cb);
        snap.skills.entry(HACKER).or_default().insert(21718, eve_db::TrainedSkillRecord { type_id: 21718, level: 5, skill_points: 256_000.0 });
    });
    let _ = ctx.db.acquire_online_lock_with_computer_hash(2, 7104, Some(HACKER), None, true, ctx.now_unix());
    let _ = ctx.db.acquire_online_lock_with_computer_hash(3, 7105, Some(BYSTANDER), None, true, ctx.now_unix());
    let st_pos = DVec3::from_array(observe(&ctx, &mut sess, st_id).await.unwrap().position);
    let jita_sys = ctx.systems.get_or_spawn(JITA);
    let _ = jita_sys.spawn_ball(eve_destiny::Ball { id: hacker_ship, type_id: 606, position: st_pos + DVec3::new(1_000.0, 0.0, 0.0), velocity: DVec3::ZERO, radius: 40.0, mass: 1_000_000.0, mode: eve_destiny::MovementMode::Stop, speed_fraction: 0.0, max_velocity: 300.0, inertia: 1.0 }).await;
    let _ = jita_sys.spawn_ball(eve_destiny::Ball { id: st_id, type_id: 35832, position: st_pos, velocity: DVec3::ZERO, radius: 1_000.0, mass: 1.0e9, mode: eve_destiny::MovementMode::Stop, speed_fraction: 0.0, max_velocity: 0.0, inertia: 1.0 }).await;

    move_ship(&ctx, &sess, st_pos + DVec3::new(900.0, 0.0, 0.0)).await;
    let owner_data_mod = grant(&ctx, OWNER, sess.ship_id.unwrap(), 22175, InventoryFlag::MedSlot0, 1);
    assert_eq!(rpc(&ctx, &mut sess, "hackingMgr", "StartNewGameInstance", &[PyRep::Integer(st_id)]).await.expect_err("self hack").code, "CannotHackOwnStructure");
    ctx.db.with_mut_snapshot(|snap| { snap.items.remove(&owner_data_mod); });

    let pre_info = rpc(&ctx, &mut hacker_sess, "structureDirectory", "GetStructureInfo", &[PyRep::Integer(st_id)]).await.expect("pre info");
    assert_eq!(pre_info.dict_get("reinforceHour"), Some(&PyRep::None));
    let relic_mod = grant(&ctx, HACKER, hacker_ship, 22177, InventoryFlag::MedSlot0, 1);
    assert!(matches!(rpc(&ctx, &mut hacker_sess, "hackingMgr", "StartNewGameInstance", &[PyRep::Integer(st_id)]).await.expect_err("relic").code.as_str(), "AnalyzerModuleRequired" | "UserError"));
    ctx.db.with_mut_snapshot(|snap| { snap.items.remove(&relic_mod); });

    let _data_mod = grant(&ctx, HACKER, hacker_ship, 22175, InventoryFlag::MedSlot0, 1);
    async fn run_structure_hack(ctx: &ServerContext, sess: &mut SessionState, st_id: i64) -> PyRep {
        assert_eq!(kv_str(&rpc(ctx, sess, "hackingMgr", "StartNewGameInstance", &[PyRep::Integer(st_id)]).await.expect("start hack"), "status"), "InProgress");
        for _pass in 0..8 {
            for tile in 1..24 {
                for _ in 0..5 {
                    if let Ok(step) = rpc(ctx, sess, "hackingMgr", "ClickedOnTile", &[PyRep::Integer(st_id), PyRep::Integer(tile)]).await {
                        if kv_str(&step, "status") == "Won" { return step; }
                    } else { break; }
                }
            }
        }
        panic!("Data Analyzer hack must complete with Won");
    }
    let won = run_structure_hack(&ctx, &mut hacker_sess, st_id).await;
    assert_eq!(won.dict_get("reinforceHour"), Some(&PyRep::Integer(21)));
    assert_eq!(won.dict_get("nextReinforceHour"), Some(&PyRep::Integer(14)));

    let hacker_evts = ctx.drain_notifications_for_char(HACKER);
    let PyRep::Tuple(hack_tuple) = &hacker_evts.iter().find(|n| n.event == "OnStructureHacked").expect("OnStructureHacked").args[1] else { panic!("tuple") };
    assert_eq!(hack_tuple, &[PyRep::Integer(3), PyRep::Integer(21), PyRep::Integer(2), PyRep::Integer(14), PyRep::Integer(1_850_000_000)]);

    ctx.db.with_mut_snapshot(|snap| { let mut ext = load_ext_state(snap); ext.pending_reinforce_schedule.remove(&st_id); save_ext_state(snap, &ext); });
    let _ = run_structure_hack(&ctx, &mut hacker_sess, st_id).await;
    let hacker_evts2 = ctx.drain_notifications_for_char(HACKER);
    let PyRep::Tuple(nopending_tuple) = &hacker_evts2.iter().find(|n| n.event == "OnStructureHacked").unwrap().args[1] else { panic!("tuple") };
    assert_eq!(nopending_tuple, &[PyRep::Integer(3), PyRep::Integer(21), PyRep::None, PyRep::None, PyRep::None]);

    let mut bystander_sess = SessionState::new(7105);
    bystander_sess.user_id = Some(3); bystander_sess.character_id = Some(BYSTANDER); bystander_sess.corporation_id = Some(1000009); bystander_sess.solar_system_id = Some(JITA);
    let bystander_info = rpc(&ctx, &mut bystander_sess, "structureDirectory", "GetStructureInfo", &[PyRep::Integer(st_id)]).await.expect("bystander info");
    assert_eq!(bystander_info.dict_get("reinforceHour"), Some(&PyRep::None));
    rpc(&ctx, &mut sess, "structureVulnerability", "UpdateReinforceTimesFromClient", &[PyRep::Integer(st_id), PyRep::Integer(19), PyRep::Integer(4)]).await.expect("update schedule");
    assert!(!ctx.drain_notifications_for_char(BYSTANDER).iter().any(|n| n.event == "OnStructureHacked"));
}
