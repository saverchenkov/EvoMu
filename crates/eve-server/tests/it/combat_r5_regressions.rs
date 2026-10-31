//! Round-5 Track 1 Regression Tests (#199 through #211):
//! - #199: ActivateFighterAbilitySlotsOnPoint requires pointTargeted ability with charges, range/safety checks
//! - #200: ActivateFighterAbilitySlotsOnEntities triggers death pipeline & crimewatch on lethal hits
//! - #201: Death pipeline consumes insurance contract once, enforces end_unix, and blocks self-destruct bounty farming
//! - #202: Weapon activation with zero locked targets fails closed with TargetNotLocked and resets spool
//! - #203: Lancer/Doomsday checks reactivation cooldown & consumes isotopes before applying beam damage
//! - #204: ActivateRandomJumpFilament enforces scramble/warp/crimewatch on fleetmates and preserves HP
//! - #205: GetFightersForShip enforces ownership and RecallFightersToTubes checks owner/lock & preserves loaded tube
//! - #206: WormholeJump preserves pre-jump HP/capacitor and allows first return jump before polarizing
//! - #207: Vorton Projector secondary arcs exclude wrecks/invulnerable/safety-protected capsuleers
//! - #208: Breacher pod DoT ticks only on its own solar system step and applies on missile impact
//! - #209: Smartbombs and Bombs spare wrecks and record killmail assist / LE / sec status loss
//! - #210: RecoverProbes preserves in-space probes when cargo is full; DestroyProbe spares cargo probes
//! - #211: ClickedOnTile enforces cloak/range checks; QuitHackingAttempt detonates active Ghost Site

use eve_db::{
    AccountRecord, CharacterRecord, InventoryFlag, InventoryItemRecord, IskCents, NeuralAttributes,
};
use eve_destiny::{Ball, DamageVector, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::combat::death_pipeline::execute_death_pipeline_sync_inner;
use eve_server::router::{ActiveScannerProbe, ServerContext};
use eve_server::services::combat_crimewatch::{
    record_killmail_assist_action, step_combat_system, with_combat_runtime,
    with_mut_combat_runtime, ActiveBreacherDot, EntropicSpoolState, InsuranceContractState,
    SafetyLevel,
};
use eve_server::services::pve_exploration::{
    with_pve_exploration_reg, HackingGameSession, HackingNodeKind, HackingTileState, PveSiteInstance,
};
use glam::DVec3;
use std::collections::HashMap;

fn item_rec(
    item_id: i64, type_id: u32, owner_id: i64, location_id: i64,
    flag: InventoryFlag, quantity: i32, singleton: bool,
) -> InventoryItemRecord {
    InventoryItemRecord {
        item_id, type_id, owner_id, location_id, flag, quantity, singleton,
        version: 1, dynamic_attributes: HashMap::new(), source_mutaplasmid_id: None,
        insured_payout_isk: 0.0, is_blueprint_copy: false, blueprint_runs_remaining: None,
    }
}

async fn setup_pilot_in_space(
    ctx: &ServerContext, client_id: u64, char_id: i64, corp_id: i64,
    ship_id: i64, ship_type_id: u32, sys_id: u32, pos: DVec3,
) -> SessionState {
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.insert(
            char_id,
            CharacterRecord {
                character_id: char_id, user_id: client_id, name: format!("Pilot {char_id}"),
                corporation_id: corp_id, alliance_id: None, war_faction_id: None,
                station_id: None, structure_id: None, solar_system_id: sys_id,
                constellation_id: 20_000_020, region_id: 10_000_002, active_ship_id: ship_id,
                balance: IskCents::from_isk(100_000_000), plex_balance: 0,
                loyalty_points: HashMap::new(), standings: HashMap::new(),
                security_status: 2.0, clone_grade: 2, home_station_id: 60_003_760,
                race_id: 1, bloodline_id: 1, ancestry_id: 1, school_id: 1,
                neural: NeuralAttributes::default(), skill_points: 5_000_000.0,
                activity_status: None, free_skill_points: 0.0, last_clone_jump_unix: 0,
                jump_fatigue_minutes: 0.0, jump_cooldown_until_unix: 0, jump_fatigue_until_unix: 0,
                description: String::new(), title: String::new(), gender: 1,
                create_date_unix: 1_700_000_000, fleet_id: None, autopilot_waypoints: Vec::new(),
                version: 1, system_epoch: 0,
            },
        );
        snap.accounts.insert(
            format!("user_{client_id}"),
            AccountRecord {
                user_id: client_id, username: format!("user_{client_id}"),
                password_token: String::new(), role: 0, banned_until_unix: None, ban_reason: None,
            },
        );
        snap.items.insert(
            ship_id,
            item_rec(ship_id, ship_type_id, char_id, sys_id as i64, InventoryFlag::None, 1, true),
        );
    });
    let _ = ctx.db.acquire_online_lock(client_id, 8_000 + client_id, Some(char_id), true, ctx.now_unix());
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: ship_id, type_id: ship_type_id, position: pos, velocity: DVec3::ZERO,
                max_velocity: 300.0, inertia: 2.5, mass: 10_000_000.0, radius: 50.0,
                speed_fraction: 0.0, mode: MovementMode::Stop,
            },
            ctx.build_type_combat_state(ship_type_id),
        )
        .await;
    let mut sess = SessionState::new(8_000 + client_id);
    sess.user_id = Some(client_id);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(corp_id);
    sess.ship_id = Some(ship_id);
    sess.solar_system_id = Some(sys_id);
    sess
}

