//! `open_feats_b39_part2` — Standup Structure Weapons (`feat:structures.standup-point-defense-battery`,
//! `feat:structures.standup-guided-bomb-launcher`, `feat:structures.standup-superweapon-arcing-vorton`).

use eve_db::{CorporationRecord, InventoryFlag, InventoryItemRecord, UpwellStructureRecord};
use eve_destiny::{Ball, MovementMode, TargetLock};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::combat_crimewatch::SafetyLevel;
use eve_server::services::structure_standup_weapons::TYPE_KEEPSTAR;
use glam::DVec3;
use std::collections::HashMap;

fn kv_get(rep: &PyRep, key: &str) -> Option<PyRep> {
    match rep {
        PyRep::Dict(_) => rep.dict_get(key).cloned(),
        PyRep::KeyVal(map) => map.get(key).cloned(),
        PyRep::Object { args, .. } => args.dict_get(key).cloned(),
        _ => None,
    }
}

fn mk_item(id: i64, tid: u32, owner: i64, loc: i64, flag: InventoryFlag, qty: i32, singleton: bool) -> InventoryItemRecord {
    InventoryItemRecord { item_id: id, type_id: tid, owner_id: owner, location_id: loc, flag, quantity: qty, singleton, ..Default::default() }
}

async fn setup_pilot(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    ship_type_id: u32,
    solar_system_id: u32,
    position: DVec3,
    corp_id: i64,
) -> SessionState {
    let mut sess = SessionState::new(char_id as u64);
    sess.user_id = Some(char_id as u64);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(corp_id);
    sess.corp_role = eve_db::corp_roles::ROLE_DIRECTOR;
    sess.ship_id = Some(ship_id);
    sess.solar_system_id = Some(solar_system_id);

    ctx.db.with_mut_snapshot(|s| {
        s.corporations.entry(corp_id).or_default().member_roles.insert(char_id, eve_db::corp_roles::ROLE_DIRECTOR);
        let mut ch = s.characters.get(&90_000_001).cloned().unwrap();
        ch.character_id = char_id;
        ch.name = format!("Pilot-{char_id}");
        ch.corporation_id = corp_id;
        ch.alliance_id = None;
        ch.solar_system_id = solar_system_id;
        ch.active_ship_id = ship_id;
        ch.station_id = None;
        ch.structure_id = None;
        s.characters.insert(char_id, ch);
        s.items.insert(ship_id, mk_item(ship_id, ship_type_id, char_id, solar_system_id as i64, InventoryFlag::None, 1, true));
    });

    let ship_type = ctx.sde.types.get(&ship_type_id);
    let handle = ctx.systems.get_or_spawn(solar_system_id);
    let _ = handle
        .spawn_ball_with_combat(
            Ball {
                id: ship_id, type_id: ship_type_id, position, velocity: DVec3::ZERO,
                max_velocity: 250.0, inertia: 0.5, mass: ship_type.map(|t| t.mass).unwrap_or(1_500_000.0),
                radius: ship_type.map(|t| t.radius).unwrap_or(50.0), speed_fraction: 0.0, mode: MovementMode::Stop,
            },
            ctx.build_type_combat_state(ship_type_id),
        )
        .await;
    sess
}

