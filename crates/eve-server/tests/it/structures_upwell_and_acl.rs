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
    // parity: rpc:structureControl.BoardStructure, rpc:structureControl.EjectFromStructure, rpc:structureControl.GetStructurePilot, rpc:structureControl.ReleaseControl, rpc:structureControl.TakeControl
    eve_testkit::attest(&[
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
    // parity: rpc:structureControl.TakeControl, rpc:structureControl.ReleaseControl, rpc:structureControl.GetStructurePilot
    eve_testkit::attest(&[
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
    // parity: rpc:structureControl.TakeControl, rpc:structureControl.GetStructurePilot
    eve_testkit::attest(&[
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
    // parity: feat:market.structure-markets, rpc:marketProxy.PlaceBuyOrder, rpc:marketProxy.PlaceMultiSellOrder, rpc:marketProxy.ModifyCharOrder, rpc:marketProxy.GetStationAsks, rpc:reprocessingSvc.Reprocess, rpc:jumpCloneSvc.InstallCloneInStructure
    eve_testkit::attest(&[
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
    // parity: rpc:structureControl.TakeControl, rpc:beyonce.CmdWarpToStuff, rpc:structureDocking.Dock, rpc:abyssalMgr.AbyssalEntranceDeployment
    eve_testkit::attest(&[
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
    // parity: rpc:structureDeployment.Anchor
    // Exercises cross-corp profileID rejection, default profile attachment, and DeleteProfile scoping.
    eve_testkit::attest(&[
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
    // parity: feat:structures.docking-acl, rpc:structureSettings.CharacterHasService, rpc:structureSettings.CharacterCheckService
    eve_testkit::attest(&[
        "feat:structures.docking-acl",
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
    // parity: rpc:structureControl.TakeControl, rpc:structureControl.BoardStructure
    eve_testkit::attest(&[
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
    // parity: feat:structures.mobile-deploy-restrictions, rpc:posMgr.AnchorStructure
    eve_testkit::attest(&["feat:structures.mobile-deploy-restrictions", "rpc:posMgr.AnchorStructure"]);

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
    // parity: rpc:structureControl.TakeControl, rpc:structureControl.ReleaseControl, rpc:structureDeliveries.Deliver, rpc:contractProxy.CreateContract, rpc:repackagingSvc.RepackageItems
    eve_testkit::attest(&[
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


#[allow(unused_imports, dead_code, unused_variables, unused_mut)]
mod plan_upwell_structures_ops_a {
    use super::*;

// Upwell Structure Lifecycle, Asset Safety, Reinforcement, Repair & Hack Reveal
// parity tests (`plan_upwell_structures_closure.rs`).

use crate::esi_routes::issue_token_with_scopes;
use eve_db::{InventoryFlag, InventoryItemRecord, UpwellStructureRecord};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::esi::{EsiHttpRequest, EsiRouter};
use eve_server::router::ServerContext;
use eve_server::services::station_structure::structure_lifecycle as life;
use eve_server::services::station_structure::structure_upwell_ops::{load_ext_state, save_ext_state};
use glam::DVec3;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;

const OWNER: i64 = 90_000_001; const HACKER: i64 = 90_000_002; const BYSTANDER: i64 = 90_000_003; const RIVAL_CORP: i64 = 98_999_999;
const JITA: u32 = 30_000_144; const RANCER: u32 = 30_002_718; const HED_GP: u32 = 30_001_161;

fn prepare_siege(ctx: &ServerContext, id: i64, charge_dmg_em: f64) -> i64 {
    ctx.db.with_mut_snapshot(|snap| {
        let my_corp = snap.characters.get(&OWNER).map(|c| c.corporation_id).expect("corp");
        if let Some(s) = snap.structures.get_mut(&id) { s.owner_corp_id = RIVAL_CORP; }
        snap.wars.insert(77, eve_db::WarRecord { war_id: 77, aggressor_corp_id: my_corp, defender_corp_id: RIVAL_CORP, mutual: true, started_unix: 0, active: true });
        if let Some(w) = snap.items.get_mut(&100_000_005) {
            w.location_id = 100_000_001; w.flag = InventoryFlag::HiSlot0; w.singleton = true;
            if charge_dmg_em > 0.0 { w.dynamic_attributes.insert(64, 5_000.0); } else { w.dynamic_attributes.remove(&64); }
        }
        let old_ammo: Vec<i64> = snap.items.values().filter(|i| i.location_id == 100_000_001 && i.flag == InventoryFlag::HiSlot0 && i.item_id != 100_000_005).map(|i| i.item_id).collect();
        for aid in old_ammo { snap.items.remove(&aid); }
    });
    let ammo_id = grant(ctx, OWNER, 100_000_001, 185, InventoryFlag::HiSlot0, 10_000);
    ctx.invalidate_ship_fitting(100_000_001);
    ammo_id
}

async fn rpc(ctx: &ServerContext, sess: &mut SessionState, service: &str, method: &str, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None).await
}
fn kv_i64(rep: &PyRep, key: &str) -> i64 { rep.dict_get(key).and_then(|v| v.as_i64()).unwrap_or(0) }
fn kv_str(rep: &PyRep, key: &str) -> String { rep.dict_get(key).and_then(|v| v.as_str()).unwrap_or("").to_string() }

async fn fire_weapon(ctx: &ServerContext, sess: &mut SessionState, target_id: i64) -> Result<PyRep, BridgeRpcError> {
    let (sys_id, ship_id) = (sess.solar_system_id.expect("sys"), sess.ship_id.expect("ship"));
    let sys = ctx.systems.get_or_spawn(sys_id);
    // BF-03: stop the repeating cycle and let it (and the rate-of-fire delay) lapse before the next volley.
    let _ = rpc(ctx, sess, "dogmaIM", "Deactivate", &[PyRep::Integer(100_000_005), PyRep::String("projectileFired".into())]).await;
    let _ = sys.step_manual(15.0).await;
    ctx.advance_secs(15);
    if !sys.get_target_locks(ship_id).await.unwrap_or_default().iter().any(|l| l.target_id == target_id && l.locked) {
        rpc(ctx, sess, "dogmaIM", "AddTarget", &[PyRep::Integer(target_id)]).await?;
        let _ = sys.step_manual(12.0).await;
    }
    rpc(ctx, sess, "dogmaIM", "Activate", &[PyRep::Integer(100_000_005), PyRep::String("projectileFired".into()), PyRep::Integer(target_id)]).await
}

fn grant(ctx: &ServerContext, owner: i64, location: i64, type_id: u32, flag: InventoryFlag, qty: i32) -> i64 {
    ctx.db.with_mut_snapshot(|snap| {
        snap.next_entity_id += 1;
        let id = snap.next_entity_id;
        snap.items.insert(id, InventoryItemRecord { item_id: id, type_id, owner_id: owner, location_id: location, flag, quantity: qty, singleton: qty == 1, version: 1, ..Default::default() });
        id
    })
}

fn attr(ctx: &ServerContext, type_id: u32, attr_id: u16) -> f64 {
    ctx.sde.types.get(&type_id).and_then(|t| t.default_attributes.get(&attr_id).copied()).unwrap_or_else(|| panic!("SDE type {type_id} lacks attribute {attr_id}"))
}

fn new_test_ctx() -> Arc<ServerContext> {
    let ctx = Arc::new(ServerContext::new());
    ctx.set_clock(Arc::new(eve_db::VirtualServerClock::with_fixed_base(1_800_000_000)));
    ctx
}

async fn pilot_in_space(ctx: &ServerContext, sess_id: u64) -> SessionState {
    let perim_station = ctx.sde.stations.values().find(|s| s.solar_system_id == JITA).map(|s| s.station_id).expect("Perimeter station");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&OWNER) {
            let ship_id = ch.active_ship_id;
            ch.solar_system_id = JITA;
            ch.station_id = Some(perim_station);
            if let Some(sh) = snap.items.get_mut(&ship_id) {
                sh.location_id = perim_station;
            }
        }
    });
    let mut sess = SessionState::new(sess_id);
    rpc(ctx, &mut sess, "authentication", "Login", &[PyRep::String("demo".into()), PyRep::String("demo-secret".into())]).await.expect("Login");
    rpc(ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(OWNER)]).await.expect("Select");
    ctx.advance_secs(12);
    rpc(ctx, &mut sess, "corpRegistry", "AddCorporation", &[PyRep::String("Upwell Closure Corp".into()), PyRep::String("UPWL".into())]).await.expect("AddCorporation");
    ctx.advance_virtual_clock(7 * 86_400 + 12);
    rpc(ctx, &mut sess, "structureDocking", "Undock", &[]).await.expect("Undock");
    sess
}

async fn move_ship(ctx: &ServerContext, sess: &SessionState, pos: DVec3) {
    assert!(ctx.systems.get_or_spawn(sess.solar_system_id.expect("sys")).set_ball_position(sess.ship_id.expect("ship"), pos).await);
}

fn open_space(ctx: &ServerContext, sys_id: u32, k: f64) -> DVec3 {
    let base = ctx.sde.stations.values().find(|s| s.solar_system_id == sys_id).map(|s| s.position).unwrap_or(DVec3::ZERO);
    base + DVec3::new(0.0, 5.0e9 + k * 3.0e9, 0.0)
}

async fn anchor(ctx: &ServerContext, sess: &mut SessionState, type_id: u32, name: &str) -> Result<PyRep, BridgeRpcError> {
    let ship = sess.ship_id.expect("ship");
    grant(ctx, OWNER, ship, type_id, InventoryFlag::Cargo, 1);
    rpc(ctx, sess, "structureDeployment", "Anchor", &[PyRep::Integer(i64::from(type_id)), PyRep::String(name.into())]).await
}

async fn observe(ctx: &ServerContext, sess: &mut SessionState, id: i64) -> Option<UpwellStructureRecord> {
    let _ = rpc(ctx, sess, "structureGuests", "GetGuests", &[PyRep::Integer(id)]).await;
    ctx.db.with_snapshot(|snap| snap.structures.get(&id).cloned())
}

async fn online_structure(ctx: &ServerContext, sess: &mut SessionState, type_id: u32, name: &str) -> i64 {
    let id = kv_i64(&anchor(ctx, sess, type_id, name).await.expect("Anchor"), "structureID");
    ctx.advance_virtual_clock(life::ANCHOR_VULNERABLE_SECS + life::ANCHORING_SECS);
    assert_eq!(observe(ctx, sess, id).await.expect("st").state, life::STATE_NO_CORE);
    let core = attr(ctx, type_id, life::ATTR_REQUIRES_DEED_TYPE) as u32;
    move_ship(ctx, sess, DVec3::from_array(ctx.db.with_snapshot(|s| s.structures[&id].position))).await;
    rpc(ctx, sess, "structureDocking", "Dock", &[PyRep::Integer(id)]).await.expect("Dock");
    let core_item = grant(ctx, OWNER, sess.ship_id.expect("ship"), core, InventoryFlag::Cargo, 1);
    rpc(ctx, sess, "invbroker", "Add", &[PyRep::Integer(core_item), PyRep::Integer(id), PyRep::Integer(172)]).await.expect("move core");
    assert_eq!(observe(ctx, sess, id).await.expect("st").state, life::STATE_ONLINING);
    ctx.advance_virtual_clock(life::ONLINING_SECS);
    assert_eq!(observe(ctx, sess, id).await.expect("st").state, life::STATE_SHIELD_VULNERABLE);
    id
}

#[tokio::test]
async fn test_upwell_lifecycle_asset_safety_abandoned_and_leave_space() {
    // parity: feat:lifecycle.asset_safety-auto_delivery, feat:structures.asset-safety-recovery-delay-and-fee, feat:lifecycle.structure-abandoned_destroyed, feat:lifecycle.structure-leave_space
    eve_testkit::attest(&[
        "feat:lifecycle.asset_safety-auto_delivery", "feat:structures.asset-safety-recovery-delay-and-fee",
        "feat:lifecycle.structure-abandoned_destroyed", "feat:lifecycle.structure-leave_space",
    ]);

    let ctx = new_test_ctx();
    let mut sess = pilot_in_space(&ctx, 7101).await;

    // 1. Security band mapping & same-system Euclidean AU distance for nearest_npc_station_from_pos.
    let mut jita_stations: Vec<_> = ctx.sde.stations.values().filter(|s| s.solar_system_id == JITA).collect();
    jita_stations.sort_by_key(|s| s.station_id);
    let (st1, st2) = (jita_stations[0], jita_stations[1]);
    assert_eq!(life::nearest_npc_station_from_pos(&ctx, JITA, st1.position + DVec3::new(1000.0, 0.0, 0.0)), Some(st1.station_id));
    assert_eq!(life::nearest_npc_station_from_pos(&ctx, JITA, st2.position + DVec3::new(1000.0, 0.0, 0.0)), Some(st2.station_id));
    let pos_jita = open_space(&ctx, JITA, 1.0);
    let st_high = life::nearest_npc_station_from_pos(&ctx, JITA, pos_jita).expect("high-sec station");
    assert_eq!(ctx.sde.stations.get(&st_high).expect("station").solar_system_id, JITA);
    let st_low = life::nearest_npc_station_from_pos(&ctx, RANCER, DVec3::ZERO).expect("low-sec station");
    assert!((0.0..0.45).contains(&ctx.sde.solar_systems.get(&ctx.sde.stations.get(&st_low).unwrap().solar_system_id).unwrap().security_status));
    let st_null = life::nearest_npc_station_from_pos(&ctx, HED_GP, DVec3::ZERO).expect("null-sec -> low-sec station");
    assert!((0.0..0.45).contains(&ctx.sde.solar_systems.get(&ctx.sde.stations.get(&st_null).unwrap().solar_system_id).unwrap().security_status));

    // 2. Anchor two Astrahus structures in Jita: one source, one same-system delivery destination.
    move_ship(&ctx, &sess, pos_jita).await;
    let src_id = online_structure(&ctx, &mut sess, 35832, "Safety Source Astrahus").await;
    let item_5d = grant(&ctx, OWNER, src_id, 34, InventoryFlag::Hangar, 10_000);
    assert_eq!(rpc(&ctx, &mut sess, "structureAssetSafety", "MovePersonalAssetsToSafety", &[PyRep::Integer(i64::from(JITA)), PyRep::Integer(src_id)]).await.expect_err("docked in target").code, "CannotAssetSafetyWhileDockedInTargetStructure");

    rpc(&ctx, &mut sess, "structureDocking", "Undock", &[]).await.expect("Undock");
    move_ship(&ctx, &sess, open_space(&ctx, JITA, 2.0)).await;
    let dst_id = online_structure(&ctx, &mut sess, 35832, "Safety Dest Astrahus").await;

    // 3. Manual MovePersonalAssetsToSafety & MoveCorpAssetsToSafety (skipping boarded ship).
    let wrap_5d = kv_i64(&rpc(&ctx, &mut sess, "structureAssetSafety", "MovePersonalAssetsToSafety", &[PyRep::Integer(i64::from(JITA)), PyRep::Integer(src_id)]).await.expect("MovePersonalAssetsToSafety"), "wrapID");
    assert!(wrap_5d > 0);

    let corp_id = ctx.db.with_snapshot(|snap| snap.characters.get(&OWNER).unwrap().corporation_id);
    let boarded_corp_ship = grant(&ctx, corp_id, src_id, 606, InventoryFlag::CorpSAG1, 1);
    let unboarded_corp_ore = grant(&ctx, corp_id, src_id, 34, InventoryFlag::CorpSAG1, 2_500);
    ctx.db.with_mut_snapshot(|snap| {
        let mut mate = snap.characters.get(&OWNER).unwrap().clone();
        mate.character_id = HACKER; mate.user_id = 2; mate.active_ship_id = boarded_corp_ship;
        snap.characters.insert(HACKER, mate);
    });
    let corp_wrap_live = kv_i64(&rpc(&ctx, &mut sess, "structureAssetSafety", "MoveCorpAssetsToSafety", &[PyRep::Integer(i64::from(JITA)), PyRep::Integer(src_id)]).await.expect("MoveCorpAssetsToSafety"), "wrapID");
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items.get(&boarded_corp_ship).map(|i| i.location_id), Some(src_id));
        assert_eq!(snap.items.get(&unboarded_corp_ore).map(|i| i.location_id), Some(corp_wrap_live));
    });

    // Wormhole manual asset safety rejected; Faction Fortizar (47512) never becomes Abandoned.
    let (wh_sys_id, wh_manual_st) = (31_000_001u32, 990_000_099i64);
    let wh_hangar_p = grant(&ctx, OWNER, wh_manual_st, 34, InventoryFlag::Hangar, 100);
    let wh_hangar_c = grant(&ctx, corp_id, wh_manual_st, 34, InventoryFlag::CorpSAG1, 100);
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(wh_manual_st, UpwellStructureRecord {
            structure_id: wh_manual_st, owner_corp_id: corp_id, type_id: 35832, solar_system_id: wh_sys_id,
            name: "WH Manual Safety".into(), state: life::STATE_SHIELD_VULNERABLE.into(), position: [0.0, 0.0, 0.0],
            shield_hp: 10_000.0, armor_hp: 10_000.0, hull_hp: 10_000.0, fuel_blocks: 1_000, reinforced_until_unix: 0,
            last_damage_tick: 0, last_damage_unix: 0, self_destruct_at_unix: None, quantum_core_installed: true,
        });
        let ffort_id = 990_000_088i64;
        snap.structures.insert(ffort_id, UpwellStructureRecord {
            structure_id: ffort_id, owner_corp_id: corp_id, type_id: 47512, solar_system_id: JITA,
            name: "Moreau Fortizar".into(), state: life::STATE_SHIELD_VULNERABLE.into(), position: [0.0, 0.0, 0.0],
            shield_hp: 10_125_000.0, armor_hp: 8_100_000.0, hull_hp: 32_400_000.0, fuel_blocks: 0, reinforced_until_unix: 0,
            last_damage_tick: 0, last_damage_unix: 0, self_destruct_at_unix: None, quantum_core_installed: true,
        });
        let mut ext = load_ext_state(snap);
        ext.lifecycle.entry(ffort_id).or_default().low_power_since_unix = Some(1);
        assert_eq!(life::power_state(&ctx, snap, &ext, snap.structures.get(&ffort_id).unwrap(), ctx.now_unix()), life::PowerState::LowPower);
        save_ext_state(snap, &ext);
    });
    assert_eq!(rpc(&ctx, &mut sess, "structureAssetSafety", "MovePersonalAssetsToSafety", &[PyRep::Integer(i64::from(wh_sys_id)), PyRep::Integer(wh_manual_st)]).await.expect_err("wh personal").code, "AssetSafetyUnavailableInWormhole");
    assert_eq!(rpc(&ctx, &mut sess, "structureAssetSafety", "MoveCorpAssetsToSafety", &[PyRep::Integer(i64::from(wh_sys_id)), PyRep::Integer(wh_manual_st)]).await.expect_err("wh corp").code, "AssetSafetyUnavailableInWormhole");

    // Wormhole destruction drops hangar contents as space loot; NO Asset Safety wrap created (feat:structures.asset-safety-recovery-delay-and-fee).
    let wh_disposal = ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        life::destroy_structure(&ctx, snap, &mut ext, wh_manual_st, OWNER, ctx.now_unix())
    });
    assert!(wh_disposal.asset_safety_wrap_ids.is_empty(), "Wormhole destruction must create zero Asset Safety wraps");
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items.get(&wh_hangar_p).map(|i| i.location_id), Some(i64::from(wh_sys_id)), "Personal items drop directly in wormhole space");
        assert_eq!(snap.items.get(&wh_hangar_c).map(|i| i.location_id), Some(i64::from(wh_sys_id)), "Corp items drop directly in wormhole space");
    });

    // Before 5 days: rejected; at 6 days (< 20 days): same-system structure delivery charges 0.5%.
    ctx.advance_virtual_clock(4 * 86_400);
    assert_eq!(rpc(&ctx, &mut sess, "structureAssetSafety", "MoveSafetyWrapToStructure", &[PyRep::Integer(wrap_5d), PyRep::Integer(dst_id)]).await.expect_err("early").code, "AssetSafetyCooldownActive");
    ctx.advance_virtual_clock(2 * 86_400);
    let est_val = ctx.sde.types.get(&34).unwrap().base_price * 10_000.0;
    let deliv_5d = rpc(&ctx, &mut sess, "structureAssetSafety", "MoveSafetyWrapToStructure", &[PyRep::Integer(wrap_5d), PyRep::Integer(dst_id)]).await.expect("deliver 5d");
    assert!((deliv_5d.dict_get("feePaidISK").and_then(|v| v.as_f64()).unwrap_or(-1.0) - est_val * 0.005).abs() < 1.0);
    ctx.db.with_snapshot(|snap| assert_eq!(snap.items.get(&item_5d).map(|i| (i.location_id, i.flag)), Some((dst_id, InventoryFlag::Deliveries))));

    // 4. Lazy 20-day auto-delivery triggered directly via ESI GET /characters/{id}/assets/ & GET /corporations/{id}/assets/ (with NO prior structure RPC!).
    // Destroying a structure in stationless null-sec (HED-GP) triggers cross-system auto-delivery to the nearest lowsec NPC station (st_null) at 15% fee.
    let (null_sys_id, null_st_id) = (HED_GP, 990_000_077i64);
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(null_st_id, UpwellStructureRecord {
            structure_id: null_st_id, owner_corp_id: corp_id, type_id: 35832, solar_system_id: null_sys_id,
            name: "Null Safety Source".into(), state: life::STATE_SHIELD_VULNERABLE.into(), position: [0.0, 0.0, 0.0],
            shield_hp: 10_000.0, armor_hp: 10_000.0, hull_hp: 10_000.0, fuel_blocks: 100, reinforced_until_unix: 0,
            last_damage_tick: 0, last_damage_unix: 0, self_destruct_at_unix: None, quantum_core_installed: true,
        });
    });
    let item_20d = grant(&ctx, OWNER, null_st_id, 34, InventoryFlag::Hangar, 20_000);
    let wrap_20d = kv_i64(&rpc(&ctx, &mut sess, "structureAssetSafety", "MovePersonalAssetsToSafety", &[PyRep::Integer(i64::from(null_sys_id)), PyRep::Integer(null_st_id)]).await.expect("wrap 20d"), "wrapID");
    let corp_item_20d = grant(&ctx, corp_id, null_st_id, 34, InventoryFlag::CorpSAG1, 10_000);
    ctx.db.with_mut_snapshot(|snap| { if let Some(m) = snap.characters.get_mut(&HACKER) { m.active_ship_id = 0; } });
    let corp_wrap_20d = kv_i64(&rpc(&ctx, &mut sess, "structureAssetSafety", "MoveCorpAssetsToSafety", &[PyRep::Integer(i64::from(null_sys_id)), PyRep::Integer(null_st_id)]).await.expect("corp wrap 20d"), "wrapID");

    ctx.advance_virtual_clock(life::ASSET_SAFETY_AUTO_DELIVERY_SECS + 10);
    let esi_router = EsiRouter::new(Arc::clone(&ctx));
    let token = issue_token_with_scopes(&ctx, OWNER, vec!["esi-assets.read_assets.v1".into(), "esi-assets.read_corporation_assets.v1".into()]);
    let char_resp = esi_router.handle_request(&EsiHttpRequest::new("GET", format!("/v5/characters/{OWNER}/assets/")).with_bearer_token(&token));
    assert_eq!(char_resp.status_code, 200);
    let char_json: Value = serde_json::from_slice(&char_resp.body).unwrap();
    assert!(char_json.as_array().unwrap().iter().any(|a| a["item_id"].as_i64() == Some(wrap_20d) && a["location_id"].as_i64() == Some(st_null)), "ESI character assets must lazily auto-deliver 20d wrap to nearest NPC station");
    let corp_resp = esi_router.handle_request(&EsiHttpRequest::new("GET", format!("/v5/corporations/{corp_id}/assets/")).with_bearer_token(&token));
    assert_eq!(corp_resp.status_code, 200);
    let corp_json: Value = serde_json::from_slice(&corp_resp.body).unwrap();
    assert!(corp_json.as_array().unwrap().iter().any(|a| a["item_id"].as_i64() == Some(corp_wrap_20d) && a["location_id"].as_i64() == Some(st_null)), "ESI corporation assets must lazily auto-deliver 20d corp wrap to nearest NPC station");

    let est_val_20d = ctx.sde.types.get(&34).unwrap().base_price * 20_000.0;
    let deliv_20d = rpc(&ctx, &mut sess, "structureAssetSafety", "MoveSafetyWrapToStructure", &[PyRep::Integer(wrap_20d), PyRep::Integer(st_null)]).await.expect("retrieve 20d");
    assert!((deliv_20d.dict_get("feePaidISK").and_then(|v| v.as_f64()).unwrap_or(-1.0) - est_val_20d * 0.15).abs() < 1.0);
    ctx.db.with_snapshot(|snap| assert_eq!(snap.items.get(&item_20d).map(|i| (i.location_id, i.flag)), Some((st_null, InventoryFlag::Deliveries))));

    // Corporate asset safety retrieval debits corp wallet division 1 and records BOTH wallet_journal and corp_wallet_journals.
    ctx.db.with_mut_snapshot(|snap| snap.corporations.get_mut(&corp_id).unwrap().set_division_balance(1, eve_db::IskCents::from_f64_isk(50_000_000.0)));
    let corp_deliv = rpc(&ctx, &mut sess, "structureAssetSafety", "MoveSafetyWrapToStructure", &[PyRep::Integer(corp_wrap_20d), PyRep::Integer(st_null)]).await.expect("retrieve corp 20d");
    let corp_fee = corp_deliv.dict_get("feePaidISK").and_then(|v| v.as_f64()).unwrap_or(0.0);
    assert!(corp_fee > 0.0);
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items.get(&corp_item_20d).map(|i| i.location_id), Some(st_null));
        let cj = snap.corp_wallet_journals.get(&(corp_id, 1)).expect("corp_wallet_journals division 1");
        assert!(cj.iter().any(|e| e.ref_type == "AssetSafetyRecoveryTax" && (e.amount + corp_fee).abs() < 1e-2));
    });

    // 5. Unanchor completion (leave_space): wraps SpecializedFuelBay items into corp Asset Safety, removes structure ball, spawns packaged structure & dropped core balls, emits notifications.
    let dst_ship = sess.ship_id.unwrap();
    let dst_fb = grant(&ctx, OWNER, dst_ship, 4247, InventoryFlag::Cargo, 250);
    let dst_oz = grant(&ctx, OWNER, dst_ship, 16273, InventoryFlag::Cargo, 400);
    rpc(&ctx, &mut sess, "invbroker", "Add", &[PyRep::Integer(dst_fb), PyRep::Integer(dst_id), PyRep::Integer(133), PyRep::Integer(250)]).await.expect("dst fuel blocks");
    rpc(&ctx, &mut sess, "invbroker", "Add", &[PyRep::Integer(dst_oz), PyRep::Integer(dst_id), PyRep::Integer(133), PyRep::Integer(400)]).await.expect("dst ozone");
    assert_eq!(observe(&ctx, &mut sess, dst_id).await.unwrap().fuel_blocks, 250, "Ozone must not inflate fuel_blocks");
    rpc(&ctx, &mut sess, "structureDocking", "Undock", &[]).await.expect("Undock");
    let _ = ctx.drain_notifications_for_char(OWNER);
    rpc(&ctx, &mut sess, "structureDeployment", "Unanchor", &[PyRep::Integer(dst_id)]).await.expect("Unanchor");
    assert!(!ctx.drain_notifications_for_char(OWNER).iter().any(|n| n.event == "OnStructureRemoved"));
    ctx.advance_virtual_clock(7 * 86_400 + 5);
    assert!(observe(&ctx, &mut sess, dst_id).await.is_none());
    let unanchor_notifs = ctx.drain_notifications_for_char(OWNER);
    assert!(unanchor_notifs.iter().any(|n| n.event == "OnStructureRemoved") && unanchor_notifs.iter().any(|n| n.event == "OnStructureRemovedFromSpace"));
    let jita_sys = ctx.systems.get_or_spawn(JITA);
    assert!(jita_sys.get_ball_snapshot(dst_id).await.unwrap().is_none());
    ctx.db.with_snapshot(|snap| {
        let ext = load_ext_state(snap);
        let dst_wrap = ext.asset_safety_wraps.values().find(|w| w.is_corp && w.source_structure_id == dst_id).expect("dst unanchor corp wrap");
        assert!(dst_wrap.item_ids.iter().any(|id| snap.items.get(id).is_some_and(|i| i.type_id == 4247 && i.quantity == 250)), "Unanchor must wrap remaining Fuel Blocks into corp Asset Safety");
        assert!(dst_wrap.item_ids.iter().any(|id| snap.items.get(id).is_some_and(|i| i.type_id == 16273 && i.quantity == 400)), "Unanchor must wrap Liquid Ozone into corp Asset Safety");
    });

    // 6. Abandoned K-space structure destruction via live weapon combat across all 3 layers (Shield -> Armor -> Hull -> Destroyed), dropping SpecializedFuelBay fuel & reagents in space.
    ctx.advance_virtual_clock(life::ABANDON_AFTER_LOW_POWER_SECS + 60);
    let _ = observe(&ctx, &mut sess, src_id).await;
    move_ship(&ctx, &sess, pos_jita + DVec3::new(60_000.0, 0.0, 0.0)).await;
    let src_fb_in = grant(&ctx, OWNER, dst_ship, 4247, InventoryFlag::Cargo, 180);
    let src_gas_in = grant(&ctx, OWNER, dst_ship, 81143, InventoryFlag::Cargo, 320);
    rpc(&ctx, &mut sess, "invbroker", "Add", &[PyRep::Integer(src_fb_in), PyRep::Integer(src_id), PyRep::Integer(133), PyRep::Integer(180)]).await.expect("src fuel blocks");
    rpc(&ctx, &mut sess, "invbroker", "Add", &[PyRep::Integer(src_gas_in), PyRep::Integer(src_id), PyRep::Integer(133), PyRep::Integer(320)]).await.expect("src magmatic gas");
    let abandoned_hangar = grant(&ctx, OWNER, src_id, 35, InventoryFlag::Hangar, 7_777);
    let abandoned_rig = grant(&ctx, OWNER, src_id, 37146, InventoryFlag::RigSlot0, 1);
    let abandoned_hi = grant(&ctx, OWNER, src_id, 3170, InventoryFlag::HiSlot0, 1);
    prepare_siege(&ctx, src_id, 200_000.0);
    ctx.db.with_mut_snapshot(|snap| {
        let s = snap.structures.get_mut(&src_id).unwrap();
        s.shield_hp = 10.0; s.armor_hp = 10.0; s.hull_hp = 10.0;
    });
    fire_weapon(&ctx, &mut sess, src_id).await.expect("abandoned shield -> armor");
    assert_eq!(ctx.db.with_snapshot(|s| s.structures[&src_id].state.clone()), life::STATE_ARMOR_VULNERABLE);
    fire_weapon(&ctx, &mut sess, src_id).await.expect("abandoned armor -> hull");
    assert_eq!(ctx.db.with_snapshot(|s| s.structures[&src_id].state.clone()), life::STATE_HULL_VULNERABLE);
    fire_weapon(&ctx, &mut sess, src_id).await.expect("abandoned hull -> destroyed");
    assert_eq!(ctx.db.with_snapshot(|s| s.structures[&src_id].state.clone()), life::STATE_DESTROYED);
    assert!(jita_sys.get_ball_snapshot(src_id).await.unwrap().is_none());
    assert!(jita_sys.get_ball_snapshot(abandoned_hangar).await.unwrap().is_some());
    assert!(jita_sys.get_ball_snapshot(abandoned_hi).await.unwrap().is_some());
    let (dropped_fb_id, dropped_gas_id) = ctx.db.with_snapshot(|snap| {
        let ext = load_ext_state(snap);
        assert!(!ext.asset_safety_wraps.values().any(|w| w.source_structure_id == src_id && w.item_ids.contains(&abandoned_hangar)));
        assert!(!snap.items.contains_key(&abandoned_rig));
        assert_eq!(snap.items.get(&abandoned_hangar).map(|i| i.location_id), Some(i64::from(JITA)));
        let fb = snap.items.values().find(|i| i.location_id == i64::from(JITA) && i.type_id == 4247 && i.quantity == 180).expect("dropped fuel blocks").item_id;
        let gas = snap.items.values().find(|i| i.location_id == i64::from(JITA) && i.type_id == 81143 && i.quantity == 320).expect("dropped magmatic gas").item_id;
        (fb, gas)
    });
    assert!(jita_sys.get_ball_snapshot(dropped_fb_id).await.unwrap().is_some());
    assert!(jita_sys.get_ball_snapshot(dropped_gas_id).await.unwrap().is_some());

    // Destroyed structure rejects management, control, boarding, and manual asset safety RPCs.
    ctx.db.with_mut_snapshot(|snap| snap.structures.get_mut(&src_id).unwrap().owner_corp_id = corp_id);
    assert_eq!(rpc(&ctx, &mut sess, "structureAssetSafety", "MovePersonalAssetsToSafety", &[PyRep::Integer(i64::from(JITA)), PyRep::Integer(src_id)]).await.expect_err("destroyed safety").code, "StructureDestroyed");
    assert_eq!(rpc(&ctx, &mut sess, "structureAssetSafety", "MoveCorpAssetsToSafety", &[PyRep::Integer(i64::from(JITA)), PyRep::Integer(src_id)]).await.expect_err("destroyed corp safety").code, "StructureDestroyed");
    assert_eq!(rpc(&ctx, &mut sess, "structureControl", "TakeControl", &[PyRep::Integer(src_id)]).await.expect_err("destroyed take control").code, "StructureDestroyed");
    assert_eq!(rpc(&ctx, &mut sess, "structureControl", "BoardStructure", &[PyRep::Integer(src_id)]).await.expect_err("destroyed board").code, "StructureDestroyed");
    assert_eq!(rpc(&ctx, &mut sess, "structureDirectory", "SetStructureDescription", &[PyRep::Integer(src_id), PyRep::String("Dead".into())]).await.expect_err("destroyed desc").code, "StructureDestroyed");

    // 7. Lowsec transit recovery charges 15% fee.
    let lowsec_st = ctx.sde.stations.values().find(|s| ctx.sde.solar_systems.get(&s.solar_system_id).is_some_and(|sy| sy.security_status < 0.45 && sy.security_status > 0.0)).map(|s| s.station_id).unwrap();
    let lowsec_wrap_id = grant(&ctx, OWNER, lowsec_st, 34, InventoryFlag::Hangar, 10_000);
    let lowsec_val = ctx.sde.types.get(&34).unwrap().base_price * 10_000.0;
    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        ext.asset_safety_wraps.insert(lowsec_wrap_id, eve_server::services::station_structure::structure_upwell_ops::AssetSafetyWrapRecord {
            wrap_id: lowsec_wrap_id, owner_id: OWNER, is_corp: false, source_structure_id: src_id, source_structure_name: "Source".into(),
            solar_system_id: JITA, created_unix: ctx.now_unix(), deliverable_after_unix: 0, delivered_to_id: None, estimated_value_isk: lowsec_val,
            item_ids: vec![lowsec_wrap_id], nearest_npc_station_id: Some(lowsec_st), auto_delivered: false, retrieval_fee_paid: false,
        });
        save_ext_state(snap, &ext);
    });
    let lowsec_deliv = rpc(&ctx, &mut sess, "structureAssetSafety", "MoveSafetyWrapToStructure", &[PyRep::Integer(lowsec_wrap_id), PyRep::Integer(lowsec_st)]).await.expect("lowsec transit");
    assert!((lowsec_deliv.dict_get("feePaidISK").and_then(|v| v.as_f64()).unwrap_or(-1.0) - lowsec_val * 0.15).abs() < 1.0);
}

