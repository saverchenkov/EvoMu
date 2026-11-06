//! Unit tests for `structure_lifecycle_rpc` (`Anchor`, live combat precheck/damage, structure fitting, fuel jettison, deliveries, and cargo dropoff).

#![forbid(unsafe_code)]

use super::*;
use crate::router::ServerContext;
use eve_db::{
    CorpMemberRoleAssignment, CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents,
    UpwellStructureRecord,
};
use eve_destiny::{Ball, CombatState, DamageVector, MovementMode, SlimItemMeta};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::{InvType, SdeRegistry};
use glam::DVec3;
use std::collections::HashSet;
use std::sync::{Arc, OnceLock};

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn test_ctx() -> ServerContext {
    if let Some(sde) = SHARED_SDE.get() {
        ServerContext::with_sde_and_repository(
            Arc::clone(sde),
            Arc::new(eve_db::WorldRepository::with_demo_seed()),
        )
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
    } else {
        let ctx = ServerContext::new();
        let _ = SHARED_SDE.set(Arc::clone(&ctx.sde));
        ctx
    }
}

fn clone_type_with(
    sde: &mut Arc<SdeRegistry>,
    base_tid: u32,
    new_tid: u32,
    group_id: u32,
    category_id: u32,
    volume: f64,
    capacity: f64,
    attrs: &[(u16, f64)],
) {
    let reg = Arc::make_mut(sde);
    let mut rec: InvType = reg.types.values().next().cloned().expect("base sde type");
    if let Some(b) = reg.types.get(&base_tid) {
        rec = b.clone();
    }
    rec.type_id = new_tid;
    rec.group_id = group_id;
    rec.category_id = category_id;
    rec.name = format!("StructRpc_{new_tid}");
    rec.volume = volume;
    rec.capacity = capacity;
    rec.radius = 5_000.0;
    rec.mass = 1.0e11;
    rec.default_attributes.clear();
    for &(k, v) in attrs {
        rec.default_attributes.insert(k, v);
    }
    reg.types.insert(new_tid, rec);
}

fn seed_astrahus_type(ctx: &mut ServerContext, tid: u32, disallow_highsec: bool) {
    let mut attrs = vec![
        (life::ATTR_RIG_SIZE, 2.0),
        (life::ATTR_SHIELD_CAPACITY, 500_000.0),
        (life::ATTR_ARMOR_HP, 500_000.0),
        (life::ATTR_HP, 500_000.0),
        (life::ATTR_HI_SLOTS, 4.0),
        (life::ATTR_MED_SLOTS, 4.0),
        (life::ATTR_LOW_SLOTS, 4.0),
        (life::ATTR_RIG_SLOTS, 3.0),
        (life::ATTR_SERVICE_SLOTS, 3.0),
        (life::ATTR_SHIELD_DAMAGE_LIMIT, 5_000.0),
        (life::ATTR_ARMOR_DAMAGE_LIMIT, 5_000.0),
        (life::ATTR_STRUCTURE_DAMAGE_LIMIT, 5_000.0),
    ];
    if disallow_highsec {
        attrs.push((life::ATTR_DISALLOW_IN_HIGHSEC, 1.0));
    }
    clone_type_with(
        &mut ctx.sde,
        35832,
        tid,
        1657,
        life::CATEGORY_STRUCTURE,
        80_000.0,
        0.0,
        &attrs,
    );
}

fn make_session(char_id: i64, corp_id: i64, sys_id: u32, ship_id: Option<i64>, role: u64) -> SessionState {
    let mut s = SessionState::new(710);
    s.user_id = Some(char_id as u64);
    s.character_id = Some(char_id);
    s.corporation_id = Some(corp_id);
    s.solar_system_id = Some(sys_id);
    s.ship_id = ship_id;
    s.corp_role = role;
    s
}

