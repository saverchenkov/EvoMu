//! Unit and integration tests for `pve_special_sites`.

use super::*;
use crate::router::ServerContext;
use eve_db::clock::VirtualServerClock;
use eve_db::{IskCents, WorldRepository};
use eve_machonet::SessionState;
use eve_sde::{SdeRegistry, SolarSystemId};
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

#[cfg(test)]
#[path = "pve_special_sites_ops_edge_tests.rs"]
mod pve_special_sites_ops_edge_tests;

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn find_highsec_system(sde: &SdeRegistry) -> SolarSystemId {
    for &preferred in crate::services::pve_homefront_sites::HOMEFRONT_SYSTEMS {
        if sde.solar_systems.contains_key(&preferred) {
            return preferred;
        }
    }
    let mut cands: Vec<SolarSystemId> = sde
        .solar_systems
        .values()
        .filter(|s| s.security_status >= 0.5 && s.solar_system_id < 31_000_000 && s.region_id != 10_000_070)
        .map(|s| s.solar_system_id)
        .collect();
    cands.sort_unstable();
    cands.into_iter().next().unwrap_or(30_000_142)
}

fn find_pochven_or_lowsec_system(sde: &SdeRegistry) -> SolarSystemId {
    sde.solar_systems
        .values()
        .filter(|s| s.region_id == 10_000_070)
        .min_by_key(|s| s.solar_system_id)
        .map(|s| s.solar_system_id)
        .unwrap_or_else(|| {
            sde.solar_systems
                .values()
                .filter(|s| s.security_status < 0.45 && s.solar_system_id < 31_000_000)
                .min_by_key(|s| s.solar_system_id)
                .map(|s| s.solar_system_id)
                .unwrap_or(30_000_119)
        })
}

fn find_ship_type_id(sde: &SdeRegistry) -> u32 {
    sde.types
        .values()
        .filter(|t| t.category_id == 6 && t.group_id == 25 && t.capacity > 50.0)
        .min_by_key(|t| t.type_id)
        .map(|t| t.type_id)
        .unwrap_or(587)
}

fn make_ctx_and_pilot(sys_id: SolarSystemId, with_ship: bool) -> (ServerContext, SessionState, i64, i64) {
    let sde = shared_sde();
    let db = Arc::new(WorldRepository::new_empty());
    let char_id: i64 = 90_010_001;
    let ship_id: i64 = if with_ship { 1_010_000_001 } else { 0 };
    let ship_tid = find_ship_type_id(&sde);
    db.provision_gm_account("spec_site_pilot", "pw", char_id, "SpecSitePilot");
    db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            if with_ship {
                ch.station_id = None;
                ch.structure_id = None;
            }
            ch.active_ship_id = ship_id;
            ch.balance = IskCents::from_isk(1_000_000);
        }
        if with_ship {
            snap.items.insert(
                ship_id,
                InventoryItemRecord {
                    item_id: ship_id,
                    type_id: ship_tid,
                    owner_id: char_id,
                    location_id: sys_id as i64,
                    flag: InventoryFlag::Hangar,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
        }
    });
    let ctx = ServerContext::with_sde_and_repository(sde, db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_775_000_000)));
    let uid = ctx.db.with_snapshot(|s| s.characters[&char_id].user_id);
    ctx.db
        .acquire_online_lock(uid, 101, Some(char_id), false, ctx.now_unix())
        .expect("register online session for special sites test");
    let mut session = SessionState::new(101);
    session.user_id = Some(uid);
    session.character_id = Some(char_id);
    session.solar_system_id = Some(sys_id);
    session.ship_id = if with_ship { Some(ship_id) } else { None };
    (ctx, session, char_id, ship_id)
}

#[test]
fn registry_blob_and_legacy_note_persistence_roundtrip() {
    let sde = shared_sde();
    let sys_id = find_highsec_system(&sde);
    let (ctx, _, _, _) = make_ctx_and_pilot(sys_id, false);

    let legacy = PveSpecialSitesRegistry {
        next_seq: 42,
        sim_clock_sec: 1_775_123_456,
        triglavian_minor_victory_systems: vec![30_000_119],
        ..Default::default()
    };
    let legacy_json = serde_json::to_string(&legacy).expect("serialize legacy registry");
    ctx.db.with_mut_snapshot(|snap| {
        snap.owner_notes.insert(
            -8_300_000_000_000,
            eve_db::OwnerNoteRecord {
                note_id: -8_300_000_000_000,
                character_id: -83,
                item_id: 0,
                label: "pve_special_sites_registry".into(),
                note: legacy_json,
                timestamp_unix: 1_775_123_456,
            },
        );
        snap.migrate_legacy_service_state();
    });

    let loaded_seq = read_pve_special_reg(&ctx, |reg| (reg.next_seq, spec_now_sec(reg)));
    assert_eq!(loaded_seq, (42, 1_775_123_456));

    let updated = with_pve_special_reg(&ctx, |reg| next_spec_seq(reg));
    assert_eq!(updated, 43);
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.pve_special_sites.next_seq, 43);
        assert!(!snap.service_state_blobs.contains_key("incursions_state"));
        assert!(!snap.owner_notes.contains_key(&-8_300_000_000_000));
    });
    assert_eq!(read_pve_special_reg(&ctx, |reg| reg.next_seq), 43);
}

