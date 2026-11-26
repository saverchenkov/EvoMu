//! Unit and integration tests for `pve_exploration_sites`.

use super::*;
use crate::combat::npc_ai::npc_test_helpers::{get_npc_entity, has_npc_entity, npc_entity_count};
use crate::router::ServerContext;
use crate::services::pve_exploration::{
    ensure_system_pve_sites_seeded, handle_pve_exploration, read_pve_exploration_reg,
    with_pve_exploration_reg, EscalatingPathEntry, HackingGameSession, HackingNodeKind,
    HackingTileState, PveSiteInstance, SpewedLootCan,
};
use eve_db::clock::VirtualServerClock;
use eve_db::{InventoryFlag, InventoryItemRecord, ServerClock, WorldRepository};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::{SdeRegistry, SolarSystemId};
use glam::DVec3;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

#[test]
fn test_loot_generation_ded_restrictions_and_sleeper_tables() {
    let sde = shared_sde();

    // Highsec data & relic loot
    let hs_data = generate_exploration_container_loot(&sde, 30_000_142, false);
    assert!(!hs_data.is_empty());
    let hs_relic = generate_exploration_container_loot(&sde, 30_000_142, true);
    assert!(!hs_relic.is_empty());

    // Lowsec & wormhole deep-space loot includes ancient salvage on relic sites and higher quantities
    let ls_sys = sde
        .solar_systems
        .values()
        .find(|s| s.solar_system_id < 31_000_000 && s.security_status > 0.0 && s.security_status < 0.45)
        .map(|s| s.solar_system_id)
        .unwrap_or(30_000_142);
    let ls_data = generate_exploration_container_loot(&sde, ls_sys, false);
    assert!(!ls_data.is_empty());
    let wh_relic = generate_exploration_container_loot(&sde, 31_000_005, true);
    assert!(wh_relic.len() > hs_relic.len());
    let wh_data = generate_exploration_container_loot(&sde, 31_000_005, false);
    assert!(!wh_data.is_empty());

    // DED ship restrictions
    let (m1, _, g1) = ded_ship_restriction_for_rating(2);
    assert_eq!(m1, 10_000_000.0);
    assert!(g1.contains(&25) && !g1.contains(&26));
    let (m5, _, g5) = ded_ship_restriction_for_rating(5);
    assert_eq!(m5, 75_000_000.0);
    assert!(g5.contains(&26));
    let (m9, _, g9) = ded_ship_restriction_for_rating(9);
    assert_eq!(m9, 250_000_000.0);
    assert!(g9.contains(&27));

    assert_eq!(
        check_ded_gate_ship_allowed(&sde, 2, 999_999_999, 1_000_000.0)
            .unwrap_err()
            .code,
        "ShipSizeRestricted"
    );
    // Frigate (type 587, group 25) allowed in 2/10 when under mass, rejected if over mass
    assert!(check_ded_gate_ship_allowed(&sde, 2, 587, 1_200_000.0).is_ok());
    assert!(check_ded_gate_ship_allowed(&sde, 2, 587, 25_000_000.0).is_err());

    // Overseer personal effects & Sleeper wave/blue loot tables
    let ope1 = overseer_effects_for_rating(&sde, 1);
    let ope10 = overseer_effects_for_rating(&sde, 10);
    assert!(ope1.is_some() && ope10.is_some());
    assert_ne!(ope1.unwrap().0, ope10.unwrap().0);

    for wh_class in 1..=6 {
        let (t1, e1) = sleeper_npc_types_for_class_and_wave(wh_class, 1);
        let (t2, e2) = sleeper_npc_types_for_class_and_wave(wh_class, 2);
        assert!(t1 > 0 && !e1.is_empty());
        assert!(t2 > 0 && !e2.is_empty());
        assert!(sleeper_blue_loot_for_class(&sde, wh_class).is_some());
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_spewed_cans_ded_wrecks_and_sleeper_waves() {
    let sde = shared_sde();
    let db = Arc::new(WorldRepository::new_empty());
    db.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_776_100_000)));
    let ctx = ServerContext::with_sde_and_repository(sde.clone(), db);
    let sys_id: SolarSystemId = 31_000_005;
    let char_id: i64 = 90_100_001;

    // Empty slice early return
    materialize_spewed_cans_core(&ctx.db, &ctx.systems, char_id, &[]).await;

    let cans = vec![SpewedLootCan {
        can_item_id: 1_900_001,
        parent_container_id: 1_800_001,
        solar_system_id: sys_id,
        position: [1_000.0, 2_000.0, -500.0],
        loot_type_id: 34,
        loot_quantity: 250,
        spawned_at_sec: 1_776_100_000,
        despawn_at_sec: 1_776_103_600,
        scooped: false,
    }];
    materialize_spewed_cans_core(&ctx.db, &ctx.systems, char_id, &cans).await;
    ctx.db.with_snapshot(|snap| {
        assert!(snap.items.contains_key(&1_900_001));
        assert!(snap.items.values().any(|i| i.location_id == 1_900_001 && i.quantity == 250));
    });

    // DED overseer wrecks (low & high rating)
    let (wid_low, loot_low) = spawn_ded_overseer_wreck_core(
        &ctx.db,
        &ctx.systems,
        &ctx.sde,
        30_000_142,
        char_id,
        DVec3::ZERO,
        2,
    )
    .await;
    let (wid_hi, loot_hi) = spawn_ded_overseer_wreck_core(
        &ctx.db,
        &ctx.systems,
        &ctx.sde,
        30_000_142,
        char_id,
        DVec3::new(500.0, 0.0, 0.0),
        8,
    )
    .await;
    assert!(wid_low > 0 && wid_hi > wid_low);
    assert!(!loot_low.is_empty() && !loot_hi.is_empty());

    // Sleeper wave 1 (includes sentry) & wave 2 (no sentry) + sleeper wreck drop
    let w1_balls = spawn_sleeper_wave_in_system(&ctx, sys_id, 3, 1, DVec3::ZERO).await;
    let w2_balls = spawn_sleeper_wave_in_system(&ctx, sys_id, 3, 2, DVec3::new(1000.0, 0.0, 0.0)).await;
    assert_eq!(w1_balls.len(), 4);
    assert_eq!(w2_balls.len(), 3);

    let w3_balls = spawn_sleeper_wave_core(
        &ctx.systems,
        &ctx.db,
        &ctx.sde,
        &ctx.combat_ephemeral,
        sys_id,
        5,
        2,
        DVec3::ZERO,
    )
    .await;
    assert_eq!(w3_balls.len(), 3);
    assert_eq!(
        npc_entity_count(&ctx),
        w1_balls.len() + w2_balls.len() + w3_balls.len()
    );
    assert!(w1_balls
        .iter()
        .chain(w2_balls.iter())
        .chain(w3_balls.iter())
        .all(|&bid| has_npc_entity(&ctx, bid)));

    let sw_id = drop_sleeper_wreck_core(
        &ctx.db,
        &ctx.systems,
        &ctx.sde,
        sys_id,
        char_id,
        DVec3::new(250.0, 0.0, 0.0),
        5,
    )
    .await;
    assert!(sw_id > 0);
}

