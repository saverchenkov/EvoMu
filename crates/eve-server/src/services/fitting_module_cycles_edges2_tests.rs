//! Additional edge-case and branch coverage tests for `services::fitting_module_cycles`.

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
    Arc::clone(SDE.get_or_init(|| {
        if std::env::var_os("EVOMU_SDE_PATH").is_some() {
            Arc::new(ServerContext::new().sde.as_ref().clone())
        } else {
            Arc::new(SdeRegistry::with_canonical_seed())
        }
    }))
}

fn custom_inv_type(
    sde: &SdeRegistry,
    type_id: u32,
    group_id: u32,
    category_id: u32,
    name: &str,
    attrs: &[(u16, f64)],
    effect_ids: &[u16],
) -> InvType {
    let mut t = sde.types.get(&587).expect("seed type").clone();
    t.type_id = type_id;
    t.group_id = group_id;
    t.category_id = category_id;
    t.volume = 5.0;
    t.capacity = 100.0;
    t.name = name.into();
    t.effect_ids = effect_ids.to_vec();
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
    let ctx = ServerContext::with_sde_and_repository(Arc::new(sde), db)
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_800_000_000)));
    ctx
}

fn setup_pilot_and_ship(ctx: &ServerContext, char_id: i64, sys_id: u32, ship_id: i64) -> SessionState {
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().expect("seed char");
        ch.character_id = char_id;
        ch.user_id = char_id as u64;
        ch.name = format!("ModEdgePilot {char_id}");
        ch.corporation_id = 1_000_009;
        ch.station_id = None;
        ch.structure_id = None;
        ch.solar_system_id = sys_id;
        ch.active_ship_id = ship_id;
        ch.security_status = 5.0;
        snap.characters.insert(char_id, ch);

        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 999_100, // Custom 6-hi/4-med frigate hull
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });

    let mut sess = SessionState::new(9850);
    sess.user_id = Some(char_id as u64);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(1_000_009);
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(ship_id);
    sess
}

