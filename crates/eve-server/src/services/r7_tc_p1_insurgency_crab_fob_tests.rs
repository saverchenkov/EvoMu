use crate::router::ServerContext;
use crate::services::insurgency_services::{
    step_system_insurgency_capture_tick, InsurgencyCampaign, FACTION_GURISTAS,
};
use crate::services::pve_crab_beacons::{
    handle_crab_component_call, tick_all_crab_beacons, CrabBeaconPhase,
};
use crate::services::pve_fob_observatory::{
    apply_live_combat_damage_to_pirate_fob, destroy_pirate_fob_and_payout,
    get_observatory_site_state, on_world_ark_boss_killed, spawn_observatory_flashpoint,
    spawn_pirate_fob,
};
use crate::services::pve_special_sites::read_pve_special_reg;
use eve_db::{InventoryFlag, InventoryItemRecord, IskCents, SystemDynamicState};
use eve_destiny::{Ball, CombatState, DVec3, MovementMode};
use eve_machonet::SessionState;
use std::collections::HashMap;

fn make_ship_ball(id: i64, type_id: u32, pos: DVec3) -> Ball {
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

fn make_combat_state(hp: f64) -> CombatState {
    CombatState {
        shield_hp: hp,
        max_shield_hp: hp,
        armor_hp: hp,
        max_armor_hp: hp,
        structure_hp: hp,
        max_structure_hp: hp,
        shield_resonance: [1.0; 4],
        armor_resonance: [1.0; 4],
        structure_resonance: [1.0; 4],
        ..Default::default()
    }
}

#[tokio::test]
async fn r7_tc_p1_449_fw_complex_completes_during_active_insurgency() {
    let ctx = ServerContext::new();
    let sys_id = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| s.security_status > 0.0 && s.security_status < 0.45)
        .map(|s| s.solar_system_id)
        .unwrap();

    let char_id = 91_449_001_i64;
    let ship_id = 92_449_001_i64;
    let beacon_pos = [100_000.0, 0.0, 0.0];

    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_h
        .spawn_ball_with_combat(
            make_ship_ball(ship_id, 587, DVec3::new(105_000.0, 0.0, 0.0)),
            make_combat_state(1_000.0),
        )
        .await;

    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states.insert(
            sys_id,
            SystemDynamicState {
                solar_system_id: sys_id,
                fw_complex_position: Some(beacon_pos),
                fw_capture_duration_seconds: 600,
                fw_capture_seconds_elapsed: 599,
                last_fw_capture_tick: 0,
                ..Default::default()
            },
        );
        snap.insurgency_runtime.campaigns.insert(
            700_449,
            InsurgencyCampaign {
                campaign_id: 700_449,
                pirate_faction_id: FACTION_GURISTAS,
                target_faction_id: 500_001,
                fob_solar_system_id: sys_id,
                systems: vec![sys_id],
                created_at_unix: 100,
                forecasting_end_unix: 200,
                active_end_unix: 100_000,
                winddown_end_unix: 200_000,
                corruption_points: HashMap::from([(sys_id, 0)]),
                suppression_points: HashMap::from([(sys_id, 0)]),
            },
        );
        let mut ch = snap.characters.values().next().unwrap().clone();
        ch.character_id = char_id;
        ch.name = "InsurgentPilot".into();
        ch.solar_system_id = sys_id;
        ch.station_id = None;
        ch.structure_id = None;
        ch.active_ship_id = ship_id;
        ch.war_faction_id = Some(FACTION_GURISTAS);
        snap.characters.insert(char_id, ch);
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: char_id,
                location_id: i64::from(sys_id),
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });

    step_system_insurgency_capture_tick(&ctx, sys_id, ship_id, FACTION_GURISTAS).await;

    ctx.db.with_snapshot(|snap| {
        let st = &snap.system_states[&sys_id];
        assert!(
            st.fw_complex_position.is_none(),
            "FW complex in active insurgency lowsec system must complete and despawn when capture timer reaches duration"
        );
    });
}

