#![forbid(unsafe_code)]

use super::*;
use eve_db::{
    AsteroidRecord, InventoryFlag, InventoryItemRecord, VirtualServerClock, WorldRepository,
};
use eve_dogma::{FittedModule, ModuleState, ShipFittingEngine};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::{InvType, SdeRegistry};
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn custom_inv_type(
    sde: &SdeRegistry,
    type_id: u32,
    group_id: u32,
    category_id: u32,
    name: &str,
    attrs: &[(u16, f64)],
) -> InvType {
    let mut t = sde.types.get(&587).expect("seed type").clone();
    t.type_id = type_id;
    t.group_id = group_id;
    t.category_id = category_id;
    t.name = name.into();
    t.default_attributes.clear();
    for &(k, v) in attrs {
        t.default_attributes.insert(k, v);
    }
    t
}

fn test_ctx_with_custom<F: FnOnce(&mut SdeRegistry)>(mutate_sde: F) -> ServerContext {
    let mut sde = (*shared_sde()).clone();
    mutate_sde(&mut sde);
    let db = Arc::new(WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(Arc::new(sde), db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_800_000_000)));
    ctx
}

#[test]
fn compute_nanite_repair_params_scales_with_nanite_operation_and_interfacing_skills() {
    let ctx = test_ctx_with_custom(|sde| {
        sde.types.insert(
            994_001,
            custom_inv_type(sde, 994_001, 46, 7, "Test MWD", &[(1267, 10.0), (1277, 1.0)]),
        );
    });
    let mod_t = ctx.sde.types.get(&994_001);

    let (p0, r0, _) = compute_nanite_repair_params(mod_t, 0.0, 40.0, 0, 0, 0);
    assert_eq!(p0, 0);
    assert!((r0 - 10.0).abs() < 1e-9);

    let (paste_unskilled, rate_unskilled, dur_unskilled) =
        compute_nanite_repair_params(mod_t, 20.0, 40.0, 0, 0, 0);
    assert_eq!(paste_unskilled, 20);
    assert!((rate_unskilled - 10.0).abs() < 1e-9);
    assert!((dur_unskilled - 2.0).abs() < 1e-9);

    // Level 5 Nanite Operation (-25% paste cost -> 15) and Level 5 Nanite Interfacing (+100% rate -> 20 HP/s, 1s duration)
    let (paste_v, rate_v, dur_v) = compute_nanite_repair_params(mod_t, 20.0, 40.0, 5, 5, 5);
    assert_eq!(paste_v, 15);
    assert!((rate_v - 20.0).abs() < 1e-9);
    assert!((dur_v - 1.0).abs() < 1e-9);
}

