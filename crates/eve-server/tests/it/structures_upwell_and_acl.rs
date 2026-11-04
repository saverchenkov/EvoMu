//! Upwell structure anchoring restrictions, profile ACL bindings, defense retaliation,
//! structure directory filtering & masking, and Fulcrum allegiance services.
use std::collections::HashMap;
use eve_db::{InventoryFlag, InventoryItemRecord, UpwellStructureRecord};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use eve_server::services::station_structure::structure_lifecycle as life;
use eve_server::services::structure_acl_defense_rules as acl_rules;
use glam::DVec3;

const JITA_SYS: u32 = 30_000_142;
const PERIMETER_SYS: u32 = 30_000_144;
const RANCER_SYS: u32 = 30_002_718;
const ZARZAKH_SYS: u32 = 30_100_000;
const FULCRUM_ID: i64 = 60_015_187;

async fn rpc(ctx: &ServerContext, sess: &mut SessionState, svc: &str, mth: &str, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, svc, None, mth, args, &HashMap::new(), None).await
}

fn kv<'a>(rep: &'a PyRep, key: &str) -> Option<&'a PyRep> {
    match rep {
        PyRep::KeyVal(e) => e.get(key),
        PyRep::Dict(e) => e.iter().find(|(k, _)| k.as_str() == Some(key)).map(|(_, v)| v),
        _ => None,
    }
}

fn pos_tuple(p: DVec3) -> PyRep {
    PyRep::Tuple(vec![PyRep::Real(p.x), PyRep::Real(p.y), PyRep::Real(p.z)])
}

fn make_ball(id: i64, type_id: u32, pos: DVec3) -> Ball {
    Ball {
        id,
        type_id,
        position: pos,
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 2.0,
        mass: 1e6,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    }
}

async fn setup_corp_session(ctx: &ServerContext, sid: u64, cid: i64, tk: &str, sys: u32, ship: i64) -> SessionState {
    ctx.db.with_mut_snapshot(|snap| {
        let base = snap.characters.get(&90_000_001).cloned().unwrap();
        snap.characters.insert(cid, eve_db::CharacterRecord {
            character_id: cid,
            user_id: 1,
            name: format!("Pilot {cid}"),
            active_ship_id: ship,
            solar_system_id: sys,
            balance: eve_db::IskCents::from_isk(5_000_000_000),
            ..base
        });
        if let Some(base_skills) = snap.skills.get(&90_000_001).cloned() {
            snap.skills.insert(cid, base_skills);
        }
        snap.skills.entry(cid).or_default().insert(3363, eve_db::TrainedSkillRecord {
            type_id: 3363, level: 5, skill_points: 256_000.0,
        });
        snap.items.insert(ship, InventoryItemRecord {
            item_id: ship,
            type_id: 587,
            owner_id: cid,
            location_id: sys as i64,
            flag: InventoryFlag::None,
            quantity: 1,
            singleton: true,
            ..Default::default()
        });
    });
    let mut s = SessionState::new(sid);
    rpc(ctx, &mut s, "authentication", "Login", &[PyRep::String("demo".into()), PyRep::String("demo-secret".into())]).await.unwrap();
    rpc(ctx, &mut s, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(cid)]).await.unwrap();
    ctx.advance_secs(12);
    rpc(ctx, &mut s, "corpRegistry", "AddCorporation", &[PyRep::String(format!("Upwell {cid}")), PyRep::String(tk.into())]).await.unwrap();
    s.corp_role |= eve_db::corp_roles::ROLE_DIRECTOR | eve_db::corp_roles::ROLE_STATION_MANAGER;
    s.solar_system_id = Some(sys);
    s.ship_id = Some(ship);
    ctx.advance_secs(7 * 86_400 + 30);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.solar_system_id = sys; ch.active_ship_id = ship; ch.station_id = None; ch.structure_id = None;
        }
    });
    s
}

fn seed_citadel(ctx: &ServerContext, sid: i64, tid: u32, corp: i64, sys: u32, pos: [f64; 3]) {
    let p = life::structure_profile(ctx, tid).expect("prof");
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(sid, UpwellStructureRecord {
            structure_id: sid, type_id: tid, owner_corp_id: corp, solar_system_id: sys, position: pos,
            state: life::STATE_SHIELD_VULNERABLE.to_string(), quantum_core_installed: true, fuel_blocks: 3_000,
            shield_hp: p.shield_hp, armor_hp: p.armor_hp, hull_hp: p.hull_hp, name: format!("Citadel {sid}"),
            reinforced_until_unix: 0, last_damage_tick: 0, last_damage_unix: 0, self_destruct_at_unix: None,
        });
        snap.structure_vulnerability_windows.insert(sid, (18, 5));
        snap.items.insert(sid, InventoryItemRecord {
            item_id: sid, type_id: tid, owner_id: corp, location_id: sys as i64, flag: InventoryFlag::None,
            quantity: 1, singleton: true, version: 1, ..Default::default()
        });
    });
}

