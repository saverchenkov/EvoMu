#![forbid(unsafe_code)]

use super::tactical_modules::*;
use crate::router::ServerContext;
use eve_db::{
    InventoryFlag, InventoryItemRecord, VirtualServerClock, WorldRepository,
};
use eve_destiny::{Ball, CombatState, MovementMode, SlimItemMeta};
use eve_dogma::{DogmaItemState, ShipFittingEngine};
use eve_proto::PyRep;
use eve_sde::{InvType, SdeRegistry};
use glam::DVec3;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn custom_inv_type(sde: &SdeRegistry, type_id: u32, group_id: u32, category_id: u32, name: &str, attrs: &[(u16, f64)]) -> InvType {
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

fn test_ctx_with_custom_types<F: FnOnce(&mut SdeRegistry)>(mutate_sde: F) -> ServerContext {
    let mut sde = (*shared_sde()).clone();
    mutate_sde(&mut sde);
    let db = Arc::new(WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(Arc::new(sde), db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_800_000_000)));
    ctx
}

fn ensure_char(ctx: &ServerContext, char_id: i64, corp_id: i64, sys_id: u32, ship_id: i64, clone_grade: u32) {
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().expect("demo seed character");
        ch.character_id = char_id;
        ch.user_id = char_id as u64;
        ch.name = format!("Pilot {char_id}");
        ch.corporation_id = corp_id;
        ch.station_id = None;
        ch.structure_id = None;
        ch.solar_system_id = sys_id;
        ch.active_ship_id = ship_id;
        ch.security_status = 5.0;
        ch.clone_grade = clone_grade;
        ch.fleet_id = None;
        snap.characters.entry(char_id).or_insert(ch);
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
    });
    let _ = ctx.db.acquire_online_lock(char_id as u64, char_id as u64, Some(char_id), true, ctx.now_unix_secs());
}

fn insert_item(ctx: &ServerContext, item_id: i64, type_id: u32, owner_id: i64, location_id: i64, flag: InventoryFlag, quantity: i32, singleton: bool) {
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            item_id,
            InventoryItemRecord { item_id, type_id, owner_id, location_id, flag, quantity, singleton, ..Default::default() },
        );
    });
}

