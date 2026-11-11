//! Unit tests for `structure_lifecycle_rpc` (`Anchor`, live combat precheck/damage, structure fitting, fuel jettison, deliveries, and cargo dropoff).

#![forbid(unsafe_code)]

use super::*;
use crate::router::ServerContext;
use eve_db::{
    CorpMemberRoleAssignment, CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents,
    UpwellStructureRecord,
};
use eve_destiny::{Ball, CombatState, MovementMode, SlimItemMeta};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::{InvType, SdeRegistry};
use glam::DVec3;
use std::collections::HashSet;
use std::sync::{Arc, OnceLock};

#[cfg(test)]
#[path = "structure_combat_and_fitting_rpc_tests.rs"]
mod structure_combat_and_fitting_rpc_tests;

#[cfg(test)]
#[path = "structure_lifecycle_cargo_fuel_edge_tests.rs"]
mod structure_lifecycle_cargo_fuel_edge_tests;

pub(super) static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

pub(super) fn test_ctx() -> ServerContext {
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

pub(super) fn clone_type_with(
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

pub(super) fn seed_astrahus_type(ctx: &mut ServerContext, tid: u32, disallow_highsec: bool) {
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

pub(super) fn make_session(char_id: i64, corp_id: i64, sys_id: u32, ship_id: Option<i64>, role: u64) -> SessionState {
    let mut s = SessionState::new(710);
    s.user_id = Some(char_id as u64);
    s.character_id = Some(char_id);
    s.corporation_id = Some(corp_id);
    s.solar_system_id = Some(sys_id);
    s.ship_id = ship_id;
    s.corp_role = role;
    s
}

pub(super) fn seed_char_corp_ship(
    ctx: &ServerContext,
    char_id: i64,
    corp_id: i64,
    sys_id: u32,
    ship_id: i64,
    is_ceo: bool,
) {
    ctx.db.with_mut_snapshot(|snap| {
        let mut corp = { let mut __corp = CorporationRecord::default();
            __corp.corporation_id = corp_id;
            __corp.name = format!("Corp {corp_id}");
            __corp.ticker = "TST".into();
            __corp.ceo_id = if is_ceo { char_id } else { char_id + 999 };
            __corp.hq_station_id = Some(60003760);
            __corp.tax_rate = 0.05;
            __corp.members = HashSet::from([char_id]);
            __corp.set_division_balance(1, IskCents(10_000_000));
            __corp };
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

pub(super) fn seed_structure(
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

pub(super) async fn spawn_ship_ball(ctx: &ServerContext, sys_id: u32, ship_id: i64, pos: DVec3) {
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn anchor_ansiblex_with_v24_extra_vars_auto_links_reciprocal_gates() {
    let ctx = test_ctx();
    let sov_systems: Vec<u32> = ctx
        .sde
        .solar_systems
        .keys()
        .copied()
        .filter(|&sid| {
            crate::services::station_structure::structure_ansiblex_navigation::is_sovereign_nullsec_for_ansiblex(&ctx.sde, sid)
        })
        .take(80)
        .collect();
    let (sys_a, sys_b) = sov_systems
        .iter()
        .find_map(|&a| {
            sov_systems.iter().find_map(|&b| {
                let d = crate::services::space_destiny::space_nav_travel::distance_ly(&ctx, a, b);
                (a != b && d > 0.0 && d <= 4.5).then_some((a, b))
            })
        })
        .expect("pair of sov null systems within 4.5 LY");

    let char_id = 920_101_i64;
    let corp_id = 98_000_202_i64;
    let ally_id = 99_000_202_i64;
    let ship_id = 920_102_i64;
    let item_a = 920_103_i64;
    let item_b = 920_104_i64;
    seed_char_corp_ship(&ctx, char_id, corp_id, sys_a, ship_id, true);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(corp) = snap.corporations.get_mut(&corp_id) {
            corp.alliance_id = Some(ally_id);
        }
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.alliance_id = Some(ally_id);
        }
        for iid in [item_a, item_b] {
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: crate::services::station_structure::structure_ansiblex_navigation::ANSIBLEX_TYPE_ID,
                    owner_id: char_id,
                    location_id: ship_id,
                    flag: InventoryFlag::Cargo,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
        }
        // Satisfy FLEX sov alliance + hub upgrade requirement in both systems.
        for (idx, sys) in [sys_a, sys_b].into_iter().enumerate() {
            let sys_state = snap.system_states.entry(sys).or_default();
            sys_state.solar_system_id = sys;
            sys_state.sov_alliance_id = Some(ally_id);
            let hub_id = 1_000_000_920_100 + idx as i64;
            snap.structures.insert(
                hub_id,
                UpwellStructureRecord {
                    structure_id: hub_id,
                    type_id: crate::services::sov_services::TYPE_SOVEREIGNTY_HUB,
                    owner_corp_id: corp_id,
                    solar_system_id: sys,
                    name: format!("Hub {sys}"),
                    position: [0.0, 0.0, 0.0],
                    state: life::STATE_SHIELD_VULNERABLE.into(),
                    quantum_core_installed: false,
                    fuel_blocks: 100,
                    shield_hp: 1.0,
                    armor_hp: 1.0,
                    hull_hp: 1.0,
                    reinforced_until_unix: 0,
                    last_damage_tick: 0,
                    last_damage_unix: 0,
                    self_destruct_at_unix: None,
                },
            );
            let hub_rt = snap.sov_runtime.sov_hubs.entry(hub_id).or_default();
            hub_rt.hub_id = hub_id;
            hub_rt.upgrades.push(eve_db::PersistedHubUpgrade {
                type_id: crate::services::sov_gameplay::UPGRADE_ADVANCED_LOGISTICS_NETWORK,
                online: true,
                low_power: false,
            });
        }
    });
    let mut sess = make_session(char_id, corp_id, sys_a, Some(ship_id), 1);
    sess.alliance_id = Some(ally_id);
    spawn_ship_ball(&ctx, sys_a, ship_id, DVec3::new(9_000_000_000.0, 0.0, 0.0)).await;
    let res_a = handle_anchor(
        &ctx,
        &mut sess,
        &[
            PyRep::Integer(item_a),
            PyRep::None,
            PyRep::None,
            PyRep::Real(0.0),
            PyRep::Integer(0),
            PyRep::String("Gate A".into()),
            PyRep::String("".into()),
            PyRep::Integer(1),
            PyRep::Integer(12),
            PyRep::Dict(vec![(
                PyRep::String("destinationSolarsystemID".into()),
                PyRep::Integer(i64::from(sys_b)),
            )]),
        ],
    )
    .await
    .expect("anchor jb_a");
    let jb_a = match res_a {
        PyRep::KeyVal(kv) => kv.get("structureID").and_then(|v| v.as_i64()).unwrap(),
        _ => panic!(),
    };

    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().solar_system_id = sys_b;
        snap.items.get_mut(&ship_id).unwrap().location_id = sys_b as i64;
    });
    sess.solar_system_id = Some(sys_b);
    spawn_ship_ball(&ctx, sys_b, ship_id, DVec3::new(9_000_000_000.0, 0.0, 0.0)).await;
    let res_b = handle_anchor(
        &ctx,
        &mut sess,
        &[
            PyRep::Integer(item_b),
            PyRep::None,
            PyRep::None,
            PyRep::Real(0.0),
            PyRep::Integer(0),
            PyRep::String("Gate B".into()),
            PyRep::String("".into()),
            PyRep::Integer(1),
            PyRep::Integer(12),
            PyRep::Dict(vec![(
                PyRep::String("destinationSolarsystemID".into()),
                PyRep::Integer(i64::from(sys_a)),
            )]),
        ],
    )
    .await
    .expect("anchor jb_b");
    let jb_b = match res_b {
        PyRep::KeyVal(kv) => kv.get("structureID").and_then(|v| v.as_i64()).unwrap(),
        _ => panic!(),
    };

    ctx.db.with_snapshot(|snap| {
        let ext = load_ext_state(snap);
        assert_eq!(ext.jump_bridge_desired_dest.get(&jb_a), Some(&sys_b));
        assert_eq!(ext.jump_bridge_desired_dest.get(&jb_b), Some(&sys_a));
        assert_eq!(ext.jump_bridge_links.get(&jb_a), Some(&jb_b));
        assert_eq!(ext.jump_bridge_links.get(&jb_b), Some(&jb_a));
    });
}
