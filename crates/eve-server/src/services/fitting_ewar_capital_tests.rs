//! Unit tests for `fitting_ewar_capital` (`ewar_jamming_type`, `emit_committed_ewar_and_system_wide_notifications`, `prepare_ewar_and_capital_module`).

use super::*;
use eve_db::{AccountRecord, InventoryFlag, InventoryItemRecord, WorldRepository};
use eve_sde::{InvType, SdeRegistry};
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn synth_mod_type(type_id: u32, group_id: u32, name: &str, effects: Vec<u16>) -> InvType {
    let mut t = shared_sde().types.get(&587).expect("seed type").clone();
    t.type_id = type_id;
    t.group_id = group_id;
    t.category_id = 7;
    t.name = name.into();
    t.default_attributes.clear();
    t.effect_ids = effects;
    t
}

#[test]
fn ewar_jamming_type_and_notifications_cover_all_ewar_groups() {
    let scram = synth_mod_type(448, 52, "Warp Scrambler II", vec![]);
    assert_eq!(ewar_jamming_type(&scram, None, true, None), Some("warpScramblerMWD"));
    assert_eq!(ewar_jamming_type(&scram, None, false, None), Some("warpScrambler"));

    let hic = synth_mod_type(16242, 899, "Warp Disruption Field Generator I", vec![]);
    assert_eq!(ewar_jamming_type(&hic, Some(45010), false, None), Some("warpScramblerMWD"));
    assert_eq!(ewar_jamming_type(&hic, Some(28661), false, None), Some("focusedWarpScrambler"));
    assert_eq!(ewar_jamming_type(&hic, None, false, Some(20_000.0)), None);

    let web = synth_mod_type(526, 65, "Stasis Webifier I", vec![]);
    assert_eq!(ewar_jamming_type(&web, None, false, None), Some("webify"));

    let ecm = synth_mod_type(1952, 201, "Multispectral ECM I", vec![]);
    assert_eq!(ewar_jamming_type(&ecm, None, false, None), Some("electronic"));

    let damp = synth_mod_type(1968, 208, "Remote Sensor Dampener I", vec![]);
    assert_eq!(ewar_jamming_type(&damp, None, false, None), Some("ewRemoteSensorDamp"));

    let td = synth_mod_type(2108, 291, "Tracking Disruptor I", vec![]);
    assert_eq!(ewar_jamming_type(&td, None, false, None), Some("ewTrackingDisrupt"));

    let gd = synth_mod_type(37540, 291, "Guidance Disruptor I", vec![6425]);
    assert_eq!(ewar_jamming_type(&gd, None, false, None), Some("ewGuidanceDisrupt"));

    let tp = synth_mod_type(12709, 379, "Target Painter I", vec![]);
    assert_eq!(ewar_jamming_type(&tp, None, false, None), Some("ewTargetPaint"));

    let neut = synth_mod_type(533, 71, "Heavy Energy Neutralizer I", vec![]);
    assert_eq!(ewar_jamming_type(&neut, None, false, None), Some("ewEnergyNeut"));

    let nos = synth_mod_type(530, 68, "Heavy Energy Nosferatu I", vec![]);
    assert_eq!(ewar_jamming_type(&nos, None, false, None), Some("ewEnergyVampire"));

    let ctx = ServerContext::with_sde_and_repository(
        shared_sde(),
        Arc::new(WorldRepository::with_demo_seed()),
    );
    let char_id = 96_100_001i64;
    let target_owner = 96_100_002i64;
    let ship_id = 96_100_100i64;
    let target_ship_id = 96_100_200i64;
    let module_id = 96_100_300i64;

    ctx.db.with_mut_snapshot(|snap| {
        let base_ch = snap.characters.values().next().cloned().expect("seed char");
        for (uid, cid) in [(96_101u64, char_id), (96_102, target_owner)] {
            snap.accounts.insert(
                format!("user_{uid}"),
                AccountRecord {
                    user_id: uid,
                    username: format!("user_{uid}"),
                    password_token: String::new(),
                    role: 0,
                    banned_until_unix: None,
                    ban_reason: None,
                },
            );
            let mut ch = base_ch.clone();
            ch.character_id = cid;
            ch.user_id = uid;
            ch.name = format!("Pilot{uid}");
            ch.corporation_id = 1_000_009;
            ch.solar_system_id = 30_000_142;
            ch.active_ship_id = if cid == char_id { ship_id } else { target_ship_id };
            snap.characters.insert(cid, ch);
        }
        snap.items.insert(
            target_ship_id,
            InventoryItemRecord {
                item_id: target_ship_id,
                type_id: 587,
                owner_id: target_owner,
                location_id: 30_000_142,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    let _ = ctx.db.acquire_online_lock(96_101, 1, Some(char_id), true, 100);
    let _ = ctx.db.acquire_online_lock(96_102, 2, Some(target_owner), true, 100);

    let phenomena = synth_mod_type(43554, 1815, "Amarr Phenomena Generator", vec![6753]);
    emit_committed_ewar_and_system_wide_notifications(
        &ctx,
        30_000_142,
        char_id,
        ship_id,
        module_id,
        43554,
        &phenomena,
        None,
        false,
        None,
        None,
    );

    emit_committed_ewar_and_system_wide_notifications(
        &ctx,
        30_000_142,
        char_id,
        ship_id,
        module_id,
        448,
        &scram,
        None,
        true,
        None,
        Some(target_ship_id),
    );

    let notifs = ctx.drain_notifications_for_char(char_id);
    assert!(notifs.iter().any(|n| n.event == "OnSystemWideEffectStart"));
    assert!(notifs.iter().any(|n| n.event == "OnEwarStart"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn prepare_ewar_and_capital_module_validates_hull_groups_and_prepares_adc_and_nullifier() {
    let ctx = ServerContext::with_sde_and_repository(
        shared_sde(),
        Arc::new(WorldRepository::with_demo_seed()),
    );
    let sys_id = 30_000_142u32;
    let sys = ctx.systems.get_or_spawn(sys_id);
    let char_id = 96_100_010i64;
    let ship_id = 96_100_110i64;
    let module_id = 96_100_310i64;

    let mut fit = eve_dogma::ShipFittingEngine::from_sde(ship_id, 587, &ctx.sde);
    let _ = fit.fit_module_with_state(module_id, 47254, 11, true, &ctx.sde);
    let fm = fit.modules.get(&module_id).cloned().unwrap_or(eve_dogma::FittedModule {
        item_id: module_id,
        type_id: 47254,
        flag_id: 11,
        state: eve_dogma::ModuleState::Online,
        charge_type_id: None,
        charge_quantity: 0,
        damage_hp: 0.0,
        max_hp: 40.0,
        dynamic_attribute_overrides: HashMap::new(),
    });
    let adc_type = synth_mod_type(47254, 60, "Assault Damage Control I", vec![7012]);
    let mod_state = fit.build_effective_module_state(module_id, 47254, false, &ctx.sde);

    // Non-assault frigate (group 25) is rejected for ADC
    let err = prepare_ewar_and_capital_module(
        &ctx,
        &sys,
        sys_id,
        char_id,
        ship_id,
        25,
        module_id,
        &fm,
        &adc_type,
        &mod_state,
        &fit,
        None,
        false,
        false,
        false,
        &HashMap::new(),
    )
    .await
    .expect("Some")
    .err()
    .expect("ADC rejected on group 25");
    assert_eq!(err.code, "InvalidShipGroup");

    // Assault Frigate (group 324) succeeds with is_adc = true
    let prep = prepare_ewar_and_capital_module(
        &ctx,
        &sys,
        sys_id,
        char_id,
        ship_id,
        324,
        module_id,
        &fm,
        &adc_type,
        &mod_state,
        &fit,
        None,
        false,
        false,
        false,
        &HashMap::new(),
    )
    .await
    .expect("Some")
    .expect("ADC accepted on group 324");
    assert!(prep.is_adc);

    // Interdiction Nullifier (group 4117) on Interceptor (group 831) succeeds
    let nullifier_type = synth_mod_type(59600, 4117, "Compact Interdiction Nullifier", vec![]);
    let null_prep = prepare_ewar_and_capital_module(
        &ctx,
        &sys,
        sys_id,
        char_id,
        ship_id,
        831,
        module_id,
        &fm,
        &nullifier_type,
        &mod_state,
        &fit,
        None,
        false,
        false,
        false,
        &HashMap::new(),
    )
    .await
    .expect("Some")
    .expect("Nullifier accepted on group 831");
    assert!(null_prep.grants_interdiction_nullification);
}

#[tokio::test]
async fn combat_charge_depletion_corp_wallet_balance_and_role_authority_invariants() {
    use crate::router::NEXT_TRANSPORT_SESSION_ID;
    use eve_db::{corp_roles, InventoryFlag, InventoryItemRecord, IskCents};
    use eve_destiny::{Ball, CombatState, MovementMode, TargetLock};
    use eve_machonet::SessionState;
    use glam::DVec3;
    use std::sync::atomic::Ordering;

    let ctx = ServerContext::with_sde_and_repository(
        shared_sde(),
        Arc::new(WorldRepository::with_demo_seed()),
    );
    let kw = HashMap::new();

    // 1. #854: MachoBindObject string-ID hash on `"WKXJZAUNNTM\\C"` (`i64::MIN` before abs) must not panic
    let mut bind_sess = SessionState::default();
    bind_sess.user_id = Some(1);
    bind_sess.character_id = Some(90_000_001);
    bind_sess.corporation_id = Some(98_000_001);
    let min_hash_str = "WKXJZAUNNTM\\C";
    let raw_hash = min_hash_str
        .bytes()
        .fold(1_000i64, |acc, b| acc.wrapping_mul(31).wrapping_add(b as i64));
    assert_eq!(raw_hash, i64::MIN);
    let bind_err = ctx
        .dispatch_rpc(
            &mut bind_sess,
            "charMgr",
            None,
            "MachoBindObject",
            &[PyRep::String(min_hash_str.into()), PyRep::None],
            &kw,
            None,
        )
        .await
        .expect_err("different character_id should reject binding to saturated i64::MAX");
    assert_eq!(bind_err.code, "BindAuthorizationDenied");
    assert!(bind_err.message.contains(&i64::MAX.to_string()));

    let bind_tuple_err = ctx
        .dispatch_rpc(
            &mut bind_sess,
            "charMgr",
            None,
            "MachoBindObject",
            &[
                PyRep::Tuple(vec![PyRep::String(min_hash_str.into())]),
                PyRep::None,
            ],
            &kw,
            None,
        )
        .await
        .expect_err("tuple string hash should also saturate to i64::MAX without panic");
    assert_eq!(bind_tuple_err.code, "BindAuthorizationDenied");
    assert!(bind_tuple_err.message.contains(&i64::MAX.to_string()));

    // 2. #852: Shared transport session ID allocator across machoNet, gRPC, and Bridge
    let s1 = crate::macho_listener::NEXT_MACHO_SESSION_ID.fetch_add(1, Ordering::Relaxed);
    let s2 = crate::quasar_grpc::NEXT_GRPC_SESSION_ID.fetch_add(1, Ordering::Relaxed);
    let s3 = NEXT_TRANSPORT_SESSION_ID.fetch_add(1, Ordering::Relaxed);
    assert_eq!(s2, s1 + 1);
    assert_eq!(s3, s2 + 1);

    // 3. #938 + #837: `account.GetCashBalance(1)` returns corp wallet balance for authorized role
    //    and denies unauthorized members even if `session.corp_role` is stale `u64::MAX`.
    let corp_id = 98_000_001_i64;
    let ceo_id = 90_000_001_i64;
    let member_id = 90_000_002_i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&ceo_id) {
            ch.balance = IskCents::from_isk(1_234);
        }
        if let Some(ch) = snap.characters.get_mut(&member_id) {
            ch.balance = IskCents::from_isk(5_678);
        }
        if let Some(corp) = snap.corporations.get_mut(&corp_id) {
            corp.set_division_balance(1, IskCents::from_isk(77_000_000));
            corp.set_division_balance(3, IskCents::from_isk(33_000_000));
            corp.member_roles.insert(member_id, 0);
        }
    });

    let mut ceo_sess = SessionState::default();
    ceo_sess.user_id = Some(1);
    ceo_sess.character_id = Some(ceo_id);
    ceo_sess.corporation_id = Some(corp_id);
    ceo_sess.corp_role = u64::MAX;

    let personal_bal = ctx
        .dispatch_rpc(
            &mut ceo_sess,
            "account",
            None,
            "GetCashBalance",
            &[PyRep::Integer(0)],
            &kw,
            None,
        )
        .await
        .expect("personal GetCashBalance(0)");
    assert_eq!(personal_bal.as_f64(), Some(1_234.0));

    let corp_bal_div1 = ctx
        .dispatch_rpc(
            &mut ceo_sess,
            "account",
            None,
            "GetCashBalance",
            &[PyRep::Integer(1)],
            &kw,
            None,
        )
        .await
        .expect("corp GetCashBalance(1)");
    assert_eq!(corp_bal_div1.as_f64(), Some(77_000_000.0));

    let corp_bal_div3 = ctx
        .dispatch_rpc(
            &mut ceo_sess,
            "account",
            None,
            "GetCashBalance",
            &[PyRep::Bool(true), PyRep::Integer(1002)],
            &kw,
            None,
        )
        .await
        .expect("corp GetCashBalance(True, 1002)");
    assert_eq!(corp_bal_div3.as_f64(), Some(33_000_000.0));

    // Member with stale `session.corp_role = u64::MAX` but `0` in DB is denied on corp wallet query (#837 + #938)
    let mut stale_member_sess = SessionState::default();
    stale_member_sess.user_id = Some(2);
    stale_member_sess.character_id = Some(member_id);
    stale_member_sess.corporation_id = Some(corp_id);
    stale_member_sess.corp_role = u64::MAX;
    let denied_bal = ctx
        .dispatch_rpc(
            &mut stale_member_sess,
            "account",
            None,
            "GetCashBalance",
            &[PyRep::Integer(1)],
            &kw,
            None,
        )
        .await
        .expect_err("stale session.corp_role must not grant corp wallet balance access");
    assert_eq!(denied_bal.code, "CrpAccessDenied");

    // Depose CEO via DB / vote state: `ceo_sess` still has stale `session.corp_role = u64::MAX`,
    // and `require_corp_profile_manager` / `has_structure_manager_role` must reject stale `session.corp_role` (#837).
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(corp) = snap.corporations.get_mut(&corp_id) {
            corp.ceo_id = member_id;
            corp.member_roles.insert(ceo_id, 0);
            corp.member_roles.insert(member_id, u64::MAX);
        }
    });
    ctx.db.with_snapshot(|snap| {
        assert!(
            !crate::services::station_structure::structure_lifecycle_rpc::has_structure_manager_role(
                snap, &ceo_sess, ceo_id, corp_id
            ),
            "deposed CEO with stale session.corp_role must not retain structure manager role"
        );
        assert!(
            crate::services::station_structure::structure_upwell_ops::require_corp_profile_manager(
                snap, &ceo_sess, ceo_id, corp_id
            )
            .is_err(),
            "deposed CEO with stale session.corp_role must not pass require_corp_profile_manager"
        );
    });
    // Restore CEO for subsequent assertions
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(corp) = snap.corporations.get_mut(&corp_id) {
            corp.ceo_id = ceo_id;
            corp.member_roles.insert(ceo_id, u64::MAX);
            corp.member_roles.insert(member_id, corp_roles::ROLE_DIRECTOR);
        }
    });

    // 4. #836: Unloaded charge-using weapon rejects activation with `NoAmmoLoaded` (never fires 80 omni damage),
    //    and loaded weapon with 5 rounds consumes exactly 1 round per shot over 5 shots.
    let sys_id = 30_002_505_u32; // Lowsec system so no highsec CONCORD/safety block on PvP test
    let ship_id = 910_500_001_i64;
    let target_id = 910_500_002_i64;
    let gun_id = 910_500_003_i64;
    let ammo_id = 910_500_004_i64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&ceo_id) {
            ch.station_id = None;
            ch.structure_id = None;
            ch.solar_system_id = sys_id;
            ch.active_ship_id = ship_id;
        }
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587, // Rifter
                owner_id: ceo_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            gun_id,
            InventoryItemRecord {
                item_id: gun_id,
                type_id: 487, // 200mm AutoCannon I (group 55)
                owner_id: ceo_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(gun_id, true);
    });
    ctx.invalidate_ship_fitting(ship_id);

    let sys = ctx.systems.get_or_spawn(sys_id);
    for (bid, tid, x) in [(ship_id, 587_u32, 0.0), (target_id, 587_u32, 1_500.0)] {
        let mut cs = CombatState::default();
        cs.shield_hp = 500.0;
        cs.max_shield_hp = 500.0;
        cs.armor_hp = 500.0;
        cs.max_armor_hp = 500.0;
        cs.structure_hp = 500.0;
        cs.max_structure_hp = 500.0;
        cs.capacitor = 500.0;
        cs.max_capacitor = 500.0;
        let _ = sys
            .spawn_ball_with_combat(
                Ball {
                    id: bid,
                    type_id: tid,
                    position: DVec3::new(x, 0.0, 0.0),
                    velocity: DVec3::ZERO,
                    max_velocity: 300.0,
                    inertia: 2.5,
                    mass: 1_000_000.0,
                    radius: 40.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                cs,
            )
            .await;
    }
    let _ = sys
        .add_target_lock(
            ship_id,
            TargetLock {
                target_id,
                lock_time_ms: 100.0,
                remaining_lock_ms: 0.0,
                locked: true,
                max_range_m: Some(50_000.0),
            },
        )
        .await;

    ceo_sess.station_id = None;
    ceo_sess.structure_id = None;
    ceo_sess.solar_system_id = Some(sys_id);
    ceo_sess.ship_id = Some(ship_id);

    // Activating unloaded 200mm AutoCannon I must return `NoAmmoLoaded` and leave target HP untouched
    let unloaded_err = ctx
        .dispatch_rpc(
            &mut ceo_sess,
            "dogmaIM",
            None,
            "Activate",
            &[
                PyRep::Integer(gun_id),
                PyRep::String("projectileFired".into()),
                PyRep::Integer(target_id),
                PyRep::Integer(1),
            ],
            &kw,
            None,
        )
        .await
        .expect_err("unloaded turret must not activate");
    assert_eq!(unloaded_err.code, "NoAmmoLoaded");
    let target_cs = sys.get_combat_state(target_id).await.unwrap().unwrap();
    assert_eq!(target_cs.shield_hp, 500.0);

    // A5A-P1-01: Pre-overloading an unloaded weapon (`dogmaIM.Overload(gun_id)`) only toggles overload state
    // without starting a cycle, and subsequently activating that pre-overloaded empty weapon still enforces
    // `NoAmmoLoaded` instead of cycling for 0.0 damage.
    ctx.dispatch_rpc(
        &mut ceo_sess,
        "dogmaIM",
        None,
        "Overload",
        &[PyRep::Integer(gun_id)],
        &kw,
        None,
    )
    .await
    .expect("pure overload toggle on inactive turret should succeed");
    assert!(
        !sys.has_module_cycle(ship_id, gun_id).await,
        "Overload toggle without target must not start an active weapon cycle"
    );
    let pre_ovl_err = ctx
        .dispatch_rpc(
            &mut ceo_sess,
            "dogmaIM",
            None,
            "Activate",
            &[
                PyRep::Integer(gun_id),
                PyRep::String("projectileFired".into()),
                PyRep::Integer(target_id),
                PyRep::Integer(1),
            ],
            &kw,
            None,
        )
        .await
        .expect_err("pre-overloaded unloaded turret must still reject with NoAmmoLoaded");
    assert_eq!(pre_ovl_err.code, "NoAmmoLoaded");
    assert!(!sys.has_module_cycle(ship_id, gun_id).await);
    ctx.dispatch_rpc(
        &mut ceo_sess,
        "dogmaIM",
        None,
        "Overload",
        &[PyRep::Integer(gun_id)],
        &kw,
        None,
    )
    .await
    .expect("toggle overload back off");

    // Load 5 rounds of Fusion S (`185`) into HiSlot0 and fire 5 cycles (`repeat = 1`)
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ammo_id,
            InventoryItemRecord {
                item_id: ammo_id,
                type_id: 185,
                owner_id: ceo_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 5,
                singleton: false,
                ..Default::default()
            },
        );
    });
    ctx.invalidate_ship_fitting(ship_id);

    let act_ok = ctx
        .dispatch_rpc(
            &mut ceo_sess,
            "dogmaIM",
            None,
            "Activate",
            &[
                PyRep::Integer(gun_id),
                PyRep::String("projectileFired".into()),
                PyRep::Integer(target_id),
                PyRep::Integer(1),
            ],
            &kw,
            None,
        )
        .await
        .expect("loaded turret should activate");
    let dur_raw = act_ok
        .dict_get("duration")
        .and_then(|v| v.as_f64())
        .unwrap_or(5.0);
    let cycle_s = if dur_raw > 100.0 {
        dur_raw / 1_000.0
    } else {
        dur_raw
    }
    .max(1.0);

    // At activation (start of cycle 1): round #1 is chambered -> 4 rounds remaining in stack
    assert_eq!(
        ctx.db.with_snapshot(|s| s.items.get(&ammo_id).map(|i| i.quantity)),
        Some(4),
        "at start of cycle 1, round #1 is chambered so 4 of 5 rounds remain"
    );

    // Step cycles 1, 2, 3 (shots 1, 2, 3 land; cycles 2, 3, 4 chamber rounds #2, #3, #4):
    for expected_rem in [3, 2, 1] {
        let _ = sys.step_manual(cycle_s + 0.02).await;
        assert_eq!(
            ctx.db.with_snapshot(|s| s.items.get(&ammo_id).map(|i| i.quantity)),
            Some(expected_rem),
            "expected {expected_rem} rounds remaining"
        );
    }

    // Step cycle 4 (shot 4 lands; cycle 5 chambers the 5th and final round -> stack removed, cycle 5 stays active)
    let _ = sys.step_manual(cycle_s + 0.02).await;
    assert_eq!(
        ctx.db.with_snapshot(|s| s.items.get(&ammo_id).map(|i| i.quantity)),
        None,
        "at start of cycle 5, round #5 is chambered and empty stack is removed"
    );
    assert!(
        sys.has_module_cycle(ship_id, gun_id).await,
        "cycle 5 must remain active to fire the 5th chambered round"
    );

    // Step cycle 5 (shot 5 lands; no rounds remain for cycle 6 -> turret cycle stops)
    let _ = sys.step_manual(cycle_s + 0.02).await;
    assert!(
        !sys.has_module_cycle(ship_id, gun_id).await,
        "turret cycle must stop after firing its 5th and final round"
    );

    // 4b. #952 + #804: `OnDamageMessage` emitted during weapon cycles has 1 arg (`[dict]`) and includes `"isBanked": Bool(false)`
    let dmg_notifs = ctx.drain_notifications_for_char(ceo_id);
    let on_dmg_msg = dmg_notifs
        .iter()
        .find(|n| n.event == "OnDamageMessage")
        .expect("expected OnDamageMessage notification for attacker");
    assert_eq!(
        on_dmg_msg.args.len(),
        1,
        "OnDamageMessage must pass 1 argument [dmg_dict] for V24 Godma/BracketMgr"
    );
    assert_eq!(
        on_dmg_msg.args[0].dict_get("isBanked"),
        Some(&PyRep::Bool(false)),
        "combat damage dict must include 'isBanked': Bool(false) for uilog.AddCombatMessageFromDict"
    );

    // 5. #949: `/heal` in space restores ship HP and queues Destiny `OnDamageStateChange` (`[1.0, 1.0, 1.0]`)
    //    in the ballpark observer queue so V24 clients receive `DoDestinyUpdate`.
    let _ = sys
        .apply_damage(
            ship_id,
            eve_destiny::DamageVector {
                em: 400.0,
                thermal: 400.0,
                kinetic: 400.0,
                explosive: 100.0,
            },
        )
        .await;
    let pre_heal_cs = sys.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!(pre_heal_cs.shield_hp < 500.0 || pre_heal_cs.armor_hp < 500.0);
    let _ = sys.drain_observer_updates(ship_id).await;

    ceo_sess.role = u64::MAX;
    let heal_rep = ctx
        .dispatch_rpc(
            &mut ceo_sess,
            "slash",
            None,
            "SlashCmd",
            &[PyRep::String("/heal".into())],
            &kw,
            None,
        )
        .await
        .expect("/heal slash command should succeed");
    assert_eq!(heal_rep.dict_get("healed"), Some(&PyRep::Bool(true)));

    let post_heal_cs = sys.get_combat_state(ship_id).await.unwrap().unwrap();
    assert_eq!(post_heal_cs.shield_hp, 500.0);
    assert_eq!(post_heal_cs.armor_hp, 500.0);
    assert_eq!(post_heal_cs.structure_hp, 500.0);

    let (_tick, heal_events, _pyrep) = sys
        .drain_observer_updates(ship_id)
        .await
        .expect("drain observer updates after /heal");
    assert!(
        heal_events.iter().any(|ev| matches!(
            ev,
            eve_destiny::DestinyEvent::OnDamageStateChange {
                ball_id,
                shield_ratio,
                armor_ratio,
                structure_ratio,
            } if *ball_id == ship_id
                && (*shield_ratio - 1.0).abs() < 1e-9
                && (*armor_ratio - 1.0).abs() < 1e-9
                && (*structure_ratio - 1.0).abs() < 1e-9
        )),
        "expected DestinyEvent::OnDamageStateChange(1.0, 1.0, 1.0) in observer queue after /heal, got: {heal_events:?}"
    );

    // 6. #951 & A5A-P1-02: `dogmaIM.GetAllInfo` populates `maxActiveDrones` (`352`) and `maxLockedTargets` (`192`)
    //    on `charInfo` (using Target Management `3429` & Advanced Target Management `3430`, NOT Long Range Targeting `3428`),
    //    and `ship.LaunchDrones([(droneID, 2)], None, False)` launches 2 drones from a stack and returns `{}` (`PyRep::Dict`).
    let hob_stack_id = 910_500_005_i64;
    ctx.db.with_mut_snapshot(|snap| {
        let sk = snap.skills.entry(ceo_id).or_default();
        sk.remove(&3429);
        sk.remove(&3430);
        sk.insert(
            3428, // Long Range Targeting Lv5 — must NOT inflate maxLockedTargets (192)
            eve_db::TrainedSkillRecord {
                type_id: 3428,
                level: 5,
                skill_points: 256_000.0,
            },
        );
        sk.insert(
            3436,
            eve_db::TrainedSkillRecord {
                type_id: 3436,
                level: 3,
                skill_points: 8_000.0,
            },
        );
        if let Some(sh) = snap.items.get_mut(&ship_id) {
            sh.type_id = 32_880; // Venture (10 Mbit/s bandwidth, 10 m3 drone bay)
        }
        snap.items.insert(
            hob_stack_id,
            InventoryItemRecord {
                item_id: hob_stack_id,
                type_id: 2454, // Hobgoblin I
                owner_id: ceo_id,
                location_id: ship_id,
                flag: InventoryFlag::DroneBay,
                quantity: 2,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });
    ctx.invalidate_ship_fitting(ship_id);

    let all_info = ctx
        .dispatch_rpc(
            &mut ceo_sess,
            "dogmaIM",
            None,
            "GetAllInfo",
            &[PyRep::Bool(true), PyRep::Bool(true)],
            &kw,
            None,
        )
        .await
        .expect("dogmaIM.GetAllInfo");
    let extract_char_attr = |info: &PyRep, aid: i64| -> Option<f64> {
        let PyRep::Tuple(ci_parts) = info.dict_get("charInfo")? else {
            return None;
        };
        let PyRep::Dict(ci_entries) = &ci_parts[0] else {
            return None;
        };
        let PyRep::Dict(attr_pairs) = ci_entries[0].1.dict_get("attributes")? else {
            return None;
        };
        attr_pairs
            .iter()
            .find_map(|(k, v)| (k.as_i64() == Some(aid)).then(|| v.as_f64()).flatten())
    };
    assert_eq!(
        extract_char_attr(&all_info, 352),
        Some(3.0),
        "charInfo must expose maxActiveDrones (352) == 3.0 for Drones Lv3"
    );
    assert_eq!(
        extract_char_attr(&all_info, 192),
        Some(2.0),
        "Long Range Targeting (3428) Lv5 with Target Management (3429) Lv0 must keep maxLockedTargets (192) at base 2.0"
    );
    ctx.db.with_mut_snapshot(|snap| {
        let sk = snap.skills.entry(ceo_id).or_default();
        sk.insert(
            3429, // Target Management Lv4
            eve_db::TrainedSkillRecord {
                type_id: 3429,
                level: 4,
                skill_points: 45_255.0,
            },
        );
        sk.insert(
            3430, // Advanced Target Management Lv2
            eve_db::TrainedSkillRecord {
                type_id: 3430,
                level: 2,
                skill_points: 4_243.0,
            },
        );
    });
    ctx.invalidate_ship_fitting(ship_id);
    let all_info_tm = ctx
        .dispatch_rpc(
            &mut ceo_sess,
            "dogmaIM",
            None,
            "GetAllInfo",
            &[PyRep::Bool(true), PyRep::Bool(true)],
            &kw,
            None,
        )
        .await
        .expect("dogmaIM.GetAllInfo with 3429/3430");
    assert_eq!(
        extract_char_attr(&all_info_tm, 192),
        Some(8.0),
        "Target Management (3429) Lv4 + Advanced Target Management (3430) Lv2 must yield maxLockedTargets (192) == 8.0"
    );

    let launch_v24 = ctx
        .dispatch_rpc(
            &mut ceo_sess,
            "ship",
            None,
            "LaunchDrones",
            &[
                PyRep::List(vec![PyRep::Tuple(vec![
                    PyRep::Integer(hob_stack_id),
                    PyRep::Integer(2),
                ])]),
                PyRep::None,
                PyRep::Bool(false),
            ],
            &kw,
            None,
        )
        .await
        .expect("V24 ship.LaunchDrones([(drone_id, 2)], None, False)");
    assert_eq!(
        launch_v24,
        PyRep::Dict(Vec::new()),
        "V24 LaunchFromShip expects an empty error dict on full success"
    );
    let active_drone_count = crate::services::combat_crimewatch::with_combat_runtime(&ctx, |rt| {
        rt.active_drones_in_space
            .values()
            .filter(|d| d.parent_ship_id == ship_id && !d.is_abandoned)
            .count()
    });
    assert_eq!(active_drone_count, 2, "both drones from the stack of 2 must be launched into space");

    // 7. #953: `corpRegistry.GetCorporation` includes `memberLimit` on the returned KeyVal
    let corp_kv = ctx
        .dispatch_rpc(
            &mut ceo_sess,
            "corpRegistry",
            None,
            "GetCorporation",
            &[PyRep::Integer(corp_id)],
            &kw,
            None,
        )
        .await
        .expect("corpRegistry.GetCorporation");
    let member_limit = corp_kv
        .dict_get("memberLimit")
        .and_then(|v| v.as_i64())
        .expect("GetCorporation must include integer memberLimit");
    assert!(member_limit >= 1, "memberLimit must be positive, got {member_limit}");
}

#[cfg(test)]
#[path = "fitting_ewar_capital_extra_unit_tests.rs"]
mod extra_unit_tests;
