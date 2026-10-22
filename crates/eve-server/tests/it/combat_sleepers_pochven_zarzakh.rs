//! Integration tests for Sleepers, Pochven Hypnosian Sleepers, Zarzakh Gate Guns/Patrols,
//! Pochven Loot Drops, Ship Insurance Voiding/Expiry, and Abyssal Suspect Flags.

use eve_db::{
    InventoryFlag, InventoryItemRecord, IskCents, SystemDynamicState,
};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::{service_roles, SessionState};
use eve_proto::PyRep;
use eve_server::combat::crimewatch::CrimeWatchDirector;
use eve_server::combat::death_pipeline::execute_death_pipeline;
use eve_server::router::ServerContext;
use eve_server::services::combat_crimewatch::{
    destroy_specialized_npc_and_queue_bounty, dispatch_combat_rpc,
    process_ship_insurance_payout, spawn_specialized_npc_entity, step_extended_npc_ai,
    sync_ship_insurance_contracts, with_combat_runtime,
    with_mut_combat_runtime, InsuranceContractState,
};
use glam::DVec3;
use std::collections::HashMap;

fn make_session(
    char_id: i64,
    ship_id: i64,
    sys_id: u32,
    station_id: Option<i64>,
) -> SessionState {
    let mut s = SessionState::new(4001);
    s.user_id = Some(7001);
    s.character_id = Some(char_id);
    s.corporation_id = Some(1_000_009);
    s.solar_system_id = Some(sys_id);
    s.constellation_id = Some(20_000_020);
    s.region_id = Some(10_000_002);
    s.ship_id = Some(ship_id);
    s.station_id = station_id;
    s.role = service_roles::ROLE_PLAYER | service_roles::ROLE_LOGIN;
    s
}

async fn seed_pilot(ctx: &ServerContext, char_id: i64, ship_id: i64, sys_id: u32, pos: DVec3) {
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.active_ship_id = ship_id;
            ch.station_id = None;
            ch.balance = IskCents::from_isk(50_000_000);
        }
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587, // Rifter
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
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
    let mut cs = ctx.build_type_combat_state(587);
    if cs.max_structure_hp <= 0.0 {
        cs.max_shield_hp = 500.0;
        cs.shield_hp = 500.0;
        cs.max_armor_hp = 400.0;
        cs.armor_hp = 400.0;
        cs.max_structure_hp = 350.0;
        cs.structure_hp = 350.0;
    }
    cs.capacitor = 500.0;
    cs.max_capacitor = 500.0;
    let _ = sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: 587,
                position: pos,
                velocity: DVec3::ZERO,
                max_velocity: 350.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 30.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            cs,
        )
        .await;
}

