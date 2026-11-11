#![allow(unused_imports)]
//! Extra unit and mutation-killing tests for `space_entity_mining.rs`.

#![forbid(unsafe_code)]

use crate::router::{BoundObjectEntry, ServerContext};
use crate::services::space_entity_mining::handle;
use crate::system_actor::AsteroidState;
use eve_db::{
    AsteroidRecord, CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents,
    UpwellStructureRecord,
};
use eve_destiny::{Ball, CombatState, MovementMode};
use eve_machonet::{service_roles, SessionState};
use eve_proto::PyRep;
use eve_sde::{InvType, SdeRegistry, TypeMaterialEntry};
use glam::DVec3;
use std::sync::{Arc, OnceLock};

#[cfg(test)]
#[path = "space_drone_mining_and_reprocessing_unit_tests.rs"]
mod space_drone_mining_and_reprocessing_unit_tests;

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

#[allow(clippy::too_many_arguments)]
pub(super) fn upsert_type(
    sde: &mut Arc<SdeRegistry>,
    tid: u32,
    group_id: u32,
    category_id: u32,
    name: &str,
    volume: f64,
    capacity: f64,
    portion_size: u64,
    attrs: &[(u16, f64)],
) {
    let reg = Arc::make_mut(sde);
    let mut rec: InvType = reg.types.values().next().cloned().expect("base type");
    rec.type_id = tid;
    rec.group_id = group_id;
    rec.category_id = category_id;
    rec.name = name.into();
    rec.volume = volume;
    rec.capacity = capacity;
    rec.portion_size = portion_size;
    rec.base_price = 1_000.0;
    rec.default_attributes.clear();
    if category_id == 6 {
        rec.default_attributes.insert(14, 8.0);
        rec.default_attributes.insert(13, 8.0);
        rec.default_attributes.insert(12, 8.0);
        rec.default_attributes.insert(1137, 3.0);
        rec.default_attributes.insert(11, 10_000.0);
        rec.default_attributes.insert(48, 10_000.0);
    } else if category_id == 7 {
        rec.effect_ids = if group_id == 778 { vec![2663] } else { vec![11, 12, 13, 16] };
    }
    for &(k, v) in attrs {
        rec.default_attributes.insert(k, v);
    }
    reg.types.insert(tid, rec);
}

pub(super) fn make_session(
    char_id: i64,
    corp_id: i64,
    sys_id: Option<u32>,
    ship_id: Option<i64>,
) -> SessionState {
    let mut s = SessionState::new(701);
    s.user_id = Some(11);
    s.character_id = Some(char_id);
    s.corporation_id = Some(corp_id);
    s.solar_system_id = sys_id;
    s.ship_id = ship_id;
    s.role = service_roles::ROLE_PLAYER_DEFAULT;
    s
}

pub(super) fn make_item(
    item_id: i64,
    type_id: u32,
    owner_id: i64,
    location_id: i64,
    flag: InventoryFlag,
    quantity: i32,
) -> InventoryItemRecord {
    InventoryItemRecord {
        item_id,
        type_id,
        owner_id,
        location_id,
        flag,
        quantity,
        singleton: false,
        ..Default::default()
    }
}

