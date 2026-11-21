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
    insert_custom(910_002, 287, "Rogue Alvi Drone", Some(500_025), None);
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
                    ceo_id: None,
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

#[tokio::test]
async fn empire_sentry_gun_archetype_never_engages_non_criminal_venture() {
    let (mut ctx, _session, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    {
        let sde = std::sync::Arc::make_mut(&mut ctx.sde);
        sde.groups.insert(
            99,
            eve_sde::GroupInfo {
                group_id: 99,
                category_id: 11,
                name: "Sentry Gun".into(),
                anchorable: false,
                anchored: false,
                fittable_non_singleton: false,
                use_base_price: false,
                published: false,
            },
        );
        sde.groups.insert(
            180,
            eve_sde::GroupInfo {
                group_id: 180,
                category_id: 11,
                name: "Protective Sentry Gun".into(),
                anchorable: false,
                anchored: false,
                fittable_non_singleton: false,
                use_base_price: false,
                published: false,
            },
        );
        sde.groups.insert(
            383,
            eve_sde::GroupInfo {
                group_id: 383,
                category_id: 11,
                name: "Destructible Sentry Gun".into(),
                anchorable: false,
                anchored: false,
                fittable_non_singleton: false,
                use_base_price: false,
                published: false,
            },
        );

        let mut caldari_sentry = make_fighter_type(3_740, 99, 11, 1_000_000.0, vec![]);
        caldari_sentry.name = "Caldari Sentry Gun I".into();
        caldari_sentry.faction_id = Some(500_001);
        sde.types.insert(3_740, caldari_sentry);

        let mut amarr_sentry = make_fighter_type(1_194, 99, 11, 1_000_000.0, vec![]);
        amarr_sentry.name = "Amarr Sentry Gun".into();
        amarr_sentry.faction_id = None;
        sde.types.insert(1_194, amarr_sentry);

        let mut navy_sentry = make_fighter_type(990_101, 99, 11, 1_000_000.0, vec![]);
        navy_sentry.name = "Caldari Navy Sentry Gun".into();
        navy_sentry.faction_id = Some(500_001);
        sde.types.insert(990_101, navy_sentry);

        let mut police_sentry = make_fighter_type(990_102, 180, 11, 1_000_000.0, vec![]);
        police_sentry.name = "State Police Sentry Gun".into();
        police_sentry.faction_id = None;
        sde.types.insert(990_102, police_sentry);

        let mut ci_sentry = make_fighter_type(79_507, 383, 11, 1_000_000.0, vec![]);
        ci_sentry.name = "Counter-Insurgency Sentry Gun".into();
        ci_sentry.faction_id = Some(500_001);
        sde.types.insert(79_507, ci_sentry);

        // Set 1.0 security so non-outlaw -2.0 security status crosses Faction Police threshold.
        if let Some(sys) = sde.solar_systems.get_mut(&sys_id) {
            sys.security_status = 1.0;
        }
    }

    assert_eq!(derive_npc_archetype(&ctx, 3_740), "EmpireSentry");
    assert_eq!(derive_npc_archetype(&ctx, 1_194), "EmpireSentry");
    assert_eq!(derive_npc_archetype(&ctx, 990_101), "EmpireSentry");
    assert_eq!(derive_npc_archetype(&ctx, 990_102), "EmpireSentry");
    assert_ne!(derive_npc_archetype(&ctx, 79_507), "EmpireSentry");

    // Passing a poisoned "BeltRat" hint must still resolve to EmpireSentry, never Pirate/BeltRat.
    let sentry_id = spawn_specialized_npc_entity(
        &ctx,
        sys_id,
        3_740,
        "BeltRat",
        1,
        DVec3::new(10_000.0, 0.0, 0.0),
    )
    .await;
    // Also spawn 1_194 at 200 km (> 150 km normal range, within 500 km sentry range).
    let amarr_sentry_id = spawn_specialized_npc_entity(
        &ctx,
        sys_id,
        1_194,
        "",
        1,
        DVec3::new(200_000.0, 0.0, 0.0),
    )
    .await;
    with_combat_ephemeral(&ctx, |rt| {
        let s1 = &rt.npc_entities[&sentry_id];
        assert_eq!(s1.archetype, "EmpireSentry");
        assert_eq!(s1.faction_id, 500_001);
        assert_eq!(s1.orbit_range, 0.0);
        assert!(s1.optimal_range >= 500_000.0);

        let s2 = &rt.npc_entities[&amarr_sentry_id];
        assert_eq!(s2.archetype, "EmpireSentry");
        assert_eq!(s2.faction_id, 500_003);
        assert_eq!(s2.orbit_range, 0.0);
        assert!(s2.optimal_range >= 500_000.0);
    });

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    {
        let bp = sys_handle.ballpark.read().unwrap();
        assert_eq!(bp.balls[&sentry_id].max_velocity, 0.0);
        assert_eq!(bp.balls[&amarr_sentry_id].max_velocity, 0.0);
    }
    sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: 32_880,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 250.0,
                inertia: 1.0,
                mass: 1_200_000.0,
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

    // Clean non-criminal Venture pilot is never targeted or damaged.
    step_extended_npc_ai(&ctx, sys_id).await;
    with_combat_ephemeral(&ctx, |rt| {
        assert_eq!(rt.npc_entities[&sentry_id].target_id, None);
        assert_eq!(rt.npc_entities[&amarr_sentry_id].target_id, None);
    });

    // -10.0 empire and pirate standings alone (without active criminal or on-grid aggression) must not trigger sentries.
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.standings.insert(500_001, -10.0);
        ch.standings.insert(500_003, -10.0);
        ch.standings.insert(500_010, -10.0);
    });
    step_extended_npc_ai(&ctx, sys_id).await;
    with_combat_ephemeral(&ctx, |rt| {
        assert_eq!(rt.npc_entities[&sentry_id].target_id, None);
        assert_eq!(rt.npc_entities[&amarr_sentry_id].target_id, None);
    });

    // Non-outlaw -2.0 sec status in 1.0 highsec (Faction Police threshold) and -10.0 sec status without
    // active criminal timer or on-grid illegal aggression must not be engaged by Empire Sentry Guns.
    for sec in [-2.0, -10.0] {
        ctx.db.with_mut_snapshot(|snap| {
            snap.characters.get_mut(&char_id).unwrap().security_status = sec;
        });
        step_extended_npc_ai(&ctx, sys_id).await;
        with_combat_ephemeral(&ctx, |rt| {
            assert_eq!(rt.npc_entities[&sentry_id].target_id, None);
            assert_eq!(rt.npc_entities[&amarr_sentry_id].target_id, None);
        });
    }
    assert_eq!(
        sys_handle
            .get_combat_state(ship_id)
            .await
            .unwrap()
            .unwrap()
            .shield_hp,
        5_000.0
    );

    // Active criminal timer (GCC) IS engaged and damaged up to 500 km.
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.security_status = 0.0;
        ch.standings.clear();
    });
    let now = ctx.now_unix();
    {
        let mut cw = ctx.crimewatch.write().unwrap();
        cw.get_or_create(char_id).criminal_flag_until_unix = now + 900;
    }
    step_extended_npc_ai(&ctx, sys_id).await;
    with_combat_ephemeral(&ctx, |rt| {
        assert_eq!(rt.npc_entities[&sentry_id].target_id, Some(ship_id));
        assert_eq!(rt.npc_entities[&amarr_sentry_id].target_id, Some(ship_id));
    });
    assert!(
        sys_handle
            .get_combat_state(ship_id)
            .await
            .unwrap()
            .unwrap()
            .shield_hp
            < 5_000.0
    );
}

