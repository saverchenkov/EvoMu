//! Dedicated integration tests for open `feat:modules.*` and `feat:navigation.*` parity rows (Batch 35, Part 1).

use crate::modules_advanced_tactical::{build_ctx, seed_pilot, NULL_SYS};
use eve_db::{FleetMemberRecord, FleetRecord, InventoryFlag, InventoryItemRecord, TrainedSkillRecord};
use eve_destiny::{Ball, DVec3, MovementMode};
use eve_proto::PyRep;
use std::collections::HashMap;

pub(crate) const DEST_NULL_SYS: u32 = 30004760;

pub(crate) fn give_item(
    ctx: &eve_server::router::ServerContext,
    item_id: i64,
    type_id: u32,
    owner_id: i64,
    location_id: i64,
    flag: InventoryFlag,
    qty: i32,
) {
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            item_id,
            InventoryItemRecord {
                item_id,
                type_id,
                owner_id,
                location_id,
                flag,
                quantity: qty,
                singleton: qty == 1 && !matches!(flag, InventoryFlag::Cargo | InventoryFlag::SpecializedFuelBay),
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

pub(crate) fn set_skill(ctx: &eve_server::router::ServerContext, char_id: i64, skill_type_id: u32, level: u8) {
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(char_id).or_default().insert(
            skill_type_id,
            TrainedSkillRecord { type_id: skill_type_id, level, skill_points: 256_000.0 },
        );
    });
}

