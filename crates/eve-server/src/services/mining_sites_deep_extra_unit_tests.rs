use crate::rng::MechanicRng;
use crate::router::{dispatch::tick_sov_prospecting_deposit_respawn, ServerContext};
use crate::services::mining_sites::{
    displayed_security, is_designated_ice_system, is_ice_belt_ready_to_respawn,
    is_ore_anomaly_expired, resolve_gas_site, resolve_regional_ice_belt, resolve_wormhole_gas_site,
    SiteBand, GAS_SITES, ICE_BELT_RESPAWN_DELAY_SECS, ICE_BELT_SITES, ORE_ANOMALY_EXPIRY_SECS,
    WORMHOLE_GAS_SITES,
};
use crate::services::space_mining_harvest::{
    build_wormhole_guard_wave, collect_persisted_wormhole_guard_sites,
    hydrate_wormhole_ore_guards_if_needed, on_wormhole_ore_guard_destroyed_db,
    record_initial_wormhole_guard_wave, resolve_wormhole_ore_site_with_dungeon,
    sample_wormhole_ore_sites_for_system, tick_all_ice_belt_respawns_sync,
    tick_ice_belt_respawn_sync, wormhole_ore_sites_for_class, PhasedAsteroidFieldState,
    WORMHOLE_ORE_SITES,
};
use eve_db::{
    AsteroidRecord, CharacterRecord, CorporationRecord, CosmicAnomalyRecord, InventoryFlag,
    InventoryItemRecord, NeuralAttributes, ServerClock, UpwellStructureRecord,
    VirtualServerClock, WorldRepository,
};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::{DungeonAsteroidEntry, DungeonTemplate, InvType, SdeRegistry, TypeId};
use std::collections::HashMap;
use std::sync::Arc;

fn build_mining_test_sde() -> Arc<SdeRegistry> {
    let mut sde = SdeRegistry::with_canonical_seed();
    let ensure_type = |sde: &mut SdeRegistry, tid: TypeId, group_id: u32, name: &str, vol: f64| {
        sde.types.entry(tid).or_insert(InvType {
            type_id: tid,
            group_id,
            category_id: if group_id == 4768 { 65 } else { 25 },
            name: name.to_string(),
            mass: 1_000.0,
            volume: vol,
            capacity: 0.0,
            radius: 100.0,
            base_price: 100.0,
            published: true,
            portion_size: 100,
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
            description: String::new(),
            effect_ids: Vec::new(),
            default_attributes: HashMap::new(),
        });
    };
    ensure_type(&mut sde, 1230, 462, "Veldspar", 0.1);
    ensure_type(&mut sde, 1228, 460, "Scordite", 0.15);
    for (tid, name) in [
        (16262, "Clear Icicle"),
        (16263, "Glacial Mass"),
        (16264, "Blue Ice"),
        (16265, "White Glaze"),
        (16266, "Glare Crust"),
        (16267, "Dark Glitter"),
        (16268, "Gelidus"),
        (16269, "Krystallos"),
        (17975, "Thick Blue Ice"),
        (17976, "Pristine White Glaze"),
        (17977, "Smooth Glacial Mass"),
        (17978, "Enriched Clear Icicle"),
    ] {
        ensure_type(&mut sde, tid, 465, name, 1000.0);
    }
    ensure_type(&mut sde, 25268, 711, "Fullerite-C50", 1.0);
    ensure_type(&mut sde, 82589, 4768, "Prospecting Array 2", 4000.0);
    ensure_type(&mut sde, 46676, 462, "Bezdnacine", 16.0);

    let insert_dungeon =
        |sde: &mut SdeRegistry, id: u32, name: &str, ores: Vec<(TypeId, u32)>, gas: Vec<TypeId>| {
            sde.dungeons.insert(
                id,
                DungeonTemplate {
                    dungeon_id: id,
                    name: name.to_string(),
                    archetype_id: None,
                    faction_id: None,
                    difficulty: Some(2),
                },
            );
            if !ores.is_empty() {
                sde.dungeon_asteroids.insert(
                    id,
                    ores.into_iter()
                        .map(|(type_id, asteroid_count)| DungeonAsteroidEntry {
                            room_id: 1,
                            type_id,
                            asteroid_count,
                        })
                        .collect(),
                );
            }
            if !gas.is_empty() {
                sde.dungeon_gas_types.insert(id, gas);
            }
        };

    insert_dungeon(
        &mut sde,
        990_001,
        "Asteroid Cluster",
        vec![(1230, 4), (1228, 2)],
        Vec::new(),
    );
    if let Some(ice_def) = ICE_BELT_SITES.first() {
        insert_dungeon(
            &mut sde,
            990_002,
            ice_def.site.dungeon_name,
            vec![(16262, 6)],
            Vec::new(),
        );
    }
    if let Some(gas_def) = GAS_SITES.first() {
        insert_dungeon(
            &mut sde,
            990_003,
            gas_def.dungeon_name,
            Vec::new(),
            vec![25268],
        );
    }
    if let Some(wh_gas_def) = WORMHOLE_GAS_SITES.first() {
        insert_dungeon(
            &mut sde,
            990_004,
            wh_gas_def.dungeon_name,
            Vec::new(),
            vec![25268],
        );
    }
    if let Some(wh_ore_def) = WORMHOLE_ORE_SITES.first() {
        insert_dungeon(
            &mut sde,
            990_005,
            wh_ore_def.dungeon_name,
            vec![(1230, 3)],
            Vec::new(),
        );
    }
    insert_dungeon(
        &mut sde,
        825890,
        "Large Mordunium Deposit",
        vec![(46676, 3)],
        Vec::new(),
    );
    Arc::new(sde)
}

