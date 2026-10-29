//! Integration tests for Faction Warfare Frontlines, Complex Hull Gating, Supply Sites, Advantage, Battlefields & LP (`SO-5`, `Q-8`).

#![forbid(unsafe_code)]

use crate::sovereignty_equinox_gameplay_closure::{
    drain_notif_events, fit_siege_artillery, lock_target_via_rpc, make_ball, open_sqlite_ctx, register_online_pilot,
    rpc_call, set_pilot_location, step_weapon_ticks, temp_sqlite_path,
};
use eve_db::snapshot_ext::FwSystemOccupationRecord;
use eve_db::{InventoryFlag, InventoryItemRecord, TrainedSkillRecord};
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::fw_gameplay::{BATTLEFIELD_RESPAWN_COOLDOWN_SECS, BATTLEFIELD_ZONE_CENTERS};
use eve_server::services::fw_services::{
    STATUS_COMMAND, STATUS_FRONTLINE, STATUS_REARGUARD,
};
use glam::DVec3;

/// Finds a connected 4-system low-sec chain `enemy -- front -- cmd -- rear` in SDE and configures
/// Caldari (`500001`) / Gallente (`500004`) occupation so `front` is Frontline, `cmd` is Command Operations,
/// and `rear` is Rearguard.
pub(crate) fn setup_warzone_chain(ctx: &ServerContext) -> (u32, u32, u32, u32) {
    let mut lowsec: Vec<u32> = ctx
        .sde
        .solar_systems
        .values()
        .filter(|s| s.security_status > 0.0 && s.security_status < 0.45 && s.solar_system_id < 31_000_000)
        .map(|s| s.solar_system_id)
        .collect();
    lowsec.sort_unstable();
    let lowsec_set: std::collections::HashSet<u32> = lowsec.iter().copied().collect();
    let mut adj: std::collections::HashMap<u32, Vec<u32>> = std::collections::HashMap::new();
    for j in ctx.sde.jumps.values() {
        if lowsec_set.contains(&j.from_solar_system_id) && lowsec_set.contains(&j.to_solar_system_id) {
            adj.entry(j.from_solar_system_id).or_default().push(j.to_solar_system_id);
        }
    }
    let neighbors = |sys: u32| -> &[u32] {
        adj.get(&sys).map(|v| v.as_slice()).unwrap_or(&[])
    };
    let has_hs_nb = |sys: u32| -> bool {
        ctx.sde.jumps.values().any(|j| {
            j.from_solar_system_id == sys
                && ctx.sde.solar_systems.get(&j.to_solar_system_id).is_some_and(|s| {
                    (0.5..=0.8).contains(&s.security_status) && s.solar_system_id < 31_000_000
                })
        })
    };
    let mut chosen = None;
    'outer: for &front in &lowsec {
        let fnbs = neighbors(front);
        for &enemy in fnbs {
            for &cmd in fnbs {
                if cmd == enemy || neighbors(cmd).contains(&enemy) {
                    continue;
                }
                for &rear in neighbors(cmd) {
                    if rear != front
                        && rear != enemy
                        && !neighbors(rear).contains(&enemy)
                        && !neighbors(rear).contains(&front)
                        && has_hs_nb(front)
                    {
                        chosen = Some((enemy, front, cmd, rear));
                        break 'outer;
                    }
                }
            }
        }
    }
    let (enemy, front, cmd, rear) = chosen.expect("connected 4-system low-sec chain in SDE");
    ctx.db.with_mut_snapshot(|snap| {
        snap.fw_system_occupations.clear();
        let mut caldari_set = vec![front, cmd, rear];
        for base in [cmd, rear] {
            for &nb in neighbors(base) {
                if nb != enemy && !caldari_set.contains(&nb) {
                    caldari_set.push(nb);
                }
            }
        }
        snap.fw_system_occupations.insert(
            enemy,
            FwSystemOccupationRecord {
                solar_system_id: enemy,
                owner_faction_id: 500_004,
                occupier_faction_id: 500_004,
                victory_points: 0,
                victory_points_threshold: 3_000,
            },
        );
        for sys in caldari_set {
            snap.fw_system_occupations.insert(
                sys,
                FwSystemOccupationRecord {
                    solar_system_id: sys,
                    owner_faction_id: 500_001,
                    occupier_faction_id: 500_001,
                    victory_points: 0,
                    victory_points_threshold: 3_000,
                },
            );
        }
    });
    (enemy, front, cmd, rear)
}