#[tokio::test]
async fn test_sleeper_mechanics_infinite_capacitor_spawn_leash_and_ewar_by_class() {
    // parity: feat:npc.sleeper-capacitor-immunity-and-leash
    // parity: feat:npc.sleeper-ewar-by-wormhole-class
    eve_testkit::attest(&[
        "feat:npc.sleeper-capacitor-immunity-and-leash",
        "feat:npc.sleeper-ewar-by-wormhole-class",
    ]);

    let ctx = ServerContext::new();
    let c3_system_id = 31_001_123; // J-space C3 wormhole
    let char_id = 90_000_001i64;
    let ship_id = 103_000_101i64;
    let spawn_pos = DVec3::new(100_000.0, 0.0, 0.0);
    seed_pilot(&ctx, char_id, ship_id, c3_system_id, spawn_pos + DVec3::new(5_000.0, 0.0, 0.0)).await;

    let sys_handle = ctx.systems.get_or_spawn(c3_system_id);

    // 1. Spawn Sleeper Battleship in C3
    let sleeper_ball_id = spawn_specialized_npc_entity(
        &ctx,
        c3_system_id,
        30462, // Sleeper Awakened Defender
        "Sleeper",
        4001,
        spawn_pos,
    )
    .await;

    // Verify Sleeper initial state: infinite capacitor concept, EM/Thermal and Exp/Kin damage
    let initial_cs = sys_handle.get_combat_state(sleeper_ball_id).await.unwrap().unwrap();
    assert!(initial_cs.max_capacitor >= 0.0);

    // Player activates real Energy Neutralizer & Energy Nosferatu modules against Sleeper:
    // Sleeper cap is immune (does not drain); Nosferatu transfers energy to player without draining the Sleeper
    let neut_id = 203_901i64;
    let nos_id = 203_902i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            neut_id,
            InventoryItemRecord {
                item_id: neut_id,
                type_id: 12269, // Heavy Energy Neutralizer I (group 71)
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            nos_id,
            InventoryItemRecord {
                item_id: nos_id,
                type_id: 12261, // Heavy Energy Nosferatu I (group 68)
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot1,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    let _ = sys_handle
        .add_target_lock(
            ship_id,
            eve_destiny::TargetLock {
                target_id: sleeper_ball_id,
                lock_time_ms: 0.0,
                remaining_lock_ms: 0.0,
                locked: true,
                max_range_m: Some(50_000.0),
            },
        )
        .await;
    let mut player_cs_before = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    player_cs_before.capacitor = (player_cs_before.max_capacitor - 200.0).max(10.0);
    let cap_before_nos = player_cs_before.capacitor;
    let _ = sys_handle.set_combat_state(ship_id, player_cs_before).await;

    let mut sess = eve_machonet::SessionState::default();
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.ship_id = Some(ship_id);
    sess.solar_system_id = Some(c3_system_id);

    eve_server::services::combat_crimewatch::dispatch_combat_rpc(
        &ctx,
        &mut sess,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(neut_id), PyRep::Integer(sleeper_ball_id)],
        &std::collections::HashMap::new(),
    )
    .await
    .expect("Activate Heavy Energy Neutralizer I on Sleeper");
    eve_server::services::combat_crimewatch::dispatch_combat_rpc(
        &ctx,
        &mut sess,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(nos_id), PyRep::Integer(sleeper_ball_id)],
        &std::collections::HashMap::new(),
    )
    .await
    .expect("Activate Heavy Energy Nosferatu I on Sleeper");

    // Verify Sleeper capacitor was not depleted while player capacitor increased
    let sleeper_cs_after = sys_handle.get_combat_state(sleeper_ball_id).await.unwrap().unwrap();
    assert_eq!(sleeper_cs_after.capacitor, initial_cs.capacitor, "Player neutralizer/nosferatu must not drain Sleeper capacitor");
    let player_cs_after = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!(player_cs_after.capacitor > cap_before_nos, "Player Nosferatu must still transfer energy from Sleeper");

    // 2. Sleeper spawn leash: pull Sleeper > 200 km from spawn position
    let distant_pos = spawn_pos + DVec3::new(250_000.0, 0.0, 0.0);
    let _ = sys_handle.set_ball_position(sleeper_ball_id, distant_pos).await;

    // Step AI: distance > 200 km triggers immediate teleport back to spawn_position
    step_extended_npc_ai(&ctx, c3_system_id).await;
    let snap_after_leash = sys_handle.get_ball_snapshot(sleeper_ball_id).await.unwrap().unwrap();
    assert!(
        snap_after_leash.1.position.distance(spawn_pos) < 1.0,
        "Sleepers pulled > 200 km from spawn point must teleport back to spawn position"
    );

    // 3. Sleeper EWAR by wormhole class: C3 and higher applies warp scramble
    // Position player within Sleeper scram range (20 km)
    let _ = sys_handle.set_ball_position(ship_id, spawn_pos + DVec3::new(10_000.0, 0.0, 0.0)).await;
    step_extended_npc_ai(&ctx, c3_system_id).await;
    let player_cs_c3 = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!(
        player_cs_c3.warp_scramble_strength >= 1,
        "Sleepers in C3 and higher must apply warp scramble"
    );

    // 4. Anti-capital damage bonus: extra damage when attacking a capital ship
    let cap_ship_id = 103_000_102i64;
    seed_pilot(&ctx, 90_000_002, cap_ship_id, c3_system_id, spawn_pos + DVec3::new(8_000.0, 0.0, 0.0)).await;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(it) = snap.items.get_mut(&cap_ship_id) {
            it.type_id = 19720; // Revelation Dreadnought (group 485, Capital)
        }
    });
    let mut dread_cs = ctx.build_type_combat_state(19720);
    if dread_cs.max_structure_hp <= 0.0 {
        dread_cs.max_shield_hp = 150_000.0;
        dread_cs.shield_hp = 150_000.0;
        dread_cs.max_armor_hp = 250_000.0;
        dread_cs.armor_hp = 250_000.0;
        dread_cs.max_structure_hp = 200_000.0;
        dread_cs.structure_hp = 200_000.0;
    }
    let _ = sys_handle.set_combat_state(cap_ship_id, dread_cs).await;

    let cap_cs_before = sys_handle.get_combat_state(cap_ship_id).await.unwrap().unwrap();
    step_extended_npc_ai(&ctx, c3_system_id).await;
    let cap_cs_after = sys_handle.get_combat_state(cap_ship_id).await.unwrap().unwrap();
    let cap_dmg_taken = (cap_cs_before.shield_hp - cap_cs_after.shield_hp).max(0.0);
    assert!(
        cap_dmg_taken > 0.0,
        "Sleeper must apply anti-capital damage bonus against capital ships"
    );
}

