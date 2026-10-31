//! Remediation integration tests for `BF-03` (`posMgr.AnchorOrbital` undocked check, player corp &
//! Director/Station Manager role check, planet-in-system + range check, and effective Pochven
//! Triglavian standing with Diplomacy / Connections), `BF-04` (alliance-granted structure control
//! and corporation/alliance block revocation), and `BF-05` / `P2-02` (Mobile deployable 50 km Upwell
//! clearance, Mobile Cynosural Inhibitor high-sec/wormhole/Zarzakh ban, and MMJU lifetime).

use eve_db::{
    AllianceRecord, CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents,
    TrainedSkillRecord, UpwellStructureRecord,
};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use eve_server::services::mobile_deployables::{
    MMJU_LIFETIME_SECS, TYPE_MOBILE_CYNO_INHIBITOR, TYPE_MOBILE_MJU,
};
use eve_server::services::station_structure::structure_lifecycle as life;
use glam::DVec3;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

const OWNER: i64 = 90_000_001;
const ALLY_GUNNER: i64 = 90_000_071;
const NPC_PILOT: i64 = 90_000_072;
const LINE_MEMBER: i64 = 90_000_073;
const RANCER: u32 = 30_002_718; // Low-sec (sec < 0.45)
const PERIMETER: u32 = 30_000_144; // High-sec (sec >= 0.45)
const JITA_PLANET_ID: i64 = 40_009_082; // Jita IV (cross-system planet)
const WORMHOLE_SYS: u32 = 31_000_100; // W-space
const ZARZAKH_SYS: u32 = 30_100_000; // Zarzakh

async fn rpc(ctx: &ServerContext, sess: &mut SessionState, service: &str, method: &str, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None).await
}

fn kv_i64(rep: &PyRep, key: &str) -> i64 {
    rep.dict_get(key).and_then(|v| v.as_i64()).unwrap_or(0)
}

fn new_ctx() -> Arc<ServerContext> {
    let ctx = Arc::new(ServerContext::new());
    ctx.set_clock(Arc::new(eve_db::VirtualServerClock::with_fixed_base(1_800_000_000)));
    ctx
}

fn ensure_account(snap: &mut eve_db::WorldStateSnapshot, uid: u64) {
    if !snap.accounts.values().any(|a| a.user_id == uid) {
        if let Some(mut acc) = snap.accounts.get("demo").cloned() {
            acc.user_id = uid;
            acc.username = format!("pilot_{uid}");
            snap.accounts.insert(acc.username.clone(), acc);
        }
    }
}

async fn spawn_ship_ball(ctx: &ServerContext, sys_id: u32, ship_id: i64, pos: DVec3) {
    ctx.systems.get_or_spawn(sys_id).spawn_ball(Ball {
        id: ship_id,
        type_id: 587,
        position: pos,
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 1.0,
        mass: 1_000_000.0,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    }).await.expect("spawn_ball");
}

