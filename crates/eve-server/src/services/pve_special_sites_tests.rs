//! Unit and integration tests for `pve_special_sites`.

use super::*;
use crate::router::ServerContext;
use eve_db::clock::VirtualServerClock;
use eve_db::WorldRepository;
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
        .find(|s| s.region_id == 10_000_070)
        .map(|s| s.solar_system_id)
        .unwrap_or_else(|| {
            sde.solar_systems
                .values()
                .find(|s| s.security_status < 0.45 && s.solar_system_id < 31_000_000)
                .map(|s| s.solar_system_id)
                .unwrap_or(30_000_119)
        })
}

fn find_ship_type_id(sde: &SdeRegistry) -> u32 {
    sde.types
        .values()
        .find(|t| t.category_id == 6 && t.capacity > 50.0)
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
    let mut session = SessionState::new(101);
    session.user_id = Some(1);
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
    assert!(hf_entries[0].dict_get("instanceID").and_then(|v| v.as_i64()).is_some());
    assert_eq!(hf_entries[0].dict_get("solarSystemID").and_then(|v| v.as_i64()), Some(hs_sys as i64));

    let hf = read_pve_special_reg(&ctx, |reg| {
        reg.homefront_sites.get(&hs_sys).and_then(|v| v.first().cloned())
    })
    .expect("seeded homefront site in highsec");

    on_warp_to_homefront_site(&ctx, hs_sys, hf.instance_id).await;
    let sys_handle = ctx.systems.get_or_spawn(hs_sys);
    assert!(sys_handle.get_ball_snapshot(hf.boss_npc_ball_id).await.ok().flatten().is_some());

    let err_alive = complete_special_dungeon_instance(&ctx, &mut session, hf.instance_id, 0)
        .await
        .expect_err("boss alive");
    assert_eq!(err_alive.code, "WaveNpcsRemaining");

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

    let _ = sys_handle.remove_ball(hf.boss_npc_ball_id).await;
    let expected_payout = ((hf.base_pilot_isk as f64) * (1.0 / (hf.optimal_fleet_size as f64))).round() as i64;
    let _ = complete_special_dungeon_instance(&ctx, &mut session, hf.instance_id, 1)
        .await
        .expect("homefront completed");
    assert!(session.notifications.iter().any(|n| n.0 == "OnAccountChange"));
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
    let st_decayed = handle_pve_special_sites(
        &ctx, &mut session, "solarsystemInterferenceMgr", "GetSolarSystemInterferenceBand", &[], &HashMap::new(),
    )
    .await
    .expect("handled")
    .expect("decayed interference state");
    assert!(format!("{st_decayed:?}").contains("interferenceLevel"));
    assert!(session.notifications.iter().any(|n| n.0 == "OnSolarsystemInterferenceChanged"));

    let all_bands = handle_pve_special_sites(
        &ctx, &mut session, "solarsystemInterferenceMgr", "GetAllInterferenceBands", &[], &HashMap::new(),
    )
    .await
    .expect("handled")
    .expect("all bands dict");
    assert!(matches!(all_bands, PyRep::Dict(ref d) if !d.is_empty()));

    advance_pve_special_clock(&ctx, 3_600);
}

#[cfg(test)]
#[path = "pve_special_sites_extra_unit_tests.rs"]
mod pve_special_sites_extra_unit_tests;