#[tokio::test]
async fn test_upwell_anchoring_restrictions_proximity_system_and_roles() {
    // parity: feat:structures.anchor-restrictions
    eve_testkit::attest(&["feat:structures.anchor-restrictions"]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);

    let (cid, ship) = (90_000_101, 100_000_101);
    let mut dir_sess = setup_corp_session(&ctx, 601, cid, "ANCH", PERIMETER_SYS, ship).await;
    let corp_id = ctx.db.with_snapshot(|s| s.characters[&cid].corporation_id);

    let prof_rep = rpc(&ctx, &mut dir_sess, "structureProfiles", "CreateProfile", &[PyRep::String("Corp P".into()), PyRep::String("".into())]).await.unwrap();
    let pid = prof_rep.as_i64().unwrap();
    rpc(&ctx, &mut dir_sess, "structureProfiles", "SetDefaultProfile", &[PyRep::Integer(pid)]).await.unwrap();

    let far_ship = DVec3::new(1.0e11, 2.0e11, 3.0e11);
    let cargo = 880_501i64;
    ctx.db.with_mut_snapshot(|s| {
        s.items.insert(cargo, InventoryItemRecord {
            item_id: cargo, type_id: 35832, owner_id: corp_id, location_id: ship,
            flag: InventoryFlag::Cargo, quantity: 1, singleton: false, version: 1, ..Default::default()
        });
    });
    let _ = ctx.systems.get_or_spawn(PERIMETER_SYS).spawn_ball(Ball {
        id: ship, type_id: 606, position: far_ship, velocity: DVec3::ZERO,
        max_velocity: 300.0, inertia: 2.0, mass: 1e6, radius: 40.0, speed_fraction: 0.0, mode: MovementMode::Stop,
    }).await;

    let rw = PyRep::Tuple(vec![PyRep::Integer(18), PyRep::Integer(3)]);
    let sys_handle = ctx.systems.get_or_spawn(PERIMETER_SYS);

    // 1. Proximity check: < 1,000 km from station fails
    let st = ctx.sde.stations.values().find(|s| s.solar_system_id == PERIMETER_SYS).unwrap();
    let near_st_ship = st.position + DVec3::new(4e5, 0.0, 0.0);
    let _ = sys_handle.spawn_ball(make_ball(ship, 606, near_st_ship)).await;
    let err_st = rpc(&ctx, &mut dir_sess, "structureDeployment", "Anchor", &[
        PyRep::Integer(cargo), pos_tuple(near_st_ship + DVec3::new(60_000.0, 0.0, 0.0)), PyRep::Integer(pid), rw.clone(), PyRep::String("C".into()),
    ]).await.unwrap_err();
    assert_eq!(err_st.code, "TooCloseToStructureOrStation");

    // 2. Proximity check: < 1,000 km from existing citadel fails
    let cit_pos = DVec3::new(4e11, 4e11, 4e11);
    seed_citadel(&ctx, 1_000_000_500_001, 35832, corp_id, PERIMETER_SYS, cit_pos.to_array());
    let near_cit_ship = cit_pos + DVec3::new(5e5, 0.0, 0.0);
    let _ = sys_handle.spawn_ball(make_ball(ship, 606, near_cit_ship)).await;
    let err_cit = rpc(&ctx, &mut dir_sess, "structureDeployment", "Anchor", &[
        PyRep::Integer(cargo), pos_tuple(near_cit_ship + DVec3::new(60_000.0, 0.0, 0.0)), PyRep::Integer(pid), rw.clone(), PyRep::String("C".into()),
    ]).await.unwrap_err();
    assert_eq!(err_cit.code, "TooCloseToStructureOrStation");

    // 3. Proximity check: < 1,000 km from stargate fails
    let gate = ctx.sde.stargates().values().find(|g| g.from_solar_system_id == PERIMETER_SYS).unwrap();
    let near_gate_ship = gate.position + DVec3::new(3e5, 0.0, 0.0);
    let _ = sys_handle.spawn_ball(make_ball(ship, 606, near_gate_ship)).await;
    let err_gate = rpc(&ctx, &mut dir_sess, "structureDeployment", "Anchor", &[
        PyRep::Integer(cargo), pos_tuple(near_gate_ship + DVec3::new(60_000.0, 0.0, 0.0)), PyRep::Integer(pid), rw.clone(), PyRep::String("C".into()),
    ]).await.unwrap_err();
    assert_eq!(err_gate.code, "TooCloseToStructureOrStation");

    // 4. Restricted systems: Jita (trade hub) disallowed
    ctx.db.with_mut_snapshot(|s| { s.characters.get_mut(&cid).unwrap().solar_system_id = JITA_SYS; });
    dir_sess.solar_system_id = Some(JITA_SYS);
    let jita_handle = ctx.systems.get_or_spawn(JITA_SYS);
    let _ = jita_handle.spawn_ball(make_ball(ship, 606, far_ship)).await;
    let err_jh = rpc(&ctx, &mut dir_sess, "structureDeployment", "Anchor", &[
        PyRep::Integer(cargo), pos_tuple(far_ship + DVec3::new(60_000.0, 0.0, 0.0)), PyRep::Integer(pid), rw.clone(), PyRep::String("C".into()),
    ]).await.unwrap_err();
    assert_eq!(err_jh.code, "AnchoringDisallowedInTradeHub");
    ctx.db.with_mut_snapshot(|s| { s.characters.get_mut(&cid).unwrap().solar_system_id = PERIMETER_SYS; });
    dir_sess.solar_system_id = Some(PERIMETER_SYS);

    // Reset ship to far position
    let _ = sys_handle.spawn_ball(make_ball(ship, 606, far_ship)).await;

    // 5. Role requirement: pilot without Station Manager or Director role fails
    let mut no_role_sess = SessionState::new(602);
    no_role_sess.user_id = Some(2);
    no_role_sess.character_id = Some(90_000_102);
    no_role_sess.corporation_id = Some(corp_id);
    no_role_sess.solar_system_id = Some(PERIMETER_SYS);
    no_role_sess.ship_id = Some(ship);
    ctx.db.with_mut_snapshot(|s| {
        let mut ch = s.characters[&cid].clone();
        ch.character_id = 90_000_102;
        ch.user_id = 2;
        s.characters.insert(90_000_102, ch);
    });
    let err_role = rpc(&ctx, &mut no_role_sess, "structureDeployment", "Anchor", &[PyRep::Integer(cargo), PyRep::String("C".into())]).await.unwrap_err();
    assert_eq!(err_role.code, "InsufficientCorpRoles");

    // 6. Valid corp profile: foreign profile rejected
    let valid_pos = far_ship + DVec3::new(60_000.0, 0.0, 0.0);
    let err_prof = rpc(&ctx, &mut dir_sess, "structureDeployment", "Anchor", &[
        PyRep::Integer(cargo), pos_tuple(valid_pos), PyRep::Integer(79_999_999), rw.clone(), PyRep::String("C".into()),
    ]).await.unwrap_err();
    assert_eq!(err_prof.code, "StructureOwnershipViolation");

    // 7. Valid Station Manager & valid corp profile succeeds
    let rep = rpc(&ctx, &mut dir_sess, "structureDeployment", "Anchor", &[
        PyRep::Integer(cargo), pos_tuple(valid_pos), PyRep::Integer(pid), rw, PyRep::String("Valid Citadel".into()),
    ]).await.unwrap();
    let new_id = kv(&rep, "structureID").unwrap().as_i64().unwrap();
    assert!(new_id > 1_000_000_000_000);
}

