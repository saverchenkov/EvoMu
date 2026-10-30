//! Phase 4 Dogma Capital Siege/Triage/Bastion/Industrial Core/Phenomena/ZPME tests (`DG-4`, Part 2).

use crate::common;
use crate::dogma_capital_heat_crystals::*;

use eve_db::InventoryFlag;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::attr_ids;
use eve_server::router::ServerContext;
use std::collections::HashMap;

#[tokio::test]
async fn test_dg4_siege_triage_bastion_industrial_core_phenomena_and_zpme() {
    // parity: feat:modules.siege-module, feat:modules.triage-module, feat:modules.bastion-module, feat:modules.industrial-core, feat:modules.phenomena-generator, feat:modules.zero-point-mass-entangler, rpc:charUnboundMgr.SelectCharacterID
    eve_testkit::attest(&[
        "feat:modules.siege-module",
        "feat:modules.triage-module",
        "feat:modules.bastion-module",
        "feat:modules.industrial-core",
        "feat:modules.phenomena-generator",
        "feat:modules.zero-point-mass-entangler",
    ]);
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut sess = SessionState::new(4203);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_001)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");

    let char_id = 90_000_001i64;
    let ship_id = 100_000_001i64;
    let station_id = 60_003_760i64;

    for tid in [
        19720u32, 20280, 37604, 27951, 28659, 33400, 28352, 28583, 11567, 43658, 12017, 49099, 434,
    ] {
        grant_all_required_skills_for_type(&ctx, char_id, tid);
    }
    ctx.db.with_mut_snapshot(|snap| {
        let entry = snap.skills.entry(char_id).or_default();
        for &(sid, lvl) in &[(3426u32, 5u8), (3413, 5), (3418, 5), (3417, 5)] {
            entry.insert(
                sid,
                eve_db::TrainedSkillRecord {
                    type_id: sid,
                    level: lvl,
                    skill_points: 256_000.0,
                },
            );
        }
    });

    // 1. Siege Module I (20280) on Revelation Dreadnought (19720) (feat:modules.siege-module)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ship_item) = snap.items.get_mut(&ship_id) {
            ship_item.type_id = 19720;
        }
        snap.items.remove(&100_000_005);
    });
    ctx.invalidate_ship_fitting(ship_id);

    let siege_id = 100_002_201i64;
    let stront_id = 100_002_202i64;
    let heavy_water_id = 100_002_203i64;
    seed_item(&ctx, siege_id, 20280, char_id, station_id, InventoryFlag::Hangar, 1, true);
    seed_item(&ctx, stront_id, 16275, char_id, ship_id, InventoryFlag::Cargo, 2_000, false);
    seed_item(&ctx, heavy_water_id, 16272, char_id, ship_id, InventoryFlag::Cargo, 2_000, false);

    ctx.dispatch_rpc(
        &mut sess,
        "invbroker",
        None,
        "Add",
        &[
            PyRep::Integer(siege_id),
            PyRep::Integer(ship_id),
            PyRep::Integer(InventoryFlag::HiSlot0 as i64),
        ],
        &kw,
        None,
    )
    .await
    .expect("Fit Siege Module I");

    ctx.dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock");
    let sys = ctx.systems.get_or_spawn(30_000_142);
    sys.set_capacitor(ship_id, 50_000.0).await.unwrap();

    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Activate",
        &[
            PyRep::Integer(siege_id),
            PyRep::String("siegeModeEffect".into()),
            PyRep::None,
            PyRep::Integer(1),
        ],
        &kw,
        None,
    )
    .await
    .expect("Activate Siege Module I on Revelation");
    let stront_after_siege = ctx
        .db
        .with_snapshot(|s| s.items.get(&stront_id).map(|i| i.quantity).unwrap_or(0));
    assert!(
        stront_after_siege == 1_750 || stront_after_siege == 1_875,
        "Siege Module I must consume Strontium Clathrates on activation (got {stront_after_siege})"
    );
    let (_, ball_sieged) = sys.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    let cs_sieged = sys.get_combat_state(ship_id).await.unwrap().unwrap();
    assert_eq!(
        ball_sieged.max_velocity, 0.0,
        "Siege Module I must immobilize the Dreadnought (max_velocity = 0.0)"
    );
    assert!(
        cs_sieged.disallow_assistance,
        "Siege Module I must set disallow_assistance = true"
    );
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Deactivate",
        &[
            PyRep::Integer(siege_id),
            PyRep::String("siegeModeEffect".into()),
        ],
        &kw,
        None,
    )
    .await
    .expect("Deactivate Siege Module I");

    // 2. Triage Module I (27951) on Apostle Force Auxiliary (37604) (feat:modules.triage-module)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(s) = snap.items.get_mut(&ship_id) {
            s.type_id = 37604;
        }
        if let Some(m) = snap.items.get_mut(&siege_id) {
            m.type_id = 27951;
        }
    });
    ctx.invalidate_ship_fitting(ship_id);
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Activate",
        &[
            PyRep::Integer(siege_id),
            PyRep::String("triageModeEffect".into()),
            PyRep::None,
            PyRep::Integer(1),
        ],
        &kw,
        None,
    )
    .await
    .expect("Activate Triage Module I on Apostle");
    let stront_after_triage = ctx
        .db
        .with_snapshot(|s| s.items.get(&stront_id).map(|i| i.quantity).unwrap_or(0));
    assert!(
        stront_after_triage == 1_500 || stront_after_triage == 1_800,
        "Triage Module I must consume Strontium Clathrates on activation (got {stront_after_triage})"
    );
    let cs_triaged = sys.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!(
        cs_triaged.disallow_assistance,
        "Triage Module I must set disallow_assistance = true"
    );
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Deactivate",
        &[
            PyRep::Integer(siege_id),
            PyRep::String("triageModeEffect".into()),
        ],
        &kw,
        None,
    )
    .await
    .expect("Deactivate Triage Module I");

    // 3. Bastion Module I (33400) on Paladin Marauder (28659) (feat:modules.bastion-module)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(s) = snap.items.get_mut(&ship_id) {
            s.type_id = 28659;
        }
        if let Some(m) = snap.items.get_mut(&siege_id) {
            m.type_id = 33400;
        }
    });
    ctx.invalidate_ship_fitting(ship_id);
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Activate",
        &[
            PyRep::Integer(siege_id),
            PyRep::String("moduleBonusBastion".into()),
            PyRep::None,
            PyRep::Integer(1),
        ],
        &kw,
        None,
    )
    .await
    .expect("Activate Bastion Module I on Paladin");
    let (_, ball_bastion) = sys.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    assert_eq!(
        ball_bastion.max_velocity, 0.0,
        "Bastion Module I must immobilize the Marauder"
    );
    let fit_bastion = ctx
        .get_or_rebuild_ship_fitting(char_id, ship_id)
        .expect("fit_bastion");
    assert!(
        fit_bastion.ship.get_attribute(attr_ids::ARMOR_EM_DAMAGE_RESONANCE, &ctx.sde) < 0.50,
        "Bastion Module I must boost Marauder armor resistances"
    );
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Deactivate",
        &[
            PyRep::Integer(siege_id),
            PyRep::String("moduleBonusBastion".into()),
        ],
        &kw,
        None,
    )
    .await
    .expect("Deactivate Bastion Module I");

    // 4. Capital Industrial Core I (28583) on Rorqual (28352) (feat:modules.industrial-core)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(s) = snap.items.get_mut(&ship_id) {
            s.type_id = 28352;
        }
        if let Some(m) = snap.items.get_mut(&siege_id) {
            m.type_id = 28583;
        }
    });
    ctx.invalidate_ship_fitting(ship_id);
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Activate",
        &[
            PyRep::Integer(siege_id),
            PyRep::String("moduleBonusIndustrialCore".into()),
            PyRep::None,
            PyRep::Integer(1),
        ],
        &kw,
        None,
    )
    .await
    .expect("Activate Capital Industrial Core I on Rorqual");
    let hw_after_core = ctx
        .db
        .with_snapshot(|s| s.items.get(&heavy_water_id).map(|i| i.quantity).unwrap_or(0));
    assert!(
        hw_after_core < 2_000,
        "Capital Industrial Core I must consume Heavy Water on activation"
    );
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Deactivate",
        &[
            PyRep::Integer(siege_id),
            PyRep::String("moduleBonusIndustrialCore".into()),
        ],
        &kw,
        None,
    )
    .await
    .expect("Deactivate Capital Industrial Core I");

    // 5. Amarr Phenomena Generator (43658) on Avatar Titan (11567) (feat:modules.phenomena-generator)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(s) = snap.items.get_mut(&ship_id) {
            s.type_id = 11567;
        }
        if let Some(m) = snap.items.get_mut(&siege_id) {
            m.type_id = 43658;
        }
    });
    ctx.invalidate_ship_fitting(ship_id);
    let ship_info_before_phenom = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "ItemGetInfo",
            &[PyRep::Integer(ship_id)],
            &kw,
            None,
        )
        .await
        .expect("ItemGetInfo before Amarr Phenomena Generator");
    let cap_recharge_before_phenom = extract_attr_from_info(&ship_info_before_phenom, 55);
    let kin_res_before_phenom = extract_attr_from_info(&ship_info_before_phenom, 269);
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Activate",
        &[
            PyRep::Integer(siege_id),
            PyRep::String("TitanPhenomenaGenerator".into()),
            PyRep::None,
            PyRep::Integer(1),
        ],
        &kw,
        None,
    )
    .await
    .expect("Activate Amarr Phenomena Generator on Avatar");
    let ship_info_phenom = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "ItemGetInfo",
            &[PyRep::Integer(ship_id)],
            &kw,
            None,
        )
        .await
        .expect("ItemGetInfo under Amarr Phenomena Generator");
    let cap_recharge_under_phenom = extract_attr_from_info(&ship_info_phenom, 55);
    let kin_res_under_phenom = extract_attr_from_info(&ship_info_phenom, 269);
    assert!(
        cap_recharge_under_phenom < cap_recharge_before_phenom
            && kin_res_under_phenom < kin_res_before_phenom,
        "Amarr Phenomena Generator dbuff must improve cap recharge time ({cap_recharge_under_phenom} < {cap_recharge_before_phenom}) and kinetic resonance ({kin_res_under_phenom} < {kin_res_before_phenom})"
    );
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Deactivate",
        &[
            PyRep::Integer(siege_id),
            PyRep::String("TitanPhenomenaGenerator".into()),
        ],
        &kw,
        None,
    )
    .await
    .expect("Deactivate Amarr Phenomena Generator");

    // 6. Zero-Point Mass Entangler (49099) on Devoter Heavy Interdiction Cruiser (12017) (feat:modules.zero-point-mass-entangler)
    let mwd_id = 100_002_204i64;
    seed_item(&ctx, mwd_id, 434, char_id, ship_id, InventoryFlag::MedSlot0, 1, true);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(s) = snap.items.get_mut(&ship_id) {
            s.type_id = 12017;
        }
        if let Some(m) = snap.items.get_mut(&siege_id) {
            m.type_id = 49099;
            m.flag = InventoryFlag::HiSlot0;
        }
    });
    ctx.invalidate_ship_fitting(ship_id);
    let (_, ball_before_zpme) = sys.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    let orig_mass = ball_before_zpme.mass;
    let orig_vel = ball_before_zpme.max_velocity.max(200.0);

    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Activate",
        &[
            PyRep::Integer(siege_id),
            PyRep::String("moduleBonusZeroPointMassEntangler".into()),
            PyRep::None,
            PyRep::Integer(1),
        ],
        &kw,
        None,
    )
    .await
    .expect("Activate Zero-Point Mass Entangler on Devoter");
    let (_, ball_zpme) = sys.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    assert!(
        (ball_zpme.mass - orig_mass * 0.20).abs() < 1.0,
        "Zero-Point Mass Entangler must reduce HIC mass by 80% (to 20%), got {} vs {}",
        ball_zpme.mass,
        orig_mass
    );
    assert!(
        ball_zpme.max_velocity <= orig_vel * 0.05 + 0.1,
        "Zero-Point Mass Entangler must reduce HIC max velocity by 95% (to 5%)"
    );
    let mwd_under_zpme = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "Activate",
            &[
                PyRep::Integer(mwd_id),
                PyRep::String("moduleBonusMicrowarpdrive".into()),
                PyRep::None,
                PyRep::Integer(1),
            ],
            &kw,
            None,
        )
        .await;
    assert_eq!(
        mwd_under_zpme
            .expect_err("Zero-Point Mass Entangler must disable MWD activation")
            .code,
        "WarpScrambled"
    );
}
