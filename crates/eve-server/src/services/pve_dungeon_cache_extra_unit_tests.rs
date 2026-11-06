//! Extra branch-coverage and mutation-killing unit tests for `pve_dungeon_cache`.

use super::pve_dungeon_cache::*;
use crate::router::ServerContext;
use crate::services::pve_exploration::{PveSiteInstance, with_pve_exploration_reg};
use crate::services::pve_special_sites::with_pve_special_reg;
use crate::services::pve_special_sites_abyssal::{
    AbyssalPocketSession, AbyssalRoomRecord, AbyssalWeatherKind,
};
use eve_db::clock::VirtualServerClock;
use eve_db::durable_domain_types::PersistedBattlefieldRecord;
use eve_db::{CosmicAnomalyRecord, CosmicSignatureRecord, WorldRepository};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::{SessionState, service_roles};
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::{
    DungeonAsteroidEntry, DungeonTemplate, InvType, SdeRegistry, SolarSystemId, StargateJump,
};
use glam::DVec3;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn make_inv_type(type_id: u32, group_id: u32, category_id: u32, desc: &str) -> InvType {
    InvType {
        type_id,
        group_id,
        category_id,
        name: format!("Type-{type_id}"),
        mass: 1.0,
        volume: 1.0,
        capacity: 0.0,
        radius: 10.0,
        base_price: 10.0,
        published: true,
        portion_size: 1,
        market_group_id: None,
        race_id: None,
        faction_id: None,
        variation_parent_type_id: None,
        description_id: None,
        icon_id: None,
        graphic_id: None,
        sound_id: None,
        meta_group_id: None,
        meta_level: None,
        tech_level: None,
        default_effect_id: None,
        description: desc.into(),
        effect_ids: Vec::new(),
        default_attributes: HashMap::new(),
    }
}