#[tokio::test]
async fn r7_tc_p1_450_crab_link_clean_relink_and_range_disconnect() {
    let ctx = ServerContext::new();
    let sys_id = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| {
            s.solar_system_id < 30_100_000
                && matches!(s.wormhole_class_id, None | Some(7..=9))
                && s.security_status < 0.0
                && !matches!(s.region_id, 10_000_004 | 10_000_017 | 10_000_019 | 10_000_070)
        })
        .map(|s| s.solar_system_id)
        .unwrap();

    let char_id = 91_450_001_i64;
    let ship_id = 92_450_001_i64;
    let beacon_id = 93_450_001_i64;
    let ship_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.published && t.category_id == 6 && t.group_id == 547)
        .map(|t| t.type_id)
        .unwrap_or(23_911);
    let beacon_tid = ctx
        .sde
        .types
        .values()
        .find(|t| crate::services::pve_crab_beacons::is_crab_beacon_type(&ctx.sde, t.type_id))
        .map(|t| t.type_id)
        .unwrap();

    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_h
        .spawn_ball_with_combat(
            make_ship_ball(ship_id, ship_tid, DVec3::new(1_000.0, 0.0, 0.0)),
            make_combat_state(100_000.0),
        )
        .await;

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().unwrap().clone();
        ch.character_id = char_id;
        ch.name = "CarrierPilot".into();
        ch.solar_system_id = sys_id;
        ch.station_id = None;
        ch.structure_id = None;
        ch.active_ship_id = ship_id;
        snap.characters.insert(char_id, ch);
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: ship_tid,
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
            beacon_id,
            InventoryItemRecord {
                item_id: beacon_id,
                type_id: beacon_tid,
                owner_id: char_id,
                location_id: i64::from(sys_id),
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.item_positions.insert(beacon_id, [0.0, 0.0, 0.0]);
    });

    {
        let mut st = crate::services::inventory_fitting::fitting_scanners_superweapons::scanners_superweapons_state()
            .write()
            .unwrap();
        st.ship_dbuffs.entry((ctx.instance_id, ship_id)).or_default().push(
            crate::services::inventory_fitting::fitting_scanners_superweapons::ActiveDbuffEntry {
                dbuff_id: 2141,
                value: -50.0,
                expires_at_unix: u64::MAX,
                source_module_id: 999,
            },
        );
    }

    let sess = SessionState {
        session_id: 4501,
        user_id: Some(41),
        character_id: Some(char_id),
        solar_system_id: Some(sys_id),
        ship_id: Some(ship_id),
        ..Default::default()
    };
    let link_res = handle_crab_component_call(
        &ctx,
        &sess,
        sys_id,
        char_id,
        ship_id,
        beacon_id,
        "linkWithShip",
        "InitiateLink",
        &[],
    )
    .await;
    assert!(link_res.is_ok(), "linking within 10km must succeed");

    let phase = read_pve_special_reg(&ctx, |reg| reg.crab_beacons[&beacon_id].phase);
    assert_eq!(phase, CrabBeaconPhase::Linking);

    let _ = sys_h.set_ball_position(ship_id, DVec3::new(100_000.0, 0.0, 0.0)).await;
    tick_all_crab_beacons(&ctx);
    let after_phase = read_pve_special_reg(&ctx, |reg| reg.crab_beacons[&beacon_id].phase);
    assert_eq!(after_phase, CrabBeaconPhase::Idle, "leaving link range must drop CRAB link");
}

#[tokio::test]
async fn r7_tc_p1_451_observatory_site_despawns_on_boss_kill_with_under_3_fleet() {
    let ctx = ServerContext::new();
    let pochven_sys = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| s.region_id == 10_000_070)
        .map(|s| s.solar_system_id)
        .unwrap();

    let solo_cid = 91_451_001_i64;
    let solo_sid = 92_451_001_i64;
    spawn_observatory_flashpoint(&ctx, pochven_sys).await.unwrap();
    let obs = get_observatory_site_state(&ctx, pochven_sys).unwrap();
    let site_id = obs.site_id;
    let world_ark_id = obs.world_ark_id;
    let site_pos = DVec3::from_array(obs.site_pos);

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().unwrap().clone();
        ch.character_id = solo_cid;
        ch.name = "SoloHunter".into();
        ch.solar_system_id = pochven_sys;
        ch.station_id = None;
        ch.structure_id = None;
        ch.active_ship_id = solo_sid;
        ch.fleet_id = None;
        snap.characters.insert(solo_cid, ch);
        snap.items.insert(
            solo_sid,
            InventoryItemRecord {
                item_id: solo_sid,
                type_id: 587,
                owner_id: solo_cid,
                location_id: i64::from(pochven_sys),
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    let sys_h = ctx.systems.get_or_spawn(pochven_sys);
    let _ = sys_h.spawn_ball(make_ship_ball(solo_sid, 587, site_pos)).await;
    let _ = sys_h.remove_ball(world_ark_id).await;

    on_world_ark_boss_killed(&ctx, pochven_sys, solo_cid).await;

    assert!(
        get_observatory_site_state(&ctx, pochven_sys).is_none(),
        "Observatory site must despawn even when fewer than 3 fleet members kill the boss"
    );
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.service_state_blobs.contains_key(&format!("observatory_site:{site_id}")));
        assert!(snap.service_state_blobs.contains_key(&format!("observatory_respawn_until:{pochven_sys}")));
    });
}

