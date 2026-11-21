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

    // BF-01(a): Special-site/Homefront completion via `on_special_site_npc_destroyed_core`
    // followed by `ctx.notify_account_change` emits exactly one `OnAccountChange` per online pilot.
    let sys_id: u32 = 30_000_142;
    let hf_pilot = 90_091_001_i64;
    let hf_ship_id = 1_091_000_001_i64;
    ctx.db
        .provision_gm_account("hf_bf01a_pilot", "pw", hf_pilot, "HfBf01a");
    let hf_uid = ctx.db.with_snapshot(|s| s.characters[&hf_pilot].user_id);
    ctx.db
        .acquire_online_lock(hf_uid, 704, Some(hf_pilot), true, now)
        .expect("hf online lock");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&hf_pilot) {
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.fleet_id = None;
            ch.active_ship_id = hf_ship_id;
            ch.balance = eve_db::IskCents::from_isk(100_000);
        }
        snap.items.insert(
            hf_ship_id,
            eve_db::InventoryItemRecord {
                item_id: hf_ship_id,
                type_id: 587,
                owner_id: hf_pilot,
                location_id: i64::from(sys_id),
                flag: eve_db::InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    let _ = ctx.drain_notifications_for_char(hf_pilot);
    let ss = crate::services::pve_homefront_sites::spawn_homefront_site_for_dungeon(
        &ctx,
        sys_id,
        10_367,
    )
    .expect("spawn Homefront Suspicious Signal");
    let relay_bid = match ss.progress {
        crate::services::pve_homefront_sites::HomefrontArchetypeProgress::SuspiciousSignal {
            relay_ball_id,
            ..
        } => relay_ball_id,
        ref other => panic!("expected SuspiciousSignal progress, got {other:?}"),
    };
    let _ = ctx
        .systems
        .get_or_spawn(sys_id)
        .spawn_ball(eve_destiny::Ball {
            id: hf_ship_id,
            type_id: 587,
            position: ss.site_position + glam::DVec3::new(400.0, 0.0, 0.0),
            velocity: glam::DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 3.0,
            mass: 1.0e6,
            radius: 100.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        })
        .await;
    let mut hf_sess = eve_machonet::SessionState::new(hf_uid);
    hf_sess.user_id = Some(hf_uid);
    hf_sess.character_id = Some(hf_pilot);
    hf_sess.solar_system_id = Some(sys_id);
    hf_sess.ship_id = Some(hf_ship_id);
    crate::services::pve_homefront_sites::activate_homefront_acceleration_gate(
        &ctx,
        &hf_sess,
        ss.instance_id,
    )
    .await
    .expect("hf pilot enters gate");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(rec) = snap.dungeon_runtime.homefront_runtimes.get_mut(&ss.instance_id) {
            if let Some(c) = rec.contributions.get_mut(&hf_pilot) {
                c.damage_dealt = 2_000.0;
            }
            if let eve_db::service_domains::pve_sites::HomefrontProgressRecord::SuspiciousSignal {
                arrays_hacked,
                vulnerability_window_secs,
                ..
            } = &mut rec.progress
            {
                *arrays_hacked = [true; 3];
                *vulnerability_window_secs = 90;
            }
        }
    });
    let mut hf_rx = ctx.notify_tx.subscribe();
    let acct_changes = crate::services::pve_special_sites::on_special_site_npc_destroyed_core(
        &ctx.systems,
        &ctx.db,
        &ctx.sde,
        sys_id,
        relay_bid,
        Some(hf_pilot),
    )
    .await;
    assert_eq!(acct_changes.len(), 1);
    assert_eq!(acct_changes[0].0, hf_pilot);
    let expected_hf_bal = acct_changes[0].1;
    assert!(expected_hf_bal > 100_000.0);
    for (cid, bal) in acct_changes {
        let _ = ctx.notify_account_change(cid, "cash", cid, bal);
    }
    fn drain_owned_changes(
        ctx: &ServerContext,
        cid: i64,
    ) -> Vec<(String, i64, f64, String)> {
        ctx.drain_notifications_for_char(cid)
            .iter()
            .filter_map(|n| {
                account_change_summary(n)
                    .map(|(k, owner, bal, idt)| (k, owner, bal, idt.to_string()))
            })
            .collect()
    }
    fn recv_owned_changes(
        rx: &mut tokio::sync::broadcast::Receiver<(i64, PendingMachoNotification)>,
        cid: i64,
    ) -> Vec<(String, i64, f64, String)> {
        let mut out = Vec::new();
        while let Ok((target_cid, notif)) = rx.try_recv() {
            if target_cid == cid {
                if let Some((k, owner, bal, idt)) = account_change_summary(&notif) {
                    out.push((k, owner, bal, idt.to_string()));
                }
            }
        }
        out
    }

    assert_eq!(
        drain_owned_changes(&ctx, hf_pilot),
        vec![("cash".to_string(), hf_pilot, expected_hf_bal, "charid".to_string())]
    );
    assert_eq!(
        recv_owned_changes(&mut hf_rx, hf_pilot),
        vec![("cash".to_string(), hf_pilot, expected_hf_bal, "charid".to_string())]
    );

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

    // BF-03(3): Sequential same-wallet commits (B1 then B2) followed by legacy emits for B1 and B2
    // must deduplicate across ALL records in matching (both unstaged and staged paths).
    while rx.try_recv().is_ok() {}
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&pilot_id).expect("pilot").balance =
            eve_db::IskCents::from_isk(96_000);
    });
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&pilot_id).expect("pilot").balance =
            eve_db::IskCents::from_isk(97_000);
    });
    let _ = ctx.notify_account_change(pilot_id, "cash", pilot_id, 96_000.0);
    let _ = ctx.notify_account_change(pilot_id, "cash", pilot_id, 97_000.0);
    assert_eq!(
        drain_owned_changes(&ctx, pilot_id),
        vec![
            ("cash".to_string(), pilot_id, 96_000.0, "charid".to_string()),
            ("cash".to_string(), pilot_id, 97_000.0, "charid".to_string()),
        ]
    );
    assert_eq!(
        recv_owned_changes(&mut rx, pilot_id),
        vec![
            ("cash".to_string(), pilot_id, 96_000.0, "charid".to_string()),
            ("cash".to_string(), pilot_id, 97_000.0, "charid".to_string()),
        ]
    );

    // Also cover unstaged B1 drained before B2 commits, followed by late legacy emits for B1 and B2.
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&pilot_id).expect("pilot").balance =
            eve_db::IskCents::from_isk(98_000);
    });
    let _ = ctx.drain_notifications_for_char(pilot_id);
    while rx.try_recv().is_ok() {}
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&pilot_id).expect("pilot").balance =
            eve_db::IskCents::from_isk(99_000);
    });
    let _ = ctx.notify_account_change(pilot_id, "cash", pilot_id, 98_000.0);
    let _ = ctx.notify_account_change(pilot_id, "cash", pilot_id, 99_000.0);
    assert_eq!(
        drain_owned_changes(&ctx, pilot_id),
        vec![("cash".to_string(), pilot_id, 99_000.0, "charid".to_string())]
    );
    assert_eq!(
        recv_owned_changes(&mut rx, pilot_id),
        vec![("cash".to_string(), pilot_id, 99_000.0, "charid".to_string())]
    );

    // Staged two-commit scope [B1, B2] + legacy emits for B1 and B2 must flush 1 copy each.
    while rx.try_recv().is_ok() {}
    with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
        ctx.db.with_mut_snapshot(|snap| {
            snap.characters.get_mut(&pilot_id).expect("pilot").balance =
                eve_db::IskCents::from_isk(101_000);
        });
        ctx.db.with_mut_snapshot(|snap| {
            snap.characters.get_mut(&pilot_id).expect("pilot").balance =
                eve_db::IskCents::from_isk(102_000);
        });
        let _ = ctx.notify_account_change(pilot_id, "cash", pilot_id, 101_000.0);
        let _ = ctx.notify_account_change(pilot_id, "cash", pilot_id, 102_000.0);
        assert!(rx.try_recv().is_err(), "staged emits stay buffered until flush");
        flush_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx);
    })
    .await;
    assert_eq!(
        drain_owned_changes(&ctx, pilot_id),
        vec![
            ("cash".to_string(), pilot_id, 101_000.0, "charid".to_string()),
            ("cash".to_string(), pilot_id, 102_000.0, "charid".to_string()),
        ]
    );
    assert_eq!(
        recv_owned_changes(&mut rx, pilot_id),
        vec![
            ("cash".to_string(), pilot_id, 101_000.0, "charid".to_string()),
            ("cash".to_string(), pilot_id, 102_000.0, "charid".to_string()),
        ]
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

#[tokio::test]
async fn intermediate_insertion_preserves_monotonic_seq_ids_and_prevents_rpc_replay() {
    let ctx = test_ctx();
    let pilot_id = 90_000_001_i64;
    let now = 1_700_000_000_u64;
    let (uid, corp_id, sol_id, ship_id) = ctx.db.with_snapshot(|s| {
        let ch = &s.characters[&pilot_id];
        (ch.user_id, ch.corporation_id, ch.solar_system_id, ch.active_ship_id)
    });
    ctx.db
        .acquire_online_lock(uid, 705, Some(pilot_id), true, now)
        .expect("online lock");
    let _ = ctx.drain_notifications_for_char(pilot_id);

    let mut sess = eve_machonet::SessionState {
        session_id: 705,
        user_id: Some(uid),
        character_id: Some(pilot_id),
        corporation_id: Some(corp_id),
        solar_system_id: Some(sol_id),
        station_id: Some(60_003_760),
        ship_id: Some(ship_id),
        role: eve_machonet::service_roles::ROLE_PLAYER,
        live_notify_listener: false,
        ..Default::default()
    };
    ctx.sync_session(&mut sess);
    let kw = HashMap::new();

    // 1. Staged intermediate insertions (`B_mid_1`, `B_mid_2` ahead of committed `B_final`).
    with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
        ctx.db.with_mut_snapshot(|snap| {
            snap.characters.get_mut(&pilot_id).expect("pilot").balance =
                eve_db::IskCents::from_isk(110_000);
        });
        let _ = ctx.notify_account_change(pilot_id, "cash", pilot_id, 108_000.0);
        let _ = ctx.notify_account_change(pilot_id, "cash", pilot_id, 109_000.0);
        flush_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx);
    })
    .await;

    let staged_snapshot: Vec<(u64, f64)> = {
        let queues = ctx.pending_notifications.read().expect("read pending");
        queues[&pilot_id]
            .iter()
            .filter_map(|n| account_change_summary(n).map(|(_, _, bal, _)| (n.seq_id, bal)))
            .collect()
    };
    assert_eq!(
        staged_snapshot.iter().map(|&(_, b)| b).collect::<Vec<_>>(),
        vec![108_000.0, 109_000.0, 110_000.0]
    );
    assert!(
        staged_snapshot.windows(2).all(|w| w[0].0 < w[1].0),
        "staged queue seq_ids must remain strictly ascending: {staged_snapshot:?}"
    );
    assert_eq!(
        ctx.latest_notification_seq_for_char(pilot_id),
        staged_snapshot.last().unwrap().0
    );

    sess.notifications.clear();
    ctx.dispatch_rpc(&mut sess, "petitioner", None, "MayPetition", &[], &kw, None)
        .await
        .expect("MayPetition read-only RPC after staged intermediate");
    assert!(
        sess.notifications.is_empty(),
        "subsequent read-only RPC must not replay stale staged B_mid: {:?}",
        sess.notifications
    );
    let _ = ctx.drain_notifications_for_char(pilot_id);

    // 2. Unstaged intermediate insertions (`B_mid_1`, `B_mid_2` ahead of committed `B_final`).
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&pilot_id).expect("pilot").balance =
            eve_db::IskCents::from_isk(115_000);
    });
    let _ = ctx.notify_account_change(pilot_id, "cash", pilot_id, 112_000.0);
    let _ = ctx.notify_account_change(pilot_id, "cash", pilot_id, 114_000.0);

    let unstaged_snapshot: Vec<(u64, f64)> = {
        let queues = ctx.pending_notifications.read().expect("read pending");
        queues[&pilot_id]
            .iter()
            .filter_map(|n| account_change_summary(n).map(|(_, _, bal, _)| (n.seq_id, bal)))
            .collect()
    };
    assert_eq!(
        unstaged_snapshot.iter().map(|&(_, b)| b).collect::<Vec<_>>(),
        vec![112_000.0, 114_000.0, 115_000.0]
    );
    assert!(
        unstaged_snapshot.windows(2).all(|w| w[0].0 < w[1].0),
        "unstaged queue seq_ids must remain strictly ascending: {unstaged_snapshot:?}"
    );
    assert_eq!(
        ctx.latest_notification_seq_for_char(pilot_id),
        unstaged_snapshot.last().unwrap().0
    );

    sess.notifications.clear();
    ctx.dispatch_rpc(&mut sess, "petitioner", None, "MayPetition", &[], &kw, None)
        .await
        .expect("MayPetition read-only RPC after unstaged intermediate");
    assert!(
        sess.notifications.is_empty(),
        "subsequent read-only RPC must not replay stale unstaged B_mid: {:?}",
        sess.notifications
    );
    let _ = ctx.drain_notifications_for_char(pilot_id);

    // 3. Defense-in-depth: `latest_notification_seq_for_char` takes `.max()` even when a lower-seq
    // non-account notification follows `B_final` before an intermediate `B_mid` arrives.
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&pilot_id).expect("pilot").balance =
            eve_db::IskCents::from_isk(119_000);
    });
    {
        let mut queues = ctx.pending_notifications.write().expect("write pending");
        let n = assemble_pending_notification(
            pilot_id,
            Some(uid),
            "OnSkillForcedRefresh",
            "charid",
            vec![],
            0,
        );
        push_pending_notification_capped(queues.entry(pilot_id).or_default(), n);
    }
    let _ = ctx.notify_account_change(pilot_id, "cash", pilot_id, 118_500.0);
    let max_seq = {
        let queues = ctx.pending_notifications.read().expect("read pending");
        queues[&pilot_id].iter().map(|n| n.seq_id).max().unwrap_or(0)
    };
    assert_eq!(ctx.latest_notification_seq_for_char(pilot_id), max_seq);
    sess.notifications.clear();
    ctx.dispatch_rpc(&mut sess, "petitioner", None, "MayPetition", &[], &kw, None)
        .await
        .expect("MayPetition read-only RPC after non-tail bump");
    assert!(sess.notifications.is_empty());
    let _ = ctx.drain_notifications_for_char(pilot_id);

    // 4. Unstaged cross-epoch round-trip (`116_000` drained -> `117_000` drained -> `118_000` queued):
    // intermediate `116_000.0` before `118_000.0` must NOT alias the stale drained `116_000` commit.
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&pilot_id).expect("pilot").balance =
            eve_db::IskCents::from_isk(116_000);
    });
    let _ = ctx.drain_notifications_for_char(pilot_id);
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&pilot_id).expect("pilot").balance =
            eve_db::IskCents::from_isk(117_000);
    });
    let _ = ctx.drain_notifications_for_char(pilot_id);
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&pilot_id).expect("pilot").balance =
            eve_db::IskCents::from_isk(118_000);
    });
    let _ = ctx.notify_account_change(pilot_id, "cash", pilot_id, 116_000.0);
    let rt_snapshot: Vec<(u64, f64)> = {
        let queues = ctx.pending_notifications.read().expect("read pending");
        queues[&pilot_id]
            .iter()
            .filter_map(|n| account_change_summary(n).map(|(_, _, bal, _)| (n.seq_id, bal)))
            .collect()
    };
    assert_eq!(
        rt_snapshot.iter().map(|&(_, b)| b).collect::<Vec<_>>(),
        vec![116_000.0, 118_000.0]
    );
    assert!(rt_snapshot[0].0 < rt_snapshot[1].0);
    let _ = ctx.drain_notifications_for_char(pilot_id);
}