#[tokio::test]
async fn test_b39_standup_point_defense_battery() {
    // parity: feat:structures.standup-point-defense-battery
    eve_testkit::attest(&["feat:structures.standup-point-defense-battery"]);
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let (hisec_sys, lowsec_sys) = (30_000_144_u32, 30_000_012_u32);
    let (corp_id, ally_corp_id, alliance_id, hostile_corp) = (98_000_555_i64, 98_000_556_i64, 99_000_555_i64, 98_000_599_i64);
    let (gunner_id, gunner_ship, keepstar_id, pdb_mod_id, flak_item_id) = (91_000_031_i64, 101_000_031_i64, 101_000_500_i64, 101_000_501_i64, 101_000_505_i64);

    let mut sess = setup_pilot(&ctx, gunner_id, gunner_ship, 587, hisec_sys, DVec3::ZERO, corp_id).await;
    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&gunner_id).unwrap().alliance_id = Some(alliance_id);
        s.corporations.insert(corp_id, CorporationRecord {
            corporation_id: corp_id, alliance_id: Some(alliance_id), name: "Keepstar Corp".into(), ticker: "KEEP".into(),
            ceo_id: gunner_id, members: [gunner_id].into_iter().collect(), ..Default::default()
        });
        s.corporations.insert(ally_corp_id, CorporationRecord {
            corporation_id: ally_corp_id, alliance_id: Some(alliance_id), name: "Alliance Mate Corp".into(), ticker: "ALLY".into(),
            ceo_id: gunner_id, ..Default::default()
        });
        s.structures.insert(keepstar_id, UpwellStructureRecord {
            structure_id: keepstar_id, type_id: 35832, owner_corp_id: corp_id, solar_system_id: hisec_sys,
            name: "Test Keepstar".into(), position: [0.0; 3], state: "ShieldVulnerable".into(),
            quantum_core_installed: true, fuel_blocks: 1000, shield_hp: 1.0, armor_hp: 1.0, hull_hp: 1.0,
            reinforced_until_unix: 0, last_damage_tick: 0, last_damage_unix: 0, self_destruct_at_unix: None,
        });
        s.items.insert(keepstar_id, mk_item(keepstar_id, TYPE_KEEPSTAR, corp_id, hisec_sys as i64, InventoryFlag::None, 1, true));
        s.items.insert(pdb_mod_id, mk_item(pdb_mod_id, 35926, corp_id, keepstar_id, InventoryFlag::HiSlot0, 1, true));
        s.module_online_states.insert(pdb_mod_id, false);
    });

    let hi_handle = ctx.systems.get_or_spawn(hisec_sys);
    let _ = hi_handle.spawn_ball_with_combat(
        Ball { id: keepstar_id, type_id: TYPE_KEEPSTAR, position: DVec3::ZERO, velocity: DVec3::ZERO, max_velocity: 0.0, inertia: 1.0, mass: 1_000_000_000.0, radius: 1_000.0, speed_fraction: 0.0, mode: MovementMode::Stop },
        ctx.build_type_combat_state(TYPE_KEEPSTAR),
    ).await;

    sess.structure_id = Some(keepstar_id);
    ctx.dispatch_rpc(&mut sess, "structureControl", None, "BoardStructure", &[PyRep::Integer(keepstar_id)], &kw, None).await.expect("BoardStructure");

    // 1. Negative cases: offline module, high-sec, non-XL structure, no ammo
    assert_eq!(ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(pdb_mod_id)], &kw, None).await.unwrap_err().code, "ModuleOffline");
    ctx.db.with_mut_snapshot(|s| { s.module_online_states.insert(pdb_mod_id, true); });
    assert_eq!(ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(pdb_mod_id)], &kw, None).await.unwrap_err().code, "CannotActivateInHighSec");

    let st_pos = DVec3::new(10_000_000.0, 0.0, 0.0);
    ctx.db.with_mut_snapshot(|s| {
        s.structures.get_mut(&keepstar_id).unwrap().solar_system_id = lowsec_sys;
        s.structures.get_mut(&keepstar_id).unwrap().position = [st_pos.x, st_pos.y, st_pos.z];
        s.items.get_mut(&keepstar_id).unwrap().location_id = lowsec_sys as i64;
        s.characters.get_mut(&gunner_id).unwrap().solar_system_id = lowsec_sys;
    });
    sess.solar_system_id = Some(lowsec_sys);
    let low_handle = ctx.systems.get_or_spawn(lowsec_sys);
    let _ = low_handle.spawn_ball_with_combat(
        Ball { id: keepstar_id, type_id: TYPE_KEEPSTAR, position: st_pos, velocity: DVec3::ZERO, max_velocity: 0.0, inertia: 1.0, mass: 1_000_000_000.0, radius: 1_000.0, speed_fraction: 0.0, mode: MovementMode::Stop },
        ctx.build_type_combat_state(TYPE_KEEPSTAR),
    ).await;

    assert_eq!(ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(pdb_mod_id)], &kw, None).await.unwrap_err().code, "RequiresXLStructure");
    ctx.db.with_mut_snapshot(|s| { s.structures.get_mut(&keepstar_id).unwrap().type_id = TYPE_KEEPSTAR; });
    assert_eq!(ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(pdb_mod_id)], &kw, None).await.unwrap_err().code, "NoAmmoLoaded");

    ctx.db.with_mut_snapshot(|s| {
        s.items.insert(flak_item_id, mk_item(flak_item_id, 63195, corp_id, keepstar_id, InventoryFlag::SpecializedAmmoHold, 500, false));
    });

    // Spawn hostile carrier (23915) at 1,500m and launch a real Light Fighter squadron (Templar I, 40562)
    let (c_hostile, s_hostile, fighter_item) = (91_000_032_i64, 101_000_032_i64, 101_000_033_i64);
    let mut sess_hostile = setup_pilot(&ctx, c_hostile, s_hostile, 23915, lowsec_sys, st_pos + DVec3::new(1_500.0, 0.0, 0.0), hostile_corp).await;
    ctx.db.with_mut_snapshot(|s| {
        s.items.insert(fighter_item, mk_item(fighter_item, 40562, c_hostile, s_hostile, InventoryFlag::FighterBay, 9, false));
    });
    ctx.dispatch_rpc(&mut sess_hostile, "fighterMgr", None, "LoadFightersToTube", &[PyRep::Integer(0), PyRep::Integer(fighter_item)], &kw, None).await.expect("Load fighter");
    ctx.dispatch_rpc(&mut sess_hostile, "fighterMgr", None, "LaunchFightersFromTubes", &[], &kw, None).await.expect("Launch fighter");
    let fighter_ball_id = ctx.active_fighters.read().unwrap().keys().copied().next().expect("fighter ball");

    let spawn_drone = |did: i64, x_off: f64| {
        let h = low_handle.clone();
        let cs = ctx.build_type_combat_state(2454);
        async move {
            let _ = h.spawn_ball_with_meta(
                Ball { id: did, type_id: 2454, position: st_pos + DVec3::new(x_off, 0.0, 0.0), velocity: DVec3::ZERO, max_velocity: 300.0, inertia: 0.5, mass: 5_000.0, radius: 10.0, speed_fraction: 0.0, mode: MovementMode::Stop },
                cs,
                eve_destiny::SlimItemMeta { group_id: 100, category_id: 18, owner_id: c_hostile, char_id: Some(c_hostile), corp_id: hostile_corp, alliance_id: -1, ..Default::default() },
            ).await;
        }
    };
    spawn_drone(101_000_034, 1_800.0).await;

    let (c_ally, s_ally) = (91_000_035_i64, 101_000_035_i64);
    let _ = setup_pilot(&ctx, c_ally, s_ally, 587, lowsec_sys, st_pos + DVec3::new(1_200.0, 0.0, 0.0), ally_corp_id).await;
    ctx.db.with_mut_snapshot(|s| { s.characters.get_mut(&c_ally).unwrap().alliance_id = Some(alliance_id); });
    let s_far = 101_000_036_i64;
    let _ = setup_pilot(&ctx, 91_000_036, s_far, 587, lowsec_sys, st_pos + DVec3::new(5_000.0, 0.0, 0.0), hostile_corp).await;

    // 2. Low-sec Safety::Green spares neutral capsuleer ship (`s_hostile`); Safety::Red hits ship + drone + fighter!
    eve_server::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| { rt.safety_levels.insert(gunner_id, SafetyLevel::Green); });
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(pdb_mod_id)], &kw, None).await.expect("PDB Green safety");
    let carrier_cs_green = low_handle.get_combat_state(s_hostile).await.unwrap().unwrap();
    assert_eq!(carrier_cs_green.shield_hp, carrier_cs_green.max_shield_hp, "Safety Green spares neutral capsuleer in low-sec");
    assert_eq!(ctx.db.with_snapshot(|s| s.items.get(&flak_item_id).unwrap().quantity), 300, "Consumed 200 Flak Rounds");

    ctx.advance_secs(13);
    eve_server::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| { rt.safety_levels.insert(gunner_id, SafetyLevel::Red); });
    spawn_drone(101_000_037, 1_700.0).await;

    let cap_before = low_handle.get_combat_state(keepstar_id).await.unwrap().unwrap().capacitor;
    let fighter_hp_before = low_handle.get_combat_state(fighter_ball_id).await.unwrap().map(|c| c.shield_hp + c.armor_hp + c.structure_hp).unwrap_or(0.0);
    let red_rep = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(pdb_mod_id)], &kw, None).await.expect("PDB Red safety");
    assert!(kv_get(&red_rep, "targetsHit").and_then(|v| v.as_i64()).unwrap_or(0) >= 3);

    assert!(low_handle.get_combat_state(keepstar_id).await.unwrap().unwrap().capacitor < cap_before);
    assert_eq!(ctx.db.with_snapshot(|s| s.items.get(&flak_item_id).unwrap().quantity), 100);
    let carrier_cs_red = low_handle.get_combat_state(s_hostile).await.unwrap().unwrap();
    assert!(carrier_cs_red.shield_hp < carrier_cs_red.max_shield_hp);
    let fighter_hp_after = low_handle.get_combat_state(fighter_ball_id).await.unwrap().map(|c| c.shield_hp + c.armor_hp + c.structure_hp).unwrap_or(0.0);
    assert!(fighter_hp_after < fighter_hp_before, "Hostile fighter squadron took omni pulse damage");

    let ally_cs = low_handle.get_combat_state(s_ally).await.unwrap().unwrap();
    assert_eq!(ally_cs.shield_hp, ally_cs.max_shield_hp, "Alliance mate spared");
    let far_cs = low_handle.get_combat_state(s_far).await.unwrap().unwrap();
    assert_eq!(far_cs.shield_hp, far_cs.max_shield_hp, "Out-of-range ship spared");
    assert!(ctx.crimewatch.with_crimewatch(|cw| cw.get_aggression(gunner_id, c_hostile, ctx.now_unix()).is_some()));

    // 3. Fatal ship kill via Standup Point Defense Battery routes through `execute_death_pipeline`
    let (c_fatal, s_fatal, cargo_loot_id) = (91_000_038_i64, 101_000_038_i64, 101_000_039_i64);
    let _ = setup_pilot(&ctx, c_fatal, s_fatal, 587, lowsec_sys, st_pos + DVec3::new(1_100.0, 0.0, 0.0), hostile_corp).await;
    ctx.db.with_mut_snapshot(|s| {
        s.items.get_mut(&flak_item_id).unwrap().quantity = 400;
        s.items.insert(cargo_loot_id, mk_item(cargo_loot_id, 34, c_fatal, s_fatal, InventoryFlag::Cargo, 100, false));
    });
    let mut fatal_cs = low_handle.get_combat_state(s_fatal).await.unwrap().unwrap();
    fatal_cs.shield_hp = 0.0;
    fatal_cs.armor_hp = 0.0;
    fatal_cs.structure_hp = 50.0;
    low_handle.set_combat_state(s_fatal, fatal_cs).await.unwrap();

    ctx.advance_secs(13);
    let kill_rep = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(pdb_mod_id)], &kw, None).await.expect("PDB fatal shot");
    assert!(kv_get(&kill_rep, "destroyedCount").and_then(|v| v.as_i64()).unwrap_or(0) >= 1);
    let (ship_gone, new_pod_id, has_wreck, has_km) = ctx.db.with_snapshot(|s| {
        let gone = !s.items.contains_key(&s_fatal);
        let pod = s.characters.get(&c_fatal).map(|c| c.active_ship_id).unwrap_or(0);
        let wreck = s.items.values().any(|i| i.owner_id == c_fatal && ctx.sde.types.get(&i.type_id).is_some_and(|t| t.group_id == 186));
        let km = s.killmails.values().any(|k| k.victim_character_id == c_fatal && k.ship_type_id == 587);
        (gone, pod, wreck, km)
    });
    assert!(ship_gone, "Destroyed Rifter removed from WorldStateSnapshot.items");
    assert_ne!(new_pod_id, s_fatal, "Victim active_ship_id switched to ejected capsule");
    assert!(low_handle.get_ball_snapshot(new_pod_id).await.unwrap().is_some(), "Ejected capsule spawned in Ballpark");
    assert!(has_wreck && has_km, "Wreck and KillmailRecord generated by execute_death_pipeline");
}

