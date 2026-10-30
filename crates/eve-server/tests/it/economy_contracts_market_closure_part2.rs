//! Integration tests for HyperNet Raffle and D-6 Persistence / No-Transient-Attrs closure.
//!
//! Covers:
//! - `feat:wallet.hypernet-relay`, `feat:wallet.hypernet-hypercore-sizing-expiry`,
//!   `rpc:raffleProxy.CreateRaffle`, `rpc:raffleProxy.BuyRandomTickets`, `rpc:raffleProxy.GetActiveHistoricPrices`
//! - `plan:D-6`, `feat:invariant.no-transient-attrs-persisted`

#![forbid(unsafe_code)]

use crate::common;
use eve_db::{InventoryFlag, InventoryItemRecord, IskCents, TrainedSkillRecord, WorldRepository};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use std::collections::HashMap;
use std::sync::Arc;

const JITA_SYSTEM_ID: u32 = 30_000_142;
const JITA_STATION_ID: i64 = 60_003_760;

fn setup_test_context() -> ServerContext {
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    ctx.db.with_mut_snapshot(|snap| {
        let template = snap.characters.values().next().unwrap().clone();
        for cid in 910_004_i64..=910_006_i64 {
            let mut ch = template.clone();
            ch.character_id = cid;
            ch.name = format!("Pilot {cid}");
            ch.corporation_id = 98_000_000 + cid;
            ch.solar_system_id = JITA_SYSTEM_ID;
            ch.station_id = Some(JITA_STATION_ID);
            ch.structure_id = None;
            ch.balance = IskCents::from_isk(100_000_000);
            snap.characters.insert(cid, ch);
        }
    });
    ctx
}

fn make_session(char_id: i64, corp_id: i64, sys_id: u32) -> SessionState {
    let mut s = SessionState::default();
    s.user_id = Some(1);
    s.character_id = Some(char_id);
    s.corporation_id = Some(corp_id);
    s.solar_system_id = Some(sys_id);
    s
}

fn make_station_session(char_id: i64, corp_id: i64, sys_id: u32, station_id: i64) -> SessionState {
    let mut s = make_session(char_id, corp_id, sys_id);
    s.station_id = Some(station_id);
    s
}

fn make_item(item_id: i64, type_id: u32, owner_id: i64, location_id: i64, flag: InventoryFlag, quantity: i32, singleton: bool) -> InventoryItemRecord {
    InventoryItemRecord { item_id, type_id, owner_id, location_id, flag, quantity, singleton, version: 1, ..Default::default() }
}

async fn rpc_call(ctx: &ServerContext, sess: &mut SessionState, service: &str, method: &str, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None).await
}

fn seed_skills(ctx: &ServerContext, char_id: i64, skills: &[(u32, u8)]) {
    ctx.db.with_mut_snapshot(|snap| {
        let entry = snap.skills.entry(char_id).or_default();
        for &(tid, lvl) in skills {
            entry.insert(tid, TrainedSkillRecord { type_id: tid, level: lvl, skill_points: 256_000.0 });
        }
    });
}