#[tokio::test]
async fn test_upwell_profiles_acl_service_access_and_delayed_transition() {
    // parity: feat:structures.profiles
    eve_testkit::attest(&["feat:structures.profiles"]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);

    let (dir_char, vip_guest, ship) = (90_000_201, 90_000_202, 100_000_201);
    let mut dir_sess = setup_corp_session(&ctx, 611, dir_char, "PROX", PERIMETER_SYS, ship).await;
    let corp_id = ctx.db.with_snapshot(|s| s.characters[&dir_char].corporation_id);
    ctx.db.with_mut_snapshot(|s| {
        let mut ch = s.characters[&dir_char].clone();
        ch.character_id = vip_guest; ch.corporation_id = 98_000_222; ch.user_id = 2;
        s.characters.insert(vip_guest, ch);
    });

    let acl_rep = rpc(&ctx, &mut dir_sess, "ownerGroupManager", "Create", &[PyRep::String("VIP ACL".into()), PyRep::String("".into())]).await.unwrap();
    let vip_acl = kv(&acl_rep, "groupID").and_then(|v| v.as_i64()).or_else(|| acl_rep.as_i64()).unwrap();
    rpc(&ctx, &mut dir_sess, "ownerGroupManager", "UpdateMembershipTypes", &[
        PyRep::Integer(vip_acl), PyRep::List(vec![PyRep::Integer(vip_guest)]), PyRep::Integer(acl_rules::ACL_ROLE_MEMBER),
    ]).await.unwrap();

    let prof_id = rpc(&ctx, &mut dir_sess, "structureProfiles", "CreateProfile", &[PyRep::String("Hub".into()), PyRep::String("".into())]).await.unwrap().as_i64().unwrap();
    rpc(&ctx, &mut dir_sess, "structureProfiles", "SaveProfileSettings", &[
        PyRep::Integer(prof_id),
        PyRep::List(vec![
            PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Real(0.0), PyRep::Integer(vip_acl)]),
            PyRep::Tuple(vec![PyRep::Integer(3), PyRep::Real(0.045), PyRep::Integer(vip_acl)]),
            PyRep::Tuple(vec![PyRep::Integer(4), PyRep::Real(0.015), PyRep::Integer(vip_acl)]),
        ]),
    ]).await.unwrap();

    let st_id = 1_000_000_600_001i64;
    seed_citadel(&ctx, st_id, 35833, corp_id, PERIMETER_SYS, [1.0e10, 0.0, 0.0]);
    ctx.db.with_mut_snapshot(|s| {
        for (item_id, mod_tid, flag) in [(880_611, 35899, InventoryFlag::ServiceSlot0), (880_612, 35897, InventoryFlag::ServiceSlot1)] {
            s.items.insert(item_id, InventoryItemRecord {
                item_id, type_id: mod_tid, owner_id: corp_id, location_id: st_id, flag, quantity: 1, singleton: true, version: 1, ..Default::default()
            });
            s.module_online_states.insert(item_id, true);
        }
    });

    rpc(&ctx, &mut dir_sess, "structureProfiles", "ChangeProfiles", &[PyRep::List(vec![PyRep::Integer(st_id)]), PyRep::Integer(prof_id)]).await.unwrap();

    let mut vip_sess = SessionState::new(612);
    vip_sess.user_id = Some(2);
    vip_sess.character_id = Some(vip_guest);
    vip_sess.corporation_id = Some(98_000_222);

    rpc(&ctx, &mut vip_sess, "structureSettings", "CharacterCheckService", &[PyRep::Integer(st_id), PyRep::Integer(4)]).await.unwrap();
    let PyRep::Dict(svcs) = rpc(&ctx, &mut vip_sess, "structureSettings", "CharacterGetServices", &[PyRep::Integer(st_id)]).await.unwrap() else { panic!() };
    let tax = svcs.iter().find(|(k, _)| k.as_i64() == Some(4)).and_then(|(_, v)| v.as_f64()).unwrap();
    assert!((tax - 0.045).abs() < 1e-6);

    // Delayed profile transition (300s)
    let lock_prof = rpc(&ctx, &mut dir_sess, "structureProfiles", "CreateProfile", &[PyRep::String("Lock".into()), PyRep::String("".into())]).await.unwrap().as_i64().unwrap();
    let c_acl_rep = rpc(&ctx, &mut dir_sess, "ownerGroupManager", "Create", &[PyRep::String("Corp Only".into()), PyRep::String("".into())]).await.unwrap();
    let c_acl = kv(&c_acl_rep, "groupID").and_then(|v| v.as_i64()).or_else(|| c_acl_rep.as_i64()).unwrap();
    rpc(&ctx, &mut dir_sess, "ownerGroupManager", "UpdateMembershipTypes", &[
        PyRep::Integer(c_acl), PyRep::List(vec![PyRep::Integer(corp_id)]), PyRep::Integer(acl_rules::ACL_ROLE_MEMBER),
    ]).await.unwrap();
    rpc(&ctx, &mut dir_sess, "structureProfiles", "SaveProfileSettings", &[
        PyRep::Integer(lock_prof),
        PyRep::List(vec![
            PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Real(0.0), PyRep::Integer(c_acl)]),
            PyRep::Tuple(vec![PyRep::Integer(4), PyRep::Real(0.015), PyRep::Integer(c_acl)]),
        ]),
    ]).await.unwrap();
    rpc(&ctx, &mut dir_sess, "structureProfiles", "ChangeProfiles", &[PyRep::List(vec![PyRep::Integer(st_id)]), PyRep::Integer(lock_prof)]).await.unwrap();

    ctx.advance_secs(100);
    assert!(rpc(&ctx, &mut vip_sess, "structureSettings", "CharacterCheckService", &[PyRep::Integer(st_id), PyRep::Integer(4)]).await.is_ok());

    ctx.advance_secs(205);
    let err = rpc(&ctx, &mut vip_sess, "structureSettings", "CharacterCheckService", &[PyRep::Integer(st_id), PyRep::Integer(4)]).await.unwrap_err();
    assert_eq!(err.code, "StructureServiceAccessDenied");
}

