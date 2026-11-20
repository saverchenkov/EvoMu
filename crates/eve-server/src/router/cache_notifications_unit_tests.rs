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

    // 1. `is_corp_hangar_flag` exact boundaries: `CorpDeliveries` (62), `CorpSAG1..7` (116..=122), and `CapsuleerDeliveries` (187)
    assert!(is_corp_hangar_flag(eve_db::InventoryFlag::CorpDeliveries));
    assert!(is_corp_hangar_flag(eve_db::InventoryFlag::CapsuleerDeliveries));
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
        assert_eq!(sfx.args.get(10), Some(&PyRep::Integer(1000)));
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

fn account_change_summary(notif: &PendingMachoNotification) -> Option<(String, i64, f64, &str)> {
    if notif.event != "OnAccountChange" {
        return None;
    }
    match notif.args.as_slice() {
        [PyRep::String(k), PyRep::Integer(owner), PyRep::Real(bal)] => {
            Some((k.clone(), *owner, *bal, notif.id_type.as_str()))
        }
        _ => None,
    }
}

#[test]
fn commit_projects_personal_corp_divisions_and_plex_vault_and_rollback_emits_zero() {
    let ctx = test_ctx();
    let pilot_id = 90_000_001_i64;
    let corp_id = 98_000_772_i64;
    let now = 1_700_000_000_u64;
    let uid = ctx.db.with_snapshot(|s| s.characters[&pilot_id].user_id);

    ctx.db
        .acquire_online_lock(uid, 701, Some(pilot_id), true, now)
        .expect("online lock");

    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.insert(
            corp_id,
            eve_db::CorporationRecord::new(corp_id, "Commit Wallet Corp", "CWC", pilot_id),
        );
        if let Some(ch) = snap.characters.get_mut(&pilot_id) {
            ch.corporation_id = corp_id;
        }
    });
    let _ = ctx.drain_notifications_for_char(pilot_id);

    // 1. Committed mutation across personal ISK, all 7 corp divisions, and PLEX Vault projects OnAccountChange.
    let start_isk = ctx.db.with_snapshot(|s| s.characters[&pilot_id].balance);
    let committed_isk = eve_db::IskCents(start_isk.0 + 125_000);
    ctx.db
        .try_with_mut_snapshot(|snap| -> Result<(), String> {
            if let Some(ch) = snap.characters.get_mut(&pilot_id) {
                ch.balance = committed_isk;
            }
            if let Some(corp) = snap.corporations.get_mut(&corp_id) {
                for div_id in 1..=7_u8 {
                    corp.set_division_balance(div_id, eve_db::IskCents::from_isk(1_000 * i64::from(div_id)));
                }
            }
            snap.account_plex_vault.insert(uid, 320);
            Ok(())
        })
        .expect("commit ok");

    let committed = ctx.drain_notifications_for_char(pilot_id);
    let summaries: Vec<_> = committed.iter().filter_map(account_change_summary).collect();
    assert_eq!(summaries.len(), 9, "expected personal + 7 corp divs + plex: {summaries:?}");
    assert!(summaries.iter().any(|(k, owner, bal, id_type)| {
        k == "cash"
            && *owner == pilot_id
            && (*bal - committed_isk.to_wire_f64()).abs() < 1e-6
            && *id_type == "charid"
    }));
    assert!(summaries.iter().any(|(k, owner, bal, id_type)| {
        k == "plex" && *owner == pilot_id && (*bal - 320.0).abs() < 1e-6 && *id_type == "charid"
    }));
    for div_id in 1..=7_u8 {
        let expected_key = if div_id == 1 {
            "cash".to_string()
        } else {
            format!("cash{div_id}")
        };
        let expected_bal = 1_000.0 * f64::from(div_id);
        assert!(summaries.iter().any(|(k, owner, bal, id_type)| {
            *k == expected_key
                && *owner == corp_id
                && (*bal - expected_bal).abs() < 1e-6
                && *id_type == "corpid"
        }));
    }

    // 2. Rolled-back mutation emits zero OnAccountChange and leaves DeltaStateCache baseline intact.
    let err = ctx
        .db
        .try_with_mut_snapshot(|snap| -> Result<(), String> {
            if let Some(ch) = snap.characters.get_mut(&pilot_id) {
                ch.balance = eve_db::IskCents(committed_isk.0 + 9_999_900);
            }
            if let Some(corp) = snap.corporations.get_mut(&corp_id) {
                corp.set_division_balance(2, eve_db::IskCents::from_isk(999_999));
            }
            snap.account_plex_vault.insert(uid, 9_999);
            Err("intentional rollback".into())
        })
        .expect_err("rollback");
    assert_eq!(err, "intentional rollback");
    assert!(ctx.drain_notifications_for_char(pilot_id).is_empty());

    // Subsequent identical balance write matches preserved baseline (zero spurious deltas), while div 2 delta works.
    ctx.db
        .try_with_mut_snapshot(|snap| -> Result<(), String> {
            if let Some(corp) = snap.corporations.get_mut(&corp_id) {
                corp.set_division_balance(2, eve_db::IskCents::from_isk(2_500));
            }
            Ok(())
        })
        .expect("div 2 commit");
    let post_rollback = ctx.drain_notifications_for_char(pilot_id);
    let post_summaries: Vec<_> = post_rollback.iter().filter_map(account_change_summary).collect();
    assert_eq!(
        post_summaries,
        vec![("cash2".to_string(), corp_id, 2_500.0, "corpid")]
    );
}