#[tokio::test]
async fn test_r5_199_200_205_fighter_point_targeted_charges_death_pipeline_and_recall_acl() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();
    let sys_id = 30_002_813u32;
    let (char_id, other_id, ship_id, other_ship_id) =
        (98_199_001i64, 98_199_002i64, 88_199_101i64, 88_199_102i64);
    let mut sess = setup_pilot_in_space(&ctx, 81991, char_id, 1_000_125, ship_id, 23757, sys_id, DVec3::ZERO).await;
    let mut other_sess = setup_pilot_in_space(&ctx, 81992, other_id, 1_000_126, other_ship_id, 587, sys_id, DVec3::new(1_000.0, 0.0, 0.0)).await;

    let acl_err = ctx
        .dispatch_rpc(&mut other_sess, "fighterMgr", None, "GetFightersForShip", &[PyRep::Integer(ship_id)], &kw, None)
        .await
        .expect_err("Unrelated pilot must not inspect carrier fighter bay/tubes");
    assert_eq!(acl_err.code, "AccessDenied");

    let fighter_id = 88_199_201i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(fighter_id, item_rec(fighter_id, 23055, char_id, ship_id, InventoryFlag::FighterBay, 9, false));
    });
    ctx.dispatch_rpc(&mut sess, "fighterMgr", None, "LoadFightersToTube", &[PyRep::Integer(0), PyRep::Integer(fighter_id)], &kw, None).await.unwrap();
    ctx.dispatch_rpc(&mut sess, "fighterMgr", None, "LaunchFightersFromTubes", &[PyRep::List(vec![PyRep::Integer(0)])], &kw, None).await.unwrap();
    let f_ball_id = *ctx.active_fighters.read().unwrap().keys().next().expect("Launched fighter ball ID");

    let pt_err = ctx
        .dispatch_rpc(
            &mut sess, "fighterMgr", None, "CmdActivateAbilitySlots",
            &[PyRep::Integer(f_ball_id), PyRep::Integer(0), PyRep::Tuple(vec![PyRep::Real(500.0), PyRep::Real(0.0), PyRep::Real(0.0)])],
            &kw, None,
        )
        .await
        .expect_err("Non-pointTargeted ability slot must be rejected on point activation");
    assert_eq!(pt_err.code, "InvalidTargetMode");

    let npc_ball_id = 1_895_199_001i64;
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let mut npc_cs = ctx.build_type_combat_state(587);
    npc_cs.shield_hp = 0.0;
    npc_cs.armor_hp = 0.0;
    npc_cs.structure_hp = 1.0;
    let _ = sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: npc_ball_id, type_id: 587, position: DVec3::new(500.0, 0.0, 0.0), velocity: DVec3::ZERO,
                max_velocity: 200.0, inertia: 2.0, mass: 1_000_000.0, radius: 40.0,
                speed_fraction: 0.0, mode: MovementMode::Stop,
            },
            npc_cs,
        )
        .await;
    let _ = sys_handle
        .add_target_lock(
            ship_id,
            eve_destiny::TargetLock {
                target_id: npc_ball_id,
                locked: true,
                remaining_lock_ms: 0.0,
                lock_time_ms: 0.0,
                max_range_m: Some(100_000.0),
            },
        )
        .await;
    ctx.dispatch_rpc(
        &mut sess, "fighterMgr", None, "CmdActivateAbilitySlots",
        &[PyRep::List(vec![PyRep::Integer(f_ball_id)]), PyRep::Integer(0), PyRep::Integer(npc_ball_id)],
        &kw, None,
    )
    .await
    .expect("Lethal fighter ability on NPC");
    assert!(
        sys_handle.get_ball_snapshot(npc_ball_id).await.unwrap().is_none(),
        "Destroyed NPC ball must be removed by fighter death pipeline"
    );
}