// parity: feat:wallet.hypernet-relay, feat:wallet.hypernet-hypercore-sizing-expiry, rpc:raffleProxy.CreateRaffle, rpc:raffleProxy.BuyRandomTickets, rpc:raffleProxy.GetActiveHistoricPrices
#[tokio::test]
async fn test_hypernet_raffle_sizing_rng_and_expiry() {
    let ctx = setup_test_context();
    let creator_id = 910_004_i64;
    let buyer_id = 910_005_i64;
    let corp_id = 98_910_004_i64;
    let mut creator_sess = make_station_session(creator_id, corp_id, JITA_SYSTEM_ID, JITA_STATION_ID);
    let mut buyer_sess = make_station_session(buyer_id, corp_id, JITA_SYSTEM_ID, JITA_STATION_ID);

    let prices = rpc_call(&ctx, &mut creator_sess, "raffleProxy", "GetActiveHistoricPrices", &[PyRep::Integer(587)])
        .await
        .expect("GetActiveHistoricPrices");
    assert!(matches!(prices, PyRep::Tuple(ref t) if t.len() == 2));

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&creator_id) {
            ch.balance = IskCents::from_isk(100_000_000);
        }
        if let Some(ch) = snap.characters.get_mut(&buyer_id) {
            ch.balance = IskCents::from_isk(100_000_000);
        }
        snap.items.insert(730_001, make_item(730_001, 587, creator_id, JITA_STATION_ID, InventoryFlag::Hangar, 1, true));
        snap.items.insert(730_002, make_item(730_002, 52_568, creator_id, JITA_STATION_ID, InventoryFlag::Hangar, 50, false));
        snap.items.insert(730_003, make_item(730_003, 587, creator_id, JITA_STATION_ID, InventoryFlag::Hangar, 1, true));
    });

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&creator_id) {
            ch.station_id = None;
            ch.structure_id = Some(1_000_000_099);
        }
    });
    let mut struct_sess = make_session(creator_id, corp_id, JITA_SYSTEM_ID);
    struct_sess.structure_id = Some(1_000_000_099);
    let err_struct = rpc_call(
        &ctx,
        &mut struct_sess,
        "raffleProxy",
        "CreateRaffle",
        &[PyRep::keyval([
            ("item_id", PyRep::Integer(730_001)),
            ("ticket_count", PyRep::Integer(8)),
            ("ticket_price", PyRep::Real(1_000_000.0)),
        ])],
    )
    .await
    .expect_err("Structure CreateRaffle must fail");
    assert_eq!(err_struct.code, "InvalidRaffleLocation");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&creator_id) {
            ch.station_id = Some(JITA_STATION_ID);
            ch.structure_id = None;
        }
    });

    let raffle1 = rpc_call(
        &ctx,
        &mut creator_sess,
        "raffleProxy",
        "CreateRaffle",
        &[PyRep::keyval([
            ("item_id", PyRep::Integer(730_001)),
            ("ticket_count", PyRep::Integer(8)),
            ("ticket_price", PyRep::Real(1_000_000.0)),
        ])],
    )
    .await
    .expect("CreateRaffle 1");
    let r1_id = raffle1.dict_get("raffle_id").and_then(|v| v.as_i64()).unwrap();

    let buy_res = rpc_call(
        &ctx,
        &mut buyer_sess,
        "raffleProxy",
        "BuyRandomTickets",
        &[PyRep::Integer(r1_id), PyRep::Integer(8)],
    )
    .await
    .expect("BuyRandomTickets 8");
    assert_eq!(buy_res.dict_get("winner_id").and_then(|v| v.as_i64()), Some(buyer_id));

    let raffle2 = rpc_call(
        &ctx,
        &mut creator_sess,
        "raffleProxy",
        "CreateRaffle",
        &[PyRep::keyval([
            ("item_id", PyRep::Integer(730_003)),
            ("ticket_count", PyRep::Integer(8)),
            ("ticket_price", PyRep::Real(1_000_000.0)),
        ])],
    )
    .await
    .expect("CreateRaffle 2");
    let r2_id = raffle2.dict_get("raffle_id").and_then(|v| v.as_i64()).unwrap();
    rpc_call(
        &ctx,
        &mut buyer_sess,
        "raffleProxy",
        "BuyRandomTickets",
        &[PyRep::Integer(r2_id), PyRep::Integer(2)],
    )
    .await
    .expect("BuyRandomTickets 2");
    let bal_before_exp = ctx.db.with_snapshot(|s| s.characters.get(&buyer_id).unwrap().balance);
    ctx.advance_secs(72 * 3_600 + 1);
    let swept = eve_server::services::hypernet_raffle_ops::sweep_expired_raffles(&ctx);
    assert_eq!(swept, 1);
    let bal_after_exp = ctx.db.with_snapshot(|s| s.characters.get(&buyer_id).unwrap().balance);
    assert_eq!(bal_after_exp.0 - bal_before_exp.0, IskCents::from_isk(2_000_000).0);
}