#[test]
fn item_compare_only_emits_zero_projected_notifs_and_tracks_per_helper_counts() {
    use crate::router::delta_item_compare::{
        flush_item_compare_summary, item_compare_summary, HelperCompareCounts,
    };

    let ctx = test_ctx();
    let pilot_id = 90_000_001_i64;
    let station_id = 60_003_760_i64;
    let now = 1_700_000_100_u64;
    let uid = ctx.db.with_snapshot(|s| s.characters[&pilot_id].user_id);
    ctx.db
        .acquire_online_lock(uid, 9_301, Some(pilot_id), true, now)
        .expect("online lock");
    let _ = ctx.drain_notifications_for_char(pilot_id);

    // 1. Committed item insert does NOT push any OnItemChange/OnMultipleItemChange in compare-only mode.
    let item_id = 991_100_001_i64;
    let rec = eve_db::InventoryItemRecord {
        item_id,
        type_id: 34,
        owner_id: pilot_id,
        location_id: station_id,
        flag: eve_db::InventoryFlag::Hangar,
        quantity: 100,
        singleton: false,
        version: 1,
        ..Default::default()
    };
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(item_id, rec.clone());
    });
    assert!(
        ctx.drain_notifications_for_char(pilot_id).is_empty(),
        "compare-only commit projection must not send OnItemChange"
    );
    let s1 = item_compare_summary(&ctx);
    assert_eq!(s1.item_change.pending, 1);
    assert_eq!(s1.item_change.matched, 0);

    // Unrelated non-item DB commits must NEVER prune pending item compare entries (`#1537` rule).
    for step in 0..40_i64 {
        ctx.db.with_mut_snapshot(|snap| {
            snap.characters.get_mut(&pilot_id).expect("pilot").balance =
                eve_db::IskCents::from_isk(200_000 + step);
        });
    }
    let _ = ctx.drain_notifications_for_char(pilot_id);
    assert_eq!(item_compare_summary(&ctx).item_change.pending, 1);

    // Matching hand-written emit resolves pending -> matched under helper name.
    ctx.emit_item_change_routed(pilot_id, &rec, vec![(3, PyRep::Integer(0))]);
    let sent1 = ctx.drain_notifications_for_char(pilot_id);
    assert!(sent1.iter().any(|n| n.event == "OnItemChange"));
    let s2 = item_compare_summary(&ctx);
    assert_eq!(s2.item_change.pending, 0);
    assert_eq!(s2.item_change.matched, 1);
    assert_eq!(
        s2.item_change.by_helper.get("emit_item_change_routed"),
        Some(&HelperCompareCounts {
            matched: 1,
            hand_written_only: 0,
            projected_only: 0,
        })
    );

    // 2. Stack update matched via notify_item_record_change + delete matched via emit_item_deleted_routed.
    let mut rec_updated = rec.clone();
    rec_updated.quantity = 40;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&item_id).expect("item").quantity = 40;
    });
    let _ = ctx.notify_item_record_change(pilot_id, &rec_updated, vec![(9, PyRep::Integer(100))]);
    let _ = ctx.drain_notifications_for_char(pilot_id);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.remove(&item_id);
    });
    ctx.emit_item_deleted_routed(pilot_id, &rec_updated);
    let _ = ctx.drain_notifications_for_char(pilot_id);

    // 3. Virtual fitted-item trash with no DB mutation counts as hand_written_only under its helper.
    let _ = ctx.notify_fitted_item_trashed(pilot_id, 991_100_999, 34, 1, 1_000_001, 27);
    let _ = ctx.drain_notifications_for_char(pilot_id);

    // 4. Mismatched hand-written qty + un-emitted DB commit test `hand_written_only` and `projected_only`.
    let bad_id = 991_100_002_i64;
    let bad_rec = eve_db::InventoryItemRecord {
        item_id: bad_id,
        type_id: 34,
        owner_id: pilot_id,
        location_id: station_id,
        flag: eve_db::InventoryFlag::Hangar,
        quantity: 55,
        singleton: false,
        version: 1,
        ..Default::default()
    };
    let unemitted_id = 991_100_003_i64;
    let unemitted_rec = eve_db::InventoryItemRecord {
        item_id: unemitted_id,
        quantity: 77,
        ..bad_rec.clone()
    };
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(bad_id, bad_rec.clone());
        snap.items.insert(unemitted_id, unemitted_rec);
    });
    let mut wrong_rec = bad_rec;
    wrong_rec.quantity = 999;
    let wrong_row = crate::router::delta_projection::build_inv_item_dbrow(&wrong_rec, &ctx.sde, "");
    let _ = ctx.notify_item_change(pilot_id, wrong_row, vec![(3, PyRep::Integer(0))]);
    let _ = ctx.drain_notifications_for_char(pilot_id);

    let flushed = flush_item_compare_summary(&ctx);
    assert_eq!(flushed.item_change.pending, 0);
    assert_eq!(flushed.item_change.matched, 3);
    assert_eq!(flushed.item_change.hand_written_only, 2);
    assert_eq!(flushed.item_change.projected_only, 2);
    assert_eq!(
        flushed.item_change.by_helper.get("notify_item_record_change"),
        Some(&HelperCompareCounts {
            matched: 1,
            hand_written_only: 0,
            projected_only: 0,
        })
    );
    assert_eq!(
        flushed.item_change.by_helper.get("emit_item_deleted_routed"),
        Some(&HelperCompareCounts {
            matched: 1,
            hand_written_only: 0,
            projected_only: 0,
        })
    );
    assert_eq!(
        flushed.item_change.by_helper.get("notify_fitted_item_trashed"),
        Some(&HelperCompareCounts {
            matched: 0,
            hand_written_only: 1,
            projected_only: 0,
        })
    );
    assert_eq!(
        flushed.item_change.by_helper.get("notify_item_change"),
        Some(&HelperCompareCounts {
            matched: 0,
            hand_written_only: 1,
            projected_only: 1,
        })
    );
    assert_eq!(
        flushed.item_change.by_helper.get("<no_helper>"),
        Some(&HelperCompareCounts {
            matched: 0,
            hand_written_only: 0,
            projected_only: 1,
        })
    );
}