#[tokio::test]
async fn test_upwell_defense_settings_retaliation_and_gunner_takeover_sync() {
    // parity: feat:structures.defense-settings
    eve_testkit::attest(&["feat:structures.defense-settings"]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);

    let (dir_char, gunner_char, ship) = (90_000_301, 90_000_302, 100_000_301);
    let mut dir_sess = setup_corp_session(&ctx, 621, dir_char, "GUNN", RANCER_SYS, ship).await;
    let corp_id = ctx.db.with_snapshot(|s| s.characters[&dir_char].corporation_id);

    let st_id = 1_000_000_700_001i64;
    seed_citadel(&ctx, st_id, 35833, corp_id, RANCER_SYS, [0.0, 0.0, 0.0]);

    let gunner_ship = 100_000_302;
    ctx.db.with_mut_snapshot(|s| {
        s.accounts.insert("gunner".into(), eve_db::AccountRecord {
            user_id: 3,
            username: "gunner".into(),
            password_token: "tok".into(),
            role: eve_db::service_roles::ROLE_PLAYER_DEFAULT,
            banned_until_unix: None,
            ban_reason: None,
        });
        let mut ch = s.characters[&dir_char].clone();
        ch.character_id = gunner_char; ch.corporation_id = corp_id; ch.structure_id = Some(st_id); ch.active_ship_id = gunner_ship; ch.user_id = 3;
        s.characters.insert(gunner_char, ch);
        s.items.insert(gunner_ship, InventoryItemRecord {
            item_id: gunner_ship, type_id: 606, owner_id: gunner_char, location_id: st_id, flag: InventoryFlag::Hangar, quantity: 1, singleton: true, version: 1, ..Default::default()
        });
        if let Some(corp) = s.corporations.get_mut(&corp_id) {
            corp.member_roles.insert(gunner_char, eve_db::corp_roles::ROLE_STARBASE_OPERATOR);
            corp.member_roles.insert(90_000_303, eve_db::corp_roles::ROLE_STARBASE_OPERATOR);
        }
    });
    let _ = ctx.db.acquire_online_lock(3, 622, Some(gunner_char), true, ctx.now_unix());

    let def_acl_rep = rpc(&ctx, &mut dir_sess, "ownerGroupManager", "Create", &[PyRep::String("Def ACL".into()), PyRep::String("".into())]).await.unwrap();
    let def_acl = kv(&def_acl_rep, "groupID").and_then(|v| v.as_i64()).or_else(|| def_acl_rep.as_i64()).unwrap();
    rpc(&ctx, &mut dir_sess, "ownerGroupManager", "UpdateMembershipTypes", &[
        PyRep::Integer(def_acl), PyRep::List(vec![PyRep::Integer(gunner_char)]), PyRep::Integer(acl_rules::ACL_ROLE_MEMBER),
    ]).await.unwrap();

    let def_prof = rpc(&ctx, &mut dir_sess, "structureProfiles", "CreateProfile", &[PyRep::String("Def".into()), PyRep::String("".into())]).await.unwrap().as_i64().unwrap();
    rpc(&ctx, &mut dir_sess, "structureProfiles", "SaveProfileSettings", &[
        PyRep::Integer(def_prof),
        PyRep::List(vec![
            PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Real(0.0), PyRep::Integer(def_acl)]),
            PyRep::Tuple(vec![PyRep::Integer(17), PyRep::Tuple(vec![PyRep::Bool(true), PyRep::Bool(true), PyRep::Bool(true), PyRep::Real(0.0), PyRep::Bool(true)]), PyRep::Integer(0)]),
        ]),
    ]).await.unwrap();
    rpc(&ctx, &mut dir_sess, "structureProfiles", "ChangeProfiles", &[PyRep::List(vec![PyRep::Integer(st_id)]), PyRep::Integer(def_prof)]).await.unwrap();

    let mut gunner_sess = SessionState::new(622);
    gunner_sess.user_id = Some(3);
    gunner_sess.character_id = Some(gunner_char);
    gunner_sess.corporation_id = Some(corp_id);
    gunner_sess.corp_role = eve_db::corp_roles::ROLE_STARBASE_OPERATOR;
    gunner_sess.solar_system_id = Some(RANCER_SYS);
    gunner_sess.structure_id = Some(st_id);
    gunner_sess.ship_id = Some(gunner_ship);

    // Gunner takes control
    rpc(&ctx, &mut gunner_sess, "structureControl", "TakeControl", &[PyRep::Integer(st_id)]).await.unwrap();
    assert_eq!(gunner_sess.ship_id, Some(st_id));
    assert_eq!(rpc(&ctx, &mut gunner_sess, "structureControl", "GetStructurePilot", &[PyRep::Integer(st_id)]).await.unwrap(), PyRep::Integer(gunner_char));

    // Regular member without manager role cannot displace gunner
    let other_char = 90_000_303;
    ctx.db.with_mut_snapshot(|s| {
        let mut ch = s.characters[&dir_char].clone();
        ch.character_id = other_char; ch.corporation_id = corp_id; ch.structure_id = Some(st_id); ch.user_id = 4;
        s.characters.insert(other_char, ch);
    });
    let mut other_sess = SessionState::new(623);
    other_sess.user_id = Some(4);
    other_sess.character_id = Some(other_char); other_sess.corporation_id = Some(corp_id);
    other_sess.corp_role = eve_db::corp_roles::ROLE_STARBASE_OPERATOR;
    other_sess.structure_id = Some(st_id);
    let err_take = rpc(&ctx, &mut other_sess, "structureControl", "TakeControl", &[PyRep::Integer(st_id)]).await.unwrap_err();
    assert_eq!(err_take.code, "StructureAlreadyControlled");

    // Director performs Gunner Takeover!
    ctx.db.with_mut_snapshot(|s| { s.characters.get_mut(&dir_char).unwrap().structure_id = Some(st_id); });
    dir_sess.structure_id = Some(st_id);
    rpc(&ctx, &mut dir_sess, "structureControl", "TakeControl", &[PyRep::Integer(st_id)]).await.unwrap();
    assert_eq!(dir_sess.ship_id, Some(st_id));
    assert_eq!(rpc(&ctx, &mut dir_sess, "structureControl", "GetStructurePilot", &[PyRep::Integer(st_id)]).await.unwrap(), PyRep::Integer(dir_char));

    // Involuntary release session synchronization
    let notifs = ctx.drain_notifications_for_char(gunner_char);
    assert!(notifs.iter().any(|n| n.event == "OnSessionChanged"));
    eve_server::services::structure_control_ops::apply_forced_session_ship_sync(&ctx, &mut gunner_sess);
    assert_eq!(gunner_sess.ship_id, Some(gunner_ship));

    // Auto-defense targeting
    let war_char = 90_000_304;
    let war_corp = 98_000_333;
    ctx.db.with_mut_snapshot(|s| {
        let mut ch = s.characters[&dir_char].clone();
        ch.character_id = war_char; ch.corporation_id = war_corp;
        s.characters.insert(war_char, ch);
        s.corporations.insert(war_corp, eve_db::CorporationRecord {
            corporation_id: war_corp, name: "Hostiles".into(), ticker: "HOST".into(), ceo_id: war_char, ..Default::default()
        });
        s.wars.insert(999, eve_db::WarRecord { war_id: 999, aggressor_corp_id: war_corp, defender_corp_id: corp_id, started_unix: 100, mutual: true, active: true });
    });
    assert!(!ctx.db.with_snapshot(|s| acl_rules::evaluate_auto_defense_target(&ctx, s, st_id, dir_char)));
    assert!(ctx.db.with_snapshot(|s| acl_rules::evaluate_auto_defense_target(&ctx, s, st_id, war_char)));
}