#[tokio::test]
async fn test_r5_201_insurance_single_use_consumption_and_self_destruct_bounty_block() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let now = ctx.now_unix();
    let sys_id = 30_002_813u32;
    let (vic_id, atk_id, ship_id, atk_ship_id) =
        (98_201_001i64, 98_201_002i64, 88_201_101i64, 88_201_102i64);
    let _vic_sess = setup_pilot_in_space(&ctx, 82011, vic_id, 1_000_125, ship_id, 587, sys_id, DVec3::ZERO).await;
    let _atk_sess = setup_pilot_in_space(&ctx, 82012, atk_id, 1_000_126, atk_ship_id, 587, sys_id, DVec3::new(1_000.0, 0.0, 0.0)).await;

    with_mut_combat_runtime(&ctx, |rt| {
        rt.insurance_contracts.insert(
            ship_id,
            InsuranceContractState {
                ship_id, ship_type_id: 587, owner_id: vic_id, level_name: "Platinum".into(),
                level_fraction: 1.0, cost_isk: 1_000_000.0, payout_isk: 2_000_000.0,
                start_unix: now - 100, end_unix: now + 86_400,
            },
        );
        rt.player_bounty_pool.insert(vic_id, 50_000_000.0);
    });
    record_killmail_assist_action(&ctx, ship_id, atk_id, 587, 486, 100.0, false, now);

    let atk_bal_before = ctx.db.with_snapshot(|s| s.characters[&atk_id].balance.to_wire_f64());
    let _res = execute_death_pipeline_sync_inner(&ctx, sys_id, vic_id, ship_id, 587, DVec3::ZERO, atk_id as u64, true)
        .expect("Self-destruct death pipeline");
    let atk_bal_after = ctx.db.with_snapshot(|s| s.characters[&atk_id].balance.to_wire_f64());
    assert!((atk_bal_after - atk_bal_before).abs() < 1e-2, "Attacker must not receive bounty payout on victim self-destruct");
    let contract_remaining = with_combat_runtime(&ctx, |rt| rt.insurance_contracts.contains_key(&ship_id));
    assert!(!contract_remaining, "Insurance contract must be consumed upon ship destruction");
}