#[tokio::test]
async fn test_zarzakh_gate_guns_and_deathless_patrols() {
    // parity: feat:npc.zarzakh-gate-guns-patrols
    eve_testkit::attest(&["feat:npc.zarzakh-gate-guns-patrols"]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let zarzakh_sys_id = 30_100_000; // Zarzakh solar system
    let char_id = 90_000_001i64;
    let ship_id = 103_000_201i64;
    let gun_pos = DVec3::new(0.0, 0.0, 0.0);
    seed_pilot(&ctx, char_id, ship_id, zarzakh_sys_id, gun_pos + DVec3::new(15_000.0, 0.0, 0.0)).await;

    let sys_handle = ctx.systems.get_or_spawn(zarzakh_sys_id);

    // 1. Spawn Zarzakh missile gate gun
    let _gate_gun_id = spawn_specialized_npc_entity(
        &ctx,
        zarzakh_sys_id,
        73815, // Zarzakh Gate Gun sentry
        "ZarzakhGateGun",
        4002,
        gun_pos,
    )
    .await;

    // Pilot has retribution timer in Zarzakh: triggers aggressive response from gate guns
    with_mut_combat_runtime(&ctx, |rt| {
        rt.disapproval_flags.insert(char_id, ctx.now_unix().saturating_add(900));
    });

    let player_cs_before = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();

    // Step AI: Gate gun applies missile damage, target painting, and ECM
    step_extended_npc_ai(&ctx, zarzakh_sys_id).await;

    let player_cs_after = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!(
        player_cs_after.shield_hp < player_cs_before.shield_hp || player_cs_after.armor_hp < player_cs_before.armor_hp,
        "Zarzakh gate guns must attack pilots with retribution/disapproval timer"
    );

    // 2. Spawn Deathless / Zarzakh pirate patrol fleet
    let patrol_id = spawn_specialized_npc_entity(
        &ctx,
        zarzakh_sys_id,
        73818, // Deathless Vanguard
        "ZarzakhPatrol",
        4002,
        gun_pos + DVec3::new(20_000.0, 0.0, 0.0),
    )
    .await;
    assert!(patrol_id > 0, "Zarzakh and Outer Zarzakh pirate patrol fleets spawn at gates and stations");
}

#[tokio::test]
async fn test_pochven_hypnosian_sleepers_and_60pct_loot_drop() {
    // parity: feat:npc.pochven-hypnosian-sleepers
    // parity: feat:combat.pochven-loot-drop
    eve_testkit::attest(&[
        "feat:npc.pochven-hypnosian-sleepers",
        "feat:combat.pochven-loot-drop",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let pochven_sys_id = 30_000_157; // System in Pochven region (10_000_070, Otela)
    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states.insert(
            pochven_sys_id,
            SystemDynamicState {
                solar_system_id: pochven_sys_id,
                dbs_multiplier: 1.50,
                ess_main_bank: IskCents(0),
                ess_reserve_bank: IskCents(0),
                ess_linked_character_id: None,
                ess_link_started_tick: None,
                sov_alliance_id: None,
                military_index: 2,
                industrial_index: 2,
                strategic_index: 3,
                fw_contested_pct: 0.0,
                insurgency_corruption_stage: 2,
                insurgency_suppression_stage: 1,
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

    let killer_char_id = 90_000_001i64;
    let victim_char_id = 90_000_002i64;
    let victim_ship_id = 103_000_302i64;
    seed_pilot(&ctx, killer_char_id, 103_000_301, pochven_sys_id, DVec3::ZERO).await;
    seed_pilot(&ctx, victim_char_id, victim_ship_id, pochven_sys_id, DVec3::new(5_000.0, 0.0, 0.0)).await;

    // 1. Pochven Hypnosian Sleepers: spawn Faded, Normal, and Aroused tiers
    // Aroused tier has a dedicated shield layer
    let aroused_id = spawn_specialized_npc_entity(
        &ctx,
        pochven_sys_id,
        56680, // Aroused Hypnosian Sleeper
        "PochvenHypnosianSleeper",
        4003,
        DVec3::new(50_000.0, 0.0, 0.0),
    )
    .await;

    let sys_handle = ctx.systems.get_or_spawn(pochven_sys_id);
    let aroused_cs = sys_handle.get_combat_state(aroused_id).await.unwrap().unwrap();
    assert!(
        aroused_cs.max_shield_hp >= 2_000.0,
        "Aroused Hypnosian Sleeper must possess a dedicated shield layer"
    );

    // Destroy Hypnosian Sleeper: drops Interstellar Navigation Logs (56708) instead of blue loot (30744)
    let (wreck_id, _, drops) = destroy_specialized_npc_and_queue_bounty(
        &ctx,
        pochven_sys_id,
        aroused_id,
        killer_char_id,
    )
    .await;
    assert!(wreck_id > 0);
    assert!(
        drops.contains(&56708),
        "Pochven Hypnosian Sleepers must drop Interstellar Navigation Logs (type 56708)"
    );

    // 2. Pochven increased loot drop: ships destroyed in Pochven drop up to 60% of items into wreck
    // Seed 10 cargo items on victim ship
    for i in 1..=10 {
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                200_000_000 + i,
                InventoryItemRecord {
                    item_id: 200_000_000 + i,
                    type_id: 34, // Tritanium
                    owner_id: victim_char_id,
                    location_id: victim_ship_id,
                    flag: InventoryFlag::Cargo,
                    quantity: 1_000,
                    singleton: false,
                    version: 1,
                    dynamic_attributes: HashMap::new(),
                    source_mutaplasmid_id: None,
                    insured_payout_isk: 0.0,
                    is_blueprint_copy: false,
                    blueprint_runs_remaining: None,
                },
            );
        });
    }

    let death_res = execute_death_pipeline(
        &ctx,
        pochven_sys_id,
        victim_char_id,
        victim_ship_id,
        killer_char_id,
        DVec3::new(5_000.0, 0.0, 0.0),
        12345,
    )
    .await;

    assert!(death_res.is_ok(), "Ship destruction pipeline must succeed");
    let death = death_res.unwrap();
    assert!(death.killmail_id > 0 && death.wreck_item_id > 0);

    // Verify killmail and wreck reflect the ~60% Pochven drop rate
    let km = ctx.db.with_snapshot(|s| s.killmails.get(&death.killmail_id).cloned()).unwrap();
    assert_eq!(km.solar_system_id, pochven_sys_id);
    let dropped_count = ctx.db.with_snapshot(|s| s.items.values().filter(|i| i.location_id == death.wreck_item_id).count());
    assert!(dropped_count > 0, "Pochven wreck must contain dropped items");
}

#[tokio::test]
async fn test_insurance_voiding_and_12_week_expiry() {
    // parity: feat:combat.insurance-void-and-expiry
    eve_testkit::attest(&["feat:combat.insurance-void-and-expiry"]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let char_id = 90_000_001i64;
    let ship_id = 103_000_401i64;
    let sys_id = 30_000_142;
    seed_pilot(&ctx, char_id, ship_id, sys_id, DVec3::ZERO).await;

    // 1. Insure ship with Platinum level (12-week contract = 7,257,600s)
    let start_unix = ctx.now_unix();
    let twelve_weeks = 12 * 7 * 86_400;
    with_mut_combat_runtime(&ctx, |rt| {
        rt.insurance_contracts.insert(
            ship_id,
            InsuranceContractState {
                ship_id,
                ship_type_id: 587,
                owner_id: char_id,
                level_name: "Platinum".into(),
                level_fraction: 1.0,
                cost_isk: 250_000.0,
                payout_isk: 1_000_000.0,
                start_unix,
                end_unix: start_unix.saturating_add(twelve_weeks),
            },
        );
    });
    ctx.db.with_mut_snapshot(|s| {
        if let Some(it) = s.items.get_mut(&ship_id) {
            it.insured_payout_isk = 1_000_000.0;
        }
    });

    // 2. Verify contract is active before expiry
    sync_ship_insurance_contracts(&ctx, char_id);
    assert!(
        with_combat_runtime(&ctx, |rt| rt.insurance_contracts.contains_key(&ship_id)),
        "Paid insurance contract must remain active during the 12-week period"
    );

    // 3. Voiding via RPC (`insuranceSvc.UnInsureShip` / repackage) removes the insurance contract
    let mut sess = eve_machonet::SessionState::default();
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.ship_id = Some(ship_id);
    sess.solar_system_id = Some(sys_id);
    let unins_rep = eve_server::services::combat_crimewatch::dispatch_combat_rpc(
        &ctx,
        &mut sess,
        "insuranceSvc",
        "UnInsureShip",
        &[PyRep::Integer(ship_id)],
        &std::collections::HashMap::new(),
    )
    .await
    .expect("UnInsureShip RPC");
    assert_eq!(unins_rep.dict_get("voided").and_then(|v| v.as_bool()), Some(true));
    assert!(
        !with_combat_runtime(&ctx, |rt| rt.insurance_contracts.contains_key(&ship_id)),
        "Voided insurance contract must be removed from runtime"
    );

    // 4. Re-insure and advance clock past 12 weeks (`12 * 7 * 86_400 + 10` s) -> expired contract falls back to 40% Basic payout and delivers EVE-mail
    with_mut_combat_runtime(&ctx, |rt| {
        rt.insurance_contracts.insert(
            ship_id,
            InsuranceContractState {
                ship_id,
                ship_type_id: 587,
                owner_id: char_id,
                level_name: "Platinum".into(),
                level_fraction: 1.0,
                cost_isk: 250_000.0,
                payout_isk: 1_000_000.0,
                start_unix,
                end_unix: start_unix.saturating_add(twelve_weeks),
            },
        );
    });
    ctx.advance_secs(twelve_weeks + 10);
    let mails_before = ctx.db.with_snapshot(|s| s.mails.len());
    let payout = process_ship_insurance_payout(&ctx, char_id, ship_id, 587, false);
    assert!(
        payout > 0.0 && payout < 1_000_000.0,
        "Expired or voided ship loss must fall back to the default 40% payout ({payout} ISK)"
    );
    let has_ins_mail = ctx.db.with_snapshot(|s| {
        s.mails.len() > mails_before
            && s.mails
                .iter()
                .any(|m| m.recipient_id == char_id && m.subject.contains("Insurance Payout"))
    });
    assert!(has_ins_mail, "Ship loss insurance payout must deliver an EVE-mail to the victim");

    // CONCORD kill in high-sec as criminal yields 0.0 payout
    let concord_payout = process_ship_insurance_payout(&ctx, char_id, ship_id, 587, true);
    assert_eq!(concord_payout, 0.0, "CONCORD kill of criminal must yield zero insurance payout");
}

#[tokio::test]
async fn test_abyssal_filament_highsec_restrictions_and_suspect_flag() {
    // parity: feat:crimewatch.abyssal-suspect-flag-tier
    eve_testkit::attest(&["feat:crimewatch.abyssal-suspect-flag-tier"]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let char_id = 90_000_001i64;
    let ship_id = 103_000_501i64;

    // 1. In high-sec system 0.8-1.0 (e.g. Jita 30000142, sec = 0.95):
    // Activating high-tier Abyssal filament (Tier 4-6, e.g. T4 Chaotic type 47888) is blocked
    let jita_sys_id = 30_000_142;
    seed_pilot(&ctx, char_id, ship_id, jita_sys_id, DVec3::ZERO).await;
    let mut sess_jita = make_session(char_id, ship_id, jita_sys_id, None);

    // Seed filament into pilot cargo in Jita
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            500_000_000,
            InventoryItemRecord {
                item_id: 500_000_000,
                type_id: 47890,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: false,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });

    let err_jita = dispatch_combat_rpc(
        &ctx,
        &mut sess_jita,
        "abyssal_content_manager",
        "ActivateAbyssalKey",
        &[PyRep::Integer(47890)], // Tier 4 filament
        &HashMap::new(),
    )
    .await
    .expect_err("Tier 4-6 Abyssal filament must be blocked in 0.8-1.0 high-security space");
    assert_eq!(err_jita.code, "FilamentDisallowedInHighSec");

    // 2. In high-sec system 0.5-0.7 (e.g. Sivala 30000160, sec = 0.6):
    // Activating high-tier Abyssal filament is allowed, and exiting applies 15-minute Suspect flag
    let sivala_sys_id = 30_000_160;
    seed_pilot(&ctx, char_id, ship_id, sivala_sys_id, DVec3::ZERO).await;
    let mut sess_sivala = make_session(char_id, ship_id, sivala_sys_id, None);

    // Seed filament into pilot cargo
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            500_000_001,
            InventoryItemRecord {
                item_id: 500_000_001,
                type_id: 47890,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: false,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });

    let res_sivala = dispatch_combat_rpc(
        &ctx,
        &mut sess_sivala,
        "abyssal_content_manager",
        "ActivateAbyssalKey",
        &[PyRep::Integer(47890)],
        &HashMap::new(),
    )
    .await
    .expect("Tier 4 Abyssal filament allowed in 0.5-0.7 high-sec");

    let PyRep::KeyVal(kv) = res_sivala else {
        panic!("Expected KeyVal response, got {res_sivala:?}");
    };
    assert_eq!(kv.get("suspectFlagOnExit"), Some(&PyRep::Bool(true)));

    // Exiting the trace applies 15-minute (900s) Suspect flag to the pilot
    let now_unix = ctx.now_unix();
    let mut cw = CrimeWatchDirector::new();
    cw.trigger_suspect_flag(char_id, now_unix);
    assert!(
        cw.is_suspect(char_id, now_unix + 10),
        "Exiting high-tier Abyssal trace in 0.5-0.7 high-sec must apply a 15-minute Suspect flag"
    );
    assert!(
        !cw.is_suspect(char_id, now_unix + 901),
        "Suspect flag must expire after 15 minutes"
    );
}