#[tokio::test]
async fn test_bf03_anchor_orbital_undocked_corp_planet_and_effective_pochven_standing() {
    // parity: feat:structures.pochven-anchoring-restrictions, feat:pi.customs-office-gantry-deploy-reinforce, rpc:posMgr.AnchorOrbital
    eve_testkit::attest(&[
        "feat:structures.pochven-anchoring-restrictions",
        "feat:pi.customs-office-gantry-deploy-reinforce",
        "rpc:posMgr.AnchorOrbital",
    ]);

    let ctx = new_ctx();
    let corp_id = 98_903_001;
    let dir_ship = 890_003_001;
    let npc_ship = 890_003_002;
    let line_ship = 890_003_003;

    let rancer_planet = ctx.sde.celestials_in_system(RANCER).into_iter().find(|c| c.group_id == 7).expect("Rancer planet");
    let near_rancer_planet = rancer_planet.position + DVec3::new(rancer_planet.radius + 5_000.0, 0.0, 0.0);

    let (poch_sys_id, poch_planets) = ctx
        .sde
        .solar_systems
        .values()
        .filter(|s| s.region_id == 10_000_070)
        .find_map(|s| {
            let ps: Vec<_> = ctx.sde.celestials_in_system(s.solar_system_id).into_iter().filter(|c| c.group_id == 7).collect();
            (ps.len() >= 2).then_some((s.solar_system_id, ps))
        })
        .expect("Pochven system with >= 2 planets");

    ctx.db.with_mut_snapshot(|snap| {
        let mut corp = CorporationRecord::default();
        corp.corporation_id = corp_id;
        corp.name = "Orbital Hardening Corp".into();
        corp.ticker = "ORBH".into();
        corp.ceo_id = OWNER;
        corp.members.insert(OWNER);
        corp.members.insert(LINE_MEMBER);
        corp.member_roles.insert(OWNER, eve_db::corp_roles::ROLE_DIRECTOR);
        snap.corporations.insert(corp_id, corp);

        for (cid, ccorp, shid) in [
            (OWNER, corp_id, dir_ship),
            (NPC_PILOT, 1_000_009, npc_ship),
            (LINE_MEMBER, corp_id, line_ship),
        ] {
            let uid = cid as u64;
            ensure_account(snap, uid);
            let mut ch = snap.characters.get(&OWNER).cloned().unwrap();
            ch.character_id = cid;
            ch.user_id = uid;
            ch.corporation_id = ccorp;
            ch.solar_system_id = RANCER;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = shid;
            ch.balance = IskCents::from_isk(50_000_000);
            snap.characters.insert(cid, ch);

            snap.items.insert(shid, InventoryItemRecord {
                item_id: shid, type_id: 587, owner_id: cid, location_id: i64::from(RANCER),
                flag: InventoryFlag::None, quantity: 1, singleton: true, version: 1, ..Default::default()
            });
        }
        for idx in 0..8_i64 {
            let gid = 890_003_100 + idx;
            let (owner, loc) = match idx {
                1 => (NPC_PILOT, npc_ship),
                2 => (LINE_MEMBER, line_ship),
                _ => (OWNER, dir_ship),
            };
            snap.items.insert(gid, InventoryItemRecord {
                item_id: gid, type_id: 3962, owner_id: owner, location_id: loc,
                flag: InventoryFlag::Cargo, quantity: 1, singleton: false, version: 1, ..Default::default()
            });
        }
    });

    spawn_ship_ball(&ctx, RANCER, dir_ship, near_rancer_planet).await;
    spawn_ship_ball(&ctx, RANCER, npc_ship, near_rancer_planet).await;
    spawn_ship_ball(&ctx, RANCER, line_ship, near_rancer_planet).await;

    let mut dir_sess = SessionState::new(9301);
    dir_sess.user_id = Some(OWNER as u64);
    dir_sess.character_id = Some(OWNER);
    dir_sess.corporation_id = Some(corp_id);
    dir_sess.corp_role = eve_db::corp_roles::ROLE_DIRECTOR;
    dir_sess.solar_system_id = Some(RANCER);
    dir_sess.ship_id = Some(dir_ship);

    // 1. Docked outside Pochven -> MUST fail with NotInSpace!
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&OWNER).unwrap().station_id = Some(60_000_001);
    });
    let docked_err = rpc(&ctx, &mut dir_sess, "posMgr", "AnchorOrbital", &[PyRep::Integer(890_003_100), PyRep::Integer(rancer_planet.celestial_id)])
        .await
        .expect_err("Docked AnchorOrbital outside Pochven must fail");
    assert_eq!(docked_err.code, "NotInSpace");
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&OWNER).unwrap().station_id = None;
    });
    dir_sess.station_id = None;

    // 2. NPC corporation pilot -> MUST fail with CrpAccessDenied!
    let mut npc_sess = SessionState::new(9302);
    npc_sess.user_id = Some(NPC_PILOT as u64);
    npc_sess.character_id = Some(NPC_PILOT);
    npc_sess.corporation_id = Some(1_000_009);
    npc_sess.corp_role = eve_db::corp_roles::ROLE_DIRECTOR;
    npc_sess.solar_system_id = Some(RANCER);
    npc_sess.ship_id = Some(npc_ship);
    assert_eq!(
        rpc(&ctx, &mut npc_sess, "posMgr", "AnchorOrbital", &[PyRep::Integer(890_003_101), PyRep::Integer(rancer_planet.celestial_id)]).await.unwrap_err().code,
        "CrpAccessDenied"
    );

    // 3. Player corporation member without Director / Station Manager -> MUST fail with CrpAccessDenied!
    let mut line_sess = SessionState::new(9303);
    line_sess.user_id = Some(LINE_MEMBER as u64);
    line_sess.character_id = Some(LINE_MEMBER);
    line_sess.corporation_id = Some(corp_id);
    line_sess.corp_role = 0;
    line_sess.solar_system_id = Some(RANCER);
    line_sess.ship_id = Some(line_ship);
    assert_eq!(
        rpc(&ctx, &mut line_sess, "posMgr", "AnchorOrbital", &[PyRep::Integer(890_003_102), PyRep::Integer(rancer_planet.celestial_id)]).await.unwrap_err().code,
        "CrpAccessDenied"
    );

    // 4. Cross-system planet ID (Jita IV while in Rancer) -> MUST fail with InvalidOrbitalPlanet!
    assert_eq!(
        rpc(&ctx, &mut dir_sess, "posMgr", "AnchorOrbital", &[PyRep::Integer(890_003_100), PyRep::Integer(JITA_PLANET_ID)]).await.unwrap_err().code,
        "InvalidOrbitalPlanet"
    );

    // 5. Out-of-range planet (> radius + 100,000,000 m) -> MUST fail with OrbitalOutOfRange!
    ctx.systems
        .get_or_spawn(RANCER)
        .set_ball_position(dir_ship, rancer_planet.position + DVec3::new(rancer_planet.radius + 250_000_000.0, 0.0, 0.0))
        .await;
    assert_eq!(
        rpc(&ctx, &mut dir_sess, "posMgr", "AnchorOrbital", &[PyRep::Integer(890_003_100), PyRep::Integer(rancer_planet.celestial_id)]).await.unwrap_err().code,
        "OrbitalOutOfRange"
    );

    // 6. Pochven effective standing with Diplomacy (3357) and Connections (3359)!
    let p1 = &poch_planets[0];
    let p2 = &poch_planets[1];
    spawn_ship_ball(&ctx, poch_sys_id, dir_ship, p1.position + DVec3::new(p1.radius + 5_000.0, 0.0, 0.0)).await;

    // 6a. Raw standing = -0.5 without Diplomacy -> rejected; with Diplomacy V (-0.5 + 10.5 * 0.20 = +1.60 >= 1.5) -> accepted!
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&OWNER).unwrap();
        ch.solar_system_id = poch_sys_id;
        ch.standings.insert(500_026, -0.5);
        snap.items.get_mut(&dir_ship).unwrap().location_id = i64::from(poch_sys_id);
    });
    assert_eq!(
        rpc(&ctx, &mut dir_sess, "posMgr", "AnchorOrbital", &[PyRep::Integer(890_003_103), PyRep::Integer(p1.celestial_id)]).await.unwrap_err().code,
        "InsufficientTriglavianStanding"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(OWNER).or_default().insert(3_357, TrainedSkillRecord { type_id: 3_357, level: 5, skill_points: 256_000.0 });
    });
    let poch_ok_diplo = rpc(&ctx, &mut dir_sess, "posMgr", "AnchorOrbital", &[PyRep::Integer(890_003_103), PyRep::Integer(p1.celestial_id)])
        .await
        .expect("Diplomacy V raising -0.5 raw to +1.60 effective Triglavian standing must allow Pochven Gantry");
    assert_eq!(poch_ok_diplo.dict_get("planetID"), Some(&PyRep::Integer(p1.celestial_id)));

    // 6b. Raw standing = +0.8 without Connections -> rejected; with Connections V (0.8 + 9.2 * 0.20 = +2.64 >= 1.5) -> accepted!
    ctx.systems
        .get_or_spawn(poch_sys_id)
        .set_ball_position(dir_ship, p2.position + DVec3::new(p2.radius + 5_000.0, 0.0, 0.0))
        .await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&OWNER).unwrap().standings.insert(500_026, 0.8);
    });
    assert_eq!(
        rpc(&ctx, &mut dir_sess, "posMgr", "AnchorOrbital", &[PyRep::Integer(890_003_104), PyRep::Integer(p2.celestial_id)]).await.unwrap_err().code,
        "InsufficientTriglavianStanding"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(OWNER).or_default().insert(3_359, TrainedSkillRecord { type_id: 3_359, level: 5, skill_points: 256_000.0 });
    });
    let poch_ok_conn = rpc(&ctx, &mut dir_sess, "posMgr", "AnchorOrbital", &[PyRep::Integer(890_003_104), PyRep::Integer(p2.celestial_id)])
        .await
        .expect("Connections V raising +0.8 raw to +2.64 effective Triglavian standing must allow Pochven Gantry");
    assert_eq!(poch_ok_conn.dict_get("planetID"), Some(&PyRep::Integer(p2.celestial_id)));
}

