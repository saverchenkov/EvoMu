//! Unit and integration tests for `pve_fob_observatory`.

use super::*;
use crate::router::ServerContext;
use eve_db::clock::VirtualServerClock;
use eve_db::types::FleetRecord;
use eve_db::{ServerClock, WorldRepository};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_sde::{SdeRegistry, SolarSystemId};
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pirate_fob_spawn_defense_live_damage_and_payout() {
    let sys_id: SolarSystemId = 30_000_142;
    let pilot1: i64 = 90_040_301;
    let pilot2: i64 = 90_040_302;
    let ship1: i64 = 1_040_000_301;
    let ship2: i64 = 1_040_000_302;

    let sde = shared_sde();
    let db = Arc::new(WorldRepository::new_empty());
    db.provision_gm_account("fob_p1", "pw", pilot1, "FobP1");
    db.provision_gm_account("fob_p2", "pw", pilot2, "FobP2");
    db.with_mut_snapshot(|snap| {
        for (cid, sid, bal) in [(pilot1, ship1, 100.0), (pilot2, ship2, 200.0)] {
            if let Some(ch) = snap.characters.get_mut(&cid) {
                ch.solar_system_id = sys_id;
                ch.station_id = None;
                ch.active_ship_id = sid;
                ch.balance = eve_db::IskCents::from_f64_isk(bal);
            }
        }
    });
    let ctx = ServerContext::with_sde_and_repository(sde, db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_775_500_000)));
    let sys_handle = ctx.systems.get_or_spawn(sys_id);

    // Spawn Pirate FOB (Blood Raiders 500_012)
    let spawned = spawn_pirate_fob(&ctx, sys_id, 500_012)
        .await
        .expect("spawn_pirate_fob succeeds");
    assert!(!matches!(spawned, PyRep::None));

    let (fob_id, fob_pos) = ctx.db.with_snapshot(|snap| {
        let st = snap
            .structures
            .values()
            .find(|s| s.solar_system_id == sys_id)
            .expect("FOB structure persisted");
        (st.structure_id, DVec3::from_array(st.position))
    });

    for (cid, sid) in [(pilot1, ship1), (pilot2, ship2)] {
        let _ = sys_handle
            .spawn_ball(Ball {
                id: sid,
                type_id: 587,
                position: fob_pos + DVec3::new(15_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 250.0,
                inertia: 1.0,
                mass: 1_200_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            })
            .await;
        record_fob_combat_participant(&ctx, fob_id, cid);
    }

    // Trigger defense fleet response
    let def_res = trigger_fob_defense_response(&ctx, fob_id, &[ship1, ship2])
        .await
        .expect("defense response succeeds");
    assert!(!matches!(def_res, PyRep::None));

    // Apply partial live combat damage
    let partial =
        apply_live_combat_damage_to_pirate_fob(&ctx, sys_id, &sys_handle, pilot1, fob_id, 10_000.0)
            .await;
    assert_eq!(partial.applied_damage, 10_000.0);
    assert!(!partial.destroyed);

    // Apply massive lethal damage -> destroys FOB and pays bounties to both participants
    let lethal = apply_live_combat_damage_to_pirate_fob(
        &ctx,
        sys_id,
        &sys_handle,
        pilot2,
        fob_id,
        500_000_000.0,
    )
    .await;
    assert!(lethal.destroyed);
    assert_eq!(lethal.shield_ratio, 0.0);
    assert_eq!(lethal.armor_ratio, 0.0);
    assert_eq!(lethal.hull_ratio, 0.0);

    let (bal1, bal2) = ctx.db.with_snapshot(|snap| {
        (
            snap.characters[&pilot1].balance,
            snap.characters[&pilot2].balance,
        )
    });
    assert!(bal1 > eve_db::IskCents::from_f64_isk(100.0));
    assert!(bal2 > eve_db::IskCents::from_f64_isk(200.0));

    // Step living world spawner across non-empty universe
    step_living_world_fob_and_observatory_spawns(&ctx).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn observatory_flashpoint_access_gate_and_boss_completion() {
    let mut sde = (*shared_sde()).clone();
    let subcap_tid: u32 = 626;
    if let Some(mut ship_t) = sde.types.values().next().cloned() {
        ship_t.type_id = subcap_tid;
        ship_t.category_id = 6;
        ship_t.group_id = 26;
        ship_t.name = "Caracal".into();
        ship_t.published = true;
        sde.types.insert(subcap_tid, ship_t);
    }
    let sde = Arc::new(sde);
    let sys_id: SolarSystemId = sde
        .solar_systems
        .values()
        .find(|s| s.region_id == 10_000_070)
        .map(|s| s.solar_system_id)
        .expect("SDE has Pochven solar system");
    let p1: i64 = 90_040_401;
    let p2: i64 = 90_040_402;
    let p3: i64 = 90_040_403;
    let ship1: i64 = 1_040_000_401;

    let ship2: i64 = 1_040_000_402;
    let ship3: i64 = 1_040_000_403;

    let db = Arc::new(WorldRepository::new_empty());
    for (i, (cid, sid)) in [(p1, ship1), (p2, ship2), (p3, ship3)].into_iter().enumerate() {
        db.provision_gm_account(&format!("obs_{i}"), "pw", cid, &format!("ObsPilot{i}"));
        db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&cid) {
                ch.solar_system_id = sys_id;
                ch.station_id = None;
                ch.structure_id = None;
                ch.active_ship_id = sid;
                ch.balance = eve_db::IskCents::from_f64_isk(500.0);
            }
            snap.items.insert(
                sid,
                InventoryItem {
                    item_id: sid,
                    type_id: subcap_tid,
                    owner_id: cid,
                    location_id: sys_id as i64,
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
        });
    }
    let ctx = ServerContext::with_sde_and_repository(sde, db);
    ctx.db.with_mut_snapshot(|snap| {
        snap.fleets.insert(
            7701,
            FleetRecord {
                fleet_id: 7701,
                boss_id: p1,
                members: vec![p1, p2, p3],
                wings: Default::default(),
                squads: Default::default(),
                free_move: true,
                is_registered: false,
                ..Default::default()
            },
        );
    });
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_775_550_000)));
    let sys_handle = ctx.systems.get_or_spawn(sys_id);

    // Spawn Observatory Flashpoint
    let spawned = spawn_observatory_flashpoint(&ctx, sys_id)
        .await
        .expect("spawn_observatory_flashpoint succeeds");
    assert!(!matches!(spawned, PyRep::None));

    let obs = get_observatory_site_state(&ctx, sys_id).expect("observatory persisted");
    let gate_pos = DVec3::from_array(obs.gate_pos);

    // Spawn all 3 fleet member subcapital ships right at the acceleration gate
    for sid in [ship1, ship2, ship3] {
        let _ = sys_handle
            .spawn_ball(Ball {
                id: sid,
                type_id: subcap_tid,
                position: gate_pos + DVec3::new(100.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 200.0,
                inertia: 1.0,
                mass: 1_100_000.0,
                radius: 35.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            })
            .await;
    }

    // Validate fleet size check
    assert!(validate_observatory_access(&ctx, p1, &[p1, p2]).is_err());
    validate_observatory_access(&ctx, p1, &[p1, p2, p3]).expect("3+ fleet members accepted");

    let mut session = SessionState::new(404);
    session.user_id = Some(1);
    session.character_id = Some(p1);
    session.solar_system_id = Some(sys_id);
    session.ship_id = Some(ship1);

    // Gate mismatch returns None; matching gate validates fleet & warps
    assert!(try_activate_observatory_gate(&ctx, &session, sys_id, 123).await.is_none());
    let gate_res = try_activate_observatory_gate(&ctx, &session, sys_id, obs.site_id + 2)
        .await
        .expect("gate recognized");
    assert!(gate_res.is_ok(), "gate activation failed: {:?}", gate_res);

    // World Ark boss kill & collective completion payout
    on_world_ark_boss_killed(&ctx, sys_id, p1).await;
    let comp = complete_observatory_flashpoint(&ctx, obs.site_id, &[p1, p2, p3])
        .expect("complete_observatory_flashpoint succeeds");
    assert!(!matches!(comp, PyRep::None));
    assert!(
        ctx.db.with_snapshot(|snap| snap.characters[&p1].balance)
            > eve_db::IskCents::from_f64_isk(500.0)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pirate_fob_damage_tiers_single_trigger_eligibility_filters_and_step_spawner() {
    let sde = shared_sde();
    let db = Arc::new(WorldRepository::new_empty());
    let ctx = ServerContext::with_sde_and_repository(sde, db);
    let clock = Arc::new(VirtualServerClock::with_fixed_base(1_776_000_000));
    ctx.set_clock(clock.clone());

    // Pick a 0.0 < sec < 0.8 even system (Guristas parity) and an odd system (Blood Raider parity) and >= 0.8 (Jita 30_000_142)
    let even_midsec = ctx
        .sde
        .solar_systems
        .values()
        .filter(|s| {
            s.region_id != POCHVEN_REGION_ID
                && s.security_status > 0.05
                && s.security_status < 0.75
                && s.solar_system_id % 2 == 0
        })
        .min_by_key(|s| s.solar_system_id)
        .expect("even midsec system")
        .solar_system_id;
    let odd_midsec = ctx
        .sde
        .solar_systems
        .values()
        .filter(|s| {
            s.region_id != POCHVEN_REGION_ID
                && s.security_status > 0.05
                && s.security_status < 0.75
                && s.solar_system_id % 2 == 1
                && s.solar_system_id != even_midsec
        })
        .min_by_key(|s| s.solar_system_id)
        .expect("odd midsec system")
        .solar_system_id;
    let hisec_high = 30_000_142u32; // Jita sec ~0.95 (>= 0.8)

    // Docked ship / empty space -> step_living_world_fob_and_observatory_spawns spawns nothing
    let pilot_docked = 90_041_001i64;
    ctx.db.provision_gm_account("fob_dock", "pw", pilot_docked, "FobDock");
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&pilot_docked).unwrap();
        ch.solar_system_id = even_midsec;
        ch.station_id = Some(60_003_760);
        ch.active_ship_id = 1_041_000_001;
    });
    let _ = ctx.systems.get_or_spawn(even_midsec);
    let _ = ctx.systems.get_or_spawn(hisec_high);
    step_living_world_fob_and_observatory_spawns(&ctx).await;
    ctx.db.with_snapshot(|snap| {
        assert!(snap.structures.is_empty(), "docked pilot must not spawn FOB");
    });

    // Undock pilot in high-sec >= 0.8 -> still spawns NO FOB
    let pilot_hisec = 90_041_002i64;
    ctx.db.provision_gm_account("fob_hi", "pw", pilot_hisec, "FobHi");
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&pilot_hisec).unwrap();
        ch.solar_system_id = hisec_high;
        ch.station_id = None;
        ch.active_ship_id = 1_041_000_002;
    });
    step_living_world_fob_and_observatory_spawns(&ctx).await;
    ctx.db.with_snapshot(|snap| {
        assert!(
            !snap.structures.values().any(|s| s.solar_system_id == hisec_high),
            "sec >= 0.8 must not spawn FOB"
        );
    });

    // Put even_midsec on active FOB cooldown (`until = now + 500`), verify suppressed;
    // put undocked pilot in odd_midsec -> spawns Blood Raider FOB automatically!
    let now_u = ctx.now_unix_secs();
    let pilot_odd = 90_041_003i64;
    ctx.db.provision_gm_account("fob_odd", "pw", pilot_odd, "FobOdd");
    ctx.db.with_mut_snapshot(|snap| {
        snap.fob_observatory_state
            .fob_respawn_until
            .insert(even_midsec, now_u + 500);
        let ch = snap.characters.get_mut(&pilot_docked).unwrap();
        ch.station_id = None; // undock in even_midsec (on cooldown!)
        let ch_odd = snap.characters.get_mut(&pilot_odd).unwrap();
        ch_odd.solar_system_id = odd_midsec;
        ch_odd.station_id = None;
        ch_odd.active_ship_id = 1_041_000_003;
    });
    let _ = ctx.systems.get_or_spawn(odd_midsec);
    step_living_world_fob_and_observatory_spawns(&ctx).await;
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.structures.values().any(|s| s.solar_system_id == even_midsec));
        let odd_fob = snap
            .structures
            .values()
            .find(|s| s.solar_system_id == odd_midsec)
            .expect("odd midsec spawns Blood Raider FOB");
        assert_eq!(odd_fob.type_id, FOB_BLOOD_RAIDER_TYPE_ID);
    });

    // Advance clock past even_midsec cooldown -> spawns Guristas FOB (`type_id = 46_363`)!
    clock.advance_secs(501);
    step_living_world_fob_and_observatory_spawns(&ctx).await;
    let (gur_fob_id, gur_pos) = ctx.db.with_snapshot(|snap| {
        let gf = snap
            .structures
            .values()
            .find(|s| s.solar_system_id == even_midsec)
            .expect("even midsec spawns Guristas FOB after cooldown");
        assert_eq!(gf.type_id, FOB_GURISTAS_TYPE_ID);
        assert_eq!(snap.fob_observatory_state.fob_factions[&gf.structure_id], FACTION_GURISTAS);
        (gf.structure_id, DVec3::from_array(gf.position))
    });

    // Test incremental damage ratios across shield (500k) -> armor (500k) -> hull (500k)
    // and verify fob_defense_triggered fires ONLY on first hit
    let even_handle = ctx.systems.get_or_spawn(even_midsec);
    let p_ok = 90_041_101i64;
    let p_offgrid = 90_041_102i64;
    let p_cloaked = 90_041_103i64;
    let p_dead = 90_041_104i64;
    let p_capsule = 90_041_105i64;
    let p_bystander = 90_041_106i64;

    for (cid, offset, cloaked, dead, ship_tid) in [
        (p_ok, DVec3::new(249_999.9, 0.0, 0.0), false, false, 587u32), // ULP-safe 250km boundary -> ELIGIBLE
        (p_offgrid, DVec3::new(250_001.0, 0.0, 0.0), false, false, 587), // 1m beyond 250km -> EXCLUDED
        (p_cloaked, DVec3::new(10_000.0, 0.0, 0.0), true, false, 587),
        (p_dead, DVec3::new(10_000.0, 0.0, 0.0), false, true, 587),
        (p_capsule, DVec3::new(10_000.0, 0.0, 0.0), false, false, 670), // capsule -> EXCLUDED
        (p_bystander, DVec3::new(10_000.0, 0.0, 0.0), false, false, 587), // not in fob_combatants
    ] {
        ctx.db.provision_gm_account(&format!("fob_el_{cid}"), "pw", cid, &format!("FobEl{cid}"));
        let sid = 1_042_000_000 + (cid % 1_000);
        ctx.db.with_mut_snapshot(|snap| {
            let ch = snap.characters.get_mut(&cid).unwrap();
            ch.solar_system_id = even_midsec;
            ch.station_id = None;
            ch.active_ship_id = sid;
            ch.balance = IskCents::from_isk(100);
            snap.items.insert(
                sid,
                InventoryItem {
                    item_id: sid,
                    type_id: ship_tid,
                    owner_id: cid,
                    location_id: i64::from(even_midsec),
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
        });
        let mut cs = CombatState::default();
        cs.is_cloaked = cloaked;
        cs.structure_hp = if dead { 0.0 } else { 100.0 };
        let _ = even_handle
            .spawn_ball_with_combat(make_site_npc_ball(sid, ship_tid, gur_pos + offset), cs)
            .await;
        if cid != p_bystander {
            record_fob_combat_participant(&ctx, gur_fob_id, cid);
        }
    }

    // First hit: 100k shield damage -> triggers defense once, ratios (0.8, 1.0, 1.0)
    let hit1 = apply_live_combat_damage_to_pirate_fob(
        &ctx,
        even_midsec,
        &even_handle,
        p_ok,
        gur_fob_id,
        100_000.0,
    )
    .await;
    assert_eq!(hit1.applied_damage, 100_000.0);
    assert!(!hit1.destroyed);
    assert!((hit1.shield_ratio - 0.8).abs() < 1e-6);
    assert!((hit1.armor_ratio - 1.0).abs() < 1e-6);
    assert!((hit1.hull_ratio - 1.0).abs() < 1e-6);
    assert!(ctx.db.with_snapshot(|s| s.fob_observatory_state.fob_defense_triggered.contains(&gur_fob_id)));
    // Defender Pith Dismantler (16_984) balls spawned
    assert!(even_handle.get_ball_snapshot(gur_fob_id + 11).await.unwrap().is_some());

    // Second hit: 600k breaks remaining 400k shield + 200k armor -> ratios (0.0, 0.6, 1.0)
    let hit2 = apply_live_combat_damage_to_pirate_fob(
        &ctx,
        even_midsec,
        &even_handle,
        p_ok,
        gur_fob_id,
        600_000.0,
    )
    .await;
    assert!(!hit2.destroyed);
    assert_eq!(hit2.shield_ratio, 0.0);
    assert!((hit2.armor_ratio - 0.6).abs() < 1e-6);
    assert!((hit2.hull_ratio - 1.0).abs() < 1e-6);

    // Third hit: 600k breaks remaining 300k armor + 300k hull -> ratios (0.0, 0.0, 0.4)
    let hit3 = apply_live_combat_damage_to_pirate_fob(
        &ctx,
        even_midsec,
        &even_handle,
        p_ok,
        gur_fob_id,
        600_000.0,
    )
    .await;
    assert!(!hit3.destroyed);
    assert_eq!(hit3.shield_ratio, 0.0);
    assert_eq!(hit3.armor_ratio, 0.0);
    assert!((hit3.hull_ratio - 0.4).abs() < 1e-6);

    // Lethal 200k hull kill -> triggers destroy_pirate_fob_and_payout paying ONLY p_ok (30M ISK) & dropping Dread Guristas loot 13_865
    let hit4 = apply_live_combat_damage_to_pirate_fob(
        &ctx,
        even_midsec,
        &even_handle,
        p_ok,
        gur_fob_id,
        200_000.0,
    )
    .await;
    assert!(hit4.destroyed);
    assert_eq!(hit4.hull_ratio, 0.0);
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.characters[&p_ok].balance, IskCents::from_isk(100 + FOB_BASE_PILOT_BOUNTY_ISK));
        for exc in [p_offgrid, p_cloaked, p_dead, p_capsule, p_bystander] {
            assert_eq!(snap.characters[&exc].balance, IskCents::from_isk(100), "pilot {exc} should not be paid");
        }
        assert_eq!(snap.items[&(gur_fob_id + 90)].type_id, 17_366);
        assert_eq!(snap.items[&(gur_fob_id + 91)].type_id, 13_865);
        assert!(!snap.fob_observatory_state.fob_combatants.contains_key(&gur_fob_id));
        assert!(snap.fob_observatory_state.fob_respawn_until.contains_key(&even_midsec));
    });

    // Missing FOB damage returns destroyed fallback without panicking
    let missing_res = apply_live_combat_damage_to_pirate_fob(&ctx, even_midsec, &even_handle, p_ok, 999_888_777, 123.0).await;
    assert_eq!(
        missing_res,
        PirateFobDamageOutcome {
            applied_damage: 123.0,
            destroyed: true,
            shield_ratio: 0.0,
            armor_ratio: 0.0,
            hull_ratio: 0.0,
            invulnerable: true,
        }
    );

    // Double destroy / missing trigger error guards
    assert_eq!(destroy_pirate_fob_and_payout(&ctx, gur_fob_id, &[]).await.unwrap_err().code, "FobAlreadyDestroyed");
    assert_eq!(trigger_fob_defense_response(&ctx, 999_888_777, &[]).await.unwrap_err().code, "FobNotFound");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn observatory_gate_guards_exact_range_boundary_and_world_ark_auto_clear() {
    let mut sde = (*shared_sde()).clone();
    let cap_tid = 19_720u32; // Revelation (group 485 Dreadnought)
    let subcap_tid = 626u32; // Caracal (group 26 Cruiser)
    if let Some(mut base) = sde.types.values().next().cloned() {
        base.type_id = subcap_tid;
        base.category_id = 6;
        base.group_id = 26;
        base.published = true;
        sde.types.insert(subcap_tid, base.clone());
        base.type_id = cap_tid;
        base.group_id = 485;
        sde.types.insert(cap_tid, base);
    }
    let sde = Arc::new(sde);
    let poch_systems: Vec<SolarSystemId> = sde
        .solar_systems
        .values()
        .filter(|s| s.region_id == POCHVEN_REGION_ID)
        .map(|s| s.solar_system_id)
        .take(2)
        .collect();
    let sys_id = poch_systems[0];

    let db = Arc::new(WorldRepository::new_empty());
    let p1 = 90_042_001i64;
    let p2 = 90_042_002i64;
    let p3 = 90_042_003i64;
    let s1 = 1_043_000_001i64;
    let s2 = 1_043_000_002i64;
    let s3 = 1_043_000_003i64;
    for (cid, sid) in [(p1, s1), (p2, s2), (p3, s3)] {
        db.provision_gm_account(&format!("og_{cid}"), "pw", cid, &format!("Og{cid}"));
        db.with_mut_snapshot(|snap| {
            let ch = snap.characters.get_mut(&cid).unwrap();
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = sid;
            ch.balance = IskCents::from_isk(0);
            ch.standings.insert(FACTION_TRIGLAVIAN, 0.0); // exact 0.0 boundary allowed
            snap.items.insert(
                sid,
                InventoryItem {
                    item_id: sid,
                    type_id: subcap_tid,
                    owner_id: cid,
                    location_id: i64::from(sys_id),
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
        });
    }
    let ctx = ServerContext::with_sde_and_repository(sde, db);
    ctx.db.with_mut_snapshot(|snap| {
        snap.fleets.insert(
            8801,
            FleetRecord {
                fleet_id: 8801,
                boss_id: p1,
                members: vec![p1, p2, p3],
                free_move: true,
                ..Default::default()
            },
        );
    });
    let clock = Arc::new(VirtualServerClock::with_fixed_base(1_776_100_000));
    ctx.set_clock(clock.clone());

    spawn_observatory_flashpoint(&ctx, sys_id).await.unwrap();
    let obs = get_observatory_site_state(&ctx, sys_id).unwrap();
    let gate_pos = DVec3::from_array(obs.gate_pos);
    let sys_handle = ctx.systems.get_or_spawn(sys_id);

    for sid in [s2, s3] {
        sys_handle
            .spawn_ball(make_site_npc_ball(sid, subcap_tid, gate_pos + DVec3::new(500.0, 0.0, 0.0)))
            .await
            .unwrap();
    }

    // Session guards: unauth, docked session, wrong solar system, missing active ship, ship not in space
    let bad_sess = SessionState::new(1);
    assert!(try_activate_observatory_gate(&ctx, &bad_sess, sys_id, obs.gate_id).await.unwrap().is_err());

    let mut sess = SessionState::new(2);
    sess.user_id = Some(ctx.db.with_snapshot(|s| s.characters[&p1].user_id));
    sess.character_id = Some(p1);
    sess.station_id = Some(60_003_760);
    assert_eq!(
        try_activate_observatory_gate(&ctx, &sess, sys_id, obs.gate_id).await.unwrap().unwrap_err().code,
        "CantDoThatWhileDocked"
    );
    sess.station_id = None;
    sess.solar_system_id = Some(30_000_142);
    assert_eq!(
        try_activate_observatory_gate(&ctx, &sess, sys_id, obs.gate_id).await.unwrap().unwrap_err().code,
        "NotInSolarSystem"
    );
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = None;
    assert_eq!(
        try_activate_observatory_gate(&ctx, &sess, sys_id, obs.gate_id).await.unwrap().unwrap_err().code,
        "NoActiveShip"
    );
    sess.ship_id = Some(s1);
    assert_eq!(
        try_activate_observatory_gate(&ctx, &sess, sys_id, obs.gate_id).await.unwrap().unwrap_err().code,
        "ShipNotInSpace"
    );

    // Distance boundary: 2_501m fails OutsideOperationalRange; 2_500m exact boundary passes!
    sys_handle
        .spawn_ball(make_site_npc_ball(s1, subcap_tid, gate_pos + DVec3::new(2_501.0, 0.0, 0.0)))
        .await
        .unwrap();
    assert_eq!(
        try_activate_observatory_gate(&ctx, &sess, sys_id, obs.gate_id).await.unwrap().unwrap_err().code,
        "OutsideOperationalRange"
    );

    assert!(sys_handle
        .set_ball_position(s1, gate_pos + DVec3::new(GATE_ACTIVATION_RANGE_M, 0.0, 0.0))
        .await);

    // Capital ship item type fails ShipClassRestricted
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&s1).unwrap().type_id = cap_tid;
    });
    assert_eq!(
        try_activate_observatory_gate(&ctx, &sess, sys_id, obs.gate_id).await.unwrap().unwrap_err().code,
        "ShipClassRestricted"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&s1).unwrap().type_id = subcap_tid;
    });

    // Negative standing (-0.01) fails StandingAccessDenied even with 3 fleet members
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&p2).unwrap().standings.insert(FACTION_TRIGLAVIAN, -0.01);
    });
    assert_eq!(
        try_activate_observatory_gate(&ctx, &sess, sys_id, obs.gate_id).await.unwrap().unwrap_err().code,
        "StandingAccessDenied"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&p2).unwrap().standings.insert(FACTION_TRIGLAVIAN, 0.0);
    });

    // Exact 2_500m boundary + exact 0.0 standing succeeds and records pocket_entrant + moves ball!
    let ok_res = try_activate_observatory_gate(&ctx, &sess, sys_id, obs.gate_id).await.unwrap().unwrap();
    assert_eq!(ok_res.dict_get("accessGranted"), Some(&PyRep::Bool(true)));
    let obs_after = get_observatory_site_state(&ctx, sys_id).unwrap();
    assert!(obs_after.pocket_entrants.contains(&p1));
    let moved = sys_handle.get_ball_snapshot(s1).await.unwrap().unwrap().1;
    let expected_dest = DVec3::from_array(obs.site_pos) + DVec3::new(0.0, 0.0, -5_000.0);
    assert!(moved.position.distance(expected_dest) < 1e-3);

    // Only 1 pocket entrant so far: <= 0 killer_char_id and < 3 pocket entrants both leave site active
    on_world_ark_boss_killed(&ctx, sys_id, 0).await;
    on_world_ark_boss_killed(&ctx, sys_id, p1).await;
    assert!(get_observatory_site_state(&ctx, sys_id).is_some());

    // Have p2 and p3 activate the gate (within 500m) so all 3 fleet members are pocket entrants
    for (idx, (cid, sid)) in [(p2, s2), (p3, s3)].into_iter().enumerate() {
        let mut s_mem = SessionState::new(10 + (idx as u64));
        s_mem.user_id = Some(ctx.db.with_snapshot(|s| s.characters[&cid].user_id));
        s_mem.character_id = Some(cid);
        s_mem.solar_system_id = Some(sys_id);
        s_mem.ship_id = Some(sid);
        let mem_res = try_activate_observatory_gate(&ctx, &s_mem, sys_id, obs.gate_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(mem_res.dict_get("accessGranted"), Some(&PyRep::Bool(true)));
    }
    assert_eq!(get_observatory_site_state(&ctx, sys_id).unwrap().pocket_entrants.len(), 3);

    on_world_ark_boss_killed(&ctx, sys_id, p1).await;
    assert!(get_observatory_site_state(&ctx, sys_id).is_none());
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.dungeon_runtime.instances.contains_key(&obs.dungeon_instance_id));
        for cid in [p1, p2, p3] {
            assert_eq!(snap.characters[&cid].loyalty_points.get(&CORP_DED).copied(), Some(OBSERVATORY_BASE_PILOT_LP));
            assert_eq!(snap.characters[&cid].balance, IskCents::from_isk(OBSERVATORY_BASE_PILOT_ISK));
        }
        assert!(snap.fob_observatory_state.observatory_respawn_until.contains_key(&sys_id));
    });

    // Verify step_living_world_fob_and_observatory_spawns respects observatory cooldown until expiry!
    step_living_world_fob_and_observatory_spawns(&ctx).await;
    assert!(get_observatory_site_state(&ctx, sys_id).is_none());
    clock.advance_secs(14_401);
    step_living_world_fob_and_observatory_spawns(&ctx).await;
    assert!(get_observatory_site_state(&ctx, sys_id).is_some());
}