#[tokio::test]
async fn deactivate_stop_overload_overload_rack_nanite_repair_and_mining_scan() {
    let ctx = test_ctx_with_custom(|sde| {
        sde.types.insert(
            994_010,
            custom_inv_type(sde, 994_010, 49, 7, "Survey Scanner I", &[(40, 40.0), (197, 20_000.0), (669, 5_000.0)]),
        );
    });

    let char_id = 95_400_001;
    let sys_id = 30_002_505u32;
    let ship_id = 1_000_954_001;
    let mod_id = 1_000_954_002;
    let paste_id = 1_000_954_003;
    let ast_id = 1_000_954_010;

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().expect("seed char");
        ch.character_id = char_id;
        ch.user_id = char_id as u64;
        ch.solar_system_id = sys_id;
        ch.station_id = None;
        ch.structure_id = None;
        ch.active_ship_id = ship_id;
        snap.characters.insert(char_id, ch);
        snap.accounts.insert(
            format!("user_{char_id}"),
            eve_db::AccountRecord {
                user_id: char_id as u64,
                username: format!("user_{char_id}"),
                password_token: String::new(),
                role: 0,
                banned_until_unix: None,
                ban_reason: None,
            },
        );
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        let mut mod_rec = InventoryItemRecord {
            item_id: mod_id,
            type_id: 994_010,
            owner_id: char_id,
            location_id: ship_id,
            flag: InventoryFlag::MedSlot0,
            quantity: 1,
            singleton: true,
            ..Default::default()
        };
        mod_rec.dynamic_attributes.insert(attr_ids::DAMAGE, 10.0);
        mod_rec.dynamic_attributes.insert(attr_ids::HP_STRUCT, 40.0);
        snap.items.insert(mod_id, mod_rec);
        snap.items.insert(
            paste_id,
            InventoryItemRecord {
                item_id: paste_id,
                type_id: 28668,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 20,
                singleton: false,
                ..Default::default()
            },
        );
        snap.module_active_states.insert(mod_id);
        snap.asteroids.insert(
            ast_id,
            AsteroidRecord {
                asteroid_id: ast_id,
                solar_system_id: sys_id,
                belt_id: 40_000_001,
                type_id: 1230,
                quantity: 5_000,
                initial_quantity: 5_000,
                position: (1_000.0, 0.0, 0.0),
                radius: 200.0,
            },
        );
    });
    let _ = ctx
        .db
        .acquire_online_lock(char_id as u64, char_id as u64, Some(char_id), true, ctx.now_unix_secs());

    let mut fit = ShipFittingEngine::from_sde(ship_id, 587, &ctx.sde);
    fit.modules.insert(
        mod_id,
        FittedModule {
            item_id: mod_id,
            type_id: 994_010,
            flag_id: InventoryFlag::MedSlot0 as u16,
            state: ModuleState::Active,
            charge_type_id: None,
            charge_quantity: 0,
            damage_hp: 10.0,
            max_hp: 40.0,
            dynamic_attribute_overrides: HashMap::new(),
        },
    );
    fit.recalculate_stats(&ctx.sde);
    ctx.store_ship_fitting(ship_id, fit);

    let kw = HashMap::new();
    let mut sess = SessionState {
        user_id: Some(char_id as u64),
        character_id: Some(char_id),
        solar_system_id: Some(sys_id),
        ship_id: Some(ship_id),
        ..Default::default()
    };

    // 1. Deactivate active module -> clears module_active_states, records 5s reactivation delay
    let deact = handle_mining_and_deactivate(
        &ctx,
        &mut sess,
        None,
        "dogmaIM",
        "Deactivate",
        &[PyRep::Integer(mod_id)],
        &kw,
    )
    .await
    .unwrap()
    .expect("Deactivate");
    assert_eq!(deact.dict_get("moduleID").and_then(|v| v.as_i64()), Some(mod_id));
    assert!(ctx.is_module_in_reactivation_delay(ship_id, mod_id));

    // 2. OverloadRack (rack 1 = med slots) & StopOverloadRack & StopOverload
    let over_rack = handle_mining_and_deactivate(
        &ctx,
        &mut sess,
        None,
        "dogmaIM",
        "OverloadRack",
        &[PyRep::Integer(mod_id)],
        &kw,
    )
    .await
    .unwrap()
    .expect("OverloadRack");
    assert_eq!(over_rack.dict_get("rackIndex").and_then(|v| v.as_i64()), Some(1));
    assert_eq!(over_rack.dict_get("overloaded").and_then(|v| v.as_bool()), Some(true));

    let stop_over = handle_mining_and_deactivate(
        &ctx,
        &mut sess,
        None,
        "dogmaIM",
        "StopOverload",
        &[PyRep::Integer(mod_id)],
        &kw,
    )
    .await
    .unwrap()
    .expect("StopOverload");
    assert_eq!(stop_over.dict_get("overloaded").and_then(|v| v.as_bool()), Some(false));

    // 3. RepairModule (in space, deducts 1 initial paste) & StopRepairModule
    let rep_start = handle_mining_and_deactivate(
        &ctx,
        &mut sess,
        None,
        "dogmaIM",
        "RepairModule",
        &[PyRep::Integer(mod_id)],
        &kw,
    )
    .await
    .unwrap()
    .expect("RepairModule");
    assert_eq!(rep_start.dict_get("pasteConsumed").and_then(|v| v.as_i64()), Some(1));

    let rep_stop = handle_mining_and_deactivate(
        &ctx,
        &mut sess,
        None,
        "dogmaIM",
        "StopRepairModule",
        &[PyRep::Integer(mod_id)],
        &kw,
    )
    .await
    .unwrap()
    .expect("StopRepairModule");
    assert_eq!(rep_stop.dict_get("stopped").and_then(|v| v.as_bool()), Some(true));

    // 4. miningScanMgr.perform_scan reveals nearby asteroid
    let scan_res = handle_mining_and_deactivate(
        &ctx,
        &mut sess,
        None,
        "miningScanMgr",
        "perform_scan",
        &[PyRep::Integer(mod_id)],
        &kw,
    )
    .await
    .unwrap()
    .expect("perform_scan");
    assert_eq!(scan_res.dict_get("asteroidCount").and_then(|v| v.as_i64()), Some(1));
}