#[test]
fn item_compare_tracks_blueprints_and_explicit_in_tx_effects() {
    use crate::router::delta_item_compare::{flush_item_compare_summary, HelperCompareCounts};

    let ctx = test_ctx();
    let pilot_id = 90_000_001_i64;
    let station_id = 60_003_760_i64;
    let now = 1_700_000_200_u64;
    let uid = ctx.db.with_snapshot(|s| s.characters[&pilot_id].user_id);
    ctx.db
        .acquire_online_lock(uid, 9_302, Some(pilot_id), true, now)
        .expect("online lock");
    let _ = ctx.drain_notifications_for_char(pilot_id);

    // 1. Blueprint creation compares both OnItemChange and OnBlueprintsUpdated.
    let bp_type_id = *ctx.sde.blueprints.keys().next().expect("bp type in SDE");
    let bp_rec = eve_db::InventoryItemRecord {
        item_id: 991_200_001,
        type_id: bp_type_id,
        owner_id: pilot_id,
        location_id: station_id,
        flag: eve_db::InventoryFlag::Hangar,
        quantity: 1,
        singleton: true,
        is_blueprint_copy: true,
        blueprint_runs_remaining: Some(5),
        version: 1,
        ..Default::default()
    };
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(bp_rec.item_id, bp_rec.clone());
    });
    ctx.notify_item_mutation(pilot_id, None, Some(&bp_rec));
    let _ = ctx.drain_notifications_for_char(pilot_id);

    // 2. Explicit in-tx TxNotificationEffect skips compare pending expectations cleanly.
    let tx_item = eve_db::InventoryItemRecord {
        item_id: 991_200_002,
        type_id: 34,
        owner_id: pilot_id,
        location_id: station_id,
        flag: eve_db::InventoryFlag::Hangar,
        quantity: 300,
        singleton: false,
        version: 1,
        ..Default::default()
    };
    let tx_row = crate::router::delta_projection::build_inv_item_dbrow(&tx_item, &ctx.sde, "");
    let change_dict = PyRep::Dict(vec![(PyRep::Integer(3), PyRep::Integer(0))]);
    ctx.db
        .tx("explicit_in_tx_item_effect_test", |tx| -> Result<(), eve_db::TxError> {
            tx.snap.items.insert(tx_item.item_id, tx_item.clone());
            tx.effects.notify_character(
                pilot_id,
                "OnItemChange",
                "charid",
                vec![tx_row.clone(), change_dict.clone()],
            );
            Ok(())
        })
        .expect("tx");
    let tx_sent = ctx.drain_notifications_for_char(pilot_id);
    assert_eq!(
        tx_sent.iter().filter(|n| n.event == "OnItemChange").count(),
        1
    );

    let flushed = flush_item_compare_summary(&ctx);
    assert_eq!(flushed.explicit_in_tx_effects, 1);
    assert_eq!(flushed.item_change.matched, 1);
    assert_eq!(flushed.item_change.projected_only, 0);
    assert_eq!(
        flushed.blueprints_updated.by_helper.get("notify_item_mutation"),
        Some(&HelperCompareCounts {
            matched: 1,
            hand_written_only: 0,
            projected_only: 0,
        })
    );
}

