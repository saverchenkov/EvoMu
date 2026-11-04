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

#[allow(dead_code, unused_imports)]
pub(crate) mod upwell_structures_core {
    use super::*;

//  End-to-end remediation tests for Upwell anchoring restrictions (`feat:structures.anchor-restrictions`)
//  and Pochven anchoring / Customs Office standing rules (`feat:structures.pochven-anchoring-restrictions`).

use eve_db::{InventoryFlag, InventoryItemRecord, UpwellStructureRecord};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use eve_server::services::station_structure::structure_lifecycle as life;
use glam::DVec3;
use std::collections::HashMap;
use std::sync::Arc;

const OWNER: i64 = 90_000_001;
const PERIMETER: u32 = 30_000_144;
const RANCER: u32 = 30_002_718;
const HED_GP: u32 = 30_001_161;
const KINO_POCHVEN: u32 = 30_000_157;
const WH_C2: u32 = 31_000_100;

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None).await
}

fn kv_i64(rep: &PyRep, key: &str) -> i64 {
    rep.dict_get(key).and_then(|v| v.as_i64()).unwrap_or(0)
}

fn kv_str(rep: &PyRep, key: &str) -> String {
    rep.dict_get(key).and_then(|v| v.as_str()).unwrap_or("").to_string()
}

fn make_structure(
    structure_id: i64,
    type_id: u32,
    owner_corp_id: i64,
    solar_system_id: u32,
    name: &str,
    pos: DVec3,
    quantum_core_installed: bool,
) -> UpwellStructureRecord {
    UpwellStructureRecord {
        structure_id,
        type_id,
        owner_corp_id,
        solar_system_id,
        name: name.into(),
        state: life::STATE_SHIELD_VULNERABLE.into(),
        position: pos.to_array(),
        quantum_core_installed,
        fuel_blocks: 1_000,
        shield_hp: 1_000_000.0,
        armor_hp: 1_000_000.0,
        hull_hp: 1_000_000.0,
        reinforced_until_unix: 0,
        last_damage_tick: 0,
        last_damage_unix: 0,
        self_destruct_at_unix: None,
    }
}

fn grant(
    ctx: &ServerContext,
    owner: i64,
    location: i64,
    type_id: u32,
    flag: InventoryFlag,
    qty: i32,
) -> i64 {
    ctx.db.with_mut_snapshot(|snap| {
        snap.next_entity_id += 1;
        let id = snap.next_entity_id;
        snap.items.insert(
            id,
            InventoryItemRecord {
                item_id: id,
                type_id,
                owner_id: owner,
                location_id: location,
                flag,
                quantity: qty,
                singleton: qty == 1,
                version: 1,
                ..Default::default()
            },
        );
        id
    })
}

fn new_ctx() -> Arc<ServerContext> {
    let ctx = Arc::new(ServerContext::new());
    ctx.set_clock(Arc::new(eve_db::VirtualServerClock::with_fixed_base(1_800_000_000)));
    ctx
}

async fn login_with_new_corp(
    ctx: &ServerContext,
    sess_id: u64,
    sys_id: u32,
    corp_name: &str,
    ticker: &str,
) -> SessionState {
    let station_id = ctx
        .sde
        .stations
        .values()
        .find(|s| s.solar_system_id == sys_id)
        .map(|s| s.station_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&OWNER) {
            let ship_id = ch.active_ship_id;
            ch.solar_system_id = sys_id;
            ch.station_id = station_id;
            if let Some(sh) = snap.items.get_mut(&ship_id) {
                sh.location_id = station_id.unwrap_or(i64::from(sys_id));
            }
        }
    });
    let mut sess = SessionState::new(sess_id);
    rpc(ctx, &mut sess, "authentication", "Login", &[PyRep::String("demo".into()), PyRep::String("demo-secret".into())])
        .await
        .expect("Login");
    rpc(ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(OWNER)])
        .await
        .expect("SelectCharacterID");
    ctx.advance_secs(12);
    rpc(
        ctx,
        &mut sess,
        "corpRegistry",
        "AddCorporation",
        &[PyRep::String(corp_name.into()), PyRep::String(ticker.into())],
    )
    .await
    .expect("AddCorporation");
    if station_id.is_some() {
        rpc(ctx, &mut sess, "structureDocking", "Undock", &[]).await.expect("Undock");
    } else {
        let ship_id = sess.ship_id.expect("ship");
        sess.station_id = None;
        sess.solar_system_id = Some(sys_id);
        let _ = ctx
            .systems
            .get_or_spawn(sys_id)
            .spawn_ball_with_combat(
                Ball {
                    id: ship_id,
                    type_id: 587,
                    position: DVec3::new(0.0, 5.0e9, 0.0),
                    velocity: DVec3::ZERO,
                    radius: 40.0,
                    mass: 1.0e6,
                    mode: MovementMode::Stop,
                    speed_fraction: 0.0,
                    max_velocity: 300.0,
                    inertia: 1.0,
                },
                ctx.build_type_combat_state(587),
            )
            .await;
    }
    sess
}

async fn move_pilot_to_system(ctx: &ServerContext, sess: &mut SessionState, sys_id: u32, pos: DVec3) {
    let ship_id = sess.ship_id.expect("ship");
    sess.solar_system_id = Some(sys_id);
    sess.station_id = None;
    sess.structure_id = None;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&OWNER) {
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
        }
        if let Some(sh) = snap.items.get_mut(&ship_id) {
            sh.location_id = i64::from(sys_id);
        }
    });
    let sys = ctx.systems.get_or_spawn(sys_id);
    let _ = sys
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: 587,
                position: pos,
                velocity: DVec3::ZERO,
                radius: 40.0,
                mass: 1.0e6,
                mode: MovementMode::Stop,
                speed_fraction: 0.0,
                max_velocity: 300.0,
                inertia: 1.0,
            },
            ctx.build_type_combat_state(587),
        )
        .await;
    let _ = sys.set_ball_position(ship_id, pos).await;
}

async fn try_anchor(
    ctx: &ServerContext,
    sess: &mut SessionState,
    type_id: u32,
    pos_arg: Option<DVec3>,
    name: &str,
) -> Result<PyRep, BridgeRpcError> {
    let ship_id = sess.ship_id.expect("ship");
    let item_id = grant(ctx, OWNER, ship_id, type_id, InventoryFlag::Cargo, 1);
    let res = match pos_arg {
        Some(p) => {
            rpc(
                ctx,
                sess,
                "structureDeployment",
                "Anchor",
                &[
                    PyRep::Integer(item_id),
                    PyRep::List(vec![PyRep::Real(p.x), PyRep::Real(p.y), PyRep::Real(p.z)]),
                    PyRep::Integer(1),
                    PyRep::Tuple(vec![PyRep::Integer(2), PyRep::Integer(18)]),
                    PyRep::String(name.into()),
                ],
            )
            .await
        }
        None => {
            rpc(
                ctx,
                sess,
                "structureDeployment",
                "Anchor",
                &[PyRep::Integer(item_id), PyRep::String(name.into())],
            )
            .await
        }
    };
    if res.is_err() {
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.remove(&item_id);
        });
    }
    res
}

#[tokio::test]
async fn test_remediated_upwell_anchoring_restrictions_end_to_end() {
    // parity: feat:structures.anchor-restrictions
    eve_testkit::attest(&["feat:structures.anchor-restrictions"]);

    let ctx = new_ctx();
    let mut sess = login_with_new_corp(&ctx, 8101, PERIMETER, "Anchor Rules Corp", "ANCR").await;
    let open_perim = DVec3::new(0.0, 5.0e9, 0.0);
    move_pilot_to_system(&ctx, &mut sess, PERIMETER, open_perim).await;

    // 0. Player corporations younger than 7 days (604,800 s) cannot anchor Upwell structures, preserving the cargo item.
    let young_item = grant(&ctx, OWNER, sess.ship_id.unwrap(), 35832, InventoryFlag::Cargo, 1);
    assert_eq!(
        rpc(&ctx, &mut sess, "structureDeployment", "Anchor", &[PyRep::Integer(young_item), PyRep::String("Young Corp Citadel".into())])
            .await
            .expect_err("corp age 0s < 7d")
            .code,
        "CorporationTooYoungToAnchor"
    );
    assert!(ctx.db.get_item(young_item).is_some(), "Rejected anchor must leave structure item in cargo");
    ctx.advance_virtual_clock(7 * 86_400 - 1);
    assert_eq!(
        rpc(&ctx, &mut sess, "structureDeployment", "Anchor", &[PyRep::Integer(young_item), PyRep::String("6d23h59m59s Corp".into())])
            .await
            .expect_err("corp age 7d-1s < 7d")
            .code,
        "CorporationTooYoungToAnchor"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.remove(&young_item);
    });
    ctx.advance_virtual_clock(1);

    // 1. Trade hub systems (Jita, Amarr, Dodixie, Rens, Hek) reject Upwell anchoring.
    for hub_sys in [30_000_142u32, 30_002_187, 30_002_659, 30_002_510, 30_002_053] {
        move_pilot_to_system(&ctx, &mut sess, hub_sys, DVec3::new(0.0, 5.0e9, 0.0)).await;
        assert_eq!(
            try_anchor(&ctx, &mut sess, 35832, None, "Trade Hub Citadel").await.expect_err("trade hub").code,
            "AnchoringDisallowedInTradeHub"
        );
    }

    // 3. Starter and career-agent systems reject Upwell anchoring.
    for starter_sys in [30_000_141u32, 30_030_141] {
        move_pilot_to_system(&ctx, &mut sess, starter_sys, DVec3::new(0.0, 5.0e9, 0.0)).await;
        assert_eq!(
            try_anchor(&ctx, &mut sess, 35832, None, "Starter Citadel").await.expect_err("starter sys").code,
            "AnchoringDisallowedInStarterOrCareerSystem"
        );
    }

    // 4. Explicit ship-to-anchor coordinate distance [50 km, 75 km] and celestial/stargate 1,000 km clearance.
    move_pilot_to_system(&ctx, &mut sess, PERIMETER, open_perim).await;
    assert_eq!(
        try_anchor(&ctx, &mut sess, 35832, Some(open_perim + DVec3::new(30_000.0, 0.0, 0.0)), "Too Close Ship")
            .await
            .expect_err("30 km < 50 km")
            .code,
        "InvalidAnchorDistanceFromShip"
    );
    assert_eq!(
        try_anchor(&ctx, &mut sess, 35832, Some(open_perim + DVec3::new(90_000.0, 0.0, 0.0)), "Too Far Ship")
            .await
            .expect_err("90 km > 75 km")
            .code,
        "InvalidAnchorDistanceFromShip"
    );

    let gate = ctx
        .sde
        .celestials_in_system(PERIMETER)
        .into_iter()
        .find(|c| c.group_id == 10)
        .expect("Perimeter stargate");
    move_pilot_to_system(&ctx, &mut sess, PERIMETER, gate.position + DVec3::new(gate.radius + 500_000.0, 0.0, 0.0)).await;
    assert_eq!(
        try_anchor(&ctx, &mut sess, 35832, None, "Near Gate Citadel").await.expect_err("near gate").code,
        "TooCloseToStructureOrStation"
    );

    move_pilot_to_system(&ctx, &mut sess, PERIMETER, open_perim).await;
    let valid_rep = try_anchor(&ctx, &mut sess, 35832, Some(open_perim + DVec3::new(60_000.0, 0.0, 0.0)), "Valid Perimeter Astrahus")
        .await
        .expect("60 km anchor succeeds");
    assert!(kv_i64(&valid_rep, "structureID") > 0);

    // 5. FLEX structures require K-space sov nullsec, system sovereignty + Sov Hub ownership, online Sov Hub upgrade, 500 km structure clearance, and 100,000 km stargate clearance (Ansiblex).
    for (bad_sys, label) in [(PERIMETER, "hs"), (RANCER, "ls"), (WH_C2, "wh")] {
        move_pilot_to_system(&ctx, &mut sess, bad_sys, DVec3::new(0.0, 5.0e9, 0.0)).await;
        assert_eq!(
            try_anchor(&ctx, &mut sess, 35841, None, label).await.expect_err("flex outside sov null").code,
            "CannotAnchorFlexOutsideSovNull"
        );
    }

    let my_corp = sess.corporation_id.expect("corp");
    let my_ally = 99_810_101;
    let foreign_corp = 98_810_999;
    let foreign_ally = 99_810_999;
    let hub_id = 880_810_101;
    let upg_id = 880_810_102;
    let hed_gate = ctx
        .sde
        .celestials_in_system(HED_GP)
        .into_iter()
        .find(|c| c.group_id == 10)
        .expect("HED-GP gate");
    move_pilot_to_system(&ctx, &mut sess, HED_GP, DVec3::new(1_000_000.0, 5.0e9, 0.0)).await;

    // 5a. Foreign alliance holds system sov + Sov Hub (with upgrade online) -> rejected with FlexAnchoringRequiresSystemSovereignty.
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.insert(
            foreign_corp,
            eve_db::CorporationRecord {
                corporation_id: foreign_corp,
                alliance_id: Some(foreign_ally),
                ..Default::default()
            },
        );
        snap.system_states.insert(
            HED_GP,
            eve_db::SystemDynamicState {
                solar_system_id: HED_GP,
                sov_alliance_id: Some(foreign_ally),
                ..Default::default()
            },
        );
        snap.structures.insert(
            hub_id,
            make_structure(hub_id, 32458, foreign_corp, HED_GP, "Foreign Sov Hub", DVec3::new(0.0, 5.0e9, 0.0), true),
        );
        snap.items.insert(
            upg_id,
            InventoryItemRecord {
                item_id: upg_id,
                type_id: 81_621,
                owner_id: foreign_corp,
                location_id: hub_id,
                flag: InventoryFlag::ServiceSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(upg_id, true);
        snap.sov_runtime.sov_hubs.insert(
            hub_id,
            eve_db::PersistedSovHubData {
                hub_id,
                upgrades: vec![eve_db::PersistedHubUpgrade { type_id: 81_621, online: true, low_power: false }],
                ..Default::default()
            },
        );
    });
    assert_eq!(
        try_anchor(&ctx, &mut sess, 35841, None, "Ansiblex Foreign Sov").await.expect_err("foreign sov").code,
        "FlexAnchoringRequiresSystemSovereignty"
    );

    // 5b. Transfer system sov & Sov Hub ownership to caller's alliance (with upgrade offline first -> RequiredSovUpgradeOffline).
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.get_mut(&my_corp).unwrap().alliance_id = Some(my_ally);
        snap.system_states.get_mut(&HED_GP).unwrap().sov_alliance_id = Some(my_ally);
        snap.structures.get_mut(&hub_id).unwrap().owner_corp_id = my_corp;
        snap.items.remove(&upg_id);
        snap.module_online_states.remove(&upg_id);
        snap.sov_runtime.sov_hubs.remove(&hub_id);
    });
    assert_eq!(
        try_anchor(&ctx, &mut sess, 35841, None, "Ansiblex No Upgrade").await.expect_err("no upgrade").code,
        "RequiredSovUpgradeOffline"
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            upg_id,
            InventoryItemRecord {
                item_id: upg_id,
                type_id: 81_621, // Advanced Logistics Network (Ansiblex upgrade)
                owner_id: my_corp,
                location_id: hub_id,
                flag: InventoryFlag::ServiceSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(upg_id, true);
        snap.sov_runtime.sov_hubs.insert(
            hub_id,
            eve_db::PersistedSovHubData {
                hub_id,
                upgrades: vec![eve_db::PersistedHubUpgrade { type_id: 81_621, online: true, low_power: false }],
                ..Default::default()
            },
        );
    });
    // 50,000 km < 100,000 km stargate clearance for Ansiblex -> rejected
    move_pilot_to_system(&ctx, &mut sess, HED_GP, hed_gate.position + DVec3::new(hed_gate.radius + 50_000_000.0, 0.0, 0.0)).await;
    assert_eq!(
        try_anchor(&ctx, &mut sess, 35841, None, "Ansiblex Too Close Gate").await.expect_err("50,000 km < 100,000 km").code,
        "TooCloseToStructureOrStation"
    );
    // 120,000 km from gate, 300 km from Sov Hub (< 500 km FLEX structure clearance) -> rejected
    move_pilot_to_system(&ctx, &mut sess, HED_GP, DVec3::new(300_000.0, 5.0e9, 0.0)).await;
    assert_eq!(
        try_anchor(&ctx, &mut sess, 35841, None, "Ansiblex Too Close Hub").await.expect_err("300 km < 500 km").code,
        "TooCloseToStructureOrStation"
    );
    // 1,000 km from Sov Hub and > 100,000 km from gates -> succeeds with 45-minute (2,700s) anchoring timer and default +60 km ship offset!
    let ans_ship_pos = DVec3::new(1_000_000.0, 5.0e9, 0.0);
    move_pilot_to_system(&ctx, &mut sess, HED_GP, ans_ship_pos).await;
    let ans_id = kv_i64(&try_anchor(&ctx, &mut sess, 35841, None, "HED-GP Ansiblex").await.expect("valid Ansiblex"), "structureID");
    let ans_pos = DVec3::from_array(ctx.db.with_snapshot(|s| s.structures[&ans_id].position));
    assert!((ans_pos.distance(ans_ship_pos) - 60_000.0).abs() < 1.0, "Default 2-arg anchor must offset 60 km from ship");
    ctx.advance_virtual_clock(life::ANCHOR_VULNERABLE_SECS + 2_700 + life::ONLINING_SECS + 5);
    life::sync_all_structures(&ctx);
    assert_eq!(ctx.db.with_snapshot(|s| s.structures[&ans_id].state.clone()), life::STATE_SHIELD_VULNERABLE);

    // 5c. V24 10-positional-arg Anchor + Strategic Index scaling on FLEX structures (2,700s * (1 + strategic_index)).
    let pharo_upg_id = 880_810_103;
    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states.get_mut(&HED_GP).unwrap().strategic_index = 2;
        snap.items.insert(pharo_upg_id, InventoryItemRecord {
            item_id: pharo_upg_id, type_id: 81_615, owner_id: my_corp, location_id: hub_id,
            flag: InventoryFlag::ServiceSlot1, quantity: 1, singleton: true, version: 1, ..Default::default()
        });
        snap.module_online_states.insert(pharo_upg_id, true);
        snap.sov_runtime.sov_hubs.get_mut(&hub_id).unwrap().upgrades.push(eve_db::PersistedHubUpgrade { type_id: 81_615, online: true, low_power: false });
    });
    let pharo_ship_pos = DVec3::new(2_000_000.0, 5.0e9, 0.0);
    move_pilot_to_system(&ctx, &mut sess, HED_GP, pharo_ship_pos).await;
    let pharo_item = grant(&ctx, OWNER, sess.ship_id.unwrap(), 35840, InventoryFlag::Cargo, 1);
    assert_eq!(
        rpc(&ctx, &mut sess, "structureDeployment", "Anchor", &[
            PyRep::Integer(pharo_item), PyRep::Real(pharo_ship_pos.x + 20_000.0), PyRep::Real(pharo_ship_pos.z),
            PyRep::Real(0.0), PyRep::Integer(0), PyRep::Integer(3), PyRep::Integer(19),
            PyRep::String("Too Close 10-Arg Pharolux".into()), PyRep::String("desc".into()), PyRep::None,
        ]).await.expect_err("10-arg 20 km < 50 km").code,
        "InvalidAnchorDistanceFromShip"
    );
    let pharo_rep = rpc(&ctx, &mut sess, "structureDeployment", "Anchor", &[
        PyRep::Integer(pharo_item), PyRep::Real(pharo_ship_pos.x + 60_000.0), PyRep::Real(pharo_ship_pos.z),
        PyRep::Real(0.0), PyRep::Integer(0), PyRep::Integer(3), PyRep::Integer(19),
        PyRep::String("HED-GP Pharolux".into()), PyRep::String("V24 10-arg FLEX beacon".into()), PyRep::None,
    ]).await.expect("V24 10-arg Anchor succeeds at 60 km");
    let pharo_id = kv_i64(&pharo_rep, "structureID");
    assert_eq!(
        rpc(&ctx, &mut sess, "structureDirectory", "GetStructureDescription", &[PyRep::Integer(pharo_id)]).await.unwrap(),
        PyRep::String("V24 10-arg FLEX beacon".into())
    );
    assert_eq!(ctx.db.with_snapshot(|s| s.structure_vulnerability_windows.get(&pharo_id).copied()), Some((19, 3)));
    ctx.advance_virtual_clock(life::ANCHOR_VULNERABLE_SECS + 2_700 + life::ONLINING_SECS + 10);
    life::sync_all_structures(&ctx);
    assert_eq!(ctx.db.with_snapshot(|s| s.structures[&pharo_id].state.clone()), life::STATE_ANCHORING, "Strategic Index 2 FLEX needs 8,100s");
    ctx.advance_virtual_clock(2 * 2_700);
    life::sync_all_structures(&ctx);
    assert_eq!(ctx.db.with_snapshot(|s| s.structures[&pharo_id].state.clone()), life::STATE_SHIELD_VULNERABLE);

    // 6. Foreign Sov Null-Sec ADM > 4.0 blocks Medium structures, and Strategic Index adds +24h per level.
    let hostile_ally = 99_810_202;
    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states.insert(
            HED_GP,
            eve_db::SystemDynamicState {
                solar_system_id: HED_GP,
                sov_alliance_id: Some(hostile_ally),
                military_index: 5,
                industrial_index: 5,
                strategic_index: 2,
                ..Default::default()
            },
        );
    });
    move_pilot_to_system(&ctx, &mut sess, HED_GP, DVec3::new(3_000_000.0, 5.0e9, 0.0)).await;
    assert_eq!(
        try_anchor(&ctx, &mut sess, 35832, None, "Blocked Hostile Medium").await.expect_err("ADM > 4.0 blocks Medium").code,
        "AdmBlocksHostileMediumStructure"
    );

    // Lower ADM <= 4.0 while keeping strategic_index = 2 -> anchoring timer becomes 24h + 2 * 24h = 72h!
    ctx.db.with_mut_snapshot(|snap| {
        let st = snap.system_states.get_mut(&HED_GP).unwrap();
        st.military_index = 1;
        st.industrial_index = 1;
        st.strategic_index = 2;
    });
    let hostile_med_id = kv_i64(
        &try_anchor(&ctx, &mut sess, 35832, None, "Hostile Sov Astrahus").await.expect("ADM <= 4.0 allows Medium"),
        "structureID",
    );
    ctx.advance_virtual_clock(life::ANCHOR_VULNERABLE_SECS + life::ANCHORING_SECS + 10);
    life::sync_all_structures(&ctx);
    assert_eq!(
        ctx.db.with_snapshot(|s| s.structures[&hostile_med_id].state.clone()),
        life::STATE_ANCHORING,
        "At 24h, Strategic Index 2 hostile sov structure must still be Anchoring (needs 72h)"
    );
    ctx.advance_virtual_clock(2 * 86_400);
    life::sync_all_structures(&ctx);
    assert_eq!(
        ctx.db.with_snapshot(|s| s.structures[&hostile_med_id].state.clone()),
        life::STATE_NO_CORE,
        "At 72h (24h + 2 * 24h), Strategic Index 2 structure finishes anchoring"
    );
}

#[tokio::test]
async fn test_remediated_pochven_anchoring_and_customs_office_standing() {
    // parity: feat:structures.pochven-anchoring-restrictions
    eve_testkit::attest(&["feat:structures.pochven-anchoring-restrictions"]);

    let ctx = new_ctx();
    let mut sess = login_with_new_corp(&ctx, 8102, PERIMETER, "Pochven Ops Corp", "POCH").await;
    ctx.advance_virtual_clock(7 * 86_400 + 25);

    let planet = ctx
        .sde
        .celestials_in_system(KINO_POCHVEN)
        .into_iter()
        .find(|c| c.group_id == 7)
        .expect("Kino planet");
    let near_planet = planet.position + DVec3::new(planet.radius + 5_000_000.0, 0.0, 0.0);
    move_pilot_to_system(&ctx, &mut sess, KINO_POCHVEN, near_planet).await;

    // 1. Upwell structures cannot be anchored in Pochven.
    assert_eq!(
        try_anchor(&ctx, &mut sess, 35832, None, "Pochven Astrahus").await.expect_err("Upwell in Pochven").code,
        "AnchoringDisallowedInPochven"
    );
    assert_eq!(
        try_anchor(&ctx, &mut sess, 2233, None, "Pochven POCO via Upwell").await.expect_err("POCO via Upwell RPC").code,
        "AnchoringDisallowedInPochven"
    );

    // 2. Pre-existing Upwell structure in Pochven survives sync, allows docking, and blocks Quantum Core insertion.
    let existing_id = 880_810_201;
    let my_corp = sess.corporation_id.expect("corp");
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            existing_id,
            make_structure(existing_id, 35832, my_corp, KINO_POCHVEN, "Legacy Kino Citadel", near_planet, false),
        );
    });
    life::sync_all_structures(&ctx);
    assert!(ctx.db.with_snapshot(|s| s.structures.contains_key(&existing_id)), "Pre-existing Pochven Upwell structure must survive");
    rpc(&ctx, &mut sess, "structureDocking", "Dock", &[PyRep::Integer(existing_id)])
        .await
        .expect("Dock at pre-existing Pochven Upwell structure");
    let core_item = grant(&ctx, OWNER, sess.ship_id.unwrap(), 56201, InventoryFlag::Cargo, 1);
    assert_eq!(
        rpc(&ctx, &mut sess, "invbroker", "Add", &[PyRep::Integer(core_item), PyRep::Integer(existing_id), PyRep::Integer(172)])
            .await
            .expect_err("Quantum core insertion blocked in Pochven")
            .code,
        "QuantumCoreDisallowedInPochven"
    );
    rpc(&ctx, &mut sess, "structureDocking", "Undock", &[]).await.expect("Undock");
    move_pilot_to_system(&ctx, &mut sess, KINO_POCHVEN, near_planet).await;

    // 3. Customs Office Gantry (3962) & upgrade to Customs Office (2233) require >= 1.5 Triglavian standing (500026).
    let ship_id = sess.ship_id.unwrap();
    let gantry_id = grant(&ctx, OWNER, ship_id, 3962, InventoryFlag::Cargo, 1);
    let poco_item_id = grant(&ctx, OWNER, ship_id, 2233, InventoryFlag::Cargo, 1);
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&OWNER).unwrap().standings.insert(500_026, 1.49);
    });
    assert_eq!(
        rpc(&ctx, &mut sess, "posMgr", "AnchorOrbital", &[PyRep::Integer(gantry_id), PyRep::Integer(planet.celestial_id)])
            .await
            .expect_err("1.49 standing < 1.5")
            .code,
        "InsufficientTriglavianStanding"
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&OWNER).unwrap().standings.insert(500_026, 1.50);
    });
    let orb_rep = rpc(&ctx, &mut sess, "posMgr", "AnchorOrbital", &[PyRep::Integer(gantry_id), PyRep::Integer(planet.celestial_id)])
        .await
        .expect("1.50 standing allows AnchorOrbital");
    assert_eq!(kv_i64(&orb_rep, "orbitalID"), gantry_id);
    assert_eq!(kv_str(&orb_rep, "state"), "Anchored");

    // Drop standing below 1.5 before OnlineOrbital -> rejected with InsufficientTriglavianStanding.
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&OWNER).unwrap().standings.insert(500_026, 1.25);
    });
    assert_eq!(
        rpc(&ctx, &mut sess, "posMgr", "OnlineOrbital", &[PyRep::Integer(gantry_id), PyRep::Integer(planet.celestial_id)])
            .await
            .expect_err("OnlineOrbital requires >= 1.5 standing")
            .code,
        "InsufficientTriglavianStanding"
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&OWNER).unwrap().standings.insert(500_026, 1.75);
    });
    let upg_rep = rpc(&ctx, &mut sess, "posMgr", "OnlineOrbital", &[PyRep::Integer(gantry_id), PyRep::Integer(planet.celestial_id)])
        .await
        .expect("OnlineOrbital upgrades Gantry to Customs Office");
    assert_eq!(kv_i64(&upg_rep, "typeID"), 2233);
    assert_eq!(kv_str(&upg_rep, "state"), "ShieldVulnerable");
    assert!(ctx.db.get_item(poco_item_id).is_none(), "Customs Office upgrade item consumed");
}

}

