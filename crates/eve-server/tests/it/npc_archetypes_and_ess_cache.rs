//! Integration tests for SDE-derived NPC behavioral archetypes and ESS cache priming.

use std::collections::HashMap;
use eve_db::{CharacterRecord, InventoryItemRecord, IskCents, NeuralAttributes, SystemDynamicState};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::combat_crimewatch::{
    destroy_specialized_npc_and_queue_bounty, derive_npc_archetype,
    spawn_npc_entity_from_sde, spawn_specialized_npc_entity,
    step_extended_npc_ai, with_combat_runtime, with_mut_combat_runtime,
};
use glam::DVec3;

const NULLSEC_SYS_ID: u32 = 30_004_759; // VFK-IV (K-space null-sec)
const HIGHSEC_JITA_SYS_ID: u32 = 30_000_142; // Jita (High-sec)
const CHAR_ID: i64 = 99_000_101;
const SHIP_ID: i64 = 99_000_201;
const RIFTER_TYPE_ID: u32 = 587;

fn dict_get<'a>(rep: &'a PyRep, key: &str) -> Option<&'a PyRep> {
    match rep {
        PyRep::KeyVal(map) => map.get(key),
        PyRep::Dict(entries) => entries
            .iter()
            .find_map(|(k, v)| (k.as_str() == Some(key)).then_some(v)),
        _ => None,
    }
}

async fn setup_pilot_session(
    ctx: &ServerContext,
    user_id: u64,
    char_id: i64,
    ship_id: i64,
    sys_id: u32,
) -> SessionState {
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.insert(
            char_id,
            CharacterRecord {
                character_id: char_id,
                user_id,
                name: format!("Pilot {char_id}"),
                corporation_id: 1_000_125,
                alliance_id: None,
                war_faction_id: None,
                station_id: None,
                structure_id: None,
                solar_system_id: sys_id,
                constellation_id: 20_000_020,
                region_id: 10_000_002,
                active_ship_id: ship_id,
                balance: IskCents::from_isk(100_000_000),
                plex_balance: 0,
                loyalty_points: HashMap::new(),
                standings: HashMap::new(),
                security_status: 0.0,
                clone_grade: 2,
                home_station_id: 60_003_760,
                race_id: 1,
                bloodline_id: 1,
                ancestry_id: 1,
                school_id: 1,
                neural: NeuralAttributes::default(),
                skill_points: 5_000_000.0,
                activity_status: None,
                free_skill_points: 0.0,
                last_clone_jump_unix: 0,
                jump_fatigue_minutes: 0.0,
                jump_cooldown_until_unix: 0,
                jump_fatigue_until_unix: 0,
                description: String::new(),
                title: String::new(),
                gender: 1,
                create_date_unix: 1_700_000_000,
                fleet_id: None,
                version: 1,
                system_epoch: 0,
            },
        );
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: RIFTER_TYPE_ID,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: eve_db::InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: RIFTER_TYPE_ID,
                position: DVec3::new(1_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 350.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 35.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_type_combat_state(RIFTER_TYPE_ID),
        )
        .await;

    let mut sess = SessionState::default();
    sess.user_id = Some(user_id);
    sess.character_id = Some(char_id);
    sess.ship_id = Some(ship_id);
    sess.solar_system_id = Some(sys_id);
    sess.station_id = None;
    sess
}