#[tokio::test]
async fn test_upwell_reinforcement_durations_exit_hack_and_repair() {
    // parity: feat:structures.armor-reinforce-durations, feat:structures.hull-reinforce-durations, feat:structures.reinforce-exit-time-rules, feat:structures.reinforce-hour-hack-reveal, feat:structures.repair-timer-pause-threshold, feat:structures.vulnerability-repair-window
    eve_testkit::attest(&[
        "feat:structures.armor-reinforce-durations", "feat:structures.hull-reinforce-durations",
        "feat:structures.reinforce-exit-time-rules", "feat:structures.reinforce-hour-hack-reveal",
        "feat:structures.repair-timer-pause-threshold", "feat:structures.vulnerability-repair-window",
    ]);

    let ctx = new_test_ctx();
    let mut sess = pilot_in_space(&ctx, 7103).await;
    let pos4 = open_space(&ctx, JITA, 4.0);
    move_ship(&ctx, &sess, pos4).await;
    let st_id = online_structure(&ctx, &mut sess, 35832, "Reinforce Test Astrahus").await;
    rpc(&ctx, &mut sess, "structureDocking", "Undock", &[]).await.expect("Undock");
    move_ship(&ctx, &sess, pos4 + DVec3::new(60_000.0, 0.0, 0.0)).await;

    // 1. Armor & Hull reinforcement duration tables & jitter window rules.
    use life::{armor_reinforcement_secs as a_secs, hull_reinforcement_secs as h_secs, HullClass::*, PowerState::*, SpaceClass::*};
    assert_eq!(a_secs(Medium, HighSec, FullPower), Some(108 * 3600)); assert_eq!(a_secs(Medium, LowNullSec, FullPower), Some(84 * 3600)); assert_eq!(a_secs(Medium, Wormhole, FullPower), Some(60 * 3600));
    assert_eq!(a_secs(Medium, WarHq, FullPower), Some(24 * 3600)); assert_eq!(a_secs(Large, HighSec, FullPower), Some(24 * 3600)); assert_eq!(a_secs(XLarge, LowNullSec, FullPower), Some(24 * 3600));
    assert_eq!(a_secs(Large, HighSec, LowPower), None); assert_eq!(a_secs(Flex, LowNullSec, FullPower), None); assert_eq!(a_secs(Medium, HighSec, Abandoned), None);
    assert_eq!(h_secs(Medium, HighSec, FullPower), None); assert_eq!(h_secs(Large, HighSec, FullPower), Some(108 * 3600)); assert_eq!(h_secs(Large, LowNullSec, FullPower), Some(60 * 3600));
    assert_eq!(h_secs(XLarge, Wormhole, FullPower), Some(36 * 3600)); assert_eq!(h_secs(XLarge, WarHq, FullPower), Some(24 * 3600)); assert_eq!(h_secs(Flex, LowNullSec, FullPower), Some(1800)); assert_eq!(h_secs(Large, HighSec, Abandoned), None);
    assert_eq!(life::reinforce_jitter_window_secs(Medium, true), 5_400); assert_eq!(life::reinforce_jitter_window_secs(Large, true), 10_800);
    assert_eq!(life::REPAIR_HULL_SECS, 30 * 60, "HullVulnerable repair window must be 30 minutes (1800s)");

    // 2. Post-resist repair timer pause threshold (10% of layer damage cap) & 15m shield / 30m hull repair (including Abandoned structures) via live dogmaIM.Activate.
    // Astrahus shield pause threshold = 500 DPS, EM resonance = 0.8 (20% resist), 200mm AutoCannon II cycle = 1.875s / dmg_mult:
    prepare_siege(&ctx, st_id, 0.0);
    fire_weapon(&ctx, &mut sess, st_id).await.expect("light hit");
    let rep_after_light = ctx.db.with_snapshot(|s| load_ext_state(s).lifecycle.get(&st_id).and_then(|l| l.repair_until_unix)).expect("repair timer started");
    ctx.advance_virtual_clock(5);
    prepare_siege(&ctx, st_id, 20_000.0);
    fire_weapon(&ctx, &mut sess, st_id).await.expect("qualifying heavy hit");
    let rep_after_heavy = ctx.db.with_snapshot(|s| load_ext_state(s).lifecycle.get(&st_id).and_then(|l| l.repair_until_unix)).expect("repair timer paused");
    assert!(rep_after_heavy > rep_after_light, "qualifying post-resist DPS must advance repair_until_unix");
    ctx.advance_virtual_clock(life::REPAIR_SHIELD_ARMOR_SECS + 30);
    let prof = life::structure_profile(&ctx, 35832).unwrap();
    assert!((observe(&ctx, &mut sess, st_id).await.unwrap().shield_hp - prof.shield_hp).abs() < 1.0);

    // Abandoned structure also pauses its repair timer on qualifying DPS and heals after uncontested 30m hull window:
    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        ext.lifecycle.entry(st_id).or_default().low_power_since_unix = Some(1);
        save_ext_state(snap, &ext);
        let s = snap.structures.get_mut(&st_id).unwrap();
        s.shield_hp = 10.0; s.armor_hp = 10.0; s.hull_hp = 50_000.0;
    });
    fire_weapon(&ctx, &mut sess, st_id).await.expect("abandoned shield -> armor");
    fire_weapon(&ctx, &mut sess, st_id).await.expect("abandoned armor -> hull");
    let ab_hull_rep0 = ctx.db.with_snapshot(|s| load_ext_state(s).lifecycle.get(&st_id).and_then(|l| l.repair_until_unix)).expect("abandoned hull repair timer");
    ctx.advance_virtual_clock(10);
    fire_weapon(&ctx, &mut sess, st_id).await.expect("pause abandoned hull repair timer");
    let ab_hull_rep1 = ctx.db.with_snapshot(|s| load_ext_state(s).lifecycle.get(&st_id).and_then(|l| l.repair_until_unix)).unwrap();
    assert!(ab_hull_rep1 > ab_hull_rep0, "Abandoned HullVulnerable repair timer must pause on qualifying DPS");
    ctx.advance_virtual_clock(life::REPAIR_HULL_SECS + 5);
    let ab_healed = observe(&ctx, &mut sess, st_id).await.unwrap();
    assert_eq!(ab_healed.state, life::STATE_SHIELD_VULNERABLE, "Abandoned HullVulnerable heals after 30m");
    assert!((ab_healed.hull_hp - prof.hull_hp).abs() < 1.0, "Abandoned structure hull fully restored");
    assert!((ab_healed.armor_hp - prof.armor_hp).abs() < 1.0, "Abandoned structure armor fully restored");
    assert!((ab_healed.shield_hp - prof.shield_hp).abs() < 1.0, "Abandoned structure shield fully restored");
    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        ext.lifecycle.entry(st_id).or_default().low_power_since_unix = None;
        save_ext_state(snap, &ext);
    });

    // 3. Multi-timer High-Sec FullPower Fortizar (1.0d + 4.5d anchored to shield_fight_started_unix) + 30-minute HullVulnerable repair window via live RPCs.
    let my_corp = ctx.db.with_snapshot(|snap| snap.characters.get(&OWNER).unwrap().corporation_id);
    ctx.db.with_mut_snapshot(|snap| snap.structures.get_mut(&st_id).unwrap().owner_corp_id = my_corp);
    let pos_fort = open_space(&ctx, JITA, 6.0);
    move_ship(&ctx, &sess, pos_fort).await;
    let fort_id = online_structure(&ctx, &mut sess, 35833, "MultiTimer Fortizar").await;
    let ship_id = sess.ship_id.unwrap();
    let fuel_item = grant(&ctx, OWNER, ship_id, 4247, InventoryFlag::Cargo, 25_000);
    let svc_item = grant(&ctx, OWNER, ship_id, 35894, InventoryFlag::Cargo, 1);
    rpc(&ctx, &mut sess, "invbroker", "Add", &[PyRep::Integer(fuel_item), PyRep::Integer(fort_id), PyRep::Integer(133), PyRep::Integer(25_000)]).await.expect("fuel fort");
    let mut kw_on = HashMap::new();
    kw_on.insert("online".into(), PyRep::Bool(true));
    ctx.dispatch_rpc(&mut sess, "invbroker", None, "Add", &[PyRep::Integer(svc_item), PyRep::Integer(fort_id), PyRep::Integer(InventoryFlag::ServiceSlot0 as i64)], &kw_on, None).await.expect("online service");
    rpc(&ctx, &mut sess, "structureDocking", "Undock", &[]).await.expect("Undock fort");
    move_ship(&ctx, &sess, pos_fort).await;

    // Align clock to exact 00:00 UTC with reinforce_hour = 18 so armor unjittered exit = t0 + 42h (18:00 Day 1)
    // and multi-timer hull unjittered exit = next_reinforce_exit(t0 + (24 + 108)*3600, 18) = t0 + 138h (18:00 Day 5, NOT Day 6!).
    let cur = ctx.now_unix();
    let next_midnight = ((cur / 86_400) + 1) * 86_400;
    ctx.advance_virtual_clock(next_midnight - cur);
    let t0 = ctx.now_unix();
    let fort_prof = life::structure_profile(&ctx, 35833).unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        snap.structure_vulnerability_windows.insert(fort_id, (18, 3));
        let s = snap.structures.get_mut(&fort_id).unwrap();
        s.shield_hp = 10.0; s.armor_hp = 50_000.0; s.hull_hp = fort_prof.hull_hp;
    });
    prepare_siege(&ctx, fort_id, 20_000.0);
    fire_weapon(&ctx, &mut sess, fort_id).await.expect("strip fort shield");
    let info_ar = rpc(&ctx, &mut sess, "structureDirectory", "GetStructureInfo", &[PyRep::Integer(fort_id)]).await.expect("info ar");
    assert_eq!(kv_str(&info_ar, "state"), life::STATE_ARMOR_REINFORCED);
    let ar_exit = observe(&ctx, &mut sess, fort_id).await.unwrap().reinforced_until_unix;
    let exp_ar_base = t0 + 42 * 3_600;
    assert!((ar_exit as i64 - exp_ar_base as i64).abs() <= 10_800, "Fortizar armor exit within ±3h of Day 1 18:00");

    // Advance into ArmorVulnerable, pause armor repair timer with qualifying damage, then strip armor -> HullReinforced.
    ctx.advance_virtual_clock((ar_exit + 5).saturating_sub(ctx.now_unix()));
    assert_eq!(kv_str(&rpc(&ctx, &mut sess, "structureDirectory", "GetStructureInfo", &[PyRep::Integer(fort_id)]).await.unwrap(), "state"), life::STATE_ARMOR_VULNERABLE);
    fire_weapon(&ctx, &mut sess, fort_id).await.expect("pause armor repair timer");
    ctx.advance_virtual_clock(15);
    ctx.db.with_mut_snapshot(|snap| snap.structures.get_mut(&fort_id).unwrap().armor_hp = 10.0);
    fire_weapon(&ctx, &mut sess, fort_id).await.expect("strip fort armor");
    assert_eq!(kv_str(&rpc(&ctx, &mut sess, "structureDirectory", "GetStructureInfo", &[PyRep::Integer(fort_id)]).await.unwrap(), "state"), life::STATE_HULL_REINFORCED);
    let hu_exit = observe(&ctx, &mut sess, fort_id).await.unwrap().reinforced_until_unix;
    let exp_hu_base = life::next_reinforce_exit(t0 + (24 + 108) * 3_600, 18);
    assert_eq!(exp_hu_base, t0 + 138 * 3_600);
    assert!((hu_exit as i64 - exp_hu_base as i64).abs() <= 10_800, "Multi-timer hull exit must anchor to t0 + 24h + 108h (Day 5 18:00), not armor exit + 108h");

    // Advance into HullVulnerable and verify exact 30-minute (1800s) repair window via GetStructureInfo.
    ctx.advance_virtual_clock(hu_exit.saturating_sub(ctx.now_unix()));
    assert_eq!(kv_str(&rpc(&ctx, &mut sess, "structureDirectory", "GetStructureInfo", &[PyRep::Integer(fort_id)]).await.unwrap(), "state"), life::STATE_HULL_VULNERABLE);
    ctx.advance_virtual_clock(1_799);
    assert_eq!(kv_str(&rpc(&ctx, &mut sess, "structureDirectory", "GetStructureInfo", &[PyRep::Integer(fort_id)]).await.unwrap(), "state"), life::STATE_HULL_VULNERABLE, "at 1799s structure must still be in HullVulnerable");
    ctx.advance_virtual_clock(1);
    assert_eq!(kv_str(&rpc(&ctx, &mut sess, "structureDirectory", "GetStructureInfo", &[PyRep::Integer(fort_id)]).await.unwrap(), "state"), life::STATE_SHIELD_VULNERABLE, "at 1800s HullVulnerable 30m repair completes to ShieldVulnerable");
    let healed_fort = observe(&ctx, &mut sess, fort_id).await.unwrap();
    assert!(healed_fort.shield_hp >= fort_prof.shield_hp && (healed_fort.armor_hp - fort_prof.armor_hp).abs() < 1.0 && (healed_fort.hull_hp - fort_prof.hull_hp).abs() < 1.0);

    // 4. FLEX negative-jitter clamp: Pharolux Cyno Beacon (35840) requires Sov Nullsec + online Sov Hub upgrade, and never gets reinforced_until_unix <= now.
    let flex_day = ((ctx.now_unix() / 86_400) + 1) * 86_400;
    let target_t = flex_day + 17 * 3_600 + 35 * 60;
    ctx.advance_virtual_clock(target_t - ctx.now_unix());
    sess.solar_system_id = Some(RANCER);
    ctx.db.with_mut_snapshot(|snap| snap.characters.get_mut(&OWNER).unwrap().solar_system_id = RANCER);
    let rancer_pos = open_space(&ctx, RANCER, 1.0);
    let rancer_sys = ctx.systems.get_or_spawn(RANCER);
    let _ = rancer_sys.spawn_ball_with_combat(eve_destiny::Ball { id: ship_id, type_id: 587, position: rancer_pos, velocity: DVec3::ZERO, radius: 40.0, mass: 1.0e6, mode: eve_destiny::MovementMode::Stop, speed_fraction: 0.0, max_velocity: 300.0, inertia: 1.0 }, ctx.build_type_combat_state(587)).await;
    assert_eq!(anchor(&ctx, &mut sess, 35840, "Rancer Flex Beacon").await.expect_err("lowsec flex rejected").code, "CannotAnchorFlexOutsideSovNull");
    ctx.db.with_mut_snapshot(|snap| snap.items.retain(|_, it| !(it.location_id == ship_id && it.type_id == 35840 && it.flag == InventoryFlag::Cargo)));

    sess.solar_system_id = Some(HED_GP);
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&OWNER).unwrap().solar_system_id = HED_GP;
        let my_corp = snap.characters.get(&OWNER).unwrap().corporation_id;
        let ally_id = 99_001_161;
        snap.corporations.get_mut(&my_corp).unwrap().alliance_id = Some(ally_id);
        snap.system_states.entry(HED_GP).or_insert_with(|| eve_db::SystemDynamicState { solar_system_id: HED_GP, ..Default::default() }).sov_alliance_id = Some(ally_id);
        let hub_id = 880_001_161;
        snap.structures.insert(hub_id, UpwellStructureRecord {
            structure_id: hub_id, type_id: 32458, owner_corp_id: my_corp, solar_system_id: HED_GP,
            name: "HED-GP Sov Hub".into(), state: life::STATE_SHIELD_VULNERABLE.into(), position: [0.0, 0.0, 0.0],
            quantum_core_installed: true, fuel_blocks: 1_000, shield_hp: 100_000.0, armor_hp: 100_000.0, hull_hp: 100_000.0,
            reinforced_until_unix: 0, last_damage_tick: 0, last_damage_unix: 0, self_destruct_at_unix: None,
        });
        let upg_id = 880_001_162;
        snap.items.insert(upg_id, InventoryItemRecord { item_id: upg_id, type_id: 81615, owner_id: my_corp, location_id: hub_id, flag: InventoryFlag::ServiceSlot0, quantity: 1, singleton: true, version: 1, ..Default::default() });
        snap.module_online_states.insert(upg_id, true);
        snap.sov_runtime.sov_hubs.insert(
            hub_id,
            eve_db::PersistedSovHubData {
                hub_id,
                upgrades: vec![eve_db::PersistedHubUpgrade { type_id: 81615, online: true, low_power: false }],
                ..Default::default()
            },
        );
    });
    let hed_pos = open_space(&ctx, HED_GP, 1.0);
    let hed_sys = ctx.systems.get_or_spawn(HED_GP);
    let _ = hed_sys.spawn_ball_with_combat(eve_destiny::Ball { id: ship_id, type_id: 587, position: hed_pos, velocity: DVec3::ZERO, radius: 40.0, mass: 1.0e6, mode: eve_destiny::MovementMode::Stop, speed_fraction: 0.0, max_velocity: 300.0, inertia: 1.0 }, ctx.build_type_combat_state(587)).await;
    let flex_id = kv_i64(&anchor(&ctx, &mut sess, 35840, "HED-GP Flex Beacon").await.expect("anchor flex"), "structureID");
    move_ship(&ctx, &sess, hed_pos + DVec3::new(60_000.0, 0.0, 0.0)).await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.structure_vulnerability_windows.insert(flex_id, (18, 3));
        let f = snap.structures.get_mut(&flex_id).unwrap();
        f.state = life::STATE_SHIELD_VULNERABLE.into(); f.quantum_core_installed = true; f.fuel_blocks = 2_000;
        f.shield_hp = 10.0; f.armor_hp = 10.0; f.hull_hp = 50_000.0; f.reinforced_until_unix = 0;
    });
    let flex_svc = grant(&ctx, OWNER, flex_id, 35894, InventoryFlag::ServiceSlot0, 1);
    ctx.db.with_mut_snapshot(|snap| snap.module_online_states.insert(flex_svc, true));
    prepare_siege(&ctx, flex_id, 20_000.0);
    let flex_now = ctx.now_unix();
    fire_weapon(&ctx, &mut sess, flex_id).await.expect("flex shield -> armor");
    fire_weapon(&ctx, &mut sess, flex_id).await.expect("flex armor -> hull reinforced");
    let flex_st = observe(&ctx, &mut sess, flex_id).await.unwrap();
    assert_eq!(flex_st.state, life::STATE_HULL_REINFORCED);
    assert!(flex_st.reinforced_until_unix > flex_now, "FLEX negative jitter must never place reinforced_until_unix in the past");

    // Return session to JITA for section 5 Data Analyzer hack reveal test.
    sess.solar_system_id = Some(JITA);
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&OWNER).unwrap().solar_system_id = JITA;
        snap.structures.get_mut(&st_id).unwrap().owner_corp_id = my_corp;
        snap.structure_vulnerability_windows.insert(st_id, (21, 3));
        let mut ext = load_ext_state(snap);
        ext.pending_reinforce_schedule.insert(st_id, (14, 2, 1_850_000_000));
        let prof_rec = ext.access_profiles.entry(st_id).or_default();
        prof_rec.corp_only = false;
        prof_rec.allowed_characters.extend([HACKER, BYSTANDER]);
        save_ext_state(snap, &ext);
    });
    let owner_info = rpc(&ctx, &mut sess, "structureDirectory", "GetStructureInfo", &[PyRep::Integer(st_id)]).await.expect("owner info");
    assert_eq!(owner_info.dict_get("reinforceHour"), Some(&PyRep::Integer(21)));
    assert_eq!(owner_info.dict_get("nextReinforceHour"), Some(&PyRep::Integer(14)));

    let mut hacker_sess = SessionState::new(7104);
    hacker_sess.user_id = Some(2); hacker_sess.character_id = Some(HACKER); hacker_sess.corporation_id = Some(1000009); hacker_sess.solar_system_id = Some(JITA);
    let hacker_ship = grant(&ctx, HACKER, i64::from(JITA), 606, InventoryFlag::None, 1);
    hacker_sess.ship_id = Some(hacker_ship);
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.get(&OWNER).unwrap().clone();
        ch.character_id = HACKER; ch.user_id = 2; ch.name = "Structure Hacker".into(); ch.corporation_id = 1000009; ch.solar_system_id = JITA; ch.station_id = None; ch.structure_id = None; ch.active_ship_id = hacker_ship;
        snap.characters.insert(HACKER, ch);
        let mut cb = snap.characters.get(&OWNER).unwrap().clone();
        cb.character_id = BYSTANDER; cb.user_id = 3; cb.name = "Structure Bystander".into(); cb.corporation_id = 1000009; cb.solar_system_id = JITA; cb.station_id = None; cb.structure_id = None;
        snap.characters.insert(BYSTANDER, cb);
        snap.skills.entry(HACKER).or_default().insert(21718, eve_db::TrainedSkillRecord { type_id: 21718, level: 5, skill_points: 256_000.0 });
    });
    let _ = ctx.db.acquire_online_lock_with_computer_hash(2, 7104, Some(HACKER), None, true, ctx.now_unix());
    let _ = ctx.db.acquire_online_lock_with_computer_hash(3, 7105, Some(BYSTANDER), None, true, ctx.now_unix());
    let st_pos = DVec3::from_array(observe(&ctx, &mut sess, st_id).await.unwrap().position);
    let jita_sys = ctx.systems.get_or_spawn(JITA);
    let _ = jita_sys.spawn_ball(eve_destiny::Ball { id: hacker_ship, type_id: 606, position: st_pos + DVec3::new(1_000.0, 0.0, 0.0), velocity: DVec3::ZERO, radius: 40.0, mass: 1_000_000.0, mode: eve_destiny::MovementMode::Stop, speed_fraction: 0.0, max_velocity: 300.0, inertia: 1.0 }).await;
    let _ = jita_sys.spawn_ball(eve_destiny::Ball { id: st_id, type_id: 35832, position: st_pos, velocity: DVec3::ZERO, radius: 1_000.0, mass: 1.0e9, mode: eve_destiny::MovementMode::Stop, speed_fraction: 0.0, max_velocity: 0.0, inertia: 1.0 }).await;

    move_ship(&ctx, &sess, st_pos + DVec3::new(900.0, 0.0, 0.0)).await;
    let owner_data_mod = grant(&ctx, OWNER, sess.ship_id.unwrap(), 22175, InventoryFlag::MedSlot0, 1);
    assert_eq!(rpc(&ctx, &mut sess, "hackingMgr", "StartNewGameInstance", &[PyRep::Integer(st_id)]).await.expect_err("self hack").code, "CannotHackOwnStructure");
    ctx.db.with_mut_snapshot(|snap| { snap.items.remove(&owner_data_mod); });

    let pre_info = rpc(&ctx, &mut hacker_sess, "structureDirectory", "GetStructureInfo", &[PyRep::Integer(st_id)]).await.expect("pre info");
    assert_eq!(pre_info.dict_get("reinforceHour"), Some(&PyRep::None));
    let relic_mod = grant(&ctx, HACKER, hacker_ship, 22177, InventoryFlag::MedSlot0, 1);
    assert!(matches!(rpc(&ctx, &mut hacker_sess, "hackingMgr", "StartNewGameInstance", &[PyRep::Integer(st_id)]).await.expect_err("relic").code.as_str(), "AnalyzerModuleRequired" | "UserError"));
    ctx.db.with_mut_snapshot(|snap| { snap.items.remove(&relic_mod); });

    let _data_mod = grant(&ctx, HACKER, hacker_ship, 22175, InventoryFlag::MedSlot0, 1);
    async fn run_structure_hack(ctx: &ServerContext, sess: &mut SessionState, st_id: i64) -> PyRep {
        assert_eq!(kv_str(&rpc(ctx, sess, "hackingMgr", "StartNewGameInstance", &[PyRep::Integer(st_id)]).await.expect("start hack"), "status"), "InProgress");
        for _pass in 0..8 {
            for tile in 1..24 {
                for _ in 0..5 {
                    if let Ok(step) = rpc(ctx, sess, "hackingMgr", "ClickedOnTile", &[PyRep::Integer(st_id), PyRep::Integer(tile)]).await {
                        if kv_str(&step, "status") == "Won" { return step; }
                    } else { break; }
                }
            }
        }
        panic!("Data Analyzer hack must complete with Won");
    }
    let won = run_structure_hack(&ctx, &mut hacker_sess, st_id).await;
    assert_eq!(won.dict_get("reinforceHour"), Some(&PyRep::Integer(21)));
    assert_eq!(won.dict_get("nextReinforceHour"), Some(&PyRep::Integer(14)));

    let hacker_evts = ctx.drain_notifications_for_char(HACKER);
    let PyRep::Tuple(hack_tuple) = &hacker_evts.iter().find(|n| n.event == "OnStructureHacked").expect("OnStructureHacked").args[1] else { panic!("tuple") };
    assert_eq!(hack_tuple, &[PyRep::Integer(3), PyRep::Integer(21), PyRep::Integer(2), PyRep::Integer(14), PyRep::Integer(1_850_000_000)]);

    ctx.db.with_mut_snapshot(|snap| { let mut ext = load_ext_state(snap); ext.pending_reinforce_schedule.remove(&st_id); save_ext_state(snap, &ext); });
    let _ = run_structure_hack(&ctx, &mut hacker_sess, st_id).await;
    let hacker_evts2 = ctx.drain_notifications_for_char(HACKER);
    let PyRep::Tuple(nopending_tuple) = &hacker_evts2.iter().find(|n| n.event == "OnStructureHacked").unwrap().args[1] else { panic!("tuple") };
    assert_eq!(nopending_tuple, &[PyRep::Integer(3), PyRep::Integer(21), PyRep::None, PyRep::None, PyRep::None]);

    let mut bystander_sess = SessionState::new(7105);
    bystander_sess.user_id = Some(3); bystander_sess.character_id = Some(BYSTANDER); bystander_sess.corporation_id = Some(1000009); bystander_sess.solar_system_id = Some(JITA);
    let bystander_info = rpc(&ctx, &mut bystander_sess, "structureDirectory", "GetStructureInfo", &[PyRep::Integer(st_id)]).await.expect("bystander info");
    assert_eq!(bystander_info.dict_get("reinforceHour"), Some(&PyRep::None));
    rpc(&ctx, &mut sess, "structureVulnerability", "UpdateReinforceTimesFromClient", &[PyRep::Integer(st_id), PyRep::Integer(19), PyRep::Integer(4)]).await.expect("update schedule");
    assert!(!ctx.drain_notifications_for_char(BYSTANDER).iter().any(|n| n.event == "OnStructureHacked"));
}

}


