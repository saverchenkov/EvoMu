use crate::router::ServerContext;
use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::{
    make_fighter_ctx_and_session, make_fighter_item, make_fighter_type,
};
use crate::services::combat_crimewatch::{with_mut_combat_runtime, SafetyLevel};
use crate::services::encounter_surveillance_system::{handle_ess_link, tick_ess_main_bank_links};
use crate::services::insurgency_complex_ops::{
    activate_outpost_raid_gate_in_space, load_ops_state, save_ops_state,
    tick_insurgency_complex_ops, OutpostRaidInstance, OutpostRaidSize, OutpostRaidSizeExt,
};
use crate::services::insurgency_services::{InsurgencyCampaign, FACTION_GURISTAS};
use crate::services::pve_ess_reserve_banks::tick_ess_reserve_bank_links;
use crate::services::space_destiny::{
    get_ess_reserve_pulses, set_ess_reserve_pulses, EssReservePulseState,
};
use eve_db::{InventoryFlag, InventoryItemRecord, IskCents, SystemDynamicState};
use eve_destiny::{Ball, CombatState, DVec3, MovementMode};
use eve_proto::PyRep;
use eve_sde::SolarSystem;
use std::collections::HashMap;
use std::sync::Arc;

fn test_ship_ball(id: i64, type_id: u32, pos: DVec3) -> Ball {
    Ball {
        id,
        type_id,
        position: pos,
        velocity: DVec3::ZERO,
        max_velocity: 350.0,
        inertia: 1.0,
        mass: 1_000_000.0,
        radius: 35.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    }
}

