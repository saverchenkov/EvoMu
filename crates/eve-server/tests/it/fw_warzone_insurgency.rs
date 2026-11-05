//! Integration tests for Faction Warfare Frontlines, Complex Hull Gating, Supply Sites, Advantage, Battlefields & LP (`SO-5`, `Q-8`).

#![forbid(unsafe_code)]

use crate::sovereignty_equinox_gameplay::{
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
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                997_010,
                InventoryItemRecord {
                    item_id: 997_010,
                    type_id: 484, // 125mm Gatling AutoCannon I (non-lethal subcap hit on 4,500 HP outpost)
                    owner_id: gal_cid,
                    location_id: gal_ship,
                    flag: InventoryFlag::HiSlot3,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
            snap.items.insert(
                997_011,
                InventoryItemRecord {
                    item_id: 997_011,
                    type_id: 185, // EMP S
                    owner_id: gal_cid,
                    location_id: gal_ship,
                    flag: InventoryFlag::HiSlot3,
                    quantity: 100,
                    ..Default::default()
                },
            );
            snap.module_online_states.insert(997_010, true);
            snap.module_active_charges.insert(997_010, 185);
        });
        ctx.invalidate_ship_fitting(gal_ship);
        set_hull(&ctx, gal_ship, 19_722, def_pos + DVec3::new(2_000.0, 0.0, 0.0)).await;
        lock_target_via_rpc(&ctx, &mut s_gal, front_sys, gal_ship, def_sid).await;
        let _ = rpc_call(&ctx, &mut s_gal, "dogmaIM", "Activate", &[
            PyRep::Integer(997_010),
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

#[allow(dead_code, unused_imports)]
pub(crate) mod insurgency_fob_operations {
    use super::*;

//  Part 2 of FW & Insurgency closure tests: FW I-Hub Siege/Regen, System Tiers, War HQs, Leaderboards, FW Agents & Corp/Alliance Enlistment (`SO-5`, `Q-8`).


use crate::fw_warzone_insurgency::setup_warzone_chain;
use crate::sovereignty_equinox_gameplay::{
    drain_notif_events, fit_siege_artillery, lock_target_via_rpc, make_ball, open_sqlite_ctx,
    register_online_pilot, rpc_call, set_pilot_location, step_weapon_ticks, temp_sqlite_path,
};
use eve_db::{corp_roles, IskCents};
use eve_proto::PyRep;
use eve_server::services::fw_gameplay::TYPE_FW_IHUB;
use eve_server::services::fw_services::STATUS_FRONTLINE;
use glam::DVec3;

const FW_ENLISTMENT_COOLDOWN_SECS: u64 = 24 * 3_600;
const IHUB_UNATTACKED_REGEN_SECS: u64 = 2 * 3_600;

#[tokio::test]
async fn test_fw_ihub_siege_tiers_war_hqs_leaderboards_missions_and_corp_enlistment() {
    // parity: feat:fw.infrastructure-hub
    // parity: feat:fw.ihub-capture-and-tier-scaling
    // parity: feat:fw.war-hq-systems
    // parity: feat:fw.fw-leaderboards
    // parity: feat:fw.fw-missions
    // parity: notify:OnWarzoneOccupationStateUpdated
    eve_testkit::attest(&[
        "feat:fw.infrastructure-hub",
        "feat:fw.ihub-capture-and-tier-scaling",
        "feat:fw.war-hq-systems",
        "feat:fw.fw-leaderboards",
        "feat:fw.fw-missions",
        "notify:OnWarzoneOccupationStateUpdated",
    ]);

    let db_path = temp_sqlite_path("fw_warzone_f2");
    let ctx = open_sqlite_ctx(&db_path, true);
    let (_enemy_sys, front_sys, cmd_sys, _rear_sys) = setup_warzone_chain(&ctx);

    let cal_cid = 90_000_021i64;
    let cal_corp = 98_000_021i64;
    let cal_ally = 99_000_021i64;
    let cal_ship = 100_000_021i64;

    let gal_cid = 90_000_022i64;
    let gal_corp = 98_000_022i64;
    let gal_ship = 100_000_022i64;

    let mut s_cal = register_online_pilot(
        &ctx, 82_001, "demo", "demo-secret", cal_cid, cal_corp, Some(cal_ally),
        front_sys, cal_ship, 639, DVec3::new(202_000.0, 0.0, 0.0), corp_roles::ROLE_DIRECTOR,
    ).await;
    let mut s_gal = register_online_pilot(
        &ctx, 82_002, "demo2", "demo2-secret", gal_cid, gal_corp, None,
        front_sys, gal_ship, 639, DVec3::new(202_500.0, 0.0, 0.0), corp_roles::ROLE_DIRECTOR,
    ).await;

    // 1. War HQ systems (`fwWarzoneSolarsystem.GetHQSystemIDs`) & unrestricted War HQ station access (`stationSvc.GetStation`).
    let hqs = rpc_call(&ctx, &mut s_cal, "fwWarzoneSolarsystem", "GetHQSystemIDs", &[]).await.unwrap();
    let hq_str = format!("{hqs:?}");
    for hq_sys in [30_002_974u32, 30_045_324, 30_003_788, 30_002_055] {
        assert!(hq_str.contains(&hq_sys.to_string()));
        let war_hq_sta = ctx.sde.solar_systems[&hq_sys].stations[0].station_id;
        let sta_rep = rpc_call(&ctx, &mut s_cal, "stationSvc", "GetStation", &[PyRep::Integer(war_hq_sta)]).await.unwrap();
        assert_eq!(sta_rep.dict_get("isWarHQ"), Some(&PyRep::Bool(true)));
        assert!(matches!(sta_rep.dict_get("warHQServices"), Some(PyRep::List(v)) if !v.is_empty()));
    }
    let jita_rep = rpc_call(&ctx, &mut s_cal, "stationSvc", "GetStation", &[PyRep::Integer(60_003_760)]).await.unwrap();
    assert_eq!(jita_rep.dict_get("isWarHQ"), Some(&PyRep::Bool(false)));

    // 2. Corporate & Alliance Militia Enlistment (`facWarMgr`), 24h cooldown, and individual pilot preservation on corp leave!
    let mut s_norole = register_online_pilot(
        &ctx, 82_003, "demo3", "demo3-secret", 90_000_023, cal_corp, Some(cal_ally),
        front_sys, 100_000_023, 587, DVec3::new(10_000.0, 0.0, 0.0), 0,
    ).await;
    assert_eq!(
        rpc_call(&ctx, &mut s_norole, "facWarMgr", "JoinFactionAsCorporation", &[PyRep::Integer(cal_corp), PyRep::Integer(500_001)])
            .await.unwrap_err().code,
        "AccessDenied"
    );
    // Individual direct enlistment for pilot `90_000_023` BEFORE corp joins/leaves.
    rpc_call(&ctx, &mut s_norole, "fwCharacterEnlistmentMgr", "CreateMyDirectEnlistment", &[PyRep::Integer(500_001)])
        .await.expect("direct enlistment");

    rpc_call(&ctx, &mut s_cal, "facWarMgr", "JoinFactionAsCorporation", &[PyRep::Integer(cal_corp), PyRep::Integer(500_001)])
        .await.expect("JoinFactionAsCorporation");
    assert_eq!(
        rpc_call(&ctx, &mut s_cal, "facWarMgr", "JoinFactionAsCorporation", &[PyRep::Integer(cal_corp), PyRep::Integer(500_001)])
            .await.unwrap_err().code,
        "CorporationAlreadyEnlisted"
    );
    assert_eq!(
        rpc_call(&ctx, &mut s_cal, "facWarMgr", "LeaveFactionAsCorporation", &[PyRep::Integer(cal_corp), PyRep::Integer(500_001)])
            .await.unwrap_err().code,
        "MilitiaCooldownActive"
    );
    ctx.advance_secs(FW_ENLISTMENT_COOLDOWN_SECS + 10);
    rpc_call(&ctx, &mut s_cal, "facWarMgr", "LeaveFactionAsCorporation", &[PyRep::Integer(cal_corp), PyRep::Integer(500_001)])
        .await.expect("LeaveFactionAsCorporation after 24h");
    assert_eq!(
        ctx.db.with_snapshot(|s| s.characters[&90_000_023].war_faction_id),
        Some(500_001),
        "leaving corporate militia preserves individually direct-enlisted member"
    );

    // Alliance enlistment & re-enlistment for Caldari (`500001`) and Gallente (`500004`).
    ctx.advance_secs(FW_ENLISTMENT_COOLDOWN_SECS + 10);
    rpc_call(&ctx, &mut s_cal, "facWarMgr", "JoinFactionAsAlliance", &[PyRep::Integer(cal_ally), PyRep::Integer(500_001)])
        .await.expect("JoinFactionAsAlliance");
    rpc_call(&ctx, &mut s_gal, "facWarMgr", "JoinFactionAsCorporation", &[PyRep::Integer(gal_corp), PyRep::Integer(500_004)])
        .await.expect("Gallente corp enlist");

    // 3. I-Hub LP Donations (`facWarMgr.DonateLPsToSolarSystem`) & live `marketProxy.PlaceBuyOrder` broker fee reduction!
    assert_eq!(
        rpc_call(&ctx, &mut s_cal, "facWarMgr", "DonateLPsToSolarSystem", &[PyRep::Integer(30_000_142), PyRep::Integer(1_000)])
            .await.unwrap_err().code,
        "NotInWarzone"
    );
    // Find a warzone system occupied by Caldari (or seed one) that has an NPC station so we can compare real broker fees!
    let (mkt_sys, mkt_sta, mkt_reg) = ctx.sde.solar_systems.values()
        .filter(|s| s.security_status > 0.0 && s.security_status < 0.45 && !s.stations.is_empty())
        .map(|s| (s.solar_system_id, s.stations[0].station_id, s.region_id))
        .next()
        .expect("low-sec system with NPC station");
    ctx.db.with_mut_snapshot(|s| {
        s.fw_system_occupations.entry(mkt_sys).or_insert(eve_db::snapshot_ext::FwSystemOccupationRecord {
            solar_system_id: mkt_sys,
            owner_faction_id: 500_001,
            occupier_faction_id: 500_001,
            victory_points: 0,
            victory_points_threshold: 3_000,
        }).occupier_faction_id = 500_001;
        let ch = s.characters.get_mut(&cal_cid).unwrap();
        ch.solar_system_id = mkt_sys;
        ch.station_id = Some(mkt_sta);
        ch.region_id = mkt_reg;
        ch.constellation_id = ctx.sde.solar_systems[&mkt_sys].constellation_id;
        ch.balance = IskCents::from_isk(500_000_000);
        ch.loyalty_points.insert(1_000_180, 500_000);
    });
    s_cal.solar_system_id = Some(mkt_sys);
    s_cal.station_id = Some(mkt_sta);
    s_cal.region_id = Some(mkt_reg);

    let bp_id = *ctx.sde.blueprints.keys().next().unwrap() as i64;
    let bal0 = ctx.db.with_snapshot(|s| s.characters[&cal_cid].balance.0);
    rpc_call(
        &ctx, &mut s_cal, "marketProxy", "PlaceBuyOrder",
        &[PyRep::Integer(mkt_sta), PyRep::Integer(34), PyRep::Real(1_000.0), PyRep::Integer(1_000), PyRep::Integer(-1), PyRep::Integer(1), PyRep::Integer(90)],
    ).await.expect("PlaceBuyOrder at Tier 0");
    let fee_tier0 = (bal0 - ctx.db.with_snapshot(|s| s.characters[&cal_cid].balance.0)) - 100_000_000;
    let bp_data0 = rpc_call(&ctx, &mut s_cal, "blueprintManager", "GetBlueprintData", &[PyRep::Integer(bp_id)]).await.unwrap();
    let tax_tier0 = bp_data0.dict_get("facilityTaxISK").and_then(|v| v.as_f64()).unwrap();
    assert!(tax_tier0 > 0.0);
    assert!(fee_tier0 > 0);

    for (tier, donate_amt) in [(1u8, 40_000), (2, 20_000), (3, 30_000), (4, 50_000), (5, 60_000)] {
        let don = rpc_call(&ctx, &mut s_cal, "facWarMgr", "DonateLPsToSolarSystem", &[PyRep::Integer(mkt_sys as i64), PyRep::Integer(donate_amt)]).await.unwrap();
        assert_eq!(don.dict_get("tier").and_then(|v| v.as_i64()), Some(tier as i64));

        let expected_mult = 1.0 - 0.10 * (tier as f64);

        ctx.db.with_mut_snapshot(|s| s.market_orders.clear());
        let bal_before = ctx.db.with_snapshot(|s| s.characters[&cal_cid].balance.0);
        rpc_call(
            &ctx, &mut s_cal, "marketProxy", "PlaceBuyOrder",
            &[PyRep::Integer(mkt_sta), PyRep::Integer(34), PyRep::Real(1_000.0), PyRep::Integer(1_000), PyRep::Integer(-1), PyRep::Integer(1), PyRep::Integer(90)],
        ).await.expect("PlaceBuyOrder");
        let fee_t = (bal_before - ctx.db.with_snapshot(|s| s.characters[&cal_cid].balance.0)) - 100_000_000;
        let expected_fee = ((fee_tier0 as f64) * expected_mult).round() as i64;
        assert!((fee_t - expected_fee).abs() <= 100, "broker fee at Tier {tier}: {fee_t} expected ~{expected_fee}");

        let bp_data = rpc_call(&ctx, &mut s_cal, "blueprintManager", "GetBlueprintData", &[PyRep::Integer(bp_id)]).await.unwrap();
        let tax_t = bp_data.dict_get("facilityTaxISK").and_then(|v| v.as_f64()).unwrap();
        let expected_tax = ((tax_tier0 * expected_mult) * 100.0).round() / 100.0;
        assert!((tax_t - expected_tax).abs() <= 0.05, "facility tax at Tier {tier}: {tax_t} expected ~{expected_tax}");
    }

    // Move Caldari pilot back to `front_sys` in space.
    set_pilot_location(&ctx, &mut s_cal, front_sys, None);

    // 4. FW I-Hub (`32226`) invulnerability before 100% contested, 2h unattacked regeneration to 80%, and live siege flip!
    let _ = rpc_call(&ctx, &mut s_gal, "dungeonInstanceCacheMgr", "GetFactionWarfareInstances", &[]).await.unwrap();
    let ihub_id = ctx.db.with_snapshot(|s| {
        s.structures.values().find(|st| st.solar_system_id == front_sys && st.type_id == TYPE_FW_IHUB).unwrap().structure_id
    });
    fit_siege_artillery(&ctx, gal_cid, gal_ship, 998_000);
    lock_target_via_rpc(&ctx, &mut s_gal, front_sys, gal_ship, ihub_id).await;
    assert_eq!(
        rpc_call(&ctx, &mut s_gal, "dogmaIM", "Activate", &[
            PyRep::Integer(998_000),
            PyRep::String("targetAttack".into()),
            PyRep::Integer(ihub_id),
        ]).await.unwrap_err().code,
        "IHubInvulnerable"
    );

    let vulnerable_ts = ctx.now_unix();
    ctx.db.with_mut_snapshot(|s| {
        s.fw_system_occupations.get_mut(&front_sys).unwrap().victory_points = 3_000;
        s.fw_runtime.system_victory_points.insert(front_sys, 3_000);
        s.fw_runtime.ihub_vulnerable.insert(front_sys, true);
        s.fw_runtime.last_ihub_attacked_unix.insert(front_sys, vulnerable_ts);
        s.system_states.entry(front_sys).or_default().fw_contested_pct = 100.0;
    });
    ctx.advance_secs(IHUB_UNATTACKED_REGEN_SECS + 5);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    assert_eq!(
        ctx.db.with_snapshot(|s| s.system_states[&front_sys].fw_contested_pct),
        80.0,
        "unattacked 100%-contested FW I-Hub regenerates back to 80% after 2 hours"
    );

    // Re-contest to 100% and destroy FW I-Hub via `dogmaIM.Activate`: flips occupancy, emits `OnWarzoneOccupationStateUpdated`, and updates `cmd_sys` to Frontline!
    let attack_ts = ctx.now_unix();
    ctx.db.with_mut_snapshot(|s| {
        s.fw_system_occupations.get_mut(&front_sys).unwrap().victory_points = 3_000;
        s.fw_runtime.system_victory_points.insert(front_sys, 3_000);
        s.fw_runtime.ihub_vulnerable.insert(front_sys, true);
        s.fw_runtime.last_ihub_attacked_unix.insert(front_sys, attack_ts);
        s.system_states.entry(front_sys).or_default().fw_contested_pct = 100.0;
        if let Some(st) = s.structures.get_mut(&ihub_id) {
            st.shield_hp = 0.0;
            st.armor_hp = 0.0;
            st.hull_hp = 50.0;
        }
    });
    lock_target_via_rpc(&ctx, &mut s_gal, front_sys, gal_ship, ihub_id).await;
    let _ = drain_notif_events(&ctx, gal_cid);
    for _ in 0..8 {
        let _ = rpc_call(&ctx, &mut s_gal, "dogmaIM", "Activate", &[
            PyRep::Integer(998_000),
            PyRep::String("targetAttack".into()),
            PyRep::Integer(ihub_id),
        ]).await;
        step_weapon_ticks(&ctx, front_sys).await;
        if ctx.db.with_snapshot(|s| s.fw_runtime.pending_downtime_captures.get(&front_sys) == Some(&500_004)) {
            break;
        }
        ctx.advance_secs(25);
    }
    // Destroyed I-Hub only queues the capture; the occupier flips at the next daily downtime.
    assert_eq!(ctx.db.with_snapshot(|s| s.fw_system_occupations[&front_sys].occupier_faction_id), 500_001);
    let dt_sec = ctx.systems.downtime_utc_secs_of_day();
    ctx.advance_secs((86_400 + dt_sec - ctx.now_unix() % 86_400) % 86_400 + 5);
    assert_eq!(ctx.db.with_snapshot(|s| s.fw_system_occupations[&front_sys].occupier_faction_id), 500_004);
    assert!(drain_notif_events(&ctx, gal_cid).iter().any(|e| e == "OnWarzoneOccupationStateUpdated"));
    set_pilot_location(&ctx, &mut s_gal, cmd_sys, None);
    let cmd_after = rpc_call(&ctx, &mut s_gal, "fwWarzoneSolarsystem", "GetLocalOccupationState", &[]).await.unwrap();
    let PyRep::Tuple(cmd_tup) = &cmd_after else { panic!("expected Tuple, got {cmd_after:?}") };
    assert_eq!(cmd_tup[1].dict_get("adjacencyState").and_then(|v| v.as_i64()), Some(STATUS_FRONTLINE));
    set_pilot_location(&ctx, &mut s_gal, front_sys, None);

    // 5. Live militia PvP ship kill updates `facWarMgr.GetStats_TopAndAllKillsAndVPs`!
    let mut victim_cs = ctx.build_ship_combat_state(cal_cid, cal_ship, 639);
    victim_cs.shield_hp = 100.0;
    victim_cs.armor_hp = 100.0;
    victim_cs.structure_hp = 100.0;
    let _ = ctx.systems.get_or_spawn(front_sys).spawn_ball_with_combat(make_ball(cal_ship, 639, DVec3::new(202_500.0, 0.0, 0.0)), victim_cs).await;
    let atk_cs = ctx.build_ship_combat_state(gal_cid, gal_ship, 19_722);
    let _ = ctx.systems.get_or_spawn(front_sys).spawn_ball_with_combat(make_ball(gal_ship, 19_722, DVec3::new(200_000.0, 0.0, 0.0)), atk_cs).await;
    lock_target_via_rpc(&ctx, &mut s_gal, front_sys, gal_ship, cal_ship).await;
    ctx.set_invention_roll_override(Some(0.05));
    rpc_call(&ctx, &mut s_gal, "dogmaIM", "Activate", &[PyRep::Integer(998_000), PyRep::Integer(cal_ship)])
        .await.expect("destroy enemy militia ship");

    // Add historical FW stat events at distinct rolling window boundaries:
    // Event 1 (just occurred): 1 kill, 20 VP (within yesterday < 24h, lastWeek < 7d, total)
    // Event 2 (2 days ago): 2 kills, 40 VP (outside yesterday, within lastWeek < 7d, total)
    // Event 3 (10 days ago): 3 kills, 60 VP (outside 7d, folded into total and pruned from stat_events)
    let now_sec = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        snap.fw_runtime.stat_events.push(eve_db::PersistedFwStatEvent {
            timestamp_unix: now_sec - 2 * 86_400,
            character_id: gal_cid,
            corporation_id: gal_corp,
            alliance_id: None,
            faction_id: 500_004,
            kills: 2,
            victory_points: 40,
        });
        snap.fw_runtime.stat_events.push(eve_db::PersistedFwStatEvent {
            timestamp_unix: now_sec - 10 * 86_400,
            character_id: gal_cid,
            corporation_id: gal_corp,
            alliance_id: None,
            faction_id: 500_004,
            kills: 3,
            victory_points: 60,
        });
        eve_server::services::fw_services::prune_and_fold_fw_stat_events_in_snap(snap, now_sec);
    });

    let lb = rpc_call(&ctx, &mut s_gal, "facWarMgr", "GetStats_TopAndAllKillsAndVPs", &[]).await.unwrap();
    let find_stat = |cat_name: &str, field_key: &str, id_key: &str, expected_id: i64| -> i64 {
        let sub = lb.dict_get(cat_name).unwrap();
        let list = sub.dict_get(field_key).and_then(|v| match v { PyRep::List(l) => Some(l), _ => None }).unwrap();
        list.iter()
            .find(|row| row.dict_get(id_key).and_then(|v| v.as_i64()) == Some(expected_id))
            .and_then(|row| row.dict_get("amount").and_then(|v| v.as_i64()))
            .unwrap_or(0)
    };

    // Verify rolling window progression: yesterday (1) < lastWeek (1+2=3) < total (1+2+3=6)
    assert_eq!(find_stat("characters", "killsY", "characterID", gal_cid), 1);
    assert_eq!(find_stat("characters", "killsLW", "characterID", gal_cid), 3);
    assert_eq!(find_stat("characters", "killsTotal", "characterID", gal_cid), 6);
    assert!(find_stat("characters", "killsY", "characterID", gal_cid) < find_stat("characters", "killsLW", "characterID", gal_cid));
    assert!(find_stat("characters", "killsLW", "characterID", gal_cid) < find_stat("characters", "killsTotal", "characterID", gal_cid));

    assert_eq!(find_stat("corporations", "killsY", "corporationID", gal_corp), 1);
    assert_eq!(find_stat("corporations", "killsLW", "corporationID", gal_corp), 3);
    assert_eq!(find_stat("corporations", "killsTotal", "corporationID", gal_corp), 6);

    assert_eq!(find_stat("factions", "killsY", "factionID", 500_004), 1);
    assert_eq!(find_stat("factions", "killsLW", "factionID", 500_004), 3);
    assert_eq!(find_stat("factions", "killsTotal", "factionID", 500_004), 6);

    // Verify rolling window progression for victory points: yesterday (0, PvP kills award LP not VP) < lastWeek (40) < total (100)
    assert_eq!(find_stat("characters", "vpY", "characterID", gal_cid), 0);
    assert_eq!(find_stat("characters", "vpLW", "characterID", gal_cid), 40);
    assert_eq!(find_stat("characters", "vpTotal", "characterID", gal_cid), 100);
    assert!(find_stat("characters", "vpY", "characterID", gal_cid) < find_stat("characters", "vpLW", "characterID", gal_cid));
    assert!(find_stat("characters", "vpLW", "characterID", gal_cid) < find_stat("characters", "vpTotal", "characterID", gal_cid));

    assert_eq!(find_stat("corporations", "vpY", "corporationID", gal_corp), 0);
    assert_eq!(find_stat("corporations", "vpLW", "corporationID", gal_corp), 40);
    assert_eq!(find_stat("corporations", "vpTotal", "corporationID", gal_corp), 100);

    assert_eq!(find_stat("factions", "vpY", "factionID", 500_004), 0);
    assert_eq!(find_stat("factions", "vpLW", "factionID", 500_004), 40);
    assert_eq!(find_stat("factions", "vpTotal", "factionID", 500_004), 100);

    // Verify bounded log: events older than 7 days were folded and pruned from active stat_events
    ctx.db.with_snapshot(|snap| {
        assert!(snap.fw_runtime.stat_events.iter().all(|e| now_sec.saturating_sub(e.timestamp_unix) <= 7 * 86_400));
        assert_eq!(snap.fw_runtime.historical_char_totals.get(&gal_cid), Some(&(3, 60)));
    });

    // 6. SDE FW Agent (`agent_type_id == 9`) mission gating (`NotEnlistedInMilitia`) & repeatable acceptance!
    let (fw_agent_id, fw_sys, fw_sta, fw_fac) = ctx.sde.agents.values()
        .filter(|a| a.agent_type_id == 9 && a.solar_system_id.is_some() && a.station_id.is_some())
        .find_map(|a| {
            let fac = ctx.sde.factions.values().find(|f| f.militia_corporation_id == Some(a.corporation_id))?.faction_id as i64;
            Some((a.agent_id, a.solar_system_id.unwrap(), a.station_id.unwrap(), fac))
        })
        .expect("SDE FW agent");
    s_norole.solar_system_id = Some(fw_sys);
    s_norole.station_id = Some(fw_sta);
    ctx.db.with_mut_snapshot(|s| {
        let ch = s.characters.get_mut(&90_000_023).unwrap();
        ch.solar_system_id = fw_sys;
        ch.station_id = Some(fw_sta);
        ch.war_faction_id = None;
    });
    assert_eq!(
        rpc_call(&ctx, &mut s_norole, "agentMgr", "DoAction", &[PyRep::Integer(fw_agent_id), PyRep::String("RequestMission".into())])
            .await.unwrap_err().code,
        "NotEnlistedInMilitia"
    );
    ctx.db.with_mut_snapshot(|s| {
        let ch = s.characters.get_mut(&90_000_023).unwrap();
        ch.war_faction_id = Some(fw_fac);
        ch.standings.insert(fw_fac, 8.0);
    });
    s_norole.war_faction_id = Some(fw_fac);
    rpc_call(&ctx, &mut s_norole, "agentMgr", "DoAction", &[PyRep::Integer(fw_agent_id), PyRep::String("RequestMission".into())])
        .await.expect("Request FW mission");
    rpc_call(&ctx, &mut s_norole, "agentMgr", "DoAction", &[PyRep::Integer(fw_agent_id), PyRep::String("AcceptMission".into())])
        .await.expect("Accept FW mission");
    let journal = rpc_call(&ctx, &mut s_norole, "agentMgr", "GetMyJournalDetails", &[]).await.unwrap();
    assert!(format!("{journal:?}").contains(&fw_agent_id.to_string()));

    drop(ctx);
    let _ = std::fs::remove_file(&db_path);
}

}

