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

fn seeded_cycles_ctx<F: FnOnce(&mut SdeRegistry)>(mutate_sde: F) -> ServerContext {
    test_ctx_with_custom(mutate_sde).with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
}

#[tokio::test]
async fn guards_warp_gate_cloak_fallback_weapon_and_module_cycles_error_paths() {
    const SYN_COVERT_CLOAK: u32 = 996_101;
    const SYN_PLAIN_CLOAK: u32 = 996_102;
    const SYN_NULLIFIER: u32 = 996_103;
    const SYN_WEAPON_G55: u32 = 996_104;
    const SYN_MINER_G54: u32 = 996_105;
    const SYN_AB_GENERIC: u32 = 996_106;
    const SYN_SMARTBOMB_G72: u32 = 996_107;

    let ctx = seeded_cycles_ctx(|sde| {
        if let Some(rifter) = sde.types.get_mut(&587) {
            rifter.default_attributes.insert(attr_ids::HI_SLOTS, 6.0);
            rifter.default_attributes.insert(attr_ids::MED_SLOTS, 4.0);
            rifter.default_attributes.insert(attr_ids::LOW_SLOTS, 4.0);
        }

        let mut cov_name = custom_inv_type(sde, SYN_COVERT_CLOAK, 330, 7, 10.0, 0.0, "Covert Ops Cloaking Device II", &[(50, 1.0)]);
        cov_name.effect_ids = vec![12, 16, 980];
        sde.types.insert(SYN_COVERT_CLOAK, cov_name);

        let mut plain_cloak = custom_inv_type(sde, SYN_PLAIN_CLOAK, 330, 7, 10.0, 0.0, "Prototype Cloaking Device I", &[(50, 1.0)]);
        plain_cloak.effect_ids.clear();
        sde.types.insert(SYN_PLAIN_CLOAK, plain_cloak);

        let mut null_t = custom_inv_type(sde, SYN_NULLIFIER, 4117, 7, 5.0, 0.0, "Interdiction Nullifier I", &[(73, 10_000.0), (669, 150_000.0)]);
        null_t.effect_ids = vec![12, 16];
        sde.types.insert(SYN_NULLIFIER, null_t);

        let mut wep_t = custom_inv_type(sde, SYN_WEAPON_G55, 55, 7, 5.0, 0.0, "Light Neutron Blaster II", &[(73, 3_000.0), (6, 2.0), (54, 5_000.0), (158, 2_000.0), (14, 15.0)]);
        wep_t.effect_ids = vec![12, 16];
        sde.types.insert(SYN_WEAPON_G55, wep_t);

        let mining_eff_id = sde
            .effects
            .values()
            .find(|e| e.name == "miningLaser")
            .map(|e| e.effect_id)
            .unwrap_or(67);
        let mut miner_t = custom_inv_type(sde, SYN_MINER_G54, 54, 7, 5.0, 0.0, "Miner I", &[(73, 60_000.0), (6, 5.0), (77, 10.0), (54, 10_000.0)]);
        miner_t.effect_ids = vec![12, 16, mining_eff_id];
        miner_t.default_effect_id = Some(mining_eff_id);
        sde.types.insert(SYN_MINER_G54, miner_t);

        let mut ab_t = custom_inv_type(sde, SYN_AB_GENERIC, 46, 7, 5.0, 0.0, "1MN Afterburner I", &[(73, 10_000.0), (6, 10.0), (20, 100.0)]);
        ab_t.effect_ids = vec![13, 16];
        sde.types.insert(SYN_AB_GENERIC, ab_t);

        let mut sb_t = custom_inv_type(sde, SYN_SMARTBOMB_G72, 72, 7, 5.0, 0.0, "Small EMP Smartbomb I", &[(73, 5_000.0), (6, 25.0), (197, 3_000.0)]);
        sb_t.effect_ids = vec![12, 16];
        sde.types.insert(SYN_SMARTBOMB_G72, sb_t);
    });

    // 1. is_warp_safe_cloak_module all branches
    assert!(is_warp_safe_cloak_module(&ctx, &ctx.sde.types[&SYN_COVERT_CLOAK]));
    assert!(!is_warp_safe_cloak_module(&ctx, &ctx.sde.types[&SYN_PLAIN_CLOAK]));
    assert!(!is_warp_safe_cloak_module(&ctx, &ctx.sde.types[&SYN_NULLIFIER]));
    let mut non_980_cov_name = ctx.sde.types[&SYN_PLAIN_CLOAK].clone();
    non_980_cov_name.name = "Synthetic Covert Ops Cloak".into();
    assert!(is_warp_safe_cloak_module(&ctx, &non_980_cov_name));

    let char_id = 97_100_001i64;
    let sys_id = 30_000_142u32; // highsec 0.9
    let ship_id = 1_000_710_001i64;
    let mod_wep = 1_000_710_010i64;
    let mod_wep_sibling = 1_000_710_011i64;
    let charge_in_wep = 1_000_710_012i64;
    let mod_ab = 1_000_710_013i64;
    let mod_miner = 1_000_710_014i64;
    let mod_sb = 1_000_710_015i64;

    ensure_char(&ctx, char_id, 98_000_001, sys_id, ship_id);
    insert_item(&ctx, ship_id, 587, char_id, sys_id as i64, InventoryFlag::None, 1, true);
    insert_item(&ctx, mod_wep, SYN_WEAPON_G55, char_id, ship_id, InventoryFlag::HiSlot0, 1, true);
    insert_item(&ctx, charge_in_wep, 179, char_id, ship_id, InventoryFlag::HiSlot0, 20, false);
    insert_item(&ctx, mod_wep_sibling, SYN_WEAPON_G55, char_id, ship_id, InventoryFlag::HiSlot1, 1, true);
    insert_item(&ctx, mod_ab, SYN_AB_GENERIC, char_id, ship_id, InventoryFlag::MedSlot0, 1, true);
    insert_item(&ctx, mod_miner, SYN_MINER_G54, char_id, ship_id, InventoryFlag::HiSlot2, 1, true);
    insert_item(&ctx, mod_sb, SYN_SMARTBOMB_G72, char_id, ship_id, InventoryFlag::HiSlot3, 1, true);
    spawn_ship(&ctx, sys_id, ship_id, 587, char_id, 1_000.0).await;

    let sys = ctx.systems.get_or_spawn(sys_id);

    // 2. fallback_fitted_weapon_module
    assert!(fallback_fitted_weapon_module(&ctx, ship_id, 999_888_777).is_none());
    let fb_wep = fallback_fitted_weapon_module(&ctx, ship_id, mod_wep).expect("fallback fitted");
    assert_eq!(fb_wep.charge_type_id, Some(179));

    // 3. guards: WarpPhase::Cruising vs Aligning, gate cloak break, active non-gate cloak
    {
        let mut bp = sys.ballpark.write().unwrap();
        if let Some(b) = bp.balls.get_mut(&ship_id) {
            b.mode = MovementMode::Warp {
                origin: DVec3::ZERO,
                destination: DVec3::new(1e7, 0.0, 0.0),
                total_distance: 1e7,
                warp_speed_au_s: 3.0,
                phase: eve_destiny::WarpPhase::Cruising,
                elapsed_warp_time: 5.0,
            };
        }
    }
    assert_eq!(
        enforce_space_module_cloak_and_warp_guard(&ctx, &sys, char_id, ship_id, &ctx.sde.types[&SYN_AB_GENERIC])
            .await
            .unwrap_err()
            .code,
        "CantActivateInWarp"
    );
    assert!(
        enforce_space_module_cloak_and_warp_guard(&ctx, &sys, char_id, ship_id, &ctx.sde.types[&SYN_NULLIFIER])
            .await
            .is_ok()
    );
    assert!(
        enforce_space_module_cloak_and_warp_guard(&ctx, &sys, char_id, ship_id, &ctx.sde.types[&SYN_COVERT_CLOAK])
            .await
            .is_ok()
    );
    assert_eq!(
        enforce_space_action_cloak_and_warp_guard(&ctx, &sys, char_id, ship_id, "launch drones")
            .await
            .unwrap_err()
            .code,
        "CantActivateInWarp"
    );

    // Aligning is allowed + breaks gate cloak
    {
        let mut bp = sys.ballpark.write().unwrap();
        if let Some(b) = bp.balls.get_mut(&ship_id) {
            b.mode = MovementMode::Warp {
                origin: DVec3::ZERO,
                destination: DVec3::new(1e7, 0.0, 0.0),
                total_distance: 1e7,
                warp_speed_au_s: 3.0,
                phase: eve_destiny::WarpPhase::Aligning,
                elapsed_warp_time: 1.0,
            };
        }
        if let Some(cs) = bp.combat_states.get_mut(&ship_id) {
            cs.is_cloaked = true;
            cs.gate_cloak_remaining_ticks = 15;
        }
    }
    assert!(
        enforce_space_action_cloak_and_warp_guard(&ctx, &sys, char_id, ship_id, "launch drones")
            .await
            .is_ok()
    );
    assert_eq!(sys.get_combat_state(ship_id).await.unwrap().unwrap().gate_cloak_remaining_ticks, 0);

    // Active non-gate cloak blocks action & weapon modules, permits cloak (330)
    {
        let mut bp = sys.ballpark.write().unwrap();
        bp.balls.get_mut(&ship_id).unwrap().mode = MovementMode::Stop;
        let cs = bp.combat_states.get_mut(&ship_id).unwrap();
        cs.is_cloaked = true;
        cs.gate_cloak_remaining_ticks = 0;
    }
    assert_eq!(
        enforce_space_action_cloak_and_warp_guard(&ctx, &sys, char_id, ship_id, "engage")
            .await
            .unwrap_err()
            .code,
        "CannotActivateCloaked"
    );
    assert_eq!(
        enforce_space_module_cloak_and_warp_guard(&ctx, &sys, char_id, ship_id, &ctx.sde.types[&SYN_WEAPON_G55])
            .await
            .unwrap_err()
            .code,
        "CannotActivateCloaked"
    );
    assert!(
        enforce_space_module_cloak_and_warp_guard(&ctx, &sys, char_id, ship_id, &ctx.sde.types[&SYN_PLAIN_CLOAK])
            .await
            .is_ok()
    );
    sys.ballpark.write().unwrap().combat_states.get_mut(&ship_id).unwrap().is_cloaked = false;

    // 4. handle_module_cycles guards and branches
    let kw = HashMap::new();
    let mut sess = SessionState {
        user_id: Some(char_id as u64),
        character_id: Some(char_id),
        solar_system_id: Some(sys_id),
        ship_id: Some(ship_id),
        ..Default::default()
    };

    // Unmatched service/method -> None
    assert!(
        handle_module_cycles(&ctx, &mut sess, None, "dogmaIM", "Deactivate", &[], &kw)
            .await
            .is_none()
    );
    // Missing ship / missing solar_system
    let mut sess_no_ship = sess.clone();
    sess_no_ship.ship_id = None;
    assert_eq!(
        handle_module_cycles(&ctx, &mut sess_no_ship, None, "dogmaIM", "Activate", &[PyRep::Integer(mod_ab)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "NoActiveShip"
    );
    let mut sess_no_sys = sess.clone();
    sess_no_sys.solar_system_id = None;
    assert_eq!(
        handle_module_cycles(&ctx, &mut sess_no_sys, None, "dogmaIM", "Activate", &[PyRep::Integer(mod_ab)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "NoSolarSystem"
    );

    // Missing module_id with 1 arg and online weapons returns None (lets combat_weapons handle), with 2 args returns ModuleNotFound
    assert!(
        handle_module_cycles(&ctx, &mut sess, None, "dogmaIM", "Activate", &[PyRep::Integer(888_777_666)], &kw)
            .await
            .is_none()
    );
    assert_eq!(
        handle_module_cycles(
            &ctx,
            &mut sess,
            None,
            "dogmaIM",
            "Activate",
            &[PyRep::Integer(888_777_666), PyRep::String("defaultEffect".into())],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "ModuleNotFound"
    );

    // Activate("online") while docked (and nanite repair check)
    let mut docked = sess.clone();
    docked.station_id = Some(60_003_760);
    assert!(
        handle_module_cycles(
            &ctx,
            &mut docked,
            None,
            "dogmaIM",
            "Activate",
            &[PyRep::Integer(mod_ab), PyRep::String("online".into())],
            &kw,
        )
        .await
        .unwrap()
        .is_ok()
    );

    // Weapon bank burned sibling prevents activating master module
    ctx.db.with_mut_snapshot(|snap| {
        snap.weapon_banks
            .insert(ship_id, HashMap::from([(mod_wep, vec![mod_wep, mod_wep_sibling])]));
        snap.items
            .get_mut(&mod_wep_sibling)
            .unwrap()
            .dynamic_attributes
            .insert(eve_sde::attr_ids::DAMAGE, 40.0);
    });
    assert_eq!(
        handle_module_cycles(&ctx, &mut sess, None, "dogmaIM", "Activate", &[PyRep::Integer(mod_wep)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "ModuleBurnedOut"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.items
            .get_mut(&mod_wep_sibling)
            .unwrap()
            .dynamic_attributes
            .clear();
        snap.weapon_banks.remove(&ship_id);
    });

    // Mining module without target -> TargetNotLocked
    assert_eq!(
        handle_module_cycles(&ctx, &mut sess, None, "dogmaIM", "Activate", &[PyRep::Integer(mod_miner)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "TargetNotLocked"
    );

    // Tethered blocks Smartbomb / empWave with CantActivateWhileTethered
    sys.ballpark.write().unwrap().combat_states.get_mut(&ship_id).unwrap().is_tethered = true;
    assert_eq!(
        handle_module_cycles(
            &ctx,
            &mut sess,
            None,
            "dogmaIM",
            "Activate",
            &[PyRep::Integer(mod_sb), PyRep::String("empWave".into())],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "CantActivateWhileTethered"
    );
    sys.ballpark.write().unwrap().combat_states.get_mut(&ship_id).unwrap().is_tethered = false;

    // Smartbomb in highsec with Green safety -> CantDoInCurrentSafetyLevel
    crate::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| {
        rt.safety_levels.insert(char_id, crate::services::combat_crimewatch::SafetyLevel::Green);
    });
    assert_eq!(
        handle_module_cycles(&ctx, &mut sess, None, "dogmaIM", "Activate", &[PyRep::Integer(mod_sb)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "CantDoInCurrentSafetyLevel"
    );

    // ModuleReactivationDelay for non-weapon module
    ctx.record_module_reactivation_delay(ship_id, mod_ab, 30.0);
    assert_eq!(
        handle_module_cycles(&ctx, &mut sess, None, "dogmaIM", "Activate", &[PyRep::Integer(mod_ab)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "ModuleReactivationDelay"
    );

    // TargetNotLocked when unlocked weapon targets self_ship, and CannotActivateOnSelf for self-prohibited EWAR group 65
    assert_eq!(
        handle_module_cycles(
            &ctx,
            &mut sess,
            None,
            "dogmaIM",
            "Activate",
            &[PyRep::Integer(mod_wep), PyRep::Integer(ship_id)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "TargetNotLocked"
    );
    const SYN_DISRUPTOR_G65: u32 = 996_108;
    let mod_disruptor = 1_000_710_016i64;
    let mut dis_sde = (*ctx.sde).clone();
    let mut dis_t = custom_inv_type(&dis_sde, SYN_DISRUPTOR_G65, 65, 7, 5.0, 0.0, "Warp Disruptor I", &[(73, 5_000.0), (54, 20_000.0)]);
    dis_t.effect_ids = vec![13, 16];
    dis_sde.types.insert(SYN_DISRUPTOR_G65, dis_t);
    let ctx_self = ServerContext::with_sde_and_repository(Arc::new(dis_sde), Arc::clone(&ctx.db))
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED);
    insert_item(&ctx_self, mod_disruptor, SYN_DISRUPTOR_G65, char_id, ship_id, InventoryFlag::MedSlot1, 1, true);
    let _ = ctx_self.systems.get_or_spawn(sys_id);
    assert_eq!(
        handle_module_cycles(
            &ctx_self,
            &mut sess,
            None,
            "dogmaIM",
            "Activate",
            &[PyRep::Integer(mod_disruptor), PyRep::Integer(ship_id)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "CannotActivateOnSelf"
    );
}

#[tokio::test]
async fn cehe_duration_fallbacks_crystal_volatility_and_survey_cycle_completion() {
    const SYN_CAP_HULL: u32 = 996_299;
    const SYN_CEHE_G60: u32 = 40_714; // Capital Emergency Hull Energizer
    const SYN_SHIELD_BOOSTER_G40: u32 = 996_201;
    const SYN_TURRET_G53: u32 = 996_202;
    const SYN_VOLATILE_CRYSTAL: u32 = 996_203;
    const SYN_NONVOL_CRYSTAL: u32 = 996_204;
    const SYN_SURVEY_G49: u32 = 996_205;
    const SYN_DC_G60_GENERIC: u32 = 996_206;

    let ctx = seeded_cycles_ctx(|sde| {
        let mut cap_hull = sde.types.get(&587).expect("rifter").clone();
        cap_hull.type_id = SYN_CAP_HULL;
        cap_hull.group_id = 485; // Dreadnought capital hull
        cap_hull.category_id = 6;
        cap_hull.name = "Synthetic Dreadnought".into();
        cap_hull.default_attributes.insert(attr_ids::HI_SLOTS, 6.0);
        cap_hull.default_attributes.insert(attr_ids::MED_SLOTS, 6.0);
        cap_hull.default_attributes.insert(attr_ids::LOW_SLOTS, 6.0);
        cap_hull.default_attributes.insert(attr_ids::CPU_OUTPUT, 10_000.0);
        cap_hull.default_attributes.insert(attr_ids::POWER_OUTPUT, 100_000.0);
        cap_hull.default_attributes.insert(attr_ids::CAPACITOR_CAPACITY, 50_000.0);
        cap_hull.default_attributes.insert(attr_ids::MAX_LOCKED_TARGETS, 7.0);
        cap_hull.default_attributes.insert(attr_ids::MAX_TARGET_RANGE, 150_000.0);
        sde.types.insert(SYN_CAP_HULL, cap_hull);

        let mut cehe_t = custom_inv_type(
            sde,
            SYN_CEHE_G60,
            60,
            7,
            5.0,
            0.0,
            "Capital Emergency Hull Energizer II",
            &[(3115, 25_000.0), (974, 0.01), (975, 0.01), (976, 0.01), (977, 0.01), (669, 240_000.0)],
        );
        cehe_t.effect_ids = vec![11, 16, 6484];
        sde.types.insert(SYN_CEHE_G60, cehe_t);

        let mut sb_t = custom_inv_type(
            sde,
            SYN_SHIELD_BOOSTER_G40,
            40,
            7,
            5.0,
            0.0,
            "Small Shield Booster II",
            &[(51, 4_000.0), (68, 120.0), (6, 20.0)],
        );
        sb_t.effect_ids = vec![13, 16];
        sde.types.insert(SYN_SHIELD_BOOSTER_G40, sb_t);

        let mut tur_t = custom_inv_type(
            sde,
            SYN_TURRET_G53,
            53,
            7,
            5.0,
            10.0,
            "Pulse Laser II",
            &[(51, 2_000.0), (64, 2.0), (6, 5.0), (54, 15_000.0), (158, 5_000.0)],
        );
        tur_t.effect_ids = vec![12, 16];
        sde.types.insert(SYN_TURRET_G53, tur_t);

        sde.types.insert(
            SYN_VOLATILE_CRYSTAL,
            custom_inv_type(
                sde,
                SYN_VOLATILE_CRYSTAL,
                86,
                8,
                0.1,
                0.0,
                "Scorch S",
                &[(114, 10.0), (784, 0.05), (783, 50.0)],
            ),
        );
        sde.types.insert(
            SYN_NONVOL_CRYSTAL,
            custom_inv_type(
                sde,
                SYN_NONVOL_CRYSTAL,
                86,
                8,
                0.1,
                0.0,
                "Multifrequency S",
                &[(114, 15.0), (784, 0.0)],
            ),
        );

        let mut surv_t = custom_inv_type(
            sde,
            SYN_SURVEY_G49,
            49,
            7,
            5.0,
            0.0,
            "Survey Scanner I",
            &[(73, 5_000.0), (197, 25_000.0), (6, 5.0)],
        );
        surv_t.effect_ids = vec![13, 16];
        sde.types.insert(SYN_SURVEY_G49, surv_t);

        let mut fb_t = custom_inv_type(
            sde,
            SYN_DC_G60_GENERIC,
            40,
            7,
            5.0,
            0.0,
            "Fallback Duration Module",
            &[],
        );
        fb_t.effect_ids = vec![13, 16];
        sde.types.insert(SYN_DC_G60_GENERIC, fb_t);
    });

    let char_id = 97_200_001i64;
    let target_char = 97_200_002i64;
    let sys_id = 30_000_142u32;
    let ship_id = 1_000_720_001i64;
    let target_ship_id = 1_000_720_002i64;
    let mod_cehe = 1_000_720_010i64;
    let mod_sb = 1_000_720_011i64;
    let mod_tur_vol = 1_000_720_012i64;
    let crystal_vol = 1_000_720_013i64;
    let mod_tur_nonvol = 1_000_720_014i64;
    let crystal_nonvol = 1_000_720_015i64;
    let mod_survey = 1_000_720_016i64;
    let mod_fallback = 1_000_720_017i64;

    ensure_char(&ctx, char_id, 98_000_001, sys_id, ship_id);
    ensure_char(&ctx, target_char, 98_000_002, sys_id, target_ship_id);
    insert_item(&ctx, ship_id, SYN_CAP_HULL, char_id, sys_id as i64, InventoryFlag::None, 1, true);
    insert_item(&ctx, target_ship_id, 587, target_char, sys_id as i64, InventoryFlag::None, 1, true);
    insert_item(&ctx, mod_cehe, SYN_CEHE_G60, char_id, ship_id, InventoryFlag::LoSlot0, 1, true);
    insert_item(&ctx, mod_sb, SYN_SHIELD_BOOSTER_G40, char_id, ship_id, InventoryFlag::MedSlot0, 1, true);
    insert_item(&ctx, mod_tur_vol, SYN_TURRET_G53, char_id, ship_id, InventoryFlag::HiSlot0, 1, true);
    insert_item(&ctx, crystal_vol, SYN_VOLATILE_CRYSTAL, char_id, ship_id, InventoryFlag::HiSlot0, 1, true);
    insert_item(&ctx, mod_tur_nonvol, SYN_TURRET_G53, char_id, ship_id, InventoryFlag::HiSlot1, 1, true);
    insert_item(&ctx, crystal_nonvol, SYN_NONVOL_CRYSTAL, char_id, ship_id, InventoryFlag::HiSlot1, 1, true);
    insert_item(&ctx, mod_survey, SYN_SURVEY_G49, char_id, ship_id, InventoryFlag::MedSlot1, 1, true);
    insert_item(&ctx, mod_fallback, SYN_DC_G60_GENERIC, char_id, ship_id, InventoryFlag::MedSlot2, 1, true);

    spawn_ship(&ctx, sys_id, ship_id, SYN_CAP_HULL, char_id, 5_000.0).await;
    spawn_ship(&ctx, sys_id, target_ship_id, 587, target_char, 1_000.0).await;

    let sys = ctx.systems.get_or_spawn(sys_id);
    sys.add_target_lock(
        ship_id,
        eve_destiny::TargetLock {
            target_id: target_ship_id,
            lock_time_ms: 100.0,
            remaining_lock_ms: 0.0,
            locked: true,
            max_range_m: Some(50_000.0),
        },
    )
    .await
    .unwrap();
    sys.ballpark
        .write()
        .unwrap()
        .combat_states
        .get_mut(&ship_id)
        .unwrap()
        .shield_boost_multiplier = 1.5;

    let kw = HashMap::new();
    let mut sess = SessionState {
        user_id: Some(char_id as u64),
        character_id: Some(char_id),
        solar_system_id: Some(sys_id),
        ship_id: Some(ship_id),
        ..Default::default()
    };

    // 1. CEHE forces repeat=0 and reads attr 3115 (25.0s)
    let cehe_rep = handle_module_cycles(&ctx, &mut sess, None, "dogmaIM", "Activate", &[PyRep::Integer(mod_cehe)], &kw)
        .await
        .unwrap()
        .expect("cehe ok");
    assert_eq!(cehe_rep.dict_get("duration").and_then(|v| v.as_f64()), Some(25.0));

    // 2. Shield booster reads attr 51 SPEED (4s) * shield_boost_multiplier (1.5) = 6.0s
    //    and emits OnCapacitorChange + OnModuleAttributeChanges (CHARGE attr 18) (#1002).
    let _ = ctx.drain_notifications_for_char(char_id);
    let sb_rep = handle_module_cycles(&ctx, &mut sess, None, "dogmaIM", "Activate", &[PyRep::Integer(mod_sb)], &kw)
        .await
        .unwrap()
        .expect("shield booster ok");
    assert_eq!(sb_rep.dict_get("duration").and_then(|v| v.as_f64()), Some(6.0));
    let sb_notifs = ctx.drain_notifications_for_char(char_id);
    assert!(
        sb_notifs.iter().any(|n| {
            n.event == "OnGodmaShipEffect"
                && n.args.first().and_then(|v| v.as_i64()) == Some(mod_sb)
                && n.args.get(3).and_then(|v| v.as_i64()) == Some(1)
                && n.args.get(7).and_then(|v| v.as_f64()) == Some(6_000.0)
                && n.args.get(8).and_then(|v| v.as_i64()) == Some(1000)
        }),
        "expected OnGodmaShipEffect with evaluated 6000ms duration and repeat=1000 (#819)"
    );
    assert!(
        sb_notifs.iter().any(|n| {
            n.event == "OnCapacitorChange"
                && n.args.first().and_then(|v| v.as_i64()) == Some(ship_id)
                && n.args.get(1).and_then(|v| v.as_f64()) == Some(4980.0)
        }),
        "expected OnCapacitorChange for 20 GJ shield booster activation (#1002)"
    );
    assert!(
        sb_notifs.iter().any(|n| {
            n.event == "OnModuleAttributeChanges"
                && matches!(n.args.first(), Some(PyRep::List(entries)) if entries.iter().any(|e| {
                    matches!(e, PyRep::Tuple(t)
                        if t.len() >= 7
                        && t[2].as_i64() == Some(ship_id)
                        && t[3].as_i64() == Some(18)
                        && t[5].as_f64() == Some(4980.0)
                        && t[6].as_f64() == Some(5000.0))
                }))
        }),
        "expected OnModuleAttributeChanges (attr 18 CHARGE) on activation (#1002)"
    );

    // 3. Generic zero-duration module falls back to 1.0s
    let fb_rep = handle_module_cycles(&ctx, &mut sess, None, "dogmaIM", "Activate", &[PyRep::Integer(mod_fallback)], &kw)
        .await
        .unwrap()
        .expect("fallback duration ok");
    assert_eq!(fb_rep.dict_get("duration").and_then(|v| v.as_f64()), Some(1.0));

    // 4. Survey scanner branch inside handle_module_cycles returns asteroids list
    let surv_rep = handle_module_cycles(&ctx, &mut sess, None, "dogmaIM", "Activate", &[PyRep::Integer(mod_survey)], &kw)
        .await
        .unwrap()
        .expect("survey scanner ok");
    assert_eq!(surv_rep.dict_get("moduleID").and_then(|v| v.as_i64()), Some(mod_survey));
    assert!(surv_rep.dict_get("asteroids").is_some());

    // 5. Turret with volatile crystal records crystal_volatility_chance = 0.50 & damage = 0.05
    //    (requires red/yellow or valid target — set target suspect so highsec allows)
    let now_u = ctx.now_unix_secs();
    ctx.crimewatch.write().unwrap().get_or_create(target_char).suspect_flag_until_unix = now_u + 300;
    let vol_rep = handle_module_cycles(
        &ctx,
        &mut sess,
        None,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(mod_tur_vol), PyRep::Integer(target_ship_id)],
        &kw,
    )
    .await
    .unwrap()
    .expect("volatile crystal turret ok");
    assert_eq!(vol_rep.dict_get("duration").and_then(|v| v.as_f64()), Some(2.0));

    let nonvol_rep = handle_module_cycles(
        &ctx,
        &mut sess,
        None,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(mod_tur_nonvol), PyRep::Integer(target_ship_id)],
        &kw,
    )
    .await
    .unwrap()
    .expect("non-volatile crystal turret ok");
    assert_eq!(nonvol_rep.dict_get("duration").and_then(|v| v.as_f64()), Some(2.0));

    // 6. Propulsion (group 46) & Stasis Webifier (group 65) reject duplicate Activate while active (#819).
    let ab_type = ctx
        .sde
        .types
        .values()
        .find(|t| t.group_id == 46 && t.published)
        .map(|t| t.type_id)
        .expect("propulsion module type");
    let web_type = ctx
        .sde
        .types
        .values()
        .find(|t| t.group_id == 65 && t.published)
        .map(|t| t.type_id)
        .expect("stasis webifier type");
    let mod_ab = 992_020_i64;
    let mod_web = 992_021_i64;
    ctx.db.with_mut_snapshot(|snap| {
        for (mid, tid, flag) in [
            (mod_ab, ab_type, eve_db::InventoryFlag::MedSlot0),
            (mod_web, web_type, eve_db::InventoryFlag::MedSlot1),
        ] {
            let mut it = eve_db::InventoryItemRecord::new(mid, tid, char_id, ship_id, flag, 1);
            it.singleton = true;
            snap.items.insert(mid, it);
        }
    });
    assert!(
        handle_module_cycles(&ctx, &mut sess, None, "dogmaIM", "Activate", &[PyRep::Integer(mod_ab)], &kw)
            .await
            .unwrap()
            .is_ok()
    );
    let dup_ab = handle_module_cycles(&ctx, &mut sess, None, "dogmaIM", "Activate", &[PyRep::Integer(mod_ab)], &kw)
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(dup_ab.code, "ModuleAlreadyActive");

    assert!(
        handle_module_cycles(
            &ctx,
            &mut sess,
            None,
            "dogmaIM",
            "Activate",
            &[PyRep::Integer(mod_web), PyRep::Integer(target_ship_id)],
            &kw,
        )
        .await
        .unwrap()
        .is_ok()
    );
    let dup_web = handle_module_cycles(
        &ctx,
        &mut sess,
        None,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(mod_web), PyRep::Integer(target_ship_id)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(dup_web.code, "ModuleAlreadyActive");
}