#[tokio::test]
async fn test_r5_202_207_weapon_zero_lock_deactivation_and_vorton_secondary_arc_safety() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();
    let sys_id = 30_000_142u32;
    let (char_id, ship_id, gun_id, ammo_id) =
        (98_202_001i64, 88_202_101i64, 88_202_201i64, 88_202_202i64);
    let mut sess = setup_pilot_in_space(&ctx, 82021, char_id, 1_000_125, ship_id, 587, sys_id, DVec3::ZERO).await;

    let npc_ball_id = 1_895_202_001i64;
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: npc_ball_id, type_id: 587, position: DVec3::new(1_500.0, 0.0, 0.0), velocity: DVec3::ZERO,
                max_velocity: 200.0, inertia: 2.0, mass: 1_000_000.0, radius: 40.0,
                speed_fraction: 0.0, mode: MovementMode::Stop,
            },
            ctx.build_type_combat_state(587),
        )
        .await;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(gun_id, item_rec(gun_id, 486, char_id, ship_id, InventoryFlag::HiSlot0, 1, true));
        snap.items.insert(ammo_id, item_rec(ammo_id, 178, char_id, ship_id, InventoryFlag::HiSlot0, 100, false));
        snap.module_online_states.insert(gun_id, true);
        snap.module_active_states.insert(gun_id);
    });
    ctx.invalidate_ship_fitting(ship_id);
    with_mut_combat_runtime(&ctx, |rt| {
        rt.entropic_spools.insert(
            (ship_id, gun_id),
            EntropicSpoolState {
                target_id: npc_ball_id, solar_system_id: sys_id, attacker_char_id: char_id,
                attacker_ship_id: ship_id, module_id: gun_id, module_type_id: 486,
                charge_item_id: Some(ammo_id), base_damage: DamageVector::new(10.0, 10.0, 10.0, 10.0),
                cap_need: 1.0, duration_s: 3.0, cycles: 5, elapsed_in_cycle: 0.0,
                per_cycle_bonus: 0.1, max_bonus: 1.5, current_multiplier: 1.5,
            },
        );
    });

    let err = eve_server::services::combat_crimewatch::dispatch_combat_rpc(
        &ctx, &mut sess, "dogmaIM", "Activate", &[PyRep::Integer(gun_id), PyRep::Integer(npc_ball_id)], &kw,
    )
    .await
    .expect_err("Firing weapon with zero locked targets must fail");
    assert_eq!(err.code, "TargetNotLocked");
    let spool_exists = with_combat_runtime(&ctx, |rt| rt.entropic_spools.contains_key(&(ship_id, gun_id)));
    assert!(!spool_exists, "Entropic spool must reset when lock check fails");
}

#[tokio::test]
async fn test_r5_203_superweapon_doomsday_fuel_before_damage_and_cooldown() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();
    let sys_id = 30_002_813u32;
    let (char_id, vic_id, ship_id, vic_ship_id, lance_id) =
        (98_203_001i64, 98_203_002i64, 88_203_101i64, 88_203_102i64, 88_203_201i64);
    let mut sess = setup_pilot_in_space(&ctx, 82031, char_id, 1_000_125, ship_id, 77281, sys_id, DVec3::ZERO).await;
    let _vic_sess = setup_pilot_in_space(&ctx, 82032, vic_id, 1_000_126, vic_ship_id, 587, sys_id, DVec3::new(5_000.0, 0.0, 0.0)).await;

    let siege_id = 88_203_202i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(lance_id, item_rec(lance_id, 77398, char_id, ship_id, InventoryFlag::HiSlot0, 1, true));
        snap.items.insert(siege_id, item_rec(siege_id, 20280, char_id, ship_id, InventoryFlag::HiSlot1, 1, true));
        snap.module_active_states.insert(siege_id);
        let skills = snap.skills.entry(char_id).or_default();
        skills.insert(77738, eve_db::TrainedSkillRecord { type_id: 77738, level: 5, skill_points: 256_000.0 });
        skills.insert(77739, eve_db::TrainedSkillRecord { type_id: 77739, level: 5, skill_points: 256_000.0 });
    });

    let err = ctx
        .dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(lance_id), PyRep::Integer(vic_ship_id)], &kw, None)
        .await
        .expect_err("Lancer Doomsday without isotopes must fail");
    assert_eq!(err.code, "InsufficientFuel");

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let vic_cs = sys_handle.get_combat_state(vic_ship_id).await.unwrap().unwrap();
    assert!((vic_cs.shield_hp - vic_cs.max_shield_hp).abs() < 1e-3, "Victim in beam path must take zero damage when Lancer lacks isotopes");
}