#[allow(unused_imports, dead_code, unused_variables, unused_mut)]
mod plan_upwell_structures_ops_b {
    use super::*;

// Upwell Structure Clone Bay, Service Modules, Repairshop, Access Profiles, Directory,
// Fitting & Hull Types parity tests (`plan_upwell_structures_closure_part2.rs`).

use eve_db::{InventoryFlag, InventoryItemRecord, UpwellStructureRecord};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::attr_ids;
use eve_server::router::ServerContext;
use eve_server::services::station_structure::structure_lifecycle as life;
use glam::DVec3;
use std::collections::HashMap;

const OWNER: i64 = 90_000_001;
const GUEST: i64 = 90_000_002;
const OUTSIDER: i64 = 90_000_003;
const CORP_GRUNT: i64 = 90_000_004;
const JITA: u32 = 30_000_144;
const JITA_44: i64 = 60_000_358;

async fn rpc(ctx: &ServerContext, sess: &mut SessionState, service: &str, method: &str, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None).await
}

async fn inv_add(ctx: &ServerContext, sess: &mut SessionState, item_id: i64, target_loc: i64, flag: InventoryFlag, online: bool) -> Result<PyRep, BridgeRpcError> {
    let mut kw = HashMap::new();
    kw.insert("online".into(), PyRep::Bool(online));
    ctx.dispatch_rpc(sess, "invbroker", None, "Add", &[PyRep::Integer(item_id), PyRep::Integer(target_loc), PyRep::Integer(flag as i64)], &kw, None).await
}

fn kv_i64(rep: &PyRep, key: &str) -> i64 { rep.dict_get(key).and_then(|v| v.as_i64()).unwrap_or(0) }
fn kv_str(rep: &PyRep, key: &str) -> String { rep.dict_get(key).and_then(|v| v.as_str()).unwrap_or("").to_string() }
fn kv_bool(rep: &PyRep, key: &str) -> bool { rep.dict_get(key).and_then(|v| v.as_bool()).unwrap_or(false) }

fn grant(ctx: &ServerContext, owner: i64, location: i64, type_id: u32, flag: InventoryFlag, qty: i32) -> i64 {
    ctx.db.with_mut_snapshot(|snap| {
        snap.next_entity_id += 1;
        let id = snap.next_entity_id;
        snap.items.insert(id, InventoryItemRecord { item_id: id, type_id, owner_id: owner, location_id: location, flag, quantity: qty, singleton: qty == 1, version: 1, ..Default::default() });
        id
    })
}

fn attr(ctx: &ServerContext, type_id: u32, attr_id: u16) -> f64 {
    ctx.sde.types.get(&type_id).and_then(|t| t.default_attributes.get(&attr_id).copied()).unwrap_or_else(|| panic!("SDE type {type_id} lacks attribute {attr_id}"))
}

fn new_test_ctx() -> ServerContext {
    let ctx = ServerContext::new();
    ctx.set_clock(std::sync::Arc::new(eve_db::VirtualServerClock::with_fixed_base(1_800_000_000)));
    ctx
}

async fn pilot_in_space(ctx: &ServerContext, sess_id: u64) -> SessionState {
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&OWNER) {
            let ship_id = ch.active_ship_id;
            ch.solar_system_id = JITA;
            ch.station_id = Some(JITA_44);
            if let Some(sh) = snap.items.get_mut(&ship_id) {
                sh.location_id = JITA_44;
            }
        }
    });
    let mut sess = SessionState::new(sess_id);
    rpc(ctx, &mut sess, "authentication", "Login", &[PyRep::String("demo".into()), PyRep::String("demo-secret".into())]).await.expect("Login");
    rpc(ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(OWNER)]).await.expect("Select");
    ctx.advance_secs(12);
    rpc(ctx, &mut sess, "corpRegistry", "AddCorporation", &[PyRep::String("Upwell Services Corp".into()), PyRep::String("USVC".into())]).await.expect("AddCorporation");
    ctx.advance_virtual_clock(7 * 86_400 + 12);
    rpc(ctx, &mut sess, "structureDocking", "Undock", &[]).await.expect("Undock");
    sess
}

async fn move_ship(ctx: &ServerContext, sess: &SessionState, pos: DVec3) {
    assert!(ctx.systems.get_or_spawn(sess.solar_system_id.expect("sys")).set_ball_position(sess.ship_id.expect("ship"), pos).await);
}

fn open_space(ctx: &ServerContext, sys_id: u32, k: f64) -> DVec3 {
    let base = ctx.sde.stations.values().find(|s| s.solar_system_id == sys_id).map(|s| s.position).unwrap_or(DVec3::ZERO);
    base + DVec3::new(0.0, 5.0e9 + k * 3.0e9, 0.0)
}

async fn anchor(ctx: &ServerContext, sess: &mut SessionState, type_id: u32, name: &str) -> Result<PyRep, BridgeRpcError> {
    let ship = sess.ship_id.expect("ship");
    grant(ctx, OWNER, ship, type_id, InventoryFlag::Cargo, 1);
    rpc(ctx, sess, "structureDeployment", "Anchor", &[PyRep::Integer(i64::from(type_id)), PyRep::String(name.into())]).await
}

async fn observe(ctx: &ServerContext, sess: &mut SessionState, id: i64) -> Option<UpwellStructureRecord> {
    let _ = rpc(ctx, sess, "structureGuests", "GetGuests", &[PyRep::Integer(id)]).await;
    ctx.db.with_snapshot(|snap| snap.structures.get(&id).cloned())
}

async fn online_structure(ctx: &ServerContext, sess: &mut SessionState, type_id: u32, name: &str) -> i64 {
    let id = kv_i64(&anchor(ctx, sess, type_id, name).await.expect("Anchor"), "structureID");
    ctx.advance_virtual_clock(life::ANCHOR_VULNERABLE_SECS + life::ANCHORING_SECS);
    assert_eq!(observe(ctx, sess, id).await.expect("st").state, life::STATE_NO_CORE);
    let core = attr(ctx, type_id, life::ATTR_REQUIRES_DEED_TYPE) as u32;
    move_ship(ctx, sess, DVec3::from_array(ctx.db.with_snapshot(|s| s.structures[&id].position))).await;
    rpc(ctx, sess, "structureDocking", "Dock", &[PyRep::Integer(id)]).await.expect("Dock");
    let core_item = grant(ctx, OWNER, sess.ship_id.expect("ship"), core, InventoryFlag::Cargo, 1);
    rpc(ctx, sess, "invbroker", "Add", &[PyRep::Integer(core_item), PyRep::Integer(id), PyRep::Integer(172)]).await.expect("move core");
    assert_eq!(observe(ctx, sess, id).await.expect("st").state, life::STATE_ONLINING);
    ctx.advance_virtual_clock(life::ONLINING_SECS);
    assert_eq!(observe(ctx, sess, id).await.expect("st").state, life::STATE_SHIELD_VULNERABLE);
    id
}

