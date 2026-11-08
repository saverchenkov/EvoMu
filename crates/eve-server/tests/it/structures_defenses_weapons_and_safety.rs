//! Integration tests for Upwell structure defenses, damage caps, vulnerability schedules,
//! weapons, control, reactions, moon drill, killmail, and asset safety.
//!
//! Parity rows:
//! - `feat:structures.service-reactions`
//! - `feat:structures.service-moon-drill`
//! - `feat:structures.repair-timer`
//! - `feat:structures.damage-cap`
//! - `feat:structures.vulnerability-schedule`
//! - `feat:structures.structure-killmail`
//! - `feat:structures.asset-safety-auto`
//! - `feat:structures.access-lists`
//! - `feat:structures.structure-control`
//! - `feat:structures.structure-weapons`

#![forbid(unsafe_code)]

use eve_db::{
    corp_roles, InventoryFlag, InventoryItemRecord, IskCents, TrainedSkillRecord,
    UpwellStructureRecord,
};
use eve_destiny::{Ball, CombatState, MovementMode, SlimItemMeta};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use eve_server::services::station_structure::structure_lifecycle as life;
use eve_server::services::station_structure::structure_upwell_ops::{load_ext_state, save_ext_state};
use eve_server::services::structure_acl_defense_rules as acl_rules;
use glam::DVec3;
use std::collections::HashMap;

const OWNER: i64 = 90_000_001;
const OTHER_PILOT: i64 = 90_000_002;
const BLOCKED_PILOT: i64 = 90_000_003;
const CORP_ID: i64 = 98_000_001;
const RIVAL_CORP: i64 = 98_999_999;
const JITA: u32 = 30_000_144;
const LOWSEC_SYS: u32 = 30_000_012;
const WH_SYS: u32 = 31_000_001;

async fn rpc(ctx: &ServerContext, sess: &mut SessionState, service: &str, method: &str, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None).await
}

fn kv_i64(rep: &PyRep, key: &str) -> i64 { rep.dict_get(key).and_then(|v| v.as_i64()).unwrap_or(0) }

fn grant(ctx: &ServerContext, owner: i64, location: i64, type_id: u32, flag: InventoryFlag, qty: i32) -> i64 {
    ctx.db.with_mut_snapshot(|snap| {
        snap.next_entity_id += 1;
        let id = snap.next_entity_id;
        snap.items.insert(id, InventoryItemRecord {
            item_id: id, type_id, owner_id: owner, location_id: location, flag,
            quantity: qty, singleton: qty == 1, version: 1, ..Default::default()
        });
        id
    })
}

fn attr(ctx: &ServerContext, type_id: u32, attr_id: u16) -> f64 {
    ctx.sde.types.get(&type_id).and_then(|t| t.default_attributes.get(&attr_id).copied()).unwrap_or(0.0)
}

fn setup_pilot_session(ctx: &ServerContext, sess_id: u64, char_id: i64, corp_id: i64, roles: u64, sys_id: u32) -> SessionState {
    let mut sess = SessionState::new(sess_id);
    sess.user_id = Some(sess_id);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(corp_id);
    sess.solar_system_id = Some(sys_id);
    sess.corp_role = roles;
    let ship_id = grant(ctx, char_id, i64::from(sys_id), 606, InventoryFlag::None, 1);
    sess.ship_id = Some(ship_id);
    ctx.db.with_mut_snapshot(|snap| {
        let base_ch = snap.characters.values().next().cloned().expect("base char");
        let ch = snap.characters.entry(char_id).or_insert_with(|| eve_db::CharacterRecord {
            character_id: char_id, user_id: sess_id, name: format!("Pilot {char_id}"),
            corporation_id: corp_id, solar_system_id: sys_id, balance: IskCents::from_isk(1_000_000_000),
            active_ship_id: ship_id, ..base_ch
        });
        ch.corporation_id = corp_id;
        ch.solar_system_id = sys_id;
        ch.balance = IskCents::from_isk(1_000_000_000);
        ch.active_ship_id = ship_id;
        let corp = snap.corporations.entry(corp_id).or_insert_with(|| eve_db::CorporationRecord {
            corporation_id: corp_id, name: format!("Corp {corp_id}"), ticker: "CORP".into(),
            ceo_id: char_id, master_wallet: IskCents::from_isk(1_000_000_000), ..Default::default()
        });
        corp.members.insert(char_id);
        corp.member_roles.insert(char_id, roles);
    });
    sess
}