#[tokio::test]
async fn test_npc_archetypes_dynamically_derived_from_sde() {
    // parity: feat:npc.rogue-drones
    // parity: feat:npc.incursion-npc
    // parity: feat:npc.edencom-npc
    // parity: feat:npc.officer-npc
    // parity: feat:npc.sleepers
    eve_testkit::attest(&[
        "feat:npc.rogue-drones",
        "feat:npc.incursion-npc",
        "feat:npc.edencom-npc",
        "feat:npc.officer-npc",
        "feat:npc.sleepers",
    ]);

    let ctx = ServerContext::new();
    let sys_id = 30_002_537;
    let char_id = 91_000_050;
    let ship_id = 101_000_050;

    let _sess = setup_pilot_session(&ctx, 8050, char_id, ship_id, sys_id).await;

    // 1. Verify dynamic archetype derivation from SDE group/type metadata
    assert_eq!(derive_npc_archetype(&ctx, 30188), "Sleeper"); // Sleepless Patroller (group 983)
    assert_eq!(derive_npc_archetype(&ctx, 3744), "RogueDrone"); // Silverfish Alvi (group 805)
    assert_eq!(derive_npc_archetype(&ctx, 54335), "EDENCOM"); // EDENCOM Fortification Orca (group 4034, faction 500027)
    assert_eq!(derive_npc_archetype(&ctx, 2845), "IncursionSansha"); // Outuni Mesen (group 1056)
    assert_eq!(derive_npc_archetype(&ctx, 13654), "Officer"); // Cormack Vaaja (group 574)

    // 2. Sleeper (`30188`): spawn without passing archetype string, verify remote repair & blue loot drop
    let sleeper_npc = spawn_npc_entity_from_sde(&ctx, sys_id, 30188, 41, DVec3::new(5_000.0, 0.0, 0.0)).await;
    let (sleeper_arch, sl_s_rep, sl_a_rep) = with_combat_runtime(&ctx, |rt| {
        let n = &rt.npc_entities[&sleeper_npc];
        (n.archetype.clone(), n.remote_shield_rep, n.remote_armor_rep)
    });
    assert_eq!(sleeper_arch, "Sleeper");
    assert_eq!(sl_s_rep, 250.0);
    assert_eq!(sl_a_rep, 250.0);
    let (sl_wreck, sl_bounty, sl_loot) =
        destroy_specialized_npc_and_queue_bounty(&ctx, sys_id, sleeper_npc, char_id).await;
    assert_eq!(sl_bounty, 0.0, "Sleepers pay 0 ISK direct bounty");
    assert!(sl_loot.contains(&30744), "Sleeper must drop Sleeper Data Library blue loot (30744)");
    assert!(sl_loot.contains(&30259), "Sleeper must drop Melted Nanoribbons ancient salvage (30259)");
    for &tid in &sl_loot {
        assert!(ctx.sde.types.contains_key(&tid), "Sleeper loot {tid} must exist in SDE types");
    }
    let wreck_type = ctx.db.with_snapshot(|snap| snap.items.get(&sl_wreck).map(|i| i.type_id));
    assert_eq!(wreck_type, Some(30495), "Sleeper drops Sleeper Intermediate Wreck (30495)");

    // 3. Rogue Drone (`3744`): spawn dynamically, verify archetype and Drone Alloy drop
    let drone_npc = spawn_npc_entity_from_sde(&ctx, sys_id, 3744, 42, DVec3::new(6_000.0, 0.0, 0.0)).await;
    let drone_arch = with_combat_runtime(&ctx, |rt| rt.npc_entities[&drone_npc].archetype.clone());
    assert_eq!(drone_arch, "RogueDrone");
    let (_dw, _db, drone_loot) =
        destroy_specialized_npc_and_queue_bounty(&ctx, sys_id, drone_npc, char_id).await;
    assert!(drone_loot.contains(&11732), "Rogue Drone must drop Drone Alloy (11732)");

    // 4. EDENCOM (`54335`): spawn dynamically, verify archetype, damage vector, and vorton arc hostility filtering
    let edencom_npc = spawn_specialized_npc_entity(&ctx, sys_id, 54335, "", 43, DVec3::new(7_000.0, 0.0, 0.0)).await;
    let (ed_arch, ed_dmg) = with_combat_runtime(&ctx, |rt| {
        let n = &rt.npc_entities[&edencom_npc];
        (n.archetype.clone(), n.damage_vector)
    });
    assert_eq!(ed_arch, "EDENCOM");
    assert_eq!(ed_dmg.em, 60.0);
    assert_eq!(ed_dmg.kinetic, 60.0);

    // Set up primary hostile player target within range of EDENCOM NPC with negative standing
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_handle.set_ball_position(ship_id, DVec3::new(7_400.0, 0.0, 0.0)).await;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.standings.insert(500_027, -10.0);
        }
    });

    // Secondary hostile target in threat table at 7,800m (400m from primary target)
    let hostile_ship_id = 91_000_151;
    let _ = sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: hostile_ship_id,
                type_id: RIFTER_TYPE_ID,
                position: DVec3::new(7_800.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 350.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 35.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_type_combat_state(RIFTER_TYPE_ID),
        )
        .await;

    // Neutral ball at 7,600m (200m from primary target, closer than hostile ship, but NOT in threat table)
    let neutral_ship_id = 91_000_152;
    let _ = sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: neutral_ship_id,
                type_id: RIFTER_TYPE_ID,
                position: DVec3::new(7_600.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 350.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 35.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_type_combat_state(RIFTER_TYPE_ID),
        )
        .await;

    // Insert hostile target into threat table
    with_mut_combat_runtime(&ctx, |rt| {
        if let Some(npc) = rt.npc_entities.get_mut(&edencom_npc) {
            npc.threat_table.insert(hostile_ship_id, 150.0);
            npc.aggression_threat.insert(hostile_ship_id, 150.0);
        }
    });

    let pri_before = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    let host_before = sys_handle.get_combat_state(hostile_ship_id).await.unwrap().unwrap();
    let neut_before = sys_handle.get_combat_state(neutral_ship_id).await.unwrap().unwrap();

    // Step AI: EDENCOM Vorton projector fires at primary target and arcs ONLY to hostile target in threat table
    step_extended_npc_ai(&ctx, sys_id).await;

    let pri_after = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    let host_after = sys_handle.get_combat_state(hostile_ship_id).await.unwrap().unwrap();
    let neut_after = sys_handle.get_combat_state(neutral_ship_id).await.unwrap().unwrap();

    assert!(pri_after.shield_hp < pri_before.shield_hp, "Primary hostile target must be damaged");
    assert!(host_after.shield_hp < host_before.shield_hp, "Secondary hostile target in threat table must receive arc damage");
    assert_eq!(neut_after.shield_hp, neut_before.shield_hp, "Neutral ball must NOT receive vorton arc damage");

    let (_ew, _eb, ed_loot) =
        destroy_specialized_npc_and_queue_bounty(&ctx, sys_id, edencom_npc, char_id).await;
    assert!(!ed_loot.is_empty(), "EDENCOM NPC must drop salvage");
    for &tid in &ed_loot {
        assert!(ctx.sde.types.contains_key(&tid), "EDENCOM loot {tid} must exist in SDE types");
    }

    // 5. Incursion Sansha (`2845`): spawn dynamically, verify archetype and remote reps
    let incursion_npc = spawn_specialized_npc_entity(&ctx, sys_id, 2845, "", 44, DVec3::new(8_000.0, 0.0, 0.0)).await;
    let (inc_arch, inc_s_rep, inc_a_rep) = with_combat_runtime(&ctx, |rt| {
        let n = &rt.npc_entities[&incursion_npc];
        (n.archetype.clone(), n.remote_shield_rep, n.remote_armor_rep)
    });
    assert_eq!(inc_arch, "IncursionSansha");
    assert_eq!(inc_s_rep, 250.0);
    assert_eq!(inc_a_rep, 250.0);
    let _ = destroy_specialized_npc_and_queue_bounty(&ctx, sys_id, incursion_npc, char_id).await;

    // 6. Officer (`13654`): spawn dynamically, verify officer bounty and faction module drop
    let officer_npc = spawn_specialized_npc_entity(&ctx, sys_id, 13654, "", 45, DVec3::new(9_000.0, 0.0, 0.0)).await;
    let off_arch = with_combat_runtime(&ctx, |rt| rt.npc_entities[&officer_npc].archetype.clone());
    assert_eq!(off_arch, "Officer");
    let (_ow, off_bounty, off_loot) =
        destroy_specialized_npc_and_queue_bounty(&ctx, sys_id, officer_npc, char_id).await;
    assert!(off_bounty >= 25_000_000.0, "Officer NPC must pay high bounty");
    assert!(off_loot.contains(&14254), "Officer NPC must drop officer module (14254)");

    // 7. Triglavian (`48086`): spawn dynamically, verify archetype and SDE loot (database & condensate)
    let triglavian_npc = spawn_specialized_npc_entity(&ctx, sys_id, 48086, "", 46, DVec3::new(10_000.0, 0.0, 0.0)).await;
    let trig_arch = with_combat_runtime(&ctx, |rt| rt.npc_entities[&triglavian_npc].archetype.clone());
    assert_eq!(trig_arch, "Triglavian");
    let (_tw, _tb, trig_loot) =
        destroy_specialized_npc_and_queue_bounty(&ctx, sys_id, triglavian_npc, char_id).await;
    assert!(trig_loot.contains(&48121), "Triglavian NPC must drop Triglavian Survey Database (48121)");
    assert!(trig_loot.contains(&48112), "Triglavian NPC must drop Zero-Point Condensate (48112)");
    for &tid in &trig_loot {
        assert!(ctx.sde.types.contains_key(&tid), "Triglavian loot {tid} must exist in SDE types");
    }
}