pub(super) fn make_ship_ball(ship_id: i64, type_id: u32, pos: DVec3) -> Ball {
    Ball {
        id: ship_id,
        type_id,
        position: pos,
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 1.0,
        mass: 1_000_000.0,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ice_harvesting_module_compatibility_and_bound_system_resolution() {
    let mut ctx = test_ctx();
    let (sys_id, char_id, corp_id) = (30_000_142, 90_000_001, 98_000_001);
    let (ship_id, ice_mod_id, ore_mod_id, ice_ast_id) =
        (1_090_000_001, 1_090_000_002, 1_090_000_003, 1_090_000_010);

    // Mackinaw-like ship with SpecializedIceHold (attr 3136)
    upsert_type(
        &mut ctx.sde,
        996_001,
        543,
        6,
        "Test Mackinaw",
        28_000.0,
        500.0,
        1,
        &[(3136, 45_000.0)],
    );
    upsert_type(
        &mut ctx.sde,
        996_002,
        464,
        7,
        "Ice Harvester II",
        5.0,
        0.0,
        1,
        &[(77, 1_000.0), (54, 12_000.0), (3154, 34.0), (780, 1.0)],
    );
    upsert_type(
        &mut ctx.sde,
        996_003,
        54,
        7,
        "Miner I",
        5.0,
        0.0,
        1,
        &[(77, 50.0), (54, 10_000.0)],
    );
    upsert_type(&mut ctx.sde, 996_004, 465, 25, "Clear Icicle", 1_000.0, 0.0, 1, &[]);

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ship_id,
            make_item(ship_id, 996_001, char_id, sys_id as i64, InventoryFlag::None, 1),
        );
        snap.items.insert(
            ice_mod_id,
            make_item(ice_mod_id, 996_002, char_id, ship_id, InventoryFlag::HiSlot0, 1),
        );
        snap.items.insert(
            ore_mod_id,
            make_item(ore_mod_id, 996_003, char_id, ship_id, InventoryFlag::HiSlot1, 1),
        );
        snap.items.insert(
            1_090_000_004,
            make_item(1_090_000_004, 996_002, char_id, ship_id, InventoryFlag::Cargo, 1),
        );
    });

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_handle
        .spawn_ball_with_combat(
            make_ship_ball(ship_id, 996_001, DVec3::ZERO),
            CombatState::default(),
        )
        .await;
    // Spawn ice asteroid directly in ballpark (without snap.asteroids entry, belt_id = 77)
    let _ = sys_handle
        .spawn_asteroid(
            AsteroidState {
                asteroid_id: ice_ast_id,
                belt_id: 77,
                type_id: 996_004,
                initial_units: 10,
                remaining_units: 10,
                unit_volume: 1_000.0,
                depleted: false,
                respawn_at_tick: None,
            },
            Ball {
                id: ice_ast_id,
                type_id: 996_004,
                position: DVec3::new(2_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 0.0,
                mass: 1.0e10,
                radius: 100.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
        )
        .await;

    // Session without solar_system_id, resolved via bound_entry for "beyonce"
    let mut sess = make_session(char_id, corp_id, None, Some(ship_id));
    let bound = BoundObjectEntry {
        bound_id: 1,
        session_id: 701,
        user_id: 11,
        character_id: char_id,
        service: "beyonce".into(),
        object_id: sys_id as i64,
        oid_str: "oid:1".into(),
        ref_id: 1,
    };

    // Specifying incompatible ore_mod_id on Ice asteroid -> InvalidMiningModule
    let incompat_err = handle(
        &ctx,
        &mut sess,
        "beyonce",
        "CmdMineRepeatedly",
        &[PyRep::Integer(ice_ast_id), PyRep::Integer(ore_mod_id)],
        Some(&bound),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(incompat_err.code, "InvalidMiningModule");

    // Specifying non-fitted module ID (in cargo) -> InvalidMiningModule
    let unfitted_err = handle(
        &ctx,
        &mut sess,
        "beyonce",
        "CmdMineRepeatedly",
        &[PyRep::Integer(ice_ast_id), PyRep::Integer(1_090_000_004)],
        Some(&bound),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(unfitted_err.code, "InvalidMiningModule");

    // Auto-selects compatible Ice Harvester II and deposits into SpecializedIceHold (181)
    let ok = handle(
        &ctx,
        &mut sess,
        "beyonce",
        "CmdMineRepeatedly",
        &[PyRep::Integer(ice_ast_id)],
        Some(&bound),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        ok.dict_get("flagID").and_then(|v| v.as_i64()),
        Some(InventoryFlag::SpecializedIceHold as i64)
    );
    assert_eq!(ok.dict_get("minedUnits").and_then(|v| v.as_i64()), Some(1));
    // Verify snap.asteroids entry was created because belt_id != 0
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.asteroids.get(&ice_ast_id).map(|a| a.belt_id), Some(77));
    });
}
