//! Unit and mutation-killing tests for `router::cache_notifications`.

use super::*;
use crate::router::ServerContext;
use eve_db::{FleetRecord, SystemDynamicState, WorldRepository};
use eve_machonet::MachoPacketType;
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use std::sync::{Arc, OnceLock};

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn test_ctx() -> ServerContext {
    if let Some(sde) = SHARED_SDE.get() {
        ServerContext::with_sde_and_repository(
            Arc::clone(sde),
            Arc::new(WorldRepository::with_demo_seed()),
        )
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
    } else {
        let ctx = ServerContext::new();
        let _ = SHARED_SDE.set(Arc::clone(&ctx.sde));
        ctx
    }
}

#[test]
fn crc32_cache_keys_and_object_caching_lifecycle_preserve_monotonic_timestamps() {
    // IEEE 802.3 CRC32 standard check value for "123456789" is 0xCBF43926
    assert_eq!(crc32_ieee(b""), 0);
    assert_eq!(crc32_ieee(b"123456789"), 0xCBF4_3926_u32 as i32);
    assert_ne!(crc32_ieee(b"123456789"), crc32_ieee(b"123456780"));

    assert_eq!(cache_object_key(&PyRep::String("config.Units".into())), "config.Units");
    assert_eq!(cache_object_key(&PyRep::Integer(42)), "42");
    assert_eq!(
        cache_object_key(&ServerContext::method_call_object_id("stationSvc", "GetAllianceSystems")),
        "(Method Call,server,(stationSvc,GetAllianceSystems))"
    );

    let ctx = test_ctx();
    let oid = ServerContext::method_call_object_id("stationSvc", "GetStation");
    let r1 = ctx.put_cached_object(oid.clone(), PyRep::Integer(100), true);
    assert!(matches!(
        r1.to_cached_object_pyrep(),
        PyRep::Object { ref type_name, .. }
            if type_name == eve_proto::marshal::V24_CACHED_OBJECT_CLASS
    ));
    assert!(matches!(
        r1.to_cache_hint_pyrep(),
        PyRep::Object { ref type_name, .. }
            if type_name == eve_proto::marshal::V24_CACHE_HINT_CLASS
    ));
    assert!(matches!(
        r1.to_cached_method_call_result_pyrep("run", Some("solarsystemid2")),
        PyRep::Object { ref type_name, .. }
            if type_name == eve_proto::marshal::V24_CACHED_METHOD_CALL_RESULT_CLASS
    ));
    assert!(matches!(
        r1.to_cached_method_call_hint_pyrep("run"),
        PyRep::Object { ref type_name, .. }
            if type_name == eve_proto::marshal::V24_CACHED_METHOD_CALL_RESULT_CLASS
    ));

    // Invalidate and re-insert: timestamp must strictly advance (> r1.timestamp)
    assert!(ctx.invalidate_cached_method("stationSvc", "GetStation"));
    assert!(!ctx.invalidate_cached_method("stationSvc", "GetStation"));
    let r2 = ctx.put_cached_object(oid, PyRep::Integer(200), false);
    assert!(r2.timestamp > r1.timestamp);

    let wrapped = ctx.cache_method_call_result("stationSvc", "GetStation", PyRep::Bool(true), "run", None);
    assert!(matches!(wrapped, PyRep::Object { .. }));
}

#[test]
fn get_or_populate_cached_object_synthesizes_all_bulk_sde_and_db_keys() {
    let ctx = test_ctx();
    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states.insert(
            30_000_142,
            SystemDynamicState {
                solar_system_id: 30_000_142,
                sov_alliance_id: Some(99_000_001),
                ..Default::default()
            },
        );
    });

    for key in [
        "config.Units",
        "config.MarketGroups",
        "config.Groups",
        "config.Categories",
        "config.StaticOwners",
        "(Method Call,server,(stationSvc,GetAllianceSystems))",
        "config.Races",
        "config.Bloodlines",
        "config.Ancestries",
        "config.Schools",
    ] {
        let oid = PyRep::String(key.into());
        let rec = ctx
            .get_or_populate_cached_object(&oid, true)
            .unwrap_or_else(|| panic!("expected synthesized cache entry for {key}"));
        assert!(!rec.marshaled_bytes.is_empty());
        // Second lookup hits in-memory object_cache directly
        let cached = ctx.get_or_populate_cached_object(&oid, true).unwrap();
        assert_eq!(cached.version, rec.version);
    }

    assert!(
        ctx.get_or_populate_cached_object(&PyRep::String("nonexistent.Key".into()), true)
            .is_none()
    );
}