#[tokio::test]
async fn test_upwell_structure_directory_acl_filtering_and_sensitive_masking() {
    // parity: feat:structures.structure-directory
    eve_testkit::attest(&["feat:structures.structure-directory"]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);

    let (dir_char, guest, stranger, ship) = (90_000_401, 90_000_402, 90_000_403, 100_000_401);
    let mut dir_sess = setup_corp_session(&ctx, 631, dir_char, "DIRX", RANCER_SYS, ship).await;
    let corp_id = ctx.db.with_snapshot(|s| s.characters[&dir_char].corporation_id);

    ctx.db.with_mut_snapshot(|s| {
        for (cid, ccorp, uid, sys) in [(guest, 98_000_444, 5, RANCER_SYS), (stranger, 98_000_445, 6, JITA_SYS)] {
            let mut ch = s.characters[&dir_char].clone();
            ch.character_id = cid; ch.corporation_id = ccorp; ch.user_id = uid; ch.solar_system_id = sys;
            s.characters.insert(cid, ch);
        }
    });

    let st_id = 1_000_000_800_001i64;
    seed_citadel(&ctx, st_id, 35833, corp_id, RANCER_SYS, [1.0e10, 0.0, 0.0]);

    let acl_rep = rpc(&ctx, &mut dir_sess, "ownerGroupManager", "Create", &[PyRep::String("Dir ACL".into()), PyRep::String("".into())]).await.unwrap();
    let g_acl = kv(&acl_rep, "groupID").and_then(|v| v.as_i64()).or_else(|| acl_rep.as_i64()).unwrap();
    rpc(&ctx, &mut dir_sess, "ownerGroupManager", "UpdateMembershipTypes", &[
        PyRep::Integer(g_acl), PyRep::List(vec![PyRep::Integer(guest)]), PyRep::Integer(0),
    ]).await.unwrap();

    let prof = rpc(&ctx, &mut dir_sess, "structureProfiles", "CreateProfile", &[PyRep::String("P".into()), PyRep::String("".into())]).await.unwrap().as_i64().unwrap();
    rpc(&ctx, &mut dir_sess, "structureProfiles", "SaveProfileSettings", &[
        PyRep::Integer(prof), PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Real(0.0), PyRep::Integer(g_acl)])]),
    ]).await.unwrap();
    rpc(&ctx, &mut dir_sess, "structureProfiles", "ChangeProfiles", &[PyRep::List(vec![PyRep::Integer(st_id)]), PyRep::Integer(prof)]).await.unwrap();

    // Director: unmasked sensitive data
    let PyRep::Dict(corp_map) = rpc(&ctx, &mut dir_sess, "structureDirectory", "GetMyCorporationStructures", &[]).await.unwrap() else { panic!() };
    let cit = corp_map.iter().find(|(k, _)| k.as_i64() == Some(st_id)).map(|(_, v)| v).unwrap();
    assert_eq!(kv(cit, "fuelBlocks"), Some(&PyRep::Integer(3_000)));
    assert_eq!(kv(cit, "profileID"), Some(&PyRep::Integer(prof)));

    // ACL Guest: listed in dockable, masked sensitive data
    let mut guest_sess = SessionState::new(632);
    guest_sess.user_id = Some(5);
    guest_sess.character_id = Some(guest); guest_sess.corporation_id = Some(98_000_444); guest_sess.solar_system_id = Some(RANCER_SYS);
    let PyRep::List(d_list) = rpc(&ctx, &mut guest_sess, "structureDirectory", "GetMyDockableStructures", &[PyRep::Integer(RANCER_SYS as i64)]).await.unwrap() else { panic!() };
    assert!(d_list.contains(&PyRep::Integer(st_id)));

    let g_info = rpc(&ctx, &mut guest_sess, "structureDirectory", "GetStructureInfo", &[PyRep::Integer(st_id)]).await.unwrap();
    assert_eq!(kv(&g_info, "fuelBlocks"), Some(&PyRep::None));
    assert_eq!(kv(&g_info, "profileID"), Some(&PyRep::None));

    // Stranger: denied
    let mut str_sess = SessionState::new(633);
    str_sess.user_id = Some(6);
    str_sess.character_id = Some(stranger); str_sess.corporation_id = Some(98_000_445); str_sess.solar_system_id = Some(JITA_SYS);
    assert_eq!(rpc(&ctx, &mut str_sess, "structureDirectory", "GetStructureInfo", &[PyRep::Integer(st_id)]).await.unwrap_err().code, "StructureAccessDenied");
    let PyRep::List(s_dock) = rpc(&ctx, &mut str_sess, "structureDirectory", "GetMyDockableStructures", &[PyRep::Integer(RANCER_SYS as i64)]).await.unwrap() else { panic!() };
    assert!(!s_dock.contains(&PyRep::Integer(st_id)));
}