#[tokio::test]
async fn test_upwell_services_clone_bay_modules_and_repairshop() {
    // parity: feat:structures.service-clone-bay, feat:structures.service-modules
    eve_testkit::attest(&[
        "feat:structures.service-clone-bay",
        "feat:structures.service-modules",
    ]);

    let ctx = new_test_ctx();
    let mut sess = pilot_in_space(&ctx, 7203).await;
    let st_pos = open_space(&ctx, JITA, 2.0);
    move_ship(&ctx, &sess, st_pos).await;
    let st_id = online_structure(&ctx, &mut sess, 35832, "Services Astrahus").await;
    let ship_id = sess.ship_id.unwrap();

    // 1. Service module fitting via live invbroker.Add RPC, fuel-gated activation via invbroker.Add (flag 133), and 1-arg OnStructureServiceChanged.
    let clone_mod = grant(&ctx, OWNER, ship_id, 35894, InventoryFlag::Cargo, 1);
    let fit_res = inv_add(&ctx, &mut sess, clone_mod, st_id, InventoryFlag::ServiceSlot0, false).await.expect("FitModule via invbroker.Add");
    assert!(!kv_bool(&fit_res, "online"));
    assert_eq!(rpc(&ctx, &mut sess, "dogmaIM", "SetModuleOnline", &[PyRep::Integer(st_id), PyRep::Integer(clone_mod)]).await.expect_err("no fuel").code, "InsufficientStructureFuelToOnline");

    let online_fuel = (attr(&ctx, 35894, life::ATTR_SERVICE_FUEL_ONLINE) * 0.75).round() as i32;
    let hourly_fuel = (attr(&ctx, 35894, life::ATTR_SERVICE_FUEL_PER_HOUR) * 0.75).round() as i32;
    let short_fuel = grant(&ctx, OWNER, ship_id, 4247, InventoryFlag::Cargo, online_fuel - 1);
    rpc(&ctx, &mut sess, "invbroker", "Add", &[PyRep::Integer(short_fuel), PyRep::Integer(st_id), PyRep::Integer(133), PyRep::Integer(i64::from(online_fuel - 1))]).await.expect("short fuel");
    assert_eq!(rpc(&ctx, &mut sess, "dogmaIM", "SetModuleOnline", &[PyRep::Integer(st_id), PyRep::Integer(clone_mod)]).await.expect_err("539 < 540").code, "InsufficientStructureFuelToOnline");
    let fuel_item = grant(&ctx, OWNER, ship_id, 4247, InventoryFlag::Cargo, 1 + hourly_fuel * 2);
    rpc(&ctx, &mut sess, "invbroker", "Add", &[PyRep::Integer(fuel_item), PyRep::Integer(st_id), PyRep::Integer(133), PyRep::Integer(i64::from(1 + hourly_fuel * 2))]).await.expect("Fuel bay via invbroker.Add");
    let _ = ctx.drain_notifications_for_char(OWNER);
    let on_res = rpc(&ctx, &mut sess, "dogmaIM", "SetModuleOnline", &[PyRep::Integer(st_id), PyRep::Integer(clone_mod)]).await.expect("OnlineModule");
    assert!(kv_bool(&on_res, "online"));
    assert_eq!(observe(&ctx, &mut sess, st_id).await.unwrap().fuel_blocks, (hourly_fuel * 2) as u32, "SetModuleOnline must deduct 540 startup fuel blocks");
    let reproc_mod = grant(&ctx, OWNER, ship_id, 35899, InventoryFlag::Cargo, 1);
    inv_add(&ctx, &mut sess, reproc_mod, st_id, InventoryFlag::ServiceSlot1, false).await.expect("fit reproc");
    assert_eq!(rpc(&ctx, &mut sess, "dogmaIM", "SetModuleOnline", &[PyRep::Integer(st_id), PyRep::Integer(reproc_mod)]).await.expect_err("16 < 2160").code, "InsufficientStructureFuelToOnline");
    assert_eq!(rpc(&ctx, &mut sess, "structureSettings", "CharacterHasService", &[PyRep::Integer(st_id), PyRep::Integer(10)]).await.expect("has service"), PyRep::Bool(true));
    assert!(ctx.drain_notifications_for_char(OWNER).into_iter().any(|n| n.event == "OnStructureServiceChanged" && n.args == vec![PyRep::Integer(st_id)]), "OnStructureServiceChanged must emit exact 1-arg [structure_id]");

    // 2. Clone Bay: jump clone + medical home station + live pod-death respawn into Upwell Clone Bay structure!
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(OWNER).or_default().insert(24242, eve_db::TrainedSkillRecord { type_id: 24242, level: 5, skill_points: 256_000.0 });
    });
    assert!(kv_i64(&rpc(&ctx, &mut sess, "jumpCloneSvc", "InstallCloneInStructure", &[]).await.expect("InstallCloneInStructure"), "jumpCloneID") > 0);
    assert_eq!(rpc(&ctx, &mut sess, "charMgr", "SetHomeStation", &[PyRep::Integer(st_id)]).await.expect("SetHomeStation").as_i64(), Some(st_id));

    // Leave ship in structure hangar, undock in capsule, self-destruct capsule in space -> respawns INSIDE st_id!
    rpc(&ctx, &mut sess, "ship", "LeaveShip", &[]).await.expect("LeaveShip in structure");
    ctx.advance_secs(12);
    rpc(&ctx, &mut sess, "structureDocking", "Undock", &[]).await.expect("Undock capsule");
    rpc(&ctx, &mut sess, "ship", "SelfDestruct", &[]).await.expect("Start self-destruct");
    ctx.advance_virtual_clock(125);
    assert_eq!(rpc(&ctx, &mut sess, "charMgr", "GetHomeStation", &[]).await.expect("Sync session after pod self-destruct").as_i64(), Some(st_id));
    assert_eq!(sess.structure_id, Some(st_id), "Pod death with online Clone Bay must respawn pilot inside structure");
    assert_eq!(sess.station_id, None);
    ctx.db.with_snapshot(|snap| {
        let ch = snap.characters.get(&OWNER).unwrap();
        assert_eq!((ch.structure_id, ch.station_id, ch.home_station_id), (Some(st_id), None, st_id));
        assert_eq!(snap.items.get(&ch.active_ship_id).map(|i| i.location_id), Some(st_id));
    });
    ctx.advance_secs(12);
    rpc(&ctx, &mut sess, "ship", "Board", &[PyRep::Integer(ship_id)]).await.expect("Re-board ship in structure");

    // Advance 100 hours so fuel runs dry; Clone Bay goes offline, structure drops to LowPower, and pod death falls back to starter school NPC station!
    ctx.advance_virtual_clock(100 * 3_600);
    let st_low = observe(&ctx, &mut sess, st_id).await.unwrap();
    assert!(st_low.fuel_blocks < hourly_fuel as u32);
    assert!(matches!(rpc(&ctx, &mut sess, "jumpCloneSvc", "InstallCloneInStructure", &[]).await.expect_err("offline jc").code.as_str(), "NoCloneBayService" | "CloneBayServiceOffline"));
    assert_eq!(rpc(&ctx, &mut sess, "charMgr", "SetHomeStation", &[PyRep::Integer(st_id)]).await.expect_err("offline home").code, "CloneBayServiceOffline");
    assert_eq!(rpc(&ctx, &mut sess, "structureSettings", "CharacterHasService", &[PyRep::Integer(st_id), PyRep::Integer(2)]).await.expect("svc off"), PyRep::Bool(false));

    rpc(&ctx, &mut sess, "ship", "LeaveShip", &[]).await.expect("LeaveShip for offline pod test");
    ctx.advance_secs(12);
    rpc(&ctx, &mut sess, "structureDocking", "Undock", &[]).await.expect("Undock capsule offline");
    rpc(&ctx, &mut sess, "ship", "SelfDestruct", &[]).await.expect("Start self-destruct 2");
    ctx.advance_virtual_clock(125);
    let _ = rpc(&ctx, &mut sess, "charMgr", "GetHomeStation", &[]).await.expect("Sync session after offline Clone Bay self-destruct");
    assert_eq!(sess.structure_id, None, "Offline Clone Bay must not respawn pilot inside structure");
    let fallback_station = sess.station_id.expect("Fallback starter school NPC station");
    assert!(fallback_station < 1_000_000_000_000);
    ctx.db.with_snapshot(|snap| assert_eq!(snap.characters.get(&OWNER).unwrap().home_station_id, fallback_station, "home_station_id reset to starter school station"));

    // Return pilot to st_id and re-board ship_id for repairSvc tests.
    let pod_after = sess.ship_id.unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&OWNER).unwrap();
        ch.station_id = None; ch.structure_id = Some(st_id); ch.solar_system_id = JITA;
        snap.items.get_mut(&pod_after).unwrap().location_id = st_id;
    });
    sess.station_id = None; sess.structure_id = Some(st_id); sess.solar_system_id = Some(JITA);
    ctx.advance_secs(12);
    rpc(&ctx, &mut sess, "ship", "Board", &[PyRep::Integer(ship_id)]).await.expect("Board ship_id");

    // 3. repairSvc: LowPower/Abandoned/NoCore/ACL gates, undocked hangar rejection, container rejection, CorpSAG role gate, 100% owner-corp tax, and free 0% repair!
    let mod_id = grant(&ctx, OWNER, ship_id, 3170, InventoryFlag::HiSlot0, 1);
    let drone_id = grant(&ctx, OWNER, ship_id, 2454, InventoryFlag::DroneBay, 1);
    let owner_corp_id = ctx.db.with_snapshot(|snap| snap.structures.get(&st_id).unwrap().owner_corp_id);
    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = eve_server::services::station_structure::structure_upwell_ops::load_ext_state(snap);
        ext.access_profiles.entry(st_id).or_default().service_settings.insert(13, 500);
        eve_server::services::station_structure::structure_upwell_ops::save_ext_state(snap, &ext);
        snap.characters.get_mut(&OWNER).unwrap().balance = eve_db::IskCents::from_f64_isk(50_000_000.0);
        snap.corporations.get_mut(&owner_corp_id).unwrap().set_division_balance(1, eve_db::IskCents::from_f64_isk(1_000_000.0));
        let sh = snap.items.get_mut(&ship_id).unwrap();
        sh.location_id = st_id;
        sh.dynamic_attributes.insert(attr_ids::DAMAGE, 50.0);
        sh.dynamic_attributes.insert(attr_ids::ARMOR_DAMAGE, 100.0);
        snap.items.get_mut(&mod_id).unwrap().dynamic_attributes.insert(attr_ids::DAMAGE, 20.0);
        snap.items.get_mut(&drone_id).unwrap().dynamic_attributes.insert(attr_ids::DAMAGE, 15.0);
        snap.items.get_mut(&drone_id).unwrap().dynamic_attributes.insert(attr_ids::ARMOR_DAMAGE, 25.0);
    });

    // While st_id is LowPower (unfueled, with Quantum Core installed), Repair Shop remains available!
    let lp_quotes = rpc(&ctx, &mut sess, "repairSvc", "GetRepairQuotes", &[PyRep::List(vec![PyRep::Integer(ship_id)])]).await.expect("low power quote succeeds with core");
    assert!(matches!(lp_quotes, PyRep::Dict(ref d) if !d.is_empty()), "LowPower structure with Quantum Core must provide Repair Shop quotes");

    // NaN/negative budget, Abandoned, and QuantumCoreRequired rejections.
    assert_eq!(rpc(&ctx, &mut sess, "repairSvc", "RepairItemsInStructure", &[PyRep::List(vec![PyRep::Integer(ship_id)]), PyRep::Real(f64::NAN)]).await.expect_err("NaN").code, "InvalidArgument");
    assert_eq!(rpc(&ctx, &mut sess, "repairSvc", "RepairItemsInStructure", &[PyRep::List(vec![PyRep::Integer(ship_id)]), PyRep::Real(-5.0)]).await.expect_err("neg").code, "InvalidArgument");
    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = eve_server::services::station_structure::structure_upwell_ops::load_ext_state(snap);
        ext.lifecycle.entry(st_id).or_default().low_power_since_unix = Some(1);
        eve_server::services::station_structure::structure_upwell_ops::save_ext_state(snap, &ext);
    });
    assert_eq!(rpc(&ctx, &mut sess, "repairSvc", "RepairItemsInStructure", &[PyRep::List(vec![PyRep::Integer(ship_id)])]).await.expect_err("abandoned").code, "StructureAbandoned");
    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = eve_server::services::station_structure::structure_upwell_ops::load_ext_state(snap);
        ext.lifecycle.entry(st_id).or_default().low_power_since_unix = None;
        eve_server::services::station_structure::structure_upwell_ops::save_ext_state(snap, &ext);
        snap.structures.get_mut(&st_id).unwrap().quantum_core_installed = false;
    });
    assert_eq!(rpc(&ctx, &mut sess, "repairSvc", "RepairItemsInStructure", &[PyRep::List(vec![PyRep::Integer(ship_id)])]).await.expect_err("no core").code, "QuantumCoreRequired");
    ctx.db.with_mut_snapshot(|snap| snap.structures.get_mut(&st_id).unwrap().quantum_core_installed = true);

    // Refuel st_id and re-online clone_mod so st_id is FullPower!
    let refuel_item = grant(&ctx, OWNER, ship_id, 4247, InventoryFlag::Cargo, 5_000);
    rpc(&ctx, &mut sess, "invbroker", "Add", &[PyRep::Integer(refuel_item), PyRep::Integer(st_id), PyRep::Integer(133), PyRep::Integer(5_000)]).await.expect("refuel");
    rpc(&ctx, &mut sess, "dogmaIM", "SetModuleOnline", &[PyRep::Integer(st_id), PyRep::Integer(clone_mod)]).await.expect("re-online clone bay");

    // ACL-blocked outsider, undocked hangar item, container-nested item, and restricted CorpSAG2 without role are all rejected.
    let out_ship = grant(&ctx, OUTSIDER, st_id, 606, InventoryFlag::Hangar, 1);
    let mut out_sess = SessionState::new(7209);
    out_sess.user_id = Some(3); out_sess.character_id = Some(OUTSIDER); out_sess.corporation_id = Some(1000009); out_sess.solar_system_id = Some(JITA); out_sess.structure_id = Some(st_id); out_sess.ship_id = Some(out_ship);
    let box_id = grant(&ctx, OWNER, st_id, 17366, InventoryFlag::Hangar, 1);
    let boxed_mod = grant(&ctx, OWNER, box_id, 3170, InventoryFlag::Cargo, 1);
    let corp_div2_mod = grant(&ctx, owner_corp_id, st_id, 3170, InventoryFlag::CorpSAG2, 1);
    let mut grunt_rep_sess = SessionState::new(7210);
    grunt_rep_sess.user_id = Some(4); grunt_rep_sess.character_id = Some(CORP_GRUNT); grunt_rep_sess.corporation_id = Some(owner_corp_id); grunt_rep_sess.solar_system_id = Some(JITA); grunt_rep_sess.structure_id = Some(st_id); grunt_rep_sess.corp_role = 0;
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.get(&OWNER).unwrap().clone();
        ch.character_id = OUTSIDER; ch.corporation_id = 1000009; ch.active_ship_id = out_ship;
        snap.characters.insert(OUTSIDER, ch);
        let mut cg = snap.characters.get(&OWNER).unwrap().clone();
        cg.character_id = CORP_GRUNT; cg.corporation_id = owner_corp_id; cg.structure_id = Some(st_id);
        snap.characters.insert(CORP_GRUNT, cg);
        snap.corporations.get_mut(&owner_corp_id).unwrap().members.insert(CORP_GRUNT);
        snap.structure_docking_acl_denied.entry(st_id).or_default().insert(OUTSIDER);
        for iid in [out_ship, boxed_mod, corp_div2_mod] {
            snap.items.get_mut(&iid).unwrap().dynamic_attributes.insert(attr_ids::DAMAGE, 10.0);
        }
    });
    assert_eq!(rpc(&ctx, &mut out_sess, "repairSvc", "RepairItemsInStructure", &[PyRep::List(vec![PyRep::Integer(out_ship)])]).await.expect_err("acl blocked").code, "StructureRepairAccessDenied");
    assert_eq!(rpc(&ctx, &mut sess, "repairSvc", "RepairItemsInStructure", &[PyRep::List(vec![PyRep::Integer(boxed_mod)])]).await.expect_err("inside container").code, "ItemInsideContainer");
    assert_eq!(rpc(&ctx, &mut grunt_rep_sess, "repairSvc", "RepairItemsInStructure", &[PyRep::List(vec![PyRep::Integer(corp_div2_mod)])]).await.expect_err("corp role denied").code, "CorpRoleDenied");
    let hangar_mod = grant(&ctx, OWNER, st_id, 3170, InventoryFlag::Hangar, 1);
    ctx.db.with_mut_snapshot(|snap| { snap.items.get_mut(&hangar_mod).unwrap().dynamic_attributes.insert(attr_ids::DAMAGE, 10.0); });
    rpc(&ctx, &mut sess, "structureDocking", "Undock", &[]).await.expect("Undock to test tethered hangar repair rejection");
    move_ship(&ctx, &sess, DVec3::from_array(ctx.db.with_snapshot(|s| s.structures[&st_id].position)) + DVec3::new(500.0, 0.0, 0.0)).await;
    let _ = ctx.systems.get_or_spawn(JITA).step_manual(1.0).await;
    if let Ok(mut bp) = ctx.systems.get_or_spawn(JITA).ballpark.write() { bp.combat_states.entry(ship_id).or_default().is_tethered = true; }
    assert_eq!(rpc(&ctx, &mut sess, "repairSvc", "RepairItemsInStructure", &[PyRep::List(vec![PyRep::Integer(hangar_mod)])]).await.expect_err("undocked hangar repair").code, "NotDockedInStructure");
    ctx.advance_secs(12);
    rpc(&ctx, &mut sess, "structureDocking", "Dock", &[PyRep::Integer(st_id)]).await.expect("Re-dock after tethered hangar check");
    ctx.db.with_mut_snapshot(|snap| {
        let sh = snap.items.get_mut(&ship_id).unwrap();
        sh.dynamic_attributes.insert(attr_ids::DAMAGE, 50.0);
        sh.dynamic_attributes.insert(attr_ids::ARMOR_DAMAGE, 100.0);
    });

    // At 5% structure repair tax: baseCost is 0.0, totalRepairCost == serviceCharge > 0.0, and 100% of paid ISK credits owner corp division 1!
    let quotes = rpc(&ctx, &mut sess, "repairSvc", "GetRepairQuotes", &[PyRep::List(vec![PyRep::Integer(ship_id)])]).await.expect("GetRepairQuotes");
    let PyRep::Dict(q_entries) = quotes else { panic!("expected dict") };
    let (_, PyRep::List(q_rows)) = &q_entries[0] else { panic!("expected quote list") };
    for iid in [ship_id, mod_id, drone_id] {
        assert!(q_rows.iter().any(|r| kv_i64(r, "itemID") == iid && r.dict_get("baseCost").and_then(|v| v.as_f64()) == Some(0.0) && r.dict_get("totalRepairCost").and_then(|v| v.as_f64()).unwrap_or(0.0) > 0.0));
    }
    let expected_tax_isk: f64 = q_rows.iter().filter_map(|r| r.dict_get("serviceCharge").and_then(|v| v.as_f64())).sum();
    assert!(expected_tax_isk > 0.0);

    let jita_sys = ctx.systems.get_or_spawn(JITA);
    if let Ok(mut bp) = jita_sys.ballpark.write() {
        let cs = bp.combat_states.entry(ship_id).or_insert_with(eve_destiny::CombatState::default);
        cs.max_shield_hp = 500.0; cs.shield_hp = 50.0;
    }
    let rep_res = rpc(&ctx, &mut sess, "repairSvc", "RepairItemsInStructure", &[PyRep::List(vec![PyRep::Integer(ship_id)])]).await.expect("RepairItemsInStructure");
    let paid = rep_res.dict_get("totalCostISK").and_then(|v| v.as_f64()).unwrap_or(0.0);
    assert!((paid - expected_tax_isk).abs() < 1e-2, "Upwell repair must charge only the structure owner tax");
    ctx.db.with_snapshot(|snap| {
        for iid in [ship_id, mod_id, drone_id] {
            let it = snap.items.get(&iid).unwrap();
            assert!(!it.dynamic_attributes.contains_key(&attr_ids::DAMAGE) && !it.dynamic_attributes.contains_key(&attr_ids::ARMOR_DAMAGE));
        }
        assert!((snap.characters.get(&OWNER).unwrap().balance.to_wire_f64() - (50_000_000.0 - paid)).abs() < 1e-2);
        let corp = snap.corporations.get(&owner_corp_id).unwrap();
        assert!((corp.division_balance(1).to_wire_f64() - (1_000_000.0 + paid)).abs() < 1e-2, "100% of structure repair fee credited to owner corp");
        let cj = snap.corp_wallet_journals.get(&(owner_corp_id, 1)).expect("corp div 1 journal");
        assert!(cj.iter().any(|e| e.ref_type == "RepairBill" && (e.amount - paid).abs() < 1e-2));
    });

    // At 0% structure repair tax: Upwell repair quote AND repair execution are completely FREE (0.0 ISK)!
    let corp_bal_before_zero = ctx.db.with_snapshot(|snap| snap.corporations.get(&owner_corp_id).unwrap().division_balance(1).to_wire_f64());
    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = eve_server::services::station_structure::structure_upwell_ops::load_ext_state(snap);
        ext.access_profiles.entry(st_id).or_default().service_settings.insert(13, 0);
        eve_server::services::station_structure::structure_upwell_ops::save_ext_state(snap, &ext);
        snap.items.get_mut(&ship_id).unwrap().dynamic_attributes.insert(attr_ids::DAMAGE, 30.0);
        snap.characters.get_mut(&OWNER).unwrap().balance = eve_db::IskCents::from_f64_isk(40_000_000.0);
    });
    let base_quotes = rpc(&ctx, &mut sess, "repairSvc", "GetRepairQuotes", &[PyRep::List(vec![PyRep::Integer(ship_id)])]).await.expect("0% quotes");
    let PyRep::Dict(bq_entries) = base_quotes else { panic!("expected dict") };
    let (_, PyRep::List(bq_rows)) = &bq_entries[0] else { panic!("expected list") };
    assert_eq!(bq_rows[0].dict_get("totalRepairCost").and_then(|v| v.as_f64()), Some(0.0), "0% tax Upwell repair quote is 0.0 ISK");
    let free_rep = rpc(&ctx, &mut sess, "repairSvc", "RepairItemsInStructure", &[PyRep::List(vec![PyRep::Integer(ship_id)])]).await.expect("0% repair");
    assert_eq!(free_rep.dict_get("totalCostISK").and_then(|v| v.as_f64()), Some(0.0), "0% tax Upwell repair costs 0.0 ISK");
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.get(&ship_id).unwrap().dynamic_attributes.contains_key(&attr_ids::DAMAGE));
        assert!((snap.characters.get(&OWNER).unwrap().balance.to_wire_f64() - 40_000_000.0).abs() < 1e-4);
        assert!((snap.corporations.get(&owner_corp_id).unwrap().division_balance(1).to_wire_f64() - corp_bal_before_zero).abs() < 1e-4);
    });
}

