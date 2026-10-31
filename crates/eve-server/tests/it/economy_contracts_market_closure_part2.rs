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
    let rah_id = 740_003_i64;
    let mutadaptive_id = 740_004_i64;
    let target_ship_id = 740_005_i64;
    let dread_id = 740_006_i64;
    let siege_id = 740_007_i64;
    let ehe_id = 740_008_i64;
    let mutated_ar_id = 740_010_i64;
    let mutated_dcu_id = 740_011_i64;
    let fax_id = 740_012_i64;
    let triage_id = 740_013_i64;
    let weapon_id = 740_015_i64;

    let (ship_dyn_before, rah_dyn_before, mut_dyn_before, dread_dyn_before, fax_dyn_before, mutated_ar_dyn_before, mutated_dcu_dyn_before) = {
        let repo = Arc::new(WorldRepository::open_sqlite(&db_path).expect("open repo 1"));
        let ctx = ServerContext::with_repository(repo);
        let mut sess = make_session(char_id, corp_id, JITA_SYSTEM_ID);
        let mutadaptive_tid = ctx
            .sde
            .types
            .values()
            .find(|t| t.group_id == 2018 && t.published)
            .map(|t| t.type_id)
            .unwrap_or(48_636);

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
            snap.items.insert(
                rah_id,
                make_item(rah_id, 4_403, char_id, ship_id, InventoryFlag::LoSlot0, 1, true),
            );
            snap.items.insert(
                mutadaptive_id,
                make_item(mutadaptive_id, mutadaptive_tid, char_id, ship_id, InventoryFlag::HiSlot1, 1, true),
            );
            snap.items.insert(
                target_ship_id,
                make_item(target_ship_id, 638, char_id + 1, JITA_SYSTEM_ID as i64, InventoryFlag::None, 1, true),
            );
            snap.items.insert(
                dread_id,
                make_item(dread_id, 19_720, char_id, JITA_SYSTEM_ID as i64, InventoryFlag::None, 1, true),
            );
            snap.items.insert(
                siege_id,
                make_item(siege_id, 20_280, char_id, dread_id, InventoryFlag::HiSlot0, 1, true),
            );
            snap.items.insert(
                ehe_id,
                make_item(ehe_id, 41_485, char_id, dread_id, InventoryFlag::LoSlot0, 1, true),
            );
            snap.items.insert(
                740_009,
                make_item(740_009, 16_275, char_id, dread_id, InventoryFlag::Cargo, 500, false),
            );
            let mut mutated_ar = make_item(mutated_ar_id, 3_530, char_id, ship_id, InventoryFlag::LoSlot1, 1, true);
            mutated_ar.source_mutaplasmid_id = Some(47_732);
            mutated_ar.dynamic_attributes.insert(84, 312.5);
            snap.items.insert(mutated_ar_id, mutated_ar);
            let mut mutated_dcu = make_item(mutated_dcu_id, 2_048, char_id, ship_id, InventoryFlag::LoSlot2, 1, true);
            mutated_dcu.source_mutaplasmid_id = Some(47_736);
            for aid in 267..=270 {
                mutated_dcu.dynamic_attributes.insert(aid, 0.81);
            }
            for aid in 271..=274 {
                mutated_dcu.dynamic_attributes.insert(aid, 0.84);
            }
            snap.items.insert(mutated_dcu_id, mutated_dcu);
            snap.items.insert(
                fax_id,
                make_item(fax_id, 37_604, char_id, JITA_SYSTEM_ID as i64, InventoryFlag::None, 1, true),
            );
            snap.items.insert(
                triage_id,
                make_item(triage_id, 27_951, char_id, fax_id, InventoryFlag::HiSlot0, 1, true),
            );
            snap.items.insert(
                740_014,
                make_item(740_014, 16_275, char_id, fax_id, InventoryFlag::Cargo, 500, false),
            );
            snap.items.insert(
                weapon_id,
                make_item(weapon_id, 3_041, char_id, ship_id, InventoryFlag::HiSlot5, 1, true),
            );
            for mid in [bastion_id, rah_id, mutadaptive_id, siege_id, ehe_id, mutated_ar_id, mutated_dcu_id, triage_id, weapon_id] {
                snap.module_online_states.insert(mid, true);
            }
        });
        seed_skills(&ctx, char_id, &[(3300, 5), (3426, 5), (28_164, 5)]);

        let _ = ctx.db.try_with_mut_snapshot(|snap| {
            snap.characters.get_mut(&char_id).unwrap().balance = IskCents::from_isk(999_999_999);
            Err::<(), String>("forced rollback".into())
        });
        let bal_after_rb = ctx.db.with_snapshot(|s| s.characters.get(&char_id).unwrap().balance);
        assert_eq!(bal_after_rb, IskCents::from_isk(50_000_000));

        let (ship_dyn_before, rah_dyn_before, mut_dyn_before, dread_dyn_before, fax_dyn_before, mutated_ar_dyn_before, mutated_dcu_dyn_before) = ctx.db.with_snapshot(|s| {
            (
                s.items.get(&ship_id).unwrap().dynamic_attributes.clone(),
                s.items.get(&rah_id).unwrap().dynamic_attributes.clone(),
                s.items.get(&mutadaptive_id).unwrap().dynamic_attributes.clone(),
                s.items.get(&dread_id).unwrap().dynamic_attributes.clone(),
                s.items.get(&fax_id).unwrap().dynamic_attributes.clone(),
                s.items.get(&mutated_ar_id).unwrap().dynamic_attributes.clone(),
                s.items.get(&mutated_dcu_id).unwrap().dynamic_attributes.clone(),
            )
        });

        let sys_h = ctx.systems.get_or_spawn(JITA_SYSTEM_ID);
        for (bid, tid, oid, x) in [
            (ship_id, 28_661_u32, char_id, 0.0_f64),
            (target_ship_id, 638_u32, char_id + 1, 2_000.0_f64),
            (dread_id, 19_720_u32, char_id, 5_000.0_f64),
            (fax_id, 37_604_u32, char_id, 8_000.0_f64),
        ] {
            let cs = ctx.build_type_combat_state(tid);
            let b = eve_destiny::Ball {
                id: bid,
                type_id: tid,
                position: eve_destiny::DVec3::new(x, 0.0, 0.0),
                velocity: eve_destiny::DVec3::ZERO,
                max_velocity: 150.0,
                inertia: 1.0,
                mass: 10_000_000.0,
                radius: 100.0,
                speed_fraction: 0.0,
                mode: eve_destiny::MovementMode::Stop,
            };
            let meta = eve_destiny::SlimItemMeta {
                group_id: 26,
                category_id: 6,
                owner_id: oid,
                char_id: Some(oid),
                corp_id,
                alliance_id: -1,
                security_status: 0.0,
                is_cloaked: false,
            };
            let _ = sys_h.spawn_ball_with_meta(b, cs, meta).await;
        }
        let _ = sys_h
            .add_target_lock(
                ship_id,
                eve_destiny::TargetLock {
                    target_id: target_ship_id,
                    lock_time_ms: 0.0,
                    remaining_lock_ms: 0.0,
                    locked: true,
                    max_range_m: Some(50_000.0),
                },
            )
            .await;

        rpc_call(
            &ctx,
            &mut sess,
            "dogmaIM",
            "Activate",
            &[PyRep::Integer(bastion_id), PyRep::String("moduleBonusBastion".into())],
        )
        .await
        .expect("Activate Bastion");
        rpc_call(
            &ctx,
            &mut sess,
            "dogmaIM",
            "Activate",
            &[PyRep::Integer(rah_id), PyRep::String("adaptiveArmorHardener".into())],
        )
        .await
        .expect("Activate RAH");
        rpc_call(
            &ctx,
            &mut sess,
            "dogmaIM",
            "Activate",
            &[
                PyRep::Integer(mutadaptive_id),
                PyRep::String("remoteArmorRepair".into()),
                PyRep::Integer(target_ship_id),
            ],
        )
        .await
        .expect("Activate Mutadaptive RAR");

        rpc_call(
            &ctx,
            &mut sess,
            "dogmaIM",
            "Overload",
            &[
                PyRep::Integer(weapon_id),
                PyRep::String("targetAttack".into()),
                PyRep::Integer(target_ship_id),
                PyRep::Integer(1),
            ],
        )
        .await
        .expect("Overload weapon");

        sess.ship_id = Some(dread_id);
        ctx.db.with_mut_snapshot(|snap| {
            snap.characters.get_mut(&char_id).unwrap().active_ship_id = dread_id;
        });
        rpc_call(
            &ctx,
            &mut sess,
            "dogmaIM",
            "Activate",
            &[PyRep::Integer(siege_id), PyRep::String("moduleBonusSiege".into())],
        )
        .await
        .expect("Activate Siege");
        rpc_call(
            &ctx,
            &mut sess,
            "dogmaIM",
            "Activate",
            &[PyRep::Integer(ehe_id), PyRep::String("emergencyHullEnergizer".into())],
        )
        .await
        .expect("Activate EHE/ADC");

        sess.ship_id = Some(fax_id);
        ctx.db.with_mut_snapshot(|snap| {
            snap.characters.get_mut(&char_id).unwrap().active_ship_id = fax_id;
        });
        rpc_call(
            &ctx,
            &mut sess,
            "dogmaIM",
            "Activate",
            &[PyRep::Integer(triage_id), PyRep::String("moduleBonusTriage".into())],
        )
        .await
        .expect("Activate Triage");

        let _ = sys_h
            .apply_damage(ship_id, eve_destiny::DamageVector::new(100.0, 0.0, 0.0, 0.0))
            .await;
        for _ in 0..25 {
            let _ = sys_h.step_manual(1.0).await;
        }

        assert!(ctx.db.with_snapshot(|s| s.module_active_states.contains(&bastion_id)));
        assert!(ctx.db.with_snapshot(|s| s.module_active_states.contains(&siege_id)));
        assert!(ctx.db.with_snapshot(|s| s.module_active_states.contains(&triage_id)));
        ctx.db.with_snapshot(|s| {
            let rah_em = s
                .transient_item_attributes
                .get(&rah_id)
                .and_then(|t| t.get(&267).copied())
                .unwrap_or(0.85);
            assert!(rah_em < 0.85, "RAH EM resonance must shift below 0.85 before crash, got {rah_em}");
            let spooled_rep = s
                .transient_item_attributes
                .get(&mutadaptive_id)
                .and_then(|t| t.get(&84).copied())
                .unwrap_or(0.0);
            assert!(spooled_rep > 0.0, "Mutadaptive RAR spool attr 84 must be populated before crash");
            let hi_heat = s
                .transient_item_attributes
                .get(&ship_id)
                .and_then(|t| t.get(&1177).copied())
                .unwrap_or(0.0);
            assert!(hi_heat > 0.0, "High rack heat attr 1177 must be > 0 before crash, got {hi_heat}");
        });
        let mut active_attrs = std::collections::HashMap::new();
        eve_server::services::tactical_modules::apply_tactical_module_ship_attributes(
            &ctx,
            ship_id,
            &mut active_attrs,
        );
        assert_eq!(active_attrs.get(&2135).copied(), Some(0.05));
        let mut triage_attrs = std::collections::HashMap::new();
        eve_server::services::tactical_modules::apply_tactical_module_ship_attributes(
            &ctx,
            fax_id,
            &mut triage_attrs,
        );
        assert_eq!(triage_attrs.get(&2135).copied(), Some(0.20));
        (
            ship_dyn_before,
            rah_dyn_before,
            mut_dyn_before,
            dread_dyn_before,
            fax_dyn_before,
            mutated_ar_dyn_before,
            mutated_dcu_dyn_before,
        )
    };

    let repo2 = Arc::new(WorldRepository::open_sqlite(&db_path).expect("open repo 2"));
    let ctx2 = ServerContext::with_repository(repo2);
    ctx2.db.with_snapshot(|snap| {
        assert_eq!(snap.characters.get(&char_id).unwrap().balance, IskCents::from_isk(50_000_000));
        assert!(
            snap.module_active_states.is_empty(),
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
            snap.items.get(&rah_id).unwrap().dynamic_attributes,
            rah_dyn_before,
            "RAH dynamic_attributes must equal un-buffed fitted values after restart"
        );
        let mut mut_dyn_after = snap.items.get(&mutadaptive_id).unwrap().dynamic_attributes.clone();
        mut_dyn_after.remove(&3);
        assert_eq!(
            mut_dyn_after,
            mut_dyn_before,
            "Mutadaptive RAR dynamic_attributes (excluding persistent heat damage 3) must equal un-buffed fitted values after restart"
        );
        assert!(
            snap.items.get(&weapon_id).unwrap().dynamic_attributes.get(&3).copied().unwrap_or(0.0) > 0.0,
            "Overloaded weapon heat damage (attr 3) should persist across restart"
        );
        assert_eq!(
            snap.items.get(&dread_id).unwrap().dynamic_attributes,
            dread_dyn_before,
            "Dreadnought dynamic_attributes must equal un-buffed fitted values after restart"
        );
        assert_eq!(
            snap.items.get(&fax_id).unwrap().dynamic_attributes,
            fax_dyn_before,
            "FAX dynamic_attributes must equal un-buffed fitted values after restart"
        );
        assert!(
            snap.transient_item_attributes.is_empty(),
            "Transient item attributes must not persist across server restart"
        );
        let ar_after = snap.items.get(&mutated_ar_id).unwrap();
        assert_eq!(
            ar_after.dynamic_attributes,
            mutated_ar_dyn_before,
            "Mutated Armor Repairer rolled attr 84 must survive SQLite restart"
        );
        assert_eq!(ar_after.source_mutaplasmid_id, Some(47_732));
        let dcu_after = snap.items.get(&mutated_dcu_id).unwrap();
        assert_eq!(
            dcu_after.dynamic_attributes,
            mutated_dcu_dyn_before,
            "Mutated Damage Control rolled attrs 267..=274 must survive SQLite restart"
        );
        assert_eq!(dcu_after.source_mutaplasmid_id, Some(47_736));
    });
    let mut post_restart_attrs = std::collections::HashMap::new();
    eve_server::services::tactical_modules::apply_tactical_module_ship_attributes(
        &ctx2,
        ship_id,
        &mut post_restart_attrs,
    );
    assert!(
        post_restart_attrs.get(&2135).is_none() && post_restart_attrs.get(&2112).is_none(),
        "Bastion/Siege/Triage resonance/bonus attributes must not persist across server restart"
    );
    let mut post_restart_fax_attrs = std::collections::HashMap::new();
    eve_server::services::tactical_modules::apply_tactical_module_ship_attributes(
        &ctx2,
        fax_id,
        &mut post_restart_fax_attrs,
    );
    assert!(
        post_restart_fax_attrs.get(&2135).is_none() && post_restart_fax_attrs.get(&2112).is_none(),
        "Triage attributes must not persist across server restart"
    );
    let _ = std::fs::remove_file(&db_path);
}
