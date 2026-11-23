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

#[tokio::test]
async fn handle_weapons_targetless_overload_bypasses_warp_cloak_tether_safety_and_salvager_branch() {
    let sb_tid = 990_072_u32;
    let salv_tid = 991_122_u32;
    let turret_tid = 990_053_u32;
    let ctx = test_ctx_with_custom(|sde| {
        sde.effects.entry(3002).or_insert_with(|| eve_sde::DogmaEffectType {
            effect_id: 3002,
            name: "overloadSelfDurationBonus".into(),
            effect_category: 5,
            published: true,
            ..Default::default()
        });
        let mut sb = custom_inv_type(
            sde,
            sb_tid,
            72,
            7,
            5.0,
            0.0,
            "Test Smartbomb",
            &[(1211, 6.7), (73, 7_500.0), (54, 5_000.0), (114, 200.0), (6, 50.0)],
        );
        sb.effect_ids = vec![38, 3002];
        sde.types.insert(sb_tid, sb);

        let mut salv = custom_inv_type(sde, salv_tid, 1122, 7, 5.0, 0.0, "Test Salvager", &[]);
        salv.effect_ids = vec![2757];
        sde.types.insert(salv_tid, salv);

        let mut turret = custom_inv_type(
            sde,
            turret_tid,
            53,
            7,
            5.0,
            0.0,
            "Test Laser",
            &[(1211, 0.6), (51, 2_000.0), (54, 10_000.0), (6, 10.0)],
        );
        turret.effect_ids = vec![10, 3002];
        sde.types.insert(turret_tid, turret);
    });

    let sys_id = 30_000_142_u32;
    let char_id = 911_001_i64;
    let ship_id = 912_001_i64;
    let sb_id = 913_001_i64;
    let salv_id = 913_002_i64;
    let turret_id = 913_003_i64;
    ensure_char(&ctx, char_id, 1_000_009, sys_id, ship_id);
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(char_id).or_default().insert(
            28_164,
            eve_db::TrainedSkillRecord { type_id: 28_164, level: 5, skill_points: 256_000.0 },
        );
        for (iid, tid, flag) in [
            (ship_id, 587_u32, InventoryFlag::None),
            (sb_id, sb_tid, InventoryFlag::HiSlot0),
            (salv_id, salv_tid, InventoryFlag::HiSlot1),
            (turret_id, turret_tid, InventoryFlag::HiSlot2),
        ] {
            let loc = if iid == ship_id { sys_id as i64 } else { ship_id };
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: tid,
                    owner_id: char_id,
                    location_id: loc,
                    flag,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
        }
    });
    ctx.invalidate_ship_fitting(ship_id);

    let sys = ctx.systems.get_or_spawn(sys_id);
    let mut sess = SessionState::new(char_id as u64);
    sess.user_id = Some(char_id as u64);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(1_000_009);
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(ship_id);
    let kw = HashMap::new();

    // Seed Green safety in high-sec
    crate::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| {
        rt.safety_levels.insert(char_id, crate::services::combat_crimewatch::SafetyLevel::Green);
    });

    for (mode, cloaked, gate_ticks, tethered) in [
        (
            MovementMode::Warp {
                origin: DVec3::ZERO,
                destination: DVec3::new(1_000_000_000.0, 0.0, 0.0),
                warp_speed_au_s: 3.0,
                total_distance: 1_000_000_000.0,
                elapsed_warp_time: 1.0,
                phase: eve_destiny::WarpPhase::Cruising,
            },
            false,
            0_u32,
            false,
        ),
        (MovementMode::Stop, true, 20, false),
        (MovementMode::Stop, true, 0, false),
        (MovementMode::Stop, false, 0, true),
    ] {
        let _ = sys.remove_ball(ship_id).await;
        let mut cs = ctx.build_ship_combat_state(char_id, ship_id, 587);
        cs.capacitor = 1_000.0;
        cs.max_capacitor = 1_000.0;
        cs.is_cloaked = cloaked;
        cs.gate_cloak_remaining_ticks = gate_ticks;
        cs.undock_invulnerable_remaining_ticks = 60;
        cs.warp_exit_invulnerable_remaining_ticks = 45;
        cs.is_tethered = tethered;
        sys.spawn_ball_with_meta(
            Ball {
                id: ship_id,
                type_id: 587,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 365.0,
                inertia: 1.0,
                mass: 1_067_000.0,
                radius: 37.0,
                speed_fraction: 0.0,
                mode,
            },
            cs,
            ctx.build_ship_slim_meta(char_id, 587),
        )
        .await
        .unwrap();

        let sb_res = crate::services::combat_crimewatch::combat_weapons::handle_weapons(
            &ctx, &mut sess, "dogmaIM", "Overload", &[PyRep::Integer(sb_id)], &kw,
        )
        .await
        .unwrap()
        .expect("handle_weapons targetless Overload on Smartbomb must succeed");
        assert_eq!(sb_res.dict_get("overloaded"), Some(&PyRep::Bool(true)));
        assert!(!sys.has_module_cycle(ship_id, sb_id).await);

        let turret_res = crate::services::combat_crimewatch::combat_weapons::handle_weapons(
            &ctx, &mut sess, "dogmaIM", "Overload", &[PyRep::Integer(turret_id)], &kw,
        )
        .await
        .unwrap()
        .expect("handle_weapons targetless Overload on Turret must succeed");
        assert_eq!(turret_res.dict_get("overloaded"), Some(&PyRep::Bool(true)));
        assert!(!sys.has_module_cycle(ship_id, turret_id).await);

        let cs_after = sys.get_combat_state(ship_id).await.unwrap().unwrap();
        assert_eq!(cs_after.is_cloaked, cloaked);
        assert_eq!(cs_after.gate_cloak_remaining_ticks, gate_ticks);
        assert_eq!(
            cs_after.undock_invulnerable_remaining_ticks, 60,
            "Targetless Overload in handle_weapons must preserve undock invulnerability"
        );
        assert_eq!(
            cs_after.warp_exit_invulnerable_remaining_ticks, 45,
            "Targetless Overload in handle_weapons must preserve warp-exit invulnerability"
        );

        let salv_err = crate::services::combat_crimewatch::combat_weapons::handle_weapons(
            &ctx, &mut sess, "dogmaIM", "Overload", &[PyRep::Integer(salv_id)], &kw,
        )
        .await
        .unwrap()
        .unwrap_err();
        assert_eq!(salv_err.code, "CannotOverloadModule");

        ctx.db.with_mut_snapshot(|snap| {
            if let Some(fi) = snap.ship_fit_inputs.get_mut(&ship_id) {
                fi.overloaded_modules.clear();
            }
        });
        ctx.invalidate_ship_fitting(ship_id);
    }
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
        let cat5_eid = sde
            .effects
            .values()
            .find(|e| e.effect_category == 5)
            .map(|e| e.effect_id)
            .unwrap_or_else(|| {
                let mut eff = sde.effects.values().next().cloned().expect("seed effects");
                eff.effect_id = 998;
                eff.name = "testOverloadEffect".into();
                eff.effect_category = 5;
                sde.effects.insert(998, eff);
                998
            });
        let mut ab_type = custom_inv_type(
            sde,
            999_501,
            46,
            7,
            5.0,
            0.0,
            "1MN Test Afterburner I",
            &[(6, 10.0), (73, 10_000.0), (20, 100.0), (1211, 2.0)],
        );
        if !ab_type.effect_ids.contains(&cat5_eid) {
            ab_type.effect_ids.push(cat5_eid);
        }
        sde.types.insert(999_501, ab_type);
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
        snap.skills.entry(char_id).or_default().insert(
            28_164,
            eve_db::TrainedSkillRecord {
                type_id: 28_164,
                level: 1,
                skill_points: 250.0,
            },
        );
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

    // Overload Afterburner while idle: pre-arms overload without activating (emits OnHeatAdded, not OnGodmaShipEffect)
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
    assert!(!notifs.iter().any(|n| n.event == "OnGodmaShipEffect"));

    // Activate Afterburner: emits OnGodmaShipEffect while remaining pre-overloaded
    let act_rep = handle_module_cycles(
        &ctx,
        &mut sess,
        None,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(ab_mod_id), PyRep::String("defaultEffect".into())],
        &kw,
    )
    .await
    .unwrap()
    .expect("activate afterburner");
    assert!(act_rep.dict_get("active").and_then(|v| v.as_bool()).unwrap_or(false) || act_rep.dict_get("effectID").is_some());
    assert!(ctx.drain_notifications_for_char(char_id).iter().any(|n| n.event == "OnGodmaShipEffect"));

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
    let pick_deterministic_small_type =
        |group_id: u32, prefer_name: &str, prefer_effect: Option<u16>| -> u32 {
            ctx.sde
                .types
                .values()
                .filter(|t| t.group_id == group_id && t.published)
                .min_by(|a, b| {
                    let eff_a = prefer_effect.is_some_and(|eid| !a.effect_ids.contains(&eid));
                    let eff_b = prefer_effect.is_some_and(|eid| !b.effect_ids.contains(&eid));
                    let name_a = !a.name.contains(prefer_name);
                    let name_b = !b.name.contains(prefer_name);
                    let cap_a = a
                        .default_attributes
                        .get(&attr_ids::CAPACITOR_NEED)
                        .copied()
                        .unwrap_or(f64::MAX);
                    let cap_b = b
                        .default_attributes
                        .get(&attr_ids::CAPACITOR_NEED)
                        .copied()
                        .unwrap_or(f64::MAX);
                    eff_a
                        .cmp(&eff_b)
                        .then_with(|| name_a.cmp(&name_b))
                        .then_with(|| cap_a.total_cmp(&cap_b))
                        .then_with(|| a.type_id.cmp(&b.type_id))
                })
                .map(|t| t.type_id)
                .expect("published module type in group")
        };
    let ab_type = pick_deterministic_small_type(46, "1MN Afterburner", Some(6731));
    let web_type = pick_deterministic_small_type(65, "Stasis Webifier", None);
    let mod_ab = 992_020_i64;
    let mod_web = 992_021_i64;
    ctx.db.with_mut_snapshot(|snap| {
        for (mid, tid, flag) in [
            (mod_ab, ab_type, eve_db::InventoryFlag::MedSlot0),
            (mod_web, web_type, eve_db::InventoryFlag::MedSlot1),
        ] {
            snap.items.insert(
                mid,
                eve_db::InventoryItemRecord {
                    item_id: mid,
                    type_id: tid,
                    owner_id: char_id,
                    location_id: ship_id,
                    flag,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
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

#[tokio::test]
async fn activate_5mn_mwd_on_battleship_uses_mass_scaled_propulsion_boost_860() {
    const SYN_RAVEN: u32 = 999_638;
    const SYN_5MN_MWD: u32 = 999_440;
    let ctx = test_ctx_with_custom(|sde| {
        let mut raven = custom_inv_type(
            sde,
            SYN_RAVEN,
            27,
            6,
            400_000.0,
            500.0,
            "Raven",
            &[
                (4, 99_100_000.0),
                (37, 112.0),
                (70, 0.115),
                (482, 5_500.0),
                (55, 950_000.0),
                (263, 8_000.0),
                (265, 5_000.0),
                (9, 6_000.0),
                (552, 420.0),
                (192, 7.0),
                (76, 95_000.0),
            ],
        );
        raven.mass = 99_100_000.0;
        sde.types.insert(SYN_RAVEN, raven);
        let mut mwd = custom_inv_type(
            sde,
            SYN_5MN_MWD,
            46,
            7,
            5.0,
            0.0,
            "5MN Microwarpdrive II",
            &[
                (6, 50.0),
                (73, 10_000.0),
                (20, 510.0),
                (567, 500_000.0),
                (796, 500_000.0),
                (554, 500.0),
            ],
        );
        mwd.effect_ids = vec![13, 6730];
        sde.types.insert(SYN_5MN_MWD, mwd);
    });

    let char_id = 97_000_860;
    let sys_id = 30_002_505;
    let ship_id = 1_000_860_001;
    let mwd_id = 1_000_860_002;
    ensure_char(&ctx, char_id, 98_000_001, sys_id, ship_id);
    insert_item(&ctx, ship_id, SYN_RAVEN, char_id, sys_id as i64, InventoryFlag::None, 1, true);
    insert_item(&ctx, mwd_id, SYN_5MN_MWD, char_id, ship_id, InventoryFlag::MedSlot0, 1, true);
    spawn_ship(&ctx, sys_id, ship_id, SYN_RAVEN, char_id, 5_500.0).await;

    let kw = HashMap::new();
    let mut sess = SessionState {
        user_id: Some(char_id as u64),
        character_id: Some(char_id),
        corporation_id: Some(98_000_001),
        solar_system_id: Some(sys_id),
        ship_id: Some(ship_id),
        ..Default::default()
    };

    handle_module_cycles(
        &ctx,
        &mut sess,
        None,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(mwd_id), PyRep::String("moduleBonusMicrowarpdrive".into())],
        &kw,
    )
    .await
    .unwrap()
    .expect("Activate 5MN MWD on Raven");

    let sys = ctx.systems.get_or_spawn(sys_id);
    let (_, ball) = sys.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    let expected_boost = eve_dogma::compute_propulsion_speed_boost(510.0, 500_000.0, 99_100_000.0, 500_000.0);
    let expected_vel = 112.0 * (1.0 + expected_boost);
    assert!(
        ball.max_velocity < 125.0 && (ball.max_velocity - expected_vel).abs() < 0.5,
        "5MN MWD on Battleship must scale by ship mass ({expected_vel:.2} m/s), not raw +510% ({} m/s)",
        ball.max_velocity
    );
}

#[tokio::test]
async fn activate_afterburner_and_matched_mwd_use_mass_scaled_propulsion_boost_860() {
    const SYN_TEMPEST: u32 = 999_639;
    const SYN_500MN_MWD: u32 = 999_441;
    const SYN_100MN_AB: u32 = 999_442;
    let ctx = test_ctx_with_custom(|sde| {
        let mut tempest = custom_inv_type(
            sde,
            SYN_TEMPEST,
            27,
            6,
            500_000.0,
            500.0,
            "Tempest",
            &[
                (4, 99_500_000.0),
                (37, 127.0),
                (70, 0.115),
                (482, 5_500.0),
                (55, 950_000.0),
                (263, 8_000.0),
                (265, 5_000.0),
                (9, 6_000.0),
                (552, 360.0),
                (192, 7.0),
                (76, 95_000.0),
            ],
        );
        tempest.mass = 99_500_000.0;
        sde.types.insert(SYN_TEMPEST, tempest);

        let mut mwd_500mn = custom_inv_type(
            sde,
            SYN_500MN_MWD,
            46,
            7,
            5.0,
            0.0,
            "500MN Microwarpdrive I",
            &[
                (6, 50.0),
                (73, 10_000.0),
                (20, 500.0),
                (567, 50_000_000.0),
                (796, 50_000_000.0),
                (554, 500.0),
            ],
        );
        mwd_500mn.effect_ids = vec![13, 6730];
        sde.types.insert(SYN_500MN_MWD, mwd_500mn);

        let mut ab_100mn = custom_inv_type(
            sde,
            SYN_100MN_AB,
            46,
            7,
            5.0,
            0.0,
            "100MN Afterburner I",
            &[
                (6, 50.0),
                (73, 10_000.0),
                (20, 135.0),
                (567, 50_000_000.0),
                (796, 50_000_000.0),
            ],
        );
        ab_100mn.effect_ids = vec![13, 6731];
        sde.types.insert(SYN_100MN_AB, ab_100mn);
    });

    let char_id = 97_000_861;
    let sys_id = 30_002_505;
    let ship_mwd_id = 1_000_861_001;
    let mwd_id = 1_000_861_002;
    ensure_char(&ctx, char_id, 98_000_001, sys_id, ship_mwd_id);
    insert_item(&ctx, ship_mwd_id, SYN_TEMPEST, char_id, sys_id as i64, InventoryFlag::None, 1, true);
    insert_item(&ctx, mwd_id, SYN_500MN_MWD, char_id, ship_mwd_id, InventoryFlag::MedSlot0, 1, true);
    spawn_ship(&ctx, sys_id, ship_mwd_id, SYN_TEMPEST, char_id, 5_500.0).await;

    let kw = HashMap::new();
    let mut sess = SessionState {
        user_id: Some(char_id as u64),
        character_id: Some(char_id),
        corporation_id: Some(98_000_001),
        solar_system_id: Some(sys_id),
        ship_id: Some(ship_mwd_id),
        ..Default::default()
    };

    handle_module_cycles(
        &ctx,
        &mut sess,
        None,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(mwd_id), PyRep::String("moduleBonusMicrowarpdrive".into())],
        &kw,
    )
    .await
    .unwrap()
    .expect("Activate matched 500MN MWD on Tempest");

    let sys = ctx.systems.get_or_spawn(sys_id);
    let (_, ball_mwd) = sys.get_ball_snapshot(ship_mwd_id).await.unwrap().unwrap();
    let expected_mwd_boost =
        eve_dogma::compute_propulsion_speed_boost(500.0, 50_000_000.0, 99_500_000.0, 50_000_000.0);
    let expected_mwd_vel = 127.0 * (1.0 + expected_mwd_boost);
    assert!(
        (ball_mwd.max_velocity - expected_mwd_vel).abs() < 0.5,
        "Matched 500MN MWD on Tempest expected ~{expected_mwd_vel:.2} m/s, got {} m/s",
        ball_mwd.max_velocity
    );

    let char_ab_id = 97_000_862;
    let ship_ab_id = 1_000_862_001;
    let ab_id = 1_000_862_002;
    ensure_char(&ctx, char_ab_id, 98_000_001, sys_id, ship_ab_id);
    insert_item(&ctx, ship_ab_id, SYN_TEMPEST, char_ab_id, sys_id as i64, InventoryFlag::None, 1, true);
    insert_item(&ctx, ab_id, SYN_100MN_AB, char_ab_id, ship_ab_id, InventoryFlag::MedSlot0, 1, true);
    spawn_ship(&ctx, sys_id, ship_ab_id, SYN_TEMPEST, char_ab_id, 5_500.0).await;

    let mut sess_ab = SessionState {
        user_id: Some(char_ab_id as u64),
        character_id: Some(char_ab_id),
        corporation_id: Some(98_000_001),
        solar_system_id: Some(sys_id),
        ship_id: Some(ship_ab_id),
        ..Default::default()
    };
    handle_module_cycles(
        &ctx,
        &mut sess_ab,
        None,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(ab_id), PyRep::String("moduleBonusAfterburner".into())],
        &kw,
    )
    .await
    .unwrap()
    .expect("Activate matched 100MN AB on Tempest");

    let (_, ball_ab) = sys.get_ball_snapshot(ship_ab_id).await.unwrap().unwrap();
    let expected_ab_boost =
        eve_dogma::compute_propulsion_speed_boost(135.0, 50_000_000.0, 99_500_000.0, 50_000_000.0);
    let expected_ab_vel = 127.0 * (1.0 + expected_ab_boost);
    assert!(
        (ball_ab.max_velocity - expected_ab_vel).abs() < 0.5,
        "Matched 100MN AB on Tempest expected ~{expected_ab_vel:.2} m/s, got {} m/s",
        ball_ab.max_velocity
    );
}

#[tokio::test]
async fn weapon_fires_first_volley_on_activate_and_rejects_duplicate_activate_890() {
    const SYN_PULSE: u32 = 999_503;
    const SYN_CRYSTAL: u32 = 999_504;
    let ctx = test_ctx_with_custom(|sde| {
        let mut turret = custom_inv_type(
            sde,
            SYN_PULSE,
            53,
            7,
            5.0,
            10.0,
            "Small Focused Pulse Laser I",
            &[
                (6, 5.0),
                (73, 4_000.0),
                (54, 10_000.0),
                (158, 5_000.0),
                (160, 1.0),
                (620, 40.0),
                (64, 2.0),
            ],
        );
        turret.effect_ids = vec![13, 10];
        sde.types.insert(SYN_PULSE, turret);
        sde.types.insert(
            SYN_CRYSTAL,
            custom_inv_type(
                sde,
                SYN_CRYSTAL,
                86,
                8,
                1.0,
                0.0,
                "Multifrequency S",
                &[(114, 50.0), (118, 50.0)],
            ),
        );
    });

    let char_id = 97_000_890;
    let target_char = 97_000_891;
    let sys_id = 30_002_505;
    let ship_id = 1_000_890_001;
    let turret_id = 1_000_890_002;
    let crystal_id = 1_000_890_003;
    let target_id = 1_000_890_010;

    ensure_char(&ctx, char_id, 98_000_001, sys_id, ship_id);
    ensure_char(&ctx, target_char, 98_000_002, sys_id, target_id);
    insert_item(&ctx, ship_id, 587, char_id, sys_id as i64, InventoryFlag::None, 1, true);
    insert_item(&ctx, turret_id, SYN_PULSE, char_id, ship_id, InventoryFlag::HiSlot0, 1, true);
    insert_item(&ctx, crystal_id, SYN_CRYSTAL, char_id, ship_id, InventoryFlag::HiSlot0, 1, true);
    insert_item(&ctx, target_id, 587, target_char, sys_id as i64, InventoryFlag::None, 1, true);

    crate::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| {
        rt.safety_levels
            .insert(char_id, crate::services::combat_crimewatch::SafetyLevel::Red);
    });

    spawn_ship(&ctx, sys_id, ship_id, 587, char_id, 1_000.0).await;
    let sys = ctx.systems.get_or_spawn(sys_id);
    sys.spawn_ball_with_meta(
        Ball {
            id: target_id,
            type_id: 587,
            position: DVec3::new(11_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 350.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
        CombatState {
            shield_hp: 500.0,
            max_shield_hp: 500.0,
            armor_hp: 500.0,
            max_armor_hp: 500.0,
            shield_recharge_time_ms: 0.0,
            capacitor_recharge_time_ms: 0.0,
            ..Default::default()
        },
        SlimItemMeta {
            group_id: 25,
            category_id: 6,
            owner_id: target_char,
            char_id: Some(target_char),
            corp_id: 98_000_002,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    sys.add_target_lock(
        ship_id,
        eve_destiny::TargetLock {
            target_id,
            lock_time_ms: 100.0,
            remaining_lock_ms: 0.0,
            locked: true,
            max_range_m: Some(50_000.0),
        },
    )
    .await
    .unwrap();

    let kw = HashMap::new();
    let mut sess = SessionState {
        user_id: Some(char_id as u64),
        character_id: Some(char_id),
        corporation_id: Some(98_000_001),
        solar_system_id: Some(sys_id),
        ship_id: Some(ship_id),
        ..Default::default()
    };

    handle_module_cycles(
        &ctx,
        &mut sess,
        None,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(turret_id), PyRep::String("targetAttack".into()), PyRep::Integer(target_id)],
        &kw,
    )
    .await
    .unwrap()
    .expect("Activate turret fires immediate first volley");

    let target_cs = sys.get_combat_state(target_id).await.unwrap().unwrap();
    assert!(
        target_cs.shield_hp < 500.0,
        "First weapon volley must apply damage immediately at Activate (t=0) without stepping (got shield={})",
        target_cs.shield_hp
    );

    let dup_err = handle_module_cycles(
        &ctx,
        &mut sess,
        None,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(turret_id), PyRep::String("targetAttack".into()), PyRep::Integer(target_id)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(dup_err.code, "ModuleAlreadyActive");
}

#[tokio::test]
async fn cloaking_device_activation_emits_zero_godma_duration_ms() {
    const SYN_CLOAK_TEST: u32 = 999_893;
    let ctx = test_ctx_with_custom(|sde| {
        let cloak_eid = sde
            .effects
            .values()
            .find(|e| e.name == "cloaking")
            .map(|e| e.effect_id)
            .unwrap_or(12);
        let mut cloak = custom_inv_type(
            sde,
            SYN_CLOAK_TEST,
            330,
            7,
            5.0,
            0.0,
            "Prototype Cloaking Device I",
            &[(6, 0.0), (51, 2000.0), (eve_sde::attr_ids::HP_STRUCT, 40.0)],
        );
        cloak.effect_ids = vec![16, cloak_eid];
        sde.types.insert(SYN_CLOAK_TEST, cloak);
    });

    let char_id = 97_000_895;
    let sys_id = 30_002_506;
    let ship_id = 1_000_895_001;
    let cloak_id = 1_000_895_002;
    ensure_char(&ctx, char_id, 98_000_001, sys_id, ship_id);
    insert_item(&ctx, ship_id, 587, char_id, sys_id as i64, InventoryFlag::None, 1, true);
    insert_item(&ctx, cloak_id, SYN_CLOAK_TEST, char_id, ship_id, InventoryFlag::HiSlot0, 1, true);
    spawn_ship(&ctx, sys_id, ship_id, 587, char_id, 1_000.0).await;

    let kw = HashMap::new();
    let mut sess = SessionState {
        user_id: Some(char_id as u64),
        character_id: Some(char_id),
        corporation_id: Some(98_000_001),
        solar_system_id: Some(sys_id),
        ship_id: Some(ship_id),
        ..Default::default()
    };
    let _ = ctx.drain_notifications_for_char(char_id);

    handle_module_cycles(
        &ctx,
        &mut sess,
        None,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(cloak_id), PyRep::String("cloaking".into())],
        &kw,
    )
    .await
    .unwrap()
    .expect("Activate cloak succeeds");

    let notifs = ctx.drain_notifications_for_char(char_id);
    let eff = notifs
        .iter()
        .find(|n| n.event == "OnGodmaShipEffect")
        .expect("OnGodmaShipEffect emitted for cloak activation");
    assert_eq!(eff.args.get(7), Some(&PyRep::Real(0.0)));
}

#[tokio::test]
async fn titan_civilian_laser_vs_concord_surface_distance_and_miss_logs_1247() {
    const SYN_AVATAR: u32 = 11_567;
    const SYN_CIV_PULSE: u32 = 3_634;
    const SYN_TIGHT_PULSE: u32 = 996_247;
    const SYN_CONCORD_SWAT: u32 = 10_035;

    let sys_id = 30_000_142_u32;
    let ctx = seeded_cycles_ctx(|sde| {
        if let Some(sys_rec) = sde.solar_systems.get_mut(&sys_id) {
            sys_rec.security_status = -0.17;
        }
        let mut avatar = sde.types.get(&587).expect("rifter").clone();
        avatar.type_id = SYN_AVATAR;
        avatar.group_id = 30;
        avatar.category_id = 6;
        avatar.name = "Avatar".into();
        avatar.radius = 6_800.0;
        avatar.mass = 2_400_000_000.0;
        avatar
            .default_attributes
            .retain(|k, _| !matches!(k, 182..=184 | 277..=279));
        avatar.default_attributes.insert(attr_ids::HI_SLOTS, 8.0);
        avatar.default_attributes.insert(attr_ids::MED_SLOTS, 6.0);
        avatar.default_attributes.insert(attr_ids::LOW_SLOTS, 6.0);
        avatar
            .default_attributes
            .insert(attr_ids::TURRET_SLOTS_LEFT, 8.0);
        avatar.default_attributes.insert(attr_ids::CPU_OUTPUT, 1_000.0);
        avatar
            .default_attributes
            .insert(attr_ids::POWER_OUTPUT, 1_000_000.0);
        avatar
            .default_attributes
            .insert(attr_ids::CAPACITOR_CAPACITY, 10_000.0);
        avatar
            .default_attributes
            .insert(attr_ids::MAX_LOCKED_TARGETS, 7.0);
        avatar
            .default_attributes
            .insert(attr_ids::MAX_TARGET_RANGE, 200_000.0);
        avatar
            .default_attributes
            .insert(attr_ids::SCAN_RESOLUTION, 100.0);
        avatar
            .default_attributes
            .insert(attr_ids::SIGNATURE_RADIUS, 16_000.0);
        sde.types.insert(SYN_AVATAR, avatar);

        let civ_attrs = [
            (6_u16, 1.0_f64),
            (51, 2_000.0),
            (54, 3_300.0),
            (64, 1.3),
            (114, 3.0),
            (116, 0.0),
            (117, 0.0),
            (118, 2.0),
            (158, 1_000.0),
            (160, 625.0),
            (620, 40_000.0),
            (1795, 0.01),
        ];
        let mut civ = custom_inv_type(
            sde,
            SYN_CIV_PULSE,
            53,
            7,
            5.0,
            0.0,
            "Civilian Gatling Pulse Laser",
            &civ_attrs,
        );
        civ.effect_ids = vec![10, 12, 16, 42];
        civ.default_effect_id = Some(10);
        sde.types.insert(SYN_CIV_PULSE, civ);

        let mut tight = custom_inv_type(
            sde,
            SYN_TIGHT_PULSE,
            53,
            7,
            5.0,
            0.0,
            "Synthetic Zero-Tracking Laser",
            &[
                (6, 1.0),
                (51, 2_000.0),
                (54, 3_300.0),
                (64, 1.3),
                (114, 3.0),
                (118, 2.0),
                (158, 1_000.0),
                (160, 0.0001),
                (620, 40_000.0),
                (1795, 0.01),
            ],
        );
        tight.effect_ids = vec![10, 12, 16, 42];
        tight.default_effect_id = Some(10);
        sde.types.insert(SYN_TIGHT_PULSE, tight);

        let mut concord_t = sde.types.get(&587).expect("rifter").clone();
        concord_t.type_id = SYN_CONCORD_SWAT;
        concord_t.group_id = 182;
        concord_t.category_id = 11;
        concord_t.name = "CONCORD SWAT Officer".into();
        concord_t.radius = 38.0;
        concord_t
            .default_attributes
            .insert(attr_ids::SIGNATURE_RADIUS, 25.0);
        sde.types.insert(SYN_CONCORD_SWAT, concord_t);
    });

    let char_id = 97_124_701_i64;
    let ship_id = 1_124_701_i64;
    let laser_hit_id = 1_124_710_i64;
    let laser_miss_id = 1_124_711_i64;
    let laser_oob_id = 1_124_712_i64;
    let concord_hit_id = 1_124_720_i64;
    let concord_miss_id = 1_124_721_i64;
    let concord_oob_id = 1_124_722_i64;

    ensure_char(&ctx, char_id, 98_000_001, sys_id, ship_id);
    let uid = ctx.db.with_snapshot(|s| s.characters[&char_id].user_id);
    let _ = ctx
        .db
        .acquire_online_lock(uid, 1, Some(char_id), false, ctx.now_unix());
    insert_item(
        &ctx,
        ship_id,
        SYN_AVATAR,
        char_id,
        sys_id as i64,
        InventoryFlag::None,
        1,
        true,
    );
    insert_item(
        &ctx,
        laser_hit_id,
        SYN_CIV_PULSE,
        char_id,
        ship_id,
        InventoryFlag::HiSlot0,
        1,
        true,
    );
    insert_item(
        &ctx,
        laser_miss_id,
        SYN_TIGHT_PULSE,
        char_id,
        ship_id,
        InventoryFlag::HiSlot1,
        1,
        true,
    );
    insert_item(
        &ctx,
        laser_oob_id,
        SYN_CIV_PULSE,
        char_id,
        ship_id,
        InventoryFlag::HiSlot2,
        1,
        true,
    );

    crate::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| {
        rt.safety_levels
            .insert(char_id, crate::services::combat_crimewatch::SafetyLevel::Red);
    });

    let sys = ctx.systems.get_or_spawn(sys_id);
    sys.spawn_ball_with_meta(
        Ball {
            id: ship_id,
            type_id: SYN_AVATAR,
            position: DVec3::ZERO,
            velocity: DVec3::ZERO,
            max_velocity: 60.0,
            inertia: 1.0,
            mass: 2_400_000_000.0,
            radius: 6_800.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
        CombatState {
            shield_hp: 100_000.0,
            max_shield_hp: 100_000.0,
            armor_hp: 100_000.0,
            max_armor_hp: 100_000.0,
            structure_hp: 100_000.0,
            max_structure_hp: 100_000.0,
            capacitor: 10_000.0,
            max_capacitor: 10_000.0,
            signature_radius: 16_000.0,
            ..CombatState::default()
        },
        SlimItemMeta {
            group_id: 30,
            category_id: 6,
            owner_id: char_id,
            char_id: Some(char_id),
            corp_id: 98_000_001,
            ..Default::default()
        },
    )
    .await
    .unwrap();

    for (tid, pos, vel, inertia) in [
        (concord_hit_id, DVec3::new(6_855.5, 0.0, 0.0), DVec3::ZERO, 1.0),
        (
            concord_miss_id,
            DVec3::new(6_855.5, 0.0, 0.0),
            DVec3::new(0.0, 200.0, 0.0),
            100.0,
        ),
        (concord_oob_id, DVec3::new(13_200.0, 0.0, 0.0), DVec3::ZERO, 1.0),
    ] {
        sys.spawn_ball_with_meta(
            Ball {
                id: tid,
                type_id: SYN_CONCORD_SWAT,
                position: pos,
                velocity: vel,
                max_velocity: 500.0,
                inertia,
                mass: 1_000_000.0,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState {
                shield_hp: 500.0,
                max_shield_hp: 500.0,
                armor_hp: 550.0,
                max_armor_hp: 550.0,
                structure_hp: 500.0,
                max_structure_hp: 500.0,
                signature_radius: 25.0,
                shield_recharge_time_ms: 0.0,
                capacitor_recharge_time_ms: 0.0,
                ..CombatState::default()
            },
            SlimItemMeta {
                group_id: 182,
                category_id: 11,
                owner_id: 1_000_125,
                char_id: None,
                corp_id: 1_000_125,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        sys.add_target_lock(
            ship_id,
            eve_destiny::TargetLock {
                target_id: tid,
                lock_time_ms: 100.0,
                remaining_lock_ms: 0.0,
                locked: true,
                max_range_m: Some(100_000.0),
            },
        )
        .await
        .unwrap();
    }

    let kw = HashMap::new();
    let mut sess = SessionState {
        user_id: Some(uid),
        character_id: Some(char_id),
        corporation_id: Some(98_000_001),
        solar_system_id: Some(sys_id),
        ship_id: Some(ship_id),
        ..Default::default()
    };
    let _ = ctx.drain_notifications_for_char(char_id);

    // 1. Hit path: Avatar (r=6800) shooting CONCORD (r=50) at 5.5m surface distance.
    handle_module_cycles(
        &ctx,
        &mut sess,
        None,
        "dogmaIM",
        "Activate",
        &[
            PyRep::Integer(laser_hit_id),
            PyRep::String("targetAttack".into()),
            PyRep::Integer(concord_hit_id),
            PyRep::Integer(1000),
        ],
        &kw,
    )
    .await
    .unwrap()
    .expect("civilian laser activation on CONCORD at 5.5m surface distance");

    // 2. Miss path: extreme transversal target emits miss log rather than failing silently.
    handle_module_cycles(
        &ctx,
        &mut sess,
        None,
        "dogmaIM",
        "Activate",
        &[
            PyRep::Integer(laser_miss_id),
            PyRep::String("targetAttack".into()),
            PyRep::Integer(concord_miss_id),
            PyRep::Integer(1000),
        ],
        &kw,
    )
    .await
    .unwrap()
    .expect("zero-tracking laser activation for deterministic miss log test");

    // 3. Out-of-range path: surface distance 6350m (> 6300m optimal+3*falloff cutoff).
    let cap_before_oob = sys.get_combat_state(ship_id).await.unwrap().unwrap().capacitor;
    let oob_err = handle_module_cycles(
        &ctx,
        &mut sess,
        None,
        "dogmaIM",
        "Activate",
        &[
            PyRep::Integer(laser_oob_id),
            PyRep::String("targetAttack".into()),
            PyRep::Integer(concord_oob_id),
            PyRep::Integer(1000),
        ],
        &kw,
    )
    .await
    .unwrap()
    .expect_err("Cycle-0 turret activation past optimal+3*falloff must return TargetOutOfRange");
    assert_eq!(oob_err.code, "TargetOutOfRange");
    assert_eq!(
        sys.get_combat_state(ship_id).await.unwrap().unwrap().capacitor,
        cap_before_oob,
        "rejected Cycle-0 turret must not consume capacitor"
    );
    assert!(
        !ctx.db.with_snapshot(|s| s.module_active_states.contains(&laser_oob_id)),
        "rejected Cycle-0 turret must not be marked active in DB"
    );

    let _ = sys.step_manual(2.1).await.unwrap();
    crate::services::inventory_fitting::execute_pending_cycle_actions(&ctx, sys_id).await;

    let hit_cs = sys.get_combat_state(concord_hit_id).await.unwrap().unwrap();
    assert!(
        hit_cs.shield_hp < 500.0,
        "CONCORD at 5.5m surface distance must take damage from Titan Civilian Pulse Laser (shield={})",
        hit_cs.shield_hp
    );
    assert!(
        sys.has_module_cycle(ship_id, laser_hit_id).await,
        "in-range repeating turret cycle must stay active"
    );

    let miss_cs = sys.get_combat_state(concord_miss_id).await.unwrap().unwrap();
    assert_eq!(
        miss_cs.shield_hp, 500.0,
        "missed fast target must stay at full shield"
    );

    let oob_cs = sys.get_combat_state(concord_oob_id).await.unwrap().unwrap();
    assert_eq!(
        oob_cs.shield_hp, 500.0,
        "out-of-range target past 6300m surface distance must stay at full shield"
    );
    assert!(
        !sys.has_module_cycle(ship_id, laser_oob_id).await,
        "out-of-range turret cycle must not remain active"
    );

    let notifs = ctx.drain_notifications_for_char(char_id);
    assert!(
        notifs.iter().any(|n| {
            n.event == "OnDamageMessage"
                && n.args.first().and_then(|d| d.dict_get("target")).and_then(|v| v.as_i64())
                    == Some(concord_hit_id)
                && n.args.first().and_then(|d| d.dict_get("damage")).and_then(|v| v.as_f64())
                    .is_some_and(|d| d > 0.0)
        }),
        "expected OnDamageMessage for CONCORD hit"
    );
    assert!(
        notifs.iter().any(|n| {
            n.event == "OnDamageMessages"
                && matches!(n.args.first(), Some(PyRep::List(items)) if items.iter().any(|entry| matches!(
                    entry,
                    PyRep::Tuple(pair) if pair.first().and_then(|v| v.as_str()) == Some("AttackHit")
                )))
        }),
        "expected AttackHit OnDamageMessages for CONCORD hit"
    );
    assert!(
        notifs.iter().any(|n| {
            n.event == "OnDamageMessage"
                && n.args.first().and_then(|d| d.dict_get("target")).and_then(|v| v.as_i64())
                    == Some(concord_miss_id)
                && n.args.first().and_then(|d| d.dict_get("damage")).and_then(|v| v.as_f64())
                    == Some(0.0)
                && n.args.first().and_then(|d| d.dict_get("hitQuality")).and_then(|v| v.as_i64())
                    == Some(0)
        }),
        "expected OnDamageMessage (damage=0.0, hitQuality=0) on turret miss"
    );
    assert!(
        notifs.iter().any(|n| {
            n.event == "OnDamageMessages"
                && matches!(n.args.first(), Some(PyRep::List(items)) if items.iter().any(|entry| matches!(
                    entry,
                    PyRep::Tuple(pair) if pair.first().and_then(|v| v.as_str()) == Some("AttackMiss")
                )))
        }),
        "expected AttackMiss OnDamageMessages on turret miss"
    );
    assert!(
        !notifs.iter().any(|n| {
            n.event == "OnDamageMessage"
                && n.args.first().and_then(|d| d.dict_get("target")).and_then(|v| v.as_i64())
                    == Some(concord_oob_id)
        }),
        "rejected Cycle-0 OOR turret must not emit OnDamageMessage or AttackMiss for target"
    );
}

#[tokio::test]
async fn cycle0_turret_oor_and_ewar_surface_distance_on_titan_1247() {
    const SYN_AVATAR: u32 = 996_250;
    const SYN_AUTOCANNON: u32 = 996_251;
    const SYN_AMMO: u32 = 996_252;
    const SYN_SCRAM: u32 = 996_253;
    const SYN_TP: u32 = 996_254;
    const SYN_TARGET_SHIP: u32 = 996_255;

    let sys_id = 30_004_759_u32;
    let ctx = seeded_cycles_ctx(|sde| {
        if let Some(sys_rec) = sde.solar_systems.get_mut(&sys_id) {
            sys_rec.security_status = 0.0;
        }
        let mut avatar = sde.types.get(&587).expect("rifter").clone();
        avatar.type_id = SYN_AVATAR;
        avatar.group_id = 30;
        avatar.category_id = 6;
        avatar.name = "Avatar".into();
        avatar.radius = 6_800.0;
        avatar.mass = 2_400_000_000.0;
        avatar
            .default_attributes
            .retain(|k, _| !matches!(k, 182..=184 | 277..=279));
        avatar.default_attributes.insert(attr_ids::HI_SLOTS, 8.0);
        avatar.default_attributes.insert(attr_ids::MED_SLOTS, 6.0);
        avatar.default_attributes.insert(attr_ids::LOW_SLOTS, 6.0);
        avatar.default_attributes.insert(attr_ids::TURRET_SLOTS_LEFT, 8.0);
        avatar.default_attributes.insert(attr_ids::CPU_OUTPUT, 1_000.0);
        avatar.default_attributes.insert(attr_ids::POWER_OUTPUT, 1_000_000.0);
        avatar.default_attributes.insert(attr_ids::CAPACITOR_CAPACITY, 10_000.0);
        avatar.default_attributes.insert(attr_ids::MAX_LOCKED_TARGETS, 7.0);
        avatar.default_attributes.insert(attr_ids::MAX_TARGET_RANGE, 200_000.0);
        avatar.default_attributes.insert(attr_ids::SCAN_RESOLUTION, 100.0);
        avatar.default_attributes.insert(attr_ids::SIGNATURE_RADIUS, 16_000.0);
        sde.types.insert(SYN_AVATAR, avatar);

        let mut ac = custom_inv_type(
            sde,
            SYN_AUTOCANNON,
            55,
            7,
            5.0,
            5.0,
            "Test Autocannon I",
            &[
                (6, 5.0),
                (51, 2_000.0),
                (54, 2_000.0),
                (56, 1.0),
                (64, 1.5),
                (128, 1.0),
                (158, 1_000.0),
                (160, 1.0),
                (604, 83.0),
                (620, 40.0),
            ],
        );
        ac.effect_ids = vec![10, 12, 16, 42];
        ac.default_effect_id = Some(10);
        sde.types.insert(SYN_AUTOCANNON, ac);

        let ammo = custom_inv_type(
            sde,
            SYN_AMMO,
            83,
            8,
            0.01,
            0.0,
            "Test Projectile S",
            &[(114, 10.0), (128, 1.0)],
        );
        sde.types.insert(SYN_AMMO, ammo);

        let mut scram = custom_inv_type(
            sde,
            SYN_SCRAM,
            52,
            7,
            5.0,
            0.0,
            "Test Warp Scrambler I",
            &[(6, 5.0), (54, 9_000.0), (73, 5_000.0), (105, 2.0)],
        );
        scram.effect_ids = vec![12, 16, 39];
        sde.types.insert(SYN_SCRAM, scram);

        let mut tp = custom_inv_type(
            sde,
            SYN_TP,
            379,
            7,
            5.0,
            0.0,
            "Test Target Painter I",
            &[(6, 5.0), (54, 30_000.0), (73, 5_000.0), (554, 30.0), (2044, 10_000.0)],
        );
        tp.effect_ids = vec![12, 16];
        sde.types.insert(SYN_TP, tp);

        let mut tgt = sde.types.get(&587).expect("rifter").clone();
        tgt.type_id = SYN_TARGET_SHIP;
        tgt.radius = 50.0;
        tgt.default_attributes.insert(attr_ids::SIGNATURE_RADIUS, 40.0);
        sde.types.insert(SYN_TARGET_SHIP, tgt);
    });

    let char_id = 97_124_702_i64;
    let vic_char_id = 97_124_703_i64;
    let ship_id = 1_124_801_i64;
    let ac_id = 1_124_810_i64;
    let ammo_id = 1_124_811_i64;
    let scram_id = 1_124_812_i64;
    let tp_id = 1_124_813_i64;
    let mid_id = 1_124_820_i64;
    let far_id = 1_124_821_i64;

    ensure_char(&ctx, char_id, 98_000_001, sys_id, ship_id);
    ensure_char(&ctx, vic_char_id, 98_000_002, sys_id, far_id);
    let uid = ctx.db.with_snapshot(|s| s.characters[&char_id].user_id);
    let _ = ctx.db.acquire_online_lock(uid, 2, Some(char_id), false, ctx.now_unix());

    insert_item(&ctx, ship_id, SYN_AVATAR, char_id, sys_id as i64, InventoryFlag::None, 1, true);
    insert_item(&ctx, ac_id, SYN_AUTOCANNON, char_id, ship_id, InventoryFlag::HiSlot0, 1, true);
    insert_item(&ctx, ammo_id, SYN_AMMO, char_id, ship_id, InventoryFlag::HiSlot0, 10, false);
    insert_item(&ctx, scram_id, SYN_SCRAM, char_id, ship_id, InventoryFlag::MedSlot0, 1, true);
    insert_item(&ctx, tp_id, SYN_TP, char_id, ship_id, InventoryFlag::MedSlot1, 1, true);

    crate::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| {
        rt.safety_levels
            .insert(char_id, crate::services::combat_crimewatch::SafetyLevel::Red);
    });

    let sys = ctx.systems.get_or_spawn(sys_id);
    sys.spawn_ball_with_meta(
        Ball {
            id: ship_id,
            type_id: SYN_AVATAR,
            position: DVec3::ZERO,
            velocity: DVec3::ZERO,
            max_velocity: 60.0,
            inertia: 1.0,
            mass: 2_400_000_000.0,
            radius: 6_800.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
        CombatState {
            shield_hp: 100_000.0,
            max_shield_hp: 100_000.0,
            armor_hp: 100_000.0,
            max_armor_hp: 100_000.0,
            structure_hp: 100_000.0,
            max_structure_hp: 100_000.0,
            capacitor: 10_000.0,
            max_capacitor: 10_000.0,
            signature_radius: 16_000.0,
            ..CombatState::default()
        },
        SlimItemMeta {
            group_id: 30,
            category_id: 6,
            owner_id: char_id,
            char_id: Some(char_id),
            corp_id: 98_000_001,
            ..Default::default()
        },
    )
    .await
    .unwrap();

    // `mid_id`: center distance = 10_850m (> 9_000m scram range), surface distance = 4_000m (<= 9_000m).
    // `far_id`: center distance = 16_350m, surface distance = 9_500m (> 5_000m turret & > 9_000m scram).
    for (tid, x, owner) in [
        (mid_id, 10_850.0, vic_char_id),
        (far_id, 16_350.0, vic_char_id),
    ] {
        sys.spawn_ball_with_meta(
            Ball {
                id: tid,
                type_id: SYN_TARGET_SHIP,
                position: DVec3::new(x, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 500.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState {
                shield_hp: 500.0,
                max_shield_hp: 500.0,
                armor_hp: 500.0,
                max_armor_hp: 500.0,
                structure_hp: 500.0,
                max_structure_hp: 500.0,
                signature_radius: 40.0,
                ..CombatState::default()
            },
            SlimItemMeta {
                group_id: 25,
                category_id: 6,
                owner_id: owner,
                char_id: Some(owner),
                corp_id: 98_000_002,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        sys.add_target_lock(
            ship_id,
            eve_destiny::TargetLock {
                target_id: tid,
                lock_time_ms: 100.0,
                remaining_lock_ms: 0.0,
                locked: true,
                max_range_m: Some(200_000.0),
            },
        )
        .await
        .unwrap();
    }

    let kw = HashMap::new();
    let mut sess = SessionState {
        user_id: Some(uid),
        character_id: Some(char_id),
        corporation_id: Some(98_000_001),
        solar_system_id: Some(sys_id),
        ship_id: Some(ship_id),
        ..Default::default()
    };
    let _ = ctx.drain_notifications_for_char(char_id);

    // 1. Cycle-0 turret activation against `far_id` (9_500m surface > 5_000m cutoff) fails cleanly before cap/ammo/weapon timer.
    let ac_oob = handle_module_cycles(
        &ctx,
        &mut sess,
        None,
        "dogmaIM",
        "Activate",
        &[
            PyRep::Integer(ac_id),
            PyRep::String("targetAttack".into()),
            PyRep::Integer(far_id),
            PyRep::Integer(1000),
        ],
        &kw,
    )
    .await
    .unwrap()
    .expect_err("Cycle-0 turret past optimal+3*falloff must return TargetOutOfRange");
    assert_eq!(ac_oob.code, "TargetOutOfRange");
    assert_eq!(sys.get_combat_state(ship_id).await.unwrap().unwrap().capacitor, 10_000.0);
    assert_eq!(ctx.db.with_snapshot(|s| s.items[&ammo_id].quantity), 10);
    assert_eq!(
        ctx.crimewatch
            .state_of(char_id)
            .map_or(0, |s| s.weapon_timer_until_unix),
        0,
        "rejected OOR Cycle-0 turret must not grant a weapon timer"
    );
    assert!(ctx.db.with_snapshot(|s| !s.sp6_lifecycle.weapon_timers_until.contains_key(&char_id)));
    assert!(!sys.has_module_cycle(ship_id, ac_id).await);
    assert!(ctx.drain_notifications_for_char(char_id).is_empty());

    // 2. Tackle (`group 52`) & Target Painter (`group 379`) use surface distance (4_000m <= 9_000m despite 10_850m center distance).
    let scram_oob = handle_module_cycles(
        &ctx,
        &mut sess,
        None,
        "dogmaIM",
        "Activate",
        &[
            PyRep::Integer(scram_id),
            PyRep::String("warpScrambleForEntity".into()),
            PyRep::Integer(far_id),
            PyRep::Integer(1000),
        ],
        &kw,
    )
    .await
    .unwrap()
    .expect_err("Warp Scrambler past 9_000m surface distance must fail TargetOutOfRange");
    assert_eq!(scram_oob.code, "TargetOutOfRange");

    handle_module_cycles(
        &ctx,
        &mut sess,
        None,
        "dogmaIM",
        "Activate",
        &[
            PyRep::Integer(scram_id),
            PyRep::String("warpScrambleForEntity".into()),
            PyRep::Integer(mid_id),
            PyRep::Integer(1000),
        ],
        &kw,
    )
    .await
    .unwrap()
    .expect("Warp Scrambler at 4_000m surface distance (10_850m center) must activate on Titan");
    assert!(sys.has_module_cycle(ship_id, scram_id).await);

    handle_module_cycles(
        &ctx,
        &mut sess,
        None,
        "dogmaIM",
        "Activate",
        &[
            PyRep::Integer(tp_id),
            PyRep::String("targetPassively".into()),
            PyRep::Integer(mid_id),
            PyRep::Integer(1000),
        ],
        &kw,
    )
    .await
    .unwrap()
    .expect("Target Painter at 4_000m surface distance must activate on Titan");
    let tp_dist = ctx
        .projected_ewar
        .read()
        .unwrap()
        .get(&mid_id)
        .and_then(|v| v.iter().find(|e| e.source_module_id == tp_id).map(|e| e.distance))
        .expect("Target Painter projected EWAR recorded on target");
    assert!(
        (tp_dist - 4_000.0).abs() < 1e-6,
        "Target Painter projected EWAR must store 4_000m surface distance, got {tp_dist}"
    );
}

#[tokio::test]
async fn cycle0_unguided_missile_surface_distance_and_eff_range_1247() {
    const SYN_AVATAR: u32 = 996_260;
    const SYN_DEFENDER: u32 = 996_261;
    const SYN_DEF_CHARGE: u32 = 996_262;
    const SYN_TARGET_SHIP: u32 = 996_263;

    let sys_id = 30_004_759_u32;
    let ctx = seeded_cycles_ctx(|sde| {
        if let Some(sys_rec) = sde.solar_systems.get_mut(&sys_id) {
            sys_rec.security_status = 0.0;
        }
        let mut avatar = sde.types.get(&587).expect("rifter").clone();
        avatar.type_id = SYN_AVATAR;
        avatar.group_id = 30;
        avatar.category_id = 6;
        avatar.name = "Avatar".into();
        avatar.radius = 6_800.0;
        avatar.mass = 2_400_000_000.0;
        avatar
            .default_attributes
            .retain(|k, _| !matches!(k, 182..=184 | 277..=279));
        avatar.default_attributes.insert(attr_ids::HI_SLOTS, 8.0);
        avatar.default_attributes.insert(attr_ids::CPU_OUTPUT, 1_000.0);
        avatar.default_attributes.insert(attr_ids::POWER_OUTPUT, 1_000_000.0);
        avatar.default_attributes.insert(attr_ids::CAPACITOR_CAPACITY, 10_000.0);
        avatar.default_attributes.insert(attr_ids::MAX_LOCKED_TARGETS, 7.0);
        avatar.default_attributes.insert(attr_ids::MAX_TARGET_RANGE, 200_000.0);
        avatar.default_attributes.insert(attr_ids::SCAN_RESOLUTION, 100.0);
        avatar.default_attributes.insert(attr_ids::SIGNATURE_RADIUS, 16_000.0);
        sde.types.insert(SYN_AVATAR, avatar);

        // Group 512 (`is_missile_group = true`, `is_guided_missile = false`).
        // Give falloff=3_000 so unpatched `else if opt_range > 0` would compute 14_000m instead of weapon_optimal_m=5_000m.
        let mut def_launcher = custom_inv_type(
            sde,
            SYN_DEFENDER,
            512,
            7,
            5.0,
            5.0,
            "Test Defender Launcher I",
            &[(6, 5.0), (51, 2_000.0), (54, 5_000.0), (158, 3_000.0), (128, 1.0), (604, 83.0)],
        );
        def_launcher.effect_ids = vec![101, 12, 16];
        def_launcher.default_effect_id = Some(101);
        sde.types.insert(SYN_DEFENDER, def_launcher);

        let def_charge = custom_inv_type(
            sde,
            SYN_DEF_CHARGE,
            83,
            8,
            0.01,
            0.0,
            "Test Defender Charge",
            &[(37, 2_000.0), (281, 2_500.0), (114, 50.0)],
        );
        sde.types.insert(SYN_DEF_CHARGE, def_charge);

        let mut tgt = sde.types.get(&587).expect("rifter").clone();
        tgt.type_id = SYN_TARGET_SHIP;
        tgt.radius = 50.0;
        tgt.default_attributes.insert(attr_ids::SIGNATURE_RADIUS, 40.0);
        sde.types.insert(SYN_TARGET_SHIP, tgt);
    });

    let char_id = 97_124_704_i64;
    let vic_char_id = 97_124_705_i64;
    let ship_id = 1_124_901_i64;
    let def_id = 1_124_910_i64;
    let charge_id = 1_124_911_i64;
    let mid_id = 1_124_920_i64;
    let far_id = 1_124_921_i64;

    ensure_char(&ctx, char_id, 98_000_003, sys_id, ship_id);
    ensure_char(&ctx, vic_char_id, 98_000_004, sys_id, far_id);
    let uid = ctx.db.with_snapshot(|s| s.characters[&char_id].user_id);
    let _ = ctx.db.acquire_online_lock(uid, 2, Some(char_id), false, ctx.now_unix());

    insert_item(&ctx, ship_id, SYN_AVATAR, char_id, sys_id as i64, InventoryFlag::None, 1, true);
    insert_item(&ctx, def_id, SYN_DEFENDER, char_id, ship_id, InventoryFlag::HiSlot0, 1, true);
    insert_item(&ctx, charge_id, SYN_DEF_CHARGE, char_id, ship_id, InventoryFlag::HiSlot0, 10, false);

    crate::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| {
        rt.safety_levels
            .insert(char_id, crate::services::combat_crimewatch::SafetyLevel::Red);
    });

    let sys = ctx.systems.get_or_spawn(sys_id);
    sys.spawn_ball_with_meta(
        Ball {
            id: ship_id,
            type_id: SYN_AVATAR,
            position: DVec3::ZERO,
            velocity: DVec3::ZERO,
            max_velocity: 60.0,
            inertia: 1.0,
            mass: 2_400_000_000.0,
            radius: 6_800.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
        CombatState {
            shield_hp: 100_000.0,
            max_shield_hp: 100_000.0,
            armor_hp: 100_000.0,
            max_armor_hp: 100_000.0,
            structure_hp: 100_000.0,
            max_structure_hp: 100_000.0,
            capacitor: 10_000.0,
            max_capacitor: 10_000.0,
            signature_radius: 16_000.0,
            ..CombatState::default()
        },
        SlimItemMeta {
            group_id: 30,
            category_id: 6,
            owner_id: char_id,
            char_id: Some(char_id),
            corp_id: 98_000_003,
            ..Default::default()
        },
    )
    .await
    .unwrap();

    // `mid_id`: center = 10_850m (> 5_000m optimal), surface = 4_000m (<= 5_000m).
    // `far_id`: center = 16_350m, surface = 9_500m (> 5_000m optimal, even though < 14_000m opt+3*falloff).
    for (tid, x) in [(mid_id, 10_850.0), (far_id, 16_350.0)] {
        sys.spawn_ball_with_meta(
            Ball {
                id: tid,
                type_id: SYN_TARGET_SHIP,
                position: DVec3::new(x, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 500.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState {
                shield_hp: 500.0,
                max_shield_hp: 500.0,
                armor_hp: 500.0,
                max_armor_hp: 500.0,
                structure_hp: 500.0,
                max_structure_hp: 500.0,
                signature_radius: 40.0,
                ..CombatState::default()
            },
            SlimItemMeta {
                group_id: 25,
                category_id: 6,
                owner_id: vic_char_id,
                char_id: Some(vic_char_id),
                corp_id: 98_000_004,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        sys.add_target_lock(
            ship_id,
            eve_destiny::TargetLock {
                target_id: tid,
                lock_time_ms: 100.0,
                remaining_lock_ms: 0.0,
                locked: true,
                max_range_m: Some(200_000.0),
            },
        )
        .await
        .unwrap();
    }

    let kw = HashMap::new();
    let mut sess = SessionState {
        user_id: Some(uid),
        character_id: Some(char_id),
        corporation_id: Some(98_000_003),
        solar_system_id: Some(sys_id),
        ship_id: Some(ship_id),
        ..Default::default()
    };
    let _ = ctx.drain_notifications_for_char(char_id);

    // 1. Cycle-0 activation against `far_id` (9_500m surface > 5_000m optimal) rejects TargetOutOfRange before cap/ammo/timer commit.
    let far_err = handle_module_cycles(
        &ctx,
        &mut sess,
        None,
        "dogmaIM",
        "Activate",
        &[
            PyRep::Integer(def_id),
            PyRep::String("useMissiles".into()),
            PyRep::Integer(far_id),
            PyRep::Integer(1000),
        ],
        &kw,
    )
    .await
    .unwrap()
    .expect_err("Cycle-0 unguided missile past weapon_optimal_m must fail TargetOutOfRange");
    assert_eq!(far_err.code, "TargetOutOfRange");
    assert_eq!(sys.get_combat_state(ship_id).await.unwrap().unwrap().capacitor, 10_000.0);
    assert_eq!(ctx.db.with_snapshot(|s| s.items[&charge_id].quantity), 10);
    assert!(!sys.has_module_cycle(ship_id, def_id).await);
    assert!(ctx.drain_notifications_for_char(char_id).is_empty());

    // 2. Cycle-0 against `mid_id` (4_000m surface <= 5_000m despite 10_850m center) activates; stepping past 5_000m surface cancels via `max_effective_range_m == 5_000m`.
    handle_module_cycles(
        &ctx,
        &mut sess,
        None,
        "dogmaIM",
        "Activate",
        &[
            PyRep::Integer(def_id),
            PyRep::String("useMissiles".into()),
            PyRep::Integer(mid_id),
            PyRep::Integer(1000),
        ],
        &kw,
    )
    .await
    .unwrap()
    .expect("unguided missile within 5_000m surface range must activate on Titan");
    assert!(sys.has_module_cycle(ship_id, def_id).await);
    let shield_after_cycle0_hit = sys.get_combat_state(mid_id).await.unwrap().unwrap().shield_hp;
    assert!(
        shield_after_cycle0_hit < 500.0,
        "Cycle-0 unguided missile at 4_000m surface distance (10_850m center) must hit Titan"
    );

    // Move `mid_id` to x = 12_850m (surface = 6_000m > 5_000m, still < 14_000m): cycle cancels before firing with 0 additional damage and 0 miss logs.
    assert!(sys.set_ball_position(mid_id, DVec3::new(12_850.0, 0.0, 0.0)).await);
    let _ = ctx.drain_notifications_for_char(char_id);
    let _ = sys.step_manual(2.1).await.unwrap();
    crate::services::inventory_fitting::execute_pending_cycle_actions(&ctx, sys_id).await;
    assert!(
        !sys.has_module_cycle(ship_id, def_id).await,
        "cycle with max_effective_range_m=5_000m must cancel once target leaves 5_000m surface distance"
    );
    let shield_after_step = sys.get_combat_state(mid_id).await.unwrap().unwrap().shield_hp;
    assert!(
        shield_after_step >= shield_after_cycle0_hit,
        "cancelled OOR missile cycle must not deal additional damage (before={shield_after_cycle0_hit}, after={shield_after_step})"
    );
    assert!(
        !ctx.drain_notifications_for_char(char_id)
            .iter()
            .any(|n| n.event == "OnDamageMessage" || n.event == "OnDamageMessages"),
        "cancelled OOR missile cycle must not emit hit or miss logs"
    );
}

#[tokio::test]
async fn handle_module_cycles_overload_thermo_cat5_delay_and_preoverload_guards() {
    let ctx = seeded_cycles_ctx(|sde| {
        let mut eff = sde.effects.values().next().expect("seed effect").clone();
        eff.effect_id = 65_001;
        eff.name = "overloadTestEffect".into();
        eff.effect_category = 5;
        sde.effects.insert(65_001, eff);

        let mut ship_t = custom_inv_type(
            sde,
            999_600,
            25,
            6,
            1_000.0,
            100.0,
            "Test Frig",
            &[
                (11, 10_000.0),
                (12, 4.0),
                (13, 4.0),
                (14, 4.0),
                (48, 10_000.0),
                (55, 1_000.0),
                (482, 500.0),
            ],
        );
        ship_t.effect_ids = vec![16];
        sde.types.insert(999_600, ship_t);

        let mut cat5_ab = custom_inv_type(
            sde,
            999_601,
            46,
            7,
            5.0,
            0.0,
            "1MN Overloadable AB",
            &[(6, 50.0), (9, 40.0), (73, 5_000.0), (1212, 1.0)],
        );
        cat5_ab.effect_ids = vec![13, 65_001];
        sde.types.insert(999_601, cat5_ab);

        let mut plain_ab = custom_inv_type(
            sde,
            999_602,
            60,
            7,
            5.0,
            0.0,
            "Plain Damage Control",
            &[(6, 10.0), (9, 40.0), (73, 5_000.0)],
        );
        plain_ab.effect_ids = vec![11];
        sde.types.insert(999_602, plain_ab);
    });

    let char_id = 90_000_001_i64;
    let ship_id = 880_100_001_i64;
    let sys_id = 30_000_142_u32;
    let ab_cat5 = 880_100_010_i64;
    let ab_plain = 880_100_011_i64;

    insert_item(&ctx, ship_id, 999_600, char_id, sys_id as i64, InventoryFlag::None, 1, true);
    insert_item(&ctx, ab_cat5, 999_601, char_id, ship_id, InventoryFlag::MedSlot0, 1, true);
    insert_item(&ctx, ab_plain, 999_602, char_id, ship_id, InventoryFlag::LoSlot0, 1, true);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.structure_id = None;
            ch.solar_system_id = sys_id;
            ch.active_ship_id = ship_id;
        }
        snap.module_online_states.insert(ab_cat5, true);
        snap.module_online_states.insert(ab_plain, true);
        if let Some(sk) = snap.skills.get_mut(&char_id) {
            sk.remove(&28_164);
        }
    });
    ctx.invalidate_ship_fitting(ship_id);
    spawn_ship(&ctx, sys_id, ship_id, 999_600, char_id, 500.0).await;
    let sys = ctx.systems.get_or_spawn(sys_id);

    let mut sess = SessionState::default();
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(1_000_009);
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(ship_id);
    let kw = HashMap::new();

    // 1. Non-Cat5 module rejects Overload with CannotOverloadModule
    assert_eq!(
        handle_module_cycles(&ctx, &mut sess, None, "dogmaIM", "Overload", &[PyRep::Integer(ab_plain)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "CannotOverloadModule"
    );

    // 2. Cat5 module without Thermodynamics rejects Overload cleanly without cap/state mutation
    assert_eq!(
        handle_module_cycles(&ctx, &mut sess, None, "dogmaIM", "Overload", &[PyRep::Integer(ab_cat5)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "DontHaveThermoDynamicsSkillz"
    );
    assert!((sys.get_combat_state(ship_id).await.unwrap().unwrap().capacitor - 500.0).abs() < 1e-6);
    assert!(!ctx.db.with_snapshot(|s| {
        s.module_active_states.contains(&ab_cat5)
            || s.ship_fit_inputs
                .get(&ship_id)
                .is_some_and(|fi| fi.overloaded_modules.contains(&ab_cat5))
    }));

    // Grant Thermodynamics L1
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(char_id).or_default().insert(
            28_164,
            eve_db::TrainedSkillRecord {
                type_id: 28_164,
                level: 1,
                skill_points: 250.0,
            },
        );
    });

    // 3. Pre-overloaded idle module preserves ModuleState::Overloaded across failed Activate
    let fit_before = ctx.get_or_rebuild_ship_fitting(char_id, ship_id).unwrap();
    let _ = crate::services::fitting_cycles_overheat::handle_active_overload_toggle(
        &ctx, &sys, char_id, ship_id, ab_cat5, fit_before, false,
    )
    .await;
    sys.set_capacitor(ship_id, 5.0).await.unwrap();
    let cap_err = handle_module_cycles(&ctx, &mut sess, None, "dogmaIM", "Activate", &[PyRep::Integer(ab_cat5)], &kw)
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(cap_err.code, "NotEnoughCapacitor");
    assert_eq!(
        ctx.get_or_rebuild_ship_fitting(char_id, ship_id).unwrap().modules[&ab_cat5].state,
        eve_dogma::ModuleState::Overloaded,
        "Failed Activate in handle_module_cycles must preserve ModuleState::Overloaded"
    );
    assert!(ctx.db.with_snapshot(|s| {
        s.ship_fit_inputs
            .get(&ship_id)
            .is_some_and(|fi| fi.overloaded_modules.contains(&ab_cat5))
    }));

    // 4. In reactivation delay (!in_cycle): Activate -> ModuleReactivationDelay, Overload -> ModuleAlreadyActive
    sys.set_capacitor(ship_id, 500.0).await.unwrap();
    ctx.record_module_reactivation_delay(ship_id, ab_cat5, 10.0);
    assert_eq!(
        handle_module_cycles(&ctx, &mut sess, None, "dogmaIM", "Activate", &[PyRep::Integer(ab_cat5)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "ModuleReactivationDelay"
    );
    assert_eq!(
        handle_module_cycles(&ctx, &mut sess, None, "dogmaIM", "Overload", &[PyRep::Integer(ab_cat5)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "ModuleAlreadyActive"
    );
    ctx.module_reactivation_cooldowns.write().unwrap().clear();

    // 5. Burned-out module returns ModuleBurnedOut before ModuleOffline
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(ab_cat5, false);
        snap.items
            .get_mut(&ab_cat5)
            .unwrap()
            .dynamic_attributes
            .insert(eve_sde::attr_ids::DAMAGE, 40.0);
    });
    ctx.invalidate_ship_fitting(ship_id);
    assert_eq!(
        handle_module_cycles(&ctx, &mut sess, None, "dogmaIM", "Overload", &[PyRep::Integer(ab_cat5)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "ModuleBurnedOut"
    );
}

#[tokio::test]
async fn mercden_remote_shield_repair_overload_persists_heat_and_guards() {
    const SYN_RSB_G41: u32 = 996_201;
    const SYN_RSB_NO_CAT5: u32 = 996_202;

    let ctx = seeded_cycles_ctx(|sde| {
        if let Some(rifter) = sde.types.get_mut(&587) {
            rifter.default_attributes.insert(eve_sde::attr_ids::HI_SLOTS, 6.0);
            rifter.default_attributes.insert(eve_sde::attr_ids::MED_SLOTS, 4.0);
        }
        let cat5_eid = sde
            .effects
            .values()
            .find(|e| e.effect_category == 5)
            .map(|e| e.effect_id)
            .unwrap_or_else(|| {
                let mut eff = sde.effects.values().next().cloned().expect("seed effects");
                eff.effect_id = 998;
                eff.name = "testOverloadEffect".into();
                eff.effect_category = 5;
                sde.effects.insert(998, eff);
                998
            });
        let mut rsb_t = custom_inv_type(
            sde,
            SYN_RSB_G41,
            41,
            7,
            5.0,
            0.0,
            "Small Remote Shield Booster I",
            &[(6, 20.0), (54, 15_000.0), (68, 1_500.0), (73, 5_000.0), (9, 40.0)],
        );
        rsb_t.effect_ids = vec![12, 16, cat5_eid];
        sde.types.insert(SYN_RSB_G41, rsb_t);

        let mut no_cat5 = custom_inv_type(
            sde,
            SYN_RSB_NO_CAT5,
            41,
            7,
            5.0,
            0.0,
            "Non-Overloadable Remote Shield Booster",
            &[(6, 20.0), (54, 15_000.0), (68, 1_500.0), (73, 5_000.0), (9, 40.0)],
        );
        no_cat5.effect_ids = vec![12, 16];
        sde.types.insert(SYN_RSB_NO_CAT5, no_cat5);
    });

    let char_id = 93_200_001_i64;
    let corp_id = 98_000_001_i64;
    let sys_id = 30_000_142_u32;
    let ship_id = 1_000_500_001_i64;
    let rsb_id = 1_000_500_002_i64;
    let rsb_no_overload_id = 1_000_500_003_i64;
    let den_id = 1_000_500_010_i64;

    ensure_char(&ctx, char_id, corp_id, sys_id, ship_id);
    insert_item(&ctx, ship_id, 587, char_id, sys_id as i64, InventoryFlag::None, 1, true);
    insert_item(&ctx, rsb_id, SYN_RSB_G41, char_id, ship_id, InventoryFlag::HiSlot0, 1, true);
    insert_item(&ctx, rsb_no_overload_id, SYN_RSB_NO_CAT5, char_id, ship_id, InventoryFlag::HiSlot1, 1, true);
    spawn_ship(&ctx, sys_id, ship_id, 587, char_id, 500.0).await;

    let sys = ctx.systems.get_or_spawn(sys_id);
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            den_id,
            eve_db::UpwellStructureRecord {
                structure_id: den_id,
                type_id: 85_230,
                owner_corp_id: corp_id,
                solar_system_id: sys_id,
                name: "TestMercDen".into(),
                state: "ShieldVulnerable".into(),
                position: [12_000.0, 0.0, 0.0],
                shield_hp: 500.0,
                armor_hp: 5_000.0,
                hull_hp: 5_000.0,
                quantum_core_installed: true,
                ..Default::default()
            },
        );
        snap.sov_runtime.mercenary_dens.insert(
            den_id,
            eve_db::durable_domain_types::PersistedMercenaryDen {
                den_id,
                type_id: 85_230,
                owner_char_id: char_id,
                skyhook_id: 1,
                solar_system_id: sys_id,
                planet_id: 40_009_001,
                enabled: false,
                ..Default::default()
            },
        );
        snap.sov_runtime.mercenary_den_reinforcement.insert(den_id, (0, 0, 10));
    });
    sys.spawn_ball_with_combat(
        Ball {
            id: den_id,
            type_id: 85_230,
            position: DVec3::new(12_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 500.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
        CombatState {
            shield_hp: 500.0,
            max_shield_hp: 5_000.0,
            armor_hp: 5_000.0,
            max_armor_hp: 5_000.0,
            structure_hp: 5_000.0,
            max_structure_hp: 5_000.0,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    sys.add_target_lock(
        ship_id,
        eve_destiny::TargetLock {
            target_id: den_id,
            lock_time_ms: 0.0,
            remaining_lock_ms: 0.0,
            locked: true,
            max_range_m: Some(100_000.0),
        },
    )
    .await
    .unwrap();

    let mut sess = SessionState {
        user_id: Some(char_id as u64),
        character_id: Some(char_id),
        corporation_id: Some(corp_id),
        solar_system_id: Some(sys_id),
        ship_id: Some(ship_id),
        ..Default::default()
    };
    let kw = HashMap::new();

    // 1. Pre-overload guards still reject before MercDen repair (missing Thermodynamics, non-overloadable module, burned-out module)
    assert_eq!(
        handle_module_cycles(
            &ctx,
            &mut sess,
            None,
            "dogmaIM",
            "Overload",
            &[PyRep::Integer(rsb_id), PyRep::String("defaultEffect".into()), PyRep::Integer(den_id)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "DontHaveThermoDynamicsSkillz"
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(char_id).or_default().insert(
            28_164,
            eve_db::TrainedSkillRecord {
                type_id: 28_164,
                level: 1,
                skill_points: 250.0,
            },
        );
    });

    assert_eq!(
        handle_module_cycles(
            &ctx,
            &mut sess,
            None,
            "dogmaIM",
            "Overload",
            &[
                PyRep::Integer(rsb_no_overload_id),
                PyRep::String("defaultEffect".into()),
                PyRep::Integer(den_id),
            ],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "CannotOverloadModule"
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.items
            .get_mut(&rsb_id)
            .unwrap()
            .dynamic_attributes
            .insert(eve_sde::attr_ids::DAMAGE, 40.0);
    });
    ctx.invalidate_ship_fitting(ship_id);
    assert_eq!(
        handle_module_cycles(
            &ctx,
            &mut sess,
            None,
            "dogmaIM",
            "Overload",
            &[PyRep::Integer(rsb_id), PyRep::String("defaultEffect".into()), PyRep::Integer(den_id)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "ModuleBurnedOut"
    );
    assert_eq!(
        ctx.db.with_snapshot(|s| s.sov_runtime.mercenary_den_reinforcement[&den_id].2),
        10
    );

    // Clear burnout and verify Overload MercDen repair persists ModuleState::Overloaded & emits OnHeatAdded
    ctx.db.with_mut_snapshot(|snap| {
        snap.items
            .get_mut(&rsb_id)
            .unwrap()
            .dynamic_attributes
            .remove(&eve_sde::attr_ids::DAMAGE);
    });
    ctx.invalidate_ship_fitting(ship_id);
    let _ = ctx.drain_notifications_for_char(char_id);

    let ov_rep = handle_module_cycles(
        &ctx,
        &mut sess,
        None,
        "dogmaIM",
        "Overload",
        &[PyRep::Integer(rsb_id), PyRep::String("defaultEffect".into()), PyRep::Integer(den_id)],
        &kw,
    )
    .await
    .unwrap()
    .expect("MercDen Overload repair");
    let pct_1 = ov_rep.dict_get("shieldPct").and_then(|v| v.as_i64()).expect("shieldPct");
    assert!(pct_1 > 10, "expected shieldPct > 10 after repair, got {pct_1}");
    assert!(ctx.db.with_snapshot(|s| {
        s.ship_fit_inputs
            .get(&ship_id)
            .is_some_and(|fi| fi.overloaded_modules.contains(&rsb_id))
    }));
    assert_eq!(
        ctx.get_or_rebuild_ship_fitting(char_id, ship_id).unwrap().modules[&rsb_id].state,
        eve_dogma::ModuleState::Overloaded
    );
    assert!(
        ctx.drain_notifications_for_char(char_id).iter().any(|n| {
            n.event == "OnHeatAdded"
                && n.args == vec![PyRep::Integer(1175), PyRep::Integer(rsb_id)]
        }),
        "expected OnHeatAdded([1175, rsb_id]) on overloaded MercDen repair"
    );

    // Pre-overloaded Activate against MercDen also preserves Overloaded state and emits OnHeatAdded
    let _ = ctx.drain_notifications_for_char(char_id);
    let act_rep = handle_module_cycles(
        &ctx,
        &mut sess,
        None,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(rsb_id), PyRep::String("defaultEffect".into()), PyRep::Integer(den_id)],
        &kw,
    )
    .await
    .unwrap()
    .expect("Pre-overloaded MercDen Activate repair");
    let pct_2 = act_rep.dict_get("shieldPct").and_then(|v| v.as_i64()).expect("shieldPct");
    assert!(pct_2 > pct_1, "expected shieldPct > {pct_1} after second repair, got {pct_2}");
    assert!(ctx.db.with_snapshot(|s| {
        s.ship_fit_inputs
            .get(&ship_id)
            .is_some_and(|fi| fi.overloaded_modules.contains(&rsb_id))
    }));
    assert_eq!(
        ctx.get_or_rebuild_ship_fitting(char_id, ship_id).unwrap().modules[&rsb_id].state,
        eve_dogma::ModuleState::Overloaded
    );
    assert!(
        ctx.drain_notifications_for_char(char_id).iter().any(|n| {
            n.event == "OnHeatAdded"
                && n.args == vec![PyRep::Integer(1175), PyRep::Integer(rsb_id)]
        }),
        "expected OnHeatAdded([1175, rsb_id]) on pre-overloaded MercDen Activate"
    );
}

#[tokio::test]
async fn handle_module_cycles_overload_rejects_docked_and_preserves_invulnerability() {
    let ab_tid = 990_439_u32;
    let ctx = test_ctx_with_custom(|sde| {
        sde.effects.entry(3002).or_insert_with(|| eve_sde::DogmaEffectType {
            effect_id: 3002,
            name: "overloadSelfSpeedFactorBonus".into(),
            effect_category: 5,
            published: true,
            ..Default::default()
        });
        let mut ab = custom_inv_type(
            sde,
            ab_tid,
            46,
            7,
            5.0,
            0.0,
            "Test Overload AB",
            &[(1212, 1.0), (54, 10_000.0), (6, 10.0)],
        );
        ab.effect_ids = vec![16, 3002];
        sde.types.insert(ab_tid, ab);
    });

    let sys_id = 30_000_142_u32;
    let char_id = 911_055_i64;
    let ship_id = 912_055_i64;
    let ab_id = 913_055_i64;
    ensure_char(&ctx, char_id, 1_000_009, sys_id, ship_id);
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(char_id).or_default().insert(
            28_164,
            eve_db::TrainedSkillRecord { type_id: 28_164, level: 5, skill_points: 256_000.0 },
        );
        for (iid, tid, flag) in [
            (ship_id, 587_u32, InventoryFlag::None),
            (ab_id, ab_tid, InventoryFlag::MedSlot0),
        ] {
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: tid,
                    owner_id: char_id,
                    location_id: if iid == ship_id { sys_id as i64 } else { ship_id },
                    flag,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
        }
    });
    ctx.invalidate_ship_fitting(ship_id);

    let kw = HashMap::new();
    let mut sess = SessionState::new(char_id as u64);
    sess.user_id = Some(char_id as u64);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(1_000_009);
    sess.ship_id = Some(ship_id);

    for (station, structure, sys_opt, label) in [
        (Some(60_003_760), None, Some(sys_id), "station with solar_system_id"),
        (Some(60_003_760), None, None, "station without solar_system_id"),
        (None, Some(1_000_000_777), Some(sys_id), "structure with solar_system_id"),
    ] {
        sess.station_id = station;
        sess.structure_id = structure;
        sess.solar_system_id = sys_opt;
        let err = handle_module_cycles(
            &ctx,
            &mut sess,
            None,
            "dogmaIM",
            "Overload",
            &[PyRep::Integer(ab_id), PyRep::Integer(3002)],
            &kw,
        )
        .await
        .expect("Overload must handle docked check")
        .unwrap_err();
        assert_eq!(err.code, "ShipDocked", "{label} must return ShipDocked");
        assert!(
            ctx.db.with_snapshot(|s| {
                s.ship_fit_inputs
                    .get(&ship_id)
                    .is_none_or(|fi| !fi.overloaded_modules.contains(&ab_id))
            }),
            "{label} must not persist overload state"
        );
    }

    // Undocked in space preserves active ship protection (undock + warp-exit timers)
    sess.station_id = None;
    sess.structure_id = None;
    sess.solar_system_id = Some(sys_id);
    let sys = ctx.systems.get_or_spawn(sys_id);
    let mut cs = ctx.build_ship_combat_state(char_id, ship_id, 587);
    cs.capacitor = 1_000.0;
    cs.max_capacitor = 1_000.0;
    cs.undock_invulnerable_remaining_ticks = 60;
    cs.warp_exit_invulnerable_remaining_ticks = 45;
    sys.spawn_ball_with_meta(
        Ball {
            id: ship_id,
            type_id: 587,
            position: DVec3::ZERO,
            velocity: DVec3::ZERO,
            max_velocity: 365.0,
            inertia: 1.0,
            mass: 1_067_000.0,
            radius: 37.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
        cs,
        ctx.build_ship_slim_meta(char_id, 587),
    )
    .await
    .unwrap();

    let ok = handle_module_cycles(
        &ctx,
        &mut sess,
        None,
        "dogmaIM",
        "Overload",
        &[PyRep::Integer(ab_id)],
        &kw,
    )
    .await
    .expect("Overload in space handled")
    .expect("Overload in space succeeds");
    assert_eq!(ok.dict_get("overloaded"), Some(&PyRep::Bool(true)));
    let cs_after = sys.get_combat_state(ship_id).await.unwrap().unwrap();
    assert_eq!(
        cs_after.undock_invulnerable_remaining_ticks, 60,
        "Pre-overload in handle_module_cycles must preserve undock invulnerability"
    );
    assert_eq!(
        cs_after.warp_exit_invulnerable_remaining_ticks, 45,
        "Pre-overload in handle_module_cycles must preserve warp-exit invulnerability"
    );
}

#[tokio::test]
async fn activate_online_cloak_targetless_weapon_and_deadspace_mwd_guards_zero_mutation() {
    let turret_tid = 998_053_u32;
    let sb_tid = 998_077_u32;
    let ab_tid = 998_046_u32;
    let mwd_tid = 998_146_u32;
    let launcher_tid = 998_507_u32;
    let std_missile_tid = 998_384_u32;
    let fof_missile_tid = 998_395_u32;

    let ctx = test_ctx_with_custom(|sde| {
        let mut turret = custom_inv_type(
            sde,
            turret_tid,
            53,
            7,
            5.0,
            0.0,
            "Test Laser I",
            &[(51, 2_000.0), (54, 10_000.0), (6, 10.0), (50, 1.0), (30, 1.0)],
        );
        turret.effect_ids = vec![10, 12, 16];
        sde.types.insert(turret_tid, turret);

        let mut sb = custom_inv_type(
            sde,
            sb_tid,
            77,
            7,
            5.0,
            0.0,
            "Small Test Shield Booster I",
            &[(6, 10.0), (73, 10_000.0), (50, 1.0), (30, 1.0)],
        );
        sb.effect_ids = vec![13, 16];
        sde.types.insert(sb_tid, sb);

        let mut ab = custom_inv_type(
            sde,
            ab_tid,
            46,
            7,
            5.0,
            0.0,
            "1MN Test Afterburner I",
            &[(6, 10.0), (73, 10_000.0), (20, 100.0), (50, 1.0), (30, 1.0)],
        );
        ab.effect_ids = vec![13, 16];
        sde.types.insert(ab_tid, ab);

        let mut mwd = custom_inv_type(
            sde,
            mwd_tid,
            46,
            7,
            5.0,
            0.0,
            "5MN Test Microwarpdrive I",
            &[(6, 10.0), (73, 10_000.0), (20, 100.0), (554, 500.0), (50, 1.0), (30, 1.0)],
        );
        mwd.effect_ids = vec![13, 16, 6730];
        sde.types.insert(mwd_tid, mwd);

        let mut launcher = custom_inv_type(
            sde,
            launcher_tid,
            507,
            7,
            5.0,
            50.0,
            "Test Rocket Launcher I",
            &[(51, 2_000.0), (50, 1.0), (30, 1.0)],
        );
        launcher.effect_ids = vec![12, 16, 101];
        sde.types.insert(launcher_tid, launcher);

        sde.types.insert(
            std_missile_tid,
            custom_inv_type(sde, std_missile_tid, 384, 8, 0.01, 0.0, "Test Standard Rocket", &[]),
        );
        sde.types.insert(
            fof_missile_tid,
            custom_inv_type(sde, fof_missile_tid, 395, 8, 0.01, 0.0, "Test FoF Light Missile", &[]),
        );
    });

    let char_id = 97_990_001_i64;
    let sys_id = 30_002_505_u32;
    let ship_id = 1_097_990_001_i64;
    let turret_id = 1_097_990_002_i64;
    let ab_id = 1_097_990_003_i64;
    let offline_sb_id = 1_097_990_004_i64;
    let mwd_id = 1_097_990_005_i64;
    let std_launcher_id = 1_097_990_006_i64;
    let std_charge_id = 1_097_990_007_i64;
    let fof_launcher_id = 1_097_990_008_i64;
    let fof_charge_id = 1_097_990_009_i64;

    ensure_char(&ctx, char_id, 98_000_001, sys_id, ship_id);
    insert_item(&ctx, ship_id, 587, char_id, sys_id as i64, InventoryFlag::None, 1, true);
    insert_item(&ctx, turret_id, turret_tid, char_id, ship_id, InventoryFlag::HiSlot0, 1, true);
    insert_item(&ctx, std_launcher_id, launcher_tid, char_id, ship_id, InventoryFlag::HiSlot1, 1, true);
    insert_item(&ctx, std_charge_id, std_missile_tid, char_id, ship_id, InventoryFlag::HiSlot1, 10, false);
    insert_item(&ctx, fof_launcher_id, launcher_tid, char_id, ship_id, InventoryFlag::HiSlot2, 1, true);
    insert_item(&ctx, fof_charge_id, fof_missile_tid, char_id, ship_id, InventoryFlag::HiSlot2, 10, false);
    insert_item(&ctx, ab_id, ab_tid, char_id, ship_id, InventoryFlag::MedSlot0, 1, true);
    insert_item(&ctx, offline_sb_id, sb_tid, char_id, ship_id, InventoryFlag::MedSlot1, 1, true);
    insert_item(&ctx, mwd_id, mwd_tid, char_id, ship_id, InventoryFlag::MedSlot2, 1, true);
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(turret_id, false);
        snap.module_online_states.insert(offline_sb_id, false);
        snap.module_online_states.insert(mwd_id, false);
    });
    ctx.invalidate_ship_fitting(ship_id);

    spawn_ship(&ctx, sys_id, ship_id, 587, char_id, 500.0).await;
    let sys = ctx.systems.get_or_spawn(sys_id);
    let kw = HashMap::new();

    // 1. Docked "online" brings offline module online and stays idempotent when already online.
    let mut docked_sess = SessionState {
        user_id: Some(char_id as u64),
        character_id: Some(char_id),
        corporation_id: Some(98_000_001),
        station_id: Some(60_003_760),
        solar_system_id: Some(sys_id),
        ship_id: Some(ship_id),
        ..Default::default()
    };
    let on_docked = handle_module_cycles(
        &ctx,
        &mut docked_sess,
        None,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(turret_id), PyRep::String("online".into())],
        &kw,
    )
    .await
    .unwrap()
    .expect("docked online must succeed");
    assert_eq!(on_docked.dict_get("online"), Some(&PyRep::Bool(true)));
    let on_docked_again = handle_module_cycles(
        &ctx,
        &mut docked_sess,
        None,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(turret_id), PyRep::String("online".into())],
        &kw,
    )
    .await
    .unwrap()
    .expect("docked online on already-online module must succeed");
    assert_eq!(on_docked_again.dict_get("online"), Some(&PyRep::Bool(true)));

    // 2. Actively cloaked in space: both offline and already-online "online" calls reject with 0 mutation.
    let mut cs = sys.get_combat_state(ship_id).await.unwrap().unwrap();
    cs.is_cloaked = true;
    cs.gate_cloak_remaining_ticks = 0;
    sys.set_combat_state(ship_id, cs).await.unwrap();

    let mut space_sess = SessionState {
        user_id: Some(char_id as u64),
        character_id: Some(char_id),
        corporation_id: Some(98_000_001),
        solar_system_id: Some(sys_id),
        ship_id: Some(ship_id),
        ..Default::default()
    };
    for (mod_id, was_offline) in [(offline_sb_id, true), (turret_id, false), (ab_id, false)] {
        let err = handle_module_cycles(
            &ctx,
            &mut space_sess,
            None,
            "dogmaIM",
            "Activate",
            &[PyRep::Integer(mod_id), PyRep::String("online".into())],
            &kw,
        )
        .await
        .unwrap()
        .expect_err("actively cloaked space Activate(..., \"online\") must reject");
        assert_eq!(err.code, "CannotActivateCloaked");
        assert!(!sys.has_module_cycle(ship_id, mod_id).await);
        ctx.db.with_snapshot(|snap| {
            assert!(!snap.module_active_states.contains(&mod_id));
            if was_offline {
                assert_eq!(snap.module_online_states.get(&mod_id), Some(&false));
            }
        });
    }
    assert!((sys.get_combat_state(ship_id).await.unwrap().unwrap().capacitor - 500.0).abs() < 1e-6);

    // 3. Uncloaked in space: offline "online" succeeds; targetless turrets/std-missiles reject cleanly; FoF exempt.
    let mut cs_uncloaked = sys.get_combat_state(ship_id).await.unwrap().unwrap();
    cs_uncloaked.is_cloaked = false;
    cs_uncloaked.gate_cloak_remaining_ticks = 0;
    sys.set_combat_state(ship_id, cs_uncloaked).await.unwrap();

    let on_space = handle_module_cycles(
        &ctx,
        &mut space_sess,
        None,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(offline_sb_id), PyRep::String("online".into())],
        &kw,
    )
    .await
    .unwrap()
    .expect("uncloaked space offline Activate(..., \"online\") must succeed");
    assert_eq!(on_space.dict_get("online"), Some(&PyRep::Bool(true)));

    for effect_arg in [None, Some("online")] {
        let mut args = vec![PyRep::Integer(turret_id)];
        if let Some(eff) = effect_arg {
            args.push(PyRep::String(eff.into()));
        }
        let err = handle_module_cycles(&ctx, &mut space_sess, None, "dogmaIM", "Activate", &args, &kw)
            .await
            .unwrap()
            .expect_err("targetless turret Activate must reject with TargetRequired");
        assert_eq!(err.code, "TargetRequired");
        assert!(!sys.has_module_cycle(ship_id, turret_id).await);
        ctx.db.with_snapshot(|snap| assert!(!snap.module_active_states.contains(&turret_id)));
    }
    assert!((sys.get_combat_state(ship_id).await.unwrap().unwrap().capacitor - 500.0).abs() < 1e-6);

    let std_err = handle_module_cycles(
        &ctx,
        &mut space_sess,
        None,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(std_launcher_id)],
        &kw,
    )
    .await
    .unwrap()
    .expect_err("targetless standard missile launcher must reject with TargetRequired");
    assert_eq!(std_err.code, "TargetRequired");
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&std_charge_id].quantity, 10);
        assert!(!snap.module_active_states.contains(&std_launcher_id));
    });

    let fit = ctx.get_or_rebuild_ship_fitting(char_id, ship_id).unwrap();
    assert!(activation_requires_explicit_target(&ctx, ship_id, &fit.modules[&std_launcher_id], 507, true));
    assert!(!activation_requires_explicit_target(&ctx, ship_id, &fit.modules[&fof_launcher_id], 507, true));
    if let Some(Err(e)) = handle_module_cycles(
        &ctx,
        &mut space_sess,
        None,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(fof_launcher_id)],
        &kw,
    )
    .await
    {
        assert_ne!(e.code, "TargetRequired", "FoF-loaded launcher must remain exempt from TargetRequired");
    }

    // 4. Uncloaked already-online AB activates cleanly; deadspace MWD with "online" rejects without mutation.
    let ab_ok = handle_module_cycles(
        &ctx,
        &mut space_sess,
        None,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(ab_id), PyRep::String("online".into())],
        &kw,
    )
    .await
    .unwrap()
    .expect("uncloaked space online AB Activate(..., \"online\") keeps active cycle fallthrough");
    assert_eq!(ab_ok.dict_get("active"), Some(&PyRep::Bool(true)));

    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(ab_id, false);
        snap.module_online_states.insert(mwd_id, true);
    });
    ctx.invalidate_ship_fitting(ship_id);

    // 5. F-02 & F-03: While gate-cloaked, rejected calls (targetless turret, dungeon runtime
    //    MicroWarpdriveDisallowedInDeadspace, dungeon editor ModuleNotAllowedInDeadspace) MUST NOT
    //    consume gate cloak or undock invulnerability; valid activation breaks gate cloak on commit.
    let mut gate_cs = sys.get_combat_state(ship_id).await.unwrap().unwrap();
    gate_cs.is_cloaked = true;
    gate_cs.gate_cloak_remaining_ticks = 30;
    gate_cs.undock_invulnerable_remaining_ticks = 15;
    sys.set_combat_state(ship_id, gate_cs).await.unwrap();
    let _ = ctx.drain_notifications_for_char(char_id);

    for effect_arg in [None, Some("online")] {
        let mut args = vec![PyRep::Integer(turret_id)];
        if let Some(eff) = effect_arg {
            args.push(PyRep::String(eff.into()));
        }
        let err = handle_module_cycles(&ctx, &mut space_sess, None, "dogmaIM", "Activate", &args, &kw)
            .await
            .unwrap()
            .expect_err("gate-cloaked targetless turret must reject with TargetRequired");
        assert_eq!(err.code, "TargetRequired");
        let after = sys.get_combat_state(ship_id).await.unwrap().unwrap();
        assert!(after.is_cloaked);
        assert_eq!(after.gate_cloak_remaining_ticks, 30);
        assert_eq!(after.undock_invulnerable_remaining_ticks, 15);
        let notifs = ctx.drain_notifications_for_char(char_id);
        assert!(!notifs.iter().any(|n| n.event == "OnJumpCloakCancelled" || n.event == "OnInvulnCancelled"));
    }

    let cap_before_rt_mwd = sys.get_combat_state(ship_id).await.unwrap().unwrap().capacitor;
    crate::services::pve_dungeon_runtime::with_dungeon_runtime_store(&ctx, |store| {
        store.instances.insert(
            998_101,
            crate::services::pve_dungeon_runtime::DungeonRuntimeInstance {
                instance_id: 998_101,
                dungeon_id: 42,
                dungeon_name: "Deadspace Runtime Test".into(),
                category: "combat".into(),
                archetype: "pirate".into(),
                solar_system_id: sys_id,
                owner_char_id: char_id,
                base_position: [10_000.0, 0.0, 0.0],
                disallow_mwd: true,
                entry_gate_ball_id: None,
                entry_gate_pos: None,
                entry_gate_locked: false,
                current_room_index: 1,
                rooms: vec![crate::services::pve_dungeon_runtime::DungeonRuntimeRoom {
                    room_index: 0,
                    room_id: 1,
                    room_name: "MWD Blocked Pocket".into(),
                    position: [10_000.0, 0.0, 0.0],
                    gate_destination_pos: [10_000.0, 0.0, 0.0],
                    gated: false,
                    disallow_mwd: Some(true),
                    gate_ball_id: None,
                    gate_locked: false,
                    objects: Vec::new(),
                    npc_groups: Vec::new(),
                    triggers: Vec::new(),
                    spawned: true,
                    cleared: false,
                }],
                completed: false,
                despawned: false,
                active_objective_id: 1,
                last_completed_task_id: None,
                completed_task_ids: Vec::new(),
                created_unix: ctx.now_unix(),
            },
        );
    });
    for effect_arg in [None, Some("online")] {
        let mut args = vec![PyRep::Integer(mwd_id)];
        if let Some(eff) = effect_arg {
            args.push(PyRep::String(eff.into()));
        }
        let rt_mwd_err = handle_module_cycles(&ctx, &mut space_sess, None, "dogmaIM", "Activate", &args, &kw)
            .await
            .unwrap()
            .expect_err("dungeon runtime disallow_mwd must reject with MicroWarpdriveDisallowedInDeadspace");
        assert_eq!(rt_mwd_err.code, "MicroWarpdriveDisallowedInDeadspace");
        assert!(!sys.has_module_cycle(ship_id, mwd_id).await);
        ctx.db.with_snapshot(|snap| assert!(!snap.module_active_states.contains(&mwd_id)));
        let after = sys.get_combat_state(ship_id).await.unwrap().unwrap();
        assert!((after.capacitor - cap_before_rt_mwd).abs() < 1e-6);
        assert!(after.is_cloaked);
        assert_eq!(after.gate_cloak_remaining_ticks, 30);
        assert_eq!(after.undock_invulnerable_remaining_ticks, 15);
        let notifs = ctx.drain_notifications_for_char(char_id);
        assert!(!notifs.iter().any(|n| n.event == "OnJumpCloakCancelled" || n.event == "OnInvulnCancelled"));
    }
    crate::services::pve_dungeon_runtime::with_dungeon_runtime_store(&ctx, |store| {
        store.instances.clear();
    });

    crate::services::pve_dungeon_editor::with_dungeon_editor_store(&ctx, |ed| {
        ed.dungeon_disallow_mwd.insert(1, true);
        ed.active_editors.insert(
            char_id,
            crate::services::pve_dungeon_editor::GmActiveEditorState {
                solar_system_id: Some(sys_id),
                dungeon_id: Some(1),
                room_id: None,
                ..Default::default()
            },
        );
    });
    let cap_before_mwd = sys.get_combat_state(ship_id).await.unwrap().unwrap().capacitor;
    let mwd_err = handle_module_cycles(
        &ctx,
        &mut space_sess,
        None,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(mwd_id), PyRep::String("online".into())],
        &kw,
    )
    .await
    .unwrap()
    .expect_err("deadspace MWD Activate(..., \"online\") must reject");
    assert_eq!(mwd_err.code, "ModuleNotAllowedInDeadspace");
    assert!(!sys.has_module_cycle(ship_id, mwd_id).await);
    ctx.db.with_snapshot(|snap| assert!(!snap.module_active_states.contains(&mwd_id)));
    let after_ed_mwd = sys.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!((after_ed_mwd.capacitor - cap_before_mwd).abs() < 1e-6);
    assert!(after_ed_mwd.is_cloaked);
    assert_eq!(after_ed_mwd.gate_cloak_remaining_ticks, 30);
    assert_eq!(after_ed_mwd.undock_invulnerable_remaining_ticks, 15);
    let _ = crate::services::pve_dungeon_editor::with_dungeon_editor_store(&ctx, |ed| {
        ed.dungeon_disallow_mwd.clear();
        ed.active_editors.clear();
    });

    let sb_ok = handle_module_cycles(
        &ctx,
        &mut space_sess,
        None,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(offline_sb_id)],
        &kw,
    )
    .await
    .unwrap()
    .expect("valid gate-cloaked shield booster activation must succeed and break gate cloak");
    assert_eq!(sb_ok.dict_get("active"), Some(&PyRep::Bool(true)));
    let after_sb = sys.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!(!after_sb.is_cloaked);
    assert_eq!(after_sb.gate_cloak_remaining_ticks, 0);
    assert_eq!(after_sb.undock_invulnerable_remaining_ticks, 0);
    let sb_notifs = ctx.drain_notifications_for_char(char_id);
    assert!(sb_notifs.iter().any(|n| n.event == "OnJumpCloakCancelled"));
    assert!(sb_notifs.iter().any(|n| n.event == "OnInvulnCancelled"));

    // 6. P2: Stale DB module_active_states on a weapon with no actor cycle or reactivation delay
    //    must still enforce TargetRequired cleanly rather than bypassing targeting.
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_active_states.insert(turret_id);
    });
    ctx.invalidate_ship_fitting(ship_id);
    for effect_arg in [None, Some("online")] {
        let mut args = vec![PyRep::Integer(turret_id)];
        if let Some(eff) = effect_arg {
            args.push(PyRep::String(eff.into()));
        }
        let stale_err = handle_module_cycles(&ctx, &mut space_sess, None, "dogmaIM", "Activate", &args, &kw)
            .await
            .unwrap()
            .expect_err("stale DB module_active_states must not bypass TargetRequired for weapons");
        assert_eq!(stale_err.code, "TargetRequired");
        assert!(!sys.has_module_cycle(ship_id, turret_id).await);
    }
}

#[tokio::test]
async fn f01_mercden_remote_shield_repair_gate_cloak_and_defers_break() {
    let rep_tid = 998_141_u32;
    let ctx = test_ctx_with_custom(|sde| {
        let mut rep_t = custom_inv_type(
            sde,
            rep_tid,
            41,
            7,
            5.0,
            0.0,
            "Small Test Remote Shield Booster",
            &[(54, 10_000.0), (158, 2_000.0), (6, 50.0), (68, 1_500.0), (73, 5_000.0)],
        );
        rep_t.effect_ids = vec![13, 16];
        sde.types.insert(rep_tid, rep_t);
    });

    let char_id = 97_991_001_i64;
    let sys_id = 30_000_142_u32;
    let ship_id = 1_097_991_001_i64;
    let rep_mod_id = 1_097_991_002_i64;
    let den_id = 1_097_991_900_i64;

    ensure_char(&ctx, char_id, 98_000_001, sys_id, ship_id);
    insert_item(&ctx, ship_id, 587, char_id, i64::from(sys_id), InventoryFlag::None, 1, true);
    insert_item(&ctx, rep_mod_id, rep_tid, char_id, ship_id, InventoryFlag::MedSlot0, 1, true);
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(rep_mod_id, true);
        snap.structures.insert(
            den_id,
            eve_db::UpwellStructureRecord {
                structure_id: den_id,
                solar_system_id: sys_id,
                type_id: 85_230,
                owner_corp_id: 98_000_001,
                name: "Test Mercenary Den".into(),
                state: "online".into(),
                shield_hp: 500.0,
                armor_hp: 1_000.0,
                hull_hp: 1_000.0,
                position: [25_000.0, 0.0, 0.0],
                ..Default::default()
            },
        );
        snap.sov_runtime.mercenary_dens.insert(
            den_id,
            eve_db::durable_domain_types::PersistedMercenaryDen {
                den_id,
                owner_char_id: char_id,
                skyhook_id: 79_000_001,
                solar_system_id: sys_id,
                planet_id: 40_000_001,
                type_id: 85_230,
                enabled: false,
                cargo_extraction_enabled: true,
                development_level: 0,
                anarchy_level: 0,
                infomorphs: 0,
                simulated_at_unix: 1_800_000_000,
                next_activity_generation_unix: 1_800_000_000,
            },
        );
    });
    ctx.invalidate_ship_fitting(ship_id);

    spawn_ship(&ctx, sys_id, ship_id, 587, char_id, 500.0).await;
    let sys = ctx.systems.get_or_spawn(sys_id);
    let mut sess = SessionState {
        user_id: Some(char_id as u64),
        character_id: Some(char_id),
        corporation_id: Some(98_000_001),
        solar_system_id: Some(sys_id),
        ship_id: Some(ship_id),
        ..Default::default()
    };
    let kw = HashMap::new();
    let args = [
        PyRep::Integer(rep_mod_id),
        PyRep::String("activate".into()),
        PyRep::Integer(den_id),
    ];

    // 1. Direct helper still rejects non-gate active cloak with Cloaked.
    let fit = ctx.get_or_rebuild_ship_fitting(char_id, ship_id).unwrap();
    let fm = fit.modules[&rep_mod_id].clone();
    let mod_t = ctx.sde.types[&rep_tid].clone();
    sys.add_target_lock(
        ship_id,
        eve_destiny::TargetLock {
            target_id: den_id,
            lock_time_ms: 100.0,
            remaining_lock_ms: 0.0,
            locked: true,
            max_range_m: Some(50_000.0),
        },
    )
    .await
    .unwrap();
    sys.set_combat_state(
        ship_id,
        CombatState {
            is_cloaked: true,
            gate_cloak_remaining_ticks: 0,
            capacitor: 500.0,
            max_capacitor: 500.0,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let direct_active_err = try_handle_mercden_remote_shield_repair(
        &ctx, &sys, sys_id, char_id, ship_id, rep_mod_id, &fm, &mod_t, &fit, "Activate", den_id,
    )
    .await
    .expect_err("non-gate active cloak must reject MercDen remote shield repair with Cloaked");
    assert_eq!(direct_active_err.code, "Cloaked");

    // 2. Gate cloak allows MercDen handler past cloak guard; out-of-range failure preserves gate cloak & undock invuln.
    sys.set_combat_state(
        ship_id,
        CombatState {
            is_cloaked: true,
            gate_cloak_remaining_ticks: 30,
            undock_invulnerable_remaining_ticks: 15,
            capacitor: 500.0,
            max_capacitor: 500.0,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let _ = ctx.drain_notifications_for_char(char_id);
    let range_err = handle_module_cycles(&ctx, &mut sess, None, "dogmaIM", "Activate", &args, &kw)
        .await
        .unwrap()
        .expect_err("gate-cloaked MercDen out-of-range must return TargetOutOfRange, not Cloaked");
    assert_eq!(range_err.code, "TargetOutOfRange");
    let cs_range = sys.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!(cs_range.is_cloaked);
    assert_eq!(cs_range.gate_cloak_remaining_ticks, 30);
    assert_eq!(cs_range.undock_invulnerable_remaining_ticks, 15);
    assert!(!ctx.drain_notifications_for_char(char_id).iter().any(|n| n.event == "OnJumpCloakCancelled"));

    // 3. Move MercDen within range (2km from ship at 10km): gate-cloaked activation succeeds and breaks cloak on commit.
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.get_mut(&den_id).unwrap().position = [12_000.0, 0.0, 0.0];
    });
    let ok = handle_module_cycles(&ctx, &mut sess, None, "dogmaIM", "Activate", &args, &kw)
        .await
        .unwrap()
        .expect("gate-cloaked MercDen repair in range must succeed");
    assert_eq!(ok.dict_get("moduleID").and_then(PyRep::as_i64), Some(rep_mod_id));
    let cs_ok = sys.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!(!cs_ok.is_cloaked);
    assert_eq!(cs_ok.gate_cloak_remaining_ticks, 0);
    assert_eq!(cs_ok.undock_invulnerable_remaining_ticks, 0);
    assert!(ctx.drain_notifications_for_char(char_id).iter().any(|n| n.event == "OnJumpCloakCancelled"));
}

#[tokio::test]
async fn f02_post_cycle_safety_rollback_restores_undock_invuln_and_gate_cloak() {
    let web_tid = 998_065_u32;
    let burst_tid = 998_080_u32;
    let ctx = test_ctx_with_custom(|sde| {
        let mut web_t = custom_inv_type(
            sde,
            web_tid,
            65,
            7,
            5.0,
            0.0,
            "Test Stasis Webifier I",
            &[(54, 10_000.0), (20, -50.0), (73, 5_000.0), (6, 25.0)],
        );
        web_t.effect_ids = vec![13, 16];
        sde.types.insert(web_tid, web_t);

        let mut burst_t = custom_inv_type(
            sde,
            burst_tid,
            80,
            7,
            5.0,
            0.0,
            "Test Burst Jammer I",
            &[(238, 80.0), (239, 80.0), (240, 80.0), (241, 80.0), (197, 15_000.0), (73, 5_000.0), (6, 25.0)],
        );
        burst_t.effect_ids = vec![13, 16];
        sde.types.insert(burst_tid, burst_t);
    });

    let char_id = 97_992_001_i64;
    let victim_char_id = 97_992_002_i64;
    let sys_id = 30_000_142_u32;
    let ship_id = 1_097_992_001_i64;
    let victim_ship_id = 1_097_992_002_i64;
    let web_id = 1_097_992_003_i64;
    let burst_id = 1_097_992_004_i64;

    ensure_char(&ctx, char_id, 98_000_001, sys_id, ship_id);
    ensure_char(&ctx, victim_char_id, 98_000_002, sys_id, victim_ship_id);
    insert_item(&ctx, ship_id, 587, char_id, i64::from(sys_id), InventoryFlag::None, 1, true);
    insert_item(&ctx, victim_ship_id, 587, victim_char_id, i64::from(sys_id), InventoryFlag::None, 1, true);
    insert_item(&ctx, web_id, web_tid, char_id, ship_id, InventoryFlag::MedSlot0, 1, true);
    insert_item(&ctx, burst_id, burst_tid, char_id, ship_id, InventoryFlag::MedSlot1, 1, true);
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(web_id, true);
        snap.module_online_states.insert(burst_id, true);
    });
    ctx.invalidate_ship_fitting(ship_id);

    spawn_ship(&ctx, sys_id, ship_id, 587, char_id, 200.0).await;
    let sys = ctx.systems.get_or_spawn(sys_id);
    sys.spawn_ball_with_meta(
        Ball {
            id: victim_ship_id,
            type_id: 587,
            position: DVec3::new(11_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 500.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 100.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
        CombatState {
            capacitor: 200.0,
            max_capacitor: 200.0,
            ..Default::default()
        },
        SlimItemMeta {
            group_id: 25,
            category_id: 6,
            owner_id: victim_char_id,
            char_id: Some(victim_char_id),
            corp_id: 98_000_002,
            security_status: 5.0,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    sys.add_target_lock(
        ship_id,
        eve_destiny::TargetLock {
            target_id: victim_ship_id,
            lock_time_ms: 100.0,
            remaining_lock_ms: 0.0,
            locked: true,
            max_range_m: Some(50_000.0),
        },
    )
    .await
    .unwrap();

    let _ = ctx.db.acquire_online_lock(char_id as u64, 1, Some(char_id), true, ctx.now_unix());
    crate::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| {
        rt.safety_levels.insert(char_id, crate::services::combat_crimewatch::SafetyLevel::Green);
    });
    let mut sess = SessionState {
        user_id: Some(char_id as u64),
        character_id: Some(char_id),
        corporation_id: Some(98_000_001),
        solar_system_id: Some(sys_id),
        ship_id: Some(ship_id),
        ..Default::default()
    };
    let kw = HashMap::new();

    // 1. Burst Jammer in highsec with Green safety fails upfront and preserves undock invuln + gate cloak.
    sys.set_combat_state(
        ship_id,
        CombatState {
            is_cloaked: true,
            gate_cloak_remaining_ticks: 30,
            undock_invulnerable_remaining_ticks: 15,
            capacitor: 200.0,
            max_capacitor: 200.0,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let _ = ctx.drain_notifications_for_char(char_id);
    let burst_err = handle_module_cycles(&ctx, &mut sess, None, "dogmaIM", "Activate", &[PyRep::Integer(burst_id)], &kw)
        .await
        .unwrap()
        .expect_err("Burst Jammer in highsec on Green safety must reject");
    assert_eq!(burst_err.code, "CantDoInCurrentSafetyLevel");
    let cs_burst = sys.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!(cs_burst.is_cloaked);
    assert_eq!(cs_burst.gate_cloak_remaining_ticks, 30);
    assert_eq!(cs_burst.undock_invulnerable_remaining_ticks, 15);
    assert!((cs_burst.capacitor - 200.0).abs() < 1e-6);
    assert!(!sys.has_module_cycle(ship_id, burst_id).await);
    assert!(!ctx.drain_notifications_for_char(char_id).iter().any(|n| n.event == "OnJumpCloakCancelled" || n.event == "OnInvulnCancelled"));

    // 2. Targeted Stasis Webifier fails read-only Crimewatch safety precheck before add_module_cycle,
    //    preserving undock_invuln, warp_exit_invuln, capacitor, and gate cloak with zero notifications.
    let web_args = [
        PyRep::Integer(web_id),
        PyRep::String("activate".into()),
        PyRep::Integer(victim_ship_id),
    ];
    for (cloaked, gate_ticks) in [(false, 0_u32), (true, 30_u32)] {
        sys.set_combat_state(
            ship_id,
            CombatState {
                is_cloaked: cloaked,
                gate_cloak_remaining_ticks: gate_ticks,
                undock_invulnerable_remaining_ticks: 15,
                sync_invuln_with_undock_timer: true,
                warp_exit_invulnerable_remaining_ticks: 15,
                capacitor: 200.0,
                max_capacitor: 200.0,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let _ = ctx.drain_notifications_for_char(char_id);
        let web_err = handle_module_cycles(&ctx, &mut sess, None, "dogmaIM", "Activate", &web_args, &kw)
            .await
            .unwrap()
            .expect_err("Webifier vs neutral in highsec on Green safety must reject cleanly");
        assert_eq!(web_err.code, "CantDoInCurrentSafetyLevel");
        let cs_after = sys.get_combat_state(ship_id).await.unwrap().unwrap();
        assert_eq!(cs_after.undock_invulnerable_remaining_ticks, 15);
        assert_eq!(cs_after.gate_cloak_remaining_ticks, gate_ticks);
        assert_eq!(cs_after.is_cloaked, cloaked);
        assert!((cs_after.capacitor - 200.0).abs() < 1e-6);
        assert!(!sys.has_module_cycle(ship_id, web_id).await);
        assert!(!ctx.drain_notifications_for_char(char_id).iter().any(|n| n.event == "OnJumpCloakCancelled" || n.event == "OnInvulnCancelled"));
    }

    // 3. Red safety allows the Webifier to commit, deducts cap, adds cycle, and breaks gate cloak + undock invuln (single OnInvulnCancelled).
    crate::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| {
        rt.safety_levels.insert(char_id, crate::services::combat_crimewatch::SafetyLevel::Red);
    });
    let _ = ctx.drain_notifications_for_char(char_id);
    let ok = handle_module_cycles(&ctx, &mut sess, None, "dogmaIM", "Activate", &web_args, &kw)
        .await
        .unwrap()
        .expect("Webifier on Red safety must commit");
    assert_eq!(ok.dict_get("active"), Some(&PyRep::Bool(true)));
    let cs_ok = sys.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!(!cs_ok.is_cloaked);
    assert_eq!(cs_ok.gate_cloak_remaining_ticks, 0);
    assert_eq!(cs_ok.undock_invulnerable_remaining_ticks, 0);
    assert_eq!(cs_ok.warp_exit_invulnerable_remaining_ticks, 0);
    assert!((cs_ok.capacitor - 175.0).abs() < 1e-6);
    assert!(sys.has_module_cycle(ship_id, web_id).await);
    let commit_notes = ctx.drain_notifications_for_char(char_id);
    assert!(commit_notes.iter().any(|n| n.event == "OnJumpCloakCancelled"));
    assert_eq!(commit_notes.iter().filter(|n| n.event == "OnInvulnCancelled").count(), 1);
}

#[tokio::test]
async fn p2_rapid_launchers_is_weapon_and_targeted_ewar_assist_require_target() {
    const TARGETED_GROUPS: &[u32] = &[
        52, 65, 68, 71, 201, 208, 291, 379, 1672,
        41, 67, 209, 290, 325, 585, 1697, 1698, 2018,
        771, 1245,
    ];
    let ctx = test_ctx_with_custom(|sde| {
        for &grp in TARGETED_GROUPS {
            let tid = 997_000 + grp;
            let mut t = custom_inv_type(
                sde,
                tid,
                grp,
                7,
                5.0,
                0.0,
                &format!("TestTargetedGroup_{grp}"),
                &[(73, 5_000.0), (54, 10_000.0), (50, 1.0), (30, 1.0)],
            );
            t.effect_ids = vec![13, 16];
            sde.types.insert(tid, t);
        }
    });

    let char_id = 97_993_001_i64;
    let sys_id = 30_000_142_u32;
    let ship_id = 1_097_993_001_i64;
    let mod_id = 1_097_993_002_i64;

    ensure_char(&ctx, char_id, 98_000_001, sys_id, ship_id);
    insert_item(&ctx, ship_id, 587, char_id, i64::from(sys_id), InventoryFlag::None, 1, true);
    insert_item(&ctx, mod_id, 997_052, char_id, ship_id, InventoryFlag::MedSlot0, 1, true);
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(mod_id, true);
    });
    spawn_ship(&ctx, sys_id, ship_id, 587, char_id, 500.0).await;
    let sys = ctx.systems.get_or_spawn(sys_id);
    sys.set_combat_state(
        ship_id,
        CombatState {
            is_cloaked: true,
            gate_cloak_remaining_ticks: 30,
            undock_invulnerable_remaining_ticks: 15,
            capacitor: 500.0,
            max_capacitor: 500.0,
            ..Default::default()
        },
    )
    .await
    .unwrap();

    let mut sess = SessionState {
        user_id: Some(char_id as u64),
        character_id: Some(char_id),
        corporation_id: Some(98_000_001),
        solar_system_id: Some(sys_id),
        ship_id: Some(ship_id),
        ..Default::default()
    };
    let kw = HashMap::new();

    for &grp in TARGETED_GROUPS {
        let tid = 997_000 + grp;
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.get_mut(&mod_id).unwrap().type_id = tid;
            // Even stale DB active state must NOT let weapons 771/1245 bypass TargetRequired.
            if matches!(grp, 771 | 1245) {
                snap.module_active_states.insert(mod_id);
            } else {
                snap.module_active_states.remove(&mod_id);
            }
        });
        ctx.invalidate_ship_fitting(ship_id);

        let err = handle_module_cycles(&ctx, &mut sess, None, "dogmaIM", "Activate", &[PyRep::Integer(mod_id)], &kw)
            .await
            .unwrap()
            .expect_err("targetless Activate on targeted EWAR/remote-assist/rapid launcher must reject");
        assert_eq!(err.code, "TargetRequired", "group {grp} must require explicit target");
        let cs = sys.get_combat_state(ship_id).await.unwrap().unwrap();
        assert!(cs.is_cloaked, "group {grp} TargetRequired must preserve gate cloak");
        assert_eq!(cs.gate_cloak_remaining_ticks, 30);
        assert_eq!(cs.undock_invulnerable_remaining_ticks, 15);
    }

    // Missile launchers 771 & 1245 are treated as weapons during reactivation delay too.
    for grp in [771_u32, 1245_u32] {
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.get_mut(&mod_id).unwrap().type_id = 997_000 + grp;
            snap.module_active_states.remove(&mod_id);
        });
        ctx.invalidate_ship_fitting(ship_id);
        ctx.record_module_reactivation_delay(ship_id, mod_id, 10.0);
        let delay_err = handle_module_cycles(&ctx, &mut sess, None, "dogmaIM", "Activate", &[PyRep::Integer(mod_id)], &kw)
            .await
            .unwrap()
            .expect_err("rapid launcher in reactivation delay must reject as weapon");
        assert_eq!(delay_err.code, "ModuleAlreadyActive");
        ctx.module_reactivation_cooldowns.write().unwrap().clear();
    }
}