#[tokio::test]
async fn empire_sentry_faction_resolution_on_grid_aggression_and_stage5_corruption() {
    let (mut ctx, _session, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    {
        let sde = std::sync::Arc::make_mut(&mut ctx.sde);
        sde.groups.insert(
            99,
            eve_sde::GroupInfo {
                group_id: 99,
                category_id: 11,
                name: "Sentry Gun".into(),
                anchorable: false,
                anchored: false,
                fittable_non_singleton: false,
                use_base_price: false,
                published: false,
            },
        );
        sde.groups.insert(
            180,
            eve_sde::GroupInfo {
                group_id: 180,
                category_id: 11,
                name: "Protective Sentry Gun".into(),
                anchorable: false,
                anchored: false,
                fittable_non_singleton: false,
                use_base_price: false,
                published: false,
            },
        );

        let mut amarr_sentry = make_fighter_type(1_194, 99, 11, 1_000_000.0, vec![]);
        amarr_sentry.name = "Amarr Sentry Gun".into();
        amarr_sentry.faction_id = None;
        sde.types.insert(1_194, amarr_sentry);

        let mut caldari_sentry = make_fighter_type(3_740, 99, 11, 1_000_000.0, vec![]);
        caldari_sentry.name = "Caldari Sentry Gun I".into();
        caldari_sentry.faction_id = Some(500_001);
        sde.types.insert(3_740, caldari_sentry);

        let mut generic_sentry = make_fighter_type(990_103, 180, 11, 1_000_000.0, vec![]);
        generic_sentry.name = "Protective Sentry Gun".into();
        generic_sentry.faction_id = None;
        sde.types.insert(990_103, generic_sentry);
    }

    // 1. Spawn 1_194 (faction_id: None) and generic group 180 sentry against a non-criminal pilot
    // with -10.0 regional pirate standing (500_010 Guristas): both must resolve empire factions,
    // remain static (max_vel == 0.0, orb_range == 0.0), and never target the pilot.
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.security_status = 0.0;
        ch.standings.insert(500_010, -10.0);
    });
    let amarr_sentry_id = spawn_specialized_npc_entity(
        &ctx,
        sys_id,
        1_194,
        "BeltRat",
        1,
        DVec3::new(15_000.0, 0.0, 0.0),
    )
    .await;
    let generic_sentry_id = spawn_specialized_npc_entity(
        &ctx,
        sys_id,
        990_103,
        "",
        1,
        DVec3::new(20_000.0, 0.0, 0.0),
    )
    .await;
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: 32_880,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 250.0,
                inertia: 1.0,
                mass: 1_200_000.0,
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
        let amarr_ent = &rt.npc_entities[&amarr_sentry_id];
        assert_eq!(amarr_ent.archetype, "EmpireSentry");
        assert_eq!(amarr_ent.faction_id, 500_003);
        assert_eq!(amarr_ent.orbit_range, 0.0);
        assert_eq!(amarr_ent.target_id, None);

        let generic_ent = &rt.npc_entities[&generic_sentry_id];
        assert_eq!(generic_ent.archetype, "EmpireSentry");
        assert_eq!(generic_ent.faction_id, 500_001);
        assert_eq!(generic_ent.orbit_range, 0.0);
        assert_eq!(generic_ent.target_id, None);
    });
    {
        let bp = sys_handle.ballpark.read().unwrap();
        assert_eq!(bp.balls[&amarr_sentry_id].max_velocity, 0.0);
        assert_eq!(bp.balls[&generic_sentry_id].max_velocity, 0.0);
    }

    // 2. Low-sec on-grid illegal aggression engages sentry, and Stage-5 corruption disables it.
    let low_sys_id = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| 0.05 < s.security_status && s.security_status < 0.45)
        .map(|s| s.solar_system_id)
        .expect("canonical seed low-sec system");
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().solar_system_id = low_sys_id;
    });
    let low_sentry_id = spawn_specialized_npc_entity(
        &ctx,
        low_sys_id,
        3_740,
        "",
        1,
        DVec3::new(10_000.0, 0.0, 0.0),
    )
    .await;
    let low_sys_handle = ctx.systems.get_or_spawn(low_sys_id);
    low_sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: 32_880,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 250.0,
                inertia: 1.0,
                mass: 1_200_000.0,
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

    // Peaceful pilot in lowsec is not targeted.
    step_extended_npc_ai(&ctx, low_sys_id).await;
    with_combat_ephemeral(&ctx, |rt| {
        assert_eq!(rt.npc_entities[&low_sentry_id].target_id, None);
    });

    // On-grid illegal aggression triggers sentry targeting and damage.
    let now = ctx.now_unix();
    crate::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| {
        rt.sentry_gun_aggression.insert(
            (low_sys_id, ship_id),
            crate::services::combat_crimewatch::SentryAggressionState {
                solar_system_id: low_sys_id,
                offender_char_id: char_id,
                offender_ship_id: ship_id,
                sentry_origin: DVec3::new(10_000.0, 0.0, 0.0),
                active_on_grid: true,
                total_sentry_damage_dealt: 0.0,
            },
        );
    });
    {
        let mut cw = ctx.crimewatch.write().unwrap();
        cw.aggression_mgr
            .record_aggression(char_id, 90_000_002, low_sys_id, false, false, now);
    }
    step_extended_npc_ai(&ctx, low_sys_id).await;
    with_combat_ephemeral(&ctx, |rt| {
        assert_eq!(rt.npc_entities[&low_sentry_id].target_id, Some(ship_id));
    });
    let shield_before_corruption = low_sys_handle
        .get_combat_state(ship_id)
        .await
        .unwrap()
        .unwrap()
        .shield_hp;
    assert!(shield_before_corruption < 5_000.0);

    // Stage-5 Insurgency corruption disables sentry targeting and clears active threat.
    ctx.db.with_mut_snapshot(|snap| {
        let mut st = eve_db::SystemDynamicState::default();
        st.solar_system_id = low_sys_id;
        st.insurgency_corruption_stage = 5;
        snap.system_states.insert(low_sys_id, st);
        snap.insurgency_runtime.campaigns.insert(
            1,
            eve_db::PersistedInsurgencyCampaign {
                campaign_id: 1,
                pirate_faction_id: 500_010,
                target_faction_id: 500_001,
                fob_solar_system_id: low_sys_id,
                systems: vec![low_sys_id],
                created_at_unix: now.saturating_sub(200),
                forecasting_end_unix: now.saturating_sub(100),
                active_end_unix: now + 1_800,
                winddown_end_unix: now + 3_600,
                corruption_points: [(low_sys_id, 1_000)].into_iter().collect(),
                suppression_points: Default::default(),
            },
        );
    });
    assert!(crate::services::combat_npc_police::is_sentry_disabled_by_corruption(
        &ctx, low_sys_id
    ));
    record_npc_threat_event(&ctx, low_sentry_id, ship_id, 100.0, false, false);
    step_extended_npc_ai(&ctx, low_sys_id).await;
    with_combat_ephemeral(&ctx, |rt| {
        assert_eq!(rt.npc_entities[&low_sentry_id].target_id, None);
        assert!(rt.npc_entities[&low_sentry_id].aggression_threat.is_empty());
    });
    let shield_after_corruption = low_sys_handle
        .get_combat_state(ship_id)
        .await
        .unwrap()
        .unwrap()
        .shield_hp;
    assert_eq!(shield_after_corruption, shield_before_corruption);
}