#[test]
fn item_compare_off_switch_skips_projection_and_recording() {
    use crate::router::delta_item_compare::{
        flush_item_compare_summary, item_compare_enabled_from_str, item_compare_summary,
        log_item_compare_summary, set_item_compare_override_for_test,
    };

    assert!(item_compare_enabled_from_str(None));
    assert!(item_compare_enabled_from_str(Some("")));
    assert!(item_compare_enabled_from_str(Some("1")));
    assert!(item_compare_enabled_from_str(Some("true")));
    assert!(!item_compare_enabled_from_str(Some("0")));
    assert!(!item_compare_enabled_from_str(Some("false")));
    assert!(!item_compare_enabled_from_str(Some(" off ")));
    assert!(!item_compare_enabled_from_str(Some("NO")));

    let ctx = test_ctx();
    set_item_compare_override_for_test(&ctx, Some(false), None);
    let pilot_id = 90_000_001_i64;
    let station_id = 60_003_760_i64;
    let uid = ctx.db.with_snapshot(|s| s.characters[&pilot_id].user_id);
    ctx.db
        .acquire_online_lock(uid, 9_303, Some(pilot_id), true, 1_700_000_300)
        .expect("online lock");
    let _ = ctx.drain_notifications_for_char(pilot_id);

    let rec = eve_db::InventoryItemRecord {
        item_id: 991_300_001,
        type_id: 34,
        owner_id: pilot_id,
        location_id: station_id,
        flag: eve_db::InventoryFlag::Hangar,
        quantity: 25,
        singleton: false,
        version: 1,
        ..Default::default()
    };
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(rec.item_id, rec.clone());
    });
    assert_eq!(item_compare_summary(&ctx).item_change.pending, 0);

    let _ = ctx.notify_item_record_change(pilot_id, &rec, vec![(3, PyRep::Integer(0))]);
    let delivered = ctx.drain_notifications_for_char(pilot_id);
    assert_eq!(
        delivered.iter().filter(|n| n.event == "OnItemChange").count(),
        1,
        "off-switch must not alter live hand-written notification delivery"
    );
    log_item_compare_summary(&ctx);
    let summary = flush_item_compare_summary(&ctx);
    assert_eq!(summary, crate::router::delta_item_compare::ItemCompareSummary::default());
}

