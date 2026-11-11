//! Unit and mutation-killing tests for `combat_npc_ai`.

use super::*;
use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::{
    make_fighter_ctx_and_session, make_fighter_type,
};
use crate::services::combat_crimewatch::DroneSpaceState;
use eve_destiny::{Ball, CombatState, DamageVector, MovementMode};
use glam::DVec3;
use std::collections::HashMap;

#[cfg(test)]
#[path = "combat_npc_ai_edges_tests.rs"]
mod combat_npc_ai_edges_tests;

pub(in crate::services) fn make_test_npc_entity(
    ball_id: i64,
    type_id: u32,
    solar_system_id: u32,
    archetype: &str,
) -> ExtendedNpcEntity {
    ExtendedNpcEntity {
        ball_id,
        type_id,
        faction_id: 500_027,
        archetype: archetype.to_string(),
        solar_system_id,
        spawn_group_id: 1,
        target_id: None,
        aggression_threat: HashMap::new(),
        threat_table: HashMap::new(),
        bounty_isk: 0.0,
        optimal_range: 15_000.0,
        orbit_range: 10_000.0,
        damage_vector: DamageVector::default(),
        entropic_cycles: 0,
        drifter_overshield_hp: 0.0,
        drifter_doomsday_fired: false,
        web_speed_factor: 0.0,
        web_range: 0.0,
        scram_strength: 0.0,
        scram_range: 0.0,
        neut_amount: 0.0,
        ecm_jammed: false,
        remote_shield_rep: 0.0,
        remote_armor_rep: 0.0,
        remote_rep_range: 0.0,
        tag_drop_type_id: None,
        spawn_position: DVec3::ZERO,
        sleeper_wh_class: 0,
        custom_cargo_loot: None,
    }
}

#[test]
fn derive_npc_archetype_covers_all_15_faction_and_group_branches() {
    let (mut ctx, _session, _char_id, _ship_id, _sys_id) = make_fighter_ctx_and_session();
    let mut sde = (*ctx.sde).clone();

    let mut insert_custom = |tid: u32, gid: u32, name: &str, faction: Option<u32>, meta_lvl: Option<i64>| {
        let mut t = make_fighter_type(tid, gid, 11, 1_000.0, vec![]);
        t.name = name.to_string();
        t.faction_id = faction;
        t.meta_level = meta_lvl;
        sde.types.insert(tid, t);
    };

    insert_custom(910_001, 959, "Sleeper Sentinel", Some(500_021), None);
    insert_custom(910_002, 287, "Rogue Alvi Drone", Some(500_008), None);
    insert_custom(910_003, 4_034, "EDENCOM GunStar", Some(500_027), None);
    insert_custom(910_004, 1_051, "Outuni Meson", None, None);
    insert_custom(910_005, 1_285, "Mordu Special Unit", None, None);
    insert_custom(910_006, 4_637, "Roaming Pirate Commander", None, Some(12));
    insert_custom(910_007, 4_638, "Roaming Enforcer", None, None);
    insert_custom(910_008, 550, "Dread Guristas Officer", None, Some(12));
    insert_custom(910_009, 1_310, "Apollo Tyrannos Drifter", Some(500_024), None);
    insert_custom(910_010, 4_028, "Raznaborg Damavik", Some(500_026), None);
    insert_custom(910_011, 550, "Clone Soldier Transporter", None, None);
    insert_custom(910_012, 1_767, "♦ Mining Hauler", None, None);
    insert_custom(910_013, 550, "Caldari Navy Raven", Some(500_001), None);
    insert_custom(910_014, 550, "Guristas Pithi Invader", None, None);

    ctx.sde = std::sync::Arc::new(sde);

    assert_eq!(derive_npc_archetype(&ctx, 41_589), "TransmissionRelay");
    assert_eq!(derive_npc_archetype(&ctx, 56_680), "PochvenHypnosianSleeper");
    assert_eq!(derive_npc_archetype(&ctx, 910_001), "Sleeper");
    assert_eq!(derive_npc_archetype(&ctx, 910_002), "RogueDrone");
    assert_eq!(derive_npc_archetype(&ctx, 910_003), "EDENCOM");
    assert_eq!(derive_npc_archetype(&ctx, 910_004), "IncursionSansha");
    assert_eq!(derive_npc_archetype(&ctx, 910_005), "MorduCommander");
    assert_eq!(derive_npc_archetype(&ctx, 910_006), "InsurgencyPirateRoamer");
    assert_eq!(derive_npc_archetype(&ctx, 910_007), "InsurgencyEnforcer");
    assert_eq!(derive_npc_archetype(&ctx, 910_008), "Officer");
    assert_eq!(derive_npc_archetype(&ctx, 910_009), "Drifter");
    assert_eq!(derive_npc_archetype(&ctx, 910_010), "Triglavian");
    assert_eq!(derive_npc_archetype(&ctx, 910_011), "CloneSoldier");
    assert_eq!(derive_npc_archetype(&ctx, 910_012), "Hauler");
    assert_eq!(derive_npc_archetype(&ctx, 910_013), "Navy");
    assert_eq!(derive_npc_archetype(&ctx, 910_014), "Pirate");
}