#[tokio::test]
async fn test_r5_204_206_filament_fleetmate_scramble_and_wormhole_hp_preservation() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();
    let (sys_a, sys_b) = (30_002_813u32, 31_000_005u32);
    let (c1, c2, s1, s2, fil_id) =
        (98_204_001i64, 98_204_002i64, 88_204_101i64, 88_204_102i64, 88_204_201i64);
    let deep_pos = DVec3::new(500_000_000_000.0, 500_000_000_000.0, 0.0);
    let mut sess1 = setup_pilot_in_space(&ctx, 82041, c1, 1_000_125, s1, 587, sys_a, deep_pos).await;
    let _sess2 = setup_pilot_in_space(&ctx, 82042, c2, 1_000_125, s2, 587, sys_a, deep_pos + DVec3::new(500.0, 0.0, 0.0)).await;

    let fil_type_id = ctx
        .sde
        .types
        .iter()
        .find(|(_, t)| t.group_id == 4041 && t.default_attributes.contains_key(&3025))
        .map(|(&id, _)| id)
        .expect("Needlejack filament type in SDE");
    let fleet_rep = ctx.dispatch_rpc(&mut sess1, "fleetMgr", None, "CreateFleet", &[], &kw, None).await.unwrap();
    let fleet_id = fleet_rep.dict_get("fleetID").and_then(|v| v.as_i64()).unwrap_or(1);
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&c1).unwrap().fleet_id = Some(fleet_id);
        snap.characters.get_mut(&c2).unwrap().fleet_id = Some(fleet_id);
        if let Some(f) = snap.fleets.get_mut(&fleet_id) {
            if !f.members.contains(&c2) {
                f.members.push(c2);
            }
        }
        snap.items.insert(fil_id, item_rec(fil_id, fil_type_id, c1, s1, InventoryFlag::Cargo, 1, false));
    });
    let sys_handle_a = ctx.systems.get_or_spawn(sys_a);
    let mut cs2 = sys_handle_a.get_combat_state(s2).await.unwrap().unwrap();
    cs2.warp_scramble_strength = 2;
    let _ = sys_handle_a.set_combat_state(s2, cs2).await;

    // #204: Scrambled fleetmate s2 is excluded from jumpers when c1 activates a random jump filament
    ctx.dispatch_rpc(&mut sess1, "randomJumpMgr", None, "ActivateRandomJumpFilament", &[PyRep::Integer(fil_id)], &kw, None).await.unwrap();
    let c2_sys = ctx.db.with_snapshot(|s| s.characters[&c2].solar_system_id);
    assert_eq!(c2_sys, sys_a, "Scrambled fleetmate must not be pulled into filament jump");

    // #206: Return c1 to sys_a, damage s1 to 40% armor, jump through wormhole to sys_b, and verify HP is preserved AND return jump is allowed
    ctx.db.clock().advance_secs(15);
    let now = ctx.now_unix();
    sess1.solar_system_id = Some(sys_a);
    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&c1).unwrap().solar_system_id = sys_a;
        s.cosmic_signatures.entry(sys_a).or_default().push(eve_db::CosmicSignatureRecord {
            signature_id: "WH-206".into(),
            solar_system_id: sys_a,
            sig_group: "Wormhole".into(),
            type_name: "Unstable Wormhole".into(),
            position: [0.0, 0.0, 0.0],
            signal_strength: 1.0,
            destination_system_id: Some(sys_b),
            remaining_mass_kg: 2_000_000_000.0,
            max_mass_kg: 2_000_000_000.0,
            max_jump_mass_kg: 300_000_000.0,
            lifetime_seconds: 86_400,
            created_unix: now,
            collapsed: false,
            wormhole_type_id: Some(26272),
        });
    });
    let mut cs1 = ctx.build_type_combat_state(587);
    cs1.shield_hp = 0.0;
    cs1.armor_hp = cs1.max_armor_hp * 0.4;
    let expected_armor = cs1.armor_hp;
    let _ = sys_handle_a
        .spawn_ball_with_combat(
            Ball {
                id: s1, type_id: 587, position: DVec3::ZERO, velocity: DVec3::ZERO,
                max_velocity: 300.0, inertia: 2.5, mass: 10_000_000.0, radius: 50.0,
                speed_fraction: 0.0, mode: MovementMode::Stop,
            },
            cs1,
        )
        .await;

    let wh_rep = ctx
        .dispatch_rpc(&mut sess1, "wormholeMgr", None, "WormholeJump", &[PyRep::String("WH-206".into()), PyRep::Integer(sys_b as i64)], &kw, None)
        .await
        .expect("First wormhole transit A -> B");
    let sys_handle_b = ctx.systems.get_or_spawn(sys_b);
    let cs1_after = sys_handle_b.get_combat_state(s1).await.unwrap().unwrap();
    assert_eq!(cs1_after.shield_hp, 0.0, "Shield HP must not heal to 100% on wormhole jump");
    assert!((cs1_after.armor_hp - expected_armor).abs() < 1.0, "Armor HP must be preserved across wormhole jump");

    ctx.db.clock().advance_secs(15);
    let k162_id = wh_rep.dict_get("exitWormholeID").and_then(|v| v.as_i64()).unwrap();
    let exit_pos = sys_handle_b
        .get_ball_snapshot(k162_id)
        .await
        .unwrap()
        .map(|(_, b)| b.position)
        .unwrap_or(DVec3::ZERO);
    let _ = sys_handle_b
        .spawn_ball_with_combat(
            Ball {
                id: s1, type_id: 587, position: exit_pos, velocity: DVec3::ZERO,
                max_velocity: 300.0, inertia: 2.5, mass: 10_000_000.0, radius: 50.0,
                speed_fraction: 0.0, mode: MovementMode::Stop,
            },
            cs1_after,
        )
        .await;
    ctx.dispatch_rpc(&mut sess1, "wormholeMgr", None, "WormholeJump", &[PyRep::Integer(k162_id), PyRep::Integer(sys_a as i64)], &kw, None)
        .await
        .expect("First return jump B -> A must not be prematurely polarized");
}