#[test]
fn macho_packets_notification_targets_and_helper_emitters_work_end_to_end() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let sys_id = 30_000_142_u32;
    let st_id = 60_003_760_i64;
    let corp_id = 1_000_009_i64;
    let ally_id = 99_000_001_i64;
    let fleet_id = 700_001_i64;

    let (n_pkt, n_bytes) = ServerContext::build_macho_notification_packet(
        char_id,
        Some(1),
        "OnAccountChange",
        "charid",
        vec![PyRep::Integer(1)],
    );
    assert_eq!(n_pkt.packet_type, MachoPacketType::Notification);
    assert!(!n_bytes.is_empty());

    let (rsp_pkt, rsp_bytes) = ServerContext::build_macho_call_response_packet(
        Some("charMgr"),
        10,
        20,
        Some(1),
        PyRep::Bool(true),
    );
    assert_eq!(rsp_pkt.packet_type, MachoPacketType::CallRsp);
    assert!(!rsp_bytes.is_empty());

    let err_code = ["Invalid", "Argument"].concat();
    let (err_pkt, err_bytes) = ServerContext::build_macho_error_response_packet(
        None,
        10,
        20,
        Some(1),
        &err_code,
        "Invalid argument",
    );
    assert_eq!(err_pkt.packet_type, MachoPacketType::ErrorResponse);
    assert!(!err_bytes.is_empty());

    // Mark char_id online and configure corp/alliance/fleet/station/solar_system
    let uid = ctx.db.with_snapshot(|s| s.characters[&char_id].user_id);
    let _ = ctx
        .db
        .acquire_online_lock(uid, 101, Some(char_id), false, ctx.now_unix());
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = corp_id;
            ch.alliance_id = Some(ally_id);
            ch.solar_system_id = sys_id;
            ch.station_id = Some(st_id);
        }
        snap.fleets.insert(
            fleet_id,
            FleetRecord {
                fleet_id,
                boss_id: char_id,
                members: vec![char_id],
                ..Default::default()
            },
        );
    });

    assert!(ctx.is_character_online(char_id, uid));
    for target in [
        NotificationTarget::Character(char_id),
        NotificationTarget::Corporation(corp_id),
        NotificationTarget::Alliance(ally_id),
        NotificationTarget::Fleet(fleet_id),
        NotificationTarget::SolarSystem(sys_id),
        NotificationTarget::Station(st_id),
        NotificationTarget::All,
    ] {
        let rcpts = ctx.resolve_notification_recipients(&target);
        assert!(rcpts.iter().any(|&(cid, _)| cid == char_id));
    }

    // Queue cap eviction at MAX_PENDING_NOTIFICATIONS_PER_CHAR (128)
    for i in 0..(MAX_PENDING_NOTIFICATIONS_PER_CHAR + 5) {
        let _ = ctx.notify(
            NotificationTarget::Character(char_id),
            "OnAccountChange",
            "charid",
            vec![PyRep::Integer(i as i64)],
        );
    }
    let drained = ctx.drain_notifications_for_char(char_id);
    assert_eq!(drained.len(), MAX_PENDING_NOTIFICATIONS_PER_CHAR);
    assert!(ctx.drain_notifications_for_char(char_id).is_empty());

    // Exercise all notification helper methods & persistent character notifications
    assert_eq!(
        ctx.notify_remote_message(
            NotificationTarget::Character(char_id),
            "CustomInfo",
            vec![("msg", PyRep::String("hi".into()))]
        )
        .len(),
        1
    );
    assert_eq!(ctx.notify_account_change(char_id, "cash", char_id, 1000.0).len(), 1);
    assert_eq!(ctx.notify_item_change(char_id, PyRep::None, vec![(2, PyRep::Integer(1))]).len(), 1);
    assert_eq!(ctx.notify_skill_trained(char_id, 10, 3300, 3).len(), 1);
    assert_eq!(ctx.notify_skill_queue_paused(char_id).len(), 1);
    assert_eq!(ctx.notify_skill_queue_changed(char_id).len(), 1);
    assert_eq!(
        ctx.notify_damage_state_change(NotificationTarget::Character(char_id), 100, 0.9, 0.8, 1.0)
            .len(),
        1
    );
    assert_eq!(
        ctx.notify_godma_ship_effect(NotificationTarget::Character(char_id), 100, 10, true, 5000, 1)
            .len(),
        1
    );
    assert_eq!(ctx.notify_fleet_broadcast(fleet_id, "Target", char_id, 999).len(), 1);
    assert_eq!(
        ctx.notify_lsc_message(NotificationTarget::Character(char_id), "local", char_id, "o7")
            .len(),
        1
    );
    assert_eq!(ctx.notify_warp_to_asteroid_belt(char_id, 40_009_077).len(), 1);
    assert_eq!(ctx.notify_asteroid_terminated(sys_id, 700_111).len(), 1);
    assert_eq!(
        ctx.notify_mining_ledger_graph_setting_changed(char_id, "filter", PyRep::Bool(true))
            .len(),
        1
    );
    assert_eq!(
        ctx.notify_module_effect_started(char_id, 100_000_001, 500, 2048, None, None)
            .len(),
        1
    );
    assert_eq!(ctx.notify_module_online_state(char_id, 500, true).len(), 1);
    assert_eq!(ctx.notify_target_events(char_id, &[("add", 999, "")]).len(), 1);
    assert_eq!(
        ctx.notify_system_criminal_flag_updates(sys_id, &[char_id], &[], &[])
            .len(),
        1
    );
    let trashed_notifs = ctx.notify_fitted_item_trashed(char_id, 500, 2048, 1, 100_000_001, 11);
    assert_eq!(trashed_notifs.len(), 1);
    let PyRep::DBRow { columns, .. } = &trashed_notifs[0].args[0] else {
        panic!("notify_fitted_item_trashed must emit a DBRow, got {:?}", trashed_notifs[0].args[0]);
    };
    let col = |name: &str| columns.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone());
    let expected_type = ctx.sde.types.get(&2048).expect("2048 in SDE");
    assert_eq!(col("itemID"), Some(PyRep::Integer(500)));
    assert_eq!(col("typeID"), Some(PyRep::Integer(2048)));
    assert_eq!(col("ownerID"), Some(PyRep::Integer(char_id)));
    assert_eq!(col("locationID"), Some(PyRep::Integer(10)));
    assert_eq!(col("flagID"), Some(PyRep::Integer(147)));
    assert_eq!(col("quantity"), Some(PyRep::Integer(-1)));
    assert_eq!(col("groupID"), Some(PyRep::Integer(i64::from(expected_type.group_id))));
    assert_eq!(col("categoryID"), Some(PyRep::Integer(i64::from(expected_type.category_id))));
    assert_eq!(col("customInfo"), Some(PyRep::String(String::new())));

    let _ = ctx.drain_notifications_for_char(char_id);
    let rec = ctx.create_character_notification(1_000_009, char_id, 5, 1, "text: hello");
    assert_eq!(rec.receiver_id, char_id);
    ctx.sync_character_notifications_to_db(char_id);
    let _ = ctx.drain_notifications_for_char(char_id);
    ctx.deliver_unprocessed_notifications_on_login(char_id);
    assert_eq!(ctx.drain_notifications_for_char(char_id).len(), 1);

    ctx.sync_character_local_chat(char_id, Some(sys_id), 30_000_144);
    assert!(ctx.db.with_snapshot(|s| {
        s.chat_channels
            .get("solarsystemid2:30000144")
            .is_some_and(|c| c.members.contains(&char_id))
    }));
}