#[test]
fn is_candidate_hostile_to_edencom_evaluates_standings_fleets_drones_and_npcs() {
    let (ctx, _session, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    let wingman_char_id = 90_000_002_i64;
    let wingman_ship_id = 200_000_002_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut wing = snap.characters.get(&char_id).cloned().unwrap();
        wing.character_id = wingman_char_id;
        wing.active_ship_id = wingman_ship_id;
        wing.fleet_id = Some(777);
        snap.characters.insert(wingman_char_id, wing);
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.fleet_id = Some(777);
        }
    });

    let mut edencom_npc = make_test_npc_entity(500_001, 54_800, sys_id, "EDENCOM");
    edencom_npc.target_id = Some(ship_id);

    // Wingman in same fleet as primary target is hostile
    assert!(is_candidate_hostile_to_edencom(
        &ctx,
        &edencom_npc,
        wingman_ship_id,
        ship_id
    ));

    // Different fleet and neutral standing -> not hostile; negative standing -> hostile
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&wingman_char_id).unwrap().fleet_id = None;
    });
    assert!(!is_candidate_hostile_to_edencom(
        &ctx,
        &edencom_npc,
        wingman_ship_id,
        ship_id
    ));
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters
            .get_mut(&wingman_char_id)
            .unwrap()
            .standings
            .insert(500_027, -1.5);
    });
    assert!(is_candidate_hostile_to_edencom(
        &ctx,
        &edencom_npc,
        wingman_ship_id,
        ship_id
    ));

    // Drone owned by primary target is hostile
    let drone_id = 600_001_i64;
    with_mut_combat_ephemeral(&ctx, |rt| {
        rt.active_drones_in_space.insert(
            drone_id,
            DroneSpaceState {
                ball_id: drone_id,
                drone_item_id: drone_id,
                type_id: 2_454,
                group_id: 100,
                owner_char_id: char_id,
                original_owner_char_id: char_id,
                parent_ship_id: ship_id,
                solar_system_id: sys_id,
                target_id: None,
                activity_state: 0,
                assist_char_id: None,
                guard_char_id: None,
                is_abandoned: false,
                is_sentry: false,
                bandwidth_used: 5.0,
                max_velocity: 1_000.0,
                optimal_range: 5_000.0,
                damage_vector: DamageVector::default(),
                shield_repair_amount: 0.0,
                armor_repair_amount: 0.0,
                hull_repair_amount: 0.0,
                web_speed_factor: 0.0,
                neut_amount: 0.0,
                ecm_strength: 0.0,
                tp_sig_bonus_pct: 0.0,
            },
        );
    });
    assert!(is_candidate_hostile_to_edencom(
        &ctx,
        &edencom_npc,
        drone_id,
        ship_id
    ));

    // Triglavian NPC is hostile to EDENCOM; another EDENCOM NPC is not
    let trig_ball_id = 500_002_i64;
    let ally_ball_id = 500_003_i64;
    with_mut_combat_ephemeral(&ctx, |rt| {
        let mut trig = edencom_npc.clone();
        trig.ball_id = trig_ball_id;
        trig.archetype = "Triglavian".into();
        trig.faction_id = 500_026;
        rt.npc_entities.insert(trig_ball_id, trig);

        let mut ally = edencom_npc.clone();
        ally.ball_id = ally_ball_id;
        rt.npc_entities.insert(ally_ball_id, ally);
    });
    assert!(is_candidate_hostile_to_edencom(
        &ctx,
        &edencom_npc,
        trig_ball_id,
        ship_id
    ));
    assert!(!is_candidate_hostile_to_edencom(
        &ctx,
        &edencom_npc,
        ally_ball_id,
        ship_id
    ));
}