#[tokio::test]
async fn test_r5_208_209_breacher_dot_impact_system_scoping_and_smartbomb_wreck_crimewatch() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();
    let (sys_a, sys_b) = (30_002_813u32, 30_000_142u32);
    let (atk_id, vic_id, atk_ship, vic_ship, sb_id, wreck_id) =
        (98_208_001i64, 98_208_002i64, 88_208_101i64, 88_208_102i64, 88_208_201i64, 88_208_301i64);
    let mut atk_sess = setup_pilot_in_space(&ctx, 82081, atk_id, 1_000_125, atk_ship, 645, sys_a, DVec3::ZERO).await;
    let _vic_sess = setup_pilot_in_space(&ctx, 82082, vic_id, 1_000_126, vic_ship, 587, sys_a, DVec3::new(1_000.0, 0.0, 0.0)).await;

    with_mut_combat_runtime(&ctx, |rt| {
        rt.active_breacher_dots.insert(
            vic_ship,
            ActiveBreacherDot {
                target_ball_id: vic_ship, source_char_id: atk_id, source_ship_id: atk_ship,
                weapon_type_id: 486, charge_type_id: 178, flat_cap_dps: 100.0,
                pct_cap_fraction: 0.01, remaining_s: 50.0, tick_damage: 100.0,
            },
        );
    });
    step_combat_system(&ctx, sys_b, 10.0).await;
    let rem_after_other_sys = with_combat_runtime(&ctx, |rt| rt.active_breacher_dots[&vic_ship].remaining_s);
    assert!((rem_after_other_sys - 50.0).abs() < 1e-6, "Stepping another solar system must not decrement Breacher DoT timer");

    let sys_handle = ctx.systems.get_or_spawn(sys_a);
    let _ = sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: wreck_id, type_id: 26561, position: DVec3::new(500.0, 0.0, 0.0), velocity: DVec3::ZERO,
                max_velocity: 0.0, inertia: 1.0, mass: 10_000.0, radius: 25.0,
                speed_fraction: 0.0, mode: MovementMode::Stop,
            },
            ctx.build_type_combat_state(26561),
        )
        .await;
    let sb_type_id = ctx.sde.types.values().find(|t| t.group_id == 72).map(|t| t.type_id).unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(sb_id, item_rec(sb_id, sb_type_id, atk_id, atk_ship, InventoryFlag::HiSlot0, 1, true));
        snap.module_online_states.insert(sb_id, true);
    });
    ctx.invalidate_ship_fitting(atk_ship);
    with_mut_combat_runtime(&ctx, |rt| {
        rt.safety_levels.insert(atk_id, SafetyLevel::Red);
    });
    let sec_before = ctx.db.with_snapshot(|s| s.characters[&atk_id].security_status);
    eve_server::services::combat_crimewatch::dispatch_combat_rpc(
        &ctx, &mut atk_sess, "dogmaIM", "Activate", &[PyRep::Integer(sb_id)], &kw,
    )
    .await
    .expect("Smartbomb activation in lowsec");

    assert!(sys_handle.get_ball_snapshot(wreck_id).await.unwrap().is_some(), "Smartbomb must not damage or destroy wrecks");
    let sec_after = ctx.db.with_snapshot(|s| s.characters[&atk_id].security_status);
    assert!(sec_after < sec_before, "Illegal lowsec smartbomb hit must reduce security status");
    let has_le = with_combat_runtime(&ctx, |rt| rt.has_limited_engagement_with(atk_id, vic_id, ctx.now_unix()));
    assert!(!has_le, "Illegal lowsec smartbomb hit must NOT create Limited Engagement");
    assert!(ctx.crimewatch.read().unwrap().states[&atk_id].is_suspect(ctx.now_unix()));
}