fn make_operational_structure(ctx: &ServerContext, structure_id: i64, owner_corp_id: i64, type_id: u32, sys_id: u32, name: &str) -> i64 {
    let hp = attr(ctx, type_id, life::ATTR_HP);
    let armor = attr(ctx, type_id, life::ATTR_ARMOR_HP);
    let shield = attr(ctx, type_id, life::ATTR_SHIELD_CAPACITY);
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(structure_id, UpwellStructureRecord {
            structure_id, type_id, owner_corp_id, solar_system_id: sys_id,
            name: name.into(), position: [1000.0, 2000.0, 3000.0],
            state: life::STATE_SHIELD_VULNERABLE.into(), quantum_core_installed: true,
            fuel_blocks: 10_000, shield_hp: shield, armor_hp: armor, hull_hp: hp,
            reinforced_until_unix: 0, last_damage_tick: 0, last_damage_unix: 0,
            self_destruct_at_unix: None,
        });
    });
    structure_id
}

#[allow(clippy::too_many_arguments)]
async fn spawn_target(
    ctx: &ServerContext,
    sys_id: u32,
    ball_id: i64,
    type_id: u32,
    pos: DVec3,
    shield_hp: f64,
    armor_hp: f64,
    hull_hp: f64,
    sig_radius: f64,
    char_id: Option<i64>,
    corp_id: i64,
) {
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let rad = ctx.sde.types.get(&type_id).map(|t| t.radius).unwrap_or(40.0).max(5.0);
    let mut cs = CombatState::new();
    cs.max_shield_hp = shield_hp.max(1.0);
    cs.max_armor_hp = armor_hp.max(1.0);
    cs.max_structure_hp = hull_hp.max(1.0);
    cs.shield_hp = shield_hp;
    cs.armor_hp = armor_hp;
    cs.structure_hp = hull_hp;
    cs.signature_radius = sig_radius;
    cs.max_capacitor = 10_000.0;
    cs.capacitor = 10_000.0;
    let meta = SlimItemMeta {
        owner_id: char_id.unwrap_or(corp_id),
        char_id,
        corp_id,
        alliance_id: 0,
        security_status: 0.0,
        is_cloaked: false,
        category_id: 6,
        group_id: 25,
        skin_id: None,
        skin_material_set_id: None,
        jump_target: None,
        ..SlimItemMeta::default()
    };
    let _ = sys_h
        .spawn_ball_with_meta(
            Ball {
                id: ball_id,
                type_id,
                position: pos,
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 2.0,
                mass: 1_500_000.0,
                radius: rad,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            cs,
            meta,
        )
        .await;
}

#[tokio::test]
async fn test_structure_reactions_moon_drill_and_access_lists() {
    // parity: feat:structures.service-reactions
    // parity: feat:structures.service-moon-drill
    // parity: feat:structures.access-lists
    eve_testkit::attest(&[
        "feat:structures.service-reactions",
        "feat:structures.service-moon-drill",
        "feat:structures.access-lists",
    ]);

    let ctx = ServerContext::new();
    ctx.set_clock(std::sync::Arc::new(eve_db::VirtualServerClock::with_fixed_base(1_800_000_000)));
    let roles = corp_roles::ROLE_DIRECTOR | corp_roles::ROLE_STATION_MANAGER;
    let mut sess = setup_pilot_session(&ctx, 7001, OWNER, CORP_ID, roles, JITA);
    let _ = setup_pilot_session(&ctx, 7002, OTHER_PILOT, CORP_ID, roles, JITA);
    let _ = setup_pilot_session(&ctx, 7003, BLOCKED_PILOT, RIVAL_CORP, 0, JITA);

    // 1. Access Lists: Create ACL, assign Member and Blocked roles, verify GetMembers
    let create_rep = rpc(&ctx, &mut sess, "ownerGroupManager", "Create", &[
        PyRep::String("Defense ACL".into()), PyRep::String("Structure defense access list".into())
    ]).await.expect("Create ACL");
    let group_id = create_rep.dict_get("groupID").and_then(|v| v.as_i64()).unwrap_or_else(|| create_rep.as_i64().unwrap_or(0));
    assert!(group_id > 0);

    rpc(&ctx, &mut sess, "ownerGroupManager", "UpdateMembershipTypes", &[
        PyRep::Integer(group_id), PyRep::List(vec![PyRep::Integer(OTHER_PILOT)]), PyRep::Integer(acl_rules::ACL_ROLE_MEMBER)
    ]).await.expect("Set member role");

    rpc(&ctx, &mut sess, "ownerGroupManager", "UpdateMembershipTypes", &[
        PyRep::Integer(group_id), PyRep::List(vec![PyRep::Integer(BLOCKED_PILOT)]), PyRep::Integer(acl_rules::ACL_ROLE_BLOCKED)
    ]).await.expect("Set blocked role");

    let members_rep = rpc(&ctx, &mut sess, "ownerGroupManager", "GetMembers", &[PyRep::Integer(group_id)]).await.expect("GetMembers");
    let PyRep::Dict(members) = members_rep else { panic!("Expected dict from GetMembers") };
    let get_role = |cid: i64| -> Option<i64> {
        members.iter().find(|(k, _)| k.as_i64() == Some(cid)).and_then(|(_, v)| v.as_i64())
    };
    assert_eq!(get_role(OTHER_PILOT), Some(acl_rules::ACL_ROLE_MEMBER));
    assert_eq!(get_role(BLOCKED_PILOT), Some(acl_rules::ACL_ROLE_BLOCKED));

    // 2. Reactions Service: High-Sec Athanor rejects reactions; Low-Sec Tatara with reactor accepts
    let hs_athanor = 1_000_000_700_001i64;
    make_operational_structure(&ctx, hs_athanor, CORP_ID, 35835, JITA, "HighSec Athanor");
    let ls_tatara = 1_000_000_700_002i64;
    make_operational_structure(&ctx, ls_tatara, CORP_ID, 35836, LOWSEC_SYS, "LowSec Tatara");

    // Fit Standup Composite (45537), Hybrid (45538), and Biochemical (45539) reactors
    for (idx, &rid) in [45537u32, 45538, 45539].iter().enumerate() {
        let flag = match idx {
            0 => InventoryFlag::ServiceSlot0,
            1 => InventoryFlag::ServiceSlot1,
            _ => InventoryFlag::ServiceSlot2,
        };
        let h_mod = grant(&ctx, CORP_ID, hs_athanor, rid, flag, 1);
        let l_mod = grant(&ctx, CORP_ID, ls_tatara, rid, flag, 1);
        ctx.db.with_mut_snapshot(|snap| {
            snap.module_online_states.insert(h_mod, true);
            snap.module_online_states.insert(l_mod, true);
        });
    }
    ctx.db.with_mut_snapshot(|snap| {
        let sk = snap.skills.entry(OWNER).or_default();
        sk.insert(45746, TrainedSkillRecord { type_id: 45746, level: 5, skill_points: 256_000.0 }); // Reactions
        sk.insert(45750, TrainedSkillRecord { type_id: 45750, level: 5, skill_points: 256_000.0 }); // Mass Reactions
    });

    let (react_bp_tid, react_mats) = ctx.sde.blueprints.values().find_map(|bp| {
        bp.activities.get("reaction").or_else(|| bp.activities.get("9")).filter(|a| !a.materials.is_empty() && !a.products.is_empty()).map(|a| (bp.blueprint_type_id, a.materials.clone()))
    }).expect("reaction blueprint in SDE");
    let hs_bp = grant(&ctx, OWNER, hs_athanor, react_bp_tid, InventoryFlag::Hangar, 1);
    let ls_bp = grant(&ctx, OWNER, ls_tatara, react_bp_tid, InventoryFlag::Hangar, 1);
    for rm in &react_mats {
        grant(&ctx, OWNER, hs_athanor, rm.type_id, InventoryFlag::Hangar, (rm.quantity as i32) * 10);
        grant(&ctx, OWNER, ls_tatara, rm.type_id, InventoryFlag::Hangar, (rm.quantity as i32) * 10);
    }

    // High-sec reaction job rejected
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&OWNER) { ch.structure_id = Some(hs_athanor); ch.station_id = None; }
    });
    sess.structure_id = Some(hs_athanor);
    sess.station_id = None;
    let hs_react_err = rpc(&ctx, &mut sess, "industryManager", "InstallJob", &[PyRep::keyval([
        ("blueprintID", PyRep::Integer(hs_bp)), ("blueprintTypeID", PyRep::Integer(i64::from(react_bp_tid))),
        ("facilityID", PyRep::Integer(hs_athanor)), ("activityID", PyRep::Integer(9)), ("runs", PyRep::Integer(1)),
    ])]).await.expect_err("high-sec reaction forbidden");
    assert_eq!(hs_react_err.code, "ReactionsForbiddenInHighSec");

    // Low-sec reaction job installed successfully
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&OWNER) {
            ch.solar_system_id = LOWSEC_SYS; ch.structure_id = Some(ls_tatara); ch.station_id = None;
        }
    });
    sess.solar_system_id = Some(LOWSEC_SYS);
    sess.structure_id = Some(ls_tatara);
    sess.station_id = None;
    let ls_react_job = rpc(&ctx, &mut sess, "industryManager", "InstallJob", &[PyRep::keyval([
        ("blueprintID", PyRep::Integer(ls_bp)), ("blueprintTypeID", PyRep::Integer(i64::from(react_bp_tid))),
        ("facilityID", PyRep::Integer(ls_tatara)), ("activityID", PyRep::Integer(9)), ("runs", PyRep::Integer(1)),
    ])]).await.expect("low-sec reaction job");
    assert_eq!(ls_react_job.dict_get("activityID").and_then(|v| v.as_i64()), Some(9));

    // 3. Moon Drill Service: Start extraction, validation of duration, corp query, cancellation
    let moon_drill_mod = grant(&ctx, CORP_ID, ls_tatara, 35894, InventoryFlag::ServiceSlot3, 1);
    ctx.db.with_mut_snapshot(|snap| { snap.module_online_states.insert(moon_drill_mod, true); });

    // Duration outside [6 days..=56 days] rejected
    let too_short = rpc(&ctx, &mut sess, "moonExtractions", "StartNewExtraction", &[
        PyRep::Integer(ls_tatara), PyRep::Integer(86_400) // 1 day < 6 days
    ]).await.expect_err("too short");
    assert_eq!(too_short.code, "InvalidExtractionDuration");

    let too_long = rpc(&ctx, &mut sess, "moonExtractions", "StartNewExtraction", &[
        PyRep::Integer(ls_tatara), PyRep::Integer(5_000_000) // ~58 days > 56 days
    ]).await.expect_err("too long");
    assert_eq!(too_long.code, "InvalidExtractionDuration");

    // Valid 6-day extraction
    let start_res = rpc(&ctx, &mut sess, "moonExtractions", "StartNewExtraction", &[
        PyRep::Integer(ls_tatara), PyRep::Integer(518_400) // 6 days
    ]).await.expect("valid 6-day extraction");
    assert_eq!(start_res.dict_get("status").and_then(|v| v.as_str()), Some("Started"));

    let corp_exts = rpc(&ctx, &mut sess, "moonExtractions", "GetExtractionsForCorp", &[]).await.expect("GetExtractionsForCorp");
    let PyRep::List(ext_list) = corp_exts else { panic!("expected list") };
    assert!(ext_list.iter().any(|e| e.dict_get("structureID").and_then(|v| v.as_i64()) == Some(ls_tatara)));

    let cancel_res = rpc(&ctx, &mut sess, "moonExtractions", "CancelExtraction", &[PyRep::Integer(ls_tatara)]).await.expect("CancelExtraction");
    assert_eq!(cancel_res, PyRep::Bool(true));
}