#[allow(dead_code, unused_imports)]
pub(crate) mod insurgency_corruption_suppression {
    use super::*;

//  Part 3 of FW & Insurgency closure tests: Pirate Enlistment, 48h Forecasting -> 7d Active -> WindDown Lifecycle, Spread & Stage 5 Corruption Effects (`SO-5`, `Q-8`).


use crate::fw_warzone_insurgency::setup_warzone_chain;
use crate::sovereignty_equinox_gameplay::{
    drain_notif_events, fit_siege_artillery, lock_target_via_rpc, make_ball,
    register_online_pilot, rpc_call,
};
use eve_db::{InventoryFlag, InventoryItemRecord, TrainedSkillRecord};
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::combat_npc_police::{
    is_concord_suppressed_in_system, is_sentry_disabled_by_corruption,
};
use eve_server::services::fw_gameplay::PIRATE_FOB_STRUCTURE_BASE_ID;
use eve_server::services::insurgency_services::{
    add_system_corruption, add_system_suppression, ACTIVE_DURATION_SECS,
    CAMPAIGN_STATE_ACTIVE, CAMPAIGN_STATE_ANTIPIRATE_WIN, CAMPAIGN_STATE_FORECASTING,
    CAMPAIGN_STATE_NO_WINNER, CAMPAIGN_STATE_PIRATE_WIN, FORECASTING_DURATION_SECS,
    WINDDOWN_DURATION_SECS,
};
use glam::DVec3;

#[tokio::test]
async fn test_pirate_insurgency_lifecycle_spread_and_stage5_highsec_lowsec_effects() {
    // parity: feat:fw.fw-pirate
    // parity: feat:fw.insurgency-lifecycle
    // parity: feat:fw.insurgency-spread
    // parity: feat:fw.corruption-stage5-highsec-bubbles-concord
    // parity: notify:OnInsurgencyCampaignStartedForLocation
    // parity: notify:OnInsurgencyCampaignUpdatedForLocation
    // parity: notify:OnInsurgencyStateChangedForFaction
    eve_testkit::attest(&[
        "feat:fw.fw-pirate",
        "feat:fw.insurgency-lifecycle",
        "feat:fw.insurgency-spread",
        "feat:fw.corruption-stage5-highsec-bubbles-concord",
        "notify:OnInsurgencyCampaignStartedForLocation",
        "notify:OnInsurgencyCampaignUpdatedForLocation",
        "notify:OnInsurgencyStateChangedForFaction",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let (_enemy_sys, front_sys, _cmd_sys, _rear_sys) = setup_warzone_chain(&ctx);

    let pir_cid = 90_000_031i64;
    let pir_ship = 100_000_031i64;
    let mut s_pir = register_online_pilot(
        &ctx, 83_001, "demo", "demo-secret", pir_cid, 98_000_031, None,
        front_sys, pir_ship, 22_456, DVec3::ZERO, 0, // Sabre (Interdictor) at complex grid [0,0,0]
    ).await;

    // 1. Pirate militia enlistment (`500010` Guristas) + auto-seeding Frontline Insurgency in `48h Forecasting`!
    rpc_call(&ctx, &mut s_pir, "fwCharacterEnlistmentMgr", "CreateMyDirectEnlistment", &[PyRep::Integer(500_010)])
        .await.expect("enlist Guristas");
    let _ = drain_notif_events(&ctx, pir_cid);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    let notifs_start = ctx.drain_notifications_for_char(pir_cid);
    let start_ev = notifs_start.iter().find(|n| n.event == "OnInsurgencyCampaignStartedForLocation").expect("OnInsurgencyCampaignStartedForLocation");
    assert_eq!(start_ev.args.len(), 1, "OnInsurgencyCampaignStartedForLocation arity 1");
    assert!(start_ev.args[0].dict_get("coveredSolarsystemIDs").is_some(), "snapshot must contain coveredSolarsystemIDs");
    let state_start = notifs_start.iter().find(|n| n.event == "OnInsurgencyStateChangedForFaction").expect("OnInsurgencyStateChangedForFaction on start");
    assert_eq!(state_start.args[0].as_i64(), Some(CAMPAIGN_STATE_FORECASTING));
    assert_eq!(state_start.args[1].as_i64(), Some(500_010));

    let (camp_id, start_systems) = ctx.db.with_snapshot(|s| {
        let c = s.insurgency_runtime.campaigns.values().next().expect("auto-seeded campaign");
        (c.campaign_id, c.systems.clone())
    });
    assert!(start_systems.contains(&front_sys) && !start_systems.is_empty());

    let vis = rpc_call(&ctx, &mut s_pir, "insurgencySolarsystem", "GetAllVisibleCampaigns", &[]).await.unwrap();
    let snap_fc = rpc_call(&ctx, &mut s_pir, "insurgencySolarsystem", "GetLocalCampaignClientSnapshot", &[]).await.unwrap();
    assert!(format!("{vis:?}").contains(&camp_id.to_string()));
    let PyRep::Tuple(fc_tup) = &snap_fc else { panic!("expected Tuple, got {snap_fc:?}") };
    assert_eq!(fc_tup[1].dict_get("phase").and_then(|v| v.as_str()), Some("Forecasting"));

    // During 48h Forecasting: zero non-FOB insurgency sites exist and Pirate FOB (`1_897_000_000 + camp_id`) is invulnerable!
    let non_fob_fc = ctx.db.with_snapshot(|s| {
        s.insurgency_runtime.insurgency_instances.iter().filter(|i| i.site_type != "PirateStronghold").count()
    });
    assert_eq!(non_fob_fc, 0);
    let fob_id = PIRATE_FOB_STRUCTURE_BASE_ID + camp_id;
    let emp_cid = 90_000_032i64;
    let emp_ship = 100_000_032i64;
    let mut s_emp = register_online_pilot(
        &ctx, 83_002, "demo2", "demo2-secret", emp_cid, 98_000_032, None,
        front_sys, emp_ship, 639, DVec3::new(97_000.0, 0.0, 45_000.0), 0,
    ).await;
    rpc_call(&ctx, &mut s_emp, "fwCharacterEnlistmentMgr", "CreateMyDirectEnlistment", &[PyRep::Integer(500_001)])
        .await.expect("enlist Caldari");
    fit_siege_artillery(&ctx, emp_cid, emp_ship, 999_000);
    lock_target_via_rpc(&ctx, &mut s_emp, front_sys, emp_ship, fob_id).await;
    assert_eq!(
        rpc_call(&ctx, &mut s_emp, "dogmaIM", "Activate", &[
            PyRep::Integer(999_000),
            PyRep::String("targetAttack".into()),
            PyRep::Integer(fob_id),
        ]).await.unwrap_err().code,
        "FobInvulnerableInPhase"
    );

    // 2. Advance 48h into `7d Active`: combat insurgency sites spawn, and 60s destiny pulses at `[0,0,0]`
    //    advance Corruption stages, emitting `OnInsurgencyCampaignUpdatedForLocation` and spreading at Stage 3!
    ctx.advance_secs(FORECASTING_DURATION_SECS + 10);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    let notifs_act = ctx.drain_notifications_for_char(pir_cid);
    let state_act = notifs_act.iter().find(|n| n.event == "OnInsurgencyStateChangedForFaction").expect("OnInsurgencyStateChangedForFaction on active");
    assert_eq!(state_act.args[0].as_i64(), Some(CAMPAIGN_STATE_ACTIVE));
    assert_eq!(state_act.args[1].as_i64(), Some(500_010));
    let snap_act = rpc_call(&ctx, &mut s_pir, "insurgencySolarsystem", "GetLocalCampaignClientSnapshot", &[]).await.unwrap();
    let PyRep::Tuple(act_tup) = &snap_act else { panic!("expected Tuple, got {snap_act:?}") };
    assert_eq!(act_tup[1].dict_get("phase").and_then(|v| v.as_str()), Some("Active"));

    let _ = drain_notif_events(&ctx, pir_cid);
    let front_beacon = ctx.db.with_snapshot(|s| s.system_states[&front_sys].fw_complex_position.expect("insurgency beacon spawned"));
    let sys_front_h = ctx.systems.get_or_spawn(front_sys);
    let _ = sys_front_h.spawn_ball(make_ball(pir_ship, 621, DVec3::from_array(front_beacon))).await;
    for _ in 0..3 {
        sys_front_h.step_manual(60.0).await.unwrap();
        eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    }
    let stage3_notifs = drain_notif_events(&ctx, pir_cid);
    assert!(stage3_notifs.iter().any(|e| e == "OnInsurgencyCampaignUpdatedForLocation"));
    let after_spread = ctx.db.with_snapshot(|s| s.insurgency_runtime.campaigns[&camp_id].systems.clone());
    assert!(after_spread.len() > start_systems.len(), "Corruption Stage 3 spreads insurgency to adjacent 0.0..=0.8 system");
    assert!(!is_sentry_disabled_by_corruption(&ctx, front_sys), "Stage 3 corruption does not disable gate/station sentry guns yet");

    // Verify 0.4-rounded (`0.40 < true_sec < 0.45`) low-sec sentry guns:
    // (a) engage when system is NOT in an active insurgency campaign even if stale `insurgency_corruption_stage == 5`
    // (b) cease fire (`(false, 0.0)`) once covered by an active insurgency campaign at Corruption Stage 5!
    let ls_044_sys = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| s.solar_system_id < 30_100_000 && s.security_status > 0.43 && s.security_status < 0.45 && !after_spread.contains(&s.solar_system_id))
        .expect("0.44 true-sec low-sec system in SDE")
        .solar_system_id;
    let ls_044_ship = 100_000_039i64;
    let ls_044_h = ctx.systems.get_or_spawn(ls_044_sys);
    let _ = ls_044_h
        .spawn_ball_with_combat(make_ball(ls_044_ship, 621, DVec3::new(5_000.0, 0.0, 0.0)), ctx.build_type_combat_state(621))
        .await;
    ctx.db.with_mut_snapshot(|s| {
        s.system_states.entry(ls_044_sys).or_default().insurgency_corruption_stage = 5;
    });
    assert!(!is_sentry_disabled_by_corruption(&ctx, ls_044_sys), "Non-campaign system must not disable sentries");
    let (eng_non_camp, dmg_non_camp) = eve_server::services::combat_crimewatch::step_sentry_gun_grid_aggression(
        &ctx, ls_044_sys, pir_cid, ls_044_ship, DVec3::ZERO, true,
    ).await;
    assert!(eng_non_camp && dmg_non_camp > 0.0, "Sentries must engage in 0.44 low-sec outside active insurgency");
    ctx.db.with_mut_snapshot(|s| {
        let c = s.insurgency_runtime.campaigns.get_mut(&camp_id).unwrap();
        c.systems.push(ls_044_sys);
        c.corruption_points.insert(ls_044_sys, 1_000);
    });
    assert!(is_sentry_disabled_by_corruption(&ctx, ls_044_sys), "0.44 true-sec (0.4 rounded) Stage 5 insurgency disables sentries");
    let (eng_corr5, dmg_corr5) = eve_server::services::combat_crimewatch::step_sentry_gun_grid_aggression(
        &ctx, ls_044_sys, pir_cid, ls_044_ship, DVec3::ZERO, true,
    ).await;
    assert_eq!((eng_corr5, dmg_corr5), (false, 0.0), "Stage 5 corruption in 0.44 low-sec disables step_sentry_gun_grid_aggression");

    // Verify mid-campaign stage reduction to 0 when points drop to 0 in add_system_corruption and add_system_suppression:
    let (c_pts_zero, c_stage_zero) = add_system_corruption(&ctx, camp_id, ls_044_sys, -10_000);
    assert_eq!((c_pts_zero, c_stage_zero), (0, 0));
    assert_eq!(ctx.db.with_snapshot(|s| s.system_states[&ls_044_sys].insurgency_corruption_stage), 0);

    let (s_pts_up, s_stage_up) = add_system_suppression(&ctx, camp_id, ls_044_sys, 450);
    assert_eq!((s_pts_up, s_stage_up), (450, 3));
    assert_eq!(ctx.db.with_snapshot(|s| s.system_states[&ls_044_sys].insurgency_suppression_stage), 3);
    let (s_pts_zero, s_stage_zero) = add_system_suppression(&ctx, camp_id, ls_044_sys, -10_000);
    assert_eq!((s_pts_zero, s_stage_zero), (0, 0));
    assert_eq!(ctx.db.with_snapshot(|s| s.system_states[&ls_044_sys].insurgency_suppression_stage), 0);

    // Pulse 2 more times on `front_sys` to reach Corruption Stage 5 (`1,000` pts)!
    for _ in 0..2 {
        sys_front_h.step_manual(60.0).await.unwrap();
        eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    }
    assert_eq!(ctx.db.with_snapshot(|s| s.system_states[&front_sys].insurgency_corruption_stage), 5);
    assert!(is_sentry_disabled_by_corruption(&ctx, front_sys), "Stage 5 corruption disables low-sec gate/station sentry guns");
    let mod_sys = rpc_call(&ctx, &mut s_pir, "securityMgr", "get_modified_systems", &[]).await.unwrap();
    assert!(format!("{mod_sys:?}").contains(&front_sys.to_string()));

    // 3. Low-Sec Stage 5 Corruption: `Interdiction Sphere Launcher I` (`22782`), `Mobile Small Warp Disruptor I` (`12198`),
    //    and `Bomb Launcher I` (`27914`) succeed without `LowSecDisallowed`!
    ctx.db.with_mut_snapshot(|snap| {
        let sk = snap.skills.entry(pir_cid).or_default();
        for sid in [3_435u32, 12_098, 28_073, 3_319, 11_584] {
            sk.insert(sid, TrainedSkillRecord { type_id: sid, level: 5, skill_points: 256_000.0 });
        }
        for (iid, tid, loc, flg, qty) in [
            (999_100, 22_782, pir_ship, InventoryFlag::HiSlot0, 1), // Interdiction Sphere Launcher I
            (999_101, 22_778, pir_ship, InventoryFlag::HiSlot0, 5), // Warp Disrupt Probe
            (999_102, 12_198, pir_ship, InventoryFlag::Cargo, 1),   // Mobile Small Warp Disruptor I
        ] {
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: tid,
                    owner_id: pir_cid,
                    location_id: loc,
                    flag: flg,
                    quantity: qty,
                    singleton: qty == 1 && flg == InventoryFlag::HiSlot0,
                    ..Default::default()
                },
            );
        }
        snap.module_online_states.insert(999_100, true);
    });
    ctx.invalidate_ship_fitting(pir_ship);
    assert!(rpc_call(&ctx, &mut s_pir, "dogmaIM", "Activate", &[PyRep::Integer(999_100)]).await.is_ok());
    assert!(rpc_call(&ctx, &mut s_pir, "structureDeployment", "Anchor", &[PyRep::Integer(999_102), PyRep::String("Bubble".into())]).await.is_ok());

    // Switch pirate ship to Manticore (`12032`) with Bomb Launcher I (`27914`) + Concussion Bomb (`27912`) in Low-Sec Stage 5!
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&pir_ship).unwrap().type_id = 12_032;
        snap.items.insert(
            999_110,
            InventoryItemRecord {
                item_id: 999_110,
                type_id: 27_914,
                owner_id: pir_cid,
                location_id: pir_ship,
                flag: InventoryFlag::HiSlot1,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            999_111,
            InventoryItemRecord {
                item_id: 999_111,
                type_id: 27_912,
                owner_id: pir_cid,
                location_id: pir_ship,
                flag: InventoryFlag::HiSlot1,
                quantity: 5,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(999_110, true);
    });
    ctx.invalidate_ship_fitting(pir_ship);
    let _ = sys_front_h.spawn_ball(make_ball(pir_ship, 12_032, DVec3::new(15_000.0, 0.0, 0.0))).await;
    assert!(rpc_call(&ctx, &mut s_pir, "dogmaIM", "Activate", &[PyRep::Integer(999_110)]).await.is_ok());

    // 4. High-Sec (`0.5..=0.8`) Stage 5 Corruption: live beacon pulses corrupt the stargate-spread High-Sec system to Stage 5 ->
    //    ship PvP gives `Suspect` without CONCORD, whereas Capsule (`670`) PvP still gives `Criminal` + CONCORD!
    let hs_sys = *after_spread
        .iter()
        .find(|&&s| ctx.sde.solar_systems[&s].security_status >= 0.5)
        .expect("Stage 3 stargate spread added adjacent 0.5..=0.8 High-Sec system");
    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&pir_cid).unwrap().solar_system_id = hs_sys;
        s.items.get_mut(&pir_ship).unwrap().type_id = 639;
        s.items.get_mut(&pir_ship).unwrap().location_id = hs_sys as i64;
    });
    s_pir.solar_system_id = Some(hs_sys);
    fit_siege_artillery(&ctx, pir_cid, pir_ship, 999_200);
    let hs_beacon = ctx.db.with_snapshot(|s| s.system_states[&hs_sys].fw_complex_position.expect("insurgency beacon spawned"));
    let hs_h = ctx.systems.get_or_spawn(hs_sys);
    let cs_pir_hs = ctx.build_ship_combat_state(pir_cid, pir_ship, 19_722);
    let _ = hs_h.spawn_ball_with_combat(make_ball(pir_ship, 19_722, DVec3::from_array(hs_beacon)), cs_pir_hs).await;
    for _ in 0..5 {
        hs_h.step_manual(60.0).await.unwrap();
        eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    }
    assert!(is_concord_suppressed_in_system(&ctx, hs_sys));

    let neutral_cid = 90_000_033i64;
    let neutral_ship = 100_000_033i64;
    let _s_neut = register_online_pilot(
        &ctx, 83_003, "demo3", "demo3-secret", neutral_cid, 98_000_033, None,
        hs_sys, neutral_ship, 621, DVec3::from_array(hs_beacon) + DVec3::new(2_500.0, 0.0, 0.0), 0,
    ).await;
    lock_target_via_rpc(&ctx, &mut s_pir, hs_sys, pir_ship, neutral_ship).await;
    rpc_call(&ctx, &mut s_pir, "dogmaIM", "Activate", &[PyRep::Integer(999_200), PyRep::Integer(neutral_ship)])
        .await.expect("attack ship in High-Sec Stage 5");
    let now_hs = ctx.now_unix();
    let (ship_susp, ship_crim, ship_concord) = ctx.crimewatch.with_crimewatch(|cw| {
        let st = &cw.states[&pir_cid];
        (st.is_suspect(now_hs), st.is_criminal(now_hs), st.pending_concord_arrival.is_some())
    });
    assert_eq!((ship_susp, ship_crim, ship_concord), (true, false, false), "High-Sec Stage 5 ship attack triggers Suspect without CONCORD");

    // Attack a Capsule (`670`) in High-Sec Stage 5 -> triggers Criminal + CONCORD!
    let pod_cid = 90_000_034i64;
    let pod_id = 100_000_034i64;
    let _s_pod = register_online_pilot(
        &ctx, 83_004, "demo4", "demo4-secret", pod_cid, 98_000_034, None,
        hs_sys, pod_id, 670, DVec3::from_array(hs_beacon) + DVec3::new(2_800.0, 0.0, 0.0), 0,
    ).await;
    ctx.advance_secs(25);
    lock_target_via_rpc(&ctx, &mut s_pir, hs_sys, pir_ship, pod_id).await;
    // The turret is still cycling on the ship target: stop it before re-targeting the capsule.
    let _ = rpc_call(&ctx, &mut s_pir, "dogmaIM", "Deactivate", &[PyRep::Integer(999_200)]).await;
    ctx.advance_secs(15);
    rpc_call(&ctx, &mut s_pir, "dogmaIM", "Activate", &[PyRep::Integer(999_200), PyRep::Integer(pod_id)])
        .await.expect("attack capsule in High-Sec Stage 5");
    let now_pod = ctx.now_unix();
    let (pod_crim, pod_concord) = ctx.crimewatch.with_crimewatch(|cw| {
        let st = &cw.states[&pir_cid];
        (st.is_criminal(now_pod), st.pending_concord_arrival.is_some())
    });
    assert_eq!((pod_crim, pod_concord), (true, true), "High-Sec Stage 5 capsule attack still triggers Criminal + CONCORD");

    // 5. Advance past 7d Active + 48h WindDown -> campaign ends, clearing all Corruption stages and emitting `OnInsurgencyStateChangedForFaction`!
    let _ = drain_notif_events(&ctx, pir_cid);
    ctx.advance_secs(ACTIVE_DURATION_SECS + WINDDOWN_DURATION_SECS + 120);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    let end_notifs = ctx.drain_notifications_for_char(pir_cid);
    let end_ev = end_notifs.iter().find(|n| n.event == "OnInsurgencyStateChangedForFaction").expect("OnInsurgencyStateChangedForFaction on campaign end");
    assert_eq!(end_ev.args[0].as_i64(), Some(CAMPAIGN_STATE_NO_WINNER));
    assert_eq!(end_ev.args[1].as_i64(), Some(500_010));
    assert_eq!(ctx.db.with_snapshot(|s| s.system_states[&hs_sys].insurgency_corruption_stage), 0);

    // Assert OnInsurgencyStateChangedForFaction matches authoritative V24 constants for PirateWin and AntiPirateWin:
    let test_camp_p = 888_001i64;
    eve_server::services::insurgency_services::init_insurgency_campaign(&ctx, test_camp_p, 500_010, 500_001, hs_sys, vec![hs_sys]);
    let _ = ctx.drain_notifications_for_char(pir_cid);
    ctx.db.with_mut_snapshot(|s| {
        s.insurgency_runtime.campaign_outcomes.insert(test_camp_p, "PirateWin".into());
    });
    eve_server::services::insurgency_services::end_insurgency_campaign(&ctx, test_camp_p);
    let p_notifs = ctx.drain_notifications_for_char(pir_cid);
    let p_ev = p_notifs.iter().find(|n| n.event == "OnInsurgencyStateChangedForFaction").expect("PirateWin state notif");
    assert_eq!(p_ev.args[0].as_i64(), Some(CAMPAIGN_STATE_PIRATE_WIN));

    let test_camp_e = 888_002i64;
    eve_server::services::insurgency_services::init_insurgency_campaign(&ctx, test_camp_e, 500_010, 500_001, hs_sys, vec![hs_sys]);
    let _ = ctx.drain_notifications_for_char(pir_cid);
    ctx.db.with_mut_snapshot(|s| {
        s.insurgency_runtime.campaign_outcomes.insert(test_camp_e, "AntiPirateWin".into());
    });
    eve_server::services::insurgency_services::end_insurgency_campaign(&ctx, test_camp_e);
    let e_notifs = ctx.drain_notifications_for_char(pir_cid);
    let e_ev = e_notifs.iter().find(|n| n.event == "OnInsurgencyStateChangedForFaction").expect("AntiPirateWin state notif");
    assert_eq!(e_ev.args[0].as_i64(), Some(CAMPAIGN_STATE_ANTIPIRATE_WIN));
}

}