#[tokio::test]
async fn test_bf04_structure_control_alliance_grant_and_corp_alliance_block_revocation() {
    // parity: feat:structures.board-structure, feat:structures.defense-settings, rpc:structureControl.TakeControl, rpc:structureControl.BoardStructure
    eve_testkit::attest(&[
        "feat:structures.board-structure",
        "feat:structures.defense-settings",
        "rpc:structureControl.TakeControl",
        "rpc:structureControl.BoardStructure",
    ]);

    let ctx = new_ctx();
    let owner_corp = 98_904_001;
    let sister_corp = 98_904_002;
    let alliance_id = 99_904_001;
    let st_id = 1_000_890_004_001;
    let owner_ship = 890_004_011;
    let gunner_ship = 890_004_012;
    let now_init = ctx.now_unix();

    ctx.db.with_mut_snapshot(|snap| {
        snap.alliances.insert(alliance_id, AllianceRecord {
            alliance_id,
            name: "Gunner Coalition".into(),
            short_name: "GCOA".into(),
            executor_corp_id: owner_corp,
            member_corp_ids: HashSet::from([owner_corp, sister_corp]),
        });
        for (cid, ceo) in [(owner_corp, OWNER), (sister_corp, ALLY_GUNNER)] {
            let mut c = CorporationRecord::default();
            c.corporation_id = cid;
            c.name = format!("Corp {cid}");
            c.ticker = "GCRP".into();
            c.ceo_id = ceo;
            c.alliance_id = Some(alliance_id);
            c.members.insert(ceo);
            c.member_roles.insert(ceo, eve_db::corp_roles::ROLE_DIRECTOR);
            snap.corporations.insert(cid, c);
        }
        snap.structures.insert(st_id, UpwellStructureRecord {
            structure_id: st_id, type_id: 35832, owner_corp_id: owner_corp, solar_system_id: RANCER,
            name: "Alliance Gunner Citadel".into(), position: [0.0, 6.0e9, 0.0], state: life::STATE_SHIELD_VULNERABLE.into(),
            quantum_core_installed: true, fuel_blocks: 1_500, shield_hp: 1_000_000.0, armor_hp: 1_000_000.0,
            hull_hp: 1_000_000.0, reinforced_until_unix: 0, last_damage_tick: 0, last_damage_unix: 0, self_destruct_at_unix: None,
        });
        for (idx, (pid, pcorp, shid)) in [(OWNER, owner_corp, owner_ship), (ALLY_GUNNER, sister_corp, gunner_ship)].into_iter().enumerate() {
            let uid = (40 + idx) as u64;
            ensure_account(snap, uid);
            let mut ch = snap.characters.get(&OWNER).cloned().unwrap();
            ch.character_id = pid;
            ch.user_id = uid;
            ch.corporation_id = pcorp;
            ch.alliance_id = Some(alliance_id);
            ch.solar_system_id = RANCER;
            ch.structure_id = Some(st_id);
            ch.station_id = None;
            ch.active_ship_id = shid;
            snap.characters.insert(pid, ch);
            snap.items.insert(shid, InventoryItemRecord {
                item_id: shid, type_id: 587, owner_id: pid, location_id: st_id,
                flag: InventoryFlag::Hangar, quantity: 1, singleton: true, version: 1, ..Default::default()
            });
        }
    });
    ctx.db.acquire_online_lock(40, 9401, Some(OWNER), true, now_init).unwrap();
    ctx.db.acquire_online_lock(41, 9402, Some(ALLY_GUNNER), true, now_init).unwrap();

    let mut owner_sess = SessionState::new(9401);
    owner_sess.user_id = Some(40);
    owner_sess.character_id = Some(OWNER);
    owner_sess.corporation_id = Some(owner_corp);
    owner_sess.alliance_id = Some(alliance_id);
    owner_sess.corp_role = eve_db::corp_roles::ROLE_DIRECTOR;
    owner_sess.solar_system_id = Some(RANCER);
    owner_sess.structure_id = Some(st_id);
    owner_sess.ship_id = Some(owner_ship);

    let mut gunner_sess = SessionState::new(9402);
    gunner_sess.user_id = Some(41);
    gunner_sess.character_id = Some(ALLY_GUNNER);
    gunner_sess.corporation_id = Some(sister_corp);
    gunner_sess.alliance_id = Some(alliance_id);
    gunner_sess.solar_system_id = Some(RANCER);
    gunner_sess.structure_id = Some(st_id);
    gunner_sess.ship_id = Some(gunner_ship);

    // 1. Create defense group granting alliance_id -> sister-corp pilot MUST be able to TakeControl!
    let def_grp = kv_i64(&rpc(&ctx, &mut owner_sess, "ownerGroupManager", "Create", &[PyRep::String("Alliance Gunners".into()), PyRep::String("Coalition".into())]).await.unwrap(), "groupID");
    rpc(&ctx, &mut owner_sess, "ownerGroupManager", "AddMembers", &[PyRep::Integer(def_grp), PyRep::List(vec![PyRep::Integer(alliance_id)])]).await.unwrap();
    let prof_id = rpc(&ctx, &mut owner_sess, "structureProfiles", "CreateProfile", &[PyRep::String("Alliance Defense".into()), PyRep::String("Ally".into())]).await.unwrap().as_i64().unwrap();
    rpc(&ctx, &mut owner_sess, "structureProfiles", "SaveProfileSettings", &[
        PyRep::Integer(prof_id),
        PyRep::List(vec![
            PyRep::Tuple(vec![PyRep::Integer(19), PyRep::Integer(0), PyRep::Integer(1)]),
            PyRep::Tuple(vec![PyRep::Integer(17), PyRep::Integer(def_grp), PyRep::Integer(1)]),
        ]),
    ]).await.unwrap();
    rpc(&ctx, &mut owner_sess, "structureProfiles", "ChangeProfiles", &[PyRep::List(vec![PyRep::Integer(st_id)]), PyRep::Integer(prof_id)]).await.unwrap();

    rpc(&ctx, &mut gunner_sess, "structureControl", "TakeControl", &[PyRep::Integer(st_id)])
        .await
        .expect("Alliance-level defense ACL grant must authorize sister-corp pilot TakeControl");
    assert_eq!(gunner_sess.ship_id, Some(st_id));

    // 2. Blocking sister_corp (-1 in def_grp) revokes active control and denies subsequent TakeControl!
    let _ = ctx.drain_notifications_for_char(ALLY_GUNNER);
    rpc(&ctx, &mut owner_sess, "ownerGroupManager", "AddMembers", &[PyRep::Integer(def_grp), PyRep::List(vec![PyRep::Integer(sister_corp)]), PyRep::Integer(-1)]).await.unwrap();
    assert_eq!(rpc(&ctx, &mut owner_sess, "structureControl", "GetStructurePilot", &[PyRep::Integer(st_id)]).await.unwrap(), PyRep::None);
    let corp_blk_notifs = ctx.drain_notifications_for_char(ALLY_GUNNER);
    assert!(corp_blk_notifs.iter().any(|n| n.event == "OnSessionChanged"));
    ctx.sync_session(&mut gunner_sess);
    assert_eq!(gunner_sess.ship_id, Some(gunner_ship));
    assert_eq!(rpc(&ctx, &mut gunner_sess, "structureControl", "TakeControl", &[PyRep::Integer(st_id)]).await.unwrap_err().code, "DefenseAccessDenied");

    // 3. Unblock sister_corp, re-board via BoardStructure, then block alliance_id via blockedEntities -> revokes & denies!
    rpc(&ctx, &mut owner_sess, "ownerGroupManager", "DeleteMembers", &[PyRep::Integer(def_grp), PyRep::List(vec![PyRep::Integer(sister_corp)])]).await.unwrap();
    rpc(&ctx, &mut gunner_sess, "structureControl", "BoardStructure", &[PyRep::Integer(st_id)]).await.expect("BoardStructure restored after removing corp block");
    rpc(&ctx, &mut owner_sess, "structureProfiles", "SaveProfileSettings", &[
        PyRep::Integer(prof_id),
        PyRep::List(vec![
            PyRep::Tuple(vec![PyRep::Integer(19), PyRep::Integer(0), PyRep::Integer(1)]),
            PyRep::Tuple(vec![PyRep::Integer(19), PyRep::Integer(alliance_id), PyRep::Integer(-1)]),
            PyRep::Tuple(vec![PyRep::Integer(17), PyRep::Integer(def_grp), PyRep::Integer(1)]),
        ]),
    ]).await.unwrap();
    assert_eq!(rpc(&ctx, &mut owner_sess, "structureControl", "GetStructurePilot", &[PyRep::Integer(st_id)]).await.unwrap(), PyRep::None);
    ctx.sync_session(&mut gunner_sess);
    assert_eq!(gunner_sess.ship_id, Some(gunner_ship));
    assert_eq!(rpc(&ctx, &mut gunner_sess, "structureControl", "TakeControl", &[PyRep::Integer(st_id)]).await.unwrap_err().code, "DefenseAccessDenied");
}