#[tokio::test]
async fn empire_sentry_offgrid_belt_cross_gate_reship_and_grid_exit_scoping() {
    let (mut ctx, _session, char_id, ship_id, _hi_sys) = make_fighter_ctx_and_session();
    let reship_id = ship_id + 1;
    let low_sys_id = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| 0.05 < s.security_status && s.security_status < 0.45)
        .map(|s| s.solar_system_id)
        .expect("canonical seed low-sec system");
    {
        let sde = std::sync::Arc::make_mut(&mut ctx.sde);
        sde.groups.insert(
            99,
            eve_sde::GroupInfo {
                group_id: 99,
                category_id: 11,
                name: "Sentry Gun".into(),
                anchorable: false,
                anchored: false,
                fittable_non_singleton: false,
                use_base_price: false,
                published: false,
            },
        );
        let mut caldari_sentry = make_fighter_type(3_740, 99, 11, 1_000_000.0, vec![]);
        caldari_sentry.name = "Caldari Sentry Gun I".into();
        caldari_sentry.faction_id = Some(500_001);
        sde.types.insert(3_740, caldari_sentry);
    }

    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.solar_system_id = low_sys_id;
        ch.security_status = 0.0;
        if let Some(sh) = snap.items.get(&ship_id).cloned() {
            let mut sh2 = sh;
            sh2.item_id = reship_id;
            snap.items.insert(reship_id, sh2);
        }
    });

    let gate_a_origin = DVec3::ZERO;
    let gate_b_origin = DVec3::new(2_000_000.0, 0.0, 0.0);
    let gate_a_sentry_id = spawn_specialized_npc_entity(
        &ctx,
        low_sys_id,
        3_740,
        "",
        1,
        gate_a_origin + DVec3::new(10_000.0, 0.0, 0.0),
    )
    .await;
    let gate_b_sentry_id = spawn_specialized_npc_entity(
        &ctx,
        low_sys_id,
        3_740,
        "",
        1,
        gate_b_origin + DVec3::new(10_000.0, 0.0, 0.0),
    )
    .await;

    let make_frigate = |id: i64, pos: DVec3| Ball {
        id,
        type_id: 32_880,
        position: pos,
        velocity: DVec3::ZERO,
        max_velocity: 250.0,
        inertia: 1.0,
        mass: 1_200_000.0,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let make_cs = || CombatState {
        shield_hp: 5_000.0,
        max_shield_hp: 5_000.0,
        armor_hp: 5_000.0,
        max_armor_hp: 5_000.0,
        structure_hp: 5_000.0,
        max_structure_hp: 5_000.0,
        signature_radius: 40.0,
        ..CombatState::default()
    };

    let sys_handle = ctx.systems.get_or_spawn(low_sys_id);
    sys_handle
        .spawn_ball_with_combat(make_frigate(ship_id, gate_a_origin), make_cs())
        .await
        .unwrap();

    // (a) Off-grid belt illegal PvP (> 500 km from gates/stations, so NO `rt.sentry_gun_aggression`
    // entry exists, only an unexpired `!is_legal` entry in `cw.aggression_mgr.records`) ->
    // warping to Gate A must NOT trigger Gate A `EmpireSentry`.
    let now = ctx.now_unix();
    {
        let mut cw = ctx.crimewatch.write().unwrap();
        cw.aggression_mgr
            .record_aggression(char_id, 90_000_002, low_sys_id, false, false, now);
    }
    step_extended_npc_ai(&ctx, low_sys_id).await;
    with_combat_ephemeral(&ctx, |rt| {
        assert_eq!(rt.npc_entities[&gate_a_sentry_id].target_id, None);
        assert_eq!(rt.npc_entities[&gate_b_sentry_id].target_id, None);
    });
    assert_eq!(
        sys_handle
            .get_combat_state(ship_id)
            .await
            .unwrap()
            .unwrap()
            .shield_hp,
        5_000.0
    );

    // (b1) Gate A illegal aggression (`sentry_origin = Gate A`) triggers ONLY Gate A sentry, NOT Gate B.
    crate::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| {
        rt.sentry_gun_aggression.insert(
            (low_sys_id, ship_id),
            crate::services::combat_crimewatch::SentryAggressionState {
                solar_system_id: low_sys_id,
                offender_char_id: char_id,
                offender_ship_id: ship_id,
                sentry_origin: gate_a_origin,
                active_on_grid: true,
                total_sentry_damage_dealt: 0.0,
            },
        );
    });
    step_extended_npc_ai(&ctx, low_sys_id).await;
    with_combat_ephemeral(&ctx, |rt| {
        assert_eq!(rt.npc_entities[&gate_a_sentry_id].target_id, Some(ship_id));
        assert_eq!(rt.npc_entities[&gate_b_sentry_id].target_id, None);
    });
    let shield_after_gate_a_hit = sys_handle
        .get_combat_state(ship_id)
        .await
        .unwrap()
        .unwrap()
        .shield_hp;
    assert!(shield_after_gate_a_hit < 5_000.0);

    // (b2) Reshipping into `reship_id` on Gate A grid must NOT transfer `ship_id`'s sentry aggro.
    sys_handle
        .spawn_ball_with_combat(
            make_frigate(reship_id, gate_a_origin + DVec3::new(2_000.0, 0.0, 0.0)),
            make_cs(),
        )
        .await
        .unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().active_ship_id = reship_id;
    });
    step_extended_npc_ai(&ctx, low_sys_id).await;
    with_combat_ephemeral(&ctx, |rt| {
        assert_ne!(rt.npc_entities[&gate_a_sentry_id].target_id, Some(reship_id));
    });
    assert_eq!(
        sys_handle
            .get_combat_state(reship_id)
            .await
            .unwrap()
            .unwrap()
            .shield_hp,
        5_000.0
    );
    let _ = sys_handle.remove_ball(reship_id).await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().active_ship_id = ship_id;
    });

    // (b3) Moving directly from Gate A to Gate B (> 500 km from Gate A) must NOT trigger Gate B
    // and must clear `active_on_grid` + Gate A target lock.
    let _ = sys_handle.set_ball_position(ship_id, gate_b_origin).await;
    step_extended_npc_ai(&ctx, low_sys_id).await;
    with_combat_ephemeral(&ctx, |rt| {
        assert_eq!(rt.npc_entities[&gate_b_sentry_id].target_id, None);
        assert_eq!(rt.npc_entities[&gate_a_sentry_id].target_id, None);
        assert!(rt.npc_entities[&gate_a_sentry_id].aggression_threat.is_empty());
    });
    assert!(!crate::services::combat_crimewatch::with_combat_runtime(&ctx, |rt| {
        rt.sentry_gun_aggression[&(low_sys_id, ship_id)].active_on_grid
    }));

    // Returning to Gate A after moving > 500 km off-grid keeps Gate A sentry passive.
    let _ = sys_handle.set_ball_position(ship_id, gate_a_origin).await;
    step_extended_npc_ai(&ctx, low_sys_id).await;
    with_combat_ephemeral(&ctx, |rt| {
        assert_eq!(rt.npc_entities[&gate_a_sentry_id].target_id, None);
    });

    // (b4) Entering `MovementMode::Warp` while still < 500 km clears `active_on_grid` so arriving back keeps Gate A passive.
    crate::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| {
        rt.sentry_gun_aggression
            .get_mut(&(low_sys_id, ship_id))
            .unwrap()
            .active_on_grid = true;
    });
    let _ = sys_handle
        .set_movement_mode(
            ship_id,
            MovementMode::Warp {
                origin: gate_a_origin,
                destination: gate_b_origin,
                total_distance: 2_000_000.0,
                warp_speed_au_s: 3.0,
                phase: eve_destiny::WarpPhase::Cruising,
                elapsed_warp_time: 1.0,
            },
        )
        .await;
    step_extended_npc_ai(&ctx, low_sys_id).await;
    assert!(!crate::services::combat_crimewatch::with_combat_runtime(&ctx, |rt| {
        rt.sentry_gun_aggression[&(low_sys_id, ship_id)].active_on_grid
    }));
    let _ = sys_handle.set_movement_mode(ship_id, MovementMode::Stop).await;
    step_extended_npc_ai(&ctx, low_sys_id).await;
    with_combat_ephemeral(&ctx, |rt| {
        assert_eq!(rt.npc_entities[&gate_a_sentry_id].target_id, None);
    });

    // (b5) Docking / removal from ballpark clears `active_on_grid` as well.
    crate::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| {
        rt.sentry_gun_aggression
            .get_mut(&(low_sys_id, ship_id))
            .unwrap()
            .active_on_grid = true;
    });
    let _ = sys_handle.remove_ball(ship_id).await;
    step_extended_npc_ai(&ctx, low_sys_id).await;
    assert!(!crate::services::combat_crimewatch::with_combat_runtime(&ctx, |rt| {
        rt.sentry_gun_aggression[&(low_sys_id, ship_id)].active_on_grid
    }));
}