#[tokio::test]
async fn test_ess_mgr_prime_cache_result_canonical() {
    // parity: rpc:essMgr.prime_cache_result
    eve_testkit::attest(&["rpc:essMgr.prime_cache_result"]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut sess = setup_pilot_session(&ctx, 8051, CHAR_ID, SHIP_ID, NULLSEC_SYS_ID).await;

    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states.insert(
            NULLSEC_SYS_ID,
            SystemDynamicState {
                solar_system_id: NULLSEC_SYS_ID,
                dbs_multiplier: 1.40,
                ess_main_bank: IskCents::from_f64_isk(25_000_000.0),
                ess_reserve_bank: IskCents::from_f64_isk(60_000_000.0),
                ess_linked_character_id: None,
                ess_link_started_tick: None,
                sov_alliance_id: None,
                military_index: 2,
                industrial_index: 2,
                strategic_index: 2,
                fw_contested_pct: 0.0,
                insurgency_corruption_stage: 0,
                insurgency_suppression_stage: 0,
                wormhole_remaining_mass_kg: 0.0,
                wormhole_destination_system_id: None,
                signature_position: None,
                fw_complex_position: None,
                fw_capture_seconds_elapsed: 0,
                fw_capture_duration_seconds: 0,
                last_fw_capture_tick: 0,
                last_sov_claim_tick: 0,
            },
        );
    });

    let kw = HashMap::new();

    // 1. prime_cache_result with no args defaults to current solar system
    let prime_rep = ctx
        .dispatch_rpc(&mut sess, "essMgr", None, "prime_cache_result", &[], &kw, None)
        .await
        .expect("prime_cache_result");

    assert_eq!(
        dict_get(&prime_rep, "solarSystemID"),
        Some(&PyRep::Integer(NULLSEC_SYS_ID as i64))
    );
    assert_eq!(
        dict_get(&prime_rep, "essID"),
        Some(&PyRep::Integer(820_000_000 + NULLSEC_SYS_ID as i64))
    );
    assert_eq!(
        dict_get(&prime_rep, "beaconID"),
        Some(&PyRep::Integer(820_000_000 + NULLSEC_SYS_ID as i64))
    );
    assert_eq!(
        dict_get(&prime_rep, "mainValue"),
        Some(&PyRep::Real(25_000_000.0))
    );
    assert_eq!(
        dict_get(&prime_rep, "reserveValue"),
        Some(&PyRep::Real(60_000_000.0))
    );
    assert_eq!(dict_get(&prime_rep, "mainBankLink"), Some(&PyRep::None));
    assert_eq!(
        dict_get(&prime_rep, "reserveBankActiveLinks"),
        Some(&PyRep::Integer(0))
    );

    // 2. prime_cache_result with explicit solarSystemID argument
    let explicit_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "essMgr",
            None,
            "prime_cache_result",
            &[PyRep::Integer(NULLSEC_SYS_ID as i64)],
            &kw,
            None,
        )
        .await
        .expect("prime_cache_result with arg");

    assert_eq!(
        dict_get(&explicit_rep, "mainValue"),
        Some(&PyRep::Real(25_000_000.0))
    );
    assert_eq!(
        dict_get(&explicit_rep, "reserveValue"),
        Some(&PyRep::Real(60_000_000.0))
    );

    // 3. Link character to main bank and verify primed link metadata
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(st) = snap.system_states.get_mut(&NULLSEC_SYS_ID) {
            st.ess_linked_character_id = Some(CHAR_ID);
            st.ess_link_started_tick = Some(100);
        }
    });

    let linked_rep = ctx
        .dispatch_rpc(&mut sess, "essMgr", None, "prime_cache_result", &[], &kw, None)
        .await
        .expect("prime_cache_result linked");

    let link_obj = dict_get(&linked_rep, "mainBankLink").expect("mainBankLink present");
    assert_eq!(
        dict_get(link_obj, "characterID"),
        Some(&PyRep::Integer(CHAR_ID))
    );
    assert_eq!(dict_get(link_obj, "linkID"), Some(&PyRep::Integer(1)));
    assert!(dict_get(link_obj, "startedAt").is_some());
    assert!(dict_get(link_obj, "completesAt").is_some());

    // 4. prime_cache_result on high-sec Jita returns None due to null-sec guard
    let jita_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "essMgr",
            None,
            "prime_cache_result",
            &[PyRep::Integer(HIGHSEC_JITA_SYS_ID as i64)],
            &kw,
            None,
        )
        .await
        .expect("prime_cache_result on high-sec Jita");
    assert_eq!(jita_rep, PyRep::None);
}