#[allow(dead_code, unused_imports)]
pub(crate) mod insurgency_battlefields_zones {
    use super::*;

//  Part 4 of FW & Insurgency closure tests: Pirate FOB Docking & 3h±1h Armor Siege, Ambition Scaling, Surrender I-Hub Vulnerability & Contribution Payouts (`SO-5`, `Q-8`).


use crate::fw_warzone_insurgency::setup_warzone_chain;
use crate::sovereignty_equinox_gameplay::{
    drain_notif_events, fit_siege_artillery, lock_target_via_rpc, make_ball, open_sqlite_ctx, register_online_pilot,
    rpc_call, step_weapon_ticks, temp_sqlite_path,
};
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::fw_gameplay::{
    INSURGENCY_MAX_PAYOUT_LP, PIRATE_FOB_STRUCTURE_BASE_ID, SURRENDER_IHUB_VULNERABILITY_DELAY_SECS,
    TYPE_FW_IHUB,
};
use eve_server::services::insurgency_services::{
    ACTIVE_DURATION_SECS, FORECASTING_DURATION_SECS, WINDDOWN_DURATION_SECS,
};
use glam::DVec3;

#[tokio::test]
async fn test_pirate_fob_docking_siege_ambition_surrender_ihub_and_payouts() {
    // parity: feat:fw.insurgency-fob
    // parity: feat:fw.insurgency-fob-ambition
    // parity: feat:fw.insurgency-ambition-victory
    // parity: feat:fw.insurgency-contribution-payout
    // parity: notify:OnInsurgencyCampaignEndingForLocation
    eve_testkit::attest(&[
        "feat:fw.insurgency-fob",
        "feat:fw.insurgency-fob-ambition",
        "feat:fw.insurgency-ambition-victory",
        "feat:fw.insurgency-contribution-payout",
        "notify:OnInsurgencyCampaignEndingForLocation",
    ]);

    let db_path = temp_sqlite_path("fw_insurgency_f4");
    let ctx = open_sqlite_ctx(&db_path, true);
    let (_enemy_sys, front_sys, _cmd_sys, _rear_sys) = setup_warzone_chain(&ctx);

    let fob_pos = DVec3::new(95_000.0, 0.0, 45_000.0);
    let pir_cid = 90_000_041i64;
    let pir_ship = 100_000_041i64;
    let emp_cid = 90_000_042i64;
    let emp_ship = 100_000_042i64;
    let ang_cid = 90_000_043i64;
    let ang_ship = 100_000_043i64;

    let mut s_pir = register_online_pilot(
        &ctx, 84_001, "demo", "demo-secret", pir_cid, 98_000_041, None,
        front_sys, pir_ship, 621, fob_pos + DVec3::new(500.0, 0.0, 0.0), 0,
    ).await;
    let mut s_emp = register_online_pilot(
        &ctx, 84_002, "demo2", "demo2-secret", emp_cid, 98_000_042, None,
        front_sys, emp_ship, 639, fob_pos + DVec3::new(600.0, 0.0, 0.0), 0,
    ).await;
    let mut s_ang = register_online_pilot(
        &ctx, 84_003, "demo3", "demo3-secret", ang_cid, 98_000_043, None,
        front_sys, ang_ship, 621, fob_pos + DVec3::new(700.0, 0.0, 0.0), 0,
    ).await;

    rpc_call(&ctx, &mut s_pir, "fwCharacterEnlistmentMgr", "CreateMyDirectEnlistment", &[PyRep::Integer(500_010)]).await.unwrap();
    rpc_call(&ctx, &mut s_emp, "fwCharacterEnlistmentMgr", "CreateMyDirectEnlistment", &[PyRep::Integer(500_001)]).await.unwrap();
    rpc_call(&ctx, &mut s_ang, "fwCharacterEnlistmentMgr", "CreateMyDirectEnlistment", &[PyRep::Integer(500_011)]).await.unwrap();

    // Seed FW I-Hub (`32226`) in `front_sys` and auto-seed Campaign 1 (`Guristas` `500010`).
    let _ = rpc_call(&ctx, &mut s_emp, "dungeonInstanceCacheMgr", "GetFactionWarfareInstances", &[]).await.unwrap();
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    let camp1 = ctx.db.with_snapshot(|s| *s.insurgency_runtime.campaigns.keys().next().unwrap());
    let fob1 = PIRATE_FOB_STRUCTURE_BASE_ID + camp1;

    // 1. Pirate FOB (`79172`) docking access via `beyonce.CmdDock`: Guristas (`500010`) allowed, Caldari (`500001`) & Angel (`500011`) rejected!
    assert_eq!(
        rpc_call(&ctx, &mut s_emp, "beyonce", "CmdDock", &[PyRep::Integer(fob1)]).await.unwrap_err().code,
        "FobDockingRestrictedToPirateMilitia"
    );
    assert_eq!(
        rpc_call(&ctx, &mut s_ang, "beyonce", "CmdDock", &[PyRep::Integer(fob1)]).await.unwrap_err().code,
        "FobDockingRestrictedToPirateMilitia"
    );
    rpc_call(&ctx, &mut s_pir, "beyonce", "CmdDock", &[PyRep::Integer(fob1)]).await.expect("Guristas pilot docks at Guristas FOB");
    assert_eq!(s_pir.structure_id, Some(fob1));
    fast_forward_secs(&ctx, 12);
    rpc_call(&ctx, &mut s_pir, "beyonce", "Undock", &[]).await.expect("undock from Guristas FOB");

    // 2. Campaign 1 Pirate Victory (`5 + 0` Stage-5 corrupted systems via live 60s pulses + `tick_background_cluster`):
    //    - Pirate Ambition modifier rises `0 -> 1` (requiring `6` Stage-5 systems next campaign!)
    //    - Winner (`pir_cid`, 100% >= 45% share) gets `1,500,000 LP` with Guristas (`1000434`/`militia_lp_corp_id`) + ISK
    //    - Loser (`emp_cid`, 100% >= 45% share) gets `0.4x` (`600,000 LP`) with State Protectorate (`1000180`) + ISK
    //    - Surrendered FOB system's FW I-Hub unlocks after `25h` (`90,000s`) for live `dogmaIM.Activate` weapon attack!
    fast_forward_secs(&ctx, FORECASTING_DURATION_SECS + 10);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;

    // Give Empire pilot 1 pulse of Suppression contribution on `front_sys` before Pirate corrupts 5 systems to Stage 5.
    let front_beacon = DVec3::from_array(ctx.db.with_snapshot(|s| s.system_states[&front_sys].fw_complex_position.expect("insurgency beacon spawned")));
    let front_h = ctx.systems.get_or_spawn(front_sys);
    let _ = front_h.remove_ball(pir_ship).await;
    let _ = front_h.remove_ball(emp_ship).await;
    let _ = front_h.spawn_ball(make_ball(pir_ship, 621, front_beacon + DVec3::new(90_000.0, 0.0, 0.0))).await;
    let _ = front_h.spawn_ball(make_ball(emp_ship, 639, front_beacon)).await;
    front_h.step_manual(60.0).await.unwrap();
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    let _ = front_h.remove_ball(emp_ship).await;
    let _ = front_h.spawn_ball(make_ball(emp_ship, 639, front_beacon + DVec3::new(90_000.0, 0.0, 0.0))).await;

    let emp_lp0 = ctx.db.with_snapshot(|s| s.characters[&emp_cid].loyalty_points.get(&1_000_180).copied().unwrap_or(0));
    let pir_lp0: i64 = ctx.db.with_snapshot(|s| s.characters[&pir_cid].loyalty_points.values().sum());
    let emp_isk0 = ctx.db.with_snapshot(|s| s.characters[&emp_cid].balance.to_wire_f64());
    let pir_isk0 = ctx.db.with_snapshot(|s| s.characters[&pir_cid].balance.to_wire_f64());

    // Corrupt 5 campaign systems to Stage 5 via live 60s destiny pulses (Stage 3 on `front_sys` spreads the 5th stargate-connected system!).
    for idx in 0..5usize {
        let (target_sys, target_beacon) = ctx.db.with_snapshot(|s| (
            s.insurgency_runtime.campaigns[&camp1].systems[idx],
            s.system_states[&s.insurgency_runtime.campaigns[&camp1].systems[idx]].fw_complex_position.expect("insurgency beacon spawned"),
        ));
        s_pir.solar_system_id = Some(target_sys);
        ctx.db.with_mut_snapshot(|s| {
            s.characters.get_mut(&pir_cid).unwrap().solar_system_id = target_sys;
            s.items.get_mut(&pir_ship).unwrap().location_id = target_sys as i64;
        });
        let sys_h = ctx.systems.get_or_spawn(target_sys);
        let _ = sys_h.remove_ball(pir_ship).await;
        let _ = sys_h.spawn_ball(make_ball(pir_ship, 621, DVec3::from_array(target_beacon))).await;
        for _ in 0..5 {
            sys_h.step_manual(60.0).await.unwrap();
            eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
        }
        let _ = sys_h.remove_ball(pir_ship).await;
    }

    assert_eq!(ctx.db.with_snapshot(|s| s.insurgency_runtime.campaign_outcomes.get(&camp1).cloned()), Some("PirateWin".into()));
    assert_eq!(ctx.db.with_snapshot(|s| s.insurgency_runtime.pirate_ambition_modifier[&500_010]), 1);
    let pir_lp_total: i64 = ctx.db.with_snapshot(|s| s.characters[&pir_cid].loyalty_points.values().sum::<i64>()) - pir_lp0;
    let emp_lp_gain = ctx.db.with_snapshot(|s| s.characters[&emp_cid].loyalty_points[&1_000_180]) - emp_lp0;
    assert_eq!(pir_lp_total, INSURGENCY_MAX_PAYOUT_LP, "winner at >=45% share receives max 1,500,000 LP");
    assert_eq!(emp_lp_gain, 600_000, "loser at >=45% share receives 0.4x (600,000 LP)");
    let pir_isk_gain = ctx.db.with_snapshot(|s| s.characters[&pir_cid].balance.to_wire_f64()) - pir_isk0;
    let emp_isk_gain = ctx.db.with_snapshot(|s| s.characters[&emp_cid].balance.to_wire_f64()) - emp_isk0;
    assert_eq!(pir_isk_gain, (INSURGENCY_MAX_PAYOUT_LP * 100) as f64, "winner received contribution-scaled ISK");
    assert_eq!(emp_isk_gain, (600_000 * 100) as f64, "loser received contribution-scaled ISK");

    // Surrendered `front_sys` FW I-Hub is still invulnerable at +24h, then unlocks at +25h (`SURRENDER_IHUB_VULNERABILITY_DELAY_SECS`)!
    let ihub_id = ctx.db.with_snapshot(|s| {
        s.structures.values().find(|st| st.solar_system_id == front_sys && st.type_id == TYPE_FW_IHUB).unwrap().structure_id
    });
    let mut s_gal = register_online_pilot(
        &ctx, 84_004, "demo4", "demo4-secret", 90_000_044, 98_000_044, None,
        front_sys, 100_000_044, 639, DVec3::new(202_000.0, 0.0, 0.0), 0,
    ).await;
    rpc_call(&ctx, &mut s_gal, "fwCharacterEnlistmentMgr", "CreateMyDirectEnlistment", &[PyRep::Integer(500_004)]).await.unwrap();
    fit_siege_artillery(&ctx, 90_000_044, 100_000_044, 999_400);

    fast_forward_secs(&ctx, 24 * 3_600);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    lock_target_via_rpc(&ctx, &mut s_gal, front_sys, 100_000_044, ihub_id).await;
    assert_eq!(
        rpc_call(&ctx, &mut s_gal, "dogmaIM", "Activate", &[
            PyRep::Integer(999_400),
            PyRep::String("targetAttack".into()),
            PyRep::Integer(ihub_id),
        ]).await.unwrap_err().code,
        "IHubInvulnerable"
    );
    fast_forward_secs(&ctx, SURRENDER_IHUB_VULNERABILITY_DELAY_SECS - 24 * 3_600 + 10);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    assert!(rpc_call(&ctx, &mut s_gal, "dogmaIM", "Activate", &[
        PyRep::Integer(999_400),
        PyRep::String("targetAttack".into()),
        PyRep::Integer(ihub_id),
    ]).await.is_ok());

    // Finish Campaign 1 WindDown -> emits `OnInsurgencyCampaignEndingForLocation` and auto-seeds Campaign 2!
    s_pir.solar_system_id = Some(front_sys);
    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&pir_cid).unwrap().solar_system_id = front_sys;
    });
    let _ = drain_notif_events(&ctx, pir_cid);
    fast_forward_secs(&ctx, WINDDOWN_DURATION_SECS + 60);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    assert!(drain_notif_events(&ctx, pir_cid).iter().any(|e| e == "OnInsurgencyCampaignEndingForLocation"));

    // 3. Campaign 2 Empire Underdog Win via Pirate FOB Siege:
    //    - Auto-seed Campaign 2, advance past 48h Forecasting
    //    - Live attack on FOB rejected with `SuppressionGoalNotReached` before 5 Stage-5 suppressed systems
    //    - Suppress 5 systems to Stage 5 via live Empire pulses -> weapon fire depletes shield+armor into `3h ± 1h` `ArmorReinforced`
    //    - Weapon attack during timer rejected with `FobArmorReinforced`; after timer elapses, hull kill destroys FOB,
    //      raises Suppression `+1` across campaign systems, and reduces Pirate Ambition modifier `1 -> 0`!
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    let camp2 = ctx.db.with_snapshot(|s| *s.insurgency_runtime.campaigns.keys().next().unwrap());
    let fob2 = PIRATE_FOB_STRUCTURE_BASE_ID + camp2;
    fast_forward_secs(&ctx, FORECASTING_DURATION_SECS + 10);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;

    let vis_before_supp = rpc_call(&ctx, &mut s_emp, "insurgencySolarsystem", "GetAllVisibleCampaigns", &[]).await.unwrap();
    let PyRep::List(vis_list0) = &vis_before_supp else { panic!("expected List") };
    assert_eq!(vis_list0[0].dict_get("isFobVulnerable"), Some(&PyRep::Bool(false)), "FOB is invulnerable before 5 suppressed systems");

    fit_siege_artillery(&ctx, emp_cid, emp_ship, 999_500);
    let _ = ctx.systems.get_or_spawn(front_sys).spawn_ball(make_ball(emp_ship, 19_722, fob_pos + DVec3::new(1_000.0, 0.0, 0.0))).await;
    lock_target_via_rpc(&ctx, &mut s_emp, front_sys, emp_ship, fob2).await;
    assert_eq!(
        rpc_call(&ctx, &mut s_emp, "dogmaIM", "Activate", &[
            PyRep::Integer(999_500),
            PyRep::String("targetAttack".into()),
            PyRep::Integer(fob2),
        ]).await.unwrap_err().code,
        "SuppressionGoalNotReached"
    );

    // Verify Suppression Stage 3 and Stage 5 do NOT spread insurgency, AND Stage 5 Suppression blocks Corruption Stage 3 spread!
    let _ = ctx.systems.get_or_spawn(front_sys).spawn_ball(make_ball(pir_ship, 621, front_beacon + DVec3::new(90_000.0, 0.0, 0.0))).await;
    let first_sys = ctx.db.with_snapshot(|s| s.insurgency_runtime.campaigns[&camp2].systems[0]);
    s_emp.solar_system_id = Some(first_sys);
    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&emp_cid).unwrap().solar_system_id = first_sys;
        s.items.get_mut(&emp_ship).unwrap().location_id = first_sys as i64;
    });
    let first_beacon = DVec3::from_array(ctx.db.with_snapshot(|s| s.system_states[&first_sys].fw_complex_position.expect("insurgency beacon spawned")));
    let sh0 = ctx.systems.get_or_spawn(first_sys);
    let _ = sh0.spawn_ball(make_ball(emp_ship, 19_722, first_beacon)).await;
    for _ in 0..5 {
        sh0.step_manual(60.0).await.unwrap();
        eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    }
    let _ = sh0.remove_ball(emp_ship).await;
    assert_eq!(ctx.db.with_snapshot(|s| s.insurgency_runtime.campaigns[&camp2].systems.len()), 4, "Suppression Stage 3/5 must never spread insurgency");

    // Pulse Corruption Stage 3 on Stage-5-suppressed `first_sys` -> blocked from spreading!
    s_pir.solar_system_id = Some(first_sys);
    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&pir_cid).unwrap().solar_system_id = first_sys;
        s.items.get_mut(&pir_ship).unwrap().location_id = first_sys as i64;
    });
    let _ = sh0.spawn_ball(make_ball(pir_ship, 621, first_beacon)).await;
    for _ in 0..3 {
        sh0.step_manual(60.0).await.unwrap();
        eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    }
    let _ = sh0.remove_ball(pir_ship).await;
    assert_eq!(ctx.db.with_snapshot(|s| s.insurgency_runtime.campaigns[&camp2].systems.len()), 4, "Stage 5 Suppression blocks Corruption Stage 3 spread");

    // Spread `camp2` to 5th and 6th systems via Corruption Stage 3 on unsuppressed `systems[1]` and `systems[2]`.
    for idx in 1..=2usize {
        let (csys, c_beacon) = ctx.db.with_snapshot(|s| (
            s.insurgency_runtime.campaigns[&camp2].systems[idx],
            s.system_states[&s.insurgency_runtime.campaigns[&camp2].systems[idx]].fw_complex_position.expect("insurgency beacon spawned"),
        ));
        s_pir.solar_system_id = Some(csys);
        ctx.db.with_mut_snapshot(|s| {
            s.characters.get_mut(&pir_cid).unwrap().solar_system_id = csys;
            s.items.get_mut(&pir_ship).unwrap().location_id = csys as i64;
        });
        let sh = ctx.systems.get_or_spawn(csys);
        let _ = sh.spawn_ball(make_ball(pir_ship, 621, DVec3::from_array(c_beacon))).await;
        for _ in 0..3 {
            sh.step_manual(60.0).await.unwrap();
            eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
        }
        let _ = sh.remove_ball(pir_ship).await;
    }
    assert_eq!(ctx.db.with_snapshot(|s| s.insurgency_runtime.campaigns[&camp2].systems.len()), 6);

    // Suppress `systems[1..5]` to Stage 5 (making 5 Stage-5-suppressed systems total) and pulse `systems[5]` 3 times to 600 pts!
    for idx in 1..6usize {
        let (tsys, t_beacon) = ctx.db.with_snapshot(|s| (
            s.insurgency_runtime.campaigns[&camp2].systems[idx],
            s.system_states[&s.insurgency_runtime.campaigns[&camp2].systems[idx]].fw_complex_position.expect("insurgency beacon spawned"),
        ));
        s_emp.solar_system_id = Some(tsys);
        ctx.db.with_mut_snapshot(|s| {
            s.characters.get_mut(&emp_cid).unwrap().solar_system_id = tsys;
            s.items.get_mut(&emp_ship).unwrap().location_id = tsys as i64;
        });
        let sh = ctx.systems.get_or_spawn(tsys);
        let _ = sh.spawn_ball(make_ball(emp_ship, 19_722, DVec3::from_array(t_beacon))).await;
        let pulses = if idx < 5 { 5 } else { 3 };
        for _ in 0..pulses {
            sh.step_manual(60.0).await.unwrap();
            eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
        }
        let _ = sh.remove_ball(emp_ship).await;
    }
    let sixth_sys = ctx.db.with_snapshot(|s| s.insurgency_runtime.campaigns[&camp2].systems[5]);
    assert_eq!(ctx.db.with_snapshot(|s| s.insurgency_runtime.campaigns[&camp2].suppression_points[&sixth_sys]), 600);

    let vis_after_supp = rpc_call(&ctx, &mut s_emp, "insurgencySolarsystem", "GetAllVisibleCampaigns", &[]).await.unwrap();
    let PyRep::List(vis_list1) = &vis_after_supp else { panic!("expected List") };
    assert_eq!(vis_list1[0].dict_get("isFobVulnerable"), Some(&PyRep::Bool(true)), "FOB becomes vulnerable once 5 systems reach Suppression Stage 5");

    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&emp_cid).unwrap().solar_system_id = front_sys;
        s.items.get_mut(&emp_ship).unwrap().location_id = front_sys as i64;
    });
    s_emp.solar_system_id = Some(front_sys);
    let cs_emp = ctx.build_ship_combat_state(emp_cid, emp_ship, 19_722);
    let _ = ctx.systems.get_or_spawn(front_sys).spawn_ball_with_combat(make_ball(emp_ship, 19_722, fob_pos + DVec3::new(1_000.0, 0.0, 0.0)), cs_emp).await;
    lock_target_via_rpc(&ctx, &mut s_emp, front_sys, emp_ship, fob2).await;

    let mut reinf_until = 0u64;
    for _ in 0..10 {
        for gid in [999_500i64, 999_502, 999_504] {
            let _ = rpc_call(&ctx, &mut s_emp, "dogmaIM", "Activate", &[
                PyRep::Integer(gid),
                PyRep::String("targetAttack".into()),
                PyRep::Integer(fob2),
            ]).await;
        }
        step_weapon_ticks(&ctx, front_sys).await;
        if ctx.db.with_snapshot(|s| s.structures[&fob2].state == "ArmorReinforced") {
            reinf_until = ctx.db.with_snapshot(|s| s.structures[&fob2].reinforced_until_unix);
            break;
        }
        fast_forward_secs(&ctx, 25);
    }
    let timer_len = reinf_until.saturating_sub(ctx.now_unix() - 25);
    assert!((7_200..=14_400).contains(&timer_len), "FOB armor reinforcement is 3h ± 1h (got {timer_len}s)");
    lock_target_via_rpc(&ctx, &mut s_emp, front_sys, emp_ship, fob2).await;
    assert_eq!(
        rpc_call(&ctx, &mut s_emp, "dogmaIM", "Activate", &[
            PyRep::Integer(999_500),
            PyRep::String("targetAttack".into()),
            PyRep::Integer(fob2),
        ]).await.unwrap_err().code,
        "FobArmorReinforced"
    );

    fast_forward_secs(&ctx, 14_500);
    lock_target_via_rpc(&ctx, &mut s_emp, front_sys, emp_ship, fob2).await;
    for _ in 0..10 {
        for gid in [999_500i64, 999_502, 999_504] {
            let _ = rpc_call(&ctx, &mut s_emp, "dogmaIM", "Activate", &[
                PyRep::Integer(gid),
                PyRep::String("targetAttack".into()),
                PyRep::Integer(fob2),
            ]).await;
        }
        step_weapon_ticks(&ctx, front_sys).await;
        if ctx.db.with_snapshot(|s| s.structures[&fob2].state == "Destroyed") {
            break;
        }
        fast_forward_secs(&ctx, 25);
    }
    assert_eq!(ctx.db.with_snapshot(|s| s.structures[&fob2].state.clone()), "Destroyed");
    assert_eq!(ctx.db.with_snapshot(|s| s.insurgency_runtime.campaigns[&camp2].suppression_points[&sixth_sys]), 800);
    assert_eq!(ctx.db.with_snapshot(|s| s.insurgency_runtime.pirate_ambition_modifier[&500_010]), 0, "Empire underdog win reduces Pirate Ambition 1 -> 0");

    // 4. Campaign 3 7d Expiry Tie (`0` Ambition bonus, `0` LP payout) + SQLite restart durability!
    fast_forward_secs(&ctx, WINDDOWN_DURATION_SECS + 60);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    let camp3 = ctx.db.with_snapshot(|s| *s.insurgency_runtime.campaigns.keys().next().unwrap());
    fast_forward_secs(&ctx, FORECASTING_DURATION_SECS + ACTIVE_DURATION_SECS + 60);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    assert_eq!(ctx.db.with_snapshot(|s| s.insurgency_runtime.campaign_outcomes.get(&camp3).cloned()), Some("Tie".into()));
    assert_eq!(ctx.db.with_snapshot(|s| s.insurgency_runtime.pirate_ambition_modifier[&500_010]), 0);
    assert_eq!(ctx.db.with_snapshot(|s| s.insurgency_runtime.empire_ambition_modifier.get(&500_010).copied().unwrap_or(0)), 0);

    drop(ctx);
    let ctx_restarted = open_sqlite_ctx(&db_path, false);
    assert_eq!(ctx_restarted.db.with_snapshot(|s| s.insurgency_runtime.campaign_outcomes.get(&camp3).cloned()), Some("Tie".into()));
    let _ = std::fs::remove_file(&db_path);
}

fn fast_forward_secs(ctx: &ServerContext, secs: u64) {
    ctx.clock().advance_secs(secs);
    let cur = ctx.now_unix_secs();
    ctx.db.with_mut_snapshot(|snap| {
        snap.service_state_blobs.insert("__server_now_unix".into(), cur.to_string());
    });
}

}

#[allow(dead_code, unused_imports)]
pub(crate) mod insurgency_frontlines_advantage {
    use super::*;

//  Part 5 of FW & Insurgency closure tests:
//  - Published per-stage Corruption (1–5) & Suppression (1–5) mechanics, `securityMgr.get_modified_systems`
//    (LAWLESS Stage 5 + manual overrides), documented insurgency sites, roaming Pirate/Enforcer NPCs,
//    Counter-Insurgency Sentry Guns (`79507`), non-retroactive Stage 5 corruption cap, and post-campaign persistence.
//  - Battlefield capture speed scaling by Advantage, combat-capable wave NPCs, Social skill standing scaling (`0` vs `5`),
//    time-on-grid ranked 30-pilot standing cap, and VP/Advantage shifts (`feat:fw.battlefield-capture-rules`).


use crate::fw_warzone_insurgency::setup_warzone_chain;
use crate::sovereignty_equinox_gameplay::{make_ball, register_online_pilot, rpc_call};
use eve_db::{InventoryFlag, InventoryItemRecord, TrainedSkillRecord};
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::combat_crimewatch::{step_sentry_gun_grid_aggression, with_combat_runtime};
use eve_server::services::combat_npc_police::{
    is_concord_suppressed_in_system, is_faction_police_suppressed, is_sentry_disabled_by_corruption,
};
use eve_server::services::fw_gameplay::{
    BATTLEFIELD_MAX_FULL_PAYOUT_PILOTS, BATTLEFIELD_MAX_PILOT_LP, BATTLEFIELD_ZONE_CENTERS,
};
use eve_server::services::insurgency_services::{
    add_system_corruption, add_system_suppression, FORECASTING_DURATION_SECS, ACTIVE_DURATION_SECS,
    WINDDOWN_DURATION_SECS,
};
use eve_server::services::security_mgr::{
    insurgency_pvp_loot_drop_threshold, insurgency_suppression_bounty_and_sec_multiplier,
    insurgency_suppression_empire_lp_multiplier, insurgency_suppression_web_scram_range_multiplier,
};
use eve_server::services::space_destiny::{compute_pilot_warp_speed_au_s, compute_ship_warp_speed_au_s};
use glam::DVec3;

fn lookup_modified_sec(rep: &PyRep, sys_id: u32) -> Option<f64> {
    let PyRep::Dict(entries) = rep else { panic!("expected Dict from securityMgr.get_modified_systems, got {rep:?}") };
    entries.iter().find_map(|(k, v)| match (k, v) {
        (PyRep::Integer(id), PyRep::Real(sec)) if *id == i64::from(sys_id) => Some(*sec),
        _ => None,
    })
}

fn active_sites_in_sys(ctx: &ServerContext, sys_id: u32) -> Vec<String> {
    ctx.db.with_snapshot(|s| {
        s.insurgency_runtime.insurgency_instances.iter()
            .filter(|i| i.solar_system_id == sys_id && !i.completed)
            .map(|i| i.site_type.clone())
            .collect()
    })
}

fn fast_forward_secs(ctx: &ServerContext, secs: u64) {
    ctx.clock().advance_secs(secs);
    let cur = ctx.now_unix_secs();
    ctx.db.with_mut_snapshot(|snap| { snap.service_state_blobs.insert("__server_now_unix".into(), cur.to_string()); });
}

#[tokio::test]
async fn test_corruption_and_suppression_per_stage_effects() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let (enemy_sys, front_sys, _cmd_sys, _rear_sys) = setup_warzone_chain(&ctx);

    let emp_cid = 90_000_051i64;
    let emp_ship = 100_000_051i64;
    let pir_cid = 90_000_052i64;
    let pir_ship = 100_000_052i64;

    let mut s_emp = register_online_pilot(
        &ctx, 85_001, "demo", "demo-secret", emp_cid, 98_000_051, None,
        front_sys, emp_ship, 639, DVec3::new(50_000.0, 0.0, 0.0), 0,
    ).await;
    let mut s_pir = register_online_pilot(
        &ctx, 85_002, "demo2", "demo2-secret", pir_cid, 98_000_052, None,
        front_sys, pir_ship, 621, DVec3::new(60_000.0, 0.0, 0.0), 0,
    ).await;

    rpc_call(&ctx, &mut s_emp, "fwCharacterEnlistmentMgr", "CreateMyDirectEnlistment", &[PyRep::Integer(500_001)])
        .await.expect("enlist Caldari");
    rpc_call(&ctx, &mut s_pir, "fwCharacterEnlistmentMgr", "CreateMyDirectEnlistment", &[PyRep::Integer(500_010)])
        .await.expect("enlist Guristas");

    // Seed campaign and advance past 48h Forecasting into 7d Active phase.
    let _ = rpc_call(&ctx, &mut s_emp, "dungeonInstanceCacheMgr", "GetFactionWarfareInstances", &[]).await.unwrap();
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    let (camp_id, fob_sys) = ctx.db.with_snapshot(|s| {
        let (&cid, c) = s.insurgency_runtime.campaigns.iter().next().unwrap();
        (cid, c.fob_solar_system_id)
    });
    fast_forward_secs(&ctx, FORECASTING_DURATION_SECS + 10);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;

    assert!(active_sites_in_sys(&ctx, front_sys).contains(&"CorporateOutpostRaid".into()));
    assert!(active_sites_in_sys(&ctx, fob_sys).contains(&"PirateLaunderingFacility".into()));
    assert!(active_sites_in_sys(&ctx, 30_045_324).contains(&"CounterInsurgencyOutpost".into()), "War HQ Onnamon has CounterInsurgencyOutpost");
    let mod0 = rpc_call(&ctx, &mut s_emp, "securityMgr", "get_modified_systems", &[]).await.unwrap();
    assert_eq!(lookup_modified_sec(&mod0, front_sys), None, "Stage 0 system has no security modification");

    // 1. Corruption Stages 1..=5: PvP loot drop bonus (+7.5%/stage), Faction Police suppression (2+),
    //    documented sites, roaming Pirate NPCs (2+), +2 AU/s pirate warp speed (4+), and Stage 5 LAWLESS.
    let base_pir_warp = compute_ship_warp_speed_au_s(&ctx, 621);
    let base_front_sec = ctx.sde.solar_systems[&front_sys].security_status;
    ctx.drain_notifications_for_char(emp_cid);
    for stage in 1u8..=5 {
        let (_pts, cur_st) = add_system_corruption(&ctx, camp_id, front_sys, 200);
        assert_eq!(cur_st, stage);
        eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;

        let mod_rep = rpc_call(&ctx, &mut s_emp, "securityMgr", "get_modified_systems", &[]).await.unwrap();
        assert_eq!(
            lookup_modified_sec(&mod_rep, front_sys),
            if stage >= 5 { Some(0.0) } else { None },
            "Only Corruption Stage 5 overrides system security to 0.0 LAWLESS"
        );

        let drop_p = insurgency_pvp_loot_drop_threshold(&ctx, front_sys, pir_cid, emp_cid, false);
        assert!((drop_p - (0.50 + 0.075 * f64::from(stage))).abs() < 1e-6);
        assert!((insurgency_pvp_loot_drop_threshold(&ctx, front_sys, emp_cid, pir_cid, false) - 0.50).abs() < 1e-6);

        assert_eq!(is_faction_police_suppressed(&ctx, front_sys), stage >= 2);
        assert_eq!(is_sentry_disabled_by_corruption(&ctx, front_sys), stage >= 5);
        assert_eq!(is_concord_suppressed_in_system(&ctx, front_sys), stage >= 5);

        let sites = active_sites_in_sys(&ctx, front_sys);
        assert!(sites.contains(&"CorporateOutpostRaid".into()));
        assert!(sites.contains(&"MiningOpAmbush".into()));
        assert_eq!(sites.contains(&"IceRefineryHeist".into()), (3..=4).contains(&stage));

        let pirate_roamer_tid = with_combat_runtime(&ctx, |rt| {
            rt.npc_entities.values().find_map(|n| {
                let is_pir = ctx.sde.types.get(&n.type_id).is_some_and(|t| t.group_id == 4637);
                (n.solar_system_id == front_sys && is_pir).then_some(n.type_id)
            })
        });
        assert_eq!(pirate_roamer_tid.is_some(), stage >= 2, "Roaming pirates spawn at Corruption 2+");

        let expected_warp = if stage >= 4 { base_pir_warp + 2.0 } else { base_pir_warp };
        assert!((compute_pilot_warp_speed_au_s(&ctx, pir_cid, front_sys, 621) - expected_warp).abs() < 1e-6);
        assert!((compute_pilot_warp_speed_au_s(&ctx, emp_cid, front_sys, 621) - base_pir_warp).abs() < 1e-6);
    }
    let sec_up = ctx.drain_notifications_for_char(emp_cid);
    assert!(sec_up.iter().any(|n| n.event == "OnSecurityModified" && n.args == vec![PyRep::Integer(i64::from(front_sys)), PyRep::Real(-base_front_sec), PyRep::Real(0.0)]));

    // Stage 5 lowsec sentry disabling verified on grid aggression:
    let sys_h = ctx.systems.get_or_spawn(front_sys);
    let _ = sys_h
        .spawn_ball_with_combat(make_ball(pir_ship, 621, DVec3::ZERO), ctx.build_type_combat_state(621))
        .await;
    assert_eq!(
        step_sentry_gun_grid_aggression(&ctx, front_sys, pir_cid, pir_ship, DVec3::ZERO, true).await,
        (false, 0.0),
        "Low-sec sentry guns cease firing at Corruption Stage 5"
    );
    let _ = sys_h.remove_ball(pir_ship).await;

    // 2. Dynamic pruning & restoration when Corruption drops:
    let (_pts_drop3, st_drop3) = add_system_corruption(&ctx, camp_id, front_sys, -500);
    assert_eq!(st_drop3, 3);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    assert!(active_sites_in_sys(&ctx, front_sys).contains(&"IceRefineryHeist".into()), "Stage 3 restores IceRefineryHeist");
    let mod_drop3 = rpc_call(&ctx, &mut s_emp, "securityMgr", "get_modified_systems", &[]).await.unwrap();
    assert_eq!(lookup_modified_sec(&mod_drop3, front_sys), None, "Dropping below Stage 5 restores normal security");
    let sec_down = ctx.drain_notifications_for_char(emp_cid);
    assert!(sec_down.iter().any(|n| n.event == "OnSecurityModified" && n.args == vec![PyRep::Integer(i64::from(front_sys)), PyRep::Real(0.0), PyRep::Real(base_front_sec)]));