async fn insert_ship_ball(ctx: &ServerContext, sys_id: u32, ship_id: i64, pos: DVec3) {
    let sys = ctx.systems.get_or_spawn(sys_id);
    let _ = sys
        .spawn_ball_with_meta(
            Ball {
                id: ship_id,
                type_id: 587,
                position: pos,
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState::default(),
            SlimItemMeta {
                owner_id: 1_000_009,
                group_id: 25,
                category_id: 6,
                name: Some("EdgeShip".into()),
                ..Default::default()
            },
        )
        .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn activate_auth_offline_burnout_nanite_reactivation_and_overload_edges() {
    let custom_gun_tid = 999_101_u32;
    let custom_cloak_tid = 999_102_u32;
    let custom_lance_tid = 999_103_u32;
    let custom_sb_tid = 999_104_u32;
    let custom_sal_tid = 999_105_u32;
    let custom_mwd_tid = 999_106_u32;
    let custom_rep_tid = 999_107_u32;

    let ctx = test_ctx_with_custom(|sde| {
        let mut custom_hull = sde.types.get(&587).expect("seed type").clone();
        custom_hull.type_id = 999_100;
        custom_hull.name = "Test Frigate Hull".into();
        custom_hull.default_attributes.insert(eve_sde::attr_ids::HI_SLOTS, 6.0);
        custom_hull.default_attributes.insert(eve_sde::attr_ids::MED_SLOTS, 4.0);
        sde.types.insert(999_100, custom_hull);

        let covert_eid = sde
            .effects
            .values()
            .find(|e| e.name == "cloakingWarpSafe")
            .map(|e| e.effect_id)
            .unwrap_or_else(|| {
                let mut eff = sde
                    .effects
                    .values()
                    .next()
                    .cloned()
                    .expect("seed effects");
                eff.effect_id = 980;
                eff.name = "cloakingWarpSafe".into();
                eff.is_warp_safe = true;
                sde.effects.insert(980, eff);
                980
            });

        sde.types.insert(
            custom_gun_tid,
            custom_inv_type(
                sde,
                custom_gun_tid,
                55, // Projectile Weapon
                7,
                "Test Autocannon",
                &[(6, 10.0), (51, 2000.0), (eve_sde::attr_ids::HP_STRUCT, 40.0)],
                &[12],
            ),
        );
        sde.types.insert(
            custom_cloak_tid,
            custom_inv_type(
                sde,
                custom_cloak_tid,
                330, // Cloaking Device
                7,
                "Test Covert Cloak",
                &[(6, 0.0), (51, 2000.0), (1034, 1.0), (eve_sde::attr_ids::HP_STRUCT, 40.0)], // attr 1034 covert cloak
                &[12, covert_eid],
            ),
        );
        sde.types.insert(
            custom_lance_tid,
            custom_inv_type(
                sde,
                custom_lance_tid,
                588, // Disruptive Lance / Superweapon
                7,
                "Test Lance",
                &[(6, 100.0), (51, 5000.0), (eve_sde::attr_ids::HP_STRUCT, 40.0)],
                &[12],
            ),
        );
        sde.types.insert(
            custom_sb_tid,
            custom_inv_type(
                sde,
                custom_sb_tid,
                72, // Smart Bomb
                7,
                "Test Smartbomb",
                &[(6, 20.0), (51, 4000.0), (99, 5000.0), (eve_sde::attr_ids::HP_STRUCT, 40.0)],
                &[12],
            ),
        );
        sde.types.insert(
            custom_sal_tid,
            custom_inv_type(
                sde,
                custom_sal_tid,
                1122, // Salvager
                7,
                "Test Salvager",
                &[(6, 10.0), (51, 5000.0), (eve_sde::attr_ids::HP_STRUCT, 40.0)],
                &[13],
            ),
        );
        sde.types.insert(
            custom_mwd_tid,
            custom_inv_type(
                sde,
                custom_mwd_tid,
                46, // Propulsion Module
                7,
                "Test Microwarpdrive",
                &[(6, 20.0), (51, 5000.0), (20, 500.0), (554, 500.0), (eve_sde::attr_ids::HP_STRUCT, 40.0)],
                &[13, 6730],
            ),
        );
        sde.types.insert(
            custom_rep_tid,
            custom_inv_type(
                sde,
                custom_rep_tid,
                41, // Remote Shield Booster
                7,
                "Test Remote Rep",
                &[(6, 20.0), (51, 5000.0), (eve_sde::attr_ids::HP_STRUCT, 40.0)],
                &[13],
            ),
        );
        sde.types.insert(
            999_108,
            custom_inv_type(
                sde,
                999_108,
                83, // Projectile Ammo
                8,  // Charge category
                "Test Projectile Ammo",
                &[],
                &[],
            ),
        );
    });

    let char_id = 96_800_001_i64;
    let sys_id = 30_000_142_u32;
    let ship_id = 96_800_100_i64;
    let gun_id = 96_800_101_i64;
    let offline_id = 96_800_102_i64;
    let burned_id = 96_800_103_i64;
    let cloak_id = 96_800_104_i64;
    let lance_id = 96_800_105_i64;
    let sb_id = 96_800_106_i64;
    let sal_id = 96_800_107_i64;
    let mwd_id = 96_800_108_i64;
    let rep_id = 96_800_109_i64;
    let target_id = 96_800_200_i64;
    let kw = HashMap::new();

    let mut sess = setup_pilot_and_ship(&ctx, char_id, sys_id, ship_id);
    insert_ship_ball(&ctx, sys_id, ship_id, DVec3::ZERO).await;
    insert_ship_ball(&ctx, sys_id, target_id, DVec3::new(1000.0, 0.0, 0.0)).await;

    // 1. NoActiveShip & NoSolarSystem
    let mut no_ship = sess.clone();
    no_ship.ship_id = None;
    assert_eq!(
        handle_module_cycles(&ctx, &mut no_ship, None, "dogmaIM", "Activate", &[PyRep::Integer(gun_id), PyRep::String("online".into())], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "NoActiveShip"
    );
    let mut no_sys = sess.clone();
    no_sys.solar_system_id = None;
    assert_eq!(
        handle_module_cycles(&ctx, &mut no_sys, None, "dogmaIM", "Activate", &[PyRep::Integer(gun_id), PyRep::String("activate".into())], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "NoSolarSystem"
    );

    // 2. ModuleNotFound vs online effect pass-through when raw_module_id > 0 (weapon bank synthetic id)
    assert_eq!(
        handle_module_cycles(&ctx, &mut sess, None, "dogmaIM", "Activate", &[PyRep::Integer(99_999_999), PyRep::String("activate".into())], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "ModuleNotFound"
    );
    assert_eq!(
        handle_module_cycles(&ctx, &mut sess, None, "dogmaIM", "Activate", &[PyRep::Integer(99_999_999), PyRep::String("online".into())], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "ModuleNotFound"
    );

    // Seed modules on ship
    ctx.db.with_mut_snapshot(|snap| {
        for (id, tid, flag, online, dmg) in [
            (gun_id, custom_gun_tid, InventoryFlag::HiSlot0, true, 0.0),
            (offline_id, custom_gun_tid, InventoryFlag::HiSlot1, false, 0.0),
            (burned_id, custom_gun_tid, InventoryFlag::HiSlot2, true, 99.0),
            (cloak_id, custom_cloak_tid, InventoryFlag::HiSlot3, true, 0.0),
            (lance_id, custom_lance_tid, InventoryFlag::HiSlot4, true, 0.0),
            (sb_id, custom_sb_tid, InventoryFlag::HiSlot5, true, 0.0),
            (sal_id, custom_sal_tid, InventoryFlag::MedSlot0, true, 0.0),
            (mwd_id, custom_mwd_tid, InventoryFlag::MedSlot1, true, 0.0),
            (rep_id, custom_rep_tid, InventoryFlag::MedSlot2, true, 0.0),
        ] {
            let mut attrs = HashMap::new();
            if dmg > 0.0 {
                attrs.insert(attr_ids::DAMAGE, dmg);
            }
            snap.module_online_states.insert(id, online);
            snap.items.insert(
                id,
                InventoryItemRecord {
                    item_id: id,
                    type_id: tid,
                    owner_id: char_id,
                    location_id: ship_id,
                    flag,
                    quantity: 1,
                    singleton: true,
                    dynamic_attributes: attrs,
                    version: 1,
                    ..Default::default()
                },
            );
        }
    });
    ctx.invalidate_ship_fitting(ship_id);

    // 3. Offline module & burned out module
    assert_eq!(
        handle_module_cycles(&ctx, &mut sess, None, "dogmaIM", "Activate", &[PyRep::Integer(offline_id), PyRep::String("activate".into())], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "ModuleOffline"
    );
    assert_eq!(
        handle_module_cycles(&ctx, &mut sess, None, "dogmaIM", "Activate", &[PyRep::Integer(burned_id), PyRep::String("activate".into())], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "ModuleBurnedOut"
    );

    // 4. Online effect on fitted module returns online state KeyVal
    sess.station_id = Some(60_003_760);
    let online_res = handle_module_cycles(&ctx, &mut sess, None, "dogmaIM", "Activate", &[PyRep::Integer(gun_id), PyRep::String("online".into())], &kw)
        .await
        .unwrap()
        .unwrap();
    sess.station_id = None;
    assert_eq!(online_res.dict_get("online"), Some(&PyRep::Bool(true)));
    assert_eq!(online_res.dict_get("moduleID"), Some(&PyRep::Integer(gun_id)));

    // 5. Salvager requires target & unlocked target rejected; remote rep self-target rejected
    assert_eq!(
        handle_module_cycles(&ctx, &mut sess, None, "dogmaIM", "Activate", &[PyRep::Integer(sal_id), PyRep::String("activate".into())], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "TargetRequired"
    );
    assert_eq!(
        handle_module_cycles(&ctx, &mut sess, None, "dogmaIM", "Activate", &[PyRep::Integer(rep_id), PyRep::String("activate".into()), PyRep::Integer(ship_id)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "CannotActivateOnSelf"
    );
    assert_eq!(
        handle_module_cycles(&ctx, &mut sess, None, "dogmaIM", "Activate", &[PyRep::Integer(sal_id), PyRep::String("activate".into()), PyRep::Integer(target_id)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "TargetNotLocked"
    );

    // 6. Smartbomb in highsec without Red safety -> CantDoInCurrentSafetyLevel
    crate::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| {
        rt.safety_levels
            .insert(char_id, crate::services::combat_crimewatch::SafetyLevel::Green);
    });
    assert_eq!(
        handle_module_cycles(&ctx, &mut sess, None, "dogmaIM", "Activate", &[PyRep::Integer(sb_id), PyRep::String("activate".into())], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "CantDoInCurrentSafetyLevel"
    );

    // 7. Covert cloak with Lance fitted -> CannotCloakWithLanceFitted (or ShipCannotUseCovertCloak after lance removed)
    assert_eq!(
        handle_module_cycles(&ctx, &mut sess, None, "dogmaIM", "Activate", &[PyRep::Integer(cloak_id), PyRep::String("activate".into())], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "CannotCloakWithLanceFitted"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.remove(&lance_id);
    });
    ctx.invalidate_ship_fitting(ship_id);
    assert_eq!(
        handle_module_cycles(&ctx, &mut sess, None, "dogmaIM", "Activate", &[PyRep::Integer(cloak_id), PyRep::String("activate".into())], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "ShipCannotUseCovertCloak"
    );

    // 8. Tethered offensive gun rejected; MWD disabled while scrammed; NotEnoughCapacitor when drained
    let sys = ctx.systems.get_or_spawn(sys_id);
    let _ = sys.try_update_combat_state(ship_id, |cs| cs.is_tethered = true);
    assert_eq!(
        handle_module_cycles(&ctx, &mut sess, None, "dogmaIM", "Activate", &[PyRep::Integer(gun_id), PyRep::String("activate".into()), PyRep::Integer(target_id)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "CantActivateWhileTethered"
    );
    let _ = sys.set_warp_scramble_strength(ship_id, 2).await;
    let _ = sys.try_update_combat_state(ship_id, |cs| {
        cs.is_tethered = false;
        cs.is_mwd_mjd_disabled = true;
    });
    assert_eq!(
        handle_module_cycles(&ctx, &mut sess, None, "dogmaIM", "Activate", &[PyRep::Integer(mwd_id), PyRep::String("activate".into())], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "WarpScrambled"
    );

    // Drain capacitor and clear scram -> NotEnoughCapacitor on MWD
    let _ = sys.set_warp_scramble_strength(ship_id, 0).await;
    let _ = sys.try_update_combat_state(ship_id, |cs| cs.is_mwd_mjd_disabled = false);
    let _ = sys.set_capacitor(ship_id, 0.0).await;
    assert_eq!(
        handle_module_cycles(&ctx, &mut sess, None, "dogmaIM", "Activate", &[PyRep::Integer(mwd_id), PyRep::String("activate".into())], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "NotEnoughCapacitor"
    );

    // 9. Overload toggle (enable and disable) + nanite repair blocks online & Activate
    let ov_on = handle_module_cycles(
        &ctx,
        &mut sess,
        None,
        "dogmaIM",
        "Overload",
        &[PyRep::Integer(gun_id)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(ov_on.dict_get("overloaded").and_then(|v| v.as_bool()), Some(true));

    let ov_off = crate::services::inventory_fitting::fitting_mining_lasers::handle_mining_and_deactivate(
        &ctx,
        &mut sess,
        None,
        "dogmaIM",
        "StopOverload",
        &[PyRep::Integer(gun_id)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(ov_off.dict_get("overloaded").and_then(|v| v.as_bool()), Some(false));

    // Nanite repair block
    let _ = sys
        .start_nanite_repair(crate::system_actor::ActiveNaniteRepair {
            char_id,
            ship_id,
            module_id: gun_id,
            initial_damage_hp: 1.0,
            paste_total_needed: 1,
            paste_consumed_so_far: 0,
            duration_s: 10.0,
            elapsed_s: 0.0,
        })
        .await;
    assert_eq!(
        handle_module_cycles(&ctx, &mut sess, None, "dogmaIM", "Activate", &[PyRep::Integer(gun_id), PyRep::String("online".into())], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "ModuleBeingRepaired"
    );
    assert_eq!(
        handle_module_cycles(&ctx, &mut sess, None, "dogmaIM", "Activate", &[PyRep::Integer(gun_id), PyRep::String("activate".into())], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "ModuleBeingRepaired"
    );

    // 10. Cloak in Abyssal space (32_000_001) -> CannotCloakInAbyssalSpace
    let abyss_sys = 32_000_001_u32;
    let abyss_ship = 96_800_300_i64;
    let abyss_cloak = 96_800_301_i64;
    let mut abyss_sess = setup_pilot_and_ship(&ctx, char_id + 1, abyss_sys, abyss_ship);
    insert_ship_ball(&ctx, abyss_sys, abyss_ship, DVec3::ZERO).await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(abyss_cloak, true);
        snap.items.insert(
            abyss_cloak,
            InventoryItemRecord {
                item_id: abyss_cloak,
                type_id: custom_cloak_tid,
                owner_id: char_id + 1,
                location_id: abyss_ship,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    assert_eq!(
        handle_module_cycles(&ctx, &mut abyss_sess, None, "dogmaIM", "Activate", &[PyRep::Integer(abyss_cloak), PyRep::String("activate".into())], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "CannotCloakInAbyssalSpace"
    );

    // 11. GH-896: Weapon charge consumption on real `step_manual` -> `execute_pending_cycle_actions`
    // emits `CycleChargeItemMutations`, producing both `OnItemChange` and `OnModuleAttributeChanges` (805).
    let charge_id = 96_800_401_i64;
    let seed_uid = ctx.db.with_snapshot(|snap| {
        snap.accounts.values().next().expect("seed account").user_id
    });
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.user_id = seed_uid;
        }
        snap.module_active_states.insert(gun_id);
        snap.items.insert(
            charge_id,
            InventoryItemRecord {
                item_id: charge_id,
                type_id: 999_108, // Custom Projectile Ammo (category 8)
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 10,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });
    let _ = ctx
        .db
        .acquire_online_lock(seed_uid, 9850, Some(char_id), true, 1_000);
    let _ = sys
        .add_module_cycle(crate::system_actor::ActiveModuleCycle {
            ship_id,
            owner_char_id: Some(char_id),
            module_id: gun_id,
            module_type_id: custom_gun_tid,
            target_id: Some(target_id),
            effect_name: "projectileFired".into(),
            duration_s: 2.0,
            elapsed_s: 0.0,
            repeat: 1,
            linked_turrets: vec![gun_id],
            ammo_type_id: Some(999_108),
            crystal_volatility_chance: 0.0,
            crystal_volatility_damage: 0.0,
            weapon_is_turret: true,
            ..Default::default()
        })
        .await;
    let _ = ctx.drain_notifications(char_id);
    sys.step_manual(2.1).await.unwrap();
    execute_pending_cycle_actions(&ctx, sys_id).await;
    let notifs = ctx.drain_notifications(char_id);
    assert!(
        notifs.iter().any(|n| n.event == "OnItemChange"),
        "expected OnItemChange on weapon charge cycle consumption"
    );
    assert!(
        notifs.iter().any(|n| n.event == "OnModuleAttributeChanges"),
        "expected OnModuleAttributeChanges (805) on weapon charge cycle consumption"
    );
}

fn count_attr805_changes_for_flag(
    notifs: &[crate::router::PendingMachoNotification],
    ship_id: i64,
    flag: InventoryFlag,
) -> Vec<(f64, f64)> {
    let mut out = Vec::new();
    for n in notifs {
        if n.event != "OnModuleAttributeChanges" {
            continue;
        }
        if let Some(PyRep::List(entries)) = n.args.first() {
            for entry in entries {
                let (PyRep::Tuple(tup) | PyRep::List(tup)) = entry else {
                    continue;
                };
                let Some(PyRep::Tuple(subloc)) = tup.get(2) else {
                    continue;
                };
                if subloc.first().and_then(|v| v.as_i64()) == Some(ship_id)
                    && subloc.get(1).and_then(|v| v.as_i64()) == Some(i64::from(flag as u16))
                    && tup.get(3).and_then(|v| v.as_i64()) == Some(i64::from(eve_sde::attr_ids::QUANTITY))
                {
                    let new_v = tup.get(5).and_then(|v| v.as_f64()).unwrap_or(-1.0);
                    let old_v = tup.get(6).and_then(|v| v.as_f64()).unwrap_or(-1.0);
                    out.push((old_v, new_v));
                }
            }
        }
    }
    out
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bf02_ancillary_transactionality_and_repeating_cycle_charge_notifications() {
    let custom_gun_tid = 999_201_u32;
    let custom_ammo_tid = 999_202_u32;
    let custom_asb_tid = 999_203_u32;
    let custom_cap_charge_tid = 999_204_u32;
    let custom_aar_tid = 999_205_u32;
    let custom_raar_tid = 999_206_u32;
    let custom_cap_booster_tid = 999_207_u32;

    let ctx = test_ctx_with_custom(|sde| {
        let mut custom_hull = sde.types.get(&587).expect("seed type").clone();
        custom_hull.type_id = 999_100;
        custom_hull.name = "BF02 Frigate Hull".into();
        custom_hull.default_attributes.insert(eve_sde::attr_ids::HI_SLOTS, 6.0);
        custom_hull.default_attributes.insert(eve_sde::attr_ids::MED_SLOTS, 4.0);
        custom_hull.default_attributes.insert(eve_sde::attr_ids::LOW_SLOTS, 4.0);
        sde.types.insert(999_100, custom_hull);

        let mut gun = custom_inv_type(
            sde,
            custom_gun_tid,
            55,
            7,
            "BF02 Autocannon",
            &[(6, 0.0), (51, 2000.0), (1795, 5000.0), (eve_sde::attr_ids::HP_STRUCT, 40.0)],
            &[12],
        );
        gun.capacity = 1.0;
        sde.types.insert(custom_gun_tid, gun);

        let mut ammo = custom_inv_type(
            sde,
            custom_ammo_tid,
            83,
            8,
            "BF02 Projectile Ammo",
            &[],
            &[],
        );
        ammo.volume = 0.01;
        sde.types.insert(custom_ammo_tid, ammo);

        sde.types.insert(
            custom_asb_tid,
            custom_inv_type(
                sde,
                custom_asb_tid,
                1156,
                7,
                "BF02 Ancillary Shield Booster",
                &[(6, 120.0), (51, 2000.0), (68, 150.0), (eve_sde::attr_ids::HP_STRUCT, 40.0)],
                &[13],
            ),
        );
        sde.types.insert(
            custom_cap_charge_tid,
            custom_inv_type(
                sde,
                custom_cap_charge_tid,
                87,
                8,
                "BF02 Cap Booster Charge",
                &[(67, 100.0)],
                &[],
            ),
        );
        sde.types.insert(
            custom_aar_tid,
            custom_inv_type(
                sde,
                custom_aar_tid,
                1199,
                7,
                "BF02 Ancillary Armor Repairer",
                &[(6, 50.0), (51, 2000.0), (56, 4.0), (84, 100.0), (eve_sde::attr_ids::HP_STRUCT, 40.0)],
                &[11],
            ),
        );
        sde.types.insert(
            custom_raar_tid,
            custom_inv_type(
                sde,
                custom_raar_tid,
                1698,
                7,
                "BF02 Remote Ancillary Armor Repairer",
                &[(6, 10.0), (51, 2000.0), (56, 4.0), (84, 100.0), (54, 10_000.0), (eve_sde::attr_ids::HP_STRUCT, 40.0)],
                &[11],
            ),
        );
        sde.types.insert(
            custom_cap_booster_tid,
            custom_inv_type(
                sde,
                custom_cap_booster_tid,
                76,
                7,
                "BF02 Capacitor Booster",
                &[(6, 0.0), (51, 2000.0), (eve_sde::attr_ids::HP_STRUCT, 40.0)],
                &[13],
            ),
        );
    });

    let sys_id = 30_000_142_u32;
    let char_id = 90_000_001_i64;
    let ship_id = 97_900_100_i64;
    let crim_char_id = 90_000_002_i64;
    let crim_ship_id = 97_900_200_i64;
    let mut sess = setup_pilot_and_ship(&ctx, char_id, sys_id, ship_id);
    let _ = setup_pilot_and_ship(&ctx, crim_char_id, sys_id, crim_ship_id);
    let sys = ctx.systems.get_or_spawn(sys_id);
    insert_ship_ball(&ctx, sys_id, ship_id, DVec3::ZERO).await;
    insert_ship_ball(&ctx, sys_id, crim_ship_id, DVec3::new(1000.0, 0.0, 0.0)).await;
    let _ = sys
        .add_target_lock(
            ship_id,
            eve_destiny::TargetLock {
                target_id: crim_ship_id,
                lock_time_ms: 1_000.0,
                remaining_lock_ms: 0.0,
                locked: true,
                max_range_m: None,
            },
        )
        .await;
    let seed_uid = ctx.db.with_snapshot(|s| s.accounts.values().next().expect("seed account").user_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.user_id = seed_uid;
        }
    });
    sess.user_id = Some(seed_uid);
    ctx.db
        .acquire_online_lock(seed_uid, 9850, Some(char_id), true, 1_000)
        .expect("online lock");
    {
        let now_u = ctx.now_unix();
        let mut cw = ctx.crimewatch.write().unwrap();
        cw.get_or_create(crim_char_id).criminal_flag_until_unix = now_u + 900;
    }
    crate::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| {
        rt.safety_levels.insert(char_id, crate::services::combat_crimewatch::SafetyLevel::Green);
    });

    let mod_gun0 = 97_900_301_i64;
    let mod_gun1 = 97_900_302_i64;
    let chg_gun0 = 97_900_311_i64;
    let chg_gun1 = 97_900_312_i64;
    let cargo_ammo = 97_900_313_i64;
    let mod_asb = 97_900_303_i64;
    let chg_asb = 97_900_314_i64;
    let mod_capb = 97_900_304_i64;
    let chg_capb = 97_900_315_i64;
    let mod_aar = 97_900_305_i64;
    let paste_aar = 97_900_316_i64;
    let mod_raar = 97_900_306_i64;
    let paste_raar = 97_900_317_i64;

    ctx.db.with_mut_snapshot(|snap| {
        for (id, tid, flag, qty, singleton) in [
            (mod_gun0, custom_gun_tid, InventoryFlag::HiSlot0, 1, true),
            (mod_gun1, custom_gun_tid, InventoryFlag::HiSlot1, 1, true),
            (mod_raar, custom_raar_tid, InventoryFlag::HiSlot2, 1, true),
            (mod_asb, custom_asb_tid, InventoryFlag::MedSlot0, 1, true),
            (mod_capb, custom_cap_booster_tid, InventoryFlag::MedSlot1, 1, true),
            (mod_aar, custom_aar_tid, InventoryFlag::LoSlot0, 1, true),
            (chg_gun0, custom_ammo_tid, InventoryFlag::HiSlot0, 2, false),
            (chg_gun1, custom_ammo_tid, InventoryFlag::HiSlot1, 2, false),
            (cargo_ammo, custom_ammo_tid, InventoryFlag::Cargo, 50, false),
            (chg_asb, custom_cap_charge_tid, InventoryFlag::MedSlot0, 3, false),
            (chg_capb, custom_cap_charge_tid, InventoryFlag::MedSlot1, 2, false),
            (paste_aar, 28668, InventoryFlag::LoSlot0, 8, false),
            (paste_raar, 28668, InventoryFlag::HiSlot2, 8, false),
        ] {
            if singleton {
                snap.module_online_states.insert(id, true);
            }
            snap.items.insert(
                id,
                InventoryItemRecord {
                    item_id: id,
                    type_id: tid,
                    owner_id: char_id,
                    location_id: ship_id,
                    flag,
                    quantity: qty,
                    singleton,
                    version: 1,
                    ..Default::default()
                },
            );
        }
    });
    ctx.invalidate_ship_fitting(ship_id);

    // Part A1: Local AAR activation fails with NotEnoughCapacitor -> paste stays 8 and 0 notifications emit
    let _ = sys.set_capacitor(ship_id, 5.0).await;
    let kw = HashMap::new();
    let _ = ctx.drain_notifications(char_id);
    let err_cap = handle_module_cycles(
        &ctx,
        &mut sess,
        None,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(mod_aar), PyRep::String("activate".into())],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err_cap.code, "NotEnoughCapacitor");
    assert_eq!(ctx.db.with_snapshot(|s| s.items[&paste_aar].quantity), 8);
    assert!(ctx.drain_notifications(char_id).is_empty());

    // Part A2: Remote AAR activation fails Safety Green (CantDoInCurrentSafetyLevel) -> paste stays 8 and 0 notifications
    let _ = sys.set_capacitor(ship_id, 1000.0).await;
    let err_safety = handle_module_cycles(
        &ctx,
        &mut sess,
        None,
        "dogmaIM",
        "Activate",
        &[
            PyRep::Integer(mod_raar),
            PyRep::String("activate".into()),
            PyRep::Integer(crim_ship_id),
        ],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err_safety.code, "CantDoInCurrentSafetyLevel");
    assert_eq!(ctx.db.with_snapshot(|s| s.items[&paste_raar].quantity), 8);
    assert!(ctx.drain_notifications(char_id).is_empty());

    // Switching to Safety Red allows remote AAR activation -> consumes paste 8 -> 4 & emits notifications
    crate::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| {
        rt.safety_levels.insert(char_id, crate::services::combat_crimewatch::SafetyLevel::Red);
    });
    handle_module_cycles(
        &ctx,
        &mut sess,
        None,
        "dogmaIM",
        "Activate",
        &[
            PyRep::Integer(mod_raar),
            PyRep::String("activate".into()),
            PyRep::Integer(crim_ship_id),
        ],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(ctx.db.with_snapshot(|s| s.items[&paste_raar].quantity), 4);
    let raar_notifs = ctx.drain_notifications(char_id);
    assert!(raar_notifs.iter().any(|n| n.event == "OnItemChange"));
    assert_eq!(
        count_attr805_changes_for_flag(&raar_notifs, ship_id, InventoryFlag::HiSlot2),
        vec![(8.0, 4.0)]
    );

    // Part B: Repeating cycles via real `step_manual` + `execute_pending_cycle_actions`
    // Queue 2 linked turrets (missing zero-damage shot), repeating ASB, repeating AAR, repeating Cap Booster
    let _ = sys.remove_module_cycle(ship_id, mod_raar).await;
    for cycle in [
        crate::system_actor::ActiveModuleCycle {
            ship_id,
            owner_char_id: Some(char_id),
            module_id: mod_gun0,
            module_type_id: custom_gun_tid,
            target_id: Some(crim_ship_id),
            effect_name: "projectileFired".into(),
            duration_s: 2.0,
            elapsed_s: 0.0,
            repeat: 2,
            linked_turrets: vec![mod_gun0, mod_gun1],
            ammo_type_id: Some(custom_ammo_tid),
            crystal_volatility_chance: 0.0,
            crystal_volatility_damage: 0.0,
            weapon_is_turret: true,
            // zero damage_vector tests miss case (no CycleWeaponDamage damage > 0 emitted, yet charges still notify)
            damage_vector: eve_destiny::DamageVector::default(),
            ..Default::default()
        },
        crate::system_actor::ActiveModuleCycle {
            ship_id,
            owner_char_id: Some(char_id),
            module_id: mod_asb,
            module_type_id: custom_asb_tid,
            effect_name: "shieldBoosting".into(),
            duration_s: 2.0,
            elapsed_s: 0.0,
            repeat: 2,
            is_asb: true,
            asb_fallback_cap_need: 120.0,
            cap_need_per_cycle: 0.0,
            ..Default::default()
        },
        crate::system_actor::ActiveModuleCycle {
            ship_id,
            owner_char_id: Some(char_id),
            module_id: mod_aar,
            module_type_id: custom_aar_tid,
            effect_name: "armorRepair".into(),
            duration_s: 2.0,
            elapsed_s: 0.0,
            repeat: 2,
            is_aar: true,
            aar_paste_rate: 4,
            aar_base_armor_repair: 100.0,
            armor_repair_amount: 300.0,
            ..Default::default()
        },
        crate::system_actor::ActiveModuleCycle {
            ship_id,
            owner_char_id: Some(char_id),
            module_id: mod_capb,
            module_type_id: custom_cap_booster_tid,
            effect_name: "capacitorBooster".into(),
            duration_s: 2.0,
            elapsed_s: 0.0,
            repeat: 2,
            cap_booster_inject_amount: 100.0,
            ..Default::default()
        },
    ] {
        sys.add_module_cycle(cycle).await.unwrap();
    }
    let _ = ctx.drain_notifications(char_id);

    // Tick 1: linked turrets 2 -> 1 (miss), ASB 3 -> 2, AAR 8 -> 4, Cap Booster 2 -> 1
    sys.step_manual(2.1).await.unwrap();
    execute_pending_cycle_actions(&ctx, sys_id).await;
    let t1_notifs = ctx.drain_notifications(char_id);
    assert_eq!(
        count_attr805_changes_for_flag(&t1_notifs, ship_id, InventoryFlag::HiSlot0),
        vec![(2.0, 1.0)]
    );
    assert_eq!(
        count_attr805_changes_for_flag(&t1_notifs, ship_id, InventoryFlag::HiSlot1),
        vec![(2.0, 1.0)],
        "second linked turret must also receive OnModuleAttributeChanges (805)"
    );
    assert_eq!(
        count_attr805_changes_for_flag(&t1_notifs, ship_id, InventoryFlag::MedSlot0),
        vec![(3.0, 2.0)]
    );
    assert_eq!(
        count_attr805_changes_for_flag(&t1_notifs, ship_id, InventoryFlag::MedSlot1),
        vec![(2.0, 1.0)]
    );
    assert_eq!(
        count_attr805_changes_for_flag(&t1_notifs, ship_id, InventoryFlag::LoSlot0),
        vec![(8.0, 4.0)]
    );
    assert!(t1_notifs.iter().filter(|n| n.event == "OnItemChange").count() >= 5);

    // Tick 2: linked turrets 1 -> 0 + cargo auto-reload (both slots get 0 -> reloaded_qty + OnGodmaPrimeItem);
    // ASB 2 -> 1, AAR 4 -> 0 (tombstone), Cap Booster 1 -> 0 (tombstone)
    sys.step_manual(2.1).await.unwrap();
    execute_pending_cycle_actions(&ctx, sys_id).await;
    let t2_notifs = ctx.drain_notifications(char_id);
    assert_eq!(
        count_attr805_changes_for_flag(&t2_notifs, ship_id, InventoryFlag::HiSlot0),
        vec![(1.0, 0.0), (0.0, 25.0)]
    );
    assert_eq!(
        count_attr805_changes_for_flag(&t2_notifs, ship_id, InventoryFlag::HiSlot1),
        vec![(1.0, 0.0), (0.0, 25.0)]
    );
    assert!(
        t2_notifs.iter().filter(|n| n.event == "OnGodmaPrimeItem").count() >= 2,
        "cargo auto-reload across two linked turrets must prime both reloaded charge stacks"
    );
    assert_eq!(
        count_attr805_changes_for_flag(&t2_notifs, ship_id, InventoryFlag::MedSlot0),
        vec![(2.0, 1.0)]
    );
    assert_eq!(
        count_attr805_changes_for_flag(&t2_notifs, ship_id, InventoryFlag::MedSlot1),
        vec![(1.0, 0.0)]
    );
    assert_eq!(
        count_attr805_changes_for_flag(&t2_notifs, ship_id, InventoryFlag::LoSlot0),
        vec![(4.0, 0.0)]
    );
    assert!(ctx.db.with_snapshot(|s| !s.items.contains_key(&paste_aar)));
    assert!(ctx.db.with_snapshot(|s| !s.items.contains_key(&chg_capb)));
    assert!(!ctx.db.with_snapshot(|s| s.items.contains_key(&cargo_ammo)));
}