async fn spawn_ship_ball_with_cs(
    ctx: &ServerContext,
    sys_id: u32,
    ball_id: i64,
    type_id: u32,
    owner_id: i64,
    corp_id: i64,
    pos: DVec3,
    cap: f64,
) {
    let sys = ctx.systems.get_or_spawn(sys_id);
    let ball = Ball {
        id: ball_id,
        type_id,
        position: pos,
        velocity: DVec3::ZERO,
        max_velocity: 2_000.0,
        inertia: 1.0,
        mass: 1_000_000.0,
        radius: 200.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let mut cs = CombatState::default();
    cs.capacitor = cap;
    cs.max_capacitor = cap.max(50_000.0);
    cs.shield_hp = 5_000.0;
    cs.max_shield_hp = 5_000.0;
    cs.armor_hp = 5_000.0;
    cs.max_armor_hp = 5_000.0;
    cs.structure_hp = 5_000.0;
    cs.max_structure_hp = 5_000.0;
    cs.signature_radius = 250.0;
    let meta = SlimItemMeta {
        group_id: 25,
        category_id: 6,
        owner_id,
        char_id: Some(owner_id),
        corp_id,
        alliance_id: 0,
        security_status: 5.0,
        is_cloaked: false,
        ..Default::default()
    };
    sys.spawn_ball_with_meta(ball, cs, meta).await.expect("spawn ball");
}

#[test]
fn test_navigation_lock_entosis_unlock_and_remote_repair_boundaries() {
    let ctx = test_ctx_with_custom_types(|_| {});
    let db_key = db_ptr_key(&ctx.db);
    let now = ctx.now_unix_secs();
    let ship_id = 910_001;

    assert!(!is_clone_vat_bay_active_required(&ctx, ship_id));
    assert!(!has_lance_caster_lock(&ctx, ship_id));
    assert!((incoming_remote_repair_multiplier(&ctx, ship_id) - 1.0).abs() < 1e-6);
    assert!(check_ship_navigation_lock(&ctx, ship_id, "warp").is_ok());

    {
        let mut st = ctx.tactical_state.write().unwrap();
        st.clone_vat_bay_activated_ships.insert((db_key, ship_id));
        st.lance_caster_lock_until.insert((db_key, ship_id), now + 120);
    }
    assert!(is_clone_vat_bay_active_required(&ctx, ship_id));
    assert!(has_lance_caster_lock(&ctx, ship_id));
    assert_eq!(check_ship_navigation_lock(&ctx, ship_id, "warp").unwrap_err().code, "LanceFiringLockActive");

    // Exact boundary now == until should NOT lock
    {
        let mut st = ctx.tactical_state.write().unwrap();
        st.lance_caster_lock_until.insert((db_key, ship_id), now);
        st.lance_victim_debuff_until.insert((db_key, ship_id), now + 45);
    }
    assert!(!has_lance_caster_lock(&ctx, ship_id));
    assert!((incoming_remote_repair_multiplier(&ctx, ship_id) - 0.50).abs() < 1e-6);
    assert_eq!(check_ship_navigation_lock(&ctx, ship_id, "jump").unwrap_err().code, "DisruptiveLanceDebuffActive");

    // Expired victim debuff (now == until) + active Entosis link
    {
        let mut st = ctx.tactical_state.write().unwrap();
        st.lance_victim_debuff_until.insert((db_key, ship_id), now);
        st.entosis_active_links.insert(
            (db_key, ship_id),
            ActiveEntosisLinkState {
                ship_id,
                char_id: 9001,
                side_id: 1000,
                module_id: 920_001,
                target_id: 930_001,
                cycle_index: 1,
                cycle_duration_s: 300.0,
                started_unix: now,
                expires_unix: now + 300,
                is_capital: false,
            },
        );
        st.entosis_target_cycles.insert((db_key, 930_001, ship_id), 1);
        st.entosis_target_cycles.insert((db_key, 930_002, ship_id), 2);
    }
    assert!((incoming_remote_repair_multiplier(&ctx, ship_id) - 1.0).abs() < 1e-6);
    assert_eq!(check_ship_navigation_lock(&ctx, ship_id, "dock").unwrap_err().code, "EntosisLinkActive");

    clear_entosis_warmup_on_unlock(&ctx, ship_id, Some(930_002));
    assert!(check_ship_navigation_lock(&ctx, ship_id, "dock").is_err());

    clear_entosis_warmup_on_unlock(&ctx, ship_id, None);
    assert!(check_ship_navigation_lock(&ctx, ship_id, "dock").is_ok());
}

#[test]
fn test_projected_remote_assistance_falloff_crimewatch_and_removal() {
    let ctx = test_ctx_with_custom_types(|sde| {
        sde.types.insert(
            990_209,
            custom_inv_type(sde, 990_209, 209, 7, "Remote Tracking Computer II", &[(54, 30_000.0), (2044, 15_000.0), (351, 15.0), (349, 30.0), (767, 30.0)]),
        );
        sde.types.insert(
            990_290,
            custom_inv_type(sde, 990_290, 290, 7, "Remote Sensor Booster II", &[(54, 30_000.0), (2044, 15_000.0), (309, 25.0), (566, 30.0), (1027, 20.0)]),
        );
    });
    let logi_char = 911_001;
    let tgt_char = 911_002;
    let logi_ship = 912_001;
    let tgt_ship = 912_002;
    ensure_char(&ctx, logi_char, 1000125, 30000142, logi_ship, 1);
    ensure_char(&ctx, tgt_char, 1000125, 30000142, tgt_ship, 1);
    insert_item(&ctx, logi_ship, 587, logi_char, 30000142, InventoryFlag::Hangar, 1, true);
    insert_item(&ctx, tgt_ship, 587, tgt_char, 30000142, InventoryFlag::Hangar, 1, true);

    let now = ctx.now_unix_secs();
    {
        let mut cw = ctx.crimewatch.write().unwrap();
        let ts = cw.get_or_create(tgt_char);
        ts.weapon_timer_until_unix = now + 60;
        ts.pvp_timer_until_unix = now + 900;
        ts.criminal_flag_until_unix = now + 900;
    }

    let rtc_type = ctx.sde.types.get(&990_209).unwrap().clone();
    let rsb_type = ctx.sde.types.get(&990_290).unwrap().clone();
    let rtc_state = DogmaItemState::from_sde(913_001, 990_209, &ctx.sde);
    let rsb_state = DogmaItemState::from_sde(913_002, 990_290, &ctx.sde);

    assert_eq!(
        apply_projected_remote_assistance(&ctx, logi_char, logi_ship, 913_001, &rtc_type, &rtc_state, None, 10_000.0).unwrap_err().code,
        "TargetRequired"
    );
    assert_eq!(
        apply_projected_remote_assistance(&ctx, logi_char, logi_ship, 913_001, &rtc_type, &rtc_state, Some(tgt_ship), 61_000.0).unwrap_err().code,
        "OutOfRange"
    );

    apply_projected_remote_assistance(&ctx, logi_char, logi_ship, 913_001, &rtc_type, &rtc_state, Some(tgt_ship), 45_000.0).expect("rtc in falloff");
    apply_projected_remote_assistance(&ctx, logi_char, logi_ship, 913_002, &rsb_type, &rsb_state, Some(tgt_ship), 45_000.0).expect("rsb in falloff");

    {
        let mut cw = ctx.crimewatch.write().unwrap();
        let cs = cw.get_or_create(logi_char);
        assert_eq!(cs.weapon_timer_until_unix, now + 60);
        assert_eq!(cs.pvp_timer_until_unix, now + 900);
        assert_eq!(cs.criminal_flag_until_unix, now + 900);
    }

    let mut mod_attrs = HashMap::from([(54u16, 10_000.0), (158u16, 5_000.0), (160u16, 1.0)]);
    apply_projected_rtc_to_module_attrs(&ctx, tgt_ship, &mut mod_attrs);
    assert!((mod_attrs[&54] - 10_750.0).abs() < 1e-3);
    assert!((mod_attrs[&158] - 5_750.0).abs() < 1e-3);
    assert!((mod_attrs[&160] - 1.15).abs() < 1e-4);

    remove_projected_remote_assistance(&ctx, logi_ship, 913_001);
    let mut mod_attrs2 = HashMap::from([(54u16, 10_000.0)]);
    apply_projected_rtc_to_module_attrs(&ctx, tgt_ship, &mut mod_attrs2);
    assert!((mod_attrs2[&54] - 10_000.0).abs() < 1e-3);
}

#[test]
fn test_apply_tactical_module_ship_attributes_rig_entosis_cehe_tsb_siege_triage_wubble() {
    let ctx = test_ctx_with_custom_types(|sde| {
        sde.types.insert(990_782, custom_inv_type(sde, 990_782, 782, 7, "Cargohold Optimization I", &[]));
        sde.types.insert(991_313, custom_inv_type(sde, 991_313, 1313, 7, "Entosis Link I", &[]));
        sde.types.insert(
            40_714,
            custom_inv_type(sde, 40_714, 60, 7, "Capital Emergency Hull Energizer I", &[(974, 0.001), (975, 0.001), (976, 0.001), (977, 0.001)]),
        );
        sde.types.insert(4_292, custom_inv_type(sde, 4_292, 515, 7, "Siege Module II", &[]));
        sde.types.insert(4_294, custom_inv_type(sde, 4_294, 515, 7, "Triage Module II", &[]));
    });
    let char_id = 914_001;
    let ship_id = 914_100;
    ensure_char(&ctx, char_id, 1000125, 30000142, ship_id, 1);
    insert_item(&ctx, ship_id, 587, char_id, 30000142, InventoryFlag::Hangar, 1, true);
    insert_item(&ctx, 914_201, 990_782, char_id, ship_id, InventoryFlag::RigSlot0, 1, true);
    insert_item(&ctx, 914_202, 991_313, char_id, ship_id, InventoryFlag::HiSlot0, 1, true);
    insert_item(&ctx, 914_203, 40_714, char_id, ship_id, InventoryFlag::LoSlot0, 1, true);
    insert_item(&ctx, 914_204, 4_294, char_id, ship_id, InventoryFlag::HiSlot1, 1, true);

    ctx.db.with_mut_snapshot(|snap| {
        snap.module_active_states.insert(914_203);
        snap.module_active_states.insert(914_204);
        snap.transient_item_attributes.entry(914_203).or_default().insert(999, 42.5);
    });

    let db_key = db_ptr_key(&ctx.db);
    let now = ctx.now_unix_secs();
    {
        let mut st = ctx.tactical_state.write().unwrap();
        st.tsb_self_penalty_until.insert((db_key, ship_id), now + 30);
        st.active_wubble_debuffs.insert(
            (db_key, ship_id),
            ActiveWubbleDebuff {
                velocity_multiplier: 0.5,
                remaining_s: 10.0,
            },
        );
    }

    let mut attrs = HashMap::from([(37u16, 10_000.0), (564u16, 200.0), (208u16, 20.0), (192u16, 5.0)]);
    apply_tactical_module_ship_attributes(&ctx, ship_id, &mut attrs);
    assert!((attrs[&37] - 2_000.0).abs() < 1e-3);
    assert!((attrs[&564] - 150.0).abs() < 1e-3);
    assert!((attrs[&208] - 15.0).abs() < 1e-3);
    assert!((attrs[&113] - 0.001).abs() < 1e-6);
    assert!((attrs[&109] - 0.001).abs() < 1e-6);
    assert!((attrs[&2135] - 0.20).abs() < 1e-6);
    assert!((attrs[&2112] - 0.30).abs() < 1e-6);

    let mut merged = HashMap::new();
    apply_transient_item_attrs(&ctx, 914_203, &mut merged);
    assert_eq!(merged.get(&999).copied(), Some(42.5));
}

#[tokio::test]
async fn test_tsb_capacitor_and_superweapon_doomsday_and_slash_branches() {
    let ctx = test_ctx_with_custom_types(|sde| {
        sde.types.insert(11_567, custom_inv_type(sde, 11_567, 30, 6, "Avatar", &[(4, 2.4e9)]));
        sde.types.insert(
            24_550,
            custom_inv_type(
                sde,
                24_550,
                588,
                7,
                "Judgement",
                &[(54, 250_000.0), (114, 1_000_000.0), (713, 16_274.0), (714, 100.0), (2262, 15_000.0), (2263, 2_500.0), (2264, 15_000.0), (2265, 1_000.0), (2427, 300_000.0)],
            ),
        );
        sde.types.insert(
            4_409,
            custom_inv_type(sde, 4_409, 1154, 7, "Target Spectrum Breaker", &[(6, 500.0), (3114, 0.25), (3115, 12_000.0), (669, 150_000.0)]),
        );
    });
    let null_sys = 30_004_759;
    let char_id = 915_001;
    let titan_id = 915_100;
    let mod_id = 915_200;
    let tgt_id = 915_300;
    ensure_char(&ctx, char_id, 1000125, null_sys, titan_id, 1);
    insert_item(&ctx, titan_id, 11_567, char_id, null_sys as i64, InventoryFlag::Hangar, 1, true);
    insert_item(&ctx, mod_id, 24_550, char_id, titan_id, InventoryFlag::HiSlot0, 1, true);
    insert_item(&ctx, tgt_id, 19_720, 915_002, null_sys as i64, InventoryFlag::Hangar, 1, true);

    spawn_ship_ball_with_cs(&ctx, null_sys, titan_id, 11_567, char_id, 1000125, DVec3::ZERO, 100.0).await;
    spawn_ship_ball_with_cs(&ctx, null_sys, tgt_id, 19_720, 915_002, 1000125, DVec3::new(10_000.0, 0.0, 0.0), 1000.0).await;
    let sys = ctx.systems.get_or_spawn(null_sys);
    let _ = sys
        .add_target_lock(
            titan_id,
            eve_destiny::TargetLock {
                target_id: tgt_id,
                lock_time_ms: 1_000.0,
                remaining_lock_ms: 0.0,
                locked: true,
                max_range_m: Some(250_000.0),
            },
        )
        .await;

    let tsb_type = ctx.sde.types.get(&4_409).unwrap().clone();
    let tsb_state = DogmaItemState::from_sde(915_299, 4_409, &ctx.sde);
    assert_eq!(
        activate_target_spectrum_breaker(&ctx, &sys, char_id, titan_id, 915_299, &tsb_type, &tsb_state).await.unwrap_err().code,
        "NotEnoughCapacitor"
    );

    for (tgt, expected) in [(None, "TargetRequired"), (Some(titan_id), "InvalidTarget"), (Some(999_999), "TargetNotInSpace"), (Some(tgt_id), "InsufficientFuel")] {
        assert_eq!(
            activate_superweapon_or_lance(&ctx, char_id, titan_id, null_sys, mod_id, "Activate", tgt).await.unwrap_err().code,
            expected
        );
    }

    insert_item(&ctx, 915_400, 16_274, char_id, titan_id, InventoryFlag::Cargo, 500, false);
    let rep = activate_superweapon_or_lance(&ctx, char_id, titan_id, null_sys, mod_id, "Activate", Some(tgt_id))
        .await
        .expect("single target doomsday");
    assert_eq!(rep.dict_get("slashMode").and_then(|v| v.as_bool()), Some(false));
    assert_eq!(rep.dict_get("fuelConsumed").and_then(|v| v.as_i64()), Some(100));

    assert_eq!(
        activate_superweapon_or_lance(&ctx, char_id, titan_id, null_sys, mod_id, "Activate", Some(tgt_id)).await.unwrap_err().code,
        "ModuleInReactivationDelay"
    );

    let mod2_id = 915_201;
    insert_item(&ctx, mod2_id, 24_550, char_id, titan_id, InventoryFlag::HiSlot1, 1, true);
    let bad_args = vec![PyRep::Integer(mod2_id), PyRep::Tuple(vec![PyRep::Real(f64::NAN), PyRep::Real(0.0), PyRep::Real(0.0)])];
    assert_eq!(
        activate_superweapon_with_args(&ctx, char_id, titan_id, null_sys, mod2_id, "ActivateSlashModule", None, &bad_args).await.unwrap_err().code,
        "InvalidDirectionVector"
    );

    let good_args = vec![
        PyRep::Integer(mod2_id),
        PyRep::Tuple(vec![PyRep::Real(0.0), PyRep::Real(0.0), PyRep::Real(0.0)]),
        PyRep::Tuple(vec![PyRep::Real(1.0), PyRep::Real(0.0), PyRep::Real(0.0)]),
    ];
    let slash_rep = activate_superweapon_with_args(&ctx, char_id, titan_id, null_sys, mod2_id, "ActivateSlashModule", None, &good_args)
        .await
        .expect("slash module activation");
    assert_eq!(slash_rep.dict_get("slashMode").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(slash_rep.dict_get("tickCount").and_then(|v| v.as_i64()), Some(15));
}

#[tokio::test]
async fn test_entosis_link_interceptor_vulnerability_capital_and_command_burst_errors() {
    let ctx = test_ctx_with_custom_types(|sde| {
        sde.types.insert(11_176, custom_inv_type(sde, 11_176, 831, 6, "Crow", &[]));
        sde.types.insert(19_720, custom_inv_type(sde, 19_720, 485, 6, "Revelation", &[]));
        sde.types.insert(34_593, custom_inv_type(sde, 34_593, 1313, 7, "Entosis Link I", &[(73, 120_000.0)]));
        sde.types.insert(43_551, custom_inv_type(sde, 43_551, 1770, 7, "Mining Foreman Burst II", &[(422, 2.0), (604, 1769.0)]));
        sde.types.insert(42_832, custom_inv_type(sde, 42_832, 1769, 8, "Mining Laser Optimization Charge", &[(2468, 60.0), (2596, -15.0)]));
        sde.types.insert(28_606, custom_inv_type(sde, 28_606, 941, 6, "Orca", &[]));
    });
    let sys_id = 30000142;
    let char_id = 916_001;
    let inty_id = 916_100;
    let dread_id = 916_101;
    let ent_id = 916_200;
    let node_id = 916_300;
    ensure_char(&ctx, char_id, 1000125, sys_id, inty_id, 1);
    insert_item(&ctx, inty_id, 11_176, char_id, sys_id as i64, InventoryFlag::Hangar, 1, true);
    insert_item(&ctx, dread_id, 19_720, char_id, sys_id as i64, InventoryFlag::Hangar, 1, true);
    insert_item(&ctx, ent_id, 34_593, char_id, inty_id, InventoryFlag::HiSlot0, 1, true);
    insert_item(&ctx, node_id, 587, 1000125, sys_id as i64, InventoryFlag::Hangar, 1, true);

    let sys = ctx.systems.get_or_spawn(sys_id);
    let ent_type = ctx.sde.types.get(&34_593).unwrap().clone();

    assert_eq!(handle_entosis_link_activation(&ctx, &sys, char_id, inty_id, ent_id, &ent_type, None).await.unwrap_err().code, "TargetRequired");
    assert_eq!(
        handle_entosis_link_activation(&ctx, &sys, char_id, inty_id, ent_id, &ent_type, Some(node_id)).await.unwrap_err().code,
        "InterceptorCannotActivateEntosis"
    );

    insert_item(&ctx, ent_id, 34_593, char_id, dread_id, InventoryFlag::HiSlot0, 1, true);
    assert_eq!(
        handle_entosis_link_activation(&ctx, &sys, char_id, dread_id, ent_id, &ent_type, Some(node_id)).await.unwrap_err().code,
        "InsufficientFuel"
    );

    insert_item(&ctx, 916_400, 16_275, char_id, dread_id, InventoryFlag::Cargo, 5, false);
    let rep = handle_entosis_link_activation(&ctx, &sys, char_id, dread_id, ent_id, &ent_type, Some(node_id))
        .await
        .expect("capital entosis activation");
    assert_eq!(rep.dict_get("disallowAssistance").and_then(|v| v.as_bool()), Some(true));
    assert!((rep.dict_get("cycleDurationSeconds").and_then(|v| v.as_f64()).unwrap_or(0.0) - 600.0).abs() < 1e-3);

    assert_eq!(
        handle_entosis_link_activation(&ctx, &sys, char_id, dread_id, ent_id, &ent_type, Some(node_id)).await.unwrap_err().code,
        "ModuleAlreadyActive"
    );
    assert_eq!(
        handle_entosis_link_activation(&ctx, &sys, char_id, dread_id, ent_id + 1, &ent_type, Some(node_id)).await.unwrap_err().code,
        "MaxGroupActiveExceeded"
    );

    let orca_id = 916_102;
    let burst_id = 916_250;
    insert_item(&ctx, orca_id, 28_606, char_id, sys_id as i64, InventoryFlag::Hangar, 1, true);
    insert_item(&ctx, burst_id, 43_551, char_id, orca_id, InventoryFlag::HiSlot0, 1, true);
    let burst_type = ctx.sde.types.get(&43_551).unwrap().clone();
    let burst_state = DogmaItemState::from_sde(burst_id, 43_551, &ctx.sde);
    let mut fit = ShipFittingEngine::from_sde(orca_id, 28_606, &ctx.sde);

    assert_eq!(
        activate_command_burst(&ctx, &sys, char_id, orca_id, sys_id, burst_id, &burst_type, &burst_state, None, &fit).await.unwrap_err().code,
        "NoChargesLoaded"
    );

    insert_item(&ctx, 916_500, 16_275, char_id, orca_id, InventoryFlag::HiSlot0, 10, false);
    assert_eq!(
        activate_command_burst(&ctx, &sys, char_id, orca_id, sys_id, burst_id, &burst_type, &burst_state, None, &fit).await.unwrap_err().code,
        "InvalidChargeGroup"
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.remove(&916_500);
    });
    insert_item(&ctx, 916_501, 42_832, char_id, orca_id, InventoryFlag::HiSlot0, 10, false);
    assert_eq!(
        activate_command_burst(&ctx, &sys, char_id, orca_id, sys_id, burst_id, &burst_type, &burst_state, None, &fit).await.unwrap_err().code,
        "SkillMissing"
    );

    fit.character_skills.insert(22_552, 5);
    fit.character_skills.insert(22_536, 5);
    fit.character_skills.insert(29_637, 5);
    let burst_rep = activate_command_burst(&ctx, &sys, char_id, orca_id, sys_id, burst_id, &burst_type, &burst_state, None, &fit)
        .await
        .expect("mining burst on orca");
    assert_eq!(burst_rep.dict_get("active").and_then(|v| v.as_bool()), Some(true));
    assert!(burst_rep.dict_get("aoeRangeMeters").and_then(|v| v.as_f64()).unwrap_or(0.0) > 30_000.0);
}

#[tokio::test]
async fn test_tactical_probes_formulas_ball_filters_and_wubble_lifecycle() {
    // Siege/Triage Strontium & Nullifier formulas
    assert_eq!(compute_siege_strontium_cost(0), 250);
    assert_eq!(compute_siege_strontium_cost(2), 200);
    assert_eq!(compute_siege_strontium_cost(5), 125);
    assert_eq!(compute_siege_strontium_cost(9), 125);

    assert_eq!(compute_triage_strontium_cost(0), 200);
    assert_eq!(compute_triage_strontium_cost(3), 125);
    assert_eq!(compute_triage_strontium_cost(5), 75);
    assert_eq!(compute_triage_strontium_cost(9), 75);

    let (d0, c0) = compute_nullifier_duration_and_cooldown(10.0, 150.0, 0);
    assert!((d0 - 10.0).abs() < 1e-9 && (c0 - 150.0).abs() < 1e-9);
    let (d5, c5) = compute_nullifier_duration_and_cooldown(10.0, 150.0, 5);
    assert!((d5 - 15.0).abs() < 1e-9 && (c5 - 75.0).abs() < 1e-9);
    let (d8, c8) = compute_nullifier_duration_and_cooldown(10.0, 150.0, 8);
    assert!((d8 - 15.0).abs() < 1e-9 && (c8 - 75.0).abs() < 1e-9);

    let ctx = test_ctx_with_custom_types(|sde| {
        sde.types.insert(
            56_748,
            custom_inv_type(
                sde,
                56_748,
                649,
                8,
                "Stasis Webification Probe",
                &[(20, -30.0), (2262, 2_000.0), (2279, 20_000.0), (2280, 10_000.0)],
            ),
        );
    });
    assert!(!is_combatant_ship_or_entity_ball(Some(&ctx.sde), Some(&ctx.db), 40_000_001, 587));
    assert!(!is_player_ship_ball(Some(&ctx.sde), Some(&ctx.db), 40_000_001, 587));
    assert!(is_combatant_ship_or_entity_ball(Some(&ctx.sde), Some(&ctx.db), 991_001, 587));
    assert!(is_player_ship_ball(Some(&ctx.sde), Some(&ctx.db), 991_001, 587));
    assert!(is_combatant_ship_or_entity_ball(Some(&ctx.sde), Some(&ctx.db), 991_002, 35832));
    assert!(!is_player_ship_ball(Some(&ctx.sde), Some(&ctx.db), 991_002, 35832));
    assert!(!is_combatant_ship_or_entity_ball(Some(&ctx.sde), Some(&ctx.db), 991_003, 23));
    assert!(is_combatant_ship_or_entity_ball(Some(&ctx.sde), Some(&ctx.db), 991_004, 999_777));
    assert!(!is_player_ship_ball(Some(&ctx.sde), Some(&ctx.db), 991_004, 999_777));

    insert_item(&ctx, 991_005, 587, 90_000_001, 30_000_142, InventoryFlag::None, 1, true);
    assert!(is_combatant_ship_or_entity_ball(Some(&ctx.sde), Some(&ctx.db), 991_005, 0));
    assert!(is_player_ship_ball(Some(&ctx.sde), Some(&ctx.db), 991_005, 0));

    // Wubble probe lifecycle: wrong sys kept, dead probe cleaned, arming -> detonate -> debuff -> expiry
    let mut ballpark = eve_destiny::Ballpark::new(30_000_142);
    let mk_ball = |id: i64, tid: u32, pos: DVec3| Ball {
        id,
        type_id: tid,
        position: pos,
        velocity: DVec3::ZERO,
        max_velocity: 400.0,
        inertia: 1.0,
        mass: 1_000_000.0,
        radius: 50.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    ballpark.add_ball(mk_ball(991_010, 587, DVec3::new(5_000.0, 0.0, 0.0)));
    ballpark.add_ball(mk_ball(991_011, 587, DVec3::new(50_000.0, 0.0, 0.0)));
    ballpark.add_ball(mk_ball(991_012, 23, DVec3::new(100.0, 0.0, 0.0)));
    ballpark.add_ball(mk_ball(991_020, 56_748, DVec3::ZERO));
    ballpark.add_ball(mk_ball(991_021, 56_748, DVec3::ZERO));
    ballpark.combat_states.insert(991_021, CombatState { structure_hp: 0.0, ..Default::default() });
    ballpark.add_warp_bubble(991_020, DVec3::ZERO, 20_000.0);
    ballpark.add_warp_bubble(991_021, DVec3::ZERO, 20_000.0);

    register_launched_wubble_probe(&ctx, 991_020, 56_748, 30_000_142, DVec3::ZERO, 15_000.0);
    register_launched_wubble_probe(&ctx, 991_021, 56_748, 30_000_142, DVec3::ZERO, 15_000.0);
    register_launched_wubble_probe(&ctx, 991_022, 999_777, 30_000_144, DVec3::ZERO, 15_000.0);

    let sde_some = Some(ctx.sde.clone());
    let db_some = Some(ctx.db.clone());
    // db_opt == None no-op
    step_wubble_probes_and_debuffs(&mut ballpark, &sde_some, &None, &ctx.tactical_state, 1.0);
    assert!(ballpark.balls.contains_key(&991_020));

    // Step 1.0s: 991_021 dead cleaned, 991_020 still arming (1.0s left), 991_022 wrong system kept
    step_wubble_probes_and_debuffs(&mut ballpark, &sde_some, &db_some, &ctx.tactical_state, 1.0);
    assert!(!ballpark.balls.contains_key(&991_021));
    assert!(ballpark.balls.contains_key(&991_020));

    // Step 1.1s: 991_020 detonates -> 991_010 debuffed (0.70), out-of-range 991_011 & cargo 991_012 untouched
    step_wubble_probes_and_debuffs(&mut ballpark, &sde_some, &db_some, &ctx.tactical_state, 1.1);
    assert!(!ballpark.balls.contains_key(&991_020));
    let mults = active_wubble_multipliers_for_ballpark(&ballpark);
    assert!((mults.get(&991_010).copied().unwrap_or(0.0) - 0.70).abs() < 1e-6);
    assert!(!mults.contains_key(&991_011));
    assert!(!mults.contains_key(&991_012));

    // Step 10.5s: debuff expires and clears active_wubble_multipliers + only wrong-sys pending remains
    step_wubble_probes_and_debuffs(&mut ballpark, &sde_some, &db_some, &ctx.tactical_state, 10.5);
    assert!(active_wubble_multipliers_for_ballpark(&ballpark).is_empty());
    let st = ctx.tactical_state.read().unwrap();
    assert_eq!(st.pending_wubble_probes.len(), 1);
    assert_eq!(st.pending_wubble_probes[0].1.probe_id, 991_022);
}

#[tokio::test]
async fn test_tactical_rtc_tsb_break_locks_autotarget_wh_jump_and_nav_locks() {
    let ctx = test_ctx_with_custom_types(|sde| {
        sde.types.insert(920_001, custom_inv_type(sde, 920_001, 209, 7, "Remote Tracking Comp", &[(351, 20.0), (349, 30.0), (767, 40.0), (54, 10_000.0), (2044, 5_000.0)]));
        sde.types.insert(920_002, custom_inv_type(sde, 920_002, 1154, 7, "Spectrum Breaker", &[(6, 10.0), (73, 12_000.0), (669, 150_000.0), (3114, 0.20)]));
        sde.types.insert(920_003, custom_inv_type(sde, 920_003, 96, 7, "Auto Targeting System I", &[(143, 60_000.0)]));
    });
    let sys_id = 30_000_142_u32;
    let char_id = 92_100_001_i64;
    let enemy_cid = 92_100_002_i64;
    let fleet_cid = 92_100_003_i64;
    let ship_id = 92_200_001_i64;
    let weak_locker = 92_200_002_i64;
    let strong_locker = 92_200_003_i64;
    let fleet_ship = 92_200_004_i64;
    let far_enemy = 92_200_005_i64;

    ensure_char(&ctx, char_id, 1_000_170, sys_id, ship_id, 2);
    ensure_char(&ctx, enemy_cid, 98_900_001, sys_id, weak_locker, 2);
    ensure_char(&ctx, fleet_cid, 1_000_170, sys_id, fleet_ship, 2);
    insert_item(&ctx, ship_id, 587, char_id, sys_id as i64, InventoryFlag::None, 1, true);
    insert_item(&ctx, weak_locker, 587, enemy_cid, sys_id as i64, InventoryFlag::None, 1, true);
    insert_item(&ctx, strong_locker, 19_720, enemy_cid, sys_id as i64, InventoryFlag::None, 1, true);
    insert_item(&ctx, fleet_ship, 587, fleet_cid, sys_id as i64, InventoryFlag::None, 1, true);
    insert_item(&ctx, far_enemy, 587, enemy_cid, sys_id as i64, InventoryFlag::None, 1, true);
    ctx.db.with_mut_snapshot(|snap| {
        snap.fleets.insert(5001, eve_db::FleetRecord {
            fleet_id: 5001,
            boss_id: char_id,
            members: vec![char_id, fleet_cid],
            ..Default::default()
        });
    });

    let sys = ctx.systems.get_or_spawn(sys_id);
    let mk_cs = |cap: f64| CombatState { capacitor: cap, max_capacitor: 500.0, shield_hp: 1000.0, max_shield_hp: 1000.0, armor_hp: 1000.0, max_armor_hp: 1000.0, structure_hp: 1000.0, max_structure_hp: 1000.0, signature_radius: 40.0, ..Default::default() };
    let _ = sys.spawn_ball_with_combat(Ball { id: ship_id, type_id: 587, position: DVec3::ZERO, velocity: DVec3::ZERO, max_velocity: 400.0, inertia: 1.0, mass: 1e6, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop }, mk_cs(400.0)).await;
    let _ = sys.spawn_ball_with_combat(Ball { id: weak_locker, type_id: 587, position: DVec3::new(10_000.0, 0.0, 0.0), velocity: DVec3::ZERO, max_velocity: 400.0, inertia: 1.0, mass: 1e6, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop }, mk_cs(400.0)).await;
    let _ = sys.spawn_ball_with_combat(Ball { id: strong_locker, type_id: 19_720, position: DVec3::new(15_000.0, 0.0, 0.0), velocity: DVec3::ZERO, max_velocity: 100.0, inertia: 1.0, mass: 1e9, radius: 500.0, speed_fraction: 0.0, mode: MovementMode::Stop }, mk_cs(400.0)).await;
    let _ = sys.spawn_ball_with_combat(Ball { id: fleet_ship, type_id: 587, position: DVec3::new(5_000.0, 0.0, 0.0), velocity: DVec3::ZERO, max_velocity: 400.0, inertia: 1.0, mass: 1e6, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop }, mk_cs(400.0)).await;
    let _ = sys.spawn_ball_with_combat(Ball { id: far_enemy, type_id: 587, position: DVec3::new(250_000.0, 0.0, 0.0), velocity: DVec3::ZERO, max_velocity: 400.0, inertia: 1.0, mass: 1e6, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop }, mk_cs(400.0)).await;

    // apply_projected_remote_assistance CharacterNotFound & apply_projected_rtc_to_module_attrs (1.0 / 0.20 / 0.05)
    let rtc_t = ctx.sde.types.get(&920_001).unwrap().clone();
    let rtc_st = DogmaItemState::from_sde(920_501, 920_001, &ctx.sde);
    assert_eq!(
        apply_projected_remote_assistance(&ctx, 99_999_999, weak_locker, 920_501, &rtc_t, &rtc_st, Some(ship_id), 1_000.0)
            .unwrap_err()
            .code,
        "CharacterNotFound"
    );
    apply_projected_remote_assistance(&ctx, char_id, fleet_ship, 920_501, &rtc_t, &rtc_st, Some(ship_id), 1_000.0).expect("rtc assist");
    let mut mod_attrs_base = HashMap::from([(54u16, 10_000.0), (158u16, 5_000.0), (160u16, 1.0)]);
    apply_projected_rtc_to_module_attrs(&ctx, ship_id, &mut mod_attrs_base);
    assert!((mod_attrs_base[&54] - 12_000.0).abs() < 1e-6);

    insert_item(&ctx, 920_600, 20280, char_id, ship_id, InventoryFlag::HiSlot0, 1, true);
    ctx.db.with_mut_snapshot(|snap| { snap.module_active_states.insert(920_600); });
    let mut mod_attrs_siege = HashMap::from([(54u16, 10_000.0)]);
    apply_projected_rtc_to_module_attrs(&ctx, ship_id, &mut mod_attrs_siege);
    assert!((mod_attrs_siege[&54] - 10_400.0).abs() < 1e-6);

    insert_item(&ctx, 920_601, 33400, char_id, ship_id, InventoryFlag::HiSlot1, 1, true);
    ctx.db.with_mut_snapshot(|snap| { snap.module_active_states.insert(920_601); });
    let mut mod_attrs_bastion = HashMap::from([(54u16, 10_000.0)]);
    apply_projected_rtc_to_module_attrs(&ctx, ship_id, &mut mod_attrs_bastion);
    assert!((mod_attrs_bastion[&54] - 10_100.0).abs() < 1e-6);

    // prepare_ship_for_wormhole_jump: immobilized by active Bastion -> clear active -> clean -> skyhook nav lock
    let mut dummy_ball = mk_cs(0.0);
    let _ = &mut dummy_ball;
    let mut ship_ball = Ball { id: ship_id, type_id: 587, position: DVec3::ZERO, velocity: DVec3::ZERO, max_velocity: 400.0, inertia: 1.0, mass: 1e6, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop };
    assert_eq!(
        prepare_ship_for_wormhole_jump(&ctx, &sys, char_id, ship_id, &mut ship_ball).await.unwrap_err().code,
        "CannotJumpWormholeWhileImmobilized"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_active_states.remove(&920_600);
        snap.module_active_states.remove(&920_601);
    });
    assert_eq!(prepare_ship_for_wormhole_jump(&ctx, &sys, char_id, ship_id, &mut ship_ball).await.unwrap(), false);

    // ship_navigation_lock_expiry_unix & check_ship_navigation_lock for skyhook link
    let exp = ctx.now_unix_secs() + 120;
    ctx.db.with_mut_snapshot(|snap| {
        snap.sov_runtime.skyhook_silo_links.insert(888, (99, ship_id, exp, 0.0));
    });
    assert!(ship_navigation_lock_expiry_unix(&ctx, ship_id) >= exp);
    assert_eq!(check_ship_navigation_lock(&ctx, ship_id, "warp").unwrap_err().code, "SkyhookLinkActive");
    ctx.db.with_mut_snapshot(|snap| { snap.sov_runtime.skyhook_silo_links.clear(); });

    // activate_target_spectrum_breaker with real locking ships
    let _ = sys.add_target_lock(weak_locker, eve_destiny::TargetLock { target_id: ship_id, lock_time_ms: 1.0, remaining_lock_ms: 0.0, locked: true, max_range_m: None }).await;
    let _ = sys.add_target_lock(strong_locker, eve_destiny::TargetLock { target_id: ship_id, lock_time_ms: 1.0, remaining_lock_ms: 0.0, locked: true, max_range_m: None }).await;
    let tsb_t = ctx.sde.types.get(&920_002).unwrap().clone();
    let tsb_st = DogmaItemState::from_sde(920_700, 920_002, &ctx.sde);
    let tsb_rep = activate_target_spectrum_breaker(&ctx, &sys, char_id, ship_id, 920_700, &tsb_t, &tsb_st).await.expect("tsb break locks");
    assert_eq!(tsb_rep.dict_get("lockingShipCount"), Some(&PyRep::Integer(2)));
    assert!(tsb_rep.dict_get("brokenLocks").and_then(|v| v.as_i64()).unwrap_or(0) >= 1);
    assert!(sys.get_target_locks(weak_locker).await.unwrap().is_empty());

    // activate_auto_targeting_system locking offensive aggressor while skipping fleetmate & out-of-range
    ctx.crimewatch
        .write()
        .unwrap()
        .get_or_create(enemy_cid)
        .criminal_flag_until_unix = ctx.now_unix_secs() + 60;
    let _ = sys.add_target_lock(weak_locker, eve_destiny::TargetLock { target_id: ship_id, lock_time_ms: 1.0, remaining_lock_ms: 0.0, locked: true, max_range_m: None }).await;
    let ats_t = ctx.sde.types.get(&920_003).unwrap().clone();
    let ats_st = DogmaItemState::from_sde(920_701, 920_003, &ctx.sde);
    insert_item(&ctx, 920_701, 920_003, char_id, ship_id, InventoryFlag::MedSlot0, 1, true);
    let fit = ShipFittingEngine::from_sde(ship_id, 587, &ctx.sde);
    let ats_rep = activate_auto_targeting_system(&ctx, &sys, char_id, ship_id, 920_701, &ats_t, &ats_st, &fit).await.expect("auto target");
    assert_eq!(ats_rep.dict_get("active"), Some(&PyRep::Bool(true)));
    let locked_ids = match ats_rep.dict_get("autoLockingTargetIDs") {
        Some(PyRep::List(v)) => v.iter().filter_map(|x| x.as_i64()).collect::<Vec<_>>(),
        _ => Vec::new(),
    };
    assert!(locked_ids.contains(&weak_locker));
    assert!(!locked_ids.contains(&fleet_ship));
    assert!(!locked_ids.contains(&far_enemy));

    // apply_transient_item_attrs
    ctx.db.with_mut_snapshot(|snap| {
        snap.transient_item_attributes.insert(ship_id, HashMap::from([(9u16, 1234.0)]));
    });
    let mut merged = HashMap::new();
    apply_transient_item_attrs(&ctx, ship_id, &mut merged);
    assert_eq!(merged.get(&9), Some(&1234.0));
}

#[tokio::test]
async fn test_entosis_subcap_contested_persist_and_command_burst_variants_crimewatch() {
    let ctx = test_ctx_with_custom_types(|sde| {
        sde.types.insert(930_001, custom_inv_type(sde, 930_001, 1313, 7, "Entosis Link I", &[(73, 120_000.0)]));
        sde.types.insert(930_002, custom_inv_type(sde, 930_002, 200, 7, "Rig Command Proc", &[]));
        if let Some(t) = sde.types.get_mut(&930_002) {
            t.effect_ids.push(6766);
            t.default_attributes.insert(2542, 1.0);
        }
        sde.types.insert(930_003, custom_inv_type(sde, 930_003, 300, 8, "Armored Command Mindlink", &[]));
        sde.types.insert(930_010, custom_inv_type(sde, 930_010, 1770, 7, "Shield Command Burst I", &[(422, 2.0), (604, 1771.0)]));
        sde.types.insert(930_011, custom_inv_type(sde, 930_011, 1770, 7, "Armor Command Burst I", &[(422, 2.0), (604, 1772.0)]));
        sde.types.insert(930_012, custom_inv_type(sde, 930_012, 1770, 7, "Skirmish Command Burst I", &[(422, 2.0), (604, 1773.0)]));
        sde.types.insert(930_013, custom_inv_type(sde, 930_013, 1770, 7, "Information Command Burst I", &[(422, 2.0), (604, 1774.0)]));
        sde.types.insert(930_020, custom_inv_type(sde, 930_020, 1771, 8, "Shield Extension Charge", &[(2468, 90.0), (2596, -15.0)]));
        sde.types.insert(930_021, custom_inv_type(sde, 930_021, 1772, 8, "Armor Energizing Charge", &[(2468, 90.0), (2596, -15.0)]));
        sde.types.insert(930_022, custom_inv_type(sde, 930_022, 1773, 8, "Rapid Deployment Charge", &[(2468, 90.0), (2596, -15.0)]));
        sde.types.insert(930_023, custom_inv_type(sde, 930_023, 1774, 8, "Sensor Optimization Charge", &[(2468, 90.0), (2596, -15.0)]));
    });
    let high_sys = 30_000_142_u32;
    let low_sys = 30_000_144_u32;
    let char_a = 93_100_001_i64;
    let char_b = 93_100_002_i64;
    let crim_c = 93_100_003_i64;
    let susp_c = 93_100_004_i64;
    let le_c = 93_100_005_i64;
    let far_c = 93_100_006_i64;

    let subcap_a = 93_200_001_i64;
    let subcap_b = 93_200_002_i64;
    let crim_ship = 93_200_003_i64;
    let susp_ship = 93_200_004_i64;
    let le_ship = 93_200_005_i64;
    let far_ship = 93_200_006_i64;
    let node_id = 93_300_001_i64;
    let unvuln_node_id = 93_300_002_i64;

    ensure_char(&ctx, char_a, 98_930_001, high_sys, subcap_a, 2);
    ensure_char(&ctx, char_b, 98_930_002, high_sys, subcap_b, 2);
    ensure_char(&ctx, crim_c, 98_930_003, high_sys, crim_ship, 2);
    ensure_char(&ctx, susp_c, 98_930_004, high_sys, susp_ship, 2);
    ensure_char(&ctx, le_c, 98_930_005, high_sys, le_ship, 2);
    ensure_char(&ctx, far_c, 98_930_001, high_sys, far_ship, 2);

    insert_item(&ctx, subcap_a, 24_696, char_a, high_sys as i64, InventoryFlag::None, 1, true); // Command Ship or Battlecruiser
    insert_item(&ctx, subcap_b, 587, char_b, high_sys as i64, InventoryFlag::None, 1, true);
    insert_item(&ctx, crim_ship, 587, crim_c, high_sys as i64, InventoryFlag::None, 1, true);
    insert_item(&ctx, susp_ship, 587, susp_c, high_sys as i64, InventoryFlag::None, 1, true);
    insert_item(&ctx, le_ship, 587, le_c, high_sys as i64, InventoryFlag::None, 1, true);
    insert_item(&ctx, far_ship, 587, far_c, high_sys as i64, InventoryFlag::None, 1, true);

    insert_item(&ctx, 93_400_001, 930_001, char_a, subcap_a, InventoryFlag::HiSlot0, 1, true);
    insert_item(&ctx, 93_400_002, 16_275, char_a, subcap_a, InventoryFlag::Cargo, 10, false);
    insert_item(&ctx, 93_400_003, 930_001, char_b, subcap_b, InventoryFlag::HiSlot0, 1, true);
    insert_item(&ctx, 93_400_004, 16_275, char_b, subcap_b, InventoryFlag::Cargo, 10, false);
    insert_item(&ctx, node_id, 32_226, char_a, high_sys as i64, InventoryFlag::None, 1, true);
    insert_item(&ctx, unvuln_node_id, 32_226, char_a, high_sys as i64, InventoryFlag::None, 1, true);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&unvuln_node_id).unwrap().dynamic_attributes.insert(2110, 0.0);
    });

    let sys = ctx.systems.get_or_spawn(high_sys);
    let _ = sys.spawn_ball_with_combat(Ball { id: subcap_a, type_id: 24_696, position: DVec3::ZERO, velocity: DVec3::ZERO, max_velocity: 6_000.0, inertia: 1.0, mass: 1e7, radius: 200.0, speed_fraction: 0.0, mode: MovementMode::Stop }, CombatState { max_capacitor: 1000.0, capacitor: 1000.0, max_velocity_cap: Some(4_000.0), ..Default::default() }).await;
    let _ = sys.spawn_ball_with_combat(Ball { id: subcap_b, type_id: 587, position: DVec3::new(1000.0, 0.0, 0.0), velocity: DVec3::ZERO, max_velocity: 400.0, inertia: 1.0, mass: 1e6, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop }, CombatState::default()).await;
    let _ = sys.spawn_ball_with_combat(Ball { id: crim_ship, type_id: 587, position: DVec3::new(1000.0, 0.0, 0.0), velocity: DVec3::ZERO, max_velocity: 400.0, inertia: 1.0, mass: 1e6, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop }, CombatState::default()).await;
    let _ = sys.spawn_ball_with_combat(Ball { id: susp_ship, type_id: 587, position: DVec3::new(1000.0, 0.0, 0.0), velocity: DVec3::ZERO, max_velocity: 400.0, inertia: 1.0, mass: 1e6, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop }, CombatState::default()).await;
    let _ = sys.spawn_ball_with_combat(Ball { id: le_ship, type_id: 587, position: DVec3::new(1000.0, 0.0, 0.0), velocity: DVec3::ZERO, max_velocity: 400.0, inertia: 1.0, mass: 1e6, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop }, CombatState::default()).await;
    let _ = sys.spawn_ball_with_combat(Ball { id: far_ship, type_id: 587, position: DVec3::new(900_000.0, 0.0, 0.0), velocity: DVec3::ZERO, max_velocity: 400.0, inertia: 1.0, mass: 1e6, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop }, CombatState::default()).await;

    let ent_t = ctx.sde.types.get(&930_001).unwrap().clone();
    assert_eq!(
        handle_entosis_link_activation(&ctx, &sys, char_a, subcap_a, 93_400_001, &ent_t, Some(unvuln_node_id)).await.unwrap_err().code,
        "TargetNotVulnerable"
    );

    // Subcap cycle 1 -> clamps velocity 6000->4000, cycleDurationSeconds == 120, isWarmup == true
    let e1 = handle_entosis_link_activation(&ctx, &sys, char_a, subcap_a, 93_400_001, &ent_t, Some(node_id)).await.unwrap();
    assert_eq!(e1.dict_get("cycleDurationSeconds"), Some(&PyRep::Real(120.0)));
    assert_eq!(e1.dict_get("isWarmup"), Some(&PyRep::Bool(true)));
    assert_eq!(e1.dict_get("captureRateMultiplier"), Some(&PyRep::Real(0.0)));
    assert!((sys.get_ball_snapshot(subcap_a).await.unwrap().unwrap().1.max_velocity - 4_000.0).abs() < 1e-6);

    // Advance clock past expiry & run cycle 2 uncontested -> rate == 1.0, then add opposing corp cycle 2 -> contested/paused
    let db_key = db_ptr_key(&ctx.db);
    let now_u = ctx.now_unix_secs();
    {
        let mut st = ctx.tactical_state.write().unwrap();
        st.entosis_active_links.get_mut(&(db_key, subcap_a)).unwrap().expires_unix = now_u;
    }
    let e2 = handle_entosis_link_activation(&ctx, &sys, char_a, subcap_a, 93_400_001, &ent_t, Some(node_id)).await.unwrap();
    assert_eq!(e2.dict_get("cycleIndex"), Some(&PyRep::Integer(2)));
    assert_eq!(e2.dict_get("captureRateMultiplier"), Some(&PyRep::Real(1.0)));
    assert_eq!(e2.dict_get("isPaused"), Some(&PyRep::Bool(false)));

    {
        let mut st = ctx.tactical_state.write().unwrap();
        st.entosis_target_cycles.insert((db_key, node_id, subcap_b), 1);
    }
    let e_contest = handle_entosis_link_activation(&ctx, &sys, char_b, subcap_b, 93_400_003, &ent_t, Some(node_id)).await.unwrap();
    assert_eq!(e_contest.dict_get("isPaused"), Some(&PyRep::Bool(true)));
    assert_eq!(e_contest.dict_get("contested"), Some(&PyRep::Bool(true)));
    assert_eq!(e_contest.dict_get("captureRateMultiplier"), Some(&PyRep::Real(0.0)));

    // Command Burst: Omega gate, MaxGroupActiveExceeded unlocked by Command Processor rig, Shield/Armor/Skirmish/Info + Mindlink + Highsec vs Lowsec crimewatch
    let shield_burst_tid = 930_010_u32;
    let armor_burst_tid = 930_011_u32;
    let skirmish_burst_tid = 930_012_u32;
    let info_burst_tid = 930_013_u32;

    let sh_c = 930_020_u32;
    let ar_c = 930_021_u32;
    let sk_c = 930_022_u32;
    let in_c = 930_023_u32;

    let b_mod_1 = 93_500_001_i64;
    let b_mod_2 = 93_500_002_i64;
    let b_mod_3 = 93_500_003_i64;
    let b_mod_4 = 93_500_004_i64;
    let c_id_1 = 93_510_001_i64;
    let c_id_2 = 93_510_002_i64;
    let c_id_3 = 93_510_003_i64;
    let c_id_4 = 93_510_004_i64;
    insert_item(&ctx, b_mod_1, armor_burst_tid, char_a, subcap_a, InventoryFlag::HiSlot1, 1, true);
    insert_item(&ctx, c_id_1, ar_c, char_a, subcap_a, InventoryFlag::HiSlot1, 1, false); // qty=1 exhausts charge!
    insert_item(&ctx, b_mod_2, shield_burst_tid, char_a, subcap_a, InventoryFlag::HiSlot2, 1, true);
    insert_item(&ctx, c_id_2, sh_c, char_a, subcap_a, InventoryFlag::HiSlot2, 10, false);
    insert_item(&ctx, b_mod_3, skirmish_burst_tid, char_a, subcap_a, InventoryFlag::HiSlot3, 1, true);
    insert_item(&ctx, c_id_3, sk_c, char_a, subcap_a, InventoryFlag::HiSlot3, 10, false);
    insert_item(&ctx, b_mod_4, info_burst_tid, char_a, subcap_a, InventoryFlag::HiSlot4, 1, true);
    insert_item(&ctx, c_id_4, in_c, char_a, subcap_a, InventoryFlag::HiSlot4, 10, false);

    // Alpha clone blocked
    ctx.db.with_mut_snapshot(|snap| { snap.characters.get_mut(&char_a).unwrap().clone_grade = 0; });
    let mut fit = ShipFittingEngine::from_sde(subcap_a, 24_696, &ctx.sde);
    for sk in [3350u32, 3351, 3352, 3353, 3349, 3355, 20494, 20495, 11574, 24764, 3354, 33095] {
        fit.character_skills.insert(sk, 5);
    }
    fit.implants.push(930_003);
    let ar_t = ctx.sde.types.get(&armor_burst_tid).unwrap().clone();
    let ar_s = DogmaItemState::from_sde(b_mod_1, armor_burst_tid, &ctx.sde);
    assert_eq!(
        activate_command_burst(&ctx, &sys, char_a, subcap_a, high_sys, b_mod_1, &ar_t, &ar_s, None, &fit).await.unwrap_err().code,
        "OmegaRequiredForCommandBurst"
    );

    // Omega + Fleet + SafetyLevel::Green in Highsec: excludes crim_c, susp_c, le_c, far_c; exhausts qty=1 charge
    let now_u2 = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_a).unwrap().clone_grade = 2;
        snap.fleets.insert(9301, eve_db::FleetRecord {
            fleet_id: 9301,
            boss_id: char_a,
            members: vec![char_a, crim_c, susp_c, le_c, far_c],
            ..Default::default()
        });
    });
    {
        let mut cw = ctx.crimewatch.write().unwrap();
        cw.get_or_create(crim_c).criminal_flag_until_unix = now_u2 + 900;
        cw.get_or_create(susp_c).suspect_flag_until_unix = now_u2 + 900;
    }
    crate::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| {
        rt.safety_levels.insert(char_a, crate::services::combat_crimewatch::SafetyLevel::Green);
        rt.limited_engagements.insert(le_c, HashMap::from([(9_999_999, now_u2 + 600)]));
        rt.npc_timers.insert(le_c, now_u2 + 600);
    });

    // Pass stale charge_tid_opt Some(sh_c) on Armor burst to verify fallback to slot's loaded ar_c + qty=1 exhaustion!
    let br_arm = activate_command_burst(&ctx, &sys, char_a, subcap_a, high_sys, b_mod_1, &ar_t, &ar_s, Some(sh_c), &fit).await.unwrap();
    assert_eq!(br_arm.dict_get("chargeTypeID"), Some(&PyRep::Integer(i64::from(ar_c))));
    assert!((br_arm.dict_get("buffDurationSeconds").and_then(|v| v.as_f64()).unwrap() - 112.5).abs() < 1e-6);
    ctx.db.with_snapshot(|snap| assert!(!snap.module_active_charges.contains_key(&b_mod_1)));
    let affected_armor: Vec<i64> = match br_arm.dict_get("affectedShipIDs") {
        Some(PyRep::List(v)) => v.iter().filter_map(|x| x.as_i64()).collect(),
        _ => vec![],
    };
    assert!(affected_armor.contains(&subcap_a));
    assert!(!affected_armor.contains(&crim_ship));
    assert!(!affected_armor.contains(&susp_ship));
    assert!(!affected_armor.contains(&far_ship));

    // Shield & Skirmish bursts (3 bursts allowed once Command Processor rig fitted, blocked before)
    let sh_t = ctx.sde.types.get(&shield_burst_tid).unwrap().clone();
    let sh_s = DogmaItemState::from_sde(b_mod_2, shield_burst_tid, &ctx.sde);
    activate_command_burst(&ctx, &sys, char_a, subcap_a, high_sys, b_mod_2, &sh_t, &sh_s, None, &fit).await.unwrap();

    let sk_t = ctx.sde.types.get(&skirmish_burst_tid).unwrap().clone();
    let sk_s = DogmaItemState::from_sde(b_mod_3, skirmish_burst_tid, &ctx.sde);
    assert_eq!(
        activate_command_burst(&ctx, &sys, char_a, subcap_a, high_sys, b_mod_3, &sk_t, &sk_s, None, &fit).await.unwrap_err().code,
        "MaxGroupActiveExceeded"
    );

    // Add Command Processor rig -> unlocks 3rd active burst & test Yellow suspect trigger in Highsec vs Red criminal CONCORD
    insert_item(&ctx, 93_590_001, 930_002, char_a, subcap_a, InventoryFlag::RigSlot0, 1, true);
    crate::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| {
        rt.safety_levels.insert(char_a, crate::services::combat_crimewatch::SafetyLevel::Yellow);
    });
    let br_sk = activate_command_burst(&ctx, &sys, char_a, subcap_a, high_sys, b_mod_3, &sk_t, &sk_s, None, &fit).await.unwrap();
    let affected_sk: Vec<i64> = match br_sk.dict_get("affectedShipIDs") {
        Some(PyRep::List(v)) => v.iter().filter_map(|x| x.as_i64()).collect(),
        _ => vec![],
    };
    assert!(affected_sk.contains(&susp_ship));
    assert!(!affected_sk.contains(&crim_ship));
    assert!(!affected_sk.contains(&far_ship));

    // Lowsec test with Info burst + Red safety -> includes LE + crim_ship and triggers criminal response
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_active_states.remove(&b_mod_1);
        for cid in [char_a, crim_c, le_c] {
            if let Some(ch) = snap.characters.get_mut(&cid) {
                ch.solar_system_id = low_sys;
            }
        }
        if let Some(ch) = snap.characters.get_mut(&susp_c) {
            ch.solar_system_id = high_sys; // not in low_sys
        }
    });
    let low_handle = ctx.systems.get_or_spawn(low_sys);
    let _ = low_handle.spawn_ball_with_combat(Ball { id: subcap_a, type_id: 24_696, position: DVec3::ZERO, velocity: DVec3::ZERO, max_velocity: 400.0, inertia: 1.0, mass: 1e7, radius: 200.0, speed_fraction: 0.0, mode: MovementMode::Stop }, CombatState::default()).await;
    let _ = low_handle.spawn_ball_with_combat(Ball { id: crim_ship, type_id: 587, position: DVec3::new(1000.0, 0.0, 0.0), velocity: DVec3::ZERO, max_velocity: 400.0, inertia: 1.0, mass: 1e6, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop }, CombatState::default()).await;
    let _ = low_handle.spawn_ball_with_combat(Ball { id: le_ship, type_id: 587, position: DVec3::new(1000.0, 0.0, 0.0), velocity: DVec3::ZERO, max_velocity: 400.0, inertia: 1.0, mass: 1e6, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop }, CombatState::default()).await;
    crate::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| {
        rt.safety_levels.insert(char_a, crate::services::combat_crimewatch::SafetyLevel::Red);
    });
    let in_t = ctx.sde.types.get(&info_burst_tid).unwrap().clone();
    let in_s = DogmaItemState::from_sde(b_mod_4, info_burst_tid, &ctx.sde);
    let br_in = activate_command_burst(&ctx, &low_handle, char_a, subcap_a, low_sys, b_mod_4, &in_t, &in_s, None, &fit).await.unwrap();
    let affected_in: Vec<i64> = match br_in.dict_get("affectedShipIDs") {
        Some(PyRep::List(v)) => v.iter().filter_map(|x| x.as_i64()).collect(),
        _ => vec![],
    };
    assert!(affected_in.contains(&crim_ship));
    assert!(affected_in.contains(&le_ship));
}

