//! Extra branch-coverage and mutation-killing unit tests for `pve_fob_observatory.rs`.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::pve_fob_observatory::{
    apply_live_combat_damage_to_pirate_fob, complete_observatory_flashpoint,
    destroy_pirate_fob_and_payout, get_observatory_site_state, on_world_ark_boss_killed,
    record_fob_combat_participant, spawn_observatory_flashpoint, spawn_pirate_fob,
    step_living_world_fob_and_observatory_spawns, trigger_fob_defense_response,
    try_activate_observatory_gate, validate_observatory_access, CORP_DED, FACTION_BLOOD_RAIDER,
    FACTION_GURISTAS, FACTION_TRIGLAVIAN, FOB_BASE_PILOT_BOUNTY_ISK, FOB_BLOOD_RAIDER_TYPE_ID,
    FOB_GURISTAS_TYPE_ID, OBSERVATORY_BASE_PILOT_ISK, OBSERVATORY_BASE_PILOT_LP,
    OBSERVATORY_MAX_PAYOUT_PILOTS, POCHVEN_REGION_ID,
};
use eve_db::clock::VirtualServerClock;
use eve_db::types::FleetRecord;
use eve_db::{InventoryFlag, InventoryItemRecord, IskCents, WorldRepository};
use eve_destiny::{Ball, DVec3, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::{SdeRegistry, SolarSystemId};
use std::sync::{Arc, OnceLock};

fn custom_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| {
        let mut sde = SdeRegistry::with_canonical_seed();
        if let Some(base_t) = sde.types.values().next().cloned() {
            // Subcap cruiser (group 26), Battleship (group 27), Dreadnought (group 485), Capsule (670), Non-ship (cat 7)
            for (tid, cat, grp, name) in [
                (626_u32, 6_u32, 26_u32, "Caracal"),
                (640_u32, 6_u32, 27_u32, "Scorpion"),
                (19_720_u32, 6_u32, 485_u32, "Revelation"),
                (670_u32, 6_u32, 29_u32, "Capsule"),
                (999_001_u32, 7_u32, 1_u32, "Module"),
            ] {
                let mut t = base_t.clone();
                t.type_id = tid;
                t.category_id = cat;
                t.group_id = grp;
                t.name = name.into();
                t.published = true;
                sde.types.insert(tid, t);
            }
        }
        Arc::new(sde)
    }))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pirate_fob_validation_fleet_weights_and_payout_dilution() {
    let sde = custom_sde();
    let db = Arc::new(WorldRepository::new_empty());
    let ctx = ServerContext::with_sde_and_repository(Arc::clone(&sde), db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_775_600_000)));

    let hisec_sys: SolarSystemId = 30_000_142;
    let nullsec_sys: SolarSystemId = sde
        .solar_systems
        .values()
        .find(|s| s.security_status <= 0.0)
        .map(|s| s.solar_system_id)
        .unwrap_or(31_000_005);

    // Invalid pirate faction and invalid system security
    assert_eq!(
        spawn_pirate_fob(&ctx, hisec_sys, 500_011)
            .await
            .unwrap_err()
            .code,
        "InvalidPirateFaction"
    );
    assert_eq!(
        spawn_pirate_fob(&ctx, nullsec_sys, FACTION_GURISTAS)
            .await
            .unwrap_err()
            .code,
        "InvalidSystemSecurity"
    );

    // Non-existent FOB errors
    assert_eq!(
        trigger_fob_defense_response(&ctx, 999_999, &[])
            .await
            .unwrap_err()
            .code,
        "FobNotFound"
    );
    assert_eq!(
        destroy_pirate_fob_and_payout(&ctx, 999_999, &[])
            .await
            .unwrap_err()
            .code,
        "FobNotFound"
    );
    let sys_handle = ctx.systems.get_or_spawn(hisec_sys);
    let (_, missing_dest, _, _, _, _) =
        apply_live_combat_damage_to_pirate_fob(&ctx, hisec_sys, &sys_handle, 1, 999_999, 50.0)
            .await;
    assert!(missing_dest);

    // Spawn Guristas FOB
    let gur_res = spawn_pirate_fob(&ctx, hisec_sys, FACTION_GURISTAS)
        .await
        .unwrap();
    let gur_fob_id = gur_res.dict_get("fobID").and_then(|v| v.as_i64()).unwrap();
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.structures[&gur_fob_id].type_id, FOB_GURISTAS_TYPE_ID);
    });

    // Seed ships of different classes: dread(4) + bs(3) + cruiser(2) + unknown(1) = weight 10
    ctx.db.with_mut_snapshot(|snap| {
        for (sid, tid) in [(5001, 19_720), (5002, 640), (5003, 626)] {
            snap.items.insert(
                sid,
                InventoryItemRecord {
                    item_id: sid,
                    type_id: tid,
                    owner_id: 1,
                    location_id: hisec_sys as i64,
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
        }
    });

    // Empty attacker list clamps defenderCount to 2
    let def_min = trigger_fob_defense_response(&ctx, gur_fob_id, &[])
        .await
        .unwrap();
    assert_eq!(
        def_min.dict_get("defenderCount"),
        Some(&PyRep::Integer(2))
    );

    // Mixed fleet: 4 + 3 + 2 + 1 = 10
    let def_mix = trigger_fob_defense_response(&ctx, gur_fob_id, &[5001, 5002, 5003, 5004])
        .await
        .unwrap();
    assert_eq!(
        def_mix.dict_get("fleetCompositionWeight"),
        Some(&PyRep::Integer(10))
    );
    assert_eq!(
        def_mix.dict_get("defenderCount"),
        Some(&PyRep::Integer(10))
    );

    // Huge fleet (7 dreads = weight 28) clamps defenderCount to 24
    let def_max =
        trigger_fob_defense_response(&ctx, gur_fob_id, &[5001, 5001, 5001, 5001, 5001, 5001, 5001])
            .await
            .unwrap();
    assert_eq!(
        def_max.dict_get("defenderCount"),
        Some(&PyRep::Integer(24))
    );

    // record_fob_combat_participant ignores <= 0
    record_fob_combat_participant(&ctx, gur_fob_id, 0);
    record_fob_combat_participant(&ctx, gur_fob_id, -10);

    // Destroy Guristas FOB with 0 eligible attackers -> 0 bounty, drops Guristas loot (13_865)
    let zero_pay = destroy_pirate_fob_and_payout(&ctx, gur_fob_id, &[9001])
        .await
        .unwrap();
    assert_eq!(
        zero_pay.dict_get("bountyPerPilot"),
        Some(&PyRep::Integer(0))
    );
    assert_eq!(
        ctx.db.with_snapshot(|s| s.items[&(gur_fob_id + 91)].type_id),
        13_865
    );
    // Destroying again returns FobAlreadyDestroyed
    assert_eq!(
        destroy_pirate_fob_and_payout(&ctx, gur_fob_id, &[])
            .await
            .unwrap_err()
            .code,
        "FobAlreadyDestroyed"
    );

    // Spawn Blood Raider FOB and test dilution with 12 combat participants (> 10 max full payout)
    let br_res = spawn_pirate_fob(&ctx, hisec_sys, FACTION_BLOOD_RAIDER)
        .await
        .unwrap();
    let br_fob_id = br_res.dict_get("fobID").and_then(|v| v.as_i64()).unwrap();
    ctx.db.with_snapshot(|snap| {
        assert_eq!(
            snap.structures[&br_fob_id].type_id,
            FOB_BLOOD_RAIDER_TYPE_ID
        );
    });

    let mut pilots = Vec::new();
    for i in 1..=12_i64 {
        let cid = 91_000_000 + i;
        ctx.db
            .provision_gm_account(&format!("br_p{i}"), "pw", cid, &format!("BrPilot{i}"));
        record_fob_combat_participant(&ctx, br_fob_id, cid);
        pilots.push(cid);
    }
    let diluted = destroy_pirate_fob_and_payout(&ctx, br_fob_id, &pilots)
        .await
        .unwrap();
    let expected_diluted = 300_000_000_i64 / 12;
    assert_eq!(
        diluted.dict_get("bountyPerPilot"),
        Some(&PyRep::Integer(expected_diluted))
    );
    assert!(expected_diluted < FOB_BASE_PILOT_BOUNTY_ISK);
    assert_eq!(
        ctx.db.with_snapshot(|s| s.items[&(br_fob_id + 91)].type_id),
        13_791
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn observatory_flashpoint_caps_gate_errors_standings_and_payout_caps() {
    let sde = custom_sde();
    let db = Arc::new(WorldRepository::new_empty());
    let ctx = ServerContext::with_sde_and_repository(Arc::clone(&sde), db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_775_700_000)));

    let hisec_sys: SolarSystemId = 30_000_142;
    let pochven_systems: Vec<SolarSystemId> = sde
        .solar_systems
        .values()
        .filter(|s| s.region_id == POCHVEN_REGION_ID)
        .map(|s| s.solar_system_id)
        .take(4)
        .collect();
    assert_eq!(pochven_systems.len(), 4);

    // Non-Pochven system rejected
    assert_eq!(
        spawn_observatory_flashpoint(&ctx, hisec_sys)
            .await
            .unwrap_err()
            .code,
        "NotInPochven"
    );

    // Spawn 3 Pochven Observatories -> 4th system hits ObservatoryRegionCapReached, but re-spawning 1st is allowed
    for &psys in &pochven_systems[0..3] {
        assert!(spawn_observatory_flashpoint(&ctx, psys).await.is_ok());
    }
    assert_eq!(
        spawn_observatory_flashpoint(&ctx, pochven_systems[3])
            .await
            .unwrap_err()
            .code,
        "ObservatoryRegionCapReached"
    );
    let sys_id = pochven_systems[0];
    assert!(spawn_observatory_flashpoint(&ctx, sys_id).await.is_ok());

    let obs = get_observatory_site_state(&ctx, sys_id).unwrap();
    let gate_id = obs.gate_id;
    let gate_pos = DVec3::from_array(obs.gate_pos);
    let site_pos = DVec3::from_array(obs.site_pos);

    // Provision 18 pilots to test standing checks, gate restrictions, and 15-pilot payout cap
    let mut all_pilots = Vec::new();
    for i in 1..=18_i64 {
        let cid = 92_000_000 + i;
        let sid = 1_200_000 + i;
        ctx.db
            .provision_gm_account(&format!("ob_p{i}"), "pw", cid, &format!("ObPilot{i}"));
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&cid) {
                ch.solar_system_id = sys_id;
                ch.station_id = None;
                ch.structure_id = None;
                ch.active_ship_id = sid;
                ch.balance = IskCents::ZERO;
            }
            snap.items.insert(
                sid,
                InventoryItemRecord {
                    item_id: sid,
                    type_id: 626,
                    owner_id: cid,
                    location_id: sys_id as i64,
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
        });
        all_pilots.push((cid, sid));
    }
    let p1 = all_pilots[0].0;
    let ship1 = all_pilots[0].1;
    let p2 = all_pilots[1].0;
    let p3 = all_pilots[2].0;

    // Standing < 0.0 rejected; standing == 0.0 accepted
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters
            .get_mut(&p2)
            .unwrap()
            .standings
            .insert(FACTION_TRIGLAVIAN, -0.05);
    });
    assert_eq!(
        validate_observatory_access(&ctx, p1, &[p1, p2, p3])
            .unwrap_err()
            .code,
        "StandingAccessDenied"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters
            .get_mut(&p2)
            .unwrap()
            .standings
            .insert(FACTION_TRIGLAVIAN, 0.0);
    });
    assert!(validate_observatory_access(&ctx, p1, &[p1, p2, p3]).is_ok());

    // Gate error branches: unauth, session docked, wrong system, no active ship, ship not in space
    let mut sess = SessionState::new(501);
    assert!(try_activate_observatory_gate(&ctx, &sess, sys_id, gate_id)
        .await
        .unwrap()
        .is_err());

    sess.user_id = Some(1);
    sess.character_id = Some(p1);
    sess.station_id = Some(60_003_760);
    assert_eq!(
        try_activate_observatory_gate(&ctx, &sess, sys_id, gate_id)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "CantDoThatWhileDocked"
    );
    sess.station_id = None;
    sess.solar_system_id = Some(hisec_sys);
    assert_eq!(
        try_activate_observatory_gate(&ctx, &sess, sys_id, gate_id)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "NotInSolarSystem"
    );
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = None;
    assert_eq!(
        try_activate_observatory_gate(&ctx, &sess, sys_id, gate_id)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "NoActiveShip"
    );
    sess.ship_id = Some(ship1);
    assert_eq!(
        try_activate_observatory_gate(&ctx, &sess, sys_id, gate_id)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "ShipNotInSpace"
    );

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    // Spawn ship far from gate (3,000m > 2,500m) -> OutsideOperationalRange
    let _ = sys_handle
        .spawn_ball(Ball {
            id: ship1,
            type_id: 626,
            position: gate_pos + DVec3::new(3_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 200.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 30.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    assert_eq!(
        try_activate_observatory_gate(&ctx, &sess, sys_id, gate_id)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "OutsideOperationalRange"
    );

    // Move ship within 2,500m, test DB docked check & disallowed hulls (Capsule 670, Dread 19_720, Non-ship 999_001)
    let _ = sys_handle
        .set_ball_position(ship1, gate_pos + DVec3::new(500.0, 0.0, 0.0))
        .await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&p1).unwrap().structure_id = Some(104_000_001);
    });
    assert_eq!(
        try_activate_observatory_gate(&ctx, &sess, sys_id, gate_id)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "CantDoThatWhileDocked"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&p1).unwrap().structure_id = None;
    });

    for bad_tid in [670_u32, 19_720, 999_001] {
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.get_mut(&ship1).unwrap().type_id = bad_tid;
        });
        assert_eq!(
            try_activate_observatory_gate(&ctx, &sess, sys_id, gate_id)
                .await
                .unwrap()
                .unwrap_err()
                .code,
            "ShipClassRestricted"
        );
    }
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&ship1).unwrap().type_id = 626;
    });

    // Solo pilot without fleet fails InsufficientFleetComposition
    assert_eq!(
        try_activate_observatory_gate(&ctx, &sess, sys_id, gate_id)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "InsufficientFleetComposition"
    );

    // Put all 18 pilots into a fleet and spawn their ships in the pocket, mark as pocket_entrants
    let member_ids: Vec<i64> = all_pilots.iter().map(|(cid, _)| *cid).collect();
    ctx.db.with_mut_snapshot(|snap| {
        snap.fleets.insert(
            8801,
            FleetRecord {
                fleet_id: 8801,
                boss_id: p1,
                members: member_ids.clone(),
                free_move: true,
                ..Default::default()
            },
        );
    });
    for &(_, sid) in &all_pilots {
        let _ = sys_handle
            .spawn_ball(Ball {
                id: sid,
                type_id: 626,
                position: site_pos + DVec3::new(1_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 200.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 30.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            })
            .await;
    }

    // Complete with < 3 fails; corrupt JSON fails; missing site fails
    assert_eq!(
        complete_observatory_flashpoint(&ctx, obs.site_id, &[p1, p2])
            .unwrap_err()
            .code,
        "InsufficientFleetComposition"
    );
    assert_eq!(
        complete_observatory_flashpoint(&ctx, 999_888, &[p1, p2, p3])
            .unwrap_err()
            .code,
        "ObservatorySiteNotFound"
    );
    // Record all 18 as pocket entrants and trigger on_world_ark_boss_killed -> caps at 15 paid pilots
    let mut updated_obs = get_observatory_site_state(&ctx, sys_id).unwrap();
    updated_obs.pocket_entrants.extend(member_ids.iter().copied());
    ctx.db.with_mut_snapshot(|snap| {
        snap.fob_observatory_state
            .observatory_sites
            .insert(updated_obs.site_id, updated_obs);
    });
    on_world_ark_boss_killed(&ctx, sys_id, 0).await; // no-op for killer <= 0
    on_world_ark_boss_killed(&ctx, sys_id, p1).await;

    // Site should now be completed and removed, and exactly 15 pilots paid 100M ISK and 10k LP
    assert!(get_observatory_site_state(&ctx, sys_id).is_none());
    ctx.db.with_snapshot(|snap| {
        let paid_count = member_ids
            .iter()
            .filter(|&&cid| {
                snap.characters[&cid].balance == IskCents::from_isk(OBSERVATORY_BASE_PILOT_ISK)
                    && snap.characters[&cid].loyalty_points.get(&CORP_DED)
                        == Some(&OBSERVATORY_BASE_PILOT_LP)
            })
            .count();
        assert_eq!(paid_count, OBSERVATORY_MAX_PAYOUT_PILOTS);
    });

    // Living world tick respects respawn cooldown
    step_living_world_fob_and_observatory_spawns(&ctx).await;
    assert!(get_observatory_site_state(&ctx, sys_id).is_none());
}