#[allow(dead_code, unused_imports)]
pub(crate) mod upwell_structures_docking {
    use super::*;

//  End-to-end remediation tests for structure boarding/control (`feat:structures.board-structure`,
//  `rpc:structureControl.{BoardStructure,EjectFromStructure,GetStructurePilot,ReleaseControl,TakeControl}`)
//  and Upwell defense settings & automated Standup weapon/PDB firing (`feat:structures.defense-settings`,
//  `feat:structures.profiles`).

use eve_db::{InventoryFlag, InventoryItemRecord, UpwellStructureRecord};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use eve_server::services::station_structure::structure_lifecycle as life;
use glam::DVec3;
use std::collections::HashMap;
use std::sync::Arc;

const OWNER: i64 = 90_000_001;
const DIRECTOR_2: i64 = 90_000_002;
const GRUNT: i64 = 90_000_003;
const HOSTILE: i64 = 90_000_004;
const PERIMETER: u32 = 30_000_144;
const RANCER: u32 = 30_002_718;

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None).await
}

fn kv_bool(rep: &PyRep, key: &str) -> bool {
    rep.dict_get(key).and_then(|v| v.as_bool()).unwrap_or(false)
}

fn make_structure(
    structure_id: i64,
    type_id: u32,
    owner_corp_id: i64,
    solar_system_id: u32,
    name: &str,
    pos: DVec3,
    quantum_core_installed: bool,
    fuel_blocks: u32,
) -> UpwellStructureRecord {
    UpwellStructureRecord {
        structure_id,
        type_id,
        owner_corp_id,
        solar_system_id,
        name: name.into(),
        state: life::STATE_SHIELD_VULNERABLE.into(),
        position: pos.to_array(),
        quantum_core_installed,
        fuel_blocks,
        shield_hp: 1_000_000.0,
        armor_hp: 1_000_000.0,
        hull_hp: 1_000_000.0,
        reinforced_until_unix: 0,
        last_damage_tick: 0,
        last_damage_unix: 0,
        self_destruct_at_unix: None,
    }
}

fn new_ctx() -> Arc<ServerContext> {
    let ctx = Arc::new(ServerContext::new());
    ctx.set_clock(Arc::new(eve_db::VirtualServerClock::with_fixed_base(1_800_000_000)));
    ctx
}

async fn login_with_new_corp(
    ctx: &ServerContext,
    sess_id: u64,
    sys_id: u32,
    corp_name: &str,
    ticker: &str,
) -> SessionState {
    let station_id = ctx
        .sde
        .stations
        .values()
        .find(|s| s.solar_system_id == sys_id)
        .map(|s| s.station_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&OWNER) {
            let ship_id = ch.active_ship_id;
            ch.solar_system_id = sys_id;
            ch.station_id = station_id;
            if let Some(sh) = snap.items.get_mut(&ship_id) {
                sh.location_id = station_id.unwrap_or(i64::from(sys_id));
            }
        }
    });
    let mut sess = SessionState::new(sess_id);
    rpc(ctx, &mut sess, "authentication", "Login", &[PyRep::String("demo".into()), PyRep::String("demo-secret".into())])
        .await
        .expect("Login");
    rpc(ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(OWNER)])
        .await
        .expect("SelectCharacterID");
    ctx.advance_secs(12);
    rpc(
        ctx,
        &mut sess,
        "corpRegistry",
        "AddCorporation",
        &[PyRep::String(corp_name.into()), PyRep::String(ticker.into())],
    )
    .await
    .expect("AddCorporation");
    ctx.advance_virtual_clock(15);
    if station_id.is_some() {
        rpc(ctx, &mut sess, "structureDocking", "Undock", &[]).await.expect("Undock");
    }
    sess
}

async fn move_pilot_to_system(ctx: &ServerContext, sess: &mut SessionState, sys_id: u32, pos: DVec3) {
    let ship_id = sess.ship_id.expect("ship");
    sess.solar_system_id = Some(sys_id);
    sess.station_id = None;
    sess.structure_id = None;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&OWNER) {
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
        }
        if let Some(sh) = snap.items.get_mut(&ship_id) {
            sh.location_id = i64::from(sys_id);
        }
    });
    let sys = ctx.systems.get_or_spawn(sys_id);
    let _ = sys
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: 587,
                position: pos,
                velocity: DVec3::ZERO,
                radius: 40.0,
                mass: 1.0e6,
                mode: MovementMode::Stop,
                speed_fraction: 0.0,
                max_velocity: 300.0,
                inertia: 1.0,
            },
            ctx.build_type_combat_state(587),
        )
        .await;
    let _ = sys.set_ball_position(ship_id, pos).await;
}

#[tokio::test]
async fn test_remediated_structure_control_and_boarding_lifecycle() {
    // parity: feat:structures.board-structure, rpc:structureControl.BoardStructure, rpc:structureControl.EjectFromStructure, rpc:structureControl.GetStructurePilot, rpc:structureControl.ReleaseControl, rpc:structureControl.TakeControl
    eve_testkit::attest(&[
        "feat:structures.board-structure",
        "rpc:structureControl.BoardStructure",
        "rpc:structureControl.EjectFromStructure",
        "rpc:structureControl.GetStructurePilot",
        "rpc:structureControl.ReleaseControl",
        "rpc:structureControl.TakeControl",
    ]);

    let ctx = new_ctx();
    let mut sess = login_with_new_corp(&ctx, 8103, PERIMETER, "Control Ops Corp", "CTRL").await;
    let corp_id = sess.corporation_id.expect("corp");
    let orig_ship = sess.ship_id.expect("ship");
    let struct_id = 880_810_301;
    let st_pos = DVec3::new(0.0, 5.0e9, 0.0);

    let mut dir2_sess = SessionState::new(8104);
    dir2_sess.user_id = Some(2);
    dir2_sess.character_id = Some(DIRECTOR_2);
    dir2_sess.corporation_id = Some(corp_id);
    dir2_sess.solar_system_id = Some(PERIMETER);
    dir2_sess.structure_id = Some(struct_id);
    dir2_sess.corp_role = eve_db::corp_roles::ROLE_DIRECTOR;

    let mut grunt_sess = SessionState::new(8105);
    grunt_sess.user_id = Some(3);
    grunt_sess.character_id = Some(GRUNT);
    grunt_sess.corporation_id = Some(corp_id);
    grunt_sess.solar_system_id = Some(PERIMETER);
    grunt_sess.structure_id = Some(struct_id);
    grunt_sess.corp_role = 0;

    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            struct_id,
            make_structure(struct_id, 35832, corp_id, PERIMETER, "Command Astrahus", st_pos, true, 1_000),
        );
        let mut c2 = snap.characters.get(&OWNER).cloned().unwrap();
        c2.character_id = DIRECTOR_2;
        c2.corporation_id = corp_id;
        c2.station_id = None;
        c2.structure_id = Some(struct_id);
        snap.characters.insert(DIRECTOR_2, c2);

        let mut c3 = snap.characters.get(&OWNER).cloned().unwrap();
        c3.character_id = GRUNT;
        c3.corporation_id = corp_id;
        c3.station_id = None;
        c3.structure_id = Some(struct_id);
        snap.characters.insert(GRUNT, c3);

        let corp = snap.corporations.get_mut(&corp_id).unwrap();
        corp.members.insert(DIRECTOR_2);
        corp.members.insert(GRUNT);
        corp.member_roles.insert(DIRECTOR_2, eve_db::corp_roles::ROLE_DIRECTOR);
    });
    life::sync_all_structures(&ctx);
    eve_server::services::station_structure::structure_lifecycle_rpc::ensure_structure_ball_in_space(&ctx, PERIMETER, struct_id).await;

    // 1. Undocked pilot cannot board/take control of a dockable structure.
    move_pilot_to_system(&ctx, &mut sess, PERIMETER, st_pos + DVec3::new(1_000.0, 0.0, 0.0)).await;
    assert_eq!(
        rpc(&ctx, &mut sess, "structureControl", "BoardStructure", &[PyRep::Integer(struct_id)])
            .await
            .expect_err("undocked BoardStructure")
            .code,
        "NotDockedInStructure"
    );
    assert_eq!(
        rpc(&ctx, &mut sess, "structureControl", "TakeControl", &[PyRep::Integer(struct_id)])
            .await
            .expect_err("undocked TakeControl")
            .code,
        "NotDockedInStructure"
    );

    // 2. Docked director takes control via BoardStructure; unprivileged grunt and second director are denied.
    rpc(&ctx, &mut sess, "structureDocking", "Dock", &[PyRep::Integer(struct_id)]).await.expect("Dock");
    assert_eq!(
        rpc(&ctx, &mut grunt_sess, "structureControl", "BoardStructure", &[PyRep::Integer(struct_id)])
            .await
            .expect_err("grunt without defense role")
            .code,
        "CorpRoleDenied"
    );
    let _ = sess.drain_notifications();
    let board_rep = rpc(&ctx, &mut sess, "structureControl", "BoardStructure", &[PyRep::Integer(struct_id)])
        .await
        .expect("BoardStructure");
    assert!(kv_bool(&board_rep, "boarded"));
    assert_eq!(sess.ship_id, Some(struct_id));
    assert_eq!(
        rpc(&ctx, &mut sess, "structureControl", "GetStructurePilot", &[PyRep::Integer(struct_id)]).await.unwrap(),
        PyRep::Integer(OWNER)
    );
    assert!(sess.drain_notifications().iter().any(|(ev, _)| ev == "OnSessionChanged"));
    assert_eq!(
        rpc(&ctx, &mut dir2_sess, "structureControl", "TakeControl", &[PyRep::Integer(struct_id)])
            .await
            .expect_err("already controlled")
            .code,
        "StructureAlreadyControlled"
    );

    // 3. EjectFromStructure restores original ship and allows second director to TakeControl; Undock auto-releases control.
    let eject_rep = rpc(&ctx, &mut sess, "structureControl", "EjectFromStructure", &[PyRep::Integer(struct_id)])
        .await
        .expect("EjectFromStructure");
    assert!(!kv_bool(&eject_rep, "boarded"));
    assert_eq!(sess.ship_id, Some(orig_ship));
    assert_eq!(
        rpc(&ctx, &mut sess, "structureControl", "GetStructurePilot", &[PyRep::Integer(struct_id)]).await.unwrap(),
        PyRep::None
    );

    let take2 = rpc(&ctx, &mut dir2_sess, "structureControl", "TakeControl", &[PyRep::Integer(struct_id)])
        .await
        .expect("Director 2 TakeControl");
    assert!(kv_bool(&take2, "success"));
    assert_eq!(
        rpc(&ctx, &mut sess, "structureControl", "GetStructurePilot", &[PyRep::Integer(struct_id)]).await.unwrap(),
        PyRep::Integer(DIRECTOR_2)
    );
    rpc(&ctx, &mut dir2_sess, "structureControl", "ReleaseControl", &[PyRep::Integer(struct_id)])
        .await
        .expect("ReleaseControl");
    assert_eq!(
        rpc(&ctx, &mut sess, "structureControl", "GetStructurePilot", &[PyRep::Integer(struct_id)]).await.unwrap(),
        PyRep::None
    );

    rpc(&ctx, &mut sess, "structureControl", "TakeControl", &[PyRep::Integer(struct_id)]).await.expect("TakeControl before Undock");
    // Failed Undock must NOT prematurely release structure control!
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&orig_ship).unwrap().location_id = 0;
    });
    assert_eq!(
        rpc(&ctx, &mut sess, "structureDocking", "Undock", &[]).await.expect_err("failed undock").code,
        "UndockFailed"
    );
    assert_eq!(sess.ship_id, Some(struct_id));
    assert_eq!(
        rpc(&ctx, &mut sess, "structureControl", "GetStructurePilot", &[PyRep::Integer(struct_id)]).await.unwrap(),
        PyRep::Integer(OWNER)
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&orig_ship).unwrap().location_id = struct_id;
    });
    rpc(&ctx, &mut sess, "structureDocking", "Undock", &[]).await.expect("Undock auto-releases structure control");
    assert_eq!(sess.ship_id, Some(orig_ship));
    assert_eq!(
        rpc(&ctx, &mut sess, "structureControl", "GetStructurePilot", &[PyRep::Integer(struct_id)]).await.unwrap(),
        PyRep::None
    );

    // 4. FLEX structure (Ansiblex 35841) boards in space within radius + 2,500 m without docking.
    let flex_id = 880_810_302;
    let flex_pos = DVec3::new(10_000_000.0, 5.0e9, 0.0);
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            flex_id,
            make_structure(flex_id, 35841, corp_id, PERIMETER, "Command Ansiblex", flex_pos, false, 1_000),
        );
    });
    life::sync_all_structures(&ctx);
    eve_server::services::station_structure::structure_lifecycle_rpc::ensure_structure_ball_in_space(&ctx, PERIMETER, flex_id).await;
    move_pilot_to_system(&ctx, &mut sess, PERIMETER, flex_pos + DVec3::new(50_000.0, 0.0, 0.0)).await;
    assert_eq!(
        rpc(&ctx, &mut sess, "structureControl", "BoardStructure", &[PyRep::Integer(flex_id)])
            .await
            .expect_err("out of FLEX boarding range")
            .code,
        "OutOfBoardingRange"
    );
    move_pilot_to_system(&ctx, &mut sess, PERIMETER, flex_pos + DVec3::new(1_000.0, 0.0, 0.0)).await;
    let flex_board = rpc(&ctx, &mut sess, "structureControl", "BoardStructure", &[PyRep::Integer(flex_id)])
        .await
        .expect("Board FLEX within 2,500m");
    assert!(kv_bool(&flex_board, "boarded"));
    assert_eq!(sess.ship_id, Some(flex_id));
    rpc(&ctx, &mut sess, "structureControl", "EjectFromStructure", &[PyRep::Integer(flex_id)])
        .await
        .expect("Eject from FLEX");
    assert_eq!(sess.ship_id, Some(orig_ship));
}

#[tokio::test]
async fn test_remediated_structure_defense_settings_and_auto_fire_ballpark() {
    // parity: feat:structures.defense-settings, feat:structures.profiles
    eve_testkit::attest(&["feat:structures.defense-settings", "feat:structures.profiles"]);

    let ctx = new_ctx();
    let mut sess = login_with_new_corp(&ctx, 8106, RANCER, "Defense Grid Corp", "DEFG").await;
    let corp_id = sess.corporation_id.expect("corp");
    let struct_id = 880_810_601;
    let st_pos = DVec3::new(0.0, 5.0e9, 0.0);
    let friendly_ship = 880_810_610;
    let hostile_close_ship = 880_810_611;
    let hostile_far_ship = 880_810_612;

    let prof_id = rpc(&ctx, &mut sess, "structureProfiles", "CreateProfile", &[PyRep::String("Perimeter Defense Profile".into()), PyRep::String("Auto defense".into())])
        .await
        .expect("CreateProfile")
        .as_i64()
        .unwrap();

    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            struct_id,
            make_structure(struct_id, 35832, corp_id, RANCER, "Auto-Defense Astrahus", st_pos, true, 2_000),
        );
        let hostile_2 = 90_000_077;
        let mut h_char = snap.characters.get(&OWNER).cloned().unwrap();
        h_char.character_id = HOSTILE;
        h_char.corporation_id = 98_999_666;
        h_char.station_id = None;
        h_char.solar_system_id = RANCER;
        h_char.active_ship_id = hostile_close_ship;
        snap.characters.insert(HOSTILE, h_char);

        let mut h2_char = snap.characters.get(&OWNER).cloned().unwrap();
        h2_char.character_id = hostile_2;
        h2_char.corporation_id = 98_999_667;
        h2_char.station_id = None;
        h2_char.solar_system_id = RANCER;
        h2_char.active_ship_id = hostile_far_ship;
        snap.characters.insert(hostile_2, h2_char);
        snap.corp_contacts.entry(corp_id).or_default().push(eve_db::SocialContactRecord {
            contact_id: hostile_2,
            contact_type: "character".into(),
            standing: -5.0,
            is_watched: false,
            is_blocked: false,
            label_ids: Vec::new(),
        });

        let mut g_char = snap.characters.get(&OWNER).cloned().unwrap();
        g_char.character_id = GRUNT;
        g_char.corporation_id = corp_id;
        g_char.station_id = None;
        g_char.solar_system_id = RANCER;
        g_char.active_ship_id = friendly_ship;
        snap.characters.insert(GRUNT, g_char);

        for (sid, owner_id) in [
            (friendly_ship, GRUNT),
            (hostile_close_ship, HOSTILE),
            (hostile_far_ship, hostile_2),
        ] {
            snap.items.insert(
                sid,
                InventoryItemRecord {
                    item_id: sid,
                    type_id: 587,
                    owner_id,
                    location_id: i64::from(RANCER),
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
        }
        // Fit Standup Multirole Missile Launcher I (35922) + Standup Heavy Missile (37847)
        // and Standup Point Defense Battery I (35926) + Standup Flak Round I (63195).
        let launcher_id = 880_810_620;
        let charge_id = 880_810_621;
        let pdb_id = 880_810_622;
        let pdb_charge_id = 880_810_623;
        for (iid, tid, flag, qty, singleton) in [
            (launcher_id, 35922u32, InventoryFlag::HiSlot0, 1i32, true),
            (charge_id, 37847, InventoryFlag::HiSlot0, 20, false),
            (pdb_id, 35926, InventoryFlag::HiSlot1, 1, true),
            (pdb_charge_id, 63195, InventoryFlag::HiSlot1, 20, false),
        ] {
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: tid,
                    owner_id: corp_id,
                    location_id: struct_id,
                    flag,
                    quantity: qty,
                    singleton,
                    version: 1,
                    ..Default::default()
                },
            );
        }
        snap.module_online_states.insert(launcher_id, true);
        snap.module_online_states.insert(pdb_id, true);
    });
    life::sync_all_structures(&ctx);
    eve_server::services::station_structure::structure_lifecycle_rpc::ensure_structure_ball_in_space(&ctx, RANCER, struct_id).await;

    // Non-client defense RPCs must fall through to UnimplementedRPC.
    let expected_err = "UnimplementedRPC";
    for bad_method in ["GetDefenseSettings", "SetDefenseSettings", "EvaluateDefenseTarget"] {
        assert_eq!(
            rpc(&ctx, &mut sess, "structureControl", bad_method, &[PyRep::Integer(struct_id)])
                .await
                .expect_err(bad_method)
                .code,
            expected_err
        );
    }

    // Assign profile and configure defenseSettings via V24 structureProfiles RPCs.
    rpc(&ctx, &mut sess, "structureProfiles", "ChangeProfiles", &[PyRep::List(vec![PyRep::Integer(struct_id)]), PyRep::Integer(prof_id)])
        .await
        .expect("ChangeProfiles");
    assert_eq!(
        rpc(
            &ctx,
            &mut sess,
            "structureProfiles",
            "SaveProfileSettings",
            &[
                PyRep::Integer(prof_id),
                PyRep::List(vec![
                    PyRep::Tuple(vec![PyRep::Integer(17), PyRep::Integer(999_999_999), PyRep::Integer(-1)]),
                ]),
            ],
        )
        .await
        .expect_err("unknown defenseBlocked entity")
        .code,
        "InvalidAccessEntity"
    );
    rpc(
        &ctx,
        &mut sess,
        "structureProfiles",
        "SaveProfileSettings",
        &[
            PyRep::Integer(prof_id),
            PyRep::List(vec![
                PyRep::Tuple(vec![PyRep::Integer(19), PyRep::Integer(0), PyRep::Integer(1)]),
                PyRep::Tuple(vec![PyRep::Integer(17), PyRep::Integer(HOSTILE), PyRep::Integer(-1)]),
            ]),
        ],
    )
    .await
    .expect("SaveProfileSettings");
    assert_eq!(
        rpc(&ctx, &mut sess, "structureProfiles", "SaveProfileSettings", &[
            PyRep::Integer(prof_id),
            PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(17), PyRep::Tuple(vec![PyRep::Bool(true), PyRep::Bool(false), PyRep::Bool(false), PyRep::Real(f64::NAN), PyRep::Bool(true)]), PyRep::Integer(0)])]),
        ]).await.expect_err("NaN standing threshold").code,
        "InvalidDefenseStandingThreshold"
    );
    macro_rules! set_def {
        ($atk:expr, $war:expr, $acl:expr) => {
            rpc(&ctx, &mut sess, "structureProfiles", "SaveProfileSettings", &[
                PyRep::Integer(prof_id),
                PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(17), PyRep::Tuple(vec![PyRep::Bool(true), PyRep::Bool($atk), PyRep::Bool($war), PyRep::Real(-1.0), PyRep::Bool($acl)]), PyRep::Integer(0)])]),
            ]).await.expect("SaveProfileSettings defense");
        };
    }
    set_def!(false, false, true);

    // Spawn friendly ship at 15 km, hostile_close at 15 km (inside PDB 25 km), and hostile_far at 60 km (outside PDB 25 km).
    let sys = ctx.systems.get_or_spawn(RANCER);
    let spawn_ship = |bid: i64, pos: DVec3| {
        let sys_c = sys.clone();
        let mut cs = ctx.build_type_combat_state(587);
        cs.shield_hp = 5_000.0;
        cs.max_shield_hp = 5_000.0;
        cs.armor_hp = 5_000.0;
        cs.max_armor_hp = 5_000.0;
        async move {
            let _ = sys_c
                .spawn_ball_with_combat(
                    Ball {
                        id: bid,
                        type_id: 587,
                        position: pos,
                        velocity: DVec3::ZERO,
                        radius: 40.0,
                        mass: 1.0e6,
                        mode: MovementMode::Stop,
                        speed_fraction: 0.0,
                        max_velocity: 300.0,
                        inertia: 1.0,
                    },
                    cs,
                )
                .await;
        }
    };
    spawn_ship(friendly_ship, st_pos + DVec3::new(15_000.0, 0.0, 0.0)).await;
    spawn_ship(hostile_close_ship, st_pos + DVec3::new(0.0, 15_000.0, 0.0)).await;
    spawn_ship(hostile_far_ship, st_pos + DVec3::new(0.0, 0.0, 60_000.0)).await;

    // 1. After 5s (< 55.39s lock time): lock is still acquiring (locked=false), directed missiles have NOT fired or consumed ammo,
    // while untargeted PDB fires AoE at 15 km and consumes Flak Rounds!
    let _ = sys.step_manual(5.0).await.expect("step_5s");
    let locks_5 = sys.get_target_locks(struct_id).await.unwrap_or_default();
    assert!(locks_5.iter().any(|l| !l.locked && l.target_id == hostile_close_ship));
    assert_eq!(ctx.db.with_snapshot(|s| s.items[&880_810_621].quantity), 20, "Missiles must not fire before target lock completes");
    assert!(ctx.db.with_snapshot(|s| s.items[&880_810_623].quantity) < 20, "PDB must consume Flak Rounds when firing");
    let friendly_cs = sys.get_combat_state(friendly_ship).await.unwrap().unwrap();
    let close_cs_5 = sys.get_combat_state(hostile_close_ship).await.unwrap().unwrap();
    let far_cs_5 = sys.get_combat_state(hostile_far_ship).await.unwrap().unwrap();
    assert!((friendly_cs.shield_hp - 5_000.0).abs() < 1e-6, "Friendly corp ship must take zero auto-defense damage");
    assert!((far_cs_5.shield_hp - 5_000.0).abs() < 1e-6, "Secondary hostile at 60 km must not be hit by 25 km PDB AoE");
    assert!(close_cs_5.shield_hp < 5_000.0, "Close hostile within 25 km takes PDB AoE damage");

    // 2. Step 56s more (>= 55.40s SDE lock time) -> lock completes and directed Standup missiles fire & consume ammo!
    let _ = sys.step_manual(56.0).await.expect("step_60s");
    let locks_60 = sys.get_target_locks(struct_id).await.unwrap_or_default();
    assert!(locks_60.iter().any(|l| l.locked && l.target_id == hostile_close_ship));
    let close_cs_60 = sys.get_combat_state(hostile_close_ship).await.unwrap().unwrap();
    assert!(close_cs_60.shield_hp < close_cs_5.shield_hp, "Directed missile launcher fires once lock completes");
    assert!(ctx.db.with_snapshot(|s| s.items[&880_810_621].quantity) < 20, "Missile charges must be consumed");

    // 3. Remove close hostile, leave only 1 missile in launcher, and verify launcher stops firing once empty!
    let _ = sys.remove_ball(hostile_close_ship).await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&880_810_621).unwrap().quantity = 1;
    });
    let _ = sys.step_manual(1.0).await.expect("step_far_lock_start");
    let _ = sys.step_manual(60.0).await.expect("step_far_lock_and_last_shot");
    let far_cs_1 = sys.get_combat_state(hostile_far_ship).await.unwrap().unwrap();
    assert!(far_cs_1.shield_hp < 5_000.0, "Far hostile at 60 km takes 1 missile shot");
    assert!(ctx.db.with_snapshot(|s| !s.items.contains_key(&880_810_621)), "Empty missile stack must be removed");
    let _ = sys.step_manual(6.0).await.expect("step_empty_launcher");
    let far_cs_2 = sys.get_combat_state(hostile_far_ship).await.unwrap().unwrap();
    assert!(far_cs_2.shield_hp >= far_cs_1.shield_hp - 1e-6, "Empty launcher must not deal phantom ammo damage");

    // 4. Reload launcher and test autoTargetAttackers (positive, negative, TTL expiry) and autoTargetWarTargets (positive, negative)!
    let _ = sys.remove_ball(hostile_far_ship).await;
    let attacker_pilot = 90_000_088;
    let war_pilot = 90_000_089;
    let attacker_ship = 880_810_613;
    let war_ship = 880_810_614;
    let war_corp = 98_999_689;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            880_810_624,
            InventoryItemRecord {
                item_id: 880_810_624,
                type_id: 37847,
                owner_id: corp_id,
                location_id: struct_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 50,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        for (cid, ccorp, shid) in [(attacker_pilot, 98_999_688, attacker_ship), (war_pilot, war_corp, war_ship)] {
            let mut ch = snap.characters.get(&OWNER).cloned().unwrap();
            ch.character_id = cid;
            ch.corporation_id = ccorp;
            ch.station_id = None;
            ch.solar_system_id = RANCER;
            ch.active_ship_id = shid;
            snap.characters.insert(cid, ch);
        }
        snap.wars.insert(
            770_810_601,
            eve_db::WarRecord {
                war_id: 770_810_601,
                aggressor_corp_id: war_corp,
                defender_corp_id: corp_id,
                mutual: false,
                started_unix: ctx.now_unix().saturating_sub(90_000),
                active: true,
            },
        );
    });
    spawn_ship(attacker_ship, st_pos + DVec3::new(80_000.0, 0.0, 0.0)).await;
    spawn_ship(war_ship, st_pos + DVec3::new(0.0, 90_000.0, 0.0)).await;

    // attacker_pilot damages the structure, recording an active attacker entry.
    let _ = eve_server::services::station_structure::structure_lifecycle_rpc::apply_live_combat_damage_to_upwell_structure(
        &ctx,
        &sys,
        RANCER,
        attacker_pilot,
        struct_id,
        eve_destiny::DamageVector::new(100.0, 0.0, 0.0, 0.0),
        1.0,
        ctx.now_unix(),
    )
    .await
    .expect("attacker hits structure");

    // 4a. Both flags OFF -> neither attacker nor war target is engaged.
    set_def!(false, false, false);
    let _ = sys.step_manual(5.0).await.expect("step_flags_off");
    assert!(sys.get_target_locks(struct_id).await.unwrap_or_default().is_empty(), "No locks when flags are off");

    // 4b. autoTargetAttackers ON, autoTargetWarTargets OFF -> engages attacker_ship, ignores war_ship!
    set_def!(true, false, false);
    let _ = sys.step_manual(1.0).await.expect("step_attacker_lock_start");
    let _ = sys.step_manual(60.0).await.expect("step_attacker_on");
    assert!(sys.get_combat_state(attacker_ship).await.unwrap().unwrap().shield_hp < 5_000.0, "Attacker must be damaged when autoTargetAttackers=true");
    assert!((sys.get_combat_state(war_ship).await.unwrap().unwrap().shield_hp - 5_000.0).abs() < 1e-6, "War target must not be damaged when autoTargetWarTargets=false");

    // 4c. Advance clock past STRUCTURE_ATTACKER_TTL_SECS (900s) -> attacker expires and is no longer targeted!
    ctx.advance_virtual_clock(eve_server::services::structure_acl_defense_rules::STRUCTURE_ATTACKER_TTL_SECS + 5);
    let _ = sys.remove_target_lock(struct_id, attacker_ship).await;
    let _ = sys.step_manual(5.0).await.expect("step_attacker_expired");
    assert!(sys.get_target_locks(struct_id).await.unwrap_or_default().is_empty(), "Expired attacker must no longer be targeted");

    // 4d. autoTargetWarTargets ON, autoTargetAttackers OFF -> engages war_ship!
    set_def!(false, true, false);
    let _ = sys.step_manual(1.0).await.expect("step_war_lock_start");
    let _ = sys.step_manual(60.0).await.expect("step_war_on");
    assert!(sys.get_combat_state(war_ship).await.unwrap().unwrap().shield_hp < 5_000.0, "War target must be damaged when autoTargetWarTargets=true");
}

}