#[tokio::test]
async fn test_structure_repair_timer_damage_caps_and_vulnerability_schedule() {
    // parity: feat:structures.repair-timer
    // parity: feat:structures.damage-cap
    // parity: feat:structures.vulnerability-schedule
    eve_testkit::attest(&[
        "feat:structures.repair-timer",
        "feat:structures.damage-cap",
        "feat:structures.vulnerability-schedule",
    ]);

    let ctx = ServerContext::new();
    ctx.set_clock(std::sync::Arc::new(eve_db::VirtualServerClock::with_fixed_base(1_800_000_000)));
    let roles = corp_roles::ROLE_DIRECTOR | corp_roles::ROLE_STATION_MANAGER;
    let mut sess = setup_pilot_session(&ctx, 7002, OWNER, CORP_ID, roles, LOWSEC_SYS);

    // 1. Damage Cap: Type limits hierarchy and DPS clipping
    let ast_armor_cap = attr(&ctx, 35832, life::ATTR_ARMOR_DAMAGE_LIMIT);
    let fort_armor_cap = attr(&ctx, 35833, life::ATTR_ARMOR_DAMAGE_LIMIT);
    let ks_armor_cap = attr(&ctx, 35834, life::ATTR_ARMOR_DAMAGE_LIMIT);
    assert!(ks_armor_cap > fort_armor_cap && fort_armor_cap > ast_armor_cap);
    assert!((ast_armor_cap - 5000.0).abs() < 1e-3);
    assert!((fort_armor_cap - 15000.0).abs() < 1e-3);
    assert!((ks_armor_cap - 75000.0).abs() < 1e-3);

    let struct_id = 1_000_000_700_010i64;
    make_operational_structure(&ctx, struct_id, CORP_ID, 35832, LOWSEC_SYS, "Defense Astrahus");

    let now = ctx.now_unix();
    let dmg_cap_res = ctx.db.with_mut_snapshot(|snap| {
        snap.structures.get_mut(&struct_id).unwrap().state = life::STATE_ARMOR_VULNERABLE.into();
        let mut ext = load_ext_state(snap);
        life::apply_structure_damage(&ctx, snap, &mut ext, struct_id, 1.0e9, 5.0, OWNER, now).expect("damage")
    });
    assert_eq!(dmg_cap_res.layer, "armor");
    assert_eq!(dmg_cap_res.applied_damage, ast_armor_cap * 5.0, "1e9 raw damage over 5s capped at SDE armor limit");

    let dmg_under_res = ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        life::apply_structure_damage(&ctx, snap, &mut ext, struct_id, 10.0, 5.0, OWNER, now).expect("damage under cap")
    });
    assert_eq!(dmg_under_res.applied_damage, 10.0, "damage under cap is applied unscaled");

    // 2. Repair Timer: 15-minute repair timer with full restoration and pause threshold
    let full_shield = attr(&ctx, 35832, life::ATTR_SHIELD_CAPACITY);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(s) = snap.structures.get_mut(&struct_id) {
            s.state = life::STATE_SHIELD_VULNERABLE.into();
            s.shield_hp = full_shield;
        }
    });

    let t0 = ctx.now_unix();
    let light_hit = ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        let res = life::apply_structure_damage(&ctx, snap, &mut ext, struct_id, 100.0, 1.0, OWNER, t0).expect("light hit");
        save_ext_state(snap, &ext);
        res
    });
    assert!(!light_hit.repair_paused);
    assert!(ctx.db.with_snapshot(|s| s.structures[&struct_id].shield_hp) < full_shield);

    // At 14m (< 15m), structure is still damaged
    ctx.advance_virtual_clock(life::REPAIR_SHIELD_ARMOR_SECS - 60);
    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        life::sync_structure(&ctx, snap, &mut ext, struct_id, ctx.now_unix());
        save_ext_state(snap, &ext);
    });
    assert!(ctx.db.with_snapshot(|s| s.structures[&struct_id].shield_hp) < full_shield);

    // At 15m, repair cycle completes and restores full shield capacity
    ctx.advance_virtual_clock(60);
    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        life::sync_structure(&ctx, snap, &mut ext, struct_id, ctx.now_unix());
        save_ext_state(snap, &ext);
    });
    assert_eq!(ctx.db.with_snapshot(|s| s.structures[&struct_id].shield_hp), full_shield);

    // Heavy fire at/above pause threshold freezes the timer
    let pause_threshold = attr(&ctx, 35832, life::ATTR_PAUSE_SHIELD_REPAIR_DPS);
    let t_heavy = ctx.now_unix();
    let heavy_hit = ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        let res = life::apply_structure_damage(&ctx, snap, &mut ext, struct_id, pause_threshold * 10.0, 10.0, OWNER, t_heavy).expect("heavy");
        save_ext_state(snap, &ext);
        res
    });
    assert!(heavy_hit.repair_paused);

    // 3. Vulnerability Schedule: Validation, 7-day change delay, and delayed application
    let bad_hour = rpc(&ctx, &mut sess, "structureVulnerability", "UpdateReinforceTimesFromClient", &[
        PyRep::Integer(struct_id), PyRep::Integer(29), PyRep::Integer(2)
    ]).await.expect_err("hour 29 invalid");
    assert_eq!(bad_hour.code, "InvalidReinforceSchedule");

    // Non-director pilot rejected
    let mut grunt_sess = setup_pilot_session(&ctx, 7003, OTHER_PILOT, CORP_ID, 0, LOWSEC_SYS);
    let no_role = rpc(&ctx, &mut grunt_sess, "structureVulnerability", "UpdateReinforceTimesFromClient", &[
        PyRep::Integer(struct_id), PyRep::Integer(19), PyRep::Integer(4)
    ]).await.expect_err("no role");
    assert_eq!(no_role.code, "InsufficientCorpRoles");

    let sched_res = rpc(&ctx, &mut sess, "structureVulnerability", "UpdateReinforceTimesFromClient", &[
        PyRep::Integer(struct_id), PyRep::Integer(19), PyRep::Integer(4)
    ]).await.expect("valid schedule");
    assert_eq!(kv_i64(&sched_res, "nextReinforceHour"), 19);
    assert_eq!(kv_i64(&sched_res, "nextReinforceWeekday"), 4);
    let apply_after = kv_i64(&sched_res, "applyAfterUnix");
    assert_eq!(apply_after, ctx.now_unix() as i64 + 7 * 86_400);

    // Cease fire and let repair timer restore shield to full before schedule change
    ctx.advance_virtual_clock(life::REPAIR_SHIELD_ARMOR_SECS + 10);
    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        life::sync_structure(&ctx, snap, &mut ext, struct_id, ctx.now_unix());
        save_ext_state(snap, &ext);
    });
    assert_eq!(ctx.db.with_snapshot(|s| s.structures[&struct_id].shield_hp), full_shield);

    // 7 days pass -> new schedule applied to active vulnerability windows
    ctx.advance_virtual_clock(7 * 86_400 + 10);
    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        life::sync_structure(&ctx, snap, &mut ext, struct_id, ctx.now_unix());
        save_ext_state(snap, &ext);
    });
    let active_win = ctx.db.with_snapshot(|snap| snap.structure_vulnerability_windows.get(&struct_id).copied());
    assert_eq!(active_win, Some((19, 4)));
}

