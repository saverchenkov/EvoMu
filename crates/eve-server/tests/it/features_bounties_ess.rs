//! Dedicated integration tests for open `feat:modules.*` parity rows (Batch 35, Part 1).

use crate::modules_advanced_tactical::{build_ctx, seed_pilot, NULL_SYS};
use eve_db::{
    FleetMemberRecord, FleetRecord, InventoryFlag, InventoryItemRecord, TrainedSkillRecord,
    UpwellStructureRecord,
};
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
                singleton: qty == 1
                    && !matches!(flag, InventoryFlag::Cargo | InventoryFlag::SpecializedFuelBay),
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });
    if (flag as u16) >= 11 {
        ctx.invalidate_ship_fitting(location_id);
    }
}

pub(crate) fn set_skill(
    ctx: &eve_server::router::ServerContext,
    char_id: i64,
    skill_type_id: u32,
    level: u8,
) {
    let mut active_ship = None;
    ctx.db.with_mut_snapshot(|snap| {
        active_ship = snap.characters.get(&char_id).map(|c| c.active_ship_id);
        snap.skills.entry(char_id).or_default().insert(
            skill_type_id,
            TrainedSkillRecord {
                type_id: skill_type_id,
                level,
                skill_points: 256_000.0,
            },
        );
    });
    if let Some(sid) = active_ship {
        ctx.invalidate_ship_fitting(sid);
    }
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

pub(crate) fn put_in_fleet(
    ctx: &eve_server::router::ServerContext,
    fleet_id: i64,
    boss_id: i64,
    members: &[(i64, bool)],
) {
    ctx.db.with_mut_snapshot(|snap| {
        for fl in snap.fleets.values_mut() {
            for &(cid, _) in members {
                fl.members.retain(|&m| m != cid);
                fl.member_details.remove(&cid);
            }
        }
        snap.fleets.retain(|_, fl| !fl.members.is_empty());
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
                    role: if cid == boss_id {
                        eve_db::FleetRole::FleetCommander
                    } else {
                        eve_db::FleetRole::SquadMember
                    },
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
    let sys = ctx.systems.get_or_spawn(NULL_SYS);
    let kw = HashMap::new();

    // Negative/abuse cases (>= 2): unauthenticated session & subcapital hull rejected
    let mut unauth = eve_machonet::SessionState::new(1);
    let dir_arg = PyRep::Tuple(vec![PyRep::Real(1.0), PyRep::Real(0.0), PyRep::Real(0.0)]);
    assert!(
        ctx.dispatch_rpc(&mut unauth, "superWeaponMgr", None, "ActivateSlashModule", &[PyRep::Integer(1), dir_arg.clone()], &kw, None)
            .await
            .is_err(),
        "Unauthenticated ActivateSlashModule must fail"
    );
    let subcap_char = 950_090;
    let subcap_ship = 960_090;
    let mut subcap_sess = seed_pilot(&ctx, subcap_char, subcap_ship, 641, NULL_SYS, false);
    spawn_ship_in_space(&ctx, subcap_char, subcap_ship, 641, NULL_SYS, DVec3::new(0.0, 20_000.0, 0.0)).await;
    give_item(&ctx, 970_090, 40631, subcap_char, subcap_ship, InventoryFlag::HiSlot0, 1);
    assert!(
        ctx.dispatch_rpc(&mut subcap_sess, "superWeaponMgr", None, "ActivateSlashModule", &[PyRep::Integer(970_090), dir_arg.clone()], &kw, None)
            .await
            .is_err(),
        "Subcapital hull must not be allowed to activate a Titan Lance Doomsday"
    );

    let titan_char = 950_101;
    let titan_ship = 960_101;
    let mut titan_sess = seed_pilot(&ctx, titan_char, titan_ship, 11567, NULL_SYS, false); // Avatar
    spawn_ship_in_space(&ctx, titan_char, titan_ship, 11567, NULL_SYS, DVec3::ZERO).await;
    let titan_beyonce = ctx.bind_object(&mut titan_sess, "beyonce", NULL_SYS as i64).unwrap();

    let lance_mod = 970_104;
    give_item(&ctx, lance_mod, 40631, titan_char, titan_ship, InventoryFlag::HiSlot0, 1);
    // Negative: insufficient fuel rejected before consuming or locking
    assert!(
        ctx.dispatch_rpc(&mut titan_sess, "superWeaponMgr", None, "ActivateSlashModule", &[PyRep::Integer(lance_mod), dir_arg.clone()], &kw, None)
            .await
            .is_err(),
        "ActivateSlashModule without topes must fail"
    );
    give_item(&ctx, 970_105, 16274, titan_char, titan_ship, InventoryFlag::Cargo, 500_000);

    // Victims: 200km in beam cone (Archon capital so it survives with heavy damage to test post-hit suppression),
    // plus a fragile Rifter at 150km in beam cone (to verify killmail assist/destruction at t=15s),
    // plus 350km (beyond 300km) and off-axis (outside cone)
    let v_200_char = 950_203;
    let v_200km = 960_203;
    let mut v_200_sess = seed_pilot(&ctx, v_200_char, v_200km, 23757, NULL_SYS, false); // Archon
    ctx.db.with_mut_snapshot(|s| {
        let mut acc = s.accounts.values().next().cloned().unwrap();
        acc.user_id = v_200_char as u64;
        acc.username = format!("u_{v_200_char}");
        s.accounts.insert(acc.username.clone(), acc);
        s.characters.get_mut(&v_200_char).unwrap().user_id = v_200_char as u64;
    });
    v_200_sess.user_id = Some(v_200_char as u64);
    give_item(&ctx, 970_204, 31796, v_200_char, v_200km, InventoryFlag::MedSlot0, 1);
    give_item(&ctx, 970_205, 31796, v_200_char, v_200km, InventoryFlag::MedSlot1, 1);
    spawn_ship_in_space(&ctx, v_200_char, v_200km, 23757, NULL_SYS, DVec3::new(200_000.0, 0.0, 0.0)).await;
    give_item(&ctx, 970_203, 16274, v_200_char, v_200km, InventoryFlag::SpecializedFuelBay, 50_000);
    let v_200_beyonce = ctx.bind_object(&mut v_200_sess, "beyonce", NULL_SYS as i64).unwrap();

    // Friendly online Upwell Astrahus (35832) right next to v_200km to verify Lance breaks & blocks tether
    let astra_id = 980_203;
    let v_corp = ctx.db.with_snapshot(|s| s.characters.get(&v_200_char).map(|c| c.corporation_id).unwrap_or(1000009));
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            astra_id,
            UpwellStructureRecord {
                structure_id: astra_id,
                type_id: 35832,
                owner_corp_id: v_corp,
                solar_system_id: NULL_SYS,
                name: "Test Astrahus".into(),
                position: [200_500.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                shield_hp: 5_000.0,
                armor_hp: 5_000.0,
                hull_hp: 5_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
    });

    let v_150_char = 950_206;
    let v_150km = 960_206;
    seed_pilot(&ctx, v_150_char, v_150km, 587, NULL_SYS, false); // Rifter (destroyed by beam at t=15s)
    spawn_ship_in_space(&ctx, v_150_char, v_150km, 587, NULL_SYS, DVec3::new(150_000.0, 0.0, 0.0)).await;

    let v_350km = 960_204;
    let v_off_axis = 960_205;
    seed_pilot(&ctx, 950_204, v_350km, 641, NULL_SYS, false);
    seed_pilot(&ctx, 950_205, v_off_axis, 641, NULL_SYS, false);
    spawn_ship_in_space(&ctx, 950_204, v_350km, 641, NULL_SYS, DVec3::new(350_000.0, 0.0, 0.0)).await;
    spawn_ship_in_space(&ctx, 950_205, v_off_axis, 641, NULL_SYS, DVec3::new(100_000.0, 50_000.0, 0.0)).await;

    // Fleet cyno in DEST_NULL_SYS so we can test live CmdJumpToFleetModuleBeacon rejection on both Titan and hit victim
    let cyno_char = 950_209;
    let cyno_ship = 960_209;
    let mut cyno_sess = seed_pilot(&ctx, cyno_char, cyno_ship, 11957, DEST_NULL_SYS, false);
    spawn_ship_in_space(&ctx, cyno_char, cyno_ship, 11957, DEST_NULL_SYS, DVec3::new(10_000.0, 0.0, 0.0)).await;
    give_item(&ctx, 970_209, 21096, cyno_char, cyno_ship, InventoryFlag::HiSlot0, 1);
    give_item(&ctx, 970_210, 16273, cyno_char, cyno_ship, InventoryFlag::Cargo, 2_000);
    put_in_fleet(&ctx, 880_101, titan_char, &[(titan_char, true), (v_200_char, true), (cyno_char, true)]);
    let cyno_res = ctx
        .dispatch_rpc(&mut cyno_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(970_209), PyRep::String("cynosuralGeneration".into()), PyRep::Integer(0), PyRep::Integer(1)], &kw, None)
        .await
        .expect("Light cyno");
    let beacon_id = cyno_res.dict_get("beaconID").and_then(|v| v.as_i64()).unwrap();

    let cs_200_init = sys.get_combat_state(v_200km).await.unwrap().unwrap();
    let cs_350_init = sys.get_combat_state(v_350km).await.unwrap().unwrap();
    let cs_off_init = sys.get_combat_state(v_off_axis).await.unwrap().unwrap();
    let titan_uid = ctx.db.with_snapshot(|s| s.characters.get(&titan_char).unwrap().user_id);
    ctx.db
        .acquire_online_lock_with_computer_hash(titan_uid, titan_char as u64, Some(titan_char), None, true, 1)
        .expect("register titan online session");
    ctx.db.acquire_online_lock_with_computer_hash(v_200_char as u64, v_200_char as u64, Some(v_200_char), None, true, 1).expect("register v_200 online");
    let _ = ctx.drain_notifications_for_char(titan_char);
    let _ = ctx.drain_notifications_for_char(v_200_char);
    let _ = ctx.drain_notifications_for_char(v_150_char);

    let origin_arg = PyRep::Tuple(vec![PyRep::Real(0.0), PyRep::Real(0.0), PyRep::Real(0.0)]);
    // Negative: Titan AoE Lance rejected in Low-Sec (0.0 < sys_sec < 0.45) and non-finite direction vectors (NaN / INFINITY) rejected before consuming fuel
    let low_sys = ctx.sde.solar_systems.values().find(|s| s.security_status > 0.05 && s.security_status < 0.45).map(|s| s.solar_system_id).unwrap();
    ctx.db.with_mut_snapshot(|s| s.characters.get_mut(&titan_char).unwrap().solar_system_id = low_sys);
    assert_eq!(
        ctx.dispatch_rpc(&mut titan_sess, "superWeaponMgr", None, "ActivateSlashModule", &[PyRep::Integer(lance_mod), origin_arg.clone(), dir_arg.clone()], &kw, None).await.unwrap_err().code,
        "CannotActivateLanceInLowSec"
    );
    ctx.db.with_mut_snapshot(|s| s.characters.get_mut(&titan_char).unwrap().solar_system_id = NULL_SYS);
    let nan_dir = PyRep::Tuple(vec![PyRep::Real(f64::NAN), PyRep::Real(0.0), PyRep::Real(0.0)]);
    let inf_dir = PyRep::Tuple(vec![PyRep::Real(f64::INFINITY), PyRep::Real(0.0), PyRep::Real(0.0)]);
    assert_eq!(ctx.dispatch_rpc(&mut titan_sess, "superWeaponMgr", None, "ActivateSlashModule", &[PyRep::Integer(lance_mod), origin_arg.clone(), nan_dir], &kw, None).await.unwrap_err().code, "InvalidDirectionVector");
    assert_eq!(ctx.dispatch_rpc(&mut titan_sess, "superWeaponMgr", None, "ActivateSlashModule", &[PyRep::Integer(lance_mod), origin_arg.clone(), inf_dir], &kw, None).await.unwrap_err().code, "InvalidDirectionVector");
    assert_eq!(ctx.db.with_snapshot(|s| s.items.get(&970_105).unwrap().quantity), 500_000, "Rejected ActivateSlashModule calls must not consume any isotopes");

    let lance_res = ctx
        .dispatch_rpc(&mut titan_sess, "superWeaponMgr", None, "ActivateSlashModule", &[PyRep::Integer(lance_mod), origin_arg, dir_arg], &kw, None)
        .await
        .expect("ActivateSlashModule");
    assert_eq!(lance_res.dict_get("warmupSeconds"), Some(&PyRep::Real(15.0)));
    assert_eq!(lance_res.dict_get("jumpBlockSeconds"), Some(&PyRep::Real(300.0)));
    assert_eq!(lance_res.dict_get("tickCount"), Some(&PyRep::Integer(15)));

    // At t = 0 (before 15s warmup completes): ZERO OnDamageMessage / OnDamageMessages may be emitted!
    let mut t0_notifs = ctx.drain_notifications_for_char(titan_char);
    t0_notifs.extend(ctx.drain_notifications_for_char(v_200_char));
    t0_notifs.extend(ctx.drain_notifications_for_char(v_150_char));
    assert!(
        !t0_notifs.iter().any(|n| n.event == "OnDamageMessage" || n.event == "OnDamageMessages"),
        "OnDamageMessage must NOT fire at t=0 before the 15s Lance warmup completes"
    );

    // During 15s warmup: Titan cannot move, steer, align, orbit, warp, or jump via live RPCs
    let gate_id = ctx.sde.stargates().values().find(|j| j.from_solar_system_id == NULL_SYS).map(|j| j.stargate_id).unwrap();
    assert!(ctx.dispatch_rpc(&mut titan_sess, "beyonce", Some(titan_beyonce), "CmdGotoDirection", &[PyRep::Real(0.0), PyRep::Real(1.0), PyRep::Real(0.0)], &kw, None).await.is_err());
    assert!(ctx.dispatch_rpc(&mut titan_sess, "beyonce", Some(titan_beyonce), "CmdSteerDirection", &[PyRep::Real(0.0), PyRep::Real(1.0), PyRep::Real(0.0)], &kw, None).await.is_err());
    assert!(ctx.dispatch_rpc(&mut titan_sess, "beyonce", Some(titan_beyonce), "CmdGotoPoint", &[PyRep::Real(5_000.0), PyRep::Real(0.0), PyRep::Real(0.0)], &kw, None).await.is_err());
    assert!(ctx.dispatch_rpc(&mut titan_sess, "beyonce", Some(titan_beyonce), "CmdSetSpeedFraction", &[PyRep::Real(1.0)], &kw, None).await.is_err());
    assert!(ctx.dispatch_rpc(&mut titan_sess, "beyonce", Some(titan_beyonce), "CmdOrbit", &[PyRep::Integer(v_200km), PyRep::Real(2_500.0)], &kw, None).await.is_err());
    assert!(ctx.dispatch_rpc(&mut titan_sess, "beyonce", Some(titan_beyonce), "CmdAlignTo", &[PyRep::Integer(gate_id)], &kw, None).await.is_err());
    assert!(ctx.dispatch_rpc(&mut titan_sess, "beyonce", Some(titan_beyonce), "CmdWarpToStuff", &[PyRep::String("item".into()), PyRep::Integer(gate_id), PyRep::Real(0.0)], &kw, None).await.is_err());
    assert!(ctx.dispatch_rpc(&mut titan_sess, "beyonce", None, "CmdJumpToFleetModuleBeacon", &[PyRep::Integer(cyno_char), PyRep::Integer(DEST_NULL_SYS as i64), PyRep::Integer(beacon_id)], &kw, None).await.is_err());

    // Step 12 ticks (< 15s even with 1Hz background actor): warmup still running -> 0 damage
    for _ in 0..12 {
        let _ = sys.try_step_full_tick(1.0);
    }
    let cs_200_mid = sys.get_combat_state(v_200km).await.unwrap().unwrap();
    assert_eq!(cs_200_mid.shield_hp, cs_200_init.shield_hp, "Damage must not land before 15s warmup completes");

    // Step remaining ticks to cross t = 15s warmup + beam ticks -> beam fires!
    for _ in 0..10 {
        let _ = sys.try_step_full_tick(1.0);
    }
    let (mut titan_notifs, mut victim_notifs) = (Vec::new(), Vec::new());
    for _ in 0..500 {
        titan_notifs.extend(ctx.drain_notifications_for_char(titan_char));
        victim_notifs.extend(ctx.drain_notifications_for_char(v_200_char));
        if titan_notifs.iter().any(|n| n.event == "OnDamageMessage" && n.args.len() == 1)
            && victim_notifs.iter().any(|n| n.event == "OnDamageMessage" && n.args.len() == 1)
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(titan_notifs.iter().any(|n| n.event == "OnDamageMessage" && n.args.len() == 1), "Attacker OnDamageMessage must be emitted at t=15s");
    assert!(victim_notifs.iter().any(|n| n.event == "OnDamageMessage" && n.args.len() == 1), "Hit player victim must receive incoming OnDamageMessage at t=15s");

    // At t = 90s (> 60s weapons timer, < 300s caster lock): Titan caster STILL cannot tether or maneuver!
    ctx.advance_time_seconds(75);
    let _ = sys.set_ball_position(titan_ship, DVec3::new(200_500.0, 0.0, 0.0)).await;
    eve_server::services::space_destiny::space_nav_travel::sync_tether(&ctx, titan_char, NULL_SYS, titan_ship).await;
    assert!(!sys.get_combat_state(titan_ship).await.unwrap().unwrap().is_tethered, "Titan caster must remain tether-locked for the full 300s after firing Lance");
    assert!(ctx.dispatch_rpc(&mut titan_sess, "beyonce", Some(titan_beyonce), "CmdGotoDirection", &[PyRep::Real(0.0), PyRep::Real(1.0), PyRep::Real(0.0)], &kw, None).await.is_err(), "Titan caster must remain subwarp-immobilized during 300s lock");
    assert!(
        ctx.db.with_snapshot(|s| s.killmails.values().any(|k| k.victim_character_id == v_150_char && k.attacker_character_id == titan_char)),
        "Lance beam kill at t=15s must record Titan attacker on killmail"
    );

    let cs_200_after = sys.get_combat_state(v_200km).await.unwrap().unwrap();
    let cs_350_after = sys.get_combat_state(v_350km).await.unwrap().unwrap();
    let cs_off_after = sys.get_combat_state(v_off_axis).await.unwrap().unwrap();
    assert!(cs_200_after.shield_hp < cs_200_init.shield_hp, "200km ship inside 200km Lance cylinder must take damage across 15s beam ticks");
    assert_eq!(cs_350_after.shield_hp, cs_350_init.shield_hp, "350km ship beyond 200km Lance range must not be hit");
    assert_eq!(cs_off_after.shield_hp, cs_off_init.shield_hp, "Off-axis ship beyond 2.5km beam radius must not be hit");

    // Step another tick: Lance Doomsday does NOT warp-scramble victims (only caster is locked for 300s)
    let _ = sys.try_step_full_tick(1.0);
    assert!(!sys.is_ball_scrambled(v_200km).await.unwrap(), "Hit victim must NOT be warp-scrambled by standard Lance Doomsday");
    ctx.dispatch_rpc(&mut v_200_sess, "beyonce", Some(v_200_beyonce), "CmdWarpToStuff", &[PyRep::String("item".into()), PyRep::Integer(gate_id), PyRep::Real(0.0)], &kw, None).await.expect("Hit victim can warp freely");
    assert!(ctx.dispatch_rpc(&mut titan_sess, "beyonce", Some(titan_beyonce), "CmdWarpToStuff", &[PyRep::String("item".into()), PyRep::Integer(gate_id), PyRep::Real(0.0)], &kw, None).await.is_err(), "Titan caster cannot warp during 300s lock");
}

#[tokio::test]
async fn test_b35_02_energy_nosferatu_relative_capacitor() {
    // parity: feat:modules.energy-nosferatu-relative-capacitor
    eve_testkit::attest(&["feat:modules.energy-nosferatu-relative-capacitor"]);
    let ctx = build_ctx();
    let sys = ctx.systems.get_or_spawn(NULL_SYS);
    let kw = HashMap::new();

    // Negative/abuse cases (>= 2): unauthenticated session & unlocked target rejected
    let mut unauth = eve_machonet::SessionState::new(1);
    assert!(ctx.dispatch_rpc(&mut unauth, "dogmaIM", None, "Activate", &[PyRep::Integer(1)], &kw, None).await.is_err());

    let c_norm = 951_101;
    let s_norm = 961_101;
    let mut sess_norm = seed_pilot(&ctx, c_norm, s_norm, 641, NULL_SYS, false); // Megathron
    spawn_ship_in_space(&ctx, c_norm, s_norm, 641, NULL_SYS, DVec3::ZERO).await;
    set_skill(&ctx, c_norm, 3336, 5);
    set_skill(&ctx, c_norm, 3423, 5); // Capacitor Emission Systems
    let nos_mod = 971_101;
    give_item(&ctx, nos_mod, 12261, c_norm, s_norm, InventoryFlag::HiSlot0, 1);

    // Identical idle control Megathron at exact same initial capacitor to isolate Nosferatu capacitor transfer from passive recharge
    let c_ctrl = 951_109;
    let s_ctrl = 961_109;
    seed_pilot(&ctx, c_ctrl, s_ctrl, 641, NULL_SYS, false);
    spawn_ship_in_space(&ctx, c_ctrl, s_ctrl, 641, NULL_SYS, DVec3::new(0.0, 5_000.0, 0.0)).await;

    let c_tgt = 951_102;
    let s_tgt = 961_102;
    seed_pilot(&ctx, c_tgt, s_tgt, 587, NULL_SYS, false); // Rifter
    spawn_ship_in_space(&ctx, c_tgt, s_tgt, 587, NULL_SYS, DVec3::new(5_000.0, 0.0, 0.0)).await;

    // Negative: activating Nosferatu on unlocked target fails
    assert!(
        ctx.dispatch_rpc(&mut sess_norm, "dogmaIM", None, "Activate", &[PyRep::Integer(nos_mod), PyRep::String("energyDestabilizationNew".into()), PyRep::Integer(s_tgt), PyRep::Integer(1)], &kw, None)
            .await
            .is_err(),
        "Nosferatu activation on unlocked target must fail"
    );

    let set_caps = | norm_cap: f64, ctrl_cap: f64, tgt_cap: f64 | {
        let sys = sys.clone();
        async move {
            for (sid, max_c, cur_c) in [(s_norm, 5_000.0, norm_cap), (s_ctrl, 5_000.0, ctrl_cap), (s_tgt, 300.0, tgt_cap)] {
                let mut cs = sys.get_combat_state(sid).await.unwrap().unwrap();
                cs.max_capacitor = max_c;
                cs.capacitor = cur_c;
                sys.set_combat_state(sid, cs).await.unwrap();
            }
        }
    };

    // 1. Higher cap % (80% > 50%): cycles without draining target or transferring above idle control
    set_caps(4_000.0, 4_000.0, 150.0).await;
    let _ = sys.add_target_lock(s_norm, eve_destiny::TargetLock { target_id: s_tgt, locked: true, remaining_lock_ms: 0.0, lock_time_ms: 0.0, max_range_m: Some(100_000.0) }).await;
    ctx.dispatch_rpc(&mut sess_norm, "dogmaIM", None, "Activate", &[PyRep::Integer(nos_mod), PyRep::String("energyDestabilizationNew".into()), PyRep::Integer(s_tgt), PyRep::Integer(1)], &kw, None).await.expect("Activate Nosferatu");
    let _ = sys.step_manual(15.0).await;
    let tgt_after_high = sys.get_combat_state(s_tgt).await.unwrap().unwrap();
    let src_after_high = sys.get_combat_state(s_norm).await.unwrap().unwrap();
    let ctrl_after_high = sys.get_combat_state(s_ctrl).await.unwrap().unwrap();
    assert!(tgt_after_high.capacitor >= 150.0, "Higher cap % (80% > 50%) must not drain");
    assert!((src_after_high.capacitor - ctrl_after_high.capacitor).abs() < 1e-3, "Higher cap % must transfer 0 GJ to activator");

    // 2. Equal cap % (100% == 100%): must not drain
    set_caps(5_000.0, 5_000.0, 300.0).await;
    let _ = sys.step_manual(15.0).await;
    let tgt_after_eq = sys.get_combat_state(s_tgt).await.unwrap().unwrap();
    assert!(tgt_after_eq.capacitor >= 300.0 - 1e-3, "Equal cap % (100% == 100%) must not drain");

    // 3a. Lower cap % (30% < 50%, but 1500 GJ > 150 GJ): EVE checks absolute GJ, so must NOT drain!
    set_caps(1_500.0, 1_500.0, 150.0).await;
    let _ = sys.step_manual(15.0).await;
    assert!(sys.get_combat_state(s_tgt).await.unwrap().unwrap().capacitor >= 150.0 - 1e-3, "Higher absolute GJ (1500 GJ > 150 GJ) must not drain even at lower %");
    assert!((sys.get_combat_state(s_norm).await.unwrap().unwrap().capacitor - sys.get_combat_state(s_ctrl).await.unwrap().unwrap().capacitor).abs() < 1e-3);

    // 3b. Strictly lower absolute GJ (400 GJ = 80% < 950 GJ = 95%): drains target AND transfers capacitor to activating ship!
    for (sid, max_c, cur_c) in [(s_norm, 500.0, 400.0), (s_ctrl, 500.0, 400.0), (s_tgt, 1_000.0, 950.0)] {
        let mut cs = sys.get_combat_state(sid).await.unwrap().unwrap();
        cs.capacitor_recharge_time_ms = 0.0;
        cs.max_capacitor = max_c;
        cs.capacitor = cur_c;
        sys.set_combat_state(sid, cs).await.unwrap();
    }
    let _ = sys.step_manual(15.0).await;
    assert!(sys.get_combat_state(s_tgt).await.unwrap().unwrap().capacitor < 950.0, "Lower absolute GJ (400 GJ < 950 GJ) must drain target");
    assert!(sys.get_combat_state(s_norm).await.unwrap().unwrap().capacitor > sys.get_combat_state(s_ctrl).await.unwrap().unwrap().capacitor + 5.0);

    // 4. Sleeper NPC target (Sleepless Patroller 30188): standard hull transfers cap even at 80% > 50% without draining Sleeper cap
    let _ = sys.remove_module_cycle(s_norm, nos_mod).await;
    let sleeper_id = 700_301_880;
    let _ = sys.spawn_ball_with_meta(
        Ball {
            id: sleeper_id,
            type_id: 30188,
            position: DVec3::new(4_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 200.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 100.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
        eve_destiny::CombatState {
            max_capacitor: 1_000.0,
            capacitor: 500.0, // 50%
            capacitor_recharge_time_ms: 100_000_000.0,
            ..Default::default()
        },
        eve_destiny::SlimItemMeta { group_id: 959, category_id: 11, ..Default::default() },
    ).await;
    set_caps(4_000.0, 4_000.0, 150.0).await; // s_norm at 80% > Sleeper 50%
    let _ = sys.add_target_lock(s_norm, eve_destiny::TargetLock { target_id: sleeper_id, locked: true, remaining_lock_ms: 0.0, lock_time_ms: 0.0, max_range_m: Some(100_000.0) }).await;
    ctx.dispatch_rpc(&mut sess_norm, "dogmaIM", None, "Activate", &[PyRep::Integer(nos_mod), PyRep::String("energyDestabilizationNew".into()), PyRep::Integer(sleeper_id), PyRep::Integer(1)], &kw, None).await.expect("Activate Nosferatu on Sleeper");
    let _ = sys.step_manual(15.0).await;
    let sleeper_after = sys.get_combat_state(sleeper_id).await.unwrap().unwrap();
    let src_after_slp = sys.get_combat_state(s_norm).await.unwrap().unwrap();
    let ctrl_after_slp = sys.get_combat_state(s_ctrl).await.unwrap().unwrap();
    assert!(sleeper_after.capacitor >= 500.0 - 1e-3, "Sleeper NPC capacitor must not be depleted");
    assert!(src_after_slp.capacitor > ctrl_after_slp.capacitor + 5.0, "Nosferatu against Sleeper NPC must transfer capacitor even above target %");
    let _ = sys.remove_module_cycle(s_norm, nos_mod).await;

    // 5. Blood Raider hull (Bhaalgorn 17920) at 85% > 50%: drains target AND gains capacitor above idle Bhaalgorn control
    let c_br = 951_103;
    let s_br = 961_103;
    let mut sess_br = seed_pilot(&ctx, c_br, s_br, 17920, NULL_SYS, false);
    spawn_ship_in_space(&ctx, c_br, s_br, 17920, NULL_SYS, DVec3::new(1_000.0, 0.0, 0.0)).await;
    set_skill(&ctx, c_br, 3332, 5);
    set_skill(&ctx, c_br, 3335, 5);
    set_skill(&ctx, c_br, 3423, 5);
    let nos_br = 971_102;
    give_item(&ctx, nos_br, 12261, c_br, s_br, InventoryFlag::HiSlot0, 1);

    let c_br_ctrl = 951_104;
    let s_br_ctrl = 961_104;
    seed_pilot(&ctx, c_br_ctrl, s_br_ctrl, 17920, NULL_SYS, false);
    spawn_ship_in_space(&ctx, c_br_ctrl, s_br_ctrl, 17920, NULL_SYS, DVec3::new(1_000.0, 5_000.0, 0.0)).await;

    for (sid, max_c, cur_c) in [(s_br, 5_000.0, 4_250.0), (s_br_ctrl, 5_000.0, 4_250.0), (s_tgt, 300.0, 150.0)] {
        let mut cs = sys.get_combat_state(sid).await.unwrap().unwrap();
        cs.max_capacitor = max_c;
        cs.capacitor = cur_c;
        sys.set_combat_state(sid, cs).await.unwrap();
    }
    let _ = sys.add_target_lock(s_br, eve_destiny::TargetLock { target_id: s_tgt, locked: true, remaining_lock_ms: 0.0, lock_time_ms: 0.0, max_range_m: Some(100_000.0) }).await;
    ctx.dispatch_rpc(&mut sess_br, "dogmaIM", None, "Activate", &[PyRep::Integer(nos_br), PyRep::String("energyDestabilizationNew".into()), PyRep::Integer(s_tgt), PyRep::Integer(1)], &kw, None).await.expect("Activate Bhaalgorn Nosferatu");
    let _ = sys.step_manual(15.0).await;
    let tgt_after_br = sys.get_combat_state(s_tgt).await.unwrap().unwrap();
    let br_after = sys.get_combat_state(s_br).await.unwrap().unwrap();
    let br_ctrl_after = sys.get_combat_state(s_br_ctrl).await.unwrap().unwrap();
    assert!(tgt_after_br.capacitor < 150.0, "Blood Raider hull must override cap % restriction and drain target");
    assert!(br_after.capacitor > br_ctrl_after.capacitor + 5.0, "Blood Raider hull must gain positive capacitor from Nosferatu drain");
}

#[tokio::test]
async fn test_b35_03_mobile_depot_deployment_and_refitting() {
    let ctx = build_ctx();
    let sys = ctx.systems.get_or_spawn(NULL_SYS);
    let kw = HashMap::new();

    // Negative/abuse case 1: unauthenticated posMgr.AnchorStructure rejected
    let mut unauth = eve_machonet::SessionState::new(1);
    assert!(ctx.dispatch_rpc(&mut unauth, "posMgr", None, "AnchorStructure", &[PyRep::Integer(1)], &kw, None).await.is_err());

    let char_id = 951_201;
    let ship_id = 961_201;
    let mut sess = seed_pilot(&ctx, char_id, ship_id, 641, NULL_SYS, false);
    spawn_ship_in_space(&ctx, char_id, ship_id, 641, NULL_SYS, DVec3::new(100_000.0, 0.0, 0.0)).await;
    for &(sk, lvl) in &[(3336, 5), (3300, 5), (3303, 5), (12213, 5)] {
        set_skill(&ctx, char_id, sk, lvl);
    }

    // Negative/abuse case 2: reject deployment near stargate (<50km) and SDE celestial (<5km)
    let gate = ctx.sde.stargates().values().find(|j| j.from_solar_system_id == NULL_SYS).expect("SDE stargate in NULL_SYS");
    let depot_item = 971_201;
    give_item(&ctx, depot_item, 33474, char_id, ship_id, InventoryFlag::Cargo, 1);
    sys.try_set_ball_position(ship_id, gate.position + DVec3::new(1_000.0, 0.0, 0.0));
    let near_pos = gate.position + DVec3::new(1_200.0, 0.0, 0.0);
    assert!(ctx.dispatch_rpc(&mut sess, "posMgr", None, "AnchorStructure", &[PyRep::Integer(depot_item), PyRep::Tuple(vec![PyRep::Real(near_pos.x), PyRep::Real(near_pos.y), PyRep::Real(near_pos.z)])], &kw, None).await.is_err());
    let cel_pos = ctx.sde.celestials_in_system(NULL_SYS).into_iter().find(|c| c.group_id == 7).unwrap().position + DVec3::new(2_000.0, 0.0, 0.0);
    sys.try_set_ball_position(ship_id, cel_pos);
    assert!(ctx.dispatch_rpc(&mut sess, "posMgr", None, "AnchorStructure", &[PyRep::Integer(depot_item), PyRep::Tuple(vec![PyRep::Real(cel_pos.x), PyRep::Real(cel_pos.y), PyRep::Real(cel_pos.z)])], &kw, None).await.is_err(), "Mobile Depot deployment <5km from SDE planet must be rejected");

    // Launch from cargo >= 50km from celestials: 60s anchoring countdown
    let dep_pos = DVec3::new(100_000.0, 0.0, 0.0);
    sys.try_set_ball_position(ship_id, dep_pos);
    let target_pos = dep_pos + DVec3::new(500.0, 0.0, 0.0);
    let dep_res = ctx
        .dispatch_rpc(&mut sess, "posMgr", None, "AnchorStructure", &[PyRep::Integer(depot_item), PyRep::Tuple(vec![PyRep::Real(target_pos.x), PyRep::Real(target_pos.y), PyRep::Real(target_pos.z)])], &kw, None)
        .await
        .expect("AnchorStructure succeeds");
    assert_eq!(dep_res.dict_get("anchoringDelaySeconds"), Some(&PyRep::Real(60.0)));

    // 5 km spacing rule: deploying a second Mobile Depot within < 5,000 m of the first is rejected!
    let depot2_item = 971_202;
    give_item(&ctx, depot2_item, 33474, char_id, ship_id, InventoryFlag::Cargo, 1);
    let too_close = target_pos + DVec3::new(2_000.0, 0.0, 0.0);
    assert!(
        ctx.dispatch_rpc(&mut sess, "posMgr", None, "AnchorStructure", &[PyRep::Integer(depot2_item), PyRep::Tuple(vec![PyRep::Real(too_close.x), PyRep::Real(too_close.y), PyRep::Real(too_close.z)])], &kw, None).await.is_err(),
        "Second Mobile Depot within 5km of existing Mobile Depot must be rejected"
    );

    // During 60s anchoring timer: IS damageable (not invulnerable) AND cannot refit before anchoring finishes
    let (applied_anchor, _) = eve_server::services::station_structure::structure_lifecycle_rpc::apply_damage_structure_or_ball(
        &ctx, &sys, NULL_SYS, char_id, depot_item, eve_destiny::DamageVector::new(100.0, 0.0, 0.0, 0.0), 1.0, ctx.now_unix_secs(),
    ).await.ok().flatten().map(|(a, d, _)| (a, d)).unwrap_or((0.0, false));
    assert!(applied_anchor > 0.0, "Mobile Depot must be damageable during its 60s anchoring countdown");

    let mod_item = 971_210;
    give_item(&ctx, mod_item, 3041, char_id, ship_id, InventoryFlag::Cargo, 1);
    assert!(
        ctx.dispatch_rpc(&mut sess, "invbroker", None, "Add", &[PyRep::Integer(mod_item), PyRep::Integer(ship_id), PyRep::Integer(27)], &kw, None).await.is_err(),
        "Cannot refit in space while Mobile Depot is still anchoring"
    );

    // Advance 61s -> step -> anchor_completed == true, and depot now takes damage once anchored
    ctx.advance_time_seconds(61);
    let _ = sys.step_manual(1.0).await;
    let st_anchored = ctx.db.with_snapshot(|snap| eve_server::services::mobile_deployables::read_deployable_state_snap(snap).deployables.get(&depot_item).cloned()).unwrap();
    assert!(st_anchored.anchor_completed, "Mobile Depot must complete anchoring after 60s");
    let (applied_anchored, _) = eve_server::services::station_structure::structure_lifecycle_rpc::apply_damage_structure_or_ball(
        &ctx, &sys, NULL_SYS, char_id, depot_item, eve_destiny::DamageVector::new(50.0, 0.0, 0.0, 0.0), 1.0, ctx.now_unix_secs(),
    ).await.ok().flatten().map(|(a, d, _)| (a, d)).unwrap_or((0.0, false));
    assert!(applied_anchored > 0.0, "Mobile Depot must take damage once 60s anchoring completes");

    // Dedicated storage bay deposit & retrieval via invbroker.Add
    let bay_item = 971_215;
    give_item(&ctx, bay_item, 34, char_id, ship_id, InventoryFlag::Cargo, 250);
    ctx.dispatch_rpc(&mut sess, "invbroker", None, "Add", &[PyRep::Integer(bay_item), PyRep::Integer(depot_item), PyRep::Integer(5)], &kw, None).await.expect("Deposit into Mobile Depot storage bay");
    assert_eq!(ctx.db.with_snapshot(|s| s.items.get(&bay_item).unwrap().location_id), depot_item);
    ctx.dispatch_rpc(&mut sess, "invbroker", None, "Add", &[PyRep::Integer(bay_item), PyRep::Integer(ship_id), PyRep::Integer(5)], &kw, None).await.expect("Withdraw from Mobile Depot storage bay");
    assert_eq!(ctx.db.with_snapshot(|s| s.items.get(&bay_item).unwrap().location_id), ship_id);

    // Weapons timer blocks refitting even within 2,500m of an anchored Mobile Depot
    ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner()).get_or_create(char_id).weapon_timer_until_unix = ctx.now_unix_secs() + 60;
    assert!(ctx.dispatch_rpc(&mut sess, "invbroker", None, "Add", &[PyRep::Integer(mod_item), PyRep::Integer(ship_id), PyRep::Integer(27)], &kw, None).await.is_err(), "Weapons Timer must block Mobile Depot refitting");
    ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner()).get_or_create(char_id).weapon_timer_until_unix = 0;

    // Live refit within <= 2,500m via invbroker.Add:
    // (a) Fit & unfit module (3041 -> HiSlot0 = 27 -> Cargo = 5)
    ctx.dispatch_rpc(&mut sess, "invbroker", None, "Add", &[PyRep::Integer(mod_item), PyRep::Integer(ship_id), PyRep::Integer(27)], &kw, None).await.expect("Fit module near Mobile Depot");
    assert_eq!(ctx.db.with_snapshot(|s| s.items.get(&mod_item).unwrap().flag), InventoryFlag::HiSlot0);
    ctx.dispatch_rpc(&mut sess, "invbroker", None, "Add", &[PyRep::Integer(mod_item), PyRep::Integer(ship_id), PyRep::Integer(5)], &kw, None).await.expect("Unfit module near Mobile Depot");
    assert_eq!(ctx.db.with_snapshot(|s| s.items.get(&mod_item).unwrap().flag), InventoryFlag::Cargo);

    // (b) Fit rig (Large Trimark Armor Pump I 26302 -> RigSlot0 = 92) and test dogmaIM.DestroyFitting guards (Weapons Timer, >2,500m range) + destruction
    let rig_item = 971_211;
    let rig_item_2 = 971_212;
    give_item(&ctx, rig_item, 26302, char_id, ship_id, InventoryFlag::Cargo, 1);
    give_item(&ctx, rig_item_2, 26302, char_id, ship_id, InventoryFlag::RigSlot1, 1);
    ctx.dispatch_rpc(&mut sess, "invbroker", None, "Add", &[PyRep::Integer(rig_item), PyRep::Integer(ship_id), PyRep::Integer(92)], &kw, None).await.expect("Fit rig near Mobile Depot");
    assert_eq!(ctx.db.with_snapshot(|s| s.items.get(&rig_item).unwrap().flag), InventoryFlag::RigSlot0);
    ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner()).get_or_create(char_id).weapon_timer_until_unix = ctx.now_unix_secs() + 60;
    assert_eq!(ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "DestroyFitting", &[PyRep::Integer(rig_item)], &kw, None).await.unwrap_err().code, "CannotRefitWithWeaponsTimer", "Weapons Timer must block in-space DestroyFitting");
    ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner()).get_or_create(char_id).weapon_timer_until_unix = 0;
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "DestroyFitting", &[PyRep::Integer(rig_item)], &kw, None).await.expect("Destroy fitted rig");
    assert!(ctx.db.with_snapshot(|s| !s.items.contains_key(&rig_item)), "Removed rig must be destroyed");

    // (c) Personal-access enforcement + T3 Strategic Cruiser (Tengu 29984) subsystem swap within 2,500m
    let stranger_char = 951_205;
    let stranger_ship = 961_205;
    let mut stranger_sess = seed_pilot(&ctx, stranger_char, stranger_ship, 641, NULL_SYS, false);
    spawn_ship_in_space(&ctx, stranger_char, stranger_ship, 641, NULL_SYS, target_pos + DVec3::new(800.0, 0.0, 0.0)).await;
    give_item(&ctx, 971_219, 3041, stranger_char, stranger_ship, InventoryFlag::Cargo, 1);
    assert!(
        ctx.dispatch_rpc(&mut stranger_sess, "invbroker", None, "Add", &[PyRep::Integer(971_219), PyRep::Integer(stranger_ship), PyRep::Integer(27)], &kw, None).await.is_err(),
        "Another pilot must not be able to refit off a personal Mobile Depot"
    );

    let t3_ship = 961_206;
    give_item(&ctx, t3_ship, 29984, char_id, NULL_SYS as i64, InventoryFlag::None, 1);
    ctx.db.with_mut_snapshot(|s| { if let Some(ch) = s.characters.get_mut(&char_id) { ch.active_ship_id = t3_ship; } });
    sess.ship_id = Some(t3_ship);
    spawn_ship_in_space(&ctx, char_id, t3_ship, 29984, NULL_SYS, target_pos + DVec3::new(1_000.0, 0.0, 0.0)).await;
    for &(sk, lvl) in &[(30651, 5), (30544, 5), (30548, 5), (30549, 5), (30552, 5)] { set_skill(&ctx, char_id, sk, lvl); }
    give_item(&ctx, 971_220, 45589, char_id, t3_ship, InventoryFlag::SubSystemSlot1, 1);
    give_item(&ctx, 971_221, 45590, char_id, t3_ship, InventoryFlag::Cargo, 1);
    ctx.dispatch_rpc(&mut sess, "invbroker", None, "Add", &[PyRep::Integer(971_220), PyRep::Integer(t3_ship), PyRep::Integer(5)], &kw, None).await.expect("Unfit old Tengu subsystem near Mobile Depot");
    ctx.dispatch_rpc(&mut sess, "invbroker", None, "Add", &[PyRep::Integer(971_221), PyRep::Integer(t3_ship), PyRep::Integer(126)], &kw, None).await.expect("Fit new Tengu subsystem near Mobile Depot");
    assert_eq!(ctx.db.with_snapshot(|s| s.items.get(&971_221).unwrap().flag), InventoryFlag::SubSystemSlot1);

    // Switch back to Megathron and move > 2,500m (5,000m from depot): live invbroker.Add & dogmaIM.DestroyFitting rejected!
    ctx.db.with_mut_snapshot(|s| { if let Some(ch) = s.characters.get_mut(&char_id) { ch.active_ship_id = ship_id; } });
    sess.ship_id = Some(ship_id);
    let _ = sys.set_ball_position(ship_id, target_pos + DVec3::new(5_000.0, 0.0, 0.0)).await;
    assert!(ctx.dispatch_rpc(&mut sess, "invbroker", None, "Add", &[PyRep::Integer(mod_item), PyRep::Integer(ship_id), PyRep::Integer(27)], &kw, None).await.is_err(), "Live module fitting > 2,500m from Mobile Depot must fail");
    assert_eq!(ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "DestroyFitting", &[PyRep::Integer(rig_item_2)], &kw, None).await.unwrap_err().code, "CannotUnfitInSpace", "DestroyFitting > 2,500m from Mobile Depot must fail");

    // Attacking anchored depot to <= 25% shield triggers 48h (172,800s) reinforced timer AND disables fitting service + DestroyFitting!
    let now_u = ctx.now_unix_secs();
    let max_sh = sys.get_combat_state(depot_item).await.unwrap().unwrap().max_shield_hp;
    let _ = eve_server::services::station_structure::structure_lifecycle_rpc::apply_damage_structure_or_ball(
        &ctx, &sys, NULL_SYS, char_id, depot_item, eve_destiny::DamageVector::new(max_sh * 0.85, 0.0, 0.0, 0.0), 1.0, now_u,
    ).await;
    let st_reinf = ctx.db.with_snapshot(|snap| eve_server::services::mobile_deployables::read_deployable_state_snap(snap).deployables.get(&depot_item).cloned()).unwrap();
    assert!(st_reinf.reinforced_until_unix.unwrap_or(0) >= now_u + 172_800 - 5, "Damaging Mobile Depot shield to <= 25% must trigger 48-hour (172,800s) reinforced timer");
    assert!(sys.get_combat_state(depot_item).await.unwrap().unwrap().shield_hp <= max_sh * 0.25 + 1.0, "Reinforcement must trigger at <= 25% shield");
    let _ = sys.set_ball_position(ship_id, target_pos + DVec3::new(500.0, 0.0, 0.0)).await;
    assert!(ctx.dispatch_rpc(&mut sess, "invbroker", None, "Add", &[PyRep::Integer(mod_item), PyRep::Integer(ship_id), PyRep::Integer(27)], &kw, None).await.is_err(), "Refitting off a reinforced Mobile Depot must be disabled during 48h reinforcement");
    assert_eq!(ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "DestroyFitting", &[PyRep::Integer(rig_item_2)], &kw, None).await.unwrap_err().code, "CannotUnfitInSpace", "DestroyFitting off a reinforced Mobile Depot must fail");
    assert!(ctx.db.with_snapshot(|s| s.items.contains_key(&rig_item_2)), "Rig must remain intact after rejected DestroyFitting");
}