#[allow(dead_code, unused_imports)]
pub(crate) mod upwell_structures_services {
    use super::*;

//  End-to-end remediation tests for Upwell docking ACLs & hull docking matrix
//  (`feat:structures.docking-acl`), Repair Shop service quotes & restoration
//  (`feat:structures.repairshop-service-quotes-and-restoration`), and Mobile Cynosural Inhibitor
//  lifecycle & activation (`feat:structures.mobile-cyno-inhibitor`).

use eve_db::{InventoryFlag, InventoryItemRecord, UpwellStructureRecord};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::attr_ids;
use eve_server::router::ServerContext;
use eve_server::services::station_structure::structure_lifecycle as life;
use eve_server::services::station_structure::structure_upwell_ops::{load_ext_state, save_ext_state};
use glam::DVec3;
use std::collections::HashMap;
use std::sync::Arc;

const OWNER: i64 = 90_000_001;
const ALLY_PILOT: i64 = 90_000_002;
const BLOCKED_PILOT: i64 = 90_000_003;
const OUTSIDER: i64 = 90_000_004;
const PERIMETER: u32 = 30_000_144;
const RANCER: u32 = 30_002_718;

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None).await
}

fn kv_i64(rep: &PyRep, key: &str) -> i64 {
    rep.dict_get(key).and_then(|v| v.as_i64()).unwrap_or(0)
}

fn kv_f64(rep: &PyRep, key: &str) -> f64 {
    rep.dict_get(key).and_then(|v| v.as_f64()).unwrap_or(0.0)
}

fn make_structure(
    structure_id: i64,
    type_id: u32,
    owner_corp_id: i64,
    solar_system_id: u32,
    name: &str,
    pos: DVec3,
    fuel_blocks: u32,
) -> UpwellStructureRecord {
    UpwellStructureRecord {
        structure_id,
        type_id,
        owner_corp_id,
        solar_system_id,
        name: name.into(),
        state: life::STATE_SHIELD_VULNERABLE.into(),
        position: pos.to_array(),
        quantum_core_installed: true,
        fuel_blocks,
        shield_hp: 1_000_000.0,
        armor_hp: 1_000_000.0,
        hull_hp: 1_000_000.0,
        reinforced_until_unix: 0,
        last_damage_tick: 0,
        last_damage_unix: 0,
        self_destruct_at_unix: None,
    }
}

fn grant(
    ctx: &ServerContext,
    owner: i64,
    location: i64,
    type_id: u32,
    flag: InventoryFlag,
    qty: i32,
) -> i64 {
    ctx.db.with_mut_snapshot(|snap| {
        snap.next_entity_id += 1;
        let id = snap.next_entity_id;
        snap.items.insert(
            id,
            InventoryItemRecord {
                item_id: id,
                type_id,
                owner_id: owner,
                location_id: location,
                flag,
                quantity: qty,
                singleton: qty == 1,
                version: 1,
                ..Default::default()
            },
        );
        id
    })
}

fn new_ctx() -> Arc<ServerContext> {
    let ctx = Arc::new(ServerContext::new());
    ctx.set_clock(Arc::new(eve_db::VirtualServerClock::with_fixed_base(1_800_000_000)));
    ctx
}

async fn setup_owner_corp(ctx: &ServerContext, sess_id: u64, sys_id: u32, name: &str, ticker: &str) -> SessionState {
    let station_id = ctx
        .sde
        .stations
        .values()
        .find(|s| s.solar_system_id == sys_id)
        .map(|s| s.station_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&OWNER) {
            let ship_id = ch.active_ship_id;
            ch.solar_system_id = sys_id;
            ch.station_id = station_id;
            if let Some(sh) = snap.items.get_mut(&ship_id) {
                sh.location_id = station_id.unwrap_or(i64::from(sys_id));
            }
        }
    });
    let mut sess = SessionState::new(sess_id);
    rpc(ctx, &mut sess, "authentication", "Login", &[PyRep::String("demo".into()), PyRep::String("demo-secret".into())])
        .await
        .expect("Login");
    rpc(ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(OWNER)])
        .await
        .expect("SelectCharacterID");
    ctx.advance_secs(12);
    rpc(
        ctx,
        &mut sess,
        "corpRegistry",
        "AddCorporation",
        &[PyRep::String(name.into()), PyRep::String(ticker.into())],
    )
    .await
    .expect("AddCorporation");
    ctx.advance_virtual_clock(7 * 86_400 + 12);
    if station_id.is_some() {
        rpc(ctx, &mut sess, "structureDocking", "Undock", &[]).await.expect("Undock");
    }
    sess
}

async fn spawn_pilot_ship(
    ctx: &ServerContext,
    sess: &mut SessionState,
    char_id: i64,
    corp_id: i64,
    sys_id: u32,
    ship_id: i64,
    ship_type_id: u32,
    pos: DVec3,
) {
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(corp_id);
    sess.solar_system_id = Some(sys_id);
    sess.station_id = None;
    sess.structure_id = None;
    sess.ship_id = Some(ship_id);
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.get(&OWNER).cloned().unwrap();
        ch.character_id = char_id;
        ch.corporation_id = corp_id;
        ch.solar_system_id = sys_id;
        ch.station_id = None;
        ch.structure_id = None;
        ch.active_ship_id = ship_id;
        snap.characters.insert(char_id, ch);
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: ship_type_id,
                owner_id: char_id,
                location_id: i64::from(sys_id),
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    ctx.advance_virtual_clock(15);
    let sys = ctx.systems.get_or_spawn(sys_id);
    let _ = sys
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: ship_type_id,
                position: pos,
                velocity: DVec3::ZERO,
                radius: 40.0,
                mass: 1.0e6,
                mode: MovementMode::Stop,
                speed_fraction: 0.0,
                max_velocity: 300.0,
                inertia: 1.0,
            },
            ctx.build_type_combat_state(ship_type_id),
        )
        .await;
    let _ = sys.set_ball_position(ship_id, pos).await;
}

#[tokio::test]
async fn test_remediated_docking_acl_and_upwell_hull_matrix() {
    // parity: feat:structures.docking-acl
    eve_testkit::attest(&["feat:structures.docking-acl"]);

    let ctx = new_ctx();
    let mut sess = setup_owner_corp(&ctx, 8201, PERIMETER, "Docking Matrix Corp", "DOCK").await;
    let corp_id = sess.corporation_id.expect("corp");
    let st_pos = DVec3::new(0.0, 5.0e9, 0.0);

    // 1. Live ACL enforcement on stationSvc.Dock & structureDocking.Dock via ownerGroupManager + structureSettings.
    ctx.db.with_mut_snapshot(|snap| {
        for (cid, corp) in [(ALLY_PILOT, 98_820_002), (BLOCKED_PILOT, 98_820_003), (OUTSIDER, 98_820_004)] {
            let mut ch = snap.characters.get(&OWNER).cloned().unwrap();
            ch.character_id = cid;
            ch.corporation_id = corp;
            snap.characters.insert(cid, ch);
        }
    });
    let group_id = kv_i64(
        &rpc(&ctx, &mut sess, "ownerGroupManager", "Create", &[PyRep::String("Docking ACL".into()), PyRep::String("Allowed".into())])
            .await
            .expect("Create ACL group"),
        "groupID",
    );
    rpc(&ctx, &mut sess, "ownerGroupManager", "AddMembers", &[PyRep::Integer(group_id), PyRep::List(vec![PyRep::Integer(ALLY_PILOT), PyRep::Integer(BLOCKED_PILOT)])])
        .await
        .expect("AddMembers");
    rpc(&ctx, &mut sess, "ownerGroupManager", "UpdateMembershipTypes", &[PyRep::Integer(group_id), PyRep::List(vec![PyRep::Integer(BLOCKED_PILOT)]), PyRep::Integer(-1)])
        .await
        .expect("Block pilot in ACL group");

    let prof_id = rpc(&ctx, &mut sess, "structureProfiles", "CreateProfile", &[PyRep::String("ACL Profile".into()), PyRep::String("Docking ACL".into())])
        .await
        .expect("CreateProfile")
        .as_i64()
        .unwrap();
    rpc(
        &ctx,
        &mut sess,
        "structureProfiles",
        "SaveProfileSettings",
        &[
            PyRep::Integer(prof_id),
            PyRep::List(vec![
                PyRep::Tuple(vec![PyRep::Integer(19), PyRep::Integer(0), PyRep::Integer(1)]),
                PyRep::Tuple(vec![PyRep::Integer(19), PyRep::Integer(group_id), PyRep::Integer(1)]),
                PyRep::Tuple(vec![PyRep::Integer(19), PyRep::Integer(BLOCKED_PILOT), PyRep::Integer(-1)]),
            ]),
        ],
    )
    .await
    .expect("SaveProfileSettings");

    let astra_id = 880_820_101;
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            astra_id,
            make_structure(astra_id, 35832, corp_id, PERIMETER, "ACL Astrahus", st_pos, 1_000),
        );
    });
    life::sync_all_structures(&ctx);
    rpc(&ctx, &mut sess, "structureProfiles", "ChangeProfiles", &[PyRep::List(vec![PyRep::Integer(astra_id)]), PyRep::Integer(prof_id)])
        .await
        .expect("ChangeProfiles");

    let mut ally_sess = SessionState::new(8202);
    ally_sess.user_id = Some(2);
    spawn_pilot_ship(&ctx, &mut ally_sess, ALLY_PILOT, 98_820_002, PERIMETER, 880_820_111, 587, st_pos + DVec3::new(1_000.0, 0.0, 0.0)).await;
    rpc(&ctx, &mut ally_sess, "stationSvc", "Dock", &[PyRep::Integer(astra_id)])
        .await
        .expect("Allowed ACL pilot docks via stationSvc.Dock");
    assert_eq!(ally_sess.structure_id, Some(astra_id));

    let mut blocked_sess = SessionState::new(8203);
    blocked_sess.user_id = Some(3);
    spawn_pilot_ship(&ctx, &mut blocked_sess, BLOCKED_PILOT, 98_820_003, PERIMETER, 880_820_112, 587, st_pos + DVec3::new(1_000.0, 0.0, 0.0)).await;
    assert_eq!(
        rpc(&ctx, &mut blocked_sess, "structureDocking", "Dock", &[PyRep::Integer(astra_id)])
            .await
            .expect_err("Blocked ACL pilot denied")
            .code,
        "StructureDockingAccessDenied"
    );

    let mut outsider_sess = SessionState::new(8204);
    outsider_sess.user_id = Some(4);
    spawn_pilot_ship(&ctx, &mut outsider_sess, OUTSIDER, 98_820_004, PERIMETER, 880_820_113, 587, st_pos + DVec3::new(1_000.0, 0.0, 0.0)).await;
    assert_eq!(
        rpc(&ctx, &mut outsider_sess, "structureDocking", "Dock", &[PyRep::Integer(astra_id)])
            .await
            .expect_err("Unaffiliated pilot denied")
            .code,
        "StructureDockingAccessDenied"
    );

    // 2. Full V24 Upwell Hull Docking Matrix across non-dockable, Medium, Tatara, Large, and XLarge structures.
    let hulls: &[(i64, u32, &[u32], &[u32])] = &[
        (880_820_201, 35840, &[], &[587]),
        (880_820_202, 35841, &[], &[587]),
        (880_820_203, 37534, &[], &[587]),
        (880_820_204, 81826, &[], &[587]),
        (880_820_205, 35832, &[587], &[28352, 19720]),
        (880_820_206, 35825, &[587], &[28352, 19720]),
        (880_820_207, 35835, &[587], &[28352, 19720]),
        (880_820_208, 35826, &[587], &[28352, 19720]),
        (880_820_209, 35836, &[587, 28352], &[19720, 23919]),
        (880_820_210, 35833, &[587, 28352, 19720], &[23919, 671]),
        (880_820_211, 47512, &[587, 28352, 19720], &[23919, 671]),
        (880_820_212, 35827, &[587, 28352, 19720], &[23919, 671]),
        (880_820_213, 35834, &[587, 28352, 19720, 23919, 671], &[]),
        (880_820_214, 40340, &[587, 28352, 19720, 23919, 671], &[]),
    ];

    for (idx, &(sid, stype, allowed_ships, denied_ships)) in hulls.iter().enumerate() {
        let pos = st_pos + DVec3::new((idx as f64 + 1.0) * 5_000_000.0, 0.0, 0.0);
        ctx.db.with_mut_snapshot(|snap| {
            snap.structures.insert(
                sid,
                make_structure(sid, stype, corp_id, PERIMETER, &format!("Matrix Structure {stype}"), pos, 1_000),
            );
        });
        life::sync_all_structures(&ctx);

        for &ship_tid in denied_ships {
            let test_ship = 880_820_500 + (idx as i64) * 20 + i64::from(ship_tid % 17);
            spawn_pilot_ship(&ctx, &mut sess, OWNER, corp_id, PERIMETER, test_ship, ship_tid, pos + DVec3::new(1_000.0, 0.0, 0.0)).await;
            let err = rpc(&ctx, &mut sess, "structureDocking", "Dock", &[PyRep::Integer(sid)])
                .await
                .expect_err("denied hull docking");
            let expected_code = if allowed_ships.is_empty() {
                "StructureDoesNotAllowDocking"
            } else {
                "ShipTooLargeForStructure"
            };
            assert_eq!(err.code, expected_code, "structure {stype} vs ship {ship_tid}");
        }

        for &ship_tid in allowed_ships {
            let test_ship = 880_820_800 + (idx as i64) * 20 + i64::from(ship_tid % 19);
            spawn_pilot_ship(&ctx, &mut sess, OWNER, corp_id, PERIMETER, test_ship, ship_tid, pos + DVec3::new(1_000.0, 0.0, 0.0)).await;
            rpc(&ctx, &mut sess, "structureDocking", "Dock", &[PyRep::Integer(sid)])
                .await
                .unwrap_or_else(|e| panic!("structure {stype} must allow ship {ship_tid}: {e:?}"));
            assert_eq!(sess.structure_id, Some(sid));
        }
    }
}

#[tokio::test]
async fn test_remediated_repairshop_service_quotes_and_restoration() {
    // parity: feat:structures.repairshop-service-quotes-and-restoration
    eve_testkit::attest(&["feat:structures.repairshop-service-quotes-and-restoration"]);

    let ctx = new_ctx();
    let mut sess = setup_owner_corp(&ctx, 8210, PERIMETER, "Repair Grid Corp", "REPR").await;
    let corp_id = sess.corporation_id.expect("corp");
    let st_id = 880_821_001;
    let st_pos = DVec3::new(0.0, 5.0e9, 0.0);
    let ship_id = 880_821_010;
    let mod_id = 880_821_011;
    let drone_id = 880_821_012;

    // Seed an unfueled (LowPower) Astrahus WITH Quantum Core installed, plus 5% Repair Shop tax (500 bps).
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            st_id,
            make_structure(st_id, 35832, corp_id, PERIMETER, "LowPower Repair Astrahus", st_pos, 0),
        );
        let mut ext = load_ext_state(snap);
        ext.access_profiles.entry(st_id).or_default().service_settings.insert(13, 500);
        save_ext_state(snap, &ext);

        snap.characters.get_mut(&OWNER).unwrap().balance = eve_db::IskCents::from_f64_isk(10_000_000.0);
        snap.corporations.get_mut(&corp_id).unwrap().set_division_balance(1, eve_db::IskCents::from_f64_isk(500_000.0));
        let ch = snap.characters.get_mut(&OWNER).unwrap();
        ch.station_id = None;
        ch.structure_id = Some(st_id);
        ch.solar_system_id = PERIMETER;
        ch.active_ship_id = ship_id;

        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587, // Rifter (category 6)
                owner_id: OWNER,
                location_id: st_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::from([
                    (attr_ids::DAMAGE, 40.0),
                    (attr_ids::ARMOR_DAMAGE, 60.0),
                ]),
                ..Default::default()
            },
        );
        snap.items.insert(
            mod_id,
            InventoryItemRecord {
                item_id: mod_id,
                type_id: 3170, // Small Armor Repairer I (category 7)
                owner_id: OWNER,
                location_id: ship_id,
                flag: InventoryFlag::LoSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::from([(attr_ids::DAMAGE, 20.0)]),
                ..Default::default()
            },
        );
        snap.items.insert(
            drone_id,
            InventoryItemRecord {
                item_id: drone_id,
                type_id: 2454, // Hobgoblin I (category 18)
                owner_id: OWNER,
                location_id: ship_id,
                flag: InventoryFlag::DroneBay,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::from([
                    (attr_ids::DAMAGE, 10.0),
                    (attr_ids::ARMOR_DAMAGE, 15.0),
                ]),
                ..Default::default()
            },
        );
    });
    sess.station_id = None;
    sess.structure_id = Some(st_id);
    sess.solar_system_id = Some(PERIMETER);
    sess.ship_id = Some(ship_id);

    // 1. Verify LowPower structure WITH Quantum Core provides Repair Shop quotes using exact evemu_Crucible modifiers!
    let quotes = rpc(&ctx, &mut sess, "repairSvc", "GetRepairQuotes", &[PyRep::List(vec![PyRep::Integer(ship_id)])])
        .await
        .expect("LowPower structure with Quantum Core must offer Repair Shop quotes");
    let PyRep::Dict(q_dict) = quotes else { panic!("expected dict") };
    let (_, PyRep::List(rows)) = &q_dict[0] else { panic!("expected quote list") };
    assert_eq!(rows.len(), 3, "Ship + fitted module + drone in bay must all be quoted");

    let ship_base = ctx.sde.types.get(&587).map(|t| t.base_price).filter(|p| *p > 0.0).unwrap_or(10_000.0);
    let mod_base = ctx.sde.types.get(&3170).map(|t| t.base_price).filter(|p| *p > 0.0).unwrap_or(10_000.0);
    let ship_unit_cents = (eve_db::IskCents::from_f64_isk((ship_base * 7.5e-6).max(0.01)).0 * 500) / 10_000;
    let mod_unit_cents = (eve_db::IskCents::from_f64_isk((mod_base * 1.25e-3).max(0.01)).0 * 500) / 10_000;
    let expected_ship_tax = (ship_unit_cents * 100) as f64 / 100.0;
    let expected_mod_tax = (mod_unit_cents * 20) as f64 / 100.0;

    let ship_row = rows.iter().find(|r| kv_i64(r, "itemID") == ship_id).expect("ship quote");
    let mod_row = rows.iter().find(|r| kv_i64(r, "itemID") == mod_id).expect("mod quote");
    assert_eq!(kv_f64(ship_row, "baseCost"), 0.0);
    assert!((kv_f64(ship_row, "totalRepairCost") - expected_ship_tax).abs() < 1e-2);
    assert!((kv_f64(mod_row, "totalRepairCost") - expected_mod_tax).abs() < 1e-2);

    // 2. Verify NoCore and Abandoned structures reject Repair Shop, then restore all HP in FullPower/LowPower.
    ctx.db.with_mut_snapshot(|snap| snap.structures.get_mut(&st_id).unwrap().quantum_core_installed = false);
    assert_eq!(
        rpc(&ctx, &mut sess, "repairSvc", "GetRepairQuotes", &[PyRep::List(vec![PyRep::Integer(ship_id)])])
            .await
            .expect_err("NoCore quote rejected")
            .code,
        "QuantumCoreRequired"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.get_mut(&st_id).unwrap().quantum_core_installed = true;
        let mut ext = load_ext_state(snap);
        ext.lifecycle.entry(st_id).or_default().low_power_since_unix = Some(1);
        save_ext_state(snap, &ext);
    });
    assert_eq!(
        rpc(&ctx, &mut sess, "repairSvc", "RepairItemsInStructure", &[PyRep::List(vec![PyRep::Integer(ship_id)])])
            .await
            .expect_err("Abandoned repair rejected")
            .code,
        "StructureAbandoned"
    );
    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        ext.lifecycle.entry(st_id).or_default().low_power_since_unix = None;
        save_ext_state(snap, &ext);
    });

    let total_expected_isk: f64 = rows.iter().map(|r| kv_f64(r, "totalRepairCost")).sum();
    let rep_res = rpc(&ctx, &mut sess, "repairSvc", "RepairItemsInStructure", &[PyRep::List(vec![PyRep::Integer(ship_id)])])
        .await
        .expect("RepairItemsInStructure succeeds in LowPower with Quantum Core");
    let paid = kv_f64(&rep_res, "totalCostISK");
    assert!((paid - total_expected_isk).abs() < 1e-2);

    ctx.db.with_snapshot(|snap| {
        for iid in [ship_id, mod_id, drone_id] {
            let it = snap.items.get(&iid).unwrap();
            assert!(!it.dynamic_attributes.contains_key(&attr_ids::DAMAGE));
            assert!(!it.dynamic_attributes.contains_key(&attr_ids::ARMOR_DAMAGE));
        }
        assert!((snap.characters.get(&OWNER).unwrap().balance.to_wire_f64() - (10_000_000.0 - paid)).abs() < 1e-2);
        assert!((snap.corporations.get(&corp_id).unwrap().division_balance(1).to_wire_f64() - (500_000.0 + paid)).abs() < 1e-2);
    });
}