    let (_pts_drop0, st_drop0) = add_system_corruption(&ctx, camp_id, front_sys, -500);
    assert_eq!(st_drop0, 0);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    let sites_drop0 = active_sites_in_sys(&ctx, front_sys);
    assert!(sites_drop0.contains(&"CorporateOutpostRaid".into()));
    assert!(!sites_drop0.contains(&"MiningOpAmbush".into()));
    assert!(!sites_drop0.contains(&"IceRefineryHeist".into()));

    // Measure baseline Stage-0 sentry damage for comparison with Counter-Insurgency Sentry Guns:
    let base_probe_ship = 100_000_058i64;
    let _ = sys_h
        .spawn_ball_with_combat(make_ball(base_probe_ship, 621, DVec3::ZERO), ctx.build_type_combat_state(621))
        .await;
    let (eng_s0, dmg_s0) = step_sentry_gun_grid_aggression(&ctx, front_sys, pir_cid, base_probe_ship, DVec3::ZERO, true).await;
    assert!(eng_s0 && dmg_s0 > 0.0, "Baseline Stage 0 sentry guns engage in low-sec");
    let _ = sys_h.remove_ball(base_probe_ship).await;

    // 3. Suppression Stages 1..=5: +5%/stage NPC bounty & sec gain, +5%/stage Empire LP (2+),
    //    -30% corruption gain & roaming Enforcers (3+), +10% web/scram range (4+), and Counter-Insurgency Sentries (5).
    for stage in 1u8..=5 {
        let (_pts, cur_st) = add_system_suppression(&ctx, camp_id, front_sys, 200);
        assert_eq!(cur_st, stage);
        eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;

        let mod_rep = rpc_call(&ctx, &mut s_emp, "securityMgr", "get_modified_systems", &[]).await.unwrap();
        assert_eq!(lookup_modified_sec(&mod_rep, front_sys), None, "Suppression does not alter securityMgr.get_modified_systems");

        assert!((insurgency_suppression_bounty_and_sec_multiplier(&ctx, front_sys) - (1.0 + 0.05 * f64::from(stage))).abs() < 1e-6);
        let expected_lp_mult = if stage >= 2 { 1.0 + 0.05 * f64::from(stage - 1) } else { 1.0 };
        assert!((insurgency_suppression_empire_lp_multiplier(&ctx, front_sys, 500_001) - expected_lp_mult).abs() < 1e-6);
        assert!((insurgency_suppression_empire_lp_multiplier(&ctx, front_sys, 500_010) - 1.0).abs() < 1e-6);

        let enforcer_exists = with_combat_runtime(&ctx, |rt| {
            rt.npc_entities.values().any(|n| {
                n.solar_system_id == front_sys && ctx.sde.types.get(&n.type_id).is_some_and(|t| t.group_id == 4638)
            })
        });
        assert_eq!(enforcer_exists, stage >= 3, "Roaming Enforcer NPCs spawn at Suppression 3+");

        let expected_range_mult = if stage >= 4 { 1.10 } else { 1.0 };
        assert!((insurgency_suppression_web_scram_range_multiplier(&ctx, emp_cid, front_sys) - expected_range_mult).abs() < 1e-6);
        assert!((insurgency_suppression_web_scram_range_multiplier(&ctx, pir_cid, front_sys) - 1.0).abs() < 1e-6);

        if stage == 3 {
            let (c_pts_reduced, _) = add_system_corruption(&ctx, camp_id, front_sys, 100);
            assert_eq!(c_pts_reduced, 70, "Suppression Stage 3 reduces corruption gain by 30%");
            let _ = add_system_corruption(&ctx, camp_id, front_sys, -100);
        }
    }

    // At Suppression Stage 5:
    // (a) Corruption below Stage 5 is capped at 800 pts (Stage 4), while deployed physical Counter-Insurgency
    //     Sentry Gun balls (`79507`) fire at higher SDE volley damage, target paint (+50% sig), and ECM jam!
    let (capped_c_pts, capped_c_stage) = add_system_corruption(&ctx, camp_id, front_sys, 2_000);
    assert_eq!((capped_c_pts, capped_c_stage), (800, 4), "Suppression Stage 5 caps corruption at Stage 4 (800 pts)");
    assert!(!is_sentry_disabled_by_corruption(&ctx, front_sys), "Stage 4 low-sec sentries remain online");
    let ci_probe_ship = 100_000_060i64;
    let mut base_cs = ctx.build_type_combat_state(621);
    base_cs.shield_hp = 50_000.0;
    base_cs.max_shield_hp = 50_000.0;
    let base_sig = base_cs.signature_radius;
    let _ = sys_h
        .spawn_ball_with_combat(make_ball(ci_probe_ship, 621, DVec3::new(10_000.0, 0.0, 0.0)), base_cs)
        .await;
    let roamer_ids: Vec<i64> = sys_h.list_balls().await.unwrap().into_iter().filter(|b| b.type_id != 79_507 && b.id != ci_probe_ship).map(|b| b.id).collect();
    {
        let mut bp = sys_h.ballpark.write().unwrap_or_else(|e| e.into_inner());
        for rid in roamer_ids {
            bp.set_ball_position(rid, DVec3::new(900_000.0, 0.0, 0.0));
        }
    }
    ctx.db.with_mut_snapshot(|s| s.characters.get_mut(&pir_cid).unwrap().active_ship_id = ci_probe_ship);
    eve_server::services::combat_crimewatch::combat_npc_ai::step_extended_npc_ai_sync(&ctx, front_sys);
    let painted_cs = sys_h.get_combat_state(ci_probe_ship).await.unwrap().unwrap();
    let dmg_ci = 50_000.0 - painted_cs.shield_hp;
    assert!(dmg_ci > dmg_s0, "Physical 79507 Counter-Insurgency Sentry fires higher SDE volley ({dmg_ci} > {dmg_s0})");
    assert!((painted_cs.signature_radius - base_sig * 1.50).abs() < 1e-3, "Counter-Insurgency Sentry Gun target paints (+50% sig)");
    assert!((painted_cs.ecm_jammed_remaining_s - 5.0).abs() < 1e-6, "Counter-Insurgency Sentry Gun applies 5.0s ECM jam within 75 km");
    ctx.db.with_mut_snapshot(|s| s.characters.get_mut(&pir_cid).unwrap().active_ship_id = pir_ship);

    // (b) Non-retroactive Stage 5 corruption cap: if `enemy_sys` is ALREADY at Corruption Stage 5 (`1,000` pts)
    //     before reaching Suppression Stage 5, it is NOT clamped down to 800, and regular gate/station sentries stay disabled!
    let (_e_cpts, e_cstage) = add_system_corruption(&ctx, camp_id, enemy_sys, 1_000);
    assert_eq!((_e_cpts, e_cstage), (1_000, 5));
    let (_e_spts, e_sstage) = add_system_suppression(&ctx, camp_id, enemy_sys, 1_000);
    assert_eq!((_e_spts, e_sstage), (1_000, 5));
    assert!(is_sentry_disabled_by_corruption(&ctx, enemy_sys), "Regular gate/station sentries stay disabled at Corruption 5 even at Suppression 5");
    let (e_after_pts, e_after_stage) = add_system_corruption(&ctx, camp_id, enemy_sys, 100);
    assert_eq!(e_after_stage, 5, "Suppression Stage 5 must not retroactively demote an existing Stage 5 system");
    assert!(e_after_pts >= 1_000);

    // (c) When the Insurgency campaign ends, roaming NPCs and sites are cleaned up while Counter-Insurgency
    //     Sentry Guns (`79507`) persist in space until destroyed!
    fast_forward_secs(&ctx, ACTIVE_DURATION_SECS + WINDDOWN_DURATION_SECS + 10);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    let (ci_sentry_persists, roamers_left) = with_combat_runtime(&ctx, |rt| {
        let ci = rt.npc_entities.values().any(|n| n.solar_system_id == front_sys && n.type_id == 79_507);
        let r = rt.npc_entities.values().any(|n| {
            n.solar_system_id == front_sys && ctx.sde.types.get(&n.type_id).is_some_and(|t| t.group_id == 4637 || t.group_id == 4638)
        });
        (ci, r)
    });
    assert!(ci_sentry_persists, "Counter-Insurgency Sentry Guns remain after insurgency ends until destroyed");
    assert!(!roamers_left, "Roaming Pirate/Enforcer NPCs despawn when insurgency ends");
}

#[tokio::test]
async fn test_battlefield_capture_social_standing_and_advantage_rules() {
    // parity: feat:fw.battlefield-capture-rules
    eve_testkit::attest(&["feat:fw.battlefield-capture-rules"]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let (_enemy_sys, front_sys, _cmd_sys, _rear_sys) = setup_warzone_chain(&ctx);

    let gal_cid = 90_000_061i64;
    let gal_ship = 100_000_061i64;
    let vet_cid = 99_999_999i64;
    let vet_ship = 109_999_999i64;
    let mut s_gal = register_online_pilot(
        &ctx, 86_001, "demo", "demo-secret", gal_cid, 98_000_061, None,
        front_sys, gal_ship, 621, DVec3::from_array(BATTLEFIELD_ZONE_CENTERS[0]), 0,
    ).await;
    rpc_call(&ctx, &mut s_gal, "fwCharacterEnlistmentMgr", "CreateMyDirectEnlistment", &[PyRep::Integer(500_004)])
        .await.expect("enlist Gallente");

    // Seed Battlefield instance in front_sys and spawn high-ID veteran (`99_999_999`) from tick 1.
    let _ = rpc_call(&ctx, &mut s_gal, "dungeonInstanceCacheMgr", "GetFactionWarfareBattlefieldInstances", &[])
        .await.unwrap();
    let bf_id = ctx.db.with_snapshot(|s| {
        s.fw_runtime
            .battlefield_instances
            .iter()
            .find(|b| b.solar_system_id == front_sys && !b.completed)
            .expect("frontline battlefield")
            .instance_id
    });
    ctx.db.with_mut_snapshot(|s| {
        let mut vch = s.characters[&gal_cid].clone();
        vch.character_id = vet_cid;
        vch.name = "Veteran High-ID Pilot".into();
        vch.active_ship_id = vet_ship;
        vch.war_faction_id = Some(500_004);
        vch.loyalty_points = Default::default();
        vch.standings = Default::default();
        s.characters.insert(vet_cid, vch);
        s.items.insert(
            vet_ship,
            InventoryItemRecord {
                item_id: vet_ship,
                type_id: 621,
                owner_id: vet_cid,
                location_id: front_sys as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });
    let sys_h = ctx.systems.get_or_spawn(front_sys);
    let _ = sys_h
        .spawn_ball(make_ball(vet_ship, 621, DVec3::from_array(BATTLEFIELD_ZONE_CENTERS[0]) + DVec3::new(300.0, 0.0, 0.0)))
        .await;

    // 1. Capture rate scales with warzone Advantage (`calculate_capture_speed_multiplier`) and spawns combat-capable wave NPCs:
    ctx.advance_secs(90);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    let (prog_neutral, wave_npcs) = ctx.db.with_snapshot(|s| {
        let bf = s.fw_runtime.battlefield_instances.iter().find(|b| b.instance_id == bf_id).unwrap();
        (
            bf.capture_progress,
            s.fw_runtime.battlefield_npc_waves.get(&bf_id).cloned().unwrap_or_default(),
        )
    });
    assert!((prog_neutral - 10.0).abs() < 1e-6, "1 zone held for 90s at 0% advantage advances 10.0% (got {prog_neutral})");
    assert_eq!(wave_npcs.len(), 6, "Battlefield spawns 6 combat-capable wave NPCs");
    let npc_cs = sys_h.get_combat_state(wave_npcs[0]).await.unwrap().expect("wave NPC CombatState");
    assert!(npc_cs.shield_hp > 0.0 && npc_cs.structure_hp > 0.0, "Wave NPCs are combat-capable");

    // Grant Gallente +50% Advantage (`speed_mult = 1.25`) -> 90s tick advances +12.5%!
    ctx.db.with_mut_snapshot(|s| {
        s.fw_runtime.system_advantage.insert(front_sys, (0.0, 50.0));
    });
    ctx.advance_secs(90);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    let prog_adv = ctx.db.with_snapshot(|s| {
        s.fw_runtime.battlefield_instances.iter().find(|b| b.instance_id == bf_id).unwrap().capture_progress
    });
    let adv_delta = prog_adv - prog_neutral;
    assert!((adv_delta - 12.5).abs() < 1e-6, "50% Advantage scales capture speed by 1.25x to 12.5%/90s (got {adv_delta})");

    // 2. Social skill standing scaling (`0` vs `5`), contribution-ranked 30-pilot standing cap (32 pilots on grid),
    //    LP scaling, & VP/Advantage shifts:
    // - `gal_cid` (`450s` on grid) has Social V (type_id 3355, level 5).
    // - `vet_cid` (`99_999_999`, `450s` on grid, highest character_id!) has Social 0.
    // - 30 latecomer wingmen (`90_200_000..90_200_030`, `270s` on grid) arrive on tick 3, including Social V on #28 & #29.
    //   Because standing recipients are ranked by cumulative time contributed on grid (`450s` > `270s`), `vet_cid`
    //   (`99_999_999`) MUST receive standing while the last 2 latecomers (`90_200_028` & `90_200_029`) are excluded!
    let mut all_cids = vec![gal_cid, vet_cid];
    ctx.db.with_mut_snapshot(|s| {
        s.skills.entry(gal_cid).or_default().insert(3355, TrainedSkillRecord {
            type_id: 3355,
            level: 5,
            skill_points: 256_000.0,
        });
        let base_ch = s.characters[&gal_cid].clone();
        for idx in 0..30i64 {
            let wcid = 90_200_000 + idx;
            let wship = 100_200_000 + idx;
            all_cids.push(wcid);
            let mut wch = base_ch.clone();
            wch.character_id = wcid;
            wch.name = format!("BF Latecomer {idx}");
            wch.solar_system_id = front_sys;
            wch.active_ship_id = wship;
            wch.war_faction_id = Some(500_004);
            wch.loyalty_points = Default::default();
            wch.standings = Default::default();
            s.characters.insert(wcid, wch);
            s.items.insert(
                wship,
                InventoryItemRecord {
                    item_id: wship,
                    type_id: 621,
                    owner_id: wcid,
                    location_id: front_sys as i64,
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
            if idx >= 28 {
                s.skills.entry(wcid).or_default().insert(3355, TrainedSkillRecord {
                    type_id: 3355,
                    level: 5,
                    skill_points: 256_000.0,
                });
            }
        }
    });
    for idx in 0..30i64 {
        let wship = 100_200_000 + idx;
        let zc = DVec3::from_array(BATTLEFIELD_ZONE_CENTERS[(idx as usize) % 3]);
        let _ = sys_h
            .spawn_ball(make_ball(wship, 621, zc + DVec3::new(400.0, 0.0, 0.0)))
            .await;
    }

    // Holding all 3 zones at 1.25x speed advances 37.5% per 90s; 3 ticks complete the remaining 77.5%!
    for _ in 0..3 {
        ctx.advance_secs(90);
        eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    }

    let expected_lp = (BATTLEFIELD_MAX_PILOT_LP * (BATTLEFIELD_MAX_FULL_PAYOUT_PILOTS as i64)) / 32;
    let (gal_fac_st, gal_corp_st, vet_fac_st, vet_corp_st, w28_fac_st, w29_fac_st, st_recipients, vp_after, adv_after) =
        ctx.db.with_snapshot(|s| {
            let gch = &s.characters[&gal_cid];
            let vch = &s.characters[&vet_cid];
            let w28 = &s.characters[&90_200_028];
            let w29 = &s.characters[&90_200_029];
            let recipients = all_cids
                .iter()
                .filter(|&&cid| s.characters[&cid].standings.get(&500_004).copied().unwrap_or(0.0) > 0.0)
                .count();
            for &cid in &all_cids {
                let lp = s.characters[&cid].loyalty_points.get(&1_000_181).copied().unwrap_or(0);
                assert_eq!(lp, expected_lp, "Every pilot in 32-pilot fleet receives scaled LP {expected_lp}");
            }
            (
                gch.standings.get(&500_004).copied().unwrap_or(0.0),
                gch.standings.get(&1_000_181).copied().unwrap_or(0.0),
                vch.standings.get(&500_004).copied().unwrap_or(0.0),
                vch.standings.get(&1_000_181).copied().unwrap_or(0.0),
                w28.standings.get(&500_004).copied().unwrap_or(0.0),
                w29.standings.get(&500_004).copied().unwrap_or(0.0),
                recipients,
                s.fw_system_occupations[&front_sys].victory_points,
                s.fw_runtime.system_advantage[&front_sys],
            )
        });

    assert_eq!(st_recipients, 30, "Standing payout is strictly capped at 30 winning pilots");
    assert!((gal_fac_st - 0.01875).abs() < 1e-6, "Social 5 veteran receives +25% scaled faction standing (got {gal_fac_st})");
    assert!((gal_corp_st - 0.01875).abs() < 1e-6, "Social 5 veteran receives +25% scaled corp standing (got {gal_corp_st})");
    assert!((vet_fac_st - 0.015).abs() < 1e-6, "High-ID Social 0 veteran (450s on grid) receives base standing (got {vet_fac_st})");
    assert!((vet_corp_st - 0.015).abs() < 1e-6, "High-ID Social 0 veteran (450s on grid) receives base corp standing (got {vet_corp_st})");
    assert_eq!(w28_fac_st, 0.0, "31st pilot by contribution receives no standing gain beyond 30-pilot cap");
    assert_eq!(w29_fac_st, 0.0, "32nd pilot by contribution receives no standing gain beyond 30-pilot cap");
    assert_eq!(vp_after, 2_000, "Attacker Battlefield victory shifts contested VP by +2,000");
    assert_eq!(adv_after, (0.0, 56.0), "Attacker Battlefield victory shifts system Advantage by +6.0%");
}

#[tokio::test]
async fn test_insurgency_live_gameplay_and_highsec_stage5() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let (_enemy_sys, front_sys, _cmd_sys, _rear_sys) = setup_warzone_chain(&ctx);

    let emp_cid = 90_000_071i64;
    let emp_ship = 100_000_071i64;
    let pir_cid = 90_000_072i64;
    let pir_ship = 100_000_072i64;
    let vic_cid = 90_000_073i64;
    let vic_ship = 100_000_073i64;
    let hs_sys = 30_045_324u32; // Onnamon (0.5555 Highsec)
    assert!(ctx.sde.solar_systems.get(&hs_sys).unwrap().security_status >= 0.45);
    let helper_cid = 90_000_077i64;
    let helper_ship = 100_000_077i64;
    let crim_cid = 90_000_078i64;
    let crim_ship = 100_000_078i64;

    let mut s_emp = register_online_pilot(
        &ctx, 87_001, "demo", "demo-secret", emp_cid, 98_000_071, None,
        front_sys, emp_ship, 639, DVec3::ZERO, 0,
    ).await;
    let mut s_pir = register_online_pilot(
        &ctx, 87_002, "demo2", "demo2-secret", pir_cid, 98_000_072, None,
        front_sys, pir_ship, 621, DVec3::new(1_000.0, 0.0, 0.0), 0,
    ).await;
    let mut _s_helper = register_online_pilot(
        &ctx, 87_003, "demo3", "demo3-secret", helper_cid, 98_000_077, None,
        hs_sys, helper_ship, 621, DVec3::ZERO, 0,
    ).await;
    ctx.db.with_mut_snapshot(|s| {
        let base_ch = s.characters[&emp_cid].clone();
        for (cid, sid, sys, name) in [
            (vic_cid, vic_ship, front_sys, "Insurgency Victim"),
            (crim_cid, crim_ship, hs_sys, "Highsec Criminal"),
        ] {
            let mut ch = base_ch.clone();
            ch.character_id = cid;
            ch.name = name.into();
            ch.solar_system_id = sys;
            ch.active_ship_id = sid;
            ch.war_faction_id = None;
            s.characters.insert(cid, ch);
            s.items.insert(sid, InventoryItemRecord {
                item_id: sid, type_id: 621, owner_id: cid, location_id: sys as i64,
                flag: InventoryFlag::None, quantity: 1, singleton: true, ..Default::default()
            });
        }
    });
    let sys_h = ctx.systems.get_or_spawn(front_sys);
    let _ = sys_h.spawn_ball(make_ball(vic_ship, 621, DVec3::new(8_000.0, 0.0, 0.0))).await;

    rpc_call(&ctx, &mut s_emp, "fwCharacterEnlistmentMgr", "CreateMyDirectEnlistment", &[PyRep::Integer(500_001)])
        .await.expect("enlist Caldari");
    rpc_call(&ctx, &mut s_pir, "fwCharacterEnlistmentMgr", "CreateMyDirectEnlistment", &[PyRep::Integer(500_010)])
        .await.expect("enlist Guristas");

    // Seed campaign and place it into Active phase covering `front_sys` and `hs_sys`.
    let _ = rpc_call(&ctx, &mut s_emp, "dungeonInstanceCacheMgr", "GetFactionWarfareInstances", &[]).await.unwrap();
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    let now_u = ctx.now_unix();
    let camp_id = ctx.db.with_mut_snapshot(|s| {
        let (&cid, c) = s.insurgency_runtime.campaigns.iter_mut().next().unwrap();
        c.forecasting_end_unix = now_u.saturating_sub(1);
        if !c.systems.contains(&hs_sys) {
            c.systems.push(hs_sys);
        }
        cid
    });

    // 1. Live PvP loot drop via `execute_death_pipeline_sync`:
    //    For `iid = 77_000_001` and `rng_seed = (iid as u64) ^ 2`, deterministic roll is `~0.690` (`0.50 < 0.690 < 0.875`).
    //    At Corruption Stage 0, `iid` is destroyed; at Corruption Stage 5 (`0.875`), `iid` drops into the wreck!
    let loot_iid = 77_000_001i64;
    let rng_seed = (loot_iid as u64) ^ 2;
    let stage0_ship = 100_000_074i64;
    let stage5_ship = 100_000_075i64;
    ctx.db.with_mut_snapshot(|s| {
        for (sid, iid) in [(stage0_ship, loot_iid), (stage5_ship, loot_iid)] {
            s.items.insert(sid, InventoryItemRecord {
                item_id: sid, type_id: 621, owner_id: vic_cid, location_id: front_sys as i64,
                flag: InventoryFlag::None, quantity: 1, singleton: true, ..Default::default()
            });
            if sid == stage0_ship {
                s.items.insert(iid, InventoryItemRecord {
                    item_id: iid, type_id: 34, owner_id: vic_cid, location_id: sid,
                    flag: InventoryFlag::Cargo, quantity: 10, singleton: false, ..Default::default()
                });
            }
        }
    });
    let res_s0 = eve_server::combat::death_pipeline::execute_death_pipeline_sync(
        &ctx, front_sys, vic_cid, stage0_ship, pir_cid, DVec3::ZERO, rng_seed,
    ).expect("stage0 death pipeline");
    assert!(res_s0.loot_dropped_item_ids.is_empty(), "Roll 0.690 > 0.50 destroys cargo at Corruption Stage 0");

    let (_cpts, cstage) = add_system_corruption(&ctx, camp_id, front_sys, 1_000);
    assert_eq!(cstage, 5);
    ctx.db.with_mut_snapshot(|s| {
        s.items.insert(loot_iid, InventoryItemRecord {
            item_id: loot_iid, type_id: 34, owner_id: vic_cid, location_id: stage5_ship,
            flag: InventoryFlag::Cargo, quantity: 10, singleton: false, ..Default::default()
        });
    });
    let res_s5 = eve_server::combat::death_pipeline::execute_death_pipeline_sync(
        &ctx, front_sys, vic_cid, stage5_ship, pir_cid, DVec3::ZERO, rng_seed,
    ).expect("stage5 death pipeline");
    assert_eq!(res_s5.loot_dropped_item_ids, vec![loot_iid], "Roll 0.690 < 0.875 drops cargo at Corruption Stage 5");

    // 2. Live `+2.0 AU/s` Pirate warp speed via `beyonce.CmdWarpToStuff` at Corruption Stage 4+:
    let base_pir_warp = compute_ship_warp_speed_au_s(&ctx, 621);
    let warp_dest_id = ctx.sde.celestials_in_system(front_sys).into_iter()
        .find(|c| c.position.distance(DVec3::new(1_000.0, 0.0, 0.0)) >= 200_000.0)
        .expect("celestial >= 200 km from pirate ship").celestial_id;
    let _ = rpc_call(&ctx, &mut s_pir, "beyonce", "CmdWarpToStuff", &[PyRep::String("item".into()), PyRep::Integer(warp_dest_id), PyRep::Real(0.0)])
        .await.expect("CmdWarpToStuff in Stage 5 system");
    let (_, pir_ball_after) = sys_h.get_ball_snapshot(pir_ship).await.unwrap().unwrap();
    match pir_ball_after.mode {
        eve_destiny::MovementMode::Warp { warp_speed_au_s, .. } => {
            assert!((warp_speed_au_s - (base_pir_warp + 2.0)).abs() < 1e-6, "Pirate warp speed +2.0 AU/s (got {warp_speed_au_s})");
        }
        other => panic!("expected MovementMode::Warp, got {other:?}"),
    }

    // 3. Live Highsec Stage 5 Criminal Assist (`Corruption_and_Suppression.md:55`):
    //    Direct ship attack in Highsec Stage 5 suppresses CONCORD, whereas assisting a Criminal triggers CONCORD
    //    without granting a Limited Engagement or Kill Right to the assisted criminal!
    let (_hs_pts, hs_stage) = add_system_corruption(&ctx, camp_id, hs_sys, 1_000);
    assert_eq!(hs_stage, 5);
    assert!(is_concord_suppressed_in_system(&ctx, hs_sys), "Highsec Stage 5 suppresses CONCORD for direct ship attacks");
    {
        let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
        cw.get_or_create(crim_cid).criminal_flag_until_unix = now_u + 900;
    }
    ctx.drain_notifications_for_char(helper_cid);
    eve_server::services::combat_crimewatch::evaluate_and_record_remote_assistance(
        &ctx, helper_cid, helper_ship, hs_sys, crim_cid,
    ).expect("remote assist evaluation");
    let (helper_is_crim, concord_scheduled) = {
        let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
        let st = cw.get_or_create(helper_cid);
        (st.is_criminal(now_u), st.pending_concord_arrival.is_some())
    };
    assert!(helper_is_crim && concord_scheduled, "Criminal assist grants Criminal flag + CONCORD");
    let assist_notes = ctx.drain_notifications_for_char(helper_cid);
    assert!(
        assist_notes.iter().any(|n| n.event == "OnSystemCriminalFlagUpdates"
            && n.args == vec![PyRep::List(vec![]), PyRep::List(vec![]), PyRep::List(vec![PyRep::Integer(helper_cid)])]),
        "OnSystemCriminalFlagUpdates must emit helper_cid in new_criminals (3rd slice)"
    );
    assert!(!with_combat_runtime(&ctx, |rt| rt.has_limited_engagement_with(helper_cid, crim_cid, now_u) || rt.has_limited_engagement_with(crim_cid, helper_cid, now_u)));
    fast_forward_secs(&ctx, 25);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    assert!(ctx.db.with_snapshot(|s| !s.kill_rights.values().any(|kr| kr.owner_id == crim_cid && kr.target_id == helper_cid)));

    // 4. Live Suppression Stage 4 `+10%` Warp Scrambler optimal range (`7500m -> 8250m`) via `dogmaIM.Activate`:
    let scram_id = 60_000_071i64;
    ctx.db.with_mut_snapshot(|s| {
        s.items.insert(scram_id, InventoryItemRecord {
            item_id: scram_id, type_id: 447, owner_id: emp_cid, location_id: emp_ship,
            flag: InventoryFlag::MedSlot0, quantity: 1, singleton: true, ..Default::default()
        });
    });
    ctx.invalidate_ship_fitting(emp_ship);
    let _ = sys_h.add_target_lock(emp_ship, eve_destiny::TargetLock {
        target_id: vic_ship, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(50_000.0),
    }).await;
    let (_s3_pts, s3_stage) = add_system_suppression(&ctx, camp_id, front_sys, 600);
    assert_eq!(s3_stage, 3);
    let err_s3 = rpc_call(
        &ctx, &mut s_emp, "dogmaIM", "Activate",
        &[PyRep::Integer(scram_id), PyRep::String("warpScrambler".into()), PyRep::Integer(vic_ship)],
    ).await.unwrap_err();
    assert_eq!(err_s3.code, "TargetOutOfRange", "8,000 m exceeds 7,500 m Warp Scrambler range at Suppression Stage 3");

    let (_s4_pts, s4_stage) = add_system_suppression(&ctx, camp_id, front_sys, 200);
    assert_eq!(s4_stage, 4);
    let ok_s4 = rpc_call(
        &ctx, &mut s_emp, "dogmaIM", "Activate",
        &[PyRep::Integer(scram_id), PyRep::String("warpScrambler".into()), PyRep::Integer(vic_ship)],
    ).await;
    assert!(ok_s4.is_ok(), "8,000 m succeeds within +10% (8,250 m) Warp Scrambler range at Suppression Stage 4: {ok_s4:?}");
    for _ in 0..3 {
        let _ = sys_h.step_manual(1.0).await.unwrap();
    }
    assert!(sys_h.get_combat_state(vic_ship).await.unwrap().unwrap().warp_scramble_strength > 0, "Scram stays active across 1 Hz ticks at 8,000 m");

    // 5. Live Suppression Stage 5 Empire FW LP (`1.20x`) & NPC Bounty/Security Status (`1.25x`):
    let (_s5_pts, s5_stage) = add_system_suppression(&ctx, camp_id, front_sys, 200);
    assert_eq!(s5_stage, 5);
    let emp_kill_lp = eve_server::services::fw_services::award_fw_kill_lp(&ctx, emp_cid, front_sys, 100_000_000.0);
    let pir_kill_lp = eve_server::services::fw_services::award_fw_kill_lp(&ctx, pir_cid, front_sys, 100_000_000.0);
    assert_eq!(emp_kill_lp, ((pir_kill_lp as f64) * 1.20).round() as i64, "Empire militia receives +20% FW LP at Suppression Stage 5");

    let (bounty_npc_tid, base_npc_bounty) = ctx
        .sde
        .types
        .values()
        .find_map(|t| {
            t.default_attributes
                .get(&eve_sde::attr_ids::ENTITY_KILL_BOUNTY)
                .copied()
                .filter(|&b| b > 0.0)
                .map(|b| (t.type_id, b))
        })
        .expect("SDE NPC type with ENTITY_KILL_BOUNTY > 0");
    let npc_ball_id = 88_000_071i64;
    let npc_ball = make_ball(npc_ball_id, bounty_npc_tid, DVec3::new(2_000.0, 0.0, 0.0));
    let _ = sys_h.spawn_ball_with_combat(npc_ball.clone(), ctx.build_type_combat_state(bounty_npc_tid)).await;
    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&emp_cid).unwrap().security_status = 0.0;
    });
    let _ = eve_server::services::combat_crimewatch::combat_weapons_spool::handle_ship_or_npc_destruction(
        &ctx, &sys_h, emp_cid, emp_ship, 639, 0, npc_ball_id, &npc_ball, None, front_sys, 100.0, ctx.now_unix(),
    ).await;
    let pending_bounty = with_combat_runtime(&ctx, |rt| rt.pending_bounty_ticks.get(&(front_sys, emp_cid)).copied().unwrap_or(0.0));
    let new_sec = ctx.db.with_snapshot(|s| s.characters[&emp_cid].security_status);
    let expected_sec = eve_server::combat::npc_ai::compute_sec_status_gain(0.0, base_npc_bounty) * 1.25;
    assert!((pending_bounty - base_npc_bounty * 1.25).abs() < 1e-3, "Suppression Stage 5 scales NPC bounty by +25% (got {pending_bounty})");
    assert!((new_sec - expected_sec).abs() < 1e-6, "Suppression Stage 5 scales NPC security status gain by +25% (got {new_sec})");
}

}