#[tokio::test]
async fn r7_tc_p1_452_pirate_fob_loot_container_item_and_combat_payout_filtering() {
    let ctx = ServerContext::new();
    let sys_id = 30_000_142_u32;
    let spawn_res = spawn_pirate_fob(&ctx, sys_id, FACTION_GURISTAS).await.unwrap();
    let fob_id = spawn_res.dict_get("fobID").and_then(|v| v.as_i64()).unwrap();

    let active_cid = 91_452_001_i64;
    let cloaked_cid = 91_452_002_i64;
    let pod_cid = 91_452_003_i64;
    let active_sid = 92_452_001_i64;
    let cloaked_sid = 92_452_002_i64;
    let pod_sid = 92_452_003_i64;

    let fob_pos = ctx.db.with_mut_snapshot(|snap| {
        let fob_pos = DVec3::from_array(snap.structures[&fob_id].position);
        let tmpl = snap.characters.values().next().unwrap().clone();
        for (cid, sid, tid) in [
            (active_cid, active_sid, 587_u32),
            (cloaked_cid, cloaked_sid, 587_u32),
            (pod_cid, pod_sid, 670_u32),
        ] {
            let mut ch = tmpl.clone();
            ch.character_id = cid;
            ch.name = format!("FobPilot-{cid}");
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = sid;
            ch.balance = IskCents::from_isk(0);
            snap.characters.insert(cid, ch);
            snap.items.insert(
                sid,
                InventoryItemRecord {
                    item_id: sid,
                    type_id: tid,
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
        fob_pos
    });

    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_h
        .spawn_ball_with_combat(
            make_ship_ball(active_sid, 587, fob_pos + DVec3::new(10_000.0, 0.0, 0.0)),
            make_combat_state(1_000.0),
        )
        .await;
    let mut cloaked_cs = make_combat_state(1_000.0);
    cloaked_cs.is_cloaked = true;
    let _ = sys_h
        .spawn_ball_with_combat(
            make_ship_ball(cloaked_sid, 587, fob_pos + DVec3::new(12_000.0, 0.0, 0.0)),
            cloaked_cs,
        )
        .await;
    let _ = sys_h
        .spawn_ball_with_combat(
            make_ship_ball(pod_sid, 670, fob_pos + DVec3::new(8_000.0, 0.0, 0.0)),
            make_combat_state(500.0),
        )
        .await;

    let _ = apply_live_combat_damage_to_pirate_fob(&ctx, sys_id, &sys_h, active_cid, fob_id, 50.0).await;
    let _ = apply_live_combat_damage_to_pirate_fob(&ctx, sys_id, &sys_h, cloaked_cid, fob_id, 50.0).await;
    let _ = apply_live_combat_damage_to_pirate_fob(&ctx, sys_id, &sys_h, pod_cid, fob_id, 50.0).await;

    destroy_pirate_fob_and_payout(&ctx, fob_id, &[active_cid, cloaked_cid, pod_cid])
        .await
        .unwrap();

    let loot_id = fob_id + 90;
    ctx.db.with_snapshot(|snap| {
        let cont = snap.items.get(&loot_id).expect("FOB loot container item must be created");
        assert_eq!(cont.type_id, 17_366);
        assert_eq!(cont.location_id, i64::from(sys_id));
        assert!(cont.singleton);
        assert!(snap.item_positions.contains_key(&loot_id));
        assert!(snap.characters[&active_cid].balance.0 > 0, "uncloaked combat ship must receive payout");
        assert_eq!(snap.characters[&cloaked_cid].balance.0, 0, "cloaked ship must not receive payout");
        assert_eq!(snap.characters[&pod_cid].balance.0, 0, "capsule must not receive payout");
    });
}