#[tokio::test]
async fn khanid_500008_sentry_classification_and_non_empire_group_99_exclusions() {
    let (mut ctx, _session, char_id, ship_id, _hi_sys) = make_fighter_ctx_and_session();
    let low_sys_id = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| 0.05 < s.security_status && s.security_status < 0.45)
        .map(|s| s.solar_system_id)
        .expect("canonical seed low-sec system");
    {
        let sde = std::sync::Arc::make_mut(&mut ctx.sde);
        for (gid, name) in [(99_u32, "Sentry Gun"), (180_u32, "Protective Sentry Gun")] {
            sde.groups.insert(
                gid,
                eve_sde::GroupInfo {
                    group_id: gid,
                    category_id: 11,
                    name: name.into(),
                    anchorable: false,
                    anchored: false,
                    fittable_non_singleton: false,
                    use_base_price: false,
                    published: false,
                },
            );
        }

        let mut khanid_sentry_99 = make_fighter_type(990_201, 99, 11, 1_000_000.0, vec![]);
        khanid_sentry_99.name = "Khanid Sentry Gun".into();
        khanid_sentry_99.faction_id = Some(500_008);
        sde.types.insert(990_201, khanid_sentry_99);

        let mut khanid_sentry_180 = make_fighter_type(990_202, 180, 11, 1_000_000.0, vec![]);
        khanid_sentry_180.name = "Khanid Protective Sentry".into();
        khanid_sentry_180.faction_id = Some(500_008);
        sde.types.insert(990_202, khanid_sentry_180);

        let mut drifter_sentry = make_fighter_type(990_203, 99, 11, 1_000_000.0, vec![]);
        drifter_sentry.name = "Drifter Defense Sentry".into();
        drifter_sentry.faction_id = Some(500_024);
        sde.types.insert(990_203, drifter_sentry);

        let mut edencom_sentry = make_fighter_type(990_204, 180, 11, 1_000_000.0, vec![]);
        edencom_sentry.name = "EDENCOM Gun Tower".into();
        edencom_sentry.faction_id = Some(500_027);
        sde.types.insert(990_204, edencom_sentry);

        let mut sleeper_sentry_g99 = make_fighter_type(990_205, 99, 11, 1_000_000.0, vec![]);
        sleeper_sentry_g99.name = "Sleeper Sentry Tower".into();
        sleeper_sentry_g99.faction_id = Some(500_021);
        sde.types.insert(990_205, sleeper_sentry_g99);

        let mut drone_sentry_g99 = make_fighter_type(990_206, 99, 11, 1_000_000.0, vec![]);
        drone_sentry_g99.name = "Rogue Drone Alvi Sentry".into();
        drone_sentry_g99.faction_id = Some(500_025);
        sde.types.insert(990_206, drone_sentry_g99);
    }

    // P1-02: Khanid Kingdom (500_008) group 99/180 sentries classify as "EmpireSentry", NOT "RogueDrone",
    // get static sentry seeding stats, and are not treated as hostile to EDENCOM.
    assert_eq!(derive_npc_archetype(&ctx, 990_201), "EmpireSentry");
    assert_eq!(derive_npc_archetype(&ctx, 990_202), "EmpireSentry");

    let khanid_id = spawn_specialized_npc_entity(
        &ctx,
        low_sys_id,
        990_201,
        "",
        1,
        DVec3::new(10_000.0, 0.0, 0.0),
    )
    .await;
    with_combat_ephemeral(&ctx, |rt| {
        let ent = &rt.npc_entities[&khanid_id];
        assert_eq!(ent.archetype, "EmpireSentry");
        assert_eq!(ent.faction_id, 500_008);
        assert_eq!(ent.orbit_range, 0.0);
        assert!(ent.optimal_range >= 500_000.0);
        assert_eq!(ent.bounty_isk, 0.0);
        assert!(is_empire_sentry_npc(&ctx, ent));
    });
    assert_eq!(
        ctx.systems.get_or_spawn(low_sys_id).ballpark.read().unwrap().balls[&khanid_id].max_velocity,
        0.0
    );

    let (_, _, rogue_fallback) = crate::services::wormhole_pve_seeding::build_specialized_npc_components(
        &ctx,
        low_sys_id,
        990_299,
        999_999_999,
        "RogueDrone",
        0,
        DVec3::ZERO,
    );
    assert_eq!(rogue_fallback.faction_id, 500_025);

    let edencom_observer = make_test_npc_entity(990_300, 990_204, low_sys_id, "EDENCOM");
    assert!(!is_candidate_hostile_to_edencom(
        &ctx,
        &edencom_observer,
        khanid_id,
        ship_id
    ));

    // P1-03: Drifter / EDENCOM / Sleeper / RogueDrone entities (even in group 99/180 or with a
    // group 99 type_id) must NOT match `is_empire_sentry_npc`.
    assert_eq!(derive_npc_archetype(&ctx, 990_203), "Drifter");
    assert_eq!(derive_npc_archetype(&ctx, 990_204), "EDENCOM");
    assert_eq!(derive_npc_archetype(&ctx, 990_205), "Sleeper");
    assert_eq!(derive_npc_archetype(&ctx, 990_206), "RogueDrone");

    for (tid, arch) in [
        (990_203_u32, "Drifter"),
        (990_204, "EDENCOM"),
        (990_205, "Sleeper"),
        (990_206, "RogueDrone"),
        (990_201, "Drifter"),
        (990_201, "EDENCOM"),
        (990_201, "Edencom"),
        (990_201, "Sleeper"),
        (990_201, "RogueDrone"),
        (990_201, "SleeperSentry"),
        (990_201, "ZarzakhGateGun"),
        (990_201, "CounterInsurgencySentry"),
    ] {
        let ent = make_test_npc_entity(990_301, tid, low_sys_id, arch);
        assert!(
            !is_empire_sentry_npc(&ctx, &ent),
            "non-Empire entity (type_id={tid}, archetype={arch}) must not match is_empire_sentry_npc"
        );
    }

    // Spawn group 180 EDENCOM entity in a Stage-5 corrupted system; it must NOT be disabled as an
    // Empire Sentry and must still target a -10.0 EDENCOM-standing pilot.
    let now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.solar_system_id = low_sys_id;
        ch.standings.insert(500_027, -10.0);
        let mut st = eve_db::SystemDynamicState::default();
        st.solar_system_id = low_sys_id;
        st.insurgency_corruption_stage = 5;
        snap.system_states.insert(low_sys_id, st);
        snap.insurgency_runtime.campaigns.insert(
            1,
            eve_db::PersistedInsurgencyCampaign {
                campaign_id: 1,
                pirate_faction_id: 500_010,
                target_faction_id: 500_001,
                fob_solar_system_id: low_sys_id,
                systems: vec![low_sys_id],
                created_at_unix: now.saturating_sub(200),
                forecasting_end_unix: now.saturating_sub(100),
                active_end_unix: now + 1_800,
                winddown_end_unix: now + 3_600,
                corruption_points: [(low_sys_id, 1_000)].into_iter().collect(),
                suppression_points: Default::default(),
            },
        );
    });
    let edencom_g180_id = spawn_specialized_npc_entity(
        &ctx,
        low_sys_id,
        990_204,
        "",
        1,
        DVec3::new(15_000.0, 0.0, 0.0),
    )
    .await;
    let sys_handle = ctx.systems.get_or_spawn(low_sys_id);
    sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: 32_880,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 250.0,
                inertia: 1.0,
                mass: 1_200_000.0,
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
    step_extended_npc_ai(&ctx, low_sys_id).await;
    with_combat_ephemeral(&ctx, |rt| {
        let ed = &rt.npc_entities[&edencom_g180_id];
        assert_eq!(ed.archetype, "EDENCOM");
        assert!(!is_empire_sentry_npc(&ctx, ed));
        assert_eq!(ed.target_id, Some(ship_id));
    });
}

