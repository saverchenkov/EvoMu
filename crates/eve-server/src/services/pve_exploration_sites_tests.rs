//! Unit and integration tests for `pve_exploration_sites`.

use super::*;
use crate::router::ServerContext;
use crate::services::combat_crimewatch::CombatRuntimeState;
use crate::services::pve_exploration::SpewedLootCan;
use eve_db::clock::VirtualServerClock;
use eve_db::WorldRepository;
use eve_sde::{SdeRegistry, SolarSystemId};
use glam::DVec3;
use std::sync::{Arc, OnceLock, RwLock};

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

    let combat_rt = Arc::new(RwLock::new(CombatRuntimeState::default()));
    let w3_balls = spawn_sleeper_wave_core(
        &ctx.systems,
        &ctx.db,
        &ctx.sde,
        &combat_rt,
        sys_id,
        5,
        2,
        DVec3::ZERO,
    )
    .await;
    assert_eq!(w3_balls.len(), 3);
    assert_eq!(combat_rt.read().unwrap().npc_entities.len(), 3);

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
    let combat_rt = Arc::new(RwLock::new(CombatRuntimeState::default()));
    let balls = spawn_sleeper_wave_core(
        &ctx.systems,
        &ctx.db,
        &Arc::new(rep_sde),
        &combat_rt,
        sys_id,
        4,
        1,
        DVec3::ZERO,
    )
    .await;
    let rt = combat_rt.read().unwrap();
    let npc = &rt.npc_entities[&balls[0]];
    assert_eq!(npc.web_speed_factor, -65.0);
    assert_eq!(npc.remote_shield_rep, 250.0);
    assert_eq!(npc.remote_armor_rep, 180.0);
    assert_eq!(npc.remote_rep_range, 25_000.0);
}