#[tokio::test]
async fn test_b39_standup_guided_bomb_and_arcing_vorton() {
    // parity: feat:structures.standup-guided-bomb-launcher, feat:structures.standup-superweapon-arcing-vorton
    eve_testkit::attest(&[
        "feat:structures.standup-guided-bomb-launcher",
        "feat:structures.standup-superweapon-arcing-vorton",
    ]);
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let null_sys = 30_000_199_u32;
    let (corp_id, hostile_corp) = (98_000_655_i64, 98_000_699_i64);
    let (gunner_id, gunner_ship, bystander_id) = (91_000_131_i64, 101_000_131_i64, 91_000_139_i64);
    let (keepstar_id, bomb_mod_id, vorton_mod_id) = (101_000_600_i64, 101_000_602_i64, 101_000_603_i64);
    let st_pos = DVec3::new(10_000_000.0, 0.0, 0.0);

    let mut sess = setup_pilot(&ctx, gunner_id, gunner_ship, 587, null_sys, st_pos, corp_id).await;
    ctx.db.with_mut_snapshot(|s| {
        s.corporations.insert(corp_id, CorporationRecord {
            corporation_id: corp_id, name: "Keepstar Corp".into(), ticker: "KEEP".into(),
            ceo_id: gunner_id, members: [gunner_id].into_iter().collect(), ..Default::default()
        });
        s.structures.insert(keepstar_id, UpwellStructureRecord {
            structure_id: keepstar_id, type_id: TYPE_KEEPSTAR, owner_corp_id: corp_id, solar_system_id: null_sys,
            name: "Test Keepstar".into(), position: [st_pos.x, st_pos.y, st_pos.z], state: "ShieldVulnerable".into(),
            quantum_core_installed: true, fuel_blocks: 1000, shield_hp: 1.0, armor_hp: 1.0, hull_hp: 1.0,
            reinforced_until_unix: 0, last_damage_tick: 0, last_damage_unix: 0, self_destruct_at_unix: None,
        });
        s.items.insert(keepstar_id, mk_item(keepstar_id, TYPE_KEEPSTAR, corp_id, null_sys as i64, InventoryFlag::None, 1, true));
        for (mid, tid, flag) in [(bomb_mod_id, 35923_u32, InventoryFlag::HiSlot1), (vorton_mod_id, 35928_u32, InventoryFlag::HiSlot2)] {
            s.items.insert(mid, mk_item(mid, tid, corp_id, keepstar_id, flag, 1, true));
            s.module_online_states.insert(mid, true);
        }
        // Bystander personal hangar guided bombs MUST NOT be usable by the structure weapon!
        s.items.insert(101_000_699, mk_item(101_000_699, 37849, bystander_id, keepstar_id, InventoryFlag::Hangar, 50, false));
    });

    let null_handle = ctx.systems.get_or_spawn(null_sys);
    let _ = null_handle.spawn_ball_with_combat(
        Ball { id: keepstar_id, type_id: TYPE_KEEPSTAR, position: st_pos, velocity: DVec3::ZERO, max_velocity: 0.0, inertia: 1.0, mass: 1_000_000_000.0, radius: 1_000.0, speed_fraction: 0.0, mode: MovementMode::Stop },
        ctx.build_type_combat_state(TYPE_KEEPSTAR),
    ).await;

    sess.structure_id = Some(keepstar_id);
    ctx.dispatch_rpc(&mut sess, "structureControl", None, "BoardStructure", &[PyRep::Integer(keepstar_id)], &kw, None).await.expect("BoardStructure");

    let (s_bs, s_frig, s_oor) = (101_000_140_i64, 101_000_141_i64, 101_000_142_i64);
    let _ = setup_pilot(&ctx, 91_000_140, s_bs, 638, null_sys, st_pos + DVec3::new(30_000.0, 0.0, 0.0), hostile_corp).await;
    let _ = setup_pilot(&ctx, 91_000_141, s_frig, 587, null_sys, st_pos + DVec3::new(34_000.0, 0.0, 0.0), hostile_corp).await;
    let _ = setup_pilot(&ctx, 91_000_142, s_oor, 638, null_sys, st_pos + DVec3::new(1_500_000.0, 0.0, 0.0), hostile_corp).await;

    // 1. Guided Bomb Launcher negative checks: unlocked target, bystander hangar ammo ignored, out of range
    let b_bs_args = [PyRep::Integer(bomb_mod_id), PyRep::String("targetAttack".into()), PyRep::Integer(s_bs)];
    assert_eq!(ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &b_bs_args, &kw, None).await.unwrap_err().code, "TargetNotLocked");

    for tid in [s_bs, s_frig, s_oor] {
        let _ = null_handle.add_target_lock(keepstar_id, TargetLock { target_id: tid, lock_time_ms: 0.0, remaining_lock_ms: 0.0, max_range_m: Some(2_000_000.0), locked: true }).await;
    }
    assert_eq!(ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &b_bs_args, &kw, None).await.unwrap_err().code, "NoAmmoLoaded");

    let heavy_bomb_stack = 101_000_620_i64;
    ctx.db.with_mut_snapshot(|s| {
        s.items.insert(heavy_bomb_stack, mk_item(heavy_bomb_stack, 37849, corp_id, keepstar_id, InventoryFlag::HiSlot1, 5, false));
    });
    let b_oor_args = [PyRep::Integer(bomb_mod_id), PyRep::String("targetAttack".into()), PyRep::Integer(s_oor)];
    assert_eq!(ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &b_oor_args, &kw, None).await.unwrap_err().code, "TargetOutOfRange");

    // 2. Launch Heavy Guided Bomb (37849) at Raven: spawns in-flight bomb Ball with 4M HP, detonates on impact with 20km AoE sig-scaled splash!
    let bs_shield_0 = null_handle.get_combat_state(s_bs).await.unwrap().unwrap().shield_hp;
    let frig_shield_0 = null_handle.get_combat_state(s_frig).await.unwrap().unwrap().shield_hp;

    let launch_rep = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &b_bs_args, &kw, None).await.expect("Launch Heavy Guided Bomb");
    assert_eq!(kv_get(&launch_rep, "inFlight"), Some(PyRep::Bool(true)));
    let bomb_ball_id = kv_get(&launch_rep, "bombID").and_then(|v| v.as_i64()).expect("bombID");
    assert_eq!(null_handle.get_combat_state(bomb_ball_id).await.unwrap().unwrap().max_structure_hp, 4_000_000.0);
    assert_eq!(null_handle.get_combat_state(s_bs).await.unwrap().unwrap().shield_hp, bs_shield_0);

    for _ in 0..15 {
        null_handle.step_manual(1.0).await.unwrap();
    }
    assert!(null_handle.get_ball_snapshot(bomb_ball_id).await.unwrap().is_none());
    let bs_dmg = bs_shield_0 - null_handle.get_combat_state(s_bs).await.unwrap().unwrap().shield_hp;
    let frig_dmg = frig_shield_0 - null_handle.get_combat_state(s_frig).await.unwrap().unwrap().shield_hp;
    assert!(bs_dmg > 0.0 && frig_dmg > 0.0, "20 km AoE splash hit both Raven and nearby Rifter");
    assert!(bs_dmg > frig_dmg * 2.0, "Signature scaling: Raven ({bs_dmg}) >> Rifter ({frig_dmg})");

    // Light Guided Bomb (37850) after 61s cycle
    ctx.advance_secs(61);
    ctx.db.with_mut_snapshot(|s| { s.items.get_mut(&heavy_bomb_stack).unwrap().type_id = 37850; });
    let b_frig_args = [PyRep::Integer(bomb_mod_id), PyRep::String("targetAttack".into()), PyRep::Integer(s_frig)];
    let light_rep = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &b_frig_args, &kw, None).await.expect("Launch Light Guided Bomb");
    let light_bomb_id = kv_get(&light_rep, "bombID").and_then(|v| v.as_i64()).unwrap();
    for _ in 0..15 {
        null_handle.step_manual(1.0).await.unwrap();
    }
    assert!(null_handle.get_ball_snapshot(light_bomb_id).await.unwrap().is_none());

    // 3. Standup Arcing Vorton Projector: Keepstar-only, <=400km range, 20km jump radius, max 5 total targets, 30s immobilization, 600s cooldown
    ctx.db.with_mut_snapshot(|s| { s.structures.get_mut(&keepstar_id).unwrap().type_id = 35833; });
    let v_bs_args = [PyRep::Integer(vorton_mod_id), PyRep::String("targetAttack".into()), PyRep::Integer(s_bs)];
    assert_eq!(ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &v_bs_args, &kw, None).await.unwrap_err().code, "CantFitToStructure");
    ctx.db.with_mut_snapshot(|s| { s.structures.get_mut(&keepstar_id).unwrap().type_id = TYPE_KEEPSTAR; });

    let v_oor_args = [PyRep::Integer(vorton_mod_id), PyRep::String("targetAttack".into()), PyRep::Integer(s_oor)];
    assert_eq!(ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &v_oor_args, &kw, None).await.unwrap_err().code, "TargetOutOfRange");

    let v_primary = 101_000_710_i64;
    let _ = setup_pilot(&ctx, 91_000_710, v_primary, 24692, null_sys, st_pos + DVec3::new(100_000.0, 0.0, 0.0), hostile_corp).await;
    let _ = null_handle.add_target_lock(keepstar_id, TargetLock { target_id: v_primary, lock_time_ms: 0.0, remaining_lock_ms: 0.0, max_range_m: Some(400_000.0), locked: true }).await;
    let mut cluster_ids = vec![v_primary];
    for idx in 0..6_i64 {
        let sid = 101_000_711 + idx;
        let _ = setup_pilot(&ctx, 91_000_711 + idx, sid, 24692, null_sys, st_pos + DVec3::new(100_000.0 + ((idx + 1) as f64) * 2_500.0, 0.0, 0.0), hostile_corp).await;
        cluster_ids.push(sid);
    }
    let outside_jump_id = 101_000_720_i64;
    let _ = setup_pilot(&ctx, 91_000_720, outside_jump_id, 24692, null_sys, st_pos + DVec3::new(145_000.0, 0.0, 0.0), hostile_corp).await;
    for &sid in cluster_ids.iter().chain(std::iter::once(&outside_jump_id)) {
        let mut cs = null_handle.get_combat_state(sid).await.unwrap().unwrap();
        cs.shield_hp = 10_000_000.0;
        cs.max_shield_hp = 10_000_000.0;
        null_handle.set_combat_state(sid, cs).await.unwrap();
    }

    ctx.advance_secs(61);
    null_handle.set_capacitor(keepstar_id, 600_000.0).await.unwrap();
    let v_args = [PyRep::Integer(vorton_mod_id), PyRep::String("targetAttack".into()), PyRep::Integer(v_primary)];
    let vorton_rep = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &v_args, &kw, None).await.expect("Fire Arcing Vorton");
    assert_eq!(kv_get(&vorton_rep, "targetsHit").and_then(|v| v.as_i64()), Some(5));

    let mut hop_losses = Vec::new();
    for &sid in &cluster_ids {
        let cs = null_handle.get_combat_state(sid).await.unwrap().unwrap();
        let lost = cs.max_shield_hp - cs.shield_hp;
        if lost > 0.0 {
            hop_losses.push(lost);
        }
    }
    assert_eq!(hop_losses.len(), 5, "Exactly 5 of 7 ships in cluster took chained beam damage");
    for w in hop_losses.windows(2) {
        assert!(w[1] < w[0], "Vorton arc damage attenuates per hop: {} -> {}", w[0], w[1]);
    }
    let out_cs = null_handle.get_combat_state(outside_jump_id).await.unwrap().unwrap();
    assert_eq!(out_cs.shield_hp, out_cs.max_shield_hp, "Target beyond 20 km jump radius spared");

    // During 30s immobilization (+10s): weapons, ReleaseControl, EjectFromStructure, and Undock all blocked
    ctx.advance_secs(10);
    assert_eq!(ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &b_bs_args, &kw, None).await.unwrap_err().code, "WeaponCycleImmobilized");
    assert_eq!(ctx.dispatch_rpc(&mut sess, "structureControl", None, "ReleaseControl", &[PyRep::Integer(keepstar_id)], &kw, None).await.unwrap_err().code, "GunnerImmobilized");
    assert_eq!(ctx.dispatch_rpc(&mut sess, "structureControl", None, "EjectFromStructure", &[PyRep::Integer(keepstar_id)], &kw, None).await.unwrap_err().code, "GunnerImmobilized");
    assert_eq!(ctx.dispatch_rpc(&mut sess, "structureDocking", None, "Undock", &[PyRep::Integer(keepstar_id)], &kw, None).await.unwrap_err().code, "GunnerImmobilized");

    // After 30s immobilization expires (+25s -> +35s): Vorton still on 600s cooldown, ReleaseControl succeeds
    ctx.advance_secs(25);
    let cyc_err = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &v_args, &kw, None).await.unwrap_err();
    assert_eq!(cyc_err.code, "ModuleAlreadyCycling");
    ctx.dispatch_rpc(&mut sess, "structureControl", None, "ReleaseControl", &[PyRep::Integer(keepstar_id)], &kw, None).await.expect("ReleaseControl after 30s");
}