#[tokio::test]
async fn test_bf05_mobile_cyno_inhibitor_upwell_50km_clearance_and_space_bans() {
    // parity: feat:structures.mobile-cyno-inhibitor, feat:structures.mobile-deploy-restrictions, rpc:posMgr.AnchorStructure
    eve_testkit::attest(&["feat:structures.mobile-cyno-inhibitor", "feat:structures.mobile-deploy-restrictions", "rpc:posMgr.AnchorStructure"]);

    let ctx = new_ctx();
    // Exercises 50km Upwell clearance, high-sec/W-space/Zarzakh MCI ban, and MMJU lifetime.
    let ship_id = 890_005_001;
    let upwell_id = 1_000_890_005_100;
    let upwell_pos = DVec3::new(9.0e10, 9.0e10, 9.0e10);
    let uid = OWNER as u64;

    // Seed Upwell structure in RANCER WITHOUT touching snap.item_positions (matching live Upwell anchoring!).
    ctx.db.with_mut_snapshot(|snap| {
        ensure_account(snap, uid);
        if let Some(ch) = snap.characters.get_mut(&OWNER) {
            ch.user_id = uid;
            ch.solar_system_id = RANCER;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = ship_id;
        }
        snap.skills.entry(OWNER).or_default().insert(11584, TrainedSkillRecord { type_id: 11584, level: 5, skill_points: 256_000.0 });
        snap.items.insert(ship_id, InventoryItemRecord {
            item_id: ship_id, type_id: 587, owner_id: OWNER, location_id: i64::from(RANCER),
            flag: InventoryFlag::None, quantity: 1, singleton: true, version: 1, ..Default::default()
        });
        snap.structures.insert(upwell_id, UpwellStructureRecord {
            structure_id: upwell_id, type_id: 35832, owner_corp_id: 98_905_001, solar_system_id: RANCER,
            name: "Proximity Citadel".into(), position: upwell_pos.to_array(), state: life::STATE_SHIELD_VULNERABLE.into(),
            quantum_core_installed: true, fuel_blocks: 1_000, shield_hp: 1_000_000.0, armor_hp: 1_000_000.0,
            hull_hp: 1_000_000.0, reinforced_until_unix: 0, last_damage_tick: 0, last_damage_unix: 0, self_destruct_at_unix: None,
        });
        for (idx, tid) in [
            TYPE_MOBILE_CYNO_INHIBITOR,
            TYPE_MOBILE_CYNO_INHIBITOR,
            TYPE_MOBILE_CYNO_INHIBITOR,
            TYPE_MOBILE_CYNO_INHIBITOR,
            TYPE_MOBILE_CYNO_INHIBITOR,
            TYPE_MOBILE_MJU,
        ].into_iter().enumerate() {
            let iid = 890_005_200 + idx as i64;
            snap.items.insert(iid, InventoryItemRecord {
                item_id: iid, type_id: tid, owner_id: OWNER, location_id: ship_id,
                flag: InventoryFlag::Cargo, quantity: 1, singleton: true, version: 1, ..Default::default()
            });
        }
    });

    let mut sess = SessionState::new(9501);
    sess.user_id = Some(uid);
    sess.character_id = Some(OWNER);
    sess.solar_system_id = Some(RANCER);
    sess.ship_id = Some(ship_id);

    // 1. Within 30 km (< 50 km) of Upwell structure -> DeploymentTooCloseToStructure!
    spawn_ship_ball(&ctx, RANCER, ship_id, upwell_pos + DVec3::new(30_000.0, 0.0, 0.0)).await;
    assert_eq!(rpc(&ctx, &mut sess, "posMgr", "AnchorStructure", &[PyRep::Integer(890_005_200)]).await.unwrap_err().code, "DeploymentTooCloseToStructure");

    // 2. High-sec (Perimeter 30000144), Wormhole (31000100), and Zarzakh (30100000) -> CantAnchorInHighSecOrWormhole!
    for (sys_id, iid) in [(PERIMETER, 890_005_201), (WORMHOLE_SYS, 890_005_202), (ZARZAKH_SYS, 890_005_203)] {
        ctx.db.with_mut_snapshot(|snap| {
            snap.characters.get_mut(&OWNER).unwrap().solar_system_id = sys_id;
            snap.items.get_mut(&ship_id).unwrap().location_id = i64::from(sys_id);
        });
        sess.solar_system_id = Some(sys_id);
        spawn_ship_ball(&ctx, sys_id, ship_id, DVec3::new(8.0e10, 8.0e10, 8.0e10)).await;
        assert_eq!(
            rpc(&ctx, &mut sess, "posMgr", "AnchorStructure", &[PyRep::Integer(iid)]).await.unwrap_err().code,
            "CantAnchorInHighSecOrWormhole",
            "Mobile Cynosural Inhibitor must be banned in system {sys_id}"
        );
    }

    // 3. >= 50 km (65 km) from Upwell structure in low-sec Rancer -> SUCCEEDS, and MMJU uses MMJU_LIFETIME_SECS!
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&OWNER).unwrap().solar_system_id = RANCER;
        snap.items.get_mut(&ship_id).unwrap().location_id = i64::from(RANCER);
    });
    sess.solar_system_id = Some(RANCER);
    ctx.systems.get_or_spawn(RANCER).set_ball_position(ship_id, upwell_pos + DVec3::new(65_000.0, 0.0, 0.0)).await;
    let ok_mci = rpc(&ctx, &mut sess, "posMgr", "AnchorStructure", &[PyRep::Integer(890_005_204)])
        .await
        .expect("Mobile Cyno Inhibitor >= 50km from Upwell structure in low-sec must succeed");
    assert_eq!(kv_i64(&ok_mci, "itemID"), 890_005_204);

    let now_before_mmju = ctx.now_unix();
    let ok_mmju = rpc(&ctx, &mut sess, "posMgr", "AnchorStructure", &[PyRep::Integer(890_005_205)])
        .await
        .expect("Mobile Micro Jump Unit anchoring >= 50km from Upwell structure must succeed");
    assert_eq!(
        kv_i64(&ok_mmju, "despawnUnix"),
        (now_before_mmju + eve_server::services::mobile_deployables::MMJU_ACTIVATION_DELAY_SECS + MMJU_LIFETIME_SECS) as i64
    );
}