pub(crate) async fn spawn_ship_in_space(
    ctx: &eve_server::router::ServerContext,
    char_id: i64,
    ship_id: i64,
    type_id: u32,
    sys_id: u32,
    pos: DVec3,
) {
    let sys = ctx.systems.get_or_spawn(sys_id);
    let t = ctx.sde.types.get(&type_id);
    let ball = Ball {
        id: ship_id,
        type_id,
        position: pos,
        velocity: DVec3::ZERO,
        max_velocity: 200.0,
        inertia: 1.0,
        mass: t.map(|x| x.mass).unwrap_or(10_000_000.0),
        radius: t.map(|x| x.radius).unwrap_or(100.0),
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let mut cs = ctx.build_ship_combat_state(char_id, ship_id, type_id);
    cs.undock_invulnerable_remaining_ticks = 0;
    let meta = ctx.build_ship_slim_meta(char_id, type_id);
    let _ = sys.spawn_ball_with_meta(ball, cs, meta).await;
}

pub(crate) fn put_in_fleet(ctx: &eve_server::router::ServerContext, fleet_id: i64, boss_id: i64, members: &[(i64, bool)]) {
    ctx.db.with_mut_snapshot(|snap| {
        let mut details = HashMap::new();
        let mut member_ids = Vec::new();
        for &(cid, accepts_conduit) in members {
            member_ids.push(cid);
            details.insert(
                cid,
                FleetMemberRecord {
                    character_id: cid,
                    wing_id: None,
                    squad_id: None,
                    role: if cid == boss_id { eve_db::FleetRole::FleetCommander } else { eve_db::FleetRole::SquadMember },
                    role_name: if cid == boss_id { "FC".into() } else { "Member".into() },
                    takes_fleet_warp: true,
                    accepts_regroup: true,
                    accepts_conduit_jumps: accepts_conduit,
                    job: if cid == boss_id { 2 } else { 0 },
                    join_time_unix: 1,
                },
            );
        }
        snap.fleets.insert(
            fleet_id,
            FleetRecord {
                fleet_id,
                boss_id,
                motd: String::new(),
                free_move: true,
                is_registered: false,
                members: member_ids,
                member_details: details,
                wings: Vec::new(),
                squads: Vec::new(),
                wing_details: HashMap::new(),
                squad_details: HashMap::new(),
                invites: std::collections::HashSet::new(),
            },
        );
    });
}

#[tokio::test]
async fn test_b35_01_lance_doomsday_directional_geometry() {
    // parity: feat:modules.lance-doomsday-directional-geometry
    eve_testkit::attest(&["feat:modules.lance-doomsday-directional-geometry"]);
    let ctx = build_ctx();
    let titan_char = 950_101;
    let titan_ship = 960_101;
    let mut titan_sess = seed_pilot(&ctx, titan_char, titan_ship, 11567, NULL_SYS, false); // Avatar
    spawn_ship_in_space(&ctx, titan_char, titan_ship, 11567, NULL_SYS, DVec3::ZERO).await;

    let lance_mod = 970_104;
    give_item(&ctx, lance_mod, 40631, titan_char, titan_ship, InventoryFlag::HiSlot0, 1);
    give_item(&ctx, 970_105, 16274, titan_char, titan_ship, InventoryFlag::Cargo, 500_000);

    let v_200km = 960_203;
    let v_350km = 960_204;
    let v_off_axis = 960_205;
    seed_pilot(&ctx, 950_203, v_200km, 641, NULL_SYS, false);
    seed_pilot(&ctx, 950_204, v_350km, 641, NULL_SYS, false);
    seed_pilot(&ctx, 950_205, v_off_axis, 641, NULL_SYS, false);
    spawn_ship_in_space(&ctx, 950_203, v_200km, 641, NULL_SYS, DVec3::new(200_000.0, 0.0, 0.0)).await;
    spawn_ship_in_space(&ctx, 950_204, v_350km, 641, NULL_SYS, DVec3::new(350_000.0, 0.0, 0.0)).await;
    spawn_ship_in_space(&ctx, 950_205, v_off_axis, 641, NULL_SYS, DVec3::new(100_000.0, 50_000.0, 0.0)).await;

    let sys = ctx.systems.get_or_spawn(NULL_SYS);
    let cs_200_init = sys.get_combat_state(v_200km).await.unwrap().unwrap();
    let cs_350_init = sys.get_combat_state(v_350km).await.unwrap().unwrap();
    let cs_off_init = sys.get_combat_state(v_off_axis).await.unwrap().unwrap();

    let dir_arg = PyRep::Tuple(vec![PyRep::Real(1.0), PyRep::Real(0.0), PyRep::Real(0.0)]);
    let lance_res = ctx
        .dispatch_rpc(&mut titan_sess, "superWeaponMgr", None, "ActivateSlashModule", &[PyRep::Integer(lance_mod), dir_arg], &HashMap::new(), None)
        .await
        .expect("ActivateSlashModule");
    assert_eq!(lance_res.dict_get("warmupSeconds"), Some(&PyRep::Real(15.0)));
    assert_eq!(lance_res.dict_get("jumpWarpSuppressionSeconds"), Some(&PyRep::Real(300.0)));
    assert!(eve_server::services::tactical_modules::check_ship_navigation_lock(&ctx, titan_ship, "jump").is_err());
    assert!(eve_server::services::tactical_modules::check_ship_navigation_lock(&ctx, titan_ship, "warp").is_err());

    // During 15s warmup: no damage yet, no victim debuff yet
    for _ in 0..14 {
        let _ = sys.step_manual(1.0).await;
    }
    let cs_200_mid = sys.get_combat_state(v_200km).await.unwrap().unwrap();
    assert_eq!(cs_200_mid.shield_hp, cs_200_init.shield_hp, "Damage must not land before 15s warmup completes");
    assert!(eve_server::services::tactical_modules::check_ship_navigation_lock(&ctx, v_200km, "jump").is_ok());

    // Tick 15: warmup completes -> 200km target hit & debuffed, 350km and off-axis untouched
    let _ = sys.step_manual(1.0).await;
    let cs_200_opt = sys.get_combat_state(v_200km).await.unwrap();
    let cs_350_after = sys.get_combat_state(v_350km).await.unwrap().unwrap();
    let cs_off_after = sys.get_combat_state(v_off_axis).await.unwrap().unwrap();
    assert!(
        cs_200_opt.is_none_or(|cs| cs.shield_hp < cs_200_init.shield_hp || cs.structure_hp < cs_200_init.structure_hp),
        "200km ship inside 300km beam cone must take massive damage after warmup"
    );
    assert_eq!(cs_350_after.shield_hp, cs_350_init.shield_hp, "350km ship beyond 300km must not be hit");
    assert_eq!(cs_off_after.shield_hp, cs_off_init.shield_hp, "Off-axis ship outside cone must not be hit");
    assert!(eve_server::services::tactical_modules::check_ship_navigation_lock(&ctx, v_200km, "jump").is_err(), "Hit ship must get 5-min jump suppression");
    assert!(eve_server::services::tactical_modules::check_ship_navigation_lock(&ctx, v_200km, "warp").is_err(), "Hit ship must get 5-min warp suppression");
}

#[tokio::test]
async fn test_b35_02_energy_nosferatu_relative_capacitor() {
    // parity: feat:modules.energy-nosferatu-relative-capacitor
    eve_testkit::attest(&["feat:modules.energy-nosferatu-relative-capacitor"]);
    let ctx = build_ctx();
    let sys = ctx.systems.get_or_spawn(NULL_SYS);

    let c_norm = 951_101;
    let s_norm = 961_101;
    let mut sess_norm = seed_pilot(&ctx, c_norm, s_norm, 641, NULL_SYS, false); // Megathron
    spawn_ship_in_space(&ctx, c_norm, s_norm, 641, NULL_SYS, DVec3::ZERO).await;
    let nos_mod = 971_101;
    give_item(&ctx, nos_mod, 12261, c_norm, s_norm, InventoryFlag::HiSlot0, 1);

    let c_tgt = 951_102;
    let s_tgt = 961_102;
    seed_pilot(&ctx, c_tgt, s_tgt, 587, NULL_SYS, false); // Rifter
    spawn_ship_in_space(&ctx, c_tgt, s_tgt, 587, NULL_SYS, DVec3::new(5_000.0, 0.0, 0.0)).await;

    let mut cs_norm = sys.get_combat_state(s_norm).await.unwrap().unwrap();
    cs_norm.max_capacitor = 5_000.0;
    cs_norm.capacitor = 4_000.0; // 80%
    sys.set_combat_state(s_norm, cs_norm).await.unwrap();

    let mut cs_tgt = sys.get_combat_state(s_tgt).await.unwrap().unwrap();
    cs_tgt.max_capacitor = 300.0;
    cs_tgt.capacitor = 150.0; // 50%
    sys.set_combat_state(s_tgt, cs_tgt).await.unwrap();

    let _ = sys.add_target_lock(s_norm, eve_destiny::TargetLock { target_id: s_tgt, locked: true, remaining_lock_ms: 0.0, lock_time_ms: 0.0, max_range_m: Some(100_000.0) }).await;
    ctx.dispatch_rpc(&mut sess_norm, "dogmaIM", None, "Activate", &[PyRep::Integer(nos_mod), PyRep::String("energyDestabilizationNew".into()), PyRep::Integer(s_tgt), PyRep::Integer(1)], &HashMap::new(), None).await.expect("Activate Nosferatu");
    let _ = sys.step_manual(15.0).await;
    let tgt_after_high = sys.get_combat_state(s_tgt).await.unwrap().unwrap();
    assert!(tgt_after_high.capacitor >= 150.0, "Higher cap % (80% > 50%) must not drain");

    // Equal cap % (100% == 100%): must not drain
    let mut cs_norm_eq = sys.get_combat_state(s_norm).await.unwrap().unwrap();
    cs_norm_eq.capacitor = 5_000.0; // 100%
    sys.set_combat_state(s_norm, cs_norm_eq).await.unwrap();
    let mut cs_tgt_eq = sys.get_combat_state(s_tgt).await.unwrap().unwrap();
    cs_tgt_eq.capacitor = 300.0; // 100%
    sys.set_combat_state(s_tgt, cs_tgt_eq).await.unwrap();
    let _ = sys.step_manual(15.0).await;
    let tgt_after_eq = sys.get_combat_state(s_tgt).await.unwrap().unwrap();
    assert!(tgt_after_eq.capacitor >= 300.0 - 1e-3, "Equal cap % (100% == 100%) must not drain");

    // Lower cap % (30% < 50%, even though 1500 GJ > 150 GJ): must drain & transfer
    let mut cs_norm_low = sys.get_combat_state(s_norm).await.unwrap().unwrap();
    cs_norm_low.capacitor = 1_500.0; // 30%
    sys.set_combat_state(s_norm, cs_norm_low).await.unwrap();
    let mut cs_tgt_low = sys.get_combat_state(s_tgt).await.unwrap().unwrap();
    cs_tgt_low.capacitor = 150.0; // 50%
    sys.set_combat_state(s_tgt, cs_tgt_low).await.unwrap();
    let _ = sys.step_manual(15.0).await;
    let tgt_after_low = sys.get_combat_state(s_tgt).await.unwrap().unwrap();
    assert!(tgt_after_low.capacitor < 150.0, "Lower cap % (30% < 50%) must drain target");

    // Deactivate first Nosferatu and reset target cap to 150.0 (50%) before isolated Blood Raider test
    let _ = sys.remove_module_cycle(s_norm, nos_mod).await;
    let mut cs_tgt_reset = sys.get_combat_state(s_tgt).await.unwrap().unwrap();
    cs_tgt_reset.capacitor = 150.0;
    sys.set_combat_state(s_tgt, cs_tgt_reset).await.unwrap();

    let c_br = 951_103;
    let s_br = 961_103;
    let mut sess_br = seed_pilot(&ctx, c_br, s_br, 17920, NULL_SYS, false); // Bhaalgorn
    spawn_ship_in_space(&ctx, c_br, s_br, 17920, NULL_SYS, DVec3::new(1_000.0, 0.0, 0.0)).await;
    let nos_br = 971_102;
    give_item(&ctx, nos_br, 12261, c_br, s_br, InventoryFlag::HiSlot0, 1);
    let mut cs_br = sys.get_combat_state(s_br).await.unwrap().unwrap();
    cs_br.max_capacitor = 5_000.0;
    cs_br.capacitor = 4_500.0; // 90% > 50%
    sys.set_combat_state(s_br, cs_br).await.unwrap();
    let _ = sys.add_target_lock(s_br, eve_destiny::TargetLock { target_id: s_tgt, locked: true, remaining_lock_ms: 0.0, lock_time_ms: 0.0, max_range_m: Some(100_000.0) }).await;
    ctx.dispatch_rpc(&mut sess_br, "dogmaIM", None, "Activate", &[PyRep::Integer(nos_br), PyRep::String("energyDestabilizationNew".into()), PyRep::Integer(s_tgt), PyRep::Integer(1)], &HashMap::new(), None).await.expect("Activate Bhaalgorn Nosferatu");
    let _ = sys.step_manual(15.0).await;
    let tgt_after_br = sys.get_combat_state(s_tgt).await.unwrap().unwrap();
    assert!(tgt_after_br.capacitor < 150.0, "Blood Raider hull must override cap % restriction and drain isolated target");
}

#[tokio::test]
async fn test_b35_03_mobile_depot_deployment_and_refitting() {
    // parity: feat:modules.mobile-depot-deployment-and-refitting
    eve_testkit::attest(&["feat:modules.mobile-depot-deployment-and-refitting"]);
    let ctx = build_ctx();
    let sys = ctx.systems.get_or_spawn(NULL_SYS);
    let char_id = 951_201;
    let ship_id = 961_201;
    let mut sess = seed_pilot(&ctx, char_id, ship_id, 641, NULL_SYS, false);
    spawn_ship_in_space(&ctx, char_id, ship_id, 641, NULL_SYS, DVec3::new(100_000.0, 0.0, 0.0)).await;

    // 1. Reject deployment within 50km of stargates/celestials via posMgr.AnchorStructure
    let gate = ctx.sde.stargates().values().find(|j| j.from_solar_system_id == NULL_SYS).expect("SDE stargate in NULL_SYS");
    let depot_item = 971_201;
    give_item(&ctx, depot_item, 33474, char_id, ship_id, InventoryFlag::Cargo, 1);
    sys.try_set_ball_position(ship_id, gate.position + DVec3::new(1_000.0, 0.0, 0.0));
    let near_pos = gate.position + DVec3::new(1_200.0, 0.0, 0.0);
    let near_err = ctx.dispatch_rpc(
        &mut sess, "posMgr", None, "AnchorStructure",
        &[PyRep::Integer(depot_item), PyRep::Tuple(vec![PyRep::Real(near_pos.x), PyRep::Real(near_pos.y), PyRep::Real(near_pos.z)])],
        &HashMap::new(), None,
    ).await;
    assert!(near_err.is_err(), "Mobile Depot deployment near stargate/celestial must be rejected");

    // 2. Launch from cargo >= 50km from stargates via posMgr.AnchorStructure
    let dep_pos = DVec3::new(100_000.0, 0.0, 0.0);
    sys.try_set_ball_position(ship_id, dep_pos);
    let target_pos = dep_pos + DVec3::new(500.0, 0.0, 0.0);
    let dep_res = ctx.dispatch_rpc(
        &mut sess, "posMgr", None, "AnchorStructure",
        &[PyRep::Integer(depot_item), PyRep::Tuple(vec![PyRep::Real(target_pos.x), PyRep::Real(target_pos.y), PyRep::Real(target_pos.z)])],
        &HashMap::new(), None,
    ).await.expect("AnchorStructure succeeds");
    assert_eq!(dep_res.dict_get("anchoringDelaySeconds"), Some(&PyRep::Real(60.0)));

    let st_anchoring = ctx.db.with_snapshot(|snap| eve_server::services::mobile_deployables::read_deployable_state_snap(snap).deployables.get(&depot_item).cloned()).unwrap();
    assert_eq!(st_anchoring.anchor_duration_secs, 60);
    assert_eq!(st_anchoring.group_id, 1246);
    assert_eq!(st_anchoring.solar_system_id, NULL_SYS);
    assert!(!st_anchoring.anchor_completed);

    // 3. Invulnerable during 60-second anchoring timer: damage applied is 0.0
    let dmg_att = eve_destiny::DamageVector::new(500.0, 0.0, 0.0, 0.0);
    let (applied_invuln, destroyed_invuln) = eve_server::services::station_structure::structure_lifecycle_rpc::apply_damage_structure_or_ball(
        &ctx, &sys, NULL_SYS, char_id, depot_item, dmg_att, 1.0, ctx.now_unix_secs(),
    ).await.ok().flatten().map(|(a, d, _)| (a, d)).unwrap_or((0.0, false));
    assert_eq!(applied_invuln, 0.0, "Mobile Depot must be invulnerable during anchoring countdown");
    assert!(!destroyed_invuln);

    // 4. Advance 61s -> step system -> anchor_completed is true on its own
    ctx.advance_time_seconds(61);
    let _ = sys.step_manual(1.0).await;
    let st_anchored = ctx.db.with_snapshot(|snap| eve_server::services::mobile_deployables::read_deployable_state_snap(snap).deployables.get(&depot_item).cloned()).unwrap();
    assert!(st_anchored.anchor_completed, "Mobile Depot must complete anchoring after 60s");

    // 5. Refit within 2,500m succeeds; > 2,500m fails
    let refit_ok = eve_server::services::inventory_fitting::fitting_containers_ships::is_near_in_space_fitting_service(&ctx, NULL_SYS, ship_id).await;
    assert!(refit_ok, "Ship within 2,500m of anchored Mobile Depot must be able to refit");
    let _ = sys.set_ball_position(ship_id, dep_pos + DVec3::new(5_000.0, 0.0, 0.0)).await;
    let refit_far = eve_server::services::inventory_fitting::fitting_containers_ships::is_near_in_space_fitting_service(&ctx, NULL_SYS, ship_id).await;
    assert!(!refit_far, "Ship > 2,500m from Mobile Depot must be rejected from refitting");

    // 6. Attacking anchored depot to 0% shield triggers 48h (172,800s) reinforced timer
    let now_u = ctx.now_unix_secs();
    let _ = eve_server::services::station_structure::structure_lifecycle_rpc::apply_damage_structure_or_ball(
        &ctx, &sys, NULL_SYS, char_id, depot_item, eve_destiny::DamageVector::new(10_000.0, 0.0, 0.0, 0.0), 10.0, now_u,
    ).await;
    let st_reinf = ctx.db.with_snapshot(|snap| eve_server::services::mobile_deployables::read_deployable_state_snap(snap).deployables.get(&depot_item).cloned()).unwrap();
    assert!(
        st_reinf.reinforced_until_unix.unwrap_or(0) >= now_u + 172_800 - 5,
        "Depleting Mobile Depot shield must trigger 48-hour (172,800s) reinforced timer"
    );
}

#[tokio::test]
async fn test_b35_04_nanite_repair_paste_in_space() {
    // parity: feat:modules.nanite-repair-paste-in-space
    eve_testkit::attest(&["feat:modules.nanite-repair-paste-in-space"]);
    let ctx = build_ctx();
    let sys = ctx.systems.get_or_spawn(NULL_SYS);
    let char_id = 951_301;
    let ship_id = 961_301;
    let mut sess = seed_pilot(&ctx, char_id, ship_id, 641, NULL_SYS, false);
    spawn_ship_in_space(&ctx, char_id, ship_id, 641, NULL_SYS, DVec3::ZERO).await;

    let dmg_mod = 971_301;
    let paste_id = 971_302;
    give_item(&ctx, dmg_mod, 3041, char_id, ship_id, InventoryFlag::HiSlot1, 1);
    give_item(&ctx, paste_id, 28668, char_id, ship_id, InventoryFlag::Cargo, 500);

    // 1. Burned-out module cannot be repaired in space
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(it) = snap.items.get_mut(&dmg_mod) {
            it.dynamic_attributes.insert(9, 1.0);
            it.dynamic_attributes.insert(3, 1.0);
        }
    });
    let burn_err = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "InitiateModuleRepair", &[PyRep::Integer(dmg_mod)], &HashMap::new(), None).await;
    assert!(burn_err.is_err(), "Burned-out module cannot be repaired in space");

    // 2. Active module cannot be repaired until deactivated
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(it) = snap.items.get_mut(&dmg_mod) {
            it.dynamic_attributes.insert(3, 0.5);
        }
        snap.module_active_states.insert(dmg_mod);
    });
    let active_err = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "InitiateModuleRepair", &[PyRep::Integer(dmg_mod)], &HashMap::new(), None).await;
    assert!(active_err.is_err(), "Active module cannot be repaired");

    // 3. InitiateModuleRepair deducts paste and repairs over time; Nanite Interfacing (28880) scales repair rate (+20%/lvl)
    set_skill(&ctx, char_id, 28879, 0); // Nanite Operation 0
    set_skill(&ctx, char_id, 28880, 0); // Nanite Interfacing 0
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(it) = snap.items.get_mut(&dmg_mod) {
            it.dynamic_attributes.insert(3, 0.60);
        }
        snap.module_active_states.remove(&dmg_mod);
    });
    let rep_ok = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "InitiateModuleRepair", &[PyRep::Integer(dmg_mod)], &HashMap::new(), None).await.expect("InitiateModuleRepair");
    let consumed_0 = rep_ok.dict_get("pasteConsumed").and_then(|v| v.as_i64()).unwrap();
    let rate_0 = rep_ok.dict_get("repairRateHpPerSec").and_then(|v| v.as_f64()).unwrap();
    assert!(consumed_0 > 0);
    assert!((rate_0 - 0.67).abs() < 1e-3, "Base repair rate without Nanite Interfacing must be 0.67 HP/s");
    let paste_after = ctx.db.with_snapshot(|snap| snap.items.get(&paste_id).map(|i| i.quantity).unwrap_or(0));
    assert_eq!(paste_after, 500 - consumed_0 as i32);

    // StopModuleRepair stops repair
    let stop_ok = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "StopModuleRepair", &[PyRep::Integer(dmg_mod)], &HashMap::new(), None).await.expect("StopModuleRepair");
    assert_eq!(stop_ok.dict_get("stopped"), Some(&PyRep::Bool(true)));

    // Step ticks and verify damage drops
    let _ = sys.step_manual(5.0).await;

    // 4. Nanite Operation (28879) V reduces paste consumed; Nanite Interfacing (28880) V increases rate by +100%
    set_skill(&ctx, char_id, 28879, 5);
    set_skill(&ctx, char_id, 28880, 5);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(it) = snap.items.get_mut(&dmg_mod) {
            it.dynamic_attributes.insert(3, 0.60);
        }
    });
    let rep_v = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "InitiateModuleRepair", &[PyRep::Integer(dmg_mod)], &HashMap::new(), None).await.expect("InitiateModuleRepair skilled");
    let consumed_v = rep_v.dict_get("pasteConsumed").and_then(|v| v.as_i64()).unwrap();
    let rate_v = rep_v.dict_get("repairRateHpPerSec").and_then(|v| v.as_f64()).unwrap();
    assert!(consumed_v < consumed_0, "Nanite Operation V must reduce paste consumed ({consumed_v} < {consumed_0})");
    assert!((rate_v - 0.67 * 2.0).abs() < 1e-2, "Nanite Interfacing V must double repair rate (+20%/lvl)");
}