#[tokio::test]
async fn test_structure_control_weapons_killmail_and_asset_safety() {
    // parity: feat:structures.structure-killmail
    // parity: feat:structures.asset-safety-auto
    // parity: feat:structures.structure-control
    // parity: feat:structures.structure-weapons
    eve_testkit::attest(&[
        "feat:structures.structure-killmail",
        "feat:structures.asset-safety-auto",
        "feat:structures.structure-control",
        "feat:structures.structure-weapons",
    ]);

    let ctx = ServerContext::new();
    ctx.set_clock(std::sync::Arc::new(eve_db::VirtualServerClock::with_fixed_base(1_800_000_000)));
    let roles = corp_roles::ROLE_DIRECTOR | corp_roles::ROLE_STATION_MANAGER;
    let mut sess = setup_pilot_session(&ctx, 7004, OWNER, CORP_ID, roles, LOWSEC_SYS);
    let _ = setup_pilot_session(&ctx, 7005, OTHER_PILOT, RIVAL_CORP, 0, LOWSEC_SYS);
    let _ = setup_pilot_session(&ctx, 7006, BLOCKED_PILOT, RIVAL_CORP, 0, LOWSEC_SYS);

    let fort_id = 1_000_000_700_020i64;
    make_operational_structure(&ctx, fort_id, CORP_ID, 35833, LOWSEC_SYS, "Armed Fortizar");

    // 1. Structure Control: TakeControl, GetStructurePilot, ReleaseControl
    assert_eq!(rpc(&ctx, &mut sess, "structureControl", "GetStructurePilot", &[PyRep::Integer(fort_id)]).await.expect("initial pilot"), PyRep::None);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&OWNER) { ch.structure_id = Some(fort_id); ch.station_id = None; }
    });
    sess.structure_id = Some(fort_id);
    sess.station_id = None;

    rpc(&ctx, &mut sess, "structureControl", "BoardStructure", &[PyRep::Integer(fort_id)]).await.expect("BoardStructure");
    rpc(&ctx, &mut sess, "structureControl", "TakeControl", &[PyRep::Integer(fort_id)]).await.expect("TakeControl");
    assert_eq!(rpc(&ctx, &mut sess, "structureControl", "GetStructurePilot", &[PyRep::Integer(fort_id)]).await.expect("pilot"), PyRep::Integer(OWNER));

    rpc(&ctx, &mut sess, "structureControl", "ReleaseControl", &[PyRep::Integer(fort_id)]).await.expect("ReleaseControl");
    assert_eq!(rpc(&ctx, &mut sess, "structureControl", "GetStructurePilot", &[PyRep::Integer(fort_id)]).await.expect("released pilot"), PyRep::None);
    let release_twice = rpc(&ctx, &mut sess, "structureControl", "ReleaseControl", &[PyRep::Integer(fort_id)]).await.expect_err("second release");
    assert_eq!(release_twice.code, "NotControllingStructure");

    rpc(&ctx, &mut sess, "structureControl", "TakeControl", &[PyRep::Integer(fort_id)]).await.expect("RetakeControl");

    // 2. Structure Weapons: Guided Bomb Launcher & Point Defense Battery
    let gbl_mod = grant(&ctx, CORP_ID, fort_id, 35923, InventoryFlag::HiSlot0, 1); // Standup Guided Bomb Launcher I
    let pdb_mod = grant(&ctx, CORP_ID, fort_id, 35924, InventoryFlag::HiSlot1, 1); // Standup Point Defense Battery I
    let _ammo_item = grant(&ctx, CORP_ID, fort_id, 37850, InventoryFlag::HiSlot0, 5); // Standup Light Guided Bomb
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(gbl_mod, true);
        snap.module_online_states.insert(pdb_mod, true);
    });

    let primary_cruiser = 962_001i64;
    let nearby_frigate = 962_002i64;
    let far_battleship = 962_003i64;
    spawn_target(&ctx, LOWSEC_SYS, primary_cruiser, 626, DVec3::new(60_000.0, 0.0, 0.0), 20_000.0, 20_000.0, 20_000.0, 140.0, Some(OTHER_PILOT), RIVAL_CORP).await;
    spawn_target(&ctx, LOWSEC_SYS, nearby_frigate, 587, DVec3::new(60_000.0, 12_000.0, 0.0), 20_000.0, 20_000.0, 20_000.0, 35.0, Some(BLOCKED_PILOT), RIVAL_CORP).await;
    spawn_target(&ctx, LOWSEC_SYS, far_battleship, 642, DVec3::new(60_000.0, 35_000.0, 0.0), 20_000.0, 20_000.0, 20_000.0, 420.0, None, RIVAL_CORP).await;

    // Firing without lock rejected with TargetNotLocked
    let unlock_err = rpc(&ctx, &mut sess, "dogmaIM", "Activate", &[
        PyRep::Integer(gbl_mod), PyRep::String("defaultEffect".into()), PyRep::Integer(primary_cruiser)
    ]).await.expect_err("unlocked target");
    assert_eq!(unlock_err.code, "TargetNotLocked");

    let sys_h = ctx.systems.get_or_spawn(LOWSEC_SYS);
    rpc(&ctx, &mut sess, "dogmaIM", "AddTarget", &[PyRep::Integer(primary_cruiser)]).await.expect("AddTarget cruiser");
    let mut cruiser_locked = false;
    for _ in 0..240 {
        sys_h.step_manual(1.0).await.unwrap();
        if sys_h.get_target_locks(fort_id).await.is_ok_and(|l| l.iter().any(|t| t.target_id == primary_cruiser && t.locked)) {
            cruiser_locked = true;
            break;
        }
    }
    assert!(cruiser_locked, "Fortizar failed to lock primary_cruiser within 240s");
    rpc(&ctx, &mut sess, "dogmaIM", "Activate", &[
        PyRep::Integer(gbl_mod), PyRep::String("defaultEffect".into()), PyRep::Integer(primary_cruiser)
    ]).await.expect("Fire Light Guided Bomb");
    for _ in 0..30 {
        sys_h.step_manual(1.0).await.unwrap();
    }

    let c_shield = sys_h.get_combat_state(primary_cruiser).await.unwrap().unwrap().shield_hp;
    let f_shield = sys_h.get_combat_state(nearby_frigate).await.unwrap().unwrap().shield_hp;
    let far_shield = sys_h.get_combat_state(far_battleship).await.unwrap().unwrap().shield_hp;
    let c_lost = 20_000.0 - c_shield;
    let f_lost = 20_000.0 - f_shield;
    let far_lost = 20_000.0 - far_shield;
    assert!(c_lost > f_lost * 1.5, "Cruiser with 140m sig takes greater bomb damage ({c_lost}) than 35m sig frigate ({f_lost})");
    assert!(f_lost > 0.0, "Nearby frigate inside 20km AoE must take blast damage");
    assert!(far_lost.abs() < 1e-6, "Far battleship at 35km (>20km AoE) must take zero bomb damage");

    // 3. Structure Killmail and Automatic Asset Safety (Empire/K-Space vs Wormhole/J-Space)
    let cargo = grant(&ctx, OWNER, fort_id, 34, InventoryFlag::Hangar, 2_500);
    let kspace_pos = ctx.db.with_snapshot(|s| s.structures[&fort_id].position);
    let expected_npc_station = life::nearest_npc_station_from_pos(&ctx, LOWSEC_SYS, DVec3::from_array(kspace_pos));

    let kspace_kill = ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        let res = life::destroy_structure(&ctx, snap, &mut ext, fort_id, OTHER_PILOT, ctx.now_unix());
        save_ext_state(snap, &ext);
        res
    });
    assert!(kspace_kill.killmail_id.is_some(), "destruction must produce killmail");
    let km_id = kspace_kill.killmail_id.unwrap();
    ctx.db.with_snapshot(|snap| {
        let km = snap.killmails.get(&km_id).expect("killmail in DB");
        assert_eq!(km.ship_type_id, 35833);
        assert_eq!(km.victim_character_id, CORP_ID);
        assert_eq!(km.attacker_character_id, OTHER_PILOT);
        assert_eq!(km.solar_system_id, LOWSEC_SYS);
        assert_eq!(km.victim_corp_id, CORP_ID);
        assert!(km.total_value_isk >= 0.0);
    });
    let exp_core_tid = life::structure_profile(&ctx, 35833).and_then(|p| p.core_type_id).unwrap();
    assert!(kspace_kill.dropped_core_item_id.is_some(), "quantum core deed must drop into space/wreck");
    let dropped_core_id = kspace_kill.dropped_core_item_id.unwrap();
    ctx.db.with_snapshot(|snap| {
        let core_item = snap.items.get(&dropped_core_id).expect("dropped core in snap");
        assert_eq!(core_item.type_id, exp_core_tid);
        assert_eq!(core_item.location_id, kspace_kill.wreck_item_id.unwrap());
    });
    assert_eq!(kspace_kill.asset_safety_wrap_ids.len(), 2, "k-space destruction creates asset safety wraps for pilot and corp");
    let owner_wrap_id = kspace_kill.asset_safety_wrap_ids.iter().copied().find(|&wid| {
        ctx.db.with_snapshot(|snap| {
            load_ext_state(snap).asset_safety_wraps.get(&wid).map(|w| w.owner_id) == Some(OWNER)
        })
    }).expect("owner wrap");
    ctx.db.with_snapshot(|snap| {
        let ext = load_ext_state(snap);
        let wrap = ext.asset_safety_wraps.get(&owner_wrap_id).expect("wrap record");
        assert_eq!(wrap.owner_id, OWNER);
        assert!(wrap.item_ids.contains(&cargo));
        assert_eq!(wrap.nearest_npc_station_id, expected_npc_station);
        assert_eq!(snap.items.get(&cargo).map(|i| (i.location_id, i.flag)), Some((owner_wrap_id, InventoryFlag::Locked)));
    });

    // Wormhole destruction: assets drop directly into space with zero asset safety wraps
    let wh_fort_id = 1_000_000_700_030i64;
    make_operational_structure(&ctx, wh_fort_id, CORP_ID, 35833, WH_SYS, "J-Space Fortizar");
    let wh_cargo = grant(&ctx, OWNER, wh_fort_id, 34, InventoryFlag::Hangar, 800);
    let wh_kill = ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        let res = life::destroy_structure(&ctx, snap, &mut ext, wh_fort_id, OTHER_PILOT, ctx.now_unix());
        save_ext_state(snap, &ext);
        res
    });
    assert!(wh_kill.asset_safety_wrap_ids.is_empty(), "wormhole structures have no asset safety");
    assert!(wh_kill.dropped_item_ids.contains(&wh_cargo), "wormhole structure drops hangar items directly to space");
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items.get(&wh_cargo).map(|i| i.location_id), Some(i64::from(WH_SYS)));
    });
}