#[tokio::test]
async fn spawn_threat_ai_step_and_destroy_specialized_npc_loot_and_bounties() {
    let (mut ctx, _session, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    let mut sde = (*ctx.sde).clone();
    let mut clone_t = make_fighter_type(
        920_001,
        550,
        11,
        1_000.0,
        vec![(eve_sde::attr_ids::ENTITY_KILL_BOUNTY, 250_000.0)],
    );
    clone_t.name = "Clone Soldier Negotiator".into();
    sde.types.insert(920_001, clone_t);

    let mut drifter_t = make_fighter_type(
        37_473,
        1_310,
        11,
        5_000.0,
        vec![(eve_sde::attr_ids::ENTITY_KILL_BOUNTY, 500_000.0)],
    );
    drifter_t.name = "Apollo Tyrannos".into();
    sde.types.insert(37_473, drifter_t);

    let mut hauler_t = make_fighter_type(
        920_002,
        1_767,
        11,
        10_000.0,
        vec![(eve_sde::attr_ids::ENTITY_KILL_BOUNTY, 50_000.0)],
    );
    hauler_t.name = "♦ Mining Hauler".into();
    sde.types.insert(920_002, hauler_t);
    ctx.sde = std::sync::Arc::new(sde);

    // Spawn player ship in space so step_extended_npc_ai_sync can evaluate targets
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: 23_915,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 200.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState {
                shield_hp: 1_000.0,
                max_shield_hp: 1_000.0,
                armor_hp: 1_000.0,
                max_armor_hp: 1_000.0,
                structure_hp: 1_000.0,
                max_structure_hp: 1_000.0,
                signature_radius: 40.0,
                ..CombatState::default()
            },
        )
        .await
        .unwrap();

    let clone_id =
        spawn_npc_entity_from_sde(&ctx, sys_id, 920_001, 1, DVec3::new(2_000.0, 0.0, 0.0)).await;
    let drifter_id = spawn_specialized_npc_entity(
        &ctx,
        sys_id,
        37_473,
        "Drifter",
        1,
        DVec3::new(3_000.0, 0.0, 0.0),
    )
    .await;
    let hauler_id =
        spawn_npc_entity_from_sde(&ctx, sys_id, 920_002, 1, DVec3::new(4_000.0, 0.0, 0.0)).await;

    // Record threat events (normal, EWAR 1.5x, logistics 1.3x) and verify Drifter overshield reduction
    with_mut_combat_ephemeral(&ctx, |rt| {
        rt.npc_entities.get_mut(&drifter_id).unwrap().drifter_overshield_hp = 1_000.0;
    });
    record_npc_threat_event(&ctx, drifter_id, ship_id, 300.0, false, false);
    record_npc_threat_event(&ctx, drifter_id, ship_id, 0.0, true, false);
    record_npc_threat_event(&ctx, drifter_id, ship_id, 0.0, false, true);
    with_combat_ephemeral(&ctx, |rt| {
        let d = rt.npc_entities.get(&drifter_id).unwrap();
        assert!((d.drifter_overshield_hp - 700.0).abs() < 1e-6);
        let threat = d.threat_table.get(&ship_id).copied().unwrap_or(0.0);
        // 100 base + (300+10)*1.0 + (120+10)*1.5 + (120+10)*1.3 = 100 + 310 + 195 + 169 = 774
        assert!((threat - 774.0).abs() < 1e-6);
    });

    // Step AI tick
    step_extended_npc_ai(&ctx, sys_id).await;

    // Destroy Clone Soldier -> sets cooldown and spawns wreck
    let (wreck_clone, bounty_clone, _drops_clone) =
        destroy_specialized_npc_and_queue_bounty(&ctx, sys_id, clone_id, char_id).await;
    assert!(wreck_clone > 0);
    assert!((bounty_clone - 250_000.0).abs() < 1e-2);
    with_combat_ephemeral(&ctx, |rt| {
        assert!(rt.clone_soldier_cooldown_until.contains_key(&sys_id));
    });

    // Destroy Hauler -> drops mineral types [34, 35, 36, 3828] with 50,000 qty for 34/35
    let (wreck_hauler, _bounty_hauler, drops_hauler) =
        destroy_specialized_npc_and_queue_bounty(&ctx, sys_id, hauler_id, char_id).await;
    assert!(drops_hauler.contains(&34) && drops_hauler.contains(&35));
    ctx.db.with_snapshot(|snap| {
        let trit_qty: i32 = snap
            .items
            .values()
            .filter(|i| i.location_id == wreck_hauler && i.type_id == 34)
            .map(|i| i.quantity)
            .sum();
        assert_eq!(trit_qty, 50_000);
    });

    // Verify derive_npc_archetype exact ID and group boundaries
    assert_eq!(derive_npc_archetype(&ctx, 41_589), "TransmissionRelay");
    assert_eq!(derive_npc_archetype(&ctx, 30_460), "SleeperSentry");
    assert_eq!(derive_npc_archetype(&ctx, 56_680), "PochvenHypnosianSleeper");
    assert_eq!(derive_npc_archetype(&ctx, 56_319), "PochvenHypnosianSleeper");
    assert_eq!(derive_npc_archetype(&ctx, 33_864), "MorduCommander");
}

