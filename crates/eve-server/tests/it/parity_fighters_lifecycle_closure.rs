//! Parity remediation test suite for:
//! - Heavy fighter point-targeted AoE bomb ability (`feat:drones.fighter-heavy-long-range-bomb`)
//! - Fighter tube refueling, rearming, and HP repair cycles (`feat:drones.fighter-refuel-rearm-repair`)
//! - Support fighters EWAR application and scaling (`feat:drones.fighter-support-cenotaph-scarab`, `feat:drones.support-fighter-abilities`)
//! - Space superiority light fighters anti-fighter bonus & Evasive Maneuvers (`feat:drones.light-fighter-space-superiority`)
//! - Industry pause timing and CompleteManyJobs (`feat:lifecycle.industry-pause-and-many`)
//! - Quitting corporation full cleanup and bloodline NPC assignment (`feat:lifecycle.corp-quit-cleanup`)

use crate::open_feats_b35::give_item;
use eve_db::{
    corp_roles, ChatChannelRecord, CorporationRecord, InventoryFlag, IskCents,
    UpwellStructureRecord,
};
use eve_destiny::{Ball, DVec3, MovementMode};
use eve_machonet::{service_roles, SessionState};
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::context::ActiveFighterSquadron;
use eve_server::router::ServerContext;
use eve_server::services::combat_crimewatch::with_combat_runtime;
use eve_server::services::industry_pi::*;
use std::collections::{HashMap, HashSet};

const NULLSEC_SYS_ID: u32 = 30_004_759; // 1DQ1-A (-0.38 nullsec)
const NYX_SUPERCARRIER_TYPE_ID: u32 = 23913;
const ARCHON_CARRIER_TYPE_ID: u32 = 23757;
const TARGET_CRUISER_TYPE_ID: u32 = 626; // Vexor
const AMETAT_HEAVY_FIGHTER_TYPE_ID: u32 = 40362;
const SIREN_SUPPORT_FIGHTER_TYPE_ID: u32 = 40346;
const DROMI_SUPPORT_FIGHTER_TYPE_ID: u32 = 40347;
const SCARAB_SUPPORT_FIGHTER_TYPE_ID: u32 = 40345;
const EQUITE_LIGHT_FIGHTER_TYPE_ID: u32 = 40358;

fn make_session(char_id: i64, ship_id: i64, sys_id: u32, corp_id: i64) -> SessionState {
    let mut s = SessionState::new(char_id as u64);
    s.user_id = Some((char_id as u64).wrapping_add(10_000));
    s.character_id = Some(char_id);
    s.corporation_id = Some(corp_id);
    s.solar_system_id = Some(sys_id);
    s.constellation_id = Some(20_000_020);
    s.region_id = Some(10_000_002);
    s.ship_id = Some(ship_id);
    s.role = service_roles::ROLE_PLAYER | service_roles::ROLE_LOGIN;
    s
}

async fn rpc(
    ctx: &ServerContext, sess: &mut SessionState, service: &str, method: &str, args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None).await
}

async fn setup_ship_ball(ctx: &ServerContext, ship_id: i64, type_id: u32, char_id: i64, sys_id: u32, pos: DVec3) {
    give_item(ctx, ship_id, type_id, char_id, sys_id as i64, InventoryFlag::None, 1);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(base) = snap.characters.get(&90_000_001).cloned() {
            let mut ch = base;
            ch.character_id = char_id;
            ch.solar_system_id = sys_id;
            ch.active_ship_id = ship_id;
            snap.characters.insert(char_id, ch);
        }
    });
    let sys = ctx.systems.get_or_spawn(sys_id);
    let mut cs = ctx.build_type_combat_state(type_id);
    if cs.max_shield_hp <= 0.0 {
        cs.max_shield_hp = 10_000.0; cs.shield_hp = 10_000.0;
        cs.max_armor_hp = 10_000.0; cs.armor_hp = 10_000.0;
        cs.max_structure_hp = 10_000.0; cs.structure_hp = 10_000.0;
    }
    cs.capacitor = 100_000.0; cs.max_capacitor = 100_000.0;
    let _ = sys.spawn_ball_with_combat(Ball {
        id: ship_id, type_id, position: pos, velocity: DVec3::ZERO,
        max_velocity: 200.0, inertia: 1.0, mass: 10_000_000.0, radius: 150.0,
        speed_fraction: 0.0, mode: MovementMode::Stop,
    }, cs).await;
}