fn make_isolated_extended_npc_ctx(
    sde: std::sync::Arc<eve_sde::SdeRegistry>,
) -> (ServerContext, i64, i64, u32) {
    let db = std::sync::Arc::new(eve_db::WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(sde, db)
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED);
    let char_id = 90_000_001_i64;
    let ship_id = 200_000_001_i64;
    let sys_id = 30_000_142_u32;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ship_id,
            crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::make_fighter_item(
                ship_id,
                587,
                char_id,
                sys_id as i64,
                eve_db::InventoryFlag::None,
                1,
            ),
        );
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.active_ship_id = ship_id;
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
        }
    });
    (ctx, char_id, ship_id, sys_id)
}

async fn spawn_extended_test_ship(
    ctx: &ServerContext,
    sys_id: u32,
    ship_id: i64,
    pos: DVec3,
    vel: DVec3,
    sig_radius: f64,
    shield_hp: f64,
) {
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: 587,
                position: pos,
                velocity: vel,
                max_velocity: 1_500.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: if vel.length_squared() > 0.0 { 1.0 } else { 0.0 },
                mode: MovementMode::Stop,
            },
            CombatState {
                shield_hp,
                max_shield_hp: shield_hp,
                armor_hp: 1_000_000.0,
                max_armor_hp: 1_000_000.0,
                structure_hp: 1_000_000.0,
                max_structure_hp: 1_000_000.0,
                shield_resonance: [1.0; 4],
                armor_resonance: [1.0; 4],
                structure_resonance: [1.0; 4],
                capacitor: 1_000.0,
                max_capacitor: 1_000.0,
                signature_radius: sig_radius,
                base_signature_radius: Some(sig_radius),
                ..CombatState::default()
            },
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn extended_npc_ai_sde_dps_within_one_percent_five_types_and_zero_neut() {
    let (base_ctx, _, _, _, _) = make_fighter_ctx_and_session();
    let cases = [
        (2_382_u32, 500_010_u32, 4_000_usize, "Guristas Arrogator (Frigate Hybrid)"),
        (13_032, 500_011, 4_000, "Arch Angel Rogue (Frigate Projectile)"),
        (2_381, 500_020, 4_000, "Serpentis Chief Spy (Cruiser Hybrid)"),
        (10_281, 500_012, 4_000, "Blood Arch Reaver (Cruiser Laser)"),
        (17_641, 500_011, 6_000, "Arch Angel Raelek (Battleship Projectile+Missile)"),
    ];
    let initial_shield = 1e12_f64;

    for &(npc_tid, faction_id, n_ticks, label) in &cases {
        let (ctx, _char_id, ship_id, sys_id) =
            make_isolated_extended_npc_ctx(std::sync::Arc::clone(&base_ctx.sde));
        spawn_extended_test_ship(
            &ctx,
            sys_id,
            ship_id,
            DVec3::new(300.0, 0.0, 0.0),
            DVec3::ZERO,
            2_000.0,
            initial_shield,
        )
        .await;

        let _npc_id = spawn_specialized_npc_entity(
            &ctx,
            sys_id,
            npc_tid,
            "Pirate",
            faction_id,
            DVec3::ZERO,
        )
        .await;

        let sys_handle = ctx.systems.get_or_spawn(sys_id);
        for tick in 0..n_ticks {
            step_extended_npc_ai_sync(&ctx, sys_id);
            if (tick + 1) % 250 == 0 {
                let mut bp = sys_handle.ballpark.write().unwrap_or_else(|e| e.into_inner());
                bp.pending_events.clear();
                bp.observer_events.clear();
            }
        }

        let ship_cs = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
        let empirical_dps = (initial_shield - ship_cs.shield_hp) / (n_ticks as f64);

        let (profile, fallbacks) = crate::combat::npc_ai_weapons_loot::inspect_npc_sde_profile(
            &ctx.sde,
            npc_tid,
            None,
            faction_id,
        );
        assert!(
            fallbacks.is_empty(),
            "{label} ({npc_tid}) must resolve from SDE without fallbacks, got {fallbacks:?}"
        );
        let turret_exp = profile
            .turret
            .as_ref()
            .map(|t| (t.shot_damage.total() / t.cycle_duration_s) * 1.01505)
            .unwrap_or(0.0);
        let missile_exp = profile
            .missile
            .as_ref()
            .map(|m| m.volley_damage.total() / m.cycle_duration_s)
            .unwrap_or(0.0);
        let expected_dps = turret_exp + missile_exp;
        let rel_err = (empirical_dps - expected_dps).abs() / expected_dps.max(1e-9);
        assert!(
            rel_err <= 0.01,
            "{label}: empirical DPS {empirical_dps:.4} vs expected {expected_dps:.4} (rel_err={:.3}%)",
            rel_err * 100.0
        );

        if matches!(npc_tid, 2_382 | 13_032) {
            assert_eq!(
                ship_cs.capacitor, 1_000.0,
                "{label} ({npc_tid}) must drain 0.0 GJ capacitor from target"
            );
        }
    }
}

#[tokio::test]
async fn extended_npc_ai_turret_and_missile_hit_rules_reduce_damage_on_fast_frigate() {
    let (base_ctx, _, _, _, _) = make_fighter_ctx_and_session();

    // 1. Turret angular-velocity & signature rule: Serpentis Chief Spy (2381) vs stationary vs orbiting frigate
    let run_turret_case = |sig: f64, vel: DVec3| {
        let sde = std::sync::Arc::clone(&base_ctx.sde);
        async move {
            let (ctx, _char_id, ship_id, sys_id) = make_isolated_extended_npc_ctx(sde);
            let initial_shield = 1_000_000.0;
            spawn_extended_test_ship(
                &ctx,
                sys_id,
                ship_id,
                DVec3::new(2_000.0, 0.0, 0.0),
                vel,
                sig,
                initial_shield,
            )
            .await;
            let _npc = spawn_specialized_npc_entity(&ctx, sys_id, 2_381, "Pirate", 500_020, DVec3::ZERO).await;
            for _ in 0..250 {
                step_extended_npc_ai_sync(&ctx, sys_id);
            }
            let cs = ctx
                .systems
                .get_or_spawn(sys_id)
                .get_combat_state(ship_id)
                .await
                .unwrap()
                .unwrap();
            initial_shield - cs.shield_hp
        }
    };
    let turret_stationary = run_turret_case(400.0, DVec3::ZERO).await;
    let turret_fast_frigate = run_turret_case(35.0, DVec3::new(0.0, 150.0, 0.0)).await;
    assert!(turret_stationary > 5_000.0, "expected substantial stationary turret dmg, got {turret_stationary}");
    assert!(
        turret_fast_frigate > 0.0 && turret_fast_frigate < turret_stationary * 0.25,
        "fast small frigate ({turret_fast_frigate}) must take <25% of stationary turret dmg ({turret_stationary})"
    );

    // 2. Missile explosion radius/velocity rule: Guristas Plunderer (2386, Scourge Rocket 266, 66 dmg)
    let run_missile_case = |sig: f64, vel: DVec3| {
        let sde = std::sync::Arc::clone(&base_ctx.sde);
        async move {
            let (ctx, _char_id, ship_id, sys_id) = make_isolated_extended_npc_ctx(sde);
            let initial_shield = 10_000.0;
            spawn_extended_test_ship(
                &ctx,
                sys_id,
                ship_id,
                DVec3::new(1_000.0, 0.0, 0.0),
                vel,
                sig,
                initial_shield,
            )
            .await;
            let _npc = spawn_specialized_npc_entity(&ctx, sys_id, 2_386, "Pirate", 500_010, DVec3::ZERO).await;
            step_extended_npc_ai_sync(&ctx, sys_id);
            let cs = ctx
                .systems
                .get_or_spawn(sys_id)
                .get_combat_state(ship_id)
                .await
                .unwrap()
                .unwrap();
            initial_shield - cs.shield_hp
        }
    };
    let missile_stationary = run_missile_case(40.0, DVec3::ZERO).await;
    let missile_fast_frigate = run_missile_case(35.0, DVec3::new(0.0, 500.0, 0.0)).await;
    assert!((missile_stationary - 66.0).abs() < 1e-6);
    assert!(
        missile_fast_frigate > 35.0 && missile_fast_frigate < 50.0,
        "fast 500m/s frigate ({missile_fast_frigate}) must take reduced missile damage vs stationary ({missile_stationary})"
    );
}