#[test]
fn faction_loot_keywords_ded_boundaries_and_empty_sde_fallbacks() {
    let sde = shared_sde();
    let empty_sde = SdeRegistry::default();

    // Empty SDE returns None for overseer effects and sleeper blue loot
    assert!(overseer_effects_for_rating(&empty_sde, 5).is_none());
    assert!(sleeper_blue_loot_for_class(&empty_sde, 3).is_none());
    assert!(generate_exploration_container_loot(&empty_sde, 999_999_999, false).is_empty());
    assert!(generate_exploration_container_loot(&empty_sde, 999_999_999, true).is_empty());

    // Exercise all 5 pirate factions (Amarr/Blood/Sansha, Caldari/Guristas, Gallente/Serpentis, Minmatar/Angel)
    let mut tested_factions = std::collections::HashSet::new();
    for sys in sde.solar_systems.values() {
        if sys.solar_system_id >= 30_100_000 {
            continue;
        }
        let fac = pirate_faction_for_system(&sde, sys.solar_system_id);
        let tag = format!("{fac:?}");
        if tested_factions.insert(tag) {
            let d = generate_exploration_container_loot(&sde, sys.solar_system_id, false);
            let r = generate_exploration_container_loot(&sde, sys.solar_system_id, true);
            assert!(!d.is_empty() && !r.is_empty());
        }
        if tested_factions.len() == 5 {
            break;
        }
    }
    assert_eq!(tested_factions.len(), 5);

    // Nullsec system gets deep_space 4x multiplier & ancient salvage on relic
    if let Some(ns_sys) = sde
        .solar_systems
        .values()
        .find(|s| s.solar_system_id < 30_100_000 && s.security_status <= 0.0)
        .map(|s| s.solar_system_id)
    {
        let ns_relic = generate_exploration_container_loot(&sde, ns_sys, true);
        let hs_relic = generate_exploration_container_loot(&sde, 30_000_142, true);
        assert!(ns_relic.len() > hs_relic.len());
    }

    // DED gate ship boundary checks across all 3 brackets (1..=3, 4..=6, 7..=10)
    assert_eq!(ded_ship_restriction_for_rating(1).0, 10_000_000.0);
    assert_eq!(ded_ship_restriction_for_rating(3).0, 10_000_000.0);
    assert_eq!(ded_ship_restriction_for_rating(4).0, 75_000_000.0);
    assert_eq!(ded_ship_restriction_for_rating(6).0, 75_000_000.0);
    assert_eq!(ded_ship_restriction_for_rating(7).0, 250_000_000.0);
    assert_eq!(ded_ship_restriction_for_rating(10).0, 250_000_000.0);

    // Exact mass boundary passes; +1.0 kg fails
    assert!(check_ded_gate_ship_allowed(&sde, 3, 587, 10_000_000.0).is_ok());
    assert_eq!(
        check_ded_gate_ship_allowed(&sde, 3, 587, 10_000_001.0)
            .unwrap_err()
            .code,
        "ShipSizeRestricted"
    );
    // Cruiser/Battlecruiser/Battleship groups tested via synthetic types
    let make_type = |type_id: u32, group_id: u32, name: &str| eve_sde::InvType {
        type_id,
        group_id,
        category_id: 6,
        name: name.into(),
        mass: 1_000_000.0,
        volume: 1_000.0,
        capacity: 100.0,
        radius: 50.0,
        base_price: 0.0,
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
        description: String::new(),
        effect_ids: Vec::new(),
        default_attributes: std::collections::HashMap::new(),
    };
    let mut custom_sde = SdeRegistry::default();
    custom_sde.types.insert(901, make_type(901, 26, "Caracal"));
    custom_sde.types.insert(902, make_type(902, 27, "Raven"));
    assert!(check_ded_gate_ship_allowed(&custom_sde, 2, 901, 5_000_000.0).is_err());
    assert!(check_ded_gate_ship_allowed(&custom_sde, 4, 901, 75_000_000.0).is_ok());
    assert!(check_ded_gate_ship_allowed(&custom_sde, 6, 902, 50_000_000.0).is_err());
    assert!(check_ded_gate_ship_allowed(&custom_sde, 7, 902, 250_000_000.0).is_ok());

    // Overseer personal effects monotonic across 1..=10 and sleeper_blue_loot clamping
    let mut prev_tier = 0_u32;
    for r in 1..=10_u8 {
        let (tid, name) = overseer_effects_for_rating(&sde, r).unwrap();
        assert!(tid > 0 && !name.is_empty());
        let tier: u32 = name
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect::<String>()
            .parse()
            .unwrap_or(0);
        assert!(tier >= prev_tier, "expected monotonic tier for DED {r}: {name}");
        prev_tier = tier;
    }
    assert_eq!(sleeper_blue_loot_for_class(&sde, 0).unwrap().1, 2);
    assert_eq!(sleeper_blue_loot_for_class(&sde, 7).unwrap().1, 14);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn empty_sde_wrecks_multi_cans_and_sleeper_remote_rep_attributes() {
    let empty_sde = Arc::new(SdeRegistry::default());
    let db = Arc::new(WorldRepository::new_empty());
    db.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_776_100_000)));
    let ctx = ServerContext::with_sde_and_repository(empty_sde.clone(), db);
    let sys_id: SolarSystemId = 31_000_005;
    let char_id: i64 = 90_100_002;

    // Multi-can spew materializes all cans & items & ballpark balls
    let cans = vec![
        SpewedLootCan {
            can_item_id: 1_910_001,
            parent_container_id: 1_810_001,
            solar_system_id: sys_id,
            position: [100.0, 0.0, 0.0],
            loot_type_id: 34,
            loot_quantity: 100,
            spawned_at_sec: 1_776_100_000,
            despawn_at_sec: 1_776_103_600,
            scooped: false,
        },
        SpewedLootCan {
            can_item_id: 1_910_002,
            parent_container_id: 1_810_001,
            solar_system_id: sys_id,
            position: [200.0, 0.0, 0.0],
            loot_type_id: 35,
            loot_quantity: 200,
            spawned_at_sec: 1_776_100_000,
            despawn_at_sec: 1_776_103_600,
            scooped: false,
        },
    ];
    materialize_spewed_cans_core(&ctx.db, &ctx.systems, char_id, &cans).await;
    ctx.db.with_snapshot(|snap| {
        assert!(snap.items.contains_key(&1_910_001));
        assert!(snap.items.contains_key(&1_910_002));
        assert_eq!(snap.item_positions[&1_910_002], [200.0, 0.0, 0.0]);
    });
    assert!(ctx
        .systems
        .get_or_spawn(sys_id)
        .get_ball_position_sync(1_910_002)
        .is_some());

    // Empty SDE DED wreck (rating 5 -> battleship wreck 26561) and sleeper wreck (no loot)
    let (wid, loot) = spawn_ded_overseer_wreck_core(
        &ctx.db,
        &ctx.systems,
        &empty_sde,
        30_000_142,
        char_id,
        DVec3::ZERO,
        5,
    )
    .await;
    assert!(loot.is_empty());
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&wid].type_id, 26561);
    });

    let sw_empty = drop_sleeper_wreck_core(
        &ctx.db,
        &ctx.systems,
        &empty_sde,
        sys_id,
        char_id,
        DVec3::ZERO,
        2,
    )
    .await;
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&sw_empty].type_id, 30495);
        assert!(!snap.items.values().any(|i| i.location_id == sw_empty));
    });

    // Sleeper wave with custom remote shield/armor rep & web attributes
    let mut rep_sde = SdeRegistry::default();
    let (trigger_tid, _) = sleeper_npc_types_for_class_and_wave(4, 1);
    let mut t = InvType {
        type_id: trigger_tid,
        group_id: 983,
        category_id: 11,
        name: "Sleeper".into(),
        mass: 5_000_000.0,
        volume: 1_000.0,
        capacity: 0.0,
        radius: 45.0,
        base_price: 0.0,
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
        description: String::new(),
        effect_ids: Vec::new(),
        default_attributes: HashMap::new(),
    };
    t.default_attributes.insert(20, -65.0); // web speed factor
    t.default_attributes.insert(68, 250.0); // remote shield rep
    t.default_attributes.insert(84, 180.0); // remote armor rep
    rep_sde.types.insert(trigger_tid, t);
    let balls = spawn_sleeper_wave_core(
        &ctx.systems,
        &ctx.db,
        &Arc::new(rep_sde),
        &ctx.combat_ephemeral,
        sys_id,
        4,
        1,
        DVec3::ZERO,
    )
    .await;
    let npc = get_npc_entity(&ctx, balls[0]).unwrap();
    assert_eq!(npc.web_speed_factor, -65.0);
    assert_eq!(npc.remote_shield_rep, 250.0);
    assert_eq!(npc.remote_armor_rep, 180.0);
    assert_eq!(npc.remote_rep_range, 25_000.0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn activate_acceleration_gate_read_pure_on_invalid_calls_in_seeded_system() {
    let sys_id: SolarSystemId = 30_000_142;
    let char_id: i64 = 90_050_513;
    let ship_id: i64 = 1_050_401_513;

    let sde = shared_sde();
    let db = Arc::new(WorldRepository::new_empty());
    db.provision_gm_account("exp_r6_f02", "pw", char_id, "ExpR6F02");
    db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.active_ship_id = ship_id;
        }
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587, // Rifter (Frigate)
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    let ctx = ServerContext::with_sde_and_repository(sde, db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_775_820_100)));
    let kw = HashMap::new();

    // Seed two k-space systems + multi-entry collections so `BTreeMap`/`BTreeSet` round-trips cleanly.
    ensure_system_pve_sites_seeded(&ctx, sys_id).await;
    let sys_id_2 = ctx
        .sde
        .solar_systems
        .keys()
        .copied()
        .find(|&id| id != sys_id && id < 31_000_000)
        .expect("second k-space system for BF-1480-02 multi-system read-purity check");
    ensure_system_pve_sites_seeded(&ctx, sys_id_2).await;
    let guarded_site_id = "ANOM-R6-GUARDED-1";
    let guarded_npc_bid = 9_146_999_i64;
    with_pve_exploration_reg(&ctx, |reg| {
        reg.hacked_containers.insert((sys_id, 9_146_001));
        reg.hacked_containers.insert((sys_id_2, 9_146_002));
        reg.failure_counts.insert((sys_id, 9_146_001), 1);
        reg.failure_counts.insert((sys_id_2, 9_146_002), 1);
        reg.sites_by_system.entry(sys_id).or_default().push(PveSiteInstance {
            site_id: guarded_site_id.into(),
            dungeon_id: 9_146_999,
            dungeon_name: "Guarded Anomaly".into(),
            solar_system_id: sys_id,
            site_category: "CombatAnomaly".into(),
            position: [25_000_000.0, 0.0, 0.0],
            signal_strength: 1.0,
            current_room: 1,
            total_rooms: 2,
            current_wave: 1,
            total_waves: 2,
            active_npc_ball_ids: vec![guarded_npc_bid],
            cleared: false,
            created_unix: 1_775_820_100,
            lifetime_seconds: 86_400,
            ..Default::default()
        });
    });
    let _ = ctx
        .systems
        .get_or_spawn(sys_id)
        .spawn_ball_with_combat(
            Ball {
                id: guarded_npc_bid,
                type_id: 587,
                position: DVec3::new(25_000_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 200.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_ship_combat_state(char_id, guarded_npc_bid, 587),
        )
        .await;
    let (dsg_inst_id, dsg_entry_gate_ball_id, dsg_r0_gate_ball_id) = ctx.db.with_snapshot(|snap| {
        snap.dungeon_runtime
            .instances
            .values()
            .find(|i| !i.completed && !i.despawned && i.solar_system_id == sys_id)
            .map(|i| {
                (
                    i.instance_id,
                    i.entry_gate_ball_id
                        .expect("seeded DSG instance has materialized entry_gate_ball_id"),
                    i.rooms
                        .first()
                        .and_then(|r| r.gate_ball_id)
                        .expect("seeded 2-room DSG instance has materialized room 0 gate_ball_id"),
                )
            })
            .expect("seeded system must contain an active DSG DungeonRuntimeInstance")
    });

    // Spawn the pilot's ship ball at origin (well outside 2,500m of DSG-{sys_id} at [68M, -34M, 26M]).
    let _ = ctx
        .systems
        .get_or_spawn(sys_id)
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: 587,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 350.0,
                inertia: 0.5,
                mass: 1_000_000.0,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_ship_combat_state(char_id, ship_id, 587),
        )
        .await;

    let mut sess = SessionState::new(513);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(ship_id);

    let e_seeded = ctx.db.state_epoch();
    let c_seeded = ctx.db.tx_commit_count();

    // 1. Non-matching integer gate ID falls through as None (no EveDb write).
    assert!(
        handle_pve_exploration(
            &ctx,
            &mut sess,
            "keeper",
            "ActivateAccelerationGate",
            &[PyRep::Integer(999_999_999)],
            &kw,
        )
        .await
        .is_none()
    );

    // 2. Non-matching string gate IDs and empty args return DungeonNotFound (no EveDb write).
    for bad_args in [
        vec![PyRep::String("NO-SUCH-GATE".into())],
        vec![PyRep::String("DNG-INST-999999".into())],
        Vec::new(),
    ] {
        let err = handle_pve_exploration(
            &ctx,
            &mut sess,
            "keeper",
            "ActivateAccelerationGate",
            &bad_args,
            &kw,
        )
        .await
        .expect("handled")
        .expect_err("invalid gate arg must return DungeonNotFound");
        assert_eq!(err.code, "DungeonNotFound");
    }

    // 3. EnterNextDungeonRoom with non-matching integer returns DungeonNotFound (no EveDb write).
    let next_err = handle_pve_exploration(
        &ctx,
        &mut sess,
        "keeper",
        "EnterNextDungeonRoom",
        &[PyRep::Integer(999_999_999)],
        &kw,
    )
    .await
    .expect("handled")
    .expect_err("invalid EnterNextDungeonRoom integer must return DungeonNotFound");
    assert_eq!(next_err.code, "DungeonNotFound");

    // 4. Shipless session with non-matching integer gate ID must not falsely match has_runtime_gate.
    let mut shipless = sess.clone();
    shipless.ship_id = None;
    assert!(
        handle_pve_exploration(
            &ctx,
            &mut shipless,
            "keeper",
            "ActivateAccelerationGate",
            &[PyRep::Integer(999_999_999)],
            &kw,
        )
        .await
        .is_none()
    );

    // 5. Direct try_activate_dungeon_runtime_gate misses stay read-pure (with_snapshot).
    assert!(
        crate::services::pve_dungeon_runtime_rpc::try_activate_dungeon_runtime_gate(
            &ctx,
            &mut sess,
            sys_id,
            &[PyRep::Integer(999_999_999)],
        )
        .await
        .is_none()
    );
    assert!(
        crate::services::pve_dungeon_runtime_rpc::try_activate_dungeon_runtime_gate(
            &ctx,
            &mut sess,
            sys_id,
            &[PyRep::String("DNG-INST-999999".into())],
        )
        .await
        .is_none()
    );

    // 6. Matching DSG-{sys_id} runtime gate out-of-range error paths (DNG-INST-*, entry gate, room gate) stay read-pure.
    let dng_tag_err = handle_pve_exploration(
        &ctx,
        &mut sess,
        "keeper",
        "ActivateAccelerationGate",
        &[PyRep::String(format!("DNG-INST-{dsg_inst_id}"))],
        &kw,
    )
    .await
    .expect("handled")
    .expect_err("out-of-range DNG-INST-* gate must reject");
    assert_eq!(dng_tag_err.code, "OutsideGateActivationRange");

    for matched_gate_id in [dsg_entry_gate_ball_id, dsg_r0_gate_ball_id] {
        let gate_err = handle_pve_exploration(
            &ctx,
            &mut sess,
            "keeper",
            "ActivateAccelerationGate",
            &[PyRep::Integer(matched_gate_id)],
            &kw,
        )
        .await
        .expect("handled")
        .expect_err("out-of-range runtime gate ball must reject");
        assert_eq!(gate_err.code, "OutsideGateActivationRange");
    }

    // 6b. No-op write closure across multi-system registry and rejected `WaveNpcsRemaining`
    //     precheck must both stay commit-free (BF-1480-02).
    with_pve_exploration_reg(&ctx, |_reg| {});
    let wave_rem_err = advance_site_wave_or_complete(&ctx, &mut sess, sys_id, guarded_site_id)
        .await
        .expect_err("site with living wave NPC must reject");
    assert_eq!(wave_rem_err.code, "WaveNpcsRemaining");

    assert_eq!(ctx.db.state_epoch(), e_seeded);
    assert_eq!(ctx.db.tx_commit_count(), c_seeded);

    // 7. Regression (#1435): Valid active INC-* gate call when incursion metadata is lazily
    //    initialized (balls not yet spawned) must spawn the incursion NPC ball into the ballpark,
    //    while unknown INC-* IDs stay read-pure and return DungeonNotFound.
    let (const_id, _) = crate::services::pve_incursion_lifecycle::eligible_incursion_constellations(&ctx)
        .into_iter()
        .next()
        .expect("eligible incursion constellation");
    crate::services::pve_special_sites::with_pve_special_reg(&ctx, |reg| {
        reg.initialized = true;
        crate::services::pve_incursion_sites::seed_incursion_for_constellation(&ctx, reg, const_id);
    });
    let (inc_sys, inc_site_id, inc_npc_ball_id) =
        crate::services::pve_special_sites::read_pve_special_reg(&ctx, |reg| {
            let inc = reg.incursions.get(&const_id).expect("seeded incursion");
            let s = inc.sites.first().expect("incursion site");
            let first_npc_ball = s
                .escort_spawns
                .first()
                .map(|e| e.npc_ball_id)
                .unwrap_or(s.boss_npc_ball_id);
            (s.solar_system_id, s.site_id.clone(), first_npc_ball)
        });
    let inc_handle = ctx.systems.get_or_spawn(inc_sys);
    assert!(
        inc_handle.get_ball_snapshot(inc_npc_ball_id).await.ok().flatten().is_none(),
        "lazy incursion metadata must not have spawned NPC ball prior to ActivateAccelerationGate"
    );
    sess.solar_system_id = Some(inc_sys);
    let _ = inc_handle
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: 587,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 350.0,
                inertia: 0.5,
                mass: 1_000_000.0,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_ship_combat_state(char_id, ship_id, 587),
        )
        .await;

    let e_inc = ctx.db.state_epoch();
    let c_inc = ctx.db.tx_commit_count();
    let bogus_inc_err = handle_pve_exploration(
        &ctx,
        &mut sess,
        "keeper",
        "ActivateAccelerationGate",
        &[PyRep::String("INC-BOGUS-999999".into())],
        &kw,
    )
    .await
    .expect("handled")
    .expect_err("unknown INC-* site must reject with DungeonNotFound");
    assert_eq!(bogus_inc_err.code, "DungeonNotFound");
    assert_eq!(ctx.db.state_epoch(), e_inc);
    assert_eq!(ctx.db.tx_commit_count(), c_inc);
    assert!(
        inc_handle.get_ball_snapshot(inc_npc_ball_id).await.ok().flatten().is_none(),
        "bogus INC-* gate call must not spawn incursion NPC balls"
    );

    let inc_gate_err = handle_pve_exploration(
        &ctx,
        &mut sess,
        "keeper",
        "ActivateAccelerationGate",
        &[PyRep::String(inc_site_id)],
        &kw,
    )
    .await
    .expect("handled")
    .expect_err("active INC-* site with live NPCs rejects with IncursionSiteNpcsRemaining");
    assert_eq!(inc_gate_err.code, "IncursionSiteNpcsRemaining");
    assert!(
        inc_handle.get_ball_snapshot(inc_npc_ball_id).await.ok().flatten().is_some(),
        "valid active INC-* gate call must spawn incursion NPC ball into system ballpark (#1435)"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cleared_preceding_site_does_not_shadow_active_wave2_writeback_or_gate_lookup() {
    let sys_id: SolarSystemId = 30_000_142;
    let char_id: i64 = 90_051_462;
    let ship_id: i64 = 1_051_462_001;

    let sde = shared_sde();
    let db = Arc::new(WorldRepository::new_empty());
    db.provision_gm_account("hf1462_shadow", "pw", char_id, "Hf1462Shadow");
    db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.active_ship_id = ship_id;
            ch.station_id = None;
            ch.structure_id = None;
        }
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });

    let ctx = ServerContext::with_sde_and_repository(sde, db);
    let clock = Arc::new(VirtualServerClock::with_fixed_base(1_776_000_000));
    ctx.set_clock(clock.clone());
    let now = 1_776_000_000_u64;

    let mut session = SessionState::new(1462);
    session.user_id = Some(1);
    session.character_id = Some(char_id);
    session.solar_system_id = Some(sys_id);
    session.ship_id = Some(ship_id);

    let shared_dungeon_id = 1420_u32;
    let site_pos = [15_000_000.0, 0.0, 0.0];
    let shared_gate_ball_id = 9_146_201_i64;
    let cleared_first_id = "EXP-CLEARED-FIRST-1";
    let expired_second_id = "EXP-EXPIRED-SECOND-1";
    let active_third_id = "EXP-ACTIVE-THIRD-1";
    let active_canonical_id = "EXP-ACTIVE-CANONICAL-1";

    with_pve_exploration_reg(&ctx, |reg| {
        reg.seeded_systems.insert(sys_id, true);
        let sites = reg.sites_by_system.entry(sys_id).or_default();
        // Cleared site sharing dungeon_id and gate_ball_id placed first.
        sites.push(PveSiteInstance {
            site_id: cleared_first_id.into(),
            dungeon_id: shared_dungeon_id,
            dungeon_name: "Guristas Scout Outpost [Cleared]".into(),
            solar_system_id: sys_id,
            site_category: "CombatAnomaly".into(),
            position: site_pos,
            signal_strength: 1.0,
            current_room: 2,
            total_rooms: 2,
            current_wave: 2,
            total_waves: 2,
            active_npc_ball_ids: Vec::new(),
            cleared: true,
            created_unix: now.saturating_sub(1_000),
            lifetime_seconds: 86_400,
            gate_ball_id: Some(shared_gate_ball_id),
            ..Default::default()
        });
        // Uncleared expired site sharing dungeon_id placed second.
        sites.push(PveSiteInstance {
            site_id: expired_second_id.into(),
            dungeon_id: shared_dungeon_id,
            dungeon_name: "Guristas Scout Outpost [Expired]".into(),
            solar_system_id: sys_id,
            site_category: "CombatAnomaly".into(),
            position: site_pos,
            signal_strength: 1.0,
            current_room: 1,
            total_rooms: 2,
            current_wave: 1,
            total_waves: 2,
            active_npc_ball_ids: Vec::new(),
            cleared: false,
            created_unix: now.saturating_sub(90_000),
            lifetime_seconds: 86_400,
            gate_ball_id: Some(shared_gate_ball_id),
            ..Default::default()
        });
        // Active site sharing dungeon_id and gate_ball_id placed third.
        sites.push(PveSiteInstance {
            site_id: active_third_id.into(),
            dungeon_id: shared_dungeon_id,
            dungeon_name: "Guristas Scout Outpost [Active]".into(),
            solar_system_id: sys_id,
            site_category: "CombatAnomaly".into(),
            position: site_pos,
            signal_strength: 1.0,
            current_room: 1,
            total_rooms: 2,
            current_wave: 1,
            total_waves: 2,
            active_npc_ball_ids: Vec::new(),
            cleared: false,
            created_unix: now,
            lifetime_seconds: 86_400,
            gate_ball_id: Some(shared_gate_ball_id),
            ..Default::default()
        });
    });

    ctx.db.with_mut_snapshot(|snap| {
        snap.cosmic_anomalies
            .entry(sys_id)
            .or_default()
            .push(eve_db::CosmicAnomalyRecord {
                anomaly_id: active_third_id.into(),
                solar_system_id: sys_id,
                name: "Guristas Scout Outpost [Active]".into(),
                anomaly_type: "Combat".into(),
                position: site_pos,
                signal_strength: 1.0,
                ore_types: Vec::new(),
                remaining_volume: 0.0,
                created_unix: now,
                lifetime_seconds: 86_400,
                cleared: false,
            });
        snap.cosmic_signatures
            .entry(sys_id)
            .or_default()
            .push(eve_db::CosmicSignatureRecord {
                signature_id: active_third_id.into(),
                solar_system_id: sys_id,
                type_name: "Combat".into(),
                sig_group: "Combat".into(),
                position: site_pos,
                signal_strength: 1.0,
                wormhole_type_id: None,
                destination_system_id: None,
                created_unix: now,
                lifetime_seconds: 86_400,
                max_mass_kg: 0.0,
                remaining_mass_kg: 0.0,
                max_jump_mass_kg: 0.0,
                collapsed: false,
            });
    });

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: 587,
                position: DVec3::from_array(site_pos),
                velocity: DVec3::ZERO,
                max_velocity: 350.0,
                inertia: 0.5,
                mass: 1_000_000.0,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_ship_combat_state(char_id, ship_id, 587),
        )
        .await;

    // 1. Advancing via shared numeric dungeon_id alias must fail closed (`SiteNotFound`), whereas
    // advancing via canonical `site_id` must target active_third_id and write Wave-2 NPCs onto it.
    let did_alias_err = advance_site_wave_or_complete(&ctx, &mut session, sys_id, "1420")
        .await
        .expect_err("numeric dungeon_id string alias must be rejected under #1495");
    assert_eq!(did_alias_err.code, "SiteNotFound");

    let wave2_rep = advance_site_wave_or_complete(&ctx, &mut session, sys_id, active_third_id)
        .await
        .expect("canonical site_id advance should target active site");
    assert_eq!(
        wave2_rep.dict_get("siteID"),
        Some(&PyRep::String(active_third_id.into()))
    );
    assert_eq!(wave2_rep.dict_get("currentWave"), Some(&PyRep::Integer(2)));
    assert_eq!(
        wave2_rep.dict_get("completed"),
        Some(&PyRep::Bool(false))
    );

    let wave2_balls = read_pve_exploration_reg(&ctx, |reg| {
        let sites = &reg.sites_by_system[&sys_id];
        let cleared_site = sites.iter().find(|s| s.site_id == cleared_first_id).unwrap();
        let expired_site = sites.iter().find(|s| s.site_id == expired_second_id).unwrap();
        let active_site = sites.iter().find(|s| s.site_id == active_third_id).unwrap();
        assert!(
            cleared_site.active_npc_ball_ids.is_empty(),
            "cleared preceding site must never absorb Wave-2 NPC ball IDs"
        );
        assert!(
            expired_site.active_npc_ball_ids.is_empty(),
            "expired preceding site must never absorb Wave-2 NPC ball IDs"
        );
        assert!(
            !active_site.active_npc_ball_ids.is_empty(),
            "active site must retain spawned Wave-2 NPC ball IDs"
        );
        assert_eq!(active_site.current_wave, 2);
        active_site.active_npc_ball_ids.clone()
    });

    // 2. Immediate repeat gate advance while Wave-2 NPCs are alive must fail with WaveNpcsRemaining (no wave skip).
    let skip_err = advance_site_wave_or_complete(&ctx, &mut session, sys_id, active_third_id)
        .await
        .expect_err("WaveNpcsRemaining must block immediate second advance");
    assert_eq!(skip_err.code, "WaveNpcsRemaining");

    // 3. Killing Wave-2 NPCs clears active_third_id + its canonical anomaly and signature.
    for (idx, bid) in wave2_balls.iter().enumerate() {
        let _ = sys_handle.remove_ball(*bid).await;
        let (cleared_opt, _, _) = handle_pve_site_npc_destruction_core(
            &ctx.systems,
            &ctx.db,
            &ctx.sde,
            &ctx.mechanic_rng,
            &*ctx.combat_ephemeral,
            sys_id,
            *bid,
            char_id,
            false,
        )
        .await;
        if idx + 1 == wave2_balls.len() {
            assert_eq!(cleared_opt.as_deref(), Some(active_third_id));
        } else {
            assert!(cleared_opt.is_none());
        }
    }
    read_pve_exploration_reg(&ctx, |reg| {
        let active_site = reg.sites_by_system[&sys_id]
            .iter()
            .find(|s| s.site_id == active_third_id)
            .unwrap();
        assert!(active_site.cleared);
    });
    ctx.db.with_snapshot(|snap| {
        assert!(snap.cosmic_anomalies[&sys_id]
            .iter()
            .find(|a| a.anomaly_id == active_third_id)
            .unwrap()
            .cleared);
        assert!(snap.cosmic_signatures[&sys_id]
            .iter()
            .find(|s| s.signature_id == active_third_id)
            .unwrap()
            .collapsed);
    });

    // 4. Seed a second active site sharing dungeon_id & gate_ball_id and advance using canonical site_id directly
    // (previously `s.site_id == site_id || s.dungeon_id == dungeon_id` still matched cleared_first_id).
    with_pve_exploration_reg(&ctx, |reg| {
        reg.sites_by_system
            .entry(sys_id)
            .or_default()
            .push(PveSiteInstance {
                site_id: active_canonical_id.into(),
                dungeon_id: shared_dungeon_id,
                dungeon_name: "Guristas Scout Outpost [Canonical]".into(),
                solar_system_id: sys_id,
                site_category: "CombatAnomaly".into(),
                position: site_pos,
                signal_strength: 1.0,
                current_room: 1,
                total_rooms: 2,
                current_wave: 1,
                total_waves: 2,
                active_npc_ball_ids: Vec::new(),
                cleared: false,
                created_unix: now,
                lifetime_seconds: 86_400,
                gate_ball_id: Some(shared_gate_ball_id),
                ..Default::default()
            });
    });

    // Gate lookup by shared integer gate_ball_id must pick active_canonical_id over cleared/expired sites.
    let kw = HashMap::new();
    let gate_res = handle_pve_exploration(
        &ctx,
        &mut session,
        "keeper",
        "ActivateAccelerationGate",
        &[PyRep::Integer(shared_gate_ball_id)],
        &kw,
    )
    .await
    .expect("handled")
    .expect("ActivateAccelerationGate by gate_ball_id must advance active site");
    assert_eq!(
        gate_res.dict_get("siteID"),
        Some(&PyRep::String(active_canonical_id.into()))
    );
    assert_eq!(gate_res.dict_get("currentWave"), Some(&PyRep::Integer(2)));

    read_pve_exploration_reg(&ctx, |reg| {
        let sites = &reg.sites_by_system[&sys_id];
        let cleared_site = sites.iter().find(|s| s.site_id == cleared_first_id).unwrap();
        let canonical_site = sites
            .iter()
            .find(|s| s.site_id == active_canonical_id)
            .unwrap();
        assert!(cleared_site.active_npc_ball_ids.is_empty());
        assert!(!canonical_site.active_npc_ball_ids.is_empty());
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn expired_unpruned_sites_reject_and_prune_on_npc_kill_and_gate_advance() {
    let sys_id: SolarSystemId = 30_000_142;
    let char_id: i64 = 90_052_462;
    let ship_id: i64 = 1_052_462_001;

    let sde = shared_sde();
    let db = Arc::new(WorldRepository::new_empty());
    db.provision_gm_account("hf1462_expiry", "pw", char_id, "Hf1462Expiry");
    db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.active_ship_id = ship_id;
            ch.station_id = None;
            ch.structure_id = None;
        }
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });

    let ctx = ServerContext::with_sde_and_repository(sde, db);
    let clock = Arc::new(VirtualServerClock::with_fixed_base(1_776_100_000));
    ctx.set_clock(clock.clone());
    let base_now = 1_776_100_000_u64;

    let mut session = SessionState::new(1463);
    session.user_id = Some(1);
    session.character_id = Some(char_id);
    session.solar_system_id = Some(sys_id);
    session.ship_id = Some(ship_id);

    let site_pos = [20_000_000.0, 0.0, 0.0];
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let spawn_npc = |bid: i64| {
        let h = sys_handle.clone();
        let cs = build_type_combat_state_from_sde(&ctx.sde, 23_333);
        async move {
            let _ = h
                .spawn_ball_with_combat(
                    Ball {
                        id: bid,
                        type_id: 23_333,
                        position: DVec3::from_array(site_pos),
                        velocity: DVec3::ZERO,
                        max_velocity: 200.0,
                        inertia: 1.0,
                        mass: 500_000.0,
                        radius: 40.0,
                        speed_fraction: 0.0,
                        mode: MovementMode::Stop,
                    },
                    cs,
                )
                .await;
        }
    };

    let npc_kill_wave1 = 9_146_301_i64;
    let npc_kill_final = 9_146_302_i64;
    let npc_gate_expired = 9_146_303_i64;
    let npc_linked_esc_expired = 9_146_304_i64;
    for bid in [
        npc_kill_wave1,
        npc_kill_final,
        npc_gate_expired,
        npc_linked_esc_expired,
    ] {
        spawn_npc(bid).await;
    }

    let linked_inst_id = 888_462_i64;
    with_pve_exploration_reg(&ctx, |reg| {
        reg.seeded_systems.insert(sys_id, true);
        let sites = reg.sites_by_system.entry(sys_id).or_default();
        // 1. Multi-wave combat anomaly whose site lifetime has expired.
        sites.push(PveSiteInstance {
            site_id: "ANOM-EXP-W1".into(),
            dungeon_id: 1420,
            dungeon_name: "Expired Outpost W1".into(),
            solar_system_id: sys_id,
            site_category: "CombatAnomaly".into(),
            position: site_pos,
            signal_strength: 1.0,
            current_room: 1,
            total_rooms: 1,
            current_wave: 1,
            total_waves: 2,
            active_npc_ball_ids: vec![npc_kill_wave1],
            cleared: false,
            created_unix: base_now,
            lifetime_seconds: 3_600,
            ..Default::default()
        });
        // 2. Final-wave combat anomaly whose site lifetime has expired.
        sites.push(PveSiteInstance {
            site_id: "ANOM-EXP-FINAL".into(),
            dungeon_id: 1420,
            dungeon_name: "Expired Outpost Final".into(),
            solar_system_id: sys_id,
            site_category: "CombatAnomaly".into(),
            position: site_pos,
            signal_strength: 1.0,
            current_room: 1,
            total_rooms: 1,
            current_wave: 1,
            total_waves: 1,
            active_npc_ball_ids: vec![npc_kill_final],
            cleared: false,
            created_unix: base_now,
            lifetime_seconds: 3_600,
            ..Default::default()
        });
        // 3. Expired site tested via advance_site_wave_or_complete (no remaining balls in space or unpruned NPC).
        sites.push(PveSiteInstance {
            site_id: "SITE-EXP-GATE".into(),
            dungeon_id: 1430,
            dungeon_name: "Expired Gate Site".into(),
            solar_system_id: sys_id,
            site_category: "CombatAnomaly".into(),
            position: site_pos,
            signal_strength: 1.0,
            current_room: 1,
            total_rooms: 2,
            current_wave: 1,
            total_waves: 2,
            active_npc_ball_ids: vec![npc_gate_expired],
            cleared: false,
            created_unix: base_now,
            lifetime_seconds: 3_600,
            ..Default::default()
        });
        // 4. Site with lifetime_seconds == 0 whose linked EscalatingPathEntry has expired.
        sites.push(PveSiteInstance {
            site_id: format!("EXP-{linked_inst_id}-1"),
            dungeon_id: 1420,
            dungeon_name: "Linked Expired Escalation".into(),
            solar_system_id: sys_id,
            site_category: "Expedition".into(),
            position: site_pos,
            signal_strength: 1.0,
            current_room: 1,
            total_rooms: 1,
            current_wave: 1,
            total_waves: 1,
            active_npc_ball_ids: vec![npc_linked_esc_expired],
            cleared: false,
            created_unix: base_now,
            lifetime_seconds: 0,
            expedition_instance_id: Some(linked_inst_id),
            ..Default::default()
        });
        reg.escalations_by_char
            .entry(char_id)
            .or_default()
            .push(EscalatingPathEntry {
                instance_id: linked_inst_id,
                character_id: char_id,
                source_dungeon_id: 1420,
                expedition_dungeon_id: 1420,
                expedition_name: "Linked Expired Escalation".into(),
                destination_solar_system_id: sys_id,
                step_index: 1,
                total_steps: 2,
                journal_text: "Expired".into(),
                created_at_sec: base_now,
                expires_at_sec: base_now + 3_600,
                dungeons_entered: vec![1420],
                journal_entry_template_id: 0,
            });
    });

    ctx.db.with_mut_snapshot(|snap| {
        snap.cosmic_anomalies
            .entry(sys_id)
            .or_default()
            .push(eve_db::CosmicAnomalyRecord {
                anomaly_id: "ANOM-EXP-W1".into(),
                solar_system_id: sys_id,
                name: "Expired Outpost W1".into(),
                anomaly_type: "Combat".into(),
                position: site_pos,
                signal_strength: 1.0,
                ore_types: Vec::new(),
                remaining_volume: 0.0,
                created_unix: base_now,
                lifetime_seconds: 3_600,
                cleared: false,
            });
        snap.bookmarks.insert(
            77_1462,
            eve_db::BookmarkRecord {
                bookmark_id: 77_1462,
                owner_id: char_id,
                label: "Expired Linked Esc BM".into(),
                solar_system_id: sys_id,
                x: site_pos[0],
                y: site_pos[1],
                z: site_pos[2],
                item_id: Some(linked_inst_id),
                memo: String::new(),
                folder_id: None,
                is_shared: false,
            },
        );
    });

    let items_before = ctx.db.with_snapshot(|snap| snap.items.len());
    clock.advance_secs(4_000);

    // Case 1 & 2: Killing Wave-1 or final-wave NPC in expired site must return None, spawn no Wave 2, drop no loot, and prune site/balls.
    let res1 = handle_pve_site_npc_destruction_core(
        &ctx.systems,
        &ctx.db,
        &ctx.sde,
        &ctx.mechanic_rng,
        &*ctx.combat_ephemeral,
        sys_id,
        npc_kill_wave1,
        char_id,
        false,
    )
    .await;
    assert!(res1.0.is_none() && res1.1.is_none() && res1.2.is_none());

    let res2 = handle_pve_site_npc_destruction_core(
        &ctx.systems,
        &ctx.db,
        &ctx.sde,
        &ctx.mechanic_rng,
        &*ctx.combat_ephemeral,
        sys_id,
        npc_kill_final,
        char_id,
        false,
    )
    .await;
    assert!(res2.0.is_none() && res2.1.is_none() && res2.2.is_none());

    // Case 3: Gate / wave advance on expired site must reject with SiteExpired and prune site + live NPC ball.
    let gate_err = advance_site_wave_or_complete(&ctx, &mut session, sys_id, "SITE-EXP-GATE")
        .await
        .expect_err("expired site must reject with SiteExpired");
    assert_eq!(gate_err.code, "SiteExpired");

    // Case 4: Active-lifetime_seconds==0 site linked to an expired EscalatingPathEntry must reject on NPC kill and prune escalation + bookmark + ball.
    let res4 = handle_pve_site_npc_destruction_core(
        &ctx.systems,
        &ctx.db,
        &ctx.sde,
        &ctx.mechanic_rng,
        &*ctx.combat_ephemeral,
        sys_id,
        npc_linked_esc_expired,
        char_id,
        false,
    )
    .await;
    assert!(res4.0.is_none() && res4.1.is_none() && res4.2.is_none());

    assert_eq!(
        ctx.db.with_snapshot(|snap| snap.items.len()),
        items_before,
        "expired unpruned site kills/advances must drop zero loot items"
    );
    ctx.db.with_snapshot(|snap| {
        assert!(snap.cosmic_anomalies[&sys_id]
            .iter()
            .find(|a| a.anomaly_id == "ANOM-EXP-W1")
            .unwrap()
            .cleared);
        assert!(!snap.bookmarks.contains_key(&77_1462));
    });
    read_pve_exploration_reg(&ctx, |reg| {
        for sid in [
            "ANOM-EXP-W1",
            "ANOM-EXP-FINAL",
            "SITE-EXP-GATE",
            &format!("EXP-{linked_inst_id}-1"),
        ] {
            let site = reg.sites_by_system[&sys_id]
                .iter()
                .find(|s| s.site_id == sid)
                .unwrap();
            assert!(site.cleared, "{sid} must be marked cleared after expiry prune");
            assert_eq!(site.current_wave, 1, "{sid} must never advance to Wave 2");
            assert!(site.active_npc_ball_ids.is_empty());
        }
        assert!(
            reg.escalations_by_char
                .get(&char_id)
                .is_none_or(|l| l.is_empty()),
            "expired linked escalation must be pruned"
        );
    });
    for bid in [
        npc_kill_wave1,
        npc_kill_final,
        npc_gate_expired,
        npc_linked_esc_expired,
    ] {
        assert_eq!(
            sys_handle.get_ball_snapshot(bid).await.unwrap(),
            None,
            "expired NPC ball {bid} must be removed from ballpark"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_container_hacks_across_two_systems_preserve_both_entries_and_loot() {
    let mut sde_raw = (*shared_sde()).clone();
    if !sde_raw.types.contains_key(&22_588) {
        if let Some(mut relic_t) = sde_raw.types.values().next().cloned() {
            relic_t.type_id = 22_588;
            relic_t.group_id = 538;
            relic_t.name = "Relic Analyzer I".into();
            relic_t.published = true;
            relic_t.default_attributes.insert(1909, 40.0);
            relic_t.default_attributes.insert(1910, 20.0);
            relic_t.default_attributes.insert(1911, 2.0);
            relic_t.default_attributes.insert(54, 5_000.0);
            sde_raw.types.insert(22_588, relic_t);
        }
    }
    let sys_a: SolarSystemId = 30_000_142;
    let sys_b: SolarSystemId = sde_raw
        .solar_systems
        .keys()
        .copied()
        .find(|&id| id != sys_a && id < 31_000_000)
        .expect("second k-space system required for cross-system test");
    let sde = Arc::new(sde_raw);

    let char_a: i64 = 90_061_480;
    let ship_a: i64 = 1_061_480_001;
    let can_a: i64 = 1_061_480_901;
    let char_b: i64 = 90_061_481;
    let ship_b: i64 = 1_061_481_001;
    let can_b: i64 = 1_061_481_901;

    let db = Arc::new(WorldRepository::new_empty());
    db.provision_gm_account("conc_hack_a", "pw", char_a, "ConcHackA");
    db.provision_gm_account("conc_hack_b", "pw", char_b, "ConcHackB");
    db.with_mut_snapshot(|snap| {
        for (cid, sid, sys) in [(char_a, ship_a, sys_a), (char_b, ship_b, sys_b)] {
            if let Some(ch) = snap.characters.get_mut(&cid) {
                ch.solar_system_id = sys;
                ch.active_ship_id = sid;
                ch.station_id = None;
                ch.structure_id = None;
            }
            snap.items.insert(
                sid,
                InventoryItemRecord {
                    item_id: sid,
                    type_id: 587,
                    owner_id: cid,
                    location_id: sys as i64,
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
            snap.items.insert(
                sid + 1,
                InventoryItemRecord {
                    item_id: sid + 1,
                    type_id: 22_588,
                    owner_id: cid,
                    location_id: sid,
                    flag: InventoryFlag::MedSlot0,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
        }
    });

    let ctx = Arc::new(ServerContext::with_sde_and_repository(sde, db));
    let base_now = 1_776_200_000_u64;
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(base_now)));

    let site_pos = [100_000.0, 0.0, 0.0];
    let can_pos = DVec3::new(101_000.0, 0.0, 0.0);
    for (sys, sid, cid) in [(sys_a, ship_a, can_a), (sys_b, ship_b, can_b)] {
        let h = ctx.systems.get_or_spawn(sys);
        let _ = h
            .spawn_ball(Ball {
                id: sid,
                type_id: 587,
                position: DVec3::from_array(site_pos),
                velocity: DVec3::ZERO,
                max_velocity: 200.0,
                inertia: 1.0,
                mass: 1_100_000.0,
                radius: 35.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            })
            .await;
        let _ = h
            .spawn_ball(Ball {
                id: cid,
                type_id: 22_270,
                position: can_pos,
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 1.0,
                mass: 1.0,
                radius: 200.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            })
            .await;
    }

    with_pve_exploration_reg(&ctx, |reg| {
        for (idx, (sys, cid, can_id)) in
            [(sys_a, char_a, can_a), (sys_b, char_b, can_b)].into_iter().enumerate()
        {
            let site_id = format!("RELIC-CONC-{sys}-{}", idx + 1);
            reg.seeded_systems.insert(sys, true);
            reg.sites_by_system.entry(sys).or_default().push(PveSiteInstance {
                site_id: site_id.clone(),
                dungeon_id: 2010,
                dungeon_name: "Ruined Pirate Relic Site".into(),
                solar_system_id: sys,
                site_category: "Relic".into(),
                position: site_pos,
                probe_required: true,
                signal_strength: 1.0,
                current_room: 1,
                total_rooms: 1,
                current_wave: 1,
                total_waves: 1,
                hackable_can_ids: vec![can_id],
                ghost_detonate_after_sec: 120,
                created_unix: base_now,
                lifetime_seconds: 86_400,
                ..Default::default()
            });
            reg.hacking_games.insert(
                (cid, can_id),
                HackingGameSession {
                    game_id: 900_001 + idx as i64,
                    character_id: cid,
                    solar_system_id: sys,
                    container_id: can_id,
                    site_id,
                    is_relic: true,
                    is_ghost_site: false,
                    is_sleeper_cache_puzzle: false,
                    virus_coherence: 100,
                    max_virus_coherence: 100,
                    base_virus_strength: 40,
                    polymorphic_shield_charges: 0,
                    secondary_vector_target: None,
                    utilities: vec![None; 2],
                    tiles: vec![
                        HackingTileState {
                            tile_index: 0,
                            revealed: true,
                            blocked_by_defensive: false,
                            kind: HackingNodeKind::Empty,
                            coherence: 0,
                            strength: 0,
                            utility_drop: None,
                        },
                        HackingTileState {
                            tile_index: 1,
                            revealed: false,
                            blocked_by_defensive: false,
                            kind: HackingNodeKind::SystemCore,
                            coherence: 10,
                            strength: 10,
                            utility_drop: None,
                        },
                    ],
                    status: "InProgress".into(),
                },
            );
        }
    });

    let make_sess = |conn_id: u64, cid: i64, sys: SolarSystemId, sid: i64| {
        let mut s = SessionState::new(conn_id);
        s.user_id = Some(1);
        s.character_id = Some(cid);
        s.solar_system_id = Some(sys);
        s.ship_id = Some(sid);
        s
    };

    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let mut sess_a = make_sess(61480, char_a, sys_a, ship_a);
    let mut sess_b = make_sess(61481, char_b, sys_b, ship_b);

    let (ctx_a, b_a) = (Arc::clone(&ctx), Arc::clone(&barrier));
    let h_a = tokio::spawn(async move {
        let kw = HashMap::new();
        b_a.wait().await;
        let res = handle_pve_exploration(
            &ctx_a,
            &mut sess_a,
            "hackingMgr",
            "ClickedOnTile",
            &[PyRep::Integer(can_a), PyRep::Integer(1)],
            &kw,
        )
        .await;
        (sess_a, res)
    });
    let (ctx_b, b_b) = (Arc::clone(&ctx), Arc::clone(&barrier));
    let h_b = tokio::spawn(async move {
        let kw = HashMap::new();
        b_b.wait().await;
        let res = handle_pve_exploration(
            &ctx_b,
            &mut sess_b,
            "hackingMgr",
            "ClickedOnTile",
            &[PyRep::Integer(can_b), PyRep::Integer(1)],
            &kw,
        )
        .await;
        (sess_b, res)
    });

    let (mut sess_a, res_a) = h_a.await.unwrap();
    let (mut sess_b, res_b) = h_b.await.unwrap();
    let rep_a = res_a.expect("handled").expect("system A hack must succeed");
    let rep_b = res_b.expect("handled").expect("system B hack must succeed");
    assert_eq!(rep_a.dict_get("status"), Some(&PyRep::String("Won".into())));
    assert_eq!(rep_b.dict_get("status"), Some(&PyRep::String("Won".into())));

    let total_spewed_after_win = read_pve_exploration_reg(&ctx, |reg| {
        assert!(
            reg.hacked_containers.contains(&(sys_a, can_a)),
            "concurrent cross-system registry write must preserve sys_a hacked container"
        );
        assert!(
            reg.hacked_containers.contains(&(sys_b, can_b)),
            "concurrent cross-system registry write must preserve sys_b hacked container"
        );
        assert!(
            reg.spewed_cans.values().any(|c| c.parent_container_id == can_a),
            "sys_a spewed cans must be preserved"
        );
        assert!(
            reg.spewed_cans.values().any(|c| c.parent_container_id == can_b),
            "sys_b spewed cans must be preserved"
        );
        reg.spewed_cans.len()
    });
    let items_after_win = ctx.db.with_snapshot(|snap| snap.items.len());

    // Re-running concurrent clicks on the already-won containers must reject cleanly with zero double-loot.
    let kw = HashMap::new();
    let args_a = [PyRep::Integer(can_a), PyRep::Integer(1)];
    let args_b = [PyRep::Integer(can_b), PyRep::Integer(1)];
    let (retry_a, retry_b) = tokio::join!(
        handle_pve_exploration(
            &ctx,
            &mut sess_a,
            "hackingMgr",
            "ClickedOnTile",
            &args_a,
            &kw,
        ),
        handle_pve_exploration(
            &ctx,
            &mut sess_b,
            "hackingMgr",
            "ClickedOnTile",
            &args_b,
            &kw,
        )
    );
    assert_eq!(
        retry_a.expect("handled").expect_err("won can must reject").code,
        "HackingGameAlreadyFinished"
    );
    assert_eq!(
        retry_b.expect("handled").expect_err("won can must reject").code,
        "HackingGameAlreadyFinished"
    );
    assert_eq!(
        read_pve_exploration_reg(&ctx, |reg| reg.spewed_cans.len()),
        total_spewed_after_win
    );
    assert_eq!(ctx.db.with_snapshot(|snap| snap.items.len()), items_after_win);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_advance_site_wave_or_complete_on_final_wave_executes_once() {
    let sys_id: SolarSystemId = 30_000_142;
    let char_a: i64 = 90_062_480;
    let ship_a: i64 = 1_062_480_001;
    let char_b: i64 = 90_062_481;
    let ship_b: i64 = 1_062_481_001;

    let sde = shared_sde();
    let db = Arc::new(WorldRepository::new_empty());
    db.provision_gm_account("conc_fin_a", "pw", char_a, "ConcFinA");
    db.provision_gm_account("conc_fin_b", "pw", char_b, "ConcFinB");
    db.with_mut_snapshot(|snap| {
        for (cid, sid) in [(char_a, ship_a), (char_b, ship_b)] {
            if let Some(ch) = snap.characters.get_mut(&cid) {
                ch.solar_system_id = sys_id;
                ch.active_ship_id = sid;
                ch.station_id = None;
                ch.structure_id = None;
            }
            snap.items.insert(
                sid,
                InventoryItemRecord {
                    item_id: sid,
                    type_id: 587,
                    owner_id: cid,
                    location_id: sys_id as i64,
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
        }
    });

    let ctx = Arc::new(ServerContext::with_sde_and_repository(sde, db));
    let base_now = 1_776_210_000_u64;
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(base_now)));

    let site_id = "ANOM-FINAL-RACE";
    let site_pos = [30_000_000.0, 0.0, 0.0];
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    for (cid, sid) in [(char_a, ship_a), (char_b, ship_b)] {
        let _ = sys_handle
            .spawn_ball_with_combat(
                Ball {
                    id: sid,
                    type_id: 587,
                    position: DVec3::from_array(site_pos),
                    velocity: DVec3::ZERO,
                    max_velocity: 350.0,
                    inertia: 0.5,
                    mass: 1_000_000.0,
                    radius: 50.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                ctx.build_ship_combat_state(cid, sid, 587),
            )
            .await;
    }

    with_pve_exploration_reg(&ctx, |reg| {
        reg.seeded_systems.insert(sys_id, true);
        reg.sites_by_system.entry(sys_id).or_default().push(PveSiteInstance {
            site_id: site_id.into(),
            dungeon_id: 1420,
            dungeon_name: "Guristas Final Race".into(),
            solar_system_id: sys_id,
            site_category: "CombatAnomaly".into(),
            position: site_pos,
            signal_strength: 1.0,
            current_room: 2,
            total_rooms: 2,
            current_wave: 2,
            total_waves: 2,
            active_npc_ball_ids: Vec::new(),
            cleared: false,
            created_unix: base_now,
            lifetime_seconds: 86_400,
            ..Default::default()
        });
    });
    ctx.db.with_mut_snapshot(|snap| {
        snap.cosmic_anomalies
            .entry(sys_id)
            .or_default()
            .push(eve_db::CosmicAnomalyRecord {
                anomaly_id: site_id.into(),
                solar_system_id: sys_id,
                name: "Guristas Final Race".into(),
                anomaly_type: "Combat".into(),
                position: site_pos,
                signal_strength: 1.0,
                ore_types: Vec::new(),
                remaining_volume: 0.0,
                created_unix: base_now,
                lifetime_seconds: 86_400,
                cleared: false,
            });
    });
    let items_before = ctx.db.with_snapshot(|snap| snap.items.len());

    let make_sess = |conn_id: u64, cid: i64, sid: i64| {
        let mut s = SessionState::new(conn_id);
        s.user_id = Some(1);
        s.character_id = Some(cid);
        s.solar_system_id = Some(sys_id);
        s.ship_id = Some(sid);
        s
    };

    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let (ctx_a, b_a, mut sess_a) = (
        Arc::clone(&ctx),
        Arc::clone(&barrier),
        make_sess(62480, char_a, ship_a),
    );
    let h_a = tokio::spawn(async move {
        b_a.wait().await;
        advance_site_wave_or_complete(&ctx_a, &mut sess_a, sys_id, site_id).await
    });
    let (ctx_b, b_b, mut sess_b) = (
        Arc::clone(&ctx),
        Arc::clone(&barrier),
        make_sess(62481, char_b, ship_b),
    );
    let h_b = tokio::spawn(async move {
        b_b.wait().await;
        advance_site_wave_or_complete(&ctx_b, &mut sess_b, sys_id, site_id).await
    });

    let mut oks = Vec::new();
    let mut errs = Vec::new();
    for res in [h_a.await.unwrap(), h_b.await.unwrap()] {
        match res {
            Ok(rep) => oks.push(rep),
            Err(err) => errs.push(err),
        }
    }
    assert_eq!(oks.len(), 1, "exactly one concurrent final-wave advance must win");
    assert_eq!(errs.len(), 1, "second concurrent final-wave advance must reject");
    assert_eq!(errs[0].code, "SiteAlreadyCleared");
    assert_eq!(oks[0].dict_get("completed"), Some(&PyRep::Bool(true)));
    assert!(matches!(
        oks[0].dict_get("droppedLoot"),
        Some(PyRep::List(loot)) if loot.len() == 1
    ));

    read_pve_exploration_reg(&ctx, |reg| {
        let site = reg.sites_by_system[&sys_id]
            .iter()
            .find(|s| s.site_id == site_id)
            .unwrap();
        assert!(site.cleared);
    });
    ctx.db.with_snapshot(|snap| {
        assert!(snap.cosmic_anomalies[&sys_id]
            .iter()
            .find(|a| a.anomaly_id == site_id)
            .unwrap()
            .cleared);
        assert_eq!(
            snap.items.len() - items_before,
            2,
            "final-wave completion must drop exactly one container + one loot stack"
        );
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn advance_site_wave_or_complete_rejects_with_site_wave_changed_on_mid_await_race() {
    let sys_id: SolarSystemId = 30_000_142;
    let char_a: i64 = 90_063_480;
    let ship_a: i64 = 1_063_480_001;
    let char_b: i64 = 90_063_481;
    let ship_b: i64 = 1_063_481_001;

    let sde = shared_sde();
    let db = Arc::new(WorldRepository::new_empty());
    db.provision_gm_account("wave_race_a", "pw", char_a, "WaveRaceA");
    db.provision_gm_account("wave_race_b", "pw", char_b, "WaveRaceB");
    db.with_mut_snapshot(|snap| {
        for (cid, sid) in [(char_a, ship_a), (char_b, ship_b)] {
            if let Some(ch) = snap.characters.get_mut(&cid) {
                ch.solar_system_id = sys_id;
                ch.active_ship_id = sid;
                ch.station_id = None;
                ch.structure_id = None;
            }
            snap.items.insert(
                sid,
                InventoryItemRecord {
                    item_id: sid,
                    type_id: 587,
                    owner_id: cid,
                    location_id: sys_id as i64,
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
        }
    });

    let ctx = Arc::new(ServerContext::with_sde_and_repository(sde, db));
    let base_now = 1_776_220_000_u64;
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(base_now)));

    let site_pos = [40_000_000.0, 0.0, 0.0];
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    for (cid, sid) in [(char_a, ship_a), (char_b, ship_b)] {
        let _ = sys_handle
            .spawn_ball_with_combat(
                Ball {
                    id: sid,
                    type_id: 587,
                    position: DVec3::from_array(site_pos),
                    velocity: DVec3::ZERO,
                    max_velocity: 350.0,
                    inertia: 0.5,
                    mass: 1_000_000.0,
                    radius: 50.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                ctx.build_ship_combat_state(cid, sid, 587),
            )
            .await;
    }

    let hook_site_id = "ANOM-WAVE-RACE";
    let conc_site_id = "ANOM-WAVE-CONC";
    with_pve_exploration_reg(&ctx, |reg| {
        reg.seeded_systems.insert(sys_id, true);
        let sites = reg.sites_by_system.entry(sys_id).or_default();
        sites.push(PveSiteInstance {
            site_id: hook_site_id.into(),
            dungeon_id: 1420,
            dungeon_name: "Guristas Wave Hook Race".into(),
            solar_system_id: sys_id,
            site_category: "CombatAnomaly".into(),
            position: site_pos,
            signal_strength: 1.0,
            current_room: 1,
            total_rooms: 3,
            current_wave: 1,
            total_waves: 3,
            active_npc_ball_ids: Vec::new(),
            cleared: false,
            created_unix: base_now,
            lifetime_seconds: 86_400,
            ..Default::default()
        });
        sites.push(PveSiteInstance {
            site_id: conc_site_id.into(),
            dungeon_id: 1421,
            dungeon_name: "Guristas Wave Concurrent Race".into(),
            solar_system_id: sys_id,
            site_category: "CombatAnomaly".into(),
            position: site_pos,
            signal_strength: 1.0,
            current_room: 1,
            total_rooms: 2,
            current_wave: 1,
            total_waves: 2,
            active_npc_ball_ids: Vec::new(),
            cleared: false,
            created_unix: base_now,
            lifetime_seconds: 86_400,
            ..Default::default()
        });
    });

    let make_sess = |conn_id: u64, cid: i64, sid: i64| {
        let mut s = SessionState::new(conn_id);
        s.user_id = Some(1);
        s.character_id = Some(cid);
        s.solar_system_id = Some(sys_id);
        s.ship_id = Some(sid);
        s
    };

    // 1. Deterministic mid-await race: Task B advances Wave 1 -> 2 across Task A's precheck/await window.
    let mut sess_a = make_sess(63480, char_a, ship_a);
    let mut sess_b = make_sess(63481, char_b, ship_b);
    let hook_ctx = Arc::clone(&ctx);
    let mut hook_sess = sess_b.clone();
    let err_a = advance_site_wave_or_complete_with_mid_await_hook(
        &ctx,
        &mut sess_a,
        sys_id,
        hook_site_id,
        || async move {
            let rep_b = advance_site_wave_or_complete(&hook_ctx, &mut hook_sess, sys_id, hook_site_id)
                .await
                .expect("concurrent Task B must advance wave 1 -> 2");
            assert_eq!(rep_b.dict_get("currentWave"), Some(&PyRep::Integer(2)));
            assert_eq!(rep_b.dict_get("completed"), Some(&PyRep::Bool(false)));
        },
    )
    .await
    .expect_err("stale Task A must reject when current_wave changed across await");
    assert_eq!(err_a.code, "SiteWaveChanged");

    read_pve_exploration_reg(&ctx, |reg| {
        let site = reg.sites_by_system[&sys_id]
            .iter()
            .find(|s| s.site_id == hook_site_id)
            .unwrap();
        assert_eq!(site.current_wave, 2, "stale caller must not skip wave 2 -> 3");
        assert!(!site.cleared);
        assert!(!site.active_npc_ball_ids.is_empty());
    });

    // 2. Real unhooked concurrent tokio::spawn race on Wave 1 of a 2-wave site: only one advances to Wave 2.
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let (ctx_1, b_1) = (Arc::clone(&ctx), Arc::clone(&barrier));
    let h_1 = tokio::spawn(async move {
        b_1.wait().await;
        advance_site_wave_or_complete(&ctx_1, &mut sess_a, sys_id, conc_site_id).await
    });
    let (ctx_2, b_2) = (Arc::clone(&ctx), Arc::clone(&barrier));
    let h_2 = tokio::spawn(async move {
        b_2.wait().await;
        advance_site_wave_or_complete(&ctx_2, &mut sess_b, sys_id, conc_site_id).await
    });

    let mut conc_oks = Vec::new();
    let mut conc_errs = Vec::new();
    for res in [h_1.await.unwrap(), h_2.await.unwrap()] {
        match res {
            Ok(rep) => conc_oks.push(rep),
            Err(err) => conc_errs.push(err),
        }
    }
    assert_eq!(conc_oks.len(), 1);
    assert_eq!(conc_errs.len(), 1);
    assert_eq!(conc_oks[0].dict_get("currentWave"), Some(&PyRep::Integer(2)));
    assert_eq!(conc_oks[0].dict_get("completed"), Some(&PyRep::Bool(false)));
    assert!(
        matches!(conc_errs[0].code.as_str(), "SiteWaveChanged" | "WaveNpcsRemaining"),
        "expected SiteWaveChanged or WaveNpcsRemaining, got {}",
        conc_errs[0].code
    );
    read_pve_exploration_reg(&ctx, |reg| {
        let site = reg.sites_by_system[&sys_id]
            .iter()
            .find(|s| s.site_id == conc_site_id)
            .unwrap();
        assert_eq!(site.current_wave, 2);
        assert!(!site.cleared);
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_npc_destruction_and_gate_advance_preserve_wave_next_balls() {
    let sys_id: SolarSystemId = 30_000_142;
    let char_id: i64 = 90_064_480;
    let ship_id: i64 = 1_064_480_001;

    let sde = shared_sde();
    let db = Arc::new(WorldRepository::new_empty());
    db.provision_gm_account("bf1480_01", "pw", char_id, "Bf1480_01");
    db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.active_ship_id = ship_id;
            ch.station_id = None;
            ch.structure_id = None;
        }
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });

    let ctx = Arc::new(ServerContext::with_sde_and_repository(sde, db));
    let base_now = 1_776_230_000_u64;
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(base_now)));

    let site_pos = [50_000_000.0, 0.0, 0.0];
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: 587,
                position: DVec3::from_array(site_pos),
                velocity: DVec3::ZERO,
                max_velocity: 350.0,
                inertia: 0.5,
                mass: 1_000_000.0,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_ship_combat_state(char_id, ship_id, 587),
        )
        .await;

    let mut base_sess = SessionState::new(64480);
    base_sess.user_id = Some(1);
    base_sess.character_id = Some(char_id);
    base_sess.solar_system_id = Some(sys_id);
    base_sess.ship_id = Some(ship_id);

    // Run multiple barrier-synchronized iterations racing last-NPC destruction against
    // `advance_site_wave_or_complete` so both winner orderings are exercised.
    for iter in 0..6_i64 {
        let site_id = format!("ANOM-BF1480-01-{iter}");
        let last_npc_id = 9_640_000 + iter;
        let iter_pos = [site_pos[0] + (iter as f64) * 50_000.0, 0.0, 0.0];
        with_pve_exploration_reg(&ctx, |reg| {
            reg.seeded_systems.insert(sys_id, true);
            reg.sites_by_system.entry(sys_id).or_default().push(PveSiteInstance {
                site_id: site_id.clone(),
                dungeon_id: 1420 + iter as u32,
                dungeon_name: format!("BF-1480-01 Race Site {iter}"),
                solar_system_id: sys_id,
                site_category: "CombatAnomaly".into(),
                position: iter_pos,
                signal_strength: 1.0,
                current_room: 1,
                total_rooms: 2,
                current_wave: 1,
                total_waves: 2,
                // Last Wave-1 NPC already left ballpark right before kill hook + gate call race.
                active_npc_ball_ids: vec![last_npc_id],
                cleared: false,
                created_unix: base_now,
                lifetime_seconds: 86_400,
                ..Default::default()
            });
        });

        let barrier = Arc::new(tokio::sync::Barrier::new(2));
        let (ctx_kill, b_kill) = (Arc::clone(&ctx), Arc::clone(&barrier));
        let h_kill = tokio::spawn(async move {
            b_kill.wait().await;
            handle_pve_site_npc_destruction_core(
                &ctx_kill.systems,
                &ctx_kill.db,
                &ctx_kill.sde,
                &ctx_kill.mechanic_rng,
                &*ctx_kill.combat_ephemeral,
                sys_id,
                last_npc_id,
                char_id,
                false,
            )
            .await
        });

        let (ctx_gate, b_gate, sid_gate, mut sess_gate) = (
            Arc::clone(&ctx),
            Arc::clone(&barrier),
            site_id.clone(),
            base_sess.clone(),
        );
        let h_gate = tokio::spawn(async move {
            b_gate.wait().await;
            advance_site_wave_or_complete(&ctx_gate, &mut sess_gate, sys_id, &sid_gate).await
        });

        let _kill_res = h_kill.await.unwrap();
        let gate_res = h_gate.await.unwrap();
        if let Ok(rep) = &gate_res {
            assert_eq!(rep.dict_get("currentWave"), Some(&PyRep::Integer(2)));
            assert_eq!(rep.dict_get("completed"), Some(&PyRep::Bool(false)));
        }

        let wave2_balls = read_pve_exploration_reg(&ctx, |reg| {
            let site = reg.sites_by_system[&sys_id]
                .iter()
                .find(|s| s.site_id == site_id)
                .unwrap();
            assert_eq!(site.current_wave, 2, "iteration {iter} must land on wave 2");
            assert!(!site.cleared, "iteration {iter} must not clear before wave 2");
            assert!(
                !site.active_npc_ball_ids.is_empty(),
                "iteration {iter} must retain wave 2 NPC ball IDs"
            );
            site.active_npc_ball_ids.clone()
        });

        for bid in wave2_balls {
            assert!(
                sys_handle.get_ball_snapshot(bid).await.unwrap().is_some(),
                "iteration {iter} live wave 2 NPC ball {bid} must not be removed by losing cleanup"
            );
        }

        let mut follow_sess = base_sess.clone();
        let skip_err = advance_site_wave_or_complete(&ctx, &mut follow_sess, sys_id, &site_id)
            .await
            .expect_err("wave 2 live NPCs must prevent advancing/clearing site");
        assert_eq!(skip_err.code, "WaveNpcsRemaining");
    }
}