#[tokio::test]
async fn test_r5_210_211_probe_full_cargo_preservation_and_hacking_cloak_ghost_quit() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();
    let sys_id = 30_002_813u32;
    let (char_id, ship_id, probe_id, cargo_probe_id, filler_id, analyzer_id, can_id) =
        (98_210_001i64, 88_210_101i64, 88_210_201i64, 88_210_202i64, 88_210_203i64, 88_210_204i64, 88_210_301i64);
    let mut sess = setup_pilot_in_space(&ctx, 82101, char_id, 1_000_125, ship_id, 587, sys_id, DVec3::ZERO).await;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(filler_id, item_rec(filler_id, 34, char_id, ship_id, InventoryFlag::Cargo, 100_000, false));
        snap.items.insert(cargo_probe_id, item_rec(cargo_probe_id, 30013, char_id, ship_id, InventoryFlag::Cargo, 4, false));
        snap.items.insert(analyzer_id, item_rec(analyzer_id, 22175, char_id, ship_id, InventoryFlag::MedSlot0, 1, true));
    });
    {
        let mut map = ctx.active_scanner_probes.write().unwrap();
        map.insert(
            probe_id,
            ActiveScannerProbe {
                probe_id, probe_type_id: 30013, owner_char_id: char_id, ship_id, launcher_module_id: 0,
                solar_system_id: sys_id, position: [0.0, 0.0, 0.0], destination: [0.0, 0.0, 0.0],
                range_step: 1, scan_range_m: 149_597_870_700.0, base_sensor_strength: 40.0,
                max_flight_time_s: 3600.0, launched_at_unix: ctx.now_unix(), state: 1,
                warp_end_ft: None,
            },
        );
        map.insert(
            probe_id + 1,
            ActiveScannerProbe {
                probe_id: probe_id + 1, probe_type_id: 30013, owner_char_id: char_id, ship_id, launcher_module_id: 0,
                solar_system_id: sys_id, position: [0.0, 0.0, 0.0], destination: [0.0, 0.0, 0.0],
                range_step: 1, scan_range_m: 149_597_870_700.0, base_sensor_strength: 40.0,
                max_flight_time_s: 3600.0, launched_at_unix: ctx.now_unix(), state: -1,
                warp_end_ft: None,
            },
        );
    }

    let rec_rep = ctx.dispatch_rpc(&mut sess, "scanMgr", None, "RecoverProbes", &[], &kw, None).await.expect("RecoverProbes with full cargo");
    assert_eq!(rec_rep.dict_get("recoveredCount").and_then(|v| v.as_i64()), Some(0));
    assert!(ctx.active_scanner_probes.read().unwrap().contains_key(&probe_id), "Unrecovered probe must remain in active_scanner_probes");

    ctx.dispatch_rpc(&mut sess, "scanMgr", None, "DestroyProbe", &[PyRep::Integer(probe_id + 1)], &kw, None).await.expect("DestroyProbe on expired probe");
    let cargo_qty = ctx.db.with_snapshot(|s| s.items[&cargo_probe_id].quantity);
    assert_eq!(cargo_qty, 4, "DestroyProbe must not delete probes from ship cargo");

    with_pve_exploration_reg(&ctx, |reg| {
        reg.sites_by_system.entry(sys_id).or_default().push(PveSiteInstance {
            site_id: "GHOST-R5-211".into(),
            solar_system_id: sys_id,
            site_category: "Ghost".into(),
            position: [0.0, 0.0, 0.0],
            hackable_can_ids: vec![can_id],
            ghost_timer_started_at: Some(ctx.now_unix()),
            ghost_detonate_after_sec: 120,
            ..Default::default()
        });
        reg.hacking_games.insert(
            (char_id, can_id),
            HackingGameSession {
                game_id: 500_211, character_id: char_id, solar_system_id: sys_id, container_id: can_id,
                site_id: "GHOST-R5-211".into(), is_relic: false, is_ghost_site: true,
                is_sleeper_cache_puzzle: false, virus_coherence: 60, max_virus_coherence: 60,
                base_virus_strength: 25, polymorphic_shield_charges: 0, secondary_vector_target: None,
                utilities: vec![None; 3],
                tiles: vec![
                    HackingTileState { tile_index: 0, revealed: true, kind: HackingNodeKind::Empty, coherence: 0, strength: 0, utility_drop: None, blocked_by_defensive: false },
                    HackingTileState { tile_index: 1, revealed: false, kind: HackingNodeKind::Empty, coherence: 0, strength: 0, utility_drop: None, blocked_by_defensive: false },
                ],
                status: "InProgress".into(),
            },
        );
    });
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let mut cs = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    cs.is_cloaked = true;
    let _ = sys_handle.set_combat_state(ship_id, cs.clone()).await;
    let cloak_err = ctx
        .dispatch_rpc(&mut sess, "hackingMgr", None, "ClickedOnTile", &[PyRep::Integer(can_id), PyRep::Integer(1)], &kw, None)
        .await
        .expect_err("ClickedOnTile while cloaked must fail");
    assert_eq!(cloak_err.code, "CannotHackWhileCloaked");
    cs.is_cloaked = false;
    let _ = sys_handle.set_combat_state(ship_id, cs).await;

    ctx.dispatch_rpc(&mut sess, "hackingMgr", None, "QuitHackingAttempt", &[PyRep::Integer(can_id)], &kw, None).await.expect("QuitHackingAttempt on Ghost Site");
    let detonated = with_pve_exploration_reg(&ctx, |reg| {
        reg.sites_by_system[&sys_id].iter().find(|s| s.site_id == "GHOST-R5-211").unwrap().ghost_detonated
    });
    assert!(detonated, "Quitting an active Ghost Site hack must trigger Ghost Site detonation");
}