#[tokio::test]
async fn test_fulcrum_docking_allegiance_gating_and_alignment_speed_buff() {
    // parity: feat:structures.fulcrum-alignment-services
    eve_testkit::attest(&["feat:structures.fulcrum-alignment-services"]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);

    let (cid, ship) = (90_000_501, 100_000_501);
    let mut sess = setup_corp_session(&ctx, 641, cid, "PIRT", ZARZAKH_SYS, ship).await;
    ctx.db.with_mut_snapshot(|s| { s.items.get_mut(&ship).unwrap().type_id = 621; });
    let z_handle = ctx.systems.get_or_spawn(ZARZAKH_SYS);
    let fulcrum_pos = ctx.sde.stations.get(&FULCRUM_ID).map(|s| s.position).unwrap_or(DVec3::ZERO);
    let _ = z_handle.spawn_ball(Ball {
        id: ship, type_id: 621, position: fulcrum_pos + DVec3::new(100.0, 0.0, 0.0), velocity: DVec3::ZERO,
        max_velocity: 250.0, inertia: 0.5, mass: 1e6, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop,
    }).await;

    // 1. Empire militia refused
    ctx.db.with_mut_snapshot(|s| { s.characters.get_mut(&cid).unwrap().war_faction_id = Some(500_001); });
    let emp_err = rpc(&ctx, &mut sess, "stationSvc", "Dock", &[PyRep::Integer(FULCRUM_ID), PyRep::Integer(ship)]).await.unwrap_err();
    assert_eq!(emp_err.code, "FulcrumRefusesEmpireMilitia");

    // 2. Active weapons & retribution timers refuse docking
    ctx.db.with_mut_snapshot(|s| { s.characters.get_mut(&cid).unwrap().war_faction_id = None; });
    let now = ctx.now_unix();
    ctx.crimewatch.write().unwrap().states.entry(cid).or_default().weapon_timer_until_unix = now + 60;
    assert_eq!(rpc(&ctx, &mut sess, "stationSvc", "Dock", &[PyRep::Integer(FULCRUM_ID), PyRep::Integer(ship)]).await.unwrap_err().code, "FulcrumRefusesTimerActive");
    ctx.crimewatch.write().unwrap().states.entry(cid).or_default().weapon_timer_until_unix = 0;

    eve_server::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| { rt.disapproval_flags.insert(cid, now + 300); });
    assert_eq!(rpc(&ctx, &mut sess, "stationSvc", "Dock", &[PyRep::Integer(FULCRUM_ID), PyRep::Integer(ship)]).await.unwrap_err().code, "FulcrumRefusesTimerActive");
    eve_server::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| { rt.disapproval_flags.remove(&cid); });

    // 3. Neutral pilot with 15m PvP timer allowed to dock
    ctx.crimewatch.write().unwrap().states.entry(cid).or_default().pvp_timer_until_unix = now + 900;
    assert!(rpc(&ctx, &mut sess, "stationSvc", "Dock", &[PyRep::Integer(FULCRUM_ID), PyRep::Integer(ship)]).await.is_ok());
    sess.station_id = Some(FULCRUM_ID);

    // 4. Standard station services
    assert!(rpc(&ctx, &mut sess, "repairSvc", "GetRepairQuotes", &[]).await.is_ok());
    assert!(rpc(&ctx, &mut sess, "insuranceSvc", "GetInsurancePrices", &[]).await.is_ok());

    // 5. Jump clone: neutral refused, pirate allowed
    assert_eq!(rpc(&ctx, &mut sess, "jumpCloneSvc", "InstallCloneInStation", &[]).await.unwrap_err().code, "FulcrumCloneRequiresPirateEnlistment");
    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&cid).unwrap().war_faction_id = Some(500_010);
        s.skills.entry(cid).or_default().insert(24242, eve_db::TrainedSkillRecord { type_id: 24242, level: 5, skill_points: 256_000.0 });
    });
    assert!(rpc(&ctx, &mut sess, "jumpCloneSvc", "InstallCloneInStation", &[]).await.is_ok());

    // 6. LP Store offers for Angel Cartel (1000138)
    let PyRep::List(a_offers) = rpc(&ctx, &mut sess, "LPStoreMgr", "GetAvailableOffersFromCorp", &[PyRep::Integer(1_000_138)]).await.unwrap() else { panic!() };
    assert!(!a_offers.is_empty());

    // 7. Alignment speed service buff (20% faster alignment / 0.80x inertia multiplier)
    let pirate_mult = ctx.db.with_snapshot(|s| eve_server::services::station_structure_handlers::fulcrum_alignment_inertia_multiplier(s, cid, Some(FULCRUM_ID), ZARZAKH_SYS));
    assert!((pirate_mult - 0.80).abs() < 1e-6);
    let neutral_mult = ctx.db.with_snapshot(|s| eve_server::services::station_structure_handlers::fulcrum_alignment_inertia_multiplier(s, 90_000_999, Some(FULCRUM_ID), ZARZAKH_SYS));
    assert!((neutral_mult - 1.0).abs() < 1e-6);

    // Undock from Fulcrum applies buff
    assert!(rpc(&ctx, &mut sess, "stationSvc", "Undock", &[]).await.is_ok());
    let ball_snap = z_handle.get_ball_snapshot(ship).await.unwrap().unwrap().1;
    let base_agility = ctx.sde.types.get(&621).and_then(|t| t.default_attributes.get(&eve_sde::attr_ids::AGILITY).copied()).unwrap_or(1.0);
    assert!((ball_snap.inertia - base_agility * 0.80).abs() < 1e-4);
}