#[tokio::test]
async fn test_upwell_profiles_directory_fitting_and_types() {
    // parity: feat:structures.structure-fitting, feat:structures.structure-types
    eve_testkit::attest(&[
        "feat:structures.structure-fitting",
        "feat:structures.structure-types",
    ]);

    let ctx = new_test_ctx();
    let mut sess = pilot_in_space(&ctx, 7204).await;

    // 1. Structure types & SDE profiles across Citadels, Engineering Complexes, Refineries, FLEX, Faction Fortizars, and Palatine Keepstar.
    for (tid, name, exp_sh, exp_ar, exp_hu, hi, med, low, svc, rig) in [
        (35832u32, "Astrahus", 3600000.0, 1800000.0, 1800000.0, 4, 4, 3, 3, 3),
        (35833, "Fortizar", 6750000.0, 5400000.0, 21600000.0, 6, 5, 4, 5, 3),
        (35834, "Keepstar", 33750000.0, 27000000.0, 108000000.0, 8, 6, 5, 7, 3),
        (35825, "Raitaru", 2400000.0, 1200000.0, 1200000.0, 3, 2, 1, 3, 3),
        (35826, "Azbel", 4500000.0, 3600000.0, 14400000.0, 4, 3, 2, 5, 3),
        (35827, "Sotiyo", 22500000.0, 18000000.0, 72000000.0, 6, 4, 3, 6, 3),
        (35835, "Athanor", 2400000.0, 1200000.0, 1200000.0, 3, 3, 1, 3, 3),
        (35836, "Tatara", 4500000.0, 3600000.0, 14400000.0, 5, 4, 3, 5, 3),
    ] {
        let p = life::structure_profile(&ctx, tid).unwrap_or_else(|| panic!("missing profile for {name}"));
        assert_eq!((p.shield_hp, p.armor_hp, p.hull_hp), (exp_sh, exp_ar, exp_hu));
        assert_eq!((p.high_slots, p.mid_slots, p.low_slots, p.service_slots, p.rig_slots), (hi, med, low, svc, rig));
    }
    for tid in [35840, 35841, 37534] {
        assert_eq!(life::structure_profile(&ctx, tid).unwrap().hull_class, life::HullClass::Flex);
    }
    assert!(life::structure_profile(&ctx, 35834).unwrap().disallow_in_highsec);
    let raitaru_p = life::structure_profile(&ctx, 35825).unwrap();
    assert!(raitaru_p.eng_time_bonus_pct != 0.0 && raitaru_p.eng_mat_bonus_pct != 0.0);
    for fid in 47512..=47516 {
        let fp = life::structure_profile(&ctx, fid).expect("faction fortizar");
        assert_eq!((fp.hull_class, fp.high_slots, fp.rig_slots), (life::HullClass::Large, 7, 3));
        assert!(fp.fuel_bonus_pct < 0.0 && (7..=8).contains(&fp.service_slots));
    }
    let palatine = life::structure_profile(&ctx, 40340).expect("palatine");
    assert_eq!((palatine.hull_class, palatine.fuel_bonus_pct, palatine.service_slots), (life::HullClass::XLarge, -25.0, 8));
    assert_eq!((life::structure_profile(&ctx, 35835).unwrap().refining_yield_bonus, life::structure_profile(&ctx, 35836).unwrap().refining_yield_bonus), (2.0, 5.5));

    move_ship(&ctx, &sess, open_space(&ctx, JITA, 5.0)).await;
    assert_eq!(anchor(&ctx, &mut sess, 35834, "Jita Keepstar").await.expect_err("hs keepstar").code, "CannotAnchorInHighSec");
    assert!(matches!(anchor(&ctx, &mut sess, 35840, "Jita Pharolux").await.expect_err("hs pharolux").code.as_str(), "CannotAnchorInHighSec" | "CannotAnchorCynoBeaconInHighSec" | "CannotAnchorFlexOutsideSovNull"));

    // 2. Live invbroker.Add fitting, rig destruction on unfit, authorization, and state/HP locks.
    let st_id = online_structure(&ctx, &mut sess, 35832, "Fitting & Profiles Astrahus").await;
    let ship_id = sess.ship_id.unwrap();
    let owner_corp = ctx.db.with_snapshot(|snap| snap.structures.get(&st_id).unwrap().owner_corp_id);

    let siege_mod = grant(&ctx, OWNER, ship_id, 4292, InventoryFlag::Cargo, 1);
    assert_eq!(inv_add(&ctx, &mut sess, siege_mod, st_id, InventoryFlag::HiSlot0, false).await.expect_err("siege mod").code, "ModuleCannotFitStructureType");

    let rig_id = grant(&ctx, OWNER, ship_id, 37146, InventoryFlag::Cargo, 1);
    inv_add(&ctx, &mut sess, rig_id, st_id, InventoryFlag::RigSlot0, false).await.expect("fit rig");
    assert_eq!(inv_add(&ctx, &mut sess, rig_id, st_id, InventoryFlag::RigSlot1, false).await.expect_err("move rig").code, "CannotMoveFittedRig");
    assert!(kv_bool(&inv_add(&ctx, &mut sess, rig_id, st_id, InventoryFlag::Hangar, false).await.expect("unfit rig"), "destroyed"));
    ctx.db.with_snapshot(|snap| assert!(!snap.items.contains_key(&rig_id)));

    let high_mod = grant(&ctx, OWNER, ship_id, 35922, InventoryFlag::Cargo, 1);
    let mid_mod = grant(&ctx, OWNER, ship_id, 35947, InventoryFlag::Cargo, 1);
    let low_mod = grant(&ctx, OWNER, ship_id, 35959, InventoryFlag::Cargo, 1);
    let guarded_rig = grant(&ctx, OWNER, ship_id, 37146, InventoryFlag::Cargo, 1);
    inv_add(&ctx, &mut sess, high_mod, st_id, InventoryFlag::HiSlot0, false).await.expect("fit high");
    inv_add(&ctx, &mut sess, mid_mod, st_id, InventoryFlag::MedSlot0, false).await.expect("fit mid");
    inv_add(&ctx, &mut sess, low_mod, st_id, InventoryFlag::LoSlot0, false).await.expect("fit low");
    inv_add(&ctx, &mut sess, guarded_rig, st_id, InventoryFlag::RigSlot0, false).await.expect("fit guarded rig");

    let mut guest_sess = SessionState::new(7205);
    guest_sess.user_id = Some(2); guest_sess.character_id = Some(GUEST); guest_sess.corporation_id = Some(1000009); guest_sess.solar_system_id = Some(JITA);
    let mut outsider_sess = SessionState::new(7206);
    outsider_sess.user_id = Some(3); outsider_sess.character_id = Some(OUTSIDER); outsider_sess.corporation_id = Some(1000009); outsider_sess.solar_system_id = Some(JITA); outsider_sess.station_id = Some(JITA_44);
    let mut grunt_sess = SessionState::new(7207);
    grunt_sess.user_id = Some(4); grunt_sess.character_id = Some(CORP_GRUNT); grunt_sess.corporation_id = Some(owner_corp); grunt_sess.solar_system_id = Some(JITA); grunt_sess.structure_id = Some(st_id); grunt_sess.corp_role = 0;
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch_g = snap.characters.get(&OWNER).cloned().unwrap();
        ch_g.character_id = GUEST; ch_g.corporation_id = 1000009; snap.characters.insert(GUEST, ch_g);
        let mut ch_o = snap.characters.get(&OWNER).cloned().unwrap();
        ch_o.character_id = OUTSIDER; ch_o.corporation_id = 1000009; ch_o.station_id = Some(JITA_44); ch_o.structure_id = None; snap.characters.insert(OUTSIDER, ch_o);
        let mut ch_m = snap.characters.get(&OWNER).cloned().unwrap();
        ch_m.character_id = CORP_GRUNT; ch_m.corporation_id = owner_corp; ch_m.structure_id = Some(st_id); snap.characters.insert(CORP_GRUNT, ch_m);
        snap.corporations.get_mut(&owner_corp).unwrap().members.insert(CORP_GRUNT);
    });

    let owner_ship_rig = grant(&ctx, OWNER, ship_id, 31358, InventoryFlag::RigSlot0, 1);
    assert_eq!(inv_add(&ctx, &mut outsider_sess, owner_ship_rig, JITA_44, InventoryFlag::Hangar, false).await.expect_err("foreign ship rig").code, "ItemOwnershipViolation");
    assert_eq!(inv_add(&ctx, &mut outsider_sess, guarded_rig, JITA_44, InventoryFlag::Hangar, false).await.expect_err("outsider struct rig").code, "StructureOwnershipViolation");
    assert_eq!(inv_add(&ctx, &mut grunt_sess, guarded_rig, st_id, InventoryFlag::Hangar, false).await.expect_err("grunt struct rig").code, "InsufficientCorpRoles");

    let extra_high = grant(&ctx, OWNER, ship_id, 35922, InventoryFlag::Cargo, 1);
    let fut = ctx.now_unix() + 100_000;
    ctx.db.with_mut_snapshot(|snap| { let s = snap.structures.get_mut(&st_id).unwrap(); s.state = life::STATE_ARMOR_REINFORCED.into(); s.reinforced_until_unix = fut; });
    assert_eq!(inv_add(&ctx, &mut sess, extra_high, st_id, InventoryFlag::HiSlot1, false).await.expect_err("armor refit").code, "StructureFittingLockedInState");
    assert_eq!(inv_add(&ctx, &mut sess, guarded_rig, st_id, InventoryFlag::Hangar, false).await.expect_err("armor rig unfit").code, "StructureFittingLockedInState");
    ctx.db.with_mut_snapshot(|snap| { let s = snap.structures.get_mut(&st_id).unwrap(); s.state = life::STATE_HULL_REINFORCED.into(); s.reinforced_until_unix = fut; });
    assert_eq!(inv_add(&ctx, &mut sess, extra_high, st_id, InventoryFlag::HiSlot1, false).await.expect_err("hull refit").code, "StructureFittingLockedInState");
    ctx.db.with_mut_snapshot(|snap| { let s = snap.structures.get_mut(&st_id).unwrap(); s.state = life::STATE_SHIELD_VULNERABLE.into(); s.reinforced_until_unix = 0; s.shield_hp = 100.0; });
    let svc_mod = grant(&ctx, OWNER, ship_id, 35894, InventoryFlag::Cargo, 1);
    assert_eq!(inv_add(&ctx, &mut sess, svc_mod, st_id, InventoryFlag::ServiceSlot0, false).await.expect_err("damaged refit").code, "StructureDamagedCannotRefit");
    assert_eq!(inv_add(&ctx, &mut sess, guarded_rig, st_id, InventoryFlag::Hangar, false).await.expect_err("damaged rig unfit").code, "StructureDamagedCannotRefit");

    let max_sh = life::structure_profile(&ctx, 35832).unwrap().shield_hp;
    ctx.db.with_mut_snapshot(|snap| { snap.structures.get_mut(&st_id).unwrap().shield_hp = max_sh; });
    let fit_fuel = grant(&ctx, OWNER, ship_id, 4247, InventoryFlag::Cargo, 5_000);
    rpc(&ctx, &mut sess, "invbroker", "Add", &[PyRep::Integer(fit_fuel), PyRep::Integer(st_id), PyRep::Integer(133), PyRep::Integer(5_000)]).await.expect("fuel via invbroker.Add");
    inv_add(&ctx, &mut sess, svc_mod, st_id, InventoryFlag::ServiceSlot0, true).await.expect("fit & online service via invbroker.Add");

    ctx.advance_virtual_clock(3_600);
    assert_eq!(observe(&ctx, &mut sess, st_id).await.unwrap().fuel_blocks, 4_452);

    // 3. Access groups, structure profiles (via structureSettings), and Structure Directory ACL & fuelBlocks masking.
    let group_id = rpc(&ctx, &mut sess, "ownerGroupManager", "Create", &[PyRep::String("Allied Pilots".into()), PyRep::String("ACL group".into())]).await.map(|r| r.as_i64().unwrap_or_else(|| kv_i64(&r, "groupID"))).unwrap();
    rpc(&ctx, &mut sess, "ownerGroupManager", "AddMembers", &[PyRep::Integer(group_id), PyRep::List(vec![PyRep::Integer(GUEST)])]).await.expect("AddMembers");
    let profile_id = rpc(&ctx, &mut sess, "structureSettings", "CreateProfile", &[PyRep::String("Public Market Profile".into()), PyRep::String("Taxed market".into())]).await.map(|r| r.as_i64().unwrap_or_else(|| kv_i64(&r, "profileID"))).unwrap();
    rpc(&ctx, &mut sess, "structureSettings", "ChangeProfiles", &[PyRep::List(vec![PyRep::Integer(st_id)]), PyRep::Integer(profile_id)]).await.expect("ChangeProfiles");
    let settings_payload = PyRep::List(vec![
        PyRep::Tuple(vec![PyRep::Integer(19), PyRep::Integer(0), PyRep::Integer(1)]),
        PyRep::Tuple(vec![PyRep::Integer(19), PyRep::Integer(group_id), PyRep::Integer(1)]),
        PyRep::Tuple(vec![PyRep::Integer(19), PyRep::Integer(OUTSIDER), PyRep::Integer(-1)]),
        PyRep::Tuple(vec![PyRep::Integer(23), PyRep::Real(0.025)]),
    ]);
    rpc(&ctx, &mut sess, "structureSettings", "SaveProfileSettings", &[PyRep::Integer(profile_id), settings_payload]).await.expect("SaveProfileSettings");
    ctx.advance_virtual_clock(305);

    assert!((rpc(&ctx, &mut guest_sess, "structureSettings", "CharacterGetService", &[PyRep::Integer(st_id), PyRep::Integer(10)]).await.unwrap().as_f64().unwrap() - 0.025).abs() < 1e-6);
    assert_eq!(rpc(&ctx, &mut outsider_sess, "structureSettings", "CharacterGetService", &[PyRep::Integer(st_id), PyRep::Integer(10)]).await.unwrap(), PyRep::None);
    assert_eq!(rpc(&ctx, &mut sess, "structureDirectory", "GetStructureInfo", &[PyRep::Integer(st_id)]).await.unwrap().dict_get("fuelBlocks"), Some(&PyRep::Integer(4_452)));
    assert_eq!(rpc(&ctx, &mut grunt_sess, "structureDirectory", "GetStructureInfo", &[PyRep::Integer(st_id)]).await.unwrap().dict_get("fuelBlocks"), Some(&PyRep::None));
    assert_eq!(rpc(&ctx, &mut guest_sess, "structureDirectory", "GetStructureInfo", &[PyRep::Integer(st_id)]).await.unwrap().dict_get("fuelBlocks"), Some(&PyRep::None));

    outsider_sess.solar_system_id = Some(30_002_718); outsider_sess.station_id = None;
    ctx.db.with_mut_snapshot(|snap| { let c = snap.characters.get_mut(&OUTSIDER).unwrap(); c.solar_system_id = 30_002_718; c.station_id = None; });
    assert_eq!(rpc(&ctx, &mut outsider_sess, "structureDirectory", "GetStructureInfo", &[PyRep::Integer(st_id)]).await.expect_err("remote blocked").code, "StructureAccessDenied");
    outsider_sess.solar_system_id = Some(JITA); outsider_sess.station_id = Some(JITA_44);
    ctx.db.with_mut_snapshot(|snap| { let c = snap.characters.get_mut(&OUTSIDER).unwrap(); c.solar_system_id = JITA; c.station_id = Some(JITA_44); });
    let local_blocked = rpc(&ctx, &mut outsider_sess, "structureDirectory", "GetStructureInfo", &[PyRep::Integer(st_id)]).await.expect("in-system blocked");
    assert_eq!(local_blocked.dict_get("services"), Some(&PyRep::Dict(vec![])));

    let PyRep::Dict(g_map) = rpc(&ctx, &mut guest_sess, "structureDirectory", "GetStructures", &[]).await.unwrap() else { panic!("dict") };
    let entry = g_map.iter().find(|(k, _)| k.as_i64() == Some(st_id)).map(|(_, v)| v).expect("entry");
    assert_eq!(kv_str(entry, "powerState"), "FullPower");
    assert_eq!(entry.dict_get("fuelBlocks"), Some(&PyRep::None));
    let PyRep::Dict(o_map) = rpc(&ctx, &mut outsider_sess, "structureDirectory", "GetStructures", &[]).await.unwrap() else { panic!("dict") };
    assert!(!o_map.iter().any(|(k, _)| k.as_i64() == Some(st_id)));

    // 4. Delayed profile application (300s) + defense ACL (setting ID 17) + V24 3-arg OnSessionChanged verification.
    let profile_id_2 = rpc(&ctx, &mut sess, "structureSettings", "CreateProfile", &[PyRep::String("Profile 2".into()), PyRep::String("Second".into())]).await.map(|r| r.as_i64().unwrap_or_else(|| kv_i64(&r, "profileID"))).unwrap();
    rpc(&ctx, &mut sess, "structureSettings", "ChangeProfiles", &[PyRep::List(vec![PyRep::Integer(st_id)]), PyRep::Integer(profile_id_2)]).await.expect("ChangeProfiles 2");
    ctx.advance_virtual_clock(305);
    let _ = rpc(&ctx, &mut sess, "structureSettings", "CharacterHasSetting", &[PyRep::Integer(st_id), PyRep::Integer(1)]).await.unwrap();
    let def_group_id = rpc(&ctx, &mut sess, "ownerGroupManager", "Create", &[PyRep::String("Gunners".into()), PyRep::String("Defense ACL".into())]).await.map(|r| r.as_i64().unwrap_or_else(|| kv_i64(&r, "groupID"))).unwrap();
    rpc(&ctx, &mut sess, "ownerGroupManager", "AddMembers", &[PyRep::Integer(def_group_id), PyRep::List(vec![PyRep::Integer(OUTSIDER)])]).await.unwrap();
    let profile_rows = PyRep::List(vec![
        PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(group_id), PyRep::Integer(1)]),
        PyRep::Tuple(vec![PyRep::Integer(17), PyRep::Integer(def_group_id), PyRep::Integer(1)]),
    ]);
    rpc(&ctx, &mut sess, "structureSettings", "SaveProfileSettings", &[PyRep::Integer(profile_id_2), profile_rows]).await.unwrap();
    ctx.advance_virtual_clock(305);
    guest_sess.structure_id = Some(st_id);
    assert_eq!(rpc(&ctx, &mut guest_sess, "structureControl", "TakeControl", &[PyRep::Integer(st_id)]).await.expect_err("docking guest denied").code, "DefenseAccessDenied");
    outsider_sess.structure_id = Some(st_id); outsider_sess.station_id = None;
    ctx.db.with_mut_snapshot(|snap| { let c = snap.characters.get_mut(&OUTSIDER).unwrap(); c.station_id = None; c.structure_id = Some(st_id); });
    assert!(rpc(&ctx, &mut outsider_sess, "structureControl", "TakeControl", &[PyRep::Integer(st_id)]).await.is_ok());
    assert!(rpc(&ctx, &mut outsider_sess, "structureControl", "ReleaseControl", &[PyRep::Integer(st_id)]).await.is_ok());

    sess.structure_id = Some(st_id);
    let _ = ctx.drain_notifications_for_char(OWNER);
    let prev_ship = sess.ship_id.unwrap();
    rpc(&ctx, &mut sess, "structureControl", "TakeControl", &[PyRep::Integer(st_id)]).await.expect("Owner TakeControl");
    let take_evts = ctx.drain_notifications_for_char(OWNER);
    let sc_take = take_evts.iter().find(|n| n.event == "OnSessionChanged").expect("OnSessionChanged on TakeControl");
    assert_eq!(sc_take.args.len(), 3, "OnSessionChanged must use exact 3-arg V24 shape (isRemote, sess, changes)");
    assert_eq!(sc_take.args[0], PyRep::Bool(false));
    assert_eq!(sc_take.args[2], PyRep::Dict(vec![(PyRep::String("shipid".into()), PyRep::Tuple(vec![PyRep::Integer(prev_ship), PyRep::Integer(st_id)]))]));

    rpc(&ctx, &mut sess, "structureControl", "ReleaseControl", &[PyRep::Integer(st_id)]).await.expect("Owner ReleaseControl");
    let rel_evts = ctx.drain_notifications_for_char(OWNER);
    let sc_rel = rel_evts.iter().find(|n| n.event == "OnSessionChanged").expect("OnSessionChanged on ReleaseControl");
    assert_eq!(sc_rel.args.len(), 3);
    assert_eq!(sc_rel.args[2], PyRep::Dict(vec![(PyRep::String("shipid".into()), PyRep::Tuple(vec![PyRep::Integer(st_id), PyRep::Integer(prev_ship)]))]));

    // 5. Reinforce change delay per security band & full shield/armor gate.
    assert_eq!(eve_server::services::station_structure::structure_upwell_ops::reinforce_change_delay_secs(&ctx, JITA), 30 * 86_400);
    assert_eq!(eve_server::services::station_structure::structure_upwell_ops::reinforce_change_delay_secs(&ctx, 30_000_157), 7 * 86_400);

    // 6. Symmetric SpecializedFuelBay (flag 133) role/item enforcement across Add & MultiAdd, JettisonStructureFuel type preservation, and Ansiblex Liquid Ozone via invbroker.Add.
    let guest_ship = grant(&ctx, GUEST, st_id, 606, InventoryFlag::Hangar, 1);
    let grunt_ship = grant(&ctx, CORP_GRUNT, st_id, 606, InventoryFlag::Hangar, 1);
    guest_sess.ship_id = Some(guest_ship); guest_sess.station_id = None; guest_sess.structure_id = Some(st_id);
    grunt_sess.ship_id = Some(grunt_ship); grunt_sess.station_id = None; grunt_sess.structure_id = Some(st_id);
    ctx.db.with_mut_snapshot(|snap| {
        let cg = snap.characters.get_mut(&GUEST).unwrap(); cg.station_id = None; cg.structure_id = Some(st_id); cg.active_ship_id = guest_ship;
        let cm = snap.characters.get_mut(&CORP_GRUNT).unwrap(); cm.station_id = None; cm.structure_id = Some(st_id); cm.active_ship_id = grunt_ship;
    });
    let guest_fb = grant(&ctx, GUEST, guest_ship, 4247, InventoryFlag::Cargo, 100);
    let grunt_fb = grant(&ctx, CORP_GRUNT, grunt_ship, 4247, InventoryFlag::Cargo, 100);
    assert_eq!(rpc(&ctx, &mut guest_sess, "invbroker", "Add", &[PyRep::Integer(guest_fb), PyRep::Integer(st_id), PyRep::Integer(133), PyRep::Integer(100)]).await.expect_err("guest Add").code, "StructureOwnershipViolation");
    assert_eq!(rpc(&ctx, &mut guest_sess, "invbroker", "MultiAdd", &[PyRep::List(vec![PyRep::Integer(guest_fb)]), PyRep::Integer(st_id), PyRep::Integer(133)]).await.expect_err("guest MultiAdd").code, "StructureOwnershipViolation");
    assert_eq!(rpc(&ctx, &mut grunt_sess, "invbroker", "Add", &[PyRep::Integer(grunt_fb), PyRep::Integer(st_id), PyRep::Integer(133), PyRep::Integer(100)]).await.expect_err("grunt Add").code, "CorpRoleDenied");
    assert_eq!(rpc(&ctx, &mut grunt_sess, "invbroker", "MultiAdd", &[PyRep::List(vec![PyRep::Integer(grunt_fb)]), PyRep::Integer(st_id), PyRep::Integer(133)]).await.expect_err("grunt MultiAdd").code, "CorpRoleDenied");
    let bay_fb_id = ctx.db.with_snapshot(|s| s.items.values().find(|i| i.location_id == st_id && i.flag == InventoryFlag::SpecializedFuelBay && i.type_id == 4247).unwrap().item_id);
    assert_eq!(rpc(&ctx, &mut grunt_sess, "invbroker", "Add", &[PyRep::Integer(bay_fb_id), PyRep::Integer(grunt_ship), PyRep::Integer(5), PyRep::Integer(10)]).await.expect_err("grunt withdraw Add").code, "CorpRoleDenied");
    assert_eq!(rpc(&ctx, &mut grunt_sess, "invbroker", "MultiAdd", &[PyRep::List(vec![PyRep::Integer(bay_fb_id)]), PyRep::Integer(grunt_ship), PyRep::Integer(5)]).await.expect_err("grunt withdraw MultiAdd").code, "CorpRoleDenied");

    let heavy_water = grant(&ctx, OWNER, ship_id, 16272, InventoryFlag::Cargo, 100);
    assert_eq!(rpc(&ctx, &mut sess, "invbroker", "Add", &[PyRep::Integer(heavy_water), PyRep::Integer(st_id), PyRep::Integer(133), PyRep::Integer(100)]).await.expect_err("heavy water Add").code, "InvalidInventoryFlag");
    assert_eq!(rpc(&ctx, &mut sess, "invbroker", "MultiAdd", &[PyRep::List(vec![PyRep::Integer(heavy_water)]), PyRep::Integer(st_id), PyRep::Integer(133)]).await.expect_err("heavy water MultiAdd").code, "InvalidInventoryFlag");

    let oz_item = grant(&ctx, OWNER, ship_id, 16273, InventoryFlag::Cargo, 700);
    let gas_item = grant(&ctx, OWNER, ship_id, 81143, InventoryFlag::Cargo, 300);
    rpc(&ctx, &mut sess, "invbroker", "Add", &[PyRep::Integer(oz_item), PyRep::Integer(st_id), PyRep::Integer(133), PyRep::Integer(700)]).await.expect("ozone Add");
    rpc(&ctx, &mut sess, "invbroker", "MultiAdd", &[PyRep::List(vec![PyRep::Integer(gas_item)]), PyRep::Integer(st_id), PyRep::Integer(133)]).await.expect("gas MultiAdd");
    assert_eq!(observe(&ctx, &mut sess, st_id).await.unwrap().fuel_blocks, 4_452, "Ozone and reagents must never inflate fuel_blocks");
    let jet_gas = rpc(&ctx, &mut sess, "structure", "JettisonStructureFuel", &[PyRep::Integer(st_id), PyRep::List(vec![PyRep::Integer(gas_item)])]).await.expect("jettison gas");
    let jet_oz = rpc(&ctx, &mut sess, "structure", "JettisonStructureFuel", &[PyRep::Integer(st_id), PyRep::Integer(16273), PyRep::Integer(100)]).await.expect("jettison ozone");
    let (gas_cid, oz_cid) = (kv_i64(&jet_gas, "containerID"), kv_i64(&jet_oz, "containerID"));
    ctx.db.with_snapshot(|snap| {
        assert!(snap.items.values().any(|i| i.location_id == gas_cid && i.type_id == 81143 && i.quantity == 300), "Jettisoned reagent must preserve type_id 81143");
        assert!(snap.items.values().any(|i| i.location_id == oz_cid && i.type_id == 16273 && i.quantity == 100), "Jettisoned ozone must preserve type_id 16273");
        assert_eq!(snap.structures[&st_id].fuel_blocks, 4_452);
    });

    // Ansiblex Jump Bridge fueled via live invbroker.Add from in-space ship consumes Liquid Ozone on CmdJumpThroughStructureStargate.
    let (jb_src, jb_dst, src_sys, dst_sys) = (995_000_001i64, 995_000_002i64, 30_004_712u32, 30_004_713u32);
    ctx.db.with_mut_snapshot(|snap| {
        for (jid, sys_id, nm) in [(jb_src, src_sys, "HED-GP Ansiblex"), (jb_dst, dst_sys, "Rancer Ansiblex")] {
            snap.structures.insert(jid, UpwellStructureRecord {
                structure_id: jid, owner_corp_id: owner_corp, type_id: 35841, solar_system_id: sys_id,
                name: nm.into(), state: life::STATE_SHIELD_VULNERABLE.into(), position: [0.0, 0.0, 0.0],
                shield_hp: 500_000.0, armor_hp: 500_000.0, hull_hp: 500_000.0, fuel_blocks: 1_000, reinforced_until_unix: 0,
                last_damage_tick: 0, last_damage_unix: 0, self_destruct_at_unix: None, quantum_core_installed: true,
            });
        }
        let mut ext = eve_server::services::station_structure::structure_upwell_ops::load_ext_state(snap);
        ext.jump_bridge_links.insert(jb_src, jb_dst);
        eve_server::services::station_structure::structure_upwell_ops::save_ext_state(snap, &ext);
        let ch = snap.characters.get_mut(&OWNER).unwrap();
        ch.station_id = None; ch.structure_id = None; ch.solar_system_id = src_sys;
        snap.items.get_mut(&ship_id).unwrap().location_id = i64::from(src_sys);
    });
    sess.station_id = None; sess.structure_id = None; sess.solar_system_id = Some(src_sys);
    let _ = ctx.systems.get_or_spawn(src_sys).spawn_ball_with_combat(eve_destiny::Ball { id: ship_id, type_id: 587, position: DVec3::new(1_000.0, 0.0, 0.0), velocity: DVec3::ZERO, radius: 40.0, mass: 1.0e6, mode: eve_destiny::MovementMode::Stop, speed_fraction: 0.0, max_velocity: 300.0, inertia: 1.0 }, ctx.build_type_combat_state(587)).await;
    ctx.advance_secs(12);
    assert_eq!(rpc(&ctx, &mut sess, "structureJumpBridgeMgr", "CmdJumpThroughStructureStargate", &[PyRep::Integer(jb_src)]).await.expect_err("no ozone").code, "InsufficientLiquidOzone");
    let jb_oz = grant(&ctx, OWNER, ship_id, 16273, InventoryFlag::Cargo, 600);
    rpc(&ctx, &mut sess, "invbroker", "Add", &[PyRep::Integer(jb_oz), PyRep::Integer(jb_src), PyRep::Integer(133), PyRep::Integer(600)]).await.expect("in-space ozone Add");
    let jb_res = rpc(&ctx, &mut sess, "structureJumpBridgeMgr", "CmdJumpThroughStructureStargate", &[PyRep::Integer(jb_src)]).await.expect("jump through Ansiblex");
    assert_eq!(kv_i64(&jb_res, "solarSystemID"), i64::from(dst_sys));
    ctx.db.with_snapshot(|snap| {
        let rem_oz: i32 = snap.items.values().filter(|i| i.location_id == jb_src && i.flag == InventoryFlag::SpecializedFuelBay && i.type_id == 16273).map(|i| i.quantity).sum();
        assert_eq!(rem_oz, 547, "Ansiblex jump must deduct mass-based Liquid Ozone loaded via invbroker.Add");
    });
}

}