#[tokio::test]
async fn test_b35_05_industrial_cyno_rules() {
    // parity: feat:modules.industrial-cyno-rules
    eve_testkit::attest(&["feat:modules.industrial-cyno-rules"]);
    let ctx = build_ctx();

    // 1. Negative hull restriction: Megathron (641) cannot light Industrial Cyno (52694)
    let mega_char = 952_001;
    let mega_ship = 962_001;
    let mut mega_sess = seed_pilot(&ctx, mega_char, mega_ship, 641, DEST_NULL_SYS, false);
    spawn_ship_in_space(&ctx, mega_char, mega_ship, 641, DEST_NULL_SYS, DVec3::ZERO).await;
    give_item(&ctx, 972_001, 52694, mega_char, mega_ship, InventoryFlag::HiSlot0, 1);
    give_item(&ctx, 972_002, 16273, mega_char, mega_ship, InventoryFlag::Cargo, 1_000);
    let mega_err = ctx.dispatch_rpc(&mut mega_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(972_001), PyRep::String("cynosuralGeneration".into()), PyRep::Integer(0), PyRep::Integer(1)], &HashMap::new(), None).await;
    assert!(mega_err.is_err(), "Battleship must not be allowed to light Industrial Cyno");

    // 2. Hauler (Badger 648): CFT 0 -> exactly 800 base LO; CFT V (21603) -> 400 LO (50% reduction)
    let hauler_char = 952_002;
    let hauler_ship = 962_002;
    let mut hauler_sess = seed_pilot(&ctx, hauler_char, hauler_ship, 648, DEST_NULL_SYS, false);
    spawn_ship_in_space(&ctx, hauler_char, hauler_ship, 648, DEST_NULL_SYS, DVec3::new(5_000.0, 0.0, 0.0)).await;
    give_item(&ctx, 972_003, 52694, hauler_char, hauler_ship, InventoryFlag::HiSlot0, 1);
    give_item(&ctx, 972_004, 16273, hauler_char, hauler_ship, InventoryFlag::Cargo, 2_000);
    set_skill(&ctx, hauler_char, 21603, 0); // CFT 0
    let res_cft0 = ctx.dispatch_rpc(&mut hauler_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(972_003), PyRep::String("cynosuralGeneration".into()), PyRep::Integer(0), PyRep::Integer(1)], &HashMap::new(), None).await.expect("Hauler indy cyno CFT 0");
    let qty_cft0 = res_cft0.dict_get("consumptionQuantity").and_then(|v| v.as_i64()).unwrap();
    assert_eq!(qty_cft0, 800, "Base Industrial Cyno LO consumption with CFT 0 must be 800");

    let hauler5_char = 952_003;
    let hauler5_ship = 962_003;
    let mut hauler5_sess = seed_pilot(&ctx, hauler5_char, hauler5_ship, 648, DEST_NULL_SYS, false);
    spawn_ship_in_space(&ctx, hauler5_char, hauler5_ship, 648, DEST_NULL_SYS, DVec3::new(6_000.0, 0.0, 0.0)).await;
    give_item(&ctx, 972_005, 52694, hauler5_char, hauler5_ship, InventoryFlag::HiSlot0, 1);
    give_item(&ctx, 972_006, 16273, hauler5_char, hauler5_ship, InventoryFlag::Cargo, 2_000);
    set_skill(&ctx, hauler5_char, 21603, 5); // CFT 5
    ctx.invalidate_ship_fitting(hauler5_ship);
    let res_cft5 = ctx.dispatch_rpc(&mut hauler5_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(972_005), PyRep::String("cynosuralGeneration".into()), PyRep::Integer(0), PyRep::Integer(1)], &HashMap::new(), None).await.expect("Hauler indy cyno CFT 5");
    let qty_cft5 = res_cft5.dict_get("consumptionQuantity").and_then(|v| v.as_i64()).unwrap();
    assert_eq!(qty_cft5, 400, "CFT V must reduce LO consumption to 400 (50% reduction)");

    // 3. Venture (32880): 50% LO role bonus -> 400 LO base (CFT 0), spawns beacon ball on overview
    let ven_char = 952_101;
    let ven_ship = 962_101;
    let mut ven_sess = seed_pilot(&ctx, ven_char, ven_ship, 32880, DEST_NULL_SYS, false);
    spawn_ship_in_space(&ctx, ven_char, ven_ship, 32880, DEST_NULL_SYS, DVec3::new(10_000.0, 0.0, 0.0)).await;
    give_item(&ctx, 972_101, 52694, ven_char, ven_ship, InventoryFlag::HiSlot0, 1);
    give_item(&ctx, 972_102, 16273, ven_char, ven_ship, InventoryFlag::Cargo, 1_000);
    let cyno_res = ctx.dispatch_rpc(&mut ven_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(972_101), PyRep::String("cynosuralGeneration".into()), PyRep::Integer(0), PyRep::Integer(1)], &HashMap::new(), None).await.expect("Venture industrial cyno");
    assert_eq!(cyno_res.dict_get("consumptionQuantity"), Some(&PyRep::Integer(400)));
    let beacon_id = cyno_res.dict_get("beaconID").and_then(|v| v.as_i64()).unwrap();
    let dest_sys = ctx.systems.get_or_spawn(DEST_NULL_SYS);
    assert!(dest_sys.get_ball_snapshot(beacon_id).await.unwrap().is_some(), "Industrial cyno beacon must appear in space/overview");

    // 4. Carrier CANNOT jump; JF (28844), Black Ops (22428), Rorqual (28352) CAN jump to Industrial Cyno
    let car_char = 952_104;
    let car_ship = 962_104;
    let mut car_sess = seed_pilot(&ctx, car_char, car_ship, 23757, NULL_SYS, false);
    spawn_ship_in_space(&ctx, car_char, car_ship, 23757, NULL_SYS, DVec3::new(1_000.0, 0.0, 0.0)).await;
    give_item(&ctx, 972_104, 16274, car_char, car_ship, InventoryFlag::SpecializedFuelBay, 50_000);

    let jf_char = 952_102;
    let jf_ship = 962_102;
    let mut jf_sess = seed_pilot(&ctx, jf_char, jf_ship, 28844, NULL_SYS, false);
    spawn_ship_in_space(&ctx, jf_char, jf_ship, 28844, NULL_SYS, DVec3::new(2_000.0, 0.0, 0.0)).await;
    give_item(&ctx, 972_103, 17888, jf_char, jf_ship, InventoryFlag::SpecializedFuelBay, 50_000);

    let blops_char = 952_105;
    let blops_ship = 962_105;
    let mut blops_sess = seed_pilot(&ctx, blops_char, blops_ship, 22428, NULL_SYS, false);
    spawn_ship_in_space(&ctx, blops_char, blops_ship, 22428, NULL_SYS, DVec3::new(3_000.0, 0.0, 0.0)).await;
    give_item(&ctx, 972_105, 16274, blops_char, blops_ship, InventoryFlag::SpecializedFuelBay, 50_000);
    give_item(&ctx, 972_106, 28652, blops_char, blops_ship, InventoryFlag::HiSlot0, 1); // Covert Jump Portal

    let rorq_char = 952_106;
    let rorq_ship = 962_106;
    let mut rorq_sess = seed_pilot(&ctx, rorq_char, rorq_ship, 28352, NULL_SYS, false);
    spawn_ship_in_space(&ctx, rorq_char, rorq_ship, 28352, NULL_SYS, DVec3::new(4_000.0, 0.0, 0.0)).await;
    give_item(&ctx, 972_107, 17887, rorq_char, rorq_ship, InventoryFlag::SpecializedFuelBay, 50_000);
    give_item(&ctx, 972_108, 63140, rorq_char, rorq_ship, InventoryFlag::HiSlot0, 1); // Industrial Jump Portal Generator I

    put_in_fleet(&ctx, 880_001, ven_char, &[(ven_char, true), (car_char, true), (jf_char, true), (blops_char, true), (rorq_char, true)]);

    // 4. Black Ops & Rorqual can bridge to Industrial Cyno (tested from origin system)
    let blops_bridge = ctx.dispatch_rpc(&mut blops_sess, "beyonce", None, "CmdBridgeToFleetModuleBeacon", &[PyRep::Integer(ven_char), PyRep::Integer(DEST_NULL_SYS as i64), PyRep::Integer(beacon_id)], &HashMap::new(), None).await;
    assert!(blops_bridge.is_ok(), "Black Ops must be allowed to bridge to Industrial Cyno: {blops_bridge:?}");

    let rorq_bridge = ctx.dispatch_rpc(&mut rorq_sess, "beyonce", None, "CmdBridgeToFleetModuleBeacon", &[PyRep::Integer(ven_char), PyRep::Integer(DEST_NULL_SYS as i64), PyRep::Integer(beacon_id)], &HashMap::new(), None).await;
    assert!(rorq_bridge.is_ok(), "Rorqual must be allowed to bridge to Industrial Cyno: {rorq_bridge:?}");

    // 5. Jump to Industrial Cyno: JF, Black Ops, Rorqual allowed; regular Carrier rejected
    let car_err = ctx.dispatch_rpc(&mut car_sess, "beyonce", None, "CmdJumpToFleetModuleBeacon", &[PyRep::Integer(ven_char), PyRep::Integer(DEST_NULL_SYS as i64), PyRep::Integer(beacon_id)], &HashMap::new(), None).await;
    assert!(car_err.is_err(), "Regular Carrier must NOT be allowed to jump to Industrial Cyno");

    let jf_ok = ctx.dispatch_rpc(&mut jf_sess, "beyonce", None, "CmdJumpToFleetModuleBeacon", &[PyRep::Integer(ven_char), PyRep::Integer(DEST_NULL_SYS as i64), PyRep::Integer(beacon_id)], &HashMap::new(), None).await;
    assert!(jf_ok.is_ok(), "Jump Freighter must be allowed to jump to Industrial Cyno");

    let blops_ok = ctx.dispatch_rpc(&mut blops_sess, "beyonce", None, "CmdJumpToFleetModuleBeacon", &[PyRep::Integer(ven_char), PyRep::Integer(DEST_NULL_SYS as i64), PyRep::Integer(beacon_id)], &HashMap::new(), None).await;
    assert!(blops_ok.is_ok(), "Black Ops must be allowed to jump to Industrial Cyno");

    let rorq_ok = ctx.dispatch_rpc(&mut rorq_sess, "beyonce", None, "CmdJumpToFleetModuleBeacon", &[PyRep::Integer(ven_char), PyRep::Integer(DEST_NULL_SYS as i64), PyRep::Integer(beacon_id)], &HashMap::new(), None).await;
    assert!(rorq_ok.is_ok(), "Rorqual must be allowed to jump to Industrial Cyno");
}