#[tokio::test]
async fn r7_tc_06_outpost_raid_signed_capture_and_gate_occupant_filter() {
    let ctx = ServerContext::new();
    let (pir_char, emp_gate_char, emp_bypass_char) =
        (95_300_101_i64, 95_300_102_i64, 95_300_103_i64);
    let (pir_ship, emp_gate_ship, emp_bypass_ship) =
        (96_300_101_i64, 96_300_102_i64, 96_300_103_i64);
    let sys_id = 30_002_538_u32;
    let gate_id = 1_893_100_001_i64;
    let now = 20_000_u64;
    let gate_pos = OutpostRaidSize::Small.gate_position();
    let pocket = OutpostRaidSize::Small.pocket_position();
    assert!(
        gate_pos.distance(pocket) >= 2_000_000.0,
        "Outpost raid pocket must be deep in deadspace far beyond MJD/subwarp range from gate"
    );

    let frig_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.published && t.category_id == 6 && t.group_id == 25)
        .map(|t| t.type_id)
        .unwrap_or(587);

    ctx.db.with_mut_snapshot(|snap| {
        let tmpl = snap.characters.values().next().unwrap().clone();
        for (cid, sid, wf) in [
            (pir_char, pir_ship, FACTION_GURISTAS),
            (emp_gate_char, emp_gate_ship, 500_001),
            (emp_bypass_char, emp_bypass_ship, 500_001),
        ] {
            let mut ch = tmpl.clone();
            ch.character_id = cid;
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = sid;
            ch.war_faction_id = Some(wf);
            snap.characters.insert(cid, ch);
            snap.items.insert(
                sid,
                InventoryItemRecord {
                    item_id: sid,
                    type_id: frig_tid,
                    owner_id: cid,
                    location_id: i64::from(sys_id),
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
        }
        snap.insurgency_runtime.campaigns.insert(
            81,
            InsurgencyCampaign {
                campaign_id: 81,
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
        ops.outpost_raids.insert(
            gate_id,
            OutpostRaidInstance {
                gate_id,
                beacon_id: gate_id + 100,
                campaign_id: 81,
                solar_system_id: sys_id,
                pirate_faction_id: FACTION_GURISTAS,
                target_faction_id: 500_001,
                size: OutpostRaidSize::Small,
                is_static: false,
                occupants: vec![(pir_char, pir_ship)],
                mordus_defender_ball_id: None,
                pirate_defender_ball_id: None,
                mordus_defender_cleared: true,
                pirate_defender_cleared: true,
                weighted_elapsed_secs: 570.0,
                last_step_unix: now,
                completed_on_cooldown: false,
                respawn_at_unix: 0,
            },
        );
        save_ops_state(snap, &ops);
    });

    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_h
        .spawn_ball(test_ship_ball(pir_ship, frig_tid, pocket))
        .await;
    let _ = sys_h
        .spawn_ball(test_ship_ball(emp_bypass_ship, frig_tid, pocket))
        .await;
    let _ = sys_h
        .spawn_ball(test_ship_ball(emp_gate_ship, frig_tid, gate_pos))
        .await;

    tick_insurgency_complex_ops(&ctx, now + 10).await;
    ctx.db.with_snapshot(|snap| {
        let r = &load_ops_state(snap).outpost_raids[&gate_id];
        assert!(!r.completed_on_cooldown);
        assert_eq!(r.weighted_elapsed_secs, 580.0);
    });

    let _ = sys_h.set_ball_position(pir_ship, gate_pos).await;
    let _ = sys_h.set_ball_position(emp_bypass_ship, gate_pos).await;
    activate_outpost_raid_gate_in_space(&ctx, emp_gate_char, emp_gate_ship, sys_id, gate_id)
        .await
        .expect("Empire gate entry");

    tick_insurgency_complex_ops(&ctx, now + 40).await;
    ctx.db.with_snapshot(|snap| {
        let r = &load_ops_state(snap).outpost_raids[&gate_id];
        assert!(!r.completed_on_cooldown);
        assert_eq!(r.weighted_elapsed_secs, 550.0);
    });
}

#[tokio::test]
async fn r7_tc_11_ess_bank_isolation_hull_cloak_warp_and_1hz_range() {
    let (mut ctx, mut sess, char_id, ship_id, _) = make_fighter_ctx_and_session();
    let null_sys_id = 30_004_759_u32;
    let ess_ball_id = 820_000_000 + null_sys_id as i64;
    let sde_mut = Arc::make_mut(&mut ctx.sde);
    sde_mut.solar_systems.insert(
        null_sys_id,
        SolarSystem {
            solar_system_id: null_sys_id,
            region_id: 10_000_060,
            constellation_id: 20_000_700,
            name: "1DQ1-A".into(),
            security_status: -0.9,
            radius: 1.0e12,
            galactic_pos_ly: [-10.0, 0.0, -10.0],
            planet_ids: Vec::new(),
            stations: Vec::new(),
            wormhole_class_id: None,
        },
    );
    sde_mut
        .types
        .insert(57720, make_fighter_type(57720, 4102, 17, 1.0, vec![]));

    sess.solar_system_id = Some(null_sys_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = null_sys_id;
            ch.station_id = None;
            ch.structure_id = None;
        }
        snap.system_states.insert(
            null_sys_id,
            SystemDynamicState {
                solar_system_id: null_sys_id,
                dbs_multiplier: 1.0,
                ess_main_bank: IskCents::from_isk(15_000_000),
                ess_reserve_bank: IskCents::from_isk(45_000_000),
                ..SystemDynamicState::default()
            },
        );
        snap.items.insert(
            990_011,
            make_fighter_item(990_011, 57720, char_id, ship_id, InventoryFlag::Cargo, 1),
        );
    });
    with_mut_combat_runtime(&ctx, |rt| {
        rt.safety_levels.insert(char_id, SafetyLevel::Red);
    });

    let sys = ctx.systems.get_or_spawn(null_sys_id);
    let _ = sys
        .spawn_ball_with_combat(
            test_ship_ball(ess_ball_id, 32_882, DVec3::ZERO),
            CombatState::new(),
        )
        .await;

    let _ = sys
        .spawn_ball_with_combat(
            test_ship_ball(ship_id, 670, DVec3::new(1_000.0, 0.0, 0.0)),
            CombatState::new(),
        )
        .await;
    assert_eq!(
        handle_ess_link(&ctx, &sess, "AttemptLinkToMainBank", &[PyRep::Integer(ess_ball_id)])
            .await
            .unwrap_err()
            .code,
        "CantDoThatInPod"
    );

    let _ = sys.remove_ball(ship_id).await;
    let mut cloaked_cs = CombatState::new();
    cloaked_cs.is_cloaked = true;
    let _ = sys
        .spawn_ball_with_combat(
            test_ship_ball(ship_id, 600, DVec3::new(1_000.0, 0.0, 0.0)),
            cloaked_cs,
        )
        .await;
    assert_eq!(
        handle_ess_link(&ctx, &sess, "AttemptLinkToMainBank", &[PyRep::Integer(ess_ball_id)])
            .await
            .unwrap_err()
            .code,
        "CantLinkWhileCloaked"
    );

    let _ = sys.remove_ball(ship_id).await;
    let mut warp_ball = test_ship_ball(ship_id, 600, DVec3::new(1_000.0, 0.0, 0.0));
    warp_ball.mode = MovementMode::new_warp(
        DVec3::new(1_000.0, 0.0, 0.0),
        DVec3::new(100_000.0, 0.0, 0.0),
        3.0,
    );
    let _ = sys.spawn_ball_with_combat(warp_ball, CombatState::new()).await;
    assert_eq!(
        handle_ess_link(&ctx, &sess, "AttemptLinkToMainBank", &[PyRep::Integer(ess_ball_id)])
            .await
            .unwrap_err()
            .code,
        "ShipWarping"
    );

    let _ = sys.remove_ball(ship_id).await;
    let _ = sys
        .spawn_ball_with_combat(
            test_ship_ball(ship_id, 600, DVec3::new(1_000.0, 0.0, 0.0)),
            CombatState::new(),
        )
        .await;
    for _ in 0..7 {
        let _ = sys.try_step_full_tick(1.0);
    }
    let res_start = handle_ess_link(
        &ctx,
        &sess,
        "AttemptLinkToReserveBank",
        &[PyRep::Integer(ess_ball_id)],
    )
    .await
    .unwrap();
    assert_eq!(res_start.dict_get("linkStartedOnly"), Some(&PyRep::Bool(true)));
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.system_states[&null_sys_id].ess_linked_character_id, None);
        assert_eq!(
            get_ess_reserve_pulses(&snap, null_sys_id)
                .reserve_linked_character_id,
            Some(char_id)
        );
    });

    let main_start = handle_ess_link(
        &ctx,
        &sess,
        "AttemptLinkToMainBank",
        &[PyRep::Integer(ess_ball_id)],
    )
    .await
    .unwrap();
    assert_eq!(main_start.dict_get("linkStartedOnly"), Some(&PyRep::Bool(true)));
    ctx.db.with_mut_snapshot(|snap| {
        set_ess_reserve_pulses(
            snap,
            null_sys_id,
            &EssReservePulseState {
                pulses_total: 1,
                pulses_remaining: 1,
                last_pulse_sec: None,
                reserve_linked_character_id: Some(char_id),
                reserve_link_started_tick: Some(0),
                link_started_unix: Some(100),
                unlocked_until_unix: 100 + 60,
            },
        );
    });
    let _ = tick_ess_reserve_bank_links(&ctx, 160).await;
    ctx.db.with_snapshot(|snap| {
        assert_eq!(
            snap.system_states[&null_sys_id].ess_linked_character_id,
            Some(char_id)
        );
        assert_eq!(
            get_ess_reserve_pulses(&snap, null_sys_id)
                .reserve_linked_character_id,
            None
        );
    });

    let _ = sys
        .set_ball_position(ship_id, DVec3::new(3_000.0, 0.0, 0.0))
        .await;
    assert_eq!(tick_ess_main_bank_links(&ctx).await, 1);
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.system_states[&null_sys_id].ess_linked_character_id, None);
    });
}