#[tokio::test]
async fn test_remediated_mobile_cyno_inhibitor_lifecycle_and_activation() {
    // parity: feat:structures.mobile-cyno-inhibitor
    eve_testkit::attest(&["feat:structures.mobile-cyno-inhibitor"]);

    let ctx = new_ctx();
    let mut sess = setup_owner_corp(&ctx, 8220, PERIMETER, "Cyno Inhibitor Corp", "CINH").await;
    let corp_id = sess.corporation_id.expect("corp");
    let base_pos = DVec3::new(0.0, 5.0e9, 0.0);
    let falcon_id = 880_822_010;
    let venture_id = 880_822_011;

    spawn_pilot_ship(&ctx, &mut sess, OWNER, corp_id, RANCER, falcon_id, 11957, base_pos).await;
    let inhib_1 = grant(&ctx, OWNER, falcon_id, 33476, InventoryFlag::Cargo, 1);
    let inhib_2 = grant(&ctx, OWNER, falcon_id, 33476, InventoryFlag::Cargo, 1);
    let std_cyno = grant(&ctx, OWNER, falcon_id, 21096, InventoryFlag::HiSlot0, 1);
    let cov_cyno = grant(&ctx, OWNER, falcon_id, 28646, InventoryFlag::HiSlot1, 1);
    let _ozone = grant(&ctx, OWNER, falcon_id, 16273, InventoryFlag::Cargo, 5_000);
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(std_cyno, true);
        snap.module_online_states.insert(cov_cyno, true);
        let sk = snap.skills.entry(OWNER).or_default();
        sk.insert(11584, eve_db::TrainedSkillRecord { type_id: 11584, level: 5, skill_points: 256_000.0 });
        sk.insert(21603, eve_db::TrainedSkillRecord { type_id: 21603, level: 5, skill_points: 256_000.0 });
        sk.insert(21611, eve_db::TrainedSkillRecord { type_id: 21611, level: 5, skill_points: 256_000.0 });
        snap.fleets.insert(5001, eve_db::FleetRecord { fleet_id: 5001, boss_id: OWNER, members: vec![OWNER], ..Default::default() });
    });
    ctx.invalidate_ship_fitting(falcon_id);

    // 1. Deploy Mobile Cynosural Inhibitor via posMgr.AnchorStructure -> 120s delay, 3,600s lifetime, non-scoopable, 200 km overlap ban.
    let t0 = ctx.now_unix();
    let anc1 = rpc(&ctx, &mut sess, "posMgr", "AnchorStructure", &[PyRep::Integer(inhib_1)])
        .await
        .expect("Anchor Mobile Cynosural Inhibitor");
    assert_eq!(kv_f64(&anc1, "anchoringDelaySeconds"), 120.0);
    assert_eq!(kv_i64(&anc1, "despawnUnix"), (t0 + 3_600) as i64);
    assert!(
        !eve_server::services::mobile_deployables::can_deployable_be_scooped(1249),
        "Mobile Cynosural Inhibitor (group 1249) must not be scoopable"
    );

    // Second Mobile Cyno Inhibitor within 200 km is rejected with MobileCynoInhibitorOverlap.
    assert_eq!(
        rpc(&ctx, &mut sess, "posMgr", "AnchorStructure", &[PyRep::Integer(inhib_2)])
            .await
            .expect_err("200 km overlap rejected")
            .code,
        "MobileCynoInhibitorOverlap"
    );

    let inhib_pos = ctx.db.with_snapshot(|s| DVec3::from_array(s.item_positions[&inhib_1]));
    let sys = ctx.systems.get_or_spawn(RANCER);
    let _ = sys.set_ball_position(falcon_id, inhib_pos + DVec3::new(50_000.0, 0.0, 0.0)).await;

    // 2. During initial 120s spool-up (t + 60s), inhibitor is not yet online -> standard cyno succeeds.
    ctx.advance_virtual_clock(60);
    rpc(&ctx, &mut sess, "dogmaIM", "Activate", &[PyRep::Integer(std_cyno), PyRep::String("cynosuralGeneration".into())])
        .await
        .expect("Standard cyno succeeds during 120s activation delay");

    // 3. After 120s (t + 125s): Standard & Industrial cynos blocked within 100 km, Covert cyno allowed, > 100 km allowed!
    ctx.advance_virtual_clock(65);
    assert_eq!(
        rpc(&ctx, &mut sess, "dogmaIM", "Activate", &[PyRep::Integer(std_cyno), PyRep::String("cynosuralGeneration".into())])
            .await
            .expect_err("Standard cyno blocked within 100 km")
            .code,
        "CynoInhibited"
    );

    // Covert Cynosural Field Generator I (28646) is unaffected by Mobile Cynosural Inhibitor!
    let covops_id = falcon_id + 10;
    spawn_pilot_ship(&ctx, &mut sess, OWNER, corp_id, RANCER, covops_id, 11_188, inhib_pos + DVec3::new(50_000.0, 0.0, 0.0)).await;
    let cov_cyno_2 = grant(&ctx, OWNER, covops_id, 28646, InventoryFlag::HiSlot0, 1);
    let _ozone_cov = grant(&ctx, OWNER, covops_id, 16273, InventoryFlag::Cargo, 500);
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(cov_cyno_2, true);
    });
    ctx.invalidate_ship_fitting(covops_id);
    rpc(&ctx, &mut sess, "dogmaIM", "Activate", &[PyRep::Integer(cov_cyno_2), PyRep::String("cynosuralGeneration".into())])
        .await
        .expect("Covert cyno succeeds inside 100 km of Mobile Cyno Inhibitor");

    // Industrial Cynosural Field Generator (52694) on a Venture (32880) at 50 km is also blocked!
    spawn_pilot_ship(&ctx, &mut sess, OWNER, corp_id, RANCER, venture_id, 32880, inhib_pos + DVec3::new(50_000.0, 0.0, 0.0)).await;
    let indy_cyno = grant(&ctx, OWNER, venture_id, 52694, InventoryFlag::HiSlot0, 1);
    let _ozone_v = grant(&ctx, OWNER, venture_id, 16273, InventoryFlag::Cargo, 5_000);
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(indy_cyno, true);
    });
    ctx.invalidate_ship_fitting(venture_id);
    assert_eq!(
        rpc(&ctx, &mut sess, "dogmaIM", "Activate", &[PyRep::Integer(indy_cyno), PyRep::String("cynosuralGeneration".into())])
            .await
            .expect_err("Industrial cyno blocked within 100 km")
            .code,
        "CynoInhibited"
    );

    // Move Falcon to 150 km (> 100 km) after prior cyno expires -> Standard cyno succeeds!
    ctx.advance_virtual_clock(600);
    spawn_pilot_ship(&ctx, &mut sess, OWNER, corp_id, RANCER, falcon_id, 11957, inhib_pos + DVec3::new(150_000.0, 0.0, 0.0)).await;
    rpc(&ctx, &mut sess, "dogmaIM", "Activate", &[PyRep::Integer(std_cyno), PyRep::String("cynosuralGeneration".into())])
        .await
        .expect("Standard cyno succeeds beyond 100 km");

    // 4. After 1-hour (3,600s) lifetime, inhibitor expires and standard cyno at 50 km succeeds again!
    ctx.advance_virtual_clock(3_500);
    let _ = sys.set_ball_position(falcon_id, inhib_pos + DVec3::new(50_000.0, 0.0, 0.0)).await;
    rpc(&ctx, &mut sess, "dogmaIM", "Activate", &[PyRep::Integer(std_cyno), PyRep::String("cynosuralGeneration".into())])
        .await
        .expect("Standard cyno succeeds after 1-hour inhibitor expiry");
}

}

#[allow(dead_code, unused_imports)]
pub(crate) mod upwell_structures_reinforce {
    use super::*;

//  End-to-end remediation tests for Structure Directory visibility & ACL filtering
//  (`feat:structures.structure-directory`) and Structure Profile lifecycle / 5-minute transition delay
//  (`feat:structures.profiles`).

use eve_db::{InventoryFlag, InventoryItemRecord, UpwellStructureRecord};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use eve_server::services::station_structure::structure_lifecycle as life;
use glam::DVec3;
use std::collections::HashMap;
use std::sync::Arc;

const OWNER: i64 = 90_000_001;
const ALLY_PILOT: i64 = 90_000_002;
const PERIMETER: u32 = 30_000_144;
const RANCER: u32 = 30_002_718;

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None).await
}

fn kv_i64(rep: &PyRep, key: &str) -> i64 {
    rep.dict_get(key).and_then(|v| v.as_i64()).unwrap_or(0)
}

fn kv_str(rep: &PyRep, key: &str) -> String {
    rep.dict_get(key).and_then(|v| v.as_str()).unwrap_or("").to_string()
}

fn new_ctx() -> Arc<ServerContext> {
    let ctx = Arc::new(ServerContext::new());
    ctx.set_clock(Arc::new(eve_db::VirtualServerClock::with_fixed_base(1_800_000_000)));
    ctx
}

async fn setup_owner_corp(ctx: &ServerContext, sess_id: u64, sys_id: u32, name: &str, ticker: &str) -> SessionState {
    let station_id = ctx
        .sde
        .stations
        .values()
        .find(|s| s.solar_system_id == sys_id)
        .map(|s| s.station_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&OWNER) {
            let ship_id = ch.active_ship_id;
            ch.solar_system_id = sys_id;
            ch.station_id = station_id;
            if let Some(sh) = snap.items.get_mut(&ship_id) {
                sh.location_id = station_id.unwrap_or(i64::from(sys_id));
            }
        }
    });
    let mut sess = SessionState::new(sess_id);
    rpc(ctx, &mut sess, "authentication", "Login", &[PyRep::String("demo".into()), PyRep::String("demo-secret".into())])
        .await
        .expect("Login");
    rpc(ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(OWNER)])
        .await
        .expect("SelectCharacterID");
    ctx.advance_secs(12);
    rpc(
        ctx,
        &mut sess,
        "corpRegistry",
        "AddCorporation",
        &[PyRep::String(name.into()), PyRep::String(ticker.into())],
    )
    .await
    .expect("AddCorporation");
    ctx.advance_virtual_clock(7 * 86_400 + 12);
    if station_id.is_some() {
        rpc(ctx, &mut sess, "structureDocking", "Undock", &[]).await.expect("Undock");
    }
    sess
}

async fn spawn_pilot_ship(
    ctx: &ServerContext,
    sess: &mut SessionState,
    char_id: i64,
    corp_id: i64,
    sys_id: u32,
    ship_id: i64,
    ship_type_id: u32,
    pos: DVec3,
) {
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(corp_id);
    sess.solar_system_id = Some(sys_id);
    sess.station_id = None;
    sess.structure_id = None;
    sess.ship_id = Some(ship_id);
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.get(&OWNER).cloned().unwrap();
        ch.character_id = char_id;
        ch.corporation_id = corp_id;
        ch.solar_system_id = sys_id;
        ch.station_id = None;
        ch.structure_id = None;
        ch.active_ship_id = ship_id;
        snap.characters.insert(char_id, ch);
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: ship_type_id,
                owner_id: char_id,
                location_id: i64::from(sys_id),
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    let sys = ctx.systems.get_or_spawn(sys_id);
    let _ = sys
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: ship_type_id,
                position: pos,
                velocity: DVec3::ZERO,
                radius: 40.0,
                mass: 1.0e6,
                mode: MovementMode::Stop,
                speed_fraction: 0.0,
                max_velocity: 300.0,
                inertia: 1.0,
            },
            ctx.build_type_combat_state(ship_type_id),
        )
        .await;
    let _ = sys.set_ball_position(ship_id, pos).await;
}