#[test]
fn test_displayed_security_and_site_band_matching_boundaries() {
    assert!((displayed_security(0.449) - 0.4).abs() < 1e-9);
    assert!((displayed_security(0.45) - 0.5).abs() < 1e-9);
    assert!((displayed_security(-0.049) - 0.0).abs() < 1e-9);
    assert!((displayed_security(-0.05) - (-0.1)).abs() < 1e-9);

    let hs = SiteBand::HighSec;
    assert!(!hs.matches(30_000_142, 0.44));
    assert!(hs.matches(30_000_142, 0.45));
    assert!(hs.matches(30_000_142, 0.74));
    assert!(hs.matches(30_000_142, 0.85));
    assert!(!hs.matches(31_000_001, 0.5));

    let hs_mid = SiteBand::HighSecMid;
    assert!(hs_mid.matches(30_000_142, 0.5));
    assert!(hs_mid.matches(30_000_142, 0.84));
    assert!(!hs_mid.matches(30_000_142, 0.85));

    let ls = SiteBand::LowSec;
    assert!(!ls.matches(30_002_510, 0.0));
    assert!(ls.matches(30_002_510, 0.001));
    assert!(ls.matches(30_002_510, 0.06));
    assert!(ls.matches(30_002_510, 0.44));
    assert!(!ls.matches(30_002_510, 0.45));
    assert!(!ls.matches(31_000_001, 0.25));

    let ns = SiteBand::NullSec;
    assert!(ns.matches(30_004_759, -0.95));
    assert!(ns.matches(30_004_759, 0.0));
    assert!(ns.matches(30_004_759, -0.049));
    assert!(!ns.matches(30_004_759, 0.001));
    assert!(!ns.matches(30_004_759, 0.051));
    assert!(!ns.matches(31_000_001, -0.5));
}

