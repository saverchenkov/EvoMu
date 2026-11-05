#![forbid(unsafe_code)]

use super::*;
use eve_db::{InventoryFlag, InventoryItemRecord, VirtualServerClock, WorldRepository};
use eve_destiny::{Ball, CombatState, MovementMode, SlimItemMeta};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::{InvType, SdeRegistry};
use glam::DVec3;
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
    volume: f64,
    capacity: f64,
    name: &str,
    attrs: &[(u16, f64)],
) -> InvType {
    let mut t = sde.types.get(&587).expect("seed type").clone();
    t.type_id = type_id;
    t.group_id = group_id;
    t.category_id = category_id;
    t.volume = volume;
    t.capacity = capacity;
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

fn ensure_char(ctx: &ServerContext, char_id: i64, corp_id: i64, sys_id: u32, ship_id: i64) {
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
        ch.clone_grade = 1;
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
    let _ = ctx
        .db
        .acquire_online_lock(char_id as u64, char_id as u64, Some(char_id), true, ctx.now_unix_secs());
}

fn insert_item(
    ctx: &ServerContext,
    item_id: i64,
    type_id: u32,
    owner_id: i64,
    location_id: i64,
    flag: InventoryFlag,
    quantity: i32,
    singleton: bool,
) {
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            item_id,
            InventoryItemRecord {
                item_id,
                type_id,
                owner_id,
                location_id,
                flag,
                quantity,
                singleton,
                ..Default::default()
            },
        );
    });
}