fn seed_char_corp_ship(
    ctx: &ServerContext,
    char_id: i64,
    corp_id: i64,
    sys_id: u32,
    ship_id: i64,
    is_ceo: bool,
) {
    ctx.db.with_mut_snapshot(|snap| {
        let mut corp = CorporationRecord {
            corporation_id: corp_id,
            name: format!("Corp {corp_id}"),
            ticker: "TST".into(),
            ceo_id: if is_ceo { char_id } else { char_id + 999 },
            hq_station_id: Some(60003760),
            tax_rate: 0.05,
            master_wallet: IskCents(10_000_000),
            members: HashSet::from([char_id]),
            ..Default::default()
        };
        if !is_ceo {
            corp.member_roles
                .insert(char_id, eve_db::corp_roles::ROLE_STATION_MANAGER);
        }
        snap.corporations.insert(corp_id, corp);
        let mut ch = snap.characters.values().next().cloned().expect("demo char");
        ch.character_id = char_id;
        ch.name = format!("Pilot {char_id}");
        ch.corporation_id = corp_id;
        ch.alliance_id = None;
        ch.solar_system_id = sys_id;
        ch.station_id = None;
        ch.structure_id = None;
        ch.active_ship_id = ship_id;
        ch.home_station_id = 60003760;
        ch.balance = IskCents(10_000_000);
        snap.characters.insert(char_id, ch);
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 670,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
}

fn seed_structure(
    ctx: &ServerContext,
    structure_id: i64,
    type_id: u32,
    corp_id: i64,
    sys_id: u32,
    state: &str,
    fuel_blocks: u32,
) {
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            structure_id,
            UpwellStructureRecord {
                structure_id,
                type_id,
                owner_corp_id: corp_id,
                solar_system_id: sys_id,
                name: format!("Citadel {structure_id}"),
                position: [2_000_000_000.0, 0.0, 0.0],
                state: state.to_string(),
                quantum_core_installed: true,
                fuel_blocks,
                shield_hp: 500_000.0,
                armor_hp: 500_000.0,
                hull_hp: 500_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
    });
}

async fn spawn_ship_ball(ctx: &ServerContext, sys_id: u32, ship_id: i64, pos: DVec3) {
    let sys = ctx.systems.get_or_spawn(sys_id);
    let ball = Ball {
        id: ship_id,
        type_id: 670,
        position: pos,
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 2.0,
        mass: 1_000_000.0,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let _ = sys
        .spawn_ball_with_meta(ball, CombatState::new(), SlimItemMeta::default())
        .await;
}

#[test]
fn structure_manager_role_checks_gm_session_and_corp_assignments() {
    let ctx = test_ctx();
    let mut sess = make_session(9101, 98101, 30000142, Some(10001), 0);
    seed_char_corp_ship(&ctx, 9101, 98101, 30000142, 10001, false);
    sess.corp_role = 0;

    let station_mgr_only = ctx.db.with_snapshot(|s| has_structure_manager_role(s, &sess, 9101, 98101));
    assert!(station_mgr_only);

    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.get_mut(&98101).unwrap().member_roles.clear();
        snap.corp_role_assignments.insert(
            (98101, 9101),
            CorpMemberRoleAssignment {
                roles: eve_db::corp_roles::ROLE_DIRECTOR,
                ..Default::default()
            },
        );
    });
    assert!(ctx.db.with_snapshot(|s| has_structure_manager_role(s, &sess, 9101, 98101)));

    ctx.db.with_mut_snapshot(|snap| {
        snap.corp_role_assignments.clear();
    });
    assert!(!ctx.db.with_snapshot(|s| has_structure_manager_role(s, &sess, 9101, 98101)));
    sess.role = eve_machonet::service_roles::ROLE_GMH;
    assert!(ctx.db.with_snapshot(|s| has_structure_manager_role(s, &sess, 9101, 98101)));
}