#[tokio::test]
async fn gh829_navy_classification_and_npc_corp_id_resolution_across_spawners() {
    let (mut ctx, _session, _char_id, _ship_id, sys_id) = make_fighter_ctx_and_session();
    {
        let sde = std::sync::Arc::make_mut(&mut ctx.sde);
        sde.groups.insert(
            4034,
            eve_sde::GroupInfo {
                group_id: 4034,
                category_id: 11,
                name: "EDENCOM Entities".into(),
                anchorable: false,
                anchored: false,
                fittable_non_singleton: false,
                use_base_price: false,
                published: false,
            },
        );
        let mut state_cerberus = make_fighter_type(52_936, 4034, 11, 12_000_000.0, vec![]);
        state_cerberus.name = "State Navy Cerberus".into();
        state_cerberus.faction_id = Some(500_027);
        sde.types.insert(52_936, state_cerberus);

        let mut republic_muninn = make_fighter_type(52_959, 4034, 11, 12_000_000.0, vec![]);
        republic_muninn.name = "Republic Fleet Muninn".into();
        republic_muninn.faction_id = Some(500_027);
        sde.types.insert(52_959, republic_muninn);

        let mut edencom_orca = make_fighter_type(54_335, 4034, 11, 150_000_000.0, vec![]);
        edencom_orca.name = "EDENCOM Fortification Orca".into();
        edencom_orca.faction_id = Some(500_027);
        sde.types.insert(54_335, edencom_orca);

        let mut pirate_rat = make_fighter_type(23_335, 550, 11, 1_000_000.0, vec![]);
        pirate_rat.name = "Guristas Saboteur".into();
        pirate_rat.faction_id = Some(500_010);
        sde.types.insert(23_335, pirate_rat);

        let mut sleeper_guard = make_fighter_type(30_197, 987, 11, 5_000_000.0, vec![]);
        sleeper_guard.name = "Emergent Patroller".into();
        sleeper_guard.faction_id = Some(500_021);
        sde.types.insert(30_197, sleeper_guard);

        let mut sentry_tower = make_fighter_type(30_457, 991, 11, 10_000_000.0, vec![]);
        sentry_tower.name = "Sleeper Sentry Gun".into();
        sentry_tower.faction_id = Some(500_021);
        sde.types.insert(30_457, sentry_tower);

        // Multiple Caldari NPC corps ensure deterministic .min() selection (1_000_002 < 1_000_035)
        for (cid, cname, fid) in [
            (1_000_035, "Caldari Navy", Some(500_001)),
            (1_000_002, "CBD Corporation", Some(500_001)),
            (1_000_127, "Guristas", Some(500_010)),
            (1_000_431, "The Sleepers", Some(500_021)),
            (1_000_297, "EDENCOM Defense Initiative", Some(500_027)),
        ] {
            sde.npc_corporations.insert(
                cid,
                eve_sde::NpcCorporationInfo {
                    corporation_id: cid,
                    name: cname.into(),
                    ticker_name: "TKR".into(),
                    faction_id: fid,
                    solar_system_id: None,
                    station_id: None,
                    tax_rate: 0.1,
                },
            );
        }

        sde.npc_spawn_groups.insert(
            77_001,
            eve_sde::NpcSpawnGroupInfo {
                spawn_group_id: 77_001,
                faction_id: 500_010,
                ship_class: 1,
                group_id: 550,
                group_name: "Guristas Test Patrol".into(),
                npc_type_ids: vec![23_335],
            },
        );
    }

    // 1. Secondary fix: Navy/Republic Fleet hulls in group 4034 classify as "Navy", not "EDENCOM"
    assert_eq!(derive_npc_archetype(&ctx, 52_936), "Navy");
    assert_eq!(derive_npc_archetype(&ctx, 52_959), "Navy");
    assert_eq!(derive_npc_archetype(&ctx, 54_335), "EDENCOM");

    // 2. Deterministic NPC corp lookup + -1 fallback for unknown faction (never faction IDs)
    assert_eq!(resolve_npc_corp_id_for_faction(&ctx, 500_001), 1_000_002);
    assert_eq!(resolve_npc_corp_id_for_faction(&ctx, 500_010), 1_000_127);
    assert_eq!(resolve_npc_corp_id_for_faction(&ctx, 500_022), -1);

    // 3. spawn_specialized_npc_entity: State Navy Cerberus with Caldari sys_faction 500_001
    let navy_id = spawn_specialized_npc_entity(
        &ctx,
        sys_id,
        52_936,
        "CaldariNavy",
        500_001,
        glam::DVec3::new(10_000.0, 0.0, 0.0),
    )
    .await;

    // 4. NpcSpawnManager::spawn_group, spawn_wormhole_ore_guards, spawn_sleeper_sentry_tower
    let mut spawn_mgr = crate::combat::npc_ai::NpcSpawnManager::new();
    let group_ids = spawn_mgr
        .spawn_group(&ctx, sys_id, 77_001, glam::DVec3::new(20_000.0, 0.0, 0.0))
        .await
        .expect("spawn_group succeeds");
    assert!(!group_ids.is_empty());

    let ore_guard_id = 910_829_001i64;
    crate::services::space_mining_harvest::spawn_wormhole_ore_guards(
        &ctx,
        sys_id,
        vec![(ore_guard_id, 30_197, glam::DVec3::new(30_000.0, 0.0, 0.0))],
    )
    .await;

    let tower_id = crate::services::wormhole_pve_seeding::spawn_sleeper_sentry_tower(
        &ctx,
        sys_id,
        3,
        glam::DVec3::new(40_000.0, 0.0, 0.0),
    )
    .await;

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let bp = sys_handle.ballpark.read().unwrap();

    let navy_meta = bp.ball_slim_meta.get(&navy_id).expect("State Navy Cerberus SlimItemMeta");
    assert_eq!(navy_meta.owner_id, 500_001, "Caldari Navy Cerberus must belong to Caldari (500001), not EDENCOM (500027)");
    assert_eq!(navy_meta.corp_id, 1_000_002, "corp_id must resolve to Caldari NPC corp, never faction ID 500027");

    for (label, id, expected_corp) in [
        ("spawn_group", group_ids[0], 1_000_127),
        ("ore_guard", ore_guard_id, 1_000_431),
        ("sleeper_sentry_tower", tower_id, 1_000_149),
    ] {
        let meta = bp.ball_slim_meta.get(&id).unwrap_or_else(|| panic!("{label} SlimItemMeta missing"));
        assert!(
            !(500_001..=500_030).contains(&meta.corp_id),
            "{label} SlimItemMeta.corp_id must never be a faction ID (got {})",
            meta.corp_id
        );
        assert_eq!(meta.corp_id, expected_corp, "{label} should resolve deterministic NPC corp ID");
    }
}