#[test]
fn ship_fitting_load_changes_corp_hangar_flags_and_trashed_quantity_boundaries() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let uid = ctx.db.with_snapshot(|s| s.characters[&char_id].user_id);
    let _ = ctx
        .db
        .acquire_online_lock(uid, 501, Some(char_id), false, ctx.now_unix());
    let _ = ctx.drain_notifications_for_char(char_id);

    // 1. `is_corp_hangar_flag` exact boundaries: `CorpDeliveries` (62) and `CorpSAG1..7` (116..=122)
    assert!(is_corp_hangar_flag(eve_db::InventoryFlag::CorpDeliveries));
    assert!(ServerContext::is_corp_hangar_flag(eve_db::InventoryFlag::CorpSAG1));
    assert!(is_corp_hangar_flag(eve_db::InventoryFlag::CorpSAG7));
    assert!(!is_corp_hangar_flag(eve_db::InventoryFlag::Hangar));
    assert!(!is_corp_hangar_flag(eve_db::InventoryFlag::Cargo));
    assert!(!is_corp_hangar_flag(eve_db::InventoryFlag::DroneBay));

    // 2. `notify_ship_fitting_load_changes` emits 2-entry `OnModuleAttributeChanges` list
    //    with `CPU_LOAD` (49) and `POWER_LOAD` (15), `new_val` at index 5, `old_val` at index 6.
    let load_notifs = ctx.notify_ship_fitting_load_changes(char_id, 100_000_001, 12.5, 45.0, 5.0, 20.0);
    assert_eq!(load_notifs.len(), 1);
    assert_eq!(load_notifs[0].event, "OnModuleAttributeChanges");
    let PyRep::List(ref entries) = load_notifs[0].args[0] else {
        panic!("expected List in OnModuleAttributeChanges");
    };
    assert_eq!(entries.len(), 2);
    let PyRep::Tuple(ref cpu_tup) = entries[0] else { panic!("expected Tuple") };
    assert_eq!(cpu_tup[0], PyRep::String("OnModuleAttributeChange".into()));
    assert_eq!(cpu_tup[1], PyRep::Integer(char_id));
    assert_eq!(cpu_tup[2], PyRep::Integer(100_000_001));
    assert_eq!(cpu_tup[3], PyRep::Integer(i64::from(eve_sde::attr_ids::CPU_LOAD)));
    assert_eq!(cpu_tup[5], PyRep::Real(45.0));
    assert_eq!(cpu_tup[6], PyRep::Real(12.5));
    assert_eq!(cpu_tup[7], PyRep::None);

    let PyRep::Tuple(ref pg_tup) = entries[1] else { panic!("expected Tuple") };
    assert_eq!(pg_tup[3], PyRep::Integer(i64::from(eve_sde::attr_ids::POWER_LOAD)));
    assert_eq!(pg_tup[5], PyRep::Real(20.0));
    assert_eq!(pg_tup[6], PyRep::Real(5.0));

    // 3. `notify_fitted_item_trashed` with `quantity > 1` preserves positive stack quantity,
    //    and unknown `type_id` falls back to `(group_id, category_id) == (0, 0)`.
    let stack_trashed = ctx.notify_fitted_item_trashed(char_id, 501, 999_999_999, 7, 100_000_001, 27);
    assert_eq!(stack_trashed.len(), 1);
    assert_eq!(stack_trashed[0].args[0].dbrow_get("quantity"), Some(&PyRep::Integer(7)));
    assert_eq!(stack_trashed[0].args[0].dbrow_get("groupID"), Some(&PyRep::Integer(0)));
    assert_eq!(stack_trashed[0].args[0].dbrow_get("categoryID"), Some(&PyRep::Integer(0)));
}