#[allow(dead_code, unused_imports)]
pub(crate) mod insurgency_lp_rewards {
    use super::*;

//  Part 6 of FW & Insurgency closure tests:
//  - Roaming Pirate (`group 4637`) & Enforcer (`group 4638`) dynamic SDE selection by stage/faction,
//    celestial waypoint warp patrol movement, campaign-faction targeting rules, and despawn on stage drop.
//  - `OnSecurityModified` broadcast on campaign end from Stage 5 LAWLESS, manual override precedence,
//    Suppression Stage 4+ `+10%` Stasis Webifier optimal range across 1 Hz ticks, `IncreasedNotice` accuracy,
//    and Highsec Stage 5 Green/Yellow/Red safety & Faction Police suppression.


use crate::fw_warzone_insurgency::setup_warzone_chain;
use crate::quasar_sov_equinox_hub::{field, varint};
use crate::sovereignty_equinox_gameplay::{make_ball, register_online_pilot, rpc_call};
use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_destiny::MovementMode;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::combat_crimewatch::combat_npc_ai::step_extended_npc_ai_sync;
use eve_server::services::combat_crimewatch::{with_combat_runtime, with_mut_combat_runtime};
use eve_server::services::insurgency_services::{
    add_system_corruption, add_system_suppression, end_insurgency_campaign,
    FORECASTING_DURATION_SECS,
};
use eve_server::services::security_mgr::{
    insurgency_pirate_warp_bonus_au_s, insurgency_pvp_loot_drop_threshold,
};
use glam::DVec3;

fn total_sde_hp(ctx: &ServerContext, type_id: u32) -> f64 {
    ctx.sde.types.get(&type_id).map_or(0.0, |t| {
        t.default_attributes.get(&263).copied().unwrap_or(0.0)
            + t.default_attributes.get(&265).copied().unwrap_or(0.0)
            + t.default_attributes.get(&9).copied().unwrap_or(0.0)
    })
}

fn fast_forward_secs(ctx: &ServerContext, secs: u64) {
    ctx.clock().advance_secs(secs);
    let cur = ctx.now_unix_secs();
    ctx.db.with_mut_snapshot(|snap| {
        snap.service_state_blobs
            .insert("__server_now_unix".into(), cur.to_string());
    });
}

#[tokio::test]
async fn test_insurgency_roamers_dynamic_sde_movement_targeting_and_despawn() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let (_enemy_sys, front_sys, _cmd_sys, _rear_sys) = setup_warzone_chain(&ctx);

    let emp_cid = 90_000_081i64;
    let emp_ship = 100_000_081i64;
    let pir_cid = 90_000_082i64;
    let pir_ship = 100_000_082i64;
    let neu_cid = 90_000_083i64;
    let neu_ship = 100_000_083i64;

    // Place ships well away from initial celestial patrol anchors until targeting checks
    let mut s_emp = register_online_pilot(
        &ctx, 88_001, "demo", "demo-secret", emp_cid, 98_000_081, None, front_sys, emp_ship, 639, DVec3::new(900_000.0, 900_000.0, 0.0), 0,
    ).await;
    let mut s_pir = register_online_pilot(
        &ctx, 88_002, "demo2", "demo2-secret", pir_cid, 98_000_082, None, front_sys, pir_ship, 621, DVec3::new(910_000.0, 900_000.0, 0.0), 0,
    ).await;
    ctx.db.with_mut_snapshot(|s| {
        let mut neu_ch = s.characters[&emp_cid].clone();
        neu_ch.character_id = neu_cid;
        neu_ch.name = "Neutral Explorer".into();
        neu_ch.solar_system_id = front_sys;
        neu_ch.active_ship_id = neu_ship;
        neu_ch.war_faction_id = None;
        s.characters.insert(neu_cid, neu_ch);
        s.items.insert(neu_ship, InventoryItemRecord {
            item_id: neu_ship, type_id: 621, owner_id: neu_cid, location_id: i64::from(front_sys),
            flag: InventoryFlag::None, quantity: 1, singleton: true, ..Default::default()
        });
    });
    let sys_h = ctx.systems.get_or_spawn(front_sys);
    let _ = sys_h.spawn_ball(make_ball(neu_ship, 621, DVec3::new(920_000.0, 900_000.0, 0.0))).await;

    rpc_call(&ctx, &mut s_emp, "fwCharacterEnlistmentMgr", "CreateMyDirectEnlistment", &[PyRep::Integer(500_001)])
        .await.expect("enlist Caldari");
    rpc_call(&ctx, &mut s_pir, "fwCharacterEnlistmentMgr", "CreateMyDirectEnlistment", &[PyRep::Integer(500_010)])
        .await.expect("enlist Guristas");

    // Seed campaign and enter Active phase
    let _ = rpc_call(&ctx, &mut s_emp, "dungeonInstanceCacheMgr", "GetFactionWarfareInstances", &[]).await.unwrap();
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    let (camp_id, pirate_fac) = ctx.db.with_snapshot(|s| {
        let (&cid, c) = s.insurgency_runtime.campaigns.iter().next().unwrap();
        (cid, c.pirate_faction_id)
    });
    fast_forward_secs(&ctx, FORECASTING_DURATION_SECS + 10);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;

    // 1. Dynamic SDE tier escalation for Roaming Pirates (group 4637) across Corruption 2..=5:
    let mut pir_tids_by_stage = Vec::new();
    for stage in [2u8, 5] {
        let delta = if stage == 2 { 400 } else { 600 };
        let (_pts, cur_st) = add_system_corruption(&ctx, camp_id, front_sys, delta);
        assert_eq!(cur_st, stage);
        eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
        let (_bid, tid) = with_combat_runtime(&ctx, |rt| {
            rt.npc_entities
                .values()
                .find_map(|n| {
                    let t = ctx.sde.types.get(&n.type_id)?;
                    (n.solar_system_id == front_sys
                        && t.group_id == 4637
                        && t.faction_id == Some(pirate_fac as u32))
                    .then_some((n.ball_id, n.type_id))
                })
                .expect("SDE group 4637 Pirate roamer for campaign faction")
        });
        pir_tids_by_stage.push(tid);
    }
    assert!(
        total_sde_hp(&ctx, pir_tids_by_stage[1]) > total_sde_hp(&ctx, pir_tids_by_stage[0]),
        "Stage 5 Pirate roamer must escalate to a higher-HP SDE hull than Stage 2"
    );

    // 2. Waypoint patrol movement across system celestials via Warp (no instant teleport):
    let pir_bid = with_combat_runtime(&ctx, |rt| {
        rt.npc_entities
            .values()
            .find(|n| {
                n.solar_system_id == front_sys
                    && ctx.sde.types.get(&n.type_id).is_some_and(|t| t.group_id == 4637)
            })
            .unwrap()
            .ball_id
    });
    let pos_before = sys_h.get_ball_snapshot(pir_bid).await.unwrap().unwrap().1.position;
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    let (_, ball_warping) = sys_h.get_ball_snapshot(pir_bid).await.unwrap().unwrap();
    assert!(
        matches!(ball_warping.mode, MovementMode::Warp { .. }),
        "Idle Pirate roamer must enter MovementMode::Warp toward next celestial waypoint, got {:?}",
        ball_warping.mode
    );
    assert!(
        ball_warping.position.distance(pos_before) < 5_000.0,
        "Roamer must not teleport instantaneously across celestials on warp initiation (moved {} m)",
        ball_warping.position.distance(pos_before)
    );
    for _ in 0..4 {
        let _ = sys_h.step_manual(10.0).await.unwrap();
    }
    let (_, ball_after_roam) = sys_h.get_ball_snapshot(pir_bid).await.unwrap().unwrap();
    assert!(
        ball_after_roam.position.distance(pos_before) > 100_000.0,
        "Idle Pirate roamer must arrive at next celestial waypoint after stepping warp ticks"
    );

    // 3. Stage & campaign-faction-gated targeting for InsurgencyPirateRoamer (Corruption >= 2):
    //    Attacks neutral, Empire militia, and rival pirate militia pilots while ignoring aligned Pirate militia!
    let rival_pir_cid = 90_000_084i64;
    let rival_pir_ship = 100_000_084i64;
    let rival_fac = if pirate_fac == 500_010 { 500_011 } else { 500_010 };
    ctx.db.with_mut_snapshot(|s| {
        let mut r_ch = s.characters[&pir_cid].clone();
        r_ch.character_id = rival_pir_cid;
        r_ch.name = "Rival Pirate Pilot".into();
        r_ch.active_ship_id = rival_pir_ship;
        r_ch.war_faction_id = Some(rival_fac);
        s.characters.insert(rival_pir_cid, r_ch);
        s.items.insert(rival_pir_ship, InventoryItemRecord {
            item_id: rival_pir_ship, type_id: 621, owner_id: rival_pir_cid, location_id: i64::from(front_sys),
            flag: InventoryFlag::None, quantity: 1, singleton: true, ..Default::default()
        });
    });
    let mut rival_cs = ctx.build_type_combat_state(621);
    rival_cs.shield_hp = 500_000.0;
    rival_cs.max_shield_hp = 500_000.0;
    let _ = sys_h.spawn_ball_with_combat(make_ball(rival_pir_ship, 621, DVec3::new(930_000.0, 900_000.0, 0.0)), rival_cs).await;
    assert_eq!(insurgency_pirate_warp_bonus_au_s(&ctx, rival_pir_cid, front_sys), 0.0, "Rival pirate receives no +2.0 AU/s warp bonus");
    assert_eq!(insurgency_pirate_warp_bonus_au_s(&ctx, pir_cid, front_sys), 2.0, "Aligned pirate receives +2.0 AU/s warp bonus");
    assert_eq!(insurgency_pvp_loot_drop_threshold(&ctx, front_sys, rival_pir_cid, emp_cid, false), 0.50, "Rival pirate receives no PvP loot bonus");
    assert!((insurgency_pvp_loot_drop_threshold(&ctx, front_sys, pir_cid, emp_cid, false) - 0.875).abs() < 1e-6);

    let roamer_pos = ball_after_roam.position;
    {
        let mut bp = sys_h.ballpark.write().unwrap_or_else(|e| e.into_inner());
        bp.set_ball_position(pir_ship, roamer_pos + DVec3::new(10_000.0, 0.0, 0.0));
        bp.set_ball_position(rival_pir_ship, roamer_pos + DVec3::new(13_000.0, 0.0, 0.0));
        bp.set_ball_position(neu_ship, roamer_pos + DVec3::new(900_000.0, 0.0, 0.0));
        bp.set_ball_position(emp_ship, roamer_pos + DVec3::new(900_000.0, 0.0, 0.0));
    }
    step_extended_npc_ai_sync(&ctx, front_sys);
    with_combat_runtime(&ctx, |rt| {
        let n = &rt.npc_entities[&pir_bid];
        assert!(!n.aggression_threat.contains_key(&pir_ship), "Roamer must not attack aligned Pirate militia unprovoked");
        assert_eq!(n.target_id, Some(rival_pir_ship), "Roamer must engage rival non-campaign Pirate militia pilot");
    });
    {
        let mut bp = sys_h.ballpark.write().unwrap_or_else(|e| e.into_inner());
        bp.set_ball_position(rival_pir_ship, roamer_pos + DVec3::new(900_000.0, 0.0, 0.0));
        bp.set_ball_position(neu_ship, roamer_pos + DVec3::new(15_000.0, 0.0, 0.0));
    }
    step_extended_npc_ai_sync(&ctx, front_sys);
    with_combat_runtime(&ctx, |rt| {
        assert_eq!(rt.npc_entities[&pir_bid].target_id, Some(neu_ship), "Roamer must engage neutral pilot");
    });
    {
        let mut bp = sys_h.ballpark.write().unwrap_or_else(|e| e.into_inner());
        bp.set_ball_position(neu_ship, roamer_pos + DVec3::new(900_000.0, 0.0, 0.0));
        bp.set_ball_position(emp_ship, roamer_pos + DVec3::new(15_000.0, 0.0, 0.0));
    }
    step_extended_npc_ai_sync(&ctx, front_sys);
    with_combat_runtime(&ctx, |rt| {
        assert_eq!(rt.npc_entities[&pir_bid].target_id, Some(emp_ship), "Roamer must engage Empire militia pilot");
    });
    // Move all pilots far away from Pirate roamer before testing Enforcers
    {
        let mut bp = sys_h.ballpark.write().unwrap_or_else(|e| e.into_inner());
        bp.set_ball_position(emp_ship, roamer_pos + DVec3::new(900_000.0, 900_000.0, 0.0));
        bp.set_ball_position(pir_ship, roamer_pos + DVec3::new(910_000.0, 900_000.0, 0.0));
        bp.set_ball_position(neu_ship, roamer_pos + DVec3::new(920_000.0, 900_000.0, 0.0));
        bp.set_ball_position(rival_pir_ship, roamer_pos + DVec3::new(930_000.0, 900_000.0, 0.0));
    }

    // 4. Dynamic SDE tier escalation & targeting for Roaming Enforcers (group 4638) across Suppression 3..=5:
    let mut enf_tids_by_stage = Vec::new();
    for stage in [3u8, 5] {
        let delta = if stage == 3 { 600 } else { 400 };
        let (_pts, cur_st) = add_system_suppression(&ctx, camp_id, front_sys, delta);
        assert_eq!(cur_st, stage);
        eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
        let tid = with_combat_runtime(&ctx, |rt| {
            rt.npc_entities
                .values()
                .find_map(|n| {
                    let t = ctx.sde.types.get(&n.type_id)?;
                    (n.solar_system_id == front_sys && t.group_id == 4638).then_some(n.type_id)
                })
                .expect("SDE group 4638 Enforcer roamer")
        });
        enf_tids_by_stage.push(tid);
    }
    assert!(
        total_sde_hp(&ctx, enf_tids_by_stage[1]) > total_sde_hp(&ctx, enf_tids_by_stage[0]),
        "Stage 5 Enforcer roamer must escalate to a higher-HP SDE hull than Stage 3"
    );

    let (enf_bid, enf_pos) = with_combat_runtime(&ctx, |rt| {
        let n = rt
            .npc_entities
            .values()
            .find(|n| {
                n.solar_system_id == front_sys
                    && ctx.sde.types.get(&n.type_id).is_some_and(|t| t.group_id == 4638)
            })
            .unwrap();
        (n.ball_id, n.spawn_position)
    });
    // Enforcer stays neutral to Empire militia & neutral pilots, and engages ALL pirate militias (`500_010` and `500_011`)!
    {
        let mut bp = sys_h.ballpark.write().unwrap_or_else(|e| e.into_inner());
        bp.set_ball_position(emp_ship, enf_pos + DVec3::new(10_000.0, 0.0, 0.0));
        bp.set_ball_position(neu_ship, enf_pos + DVec3::new(12_000.0, 0.0, 0.0));
        bp.set_ball_position(rival_pir_ship, enf_pos + DVec3::new(900_000.0, 0.0, 0.0));
        bp.set_ball_position(pir_ship, enf_pos + DVec3::new(910_000.0, 0.0, 0.0));
    }
    step_extended_npc_ai_sync(&ctx, front_sys);
    with_combat_runtime(&ctx, |rt| {
        assert_eq!(
            rt.npc_entities[&enf_bid].target_id, None,
            "InsurgencyEnforcer must remain neutral to Empire and neutral pilots"
        );
    });
    {
        let mut bp = sys_h.ballpark.write().unwrap_or_else(|e| e.into_inner());
        bp.set_ball_position(rival_pir_ship, enf_pos + DVec3::new(13_000.0, 0.0, 0.0));
    }
    step_extended_npc_ai_sync(&ctx, front_sys);
    with_combat_runtime(&ctx, |rt| {
        assert_eq!(
            rt.npc_entities[&enf_bid].target_id,
            Some(rival_pir_ship),
            "InsurgencyEnforcer must engage rival pirate militia (`500_011` / `500_010`) at Suppression >= 3"
        );
    });
    {
        let mut bp = sys_h.ballpark.write().unwrap_or_else(|e| e.into_inner());
        bp.set_ball_position(rival_pir_ship, enf_pos + DVec3::new(900_000.0, 0.0, 0.0));
        bp.set_ball_position(pir_ship, enf_pos + DVec3::new(14_000.0, 0.0, 0.0));
    }
    step_extended_npc_ai_sync(&ctx, front_sys);
    with_combat_runtime(&ctx, |rt| {
        assert_eq!(rt.npc_entities[&enf_bid].target_id, Some(pir_ship), "InsurgencyEnforcer must engage campaign pirate militia at Suppression >= 3");
    });
    let (ci_bid, ci_pos) = with_combat_runtime(&ctx, |rt| {
        let ci = rt.npc_entities.values().find(|n| n.solar_system_id == front_sys && n.type_id == 79_507).expect("CI Sentry 79507");
        (ci.ball_id, ci.spawn_position)
    });
    {
        let mut bp = sys_h.ballpark.write().unwrap_or_else(|e| e.into_inner());
        bp.set_ball_position(pir_ship, ci_pos + DVec3::new(900_000.0, 0.0, 0.0));
        bp.set_ball_position(rival_pir_ship, ci_pos + DVec3::new(8_000.0, 0.0, 0.0));
    }
    step_extended_npc_ai_sync(&ctx, front_sys);
    with_combat_runtime(&ctx, |rt| {
        assert_eq!(rt.npc_entities[&ci_bid].target_id, Some(rival_pir_ship), "Counter-Insurgency Sentry Gun must engage rival pirate militia");
    });

    // 5. Despawn when Corruption drops < 2 and Suppression drops < 3:
    let (_c_low_pts, c_low_st) = add_system_corruption(&ctx, camp_id, front_sys, -800);
    assert_eq!(c_low_st, 1);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    assert!(
        !with_combat_runtime(&ctx, |rt| rt.npc_entities.values().any(|n| {
            n.solar_system_id == front_sys
                && ctx.sde.types.get(&n.type_id).is_some_and(|t| t.group_id == 4637)
        })),
        "Pirate roamers must despawn when Corruption drops below Stage 2"
    );
    assert!(sys_h.get_ball_snapshot(pir_bid).await.unwrap().is_none());

    let (_s_low_pts, s_low_st) = add_system_suppression(&ctx, camp_id, front_sys, -600);
    assert_eq!(s_low_st, 2);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    assert!(
        !with_combat_runtime(&ctx, |rt| rt.npc_entities.values().any(|n| {
            n.solar_system_id == front_sys
                && ctx.sde.types.get(&n.type_id).is_some_and(|t| t.group_id == 4638)
        })),
        "Enforcer roamers must despawn when Suppression drops below Stage 3"
    );
    assert!(sys_h.get_ball_snapshot(enf_bid).await.unwrap().is_none());
}