#[tokio::test]
async fn bf1_bf2_highsec_green_ewar_neut_remote_assist_zero_target_mutation_and_tractor_post_cycle_rollback() {
    let neut_tid = 996_071_u32;
    let rsb_tid = 996_041_u32;
    let arsb_tid = 996_697_u32;
    let rct_tid = 996_067_u32;
    let rtc_tid = 996_209_u32;
    let rsb_ewar_tid = 996_290_u32;
    let tp_tid = 996_379_u32;
    let tractor_tid = 996_650_u32;
    let smartbomb_tid = 996_072_u32;
    let burst_tid = 996_080_u32;
    let wreck_tid = 996_999_u32;

    let ctx = test_ctx_with_custom(|sde| {
        let mut neut_t = custom_inv_type(
            sde,
            neut_tid,
            71,
            7,
            5.0,
            0.0,
            "Test Energy Neutralizer I",
            &[(6, 20.0), (54, 15_000.0), (73, 5_000.0), (97, 50.0)],
        );
        neut_t.effect_ids = vec![13, 16];
        sde.types.insert(neut_tid, neut_t);

        let mut rsb_t = custom_inv_type(
            sde,
            rsb_tid,
            41,
            7,
            5.0,
            0.0,
            "Test Remote Shield Booster I",
            &[(6, 20.0), (54, 15_000.0), (73, 5_000.0), (68, 100.0)],
        );
        rsb_t.effect_ids = vec![13, 16];
        sde.types.insert(rsb_tid, rsb_t);

        let mut arsb_t = custom_inv_type(
            sde,
            arsb_tid,
            1697,
            7,
            5.0,
            0.0,
            "Test Ancillary Remote Shield Booster",
            &[(6, 20.0), (54, 15_000.0), (73, 5_000.0), (68, 100.0)],
        );
        arsb_t.effect_ids = vec![13, 16];
        sde.types.insert(arsb_tid, arsb_t);

        let mut rct_t = custom_inv_type(
            sde,
            rct_tid,
            67,
            7,
            5.0,
            0.0,
            "Test Remote Capacitor Transmitter I",
            &[(6, 20.0), (54, 15_000.0), (73, 5_000.0), (90, 60.0)],
        );
        rct_t.effect_ids = vec![13, 16];
        sde.types.insert(rct_tid, rct_t);

        let mut rtc_t = custom_inv_type(
            sde,
            rtc_tid,
            209,
            7,
            5.0,
            0.0,
            "Test Remote Tracking Computer I",
            &[(6, 20.0), (54, 15_000.0), (73, 5_000.0)],
        );
        rtc_t.effect_ids = vec![13, 16];
        sde.types.insert(rtc_tid, rtc_t);

        let mut rsb_ewar_t = custom_inv_type(
            sde,
            rsb_ewar_tid,
            290,
            7,
            5.0,
            0.0,
            "Test Remote Sensor Booster I",
            &[(6, 20.0), (54, 15_000.0), (73, 5_000.0)],
        );
        rsb_ewar_t.effect_ids = vec![13, 16];
        sde.types.insert(rsb_ewar_tid, rsb_ewar_t);

        let mut tp_t = custom_inv_type(
            sde,
            tp_tid,
            379,
            7,
            5.0,
            0.0,
            "Test Target Painter I",
            &[(6, 20.0), (54, 20_000.0), (73, 5_000.0), (554, 30.0), (2044, 10_000.0)],
        );
        tp_t.effect_ids = vec![13, 16];
        sde.types.insert(tp_tid, tp_t);

        let mut tractor_t = custom_inv_type(
            sde,
            tractor_tid,
            650,
            7,
            5.0,
            0.0,
            "Test Small Tractor Beam I",
            &[(6, 15.0), (54, 20_000.0), (73, 5_000.0)],
        );
        tractor_t.effect_ids = vec![13, 16];
        sde.types.insert(tractor_tid, tractor_t);

        let mut sb_t = custom_inv_type(
            sde,
            smartbomb_tid,
            72,
            7,
            5.0,
            0.0,
            "Test Small EMP Smartbomb I",
            &[(6, 25.0), (73, 5_000.0), (99, 5_000.0), (114, 40.0)],
        );
        sb_t.effect_ids = vec![13, 16];
        sde.types.insert(smartbomb_tid, sb_t);

        let mut burst_t = custom_inv_type(
            sde,
            burst_tid,
            80,
            7,
            5.0,
            0.0,
            "Test ECM Burst I",
            &[(6, 25.0), (73, 5_000.0), (142, 5_000.0), (238, 5.0), (239, 5.0), (240, 5.0), (241, 5.0)],
        );
        burst_t.effect_ids = vec![13, 16];
        sde.types.insert(burst_tid, burst_t);

        let wreck_t = custom_inv_type(sde, wreck_tid, 186, 2, 100.0, 0.0, "Test Wreck", &[]);
        sde.types.insert(wreck_tid, wreck_t);
    });

    let char_id = 97_994_001_i64;
    let victim_char_id = 97_994_002_i64;
    let sys_id = 30_000_142_u32;
    let ship_id = 1_097_994_001_i64;
    let victim_ship_id = 1_097_994_002_i64;
    let mod_id = 1_097_994_003_i64;
    let far_wreck_id = 1_097_994_004_i64;

    ensure_char(&ctx, char_id, 98_000_001, sys_id, ship_id);
    ensure_char(&ctx, victim_char_id, 98_000_002, sys_id, victim_ship_id);
    let _ = ctx.db.acquire_online_lock(char_id as u64, 1, Some(char_id), true, ctx.now_unix());
    insert_item(&ctx, ship_id, 587, char_id, i64::from(sys_id), InventoryFlag::None, 1, true);
    insert_item(&ctx, victim_ship_id, 587, victim_char_id, i64::from(sys_id), InventoryFlag::None, 1, true);
    insert_item(&ctx, mod_id, neut_tid, char_id, ship_id, InventoryFlag::MedSlot0, 1, true);
    insert_item(&ctx, far_wreck_id, wreck_tid, victim_char_id, i64::from(sys_id), InventoryFlag::None, 1, true);
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(mod_id, true);
    });
    spawn_ship(&ctx, sys_id, ship_id, 587, char_id, 500.0).await;
    let sys = ctx.systems.get_or_spawn(sys_id);
    sys.spawn_ball_with_meta(
        Ball {
            id: victim_ship_id,
            type_id: 587,
            position: DVec3::new(11_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 500.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 100.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
        CombatState {
            capacitor: 150.0,
            max_capacitor: 300.0,
            shield_hp: 100.0,
            max_shield_hp: 250.0,
            signature_radius: 40.0,
            base_signature_radius: Some(40.0),
            ..Default::default()
        },
        SlimItemMeta {
            group_id: 25,
            category_id: 6,
            owner_id: victim_char_id,
            char_id: Some(victim_char_id),
            corp_id: 98_000_002,
            security_status: 5.0,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    sys.spawn_ball_with_meta(
        Ball {
            id: far_wreck_id,
            type_id: wreck_tid,
            position: DVec3::new(50_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 1_000.0,
            radius: 50.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
        CombatState::default(),
        SlimItemMeta {
            group_id: 186,
            category_id: 2,
            owner_id: victim_char_id,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    for tid in [victim_ship_id, far_wreck_id] {
        sys.add_target_lock(
            ship_id,
            eve_destiny::TargetLock {
                target_id: tid,
                lock_time_ms: 100.0,
                remaining_lock_ms: 0.0,
                locked: true,
                max_range_m: Some(100_000.0),
            },
        )
        .await
        .unwrap();
    }

    let mut sess = SessionState {
        user_id: Some(char_id as u64),
        character_id: Some(char_id),
        corporation_id: Some(98_000_001),
        solar_system_id: Some(sys_id),
        ship_id: Some(ship_id),
        ..Default::default()
    };
    let kw = HashMap::new();

    // 1. Highsec Green safety blocks targeted Energy Neutralizer (71) and Target Painter (379)
    //    vs neutral target before mutating target cap / tp_sig_multiplier or attacker state.
    crate::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| {
        rt.safety_levels.insert(char_id, crate::services::combat_crimewatch::SafetyLevel::Green);
    });
    let reset_states = || {
        let sys_c = sys.clone();
        async move {
            sys_c
                .set_combat_state(
                    ship_id,
                    CombatState {
                        is_cloaked: true,
                        gate_cloak_remaining_ticks: 30,
                        undock_invulnerable_remaining_ticks: 15,
                        capacitor: 500.0,
                        max_capacitor: 500.0,
                        ..Default::default()
                    },
                )
                .await
                .unwrap();
            sys_c
                .set_combat_state(
                    victim_ship_id,
                    CombatState {
                        capacitor: 150.0,
                        max_capacitor: 300.0,
                        shield_hp: 100.0,
                        max_shield_hp: 250.0,
                        signature_radius: 40.0,
                        base_signature_radius: Some(40.0),
                        ..Default::default()
                    },
                )
                .await
                .unwrap();
        }
    };

    for (grp, tid, label) in [
        (71_u32, neut_tid, "Energy Neutralizer"),
        (379_u32, tp_tid, "Target Painter"),
    ] {
        reset_states().await;
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.get_mut(&mod_id).unwrap().type_id = tid;
        });
        ctx.invalidate_ship_fitting(ship_id);
        let _ = ctx.drain_notifications_for_char(char_id);

        let args = [
            PyRep::Integer(mod_id),
            PyRep::String("activate".into()),
            PyRep::Integer(victim_ship_id),
        ];
        let err = handle_module_cycles(&ctx, &mut sess, None, "dogmaIM", "Activate", &args, &kw)
            .await
            .unwrap()
            .expect_err("Green highsec offensive module must reject");
        assert_eq!(err.code, "CantDoInCurrentSafetyLevel", "{label} ({grp})");

        let att_cs = sys.get_combat_state(ship_id).await.unwrap().unwrap();
        assert!((att_cs.capacitor - 500.0).abs() < 1e-6, "{label} attacker cap");
        assert!(att_cs.is_cloaked && att_cs.gate_cloak_remaining_ticks == 30);
        assert_eq!(att_cs.undock_invulnerable_remaining_ticks, 15);
        assert!(!sys.has_module_cycle(ship_id, mod_id).await);

        let tgt_cs = sys.get_combat_state(victim_ship_id).await.unwrap().unwrap();
        assert!((tgt_cs.capacitor - 150.0).abs() < 1e-6, "{label} target cap leaked");
        assert!((tgt_cs.shield_hp - 100.0).abs() < 1e-6, "{label} target shield changed");
        assert!(tgt_cs.tp_sig_multiplier.is_none(), "{label} target tp_sig_multiplier leaked");
        assert!((ctx.effective_target_painter_multiplier(victim_ship_id) - 1.0).abs() < 1e-9);
        assert!(ctx
            .projected_ewar
            .read()
            .unwrap()
            .get(&victim_ship_id)
            .is_none_or(|v| v.is_empty()));
        let cw_st = ctx.crimewatch.read().unwrap().states.get(&char_id).cloned().unwrap_or_default();
        assert!(!cw_st.is_criminal(ctx.now_unix()) && !cw_st.is_suspect(ctx.now_unix()));
        assert!(!ctx.drain_notifications_for_char(char_id).iter().any(|n| n.event == "OnJumpCloakCancelled" || n.event == "OnInvulnCancelled"));
    }

    // 2. Highsec Green safety blocks remote assistance (41, 1697, 67, 209, 290) onto a Suspect pilot
    //    with zero target shield_hp / capacitor mutation.
    {
        let mut cw = ctx.crimewatch.write().unwrap();
        cw.get_or_create(victim_char_id).suspect_flag_until_unix = ctx.now_unix() + 600;
    }
    for (grp, tid, label) in [
        (41_u32, rsb_tid, "Remote Shield Booster"),
        (1697_u32, arsb_tid, "Ancillary Remote Shield Booster"),
        (67_u32, rct_tid, "Remote Cap Transmitter"),
        (209_u32, rtc_tid, "Remote Tracking Computer"),
        (290_u32, rsb_ewar_tid, "Remote Sensor Booster"),
    ] {
        reset_states().await;
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.get_mut(&mod_id).unwrap().type_id = tid;
        });
        ctx.invalidate_ship_fitting(ship_id);
        let _ = ctx.drain_notifications_for_char(char_id);

        let args = [
            PyRep::Integer(mod_id),
            PyRep::String("activate".into()),
            PyRep::Integer(victim_ship_id),
        ];
        let err = handle_module_cycles(&ctx, &mut sess, None, "dogmaIM", "Activate", &args, &kw)
            .await
            .unwrap()
            .expect_err("Green highsec remote assist vs Suspect must reject");
        assert_eq!(err.code, "CantDoInCurrentSafetyLevel", "{label} ({grp})");

        let att_cs = sys.get_combat_state(ship_id).await.unwrap().unwrap();
        assert!((att_cs.capacitor - 500.0).abs() < 1e-6);
        assert!(att_cs.is_cloaked && att_cs.gate_cloak_remaining_ticks == 30);
        assert_eq!(att_cs.undock_invulnerable_remaining_ticks, 15);
        assert!(!sys.has_module_cycle(ship_id, mod_id).await);

        let tgt_cs = sys.get_combat_state(victim_ship_id).await.unwrap().unwrap();
        assert!((tgt_cs.shield_hp - 100.0).abs() < 1e-6, "{label} target shield leaked");
        assert!((tgt_cs.capacitor - 150.0).abs() < 1e-6, "{label} target cap leaked");
        assert!(!ctx.drain_notifications_for_char(char_id).iter().any(|n| n.event == "OnJumpCloakCancelled" || n.event == "OnInvulnCancelled"));
    }

    // 3. Target Painter (379) on Red safety with insufficient attacker cap fails in add_module_cycle
    //    and MUST clear both projected EWAR and target tp_sig_multiplier.
    crate::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| {
        rt.safety_levels.insert(char_id, crate::services::combat_crimewatch::SafetyLevel::Red);
    });
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&mod_id).unwrap().type_id = tp_tid;
    });
    ctx.invalidate_ship_fitting(ship_id);
    sys.set_combat_state(
        ship_id,
        CombatState {
            is_cloaked: true,
            gate_cloak_remaining_ticks: 30,
            undock_invulnerable_remaining_ticks: 15,
            capacitor: 5.0,
            max_capacitor: 500.0,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let tp_cap_err = handle_module_cycles(
        &ctx,
        &mut sess,
        None,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(mod_id), PyRep::String("activate".into()), PyRep::Integer(victim_ship_id)],
        &kw,
    )
    .await
    .unwrap()
    .expect_err("low-cap Target Painter must reject");
    assert_eq!(tp_cap_err.code, "NotEnoughCapacitor");
    let tgt_after_tp_err = sys.get_combat_state(victim_ship_id).await.unwrap().unwrap();
    assert!(tgt_after_tp_err.tp_sig_multiplier.is_none());
    assert!((ctx.effective_target_painter_multiplier(victim_ship_id) - 1.0).abs() < 1e-9);
    assert!(ctx
        .projected_ewar
        .read()
        .unwrap()
        .get(&victim_ship_id)
        .is_none_or(|v| v.is_empty()));

    // 4. Tractor Beam (650) pre-commit read-only validation rejects out-of-range and wrong-owner
    //    wrecks BEFORE actor commit with zero cap/invuln/gate-cloak mutation and zero break notices.
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&mod_id).unwrap().type_id = tractor_tid;
    });
    ctx.invalidate_ship_fitting(ship_id);
    reset_states().await;
    let _ = ctx.drain_notifications_for_char(char_id);
    let tractor_err = handle_module_cycles(
        &ctx,
        &mut sess,
        None,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(mod_id), PyRep::String("activate".into()), PyRep::Integer(far_wreck_id)],
        &kw,
    )
    .await
    .unwrap()
    .expect_err("out-of-range Tractor Beam must reject before actor commit");
    assert_eq!(tractor_err.code, "TargetOutOfRange");
    let att_after_tractor = sys.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!((att_after_tractor.capacitor - 500.0).abs() < 1e-6);
    assert_eq!(att_after_tractor.undock_invulnerable_remaining_ticks, 15);
    assert!(att_after_tractor.is_cloaked && att_after_tractor.gate_cloak_remaining_ticks == 30);
    assert!(!sys.has_module_cycle(ship_id, mod_id).await);
    assert!(!ctx.drain_notifications_for_char(char_id).iter().any(|n| n.event == "OnJumpCloakCancelled" || n.event == "OnInvulnCancelled"));

    let _ = sys.set_ball_position(far_wreck_id, DVec3::new(1_000.0, 0.0, 0.0)).await;
    let _ = ctx.drain_notifications_for_char(char_id);
    let tractor_owner_err = handle_module_cycles(
        &ctx,
        &mut sess,
        None,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(mod_id), PyRep::String("activate".into()), PyRep::Integer(far_wreck_id)],
        &kw,
    )
    .await
    .unwrap()
    .expect_err("wrong-owner Tractor Beam must reject before actor commit");
    assert_eq!(tractor_owner_err.code, "TractorBeamOwnershipRestricted");
    let att_after_owner = sys.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!((att_after_owner.capacitor - 500.0).abs() < 1e-6);
    assert_eq!(att_after_owner.undock_invulnerable_remaining_ticks, 15);
    assert!(att_after_owner.is_cloaked && att_after_owner.gate_cloak_remaining_ticks == 30);
    assert!(!sys.has_module_cycle(ship_id, mod_id).await);
    assert!(!ctx.drain_notifications_for_char(char_id).iter().any(|n| n.event == "OnJumpCloakCancelled" || n.event == "OnInvulnCancelled"));

    // 5. Smartbomb (72) & Burst Jammer (80) safety blocks BEFORE actor commit, Burst precheck
    //    SafetyBlocked without remove_module_cycle, Smartbomb low-cap re-trigger preserves active cycle,
    //    and valid atomic commit updates cap/cycle/protection + target state cleanly.
    {
        let mut cw = ctx.crimewatch.write().unwrap();
        cw.get_or_create(victim_char_id).suspect_flag_until_unix = 0;
    }
    crate::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| {
        rt.safety_levels.insert(char_id, crate::services::combat_crimewatch::SafetyLevel::Green);
    });
    for (tid, label) in [(smartbomb_tid, "Smartbomb"), (burst_tid, "Burst Jammer")] {
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.get_mut(&mod_id).unwrap().type_id = tid;
        });
        ctx.invalidate_ship_fitting(ship_id);
        reset_states().await;
        let _ = ctx.drain_notifications_for_char(char_id);
        let err = handle_module_cycles(
            &ctx,
            &mut sess,
            None,
            "dogmaIM",
            "Activate",
            &[PyRep::Integer(mod_id)],
            &kw,
        )
        .await
        .unwrap()
        .expect_err("Green highsec AOE safety check must reject before commit");
        assert_eq!(err.code, "CantDoInCurrentSafetyLevel", "{label}");
        let att_cs = sys.get_combat_state(ship_id).await.unwrap().unwrap();
        assert!((att_cs.capacitor - 500.0).abs() < 1e-6);
        assert_eq!(att_cs.undock_invulnerable_remaining_ticks, 15);
        assert!(att_cs.is_cloaked && att_cs.gate_cloak_remaining_ticks == 30);
        assert!(!sys.has_module_cycle(ship_id, mod_id).await);
        assert!(!ctx.drain_notifications_for_char(char_id).iter().any(|n| n.event == "OnJumpCloakCancelled" || n.event == "OnInvulnCancelled"));
    }

    let burst_type_ref = ctx.sde.types.get(&burst_tid).unwrap();
    let burst_state = eve_dogma::DogmaItemState::from_sde(mod_id, burst_tid, &ctx.sde);
    let burst_precheck_err = prepare_burst_jammer_plan(
        &ctx, &sys, sys_id, char_id, ship_id, burst_type_ref, &burst_state, 5.0,
    )
    .await
    .err()
    .expect("prepare_burst_jammer_plan near neutral capsuleer on Green highsec must reject with SafetyBlocked");
    assert_eq!(burst_precheck_err.code, "SafetyBlocked");

    crate::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| {
        rt.safety_levels.insert(char_id, crate::services::combat_crimewatch::SafetyLevel::Red);
    });
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&mod_id).unwrap().type_id = smartbomb_tid;
    });
    ctx.invalidate_ship_fitting(ship_id);
    reset_states().await;
    let _ = ctx.drain_notifications_for_char(char_id);
    handle_module_cycles(
        &ctx,
        &mut sess,
        None,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(mod_id)],
        &kw,
    )
    .await
    .unwrap()
    .expect("Smartbomb first commit on Red safety must succeed");
    assert!(sys.has_module_cycle(ship_id, mod_id).await);
    let sb_ok_cs = sys.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!((sb_ok_cs.capacitor - 475.0).abs() < 1e-6);
    assert_eq!((sb_ok_cs.gate_cloak_remaining_ticks, sb_ok_cs.undock_invulnerable_remaining_ticks), (0, 0));
    let sb_notes = ctx.drain_notifications_for_char(char_id);
    assert_eq!(sb_notes.iter().filter(|n| n.event == "OnJumpCloakCancelled").count(), 1);
    assert_eq!(sb_notes.iter().filter(|n| n.event == "OnInvulnCancelled").count(), 1);

    ctx.module_reactivation_cooldowns.write().unwrap().clear();
    let _ = sys.set_capacitor(ship_id, 5.0).await;
    let sb_retrigger_err = handle_module_cycles(
        &ctx,
        &mut sess,
        None,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(mod_id)],
        &kw,
    )
    .await
    .unwrap()
    .expect_err("Smartbomb low-cap re-trigger must reject atomically");
    assert_eq!(sb_retrigger_err.code, "NotEnoughCapacitor");
    assert!(
        sys.has_module_cycle(ship_id, mod_id).await,
        "failed Smartbomb re-trigger must preserve existing active cycle"
    );
    let _ = sys.remove_module_cycle(ship_id, mod_id).await;

    // 6. Remote Cap Transmitter (67) atomic commit & duplicate/low-cap precheck preservation
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&mod_id).unwrap().type_id = rct_tid;
    });
    ctx.invalidate_ship_fitting(ship_id);
    reset_states().await;
    let _ = ctx.drain_notifications_for_char(char_id);
    handle_module_cycles(
        &ctx,
        &mut sess,
        None,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(mod_id), PyRep::String("activate".into()), PyRep::Integer(victim_ship_id)],
        &kw,
    )
    .await
    .unwrap()
    .expect("Remote Cap Transmitter valid commit must succeed");
    assert!(sys.has_module_cycle(ship_id, mod_id).await);
    assert!((sys.get_combat_state(ship_id).await.unwrap().unwrap().capacitor - 480.0).abs() < 1e-6);
    assert!((sys.get_combat_state(victim_ship_id).await.unwrap().unwrap().capacitor - 210.0).abs() < 1e-6);
    ctx.module_reactivation_cooldowns.write().unwrap().clear();
    let rct_dup_err = handle_module_cycles(
        &ctx,
        &mut sess,
        None,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(mod_id), PyRep::String("activate".into()), PyRep::Integer(victim_ship_id)],
        &kw,
    )
    .await
    .unwrap()
    .expect_err("already-active Remote Cap Transmitter must reject cleanly");
    assert_eq!(rct_dup_err.code, "ModuleAlreadyActive");
    assert!((sys.get_combat_state(victim_ship_id).await.unwrap().unwrap().capacitor - 210.0).abs() < 1e-6);
    let _ = sys.remove_module_cycle(ship_id, mod_id).await;
}

