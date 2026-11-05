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
        assert_eq!(cs.suspect_flag_until_unix, now + 900);
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
    insert_item(&ctx, tgt_id, 587, 915_002, null_sys as i64, InventoryFlag::Hangar, 1, true);

    spawn_ship_ball_with_cs(&ctx, null_sys, titan_id, 11_567, char_id, 1000125, DVec3::ZERO, 100.0).await;
    spawn_ship_ball_with_cs(&ctx, null_sys, tgt_id, 587, 915_002, 1000125, DVec3::new(10_000.0, 0.0, 0.0), 1000.0).await;
    let sys = ctx.systems.get_or_spawn(null_sys);

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