#[test]
fn item_compare_handles_on_multiple_item_change_without_double_counting_and_writes_ndjson() {
    use crate::router::delta_item_compare::{
        flush_item_compare_summary, log_item_compare_summary, set_item_compare_override_for_test,
        HelperCompareCounts,
    };

    let report_path = std::env::temp_dir().join(format!(
        "evomu_item_compare_test_{}_{:?}.ndjson",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_file(&report_path);

    {
        let ctx = test_ctx();
        set_item_compare_override_for_test(&ctx, Some(true), Some(Some(report_path.clone())));
        let pilot_id = 90_000_001_i64;
        let station_id = 60_003_760_i64;
        let uid = ctx.db.with_snapshot(|s| s.characters[&pilot_id].user_id);
        ctx.db
            .acquire_online_lock(uid, 9_304, Some(pilot_id), true, 1_700_000_400)
            .expect("online lock");
        let _ = ctx.drain_notifications_for_char(pilot_id);

        // 1. Paired OnItemChange + OnMultipleItemChange via emit_item_change_routed must count once.
        let single_rec = eve_db::InventoryItemRecord {
            item_id: 991_400_001,
            type_id: 34,
            owner_id: pilot_id,
            location_id: station_id,
            flag: eve_db::InventoryFlag::Hangar,
            quantity: 10,
            singleton: false,
            version: 1,
            ..Default::default()
        };
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(single_rec.item_id, single_rec.clone());
        });
        ctx.emit_item_change_routed(pilot_id, &single_rec, vec![(3, PyRep::Integer(0))]);
        let routed_sent = ctx.drain_notifications_for_char(pilot_id);
        assert!(routed_sent.iter().any(|n| n.event == "OnItemChange"));
        assert!(routed_sent.iter().any(|n| n.event == "OnMultipleItemChange"));

        // 2. Standalone OnMultipleItemChange (e.g. saved fitting multi-module fit) matches each row.
        let multi_a = eve_db::InventoryItemRecord {
            item_id: 991_400_002,
            type_id: 34,
            owner_id: pilot_id,
            location_id: station_id,
            flag: eve_db::InventoryFlag::Hangar,
            quantity: 5,
            singleton: false,
            version: 1,
            ..Default::default()
        };
        let multi_b = eve_db::InventoryItemRecord {
            item_id: 991_400_003,
            quantity: 7,
            ..multi_a.clone()
        };
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(multi_a.item_id, multi_a.clone());
            snap.items.insert(multi_b.item_id, multi_b.clone());
        });
        let rows = vec![
            crate::router::delta_projection::build_inv_item_dbrow(&multi_a, &ctx.sde, ""),
            crate::router::delta_projection::build_inv_item_dbrow(&multi_b, &ctx.sde, ""),
        ];
        let _ = ctx.emit_typed(
            NotificationTarget::Character(pilot_id),
            crate::router::notify_registry::OnMultipleItemChange {
                items: rows,
                change: PyRep::Dict(vec![(PyRep::Integer(3), PyRep::Integer(0))]),
            },
        );
        let _ = ctx.drain_notifications_for_char(pilot_id);

        log_item_compare_summary(&ctx);
        let summary = flush_item_compare_summary(&ctx);
        assert_eq!(summary.item_change.matched, 3);
        assert_eq!(summary.item_change.hand_written_only, 0);
        assert_eq!(summary.item_change.projected_only, 0);
        assert_eq!(
            summary.item_change.by_helper.get("emit_item_change_routed"),
            Some(&HelperCompareCounts {
                matched: 1,
                hand_written_only: 0,
                projected_only: 0,
            })
        );
        assert_eq!(
            summary.item_change.by_helper.get("OnMultipleItemChange"),
            Some(&HelperCompareCounts {
                matched: 2,
                hand_written_only: 0,
                projected_only: 0,
            })
        );
    }

    let raw = std::fs::read_to_string(&report_path).expect("read NDJSON report");
    let lines: Vec<&str> = raw.lines().collect();
    assert_eq!(
        lines.len(),
        1,
        "shutdown log_item_compare_summary + drop must append once: {raw}"
    );
    let parsed: serde_json::Value = serde_json::from_str(lines[0]).expect("valid NDJSON line");
    assert_eq!(parsed["item_change"]["matched"], 3);
    assert_eq!(parsed["item_change"]["hand_written_only"], 0);
    assert_eq!(parsed["item_change"]["projected_only"], 0);
    assert_eq!(
        parsed["item_change"]["by_helper"]["OnMultipleItemChange"]["matched"],
        2
    );
    let _ = std::fs::remove_file(&report_path);
}