#[tokio::test]
async fn staged_dispatch_tx_effects_dedup_and_legacy_same_epoch_ordering() {
    let ctx = test_ctx();
    let pilot_id = 90_000_001_i64;
    let now = 1_700_000_000_u64;
    let uid = ctx.db.with_snapshot(|s| s.characters[&pilot_id].user_id);

    ctx.db
        .acquire_online_lock(uid, 702, Some(pilot_id), true, now)
        .expect("online lock");
    let _ = ctx.drain_notifications_for_char(pilot_id);

    // 1. Within-tx TxNotificationEffect suppresses duplicate OnAccountChange from commit projection.
    let tx_bal = eve_db::IskCents::from_isk(55_000);
    ctx.db
        .tx("test_tx_effect_dedup", |wtx| -> Result<(), String> {
            if let Some(ch) = wtx.snap.characters.get_mut(&pilot_id) {
                ch.balance = tx_bal;
            }
            wtx.effects.notify_character(
                pilot_id,
                "OnAccountChange",
                "charid",
                vec![
                    PyRep::String("cash".into()),
                    PyRep::Integer(pilot_id),
                    PyRep::Real(tx_bal.to_wire_f64()),
                ],
            );
            Ok(())
        })
        .expect("tx commit");
    let after_tx = ctx.drain_notifications_for_char(pilot_id);
    assert_eq!(
        after_tx.iter().filter_map(account_change_summary).collect::<Vec<_>>(),
        vec![("cash".to_string(), pilot_id, 55_000.0, "charid")]
    );

    // Empty-args OnAccountChange marker effect also suppresses duplicate commit projection.
    ctx.db
        .tx("test_empty_marker_effect", |wtx| -> Result<(), String> {
            if let Some(ch) = wtx.snap.characters.get_mut(&pilot_id) {
                ch.balance = eve_db::IskCents::from_isk(56_000);
            }
            wtx.effects
                .notify_character(pilot_id, "OnAccountChange", "charid", Vec::new());
            Ok(())
        })
        .expect("marker tx");
    let marker_notifs = ctx.drain_notifications_for_char(pilot_id);
    assert_eq!(marker_notifs.len(), 1);
    assert!(marker_notifs[0].args.is_empty());

    // 2. Staged dispatch scope holds commit OnAccountChange until flushed, and drops on un-flushed abort.
    let mut rx = ctx.notify_tx.subscribe();
    with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
        ctx.db
            .try_with_mut_snapshot(|snap| -> Result<(), String> {
                if let Some(ch) = snap.characters.get_mut(&pilot_id) {
                    ch.balance = eve_db::IskCents::from_isk(60_000);
                }
                Ok(())
            })
            .expect("staged commit");
        // Not delivered before flush; same-epoch duplicate manual notify deduplicates,
        // while pre-settlement intermediate balance is inserted ahead of final committed balance.
        assert!(ctx.drain_notifications_for_char(pilot_id).is_empty());
        let dup = ctx.notify_account_change(pilot_id, "cash", pilot_id, 60_000.0);
        assert_eq!(dup.len(), 1);
        let pre = ctx.notify_account_change(pilot_id, "cash", pilot_id, 58_000.0);
        assert_eq!(pre.len(), 1);
        flush_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx);
    })
    .await;

    let flushed_raw = ctx.drain_notifications_for_char(pilot_id);
    let flushed: Vec<_> = flushed_raw.iter().filter_map(account_change_summary).collect();
    assert_eq!(
        flushed,
        vec![
            ("cash".to_string(), pilot_id, 58_000.0, "charid"),
            ("cash".to_string(), pilot_id, 60_000.0, "charid"),
        ]
    );
    let mut rx_bals = Vec::new();
    while let Ok((_, notif)) = rx.try_recv() {
        if let Some((_, _, bal, _)) = account_change_summary(&notif) {
            rx_bals.push(bal);
        }
    }
    assert_eq!(rx_bals, vec![58_000.0, 60_000.0]);

    // Unflushed staged rollback (failed RPC after a prior balance write would abort) delivers 0 on exit.
    with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
        let _ = ctx
            .db
            .try_with_mut_snapshot(|snap| -> Result<(), String> {
                if let Some(ch) = snap.characters.get_mut(&pilot_id) {
                    ch.balance = eve_db::IskCents::from_isk(61_000);
                }
                Err("aborted".into())
            });
    })
    .await;
    assert!(ctx.drain_notifications_for_char(pilot_id).is_empty());
    assert!(rx.try_recv().is_err());

    // 3. Unstaged same-epoch manual coordination deduplicates identical and prepends intermediate balance.
    ctx.db
        .try_with_mut_snapshot(|snap| -> Result<(), String> {
            if let Some(ch) = snap.characters.get_mut(&pilot_id) {
                ch.balance = eve_db::IskCents::from_isk(70_000);
            }
            Ok(())
        })
        .expect("unstaged commit");
    let _ = ctx.notify_account_change(pilot_id, "cash", pilot_id, 70_000.0);
    let _ = ctx.notify_account_change(pilot_id, "cash", pilot_id, 65_000.0);
    let unstaged_raw = ctx.drain_notifications_for_char(pilot_id);
    let unstaged: Vec<_> = unstaged_raw.iter().filter_map(account_change_summary).collect();
    assert_eq!(
        unstaged,
        vec![
            ("cash".to_string(), pilot_id, 65_000.0, "charid"),
            ("cash".to_string(), pilot_id, 70_000.0, "charid"),
        ]
    );
}