#[tokio::test]
async fn test_remediated_structure_directory_and_profiles_visibility() {
    // parity: feat:structures.structure-directory, feat:structures.profiles
    eve_testkit::attest(&[
        "feat:structures.structure-directory",
        "feat:structures.profiles",
    ]);

    let ctx = new_ctx();
    let mut sess = setup_owner_corp(&ctx, 8230, PERIMETER, "Directory Corp", "DIRC").await;
    let corp_id = sess.corporation_id.expect("corp");
    let st_id = 880_823_001;
    let st_pos = DVec3::new(0.0, 5.0e9, 0.0);

    let group_a = kv_i64(
        &rpc(&ctx, &mut sess, "ownerGroupManager", "Create", &[PyRep::String("Group A".into()), PyRep::String("A".into())])
            .await
            .unwrap(),
        "groupID",
    );
    rpc(&ctx, &mut sess, "ownerGroupManager", "AddMembers", &[PyRep::Integer(group_a), PyRep::List(vec![PyRep::Integer(ALLY_PILOT)])])
        .await
        .unwrap();

    let prof_a = rpc(&ctx, &mut sess, "structureProfiles", "CreateProfile", &[PyRep::String("Profile A".into()), PyRep::String("Initial".into())])
        .await
        .unwrap()
        .as_i64()
        .unwrap();
    let prof_b = rpc(&ctx, &mut sess, "structureProfiles", "CreateProfile", &[PyRep::String("Profile B".into()), PyRep::String("Delayed".into())])
        .await
        .unwrap()
        .as_i64()
        .unwrap();
    rpc(&ctx, &mut sess, "structureProfiles", "UpdateProfile", &[PyRep::Integer(prof_a), PyRep::String("Profile A Renamed".into()), PyRep::String("Updated desc".into())])
        .await
        .unwrap();
    rpc(&ctx, &mut sess, "structureProfiles", "SetDefaultProfile", &[PyRep::Integer(prof_a)])
        .await
        .unwrap();
    let dup_id = rpc(&ctx, &mut sess, "structureProfiles", "DuplicateProfile", &[PyRep::Integer(prof_a)])
        .await
        .unwrap()
        .as_i64()
        .unwrap();
    let profiles_list = rpc(&ctx, &mut sess, "structureProfiles", "GetProfiles", &[]).await.unwrap();
    let PyRep::List(pl) = &profiles_list else { panic!("expected list from GetProfiles") };
    assert_eq!(pl.len(), 3);
    let pa_entry = pl.iter().find(|p| kv_i64(p, "profileID") == prof_a).expect("prof_a in GetProfiles");
    assert_eq!(kv_str(pa_entry, "name"), "Profile A Renamed");
    assert_eq!(pa_entry.dict_get("isDefault"), Some(&PyRep::Bool(true)));
    rpc(&ctx, &mut sess, "structureProfiles", "DeleteProfile", &[PyRep::Integer(dup_id)])
        .await
        .expect("DeleteProfile on unused duplicate");

    rpc(
        &ctx,
        &mut sess,
        "structureProfiles",
        "SaveProfileSettings",
        &[
            PyRep::Integer(prof_a),
            PyRep::List(vec![
                PyRep::Tuple(vec![PyRep::Integer(19), PyRep::Integer(0), PyRep::Integer(1)]),
                PyRep::Tuple(vec![PyRep::Integer(19), PyRep::Integer(group_a), PyRep::Integer(1)]),
                PyRep::Tuple(vec![PyRep::Integer(4), PyRep::Real(0.015)]),
            ]),
        ],
    )
    .await
    .unwrap();
    let settings_rep = rpc(&ctx, &mut sess, "structureProfiles", "GetProfileSettings", &[PyRep::Integer(prof_a)])
        .await
        .expect("GetProfileSettings");
    let PyRep::Dict(sdict) = settings_rep else { panic!("expected dict from GetProfileSettings") };
    assert!(sdict.iter().any(|(k, _)| k.as_i64() == Some(19)), "GetProfileSettings must include docking setting 19");
    assert!(sdict.iter().any(|(k, _)| k.as_i64() == Some(4)), "GetProfileSettings must include market tax setting 4");

    // Partial SaveProfileSettings with V24 settingID 17 and raw character ID >= 90,000,001 preserves omitted settings.
    rpc(
        &ctx,
        &mut sess,
        "structureProfiles",
        "SaveProfileSettings",
        &[
            PyRep::Integer(prof_a),
            PyRep::Dict(vec![(PyRep::Integer(17), PyRep::List(vec![PyRep::Integer(ALLY_PILOT)]))]),
        ],
    )
    .await
    .unwrap();
    let PyRep::Dict(sdict17) = rpc(&ctx, &mut sess, "structureProfiles", "GetProfileSettings", &[PyRep::Integer(prof_a)])
        .await
        .unwrap()
    else {
        panic!("expected dict");
    };
    let PyRep::List(s17_rows) = sdict17.iter().find(|(k, _)| k.as_i64() == Some(17)).map(|(_, v)| v).unwrap() else {
        panic!("expected list for setting 17");
    };
    assert!(
        s17_rows.iter().any(|r| r.dict_get("groupID").and_then(|v| v.as_i64()) == Some(ALLY_PILOT)),
        "Setting 17 must store raw character ID >= 90,000,001"
    );
    assert!(sdict17.iter().any(|(k, _)| k.as_i64() == Some(19)), "Partial update must preserve setting 19");
    assert!(sdict17.iter().any(|(k, _)| k.as_i64() == Some(4)), "Partial update must preserve setting 4");
    rpc(
        &ctx,
        &mut sess,
        "structureProfiles",
        "SaveProfileSettings",
        &[PyRep::Integer(prof_a), PyRep::Dict(vec![(PyRep::Integer(17), PyRep::List(Vec::new()))])],
    )
    .await
    .unwrap();

    rpc(
        &ctx,
        &mut sess,
        "structureProfiles",
        "SaveProfileSettings",
        &[
            PyRep::Integer(prof_b),
            PyRep::List(vec![
                PyRep::Tuple(vec![PyRep::Integer(19), PyRep::Integer(0), PyRep::Integer(0)]),
                PyRep::Tuple(vec![PyRep::Integer(4), PyRep::Real(0.04)]),
            ]),
        ],
    )
    .await
    .unwrap();

    let jb_a_id = 880_823_002;
    let jb_b_id = 880_823_003;
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            st_id,
            UpwellStructureRecord {
                structure_id: st_id,
                type_id: 35832,
                owner_corp_id: corp_id,
                solar_system_id: PERIMETER,
                name: "Directory Astrahus".into(),
                state: life::STATE_SHIELD_VULNERABLE.into(),
                position: st_pos.to_array(),
                quantum_core_installed: true,
                fuel_blocks: 1_200,
                shield_hp: 1_000_000.0,
                armor_hp: 1_000_000.0,
                hull_hp: 1_000_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
        for (jbid, sys) in [(jb_a_id, PERIMETER), (jb_b_id, RANCER)] {
            snap.structures.insert(
                jbid,
                UpwellStructureRecord {
                    structure_id: jbid,
                    type_id: 35841,
                    owner_corp_id: corp_id,
                    solar_system_id: sys,
                    name: format!("JB {jbid}"),
                    state: life::STATE_SHIELD_VULNERABLE.into(),
                    position: (st_pos + DVec3::new(200_000_000.0, 0.0, 0.0)).to_array(),
                    quantum_core_installed: false,
                    fuel_blocks: 1_200,
                    shield_hp: 1_000_000.0,
                    armor_hp: 1_000_000.0,
                    hull_hp: 1_000_000.0,
                    reinforced_until_unix: 0,
                    last_damage_tick: 0,
                    last_damage_unix: 0,
                    self_destruct_at_unix: None,
                },
            );
        }
        let mut ext = eve_server::services::station_structure::structure_upwell_ops::load_ext_state(snap);
        ext.jump_bridge_links.insert(jb_a_id, jb_b_id);
        ext.jump_bridge_links.insert(jb_b_id, jb_a_id);
        eve_server::services::station_structure::structure_upwell_ops::save_ext_state(snap, &ext);
        let svc_id = 880_823_050;
        snap.items.insert(
            svc_id,
            InventoryItemRecord {
                item_id: svc_id,
                type_id: 35892, // Standup Market Hub I
                owner_id: corp_id,
                location_id: st_id,
                flag: InventoryFlag::ServiceSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(svc_id, true);
    });
    life::sync_all_structures(&ctx);

    // Initial profile assignment applies immediately; switching to Profile B queues a 5-minute (300s) delay!
    rpc(
        &ctx,
        &mut sess,
        "structureProfiles",
        "ChangeProfiles",
        &[PyRep::List(vec![PyRep::Integer(st_id), PyRep::Integer(jb_a_id)]), PyRep::Integer(prof_a)],
    )
    .await
    .unwrap();
    assert_eq!(
        rpc(&ctx, &mut sess, "structureProfiles", "DeleteProfile", &[PyRep::Integer(prof_a)])
            .await
            .expect_err("cannot delete profile in use")
            .code,
        "ProfileInUse"
    );
    rpc(
        &ctx,
        &mut sess,
        "structureProfiles",
        "ChangeProfiles",
        &[PyRep::List(vec![PyRep::Integer(st_id), PyRep::Integer(jb_a_id)]), PyRep::Integer(prof_b)],
    )
    .await
    .unwrap();

    let mut ally_sess = SessionState::new(8231);
    ally_sess.user_id = Some(2);
    spawn_pilot_ship(&ctx, &mut ally_sess, ALLY_PILOT, 98_820_002, PERIMETER, 880_823_011, 587, st_pos + DVec3::new(1_000.0, 0.0, 0.0)).await;

    // At t + 299s: Profile A is still active -> ally sees structure in GetStructures and GetJumpBridgesWithMyAccess returns V24 3-tuple.
    ctx.advance_virtual_clock(299);
    let PyRep::Tuple(jb_before) = rpc(&ctx, &mut ally_sess, "structureDirectory", "GetJumpBridgesWithMyAccess", &[]).await.unwrap() else {
        panic!("expected 3-tuple from GetJumpBridgesWithMyAccess");
    };
    assert_eq!(jb_before.len(), 3);
    let PyRep::List(jb_before_ids) = &jb_before[1] else { panic!("expected list at tuple[1]") };
    assert!(jb_before_ids.contains(&PyRep::Integer(jb_a_id)), "Ally must see jb_a_id before delayed profile switch completes");
    let corp_structs = rpc(&ctx, &mut sess, "structureDirectory", "GetCorporationStructures", &[]).await.unwrap();
    let PyRep::Dict(cs_map) = corp_structs else { panic!("expected dict") };
    let cs_entry = cs_map.iter().find(|(k, _)| k.as_i64() == Some(st_id)).map(|(_, v)| v).expect("corp structure");
    assert_eq!(kv_i64(cs_entry, "profileID"), prof_a);
    assert_eq!(kv_str(cs_entry, "powerState"), "FullPower");

    let ally_dir = rpc(&ctx, &mut ally_sess, "structureDirectory", "GetStructures", &[]).await.unwrap();
    let PyRep::Dict(ad_map) = ally_dir else { panic!("expected dict") };
    let ad_entry = ad_map.iter().find(|(k, _)| k.as_i64() == Some(st_id)).map(|(_, v)| v).expect("ally sees structure");
    assert_eq!(ad_entry.dict_get("fuelBlocks"), Some(&PyRep::None), "Non-corp pilot must not see exact fuelBlocks");
    assert_eq!(ad_entry.dict_get("profileID"), Some(&PyRep::None), "External pilot must not see profileID in GetStructures");
    let Some(PyRep::Dict(ad_services)) = ad_entry.dict_get("services") else { panic!("expected services dict") };
    assert!(ad_services.iter().any(|(k, v)| k.as_i64() == Some(5) && v.as_i64() == Some(1)), "Ally entry must expose online market service (SERVICE_MARKET = 5)");
    let Some(PyRep::List(ad_online_types)) = ad_entry.dict_get("onlineServiceTypeIDs") else { panic!("expected onlineServiceTypeIDs list") };
    assert!(ad_online_types.contains(&PyRep::Integer(35892)), "Ally entry must expose Standup Market Hub I (35892) in onlineServiceTypeIDs");

    let ally_info = rpc(&ctx, &mut ally_sess, "structureDirectory", "GetStructureInfo", &[PyRep::Integer(st_id)]).await.unwrap();
    assert_eq!(ally_info.dict_get("profileID"), Some(&PyRep::None), "External pilot must not see profileID in GetStructureInfo");
    let owner_info = rpc(&ctx, &mut sess, "structureDirectory", "GetStructureInfo", &[PyRep::Integer(st_id)]).await.unwrap();
    assert_eq!(kv_i64(&owner_info, "profileID"), prof_a, "Owning corp manager must see profileID in GetStructureInfo");

    // At t + 300s: Profile B (corpOnly = true) activates -> remote ally is denied GetStructureInfo and excluded from GetJumpBridgesWithMyAccess!
    ctx.advance_virtual_clock(1);
    let PyRep::Tuple(jb_after) = rpc(&ctx, &mut ally_sess, "structureDirectory", "GetJumpBridgesWithMyAccess", &[]).await.unwrap() else {
        panic!("expected 3-tuple");
    };
    let PyRep::List(jb_after_ids) = &jb_after[1] else { panic!("expected list") };
    assert!(!jb_after_ids.contains(&PyRep::Integer(jb_a_id)), "Ally must lose jb_a_id access at t + 300s");
    let ally_dir_after = rpc(&ctx, &mut ally_sess, "structureDirectory", "GetStructures", &[]).await.unwrap();
    let PyRep::Dict(ad_map_after) = ally_dir_after else { panic!("expected dict") };
    assert!(!ad_map_after.iter().any(|(k, _)| k.as_i64() == Some(st_id)), "Pilot without access must get nothing back from GetStructures");
    ally_sess.solar_system_id = Some(RANCER);
    ctx.db.with_mut_snapshot(|snap| snap.characters.get_mut(&ALLY_PILOT).unwrap().solar_system_id = RANCER);
    assert_eq!(
        rpc(&ctx, &mut ally_sess, "structureDirectory", "GetStructureInfo", &[PyRep::Integer(st_id)])
            .await
            .expect_err("Profile B corpOnly blocks remote ally")
            .code,
        "StructureAccessDenied"
    );
    let updated_cs = rpc(&ctx, &mut sess, "structureDirectory", "GetCorporationStructures", &[]).await.unwrap();
    let PyRep::Dict(ucs_map) = updated_cs else { panic!("expected dict") };
    let ucs_entry = ucs_map.iter().find(|(k, _)| k.as_i64() == Some(st_id)).map(|(_, v)| v).expect("corp structure");
    assert_eq!(kv_i64(ucs_entry, "profileID"), prof_b);
}

#[tokio::test]
async fn test_remediated_structure_control_release_and_flex_zero_arg_release() {
    // parity: feat:structures.board-structure, feat:structures.defense-settings, rpc:structureControl.TakeControl, rpc:structureControl.ReleaseControl, rpc:structureControl.GetStructurePilot
    eve_testkit::attest(&[
        "feat:structures.board-structure",
        "feat:structures.defense-settings",
        "rpc:structureControl.TakeControl",
        "rpc:structureControl.ReleaseControl",
        "rpc:structureControl.GetStructurePilot",
    ]);

    let ctx = new_ctx();
    let mut owner_sess = setup_owner_corp(&ctx, 8240, RANCER, "Control Release Corp", "CREL").await;
    let corp_id = owner_sess.corporation_id.expect("corp");
    let owner_orig_ship = owner_sess.ship_id.expect("owner ship");
    let st_id = 880_824_001;
    let flex_id = 880_824_002;
    let dir2: i64 = 90_000_041;
    let dir3: i64 = 90_000_042;
    let gunner: i64 = 90_000_043;
    let dir2_ship = 880_824_012;
    let dir3_ship = 880_824_013;
    let gunner_ship = 880_824_014;
    let st_pos = DVec3::new(0.0, 5.0e9, 0.0);
    let flex_pos = DVec3::new(1.0e9, 5.0e9, 0.0);

    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            st_id,
            UpwellStructureRecord {
                structure_id: st_id,
                type_id: 35832,
                owner_corp_id: corp_id,
                solar_system_id: RANCER,
                name: "Control Citadel".into(),
                state: life::STATE_SHIELD_VULNERABLE.into(),
                position: st_pos.to_array(),
                quantum_core_installed: true,
                fuel_blocks: 1_200,
                shield_hp: 1_000_000.0,
                armor_hp: 1_000_000.0,
                hull_hp: 1_000_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
        snap.structures.insert(
            flex_id,
            UpwellStructureRecord {
                structure_id: flex_id,
                type_id: 35841, // Ansiblex Jump Bridge (FLEX)
                owner_corp_id: corp_id,
                solar_system_id: RANCER,
                name: "Control Ansiblex".into(),
                state: life::STATE_SHIELD_VULNERABLE.into(),
                position: flex_pos.to_array(),
                quantum_core_installed: false,
                fuel_blocks: 1_200,
                shield_hp: 1_000_000.0,
                armor_hp: 1_000_000.0,
                hull_hp: 1_000_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
        for (cid, ccorp, shid, is_dir) in [
            (dir2, corp_id, dir2_ship, true),
            (dir3, corp_id, dir3_ship, true),
            (gunner, 98_824_099, gunner_ship, false),
        ] {
            let mut ch = snap.characters.get(&OWNER).cloned().unwrap();
            ch.character_id = cid;
            ch.corporation_id = ccorp;
            ch.solar_system_id = RANCER;
            ch.structure_id = Some(st_id);
            ch.station_id = None;
            ch.active_ship_id = shid;
            snap.characters.insert(cid, ch);
            snap.items.insert(
                shid,
                InventoryItemRecord {
                    item_id: shid,
                    type_id: 587,
                    owner_id: cid,
                    location_id: st_id,
                    flag: InventoryFlag::Hangar,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
            if is_dir {
                if let Some(corp) = snap.corporations.get_mut(&corp_id) {
                    corp.members.insert(cid);
                    corp.member_roles.insert(cid, eve_db::corp_roles::ROLE_DIRECTOR);
                }
            }
        }
    });
    life::sync_all_structures(&ctx);

    let dock_session = |sid: u64, uid: u64, cid: i64, ccorp: i64, role: u64, shid: i64| {
        let mut s = SessionState::new(sid);
        s.user_id = Some(uid);
        s.character_id = Some(cid);
        s.corporation_id = Some(ccorp);
        s.corp_role = role;
        s.solar_system_id = Some(RANCER);
        s.structure_id = Some(st_id);
        s.ship_id = Some(shid);
        s
    };

    // 1. Logout releases control & restores persisted active_ship_id so second director can TakeControl.
    let rancer_sys = ctx.systems.get_or_spawn(RANCER);
    assert!(rancer_sys.set_ball_position(owner_orig_ship, st_pos + DVec3::new(100.0, 0.0, 0.0)).await);
    rpc(&ctx, &mut owner_sess, "stationSvc", "Dock", &[PyRep::Integer(st_id)]).await.expect("owner dock");
    rpc(&ctx, &mut owner_sess, "structureControl", "TakeControl", &[PyRep::Integer(st_id)]).await.expect("owner TakeControl");
    assert_eq!(ctx.db.with_snapshot(|s| s.characters[&OWNER].active_ship_id), st_id);

    let mut dir2_sess = dock_session(8241, 2, dir2, corp_id, eve_db::corp_roles::ROLE_DIRECTOR, dir2_ship);
    assert_eq!(
        rpc(&ctx, &mut dir2_sess, "structureControl", "TakeControl", &[PyRep::Integer(st_id)])
            .await
            .expect_err("second director blocked while owner controlling")
            .code,
        "StructureAlreadyControlled"
    );

    rpc(&ctx, &mut owner_sess, "authentication", "Logout", &[]).await.expect("Logout");
    assert_eq!(ctx.db.with_snapshot(|s| s.characters[&OWNER].active_ship_id), owner_orig_ship, "Logout must restore persisted active_ship_id");
    assert_eq!(
        rpc(&ctx, &mut dir2_sess, "structureControl", "GetStructurePilot", &[PyRep::Integer(st_id)]).await.unwrap(),
        PyRep::None
    );

    // 2. Corporation change (LeaveCorp via KickOutMembers self) releases control & restores active_ship_id.
    rpc(&ctx, &mut dir2_sess, "structureControl", "TakeControl", &[PyRep::Integer(st_id)]).await.expect("dir2 TakeControl");
    assert_eq!(ctx.db.with_snapshot(|s| s.characters[&dir2].active_ship_id), st_id);
    ctx.advance_secs(12);
    rpc(&ctx, &mut dir2_sess, "corpRegistry", "KickOutMembers", &[PyRep::Integer(dir2)]).await.expect("dir2 LeaveCorp");
    assert_eq!(ctx.db.with_snapshot(|s| s.characters[&dir2].active_ship_id), dir2_ship, "Leaving corp must restore active_ship_id");
    assert_eq!(dir2_sess.ship_id, Some(dir2_ship));

    // 3. Role loss (UpdateMember stripping ROLE_DIRECTOR) releases control & restores active_ship_id.
    let mut dir3_sess = dock_session(8242, 3, dir3, corp_id, eve_db::corp_roles::ROLE_DIRECTOR, dir3_ship);
    rpc(&ctx, &mut dir3_sess, "structureControl", "TakeControl", &[PyRep::Integer(st_id)]).await.expect("dir3 TakeControl");
    assert_eq!(ctx.db.with_snapshot(|s| s.characters[&dir3].active_ship_id), st_id);

    // Log OWNER back in so OWNER can strip dir3's director role.
    let mut owner_relogin = SessionState::new(8243);
    rpc(&ctx, &mut owner_relogin, "authentication", "Login", &[PyRep::String("demo".into()), PyRep::String("demo-secret".into())]).await.unwrap();
    rpc(&ctx, &mut owner_relogin, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(OWNER)]).await.unwrap();
    rpc(&ctx, &mut owner_relogin, "corpRegistry", "UpdateMember", &[PyRep::Integer(dir3), PyRep::Integer(0)]).await.expect("strip dir3 roles");
    assert_eq!(ctx.db.with_snapshot(|s| s.characters[&dir3].active_ship_id), dir3_ship, "Role loss must restore active_ship_id");
    ctx.sync_session(&mut dir3_sess);
    assert_eq!(dir3_sess.ship_id, Some(dir3_ship), "Role loss must synchronize live SessionState.ship_id");

    // 4. ACL loss (removing gunner from defense access group) releases control & restores active_ship_id.
    let def_grp = kv_i64(
        &rpc(&ctx, &mut owner_relogin, "ownerGroupManager", "Create", &[PyRep::String("Defense Gunners".into()), PyRep::String("Gunners".into())]).await.unwrap(),
        "groupID",
    );
    rpc(&ctx, &mut owner_relogin, "ownerGroupManager", "AddMembers", &[PyRep::Integer(def_grp), PyRep::List(vec![PyRep::Integer(gunner)])]).await.unwrap();
    let def_prof = rpc(&ctx, &mut owner_relogin, "structureProfiles", "CreateProfile", &[PyRep::String("Gunner ACL Profile".into()), PyRep::String("Defense ACL".into())]).await.unwrap().as_i64().unwrap();
    rpc(
        &ctx,
        &mut owner_relogin,
        "structureProfiles",
        "SaveProfileSettings",
        &[
            PyRep::Integer(def_prof),
            PyRep::List(vec![
                PyRep::Tuple(vec![PyRep::Integer(19), PyRep::Integer(0), PyRep::Integer(1)]),
                PyRep::Tuple(vec![PyRep::Integer(17), PyRep::Integer(def_grp), PyRep::Integer(1)]),
            ]),
        ],
    )
    .await
    .unwrap();
    rpc(&ctx, &mut owner_relogin, "structureProfiles", "ChangeProfiles", &[PyRep::List(vec![PyRep::Integer(st_id)]), PyRep::Integer(def_prof)]).await.unwrap();

    let mut gunner_sess = dock_session(8244, 4, gunner, 98_824_099, 0, gunner_ship);
    rpc(&ctx, &mut gunner_sess, "structureControl", "TakeControl", &[PyRep::Integer(st_id)]).await.expect("gunner TakeControl via defense ACL");
    assert_eq!(ctx.db.with_snapshot(|s| s.characters[&gunner].active_ship_id), st_id);

    rpc(&ctx, &mut owner_relogin, "ownerGroupManager", "DeleteMembers", &[PyRep::Integer(def_grp), PyRep::List(vec![PyRep::Integer(gunner)])]).await.expect("remove gunner from defense ACL");
    assert_eq!(ctx.db.with_snapshot(|s| s.characters[&gunner].active_ship_id), gunner_ship, "ACL loss must restore active_ship_id");
    ctx.sync_session(&mut gunner_sess);
    assert_eq!(gunner_sess.ship_id, Some(gunner_ship), "ACL loss must synchronize live SessionState.ship_id");

    // 5. Abrupt disconnect releases control & restores active_ship_id.
    rpc(&ctx, &mut owner_relogin, "structureControl", "TakeControl", &[PyRep::Integer(st_id)]).await.expect("owner TakeControl before disconnect");
    assert_eq!(ctx.db.with_snapshot(|s| s.characters[&OWNER].active_ship_id), st_id);
    eve_server::services::fleet_social::fleet_social_notify::handle_character_disconnect(&ctx, OWNER);
    assert_eq!(ctx.db.with_snapshot(|s| s.characters[&OWNER].active_ship_id), owner_orig_ship, "Disconnect must restore active_ship_id");

    // 6. 0-arg ReleaseControl() on a FLEX structure in space (session.structure_id == None) succeeds & restores ship_id!
    rpc(&ctx, &mut owner_relogin, "authentication", "Logout", &[]).await.unwrap();
    let mut flex_sess = SessionState::new(8245);
    rpc(&ctx, &mut flex_sess, "authentication", "Login", &[PyRep::String("demo".into()), PyRep::String("demo-secret".into())]).await.unwrap();
    rpc(&ctx, &mut flex_sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(OWNER)]).await.unwrap();
    ctx.advance_secs(12);
    rpc(&ctx, &mut flex_sess, "structureDocking", "Undock", &[]).await.expect("undock for FLEX test");
    assert!(rancer_sys.set_ball_position(owner_orig_ship, flex_pos + DVec3::new(500.0, 0.0, 0.0)).await);
    rpc(&ctx, &mut flex_sess, "structureControl", "TakeControl", &[PyRep::Integer(flex_id)]).await.expect("TakeControl on FLEX");
    assert_eq!(flex_sess.ship_id, Some(flex_id));
    rpc(&ctx, &mut flex_sess, "structureControl", "ReleaseControl", &[]).await.expect("0-arg ReleaseControl on FLEX");
    assert_eq!(flex_sess.ship_id, Some(owner_orig_ship), "0-arg ReleaseControl on FLEX must restore session.ship_id");
    assert_eq!(ctx.db.with_snapshot(|s| s.characters[&OWNER].active_ship_id), owner_orig_ship);

    // 7. Standing direction: pilot->corp negative standing is NOT auto-targeted, whereas corp->pilot negative standing IS auto-targeted!
    ctx.db.with_mut_snapshot(|snap| {
        let mut ds = eve_server::services::structure_acl_defense_rules::load_defense_settings(snap, st_id);
        ds.auto_defense_enabled = true;
        ds.standing_threshold = -1.0;
        ds.target_blocked_acl = false;
        eve_server::services::structure_acl_defense_rules::save_defense_settings(snap, &ds);
        if let Some(ch) = snap.characters.get_mut(&gunner) {
            ch.standings.insert(corp_id, -10.0);
        }
    });
    let pilot_to_corp_targeted = ctx.db.with_snapshot(|snap| {
        eve_server::services::structure_acl_defense_rules::evaluate_auto_defense_target(&ctx, snap, st_id, gunner)
    });
    assert!(!pilot_to_corp_targeted, "Pilot->corp standing must NOT trigger structure auto-defense");
    ctx.db.with_mut_snapshot(|snap| {
        snap.corp_contacts.entry(corp_id).or_default().push(eve_db::SocialContactRecord {
            contact_id: gunner,
            contact_type: "character".into(),
            standing: -5.0,
            is_watched: false,
            is_blocked: false,
            label_ids: Vec::new(),
        });
    });
    let corp_to_pilot_targeted = ctx.db.with_snapshot(|snap| {
        eve_server::services::structure_acl_defense_rules::evaluate_auto_defense_target(&ctx, snap, st_id, gunner)
    });
    assert!(corp_to_pilot_targeted, "Corp->pilot standing < standingThreshold MUST trigger structure auto-defense");
}

}

#[allow(dead_code, unused_imports)]
pub(crate) mod upwell_structures_damage_caps {
    use super::*;

//  End-to-end remediation tests for involuntary structure control release synchronization
//  (`OnSessionChanged` `"shipid"`, `OnStructureTargetRemoved`, live `SessionState.ship_id`,
//  corp-join control release, and delayed 5-minute profile transition control release).

use eve_db::{CorpApplicationRecord, CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents, UpwellStructureRecord};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use eve_server::services::station_structure::{structure_lifecycle as life, structure_upwell_ops as ops};
use eve_server::services::structure_profile_services as svc_acl;
use glam::DVec3;
use std::collections::HashMap;
use std::sync::Arc;

const OWNER: i64 = 90_000_001;
const RANCER: u32 = 30_002_718;

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
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

fn make_citadel(structure_id: i64, owner_corp_id: i64, name: &str, position: [f64; 3]) -> UpwellStructureRecord {
    UpwellStructureRecord {
        structure_id, type_id: 35833, owner_corp_id, solar_system_id: RANCER, name: name.into(),
        state: life::STATE_SHIELD_VULNERABLE.into(), position, quantum_core_installed: true,
        fuel_blocks: 1_200, shield_hp: 1_000_000.0, armor_hp: 1_000_000.0, hull_hp: 1_000_000.0,
        reinforced_until_unix: 0, last_damage_tick: 0, last_damage_unix: 0, self_destruct_at_unix: None,
    }
}

fn make_item(item_id: i64, type_id: u32, owner_id: i64, location_id: i64, flag: InventoryFlag, quantity: i32, singleton: bool) -> InventoryItemRecord {
    InventoryItemRecord { item_id, type_id, owner_id, location_id, flag, quantity, singleton, version: 1, ..Default::default() }
}

fn has_shipid_session_change(notifs: &[eve_server::router::PendingMachoNotification], expected_ship: i64) -> bool {
    notifs.iter().any(|n| {
        n.event == "OnSessionChanged"
            && n.args
                .get(2)
                .and_then(|d| d.dict_get("shipid"))
                .and_then(|t| match t {
                    PyRep::Tuple(pair) if pair.len() == 2 => pair[1].as_i64(),
                    _ => None,
                })
                == Some(expected_ship)
    })
}

#[tokio::test]
async fn test_remediated_involuntary_structure_control_release_session_sync() {
    // parity: feat:structures.board-structure, feat:structures.defense-settings, rpc:structureControl.TakeControl, rpc:structureControl.GetStructurePilot
    eve_testkit::attest(&[
        "feat:structures.board-structure",
        "feat:structures.defense-settings",
        "rpc:structureControl.TakeControl",
        "rpc:structureControl.GetStructurePilot",
    ]);

    let ctx = new_ctx();
    let station_id = ctx.sde.stations.values().find(|s| s.solar_system_id == RANCER).map(|s| s.station_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&OWNER) {
            ch.solar_system_id = RANCER;
            ch.station_id = station_id;
            if let Some(sh) = snap.items.get_mut(&ch.active_ship_id) {
                sh.location_id = station_id.unwrap_or(i64::from(RANCER));
            }
        }
    });
    let mut owner_sess = SessionState::new(8250);
    rpc(&ctx, &mut owner_sess, "authentication", "Login", &[PyRep::String("demo".into()), PyRep::String("demo-secret".into())]).await.unwrap();
    rpc(&ctx, &mut owner_sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(OWNER)]).await.unwrap();
    ctx.advance_secs(12);
    rpc(&ctx, &mut owner_sess, "corpRegistry", "AddCorporation", &[PyRep::String("Session Sync Corp".into()), PyRep::String("SSYN".into())]).await.unwrap();
    ctx.advance_virtual_clock(7 * 86_400 + 12);
    let corp_id = owner_sess.corporation_id.expect("corp");

    let st_id = 880_825_001;
    let dir_pilot: i64 = 90_000_051;
    let acl_pilot: i64 = 90_000_052;
    let join_pilot: i64 = 90_000_053;
    let other_ceo: i64 = 90_000_054;
    let dir_ship = 880_825_011;
    let acl_ship = 880_825_012;
    let join_ship = 880_825_013;
    let other_ceo_ship = 880_825_014;
    let old_corp_id = 98_825_055;
    let target_corp_id = 98_825_077;

    let now_init = ctx.now_unix();
    let online_pilots = [
        (dir_pilot, corp_id, dir_ship),
        (acl_pilot, 98_825_088, acl_ship),
        (join_pilot, old_corp_id, join_ship),
        (other_ceo, target_corp_id, other_ceo_ship),
    ];
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(st_id, make_citadel(st_id, corp_id, "Sync Citadel", [0.0, 5.0e9, 0.0]));
        for (cid, name, ticker) in [(old_corp_id, "Old Gunner Corp", "OLDG"), (target_corp_id, "New Target Corp", "NEWT")] {
            let mut c = CorporationRecord::default();
            c.corporation_id = cid;
            c.name = name.into();
            c.ticker = ticker.into();
            c.ceo_id = if cid == target_corp_id { other_ceo } else { 90_000_059 };
            c.members.insert(c.ceo_id);
            snap.corporations.insert(cid, c);
        }
        snap.corporations.get_mut(&old_corp_id).unwrap().members.insert(join_pilot);
        snap.corporations.get_mut(&corp_id).unwrap().members.insert(dir_pilot);
        snap.corporations.get_mut(&corp_id).unwrap().member_roles.insert(dir_pilot, eve_db::corp_roles::ROLE_DIRECTOR);

        for (idx, (cid, ccorp, shid)) in online_pilots.into_iter().enumerate() {
            let uid = (idx + 2) as u64;
            if let Some(mut acc) = snap.accounts.get("demo").cloned() {
                acc.user_id = uid;
                acc.username = format!("pilot_{uid}");
                snap.accounts.insert(acc.username.clone(), acc);
            }
            let mut ch = snap.characters.get(&OWNER).cloned().unwrap();
            ch.character_id = cid;
            ch.user_id = uid;
            ch.corporation_id = ccorp;
            ch.solar_system_id = RANCER;
            ch.structure_id = Some(st_id);
            ch.station_id = None;
            ch.active_ship_id = shid;
            snap.characters.insert(cid, ch);
            snap.items.insert(shid, make_item(shid, 587, cid, st_id, InventoryFlag::Hangar, 1, true));
        }
    });
    for (idx, (cid, _, _)) in online_pilots.into_iter().enumerate() {
        ctx.db.acquire_online_lock((idx + 2) as u64, (8251 + idx) as u64, Some(cid), true, now_init).unwrap();
    }
    life::sync_all_structures(&ctx);

    let dock_session = |sid: u64, uid: u64, cid: i64, ccorp: i64, role: u64, shid: i64| {
        let mut s = SessionState::new(sid);
        s.user_id = Some(uid);
        s.character_id = Some(cid);
        s.corporation_id = Some(ccorp);
        s.corp_role = role;
        s.solar_system_id = Some(RANCER);
        s.structure_id = Some(st_id);
        s.ship_id = Some(shid);
        s
    };

    // 1. Role loss emits OnSessionChanged("shipid") + OnStructureTargetRemoved and syncs live SessionState.ship_id.
    let mut dir_sess = dock_session(8251, 2, dir_pilot, corp_id, eve_db::corp_roles::ROLE_DIRECTOR, dir_ship);
    rpc(&ctx, &mut dir_sess, "structureControl", "TakeControl", &[PyRep::Integer(st_id)]).await.unwrap();
    assert_eq!(dir_sess.ship_id, Some(st_id));
    let _ = ctx.drain_notifications_for_char(dir_pilot);
    rpc(&ctx, &mut owner_sess, "corpRegistry", "UpdateMember", &[PyRep::Integer(dir_pilot), PyRep::Integer(0)]).await.unwrap();
    let dir_notifs = ctx.drain_notifications_for_char(dir_pilot);
    assert!(has_shipid_session_change(&dir_notifs, dir_ship), "Role loss must emit OnSessionChanged shipid");
    assert!(dir_notifs.iter().any(|n| n.event == "OnStructureTargetRemoved"), "Role loss must emit OnStructureTargetRemoved");
    ctx.sync_session(&mut dir_sess);
    assert_eq!(dir_sess.ship_id, Some(dir_ship), "Live SessionState.ship_id must restore on role loss");

    // 2. Configure defense ACL profile with acl_pilot and old_corp_id, then test ACL loss and corp-join control release.
    let acl_grp = kv_i64(&rpc(&ctx, &mut owner_sess, "ownerGroupManager", "Create", &[PyRep::String("Sync ACL".into()), PyRep::String("SACL".into())]).await.unwrap(), "groupID");
    rpc(&ctx, &mut owner_sess, "ownerGroupManager", "AddMembers", &[PyRep::Integer(acl_grp), PyRep::List(vec![PyRep::Integer(acl_pilot), PyRep::Integer(old_corp_id)])]).await.unwrap();
    let acl_prof = rpc(&ctx, &mut owner_sess, "structureProfiles", "CreateProfile", &[PyRep::String("ACL Prof".into()), PyRep::String("ACL".into())]).await.unwrap().as_i64().unwrap();
    let corp_only_prof = rpc(&ctx, &mut owner_sess, "structureProfiles", "CreateProfile", &[PyRep::String("Corp Only Prof".into()), PyRep::String("CORP".into())]).await.unwrap().as_i64().unwrap();
    rpc(&ctx, &mut owner_sess, "structureProfiles", "SaveProfileSettings", &[
        PyRep::Integer(acl_prof),
        PyRep::List(vec![
            PyRep::Tuple(vec![PyRep::Integer(19), PyRep::Integer(0), PyRep::Integer(1)]),
            PyRep::Tuple(vec![PyRep::Integer(17), PyRep::Integer(acl_grp), PyRep::Integer(1)]),
        ]),
    ]).await.unwrap();
    rpc(&ctx, &mut owner_sess, "structureProfiles", "SaveProfileSettings", &[
        PyRep::Integer(corp_only_prof),
        PyRep::List(vec![
            PyRep::Tuple(vec![PyRep::Integer(19), PyRep::Integer(0), PyRep::Integer(0)]),
        ]),
    ]).await.unwrap();
    rpc(&ctx, &mut owner_sess, "structureProfiles", "ChangeProfiles", &[PyRep::List(vec![PyRep::Integer(st_id)]), PyRep::Integer(acl_prof)]).await.unwrap();

    let mut acl_sess = dock_session(8252, 3, acl_pilot, 98_825_088, 0, acl_ship);
    rpc(&ctx, &mut acl_sess, "structureControl", "TakeControl", &[PyRep::Integer(st_id)]).await.unwrap();
    let _ = ctx.drain_notifications_for_char(acl_pilot);
    rpc(&ctx, &mut owner_sess, "ownerGroupManager", "DeleteMembers", &[PyRep::Integer(acl_grp), PyRep::List(vec![PyRep::Integer(acl_pilot)])]).await.unwrap();
    let acl_notifs = ctx.drain_notifications_for_char(acl_pilot);
    assert!(has_shipid_session_change(&acl_notifs, acl_ship), "ACL loss must emit OnSessionChanged shipid");
    ctx.sync_session(&mut acl_sess);
    assert_eq!(acl_sess.ship_id, Some(acl_ship), "Live SessionState.ship_id must restore on ACL loss");

    // 3. Corp join via UpdateApplicationOffer releases control when old corp loses ACL authorization.
    let mut join_sess = dock_session(8253, 4, join_pilot, old_corp_id, 0, join_ship);
    rpc(&ctx, &mut join_sess, "structureControl", "TakeControl", &[PyRep::Integer(st_id)]).await.unwrap();
    assert_eq!(join_sess.ship_id, Some(st_id));
    let app_id = 770_825_001;
    let now_u = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.get_mut(&target_corp_id).unwrap().applications.insert(
            app_id,
            CorpApplicationRecord {
                application_id: app_id,
                character_id: join_pilot,
                corporation_id: target_corp_id,
                message: "Joining new corp".into(),
                status: "Pending".into(),
                created_unix: now_u,
            },
        );
    });
    let _ = ctx.drain_notifications_for_char(join_pilot);
    let mut ceo_sess = dock_session(8254, 5, other_ceo, target_corp_id, eve_db::corp_roles::ROLE_DIRECTOR, other_ceo_ship);
    rpc(&ctx, &mut ceo_sess, "corpRegistry", "UpdateApplicationOffer", &[PyRep::Integer(app_id), PyRep::String("Accepted".into())]).await.unwrap();
    let join_notifs = ctx.drain_notifications_for_char(join_pilot);
    assert!(has_shipid_session_change(&join_notifs, join_ship), "Corp join must emit OnSessionChanged shipid");
    assert!(join_notifs.iter().any(|n| n.event == "OnStructureTargetRemoved"));
    ctx.sync_session(&mut join_sess);
    assert_eq!(join_sess.ship_id, Some(join_ship), "Live SessionState.ship_id must restore on corp join");

    // 4. Delayed 300s profile transition releases unauthorized gunner control and emits OnSessionChanged("shipid").
    rpc(&ctx, &mut owner_sess, "ownerGroupManager", "AddMembers", &[PyRep::Integer(acl_grp), PyRep::List(vec![PyRep::Integer(acl_pilot)])]).await.unwrap();
    rpc(&ctx, &mut acl_sess, "structureControl", "TakeControl", &[PyRep::Integer(st_id)]).await.unwrap();
    assert_eq!(acl_sess.ship_id, Some(st_id));
    rpc(&ctx, &mut owner_sess, "structureProfiles", "ChangeProfiles", &[PyRep::List(vec![PyRep::Integer(st_id)]), PyRep::Integer(corp_only_prof)]).await.unwrap();
    let _ = ctx.drain_notifications_for_char(acl_pilot);
    ctx.advance_virtual_clock(300);
    assert_eq!(rpc(&ctx, &mut owner_sess, "structureControl", "GetStructurePilot", &[PyRep::Integer(st_id)]).await.unwrap(), PyRep::None);
    let prof_notifs = ctx.drain_notifications_for_char(acl_pilot);
    assert!(has_shipid_session_change(&prof_notifs, acl_ship), "Delayed profile switch must emit OnSessionChanged shipid");
    ctx.sync_session(&mut acl_sess);
    assert_eq!(acl_sess.ship_id, Some(acl_ship), "Live SessionState.ship_id must restore after delayed profile switch");
}