#[tokio::test(flavor = "current_thread")]
async fn anchor_validates_docked_ship_cargo_highsec_palatine_and_succeeds_v24() {
    let mut ctx = test_ctx();
    let tid = 910_001u32;
    let hs_blocked_tid = 910_002u32;
    seed_astrahus_type(&mut ctx, tid, false);
    seed_astrahus_type(&mut ctx, hs_blocked_tid, true);

    let sys_id = 30_000_144_u32;
    seed_char_corp_ship(&ctx, 9201, 98_000_201, sys_id, 20001, true);
    let far_pos = DVec3::new(9_000_000_000.0, 5_000_000_000.0, -3_000_000_000.0);
    spawn_ship_ball(&ctx, sys_id, 20001, far_pos).await;

    // Docked ship fails immediately
    let mut docked_sess = make_session(9201, 98_000_201, sys_id, Some(20001), 1);
    docked_sess.station_id = Some(60003760);
    let err_docked = handle_anchor(&ctx, &mut docked_sess, &[PyRep::Integer(i64::from(tid))])
        .await
        .unwrap_err();
    assert_eq!(err_docked.code, "ShipDocked");

    // Highsec blocked hull fails
    let mut sess = make_session(9201, 98_000_201, sys_id, Some(20001), 1);
    let err_hs = handle_anchor(&ctx, &mut sess, &[PyRep::Integer(i64::from(hs_blocked_tid))])
        .await
        .unwrap_err();
    assert_eq!(err_hs.code, "CannotAnchorInHighSec");

    // Wrong owner / wrong location cargo item fails
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            55501,
            InventoryItemRecord {
                item_id: 55501,
                type_id: tid,
                owner_id: 9201,
                location_id: 60003760,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            55502,
            InventoryItemRecord {
                item_id: 55502,
                type_id: tid,
                owner_id: 9201,
                location_id: 20001,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    assert_eq!(
        handle_anchor(&ctx, &mut sess, &[PyRep::Integer(55501)])
            .await
            .unwrap_err()
            .code,
        "ItemNotInCargo"
    );

    // Non-director without role cannot anchor
    sess.corp_role = 0;
    ctx.db.with_mut_snapshot(|snap| {
        let corp = snap.corporations.get_mut(&98_000_201).unwrap();
        corp.ceo_id = 999999;
        corp.member_roles.clear();
    });
    assert_eq!(
        handle_anchor(&ctx, &mut sess, &[PyRep::Integer(55502)])
            .await
            .unwrap_err()
            .code,
        "InsufficientCorpRoles"
    );
    sess.corp_role = 1;
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.get_mut(&98_000_201).unwrap().ceo_id = 9201;
    });

    // V24 10-arg Anchor consumes item 55502, creates structure & spawns ball
    // `(itemID, x, z, yaw, profileID, reinforceWeekday, reinforceHour, name, description, extraVars)`
    let args = vec![
        PyRep::Integer(55502),
        PyRep::None,
        PyRep::None,
        PyRep::Real(0.0),
        PyRep::Integer(1),
        PyRep::Integer(2),
        PyRep::Integer(18),
        PyRep::String("Fort Bravo".into()),
        PyRep::String("Main HQ".into()),
        PyRep::Dict(vec![]),
    ];
    let res = handle_anchor(&ctx, &mut sess, &args).await.expect("v24 anchor");
    let PyRep::KeyVal(kv) = res else {
        panic!("expected KeyVal");
    };
    let new_sid = kv.get("structureID").and_then(|v| v.as_i64()).expect("structure id");
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&55502));
        let st = snap.structures.get(&new_sid).expect("anchored structure");
        assert_eq!(st.name, "Fort Bravo");
        assert_eq!(st.state, life::STATE_ANCHOR_VULNERABLE);
        let ext = load_ext_state(snap);
        assert_eq!(ext.descriptions.get(&new_sid).map(String::as_str), Some("Main HQ"));
        assert_eq!(snap.structure_vulnerability_windows.get(&new_sid).copied(), Some((18, 2)));
    });
    let ball = ctx
        .systems
        .get_or_spawn(sys_id)
        .get_ball_snapshot(new_sid)
        .await
        .ok()
        .flatten();
    assert!(ball.is_some());
}