#[test]
fn combat_state_and_loot_deposit_ship_vs_container() {
    let sde = shared_sde();
    let sys_id = find_highsec_system(&sde);

    let known_cs = build_special_site_npc_combat_state(&sde, 48_090);
    assert!(known_cs.max_shield_hp + known_cs.max_armor_hp + known_cs.max_structure_hp > 0.0);
    let fallback_cs = build_special_site_npc_combat_state(&sde, 999_999_991);
    assert_eq!(fallback_cs.max_shield_hp, 1_200.0);
    assert_eq!(fallback_cs.max_structure_hp, 1_000.0);

    let (ctx, _, char_id, ship_id) = make_ctx_and_pilot(sys_id, true);
    let cid_none = ctx.db.with_mut_snapshot(|snap| {
        deposit_loot_into_ship_or_container_in_snapshot(
            snap, &ctx.sde, char_id, sys_id, ship_id, &[(9_001, 30_745, 2)],
        )
    });
    assert!(cid_none.is_none());
    ctx.db.with_snapshot(|snap| {
        let item = snap.items.get(&9_001).expect("loot deposited in ship cargo");
        assert_eq!(item.location_id, ship_id);
        assert_eq!(item.flag, InventoryFlag::Cargo);
        assert_eq!(item.quantity, 2);
    });

    let cid_some = ctx.db.with_mut_snapshot(|snap| {
        deposit_loot_into_ship_or_container_in_snapshot(
            snap, &ctx.sde, char_id, sys_id, ship_id, &[(9_002, 34, 1_000_000_000)],
        )
    });
    let container_id = cid_some.expect("overflow spawns space cargo container");
    ctx.db.with_snapshot(|snap| {
        let container = snap.items.get(&container_id).expect("container item");
        assert_eq!(container.type_id, 23);
        assert_eq!(container.location_id, sys_id as i64);
        let overflow = snap.items.get(&9_002).expect("overflow loot inside container");
        assert_eq!(overflow.location_id, container_id);
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn drifter_hive_multi_room_gate_checks_and_completion() {
    let hive_sys: SolarSystemId = 31_000_001;
    let (ctx, mut session, char_id, ship_id) = make_ctx_and_pilot(hive_sys, true);

    ensure_special_sites_spawned(&ctx).await;

    let hive = read_pve_special_reg(&ctx, |reg| reg.drifter_hives.get(&hive_sys).cloned())
        .expect("seeded drifter hive 31000001");
    assert_eq!(hive.current_room, 1);
    assert_eq!(hive.total_rooms, 3);

    let r1 = &hive.rooms[0];
    let gate_err = check_drifter_hive_gate_access(&ctx, hive_sys, r1.beacon_ball_id)
        .expect_err("room 1 NPCs block gate");
    assert_eq!(gate_err.code, "RoomNotCleared");

    let wave_err = complete_special_dungeon_instance(&ctx, &mut session, hive_sys as i64, 1)
        .await
        .expect_err("alive room 1 NPC blocks completion");
    assert_eq!(wave_err.code, "WaveNpcsRemaining");

    let sys_handle = ctx.systems.get_or_spawn(hive_sys);
    for &bid in &r1.npc_ball_ids {
        let _ = sys_handle.remove_ball(bid).await;
    }
    with_pve_special_reg(&ctx, |reg| {
        if let Some(h) = reg.drifter_hives.get_mut(&hive_sys) {
            h.rooms[0].destroyed_npc_ball_ids = h.rooms[0].npc_ball_ids.clone();
        }
    });
    check_drifter_hive_gate_access(&ctx, hive_sys, r1.beacon_ball_id)
        .expect("destroyed room 1 NPCs unlock gate");

    let step2 = complete_special_dungeon_instance(&ctx, &mut session, hive.dungeon_id as i64, 1)
        .await
        .expect("advance room 1 -> room 2");
    assert!(format!("{step2:?}").contains("currentRoom"));

    for bid in &hive.rooms[1].npc_ball_ids {
        let _ = sys_handle.remove_ball(*bid).await;
    }
    let _ = complete_special_dungeon_instance(&ctx, &mut session, hive_sys as i64, 1)
        .await
        .expect("advance room 2 -> room 3");

    let warden_err = complete_special_dungeon_instance(&ctx, &mut session, hive_sys as i64, 1)
        .await
        .expect_err("alive warden blocks final clear");
    assert_eq!(warden_err.code, "WaveNpcsRemaining");

    for &bid in &hive.rooms[2].npc_ball_ids {
        let _ = sys_handle.remove_ball(bid).await;
    }
    let _ = sys_handle.remove_ball(hive.warden_npc_ball_id).await;
    let final_kv = complete_special_dungeon_instance(&ctx, &mut session, hive_sys as i64, 1)
        .await
        .expect("final drifter hive clear succeeds");
    assert!(format!("{final_kv:?}").contains("elementTypeID"));

    ctx.db.with_snapshot(|snap| {
        let blue_loot_total: i32 = snap
            .items
            .values()
            .filter(|i| i.owner_id == char_id && i.location_id == ship_id && i.type_id == 30_745)
            .map(|i| i.quantity)
            .sum();
        assert_eq!(blue_loot_total, 5);
    });

    let dup_err = complete_special_dungeon_instance(&ctx, &mut session, hive_sys as i64, 1)
        .await
        .expect_err("cleared hive cannot re-complete");
    assert_eq!(dup_err.code, "SiteAlreadyCompleted");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn homefront_and_triglavian_site_workflows_and_errors() {
    let sde = shared_sde();
    let hs_sys = find_highsec_system(&sde);
    let (ctx, mut session, char_id, _) = make_ctx_and_pilot(hs_sys, true);

    let res = handle_pve_special_sites(
        &ctx, &mut session, "dungeonInstanceCacheMgr", "GetHomefrontSiteInstances", &[], &HashMap::new(),
    )
    .await
    .expect("handled")
    .expect("homefront dict");
    let PyRep::Dict(ref hf_outer) = res else {
        panic!("expected homefront outer dict, got {res:?}");
    };
    let (_, PyRep::List(ref hf_entries)) = hf_outer[0] else {
        panic!("expected homefront list, got {hf_outer:?}");
    };
    assert_eq!(hf_entries.len(), 2);
    assert!(hf_entries[0].dict_get("dungeonID").and_then(|v| v.as_i64()).is_some());
    let cold_inst0 = hf_entries[0].dict_get("instanceID").and_then(|v| v.as_i64()).unwrap();
    assert_eq!(hf_entries[0].dict_get("solarSystemID").and_then(|v| v.as_i64()), Some(hs_sys as i64));
    read_pve_special_reg(&ctx, |reg| {
        assert!(!reg.initialized, "cold GetHomefrontSiteInstances must not initialize registry");
        assert!(reg.homefront_sites.is_empty(), "cold GetHomefrontSiteInstances must not write homefront_sites");
    });

    crate::services::pve_special_sites::ensure_special_sites_initialized(&ctx);
    let hf = read_pve_special_reg(&ctx, |reg| {
        let list = reg.homefront_sites.get(&hs_sys).expect("seeded homefront site in highsec");
        assert!(list.iter().any(|s| s.instance_id == cold_inst0));
        list.iter()
            .find(|s| s.dungeon_id != 10_346)
            .cloned()
            .or_else(|| list.first().cloned())
    })
    .expect("seeded non-AAR homefront site in highsec");

    on_warp_to_homefront_site(&ctx, hs_sys, hf.instance_id).await;
    let sys_handle = ctx.systems.get_or_spawn(hs_sys);
    let hf_rt = crate::services::pve_homefront_sites::get_homefront_runtime_record(&ctx, hf.instance_id)
        .expect("persisted homefront runtime record after warp");
    assert!(
        sys_handle
            .get_ball_snapshot(hf_rt.outer_gate_ball_id)
            .await
            .ok()
            .flatten()
            .is_some(),
        "outer acceleration gate must spawn on warp arrival"
    );
    assert!(
        sys_handle
            .get_ball_snapshot(hf.boss_npc_ball_id)
            .await
            .ok()
            .flatten()
            .is_none(),
        "pocket entities must NOT spawn before gate activation"
    );

    let err_unentered = complete_special_dungeon_instance(&ctx, &mut session, hf.instance_id, 0)
        .await
        .expect_err("gate not entered");
    assert_eq!(err_unentered.code, "GateNotEntered");

    session.station_id = Some(60_003_760);
    let err_docked = complete_special_dungeon_instance(&ctx, &mut session, hf.instance_id, 1)
        .await
        .expect_err("docked blocked");
    assert_eq!(err_docked.code, "ShipDocked");
    session.station_id = None;

    session.solar_system_id = Some(30_000_012);
    let err_wrong_sys = complete_special_dungeon_instance(&ctx, &mut session, hf.instance_id, 1)
        .await
        .expect_err("wrong system blocked");
    assert_eq!(err_wrong_sys.code, "NotInSiteSystem");
    session.solar_system_id = Some(hs_sys);
    let ship_id_hf = session.ship_id.unwrap();
    let ship_tid_hf = ctx.db.with_snapshot(|s| s.items[&ship_id_hf].type_id);
    let _ = ctx
        .systems
        .get_or_spawn(hs_sys)
        .spawn_ball(eve_destiny::Ball {
            id: ship_id_hf,
            type_id: ship_tid_hf,
            position: glam::DVec3::new(10_400.0, 0.0, 0.0),
            velocity: glam::DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        })
        .await;
    if let Some(st) = crate::services::pve_homefront_sites::get_homefront_runtime_state(&ctx, hf.instance_id) {
        let _ = ctx
            .systems
            .get_or_spawn(hs_sys)
            .set_ball_position(ship_id_hf, st.site_position + glam::DVec3::new(400.0, 0.0, 0.0))
            .await;
    }

    crate::services::pve_homefront_sites::activate_homefront_acceleration_gate(
        &ctx,
        &session,
        hf.instance_id,
    )
    .await
    .expect("eligible frigate enters homefront gate");
    let err_incomplete = complete_special_dungeon_instance(&ctx, &mut session, hf.instance_id, 1)
        .await
        .expect_err("objectives incomplete");
    assert_eq!(err_incomplete.code, "ObjectivesIncomplete");

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(rec) = snap.dungeon_runtime.homefront_runtimes.get_mut(&hf.instance_id) {
            if let Some(c) = rec.contributions.get_mut(&char_id) {
                c.damage_dealt = 2_000.0;
                c.objective_actions = 1;
            }
            match &mut rec.progress {
                eve_db::service_domains::pve_sites::HomefrontProgressRecord::Raid {
                    cargo_delivered,
                    required_deliveries,
                    ..
                } => *cargo_delivered = *required_deliveries,
                eve_db::service_domains::pve_sites::HomefrontProgressRecord::EmergencyAid {
                    protectee_hp,
                    survival_elapsed_secs,
                    required_survival_secs,
                    ..
                } => {
                    *protectee_hp = 50_000.0;
                    *survival_elapsed_secs = *required_survival_secs;
                }
                eve_db::service_domains::pve_sites::HomefrontProgressRecord::MetaliminalMeteoroid {
                    remaining_units,
                    ore_mined_by_fleet,
                    total_units,
                    ..
                } => {
                    *remaining_units = 0;
                    *ore_mined_by_fleet = *total_units;
                }
                eve_db::service_domains::pve_sites::HomefrontProgressRecord::DreadAssault {
                    siege_cycles_completed,
                    target_siege_cycles,
                    enemy_base_hp,
                    ..
                } => {
                    *siege_cycles_completed = *target_siege_cycles;
                    *enemy_base_hp = 0.0;
                }
                eve_db::service_domains::pve_sites::HomefrontProgressRecord::AbyssalArtifactRecovery {
                    current_wave,
                    total_waves,
                    mutanite_units_remaining,
                    warp_bubble_active,
                    ..
                } => {
                    *current_wave = *total_waves;
                    *mutanite_units_remaining = 0;
                    *warp_bubble_active = false;
                }
                eve_db::service_domains::pve_sites::HomefrontProgressRecord::SuspiciousSignal {
                    relay_hp,
                    arrays_hacked,
                    ..
                } => {
                    *arrays_hacked = [true; 3];
                    *relay_hp = 0.0;
                }
                eve_db::service_domains::pve_sites::HomefrontProgressRecord::SalvageResearch {
                    wrecks_salvaged,
                    total_wrecks,
                    ..
                } => *wrecks_salvaged = *total_wrecks,
                eve_db::service_domains::pve_sites::HomefrontProgressRecord::StabilizeRift {
                    held_low_secs,
                    required_hold_secs,
                    ..
                } => *held_low_secs = *required_hold_secs,
                eve_db::service_domains::pve_sites::HomefrontProgressRecord::TrafficStop {
                    survival_elapsed_secs,
                    required_survival_secs,
                    smugglers_destroyed,
                    required_smugglers,
                    ..
                } => {
                    *survival_elapsed_secs = *required_survival_secs;
                    *smugglers_destroyed = *required_smugglers;
                }
            }
        }
    });

    let expected_payout =
        crate::services::pve_special_sites_ops::compute_homefront_combat_pilot_isk(
            hf.base_pilot_isk,
            hf.optimal_fleet_size,
            1,
        );
    let _ = complete_special_dungeon_instance(&ctx, &mut session, hf.instance_id, 1)
        .await
        .expect("homefront completed");
    let account_notifs: Vec<_> = ctx
        .drain_notifications_for_char(char_id)
        .into_iter()
        .filter(|n| n.event == "OnAccountChange")
        .collect();
    assert_eq!(account_notifs.len(), 1);
    ctx.db.with_snapshot(|snap| {
        let ch = snap.characters.get(&char_id).unwrap();
        assert_eq!(ch.balance, IskCents::from_isk(1_000_000 + expected_payout));
    });

    let trig_sys: SolarSystemId = 30_000_012;
    session.solar_system_id = Some(trig_sys);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.solar_system_id = trig_sys;
        }
    });
    let trig_res = handle_pve_special_sites(
        &ctx, &mut session, "dungeonInstanceCacheMgr", "GetTriglavianSiteInstances", &[], &HashMap::new(),
    )
    .await
    .expect("handled")
    .expect("trig dict");
    let PyRep::Dict(ref trig_outer) = trig_res else {
        panic!("expected trig outer dict, got {trig_res:?}");
    };
    let (_, PyRep::List(ref trig_entries)) = trig_outer[0] else {
        panic!("expected trig list, got {trig_outer:?}");
    };
    assert_eq!(trig_entries.len(), 2);
    assert!(trig_entries[0].dict_get("dungeonID").and_then(|v| v.as_i64()).is_some());
    assert!(trig_entries[0].dict_get("instanceID").and_then(|v| v.as_i64()).is_some());
    assert_eq!(trig_entries[0].dict_get("solarSystemID").and_then(|v| v.as_i64()), Some(trig_sys as i64));

    let trig = read_pve_special_reg(&ctx, |reg| {
        reg.triglavian_sites.get(&trig_sys).and_then(|v| v.first().cloned())
    })
    .expect("seeded trig site");

    let trig_handle = ctx.systems.get_or_spawn(trig_sys);
    ctx.db.with_mut_snapshot(|snap| {
        snap.item_positions.insert(ship_id_hf, [10_500.0, 0.0, 0.0]);
    });
    let _ = trig_handle
        .spawn_ball(eve_destiny::Ball {
            id: ship_id_hf,
            type_id: ship_tid_hf,
            position: glam::DVec3::new(10_500.0, 0.0, 0.0),
            velocity: glam::DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        })
        .await;
    let _ = trig_handle.remove_ball(trig.boss_npc_ball_id).await;
    for &ebid in &trig.escort_ball_ids {
        let _ = trig_handle.remove_ball(ebid).await;
    }
    let _ = complete_special_dungeon_instance(&ctx, &mut session, trig.instance_id, 1)
        .await
        .expect("trig site completed");

    ctx.db.with_snapshot(|snap| {
        let ch = snap.characters.get(&char_id).unwrap();
        assert!((*ch.standings.get(&500_026).unwrap_or(&0.0) - 0.15).abs() < 1e-6);
        assert!((*ch.standings.get(&500_027).unwrap_or(&0.0) - (-0.15)).abs() < 1e-6);
    });

    let not_found = complete_special_dungeon_instance(&ctx, &mut session, 999_888_777, 1)
        .await
        .expect_err("unknown dungeon");
    assert_eq!(not_found.code, "DungeonNotFound");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rpc_dispatch_interference_storms_and_errors() {
    let sde = shared_sde();
    let poch_or_ls = find_pochven_or_lowsec_system(&sde);
    let (ctx, mut session, _char_id, _ship_id) = make_ctx_and_pilot(poch_or_ls, true);

    assert!(handle_pve_special_sites(&ctx, &mut session, "", "", &[], &HashMap::new()).await.is_none());
    assert!(handle_pve_special_sites(&ctx, &mut session, "map", "", &[], &HashMap::new()).await.is_none());

    let bad_arg = handle_pve_special_sites(
        &ctx, &mut session, "dungeonInstanceCacheMgr", "GetTriglavianSiteInstances", &[PyRep::String("not_an_int".into())], &HashMap::new(),
    )
    .await
    .expect("handled")
    .expect_err("non-int arg");
    assert_eq!(bad_arg.code, "UnknownSolarSystem");

    let unknown_sys = handle_pve_special_sites(
        &ctx, &mut session, "dungeonInstanceCacheMgr", "GetTriglavianSiteInstances", &[PyRep::Integer(99_999_999)], &HashMap::new(),
    )
    .await
    .expect("handled")
    .expect_err("unknown system id");
    assert_eq!(unknown_sys.code, "UnknownSolarSystem");

    let mv_list = handle_pve_special_sites(
        &ctx, &mut session, "dungeonInstanceCacheMgr", "GetTriglavianMinorVictorySystems", &[], &HashMap::new(),
    )
    .await
    .expect("handled")
    .expect("mv list");
    assert!(matches!(mv_list, PyRep::List(ref l) if !l.is_empty()));

    let storms = handle_pve_special_sites(
        &ctx, &mut session, "map", "GetAllRoamingWeatherSystems", &[], &HashMap::new(),
    )
    .await
    .expect("handled")
    .expect("storms list");
    assert!(matches!(storms, PyRep::List(_)));

    let sig_hive = handle_pve_special_sites(
        &ctx, &mut session, "dungeonInstanceCacheMgr", "GetSignatureInstances", &[PyRep::Integer(31_000_002)], &HashMap::new(),
    )
    .await
    .expect("handled")
    .expect("sig dict");
    assert!(matches!(sig_hive, PyRep::Dict(ref d) if !d.is_empty()));
    assert!(handle_pve_special_sites(
        &ctx, &mut session, "dungeonInstanceCacheMgr", "GetSignatureInstances", &[PyRep::Integer(poch_or_ls as i64)], &HashMap::new(),
    )
    .await
    .is_none());

    with_pve_special_reg(&ctx, |reg| {
        reg.interference_by_system.insert(
            poch_or_ls,
            SystemInterferenceRecord {
                solar_system_id: poch_or_ls,
                interference_level: 0.80,
                band: 3,
                last_updated_sec: 1_775_000_000,
            },
        );
    });
    let st_high = handle_pve_special_sites(
        &ctx, &mut session, "solarsystemInterferenceMgr", "GetLocalInterferenceState", &[], &HashMap::new(),
    )
    .await
    .expect("handled")
    .expect("high interference state");
    assert!(format!("{st_high:?}").contains("High"));

    with_pve_special_reg(&ctx, |reg| {
        if let Some(e) = reg.interference_by_system.get_mut(&poch_or_ls) {
            e.last_updated_sec = e.last_updated_sec.saturating_sub(25_000);
        }
    });
    let expected_ts_ft = read_pve_special_reg(&ctx, |reg| {
        (reg.interference_by_system[&poch_or_ls].last_updated_sec as i64 + 11_644_473_600)
            * 10_000_000
    });
    session.notifications.clear();
    let _ = ctx.drain_notifications_for_char(_char_id);
    let epoch_before_reads = ctx.db.state_epoch();
    let commits_before_reads = ctx.db.tx_commit_count();

    // Repeated interference reads reflect decayed level & band read-only using entry.last_updated_sec,
    // without mutating DB state or emitting OnSolarsystemInterferenceChanged.
    for _ in 0..2 {
        let st_decayed = handle_pve_special_sites(
            &ctx, &mut session, "solarsystemInterferenceMgr", "GetSolarSystemInterferenceBand", &[], &HashMap::new(),
        )
        .await
        .expect("handled")
        .expect("decayed interference state");
        assert!(format!("{st_decayed:?}").contains("interferenceLevel"));
        assert_eq!(st_decayed.dict_get("interferenceBand").and_then(|v| v.as_i64()), Some(2));
        assert_eq!(
            st_decayed.dict_get("lastDecayTickTimestamp").and_then(|v| v.as_i64()),
            Some(expected_ts_ft)
        );
    }
    assert_eq!(ctx.db.state_epoch(), epoch_before_reads);
    assert_eq!(ctx.db.tx_commit_count(), commits_before_reads);
    assert!(
        !session.notifications.iter().any(|n| n.0 == "OnSolarsystemInterferenceChanged"),
        "read-only interference queries must not emit OnSolarsystemInterferenceChanged"
    );
    assert!(
        ctx.drain_notifications_for_char(_char_id).is_empty(),
        "read-only interference queries must not queue cluster notifications"
    );

    // Untracked session system must not be fabricated into GetAllInterferenceBands, and invalid explicit system rejects.
    let mut jita_session = session.clone();
    jita_session.solar_system_id = Some(30_000_142);
    let all_bands = handle_pve_special_sites(
        &ctx, &mut jita_session, "solarsystemInterferenceMgr", "GetAllInterferenceBands", &[], &HashMap::new(),
    )
    .await
    .expect("handled")
    .expect("all bands dict");
    let PyRep::Dict(ref band_items) = all_bands else { panic!("expected dict") };
    assert!(band_items.iter().any(|(k, _)| k.as_i64() == Some(poch_or_ls as i64)));
    assert!(
        !band_items.iter().any(|(k, _)| k.as_i64() == Some(30_000_142)),
        "GetAllInterferenceBands must not fabricate untracked session systems"
    );
    for bad_sys in [
        PyRep::Integer(-1),
        PyRep::Integer((1i64 << 32) + 30_000_142),
        PyRep::Integer(99_999_999),
        PyRep::String("bad".into()),
    ] {
        let err = handle_pve_special_sites(
            &ctx, &mut session, "solarsystemInterferenceMgr", "GetLocalInterferenceState", &[bad_sys], &HashMap::new(),
        )
        .await
        .expect("handled")
        .expect_err("invalid solarSystemID must reject");
        assert_eq!(err.code, "UnknownSolarSystem");
    }

    // Background clock tick persists decay and emits OnSolarsystemInterferenceChanged once on band crossing.
    advance_pve_special_clock(&ctx, 3_600);
    let tick_notifs = ctx.drain_notifications_for_char(_char_id);
    assert!(
        tick_notifs.iter().any(|n| n.event == "OnSolarsystemInterferenceChanged"),
        "advance_pve_special_clock must emit OnSolarsystemInterferenceChanged when band decays"
    );
    assert_eq!(
        read_pve_special_reg(&ctx, |reg| reg.interference_by_system.get(&poch_or_ls).map(|e| e.band)),
        Some(2)
    );
    advance_pve_special_clock(&ctx, 3_600);
    assert!(
        !ctx.drain_notifications_for_char(_char_id)
            .iter()
            .any(|n| n.event == "OnSolarsystemInterferenceChanged"),
        "subsequent tick without band crossing must not re-emit OnSolarsystemInterferenceChanged"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn special_site_queries_do_not_spawn_unvisited_system_actors() {
    let jita: SolarSystemId = 30_000_142;
    let (ctx, mut session, _char_id, _ship_id) = make_ctx_and_pilot(jita, true);
    let kw = HashMap::new();

    // Querying combat anomalies, homefront sites, triglavian info, weather, and interference
    // in Jita must lazily initialize metadata without spawning all ~135 unvisited special-site systems.
    let _ = crate::services::pve_dungeon_cache::handle(
        &ctx,
        &mut session,
        "dungeonInstanceCacheMgr",
        "GetCombatAnomalyInstances",
        &[],
        &kw,
    )
    .await;
    let _ = handle_pve_special_sites(
        &ctx,
        &mut session,
        "dungeonInstanceCacheMgr",
        "GetHomefrontSiteInstances",
        &[],
        &kw,
    )
    .await;
    let _ = handle_pve_special_sites(
        &ctx,
        &mut session,
        "dungeonInstanceCacheMgr",
        "GetTriglavianSiteInstances",
        &[PyRep::Integer(30_000_012)],
        &kw,
    )
    .await;
    let _ = handle_pve_special_sites(
        &ctx,
        &mut session,
        "map",
        "GetAllRoamingWeatherSystems",
        &[],
        &kw,
    )
    .await;

    on_server_clock_advanced(&ctx, 1);

    assert!(
        ctx.systems.get_handle(31_000_005).is_none(),
        "unvisited Drifter hive 31000005 must not be spawned from Jita RPCs (`#1159`)"
    );
    assert!(
        ctx.systems.get_handle(30_003_088).is_none(),
        "unvisited EDENCOM system 30003088 must not be spawned from Jita RPCs (`#1159`)"
    );
    assert!(
        ctx.systems.get_handle(30_000_012).is_none(),
        "remote Triglavian query must not spawn unvisited 30000012 (`#1159`)"
    );

    // Entering the active Triglavian system spawns only that system, still leaving others unspawned.
    let trig_sys: SolarSystemId = 30_000_012;
    session.solar_system_id = Some(trig_sys);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&_char_id) {
            c.solar_system_id = trig_sys;
        }
    });
    let _ = handle_pve_special_sites(
        &ctx,
        &mut session,
        "dungeonInstanceCacheMgr",
        "GetTriglavianSiteInstances",
        &[],
        &kw,
    )
    .await;
    assert!(
        ctx.systems.get_handle(trig_sys).is_some(),
        "active caller system 30000012 should spawn its special sites lazily"
    );
    assert!(
        ctx.systems.get_handle(31_000_005).is_none(),
        "unvisited Drifter hive 31000005 must remain unspawned"
    );
    assert!(
        ctx.systems.get_handle(30_003_088).is_none(),
        "unvisited EDENCOM system 30003088 must remain unspawned"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn special_sites_map_tales_and_interference_queries_are_read_pure_and_authz_gated() {
    let jita: SolarSystemId = 30_000_142;
    let (ctx, mut session, _char_id, _ship_id) = make_ctx_and_pilot(jita, true);
    let kw = HashMap::new();

    let epoch0 = ctx.db.state_epoch();
    let commits0 = ctx.db.tx_commit_count();

    // 1. Unauthenticated calls across all special-site, map, interference, and taleMgr RPCs
    // must reject without initializing the registry or bumping DB epoch/commit counters.
    let mut unauth = SessionState::new(999);
    let unauth_calls: &[(&str, &str, Vec<PyRep>)] = &[
        ("dungeonInstanceCacheMgr", "GetHomefrontSiteInstances", vec![]),
        ("dungeonInstanceCacheMgr", "GetTriglavianSiteInstances", vec![PyRep::Integer(30_000_012)]),
        ("dungeonInstanceCacheMgr", "GetSignatureInstances", vec![PyRep::Integer(31_000_001)]),
        ("dungeonInstanceCacheMgr", "GetTriglavianMinorVictorySystems", vec![]),
        ("dungeonInstanceCacheMgr", "GetCombatAnomalyInstances", vec![]),
        ("map", "GetAllRoamingWeatherSystems", vec![]),
        ("map", "GetEdencomFortressSystems", vec![]),
        ("map", "GetEdencomMinorVictorySystems", vec![]),
        ("map", "GetIncursionGlobalReport", vec![]),
        ("map", "GetSystemsInIncursions", vec![]),
        ("map", "GetTriglavianMinorVictorySystems", vec![]),
        ("map", "GetDeadspaceComplexMap", vec![]),
        ("map", "GetDeadspaceAgentsMap", vec![]),
        ("map", "GetStationCount", vec![]),
        ("solarsystemInterferenceMgr", "GetLocalInterferenceState", vec![]),
        ("solarsystemInterferenceMgr", "GetSolarSystemInterferenceBand", vec![]),
        ("solarsystemInterferenceMgr", "GetAllInterferenceBands", vec![]),
        ("taleMgr", "GetGlobalWorldEventTales", vec![]),
        ("taleMgr", "get_active_tales_by_template", vec![PyRep::Integer(5158)]),
    ];
    for (svc, meth, args) in unauth_calls {
        let res = ctx.dispatch_rpc(&mut unauth, svc, None, meth, args, &kw, None).await;
        assert!(res.is_err(), "unauthenticated {svc}.{meth} must reject");
    }
    assert!(ctx
        .dispatch_rpc(&mut unauth, "incursion", None, "GetIncursionLPLog", &[], &kw, None)
        .await
        .is_err());
    assert_eq!(ctx.db.state_epoch(), epoch0, "unauthenticated reads must not bump state_epoch");
    assert_eq!(ctx.db.tx_commit_count(), commits0, "unauthenticated reads must not bump tx_commit_count");
    assert!(read_pve_special_reg(&ctx, |reg| !reg.initialized));

    // 2. Authenticated read-only queries on a cold DB must return expected data with ZERO DB writes.
    let hf = ctx
        .dispatch_rpc(&mut session, "dungeonInstanceCacheMgr", None, "GetHomefrontSiteInstances", &[], &kw, None)
        .await
        .expect("GetHomefrontSiteInstances");
    assert!(matches!(hf, PyRep::Dict(ref d) if !d.is_empty()));

    let trig_remote = ctx
        .dispatch_rpc(
            &mut session,
            "dungeonInstanceCacheMgr",
            None,
            "GetTriglavianSiteInstances",
            &[PyRep::Integer(30_000_012)],
            &kw,
            None,
        )
        .await
        .expect("GetTriglavianSiteInstances remote");
    let PyRep::Dict(ref trig_d) = trig_remote else { panic!("expected dict") };
    let PyRep::List(ref trig_items) = trig_d[0].1 else { panic!("expected list") };
    assert_eq!(trig_items.len(), 2, "cold remote Triglavian query must compute 2 deterministic sites");

    let drifter_sig = ctx
        .dispatch_rpc(
            &mut session,
            "dungeonInstanceCacheMgr",
            None,
            "GetSignatureInstances",
            &[PyRep::Integer(31_000_001)],
            &kw,
            None,
        )
        .await
        .expect("GetSignatureInstances Drifter Hive");
    assert!(matches!(drifter_sig, PyRep::Dict(ref d) if !d.is_empty()));

    let mv = ctx
        .dispatch_rpc(&mut session, "dungeonInstanceCacheMgr", None, "GetTriglavianMinorVictorySystems", &[], &kw, None)
        .await
        .expect("GetTriglavianMinorVictorySystems");
    assert!(matches!(mv, PyRep::List(ref l) if l.len() == 28));

    let storms = ctx
        .dispatch_rpc(&mut session, "map", None, "GetAllRoamingWeatherSystems", &[], &kw, None)
        .await
        .expect("map.GetAllRoamingWeatherSystems");
    assert!(matches!(storms, PyRep::List(ref l) if !l.is_empty()));

    let ef = ctx
        .dispatch_rpc(&mut session, "map", None, "GetEdencomFortressSystems", &[], &kw, None)
        .await
        .expect("map.GetEdencomFortressSystems");
    assert!(matches!(ef, PyRep::List(ref l) if !l.is_empty()));

    let emv = ctx
        .dispatch_rpc(&mut session, "map", None, "GetEdencomMinorVictorySystems", &[], &kw, None)
        .await
        .expect("map.GetEdencomMinorVictorySystems");
    assert!(matches!(emv, PyRep::List(ref l) if !l.is_empty()));

    let inc_rep = ctx
        .dispatch_rpc(&mut session, "map", None, "GetIncursionGlobalReport", &[], &kw, None)
        .await
        .expect("map.GetIncursionGlobalReport");
    assert!(matches!(inc_rep, PyRep::List(_)));

    let inc_sys = ctx
        .dispatch_rpc(&mut session, "map", None, "GetSystemsInIncursions", &[], &kw, None)
        .await
        .expect("map.GetSystemsInIncursions");
    assert!(matches!(inc_sys, PyRep::List(_)));

    let map_tmv = ctx
        .dispatch_rpc(&mut session, "map", None, "GetTriglavianMinorVictorySystems", &[], &kw, None)
        .await
        .expect("map.GetTriglavianMinorVictorySystems");
    assert!(matches!(map_tmv, PyRep::List(ref l) if l.len() == 28));

    let _ = ctx
        .dispatch_rpc(&mut session, "map", None, "GetDeadspaceComplexMap", &[], &kw, None)
        .await
        .expect("map.GetDeadspaceComplexMap");
    let _ = ctx
        .dispatch_rpc(&mut session, "map", None, "GetDeadspaceAgentsMap", &[], &kw, None)
        .await
        .expect("map.GetDeadspaceAgentsMap");

    let tales = ctx
        .dispatch_rpc(&mut session, "taleMgr", None, "GetGlobalWorldEventTales", &[], &kw, None)
        .await
        .expect("taleMgr.GetGlobalWorldEventTales");
    assert!(matches!(tales, PyRep::List(ref l) if !l.is_empty()));

    let tpl_tales = ctx
        .dispatch_rpc(
            &mut session,
            "taleMgr",
            None,
            "get_active_tales_by_template",
            &[PyRep::Integer(5201)],
            &kw,
            None,
        )
        .await
        .expect("taleMgr.get_active_tales_by_template");
    assert!(matches!(tpl_tales, PyRep::List(ref l) if !l.is_empty()));

    let local_inf = ctx
        .dispatch_rpc(&mut session, "solarsystemInterferenceMgr", None, "GetLocalInterferenceState", &[], &kw, None)
        .await
        .expect("GetLocalInterferenceState");
    assert_eq!(local_inf.dict_get("interferenceBand").and_then(|v| v.as_i64()), Some(1));

    let sys_band = ctx
        .dispatch_rpc(
            &mut session,
            "solarsystemInterferenceMgr",
            None,
            "GetSolarSystemInterferenceBand",
            &[PyRep::Integer(i64::from(jita))],
            &kw,
            None,
        )
        .await
        .expect("GetSolarSystemInterferenceBand");
    assert_eq!(sys_band.dict_get("interferenceBand").and_then(|v| v.as_i64()), Some(1));

    let all_bands = ctx
        .dispatch_rpc(&mut session, "solarsystemInterferenceMgr", None, "GetAllInterferenceBands", &[], &kw, None)
        .await
        .expect("GetAllInterferenceBands");
    assert!(matches!(all_bands, PyRep::Dict(ref d) if d.is_empty()));

    assert_eq!(ctx.db.state_epoch(), epoch0, "authenticated read-only queries must not bump state_epoch");
    assert_eq!(ctx.db.tx_commit_count(), commits0, "authenticated read-only queries must not bump tx_commit_count");
    assert!(read_pve_special_reg(&ctx, |reg| !reg.initialized));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn drifter_hive_in_system_signature_query_is_read_pure_and_pochven_boss_kill_persists() {
    let drifter_sys: SolarSystemId = 31_000_001;
    let (drifter_ctx, mut drifter_sess, _d_char, _d_ship) = make_ctx_and_pilot(drifter_sys, true);
    let kw = HashMap::new();

    let d_epoch0 = drifter_ctx.db.state_epoch();
    let d_commits0 = drifter_ctx.db.tx_commit_count();
    let in_sys_sig = drifter_ctx
        .dispatch_rpc(
            &mut drifter_sess,
            "dungeonInstanceCacheMgr",
            None,
            "GetSignatureInstances",
            &[PyRep::Integer(i64::from(drifter_sys))],
            &kw,
            None,
        )
        .await
        .expect("in-system GetSignatureInstances");
    assert!(matches!(in_sys_sig, PyRep::Dict(ref d) if !d.is_empty()));
    assert_eq!(drifter_ctx.db.state_epoch(), d_epoch0, "in-system Drifter Hive query must not bump state_epoch");
    assert_eq!(drifter_ctx.db.tx_commit_count(), d_commits0, "in-system Drifter Hive query must not bump tx_commit_count");
    assert!(drifter_ctx.systems.get_handle(drifter_sys).is_none(), "in-system GetSignatureInstances must not spawn system actor");

    let pochven_sys = shared_sde()
        .solar_systems
        .values()
        .filter(|s| s.region_id == 10_000_070)
        .min_by_key(|s| s.solar_system_id)
        .map(|s| s.solar_system_id)
        .expect("canonical SDE contains Pochven solar system");
    let (ctx, mut session, char_id, ship_id) = make_ctx_and_pilot(pochven_sys, true);
    let epoch0 = ctx.db.state_epoch();
    let commits0 = ctx.db.tx_commit_count();

    // Remote / docked Pochven queries remain 100% read-pure with zero DB writes or ball spawns.
    session.station_id = Some(60_003_760);
    let remote_res = handle_pve_special_sites(
        &ctx,
        &mut session,
        "dungeonInstanceCacheMgr",
        "GetTriglavianSiteInstances",
        &[PyRep::Integer(i64::from(pochven_sys))],
        &kw,
    )
    .await
    .expect("handled")
    .expect("docked Pochven query succeeds");
    assert!(matches!(remote_res, PyRep::Dict(ref d) if !d.is_empty()));
    assert_eq!(ctx.db.state_epoch(), epoch0);
    assert_eq!(ctx.db.tx_commit_count(), commits0);
    assert!(ctx.systems.get_handle(pochven_sys).is_none());

    // Undocked in-system Pochven query persists reg.triglavian_sites FIRST before spawning NPC balls,
    // so both boss-kill events and complete_special_dungeon_instance succeed on a cold DB.
    session.station_id = None;
    let in_sys_res = handle_pve_special_sites(
        &ctx,
        &mut session,
        "dungeonInstanceCacheMgr",
        "GetTriglavianSiteInstances",
        &[PyRep::Integer(i64::from(pochven_sys))],
        &kw,
    )
    .await
    .expect("handled")
    .expect("undocked Pochven query succeeds");
    assert!(matches!(in_sys_res, PyRep::Dict(ref d) if !d.is_empty()));

    let sites = read_pve_special_reg(&ctx, |reg| reg.triglavian_sites.get(&pochven_sys).cloned())
        .expect("Pochven triglavian_sites must be persisted when spawning balls");
    assert_eq!(sites.len(), 2);

    // Repeated in-system read skips unnecessary DB write commits.
    let commits_after_seed = ctx.db.tx_commit_count();
    let _ = handle_pve_special_sites(
        &ctx,
        &mut session,
        "dungeonInstanceCacheMgr",
        "GetTriglavianSiteInstances",
        &[PyRep::Integer(i64::from(pochven_sys))],
        &kw,
    )
    .await
    .expect("handled")
    .expect("repeat Pochven query succeeds");
    assert_eq!(ctx.db.tx_commit_count(), commits_after_seed);

    let sys_handle = ctx.systems.get_handle(pochven_sys).expect("Pochven system actor spawned");
    let site0 = &sites[0];
    assert!(sys_handle.get_ball_snapshot(site0.boss_npc_ball_id).await.ok().flatten().is_some());

    // Boss kill via on_special_site_npc_destroyed_core finds persisted site 0 and awards standings/ISK.
    let kill_accts = crate::services::pve_special_sites_combat::on_special_site_npc_destroyed_core(
        &ctx.systems,
        &ctx.db,
        &ctx.sde,
        pochven_sys,
        site0.boss_npc_ball_id,
        Some(char_id),
    )
    .await;
    assert!(
        kill_accts.iter().any(|(cid, _)| *cid == char_id),
        "Pochven boss kill must award killer character"
    );
    assert!(read_pve_special_reg(&ctx, |reg| reg.triglavian_sites[&pochven_sys][0].completed));

    // Manual completion via complete_special_dungeon_instance on site 1 succeeds as well.
    let site1 = &sites[1];
    let ship_tid = find_ship_type_id(&shared_sde());
    ctx.db.with_mut_snapshot(|snap| {
        snap.item_positions.insert(ship_id, [30_500.0, 0.0, 0.0]);
    });
    let _ = sys_handle
        .spawn_ball(eve_destiny::Ball {
            id: ship_id,
            type_id: ship_tid,
            position: glam::DVec3::new(30_500.0, 0.0, 0.0),
            velocity: glam::DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        })
        .await;
    let _ = sys_handle.remove_ball(site1.boss_npc_ball_id).await;
    for &ebid in &site1.escort_ball_ids {
        let _ = sys_handle.remove_ball(ebid).await;
    }
    let _ = complete_special_dungeon_instance(&ctx, &mut session, site1.instance_id, 1)
        .await
        .expect("Pochven site completion succeeds");
    assert!(read_pve_special_reg(&ctx, |reg| reg.triglavian_sites[&pochven_sys][1].completed));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn triglavian_site_concurrent_completion_pays_exactly_once() {
    let sde = shared_sde();
    let pochven_sys = sde
        .solar_systems
        .values()
        .filter(|s| s.region_id == 10_000_070)
        .min_by_key(|s| s.solar_system_id)
        .map(|s| s.solar_system_id)
        .expect("canonical SDE contains Pochven solar system");
    let (ctx, mut sess1, char1, ship1) = make_ctx_and_pilot(pochven_sys, true);
    let char2: i64 = 90_010_002;
    let ship2: i64 = 1_010_000_002;
    let ship_tid = find_ship_type_id(&sde);
    ctx.db.provision_gm_account("spec_site_pilot_2", "pw", char2, "SpecSitePilot2");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char2) {
            ch.solar_system_id = pochven_sys;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = ship2;
            ch.balance = IskCents::from_isk(1_000_000);
        }
        snap.items.insert(
            ship2,
            InventoryItemRecord {
                item_id: ship2,
                type_id: ship_tid,
                owner_id: char2,
                location_id: pochven_sys as i64,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.item_positions.insert(ship1, [10_500.0, 0.0, 0.0]);
        snap.item_positions.insert(ship2, [10_500.0, 0.0, 0.0]);
    });
    let uid2 = ctx.db.with_snapshot(|s| s.characters[&char2].user_id);
    ctx.db
        .acquire_online_lock(uid2, 102, Some(char2), false, ctx.now_unix())
        .expect("register online session for pilot 2");
    let mut sess2 = SessionState::new(102);
    sess2.user_id = Some(uid2);
    sess2.character_id = Some(char2);
    sess2.solar_system_id = Some(pochven_sys);
    sess2.ship_id = Some(ship2);

    // Seed and spawn Pochven Triglavian sites undocked in-system.
    let _ = handle_pve_special_sites(
        &ctx,
        &mut sess1,
        "dungeonInstanceCacheMgr",
        "GetTriglavianSiteInstances",
        &[PyRep::Integer(i64::from(pochven_sys))],
        &HashMap::new(),
    )
    .await
    .expect("handled")
    .expect("seed Pochven sites");

    let site0 = read_pve_special_reg(&ctx, |reg| reg.triglavian_sites[&pochven_sys][0].clone());
    let sys_handle = ctx.systems.get_or_spawn(pochven_sys);
    for (sid, bid) in [(ship1, ship1), (ship2, ship2)] {
        let _ = sid;
        sys_handle
            .spawn_ball(eve_destiny::Ball {
                id: bid,
                type_id: ship_tid,
                position: glam::DVec3::new(10_500.0, 0.0, 0.0),
                velocity: glam::DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: eve_destiny::MovementMode::Stop,
            })
            .await
            .expect("spawn pilot ball");
    }

    assert!(
        sys_handle.get_ball_snapshot(site0.boss_npc_ball_id).await.unwrap().is_some(),
        "site0 boss NPC must be spawned before clearing"
    );
    sys_handle.remove_ball(site0.boss_npc_ball_id).await.expect("clear boss");
    for &ebid in &site0.escort_ball_ids {
        sys_handle.remove_ball(ebid).await.expect("clear escort");
    }

    // Two concurrent completions across two pilots must execute atomically and pay strictly once.
    let (res1, res2) = tokio::join!(
        complete_special_dungeon_instance(&ctx, &mut sess1, site0.instance_id, 1),
        complete_special_dungeon_instance(&ctx, &mut sess2, site0.instance_id, 1),
    );
    let (winner_cid, loser_cid, winner_rep, loser_err) = match (res1, res2) {
        (Ok(ok), Err(err)) => (char1, char2, ok, err),
        (Err(err), Ok(ok)) => (char2, char1, ok, err),
        (Ok(a), Ok(b)) => panic!("Both concurrent callers paid out! res1={a:?} res2={b:?}"),
        (Err(a), Err(b)) => panic!("Neither caller completed site: err1={a:?} err2={b:?}"),
    };
    assert_eq!(loser_err.code, "SiteAlreadyCompleted");
    assert_eq!(winner_rep.dict_get("completed"), Some(&PyRep::Bool(true)));
    assert_eq!(
        winner_rep.dict_get("baseIsk").and_then(|v| v.as_i64()),
        Some(site0.base_isk)
    );

    ctx.db.with_snapshot(|snap| {
        assert!(snap.pve_special_sites.triglavian_sites[&pochven_sys][0].completed);
        let win_ch = &snap.characters[&winner_cid];
        let lose_ch = &snap.characters[&loser_cid];
        assert_eq!(
            win_ch.balance,
            IskCents::from_isk(1_000_000 + site0.base_isk),
            "winner must receive exact single base_isk payout"
        );
        assert_eq!(
            lose_ch.balance,
            IskCents::from_isk(1_000_000),
            "loser must receive zero ISK"
        );
        assert!((win_ch.standings.get(&500_026).copied().unwrap_or(0.0)).abs() > 0.0);
        assert_eq!(lose_ch.standings.get(&500_026).copied().unwrap_or(0.0), 0.0);
        assert_eq!(lose_ch.standings.get(&500_027).copied().unwrap_or(0.0), 0.0);

        let qty_for = |cid: i64, tid: u32| -> i32 {
            snap.items
                .values()
                .filter(|it| it.owner_id == cid && it.type_id == tid)
                .map(|it| it.quantity)
                .sum()
        };
        assert_eq!(qty_for(winner_cid, 48_121), site0.survey_db_drop as i32);
        assert_eq!(qty_for(winner_cid, 30_747), 2);
        assert_eq!(qty_for(loser_cid, 48_121), 0);
        assert_eq!(qty_for(loser_cid, 30_747), 0);
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn triglavian_site_cannot_complete_unspawned_or_remotely_queried_without_killing_npcs() {
    let sde = shared_sde();
    let jita_sys: SolarSystemId = 30_000_142;
    let pochven_sys = sde
        .solar_systems
        .values()
        .filter(|s| s.region_id == 10_000_070)
        .min_by_key(|s| s.solar_system_id)
        .map(|s| s.solar_system_id)
        .expect("canonical SDE contains Pochven solar system");
    let (ctx, mut session, char_id, ship_id) = make_ctx_and_pilot(jita_sys, true);
    let kw = HashMap::new();

    // 1. Remote query from Jita returns site metadata read-only without seeding DB or spawning balls.
    let epoch0 = ctx.db.state_epoch();
    let commits0 = ctx.db.tx_commit_count();
    let remote_rep = handle_pve_special_sites(
        &ctx,
        &mut session,
        "dungeonInstanceCacheMgr",
        "GetTriglavianSiteInstances",
        &[PyRep::Integer(i64::from(pochven_sys))],
        &kw,
    )
    .await
    .expect("handled")
    .expect("remote Pochven read succeeds");
    let PyRep::Dict(ref outer) = remote_rep else {
        panic!("expected dict, got {remote_rep:?}");
    };
    let (_, PyRep::List(ref site_list)) = outer[0] else {
        panic!("expected site list");
    };
    let remote_inst_id = site_list[0]
        .dict_get("instanceID")
        .and_then(|v| v.as_i64())
        .expect("remote query returns instanceID");
    assert_eq!(ctx.db.state_epoch(), epoch0);
    assert_eq!(ctx.db.tx_commit_count(), commits0);
    assert!(read_pve_special_reg(&ctx, |reg| !reg.triglavian_sites.contains_key(&pochven_sys)));
    assert!(ctx.systems.get_handle(pochven_sys).is_none());

    // Remote completion attempt fails.
    let remote_complete = complete_special_dungeon_instance(&ctx, &mut session, remote_inst_id, 1)
        .await
        .expect_err("remote completion must fail");
    assert_eq!(remote_complete.code, "DungeonNotFound");

    // 2. Arriving in Pochven on-grid without killing NPCs fails (spawns NPCs -> WaveNpcsRemaining).
    let ship_tid = find_ship_type_id(&sde);
    session.solar_system_id = Some(pochven_sys);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = pochven_sys;
        }
        snap.item_positions.insert(ship_id, [10_500.0, 0.0, 0.0]);
    });
    let sys_handle = ctx.systems.get_or_spawn(pochven_sys);
    sys_handle
        .spawn_ball(eve_destiny::Ball {
            id: ship_id,
            type_id: ship_tid,
            position: glam::DVec3::new(10_500.0, 0.0, 0.0),
            velocity: glam::DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        })
        .await
        .expect("spawn pilot ship");

    let live_err = complete_special_dungeon_instance(&ctx, &mut session, remote_inst_id, 1)
        .await
        .expect_err("site with live NPCs must reject completion");
    assert_eq!(live_err.code, "WaveNpcsRemaining");

    // 3. Unspawned zero-ball sites cannot complete from ballpark absence alone.
    let zero_ball_inst = 720_991;
    with_pve_special_reg(&ctx, |reg| {
        reg.triglavian_sites
            .entry(pochven_sys)
            .or_default()
            .push(TriglavianSiteRecord {
                instance_id: zero_ball_inst,
                dungeon_id: 900_991,
                dungeon_name: "Zero-Ball Exploit Site".to_string(),
                solar_system_id: pochven_sys,
                faction_owner_id: 500_026,
                base_isk: 30_000_000,
                survey_db_drop: 150,
                completed: false,
                boss_npc_ball_id: 0,
                boss_type_id: 0,
                escort_ball_ids: Vec::new(),
                escort_type_ids: Vec::new(),
                beacon_ball_id: 0,
            });
    });

    let zero_err = complete_special_dungeon_instance(&ctx, &mut session, zero_ball_inst, 1)
        .await
        .expect_err("zero-ball site must not complete");
    assert_eq!(zero_err.code, "SiteNotSpawned");

    // 4. Real transient spawn failure rolls back `Spawning` via RAII Drop, rejects completion
    //    without payout, and recovers cleanly on subsequent retry.
    let pochven_sys_2 = sde
        .solar_systems
        .values()
        .filter(|s| s.region_id == 10_000_070 && s.solar_system_id != pochven_sys)
        .min_by_key(|s| s.solar_system_id)
        .map(|s| s.solar_system_id)
        .expect("canonical SDE contains second Pochven solar system");
    let sys2_handle = ctx.systems.get_or_spawn(pochven_sys_2);
    session.solar_system_id = Some(pochven_sys_2);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = pochven_sys_2;
        }
        snap.item_positions.insert(ship_id, [10_500.0, 0.0, 0.0]);
    });
    sys2_handle
        .spawn_ball(eve_destiny::Ball {
            id: ship_id,
            type_id: ship_tid,
            position: glam::DVec3::new(10_500.0, 0.0, 0.0),
            velocity: glam::DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        })
        .await
        .expect("spawn pilot ship in sys2");

    // Persist sys2 Triglavian sites directly into the registry (simulating an unspawned
    // persisted site before `spawn_special_site_balls` runs) and inject a transient actor panic on
    // command 2 (`spawn_ball_with_combat` of boss NPC after `get_ball_snapshot`).
    let sys2_sites = build_default_triglavian_sites_for_system(
        &ctx,
        pochven_sys_2,
        &edencom_minor_victory_systems(),
        &triglavian_minor_victory_systems(),
    );
    assert!(!sys2_sites.is_empty());
    let sys2_inst = sys2_sites[0].instance_id;
    let sys2_boss = sys2_sites[0].boss_npc_ball_id;
    let sys2_escorts = sys2_sites[0].escort_ball_ids.clone();
    with_pve_special_reg(&ctx, |reg| {
        reg.triglavian_sites.insert(pochven_sys_2, sys2_sites);
    });

    sys2_handle
        .panic_on_command_countdown_for_test
        .store(2, std::sync::atomic::Ordering::SeqCst);
    let transient_err = complete_special_dungeon_instance(&ctx, &mut session, sys2_inst, 1)
        .await
        .expect_err("transient spawn failure must reject completion");
    assert_eq!(transient_err.code, "BallparkSpawnFailed");
    assert!(
        !ctx.systems.is_special_site_ball_spawned(pochven_sys_2, sys2_boss),
        "failed spawn must roll back Spawning guard"
    );
    assert!(
        sys2_handle.get_ball_snapshot(sys2_boss).await.unwrap().is_none(),
        "failed spawn must leave boss absent from ballpark"
    );

    ctx.db.with_snapshot(|snap| {
        let ch = &snap.characters[&char_id];
        assert_eq!(ch.balance, IskCents::from_isk(1_000_000));
        assert_eq!(ch.standings.get(&500_026).copied().unwrap_or(0.0), 0.0);
        assert_eq!(ch.standings.get(&500_027).copied().unwrap_or(0.0), 0.0);
    });

    // Retrying once actor recovers spawns the NPCs cleanly (never stuck as SiteNotSpawned) and
    // rejects completion because live NPCs are now in space.
    let retry_live = complete_special_dungeon_instance(&ctx, &mut session, sys2_inst, 1)
        .await
        .expect_err("recovered retry must spawn NPCs and reject while NPCs remain alive");
    assert_eq!(retry_live.code, "WaveNpcsRemaining");
    assert!(ctx.systems.is_special_site_ball_spawned(pochven_sys_2, sys2_boss));
    assert!(sys2_handle.get_ball_snapshot(sys2_boss).await.unwrap().is_some());

    // After killing/clearing all spawned NPCs on retry, completion succeeds once.
    sys2_handle.remove_ball(sys2_boss).await.expect("clear recovered boss");
    for &ebid in &sys2_escorts {
        sys2_handle.remove_ball(ebid).await.expect("clear recovered escort");
    }
    let ok = complete_special_dungeon_instance(&ctx, &mut session, sys2_inst, 1)
        .await
        .expect("completion succeeds after clearing recovered NPCs");
    assert_eq!(ok.dict_get("completed"), Some(&PyRep::Bool(true)));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn triglavian_cold_system_concurrent_spawn_synchronizes_via_notify() {
    let sde = shared_sde();
    let mv_sys = edencom_minor_victory_systems()[0];
    let (ctx, sess1, _char1, ship1) = make_ctx_and_pilot(mv_sys, true);
    ensure_special_sites_initialized(&ctx);

    let site0 = read_pve_special_reg(&ctx, |reg| reg.triglavian_sites[&mv_sys][0].clone());
    let ship_tid = find_ship_type_id(&sde);
    let char2: i64 = 90_010_003;
    let ship2: i64 = 1_010_000_003;
    ctx.db.provision_gm_account("spec_notify_2", "pw", char2, "SpecNotify2");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char2) {
            ch.solar_system_id = mv_sys;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = ship2;
        }
        snap.items.insert(
            ship2,
            InventoryItemRecord {
                item_id: ship2,
                type_id: ship_tid,
                owner_id: char2,
                location_id: mv_sys as i64,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.item_positions.insert(ship1, [10_500.0, 0.0, 0.0]);
        snap.item_positions.insert(ship2, [10_500.0, 0.0, 0.0]);
    });
    let uid2 = ctx.db.with_snapshot(|s| s.characters[&char2].user_id);
    ctx.db
        .acquire_online_lock(uid2, 103, Some(char2), false, ctx.now_unix())
        .expect("register online session");
    let mut sess2 = SessionState::new(103);
    sess2.user_id = Some(uid2);
    sess2.character_id = Some(char2);
    sess2.solar_system_id = Some(mv_sys);
    sess2.ship_id = Some(ship2);

    let sys_handle = ctx.systems.get_or_spawn(mv_sys);
    for bid in [ship1, ship2] {
        sys_handle
            .spawn_ball(eve_destiny::Ball {
                id: bid,
                type_id: ship_tid,
                position: glam::DVec3::new(10_500.0, 0.0, 0.0),
                velocity: glam::DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: eve_destiny::MovementMode::Stop,
            })
            .await
            .expect("spawn pilot ship");
    }

    // 1. Hold active `Spawning` guard on boss NPC to simulate an in-flight cold-system spawner,
    //    and launch concurrent `GetTriglavianSiteInstances` and `complete_special_dungeon_instance`.
    let hold_guard = ctx
        .systems
        .begin_special_site_ball_spawn(mv_sys, site0.boss_npc_ball_id)
        .await
        .expect("acquire initial Spawning guard");

    let ctx_get = ctx.clone();
    let mut sess_get = sess1.clone();
    let get_task = tokio::spawn(async move {
        handle_pve_special_sites(
            &ctx_get,
            &mut sess_get,
            "dungeonInstanceCacheMgr",
            "GetTriglavianSiteInstances",
            &[PyRep::Integer(i64::from(mv_sys))],
            &HashMap::new(),
        )
        .await
    });

    let ctx_comp = ctx.clone();
    let comp_inst = site0.instance_id;
    let comp_task = tokio::spawn(async move {
        complete_special_dungeon_instance(&ctx_comp, &mut sess2, comp_inst, 1).await
    });

    tokio::task::yield_now().await;
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    assert!(
        !get_task.is_finished(),
        "concurrent GetTriglavianSiteInstances must wait on Notify rather than returning before boss spawns"
    );
    assert!(
        !comp_task.is_finished(),
        "concurrent completion must wait on Notify rather than skipping ahead to SiteNotSpawned"
    );

    // Finish spawning boss NPC and confirm the guard -> both waiters unblock cleanly.
    let boss_tid = if site0.boss_type_id != 0 { site0.boss_type_id } else { 48_122 };
    let cs = build_special_site_npc_combat_state(&ctx.sde, boss_tid);
    sys_handle
        .spawn_ball_with_combat(
            make_site_npc_ball(site0.boss_npc_ball_id, boss_tid, glam::DVec3::new(10_000.0, 0.0, 0.0)),
            cs,
        )
        .await
        .expect("spawn held boss NPC");
    hold_guard.confirm();

    let get_res = tokio::time::timeout(std::time::Duration::from_secs(3), get_task)
        .await
        .expect("GetTriglavianSiteInstances wakes after confirm()")
        .expect("get task join ok")
        .expect("rpc handled")
        .expect("GetTriglavianSiteInstances succeeds");
    assert!(matches!(get_res, PyRep::Dict(ref d) if !d.is_empty()));

    let comp_err = tokio::time::timeout(std::time::Duration::from_secs(3), comp_task)
        .await
        .expect("complete_special_dungeon_instance wakes after confirm()")
        .expect("complete task join ok")
        .expect_err("must reject while spawned NPCs remain alive");
    assert_eq!(
        comp_err.code, "WaveNpcsRemaining",
        "Notify-synchronized completion must observe WaveNpcsRemaining, never SiteNotSpawned"
    );
    let _ = sess1.character_id;
}

#[cfg(test)]
#[path = "pve_special_sites_extra_unit_tests.rs"]
mod pve_special_sites_extra_unit_tests;
