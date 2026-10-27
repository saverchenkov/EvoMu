//! Phase 4 Dogma remote logistics, mutadaptive spool, and capital remote assistance impedance tests (`DG-4` part 3).

use crate::common;
use super::dogma_tank_logistics::{grant_all_required_skills_for_type, seed_item};

use eve_db::InventoryFlag;
use eve_destiny::{Ball, CombatState, DVec3, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::HashMap;

#[tokio::test]
async fn test_dg4_remote_logistics_mutadaptive_spool_and_impedance() {
    // parity: feat:modules.remote-shield, feat:modules.remote-armor, feat:modules.remote-hull, feat:modules.remote-cap, feat:modules.remote-assistance-impedance, rpc:charUnboundMgr.SelectCharacterID
    eve_testkit::attest(&[
        "feat:modules.remote-shield",
        "feat:modules.remote-armor",
        "feat:modules.remote-hull",
        "feat:modules.remote-cap",
        "feat:modules.remote-assistance-impedance",
    ]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();
    let mut sess = SessionState::new(4202);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(90_000_001)], &kw, None)
        .await
        .expect("SelectCharacterID");

    let char_id = 90_000_001i64;
    let ship_id = 100_000_001i64;
    let station_id = 60_003_760i64;

    // Upgrade ship to Zarmazd (49713, Triglavian Logistics Cruiser, 5 HiSlots)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ship_item) = snap.items.get_mut(&ship_id) {
            ship_item.type_id = 49713;
        }
        snap.items.remove(&100_000_005);
    });
    ctx.invalidate_ship_fitting(ship_id);

    for tid in [49713u32, 3586, 11355, 49770, 4299, 529, 587, 42242, 24569] {
        grant_all_required_skills_for_type(&ctx, char_id, tid);
    }
    ctx.db.with_mut_snapshot(|snap| {
        let entry = snap.skills.entry(char_id).or_default();
        for &(sid, lvl) in &[(3426u32, 5u8), (3413, 5), (3418, 5), (3417, 5), (3428, 5), (3429, 5), (3431, 5), (49743, 5)] {
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

    let rem_shield_id = 100_002_101i64;
    let rem_armor_1_id = 100_002_102i64;
    let rem_armor_2_id = 100_002_103i64;
    let mutadaptive_id = 100_002_104i64;
    let rem_hull_id = 100_002_105i64;
    let rem_cap_id = 100_002_106i64;

    seed_item(&ctx, rem_shield_id, 3586, char_id, station_id, InventoryFlag::Hangar, 1, true);
    seed_item(&ctx, rem_armor_1_id, 11355, char_id, station_id, InventoryFlag::Hangar, 1, true);
    seed_item(&ctx, rem_armor_2_id, 11355, char_id, station_id, InventoryFlag::Hangar, 1, true);
    seed_item(&ctx, mutadaptive_id, 49770, char_id, station_id, InventoryFlag::Hangar, 1, true);
    seed_item(&ctx, rem_hull_id, 4299, char_id, ship_id, InventoryFlag::Cargo, 1, true);
    seed_item(&ctx, rem_cap_id, 529, char_id, ship_id, InventoryFlag::Cargo, 1, true);

    for (mid, flag) in [
        (rem_shield_id, InventoryFlag::HiSlot0),
        (rem_armor_1_id, InventoryFlag::HiSlot1),
        (rem_armor_2_id, InventoryFlag::HiSlot2),
        (mutadaptive_id, InventoryFlag::HiSlot3),
    ] {
        ctx.dispatch_rpc(
            &mut sess,
            "invbroker",
            None,
            "Add",
            &[PyRep::Integer(mid), PyRep::Integer(ship_id), PyRep::Integer(flag as i64)],
            &kw,
            None,
        )
        .await
        .expect("Fit remote logistics module");
    }

    ctx.dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock");
    let sys = ctx.systems.get_or_spawn(30_000_142);
    let (_, ship_ball) = sys.get_ball_snapshot(ship_id).await.unwrap().unwrap();

    let ally_char_id = 90_000_002i64;
    let ally_id = 100_002_120i64;
    seed_item(&ctx, ally_id, 587, ally_char_id, 30_000_142, InventoryFlag::None, 1, true);
    let mut ally_cs = CombatState::default();
    ally_cs.max_shield_hp = 2000.0;
    ally_cs.shield_hp = 100.0;
    ally_cs.max_armor_hp = 20_000.0;
    ally_cs.armor_hp = 100.0;
    ally_cs.max_structure_hp = 2000.0;
    ally_cs.structure_hp = 100.0;
    ally_cs.max_capacitor = 1000.0;
    ally_cs.capacitor = 100.0;
    ally_cs.shield_recharge_time_ms = 0.0;
    ally_cs.capacitor_recharge_time_ms = 0.0;
    sys.spawn_ball_with_combat(
        Ball {
            id: ally_id,
            type_id: 587,
            position: ship_ball.position + DVec3::new(3_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 365.0,
            inertia: 3.2,
            mass: 1_350_000.0,
            radius: 35.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
        ally_cs,
    )
    .await
    .unwrap();
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "AddTarget", &[PyRep::Integer(ally_id)], &kw, None)
        .await
        .expect("AddTarget on ally_id");
    for _ in 0..24 {
        if sys.get_target_locks(ship_id).await.unwrap().iter().any(|l| l.target_id == ally_id && l.locked) {
            break;
        }
        sys.step_manual(0.5).await.unwrap();
    }
    assert!(sys.get_target_locks(ship_id).await.unwrap().iter().any(|l| l.target_id == ally_id && l.locked));

    let now_unix = ctx.now_unix_secs();
    {
        let mut cw = ctx.crimewatch.write().unwrap();
        let a_state = cw.get_or_create(ally_char_id);
        a_state.suspect_flag_until_unix = now_unix + 900;
        a_state.weapon_timer_until_unix = now_unix + 60;
    }

    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Activate",
        &[
            PyRep::Integer(rem_shield_id),
            PyRep::String("shipModuleRemoteShieldBooster".into()),
            PyRep::Integer(ally_id),
            PyRep::Integer(0),
        ],
        &kw,
        None,
    )
    .await
    .expect("Activate Small Remote Shield Booster I");
    let ally_shield_after = sys.get_combat_state(ally_id).await.unwrap().unwrap().shield_hp;
    assert!(ally_shield_after > 130.0, "got {ally_shield_after}");
    {
        let cw = ctx.crimewatch.read().unwrap();
        let l_state = cw.states.get(&char_id).expect("logistics crimewatch state");
        assert!(l_state.is_suspect(now_unix) && l_state.is_weapon_timer_active(now_unix));
    }
    for _ in 0..5 {
        sys.step_manual(1.0).await.unwrap();
    }

    // 2. Verify Remote Hull Repairer (4299) & Remote Capacitor Transmitter (529)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(s_it) = snap.items.get_mut(&rem_shield_id) {
            s_it.type_id = 4299;
        }
    });
    ctx.invalidate_ship_fitting(ship_id);
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Activate",
        &[
            PyRep::Integer(rem_shield_id),
            PyRep::String("RemoteHullRepair".into()),
            PyRep::Integer(ally_id),
            PyRep::Integer(0),
        ],
        &kw,
        None,
    )
    .await
    .expect("Activate Small Remote Hull Repairer I");
    for _ in 0..8 {
        sys.step_manual(1.0).await.unwrap();
    }
    let ally_hull_after = sys.get_combat_state(ally_id).await.unwrap().unwrap().structure_hp;
    assert!(ally_hull_after > 110.0, "got {ally_hull_after}");

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(s_it) = snap.items.get_mut(&rem_shield_id) {
            s_it.type_id = 529;
        }
    });
    ctx.invalidate_ship_fitting(ship_id);
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Activate",
        &[
            PyRep::Integer(rem_shield_id),
            PyRep::String("energyTransfer".into()),
            PyRep::Integer(ally_id),
            PyRep::Integer(0),
        ],
        &kw,
        None,
    )
    .await
    .expect("Activate Small Remote Capacitor Transmitter I");
    let ally_cap_after = sys.get_combat_state(ally_id).await.unwrap().unwrap().capacitor;
    assert!(ally_cap_after > 120.0, "got {ally_cap_after}");

    // 3a. Sub-capital Remote Armor Repairers (`11355`) stack 100% additively (`2.0x` single repair, `R-REMOTEREP-STACK-01`)
    let armor_before_single = sys.get_combat_state(ally_id).await.unwrap().unwrap().armor_hp;
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Activate",
        &[
            PyRep::Integer(rem_armor_1_id),
            PyRep::String("shipModuleRemoteArmorRepairer".into()),
            PyRep::Integer(ally_id),
            PyRep::Integer(0),
        ],
        &kw,
        None,
    )
    .await
    .expect("Activate first Small Remote Armor Repairer I");
    for _ in 0..6 {
        sys.step_manual(1.0).await.unwrap();
    }
    let armor_after_single = sys.get_combat_state(ally_id).await.unwrap().unwrap().armor_hp;
    let single_rep_gain = armor_after_single - armor_before_single;
    assert!(single_rep_gain > 30.0);

    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Activate",
        &[
            PyRep::Integer(rem_armor_1_id),
            PyRep::String("shipModuleRemoteArmorRepairer".into()),
            PyRep::Integer(ally_id),
            PyRep::Integer(0),
        ],
        &kw,
        None,
    )
    .await
    .unwrap();
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Activate",
        &[
            PyRep::Integer(rem_armor_2_id),
            PyRep::String("shipModuleRemoteArmorRepairer".into()),
            PyRep::Integer(ally_id),
            PyRep::Integer(0),
        ],
        &kw,
        None,
    )
    .await
    .unwrap();
    for _ in 0..6 {
        sys.step_manual(1.0).await.unwrap();
    }
    let armor_after_dual = sys.get_combat_state(ally_id).await.unwrap().unwrap().armor_hp;
    let dual_rep_gain = armor_after_dual - armor_after_single;
    assert!(
        (dual_rep_gain - 2.0 * single_rep_gain).abs() < 1e-3,
        "Sub-capital remote armor repairers must stack 100% additively (2.0x single={:.2}, got {:.2})",
        2.0 * single_rep_gain,
        dual_rep_gain
    );

    // 4. Verify Triglavian Heavy Mutadaptive Remote Armor Repairer I (49770) spool-up (+10%/cycle)
    sys.set_capacitor(ship_id, 5000.0).await.unwrap();
    let armor_before_mut = sys.get_combat_state(ally_id).await.unwrap().unwrap().armor_hp;
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Activate",
        &[
            PyRep::Integer(mutadaptive_id),
            PyRep::String("shipModuleRemoteArmorMutadaptiveRepairer".into()),
            PyRep::Integer(ally_id),
            PyRep::Integer(1),
        ],
        &kw,
        None,
    )
    .await
    .expect("Activate Heavy Mutadaptive Remote Armor Repairer I");
    for _ in 0..7 {
        sys.step_manual(1.0).await.unwrap();
    }
    let armor_after_mut_c1 = sys.get_combat_state(ally_id).await.unwrap().unwrap().armor_hp;
    let gain_c1 = armor_after_mut_c1 - armor_before_mut;
    for _ in 0..7 {
        sys.step_manual(1.0).await.unwrap();
    }
    let armor_after_mut_c2 = sys.get_combat_state(ally_id).await.unwrap().unwrap().armor_hp;
    let gain_c2 = armor_after_mut_c2 - armor_after_mut_c1;
    assert!((gain_c2 - gain_c1 * 1.10).abs() < 1.0, "c1={gain_c1}, c2={gain_c2}");
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Deactivate",
        &[PyRep::Integer(mutadaptive_id), PyRep::String("shipModuleRemoteArmorMutadaptiveRepairer".into())],
        &kw,
        None,
    )
    .await
    .expect("Deactivate Heavy Mutadaptive Remote Armor Repairer I");

    // 3b. Capital Remote Assistance Impedance (`feat:modules.remote-assistance-impedance`):
    // Two Capital Remote Armor Repairer I (`24569`) on Dagon FAX (`42242`) experience diminishing returns!
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ship_item) = snap.items.get_mut(&ship_id) {
            ship_item.type_id = 42242;
        }
        if let Some(m1) = snap.items.get_mut(&rem_armor_1_id) {
            m1.type_id = 24569;
        }
        if let Some(m2) = snap.items.get_mut(&rem_armor_2_id) {
            m2.type_id = 24569;
        }
    });
    ctx.invalidate_ship_fitting(ship_id);
    let mut fax_cs = sys.get_combat_state(ship_id).await.unwrap().unwrap();
    fax_cs.max_capacitor = 80_000.0;
    fax_cs.capacitor = 80_000.0;
    sys.set_combat_state(ship_id, fax_cs).await.unwrap();

    let cap_armor_0 = sys.get_combat_state(ally_id).await.unwrap().unwrap().armor_hp;
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Activate",
        &[
            PyRep::Integer(rem_armor_1_id),
            PyRep::String("shipModuleRemoteArmorRepairer".into()),
            PyRep::Integer(ally_id),
            PyRep::Integer(0),
        ],
        &kw,
        None,
    )
    .await
    .expect("Activate single Capital Remote Armor Repairer I");
    for _ in 0..16 {
        sys.step_manual(1.0).await.unwrap();
    }
    let cap_armor_1 = sys.get_combat_state(ally_id).await.unwrap().unwrap().armor_hp;
    let single_cap_gain = cap_armor_1 - cap_armor_0;
    assert!(single_cap_gain > 500.0, "got {single_cap_gain}");

    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Activate",
        &[
            PyRep::Integer(rem_armor_1_id),
            PyRep::String("shipModuleRemoteArmorRepairer".into()),
            PyRep::Integer(ally_id),
            PyRep::Integer(0),
        ],
        &kw,
        None,
    )
    .await
    .unwrap();
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Activate",
        &[
            PyRep::Integer(rem_armor_2_id),
            PyRep::String("shipModuleRemoteArmorRepairer".into()),
            PyRep::Integer(ally_id),
            PyRep::Integer(0),
        ],
        &kw,
        None,
    )
    .await
    .unwrap();
    for _ in 0..16 {
        sys.step_manual(1.0).await.unwrap();
    }
    let cap_armor_2 = sys.get_combat_state(ally_id).await.unwrap().unwrap().armor_hp;
    let dual_cap_gain = cap_armor_2 - cap_armor_1;
    assert!(
        dual_cap_gain > single_cap_gain && dual_cap_gain < 2.0 * single_cap_gain - 10.0,
        "Capital Remote Assistance Impedance must attenuate second Capital Remote Armor Repairer I (dual={dual_cap_gain} vs 2x single={})",
        2.0 * single_cap_gain
    );
}