#[test]
fn test_cosmic_site_timers_designated_ice_and_gas_resolution() {
    assert!(!is_ore_anomaly_expired(ORE_ANOMALY_EXPIRY_SECS - 1));
    assert!(is_ore_anomaly_expired(ORE_ANOMALY_EXPIRY_SECS));
    assert!(is_ore_anomaly_expired(ORE_ANOMALY_EXPIRY_SECS + 1));

    assert!(!is_ice_belt_ready_to_respawn(ICE_BELT_RESPAWN_DELAY_SECS - 1));
    assert!(is_ice_belt_ready_to_respawn(ICE_BELT_RESPAWN_DELAY_SECS));
    assert!(is_ice_belt_ready_to_respawn(ICE_BELT_RESPAWN_DELAY_SECS + 1));

    assert!(is_designated_ice_system(30_000_180));
    assert!(is_designated_ice_system(30_000_156));
    assert!(!is_designated_ice_system(30_000_142));
    assert!(!is_designated_ice_system(31_000_005));

    let sde = build_mining_test_sde();
    let gas_resolved = resolve_gas_site(&sde, &GAS_SITES[0]).expect("gas site resolves");
    assert!(gas_resolved.gas_type_id > 0);
    assert_eq!(gas_resolved.cloud_units, GAS_SITES[0].cloud_units);

    let (wh_dungeon_id, wh_clouds) =
        resolve_wormhole_gas_site(&sde, &WORMHOLE_GAS_SITES[0]).expect("wh gas resolves");
    assert!(wh_dungeon_id > 0 && !wh_clouds.is_empty());

    assert!(resolve_regional_ice_belt(&sde, 31_000_005).is_none());
    let ice_sys = sde
        .solar_systems
        .values()
        .find(|sys| {
            is_designated_ice_system(sys.solar_system_id)
                && sde
                    .regions
                    .get(&sys.region_id)
                    .is_some_and(|r| ICE_BELT_SITES[0].region_names.contains(&r.name.as_str()))
        })
        .map(|s| s.solar_system_id)
        .unwrap_or(30_000_156);
    let ice_res = resolve_regional_ice_belt(&sde, ice_sys);
    assert!(ice_res.is_some());

    let (d_id, rocks) =
        resolve_wormhole_ore_site_with_dungeon(&sde, &WORMHOLE_ORE_SITES[0]).expect("wh ore");
    assert!(d_id > 0);
    assert!(!rocks.is_empty());
}

#[tokio::test]
async fn test_ice_belt_respawn_timing_and_signature_registration() {
    let sde = build_mining_test_sde();
    let db = Arc::new(WorldRepository::new_empty());
    let clock = Arc::new(VirtualServerClock::with_fixed_base(10_000));
    let ctx = ServerContext::with_sde_and_repository(sde, db.clone());
    ctx.set_clock(clock.clone());
    let ice_sys = 30_000_156u32;

    db.with_mut_snapshot(|snap| {
        snap.cosmic_anomalies.insert(
            ice_sys,
            vec![CosmicAnomalyRecord {
                anomaly_id: "ICE-1".into(),
                solar_system_id: ice_sys,
                name: ICE_BELT_SITES[0].site.dungeon_name.into(),
                anomaly_type: "IceBelt".into(),
                position: [1_000.0, 2_000.0, 3_000.0],
                signal_strength: 1.0,
                ore_types: vec![16262],
                remaining_volume: 0.0,
                created_unix: 10_000,
                lifetime_seconds: ICE_BELT_RESPAWN_DELAY_SECS,
                cleared: true,
            }],
        );
    });

    clock.advance_secs(ICE_BELT_RESPAWN_DELAY_SECS - 1);
    tick_ice_belt_respawn_sync(&ctx, ice_sys);
    db.with_snapshot(|snap| {
        assert!(
            snap.cosmic_anomalies[&ice_sys][0].cleared,
            "ice belt must not respawn 1s before 4h timer"
        );
    });

    clock.advance_secs(1);
    tick_all_ice_belt_respawns_sync(&ctx);
    db.with_snapshot(|snap| {
        let anom = &snap.cosmic_anomalies[&ice_sys][0];
        assert!(!anom.cleared, "ice belt must respawn at exact 4h threshold");
        assert!(anom.remaining_volume > 0.0);
        let spawned_ice: Vec<&AsteroidRecord> = snap
            .asteroids
            .values()
            .filter(|a| a.solar_system_id == ice_sys)
            .collect();
        assert!(!spawned_ice.is_empty());
        assert!(snap
            .cosmic_signatures
            .get(&ice_sys)
            .is_some_and(|sigs| !sigs.is_empty()));
    });
}