#[tokio::test]
async fn test_parity_fighters_heavy_bomb_and_tube_rearm_repair() {
    // parity: feat:drones.fighter-heavy-long-range-bomb
    // parity: feat:drones.fighter-refuel-rearm-repair
    eve_testkit::attest(&["feat:drones.fighter-heavy-long-range-bomb", "feat:drones.fighter-refuel-rearm-repair"]);

    let ctx = crate::modules_advanced_tactical::build_ctx();
    let carrier_char_id = 92_100_001i64;
    let carrier_ship_id = 92_100_002i64;
    let target_char_id = 92_100_003i64;
    let target_ship_id = 92_100_004i64;
    let sys_id = NULLSEC_SYS_ID;

    setup_ship_ball(&ctx, carrier_ship_id, NYX_SUPERCARRIER_TYPE_ID, carrier_char_id, sys_id, DVec3::ZERO).await;
    setup_ship_ball(&ctx, target_ship_id, TARGET_CRUISER_TYPE_ID, target_char_id, sys_id, DVec3::new(25_000.0, 0.0, 0.0)).await;

    let mut sess = make_session(carrier_char_id, carrier_ship_id, sys_id, 1_000_009);
    let heavy_item_id = 92_100_101i64;
    let reserve_item_id = 92_100_102i64;
    give_item(&ctx, heavy_item_id, AMETAT_HEAVY_FIGHTER_TYPE_ID, carrier_char_id, carrier_ship_id, InventoryFlag::FighterBay, 6);
    give_item(&ctx, reserve_item_id, AMETAT_HEAVY_FIGHTER_TYPE_ID, carrier_char_id, carrier_ship_id, InventoryFlag::FighterBay, 5);

    rpc(&ctx, &mut sess, "fighterMgr", "LoadFightersToTube", &[PyRep::Integer(0), PyRep::Integer(heavy_item_id)])
        .await.expect("Load Ametat heavy fighter to tube 0 on Nyx Supercarrier");

    let launch_rep = rpc(&ctx, &mut sess, "fighterMgr", "LaunchFightersFromTubes", &[PyRep::List(vec![PyRep::Integer(0)])])
        .await.expect("Launch heavy fighter squadron");
    let PyRep::List(launched) = launch_rep else { panic!("Expected List of launched fighters"); };
    let heavy_ball_id = launched[0].as_i64().expect("fighter ball id");

    let sys = ctx.systems.get_or_spawn(sys_id);
    let target_before = sys.get_combat_state(target_ship_id).await.unwrap().unwrap().shield_hp;

    let bomb_target_pt = PyRep::Tuple(vec![PyRep::Real(25_000.0), PyRep::Real(0.0), PyRep::Real(0.0)]);
    let bomb_rep = rpc(&ctx, &mut sess, "fighterMgr", "ActivateAbilitySlotsAtPoint", &[PyRep::Integer(heavy_ball_id), PyRep::Integer(2), bomb_target_pt.clone()])
        .await.expect("ActivateAbilitySlotsAtPoint bomb detonation");
    assert_eq!(bomb_rep.dict_get("remainingCharges").and_then(|v| v.as_i64()).unwrap(), 2, "1st bomb activation leaves 2 charges");

    let target_after = sys.get_combat_state(target_ship_id).await.unwrap().unwrap().shield_hp;
    assert!(target_after < target_before, "AoE bomb must deal shield damage at target coordinate");

    let cd_err = rpc(&ctx, &mut sess, "fighterMgr", "ActivateAbilitySlotsAtPoint", &[PyRep::Integer(heavy_ball_id), PyRep::Integer(2), bomb_target_pt.clone()])
        .await.expect_err("Immediate reactivation must be blocked on cooldown");
    assert_eq!(cd_err.code, "AbilityOnCooldown");

    ctx.advance_secs(95);
    let b2 = rpc(&ctx, &mut sess, "fighterMgr", "ActivateAbilitySlotsAtPoint", &[PyRep::Integer(heavy_ball_id), PyRep::Integer(2), bomb_target_pt.clone()]).await.expect("2nd bomb");
    assert_eq!(b2.dict_get("remainingCharges").and_then(|v| v.as_i64()), Some(1));

    ctx.advance_secs(95);
    let b3 = rpc(&ctx, &mut sess, "fighterMgr", "ActivateAbilitySlotsAtPoint", &[PyRep::Integer(heavy_ball_id), PyRep::Integer(2), bomb_target_pt.clone()]).await.expect("3rd bomb");
    assert_eq!(b3.dict_get("remainingCharges").and_then(|v| v.as_i64()), Some(0));

    ctx.advance_secs(95);
    let ooc_err = rpc(&ctx, &mut sess, "fighterMgr", "ActivateAbilitySlotsAtPoint", &[PyRep::Integer(heavy_ball_id), PyRep::Integer(2), bomb_target_pt])
        .await.expect_err("4th bomb activation with 0 charges must fail");
    assert_eq!(ooc_err.code, "OutOfCharges");

    ctx.active_fighters.write().unwrap().get_mut(&heavy_ball_id).unwrap().squadron_size = 5;
    eve_server::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| {
        rt.fighter_unit_damage_accum.insert(heavy_ball_id, 450.0);
    });

    let recall_rep = rpc(&ctx, &mut sess, "fighterMgr", "RecallFightersToTubes", &[PyRep::List(vec![PyRep::Integer(heavy_ball_id)])]).await.expect("RecallFightersToTubes");
    assert_eq!(recall_rep, PyRep::List(vec![PyRep::Integer(heavy_ball_id)]));

    let relaunch_err = rpc(&ctx, &mut sess, "fighterMgr", "LaunchFightersFromTubes", &[PyRep::List(vec![PyRep::Integer(0)])])
        .await.expect_err("Relaunch during rearm/refuel cycle must be blocked");
    assert_eq!(relaunch_err.code, "TubeRearming");

    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items.get(&heavy_item_id).unwrap().quantity, 6, "Destroyed fighter refilled from reserve in FighterBay");
        assert_eq!(snap.items.get(&reserve_item_id).unwrap().quantity, 4, "Reserve stack in FighterBay decremented by 1");
    });
    assert!(with_combat_runtime(&ctx, |rt| !rt.fighter_unit_damage_accum.contains_key(&heavy_ball_id)), "Unit damage accumulator cleared on recall");

    ctx.advance_secs(20);
    let relaunch_rep = rpc(&ctx, &mut sess, "fighterMgr", "LaunchFightersFromTubes", &[PyRep::List(vec![PyRep::Integer(0)])]).await.expect("Relaunch after rearm");
    let PyRep::List(relaunched) = relaunch_rep else { panic!("Expected list") };
    let new_ball_id = relaunched[0].as_i64().unwrap();
    assert_eq!(ctx.active_fighters.read().unwrap().get(&new_ball_id).unwrap().squadron_size, 6, "Relaunched squadron has full size");
}

