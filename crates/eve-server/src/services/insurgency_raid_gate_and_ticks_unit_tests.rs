//! Semantic unit tests split from `insurgency_complex_ops_extra_unit_tests`.

#![forbid(unsafe_code)]

use super::insurgency_complex_ops_extra_unit_tests::*;

use super::insurgency_complex_ops::*;
use crate::services::insurgency_services::{
    InsurgencyCampaign, FACTION_GURISTAS,
};
use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, MovementMode};
use glam::DVec3;
use std::collections::HashMap;

#[tokio::test]
async fn activate_outpost_raid_gate_validates_all_restrictions_and_spawns_defender() {
    let mut ctx = test_ctx();
    let frig_tid = 940_010u32;
    let bs_tid = 940_011u32;
    let wcs_tid = 940_012u32;
    ensure_test_type(&mut ctx.sde, frig_tid, 25, None);
    ensure_test_type(&mut ctx.sde, bs_tid, 27, None);
    ensure_test_type(&mut ctx.sde, wcs_tid, 315, None);

    let sys_id = 30002538u32;
    let gate_id = 1_893_999_011i64;
    let beacon_id = gate_id + 1_000_000;
    let char_id = 94_301i64;
    let ship_id = 94_302i64;
    let wcs_item_id = 94_303i64;
    let gpos = OutpostRaidSize::Small.gate_position();
    let now = ctx.now_unix();

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().expect("demo char");
        ch.character_id = char_id;
        ch.solar_system_id = sys_id;
        ch.active_ship_id = ship_id;
        ch.war_faction_id = None;
        snap.characters.insert(char_id, ch);

        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: bs_tid,
                owner_id: char_id,
                location_id: i64::from(sys_id),
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            wcs_item_id,
            InventoryItemRecord {
                item_id: wcs_item_id,
                type_id: wcs_tid,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::LoSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        let mut ops = load_ops_state(snap);
        ops.outpost_raids.insert(
            gate_id,
            OutpostRaidInstance {
                gate_id,
                beacon_id,
                campaign_id: 78,
                solar_system_id: sys_id,
                pirate_faction_id: FACTION_GURISTAS,
                target_faction_id: 500_001,
                size: OutpostRaidSize::Small,
                is_static: false,
                occupants: Vec::new(),
                mordus_defender_ball_id: None,
                pirate_defender_ball_id: None,
                mordus_defender_cleared: false,
                pirate_defender_cleared: false,
                weighted_elapsed_secs: 0.0,
                last_step_unix: now,
                completed_on_cooldown: false,
                respawn_at_unix: 0,
            },
        );
        save_ops_state(snap, &ops);
    });

    assert!(ctx.db.with_snapshot(|snap| is_outpost_raid_gate(snap, gate_id)));

    // 1) NotEnlistedInMilitia
    assert_eq!(
        activate_outpost_raid_gate_in_space(&ctx, char_id, ship_id, sys_id, gate_id)
            .await
            .unwrap_err()
            .code,
        "NotEnlistedInMilitia"
    );

    // 2) WarpCoreStabilizerRestricted
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().war_faction_id = Some(FACTION_GURISTAS);
    });
    assert_eq!(
        activate_outpost_raid_gate_in_space(&ctx, char_id, ship_id, sys_id, gate_id)
            .await
            .unwrap_err()
            .code,
        "WarpCoreStabilizerRestricted"
    );

    // 3) ShipRestrictedForComplex (Battleship in Small raid)
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.remove(&wcs_item_id);
    });
    assert_eq!(
        activate_outpost_raid_gate_in_space(&ctx, char_id, ship_id, sys_id, gate_id)
            .await
            .unwrap_err()
            .code,
        "ShipRestrictedForComplex"
    );

    // 4) ShipNotInSpace
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&ship_id).unwrap().type_id = frig_tid;
    });
    assert_eq!(
        activate_outpost_raid_gate_in_space(&ctx, char_id, ship_id, sys_id, gate_id)
            .await
            .unwrap_err()
            .code,
        "ShipNotInSpace"
    );

    // 5) TooFarFromGate (> 2500m)
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_handle
        .spawn_ball(Ball {
            id: ship_id,
            type_id: frig_tid,
            position: gpos + DVec3::new(10_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 50.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    assert_eq!(
        activate_outpost_raid_gate_in_space(&ctx, char_id, ship_id, sys_id, gate_id)
            .await
            .unwrap_err()
            .code,
        "TooFarFromGate"
    );

    // 6) Within 2500m -> teleports to pocket and spawns Mordu's defender
    let _ = sys_handle.set_ball_position(ship_id, gpos + DVec3::new(500.0, 0.0, 0.0)).await;
    let res = activate_outpost_raid_gate_in_space(&ctx, char_id, ship_id, sys_id, gate_id)
        .await
        .expect("gate activation ok");
    assert!(format!("{res:?}").contains("activated"));
    ctx.db.with_snapshot(|snap| {
        let r = &load_ops_state(snap).outpost_raids[&gate_id];
        assert!(r.occupants.contains(&(char_id, ship_id)));
        assert!(r.mordus_defender_ball_id.is_some());
    });
}

#[tokio::test]
async fn mining_ambush_and_outpost_raid_npc_kills_tick_completion_and_orphan_cleanup() {
    let mut ctx = test_ctx();
    let frig_tid = 940_020u32;
    ensure_test_type(&mut ctx.sde, frig_tid, 25, None);

    let sys_id = 30002539u32;
    let pir_char = 94_401i64;
    let pir_ship = 94_402i64;
    let emp_char = 94_403i64;
    let wreck_id = 94_404i64;
    let rorq_id = 94_410i64;
    let tackler_id = 94_411i64;
    let beacon_id = 1_892_100_000 + i64::from(sys_id);
    let gate_id = 1_893_100_000 + i64::from(sys_id) * 10 + 1;
    let raid_beacon_id = gate_id + 1_000_000;
    let def_id = 94_412i64;
    let pocket = OutpostRaidSize::Small.pocket_position();
    let now = ctx.now_unix();

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().expect("demo char");
        ch.character_id = pir_char;
        ch.solar_system_id = sys_id;
        ch.station_id = None;
        ch.structure_id = None;
        ch.active_ship_id = pir_ship;
        ch.war_faction_id = Some(FACTION_GURISTAS);
        ch.loyalty_points.clear();
        snap.characters.insert(pir_char, ch.clone());

        ch.character_id = emp_char;
        ch.active_ship_id = 0;
        ch.war_faction_id = Some(500_001); // Caldari opposes Guristas
        ch.loyalty_points.clear();
        snap.characters.insert(emp_char, ch);

        snap.items.insert(
            pir_ship,
            InventoryItemRecord {
                item_id: pir_ship,
                type_id: frig_tid,
                owner_id: pir_char,
                location_id: i64::from(sys_id),
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            wreck_id,
            InventoryItemRecord {
                item_id: wreck_id,
                type_id: 26557,
                owner_id: pir_char,
                location_id: i64::from(sys_id),
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.insurgency_runtime.campaigns.insert(
            79,
            InsurgencyCampaign {
                campaign_id: 79,
                pirate_faction_id: FACTION_GURISTAS,
                target_faction_id: 500_001,
                fob_solar_system_id: sys_id,
                systems: vec![sys_id],
                created_at_unix: now.saturating_sub(3_600),
                forecasting_end_unix: now.saturating_sub(1_800),
                active_end_unix: now + 86_400,
                winddown_end_unix: now + 172_800,
                corruption_points: HashMap::from([(sys_id, 0)]),
                suppression_points: HashMap::from([(sys_id, 0)]),
            },
        );
        let mut ops = load_ops_state(snap);
        ops.mining_ambushes.insert(
            sys_id,
            MiningAmbushInstance {
                campaign_id: 79,
                solar_system_id: sys_id,
                pirate_faction_id: FACTION_GURISTAS,
                target_faction_id: 500_001,
                beacon_id,
                rorqual_ball_id: rorq_id,
                pirate_tackler_ids: vec![tackler_id],
                ore_defender_ids: Vec::new(),
                ransom_elapsed_secs: 0,
                last_step_unix: now,
                resolved: false,
                rorqual_warping_off_until: None,
                cooldown_until_unix: 0,
            },
        );
        ops.outpost_raids.insert(
            gate_id,
            OutpostRaidInstance {
                gate_id,
                beacon_id: raid_beacon_id,
                campaign_id: 79,
                solar_system_id: sys_id,
                pirate_faction_id: FACTION_GURISTAS,
                target_faction_id: 500_001,
                size: OutpostRaidSize::Small,
                is_static: false,
                occupants: vec![(pir_char, pir_ship)],
                mordus_defender_ball_id: Some(def_id),
                pirate_defender_ball_id: None,
                mordus_defender_cleared: false,
                pirate_defender_cleared: false,
                weighted_elapsed_secs: 590.0,
                last_step_unix: now,
                completed_on_cooldown: false,
                respawn_at_unix: 0,
            },
        );
        save_ops_state(snap, &ops);
    });

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_handle
        .spawn_ball(Ball {
            id: pir_ship,
            type_id: frig_tid,
            position: pocket,
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 50.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;

    // 1) Killing outpost defender marks mordus_defender_cleared = true
    on_insurgency_npc_destroyed(&ctx, sys_id, def_id, 79_545, 0, pir_char).await;
    ctx.db.with_snapshot(|snap| {
        assert!(load_ops_state(snap).outpost_raids[&gate_id].mordus_defender_cleared);
    });

    // 2) Killing last pirate tackler resolves ambush for Empire and awards suppression points & LP
    on_insurgency_npc_destroyed(&ctx, sys_id, tackler_id, 79_597, 0, emp_char).await;
    ctx.db.with_snapshot(|snap| {
        assert!(load_ops_state(snap).mining_ambushes[&sys_id].resolved);
        assert_eq!(
            snap.insurgency_runtime.campaigns[&79].suppression_points[&sys_id],
            MINING_AMBUSH_SUPPRESSION_PTS
        );
    });

    // 3) Reset ambush for Rorqual kill test -> stamps TYPE_ORE_RORQUAL_WRECK and adds corruption
    ctx.db.with_mut_snapshot(|snap| {
        let mut ops = load_ops_state(snap);
        let amb = ops.mining_ambushes.get_mut(&sys_id).unwrap();
        amb.resolved = false;
        amb.cooldown_until_unix = 0;
        save_ops_state(snap, &ops);
    });
    on_insurgency_npc_destroyed(&ctx, sys_id, rorq_id, TYPE_ORE_RORQUAL, wreck_id, pir_char).await;
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&wreck_id].type_id, TYPE_ORE_RORQUAL_WRECK);
        assert_eq!(
            snap.insurgency_runtime.campaigns[&79].corruption_points[&sys_id],
            MINING_AMBUSH_CORRUPTION_PTS
        );
    });

    // 4) tick_insurgency_complex_ops completes the Small outpost raid capture for pir_char
    tick_insurgency_complex_ops(&ctx, now + 20).await;
    ctx.db.with_snapshot(|snap| {
        let r = &load_ops_state(snap).outpost_raids[&gate_id];
        assert!(r.completed_on_cooldown);
        let pir_lp: i64 = snap.characters[&pir_char].loyalty_points.values().sum();
        assert_eq!(pir_lp, OutpostRaidSize::Small.base_lp_reward());
    });

    // 5) cleanup_orphan_insurgency_complex_ops removes ambushes and raids for the system
    cleanup_orphan_insurgency_complex_ops(&ctx, &[sys_id]).await;
    ctx.db.with_snapshot(|snap| {
        let ops = load_ops_state(snap);
        assert!(!ops.mining_ambushes.contains_key(&sys_id));
        assert!(!ops.outpost_raids.contains_key(&gate_id));
    });
}