#[tokio::test]
async fn test_campaign_end_lawless_security_notification_and_web_range() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let (enemy_sys, front_sys, _cmd_sys, _rear_sys) = setup_warzone_chain(&ctx);

    let emp_cid = 90_000_091i64;
    let emp_ship = 100_000_091i64;
    let vic_cid = 90_000_092i64;
    let vic_ship = 100_000_092i64;

    let mut s_emp = register_online_pilot(
        &ctx,
        89_001,
        "demo",
        "demo-secret",
        emp_cid,
        98_000_091,
        None,
        front_sys,
        emp_ship,
        639,
        DVec3::ZERO,
        0,
    )
    .await;
    let mut s_vic = register_online_pilot(
        &ctx,
        89_002,
        "demo2",
        "demo2-secret",
        vic_cid,
        98_000_092,
        None,
        front_sys,
        vic_ship,
        621,
        DVec3::new(10_500.0, 0.0, 0.0),
        0,
    )
    .await;

    rpc_call(
        &ctx,
        &mut s_emp,
        "fwCharacterEnlistmentMgr",
        "CreateMyDirectEnlistment",
        &[PyRep::Integer(500_001)],
    )
    .await
    .expect("enlist Caldari");
    rpc_call(
        &ctx,
        &mut s_vic,
        "fwCharacterEnlistmentMgr",
        "CreateMyDirectEnlistment",
        &[PyRep::Integer(500_010)],
    )
    .await
    .expect("enlist Guristas");

    let _ = rpc_call(
        &ctx,
        &mut s_emp,
        "dungeonInstanceCacheMgr",
        "GetFactionWarfareInstances",
        &[],
    )
    .await
    .unwrap();
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    let camp_id = ctx.db.with_snapshot(|s| *s.insurgency_runtime.campaigns.keys().next().unwrap());
    fast_forward_secs(&ctx, FORECASTING_DURATION_SECS + 10);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;

    // 1. Live Stasis Webifier (`type_id = 526`, 10,000 m optimal) `+10%` range at Suppression Stage 4+ (10,500 m):
    let web_id = 60_000_091i64;
    ctx.db.with_mut_snapshot(|s| {
        s.items.insert(
            web_id,
            InventoryItemRecord {
                item_id: web_id,
                type_id: 526,
                owner_id: emp_cid,
                location_id: emp_ship,
                flag: InventoryFlag::MedSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });
    ctx.invalidate_ship_fitting(emp_ship);
    let sys_h = ctx.systems.get_or_spawn(front_sys);
    let _ = sys_h
        .add_target_lock(
            emp_ship,
            eve_destiny::TargetLock {
                target_id: vic_ship,
                lock_time_ms: 0.0,
                remaining_lock_ms: 0.0,
                locked: true,
                max_range_m: Some(50_000.0),
            },
        )
        .await;

    let (_s3_pts, s3_st) = add_system_suppression(&ctx, camp_id, front_sys, 600);
    assert_eq!(s3_st, 3);
    let err_web_s3 = rpc_call(
        &ctx,
        &mut s_emp,
        "dogmaIM",
        "Activate",
        &[
            PyRep::Integer(web_id),
            PyRep::String("shipModifiers".into()),
            PyRep::Integer(vic_ship),
        ],
    )
    .await
    .unwrap_err();
    assert_eq!(
        err_web_s3.code, "TargetOutOfRange",
        "10,500 m exceeds 10,000 m Stasis Webifier optimal at Suppression Stage 3"
    );

    let (_s4_pts, s4_st) = add_system_suppression(&ctx, camp_id, front_sys, 200);
    assert_eq!(s4_st, 4);
    let base_vic_vel = sys_h.get_ball_snapshot(vic_ship).await.unwrap().unwrap().1.max_velocity;
    let ok_web_s4 = rpc_call(
        &ctx,
        &mut s_emp,
        "dogmaIM",
        "Activate",
        &[
            PyRep::Integer(web_id),
            PyRep::String("shipModifiers".into()),
            PyRep::Integer(vic_ship),
        ],
    )
    .await;
    assert!(
        ok_web_s4.is_ok(),
        "10,500 m succeeds within +10% (11,000 m) Stasis Webifier optimal at Suppression Stage 4: {ok_web_s4:?}"
    );
    for _ in 0..3 {
        let _ = sys_h.step_manual(1.0).await.unwrap();
    }
    let webbed_vic_vel = sys_h.get_ball_snapshot(vic_ship).await.unwrap().unwrap().1.max_velocity;
    assert!(
        webbed_vic_vel < base_vic_vel * 0.6,
        "Stasis Webifier must keep target max_velocity reduced across 1 Hz ticks at 10,500 m ({webbed_vic_vel} vs {base_vic_vel})"
    );

    // 1b. Quasar `IncreasedNotice` reports pre-mutation `old_pts` on Suppression Stage 3 `-30%` pulses
    //     and emits no `IncreasedNotice` when capped at 800 pts under Suppression Stage 5:
    let _ = rpc_call(&ctx, &mut s_emp, "publicGatewaySvc", "Subscribe", &[]).await.unwrap();
    let _ = ctx.quasar_gateway.poll_notices(s_emp.session_id);
    assert_eq!(add_system_corruption(&ctx, camp_id, front_sys, 100), (70, 1));
    assert_eq!(add_system_corruption(&ctx, camp_id, front_sys, 100), (140, 1));
    let inc_notices: Vec<_> = ctx
        .quasar_gateway
        .poll_notices(s_emp.session_id)
        .into_iter()
        .filter_map(|n| n.payload)
        .filter(|p| p.type_url.ends_with("pirate.corruption.api.IncreasedNotice"))
        .collect();
    assert_eq!(inc_notices.len(), 2, "Expected 2 IncreasedNotice events for 0->70 and 70->140");
    let old_seq = field(&inc_notices[1].value, 2).and_then(|b| varint(&b, 1));
    let new_seq = field(&inc_notices[1].value, 3).and_then(|b| varint(&b, 1));
    assert_eq!((old_seq, new_seq), (Some(70), Some(140)), "IncreasedNotice must report pre-mutation old_pts=70 and new_pts=140");

    assert_eq!(add_system_suppression(&ctx, camp_id, front_sys, 200), (1000, 5));
    assert_eq!(add_system_corruption(&ctx, camp_id, front_sys, 1_500), (800, 4));
    let _ = ctx.quasar_gateway.poll_notices(s_emp.session_id);
    assert_eq!(add_system_corruption(&ctx, camp_id, front_sys, 100), (800, 4));
    let capped_inc = ctx
        .quasar_gateway
        .poll_notices(s_emp.session_id)
        .into_iter()
        .filter_map(|n| n.payload)
        .filter(|p| p.type_url.ends_with("pirate.corruption.api.IncreasedNotice"))
        .count();
    assert_eq!(capped_inc, 0, "Capped zero-change corruption pulse at Suppression Stage 5 must not emit IncreasedNotice");

    // 2. Campaign end on Stage-5 LAWLESS system emits `OnSecurityModified` back to SDE base security,
    //    while manual overrides in `modified_system_securities` remain untouched:
    with_mut_combat_runtime(&ctx, |rt| {
        rt.modified_system_securities.insert(enemy_sys, 0.25);
    });
    assert_eq!(add_system_suppression(&ctx, camp_id, front_sys, -1_000), (0, 0));
    let (_c5_pts, c5_st) = add_system_corruption(&ctx, camp_id, front_sys, 1_500);
    assert_eq!(c5_st, 5);
    let (_ec5_pts, ec5_st) = add_system_corruption(&ctx, camp_id, enemy_sys, 1_000);
    assert_eq!(ec5_st, 5);
    ctx.drain_notifications_for_char(emp_cid);

    let base_front_sec = ctx.sde.solar_systems[&front_sys].security_status;
    end_insurgency_campaign(&ctx, camp_id);
    let notes = ctx.drain_notifications_for_char(emp_cid);
    assert!(
        notes.iter().any(|n| n.event == "OnSecurityModified"
            && n.args
                == vec![
                    PyRep::Integer(i64::from(front_sys)),
                    PyRep::Real(0.0),
                    PyRep::Real(base_front_sec),
                ]),
        "Ending campaign on Stage 5 LAWLESS system must emit OnSecurityModified back to base security"
    );
    assert!(
        !notes.iter().any(|n| n.event == "OnSecurityModified"
            && n.args.first() == Some(&PyRep::Integer(i64::from(enemy_sys)))),
        "Manual override system must not emit OnSecurityModified on Stage 5 exit"
    );
    let mod_after = rpc_call(&ctx, &mut s_emp, "securityMgr", "get_modified_systems", &[])
        .await
        .unwrap();
    let PyRep::Dict(entries) = mod_after else {
        panic!("expected Dict from securityMgr.get_modified_systems");
    };
    assert!(
        entries
            .iter()
            .any(|(k, v)| k == &PyRep::Integer(i64::from(enemy_sys)) && v == &PyRep::Real(0.25)),
        "Manual override (0.25) must remain in get_modified_systems"
    );
    assert!(
        !entries
            .iter()
            .any(|(k, _)| k == &PyRep::Integer(i64::from(front_sys))),
        "front_sys must no longer appear in get_modified_systems after campaign end"
    );
}

#[tokio::test]
async fn test_highsec_stage5_safety_levels_and_faction_police_suppression() {
    use eve_server::services::combat_crimewatch::crimewatch_timers::{
        evaluate_and_record_pvp_aggression, evaluate_faction_police_response,
    };
    use eve_server::services::combat_crimewatch::SafetyLevel;

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let (_enemy_sys, front_sys, _cmd_sys, _rear_sys) = setup_warzone_chain(&ctx);
    let hs_sys = 30_045_324u32; // Onnamon (0.5555 Highsec)
    let atk_cid = 90_000_095i64;
    let atk_ship = 100_000_095i64;
    let vic_cid = 90_000_096i64;
    let vic_ship = 100_000_096i64;
    let vic_pod = 100_000_097i64;

    let mut s_emp = register_online_pilot(&ctx, 89_005, "demo", "demo-secret", atk_cid, 98_000_095, None, hs_sys, atk_ship, 639, DVec3::ZERO, 0).await;
    ctx.db.with_mut_snapshot(|s| {
        let mut v_ch = s.characters[&atk_cid].clone();
        v_ch.character_id = vic_cid;
        v_ch.name = "Highsec Victim".into();
        v_ch.solar_system_id = hs_sys;
        v_ch.active_ship_id = vic_ship;
        s.characters.insert(vic_cid, v_ch);
        s.characters.get_mut(&atk_cid).unwrap().security_status = -5.0;
        for (sid, tid, owner) in [(vic_ship, 621, vic_cid), (vic_pod, 670, vic_cid)] {
            s.items.insert(sid, InventoryItemRecord {
                item_id: sid, type_id: tid, owner_id: owner, location_id: i64::from(hs_sys),
                flag: InventoryFlag::None, quantity: 1, singleton: true, ..Default::default()
            });
        }
    });
    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&atk_cid).unwrap().solar_system_id = front_sys;
    });
    rpc_call(&ctx, &mut s_emp, "fwCharacterEnlistmentMgr", "CreateMyDirectEnlistment", &[PyRep::Integer(500_010)]).await.unwrap();
    let _ = rpc_call(&ctx, &mut s_emp, "dungeonInstanceCacheMgr", "GetFactionWarfareInstances", &[]).await.unwrap();
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    let now_u = ctx.now_unix();
    let camp_id = ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&atk_cid).unwrap().solar_system_id = hs_sys;
        let (&cid, c) = s.insurgency_runtime.campaigns.iter_mut().next().unwrap();
        c.forecasting_end_unix = now_u.saturating_sub(1);
        if !c.systems.contains(&hs_sys) { c.systems.push(hs_sys); }
        cid
    });

    // 1. Faction Police suppression in Highsec Stage 2..=4 (`evaluate_faction_police_response`):
    assert_eq!(add_system_corruption(&ctx, camp_id, hs_sys, 200), (200, 1));
    assert_eq!(evaluate_faction_police_response(&ctx, hs_sys, atk_cid, atk_ship).await, (true, false, true));
    assert_eq!(add_system_corruption(&ctx, camp_id, hs_sys, 400), (600, 3));
    assert_eq!(evaluate_faction_police_response(&ctx, hs_sys, atk_cid, atk_ship).await, (false, true, false));

    // 2. Highsec Corruption Stage 5 (`LAWLESS`) safety levels: Green blocks, Yellow & Red allow Suspect ship attacks, Yellow blocks pods:
    assert_eq!(add_system_corruption(&ctx, camp_id, hs_sys, 400), (1000, 5));
    with_mut_combat_runtime(&ctx, |rt| { rt.safety_levels.insert(atk_cid, SafetyLevel::Green); });
    assert_eq!(evaluate_and_record_pvp_aggression(&ctx, atk_cid, atk_ship, hs_sys, vic_ship, false).unwrap_err().code, "CantDoInCurrentSafetyLevel");

    with_mut_combat_runtime(&ctx, |rt| { rt.safety_levels.insert(atk_cid, SafetyLevel::Yellow); });
    assert_eq!(evaluate_and_record_pvp_aggression(&ctx, atk_cid, atk_ship, hs_sys, vic_pod, true).unwrap_err().code, "CantDoInCurrentSafetyLevel");
    assert!(evaluate_and_record_pvp_aggression(&ctx, atk_cid, atk_ship, hs_sys, vic_ship, false).is_ok());
    {
        let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
        let st = cw.get_or_create(atk_cid);
        assert!(st.is_suspect(now_u) && !st.is_criminal(now_u) && st.pending_concord_arrival.is_none());
    }
    let _ = front_sys;
}

}

#[allow(dead_code, unused_imports)]
pub(crate) mod insurgency_victory_points {
    use super::*;

//  Part 7 of FW & Insurgency closure tests:
//  - Highsec Corruption Stage 5 (`LAWLESS`) `dogmaIM.Activate` safety checks for Vorton Projectors
//    (`group_id == 4060`) and cross-faction FW turret shots (`SafetyLevel::Yellow` allowed on ships,
//    blocked on `SafetyLevel::Green` and on Capsules `type_id == 670`).
//  - Vorton Projector secondary arc and Smartbomb (`group_id == 72`) hits on Capsules (`670`) in
//    Highsec Stage 5 escalating to Criminal flag + CONCORD response.
//  - SQLite reload durability of `system_corruption_stages` and `is_pilot_aligned_with_insurgency_pirate`
//    returning `false` when no active/winddown campaign covers a solar system.


use crate::combat_weapons_wrecks::triglavian_entropic_spool_edencom_vorton::fit_module_and_charge;
use crate::fw_warzone_insurgency::setup_warzone_chain;
use crate::sovereignty_equinox_gameplay::{make_ball, register_online_pilot, rpc_call};
use eve_db::{
    FwSystemOccupationRecord, InventoryFlag, InventoryItemRecord, TrainedSkillRecord,
    VirtualServerClock, WorldRepository,
};
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::combat_crimewatch::{with_mut_combat_runtime, SafetyLevel};
use eve_server::services::insurgency_services::{
    add_system_corruption, init_insurgency_campaign,
};
use eve_server::services::security_mgr::is_pilot_aligned_with_insurgency_pirate;
use glam::DVec3;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

fn temp_sqlite_path(tag: &str) -> PathBuf {
    static SEQ: AtomicU64 = AtomicU64::new(1);
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let p = std::env::temp_dir().join(format!("evomu_hs_stage5_{tag}_{}_{n}.sqlite", std::process::id()));
    let _ = std::fs::remove_file(&p);
    p
}

async fn lock_ball(ctx: &ServerContext, sys_id: u32, ship_id: i64, target_id: i64) {
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_h
        .add_target_lock(
            ship_id,
            eve_destiny::TargetLock {
                target_id,
                lock_time_ms: 0.0,
                remaining_lock_ms: 0.0,
                locked: true,
                max_range_m: Some(150_000.0),
            },
        )
        .await;
}

#[tokio::test]
async fn test_highsec_stage5_vorton_weapons_yellow_safety_and_arc_capsule_concord() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let (_enemy_sys, front_sys, _cmd_sys, _rear_sys) = setup_warzone_chain(&ctx);
    let hs_sys = 30_045_324u32; // Onnamon (0.5555 Highsec)

    let atk_cid = 90_000_191i64;
    let atk_ship = 100_000_191i64;
    let vic_cid = 90_000_192i64;
    let vic_ship = 100_000_192i64;
    let pod_cid = 90_000_193i64;
    let pod_ball = 100_000_193i64;

    // 54731 = Skybreaker (EDENCOM frigate)
    let mut s_atk = register_online_pilot(
        &ctx, 89_101, "demo", "demo-secret", atk_cid, 98_000_191, None, front_sys, atk_ship, 54_731, DVec3::ZERO, 0,
    )
    .await;
    rpc_call(&ctx, &mut s_atk, "fwCharacterEnlistmentMgr", "CreateMyDirectEnlistment", &[PyRep::Integer(500_001)])
        .await
        .expect("enlist Caldari");
    let camp_id = 501i64;
    init_insurgency_campaign(&ctx, camp_id, 500_010, 500_001, front_sys, vec![front_sys, hs_sys]);

    let now0 = ctx.now_unix();
    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&atk_cid).unwrap().solar_system_id = hs_sys;
        s.items.get_mut(&atk_ship).unwrap().location_id = i64::from(hs_sys);
        // Mark hs_sys FW-occupied and victim enlisted in non-opposing Amarr (`500_003`) so cross-faction
        // turret activation (`kf != vf`) ALSO exercises `fitting_module_cycles -> handle_weapons`.
        s.fw_system_occupations.insert(
            hs_sys,
            FwSystemOccupationRecord {
                solar_system_id: hs_sys,
                occupier_faction_id: 500_001,
                owner_faction_id: 500_001,
                victory_points: 0,
                victory_points_threshold: 3_000,
            },
        );
        for (cid, sid, tid, name, wf) in [
            (vic_cid, vic_ship, 621u32, "Amarr Neutral Ally", Some(500_003i64)),
            (pod_cid, pod_ball, 670u32, "Bystander Capsule", None),
        ] {
            let mut ch = s.characters[&atk_cid].clone();
            ch.character_id = cid;
            ch.name = name.into();
            ch.solar_system_id = hs_sys;
            ch.active_ship_id = sid;
            ch.war_faction_id = wf;
            s.characters.insert(cid, ch);
            s.items.insert(
                sid,
                InventoryItemRecord {
                    item_id: sid,
                    type_id: tid,
                    owner_id: cid,
                    location_id: i64::from(hs_sys),
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
        }
        for &skill_id in &[3300u32, 3302, 55031, 55033, 55034] {
            s.skills.entry(atk_cid).or_default().insert(
                skill_id,
                TrainedSkillRecord { type_id: skill_id, level: 5, skill_points: 256_000.0 },
            );
        }
        s.insurgency_runtime.campaigns.get_mut(&camp_id).unwrap().forecasting_end_unix = now0.saturating_sub(1);
    });
    s_atk.solar_system_id = Some(hs_sys);
    eve_server::services::space_destiny::clear_session_change_timer(&ctx, atk_cid);

    let hs_h = ctx.systems.get_or_spawn(hs_sys);
    let _ = hs_h
        .spawn_ball_with_combat(make_ball(atk_ship, 54_731, DVec3::ZERO), ctx.build_ship_combat_state(atk_cid, atk_ship, 54_731))
        .await;
    let _ = hs_h
        .spawn_ball_with_combat(make_ball(vic_ship, 621, DVec3::new(3_000.0, 0.0, 0.0)), ctx.build_type_combat_state(621))
        .await;
    let _ = hs_h
        .spawn_ball_with_combat(make_ball(pod_ball, 670, DVec3::new(80_000.0, 0.0, 0.0)), ctx.build_type_combat_state(670))
        .await;

    let vorton_id = 701_001i64;
    let turret_id = 701_003i64;
    fit_module_and_charge(&ctx, atk_cid, atk_ship, vorton_id, 54_739, InventoryFlag::HiSlot0, Some((701_002, 54_769, 200)));
    fit_module_and_charge(&ctx, atk_cid, atk_ship, turret_id, 2_873, InventoryFlag::HiSlot1, Some((701_004, 182, 200)));
    ctx.db.with_mut_snapshot(|s| {
        s.module_online_states.insert(vorton_id, true);
        s.module_online_states.insert(turret_id, true);
    });
    ctx.invalidate_ship_fitting(atk_ship);
    lock_ball(&ctx, hs_sys, atk_ship, vic_ship).await;
    lock_ball(&ctx, hs_sys, atk_ship, pod_ball).await;

    assert_eq!(add_system_corruption(&ctx, camp_id, hs_sys, 1_000), (1_000, 5));

    // 1. Safety Green blocks Vorton & cross-faction turret against non-pod ship in Highsec Stage 5:
    with_mut_combat_runtime(&ctx, |rt| {
        rt.safety_levels.insert(atk_cid, SafetyLevel::Green);
    });
    let err_green = rpc_call(&ctx, &mut s_atk, "dogmaIM", "Activate", &[PyRep::Integer(vorton_id), PyRep::Integer(vic_ship)])
        .await
        .unwrap_err();
    assert_eq!(err_green.code, "CantDoInCurrentSafetyLevel");

    // 2. Safety Yellow blocks direct Capsule (`670`) attack, but ALLOWS Vorton & cross-faction turret on non-pod ship (Suspect, no CONCORD):
    with_mut_combat_runtime(&ctx, |rt| {
        rt.safety_levels.insert(atk_cid, SafetyLevel::Yellow);
    });
    let err_pod_yellow = rpc_call(&ctx, &mut s_atk, "dogmaIM", "Activate", &[PyRep::Integer(vorton_id), PyRep::Integer(pod_ball)])
        .await
        .unwrap_err();
    assert_eq!(err_pod_yellow.code, "CantDoInCurrentSafetyLevel");

    rpc_call(&ctx, &mut s_atk, "dogmaIM", "Activate", &[PyRep::Integer(vorton_id), PyRep::Integer(vic_ship)])
        .await
        .expect("Vorton Activate on non-pod ship must succeed under Safety Yellow in Highsec Stage 5");
    rpc_call(&ctx, &mut s_atk, "dogmaIM", "Activate", &[PyRep::Integer(turret_id), PyRep::Integer(vic_ship)])
        .await
        .expect("Cross-faction FW turret Activate on non-pod ship must succeed under Safety Yellow in Highsec Stage 5");

    let now_shot1 = ctx.now_unix();
    let (susp1, crim1, concord1) = ctx.crimewatch.with_crimewatch(|cw| {
        let st = &cw.states[&atk_cid];
        (st.is_suspect(now_shot1), st.is_criminal(now_shot1), st.pending_concord_arrival.is_some())
    });
    assert_eq!(
        (susp1, crim1, concord1),
        (true, false, false),
        "Highsec Stage 5 non-pod Vorton/turret shots under Safety Yellow must grant Suspect without Criminal or CONCORD"
    );

    // 3. Move Capsule (`670`) within 10 km Vorton secondary arc range of `vic_ship`:
    //    - Under Safety Yellow, Vorton skips the Capsule secondary arc without Criminal/CONCORD.
    //    - Under Safety Red, Vorton secondary arc hits Capsule (`670`) and escalates to Criminal + CONCORD!
    {
        let mut bp = hs_h.ballpark.write().unwrap_or_else(|e| e.into_inner());
        bp.set_ball_position(pod_ball, DVec3::new(3_500.0, 0.0, 0.0));
    }
    ctx.advance_secs(10);
    ctx.db.with_mut_snapshot(|s| {
        s.module_active_states.remove(&vorton_id);
    });
    let v_yellow_arc = rpc_call(&ctx, &mut s_atk, "dogmaIM", "Activate", &[PyRep::Integer(vorton_id), PyRep::Integer(vic_ship)])
        .await
        .expect("Vorton shot with nearby pod under Safety Yellow");
    assert_eq!(v_yellow_arc.dict_get("secondaryTargetsHit").and_then(|v| v.as_i64()), Some(0));

    with_mut_combat_runtime(&ctx, |rt| {
        rt.safety_levels.insert(atk_cid, SafetyLevel::Red);
    });
    ctx.advance_secs(10);
    ctx.db.with_mut_snapshot(|s| {
        s.module_active_states.remove(&vorton_id);
    });
    let v_red_arc = rpc_call(&ctx, &mut s_atk, "dogmaIM", "Activate", &[PyRep::Integer(vorton_id), PyRep::Integer(vic_ship)])
        .await
        .expect("Vorton shot chaining onto Capsule under Safety Red");
    assert_eq!(v_red_arc.dict_get("secondaryTargetsHit").and_then(|v| v.as_i64()), Some(1));

    let now_arc = ctx.now_unix();
    let (arc_crim, arc_concord) = ctx.crimewatch.with_crimewatch(|cw| {
        let st = &cw.states[&atk_cid];
        (st.is_criminal(now_arc), st.pending_concord_arrival.is_some())
    });
    assert_eq!(
        (arc_crim, arc_concord),
        (true, true),
        "Vorton secondary arc hitting Capsule (670) in Highsec Stage 5 must escalate to Criminal + CONCORD"
    );
    ctx.advance_secs(25);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    let balls_after = hs_h.list_balls().await.unwrap();
    assert!(
        balls_after.iter().any(|b| b.type_id == 11_957),
        "CONCORD Police Commander (11957) must spawn after Vorton secondary arc hits a Capsule in Highsec Stage 5"
    );
}

#[tokio::test]
async fn test_highsec_stage5_smartbomb_capsule_concord_and_corruption_reload_persistence() {
    let db_path = temp_sqlite_path("smartbomb_reload");
    let hs_sys = 30_045_324u32; // Onnamon (0.5555 Highsec)
    let jita_sys = 30_000_142u32;
    let t0 = 1_770_000_000u64;

    let atk_cid = 90_000_001i64;
    let atk_ship = 100_000_001i64;
    let ship_vic_cid = 90_000_202i64;
    let ship_vic_ball = 100_000_202i64;
    let pod_vic_cid = 90_000_203i64;
    let pod_vic_ball = 100_000_203i64;
    let sb_mod_id = 702_001i64;
    let sb_mod_id2 = 702_002i64;

    // Phase 1: Persist Highsec Stage 5 campaign & system state to SQLite, and verify `!any_covering => false`
    {
        let repo = Arc::new(WorldRepository::open_sqlite(&db_path).expect("open_sqlite 1"));
        let seed_snap = WorldRepository::with_demo_seed().with_snapshot(|s| s.clone());
        repo.with_mut_snapshot(|snap| *snap = seed_snap);
        let clock = Arc::new(VirtualServerClock::with_fixed_base(t0));
        repo.set_clock(clock.clone());
        let ctx1 = ServerContext::with_repository(repo);
        ctx1.set_clock(clock);

        let (_enemy_sys, front_sys, _cmd_sys, _rear_sys) = setup_warzone_chain(&ctx1);
        let mut s1 = register_online_pilot(
            &ctx1, 89_201, "demo", "demo-secret", atk_cid, 98_000_001, None, front_sys, atk_ship, 639, DVec3::ZERO, 0,
        )
        .await;
        rpc_call(&ctx1, &mut s1, "fwCharacterEnlistmentMgr", "CreateMyDirectEnlistment", &[PyRep::Integer(500_010)])
            .await
            .unwrap();
        let _ = rpc_call(&ctx1, &mut s1, "dungeonInstanceCacheMgr", "GetFactionWarfareInstances", &[])
            .await
            .unwrap();
        eve_server::sp6_lifecycle::tick_background_cluster(&ctx1).await;

        let camp_id = ctx1.db.with_mut_snapshot(|s| {
            // Uncovered system with stale `insurgency_corruption_stage = 5` must NOT grant pirate alignment!
            let j_st = s.system_states.entry(jita_sys).or_default();
            j_st.solar_system_id = jita_sys;
            j_st.insurgency_corruption_stage = 5;

            s.characters.get_mut(&atk_cid).unwrap().solar_system_id = hs_sys;
            s.items.get_mut(&atk_ship).unwrap().location_id = i64::from(hs_sys);
            for (mid, flag) in [(sb_mod_id, InventoryFlag::HiSlot0), (sb_mod_id2, InventoryFlag::HiSlot1)] {
                s.items.insert(
                    mid,
                    InventoryItemRecord {
                        item_id: mid,
                        type_id: 3_993, // Large EMP Smartbomb I (group 72, 6,000 m radius)
                        owner_id: atk_cid,
                        location_id: atk_ship,
                        flag,
                        quantity: 1,
                        singleton: true,
                        ..Default::default()
                    },
                );
                s.module_online_states.insert(mid, true);
            }
            for (cid, sid, tid, name) in [
                (ship_vic_cid, ship_vic_ball, 621u32, "Smartbomb Ship Victim"),
                (pod_vic_cid, pod_vic_ball, 670u32, "Smartbomb Pod Victim"),
            ] {
                let mut ch = s.characters[&atk_cid].clone();
                ch.character_id = cid;
                ch.name = name.into();
                ch.solar_system_id = hs_sys;
                ch.active_ship_id = sid;
                ch.war_faction_id = None;
                s.characters.insert(cid, ch);
                s.items.insert(
                    sid,
                    InventoryItemRecord {
                        item_id: sid,
                        type_id: tid,
                        owner_id: cid,
                        location_id: i64::from(hs_sys),
                        flag: InventoryFlag::None,
                        quantity: 1,
                        singleton: true,
                        ..Default::default()
                    },
                );
            }
            let (&cid, c) = s.insurgency_runtime.campaigns.iter_mut().next().unwrap();
            c.forecasting_end_unix = t0.saturating_sub(10);
            if !c.systems.contains(&hs_sys) {
                c.systems.push(hs_sys);
            }
            cid
        });

        assert!(
            !is_pilot_aligned_with_insurgency_pirate(&ctx1, jita_sys, atk_cid),
            "Pirate alignment must return false when no active/winddown campaign covers the solar system"
        );
        assert_eq!(add_system_corruption(&ctx1, camp_id, hs_sys, 1_000), (1_000, 5));
        assert!(is_pilot_aligned_with_insurgency_pirate(&ctx1, hs_sys, atk_cid));
    }

    // Phase 2: Reload fresh ServerContext from SQLite WITHOUT calling `add_system_corruption`!
    {
        let repo2 = Arc::new(WorldRepository::open_sqlite(&db_path).expect("open_sqlite 2"));
        let clock2 = Arc::new(VirtualServerClock::with_fixed_base(t0 + 30));
        repo2.set_clock(clock2.clone());
        let ctx2 = ServerContext::with_repository(repo2);
        ctx2.set_clock(clock2);

        assert_eq!(
            ctx2.crimewatch.read().unwrap().system_corruption_stages.get(&hs_sys),
            Some(&5),
            "Highsec Stage 5 system_corruption_stages must hydrate immediately on SQLite reload"
        );

        let mut s2 = eve_machonet::SessionState::new(89_202);
        crate::common::login_pilot(&ctx2, &mut s2, "demo", "demo-secret").await;
        rpc_call(&ctx2, &mut s2, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(atk_cid)])
            .await
            .unwrap();
        eve_server::services::space_destiny::clear_session_change_timer(&ctx2, atk_cid);

        let hs_h2 = ctx2.systems.get_or_spawn(hs_sys);
        let mut atk_cs = ctx2.build_ship_combat_state(atk_cid, atk_ship, 639);
        atk_cs.capacitor = 5_000.0;
        atk_cs.max_capacitor = 5_000.0;
        let _ = hs_h2.spawn_ball_with_combat(make_ball(atk_ship, 639, DVec3::ZERO), atk_cs).await;
        let _ = hs_h2
            .spawn_ball_with_combat(make_ball(ship_vic_ball, 621, DVec3::new(2_500.0, 0.0, 0.0)), ctx2.build_type_combat_state(621))
            .await;
        let _ = hs_h2
            .spawn_ball_with_combat(make_ball(pod_vic_ball, 670, DVec3::new(25_000.0, 0.0, 0.0)), ctx2.build_type_combat_state(670))
            .await;

        with_mut_combat_runtime(&ctx2, |rt| {
            rt.safety_levels.insert(atk_cid, SafetyLevel::Red);
        });

        // (a) Smartbomb hitting ONLY non-pod ship in reloaded Highsec Stage 5 -> Suspect, concordTriggered == false, no CONCORD!
        let sb_ship_res = rpc_call(&ctx2, &mut s2, "dogmaIM", "Activate", &[PyRep::Integer(sb_mod_id)])
            .await
            .expect("Smartbomb Activate hitting non-pod ship in Highsec Stage 5");
        assert_eq!(sb_ship_res.dict_get("concordTriggered"), Some(&PyRep::Bool(false)));
        let now1 = ctx2.now_unix();
        let (sb_susp, sb_crim, sb_concord) = ctx2.crimewatch.with_crimewatch(|cw| {
            let st = &cw.states[&atk_cid];
            (st.is_suspect(now1), st.is_criminal(now1), st.pending_concord_arrival.is_some())
        });
        assert_eq!(
            (sb_susp, sb_crim, sb_concord),
            (true, false, false),
            "Reloaded Highsec Stage 5 Smartbomb hitting only non-pod ship must grant Suspect without CONCORD"
        );

        // (b) Move Capsule (`670`) into 6 km blast radius and fire second Smartbomb -> Criminal + CONCORD!
        {
            let mut bp = hs_h2.ballpark.write().unwrap_or_else(|e| e.into_inner());
            bp.set_ball_position(pod_vic_ball, DVec3::new(2_000.0, 0.0, 0.0));
        }
        let sb_pod_res = rpc_call(&ctx2, &mut s2, "dogmaIM", "Activate", &[PyRep::Integer(sb_mod_id2)])
            .await
            .expect("Smartbomb Activate hitting Capsule in Highsec Stage 5");
        assert_eq!(sb_pod_res.dict_get("concordTriggered"), Some(&PyRep::Bool(true)));
        let now2 = ctx2.now_unix();
        let (pod_crim, pod_concord) = ctx2.crimewatch.with_crimewatch(|cw| {
            let st = &cw.states[&atk_cid];
            (st.is_criminal(now2), st.pending_concord_arrival.is_some())
        });
        assert_eq!(
            (pod_crim, pod_concord),
            (true, true),
            "Smartbomb hitting Capsule (670) in Highsec Stage 5 must escalate to Criminal + CONCORD"
        );
        ctx2.advance_secs(25);
        eve_server::sp6_lifecycle::tick_background_cluster(&ctx2).await;
        let balls_after = hs_h2.list_balls().await.unwrap();
        assert!(
            balls_after.iter().any(|b| b.type_id == 11_957),
            "CONCORD Police Commander (11957) must spawn after Smartbomb hits a Capsule in Highsec Stage 5"
        );
    }

    let _ = std::fs::remove_file(&db_path);
}

