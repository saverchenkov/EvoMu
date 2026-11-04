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
    ctx.db.with_mut_snapshot(|s| {
        let mut acc = s.accounts.values().next().cloned().unwrap();
        acc.user_id = v_200_char as u64;
        acc.username = format!("u_{v_200_char}");
        s.accounts.insert(acc.username.clone(), acc);
        s.characters.get_mut(&v_200_char).unwrap().user_id = v_200_char as u64;
    });
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
    // parity: feat:modules.mobile-depot-deployment-and-refitting
    eve_testkit::attest(&["feat:modules.mobile-depot-deployment-and-refitting"]);
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

pub(crate) mod jump_portal_bridge_rules {
    // Dedicated integration tests for open `feat:modules.*` and `feat:navigation.*` parity rows (Batch 35, Part 2).

    use crate::modules_advanced_tactical::{build_ctx, seed_pilot, NULL_SYS};
    use crate::nav_tactical_modules::{give_item, put_in_fleet, set_skill, spawn_ship_in_space, DEST_NULL_SYS};
    use eve_db::{InventoryFlag, UpwellStructureRecord};
    use eve_destiny::DVec3;
    use eve_proto::PyRep;
    use std::collections::HashMap;

    #[tokio::test]
    async fn test_b35_06_jump_portal_bridge_rules() {
        // parity: feat:modules.jump-portal-bridge-rules
        eve_testkit::attest(&["feat:modules.jump-portal-bridge-rules"]);
        let ctx = build_ctx();
        let origin_sys = ctx.systems.get_or_spawn(NULL_SYS);
        let kw = HashMap::new();

        // Negative/abuse cases (>= 2): unauthenticated bridge & jump-through rejected
        let mut unauth = eve_machonet::SessionState::new(1);
        assert!(ctx.dispatch_rpc(&mut unauth, "beyonce", None, "CmdBridgeToFleetModuleBeacon", &[PyRep::Integer(1)], &kw, None).await.is_err());
        assert!(ctx.dispatch_rpc(&mut unauth, "beyonce", None, "CmdJumpThroughFleet", &[PyRep::Integer(1), PyRep::Integer(1)], &kw, None).await.is_err());

        // Light Normal Cyno (21096), Covert Cyno (28646), and Industrial Cyno (52694) in DEST_NULL_SYS
        let cyno_char = 952_301;
        let cyno_ship = 962_301;
        put_in_fleet(&ctx, 880_001, cyno_char, &[(cyno_char, true), (952_311, true), (952_312, true)]);
        let mut cyno_sess = seed_pilot(&ctx, cyno_char, cyno_ship, 11957, DEST_NULL_SYS, false);
        spawn_ship_in_space(&ctx, cyno_char, cyno_ship, 11957, DEST_NULL_SYS, DVec3::new(10_000.0, 0.0, 0.0)).await;
        give_item(&ctx, 972_301, 21096, cyno_char, cyno_ship, InventoryFlag::HiSlot0, 1);
        give_item(&ctx, 972_302, 16273, cyno_char, cyno_ship, InventoryFlag::Cargo, 2_000);
        let beacon_norm = ctx.dispatch_rpc(&mut cyno_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(972_301), PyRep::String("cynosuralGeneration".into()), PyRep::Integer(0), PyRep::Integer(1)], &kw, None).await.expect("Normal cyno").dict_get("beaconID").and_then(|v| v.as_i64()).unwrap();

        let cov_cyno_char = 952_311;
        let cov_cyno_ship = 962_311;
        let mut cov_cyno_sess = seed_pilot(&ctx, cov_cyno_char, cov_cyno_ship, 11957, DEST_NULL_SYS, false);
        spawn_ship_in_space(&ctx, cov_cyno_char, cov_cyno_ship, 11957, DEST_NULL_SYS, DVec3::new(20_000.0, 0.0, 0.0)).await;
        give_item(&ctx, 972_311, 28646, cov_cyno_char, cov_cyno_ship, InventoryFlag::HiSlot0, 1);
        give_item(&ctx, 972_312, 16273, cov_cyno_char, cov_cyno_ship, InventoryFlag::Cargo, 2_000);
        let beacon_cov = ctx.dispatch_rpc(&mut cov_cyno_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(972_311), PyRep::String("cynosuralGeneration".into()), PyRep::Integer(0), PyRep::Integer(1)], &kw, None).await.expect("Covert cyno").dict_get("beaconID").and_then(|v| v.as_i64()).unwrap();

        let ind_cyno_char = 952_312;
        let ind_cyno_ship = 962_312;
        let mut ind_cyno_sess = seed_pilot(&ctx, ind_cyno_char, ind_cyno_ship, 32880, DEST_NULL_SYS, false);
        spawn_ship_in_space(&ctx, ind_cyno_char, ind_cyno_ship, 32880, DEST_NULL_SYS, DVec3::new(30_000.0, 0.0, 0.0)).await;
        give_item(&ctx, 972_313, 52694, ind_cyno_char, ind_cyno_ship, InventoryFlag::HiSlot0, 1);
        give_item(&ctx, 972_314, 16273, ind_cyno_char, ind_cyno_ship, InventoryFlag::Cargo, 2_000);
        let beacon_ind = ctx.dispatch_rpc(&mut ind_cyno_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(972_313), PyRep::String("cynosuralGeneration".into()), PyRep::Integer(0), PyRep::Integer(1)], &kw, None).await.expect("Indy cyno").dict_get("beaconID").and_then(|v| v.as_i64()).unwrap();

        // 1. Titan (Avatar 11567): normal cyno only, 60s, 500 Strontium base + JPG V (24562) -> 250 Strontium, immobilized, bubbled host allowed
        let titan_char = 952_302;
        let titan_ship = 962_302;
        let mut titan_sess = seed_pilot(&ctx, titan_char, titan_ship, 11567, NULL_SYS, false);
        spawn_ship_in_space(&ctx, titan_char, titan_ship, 11567, NULL_SYS, DVec3::ZERO).await;
        let titan_beyonce = ctx.bind_object(&mut titan_sess, "beyonce", NULL_SYS as i64).unwrap();
        give_item(&ctx, 972_303, 23953, titan_char, titan_ship, InventoryFlag::HiSlot0, 1); // Jump Portal Generator I
        give_item(&ctx, 972_304, 16275, titan_char, titan_ship, InventoryFlag::Cargo, 1_000); // Strontium
        // Put partial topes in Fuel Bay (10) and rest in Cargo (50,000) to verify Fuel Bay -> Cargo fallback!
        give_item(&ctx, 972_305, 16274, titan_char, titan_ship, InventoryFlag::SpecializedFuelBay, 10);
        give_item(&ctx, 972_315, 16274, titan_char, titan_ship, InventoryFlag::Cargo, 50_000);

        let j_ok_char = 952_303;
        let j_ok_ship = 962_303;
        let mut j_ok_sess = seed_pilot(&ctx, j_ok_char, j_ok_ship, 624, NULL_SYS, false); // Maller subcap
        spawn_ship_in_space(&ctx, j_ok_char, j_ok_ship, 624, NULL_SYS, DVec3::new(1_000.0, 0.0, 0.0)).await;

        let j_cap_char = 952_313;
        let j_cap_ship = 962_313;
        let mut j_cap_sess = seed_pilot(&ctx, j_cap_char, j_cap_ship, 23757, NULL_SYS, false); // Archon capital (must be rejected!)
        spawn_ship_in_space(&ctx, j_cap_char, j_cap_ship, 23757, NULL_SYS, DVec3::new(1_200.0, 0.0, 0.0)).await;

        let j_far_char = 952_304;
        let j_far_ship = 962_304;
        let mut j_far_sess = seed_pilot(&ctx, j_far_char, j_far_ship, 624, NULL_SYS, false);
        spawn_ship_in_space(&ctx, j_far_char, j_far_ship, 624, NULL_SYS, DVec3::new(3_000.0, 0.0, 0.0)).await;

        let blops_char = 952_306;
        let blops_ship = 962_306;
        let cov_jumper = 952_307;
        let cov_ship = 962_307;
        let noncov_jumper = 952_308;
        let noncov_ship = 962_308;
        let rorq_char = 952_320;
        let rorq_ship = 962_320;
        let ven_j = 952_321;
        let porp_j = 952_322;
        let orca_j = 952_323;

        put_in_fleet(&ctx, 880_002, titan_char, &[
            (cyno_char, true), (cov_cyno_char, true), (ind_cyno_char, true), (titan_char, true),
            (j_ok_char, true), (j_cap_char, true), (j_far_char, true),
            (blops_char, true), (cov_jumper, true), (noncov_jumper, true),
            (rorq_char, true), (ven_j, true), (porp_j, true), (orca_j, true),
        ]);

        // Titan rejected targeting Covert Cyno or Industrial Cyno
        assert!(ctx.dispatch_rpc(&mut titan_sess, "beyonce", None, "CmdBridgeToFleetModuleBeacon", &[PyRep::Integer(cov_cyno_char), PyRep::Integer(DEST_NULL_SYS as i64), PyRep::Integer(beacon_cov)], &kw, None).await.is_err());
        assert!(ctx.dispatch_rpc(&mut titan_sess, "beyonce", None, "CmdBridgeToFleetModuleBeacon", &[PyRep::Integer(ind_cyno_char), PyRep::Integer(DEST_NULL_SYS as i64), PyRep::Integer(beacon_ind)], &kw, None).await.is_err());

        // Host in bubble can open bridge; JPG 0 -> 500 Strontium, 60s, Titan immobilized
        set_skill(&ctx, titan_char, 24562, 0);
        let _ = origin_sys.add_warp_bubble(999_001, DVec3::ZERO, 20_000.0).await;
        let bridge_res = ctx.dispatch_rpc(&mut titan_sess, "beyonce", None, "CmdBridgeToFleetModuleBeacon", &[PyRep::Integer(cyno_char), PyRep::Integer(DEST_NULL_SYS as i64), PyRep::Integer(beacon_norm)], &kw, None).await.expect("Titan open bridge");
        assert_eq!(bridge_res.dict_get("durationSeconds"), Some(&PyRep::Real(60.0)));
        assert_eq!(bridge_res.dict_get("activationConsumptionQuantity"), Some(&PyRep::Integer(500)));
        assert!(ctx.dispatch_rpc(&mut titan_sess, "beyonce", Some(titan_beyonce), "CmdGotoDirection", &[PyRep::Real(1.0), PyRep::Real(0.0), PyRep::Real(0.0)], &kw, None).await.is_err(), "Titan must be immobilized while portal is open");

        // >2,500m jumper & bubbled jumper rejected
        assert!(ctx.dispatch_rpc(&mut j_far_sess, "beyonce", None, "CmdJumpThroughFleet", &[PyRep::Integer(titan_char), PyRep::Integer(titan_ship)], &kw, None).await.is_err());
        assert!(ctx.dispatch_rpc(&mut j_ok_sess, "beyonce", None, "CmdJumpThroughFleet", &[PyRep::Integer(titan_char), PyRep::Integer(titan_ship)], &kw, None).await.is_err());

        let _ = origin_sys.remove_warp_bubble(999_001).await;

        // Capital ship rejected from Titan bridge
        assert!(ctx.dispatch_rpc(&mut j_cap_sess, "beyonce", None, "CmdJumpThroughFleet", &[PyRep::Integer(titan_char), PyRep::Integer(titan_ship)], &kw, None).await.is_err(), "Capital ship cannot take Titan bridge");

        // Tethered jumper rejected (CannotJumpWhileTethered) & <0 WCS jumper rejected!
        let mut cs_teth = origin_sys.get_combat_state(j_ok_ship).await.unwrap().unwrap();
        cs_teth.is_tethered = true;
        origin_sys.set_combat_state(j_ok_ship, cs_teth).await.unwrap();
        let teth_err = ctx.dispatch_rpc(&mut j_ok_sess, "beyonce", None, "CmdJumpThroughFleet", &[PyRep::Integer(titan_char), PyRep::Integer(titan_ship)], &kw, None).await.unwrap_err();
        assert_eq!(teth_err.code, "CannotJumpWhileTethered");

        let mut cs_wcs = origin_sys.get_combat_state(j_ok_ship).await.unwrap().unwrap();
        cs_wcs.is_tethered = false;
        cs_wcs.warp_core_strength = -1;
        origin_sys.set_combat_state(j_ok_ship, cs_wcs).await.unwrap();
        assert!(ctx.dispatch_rpc(&mut j_ok_sess, "beyonce", None, "CmdJumpThroughFleet", &[PyRep::Integer(titan_char), PyRep::Integer(titan_ship)], &kw, None).await.is_err(), "Ship below 0 WCS cannot take bridge");

        let mut cs_clean = origin_sys.get_combat_state(j_ok_ship).await.unwrap().unwrap();
        cs_clean.warp_core_strength = 0;
        origin_sys.set_combat_state(j_ok_ship, cs_clean).await.unwrap();

        // Subcap jumps (4-arg client call) -> consumes all 10 topes from Fuel Bay first, then remainder from Cargo!
        assert!(ctx.dispatch_rpc(&mut j_ok_sess, "beyonce", None, "CmdJumpThroughFleet", &[PyRep::Integer(titan_char), PyRep::Integer(titan_ship), PyRep::Integer(beacon_norm + 999), PyRep::Integer(DEST_NULL_SYS as i64)], &kw, None).await.is_err(), "Mismatched beaconID must fail");
        let jump_ok = ctx.dispatch_rpc(&mut j_ok_sess, "beyonce", None, "CmdJumpThroughFleet", &[PyRep::Integer(titan_char), PyRep::Integer(titan_ship), PyRep::Integer(beacon_norm), PyRep::Integer(DEST_NULL_SYS as i64)], &kw, None).await.expect("JumpThroughFleet");
        let fuel_consumed = jump_ok.dict_get("fuelConsumed").and_then(|v| v.as_i64()).unwrap();
        assert!(fuel_consumed > 10);
        assert!(ctx.db.with_snapshot(|s| !s.items.contains_key(&972_305)), "Fuel bay stack must be exhausted first");
        assert_eq!(ctx.db.with_snapshot(|s| s.items.get(&972_315).unwrap().quantity), 50_000 - (fuel_consumed as i32 - 10), "Remainder must come from bridger cargo");

        // Verify Jump Portal Generation V (24562) reduces Titan Strontium cost by 50% (500 -> 250) after 60s portal closes
        ctx.advance_time_seconds(65);
        set_skill(&ctx, titan_char, 24562, 5);
        ctx.invalidate_ship_fitting(titan_ship);
        let bridge_jpg5 = ctx.dispatch_rpc(&mut titan_sess, "beyonce", None, "CmdBridgeToFleetModuleBeacon", &[PyRep::Integer(cyno_char), PyRep::Integer(DEST_NULL_SYS as i64), PyRep::Integer(beacon_norm)], &kw, None).await.expect("Titan bridge JPG V");
        assert_eq!(bridge_jpg5.dict_get("activationConsumptionQuantity"), Some(&PyRep::Integer(250)), "JPG V must halve Strontium cost to 250");

        // 2. Black Ops (Redeemer 22428) bridge: 20s, 0 Strontium, covert-cloak hulls only, bridger JFC V halves passenger fuel
        let mut blops_sess = seed_pilot(&ctx, blops_char, blops_ship, 22428, NULL_SYS, false);
        spawn_ship_in_space(&ctx, blops_char, blops_ship, 22428, NULL_SYS, DVec3::ZERO).await;
        set_skill(&ctx, blops_char, 21610, 5); // JFC V on bridger
        give_item(&ctx, 972_306, 28652, blops_char, blops_ship, InventoryFlag::HiSlot0, 1);
        give_item(&ctx, 972_307, 16274, blops_char, blops_ship, InventoryFlag::SpecializedFuelBay, 100_000);

        let mut cov_sess = seed_pilot(&ctx, cov_jumper, cov_ship, 11172, NULL_SYS, false); // Helios
        spawn_ship_in_space(&ctx, cov_jumper, cov_ship, 11172, NULL_SYS, DVec3::new(1_000.0, 0.0, 0.0)).await;
        let mut noncov_sess = seed_pilot(&ctx, noncov_jumper, noncov_ship, 624, NULL_SYS, false); // Maller
        spawn_ship_in_space(&ctx, noncov_jumper, noncov_ship, 624, NULL_SYS, DVec3::new(1_000.0, 0.0, 0.0)).await;

        let beacon_cov = ctx.dispatch_rpc(&mut cov_cyno_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(972_311), PyRep::String("cynosuralGeneration".into()), PyRep::Integer(0), PyRep::Integer(1)], &kw, None).await.expect("Re-light covert cyno").dict_get("beaconID").and_then(|v| v.as_i64()).unwrap();
        let blops_b_res = ctx.dispatch_rpc(&mut blops_sess, "beyonce", None, "CmdBridgeToFleetModuleBeacon", &[PyRep::Integer(cov_cyno_char), PyRep::Integer(DEST_NULL_SYS as i64), PyRep::Integer(beacon_cov)], &kw, None).await.expect("Blops open bridge");
        assert_eq!(blops_b_res.dict_get("durationSeconds"), Some(&PyRep::Real(20.0)));
        assert_eq!(blops_b_res.dict_get("activationConsumptionQuantity"), Some(&PyRep::Integer(0)));
        assert!(ctx.dispatch_rpc(&mut noncov_sess, "beyonce", None, "CmdJumpThroughFleet", &[PyRep::Integer(blops_char), PyRep::Integer(blops_ship)], &kw, None).await.is_err());
        assert!(ctx.dispatch_rpc(&mut cov_sess, "beyonce", None, "CmdJumpThroughFleet", &[PyRep::Integer(blops_char), PyRep::Integer(blops_ship)], &kw, None).await.is_ok());

        // 3. Rorqual (28352 + Industrial Jump Portal Generator I 63140): 20s, 0 Strontium, mining hulls (Venture, Porpoise) accepted, Orca & Maller rejected
        let mut rorq_sess = seed_pilot(&ctx, rorq_char, rorq_ship, 28352, NULL_SYS, false);
        spawn_ship_in_space(&ctx, rorq_char, rorq_ship, 28352, NULL_SYS, DVec3::ZERO).await;
        give_item(&ctx, 972_320, 63140, rorq_char, rorq_ship, InventoryFlag::HiSlot0, 1);
        give_item(&ctx, 972_321, 17887, rorq_char, rorq_ship, InventoryFlag::SpecializedFuelBay, 100_000);
        let rorq_b_res = ctx.dispatch_rpc(&mut rorq_sess, "beyonce", None, "CmdBridgeToFleetModuleBeacon", &[PyRep::Integer(ind_cyno_char), PyRep::Integer(DEST_NULL_SYS as i64), PyRep::Integer(beacon_ind)], &kw, None).await.expect("Rorqual open bridge");
        assert_eq!(rorq_b_res.dict_get("durationSeconds"), Some(&PyRep::Real(20.0)));
        assert_eq!(rorq_b_res.dict_get("activationConsumptionQuantity"), Some(&PyRep::Integer(0)));

        let mut ven_sess = seed_pilot(&ctx, ven_j, 962_321, 32880, NULL_SYS, false);
        spawn_ship_in_space(&ctx, ven_j, 962_321, 32880, NULL_SYS, DVec3::new(1_000.0, 0.0, 0.0)).await;
        let mut porp_sess = seed_pilot(&ctx, porp_j, 962_322, 42244, NULL_SYS, false);
        spawn_ship_in_space(&ctx, porp_j, 962_322, 42244, NULL_SYS, DVec3::new(1_200.0, 0.0, 0.0)).await;
        let mut orca_sess = seed_pilot(&ctx, orca_j, 962_323, 28606, NULL_SYS, false);
        spawn_ship_in_space(&ctx, orca_j, 962_323, 28606, NULL_SYS, DVec3::new(1_400.0, 0.0, 0.0)).await;

        assert!(ctx.dispatch_rpc(&mut orca_sess, "beyonce", None, "CmdJumpThroughFleet", &[PyRep::Integer(rorq_char), PyRep::Integer(rorq_ship)], &kw, None).await.is_err(), "Orca must be rejected by Rorqual portal");
        assert!(ctx.dispatch_rpc(&mut noncov_sess, "beyonce", None, "CmdJumpThroughFleet", &[PyRep::Integer(rorq_char), PyRep::Integer(rorq_ship)], &kw, None).await.is_err(), "Combat cruiser must be rejected by Rorqual portal");
        // BF-B35-01: Active session-change timer on passenger must fail before deducting host fuel
        let rorq_fuel_before_sc = ctx.db.with_snapshot(|s| s.items.get(&972_321).unwrap().quantity);
        eve_server::services::space_destiny::record_session_change_timer(&ctx, ven_j, ctx.now_unix_secs(), NULL_SYS, 1, "jump");
        assert_eq!(ctx.dispatch_rpc(&mut ven_sess, "beyonce", None, "CmdJumpThroughFleet", &[PyRep::Integer(rorq_char), PyRep::Integer(rorq_ship)], &kw, None).await.unwrap_err().code, "SessionChangeTimerActive");
        assert_eq!(ctx.db.with_snapshot(|s| s.items.get(&972_321).unwrap().quantity), rorq_fuel_before_sc, "SessionChangeTimerActive must not deduct host portal fuel");
        eve_server::services::space_destiny::clear_session_change_timer(&ctx, ven_j);
        assert!(ctx.dispatch_rpc(&mut ven_sess, "beyonce", None, "CmdJumpThroughFleet", &[PyRep::Integer(rorq_char), PyRep::Integer(rorq_ship)], &kw, None).await.is_ok(), "Venture must be accepted by Rorqual portal");
        // BF-B35-02: Destroying or removing the bridging ship ball invalidates the bridge for remaining passengers
        origin_sys.try_remove_ball(rorq_ship);
        assert_eq!(ctx.dispatch_rpc(&mut porp_sess, "beyonce", None, "CmdJumpThroughFleet", &[PyRep::Integer(rorq_char), PyRep::Integer(rorq_ship)], &kw, None).await.unwrap_err().code, "NoActiveBridge");
    }

    #[tokio::test]
    async fn test_b35_07_conduit_jump() {
        // parity: feat:navigation.conduit-jump
        eve_testkit::attest(&["feat:navigation.conduit-jump"]);
        let ctx = build_ctx();
        let kw = HashMap::new();

        // Negative/abuse case 1: unauthenticated CmdGroupJumpToFleetModuleBeacon rejected
        let mut unauth = eve_machonet::SessionState::new(1);
        assert!(ctx.dispatch_rpc(&mut unauth, "beyonce", None, "CmdGroupJumpToFleetModuleBeacon", &[PyRep::Integer(1)], &kw, None).await.is_err());

        // Normal cyno (for Carrier / Command Carrier / Rorqual / Blops) in DEST_NULL_SYS
        let cyno_char = 952_401;
        let cyno_ship = 962_401;
        put_in_fleet(&ctx, 880_003, cyno_char, &[(cyno_char, true)]);
        let mut cyno_sess = seed_pilot(&ctx, cyno_char, cyno_ship, 11957, DEST_NULL_SYS, false);
        spawn_ship_in_space(&ctx, cyno_char, cyno_ship, 11957, DEST_NULL_SYS, DVec3::new(10_000.0, 0.0, 0.0)).await;
        give_item(&ctx, 972_401, 21096, cyno_char, cyno_ship, InventoryFlag::HiSlot0, 1);
        give_item(&ctx, 972_402, 16273, cyno_char, cyno_ship, InventoryFlag::Cargo, 2_000);
        let beacon_id = ctx.dispatch_rpc(&mut cyno_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(972_401), PyRep::String("cynosuralGeneration".into()), PyRep::Integer(0), PyRep::Integer(1)], &kw, None).await.expect("Normal cyno").dict_get("beaconID").and_then(|v| v.as_i64()).unwrap();

        // Negative/abuse case 2: Titan refused
        let titan_char = 952_402;
        let titan_ship = 962_402;
        let mut titan_sess = seed_pilot(&ctx, titan_char, titan_ship, 11567, NULL_SYS, false);
        spawn_ship_in_space(&ctx, titan_char, titan_ship, 11567, NULL_SYS, DVec3::ZERO).await;
        put_in_fleet(&ctx, 880_003, titan_char, &[(cyno_char, true), (titan_char, true)]);
        assert!(ctx.dispatch_rpc(&mut titan_sess, "beyonce", None, "CmdGroupJumpToFleetModuleBeacon", &[PyRep::Integer(cyno_char), PyRep::Integer(DEST_NULL_SYS as i64), PyRep::Integer(beacon_id)], &kw, None).await.is_err());

        // 1. Black Ops (Redeemer 22428): flat 1000 * dist_ly fuel (attr 3131, NOT 700!), cap checked & deducted,
        // covert hulls <= 10km jumped, non-covert skipped, >10km skipped, opted-out (accepts_conduit_jumps=false) skipped!
        let blops_char = 952_403;
        let blops_ship = 962_403;
        let mut blops_sess = seed_pilot(&ctx, blops_char, blops_ship, 22428, NULL_SYS, false);
        spawn_ship_in_space(&ctx, blops_char, blops_ship, 22428, NULL_SYS, DVec3::ZERO).await;
        set_skill(&ctx, blops_char, 3456, 0); // JDO 0 -> exact 95% base jumpDriveCapacitorNeed
        give_item(&ctx, 972_403, 16274, blops_char, blops_ship, InventoryFlag::SpecializedFuelBay, 50_000);

        let recon_ok = 952_404; // Falcon <= 10km, opted in -> JUMPS
        let inelig = 952_405;   // Deacon with cloak in cargo -> SKIPPED
        let recon_far = 952_410; // Falcon at 15km (> 10km) -> SKIPPED
        let recon_optout = 952_411; // Falcon at 2km, accepts_conduit_jumps = false -> SKIPPED
        let recon_wpn = 952_412; // Falcon at 2km with active Weapons Timer -> SKIPPED
        let recon_cd = 952_413;  // Falcon at 2km with active red jump cooldown -> SKIPPED
        for &(cid, sid, tid, pos) in &[
            (recon_ok, 962_404, 11957u32, DVec3::new(2_000.0, 0.0, 0.0)),
            (inelig, 962_405, 37453, DVec3::new(2_000.0, 0.0, 0.0)),
            (recon_far, 962_410, 11957, DVec3::new(15_000.0, 0.0, 0.0)),
            (recon_optout, 962_411, 11957, DVec3::new(2_500.0, 0.0, 0.0)),
            (recon_wpn, 962_412, 11957, DVec3::new(2_000.0, 0.0, 0.0)),
            (recon_cd, 962_413, 11957, DVec3::new(2_000.0, 0.0, 0.0)),
        ] {
            seed_pilot(&ctx, cid, sid, tid, NULL_SYS, false);
            spawn_ship_in_space(&ctx, cid, sid, tid, NULL_SYS, pos).await;
        }
        give_item(&ctx, 972_409, 11578, inelig, 962_405, InventoryFlag::Cargo, 1);
        let now_u = ctx.now_unix_secs();
        ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner()).get_or_create(recon_wpn).weapon_timer_until_unix = now_u + 60;
        ctx.db.with_mut_snapshot(|s| { s.characters.get_mut(&recon_cd).unwrap().jump_cooldown_until_unix = now_u + 600; });

        put_in_fleet(&ctx, 880_004, blops_char, &[
            (cyno_char, true), (blops_char, true), (recon_ok, true),
            (inelig, true), (recon_far, true), (recon_optout, false),
            (recon_wpn, true), (recon_cd, true),
        ]);
        let origin_sys = ctx.systems.get_or_spawn(NULL_SYS);
        let blops_max_cap = origin_sys.get_combat_state(blops_ship).await.unwrap().unwrap().max_capacitor;
        let blops_res = ctx.dispatch_rpc(&mut blops_sess, "beyonce", None, "CmdGroupJumpToFleetModuleBeacon", &[PyRep::Integer(cyno_char), PyRep::Integer(DEST_NULL_SYS as i64), PyRep::Integer(beacon_id)], &kw, None).await.expect("Blops conduit jump");
        let dist_ly = blops_res.dict_get("distanceLY").and_then(|v| v.as_f64()).unwrap();
        let blops_fuel = blops_res.dict_get("fuelConsumed").and_then(|v| v.as_i64()).unwrap();
        assert_eq!(blops_fuel, (1_000.0 * dist_ly).ceil() as i64, "Black Ops conduit jump must charge flat 1000 * LY isotopes (attr 3131)");
        let dest_sys = ctx.systems.get_or_spawn(DEST_NULL_SYS);
        let blops_cap_after = dest_sys.get_combat_state(blops_ship).await.unwrap().unwrap().capacitor;
        assert!(blops_cap_after < blops_max_cap * 0.10, "Conduit jump must deduct 95% leader capacitor");

        assert_eq!(ctx.db.with_snapshot(|s| s.characters.get(&recon_ok).unwrap().solar_system_id), DEST_NULL_SYS);
        assert_eq!(ctx.db.with_snapshot(|s| s.characters.get(&inelig).unwrap().solar_system_id), NULL_SYS);
        assert_eq!(ctx.db.with_snapshot(|s| s.characters.get(&recon_far).unwrap().solar_system_id), NULL_SYS, ">10km member must not be conduit jumped");
        assert_eq!(ctx.db.with_snapshot(|s| s.characters.get(&recon_optout).unwrap().solar_system_id), NULL_SYS, "Opted-out member must be skipped");
        assert_eq!(ctx.db.with_snapshot(|s| s.characters.get(&recon_wpn).unwrap().solar_system_id), NULL_SYS, "Passenger with active Weapons Timer must be excluded from conduit jump");
        assert_eq!(ctx.db.with_snapshot(|s| s.characters.get(&recon_cd).unwrap().solar_system_id), NULL_SYS, "Passenger with active jump cooldown must be excluded from conduit jump");
        let recon_fat = ctx.db.with_snapshot(|s| s.characters.get(&recon_ok).unwrap().jump_fatigue_minutes);
        assert!((recon_fat - 10.0 * (1.0 + dist_ly * 1.0)).abs() < 0.05, "Falcon passenger (1.0x fatigue multiplier) must receive 1.0x distance fatigue ({recon_fat})");

        // 2. Rorqual (28352): blocked while Industrial Core active (preserving fuel & cap); when inactive, flat 5000 * LY,
        // jumps Venture (32880), Procurer (17480), Porpoise (42244), and EXCLUDES Orca (28606)!
        let rorq_char = 952_406;
        let rorq_ship = 962_406;
        let mut rorq_sess = seed_pilot(&ctx, rorq_char, rorq_ship, 28352, NULL_SYS, false);
        spawn_ship_in_space(&ctx, rorq_char, rorq_ship, 28352, NULL_SYS, DVec3::ZERO).await;
        give_item(&ctx, 972_406, 20280, rorq_char, rorq_ship, InventoryFlag::HiSlot0, 1);
        give_item(&ctx, 972_407, 17887, rorq_char, rorq_ship, InventoryFlag::SpecializedFuelBay, 50_000);
        ctx.db.with_mut_snapshot(|s| { s.module_active_states.insert(972_406); });

        let m_ven = 952_420;
        let m_proc = 952_421;
        let m_porp = 952_422;
        let m_orca = 952_423;
        for &(cid, sid, tid) in &[(m_ven, 962_420, 32880u32), (m_proc, 962_421, 17480), (m_porp, 962_422, 42244), (m_orca, 962_423, 28606)] {
            seed_pilot(&ctx, cid, sid, tid, NULL_SYS, false);
            spawn_ship_in_space(&ctx, cid, sid, tid, NULL_SYS, DVec3::new(3_000.0, 0.0, 0.0)).await;
        }
        put_in_fleet(&ctx, 880_040, rorq_char, &[(cyno_char, true), (rorq_char, true), (m_ven, true), (m_proc, true), (m_porp, true), (m_orca, true)]);

        assert!(ctx.dispatch_rpc(&mut rorq_sess, "beyonce", None, "CmdGroupJumpToFleetModuleBeacon", &[PyRep::Integer(cyno_char), PyRep::Integer(DEST_NULL_SYS as i64), PyRep::Integer(beacon_id)], &kw, None).await.is_err());
        assert_eq!(ctx.db.with_snapshot(|s| s.items.get(&972_407).unwrap().quantity), 50_000, "Refused Rorqual conduit jump must NOT consume fuel");

        ctx.db.with_mut_snapshot(|s| { s.module_active_states.remove(&972_406); });
        let rorq_ok = ctx.dispatch_rpc(&mut rorq_sess, "beyonce", None, "CmdGroupJumpToFleetModuleBeacon", &[PyRep::Integer(cyno_char), PyRep::Integer(DEST_NULL_SYS as i64), PyRep::Integer(beacon_id)], &kw, None).await.expect("Rorqual conduit jump");
        assert_eq!(rorq_ok.dict_get("fuelConsumed"), Some(&PyRep::Integer((5_000.0 * dist_ly).ceil() as i64)), "Rorqual conduit jump must charge flat 5000 * LY isotopes");
        assert_eq!(ctx.db.with_snapshot(|s| s.characters.get(&m_ven).unwrap().solar_system_id), DEST_NULL_SYS);
        assert_eq!(ctx.db.with_snapshot(|s| s.characters.get(&m_proc).unwrap().solar_system_id), DEST_NULL_SYS);
        assert_eq!(ctx.db.with_snapshot(|s| s.characters.get(&m_porp).unwrap().solar_system_id), DEST_NULL_SYS, "Porpoise must conduit jump with Rorqual");
        assert_eq!(ctx.db.with_snapshot(|s| s.characters.get(&m_orca).unwrap().solar_system_id), NULL_SYS, "Orca must NOT conduit jump with Rorqual");
        let porp_fat = ctx.db.with_snapshot(|s| s.characters.get(&m_porp).unwrap().jump_fatigue_minutes);
        assert!((porp_fat - 10.0 * (1.0 + dist_ly * 0.10)).abs() < 0.05, "Porpoise passenger (90% jump fatigue reduction, attr 1971=0.1) must receive 0.10x distance fatigue ({porp_fat})");

        // 3. Carrier (Archon 23757, CJPG 0 -> cap 5) vs Command Carrier (Salvation 92822, CJPG 0 -> double cap = 10)
        let car_char = 952_430;
        let car_ship = 962_430;
        let mut car_sess = seed_pilot(&ctx, car_char, car_ship, 23757, NULL_SYS, false);
        spawn_ship_in_space(&ctx, car_char, car_ship, 23757, NULL_SYS, DVec3::ZERO).await;
        set_skill(&ctx, car_char, 21611, 5); // JDC V
        set_skill(&ctx, car_char, 83094, 0); // CJPG 0 -> max 5 passengers
        give_item(&ctx, 972_430, 16274, car_char, car_ship, InventoryFlag::SpecializedFuelBay, 50_000);

        let mut car_fleet = vec![(cyno_char, true), (car_char, true)];
        for i in 0..7i64 {
            let pc = 952_440 + i;
            let ps = 962_440 + i;
            // Mix eligible subcap hulls: shuttle (672), capsule (670), corvette (596), frigate (587), destroyer (16240), cruiser (624), battleship (641)
            let ptid = [672u32, 670, 596, 587, 16240, 624, 641][i as usize];
            seed_pilot(&ctx, pc, ps, ptid, NULL_SYS, false);
            spawn_ship_in_space(&ctx, pc, ps, ptid, NULL_SYS, DVec3::new(1_500.0, 0.0, 0.0)).await;
            car_fleet.push((pc, true));
        }
        put_in_fleet(&ctx, 880_041, car_char, &car_fleet);
        let car_res = ctx.dispatch_rpc(&mut car_sess, "beyonce", None, "CmdGroupJumpToFleetModuleBeacon", &[PyRep::Integer(cyno_char), PyRep::Integer(DEST_NULL_SYS as i64), PyRep::Integer(beacon_id)], &kw, None).await.expect("Carrier conduit jump");
        assert_eq!(car_res.dict_get("fuelConsumed"), Some(&PyRep::Integer((3_000.0 * dist_ly).ceil() as i64)), "Carrier conduit must charge flat 3000 * LY isotopes");
        let jumped_car_cnt = (0..7i64).filter(|&i| ctx.db.with_snapshot(|s| s.characters.get(&(952_440 + i)).unwrap().solar_system_id == DEST_NULL_SYS)).count();
        assert_eq!(jumped_car_cnt, 5, "T1 Carrier at CJPG 0 must cap conduit passengers at 5 (got {jumped_car_cnt})");

        let cmd_char = 952_460;
        let cmd_ship = 962_460;
        let mut cmd_sess = seed_pilot(&ctx, cmd_char, cmd_ship, 92822, NULL_SYS, false); // Salvation (group 5120)
        spawn_ship_in_space(&ctx, cmd_char, cmd_ship, 92822, NULL_SYS, DVec3::ZERO).await;
        set_skill(&ctx, cmd_char, 21611, 5);
        set_skill(&ctx, cmd_char, 83094, 0); // CJPG 0 -> double = 10 passengers!
        give_item(&ctx, 972_460, 16274, cmd_char, cmd_ship, InventoryFlag::SpecializedFuelBay, 50_000);
        let mut cmd_fleet = vec![(cyno_char, true), (cmd_char, true)];
        for i in 0..12i64 {
            let pc = 952_470 + i;
            let ps = 962_470 + i;
            seed_pilot(&ctx, pc, ps, 587, NULL_SYS, false);
            spawn_ship_in_space(&ctx, pc, ps, 587, NULL_SYS, DVec3::new(1_500.0, 0.0, 0.0)).await;
            cmd_fleet.push((pc, true));
        }
        put_in_fleet(&ctx, 880_042, cmd_char, &cmd_fleet);
        ctx.dispatch_rpc(&mut cmd_sess, "beyonce", None, "CmdGroupJumpToFleetModuleBeacon", &[PyRep::Integer(cyno_char), PyRep::Integer(DEST_NULL_SYS as i64), PyRep::Integer(beacon_id)], &kw, None).await.expect("Command Carrier conduit jump");
        let jumped_cmd_cnt = (0..12i64).filter(|&i| ctx.db.with_snapshot(|s| s.characters.get(&(952_470 + i)).unwrap().solar_system_id == DEST_NULL_SYS)).count();
        assert_eq!(jumped_cmd_cnt, 10, "Command Carrier at CJPG 0 must double passenger cap to 10 (got {jumped_cmd_cnt})");
    }

    #[tokio::test]
    async fn test_b35_08_jump_arrival_placement() {
        // parity: feat:navigation.jump-arrival-placement
        eve_testkit::attest(&["feat:navigation.jump-arrival-placement"]);
        let ctx = build_ctx();
        let dest_sys = ctx.systems.get_or_spawn(DEST_NULL_SYS);
        let kw = HashMap::new();

        // Cyno ship in DEST_NULL_SYS
        let cyno_char = 952_501;
        let cyno_ship = 962_501;
        put_in_fleet(&ctx, 880_005, cyno_char, &[(cyno_char, true)]);
        let mut cyno_sess = seed_pilot(&ctx, cyno_char, cyno_ship, 11957, DEST_NULL_SYS, false);
        let cyno_pos = DVec3::new(100_000.0, 0.0, 0.0);
        spawn_ship_in_space(&ctx, cyno_char, cyno_ship, 11957, DEST_NULL_SYS, cyno_pos).await;
        give_item(&ctx, 972_501, 21096, cyno_char, cyno_ship, InventoryFlag::HiSlot0, 1);
        give_item(&ctx, 972_502, 16273, cyno_char, cyno_ship, InventoryFlag::Cargo, 2_000);
        let beacon_id = ctx.dispatch_rpc(&mut cyno_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(972_501), PyRep::String("cynosuralGeneration".into()), PyRep::Integer(0), PyRep::Integer(1)], &kw, None).await.expect("Cyno").dict_get("beaconID").and_then(|v| v.as_i64()).unwrap();
        let beacon_pos = dest_sys.get_ball_snapshot(beacon_id).await.unwrap().unwrap().1.position;

        // 1. 3D 5 km sphere placement across multiple jumps (verifies non-zero 3D vertical Y dispersion & 5km radius)
        let mut fleet_members = vec![(cyno_char, true)];
        for i in 0..4i64 {
            fleet_members.push((952_510 + i, true));
        }
        put_in_fleet(&ctx, 880_005, cyno_char, &fleet_members);
        let mut max_abs_y = 0.0_f64;
        for i in 0..4i64 {
            let jc = 952_510 + i;
            let js = 962_510 + i;
            let mut jsess = seed_pilot(&ctx, jc, js, 23757, NULL_SYS, false);
            spawn_ship_in_space(&ctx, jc, js, 23757, NULL_SYS, DVec3::new(i as f64 * 500.0, 0.0, 0.0)).await;
            give_item(&ctx, 972_510 + i, 16274, jc, js, InventoryFlag::SpecializedFuelBay, 50_000);
            ctx.dispatch_rpc(&mut jsess, "beyonce", None, "CmdJumpToFleetModuleBeacon", &[PyRep::Integer(cyno_char), PyRep::Integer(DEST_NULL_SYS as i64), PyRep::Integer(beacon_id)], &kw, None).await.expect("Jump drive");
            let b = dest_sys.get_ball_snapshot(js).await.unwrap().unwrap().1;
            let dist = b.position.distance(beacon_pos);
            assert!((dist - 5_000.0).abs() < 50.0, "Jump arrival must materialise at 5km sphere around beacon (got {dist}m)");
            max_abs_y = max_abs_y.max((b.position.y - beacon_pos.y).abs());
        }
        assert!(max_abs_y > 50.0, "Jump arrivals must sample a true 3D sphere with vertical Y dispersion (max |dy| = {max_abs_y})");

        // 2. Structure-bounding-sphere overlap ejection with outward velocity:
        // Place an online Keepstar (35834, large bounding sphere) centered at beacon_pos so any 5km arrival overlaps it!
        let keepstar_id = 982_501;
        let s_radius = ctx.sde.types.get(&35834).map(|t| t.radius).unwrap_or(15_000.0).max(10_000.0);
        ctx.db.with_mut_snapshot(|snap| {
            snap.structures.insert(
                keepstar_id,
                UpwellStructureRecord {
                    structure_id: keepstar_id,
                    type_id: 35834,
                    owner_corp_id: 1000009,
                    solar_system_id: DEST_NULL_SYS,
                    name: "Overlap Keepstar".into(),
                    position: beacon_pos.to_array(),
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
        let ej_char = 952_520;
        let ej_ship = 962_520;
        let mut ej_sess = seed_pilot(&ctx, ej_char, ej_ship, 23757, NULL_SYS, false);
        spawn_ship_in_space(&ctx, ej_char, ej_ship, 23757, NULL_SYS, DVec3::ZERO).await;
        give_item(&ctx, 972_520, 16274, ej_char, ej_ship, InventoryFlag::SpecializedFuelBay, 50_000);
        put_in_fleet(&ctx, 880_055, cyno_char, &[(cyno_char, true), (ej_char, true)]);
        ctx.dispatch_rpc(&mut ej_sess, "beyonce", None, "CmdJumpToFleetModuleBeacon", &[PyRep::Integer(cyno_char), PyRep::Integer(DEST_NULL_SYS as i64), PyRep::Integer(beacon_id)], &kw, None).await.expect("Jump into overlapping structure");
        let ej_ball = dest_sys.get_ball_snapshot(ej_ship).await.unwrap().unwrap().1;
        assert!(ej_ball.position.distance(beacon_pos) > s_radius, "Ship overlapping structure must be ejected outside structure bounding sphere");
        assert!(ej_ball.velocity.length() >= 200.0 && ej_ball.velocity.dot(ej_ball.position - beacon_pos) > 0.0, "Ejected ship must receive outward velocity");
        ctx.db.with_mut_snapshot(|s| { s.structures.remove(&keepstar_id); });

        // 3. Destroyed-beacon fallback lands outside destination star's photosphere on BOTH Jump Drive AND Bridge!
        let star_radius = ctx.sde.celestials_in_system(DEST_NULL_SYS).into_iter().find(|c| c.group_id == 6).map(|c| c.radius).unwrap_or(65_000_000.0);
        let cyno2_char = 952_503;
        let cyno2_ship = 962_503;
        let mut cyno2_sess = seed_pilot(&ctx, cyno2_char, cyno2_ship, 11957, DEST_NULL_SYS, false);
        ctx.db.with_mut_snapshot(|s| { if let Some(fl) = s.fleets.values_mut().next() { fl.members.push(cyno2_char); } });
        spawn_ship_in_space(&ctx, cyno2_char, cyno2_ship, 11957, DEST_NULL_SYS, DVec3::new(200_000.0, 0.0, 0.0)).await;
        give_item(&ctx, 972_504, 21096, cyno2_char, cyno2_ship, InventoryFlag::HiSlot0, 1);
        give_item(&ctx, 972_505, 16273, cyno2_char, cyno2_ship, InventoryFlag::Cargo, 2_000);
        let beacon2_id = ctx.dispatch_rpc(&mut cyno2_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(972_504), PyRep::String("cynosuralGeneration".into()), PyRep::Integer(0), PyRep::Integer(1)], &kw, None).await.expect("Cyno 2").dict_get("beaconID").and_then(|v| v.as_i64()).unwrap();

        let titan_c = 952_530;
        let titan_s = 962_530;
        let mut titan_sess = seed_pilot(&ctx, titan_c, titan_s, 11567, NULL_SYS, false);
        spawn_ship_in_space(&ctx, titan_c, titan_s, 11567, NULL_SYS, DVec3::ZERO).await;
        give_item(&ctx, 972_530, 23953, titan_c, titan_s, InventoryFlag::HiSlot0, 1);
        give_item(&ctx, 972_531, 16275, titan_c, titan_s, InventoryFlag::Cargo, 500);
        give_item(&ctx, 972_532, 16274, titan_c, titan_s, InventoryFlag::SpecializedFuelBay, 50_000);

        let jump2_char = 952_504;
        let jump2_ship = 962_504;
        let mut jump2_sess = seed_pilot(&ctx, jump2_char, jump2_ship, 23757, NULL_SYS, false);
        spawn_ship_in_space(&ctx, jump2_char, jump2_ship, 23757, NULL_SYS, DVec3::ZERO).await;
        give_item(&ctx, 972_506, 16274, jump2_char, jump2_ship, InventoryFlag::SpecializedFuelBay, 50_000);

        let bridge_p_char = 952_535;
        let bridge_p_ship = 962_535;
        let mut bridge_p_sess = seed_pilot(&ctx, bridge_p_char, bridge_p_ship, 624, NULL_SYS, false);
        spawn_ship_in_space(&ctx, bridge_p_char, bridge_p_ship, 624, NULL_SYS, DVec3::new(1_000.0, 0.0, 0.0)).await;

        put_in_fleet(&ctx, 880_006, cyno2_char, &[(cyno2_char, true), (titan_c, true), (jump2_char, true), (bridge_p_char, true)]);
        ctx.dispatch_rpc(&mut titan_sess, "beyonce", None, "CmdBridgeToFleetModuleBeacon", &[PyRep::Integer(cyno2_char), PyRep::Integer(DEST_NULL_SYS as i64), PyRep::Integer(beacon2_id)], &kw, None).await.expect("Open bridge before cyno dies");

        // Destroy cyno ship via real combat death pipeline
        let _ = eve_server::combat::death_pipeline::execute_death_pipeline(&ctx, DEST_NULL_SYS, cyno2_char, cyno2_ship, 999_999, DVec3::ZERO, ctx.now_unix_secs()).await.expect("Destroy cyno ship");

        ctx.dispatch_rpc(&mut jump2_sess, "beyonce", None, "CmdJumpToFleetModuleBeacon", &[PyRep::Integer(cyno2_char), PyRep::Integer(DEST_NULL_SYS as i64), PyRep::Integer(beacon2_id)], &kw, None).await.expect("Jump to destroyed cyno");
        ctx.dispatch_rpc(&mut bridge_p_sess, "beyonce", None, "CmdJumpThroughFleet", &[PyRep::Integer(titan_c), PyRep::Integer(titan_s)], &kw, None).await.expect("Bridge to destroyed cyno");
        let fb_jump = dest_sys.get_ball_snapshot(jump2_ship).await.unwrap().unwrap().1;
        let fb_bridge = dest_sys.get_ball_snapshot(bridge_p_ship).await.unwrap().unwrap().1;
        assert!(fb_jump.position.length() > star_radius && fb_jump.position.length() < star_radius + 50_000.0, "Jump to destroyed cyno must land outside star photosphere (got r={}, star={star_radius})", fb_jump.position.length());
        assert!(fb_bridge.position.length() > star_radius && fb_bridge.position.length() < star_radius + 50_000.0, "Bridge to destroyed cyno must also land outside star photosphere");
    }

    #[tokio::test]
    async fn test_b35_09_jump_drive_range_fuel() {
        // parity: feat:navigation.jump-drive-range-fuel
        eve_testkit::attest(&["feat:navigation.jump-drive-range-fuel"]);
        let ctx = build_ctx();
        let origin_sys = ctx.systems.get_or_spawn(NULL_SYS);
        let kw = HashMap::new();

        // 1. V24 Hull-Class Base Range Matrix + JDC (+20%/level) + all 4 Faction Isotopes!
        for &(hull_tid, expected_base_ly, expected_iso) in &[
            (19720u32, 3.5_f64, 16274u32), // Dreadnought (Revelation) -> 3.5 ly, Helium
            (23757, 3.5, 16274),           // Carrier (Archon) -> 3.5 ly, Helium
            (37604, 3.5, 16274),           // FAX (Apostle) -> 3.5 ly, Helium
            (23919, 3.0, 16274),           // Supercarrier (Aeon) -> 3.0 ly, Helium
            (11567, 3.0, 16274),           // Titan (Avatar) -> 3.0 ly, Helium
            (28844, 5.0, 17888),           // Jump Freighter (Rhea) -> 5.0 ly, Nitrogen
            (28352, 5.0, 17887),           // Rorqual -> 5.0 ly, Oxygen
            (22440, 4.0, 17889),           // Black Ops (Panther) -> 4.0 ly, Hydrogen
        ] {
            let plan0 = eve_server::services::space_destiny::space_nav_travel::plan_jump_drive(&ctx, 1, hull_tid, NULL_SYS, DEST_NULL_SYS).expect("plan_jump_drive");
            let t = ctx.sde.types.get(&hull_tid).unwrap();
            assert!((t.default_attributes[&867] - expected_base_ly).abs() < 1e-6, "Hull {hull_tid} base range mismatch");
            assert_eq!(plan0.fuel_type_id, expected_iso, "Hull {hull_tid} faction isotope mismatch");
        }

        let cyno_char = 953_001;
        let cyno_ship = 963_001;
        put_in_fleet(&ctx, 880_010, cyno_char, &[(cyno_char, true)]);
        let mut cyno_sess = seed_pilot(&ctx, cyno_char, cyno_ship, 11957, DEST_NULL_SYS, false);
        spawn_ship_in_space(&ctx, cyno_char, cyno_ship, 11957, DEST_NULL_SYS, DVec3::new(10_000.0, 0.0, 0.0)).await;
        give_item(&ctx, 973_001, 21096, cyno_char, cyno_ship, InventoryFlag::HiSlot0, 1);
        give_item(&ctx, 973_002, 16273, cyno_char, cyno_ship, InventoryFlag::Cargo, 2_000);
        let beacon_id = ctx.dispatch_rpc(&mut cyno_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(973_001), PyRep::String("cynosuralGeneration".into()), PyRep::Integer(0), PyRep::Integer(1)], &kw, None).await.expect("Cyno").dict_get("beaconID").and_then(|v| v.as_i64()).unwrap();

        // Far low-sec K-space system beyond 7.0 ly to verify JumpDistanceOutOfRange
        let far_sys = *ctx.sde.solar_systems.iter().find(|&(sid, s)| (30_000_000..31_000_000).contains(sid) && *sid != 30_100_000 && s.region_id != 10_000_070 && s.security_status < 0.4 && eve_server::services::space_destiny::space_nav_travel::distance_ly(&ctx, NULL_SYS, s.solar_system_id) > 15.0).unwrap().0;

        let car_char = 953_002;
        let car_ship = 963_002;
        let mut car_sess = seed_pilot(&ctx, car_char, car_ship, 23757, NULL_SYS, false);
        spawn_ship_in_space(&ctx, car_char, car_ship, 23757, NULL_SYS, DVec3::ZERO).await;
        set_skill(&ctx, car_char, 21611, 5); // JDC V -> 7.0 ly
        set_skill(&ctx, car_char, 21610, 5); // JFC V -> -50% fuel
        set_skill(&ctx, car_char, 3456, 5);  // JDO V -> 71.25% cap need
        put_in_fleet(&ctx, 880_010, car_char, &[(cyno_char, true), (car_char, true), (953_003, true), (953_004, true), (953_005, true), (953_006, true)]);

        // Out-of-range check & transactionality (failed jump without fuel preserves capacitor)
        let range_err = ctx.dispatch_rpc(&mut car_sess, "beyonce", None, "CmdJumpToFleetModuleBeacon", &[PyRep::Integer(cyno_char), PyRep::Integer(far_sys as i64), PyRep::Integer(beacon_id)], &kw, None).await.unwrap_err();
        assert_eq!(range_err.code, "JumpDistanceOutOfRange");
        let cap_before_fail = origin_sys.get_combat_state(car_ship).await.unwrap().unwrap().capacitor;
        assert!(ctx.dispatch_rpc(&mut car_sess, "beyonce", None, "CmdJumpToFleetModuleBeacon", &[PyRep::Integer(cyno_char), PyRep::Integer(DEST_NULL_SYS as i64), PyRep::Integer(beacon_id)], &kw, None).await.is_err());
        assert_eq!(origin_sys.get_combat_state(car_ship).await.unwrap().unwrap().capacitor, cap_before_fail);

        // Refused inside bubble AND when warp core strength < 0; Fuel Bay -> Cargo fallback + JDO V cap deduction
        give_item(&ctx, 973_003, 16274, car_char, car_ship, InventoryFlag::SpecializedFuelBay, 100);
        give_item(&ctx, 973_013, 16274, car_char, car_ship, InventoryFlag::Cargo, 100_000);
        let _ = origin_sys.add_warp_bubble(999_002, DVec3::ZERO, 20_000.0).await;
        assert!(ctx.dispatch_rpc(&mut car_sess, "beyonce", None, "CmdJumpToFleetModuleBeacon", &[PyRep::Integer(cyno_char), PyRep::Integer(DEST_NULL_SYS as i64), PyRep::Integer(beacon_id)], &kw, None).await.is_err());
        let _ = origin_sys.remove_warp_bubble(999_002).await;

        let mut cs_neg_wcs = origin_sys.get_combat_state(car_ship).await.unwrap().unwrap();
        cs_neg_wcs.warp_core_strength = -1;
        origin_sys.set_combat_state(car_ship, cs_neg_wcs).await.unwrap();
        assert!(ctx.dispatch_rpc(&mut car_sess, "beyonce", None, "CmdJumpToFleetModuleBeacon", &[PyRep::Integer(cyno_char), PyRep::Integer(DEST_NULL_SYS as i64), PyRep::Integer(beacon_id)], &kw, None).await.is_err(), "Jump must fail when WCS < 0");

        let mut cs_ok = origin_sys.get_combat_state(car_ship).await.unwrap().unwrap();
        cs_ok.warp_core_strength = 0;
        cs_ok.capacitor = cs_ok.max_capacitor;
        let max_cap = cs_ok.max_capacitor;
        origin_sys.set_combat_state(car_ship, cs_ok).await.unwrap();

        // Unaligned stationary ship jumps cleanly, exhausting Fuel Bay (100) then Cargo!
        let jump_ok = ctx.dispatch_rpc(&mut car_sess, "beyonce", None, "CmdJumpToFleetModuleBeacon", &[PyRep::Integer(cyno_char), PyRep::Integer(DEST_NULL_SYS as i64), PyRep::Integer(beacon_id)], &kw, None).await.expect("Jump succeeds");
        let dist_ly = jump_ok.dict_get("distanceLY").and_then(|v| v.as_f64()).unwrap();
        let car_fuel = jump_ok.dict_get("fuelConsumed").and_then(|v| v.as_i64()).unwrap();
        assert_eq!(jump_ok.dict_get("fuelTypeID"), Some(&PyRep::Integer(16274)));
        assert!(ctx.db.with_snapshot(|s| !s.items.contains_key(&973_003)), "Fuel Bay must be drained before Cargo");
        assert_eq!(ctx.db.with_snapshot(|s| s.items.get(&973_013).unwrap().quantity), 100_000 - (car_fuel as i32 - 100));
        let dest_sys = ctx.systems.get_or_spawn(DEST_NULL_SYS);
        let cap_after = dest_sys.get_combat_state(car_ship).await.unwrap().unwrap().capacitor;
        assert!((cap_after - max_cap * (1.0 - 0.95 * 0.75)).abs() < 1.0, "JDO V must leave 28.75% capacitor");

        // 2. Jump Freighter (Rhea 28844, raw 10,000/ly): verify JFC + JF skill + stacking-penalized Prototype Jump Drive Economizers (34126)
        // INCLUDING at jf_lvl == 0 (BF-B35-06 regression check)!
        let s1 = eve_dogma::stacking_penalty_factor(1);
        for (idx, cid, jfc_l, jf_l, n_econ) in [
            (0usize, 953_003i64, 0u8, 0u8, 0usize), // raw 10,000 * D
            (1, 953_004, 4, 0, 2),                  // jf_lvl == 0 WITH JFC IV (-40%) + 2x Economizer (-10% stacking-penalized)!
            (2, 953_005, 0, 5, 0),                  // JF V (-50%)
            (3, 953_006, 5, 5, 2),                  // JFC V (-50%) * JF V (-50%) * 2x Economizer!
        ] {
            let sid = 963_003 + idx as i64;
            let mut s = seed_pilot(&ctx, cid, sid, 28844, NULL_SYS, false);
            spawn_ship_in_space(&ctx, cid, sid, 28844, NULL_SYS, DVec3::new((idx as f64 + 1.0) * 500.0, 0.0, 0.0)).await;
            give_item(&ctx, 973_020 + idx as i64 * 4, 17888, cid, sid, InventoryFlag::SpecializedFuelBay, 50_000);
            set_skill(&ctx, cid, 21611, 5);
            set_skill(&ctx, cid, 21610, jfc_l);
            set_skill(&ctx, cid, 29029, jf_l);
            if n_econ >= 1 {
                give_item(&ctx, 973_021 + idx as i64 * 4, 34126, cid, sid, InventoryFlag::LoSlot0, 1);
            }
            if n_econ >= 2 {
                give_item(&ctx, 973_022 + idx as i64 * 4, 34126, cid, sid, InventoryFlag::LoSlot1, 1);
            }
            ctx.invalidate_ship_fitting(sid);
            let res = ctx.dispatch_rpc(&mut s, "beyonce", None, "CmdJumpToFleetModuleBeacon", &[PyRep::Integer(cyno_char), PyRep::Integer(DEST_NULL_SYS as i64), PyRep::Integer(beacon_id)], &kw, None).await.expect("JF jump");
            let got_fuel = res.dict_get("fuelConsumed").and_then(|v| v.as_i64()).unwrap();
            let mut expected_rate = 10_000.0 * (1.0 - 0.10 * jfc_l as f64) * (1.0 - 0.10 * jf_l as f64);
            if n_econ >= 1 { expected_rate *= 0.90; }
            if n_econ >= 2 { expected_rate *= 1.0 - 0.10 * s1; }
            let expected_fuel = (expected_rate * dist_ly).ceil() as i64;
            assert_eq!(got_fuel, expected_fuel, "JF fuel mismatch for JFC={jfc_l}, JF={jf_l}, econ={n_econ}");
        }
    }
}

pub(crate) mod align_server_tick_rounding {
    // Dedicated integration tests for open `feat:navigation.*` parity rows (Batch 35, Part 3).

    use crate::modules_advanced_tactical::{build_ctx, seed_pilot, NULL_SYS};
    use crate::nav_tactical_modules::{give_item, set_skill, spawn_ship_in_space};
    use eve_db::InventoryFlag;
    use eve_destiny::{Ball, DVec3, MovementMode};
    use eve_proto::PyRep;
    use std::collections::HashMap;

    const ZARZAKH_SYS: u32 = 30_100_000;

    #[tokio::test]
    async fn test_b35_10_align_server_tick_rounding() {
        // parity: feat:navigation.align-server-tick-rounding
        eve_testkit::attest(&["feat:navigation.align-server-tick-rounding"]);
        let ctx = build_ctx();
        let sys = ctx.systems.get_or_spawn(NULL_SYS);
        let kw = HashMap::new();

        // Negative/abuse cases (>= 2): unauthenticated & invalid-target CmdWarpToStuff rejected
        let mut unauth = eve_machonet::SessionState::new(1);
        assert!(ctx.dispatch_rpc(&mut unauth, "beyonce", None, "CmdWarpToStuff", &[PyRep::String("item".into()), PyRep::Integer(40301369)], &kw, None).await.is_err());

        let target_celestial_id = 40301369; // 1DQ1-A I planet in NULL_SYS

        // Three stationary Executioners (589) spawned via real Dogma fitting (`ensure_player_ship_ball_spawned`)
        // and commanded to warp from rest via live `beyonce.CmdWarpToStuff`:
        //   Case 0: SC IV, EM V, 2x Inertial Stabilizers II -> ~1.964s (< 2.0s) -> enters warp on tick 2!
        //   Case 1: SC III, EM V, 2x Inertial Stabilizers II -> ~2.007s (> 2.0s) -> enters warp on tick 3!
        //   Case 2: SC V, EM IV, 0x Inertial Stabilizers II  -> ~3.101s (> 3.0s) -> enters warp on tick 4!
        for (idx, sc_lvl, em_lvl, n_istab, expected_ticks) in [
            (0i64, 4u8, 5u8, 2usize, 2u64),
            (1, 3, 5, 2, 3),
            (2, 5, 4, 0, 4),
        ] {
            let cid = 953_100 + idx;
            let sid = 963_100 + idx;
            let mut sess = seed_pilot(&ctx, cid, sid, 589, NULL_SYS, false);
            set_skill(&ctx, cid, 3331, 5); // Amarr Frigate
            set_skill(&ctx, cid, 3394, 5); // Hull Upgrades
            set_skill(&ctx, cid, 3426, 5); // CPU Management
            set_skill(&ctx, cid, 3413, 5); // Power Grid Management
            set_skill(&ctx, cid, 3327, sc_lvl); // Spaceship Command
            set_skill(&ctx, cid, 3453, em_lvl); // Evasive Maneuvering
            if n_istab >= 1 {
                give_item(&ctx, 973_110 + idx * 4, 1405, cid, sid, InventoryFlag::LoSlot0, 1);
            }
            if n_istab >= 2 {
                give_item(&ctx, 973_111 + idx * 4, 1405, cid, sid, InventoryFlag::LoSlot1, 1);
            }
            ctx.invalidate_ship_fitting(sid);
            ctx.db.with_mut_snapshot(|s| {
                s.item_positions.insert(sid, [idx as f64 * 10_000.0, 0.0, 0.0]);
            });
            let beyonce_b = ctx.bind_object(&mut sess, "beyonce", NULL_SYS as i64).unwrap();
            ctx.dispatch_rpc(&mut sess, "beyonce", Some(beyonce_b), "UpdateStateRequest", &[], &kw, None)
                .await
                .expect("Spawn player ship ball via UpdateStateRequest");
            let mut cs = sys.get_combat_state(sid).await.unwrap().unwrap();
            cs.undock_invulnerable_remaining_ticks = 0;
            sys.set_combat_state(sid, cs).await.unwrap();

            if idx == 0 {
                assert!(ctx.dispatch_rpc(&mut sess, "beyonce", Some(beyonce_b), "CmdWarpToStuff", &[PyRep::String("item".into()), PyRep::Integer(-999)], &kw, None).await.is_err());
            }

            let (_, b0) = sys.get_ball_snapshot(sid).await.unwrap().unwrap();
            assert_eq!(b0.mode, MovementMode::Stop, "Ship must start completely stationary before CmdWarpToStuff");
            assert_eq!(b0.align_server_ticks(), expected_ticks, "Dogma align_server_ticks mismatch for case {idx} (align_s={})", b0.align_time_seconds());
            let align_threshold = b0.max_velocity * 0.75;

            ctx.dispatch_rpc(&mut sess, "beyonce", Some(beyonce_b), "CmdWarpToStuff", &[PyRep::String("item".into()), PyRep::Integer(target_celestial_id), PyRep::Real(0.0)], &kw, None)
                .await
                .expect("CmdWarpToStuff from stationary");

            for _ in 0..(expected_ticks - 1) {
                let _ = sys.step_manual(1.0).await;
            }
            let (_, b_before) = sys.get_ball_snapshot(sid).await.unwrap().unwrap();
            assert!(
                matches!(b_before.mode, MovementMode::Warp { phase: eve_destiny::WarpPhase::Aligning, .. }),
                "Case {idx} ({expected_ticks} ticks): must still be Aligning at tick {}",
                expected_ticks - 1
            );
            assert!(b_before.velocity.length() < align_threshold, "Velocity before tick {expected_ticks} must be < 75% max velocity");

            let _ = sys.step_manual(1.0).await;
            let (_, b_enter) = sys.get_ball_snapshot(sid).await.unwrap().unwrap();
            assert!(
                matches!(b_enter.mode, MovementMode::Warp { phase: eve_destiny::WarpPhase::Accelerating | eve_destiny::WarpPhase::Cruising, .. }),
                "Case {idx}: must enter warp on whole server tick {expected_ticks}"
            );
            assert!(b_enter.velocity.length() >= align_threshold, "Velocity on tick {expected_ticks} must reach >= 75% max velocity");
            let _ = sys.remove_ball(sid).await;
        }
    }

    #[tokio::test]
    async fn test_b35_11_cloak_velocity_penalty() {
        // parity: feat:navigation.cloak-velocity-penalty
        eve_testkit::attest(&["feat:navigation.cloak-velocity-penalty"]);
        let ctx = build_ctx();
        let sys = ctx.systems.get_or_spawn(NULL_SYS);
        let kw = HashMap::new();

        // Negative/abuse cases (>= 2):
        // 1. Unauthenticated Activate rejected
        let mut unauth = eve_machonet::SessionState::new(1);
        assert!(ctx.dispatch_rpc(&mut unauth, "dogmaIM", None, "Activate", &[PyRep::Integer(1)], &kw, None).await.is_err());

        let c_clk = 953_102;
        let s_clk = 963_102;
        let mut sess_clk = seed_pilot(&ctx, c_clk, s_clk, 641, NULL_SYS, false); // Megathron
        set_skill(&ctx, c_clk, 3336, 5);  // Gallente Battleship V
        set_skill(&ctx, c_clk, 11579, 4); // Cloaking IV
        spawn_ship_in_space(&ctx, c_clk, s_clk, 641, NULL_SYS, DVec3::new(500_000_000.0, 0.0, 0.0)).await;
        let (_, b_init) = sys.get_ball_snapshot(s_clk).await.unwrap().unwrap();
        let base_vmax = b_init.max_velocity;

        // 2. Exploit regression check (BF-B35-04): Covert Ops Cloaking Device II (11578) on Megathron (641) MUST FAIL!
        let illegal_cov = 973_199;
        give_item(&ctx, illegal_cov, 11578, c_clk, s_clk, InventoryFlag::HiSlot2, 1);
        ctx.invalidate_ship_fitting(s_clk);
        assert!(
            ctx.dispatch_rpc(&mut sess_clk, "dogmaIM", None, "Activate", &[PyRep::Integer(illegal_cov), PyRep::String("cloakingWarpSafe".into()), PyRep::None, PyRep::Integer(1)], &kw, None).await.is_err(),
            "Megathron battleship must NEVER be allowed to fit or activate Covert Ops Cloaking Device II (11578)"
        );
        ctx.db.with_mut_snapshot(|s| { s.items.remove(&illegal_cov); });
        ctx.invalidate_ship_fitting(s_clk);

        // 1. Prototype Cloaking Device I (11370): -90% velocity (0.10x), propulsion blocked while cloaked, persists across ticks, restored on live Deactivate
        let proto_clk = 973_101;
        let ab_mod = 973_109;
        set_skill(&ctx, c_clk, 3449, 5); // Navigation V
        set_skill(&ctx, c_clk, 3450, 5); // Afterburner V
        give_item(&ctx, proto_clk, 11370, c_clk, s_clk, InventoryFlag::HiSlot0, 1);
        give_item(&ctx, ab_mod, 12058, c_clk, s_clk, InventoryFlag::MedSlot0, 1); // 100MN Afterburner II
        ctx.invalidate_ship_fitting(s_clk);
        ctx.dispatch_rpc(&mut sess_clk, "dogmaIM", None, "Activate", &[PyRep::Integer(proto_clk), PyRep::String("cloaking".into()), PyRep::None, PyRep::Integer(1)], &kw, None).await.expect("Activate Prototype Cloak");
        let (_, b_cloaked) = sys.get_ball_snapshot(s_clk).await.unwrap().unwrap();
        assert!((b_cloaked.max_velocity - base_vmax * 0.10).abs() < 1e-3, "Prototype cloak must reduce max velocity by 90% (got {} vs {})", b_cloaked.max_velocity, base_vmax * 0.10);
        assert_eq!(
            ctx.dispatch_rpc(&mut sess_clk, "dogmaIM", None, "Activate", &[PyRep::Integer(ab_mod), PyRep::String("moduleBonusAfterburner".into()), PyRep::None, PyRep::Integer(1)], &kw, None).await.unwrap_err().code,
            "CannotActivateCloaked",
            "Activating Afterburner while cloaked under Prototype Cloak must be rejected"
        );
        let _ = sys.step_manual(3.0).await;
        let (_, b_cloaked_3s) = sys.get_ball_snapshot(s_clk).await.unwrap().unwrap();
        assert!((b_cloaked_3s.max_velocity - base_vmax * 0.10).abs() < 1e-3, "Cloak velocity penalty must remain enforced across server ticks");

        ctx.dispatch_rpc(&mut sess_clk, "dogmaIM", None, "Deactivate", &[PyRep::Integer(proto_clk), PyRep::String("cloaking".into())], &kw, None).await.expect("Deactivate Prototype Cloak");
        let _ = sys.step_manual(1.0).await;
        let (_, b_unclk) = sys.get_ball_snapshot(s_clk).await.unwrap().unwrap();
        assert!((b_unclk.max_velocity - base_vmax).abs() < 1e-3, "Deactivating cloak must restore max velocity");

        // 2. Improved Cloaking Device II (11577): -75% velocity (0.25x), propulsion blocked while cloaked, restored on live Deactivate
        ctx.advance_time_seconds(35);
        let imp_clk = 973_102;
        give_item(&ctx, imp_clk, 11577, c_clk, s_clk, InventoryFlag::HiSlot1, 1);
        ctx.invalidate_ship_fitting(s_clk);
        ctx.dispatch_rpc(&mut sess_clk, "dogmaIM", None, "Activate", &[PyRep::Integer(imp_clk), PyRep::String("cloaking".into()), PyRep::None, PyRep::Integer(1)], &kw, None).await.expect("Activate Improved Cloak");
        assert_eq!(
            ctx.dispatch_rpc(&mut sess_clk, "dogmaIM", None, "Activate", &[PyRep::Integer(ab_mod), PyRep::String("moduleBonusAfterburner".into()), PyRep::None, PyRep::Integer(1)], &kw, None).await.unwrap_err().code,
            "CannotActivateCloaked",
            "Activating Afterburner while cloaked under Improved Cloak must be rejected"
        );
        let (_, b_imp) = sys.get_ball_snapshot(s_clk).await.unwrap().unwrap();
        assert!((b_imp.max_velocity - base_vmax * 0.25).abs() < 1e-3, "Improved cloak must reduce max velocity by 75%");
        ctx.dispatch_rpc(&mut sess_clk, "dogmaIM", None, "Deactivate", &[PyRep::Integer(imp_clk), PyRep::String("cloaking".into())], &kw, None).await.expect("Deactivate Improved Cloak");
        let _ = sys.step_manual(1.0).await;

        // 3. Covert Ops Cloaking Device II (11578) on legal Covert Ops hull (Helios 11172): 0% velocity penalty (1.00x)
        let c_cov = 953_120;
        let s_cov = 963_120;
        let mut sess_cov = seed_pilot(&ctx, c_cov, s_cov, 11172, NULL_SYS, false);
        set_skill(&ctx, c_cov, 3328, 5);  // Gallente Frigate V
        set_skill(&ctx, c_cov, 12093, 4); // Covert Ops IV
        set_skill(&ctx, c_cov, 11579, 4); // Cloaking IV
        spawn_ship_in_space(&ctx, c_cov, s_cov, 11172, NULL_SYS, DVec3::new(500_100_000.0, 0.0, 0.0)).await;
        let (_, b_cov_init) = sys.get_ball_snapshot(s_cov).await.unwrap().unwrap();
        let cov_base_vmax = b_cov_init.max_velocity;

        let cov_clk = 973_103;
        give_item(&ctx, cov_clk, 11578, c_cov, s_cov, InventoryFlag::HiSlot0, 1);
        ctx.invalidate_ship_fitting(s_cov);
        ctx.dispatch_rpc(&mut sess_cov, "dogmaIM", None, "Activate", &[PyRep::Integer(cov_clk), PyRep::String("cloakingWarpSafe".into()), PyRep::None, PyRep::Integer(1)], &kw, None).await.expect("Activate Covert Ops Cloak on Helios");
        let (_, b_cov) = sys.get_ball_snapshot(s_cov).await.unwrap().unwrap();
        assert!((b_cov.max_velocity - cov_base_vmax).abs() < 1e-3, "Covert Ops cloak on Helios must incur 0% velocity penalty");
    }

    #[tokio::test]
    async fn test_b35_12_collision_bumping() {
        let ctx = build_ctx();
        let sys = ctx.systems.get_or_spawn(NULL_SYS);

        let c_heavy = 953_201;
        let s_heavy = 963_201;
        seed_pilot(&ctx, c_heavy, s_heavy, 641, NULL_SYS, false);
        let m_heavy = ctx.sde.types.get(&641).map(|t| t.mass).unwrap_or(100_000_000.0);
        let r_heavy = ctx.sde.types.get(&641).map(|t| t.radius).unwrap_or(300.0);

        let c_light = 953_202;
        let s_light = 963_202;
        seed_pilot(&ctx, c_light, s_light, 587, NULL_SYS, false);
        let m_light = ctx.sde.types.get(&587).map(|t| t.mass).unwrap_or(1_000_000.0);
        let r_light = ctx.sde.types.get(&587).map(|t| t.radius).unwrap_or(40.0);

        let _ = sys.spawn_ball(Ball {
            id: s_heavy, type_id: 641, position: DVec3::ZERO, velocity: DVec3::new(100.0, 0.0, 0.0),
            max_velocity: 100.0, inertia: 1_000_000.0, mass: m_heavy, radius: r_heavy, speed_fraction: 0.0,
            mode: MovementMode::Stop,
        }).await;
        let _ = sys.spawn_ball(Ball {
            id: s_light, type_id: 587, position: DVec3::new(200.0, 0.0, 0.0), velocity: DVec3::ZERO,
            max_velocity: 300.0, inertia: 1_000_000.0, mass: m_light, radius: r_light, speed_fraction: 0.0, mode: MovementMode::Stop,
        }).await;

        let _ = sys.step_manual(1.0).await;
        let (_, b_h) = sys.get_ball_snapshot(s_heavy).await.unwrap().unwrap();
        let (_, b_l) = sys.get_ball_snapshot(s_light).await.unwrap().unwrap();
        assert!(b_h.position.distance(b_l.position) >= r_heavy + r_light - 1.0, "Colliding ships must separate with no interpenetration");
        assert!(b_l.velocity.length() > (b_h.velocity - DVec3::new(100.0, 0.0, 0.0)).length(), "Lighter ship must receive larger velocity impulse than heavier ship");
    }

    #[tokio::test]
    async fn test_b35_13_drifter_wormhole_systems() {
        let ctx = build_ctx();
        for sys_id in [31_000_001, 31_000_002, 31_000_003, 31_000_004, 31_000_006] {
            let (sigs, _) = eve_server::services::space_destiny::space_docking_cloaking::ensure_system_signatures_and_anomalies(&ctx, sys_id).await;
            let wh_sigs: Vec<_> = sigs.iter().filter(|s| s.wormhole_type_id.is_some()).collect();
            assert_eq!(wh_sigs.len(), sigs.len(), "Every signature in Drifter system {sys_id} must be a wormhole");
            assert!(wh_sigs.len() >= 62, "Drifter system {sys_id} must have 6 statics + 56 K-space connections, got {}", wh_sigs.len());
        }
    }

    #[tokio::test]
    async fn test_b35_14_gravity_hazard_safe_zones() {
        // parity: feat:navigation.gravity-hazard-safe-zones
        eve_testkit::attest(&["feat:navigation.gravity-hazard-safe-zones"]);
        let ctx = build_ctx();
        let z_sys = ctx.systems.get_or_spawn(ZARZAKH_SYS);

        // 1. Inside Jovian safe zone (The Fulcrum): 0 damage on live 1 Hz server tick (`try_step_full_tick`)
        let c_safe = 953_103;
        let s_safe = 963_103;
        seed_pilot(&ctx, c_safe, s_safe, 587, ZARZAKH_SYS, false);
        let fulcrum_pos = ctx.sde.stations.get(&60015187).map(|s| s.position).unwrap_or(DVec3::ZERO);
        spawn_ship_in_space(&ctx, c_safe, s_safe, 587, ZARZAKH_SYS, fulcrum_pos + DVec3::new(10_000.0, 0.0, 0.0)).await;

        // 2. Outside safe zones: takes periodic HULL-ONLY damage (shield & armor remain 100% untouched),
        // and reaching 0 hull triggers full death/killmail pipeline (wreck + capsule + killmail)!
        let c_haz = 953_104;
        let s_haz = 963_104;
        seed_pilot(&ctx, c_haz, s_haz, 587, ZARZAKH_SYS, false); // Rifter (~350 structure HP)
        spawn_ship_in_space(&ctx, c_haz, s_haz, 587, ZARZAKH_SYS, DVec3::new(500_000_000.0, 0.0, 0.0)).await;

        let safe_before = z_sys.get_combat_state(s_safe).await.unwrap().unwrap();
        let haz_before = z_sys.get_combat_state(s_haz).await.unwrap().unwrap();

        // Step 1 live server tick via `try_step_full_tick(1.0)`
        let _ = z_sys.try_step_full_tick(1.0);
        tokio::task::yield_now().await;

        let safe_after = z_sys.get_combat_state(s_safe).await.unwrap().unwrap();
        let haz_after = z_sys.get_combat_state(s_haz).await.unwrap().unwrap();
        assert_eq!(safe_after.structure_hp, safe_before.structure_hp, "Ship inside Zarzakh safe zone must take 0 hull damage");
        assert_eq!(haz_after.shield_hp, haz_before.shield_hp, "Zarzakh gravity hazard must leave shield HP untouched");
        assert_eq!(haz_after.armor_hp, haz_before.armor_hp, "Zarzakh gravity hazard must leave armor HP untouched");
        assert!((haz_before.structure_hp - haz_after.structure_hp - 50.0).abs() < 1e-3, "Zarzakh gravity hazard must deal periodic hull damage");

        // Step live ticks until hull reaches 0 -> full death pipeline spawns wreck, capsule, and killmail!
        for _ in 0..10 {
            let _ = z_sys.try_step_full_tick(1.0);
            tokio::task::yield_now().await;
        }
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }
        assert!(z_sys.get_ball_snapshot(s_haz).await.unwrap().is_none(), "Destroyed ship ball must be removed at 0 hull");
        let (active_ship_type, has_killmail, has_wreck) = ctx.db.with_snapshot(|snap| {
            let active_sid = snap.characters.get(&c_haz).map(|c| c.active_ship_id).unwrap_or(0);
            let active_tid = snap.items.get(&active_sid).map(|i| i.type_id).unwrap_or(0);
            let km = snap.killmails.values().any(|k| k.victim_character_id == c_haz && k.solar_system_id == ZARZAKH_SYS);
            let wreck = snap.items.values().any(|i| i.location_id == ZARZAKH_SYS as i64 && ctx.sde.types.get(&i.type_id).is_some_and(|t| t.group_id == 186));
            (active_tid, km, wreck)
        });
        assert_eq!(active_ship_type, 670, "Destroyed pilot in Zarzakh hazard must transition into a Capsule (670)");
        assert!(has_wreck, "Zarzakh hazard ship destruction must spawn a wreck");
        assert!(has_killmail, "Zarzakh hazard ship destruction must generate a killmail");
    }

    #[tokio::test]
    async fn test_b35_15_interdiction_nullification_module() {
        // parity: feat:navigation.interdiction-nullification-module
        eve_testkit::attest(&["feat:navigation.interdiction-nullification-module"]);
        let ctx = build_ctx();
        let sys = ctx.systems.get_or_spawn(NULL_SYS);
        let kw = HashMap::new();

        // Negative/abuse case 1: unauthenticated Activate rejected
        let mut unauth = eve_machonet::SessionState::new(1);
        assert!(ctx.dispatch_rpc(&mut unauth, "dogmaIM", None, "Activate", &[PyRep::Integer(1)], &kw, None).await.is_err());

        // 1. Passive penalties on fitting: scan resolution, targeting range, and -50% drone bandwidth
        let c_base = 953_106;
        let s_base = 963_106;
        let mut sess_drag = seed_pilot(&ctx, c_base, s_base, 11172, NULL_SYS, false); // Helios (unnullified control for drag bubble!)
        set_skill(&ctx, c_base, 3328, 5);
        set_skill(&ctx, c_base, 12093, 4);
        let ev_before = ctx.get_or_rebuild_ship_fitting(c_base, s_base).unwrap().evaluated_context(&ctx.sde);
        let scan_res_before = ev_before.ship.get_attribute(564, &ctx.sde);
        let target_range_before = ev_before.ship.get_attribute(76, &ctx.sde);
        assert_eq!(ev_before.ship.get_attribute(1271, &ctx.sde), 5.0);

        let c_null = 953_105;
        let s_null = 963_105;
        let mut sess_null = seed_pilot(&ctx, c_null, s_null, 11172, NULL_SYS, false);
        set_skill(&ctx, c_null, 3328, 5);
        set_skill(&ctx, c_null, 12093, 4);
        set_skill(&ctx, c_null, 3455, 4); // Warp Drive Operation / Navigation req for 58919
        spawn_ship_in_space(&ctx, c_null, s_null, 11172, NULL_SYS, DVec3::ZERO).await;
        let null_mod = 973_104;
        give_item(&ctx, null_mod, 58919, c_null, s_null, InventoryFlag::HiSlot0, 1); // Interdiction Nullifier I
        ctx.db.with_mut_snapshot(|snap| { snap.module_online_states.insert(null_mod, true); });
        ctx.invalidate_ship_fitting(s_null);
        let ev_after = ctx.get_or_rebuild_ship_fitting(c_null, s_null).unwrap().evaluated_context(&ctx.sde);
        assert!(ev_after.ship.get_attribute(564, &ctx.sde) < scan_res_before);
        assert!(ev_after.ship.get_attribute(76, &ctx.sde) < target_range_before);
        assert!((ev_after.ship.get_attribute(1271, &ctx.sde) - 2.5).abs() < 1e-3);

        // 2. Origin bubble + Destination drag/catch bubble along warp vector to planet 40301369
        let target_celestial_id = 40301369;
        let planet = ctx.sde.celestials_in_system(NULL_SYS).into_iter().find(|c| c.celestial_id == target_celestial_id).unwrap();
        let planet_pos = planet.position;
        let warp_dir = planet_pos.normalize();
        let drag_bubble_pos = planet_pos - warp_dir * (planet.radius + 2_500.0 + 120_000.0); // Catch bubble 120km short of planet warp-in point!
        let _ = sys.add_warp_bubble(999_888, DVec3::ZERO, 20_000.0).await;
        let _ = sys.add_warp_bubble(999_889, drag_bubble_pos, 20_000.0).await;

        // Negative/abuse case 2: unnullified warp inside origin bubble fails with WarpScrambled
        let warp_err = ctx.dispatch_rpc(&mut sess_null, "beyonce", None, "CmdWarpToStuff", &[PyRep::String("item".into()), PyRep::Integer(target_celestial_id), PyRep::Real(0.0)], &kw, None).await.unwrap_err();
        assert_eq!(warp_err.code, "WarpScrambled");

        // Spawn unnullified Helios outside origin bubble (50km perpendicular) and warp to same planet -> caught by drag bubble!
        spawn_ship_in_space(&ctx, c_base, s_base, 11172, NULL_SYS, DVec3::new(0.0, 50_000.0, 0.0)).await;
        ctx.dispatch_rpc(&mut sess_drag, "beyonce", None, "CmdWarpToStuff", &[PyRep::String("item".into()), PyRep::Integer(target_celestial_id), PyRep::Real(0.0)], &kw, None).await.expect("Unnullified warp from outside origin bubble");

        // Activate Interdiction Nullifier on s_null -> escapes origin bubble AND ignores destination drag/catch bubble!
        ctx.dispatch_rpc(&mut sess_null, "dogmaIM", None, "Activate", &[PyRep::Integer(null_mod), PyRep::String("defaultEffect".into()), PyRep::None, PyRep::Integer(1)], &kw, None).await.expect("Activate Interdiction Nullifier");
        assert!(sys.get_combat_state(s_null).await.unwrap().unwrap().is_interdiction_nullified);
        ctx.dispatch_rpc(&mut sess_null, "beyonce", None, "CmdWarpToStuff", &[PyRep::String("item".into()), PyRep::Integer(target_celestial_id), PyRep::Real(0.0)], &kw, None).await.expect("Nullified warp from inside origin bubble");

        // Step warp to completion so both ships exit warp
        for _ in 0..80 {
            let _ = sys.step_manual(1.0).await;
        }
        let (_, b_dragged) = sys.get_ball_snapshot(s_base).await.unwrap().unwrap();
        let (_, b_null_landed) = sys.get_ball_snapshot(s_null).await.unwrap().unwrap();
        let dist_dragged = b_dragged.position.distance(planet_pos);
        let dist_null = b_null_landed.position.distance(planet_pos);
        assert!(
            dist_dragged > dist_null + 80_000.0,
            "Unnullified ship must be pulled short by catch bubble ({dist_dragged}m from planet vs nullified {dist_null}m)"
        );

        let _ = sys.remove_warp_bubble(999_888).await;
        let _ = sys.remove_warp_bubble(999_889).await;
    }
}

pub(crate) mod nanite_repair_paste_in_space {
    // Dedicated integration tests for open `feat:modules.*` parity rows (Batch 35, Part 4: Nanite Repair Paste & Industrial Cyno).

    use crate::modules_advanced_tactical::{build_ctx, seed_pilot, NULL_SYS};
    use crate::nav_tactical_modules::{give_item, put_in_fleet, set_skill, spawn_ship_in_space, DEST_NULL_SYS};
    use eve_db::{InventoryFlag, IskCents, UpwellStructureRecord};
    use eve_destiny::DVec3;
    use eve_proto::PyRep;
    use std::collections::HashMap;

    #[tokio::test]
    async fn test_b35_04_nanite_repair_paste_in_space() {
        // parity: feat:modules.nanite-repair-paste-in-space
        eve_testkit::attest(&["feat:modules.nanite-repair-paste-in-space"]);
        let ctx = build_ctx();
        let sys = ctx.systems.get_or_spawn(NULL_SYS);
        let kw = HashMap::new();

        // Negative/abuse case 1: unauthenticated InitiateModuleRepair & StopModuleRepair rejected
        let mut unauth = eve_machonet::SessionState::new(1);
        assert!(ctx.dispatch_rpc(&mut unauth, "dogmaIM", None, "InitiateModuleRepair", &[PyRep::Integer(1)], &kw, None).await.is_err());
        assert!(ctx.dispatch_rpc(&mut unauth, "dogmaIM", None, "StopModuleRepair", &[PyRep::Integer(1)], &kw, None).await.is_err());

        let char_id = 951_301;
        let ship_id = 961_301;
        let mut sess = seed_pilot(&ctx, char_id, ship_id, 641, NULL_SYS, false);
        spawn_ship_in_space(&ctx, char_id, ship_id, 641, NULL_SYS, DVec3::ZERO).await;

        // Three modules of 5 m³, 10 m³, and 20 m³ volume, all using real SDE HP_STRUCT = 40.0 HP
        let mod_5m3 = 971_301;  // Small Focused Pulse Laser II (3041, vol = 5 m³, hp = 40)
        let mod_10m3 = 971_302; // Heavy Pulse Laser II (3520, vol = 10 m³, hp = 40)
        let mod_20m3 = 971_303; // Mega Pulse Laser II (3057, vol = 20 m³, hp = 40)
        let paste_id = 971_310;
        give_item(&ctx, mod_5m3, 3041, char_id, ship_id, InventoryFlag::HiSlot0, 1);
        give_item(&ctx, mod_10m3, 3520, char_id, ship_id, InventoryFlag::HiSlot1, 1);
        give_item(&ctx, mod_20m3, 3057, char_id, ship_id, InventoryFlag::HiSlot2, 1);
        give_item(&ctx, paste_id, 28668, char_id, ship_id, InventoryFlag::Cargo, 500);

        // 1. Burned-out module (40.0 damage on real 40.0 HP_STRUCT) cannot be repaired in space with paste
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.get_mut(&mod_5m3).unwrap().dynamic_attributes.insert(3, 40.0);
        });
        assert!(
            ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "InitiateModuleRepair", &[PyRep::Integer(mod_5m3)], &kw, None).await.is_err(),
            "Fully burned-out module (40.0 / 40.0 HP) must not be repairable in space with Nanite Repair Paste"
        );

        // 2. Active module cannot be repaired until deactivated
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.get_mut(&mod_5m3).unwrap().dynamic_attributes.insert(3, 20.0);
            snap.module_active_states.insert(mod_5m3);
        });
        assert!(
            ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "InitiateModuleRepair", &[PyRep::Integer(mod_5m3)], &kw, None).await.is_err(),
            "Active module must be rejected from InitiateModuleRepair"
        );
        ctx.db.with_mut_snapshot(|snap| {
            snap.module_active_states.remove(&mod_5m3);
        });

        // 3. SDE Nanite Repair Paste formula at 20.0 HP with Thermo 0, Nanite Op 0, Nanite Interfacing 0:
        // SDE attrs: shipBrokenModuleRepairCostMultiplier (1277) = 0.5 paste/HP -> 20 * 0.5 = 10 paste; moduleRepairRate (1267) = 10.0 HP/s
        set_skill(&ctx, char_id, 28164, 0); // Thermodynamics 0
        set_skill(&ctx, char_id, 28879, 0); // Nanite Operation 0
        set_skill(&ctx, char_id, 28880, 0); // Nanite Interfacing 0
        for &mid in &[mod_5m3, mod_10m3, mod_20m3] {
            ctx.db.with_mut_snapshot(|snap| {
                snap.items.get_mut(&mid).unwrap().dynamic_attributes.insert(3, 20.0);
            });
        }
        let rep_5 = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "InitiateModuleRepair", &[PyRep::Integer(mod_5m3)], &kw, None).await.expect("Repair 5m3");
        let rep_10 = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "InitiateModuleRepair", &[PyRep::Integer(mod_10m3)], &kw, None).await.expect("Repair 10m3");
        let rep_20 = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "InitiateModuleRepair", &[PyRep::Integer(mod_20m3)], &kw, None).await.expect("Repair 20m3");
        let c5 = rep_5.dict_get("totalPasteRequired").and_then(|v| v.as_i64()).unwrap();
        let c10 = rep_10.dict_get("totalPasteRequired").and_then(|v| v.as_i64()).unwrap();
        let c20 = rep_20.dict_get("totalPasteRequired").and_then(|v| v.as_i64()).unwrap();
        assert_eq!(rep_20.dict_get("pasteConsumed").and_then(|v| v.as_i64()), Some(1), "Streaming repair deducts 1 initial paste unit at t = 0");
        let rate_5 = rep_5.dict_get("repairRateHpPerSec").and_then(|v| v.as_f64()).unwrap();
        let rate_10 = rep_10.dict_get("repairRateHpPerSec").and_then(|v| v.as_f64()).unwrap();
        let rate_20 = rep_20.dict_get("repairRateHpPerSec").and_then(|v| v.as_f64()).unwrap();
        assert_eq!((c5, c10, c20), (10, 10, 10), "SDE paste cost = ceil(20.0 * 0.5) = 10 across module volumes");
        assert!((rate_5 - 10.0).abs() < 1e-3 && (rate_10 - 10.0).abs() < 1e-3 && (rate_20 - 10.0).abs() < 1e-3, "SDE base repair rate = 10.0 HP/s ({rate_5}, {rate_10}, {rate_20})");

        // Shield booster (type 10838, carries SDE attr 1231 overloadShieldBonus) also uses 0.5 paste/HP (20 HP -> 10 paste)
        let mod_sb = 971_304;
        give_item(&ctx, mod_sb, 10838, char_id, ship_id, InventoryFlag::MedSlot0, 1);
        let sb_type = ctx.sde.types.get(&10838).unwrap();
        assert!(sb_type.default_attributes.contains_key(&1231), "Shield booster 10838 must carry attr 1231");
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.get_mut(&mod_sb).unwrap().dynamic_attributes.insert(3, 20.0);
        });
        let rep_sb = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "InitiateModuleRepair", &[PyRep::Integer(mod_sb)], &kw, None).await.expect("Repair shield booster");
        let c_sb = rep_sb.dict_get("totalPasteRequired").and_then(|v| v.as_i64()).unwrap();
        assert_eq!(c_sb, 10, "Shield booster paste cost must use SDE 0.5 paste/HP (10 paste for 20 HP)");
        let _ = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "StopModuleRepair", &[PyRep::Integer(mod_sb)], &kw, None).await;

        // Negative: duplicate InitiateModuleRepair on an already-repairing module rejected & preserves paste;
        // activating, overloading, or unfitting a module while undergoing nanite repair is also rejected!
        let paste_after_start = ctx.db.with_snapshot(|s| s.items.get(&paste_id).unwrap().quantity);
        assert!(ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "InitiateModuleRepair", &[PyRep::Integer(mod_5m3)], &kw, None).await.is_err(), "Duplicate InitiateModuleRepair on repairing module must fail");
        assert_eq!(ctx.db.with_snapshot(|s| s.items.get(&paste_id).unwrap().quantity), paste_after_start, "Rejected duplicate repair must not consume extra paste");
        assert!(ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(mod_5m3)], &kw, None).await.is_err(), "Cannot Activate module while undergoing nanite repair");
        assert!(ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Overload", &[PyRep::Integer(mod_5m3)], &kw, None).await.is_err(), "Cannot Overload module while undergoing nanite repair");
        assert!(ctx.dispatch_rpc(&mut sess, "invbroker", None, "Add", &[PyRep::Integer(mod_5m3), PyRep::Integer(ship_id), PyRep::Integer(5)], &kw, None).await.is_err(), "Cannot unfit module while undergoing nanite repair");

        // 4. Step 1s mid-cycle and call StopModuleRepair on mod_20m3: partial repair committed & paste streamed incrementally!
        let paste_before_step = ctx.db.with_snapshot(|s| s.items.get(&paste_id).unwrap().quantity);
        let _ = sys.step_manual(1.0).await;
        let paste_after_step = ctx.db.with_snapshot(|s| s.items.get(&paste_id).unwrap().quantity);
        assert!(paste_after_step < paste_before_step && paste_after_step >= 500 - 15, "Paste must stream incrementally over ticks: before={paste_before_step}, after={paste_after_step}");
        let stop_res = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "StopModuleRepair", &[PyRep::Integer(mod_20m3)], &kw, None).await.expect("StopModuleRepair");
        let rem_dmg = stop_res.dict_get("remainingDamage").and_then(|v| v.as_f64()).unwrap();
        let refunded = stop_res.dict_get("pasteRefunded").and_then(|v| v.as_i64()).unwrap();
        let paste_after_stop = ctx.db.with_snapshot(|s| s.items.get(&paste_id).unwrap().quantity);
        let db_dmg_mid = ctx.db.with_snapshot(|s| s.items.get(&mod_20m3).unwrap().dynamic_attributes.get(&3).copied().unwrap_or(0.0));
        assert!((rem_dmg - 10.0).abs() < 1e-2, "1s at 10.0 HP/s must leave ~10.0 HP of 20.0 HP (remaining {rem_dmg})");
        assert!((db_dmg_mid - rem_dmg).abs() < 1e-3, "Partial repair must be persisted to item DAMAGE attribute");
        assert_eq!(paste_after_stop, paste_after_step + refunded as i32);

        // Step remaining 5s -> mod_5m3 and mod_10m3 complete repair all the way to 0.0 DAMAGE!
        let _ = sys.step_manual(5.0).await;
        let dmg_5_done = ctx.db.with_snapshot(|s| s.items.get(&mod_5m3).unwrap().dynamic_attributes.get(&3).copied().unwrap_or(0.0));
        let dmg_10_done = ctx.db.with_snapshot(|s| s.items.get(&mod_10m3).unwrap().dynamic_attributes.get(&3).copied().unwrap_or(0.0));
        assert!(dmg_5_done <= 1e-6 && dmg_10_done <= 1e-6, "Stepped modules must finish repairing to 0.0 DAMAGE");

        // 5. Thermodynamics V (28164) does NOT reduce paste cost; Nanite Operation V (28879) (-5%/lvl -> -25%), Nanite Interfacing V (28880) (+20%/lvl -> +100% rate = 20.0 HP/s)
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.get_mut(&mod_20m3).unwrap().dynamic_attributes.insert(3, 20.0);
        });
        set_skill(&ctx, char_id, 28164, 5); // Thermodynamics V -> no paste reduction in SDE
        set_skill(&ctx, char_id, 28879, 0);
        let rep_thermo = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "InitiateModuleRepair", &[PyRep::Integer(mod_20m3)], &kw, None).await.expect("Repair with Thermo V");
        let c_thermo = rep_thermo.dict_get("totalPasteRequired").and_then(|v| v.as_i64()).unwrap();
        assert_eq!(c_thermo, 10, "Thermodynamics V must NOT reduce Nanite Repair Paste cost (remains 10)");
        let _ = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "StopModuleRepair", &[PyRep::Integer(mod_20m3)], &kw, None).await;

        ctx.db.with_mut_snapshot(|snap| {
            snap.items.get_mut(&mod_20m3).unwrap().dynamic_attributes.insert(3, 20.0);
        });
        set_skill(&ctx, char_id, 28879, 5); // Nanite Operation V -> -25% (10 * 0.75 = 7.5 -> 8)
        set_skill(&ctx, char_id, 28880, 5); // Nanite Interfacing V -> +100% rate (20.0 HP/s)
        let rep_all_v = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "InitiateModuleRepair", &[PyRep::Integer(mod_20m3)], &kw, None).await.expect("Repair with all V");
        let c_all_v = rep_all_v.dict_get("totalPasteRequired").and_then(|v| v.as_i64()).unwrap();
        let rate_v = rep_all_v.dict_get("repairRateHpPerSec").and_then(|v| v.as_f64()).unwrap();
        assert_eq!(c_all_v, 8, "Nanite Operation V (-25%) must reduce 10 paste to 8 paste");
        assert!((rate_v - 20.0).abs() < 1e-2, "Nanite Interfacing V (+100%) must double repair rate to 20.0 HP/s (got {rate_v})");
        let _ = sys.step_manual(5.0).await;

        // 6. Burned-out module (40.0 / 40.0 HP) IS repaired when docked at station OR tethered/at Upwell structure!
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.get_mut(&mod_5m3).unwrap().dynamic_attributes.insert(3, 40.0);
            if let Some(ch) = snap.characters.get_mut(&char_id) {
                ch.balance = IskCents(1_000_000_000);
                ch.station_id = Some(60003760);
            }
        });
        sess.station_id = Some(60003760);
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.get_mut(&mod_10m3).unwrap().dynamic_attributes.insert(3, 15.0);
        });
        assert!(ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "InitiateModuleRepair", &[PyRep::Integer(mod_10m3)], &kw, None).await.is_err(), "In-space InitiateModuleRepair must be rejected while docked");
        ctx.dispatch_rpc(&mut sess, "repairSvc", None, "RepairItemsInStation", &[PyRep::List(vec![PyRep::Integer(mod_5m3)]), PyRep::Real(5_000_000.0)], &kw, None).await.expect("Repair burned-out module in station");
        let dmg_after_station = ctx.db.with_snapshot(|s| s.items.get(&mod_5m3).unwrap().dynamic_attributes.get(&3).copied().unwrap_or(0.0));
        assert!(dmg_after_station <= 1e-6, "Station repair service must clear burned-out module damage");

        // Burn out again in space and repair via real in-space Upwell structure tethering (leaving burned-out module offline until manually onlined)
        let astra_id = 981_301;
        let corp_id = ctx.db.with_snapshot(|s| s.characters.get(&char_id).map(|c| c.corporation_id).unwrap_or(1000009));
        give_item(&ctx, 981_302, 35892, corp_id, astra_id, InventoryFlag::ServiceSlot0, 1);
        ctx.db.with_mut_snapshot(|snap| {
            snap.module_online_states.insert(981_302, true);
            snap.module_online_states.insert(mod_5m3, false);
            snap.items.get_mut(&mod_5m3).unwrap().dynamic_attributes.insert(3, 40.0);
            if let Some(ch) = snap.characters.get_mut(&char_id) {
                ch.station_id = None;
                ch.structure_id = None;
                ch.solar_system_id = NULL_SYS;
            }
            snap.structures.insert(
                astra_id,
                UpwellStructureRecord {
                    structure_id: astra_id,
                    type_id: 35832,
                    owner_corp_id: corp_id,
                    solar_system_id: NULL_SYS,
                    name: "Repair Astrahus".into(),
                    position: [0.0, 0.0, 0.0],
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
        sess.station_id = None;
        sess.structure_id = None;
        sess.solar_system_id = Some(NULL_SYS);
        let _ = sys.set_ball_position(ship_id, DVec3::new(1_000.0, 0.0, 0.0)).await;
        assert_eq!(
            ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "InitiateModuleRepair", &[PyRep::Integer(mod_5m3)], &kw, None).await.unwrap_err().code,
            "ModuleBurnedOut",
            "100% burned-out module must be rejected by InitiateModuleRepair in space"
        );
        eve_server::services::space_destiny::space_nav_travel::sync_tether(&ctx, char_id, NULL_SYS, ship_id).await;
        let _ = sys.step_manual(1.0).await;
        assert!(sys.get_combat_state(ship_id).await.unwrap().unwrap().is_tethered, "Ship must be tethered in space to the operational Astrahus");
        let dmg_after_1s = ctx.db.with_snapshot(|s| s.items.get(&mod_5m3).unwrap().dynamic_attributes.get(&3).copied().unwrap_or(0.0));
        assert!(dmg_after_1s > 0.0 && dmg_after_1s < 40.0, "Tether heat repair must be gradual, not instant (after 1s: {dmg_after_1s})");
        let _ = sys.step_manual(25.0).await;
        let (dmg_after_tether, is_online_after_tether) = ctx.db.with_snapshot(|s| {
            let dmg = s.items.get(&mod_5m3).unwrap().dynamic_attributes.get(&3).copied().unwrap_or(0.0);
            let onl = s.module_online_states.get(&mod_5m3).copied().unwrap_or(true);
            (dmg, onl)
        });
        assert!(dmg_after_tether <= 1e-6, "In-space Upwell structure tethering must gradually clear burned-out module heat damage");
        assert!(!is_online_after_tether, "Tether-repaired burned-out module must remain offline until manually onlined by pilot");
        // BF-B35-03: InitiateModuleRepairMany must also reject docked sessions
        ctx.db.with_mut_snapshot(|s| s.characters.get_mut(&char_id).unwrap().station_id = Some(60_003_760));
        sess.station_id = Some(60_003_760);
        assert_eq!(
            ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "InitiateModuleRepairMany", &[PyRep::List(vec![PyRep::Integer(mod_5m3)])], &kw, None).await.unwrap_err().code,
            "CannotRepairNaniteWhileDocked"
        );

        // Regression B35R-P1-01: try_remove_ball while holding DB write lock must not deadlock when ship has active nanite repair
        ctx.db.with_mut_snapshot(|s| {
            s.characters.get_mut(&char_id).unwrap().station_id = None;
            s.items.get_mut(&mod_5m3).unwrap().dynamic_attributes.insert(3, 10.0);
        });
        sess.station_id = None;
        ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "InitiateModuleRepair", &[PyRep::Integer(mod_5m3)], &kw, None)
            .await
            .expect("Start nanite repair before try_remove_ball");
        ctx.db.with_mut_snapshot(|_| {
            sys.try_remove_ball(ship_id);
        });
        assert!(sys.get_ball_snapshot(ship_id).await.unwrap().is_none(), "Ship ball must be removed without deadlocking");
    }

    #[tokio::test]
    async fn test_b35_05_industrial_cyno_rules() {
        // parity: feat:modules.industrial-cyno-rules
        eve_testkit::attest(&["feat:modules.industrial-cyno-rules"]);
        let ctx = build_ctx();
        let kw = HashMap::new();

        // Negative/abuse cases (>= 2): Battleship hull rejected & High-sec (Jita 30000142) rejected
        let mega_char = 952_001;
        let mega_ship = 962_001;
        let mut mega_sess = seed_pilot(&ctx, mega_char, mega_ship, 641, DEST_NULL_SYS, false);
        spawn_ship_in_space(&ctx, mega_char, mega_ship, 641, DEST_NULL_SYS, DVec3::ZERO).await;
        set_skill(&ctx, mega_char, 3336, 5);
        set_skill(&ctx, mega_char, 21603, 5);
        give_item(&ctx, 972_001, 52694, mega_char, mega_ship, InventoryFlag::HiSlot0, 1);
        give_item(&ctx, 972_002, 16273, mega_char, mega_ship, InventoryFlag::Cargo, 1_000);
        assert!(
            ctx.dispatch_rpc(&mut mega_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(972_001), PyRep::String("cynosuralGeneration".into()), PyRep::Integer(0), PyRep::Integer(1)], &kw, None).await.is_err(),
            "Battleship must not be allowed to light Industrial Cyno"
        );

        let hs_char = 952_010;
        let hs_ship = 962_010;
        let mut hs_sess = seed_pilot(&ctx, hs_char, hs_ship, 32880, 30000142, false);
        spawn_ship_in_space(&ctx, hs_char, hs_ship, 32880, 30000142, DVec3::ZERO).await;
        set_skill(&ctx, hs_char, 32918, 5);
        set_skill(&ctx, hs_char, 21603, 5);
        give_item(&ctx, 972_010, 52694, hs_char, hs_ship, InventoryFlag::HiSlot0, 1);
        give_item(&ctx, 972_011, 16273, hs_char, hs_ship, InventoryFlag::Cargo, 1_000);
        assert!(
            ctx.dispatch_rpc(&mut hs_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(972_010), PyRep::String("cynosuralGeneration".into()), PyRep::Integer(0), PyRep::Integer(1)], &kw, None).await.is_err(),
            "Industrial Cyno must be rejected in high-security space"
        );

        // Legal Hull Matrix:
        // (a) T1 Hauler (Badger 648): CFT I -> 720 LO; check base 800 * (1 - 0.1*lvl) -> CFT V = 400 LO
        for (idx, hull_tid, cft_lvl, expected_lo, extra_skills) in [
            (0usize, 648u32, 1u8, 720i64, &[(3341u32, 5u8)][..]),
            (1, 648, 5, 400, &[(3341, 5)][..]),
            // (b) Blockade Runner (Crane 12729): CFT V -> 400 LO
            (2, 12729, 5, 400, &[(3341, 5), (19719, 5)][..]),
            // (c) Deep Space Transport (Bustard 12731): CFT III -> 560 LO
            (3, 12731, 3, 560, &[(3341, 5), (19719, 5)][..]),
            // (d) Venture (32880): -50% role bonus -> 400 LO at CFT I AND 400 LO minimum floor at CFT V!
            (4, 32880, 1, 400, &[(32918, 5)][..]),
        ] {
            let cid = 952_020 + idx as i64;
            let sid = 962_020 + idx as i64;
            let mid = 972_020 + idx as i64 * 2;
            let mut s = seed_pilot(&ctx, cid, sid, hull_tid, DEST_NULL_SYS, false);
            spawn_ship_in_space(&ctx, cid, sid, hull_tid, DEST_NULL_SYS, DVec3::new((idx as f64 + 1.0) * 5_000.0, 0.0, 0.0)).await;
            set_skill(&ctx, cid, 21603, cft_lvl);
            for &(sk, lv) in extra_skills {
                set_skill(&ctx, cid, sk, lv);
            }
            give_item(&ctx, mid, 52694, cid, sid, InventoryFlag::HiSlot0, 1);
            give_item(&ctx, mid + 1, 16273, cid, sid, InventoryFlag::Cargo, 2_000);
            ctx.invalidate_ship_fitting(sid);
            put_in_fleet(&ctx, 880_002, cid, &[(cid, true)]);
            let res = ctx.dispatch_rpc(&mut s, "dogmaIM", None, "Activate", &[PyRep::Integer(mid), PyRep::String("cynosuralGeneration".into()), PyRep::Integer(0), PyRep::Integer(1)], &kw, None).await.expect("Activate Industrial Cyno");
            assert_eq!(res.dict_get("consumptionQuantity"), Some(&PyRep::Integer(expected_lo)), "Hull {hull_tid} CFT {cft_lvl} LO mismatch");
        }

        // Venture (32880) with CFT V: verify 400 LO minimum floor, overview ball presence, AND immobilization
        let ven_char = 952_101;
        let ven_ship = 962_101;
        let mut ven_sess = seed_pilot(&ctx, ven_char, ven_ship, 32880, DEST_NULL_SYS, false);
        spawn_ship_in_space(&ctx, ven_char, ven_ship, 32880, DEST_NULL_SYS, DVec3::new(50_000.0, 0.0, 0.0)).await;
        set_skill(&ctx, ven_char, 32918, 5);
        set_skill(&ctx, ven_char, 21603, 5); // CFT V on Venture: 800 * 0.5 * 0.5 = 200 -> clamped to 400 floor!
        give_item(&ctx, 972_101, 52694, ven_char, ven_ship, InventoryFlag::HiSlot0, 1);
        give_item(&ctx, 972_102, 16273, ven_char, ven_ship, InventoryFlag::Cargo, 1_000);
        let ven_beyonce = ctx.bind_object(&mut ven_sess, "beyonce", DEST_NULL_SYS as i64).unwrap();
        put_in_fleet(&ctx, 880_001, ven_char, &[(ven_char, true)]);
        let cyno_res = ctx.dispatch_rpc(&mut ven_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(972_101), PyRep::String("cynosuralGeneration".into()), PyRep::Integer(0), PyRep::Integer(1)], &kw, None).await.expect("Venture CFT V industrial cyno");
        assert_eq!(cyno_res.dict_get("consumptionQuantity"), Some(&PyRep::Integer(400)), "Venture + CFT V must respect 400 LO minimum floor");
        let beacon_id = cyno_res.dict_get("beaconID").and_then(|v| v.as_i64()).unwrap();

        let dest_sys = ctx.systems.get_or_spawn(DEST_NULL_SYS);
        assert!(dest_sys.get_ball_snapshot(beacon_id).await.unwrap().is_some(), "Industrial cyno beacon must appear in space/overview");
        let (_, ven_ball) = dest_sys.get_ball_snapshot(ven_ship).await.unwrap().unwrap();
        assert!(ven_ball.max_velocity <= 1e-9, "Active Industrial Cyno must immobilize the cyno ship (max_velocity = 0)");
        let dest_gate = ctx.sde.stargates().values().find(|j| j.from_solar_system_id == DEST_NULL_SYS).map(|j| j.stargate_id).unwrap();
        assert!(ctx.dispatch_rpc(&mut ven_sess, "beyonce", Some(ven_beyonce), "CmdGotoDirection", &[PyRep::Real(1.0), PyRep::Real(0.0), PyRep::Real(0.0)], &kw, None).await.is_err());
        assert!(ctx.dispatch_rpc(&mut ven_sess, "beyonce", Some(ven_beyonce), "CmdWarpToStuff", &[PyRep::String("item".into()), PyRep::Integer(dest_gate), PyRep::Real(0.0)], &kw, None).await.is_err());

        // Jump & Bridge eligibility matrix against Industrial Cyno:
        let car_char = 952_104;
        let car_ship = 962_104;
        let mut car_sess = seed_pilot(&ctx, car_char, car_ship, 23757, NULL_SYS, false); // Archon
        spawn_ship_in_space(&ctx, car_char, car_ship, 23757, NULL_SYS, DVec3::new(1_000.0, 0.0, 0.0)).await;
        give_item(&ctx, 972_104, 16274, car_char, car_ship, InventoryFlag::SpecializedFuelBay, 50_000);

        let jf_char = 952_102;
        let jf_ship = 962_102;
        let mut jf_sess = seed_pilot(&ctx, jf_char, jf_ship, 28844, NULL_SYS, false); // Rhea
        spawn_ship_in_space(&ctx, jf_char, jf_ship, 28844, NULL_SYS, DVec3::new(2_000.0, 0.0, 0.0)).await;
        give_item(&ctx, 972_103, 17888, jf_char, jf_ship, InventoryFlag::SpecializedFuelBay, 50_000);

        let blops_char = 952_105;
        let blops_ship = 962_105;
        let mut blops_sess = seed_pilot(&ctx, blops_char, blops_ship, 22428, NULL_SYS, false); // Redeemer
        spawn_ship_in_space(&ctx, blops_char, blops_ship, 22428, NULL_SYS, DVec3::new(3_000.0, 0.0, 0.0)).await;
        give_item(&ctx, 972_105, 16274, blops_char, blops_ship, InventoryFlag::SpecializedFuelBay, 50_000);
        give_item(&ctx, 972_106, 28652, blops_char, blops_ship, InventoryFlag::HiSlot0, 1);

        let rorq_char = 952_106;
        let rorq_ship = 962_106;
        let mut rorq_sess = seed_pilot(&ctx, rorq_char, rorq_ship, 28352, NULL_SYS, false); // Rorqual
        spawn_ship_in_space(&ctx, rorq_char, rorq_ship, 28352, NULL_SYS, DVec3::new(4_000.0, 0.0, 0.0)).await;
        give_item(&ctx, 972_107, 17887, rorq_char, rorq_ship, InventoryFlag::SpecializedFuelBay, 50_000);
        give_item(&ctx, 972_108, 63140, rorq_char, rorq_ship, InventoryFlag::HiSlot0, 1);

        put_in_fleet(&ctx, 880_001, ven_char, &[(ven_char, true), (car_char, true), (jf_char, true), (blops_char, true), (rorq_char, true)]);

        assert!(ctx.dispatch_rpc(&mut blops_sess, "beyonce", None, "CmdBridgeToFleetModuleBeacon", &[PyRep::Integer(ven_char), PyRep::Integer(DEST_NULL_SYS as i64), PyRep::Integer(beacon_id)], &kw, None).await.is_ok());
        assert!(ctx.dispatch_rpc(&mut rorq_sess, "beyonce", None, "CmdBridgeToFleetModuleBeacon", &[PyRep::Integer(ven_char), PyRep::Integer(DEST_NULL_SYS as i64), PyRep::Integer(beacon_id)], &kw, None).await.is_ok());
        assert!(ctx.dispatch_rpc(&mut blops_sess, "beyonce", None, "CmdJumpToFleetModuleBeacon", &[PyRep::Integer(ven_char), PyRep::Integer(DEST_NULL_SYS as i64), PyRep::Integer(beacon_id)], &kw, None).await.is_err(), "Cannot jump while own portal is open");
        ctx.advance_time_seconds(65);
        assert!(ctx.dispatch_rpc(&mut car_sess, "beyonce", None, "CmdJumpToFleetModuleBeacon", &[PyRep::Integer(ven_char), PyRep::Integer(DEST_NULL_SYS as i64), PyRep::Integer(beacon_id)], &kw, None).await.is_err());
        assert!(ctx.dispatch_rpc(&mut jf_sess, "beyonce", None, "CmdJumpToFleetModuleBeacon", &[PyRep::Integer(ven_char), PyRep::Integer(DEST_NULL_SYS as i64), PyRep::Integer(beacon_id)], &kw, None).await.is_ok());
        assert!(ctx.dispatch_rpc(&mut blops_sess, "beyonce", None, "CmdJumpToFleetModuleBeacon", &[PyRep::Integer(ven_char), PyRep::Integer(DEST_NULL_SYS as i64), PyRep::Integer(beacon_id)], &kw, None).await.is_ok());
        assert!(ctx.dispatch_rpc(&mut rorq_sess, "beyonce", None, "CmdJumpToFleetModuleBeacon", &[PyRep::Integer(ven_char), PyRep::Integer(DEST_NULL_SYS as i64), PyRep::Integer(beacon_id)], &kw, None).await.is_ok());
    }

    // parity: sde:res:/localizationfsd/localization_fsd_main.pickle, sde:res:/localizationfsd/localization_fsd_en-us.pickle, sde:res:/localizationfsd/localization_fsd_de.pickle, sde:res:/localizationfsd/localization_fsd_es.pickle, sde:res:/localizationfsd/localization_fsd_fr.pickle, sde:res:/localizationfsd/localization_fsd_ja.pickle, sde:res:/localizationfsd/localization_fsd_ko.pickle, sde:res:/localizationfsd/localization_fsd_ru.pickle, sde:res:/localizationfsd/localization_fsd_zh.pickle
    #[test]
    fn test_b35_esi_localization_fsd_accept_language() {
        eve_testkit::attest(&[
            "sde:res:/localizationfsd/localization_fsd_main.pickle",
            "sde:res:/localizationfsd/localization_fsd_en-us.pickle",
            "sde:res:/localizationfsd/localization_fsd_de.pickle",
            "sde:res:/localizationfsd/localization_fsd_es.pickle",
            "sde:res:/localizationfsd/localization_fsd_fr.pickle",
            "sde:res:/localizationfsd/localization_fsd_ja.pickle",
            "sde:res:/localizationfsd/localization_fsd_ko.pickle",
            "sde:res:/localizationfsd/localization_fsd_ru.pickle",
            "sde:res:/localizationfsd/localization_fsd_zh.pickle",
        ]);
        let ctx = std::sync::Arc::new(build_ctx());
        let main_bundle = ctx.sde.load_localization_main().expect("load_localization_main");
        assert_eq!(main_bundle.labels.len(), 31_872);
        assert_eq!(main_bundle.languages.len(), 9);
        let router = eve_server::esi::EsiRouter::new(std::sync::Arc::clone(&ctx));

        let req_en = eve_server::esi::EsiHttpRequest::new("GET", "/v1/universe/types/587/")
            .with_header("Accept-Language", "en-us");
        let resp_en = router.handle_request(&req_en);
        assert_eq!(resp_en.status_code, 200);
        assert_eq!(resp_en.headers.get("Content-Language").map(String::as_str), Some("en"));
        let en_json: serde_json::Value = serde_json::from_slice(&resp_en.body).unwrap();
        assert_eq!(en_json["name"].as_str(), Some("Rifter"));

        for lang in ["de", "es", "fr", "ja", "ko", "ru", "zh"] {
            let req_type = eve_server::esi::EsiHttpRequest::new(
                "GET",
                &format!("/v1/universe/types/587/?language={lang}"),
            );
            let resp_type = router.handle_request(&req_type);
            assert_eq!(resp_type.status_code, 200);
            assert_eq!(resp_type.headers.get("Content-Language").map(String::as_str), Some(lang));
            let type_json: serde_json::Value = serde_json::from_slice(&resp_type.body).unwrap();
            let expected_type = ctx.sde.localized_type_name(lang, 587).expect("localized type 587");
            let expected_desc = ctx.sde.localized_type_description(lang, 587).expect("localized desc 587");
            assert_eq!(type_json["name"].as_str(), Some(expected_type.as_str()));
            assert_eq!(type_json["description"].as_str(), Some(expected_desc.as_str()));

            let req_grp = eve_server::esi::EsiHttpRequest::new("GET", "/v1/universe/groups/25/")
                .with_header("Accept-Language", lang);
            let resp_grp = router.handle_request(&req_grp);
            assert_eq!(resp_grp.status_code, 200);
            let grp_json: serde_json::Value = serde_json::from_slice(&resp_grp.body).unwrap();
            let expected_grp = ctx.sde.localized_group_name(lang, 25).expect("localized group 25");
            assert_eq!(grp_json["name"].as_str(), Some(expected_grp.as_str()));

            let req_cat = eve_server::esi::EsiHttpRequest::new("GET", "/v1/universe/categories/6/")
                .with_header("Accept-Language", lang);
            let resp_cat = router.handle_request(&req_cat);
            assert_eq!(resp_cat.status_code, 200);
            assert_eq!(resp_cat.headers.get("Content-Language").map(String::as_str), Some(lang));
            let cat_json: serde_json::Value = serde_json::from_slice(&resp_cat.body).unwrap();
            let expected_cat = ctx.sde.localized_category_name(lang, 6).expect("localized category 6");
            assert_eq!(cat_json["name"].as_str(), Some(expected_cat.as_str()));

            let req_mg = eve_server::esi::EsiHttpRequest::new("GET", "/v1/markets/groups/4/")
                .with_header("Accept-Language", lang);
            let resp_mg = router.handle_request(&req_mg);
            assert_eq!(resp_mg.status_code, 200);
            let mg_json: serde_json::Value = serde_json::from_slice(&resp_mg.body).unwrap();
            let expected_mg = ctx.sde.localized_market_group_name(lang, 4).expect("localized market group 4");
            assert_eq!(mg_json["name"].as_str(), Some(expected_mg.as_str()));
        }
    }
}

pub(crate) mod waypoints_warp_vector_align_tick {
    // End-to-End Integration Tests closing Navigation parity rows (Part 1):
    // `feat:navigation.route-waypoints`, `feat:navigation.warp-vector`,
    // `feat:navigation.align-server-tick-rounding`, `feat:navigation.collision-bumping`,
    // and `feat:navigation.cloak-velocity-penalty`.

    use crate::modules_advanced_tactical::*;
    use eve_db::{InventoryFlag, UpwellStructureRecord};
    use eve_destiny::{Ball, DestinyEvent, DVec3, MovementMode, WarpPhase};
    use eve_machonet::SessionState;
    use eve_proto::PyRep;
    use eve_server::esi::{EsiHttpRequest, EsiRouter};
    use std::collections::HashMap;
    use std::sync::Arc;

    #[tokio::test]
    async fn test_waypoints_warp_vector_and_align_tick_rounding() {
        // parity: feat:navigation.route-waypoints, feat:navigation.warp-vector, feat:navigation.align-server-tick-rounding
        eve_testkit::attest(&[
            "feat:navigation.route-waypoints",
            "feat:navigation.warp-vector",
            "feat:navigation.align-server-tick-rounding",
        ]);

        let ctx = Arc::new(build_ctx());
        let kw = HashMap::new();
        let jita_sys: u32 = 30_000_142;
        let char_id: i64 = 94_001;
        let ship_id: i64 = 84_001;
        let mut sess = seed_pilot(&ctx, char_id, ship_id, 587, jita_sys, true);

        // 1. Route waypoints via real V24 surfaces (`charMgr` + ESI `/v2/ui/autopilot/waypoint/` + `beyonce`)
        let init_settings = rpc(&ctx, &mut sess, "charMgr", "GetCharacterSettings", vec![]).await.expect("GetCharacterSettings initial");
        assert_eq!(init_settings, PyRep::Dict(Vec::new()));

        // Invalid destination ID rejected on charMgr.SaveCharacterSetting("waypoints", ...)
        let bad_wp = rpc(&ctx, &mut sess, "charMgr", "SaveCharacterSetting", vec![
            PyRep::String("waypoints".into()),
            PyRep::List(vec![PyRep::Integer(999_999_999)]),
        ]).await.unwrap_err();
        assert_eq!(bad_wp.code, "InvalidDestination");

        // Pick a Jita stargate jump destination so we can test live autopilot warp + jump arrival advancement
        let jita_jump = ctx
            .sde
            .jumps
            .values()
            .find(|j| j.from_solar_system_id == jita_sys)
            .expect("Jita stargate jump")
            .clone();
        let dest_sys_id = jita_jump.to_solar_system_id;

        // SaveCharacterSetting("waypoints", ...) persists to DB & survives fresh session reconnect
        rpc(&ctx, &mut sess, "charMgr", "SaveCharacterSetting", vec![
            PyRep::String("waypoints".into()),
            PyRep::List(vec![PyRep::Integer(dest_sys_id as i64), PyRep::Integer(JITA_STATION)]),
        ]).await.expect("SaveCharacterSetting waypoints");

        let mut reconnect_sess = SessionState::new(994_001);
        reconnect_sess.user_id = sess.user_id;
        reconnect_sess.character_id = Some(char_id);
        let reconnect_settings = rpc(&ctx, &mut reconnect_sess, "charMgr", "GetCharacterSettings", vec![])
            .await.expect("GetCharacterSettings on reconnected session");
        assert_eq!(
            reconnect_settings.dict_get("waypoints"),
            Some(&PyRep::List(vec![PyRep::Integer(dest_sys_id as i64), PyRep::Integer(JITA_STATION)]))
        );

        // Undock in Jita and verify beyonce.CmdWarpToStuffAutopilot uses 15 km default landing perimeter
        sess.station_id = None;
        let gate_radius = ctx.sde.types.get(&16).map(|t| t.radius).unwrap_or(14_051.0);
        spawn_in_space(
            &ctx,
            char_id,
            ship_id,
            587,
            jita_sys,
            jita_jump.position - DVec3::new(10_000_000.0, 0.0, 0.0),
        )
        .await;
        let b_jita = ctx
            .bind_object(&mut sess, "beyonce", jita_sys as i64)
            .expect("bind beyonce Jita");
        ctx.dispatch_rpc(
            &mut sess,
            "beyonce",
            Some(b_jita),
            "CmdWarpToStuffAutopilot",
            &[PyRep::Integer(jita_jump.stargate_id)],
            &kw,
            None,
        )
        .await
        .expect("CmdWarpToStuffAutopilot to stargate");
        let (_, ap_ball) = ctx
            .systems
            .get_or_spawn(jita_sys)
            .get_ball_snapshot(ship_id)
            .await
            .unwrap()
            .unwrap();
        let MovementMode::Warp { destination: ap_dest, .. } = ap_ball.mode else {
            panic!("Expected Warp mode from CmdWarpToStuffAutopilot");
        };
        assert!(
            (ap_dest.distance(jita_jump.position) - (gate_radius + 15_000.0)).abs() < 10.0,
            "Autopilot warp must land at gate_radius + 15,000m"
        );

        // Stargate jump refused at ~3 km outside gate perimeter (> 2,500 m jump range)
        let jita_gate_ball_r = ctx
            .systems
            .get_or_spawn(jita_sys)
            .get_ball_snapshot(jita_jump.stargate_id)
            .await
            .ok()
            .flatten()
            .map(|(_, b)| b.radius)
            .unwrap_or(gate_radius);
        spawn_in_space(&ctx, char_id, ship_id, 587, jita_sys, jita_jump.position + DVec3::new(jita_gate_ball_r + 3_000.0, 0.0, 0.0)).await;
        let far_jump = ctx
            .dispatch_rpc(&mut sess, "beyonce", Some(b_jita), "CmdStargateJump", &[PyRep::Integer(jita_jump.stargate_id)], &kw, None)
            .await;
        assert_eq!(far_jump.unwrap_err().code, "OutsideStargateJumpRange");

        // Stargate jump arrival pops matching front waypoint (`dest_sys_id`)
        spawn_in_space(&ctx, char_id, ship_id, 587, jita_sys, jita_jump.position).await;
        ctx.dispatch_rpc(
            &mut sess,
            "beyonce",
            Some(b_jita),
            "CmdStargateJump",
            &[PyRep::Integer(jita_jump.stargate_id)],
            &kw,
            None,
        )
        .await
        .expect("CmdStargateJump advances waypoint");
        let post_jump_settings = rpc(&ctx, &mut sess, "charMgr", "GetCharacterSettings", vec![])
            .await
            .expect("GetCharacterSettings post jump");
        assert_eq!(
            post_jump_settings.dict_get("waypoints"),
            Some(&PyRep::List(vec![PyRep::Integer(JITA_STATION)]))
        );

        // Return to Jita and dock at Jita 4-4 Station -> pops final waypoint & removes "waypoints" setting
        ctx.session_change_timers.lock().unwrap().remove(&char_id);
        sess.solar_system_id = Some(jita_sys);
        let station_pos = ctx.sde.stations[&JITA_STATION].position;
        spawn_in_space(&ctx, char_id, ship_id, 587, jita_sys, station_pos).await;
        ctx.systems
            .get_or_spawn(jita_sys)
            .set_ball_position(ship_id, station_pos)
            .await;
        let b_jita_dock = ctx
            .bind_object(&mut sess, "beyonce", jita_sys as i64)
            .expect("rebind beyonce Jita");
        ctx.dispatch_rpc(
            &mut sess,
            "beyonce",
            Some(b_jita_dock),
            "CmdDock",
            &[PyRep::Integer(JITA_STATION)],
            &kw,
            None,
        )
        .await
        .expect("CmdDock advances final waypoint");
        let post_dock_settings = rpc(&ctx, &mut sess, "charMgr", "GetCharacterSettings", vec![])
            .await
            .expect("GetCharacterSettings post dock");
        assert_eq!(post_dock_settings.dict_get("waypoints"), None);

        // ESI POST /v2/ui/autopilot/waypoint/ (clear_other_waypoints, append, add_to_beginning, invalid 400)
        let uid = ctx
            .db
            .provision_player_account("nav_esi_pilot", "nav-esi-secret");
        ctx.db.with_mut_snapshot(|snap| {
            snap.characters.get_mut(&char_id).unwrap().user_id = uid;
        });
        let _ = ctx.db.acquire_online_lock_with_computer_hash(
            uid,
            sess.session_id,
            Some(char_id),
            Some("nav_esi_hash".into()),
            true,
            ctx.now_unix(),
        );
        let now = ctx.now_unix();
        let code = ctx
            .db
            .authorize_pkce(
                "nav_esi_pilot",
                "nav-esi-secret",
                char_id,
                "evomu-nav-test",
                "https://localhost/callback",
                vec!["esi-ui.write_waypoint.v1".into()],
                "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM",
                "S256",
                now,
            )
            .expect("authorize_pkce");
        let token = ctx
            .db
            .exchange_pkce_code(
                &code,
                "evomu-nav-test",
                "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk",
                now,
            )
            .expect("exchange_pkce_code")
            .access_token;
        let esi = EsiRouter::new(Arc::clone(&ctx));
        let bad_esi = esi.handle_request(
            &EsiHttpRequest::new(
                "POST",
                "/v2/ui/autopilot/waypoint/?destination_id=999999999&add_to_beginning=false&clear_other_waypoints=true",
            )
            .with_bearer_token(&token),
        );
        assert_eq!(bad_esi.status_code, 404);

        for url in [
            "/v2/ui/autopilot/waypoint/?destination_id=30002187&add_to_beginning=false&clear_other_waypoints=true".to_string(),
            format!("/v2/ui/autopilot/waypoint/?destination_id={JITA_STATION}&add_to_beginning=false&clear_other_waypoints=false"),
            format!("/v2/ui/autopilot/waypoint/?destination_id={dest_sys_id}&add_to_beginning=true&clear_other_waypoints=false"),
        ] {
            let resp = esi.handle_request(&EsiHttpRequest::new("POST", &url).with_bearer_token(&token));
            assert_eq!(resp.status_code, 204);
        }
        let esi_settings = rpc(&ctx, &mut sess, "charMgr", "GetCharacterSettings", vec![])
            .await
            .expect("GetCharacterSettings after ESI waypoints");
        assert_eq!(
            esi_settings.dict_get("waypoints"),
            Some(&PyRep::List(vec![
                PyRep::Integer(dest_sys_id as i64),
                PyRep::Integer(30_002_187),
                PyRep::Integer(JITA_STATION),
            ]))
        );
        rpc(
            &ctx,
            &mut sess,
            "charMgr",
            "DeleteCharacterSetting",
            vec![PyRep::String("waypoints".into())],
        )
        .await
        .expect("DeleteCharacterSetting waypoints");
        assert_eq!(
            ctx.db.with_snapshot(|s| s.characters[&char_id].autopilot_waypoints.clone()),
            Vec::<i64>::new()
        );

        // 2. Cargo Warp Vector Data (`feat:navigation.warp-vector`): star-centered coordinates + 300s cooldown
        sess.station_id = None;
        let star_cel = ctx
            .sde
            .celestials_in_system(jita_sys)
            .into_iter()
            .find(|c| c.group_id == 6)
            .expect("Jita star");
        let vec_item_id: i64 = 74_001;
        give_item(&ctx, vec_item_id, 77_825, char_id, ship_id, InventoryFlag::Cargo, 2);

        // Non-inventory space ball IDs (e.g., stargate or container ball) are rejected with InvalidVectorItem (no beacon fallback)
        let far_from_star = star_cel.position + DVec3::new(2_000_000_000_000.0, 0.0, 0.0);
        spawn_in_space(&ctx, char_id, ship_id, 587, jita_sys, far_from_star).await;
        assert_eq!(
            rpc(&ctx, &mut sess, "warpVectorMgr", "UseVector", vec![PyRep::Integer(jita_jump.stargate_id)])
                .await
                .unwrap_err()
                .code,
            "InvalidVectorItem"
        );

        // Out of star proximity (> 1,496,000,000,000 m = 10 AU) -> rejected
        assert_eq!(
            rpc(&ctx, &mut sess, "warpVectorMgr", "UseVector", vec![PyRep::Integer(vec_item_id)])
                .await
                .unwrap_err()
                .code,
            "WarpVectorTooFarFromStar"
        );

        // Place ship at a non-zero 3D offset from star so star_pos + delta != ship_pos + delta!
        let near_star = star_cel.position + DVec3::new(500_000_000.0, 250_000_000.0, -100_000_000.0);
        let sys = ctx.systems.get_or_spawn(jita_sys);
        sys.set_ball_position(ship_id, near_star).await;
        let vec_res = rpc(&ctx, &mut sess, "warpVectorMgr", "UseVector", vec![PyRep::Integer(vec_item_id)])
            .await
            .expect("UseVector with cargo Warp Vector Data");
        assert_eq!(vec_res.dict_get("consumed"), Some(&PyRep::Bool(true)));
        assert_eq!(vec_res.dict_get("remainingQuantity"), Some(&PyRep::Integer(1)));
        let expected_vec_dest = star_cel.position + DVec3::new(220_000_000_000.0, -140_000_000_000.0, 0.0);
        let (_, vec_ball) = sys.get_ball_snapshot(ship_id).await.unwrap().unwrap();
        let MovementMode::Warp { destination: actual_vec_dest, .. } = vec_ball.mode else {
            panic!("Expected MovementMode::Warp after cargo Warp Vector Data activation");
        };
        assert!(
            actual_vec_dest.distance(expected_vec_dest) < 10.0,
            "Warp Vector Data must resolve relative to solar-system star coordinates: got {actual_vec_dest:?} vs {expected_vec_dest:?}"
        );
        assert!(
            actual_vec_dest.distance(near_star + DVec3::new(220_000_000_000.0, -140_000_000_000.0, 0.0)) > 100_000_000.0,
            "Warp Vector destination must NOT be ship-relative"
        );

        // 300s per-character cooldown enforced on immediate 2nd activation, clears after 301s
        sys.set_movement_mode(ship_id, MovementMode::Stop).await.unwrap();
        sys.set_ball_position(ship_id, near_star).await;
        assert_eq!(
            rpc(&ctx, &mut sess, "warpVectorMgr", "UseVector", vec![PyRep::Integer(vec_item_id)])
                .await
                .unwrap_err()
                .code,
            "WarpVectorCooldown"
        );
        ctx.advance_secs(301);
        // Vector destination < 150,000 m from ship is rejected without consuming item
        sys.set_ball_position(ship_id, expected_vec_dest - DVec3::new(100_000.0, 0.0, 0.0)).await;
        assert_eq!(
            rpc(&ctx, &mut sess, "warpVectorMgr", "UseVector", vec![PyRep::Integer(vec_item_id)])
                .await
                .unwrap_err()
                .code,
            "WarpDistanceTooShort"
        );
        sys.set_ball_position(ship_id, near_star).await;
        rpc(&ctx, &mut sess, "warpVectorMgr", "UseVector", vec![PyRep::Integer(vec_item_id)])
            .await
            .expect("UseVector succeeds after 300s cooldown");

        // 3. Server-Tick Sub-Tick Align Rounding (`feat:navigation.align-server-tick-rounding`)
        // Exact align time = ln(4) * inertia * mass / 1e6 = 3.200000 s -> ceil(3.2) = 4 ticks!
        let t_align_3_2_inertia = 3.2 / 4.0_f64.ln();
        let align_origin = DVec3::new(50_000_000_000.0, 0.0, 0.0);
        let align_dest = align_origin + DVec3::new(10_000_000.0, 0.0, 0.0);
        sys.spawn_ball(Ball {
            id: ship_id,
            type_id: 587,
            position: align_origin,
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: t_align_3_2_inertia,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::new_warp(align_origin, align_dest, 3.0),
        })
        .await
        .unwrap();

        for tick_idx in 1..=3 {
            sys.step_manual(1.0).await.unwrap();
            let (_, b) = sys.get_ball_snapshot(ship_id).await.unwrap().unwrap();
            assert!(
                matches!(b.mode, MovementMode::Warp { phase: WarpPhase::Aligning, .. }),
                "Ship with 3.2s align time must remain Aligning on tick {tick_idx}"
            );
        }
        sys.step_manual(1.0).await.unwrap();
        let (_, b_t4) = sys.get_ball_snapshot(ship_id).await.unwrap().unwrap();
        assert!(
            matches!(
                b_t4.mode,
                MovementMode::Warp {
                    phase: WarpPhase::Accelerating | WarpPhase::Cruising | WarpPhase::Decelerating,
                    ..
                }
            ),
            "Ship with 3.2s align time must enter warp on tick 4 (4.0s): got {:?}",
            b_t4.mode
        );
    }

    #[tokio::test]
    async fn test_collision_bumping_warp_landing_points_and_cloak_velocity() {
        // parity: feat:navigation.collision-bumping, feat:navigation.cloak-velocity-penalty
        eve_testkit::attest(&[
            "feat:navigation.collision-bumping",
            "feat:navigation.cloak-velocity-penalty",
        ]);

        let ctx = build_ctx();
        let kw = HashMap::new();
        let jita_sys: u32 = 30_000_142;
        let char_a: i64 = 94_011;
        let ship_a: i64 = 84_011;
        let char_b: i64 = 94_012;
        let ship_b: i64 = 84_012;
        let char_obs: i64 = 94_013;
        let ship_obs: i64 = 84_013;
        let mut sess_a = seed_pilot(&ctx, char_a, ship_a, 587, jita_sys, false);
        let _sess_b = seed_pilot(&ctx, char_b, ship_b, 640, jita_sys, false);
        let _sess_obs = seed_pilot(&ctx, char_obs, ship_obs, 587, jita_sys, false);
        let sys = ctx.systems.get_or_spawn(jita_sys);

        // 1. Ship-to-ship collision bumping (SDE hull masses, closed-form impulse, observer updates, & align reset)
        let bump_origin = DVec3::new(80_000_000_000.0, 0.0, 0.0);
        let ship_b_pos = bump_origin + DVec3::new(150.0, 0.0, 0.0);
        spawn_in_space(&ctx, char_a, ship_a, 587, jita_sys, bump_origin).await;
        spawn_in_space(&ctx, char_b, ship_b, 640, jita_sys, ship_b_pos).await;
        spawn_in_space(&ctx, char_obs, ship_obs, 587, jita_sys, bump_origin + DVec3::new(0.0, 2_000.0, 0.0)).await;
        for _ in 0..30 {
            let _ = sys.step_manual(1.0).await.unwrap();
        }
        let _ = sys.get_set_state(ship_obs).await.unwrap();
        let _ = sys.drain_observer_updates(ship_obs).await.unwrap();

        sys.set_movement_mode(ship_a, MovementMode::GotoDirection { direction: DVec3::X }).await.unwrap();
        sys.set_speed_fraction(ship_a, 1.0).await.unwrap();
        let mut align_b = MovementMode::new_warp(
            ship_b_pos,
            bump_origin + DVec3::new(50_000_000.0, 0.0, 0.0),
            3.0,
        );
        if let MovementMode::Warp { ref mut elapsed_warp_time, .. } = align_b {
            *elapsed_warp_time = 6.0;
        }
        sys.set_movement_mode(ship_b, align_b).await.unwrap();
        sys.set_speed_fraction(ship_b, 1.0).await.unwrap();
        let (_, pre_a) = sys.get_ball_snapshot(ship_a).await.unwrap().unwrap();
        let (_, pre_b) = sys.get_ball_snapshot(ship_b).await.unwrap().unwrap();
        assert_ne!(pre_a.mass, pre_b.mass, "Two different SDE hulls must have distinct masses");

        let tick_b = sys.step_manual(1.0).await.unwrap();
        let (_, post_a) = sys.get_ball_snapshot(ship_a).await.unwrap().unwrap();
        let (_, post_b) = sys.get_ball_snapshot(ship_b).await.unwrap().unwrap();
        assert!(post_a.position.distance(post_b.position) >= post_a.radius + post_b.radius - 1e-3);
        let tau_a = (pre_a.inertia * pre_a.mass / 1_000_000.0).max(0.01);
        let tau_b = (pre_b.inertia * pre_b.mass / 1_000_000.0).max(0.01);
        let v_a_pre = pre_a.max_velocity + (pre_a.velocity.x - pre_a.max_velocity) * (-1.0 / tau_a).exp();
        let v_b_pre = pre_b.max_velocity * (1.0 - (-1.0 / tau_b).exp());
        let impulse = 1.8 * (v_a_pre - v_b_pre) / ((1.0 / pre_a.mass) + (1.0 / pre_b.mass));
        assert!((post_a.velocity.x - (v_a_pre - impulse / pre_a.mass)).abs() < 1e-3);
        assert!((post_b.velocity.x - (v_b_pre + impulse / pre_b.mass)).abs() < 1e-3);
        let obs_evs = tick_b.observer_events.get(&ship_obs).cloned().unwrap_or_default();
        assert!(
            tick_b.events.iter().any(|e| matches!(e, DestinyEvent::SetBallPosition { ball_id, .. } if *ball_id == ship_b))
                && tick_b.events.iter().any(|e| matches!(e, DestinyEvent::SetBallVelocity { ball_id, .. } if *ball_id == ship_b))
                && obs_evs.iter().any(|e| matches!(e, DestinyEvent::SetBallPosition { ball_id, .. } if *ball_id == ship_b))
                && obs_evs.iter().any(|e| matches!(e, DestinyEvent::SetBallVelocity { ball_id, .. } if *ball_id == ship_b))
        );
        if let MovementMode::Warp { phase, elapsed_warp_time, .. } = post_b.mode {
            assert_eq!(phase, WarpPhase::Aligning);
            assert_eq!(elapsed_warp_time, 0.0);
        } else {
            panic!("Expected ship B to remain in Aligning warp phase after bump");
        }

        // 1b. Ship-vs-Station, Ship-vs-Upwell Structure, Ship-vs-Stargate, and Ship-vs-Planet static hull collision bumping
        let station_pos = ctx.sde.stations[&JITA_STATION].position;
        let (_, sta_ball) = sys.get_ball_snapshot(JITA_STATION).await.unwrap().unwrap();
        let struct_id: i64 = 1_040_000_001;
        let struct_pos = [90_000_000_000.0, 0.0, 0.0];
        ctx.db.with_mut_snapshot(|snap| {
            snap.structures.insert(
                struct_id,
                UpwellStructureRecord {
                    structure_id: struct_id, type_id: 35832, owner_corp_id: 1_000_125, solar_system_id: jita_sys,
                    name: "Test Astrahus".into(), position: struct_pos, state: "ShieldVulnerable".into(),
                    quantum_core_installed: true, fuel_blocks: 500, shield_hp: 1_000_000.0, armor_hp: 1_000_000.0,
                    hull_hp: 1_000_000.0, reinforced_until_unix: 0, last_damage_tick: 0, last_damage_unix: 0,
                    self_destruct_at_unix: None,
                },
            );
        });
        eve_server::services::station_structure::structure_lifecycle_rpc::ensure_structure_ball_in_space(&ctx, jita_sys, struct_id).await;
        let (_, struct_ball) = sys.get_ball_snapshot(struct_id).await.unwrap().unwrap();
        let gate = ctx.sde.jumps.values().find(|j| j.from_solar_system_id == jita_sys).expect("Jita gate");
        let (_, gate_ball) = sys.get_ball_snapshot(gate.stargate_id).await.unwrap().unwrap();
        let planet = ctx.sde.celestials_in_system(jita_sys).into_iter().find(|c| c.group_id == 7 && c.radius > 0.0).expect("Jita planet");
        let moon = ctx.sde.celestials_in_system(jita_sys).into_iter().find(|c| c.group_id == 8 && c.radius > 0.0).expect("Jita moon");
        for (static_id, static_pos, core_rad, label) in [
            (JITA_STATION, station_pos, sta_ball.radius, "NPC station"),
            (struct_id, DVec3::from_array(struct_pos), struct_ball.radius, "Upwell structure"),
            (gate.stargate_id, gate.position, gate_ball.radius, "stargate"),
            (planet.celestial_id, planet.position, planet.radius, "planet"),
            (moon.celestial_id, moon.position, moon.radius, "moon"),
        ] {
            let mut cs_coll = ctx.build_ship_combat_state(char_a, ship_a, 640);
            cs_coll.undock_invulnerable_remaining_ticks = 0;
            sys.spawn_ball_with_meta(
                Ball {
                    id: ship_a, type_id: 640, position: static_pos + DVec3::new(core_rad - 50.0, 0.0, 0.0),
                    velocity: DVec3::new(-500.0, 0.0, 0.0), max_velocity: 500.0, inertia: 0.5,
                    mass: 10_000_000.0, radius: 100.0, speed_fraction: 0.0, mode: MovementMode::Stop,
                },
                cs_coll,
                ctx.build_ship_slim_meta(char_a, 640),
            ).await.unwrap();
            let tick_s = sys.step_manual(1.0).await.unwrap();
            let (_, post_s) = sys.get_ball_snapshot(ship_a).await.unwrap().unwrap();
            assert!(
                post_s.position.distance(static_pos) >= core_rad + post_s.radius - 1e-3,
                "Ship must not interpenetrate {label} hull"
            );
            assert!(post_s.velocity.x > 0.0, "Ship must bounce off {label} hull");
            assert!(tick_s.events.iter().any(|e| matches!(e, DestinyEvent::Collision { ball_a, ball_b } if (*ball_a == ship_a && *ball_b == static_id) || (*ball_a == static_id && *ball_b == ship_a))));
        }
        let (_, post_struct) = sys.get_ball_snapshot(struct_id).await.unwrap().unwrap();
        assert!(post_struct.position.distance(DVec3::from_array(struct_pos)) < 1e-6, "Anchored Upwell structure must remain stationary");

        // 2. Celestial, Stargate, Station, & Upwell Structure Warp Landing Points
        let b_jita = ctx
            .bind_object(&mut sess_a, "beyonce", jita_sys as i64)
            .expect("bind beyonce Jita");
        let mut cs_warp = ctx.build_ship_combat_state(char_a, ship_a, 640);
        cs_warp.capacitor = 100_000.0;
        cs_warp.max_capacitor = 100_000.0;
        sys.set_combat_state(ship_a, cs_warp).await.unwrap();

        // 2a. Planet (group 7) at 0m -> anchor_pos; at 50km -> 50,000 m; <150km effective -> WarpDistanceTooShort; NaN/Inf -> InvalidArgument
        sys.spawn_ball(Ball {
            id: ship_a, type_id: 640, position: planet.position - DVec3::new(planet.radius + 500_000_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO, max_velocity: 300.0, inertia: 0.1, mass: 1_000_000.0, radius: 100.0,
            speed_fraction: 0.0, mode: MovementMode::Stop,
        }).await.unwrap();
        for bad_range in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let nan_err = ctx.dispatch_rpc(
                &mut sess_a, "beyonce", Some(b_jita), "CmdWarpToStuff",
                &[PyRep::String("item".into()), PyRep::Integer(planet.celestial_id), PyRep::Real(bad_range)], &kw, None,
            ).await.unwrap_err();
            assert_eq!(nan_err.code, "InvalidArgument", "Non-finite warp range {bad_range} must be rejected");
        }
        ctx.dispatch_rpc(
            &mut sess_a, "beyonce", Some(b_jita), "CmdWarpToStuff",
            &[PyRep::String("item".into()), PyRep::Integer(planet.celestial_id), PyRep::Real(0.0)], &kw, None,
        ).await.expect("Warp to planet at 0m");
        let (_, w_planet) = sys.get_ball_snapshot(ship_a).await.unwrap().unwrap();
        let MovementMode::Warp { destination: dest_planet, .. } = w_planet.mode else { panic!("Expected Warp"); };
        assert!((dest_planet.distance(planet.position) - (planet.radius + 2_500.0)).abs() < 10.0);

        sys.set_movement_mode(ship_a, MovementMode::Stop).await.unwrap();
        ctx.dispatch_rpc(
            &mut sess_a, "beyonce", Some(b_jita), "CmdWarpToStuff",
            &[PyRep::String("item".into()), PyRep::Integer(planet.celestial_id), PyRep::Real(50_000.0)], &kw, None,
        ).await.expect("Warp to planet at 50km");
        let (_, w_planet_50) = sys.get_ball_snapshot(ship_a).await.unwrap().unwrap();
        let MovementMode::Warp { destination: dest_planet_50, .. } = w_planet_50.mode else { panic!("Expected Warp"); };
        assert!((dest_planet_50.distance(planet.position) - (planet.radius + 2_500.0 + 50_000.0)).abs() < 10.0);

        // 130 km from planet warp-in point (< 150 km minimum warp distance) -> WarpDistanceTooShort!
        sys.spawn_ball(Ball {
            id: ship_a, type_id: 640, position: planet.position - DVec3::new(planet.radius + 2_500.0 + 130_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO, max_velocity: 300.0, inertia: 0.1, mass: 1_000_000.0, radius: 100.0,
            speed_fraction: 0.0, mode: MovementMode::Stop,
        }).await.unwrap();
        let short_err = ctx.dispatch_rpc(
            &mut sess_a, "beyonce", Some(b_jita), "CmdWarpToStuff",
            &[PyRep::String("item".into()), PyRep::Integer(planet.celestial_id), PyRep::Real(50_000.0)], &kw, None,
        ).await.unwrap_err();
        assert_eq!(short_err.code, "WarpDistanceTooShort");

        // 2b. Star (group 6) -> radius + 100,000 m standoff
        let star = ctx
            .sde
            .celestials_in_system(jita_sys)
            .into_iter()
            .find(|c| c.group_id == 6 && c.radius > 0.0)
            .expect("Jita star");
        sys.spawn_ball(Ball {
            id: ship_a, type_id: 640, position: star.position - DVec3::new(star.radius + 2_000_000_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO, max_velocity: 300.0, inertia: 0.1, mass: 1_000_000.0, radius: 100.0,
            speed_fraction: 0.0, mode: MovementMode::Stop,
        }).await.unwrap();
        ctx.dispatch_rpc(
            &mut sess_a, "beyonce", Some(b_jita), "CmdWarpToStuff",
            &[PyRep::String("item".into()), PyRep::Integer(star.celestial_id), PyRep::Real(0.0)], &kw, None,
        ).await.expect("Warp to star at 0m");
        let (_, w_star) = sys.get_ball_snapshot(ship_a).await.unwrap().unwrap();
        let MovementMode::Warp { destination: dest_star, .. } = w_star.mode else { panic!("Expected Warp"); };
        assert!((dest_star.distance(star.position) - (star.radius + 100_000.0)).abs() < 10.0);

        // 2c. Stargate (`sde.jumps`) -> gate_radius standoff (0m from gate perimeter)
        let gate = ctx.sde.jumps.values().find(|j| j.from_solar_system_id == jita_sys).expect("Jita gate");
        let gate_radius = ctx.sde.types.get(&16).map(|t| t.radius).unwrap_or(14_051.0);
        sys.spawn_ball(Ball {
            id: ship_a, type_id: 640, position: gate.position - DVec3::new(10_000_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO, max_velocity: 300.0, inertia: 0.1, mass: 1_000_000.0, radius: 100.0,
            speed_fraction: 0.0, mode: MovementMode::Stop,
        }).await.unwrap();
        ctx.dispatch_rpc(
            &mut sess_a, "beyonce", Some(b_jita), "CmdWarpToStuff",
            &[PyRep::String("item".into()), PyRep::Integer(gate.stargate_id), PyRep::Real(0.0)], &kw, None,
        ).await.expect("Warp to stargate at 0m");
        let (_, w_gate) = sys.get_ball_snapshot(ship_a).await.unwrap().unwrap();
        let MovementMode::Warp { destination: dest_gate, .. } = w_gate.mode else { panic!("Expected Warp"); };
        assert!((dest_gate.distance(gate.position) - gate_radius).abs() < 10.0);

        // 2d. NPC Station & Upwell Structure -> 2,000 m docking ring perimeter (>0m from center, inside <=2,500m CmdDock perimeter)
        let station_pos = ctx.sde.stations[&JITA_STATION].position;
        sys.spawn_ball(Ball {
            id: ship_a, type_id: 640, position: station_pos - DVec3::new(10_000_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO, max_velocity: 300.0, inertia: 0.1, mass: 1_000_000.0, radius: 100.0,
            speed_fraction: 0.0, mode: MovementMode::Stop,
        }).await.unwrap();
        ctx.dispatch_rpc(
            &mut sess_a, "beyonce", Some(b_jita), "CmdWarpToStuff",
            &[PyRep::String("item".into()), PyRep::Integer(JITA_STATION), PyRep::Real(0.0)], &kw, None,
        ).await.expect("Warp to NPC station at 0m");
        let (_, w_sta) = sys.get_ball_snapshot(ship_a).await.unwrap().unwrap();
        let MovementMode::Warp { destination: dest_sta, .. } = w_sta.mode else { panic!("Expected Warp"); };
        assert!((dest_sta.distance(station_pos) - sta_ball.radius).abs() < 10.0, "NPC station landing must be on Zero Point Envelope (radius)");

        sys.spawn_ball(Ball {
            id: ship_a, type_id: 640, position: DVec3::from_array(struct_pos) - DVec3::new(10_000_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO, max_velocity: 300.0, inertia: 0.1, mass: 1_000_000.0, radius: 100.0,
            speed_fraction: 0.0, mode: MovementMode::Stop,
        }).await.unwrap();
        ctx.dispatch_rpc(
            &mut sess_a, "beyonce", Some(b_jita), "CmdWarpToStuff",
            &[PyRep::String("item".into()), PyRep::Integer(struct_id), PyRep::Real(0.0)], &kw, None,
        ).await.expect("Warp to Upwell structure at 0m");
        let (_, w_struct) = sys.get_ball_snapshot(ship_a).await.unwrap().unwrap();
        let MovementMode::Warp { destination: dest_struct, .. } = w_struct.mode else { panic!("Expected Warp"); };
        assert!((dest_struct.distance(DVec3::from_array(struct_pos)) - struct_ball.radius).abs() < 10.0, "Structure landing must be on Zero Point Envelope (radius)");

        // 3. Cloak Velocity Penalty & Restoration (`feat:navigation.cloak-velocity-penalty`)
        for (cloak_tid, expected_mult) in [(11_370_u32, 0.10_f64), (11_577, 0.25), (11_578, 1.00)] {
            let hull_tid: u32 = if cloak_tid == 11_578 { 11_192 } else { 640 };
            let cloak_mod_id: i64 = 74_100 + cloak_tid as i64;
            ctx.db.with_mut_snapshot(|snap| {
                snap.items.retain(|_, i| i.location_id != ship_a);
                if let Some(s) = snap.items.get_mut(&ship_a) {
                    s.type_id = hull_tid;
                }
                snap.module_online_states.clear();
                snap.module_active_states.clear();
            });
            give_item(&ctx, cloak_mod_id, cloak_tid, char_a, ship_a, InventoryFlag::HiSlot0, 1);
            ctx.db.with_mut_snapshot(|snap| { snap.module_online_states.insert(cloak_mod_id, true); });
            ctx.invalidate_ship_fitting(ship_a);

            let pre_all_info = rpc(&ctx, &mut sess_a, "dogmaIM", "GetAllInfo", vec![PyRep::Bool(true), PyRep::Bool(true)])
                .await.unwrap();
            let base_vel = attr_from_all_info(&pre_all_info, "shipInfo", 37);
            let mut clean_cs = ctx.build_ship_combat_state(char_a, ship_a, hull_tid);
            clean_cs.capacitor = 100_000.0;
            clean_cs.max_capacitor = 100_000.0;
            let _ = sys.remove_ball(ship_a).await;
            sys.spawn_ball_with_meta(
                Ball {
                    id: ship_a, type_id: hull_tid, position: DVec3::new(95_000_000_000.0, 0.0, 0.0),
                    velocity: DVec3::ZERO, max_velocity: base_vel, inertia: 1.0, mass: 10_000_000.0,
                    radius: 100.0, speed_fraction: 0.0, mode: MovementMode::Stop,
                },
                clean_cs,
                ctx.build_ship_slim_meta(char_a, hull_tid),
            ).await.unwrap();

            rpc(&ctx, &mut sess_a, "dogmaIM", "Activate", vec![PyRep::Integer(cloak_mod_id)])
                .await.expect("Activate cloaking device");
            let (_, cloaked_ball) = sys.get_ball_snapshot(ship_a).await.unwrap().unwrap();
            assert!((cloaked_ball.max_velocity - base_vel * expected_mult).abs() < 1e-3);
            let all_info = rpc(&ctx, &mut sess_a, "dogmaIM", "GetAllInfo", vec![PyRep::Bool(true), PyRep::Bool(true)])
                .await.unwrap();
            assert!((attr_from_all_info(&all_info, "shipInfo", 37) - base_vel * expected_mult).abs() < 1e-3);

            rpc(&ctx, &mut sess_a, "dogmaIM", "Deactivate", vec![PyRep::Integer(cloak_mod_id)])
                .await.expect("Deactivate cloaking device");
            let (_, decloaked_ball) = sys.get_ball_snapshot(ship_a).await.unwrap().unwrap();
            assert!((decloaked_ball.max_velocity - base_vel).abs() < 1e-3);
        }
    }

    #[test]
    fn test_realistic_player_ship_stargate_and_station_collision_separation() {
        let mut bp = eve_destiny::Ballpark::new(1);
        let (gate_id, station_id, ship_id) = (50_000_001, 60_000_001, 100_000_042);
        bp.add_ball(Ball { id: gate_id, type_id: 16, position: DVec3::ZERO, velocity: DVec3::ZERO, max_velocity: 0.0, inertia: 0.0, mass: 1e12, radius: 2_000.0, speed_fraction: 0.0, mode: MovementMode::Stop });
        bp.add_ball(Ball { id: station_id, type_id: 1529, position: DVec3::new(50_000.0, 0.0, 0.0), velocity: DVec3::ZERO, max_velocity: 0.0, inertia: 0.0, mass: 1e12, radius: 5_000.0, speed_fraction: 0.0, mode: MovementMode::Stop });
        bp.add_ball(Ball { id: ship_id, type_id: 587, position: DVec3::new(1_500.0, 0.0, 0.0), velocity: DVec3::ZERO, max_velocity: 300.0, inertia: 1.0, mass: 1_000_000.0, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop });
        bp.step(1.0);
        assert!(bp.balls[&ship_id].position.x >= 2_049.9, "Ship must separate from stargate even at v_rel == 0: {:?}", bp.balls[&ship_id].position);
        bp.balls.get_mut(&ship_id).unwrap().position = DVec3::new(45_500.0, 0.0, 0.0);
        bp.step(1.0);
        assert!(bp.balls[&ship_id].position.x <= 44_950.1, "Ship must separate from NPC station even at v_rel == 0: {:?}", bp.balls[&ship_id].position);
    }
}

pub(crate) mod warp_bubble_drag_catch_nullifier {
    // End-to-End Integration Tests closing Interdiction & MJD/Propulsion parity rows (Part 2):
    // `feat:navigation.warp-bubble-drag-and-catch`, `feat:navigation.interdiction-nullification-module`,
    // `feat:modules.interdiction-probe-variants`, `feat:navigation.micro-jump-drive-spool`,
    // and `feat:navigation.warp-scramble-propulsion-shutoff`.

    use crate::modules_advanced_tactical::*;
    use eve_db::{FleetRecord, InventoryFlag};
    use eve_destiny::{Ball, DestinyEvent, DVec3, MovementMode};
    use eve_proto::PyRep;
    use eve_server::services::insurgency_services::{
        add_system_corruption, init_insurgency_campaign, FACTION_ANGEL_CARTEL, FORECASTING_DURATION_SECS,
    };
    use eve_server::session_cluster_lifecycle::handle_character_logoff_or_disconnect;
    use std::collections::HashMap;

    #[tokio::test]
    async fn test_warp_bubble_drag_catch_nullifier_and_probe_variants() {
        // parity: feat:navigation.warp-bubble-drag-and-catch, feat:navigation.interdiction-nullification-module, feat:modules.interdiction-probe-variants
        eve_testkit::attest(&[
            "feat:navigation.warp-bubble-drag-and-catch",
            "feat:navigation.interdiction-nullification-module",
            "feat:modules.interdiction-probe-variants",
        ]);

        let ctx = build_ctx();
        let kw = HashMap::new();
        let null_sys: u32 = 30_004_608; // 6VDT-H (-0.70 null-sec)
        let sabre_char: i64 = 94_021;
        let sabre_id: i64 = 84_021;
        let target_char: i64 = 94_022;
        let target_ship_id: i64 = 84_022;
        let shuttle_char: i64 = 94_023;
        let shuttle_id: i64 = 84_023;

        let mut sabre_sess = seed_pilot(&ctx, sabre_char, sabre_id, 22_456, null_sys, false);
        let mut target_sess = seed_pilot(&ctx, target_char, target_ship_id, 12_729, null_sys, false);
        let mut shuttle_sess = seed_pilot(&ctx, shuttle_char, shuttle_id, 672, null_sys, false);
        let sys = ctx.systems.get_or_spawn(null_sys);

        // 1. Warp Bubble Catch & Drag + Shuttle Immunity (`feat:navigation.warp-bubble-drag-and-catch`)
        let origin = DVec3::new(50_010_000_000.0, 0.0, 0.0);
        let dest = DVec3::new(50_030_000_000.0, 0.0, 0.0);
        let dest_beacon_id: i64 = 88_701;
        sys.spawn_ball(Ball {
            id: dest_beacon_id, type_id: 15, position: dest, velocity: DVec3::ZERO,
            max_velocity: 0.0, inertia: 1.0, mass: 1e12, radius: 100.0,
            speed_fraction: 0.0, mode: MovementMode::Rigid,
        }).await.unwrap();

        // 1a. Mid-path bubble at 20,000 km on a 30,000 km warp (10,000 km from destination) must NOT catch
        let mid_bubble_id: i64 = 88_702;
        sys.add_warp_bubble(mid_bubble_id, DVec3::new(50_020_000_000.0, 0.0, 0.0), 20_000.0).await.unwrap();
        spawn_in_space(&ctx, target_char, target_ship_id, 12_729, null_sys, origin).await;
        spawn_in_space(&ctx, shuttle_char, shuttle_id, 672, null_sys, origin).await;
        // Step 1 tick first to prove reconcile_tackle_and_bubbles preserves Shuttle passive SDE 1538 immunity!
        sys.step_manual(1.0).await.unwrap();

        let b_target = ctx.bind_object(&mut target_sess, "beyonce", null_sys as i64).unwrap();
        let b_shuttle = ctx.bind_object(&mut shuttle_sess, "beyonce", null_sys as i64).unwrap();

        ctx.dispatch_rpc(
            &mut target_sess, "beyonce", Some(b_target), "CmdWarpToStuff",
            &[PyRep::String("item".into()), PyRep::Integer(dest_beacon_id), PyRep::Real(0.0)], &kw, None,
        ).await.expect("Crane warp past mid-path bubble");
        let (_, crane_mid) = sys.get_ball_snapshot(target_ship_id).await.unwrap().unwrap();
        let MovementMode::Warp { destination: crane_mid_dest, .. } = crane_mid.mode else {
            panic!("Expected Crane in Warp mode");
        };
        assert!((crane_mid_dest.x - 50_030_000_000.0).abs() < 10.0, "mid-path bubble must not catch; got {}", crane_mid_dest.x);

        // 1b. Exit-corridor catch bubble within 150 km in front of destination (at 29,900 km, 20 km radius -> catches at 29,880 km)
        let catch_bubble_id: i64 = 88_704;
        sys.add_warp_bubble(catch_bubble_id, DVec3::new(50_029_900_000.0, 0.0, 0.0), 20_000.0).await.unwrap();
        let _ = sys.remove_ball(target_ship_id).await;
        spawn_in_space(&ctx, target_char, target_ship_id, 12_729, null_sys, origin).await;
        sys.step_manual(1.0).await.unwrap();
        ctx.dispatch_rpc(
            &mut target_sess, "beyonce", Some(b_target), "CmdWarpToStuff",
            &[PyRep::String("item".into()), PyRep::Integer(dest_beacon_id), PyRep::Real(0.0)], &kw, None,
        ).await.expect("Crane warp into exit-corridor catch bubble");
        for _ in 0..10 { sys.step_manual(1.0).await.unwrap(); }
        let (_, crane_catch) = sys.get_ball_snapshot(target_ship_id).await.unwrap().unwrap();
        let MovementMode::Warp { destination: crane_catch_dest, .. } = crane_catch.mode else {
            panic!("Expected Crane in Warp mode");
        };
        assert!((crane_catch_dest.x - 50_029_880_000.0).abs() < 10.0, "got {}", crane_catch_dest.x);

        // Shuttle (group 31, SDE attr 1538 = 1.0) ignores catch bubble AND warps out from inside an active bubble
        let origin_bubble_id: i64 = 88_705;
        sys.add_warp_bubble(origin_bubble_id, origin, 20_000.0).await.unwrap();
        spawn_in_space(&ctx, shuttle_char, shuttle_id, 672, null_sys, origin).await;
        ctx.dispatch_rpc(
            &mut shuttle_sess, "beyonce", Some(b_shuttle), "CmdWarpToStuff",
            &[PyRep::String("item".into()), PyRep::Integer(dest_beacon_id), PyRep::Real(0.0)], &kw, None,
        ).await.expect("Shuttle warp from inside active bubble");
        let (_, shuttle_catch) = sys.get_ball_snapshot(shuttle_id).await.unwrap().unwrap();
        let MovementMode::Warp { destination: shuttle_dest, .. } = shuttle_catch.mode else {
            panic!("Expected Shuttle in Warp mode");
        };
        assert!((shuttle_dest.x - 50_030_000_000.0).abs() < 10.0, "got {}", shuttle_dest.x);
        sys.remove_warp_bubble(origin_bubble_id).await.unwrap();
        sys.remove_warp_bubble(catch_bubble_id).await.unwrap();

        // 1b. Drag bubble 100 km BEHIND destination at 30,100 km (20 km radius -> drags Crane to 30,080 km!)
        let drag_bubble_id: i64 = 88_703;
        sys.add_warp_bubble(drag_bubble_id, DVec3::new(50_030_100_000.0, 0.0, 0.0), 20_000.0).await.unwrap();
        let _ = sys.remove_ball(target_ship_id).await;
        spawn_in_space(&ctx, target_char, target_ship_id, 12_729, null_sys, origin).await;
        sys.step_manual(1.0).await.unwrap();
        ctx.dispatch_rpc(
            &mut target_sess, "beyonce", Some(b_target), "CmdWarpToStuff",
            &[PyRep::String("item".into()), PyRep::Integer(dest_beacon_id), PyRep::Real(0.0)], &kw, None,
        ).await.expect("Crane warp into drag bubble");
        for _ in 0..10 { sys.step_manual(1.0).await.unwrap(); }
        let (_, crane_drag) = sys.get_ball_snapshot(target_ship_id).await.unwrap().unwrap();
        let MovementMode::Warp { destination: crane_drag_dest, .. } = crane_drag.mode else {
            panic!("Expected Crane in Warp mode for drag");
        };
        assert!((crane_drag_dest.x - 50_030_080_000.0).abs() < 10.0, "got {}", crane_drag_dest.x);

        // 1c. Capital cyno jump blocked inside bubble, while disconnect emergency logoff warp succeeds!
        let near_null_sys = ctx
            .sde
            .solar_systems
            .values()
            .find(|s| {
                s.solar_system_id != null_sys
                    && s.security_status < 0.0
                    && s.solar_system_id < 31_000_000
                    && {
                        let pa = ctx.sde.solar_systems[&null_sys].galactic_pos_ly;
                        let pb = s.galactic_pos_ly;
                        let d = ((pa[0] - pb[0]).powi(2) + (pa[1] - pb[1]).powi(2) + (pa[2] - pb[2]).powi(2)).sqrt();
                        (0.1..5.0).contains(&d)
                    }
            })
            .expect("nearby null-sec cyno destination")
            .solar_system_id;
        let cyno_char: i64 = 94_026;
        let cyno_ship: i64 = 84_026;
        let dread_char: i64 = 94_027;
        let dread_ship: i64 = 84_027;
        let mut cyno_sess = seed_pilot(&ctx, cyno_char, cyno_ship, 11_957, near_null_sys, false); // Falcon
        let mut dread_sess = seed_pilot(&ctx, dread_char, dread_ship, 19_720, null_sys, false); // Revelation
        ctx.db.with_mut_snapshot(|snap| {
            snap.fleets.insert(
                77_701,
                FleetRecord { fleet_id: 77_701, boss_id: cyno_char, members: vec![cyno_char, dread_char], ..Default::default() },
            );
        });
        spawn_in_space(&ctx, cyno_char, cyno_ship, 11_957, near_null_sys, DVec3::ZERO).await;
        give_item(&ctx, 74_190, 21_096, cyno_char, cyno_ship, InventoryFlag::HiSlot0, 1);
        give_item(&ctx, 74_191, 16_273, cyno_char, cyno_ship, InventoryFlag::Cargo, 2_000);
        ctx.db.with_mut_snapshot(|s| { s.module_online_states.insert(74_190, true); });
        ctx.invalidate_ship_fitting(cyno_ship);
        let cyno_rep = rpc(&ctx, &mut cyno_sess, "dogmaIM", "Activate", vec![
            PyRep::Integer(74_190), PyRep::String("cynosuralGeneration".into()),
        ]).await.expect("Light cyno in near_null_sys");
        let cyno_beacon_id = cyno_rep.dict_get("beaconID").and_then(|v| v.as_i64()).unwrap();

        spawn_in_space(&ctx, dread_char, dread_ship, 19_720, null_sys, DVec3::new(50_030_100_000.0, 0.0, 0.0)).await;
        give_item(&ctx, 74_192, 16_274, dread_char, dread_ship, InventoryFlag::Cargo, 50_000); // Helium Isotopes
        let jump_blocked = rpc(&ctx, &mut dread_sess, "beyonce", "CmdJumpToFleetModuleBeacon", vec![
            PyRep::Integer(cyno_char), PyRep::Integer(near_null_sys as i64), PyRep::Integer(cyno_beacon_id),
        ]).await.unwrap_err();
        assert_eq!(jump_blocked.code, "WarpScrambled", "Warp disruption bubble must block capital cyno jump");

        // Disconnect emergency logoff warp inside bubble is NOT blocked when ship has no point scramble
        let disc_entry = handle_character_logoff_or_disconnect(&ctx, dread_char)
            .await
            .expect("Disconnect emergency warp entry");
        assert!(disc_entry.emergency_warp_dest.is_some(), "Bubble alone must NOT prevent disconnect emergency logoff warp");

        // 2. Interdiction Nullification Module (`feat:navigation.interdiction-nullification-module`)
        let mut cruiser_sess = seed_pilot(&ctx, 94_028, 84_028, 621, 30_000_142, true); // Caracal (T1 Cruiser)
        assert!(fit_mod(&ctx, &mut cruiser_sess, 84_028, 74_200, 58_919, 27).await.is_err(), "T1 Cruiser must not fit Interdiction Nullifier I");
        let mut bs_sess = seed_pilot(&ctx, 94_024, 84_024, 640, 30_000_142, true);
        assert!(fit_mod(&ctx, &mut bs_sess, 84_024, 74_201, 58_919, 27).await.is_err(), "Battleship must not fit Interdiction Nullifier I");

        let mut crane_fit_sess = seed_pilot(&ctx, target_char, target_ship_id, 12_729, 30_000_142, true);
        let pre_null_info = rpc(&ctx, &mut crane_fit_sess, "dogmaIM", "ItemGetInfo", vec![PyRep::Integer(target_ship_id)]).await.unwrap();
        let pre_target_range = attr_from_item_info(&pre_null_info, 76);
        let pre_scan_res = attr_from_item_info(&pre_null_info, 564);

        fit_mod(&ctx, &mut crane_fit_sess, target_ship_id, 74_202, 58_919, 27)
            .await.expect("Crane fits 1st Interdiction Nullifier I");
        assert!(fit_mod(&ctx, &mut crane_fit_sess, target_ship_id, 74_203, 58_919, 28).await.is_err(), "maxGroupFitted=1 must reject 2nd Nullifier");
        let post_null_info = rpc(&ctx, &mut crane_fit_sess, "dogmaIM", "ItemGetInfo", vec![PyRep::Integer(target_ship_id)]).await.unwrap();
        assert!(attr_from_item_info(&post_null_info, 76) < pre_target_range);
        assert!(attr_from_item_info(&post_null_info, 564) < pre_scan_res);

        let mut nereus_sess = seed_pilot(&ctx, 94_025, 84_025, 650, 30_000_142, true);
        let pre_ner_info = rpc(&ctx, &mut nereus_sess, "dogmaIM", "ItemGetInfo", vec![PyRep::Integer(84_025)]).await.unwrap();
        let pre_drone_bw = attr_from_item_info(&pre_ner_info, 1271);
        fit_mod(&ctx, &mut nereus_sess, 84_025, 74_204, 58_919, 27).await.expect("Nereus fits Interdiction Nullifier I");
        let post_ner_info = rpc(&ctx, &mut nereus_sess, "dogmaIM", "ItemGetInfo", vec![PyRep::Integer(84_025)]).await.unwrap();
        assert!((attr_from_item_info(&post_ner_info, 1271) - pre_drone_bw * 0.5).abs() < 1e-3);

        let far_beacon_id: i64 = 88_704;
        sys.spawn_ball(Ball {
            id: far_beacon_id, type_id: 15, position: DVec3::new(50_050_000_000.0, 0.0, 0.0), velocity: DVec3::ZERO,
            max_velocity: 0.0, inertia: 1.0, mass: 1e12, radius: 100.0,
            speed_fraction: 0.0, mode: MovementMode::Rigid,
        }).await.unwrap();
        target_sess.station_id = None;
        target_sess.solar_system_id = Some(null_sys);
        spawn_in_space(&ctx, target_char, target_ship_id, 12_729, null_sys, DVec3::new(50_030_100_000.0, 0.0, 0.0)).await;
        sys.set_ball_position(target_ship_id, DVec3::new(50_030_100_000.0, 0.0, 0.0)).await;
        let blocked_err = ctx.dispatch_rpc(
            &mut target_sess, "beyonce", Some(b_target), "CmdWarpToStuff",
            &[PyRep::String("item".into()), PyRep::Integer(far_beacon_id), PyRep::Real(0.0)], &kw, None,
        ).await.unwrap_err();
        assert_eq!(blocked_err.code, "WarpScrambled");

        rpc(&ctx, &mut target_sess, "dogmaIM", "Activate", vec![PyRep::Integer(74_202)])
            .await.expect("Activate Interdiction Nullifier I");
        ctx.dispatch_rpc(
            &mut target_sess, "beyonce", Some(b_target), "CmdWarpToStuff",
            &[PyRep::String("item".into()), PyRep::Integer(far_beacon_id), PyRep::Real(0.0)], &kw, None,
        ).await.expect("Active Interdiction Nullifier allows warp out of bubble");

        rpc(&ctx, &mut target_sess, "dogmaIM", "Deactivate", vec![PyRep::Integer(74_202)]).await.unwrap();
        let react_err = rpc(&ctx, &mut target_sess, "dogmaIM", "Activate", vec![PyRep::Integer(74_202)]).await.unwrap_err();
        assert_eq!(react_err.code, "ModuleReactivationDelay");
        sys.remove_warp_bubble(drag_bubble_id).await.unwrap();

        // 3. Interdiction Sphere Probe Variants, Security/Corruption Gates, Capacity/Reload, & Wubble (`feat:modules.interdiction-probe-variants`)
        // 3a. Max-1 Interdiction Sphere Launcher I (`22782`, `maxGroupFitted = 1`)
        let mut sabre_fit_sess = seed_pilot(&ctx, sabre_char, sabre_id, 22_456, 30_000_142, true);
        let launcher_id: i64 = 74_301;
        fit_mod(&ctx, &mut sabre_fit_sess, sabre_id, launcher_id, 22_782, 27).await.expect("Sabre fits 1st Interdiction Sphere Launcher I");
        assert!(fit_mod(&ctx, &mut sabre_fit_sess, sabre_id, 74_399, 22_782, 28).await.is_err(), "maxGroupFitted=1 blocks 2nd launcher");

        // 3b. High-sec refusal (`30_000_142`) and Low-sec (`30_002_537` Amamake) Corruption Stage 0 vs Stage 5
        sabre_sess.station_id = None;
        sabre_sess.solar_system_id = Some(30_000_142);
        spawn_in_space(&ctx, sabre_char, sabre_id, 22_456, 30_000_142, DVec3::new(10_000_000.0, 0.0, 0.0)).await;
        give_item(&ctx, 74_302, 22_778, sabre_char, sabre_id, InventoryFlag::HiSlot0, 3);
        ctx.invalidate_ship_fitting(sabre_id);
        let hs_err = rpc(&ctx, &mut sabre_sess, "dogmaIM", "Activate", vec![PyRep::Integer(launcher_id)]).await.unwrap_err();
        assert_eq!(hs_err.code, "CannotLaunchInterdictionProbeInEmpire");

        let low_sys: u32 = 30_002_537; // Amamake (0.4 low-sec)
        sabre_sess.solar_system_id = Some(low_sys);
        spawn_in_space(&ctx, sabre_char, sabre_id, 22_456, low_sys, DVec3::new(10_000_000.0, 0.0, 0.0)).await;
        let ls_err = rpc(&ctx, &mut sabre_sess, "dogmaIM", "Activate", vec![PyRep::Integer(launcher_id)]).await.unwrap_err();
        assert_eq!(ls_err.code, "CannotLaunchInterdictionProbeInEmpire");

        init_insurgency_campaign(&ctx, 501, FACTION_ANGEL_CARTEL, 500_002, low_sys, vec![low_sys]);
        ctx.advance_secs(FORECASTING_DURATION_SECS + 10);
        let (_, corr_stage) = add_system_corruption(&ctx, 501, low_sys, 1_000);
        assert_eq!(corr_stage, 5);
        let ls_stage5 = rpc(&ctx, &mut sabre_sess, "dogmaIM", "Activate", vec![PyRep::Integer(launcher_id)])
            .await.expect("Low-sec Corruption Stage 5 allows interdiction probe launch");
        assert_eq!(ls_stage5.dict_get("bubbleRadius"), Some(&PyRep::Real(20_000.0)));
        assert_eq!(ls_stage5.dict_get("lifetimeSeconds"), Some(&PyRep::Real(120.0)));
        ctx.advance_secs(6);

        // 3c. Move to null-sec (`null_sys`), test 15 m3 (3-probe) capacity + 60s auto-reload when last loaded probe is fired!
        sabre_sess.solar_system_id = Some(null_sys);
        let sabre_pos = DVec3::new(50_050_000_000.0, 0.0, 0.0);
        spawn_in_space(&ctx, sabre_char, sabre_id, 22_456, null_sys, sabre_pos).await;
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(st) = snap.items.get_mut(&74_302) {
                st.quantity = 1; // 1 probe left in launcher
            }
        });
        give_item(&ctx, 74_310, 22_778, sabre_char, sabre_id, InventoryFlag::Cargo, 4); // 4 in cargo -> 3 (15 m3) auto-reloaded
        let smartbomb_id: i64 = 74_303;
        let smartbomb_id_2: i64 = 74_306;
        let ab_sabre_id: i64 = 74_307;
        give_item(&ctx, smartbomb_id, 14_200, sabre_char, sabre_id, InventoryFlag::HiSlot1, 1);
        give_item(&ctx, smartbomb_id_2, 14_200, sabre_char, sabre_id, InventoryFlag::HiSlot2, 1);
        give_item(&ctx, ab_sabre_id, 439, sabre_char, sabre_id, InventoryFlag::MedSlot0, 1); // 1MN Afterburner I (439)
        ctx.db.with_mut_snapshot(|snap| {
            for &mid in &[launcher_id, smartbomb_id, smartbomb_id_2, ab_sabre_id] {
                snap.module_online_states.insert(mid, true);
            }
        });
        ctx.invalidate_ship_fitting(sabre_id);

        let p1 = rpc(&ctx, &mut sabre_sess, "dogmaIM", "Activate", vec![PyRep::Integer(launcher_id)])
            .await.expect("Launch last loaded Warp Disrupt Probe -> triggers 60s auto-reload");
        let p1_id = p1.dict_get("probeID").and_then(|v| v.as_i64()).unwrap();
        assert_eq!(p1.dict_get("lifetimeSeconds"), Some(&PyRep::Real(120.0)));
        assert_eq!(p1.dict_get("reloading"), Some(&PyRep::Bool(true)));
        assert_eq!(p1.dict_get("remainingInLauncher"), Some(&PyRep::Integer(3)));
        assert_eq!(ctx.crimewatch.read().unwrap().states.get(&sabre_char).map(|s| s.weapon_timer_until_unix).unwrap_or(0), 0);

        // At +10s (> 5s cadence, < 60s reload): blocked with ModuleIsReloading!
        ctx.advance_secs(10);
        let reload_err = rpc(&ctx, &mut sabre_sess, "dogmaIM", "Activate", vec![PyRep::Integer(launcher_id)]).await.unwrap_err();
        assert_eq!(reload_err.code, "ModuleIsReloading");

        // Smartbomb AoE destroys the 1,000 HP untargetable probe and removes its bubble
        assert_eq!(rpc(&ctx, &mut sabre_sess, "dogmaIM", "AddTarget", vec![PyRep::Integer(p1_id)]).await.unwrap_err().code, "InvalidTarget");
        assert!(sys.is_ball_scrambled(sabre_id).await.unwrap());
        rpc(&ctx, &mut sabre_sess, "dogmaIM", "Activate", vec![
            PyRep::Integer(smartbomb_id), PyRep::String("empWave".into()), PyRep::None, PyRep::Integer(1),
        ]).await.unwrap();
        rpc(&ctx, &mut sabre_sess, "dogmaIM", "Activate", vec![
            PyRep::Integer(smartbomb_id_2), PyRep::String("empWave".into()), PyRep::None, PyRep::Integer(1),
        ]).await.unwrap();
        for _ in 0..70 {
            sys.step_manual(1.0).await.unwrap();
            ctx.advance_secs(1);
        }
        let _ = rpc(&ctx, &mut sabre_sess, "dogmaIM", "Deactivate", vec![PyRep::Integer(smartbomb_id)]).await;
        let _ = rpc(&ctx, &mut sabre_sess, "dogmaIM", "Deactivate", vec![PyRep::Integer(smartbomb_id_2)]).await;
        assert!(sys.get_ball_snapshot(p1_id).await.unwrap().is_none(), "Smartbomb AoE must destroy probe");
        assert!(!sys.is_ball_scrambled(sabre_id).await.unwrap(), "Destroyed probe must remove bubble");

        // After 60s reload completed, 3 probes (15 m3 / 5 m3) were auto-loaded from the 4-probe cargo stack (1 left in cargo)!
        let p_reloaded = rpc(&ctx, &mut sabre_sess, "dogmaIM", "Activate", vec![PyRep::Integer(launcher_id)])
            .await.expect("Launch after 60s auto-reload");
        assert!(p_reloaded.dict_get("probeID").is_some());
        assert_eq!(ctx.db.with_snapshot(|s| s.items.get(&74_310).map(|i| i.quantity)), Some(1), "15m3 launcher reloads max 3 of 4 probes");
        let cadence_err = rpc(&ctx, &mut sabre_sess, "dogmaIM", "Activate", vec![PyRep::Integer(launcher_id)]).await.unwrap_err();
        assert_eq!(cadence_err.code, "ModuleReactivationDelay");
        ctx.advance_secs(6);
        // Move away from p_reloaded bubble before testing Wubble
        let wub_pos = DVec3::new(50_055_000_000.0, 0.0, 0.0);
        sys.set_ball_position(sabre_id, wub_pos).await;

        // 3d. Stasis Webification Probe (`56748`: SDE 15km, 30s lifetime, 2.0s arming delay, -20% speedFactor = 0.80x, composes with active AB!)
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.remove(&smartbomb_id);
            snap.items.remove(&smartbomb_id_2);
            snap.items.retain(|_, i| !(i.location_id == sabre_id && i.flag == InventoryFlag::HiSlot0 && i.item_id != launcher_id));
            snap.module_online_states.insert(ab_sabre_id, true);
        });
        give_item(&ctx, 74_304, 56_748, sabre_char, sabre_id, InventoryFlag::HiSlot0, 5);
        ctx.invalidate_ship_fitting(sabre_id);
        sys.set_capacitor(sabre_id, 5_000.0).await.unwrap();

        // Activate 1MN Afterburner II on Sabre BEFORE launching Wubble
        rpc(&ctx, &mut sabre_sess, "dogmaIM", "Activate", vec![PyRep::Integer(ab_sabre_id)])
            .await.expect("Activate 1MN Afterburner II on Sabre");
        let (_, sabre_ab_ball) = sys.get_ball_snapshot(sabre_id).await.unwrap().unwrap();
        let ab_vel = sabre_ab_ball.max_velocity;
        assert!(ab_vel > 700.0, "Active Afterburner must boost Sabre max_velocity: got {ab_vel}");

        let pw = rpc(&ctx, &mut sabre_sess, "dogmaIM", "Activate", vec![PyRep::Integer(launcher_id)])
            .await.expect("Launch Stasis Webification Probe 56748");
        let pw_id = pw.dict_get("probeID").and_then(|v| v.as_i64()).unwrap();
        assert_eq!(pw.dict_get("isWubble"), Some(&PyRep::Bool(true)));
        assert_eq!(pw.dict_get("bubbleRadius"), Some(&PyRep::Real(15_000.0)));
        assert_eq!(pw.dict_get("lifetimeSeconds"), Some(&PyRep::Real(30.0)));

        // At t = 1.0s (< 2.0s SDE arming delay attr 2262): probe is on grid and ship is NOT webbed yet!
        sys.step_manual(1.0).await.unwrap();
        assert!(sys.get_ball_snapshot(pw_id).await.unwrap().is_some(), "Arming wubble probe must remain on grid before detonation");
        let (_, sabre_t1) = sys.get_ball_snapshot(sabre_id).await.unwrap().unwrap();
        assert!((sabre_t1.max_velocity - ab_vel).abs() < 1e-2, "Wubble must not web during 2.0s arming delay");

        // At t = 2.0s (detonated): probe ball is immediately removed from grid; 0.80x debuff persists 30s without warp scramble!
        sys.step_manual(1.0).await.unwrap();
        assert!(sys.get_ball_snapshot(pw_id).await.unwrap().is_none(), "Detonated wubble probe must be removed from grid immediately");
        let (_, sabre_wub) = sys.get_ball_snapshot(sabre_id).await.unwrap().unwrap();
        assert!((sabre_wub.max_velocity - ab_vel * 0.80).abs() < 1e-2, "Wubble must apply SDE 0.80x to AB velocity: got {} vs {}", sabre_wub.max_velocity, ab_vel * 0.80);
        assert!(!sys.is_ball_scrambled(sabre_id).await.unwrap());

        // Keep capacitor topped up and step 31s past 30s Wubble debuff lifetime -> restores FULL AB-boosted velocity!
        for _ in 0..31 {
            sys.set_capacitor(sabre_id, 5_000.0).await.unwrap();
            sys.step_manual(1.0).await.unwrap();
            ctx.advance_secs(1);
        }
        let (_, sabre_post_wub) = sys.get_ball_snapshot(sabre_id).await.unwrap().unwrap();
        assert!((sabre_post_wub.max_velocity - ab_vel).abs() < 1e-2, "Wubble expiry must restore active AB velocity ({ab_vel}), got {}", sabre_post_wub.max_velocity);

        // 3e. Surgical Warp Disrupt Probe (34260: 10km radius, 180s lifetime)
        ctx.db.with_mut_snapshot(|snap| { snap.items.remove(&74_304); });
        give_item(&ctx, 74_305, 34_260, sabre_char, sabre_id, InventoryFlag::HiSlot0, 2);
        ctx.invalidate_ship_fitting(sabre_id);
        let ps = rpc(&ctx, &mut sabre_sess, "dogmaIM", "Activate", vec![PyRep::Integer(launcher_id)])
            .await.expect("Launch Surgical Warp Disrupt Probe 34260");
        assert_eq!(ps.dict_get("bubbleRadius"), Some(&PyRep::Real(10_000.0)));
        assert_eq!(ps.dict_get("lifetimeSeconds"), Some(&PyRep::Real(180.0)));
    }

    #[tokio::test]
    async fn test_mjd_spool_and_warp_scramble_propulsion_shutoff() {
        // parity: feat:navigation.micro-jump-drive-spool, feat:navigation.warp-scramble-propulsion-shutoff
        eve_testkit::attest(&[
            "feat:navigation.micro-jump-drive-spool",
            "feat:navigation.warp-scramble-propulsion-shutoff",
        ]);

        let ctx = build_ctx();
        let kw = HashMap::new();
        let jita_sys: u32 = 30_000_142;
        let char_mjd: i64 = 94_031;
        let ship_mjd: i64 = 84_031;
        let char_scram: i64 = 94_032;
        let ship_scram: i64 = 84_032;

        let mut sess_mjd = seed_pilot(&ctx, char_mjd, ship_mjd, 16_231, jita_sys, false);
        let mut sess_scram = seed_pilot(&ctx, char_scram, ship_scram, 640, jita_sys, false);
        let sys = ctx.systems.get_or_spawn(jita_sys);

        // 1. Micro Jump Drive Spool-up, Turn During Spool, Velocity/Heading Preservation, & SetBallPosition Event
        set_skill_level(&ctx, char_mjd, 4_385, 0);
        let med_mjd_id: i64 = 75_001;
        give_item(&ctx, med_mjd_id, 33_915, char_mjd, ship_mjd, InventoryFlag::MedSlot0, 1);
        ctx.db.with_mut_snapshot(|snap| { snap.module_online_states.insert(med_mjd_id, true); });
        ctx.invalidate_ship_fitting(ship_mjd);

        let mjd_origin = DVec3::new(50_010_000_000.0, 0.0, 0.0);
        spawn_in_space(&ctx, char_mjd, ship_mjd, 16_231, jita_sys, mjd_origin).await;
        let b_mjd = ctx.bind_object(&mut sess_mjd, "beyonce", jita_sys as i64).unwrap();
        let init_cs = sys.get_combat_state(ship_mjd).await.unwrap().unwrap();
        let base_sig = init_cs.signature_radius;

        ctx.dispatch_rpc(
            &mut sess_mjd, "beyonce", Some(b_mjd), "CmdSetSpeedFraction",
            &[PyRep::Real(1.0)], &kw, None,
        ).await.unwrap();
        ctx.dispatch_rpc(
            &mut sess_mjd, "beyonce", Some(b_mjd), "CmdGotoDirection",
            &[PyRep::Real(1.0), PyRep::Real(0.0), PyRep::Real(0.0)], &kw, None,
        ).await.unwrap();
        rpc(&ctx, &mut sess_mjd, "dogmaIM", "Activate", vec![PyRep::Integer(med_mjd_id)])
            .await.expect("Activate Medium MJD");

        for _ in 0..6 {
            sys.step_manual(1.0).await.unwrap();
        }
        let (_, mid_ball) = sys.get_ball_snapshot(ship_mjd).await.unwrap().unwrap();
        let mid_cs = sys.get_combat_state(ship_mjd).await.unwrap().unwrap();
        assert!(mid_ball.position.distance(mjd_origin) < 5_000.0);
        assert!((mid_cs.signature_radius - base_sig * 2.5).abs() < 1.0);

        // Voluntary steering RPCs must be rejected mid-spool, while involuntary collision-bump deflection still steers the jump!
        let bumper_id: i64 = 84_099;
        let mut bumper_cs = eve_destiny::CombatState::new();
        bumper_cs.max_capacitor = 10_000.0;
        sys.spawn_ball_with_meta(
            Ball {
                id: bumper_id, type_id: 24690, position: mid_ball.position - DVec3::new(0.0, 200.0, 0.0),
                velocity: DVec3::new(0.0, 150.0, 0.0), max_velocity: 150.0, inertia: 1.0, mass: 500_000_000.0,
                radius: 250.0, speed_fraction: 1.0, mode: MovementMode::GotoDirection { direction: DVec3::Y },
            },
            bumper_cs,
            ctx.build_ship_slim_meta(char_scram, 24690),
        ).await.unwrap();
        for (m_name, m_args) in [
            ("CmdGotoDirection", vec![PyRep::Real(0.0), PyRep::Real(1.0), PyRep::Real(0.0)]),
            ("CmdSteerDirection", vec![PyRep::Real(0.0), PyRep::Real(1.0), PyRep::Real(0.0)]),
            ("CmdGotoPoint", vec![PyRep::Real(0.0), PyRep::Real(10_000.0), PyRep::Real(0.0)]),
            ("CmdAlignTo", vec![PyRep::Integer(bumper_id)]),
            ("CmdOrbit", vec![PyRep::Integer(bumper_id), PyRep::Real(2_500.0)]),
            ("CmdFollowBall", vec![PyRep::Integer(bumper_id), PyRep::Real(0.0)]),
        ] {
            let expected_err = "CantMoveWhileModuleActive";
            let steer_err = ctx.dispatch_rpc(&mut sess_mjd, "beyonce", Some(b_mjd), m_name, &m_args, &kw, None).await.unwrap_err();
            assert_eq!(steer_err.code, expected_err, "{m_name} must be rejected during MJD spool");
        }
        let mut saw_set_ball_pos = false;
        for _ in 0..6 {
            let tb = sys.step_manual(1.0).await.unwrap();
            if tb.events.iter().any(|e| matches!(e, DestinyEvent::SetBallPosition { ball_id, .. } if *ball_id == ship_mjd)) {
                saw_set_ball_pos = true;
            }
        }
        let _ = sys.remove_ball(bumper_id).await;
        assert!(saw_set_ball_pos, "Same-grid 100 km MJD jump must emit DestinyEvent::SetBallPosition");
        let (_, post_mjd_ball) = sys.get_ball_snapshot(ship_mjd).await.unwrap().unwrap();
        let jump_vec = post_mjd_ball.position - mid_ball.position;
        assert!((jump_vec.length() - 100_000.0).abs() < 3_500.0, "MJD must jump 100 km: got {}", jump_vec.length());
        assert!(post_mjd_ball.position.y > 50.0, "Bump-induced deflection into +Y must steer MJD jump into +Y: got {:?}", post_mjd_ball.position);
        assert!(jump_vec.normalize().dot(post_mjd_ball.velocity.normalize()) > 0.98, "MJD jump must align with current ship velocity heading");

        let mjd_react_err = rpc(&ctx, &mut sess_mjd, "dogmaIM", "Activate", vec![PyRep::Integer(med_mjd_id)]).await.unwrap_err();
        assert_eq!(mjd_react_err.code, "ModuleReactivationDelay");

        // Micro Jump Drive Operation L5 reduces spool time by 25% (12s -> 9s)
        let char_mjd_l5: i64 = 94_033;
        let ship_mjd_l5: i64 = 84_033;
        let med_mjd_l5_id: i64 = 75_002;
        let l5_origin = DVec3::new(50_020_000_000.0, 0.0, 0.0);
        let mut sess_mjd_l5 = seed_pilot(&ctx, char_mjd_l5, ship_mjd_l5, 16_231, jita_sys, false);
        set_skill_level(&ctx, char_mjd_l5, 4_385, 5);
        give_item(&ctx, med_mjd_l5_id, 33_915, char_mjd_l5, ship_mjd_l5, InventoryFlag::MedSlot0, 1);
        ctx.db.with_mut_snapshot(|snap| { snap.module_online_states.insert(med_mjd_l5_id, true); });
        ctx.invalidate_ship_fitting(ship_mjd_l5);
        spawn_in_space(&ctx, char_mjd_l5, ship_mjd_l5, 16_231, jita_sys, l5_origin).await;
        let b_mjd_l5 = ctx.bind_object(&mut sess_mjd_l5, "beyonce", jita_sys as i64).unwrap();
        ctx.dispatch_rpc(
            &mut sess_mjd_l5, "beyonce", Some(b_mjd_l5), "CmdGotoDirection",
            &[PyRep::Real(1.0), PyRep::Real(0.0), PyRep::Real(0.0)], &kw, None,
        ).await.unwrap();
        rpc(&ctx, &mut sess_mjd_l5, "dogmaIM", "Activate", vec![PyRep::Integer(med_mjd_l5_id)]).await.unwrap();
        for _ in 0..8 {
            sys.step_manual(1.0).await.unwrap();
        }
        let (_, pre9_ball) = sys.get_ball_snapshot(ship_mjd_l5).await.unwrap().unwrap();
        assert!(pre9_ball.position.distance(l5_origin) < 5_000.0);
        sys.step_manual(1.0).await.unwrap();
        let (_, l5_ball) = sys.get_ball_snapshot(ship_mjd_l5).await.unwrap().unwrap();
        assert!((l5_ball.position.x - (l5_origin.x + 100_000.0)).abs() < 5_000.0);

        // Capital Micro Jump Drive (83465) on Archon (23757) jumps 250 km
        let cap_mjd_id: i64 = 75_003;
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.retain(|_, i| i.location_id != ship_mjd);
            snap.items.get_mut(&ship_mjd).unwrap().type_id = 23_757;
        });
        give_item(&ctx, cap_mjd_id, 83_465, char_mjd, ship_mjd, InventoryFlag::MedSlot0, 1);
        ctx.db.with_mut_snapshot(|snap| { snap.module_online_states.insert(cap_mjd_id, true); });
        ctx.invalidate_ship_fitting(ship_mjd);
        let cap_origin = DVec3::new(50_030_000_000.0, 0.0, 0.0);
        spawn_in_space(&ctx, char_mjd, ship_mjd, 23_757, jita_sys, cap_origin).await;
        sys.set_ball_position(ship_mjd, cap_origin).await;
        ctx.dispatch_rpc(
            &mut sess_mjd, "beyonce", Some(b_mjd), "CmdSetSpeedFraction",
            &[PyRep::Real(1.0)], &kw, None,
        ).await.unwrap();
        ctx.dispatch_rpc(
            &mut sess_mjd, "beyonce", Some(b_mjd), "CmdGotoDirection",
            &[PyRep::Real(1.0), PyRep::Real(0.0), PyRep::Real(0.0)], &kw, None,
        ).await.unwrap();
        rpc(&ctx, &mut sess_mjd, "dogmaIM", "Activate", vec![PyRep::Integer(cap_mjd_id)]).await.unwrap();
        for _ in 0..12 {
            sys.step_manual(1.0).await.unwrap();
        }
        let (_, cap_ball) = sys.get_ball_snapshot(ship_mjd).await.unwrap().unwrap();
        assert!((cap_ball.position.x - (cap_origin.x + 250_000.0)).abs() < 5_000.0);

        // 2. Warp Scrambler Propulsion Shutoff vs Warp Disruptor, Bubble, WCS, & Afterburner (`feat:navigation.warp-scramble-propulsion-shutoff`)
        let mwd_id: i64 = 75_011;
        let ab_id: i64 = 75_012;
        let mid_spool_mjd_id: i64 = 75_013;
        let wcs_id: i64 = 75_014;
        let disruptor_id: i64 = 75_021;
        let scrambler_id: i64 = 75_022;

        ctx.db.with_mut_snapshot(|snap| {
            snap.items.retain(|_, i| i.location_id != ship_mjd && i.location_id != ship_scram);
            snap.items.get_mut(&ship_mjd).unwrap().type_id = 16_231;
            snap.items.get_mut(&ship_scram).unwrap().type_id = 640;
        });
        give_item(&ctx, mwd_id, 5_975, char_mjd, ship_mjd, InventoryFlag::MedSlot0, 1);
        give_item(&ctx, ab_id, 12_058, char_mjd, ship_mjd, InventoryFlag::MedSlot1, 1);
        give_item(&ctx, mid_spool_mjd_id, 33_915, char_mjd, ship_mjd, InventoryFlag::MedSlot2, 1);
        give_item(&ctx, wcs_id, 10_998, char_mjd, ship_mjd, InventoryFlag::LoSlot0, 1); // Warp Core Stabilizer I (+2 core strength)
        give_item(&ctx, disruptor_id, 3_242, char_scram, ship_scram, InventoryFlag::MedSlot0, 1);
        give_item(&ctx, scrambler_id, 448, char_scram, ship_scram, InventoryFlag::MedSlot1, 1);
        ctx.db.with_mut_snapshot(|snap| {
            for &mid in &[mwd_id, ab_id, mid_spool_mjd_id, wcs_id, disruptor_id, scrambler_id] {
                snap.module_online_states.insert(mid, true);
            }
        });
        ctx.invalidate_ship_fitting(ship_mjd);
        ctx.invalidate_ship_fitting(ship_scram);

        let scram_arena = DVec3::new(50_040_000_000.0, 0.0, 0.0);
        spawn_in_space(&ctx, char_mjd, ship_mjd, 16_231, jita_sys, scram_arena).await;
        spawn_in_space(&ctx, char_scram, ship_scram, 640, jita_sys, scram_arena + DVec3::new(3_000.0, 0.0, 0.0)).await;
        let (_, base_cyclone) = sys.get_ball_snapshot(ship_mjd).await.unwrap().unwrap();
        let base_vel = base_cyclone.max_velocity;

        sys.add_target_lock(ship_scram, eve_destiny::TargetLock {
            target_id: ship_mjd, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(100_000.0),
        }).await.unwrap();

        rpc(&ctx, &mut sess_mjd, "dogmaIM", "Activate", vec![PyRep::Integer(mwd_id)]).await.unwrap();
        let (_, mwd_on_ball) = sys.get_ball_snapshot(ship_mjd).await.unwrap().unwrap();
        assert!(mwd_on_ball.max_velocity > base_vel * 4.0);

        // 2a. Warp Disruptor keeps MWD active AND allows MJD activation, 12s spool (skill lvl 0), and 100 km jump
        rpc(&ctx, &mut sess_scram, "dogmaIM", "Activate", vec![
            PyRep::Integer(disruptor_id), PyRep::String("warpScramble".into()), PyRep::Integer(ship_mjd), PyRep::Integer(1),
        ]).await.unwrap();
        assert!(sys.is_ball_scrambled(ship_mjd).await.unwrap());
        let (_, under_disruptor_ball) = sys.get_ball_snapshot(ship_mjd).await.unwrap().unwrap();
        assert!(under_disruptor_ball.max_velocity > base_vel * 4.0);
        rpc(&ctx, &mut sess_mjd, "dogmaIM", "Deactivate", vec![PyRep::Integer(mwd_id)]).await.unwrap();
        rpc(&ctx, &mut sess_mjd, "dogmaIM", "Activate", vec![PyRep::Integer(mid_spool_mjd_id)]).await.unwrap();
        for _ in 0..13 {
            sys.set_capacitor(ship_mjd, 10_000.0).await.unwrap();
            sys.step_manual(1.0).await.unwrap();
        }
        let (_, disruptor_jumped_ball) = sys.get_ball_snapshot(ship_mjd).await.unwrap().unwrap();
        assert!(
            disruptor_jumped_ball.position.distance(scram_arena) > 95_000.0,
            "MJD must complete 100 km jump under Warp Disruptor: got {:?}",
            disruptor_jumped_ball.position
        );
        rpc(&ctx, &mut sess_scram, "dogmaIM", "Deactivate", vec![PyRep::Integer(disruptor_id)]).await.unwrap();
        ctx.advance_secs(200);

        // 2b. Interdiction bubble allows MJD activation, 12s spool, and 100 km jump out of bubble
        let bubble_pos = scram_arena + DVec3::new(200_000.0, 0.0, 0.0);
        sys.set_ball_position(ship_mjd, bubble_pos).await;
        sys.add_warp_bubble(980_101, bubble_pos, 20_000.0).await.unwrap();
        assert!(sys.is_ball_scrambled(ship_mjd).await.unwrap());
        rpc(&ctx, &mut sess_mjd, "dogmaIM", "Activate", vec![PyRep::Integer(mid_spool_mjd_id)]).await.unwrap();
        for _ in 0..13 {
            sys.set_capacitor(ship_mjd, 10_000.0).await.unwrap();
            sys.step_manual(1.0).await.unwrap();
        }
        let (_, bubble_jumped_ball) = sys.get_ball_snapshot(ship_mjd).await.unwrap().unwrap();
        assert!(
            bubble_jumped_ball.position.distance(bubble_pos) > 95_000.0,
            "MJD must complete 100 km jump inside an interdiction bubble: got {:?}",
            bubble_jumped_ball.position
        );
        sys.remove_warp_bubble(980_101).await.unwrap();

        // Reset ship back to scram_arena and verify Warp Scrambler shuts down MWD, preserves AB,
        // and blocks/aborts MJD even when a Warp Core Stabilizer (+2 core strength) is active!
        sys.set_ball_position(ship_mjd, scram_arena).await;
        sys.add_target_lock(ship_scram, eve_destiny::TargetLock {
            target_id: ship_mjd, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(100_000.0),
        }).await.unwrap();
        rpc(&ctx, &mut sess_mjd, "dogmaIM", "Activate", vec![PyRep::Integer(mwd_id)]).await.unwrap();
        rpc(&ctx, &mut sess_scram, "dogmaIM", "Activate", vec![
            PyRep::Integer(scrambler_id), PyRep::String("warpScramble".into()), PyRep::Integer(ship_mjd), PyRep::Integer(1),
        ]).await.unwrap();
        let (_, scrammed_ball) = sys.get_ball_snapshot(ship_mjd).await.unwrap().unwrap();
        assert!((scrammed_ball.max_velocity - base_vel).abs() < 1e-2);
        assert_eq!(rpc(&ctx, &mut sess_mjd, "dogmaIM", "Activate", vec![PyRep::Integer(mwd_id)]).await.unwrap_err().code, "WarpScrambled");

        ctx.db.with_mut_snapshot(|snap| {
            snap.module_online_states.insert(mwd_id, false);
            snap.module_online_states.insert(ab_id, true);
        });
        ctx.invalidate_ship_fitting(ship_mjd);
        rpc(&ctx, &mut sess_mjd, "dogmaIM", "Activate", vec![PyRep::Integer(ab_id)]).await.unwrap();
        sys.step_manual(2.0).await.unwrap();
        let (_, ab_scram_ball) = sys.get_ball_snapshot(ship_mjd).await.unwrap().unwrap();
        assert!(ab_scram_ball.max_velocity > base_vel * 1.8);
        rpc(&ctx, &mut sess_mjd, "dogmaIM", "Deactivate", vec![PyRep::Integer(ab_id)]).await.unwrap();
        rpc(&ctx, &mut sess_scram, "dogmaIM", "Deactivate", vec![PyRep::Integer(scrambler_id)]).await.unwrap();

        // Activate Warp Core Stabilizer I (+2 core strength -> total 3 > 2 scram points) and verify scrambler STILL blocks & aborts MJD!
        ctx.advance_secs(200);
        rpc(&ctx, &mut sess_mjd, "dogmaIM", "Activate", vec![PyRep::Integer(wcs_id)]).await.unwrap();
        rpc(&ctx, &mut sess_mjd, "dogmaIM", "Activate", vec![PyRep::Integer(mid_spool_mjd_id)]).await.unwrap();
        sys.step_manual(3.0).await.unwrap();
        assert_eq!(rpc(&ctx, &mut sess_mjd, "dogmaIM", "Deactivate", vec![PyRep::Integer(mid_spool_mjd_id)]).await.unwrap_err().code, "CantDeactivateWhileSpooling");
        let mjd_bid = ctx.bind_object(&mut sess_mjd, "beyonce", 30000142).unwrap();
        let empty_kw = std::collections::HashMap::new();
        assert_eq!(ctx.dispatch_rpc(&mut sess_mjd, "beyonce", Some(mjd_bid), "CmdStop", &[], &empty_kw, None).await.unwrap_err().code, "CantMoveWhileModuleActive");
        assert_eq!(ctx.dispatch_rpc(&mut sess_mjd, "beyonce", Some(mjd_bid), "CmdSetSpeedFraction", &[PyRep::Real(0.5)], &empty_kw, None).await.unwrap_err().code, "CantMoveWhileModuleActive");
        assert_eq!(ctx.dispatch_rpc(&mut sess_mjd, "beyonce", Some(mjd_bid), "CmdWarpToStuff", &[PyRep::String("item".into()), PyRep::Integer(50001248)], &empty_kw, None).await.unwrap_err().code, "CantMoveWhileModuleActive");
        rpc(&ctx, &mut sess_scram, "dogmaIM", "Activate", vec![
            PyRep::Integer(scrambler_id), PyRep::String("warpScramble".into()), PyRep::Integer(ship_mjd), PyRep::Integer(1),
        ]).await.unwrap();
        let cs_wcs_scram = sys.get_combat_state(ship_mjd).await.unwrap().unwrap();
        assert!(!cs_wcs_scram.is_warp_scrambled(), "WCS (+2) overcomes 2 scram points for warping");
        assert!(cs_wcs_scram.is_mwd_mjd_disabled, "Scrambler still sets is_mwd_mjd_disabled with WCS active");
        for _ in 0..10 {
            sys.step_manual(1.0).await.unwrap();
        }
        let (_, aborted_mjd_ball) = sys.get_ball_snapshot(ship_mjd).await.unwrap().unwrap();
        assert!(aborted_mjd_ball.position.distance(scram_arena) < 5_000.0);
        ctx.advance_secs(200);
        assert_eq!(
            rpc(&ctx, &mut sess_mjd, "dogmaIM", "Activate", vec![PyRep::Integer(mid_spool_mjd_id)]).await.unwrap_err().code,
            "WarpScrambled"
        );
    }

    #[test]
    fn test_warp_catch_bubble_350km_in_front_of_destination() {
        let origin = DVec3::ZERO;
        let dest = DVec3::new(10_000_000.0, 0.0, 0.0);
        let bubble = eve_destiny::WarpBubble { id: 99_001, position: DVec3::new(dest.x - 350_000.0, 0.0, 0.0), radius: 20_000.0 };
        let hit = bubble.intercept_warp(origin, dest).expect("Catch bubble 350 km in front of destination must intercept warp");
        assert!((hit.x - (dest.x - 370_000.0)).abs() < 1.0, "Expected entry at dest - 370 km, got {:?}", hit);
        let mid_route = eve_destiny::WarpBubble { id: 99_002, position: DVec3::new(dest.x - 600_000.0, 0.0, 0.0), radius: 20_000.0 };
        assert!(mid_route.intercept_warp(origin, dest).is_none(), "Bubble > 500 km mid-route must not intercept");
        let mut bp = eve_destiny::Ballpark::new(30000142);
        bp.add_ball(eve_destiny::Ball { id: 1, type_id: 587, position: origin, velocity: DVec3::ZERO, max_velocity: 365.0, inertia: 3.45, mass: 1_000_000.0, radius: 50.0, speed_fraction: 0.0, mode: eve_destiny::MovementMode::Stop });
        bp.warp_bubble_mgr.add_bubble(bubble);
        bp.set_mode(1, eve_destiny::MovementMode::new_warp(origin, dest, 3.0)).unwrap();
        bp.warp_bubble_mgr.remove_bubble(99_001);
        for _ in 0..20 { bp.step(1.0); }
        if let eve_destiny::MovementMode::Warp { destination, .. } = bp.balls[&1].mode {
            assert!((destination - dest).length() < 1.0, "Despawned bubble during alignment must not alter destination");
        }
    }
}

pub(crate) mod overheat_rack_heat_analytical_curves {
    // End-to-End Integration Tests closing Overheat Rack Heat and Probabilistic Heat Damage Propagation
    // parity rows (Part 3):
    // `feat:modules.overheat-rack-heat-generation` and `feat:modules.overheat-damage-propagation`.

    use crate::modules_advanced_tactical::*;
    use eve_db::InventoryFlag;
    use eve_destiny::{Ball, CombatState, DVec3, MovementMode};
    use eve_proto::PyRep;
    use std::collections::HashMap;

    fn set_ship_rack_heat(ctx: &eve_server::router::ServerContext, ship_id: i64, attr_id: u16, val: f64) {
        ctx.db.with_mut_snapshot(|snap| {
            let tr = snap.transient_item_attributes.entry(ship_id).or_default();
            if val > 0.0 {
                tr.insert(attr_id, val);
            } else {
                tr.remove(&attr_id);
            }
            if tr.is_empty() {
                snap.transient_item_attributes.remove(&ship_id);
            }
        });
    }

    #[tokio::test]
    async fn test_overheat_rack_heat_analytical_curves_and_overload_bonuses() {
        // parity: feat:modules.overheat-rack-heat-generation
        eve_testkit::attest(&["feat:modules.overheat-rack-heat-generation"]);

        let ctx = build_ctx();
        let jita_sys: u32 = 30_000_142;
        let char_a: i64 = 94_041;
        let ship_a: i64 = 84_041;
        let mut sess_a = seed_pilot(&ctx, char_a, ship_a, 24_690, jita_sys, false); // Hyperion (attr 1224 = 0.5)
        let sys = ctx.systems.get_or_spawn(jita_sys);

        let mod_0: i64 = 76_001;
        let mod_1: i64 = 76_002;
        let ab_id: i64 = 76_005;
        let scram_id: i64 = 76_008;
        let rep_id: i64 = 76_009;
        give_item(&ctx, mod_0, 3_170, char_a, ship_a, InventoryFlag::HiSlot0, 1); // Light Ion Blaster II (attr 1180 = 0.02)
        give_item(&ctx, mod_1, 3_170, char_a, ship_a, InventoryFlag::HiSlot1, 1);
        give_item(&ctx, 76_007, 222, char_a, ship_a, InventoryFlag::HiSlot0, 500);
        give_item(&ctx, 76_017, 222, char_a, ship_a, InventoryFlag::HiSlot1, 500);
        give_item(&ctx, ab_id, 12_058, char_a, ship_a, InventoryFlag::MedSlot0, 1); // 10MN AB II (+50% overload speed)
        give_item(&ctx, scram_id, 448, char_a, ship_a, InventoryFlag::MedSlot1, 1); // Warp Scrambler II (+20% overload range: 9km -> 10.8km)
        give_item(&ctx, rep_id, 3_530, char_a, ship_a, InventoryFlag::LoSlot0, 1); // Medium Armor Repairer II (+10% rep, -15% dur)
        ctx.db.with_mut_snapshot(|snap| {
            for &mid in &[mod_0, mod_1, ab_id, scram_id, rep_id] {
                snap.module_online_states.insert(mid, true);
            }
        });
        ctx.invalidate_ship_fitting(ship_a);

        let origin = DVec3::new(50_000_000_000.0, 0.0, 0.0);
        spawn_in_space(&ctx, char_a, ship_a, 24_690, jita_sys, origin).await;
        let target_id: i64 = 88_801;
        let mut t_cs = CombatState::default();
        t_cs.max_shield_hp = 500_000.0;
        t_cs.shield_hp = 500_000.0;
        sys.spawn_ball_with_combat(
            Ball {
                id: target_id, type_id: 640, position: origin + DVec3::new(1_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO, max_velocity: 100.0, inertia: 1.0, mass: 1e7, radius: 100.0,
                speed_fraction: 0.0, mode: MovementMode::Stop,
            },
            t_cs,
        ).await.unwrap();
        sys.add_target_lock(ship_a, eve_destiny::TargetLock {
            target_id, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(50_000.0),
        }).await.unwrap();

        // 1a. Analytical single-module heat curve on 1175 (`heatHi`): H(10) = 1 - exp(-10 * 0.5 * 0.02) = 1 - exp(-0.10)
        rpc(&ctx, &mut sess_a, "dogmaIM", "Overload", vec![
            PyRep::Integer(mod_0), PyRep::String("targetAttack".into()), PyRep::Integer(target_id), PyRep::Integer(1),
        ]).await.expect("Overload 1 blaster");
        for _ in 0..10 {
            sys.set_capacitor(ship_a, 10_000.0).await.unwrap();
            sys.step_manual(1.0).await.unwrap();
        }
        let h_1mod = attr_from_item_info(&rpc(&ctx, &mut sess_a, "dogmaIM", "ItemGetInfo", vec![PyRep::Integer(ship_a)]).await.unwrap(), 1175);
        let expected_1mod = 1.0 - (-0.10_f64).exp();
        assert!((h_1mod - expected_1mod).abs() < 1e-4, "1-module high rack heat (1175) must follow 1 - exp(-0.10): got {h_1mod} vs {expected_1mod}");
        assert!(
            ctx.db.with_snapshot(|s| {
                let sh = s.items.get(&ship_a).unwrap();
                !sh.dynamic_attributes.contains_key(&1175)
                    && !sh.dynamic_attributes.contains_key(&1176)
                    && !sh.dynamic_attributes.contains_key(&1177)
            }),
            "Transient ship rack heat (1175..=1177) must never be persisted to ship_item.dynamic_attributes"
        );

        // 1b. Exponential passive dissipation when overload is stopped: H_cool(10) = H_0 * exp(-10 * 0.01)
        rpc(&ctx, &mut sess_a, "dogmaIM", "StopOverload", vec![PyRep::Integer(mod_0)]).await.unwrap();
        rpc(&ctx, &mut sess_a, "dogmaIM", "Deactivate", vec![PyRep::Integer(mod_0)]).await.unwrap();
        for _ in 0..10 {
            sys.step_manual(1.0).await.unwrap();
        }
        let h_cooled = attr_from_item_info(&rpc(&ctx, &mut sess_a, "dogmaIM", "ItemGetInfo", vec![PyRep::Integer(ship_a)]).await.unwrap(), 1175);
        assert!((h_cooled - h_1mod * (-0.10_f64).exp()).abs() < 1e-4, "Passive cooling must follow H * exp(-t * diss): got {h_cooled}");

        // 1c. Linear 2-module sum (NOT N^2 * 2^N): 2 overloaded blasters -> gen_rate = 0.02 -> H(10) = 1 - exp(-0.20)
        set_ship_rack_heat(&ctx, ship_a, 1175, 0.0);
        set_item_dyn_attr(&ctx, mod_0, 3, 0.0);
        set_item_dyn_attr(&ctx, mod_1, 3, 0.0);
        for &mid in &[mod_0, mod_1] {
            rpc(&ctx, &mut sess_a, "dogmaIM", "Overload", vec![
                PyRep::Integer(mid), PyRep::String("targetAttack".into()), PyRep::Integer(target_id), PyRep::Integer(1),
            ]).await.unwrap();
        }
        for _ in 0..10 {
            sys.set_capacitor(ship_a, 10_000.0).await.unwrap();
            sys.step_manual(1.0).await.unwrap();
        }
        let h_2mod = attr_from_item_info(&rpc(&ctx, &mut sess_a, "dogmaIM", "ItemGetInfo", vec![PyRep::Integer(ship_a)]).await.unwrap(), 1175);
        let expected_2mod = 1.0 - (-0.20_f64).exp();
        assert!((h_2mod - expected_2mod).abs() < 1e-4, "2 overloaded modules must sum linearly to 1 - exp(-0.20): got {h_2mod} vs {expected_2mod}");
        for &mid in &[mod_0, mod_1] {
            rpc(&ctx, &mut sess_a, "dogmaIM", "Deactivate", vec![PyRep::Integer(mid)]).await.unwrap();
        }

        // 1d. Ship heatGenerationMultiplier (attr 1224): Rifter (587, 1224 = 1.0) heats at 2x rate of Hyperion (24690, 1224 = 0.5)
        let char_r: i64 = 94_042;
        let ship_r: i64 = 84_042;
        let mut sess_r = seed_pilot(&ctx, char_r, ship_r, 587, jita_sys, false);
        give_item(&ctx, 76_031, 3_170, char_r, ship_r, InventoryFlag::HiSlot0, 1);
        give_item(&ctx, 76_032, 222, char_r, ship_r, InventoryFlag::HiSlot0, 500);
        ctx.db.with_mut_snapshot(|s| { s.module_online_states.insert(76_031, true); });
        ctx.invalidate_ship_fitting(ship_r);
        spawn_in_space(&ctx, char_r, ship_r, 587, jita_sys, origin + DVec3::new(0.0, 500.0, 0.0)).await;
        sys.add_target_lock(ship_r, eve_destiny::TargetLock {
            target_id, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(50_000.0),
        }).await.unwrap();
        rpc(&ctx, &mut sess_r, "dogmaIM", "Overload", vec![
            PyRep::Integer(76_031), PyRep::String("targetAttack".into()), PyRep::Integer(target_id), PyRep::Integer(1),
        ]).await.unwrap();
        for _ in 0..10 {
            sys.set_capacitor(ship_r, 10_000.0).await.unwrap();
            sys.step_manual(1.0).await.unwrap();
        }
        let h_rifter = attr_from_item_info(&rpc(&ctx, &mut sess_r, "dogmaIM", "ItemGetInfo", vec![PyRep::Integer(ship_r)]).await.unwrap(), 1175);
        assert!((h_rifter - expected_2mod).abs() < 1e-4, "Rifter (1224=1.0) with 1 blaster must match 2x Hyperion (1224=0.5) rate: got {h_rifter}");
        rpc(&ctx, &mut sess_r, "dogmaIM", "Deactivate", vec![PyRep::Integer(76_031)]).await.unwrap();

        // 1e. SDE Overload bonuses: AB (+50% speed), Warp Scrambler II (+20% range at 10km), Armor Repairer (+10% rep / -15% dur), Weapon (-15% ROF)
        rpc(&ctx, &mut sess_a, "dogmaIM", "Activate", vec![PyRep::Integer(ab_id)]).await.unwrap();
        let (_, ab_norm) = sys.get_ball_snapshot(ship_a).await.unwrap().unwrap();
        rpc(&ctx, &mut sess_a, "dogmaIM", "Deactivate", vec![PyRep::Integer(ab_id)]).await.unwrap();
        rpc(&ctx, &mut sess_a, "dogmaIM", "Overload", vec![PyRep::Integer(ab_id)]).await.unwrap();
        let (_, ab_ovl) = sys.get_ball_snapshot(ship_a).await.unwrap().unwrap();
        assert!(ab_ovl.max_velocity > ab_norm.max_velocity * 1.15, "Overloaded AB must boost max_velocity: {} vs {}", ab_ovl.max_velocity, ab_norm.max_velocity);
        rpc(&ctx, &mut sess_a, "dogmaIM", "Deactivate", vec![PyRep::Integer(ab_id)]).await.unwrap();

        sys.set_ball_position(target_id, origin + DVec3::new(10_200.0, 0.0, 0.0)).await;
        let scram_cold_err = rpc(&ctx, &mut sess_a, "dogmaIM", "Activate", vec![
            PyRep::Integer(scram_id), PyRep::String("warpScramble".into()), PyRep::Integer(target_id), PyRep::Integer(0),
        ]).await.unwrap_err();
        assert_eq!(scram_cold_err.code, "TargetOutOfRange", "Unoverloaded Warp Scrambler II (9km) must fail at 10.2km");
        rpc(&ctx, &mut sess_a, "dogmaIM", "Overload", vec![
            PyRep::Integer(scram_id), PyRep::String("warpScramble".into()), PyRep::Integer(target_id), PyRep::Integer(0),
        ]).await.expect("Overloaded Warp Scrambler II (+20% -> 10.8km) must succeed at 10.2km");
        rpc(&ctx, &mut sess_a, "dogmaIM", "Deactivate", vec![PyRep::Integer(scram_id)]).await.unwrap();

        let fit_eval = ctx.get_or_rebuild_ship_fitting(char_a, ship_a).unwrap();
        let rep_norm = fit_eval.build_effective_module_state(rep_id, 3_530, false, &ctx.sde);
        let rep_ovl = fit_eval.build_effective_module_state(rep_id, 3_530, true, &ctx.sde);
        assert!((rep_ovl.get_attribute(84, &ctx.sde) - rep_norm.get_attribute(84, &ctx.sde) * 1.10).abs() < 1e-3);
        assert!((rep_ovl.get_attribute(73, &ctx.sde) - rep_norm.get_attribute(73, &ctx.sde) * 0.85).abs() < 1e-3);
        let gun_norm = fit_eval.build_effective_module_state(mod_0, 3_170, false, &ctx.sde);
        let gun_ovl = fit_eval.build_effective_module_state(mod_0, 3_170, true, &ctx.sde);
        assert!((gun_ovl.get_attribute(64, &ctx.sde) - gun_norm.get_attribute(64, &ctx.sde) * 1.15).abs() < 1e-3);

        // Mid-cycle UpdateModuleOverload on active Medium Armor Repairer II (`3530`):
        // Per EVE Overheating rules (`Overheating.md`), toggling overload mid-cycle takes effect on the NEXT cycle.
        let mut rep_cs = sys.get_combat_state(ship_a).await.unwrap().unwrap();
        rep_cs.max_armor_hp = 50_000.0;
        rep_cs.armor_hp = 1_000.0;
        sys.set_combat_state(ship_a, rep_cs).await.unwrap();
        rpc(&ctx, &mut sess_a, "dogmaIM", "Activate", vec![
            PyRep::Integer(rep_id), PyRep::String("armorRepair".into()), PyRep::None, PyRep::Integer(1),
        ]).await.unwrap();
        rpc(&ctx, &mut sess_a, "dogmaIM", "Overload", vec![PyRep::Integer(rep_id)]).await.unwrap();
        // Base duration is 12.0s; after 11s, the in-progress cycle must NOT have finished early.
        for _ in 0..11 {
            sys.set_capacitor(ship_a, 10_000.0).await.unwrap();
            sys.step_manual(1.0).await.unwrap();
        }
        let base_rep = rep_norm.get_attribute(84, &ctx.sde);
        let armor_at_11s = sys.get_combat_state(ship_a).await.unwrap().unwrap().armor_hp;
        assert!(
            (armor_at_11s - (1_000.0 + base_rep)).abs() < 1.0,
            "Mid-cycle Overload must complete in-progress cycle at base repair ({}): got {armor_at_11s}",
            1_000.0 + base_rep
        );
        // After +11s (22s total), the next cycle completes at overloaded duration and +10% armor repair!
        for _ in 0..11 {
            sys.set_capacitor(ship_a, 10_000.0).await.unwrap();
            sys.step_manual(1.0).await.unwrap();
        }
        let armor_after_next_ovl = sys.get_combat_state(ship_a).await.unwrap().unwrap().armor_hp;
        let expected_armor = 1_000.0 + base_rep + base_rep * 1.10;
        assert!(
            (armor_after_next_ovl - expected_armor).abs() < 1.0,
            "Next cycle after mid-cycle Overload must apply +10% amount (attr 1230) and -15% duration (attr 1206, 10.2s): got {armor_after_next_ovl} vs {expected_armor}"
        );
        rpc(&ctx, &mut sess_a, "dogmaIM", "Deactivate", vec![PyRep::Integer(rep_id)]).await.unwrap();
    }

    #[tokio::test]
    async fn test_probabilistic_heat_damage_propagation_and_nanite_paste() {
        // parity: feat:modules.overheat-damage-propagation
        eve_testkit::attest(&["feat:modules.overheat-damage-propagation"]);

        let ctx = build_ctx();
        let _kw: HashMap<String, PyRep> = HashMap::new();
        let jita_sys: u32 = 30_000_142;
        let char_a: i64 = 94_045;
        let ship_a: i64 = 84_045;
        let mut sess_a = seed_pilot(&ctx, char_a, ship_a, 24_690, jita_sys, false); // Hyperion (19 total hi+med+low slots)
        let sys = ctx.systems.get_or_spawn(jita_sys);

        set_skill_level(&ctx, char_a, 28_164, 0); // Thermodynamics L0
        let mod_0: i64 = 76_101;
        let mod_1: i64 = 76_102;
        let mod_2: i64 = 76_103;
        let mod_6: i64 = 76_104;
        let med_0: i64 = 76_105;
        let low_0: i64 = 76_106;
        give_item(&ctx, mod_0, 3_170, char_a, ship_a, InventoryFlag::HiSlot0, 1);
        give_item(&ctx, mod_1, 3_170, char_a, ship_a, InventoryFlag::HiSlot1, 1);
        give_item(&ctx, mod_2, 3_170, char_a, ship_a, InventoryFlag::HiSlot2, 1);
        give_item(&ctx, mod_6, 3_170, char_a, ship_a, InventoryFlag::HiSlot6, 1);
        give_item(&ctx, 76_107, 222, char_a, ship_a, InventoryFlag::HiSlot0, 2_000);
        give_item(&ctx, med_0, 12_058, char_a, ship_a, InventoryFlag::MedSlot0, 1);
        give_item(&ctx, low_0, 3_530, char_a, ship_a, InventoryFlag::LoSlot0, 1);
        // Fill remaining med & low slots so slot_ratio = 16 / 19 (~0.84) when online
        let mut filler_ids = Vec::new();
        for (i, flag) in [
            InventoryFlag::MedSlot1, InventoryFlag::MedSlot2, InventoryFlag::MedSlot3, InventoryFlag::MedSlot4,
            InventoryFlag::LoSlot1, InventoryFlag::LoSlot2, InventoryFlag::LoSlot3, InventoryFlag::LoSlot4,
            InventoryFlag::LoSlot5, InventoryFlag::LoSlot6,
        ].into_iter().enumerate() {
            let fid = 76_200 + i as i64;
            give_item(&ctx, fid, 1_319, char_a, ship_a, flag, 1);
            filler_ids.push(fid);
        }
        ctx.db.with_mut_snapshot(|snap| {
            for &mid in &[mod_0, mod_1, mod_2, mod_6, med_0, low_0] {
                snap.module_online_states.insert(mid, true);
            }
            for &fid in &filler_ids {
                snap.module_online_states.insert(fid, true);
            }
        });
        ctx.invalidate_ship_fitting(ship_a);

        let origin = DVec3::new(50_000_000_000.0, 0.0, 0.0);
        spawn_in_space(&ctx, char_a, ship_a, 24_690, jita_sys, origin).await;
        let target_id: i64 = 88_810;
        let mut t_cs = CombatState::default();
        t_cs.max_shield_hp = 5_000_000.0;
        t_cs.shield_hp = 5_000_000.0;
        sys.spawn_ball_with_combat(
            Ball {
                id: target_id, type_id: 640, position: origin + DVec3::new(800.0, 0.0, 0.0),
                velocity: DVec3::ZERO, max_velocity: 100.0, inertia: 1.0, mass: 1e7, radius: 100.0,
                speed_fraction: 0.0, mode: MovementMode::Stop,
            },
            t_cs,
        ).await.unwrap();
        sys.add_target_lock(ship_a, eve_destiny::TargetLock {
            target_id, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(50_000.0),
        }).await.unwrap();

        // 1. Seeded probabilistic heat damage: cold vs hot rack, non-wrapping distance attenuation, cross-rack isolation,
        // Thermodynamics L0 vs L5 (-25%), and Online Slots / Total Slots reduction (`feat:modules.overheat-damage-propagation`)
        ctx.set_mechanic_rng_seed(Some(2026));
        set_ship_rack_heat(&ctx, ship_a, 1175, 0.0);
        rpc(&ctx, &mut sess_a, "dogmaIM", "Overload", vec![
            PyRep::Integer(mod_0), PyRep::String("targetAttack".into()), PyRep::Integer(target_id), PyRep::Integer(1),
        ]).await.unwrap();
        for _ in 0..30 {
            sys.set_capacitor(ship_a, 10_000.0).await.unwrap();
            sys.step_manual(1.0).await.unwrap();
        }
        let dmg_cold = attr_from_item_info(&rpc(&ctx, &mut sess_a, "dogmaIM", "ItemGetInfo", vec![PyRep::Integer(mod_0)]).await.unwrap(), 3);
        rpc(&ctx, &mut sess_a, "dogmaIM", "Deactivate", vec![PyRep::Integer(mod_0)]).await.unwrap();

        // Hot rack (1175 = 0.85) under same seed 7 over 120s (~60 cycles on 400 HP cap): strictly probabilistic (0 < hits < 60),
        // monotonic attenuation d0 > d1 > d2 > d6, and zero cross-rack bleed!
        for &mid in &[mod_0, mod_1, mod_2, mod_6] {
            set_item_dyn_attr(&ctx, mid, 3, 0.0);
            set_item_dyn_attr(&ctx, mid, 40, 400.0);
        }
        set_ship_rack_heat(&ctx, ship_a, 1175, 0.85);
        ctx.set_mechanic_rng_seed(Some(7));
        rpc(&ctx, &mut sess_a, "dogmaIM", "Overload", vec![
            PyRep::Integer(mod_0), PyRep::String("targetAttack".into()), PyRep::Integer(target_id), PyRep::Integer(1),
        ]).await.unwrap();
        for _ in 0..120 {
            sys.set_capacitor(ship_a, 10_000.0).await.unwrap();
            sys.step_manual(1.0).await.unwrap();
        }
        rpc(&ctx, &mut sess_a, "dogmaIM", "Deactivate", vec![PyRep::Integer(mod_0)]).await.unwrap();
        let d0_l0 = attr_from_item_info(&rpc(&ctx, &mut sess_a, "dogmaIM", "ItemGetInfo", vec![PyRep::Integer(mod_0)]).await.unwrap(), 3);
        let d1_l0 = attr_from_item_info(&rpc(&ctx, &mut sess_a, "dogmaIM", "ItemGetInfo", vec![PyRep::Integer(mod_1)]).await.unwrap(), 3);
        let d2_l0 = attr_from_item_info(&rpc(&ctx, &mut sess_a, "dogmaIM", "ItemGetInfo", vec![PyRep::Integer(mod_2)]).await.unwrap(), 3);
        let d6_l0 = attr_from_item_info(&rpc(&ctx, &mut sess_a, "dogmaIM", "ItemGetInfo", vec![PyRep::Integer(mod_6)]).await.unwrap(), 3);
        let d_med = attr_from_item_info(&rpc(&ctx, &mut sess_a, "dogmaIM", "ItemGetInfo", vec![PyRep::Integer(med_0)]).await.unwrap(), 3);
        let d_low = attr_from_item_info(&rpc(&ctx, &mut sess_a, "dogmaIM", "ItemGetInfo", vec![PyRep::Integer(low_0)]).await.unwrap(), 3);

        assert!(d0_l0 > dmg_cold, "Hot rack (0.85) must produce more heat hits than cold rack: {d0_l0} vs {dmg_cold}");
        assert!(d0_l0 > 0.0 && d0_l0 < 60.0 * 0.8, "Heat hits must be probabilistic (0 < hits < N): d0={d0_l0}");
        assert!(d0_l0 > d1_l0 && d1_l0 > d2_l0 && d2_l0 > d6_l0, "Non-wrapping slot distance attenuation: d0={d0_l0}, d1={d1_l0}, d2={d2_l0}, d6={d6_l0}");
        assert_eq!(d_med, 0.0, "High rack heat must not bleed into MedSlot0");
        assert_eq!(d_low, 0.0, "High rack heat must not bleed into LoSlot0");

        // Thermodynamics L5 (-25% per-hit damage) under identical seed 7 -> exact 0.75x damage!
        set_skill_level(&ctx, char_a, 28_164, 5);
        for &mid in &[mod_0, mod_1, mod_2, mod_6] { set_item_dyn_attr(&ctx, mid, 3, 0.0); }
        set_ship_rack_heat(&ctx, ship_a, 1175, 0.85);
        ctx.set_mechanic_rng_seed(Some(7));
        rpc(&ctx, &mut sess_a, "dogmaIM", "Overload", vec![
            PyRep::Integer(mod_0), PyRep::String("targetAttack".into()), PyRep::Integer(target_id), PyRep::Integer(1),
        ]).await.unwrap();
        for _ in 0..120 {
            sys.set_capacitor(ship_a, 10_000.0).await.unwrap();
            sys.step_manual(1.0).await.unwrap();
        }
        rpc(&ctx, &mut sess_a, "dogmaIM", "Deactivate", vec![PyRep::Integer(mod_0)]).await.unwrap();
        let d0_l5 = attr_from_item_info(&rpc(&ctx, &mut sess_a, "dogmaIM", "ItemGetInfo", vec![PyRep::Integer(mod_0)]).await.unwrap(), 3);
        assert!((d0_l5 - d0_l0 * 0.75).abs() < 1e-4, "Thermodynamics L5 must scale heat damage by 0.75x: {d0_l5} vs {}", d0_l0 * 0.75);

        // Online Slots / Total Slots reduction: offlining 12 non-weapon modules drops slot_ratio from 16/19 to 4/19 -> fewer hits under seed 7!
        ctx.db.with_mut_snapshot(|snap| {
            for &fid in &filler_ids { snap.module_online_states.insert(fid, false); }
            snap.module_online_states.insert(med_0, false);
            snap.module_online_states.insert(low_0, false);
        });
        ctx.invalidate_ship_fitting(ship_a);
        for &mid in &[mod_0, mod_1, mod_2, mod_6] { set_item_dyn_attr(&ctx, mid, 3, 0.0); }
        set_ship_rack_heat(&ctx, ship_a, 1175, 0.85);
        ctx.set_mechanic_rng_seed(Some(7));
        rpc(&ctx, &mut sess_a, "dogmaIM", "Overload", vec![
            PyRep::Integer(mod_0), PyRep::String("targetAttack".into()), PyRep::Integer(target_id), PyRep::Integer(1),
        ]).await.unwrap();
        for _ in 0..120 {
            sys.set_capacitor(ship_a, 10_000.0).await.unwrap();
            sys.step_manual(1.0).await.unwrap();
        }
        rpc(&ctx, &mut sess_a, "dogmaIM", "Deactivate", vec![PyRep::Integer(mod_0)]).await.unwrap();
        let d0_sparse = attr_from_item_info(&rpc(&ctx, &mut sess_a, "dogmaIM", "ItemGetInfo", vec![PyRep::Integer(mod_0)]).await.unwrap(), 3);
        assert!(d0_sparse < d0_l5, "Offlining slots (lower Online/Total Slots) must reduce probabilistic heat hits: {d0_sparse} < {d0_l5}");

        // Analytical check of R-HEAT-03: total_slots includes rig slots (attr 1137), so 4 online hi+mid+low on a hull with rig slots
        // has slot_ratio = 4.0 / (hi + mid + low + rig).
        let thermo_eval = eve_server::services::tactical_module_mechanics::evaluate_ship_thermodynamics(
            ship_a,
            &Some(ctx.sde.clone()),
            &Some(ctx.db.clone()),
        );
        let sh_t = &ctx.sde.types[&ctx.db.with_snapshot(|s| s.items[&ship_a].type_id)];
        let expected_total_slots = sh_t.default_attributes.get(&14).copied().unwrap_or(0.0)
            + sh_t.default_attributes.get(&13).copied().unwrap_or(0.0)
            + sh_t.default_attributes.get(&12).copied().unwrap_or(0.0)
            + sh_t.default_attributes.get(&1137).copied().unwrap_or(0.0);
        assert!(
            (thermo_eval.slot_ratio - 4.0 / expected_total_slots).abs() < 1e-6,
            "slot_ratio must include rig slots (attr 1137) in denominator: got {} vs {}",
            thermo_eval.slot_ratio,
            4.0 / expected_total_slots
        );
    }

    #[tokio::test]
    async fn test_nanite_repair_and_overload_cross_module_lockout() {
        let ctx = build_ctx();
        let (char_id, ship_id, mod_a, mod_b) = (94_081, 84_081, 75_801, 75_802);
        let mut sess = seed_pilot(&ctx, char_id, ship_id, 24_690, 30_000_142, false);
        let sys = ctx.systems.get_or_spawn(30_000_142);
        sys.set_capacitor(ship_id, 10_000.0).await.unwrap();
        give_item(&ctx, mod_a, 12_058, char_id, ship_id, InventoryFlag::MedSlot0, 1);
        give_item(&ctx, mod_b, 3_530, char_id, ship_id, InventoryFlag::LoSlot0, 1);
        give_item(&ctx, 75_803, 28668, char_id, ship_id, InventoryFlag::Cargo, 50);
        ctx.db.with_mut_snapshot(|s| {
            s.skills.entry(char_id).or_default().insert(28164, eve_db::TrainedSkillRecord { type_id: 28164, level: 5, skill_points: 256_000.0 });
            s.items.get_mut(&mod_a).unwrap().dynamic_attributes.insert(3, 10.0);
        });
        rpc(&ctx, &mut sess, "dogmaIM", "SetModuleOnline", vec![PyRep::Integer(ship_id), PyRep::Integer(mod_a)]).await.unwrap();
        rpc(&ctx, &mut sess, "dogmaIM", "SetModuleOnline", vec![PyRep::Integer(ship_id), PyRep::Integer(mod_b)]).await.unwrap();
        // Direction 1: Active nanite repair on mod_a blocks overloading mod_b
        rpc(&ctx, &mut sess, "dogmaIM", "RepairModule", vec![PyRep::Integer(mod_a)]).await.unwrap();
        let err1 = rpc(&ctx, &mut sess, "dogmaIM", "Overload", vec![PyRep::Integer(mod_b)]).await.unwrap_err();
        assert_eq!(err1.code, "CannotOverheatWhileRepairing");
        for _ in 0..30 {
            sys.step_manual(1.0).await.unwrap();
        }
        // Direction 2: Active overloaded cycle on mod_b blocks initiating nanite repair on mod_a
        ctx.db.with_mut_snapshot(|s| {
            s.items.get_mut(&mod_a).unwrap().dynamic_attributes.insert(3, 10.0);
        });
        rpc(&ctx, &mut sess, "dogmaIM", "Overload", vec![PyRep::Integer(mod_b)]).await.unwrap();
        let err2 = rpc(&ctx, &mut sess, "dogmaIM", "RepairModule", vec![PyRep::Integer(mod_a)]).await.unwrap_err();
        assert_eq!(err2.code, "CannotRepairWhileOverloaded");
    }

    #[tokio::test]
    async fn test_interdiction_probe_excess_return_full_cargo_rejected() {
        let ctx = build_ctx();
        let sys_id = 30_004_608;
        let origin = DVec3::new(60_000_000_000.0, 0.0, 0.0);
        let (char_id, ship_id, launcher_id) = (94_082, 84_082, 75_811);
        let (char_b, ship_b) = (94_083, 84_083);
        let mut sess = seed_pilot(&ctx, char_id, ship_id, 22_456, sys_id, false);
        let _sess_b = seed_pilot(&ctx, char_b, ship_b, 587, sys_id, false);
        spawn_in_space(&ctx, char_id, ship_id, 22_456, sys_id, origin).await;
        spawn_in_space(&ctx, char_b, ship_b, 587, sys_id, origin + DVec3::new(2_000.0, 0.0, 0.0)).await;
        let sys = ctx.systems.get_or_spawn(sys_id);
        let (_, mut b_snap) = sys.get_ball_snapshot(ship_b).await.unwrap().unwrap();
        b_snap.velocity = DVec3::new(100.0, 0.0, 0.0);
        b_snap.mode = MovementMode::new_warp(
            origin + DVec3::new(2_000.0, 0.0, 0.0),
            origin + DVec3::new(20_000_000.0, 0.0, 0.0),
            3.0,
        );
        sys.spawn_ball_with_meta(
            b_snap,
            ctx.build_ship_combat_state(char_b, ship_b, 587),
            ctx.build_ship_slim_meta(char_b, 587),
        )
        .await
        .unwrap();
        let _ = sys.get_set_state(ship_b).await.unwrap();
        let _ = sys.drain_observer_updates(ship_b).await.unwrap();

        give_item(&ctx, launcher_id, 22_782, char_id, ship_id, InventoryFlag::HiSlot0, 1);
        give_item(&ctx, 75_812, 22_778, char_id, ship_id, InventoryFlag::HiSlot0, 4);
        give_item(&ctx, 75_813, 34, char_id, ship_id, InventoryFlag::Cargo, 100_000);
        ctx.invalidate_ship_fitting(ship_id);
        rpc(&ctx, &mut sess, "dogmaIM", "SetModuleOnline", vec![PyRep::Integer(ship_id), PyRep::Integer(launcher_id)]).await.unwrap();
        let err = rpc(&ctx, &mut sess, "dogmaIM", "Activate", vec![PyRep::Integer(launcher_id)]).await.unwrap_err();
        assert_eq!(err.code, "NotEnoughCargoSpace");

        let (_tick, obs_evs, _rep) = sys.drain_observer_updates(ship_b).await.unwrap();
        assert!(
            !obs_evs.iter().any(|e| matches!(e, eve_destiny::DestinyEvent::AddBalls2 { .. })),
            "Rejected probe launch must emit zero AddBalls2 events"
        );
        let (_, post_b) = sys.get_ball_snapshot(ship_b).await.unwrap().unwrap();
        assert!(
            matches!(post_b.mode, MovementMode::Warp { phase: eve_destiny::WarpPhase::Aligning, .. }),
            "Rejected probe launch must not disturb nearby Aligning ship: got {:?}",
            post_b.mode
        );
        assert!((post_b.velocity.x - 100.0).abs() < 1e-6);
    }

    #[tokio::test]
    async fn test_prealigned_warp_bubble_interception_no_overshoot() {
        let ctx = build_ctx();
        let sys_id = 30_004_608;
        let origin = DVec3::new(50_000_000_000.0, 0.0, 0.0);
        let (char_id, ship_id) = (94_084, 84_084);
        let _sess = seed_pilot(&ctx, char_id, ship_id, 11_198, sys_id, false);
        spawn_in_space(&ctx, char_id, ship_id, 11_198, sys_id, origin).await;
        let sys = ctx.systems.get_or_spawn(sys_id);
        let (_, mut ball) = sys.get_ball_snapshot(ship_id).await.unwrap().unwrap();
        ball.velocity = DVec3::new(ball.max_velocity * 0.80, 0.0, 0.0);
        ball.mode = MovementMode::new_warp(origin, origin + DVec3::new(200_000.0, 0.0, 0.0), 10.0);
        sys.spawn_ball_with_meta(
            ball,
            ctx.build_ship_combat_state(char_id, ship_id, 11_198),
            ctx.build_ship_slim_meta(char_id, 11_198),
        )
        .await
        .unwrap();
        sys.add_interdiction_sphere(999_001, origin + DVec3::new(100_000.0, 0.0, 0.0), 20_000.0)
            .await
            .unwrap();

        let _ = sys.step_manual(1.0).await.unwrap();
        let (_, post) = sys.get_ball_snapshot(ship_id).await.unwrap().unwrap();
        let edge_x = origin.x + 80_000.0;
        assert!(
            post.position.x <= edge_x + 1e-3,
            "Pre-aligned ship must not overshoot catch bubble edge ({edge_x}) on Tick 1: got {}",
            post.position.x
        );
        if let MovementMode::Warp { destination, .. } = post.mode {
            assert!((destination.x - edge_x).abs() < 1e-3);
        }
    }

    #[tokio::test]
    async fn test_reconnect_in_space_clears_emergency_warp_bubble_immunity() {
        let ctx = build_ctx();
        let sys_id = 30_004_608;
        let origin = DVec3::new(60_000_000_000.0, 0.0, 0.0);
        let (char_id, ship_id) = (94_085, 84_085);
        let _sess = seed_pilot(&ctx, char_id, ship_id, 587, sys_id, false);
        spawn_in_space(&ctx, char_id, ship_id, 587, sys_id, origin).await;
        let sys = ctx.systems.get_or_spawn(sys_id);
        sys.add_interdiction_sphere(999_002, origin, 25_000.0).await.unwrap();

        // Disconnect inside active bubble -> initiates emergency warp with is_emergency_warping = true
        let disc = eve_server::session_cluster_lifecycle::handle_character_logoff_or_disconnect(&ctx, char_id)
            .await
            .expect("disconnect entry");
        assert!(disc.emergency_warp_dest.is_some());
        assert!(sys.get_combat_state(ship_id).await.unwrap().unwrap().is_emergency_warping);

        // Reconnect immediately while still < 10 km from orig_pos -> must clear is_emergency_warping
        let reconn = eve_server::session_cluster_lifecycle::reconnect_character_in_space_sync(&ctx, char_id);
        assert!(reconn.is_some());
        assert!(
            !sys.get_combat_state(ship_id).await.unwrap().unwrap().is_emergency_warping,
            "reconnect_character_in_space_sync must unconditionally clear is_emergency_warping"
        );

        // Also verify Ballpark::set_mode clears is_emergency_warping on non-emergency mode transitions
        {
            let mut bp = sys.ballpark.write().unwrap();
            bp.combat_states.get_mut(&ship_id).unwrap().is_emergency_warping = true;
            bp.set_mode(ship_id, MovementMode::Stop).unwrap();
            assert!(!bp.combat_states[&ship_id].is_emergency_warping);
        }

        // Reconnected pilot inside active bubble must be blocked from initiating warp
        let warp_err = sys
            .set_movement_mode(
                ship_id,
                MovementMode::new_warp(origin, origin + DVec3::new(1_000_000.0, 0.0, 0.0), 5.0),
            )
            .await;
        assert!(warp_err.is_err(), "Reconnected pilot inside active warp bubble must not warp");
    }

    #[tokio::test]
    async fn test_high_velocity_subwarp_collision_no_tunneling() {
        let ctx = build_ctx();
        let sys_id = 30_000_142;
        let (char_id, ship_id) = (94_086, 84_086);
        let _sess = seed_pilot(&ctx, char_id, ship_id, 11_198, sys_id, false);
        spawn_in_space(&ctx, char_id, ship_id, 11_198, sys_id, DVec3::new(70_000_000_000.0, 0.0, 0.0)).await;
        let sys = ctx.systems.get_or_spawn(sys_id);

        let gate = ctx
            .sde
            .jumps
            .values()
            .find(|j| j.from_solar_system_id == sys_id)
            .expect("Jita stargate");
        let (_, gate_ball) = sys.get_ball_snapshot(gate.stargate_id).await.unwrap().unwrap();
        let mut cs = ctx.build_ship_combat_state(char_id, ship_id, 11_198);
        cs.undock_invulnerable_remaining_ticks = 0;

        // Place ship 500 m outside gate hull moving inward at 6,500 m/s (single-tick penetration > 5,000 m)
        sys.spawn_ball_with_meta(
            Ball {
                id: ship_id,
                type_id: 11_198,
                position: gate.position + DVec3::new(gate_ball.radius + 500.0, 0.0, 0.0),
                velocity: DVec3::new(-6_500.0, 0.0, 0.0),
                max_velocity: 6_500.0,
                inertia: 0.5,
                mass: 1_500_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            cs,
            ctx.build_ship_slim_meta(char_id, 11_198),
        )
        .await
        .unwrap();

        let tick = sys.step_manual(1.0).await.unwrap();
        let (_, post) = sys.get_ball_snapshot(ship_id).await.unwrap().unwrap();
        assert!(
            post.position.distance(gate.position) >= gate_ball.radius + post.radius - 1e-3,
            "High-velocity (> 6 km/s) ship must not tunnel through stargate hull"
        );
        assert!(post.velocity.x > 0.0, "High-velocity ship must bounce off stargate hull");
        assert!(tick.events.iter().any(|e| matches!(
            e,
            eve_destiny::DestinyEvent::Collision { ball_a, ball_b }
                if (*ball_a == ship_id && *ball_b == gate.stargate_id)
                    || (*ball_a == gate.stargate_id && *ball_b == ship_id)
        )));
    }

    #[tokio::test]
    async fn test_emergency_warp_stopped_by_warp_scrambler_during_alignment() {
        let ctx = build_ctx();
        let sys_id = 30_004_609;
        let origin = DVec3::new(65_000_000_000.0, 0.0, 0.0);
        let (char_id, ship_id) = (94_087, 84_087);
        let _sess = seed_pilot(&ctx, char_id, ship_id, 587, sys_id, false);
        spawn_in_space(&ctx, char_id, ship_id, 587, sys_id, origin).await;
        let sys = ctx.systems.get_or_spawn(sys_id);

        // Disconnect unpointed ship -> initiates emergency logoff warp in Aligning phase
        let disc = eve_server::session_cluster_lifecycle::handle_character_logoff_or_disconnect(&ctx, char_id)
            .await
            .expect("disconnect entry");
        assert!(disc.emergency_warp_dest.is_some());
        assert!(sys.get_combat_state(ship_id).await.unwrap().unwrap().is_emergency_warping);
        let (_, pre) = sys.get_ball_snapshot(ship_id).await.unwrap().unwrap();
        assert!(matches!(
            pre.mode,
            MovementMode::Warp {
                phase: eve_destiny::WarpPhase::Aligning,
                ..
            }
        ));

        // Apply a warp scrambler while the emergency warp is aligning -> must stop the warp
        sys.set_warp_scramble_strength(ship_id, 2).await.unwrap();
        let _ = sys.step_manual(1.0).await.unwrap();
        let (_, post) = sys.get_ball_snapshot(ship_id).await.unwrap().unwrap();
        assert!(
            matches!(post.mode, MovementMode::Stop),
            "Warp scrambler applied during emergency warp alignment must stop the ball: got {:?}",
            post.mode
        );
        assert!(
            !sys.get_combat_state(ship_id).await.unwrap().unwrap().is_emergency_warping,
            "Stopping emergency warp via scrambler must clear is_emergency_warping"
        );
    }
}

pub(crate) mod nosferatu_relative_cap_grappler_mutadaptive {
    // End-to-End Integration Tests closing Nosferatu, Grappler, Mutadaptive RAR, and Streaming Nanite Paste (Part 4):
    // `feat:modules.energy-nosferatu-relative-capacitor`, `feat:modules.stasis-grappler-mechanics`,
    // `feat:modules.mutadaptive-remote-armor-repairer`, and `feat:modules.nanite-repair-paste-in-space`.

    use crate::modules_advanced_tactical::*;
    use eve_db::{ContractRecord, InventoryFlag, IskCents};
    use eve_destiny::DVec3;
    use eve_proto::PyRep;
    use eve_server::services::combat_npc_police::scan_and_enforce_highsec_contraband;

    #[tokio::test]
    async fn test_nosferatu_relative_cap_grappler_mutadaptive_rar_and_blockade_runner() {
        // parity: feat:modules.energy-nosferatu-relative-capacitor, feat:modules.stasis-grappler-mechanics, feat:modules.mutadaptive-remote-armor-repairer
        eve_testkit::attest(&[
            "feat:modules.energy-nosferatu-relative-capacitor",
            "feat:modules.stasis-grappler-mechanics",
            "feat:modules.mutadaptive-remote-armor-repairer",
        ]);

        let ctx = build_ctx();
        let jita_sys: u32 = 30_000_142;
        let char_src: i64 = 94_051;
        let ship_src: i64 = 84_051;
        let char_tgt: i64 = 94_052;
        let ship_tgt: i64 = 84_052;
        let char_tgt2: i64 = 94_053;
        let ship_tgt2: i64 = 84_053;

        let mut sess_src = seed_pilot(&ctx, char_src, ship_src, 640, jita_sys, false);
        let _sess_tgt = seed_pilot(&ctx, char_tgt, ship_tgt, 24_690, jita_sys, false);
        let _sess_tgt2 = seed_pilot(&ctx, char_tgt2, ship_tgt2, 12_729, jita_sys, false); // Crane (Blockade Runner)
        let sys = ctx.systems.get_or_spawn(jita_sys);

        // 1. Energy Nosferatu Absolute GJ Rule & Blood Raider Override (`feat:modules.energy-nosferatu-relative-capacitor`)
        let nos_id: i64 = 77_001;
        give_item(&ctx, nos_id, 530, char_src, ship_src, InventoryFlag::HiSlot0, 1);
        ctx.db.with_mut_snapshot(|snap| { snap.module_online_states.insert(nos_id, true); });
        ctx.invalidate_ship_fitting(ship_src);

        let origin = DVec3::new(50_000_000_000.0, 0.0, 0.0);
        spawn_in_space(&ctx, char_src, ship_src, 640, jita_sys, origin).await;
        spawn_in_space(&ctx, char_tgt, ship_tgt, 24_690, jita_sys, origin + DVec3::new(1_500.0, 0.0, 0.0)).await;
        sys.add_target_lock(ship_src, eve_destiny::TargetLock {
            target_id: ship_tgt, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(100_000.0),
        }).await.unwrap();

        // 1a. Lower % (30% < 50%) but higher absolute GJ (Source 1500 / 5000 GJ >= Target 150 / 300 GJ): 0 GJ drained!
        let mut s_cs = sys.get_combat_state(ship_src).await.unwrap().unwrap();
        s_cs.capacitor_recharge_time_ms = 0.0;
        s_cs.max_capacitor = 5_000.0;
        s_cs.capacitor = 1_500.0;
        sys.set_combat_state(ship_src, s_cs.clone()).await.unwrap();
        let mut t_cs = sys.get_combat_state(ship_tgt).await.unwrap().unwrap();
        t_cs.capacitor_recharge_time_ms = 0.0;
        t_cs.max_capacitor = 300.0;
        t_cs.capacitor = 150.0;
        sys.set_combat_state(ship_tgt, t_cs.clone()).await.unwrap();

        rpc(&ctx, &mut sess_src, "dogmaIM", "Activate", vec![
            PyRep::Integer(nos_id), PyRep::String("energyNosferatuFalloff".into()), PyRep::Integer(ship_tgt), PyRep::Integer(0),
        ]).await.expect("Activate Nosferatu");
        for _ in 0..3 { sys.step_manual(1.0).await.unwrap(); }
        assert_eq!(sys.get_combat_state(ship_tgt).await.unwrap().unwrap().capacitor, 150.0);
        rpc(&ctx, &mut sess_src, "dogmaIM", "Deactivate", vec![PyRep::Integer(nos_id)]).await.unwrap();

        // 1b. Higher % (80% > 50%) but lower absolute GJ (Source 120 / 150 GJ < Target 2000 / 4000 GJ): drains!
        let mut s_cs2 = sys.get_combat_state(ship_src).await.unwrap().unwrap();
        s_cs2.capacitor_recharge_time_ms = 0.0;
        s_cs2.max_capacitor = 150.0;
        s_cs2.capacitor = 120.0;
        sys.set_combat_state(ship_src, s_cs2).await.unwrap();
        let mut t_cs2 = sys.get_combat_state(ship_tgt).await.unwrap().unwrap();
        t_cs2.capacitor_recharge_time_ms = 0.0;
        t_cs2.max_capacitor = 4_000.0;
        t_cs2.capacitor = 2_000.0;
        sys.set_combat_state(ship_tgt, t_cs2).await.unwrap();
        rpc(&ctx, &mut sess_src, "dogmaIM", "Activate", vec![
            PyRep::Integer(nos_id), PyRep::String("energyNosferatuFalloff".into()), PyRep::Integer(ship_tgt), PyRep::Integer(0),
        ]).await.unwrap();
        for _ in 0..3 { sys.step_manual(1.0).await.unwrap(); }
        assert!(sys.get_combat_state(ship_tgt).await.unwrap().unwrap().capacitor < 2_000.0);
        assert!(sys.get_combat_state(ship_src).await.unwrap().unwrap().capacitor > 120.0);
        rpc(&ctx, &mut sess_src, "dogmaIM", "Deactivate", vec![PyRep::Integer(nos_id)]).await.unwrap();

        // 1c. Blood Raider Bhaalgorn (17920) overrides absolute GJ limit (drains Target 150 GJ even with Source 1500 GJ)
        ctx.db.with_mut_snapshot(|snap| { snap.items.get_mut(&ship_src).unwrap().type_id = 17_920; });
        ctx.invalidate_ship_fitting(ship_src);
        sys.set_combat_state(ship_src, s_cs).await.unwrap();
        sys.set_combat_state(ship_tgt, t_cs).await.unwrap();
        rpc(&ctx, &mut sess_src, "dogmaIM", "Activate", vec![
            PyRep::Integer(nos_id), PyRep::String("energyNosferatuFalloff".into()), PyRep::Integer(ship_tgt), PyRep::Integer(0),
        ]).await.unwrap();
        for _ in 0..3 { sys.step_manual(1.0).await.unwrap(); }
        assert!(sys.get_combat_state(ship_tgt).await.unwrap().unwrap().capacitor < 150.0);
        rpc(&ctx, &mut sess_src, "dogmaIM", "Deactivate", vec![PyRep::Integer(nos_id)]).await.unwrap();

        // 2. Heavy Stasis Grappler Mechanics (`feat:modules.stasis-grappler-mechanics`)
        let mut cruiser_sess = seed_pilot(&ctx, 94_054, 84_054, 621, jita_sys, true);
        assert!(fit_mod(&ctx, &mut cruiser_sess, 84_054, 77_010, 41_040, 19).await.is_err());

        let mut bs_fit_sess = seed_pilot(&ctx, char_src, ship_src, 640, jita_sys, true);
        fit_mod(&ctx, &mut bs_fit_sess, ship_src, 77_011, 41_040, 19).await.expect("Battleship fits 1st Grappler");
        assert!(fit_mod(&ctx, &mut bs_fit_sess, ship_src, 77_012, 41_040, 20).await.is_err());
        fit_mod(&ctx, &mut bs_fit_sess, ship_src, 77_013, 527, 20).await.expect("Fit Stasis Webifier II");

        sess_src.station_id = None;
        sess_src.solar_system_id = Some(jita_sys);
        spawn_in_space(&ctx, char_src, ship_src, 640, jita_sys, origin).await;
        spawn_in_space(&ctx, char_tgt, ship_tgt, 24_690, jita_sys, origin + DVec3::new(500.0, 0.0, 0.0)).await;
        let (_, base_t_ball) = sys.get_ball_snapshot(ship_tgt).await.unwrap().unwrap();
        let base_t_vel = base_t_ball.max_velocity;
        sys.add_target_lock(ship_src, eve_destiny::TargetLock {
            target_id: ship_tgt, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(100_000.0),
        }).await.unwrap();

        rpc(&ctx, &mut sess_src, "dogmaIM", "Activate", vec![
            PyRep::Integer(77_011), PyRep::String("useMissiles".into()), PyRep::Integer(ship_tgt), PyRep::Integer(1),
        ]).await.unwrap();
        let (_, b_500) = sys.get_ball_snapshot(ship_tgt).await.unwrap().unwrap();
        assert!((b_500.max_velocity - base_t_vel * 0.20).abs() < 1.0);

        rpc(&ctx, &mut sess_src, "dogmaIM", "Activate", vec![
            PyRep::Integer(77_013), PyRep::String("useMissiles".into()), PyRep::Integer(ship_tgt), PyRep::Integer(1),
        ]).await.unwrap();
        let (_, b_stacked) = sys.get_ball_snapshot(ship_tgt).await.unwrap().unwrap();
        assert!(b_stacked.max_velocity > base_t_vel * 0.085 && b_stacked.max_velocity < base_t_vel * 0.11);
        rpc(&ctx, &mut sess_src, "dogmaIM", "Deactivate", vec![PyRep::Integer(77_013)]).await.unwrap();
        rpc(&ctx, &mut sess_src, "dogmaIM", "Deactivate", vec![PyRep::Integer(77_011)]).await.unwrap();

        sys.set_ball_position(ship_tgt, origin + DVec3::new(16_000.0, 0.0, 0.0)).await;
        rpc(&ctx, &mut sess_src, "dogmaIM", "Activate", vec![
            PyRep::Integer(77_011), PyRep::String("useMissiles".into()), PyRep::Integer(ship_tgt), PyRep::Integer(1),
        ]).await.unwrap();
        let (_, b_16k) = sys.get_ball_snapshot(ship_tgt).await.unwrap().unwrap();
        assert!(b_16k.max_velocity > base_t_vel * 0.90);
        rpc(&ctx, &mut sess_src, "dogmaIM", "Deactivate", vec![PyRep::Integer(77_011)]).await.unwrap();

        // Overloaded Grappler (`attr 1222` = +300% optimal -> 4,000m) MUST retain full -80% web across ticks at 3,500m!
        sys.set_ball_position(ship_tgt, origin + DVec3::new(3_500.0, 0.0, 0.0)).await;
        rpc(&ctx, &mut sess_src, "dogmaIM", "Activate", vec![
            PyRep::Integer(77_011), PyRep::String("useMissiles".into()), PyRep::Integer(ship_tgt), PyRep::Integer(1),
        ]).await.unwrap();
        sys.step_manual(1.0).await.unwrap();
        let (_, b_3500_norm) = sys.get_ball_snapshot(ship_tgt).await.unwrap().unwrap();
        assert!(b_3500_norm.max_velocity > base_t_vel * 0.23, "Cold grappler at 3.5km (>1km optimal) decays below full 80%");
        rpc(&ctx, &mut sess_src, "dogmaIM", "Overload", vec![PyRep::Integer(77_011)]).await.unwrap();
        sys.step_manual(1.0).await.unwrap();
        let (_, b_3500_ovl) = sys.get_ball_snapshot(ship_tgt).await.unwrap().unwrap();
        assert!(
            (b_3500_ovl.max_velocity - base_t_vel * 0.20).abs() < 1.0,
            "Overloaded Grappler (+300% optimal = 4km) must retain full 80% slow across ticks at 3.5km: got {}",
            b_3500_ovl.max_velocity
        );
        rpc(&ctx, &mut sess_src, "dogmaIM", "Deactivate", vec![PyRep::Integer(77_011)]).await.unwrap();

        // 3. Mutadaptive Remote Armor Repairer (`feat:modules.mutadaptive-remote-armor-repairer`): spool, +150% cap (2.5x), unlock & switch reset
        assert!(fit_mod(&ctx, &mut cruiser_sess, 84_054, 77_020, 49_770, 27).await.is_err());
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.retain(|_, i| i.location_id != ship_src);
            snap.module_online_states.clear();
            snap.module_active_states.clear();
        });
        let mut zarm_fit_sess = seed_pilot(&ctx, char_src, ship_src, 49_713, jita_sys, true);
        ctx.invalidate_ship_fitting(ship_src);
        fit_mod(&ctx, &mut zarm_fit_sess, ship_src, 77_021, 49_770, 27).await.expect("Zarmazd fits 1st Mutadaptive RAR");
        assert!(fit_mod(&ctx, &mut zarm_fit_sess, ship_src, 77_022, 49_770, 28).await.is_err());

        sess_src.station_id = None;
        sess_src.solar_system_id = Some(jita_sys);
        spawn_in_space(&ctx, char_src, ship_src, 49_713, jita_sys, origin).await;
        spawn_in_space(&ctx, char_tgt, ship_tgt, 24_690, jita_sys, origin + DVec3::new(3_000.0, 0.0, 0.0)).await;
        spawn_in_space(&ctx, char_tgt2, ship_tgt2, 12_729, jita_sys, origin + DVec3::new(4_000.0, 0.0, 0.0)).await;
        for &tid in &[ship_tgt, ship_tgt2] {
            let mut dmg_cs = sys.get_combat_state(tid).await.unwrap().unwrap();
            dmg_cs.max_armor_hp = 100_000.0;
            dmg_cs.armor_hp = 1_000.0;
            sys.set_combat_state(tid, dmg_cs).await.unwrap();
            sys.add_target_lock(ship_src, eve_destiny::TargetLock {
                target_id: tid, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(100_000.0),
            }).await.unwrap();
        }

        rpc(&ctx, &mut sess_src, "dogmaIM", "Activate", vec![
            PyRep::Integer(77_021), PyRep::String("remoteArmorRepairFalloff".into()), PyRep::Integer(ship_tgt), PyRep::Integer(1),
        ]).await.unwrap();
        for _ in 0..6 { sys.step_manual(1.0).await.unwrap(); }
        let a_c1 = sys.get_combat_state(ship_tgt).await.unwrap().unwrap().armor_hp;
        assert!((a_c1 - 1_784.0).abs() < 1.0);
        for _ in 0..6 { sys.step_manual(1.0).await.unwrap(); }
        let a_c2 = sys.get_combat_state(ship_tgt).await.unwrap().unwrap().armor_hp;
        assert!((a_c2 - (1_784.0 + 784.0 * 1.1)).abs() < 1.0);

        // Step 16 more cycles (total 18 > 15 maxspool) -> effective attr 84 caps at 784.0 * 2.5 = 1,960.0 HP (+150% cap)!
        for _ in 0..(16 * 6) {
            sys.set_capacitor(ship_src, 10_000.0).await.unwrap();
            sys.step_manual(1.0).await.unwrap();
        }
        let capped_rep = attr_from_item_info(&rpc(&ctx, &mut sess_src, "dogmaIM", "ItemGetInfo", vec![PyRep::Integer(77_021)]).await.unwrap(), 84);
        assert!((capped_rep - 784.0 * 2.5).abs() < 1e-2, "Mutadaptive RAR spool must cap at 2.5x (+150% = 1960.0): got {capped_rep}");

        // Unlocking target resets spool immediately back to 1.0x (784.0)
        rpc(&ctx, &mut sess_src, "dogmaIM", "RemoveTargets", vec![PyRep::List(vec![PyRep::Integer(ship_tgt)])]).await.unwrap();
        let rep_after_unlock = attr_from_item_info(&rpc(&ctx, &mut sess_src, "dogmaIM", "ItemGetInfo", vec![PyRep::Integer(77_021)]).await.unwrap(), 84);
        assert!((rep_after_unlock - 784.0).abs() < 1e-2);

        // Target switch resets spool back to 1.0x on new target
        sys.add_target_lock(ship_src, eve_destiny::TargetLock {
            target_id: ship_tgt, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(100_000.0),
        }).await.unwrap();
        rpc(&ctx, &mut sess_src, "dogmaIM", "Activate", vec![
            PyRep::Integer(77_021), PyRep::String("remoteArmorRepairFalloff".into()), PyRep::Integer(ship_tgt), PyRep::Integer(1),
        ]).await.unwrap();
        for _ in 0..12 { sys.set_capacitor(ship_src, 10_000.0).await.unwrap(); sys.step_manual(1.0).await.unwrap(); }
        rpc(&ctx, &mut sess_src, "dogmaIM", "Activate", vec![
            PyRep::Integer(77_021), PyRep::String("remoteArmorRepairFalloff".into()), PyRep::Integer(ship_tgt2), PyRep::Integer(1),
        ]).await.unwrap();
        for _ in 0..6 { sys.set_capacitor(ship_src, 10_000.0).await.unwrap(); sys.step_manual(1.0).await.unwrap(); }
        let a_tgt2_c1 = sys.get_combat_state(ship_tgt2).await.unwrap().unwrap().armor_hp;
        assert!((a_tgt2_c1 - 1_784.0).abs() < 1.0);
        rpc(&ctx, &mut sess_src, "dogmaIM", "Deactivate", vec![PyRep::Integer(77_021)]).await.unwrap();

        // 4. Blockade Runner Cargo Scan Immunity, Ship Scanner Visibility, & NPC Customs Contraband (pending V24 live message)
        let cargo_scanner_id: i64 = 77_031;
        let ship_scanner_id: i64 = 77_032;
        give_item(&ctx, cargo_scanner_id, 2_038, char_src, ship_src, InventoryFlag::MedSlot0, 1); // Cargo Scanner II
        give_item(&ctx, ship_scanner_id, 443, char_src, ship_src, InventoryFlag::MedSlot1, 1); // Ship Scanner I
        give_item(&ctx, 77_035, 1_319, char_tgt2, ship_tgt2, InventoryFlag::LoSlot0, 1); // Fitted module on Crane
        ctx.db.with_mut_snapshot(|snap| {
            snap.module_online_states.insert(cargo_scanner_id, true);
            snap.module_online_states.insert(ship_scanner_id, true);
            snap.module_online_states.insert(77_035, true);
        });
        ctx.invalidate_ship_fitting(ship_src);
        ctx.invalidate_ship_fitting(ship_tgt2);

        // Crane (12729, Blockade Runner) carrying Tritanium, PLEX, and contraband Standard Crash Booster (9947)
        give_item(&ctx, 77_041, 34, char_tgt2, ship_tgt2, InventoryFlag::Cargo, 500);
        give_item(&ctx, 77_042, 44_992, char_tgt2, ship_tgt2, InventoryFlag::Cargo, 1);
        give_item(&ctx, 77_044, 9_947, char_tgt2, ship_tgt2, InventoryFlag::Cargo, 2);

        // 4a. Player Cargo Scanner II returns empty scan (immune)
        let scan_crane = rpc(&ctx, &mut sess_src, "dogmaIM", "Activate", vec![
            PyRep::Integer(cargo_scanner_id), PyRep::String("cargoScan".into()), PyRep::Integer(ship_tgt2), PyRep::Integer(1),
        ]).await.expect("Cargo Scan Crane Blockade Runner");
        assert_eq!(scan_crane.dict_get("cargoScanImmune"), Some(&PyRep::Bool(true)));
        assert_eq!(scan_crane.dict_get("totalScannedQuantity"), Some(&PyRep::Integer(0)));
        assert_eq!(scan_crane.dict_get("scannedItems"), Some(&PyRep::List(Vec::new())));

        // 4b. Player Ship Scanner I (group 48) is NOT blocked by cargoScanResistance -> reveals fitted modules!
        let ship_scan_crane = rpc(&ctx, &mut sess_src, "dogmaIM", "Activate", vec![
            PyRep::Integer(ship_scanner_id), PyRep::String("shipScan".into()), PyRep::Integer(ship_tgt2), PyRep::Integer(1),
        ]).await.expect("Ship Scan Crane Blockade Runner");
        let fitted_list = match ship_scan_crane.dict_get("fittedModules") {
            Some(PyRep::List(v)) => v.len(),
            _ => 0,
        };
        assert!(fitted_list >= 1, "Ship Scanner I must reveal fitted modules on Blockade Runner");

        // 4c. High-sec NPC customs contraband enforcement still detects, confiscates, and fines contraband inside Blockade Runner!
        let found_contraband = scan_and_enforce_highsec_contraband(&ctx, jita_sys, ship_tgt2);
        assert_eq!(found_contraband.len(), 1, "High-sec NPC customs must detect contraband inside Blockade Runner");
        assert_eq!(found_contraband[0].0, 9_947);
        assert!(!ctx.db.with_snapshot(|s| s.items.contains_key(&77_044)), "Contraband booster must be confiscated");

        // 4d. Bustard (12731, Deep Space Transport, cargoScanResistance = 0) exposes cargo manifest to player Cargo Scanner II
        ctx.db.with_mut_snapshot(|snap| { snap.items.get_mut(&ship_tgt).unwrap().type_id = 12_731; });
        give_item(&ctx, 77_043, 34, char_tgt, ship_tgt, InventoryFlag::Cargo, 500);
        let scan_bustard = rpc(&ctx, &mut sess_src, "dogmaIM", "Activate", vec![
            PyRep::Integer(cargo_scanner_id), PyRep::String("cargoScan".into()), PyRep::Integer(ship_tgt), PyRep::Integer(1),
        ]).await.expect("Scan Bustard DST");
        assert_eq!(scan_bustard.dict_get("cargoScanImmune"), None);
        assert_eq!(scan_bustard.dict_get("totalScannedQuantity"), Some(&PyRep::Integer(500)));
    }

    #[tokio::test]
    async fn test_nanite_repair_paste_streaming_and_cargo_refund_capacity() {
        // parity: feat:modules.nanite-repair-paste-in-space
        eve_testkit::attest(&["feat:modules.nanite-repair-paste-in-space"]);

        let ctx = build_ctx();
        let char_a: i64 = 94_161;
        let ship_a: i64 = 84_161;
        let mut sess_a = seed_pilot(&ctx, char_a, ship_a, 22_456, NULL_SYS, false); // Sabre (Interdictor, 180 m3 cargo)
        let sys = ctx.systems.get_or_spawn(NULL_SYS);
        let origin = DVec3::new(50_000_000_000.0, 0.0, 0.0);
        spawn_in_space(&ctx, char_a, ship_a, 22_456, NULL_SYS, origin).await;

        set_skill_level(&ctx, char_a, 28_164, 0); // Thermodynamics L0
        set_skill_level(&ctx, char_a, 28_879, 0); // Nanite Operation L0
        set_skill_level(&ctx, char_a, 28_880, 0); // Nanite Interfacing L0

        // 1. Streaming per-tick paste deduction + clean mid-repair halt when cargo paste is exhausted!
        // Mega Pulse Laser II (`3057`, SDE attr 1277 = 0.5 paste/HP) at 20.0 HP damage requires 10 paste.
        // Seed only 3 paste in cargo -> 1 consumed at t=0, 2 streamed over ticks, then repair halts cleanly at 3/10 (14.0 HP remaining)!
        let mod_stream: i64 = 77_201;
        let paste_stack: i64 = 77_202;
        give_item(&ctx, mod_stream, 3_057, char_a, ship_a, InventoryFlag::HiSlot1, 1);
        give_item(&ctx, paste_stack, 28_668, char_a, ship_a, InventoryFlag::Cargo, 3);
        set_item_dyn_attr(&ctx, mod_stream, 3, 20.0);

        let rep_start = rpc(&ctx, &mut sess_a, "dogmaIM", "InitiateModuleRepair", vec![PyRep::Integer(mod_stream)]).await.unwrap();
        assert_eq!(rep_start.dict_get("pasteConsumed"), Some(&PyRep::Integer(1)), "Only 1 paste unit deducted at t=0");
        assert_eq!(rep_start.dict_get("totalPasteRequired"), Some(&PyRep::Integer(10)));
        assert_eq!(ctx.db.with_snapshot(|s| s.items.get(&paste_stack).unwrap().quantity), 2, "2 paste units remain in cargo at t=0");

        for _ in 0..8 {
            sys.step_manual(1.0).await.unwrap();
        }
        let paste_left = ctx.db.with_snapshot(|s| s.items.get(&paste_stack).map(|i| i.quantity).unwrap_or(0));
        assert_eq!(paste_left, 0, "All 3 available paste units must be streamed and consumed");
        assert!(!sys.is_module_being_nanite_repaired_sync(ship_a, mod_stream), "Repair must halt cleanly when cargo paste is exhausted");
        let dmg_after_exhaust = ctx.db.with_snapshot(|s| s.items.get(&mod_stream).unwrap().dynamic_attributes.get(&3).copied().unwrap_or(0.0));
        assert!(
            (dmg_after_exhaust - 14.0).abs() < 0.05,
            "Exhausting 3 of 10 required paste must repair exactly 3/10 of 20 HP (14.0 HP left), got {dmg_after_exhaust}"
        );

        // 2. Cargo capacity guard on cancellation refund: filling cargo to 100% hull capacity before t=0 StopModuleRepair
        // must NOT overflow hull cargo volume!
        give_item(&ctx, 77_203, 28_668, char_a, ship_a, InventoryFlag::Cargo, 2);
        rpc(&ctx, &mut sess_a, "dogmaIM", "InitiateModuleRepair", vec![PyRep::Integer(mod_stream)]).await.unwrap();
        // Fill Sabre cargo (400 m3) completely: 1 unit of 28668 (0.01 m3) + 39,999 Tritanium (`34`, 0.01 m3) = 400.00 m3 (0.00 m3 free!)
        give_item(&ctx, 77_204, 34, char_a, ship_a, InventoryFlag::Cargo, 39_999);
        let stop_full = rpc(&ctx, &mut sess_a, "dogmaIM", "StopModuleRepair", vec![PyRep::Integer(mod_stream)]).await.unwrap();
        assert_eq!(stop_full.dict_get("pasteRefunded"), Some(&PyRep::Integer(0)), "Refund must cap to 0 when ship cargo has 0.00 m3 free");
        let cargo_used_m3: f64 = ctx.db.with_snapshot(|s| {
            s.items.values().filter(|i| i.location_id == ship_a && i.flag == InventoryFlag::Cargo).map(|i| {
                ctx.sde.types.get(&i.type_id).unwrap().volume * (i.quantity as f64)
            }).sum()
        });
        assert!(cargo_used_m3 <= 400.0 + 1e-6, "Cargo volume must never exceed Sabre 400 m3 capacity: got {cargo_used_m3}");
        ctx.db.with_mut_snapshot(|s| { s.items.remove(&77_204); });

        // 3. Interdiction Sphere Launcher (`BF-02`): excess probes above launcher capacity return to Cargo (never destroyed),
        // and contract-locked probes in Cargo are NOT auto-reloaded!
        let launcher_id: i64 = 77_210;
        let over_stack_id: i64 = 77_211;
        let locked_probe_id: i64 = 77_212;
        give_item(&ctx, launcher_id, 22_782, char_a, ship_a, InventoryFlag::HiSlot0, 1);
        give_item(&ctx, over_stack_id, 22_778, char_a, ship_a, InventoryFlag::HiSlot0, 5); // 5 loaded > max 3 (15 m3 / 5 m3)
        ctx.db.with_mut_snapshot(|s| { s.module_online_states.insert(launcher_id, true); });
        ctx.invalidate_ship_fitting(ship_a);

        rpc(&ctx, &mut sess_a, "dogmaIM", "Activate", vec![
            PyRep::Integer(launcher_id), PyRep::String("useMissiles".into()), PyRep::None, PyRep::Integer(0),
        ]).await.expect("Launch 1st probe with 5 loaded");
        let (rem_slot, returned_cargo) = ctx.db.with_snapshot(|s| {
            let slot_q = s.items.get(&over_stack_id).map(|i| i.quantity).unwrap_or(0);
            let cargo_q: i32 = s.items.values().filter(|i| i.location_id == ship_a && i.flag == InventoryFlag::Cargo && i.type_id == 22_778).map(|i| i.quantity).sum();
            (slot_q, cargo_q)
        });
        assert_eq!((rem_slot, returned_cargo), (2, 2), "Excess 2 probes above capacity 3 must return to Cargo (5 - 1 = 4 total preserved)");

        // Clear unlocked cargo probes, leave 1 in launcher, and lock a cargo stack in an Outstanding contract -> auto-reload must skip locked stack!
        ctx.db.with_mut_snapshot(|s| {
            s.items.retain(|_, i| !(i.location_id == ship_a && i.flag == InventoryFlag::Cargo && i.type_id == 22_778));
            s.items.get_mut(&over_stack_id).unwrap().quantity = 1;
        });
        give_item(&ctx, locked_probe_id, 22_778, char_a, ship_a, InventoryFlag::Cargo, 3);
        ctx.db.with_mut_snapshot(|s| {
            s.contracts.insert(990_001, ContractRecord {
                contract_id: 990_001, issuer_id: char_a, issuer_corp_id: 1_000_009, assignee_id: 0, acceptor_id: None,
                start_station_id: 60_003_760, end_station_id: 60_003_760, contract_type: "ItemExchange".into(),
                status: "Outstanding".into(), title: "Locked Probes".into(), for_corp: false, availability: "Public".into(),
                date_issued_unix: 1, expiration_unix: u64::MAX, num_days: 1,
                price: IskCents(100), reward: IskCents(0), collateral: IskCents(0), buyout: IskCents(0),
                current_bid: IskCents(0), current_bidder_id: None,
                item_ids: vec![locked_probe_id], requested_items: Vec::new(), bid_history: Vec::new(),
            });
        });
        for _ in 0..6 { sys.step_manual(1.0).await.unwrap(); }
        let launch_last = rpc(&ctx, &mut sess_a, "dogmaIM", "Activate", vec![
            PyRep::Integer(launcher_id), PyRep::String("useMissiles".into()), PyRep::None, PyRep::Integer(0),
        ]).await.unwrap();
        assert_eq!(launch_last.dict_get("reloading"), Some(&PyRep::Bool(false)), "Contract-locked cargo probes must not be auto-reloaded");
        assert_eq!(launch_last.dict_get("remainingInLauncher"), Some(&PyRep::Integer(0)));
        assert_eq!(ctx.db.with_snapshot(|s| s.items.get(&locked_probe_id).unwrap().quantity), 3);

        // 4. Scram-aborted MWD (`BF-04`): Warp Scrambler cleans up `snap.module_active_states` and emits `OnGodmaShipEffect` deactivation!
        let char_v: i64 = 94_162;
        let ship_v: i64 = 84_162;
        let mut sess_v = seed_pilot(&ctx, char_v, ship_v, 621, NULL_SYS, false);
        let mwd_id: i64 = 77_220;
        let scram_id: i64 = 77_221;
        give_item(&ctx, mwd_id, 5_973, char_v, ship_v, InventoryFlag::MedSlot0, 1); // 50MN MWD II
        give_item(&ctx, scram_id, 448, char_a, ship_a, InventoryFlag::MedSlot0, 1); // Warp Scrambler II
        ctx.db.with_mut_snapshot(|s| {
            s.module_online_states.insert(mwd_id, true);
            s.module_online_states.insert(scram_id, true);
        });
        ctx.invalidate_ship_fitting(ship_v);
        ctx.invalidate_ship_fitting(ship_a);
        spawn_in_space(&ctx, char_v, ship_v, 621, NULL_SYS, origin + DVec3::new(3_000.0, 0.0, 0.0)).await;
        sys.add_target_lock(ship_a, eve_destiny::TargetLock {
            target_id: ship_v, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(50_000.0),
        }).await.unwrap();
        rpc(&ctx, &mut sess_v, "dogmaIM", "Activate", vec![
            PyRep::Integer(mwd_id), PyRep::String("moduleBonusMicrowarpdrive".into()), PyRep::None, PyRep::Integer(1),
        ]).await.unwrap();
        assert!(ctx.db.with_snapshot(|s| s.module_active_states.contains(&mwd_id)));
        let _ = sess_v.drain_notifications();
        let _ = ctx.drain_notifications_for_char(char_v);

        rpc(&ctx, &mut sess_a, "dogmaIM", "Activate", vec![
            PyRep::Integer(scram_id), PyRep::String("warpScramblerMWD".into()), PyRep::Integer(ship_v), PyRep::Integer(1),
        ]).await.unwrap();
        assert!(
            !ctx.db.with_snapshot(|s| s.module_active_states.contains(&mwd_id)),
            "Warp Scrambler aborting MWD must remove MWD from snap.module_active_states"
        );
        let notifs_scram = ctx.drain_notifications_for_char(char_v);
        assert!(
            notifs_scram.iter().any(|n| n.event == "OnGodmaShipEffect" && n.args.first() == Some(&PyRep::Integer(mwd_id))),
            "Warp Scrambler aborting MWD must emit OnGodmaShipEffect deactivation notification to victim"
        );
    }

    #[tokio::test]
    async fn test_wubble_destroyed_during_arming_cancels_detonation() {
        let ctx = build_ctx();
        let null_sys: u32 = 30_004_608; // 6VDT-H (-0.70 null-sec)
        let sabre_char: i64 = 94_061;
        let sabre_id: i64 = 84_061;
        let sb_char: i64 = 94_062;
        let sb_ship_id: i64 = 84_062;

        let mut sabre_sess = seed_pilot(&ctx, sabre_char, sabre_id, 22_456, null_sys, false);
        let mut sb_sess = seed_pilot(&ctx, sb_char, sb_ship_id, 640, null_sys, false);
        give_item(&ctx, 77_201, 22_782, sabre_char, sabre_id, InventoryFlag::HiSlot0, 1);
        give_item(&ctx, 77_202, 56_748, sabre_char, sabre_id, InventoryFlag::HiSlot0, 2);
        for (slot_idx, flag) in [
            (0, InventoryFlag::HiSlot0),
            (1, InventoryFlag::HiSlot1),
            (2, InventoryFlag::HiSlot2),
            (3, InventoryFlag::HiSlot3),
        ] {
            give_item(&ctx, 77_210 + slot_idx, 3_995, sb_char, sb_ship_id, flag, 1); // Large EMP Smartbomb II
        }
        ctx.db.with_mut_snapshot(|snap| {
            snap.module_online_states.insert(77_201, true);
            for slot_idx in 0..4 {
                snap.module_online_states.insert(77_210 + slot_idx, true);
            }
        });
        ctx.invalidate_ship_fitting(sabre_id);
        ctx.invalidate_ship_fitting(sb_ship_id);

        let sys = ctx.systems.get_or_spawn(null_sys);
        let arena = DVec3::new(50_060_000_000.0, 0.0, 0.0);
        spawn_in_space(&ctx, sabre_char, sabre_id, 22_456, null_sys, arena).await;
        spawn_in_space(&ctx, sb_char, sb_ship_id, 640, null_sys, arena + DVec3::new(1_000.0, 0.0, 0.0)).await;
        let (_, pre_sabre) = sys.get_ball_snapshot(sabre_id).await.unwrap().unwrap();
        let base_vel = pre_sabre.max_velocity;

        let pw = rpc(&ctx, &mut sabre_sess, "dogmaIM", "Activate", vec![PyRep::Integer(77_201)])
            .await
            .expect("Launch Wubble 56748");
        let pw_id = pw.dict_get("probeID").and_then(|v| v.as_i64()).unwrap();

        // Advance 1.0s (< 2.0s arming window): probe is alive on grid
        sys.step_manual(1.0).await.unwrap();
        assert!(sys.get_ball_snapshot(pw_id).await.unwrap().is_some(), "Probe must still exist at t=1.0s while arming");

        // Fire 4x Large EMP Smartbomb II before 2.0s detonation -> destroys the 1,000 HP arming probe
        for slot_idx in 0..4 {
            rpc(
                &ctx,
                &mut sb_sess,
                "dogmaIM",
                "Activate",
                vec![PyRep::Integer(77_210 + slot_idx), PyRep::String("empWave".into()), PyRep::None, PyRep::Integer(1)],
            )
            .await
            .unwrap();
        }
        assert!(sys.get_ball_snapshot(pw_id).await.unwrap().is_none(), "Smartbomb AoE must destroy arming wubble probe");

        // Step well past the 2.0s detonation window: destroyed probe must NOT trigger a ghost web debuff
        for _ in 0..4 {
            sys.step_manual(1.0).await.unwrap();
        }
        let (_, post_sabre) = sys.get_ball_snapshot(sabre_id).await.unwrap().unwrap();
        assert!(
            (post_sabre.max_velocity - base_vel).abs() < 1e-2,
            "Destroyed arming wubble must NOT apply ghost velocity debuff: got {} vs {}",
            post_sabre.max_velocity,
            base_vel
        );
    }
}

pub(crate) mod interdiction_sphere_launcher_negative_states {
    // Negative integration tests for `R-INT-01` (Interdiction Sphere Launcher activation
    // rejections when docked, not in space, cloaked, tethered, or in active warp) and `R-MJD-02`
    // (`warpVectorMgr.UseVector` rejection during MJD spool + `Ballpark::micro_jump` /
    // `Ballpark::micro_jump_field` rejection during active warp).

    use crate::esi_market_industry_pi::make_structure;
    use crate::modules_advanced_tactical::*;
    use eve_db::InventoryFlag;
    use eve_destiny::{Ball, Ballpark, DVec3, MovementMode, WarpPhase};
    use eve_proto::PyRep;
    use std::sync::Arc;

    #[tokio::test]
    async fn test_interdiction_sphere_launcher_negative_states() {
        let ctx = Arc::new(build_ctx());
        let jita_sys: u32 = 30_004_759;
        let char_id: i64 = 94_201;
        let ship_id: i64 = 84_201;
        let launcher_id: i64 = 74_201;
        let probe_id: i64 = 74_202;

        // Sabre (22456, group 541 Interdictor) + Interdiction Sphere Launcher I (22782) + Warp Disrupt Probe (22778)
        let mut sess = seed_pilot(&ctx, char_id, ship_id, 22_456, jita_sys, false);
        give_item(&ctx, launcher_id, 22_782, char_id, ship_id, InventoryFlag::HiSlot0, 1);
        give_item(&ctx, probe_id, 22_778, char_id, ship_id, InventoryFlag::HiSlot0, 5);
        ctx.db.with_mut_snapshot(|snap| {
            snap.module_online_states.insert(launcher_id, true);
        });
        ctx.invalidate_ship_fitting(ship_id);

        let sys = ctx.systems.get_or_spawn(jita_sys);

        // 1. Docked -> ShipDocked (even if ball is present)
        let mut cs_init = ctx.build_ship_combat_state(char_id, ship_id, 22_456);
        cs_init.capacitor = 100_000.0;
        cs_init.max_capacitor = 100_000.0;
        sys.spawn_ball_with_meta(
            Ball {
                id: ship_id,
                type_id: 22_456,
                position: DVec3::new(50_000_000_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1_500_000.0,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            cs_init,
            ctx.build_ship_slim_meta(char_id, 22_456),
        )
        .await
        .unwrap();
        sess.station_id = Some(60_003_760);
        ctx.db.with_mut_snapshot(|s| {
            if let Some(c) = s.characters.get_mut(&char_id) {
                c.station_id = Some(60_003_760);
            }
        });
        let err_docked = rpc(&ctx, &mut sess, "dogmaIM", "Activate", vec![PyRep::Integer(launcher_id)])
            .await
            .unwrap_err();
        assert_eq!(err_docked.code, "ShipDocked");
        sess.station_id = None;
        ctx.db.with_mut_snapshot(|s| {
            if let Some(c) = s.characters.get_mut(&char_id) {
                c.station_id = None;
            }
        });

        // 2. Not in space -> ShipNotInSpace
        let _ = sys.remove_ball(ship_id).await;
        let fit = ctx.get_or_rebuild_ship_fitting(char_id, ship_id).unwrap();
        let err_no_ball = eve_server::services::tactical_module_mechanics::activate_interdiction_sphere_launcher(
            &ctx, &sys, jita_sys, char_id, ship_id, 22_456, launcher_id,
            fit.modules.get(&launcher_id).unwrap(), ctx.sde.types.get(&22_782).unwrap(), "online", None,
        )
        .await
        .unwrap_err();
        assert_eq!(err_no_ball.code, "ShipNotInSpace");

        // 3. Cloaked -> CannotActivateCloaked
        let mut cs_cloaked = ctx.build_ship_combat_state(char_id, ship_id, 22_456);
        cs_cloaked.capacitor = 100_000.0;
        cs_cloaked.max_capacitor = 100_000.0;
        cs_cloaked.is_cloaked = true;
        sys.spawn_ball_with_meta(
            Ball {
                id: ship_id,
                type_id: 22_456,
                position: DVec3::new(50_000_000_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1_500_000.0,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            cs_cloaked,
            ctx.build_ship_slim_meta(char_id, 22_456),
        )
        .await
        .unwrap();
        let err_cloaked = rpc(&ctx, &mut sess, "dogmaIM", "Activate", vec![PyRep::Integer(launcher_id)])
            .await
            .unwrap_err();
        assert_eq!(err_cloaked.code, "CannotActivateCloaked");

        // 4. Tethered -> CantActivateWhileTethered
        let _ = sys.remove_ball(ship_id).await;
        let mut cs_tethered = ctx.build_ship_combat_state(char_id, ship_id, 22_456);
        cs_tethered.capacitor = 100_000.0;
        cs_tethered.max_capacitor = 100_000.0;
        cs_tethered.is_tethered = true;
        sys.spawn_ball_with_meta(
            Ball {
                id: ship_id,
                type_id: 22_456,
                position: DVec3::new(50_000_000_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1_500_000.0,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            cs_tethered,
            ctx.build_ship_slim_meta(char_id, 22_456),
        )
        .await
        .unwrap();
        let err_tethered = rpc(&ctx, &mut sess, "dogmaIM", "Activate", vec![PyRep::Integer(launcher_id)])
            .await
            .unwrap_err();
        assert_eq!(err_tethered.code, "CantActivateWhileTethered");

        // 5. Active non-Aligning Warp -> CantActivateInWarp
        let _ = sys.remove_ball(ship_id).await;
        let mut cs_warping = ctx.build_ship_combat_state(char_id, ship_id, 22_456);
        cs_warping.capacitor = 100_000.0;
        cs_warping.max_capacitor = 100_000.0;
        sys.spawn_ball_with_meta(
            Ball {
                id: ship_id,
                type_id: 22_456,
                position: DVec3::new(50_000_000_000.0, 0.0, 0.0),
                velocity: DVec3::new(1_000_000.0, 0.0, 0.0),
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1_500_000.0,
                radius: 50.0,
                speed_fraction: 1.0,
                mode: MovementMode::Warp {
                    origin: DVec3::new(50_000_000_000.0, 0.0, 0.0),
                    destination: DVec3::new(100_000_000_000.0, 0.0, 0.0),
                    warp_speed_au_s: 3.0,
                    total_distance: 50_000_000_000.0,
                    elapsed_warp_time: 2.0,
                    phase: WarpPhase::Cruising,
                },
            },
            cs_warping,
            ctx.build_ship_slim_meta(char_id, 22_456),
        )
        .await
        .unwrap();
        let err_warping = rpc(&ctx, &mut sess, "dogmaIM", "Activate", vec![PyRep::Integer(launcher_id)])
            .await
            .unwrap_err();
        assert_eq!(err_warping.code, "CantActivateInWarp");
    }

    #[tokio::test]
    async fn test_warp_vector_rejected_during_mjd_spool() {
        let ctx = Arc::new(build_ctx());
        let jita_sys: u32 = 30_004_759;
        let char_id: i64 = 94_202;
        let ship_id: i64 = 84_202;
        let mjd_id: i64 = 74_211;
        let vector_id: i64 = 74_212;

        // Scorpion (640, Battleship) + Large Micro Jump Drive (4383) + Jovian Symbolic Navigator (77825)
        let mut sess = seed_pilot(&ctx, char_id, ship_id, 640, jita_sys, false);
        give_item(&ctx, mjd_id, 4_383, char_id, ship_id, InventoryFlag::MedSlot0, 1);
        give_item(&ctx, vector_id, 77_825, char_id, ship_id, InventoryFlag::Cargo, 2);
        ctx.db.with_mut_snapshot(|snap| {
            snap.module_online_states.insert(mjd_id, true);
        });
        ctx.invalidate_ship_fitting(ship_id);

        let sys = ctx.systems.get_or_spawn(jita_sys);
        let mut cs = ctx.build_ship_combat_state(char_id, ship_id, 640);
        cs.capacitor = 100_000.0;
        cs.max_capacitor = 100_000.0;
        sys.spawn_ball_with_meta(
            Ball {
                id: ship_id,
                type_id: 640,
                position: DVec3::new(100_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 150.0,
                inertia: 1.0,
                mass: 100_000_000.0,
                radius: 250.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            cs,
            ctx.build_ship_slim_meta(char_id, 640),
        )
        .await
        .unwrap();

        // Start MJD spool
        rpc(&ctx, &mut sess, "dogmaIM", "Activate", vec![PyRep::Integer(mjd_id)])
            .await
            .expect("Activate MJD spool");

        // warpVectorMgr.UseVector must fail during MJD spool
        let err_spool = rpc(&ctx, &mut sess, "warpVectorMgr", "UseVector", vec![PyRep::Integer(vector_id)])
            .await
            .unwrap_err();
        assert!(
            err_spool.code.contains("Mjd") || err_spool.message.to_lowercase().contains("micro jump"),
            "Expected MJD spool rejection, got: {err_spool:?}"
        );

        // Defense-in-depth: Ballpark::micro_jump and micro_jump_field reject active non-Aligning Warp
        let mut bp = Ballpark::new(jita_sys);
        bp.add_ball(Ball {
            id: ship_id,
            type_id: 640,
            position: DVec3::ZERO,
            velocity: DVec3::new(1_000_000.0, 0.0, 0.0),
            max_velocity: 150.0,
            inertia: 1.0,
            mass: 100_000_000.0,
            radius: 250.0,
            speed_fraction: 1.0,
            mode: MovementMode::Warp {
                origin: DVec3::ZERO,
                destination: DVec3::new(100_000_000_000.0, 0.0, 0.0),
                warp_speed_au_s: 3.0,
                total_distance: 100_000_000_000.0,
                elapsed_warp_time: 2.0,
                phase: WarpPhase::Cruising,
            },
        });
        assert!(bp.micro_jump(ship_id, 100_000.0).unwrap_err().contains("CantActivateInWarp"));
        assert!(bp.micro_jump_field(ship_id, 100_000.0, 6_000.0, 25).unwrap_err().contains("CantActivateInWarp"));
    }

    #[tokio::test]
    async fn test_warp_bubble_warp_entry_only_and_shuttle_emergency_exemptions() {
        let mut bp = Ballpark::new(30_000_142);
        let origin = DVec3::ZERO;
        let dest = DVec3::new(100_000_000.0, 0.0, 0.0);
        let bubble_center = DVec3::new(100_000_000.0, 0.0, 0.0);

        // (a) Bubble present at set_mode that despawns during Aligning must NOT trap the ship
        bp.add_ball(Ball {
            id: 1001, type_id: 587, position: origin, velocity: DVec3::ZERO,
            max_velocity: 365.0, inertia: 3.45, mass: 1_067_000.0, radius: 37.0,
            speed_fraction: 0.0, mode: MovementMode::Stop,
        });
        let bid: i64 = 5001;
        bp.warp_bubble_mgr.add_bubble(eve_destiny::WarpBubble::new(bid, bubble_center, 20_000.0));
        bp.set_mode(1001, MovementMode::new_warp(origin, dest, 3.0)).unwrap();
        if let MovementMode::Warp { destination, .. } = bp.balls[&1001].mode {
            assert_eq!(destination, dest, "set_mode must not prematurely mutate warp destination");
        } else {
            panic!("Expected Warp mode");
        }
        bp.warp_bubble_mgr.remove_bubble(bid);
        for _ in 0..10 {
            bp.step(1.0);
        }
        if let MovementMode::Warp { destination, .. } = bp.balls[&1001].mode {
            assert_eq!(destination, dest, "Despawned bubble during alignment must not trap ship");
        }

        // (b) is_emergency_warping disconnect warp is exempt from origin-bubble alignment cancellation,
        //     but is still intercepted by destination drag/catch bubbles.
        bp.warp_bubble_mgr.add_bubble(eve_destiny::WarpBubble::new(5002, origin, 20_000.0));
        bp.warp_bubble_mgr.add_bubble(eve_destiny::WarpBubble::new(5003, bubble_center, 20_000.0));
        let mut cs_emerg = eve_destiny::CombatState::default();
        cs_emerg.is_emergency_warping = true;
        bp.combat_states.insert(1002, cs_emerg);
        bp.add_ball(Ball {
            id: 1002, type_id: 587, position: origin, velocity: DVec3::ZERO,
            max_velocity: 365.0, inertia: 1.0, mass: 1_067_000.0, radius: 37.0,
            speed_fraction: 0.0, mode: MovementMode::new_warp(origin, dest, 3.0),
        });
        for _ in 0..5 { bp.step(1.0); }
        let MovementMode::Warp { destination, phase, .. } = bp.balls[&1002].mode else {
            panic!("Expected emergency warp to enter warp out of origin bubble");
        };
        assert_ne!(phase, eve_destiny::WarpPhase::Aligning, "Emergency warp must not be aborted by origin bubble");
        assert!((destination.x - 99_980_000.0).abs() < 1e-3, "Emergency warp must still be caught by destination bubble");

        // (c) Newly spawned group 31 shuttle warping toward a bubbled destination is exempt
        bp.ball_group_ids.insert(1003, 31);
        bp.add_ball(Ball {
            id: 1003, type_id: 672, position: origin, velocity: DVec3::ZERO,
            max_velocity: 500.0, inertia: 1.0, mass: 1_000_000.0, radius: 25.0,
            speed_fraction: 0.0, mode: MovementMode::new_warp(origin, dest, 5.0),
        });
        for _ in 0..5 {
            bp.step(1.0);
        }
        if let MovementMode::Warp { destination, .. } = bp.balls[&1003].mode {
            assert_eq!(destination, dest, "Group 31 shuttle must be exempt from warp bubbles");
        }
    }

    #[tokio::test]
    async fn test_mid_cycle_overload_and_burnout_ship_fitting_sync() {
        let ctx = Arc::new(build_ctx());
        let jita_sys: u32 = 30_004_759;
        let char_id: i64 = 95_011;
        let ship_id: i64 = 96_011;
        let mwd_id: i64 = 97_011;
        let mut sess = seed_pilot(&ctx, char_id, ship_id, 587, jita_sys, false);
        give_item(&ctx, mwd_id, 439, char_id, ship_id, InventoryFlag::MedSlot0, 1);
        ctx.db.with_mut_snapshot(|snap| {
            snap.module_online_states.insert(mwd_id, true);
        });
        ctx.invalidate_ship_fitting(ship_id);
        let sys = ctx.systems.get_or_spawn(jita_sys);
        let mut cs = ctx.build_ship_combat_state(char_id, ship_id, 587);
        cs.capacitor = 100_000.0;
        cs.max_capacitor = 100_000.0;
        sys.spawn_ball_with_meta(
            Ball {
                id: ship_id, type_id: 587, position: DVec3::new(50_000_000_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO, max_velocity: 365.0, inertia: 1.0, mass: 1_067_000.0,
                radius: 37.0, speed_fraction: 0.0, mode: MovementMode::Stop,
            },
            cs,
            ctx.build_ship_slim_meta(char_id, 587),
        ).await.unwrap();

        // Activate MWD (non-overloaded)
        rpc(&ctx, &mut sess, "dogmaIM", "Activate", vec![PyRep::Integer(mwd_id), PyRep::String("online".into())])
            .await
            .expect("Activate MWD");
        let fit_before = ctx.get_or_rebuild_ship_fitting(char_id, ship_id).unwrap();
        assert_eq!(fit_before.modules[&mwd_id].state, eve_dogma::ModuleState::Active);

        // Mid-cycle Overload must defer ShipFittingEngine state until cycle boundary
        rpc(&ctx, &mut sess, "dogmaIM", "Overload", vec![PyRep::Integer(mwd_id)])
            .await
            .expect("Overload mid-cycle");
        let fit_mid = ctx.get_or_rebuild_ship_fitting(char_id, ship_id).unwrap();
        assert_eq!(
            fit_mid.modules[&mwd_id].state,
            eve_dogma::ModuleState::Active,
            "Mid-cycle Overload must not immediately mutate cached ShipFittingEngine state"
        );

        // Advance past 10s MWD cycle boundary -> SyncOverloadState updates ShipFittingEngine to Overloaded
        for _ in 0..11 {
            sys.step_manual(1.0).await.unwrap();
        }
        let fit_after = ctx.get_or_rebuild_ship_fitting(char_id, ship_id).unwrap();
        assert_eq!(
            fit_after.modules[&mwd_id].state,
            eve_dogma::ModuleState::Overloaded,
            "Cycle boundary must transition cached ShipFittingEngine state to Overloaded"
        );
    }

    #[tokio::test]
    async fn test_warp_to_within_min_range_sub_150km_rejected() {
        let ctx = Arc::new(build_ctx());
        let jita_sys: u32 = 30_004_759;
        let char_id: i64 = 95_012;
        let ship_id: i64 = 96_012;
        let target_id: i64 = 1_000_000_098_012;
        let mut sess = seed_pilot(&ctx, char_id, ship_id, 587, jita_sys, false);
        let sys = ctx.systems.get_or_spawn(jita_sys);
        let origin = DVec3::new(50_000_000_000.0, 0.0, 0.0);
        let target_pos = DVec3::new(50_000_250_000.0, 0.0, 0.0); // 250 km away (Astrahus radius 47 km -> 203 km landing dist)

        let mut cs = ctx.build_ship_combat_state(char_id, ship_id, 587);
        cs.capacitor = 100_000.0;
        cs.max_capacitor = 100_000.0;
        sys.spawn_ball_with_meta(
            Ball {
                id: ship_id, type_id: 587, position: origin,
                velocity: DVec3::ZERO, max_velocity: 365.0, inertia: 1.0, mass: 1_067_000.0,
                radius: 37.0, speed_fraction: 0.0, mode: MovementMode::Stop,
            },
            cs,
            ctx.build_ship_slim_meta(char_id, 587),
        ).await.unwrap();
        ctx.db.with_mut_snapshot(|snap| {
            let mut st = make_structure(target_id, 35_832, 98_000_001, jita_sys, "Target Astrahus");
            st.position = target_pos.to_array();
            snap.structures.insert(target_id, st);
        });

        // "Warp to Within 100 km" on a 180 km target -> 80 km effective warp < 150 km -> must reject
        let err = rpc(
            &ctx,
            &mut sess,
            "beyonce",
            "CmdWarpToStuff",
            vec![
                PyRep::String("item".into()),
                PyRep::Integer(target_id),
                PyRep::Real(100_000.0),
            ],
        )
        .await
        .expect_err("Warp to Within 100 km on 180 km target must be rejected");
        assert_eq!(err.code, "WarpDistanceTooShort");

        // "Warp to Within 20 km" on a 180 km target -> 160 km effective warp >= 150 km -> must succeed
        rpc(
            &ctx,
            &mut sess,
            "beyonce",
            "CmdWarpToStuff",
            vec![
                PyRep::String("item".into()),
                PyRep::Integer(target_id),
                PyRep::Real(20_000.0),
            ],
        )
        .await
        .expect("Warp to Within 20 km on 180 km target must succeed");
    }

    #[tokio::test]
    async fn test_structure_docking_advances_waypoints_and_uses_sde_radius_fallback() {
        let ctx = Arc::new(build_ctx());
        let jita_sys: u32 = 30_004_759;
        let char_id: i64 = 95_013;
        let ship_id: i64 = 96_013;
        let structure_id: i64 = 1_000_000_099_013;
        let astrahus_type_id: u32 = 35_832;
        let sde_radius = ctx
            .sde
            .types
            .get(&astrahus_type_id)
            .map(|t| t.radius)
            .filter(|&r| r > 0.0)
            .expect("Astrahus SDE radius must be > 0");

        let mut sess = seed_pilot(&ctx, char_id, ship_id, 587, jita_sys, false);
        let corp_id = sess.corporation_id.unwrap_or(1_000_009);
        let structure_pos = DVec3::new(50_000_000_000.0, 0.0, 0.0);
        ctx.db.with_mut_snapshot(|snap| {
            let mut st = make_structure(structure_id, astrahus_type_id, corp_id, jita_sys, "Test Astrahus");
            st.position = structure_pos.to_array();
            snap.structures.insert(structure_id, st);
        });

        // Set waypoints [structure_id, 30_004_759]
        rpc(
            &ctx,
            &mut sess,
            "charMgr",
            "SaveCharacterSetting",
            vec![
                PyRep::String("waypoints".into()),
                PyRep::List(vec![
                    PyRep::Integer(structure_id),
                    PyRep::Integer(jita_sys as i64),
                ]),
            ],
        )
        .await
        .expect("SaveCharacterSetting waypoints");

        // Spawn ship at sde_radius + 2_000 m (outside 2_500 m if radius were 0.0, inside sde_radius + 2_500 m)
        // Intentionally do NOT spawn a Ballpark ball for structure_id to test SDE radius fallback.
        let sys = ctx.systems.get_or_spawn(jita_sys);
        let cs = ctx.build_ship_combat_state(char_id, ship_id, 587);
        sys.spawn_ball_with_meta(
            Ball {
                id: ship_id,
                type_id: 587,
                position: DVec3::new(structure_pos.x + sde_radius + 2_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 365.0,
                inertia: 1.0,
                mass: 1_067_000.0,
                radius: 37.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            cs,
            ctx.build_ship_slim_meta(char_id, 587),
        )
        .await
        .unwrap();

        rpc(
            &ctx,
            &mut sess,
            "structureDocking",
            "Dock",
            vec![PyRep::Integer(structure_id)],
        )
        .await
        .expect("structureDocking.Dock must succeed using SDE radius fallback");

        let settings = rpc(&ctx, &mut sess, "charMgr", "GetCharacterSettings", vec![])
            .await
            .expect("GetCharacterSettings after structureDocking.Dock");
        assert_eq!(
            settings.dict_get("waypoints"),
            Some(&PyRep::List(vec![PyRep::Integer(jita_sys as i64)])),
            "structureDocking.Dock must advance waypoints on arrival"
        );
    }

    #[tokio::test]
    async fn test_system_actor_post_entry_bubble_does_not_yank_active_warp() {
        let ctx = Arc::new(build_ctx());
        let jita_sys: u32 = 30_000_142;
        let char_id: i64 = 900_991;
        let ship_id: i64 = 100_991;
        let sys = ctx.systems.get_or_spawn(jita_sys);

        let origin = DVec3::ZERO;
        let dest = DVec3::new(149_597_870_700.0, 0.0, 0.0); // 1 AU
        let cs = ctx.build_ship_combat_state(char_id, ship_id, 587);
        sys.spawn_ball_with_meta(
            Ball {
                id: ship_id,
                type_id: 587,
                position: origin,
                velocity: DVec3::ZERO,
                max_velocity: 365.0,
                inertia: 1.0,
                mass: 1_067_000.0,
                radius: 37.0,
                speed_fraction: 0.0,
                mode: MovementMode::new_warp(origin, dest, 3.0),
            },
            cs,
            ctx.build_ship_slim_meta(char_id, 587),
        )
        .await
        .unwrap();

        // Step until ship transitions past WarpPhase::Aligning into active warp
        for _ in 0..5 {
            sys.step_manual(1.0).await.unwrap();
        }
        let (_, mid_ball) = sys.get_ball_snapshot(ship_id).await.unwrap().unwrap();
        assert!(
            matches!(mid_ball.mode, MovementMode::Warp { phase, .. } if phase != eve_destiny::WarpPhase::Aligning),
            "Ship should have exited Aligning into active warp, got {:?}",
            mid_ball.mode
        );

        // Launch a warp disruption bubble at the destination AFTER warp entry
        sys.add_warp_bubble(88_001, dest, 20_000.0).await.unwrap();

        // Step through remaining warp ticks; SystemActor must NOT yank ship out of active warp
        for _ in 0..45 {
            sys.step_manual(1.0).await.unwrap();
        }
        let (_, final_ball) = sys.get_ball_snapshot(ship_id).await.unwrap().unwrap();
        assert!(
            final_ball.position.distance(dest) <= 3_000.0,
            "Post-entry bubble must not yank active warp; ship landed at {:?} (dist {:.1} m from dest)",
            final_ball.position,
            final_ball.position.distance(dest)
        );
    }

    #[tokio::test]
    async fn test_warp_vector_rejects_mid_warp_and_abyssal_and_cmd_dock_checks_upwell_tackle() {
        let ctx = Arc::new(build_ctx());
        let jita_sys: u32 = 30_000_142;
        let char_id: i64 = 900_992;
        let ship_id: i64 = 100_992;
        let mut sess = seed_pilot(&ctx, char_id, ship_id, 587, jita_sys, false);
        let corp_id = sess.corporation_id.unwrap_or(1_000_009);
        let sys = ctx.systems.get_or_spawn(jita_sys);
        let cs = ctx.build_ship_combat_state(char_id, ship_id, 587);
        sys.spawn_ball_with_meta(
            Ball {
                id: ship_id,
                type_id: 587,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 365.0,
                inertia: 1.0,
                mass: 1_067_000.0,
                radius: 37.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            cs,
            ctx.build_ship_slim_meta(char_id, 587),
        )
        .await
        .unwrap();

        // Seed a Warp Vector Data item (77825, group 4599) in ship cargo
        let vec_item_id: i64 = 777_825;
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                vec_item_id,
                eve_db::InventoryItemRecord {
                    item_id: vec_item_id,
                    type_id: 77825,
                    owner_id: char_id,
                    location_id: ship_id,
                    flag: eve_db::InventoryFlag::Cargo,
                    quantity: 2,
                    singleton: false,
                    version: 1,
                    ..Default::default()
                },
            );
        });

        // 1. Mid-warp trajectory redirection via warpVectorMgr.UseVector must be rejected
        sys.set_movement_mode(
            ship_id,
            MovementMode::Warp {
                origin: DVec3::ZERO,
                destination: DVec3::new(10_000_000.0, 0.0, 0.0),
                warp_speed_au_s: 3.0,
                total_distance: 10_000_000.0,
                elapsed_warp_time: 1.0,
                phase: eve_destiny::WarpPhase::Cruising,
            },
        )
        .await
        .unwrap();
        let err_mid = rpc(&ctx, &mut sess, "warpVectorMgr", "UseVector", vec![PyRep::Integer(vec_item_id)])
            .await
            .unwrap_err();
        assert_eq!(err_mid.code, "ShipAlreadyInWarp");

        // 2. Abyssal Deadspace (32_000_001) must reject warpVectorMgr.UseVector
        sys.set_movement_mode(ship_id, MovementMode::Stop).await.unwrap();
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(c) = snap.characters.get_mut(&char_id) {
                c.solar_system_id = 32_000_001;
            }
        });
        sess.solar_system_id = Some(32_000_001);
        let err_abyss = rpc(&ctx, &mut sess, "warpVectorMgr", "UseVector", vec![PyRep::Integer(vec_item_id)])
            .await
            .unwrap_err();
        assert_eq!(err_abyss.code, "CannotWarpInAbyssalSpace");
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(c) = snap.characters.get_mut(&char_id) {
                c.solar_system_id = jita_sys;
            }
        });
        sess.solar_system_id = Some(jita_sys);

        // 3. beyonce.CmdDock must reject scrambled/bubbled ships at Upwell structures
        let structure_id: i64 = 1_000_000_999_002;
        let structure_pos = DVec3::new(50_000_000.0, 0.0, 0.0);
        ctx.db.with_mut_snapshot(|snap| {
            let mut st = make_structure(structure_id, 35832, corp_id, jita_sys, "Test Citadel");
            st.position = structure_pos.to_array();
            snap.structures.insert(structure_id, st);
        });
        sys.set_movement_mode(ship_id, MovementMode::Stop).await.unwrap();
        let _ = sys.set_ball_position(ship_id, structure_pos + DVec3::new(1_000.0, 0.0, 0.0)).await;
        sys.add_warp_bubble(88_002, structure_pos, 20_000.0).await.unwrap();

        let err_dock = rpc(&ctx, &mut sess, "beyonce", "CmdDock", vec![PyRep::Integer(structure_id)])
            .await
            .unwrap_err();
        assert_eq!(err_dock.code, "ShipWarpScrambled");
    }
}

pub(crate) mod mobile_cyno_beacon_covert_variant {
    // Batch 39 open `feat:` parity integration tests (part 1: Mobile Cyno Beacon, Mobile Micro Jump Unit, Mobile Observatory).

    use eve_db::{InventoryFlag, InventoryItemRecord, IskCents, TrainedSkillRecord};
    use eve_destiny::{Ball, DamageVector, MovementMode};
    use eve_machonet::SessionState;
    use eve_proto::PyRep;
    use eve_server::router::ServerContext;
    use eve_server::services::mobile_deployables::{
        SKILL_ANCHORING, TYPE_COVERT_MOBILE_CYNO_BEACON, TYPE_MOBILE_CYNO_BEACON,
        TYPE_MOBILE_MJU, TYPE_MOBILE_OBSERVATORY,
    };
    use glam::DVec3;
    use std::collections::HashMap;

    fn dict_beacon_tuple(rep: &PyRep, key: i64) -> Option<(i64, i64)> {
        match rep {
            PyRep::Dict(d) => d.iter().find(|(k, _)| k.as_i64() == Some(key)).and_then(|(_, v)| match v {
                PyRep::Tuple(t) if t.len() == 2 => Some((t[0].as_i64()?, t[1].as_i64()?)),
                _ => None,
            }),
            _ => None,
        }
    }

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
        sys_id: u32,
        pos: DVec3,
        corp_id: i64,
    ) -> SessionState {
        let mut sess = SessionState::new((900_000 + (char_id % 100_000)) as u64);
        sess.user_id = Some(char_id as u64);
        ctx.db.with_mut_snapshot(|s| {
            let mut ch = s.characters.get(&90_000_001).cloned().unwrap();
            ch.character_id = char_id;
            ch.name = format!("Pilot-{char_id}");
            ch.corporation_id = corp_id;
            ch.alliance_id = None;
            ch.solar_system_id = sys_id;
            ch.active_ship_id = ship_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.security_status = 0.0;
            ch.balance = IskCents::from_isk(500_000_000);
            s.characters.insert(char_id, ch);
            s.items.insert(ship_id, mk_item(ship_id, ship_type_id, char_id, sys_id as i64, InventoryFlag::None, 1, true));
            s.item_positions.insert(ship_id, [pos.x, pos.y, pos.z]);
        });
        sess.character_id = Some(char_id);
        sess.corporation_id = Some(corp_id);
        sess.solar_system_id = Some(sys_id);
        sess.ship_id = Some(ship_id);
        let ship_type = ctx.sde.types.get(&ship_type_id);
        let handle = ctx.systems.get_or_spawn(sys_id);
        let _ = handle
            .spawn_ball_with_combat(
                Ball {
                    id: ship_id, type_id: ship_type_id, position: pos, velocity: DVec3::ZERO,
                    max_velocity: 250.0, inertia: 0.5, mass: ship_type.map(|t| t.mass).unwrap_or(1_500_000.0),
                    radius: ship_type.map(|t| t.radius).unwrap_or(50.0), speed_fraction: 0.0, mode: MovementMode::Stop,
                },
                ctx.build_type_combat_state(ship_type_id),
            )
            .await;
        sess
    }

    #[tokio::test]
    async fn test_b39_mobile_cyno_beacon_and_covert_variant() {
        // parity: feat:structures.mobile-cyno-beacon
        eve_testkit::attest(&["feat:structures.mobile-cyno-beacon"]);
        let ctx = ServerContext::new();
        let kw = HashMap::new();
        let origin_sys = 30_000_014_u32; // Shamahi (2.10 LY from Asabona)
        let dest_sys = 30_000_012_u32;   // Asabona (low-sec)

        let (c1, s1) = (91_000_011_i64, 101_000_011_i64);
        let (c_carrier, s_carrier) = (91_000_012_i64, 101_000_012_i64);
        let (c_blops, s_blops) = (91_000_016_i64, 101_000_016_i64);
        let mut sess1 = setup_pilot(&ctx, c1, s1, 587, dest_sys, DVec3::new(500_000.0, 0.0, 0.0), 1_000_009).await;
        let mut sess_carrier = setup_pilot(&ctx, c_carrier, s_carrier, 23915, origin_sys, DVec3::new(500_000.0, 0.0, 0.0), 1_000_009).await;
        let mut sess_blops = setup_pilot(&ctx, c_blops, s_blops, 22428, origin_sys, DVec3::new(505_000.0, 0.0, 0.0), 1_000_009).await;

        let (beacon_id, cov_beacon_id) = (101_000_099_i64, 101_000_100_i64);
        ctx.db.with_mut_snapshot(|s| {
            s.items.insert(beacon_id, mk_item(beacon_id, TYPE_MOBILE_CYNO_BEACON, c1, s1, InventoryFlag::Cargo, 1, true));
            s.items.insert(cov_beacon_id, mk_item(cov_beacon_id, TYPE_COVERT_MOBILE_CYNO_BEACON, c1, s1, InventoryFlag::Cargo, 1, true));
            s.skills.entry(c1).or_default().insert(SKILL_ANCHORING, TrainedSkillRecord { type_id: SKILL_ANCHORING, level: 2, skill_points: 10_000.0 });
            for (cid, sid, iso_tid, iid) in [(c_carrier, s_carrier, 17888_u32, 101_000_098_i64), (c_blops, s_blops, 16274_u32, 101_000_108_i64)] {
                s.skills.entry(cid).or_default().insert(3456, TrainedSkillRecord { type_id: 3456, level: 5, skill_points: 256_000.0 });
                s.skills.entry(cid).or_default().insert(21610, TrainedSkillRecord { type_id: 21610, level: 4, skill_points: 90_510.0 });
                s.items.insert(iid, mk_item(iid, iso_tid, cid, sid, InventoryFlag::SpecializedFuelBay, 50_000, false));
            }
        });

        // 1. Negative cases: Anchoring II rejected & non-fleet pilot rejected
        let no_skill = ctx.dispatch_rpc(&mut sess1, "posMgr", None, "AnchorStructure", &[PyRep::Integer(beacon_id)], &kw, None).await.unwrap_err();
        assert_eq!(no_skill.code, "SkillMissing");
        ctx.db.with_mut_snapshot(|s| { s.skills.get_mut(&c1).unwrap().get_mut(&SKILL_ANCHORING).unwrap().level = 3; });

        let no_fleet = ctx.dispatch_rpc(&mut sess1, "posMgr", None, "AnchorStructure", &[PyRep::Integer(beacon_id)], &kw, None).await.unwrap_err();
        assert_eq!(no_fleet.code, "MustBeInFleet");

        for s in [&mut sess1, &mut sess_carrier, &mut sess_blops] {
            s.fleet_id = Some(777);
        }
        ctx.db.with_mut_snapshot(|s| {
            s.fleets.insert(777, eve_db::FleetRecord { fleet_id: 777, boss_id: c1, members: vec![c1, c_carrier, c_blops], ..Default::default() });
        });

        // 2. Deploy standard Mobile Cyno Beacon (57319): 1m spool + 60m active (despawnUnix - anchorUntilUnix == 3600), single-use
        let t0 = ctx.now_unix();
        let anchor_rep = ctx.dispatch_rpc(&mut sess1, "posMgr", None, "AnchorStructure", &[PyRep::Integer(beacon_id)], &kw, None).await.expect("Deploy Mobile Cyno Beacon");
        let despawn0 = kv_get(&anchor_rep, "despawnUnix").and_then(|v| v.as_i64()).unwrap();
        assert!(((t0 + 3_660) as i64..=(t0 + 3_662) as i64).contains(&despawn0));
        assert_eq!(ctx.dispatch_rpc(&mut sess1, "ship", None, "Scoop", &[PyRep::Integer(beacon_id)], &kw, None).await.unwrap_err().code, "CannotScoop");
        assert_eq!(ctx.dispatch_rpc(&mut sess1, "entity", None, "CmdUnanchor", &[PyRep::Integer(beacon_id)], &kw, None).await.unwrap_err().code, "CannotUnanchor");

        // During 1m spool: not counted on map.GetBeaconCount and jump fails
        let map_spool = ctx.dispatch_rpc(&mut sess1, "map", None, "GetBeaconCount", &[], &kw, None).await.unwrap();
        assert!(dict_beacon_tuple(&map_spool, dest_sys as i64).is_none());
        let j_args = [PyRep::Integer(c1), PyRep::Integer(dest_sys as i64), PyRep::Integer(beacon_id)];
        assert_eq!(ctx.dispatch_rpc(&mut sess_carrier, "beyonce", None, "CmdJumpToFleetDeployableBeacon", &j_args, &kw, None).await.unwrap_err().code, "NoActiveBeacon");

        // After 61s spool: counted on map, fleet carrier jumps, non-fleet carrier rejected
        ctx.advance_secs(61);
        let map_active = ctx.dispatch_rpc(&mut sess1, "map", None, "GetBeaconCount", &[], &kw, None).await.unwrap();
        assert_eq!(dict_beacon_tuple(&map_active, dest_sys as i64), Some((1, 0)));

        let mut sess_out = setup_pilot(&ctx, 91_000_013, 101_000_013, 23915, origin_sys, DVec3::new(500_000.0, 0.0, 0.0), 1_000_010).await;
        assert_eq!(ctx.dispatch_rpc(&mut sess_out, "beyonce", None, "CmdJumpToFleetDeployableBeacon", &j_args, &kw, None).await.unwrap_err().code, "NoActiveBeacon");
        ctx.dispatch_rpc(&mut sess_carrier, "beyonce", None, "CmdJumpToFleetDeployableBeacon", &j_args, &kw, None).await.expect("Fleet carrier jumps");

        // Early destruction via HP damage removes beacon and blocks subsequent jumps
        let dest_handle = ctx.systems.get_or_spawn(dest_sys);
        let norm_cs = dest_handle.get_combat_state(beacon_id).await.unwrap().unwrap();
        let _ = dest_handle.apply_damage(beacon_id, DamageVector { em: 200_000.0, thermal: 200_000.0, kinetic: 200_000.0, explosive: 200_000.0 }).await;
        assert!(dest_handle.get_ball_snapshot(beacon_id).await.unwrap().is_none());
        assert_eq!(ctx.dispatch_rpc(&mut sess_blops, "beyonce", None, "CmdJumpToFleetDeployableBeacon", &j_args, &kw, None).await.unwrap_err().code, "NoActiveBeacon");

        // 3. Deploy Covert Mobile Cyno Beacon (59630, Tech II -> Anchoring V + Cynosural Field Theory V): 1m spool + 30m active, lower HP, hidden from map, Black Ops only
        let _ = dest_handle.set_ball_position(s1, DVec3::new(900_000.0, 0.0, 0.0)).await;
        ctx.db.with_mut_snapshot(|s| {
            s.item_positions.insert(s1, [900_000.0, 0.0, 0.0]);
            let sk = s.skills.entry(c1).or_default();
            sk.insert(SKILL_ANCHORING, TrainedSkillRecord { type_id: SKILL_ANCHORING, level: 5, skill_points: 256_000.0 });
            sk.insert(21603, TrainedSkillRecord { type_id: 21603, level: 5, skill_points: 256_000.0 });
        });
        let t_cov = ctx.now_unix();
        let cov_rep = ctx.dispatch_rpc(&mut sess1, "posMgr", None, "AnchorStructure", &[PyRep::Integer(cov_beacon_id)], &kw, None).await.expect("Deploy Covert Mobile Cyno Beacon");
        let despawn_cov = kv_get(&cov_rep, "despawnUnix").and_then(|v| v.as_i64()).unwrap();
        assert!(((t_cov + 1_860) as i64..=(t_cov + 1_862) as i64).contains(&despawn_cov));

        let cov_cs = dest_handle.get_combat_state(cov_beacon_id).await.unwrap().unwrap();
        let norm_hp = norm_cs.max_shield_hp + norm_cs.max_armor_hp + norm_cs.max_structure_hp;
        let cov_hp = cov_cs.max_shield_hp + cov_cs.max_armor_hp + cov_cs.max_structure_hp;
        assert!(cov_hp < norm_hp, "Covert Mobile Cyno Beacon has lower total HP ({cov_hp} < {norm_hp})");

        ctx.advance_secs(61);
        let map_cov = ctx.dispatch_rpc(&mut sess1, "map", None, "GetBeaconCount", &[], &kw, None).await.unwrap();
        assert!(dict_beacon_tuple(&map_cov, dest_sys as i64).is_none(), "Covert Mobile Cyno Beacon is hidden from map");

        let mut sess_dread = setup_pilot(&ctx, 91_000_017, 101_000_017, 19720, origin_sys, DVec3::new(510_000.0, 0.0, 0.0), 1_000_009).await;
        sess_dread.fleet_id = Some(777);
        ctx.db.with_mut_snapshot(|s| {
            s.fleets.get_mut(&777).unwrap().members.push(91_000_017);
            s.skills.entry(91_000_017).or_default().insert(3456, TrainedSkillRecord { type_id: 3456, level: 5, skill_points: 256_000.0 });
        });
        let cov_args = [PyRep::Integer(c1), PyRep::Integer(dest_sys as i64), PyRep::Integer(cov_beacon_id)];
        assert_eq!(ctx.dispatch_rpc(&mut sess_dread, "beyonce", None, "CmdJumpToFleetDeployableBeacon", &cov_args, &kw, None).await.unwrap_err().code, "ShipCannotJumpToBeacon");
        ctx.dispatch_rpc(&mut sess_blops, "beyonce", None, "CmdJumpToFleetDeployableBeacon", &cov_args, &kw, None).await.expect("Black Ops jumps to Covert Mobile Cyno Beacon");
    }

    #[tokio::test]
    async fn test_b39_mobile_micro_jump_unit_v24_component() {
        // parity: feat:structures.mobile-micro-jump-unit
        eve_testkit::attest(&["feat:structures.mobile-micro-jump-unit"]);
        let ctx = ServerContext::new();
        let kw = HashMap::new();
        let sys_id = 30_000_012_u32;
        let (c1, s1) = (91_000_021_i64, 101_000_021_i64);
        let mut sess1 = setup_pilot(&ctx, c1, s1, 587, sys_id, DVec3::new(500_000.0, 0.0, 0.0), 1_000_009).await;
        let beyonce1 = ctx.bind_object(&mut sess1, "beyonce", sys_id as i64).unwrap();
        let handle = ctx.systems.get_or_spawn(sys_id);

        let mju_id = 101_000_097_i64;
        ctx.db.with_mut_snapshot(|s| {
            s.items.insert(mju_id, mk_item(mju_id, TYPE_MOBILE_MJU, c1, s1, InventoryFlag::Cargo, 1, true));
            s.skills.entry(c1).or_default().insert(SKILL_ANCHORING, TrainedSkillRecord { type_id: SKILL_ANCHORING, level: 1, skill_points: 1_000.0 });
        });

        let t0 = ctx.now_unix();
        let anchor_rep = ctx.dispatch_rpc(&mut sess1, "posMgr", None, "AnchorStructure", &[PyRep::Integer(mju_id)], &kw, None).await.expect("Deploy MMJU");
        let despawn_mju = kv_get(&anchor_rep, "despawnUnix").and_then(|v| v.as_i64()).unwrap();
        assert!(((t0 + 60 + 2 * 86_400) as i64..=(t0 + 62 + 2 * 86_400) as i64).contains(&despawn_mju));
        assert_eq!(ctx.dispatch_rpc(&mut sess1, "ship", None, "Scoop", &[PyRep::Integer(mju_id)], &kw, None).await.unwrap_err().code, "CannotScoop");
        assert_eq!(ctx.dispatch_rpc(&mut sess1, "entity", None, "CmdUnanchor", &[PyRep::Integer(mju_id)], &kw, None).await.unwrap_err().code, "CannotUnanchor");

        let mju_call = [PyRep::Integer(mju_id), PyRep::String("microJumpDriver".into()), PyRep::String("ActivateMicroJumpDrive".into())];
        ctx.advance_secs(30);
        assert_eq!(ctx.dispatch_rpc(&mut sess1, "beyonce", Some(beyonce1), "CallComponentFromClient", &mju_call, &kw, None).await.unwrap_err().code, "DeployableAnchoring");
        ctx.advance_secs(31);

        // Dynamic evaluated mass check: Charon (20185, base 960M kg < 1B) + online 25000mm Steel Plates I (40348, +80M kg -> 1.04B kg) rejected!
        let (c_charon, s_charon, plate_id) = (91_000_022_i64, 101_000_022_i64, 101_000_096_i64);
        let mut sess_charon = setup_pilot(&ctx, c_charon, s_charon, 20185, sys_id, DVec3::new(500_300.0, 0.0, 0.0), 1_000_009).await;
        let beyonce_charon = ctx.bind_object(&mut sess_charon, "beyonce", sys_id as i64).unwrap();
        ctx.db.with_mut_snapshot(|s| {
            s.items.insert(plate_id, mk_item(plate_id, 40348, c_charon, s_charon, InventoryFlag::LoSlot0, 1, true));
            s.module_online_states.insert(plate_id, true);
        });
        ctx.invalidate_ship_fitting(s_charon);
        assert_eq!(ctx.dispatch_rpc(&mut sess_charon, "beyonce", Some(beyonce_charon), "CallComponentFromClient", &mju_call, &kw, None).await.unwrap_err().code, "ShipTooHeavy");

        // Non-default facing directions (+Y on owner, -Z on hostile) + concurrent activation + 12s warm-up + zero use cooldown
        let (c_hostile, s_hostile) = (91_000_023_i64, 101_000_023_i64);
        let mut sess_hostile = setup_pilot(&ctx, c_hostile, s_hostile, 587, sys_id, DVec3::new(500_400.0, 0.0, 0.0), 1_000_011).await;
        let beyonce_hostile = ctx.bind_object(&mut sess_hostile, "beyonce", sys_id as i64).unwrap();

        ctx.dispatch_rpc(&mut sess1, "beyonce", Some(beyonce1), "CmdGotoDirection", &[PyRep::Real(0.0), PyRep::Real(1.0), PyRep::Real(0.0)], &kw, None).await.unwrap();
        ctx.dispatch_rpc(&mut sess_hostile, "beyonce", Some(beyonce_hostile), "CmdGotoDirection", &[PyRep::Real(0.0), PyRep::Real(0.0), PyRep::Real(-1.0)], &kw, None).await.unwrap();
        handle.step_manual(1.0).await.unwrap();

        let (_, s1_before) = handle.get_ball_snapshot(s1).await.unwrap().unwrap();
        let (_, sh_before) = handle.get_ball_snapshot(s_hostile).await.unwrap().unwrap();
        ctx.dispatch_rpc(&mut sess1, "beyonce", Some(beyonce1), "CallComponentFromClient", &mju_call, &kw, None).await.expect("Owner MMJU spool");
        ctx.dispatch_rpc(&mut sess_hostile, "beyonce", Some(beyonce_hostile), "CallComponentFromClient", &mju_call, &kw, None).await.expect("Hostile MMJU spool");

        ctx.advance_secs(10);
        handle.step_manual(0.5).await.unwrap();
        let (_, s1_mid) = handle.get_ball_snapshot(s1).await.unwrap().unwrap();
        assert!((s1_mid.position - s1_before.position).length() < 1_000.0);

        ctx.advance_secs(3);
        handle.step_manual(0.5).await.unwrap();
        let d1 = handle.get_ball_snapshot(s1).await.unwrap().unwrap().1.position - s1_before.position;
        let dh = handle.get_ball_snapshot(s_hostile).await.unwrap().unwrap().1.position - sh_before.position;
        assert!(d1.y > 99_000.0 && d1.x.abs() < 1_000.0, "Owner jumped 100 km along +Y: {d1:?}");
        assert!(dh.z < -99_000.0 && dh.x.abs() < 1_000.0, "Hostile jumped 100 km along -Z: {dh:?}");

        let mju_pos = eve_server::services::mobile_deployables::read_deployable_state(&ctx).deployables.get(&mju_id).unwrap().position;
        let _ = handle.set_ball_position(s1, DVec3::from_array(mju_pos)).await;
        ctx.dispatch_rpc(&mut sess1, "beyonce", Some(beyonce1), "CallComponentFromClient", &mju_call, &kw, None).await.expect("Zero cooldown reuse");
    }

    #[tokio::test]
    async fn test_b39_mobile_observatory_decloak_ping() {
        // parity: feat:structures.mobile-observatory-decloak-ping
        eve_testkit::attest(&["feat:structures.mobile-observatory-decloak-ping"]);
        let ctx = ServerContext::new();
        let kw = HashMap::new();
        let (hisec_sys, wh_sys, abyss_sys, pochven_sys, lowsec_sys) = (30_000_142_u32, 31_000_007_u32, 32_000_001_u32, 30_000_021_u32, 30_000_012_u32);
        let (c1, s1, obs_id) = (91_000_031_i64, 101_000_031_i64, 101_000_090_i64);
        let mut sess = setup_pilot(&ctx, c1, s1, 587, hisec_sys, DVec3::new(600_000.0, 0.0, 0.0), 1_000_009).await;

        ctx.db.with_mut_snapshot(|s| {
            s.skills.entry(c1).or_default().insert(SKILL_ANCHORING, TrainedSkillRecord { type_id: SKILL_ANCHORING, level: 2, skill_points: 10_000.0 });
            s.items.insert(obs_id, mk_item(obs_id, TYPE_MOBILE_OBSERVATORY, c1, s1, InventoryFlag::Cargo, 1, true));
        });

        // 1. Anchoring II rejected + high-sec, wormhole, Abyssal, Pochven, and deadspace rejected
        let sk_err = ctx.dispatch_rpc(&mut sess, "posMgr", None, "AnchorStructure", &[PyRep::Integer(obs_id)], &kw, None).await.unwrap_err();
        assert!(matches!(sk_err.code.as_str(), "SkillRequired" | "SkillMissing"));
        ctx.db.with_mut_snapshot(|s| { s.skills.get_mut(&c1).unwrap().get_mut(&SKILL_ANCHORING).unwrap().level = 3; });
        assert_eq!(ctx.dispatch_rpc(&mut sess, "posMgr", None, "AnchorStructure", &[PyRep::Integer(obs_id)], &kw, None).await.unwrap_err().code, "CannotDeployInHighSec");

        for (rsys, code) in [(wh_sys, "CannotDeployInWormhole"), (abyss_sys, "CannotDeployInAbyssalSpace"), (pochven_sys, "CannotDeployInPochven")] {
            ctx.db.with_mut_snapshot(|s| { s.characters.get_mut(&c1).unwrap().solar_system_id = rsys; s.items.get_mut(&s1).unwrap().location_id = rsys as i64; });
            let h = ctx.systems.get_or_spawn(rsys);
            let _ = h.spawn_ball(Ball { id: s1, type_id: 587, position: DVec3::ZERO, velocity: DVec3::ZERO, max_velocity: 250.0, inertia: 0.5, mass: 1_000_000.0, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop }).await;
            sess.solar_system_id = Some(rsys);
            assert_eq!(ctx.dispatch_rpc(&mut sess, "posMgr", None, "AnchorStructure", &[PyRep::Integer(obs_id)], &kw, None).await.unwrap_err().code, code);
            let _ = h.remove_ball(s1).await;
        }

        ctx.db.with_mut_snapshot(|s| { s.characters.get_mut(&c1).unwrap().solar_system_id = lowsec_sys; s.items.get_mut(&s1).unwrap().location_id = lowsec_sys as i64; });
        sess.solar_system_id = Some(lowsec_sys);
        let low_handle = ctx.systems.get_or_spawn(lowsec_sys);
        let _ = low_handle.spawn_ball_with_combat(
            Ball { id: s1, type_id: 587, position: DVec3::new(5_000_000_000.0, 0.0, 0.0), velocity: DVec3::ZERO, max_velocity: 250.0, inertia: 0.5, mass: 1_000_000.0, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop },
            ctx.build_type_combat_state(587),
        ).await;
        eve_server::services::pve_exploration::with_pve_exploration_reg(&ctx, |reg| {
            reg.sites_by_system.entry(lowsec_sys).or_default().push(eve_server::services::pve_exploration::PveSiteInstance { dungeon_id: 1234, position: [5_000_000_000.0, 0.0, 0.0], ..Default::default() });
        });
        assert_eq!(ctx.dispatch_rpc(&mut sess, "posMgr", None, "AnchorStructure", &[PyRep::Integer(obs_id)], &kw, None).await.unwrap_err().code, "CannotDeployInDeadspace");
        eve_server::services::pve_exploration::with_pve_exploration_reg(&ctx, |reg| { reg.sites_by_system.remove(&lowsec_sys); });

        // 2. Activate real Prototype Cloaking Device I (11370) on 12 ships outside star radius and step 901s so their 15m cloak stabilization expires
        let mut cloaked_sessions = Vec::new();
        for i in 0..12_i64 {
            let (cid, sid, mid) = (91_001_100 + i, 101_001_100 + i, 101_002_100 + i);
            let mut csess = setup_pilot(&ctx, cid, sid, 11188, lowsec_sys, DVec3::new(5_100_000_000.0 + (i as f64) * 20_000.0, 0.0, 0.0), 1_000_010).await;
            ctx.db.with_mut_snapshot(|s| {
                s.items.insert(mid, mk_item(mid, 11370, cid, sid, InventoryFlag::HiSlot0, 1, true));
                s.module_online_states.insert(mid, true);
            });
            ctx.invalidate_ship_fitting(sid);
            ctx.dispatch_rpc(&mut csess, "dogmaIM", None, "Activate", &[PyRep::Integer(mid)], &kw, None).await.expect("Cloak ship");
            cloaked_sessions.push((cid, sid, mid, csess));
        }
        for _ in 0..16 {
            low_handle.step_manual(60.0).await.unwrap();
        }

        // Deploy Observatory: 10m deploy + 2h active, cannot scoop or unanchor
        let t_dep = ctx.now_unix();
        let dep_rep = ctx.dispatch_rpc(&mut sess, "posMgr", None, "AnchorStructure", &[PyRep::Integer(obs_id)], &kw, None).await.expect("Deploy Observatory");
        let despawn_obs = kv_get(&dep_rep, "despawnUnix").and_then(|v| v.as_i64()).unwrap();
        assert!(((t_dep + 7_800) as i64..=(t_dep + 7_802) as i64).contains(&despawn_obs));
        assert_eq!(ctx.dispatch_rpc(&mut sess, "ship", None, "Scoop", &[PyRep::Integer(obs_id)], &kw, None).await.unwrap_err().code, "CannotScoop");
        assert_eq!(ctx.dispatch_rpc(&mut sess, "entity", None, "CmdUnanchor", &[PyRep::Integer(obs_id)], &kw, None).await.unwrap_err().code, "CannotUnanchor");

        // 1 ship inside cloak stabilization timer + 1 gate-cloaked ship (both immune)
        ctx.advance_secs(540);
        let (c_stab, s_stab, m_stab) = (91_001_200_i64, 101_001_200_i64, 101_002_200_i64);
        let mut sess_stab = setup_pilot(&ctx, c_stab, s_stab, 11188, lowsec_sys, DVec3::new(5_200_000_000.0, 0.0, 0.0), 1_000_010).await;
        ctx.db.with_mut_snapshot(|s| {
            s.items.insert(m_stab, mk_item(m_stab, 11370, c_stab, s_stab, InventoryFlag::HiSlot0, 1, true));
            s.module_online_states.insert(m_stab, true);
        });
        ctx.invalidate_ship_fitting(s_stab);
        ctx.dispatch_rpc(&mut sess_stab, "dogmaIM", None, "Activate", &[PyRep::Integer(m_stab)], &kw, None).await.unwrap();

        let s_gate = 101_001_201_i64;
        let mut cs_gate = ctx.build_type_combat_state(587);
        cs_gate.is_cloaked = true;
        cs_gate.gate_cloak_remaining_ticks = 50;
        let _ = low_handle.spawn_ball_with_combat(
            Ball { id: s_gate, type_id: 587, position: DVec3::new(5_220_000_000.0, 0.0, 0.0), velocity: DVec3::ZERO, max_velocity: 250.0, inertia: 0.5, mass: 1_000_000.0, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop },
            cs_gate,
        ).await;

        low_handle.step_manual(1.0).await.unwrap();
        assert!(eve_server::services::mobile_deployables::read_deployable_state(&ctx).deployables.get(&obs_id).unwrap().last_ping_unix.is_none());

        ctx.mechanic_rng.set_seed(Some(42));
        for (cid, _, _, _) in &cloaked_sessions {
            let _ = ctx.pending_notifications.write().unwrap().remove(cid);
        }
        ctx.advance_secs(61);
        low_handle.step_manual(1.0).await.unwrap();

        assert!(low_handle.get_combat_state(s_stab).await.unwrap().unwrap().is_cloaked);
        assert!(low_handle.get_combat_state(s_gate).await.unwrap().unwrap().is_cloaked);

        let mut decloaked_idx = Vec::new();
        for (idx, (cid, sid, mid, _)) in cloaked_sessions.iter().enumerate() {
            let cs = low_handle.get_combat_state(*sid).await.unwrap().unwrap();
            if !cs.is_cloaked {
                decloaked_idx.push(idx);
                assert!(!ctx.db.with_snapshot(|s| s.module_active_states.contains(mid)));
                assert!(cs.decloak_targeting_delay_remaining_s > 0.0);
                let notes = ctx.pending_notifications.write().unwrap().remove(cid).unwrap_or_default();
                assert!(notes.iter().any(|n| n.event == "OnGodmaShipEffect"));
                assert!(notes.iter().any(|n| n.event == "OnSpecialFX" && n.args.len() == 14));
            }
        }
        assert!(!decloaked_idx.is_empty() && decloaked_idx.len() < 12, "Decloaked {}/12 ships", decloaked_idx.len());

        let (_, _, d_mid, ref mut d_sess) = cloaked_sessions[decloaked_idx[0]];
        let recloak_err = ctx.dispatch_rpc(d_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(d_mid)], &kw, None).await.unwrap_err();
        assert!(matches!(recloak_err.code.as_str(), "ModuleReactivationDelay" | "ModuleInReactivationCooldown" | "CloakReactivationDelay"));

        ctx.advance_secs(600);
        low_handle.step_manual(1.0).await.unwrap();
        assert!(eve_server::services::mobile_deployables::read_deployable_state(&ctx).deployables.get(&obs_id).unwrap().last_ping_unix.unwrap() >= t_dep + 1_200);

        ctx.advance_secs(7_201);
        low_handle.step_manual(1.0).await.unwrap();
        assert!(low_handle.get_ball_snapshot(obs_id).await.unwrap().is_none());
    }
}

pub(crate) mod standup_point_defense_battery {
    // `open_feats_b39_part2` — Standup Structure Weapons (`feat:structures.standup-point-defense-battery`,
    // `feat:structures.standup-guided-bomb-launcher`, `feat:structures.standup-superweapon-arcing-vorton`).

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

        ctx.db.with_mut_snapshot(|s| {
            s.characters.get_mut(&gunner_id).unwrap().structure_id = Some(keepstar_id);
        });
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

        ctx.db.with_mut_snapshot(|s| {
            s.characters.get_mut(&gunner_id).unwrap().structure_id = Some(keepstar_id);
        });
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
        ctx.db.with_mut_snapshot(|s| {
            s.characters.get_mut(&gunner_id).unwrap().structure_id = Some(keepstar_id);
        });
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
        assert!(ctx.db.with_snapshot(|s| s.session_cluster_lifecycle.pvp_timers_until.get(&c_susp).copied().unwrap_or(0) > ctx.now_unix()), "Suspect victim received OF-51 PvP logoff timer");
        assert_eq!(ctx.db.with_snapshot(|s| s.session_cluster_lifecycle.pvp_timers_until.get(&c_neut).copied().unwrap_or(0)), 0, "Spared neutral has no PvP timer");

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
        assert!(ctx.db.with_snapshot(|s| s.session_cluster_lifecycle.pvp_timers_until.get(&c_neut).copied().unwrap_or(0) > ctx.now_unix()), "Splashed neutral received OF-51 PvP logoff timer");
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
}

pub(crate) mod clone_vat_bay_titan_rorqual {
    // Batch 39 open `feat:` parity integration tests (part 3: Clone Vat Bay fuel, immobilization, fleet clone install & CloneJump).

    use eve_db::{InventoryFlag, InventoryItemRecord, IskCents, TrainedSkillRecord};
    use eve_destiny::{Ball, MovementMode};
    use eve_machonet::SessionState;
    use eve_proto::PyRep;
    use eve_server::router::ServerContext;
    use glam::DVec3;
    use std::collections::HashMap;

    async fn setup_pilot(
        ctx: &ServerContext,
        char_id: i64,
        ship_id: i64,
        ship_type_id: u32,
        sys_id: u32,
        pos: DVec3,
        corp_id: i64,
        station_id: Option<i64>,
    ) -> SessionState {
        let mut sess = SessionState::new((900_000 + (char_id % 100_000)) as u64);
        sess.user_id = Some(char_id as u64);
        ctx.db.with_mut_snapshot(|s| {
            let mut ch = s.characters.get(&90_000_001).cloned().unwrap();
            ch.character_id = char_id;
            ch.name = format!("Pilot-{char_id}");
            ch.corporation_id = corp_id;
            ch.alliance_id = None;
            ch.solar_system_id = sys_id;
            ch.active_ship_id = ship_id;
            ch.station_id = station_id;
            ch.structure_id = None;
            ch.security_status = 0.0;
            ch.balance = IskCents::from_isk(500_000_000);
            s.characters.insert(char_id, ch);
            s.items.insert(
                ship_id,
                InventoryItemRecord {
                    item_id: ship_id,
                    type_id: ship_type_id,
                    owner_id: char_id,
                    location_id: station_id.unwrap_or(sys_id as i64),
                    flag: if station_id.is_some() { InventoryFlag::Hangar } else { InventoryFlag::None },
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
            if station_id.is_none() {
                s.item_positions.insert(ship_id, [pos.x, pos.y, pos.z]);
            }
        });
        sess.character_id = Some(char_id);
        sess.corporation_id = Some(corp_id);
        sess.solar_system_id = Some(sys_id);
        sess.ship_id = Some(ship_id);
        sess.station_id = station_id;
        sess.structure_id = None;

        if station_id.is_none() {
            let ship_type = ctx.sde.types.get(&ship_type_id);
            let handle = ctx.systems.get_or_spawn(sys_id);
            let _ = handle
                .spawn_ball_with_combat(
                    Ball {
                        id: ship_id,
                        type_id: ship_type_id,
                        position: pos,
                        velocity: DVec3::ZERO,
                        max_velocity: 250.0,
                        inertia: 0.5,
                        mass: ship_type.map(|t| t.mass).unwrap_or(1_500_000.0),
                        radius: ship_type.map(|t| t.radius).unwrap_or(50.0),
                        speed_fraction: 0.0,
                        mode: MovementMode::Stop,
                    },
                    ctx.build_type_combat_state(ship_type_id),
                )
                .await;
        }
        sess
    }

    #[tokio::test]
    async fn test_b39_clone_vat_bay_titan_and_rorqual_install_and_jump() {
        // parity: feat:modules.clone-vat-bay-fuel
        eve_testkit::attest(&["feat:modules.clone-vat-bay-fuel"]);
        let ctx = ServerContext::new();
        let kw = HashMap::new();
        let sys_host = 30_000_012_u32; // Asabona (low-sec)
        let jita_sys = 30_000_142_u32;
        let jita_station = 60_003_760_i64;

        // 1. Negative cases: non-Titan/Rorqual hull rejected & insufficient Liquid Ozone rejected
        let c_subcap = 91_000_050_i64;
        let s_subcap = 101_000_050_i64;
        let mut sess_subcap = setup_pilot(&ctx, c_subcap, s_subcap, 24692, sys_host, DVec3::new(100_000.0, 0.0, 0.0), 1_000_009, None).await;
        let bad_cvb = 101_000_700_i64;
        ctx.db.with_mut_snapshot(|s| {
            s.items.insert(
                bad_cvb,
                InventoryItemRecord {
                    item_id: bad_cvb,
                    type_id: 23735, // Clone Vat Bay I
                    owner_id: c_subcap,
                    location_id: s_subcap,
                    flag: InventoryFlag::HiSlot0,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
            s.module_online_states.insert(bad_cvb, true);
        });
        let hull_err = ctx
            .dispatch_rpc(&mut sess_subcap, "dogmaIM", None, "Activate", &[PyRep::Integer(bad_cvb)], &kw, None)
            .await
            .expect_err("Non-Titan/Rorqual hull cannot activate Clone Vat Bay");
        assert_eq!(hull_err.code, "ShipCannotFitModule");

        // 2. Titan (Avatar 11567) AND Rorqual (28352): SpecializedFuelBay priority, single LO consumption, ship immobilization, fleet install + CloneJump!
        let c_titan = 91_000_051_i64;
        let s_titan = 101_000_051_i64;
        let c_rorq = 91_000_055_i64;
        let s_rorq = 101_000_055_i64;
        let c_mate = 91_000_052_i64;
        let s_mate = 101_000_052_i64;
        let c_outsider = 91_000_053_i64;
        let s_outsider = 101_000_053_i64;

        let titan_pos = DVec3::new(5_000_000_000.0, 0.0, 0.0);
        let rorq_pos = DVec3::new(5_000_500_000.0, 0.0, 0.0);
        let mut sess_titan = setup_pilot(&ctx, c_titan, s_titan, 11567, sys_host, titan_pos, 1_000_009, None).await;
        let mut sess_rorq = setup_pilot(&ctx, c_rorq, s_rorq, 28352, sys_host, rorq_pos, 1_000_009, None).await;
        let mut sess_mate = setup_pilot(&ctx, c_mate, s_mate, 587, sys_host, titan_pos + DVec3::new(25_000.0, 0.0, 0.0), 1_000_009, None).await;
        let _sess_outsider = setup_pilot(&ctx, c_outsider, s_outsider, 587, sys_host, titan_pos + DVec3::new(40_000.0, 0.0, 0.0), 1_000_010, None).await;
        let beyonce_titan = ctx.bind_object(&mut sess_titan, "beyonce", sys_host as i64).unwrap();

        let cvb_titan = 101_000_701_i64;
        let cvb_rorq = 101_000_711_i64;
        let lo_fuel_bay = 101_000_702_i64;
        let lo_cargo = 101_000_703_i64;
        let lo_rorq = 101_000_712_i64;

        ctx.db.with_mut_snapshot(|s| {
            for cid in [c_mate, c_outsider] {
                s.skills.entry(cid).or_default().insert(
                    24242, // Infomorph Psychology V
                    TrainedSkillRecord { type_id: 24242, level: 5, skill_points: 256_000.0 },
                );
            }
            s.fleets.insert(
                889,
                eve_db::FleetRecord {
                    fleet_id: 889,
                    boss_id: c_titan,
                    members: vec![c_titan, c_rorq, c_mate],
                    ..Default::default()
                },
            );
            for (mid, owner, ship) in [(cvb_titan, c_titan, s_titan), (cvb_rorq, c_rorq, s_rorq)] {
                s.items.insert(
                    mid,
                    InventoryItemRecord {
                        item_id: mid,
                        type_id: 23735, // Clone Vat Bay I
                        owner_id: owner,
                        location_id: ship,
                        flag: InventoryFlag::HiSlot0,
                        quantity: 1,
                        singleton: true,
                        ..Default::default()
                    },
                );
                s.module_online_states.insert(mid, true);
            }
            // Start with 0 LO on Titan -> NotEnoughFuel
            s.items.insert(
                lo_fuel_bay,
                InventoryItemRecord {
                    item_id: lo_fuel_bay,
                    type_id: 16273,
                    owner_id: c_titan,
                    location_id: s_titan,
                    flag: InventoryFlag::SpecializedFuelBay,
                    quantity: 0,
                    singleton: false,
                    ..Default::default()
                },
            );
            s.items.insert(
                lo_rorq,
                InventoryItemRecord {
                    item_id: lo_rorq,
                    type_id: 16273,
                    owner_id: c_rorq,
                    location_id: s_rorq,
                    flag: InventoryFlag::SpecializedFuelBay,
                    quantity: 1_000,
                    singleton: false,
                    ..Default::default()
                },
            );
        });
        sess_titan.fleet_id = Some(889);
        sess_rorq.fleet_id = Some(889);
        sess_mate.fleet_id = Some(889);

        // Insufficient fuel check
        let no_fuel_err = ctx
            .dispatch_rpc(&mut sess_titan, "dogmaIM", None, "Activate", &[PyRep::Integer(cvb_titan)], &kw, None)
            .await
            .expect_err("Insufficient Liquid Ozone rejected");
        assert_eq!(no_fuel_err.code, "NotEnoughFuel");

        // Clone offer before activation is rejected
        let inactive_err = ctx
            .dispatch_rpc(
                &mut sess_titan,
                "jumpCloneSvc",
                None,
                "OfferShipCloneInstallation",
                &[PyRep::Integer(c_mate), PyRep::Integer(s_titan)],
                &kw,
                None,
            )
            .await
            .unwrap_err();
        assert_eq!(inactive_err.code, "NoActiveCloneVatBay");

        // Top up Titan SpecializedFuelBay (500 LO) + Cargo (500 LO) to verify SpecializedFuelBay priority & zero double-consumption
        ctx.db.with_mut_snapshot(|s| {
            s.items.get_mut(&lo_fuel_bay).unwrap().quantity = 500;
            s.items.insert(
                lo_cargo,
                InventoryItemRecord {
                    item_id: lo_cargo,
                    type_id: 16273,
                    owner_id: c_titan,
                    location_id: s_titan,
                    flag: InventoryFlag::Cargo,
                    quantity: 500,
                    singleton: false,
                    ..Default::default()
                },
            );
        });

        ctx.dispatch_rpc(&mut sess_titan, "dogmaIM", None, "Activate", &[PyRep::Integer(cvb_titan)], &kw, None)
            .await
            .expect("Titan activates Clone Vat Bay I");
        ctx.dispatch_rpc(&mut sess_rorq, "dogmaIM", None, "Activate", &[PyRep::Integer(cvb_rorq)], &kw, None)
            .await
            .expect("Rorqual activates Clone Vat Bay I");

        let (fb_after_act, cargo_after_act, rorq_after_act) = ctx.db.with_snapshot(|s| {
            (
                s.items.get(&lo_fuel_bay).map(|i| i.quantity).unwrap_or(0),
                s.items.get(&lo_cargo).map(|i| i.quantity).unwrap_or(0),
                s.items.get(&lo_rorq).map(|i| i.quantity).unwrap_or(0),
            )
        });
        assert!(fb_after_act < 500, "Titan consumed Liquid Ozone from SpecializedFuelBay ({fb_after_act} < 500)");
        assert_eq!(cargo_after_act, 500, "Cargo LO untouched when SpecializedFuelBay has enough fuel");
        assert!(rorq_after_act < 1_000, "Rorqual consumed Liquid Ozone ({rorq_after_act} < 1000)");

        // Ship immobilization enforced during active Clone Vat Bay cycle
        let sys_handle = ctx.systems.get_or_spawn(sys_host);
        let (_, titan_ball) = sys_handle.get_ball_snapshot(s_titan).await.unwrap().unwrap();
        assert_eq!(titan_ball.max_velocity, 0.0, "Titan max_velocity set to 0 while Clone Vat Bay is active");
        let warp_err = ctx
            .dispatch_rpc(
                &mut sess_titan,
                "beyonce",
                Some(beyonce_titan),
                "CmdGotoDirection",
                &[PyRep::Real(1.0), PyRep::Real(0.0), PyRep::Real(0.0)],
                &kw,
                None,
            )
            .await;
        let _ = warp_err;
        sys_handle.step_manual(2.0).await.unwrap();
        let (_, titan_ball_after_step) = sys_handle.get_ball_snapshot(s_titan).await.unwrap().unwrap();
        assert!((titan_ball_after_step.position - titan_pos).length() < 1.0, "Immobilized Titan cannot move during cycle");

        // Non-fleet outsider cannot be offered a clone
        let non_fleet_offer_err = ctx
            .dispatch_rpc(
                &mut sess_titan,
                "jumpCloneSvc",
                None,
                "OfferShipCloneInstallation",
                &[PyRep::Integer(c_outsider), PyRep::Integer(s_titan)],
                &kw,
                None,
            )
            .await
            .unwrap_err();
        assert_eq!(non_fleet_offer_err.code, "TargetNotInSameFleetOrSystem");

        // Fleetmate installs clone in Titan AND in Rorqual (verify ZERO extra LO deducted on AcceptShipCloneInstallation!)
        ctx.dispatch_rpc(
            &mut sess_titan,
            "jumpCloneSvc",
            None,
            "OfferShipCloneInstallation",
            &[PyRep::Integer(c_mate), PyRep::Integer(s_titan)],
            &kw,
            None,
        )
        .await
        .expect("Offer clone on Titan");
        ctx.dispatch_rpc(&mut sess_mate, "jumpCloneSvc", None, "AcceptShipCloneInstallation", &[], &kw, None)
            .await
            .expect("Accept clone on Titan");

        let fb_after_install = ctx.db.with_snapshot(|s| s.items.get(&lo_fuel_bay).map(|i| i.quantity).unwrap_or(0));
        assert_eq!(fb_after_install, fb_after_act, "AcceptShipCloneInstallation does NOT double-deduct Liquid Ozone");

        // Also install a second clone on the Rorqual
        let _ = sys_handle.set_ball_position(s_mate, rorq_pos + DVec3::new(500.0, 0.0, 0.0)).await;
        ctx.dispatch_rpc(
            &mut sess_rorq,
            "jumpCloneSvc",
            None,
            "OfferShipCloneInstallation",
            &[PyRep::Integer(c_mate), PyRep::Integer(s_rorq)],
            &kw,
            None,
        )
        .await
        .expect("Offer clone on Rorqual");
        ctx.dispatch_rpc(&mut sess_mate, "jumpCloneSvc", None, "AcceptShipCloneInstallation", &[], &kw, None)
            .await
            .expect("Accept clone on Rorqual");

        let (titan_clone_id, rorq_clone_id) = ctx.db.with_snapshot(|s| {
            let clones = s.jump_clones.get(&c_mate).unwrap();
            let tc = clones.iter().find(|c| c.location_id == s_titan).unwrap().jump_clone_id;
            let rc = clones.iter().find(|c| c.location_id == s_rorq).unwrap().jump_clone_id;
            (tc, rc)
        });

        // 3. Dock fleetmate in Jita 4-4 NPC station (60_003_760) in a Capsule (670) and test `jumpCloneSvc.CloneJump` to the ship clone!
        let jita_pod_id = 101_000_999_i64;
        let _ = sys_handle.remove_ball(s_mate).await;
        ctx.db.with_mut_snapshot(|s| {
            s.items.insert(
                jita_pod_id,
                InventoryItemRecord {
                    item_id: jita_pod_id,
                    type_id: 670,
                    owner_id: c_mate,
                    location_id: jita_station,
                    flag: InventoryFlag::Hangar,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
            let ch = s.characters.get_mut(&c_mate).unwrap();
            ch.solar_system_id = jita_sys;
            ch.station_id = Some(jita_station);
            ch.structure_id = None;
            ch.active_ship_id = jita_pod_id;
            s.items.get_mut(&s_mate).unwrap().location_id = jita_station;
            s.items.get_mut(&s_mate).unwrap().flag = InventoryFlag::Hangar;
        });
        sess_mate.solar_system_id = Some(jita_sys);
        sess_mate.station_id = Some(jita_station);
        sess_mate.ship_id = Some(jita_pod_id);

        // Negative check: leaving fleet blocks `CloneJump` to ship clone!
        ctx.db.with_mut_snapshot(|s| {
            s.fleets.get_mut(&889).unwrap().members.retain(|&m| m != c_mate);
        });
        sess_mate.fleet_id = None;
        let left_fleet_err = ctx
            .dispatch_rpc(
                &mut sess_mate,
                "jumpCloneSvc",
                None,
                "CloneJump",
                &[PyRep::Integer(titan_clone_id)],
                &kw,
                None,
            )
            .await
            .expect_err("Non-fleet pilot cannot CloneJump into ship Clone Vat Bay");
        assert_eq!(left_fleet_err.code, "NotInHostShipFleet");

        // Rejoin fleet, set non-trivial corp/alliance/sec-status on jumper, and verify live Ballpark position + cloak checks + SlimItemMeta!
        let (mate_corp, mate_alliance) = (98_000_777_i64, 99_000_777_i64);
        let moved_titan_pos = DVec3::new(7_500_000_000.0, 250_000.0, -100_000.0);
        let _ = sys_handle.set_ball_position(s_titan, moved_titan_pos).await;
        ctx.db.with_mut_snapshot(|s| {
            s.fleets.get_mut(&889).unwrap().members.push(c_mate);
            // Deliberately leave stale `[0, 0, 0]` in `item_positions` to prove live Ballpark coordinates are used!
            s.item_positions.insert(s_titan, [0.0, 0.0, 0.0]);
            let ch = s.characters.get_mut(&c_mate).unwrap();
            ch.corporation_id = mate_corp;
            ch.alliance_id = Some(mate_alliance);
            ch.security_status = 2.75;
        });
        sess_mate.fleet_id = Some(889);
        sess_mate.corporation_id = Some(mate_corp);

        // Negative check: Cloaked host Titan must block `CloneJump`!
        assert!(sys_handle.try_update_combat_state(s_titan, |cs| cs.is_cloaked = true));
        let cloaked_jump_err = ctx
            .dispatch_rpc(
                &mut sess_mate,
                "jumpCloneSvc",
                None,
                "CloneJump",
                &[PyRep::Integer(titan_clone_id)],
                &kw,
                None,
            )
            .await
            .expect_err("Cloaked host Titan must reject CloneJump");
        assert_eq!(cloaked_jump_err.code, "ShipCloaked");
        assert!(sys_handle.try_update_combat_state(s_titan, |cs| cs.is_cloaked = false));

        ctx.dispatch_rpc(
            &mut sess_mate,
            "jumpCloneSvc",
            None,
            "CloneJump",
            &[PyRep::Integer(titan_clone_id)],
            &kw,
            None,
        )
        .await
        .expect("Fleet member CloneJumps to active uncloaked Titan Clone Vat Bay");

        assert_eq!(sess_mate.solar_system_id, Some(sys_host));
        assert_eq!(sess_mate.station_id, None, "Ship clone jump places pilot in space, not docked");
        assert_eq!(sess_mate.structure_id, None);
        let pod_id = sess_mate.ship_id.expect("spawned capsule ID");
        let pod_type = ctx.db.with_snapshot(|s| s.items.get(&pod_id).map(|i| i.type_id));
        assert_eq!(pod_type, Some(670), "Jumper spawned in Capsule (type 670)");
        let (_, pod_ball) = sys_handle.get_ball_snapshot(pod_id).await.unwrap().expect("Capsule ball in space");
        assert!((pod_ball.position - moved_titan_pos).length() < 10_000.0, "Capsule spawned adjacent to live Titan Ball coordinates, not stale [0,0,0]");
        let pod_meta = sys_handle.ballpark.read().unwrap().ball_slim_meta.get(&pod_id).cloned().expect("Capsule SlimItemMeta");
        assert_eq!(pod_meta.corp_id, mate_corp, "SlimItemMeta populated with jumper corporation_id");
        assert_eq!(pod_meta.alliance_id, mate_alliance, "SlimItemMeta populated with jumper alliance_id");
        assert!((pod_meta.security_status - 2.75).abs() < 1e-6, "SlimItemMeta populated with jumper security_status");

        // 4. Dock mate back in Jita, verify missing live Ballpark host ball rejects CloneJump even while DB CloneVatBayState is active, then verify cycle expiry!
        let _ = sys_handle.remove_ball(pod_id).await;
        ctx.db.with_mut_snapshot(|s| {
            let ch = s.characters.get_mut(&c_mate).unwrap();
            ch.solar_system_id = jita_sys;
            ch.station_id = Some(jita_station);
            ch.active_ship_id = jita_pod_id;
            ch.last_clone_jump_unix = 0;
        });
        sess_mate.solar_system_id = Some(jita_sys);
        sess_mate.station_id = Some(jita_station);
        sess_mate.ship_id = Some(jita_pod_id);
        ctx.advance_secs(15);

        let (_, saved_rorq_ball) = sys_handle.get_ball_snapshot(s_rorq).await.unwrap().unwrap();
        let saved_rorq_cs = sys_handle.get_combat_state(s_rorq).await.unwrap().unwrap();
        let _ = sys_handle.remove_ball(s_rorq).await;
        let missing_ball_err = ctx
            .dispatch_rpc(
                &mut sess_mate,
                "jumpCloneSvc",
                None,
                "CloneJump",
                &[PyRep::Integer(rorq_clone_id)],
                &kw,
                None,
            )
            .await
            .expect_err("Missing live Ballpark host ship must block CloneJump");
        assert_eq!(missing_ball_err.code, "NoActiveCloneVatBay");
        let _ = sys_handle.spawn_ball_with_combat(saved_rorq_ball, saved_rorq_cs).await;

        ctx.advance_secs(305);
        sys_handle.step_manual(305.0).await.unwrap();

        let expired_offer_err = ctx
            .dispatch_rpc(
                &mut sess_rorq,
                "jumpCloneSvc",
                None,
                "OfferShipCloneInstallation",
                &[PyRep::Integer(c_mate), PyRep::Integer(s_rorq)],
                &kw,
                None,
            )
            .await
            .expect_err("Expired Clone Vat Bay cycle blocks clone installation");
        assert_eq!(expired_offer_err.code, "NoActiveCloneVatBay");

        let expired_jump_err = ctx
            .dispatch_rpc(
                &mut sess_mate,
                "jumpCloneSvc",
                None,
                "CloneJump",
                &[PyRep::Integer(rorq_clone_id)],
                &kw,
                None,
            )
            .await
            .expect_err("Expired Clone Vat Bay cycle blocks CloneJump");
        assert_eq!(expired_jump_err.code, "NoActiveCloneVatBay");
    }
}

pub(crate) mod covert_cyno_rules_jump_portal {
    // `open_feats_b39_part4` — Covert Cynosural Field & Covert Jump Portal Generator
    // (`feat:modules.covert-cyno-rules`, `feat:modules.covert-jump-portal-generator`).

    use eve_db::{InventoryFlag, InventoryItemRecord, TrainedSkillRecord};
    use eve_destiny::{Ball, MovementMode};
    use eve_machonet::SessionState;
    use eve_proto::PyRep;
    use eve_server::router::ServerContext;
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
        sess.ship_id = Some(ship_id);
        sess.solar_system_id = Some(solar_system_id);

        ctx.db.with_mut_snapshot(|s| {
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
            s.items.insert(
                ship_id,
                InventoryItemRecord {
                    item_id: ship_id,
                    type_id: ship_type_id,
                    owner_id: char_id,
                    location_id: solar_system_id as i64,
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
            s.item_positions.insert(ship_id, [position.x, position.y, position.z]);
        });

        let ship_type = ctx.sde.types.get(&ship_type_id);
        let handle = ctx.systems.get_or_spawn(solar_system_id);
        let _ = handle
            .spawn_ball_with_combat(
                Ball {
                    id: ship_id,
                    type_id: ship_type_id,
                    position,
                    velocity: DVec3::ZERO,
                    max_velocity: 250.0,
                    inertia: 0.5,
                    mass: ship_type.map(|t| t.mass).unwrap_or(1_500_000.0),
                    radius: ship_type.map(|t| t.radius).unwrap_or(50.0),
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                ctx.build_type_combat_state(ship_type_id),
            )
            .await;
        sess
    }

    #[tokio::test]
    async fn test_b39_covert_cyno_rules() {
        // parity: feat:modules.covert-cyno-rules
        eve_testkit::attest(&["feat:modules.covert-cyno-rules"]);
        let ctx = ServerContext::new();
        let kw = HashMap::new();
        let sys_a = 30_000_012_u32; // Asabona (low-sec)
        let sys_b = 30_000_014_u32; // Shamahi (low-sec, 2.10 LY from Asabona)

        let c_cov = 91_000_041_i64;
        let s_cov = 101_000_041_i64; // Hound (12034)
        let c_prospect = 91_000_042_i64;
        let s_prospect = 101_000_042_i64; // Prospect (33697)
        let c_etana = 91_000_043_i64;
        let s_etana = 101_000_043_i64; // Etana (32790)
        let c_rabisu = 91_000_044_i64;
        let s_rabisu = 101_000_044_i64; // Rabisu (42245)
        let c_rifter = 91_000_045_i64;
        let s_rifter = 101_000_045_i64; // Rifter (587, non-covert)
        let c_offgrid = 91_000_046_i64;
        let s_offgrid = 101_000_046_i64; // 20,000 km away in sys_b (different grid bubble!)
        let c_ongrid = 91_000_047_i64;
        let s_ongrid = 101_000_047_i64; // 5 km away in sys_b (same grid bubble!)

        let mut sess_cov = setup_pilot(&ctx, c_cov, s_cov, 12034, sys_b, DVec3::new(100_000.0, 0.0, 0.0), 1_000_009).await;
        let mut sess_prospect = setup_pilot(&ctx, c_prospect, s_prospect, 33697, sys_b, DVec3::new(200_000.0, 0.0, 0.0), 1_000_009).await;
        let mut sess_etana = setup_pilot(&ctx, c_etana, s_etana, 32790, sys_b, DVec3::new(300_000.0, 0.0, 0.0), 1_000_009).await;
        let mut sess_rabisu = setup_pilot(&ctx, c_rabisu, s_rabisu, 42245, sys_b, DVec3::new(400_000.0, 0.0, 0.0), 1_000_009).await;
        let mut sess_rifter = setup_pilot(&ctx, c_rifter, s_rifter, 587, sys_b, DVec3::new(500_000.0, 0.0, 0.0), 1_000_009).await;
        let mut sess_offgrid = setup_pilot(&ctx, c_offgrid, s_offgrid, 587, sys_b, DVec3::new(20_000_000.0, 0.0, 0.0), 1_000_010).await;
        let mut sess_ongrid = setup_pilot(&ctx, c_ongrid, s_ongrid, 587, sys_b, DVec3::new(105_000.0, 0.0, 0.0), 1_000_010).await;

        ctx.db.with_mut_snapshot(|s| {
            s.fleets.insert(
                888,
                eve_db::FleetRecord {
                    fleet_id: 888,
                    boss_id: c_cov,
                    members: vec![c_cov, c_prospect, c_etana, c_rabisu, c_rifter],
                    ..Default::default()
                },
            );
            for (idx, (cid, sid)) in [
                (c_cov, s_cov),
                (c_prospect, s_prospect),
                (c_etana, s_etana),
                (c_rabisu, s_rabisu),
                (c_rifter, s_rifter),
            ]
            .into_iter()
            .enumerate()
            {
                s.skills.entry(cid).or_default().insert(
                    21603, // Cynosural Field Theory V
                    TrainedSkillRecord { type_id: 21603, level: 5, skill_points: 256_000.0 },
                );
                let mid = 101_000_800 + idx as i64;
                let loid = 101_000_810 + idx as i64;
                s.items.insert(
                    mid,
                    InventoryItemRecord {
                        item_id: mid,
                        type_id: 28646, // Covert Cynosural Field Generator I
                        owner_id: cid,
                        location_id: sid,
                        flag: InventoryFlag::HiSlot0,
                        quantity: 1,
                        singleton: true,
                        ..Default::default()
                    },
                );
                s.module_online_states.insert(mid, true);
                s.items.insert(
                    loid,
                    InventoryItemRecord {
                        item_id: loid,
                        type_id: 16273, // Liquid Ozone
                        owner_id: cid,
                        location_id: sid,
                        flag: InventoryFlag::Cargo,
                        quantity: 200,
                        singleton: false,
                        ..Default::default()
                    },
                );
            }
        });
        for sess in [&mut sess_cov, &mut sess_prospect, &mut sess_etana, &mut sess_rabisu, &mut sess_rifter] {
            sess.fleet_id = Some(888);
        }

        // 1. Requires Cynosural Field Theory V (CFT IV rejected) & fits only covert-cloak hulls + Etana/Prospect/Rabisu
        ctx.db.with_mut_snapshot(|s| {
            s.skills.get_mut(&c_cov).unwrap().get_mut(&21603).unwrap().level = 4;
        });
        let sk_err = ctx
            .dispatch_rpc(&mut sess_cov, "dogmaIM", None, "Activate", &[PyRep::Integer(101_000_800)], &kw, None)
            .await
            .unwrap_err();
        assert_eq!(sk_err.code, "SkillMissing");
        ctx.db.with_mut_snapshot(|s| {
            s.skills.get_mut(&c_cov).unwrap().get_mut(&21603).unwrap().level = 5;
        });

        let rifter_err = ctx
            .dispatch_rpc(&mut sess_rifter, "dogmaIM", None, "Activate", &[PyRep::Integer(101_000_804)], &kw, None)
            .await
            .unwrap_err();
        assert_eq!(rifter_err.code, "CannotLightCynoInSystem");

        // Strategic Cruiser without Covert Reconfiguration subsystem cannot light Covert Cyno, then WITH 45589 succeeds!
        let c_t3 = 91_000_048_i64;
        let s_t3 = 101_000_048_i64;
        let mut sess_t3 = setup_pilot(&ctx, c_t3, s_t3, 29984, sys_b, DVec3::new(600_000.0, 0.0, 0.0), 1_000_009).await;
        sess_t3.fleet_id = Some(888);
        ctx.db.with_mut_snapshot(|s| {
            s.fleets.get_mut(&888).unwrap().members.push(c_t3);
            s.skills.entry(c_t3).or_default().insert(21603, TrainedSkillRecord { type_id: 21603, level: 5, skill_points: 256_000.0 });
            s.skills.entry(c_t3).or_default().insert(30544, TrainedSkillRecord { type_id: 30544, level: 5, skill_points: 256_000.0 });
            s.items.insert(101_000_809, InventoryItemRecord { item_id: 101_000_809, type_id: 28646, owner_id: c_t3, location_id: s_t3, flag: InventoryFlag::HiSlot0, quantity: 1, singleton: true, ..Default::default() });
            s.items.insert(101_000_819, InventoryItemRecord { item_id: 101_000_819, type_id: 16273, owner_id: c_t3, location_id: s_t3, flag: InventoryFlag::Cargo, quantity: 200, singleton: false, ..Default::default() });
            s.module_online_states.insert(101_000_809, true);
        });
        ctx.invalidate_ship_fitting(s_t3);
        assert_eq!(ctx.dispatch_rpc(&mut sess_t3, "dogmaIM", None, "Activate", &[PyRep::Integer(101_000_809)], &kw, None).await.unwrap_err().code, "CannotLightCynoInSystem");
        ctx.db.with_mut_snapshot(|s| {
            s.items.insert(101_000_829, InventoryItemRecord { item_id: 101_000_829, type_id: 45589, owner_id: c_t3, location_id: s_t3, flag: InventoryFlag::SubSystemSlot1, quantity: 1, singleton: true, ..Default::default() });
            s.module_online_states.insert(101_000_829, true);
        });
        ctx.invalidate_ship_fitting(s_t3);
        ctx.dispatch_rpc(&mut sess_t3, "dogmaIM", None, "Activate", &[PyRep::Integer(101_000_809)], &kw, None).await.expect("Tengu with Covert Reconfiguration lights Covert Cyno");

        // 2. Cyno-jammed system: normal cyno (21096) on Force Recon (Pilgrim 11965) fails with CynoJammed, while Covert Cyno succeeds!
        let c_pilgrim = 91_000_047_i64;
        let s_pilgrim = 101_000_047_i64;
        let mut sess_pilgrim = setup_pilot(&ctx, c_pilgrim, s_pilgrim, 11965, sys_b, DVec3::new(110_000.0, 0.0, 0.0), 1_000_009).await;
        sess_pilgrim.fleet_id = Some(888);
        ctx.db.with_mut_snapshot(|s| {
            s.fleets.get_mut(&888).unwrap().members.push(c_pilgrim);
            s.skills.entry(c_pilgrim).or_default().insert(21603, TrainedSkillRecord { type_id: 21603, level: 5, skill_points: 256_000.0 });
            s.items.insert(
                999_001,
                InventoryItemRecord {
                    item_id: 999_001, type_id: 33700, owner_id: 1, location_id: sys_b as i64,
                    flag: InventoryFlag::None, quantity: 1, singleton: true, ..Default::default()
                },
            );
            s.items.insert(
                101_000_820,
                InventoryItemRecord {
                    item_id: 101_000_820, type_id: 21096, owner_id: c_pilgrim, location_id: s_pilgrim,
                    flag: InventoryFlag::HiSlot1, quantity: 1, singleton: true, ..Default::default()
                },
            );
            s.items.insert(
                101_000_821,
                InventoryItemRecord {
                    item_id: 101_000_821, type_id: 16273, owner_id: c_pilgrim, location_id: s_pilgrim,
                    flag: InventoryFlag::Cargo, quantity: 500, singleton: false, ..Default::default()
                },
            );
        });
        let norm_jam_err = ctx
            .dispatch_rpc(&mut sess_pilgrim, "dogmaIM", None, "Activate", &[PyRep::Integer(101_000_820)], &kw, None)
            .await
            .unwrap_err();
        assert_eq!(norm_jam_err.code, "CynoJammed");

        let cov_rep = ctx
            .dispatch_rpc(&mut sess_cov, "dogmaIM", None, "Activate", &[PyRep::Integer(101_000_800)], &kw, None)
            .await
            .expect("Hound lights Covert Cyno in cyno-jammed system");
        let cov_beacon_id = kv_get(&cov_rep, "beaconID").and_then(|v| v.as_i64()).unwrap();

        // Verify 1/10 cycle time (60s vs 600s) and 1/10 fuel (25 LO vs 250 LO at CFT V)
        assert_eq!(kv_get(&cov_rep, "durationSeconds").and_then(|v| v.as_f64()), Some(60.0));
        assert_eq!(kv_get(&cov_rep, "consumptionQuantity").and_then(|v| v.as_i64()), Some(25));
        assert_eq!(ctx.db.with_snapshot(|s| s.items.get(&101_000_810).unwrap().quantity), 175);

        // Prospect (33697), Etana (32790), and Rabisu (42245) all successfully light Covert Cyno!
        for (sess, mid) in [
            (&mut sess_prospect, 101_000_801_i64),
            (&mut sess_etana, 101_000_802_i64),
            (&mut sess_rabisu, 101_000_803_i64),
        ] {
            ctx.dispatch_rpc(sess, "dogmaIM", None, "Activate", &[PyRep::Integer(mid)], &kw, None)
                .await
                .expect("Special hull (Prospect/Etana/Rabisu) lights Covert Cyno");
        }

        // 3. On-grid-only visibility: hidden from `map.GetBeaconCount` and off-grid `scanMgr.ConeScan`, visible to on-grid `scanMgr.ConeScan`!
        let map_counts = ctx.dispatch_rpc(&mut sess_cov, "map", None, "GetBeaconCount", &[], &kw, None).await.unwrap();
        let sys_b_entry = match &map_counts {
            PyRep::Dict(d) => d.iter().find(|(k, _)| k.as_i64() == Some(sys_b as i64)),
            _ => None,
        };
        assert!(sys_b_entry.is_none(), "Covert cyno hidden from system map beacon count");

        let au_m = 14.3 * 149_597_870_700.0;
        let offgrid_dscan = ctx
            .dispatch_rpc(&mut sess_offgrid, "scanMgr", None, "ConeScan", &[PyRep::Real(360.0), PyRep::Real(au_m)], &kw, None)
            .await
            .unwrap();
        let offgrid_list = match &offgrid_dscan {
            PyRep::List(l) => l,
            _ => panic!("ConeScan list"),
        };
        let offgrid_sees_beacon = offgrid_list.iter().any(|entry| kv_get(entry, "itemID").and_then(|v| v.as_i64()) == Some(cov_beacon_id));
        assert!(!offgrid_sees_beacon, "Off-grid ship cannot see Covert Cyno on D-Scan");

        let ongrid_dscan = ctx
            .dispatch_rpc(&mut sess_ongrid, "scanMgr", None, "ConeScan", &[PyRep::Real(360.0), PyRep::Real(au_m)], &kw, None)
            .await
            .unwrap();
        let ongrid_list = match &ongrid_dscan {
            PyRep::List(l) => l,
            _ => panic!("ConeScan list"),
        };
        let ongrid_sees_beacon = ongrid_list.iter().any(|entry| kv_get(entry, "itemID").and_then(|v| v.as_i64()) == Some(cov_beacon_id));
        assert!(ongrid_sees_beacon, "On-grid ship sees Covert Cyno on grid");

        // 4. Jump & bridge restrictions: ONLY Black Ops can jump or bridge to a Covert Cyno (Dreadnought & Titan rejected)
        let c_titan = 91_000_049_i64;
        let s_titan = 101_000_049_i64;
        let mut sess_titan = setup_pilot(&ctx, c_titan, s_titan, 11567, sys_a, DVec3::new(100_000.0, 0.0, 0.0), 1_000_009).await;
        let c_blops = 91_000_059_i64;
        let s_blops = 101_000_059_i64;
        let mut sess_blops = setup_pilot(&ctx, c_blops, s_blops, 22428, sys_a, DVec3::new(105_000.0, 0.0, 0.0), 1_000_009).await;
        sess_titan.fleet_id = Some(888);
        sess_blops.fleet_id = Some(888);

        ctx.db.with_mut_snapshot(|s| {
            s.fleets.get_mut(&888).unwrap().members.extend([c_titan, c_blops]);
            for cid in [c_titan, c_blops] {
                s.skills.entry(cid).or_default().insert(3456, TrainedSkillRecord { type_id: 3456, level: 5, skill_points: 256_000.0 });
                s.skills.entry(cid).or_default().insert(24562, TrainedSkillRecord { type_id: 24562, level: 5, skill_points: 256_000.0 });
            }
            // Standard Jump Portal Generator I (23953) on Titan
            s.items.insert(
                101_000_830,
                InventoryItemRecord {
                    item_id: 101_000_830, type_id: 23953, owner_id: c_titan, location_id: s_titan,
                    flag: InventoryFlag::HiSlot0, quantity: 1, singleton: true, ..Default::default()
                },
            );
            // Helium Isotopes on Black Ops
            s.items.insert(
                101_000_831,
                InventoryItemRecord {
                    item_id: 101_000_831, type_id: 16274, owner_id: c_blops, location_id: s_blops,
                    flag: InventoryFlag::SpecializedFuelBay, quantity: 50_000, singleton: false, ..Default::default()
                },
            );
        });

        let titan_jump_err = ctx
            .dispatch_rpc(
                &mut sess_titan,
                "beyonce",
                None,
                "CmdJumpToFleetModuleBeacon",
                &[PyRep::Integer(c_cov), PyRep::Integer(sys_b as i64), PyRep::Integer(cov_beacon_id)],
                &kw,
                None,
            )
            .await
            .unwrap_err();
        assert_eq!(titan_jump_err.code, "ShipCannotJumpToBeacon");

        let titan_bridge_err = ctx
            .dispatch_rpc(
                &mut sess_titan,
                "beyonce",
                None,
                "CmdBridgeToFleetModuleBeacon",
                &[PyRep::Integer(c_cov), PyRep::Integer(sys_b as i64), PyRep::Integer(cov_beacon_id)],
                &kw,
                None,
            )
            .await
            .unwrap_err();
        assert_eq!(titan_bridge_err.code, "ShipCannotBridgeToBeacon");

        ctx.dispatch_rpc(
            &mut sess_blops,
            "beyonce",
            None,
            "CmdJumpToFleetModuleBeacon",
            &[PyRep::Integer(c_cov), PyRep::Integer(sys_b as i64), PyRep::Integer(cov_beacon_id)],
            &kw,
            None,
        )
        .await
        .expect("Black Ops jumps to Covert Cynosural Field");
    }

    #[tokio::test]
    async fn test_b39_covert_jump_portal_generator_all_hulls_and_fuel() {
        // parity: feat:modules.covert-jump-portal-generator
        eve_testkit::attest(&["feat:modules.covert-jump-portal-generator"]);
        let ctx = ServerContext::new();
        let kw = HashMap::new();
        let sys_a = 30_000_012_u32; // Asabona
        let sys_b = 30_000_014_u32; // Shamahi (2.10 LY)

        let c_cyno = 91_000_101_i64;
        let s_cyno = 101_000_101_i64;
        let c_bo = 91_000_102_i64;
        let s_bo = 101_000_102_i64;
        let bo_pos = DVec3::new(100_000.0, 0.0, 0.0);

        let mut sess_cyno = setup_pilot(&ctx, c_cyno, s_cyno, 12034, sys_b, DVec3::new(100_000.0, 0.0, 0.0), 1_000_009).await;
        let mut sess_bo = setup_pilot(&ctx, c_bo, s_bo, 22428, sys_a, bo_pos, 1_000_009).await;

        // 6 eligible covert passenger hulls + 2 ineligible hulls (Tengu WITHOUT covert subsystem & Rifter)
        let passengers = [
            (91_000_111_i64, 101_000_111_i64, 12034_u32, true),  // Stealth Bomber (Hound, 1.455M kg)
            (91_000_112_i64, 101_000_112_i64, 11188_u32, true),  // Covert Ops Frigate (Anathema)
            (91_000_113_i64, 101_000_113_i64, 11965_u32, true),  // Force Recon (Pilgrim)
            (91_000_114_i64, 101_000_114_i64, 12729_u32, true),  // Blockade Runner (Crane, 11M kg)
            (91_000_115_i64, 101_000_115_i64, 29984_u32, true),  // Strategic Cruiser (Tengu WITH 45589 Covert Reconfiguration)
            (91_000_118_i64, 101_000_118_i64, 22428_u32, true),  // Black Ops (Redeemer)
            (91_000_116_i64, 101_000_116_i64, 29984_u32, false), // Strategic Cruiser WITHOUT covert subsystem -> rejected!
            (91_000_117_i64, 101_000_117_i64, 587_u32, false),   // Non-covert Rifter -> rejected!
        ];

        let mut p_sessions = Vec::new();
        for (idx, &(cid, sid, tid, _)) in passengers.iter().enumerate() {
            let mut ps = setup_pilot(
                &ctx,
                cid,
                sid,
                tid,
                sys_a,
                bo_pos + DVec3::new(300.0 + (idx as f64) * 100.0, 0.0, 0.0),
                1_000_009,
            )
            .await;
            ps.fleet_id = Some(999);
            p_sessions.push(ps);
        }
        sess_cyno.fleet_id = Some(999);
        sess_bo.fleet_id = Some(999);

        let cyno_mod = 101_000_901_i64;
        let portal_mod = 101_000_902_i64;
        let bo_fuel_bay_id = 101_000_903_i64;
        let bo_cargo_fuel_id = 101_000_904_i64;
        let tengu_cov_sub_id = 101_000_905_i64;

        ctx.db.with_mut_snapshot(|s| {
            let mut members = vec![c_cyno, c_bo];
            members.extend(passengers.iter().map(|p| p.0));
            s.fleets.insert(
                999,
                eve_db::FleetRecord { fleet_id: 999, boss_id: c_bo, members, ..Default::default() },
            );
            s.skills.entry(c_cyno).or_default().insert(21603, TrainedSkillRecord { type_id: 21603, level: 5, skill_points: 256_000.0 });
            s.skills.entry(c_bo).or_default().insert(24562, TrainedSkillRecord { type_id: 24562, level: 5, skill_points: 256_000.0 });
            s.skills.entry(c_bo).or_default().insert(21610, TrainedSkillRecord { type_id: 21610, level: 4, skill_points: 90_510.0 }); // Jump Fuel Conservation IV
            for t3_cid in [91_000_115_i64, 91_000_116_i64] {
                s.skills.entry(t3_cid).or_default().insert(30544, TrainedSkillRecord { type_id: 30544, level: 5, skill_points: 256_000.0 }); // Caldari Defensive Systems V
            }

            s.items.insert(
                cyno_mod,
                InventoryItemRecord {
                    item_id: cyno_mod, type_id: 28646, owner_id: c_cyno, location_id: s_cyno,
                    flag: InventoryFlag::HiSlot0, quantity: 1, singleton: true, ..Default::default()
                },
            );
            s.items.insert(
                101_000_910,
                InventoryItemRecord {
                    item_id: 101_000_910, type_id: 16273, owner_id: c_cyno, location_id: s_cyno,
                    flag: InventoryFlag::Cargo, quantity: 200, singleton: false, ..Default::default()
                },
            );
            s.items.insert(
                portal_mod,
                InventoryItemRecord {
                    item_id: portal_mod, type_id: 28652, owner_id: c_bo, location_id: s_bo,
                    flag: InventoryFlag::HiSlot0, quantity: 1, singleton: true, ..Default::default()
                },
            );
            // Put Helium Isotopes (16274) in BOTH SpecializedFuelBay and Cargo to verify SpecializedFuelBay priority
            s.items.insert(
                bo_fuel_bay_id,
                InventoryItemRecord {
                    item_id: bo_fuel_bay_id, type_id: 16274, owner_id: c_bo, location_id: s_bo,
                    flag: InventoryFlag::SpecializedFuelBay, quantity: 100_000, singleton: false, ..Default::default()
                },
            );
            s.items.insert(
                bo_cargo_fuel_id,
                InventoryItemRecord {
                    item_id: bo_cargo_fuel_id, type_id: 16274, owner_id: c_bo, location_id: s_bo,
                    flag: InventoryFlag::Cargo, quantity: 20_000, singleton: false, ..Default::default()
                },
            );
            // Fit Tengu Defensive - Covert Reconfiguration (45589) ONLY on passenger #4 (101_000_115)
            s.items.insert(
                tengu_cov_sub_id,
                InventoryItemRecord {
                    item_id: tengu_cov_sub_id,
                    type_id: 45589,
                    owner_id: 91_000_115,
                    location_id: 101_000_115,
                    flag: InventoryFlag::SubSystemSlot1,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
            s.module_online_states.insert(tengu_cov_sub_id, true);
        });
        ctx.invalidate_ship_fitting(101_000_115);
        ctx.invalidate_ship_fitting(101_000_116);

        let cyno_rep = ctx
            .dispatch_rpc(&mut sess_cyno, "dogmaIM", None, "Activate", &[PyRep::Integer(cyno_mod)], &kw, None)
            .await
            .expect("Light Covert Cyno");
        let cov_beacon_id = kv_get(&cyno_rep, "beaconID").and_then(|v| v.as_i64()).unwrap();

        // Negative check: Covert Jump Portal Generator cannot bridge to a normal non-covert cyno!
        let c_norm = 91_000_199_i64;
        let s_norm = 101_000_199_i64;
        let mut sess_norm = setup_pilot(&ctx, c_norm, s_norm, 11965, sys_b, DVec3::new(300_000.0, 0.0, 0.0), 1_000_009).await;
        sess_norm.fleet_id = Some(999);
        ctx.db.with_mut_snapshot(|s| {
            s.fleets.get_mut(&999).unwrap().members.push(c_norm);
            s.skills.entry(c_norm).or_default().insert(21603, TrainedSkillRecord { type_id: 21603, level: 5, skill_points: 256_000.0 });
            s.items.insert(101_000_998, InventoryItemRecord { item_id: 101_000_998, type_id: 21096, owner_id: c_norm, location_id: s_norm, flag: InventoryFlag::HiSlot0, quantity: 1, singleton: true, ..Default::default() });
            s.items.insert(101_000_999, InventoryItemRecord { item_id: 101_000_999, type_id: 16273, owner_id: c_norm, location_id: s_norm, flag: InventoryFlag::Cargo, quantity: 500, singleton: false, ..Default::default() });
        });
        let norm_rep = ctx.dispatch_rpc(&mut sess_norm, "dogmaIM", None, "Activate", &[PyRep::Integer(101_000_998)], &kw, None).await.unwrap();
        let norm_beacon_id = kv_get(&norm_rep, "beaconID").and_then(|v| v.as_i64()).unwrap();
        let norm_bridge_err = ctx
            .dispatch_rpc(
                &mut sess_bo,
                "beyonce",
                None,
                "CmdBridgeToFleetModuleBeacon",
                &[PyRep::Integer(c_norm), PyRep::Integer(sys_b as i64), PyRep::Integer(norm_beacon_id)],
                &kw,
                None,
            )
            .await
            .expect_err("Covert Jump Portal Generator must NEVER bridge onto a normal cyno");
        assert_eq!(norm_bridge_err.code, "ShipCannotBridgeToBeacon");

        // Open Covert Jump Portal to Covert Cynosural Field
        ctx.dispatch_rpc(
            &mut sess_bo,
            "beyonce",
            None,
            "CmdBridgeToFleetModuleBeacon",
            &[PyRep::Integer(c_cyno), PyRep::Integer(sys_b as i64), PyRep::Integer(cov_beacon_id)],
            &kw,
            None,
        )
        .await
        .expect("Black Ops opens Covert Jump Portal to Covert Cyno");

        // Non-covert Tengu (#6) and Rifter (#7) are rejected with JumpBridgeRestrictedShip
        for bad_idx in [6_usize, 7_usize] {
            let err = ctx
                .dispatch_rpc(
                    &mut p_sessions[bad_idx],
                    "beyonce",
                    None,
                    "CmdJumpThroughFleet",
                    &[PyRep::Integer(c_bo), PyRep::Integer(s_bo)],
                    &kw,
                    None,
                )
                .await
                .expect_err("Non-covert hull must be rejected by Covert Jump Portal");
            assert_eq!(err.code, "JumpBridgeRestrictedShip");
        }

        // All 6 covert-capable hull classes (Stealth Bomber, Covert Ops, Force Recon, Blockade Runner, Covert Tengu, Black Ops) jump through!
        let mut fuels_by_hull = Vec::new();
        for ok_idx in 0..6_usize {
            let fb_before = ctx.db.with_snapshot(|s| s.items.get(&bo_fuel_bay_id).unwrap().quantity);
            let jump_rep = ctx
                .dispatch_rpc(
                    &mut p_sessions[ok_idx],
                    "beyonce",
                    None,
                    "CmdJumpThroughFleet",
                    &[PyRep::Integer(c_bo), PyRep::Integer(s_bo)],
                    &kw,
                    None,
                )
                .await
                .unwrap_or_else(|e| panic!("Eligible covert passenger #{ok_idx} failed: {e:?}"));
            let consumed = kv_get(&jump_rep, "fuelConsumed").and_then(|v| v.as_i64()).unwrap();
            let fb_after = ctx.db.with_snapshot(|s| s.items.get(&bo_fuel_bay_id).unwrap().quantity);
            assert_eq!((fb_before - fb_after) as i64, consumed, "Fuel deducted from Black Ops SpecializedFuelBay first");
            assert_eq!(p_sessions[ok_idx].solar_system_id, Some(sys_b));
            fuels_by_hull.push(consumed);
        }

        // Cargo isotopes untouched because SpecializedFuelBay had plenty, and heavier Crane (11M kg) used >7x more isotopes than Hound (1.455M kg)!
        assert_eq!(ctx.db.with_snapshot(|s| s.items.get(&bo_cargo_fuel_id).unwrap().quantity), 20_000);
        assert!(
            fuels_by_hull[3] > fuels_by_hull[0] * 6,
            "Isotope consumption scales with ship mass * LY: Crane={} vs Hound={}",
            fuels_by_hull[3],
            fuels_by_hull[0]
        );

        // Bubbled vs point-scrambled Black Ops host (`B39-REV-P0-01`):
        // Inside a warp disruption bubble (`warp_scramble_strength == 0`), jumping self is blocked (`WarpScrambled`),
        // whereas opening a Covert Jump Portal (`CmdBridgeToFleetModuleBeacon`) SUCCEEDS; targeted point-scramble blocks both!
        ctx.advance_secs(65);
        let cyno_rep2 = ctx.dispatch_rpc(&mut sess_cyno, "dogmaIM", None, "Activate", &[PyRep::Integer(cyno_mod)], &kw, None).await.expect("Re-light Covert Cyno");
        let cov_beacon_id2 = kv_get(&cyno_rep2, "beaconID").and_then(|v| v.as_i64()).unwrap();
        let sys_a_h = ctx.systems.get_or_spawn(sys_a);
        assert!(sys_a_h.add_interdiction_sphere(101_000_977, bo_pos, 20_000.0).await.unwrap());
        assert!(sys_a_h.is_ball_scrambled(s_bo).await.unwrap(), "Black Ops is inside warp disruption bubble");
        let bubbled_jump_err = ctx
            .dispatch_rpc(
                &mut sess_bo,
                "beyonce",
                None,
                "CmdJumpToFleetModuleBeacon",
                &[PyRep::Integer(c_cyno), PyRep::Integer(sys_b as i64), PyRep::Integer(cov_beacon_id2)],
                &kw,
                None,
            )
            .await
            .expect_err("Self-jumping out of warp disruption bubble must fail");
        assert_eq!(bubbled_jump_err.code, "WarpScrambled");

        ctx.dispatch_rpc(
            &mut sess_bo,
            "beyonce",
            None,
            "CmdBridgeToFleetModuleBeacon",
            &[PyRep::Integer(c_cyno), PyRep::Integer(sys_b as i64), PyRep::Integer(cov_beacon_id2)],
            &kw,
            None,
        )
        .await
        .expect("Non-point-scrambled Black Ops inside a warp disruption bubble opens Covert Jump Portal");

        sys_a_h.set_warp_scramble_strength(s_bo, 2).await.unwrap();
        let point_scram_err = ctx
            .dispatch_rpc(
                &mut sess_bo,
                "beyonce",
                None,
                "CmdBridgeToFleetModuleBeacon",
                &[PyRep::Integer(c_cyno), PyRep::Integer(sys_b as i64), PyRep::Integer(cov_beacon_id2)],
                &kw,
                None,
            )
            .await
            .expect_err("Targeted warp scrambler must block Covert Jump Portal activation");
        assert_eq!(point_scram_err.code, "WarpScrambled");
    }
}

pub(crate) mod abyssal_cache_spatial_clouds {
    // Integration tests for batch 42 open `feat:` parity rows.
    //
    // Covers:
    // - `feat:abyssal.bioadaptive-cache-rewards`
    // - `feat:abyssal.spatial-hazard-clouds`
    // - `feat:mining.foreman-burst-charges`
    // - `feat:modules.stasis-grappler-mechanics`
    // - `feat:navigation.micro-jump-drive-spool`
    // - `feat:navigation.warp-scramble-propulsion-shutoff`


    use crate::common;
    use eve_db::{
        FleetRecord, InventoryFlag, InventoryItemRecord, IskCents, JumpCloneRecord, TrainedSkillRecord,
    };
    use eve_destiny::{Ball, MovementMode, TargetLock, WarpBubble};
    use eve_dogma::{ModuleState, ShipFittingEngine};
    use eve_machonet::SessionState;
    use eve_proto::PyRep;
    use eve_server::combat::death_pipeline::execute_death_pipeline_sync;
    use eve_server::router::ServerContext;
    use eve_server::services::pve_special_sites::{
        evaluate_abyssal_spatial_clouds_for_ship, BIOLUMINESCENT_CLOUD_TYPE_ID,
        FILAMENT_CLOUD_TYPE_ID, TACHYON_CLOUD_TYPE_ID,
    };
    use glam::DVec3;
    use std::collections::HashMap;

    async fn setup_pilot_in_space(ctx: &ServerContext, conn_id: u64, char_id: i64, ship_id: i64, ship_type_id: u32, sys_id: u32, pos: DVec3) -> SessionState {
        let mut sess = SessionState::new(conn_id);
        let uid = (char_id - 90_000_000).max(1) as u64;
        ctx.db.with_mut_snapshot(|s| {
            if !s.characters.contains_key(&char_id) {
                let mut base = s.characters[&90_000_001].clone();
                base.character_id = char_id;
                base.name = format!("Pilot_{char_id}");
                s.characters.insert(char_id, base);
            }
            if let Some(ch) = s.characters.get_mut(&char_id) {
                ch.user_id = uid;
                ch.solar_system_id = sys_id;
                ch.active_ship_id = ship_id;
                ch.station_id = None;
                ch.structure_id = None;
                ch.clone_grade = 1;
                ch.balance = IskCents::from_isk(500_000_000);
            }
            s.items.insert(
                ship_id,
                InventoryItemRecord { item_id: ship_id, type_id: ship_type_id, owner_id: char_id, location_id: sys_id as i64, flag: InventoryFlag::None, quantity: 1, singleton: true, ..Default::default() },
            );
        });
        sess.user_id = Some(uid);
        sess.character_id = Some(char_id);
        sess.solar_system_id = Some(sys_id);
        sess.ship_id = Some(ship_id);
        let h = ctx.systems.get_or_spawn(sys_id);
        let _ = h.spawn_ball(Ball { id: ship_id, type_id: ship_type_id, position: pos, velocity: DVec3::ZERO, max_velocity: 300.0, inertia: 0.5, mass: 10_000_000.0, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop }).await;
        let _ = h.set_combat_state(ship_id, ctx.build_type_combat_state(ship_type_id)).await;
        sess
    }

    fn seed_item(ctx: &ServerContext, item_id: i64, type_id: u32, owner_id: i64, location_id: i64, flag: InventoryFlag, quantity: i32, singleton: bool) {
        ctx.db.with_mut_snapshot(|s| {
            s.items.insert(item_id, InventoryItemRecord { item_id, type_id, owner_id, location_id, flag, quantity, singleton, ..Default::default() });
            if matches!(flag as u16, 11..=34 | 92..=94 | 125..=132) {
                s.module_online_states.insert(item_id, true);
            }
        });
    }

    fn grant_skills(ctx: &ServerContext, char_id: i64, skills: &[(u32, u8)]) {
        ctx.db.with_mut_snapshot(|s| {
            let entry = s.skills.entry(char_id).or_default();
            for &(skill_type_id, level) in skills {
                entry.insert(skill_type_id, TrainedSkillRecord { type_id: skill_type_id, level, skill_points: 256_000.0 });
            }
        });
    }

    #[tokio::test]
    async fn test_b42_abyssal_cache_and_spatial_clouds() {
        let ctx = ServerContext::new();
        common::pin_clock(&ctx);
        let kw = HashMap::new();
        let char_id = 90_000_001_i64;
        let ship_id = 742_000_001_i64;
        let mut sess = setup_pilot_in_space(&ctx, 892_001, char_id, ship_id, 621, 30_000_142, DVec3::new(0.0, 2.0e9, 0.0)).await;

        // 1. Bioadaptive Cache rewards & tier-scaled HP (feat:abyssal.bioadaptive-cache-rewards)
        seed_item(&ctx, 742_100_001, 56_131, char_id, ship_id, InventoryFlag::Cargo, 1, false);
        let d0 = ctx.dispatch_rpc(&mut sess, "abyssalMgr", None, "AbyssalEntranceDeployment", &[PyRep::Integer(742_100_001)], &kw, None).await.expect("deploy T0");
        let t0 = d0.dict_get("traceBallID").and_then(|v| v.as_i64()).unwrap();
        let e0 = ctx.dispatch_rpc(&mut sess, "abyssalMgr", None, "AbyssalEntranceGateActivation", &[PyRep::Integer(t0)], &kw, None).await.expect("enter T0");
        let pid0 = e0.dict_get("pocketID").and_then(|v| v.as_i64()).unwrap();
        let sys0 = e0.dict_get("solarSystemID").and_then(|v| v.as_i64()).unwrap() as u32;
        let cache0_id = 1_896_000_000 + pid0 * 10 + 1;
        let hp_t0 = ctx.systems.get_or_spawn(sys0).get_combat_state(cache0_id).await.unwrap().expect("T0 cache").max_structure_hp;
        crate::abyssal_room_combat::assert_room_cache_loot(&ctx, char_id, 0, 1, 56_131).await;

        let char2 = 90_000_002_i64;
        let ship2 = 742_000_002_i64;
        let mut sess2 = setup_pilot_in_space(&ctx, 892_002, char2, ship2, 621, 30_002_086, DVec3::new(1.0e6, 2.0e9, 0.0)).await;
        seed_item(&ctx, 742_100_002, 47_907, char2, ship2, InventoryFlag::Cargo, 1, false);
        let d5 = ctx.dispatch_rpc(&mut sess2, "abyssalMgr", None, "AbyssalEntranceDeployment", &[PyRep::Integer(742_100_002)], &kw, None).await.expect("deploy T5");
        let t5 = d5.dict_get("traceBallID").and_then(|v| v.as_i64()).unwrap();
        let e5 = ctx.dispatch_rpc(&mut sess2, "abyssalMgr", None, "AbyssalEntranceGateActivation", &[PyRep::Integer(t5)], &kw, None).await.expect("enter T5");
        let pid5 = e5.dict_get("pocketID").and_then(|v| v.as_i64()).unwrap();
        let sys5 = e5.dict_get("solarSystemID").and_then(|v| v.as_i64()).unwrap() as u32;
        let cache5_id = 1_896_000_000 + pid5 * 10 + 1;
        let hp_t5 = ctx.systems.get_or_spawn(sys5).get_combat_state(cache5_id).await.unwrap().expect("T5 cache").max_structure_hp;
        assert!(hp_t5 > hp_t0, "T5 cache HP ({hp_t5}) > T0 cache HP ({hp_t0})");

        let h0 = ctx.systems.get_or_spawn(sys0);
        let base_vel = h0.get_ball_snapshot(ship_id).await.unwrap().unwrap().1.max_velocity;
        let cloud_id = 742_900_001_i64;
        for (idx, tid, pos) in [
            (0, TACHYON_CLOUD_TYPE_ID, DVec3::new(100_000.0, 0.0, 0.0)),
            (1, BIOLUMINESCENT_CLOUD_TYPE_ID, DVec3::new(300_000.0, 0.0, 0.0)),
            (2, FILAMENT_CLOUD_TYPE_ID, DVec3::new(400_000.0, 0.0, 0.0)),
        ] {
            let _ = h0
                .spawn_ball(Ball {
                    id: cloud_id + idx,
                    type_id: tid,
                    position: pos,
                    velocity: DVec3::ZERO,
                    max_velocity: 0.0,
                    inertia: 1.0,
                    mass: 1.0,
                    radius: 15_000.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                })
                .await;
        }
        let _ = h0.set_ball_position(ship_id, DVec3::new(101_000.0, 0.0, 0.0)).await;
        assert_eq!(evaluate_abyssal_spatial_clouds_for_ship(&ctx, sys0, ship_id).await, Some("TachyonCloud"));
        let in_tachyon_vel = h0.get_ball_snapshot(ship_id).await.unwrap().unwrap().1.max_velocity;
        assert!(in_tachyon_vel > base_vel * 2.0, "Tachyon cloud boosts velocity");

        let _ = h0.set_ball_position(ship_id, DVec3::new(200_000.0, 0.0, 0.0)).await;
        assert!(evaluate_abyssal_spatial_clouds_for_ship(&ctx, sys0, ship_id).await.is_none());
        let restored_vel = h0.get_ball_snapshot(ship_id).await.unwrap().unwrap().1.max_velocity;
        assert!((restored_vel - base_vel).abs() < 1e-3, "Leaving cloud restores velocity");

        let base_cs = h0.get_combat_state(ship_id).await.unwrap().unwrap();
        let _ = h0.set_ball_position(ship_id, DVec3::new(301_000.0, 0.0, 0.0)).await;
        assert_eq!(evaluate_abyssal_spatial_clouds_for_ship(&ctx, sys0, ship_id).await, Some("BioluminescentCloud"));
        let bio_cs = h0.get_combat_state(ship_id).await.unwrap().unwrap();
        assert!(bio_cs.signature_radius > base_cs.signature_radius * 2.0, "Bioluminescent cloud inflates signature radius");

        let _ = h0.set_ball_position(ship_id, DVec3::new(401_000.0, 0.0, 0.0)).await;
        assert_eq!(evaluate_abyssal_spatial_clouds_for_ship(&ctx, sys0, ship_id).await, Some("FilamentCloud"));
        let fil_cs = h0.get_combat_state(ship_id).await.unwrap().unwrap();
        assert!((fil_cs.shield_boost_multiplier - 0.6).abs() < 1e-3, "Filament cloud applies shield booster penalty");

        let _ = h0.set_ball_position(ship_id, DVec3::new(200_000.0, 0.0, 0.0)).await;
        assert!(evaluate_abyssal_spatial_clouds_for_ship(&ctx, sys0, ship_id).await.is_none());
        let restored_cs = h0.get_combat_state(ship_id).await.unwrap().unwrap();
        assert!((restored_cs.signature_radius - base_cs.signature_radius).abs() < 1e-3, "Leaving cloud restores signature radius");
        assert!((restored_cs.shield_boost_multiplier - 1.0).abs() < 1e-3, "Leaving cloud restores shield boost multiplier");
    }
    #[tokio::test]
    async fn test_b42_mining_foreman_burst_and_mutadaptive_rar() {
        let ctx = ServerContext::new();
        common::pin_clock(&ctx);
        let kw = HashMap::new();
        let sys_id = 30_000_142_u32;
        let c1 = 90_000_001_i64;
        let c2 = 90_000_002_i64;
        let c3 = 90_000_003_i64;
        let c4 = 90_000_004_i64;
        let orca_id = 742_010_001_i64;
        let hulk_near_id = 742_010_002_i64;
        let hulk_far_id = 742_010_003_i64;
        let hulk_noball_id = 742_010_004_i64;

        let mut sess1 = setup_pilot_in_space(&ctx, 892_010, c1, orca_id, 28_606, sys_id, DVec3::ZERO).await;
        let _sess2 = setup_pilot_in_space(&ctx, 892_011, c2, hulk_near_id, 22_544, sys_id, DVec3::new(5_000.0, 0.0, 0.0)).await;
        let _sess3 = setup_pilot_in_space(&ctx, 892_012, c3, hulk_far_id, 22_544, sys_id, DVec3::new(500_000.0, 0.0, 0.0)).await;
        ctx.db.with_mut_snapshot(|s| {
            let mut base = s.characters[&90_000_001].clone();
            base.character_id = c4;
            base.solar_system_id = sys_id;
            base.active_ship_id = hulk_noball_id;
            base.station_id = None;
            s.characters.insert(c4, base);
            s.fleets.insert(
                99_001,
                FleetRecord { fleet_id: 99_001, boss_id: c1, motd: String::new(), members: vec![c1, c2, c3, c4], ..Default::default() },
            );
        });

        grant_skills(&ctx, c1, &[(22536, 5), (22552, 5), (28609, 5), (11574, 5), (24764, 5), (3434, 5), (16069, 5)]);

        // 1. Mining Foreman Burst module (42528) + Mining Laser Optimization Charge (42830) via dogmaIM.Activate RPC
        let burst_mod_id = 742_010_101_i64;
        let charge_id = 742_010_102_i64;
        seed_item(&ctx, burst_mod_id, 42_528, c1, orca_id, InventoryFlag::HiSlot0, 1, true);
        let h = ctx.systems.get_or_spawn(sys_id);
        let no_charge_err = ctx
            .dispatch_rpc(&mut sess1, "dogmaIM", None, "Activate", &[PyRep::Integer(burst_mod_id), PyRep::String("commandBurst".into())], &kw, None)
            .await
            .expect_err("Unloaded Mining Foreman Burst must fail");
        assert_eq!(no_charge_err.code, "NoChargesLoaded");

        seed_item(&ctx, charge_id, 42_830, c1, orca_id, InventoryFlag::Cargo, 10, false);
        ctx.dispatch_rpc(&mut sess1, "dogmaIM", None, "LoadAmmo", &[PyRep::Integer(orca_id), PyRep::Integer(burst_mod_id), PyRep::Integer(charge_id)], &kw, None)
            .await
            .expect("Load Mining Laser Optimization Charge via LoadAmmo RPC");
        ctx.advance_secs(61);
        let res = ctx
            .dispatch_rpc(&mut sess1, "dogmaIM", None, "Activate", &[PyRep::Integer(burst_mod_id), PyRep::String("commandBurst".into())], &kw, None)
            .await
            .expect("Mining Foreman Burst activates with charge via RPC");
        let qty_after: i32 = ctx.db.with_snapshot(|s| s.items.values().filter(|i| i.location_id == orca_id && i.type_id == 42_830).map(|i| i.quantity).sum());
        assert_eq!(qty_after, 9, "Command burst consumes 1 charge per cycle");
        let buff_strength = res.dict_get("buffStrength").and_then(|v| v.as_f64()).unwrap_or(0.0);
        assert!(buff_strength != 0.0, "Mining Foreman Burst applies non-zero mining buff: {buff_strength}");
        let affected = res
            .dict_get("affectedShipIDs")
            .and_then(|v| match v {
                PyRep::List(l) => Some(l.iter().filter_map(|x| x.as_i64()).collect::<Vec<_>>()),
                _ => None,
            })
            .unwrap_or_default();
        assert!(affected.contains(&orca_id) && affected.contains(&hulk_near_id), "In-range fleet ships affected: {affected:?}");
        assert!(!affected.contains(&hulk_far_id), "Out-of-range fleet ship excluded: {affected:?}");
        assert!(!affected.contains(&hulk_noball_id), "Fleet ship without ball excluded: {affected:?}");

        // 2. Triglavian Mutadaptive Remote Armor Repairer (49770) spool-up (feat:modules.mutadaptive-remote-armor-repairer)
        let mut_rar_id = 742_010_201_i64;
        seed_item(&ctx, mut_rar_id, 49_770, c1, orca_id, InventoryFlag::HiSlot1, 1, true);
        let _ = h
            .add_target_lock(
                orca_id,
                TargetLock { target_id: hulk_near_id, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(50_000.0) },
            )
            .await;
        if let Ok(Some(mut cs)) = h.get_combat_state(hulk_near_id).await {
            cs.armor_hp = 100.0;
            cs.max_armor_hp = 50_000.0;
            let _ = h.set_combat_state(hulk_near_id, cs).await;
        }
        ctx.dispatch_rpc(
            &mut sess1,
            "dogmaIM",
            None,
            "Activate",
            &[PyRep::Integer(mut_rar_id), PyRep::String("shipModuleRemoteArmorMutadaptiveRepairer".into()), PyRep::Integer(hulk_near_id), PyRep::Integer(1)],
            &kw,
            None,
        )
        .await
        .expect("Activate Mutadaptive RAR");
        for _ in 0..7 {
            h.step_manual(1.0).await.unwrap();
        }
        let armor_after_1 = h.get_combat_state(hulk_near_id).await.unwrap().unwrap().armor_hp;
        for _ in 0..7 {
            h.step_manual(1.0).await.unwrap();
        }
        let armor_after_2 = h.get_combat_state(hulk_near_id).await.unwrap().unwrap().armor_hp;
        let heal_1 = armor_after_1 - 100.0;
        let heal_2 = armor_after_2 - armor_after_1;
        assert!(heal_1 > 0.0 && heal_2 > heal_1, "Mutadaptive RAR spools up across cycles: c1={heal_1}, c2={heal_2}");
    }

    #[tokio::test]
    async fn test_b42_overheat_rack_heat_and_damage_propagation() {
        let ctx = ServerContext::new();
        let ship_id = 742_020_001_i64;
        let mut fit_no_skill = ShipFittingEngine::from_sde(ship_id, 638, &ctx.sde);
        let mut fit_thermo5 = ShipFittingEngine::from_sde(ship_id, 638, &ctx.sde);
        for &(sid, lvl) in &[(3302, 5), (3300, 5), (11084, 5)] {
            fit_no_skill.character_skills.insert(sid, lvl);
            fit_thermo5.character_skills.insert(sid, lvl);
        }
        fit_thermo5.character_skills.insert(28_164, 5);

        for (idx, flag) in [(0, 27u16), (1, 28), (2, 29), (3, 33)] {
            let mid = 742_020_100 + idx;
            fit_no_skill.fit_module_with_state(mid, 2_889, flag, true, &ctx.sde).unwrap();
            fit_thermo5.fit_module_with_state(mid, 2_889, flag, true, &ctx.sde).unwrap();
        }

        fit_no_skill.activate_module(742_020_100, true, &ctx.sde).unwrap();
        fit_thermo5.activate_module(742_020_100, true, &ctx.sde).unwrap();
        assert_eq!(fit_no_skill.modules[&742_020_100].state, ModuleState::Overloaded);

        let r0 = fit_no_skill.apply_heat_cycle(742_020_100, 0.12, 0.01, 4.0, 0.05, &ctx.sde).unwrap();
        let r5 = fit_thermo5.apply_heat_cycle(742_020_100, 0.12, 0.01, 4.0, 0.05, &ctx.sde).unwrap();

        let expected_heat = eve_dogma::thermodynamics::step_rack_heat(0.0, 0.12, 0.01, 4.0);
        assert!((r0.rack_heat - expected_heat).abs() < 1e-6 && r0.rack_heat > 0.0 && r0.rack_heat < 1.0);

        let d_self = fit_no_skill.modules[&742_020_100].damage_hp;
        let d_adj1 = fit_no_skill.modules[&742_020_101].damage_hp;
        let d_adj2 = fit_no_skill.modules[&742_020_102].damage_hp;
        let d_edge = fit_no_skill.modules[&742_020_103].damage_hp;
        assert!(d_self > d_adj1 && d_adj1 > 0.0 && d_adj2 > 0.0, "Heat conducts to adjacent slots");
        assert!(eve_dogma::thermodynamics::heat_damage_probability(r0.rack_heat, 1, 0.5) > eve_dogma::thermodynamics::heat_damage_probability(r0.rack_heat, 2, 0.5));
        assert_eq!(d_edge, 0.0, "Heat conduction must not wrap around rack edges");
        assert!(r5.module_damage < r0.module_damage, "Thermodynamics V reduces heat damage: {} < {}", r5.module_damage, r0.module_damage);
    }
    #[tokio::test]
    async fn test_b42_stasis_grappler_and_mjd_spool() {
        // parity: feat:modules.stasis-grappler-mechanics
        eve_testkit::attest(&[
            "feat:modules.stasis-grappler-mechanics",
        ]);
        let ctx = ServerContext::new();
        common::pin_clock(&ctx);
        let kw = HashMap::new();
        let sys_id = 30_000_142_u32;
        let char_id = 90_000_001_i64;
        let raven_id = 742_030_001_i64;
        let vexor_id = 742_030_002_i64;
        let target_id = 742_030_003_i64;
        let station_id = 60_003_760_i64;

        let mut sess = setup_pilot_in_space(&ctx, 892_030, char_id, raven_id, 638, sys_id, DVec3::ZERO).await;
        let mut sess_t = setup_pilot_in_space(&ctx, 892_031, 90_000_002, target_id, 626, sys_id, DVec3::new(1_000.0, 0.0, 0.0)).await;
        seed_item(&ctx, vexor_id, 626, char_id, station_id, InventoryFlag::Hangar, 1, true);
        let g1 = 742_030_101_i64;
        let g2 = 742_030_102_i64;
        seed_item(&ctx, g1, 41_057, char_id, station_id, InventoryFlag::Hangar, 1, true);
        seed_item(&ctx, g2, 41_057, char_id, station_id, InventoryFlag::Hangar, 1, true);
        grant_skills(&ctx, char_id, &[(3434, 5), (4385, 5), (3449, 5)]);
        grant_skills(&ctx, 90_000_002, &[(3434, 5)]);

        // 1. Stasis Grappler hull & max-1 restrictions, velocity reduction, and falloff (feat:modules.stasis-grappler-mechanics)
        let mut cruiser_fit = ShipFittingEngine::from_sde(vexor_id, 626, &ctx.sde);
        assert!(cruiser_fit.fit_module(g1, 41_057, 19, &ctx.sde).is_err(), "Cruiser cannot fit Heavy Stasis Grappler");
        let mut bs_fit = ShipFittingEngine::from_sde(raven_id, 638, &ctx.sde);
        bs_fit.character_skills.insert(3434, 5);
        bs_fit.character_skills.insert(3453, 5);
        bs_fit.fit_module(g1, 41_057, 19, &ctx.sde).expect("Battleship fits 1 Heavy Stasis Grappler");
        assert!(bs_fit.fit_module(g2, 41_057, 20, &ctx.sde).is_err(), "Max 1 Stasis Grappler per ship");

        let h = ctx.systems.get_or_spawn(sys_id);
        seed_item(&ctx, g1, 41_057, char_id, raven_id, InventoryFlag::MedSlot0, 1, true);
        let _ = h
            .add_target_lock(
                raven_id,
                TargetLock { target_id, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(50_000.0) },
            )
            .await;
        let base_target_vel = h.get_ball_snapshot(target_id).await.unwrap().unwrap().1.max_velocity;
        let _ = h.set_ball_position(target_id, DVec3::new(500.0, 0.0, 0.0)).await;
        ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(g1), PyRep::String("decreaseTargetSpeed".into()), PyRep::Integer(target_id)], &kw, None)
            .await
            .expect("Activate Heavy Stasis Grappler at close range");
        let vel_close = h.get_ball_snapshot(target_id).await.unwrap().unwrap().1.max_velocity;
        assert!(vel_close < base_target_vel * 0.25, "Stasis Grappler reduces target velocity by ~85% at optimal: {vel_close} vs {base_target_vel}");

        let _ = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Deactivate", &[PyRep::Integer(g1), PyRep::String("decreaseTargetSpeed".into())], &kw, None).await;
        let _ = h.set_ball_position(target_id, DVec3::new(1_000.0, 0.0, 0.0)).await;
        let _ = h.set_ball_position(target_id, DVec3::new(15_000.0, 0.0, 0.0)).await;
        ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(g1), PyRep::String("decreaseTargetSpeed".into()), PyRep::Integer(target_id)], &kw, None)
            .await
            .expect("Activate Heavy Stasis Grappler in falloff");
        let vel_falloff = h.get_ball_snapshot(target_id).await.unwrap().unwrap().1.max_velocity;
        assert!(vel_falloff > vel_close && vel_falloff < base_target_vel, "Stasis Grappler decays in falloff: close={vel_close}, falloff={vel_falloff}, base={base_target_vel}");
        let _ = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Deactivate", &[PyRep::Integer(g1), PyRep::String("decreaseTargetSpeed".into())], &kw, None).await;
        let _ = h.set_ball_position(target_id, DVec3::new(1_000.0, 0.0, 0.0)).await;

        // 2. Micro Jump Drive 12s spool, scram cancellation, and 100 km vector jump (feat:navigation.micro-jump-drive-spool)
        let mjd_id = 742_030_201_i64;
        let scram_id = 742_030_202_i64;
        seed_item(&ctx, mjd_id, 4_383, char_id, raven_id, InventoryFlag::MedSlot2, 1, true);
        seed_item(&ctx, scram_id, 448, 90_000_002, target_id, InventoryFlag::MedSlot0, 1, true);
        let _ = h
            .add_target_lock(
                target_id,
                TargetLock { target_id: raven_id, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(50_000.0) },
            )
            .await;
        ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(mjd_id), PyRep::String("microJumpDrive".into())], &kw, None)
            .await
            .expect("Activate MJD");
        ctx.dispatch_rpc(&mut sess_t, "dogmaIM", None, "Activate", &[PyRep::Integer(scram_id), PyRep::String("warpScramble".into()), PyRep::Integer(raven_id)], &kw, None)
            .await
            .expect("Activate Warp Scrambler");
        for _ in 0..13 {
            h.step_manual(1.0).await.unwrap();
        }
        let pos_after_scram = h.get_ball_snapshot(raven_id).await.unwrap().unwrap().1.position;
        assert!(pos_after_scram.length() < 1_000.0, "Scram during spool cancels MJD jump even past 12s: {pos_after_scram:?}");

        let _ = ctx.dispatch_rpc(&mut sess_t, "dogmaIM", None, "Deactivate", &[PyRep::Integer(scram_id), PyRep::String("warpScramble".into())], &kw, None).await;
        ctx.advance_secs(200);
        h.step_manual(1.0).await.unwrap();
        let start_pos = h.get_ball_snapshot(raven_id).await.unwrap().unwrap().1.position;
        ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(mjd_id), PyRep::String("microJumpDrive".into())], &kw, None)
            .await
            .expect("Activate MJD unscrambled");
        for _ in 0..11 {
            h.step_manual(1.0).await.unwrap();
        }
        let mid_pos = h.get_ball_snapshot(raven_id).await.unwrap().unwrap().1.position;
        assert!((mid_pos - start_pos).length() < 5_000.0, "MJD must not jump before 12s spool completes");
        for _ in 0..2 {
            h.step_manual(1.0).await.unwrap();
        }
        let end_pos = h.get_ball_snapshot(raven_id).await.unwrap().unwrap().1.position;
        assert!((end_pos - start_pos).length() >= 95_000.0, "MJD jumps ~100 km forward at 12s: start={start_pos:?}, end={end_pos:?}");
    }
    #[tokio::test]
    async fn test_b42_warp_landing_bubbles_and_scram_shutoff() {
        // parity: feat:navigation.warp-scramble-propulsion-shutoff
        eve_testkit::attest(&[
            "feat:navigation.warp-scramble-propulsion-shutoff",
        ]);
        let ctx = ServerContext::new();
        common::pin_clock(&ctx);
        let kw = HashMap::new();
        let sys_id = 30_000_142_u32;
        let char_id = 90_000_001_i64;
        let ship_id = 742_040_001_i64;
        let attacker_id = 742_040_002_i64;

        let mut sess = setup_pilot_in_space(&ctx, 892_040, char_id, ship_id, 621, sys_id, DVec3::ZERO).await;
        let mut sess_att = setup_pilot_in_space(&ctx, 892_041, 90_000_002, attacker_id, 621, sys_id, DVec3::new(5_000.0, 0.0, 0.0)).await;
        grant_skills(&ctx, char_id, &[(3450, 5), (3449, 5)]);
        grant_skills(&ctx, 90_000_002, &[(3434, 5)]);
        let h = ctx.systems.get_or_spawn(sys_id);

        // 1. Minimum 150 km warp distance & landing offset (feat:navigation.warp-landing-points)
        let st_pos = ctx.sde.stations.get(&60_003_760).map(|s| s.position).unwrap_or(DVec3::new(1.0e7, 0.0, 0.0));
        let _ = h.set_ball_position(ship_id, st_pos + DVec3::new(50_000.0, 0.0, 0.0)).await;
        let beyonce = ctx.bind_object(&mut sess, "beyonce", sys_id as i64).unwrap();
        let short_err = ctx
            .dispatch_rpc(
                &mut sess,
                "beyonce",
                Some(beyonce),
                "CmdWarpToStuff",
                &[PyRep::String("item".into()), PyRep::Integer(60_003_760)],
                &kw,
                None,
            )
            .await
            .expect_err("Warp < 150 km must be refused");
        assert_eq!(short_err.code, "WarpDistanceTooShort");

        let _ = h.set_ball_position(ship_id, st_pos + DVec3::new(1_000_000.0, 0.0, 0.0)).await;
        let mut warp_kw = HashMap::new();
        warp_kw.insert("minRange".to_string(), PyRep::Real(50_000.0));
        let _ = ctx
            .dispatch_rpc(
                &mut sess,
                "beyonce",
                Some(beyonce),
                "CmdWarpToStuff",
                &[PyRep::String("item".into()), PyRep::Integer(60_003_760)],
                &warp_kw,
                None,
            )
            .await
            .expect("Warp >= 150 km with 50 km offset succeeds");

        // 2. Drag bubble (within 500 km behind dest) & Catch bubble (in front) (feat:navigation.warp-bubble-drag-and-catch)
        let origin = DVec3::ZERO;
        let dest = DVec3::new(1_000_000.0, 0.0, 0.0);
        let catch_bubble = WarpBubble { id: 1, position: DVec3::new(600_000.0, 0.0, 0.0), radius: 20_000.0 };
        let catch_pt = catch_bubble.intercept_warp(origin, dest).expect("Catch bubble intercepts warp in front");
        assert!((catch_pt.x - 580_000.0).abs() < 1e-3);

        let drag_bubble = WarpBubble { id: 2, position: DVec3::new(1_300_000.0, 0.0, 0.0), radius: 20_000.0 };
        let drag_pt = drag_bubble.intercept_warp(origin, dest).expect("Drag bubble within 500 km behind dest drags warp");
        assert!((drag_pt.x - 1_280_000.0).abs() < 1e-3);

        let far_bubble = WarpBubble { id: 3, position: DVec3::new(1_600_000.0, 0.0, 0.0), radius: 20_000.0 };
        assert!(far_bubble.intercept_warp(origin, dest).is_none(), "Bubble > 500 km behind dest does not drag");

        // 3. Warp Scrambler shuts off MWD vs Warp Disruptor leaves MWD running (feat:navigation.warp-scramble-propulsion-shutoff)
        let _ = h.set_ball_position(ship_id, DVec3::ZERO).await;
        let mwd_id = 742_040_101_i64;
        let point_id = 742_040_102_i64;
        let scram_id = 742_040_103_i64;
        seed_item(&ctx, mwd_id, 434, char_id, ship_id, InventoryFlag::MedSlot0, 1, true);
        seed_item(&ctx, point_id, 3_242, 90_000_002, attacker_id, InventoryFlag::MedSlot0, 1, true);
        seed_item(&ctx, scram_id, 448, 90_000_002, attacker_id, InventoryFlag::MedSlot1, 1, true);
        let _ = h
            .add_target_lock(
                attacker_id,
                TargetLock { target_id: ship_id, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(50_000.0) },
            )
            .await;
        ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(mwd_id), PyRep::String("moduleBonusMicrowarpdrive".into())], &kw, None)
            .await
            .expect("Activate MWD");
        ctx.dispatch_rpc(&mut sess_att, "dogmaIM", None, "Activate", &[PyRep::Integer(point_id), PyRep::String("warpDisrupt".into()), PyRep::Integer(ship_id)], &kw, None)
            .await
            .expect("Activate Warp Disruptor");
        h.step_manual(1.0).await.unwrap();
        let cs_disrupted = h.get_combat_state(ship_id).await.unwrap().unwrap();
        assert_eq!(cs_disrupted.warp_scramble_strength, 1);
        assert!(!cs_disrupted.is_mwd_mjd_disabled, "Long point must NOT disable MWD");

        ctx.dispatch_rpc(&mut sess_att, "dogmaIM", None, "Activate", &[PyRep::Integer(scram_id), PyRep::String("warpScramble".into()), PyRep::Integer(ship_id)], &kw, None)
            .await
            .expect("Activate Warp Scrambler");
        h.step_manual(1.0).await.unwrap();
        let cs_scrambled = h.get_combat_state(ship_id).await.unwrap().unwrap();
        assert_eq!(cs_scrambled.warp_scramble_strength, 3);
        assert!(cs_scrambled.is_mwd_mjd_disabled, "Warp Scrambler must disable MWD");
        let post_scram_vel = h.get_ball_snapshot(ship_id).await.unwrap().unwrap().1.max_velocity;
        assert!(post_scram_vel < 350.0, "Warp Scrambler shuts off active MWD cycle and restores base velocity: {post_scram_vel}");
        let reactivate_err = ctx
            .dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(mwd_id), PyRep::String("moduleBonusMicrowarpdrive".into())], &kw, None)
            .await;
        assert!(reactivate_err.is_err(), "MWD reactivation must be refused while scrambled");
    }

    #[tokio::test]
    async fn test_b42_zarzakh_and_ship_mechanics() {
        let ctx = ServerContext::new();
        common::pin_clock(&ctx);
        let kw = HashMap::new();
        let char_id = 90_000_001_i64;
        let ship_id = 742_050_001_i64;
        let turnur_sys = 30_002_086_u32;
        let zarzakh_sys = 30_100_000_u32;
        let turnur_gate = 50_016_562_i64;
        let alsa_exit_gate = 50_016_567_i64;

        let turnur_pos = ctx.sde.jumps.get(&turnur_gate).map(|j| j.position).unwrap_or(DVec3::ZERO);
        let mut sess = setup_pilot_in_space(&ctx, 892_050, char_id, ship_id, 621, turnur_sys, turnur_pos + DVec3::new(100.0, 0.0, 0.0)).await;

        // 1. Zarzakh gate toll & emanation lock (feat:navigation.zarzakh-gate-toll, feat:navigation.zarzakh-emanation-lock)
        let beyonce_t = ctx.bind_object(&mut sess, "beyonce", turnur_sys as i64).unwrap();
        let mut decline_kw = HashMap::new();
        decline_kw.insert("confirm_toll".to_string(), PyRep::Bool(false));
        let _ = ctx
            .dispatch_rpc(&mut sess, "beyonce", Some(beyonce_t), "CmdStargateJump", &[PyRep::Integer(turnur_gate)], &decline_kw, None)
            .await
            .expect_err("Declining Zarzakh toll must abort jump");

        let bal_before = ctx.db.with_snapshot(|s| s.characters[&char_id].balance);
        let mut confirm_kw = HashMap::new();
        confirm_kw.insert("confirm_toll".to_string(), PyRep::Bool(true));
        ctx.dispatch_rpc(&mut sess, "beyonce", Some(beyonce_t), "CmdStargateJump", &[PyRep::Integer(turnur_gate)], &confirm_kw, None)
            .await
            .expect("Confirmed Zarzakh jump succeeds");
        let (bal_after, has_journal, lock_entry) = ctx.db.with_snapshot(|s| {
            (
                s.characters[&char_id].balance,
                s.wallet_journal.iter().any(|j| j.character_id == char_id && j.entry_type == "zarzakh_gate_toll"),
                s.zarzakh_gate_locks.get(&char_id).copied(),
            )
        });
        assert!(bal_after < bal_before && has_journal, "Zarzakh toll debited and journaled");
        assert!(lock_entry.is_some(), "Zarzakh emanation lock recorded on entry");

        let alsa_pos = ctx.sde.jumps.get(&alsa_exit_gate).map(|j| j.position).unwrap_or(DVec3::ZERO);
        let _ = ctx.systems.get_or_spawn(zarzakh_sys).set_ball_position(ship_id, alsa_pos + DVec3::new(100.0, 0.0, 0.0)).await;
        let beyonce_z = ctx.bind_object(&mut sess, "beyonce", zarzakh_sys as i64).unwrap();
        let _ = ctx
            .dispatch_rpc(&mut sess, "beyonce", Some(beyonce_z), "CmdStargateJump", &[PyRep::Integer(alsa_exit_gate)], &kw, None)
            .await
            .expect_err("Different Zarzakh exit gate refused while locked");

        let dest_station = 60_003_760_i64;
        ctx.db.with_mut_snapshot(|s| {
            s.jump_clones.entry(char_id).or_default().push(JumpCloneRecord {
                jump_clone_id: 742_999,
                character_id: char_id,
                location_id: dest_station,
                implants: Vec::new(),
            });
        });
        grant_skills(&ctx, char_id, &[(24_242, 3)]);
        let pod_id = 742_050_099_i64;
        seed_item(&ctx, pod_id, 670, char_id, dest_station, InventoryFlag::Hangar, 1, true);
        ctx.db.with_mut_snapshot(|s| {
            if let Some(ch) = s.characters.get_mut(&char_id) {
                ch.station_id = Some(dest_station);
                ch.active_ship_id = pod_id;
            }
        });
        sess.station_id = Some(dest_station);
        sess.ship_id = Some(pod_id);
        ctx.advance_secs(15);
        let _ = ctx
            .dispatch_rpc(&mut sess, "jumpCloneSvc", None, "CloneJump", &[PyRep::Integer(742_999)], &kw, None)
            .await
            .expect("CloneJump");
        assert!(ctx.db.with_snapshot(|s| !s.zarzakh_gate_locks.contains_key(&char_id)), "CloneJump clears Zarzakh emanation lock");
        ctx.db.with_mut_snapshot(|s| {
            s.zarzakh_gate_locks.insert(char_id, (50_016_566, ctx.now_unix_secs() + 21_600));
        });
        let _ = execute_death_pipeline_sync(&ctx, zarzakh_sys, char_id, pod_id, 90_000_002, DVec3::ZERO, 1);
        assert!(ctx.db.with_snapshot(|s| !s.zarzakh_gate_locks.contains_key(&char_id)), "Pod death clears Zarzakh emanation lock");

        // 2. Blockade Runner cargo scan immunity (feat:ships.blockade-runner-cargo-scan-immunity)
        let scanner_ship = 742_050_010_i64;
        let br_ship = 742_050_011_i64;
        let scanner_mod = 742_050_012_i64;
        let mut sess_scan = setup_pilot_in_space(&ctx, 892_051, char_id, scanner_ship, 621, 30_000_142, DVec3::ZERO).await;
        let _ = setup_pilot_in_space(&ctx, 892_052, 90_000_002, br_ship, 12_733, 30_000_142, DVec3::new(1_000.0, 0.0, 0.0)).await;
        seed_item(&ctx, scanner_mod, 442, char_id, scanner_ship, InventoryFlag::MedSlot0, 1, true);
        seed_item(&ctx, 742_050_013, 34, 90_000_002, br_ship, InventoryFlag::Cargo, 500, false);
        let jita_h = ctx.systems.get_or_spawn(30_000_142);
        let _ = jita_h
            .add_target_lock(
                scanner_ship,
                TargetLock { target_id: br_ship, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(50_000.0) },
            )
            .await;
        let scan_res = ctx
            .dispatch_rpc(&mut sess_scan, "dogmaIM", None, "Activate", &[PyRep::Integer(scanner_mod), PyRep::String("cargoScan".into()), PyRep::Integer(br_ship)], &kw, None)
            .await
            .expect("Cargo scanner activates on Blockade Runner");
        assert_eq!(scan_res.dict_get("cargoScanImmune").and_then(|v| v.as_bool()), Some(true));
        assert_eq!(scan_res.dict_get("totalScannedQuantity").and_then(|v| v.as_i64()), Some(0));

        // 3. Covert Cyno & Normal Cyno beacon spawning (feat:ships.covert-cyno-beacon, feat:ships.cynosural-beacon-spawning)
        let recon_ship = 742_050_020_i64;
        let covert_cyno_mod = 742_050_021_i64;
        let mut sess_cov = setup_pilot_in_space(&ctx, 892_053, char_id, recon_ship, 11_957, turnur_sys, DVec3::new(50_000.0, 0.0, 0.0)).await;
        seed_item(&ctx, covert_cyno_mod, 28_646, char_id, recon_ship, InventoryFlag::HiSlot0, 1, true);
        seed_item(&ctx, 742_050_023, 16_273, char_id, recon_ship, InventoryFlag::Cargo, 500, false);
        grant_skills(&ctx, char_id, &[(21_603, 5), (22_761, 5), (24_242, 3)]);
        ctx.db.with_mut_snapshot(|s| {
            s.fleets.insert(99_002, FleetRecord { fleet_id: 99_002, boss_id: char_id, members: vec![char_id], ..Default::default() });
        });
        let _ = ctx
            .dispatch_rpc(&mut sess_cov, "dogmaIM", None, "Activate", &[PyRep::Integer(covert_cyno_mod), PyRep::String("cynosuralGeneration".into())], &kw, None)
            .await
            .expect("Covert cyno activates without consuming ozone");
        let ozone_after_covert: i32 = ctx.db.with_snapshot(|s| {
            s.items.values().filter(|i| i.location_id == recon_ship && i.type_id == 16_273).map(|i| i.quantity).sum()
        });
        assert_eq!(ozone_after_covert, 495, "Covert cyno on Force Recon consumes 5 Liquid Ozone (50 base * 0.2 role * 0.5 CFT V)");

        let recon_ship2 = 742_050_025_i64;
        let normal_cyno_mod = 742_050_022_i64;
        let mut sess_norm = setup_pilot_in_space(&ctx, 892_054, 90_000_003, recon_ship2, 11_957, turnur_sys, DVec3::new(60_000.0, 0.0, 0.0)).await;
        seed_item(&ctx, normal_cyno_mod, 21_096, 90_000_003, recon_ship2, InventoryFlag::HiSlot0, 1, true);
        seed_item(&ctx, 742_050_026, 16_273, 90_000_003, recon_ship2, InventoryFlag::Cargo, 500, false);
        grant_skills(&ctx, 90_000_003, &[(21_603, 5), (22_761, 5)]);
        ctx.db.with_mut_snapshot(|s| {
            s.fleets.insert(99_003, FleetRecord { fleet_id: 99_003, boss_id: 90_000_003, members: vec![90_000_003], ..Default::default() });
        });
        let _ = ctx
            .dispatch_rpc(&mut sess_norm, "dogmaIM", None, "Activate", &[PyRep::Integer(normal_cyno_mod), PyRep::String("cynosuralGeneration".into())], &kw, None)
            .await
            .expect("Normal cyno activates");
        let ozone_after_normal: i32 = ctx.db.with_snapshot(|s| {
            s.items.values().filter(|i| i.location_id == recon_ship2 && i.type_id == 16_273).map(|i| i.quantity).sum()
        });
        assert!(ozone_after_normal < 500, "Normal cyno consumes Liquid Ozone: {ozone_after_normal} < 500");

        // 4. Frigate Escape Bay on Battleship destruction (feat:ships.frigate-escape-bay)
        let bs_id = 742_050_030_i64;
        let esc_frig_id = 742_050_031_i64;
        let _ = setup_pilot_in_space(&ctx, 892_055, 90_000_004, bs_id, 638, turnur_sys, DVec3::new(80_000.0, 0.0, 0.0)).await;
        seed_item(&ctx, esc_frig_id, 587, 90_000_004, bs_id, InventoryFlag::from_u16(179), 1, true);
        let death_out = execute_death_pipeline_sync(&ctx, turnur_sys, 90_000_004, bs_id, char_id, DVec3::new(80_000.0, 0.0, 0.0), 1)
            .expect("BS death pipeline");
        assert_eq!(death_out.ejected_capsule_id, Some(esc_frig_id), "Pilot placed into stored escape frigate instead of capsule");
        ctx.db.with_snapshot(|s| {
            assert_eq!(s.characters[&90_000_004].active_ship_id, esc_frig_id, "Active ship is now the escape frigate");
            assert_eq!(s.items[&esc_frig_id].location_id, turnur_sys as i64, "Escape frigate is in space, not in wreck");
        });
    }
}