#[tokio::test]
async fn test_remediated_bf01_bf02_upwell_market_acl_offline_filter_and_corp_div1_ledger() {
    // parity: feat:structures.profiles, feat:market.structure-markets, rpc:marketProxy.PlaceBuyOrder, rpc:marketProxy.PlaceMultiSellOrder, rpc:marketProxy.ModifyCharOrder, rpc:marketProxy.GetStationAsks, rpc:reprocessingSvc.Reprocess, rpc:jumpCloneSvc.InstallCloneInStructure
    eve_testkit::attest(&[
        "feat:structures.profiles",
        "feat:market.structure-markets",
        "rpc:marketProxy.PlaceBuyOrder",
        "rpc:marketProxy.PlaceMultiSellOrder",
        "rpc:marketProxy.ModifyCharOrder",
        "rpc:marketProxy.GetStationAsks",
        "rpc:reprocessingSvc.Reprocess",
        "rpc:jumpCloneSvc.InstallCloneInStructure",
    ]);

    let ctx = new_ctx();
    let mkt_sys: u32 = RANCER;
    let station_id = ctx.sde.stations.values().find(|s| s.solar_system_id == mkt_sys).map(|s| s.station_id);
    let region_id = ctx.sde.solar_systems.get(&mkt_sys).map(|s| s.region_id).unwrap_or(10_000_002);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&OWNER) {
            ch.solar_system_id = mkt_sys;
            ch.region_id = region_id;
            ch.station_id = station_id;
            if let Some(sh) = snap.items.get_mut(&ch.active_ship_id) {
                sh.location_id = station_id.unwrap_or(i64::from(mkt_sys));
            }
        }
    });
    let mut owner_sess = SessionState::new(8260);
    rpc(&ctx, &mut owner_sess, "authentication", "Login", &[PyRep::String("demo".into()), PyRep::String("demo-secret".into())]).await.unwrap();
    rpc(&ctx, &mut owner_sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(OWNER)]).await.unwrap();
    ctx.advance_secs(12);
    rpc(&ctx, &mut owner_sess, "corpRegistry", "AddCorporation", &[PyRep::String("Ledger Market Corp".into()), PyRep::String("LMKT".into())]).await.unwrap();
    ctx.advance_virtual_clock(7 * 86_400 + 12);
    let corp_id = owner_sess.corporation_id.expect("corp");

    let st_id: i64 = 1_000_880_826_001;
    let vip_cid: i64 = 90_000_061;
    let std_cid: i64 = 90_000_062;
    let dock_only_cid: i64 = 90_000_063;
    let ore_item_id: i64 = 880_826_301;
    let sell_item_id: i64 = 880_826_302;

    ctx.db.with_mut_snapshot(|snap| {
        let mut citadel = make_citadel(st_id, corp_id, "Market Ledger Citadel", [1.0e9, 0.0, 0.0]);
        citadel.solar_system_id = mkt_sys;
        snap.structures.insert(st_id, citadel);
        for (idx, (flag, srv_tid)) in [
            (InventoryFlag::ServiceSlot0, 35892_u32),
            (InventoryFlag::ServiceSlot1, 35888_u32),
            (InventoryFlag::ServiceSlot2, 35894_u32),
        ].into_iter().enumerate() {
            let mid = 880_826_100 + idx as i64;
            snap.items.insert(mid, make_item(mid, srv_tid, corp_id, st_id, flag, 1, true));
            snap.module_online_states.insert(mid, true);
        }
        for (idx, cid) in [vip_cid, std_cid, dock_only_cid].into_iter().enumerate() {
            let mut ch = snap.characters.get(&OWNER).cloned().unwrap();
            ch.character_id = cid;
            ch.corporation_id = 98_826_100 + idx as i64;
            ch.solar_system_id = mkt_sys;
            ch.region_id = region_id;
            ch.structure_id = Some(st_id);
            ch.station_id = None;
            ch.active_ship_id = 880_826_200 + idx as i64;
            ch.balance = IskCents(100_000_000_00);
            snap.characters.insert(cid, ch);
            snap.skills.entry(cid).or_default().insert(3446, eve_db::TrainedSkillRecord { type_id: 3446, level: 0, skill_points: 0.0 });
            snap.skills.entry(cid).or_default().insert(24242, eve_db::TrainedSkillRecord { type_id: 24242, level: 2, skill_points: 8_000.0 });
        }
        snap.items.insert(ore_item_id, make_item(ore_item_id, 1230, vip_cid, st_id, InventoryFlag::Hangar, 1_000, false));
        snap.items.insert(sell_item_id, make_item(sell_item_id, 34, vip_cid, st_id, InventoryFlag::Hangar, 2_000, false));
    });
    life::sync_all_structures(&ctx);

    let grp_dock = kv_i64(&rpc(&ctx, &mut owner_sess, "ownerGroupManager", "Create", &[PyRep::String("Dockers".into()), PyRep::String("DOCK".into())]).await.unwrap(), "groupID");
    let grp_vip = kv_i64(&rpc(&ctx, &mut owner_sess, "ownerGroupManager", "Create", &[PyRep::String("VIP Market".into()), PyRep::String("VIPM".into())]).await.unwrap(), "groupID");
    let grp_std = kv_i64(&rpc(&ctx, &mut owner_sess, "ownerGroupManager", "Create", &[PyRep::String("STD Market".into()), PyRep::String("STDM".into())]).await.unwrap(), "groupID");
    rpc(&ctx, &mut owner_sess, "ownerGroupManager", "AddMembers", &[PyRep::Integer(grp_dock), PyRep::List(vec![PyRep::Integer(vip_cid), PyRep::Integer(std_cid), PyRep::Integer(dock_only_cid)])]).await.unwrap();
    rpc(&ctx, &mut owner_sess, "ownerGroupManager", "AddMembers", &[PyRep::Integer(grp_vip), PyRep::List(vec![PyRep::Integer(vip_cid)])]).await.unwrap();
    rpc(&ctx, &mut owner_sess, "ownerGroupManager", "AddMembers", &[PyRep::Integer(grp_std), PyRep::List(vec![PyRep::Integer(std_cid)])]).await.unwrap();

    // Configure per-service ACLs directly on st_id via SaveProfileSettings:
    // VIP = 1% market / 2% repro / 100k clone; STD = 5% market; dock_only_cid has NO market ACL!
    rpc(&ctx, &mut owner_sess, "structureProfiles", "SaveProfileSettings", &[
        PyRep::Integer(st_id),
        PyRep::List(vec![
            PyRep::Tuple(vec![PyRep::Integer(svc_acl::SETTING_HOUSING_CAN_DOCK), PyRep::Integer(grp_dock), PyRep::Real(0.0)]),
            PyRep::Tuple(vec![PyRep::Integer(svc_acl::SETTING_MARKET_TAX), PyRep::Integer(grp_vip), PyRep::Real(0.01)]),
            PyRep::Tuple(vec![PyRep::Integer(svc_acl::SETTING_MARKET_TAX), PyRep::Integer(grp_std), PyRep::Real(0.05)]),
            PyRep::Tuple(vec![PyRep::Integer(svc_acl::SETTING_REPROCESSING_TAX), PyRep::Integer(grp_vip), PyRep::Real(0.02)]),
            PyRep::Tuple(vec![PyRep::Integer(svc_acl::SETTING_CLONINGBAY_TAX), PyRep::Integer(grp_vip), PyRep::Real(100_000.0)]),
        ]),
    ]).await.unwrap();

    let mk_sess = |sid: u64, cid: i64, corp: i64| {
        let mut s = SessionState::new(sid);
        s.user_id = Some(sid);
        s.character_id = Some(cid);
        s.corporation_id = Some(corp);
        s.solar_system_id = Some(mkt_sys);
        s.region_id = Some(region_id);
        s.structure_id = Some(st_id);
        s.ship_id = Some(880_826_200 + (sid - 8261) as i64);
        s
    };
    let mut sess_vip = mk_sess(8261, vip_cid, 98_826_100);
    let mut sess_std = mk_sess(8262, std_cid, 98_826_101);
    let mut sess_dock_only = mk_sess(8263, dock_only_cid, 98_826_102);

    let buy_args = [
        PyRep::Integer(st_id), PyRep::Integer(34), PyRep::Real(100.0), PyRep::Integer(10_000),
        PyRep::Integer(-1), PyRep::Integer(1), PyRep::Integer(90), PyRep::Bool(false),
    ];
    // Negative (BF-01): Pilot with docking access only is denied market orders
    assert_eq!(rpc(&ctx, &mut sess_dock_only, "marketProxy", "PlaceBuyOrder", &buy_args).await.unwrap_err().code, "StructureServiceAccessDenied");

    // Positive (BF-01 + BF-02): VIP (1% + 0.5% SCC = 15,000 ISK) and STD (5% + 0.5% SCC = 55,000 ISK) pay distinct per-ACL rates
    let corp_div1_before = ctx.db.with_snapshot(|s| s.corporations[&corp_id].division_balance(1).0);
    let vip_bal_0 = ctx.db.get_character(vip_cid).unwrap().balance.0;
    let std_bal_0 = ctx.db.get_character(std_cid).unwrap().balance.0;
    rpc(&ctx, &mut sess_vip, "marketProxy", "PlaceBuyOrder", &buy_args).await.unwrap();
    rpc(&ctx, &mut sess_std, "marketProxy", "PlaceBuyOrder", &buy_args).await.unwrap();
    assert_eq!(vip_bal_0 - ctx.db.get_character(vip_cid).unwrap().balance.0, 1_015_000_00);
    assert_eq!(std_bal_0 - ctx.db.get_character(std_cid).unwrap().balance.0, 1_055_000_00);
    // Owner corp div 1 receives 10,000 ISK (VIP 1%) + 50,000 ISK (STD 5%) = 60,000 ISK
    let corp_div1_after_buy = ctx.db.with_snapshot(|s| s.corporations[&corp_id].division_balance(1).0);
    assert_eq!(corp_div1_after_buy - corp_div1_before, 60_000_00);

    // Positive (BF-01): VIP places MultiSellOrder (1,000 x 200 ISK = 200k ISK -> 1% owner = 2k ISK + 0.5% SCC = 1k ISK)
    let sell_entry = PyRep::keyval([
        ("stationID", PyRep::Integer(st_id)),
        ("typeID", PyRep::Integer(34)),
        ("itemID", PyRep::Integer(sell_item_id)),
        ("price", PyRep::Real(200.0)),
        ("quantity", PyRep::Integer(1_000)),
        ("duration", PyRep::Integer(90)),
    ]);
    let vip_bal_1 = ctx.db.get_character(vip_cid).unwrap().balance.0;
    let (div1_pre_sell, cj_pre_sell, wj_pre_sell) = ctx.db.with_snapshot(|s| {
        (
            s.corporations[&corp_id].division_balance(1).0,
            s.corp_wallet_journals.get(&(corp_id, 1)).map_or(0, Vec::len),
            s.wallet_journal.len(),
        )
    });
    rpc(&ctx, &mut sess_vip, "marketProxy", "PlaceMultiSellOrder", &[PyRep::List(vec![sell_entry]), PyRep::Bool(false), PyRep::Integer(90)]).await.unwrap();
    assert_eq!(vip_bal_1 - ctx.db.get_character(vip_cid).unwrap().balance.0, 3_000_00);
    ctx.db.with_snapshot(|s| {
        assert_eq!(s.corporations[&corp_id].division_balance(1).0 - div1_pre_sell, 2_000_00, "PlaceMultiSellOrder must credit 2k ISK owner share to corp div 1");
        let cj = s.corp_wallet_journals.get(&(corp_id, 1)).expect("corp div 1 journal after PlaceMultiSellOrder");
        assert_eq!(cj.len(), cj_pre_sell + 1, "PlaceMultiSellOrder must append to corp_wallet_journals[(corp_id, 1)]");
        let entry = &cj[cj_pre_sell];
        assert_eq!(entry.ref_type, "MarketBrokerFee");
        assert!((entry.amount - 2_000.0).abs() < 1e-6);
        assert!(s.wallet_journal[wj_pre_sell..].iter().any(|e| e.character_id == corp_id && e.entry_type == "MarketBrokerFee" && e.amount.0 == 2_000_00));
    });
    let sell_oid = ctx.db.with_snapshot(|s| s.market_orders.values().find(|o| !o.is_bid && o.owner_id == vip_cid).unwrap().order_id);

    // Advance 301s past order modification cooldown and test ModifyCharOrder per-pilot broker fee
    ctx.advance_virtual_clock(301);
    let vip_bal_2 = ctx.db.get_character(vip_cid).unwrap().balance.0;
    rpc(&ctx, &mut sess_vip, "marketProxy", "ModifyCharOrder", &[PyRep::Integer(sell_oid), PyRep::Real(250.0)]).await.unwrap();
    // 1.5% * (0.5 * 250k + 50k) = 0.015 * 175k = 2,625 ISK
    assert_eq!(vip_bal_2 - ctx.db.get_character(vip_cid).unwrap().balance.0, 2_625_00);

    // Positive (BF-02): ReprocessingTax and JumpCloneInstallationFee credit corp division 1 AND corp_wallet_journals[(corp_id, 1)]
    let div1_pre_repro = ctx.db.with_snapshot(|s| s.corporations[&corp_id].division_balance(1).0);
    rpc(&ctx, &mut sess_vip, "reprocessingSvc", "Reprocess", &[PyRep::Integer(ore_item_id)]).await.unwrap();
    let div1_post_repro = ctx.db.with_snapshot(|s| s.corporations[&corp_id].division_balance(1).0);
    assert!(div1_post_repro > div1_pre_repro, "ReprocessingTax must credit corp division 1");

    rpc(&ctx, &mut sess_vip, "jumpCloneSvc", "InstallCloneInStructure", &[PyRep::Integer(st_id)]).await.unwrap();
    let div1_post_clone = ctx.db.with_snapshot(|s| s.corporations[&corp_id].division_balance(1).0);
    assert_eq!(div1_post_clone - div1_post_repro, 100_000_00, "JumpCloneInstallationFee must credit corp division 1");

    ctx.db.with_snapshot(|s| {
        let corp = &s.corporations[&corp_id];
        assert_eq!(corp.division_balance(1), corp.master_wallet);
        let cj = s.corp_wallet_journals.get(&(corp_id, 1)).expect("corp div 1 journal");
        assert!(cj.iter().any(|e| e.ref_type == "ReprocessingTax" && e.amount > 0.0));
        assert!(cj.iter().any(|e| e.ref_type == "JumpCloneInstallationFee" && (e.amount - 100_000.0).abs() < 1e-6));
        assert!(cj.iter().any(|e| e.ref_type == "MarketBrokerFee" && e.amount > 0.0));
        assert!(s.wallet_journal.iter().any(|e| e.character_id == corp_id && e.entry_type == "ReprocessingTax" && e.amount.0 > 0));
        assert!(s.wallet_journal.iter().any(|e| e.character_id == corp_id && e.entry_type == "JumpCloneInstallationFee" && e.amount.0 == 100_000_00));
    });

    // Negative (BF-01): Disabling market service in profile denies new orders, and taking Market Hub offline hides asks from GetStationAsks
    let rowset_len = |rep: &PyRep| -> usize {
        match rep {
            PyRep::ObjectEx { dict, .. } => dict.len(),
            PyRep::List(rows) => rows.len(),
            PyRep::Dict(d) => d.len(),
            _ => usize::MAX,
        }
    };
    let asks_online = rpc(&ctx, &mut sess_vip, "marketProxy", "GetStationAsks", &[PyRep::Integer(st_id)]).await.unwrap();
    assert_eq!(rowset_len(&asks_online), 1, "Online market hub ask must appear in GetStationAsks");

    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = ops::load_ext_state(snap);
        ext.access_profiles.entry(st_id).or_default().general_settings.insert(svc_acl::SETTING_MARKET_TAX, false);
        ops::save_ext_state(snap, &ext);
    });
    assert_eq!(rpc(&ctx, &mut sess_vip, "marketProxy", "PlaceBuyOrder", &buy_args).await.unwrap_err().code, "StructureServiceAccessDenied");
    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = ops::load_ext_state(snap);
        ext.access_profiles.entry(st_id).or_default().general_settings.remove(&svc_acl::SETTING_MARKET_TAX);
        ops::save_ext_state(snap, &ext);
        snap.module_online_states.insert(880_826_100, false);
    });
    let asks_offline = rpc(&ctx, &mut sess_vip, "marketProxy", "GetStationAsks", &[PyRep::Integer(st_id)]).await.unwrap();
    assert_eq!(rowset_len(&asks_offline), 0, "Offline market hub asks must be filtered from GetStationAsks");
    assert_eq!(rpc(&ctx, &mut sess_vip, "marketProxy", "PlaceBuyOrder", &buy_args).await.unwrap_err().code, "MarketHubOffline");

    // R-MKT-UPWELL-ELIGIBILITY-02: Tatara (35836) is market-capable; HullReinforced is treated as offline in GetStationAsks/GetSystemAsks/PlaceBuyOrder;
    // and external NPC station sellers without structure market ACL can fill ranged Upwell buy orders when online (and not when HullReinforced).
    let npc_sta = station_id.unwrap_or(60_003_760);
    let ext_item_id = 880_826_499;
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(880_826_100, true);
        snap.structures.get_mut(&st_id).unwrap().type_id = 35836; // Tatara (Large Refinery)
        snap.skills.entry(vip_cid).or_default().insert(3447, eve_db::TrainedSkillRecord { type_id: 3447, level: 5, skill_points: 256_000.0 });
        snap.items.insert(ext_item_id, make_item(ext_item_id, 1230, dock_only_cid, npc_sta, InventoryFlag::Hangar, 500, false));
    });
    assert_eq!(rowset_len(&rpc(&ctx, &mut sess_vip, "marketProxy", "GetStationAsks", &[PyRep::Integer(st_id)]).await.unwrap()), 1, "Tatara (35836) with online Market Hub must expose asks");
    assert!(rowset_len(&rpc(&ctx, &mut sess_vip, "marketProxy", "GetSystemAsks", &[]).await.unwrap()) >= 1);
    let ranged_buy = [PyRep::Integer(st_id), PyRep::Integer(1230), PyRep::Real(120.0), PyRep::Integer(500), PyRep::Integer(32767), PyRep::Integer(1), PyRep::Integer(90), PyRep::Bool(false)];
    rpc(&ctx, &mut sess_vip, "marketProxy", "PlaceBuyOrder", &ranged_buy).await.expect("Tatara PlaceBuyOrder");
    ctx.db.with_mut_snapshot(|snap| snap.structures.get_mut(&st_id).unwrap().state = "HullReinforced".into());
    assert_eq!(rowset_len(&rpc(&ctx, &mut sess_vip, "marketProxy", "GetStationAsks", &[PyRep::Integer(st_id)]).await.unwrap()), 0, "HullReinforced structure market must be offline");
    assert_eq!(rpc(&ctx, &mut sess_vip, "marketProxy", "PlaceBuyOrder", &ranged_buy).await.unwrap_err().code, "MarketHubOffline");
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.get_mut(&st_id).unwrap().state = life::STATE_SHIELD_VULNERABLE.into();
        let ch = snap.characters.get_mut(&dock_only_cid).unwrap();
        ch.structure_id = None;
        ch.station_id = Some(npc_sta);
    });
    sess_dock_only.structure_id = None;
    sess_dock_only.station_id = Some(npc_sta);
    let ext_sell = PyRep::keyval([("stationID", PyRep::Integer(npc_sta)), ("typeID", PyRep::Integer(1230)), ("itemID", PyRep::Integer(ext_item_id)), ("price", PyRep::Real(110.0)), ("quantity", PyRep::Integer(200)), ("duration", PyRep::Integer(0))]);
    let dock_only_bal0 = ctx.db.get_character(dock_only_cid).unwrap().balance.0;
    rpc(&ctx, &mut sess_dock_only, "marketProxy", "PlaceMultiSellOrder", &[PyRep::List(vec![ext_sell]), PyRep::Bool(false), PyRep::Integer(0)]).await.expect("External NPC station seller fills ranged Tatara buy order without structure market ACL");
    assert!(ctx.db.get_character(dock_only_cid).unwrap().balance.0 > dock_only_bal0);
}

#[tokio::test]
async fn test_remediated_bf03_bf04_structure_control_immobility_and_direct_structure_profile_save() {
    // parity: feat:structures.board-structure, feat:structures.defense-settings, feat:structures.profiles, rpc:structureControl.TakeControl, rpc:beyonce.CmdWarpToStuff, rpc:structureDocking.Dock, rpc:abyssalMgr.AbyssalEntranceDeployment
    eve_testkit::attest(&[
        "feat:structures.board-structure",
        "feat:structures.defense-settings",
        "feat:structures.profiles",
        "rpc:structureControl.TakeControl",
        "rpc:beyonce.CmdWarpToStuff",
        "rpc:structureDocking.Dock",
        "rpc:abyssalMgr.AbyssalEntranceDeployment",
    ]);

    let ctx = new_ctx();
    let station_id = ctx.sde.stations.values().find(|s| s.solar_system_id == RANCER).map(|s| s.station_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&OWNER) {
            ch.solar_system_id = RANCER;
            ch.station_id = station_id;
            if let Some(sh) = snap.items.get_mut(&ch.active_ship_id) {
                sh.location_id = station_id.unwrap_or(i64::from(RANCER));
            }
        }
    });
    let mut owner_sess = SessionState::new(8270);
    rpc(&ctx, &mut owner_sess, "authentication", "Login", &[PyRep::String("demo".into()), PyRep::String("demo-secret".into())]).await.unwrap();
    rpc(&ctx, &mut owner_sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(OWNER)]).await.unwrap();
    ctx.advance_secs(12);
    rpc(&ctx, &mut owner_sess, "corpRegistry", "AddCorporation", &[PyRep::String("Defense Control Corp".into()), PyRep::String("DCTL".into())]).await.unwrap();
    ctx.advance_virtual_clock(7 * 86_400 + 12);
    let corp_id = owner_sess.corporation_id.expect("corp");

    let st_id = 880_827_001;
    let gunner_cid: i64 = 90_000_071;
    let blocked_cid: i64 = 90_000_072;
    let gunner_ship: i64 = 880_827_011;
    let blocked_ship: i64 = 880_827_012;
    let anchor_pos = [1.5e9, 2.5e9, 3.5e9];

    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(st_id, make_citadel(st_id, corp_id, "Immobile Citadel", anchor_pos));
        for (idx, (cid, shid)) in [(gunner_cid, gunner_ship), (blocked_cid, blocked_ship)].into_iter().enumerate() {
            let mut ch = snap.characters.get(&OWNER).cloned().unwrap();
            ch.character_id = cid;
            ch.corporation_id = 98_827_100 + idx as i64;
            ch.solar_system_id = RANCER;
            ch.structure_id = Some(st_id);
            ch.station_id = None;
            ch.active_ship_id = shid;
            snap.characters.insert(cid, ch);
            snap.items.insert(shid, make_item(shid, 587, cid, st_id, InventoryFlag::Hangar, 1, true));
        }
    });
    life::sync_all_structures(&ctx);

    let dock_grp = kv_i64(&rpc(&ctx, &mut owner_sess, "ownerGroupManager", "Create", &[PyRep::String("Direct Dock".into()), PyRep::String("DDCK".into())]).await.unwrap(), "groupID");
    let def_grp = kv_i64(&rpc(&ctx, &mut owner_sess, "ownerGroupManager", "Create", &[PyRep::String("Direct Def".into()), PyRep::String("DDEF".into())]).await.unwrap(), "groupID");
    rpc(&ctx, &mut owner_sess, "ownerGroupManager", "AddMembers", &[PyRep::Integer(dock_grp), PyRep::List(vec![PyRep::Integer(gunner_cid), PyRep::Integer(blocked_cid)])]).await.unwrap();
    rpc(&ctx, &mut owner_sess, "ownerGroupManager", "AddMembers", &[PyRep::Integer(def_grp), PyRep::List(vec![PyRep::Integer(gunner_cid)])]).await.unwrap();

    // Positive (BF-04): Direct SaveProfileSettings on structure_id persists docking ACLs, defense ACLs, and defense policy flags
    rpc(&ctx, &mut owner_sess, "structureProfiles", "SaveProfileSettings", &[
        PyRep::Integer(st_id),
        PyRep::List(vec![
            PyRep::Tuple(vec![PyRep::Integer(19), PyRep::Integer(0), PyRep::Integer(1)]),
            PyRep::Tuple(vec![PyRep::Integer(19), PyRep::Integer(dock_grp), PyRep::Integer(1)]),
            PyRep::Tuple(vec![PyRep::Integer(19), PyRep::Integer(gunner_cid), PyRep::Integer(1)]),
            PyRep::Tuple(vec![PyRep::Integer(19), PyRep::Integer(blocked_cid), PyRep::Integer(-1)]),
            PyRep::Tuple(vec![PyRep::Integer(17), PyRep::Integer(def_grp), PyRep::Integer(1)]),
            PyRep::Tuple(vec![PyRep::Integer(17), PyRep::Integer(blocked_cid), PyRep::Integer(-1)]),
            PyRep::Tuple(vec![PyRep::Integer(17), PyRep::Tuple(vec![PyRep::Bool(true), PyRep::Bool(true), PyRep::Bool(false), PyRep::Real(-2.5), PyRep::Bool(true)]), PyRep::Integer(0)]),
        ]),
    ]).await.unwrap();

    ctx.db.with_snapshot(|snap| {
        let ext = ops::load_ext_state(snap);
        let prof = ext.access_profiles.get(&st_id).expect("structure profile");
        assert!(prof.access_group_ids.contains(&dock_grp));
        assert!(prof.defense_group_ids.contains(&def_grp));
        assert!(prof.defense_blocked_characters.contains(&blocked_cid));
        let ds = eve_server::services::structure_acl_defense_rules::load_defense_settings(snap, st_id);
        assert!(ds.auto_defense_enabled);
        assert!(ds.auto_target_attackers);
        assert!((ds.standing_threshold - (-2.5)).abs() < 1e-6);
        assert!(ds.target_blocked_acl);
    });

    let mut gunner_sess = SessionState::new(8271);
    gunner_sess.user_id = Some(8271);
    gunner_sess.character_id = Some(gunner_cid);
    gunner_sess.corporation_id = Some(98_827_100);
    gunner_sess.solar_system_id = Some(RANCER);
    gunner_sess.structure_id = Some(st_id);
    gunner_sess.ship_id = Some(gunner_ship);

    let mut blocked_sess = SessionState::new(8272);
    blocked_sess.user_id = Some(8272);
    blocked_sess.character_id = Some(blocked_cid);
    blocked_sess.corporation_id = Some(98_827_101);
    blocked_sess.solar_system_id = Some(RANCER);
    blocked_sess.structure_id = Some(st_id);
    blocked_sess.ship_id = Some(blocked_ship);

    // Negative (BF-04): Blocked pilot cannot TakeControl; Positive: defenseGroupIDs pilot CAN TakeControl
    assert_eq!(rpc(&ctx, &mut gunner_sess, "structureSettings", "CharacterHasSetting", &[PyRep::Integer(st_id), PyRep::Integer(17)]).await.unwrap(), PyRep::Bool(true));
    assert_eq!(rpc(&ctx, &mut blocked_sess, "structureSettings", "CharacterHasSetting", &[PyRep::Integer(st_id), PyRep::Integer(17)]).await.unwrap(), PyRep::Bool(false));
    assert!(rpc(&ctx, &mut blocked_sess, "structureControl", "TakeControl", &[PyRep::Integer(st_id)]).await.is_err());
    rpc(&ctx, &mut gunner_sess, "structureControl", "TakeControl", &[PyRep::Integer(st_id)]).await.unwrap();
    assert_eq!(gunner_sess.ship_id, Some(st_id));

    // R-STRUCT-PENDING-PROFILE-SYNC-01: Delayed ChangeProfiles revokes unauthorized controller & syncs defense_settings when due
    let p_open = rpc(&ctx, &mut owner_sess, "structureProfiles", "CreateProfile", &[PyRep::String("P1".into()), PyRep::String("".into())]).await.unwrap().as_i64().unwrap();
    let p_lock = rpc(&ctx, &mut owner_sess, "structureProfiles", "CreateProfile", &[PyRep::String("P2".into()), PyRep::String("".into())]).await.unwrap().as_i64().unwrap();
    rpc(&ctx, &mut owner_sess, "structureProfiles", "SaveProfileSettings", &[PyRep::Integer(p_open), PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(19), PyRep::Integer(dock_grp), PyRep::Integer(1)]), PyRep::Tuple(vec![PyRep::Integer(17), PyRep::Integer(def_grp), PyRep::Integer(1)]), PyRep::Tuple(vec![PyRep::Integer(17), PyRep::Bool(true), PyRep::Integer(0)])])]).await.unwrap();
    rpc(&ctx, &mut owner_sess, "structureProfiles", "SaveProfileSettings", &[PyRep::Integer(p_lock), PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(19), PyRep::Integer(dock_grp), PyRep::Integer(1)]), PyRep::Tuple(vec![PyRep::Integer(17), PyRep::Bool(false), PyRep::Integer(0)])])]).await.unwrap();
    rpc(&ctx, &mut owner_sess, "structureProfiles", "ChangeProfiles", &[PyRep::List(vec![PyRep::Integer(st_id)]), PyRep::Integer(p_open)]).await.unwrap();
    rpc(&ctx, &mut owner_sess, "structureProfiles", "ChangeProfiles", &[PyRep::List(vec![PyRep::Integer(st_id)]), PyRep::Integer(p_lock)]).await.unwrap();
    ctx.advance_virtual_clock(305);
    assert_eq!(rpc(&ctx, &mut gunner_sess, "structureSettings", "CharacterHasSetting", &[PyRep::Integer(st_id), PyRep::Integer(17)]).await.unwrap(), PyRep::Bool(false));
    let _ = ctx.systems.get_or_spawn(RANCER).step_manual(1.0).await.unwrap();
    ctx.sync_session(&mut gunner_sess);
    assert_eq!(gunner_sess.ship_id, Some(gunner_ship), "Due pending profile transition on tick must release unauthorized controller");
    assert!(!ctx.db.with_snapshot(|s| eve_server::services::structure_acl_defense_rules::load_defense_settings(s, st_id).auto_defense_enabled));
    rpc(&ctx, &mut owner_sess, "structureProfiles", "SaveProfileSettings", &[PyRep::Integer(p_lock), PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(19), PyRep::Integer(dock_grp), PyRep::Integer(1)]), PyRep::Tuple(vec![PyRep::Integer(17), PyRep::Integer(def_grp), PyRep::Integer(1)])])]).await.unwrap();
    rpc(&ctx, &mut gunner_sess, "structureControl", "TakeControl", &[PyRep::Integer(st_id)]).await.unwrap();
    assert_eq!(gunner_sess.ship_id, Some(st_id));

    // Bind beyonce to RANCER and verify all navigation, warp, dock, and filament RPCs reject while controlling st_id (BF-03)
    let PyRep::Tuple(outer) = rpc(&ctx, &mut gunner_sess, "beyonce", "MachoBindObject", &[PyRep::Integer(i64::from(RANCER)), PyRep::None]).await.unwrap() else {
        panic!("expected Tuple from MachoBindObject");
    };
    let gunner_bind_id = outer[0].as_i64().map(|v| v as u64);

    assert_eq!(ctx.dispatch_rpc(&mut gunner_sess, "beyonce", gunner_bind_id, "CmdGotoPoint", &[PyRep::Real(0.0), PyRep::Real(0.0), PyRep::Real(0.0)], &HashMap::new(), None).await.unwrap_err().code, "StructureCannotNavigate");
    assert_eq!(ctx.dispatch_rpc(&mut gunner_sess, "beyonce", gunner_bind_id, "CmdGotoDirection", &[PyRep::Real(1.0), PyRep::Real(0.0), PyRep::Real(0.0)], &HashMap::new(), None).await.unwrap_err().code, "StructureCannotNavigate");
    assert_eq!(ctx.dispatch_rpc(&mut gunner_sess, "beyonce", gunner_bind_id, "CmdSetSpeedFraction", &[PyRep::Real(1.0)], &HashMap::new(), None).await.unwrap_err().code, "StructureCannotNavigate");
    assert_eq!(ctx.dispatch_rpc(&mut gunner_sess, "beyonce", gunner_bind_id, "CmdOrbit", &[PyRep::Integer(st_id), PyRep::Real(5_000.0)], &HashMap::new(), None).await.unwrap_err().code, "StructureCannotNavigate");
    assert_eq!(ctx.dispatch_rpc(&mut gunner_sess, "beyonce", gunner_bind_id, "CmdAlignTo", &[PyRep::Integer(station_id.unwrap_or(60_003_760) as i64)], &HashMap::new(), None).await.unwrap_err().code, "StructureCannotNavigate");
    assert_eq!(ctx.dispatch_rpc(&mut gunner_sess, "beyonce", gunner_bind_id, "CmdWarpToStuff", &[PyRep::String("item".into()), PyRep::Integer(station_id.unwrap_or(60_003_760) as i64)], &HashMap::new(), None).await.unwrap_err().code, "StructureCannotNavigate");
    assert_eq!(rpc(&ctx, &mut gunner_sess, "structureDocking", "Dock", &[PyRep::Integer(st_id)]).await.unwrap_err().code, "StructureCannotNavigate");
    assert_eq!(rpc(&ctx, &mut gunner_sess, "abyssalMgr", "AbyssalEntranceDeployment", &[PyRep::Integer(47_765)]).await.unwrap_err().code, "StructureCannotNavigate");

    // Verify structure ball in Ballpark never moved from anchor_pos
    let (_, st_ball) = ctx.systems.get_or_spawn(RANCER).get_ball_snapshot(st_id).await.unwrap().expect("structure ball");
    assert_eq!(st_ball.position, DVec3::from_array(anchor_pos));

    // Positive control: a normal piloted ship in space CAN issue CmdGotoPoint
    let free_ship: i64 = 880_827_099;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(free_ship, make_item(free_ship, 587, OWNER, i64::from(RANCER), InventoryFlag::Hangar, 1, true));
        if let Some(ch) = snap.characters.get_mut(&OWNER) {
            ch.active_ship_id = free_ship;
            ch.station_id = None;
            ch.structure_id = None;
        }
    });
    ctx.systems.get_or_spawn(RANCER).spawn_ball(Ball {
        id: free_ship,
        type_id: 587,
        position: DVec3::new(1.5e9 + 10_000.0, 2.5e9, 3.5e9),
        velocity: DVec3::ZERO,
        max_velocity: 350.0,
        inertia: 3.0,
        mass: 1_000_000.0,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    }).await.unwrap();
    let mut free_sess = SessionState::new(8273);
    free_sess.user_id = Some(8273);
    free_sess.character_id = Some(OWNER);
    free_sess.corporation_id = Some(corp_id);
    free_sess.solar_system_id = Some(RANCER);
    free_sess.ship_id = Some(free_ship);
    let PyRep::Tuple(free_outer) = rpc(&ctx, &mut free_sess, "beyonce", "MachoBindObject", &[PyRep::Integer(i64::from(RANCER)), PyRep::None]).await.unwrap() else {
        panic!("expected Tuple from MachoBindObject");
    };
    let free_bind_id = free_outer[0].as_i64().map(|v| v as u64);
    ctx.dispatch_rpc(&mut free_sess, "beyonce", free_bind_id, "CmdGotoPoint", &[PyRep::Real(0.0), PyRep::Real(0.0), PyRep::Real(0.0)], &HashMap::new(), None).await.expect("normal ship CmdGotoPoint succeeds");
}


}