#[tokio::test]
async fn bf4_p2_non_gate_undock_invuln_and_light_cyno_fuel_rollback() {
    let web_tid = 995_065_u32;
    let tsb_tid = 995_154_u32;
    let cyno_hull_tid = 995_310_u32;
    let cyno_mod_tid = 21_096_u32;
    let low_sys_id = 30_002_505_u32;

    let ctx = test_ctx_with_custom(|sde| {
        let mut low_sys = sde.solar_systems.get(&30_000_142).expect("seed sys").clone();
        low_sys.solar_system_id = low_sys_id;
        low_sys.security_status = -0.5;
        sde.solar_systems.insert(low_sys_id, low_sys);

        let mut web_t = custom_inv_type(
            sde,
            web_tid,
            65,
            7,
            5.0,
            0.0,
            "Test Stasis Webifier I",
            &[(54, 10_000.0), (20, -50.0), (73, 5_000.0), (6, 25.0)],
        );
        web_t.effect_ids = vec![13, 16];
        sde.types.insert(web_tid, web_t);

        let mut tsb_t = custom_inv_type(
            sde,
            tsb_tid,
            1154,
            7,
            5.0,
            0.0,
            "Test Target Spectrum Breaker",
            &[(6, 10.0), (73, 10_000.0)],
        );
        tsb_t.effect_ids = vec![13, 16];
        sde.types.insert(tsb_tid, tsb_t);

        let hull_t = custom_inv_type(
            sde,
            cyno_hull_tid,
            27,
            6,
            500.0,
            1_000.0,
            "Test Cyno Battleship",
            &[
                (attr_ids::HI_SLOTS, 8.0),
                (attr_ids::MED_SLOTS, 8.0),
                (attr_ids::CPU_OUTPUT, 10_000.0),
                (attr_ids::POWER_OUTPUT, 100_000.0),
                (attr_ids::CAPACITOR_CAPACITY, 5_000.0),
                (attr_ids::MAX_LOCKED_TARGETS, 7.0),
                (attr_ids::MAX_TARGET_RANGE, 100_000.0),
            ],
        );
        sde.types.insert(cyno_hull_tid, hull_t);

        let mut cyno_t = custom_inv_type(
            sde,
            cyno_mod_tid,
            658,
            7,
            20.0,
            0.0,
            "Cynosural Field Generator I",
            &[(713, 16273.0), (714, 500.0), (73, 600_000.0), (6, 500.0)],
        );
        cyno_t.effect_ids = vec![12, 16];
        sde.types.insert(cyno_mod_tid, cyno_t);
    });

    let char_id = 97_995_001_i64;
    let victim_char_id = 97_995_002_i64;
    let fleet_id = 777_995_i64;
    let ship_id = 1_097_995_001_i64;
    let ship_no_cloak_id = 1_097_995_011_i64;
    let victim_ship_id = 1_097_995_002_i64;
    let web_id = 1_097_995_003_i64;
    let tsb_id = 1_097_995_004_i64;
    let cyno_mod_id = 1_097_995_005_i64;
    let cyno_mod_nc_id = 1_097_995_015_i64;
    let ozone_id = 1_097_995_006_i64;
    let ozone_nc_id = 1_097_995_016_i64;

    ensure_char(&ctx, char_id, 98_000_001, low_sys_id, ship_id);
    ensure_char(&ctx, victim_char_id, 98_000_002, low_sys_id, victim_ship_id);
    let _ = ctx.db.acquire_online_lock(char_id as u64, 1, Some(char_id), true, ctx.now_unix());
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().fleet_id = Some(fleet_id);
        snap.fleets.insert(
            fleet_id,
            eve_db::FleetRecord {
                fleet_id,
                boss_id: char_id,
                members: vec![char_id, victim_char_id],
                ..Default::default()
            },
        );
    });

    insert_item(&ctx, ship_id, cyno_hull_tid, char_id, i64::from(low_sys_id), InventoryFlag::None, 1, true);
    insert_item(&ctx, ship_no_cloak_id, cyno_hull_tid, char_id, i64::from(low_sys_id), InventoryFlag::None, 1, true);
    insert_item(&ctx, victim_ship_id, 587, victim_char_id, i64::from(low_sys_id), InventoryFlag::None, 1, true);
    insert_item(&ctx, web_id, web_tid, char_id, ship_id, InventoryFlag::MedSlot0, 1, true);
    insert_item(&ctx, tsb_id, tsb_tid, char_id, ship_id, InventoryFlag::MedSlot1, 1, true);
    insert_item(&ctx, cyno_mod_id, cyno_mod_tid, char_id, ship_id, InventoryFlag::HiSlot0, 1, true);
    insert_item(&ctx, cyno_mod_nc_id, cyno_mod_tid, char_id, ship_no_cloak_id, InventoryFlag::HiSlot0, 1, true);
    ctx.db.with_mut_snapshot(|snap| {
        for mid in [web_id, tsb_id, cyno_mod_id, cyno_mod_nc_id] {
            snap.module_online_states.insert(mid, true);
        }
    });
    ctx.invalidate_ship_fitting(ship_id);
    ctx.invalidate_ship_fitting(ship_no_cloak_id);

    for sid in [ship_id, ship_no_cloak_id] {
        spawn_ship(&ctx, low_sys_id, sid, cyno_hull_tid, char_id, 5_000.0).await;
    }
    let sys = ctx.systems.get_or_spawn(low_sys_id);
    sys.spawn_ball_with_meta(
        Ball {
            id: victim_ship_id,
            type_id: 587,
            position: DVec3::new(1_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 500.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 100.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
        CombatState {
            capacitor: 200.0,
            max_capacitor: 200.0,
            ..Default::default()
        },
        SlimItemMeta {
            group_id: 25,
            category_id: 6,
            owner_id: victim_char_id,
            char_id: Some(victim_char_id),
            corp_id: 98_000_002,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    sys.add_target_lock(
        ship_id,
        eve_destiny::TargetLock {
            target_id: victim_ship_id,
            lock_time_ms: 100.0,
            remaining_lock_ms: 0.0,
            locked: true,
            max_range_m: Some(50_000.0),
        },
    )
    .await
    .unwrap();

    let mut sess = SessionState {
        user_id: Some(char_id as u64),
        character_id: Some(char_id),
        corporation_id: Some(98_000_001),
        solar_system_id: Some(low_sys_id),
        ship_id: Some(ship_id),
        ..Default::default()
    };
    let kw = HashMap::new();

    // 1. Undocked without gate cloak (gate_cloak=0, undock_invuln=15):
    //    Commit via non-delegated Web (fitting_module_cycles:2077) clears undock_invuln and emits OnInvulnCancelled (no OnJumpCloakCancelled).
    crate::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| {
        rt.safety_levels.insert(char_id, crate::services::combat_crimewatch::SafetyLevel::Red);
    });
    sys.set_combat_state(
        ship_id,
        CombatState {
            is_cloaked: false,
            gate_cloak_remaining_ticks: 0,
            undock_invulnerable_remaining_ticks: 15,
            capacitor: 5_000.0,
            max_capacitor: 5_000.0,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let _ = ctx.drain_notifications_for_char(char_id);
    handle_module_cycles(
        &ctx,
        &mut sess,
        None,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(web_id), PyRep::String("activate".into()), PyRep::Integer(victim_ship_id)],
        &kw,
    )
    .await
    .unwrap()
    .expect("non-gate undock invuln Web commit must succeed");
    let cs_web = sys.get_combat_state(ship_id).await.unwrap().unwrap();
    assert_eq!(cs_web.undock_invulnerable_remaining_ticks, 0);
    let notes_web = ctx.drain_notifications_for_char(char_id);
    assert_eq!(notes_web.iter().filter(|n| n.event == "OnInvulnCancelled").count(), 1);
    assert!(!notes_web.iter().any(|n| n.event == "OnJumpCloakCancelled"));

    // 2. Undocked without gate cloak via delegated finish_space_activation / break_gate_cloak_on_space_action
    //    clears undock_invuln and emits single OnInvulnCancelled.
    sys.set_combat_state(
        ship_id,
        CombatState {
            is_cloaked: false,
            gate_cloak_remaining_ticks: 0,
            undock_invulnerable_remaining_ticks: 15,
            capacitor: 5_000.0,
            max_capacitor: 5_000.0,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let _ = ctx.drain_notifications_for_char(char_id);
    handle_module_cycles(&ctx, &mut sess, None, "dogmaIM", "Activate", &[PyRep::Integer(tsb_id)], &kw)
        .await
        .unwrap()
        .expect("delegated TSB commit with non-gate undock invuln must succeed");
    let cs_tsb = sys.get_combat_state(ship_id).await.unwrap().unwrap();
    assert_eq!(cs_tsb.undock_invulnerable_remaining_ticks, 0);
    let notes_tsb = ctx.drain_notifications_for_char(char_id);
    assert_eq!(notes_tsb.iter().filter(|n| n.event == "OnInvulnCancelled").count(), 1);
    assert!(!notes_tsb.iter().any(|n| n.event == "OnJumpCloakCancelled"));

    // 3. light_cyno with NotEnoughFuel preserves gate cloak and undock invuln (no notifications, no cycle, no beacon);
    //    lighting with Liquid Ozone clears undock_invuln + gate cloak and emits single OnInvulnCancelled + OnJumpCloakCancelled.
    sys.set_combat_state(
        ship_id,
        CombatState {
            is_cloaked: true,
            gate_cloak_remaining_ticks: 30,
            undock_invulnerable_remaining_ticks: 15,
            capacitor: 5_000.0,
            max_capacitor: 5_000.0,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let _ = ctx.drain_notifications_for_char(char_id);
    let cyno_no_fuel = handle_module_cycles(
        &ctx,
        &mut sess,
        None,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(cyno_mod_id)],
        &kw,
    )
    .await
    .unwrap()
    .expect_err("light_cyno without Liquid Ozone must fail NotEnoughFuel");
    assert_eq!(cyno_no_fuel.code, "NotEnoughFuel");
    let cs_cyno_err = sys.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!(cs_cyno_err.is_cloaked);
    assert_eq!(cs_cyno_err.gate_cloak_remaining_ticks, 30);
    assert_eq!(cs_cyno_err.undock_invulnerable_remaining_ticks, 15);
    assert!(!sys.has_module_cycle(ship_id, cyno_mod_id).await);
    assert!(!crate::services::inventory_fitting::fitting_cyno_portal::is_ship_cyno_active(&ctx, ship_id));
    assert!(!ctx.drain_notifications_for_char(char_id).iter().any(|n| n.event == "OnInvulnCancelled" || n.event == "OnJumpCloakCancelled"));

    insert_item(&ctx, ozone_id, 16_273, char_id, ship_id, InventoryFlag::Cargo, 1_000, false);
    handle_module_cycles(&ctx, &mut sess, None, "dogmaIM", "Activate", &[PyRep::Integer(cyno_mod_id)], &kw)
        .await
        .unwrap()
        .expect("light_cyno with fuel must succeed");
    let cs_cyno_ok = sys.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!(!cs_cyno_ok.is_cloaked);
    assert_eq!(cs_cyno_ok.gate_cloak_remaining_ticks, 0);
    assert_eq!(cs_cyno_ok.undock_invulnerable_remaining_ticks, 0);
    let notes_cyno = ctx.drain_notifications_for_char(char_id);
    assert_eq!(notes_cyno.iter().filter(|n| n.event == "OnInvulnCancelled").count(), 1);
    assert!(notes_cyno.iter().any(|n| n.event == "OnJumpCloakCancelled"));

    // 4. Same light_cyno fuel rejection + commit for non-gate undock invuln (gate_cloak = 0, undock_invuln = 15).
    sess.ship_id = Some(ship_no_cloak_id);
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().active_ship_id = ship_no_cloak_id;
    });
    sys.set_combat_state(
        ship_no_cloak_id,
        CombatState {
            is_cloaked: false,
            gate_cloak_remaining_ticks: 0,
            undock_invulnerable_remaining_ticks: 15,
            capacitor: 5_000.0,
            max_capacitor: 5_000.0,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let _ = ctx.drain_notifications_for_char(char_id);
    let nc_err = handle_module_cycles(&ctx, &mut sess, None, "dogmaIM", "Activate", &[PyRep::Integer(cyno_mod_nc_id)], &kw)
        .await
        .unwrap()
        .expect_err("non-gate light_cyno without fuel must fail NotEnoughFuel");
    assert_eq!(nc_err.code, "NotEnoughFuel");
    assert_eq!(sys.get_combat_state(ship_no_cloak_id).await.unwrap().unwrap().undock_invulnerable_remaining_ticks, 15);
    assert!(!ctx.drain_notifications_for_char(char_id).iter().any(|n| n.event == "OnInvulnCancelled"));

    insert_item(&ctx, ozone_nc_id, 16_273, char_id, ship_no_cloak_id, InventoryFlag::Cargo, 1_000, false);
    handle_module_cycles(&ctx, &mut sess, None, "dogmaIM", "Activate", &[PyRep::Integer(cyno_mod_nc_id)], &kw)
        .await
        .unwrap()
        .expect("non-gate light_cyno with fuel must succeed");
    assert_eq!(sys.get_combat_state(ship_no_cloak_id).await.unwrap().unwrap().undock_invulnerable_remaining_ticks, 0);
    let notes_nc_ok = ctx.drain_notifications_for_char(char_id);
    assert_eq!(notes_nc_ok.iter().filter(|n| n.event == "OnInvulnCancelled").count(), 1);
    assert!(!notes_nc_ok.iter().any(|n| n.event == "OnJumpCloakCancelled"));
}

#[tokio::test]
async fn bf4_charge_skill_prerequisite_clears_projected_ewar_and_tp_sig_multiplier() {
    let tp_tid = 996_479_u32;
    let script_tid = 996_909_u32;
    let missing_skill_tid = 999_001_u32;

    let ctx = test_ctx_with_custom(|sde| {
        let mut tp_t = custom_inv_type(
            sde,
            tp_tid,
            379,
            7,
            5.0,
            5.0,
            "Test Scripted Target Painter I",
            &[
                (6, 20.0),
                (54, 20_000.0),
                (73, 5_000.0),
                (554, 30.0),
                (604, 909.0),
                (2044, 10_000.0),
            ],
        );
        tp_t.effect_ids = vec![13, 16];
        sde.types.insert(tp_tid, tp_t);

        let script_t = custom_inv_type(
            sde,
            script_tid,
            909,
            8,
            1.0,
            0.0,
            "Test Untrained Targeting Script",
            &[(182, f64::from(missing_skill_tid)), (277, 1.0)],
        );
        sde.types.insert(script_tid, script_t);
    });

    let char_id = 97_994_201_i64;
    let victim_char_id = 97_994_202_i64;
    let sys_id = 30_000_142_u32;
    let ship_id = 1_097_994_201_i64;
    let victim_ship_id = 1_097_994_202_i64;
    let tp_id = 1_097_994_203_i64;
    let script_id = 1_097_994_204_i64;

    ensure_char(&ctx, char_id, 98_000_001, sys_id, ship_id);
    ensure_char(&ctx, victim_char_id, 98_000_002, sys_id, victim_ship_id);
    let _ = ctx.db.acquire_online_lock(char_id as u64, 1, Some(char_id), true, ctx.now_unix());
    insert_item(&ctx, ship_id, 587, char_id, i64::from(sys_id), InventoryFlag::None, 1, true);
    insert_item(&ctx, victim_ship_id, 587, victim_char_id, i64::from(sys_id), InventoryFlag::None, 1, true);
    insert_item(&ctx, tp_id, tp_tid, char_id, ship_id, InventoryFlag::MedSlot0, 1, true);
    insert_item(&ctx, script_id, script_tid, char_id, ship_id, InventoryFlag::MedSlot0, 1, false);
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(tp_id, true);
    });
    spawn_ship(&ctx, sys_id, ship_id, 587, char_id, 500.0).await;
    let sys = ctx.systems.get_or_spawn(sys_id);
    sys.spawn_ball_with_meta(
        Ball {
            id: victim_ship_id,
            type_id: 587,
            position: DVec3::new(1_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 500.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 100.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
        CombatState {
            signature_radius: 40.0,
            base_signature_radius: Some(40.0),
            ..Default::default()
        },
        SlimItemMeta {
            group_id: 25,
            category_id: 6,
            owner_id: victim_char_id,
            char_id: Some(victim_char_id),
            corp_id: 98_000_002,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    sys.add_target_lock(
        ship_id,
        eve_destiny::TargetLock {
            target_id: victim_ship_id,
            lock_time_ms: 100.0,
            remaining_lock_ms: 0.0,
            locked: true,
            max_range_m: None,
        },
    )
    .await
    .unwrap();
    sys.set_combat_state(
        ship_id,
        CombatState {
            is_cloaked: true,
            gate_cloak_remaining_ticks: 30,
            undock_invulnerable_remaining_ticks: 15,
            capacitor: 500.0,
            max_capacitor: 500.0,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    crate::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| {
        rt.safety_levels.insert(char_id, crate::services::combat_crimewatch::SafetyLevel::Red);
    });

    let mut sess = SessionState {
        user_id: Some(char_id as u64),
        character_id: Some(char_id),
        corporation_id: Some(98_000_001),
        solar_system_id: Some(sys_id),
        ship_id: Some(ship_id),
        ..Default::default()
    };
    let kw = HashMap::new();
    let _ = ctx.drain_notifications_for_char(char_id);
    let err = handle_module_cycles(
        &ctx,
        &mut sess,
        None,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(tp_id), PyRep::String("activate".into()), PyRep::Integer(victim_ship_id)],
        &kw,
    )
    .await
    .unwrap()
    .expect_err("loaded charge lacking trained skill prerequisite must reject");
    assert_eq!(err.code, "SkillPrerequisitesNotMet");

    let tgt_after = sys.get_combat_state(victim_ship_id).await.unwrap().unwrap();
    assert!(tgt_after.tp_sig_multiplier.is_none());
    assert!((ctx.effective_target_painter_multiplier(victim_ship_id) - 1.0).abs() < 1e-9);
    assert!(ctx
        .projected_ewar
        .read()
        .unwrap()
        .get(&victim_ship_id)
        .is_none_or(|v| v.is_empty()));
    let att_after = sys.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!(att_after.is_cloaked);
    assert_eq!(att_after.gate_cloak_remaining_ticks, 30);
    assert_eq!(att_after.undock_invulnerable_remaining_ticks, 15);
    assert!(!sys.has_module_cycle(ship_id, tp_id).await);
    assert!(!ctx.drain_notifications_for_char(char_id).iter().any(|n| n.event == "OnJumpCloakCancelled" || n.event == "OnInvulnCancelled"));
}