#[allow(unused_imports, dead_code, unused_variables, unused_mut)]
mod plan_upwell_structures_ops_c {
    use super::*;

// Upwell Structure Anchoring, Quantum Core, Pochven, Palatine Keepstar, Tethering & Docking Lock
// parity tests (`plan_upwell_structures_closure_part3.rs`).

use eve_db::{InventoryFlag, InventoryItemRecord, UpwellStructureRecord};
use eve_destiny::{Ball, DamageVector, MovementMode};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use eve_server::services::space_destiny::space_nav_travel::sync_tether;
use eve_server::services::station_structure::structure_lifecycle as life;
use glam::DVec3;
use std::collections::HashMap;

const OWNER: i64 = 90_000_001;
const OUTSIDER: i64 = 90_000_003;
const RIVAL_CORP: i64 = 98_999_999;
const JITA: u32 = 30_000_144;
const RANCER: u32 = 30_002_718;
const KINO_POCHVEN: u32 = 30_000_157;

fn prepare_siege(ctx: &ServerContext, id: i64, charge_dmg_em: f64) {
    ctx.db.with_mut_snapshot(|snap| {
        let my_corp = snap.characters.get(&OWNER).map(|c| c.corporation_id).expect("corp");
        if let Some(s) = snap.structures.get_mut(&id) {
            s.owner_corp_id = RIVAL_CORP;
        }
        snap.wars.insert(
            77,
            eve_db::WarRecord {
                war_id: 77,
                aggressor_corp_id: my_corp,
                defender_corp_id: RIVAL_CORP,
                mutual: true,
                started_unix: 0,
                active: true,
            },
        );
        if let Some(w) = snap.items.get_mut(&100_000_005) {
            w.location_id = 100_000_001;
            w.flag = InventoryFlag::HiSlot0;
            w.singleton = true;
            if charge_dmg_em > 0.0 {
                w.dynamic_attributes.insert(64, 5_000.0);
            } else {
                w.dynamic_attributes.remove(&64);
            }
        }
        let old_ammo: Vec<i64> = snap.items.values().filter(|i| i.location_id == 100_000_001 && i.flag == InventoryFlag::HiSlot0 && i.item_id != 100_000_005).map(|i| i.item_id).collect();
        for aid in old_ammo { snap.items.remove(&aid); }
    });
    let _ = grant(ctx, OWNER, 100_000_001, 185, InventoryFlag::HiSlot0, 10_000);
    ctx.invalidate_ship_fitting(100_000_001);
}

async fn fire_weapon(ctx: &ServerContext, sess: &mut SessionState, target_id: i64) -> Result<PyRep, BridgeRpcError> {
    let (sys_id, ship_id) = (sess.solar_system_id.expect("sys"), sess.ship_id.expect("ship"));
    let sys = ctx.systems.get_or_spawn(sys_id);
    // BF-03: stop the repeating cycle and let it (and the rate-of-fire delay) lapse before the next volley.
    let _ = rpc(ctx, sess, "dogmaIM", "Deactivate", &[PyRep::Integer(100_000_005), PyRep::String("projectileFired".into())]).await;
    let _ = sys.step_manual(15.0).await;
    ctx.advance_secs(15);
    if !sys.get_target_locks(ship_id).await.unwrap_or_default().iter().any(|l| l.target_id == target_id && l.locked) {
        rpc(ctx, sess, "dogmaIM", "AddTarget", &[PyRep::Integer(target_id)]).await?;
        let _ = sys.step_manual(12.0).await;
    }
    rpc(ctx, sess, "dogmaIM", "Activate", &[PyRep::Integer(100_000_005), PyRep::String("projectileFired".into()), PyRep::Integer(target_id)]).await
}

async fn rpc(ctx: &ServerContext, sess: &mut SessionState, service: &str, method: &str, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None).await
}

fn kv_i64(rep: &PyRep, key: &str) -> i64 { rep.dict_get(key).and_then(|v| v.as_i64()).unwrap_or(0) }
fn kv_str(rep: &PyRep, key: &str) -> String { rep.dict_get(key).and_then(|v| v.as_str()).unwrap_or("").to_string() }

fn grant(ctx: &ServerContext, owner: i64, location: i64, type_id: u32, flag: InventoryFlag, qty: i32) -> i64 {
    ctx.db.with_mut_snapshot(|snap| {
        snap.next_entity_id += 1;
        let id = snap.next_entity_id;
        snap.items.insert(id, InventoryItemRecord { item_id: id, type_id, owner_id: owner, location_id: location, flag, quantity: qty, singleton: qty == 1, version: 1, ..Default::default() });
        id
    })
}

fn attr(ctx: &ServerContext, type_id: u32, attr_id: u16) -> f64 {
    ctx.sde.types.get(&type_id).and_then(|t| t.default_attributes.get(&attr_id).copied()).unwrap_or_else(|| panic!("SDE type {type_id} lacks attribute {attr_id}"))
}

fn new_test_ctx() -> ServerContext {
    let ctx = ServerContext::new();
    ctx.set_clock(std::sync::Arc::new(eve_db::VirtualServerClock::with_fixed_base(1_800_000_000)));
    ctx
}

async fn pilot_in_space(ctx: &ServerContext, sess_id: u64) -> SessionState {
    let perim_station = ctx.sde.stations.values().find(|s| s.solar_system_id == JITA).map(|s| s.station_id).expect("Perimeter station");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&OWNER) {
            let ship_id = ch.active_ship_id;
            ch.solar_system_id = JITA;
            ch.station_id = Some(perim_station);
            if let Some(sh) = snap.items.get_mut(&ship_id) {
                sh.location_id = perim_station;
            }
        }
    });
    let mut sess = SessionState::new(sess_id);
    rpc(ctx, &mut sess, "authentication", "Login", &[PyRep::String("demo".into()), PyRep::String("demo-secret".into())]).await.expect("Login");
    rpc(ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(OWNER)]).await.expect("Select");
    ctx.advance_secs(12);
    rpc(ctx, &mut sess, "corpRegistry", "AddCorporation", &[PyRep::String("Upwell Part3 Corp".into()), PyRep::String("UPW3".into())]).await.expect("AddCorporation");
    ctx.advance_virtual_clock(7 * 86_400 + 12);
    rpc(ctx, &mut sess, "structureDocking", "Undock", &[]).await.expect("Undock");
    sess
}

async fn move_ship(ctx: &ServerContext, sess: &SessionState, pos: DVec3) {
    assert!(ctx.systems.get_or_spawn(sess.solar_system_id.expect("sys")).set_ball_position(sess.ship_id.expect("ship"), pos).await);
}

fn open_space(ctx: &ServerContext, sys_id: u32, k: f64) -> DVec3 {
    let base = ctx.sde.stations.values().find(|s| s.solar_system_id == sys_id).map(|s| s.position).unwrap_or(DVec3::ZERO);
    base + DVec3::new(0.0, 5.0e9 + k * 3.0e9, 0.0)
}

async fn anchor(ctx: &ServerContext, sess: &mut SessionState, type_id: u32, name: &str) -> Result<PyRep, BridgeRpcError> {
    let ship = sess.ship_id.expect("ship");
    grant(ctx, OWNER, ship, type_id, InventoryFlag::Cargo, 1);
    rpc(ctx, sess, "structureDeployment", "Anchor", &[PyRep::Integer(i64::from(type_id)), PyRep::String(name.into())]).await
}

async fn observe(ctx: &ServerContext, sess: &mut SessionState, id: i64) -> Option<UpwellStructureRecord> {
    let _ = rpc(ctx, sess, "structureGuests", "GetGuests", &[PyRep::Integer(id)]).await;
    ctx.db.with_snapshot(|snap| snap.structures.get(&id).cloned())
}

async fn online_structure(ctx: &ServerContext, sess: &mut SessionState, type_id: u32, name: &str) -> i64 {
    let id = kv_i64(&anchor(ctx, sess, type_id, name).await.expect("Anchor"), "structureID");
    ctx.advance_virtual_clock(life::ANCHOR_VULNERABLE_SECS + life::ANCHORING_SECS);
    assert_eq!(observe(ctx, sess, id).await.expect("st").state, life::STATE_NO_CORE);
    let core = attr(ctx, type_id, life::ATTR_REQUIRES_DEED_TYPE) as u32;
    move_ship(ctx, sess, DVec3::from_array(ctx.db.with_snapshot(|s| s.structures[&id].position))).await;
    rpc(ctx, sess, "structureDocking", "Dock", &[PyRep::Integer(id)]).await.expect("Dock");
    let core_item = grant(ctx, OWNER, sess.ship_id.expect("ship"), core, InventoryFlag::Cargo, 1);
    rpc(ctx, sess, "invbroker", "Add", &[PyRep::Integer(core_item), PyRep::Integer(id), PyRep::Integer(172)]).await.expect("move core");
    assert_eq!(observe(ctx, sess, id).await.expect("st").state, life::STATE_ONLINING);
    ctx.advance_virtual_clock(life::ONLINING_SECS);
    assert_eq!(observe(ctx, sess, id).await.expect("st").state, life::STATE_SHIELD_VULNERABLE);
    id
}

#[tokio::test]
async fn test_upwell_anchoring_quantum_core_pochven_and_palatine() {
    // parity: feat:structures.anchoring-onlining-phases, feat:structures.quantum-core-gate, feat:structures.pochven-anchoring-restrictions, feat:structures.upwell-palatine-keepstar-limit
    eve_testkit::attest(&[
        "feat:structures.anchoring-onlining-phases",
        "feat:structures.quantum-core-gate",
        "feat:structures.pochven-anchoring-restrictions",
        "feat:structures.upwell-palatine-keepstar-limit",
    ]);

    let ctx = new_test_ctx();
    let mut sess = pilot_in_space(&ctx, 7102).await;
    let my_corp = ctx.db.with_snapshot(|snap| snap.characters.get(&OWNER).unwrap().corporation_id);
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(OWNER).or_default().insert(3416, eve_db::TrainedSkillRecord { type_id: 3416, level: 5, skill_points: 256_000.0 });
    });

    // 1. Minimum 1,000 km anchoring distance check + 4-phase anchoring/quantum-core/onlining progression via live weapon RPCs.
    let station_pos = ctx.sde.stations.values().find(|s| s.solar_system_id == JITA).unwrap().position;
    move_ship(&ctx, &sess, station_pos + DVec3::new(500_000.0, 0.0, 0.0)).await;
    assert_eq!(anchor(&ctx, &mut sess, 35832, "Too Close").await.expect_err("too close").code, "TooCloseToStructureOrStation");
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.retain(|_, it| !(it.location_id == sess.ship_id.unwrap() && it.type_id == 35832 && it.flag == InventoryFlag::Cargo));
    });

    let pos3 = open_space(&ctx, JITA, 3.0);
    move_ship(&ctx, &sess, pos3).await;
    let st_id = kv_i64(&anchor(&ctx, &mut sess, 35832, "Phased Astrahus").await.expect("Anchor"), "structureID");
    let st_anchored = observe(&ctx, &mut sess, st_id).await.unwrap();
    move_ship(&ctx, &sess, DVec3::from_array(st_anchored.position)).await;
    assert_eq!((st_anchored.state.as_str(), st_anchored.shield_hp, st_anchored.armor_hp), (life::STATE_ANCHOR_VULNERABLE, 0.0, 0.0));
    let prof_hp = life::structure_profile(&ctx, 35832).unwrap();

    // Pre-NoCore docking rejected on BOTH structureDocking.Dock and beyonce.CmdDock:
    assert_eq!(rpc(&ctx, &mut sess, "structureDocking", "Dock", &[PyRep::Integer(st_id)]).await.expect_err("dock AnchorVulnerable").code, "StructureNotDockable");
    assert_eq!(rpc(&ctx, &mut sess, "beyonce", "CmdDock", &[PyRep::Integer(st_id)]).await.expect_err("cmddock AnchorVulnerable").code, "StructureNotDockable");

    // AnchorVulnerable pause under qualifying live weapon fire and timerless destruction:
    let t_init = st_anchored.reinforced_until_unix;
    prepare_siege(&ctx, st_id, 0.0);
    fire_weapon(&ctx, &mut sess, st_id).await.expect("sub-threshold anchor hit");
    assert_eq!(ctx.db.with_snapshot(|s| s.structures[&st_id].reinforced_until_unix), t_init, "sub-threshold hit does not pause anchoring");
    prepare_siege(&ctx, st_id, 20_000.0);
    fire_weapon(&ctx, &mut sess, st_id).await.expect("qualifying anchor hit");
    assert!(ctx.db.with_snapshot(|s| s.structures[&st_id].reinforced_until_unix) > t_init, "qualifying weapon DPS pauses AnchorVulnerable timer");
    ctx.db.with_mut_snapshot(|snap| snap.structures.get_mut(&st_id).unwrap().owner_corp_id = my_corp);

    let pos4 = open_space(&ctx, JITA, 4.0);
    move_ship(&ctx, &sess, pos4).await;
    let doomed_id = kv_i64(&anchor(&ctx, &mut sess, 35832, "Doomed Anchor").await.expect("anchor doomed"), "structureID");
    move_ship(&ctx, &sess, pos4 + DVec3::new(60_000.0, 0.0, 0.0)).await;
    ctx.db.with_mut_snapshot(|snap| snap.structures.get_mut(&doomed_id).unwrap().hull_hp = 10.0);
    prepare_siege(&ctx, doomed_id, 20_000.0);
    fire_weapon(&ctx, &mut sess, doomed_id).await.expect("destroy in AnchorVulnerable");
    assert_eq!(ctx.db.with_snapshot(|s| s.structures[&doomed_id].state.clone()), life::STATE_DESTROYED);

    move_ship(&ctx, &sess, pos3 + DVec3::new(60_000.0, 0.0, 0.0)).await;
    ctx.advance_virtual_clock(life::ANCHOR_VULNERABLE_SECS + 30);
    assert_eq!(observe(&ctx, &mut sess, st_id).await.unwrap().state, life::STATE_ANCHORING);
    assert_eq!(rpc(&ctx, &mut sess, "structureDocking", "Dock", &[PyRep::Integer(st_id)]).await.expect_err("dock Anchoring").code, "StructureNotDockable");
    assert_eq!(rpc(&ctx, &mut sess, "beyonce", "CmdDock", &[PyRep::Integer(st_id)]).await.expect_err("cmddock Anchoring").code, "StructureNotDockable");
    prepare_siege(&ctx, st_id, 20_000.0);
    assert_eq!(fire_weapon(&ctx, &mut sess, st_id).await.expect_err("invuln during Anchoring").code, "StructureReinforcedInvulnerable");
    ctx.db.with_mut_snapshot(|snap| snap.structures.get_mut(&st_id).unwrap().owner_corp_id = my_corp);

    ctx.advance_virtual_clock(life::ANCHORING_SECS);
    let st_nocore = observe(&ctx, &mut sess, st_id).await.unwrap();
    assert_eq!(st_nocore.state, life::STATE_NO_CORE);
    assert!(!st_nocore.quantum_core_installed);

    // Quantum core gate: onlining a service module AND docked ship fitting while quantum_core_installed == false or Onlining are blocked.
    for (hull, core) in [(35832, 56201), (35835, 56202), (35825, 56203), (35833, 56204), (35836, 56205), (35826, 56206), (35834, 56207), (35827, 56208)] {
        assert_eq!(life::structure_profile(&ctx, hull).and_then(|p| p.core_type_id), Some(core));
    }
    ctx.advance_virtual_clock(65);
    move_ship(&ctx, &sess, DVec3::from_array(st_nocore.position) + DVec3::new(500.0, 0.0, 0.0)).await;
    rpc(&ctx, &mut sess, "structureDocking", "Dock", &[PyRep::Integer(st_id)]).await.expect("Dock");
    let active_ship = sess.ship_id.unwrap();
    let ship_mod_cargo = grant(&ctx, OWNER, active_ship, 10039, InventoryFlag::Cargo, 1);
    let ship_mod_fit = grant(&ctx, OWNER, active_ship, 10039, InventoryFlag::MedSlot0, 1);
    let _hangar_mod = grant(&ctx, OWNER, st_id, 10039, InventoryFlag::Hangar, 1);
    ctx.db.with_mut_snapshot(|snap| {
        let ship_tid = snap.items[&active_ship].type_id;
        snap.saved_fittings.insert(9001, eve_db::SavedFittingRecord {
            fitting_id: 9001, owner_id: OWNER, ship_type_id: ship_tid,
            name: "NoCore Test Fit".into(), description: String::new(),
            items: vec![(10039, InventoryFlag::MedSlot1, 1)],
        });
    });
    assert_eq!(rpc(&ctx, &mut sess, "invbroker", "Add", &[PyRep::Integer(ship_mod_cargo), PyRep::Integer(active_ship), PyRep::Integer(InventoryFlag::MedSlot1 as i64)]).await.expect_err("ship fit NoCore").code, "QuantumCoreRequired");
    assert_eq!(rpc(&ctx, &mut sess, "invbroker", "Add", &[PyRep::Integer(ship_mod_fit), PyRep::Integer(active_ship), PyRep::Integer(InventoryFlag::Cargo as i64)]).await.expect_err("ship unfit NoCore").code, "QuantumCoreRequired");
    assert_eq!(rpc(&ctx, &mut sess, "invbroker", "MultiAdd", &[PyRep::List(vec![PyRep::Integer(ship_mod_fit)]), PyRep::Integer(active_ship), PyRep::Integer(InventoryFlag::Cargo as i64)]).await.expect_err("multiadd unfit NoCore").code, "QuantumCoreRequired");
    assert_eq!(rpc(&ctx, &mut sess, "ship", "FitShips", &[PyRep::List(vec![PyRep::Integer(active_ship)]), PyRep::Integer(9001)]).await.expect_err("FitShips NoCore").code, "QuantumCoreRequired");
    assert_eq!(rpc(&ctx, &mut sess, "dogmaIM", "StripFitting", &[PyRep::Integer(active_ship)]).await.expect_err("StripFitting NoCore").code, "QuantumCoreRequired");

    let clone_bay_mod = grant(&ctx, OWNER, active_ship, 35894, InventoryFlag::Cargo, 1);
    let st_fuel = grant(&ctx, OWNER, active_ship, 4247, InventoryFlag::Cargo, 5_000);
    rpc(&ctx, &mut sess, "invbroker", "Add", &[PyRep::Integer(st_fuel), PyRep::Integer(st_id), PyRep::Integer(133), PyRep::Integer(5_000)]).await.expect("fuel");
    assert_eq!(rpc(&ctx, &mut sess, "invbroker", "Add", &[PyRep::Integer(clone_bay_mod), PyRep::Integer(st_id), PyRep::Integer(InventoryFlag::ServiceSlot0 as i64)]).await.expect_err("no fit in NoCore").code, "StructureFittingLockedInState");
    ctx.db.with_mut_snapshot(|snap| {
        let m = snap.items.get_mut(&clone_bay_mod).unwrap();
        m.owner_id = my_corp; m.location_id = st_id; m.flag = InventoryFlag::ServiceSlot0;
        snap.module_online_states.insert(clone_bay_mod, false);
    });
    assert_eq!(rpc(&ctx, &mut sess, "dogmaIM", "SetModuleOnline", &[PyRep::Integer(st_id), PyRep::Integer(clone_bay_mod)]).await.expect_err("no core").code, "QuantumCoreRequired");

    // Negative 1: wrong core type (56202) into StructureDeedBay (172) fails with InvalidItemType
    let wrong_core = grant(&ctx, OWNER, active_ship, 56202, InventoryFlag::Cargo, 1);
    assert_eq!(
        rpc(&ctx, &mut sess, "invbroker", "Add", &[PyRep::Integer(wrong_core), PyRep::Integer(st_id), PyRep::Integer(172)]).await.expect_err("wrong core type").code,
        "InvalidItemType"
    );
    ctx.db.with_mut_snapshot(|snap| { snap.items.remove(&wrong_core); });

    // Negative 2: moving quantum core (56201) to Hangar (flag 4) leaves structure in NoCore and core is NOT consumed
    let hangar_core = grant(&ctx, OWNER, active_ship, 56201, InventoryFlag::Cargo, 1);
    rpc(&ctx, &mut sess, "invbroker", "Add", &[PyRep::Integer(hangar_core), PyRep::Integer(st_id), PyRep::Integer(4)]).await.expect("add core to hangar");
    assert_eq!(observe(&ctx, &mut sess, st_id).await.unwrap().state, life::STATE_NO_CORE);
    ctx.db.with_snapshot(|snap| {
        let it = snap.items.get(&hangar_core).expect("hangar core item must exist");
        assert_eq!(it.flag, InventoryFlag::Hangar);
        assert_eq!(it.quantity, 1);
    });

    // Positive: matching core (56201) deposited into StructureDeedBay (172) enters Onlining (15m) where ship fitting remains blocked.
    let right_core = grant(&ctx, OWNER, active_ship, 56201, InventoryFlag::Cargo, 1);
    rpc(&ctx, &mut sess, "invbroker", "Add", &[PyRep::Integer(right_core), PyRep::Integer(st_id), PyRep::Integer(172)]).await.expect("add right core");
    assert_eq!(observe(&ctx, &mut sess, st_id).await.unwrap().state, life::STATE_ONLINING);
    assert_eq!(rpc(&ctx, &mut sess, "invbroker", "Add", &[PyRep::Integer(ship_mod_cargo), PyRep::Integer(active_ship), PyRep::Integer(InventoryFlag::MedSlot1 as i64)]).await.expect_err("ship fit Onlining").code, "QuantumCoreRequired");
    assert_eq!(rpc(&ctx, &mut sess, "ship", "FitShips", &[PyRep::List(vec![PyRep::Integer(active_ship)]), PyRep::Integer(9001)]).await.expect_err("FitShips Onlining").code, "QuantumCoreRequired");
    assert_eq!(rpc(&ctx, &mut sess, "dogmaIM", "StripFitting", &[PyRep::Integer(active_ship)]).await.expect_err("StripFitting Onlining").code, "QuantumCoreRequired");
    rpc(&ctx, &mut sess, "structureDocking", "Undock", &[]).await.expect("Undock for onlining combat check");

    // Qualifying weapon fire during Onlining pauses timer & damages hull; destroying during Onlining is timerless and drops quantum core ball!
    let t_on0 = ctx.db.with_snapshot(|s| s.structures[&st_id].reinforced_until_unix);
    prepare_siege(&ctx, st_id, 20_000.0);
    fire_weapon(&ctx, &mut sess, st_id).await.expect("onlining qualifying hit");
    ctx.db.with_mut_snapshot(|snap| {
        assert!(snap.structures[&st_id].reinforced_until_unix > t_on0);
        assert!(snap.structures[&st_id].hull_hp < prof_hp.hull_hp);
        snap.structures.get_mut(&st_id).unwrap().owner_corp_id = my_corp;
    });

    let pos5 = open_space(&ctx, JITA, 5.0);
    move_ship(&ctx, &sess, pos5).await;
    let doomed_on = kv_i64(&anchor(&ctx, &mut sess, 35832, "Doomed Onlining").await.expect("anchor doomed on"), "structureID");
    move_ship(&ctx, &sess, pos5 + DVec3::new(60_000.0, 0.0, 0.0)).await;
    ctx.advance_virtual_clock(life::ANCHOR_VULNERABLE_SECS + life::ANCHORING_SECS);
    let _ = observe(&ctx, &mut sess, doomed_on).await;
    ctx.advance_virtual_clock(65);
    rpc(&ctx, &mut sess, "structureDocking", "Dock", &[PyRep::Integer(doomed_on)]).await.expect("Dock doomed on");
    let core_doomed = grant(&ctx, OWNER, sess.ship_id.unwrap(), 56201, InventoryFlag::Cargo, 1);
    rpc(&ctx, &mut sess, "invbroker", "Add", &[PyRep::Integer(core_doomed), PyRep::Integer(doomed_on), PyRep::Integer(172)]).await.expect("core doomed");
    rpc(&ctx, &mut sess, "structureDocking", "Undock", &[]).await.expect("Undock doomed");
    ctx.db.with_mut_snapshot(|snap| snap.structures.get_mut(&doomed_on).unwrap().hull_hp = 10.0);
    prepare_siege(&ctx, doomed_on, 20_000.0);
    fire_weapon(&ctx, &mut sess, doomed_on).await.expect("destroy during Onlining");
    assert_eq!(ctx.db.with_snapshot(|s| s.structures[&doomed_on].state.clone()), life::STATE_DESTROYED);
    let (wreck_id, dropped_core_id) = ctx.db.with_snapshot(|snap| {
        let p = snap.structures[&doomed_on].position;
        let wreck = snap.items.values().find(|i| {
            i.location_id == JITA as i64
                && snap.item_positions.get(&i.item_id) == Some(&p)
                && (i.type_id == 26557 || ctx.sde.wrecks.values().any(|&w| w == i.type_id))
        }).expect("structure wreck container");
        let core = snap.items.values().find(|i| i.type_id == 56201 && i.location_id == wreck.item_id).expect("dropped core item inside wreck");
        (wreck.item_id, core.item_id)
    });
    assert!(ctx.systems.get_or_spawn(JITA).get_ball_snapshot(wreck_id).await.unwrap().is_some(), "structure wreck ball spawned in space");
    assert!(ctx.systems.get_or_spawn(JITA).get_ball_snapshot(dropped_core_id).await.unwrap().is_none(), "core inside wreck has no independent space ball");

    ctx.advance_virtual_clock(life::ONLINING_SECS + 65);
    move_ship(&ctx, &sess, pos3 + DVec3::new(60_000.0, 0.0, 0.0)).await;
    let st_online = observe(&ctx, &mut sess, st_id).await.unwrap();
    assert_eq!(st_online.state, life::STATE_SHIELD_VULNERABLE);
    assert!((st_online.hull_hp - prof_hp.hull_hp).abs() < 1.0 && st_online.quantum_core_installed);
    rpc(&ctx, &mut sess, "structureDocking", "Dock", &[PyRep::Integer(st_id)]).await.expect("Dock online");
    rpc(&ctx, &mut sess, "invbroker", "Add", &[PyRep::Integer(ship_mod_cargo), PyRep::Integer(active_ship), PyRep::Integer(InventoryFlag::MedSlot1 as i64)]).await.expect("ship fit online with core");
    rpc(&ctx, &mut sess, "invbroker", "Add", &[PyRep::Integer(ship_mod_fit), PyRep::Integer(active_ship), PyRep::Integer(InventoryFlag::Cargo as i64)]).await.expect("ship unfit online with core");
    rpc(&ctx, &mut sess, "invbroker", "Add", &[PyRep::Integer(ship_mod_cargo), PyRep::Integer(active_ship), PyRep::Integer(InventoryFlag::Cargo as i64)]).await.expect("restore ship_mod_cargo");
    rpc(&ctx, &mut sess, "dogmaIM", "SetModuleOnline", &[PyRep::Integer(st_id), PyRep::Integer(clone_bay_mod)]).await.expect("online service with core");

    // High-Sec alliance-level war precheck (active alliance war permitted; un-warmed non-mutual war rejected):
    rpc(&ctx, &mut sess, "structureDocking", "Undock", &[]).await.expect("Undock for alliance war check");
    move_ship(&ctx, &sess, pos3 + DVec3::new(60_000.0, 0.0, 0.0)).await;
    let (ally_a, ally_b) = (99_100_001i64, 99_100_002i64);
    prepare_siege(&ctx, st_id, 0.0);
    let now_war = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        snap.wars.clear();
        snap.corporations.get_mut(&my_corp).unwrap().alliance_id = Some(ally_a);
        let mut rcorp = snap.corporations.get(&my_corp).unwrap().clone();
        rcorp.corporation_id = RIVAL_CORP; rcorp.alliance_id = Some(ally_b);
        snap.corporations.insert(RIVAL_CORP, rcorp);
        snap.characters.get_mut(&OWNER).unwrap().alliance_id = Some(ally_a);
        snap.wars.insert(88, eve_db::WarRecord { war_id: 88, aggressor_corp_id: ally_a, defender_corp_id: ally_b, mutual: false, started_unix: now_war, active: true });
    });
    assert_eq!(fire_weapon(&ctx, &mut sess, st_id).await.expect_err("24h warmup").code, "IllegalHighSecStructureAttack");
    ctx.db.with_mut_snapshot(|snap| snap.wars.get_mut(&88).unwrap().started_unix = 0);
    fire_weapon(&ctx, &mut sess, st_id).await.expect("active High-Sec alliance war permitted");
    ctx.db.with_mut_snapshot(|snap| {
        assert!(eve_server::services::structure_acl_defense_rules::are_corps_at_war_in_snap(snap, my_corp, RIVAL_CORP, ctx.now_unix()));
        let mut ds = eve_server::services::structure_acl_defense_rules::load_defense_settings(snap, st_id);
        ds.auto_defense_enabled = true;
        eve_server::services::structure_acl_defense_rules::save_defense_settings(snap, &ds);
        assert!(eve_server::services::structure_acl_defense_rules::evaluate_auto_defense_target(&ctx, snap, st_id, OWNER));
        snap.structures.get_mut(&st_id).unwrap().owner_corp_id = my_corp;
    });

    // 2. Pochven anchoring restrictions: Upwell structures & POS towers rejected; Mobile Depot allowed; Customs Office gated by >= 1.5 Triglavian standing.
    sess.solar_system_id = Some(KINO_POCHVEN);
    ctx.db.with_mut_snapshot(|snap| snap.characters.get_mut(&OWNER).unwrap().solar_system_id = KINO_POCHVEN);
    let poch_ship = sess.ship_id.unwrap();
    let planet_pos = DVec3::new(27660817438.0, -327544042.0, 66850617032.0);
    let ship_pos = planet_pos + DVec3::new(10_000_000.0, 0.0, 0.0);
    let _ = ctx.systems.get_or_spawn(KINO_POCHVEN).spawn_ball(Ball { id: poch_ship, type_id: 606, position: ship_pos, velocity: DVec3::ZERO, radius: 40.0, mass: 1_000_000.0, mode: MovementMode::Stop, speed_fraction: 0.0, max_velocity: 300.0, inertia: 1.0 }).await;
    assert!(matches!(anchor(&ctx, &mut sess, 35832, "Pochven Citadel").await.expect_err("pochven upwell").code.as_str(), "AnchoringDisallowedInPochven" | "CannotAnchorInPochven"));

    let pos_tower = grant(&ctx, OWNER, poch_ship, 12235, InventoryFlag::Cargo, 1);
    let pos_err = rpc(&ctx, &mut sess, "posMgr", "AnchorStructure", &[PyRep::Integer(pos_tower), PyRep::Tuple(vec![PyRep::Real(ship_pos.x), PyRep::Real(ship_pos.y), PyRep::Real(ship_pos.z)])]).await.expect_err("pochven pos");
    assert!(matches!(pos_err.code.as_str(), "AnchoringRestrictedInPochven" | "StarbaseRemoved"));

    let mobile_depot = grant(&ctx, OWNER, poch_ship, 33474, InventoryFlag::Cargo, 1);
    assert_eq!(kv_str(&rpc(&ctx, &mut sess, "posMgr", "AnchorStructure", &[PyRep::Integer(mobile_depot), PyRep::Tuple(vec![PyRep::Real(ship_pos.x), PyRep::Real(ship_pos.y), PyRep::Real(ship_pos.z)])]).await.expect("pochven depot"), "state"), "Anchoring");

    let gantry_item = grant(&ctx, OWNER, poch_ship, 3962, InventoryFlag::Cargo, 1);
    assert_eq!(rpc(&ctx, &mut sess, "posMgr", "AnchorOrbital", &[PyRep::Integer(gantry_item)]).await.expect_err("low trig standing").code, "InsufficientTriglavianStanding");
    ctx.db.with_mut_snapshot(|snap| { snap.characters.get_mut(&OWNER).unwrap().standings.insert(500_026, 2.0); });
    assert_eq!(kv_str(&rpc(&ctx, &mut sess, "posMgr", "AnchorOrbital", &[PyRep::Integer(gantry_item)]).await.expect("high trig standing"), "state"), "Anchored");

    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(999_888, eve_db::UpwellStructureRecord {
            structure_id: 999_888, owner_corp_id: 1000009, solar_system_id: KINO_POCHVEN, type_id: 35832,
            state: life::STATE_SHIELD_VULNERABLE.into(), reinforced_until_unix: 0, shield_hp: 10_000.0, armor_hp: 10_000.0, hull_hp: 10_000.0,
            quantum_core_installed: true, fuel_blocks: 1_000, position: [0.0, 0.0, 0.0], name: "Pochven Legacy".into(), last_damage_tick: 0, last_damage_unix: 0, self_destruct_at_unix: None,
        });
    });
    assert!(life::is_operational(&observe(&ctx, &mut sess, 999_888).await.unwrap().state));

    // 3. Upwell Palatine Keepstar (40340) cluster-wide limit of 1 active (replaced via live combat destruction!).
    sess.solar_system_id = Some(RANCER);
    ctx.db.with_mut_snapshot(|snap| snap.characters.get_mut(&OWNER).unwrap().solar_system_id = RANCER);
    let rancer_sys = ctx.systems.get_or_spawn(RANCER);
    let _ = rancer_sys.spawn_ball_with_combat(Ball { id: poch_ship, type_id: 587, position: open_space(&ctx, RANCER, 1.0), velocity: DVec3::ZERO, radius: 40.0, mass: 1_000_000.0, mode: MovementMode::Stop, speed_fraction: 0.0, max_velocity: 300.0, inertia: 1.0 }, ctx.build_type_combat_state(587)).await;
    let pal1 = kv_i64(&anchor(&ctx, &mut sess, 40340, "Palatine One").await.expect("first Palatine"), "structureID");
    move_ship(&ctx, &sess, open_space(&ctx, RANCER, 2.0)).await;
    assert_eq!(anchor(&ctx, &mut sess, 40340, "Palatine Two").await.expect_err("second Palatine").code, "PalatineKeepstarLimitReached");

    move_ship(&ctx, &sess, open_space(&ctx, RANCER, 1.0) + DVec3::new(60_000.0, 0.0, 0.0)).await;
    ctx.db.with_mut_snapshot(|snap| snap.structures.get_mut(&pal1).unwrap().hull_hp = 10.0);
    prepare_siege(&ctx, pal1, 20_000.0);
    fire_weapon(&ctx, &mut sess, pal1).await.expect("destroy Palatine One in AnchorVulnerable");
    assert_eq!(ctx.db.with_snapshot(|s| s.structures[&pal1].state.clone()), life::STATE_DESTROYED);
    move_ship(&ctx, &sess, open_space(&ctx, RANCER, 2.0)).await;
    assert!(anchor(&ctx, &mut sess, 40340, "Palatine Replacement").await.is_ok());
}