#[tokio::test]
async fn extended_npc_ai_emits_weapon_matched_special_fx_and_wire_meta() {
    let (base_ctx, _, _, _, _) = make_fighter_ctx_and_session();
    let cases = [
        (13_032_u32, "Pirate", 500_011_u32, "effects.ProjectileFired", Some(484_u32), None),
        (2_382, "Pirate", 500_010, "effects.HybridFired", Some(562), None),
        (10_281, "Pirate", 500_012, "effects.Laser", Some(455), None),
        (2_386, "Pirate", 500_010, "effects.MissileDeployment", None, Some(266_u32)),
        (52_206, "Triglavian", 500_026, "effects.TriglavianBeam", Some(47_914), None),
    ];

    for &(npc_tid, arch, faction_id, expected_guid, expected_mod_tid, expected_other_tid) in &cases {
        let (ctx, _char_id, ship_id, sys_id) =
            make_isolated_extended_npc_ctx(std::sync::Arc::clone(&base_ctx.sde));
        spawn_extended_test_ship(
            &ctx,
            sys_id,
            ship_id,
            DVec3::new(300.0, 0.0, 0.0),
            DVec3::ZERO,
            1_000.0,
            100_000.0,
        )
        .await;
        let npc_id =
            spawn_specialized_npc_entity(&ctx, sys_id, npc_tid, arch, faction_id, DVec3::ZERO).await;

        step_extended_npc_ai_sync(&ctx, sys_id);

        let sys_handle = ctx.systems.get_or_spawn(sys_id);
        let bp = sys_handle.ballpark.read().unwrap_or_else(|e| e.into_inner());
        let has_fx = bp.pending_events.iter().any(|ev| {
            matches!(
                ev,
                eve_destiny::DestinyEvent::OnSpecialFX { ball_id, module_id: None, guid, is_active: true }
                if *ball_id == npc_id && guid == expected_guid
            )
        });
        assert!(
            has_fx,
            "NPC {npc_tid} must emit OnSpecialFX {expected_guid}, events={:?}",
            bp.pending_events
        );
        let meta = bp
            .special_fx_meta
            .get(&(npc_id, None, expected_guid.to_string()))
            .expect("SpecialFxWireMeta recorded for NPC attack");
        assert_eq!(meta.target_id, Some(ship_id));
        assert!(meta.is_offensive);
        assert_eq!(meta.module_type_id, expected_mod_tid);
        assert_eq!(meta.other_type_id, expected_other_tid);
    }
}