#[tokio::test]
async fn test_sov_hub_prospecting_deposit_seeding_and_respawn_timer() {
    let mut base_sde = (*build_mining_test_sde()).clone();
    let null_sys_id = 30_004_759u32;
    if let Some(sys) = base_sde.solar_systems.get_mut(&null_sys_id) {
        sys.security_status = -0.35;
    }
    let db = Arc::new(WorldRepository::new_empty());
    let clock = Arc::new(VirtualServerClock::with_fixed_base(50_000));
    let ctx = ServerContext::with_sde_and_repository(Arc::new(base_sde), db.clone());
    ctx.set_clock(clock.clone());
    let char_id = 90_010_001i64;
    let corp_id = 98_010_001i64;
    let alliance_id = 99_010_001i64;
    let hub_id = 1_010_000_001i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.insert(
            char_id,
            CharacterRecord {
                activity_status: None,
                character_id: char_id,
                user_id: 1,
                name: "SovDirector".into(),
                corporation_id: corp_id,
                alliance_id: Some(alliance_id),
                war_faction_id: None,
                station_id: None,
                structure_id: Some(hub_id),
                solar_system_id: null_sys_id,
                constellation_id: 20_000_001,
                region_id: 10_000_001,
                active_ship_id: 1_000_001,
                balance: Default::default(),
                plex_balance: 0,
                loyalty_points: HashMap::new(),
                standings: HashMap::new(),
                security_status: 5.0,
                clone_grade: 2,
                home_station_id: 60_003_760,
                race_id: 1,
                bloodline_id: 1,
                ancestry_id: 1,
                school_id: 1,
                neural: NeuralAttributes {
                    intelligence: 20,
                    memory: 20,
                    perception: 20,
                    willpower: 20,
                    charisma: 19,
                    bonus_remaps: 0,
                    last_remap_unix: None,
                    remap_cooldown_until_unix: None,
                },
                skill_points: 0.0,
                free_skill_points: 0.0,
                last_clone_jump_unix: 0,
                jump_fatigue_minutes: 0.0,
                jump_cooldown_until_unix: 0,
                jump_fatigue_until_unix: 0,
                description: String::new(),
                title: String::new(),
                gender: 1,
                create_date_unix: 0,
                fleet_id: None,
                autopilot_waypoints: Vec::new(),
                version: 1,
                system_epoch: 1,
            },
        );
        snap.corporations.insert(
            corp_id,
            { let mut __corp = CorporationRecord::default();
                __corp.corporation_id = corp_id;
                __corp.name = "SovCorp".into();
                __corp.ticker = "SOVC".into();
                __corp.ceo_id = char_id;
                __corp.alliance_id = Some(alliance_id);
                __corp },
        );
        snap.system_states.entry(null_sys_id).or_default().sov_alliance_id = Some(alliance_id);
        snap.structures.insert(
            hub_id,
            UpwellStructureRecord {
                structure_id: hub_id,
                type_id: 32458,
                owner_corp_id: corp_id,
                solar_system_id: null_sys_id,
                name: "IHub-1".into(),
                position: [500_000.0, 0.0, 500_000.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 500,
                ..Default::default()
            },
        );
        snap.items.insert(
            90_000_501,
            InventoryItemRecord {
                item_id: 90_000_501,
                type_id: 82589,
                owner_id: corp_id,
                location_id: hub_id,
                flag: InventoryFlag::CorpSAG1,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });

    let mut session = SessionState::new(1);
    session.user_id = Some(1);
    session.character_id = Some(char_id);
    session.corporation_id = Some(corp_id);
    session.alliance_id = Some(alliance_id);
    session.solar_system_id = Some(null_sys_id);
    session.corp_role = eve_db::corp_roles::ROLE_DIRECTOR;

    let save_res = crate::services::mining_sites::handle(
        &ctx,
        &mut session,
        "sovHubSvc",
        "SaveUpgradeConfiguration",
        &[PyRep::Integer(hub_id), PyRep::List(vec![PyRep::Integer(82589)])],
        &HashMap::new(),
    )
    .await
    .expect("handled")
    .expect("save ok");
    assert_eq!(save_res.dict_get("prospectingLevel").and_then(|v| v.as_i64()), Some(1));
    assert!(matches!(
        save_res.dict_get("spawnedAnomalies"),
        Some(PyRep::List(l)) if l.len() == 1
    ));

    let local_res = crate::services::mining_sites::handle(
        &ctx,
        &mut session,
        "sovHubSvc",
        "GetInstalledUpgradesLocal",
        &[],
        &HashMap::new(),
    )
    .await
    .expect("handled")
    .expect("local ok");
    assert_eq!(local_res.dict_get("hubID").and_then(|v| v.as_i64()), Some(hub_id));
    assert_eq!(local_res.dict_get("prospectingLevel").and_then(|v| v.as_i64()), Some(1));

    ctx.db.with_mut_snapshot(|snap| {
        snap.asteroids.retain(|_, a| a.solar_system_id != null_sys_id);
    });
    tick_sov_prospecting_deposit_respawn(&ctx, null_sys_id).await;
    let respawn_secs = ctx.db.with_snapshot(|snap| {
        snap.cosmic_anomalies
            .get(&null_sys_id)
            .and_then(|list| list.first())
            .filter(|a| a.cleared)
            .map(|a| a.lifetime_seconds)
            .expect("depleted sov deposit must transition to cleared with respawn lifetime")
    });

    clock.advance_secs(respawn_secs - 1);
    tick_sov_prospecting_deposit_respawn(&ctx, null_sys_id).await;
    ctx.db.with_snapshot(|snap| {
        assert!(
            snap.cosmic_anomalies[&null_sys_id].iter().all(|a| a.cleared),
            "sov deposit must not respawn 1s early"
        );
    });

    clock.advance_secs(1);
    tick_sov_prospecting_deposit_respawn(&ctx, null_sys_id).await;
    ctx.db.with_snapshot(|snap| {
        let active_count = snap.cosmic_anomalies[&null_sys_id]
            .iter()
            .filter(|a| a.anomaly_type == "SovOre" && !a.cleared)
            .count();
        assert_eq!(active_count, 1, "sov deposit must respawn at respawn threshold");
    });
}

#[tokio::test]
async fn test_wormhole_ore_classes_sampling_and_guard_wave_persistence() {
    let c13 = wormhole_ore_sites_for_class(13);
    assert!(c13.iter().any(|d| d.dungeon_name == "Shattered Debris Field"));
    assert!(!c13
        .iter()
        .any(|d| d.wh_class_min >= 4 && d.dungeon_name != "Shattered Debris Field"));
    let fallback = wormhole_ore_sites_for_class(0);
    assert_eq!(fallback.len(), wormhole_ore_sites_for_class(3).len());

    let sde = build_mining_test_sde();
    let wh_sys = 31_001_111u32;
    let rng = MechanicRng::seeded(42);
    let sampled = sample_wormhole_ore_sites_for_system(&sde, &rng, wh_sys, 13);
    assert!(!sampled.is_empty() && sampled.len() <= 4);
    assert_eq!(sampled[0].dungeon_name, "Shattered Debris Field");

    let db = Arc::new(WorldRepository::new_empty());
    let clock = Arc::new(VirtualServerClock::with_fixed_base(75_000));
    let ctx = ServerContext::with_sde_and_repository(sde, db.clone());
    ctx.set_clock(clock.clone());
    let mut wave_out = Vec::new();
    let mut next_id = 800_000i64;
    build_wormhole_guard_wave(&mut next_id, [0.0, 0.0, 0.0], &[(30187, 2)], &mut wave_out);
    assert_eq!(wave_out.len(), 2);

    let anom_id = "WH-ORE-1".to_string();
    let recorded_ids: Vec<i64> = db.with_mut_snapshot(|snap| {
        snap.cosmic_anomalies.insert(
            wh_sys,
            vec![CosmicAnomalyRecord {
                anomaly_id: anom_id.clone(),
                solar_system_id: wh_sys,
                name: WORMHOLE_ORE_SITES[0].dungeon_name.into(),
                anomaly_type: "OreAnomaly".into(),
                position: [10_000.0, 0.0, 10_000.0],
                signal_strength: 1.0,
                ore_types: vec![1230],
                remaining_volume: 1_000.0,
                created_unix: 75_000,
                lifetime_seconds: 259_200,
                cleared: false,
            }],
        );
        let persisted = collect_persisted_wormhole_guard_sites(snap, wh_sys);
        assert_eq!(persisted.len(), 1);
        let mut out = Vec::new();
        record_initial_wormhole_guard_wave(snap, &anom_id, [10_000.0, 0.0, 10_000.0], &[(30187, 2)], &mut out);
        out.into_iter().map(|(id, _, _)| id).collect()
    });

    on_wormhole_ore_guard_destroyed_db(&db, recorded_ids[0]);
    hydrate_wormhole_ore_guards_if_needed(&ctx, wh_sys).await;
    let balls = ctx.systems.get_or_spawn(wh_sys).list_balls().await.unwrap();
    assert!(balls.iter().all(|b| b.id != recorded_ids[0]));
    assert!(balls.iter().any(|b| b.id == recorded_ids[1]));
}

#[test]
fn test_phased_asteroid_field_fracture_and_industrial_ship_efficiency() {
    let mut field = PhasedAsteroidFieldState::new("SIG-101".into(), 30_000_142);
    assert!((field.mining_efficiency() - 0.10).abs() < 1e-9);
    field.advance_fracture(0.4);
    assert!(field.beacon_spawned);
    assert!(!field.collapsed);

    assert!(!field.connect_industrial_command_ship(5001, 60));
    assert!(!field.connect_industrial_command_ship(5001, 60));
    assert_eq!(field.mobile_phase_anchor_energy, 60);
    assert!(field.connect_industrial_command_ship(5002, 40));
    assert!((field.mining_efficiency() - 1.0).abs() < 1e-9);

    field.advance_fracture(0.5);
    assert!(field.collapsed);
    assert!(!field.phased_in);
    assert!((field.mining_efficiency() - 0.0).abs() < 1e-9);
    assert!(!field.connect_industrial_command_ship(5003, 200));
}

#[tokio::test]
async fn mining_sites_handle_unknown_none_and_save_upgrade_config_kill_mutants() {
    let sde = build_mining_test_sde();
    let db = Arc::new(WorldRepository::new_empty());
    let ctx = ServerContext::with_sde_and_repository(sde.clone(), db.clone());
    let mut sess = SessionState::new(71);
    sess.user_id = Some(1);
    sess.character_id = Some(90_710_001);

    // 1. Unrelated service returns None (kills `handle -> Some(Ok(Default::default()))`)
    assert!(
        crate::services::mining_sites::handle(
            &ctx,
            &mut sess,
            "marketProxy",
            "GetMarketGroups",
            &[],
            &HashMap::new(),
        )
        .await
        .is_none()
    );

    // 2. NotNullSecSystem error in high-sec (`handle -> None` / `Ok(Default::default())` / delete arm)
    sess.solar_system_id = Some(30_000_142);
    let hi_err = crate::services::mining_sites::handle(
        &ctx,
        &mut sess,
        "sovHubSvc",
        "SaveUpgradeConfiguration",
        &[],
        &HashMap::new(),
    )
    .await
    .expect("sovHubSvc.SaveUpgradeConfiguration handled")
    .unwrap_err();
    assert_eq!(hi_err.code, "NotNullSecSystem");

    // 3. Valid null-sec SaveUpgradeConfiguration spawns anomaly and returns populated keyval
    let null_sys_id = sde
        .solar_systems
        .values()
        .find(|s| s.security_status < -0.1 && s.region_id != 10_000_070 && s.solar_system_id < 31_000_000)
        .map(|s| s.solar_system_id)
        .expect("null-sec system");
    let char_id = 90_710_001_i64;
    let corp_id = 98_710_001_i64;
    let alliance_id = 99_710_001_i64;
    let hub_id = 1_030_710_000_001_i64;
    let ship_id = 1_000_710_001_i64;

    db.with_mut_snapshot(|snap| {
        snap.characters.insert(
            char_id,
            CharacterRecord {
                activity_status: None,
                character_id: char_id,
                user_id: 1,
                name: "SovHubTester".into(),
                corporation_id: corp_id,
                alliance_id: Some(alliance_id),
                war_faction_id: None,
                station_id: None,
                structure_id: Some(hub_id),
                solar_system_id: null_sys_id,
                constellation_id: 20_000_001,
                region_id: 10_000_001,
                active_ship_id: ship_id,
                balance: Default::default(),
                plex_balance: 0,
                loyalty_points: HashMap::new(),
                standings: HashMap::new(),
                security_status: 5.0,
                clone_grade: 2,
                home_station_id: 60_003_760,
                race_id: 1,
                bloodline_id: 1,
                ancestry_id: 1,
                school_id: 1,
                neural: NeuralAttributes {
                    intelligence: 20,
                    memory: 20,
                    perception: 20,
                    willpower: 20,
                    charisma: 19,
                    bonus_remaps: 0,
                    last_remap_unix: None,
                    remap_cooldown_until_unix: None,
                },
                skill_points: 0.0,
                free_skill_points: 0.0,
                last_clone_jump_unix: 0,
                jump_fatigue_minutes: 0.0,
                jump_cooldown_until_unix: 0,
                jump_fatigue_until_unix: 0,
                description: String::new(),
                title: String::new(),
                gender: 1,
                create_date_unix: 0,
                fleet_id: None,
                autopilot_waypoints: Vec::new(),
                version: 1,
                system_epoch: 1,
            },
        );
        let mut corp = CorporationRecord::default();
        corp.corporation_id = corp_id;
        corp.name = "SovCorp".into();
        corp.ticker = "SOVC".into();
        corp.ceo_id = char_id;
        corp.alliance_id = Some(alliance_id);
        snap.corporations.insert(corp_id, corp);
        snap.system_states.entry(null_sys_id).or_default().sov_alliance_id = Some(alliance_id);
        snap.structures.insert(
            hub_id,
            UpwellStructureRecord {
                structure_id: hub_id,
                type_id: 32458,
                solar_system_id: null_sys_id,
                owner_corp_id: corp_id,
                name: "NullHub".into(),
                position: [500_000.0, 0.0, 500_000.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 500,
                ..Default::default()
            },
        );
        snap.items.insert(
            1_000_710_999,
            InventoryItemRecord {
                item_id: 1_000_710_999,
                type_id: 82589,
                owner_id: corp_id,
                location_id: hub_id,
                flag: InventoryFlag::CorpSAG1,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    sess.corporation_id = Some(corp_id);
    sess.alliance_id = Some(alliance_id);
    sess.solar_system_id = Some(null_sys_id);
    sess.corp_role = eve_db::corp_roles::ROLE_DIRECTOR;

    let res = crate::services::mining_sites::handle(
        &ctx,
        &mut sess,
        "sovHubSvc",
        "SaveUpgradeConfiguration",
        &[PyRep::Integer(hub_id), PyRep::List(vec![PyRep::Integer(82589)])],
        &HashMap::new(),
    )
    .await
    .expect("sovHubSvc.SaveUpgradeConfiguration must return Some")
    .expect("null-sec upgrade succeeds");
    assert_eq!(res.dict_get("prospectingLevel").and_then(|v| v.as_i64()), Some(1));
    assert!(db.with_snapshot(|s| s
        .cosmic_anomalies
        .get(&null_sys_id)
        .is_some_and(|list| list.iter().any(|a| a.anomaly_type == "SovOre"))));
}