#[tokio::test]
async fn commit_listener_regression_bf01_bf02_bf03_bf04_matrix() {
    let ctx = test_ctx();
    let pilot_id = 90_000_001_i64;
    let corp_id = 98_000_773_i64;
    let now = 1_700_000_000_u64;
    let uid = ctx.db.with_snapshot(|s| s.characters[&pilot_id].user_id);

    ctx.db
        .acquire_online_lock(uid, 703, Some(pilot_id), true, now)
        .expect("online lock");
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.insert(
            corp_id,
            eve_db::CorporationRecord::new(corp_id, "BF Matrix Corp", "BFMC", pilot_id),
        );
        if let Some(ch) = snap.characters.get_mut(&pilot_id) {
            ch.corporation_id = corp_id;
        }
    });
    let _ = ctx.drain_notifications_for_char(pilot_id);

    // BF-01(b): Intervening non-wallet commit before legacy notify must still deduplicate.
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&pilot_id).expect("pilot").balance = eve_db::IskCents::from_isk(80_000);
    });
    ctx.db.with_mut_snapshot(|snap| {
        snap.quasar_state_blobs.insert("career_goal_fill".into(), "1".into());
    });
    let _ = ctx.notify_account_change(pilot_id, "cash", pilot_id, 80_000.0);
    let bf01_drain = ctx.drain_notifications_for_char(pilot_id);
    let bf01: Vec<_> = bf01_drain.iter().filter_map(account_change_summary).collect();
    assert_eq!(bf01, vec![("cash".to_string(), pilot_id, 80_000.0, "charid")]);

    // BF-02: Personal `"cash"` TxNotificationEffect must NOT suppress corp Div 1 `"cash"` delta.
    ctx.db
        .tx("bf02_personal_plus_corp_div1", |wtx| -> Result<(), String> {
            wtx.snap.characters.get_mut(&pilot_id).expect("pilot").balance =
                eve_db::IskCents::from_isk(81_000);
            wtx.snap
                .corporations
                .get_mut(&corp_id)
                .expect("corp")
                .set_division_balance(1, eve_db::IskCents::from_isk(2_500));
            wtx.effects.notify_character(
                pilot_id,
                "OnAccountChange",
                "charid",
                vec![
                    PyRep::String("cash".into()),
                    PyRep::Integer(pilot_id),
                    PyRep::Real(81_000.0),
                ],
            );
            Ok(())
        })
        .expect("bf02 tx");
    let bf02_drain = ctx.drain_notifications_for_char(pilot_id);
    let bf02: Vec<_> = bf02_drain.iter().filter_map(account_change_summary).collect();
    assert_eq!(bf02.len(), 2, "expected both personal and corp div1: {bf02:?}");
    assert!(bf02.contains(&("cash".to_string(), pilot_id, 81_000.0, "charid")));
    assert!(bf02.contains(&("cash".to_string(), corp_id, 2_500.0, "corpid")));

    // BF-03(1): Drained unstaged queue must NOT push duplicates or broadcast stale balances on notify_tx.
    let mut rx = ctx.notify_tx.subscribe();
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&pilot_id).expect("pilot").balance = eve_db::IskCents::from_isk(90_000);
    });
    assert_eq!(
        ctx.drain_notifications_for_char(pilot_id)
            .iter()
            .filter_map(account_change_summary)
            .collect::<Vec<_>>(),
        vec![("cash".to_string(), pilot_id, 90_000.0, "charid")]
    );
    while rx.try_recv().is_ok() {}
    let _ = ctx.notify_account_change(pilot_id, "cash", pilot_id, 90_000.0);
    let _ = ctx.notify_account_change(pilot_id, "cash", pilot_id, 85_000.0);
    assert!(ctx.drain_notifications_for_char(pilot_id).is_empty());
    assert!(rx.try_recv().is_err(), "stale/dup must not hit notify_tx after drain");

    // Undrained unstaged intermediate B_mid must NOT broadcast B_mid onto notify_tx after B_final.
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&pilot_id).expect("pilot").balance = eve_db::IskCents::from_isk(92_000);
    });
    while rx.try_recv().is_ok() {}
    let _ = ctx.notify_account_change(pilot_id, "cash", pilot_id, 91_000.0);
    assert!(rx.try_recv().is_err(), "unstaged B_mid must not overwrite live B_final on notify_tx");
    let undrained_raw = ctx.drain_notifications_for_char(pilot_id);
    let undrained: Vec<_> = undrained_raw.iter().filter_map(account_change_summary).collect();
    assert_eq!(
        undrained,
        vec![
            ("cash".to_string(), pilot_id, 91_000.0, "charid"),
            ("cash".to_string(), pilot_id, 92_000.0, "charid"),
        ]
    );

    // BF-03(2): Concurrent Task A unflushed staged commit must NOT suppress Task B's staged notify.
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel::<()>();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
    let task_a = with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
        ctx.db.with_mut_snapshot(|snap| {
            snap.characters.get_mut(&pilot_id).expect("pilot").balance =
                eve_db::IskCents::from_isk(95_000);
        });
        let _ = ready_tx.send(());
        let _ = release_rx.await;
    });
    let task_b = with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
        let _ = ready_rx.await;
        let _ = ctx.notify_account_change(pilot_id, "cash", pilot_id, 95_000.0);
        flush_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx);
        let _ = release_tx.send(());
    });
    tokio::join!(task_a, task_b);
    let task_b_raw = ctx.drain_notifications_for_char(pilot_id);
    let task_b_out: Vec<_> = task_b_raw.iter().filter_map(account_change_summary).collect();
    assert_eq!(
        task_b_out,
        vec![("cash".to_string(), pilot_id, 95_000.0, "charid")]
    );

    // BF-04: Folding legacy character PLEX with unchanged vault balance still emits OnAccountChange("plex").
    ctx.db.with_mut_snapshot(|snap| {
        snap.account_plex_vault.insert(uid, 100);
        snap.characters.get_mut(&pilot_id).expect("pilot").plex_balance = 100;
    });
    let _ = ctx.drain_notifications_for_char(pilot_id);
    ctx.db.with_mut_snapshot(|snap| {
        assert_eq!(snap.adjust_unified_plex_for_user(uid, -100), Some(100));
    });
    let bf04_raw = ctx.drain_notifications_for_char(pilot_id);
    let bf04: Vec<_> = bf04_raw.iter().filter_map(account_change_summary).collect();
    assert_eq!(bf04, vec![("plex".to_string(), pilot_id, 100.0, "charid")]);
}