#[test]
fn test_bastion_siege_triage_and_sensor_remote_assistance_attrs() {
    let ctx = test_ctx_with_custom_types(|sde| {
        sde.types.insert(33_400, custom_inv_type(sde, 33_400, 515, 7, "Bastion Module I", &[]));
        sde.types.insert(20_280, custom_inv_type(sde, 20_280, 515, 7, "Siege Module I", &[]));
        sde.types.insert(4_292, custom_inv_type(sde, 4_292, 515, 7, "Siege Module II", &[]));
        sde.types.insert(4_294, custom_inv_type(sde, 4_294, 515, 7, "Triage Module II", &[]));
    });
    let mut missing_attrs = HashMap::new();
    apply_tactical_module_ship_attributes(&ctx, 999_777_666, &mut missing_attrs);
    assert!(missing_attrs.is_empty());

    let char_id = 919_001;
    let ship_id = 919_100;
    let sys_id = 30_000_142;
    ensure_char(&ctx, char_id, 1_000_125, sys_id, ship_id, 1);
    insert_item(&ctx, ship_id, 587, char_id, sys_id as i64, InventoryFlag::Hangar, 1, true);
    insert_item(&ctx, 919_200, 33_400, char_id, ship_id, InventoryFlag::HiSlot0, 1, true);
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_active_states.insert(919_200);
    });
    let db_key = db_ptr_key(&ctx.db);
    {
        let mut st = ctx.tactical_state.write().unwrap();
        st.remote_assistance.insert(
            (db_key, ship_id),
            vec![ProjectedRemoteAssistance {
                source_ship_id: 919_101,
                module_id: 919_300,
                module_type_id: 1_952,
                max_target_range_bonus_pct: 50.0,
                scan_resolution_bonus_pct: 40.0,
                sensor_strength_bonus_pct: 100.0,
                optimal_range_bonus_pct: 0.0,
                falloff_bonus_pct: 0.0,
                tracking_speed_bonus_pct: 0.0,
            }],
        );
    }

    let mut bastion_attrs = HashMap::from([(76u16, 100_000.0), (564u16, 200.0), (208u16, 10.0), (209u16, 12.0)]);
    apply_tactical_module_ship_attributes(&ctx, ship_id, &mut bastion_attrs);
    assert!((bastion_attrs[&2135] - 0.05).abs() < 1e-6);
    assert!((bastion_attrs[&2115] - 1.0).abs() < 1e-6);
    // Remote assistance scaled by assist_mult 0.05 -> +5% sensor strength instead of +100%
    assert!((bastion_attrs[&208] - 10.5).abs() < 1e-4);

    // Siege T2 overrides Bastion branch
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&919_200).unwrap().type_id = 4_292;
    });
    let mut siege_attrs = HashMap::new();
    apply_tactical_module_ship_attributes(&ctx, ship_id, &mut siege_attrs);
    assert!((siege_attrs[&2112] - 0.30).abs() < 1e-6);
    assert!((siege_attrs[&2114] - 1.0).abs() < 1e-6);

    // Triage T2
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&919_200).unwrap().type_id = 4_294;
    });
    let mut triage_attrs = HashMap::from([(192u16, 5.0)]);
    apply_tactical_module_ship_attributes(&ctx, ship_id, &mut triage_attrs);
    assert!((triage_attrs[&2112] - 0.30).abs() < 1e-6);
    assert!(triage_attrs[&192] >= 9.0);
}