#[tokio::test(flavor = "current_thread")]
async fn attack_precheck_and_live_combat_damage_enforce_rules_and_destroy() {
    let mut ctx = test_ctx();
    let tid = 910_010u32;
    seed_astrahus_type(&mut ctx, tid, false);
    seed_char_corp_ship(&ctx, 9301, 98301, 30000142, 30001, true);
    seed_char_corp_ship(&ctx, 9302, 98302, 30000142, 30002, true);
    let now_u = ctx.now_unix();
    seed_structure(&ctx, 10_000_930_001, tid, 98301, 30000142, life::STATE_SHIELD_VULNERABLE, 100);
    seed_structure(&ctx, 10_000_930_002, tid, 98301, 30000142, life::STATE_ARMOR_REINFORCED, 100);
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.get_mut(&10_000_930_002).unwrap().reinforced_until_unix = now_u + 86_400;
    });

    // Friendly corp blocked
    let own = validate_upwell_structure_attack_precheck(&ctx, 30000142, 9301, 10_000_930_001, now_u)
        .unwrap_err();
    assert_eq!(own.code, "CannotAttackFriendlyStructure");

    // Reinforced blocked
    let reinf = validate_upwell_structure_attack_precheck(&ctx, 30000142, 9302, 10_000_930_002, now_u)
        .unwrap_err();
    assert_eq!(reinf.code, "StructureReinforcedInvulnerable");

    // Highsec without war blocked; with war allowed
    let sys_sec = ctx
        .sde
        .solar_systems
        .get(&30000142)
        .map(|s| s.security_status)
        .unwrap_or(0.95);
    if sys_sec >= 0.45 {
        let concord = validate_upwell_structure_attack_precheck(&ctx, 30000142, 9302, 10_000_930_001, now_u)
            .unwrap_err();
        assert_eq!(concord.code, "IllegalHighSecStructureAttack");
    }

    // Apply live combat damage to HullVulnerable structure in lowsec -> transitions to Destroyed
    let low_sys = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| s.security_status < 0.45 && s.solar_system_id < 31_000_000)
        .map(|s| s.solar_system_id)
        .unwrap_or(30000142);
    seed_structure(&ctx, 10_000_930_003, tid, 98301, low_sys, life::STATE_HULL_VULNERABLE, 50);
    ctx.db.with_mut_snapshot(|snap| {
        let st = snap.structures.get_mut(&10_000_930_003).unwrap();
        st.shield_hp = 0.0;
        st.armor_hp = 0.0;
        st.hull_hp = 1_000.0;
        st.last_damage_unix = now_u;
        let mut ext = load_ext_state(snap);
        ext.lifecycle.entry(10_000_930_003).or_default().repair_until_unix = Some(now_u + 900);
        save_ext_state(snap, &ext);
    });
    let sys_h = ctx.systems.get_or_spawn(low_sys);
    let (_, destroyed, _) = apply_damage_structure_or_ball(
        &ctx,
        &sys_h,
        low_sys,
        9302,
        10_000_930_003,
        DamageVector::new(0.0, 0.0, 2_000.0, 0.0),
        1.0,
        now_u,
    )
    .await
    .expect("damage applied")
    .expect("structure hit");
    assert!(destroyed);
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.structures[&10_000_930_003].state, life::STATE_DESTROYED);
    });
}