#[tokio::test]
async fn extended_npc_ai_destroys_with_sde_wreck_scaled_loot_and_clears_runtime() {
    let (base_ctx, _, _, _, _) = make_fighter_ctx_and_session();

    for &(npc_tid, arch, faction_id, expected_wreck, expected_ammo_tid, expected_ammo_qty, expect_zero_bounty) in &[
        (2_382_u32, "Pirate", 500_010_u32, 26_573_u32, 222_u32, 25_i32, false),
        (2_381, "Pirate", 500_020, 26_586, 230, 60, false),
        (17_641, "Pirate", 500_011, 26_563, 199, 120, false),
        (30_201, "Sleeper", 500_021, 30_492, 254, 1, true),
    ] {
        let (ctx, char_id, ship_id, sys_id) =
            make_isolated_extended_npc_ctx(std::sync::Arc::clone(&base_ctx.sde));
        spawn_extended_test_ship(
            &ctx,
            sys_id,
            ship_id,
            DVec3::new(300.0, 0.0, 0.0),
            DVec3::ZERO,
            1_000.0,
            100_000.0,
        )
        .await;
        let npc_id =
            spawn_specialized_npc_entity(&ctx, sys_id, npc_tid, arch, faction_id, DVec3::ZERO).await;
        step_extended_npc_ai_sync(&ctx, sys_id);

        let (wreck_id, bounty, dropped_types) =
            destroy_specialized_npc_with_hint(&ctx, sys_id, npc_id, char_id, None).await;
        assert!(wreck_id > 0);
        if expect_zero_bounty {
            assert_eq!(bounty, 0.0);
        } else {
            assert!(bounty > 0.0);
        }
        assert!(dropped_types.contains(&expected_ammo_tid));

        ctx.db.with_snapshot(|snap| {
            let wreck = snap.items.get(&wreck_id).expect("SDE wreck spawned");
            assert_eq!(wreck.type_id, expected_wreck);
            assert_eq!(wreck.location_id, sys_id as i64);
            assert!(snap.items.values().any(|i| {
                i.location_id == wreck_id
                    && i.type_id == expected_ammo_tid
                    && i.quantity == expected_ammo_qty
                    && i.owner_id == char_id
            }));
        });
    }

    // Verify per-entity weapon runtime cooldown is cleared when an NPC is destroyed.
    let (ctx, char_id, ship_id, sys_id) =
        make_isolated_extended_npc_ctx(std::sync::Arc::clone(&base_ctx.sde));
    spawn_extended_test_ship(
        &ctx,
        sys_id,
        ship_id,
        DVec3::new(1_000.0, 0.0, 0.0),
        DVec3::ZERO,
        40.0,
        10_000.0,
    )
    .await;
    let reused_id = spawn_specialized_npc_entity(&ctx, sys_id, 2_386, "Pirate", 500_010, DVec3::ZERO).await;
    step_extended_npc_ai_sync(&ctx, sys_id);
    let _ = destroy_specialized_npc_with_hint(&ctx, sys_id, reused_id, char_id, None).await;

    // Reinsert a new NPC with the same ball ID; its missile must fire immediately at t=0 (not inherit cooldown).
    let (ball, cs, ent) = crate::services::wormhole_pve_seeding::build_specialized_npc_components(
        &ctx,
        sys_id,
        reused_id,
        2_386,
        "Pirate",
        500_010,
        DVec3::ZERO,
    );
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    sys_handle.spawn_ball_with_combat(ball, cs).await.unwrap();
    with_mut_combat_ephemeral(&ctx, |rt| {
        rt.npc_entities.insert(reused_id, ent);
    });
    let shield_before = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap().shield_hp;
    step_extended_npc_ai_sync(&ctx, sys_id);
    let shield_after = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap().shield_hp;
    assert!(
        shield_before - shield_after >= 65.0,
        "respawned NPC reusing ball_id must have fresh weapon cooldown (lost {} HP)",
        shield_before - shield_after
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn step_extended_npc_ai_sync_never_holds_db_lock_under_ballpark_write_and_is_deadlock_free() {
    // 1. Structural source-audit lock-ordering invariant.
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let weapons_loot_src = std::fs::read_to_string(manifest_dir.join("src/combat/npc_ai_weapons_loot.rs"))
        .expect("read npc_ai_weapons_loot.rs");
    assert!(
        !weapons_loot_src.contains(".ballpark."),
        "npc_ai_weapons_loot.rs must never acquire sys_handle.ballpark locks directly"
    );

    let combat_npc_ai_src = std::fs::read_to_string(manifest_dir.join("src/services/combat_npc_ai.rs"))
        .expect("read combat_npc_ai.rs");
    let fn_start = combat_npc_ai_src
        .find("pub fn step_extended_npc_ai_sync(")
        .expect("step_extended_npc_ai_sync start");
    let fn_end = combat_npc_ai_src[fn_start..]
        .find("pub async fn step_extended_npc_ai(")
        .map(|off| fn_start + off)
        .expect("step_extended_npc_ai_sync end");
    let sync_body = &combat_npc_ai_src[fn_start..fn_end];

    let mut search_from = 0_usize;
    let mut bp_write_scopes = 0_usize;
    let next_brace_offsets = |slice: &str| -> Vec<(usize, char)> {
        let mut out = Vec::new();
        let bytes = slice.as_bytes();
        let mut i = 0;
        let mut in_str = false;
        let mut in_line_comment = false;
        while i < bytes.len() {
            let b = bytes[i];
            if in_line_comment {
                if b == b'\n' {
                    in_line_comment = false;
                }
                i += 1;
                continue;
            }
            if in_str {
                if b == b'\\' {
                    i += 2;
                    continue;
                }
                if b == b'"' {
                    in_str = false;
                }
                i += 1;
                continue;
            }
            if b == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
                in_line_comment = true;
                i += 2;
                continue;
            }
            if b == b'"' {
                in_str = true;
                i += 1;
                continue;
            }
            if b == b'{' || b == b'}' {
                out.push((i, b as char));
            }
            i += 1;
        }
        out
    };
    let all_braces = next_brace_offsets(sync_body);
    while let Some(rel) = sync_body[search_from..].find("ballpark.write()") {
        let hit = search_from + rel;
        let mut stack: Vec<usize> = Vec::new();
        for &(pos, ch) in &all_braces {
            if pos >= hit {
                break;
            }
            if ch == '{' {
                stack.push(pos);
            } else {
                stack.pop();
            }
        }
        let scope_start = *stack.last().expect("enclosing `{` before ballpark.write()");
        let target_depth = stack.len() - 1;
        let mut depth = stack.len();
        let mut scope_end = sync_body.len();
        for &(pos, ch) in &all_braces {
            if pos <= hit {
                continue;
            }
            if ch == '{' {
                depth += 1;
            } else {
                depth -= 1;
                if depth == target_depth {
                    scope_end = pos;
                    break;
                }
            }
        }
        let locked_slice = &sync_body[scope_start..scope_end];
        for forbidden in [
            "ctx.db.",
            "with_snapshot",
            "with_mut_snapshot",
            "with_combat_ephemeral",
            "with_mut_combat_ephemeral",
            "combat_ephemeral.read",
            "combat_ephemeral.write",
        ] {
            assert!(
                !locked_slice.contains(forbidden),
                "step_extended_npc_ai_sync ballpark.write() scope contains forbidden `{forbidden}`:\n{locked_slice}"
            );
        }
        bp_write_scopes += 1;
        search_from = scope_end;
    }
    assert!(
        bp_write_scopes >= 2,
        "expected ballpark.write() scopes in step_extended_npc_ai_sync"
    );

    // 2. Multi-thread concurrency stress test (Sleeper + EDENCOM + Pirate scram/web under concurrent DB locks).
    let (base_ctx, _, _, _, _) = make_fighter_ctx_and_session();
    let (ctx, char_id, ship_id, sys_id) =
        make_isolated_extended_npc_ctx(std::sync::Arc::clone(&base_ctx.sde));
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.standings.insert(500_027, -1.0);
        }
    });
    spawn_extended_test_ship(
        &ctx,
        sys_id,
        ship_id,
        DVec3::new(1_000.0, 0.0, 0.0),
        DVec3::ZERO,
        1_000.0,
        10_000_000.0,
    )
    .await;
    let _sleeper = spawn_specialized_npc_entity(&ctx, sys_id, 30_201, "Sleeper", 500_021, DVec3::ZERO).await;
    let _edencom = spawn_specialized_npc_entity(&ctx, sys_id, 56_188, "EDENCOM", 500_027, DVec3::ZERO).await;
    let _pirate = spawn_specialized_npc_entity(&ctx, sys_id, 13_032, "Pirate", 500_011, DVec3::ZERO).await;

    let ctx_arc = std::sync::Arc::new(ctx);
    let c_ai = std::sync::Arc::clone(&ctx_arc);
    let c_db1 = std::sync::Arc::clone(&ctx_arc);
    let c_db2 = std::sync::Arc::clone(&ctx_arc);

    let ai_task = tokio::task::spawn_blocking(move || {
        for _ in 0..80 {
            step_extended_npc_ai_sync(&c_ai, sys_id);
        }
    });
    let db_reader = tokio::task::spawn_blocking(move || {
        for _ in 0..400 {
            let _ = c_db1.db.with_snapshot(|s| s.items.len());
        }
    });
    let db_writer = tokio::task::spawn_blocking(move || {
        for _ in 0..200 {
            c_db2.db.with_mut_snapshot(|s| {
                let _ = s.characters.len();
            });
        }
    });

    tokio::time::timeout(std::time::Duration::from_secs(15), async {
        ai_task.await.unwrap();
        db_reader.await.unwrap();
        db_writer.await.unwrap();
    })
    .await
    .expect("concurrent step_extended_npc_ai_sync and DB snapshot locks must never deadlock");

    let ship_cs = ctx_arc
        .systems
        .get_or_spawn(sys_id)
        .get_combat_state(ship_id)
        .await
        .unwrap()
        .unwrap();
    assert!(ship_cs.shield_hp < 10_000_000.0);
}