#[tokio::test]
async fn test_parity_fighters_support_and_space_superiority() {
    // parity: feat:drones.fighter-support-cenotaph-scarab
    // parity: feat:drones.support-fighter-abilities
    // parity: feat:drones.light-fighter-space-superiority
    eve_testkit::attest(&[
        "feat:drones.fighter-support-cenotaph-scarab",
        "feat:drones.support-fighter-abilities",
        "feat:drones.light-fighter-space-superiority",
    ]);

    let ctx = crate::modules_advanced_tactical::build_ctx();
    let carrier_char_id = 92_200_001i64;
    let carrier_ship_id = 92_200_002i64;
    let target_char_id = 92_200_003i64;
    let target_ship_id = 92_200_004i64;
    let sys_id = 30_004_759;

    setup_ship_ball(&ctx, carrier_ship_id, ARCHON_CARRIER_TYPE_ID, carrier_char_id, sys_id, DVec3::ZERO).await;
    setup_ship_ball(&ctx, target_ship_id, TARGET_CRUISER_TYPE_ID, target_char_id, sys_id, DVec3::new(6_000.0, 0.0, 0.0)).await;

    let mut sess = make_session(carrier_char_id, carrier_ship_id, sys_id, 1_000_009);
    let (s_iid, d_iid, sc_iid, eq_iid) = (92_200_101i64, 92_200_102i64, 92_200_103i64, 92_200_104i64);
    give_item(&ctx, s_iid, SIREN_SUPPORT_FIGHTER_TYPE_ID, carrier_char_id, carrier_ship_id, InventoryFlag::FighterBay, 3);
    give_item(&ctx, d_iid, DROMI_SUPPORT_FIGHTER_TYPE_ID, carrier_char_id, carrier_ship_id, InventoryFlag::FighterBay, 3);
    give_item(&ctx, sc_iid, SCARAB_SUPPORT_FIGHTER_TYPE_ID, carrier_char_id, carrier_ship_id, InventoryFlag::FighterBay, 3);
    give_item(&ctx, eq_iid, EQUITE_LIGHT_FIGHTER_TYPE_ID, carrier_char_id, carrier_ship_id, InventoryFlag::FighterBay, 9);

    for (tidx, iid) in [(0, s_iid), (1, d_iid), (2, eq_iid)] {
        rpc(&ctx, &mut sess, "fighterMgr", "LoadFightersToTube", &[PyRep::Integer(tidx), PyRep::Integer(iid)]).await.unwrap();
    }

    let launched = rpc(&ctx, &mut sess, "fighterMgr", "LaunchFightersFromTubes", &[PyRep::List(vec![
        PyRep::Integer(0), PyRep::Integer(1), PyRep::Integer(2),
    ])]).await.expect("Launch 3 squadrons");
    let PyRep::List(balls) = launched else { panic!("Expected list") };
    let (siren_ball, dromi_ball, equite_ball) = (balls[0].as_i64().unwrap(), balls[1].as_i64().unwrap(), balls[2].as_i64().unwrap());

    let sys = ctx.systems.get_or_spawn(sys_id);
    let _ = sys.add_target_lock(carrier_ship_id, eve_destiny::TargetLock { target_id: target_ship_id, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(100_000.0) }).await;

    // 1. Siren Warp Disruption scaled by squadron size
    rpc(&ctx, &mut sess, "fighterMgr", "CmdActivateAbilitySlots", &[PyRep::Integer(siren_ball), PyRep::Integer(0), PyRep::Integer(target_ship_id)]).await.unwrap();
    assert_eq!(sys.get_combat_state(target_ship_id).await.unwrap().unwrap().warp_scramble_strength, 3, "Siren full squad (3) applies 3 scram points");

    ctx.advance_secs(12);
    ctx.active_fighters.write().unwrap().get_mut(&siren_ball).unwrap().squadron_size = 1;
    rpc(&ctx, &mut sess, "fighterMgr", "CmdActivateAbilitySlots", &[PyRep::Integer(siren_ball), PyRep::Integer(0), PyRep::Integer(target_ship_id)]).await.unwrap();
    assert_eq!(sys.get_combat_state(target_ship_id).await.unwrap().unwrap().warp_scramble_strength, 1, "Siren scaled down to 1 point at size 1");

    // 2. Dromi Stasis Webification
    let _ = sys.set_speed_fraction(target_ship_id, 1.0).await;
    rpc(&ctx, &mut sess, "fighterMgr", "CmdActivateAbilitySlots", &[PyRep::Integer(dromi_ball), PyRep::Integer(0), PyRep::Integer(target_ship_id)]).await.unwrap();
    assert!(sys.get_ball_snapshot(target_ship_id).await.unwrap().unwrap().1.speed_fraction < 1.0, "Dromi web reduces target speed");

    // 3. Scarab ECM Jamming: recall Dromi from tube 1, unload tube 1 to FighterBay, and load Scarab into tube 1
    rpc(&ctx, &mut sess, "fighterMgr", "RecallFightersToTubes", &[PyRep::List(vec![PyRep::Integer(dromi_ball)])]).await.unwrap();
    ctx.advance_secs(20);
    rpc(&ctx, &mut sess, "fighterMgr", "UnloadTubeToFighterBay", &[PyRep::Integer(1)]).await.unwrap();
    rpc(&ctx, &mut sess, "fighterMgr", "LoadFightersToTube", &[PyRep::Integer(1), PyRep::Integer(sc_iid)]).await.unwrap();
    let sc_launch = rpc(&ctx, &mut sess, "fighterMgr", "LaunchFightersFromTubes", &[PyRep::List(vec![PyRep::Integer(1)])]).await.unwrap();
    let PyRep::List(sc_balls) = sc_launch else { panic!("Expected list") };
    let scarab_ball = sc_balls[0].as_i64().unwrap();

    let _ = sys.add_target_lock(target_ship_id, eve_destiny::TargetLock { target_id: carrier_ship_id, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(100_000.0) }).await;
    assert!(sys.get_target_locks(target_ship_id).await.unwrap().iter().any(|l| l.target_id == carrier_ship_id && l.locked));

    rpc(&ctx, &mut sess, "fighterMgr", "CmdActivateAbilitySlots", &[PyRep::Integer(scarab_ball), PyRep::Integer(0), PyRep::Integer(target_ship_id)]).await.unwrap();
    let locks_after = sys.get_target_locks(target_ship_id).await.unwrap();
    assert!(locks_after.is_empty() || !locks_after.iter().any(|l| l.target_id == carrier_ship_id && l.locked), "Scarab ECM clears lock on carrier");

    // 4. Equite Space Superiority: Evasive Maneuvers and anti-fighter missile volley
    let orig_sig = sys.get_combat_state(equite_ball).await.unwrap().unwrap().signature_radius;
    let orig_vel = sys.get_ball_snapshot(equite_ball).await.unwrap().unwrap().1.max_velocity;

    rpc(&ctx, &mut sess, "fighterMgr", "CmdActivateAbilitySlots", &[PyRep::Integer(equite_ball), PyRep::Integer(1), PyRep::None]).await.unwrap();
    let evasive_cs = sys.get_combat_state(equite_ball).await.unwrap().unwrap();
    let evasive_vel = sys.get_ball_snapshot(equite_ball).await.unwrap().unwrap().1.max_velocity;
    assert!(evasive_vel > orig_vel, "Evasive Maneuvers increases velocity");
    assert!(evasive_cs.signature_radius < orig_sig, "Evasive Maneuvers reduces signature radius");

    let cd_err = rpc(&ctx, &mut sess, "fighterMgr", "CmdActivateAbilitySlots", &[PyRep::Integer(equite_ball), PyRep::Integer(1), PyRep::None]).await.unwrap_err();
    assert_eq!(cd_err.code, "AbilityOnCooldown");

    rpc(&ctx, &mut sess, "fighterMgr", "CmdDeactivateAbilitySlots", &[PyRep::Integer(equite_ball), PyRep::Integer(1)]).await.unwrap();
    assert_eq!(sys.get_combat_state(equite_ball).await.unwrap().unwrap().signature_radius, orig_sig, "Deactivation restores sig radius");

    // Anti-fighter missile volley vs enemy fighter
    let enemy_fighter_ball = 92_200_999i64;
    ctx.active_fighters.write().unwrap().insert(enemy_fighter_ball, ActiveFighterSquadron {
        ball_id: enemy_fighter_ball, parent_ship_id: target_ship_id, owner_char_id: target_char_id,
        tube_index: 0, type_id: EQUITE_LIGHT_FIGHTER_TYPE_ID, item_id: 92_200_998,
        squadron_size: 9, target_id: None, mwd_active: false, is_abandoned: false,
    });
    let mut ef_cs = ctx.build_type_combat_state(EQUITE_LIGHT_FIGHTER_TYPE_ID);
    ef_cs.shield_hp = 10_000.0; ef_cs.max_shield_hp = 10_000.0;
    ef_cs.armor_hp = 10_000.0; ef_cs.max_armor_hp = 10_000.0;
    ef_cs.structure_hp = 10_000.0; ef_cs.max_structure_hp = 10_000.0;
    let _ = sys.spawn_ball_with_combat(Ball {
        id: enemy_fighter_ball, type_id: EQUITE_LIGHT_FIGHTER_TYPE_ID, position: DVec3::new(4_000.0, 0.0, 0.0),
        velocity: DVec3::ZERO, max_velocity: 1000.0, inertia: 1.0, mass: 10_000.0, radius: 35.0,
        speed_fraction: 0.0, mode: MovementMode::Stop,
    }, ef_cs).await;
    let _ = sys.add_target_lock(carrier_ship_id, eve_destiny::TargetLock { target_id: enemy_fighter_ball, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(100_000.0) }).await;

    let ef_before = sys.get_combat_state(enemy_fighter_ball).await.unwrap().unwrap().shield_hp;
    rpc(&ctx, &mut sess, "fighterMgr", "CmdActivateAbilitySlots", &[PyRep::Integer(equite_ball), PyRep::Integer(2), PyRep::Integer(enemy_fighter_ball)]).await.unwrap();
    let ef_after = sys.get_combat_state(enemy_fighter_ball).await.unwrap().unwrap().shield_hp;
    assert!(ef_after < ef_before, "Anti-fighter missile volley deals damage to enemy fighter");
}