async fn call_cache_err(
    ctx: &ServerContext,
    session: &mut SessionState,
    svc: &str,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> BridgeRpcError {
    handle(ctx, session, svc, method, args, kwargs)
        .await
        .expect("handled")
        .expect_err("expected error")
}

fn make_anom(id: &str, sys_id: u32, kind: &str, pos: [f64; 3], created: u64, life: u64, cleared: bool) -> CosmicAnomalyRecord {
    CosmicAnomalyRecord {
        anomaly_id: id.into(),
        solar_system_id: sys_id,
        name: id.into(),
        anomaly_type: kind.into(),
        position: pos,
        signal_strength: 1.0,
        ore_types: Vec::new(),
        remaining_volume: 100.0,
        created_unix: created,
        lifetime_seconds: life,
        cleared,
    }
}

#[test]
fn ice_ceiling_eligible_belts_and_parse_dng_id_edge_cases() {
    let mut sde = SdeRegistry::default();
    assert_eq!(ice_type_security_ceiling(&sde, 999_999), None);

    sde.types.insert(900_001, make_inv_type(900_001, 465, 25, "No availability tag"));
    sde.types.insert(900_002, make_inv_type(900_002, 465, 25, "Available in some region without suffix"));
    sde.types.insert(
        900_003,
        make_inv_type(900_003, 465, 25, "Available in <color=0xff00ff00>notanumber</color> security status systems."),
    );
    sde.types.insert(
        900_004,
        make_inv_type(900_004, 465, 25, "Available in <color=0xff00ff00>0.4</color> security status solar systems or lower."),
    );
    sde.types.insert(
        900_005,
        make_inv_type(900_005, 465, 25, "Available in <color=0xffff0000>-0.1</color> security status solar systems or lower."),
    );

    assert_eq!(ice_type_security_ceiling(&sde, 900_001), None);
    assert_eq!(ice_type_security_ceiling(&sde, 900_002), None);
    assert_eq!(ice_type_security_ceiling(&sde, 900_003), None);
    assert_eq!(ice_type_security_ceiling(&sde, 900_004), Some(0.4));
    assert_eq!(ice_type_security_ceiling(&sde, 900_005), Some(-0.1));

    for (did, name, arch) in [
        (10, "NonIce", 27),
        (20, "NoAstEntry", DUNGEON_ARCHETYPE_ICE_BELT),
        (21, "EmptyAst", DUNGEON_ARCHETYPE_ICE_BELT),
        (22, "StormIce", DUNGEON_ARCHETYPE_ICE_BELT),
        (25, "LowsecBelt", DUNGEON_ARCHETYPE_ICE_BELT),
        (23, "NullsecBelt", DUNGEON_ARCHETYPE_ICE_BELT),
    ] {
        sde.dungeons.insert(
            did,
            DungeonTemplate {
                dungeon_id: did,
                name: name.into(),
                archetype_id: Some(arch),
                faction_id: None,
                difficulty: None,
            },
        );
    }
    sde.dungeon_asteroids.insert(21, Vec::new());
    sde.dungeon_asteroids.insert(
        22,
        vec![DungeonAsteroidEntry { room_id: 1, type_id: 900_001, asteroid_count: 5 }],
    );
    sde.dungeon_asteroids.insert(
        25,
        vec![DungeonAsteroidEntry { room_id: 1, type_id: 900_004, asteroid_count: 8 }],
    );
    sde.dungeon_asteroids.insert(
        23,
        vec![
            DungeonAsteroidEntry { room_id: 1, type_id: 900_004, asteroid_count: 4 },
            DungeonAsteroidEntry { room_id: 1, type_id: 900_005, asteroid_count: 4 },
        ],
    );

    assert!(eligible_ice_belts(&sde, 0.5).is_empty());
    assert_eq!(eligible_ice_belts(&sde, 0.4), vec![(25, 0.4)]);
    assert_eq!(eligible_ice_belts(&sde, -0.1), vec![(23, -0.1), (25, 0.4)]);

    assert_eq!(parse_dng_site_id(""), None);
    assert_eq!(parse_dng_site_id("DNG-123"), None);
    assert_eq!(parse_dng_site_id("DNG-abc-12"), None);
    assert_eq!(parse_dng_site_id("DNG-123-xyz"), None);
    assert_eq!(parse_dng_site_id("DNG-77-999"), Some((77, 999)));

    let id_a = registry_instance_id("SITE-A");
    let id_b = registry_instance_id("SITE-B");
    assert_eq!(id_a, registry_instance_id("SITE-A"));
    assert_ne!(id_a, id_b);
    assert!(id_a > 0 && id_a <= 0x001f_ffff_ffff_ffff);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn all_archetype_labels_dungeons_filters_and_rooms_branches() {
    let mut sde = (*shared_sde()).clone();
    let all_aids: &[u32] = &[
        0, 1, 2, 3, 4, 5, 6, 8, 12, 14, 16, 19, 20, 21, 22, 24, 25, 26, 27, 28, 29, 30, 31, 32, 33,
        34, 35, 36, 37, 38, 39, 43, 44, 45, 46, 47, 48, 49, 50, 51, 60, 61, 62, 63, 64, 65, 66, 67,
        68, 70, 71, 72, 73, 74, 75, 76, 77, 78, 79, 80, 81, 99,
    ];
    for &aid in all_aids {
        for (base, prefix) in [(900_000, "LaterSample"), (800_000, "MinSample")] {
            sde.dungeons.insert(
                base + aid,
                DungeonTemplate {
                    dungeon_id: base + aid,
                    name: format!("{prefix}-{aid}"),
                    archetype_id: Some(aid),
                    faction_id: Some(500_001),
                    difficulty: Some(1),
                },
            );
        }
    }

    let db = Arc::new(WorldRepository::new_empty());
    let char_id = 90_060_101;
    db.provision_gm_account("gm_arch", "pw", char_id, "GmArch");
    let ctx = ServerContext::with_sde_and_repository(Arc::new(sde), db);

    assert_eq!(ice_belt_dungeon_for_system(&ctx, 31_000_000), None);
    assert_eq!(ice_belt_dungeon_for_system(&ctx, 99_999_999), None);
    seed_system_ice_belt_from_sde(&ctx, 31_000_000);
    assert!(ctx.db.with_snapshot(|s| !s.cosmic_anomalies.contains_key(&31_000_000)));

    let ice_sys = ctx
        .sde
        .solar_systems
        .keys()
        .copied()
        .find(|&sid| ice_belt_dungeon_for_system(&ctx, sid).is_some())
        .unwrap();
    seed_system_ice_belt_from_sde(&ctx, ice_sys);
    let count_first = ctx.db.with_snapshot(|s| s.cosmic_anomalies.get(&ice_sys).map(|v| v.len()).unwrap_or(0));
    seed_system_ice_belt_from_sde(&ctx, ice_sys);
    let count_second = ctx.db.with_snapshot(|s| s.cosmic_anomalies.get(&ice_sys).map(|v| v.len()).unwrap_or(0));
    assert_eq!(count_first, count_second);

    let mut unauth = SessionState::new(1);
    let mut non_gm = SessionState::new(2);
    non_gm.user_id = Some(1);
    non_gm.character_id = Some(char_id);
    non_gm.solar_system_id = Some(30_000_142);
    non_gm.role = 0;

    let mut gm = non_gm.clone();
    gm.role = service_roles::ROLE_GM_MASK;
    let kw = HashMap::new();

    let ins_role = "InsufficientRole";
    for m in ["DEGetArchetypes", "DEGetDungeons", "DEGetFactions", "DEGetRoomObjectPaletteData", "DEGetRooms"] {
        assert!(!call_cache_err(&ctx, &mut unauth, "dungeon", m, &[], &kw).await.code.is_empty());
        assert_eq!(call_cache_err(&ctx, &mut non_gm, "dungeon", m, &[], &kw).await.code, ins_role);
    }

    let arch_rep = handle(&ctx, &mut gm, "dungeon", "DEGetArchetypes", &[], &kw).await.unwrap().unwrap();
    let arch_debug = format!("{arch_rep:?}");
    assert!(arch_debug.contains("Bulwark / Phased Field Mining Site"));
    assert!(arch_debug.contains("Sansha Incursion Site (60)"));
    assert!(arch_debug.contains("Pirate Insurgency Outpost Raid (75)"));
    assert!(arch_debug.contains("Archetype 99 (MinSample-99)"));

    let mut filter_kw = HashMap::new();
    filter_kw.insert("dungeonVID".into(), PyRep::Integer(800_081));
    filter_kw.insert("archetypeID".into(), PyRep::Integer(81));
    filter_kw.insert("factionID".into(), PyRep::Integer(500_001));
    let filtered = handle(&ctx, &mut gm, "dungeon", "DEGetDungeons", &[], &filter_kw).await.unwrap().unwrap();
    assert!(format!("{filtered:?}").contains("MinSample-81"));

    assert_eq!(call_cache_err(&ctx, &mut gm, "dungeon", "DEGetRooms", &[PyRep::Integer(0)], &kw).await.code, "DungeonIDRequired");
    assert_eq!(call_cache_err(&ctx, &mut gm, "dungeon", "DEGetRooms", &[PyRep::Integer(999_999_999)], &kw).await.code, "DungeonNotFound");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn keeper_and_agency_warp_to_content_branches() {
    let sys_id: SolarSystemId = 30_000_142;
    let char_id: i64 = 90_060_202;
    let ship_id: i64 = 1_060_202_001;
    let db = Arc::new(WorldRepository::new_empty());
    db.provision_gm_account("gm_keeper", "pw", char_id, "GmKeeper");
    let ctx = ServerContext::with_sde_and_repository(shared_sde(), db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_775_800_000)));

    let mut unauth = SessionState::new(10);
    let mut non_gm = SessionState::new(11);
    non_gm.user_id = Some(1);
    non_gm.character_id = Some(char_id);
    non_gm.solar_system_id = Some(sys_id);
    non_gm.role = 0;

    let mut gm = non_gm.clone();
    gm.role = service_roles::ROLE_GM_MASK;
    let kw = HashMap::new();

    assert_eq!(handle(&ctx, &mut gm, "keeper", "GetCurrentRoomID", &[], &kw).await.unwrap().unwrap(), PyRep::None);
    assert_eq!(handle(&ctx, &mut non_gm, "keeper", "GetCurrentDungeonForCharacter", &[], &kw).await.unwrap().unwrap(), PyRep::None);

    let room_rec = AbyssalRoomRecord {
        room_index: 1,
        dungeon_id: 5001,
        dungeon_name: "Abyssal Room 1".into(),
        npc_ball_ids: Vec::new(),
        cache_item_id: 0,
        cleared: false,
        gate_unlocked_at_sec: None,
    };
    with_pve_special_reg(&ctx, |reg| {
        reg.abyssal_pockets.insert(
            char_id,
            AbyssalPocketSession {
                pocket_id: 777_001,
                character_id: char_id,
                ship_id,
                origin_solar_system_id: sys_id,
                abyssal_solar_system_id: 32_000_001,
                origin_position: [0.0; 3],
                grid_offset: [0.0; 3],
                filament_type_id: 47763,
                tier: 1,
                weather: AbyssalWeatherKind::Electrical,
                weather_penalty_pct: -30.0,
                weather_bonus_pct: 50.0,
                npc_difficulty_mult: 1.0,
                current_room: 2,
                rooms: [room_rec.clone(), room_rec.clone(), room_rec],
                started_at_sec: 1_000,
                collapse_at_sec: 2_200,
                saved_ship_attrs: HashMap::new(),
                completed: false,
                collapsed_destroyed: false,
                trace_ball_id: 0,
                fleet_ship_ids: vec![ship_id],
                ship_room: HashMap::new(),
                max_ships: 1,
            },
        );
    });
    let active_dng = handle(&ctx, &mut non_gm, "keeper", "GetCurrentDungeonForCharacter", &[], &kw).await.unwrap().unwrap();
    if let PyRep::Tuple(items) = active_dng {
        assert_eq!(items[0], PyRep::Integer(5001));
        assert_eq!(items[1], PyRep::Integer(500_102));
        assert_eq!(items[2], PyRep::Integer(777_001));
    } else {
        panic!("expected Tuple");
    }

    assert_eq!(call_cache_err(&ctx, &mut non_gm, "keeper", "GetLevelEditor", &[], &kw).await.code, "InsufficientRole");
    let le_ok = handle(&ctx, &mut gm, "keeper", "GetLevelEditor", &[], &kw).await.unwrap().unwrap();
    assert_eq!(le_ok.dict_get("isEditorActive"), Some(&PyRep::Bool(true)));

    assert_eq!(call_cache_err(&ctx, &mut non_gm, "keeper", "GetOperationSpawnpointSolarSystem", &[PyRep::Integer(90_099_999)], &kw).await.code, "UserError");
    assert_eq!(handle(&ctx, &mut gm, "keeper", "GetOperationSpawnpointSolarSystem", &[PyRep::Integer(90_099_999), PyRep::Integer(1)], &kw).await.unwrap().unwrap(), PyRep::None);
    assert_eq!(handle(&ctx, &mut non_gm, "keeper", "IsOperationSite", &[], &kw).await.unwrap().unwrap(), PyRep::Bool(false));

    let cap = ["custom", "agency", "provider"].join("_");
    let _ = call_cache_err(&ctx, &mut unauth, &cap, "get_content_data", &[], &kw).await;
    let mut no_sys = non_gm.clone();
    no_sys.solar_system_id = None;
    assert_eq!(call_cache_err(&ctx, &mut no_sys, &cap, "get_content_data", &[], &kw).await.code, "UnknownSolarSystem");
    assert_eq!(call_cache_err(&ctx, &mut no_sys, &cap, "warp_to_content", &[], &kw).await.code, "UnknownSolarSystem");

    let mut docked = non_gm.clone();
    docked.station_id = Some(60_003_760);
    assert_eq!(call_cache_err(&ctx, &mut docked, &cap, "warp_to_content", &[], &kw).await.code, "NotInSpace");
    assert_eq!(call_cache_err(&ctx, &mut non_gm, &cap, "warp_to_content", &[], &kw).await.code, "NoActiveShip");

    non_gm.ship_id = Some(ship_id);
    assert_eq!(call_cache_err(&ctx, &mut non_gm, &cap, "warp_to_content", &[], &kw).await.code, "InvalidArgument");
    assert_eq!(call_cache_err(&ctx, &mut non_gm, &cap, "warp_to_content", &[PyRep::Integer(999_888_777)], &kw).await.code, "SiteNotFound");

    ctx.db.with_mut_snapshot(|snap| {
        let list = snap.cosmic_anomalies.entry(sys_id).or_default();
        list.push(make_anom("DNG-100-900001", sys_id, "OreAnomaly", [f64::NAN, 0.0, 0.0], 1000, 0, false));
        list.push(make_anom("DNG-100-900002", sys_id, "OreAnomaly", [50_000.0, 0.0, 0.0], 1000, 0, false));
        snap.cosmic_signatures.entry(sys_id).or_default().push(CosmicSignatureRecord {
            signature_id: "DNG-101-900003".into(),
            solar_system_id: sys_id,
            sig_group: "Gas".into(),
            type_name: "FarSig".into(),
            position: [500_000.0, 0.0, 0.0],
            signal_strength: 1.0,
            wormhole_type_id: None,
            destination_system_id: None,
            created_unix: 1000,
            lifetime_seconds: 0,
            max_mass_kg: 0.0,
            remaining_mass_kg: 0.0,
            max_jump_mass_kg: 0.0,
            collapsed: false,
        });
    });

    assert_eq!(call_cache_err(&ctx, &mut non_gm, &cap, "warp_to_content", &[PyRep::Integer(900_001)], &kw).await.code, "InvalidCoordinates");
    assert_eq!(call_cache_err(&ctx, &mut non_gm, &cap, "warp_to_content", &[PyRep::Integer(900_002)], &kw).await.code, "ShipNotInSpace");

    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_h
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 587,
            position: DVec3::ZERO,
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;

    assert_eq!(call_cache_err(&ctx, &mut non_gm, &cap, "warp_to_content", &[PyRep::Integer(900_002)], &kw).await.code, "WarpDistanceTooShort");
    let warp_sig = handle(&ctx, &mut non_gm, &cap, "warp_to_content", &[PyRep::keyval([("dungeonID", PyRep::Integer(101))])], &kw).await.unwrap().unwrap();
    assert_eq!(warp_sig, PyRep::None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bulwark_fw_instances_range_bfs_and_signature_filtering() {
    let sys_a: SolarSystemId = 30_000_142;
    let sys_b: SolarSystemId = 30_000_144;
    let char_id: i64 = 90_060_303;
    let mut sde = (*shared_sde()).clone();
    sde.jumps.insert(
        990_001,
        StargateJump {
            stargate_id: 990_001,
            from_solar_system_id: sys_a,
            to_solar_system_id: sys_b,
            destination_stargate_id: 990_002,
            position: DVec3::ZERO,
        },
    );
    sde.dungeons.insert(
        810_001,
        DungeonTemplate {
            dungeon_id: 810_001,
            name: "Bulwark Archetype 81".into(),
            archetype_id: Some(81),
            faction_id: None,
            difficulty: None,
        },
    );

    let db = Arc::new(WorldRepository::new_empty());
    db.provision_gm_account("gm_fw", "pw", char_id, "GmFw");
    let ctx = ServerContext::with_sde_and_repository(Arc::new(sde), db);
    let now = 1_775_900_000;
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(now)));

    let mut sess = SessionState::new(20);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.solar_system_id = Some(sys_a);
    let kw = HashMap::new();

    assert!(handle(&ctx, &mut sess, "dungeonInstanceCacheMgr", "GetFactionWarfareBattlefieldInstances", &[PyRep::Integer(sys_a as i64)], &kw).await.is_none());

    ctx.db.with_mut_snapshot(|snap| {
        let list = snap.cosmic_anomalies.entry(sys_a).or_default();
        list.push(make_anom("DNG-10-1", sys_a, "BulwarkMiningSite", [0.0; 3], now - 50, 100, false));
        list.push(make_anom("DNG-810001-2", sys_a, "Other", [0.0; 3], now, 0, false));
        list.push(make_anom("DNG-10-3", sys_a, "BulwarkMiningSite", [0.0; 3], now - 200, 100, false));
        list.push(make_anom("DNG-10-4", sys_a, "BulwarkMiningSite", [0.0; 3], now, 0, true));

        for (cid, tier) in [(701, "Novice"), (702, "Small"), (703, "Medium"), (704, "Large"), (705, "Open")] {
            snap.fw_runtime.fw_complexes.insert(cid, (sys_a, tier.into(), 500_001, 0.0, false));
        }
        snap.fw_runtime.battlefield_instances.push(PersistedBattlefieldRecord {
            instance_id: 888,
            solar_system_id: sys_a,
            faction_id: 500_001,
            name: "Battlefield".into(),
            capture_progress: 0.0,
            completed: false,
        });
    });

    let bulwark_rep = handle(&ctx, &mut sess, "dungeonInstanceCacheMgr", "GetBulwarkMiningSiteCounts", &[], &kw).await.unwrap().unwrap();
    if let PyRep::Dict(entries) = bulwark_rep {
        let count_a = entries.iter().find(|(k, _)| k.as_i64() == Some(sys_a as i64)).and_then(|(_, v)| v.as_i64());
        assert_eq!(count_a, Some(2));
    } else {
        panic!("expected Dict");
    }

    let fw_rep = handle(&ctx, &mut sess, "dungeonInstanceCacheMgr", "GetFactionWarfareInstances", &[], &kw).await.unwrap().unwrap();
    let fw_dbg = format!("{fw_rep:?}");
    assert!(fw_dbg.contains("701") && fw_dbg.contains("705") && fw_dbg.contains("888"));

    let range_0 = handle(&ctx, &mut sess, "dungeonInstanceCacheMgr", "GetOreAnomaliesCountInRange", &[PyRep::Integer(0)], &kw).await.unwrap().unwrap();
    assert!(matches!(range_0, PyRep::Dict(ref e) if e.len() == 1));

    let mut range_kw = HashMap::new();
    range_kw.insert("maxJumps".into(), PyRep::Integer(1));
    let range_1 = handle(&ctx, &mut sess, "dungeonInstanceCacheMgr", "GetOreAnomaliesCountInRange", &[], &range_kw).await.unwrap().unwrap();
    assert!(matches!(range_1, PyRep::Dict(ref e) if e.len() >= 2));

    with_pve_exploration_reg(&ctx, |reg| {
        let now_reg = crate::services::pve_exploration::now_sec(reg);
        reg.sites_by_system.entry(sys_a).or_default().push(PveSiteInstance {
            site_id: "EXP-SITE-1".into(),
            dungeon_id: 999_111,
            solar_system_id: sys_a,
            probe_required: false,
            cleared: false,
            created_unix: now_reg.saturating_sub(500),
            lifetime_seconds: 100,
            ..Default::default()
        });
    });
    let combat_rep = handle(&ctx, &mut sess, "dungeonInstanceCacheMgr", "GetCombatAnomalyInstances", &[], &kw).await.unwrap().unwrap();
    assert!(!format!("{combat_rep:?}").contains("999111"));
}