#[tokio::test]
async fn extended_npc_ai_idle_cooldown_decays_between_engagements() {
    let (base_ctx, _, _, _, _) = make_fighter_ctx_and_session();
    let (ctx, _char_id, ship_id, sys_id) =
        make_isolated_extended_npc_ctx(std::sync::Arc::clone(&base_ctx.sde));
    spawn_extended_test_ship(
        &ctx,
        sys_id,
        ship_id,
        DVec3::new(1_000.0, 0.0, 0.0),
        DVec3::ZERO,
        1_000.0,
        100_000.0,
    )
    .await;

    // Arch Angel Raelek (17641) has multi-second turret & missile cycles from SDE.
    let npc_id =
        spawn_specialized_npc_entity(&ctx, sys_id, 17_641, "Pirate", 500_011, DVec3::ZERO).await;

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    step_extended_npc_ai_sync(&ctx, sys_id);
    let shield_after_t0 = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap().shield_hp;
    assert!(
        shield_after_t0 < 100_000.0,
        "17641 must fire immediately on initial engagement"
    );

    let (t_cd0, m_cd0) = with_combat_ephemeral(&ctx, |rt| {
        let wrt = rt
            .extended_npc_weapon_runtime
            .get(&(sys_id, npc_id))
            .expect("17641 runtime must be initialized after firing");
        (wrt.turret_cooldown_s, wrt.missile_cooldown_s)
    });
    assert!(
        t_cd0 > 1.0 || m_cd0 > 1.0,
        "17641 should retain >1s cooldown immediately after firing (t={t_cd0}, m={m_cd0})"
    );

    // Drop the target so the NPC takes the idle / early-continue path for enough ticks to decay.
    sys_handle.try_remove_ball(ship_id);
    let idle_ticks = (t_cd0.max(m_cd0).ceil() as usize).max(2) + 2;
    for _ in 0..idle_ticks {
        step_extended_npc_ai_sync(&ctx, sys_id);
    }

    with_combat_ephemeral(&ctx, |rt| {
        let wrt = rt
            .extended_npc_weapon_runtime
            .get(&(sys_id, npc_id))
            .expect("live idle NPC must keep its weapon runtime");
        assert_eq!(wrt.turret_cooldown_s, 0.0, "turret cooldown must decay to 0 while idle");
        assert_eq!(wrt.missile_cooldown_s, 0.0, "missile cooldown must decay to 0 while idle");
    });

    // Re-engaging with a fresh target must fire on the very first tick (zero frozen cooldown).
    spawn_extended_test_ship(
        &ctx,
        sys_id,
        ship_id,
        DVec3::new(1_000.0, 0.0, 0.0),
        DVec3::ZERO,
        1_000.0,
        100_000.0,
    )
    .await;
    step_extended_npc_ai_sync(&ctx, sys_id);
    let shield_after_reengage = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap().shield_hp;
    assert!(
        shield_after_reengage < 100_000.0,
        "NPC must fire immediately on first tick of new engagement after idle decay"
    );
}

#[tokio::test]
async fn extended_npc_weapon_runtime_pruned_on_death_pipeline_empty_system_and_persisted_sync() {
    let (base_ctx, _, _, _, _) = make_fighter_ctx_and_session();
    let (ctx, char_id, ship_id, sys_id) =
        make_isolated_extended_npc_ctx(std::sync::Arc::clone(&base_ctx.sde));
    spawn_extended_test_ship(
        &ctx,
        sys_id,
        ship_id,
        DVec3::new(1_000.0, 0.0, 0.0),
        DVec3::ZERO,
        1_000.0,
        100_000.0,
    )
    .await;

    // 1. death_pipeline::on_ball_destroyed_sync clears runtime even for the last NPC in a system.
    let npc_a =
        spawn_specialized_npc_entity(&ctx, sys_id, 2_386, "Pirate", 500_010, DVec3::ZERO).await;
    step_extended_npc_ai_sync(&ctx, sys_id);
    with_combat_ephemeral(&ctx, |rt| {
        assert!(rt.extended_npc_weapon_runtime.contains_key(&(sys_id, npc_a)));
    });
    let _ = crate::combat::death_pipeline::on_ball_destroyed_sync(
        &ctx,
        sys_id,
        npc_a,
        Some(char_id),
        DVec3::ZERO,
    )
    .expect("death pipeline cleanup");
    with_combat_ephemeral(&ctx, |rt| {
        assert!(!rt.npc_entities.contains_key(&npc_a));
        assert!(!rt.extended_npc_weapon_runtime.contains_key(&(sys_id, npc_a)));
    });

    // 2. step_extended_npc_ai_sync prunes stale weapon runtime before empty-system early exits
    // and also prunes orphans on live systems when no entity state changed.
    let (_b, _cs, npc_sample_ent) =
        crate::services::wormhole_pve_seeding::build_specialized_npc_components(
            &ctx,
            sys_id,
            9_990_001,
            2_386,
            "Pirate",
            500_010,
            DVec3::ZERO,
        );
    let sample_wrt = crate::combat::npc_ai_weapons_loot::take_or_init_extended_npc_weapon_runtime(
        &ctx,
        sys_id,
        &npc_sample_ent,
    );
    crate::combat::npc_ai_weapons_loot::store_extended_npc_weapon_runtime(
        &ctx,
        sys_id,
        9_990_001,
        sample_wrt.clone(),
    );
    step_extended_npc_ai_sync(&ctx, sys_id);
    with_combat_ephemeral(&ctx, |rt| {
        assert!(!rt.extended_npc_weapon_runtime.contains_key(&(sys_id, 9_990_001)));
    });

    ctx.systems.get_or_spawn(sys_id).try_remove_ball(ship_id);
    let npc_b =
        spawn_specialized_npc_entity(&ctx, sys_id, 2_386, "Pirate", 500_010, DVec3::ZERO).await;
    let npc_b_ent = with_combat_ephemeral(&ctx, |rt| rt.npc_entities[&npc_b].clone());
    let wrt_b = crate::combat::npc_ai_weapons_loot::take_or_init_extended_npc_weapon_runtime(
        &ctx,
        sys_id,
        &npc_b_ent,
    );
    crate::combat::npc_ai_weapons_loot::store_extended_npc_weapon_runtime(
        &ctx,
        sys_id,
        npc_b,
        wrt_b.clone(),
    );
    crate::combat::npc_ai_weapons_loot::store_extended_npc_weapon_runtime(
        &ctx,
        sys_id,
        9_990_002,
        wrt_b.clone(),
    );
    step_extended_npc_ai_sync(&ctx, sys_id);
    with_combat_ephemeral(&ctx, |rt| {
        assert!(rt.extended_npc_weapon_runtime.contains_key(&(sys_id, npc_b)));
        assert!(!rt.extended_npc_weapon_runtime.contains_key(&(sys_id, 9_990_002)));
    });

    // 3. CombatRuntimeState::sync_from_persisted retains matching live (sid, bid, type_id) only,
    // drops stale/mismatched entries, and clears all entries on empty persisted payloads.
    let mut persisted =
        crate::services::combat_crimewatch::with_combat_runtime(&ctx, |rt| rt.to_persisted());
    let wrong_sys_id = sys_id + 1;
    let mut rt_clone = crate::services::combat_crimewatch::CombatRuntimeState::default();
    rt_clone.npc_entities.insert(npc_b, npc_b_ent.clone());
    rt_clone.extended_npc_weapon_runtime.insert(
        (sys_id, npc_b),
        wrt_b.clone(),
    );
    rt_clone
        .extended_npc_weapon_runtime
        .insert((sys_id, 9_990_003), sample_wrt.clone());
    rt_clone
        .extended_npc_weapon_runtime
        .insert((wrong_sys_id, npc_b), sample_wrt);
    rt_clone.sync_from_persisted(&persisted);
    assert!(rt_clone.extended_npc_weapon_runtime.contains_key(&(sys_id, npc_b)));
    assert!(!rt_clone.extended_npc_weapon_runtime.contains_key(&(sys_id, 9_990_003)));
    assert!(!rt_clone.extended_npc_weapon_runtime.contains_key(&(wrong_sys_id, npc_b)));

    // Type-ID mismatch is also pruned on sync_from_persisted.
    if let Some(wrt_live) = rt_clone.extended_npc_weapon_runtime.get_mut(&(sys_id, npc_b)) {
        wrt_live.npc_type_id = npc_b_ent.type_id.saturating_add(999);
    }
    rt_clone.sync_from_persisted(&persisted);
    assert!(!rt_clone.extended_npc_weapon_runtime.contains_key(&(sys_id, npc_b)));

    // Empty persisted extended JSON clears everything.
    persisted.extended_runtime_json.clear();
    rt_clone
        .extended_npc_weapon_runtime
        .insert((sys_id, npc_b), wrt_b);
    rt_clone.sync_from_persisted(&persisted);
    assert!(rt_clone.extended_npc_weapon_runtime.is_empty());
}
