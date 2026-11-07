//! Deep branch and mutation-killing unit tests for `structure_lifecycle_rpc`.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::station_structure::structure_lifecycle as life;
use crate::services::station_structure::structure_lifecycle_rpc::*;
use crate::services::station_structure::structure_upwell_ops::{load_ext_state, save_ext_state};
use eve_db::{
    CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents, UpwellStructureRecord,
};
use eve_destiny::{Ball, CombatState, MovementMode, SlimItemMeta};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::{InvType, SdeRegistry};
use glam::DVec3;
use std::collections::HashSet;
use std::sync::{Arc, OnceLock};

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
    new_tid: u32,
    group_id: u32,
    category_id: u32,
    volume: f64,
    capacity: f64,
    attrs: &[(u16, f64)],
) {
    let reg = Arc::make_mut(sde);
    let mut rec: InvType = reg.types.values().next().cloned().expect("base sde type");
    rec.type_id = new_tid;
    rec.group_id = group_id;
    rec.category_id = category_id;
    rec.name = format!("ExtraStructRpc_{new_tid}");
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

pub(super) fn seed_astrahus_type(ctx: &mut ServerContext, tid: u32) {
    let attrs = [
        (life::ATTR_RIG_SIZE, 2.0),
        (life::ATTR_SHIELD_CAPACITY, 500_000.0),
        (life::ATTR_ARMOR_HP, 500_000.0),
        (life::ATTR_HP, 500_000.0),
        (life::ATTR_HI_SLOTS, 3.0),
        (life::ATTR_MED_SLOTS, 3.0),
        (life::ATTR_LOW_SLOTS, 3.0),
        (life::ATTR_RIG_SLOTS, 3.0),
        (life::ATTR_SERVICE_SLOTS, 3.0),
    ];
    clone_type_with(&mut ctx.sde, tid, 1657, life::CATEGORY_STRUCTURE, 80_000.0, 0.0, &attrs);
}

pub(super) fn make_session(char_id: i64, corp_id: i64, sys_id: u32, ship_id: Option<i64>, role: u64) -> SessionState {
    let mut s = SessionState::new(715);
    s.user_id = Some(char_id as u64);
    s.character_id = Some(char_id);
    s.corporation_id = Some(corp_id);
    s.solar_system_id = Some(sys_id);
    s.ship_id = ship_id;
    s.corp_role = role;
    s
}

pub(super) fn seed_char_corp_ship(ctx: &ServerContext, char_id: i64, corp_id: i64, sys_id: u32, ship_id: i64) {
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.insert(
            corp_id,
            CorporationRecord {
                corporation_id: corp_id,
                name: format!("Corp {corp_id}"),
                ticker: "EXT".into(),
                ceo_id: char_id,
                hq_station_id: Some(60003760),
                tax_rate: 0.05,
                master_wallet: IskCents(10_000_000),
                members: HashSet::from([char_id]),
                ..Default::default()
            },
        );
        let mut ch = snap.characters.values().next().cloned().expect("demo char");
        ch.character_id = char_id;
        ch.name = format!("Pilot {char_id}");
        ch.corporation_id = corp_id;
        ch.alliance_id = None;
        ch.solar_system_id = sys_id;
        ch.station_id = None;
        ch.structure_id = None;
        ch.active_ship_id = ship_id;
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
                shield_hp: 50_000_000.0,
                armor_hp: 50_000_000.0,
                hull_hp: 50_000_000.0,
                ..Default::default()
            },
        );
    });
}