#[tokio::test]
async fn item_compare_tracks_dispatch_tick_and_neither_scopes() {
    use crate::router::delta_item_compare::*;
    let report_path = std::env::temp_dir().join(format!("evomu_scope_{}_{:?}.ndjson", std::process::id(), std::thread::current().id()));
    let _ = std::fs::remove_file(&report_path);
    let ctx = test_ctx();
    set_item_compare_override_for_test(&ctx, Some(true), Some(Some(report_path.clone())));
    let (pilot_id, station_id) = (90_000_001_i64, 60_003_760_i64);
    let uid = ctx.db.with_snapshot(|s| s.characters[&pilot_id].user_id);
    ctx.db.acquire_online_lock(uid, 9_305, Some(pilot_id), true, 1_700_000_500).expect("lock");
    let _ = ctx.drain_notifications_for_char(pilot_id);
    let mk = |item_id, type_id, bp| eve_db::InventoryItemRecord {
        item_id, type_id, owner_id: pilot_id, location_id: station_id, quantity: 1, version: 1,
        flag: eve_db::InventoryFlag::Hangar, singleton: bp, is_blueprint_copy: bp,
        blueprint_runs_remaining: bp.then_some(5), ..Default::default()
    };
    let chg = || vec![(3, PyRep::Integer(0))];
    ctx.db.with_mut_snapshot(|s| { s.items.insert(991_500_001, mk(991_500_001, 34, false)); });
    ctx.emit_item_change_routed(pilot_id, &mk(991_500_002, 34, false), chg());
    with_compare_tick_scope(async {
        let bp = mk(991_500_003, *ctx.sde.blueprints.keys().next().expect("bp"), true);
        ctx.db.with_mut_snapshot(|s| { s.items.insert(bp.item_id, bp.clone()); });
        ctx.notify_item_mutation(pilot_id, None, Some(&bp));
    }).await;
    {
        let _g = enter_compare_tick_sync_scope(Some(&ctx.db));
        let fx = mk(991_500_004, 34, false);
        let row = crate::router::delta_projection::build_inv_item_dbrow(&fx, &ctx.sde, "");
        ctx.db.tx("tick_fx", |tx| -> Result<(), eve_db::TxError> {
            tx.snap.items.insert(991_500_008, mk(991_500_008, 34, false));
            tx.snap.items.insert(fx.item_id, fx.clone());
            tx.effects.notify_character(pilot_id, "OnItemChange", "charid", vec![row.clone(), PyRep::Dict(vec![(PyRep::Integer(3), PyRep::Integer(0))])]);
            Ok(())
        }).expect("tx");
    }
    with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
        with_compare_tick_scope(async {
            let d1 = mk(991_500_005, 34, false);
            ctx.db.with_mut_snapshot(|s| { s.items.insert(d1.item_id, d1.clone()); });
            ctx.emit_item_change_routed(pilot_id, &d1, chg());
            ctx.db.with_mut_snapshot(|s| { s.items.insert(991_500_006, mk(991_500_006, 34, false)); });
            ctx.emit_item_change_routed(pilot_id, &mk(991_500_007, 34, false), chg());
            flush_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx);
        }).await;
    }).await;
    let _ = ctx.drain_notifications_for_char(pilot_id);
    log_item_compare_summary(&ctx);
    let sum = flush_item_compare_summary(&ctx);
    let s = &sum.item_change.by_scope;
    assert_eq!(((s.dispatch.matched, s.dispatch.projected_only, s.dispatch.hand_written_only), (s.tick.matched, s.tick.projected_only, s.tick.hand_written_only), (s.neither.matched, s.neither.projected_only, s.neither.hand_written_only)), ((1, 1, 1), (1, 1, 0), (0, 1, 1)));
    assert_eq!((sum.blueprints_updated.by_scope.tick.matched, sum.explicit_in_tx_effects_by_scope.tick), (1, 1));
    let p: serde_json::Value = serde_json::from_str(std::fs::read_to_string(&report_path).expect("ndjson").lines().next().expect("line")).expect("json");
    let _ = std::fs::remove_file(&report_path);
    let nd = &p["item_change"]["by_scope"];
    assert_eq!((nd["dispatch"]["matched"].as_u64(), nd["tick"]["projected_only"].as_u64(), nd["neither"]["hand_written_only"].as_u64(), p["explicit_in_tx_effects_by_scope"]["tick"].as_u64()), (Some(1), Some(1), Some(1), Some(1)));
}