#[tokio::test]
async fn step_ai_scram_cleanup_stale_target_reset_and_slim_meta_field_coverage() {
    let (mut ctx, _session, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    {
        let sde = std::sync::Arc::make_mut(&mut ctx.sde);
        let mut factionless_rat = make_fighter_type(920_080, 550, 11, 1_000_000.0, vec![]);
        factionless_rat.name = "Factionless Raider".into();
        factionless_rat.faction_id = Some(0);
        sde.types.insert(920_080, factionless_rat);

        let mut pirate_rat = make_fighter_type(
            920_081,
            550,
            11,
            1_000_000.0,
            vec![(eve_sde::attr_ids::ENTITY_KILL_BOUNTY, 120_000.0)],
        );
        pirate_rat.name = "Guristas Raider".into();
        pirate_rat.faction_id = Some(500_010);
        sde.types.insert(920_081, pirate_rat);

        let mut drifter_bs = make_fighter_type(
            37_473,
            1_310,
            11,
            5_000.0,
            vec![(eve_sde::attr_ids::ENTITY_KILL_BOUNTY, 500_000.0)],
        );
        drifter_bs.name = "Apollo Tyrannos".into();
        sde.types.insert(37_473, drifter_bs);

        let mut drifter_wreck = make_fighter_type(37_531, 186, 2, 100_000.0, vec![]);
        drifter_wreck.name = "Drifter Response Battleship Wreck".into();
        sde.types.insert(37_531, drifter_wreck);
    }

    // 1. spawn_specialized_npc_entity: full SlimItemMeta field coverage & faction_id == 0 fallback -> 500_001
    let fallback_id = spawn_specialized_npc_entity(
        &ctx,
        sys_id,
        920_080,
        "Pirate",
        0,
        DVec3::new(1_000.0, 0.0, 0.0),
    )
    .await;
    assert!(fallback_id > 0);

    let pirate_id = spawn_specialized_npc_entity(
        &ctx,
        sys_id,
        920_081,
        "Pirate",
        0,
        DVec3::new(2_500.0, 0.0, 0.0),
    )
    .await;
    assert!(pirate_id > 0);

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    {
        let bp = sys_handle.ballpark.read().unwrap();
        let fb_meta = bp.ball_slim_meta.get(&fallback_id).expect("fallback meta");
        assert_eq!(fb_meta.group_id, 550);
        assert_eq!(fb_meta.category_id, 11);
        assert_eq!(fb_meta.owner_id, 500_001);
        assert_eq!(fb_meta.char_id, None);
        assert_eq!(fb_meta.corp_id, 1_000_002);
        assert_eq!(fb_meta.alliance_id, -1);
        assert!((fb_meta.security_status - (-1.0)).abs() < 1e-6);
        assert!(!fb_meta.is_cloaked);
        assert_eq!(fb_meta.name.as_deref(), Some("Factionless Raider"));

        let pir_meta = bp.ball_slim_meta.get(&pirate_id).expect("pirate meta");
        assert_eq!(pir_meta.owner_id, 500_010);
        assert_eq!(pir_meta.corp_id, 1_000_127);
        assert_eq!(pir_meta.name.as_deref(), Some("Guristas Raider"));
    }

    // 2. step_extended_npc_ai_sync: non-empty NPC acquires ship_id, damages it, and persists target_id (L385 &&->||, L913 !=)
    sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: 23_915,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 200.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState {
                shield_hp: 5_000.0,
                max_shield_hp: 5_000.0,
                armor_hp: 5_000.0,
                max_armor_hp: 5_000.0,
                structure_hp: 5_000.0,
                max_structure_hp: 5_000.0,
                signature_radius: 40.0,
                ..CombatState::default()
            },
        )
        .await
        .unwrap();

    step_extended_npc_ai(&ctx, sys_id).await;
    with_combat_ephemeral(&ctx, |rt| {
        let p = rt.npc_entities.get(&pirate_id).expect("pirate tracked");
        assert_eq!(p.target_id, Some(ship_id), "NPC must persist acquired target_id");
    });
    let ship_shield_after = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap().shield_hp;
    assert!(ship_shield_after < 5_000.0, "step_extended_npc_ai must apply NPC damage");

    // 3. Stale target_id is cleared and persisted when no targets exist (L753 != -> ==)
    let stale_sys = 30_000_144_u32;
    let stale_npc_id = spawn_specialized_npc_entity(
        &ctx,
        stale_sys,
        920_081,
        "Pirate",
        0,
        DVec3::new(1_000.0, 0.0, 0.0),
    )
    .await;
    with_mut_combat_ephemeral(&ctx, |rt| {
        rt.npc_entities.get_mut(&stale_npc_id).unwrap().target_id = Some(999_888);
    });
    step_extended_npc_ai(&ctx, stale_sys).await;
    with_combat_ephemeral(&ctx, |rt| {
        assert_eq!(rt.npc_entities[&stale_npc_id].target_id, None);
    });

    // 4. Empty-NPC system with leftover scrammed ship clears scram flag (L385 &&->|| and !has_scrammed)
    let scram_sys = 30_000_145_u32;
    let scram_ship = 900_888_001_i64;
    let scram_handle = ctx.systems.get_or_spawn(scram_sys);
    scram_handle
        .spawn_ball_with_combat(
            Ball {
                id: scram_ship,
                type_id: 23_915,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 200.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState {
                warp_scramble_strength: 2,
                ..CombatState::default()
            },
        )
        .await
        .unwrap();
    with_mut_combat_ephemeral(&ctx, |rt| {
        rt.npc_scrambled_ships
            .insert(scram_sys, [scram_ship].into_iter().collect());
    });
    step_extended_npc_ai(&ctx, scram_sys).await;
    with_combat_ephemeral(&ctx, |rt| {
        assert!(
            !rt.npc_scrambled_ships.contains_key(&scram_sys),
            "empty-NPC system must still clear stale scram tracking"
        );
    });
    assert_eq!(
        scram_handle
            .get_combat_state(scram_ship)
            .await
            .unwrap()
            .unwrap()
            .warp_scramble_strength,
        0
    );

    // 5. destroy_specialized_npc_with_hint return value + wreck SlimItemMeta fields (L1420-1430)
    let drifter_id = spawn_specialized_npc_entity(
        &ctx,
        sys_id,
        37_473,
        "Drifter",
        0,
        DVec3::new(3_500.0, 0.0, 0.0),
    )
    .await;
    let (wreck_id, bounty, drops) =
        destroy_specialized_npc_and_queue_bounty(&ctx, sys_id, drifter_id, char_id).await;
    assert!(wreck_id > 0);
    assert!((bounty - 500_000.0).abs() < 1e-3);
    assert!(!drops.is_empty());

    let bp = sys_handle.ballpark.read().unwrap();
    let w_meta = bp.ball_slim_meta.get(&wreck_id).expect("wreck SlimItemMeta");
    assert_eq!(w_meta.group_id, 186);
    assert_eq!(w_meta.category_id, 2);
    assert_eq!(w_meta.owner_id, char_id);
    assert_eq!(w_meta.char_id, None);
    assert_eq!(w_meta.corp_id, -1);
    assert_eq!(w_meta.alliance_id, -1);
    assert_eq!(w_meta.security_status, 0.0);
    assert!(!w_meta.is_cloaked);
    assert_eq!(w_meta.name.as_deref(), Some("Drifter Response Battleship Wreck"));
}