#[allow(dead_code, unused_imports)]
pub(crate) mod upwell_structures_quantum_cores {
    use super::*;

//  Remediation integration tests for `BF-01` (Upwell `Anchor` cross-corp `profileID` rejection,
//  default corp profile auto-attachment, and `DeleteProfile` owner-corp scoping) and `BF-02`
//  (per-service `(groupID, value)` ACL mappings preserving Docking ACLs and enforcing per-group
//  service access & tax rates across Market, Reprocessing, Clone Bay, Industry, and Jump Bridge).

use eve_db::{
    AllianceRecord, CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents,
    TrainedSkillRecord, UpwellStructureRecord,
};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use eve_server::services::station_structure::structure_lifecycle as life;
use eve_server::services::structure_profile_services as svc_acl;
use glam::DVec3;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

const OWNER: i64 = 90_000_001;
const CORP_B_CEO: i64 = 90_000_061;
const PILOT_DOCK_ONLY: i64 = 90_000_062;
const PILOT_VIP: i64 = 90_000_063;
const PILOT_STD: i64 = 90_000_064;
const PILOT_BOTH: i64 = 90_000_065;
const RANCER: u32 = 30_002_718;
const SOV_NULL_A: u32 = 30_004_759; // 1DQ1-A
const SOV_NULL_B: u32 = 30_004_760; // T5ZI-S
const RIFTER_BP: i64 = 691;

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

async fn setup_corp_ceo(
    ctx: &ServerContext,
    sess_id: u64,
    char_id: i64,
    corp_id: i64,
    name: &str,
    ticker: &str,
    sys_id: u32,
    ship_id: i64,
    pos: DVec3,
) -> SessionState {
    let uid = char_id as u64;
    ctx.db.with_mut_snapshot(|snap| {
        ensure_account(snap, uid);
        let mut ch = snap.characters.get(&OWNER).cloned().unwrap();
        ch.character_id = char_id;
        ch.user_id = uid;
        ch.corporation_id = corp_id;
        ch.solar_system_id = sys_id;
        ch.station_id = None;
        ch.structure_id = None;
        ch.active_ship_id = ship_id;
        ch.balance = IskCents::from_isk(500_000_000);
        snap.characters.insert(char_id, ch);

        let mut c = CorporationRecord::default();
        c.corporation_id = corp_id;
        c.name = name.into();
        c.ticker = ticker.into();
        c.ceo_id = char_id;
        c.master_wallet = IskCents::from_isk(100_000_000);
        c.members.insert(char_id);
        c.member_roles.insert(char_id, u64::MAX);
        snap.corporations.insert(corp_id, c);

        snap.items.insert(ship_id, InventoryItemRecord {
            item_id: ship_id, type_id: 648, owner_id: char_id, location_id: i64::from(sys_id),
            flag: InventoryFlag::None, quantity: 1, singleton: true, version: 1, ..Default::default()
        });
    });
    ctx.systems.get_or_spawn(sys_id).spawn_ball(Ball {
        id: ship_id, type_id: 648, position: pos, velocity: DVec3::ZERO,
        max_velocity: 200.0, inertia: 1.0, mass: 1_500_000.0, radius: 120.0,
        speed_fraction: 0.0, mode: MovementMode::Stop,
    }).await.expect("spawn_ball");
    let mut sess = SessionState::new(sess_id);
    sess.user_id = Some(uid);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(corp_id);
    sess.corp_role = u64::MAX;
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(ship_id);
    sess
}

fn seed_docked_pilot(
    ctx: &ServerContext,
    sess_id: u64,
    char_id: i64,
    corp_id: i64,
    alliance_id: Option<i64>,
    sys_id: u32,
    st_id: i64,
    ship_id: i64,
    ship_type_id: u32,
) -> SessionState {
    let uid = char_id as u64;
    let reg_id = ctx.sde.solar_systems.get(&sys_id).map(|s| s.region_id).unwrap_or(10_000_002);
    ctx.db.with_mut_snapshot(|snap| {
        ensure_account(snap, uid);
        let mut ch = snap.characters.get(&OWNER).cloned().unwrap();
        ch.character_id = char_id;
        ch.user_id = uid;
        ch.corporation_id = corp_id;
        ch.alliance_id = alliance_id;
        ch.solar_system_id = sys_id;
        ch.region_id = reg_id;
        ch.station_id = None;
        ch.structure_id = Some(st_id);
        ch.active_ship_id = ship_id;
        ch.clone_grade = 2; // Omega
        ch.balance = IskCents::from_isk(100_000_000);
        snap.characters.insert(char_id, ch);

        if corp_id >= 98_000_000 && !snap.corporations.contains_key(&corp_id) {
            let mut c = CorporationRecord::default();
            c.corporation_id = corp_id;
            c.name = format!("Pilot Corp {corp_id}");
            c.ticker = "PCRP".into();
            c.ceo_id = char_id;
            c.alliance_id = alliance_id;
            c.members.insert(char_id);
            snap.corporations.insert(corp_id, c);
        } else if let Some(c) = snap.corporations.get_mut(&corp_id) {
            c.members.insert(char_id);
        }
        if let Some(aid) = alliance_id {
            snap.alliances
                .entry(aid)
                .or_insert_with(|| AllianceRecord {
                    alliance_id: aid,
                    name: format!("Alliance {aid}"),
                    short_name: "ALLY".into(),
                    executor_corp_id: corp_id,
                    member_corp_ids: HashSet::new(),
                })
                .member_corp_ids
                .insert(corp_id);
        }

        let sk = snap.skills.entry(char_id).or_default();
        sk.insert(24242, TrainedSkillRecord { type_id: 24242, level: 5, skill_points: 256_000.0 });
        sk.insert(3380, TrainedSkillRecord { type_id: 3380, level: 5, skill_points: 256_000.0 });
        sk.insert(3387, TrainedSkillRecord { type_id: 3387, level: 5, skill_points: 256_000.0 });

        snap.items.insert(ship_id, InventoryItemRecord {
            item_id: ship_id, type_id: ship_type_id, owner_id: char_id, location_id: st_id,
            flag: InventoryFlag::Hangar, quantity: 1, singleton: true, version: 1, ..Default::default()
        });
    });
    let mut s = SessionState::new(sess_id);
    s.user_id = Some(uid);
    s.character_id = Some(char_id);
    s.corporation_id = Some(corp_id);
    s.alliance_id = alliance_id;
    s.solar_system_id = Some(sys_id);
    s.structure_id = Some(st_id);
    s.ship_id = Some(ship_id);
    s
}

#[tokio::test]
async fn test_bf01_anchor_cross_corp_profile_rejected_default_attached_and_delete_scoped() {
    // parity: feat:structures.anchor-restrictions, feat:structures.profiles, rpc:structureDeployment.Anchor
    // Exercises cross-corp profileID rejection, default profile attachment, and DeleteProfile scoping.
    eve_testkit::attest(&[
        "feat:structures.anchor-restrictions",
        "feat:structures.profiles",
        "rpc:structureDeployment.Anchor",
    ]);

    let ctx = new_ctx();
    let corp_a = 98_901_001;
    let corp_b = 98_901_002;
    let pos_a = DVec3::new(5.0e10, 5.0e10, 5.0e10);
    let pos_b = DVec3::new(-5.0e10, 5.0e10, 5.0e10);
    let ship_a = 890_001_001;
    let ship_b = 890_001_002;
    let mut sess_a = setup_corp_ceo(&ctx, 9101, OWNER, corp_a, "Alpha Anchor Corp", "AANC", RANCER, ship_a, pos_a).await;
    let mut sess_b = setup_corp_ceo(&ctx, 9102, CORP_B_CEO, corp_b, "Bravo Victim Corp", "BVIC", RANCER, ship_b, pos_b).await;

    let prof_a_def = rpc(&ctx, &mut sess_a, "structureProfiles", "CreateProfile", &[PyRep::String("Alpha Default".into()), PyRep::String("Default".into())]).await.unwrap().as_i64().unwrap();
    rpc(&ctx, &mut sess_a, "structureProfiles", "SetDefaultProfile", &[PyRep::Integer(prof_a_def)]).await.unwrap();
    let prof_b = rpc(&ctx, &mut sess_b, "structureProfiles", "CreateProfile", &[PyRep::String("Bravo Private".into()), PyRep::String("Private".into())]).await.unwrap().as_i64().unwrap();

    let astrahus_cargo_id = 890_001_010;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(astrahus_cargo_id, InventoryItemRecord {
            item_id: astrahus_cargo_id, type_id: 35832, owner_id: OWNER, location_id: ship_a,
            flag: InventoryFlag::Cargo, quantity: 1, singleton: false, version: 1, ..Default::default()
        });
    });

    // 1. Cross-corp or missing profileID on Anchor MUST fail with StructureOwnershipViolation BEFORE consuming cargo or creating structure!
    let anchor_pos = PyRep::Tuple(vec![PyRep::Real(pos_a.x + 60_000.0), PyRep::Real(pos_a.y), PyRep::Real(pos_a.z)]);
    let rw = PyRep::Tuple(vec![PyRep::Integer(18), PyRep::Integer(2)]);
    for bad_pid in [prof_b, 70_999_999_i64] {
        let bad_err = rpc(
            &ctx,
            &mut sess_a,
            "structureDeployment",
            "Anchor",
            &[PyRep::Integer(astrahus_cargo_id), anchor_pos.clone(), PyRep::Integer(bad_pid), rw.clone(), PyRep::String("Hijacked Citadel".into())],
        )
        .await
        .expect_err("Cross-corp or missing profileID on Anchor must be rejected");
        assert_eq!(bad_err.code, "StructureOwnershipViolation");
        assert!(ctx.db.get_item(astrahus_cargo_id).is_some(), "Cargo item must remain intact on failed Anchor");
        assert!(ctx.db.with_snapshot(|s| s.structures.is_empty()), "No structure may be created on failed Anchor");
    }

    // 2. Omitting profileID automatically attaches corp_a's default profile (`prof_a_def`).
    let anchored = rpc(
        &ctx,
        &mut sess_a,
        "structureDeployment",
        "Anchor",
        &[PyRep::Integer(astrahus_cargo_id), anchor_pos, PyRep::None, rw, PyRep::String("Default Profile Citadel".into())],
    )
    .await
    .expect("Anchor with omitted profileID");
    let st_id = kv_i64(&anchored, "structureID");
    assert!(st_id > 1_000_000_000_000);
    assert!(ctx.db.get_item(astrahus_cargo_id).is_none(), "Cargo item consumed after valid Anchor");
    let attached_pid = ctx.db.with_snapshot(|snap| {
        eve_server::services::station_structure::structure_upwell_ops::load_ext_state(snap)
            .structure_profile_id
            .get(&st_id)
            .copied()
    });
    assert_eq!(attached_pid, Some(prof_a_def), "Default corp profile must be auto-attached when profileID is None");

    // 3. DeleteProfile refuses deleting prof_a_def while corp_a's structure uses it, while scoping ProfileInUse to owner_corp.
    assert_eq!(
        rpc(&ctx, &mut sess_a, "structureProfiles", "DeleteProfile", &[PyRep::Integer(prof_a_def)]).await.unwrap_err().code,
        "ProfileInUse"
    );

    // Even if a stale cross-corp mapping pointed at prof_b, Corp B is never DoS'd from deleting its own unused profile.
    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = eve_server::services::station_structure::structure_upwell_ops::load_ext_state(snap);
        ext.structure_profile_id.insert(1_000_999_999_001, prof_b);
        eve_server::services::station_structure::structure_upwell_ops::save_ext_state(snap, &ext);
    });
    assert_eq!(
        rpc(&ctx, &mut sess_b, "structureProfiles", "DeleteProfile", &[PyRep::Integer(prof_b)]).await.unwrap(),
        PyRep::Bool(true)
    );
}