#[test]
fn item_compare_attributes_guided_missile_deployable_kill_to_tick_scope() {
    use crate::router::delta_item_compare::{enter_compare_tick_sync_scope, flush_item_compare_summary, set_item_compare_override_for_test};
    use crate::services::mobile_deployables::TYPE_MTU_STANDARD;
    use crate::system_actor::{step_guided_missiles_in_actor, ActiveGuidedMissile};
    use eve_db::{InventoryFlag, InventoryItemRecord};
    use eve_destiny::{Ball, Ballpark, CombatState, DamageVector, MovementMode};
    use glam::DVec3;

    let ctx = test_ctx();
    set_item_compare_override_for_test(&ctx, Some(false), Some(None));
    let (pilot_id, sys_id, mtu_id, missile_id) = (90_000_001_i64, 30_000_142_u32, 991_600_001_i64, 9_950_000_001_i64);
    let uid = ctx.db.with_snapshot(|s| s.characters[&pilot_id].user_id);
    ctx.db.acquire_online_lock(uid, 9_306, Some(pilot_id), true, 1_700_000_500).expect("lock");
    {
        let _disabled_tick = enter_compare_tick_sync_scope(Some(&ctx.db));
        ctx.db.with_mut_snapshot(|s| {
            s.items.insert(mtu_id, InventoryItemRecord { item_id: mtu_id, type_id: TYPE_MTU_STANDARD, owner_id: pilot_id, location_id: sys_id as i64, quantity: 1, version: 1, flag: InventoryFlag::None, singleton: true, ..Default::default() });
        });
    }
    set_item_compare_override_for_test(&ctx, Some(true), Some(None));
    let mut bp = Ballpark::new(sys_id);
    bp.current_tick = 5;
    for (id, ty, r) in [(mtu_id, TYPE_MTU_STANDARD, 100.0), (missile_id, 209, 5.0)] {
        bp.add_ball(Ball { id, type_id: ty, position: DVec3::ZERO, velocity: DVec3::ZERO, max_velocity: 0.0, inertia: 1.0, mass: 100.0, radius: r, speed_fraction: 0.0, mode: MovementMode::Stop });
    }
    bp.combat_states.insert(mtu_id, CombatState { structure_hp: 1.0, max_structure_hp: 100.0, signature_radius: 100.0, ..Default::default() });
    let mut missiles = vec![ActiveGuidedMissile { ball_id: missile_id, spawn_tick: 1, char_id: pilot_id, attacker_ship_id: 991_600_000, target_ball_id: mtu_id, module_id: 991_600_002, module_type_id: 2410, ammo_type_id: 209, max_velocity_ms: 1000.0, flight_time_s: 10.0, elapsed_sim_s: 1.0, min_visual_sim_s: 1.0, impacted: true, explosion_radius_m: 100.0, explosion_velocity_ms: 100.0, drf: 1.0, damage_vector: DamageVector::new(1000.0, 0.0, 0.0, 0.0) }];
    let (mut rah, mut actions, mut destroyed) = (std::collections::HashMap::new(), Vec::new(), Vec::new());
    step_guided_missiles_in_actor(&mut bp, &mut missiles, &std::collections::HashMap::new(), &mut rah, &Some(Arc::clone(&ctx.sde)), &Some(Arc::clone(&ctx.db)), &mut actions, &mut destroyed, None, 1.0);
    assert!(!ctx.db.with_snapshot(|s| s.items.contains_key(&mtu_id)), "missile kill removes MTU from snap.items");
    let s = &flush_item_compare_summary(&ctx).item_change.by_scope;
    assert!(s.tick.projected_only > 0 && s.neither.projected_only == 0 && s.dispatch.projected_only == 0, "expected tick-only attribution, got {s:?}");
}