#[tokio::test]
async fn test_upwell_tethering_eligibility_restrictions_and_docking_lock() {
    // parity: feat:structures.tether-eligibility-and-restrictions, feat:structures.tethering-docking-lock
    eve_testkit::attest(&[
        "feat:structures.tether-eligibility-and-restrictions",
        "feat:structures.tethering-docking-lock",
    ]);

    let ctx = new_test_ctx();
    let mut sess = pilot_in_space(&ctx, 7201).await;
    let st_pos = open_space(&ctx, JITA, 1.0);
    move_ship(&ctx, &sess, st_pos).await;
    let astra_id = online_structure(&ctx, &mut sess, 35832, "Tether Astrahus").await;
    let st_pos = DVec3::from_array(ctx.db.with_snapshot(|s| s.structures[&astra_id].position));
    rpc(&ctx, &mut sess, "structureDocking", "Undock", &[]).await.expect("Undock");

    let ship_id = sess.ship_id.unwrap();
    let jita_sys = ctx.systems.get_or_spawn(JITA);
    move_ship(&ctx, &sess, st_pos + DVec3::new(1_500.0, 0.0, 0.0)).await;

    {
        let mut cs = jita_sys.get_combat_state(ship_id).await.unwrap().unwrap();
        cs.shield_hp = cs.max_shield_hp * 0.4; cs.armor_hp = cs.max_armor_hp * 0.4;
        cs.structure_hp = cs.max_structure_hp * 0.4; cs.capacitor = cs.max_capacitor * 0.4;
        let _ = jita_sys.set_combat_state(ship_id, cs).await;
    }
    sync_tether(&ctx, OWNER, JITA, ship_id).await;
    let cs_tethered = jita_sys.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!(cs_tethered.is_tethered && cs_tethered.shield_hp > cs_tethered.max_shield_hp * 0.6);

    let hp_before = cs_tethered.shield_hp;
    let _ = jita_sys.apply_damage(ship_id, DamageVector { em: 5_000.0, thermal: 5_000.0, kinetic: 5_000.0, explosive: 5_000.0 }).await;
    assert!((jita_sys.get_combat_state(ship_id).await.unwrap().unwrap().shield_hp - hp_before).abs() < 1e-3);

    let unpiloted_ship = grant(&ctx, OUTSIDER, i64::from(JITA), 606, InventoryFlag::None, 1);
    let _ = jita_sys.spawn_ball_with_combat(Ball { id: unpiloted_ship, type_id: 606, position: st_pos + DVec3::new(1_200.0, 0.0, 0.0), velocity: DVec3::ZERO, radius: 40.0, mass: 1_000_000.0, mode: MovementMode::Stop, speed_fraction: 0.0, max_velocity: 300.0, inertia: 1.0 }, ctx.build_type_combat_state(606)).await;
    sync_tether(&ctx, 0, JITA, unpiloted_ship).await;
    assert!(!jita_sys.get_combat_state(unpiloted_ship).await.unwrap().unwrap().is_tethered);

    let pod_id = grant(&ctx, OWNER, i64::from(JITA), 670, InventoryFlag::None, 1);
    let _ = jita_sys.spawn_ball_with_combat(Ball { id: pod_id, type_id: 670, position: st_pos + DVec3::new(800.0, 0.0, 0.0), velocity: DVec3::ZERO, radius: 10.0, mass: 10_000.0, mode: MovementMode::Stop, speed_fraction: 0.0, max_velocity: 500.0, inertia: 1.0 }, ctx.build_type_combat_state(670)).await;
    ctx.db.with_mut_snapshot(|snap| snap.characters.get_mut(&OWNER).unwrap().active_ship_id = pod_id);
    sync_tether(&ctx, OWNER, JITA, pod_id).await;
    assert!(jita_sys.get_combat_state(pod_id).await.unwrap().unwrap().is_tethered);
    ctx.db.with_mut_snapshot(|snap| snap.characters.get_mut(&OWNER).unwrap().active_ship_id = ship_id);

    let mut attacker_sess = SessionState::new(7202);
    attacker_sess.user_id = Some(3); attacker_sess.character_id = Some(OUTSIDER); attacker_sess.corporation_id = Some(1000009); attacker_sess.solar_system_id = Some(JITA); attacker_sess.ship_id = Some(unpiloted_ship);
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.get(&OWNER).unwrap().clone();
        ch.character_id = OUTSIDER; ch.user_id = 3; ch.name = "Attacker".into(); ch.corporation_id = 1000009; ch.solar_system_id = JITA; ch.active_ship_id = unpiloted_ship;
        snap.characters.insert(OUTSIDER, ch);
        snap.structure_docking_acl_denied.entry(astra_id).or_default().insert(OUTSIDER);
    });
    sync_tether(&ctx, OUTSIDER, JITA, unpiloted_ship).await;
    assert!(!jita_sys.get_combat_state(unpiloted_ship).await.unwrap().unwrap().is_tethered);
    assert!(matches!(rpc(&ctx, &mut attacker_sess, "structureDocking", "Dock", &[PyRep::Integer(astra_id)]).await.expect_err("acl dock").code.as_str(), "ShipNotDockableAtStructure" | "DockingPermissionDenied" | "StructureDockingAccessDenied"));
    assert_eq!(rpc(&ctx, &mut attacker_sess, "dogmaIM", "AddTarget", &[PyRep::Integer(ship_id)]).await.expect_err("tethered lock").code, "TargetInvulnerable");

    let sb_mod = grant(&ctx, OWNER, ship_id, 4266, InventoryFlag::HiSlot0, 1);
    ctx.invalidate_ship_fitting(ship_id);
    assert_eq!(rpc(&ctx, &mut sess, "dogmaIM", "Activate", &[PyRep::Integer(sb_mod), PyRep::String("empWave".into())]).await.expect_err("sb tethered").code, "CantActivateWhileTethered");
    let ore_item = grant(&ctx, OWNER, ship_id, 1230, InventoryFlag::SpecializedOreHold, 1_000);
    assert_eq!(rpc(&ctx, &mut sess, "inSpaceCompressionMgr", "CompressItemInSpace", &[PyRep::Integer(ore_item)]).await.expect_err("compress tethered").code, "CannotCompressWhileTethered");

    rpc(&ctx, &mut sess, "dogmaIM", "AddTarget", &[PyRep::Integer(unpiloted_ship)]).await.expect("lock target");
    assert!(!jita_sys.get_combat_state(ship_id).await.unwrap().unwrap().is_tethered);
    let _ = jita_sys.clear_target_locks(ship_id).await;
    sync_tether(&ctx, OWNER, JITA, ship_id).await;
    assert!(jita_sys.get_combat_state(ship_id).await.unwrap().unwrap().is_tethered);

    ctx.active_fighters.write().unwrap().insert(999_001, eve_server::router::context::ActiveFighterSquadron {
        ball_id: 999_001, item_id: 999_001, type_id: 23055, owner_char_id: OWNER, parent_ship_id: ship_id, tube_index: 0, squadron_size: 9, target_id: None, mwd_active: false, is_abandoned: false,
    });
    sync_tether(&ctx, OWNER, JITA, ship_id).await;
    assert!(!jita_sys.get_combat_state(ship_id).await.unwrap().unwrap().is_tethered);
    ctx.active_fighters.write().unwrap().get_mut(&999_001).unwrap().is_abandoned = true;
    sync_tether(&ctx, OWNER, JITA, ship_id).await;
    assert!(jita_sys.get_combat_state(ship_id).await.unwrap().unwrap().is_tethered);
    ctx.active_fighters.write().unwrap().clear();

    for mod_tid in [4292u32, 21096] {
        let m = grant(&ctx, OWNER, ship_id, mod_tid, InventoryFlag::HiSlot1, 1);
        ctx.db.with_mut_snapshot(|snap| { snap.module_active_states.insert(m); });
        sync_tether(&ctx, OWNER, JITA, ship_id).await;
        assert!(!jita_sys.get_combat_state(ship_id).await.unwrap().unwrap().is_tethered);
        ctx.db.with_mut_snapshot(|snap| { snap.module_active_states.remove(&m); });
    }

    let mut scram_cs = jita_sys.get_combat_state(ship_id).await.unwrap().unwrap();
    scram_cs.warp_core_strength = 2; scram_cs.warp_scramble_strength = 1;
    let _ = jita_sys.set_combat_state(ship_id, scram_cs.clone()).await;
    sync_tether(&ctx, OWNER, JITA, ship_id).await;
    assert!(jita_sys.get_combat_state(ship_id).await.unwrap().unwrap().is_tethered);
    scram_cs.warp_scramble_strength = 3;
    let _ = jita_sys.set_combat_state(ship_id, scram_cs.clone()).await;
    sync_tether(&ctx, OWNER, JITA, ship_id).await;
    assert!(!jita_sys.get_combat_state(ship_id).await.unwrap().unwrap().is_tethered);
    assert_eq!(rpc(&ctx, &mut sess, "structureDocking", "Dock", &[PyRep::Integer(astra_id)]).await.expect_err("scram dock").code, "ShipWarpScrambled");
    scram_cs.warp_scramble_strength = 0; scram_cs.warp_core_strength = 0;
    let _ = jita_sys.set_combat_state(ship_id, scram_cs).await;

    ctx.crimewatch.write().unwrap().trigger_weapon_timer(OWNER, ctx.now_unix());
    sync_tether(&ctx, OWNER, JITA, ship_id).await;
    assert!(!jita_sys.get_combat_state(ship_id).await.unwrap().unwrap().is_tethered);
    assert_eq!(rpc(&ctx, &mut sess, "structureDocking", "Dock", &[PyRep::Integer(astra_id)]).await.expect_err("weapons timer dock").code, "WeaponsTimerActive");
    ctx.advance_virtual_clock(65);

    ctx.crimewatch.write().unwrap().get_or_create(OWNER).pvp_timer_until_unix = ctx.now_unix() + 900;
    sync_tether(&ctx, OWNER, JITA, ship_id).await;
    assert!(!jita_sys.get_combat_state(ship_id).await.unwrap().unwrap().is_tethered);
    ctx.crimewatch.write().unwrap().get_or_create(OWNER).pvp_timer_until_unix = 0;

    ctx.db.with_mut_snapshot(|snap| snap.characters.get_mut(&OWNER).unwrap().jump_cooldown_until_unix = ctx.now_unix() + 600);
    sync_tether(&ctx, OWNER, JITA, ship_id).await;
    assert!(!jita_sys.get_combat_state(ship_id).await.unwrap().unwrap().is_tethered);
    ctx.db.with_mut_snapshot(|snap| snap.characters.get_mut(&OWNER).unwrap().jump_cooldown_until_unix = 0);

    ctx.db.with_mut_snapshot(|snap| snap.items.get_mut(&ship_id).unwrap().type_id = 11567);
    let _ = jita_sys.spawn_ball_with_combat(Ball { id: ship_id, type_id: 11567, position: st_pos + DVec3::new(1_500.0, 0.0, 0.0), velocity: DVec3::ZERO, radius: 400.0, mass: 1.0e9, mode: MovementMode::Stop, speed_fraction: 0.0, max_velocity: 100.0, inertia: 1.0 }, ctx.build_type_combat_state(11567)).await;
    assert!(matches!(rpc(&ctx, &mut sess, "structureDocking", "Dock", &[PyRep::Integer(astra_id)]).await.expect_err("titan in astrahus").code.as_str(), "ShipTooLargeForStructure" | "ShipNotDockableAtStructure"));
    ctx.db.with_mut_snapshot(|snap| snap.structures.get_mut(&astra_id).unwrap().type_id = 35834);
    assert!(rpc(&ctx, &mut sess, "structureDocking", "Dock", &[PyRep::Integer(astra_id)]).await.is_ok());
}

}

#[allow(dead_code, unused_imports)]
pub(crate) mod structures_anchoring_and_evermarks {
    use super::*;

//  End-to-end parity closure tests for the 11 remediated `feat:*` rows:
//  - `feat:wallet.nes-store`, `feat:wallet.evermarks`, `feat:wallet.hypernet-relay`
//  - `feat:structures.anchor-restrictions`, `feat:structures.profiles`,
//    `feat:structures.defense-settings`, `feat:structures.structure-directory`
//  - `feat:serverops.daily-jobs`

use crate::common;
use eve_db::{
    AsteroidRecord, CharacterRecord, ContractRecord, CorporationRecord, InventoryFlag,
    InventoryItemRecord, IskCents, NeuralAttributes, SocialContactRecord, SystemDynamicState,
    TrainedSkillRecord,
};
use eve_machonet::SessionState;
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, DynamicValue, EvePublicRequest,
    EvePublicResponse, GoogleAny, GoogleTimestamp, QuasarDescriptorRegistry, QuasarMessage,
    TYPE_GOOGLEAPIS_PREFIX,
};
use eve_proto::PyRep;
use eve_server::quasar_gateway::STATUS_OK;
use eve_server::router::ServerContext;
use eve_server::services::quasar_cosmetics_skinr::COMP_CAT_PATTERN;
use std::collections::HashMap;

fn extract_response_proto(rep: &PyRep) -> EvePublicResponse {
    if let PyRep::KeyVal(entries) = rep {
        for (k, v) in entries {
            if k == "response_proto" {
                if let PyRep::Bytes(b) = v {
                    return EvePublicResponse::decode_proto(b).expect("decode EvePublicResponse");
                }
            }
        }
    }
    panic!("Expected KeyVal containing response_proto, got {rep:?}");
}

async fn quasar_rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    type_name: &str,
    value: Vec<u8>,
) -> EvePublicResponse {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let req = EvePublicRequest {
        issued: Some(GoogleTimestamp { seconds: ctx.now_unix() as i64, nanos: 0 }),
        correlation_uuid: format!("closure-test-{n:06}").into_bytes(),
        payload: Some(GoogleAny { type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"), value }),
        external_origin: "eve-client".into(),
        application_instance_uuid: b"closure-it-suite".to_vec(),
        authoritative_context: None,
        trace_context: None,
    };
    let rep = ctx
        .dispatch_rpc(sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &HashMap::new(), None)
        .await
        .expect("publicGatewaySvc.SendRequest");
    extract_response_proto(&rep)
}

async fn call_rpc(ctx: &ServerContext, sess: &mut SessionState, service: &str, method: &str, args: &[PyRep]) -> Result<PyRep, eve_proto::BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None).await
}

fn make_sess(sid: u64, uid: u64, cid: i64, corp_id: i64, sys_id: u32, station_id: Option<i64>, role: u64, corp_role: u64) -> SessionState {
    let mut s = SessionState::new(sid);
    s.user_id = Some(uid);
    s.character_id = Some(cid);
    s.corporation_id = Some(corp_id);
    s.solar_system_id = Some(sys_id);
    s.station_id = station_id;
    s.role = role;
    s.corp_role = corp_role;
    s
}

fn register_char(ctx: &ServerContext, uid: u64, cid: i64, corp_id: i64, sys_id: u32, station_id: i64, isk: i64, plex: i64) -> i64 {
    ctx.db.with_mut_snapshot(|snap| {
        snap.next_entity_id += 1;
        let ship_id = snap.next_entity_id;
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: cid,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.characters.insert(
            cid,
            CharacterRecord {
                activity_status: None, character_id: cid, user_id: uid, name: format!("Pilot {cid}"),
                corporation_id: corp_id, alliance_id: None, war_faction_id: None,
                station_id: Some(station_id), structure_id: None, solar_system_id: sys_id,
                constellation_id: 20_000_020, region_id: 10_000_002, active_ship_id: ship_id,
                balance: IskCents::from_isk(isk), plex_balance: plex,
                loyalty_points: HashMap::new(), standings: HashMap::new(), security_status: 0.0,
                clone_grade: 1, home_station_id: station_id, race_id: 1, bloodline_id: 1,
                ancestry_id: 1, school_id: 11,
                neural: NeuralAttributes::default(),
                skill_points: 0.0, free_skill_points: 0.0, last_clone_jump_unix: 0,
                jump_fatigue_minutes: 0.0, jump_cooldown_until_unix: 0, jump_fatigue_until_unix: 0,
                description: String::new(), title: String::new(), gender: 1,
                create_date_unix: 1_700_000_000, fleet_id: None, autopilot_waypoints: Vec::new(),
                version: 1, system_epoch: 1,
            },
        );
        snap.account_plex_vault.insert(uid, plex);
        ship_id
    })
}