#[tokio::test(flavor = "current_thread")]
async fn anchor_guards_palatine_zarzakh_thera_npc_corp_profile_and_stack_quantity() {
    let mut ctx = test_ctx();
    let tid = 918_001_u32;
    let palatine_tid = 40340_u32;
    seed_astrahus_type(&mut ctx, tid);
    seed_astrahus_type(&mut ctx, palatine_tid);

    let sys_id = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| {
            s.security_status >= 0.5
                && !crate::services::structure_anchoring_rules::is_trade_hub_system(s.solar_system_id)
                && !crate::services::structure_anchoring_rules::is_starter_or_career_system(&ctx.sde, s.solar_system_id)
        })
        .map(|s| s.solar_system_id)
        .unwrap_or(30000144);
    let char_id = 918_101_i64;
    let corp_id = 98_918_101_i64;
    let ship_id = 918_201_i64;
    seed_char_corp_ship(&ctx, char_id, corp_id, sys_id, ship_id);

    // 1. NoActiveShip
    let mut no_ship_sess = make_session(char_id, corp_id, sys_id, None, 1);
    assert_eq!(
        handle_anchor(&ctx, &mut no_ship_sess, &[PyRep::Integer(i64::from(tid))])
            .await
            .unwrap_err()
            .code,
        "NoActiveShip"
    );

    // 2. ItemNotFound (negative item ID not in items) & InvalidStructureType
    let mut sess = make_session(char_id, corp_id, sys_id, Some(ship_id), 1);
    assert_eq!(
        handle_anchor(&ctx, &mut sess, &[])
            .await
            .unwrap_err()
            .code,
        "InvalidStructureType"
    );
    assert_eq!(
        handle_anchor(&ctx, &mut sess, &[PyRep::Integer(-999)])
            .await
            .unwrap_err()
            .code,
        "ItemNotFound"
    );
    assert_eq!(
        handle_anchor(&ctx, &mut sess, &[PyRep::Integer(587)])
            .await
            .unwrap_err()
            .code,
        "InvalidStructureType"
    );

    // 3. Thera (31000005) & Zarzakh (30100000) anchoring prohibitions
    {
        let sde = Arc::make_mut(&mut ctx.sde);
        let mut z = sde.solar_systems.get(&sys_id).unwrap().clone();
        z.solar_system_id = 30_100_000;
        sde.solar_systems.insert(30_100_000, z.clone());
        z.solar_system_id = 31_000_005;
        sde.solar_systems.insert(31_000_005, z);
    }
    sess.solar_system_id = Some(30_100_000);
    assert_eq!(
        handle_anchor(&ctx, &mut sess, &[PyRep::Integer(i64::from(tid))])
            .await
            .unwrap_err()
            .code,
        "AnchoringDisallowedInZarzakh"
    );
    sess.solar_system_id = Some(31_000_005);
    assert_eq!(
        handle_anchor(&ctx, &mut sess, &[PyRep::Integer(i64::from(tid))])
            .await
            .unwrap_err()
            .code,
        "AnchoringDisallowedInTheraOrShattered"
    );

    // 4. Palatine Keepstar limit (40340 already anchored) & ShipNotInSpace guard
    sess.solar_system_id = Some(sys_id);
    assert_eq!(
        handle_anchor(&ctx, &mut sess, &[PyRep::Integer(i64::from(tid))])
            .await
            .unwrap_err()
            .code,
        "ShipNotInSpace"
    );
    ctx.systems
        .get_or_spawn(sys_id)
        .spawn_ball_with_meta(
            Ball {
                id: ship_id,
                type_id: 670,
                position: DVec3::new(8.0e9 - 10_000.0, 1.0e9, 2.0e9),
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1.0e6,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState::new(),
            SlimItemMeta::default(),
        )
        .await
        .unwrap();
    seed_structure(&ctx, 10_000_999_001, palatine_tid, corp_id, sys_id, life::STATE_SHIELD_VULNERABLE, 100);
    assert_eq!(
        handle_anchor(&ctx, &mut sess, &[PyRep::Integer(i64::from(palatine_tid))])
            .await
            .unwrap_err()
            .code,
        "PalatineKeepstarLimitReached"
    );

    // 5. NPC Corp cannot anchor
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().corporation_id = 1_000_009;
    });
    sess.corporation_id = Some(1_000_009);
    assert_eq!(
        handle_anchor(&ctx, &mut sess, &[PyRep::Integer(i64::from(tid))])
            .await
            .unwrap_err()
            .code,
        "NpcCorpCannotAnchorStructure"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().corporation_id = corp_id;
    });
    sess.corporation_id = Some(corp_id);

    // 6. Foreign profile ownership violation & stack quantity > 1 decrement
    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        let mut prof = ext.profiles_by_id.values().next().cloned().unwrap_or_default();
        prof.profile_id = 777;
        prof.owner_corp_id = corp_id + 1;
        ext.profiles_by_id.insert(777, prof);
        save_ext_state(snap, &ext);
        snap.items.insert(
            55_901,
            InventoryItemRecord {
                item_id: 55_901,
                type_id: tid,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 2,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });
    let bad_prof_args = vec![
        PyRep::Integer(55_901),
        PyRep::None,
        PyRep::None,
        PyRep::Real(0.0),
        PyRep::Integer(777),
        PyRep::Integer(1),
        PyRep::Integer(18),
        PyRep::String("My Astrahus".into()),
    ];
    assert_eq!(
        handle_anchor(&ctx, &mut sess, &bad_prof_args)
            .await
            .unwrap_err()
            .code,
        "StructureOwnershipViolation"
    );

    let ok_args = vec![
        PyRep::Integer(55_901),
        PyRep::None,
        PyRep::None,
        PyRep::Real(0.0),
        PyRep::Integer(1),
        PyRep::Integer(1),
        PyRep::Integer(18),
        PyRep::String("My Astrahus".into()),
    ];
    assert!(handle_anchor(&ctx, &mut sess, &ok_args).await.is_ok());
    assert_eq!(ctx.db.with_snapshot(|s| s.items[&55_901].quantity), 1);
}