#[tokio::test]
async fn test_fw_frontlines_hull_gating_supply_advantage_plexes_battlefields_and_lp() {
    // parity: feat:fw.frontline-system-designation
    // parity: feat:fw.complex-ship-size-gating
    // parity: feat:fw.plexes
    // parity: feat:fw.supply-caches-depots
    // parity: feat:fw.advantage-mechanics
    // parity: feat:fw.fw-site-dungeons
    // parity: feat:fw.battlefield-capture-rules
    // parity: feat:fw.battlefield-gameplay
    // DATA GAP: feat:fw.fw-lp (open until real SDE lpstore tables imported)
    eve_testkit::attest(&[
        "feat:fw.frontline-system-designation",
        "feat:fw.complex-ship-size-gating",
        "feat:fw.plexes",
        "feat:fw.supply-caches-depots",
        "feat:fw.advantage-mechanics",
        "feat:fw.fw-site-dungeons",
        "feat:fw.battlefield-capture-rules",
        "feat:fw.battlefield-gameplay",
    ]);

    let db_path = temp_sqlite_path("fw_warzone_f1");
    let ctx = open_sqlite_ctx(&db_path, true);
    let (_enemy_sys, front_sys, cmd_sys, rear_sys) = setup_warzone_chain(&ctx);

    let gal_cid = 90_000_011i64;
    let gal_ship = 100_000_011i64;
    let cal_cid = 90_000_012i64;
    let cal_ship = 100_000_012i64;

    let mut s_gal = register_online_pilot(
        &ctx, 81_001, "demo", "demo-secret", gal_cid, 98_000_011, None,
        front_sys, gal_ship, 593, DVec3::new(100_000.0, 0.0, 0.0), 0, // Tristan (T1 Frigate)
    ).await;
    let mut s_cal = register_online_pilot(
        &ctx, 81_002, "demo2", "demo2-secret", cal_cid, 98_000_012, None,
        front_sys, cal_ship, 587, DVec3::new(100_000.0, 5_000.0, 0.0), 0, // Rifter
    ).await;

    rpc_call(&ctx, &mut s_gal, "fwCharacterEnlistmentMgr", "CreateMyDirectEnlistment", &[PyRep::Integer(500_004)])
        .await.expect("enlist Gallente");
    rpc_call(&ctx, &mut s_cal, "fwCharacterEnlistmentMgr", "CreateMyDirectEnlistment", &[PyRep::Integer(500_001)])
        .await.expect("enlist Caldari");

    // 1. Verify Frontline (1), Command Operations (2), Rearguard (3) stargate adjacency & site seeding.
    let occ_adj = |rep: &PyRep| -> Option<i64> {
        let PyRep::Tuple(t) = rep else { return None };
        t.get(1)?.dict_get("adjacencyState")?.as_i64()
    };
    set_pilot_location(&ctx, &mut s_gal, front_sys, None);
    let f_state = rpc_call(&ctx, &mut s_gal, "fwWarzoneSolarsystem", "GetLocalOccupationState", &[]).await.unwrap();
    set_pilot_location(&ctx, &mut s_gal, cmd_sys, None);
    let c_state = rpc_call(&ctx, &mut s_gal, "fwWarzoneSolarsystem", "GetLocalOccupationState", &[]).await.unwrap();
    set_pilot_location(&ctx, &mut s_gal, rear_sys, None);
    let r_state = rpc_call(&ctx, &mut s_gal, "fwWarzoneSolarsystem", "GetLocalOccupationState", &[]).await.unwrap();
    assert_eq!(occ_adj(&f_state), Some(STATUS_FRONTLINE));
    assert_eq!(occ_adj(&c_state), Some(STATUS_COMMAND));
    assert_eq!(occ_adj(&r_state), Some(STATUS_REARGUARD));
    let _ = rpc_call(&ctx, &mut s_gal, "fwWarzoneSolarsystem", "GetAllWarzonesOccupationStates", &[]).await.unwrap();

    // Seed warzone sites across Rearguard, Command, and Frontline: Large/Open + Battlefields ONLY in Frontline!
    let tiers_in = |ctx: &ServerContext, sys: u32| -> Vec<String> {
        ctx.db.with_snapshot(|s| {
            s.fw_runtime.fw_complexes.values()
                .filter(|(csys, _, _, _, done)| *csys == sys && !*done)
                .map(|(_, t, _, _, _)| t.clone())
                .collect()
        })
    };
    set_pilot_location(&ctx, &mut s_gal, rear_sys, None);
    let r_sites = rpc_call(&ctx, &mut s_gal, "dungeonInstanceCacheMgr", "GetFactionWarfareInstances", &[]).await.unwrap();
    let r_tiers = tiers_in(&ctx, rear_sys);
    assert!(!r_tiers.is_empty() && !r_tiers.iter().any(|t| t == "Medium" || t == "Large" || t == "Open"));

    set_pilot_location(&ctx, &mut s_gal, cmd_sys, None);
    let c_sites = rpc_call(&ctx, &mut s_gal, "dungeonInstanceCacheMgr", "GetFactionWarfareInstances", &[]).await.unwrap();
    let c_bfs = rpc_call(&ctx, &mut s_gal, "dungeonInstanceCacheMgr", "GetFactionWarfareBattlefieldInstances", &[]).await.unwrap();
    let c_tiers = tiers_in(&ctx, cmd_sys);
    assert!(c_tiers.iter().any(|t| t == "Medium") && !c_tiers.iter().any(|t| t == "Large" || t == "Open"));
    assert_eq!(c_bfs, PyRep::List(Vec::new()));

    set_pilot_location(&ctx, &mut s_gal, front_sys, None);
    let f_sites = rpc_call(&ctx, &mut s_gal, "dungeonInstanceCacheMgr", "GetFactionWarfareInstances", &[]).await.unwrap();
    let f_bfs = rpc_call(&ctx, &mut s_gal, "dungeonInstanceCacheMgr", "GetFactionWarfareBattlefieldInstances", &[]).await.unwrap();
    let f_tiers = tiers_in(&ctx, front_sys);
    assert!(f_tiers.iter().any(|t| t == "Large") && f_tiers.iter().any(|t| t == "Open"));
    let site_count = |rep: &PyRep| match rep {
        PyRep::Dict(m) => m.first().and_then(|(_, v)| match v { PyRep::Dict(inner) => Some(inner.len()), _ => None }).unwrap_or(m.len()),
        _ => 0,
    };
    assert!(site_count(&r_sites) < site_count(&c_sites) && site_count(&c_sites) < site_count(&f_sites));
    assert!(matches!(f_bfs, PyRep::List(ref v) if !v.is_empty()));

    let gates: Vec<(i64, String, DVec3)> = ctx.db.with_snapshot(|s| {
        let mut v: Vec<_> = s.fw_runtime.fw_complexes.iter()
            .filter(|(_, (sys, _, _, _, done))| *sys == front_sys && !*done)
            .map(|(&gid, (_, tier, _, _, _))| (gid, tier.clone(), DVec3::from_array(s.item_positions[&gid])))
            .collect();
        v.sort_by_key(|x| x.0);
        v
    });
    let gate_for = |tier: &str| -> (i64, DVec3) {
        let g = gates.iter().find(|(_, t, _)| t == tier).unwrap();
        (g.0, g.2)
    };
    let (novice_gate, novice_pos) = gate_for("Novice");
    let (small_gate, small_pos) = gate_for("Small");
    let (medium_gate, medium_pos) = gate_for("Medium");
    let (large_gate, large_pos) = gate_for("Large");
    let (open_gate, open_pos) = gate_for("Open");

    // 2. Live `keeper.ActivateAccelerationGate`: docked, >2500m range, Pirate/T2 frigate on Novice, and hull size limits!
    set_pilot_location(&ctx, &mut s_gal, front_sys, Some(60_003_760));
    assert_eq!(
        rpc_call(&ctx, &mut s_gal, "keeper", "ActivateAccelerationGate", &[PyRep::Integer(novice_gate)]).await.unwrap_err().code,
        "NotInSpace"
    );
    set_pilot_location(&ctx, &mut s_gal, front_sys, None);

    let set_hull = |ctx: &ServerContext, ship_id: i64, tid: u32, pos: DVec3| {
        let sys_h = ctx.systems.get_or_spawn(front_sys);
        ctx.db.with_mut_snapshot(|s| {
            if let Some(it) = s.items.get_mut(&ship_id) {
                it.type_id = tid;
            }
        });
        async move {
            let _ = sys_h.spawn_ball(make_ball(ship_id, tid, pos)).await;
        }
    };

    set_hull(&ctx, gal_ship, 593, novice_pos + DVec3::new(6_000.0, 0.0, 0.0)).await;
    assert_eq!(
        rpc_call(&ctx, &mut s_gal, "keeper", "ActivateAccelerationGate", &[PyRep::Integer(novice_gate)]).await.unwrap_err().code,
        "OutsideGateActivationRange"
    );
    // Pirate Frigate (`17932` Dramiel) & T2 Assault Frigate (`11381` Hawk) rejected from Novice (`ShipTypeNotAllowedInComplex`).
    for bad_frig in [17_932u32, 11_381] {
        set_hull(&ctx, gal_ship, bad_frig, novice_pos + DVec3::new(1_000.0, 0.0, 0.0)).await;
        assert_eq!(
            rpc_call(&ctx, &mut s_gal, "keeper", "ActivateAccelerationGate", &[PyRep::Integer(novice_gate)]).await.unwrap_err().code,
            "ShipTypeNotAllowedInComplex"
        );
    }
    // Destroyer (`16242` Thrasher) -> Novice, Cruiser (`621` Caracal) -> Small, BS (`638` Raven) -> Medium, Dread (`19726` Phoenix) -> Large (`ShipTooLargeForGate`).
    for (hull_tid, gid, gpos) in [
        (16_242u32, novice_gate, novice_pos),
        (621, small_gate, small_pos),
        (638, medium_gate, medium_pos),
        (19_726, large_gate, large_pos),
    ] {
        set_hull(&ctx, gal_ship, hull_tid, gpos + DVec3::new(1_000.0, 0.0, 0.0)).await;
        assert_eq!(
            rpc_call(&ctx, &mut s_gal, "keeper", "ActivateAccelerationGate", &[PyRep::Integer(gid)]).await.unwrap_err().code,
            "ShipTooLargeForGate"
        );
    }
    // Dreadnought (`19726`) CAN enter `Open` complex; Empire Navy Frigate (`17841` Federation Navy Comet) CAN enter `Novice` complex!
    set_hull(&ctx, gal_ship, 19_726, open_pos + DVec3::new(1_000.0, 0.0, 0.0)).await;
    assert!(rpc_call(&ctx, &mut s_gal, "keeper", "ActivateAccelerationGate", &[PyRep::Integer(open_gate)]).await.is_ok());
    set_hull(&ctx, gal_ship, 17_841, novice_pos + DVec3::new(1_000.0, 0.0, 0.0)).await;
    let navy_novice = rpc_call(&ctx, &mut s_gal, "keeper", "ActivateAccelerationGate", &[PyRep::Integer(novice_gate)]).await.expect("Navy Frigate allowed in Novice");
    assert_eq!(navy_novice.dict_get("activated"), Some(&PyRep::Bool(true)));
    assert_eq!(navy_novice.dict_get("complexTier"), Some(&PyRep::String("Novice".into())));
    // Clear occupancy on `open_gate` and `novice_gate` before testing Novice capture.
    ctx.db.with_mut_snapshot(|s| { s.fw_runtime.complex_occupants.clear(); });

    // 3. Live `dogmaIM.Activate` combat weapon fire on FW Supply Cache/Depot/ListeningPost/Propaganda:
    //    raises attacker Advantage (`OnSolarsystemAdvantageStateUpdated_Local`), drops Cargo Container (`23`) loot, and speeds up complex capture!
    fit_siege_artillery(&ctx, gal_cid, gal_ship, 997_000);
    let mut supply_sites: Vec<(i64, DVec3)> = ctx.db.with_snapshot(|s| {
        s.fw_runtime.supply_sites.iter()
            .filter(|(_, (sys, _, _))| *sys == front_sys)
            .map(|(&sid, _)| (sid, DVec3::from_array(s.structures[&sid].position)))
            .collect()
    });
    supply_sites.sort_by_key(|&(sid, _)| sid);
    assert!(supply_sites.len() >= 2);
    let _ = drain_notif_events(&ctx, gal_cid);
    for &(sid, spos) in supply_sites.iter().take(2) {
        set_hull(&ctx, gal_ship, 19_722, spos + DVec3::new(2_000.0, 0.0, 0.0)).await;
        lock_target_via_rpc(&ctx, &mut s_gal, front_sys, gal_ship, sid).await;
        for _ in 0..8 {
            for gun_id in [997_000, 997_002, 997_004] {
                let _ = rpc_call(&ctx, &mut s_gal, "dogmaIM", "Activate", &[
                    PyRep::Integer(gun_id),
                    PyRep::String("targetAttack".into()),
                    PyRep::Integer(sid),
                ]).await;
            }
            step_weapon_ticks(&ctx, front_sys).await;
            if ctx.db.with_snapshot(|s| !s.fw_runtime.supply_sites.contains_key(&sid) || s.structures.get(&sid).map(|st| st.state.as_str()) == Some("Destroyed")) {
                break;
            }
            ctx.advance_secs(25);
        }
    }
    assert!(drain_notif_events(&ctx, gal_cid).iter().any(|e| e == "OnSolarsystemAdvantageStateUpdated_Local"));
    let balls_after_supply = ctx.systems.get_or_spawn(front_sys).list_balls().await.unwrap();
    assert!(balls_after_supply.iter().any(|b| b.type_id == 23), "destroyed FW Supply site drops loot container");

    // Taking non-lethal damage on a friendly supply site and holding defense for 30s resolves defense:
    if let Some(&(def_sid, def_pos)) = supply_sites.iter().skip(2).next() {
        set_hull(&ctx, gal_ship, 19_722, def_pos + DVec3::new(2_000.0, 0.0, 0.0)).await;
        lock_target_via_rpc(&ctx, &mut s_gal, front_sys, gal_ship, def_sid).await;
        let _ = rpc_call(&ctx, &mut s_gal, "dogmaIM", "Activate", &[
            PyRep::Integer(997_000),
            PyRep::String("targetAttack".into()),
            PyRep::Integer(def_sid),
        ]).await;
        step_weapon_ticks(&ctx, front_sys).await;
        assert!(ctx.db.with_snapshot(|s| s.structures[&def_sid].last_damage_unix > 0));

        // Move attacker away, place friendly Caldari defender on grid:
        set_hull(&ctx, gal_ship, 19_722, DVec3::new(0.0, 0.0, 0.0)).await;
        set_hull(&ctx, cal_ship, 587, def_pos + DVec3::new(1_000.0, 0.0, 0.0)).await;
        ctx.db.with_mut_snapshot(|snap| {
            let sk = snap.skills.entry(cal_cid).or_default();
            for sid in [3300u32, 3303, 3306, 3413, 3426, 3456] {
                sk.insert(sid, TrainedSkillRecord { type_id: sid, level: 5, skill_points: 256_000.0 });
            }
            snap.items.insert(
                996_000,
                InventoryItemRecord {
                    item_id: 996_000,
                    type_id: 484, // 125mm Gatling AutoCannon I
                    owner_id: cal_cid,
                    location_id: cal_ship,
                    flag: InventoryFlag::HiSlot0,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
            snap.items.insert(
                996_001,
                InventoryItemRecord {
                    item_id: 996_001,
                    type_id: 185, // EMP S
                    owner_id: cal_cid,
                    location_id: cal_ship,
                    flag: InventoryFlag::HiSlot0,
                    quantity: 100,
                    ..Default::default()
                },
            );
            snap.module_online_states.insert(996_000, true);
            snap.module_active_charges.insert(996_000, 185);
        });
        ctx.invalidate_ship_fitting(cal_ship);
        let _ = ctx.systems.get_or_spawn(front_sys).add_target_lock(
            cal_ship,
            eve_destiny::TargetLock {
                target_id: def_sid,
                lock_time_ms: 1000.0,
                remaining_lock_ms: 0.0,
                locked: true,
                max_range_m: Some(100_000.0),
            },
        ).await;

        // Friendly militia cannot attack own supply site:
        assert_eq!(
            rpc_call(&ctx, &mut s_cal, "dogmaIM", "Activate", &[
                PyRep::Integer(996_000), PyRep::String("targetAttack".into()), PyRep::Integer(def_sid),
            ]).await.unwrap_err().code,
            "CannotAttackFriendlyStructure"
        );
        let _ = drain_notif_events(&ctx, cal_cid);

        // Advance 31s and tick background cluster:
        ctx.advance_secs(31);
        eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;

        assert!(!ctx.db.with_snapshot(|s| s.structures.contains_key(&def_sid)), "friendly defense resolves and despawns site");
        assert!(drain_notif_events(&ctx, cal_cid).iter().any(|e| e == "OnSolarsystemAdvantageStateUpdated_Local"), "defense emits OnSolarsystemAdvantageStateUpdated_Local");
        let (occ_adv, att_adv) = ctx.db.with_snapshot(|s| s.fw_runtime.system_advantage[&front_sys]);
        assert!(occ_adv > 0.0 && att_adv > occ_adv, "occupier defense credits occ_adv ({occ_adv}) while attacker supply kills credit att_adv ({att_adv})");
    }

    // 4. Live Novice complex capture via `keeper.ActivateAccelerationGate` + `tick_background_cluster`:
    //    opposing militia ship in pocket pauses timer; removing defender completes capture in <4 ticks thanks to Advantage,
    //    crediting Federation Navy (`1000181`) LP!
    set_hull(&ctx, gal_ship, 593, novice_pos + DVec3::new(1_000.0, 0.0, 0.0)).await;
    set_hull(&ctx, cal_ship, 587, novice_pos + DVec3::new(1_200.0, 0.0, 0.0)).await;
    rpc_call(&ctx, &mut s_gal, "keeper", "ActivateAccelerationGate", &[PyRep::Integer(novice_gate)]).await.unwrap();
    rpc_call(&ctx, &mut s_cal, "keeper", "ActivateAccelerationGate", &[PyRep::Integer(novice_gate)]).await.unwrap();
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    let prog_contested = ctx.db.with_snapshot(|s| s.fw_runtime.fw_complexes[&novice_gate].3);
    assert_eq!(prog_contested, 0.0, "opposing militia ship inside pocket pauses FW complex capture timer");

    // Warp defender out of pocket and clear guard -> Advantage (>0%) lets Gallente advance >25% on first 150s tick and finish in 4 ticks (600s total)!
    set_hull(&ctx, cal_ship, 587, DVec3::new(0.0, 0.0, 0.0)).await;
    let guard_id = novice_gate + 500_000_000;
    let _ = ctx.systems.get_or_spawn(front_sys).remove_ball(guard_id).await;
    ctx.db.with_mut_snapshot(|s| {
        s.fw_runtime.complex_guards_cleared.insert(novice_gate, true);
    });
    ctx.advance_secs(150);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    let prog_adv = ctx.db.with_snapshot(|s| s.fw_runtime.fw_complexes[&novice_gate].3);
    assert!(prog_adv > 25.0, "Gallente Advantage speeds up complex capture rate above 25%/tick (got {prog_adv})");
    for _ in 0..3 {
        ctx.advance_secs(150);
        eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    }
    let gal_lp = ctx.db.with_snapshot(|s| s.characters[&gal_cid].loyalty_points.get(&1_000_181).copied().unwrap_or(0));
    assert!(gal_lp >= 10_000, "Frontline Novice capture awards Federation Navy (1000181) LP (got {gal_lp})");
    let lp_bals = rpc_call(&ctx, &mut s_gal, "LPSvc", "GetAllMyCharacterWalletLPBalances", &[]).await.unwrap();
    assert!(format!("{lp_bals:?}").contains("1000181"));

    // 5. Live 3-zone Frontline Battlefield capture:
    //    - First verify defending Caldari militia (`500001`) holds Zone 0, spawns combat-capable orbiting NPC waves,
    //      completes Battlefield capture (`-2,000` VP decontest + State Protectorate LP & faction/corp standing), and triggers 4h cooldown
    //    - Advance past 4h `BATTLEFIELD_RESPAWN_COOLDOWN_SECS` to spawn a fresh Battlefield, then place 31 attacking Gallente ships
    //      across the 3 `BATTLEFIELD_ZONE_CENTERS` -> verify de-contesting, `> 30` pilot LP scaling (`145,161 LP`), standing gains, & SQLite durability!
    let bf_id = ctx.db.with_snapshot(|s| {
        s.fw_runtime.battlefield_instances.iter().find(|b| b.solar_system_id == front_sys && !b.completed).unwrap().instance_id
    });
    let vp_before_def = ctx.db.with_snapshot(|s| s.fw_system_occupations[&front_sys].victory_points);
    assert!(vp_before_def > 0, "earlier Novice capture contested front_sys above 0 VP ({vp_before_def})");
    set_hull(&ctx, gal_ship, 621, DVec3::new(0.0, 0.0, 0.0)).await;
    set_hull(&ctx, cal_ship, 587, DVec3::from_array(BATTLEFIELD_ZONE_CENTERS[0])).await;
    ctx.advance_secs(90);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    let (def_zone0, def_fac, def_prog, wave_npcs) = ctx.db.with_snapshot(|s| {
        let bf = s.fw_runtime.battlefield_instances.iter().find(|b| b.instance_id == bf_id).unwrap();
        (
            s.fw_runtime.battlefield_zones[&bf_id][0],
            bf.faction_id,
            bf.capture_progress,
            s.fw_runtime.battlefield_npc_waves.get(&bf_id).cloned().unwrap_or_default(),
        )
    });
    assert_eq!((def_zone0, def_fac), (500_001, 500_001), "defending Caldari militia captures Zone 0");
    assert!(def_prog > 0.0 && def_prog < 100.0, "defender holding 1 zone for 90s advances Battlefield progress ({def_prog}%)");
    assert_eq!(wave_npcs.len(), 6, "Battlefield spawns 6 friendly/enemy NPC wave ships across the 3 zones");
    let first_npc_cs = ctx.systems.get_or_spawn(front_sys).get_combat_state(wave_npcs[0]).await.unwrap().expect("wave NPC combat state");
    assert!(first_npc_cs.shield_hp > 0.0 && first_npc_cs.structure_hp > 0.0, "Battlefield wave NPC has active CombatState");
    for _ in 0..9 {
        ctx.advance_secs(100);
        eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    }
    let (vp_after_def, cal_bf_lp, cal_fac_st) = ctx.db.with_snapshot(|s| {
        let ch = &s.characters[&cal_cid];
        (
            s.fw_system_occupations[&front_sys].victory_points,
            ch.loyalty_points.get(&1_000_180).copied().unwrap_or(0),
            ch.standings.get(&500_001).copied().unwrap_or(0.0),
        )
    });
    assert_eq!(vp_after_def, 0, "defender Battlefield completion applies -2,000 VP decontest clamped at 0");
    assert!(cal_bf_lp >= 150_000 && cal_fac_st > 0.0, "defender earns 150k State Protectorate LP ({cal_bf_lp}) and standing ({cal_fac_st})");
    set_hull(&ctx, cal_ship, 587, DVec3::new(0.0, 0.0, 0.0)).await;

    ctx.advance_secs(BATTLEFIELD_RESPAWN_COOLDOWN_SECS + 10);
    let respawned_bfs = rpc_call(&ctx, &mut s_gal, "dungeonInstanceCacheMgr", "GetFactionWarfareBattlefieldInstances", &[]).await.unwrap();
    let PyRep::List(resp_list) = &respawned_bfs else { panic!("expected List") };
    assert_eq!(resp_list.len(), 1, "new Frontline Battlefield spawns after 4h cooldown expires");

    ctx.db.with_mut_snapshot(|s| {
        let base_ch = s.characters[&gal_cid].clone();
        for idx in 0..30i64 {
            let extra_cid = 90_100_000 + idx;
            let extra_ship = 100_100_000 + idx;
            let mut wch = base_ch.clone();
            wch.character_id = extra_cid;
            wch.name = format!("BF Wingman {idx}");
            wch.solar_system_id = front_sys;
            wch.active_ship_id = extra_ship;
            wch.war_faction_id = Some(500_004);
            wch.loyalty_points = Default::default();
            s.characters.insert(extra_cid, wch);
            s.items.insert(
                extra_ship,
                InventoryItemRecord {
                    item_id: extra_ship,
                    type_id: 621,
                    owner_id: extra_cid,
                    location_id: front_sys as i64,
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
        }
    });
    for idx in 0..30i64 {
        let extra_ship = 100_100_000 + idx;
        let zc = DVec3::from_array(BATTLEFIELD_ZONE_CENTERS[(idx as usize) % 3]);
        let _ = ctx.systems.get_or_spawn(front_sys).spawn_ball(make_ball(extra_ship, 621, zc + DVec3::new(500.0, 0.0, 0.0))).await;
    }
    set_hull(&ctx, gal_ship, 621, DVec3::from_array(BATTLEFIELD_ZONE_CENTERS[0])).await;
    let lp_before_bf = ctx.db.with_snapshot(|s| s.characters[&gal_cid].loyalty_points[&1_000_181]);
    for _ in 0..4 {
        ctx.advance_secs(120);
        eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
        if ctx.db.with_snapshot(|s| s.characters[&gal_cid].loyalty_points[&1_000_181] > lp_before_bf) {
            break;
        }
    }
    let (lp_after_bf, fac_st, corp_st) = ctx.db.with_snapshot(|s| {
        let ch = &s.characters[&gal_cid];
        (
            ch.loyalty_points[&1_000_181],
            ch.standings.get(&500_004).copied().unwrap_or(0.0),
            ch.standings.get(&1_000_181).copied().unwrap_or(0.0),
        )
    });
    let bf_award = lp_after_bf - lp_before_bf;
    let expected_scaled = (150_000 * 30) / 31;
    assert_eq!(bf_award, expected_scaled, "31 pilots in 3-zone Battlefield scale 150k LP to 145,161 LP");
    assert!(fac_st > 0.0 && corp_st > 0.0, "Battlefield completion awards faction ({fac_st}) and militia corp ({corp_st}) standing");
    let wingman_last_lp = ctx.db.with_snapshot(|s| s.characters[&(90_100_000 + 29)].loyalty_points.get(&1_000_181).copied().unwrap_or(0));
    assert_eq!(wingman_last_lp, expected_scaled, "31st pilot also receives scaled LP");

    let bf_cd = ctx.db.with_snapshot(|s| s.fw_runtime.battlefield_respawn_unix[&front_sys]);
    assert_eq!(bf_cd - ctx.now_unix(), BATTLEFIELD_RESPAWN_COOLDOWN_SECS);
    let bfs_on_cd = rpc_call(&ctx, &mut s_gal, "dungeonInstanceCacheMgr", "GetFactionWarfareBattlefieldInstances", &[]).await.unwrap();
    assert_eq!(bfs_on_cd, PyRep::List(Vec::new()), "4h cooldown blocks immediate Battlefield respawn");

    drop(ctx);
    let ctx_restarted = open_sqlite_ctx(&db_path, false);
    assert_eq!(ctx_restarted.db.with_snapshot(|s| s.fw_runtime.battlefield_respawn_unix.get(&front_sys).copied()), Some(bf_cd));
    let _ = std::fs::remove_file(&db_path);
}