#[tokio::test]
async fn test_wallet_nes_evermarks_hypernet_closure() {
    // parity: feat:wallet.nes-store, feat:wallet.evermarks, feat:wallet.hypernet-relay
    eve_testkit::attest(&[
        "feat:wallet.nes-store",
        "feat:wallet.evermarks",
        "feat:wallet.hypernet-relay",
    ]);

    let ctx = ServerContext::new();
    let t0 = 1_800_000_000_u64;
    ctx.set_clock(std::sync::Arc::new(eve_db::VirtualServerClock::with_fixed_base(t0)));
    common::seed_test_nes_store_catalog(&ctx);

    let corp_id = 98_100_001_i64;
    let cid = 2_100_000_101_i64;
    let buyer_cid = 2_100_000_102_i64;
    let jita_station = 60_003_760_i64;
    register_char(&ctx, 501, cid, corp_id, 30_000_142, jita_station, 5_000_000_000, 1_000);
    register_char(&ctx, 502, buyer_cid, 1_000_009, 30_000_142, jita_station, 5_000_000_000, 100);
    let mut sess = make_sess(10, 501, cid, corp_id, 30_000_142, Some(jita_station), 0, 1);

    // 1. NES Store: catalog, Omega purchase, weekly 15,000 SP reward & Tuesday reset
    let offers = call_rpc(&ctx, &mut sess, "storeManager", "get_offers", &[]).await.expect("get_offers");
    assert!(matches!(offers, PyRep::List(ref xs) if !xs.is_empty()));
    let can_alpha = call_rpc(&ctx, &mut sess, "storeManager", "can_purchase_offer", &[PyRep::Integer(1005)])
        .await
        .expect("can_purchase_offer 1005");
    assert_eq!(can_alpha, PyRep::Bool(false));

    call_rpc(
        &ctx,
        &mut sess,
        "storeManager",
        "buy_offer",
        &[PyRep::Integer(1001), PyRep::String("PLX".into()), PyRep::Integer(1)],
    )
    .await
    .expect("buy_offer 1001");
    call_rpc(&ctx, &mut sess, "storeManager", "buy_offer", &[PyRep::Integer(1005)])
        .await
        .expect("buy_offer 1005");
    assert!(
        call_rpc(&ctx, &mut sess, "storeManager", "buy_offer", &[PyRep::Integer(1005)]).await.is_err(),
        "duplicate weekly SP claim must fail"
    );
    ctx.set_clock(std::sync::Arc::new(eve_db::VirtualServerClock::with_fixed_base(t0 + 7 * 86_400)));
    call_rpc(&ctx, &mut sess, "storeManager", "buy_offer", &[PyRep::Integer(1005)])
        .await
        .expect("buy_offer 1005 after weekly reset");

    // 2. EverMarks: accrue via Quasar LP wallet (verifying 1-to-1 corp EverMark match) & spend in Paragon SDE LP store
    let mut admin_sess = make_sess(13, 501, cid, corp_id, 30_000_142, Some(jita_station), eve_machonet::service_roles::ROLE_GM_DEFAULT, 1);
    let mut dep_req = Vec::new();
    let mut op_id_m = Vec::new();
    encode_bytes_field(1, b"paragon-em-op-01", &mut op_id_m);
    let mut cid_msg = Vec::new();
    encode_varint_field(1, cid as u64, &mut cid_msg);
    let mut issuer_msg = Vec::new();
    encode_varint_field(1, 1_000_419, &mut issuer_msg);
    let mut wallet_id = Vec::new();
    encode_bytes_field(1, &cid_msg, &mut wallet_id);
    encode_bytes_field(3, &issuer_msg, &mut wallet_id);
    let mut curr_m = Vec::new();
    encode_varint_field(1, 50_000, &mut curr_m);
    let mut op_attr = Vec::new();
    encode_bytes_field(1, &wallet_id, &mut op_attr);
    encode_bytes_field(2, &curr_m, &mut op_attr);
    encode_bytes_field(1, &op_id_m, &mut dep_req);
    encode_bytes_field(2, &op_attr, &mut dep_req);
    let dep_rsp = quasar_rpc(&ctx, &mut admin_sess, "eve.loyaltypoints.assetholding.operation.api.DepositRequest", dep_req).await;
    assert_eq!(dep_rsp.status_code, STATUS_OK);

    let (char_em, corp_em) = ctx.db.with_snapshot(|s| {
        let cem = s.characters.get(&cid).and_then(|c| c.loyalty_points.get(&1_000_419).copied()).unwrap_or(0);
        let corpem = s.corp_lp_balances.get(&(corp_id, 1_000_419)).copied().unwrap_or(0);
        (cem, corpem)
    });
    assert_eq!(char_em, 50_000);
    assert_eq!(corp_em, 50_000, "corp must receive EverMarks 1-to-1");

    let paragon_offers = call_rpc(&ctx, &mut sess, "LPStoreMgr", "GetAvailableOffersFromCorp", &[PyRep::Integer(1_000_419)])
        .await
        .expect("GetAvailableOffersFromCorp");
    let first_offer_id = match &paragon_offers {
        PyRep::List(rows) => rows[0].dict_get("offerID").and_then(|v| v.as_i64()).expect("offerID"),
        _ => panic!("expected List of offers"),
    };
    ctx.db.with_mut_snapshot(|s| {
        if let Some(ch) = s.characters.get_mut(&cid) {
            ch.station_id = Some(60_015_169);
        }
    });
    sess.station_id = Some(60_015_169); // Jita VI - Paragon Fulfillment Center
    call_rpc(
        &ctx,
        &mut sess,
        "LPStoreMgr",
        "TakeOfferForCharacter",
        &[PyRep::Integer(1_000_419), PyRep::Integer(first_offer_id), PyRep::Integer(1)],
    )
    .await
    .expect("TakeOfferForCharacter");
    ctx.db.with_mut_snapshot(|s| {
        if let Some(ch) = s.characters.get_mut(&cid) {
            ch.station_id = Some(jita_station);
        }
    });
    sess.station_id = Some(jita_station);
    let char_em_after = ctx.db.with_snapshot(|s| s.characters.get(&cid).and_then(|c| c.loyalty_points.get(&1_000_419).copied()).unwrap_or(0));
    assert!(char_em_after < char_em);

    // 3. HyperNet Relay: NPC station enforcement, HyperCore consumption, random ticket purchase, and winner delivery
    let rifter_id = ctx.db.with_mut_snapshot(|snap| {
        snap.next_entity_id += 1;
        let rid = snap.next_entity_id;
        snap.items.insert(
            rid,
            InventoryItemRecord {
                item_id: rid,
                type_id: 587,
                owner_id: cid,
                location_id: jita_station,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.next_entity_id += 1;
        let hid = snap.next_entity_id;
        snap.items.insert(
            hid,
            InventoryItemRecord {
                item_id: hid,
                type_id: 52_568,
                owner_id: cid,
                location_id: jita_station,
                flag: InventoryFlag::Hangar,
                quantity: 500,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        rid
    });
    ctx.db.with_mut_snapshot(|s| {
        if let Some(ch) = s.characters.get_mut(&cid) {
            ch.station_id = None;
            ch.structure_id = Some(1_000_000_000_001);
        }
    });
    assert!(
        call_rpc(
            &ctx,
            &mut sess,
            "raffleProxy",
            "CreateRaffle",
            &[PyRep::Integer(rifter_id), PyRep::Real(1_000_000.0), PyRep::Integer(8)],
        )
        .await
        .is_err()
    );
    ctx.db.with_mut_snapshot(|s| {
        if let Some(ch) = s.characters.get_mut(&cid) {
            ch.station_id = Some(jita_station);
            ch.structure_id = None;
        }
    });
    let create_res = call_rpc(
        &ctx,
        &mut sess,
        "raffleProxy",
        "CreateRaffle",
        &[PyRep::Integer(rifter_id), PyRep::Real(1_000_000.0), PyRep::Integer(8)],
    )
    .await
    .expect("CreateRaffle");
    let raffle_id = create_res.dict_get("raffleID").and_then(|v| v.as_i64()).expect("raffleID");
    let mut buyer_sess = make_sess(12, 502, buyer_cid, 1_000_009, 30_000_142, Some(jita_station), 0, 0);
    call_rpc(
        &ctx,
        &mut buyer_sess,
        "raffleProxy",
        "BuyRandomTickets",
        &[PyRep::Integer(raffle_id), PyRep::Integer(8)],
    )
    .await
    .expect("BuyRandomTickets");
    call_rpc(&ctx, &mut buyer_sess, "raffleProxy", "AwardItem", &[PyRep::Integer(raffle_id)])
        .await
        .expect("AwardItem");
    assert_eq!(ctx.db.with_snapshot(|s| s.items.get(&rifter_id).map(|i| i.owner_id)), Some(buyer_cid));
}

#[tokio::test]
async fn test_structures_anchoring_profiles_defense_directory_closure() {
    let ctx = ServerContext::new();
    let t0 = 1_800_000_000_u64;
    ctx.set_clock(std::sync::Arc::new(eve_db::VirtualServerClock::with_fixed_base(t0)));

    let corp_id = 98_200_001_i64;
    let cid = 2_100_000_201_i64;
    let attacker_cid = 2_100_000_202_i64;
    let lowsec_sys = 30_002_718_u32;
    let jita_station = 60_003_760_i64;
    let ship_id = register_char(&ctx, 601, cid, corp_id, lowsec_sys, jita_station, 10_000_000_000, 0);
    register_char(&ctx, 602, attacker_cid, 1_000_009, lowsec_sys, jita_station, 1_000_000_000, 0);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.station_id = None;
        }
        snap.corporations.insert(
            corp_id,
            CorporationRecord {
                corporation_id: corp_id,
                name: "Test Upwell Corp".into(),
                ticker: "TUPC".into(),
                ceo_id: cid,
                ..Default::default()
            },
        );
        snap.corp_governance.entry(corp_id).or_default().created_unix = t0 - 10 * 86_400;
        snap.next_entity_id += 1;
        let astra_item = snap.next_entity_id;
        snap.items.insert(
            astra_item,
            InventoryItemRecord {
                item_id: astra_item,
                type_id: 35_832,
                owner_id: cid,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });

    let mut sess = make_sess(20, 601, cid, corp_id, lowsec_sys, None, 0, 1);
    sess.ship_id = Some(ship_id);

    // 1. Structure Profiles + Defense Settings
    let profile_id = call_rpc(
        &ctx,
        &mut sess,
        "structureProfiles",
        "CreateProfile",
        &[PyRep::String("Doctrine Profile".into()), PyRep::String("Auto-defense".into())],
    )
    .await
    .expect("CreateProfile")
    .as_i64()
    .expect("profile_id");
    let defense_kv = PyRep::Tuple(vec![PyRep::Bool(true), PyRep::Bool(true), PyRep::Bool(true), PyRep::Real(-1.0), PyRep::Bool(false)]);
    let def_rows = PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(1003), PyRep::Integer(0), defense_kv.clone()])]);
    call_rpc(&ctx, &mut sess, "structureProfiles", "SaveProfileSettings", &[PyRep::Integer(profile_id), def_rows])
        .await
        .expect("SaveProfileSettings");

    // 2. Anchor Restrictions: Jita rejected; lowsec Rancer succeeds
    let mut jita_sess = make_sess(21, 601, cid, corp_id, 30_000_142, None, 0, 1);
    jita_sess.ship_id = Some(ship_id);
    assert!(
        call_rpc(
            &ctx,
            &mut jita_sess,
            "structureDeployment",
            "Anchor",
            &[PyRep::Integer(35_832), PyRep::String("Illegal Jita Astra".into())],
        )
        .await
        .is_err()
    );

    let clear_pos = glam::DVec3::new(5.0e11, 0.0, 5.0e11);
    let _ = ctx
        .systems
        .get_or_spawn(lowsec_sys)
        .spawn_ball_with_combat(
            eve_destiny::Ball {
                id: ship_id,
                type_id: 587,
                position: clear_pos,
                velocity: glam::DVec3::ZERO,
                radius: 40.0,
                mass: 1.0e6,
                mode: eve_destiny::MovementMode::Stop,
                speed_fraction: 0.0,
                max_velocity: 300.0,
                inertia: 1.0,
            },
            ctx.build_type_combat_state(587),
        )
        .await;
    call_rpc(
        &ctx,
        &mut sess,
        "structureDeployment",
        "Anchor",
        &[
            PyRep::Integer(35_832),
            PyRep::List(vec![
                PyRep::Real(clear_pos.x + 60_000.0),
                PyRep::Real(clear_pos.y),
                PyRep::Real(clear_pos.z),
            ]),
            PyRep::Integer(profile_id),
            PyRep::Tuple(vec![PyRep::Integer(18), PyRep::Integer(2)]),
            PyRep::String("Rancer Fort".into()),
        ],
    )
    .await
    .expect("Anchor in lowsec");
    let (structure_id, st_type_id) = ctx.db.with_snapshot(|s| {
        s.structures
            .values()
            .find(|st| st.owner_corp_id == corp_id)
            .map(|st| (st.structure_id, st.type_id))
            .expect("anchored structure")
    });
    assert_eq!(st_type_id, 35_832);
    call_rpc(&ctx, &mut sess, "structureProfiles", "SaveProfileSettings", &[PyRep::Integer(structure_id), PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(1003), PyRep::Integer(0), defense_kv])])])
        .await
        .expect("SaveProfileSettings on structure");

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(st) = snap.structures.get_mut(&structure_id) {
            st.state = "Online".into();
        }
        snap.corp_contacts.entry(corp_id).or_default().push(SocialContactRecord {
            contact_id: attacker_cid,
            contact_type: "character".into(),
            standing: -5.0,
            is_watched: false,
            is_blocked: false,
            label_ids: Vec::new(),
        });
    });

    // 3. Defense Settings evaluation
    let (should_target, friendly_target) = ctx.db.with_snapshot(|snap| {
        (
            eve_server::services::structure_acl_defense_rules::evaluate_auto_defense_target(&ctx, snap, structure_id, attacker_cid),
            eve_server::services::structure_acl_defense_rules::evaluate_auto_defense_target(&ctx, snap, structure_id, cid),
        )
    });
    assert_eq!((should_target, friendly_target), (true, false));

    // 4. Structure Directory
    let corp_structs = call_rpc(&ctx, &mut sess, "structureDirectory", "GetMyCorporationStructures", &[])
        .await
        .expect("GetMyCorporationStructures");
    let corp_count = match &corp_structs { PyRep::Dict(xs) => xs.iter().filter(|(k, _)| k.as_i64() == Some(structure_id)).count(), _ => 0 };
    assert_eq!(corp_count, 1);
    let map_data = call_rpc(&ctx, &mut sess, "structureDirectory", "GetStructureMapData", &[PyRep::Integer(lowsec_sys as i64)])
        .await
        .expect("GetStructureMapData");
    let map_count = match &map_data { PyRep::List(xs) => xs.len(), _ => 0 };
    assert_eq!(map_count, 1);
}

#[tokio::test]
async fn test_cosmetics_skinr_designer_and_market_closure() {
    let ctx = ServerContext::new();
    let t0 = 1_800_000_000_u64;
    ctx.set_clock(std::sync::Arc::new(eve_db::VirtualServerClock::with_fixed_base(t0)));

    let seller_cid = 2_100_000_301_i64;
    let buyer_cid = 2_100_000_302_i64;
    let jita_station = 60_003_760_i64;
    register_char(&ctx, 701, seller_cid, 1_000_009, 30_000_142, jita_station, 5_000_000_000, 500);
    register_char(&ctx, 702, buyer_cid, 1_000_009, 30_000_142, jita_station, 5_000_000_000, 500);

    ctx.db.with_mut_snapshot(|snap| {
        let sk = snap.skills.entry(seller_cid).or_default();
        sk.insert(84_217, TrainedSkillRecord { type_id: 84_217, level: 5, skill_points: 256_000.0 });
        sk.insert(81_377, TrainedSkillRecord { type_id: 81_377, level: 5, skill_points: 256_000.0 });
        if let Some(b) = snap.characters.get_mut(&buyer_cid) {
            b.loyalty_points.insert(1_000_419, 50_000);
        }
    });

    let mut admin_s = make_sess(30, 701, seller_cid, 1_000_009, 30_000_142, Some(jita_station), eve_machonet::service_roles::ROLE_GM_DEFAULT, 0);
    let mut seller_s = make_sess(31, 701, seller_cid, 1_000_009, 30_000_142, Some(jita_station), 0, 0);

    // 1. SKINR Designer: Tier 1 PLEX cost = 2500 cents (25 PLEX, 0% skill discount)
    let pricing_rsp = quasar_rpc(
        &ctx,
        &mut seller_s,
        "eve_public.cosmetic.ship.skin.thirdparty.sequencing.api.GetTierPricingRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(pricing_rsp.status_code, STATUS_OK);
    let reg = QuasarDescriptorRegistry::open_default().expect("open_default");
    let p_any = pricing_rsp.payload.as_ref().unwrap();
    let p_dyn = reg.decode_dynamic(p_any.normalized_type_name(), &p_any.value).unwrap();
    assert_eq!(p_dyn.get(2), Some(&DynamicValue::Uint32(0)), "no skill discount on SKINR PLEX cost");

    let skinr_cat = eve_server::services::quasar_cosmetics_skinr::SkinrStaticCatalog::load(&ctx.sde);
    let coat_id = skinr_cat
        .components
        .values()
        .find(|c| !c.is_pattern() && !c.disallowed_slots.contains(&1) && skinr_cat.compute_points_and_tier(&[c.component_id]).1 == 1)
        .expect("Tier 1 coating comp")
        .component_id;
    let mut grant_comp = Vec::new();
    let mut comp_m = Vec::new();
    encode_varint_field(1, coat_id as u64, &mut comp_m);
    let mut comp_id_outer = Vec::new();
    encode_bytes_field(1, &comp_m, &mut comp_id_outer);
    encode_bytes_field(1, &comp_id_outer, &mut grant_comp);
    encode_varint_field(2, 5, &mut grant_comp);
    assert_eq!(quasar_rpc(&ctx, &mut admin_s, "eve_public.cosmetic.ship.skin.thirdparty.component.license.finite.api.admin.GrantRequest", grant_comp).await.status_code, STATUS_OK);

    let (mut init_req, mut layout_m, mut slot_m, mut slot_id_m, mut cfg_m, mut coat_cfg_m, mut ship_m) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
    encode_string_field(1, "Closure SKIN", &mut init_req);
    encode_string_field(2, "Closure Line", &mut init_req);
    encode_varint_field(1, 1, &mut slot_id_m);
    encode_bytes_field(1, &comp_m, &mut coat_cfg_m);
    encode_bytes_field(1, &coat_cfg_m, &mut cfg_m);
    encode_bytes_field(1, &slot_id_m, &mut slot_m);
    encode_bytes_field(2, &cfg_m, &mut slot_m);
    encode_bytes_field(1, &slot_m, &mut layout_m);
    encode_varint_field(1, 587, &mut ship_m);
    encode_bytes_field(2, &ship_m, &mut layout_m);
    encode_varint_field(3, 1, &mut layout_m);
    encode_bytes_field(3, &layout_m, &mut init_req);
    encode_varint_field(5, 1, &mut init_req);

    let init_rsp = quasar_rpc(&ctx, &mut seller_s, "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.InitiateRequest", init_req).await;
    assert_eq!(init_rsp.status_code, STATUS_OK);
    assert_eq!(ctx.db.with_snapshot(|s| s.characters.get(&seller_cid).map(|c| c.plex_balance)), Some(475), "Tier 1 SKINR sequencing must deduct 25 PLEX (500 -> 475)");
    let init_any = init_rsp.payload.as_ref().unwrap();
    let init_dyn = reg.decode_dynamic(init_any.normalized_type_name(), &init_any.value).unwrap();
    let job_uuid_bytes = match init_dyn.get(1) {
        Some(DynamicValue::Message(jm)) => match jm.get(1) {
            Some(DynamicValue::Bytes(b)) => b.clone(),
            _ => panic!("missing job uuid"),
        },
        _ => panic!("missing job id"),
    };
    let (mut comp_early_req, mut jid_m) = (Vec::new(), Vec::new());
    encode_bytes_field(1, &job_uuid_bytes, &mut jid_m);
    encode_bytes_field(1, &jid_m, &mut comp_early_req);
    assert_eq!(quasar_rpc(&ctx, &mut seller_s, "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.CompleteEarlyRequest", comp_early_req).await.status_code, STATUS_OK);

    // 2. SKINR Market: Paragon Relations V reduces 20% vendor tax to 15%
    let tax_rsp = quasar_rpc(&ctx, &mut seller_s, "eve_public.cosmetic.market.api.GetTaxRateRequest", Vec::new()).await;
    assert_eq!(tax_rsp.status_code, STATUS_OK);
    let tax_any = tax_rsp.payload.as_ref().unwrap();
    let tax_dyn = reg.decode_dynamic(tax_any.normalized_type_name(), &tax_any.value).unwrap();
    let tax_pct = match tax_dyn.get(1) {
        Some(DynamicValue::Message(tr)) => match tr.get(1) {
            Some(DynamicValue::Sint64(s)) => *s,
            _ => 0,
        },
        _ => 0,
    };
    assert_eq!(tax_pct, 15);
    let _ = COMP_CAT_PATTERN;
}

#[tokio::test]
async fn test_serverops_daily_jobs_and_time_dilation_closure() {
    // parity: feat:serverops.daily-jobs
    eve_testkit::attest(&[
        "feat:serverops.daily-jobs",
    ]);

    let ctx = ServerContext::new();
    let t0 = 1_800_000_000_u64;
    ctx.set_clock(std::sync::Arc::new(eve_db::VirtualServerClock::with_fixed_base(t0)));

    let sys_id = 30_002_718_u32;
    let cid = 2_100_000_401_i64;
    register_char(&ctx, 801, cid, 1_000_009, sys_id, 60_003_760, 1_000_000_000, 0);

    let (ast_id, contract_id) = ctx.db.with_mut_snapshot(|snap| {
        snap.next_entity_id += 1;
        let aid = snap.next_entity_id;
        snap.asteroids.insert(
            aid,
            AsteroidRecord {
                asteroid_id: aid,
                solar_system_id: sys_id,
                belt_id: 1,
                type_id: 1230,
                quantity: 0,
                initial_quantity: 10_000,
                position: (1.0e9, 0.0, 0.0),
                radius: 500.0,
            },
        );
        snap.next_entity_id += 1;
        let kid = snap.next_entity_id;
        snap.contracts.insert(
            kid,
            ContractRecord {
                contract_id: kid, issuer_id: cid, acceptor_id: None,
                contract_type: "ItemExchange".into(), status: "Outstanding".into(),
                start_station_id: 60_003_760, end_station_id: 60_003_760,
                price: IskCents::from_isk(1_000_000), reward: IskCents::ZERO,
                collateral: IskCents::ZERO, item_ids: Vec::new(),
                expiration_unix: t0 - 100, issuer_corp_id: 1_000_009,
                date_issued_unix: t0 - 10 * 86_400, ..Default::default()
            },
        );
        snap.system_states.insert(
            sys_id,
            SystemDynamicState {
                solar_system_id: sys_id, dbs_multiplier: 1.0,
                ess_main_bank: IskCents::ZERO, ess_reserve_bank: IskCents::ZERO,
                ess_linked_character_id: None, ess_link_started_tick: None,
                ess_cycle_started_unix: 0, ess_main_bank_contributors: HashMap::new(),
                sov_alliance_id: None, military_index: 3, industrial_index: 2, strategic_index: 1,
                fw_contested_pct: 0.0, insurgency_corruption_stage: 0, insurgency_suppression_stage: 0,
                wormhole_remaining_mass_kg: 0.0, wormhole_destination_system_id: None,
                signature_position: None, fw_complex_position: None,
                fw_capture_seconds_elapsed: 0, fw_capture_duration_seconds: 0,
                last_fw_capture_tick: 0, last_sov_claim_tick: 0,
            },
        );
        (aid, kid)
    });

    let report = eve_server::services::serverops_cluster::execute_daily_maintenance_jobs(&ctx);
    assert!(report.get("replenishedAsteroids").copied().unwrap_or(0) >= 1);
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.asteroids.get(&ast_id).map(|a| a.quantity), Some(10_000));
        assert_eq!(snap.contracts.get(&contract_id).map(|c| c.status.as_str()), Some("Expired"));
    });

    let _sys_handle = ctx.systems.get_or_spawn(sys_id);
    let mut gm_sess = make_sess(40, 801, cid, 1_000_009, sys_id, None, eve_machonet::service_roles::ROLE_GM_DEFAULT, 0);
    let tidi_rep = call_rpc(&ctx, &mut gm_sess, "slash", "SlashCmd", &[PyRep::String("/tidi 0.25".into())]).await.expect("/tidi command");
    assert_eq!(tidi_rep.as_f64(), Some(0.25));
    assert!((eve_server::services::serverops_cluster::get_system_tidi_factor(&ctx, sys_id) - 0.25).abs() < 1e-6);
}

}
