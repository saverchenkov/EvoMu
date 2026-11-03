//! End-to-end closure tests for Insurance voiding & 12-week expiry (`feat:combat.insurance-void-and-expiry`).

#![forbid(unsafe_code)]

use eve_db::{InventoryFlag, InventoryItemRecord, IskCents};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::combat::death_pipeline::execute_death_pipeline;
use eve_server::formulas::{compute_insurance_quote, compute_ship_insurance_reference_value};
use eve_server::router::ServerContext;
use glam::DVec3;
use std::collections::HashMap;
use std::sync::Arc;

async fn insure_new_ship(
    ctx: &ServerContext,
    sess: &mut SessionState,
    char_id: i64,
    station_id: i64,
) -> i64 {
    let sid = ctx.db.with_mut_snapshot(|snap| {
        snap.next_entity_id += 1;
        let id = snap.next_entity_id;
        snap.items.insert(
            id,
            InventoryItemRecord {
                item_id: id,
                type_id: 587,
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        id
    });
    let _ = ctx
        .dispatch_rpc(
            sess,
            "insuranceSvc",
            None,
            "InsureShip",
            &[
                PyRep::Integer(sid),
                PyRep::Real(1.0),
                PyRep::Bool(false),
                PyRep::Bool(true),
            ],
            &HashMap::new(),
            None,
        )
        .await
        .expect("InsureShip");
    sid
}

#[tokio::test]
async fn test_e2e_insurance_void_and_expiry() {
    // parity: feat:combat.insurance-void-and-expiry
    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);
    let char_id = 90_000_001_i64;
    let sys_id = 30_000_142_u32;
    let station_id = 60_003_760_i64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.user_id = 1;
            ch.balance = IskCents::from_f64_isk(500_000_000.0);
            ch.solar_system_id = sys_id;
            ch.station_id = Some(station_id);
        }
    });

    let ref_val = compute_ship_insurance_reference_value(&ctx.sde, 587);
    let (_, basic_40_payout) = compute_insurance_quote(ref_val, 0.40);
    let (_, plat_payout) = compute_insurance_quote(ref_val, 1.0);
    assert!(plat_payout > basic_40_payout);

    let mut sess = SessionState::default();
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.solar_system_id = Some(sys_id);
    sess.station_id = Some(station_id);

    // 1. Active Platinum contract pays out immediately on real ship destruction + sends EVE-mail
    let ship_a = insure_new_ship(&ctx, &mut sess, char_id, station_id).await;
    let bal_before_a = ctx.db.with_snapshot(|s| s.characters[&char_id].balance.to_wire_f64());
    let mails_before_a = ctx.db.with_snapshot(|s| s.mails.len());
    let _ = execute_death_pipeline(
        &ctx,
        sys_id,
        char_id,
        ship_a,
        90_000_002,
        DVec3::ZERO,
        11,
    )
    .await;
    let bal_after_a = ctx.db.with_snapshot(|s| s.characters[&char_id].balance.to_wire_f64());
    assert!((bal_after_a - bal_before_a - plat_payout).abs() < 1.0);
    assert!(ctx.db.with_snapshot(|s| {
        s.mails.len() > mails_before_a
            && s.mails
                .iter()
                .any(|m| m.recipient_id == char_id && m.subject.contains(&format!("Ship {ship_a}")))
    }));

    // 2. Repackaging voids insurance -> falls back to 40% Basic payout on destruction
    let ship_b = insure_new_ship(&ctx, &mut sess, char_id, station_id).await;
    let _ = ctx
        .dispatch_rpc(
            &mut sess,
            "repackagingSvc",
            None,
            "RepackageItems",
            &[PyRep::List(vec![PyRep::Integer(ship_b)])],
            &HashMap::new(),
            None,
        )
        .await
        .expect("RepackageItems");
    let c_b = ctx
        .dispatch_rpc(
            &mut sess,
            "insuranceSvc",
            None,
            "GetContractForShip",
            &[PyRep::Integer(ship_b)],
            &HashMap::new(),
            None,
        )
        .await
        .expect("GetContractForShip after repackage");
    assert!(matches!(c_b, PyRep::None), "Repackaging must void insurance contract");

    let bal_before_b = ctx.db.with_snapshot(|s| s.characters[&char_id].balance.to_wire_f64());
    let _ = execute_death_pipeline(
        &ctx,
        sys_id,
        char_id,
        ship_b,
        90_000_002,
        DVec3::ZERO,
        12,
    )
    .await;
    let bal_after_b = ctx.db.with_snapshot(|s| s.characters[&char_id].balance.to_wire_f64());
    assert!((bal_after_b - bal_before_b - basic_40_payout).abs() < 1.0);

    // 3. Moving ship into Corporation Hangar voids insurance
    let ship_c = insure_new_ship(&ctx, &mut sess, char_id, station_id).await;
    let _ = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            None,
            "Add",
            &[
                PyRep::Integer(ship_c),
                PyRep::Integer(station_id),
                PyRep::Integer(InventoryFlag::CorpSAG1 as i64),
            ],
            &HashMap::new(),
            None,
        )
        .await
        .expect("invbroker.Add to CorpSAG1");
    let c_c = ctx
        .dispatch_rpc(
            &mut sess,
            "insuranceSvc",
            None,
            "GetContractForShip",
            &[PyRep::Integer(ship_c)],
            &HashMap::new(),
            None,
        )
        .await
        .expect("GetContractForShip after corp hangar move");
    assert!(matches!(c_c, PyRep::None), "Moving ship to corp hangar must void insurance");

    // 4. Contracting ship voids insurance
    let ship_d = insure_new_ship(&ctx, &mut sess, char_id, station_id).await;
    let _ = ctx
        .dispatch_rpc(
            &mut sess,
            "contractProxy",
            None,
            "CreateContract",
            &[
                PyRep::Integer(1),
                PyRep::Integer(0),
                PyRep::Integer(0),
                PyRep::Integer(1440),
                PyRep::Integer(1),
                PyRep::Integer(station_id),
                PyRep::Integer(station_id),
                PyRep::Real(50_000.0),
                PyRep::Real(0.0),
                PyRep::Real(0.0),
                PyRep::String("".into()),
                PyRep::String("".into()),
                PyRep::List(vec![PyRep::Integer(ship_d)]),
            ],
            &HashMap::new(),
            None,
        )
        .await
        .expect("CreateContract");
    let c_d = ctx
        .dispatch_rpc(
            &mut sess,
            "insuranceSvc",
            None,
            "GetContractForShip",
            &[PyRep::Integer(ship_d)],
            &HashMap::new(),
            None,
        )
        .await
        .expect("GetContractForShip after contract");
    assert!(matches!(c_d, PyRep::None), "Contracting ship must void insurance");

    // 5. 12-week expiry (84 * 86,400 s) falls back to 40% Basic payout on destruction
    let ship_e = insure_new_ship(&ctx, &mut sess, char_id, station_id).await;
    ctx.advance_secs(12 * 7 * 86_400 + 10);
    let bal_before_e = ctx.db.with_snapshot(|s| s.characters[&char_id].balance.to_wire_f64());
    let _ = execute_death_pipeline(
        &ctx,
        sys_id,
        char_id,
        ship_e,
        90_000_002,
        DVec3::ZERO,
        13,
    )
    .await;
    let bal_after_e = ctx.db.with_snapshot(|s| s.characters[&char_id].balance.to_wire_f64());
    assert!((bal_after_e - bal_before_e - basic_40_payout).abs() < 1.0);

    // 6. CONCORD kill in high-sec as criminal yields 0.0 payout
    let ship_f = insure_new_ship(&ctx, &mut sess, char_id, station_id).await;
    let now_f = ctx.now_unix();
    {
        let mut cw = ctx.crimewatch.write().unwrap();
        let _ = cw.handle_combat_aggression_with_pod(
            char_id,
            90_000_002,
            ship_f,
            sys_id,
            0.95,
            false,
            false,
            now_f,
        );
    }
    let bal_before_f = ctx.db.with_snapshot(|s| s.characters[&char_id].balance.to_wire_f64());
    let _ = execute_death_pipeline(
        &ctx,
        sys_id,
        char_id,
        ship_f,
        1_000_125,
        DVec3::ZERO,
        14,
    )
    .await;
    let bal_after_f = ctx.db.with_snapshot(|s| s.characters[&char_id].balance.to_wire_f64());
    assert!((bal_after_f - bal_before_f).abs() < 1e-6, "CONCORD kill as criminal must yield 0 payout");
}