// parity: plan:D-6, feat:invariant.no-transient-attrs-persisted
#[tokio::test]
async fn test_d6_persistence_and_no_transient_attrs_across_restart() {
    let db_path = std::env::temp_dir().join(format!("d6_restart_{}.sqlite", std::process::id()));
    let _ = std::fs::remove_file(&db_path);

    let char_id = 910_006_i64;
    let corp_id = 98_910_006_i64;
    let ship_id = 740_001_i64;
    let bastion_id = 740_002_i64;

    let (ship_dyn_before, mod_dyn_before) = {
        let repo = Arc::new(WorldRepository::open_sqlite(&db_path).expect("open repo 1"));
        let ctx = ServerContext::with_repository(repo);
        let mut sess = make_session(char_id, corp_id, JITA_SYSTEM_ID);

        let template_ch = ServerContext::new()
            .db
            .with_snapshot(|s| s.characters.values().next().unwrap().clone());
        ctx.db.with_mut_snapshot(|snap| {
            let mut ch = template_ch;
            ch.character_id = char_id;
            ch.name = "D6 Pilot".into();
            ch.corporation_id = corp_id;
            ch.solar_system_id = JITA_SYSTEM_ID;
            ch.active_ship_id = ship_id;
            ch.balance = IskCents::from_isk(50_000_000);
            snap.characters.insert(char_id, ch);
            snap.items.insert(
                ship_id,
                make_item(ship_id, 28_661, char_id, JITA_SYSTEM_ID as i64, InventoryFlag::None, 1, true),
            );
            snap.items.insert(
                bastion_id,
                make_item(bastion_id, 33_400, char_id, ship_id, InventoryFlag::HiSlot0, 1, true),
            );
            snap.module_online_states.insert(bastion_id, true);
        });
        seed_skills(&ctx, char_id, &[(3300, 5), (3426, 5)]);

        let _ = ctx.db.try_with_mut_snapshot(|snap| {
            snap.characters.get_mut(&char_id).unwrap().balance = IskCents::from_isk(999_999_999);
            Err::<(), String>("forced rollback".into())
        });
        let bal_after_rb = ctx.db.with_snapshot(|s| s.characters.get(&char_id).unwrap().balance);
        assert_eq!(bal_after_rb, IskCents::from_isk(50_000_000));

        let (ship_dyn_before, mod_dyn_before) = ctx.db.with_snapshot(|s| {
            (
                s.items.get(&ship_id).unwrap().dynamic_attributes.clone(),
                s.items.get(&bastion_id).unwrap().dynamic_attributes.clone(),
            )
        });

        rpc_call(
            &ctx,
            &mut sess,
            "dogmaIM",
            "Activate",
            &[PyRep::Integer(bastion_id), PyRep::String("moduleBonusBastion".into())],
        )
        .await
        .expect("Activate Bastion");
        assert!(ctx.db.with_snapshot(|s| s.module_active_states.contains(&bastion_id)));
        let mut active_attrs = std::collections::HashMap::new();
        eve_server::services::tactical_modules::apply_tactical_module_ship_attributes(
            &ctx,
            ship_id,
            &mut active_attrs,
        );
        assert_eq!(active_attrs.get(&2135).copied(), Some(0.05));
        assert_eq!(active_attrs.get(&2112).copied(), Some(1.0));
        assert_eq!(active_attrs.get(&2113).copied(), Some(1.0));
        assert_eq!(active_attrs.get(&2115).copied(), Some(1.0));
        (ship_dyn_before, mod_dyn_before)
    };

    let repo2 = Arc::new(WorldRepository::open_sqlite(&db_path).expect("open repo 2"));
    let ctx2 = ServerContext::with_repository(repo2);
    ctx2.db.with_snapshot(|snap| {
        assert_eq!(snap.characters.get(&char_id).unwrap().balance, IskCents::from_isk(50_000_000));
        assert!(
            !snap.module_active_states.contains(&bastion_id),
            "Active module cycle state must not persist across server restart"
        );
        assert!(
            snap.module_online_states.get(&bastion_id).copied().unwrap_or(false),
            "Module online state must persist across server restart"
        );
        assert_eq!(
            snap.items.get(&ship_id).unwrap().dynamic_attributes,
            ship_dyn_before,
            "Ship dynamic_attributes must equal un-buffed fitted values after restart"
        );
        assert_eq!(
            snap.items.get(&bastion_id).unwrap().dynamic_attributes,
            mod_dyn_before,
            "Module dynamic_attributes must equal un-buffed fitted values after restart"
        );
    });
    let mut post_restart_attrs = std::collections::HashMap::new();
    eve_server::services::tactical_modules::apply_tactical_module_ship_attributes(
        &ctx2,
        ship_id,
        &mut post_restart_attrs,
    );
    assert!(
        post_restart_attrs.get(&2135).is_none() && post_restart_attrs.get(&2112).is_none(),
        "Bastion resonance/bonus attributes must not persist across server restart"
    );
    let _ = std::fs::remove_file(&db_path);
}