#[tokio::test]
async fn test_bf02_per_service_acl_bindings_preserve_docking_and_enforce_group_taxes() {
    // parity: feat:structures.profiles, feat:structures.docking-acl, feat:structures.repairshop-service-quotes-and-restoration, rpc:structureSettings.CharacterHasService, rpc:structureSettings.CharacterCheckService
    eve_testkit::attest(&[
        "feat:structures.profiles",
        "feat:structures.docking-acl",
        "feat:structures.repairshop-service-quotes-and-restoration",
        "rpc:structureSettings.CharacterHasService",
        "rpc:structureSettings.CharacterCheckService",
    ]);

    let ctx = new_ctx();
    let owner_corp = 98_902_001;
    let mut owner_sess = setup_corp_ceo(&ctx, 9201, OWNER, owner_corp, "Service ACL Corp", "SACL", SOV_NULL_A, 890_002_001, DVec3::new(1.0e10, 0.0, 0.0)).await;

    let hub_id = 1_000_890_002_100;
    let jb_a_id = 1_000_890_002_201;
    let jb_b_id = 1_000_890_002_202;
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(hub_id, UpwellStructureRecord {
            structure_id: hub_id, type_id: 35833, owner_corp_id: owner_corp, solar_system_id: SOV_NULL_A,
            name: "ACL Service Hub".into(), position: [1.0e10, 0.0, 0.0], state: life::STATE_SHIELD_VULNERABLE.into(),
            quantum_core_installed: true, fuel_blocks: 5_000, shield_hp: 5_000_000.0, armor_hp: 5_000_000.0,
            hull_hp: 5_000_000.0, reinforced_until_unix: 0, last_damage_tick: 0, last_damage_unix: 0, self_destruct_at_unix: None,
        });
        for (idx, (flag, mod_tid)) in [
            (InventoryFlag::ServiceSlot0, 35892_u32), // Standup Market Hub I
            (InventoryFlag::ServiceSlot1, 35899_u32), // Standup Reprocessing Facility I
            (InventoryFlag::ServiceSlot2, 35894_u32), // Standup Cloning Center I
            (InventoryFlag::ServiceSlot3, 35878_u32), // Standup Manufacturing Plant I
        ].into_iter().enumerate() {
            let mid = 890_002_110 + idx as i64;
            snap.items.insert(mid, InventoryItemRecord {
                item_id: mid, type_id: mod_tid, owner_id: owner_corp, location_id: hub_id,
                flag, quantity: 1, singleton: true, version: 1, ..Default::default()
            });
            snap.module_online_states.insert(mid, true);
        }
        for (jid, sys) in [(jb_a_id, SOV_NULL_A), (jb_b_id, SOV_NULL_B)] {
            snap.structures.insert(jid, UpwellStructureRecord {
                structure_id: jid, type_id: 35841, owner_corp_id: owner_corp, solar_system_id: sys,
                name: format!("JB {jid}"), position: [2.0e10, 0.0, 0.0], state: life::STATE_SHIELD_VULNERABLE.into(),
                quantum_core_installed: true, fuel_blocks: 2_000, shield_hp: 1_000_000.0, armor_hp: 1_000_000.0,
                hull_hp: 1_000_000.0, reinforced_until_unix: 0, last_damage_tick: 0, last_damage_unix: 0, self_destruct_at_unix: None,
            });
        }
        snap.items.insert(890_002_120, InventoryItemRecord {
            item_id: 890_002_120, type_id: 16273, owner_id: owner_corp, location_id: jb_a_id,
            flag: InventoryFlag::SpecializedFuelBay, quantity: 50_000, singleton: false, version: 1, ..Default::default()
        });
        let mut ext = eve_server::services::station_structure::structure_upwell_ops::load_ext_state(snap);
        ext.jump_bridge_links.insert(jb_a_id, jb_b_id);
        ext.jump_bridge_links.insert(jb_b_id, jb_a_id);
        eve_server::services::station_structure::structure_upwell_ops::save_ext_state(snap, &ext);
    });

    let grp_dock = kv_i64(&rpc(&ctx, &mut owner_sess, "ownerGroupManager", "Create", &[PyRep::String("Docking ACL".into()), PyRep::String("Can dock".into())]).await.unwrap(), "groupID");
    let grp_vip = kv_i64(&rpc(&ctx, &mut owner_sess, "ownerGroupManager", "Create", &[PyRep::String("VIP Service".into()), PyRep::String("Low tax".into())]).await.unwrap(), "groupID");
    let grp_std = kv_i64(&rpc(&ctx, &mut owner_sess, "ownerGroupManager", "Create", &[PyRep::String("Std Service".into()), PyRep::String("Standard tax".into())]).await.unwrap(), "groupID");

    let mut sess_dock_only = seed_docked_pilot(&ctx, 9202, PILOT_DOCK_ONLY, 98_902_002, None, SOV_NULL_A, hub_id, 890_002_301, 670);
    let mut sess_vip = seed_docked_pilot(&ctx, 9203, PILOT_VIP, 98_902_003, None, SOV_NULL_A, hub_id, 890_002_302, 670);
    let mut sess_std = seed_docked_pilot(&ctx, 9204, PILOT_STD, 98_902_004, None, SOV_NULL_A, hub_id, 890_002_303, 670);
    let mut sess_both = seed_docked_pilot(&ctx, 9205, PILOT_BOTH, 98_902_005, None, SOV_NULL_A, hub_id, 890_002_304, 670);

    rpc(&ctx, &mut owner_sess, "ownerGroupManager", "AddMembers", &[
        PyRep::Integer(grp_dock),
        PyRep::List(vec![PyRep::Integer(PILOT_DOCK_ONLY), PyRep::Integer(PILOT_VIP), PyRep::Integer(PILOT_STD), PyRep::Integer(PILOT_BOTH)]),
    ]).await.unwrap();
    rpc(&ctx, &mut owner_sess, "ownerGroupManager", "AddMembers", &[PyRep::Integer(grp_vip), PyRep::List(vec![PyRep::Integer(PILOT_VIP), PyRep::Integer(PILOT_BOTH)])]).await.unwrap();
    rpc(&ctx, &mut owner_sess, "ownerGroupManager", "AddMembers", &[PyRep::Integer(grp_std), PyRep::List(vec![PyRep::Integer(PILOT_STD), PyRep::Integer(PILOT_BOTH)])]).await.unwrap();

    // Create Profile 1: configure Docking ACL FIRST, then save per-service ACL bindings in a separate SaveProfileSettings call!
    let prof_1 = rpc(&ctx, &mut owner_sess, "structureProfiles", "CreateProfile", &[PyRep::String("Tiered Services".into()), PyRep::String("Tiered".into())]).await.unwrap().as_i64().unwrap();
    rpc(&ctx, &mut owner_sess, "structureProfiles", "SaveProfileSettings", &[
        PyRep::Integer(prof_1),
        PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(svc_acl::SETTING_HOUSING_CAN_DOCK), PyRep::Integer(grp_dock), PyRep::Integer(1)])]),
    ]).await.unwrap();

    // Save per-service ACLs — MUST NOT overwrite grp_dock!
    rpc(&ctx, &mut owner_sess, "structureProfiles", "SaveProfileSettings", &[
        PyRep::Integer(prof_1),
        PyRep::List(vec![
            PyRep::Tuple(vec![PyRep::Integer(svc_acl::SETTING_MARKET_TAX), PyRep::Integer(grp_vip), PyRep::Real(0.01)]),
            PyRep::Tuple(vec![PyRep::Integer(svc_acl::SETTING_MARKET_TAX), PyRep::Integer(grp_std), PyRep::Real(0.05)]),
            PyRep::Tuple(vec![PyRep::Integer(svc_acl::SETTING_REPROCESSING_TAX), PyRep::Integer(grp_vip), PyRep::Real(0.02)]),
            PyRep::Tuple(vec![PyRep::Integer(svc_acl::SETTING_REPROCESSING_TAX), PyRep::Integer(grp_std), PyRep::Real(0.08)]),
            PyRep::Tuple(vec![PyRep::Integer(svc_acl::SETTING_CLONINGBAY_TAX), PyRep::Integer(grp_vip), PyRep::Real(100_000.0)]),
            PyRep::Tuple(vec![PyRep::Integer(svc_acl::SETTING_CLONINGBAY_TAX), PyRep::Integer(grp_std), PyRep::Real(600_000.0)]),
            PyRep::Tuple(vec![PyRep::Integer(svc_acl::SETTING_MANUFACTURING_TAX), PyRep::Integer(grp_vip), PyRep::Real(0.01)]),
            PyRep::Tuple(vec![PyRep::Integer(svc_acl::SETTING_MANUFACTURING_TAX), PyRep::Integer(grp_std), PyRep::Real(0.06)]),
            PyRep::Tuple(vec![PyRep::Integer(svc_acl::SERVICE_REPAIR), PyRep::Integer(grp_vip), PyRep::Real(0.02)]),
            PyRep::Tuple(vec![PyRep::Integer(svc_acl::SERVICE_REPAIR), PyRep::Integer(grp_std), PyRep::Real(0.10)]),
            PyRep::Tuple(vec![PyRep::Integer(svc_acl::SETTING_JUMP_BRIDGE_ACTIVATION), PyRep::Integer(grp_vip), PyRep::Real(0.0)]),
        ]),
    ]).await.unwrap();

    // Verify GetProfileSettings returns real groupID bindings for both Docking (19) and per-service settings!
    let saved_settings = rpc(&ctx, &mut owner_sess, "structureProfiles", "GetProfileSettings", &[PyRep::Integer(prof_1)]).await.unwrap();
    let PyRep::Dict(setting_entries) = saved_settings else { panic!("expected Dict from GetProfileSettings") };
    let groups_for_setting = |sid: i64| -> Vec<(i64, f64)> {
        setting_entries
            .iter()
            .find(|(k, _)| k.as_i64() == Some(sid))
            .and_then(|(_, v)| match v {
                PyRep::List(rows) => Some(
                    rows.iter()
                        .map(|r| {
                            let val = r.dict_get("value").and_then(|x| x.as_f64().or_else(|| x.as_i64().map(|i| i as f64))).unwrap_or(-1.0);
                            (kv_i64(r, "groupID"), val)
                        })
                        .collect(),
                ),
                _ => None,
            })
            .unwrap_or_default()
    };
    assert_eq!(groups_for_setting(svc_acl::SETTING_HOUSING_CAN_DOCK), vec![(grp_dock, 1.0)], "Docking ACL must be preserved");
    assert_eq!(groups_for_setting(svc_acl::SETTING_MARKET_TAX), vec![(grp_vip, 0.01), (grp_std, 0.05)]);
    assert_eq!(groups_for_setting(svc_acl::SETTING_JUMP_BRIDGE_ACTIVATION), vec![(grp_vip, 0.0)]);

    rpc(&ctx, &mut owner_sess, "structureProfiles", "ChangeProfiles", &[PyRep::List(vec![PyRep::Integer(hub_id), PyRep::Integer(jb_a_id)]), PyRep::Integer(prof_1)]).await.unwrap();

    // 1. PILOT_DOCK_ONLY retains Docking access (`CharacterHasSetting(19) == true`), yet is denied every restricted service!
    assert_eq!(
        rpc(&ctx, &mut sess_dock_only, "structureSettings", "CharacterHasSetting", &[PyRep::Integer(hub_id), PyRep::Integer(svc_acl::SETTING_HOUSING_CAN_DOCK)]).await.unwrap(),
        PyRep::Bool(true)
    );
    for svc_id in [svc_acl::SERVICE_MARKET, svc_acl::SERVICE_REPROCESSING, svc_acl::SERVICE_JUMP_CLONE, svc_acl::SERVICE_MANUFACTURING_BASIC] {
        assert_eq!(rpc(&ctx, &mut sess_dock_only, "structureSettings", "CharacterHasService", &[PyRep::Integer(hub_id), PyRep::Integer(svc_id)]).await.unwrap(), PyRep::Bool(false));
        assert_eq!(rpc(&ctx, &mut sess_dock_only, "structureSettings", "CharacterCheckService", &[PyRep::Integer(hub_id), PyRep::Integer(svc_id)]).await.unwrap_err().code, "StructureServiceAccessDenied");
    }

    // Seed ore, Rifter BPO (691), and minerals (34..=40) for all 4 pilots at hub_id.
    ctx.db.with_mut_snapshot(|snap| {
        for (idx, pid) in [PILOT_DOCK_ONLY, PILOT_VIP, PILOT_STD, PILOT_BOTH].into_iter().enumerate() {
            let ore_id = 890_002_400 + idx as i64;
            let bpo_id = 890_002_410 + idx as i64;
            snap.items.insert(ore_id, InventoryItemRecord {
                item_id: ore_id, type_id: 1230, owner_id: pid, location_id: hub_id, flag: InventoryFlag::Hangar, quantity: 10_000, singleton: false, version: 1, ..Default::default()
            });
            snap.items.insert(bpo_id, InventoryItemRecord {
                item_id: bpo_id, type_id: RIFTER_BP as u32, owner_id: pid, location_id: hub_id, flag: InventoryFlag::Hangar, quantity: 1, singleton: true, version: 1, ..Default::default()
            });
            for min_tid in 34..=40_u32 {
                let mid = 890_002_500 + (idx as i64) * 20 + i64::from(min_tid - 34);
                snap.items.insert(mid, InventoryItemRecord {
                    item_id: mid, type_id: min_tid, owner_id: pid, location_id: hub_id, flag: InventoryFlag::Hangar, quantity: 1_000_000, singleton: false, version: 1, ..Default::default()
                });
            }
        }
    });

    // PILOT_DOCK_ONLY is denied Market, Reprocessing, Clone Bay, and Manufacturing RPCs despite being docked!
    let buy_args = [
        PyRep::Integer(hub_id), PyRep::Integer(34), PyRep::Real(100.0), PyRep::Integer(10_000),
        PyRep::Integer(-1), PyRep::Integer(1), PyRep::Integer(90), PyRep::Bool(false),
    ];
    let mfg_args = [PyRep::keyval([
        ("blueprintTypeID", PyRep::Integer(RIFTER_BP)),
        ("facilityID", PyRep::Integer(hub_id)),
        ("activityID", PyRep::Integer(1)),
        ("runs", PyRep::Integer(1)),
    ])];
    assert_eq!(rpc(&ctx, &mut sess_dock_only, "marketProxy", "PlaceBuyOrder", &buy_args).await.unwrap_err().code, "StructureServiceAccessDenied");
    assert_eq!(rpc(&ctx, &mut sess_dock_only, "reprocessingSvc", "Reprocess", &[PyRep::Integer(890_002_400)]).await.unwrap_err().code, "AccessDenied");
    assert_eq!(rpc(&ctx, &mut sess_dock_only, "jumpCloneSvc", "InstallCloneInStructure", &[PyRep::Integer(hub_id)]).await.unwrap_err().code, "StructureAccessDenied");
    assert_eq!(rpc(&ctx, &mut sess_dock_only, "industryManager", "InstallJob", &mfg_args).await.unwrap_err().code, "StructureServiceAccessDenied");
    assert_eq!(rpc(&ctx, &mut sess_dock_only, "repairSvc", "GetRepairQuotes", &[PyRep::List(vec![PyRep::Integer(890_002_301)])]).await.unwrap_err().code, "StructureRepairAccessDenied");

    // 2. Market per-ACL-group broker fees (+0.5% SCC floor): VIP = 1.0% + 0.5% = 15k ISK, STD = 5.0% + 0.5% = 55k ISK, BOTH = min = 15k ISK on 1,000,000 ISK buy escrow!
    let bal_vip_0 = ctx.db.get_character(PILOT_VIP).unwrap().balance.0;
    let bal_std_0 = ctx.db.get_character(PILOT_STD).unwrap().balance.0;
    let bal_both_0 = ctx.db.get_character(PILOT_BOTH).unwrap().balance.0;
    rpc(&ctx, &mut sess_vip, "marketProxy", "PlaceBuyOrder", &buy_args).await.expect("VIP PlaceBuyOrder");
    rpc(&ctx, &mut sess_std, "marketProxy", "PlaceBuyOrder", &buy_args).await.expect("STD PlaceBuyOrder");
    rpc(&ctx, &mut sess_both, "marketProxy", "PlaceBuyOrder", &buy_args).await.expect("BOTH PlaceBuyOrder");
    assert_eq!(bal_vip_0 - ctx.db.get_character(PILOT_VIP).unwrap().balance.0, 1_015_000_00, "VIP must pay 1M escrow + 1.5% (1% owner + 0.5% SCC) broker fee");
    assert_eq!(bal_std_0 - ctx.db.get_character(PILOT_STD).unwrap().balance.0, 1_055_000_00, "STD must pay 1M escrow + 5.5% (5% owner + 0.5% SCC) broker fee");
    assert_eq!(bal_both_0 - ctx.db.get_character(PILOT_BOTH).unwrap().balance.0, 1_015_000_00, "Multi-group pilot must receive lowest matching group rate (1% + 0.5% SCC)");

    // 3. Reprocessing per-ACL-group tax rate: VIP = 2%, STD = 8%!
    let rep_vip = rpc(&ctx, &mut sess_vip, "reprocessingSvc", "Reprocess", &[PyRep::Integer(890_002_401)]).await.expect("VIP Reprocess");
    let rep_std = rpc(&ctx, &mut sess_std, "reprocessingSvc", "Reprocess", &[PyRep::Integer(890_002_402)]).await.expect("STD Reprocess");
    assert_eq!(rep_vip.dict_get("stationTaxRate"), Some(&PyRep::Real(0.02)));
    assert_eq!(rep_std.dict_get("stationTaxRate"), Some(&PyRep::Real(0.08)));

    // 4. Clone Bay per-ACL-group flat ISK fee: VIP = 100,000 ISK, STD = 600,000 ISK!
    let cb_vip_0 = ctx.db.get_character(PILOT_VIP).unwrap().balance.0;
    let cb_std_0 = ctx.db.get_character(PILOT_STD).unwrap().balance.0;
    rpc(&ctx, &mut sess_vip, "jumpCloneSvc", "InstallCloneInStructure", &[PyRep::Integer(hub_id)]).await.expect("VIP InstallClone");
    rpc(&ctx, &mut sess_std, "jumpCloneSvc", "InstallCloneInStructure", &[PyRep::Integer(hub_id)]).await.expect("STD InstallClone");
    assert_eq!(cb_vip_0 - ctx.db.get_character(PILOT_VIP).unwrap().balance.0, 100_000_00);
    assert_eq!(cb_std_0 - ctx.db.get_character(PILOT_STD).unwrap().balance.0, 600_000_00);

    // 5. Manufacturing per-ACL-group facility tax: VIP = 1%, STD = 6%!
    let taxes_vip = rpc(&ctx, &mut sess_vip, "facilityManager", "GetFacilityTaxes", &[PyRep::Integer(hub_id)]).await.unwrap();
    let taxes_std = rpc(&ctx, &mut sess_std, "facilityManager", "GetFacilityTaxes", &[PyRep::Integer(hub_id)]).await.unwrap();
    assert_eq!(taxes_vip.dict_get("1"), Some(&PyRep::Real(0.01)));
    assert_eq!(taxes_std.dict_get("1"), Some(&PyRep::Real(0.06)));
    let job_vip = rpc(&ctx, &mut sess_vip, "industryManager", "InstallJob", &mfg_args).await.expect("VIP InstallJob");
    let job_std = rpc(&ctx, &mut sess_std, "industryManager", "InstallJob", &mfg_args).await.expect("STD InstallJob");
    let vip_ftax = job_vip.dict_get("facilityTaxISK").and_then(|v| v.as_f64()).unwrap();
    let std_ftax = job_std.dict_get("facilityTaxISK").and_then(|v| v.as_f64()).unwrap();
    assert!(std_ftax > vip_ftax * 5.0, "STD 6% facility tax ({std_ftax}) must be ~6x VIP 1% facility tax ({vip_ftax})");

    // 5b. Repairshop per-ACL-group tax rate: VIP = 2%, STD = 10%!
    ctx.db.with_mut_snapshot(|snap| {
        for sh_id in [890_002_302_i64, 890_002_303_i64] {
            let sh = snap.items.get_mut(&sh_id).unwrap();
            sh.type_id = 648; // Badger
            sh.dynamic_attributes.insert(eve_sde::attr_ids::DAMAGE, 500.0);
        }
    });
    let rq_vip = rpc(&ctx, &mut sess_vip, "repairSvc", "GetRepairQuotes", &[PyRep::List(vec![PyRep::Integer(890_002_302)])]).await.expect("VIP GetRepairQuotes");
    let rq_std = rpc(&ctx, &mut sess_std, "repairSvc", "GetRepairQuotes", &[PyRep::List(vec![PyRep::Integer(890_002_303)])]).await.expect("STD GetRepairQuotes");
    let extract_repair_charge = |rep: &PyRep, iid: i64| -> f64 {
        let PyRep::Dict(entries) = rep else { panic!() };
        let PyRep::List(rows) = &entries.iter().find(|(k, _)| k.as_i64() == Some(iid)).unwrap().1 else { panic!() };
        rows[0].dict_get("serviceCharge").and_then(|v| v.as_f64()).unwrap()
    };
    assert!(extract_repair_charge(&rq_std, 890_002_303) > extract_repair_charge(&rq_vip, 890_002_302), "STD 10% repair serviceCharge must exceed VIP 2%");

    // 6. Jump Bridge per-service ACL + 300s delayed ChangeProfiles transition!
    for (sess, char_id, ship_id) in [(&mut sess_vip, PILOT_VIP, 890_002_302_i64), (&mut sess_std, PILOT_STD, 890_002_303_i64)] {
        sess.structure_id = None;
        sess.station_id = None;
        sess.solar_system_id = Some(SOV_NULL_A);
        sess.ship_id = Some(ship_id);
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&char_id) {
                ch.structure_id = None;
                ch.solar_system_id = SOV_NULL_A;
                ch.active_ship_id = ship_id;
            }
            if let Some(sh) = snap.items.get_mut(&ship_id) {
                sh.type_id = 587;
                sh.location_id = i64::from(SOV_NULL_A);
                sh.flag = InventoryFlag::None;
            }
        });
        ctx.systems.get_or_spawn(SOV_NULL_A).spawn_ball(Ball {
            id: ship_id, type_id: 587, position: DVec3::new(2.0e10 + 500.0, 0.0, 0.0), velocity: DVec3::ZERO,
            max_velocity: 300.0, inertia: 1.0, mass: 1_000_000.0, radius: 40.0, speed_fraction: 0.0, mode: MovementMode::Stop,
        }).await.unwrap();
    }
    assert_eq!(rpc(&ctx, &mut sess_std, "structureJumpBridgeMgr", "CmdJumpThroughStructureStargate", &[PyRep::Integer(jb_a_id)]).await.unwrap_err().code, "AccessDenied");
    rpc(&ctx, &mut sess_vip, "structureJumpBridgeMgr", "CmdJumpThroughStructureStargate", &[PyRep::Integer(jb_a_id)]).await.expect("VIP pilot jumps Ansiblex");
    assert_eq!(sess_vip.solar_system_id, Some(SOV_NULL_B));

    // Switch jb_a_id to prof_2 (which grants grp_std on SETTING_JUMP_BRIDGE_ACTIVATION) -> 300s delay applies!
    let prof_2 = rpc(&ctx, &mut owner_sess, "structureProfiles", "CreateProfile", &[PyRep::String("Open JB".into()), PyRep::String("JB for Std".into())]).await.unwrap().as_i64().unwrap();
    rpc(&ctx, &mut owner_sess, "structureProfiles", "SaveProfileSettings", &[
        PyRep::Integer(prof_2),
        PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(svc_acl::SETTING_JUMP_BRIDGE_ACTIVATION), PyRep::Real(0.0), PyRep::Integer(grp_std)])]),
    ]).await.unwrap();
    rpc(&ctx, &mut owner_sess, "structureProfiles", "ChangeProfiles", &[PyRep::List(vec![PyRep::Integer(jb_a_id)]), PyRep::Integer(prof_2)]).await.unwrap();

    ctx.advance_virtual_clock(100);
    assert_eq!(rpc(&ctx, &mut sess_std, "structureJumpBridgeMgr", "CmdJumpThroughStructureStargate", &[PyRep::Integer(jb_a_id)]).await.unwrap_err().code, "AccessDenied");
    ctx.advance_virtual_clock(205);
    rpc(&ctx, &mut sess_std, "structureJumpBridgeMgr", "CmdJumpThroughStructureStargate", &[PyRep::Integer(jb_a_id)]).await.expect("STD pilot jumps after 300s delayed profile switch");
    assert_eq!(sess_std.solar_system_id, Some(SOV_NULL_B));
}

}

#[allow(dead_code, unused_imports)]
pub(crate) mod upwell_structures_unanchoring {
    use super::*;

//  Remediation integration tests for `BF-03` (`posMgr.AnchorOrbital` undocked check, player corp &
//  Director/Station Manager role check, planet-in-system + range check, and effective Pochven
//  Triglavian standing with Diplomacy / Connections), `BF-04` (alliance-granted structure control
//  and corporation/alliance block revocation), and `BF-05` / `P2-02` (Mobile deployable 50 km Upwell
//  clearance, Mobile Cynosural Inhibitor high-sec/wormhole/Zarzakh ban, and MMJU lifetime).

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

#[tokio::test]
async fn test_r_struct_ctrl_ship_bypass_01_saved_ship_locks_and_flex_pod_fallback() {
    // parity: feat:structures.board-structure, rpc:structureControl.TakeControl, rpc:structureControl.ReleaseControl, rpc:structureDeliveries.Deliver, rpc:contractProxy.CreateContract, rpc:repackagingSvc.RepackageItems
    eve_testkit::attest(&[
        "feat:structures.board-structure",
        "rpc:structureControl.TakeControl",
        "rpc:structureControl.ReleaseControl",
        "rpc:structureDeliveries.Deliver",
        "rpc:contractProxy.CreateContract",
        "rpc:repackagingSvc.RepackageItems",
    ]);

    let ctx = new_ctx();
    let corp_id = 98_906_001;
    let st_id = 1_000_890_006_100;
    let flex_id = 1_000_890_006_200;
    let pilot_ship = 890_006_001;
    let spare_ship = 890_006_002;
    let flex_ship = 890_006_003;
    let hostile_ship = 890_006_004;
    let flex_pos = DVec3::new(3.0e10, 3.0e10, 0.0);
    let flex_ship_pos = flex_pos + DVec3::new(1_000.0, 0.0, 0.0);

    ctx.db.with_mut_snapshot(|snap| {
        ensure_account(snap, 60);
        ensure_account(snap, 61);
        snap.corporations.insert(corp_id, CorporationRecord {
            corporation_id: corp_id, name: "Control Guard Corp".into(), ticker: "CGD".into(), ceo_id: OWNER,
            alliance_id: None, tax_rate: 0.05,
            members: HashSet::from([OWNER]),
            member_roles: HashMap::from([(OWNER, eve_db::corp_roles::ROLE_DIRECTOR)]),
            master_wallet: IskCents::from_isk(500_000_000), ..Default::default()
        });
        let base_ch = snap.characters.get(&OWNER).cloned().unwrap();
        if let Some(ch) = snap.characters.get_mut(&OWNER) {
            ch.user_id = 60;
            ch.corporation_id = corp_id;
            ch.solar_system_id = RANCER;
            ch.station_id = None;
            ch.structure_id = Some(st_id);
            ch.active_ship_id = pilot_ship;
            ch.balance = IskCents::from_isk(500_000_000);
        }
        snap.characters.insert(ALLY_GUNNER, eve_db::CharacterRecord {
            character_id: ALLY_GUNNER, user_id: 61, name: "Recipient".into(), corporation_id: 1_000_009,
            solar_system_id: RANCER, station_id: None, structure_id: Some(st_id), active_ship_id: hostile_ship,
            ..base_ch
        });
        snap.structures.insert(st_id, UpwellStructureRecord {
            structure_id: st_id, type_id: 35832, owner_corp_id: corp_id, solar_system_id: RANCER,
            name: "Guard Astrahus".into(), position: [1.0e9, 0.0, 0.0], state: life::STATE_SHIELD_VULNERABLE.into(),
            quantum_core_installed: true, fuel_blocks: 2_000, shield_hp: 1_000_000.0, armor_hp: 1_000_000.0,
            hull_hp: 1_000_000.0, reinforced_until_unix: 0, last_damage_tick: 0, last_damage_unix: 0, self_destruct_at_unix: None,
        });
        snap.structures.insert(flex_id, UpwellStructureRecord {
            structure_id: flex_id, type_id: 35841, owner_corp_id: corp_id, solar_system_id: RANCER,
            name: "Guard Ansiblex".into(), position: flex_pos.to_array(), state: life::STATE_SHIELD_VULNERABLE.into(),
            quantum_core_installed: false, fuel_blocks: 2_000, shield_hp: 500_000.0, armor_hp: 500_000.0,
            hull_hp: 500_000.0, reinforced_until_unix: 0, last_damage_tick: 0, last_damage_unix: 0, self_destruct_at_unix: None,
        });
        for (iid, loc) in [(pilot_ship, st_id), (spare_ship, st_id), (flex_ship, i64::from(RANCER)), (hostile_ship, i64::from(RANCER))] {
            snap.items.insert(iid, InventoryItemRecord {
                item_id: iid, type_id: 587, owner_id: if iid == hostile_ship { ALLY_GUNNER } else { OWNER },
                location_id: loc, flag: if loc == st_id { InventoryFlag::Hangar } else { InventoryFlag::None },
                quantity: 1, singleton: true, version: 1, ..Default::default()
            });
        }
    });

    let mut sess = SessionState::new(9601);
    sess.user_id = Some(60);
    sess.character_id = Some(OWNER);
    sess.corporation_id = Some(corp_id);
    sess.corp_role = eve_db::corp_roles::ROLE_DIRECTOR;
    sess.solar_system_id = Some(RANCER);
    sess.structure_id = Some(st_id);
    sess.ship_id = Some(pilot_ship);

    // 1. Docked TakeControl protects saved ship against Deliver, CreateContract, RepackageItems, TrashItems, MultiAdd, and Board.
    rpc(&ctx, &mut sess, "structureControl", "TakeControl", &[PyRep::Integer(st_id)]).await.expect("TakeControl");
    assert_eq!(sess.ship_id, Some(st_id));

    assert_eq!(
        rpc(&ctx, &mut sess, "structureDeliveries", "Deliver", &[PyRep::Integer(st_id), PyRep::Integer(pilot_ship), PyRep::Integer(ALLY_GUNNER)]).await.unwrap_err().code,
        "CannotDeliverActiveShip"
    );
    let contract_args = [
        PyRep::Integer(1), PyRep::Integer(0), PyRep::Integer(0), PyRep::Integer(1440), PyRep::Integer(1),
        PyRep::Integer(st_id), PyRep::Integer(st_id), PyRep::Real(1_000_000.0), PyRep::Real(0.0), PyRep::Real(0.0),
        PyRep::String("Saved Ship Bypass".into()), PyRep::String(String::new()),
        PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(pilot_ship), PyRep::Integer(1)])]),
    ];
    assert_eq!(rpc(&ctx, &mut sess, "contractProxy", "CreateContract", &contract_args).await.unwrap_err().code, "CannotContractActiveShip");
    assert_eq!(rpc(&ctx, &mut sess, "repackagingSvc", "RepackageItems", &[PyRep::List(vec![PyRep::Integer(pilot_ship)])]).await.unwrap_err().code, "CannotRepackageActiveShip");
    assert_eq!(rpc(&ctx, &mut sess, "invbroker", "TrashItems", &[PyRep::List(vec![PyRep::Integer(pilot_ship)])]).await.unwrap_err().code, "DeleteFailed");
    assert_eq!(rpc(&ctx, &mut sess, "invbroker", "MultiAdd", &[PyRep::List(vec![PyRep::Integer(pilot_ship)]), PyRep::Integer(st_id), PyRep::Integer(116)]).await.unwrap_err().code, "CannotMoveActiveShip");
    assert_eq!(rpc(&ctx, &mut sess, "ship", "Board", &[PyRep::Integer(spare_ship)]).await.unwrap_err().code, "ControllingStructure");

    rpc(&ctx, &mut sess, "structureControl", "ReleaseControl", &[PyRep::Integer(st_id)]).await.expect("ReleaseControl");
    assert_eq!(sess.ship_id, Some(pilot_ship));

    // 2. Defense-in-depth: if saved_prev is foreign-owned and no other ship exists in hangar, ReleaseControl provisions an owned Capsule (670).
    rpc(&ctx, &mut sess, "structureControl", "TakeControl", &[PyRep::Integer(st_id)]).await.expect("TakeControl 2");
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.remove(&spare_ship);
        snap.items.get_mut(&pilot_ship).unwrap().owner_id = ALLY_GUNNER;
    });
    rpc(&ctx, &mut sess, "structureControl", "ReleaseControl", &[PyRep::Integer(st_id)]).await.expect("ReleaseControl tampered");
    let docked_pod_id = sess.ship_id.expect("docked capsule");
    assert_ne!(docked_pod_id, pilot_ship);
    ctx.db.with_snapshot(|snap| {
        let pod = snap.items.get(&docked_pod_id).expect("docked capsule record");
        assert_eq!((pod.type_id, pod.owner_id, pod.location_id, pod.flag, pod.singleton), (670, OWNER, st_id, InventoryFlag::Hangar, true));
        assert_eq!(snap.characters.get(&OWNER).unwrap().active_ship_id, docked_pod_id);
    });

    // 3. FLEX in-space TakeControl + parked ship destroyed via execute_death_pipeline -> ReleaseControl spawns/boards a live Capsule in space!
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&OWNER).unwrap();
        ch.structure_id = None;
        ch.active_ship_id = flex_ship;
    });
    sess.structure_id = None;
    sess.ship_id = Some(flex_ship);
    eve_server::services::station_structure::structure_lifecycle_rpc::ensure_structure_ball_in_space(&ctx, RANCER, flex_id).await;
    spawn_ship_ball(&ctx, RANCER, flex_ship, flex_ship_pos).await;

    rpc(&ctx, &mut sess, "structureControl", "TakeControl", &[PyRep::Integer(flex_id)]).await.expect("FLEX TakeControl");
    assert_eq!(sess.ship_id, Some(flex_id));

    eve_server::combat::death_pipeline::execute_death_pipeline(&ctx, RANCER, OWNER, flex_ship, ALLY_GUNNER, flex_ship_pos, 42)
        .await
        .expect("destroy parked FLEX controller ship");
    assert_eq!(rpc(&ctx, &mut sess, "structureControl", "GetStructurePilot", &[PyRep::Integer(flex_id)]).await.unwrap(), PyRep::Integer(OWNER));

    rpc(&ctx, &mut sess, "structureControl", "ReleaseControl", &[]).await.expect("0-arg FLEX ReleaseControl after ship loss");
    let space_pod_id = sess.ship_id.expect("in-space capsule");
    assert_ne!(space_pod_id, flex_ship);
    assert_ne!(space_pod_id, flex_id);
    ctx.db.with_snapshot(|snap| {
        let pod = snap.items.get(&space_pod_id).expect("in-space capsule item");
        assert_eq!((pod.type_id, pod.owner_id, pod.location_id, pod.singleton), (670, OWNER, i64::from(RANCER), true));
        assert_eq!(snap.characters.get(&OWNER).unwrap().active_ship_id, space_pod_id);
    });
    assert!(
        ctx.systems.get_or_spawn(RANCER).get_ball_position_sync(space_pod_id).is_some(),
        "Newly provisioned FLEX fallback Capsule must have a live Ballpark ball in space"
    );
}

}