async fn spawn_ship(ctx: &ServerContext, sys_id: u32, ship_id: i64, type_id: u32, owner_id: i64, cap: f64) {
    let sys = ctx.systems.get_or_spawn(sys_id);
    let ball = Ball {
        id: ship_id,
        type_id,
        position: DVec3::new(10_000.0, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 500.0,
        inertia: 1.0,
        mass: 1_000_000.0,
        radius: 100.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let mut cs = CombatState::default();
    cs.capacitor = cap;
    cs.max_capacitor = cap;
    let meta = SlimItemMeta {
        group_id: 25,
        category_id: 6,
        owner_id,
        char_id: Some(owner_id),
        corp_id: 98_000_001,
        alliance_id: 0,
        security_status: 5.0,
        is_cloaked: false,
        ..Default::default()
    };
    sys.spawn_ball_with_meta(ball, cs, meta)
        .await
        .expect("spawn ship");
}

#[tokio::test]
async fn handle_module_cycles_basic_error_guards_and_overheat_activation() {
    let ctx = test_ctx_with_custom(|sde| {
        sde.types.insert(
            999_501,
            custom_inv_type(
                sde,
                999_501,
                46,
                7,
                5.0,
                0.0,
                "1MN Test Afterburner I",
                &[(6, 10.0), (73, 10_000.0), (20, 100.0), (1211, 2.0)],
            ),
        );
    });

    let char_id = 97_000_001;
    let sys_id = 30_002_505;
    let ship_id = 1_000_700_001;
    let ab_mod_id = 1_000_700_002;
    let offline_mod_id = 1_000_700_003;

    ensure_char(&ctx, char_id, 98_000_001, sys_id, ship_id);
    insert_item(&ctx, ship_id, 587, char_id, sys_id as i64, InventoryFlag::None, 1, true);
    insert_item(&ctx, ab_mod_id, 999_501, char_id, ship_id, InventoryFlag::MedSlot0, 1, true);
    insert_item(&ctx, offline_mod_id, 999_501, char_id, ship_id, InventoryFlag::MedSlot1, 1, true);
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(offline_mod_id, false);
    });
    spawn_ship(&ctx, sys_id, ship_id, 587, char_id, 500.0).await;

    let kw = HashMap::new();
    let mut sess = SessionState {
        user_id: Some(char_id as u64),
        character_id: Some(char_id),
        corporation_id: Some(98_000_001),
        solar_system_id: Some(sys_id),
        ship_id: None,
        ..Default::default()
    };

    // Unhandled method returns None
    assert!(handle_module_cycles(&ctx, &mut sess, None, "dogmaIM", "GetAllInfo", &[], &kw).await.is_none());

    // Missing ship -> NoActiveShip
    assert_eq!(
        handle_module_cycles(&ctx, &mut sess, None, "dogmaIM", "Activate", &[PyRep::Integer(ab_mod_id)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "NoActiveShip"
    );

    // Docked / missing solarSystemID -> NoSolarSystem
    sess.ship_id = Some(ship_id);
    sess.solar_system_id = None;
    assert_eq!(
        handle_module_cycles(&ctx, &mut sess, None, "dogmaIM", "Activate", &[PyRep::Integer(ab_mod_id)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "NoSolarSystem"
    );

    // Offline module cannot activate
    sess.solar_system_id = Some(sys_id);
    assert!(
        handle_module_cycles(
            &ctx,
            &mut sess,
            None,
            "dogmaIM",
            "Activate",
            &[PyRep::Integer(offline_mod_id), PyRep::String("defaultEffect".into())],
            &kw,
        )
        .await
        .unwrap()
        .is_err()
    );

    // Overload Afterburner: succeeds, emits OnHeatAdded & OnGodmaShipEffect, and returns overloaded=true
    let ov_rep = handle_module_cycles(
        &ctx,
        &mut sess,
        None,
        "dogmaIM",
        "Overload",
        &[PyRep::Integer(ab_mod_id), PyRep::String("defaultEffect".into())],
        &kw,
    )
    .await
    .unwrap()
    .expect("overload afterburner");
    assert_eq!(ov_rep.dict_get("overloaded").and_then(|v| v.as_bool()), Some(true));
    let notifs = ctx.drain_notifications_for_char(char_id);
    assert!(notifs.iter().any(|n| n.event == "OnHeatAdded"));
    assert!(notifs.iter().any(|n| n.event == "OnGodmaShipEffect"));

    // Second Overload call while active succeeds with overloaded=true
    let toggle_rep = handle_module_cycles(
        &ctx,
        &mut sess,
        None,
        "dogmaIM",
        "Overload",
        &[PyRep::Integer(ab_mod_id)],
        &kw,
    )
    .await
    .unwrap()
    .expect("overload idempotent call");
    assert_eq!(toggle_rep.dict_get("overloaded").and_then(|v| v.as_bool()), Some(true));
}

#[tokio::test]
async fn handle_module_cycles_cloak_and_mwd_warp_scramble_guards() {
    let ctx = test_ctx_with_custom(|sde| {
        let mut ab_t = custom_inv_type(sde, 999_501, 46, 7, 5.0, 0.0, "1MN Test Afterburner I", &[(6, 10.0), (73, 5_000.0)]);
        ab_t.effect_ids = vec![13];
        sde.types.insert(999_501, ab_t);
        let mut cloak_t = custom_inv_type(sde, 11370, 330, 7, 5.0, 0.0, "Prototype Cloaking Device I", &[(73, 5_000.0)]);
        cloak_t.effect_ids = vec![12];
        sde.types.insert(11370, cloak_t);
        let mut mwd_t = custom_inv_type(
            sde,
            999_502,
            46,
            7,
            5.0,
            0.0,
            "5MN Test Microwarpdrive I",
            &[(6, 20.0), (73, 10_000.0), (20, 500.0), (554, 500.0)],
        );
        mwd_t.effect_ids = vec![13];
        sde.types.insert(999_502, mwd_t);
        let mut lance_t = custom_inv_type(sde, 77398, 588, 7, 100.0, 0.0, "Disruptive Lance", &[]);
        lance_t.effect_ids = vec![12];
        sde.types.insert(77398, lance_t);
        let mut ship_t = custom_inv_type(
            sde,
            999_500,
            25,
            6,
            50_000.0,
            200.0,
            "Test Multi-Slot Hull",
            &[(14, 4.0), (13, 4.0), (12, 4.0), (11, 10_000.0), (48, 10_000.0), (482, 10_000.0), (1132, 1_000.0)],
        );
        ship_t.effect_ids.clear();
        sde.types.insert(999_500, ship_t);
    });

    let char_id = 97_000_010;
    let sys_id = 30_002_505;
    let ship_id = 1_000_700_101;
    let cloak_id = 1_000_700_102;
    let mwd_id = 1_000_700_103;
    let lance_id = 1_000_700_104;

    ensure_char(&ctx, char_id, 98_000_001, sys_id, ship_id);
    insert_item(&ctx, ship_id, 999_500, char_id, sys_id as i64, InventoryFlag::None, 1, true);
    insert_item(&ctx, cloak_id, 11370, char_id, ship_id, InventoryFlag::HiSlot0, 1, true);
    insert_item(&ctx, mwd_id, 999_502, char_id, ship_id, InventoryFlag::MedSlot0, 1, true);
    insert_item(&ctx, lance_id, 77398, char_id, ship_id, InventoryFlag::HiSlot1, 1, true);
    spawn_ship(&ctx, sys_id, ship_id, 999_500, char_id, 500.0).await;

    let kw = HashMap::new();
    // 1. Cloak disallowed in Abyssal Deadspace (32_000_001)
    let mut abyss_sess = SessionState {
        user_id: Some(char_id as u64),
        character_id: Some(char_id),
        solar_system_id: Some(32_000_001),
        ship_id: Some(ship_id),
        ..Default::default()
    };
    assert_eq!(
        handle_module_cycles(&ctx, &mut abyss_sess, None, "dogmaIM", "Activate", &[PyRep::Integer(cloak_id)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "CannotCloakInAbyssalSpace"
    );

    // 2. Cloak disallowed while Disruptive Lance / Superweapon (group 588) fitted
    let mut sess = SessionState {
        user_id: Some(char_id as u64),
        character_id: Some(char_id),
        solar_system_id: Some(sys_id),
        ship_id: Some(ship_id),
        ..Default::default()
    };
    if ctx.sde.types.contains_key(&77398) {
        assert_eq!(
            handle_module_cycles(&ctx, &mut sess, None, "dogmaIM", "Activate", &[PyRep::Integer(cloak_id)], &kw)
                .await
                .unwrap()
                .unwrap_err()
                .code,
            "CannotCloakWithLanceFitted"
        );
    }

    // 3. MWD disallowed while warp scrambled (`is_mwd_mjd_disabled`)
    let sys = ctx.systems.get_or_spawn(sys_id);
    if let (Ok(Some((_, ball))), Ok(Some(mut cs))) = (sys.get_ball_snapshot(ship_id).await, sys.get_combat_state(ship_id).await) {
        cs.is_mwd_mjd_disabled = true;
        let _ = sys.spawn_ball_with_meta(ball, cs, SlimItemMeta {
            group_id: 25,
            category_id: 6,
            owner_id: char_id,
            char_id: Some(char_id),
            ..Default::default()
        }).await;
    }
    assert_eq!(
        handle_module_cycles(&ctx, &mut sess, None, "dogmaIM", "Activate", &[PyRep::Integer(mwd_id)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "WarpScrambled"
    );
}

#[tokio::test]
async fn handle_module_cycles_survey_probe_launcher_auto_reload_and_launch() {
    let ctx = test_ctx_with_custom(|sde| {
        let mut l_t = custom_inv_type(sde, 33270, 1226, 7, 5.0, 2.0, "Survey Probe Launcher I", &[(73, 10_000.0)]);
        l_t.effect_ids = vec![12];
        sde.types.insert(33270, l_t);
        sde.types.insert(
            22778,
            custom_inv_type(sde, 22778, 492, 8, 0.2, 0.0, "Quest Survey Probe I", &[(2697, 100_000_000.0)]),
        );
    });

    let char_id = 97_000_020;
    let sys_id = 30_000_142;
    let ship_id = 1_000_700_201;
    let launcher_id = 1_000_700_202;
    let loaded_probe_id = 1_000_700_203;
    let cargo_probes_id = 1_000_700_204;

    ensure_char(&ctx, char_id, 98_000_001, sys_id, ship_id);
    insert_item(&ctx, ship_id, 587, char_id, sys_id as i64, InventoryFlag::None, 1, true);
    insert_item(&ctx, launcher_id, 33270, char_id, ship_id, InventoryFlag::HiSlot0, 1, true);
    insert_item(&ctx, loaded_probe_id, 22778, char_id, ship_id, InventoryFlag::HiSlot0, 1, false);
    insert_item(&ctx, cargo_probes_id, 22778, char_id, ship_id, InventoryFlag::Cargo, 5, false);
    spawn_ship(&ctx, sys_id, ship_id, 587, char_id, 500.0).await;

    let kw = HashMap::new();
    let mut sess = SessionState {
        user_id: Some(char_id as u64),
        character_id: Some(char_id),
        solar_system_id: Some(sys_id),
        ship_id: Some(ship_id),
        ..Default::default()
    };

    // Calling with launcher_id launches last loaded probe and auto-reloads 5 from cargo!
    let rep = handle_module_cycles(
        &ctx,
        &mut sess,
        None,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(launcher_id)],
        &kw,
    )
    .await
    .unwrap()
    .expect("launch survey probe");
    assert_eq!(rep.dict_get("launcherID").and_then(|v| v.as_i64()), Some(launcher_id));
    assert_eq!(rep.dict_get("autoReloadedCount").and_then(|v| v.as_i64()), Some(5));
    assert_eq!(rep.dict_get("remainingInLauncher").and_then(|v| v.as_i64()), Some(5));
    assert!(ctx
        .drain_notifications_for_char(char_id)
        .iter()
        .any(|n| n.event == "OnNewMoonProbe"));
}