#[tokio::test]
async fn test_ci_sentry_grid_targeting_destruction_and_lowsec_sentry_separation() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let (enemy_sys, front_sys, _cmd_sys, _rear_sys) = setup_warzone_chain(&ctx);

    let emp_cid = 90_000_301i64;
    let emp_ship = 100_000_301i64;
    let neut_cid = 90_000_302i64;
    let neut_ship = 100_000_302i64;
    let crim_cid = 90_000_303i64;
    let crim_ship = 100_000_303i64;
    let pir_cid = 90_000_304i64;
    let pir_ship = 100_000_304i64;
    let offgrid_pir_cid = 90_000_305i64;
    let offgrid_pir_ship = 100_000_305i64;

    let mut s_emp = register_online_pilot(
        &ctx, 89_301, "demo", "demo-secret", emp_cid, 98_000_301, None, enemy_sys, emp_ship, 621, DVec3::new(15_000.0, 0.0, 0.0), 0,
    )
    .await;
    rpc_call(&ctx, &mut s_emp, "fwCharacterEnlistmentMgr", "CreateMyDirectEnlistment", &[PyRep::Integer(500_001)])
        .await
        .unwrap();

    init_insurgency_campaign(&ctx, 701, 500_010, 500_001, front_sys, vec![front_sys, enemy_sys]);
    let now0 = ctx.now_unix();
    ctx.db.with_mut_snapshot(|s| {
        s.insurgency_runtime.campaigns.get_mut(&701).unwrap().forecasting_end_unix = now0.saturating_sub(1);
        for (cid, sid, wf, name) in [
            (neut_cid, neut_ship, None, "Neutral Suspect"),
            (crim_cid, crim_ship, None, "Lowsec Criminal"),
            (pir_cid, pir_ship, Some(500_010i64), "On-Grid Pirate"),
            (offgrid_pir_cid, offgrid_pir_ship, Some(500_010i64), "Off-Grid Pirate"),
        ] {
            let mut ch = s.characters[&emp_cid].clone();
            ch.character_id = cid;
            ch.name = name.into();
            ch.active_ship_id = sid;
            ch.war_faction_id = wf;
            s.characters.insert(cid, ch);
            s.items.insert(
                sid,
                InventoryItemRecord {
                    item_id: sid, type_id: 621, owner_id: cid, location_id: i64::from(enemy_sys),
                    flag: InventoryFlag::None, quantity: 1, singleton: true, ..Default::default()
                },
            );
        }
    });
    // Reach Corruption 5 FIRST, then Suppression 5 in `enemy_sys`:
    assert_eq!(add_system_corruption(&ctx, 701, enemy_sys, 1_000), (1_000, 5));
    assert_eq!(eve_server::services::insurgency_services::add_system_suppression(&ctx, 701, enemy_sys, 1_000), (1_000, 5));
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;

    let sys_h = ctx.systems.get_or_spawn(enemy_sys);
    for (sid, pos) in [
        (emp_ship, DVec3::new(15_000.0, 0.0, 0.0)),
        (neut_ship, DVec3::new(20_000.0, 0.0, 0.0)),
        (crim_ship, DVec3::new(30_000.0, 0.0, 0.0)),
        (pir_ship, DVec3::new(120_000.0, 0.0, 0.0)),      // > 75 km (TP only, no ECM)
        (offgrid_pir_ship, DVec3::new(900_000.0, 0.0, 0.0)), // > 500 km off-grid
    ] {
        let mut cs = ctx.build_type_combat_state(621);
        cs.shield_hp = 50_000.0;
        cs.max_shield_hp = 50_000.0;
        let _ = sys_h.spawn_ball_with_combat(make_ball(sid, 621, pos), cs).await;
    }
    // Move roaming pirates/enforcers far away so we isolate `79_507` Counter-Insurgency Sentry Gun behavior:
    let all_balls = sys_h.list_balls().await.unwrap();
    let ci_ball_id = all_balls.iter().find(|b| b.type_id == 79_507).expect("79507 deployed").id;
    {
        let mut bp = sys_h.ballpark.write().unwrap_or_else(|e| e.into_inner());
        for b in all_balls.iter().filter(|b| b.id != ci_ball_id && b.id < 100_000_300) {
            bp.set_ball_position(b.id, DVec3::new(-900_000.0, 0.0, 0.0));
        }
    }
    assert!(eve_server::services::security_mgr::has_active_counter_insurgency_sentries(&ctx, enemy_sys));
    assert!(eve_server::services::combat_npc_police::is_sentry_disabled_by_corruption(&ctx, enemy_sys));
    assert_eq!(
        eve_server::services::combat_crimewatch::step_sentry_gun_grid_aggression(&ctx, enemy_sys, crim_cid, crim_ship, DVec3::ZERO, true).await,
        (false, 0.0),
        "Regular gate/station sentries must stay disabled at Corruption 5 and never morph into 79507"
    );

    // Flag Empire militia pilot AND neutral pilot as Suspects, and `crim_cid` as Criminal:
    {
        let mut cw = ctx.crimewatch.write().unwrap();
        cw.trigger_suspect_flag(emp_cid, now0);
        cw.trigger_suspect_flag(neut_cid, now0);
        cw.get_or_create(crim_cid).criminal_flag_until_unix = now0 + 900;
    }
    let base_sig = ctx.build_type_combat_state(621).signature_radius;

    // Step 1: `79_507` shoots closest valid target (`crim_ship` at ~30 km -> volley + TP + ECM),
    //         while completely ignoring closer `emp_ship` (15 km Suspect) and `neut_ship` (20 km Suspect)!
    eve_server::services::combat_crimewatch::combat_npc_ai::step_extended_npc_ai_sync(&ctx, enemy_sys);
    let emp_cs = sys_h.get_combat_state(emp_ship).await.unwrap().unwrap();
    let neut_cs = sys_h.get_combat_state(neut_ship).await.unwrap().unwrap();
    let crim_cs = sys_h.get_combat_state(crim_ship).await.unwrap().unwrap();
    assert_eq!((emp_cs.shield_hp, emp_cs.signature_radius, emp_cs.ecm_jammed_remaining_s), (50_000.0, base_sig, 0.0), "Empire Suspect must NOT be shot/painted/jammed by 79507");
    assert_eq!((neut_cs.shield_hp, neut_cs.signature_radius, neut_cs.ecm_jammed_remaining_s), (50_000.0, base_sig, 0.0), "Neutral Suspect must NOT be shot/painted/jammed by 79507");
    assert!(crim_cs.shield_hp < 50_000.0 && (crim_cs.signature_radius - base_sig * 1.50).abs() < 1e-3 && (crim_cs.ecm_jammed_remaining_s - 5.0).abs() < 1e-6);

    // Step 2: Remove `crim_ship` so `79_507` retargets `pir_ship` at 120 km (within 250 km TP, outside 75 km ECM)
    //         while `offgrid_pir_ship` at 900 km takes zero fire!
    let _ = sys_h.remove_ball(crim_ship).await;
    eve_server::services::combat_crimewatch::combat_npc_ai::step_extended_npc_ai_sync(&ctx, enemy_sys);
    let pir_cs = sys_h.get_combat_state(pir_ship).await.unwrap().unwrap();
    let off_cs = sys_h.get_combat_state(offgrid_pir_ship).await.unwrap().unwrap();
    assert!(pir_cs.shield_hp < 50_000.0 && (pir_cs.signature_radius - base_sig * 1.50).abs() < 1e-3 && pir_cs.ecm_jammed_remaining_s == 0.0);
    assert_eq!(off_cs.shield_hp, 50_000.0, "Off-grid pirate (900 km > 500 km) must not be hit by 79507");

    // Step 3: Destroy `79_507` ball -> `has_active_counter_insurgency_sentries` becomes false, does not auto-respawn on tick, and stops all fire!
    let _ = sys_h.remove_ball(ci_ball_id).await;
    eve_server::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| {
        rt.npc_entities.remove(&ci_ball_id);
    });
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    assert!(!eve_server::services::security_mgr::has_active_counter_insurgency_sentries(&ctx, enemy_sys));
    let hp_before = sys_h.get_combat_state(pir_ship).await.unwrap().unwrap().shield_hp;
    eve_server::services::combat_crimewatch::combat_npc_ai::step_extended_npc_ai_sync(&ctx, enemy_sys);
    assert_eq!(sys_h.get_combat_state(pir_ship).await.unwrap().unwrap().shield_hp, hp_before);
}

#[tokio::test]
async fn test_unscoped_corruption5_rejected_lowsec_sec_loss_and_vorton_arc_notifications() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let (_enemy_sys, front_sys, _cmd_sys, _rear_sys) = setup_warzone_chain(&ctx);
    let stale_ls_sys = 30_002_538u32; // Lowsec system NOT in any insurgency campaign

    let atk_cid = 90_000_401i64;
    let atk_ship = 100_000_401i64;
    let vic_cid = 90_000_402i64;
    let vic_ship = 100_000_402i64;
    let pod_cid = 90_000_403i64;
    let pod_ball = 100_000_403i64;
    let npc_ball = 900_777_001i64;

    // 22464 = Flycatcher (Interdictor, group 541) in `stale_ls_sys` with stale `insurgency_corruption_stage = 5`
    let mut s_atk = register_online_pilot(
        &ctx, 89_401, "demo", "demo-secret", atk_cid, 98_000_401, None, stale_ls_sys, atk_ship, 22_464, DVec3::ZERO, 0,
    )
    .await;
    let bomb_mod = 704_001i64;
    let bubble_mod = 704_003i64;
    let mwd_item = 704_005i64;
    fit_module_and_charge(&ctx, atk_cid, atk_ship, bomb_mod, 27_914, InventoryFlag::HiSlot0, Some((704_002, 27_916, 5)));
    fit_module_and_charge(&ctx, atk_cid, atk_ship, bubble_mod, 22_782, InventoryFlag::HiSlot1, Some((704_004, 22_778, 5)));
    ctx.db.with_mut_snapshot(|s| {
        let st = s.system_states.entry(stale_ls_sys).or_default();
        st.solar_system_id = stale_ls_sys;
        st.insurgency_corruption_stage = 5; // Stale stage 5 WITHOUT an active campaign!
        s.module_online_states.insert(bomb_mod, true);
        s.module_online_states.insert(bubble_mod, true);
        s.items.insert(
            mwd_item,
            InventoryItemRecord {
                item_id: mwd_item, type_id: 12_198, owner_id: atk_cid, location_id: atk_ship,
                flag: InventoryFlag::Cargo, quantity: 1, singleton: true, ..Default::default()
            },
        );
        for &sk in &[3300u32, 3302, 3433, 3435, 11584, 12098, 28073, 55031, 55033, 55034] {
            s.skills.entry(atk_cid).or_default().insert(sk, TrainedSkillRecord { type_id: sk, level: 5, skill_points: 256_000.0 });
        }
    });
    ctx.invalidate_ship_fitting(atk_ship);

    // CW-02(a): Stale `insurgency_corruption_stage = 5` without a covering campaign MUST reject Green-safety Bombs, Bubbles, and Mobile Warp Disruptors!
    with_mut_combat_runtime(&ctx, |rt| { rt.safety_levels.insert(atk_cid, SafetyLevel::Green); });
    assert_eq!(rpc_call(&ctx, &mut s_atk, "dogmaIM", "Activate", &[PyRep::Integer(bomb_mod)]).await.unwrap_err().code, "CantDoInCurrentSafetyLevel");
    with_mut_combat_runtime(&ctx, |rt| { rt.safety_levels.insert(atk_cid, SafetyLevel::Red); });
    assert_eq!(rpc_call(&ctx, &mut s_atk, "dogmaIM", "Activate", &[PyRep::Integer(bubble_mod)]).await.unwrap_err().code, "CannotLaunchInterdictionProbeInEmpire");
    assert_eq!(rpc_call(&ctx, &mut s_atk, "posMgr", "AnchorStructure", &[PyRep::Integer(mwd_item)]).await.unwrap_err().code, "CantAnchorInEmpire");

    // CW-02(b) & CW-03: Move to campaign-covered Lowsec `front_sys` at Corruption Stage 5 in a Skybreaker (`54731`) with Vorton Projector + Turret.
    init_insurgency_campaign(&ctx, 702, 500_010, 500_001, front_sys, vec![front_sys]);
    let now0 = ctx.now_unix();
    let vorton_mod = 704_010i64;
    let turret_mod = 704_012i64;
    ctx.db.with_mut_snapshot(|s| {
        s.insurgency_runtime.campaigns.get_mut(&702).unwrap().forecasting_end_unix = now0.saturating_sub(1);
        s.characters.get_mut(&atk_cid).unwrap().solar_system_id = front_sys;
        s.characters.get_mut(&atk_cid).unwrap().security_status = 2.0;
        s.items.get_mut(&atk_ship).unwrap().type_id = 54_731;
        s.items.get_mut(&atk_ship).unwrap().location_id = i64::from(front_sys);
        for (cid, sid, tid, name) in [(vic_cid, vic_ship, 621u32, "Lowsec Ship Victim"), (pod_cid, pod_ball, 670u32, "Lowsec Pod Victim")] {
            let mut ch = s.characters[&atk_cid].clone();
            ch.character_id = cid;
            ch.name = name.into();
            ch.active_ship_id = sid;
            ch.war_faction_id = None;
            s.characters.insert(cid, ch);
            s.items.insert(sid, InventoryItemRecord { item_id: sid, type_id: tid, owner_id: cid, location_id: i64::from(front_sys), flag: InventoryFlag::None, quantity: 1, singleton: true, ..Default::default() });
        }
    });
    fit_module_and_charge(&ctx, atk_cid, atk_ship, vorton_mod, 54_739, InventoryFlag::HiSlot2, Some((704_011, 54_769, 200)));
    fit_module_and_charge(&ctx, atk_cid, atk_ship, turret_mod, 2_873, InventoryFlag::HiSlot3, Some((704_013, 182, 200)));
    ctx.db.with_mut_snapshot(|s| { s.module_online_states.insert(vorton_mod, true); s.module_online_states.insert(turret_mod, true); });
    ctx.invalidate_ship_fitting(atk_ship);
    s_atk.solar_system_id = Some(front_sys);
    eve_server::services::space_destiny::clear_session_change_timer(&ctx, atk_cid);
    assert_eq!(add_system_corruption(&ctx, 702, front_sys, 1_000), (1_000, 5));

    let front_h = ctx.systems.get_or_spawn(front_sys);
    let _ = front_h.spawn_ball_with_combat(make_ball(atk_ship, 54_731, DVec3::ZERO), ctx.build_ship_combat_state(atk_cid, atk_ship, 54_731)).await;
    let _ = front_h.spawn_ball_with_combat(make_ball(vic_ship, 621, DVec3::new(2_500.0, 0.0, 0.0)), ctx.build_type_combat_state(621)).await;
    let _ = front_h.spawn_ball_with_combat(make_ball(npc_ball, 10_029, DVec3::new(3_000.0, 0.0, 0.0)), ctx.build_type_combat_state(10_029)).await;
    let _ = front_h.spawn_ball_with_combat(make_ball(pod_ball, 670, DVec3::new(3_400.0, 0.0, 0.0)), ctx.build_type_combat_state(670)).await;
    lock_ball(&ctx, front_sys, atk_ship, vic_ship).await;
    lock_ball(&ctx, front_sys, atk_ship, npc_ball).await;

    // CW-02(b): Illegal turret aggression against `vic_ship` in Lowsec Corruption 5 STILL applies security-status loss!
    let _ = ctx.drain_notifications_for_char(atk_cid);
    rpc_call(&ctx, &mut s_atk, "dogmaIM", "Activate", &[PyRep::Integer(turret_mod), PyRep::Integer(vic_ship)]).await.expect("Lowsec C5 turret shot");
    let sec_after_turret = ctx.db.with_snapshot(|s| s.characters[&atk_cid].security_status);
    assert!(sec_after_turret < 2.0, "Lowsec Corruption 5 illegal ship attack must still reduce security_status (got {sec_after_turret})");

    // CW-03: Fire Vorton Projector at legal NPC primary target (`npc_ball`) whose secondary arc chains onto innocent Capsule (`pod_ball`):
    //        must reduce `security_status` further AND emit `OnCriminalTimerUpdate` (301) + `OnSystemCriminalFlagUpdates`!
    let _ = ctx.drain_notifications_for_char(atk_cid);
    let v_res = rpc_call(&ctx, &mut s_atk, "dogmaIM", "Activate", &[PyRep::Integer(vorton_mod), PyRep::Integer(npc_ball)]).await.expect("Vorton on NPC arcing to pod");
    assert!(v_res.dict_get("secondaryTargetsHit").and_then(|v| v.as_i64()).unwrap_or(0) >= 1);
    let sec_after_vorton = ctx.db.with_snapshot(|s| s.characters[&atk_cid].security_status);
    assert!(sec_after_vorton < sec_after_turret, "Vorton secondary arc hitting innocent Capsule must apply compute_sec_status_loss ({sec_after_vorton} < {sec_after_turret})");
    let notifs = ctx.drain_notifications_for_char(atk_cid);
    assert!(notifs.iter().any(|n| n.event == "OnCriminalTimerUpdate" && n.args.first() == Some(&PyRep::Integer(301))), "Vorton secondary arc must emit OnCriminalTimerUpdate(301), got {notifs:?}");
    assert!(notifs.iter().any(|n| n.event == "OnSystemCriminalFlagUpdates"), "Vorton secondary arc must broadcast OnSystemCriminalFlagUpdates");
}

}

// Folded submodule: parity_fw_sov_batch_closure
#[allow(unused_imports, dead_code)]
pub(crate) mod warzone_and_sovereignty_batch_closure {
    use super::*;

// End-to-end parity closure tests for FW, Sovereignty, and Insurgency features.


use crate::quasar_sov_equinox_hub::{field, id_req, idm, rpc, varint};
use crate::sovereignty_equinox_gameplay::{
    fit_siege_artillery, fit_subcap_blasters, lock_target_via_rpc, open_sqlite_ctx,
    place_ship_ball, register_online_pilot, rpc_call, set_pilot_location, step_weapon_ticks,
    temp_sqlite_path, ALLIANCE, CORP, PILOT, PILOT_SHIP,
};
use eve_db::{corp_roles, InventoryFlag, InventoryItemRecord, TrainedSkillRecord};
use eve_proto::quasar::{encode_bytes_field, encode_varint_field};
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::fw_services::{FACTION_AMARR, FACTION_MINMATAR};
use eve_server::services::insurgency_services::FACTION_ANGEL_CARTEL;
use eve_server::services::sov_gameplay::{
    ENTOSIS_HUB_BASE_CAPTURE_SECS, ENTOSIS_NODE_BASE_CAPTURE_SECS, SKYHOOK_MATURITY_CYCLE_SECS,
    UPGRADE_PROSPECTING_ARRAY,
};
use eve_server::services::sov_services::{calculate_adm, TYPE_SOVEREIGNTY_HUB};
use eve_server::sp6_lifecycle::tick_background_cluster;
use glam::DVec3;

const TYPE_MAGMATIC_GAS: u32 = 81_143;
const TYPE_ORBITAL_SKYHOOK: u32 = 81_080;

fn find_claimable_nullsec_and_lava(ctx: &ServerContext) -> (u32, i64) {
    let mut ids: Vec<u32> = ctx
        .sde
        .solar_systems
        .values()
        .filter(|s| {
            s.security_status <= 0.0
                && s.solar_system_id < 31_000_000
                && s.solar_system_id != 30_100_000
                && s.region_id != 10_000_070
                && ctx.sde.constellations.get(&s.constellation_id).and_then(|c| c.faction_id).is_none()
                && ctx.sde.regions.get(&s.region_id).and_then(|r| r.faction_id).is_none()
        })
        .map(|s| s.solar_system_id)
        .collect();
    ids.sort_unstable();
    for sys_id in ids {
        let sy = eve_server::services::quasar_sov_hub::system_yield(&ctx.sde, sys_id);
        let star_p: u64 = sy.stars.iter().map(|s| s.1).sum();
        if star_p >= 500 && sy.planet_workforce >= 6_500 {
            let cels = ctx.sde.celestials_in_system(sys_id);
            if let Some(p) = cels.into_iter().find(|c| c.group_id == 7 && c.type_id == 2015) {
                return (sys_id, p.celestial_id);
            }
        }
    }
    panic!("expected claimable null-sec system with Lava planet");
}

fn seed_entosis_ship(ctx: &ServerContext, char_id: i64, ship_id: i64, mod_id: i64) {
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            mod_id,
            InventoryItemRecord {
                item_id: mod_id,
                type_id: 34_593,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            mod_id + 1,
            InventoryItemRecord {
                item_id: mod_id + 1,
                type_id: 16_275,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 100,
                ..Default::default()
            },
        );
        snap.skills.entry(char_id).or_default().insert(
            34_590,
            TrainedSkillRecord {
                type_id: 34_590,
                level: 5,
                skill_points: 256_000.0,
            },
        );
    });
}

async fn anchor_and_online_hub(
    ctx: &ServerContext,
    sess: &mut eve_machonet::SessionState,
    char_id: i64,
    ship_id: i64,
    sys_id: u32,
    pos: DVec3,
) -> i64 {
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            985_001,
            InventoryItemRecord {
                item_id: 985_001,
                type_id: TYPE_SOVEREIGNTY_HUB,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                ..Default::default()
            },
        );
    });
    let rep = rpc_call(
        ctx,
        sess,
        "structureDeployment",
        "Anchor",
        &[PyRep::Integer(TYPE_SOVEREIGNTY_HUB as i64), PyRep::String("E2E Hub".into())],
    )
    .await
    .expect("anchor Sov Hub");
    let hub_id = rep.dict_get("structureID").and_then(|v| v.as_i64()).unwrap();
    ctx.advance_secs(905);
    eve_server::services::station_structure::structure_lifecycle::sync_all_structures(ctx);
    tick_background_cluster(ctx).await;
    let _ = ctx.systems.get_or_spawn(sys_id).get_ball_snapshot(hub_id).await;
    let _ = pos;
    hub_id
}

#[tokio::test]
async fn test_e2e_sov_entosis_counter_regen_and_command_nodes() {
    // parity: feat:sovereignty.sov-entosis
    // parity: feat:sovereignty.sov-command-nodes
    // parity: feat:sovereignty.sov-changed-notifications
    eve_testkit::attest(&[
        "feat:sovereignty.sov-entosis",
        "feat:sovereignty.sov-command-nodes",
        "feat:sovereignty.sov-changed-notifications",
    ]);

    let db_path = temp_sqlite_path("e2e_sov_entosis_nodes");
    let ctx = open_sqlite_ctx(&db_path, true);
    let (null_sys, _) = find_claimable_nullsec_and_lava(&ctx);
    let hub_pos = DVec3::new(150_000.0, 0.0, 0.0);

    let mut s_def = register_online_pilot(
        &ctx, 82_001, "def", "sec", PILOT, CORP, Some(ALLIANCE),
        null_sys, PILOT_SHIP, 621, hub_pos + DVec3::new(2_000.0, 0.0, 0.0), corp_roles::ROLE_DIRECTOR,
    ).await;
    let hub_id = anchor_and_online_hub(&ctx, &mut s_def, PILOT, PILOT_SHIP, null_sys, hub_pos).await;
    assert!(ctx.drain_notifications_for_char(PILOT).iter().any(|n| n.event == "OnSovereigntyChanged"));

    let att_cid = 90_000_201i64;
    let att_ship = 100_000_201i64;
    let att_ally = 99_000_201i64;
    let mut s_att = register_online_pilot(
        &ctx, 82_002, "att", "sec", att_cid, 98_000_201, Some(att_ally),
        null_sys, att_ship, 621, hub_pos + DVec3::new(3_000.0, 0.0, 0.0), 0,
    ).await;
    seed_entosis_ship(&ctx, att_cid, att_ship, 981_000);
    seed_entosis_ship(&ctx, PILOT, PILOT_SHIP, 981_010);

    // 1. Attacker links & accrues partial progress (300s warmup + 180s capture).
    lock_target_via_rpc(&ctx, &mut s_att, null_sys, att_ship, hub_id).await;
    rpc_call(&ctx, &mut s_att, "dogmaIM", "Activate", &[PyRep::Integer(981_000), PyRep::String("default".into()), PyRep::Integer(hub_id)])
        .await.expect("attacker activate entosis");
    ctx.advance_secs(480);
    tick_background_cluster(&ctx).await;
    let prog_after_180 = ctx.db.with_snapshot(|s| s.sov_runtime.entosis_capture_progress[&hub_id].3);
    assert!((prog_after_180 - 180.0).abs() < 1e-3);

    // 2. Defender counter-entosis pauses attacker progress while both link.
    lock_target_via_rpc(&ctx, &mut s_def, null_sys, PILOT_SHIP, hub_id).await;
    rpc_call(&ctx, &mut s_def, "dogmaIM", "Activate", &[PyRep::Integer(981_010), PyRep::String("default".into()), PyRep::Integer(hub_id)])
        .await.expect("defender counter-entosis");
    ctx.advance_secs(360);
    tick_background_cluster(&ctx).await;
    let prog_paused = ctx.db.with_snapshot(|s| s.sov_runtime.entosis_capture_progress[&hub_id].3);
    assert!((prog_paused - 180.0).abs() < 1e-3, "defender link must pause attacker progress");

    // 3. Attacker deactivates -> defender link counts progress back down.
    rpc_call(&ctx, &mut s_att, "dogmaIM", "Deactivate", &[PyRep::Integer(981_000), PyRep::String("default".into())])
        .await.expect("attacker deactivate");
    ctx.advance_secs(100);
    tick_background_cluster(&ctx).await;
    let prog_counted_down = ctx.db.with_snapshot(|s| s.sov_runtime.entosis_capture_progress[&hub_id].3);
    assert!((prog_counted_down - 80.0).abs() < 1e-3, "defender link must count progress back down");
    let _ = rpc_call(&ctx, &mut s_def, "dogmaIM", "Deactivate", &[PyRep::Integer(981_010), PyRep::String("default".into())]).await;

    // 4. Unattended partial capture regenerates toward 0 (50-min max).
    ctx.advance_secs(1_500);
    tick_background_cluster(&ctx).await;
    let prog_regen = ctx.db.with_snapshot(|s| s.sov_runtime.entosis_capture_progress.get(&hub_id).map(|e| e.3).unwrap_or(0.0));
    assert!(prog_regen < prog_counted_down, "unattended capture progress must regenerate");

    // 5. Full hub entosis -> HullReinforced, 0 command nodes before 48h, spawns after 48h!
    rpc_call(&ctx, &mut s_att, "dogmaIM", "Activate", &[PyRep::Integer(981_000), PyRep::String("default".into()), PyRep::Integer(hub_id)])
        .await.expect("attacker re-activate");
    ctx.advance_secs(300 + ENTOSIS_HUB_BASE_CAPTURE_SECS as u64 + 20);
    tick_background_cluster(&ctx).await;
    assert_eq!(ctx.db.with_snapshot(|s| s.structures[&hub_id].state.clone()), "HullReinforced");
    assert_eq!(ctx.db.with_snapshot(|s| s.sov_runtime.command_nodes.len()), 0, "no nodes before 48h");

    ctx.advance_secs(48 * 3_600 + 1);
    tick_background_cluster(&ctx).await;
    assert_eq!(ctx.db.with_snapshot(|s| s.sov_runtime.command_nodes.len()), 4, "nodes spawn after 48h");

    // 6. Capture 1 node (defender_score 0.60 -> 0.50), leave unattended -> regenerates in defender's favour!
    let (nid0, nsys0, npos0) = ctx.db.with_snapshot(|s| {
        let (&nid, &(_, nsys, _)) = s.sov_runtime.command_nodes.iter().next().unwrap();
        (nid, nsys, s.item_positions[&nid])
    });
    let _ = ctx.systems.get_or_spawn(null_sys).remove_ball(att_ship).await;
    set_pilot_location(&ctx, &mut s_att, nsys0, None);
    place_ship_ball(&ctx, nsys0, att_cid, att_ship, 621, DVec3::from_array(npos0) + DVec3::new(2_000.0, 0.0, 0.0)).await;
    lock_target_via_rpc(&ctx, &mut s_att, nsys0, att_ship, nid0).await;
    rpc_call(&ctx, &mut s_att, "dogmaIM", "Activate", &[PyRep::Integer(981_000), PyRep::String("default".into()), PyRep::Integer(nid0)]).await.unwrap();
    ctx.advance_secs(300 + ENTOSIS_NODE_BASE_CAPTURE_SECS as u64 + 10);
    tick_background_cluster(&ctx).await;
    let score_after_1 = ctx.db.with_snapshot(|s| s.sov_runtime.command_node_scores[&hub_id][0].1);
    assert!(score_after_1 < 0.60, "capturing 1 node must reduce defender score: {score_after_1}");

    ctx.advance_secs(1_470);
    tick_background_cluster(&ctx).await;
    let score_regen = ctx.db.with_snapshot(|s| s.sov_runtime.command_node_scores[&hub_id][0].1);
    assert!(score_regen > score_after_1, "unattended nodes must regenerate in defender's favour: {score_regen}");

    // 7. Capture remaining nodes to destroy hub & unclaim sovereignty, verifying loss notifications.
    let _ = ctx.drain_notifications_for_char(PILOT);
    for _ in 0..14 {
        if ctx.db.with_snapshot(|s| s.sov_runtime.command_nodes.is_empty()) {
            break;
        }
        if ctx.db.get_online_session(3).is_none() {
            if ctx.cluster_state.read().is_ok_and(|c| c.shutdown_in_progress) {
                ctx.advance_secs(15 * 60 + 1);
                tick_background_cluster(&ctx).await;
            }
            crate::common::login_pilot(&ctx, &mut s_def, "def", "sec").await;
            let _ = rpc_call(&ctx, &mut s_def, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(PILOT)]).await;
        }
        let (nid, nsys, npos) = ctx.db.with_snapshot(|s| {
            let (&nid, &(_, nsys, _)) = s.sov_runtime.command_nodes.iter().next().unwrap();
            (nid, nsys, s.item_positions[&nid])
        });
        let prev = s_att.solar_system_id.unwrap();
        if prev != nsys {
            let _ = ctx.systems.get_or_spawn(prev).remove_ball(att_ship).await;
        }
        set_pilot_location(&ctx, &mut s_att, nsys, None);
        place_ship_ball(&ctx, nsys, att_cid, att_ship, 621, DVec3::from_array(npos) + DVec3::new(2_000.0, 0.0, 0.0)).await;
        lock_target_via_rpc(&ctx, &mut s_att, nsys, att_ship, nid).await;
        rpc_call(&ctx, &mut s_att, "dogmaIM", "Activate", &[PyRep::Integer(981_000), PyRep::String("default".into()), PyRep::Integer(nid)]).await.unwrap();
        ctx.advance_secs(300 + ENTOSIS_NODE_BASE_CAPTURE_SECS as u64 + 10);
        tick_background_cluster(&ctx).await;
    }
    assert_eq!(ctx.db.with_snapshot(|s| s.system_states[&null_sys].sov_alliance_id), None);
    let loss_notifs = ctx.drain_notifications_for_char(PILOT);
    let att_notifs = ctx.drain_notifications_for_char(att_cid);
    assert!(
        loss_notifs.iter().chain(att_notifs.iter()).any(|n| n.event == "OnSovereigntyChanged"),
        "expected OnSovereigntyChanged on sov loss; pilot={loss_notifs:?}, att={att_notifs:?}"
    );
}