#[test]
fn issue_804_notify_module_effect_started_emits_on_special_fx_for_turrets_ewar_prop_and_repairs() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let ship_id = 200_125_728_i64;
    let target_id = 200_125_590_i64;
    let uid = ctx
        .db
        .with_snapshot(|s| s.characters.get(&char_id).map(|c| c.user_id).unwrap_or(1));
    ctx.db
        .acquire_online_lock(uid, 1, Some(char_id), false, ctx.now_unix())
        .unwrap();
    let _ = ctx.drain_notifications_for_char(char_id);

    let cases: &[(u32, Option<&str>, Option<i64>, &str, bool)] = &[
        (3640, Some("targetAttack"), Some(target_id), "effects.HybridFired", true),
        (526, Some("remoteWebifierFalloff"), Some(target_id), "effects.ModifyTargetSpeed", true),
        (447, Some("warpScramble"), Some(target_id), "effects.WarpScramble", true),
        (434, Some("moduleBonusAfterburner"), None, "effects.Afterburner", false),
        (399, Some("shieldBoosting"), None, "effects.ShieldBoosting", false),
        (3530, Some("armorRepair"), None, "effects.ArmorRepair", false),
    ];

    for &(type_id, eff_name, tgt, expected_guid, expected_offensive) in cases {
        let _ = ctx.drain_notifications_for_char(char_id);
        let mod_id = 200_126_000 + i64::from(type_id);
        let ret = ctx.notify_module_effect_started(
            char_id,
            ship_id,
            mod_id,
            type_id,
            eff_name,
            tgt,
        );
        assert_eq!(ret.len(), 1);
        assert_eq!(ret[0].event, "OnGodmaShipEffect");
        assert!(ret[0].args.get(7).and_then(|v| v.as_f64()).unwrap_or(0.0) > 0.0);
        assert_eq!(ret[0].args.get(8), Some(&PyRep::Integer(1000)));
        let all = ctx.drain_notifications_for_char(char_id);
        let sfx = all
            .iter()
            .find(|n| n.event == "OnSpecialFX")
            .unwrap_or_else(|| panic!("missing OnSpecialFX for type_id={type_id}"));
        assert_eq!(sfx.args.len(), 14);
        assert_eq!(sfx.args.first(), Some(&PyRep::Integer(ship_id)));
        assert_eq!(sfx.args.get(1), Some(&PyRep::Integer(mod_id)));
        assert_eq!(sfx.args.get(2), Some(&PyRep::Integer(i64::from(type_id))));
        assert_eq!(
            sfx.args.get(3),
            Some(&tgt.map(PyRep::Integer).unwrap_or(PyRep::None))
        );
        assert_eq!(sfx.args.get(4), Some(&PyRep::None));
        assert_eq!(sfx.args.get(5), Some(&PyRep::String(expected_guid.into())));
        assert_eq!(sfx.args.get(6), Some(&PyRep::Bool(expected_offensive)));
        assert_eq!(sfx.args.get(7), Some(&PyRep::Integer(1)));
        assert_eq!(sfx.args.get(8), Some(&PyRep::Integer(1)));
        assert!(sfx.args.get(9).and_then(|v| v.as_f64()).is_some());
        assert_eq!(sfx.args.get(10), Some(&PyRep::Integer(0)));
        assert!(sfx.args.get(11).and_then(|v| v.as_i64()).unwrap_or(0) > 0);
        assert_eq!(sfx.args.get(12), Some(&PyRep::Real(0.0)));
        assert_eq!(sfx.args.get(13), Some(&PyRep::None));
    }

    // Cloaking module (`effects.Cloak`) must be filtered out in `notify_module_effect_started`
    // because cloak activation/deactivation emits its own `OnSpecialFX` in `binding.rs`.
    let _ = ctx.drain_notifications_for_char(char_id);
    let cloak_ret = ctx.notify_module_effect_started(
        char_id,
        ship_id,
        200_199_999,
        11_370,
        Some("cloaking"),
        None,
    );
    assert_eq!(cloak_ret.len(), 1);
    assert_eq!(cloak_ret[0].args.get(8), Some(&PyRep::Integer(1000)));
    let cloak_notifs = ctx.drain_notifications_for_char(char_id);
    assert!(
        !cloak_notifs.iter().any(|n| n.event == "OnSpecialFX"),
        "notify_module_effect_started must filter out effects.Cloak OnSpecialFX"
    );

    // Issue #819: passive `online` (effect_id == 16) and `notify_godma_ship_effect(..., false, ...)`
    // emit `repeat == 0` at `args[8]`, while fitted modules with skill-modified attr 73 (`duration`)
    // or attr 51 (`speed`) emit the evaluated `duration_ms` at `args[7]` and `repeat == 1000` at `args[8]`.
    let online_ret = ctx.notify_module_effect_started(
        char_id,
        ship_id,
        200_199_998,
        2048,
        Some("online"),
        None,
    );
    assert_eq!(online_ret.len(), 1);
    assert_eq!(online_ret[0].args.get(1), Some(&PyRep::Integer(16)));
    assert_eq!(online_ret[0].args.get(8), Some(&PyRep::Integer(0)));

    let deact_ret = ctx.notify_godma_ship_effect(
        NotificationTarget::Character(char_id),
        200_199_998,
        10,
        false,
        0,
        0,
    );
    assert_eq!(deact_ret.len(), 1);
    assert_eq!(deact_ret[0].args.get(3), Some(&PyRep::Integer(0)));
    assert_eq!(deact_ret[0].args.get(4), Some(&PyRep::Integer(0)));
    assert_eq!(deact_ret[0].args.get(8), Some(&PyRep::Integer(0)));

    let fitted_ship_id = 100_000_001_i64;
    let fitted_ab_id = 200_199_901_i64;
    let fitted_turret_id = 200_199_902_i64;
    let mut fit = ctx.get_or_rebuild_ship_fitting(char_id, fitted_ship_id).expect("rebuild ship fitting");
    let _ = fit.fit_module(fitted_ab_id, 439, 19, &ctx.sde);
    let _ = fit.fit_module(fitted_turret_id, 2889, 28, &ctx.sde);
    if let Some(ab_mod) = fit.modules.get_mut(&fitted_ab_id) {
        ab_mod.dynamic_attribute_overrides.insert(73, 6_250.0);
    }
    ctx.store_ship_fitting(fitted_ship_id, fit.clone());

    let ab_eval_ms = fit.get_module_effective_attribute(fitted_ab_id, 73, &ctx.sde);
    assert!(ab_eval_ms > 0.0);
    let ab_started = ctx.notify_module_effect_started(
        char_id,
        fitted_ship_id,
        fitted_ab_id,
        439,
        Some("moduleBonusAfterburner"),
        None,
    );
    assert_eq!(ab_started[0].args.get(7), Some(&PyRep::Real(ab_eval_ms)));
    assert_eq!(ab_started[0].args.get(8), Some(&PyRep::Integer(1000)));

    let turret_eval_ms = fit.get_module_effective_attribute(fitted_turret_id, 51, &ctx.sde);
    let turret_sde_ms = ctx.sde.types[&2889].default_attributes[&51];
    assert!(turret_eval_ms > 0.0 && (turret_eval_ms - turret_sde_ms).abs() > 1.0);
    let turret_started = ctx.notify_module_effect_started(
        char_id,
        fitted_ship_id,
        fitted_turret_id,
        2889,
        Some("projectileFired"),
        Some(target_id),
    );
    assert_eq!(turret_started[0].args.get(7), Some(&PyRep::Real(turret_eval_ms)));
    assert_eq!(turret_started[0].args.get(8), Some(&PyRep::Integer(1000)));

    // Explicit evaluated cycle duration (e.g. shield_boost_multiplier) and single-cycle repeat=0
    // override SDE/default fallbacks in OnGodmaShipEffect (#819).
    let custom_ret = ctx.notify_module_effect_started_with_cycle(
        char_id,
        fitted_ship_id,
        fitted_ab_id,
        439,
        Some("moduleBonusAfterburner"),
        None,
        Some(0),
        Some(6_000.0),
    );
    assert_eq!(custom_ret[0].args.get(7), Some(&PyRep::Real(6_000.0)));
    assert_eq!(custom_ret[0].args.get(8), Some(&PyRep::Integer(0)));
}