#[tokio::test]
async fn test_parity_lifecycle_industry_and_corp_quit() {
    // parity: feat:lifecycle.industry-pause-and-many
    // parity: feat:lifecycle.corp-quit-cleanup
    eve_testkit::attest(&["feat:lifecycle.industry-pause-and-many", "feat:lifecycle.corp-quit-cleanup"]);

    let ctx = crate::modules_advanced_tactical::build_ctx();
    let pilot_char_id = 92_300_001i64;
    let player_corp_id = 98_000_101i64;
    let struct_id = 92_300_500i64;
    let sys_id = 30_000_142u32; // Jita

    ctx.db.with_mut_snapshot(|s| {
        if let Some(base) = s.characters.get(&90_000_001).cloned() {
            let mut ch = base;
            ch.character_id = pilot_char_id;
            ch.corporation_id = player_corp_id;
            ch.bloodline_id = 2; // Civire bloodline -> default NPC corp 1000009
            ch.solar_system_id = sys_id;
            ch.structure_id = Some(struct_id);
            ch.active_ship_id = 92_300_002;
            s.characters.insert(pilot_char_id, ch);
        }
        if let Some(s_map) = s.skills.get(&90_000_001).cloned() {
            s.skills.insert(pilot_char_id, s_map);
        }
        s.skills.entry(pilot_char_id).or_default().insert(3380, eve_db::TrainedSkillRecord {
            type_id: 3380, skill_points: 250_000.0, level: 5,
        });
        s.skills.entry(pilot_char_id).or_default().insert(3387, eve_db::TrainedSkillRecord {
            type_id: 3387, skill_points: 250_000.0, level: 5,
        });

        let mut members = HashSet::new();
        members.insert(pilot_char_id); members.insert(90_000_002i64);
        let mut member_roles = HashMap::new();
        member_roles.insert(pilot_char_id, corp_roles::ROLE_FACTORY_MANAGER);
        s.corporations.insert(player_corp_id, CorporationRecord {
            corporation_id: player_corp_id, name: "Frontier Industrial LLC".into(), ticker: "FI-LLC".into(),
            ceo_id: 90_000_002, alliance_id: None, tax_rate: 0.05, master_wallet: IskCents(100_000_000_00),
            members, member_roles, applications: HashMap::new(), wallet_divisions: Vec::new(),
            description: String::new(), url: String::new(), hq_station_id: None,
        });

        let gov = s.corp_governance.entry(player_corp_id).or_default();
        gov.member_titles.insert(pilot_char_id, 1);
        gov.member_account_keys.insert(pilot_char_id, 1000);
        gov.pending_role_grants.insert(pilot_char_id, (corp_roles::ROLE_DIRECTOR, 9999999999));
        gov.shares.insert(pilot_char_id, 250);
        gov.offices.insert(sys_id as i64, 92_300_888);

        s.chat_channels.insert(
            format!("corpid:{player_corp_id}"),
            ChatChannelRecord {
                channel_id: format!("corpid:{player_corp_id}"),
                name: "Corp Chat".into(),
                motd: String::new(),
                owner_id: None,
                members: [pilot_char_id].into_iter().collect(),
                is_system_local: false,
                solar_system_id: None,
                is_private: false,
                operators: HashSet::new(),
                password: None,
                is_delayed_local: false,
            },
        );

        s.structures.insert(
            struct_id,
            UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 35825, // Raitaru
                owner_corp_id: player_corp_id,
                solar_system_id: sys_id,
                name: "Test Raitaru".into(),
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 5_000,
                shield_hp: 10000.0,
                armor_hp: 10000.0,
                hull_hp: 10000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
    });

    let s_mod_id = struct_id + 1;
    give_item(&ctx, s_mod_id, 35878, player_corp_id, struct_id, InventoryFlag::ServiceSlot0, 1);
    ctx.db.with_mut_snapshot(|s| {
        if let Some(it) = s.items.get_mut(&s_mod_id) {
            it.dynamic_attributes.insert(2, 1.0);
        }
        s.module_online_states.insert(s_mod_id, true);
    });

    for bp_id in [92_300_101i64, 92_300_102i64, 92_300_103i64] {
        give_item(&ctx, bp_id, 681, pilot_char_id, struct_id, InventoryFlag::Hangar, 1);
    }
    for (idx, m_tid) in (34_u32..=40_u32).enumerate() {
        give_item(&ctx, 92_300_200 + idx as i64, m_tid, pilot_char_id, struct_id, InventoryFlag::Hangar, 50_000);
    }

    let mut sess = make_session(pilot_char_id, 92_300_002, sys_id, player_corp_id);
    sess.structure_id = Some(struct_id);

    // 1. Install Job 1 (1 run, 50s) and Job 2 (5 runs, 250s) via InstallJob
    let j1_res = rpc(&ctx, &mut sess, "industryManager", "InstallJob", &[PyRep::keyval([
        ("blueprintID", PyRep::Integer(92_300_101)), ("facilityID", PyRep::Integer(struct_id)),
        ("activityID", PyRep::Integer(1)), ("runs", PyRep::Integer(1)),
    ])]).await.expect("InstallJob 1");
    let job1_id = j1_res.dict_get("jobID").and_then(|v| v.as_i64()).unwrap();

    let j2_res = rpc(&ctx, &mut sess, "industryManager", "InstallJob", &[PyRep::keyval([
        ("blueprintID", PyRep::Integer(92_300_102)), ("facilityID", PyRep::Integer(struct_id)),
        ("activityID", PyRep::Integer(1)), ("runs", PyRep::Integer(5)),
    ])]).await.expect("InstallJob 2");
    let job2_id = j2_res.dict_get("jobID").and_then(|v| v.as_i64()).unwrap();

    ctx.advance_secs(500);
    let offline_moment = ctx.now_unix();

    // Facility loses service / goes offline
    ctx.db.with_mut_snapshot(|s| {
        let mut ext = eve_server::services::station_structure::structure_upwell_ops::load_ext_state(s);
        ext.lifecycle.entry(struct_id).or_default().low_power_since_unix = Some(offline_moment);
        eve_server::services::station_structure::structure_upwell_ops::save_ext_state(s, &ext);
        s.module_online_states.insert(struct_id + 1, false);
    });

    ctx.advance_secs(120);
    ctx.db.with_mut_snapshot(|snap| sync_industry_job_pause_states(snap, ctx.now_unix()));

    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.industry_jobs.get(&job1_id).unwrap().status, "Active", "Job finished before offline is not paused");
        assert_eq!(snap.industry_jobs.get(&job2_id).unwrap().status, "Paused", "Active job is paused on offline");
        let meta = load_job_cost_meta(snap, job2_id).unwrap();
        assert_eq!(meta.paused_at_unix, Some(offline_moment), "Pause starts at offline moment, not first read");
    });

    let pause_err = rpc(&ctx, &mut sess, "industryManager", "CompleteJob", &[PyRep::Integer(job2_id)]).await.expect_err("Delivering paused job fails");
    assert_eq!(pause_err.code, "FacilityServiceOffline");

    // Online facility service again
    ctx.db.with_mut_snapshot(|s| {
        let mut ext = eve_server::services::station_structure::structure_upwell_ops::load_ext_state(s);
        ext.lifecycle.entry(struct_id).or_default().low_power_since_unix = None;
        eve_server::services::station_structure::structure_upwell_ops::save_ext_state(s, &ext);
        s.module_online_states.insert(struct_id + 1, true);
        sync_industry_job_pause_states(s, ctx.now_unix());
    });
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.industry_jobs.get(&job2_id).unwrap().status, "Active", "Job resumes Active without progress loss");
    });

    ctx.advance_secs(2000);
    let complete_many = rpc(&ctx, &mut sess, "industryManager", "CompleteManyJobs", &[PyRep::List(vec![
        PyRep::Integer(job1_id), PyRep::Integer(job2_id),
    ])]).await.expect("CompleteManyJobs delivers ready jobs");
    let PyRep::List(delivered_kvs) = complete_many else { panic!("Expected list") };
    assert_eq!(delivered_kvs.len(), 2, "CompleteManyJobs delivers both ready jobs");

    // Ex-members cannot cancel or deliver corp jobs: install corp job
    let corp_job_res = rpc(&ctx, &mut sess, "industryManager", "InstallJob", &[PyRep::keyval([
        ("blueprintID", PyRep::Integer(92_300_103)), ("facilityID", PyRep::Integer(struct_id)),
        ("activityID", PyRep::Integer(1)), ("runs", PyRep::Integer(1)), ("useCorp", PyRep::Bool(true)),
    ])]).await.expect("Install corp job");
    let corp_job_id = corp_job_res.dict_get("jobID").and_then(|v| v.as_i64()).unwrap();

    // 2. Corp quit cleanup (`feat:lifecycle.corp-quit-cleanup`)
    let quit_rep = rpc(&ctx, &mut sess, "corpRegistry", "KickOutMember", &[PyRep::Integer(pilot_char_id)]).await.expect("KickOutMember");
    assert_eq!(quit_rep.dict_get("removed"), Some(&PyRep::Bool(true)));

    ctx.db.with_snapshot(|snap| {
        let ch = snap.characters.get(&pilot_char_id).unwrap();
        let expected_npc_corp = snap.characters.get(&pilot_char_id).and_then(|c| ctx.sde.bloodline(c.bloodline_id)).map(|b| b.corporation_id).unwrap();
        assert_eq!(expected_npc_corp, 1_000_009, "Civire bloodline resolves to NPC corp 1000009");
        assert_eq!(ch.corporation_id, expected_npc_corp, "Quit pilot moved to bloodline default NPC corp");

        let gov = snap.corp_governance.get(&player_corp_id).unwrap();
        assert!(!gov.member_titles.contains_key(&pilot_char_id), "Titles cleaned up on quit");
        assert!(!gov.member_account_keys.contains_key(&pilot_char_id), "Account keys cleaned up on quit");
        assert!(!gov.pending_role_grants.contains_key(&pilot_char_id), "Pending role grants cleaned up on quit");
        assert!(!gov.shares.contains_key(&pilot_char_id), "Shares returned to company shares on quit");

        let old_chan = snap.chat_channels.get(&format!("corpid:{player_corp_id}"));
        if let Some(c) = old_chan {
            assert!(!c.members.contains(&pilot_char_id), "Removed from player corp chat");
        }
        let npc_chan = snap.chat_channels.get(&format!("corpid:{expected_npc_corp}")).unwrap();
        assert!(npc_chan.members.contains(&pilot_char_id), "Added to NPC corp chat channel");
    });

    let cancel_err = rpc(&ctx, &mut sess, "industryManager", "CancelJob", &[PyRep::Integer(corp_job_id)]).await.expect_err("Ex-member cannot cancel corp job");
    assert_eq!(cancel_err.code, "JobOwnershipViolation");

    let deliver_err = rpc(&ctx, &mut sess, "industryManager", "CompleteJob", &[PyRep::Integer(corp_job_id)]).await.expect_err("Ex-member cannot deliver corp job");
    assert_eq!(deliver_err.code, "JobOwnershipViolation");
}