#[tokio::test(flavor = "current_thread")]
async fn fitting_online_fuel_jettison_delivery_and_cargo_dropoff_work() {
    let mut ctx = test_ctx();
    let st_tid = 910_020u32;
    let mod_tid = 910_021u32;
    let svc_tid = 910_022u32;
    let fuel_tid = 4247u32;
    seed_astrahus_type(&mut ctx, st_tid, false);
    clone_type_with(
        &mut ctx.sde,
        35832,
        mod_tid,
        1329,
        66,
        500.0,
        0.0,
        &[(1298, 1657.0)],
    );
    clone_type_with(
        &mut ctx.sde,
        35832,
        svc_tid,
        1415,
        66,
        2_000.0,
        0.0,
        &[
            (life::ATTR_SERVICE_FUEL_GROUP, 1136.0),
            (life::ATTR_SERVICE_FUEL_ONLINE, 40.0),
            (life::ATTR_SERVICE_FUEL_PER_HOUR, 5.0),
        ],
    );
    clone_type_with(&mut ctx.sde, 35832, fuel_tid, 1136, 14, 5.0, 0.0, &[]);

    let sid = 10_000_940_001i64;
    seed_char_corp_ship(&ctx, 9401, 98401, 30000142, 40001, true);
    seed_char_corp_ship(&ctx, 9302, 98302, 30000142, 40002, true);
    seed_structure(&ctx, sid, st_tid, 98401, 30000142, life::STATE_SHIELD_VULNERABLE, 200);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            70001,
            InventoryItemRecord {
                item_id: 70001,
                type_id: mod_tid,
                owner_id: 9401,
                location_id: sid,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            70002,
            InventoryItemRecord {
                item_id: 70002,
                type_id: svc_tid,
                owner_id: 9401,
                location_id: sid,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });

    let mut sess = make_session(9401, 98401, 30000142, Some(40001), 1);
    sess.structure_id = Some(sid);
    let _ = handle_structure_fit_move(&ctx, &mut sess, 70001, sid, InventoryFlag::HiSlot0, Some(true))
        .expect("fit hi slot");
    let _ = handle_structure_fit_move(&ctx, &mut sess, 70002, sid, InventoryFlag::ServiceSlot0, Some(false))
        .expect("fit service slot");
    let online_res = handle_structure_module_online(&ctx, &mut sess, sid, 70002, true)
        .expect("online service module");
    assert!(matches!(online_res, PyRep::KeyVal(_)));

    // Damaged hull blocks refit
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.get_mut(&sid).unwrap().armor_hp = 100.0;
    });
    assert_eq!(
        handle_structure_fit_move(&ctx, &mut sess, 70001, sid, InventoryFlag::Hangar, None)
            .unwrap_err()
            .code,
        "StructureDamagedCannotRefit"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.get_mut(&sid).unwrap().armor_hp = 500_000.0;
    });

    // Jettison fuel blocks requires being docked in or actively controlling structure
    sess.structure_id = None;
    let jettison_args = vec![
        PyRep::Integer(sid),
        PyRep::Integer(i64::from(fuel_tid)),
        PyRep::Integer(25),
    ];
    assert_eq!(
        handle_jettison_structure_fuel(&ctx, &mut sess, &jettison_args)
            .await
            .unwrap_err()
            .code,
        "NotDockedInStructure"
    );
    sess.structure_id = Some(sid);
    let jet = handle_jettison_structure_fuel(&ctx, &mut sess, &jettison_args)
        .await
        .expect("jettison");
    assert!(matches!(jet, PyRep::KeyVal(_)));

    // Deliver item while docked & drop off cargo while in space
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            70003,
            InventoryItemRecord {
                item_id: 70003,
                type_id: fuel_tid,
                owner_id: 9401,
                location_id: sid,
                flag: InventoryFlag::Hangar,
                quantity: 10,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            70004,
            InventoryItemRecord {
                item_id: 70004,
                type_id: fuel_tid,
                owner_id: 9401,
                location_id: 40001,
                flag: InventoryFlag::Cargo,
                quantity: 5,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });
    sess.structure_id = Some(sid);
    let _ = handle_structure_deliver(
        &ctx,
        &mut sess,
        &[PyRep::Integer(sid), PyRep::Integer(70003), PyRep::Integer(9302)],
    )
    .expect("deliver item");
    assert_eq!(
        ctx.db.with_snapshot(|s| s.items[&70003].flag),
        InventoryFlag::Deliveries
    );

    sess.structure_id = None;
    sess.corp_role = 0;
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.get_mut(&98401).unwrap().ceo_id = 999999;
        snap.corporations.get_mut(&98401).unwrap().member_roles.clear();
    });
    spawn_ship_ball(&ctx, 30000142, 40001, DVec3::new(2_000_005_500.0, 0.0, 0.0)).await;
    let _ = handle_cargo_dropoff(
        &ctx,
        &mut sess,
        &[PyRep::Integer(sid), PyRep::List(vec![PyRep::Integer(70004)])],
    )
    .await
    .expect("cargo dropoff");
    assert_eq!(
        ctx.db.with_snapshot(|s| s.items[&70004].flag),
        InventoryFlag::Deliveries
    );
}