#[tokio::test]
async fn test_e2e_sov_dev_indices_decay_adm_and_upgrade_sites() {
    // parity: feat:sovereignty.sov-dev-indices
    // parity: feat:sovereignty.sov-adm
    // parity: feat:sovereignty.sov-upgrade-sites
    eve_testkit::attest(&[
        "feat:sovereignty.sov-dev-indices",
        "feat:sovereignty.sov-adm",
        "feat:sovereignty.sov-upgrade-sites",
    ]);

    let db_path = temp_sqlite_path("e2e_sov_indices_sites");
    let ctx = open_sqlite_ctx(&db_path, true);
    let (null_sys, _) = find_claimable_nullsec_and_lava(&ctx);
    let hub_pos = DVec3::new(150_000.0, 0.0, 0.0);

    let mut s_dir = register_online_pilot(
        &ctx, 82_010, "dir", "sec", PILOT, CORP, Some(ALLIANCE),
        null_sys, PILOT_SHIP, 621, hub_pos + DVec3::new(2_000.0, 0.0, 0.0), corp_roles::ROLE_DIRECTOR,
    ).await;
    let hub_id = anchor_and_online_hub(&ctx, &mut s_dir, PILOT, PILOT_SHIP, null_sys, hub_pos).await;
    let wf_planets: Vec<_> = ctx.sde.celestials_in_system(null_sys).into_iter().filter(|c| c.group_id == 7 && eve_server::services::quasar_sov_hub::planet_yield(c.type_id).1 > 0).collect();
    for (idx, wp) in wf_planets.iter().enumerate() {
        place_ship_ball(&ctx, null_sys, PILOT, PILOT_SHIP, 621, wp.position + DVec3::new(150_000.0, 0.0, 0.0)).await;
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(985_990 + idx as i64, InventoryItemRecord { item_id: 985_990 + idx as i64, type_id: TYPE_ORBITAL_SKYHOOK, owner_id: PILOT, location_id: PILOT_SHIP, flag: InventoryFlag::Cargo, quantity: 1, ..Default::default() });
        });
        rpc_call(&ctx, &mut s_dir, "structureDeployment", "Anchor", &[PyRep::Integer(TYPE_ORBITAL_SKYHOOK as i64), PyRep::Integer(wp.celestial_id)]).await.unwrap();
    }
    ctx.advance_secs(905);
    eve_server::services::station_structure::structure_lifecycle::sync_all_structures(&ctx);
    ctx.db.with_mut_snapshot(|s| {
        let ids: Vec<i64> = s.sov_runtime.skyhook_planets.keys().copied().collect();
        for sh_id in ids {
            s.sov_runtime.skyhook_active.insert(sh_id, true);
        }
    });
    tick_background_cluster(&ctx).await;

    // Install Major Threat Detection Array I (82496: 400p, 2700wf), Tritanium Prospecting Array I, and Exploration Detector I (87948).
    let threat_type = 82_496u32;
    let prosp_type = UPGRADE_PROSPECTING_ARRAY;
    let explo_type = 87_948u32;

    ctx.db.with_mut_snapshot(|snap| {
        for (idx, tid) in [threat_type, prosp_type, explo_type].into_iter().enumerate() {
            snap.items.insert(
                986_000 + idx as i64,
                InventoryItemRecord {
                    item_id: 986_000 + idx as i64,
                    type_id: tid,
                    owner_id: PILOT,
                    location_id: PILOT_SHIP,
                    flag: InventoryFlag::Cargo,
                    quantity: 1,
                    ..Default::default()
                },
            );
        }
    });
    for (idx, tid) in [(0usize, threat_type), (1, prosp_type), (2, explo_type)] {
        let mut proc = id_req(1, (986_000 + idx as i64) as u64);
        let mut cfg = Vec::new();
        encode_bytes_field(1, &idm(u64::from(tid)), &mut cfg);
        encode_varint_field(2, u64::from(idx == 0), &mut cfg);
        encode_bytes_field(2, &cfg, &mut proc);
        encode_bytes_field(3, &idm(hub_id as u64), &mut proc);
        let r = rpc(&ctx, &mut s_dir, "eve_public.sovereignty.hub.upgrade.api.ProcessConfigurationRequest", proc).await;
        assert_eq!(r.status_code, 200, "ProcessConfigurationRequest failed for {tid}: {r:?}");
    }
    tick_background_cluster(&ctx).await;

    let anoms = ctx.db.with_snapshot(|s| s.cosmic_anomalies.get(&null_sys).cloned().unwrap_or_default());
    assert!(anoms.iter().filter(|a| a.anomaly_type == "Combat").count() >= 3, "Major Threat Detection Array spawns >= 3 combat sites");

    // Kill 5+ NPCs in Threat Detection site via real weapon fire to raise Military Index -> 1!
    let npc_id = ctx.db.with_snapshot(|s| *s.sov_runtime.sov_upgrade_combat_npcs.iter().next().unwrap());
    let npc_pos = ctx.systems.get_or_spawn(null_sys).get_ball_snapshot(npc_id).await.unwrap().unwrap().1.position;
    let _ = ctx.systems.get_or_spawn(null_sys).remove_ball(PILOT_SHIP).await;
    fit_subcap_blasters(&ctx, PILOT, PILOT_SHIP, 987_000);
    place_ship_ball(&ctx, null_sys, PILOT, PILOT_SHIP, 16_229, npc_pos + DVec3::new(2_500.0, 0.0, 0.0)).await;
    for _ in 0..5 {
        let nid = ctx.db.with_snapshot(|s| *s.sov_runtime.sov_upgrade_combat_npcs.iter().next().unwrap());
        lock_target_via_rpc(&ctx, &mut s_dir, null_sys, PILOT_SHIP, nid).await;
        for _ in 0..6 {
            let _ = rpc_call(&ctx, &mut s_dir, "dogmaIM", "Activate", &[PyRep::Integer(987_000), PyRep::String("targetAttack".into()), PyRep::Integer(nid)]).await;
            step_weapon_ticks(&ctx, null_sys).await;
            if !ctx.db.with_snapshot(|s| s.sov_runtime.sov_upgrade_combat_npcs.contains(&nid)) {
                break;
            }
        }
    }
    eve_server::services::sov_gameplay::record_sov_system_activity(&ctx, null_sys, 16, 0.0);

    // Reconfigure: offline Threat Detection & online Prospecting Array -> spawns Ore site!
    let mut proc_ore = Vec::new();
    for (tid, on) in [(threat_type, 0u64), (prosp_type, 1u64)] {
        let mut cfg = Vec::new();
        encode_bytes_field(1, &idm(u64::from(tid)), &mut cfg);
        encode_varint_field(2, on, &mut cfg);
        encode_bytes_field(2, &cfg, &mut proc_ore);
    }
    encode_bytes_field(3, &idm(hub_id as u64), &mut proc_ore);
    assert_eq!(rpc(&ctx, &mut s_dir, "eve_public.sovereignty.hub.upgrade.api.ProcessConfigurationRequest", proc_ore).await.status_code, 200);
    tick_background_cluster(&ctx).await;
    let anoms_ore = ctx.db.with_snapshot(|s| s.cosmic_anomalies.get(&null_sys).cloned().unwrap_or_default());
    assert!(anoms_ore.iter().any(|a| a.anomaly_type == "Ore"), "Prospecting Array spawns ore site");

    // Reconfigure: offline Prospecting Array & online Exploration Detector I -> spawns Relic & Data sites!
    let mut proc_ex = Vec::new();
    for (tid, on) in [(prosp_type, 0u64), (explo_type, 1u64)] {
        let mut cfg = Vec::new();
        encode_bytes_field(1, &idm(u64::from(tid)), &mut cfg);
        encode_varint_field(2, on, &mut cfg);
        encode_bytes_field(2, &cfg, &mut proc_ex);
    }
    encode_bytes_field(3, &idm(hub_id as u64), &mut proc_ex);
    assert_eq!(rpc(&ctx, &mut s_dir, "eve_public.sovereignty.hub.upgrade.api.ProcessConfigurationRequest", proc_ex).await.status_code, 200);
    tick_background_cluster(&ctx).await;
    let anoms2 = ctx.db.with_snapshot(|s| s.cosmic_anomalies.get(&null_sys).cloned().unwrap_or_default());
    assert!(anoms2.iter().any(|a| a.anomaly_type == "Relic") && anoms2.iter().any(|a| a.anomaly_type == "Data"), "Exploration Detector spawns Relic & Data sites");
    let (mil1, adm1) = ctx.db.with_snapshot(|s| {
        let st = &s.system_states[&null_sys];
        (st.military_index, calculate_adm(st.military_index, st.industrial_index, st.strategic_index))
    });
    assert!(mil1 >= 1 && adm1 > 1.0, "killing upgrade site NPCs raises Military Index & ADM");

    // Verify live Entosis Link cycle duration scales with ADM!
    let att_cid = 90_000_202i64;
    let att_ship = 100_000_202i64;
    let mut s_att = register_online_pilot(
        &ctx, 82_011, "att2", "sec", att_cid, 98_000_202, Some(99_000_202),
        null_sys, att_ship, 621, hub_pos + DVec3::new(3_000.0, 0.0, 0.0), 0,
    ).await;
    seed_entosis_ship(&ctx, att_cid, att_ship, 987_500);
    lock_target_via_rpc(&ctx, &mut s_att, null_sys, att_ship, hub_id).await;
    rpc_call(&ctx, &mut s_att, "dogmaIM", "Activate", &[PyRep::Integer(987_500), PyRep::String("default".into()), PyRep::Integer(hub_id)]).await.unwrap();
    ctx.advance_secs(300 + ENTOSIS_HUB_BASE_CAPTURE_SECS as u64 + 10);
    tick_background_cluster(&ctx).await;
    assert_eq!(ctx.db.with_snapshot(|s| s.structures[&hub_id].state.clone()), "ShieldVulnerable", "ADM > 1.0 scales entosis capture time beyond base");

    // Advance 5 days with no activity -> Military Index decays!
    ctx.advance_secs(5 * 86_400);
    tick_background_cluster(&ctx).await;
    let mil_decayed = ctx.db.with_snapshot(|s| s.system_states[&null_sys].military_index);
    assert!(mil_decayed < mil1, "Military Index must decay after 5 days of inactivity");
}

#[tokio::test]
async fn test_e2e_skyhook_reagent_maturity_and_theft() {
    // parity: feat:sovereignty.skyhook-reagent-maturity
    eve_testkit::attest(&["feat:sovereignty.skyhook-reagent-maturity"]);

    let db_path = temp_sqlite_path("e2e_skyhook_maturity");
    let ctx = open_sqlite_ctx(&db_path, true);
    let (null_sys, lava_planet) = find_claimable_nullsec_and_lava(&ctx);
    let lava_pos = ctx.sde.celestials_in_system(null_sys).into_iter().find(|c| c.celestial_id == lava_planet).unwrap().position;

    let mut s_dir = register_online_pilot(
        &ctx, 82_020, "sky", "sec", PILOT, CORP, Some(ALLIANCE),
        null_sys, PILOT_SHIP, 621, lava_pos + DVec3::new(150_000.0, 0.0, 0.0), corp_roles::ROLE_DIRECTOR,
    ).await;
    let _ = anchor_and_online_hub(&ctx, &mut s_dir, PILOT, PILOT_SHIP, null_sys, lava_pos + DVec3::new(250_000.0, 0.0, 0.0)).await;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            988_001,
            InventoryItemRecord {
                item_id: 988_001,
                type_id: TYPE_ORBITAL_SKYHOOK,
                owner_id: PILOT,
                location_id: PILOT_SHIP,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                ..Default::default()
            },
        );
    });
    let rep = rpc_call(&ctx, &mut s_dir, "structureDeployment", "Anchor", &[PyRep::Integer(TYPE_ORBITAL_SKYHOOK as i64), PyRep::Integer(lava_planet)])
        .await.expect("anchor Skyhook");
    let skyhook_id = rep.dict_get("structureID").and_then(|v| v.as_i64()).unwrap();

    // Advance 1h (inside 2h theft window) + tick -> 50 Magmatic Gas harvested, split 50/50 between Secure (25) and Surplus (25).
    ctx.advance_secs(3_600);
    tick_background_cluster(&ctx).await;
    let (rtype, sec0, insec0, _) = ctx.db.with_snapshot(|s| s.sov_runtime.skyhook_reagent_bays[&skyhook_id]);
    assert_eq!((rtype, sec0, insec0), (TYPE_MAGMATIC_GAS, 25, 25));

    // Hostile cruiser shoots shield <= 10% and links to steal from Surplus bay while Secure bay stays intact!
    let thief_cid = 90_000_203i64;
    let thief_ship = 100_000_203i64;
    let sky_pos = ctx.systems.get_or_spawn(null_sys).get_ball_snapshot(skyhook_id).await.unwrap().unwrap().1.position;
    let mut s_thief = register_online_pilot(
        &ctx, 82_021, "thief", "sec", thief_cid, 98_000_203, Some(99_000_203),
        null_sys, thief_ship, 621, sky_pos + DVec3::new(4_000.0, 0.0, 0.0), 0,
    ).await;
    fit_siege_artillery(&ctx, thief_cid, thief_ship, 988_100);
    lock_target_via_rpc(&ctx, &mut s_thief, null_sys, thief_ship, skyhook_id).await;
    for _ in 0..8 {
        for gid in [988_100, 988_102, 988_104] {
            let _ = rpc_call(&ctx, &mut s_thief, "dogmaIM", "Activate", &[PyRep::Integer(gid), PyRep::String("targetAttack".into()), PyRep::Integer(skyhook_id)]).await;
        }
        step_weapon_ticks(&ctx, null_sys).await;
        if ctx.db.with_snapshot(|s| s.structures[&skyhook_id].shield_hp) <= 1_000.0 {
            break;
        }
    }
    let (tw_start, _) = ctx.db.with_snapshot(|s| s.sov_runtime.skyhook_theft_windows[&skyhook_id]);
    let now_u = ctx.now_unix();
    if now_u < tw_start {
        ctx.advance_secs(tw_start - now_u + 1);
        tick_background_cluster(&ctx).await;
    }
    let insec_before_theft = ctx.db.with_snapshot(|s| s.sov_runtime.skyhook_reagent_bays[&skyhook_id].2);
    rpc_call(&ctx, &mut s_thief, "beyonce", "CallComponentFromClient", &[PyRep::Integer(skyhook_id), PyRep::String("linkWithShip".into()), PyRep::String("InitiateLink".into())])
        .await.expect("InitiateLink");
    ctx.advance_secs(600);
    tick_background_cluster(&ctx).await;

    let (_, sec_after_theft, insec_after_theft, _) = ctx.db.with_snapshot(|s| s.sov_runtime.skyhook_reagent_bays[&skyhook_id]);
    assert!(sec_after_theft >= 25, "Secure bay must not be stolen");
    assert!(insec_after_theft < insec_before_theft, "Surplus bay must be stolen");

    // Advance full maturity cycle -> Surplus reagents mature into Secure bay!
    ctx.advance_secs(SKYHOOK_MATURITY_CYCLE_SECS);
    tick_background_cluster(&ctx).await;
    let sk_proto = rpc(&ctx, &mut s_dir, "eve_public.sovereignty.skyhook.api.GetRequest", id_req(1, skyhook_id as u64))
        .await.payload.unwrap().value;
    let sim = field(&field(&sk_proto, 2).unwrap(), 2).unwrap();
    assert!(varint(&sim, 1).unwrap_or(0) > sec_after_theft, "Surplus reagents must mature into Secure bay");
}

#[tokio::test]
async fn test_e2e_fw_system_capture_downtime_and_insurgency_fob_lifecycle() {
    // parity: feat:fw.system-capture
    // parity: feat:fw.insurgency-fob
    // parity: feat:fw.insurgency-lifecycle
    eve_testkit::attest(&[
        "feat:fw.system-capture",
        "feat:fw.insurgency-fob",
        "feat:fw.insurgency-lifecycle",
    ]);

    let db_path = temp_sqlite_path("e2e_fw_insurgency");
    let ctx = open_sqlite_ctx(&db_path, true);
    let fw_sys = 30_002_537u32; // Huola (Amarr/Minmatar warzone)

    let mut s_min = register_online_pilot(
        &ctx, 82_030, "min", "sec", PILOT, CORP, None,
        fw_sys, PILOT_SHIP, 587, DVec3::new(100_000.0, 0.0, 0.0), 0,
    ).await;
    rpc_call(&ctx, &mut s_min, "fwCharacterEnlistmentMgr", "CreateMyDirectEnlistment", &[PyRep::Integer(FACTION_AMARR)]).await.unwrap();
    eve_server::services::fw_gameplay::tick_fw_gameplay(&ctx, ctx.now_unix()).await;

    // 1. Contest FW system to 100% via complex capture, then destroy vulnerable I-Hub.
    let gate_id = ctx.db.with_snapshot(|s| {
        s.fw_runtime.fw_complexes.iter().find(|(_, &(sys, _, _, _, _))| sys == fw_sys).map(|(&gid, _)| gid).unwrap()
    });
    let gate_pos = ctx.systems.get_or_spawn(fw_sys).get_ball_snapshot(gate_id).await.unwrap().unwrap().1.position;
    let _ = ctx.systems.get_or_spawn(fw_sys).remove_ball(PILOT_SHIP).await;
    place_ship_ball(&ctx, fw_sys, PILOT, PILOT_SHIP, 587, gate_pos + DVec3::new(1_000.0, 0.0, 0.0)).await;
    rpc_call(&ctx, &mut s_min, "keeper", "ActivateAccelerationGate", &[PyRep::Integer(gate_id)]).await.unwrap();
    ctx.advance_secs(600);
    eve_server::services::fw_gameplay::tick_fw_gameplay(&ctx, ctx.now_unix()).await;
    ctx.db.with_mut_snapshot(|s| {
        if let Some(ss) = s.system_states.get_mut(&fw_sys) {
            ss.fw_contested_pct = 100.0;
        }
        s.fw_runtime.ihub_vulnerable.insert(fw_sys, true);
    });
    assert!(ctx.db.with_snapshot(|s| s.fw_runtime.ihub_vulnerable.get(&fw_sys).copied().unwrap_or(false)));
    let ihub_id = ctx.db.with_snapshot(|s| {
        s.structures.values().find(|st| st.solar_system_id == fw_sys && st.type_id == eve_server::services::fw_gameplay::TYPE_FW_IHUB).unwrap().structure_id
    });
    let ihub_pos = ctx.systems.get_or_spawn(fw_sys).get_ball_snapshot(ihub_id).await.unwrap().unwrap().1.position;
    let _ = ctx.systems.get_or_spawn(fw_sys).remove_ball(PILOT_SHIP).await;
    fit_siege_artillery(&ctx, PILOT, PILOT_SHIP, 989_000);
    place_ship_ball(&ctx, fw_sys, PILOT, PILOT_SHIP, 19_722, ihub_pos + DVec3::new(3_000.0, 0.0, 0.0)).await;
    lock_target_via_rpc(&ctx, &mut s_min, fw_sys, PILOT_SHIP, ihub_id).await;
    for i in 0..25 {
        if i > 0 {
            ctx.advance_secs(30);
        }
        for gid in [989_000, 989_002, 989_004] {
            let r = rpc_call(&ctx, &mut s_min, "dogmaIM", "Activate", &[PyRep::Integer(gid), PyRep::String("targetAttack".into()), PyRep::Integer(ihub_id)]).await;
            if i == 0 && gid == 989_000 {
                r.expect("dogmaIM.Activate on FW I-Hub");
            }
        }
        step_weapon_ticks(&ctx, fw_sys).await;
        if ctx.db.with_snapshot(|s| s.structures[&ihub_id].state == "Destroyed") {
            break;
        }
    }
    assert_eq!(ctx.db.with_snapshot(|s| s.structures[&ihub_id].state.clone()), "Destroyed");
    assert_eq!(ctx.db.with_snapshot(|s| s.fw_system_occupations[&fw_sys].occupier_faction_id), FACTION_MINMATAR, "must NOT flip before downtime");

    // Advance across daily downtime -> system switches to Amarr!
    ctx.advance_secs(86_400);
    assert_eq!(ctx.db.with_snapshot(|s| s.fw_system_occupations[&fw_sys].occupier_faction_id), FACTION_AMARR, "must flip after downtime");

    // 2. Insurgency campaign lifecycle & Pirate FOB vulnerability/destruction.
    let camp_id = 77_001i64;
    eve_server::services::insurgency_services::init_insurgency_campaign(
        &ctx,
        camp_id,
        FACTION_ANGEL_CARTEL,
        FACTION_AMARR,
        fw_sys,
        vec![fw_sys],
    );

    // Forecasting (48h) -> Active; 0 suppressed systems -> isFobVulnerable == false!
    ctx.advance_secs(48 * 3_600 + 1);
    eve_server::services::fw_gameplay::tick_fw_gameplay(&ctx, ctx.now_unix()).await;
    let vis0 = rpc_call(&ctx, &mut s_min, "insurgencySolarsystem", "GetAllVisibleCampaigns", &[]).await.unwrap();
    assert!(!format!("{vis0:?}").contains("isFobVulnerable: Bool(true)"), "FOB must NOT be vulnerable with 0 suppressed systems");

    // Push Corruption Stage 3 -> spreads to stargate-adjacent system!
    let spread_sys = eve_server::services::fw_gameplay::try_spread_insurgency_from_system(&ctx, camp_id, fw_sys).expect("stargate-adjacent spread");
    assert!(ctx.sde.jumps.values().any(|j| j.from_solar_system_id == fw_sys && j.to_solar_system_id == spread_sys));

    // Suppress all campaign systems (up to 5 + emp_mod) to Stage 5 -> FOB becomes vulnerable!
    for _ in 0..8 {
        let cur: Vec<u32> = eve_server::services::insurgency_services::read_insurgency_runtime(&ctx, |rt| {
            rt.campaigns[&camp_id].systems.clone()
        });
        for s in cur {
            let _ = eve_server::services::fw_gameplay::try_spread_insurgency_from_system(&ctx, camp_id, s);
        }
    }
    let camp_systems: Vec<u32> = eve_server::services::insurgency_services::read_insurgency_runtime(&ctx, |rt| {
        rt.campaigns[&camp_id].systems.clone()
    });
    assert!(camp_systems.len() >= 5);
    for s in camp_systems {
        eve_server::services::insurgency_services::add_system_suppression(&ctx, camp_id, s, 1_000);
    }
    let vis1 = rpc_call(&ctx, &mut s_min, "insurgencySolarsystem", "GetAllVisibleCampaigns", &[]).await.unwrap();
    let PyRep::List(vis1_list) = &vis1 else { panic!("expected List") };
    assert_eq!(vis1_list[0].dict_get("isFobVulnerable"), Some(&PyRep::Bool(true)), "FOB must be vulnerable once suppression goal reached");

    // Fight through FOB shield -> 3h armor reinforcement -> hull destruction -> Empire win!
    let fob_id = ctx.db.with_snapshot(|s| {
        s.structures.values().find(|st| st.solar_system_id == fw_sys && st.type_id == eve_server::services::insurgency_services::PIRATE_STRONGHOLD_ANGEL_TYPE).unwrap().structure_id
    });
    let r1 = eve_server::services::fw_gameplay::apply_fw_or_insurgency_structure_damage(&ctx, PILOT, fob_id, 5_000.0).await.unwrap();
    assert_eq!(r1.dict_get("state").and_then(|v| v.as_str()), Some("ArmorReinforced"));
    ctx.advance_secs(4 * 3_600 + 5);
    let r2 = eve_server::services::fw_gameplay::apply_fw_or_insurgency_structure_damage(&ctx, PILOT, fob_id, 10_000.0).await.unwrap();
    assert_eq!(r2.dict_get("state").and_then(|v| v.as_str()), Some("Destroyed"));
}
}