#[tokio::test]
async fn test_b39_standup_falloff_bomb_safety_timers_and_kills() {
    // parity: feat:structures.standup-guided-bomb-launcher, feat:structures.standup-superweapon-arcing-vorton
    eve_testkit::attest(&[
        "feat:structures.standup-guided-bomb-launcher",
        "feat:structures.standup-superweapon-arcing-vorton",
    ]);
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let low_sys = 30_000_012_u32;
    let (corp_id, hostile_corp, neutral_corp) = (98_000_755_i64, 98_000_799_i64, 98_000_788_i64);
    let (gunner_id, gunner_ship, keepstar_id, bomb_mod_id, vorton_mod_id, bomb_stack) =
        (91_000_231_i64, 101_000_231_i64, 101_000_800_i64, 101_000_802_i64, 101_000_803_i64, 101_000_820_i64);
    let st_pos = DVec3::new(20_000_000.0, 0.0, 0.0);

    let mut sess = setup_pilot(&ctx, gunner_id, gunner_ship, 587, low_sys, st_pos, corp_id).await;
    ctx.db.with_mut_snapshot(|s| {
        s.corporations.insert(corp_id, CorporationRecord {
            corporation_id: corp_id, name: "Lowsec Keepstar".into(), ticker: "LKS".into(),
            ceo_id: gunner_id, members: [gunner_id].into_iter().collect(), ..Default::default()
        });
        s.structures.insert(keepstar_id, UpwellStructureRecord {
            structure_id: keepstar_id, type_id: TYPE_KEEPSTAR, owner_corp_id: corp_id, solar_system_id: low_sys,
            name: "Lowsec Keepstar".into(), position: [st_pos.x, st_pos.y, st_pos.z], state: "ShieldVulnerable".into(),
            quantum_core_installed: true, fuel_blocks: 1000, shield_hp: 1.0, armor_hp: 1.0, hull_hp: 1.0,
            reinforced_until_unix: 0, last_damage_tick: 0, last_damage_unix: 0, self_destruct_at_unix: None,
        });
        s.items.insert(keepstar_id, mk_item(keepstar_id, TYPE_KEEPSTAR, corp_id, low_sys as i64, InventoryFlag::None, 1, true));
        for (mid, tid, flag) in [(bomb_mod_id, 35923_u32, InventoryFlag::HiSlot1), (vorton_mod_id, 35928_u32, InventoryFlag::HiSlot2)] {
            s.items.insert(mid, mk_item(mid, tid, corp_id, keepstar_id, flag, 1, true));
            s.module_online_states.insert(mid, true);
        }
        s.items.insert(bomb_stack, mk_item(bomb_stack, 37849, corp_id, keepstar_id, InventoryFlag::HiSlot1, 10, false));
    });
    let low_h = ctx.systems.get_or_spawn(low_sys);
    let _ = low_h.spawn_ball_with_combat(
        Ball { id: keepstar_id, type_id: TYPE_KEEPSTAR, position: st_pos, velocity: DVec3::ZERO, max_velocity: 0.0, inertia: 1.0, mass: 1_000_000_000.0, radius: 1_000.0, speed_fraction: 0.0, mode: MovementMode::Stop },
        ctx.build_type_combat_state(TYPE_KEEPSTAR),
    ).await;
    sess.structure_id = Some(keepstar_id);
    ctx.dispatch_rpc(&mut sess, "structureControl", None, "BoardStructure", &[PyRep::Integer(keepstar_id)], &kw, None).await.expect("BoardStructure");

    // 1. Guided Bomb splash: SafetyLevel::Green spares neutral low-sec bystander & corp friendly, hits suspect + sets OF-51 PvP/logoff timers
    let (c_susp, s_susp, c_neut, s_neut, c_frnd, s_frnd) =
        (91_000_240_i64, 101_000_240_i64, 91_000_241_i64, 101_000_241_i64, 91_000_242_i64, 101_000_242_i64);
    let _ = setup_pilot(&ctx, c_susp, s_susp, 638, low_sys, st_pos + DVec3::new(25_000.0, 0.0, 0.0), hostile_corp).await;
    let _ = setup_pilot(&ctx, c_neut, s_neut, 638, low_sys, st_pos + DVec3::new(28_000.0, 0.0, 0.0), neutral_corp).await;
    let _ = setup_pilot(&ctx, c_frnd, s_frnd, 638, low_sys, st_pos + DVec3::new(26_500.0, 0.0, 0.0), corp_id).await;
    let _ = low_h.add_target_lock(keepstar_id, TargetLock { target_id: s_susp, lock_time_ms: 0.0, remaining_lock_ms: 0.0, max_range_m: Some(400_000.0), locked: true }).await;
    {
        let mut cw = ctx.crimewatch.write().unwrap();
        cw.get_or_create(c_susp).suspect_flag_until_unix = ctx.now_unix() + 900;
    }
    eve_server::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| { rt.safety_levels.insert(gunner_id, SafetyLevel::Green); });
    let neut_sh0 = low_h.get_combat_state(s_neut).await.unwrap().unwrap().shield_hp;
    let frnd_sh0 = low_h.get_combat_state(s_frnd).await.unwrap().unwrap().shield_hp;
    let susp_sh0 = low_h.get_combat_state(s_susp).await.unwrap().unwrap().shield_hp;

    let b_args = [PyRep::Integer(bomb_mod_id), PyRep::String("targetAttack".into()), PyRep::Integer(s_susp)];
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &b_args, &kw, None).await.expect("Launch Guided Bomb under Green safety at suspect");
    for _ in 0..15 {
        low_h.step_manual(1.0).await.unwrap();
    }
    assert!(low_h.get_combat_state(s_susp).await.unwrap().unwrap().shield_hp < susp_sh0, "Suspect took guided bomb impact damage");
    assert_eq!(low_h.get_combat_state(s_neut).await.unwrap().unwrap().shield_hp, neut_sh0, "SafetyLevel::Green spared neutral low-sec bystander from AoE splash");
    assert_eq!(low_h.get_combat_state(s_frnd).await.unwrap().unwrap().shield_hp, frnd_sh0, "Friendly corp ship spared from AoE splash");
    assert!(ctx.db.with_snapshot(|s| s.sp6_lifecycle.pvp_timers_until.get(&c_susp).copied().unwrap_or(0) > ctx.now_unix()), "Suspect victim received OF-51 PvP logoff timer");
    assert_eq!(ctx.db.with_snapshot(|s| s.sp6_lifecycle.pvp_timers_until.get(&c_neut).copied().unwrap_or(0)), 0, "Spared neutral has no PvP timer");

    // Switch gunner to SafetyLevel::Red and make `s_neut` low-HP so Guided Bomb splash kills `s_neut` through `execute_death_pipeline`!
    ctx.advance_secs(61);
    eve_server::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| { rt.safety_levels.insert(gunner_id, SafetyLevel::Red); });
    let mut neut_cs = low_h.get_combat_state(s_neut).await.unwrap().unwrap();
    neut_cs.shield_hp = 0.0;
    neut_cs.armor_hp = 0.0;
    neut_cs.structure_hp = 100.0;
    low_h.set_combat_state(s_neut, neut_cs).await.unwrap();
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &b_args, &kw, None).await.expect("Launch Guided Bomb under Red safety");
    for _ in 0..15 {
        low_h.step_manual(1.0).await.unwrap();
    }
    assert!(ctx.db.with_snapshot(|s| !s.items.contains_key(&s_neut)), "Guided Bomb splash fatal kill destroyed ship via execute_death_pipeline");
    assert!(ctx.db.with_snapshot(|s| s.killmails.values().any(|k| k.victim_character_id == c_neut && k.ship_type_id == 638)), "Killmail generated for Guided Bomb kill");
    assert!(ctx.db.with_snapshot(|s| s.sp6_lifecycle.pvp_timers_until.get(&c_neut).copied().unwrap_or(0) > ctx.now_unix()), "Splashed neutral received OF-51 PvP logoff timer");
    assert_eq!(low_h.get_combat_state(s_frnd).await.unwrap().unwrap().shield_hp, frnd_sh0, "Friendly corp ship still spared under Red safety");

    // 2. Arcing Vorton optimal (250km) + falloff (50km, max 350km) range check & 0.5x falloff attenuation
    let (s_375k, s_200k, s_300k) = (101_000_250_i64, 101_000_251_i64, 101_000_252_i64);
    let _ = setup_pilot(&ctx, 91_000_250, s_375k, 24692, low_sys, st_pos + DVec3::new(375_000.0, 0.0, 0.0), hostile_corp).await;
    let _ = setup_pilot(&ctx, 91_000_251, s_200k, 24692, low_sys, st_pos + DVec3::new(200_000.0, 0.0, 0.0), hostile_corp).await;
    let _ = setup_pilot(&ctx, 91_000_252, s_300k, 24692, low_sys, st_pos + DVec3::new(0.0, 300_000.0, 0.0), hostile_corp).await;
    for sid in [s_375k, s_200k, s_300k] {
        let _ = low_h.add_target_lock(keepstar_id, TargetLock { target_id: sid, lock_time_ms: 0.0, remaining_lock_ms: 0.0, max_range_m: Some(400_000.0), locked: true }).await;
        let mut cs = low_h.get_combat_state(sid).await.unwrap().unwrap();
        cs.shield_hp = 10_000_000.0;
        cs.max_shield_hp = 10_000_000.0;
        low_h.set_combat_state(sid, cs).await.unwrap();
    }

    // Target locked at 375 km (inside Keepstar 400 km lock range, beyond Vorton 350 km optimal+2*falloff) -> TargetOutOfRange!
    low_h.set_capacitor(keepstar_id, 600_000.0).await.unwrap();
    let v_375_err = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(vorton_mod_id), PyRep::String("targetAttack".into()), PyRep::Integer(s_375k)], &kw, None).await.unwrap_err();
    assert_eq!(v_375_err.code, "TargetOutOfRange");
    assert_eq!(low_h.get_combat_state(keepstar_id).await.unwrap().unwrap().capacitor, 600_000.0, "Out-of-range Vorton check consumes zero cap");

    let rep_opt = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(vorton_mod_id), PyRep::String("targetAttack".into()), PyRep::Integer(s_200k)], &kw, None).await.expect("Vorton at 200km optimal");
    let dmg_opt = kv_get(&rep_opt, "appliedDamage").and_then(|v| v.as_f64()).unwrap();

    ctx.advance_secs(605);
    low_h.set_capacitor(keepstar_id, 600_000.0).await.unwrap();
    let rep_fall = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(vorton_mod_id), PyRep::String("targetAttack".into()), PyRep::Integer(s_300k)], &kw, None).await.expect("Vorton at 300km (optimal+falloff)");
    let dmg_fall = kv_get(&rep_fall, "appliedDamage").and_then(|v| v.as_f64()).unwrap();
    assert!(((dmg_fall / dmg_opt) - 0.5).abs() < 0.02, "At optimal + 1*falloff (300 km), Vorton damage attenuates by 0.5x: opt={dmg_opt}, fall={dmg_fall}");
}
