//! E2E `dispatch_rpc` and switch-gating regression tests for Wave F1 `#931` Track 3C-1.

#![forbid(unsafe_code)]

use crate::router::delta_item_compare::{
    flush_item_compare_summary, set_item_compare_override_for_test,
};
use crate::router::delta_item_switch::{
    projected_item_switch_summary, ItemWireFingerprint, ProjectedItemSwitchConfig,
};
use crate::router::{NotificationTarget, ServerContext};
use eve_db::{
    FleetRecord, InventoryFlag, InventoryItemRecord, IskCents, TrainedSkillRecord,
};
use eve_destiny::{Ball, MovementMode, SlimItemMeta};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use glam::DVec3;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

fn make_session(
    user_id: u64,
    char_id: i64,
    corp_id: i64,
    ship_id: Option<i64>,
    station_id: Option<i64>,
    solar_system_id: Option<u32>,
) -> SessionState {
    SessionState {
        session_id: user_id,
        user_id: Some(user_id),
        character_id: Some(char_id),
        corporation_id: Some(corp_id),
        ship_id,
        station_id,
        solar_system_id,
        ..Default::default()
    }
}

fn find_system_with_security(ctx: &ServerContext, lowsec: bool) -> u32 {
    ctx.sde
        .solar_systems
        .values()
        .filter(|s| {
            s.solar_system_id < 31_000_000
                && s.solar_system_id != 30_100_000
                && s.region_id != 10_000_070
                && if lowsec {
                    s.security_status > 0.0 && s.security_status < 0.45
                } else {
                    s.security_status <= 0.0
                }
        })
        .min_by_key(|s| s.solar_system_id)
        .map(|s| s.solar_system_id)
        .expect("matching solar system in SDE")
}

fn seed_account(snap: &mut eve_db::WorldStateSnapshot, uid: u64) {
    let mut acct = snap
        .accounts
        .values()
        .next()
        .cloned()
        .expect("seed account");
    acct.user_id = uid;
    acct.username = format!("t3c1_u_{uid}");
    snap.accounts.insert(acct.username.clone(), acct);
}

fn train_type_req_skills(
    snap: &mut eve_db::WorldStateSnapshot,
    sde: &eve_sde::SdeRegistry,
    char_id: i64,
    type_id: u32,
) {
    let Some(t) = sde.types.get(&type_id) else {
        return;
    };
    let skills = snap.skills.entry(char_id).or_default();
    for (sk_attr, lv_attr) in [(182_u16, 277_u16), (183, 278), (184, 279)] {
        if let Some(&sk_f) = t.default_attributes.get(&sk_attr) {
            let sk_id = sk_f.round() as u32;
            if sk_id > 0 {
                let req = t
                    .default_attributes
                    .get(&lv_attr)
                    .copied()
                    .unwrap_or(1.0)
                    .round()
                    .clamp(1.0, 5.0) as u8;
                let entry = skills.entry(sk_id).or_insert(TrainedSkillRecord {
                    type_id: sk_id,
                    level: req,
                    skill_points: 256_000.0,
                });
                entry.level = entry.level.max(req);
            }
        }
    }
}

fn extract_item_change_item_id(n: &crate::router::PendingMachoNotification) -> Option<i64> {
    if n.event != "OnItemChange" {
        return None;
    }
    n.args
        .first()
        .and_then(ItemWireFingerprint::from_pyrep_row)
        .map(|(id, _)| id)
}

fn extract_multi_item_change_ids(n: &crate::router::PendingMachoNotification) -> Vec<i64> {
    if n.event != "OnMultipleItemChange" {
        return Vec::new();
    }
    let Some(PyRep::List(rows) | PyRep::Tuple(rows)) = n.args.first() else {
        return Vec::new();
    };
    rows.iter()
        .filter_map(|entry| match entry {
            PyRep::Tuple(pair) if pair.len() == 2 => {
                ItemWireFingerprint::from_pyrep_row(&pair[0]).map(|(id, _)| id)
            }
            other => ItemWireFingerprint::from_pyrep_row(other).map(|(id, _)| id),
        })
        .collect()
}

#[test]
fn idle_default_server_context_has_no_activity_and_writes_no_ndjson_on_drop() {
    let report_path = std::env::temp_dir().join(format!(
        "evomu_idle_switch_ctx_drop_{}_{:?}.ndjson",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_file(&report_path);

    {
        let ctx = ServerContext::new();
        let sw = projected_item_switch_summary(&ctx);
        assert!(sw.config.is_any_enabled(), "default switches must be enabled");
        assert!(
            !sw.has_activity(),
            "idle ServerContext must report has_activity() == false: {sw:?}"
        );
        set_item_compare_override_for_test(&ctx, Some(true), Some(Some(report_path.clone())));
    }

    assert!(
        !report_path.exists(),
        "dropping an idle default ServerContext must not write an NDJSON compare line at {}",
        report_path.display()
    );
    let _ = std::fs::remove_file(&report_path);
}

#[test]
fn offline_character_and_per_kind_switch_gating_do_not_miscount_non_first_set() {
    let ctx = ServerContext::new();
    let offline_char = 98_931_701_i64;
    let online_a = 98_931_702_i64;
    let online_b = 98_931_703_i64;
    let station_id = 60_003_760_i64;
    let sys_id = 30_000_142_u32;
    let item_offline = 98_931_801_i64;
    let item_online = 98_931_802_i64;

    ctx.db.with_mut_snapshot(|snap| {
        for (cid, uid, sid_opt) in [
            (offline_char, 98_701_u64, Some(station_id)),
            (online_a, 98_702_u64, None),
            (online_b, 98_703_u64, None),
        ] {
            if uid != 98_701 {
                seed_account(snap, uid);
            }
            let mut ch = snap.characters.values().next().cloned().expect("seed char");
            ch.character_id = cid;
            ch.user_id = uid;
            ch.station_id = sid_opt;
            ch.solar_system_id = sys_id;
            snap.characters.insert(cid, ch);
        }
        snap.items.insert(
            item_offline,
            InventoryItemRecord {
                item_id: item_offline,
                type_id: 34,
                owner_id: offline_char,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 100,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            item_online,
            InventoryItemRecord {
                item_id: item_online,
                type_id: 34,
                owner_id: online_a,
                location_id: station_id,
                flag: InventoryFlag::Cargo,
                quantity: 50,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    // Offline character helper emits produce 0 delivered notifications and 0 `non_first_set_emitted`.
    let offline_item = ctx.db.with_snapshot(|s| s.items[&item_offline].clone());
    assert!(
        ctx.notify_item_record_change(offline_char, &offline_item, vec![(5, PyRep::Integer(80))])
            .is_empty()
    );
    ctx.emit_item_change_routed(offline_char, &offline_item, vec![(5, PyRep::Integer(60))]);
    ctx.emit_item_deleted_routed(offline_char, &offline_item);
    ctx.notify_item_mutation(offline_char, Some(&offline_item), None);
    let after_offline = projected_item_switch_summary(&ctx);
    assert_eq!(
        after_offline.non_first_set_emitted, 0,
        "offline recipients must not increment non_first_set_emitted: {after_offline:?}"
    );
    assert!(ctx.drain_notifications_for_char(offline_char).is_empty());

    // Bring `online_a` and `online_b` online in `sys_id` and test per-kind switch gating on
    // multi-recipient `NotificationTarget::SolarSystem(sys_id)`:
    // `char_same_scene_item_deleted = false`, `char_same_scene_item_mutation = true`.
    let now = ctx.now_unix();
    ctx.db
        .acquire_online_lock(98_702, 98_702, Some(online_a), false, now)
        .unwrap();
    ctx.db
        .acquire_online_lock(98_703, 98_703, Some(online_b), false, now)
        .unwrap();
    ctx.set_projected_item_switches(ProjectedItemSwitchConfig {
        char_same_scene_item_mutation: true,
        char_same_scene_item_deleted: false,
        char_same_scene_item_outside_rpc: false,
        char_same_scene_item_fitted: false,
        char_all_items: false,
    });

    let mut tomb_item = ctx.db.with_snapshot(|s| s.items[&item_online].clone());
    tomb_item.location_id = 6;
    tomb_item.flag = InventoryFlag::None;
    // Deletion (`locationID = 6` post-state, prior location in changeDict) under disabled deleted
    // switch must leave `non_first_set_emitted == 0`.
    let del_notifs = ctx.notify_item_record_change_with_target(
        NotificationTarget::SolarSystem(sys_id),
        "solarsystemid2",
        &tomb_item,
        vec![(3, PyRep::Integer(station_id))],
    );
    assert_eq!(del_notifs.len(), 2);
    let after_multi_del = projected_item_switch_summary(&ctx);
    assert_eq!(
        after_multi_del.non_first_set_emitted, 0,
        "multi-recipient OnItemChange tombstone must NOT count as non_first_set_emitted when deleted switch is OFF"
    );

    // Non-tombstone mutation (`locationID = station_id`) under enabled mutation switch MUST count once.
    let online_item = ctx.db.with_snapshot(|s| s.items[&item_online].clone());
    let mut_notifs = ctx.notify_item_record_change_with_target(
        NotificationTarget::SolarSystem(sys_id),
        "solarsystemid2",
        &online_item,
        vec![(3, PyRep::Integer(station_id))],
    );
    assert_eq!(mut_notifs.len(), 2);
    let after_multi_mut = projected_item_switch_summary(&ctx);
    assert_eq!(
        after_multi_mut.non_first_set_emitted, 1,
        "multi-recipient OnItemChange mutation must increment non_first_set_emitted once when mutation switch is ON"
    );
}

#[tokio::test]
async fn cargo_fueled_cyno_dispatch_rpc_rollback_is_silent_and_success_emits_once() {
    let ctx = ServerContext::new();
    set_item_compare_override_for_test(&ctx, Some(false), Some(None));
    let lowsec = find_system_with_security(&ctx, true);
    let uid = 98_931_101_u64;
    let char_id = 98_931_101_i64;
    let corp_id = 98_000_001_i64;
    let fleet_id = 98_931_199_i64;
    let ship_id = 198_931_110_i64;
    let mod_id = 198_931_111_i64;
    let fuel_id = 198_931_112_i64;

    ctx.db.with_mut_snapshot(|snap| {
        seed_account(snap, uid);
        let mut ch = snap.characters.values().next().cloned().expect("seed char");
        ch.character_id = char_id;
        ch.user_id = uid;
        ch.corporation_id = corp_id;
        ch.fleet_id = Some(fleet_id);
        ch.solar_system_id = lowsec;
        ch.station_id = None;
        ch.structure_id = None;
        ch.active_ship_id = ship_id;
        snap.characters.insert(char_id, ch);
        snap.fleets.insert(
            fleet_id,
            FleetRecord {
                fleet_id,
                boss_id: char_id,
                members: vec![char_id],
                ..Default::default()
            },
        );
        for (iid, tid, loc, flg, qty, sgl) in [
            (ship_id, 11_957_u32, lowsec as i64, InventoryFlag::None, 1, true),
            (mod_id, 21_096_u32, ship_id, InventoryFlag::HiSlot0, 1, true),
            (fuel_id, 16_273_u32, ship_id, InventoryFlag::Cargo, 1_000, false),
        ] {
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: tid,
                    owner_id: char_id,
                    location_id: loc,
                    flag: flg,
                    quantity: qty,
                    singleton: sgl,
                    version: 1,
                    ..Default::default()
                },
            );
        }
        snap.module_online_states.insert(mod_id, true);
        train_type_req_skills(snap, &ctx.sde, char_id, 21_096);
        train_type_req_skills(snap, &ctx.sde, char_id, 11_957);
        snap.skills.entry(char_id).or_default().insert(
            21_603,
            TrainedSkillRecord {
                type_id: 21_603,
                level: 5,
                skill_points: 256_000.0,
            },
        );
    });
    let now = ctx.now_unix();
    ctx.db
        .acquire_online_lock(uid, uid, Some(char_id), true, now)
        .expect("online lock");

    let sys = ctx.systems.get_or_spawn(lowsec);
    let mut cs = ctx.build_ship_combat_state(char_id, ship_id, 11_957);
    cs.capacitor = cs.max_capacitor.max(50_000.0);
    cs.max_capacitor = cs.capacitor;
    sys.spawn_ball_with_meta(
        Ball {
            id: ship_id,
            type_id: 11_957,
            position: DVec3::ZERO,
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
        cs,
        SlimItemMeta::default(),
    )
    .await
    .unwrap();

    // Seed an active cycle on `(ship_id, mod_id)` in `sys` so `commit_module_cycle_atomic_with_notifications`
    // rejects AFTER `consume_or_err_with_guard`, forcing silent Cargo fuel rollback.
    sys.add_module_cycle(crate::system_actor::ActiveModuleCycle {
        ship_id,
        module_id: mod_id,
        module_type_id: 21_096,
        owner_char_id: Some(char_id),
        effect_name: "cynosuralGeneration".into(),
        repeat: 0,
        duration_s: 30.0,
        started_at_unix: ctx.now_unix_secs(),
        ..Default::default()
    })
    .await
    .unwrap();

    let mut sess = make_session(uid, char_id, corp_id, Some(ship_id), None, Some(lowsec));
    let kw = HashMap::new();
    let args = [
        PyRep::Integer(mod_id),
        PyRep::String("cynosuralGeneration".into()),
        PyRep::Integer(0),
        PyRep::Integer(1),
    ];
    let _ = ctx.drain_notifications_for_char(char_id);

    let err = ctx
        .dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &args, &kw, None)
        .await
        .expect_err("pre-existing active cycle must trigger post-reserve cyno rollback");
    assert!(!err.message.is_empty());
    let rollback_notifs = ctx.drain_notifications_for_char(char_id);
    assert_eq!(
        rollback_notifs
            .iter()
            .filter(|n| n.event == "OnItemChange" || n.event == "OnMultipleItemChange")
            .count(),
        0,
        "silent Cargo fuel rollback must emit 0 OnItemChange/OnMultipleItemChange, got {rollback_notifs:?}"
    );
    assert_eq!(ctx.db.with_snapshot(|s| s.items[&fuel_id].quantity), 1_000);
    assert_eq!(ctx.active_fuel_reserves.active_item_count(), 0);

    // Clear conflicting cycle and activate again -> succeeds with exactly 1 OnItemChange on Cargo fuel.
    sys.remove_module_cycle(ship_id, mod_id).await.unwrap();
    let _ = ctx.drain_notifications_for_char(char_id);
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &args, &kw, None)
        .await
        .expect("cyno activation succeeds after removing conflicting cycle");
    let ok_notifs = ctx.drain_notifications_for_char(char_id);
    let item_changes: Vec<_> = ok_notifs
        .iter()
        .filter(|n| n.event == "OnItemChange")
        .collect();
    assert_eq!(
        item_changes.len(),
        1,
        "cyno Cargo fuel consumption must emit exactly 1 OnItemChange, got {item_changes:?}"
    );
    assert_eq!(extract_item_change_item_id(item_changes[0]), Some(fuel_id));
    assert_eq!(
        ok_notifs
            .iter()
            .filter(|n| n.event == "OnMultipleItemChange")
            .count(),
        0
    );
    assert!(ctx.db.with_snapshot(|s| s.items[&fuel_id].quantity) < 1_000);
    assert_eq!(ctx.active_fuel_reserves.active_item_count(), 0);
}

#[tokio::test]
async fn cargo_fueled_superweapon_slash_dispatch_rpc_rollback_is_silent_and_success_emits_once() {
    let ctx = ServerContext::new();
    set_item_compare_override_for_test(&ctx, Some(false), Some(None));
    let nullsec = find_system_with_security(&ctx, false);
    let lancer_tid = if ctx.sde.types.get(&77_283).is_some_and(|t| t.group_id == 4594) {
        77_283
    } else {
        ctx.sde
            .types
            .values()
            .filter(|t| t.group_id == 4594)
            .min_by_key(|t| t.type_id)
            .map(|t| t.type_id)
            .expect("Lancer Dreadnought hull in SDE")
    };
    let slash_mod_tid = if ctx.sde.types.get(&77_398).is_some_and(|t| t.group_id == 588) {
        77_398
    } else {
        ctx.sde
            .types
            .values()
            .filter(|t| t.group_id == 588 && matches!(t.type_id, 77_398..=77_401))
            .min_by_key(|t| t.type_id)
            .map(|t| t.type_id)
            .expect("Lancer Disruptive Lance module in SDE")
    };
    let mod_type = &ctx.sde.types[&slash_mod_tid];
    let fuel_tid = mod_type
        .default_attributes
        .get(&713)
        .map(|&v| v.round() as u32)
        .unwrap_or(17_889);
    let fuel_qty = mod_type
        .default_attributes
        .get(&714)
        .map(|&v| v.round() as i32)
        .unwrap_or(20_000);
    let cap_cost = mod_type.default_attributes.get(&6).copied().unwrap_or(30_000.0);

    let uid = 98_931_201_u64;
    let char_id = 98_931_201_i64;
    let corp_id = 98_000_001_i64;
    let ship_id = 198_931_210_i64;
    let mod_id = 198_931_211_i64;
    let fuel_id = 198_931_212_i64;
    let siege_mod_id = 198_931_213_i64;
    let start_fuel = fuel_qty + 500;

    ctx.db.with_mut_snapshot(|snap| {
        seed_account(snap, uid);
        let mut ch = snap.characters.values().next().cloned().expect("seed char");
        ch.character_id = char_id;
        ch.user_id = uid;
        ch.corporation_id = corp_id;
        ch.solar_system_id = nullsec;
        ch.station_id = None;
        ch.structure_id = None;
        ch.active_ship_id = ship_id;
        snap.characters.insert(char_id, ch);
        for (iid, tid, loc, flg, qty, sgl) in [
            (ship_id, lancer_tid, nullsec as i64, InventoryFlag::None, 1, true),
            (mod_id, slash_mod_tid, ship_id, InventoryFlag::HiSlot0, 1, true),
            (siege_mod_id, 20_280_u32, ship_id, InventoryFlag::HiSlot1, 1, true),
            (fuel_id, fuel_tid, ship_id, InventoryFlag::Cargo, start_fuel, false),
        ] {
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: tid,
                    owner_id: char_id,
                    location_id: loc,
                    flag: flg,
                    quantity: qty,
                    singleton: sgl,
                    version: 1,
                    ..Default::default()
                },
            );
        }
        snap.module_online_states.insert(mod_id, true);
        snap.module_online_states.insert(siege_mod_id, true);
        snap.module_active_states.insert(siege_mod_id);
        train_type_req_skills(snap, &ctx.sde, char_id, lancer_tid);
        train_type_req_skills(snap, &ctx.sde, char_id, slash_mod_tid);
        for sk in [77_738_u32, 77_739] {
            snap.skills.entry(char_id).or_default().insert(
                sk,
                TrainedSkillRecord {
                    type_id: sk,
                    level: 5,
                    skill_points: 256_000.0,
                },
            );
        }
    });
    let now = ctx.now_unix();
    ctx.db
        .acquire_online_lock(uid, uid, Some(char_id), true, now)
        .expect("online lock");

    let sys = ctx.systems.get_or_spawn(nullsec);
    let mut cs = ctx.build_ship_combat_state(char_id, ship_id, lancer_tid);
    cs.capacitor = cs.max_capacitor.max(cap_cost * 2.0 + 100_000.0);
    cs.max_capacitor = cs.capacitor;
    cs.base_max_capacitor = Some(cs.capacitor);
    sys.spawn_ball_with_meta(
        Ball {
            id: ship_id,
            type_id: lancer_tid,
            position: DVec3::ZERO,
            velocity: DVec3::ZERO,
            max_velocity: 100.0,
            inertia: 1.0,
            mass: 1_000_000_000.0,
            radius: 1_000.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
        cs,
        SlimItemMeta::default(),
    )
    .await
    .unwrap();

    // Seed an active cycle on `(ship_id, mod_id)` in `sys` so `commit_lancer_activation_and_break_protection`
    // fails AFTER `deduct_superweapon_fuel_with_diffs`, forcing silent Cargo fuel rollback.
    sys.add_module_cycle(crate::system_actor::ActiveModuleCycle {
        ship_id,
        module_id: mod_id,
        module_type_id: slash_mod_tid,
        owner_char_id: Some(char_id),
        effect_name: "superWeaponLanceWarmup".into(),
        repeat: 0,
        duration_s: 30.0,
        started_at_unix: ctx.now_unix_secs(),
        ..Default::default()
    })
    .await
    .unwrap();

    let mut sess = make_session(uid, char_id, corp_id, Some(ship_id), None, Some(nullsec));
    let kw = HashMap::new();
    let args = [
        PyRep::Integer(mod_id),
        PyRep::Tuple(vec![PyRep::Real(1.0), PyRep::Real(0.0), PyRep::Real(0.0)]),
        PyRep::Tuple(vec![PyRep::Real(2.0), PyRep::Real(0.0), PyRep::Real(0.0)]),
    ];
    let _ = ctx.drain_notifications_for_char(char_id);

    let err = ctx
        .dispatch_rpc(
            &mut sess,
            "superWeaponMgr",
            None,
            "ActivateSlashModule",
            &args,
            &kw,
            None,
        )
        .await
        .expect_err("existing cycle must trigger post-reserve superweapon rollback");
    assert!(!err.message.is_empty());
    let rollback_notifs = ctx.drain_notifications_for_char(char_id);
    assert_eq!(
        rollback_notifs
            .iter()
            .filter(|n| n.event == "OnItemChange" || n.event == "OnMultipleItemChange")
            .count(),
        0,
        "silent superweapon Cargo fuel rollback must emit 0 OnItemChange/OnMultipleItemChange, got {rollback_notifs:?}"
    );
    assert_eq!(ctx.db.with_snapshot(|s| s.items[&fuel_id].quantity), start_fuel);
    assert_eq!(ctx.active_fuel_reserves.active_item_count(), 0);

    // Remove conflicting cycle, clear cooldown/lock if any, and fire again -> exact 1 OnItemChange.
    sys.remove_module_cycle(ship_id, mod_id).await.unwrap();
    ctx.module_reactivation_cooldowns.write().unwrap().clear();
    ctx.tactical_state
        .write()
        .unwrap()
        .lance_caster_lock_until
        .clear();
    let _ = ctx.drain_notifications_for_char(char_id);

    ctx.dispatch_rpc(
        &mut sess,
        "superWeaponMgr",
        None,
        "ActivateSlashModule",
        &args,
        &kw,
        None,
    )
    .await
    .expect("superweapon slash activation succeeds after clearing cycle");
    let ok_notifs = ctx.drain_notifications_for_char(char_id);
    let item_changes: Vec<_> = ok_notifs
        .iter()
        .filter(|n| n.event == "OnItemChange")
        .collect();
    assert_eq!(
        item_changes.len(),
        1,
        "superweapon Cargo fuel consumption must emit exactly 1 OnItemChange, got {item_changes:?}"
    );
    assert_eq!(extract_item_change_item_id(item_changes[0]), Some(fuel_id));
    assert_eq!(
        ok_notifs
            .iter()
            .filter(|n| n.event == "OnMultipleItemChange")
            .count(),
        0
    );
    assert_eq!(ctx.db.with_snapshot(|s| s.items[&fuel_id].quantity), 500);
    assert_eq!(ctx.active_fuel_reserves.active_item_count(), 0);
}

#[tokio::test]
async fn industry_install_job_coalesces_single_complete_on_industry_materials() {
    let ctx = ServerContext::new();
    set_item_compare_override_for_test(&ctx, Some(false), Some(None));
    let station_id = 60_003_760_i64;
    let sys_id = ctx
        .sde
        .stations
        .get(&station_id)
        .map(|st| st.solar_system_id)
        .unwrap_or(30_000_142);

    // Pick a deterministic manufacturing blueprint with >= 2 distinct materials.
    let (bp_type_id, materials): (u32, Vec<(u32, i32)>) = ctx
        .sde
        .blueprints
        .iter()
        .filter(|(&tid, _)| ctx.sde.types.contains_key(&tid))
        .filter_map(|(&tid, bp)| {
            let act = bp
                .activities
                .get("manufacturing")
                .or_else(|| bp.activities.get("1"))?;
            if act.materials.len() >= 2 && act.products.iter().any(|p| p.quantity > 0) {
                Some((
                    tid,
                    act.materials
                        .iter()
                        .map(|m| (m.type_id, m.quantity as i32))
                        .collect(),
                ))
            } else {
                None
            }
        })
        .min_by_key(|(tid, _)| *tid)
        .expect("manufacturing blueprint with >=2 materials in SDE");
    let mat_set: HashSet<u32> = materials.iter().map(|(tid, _)| *tid).collect();
    let unrelated_tid = [11_399_u32, 34, 35, 36, 37, 38, 39, 40]
        .into_iter()
        .find(|tid| !mat_set.contains(tid) && ctx.sde.types.contains_key(tid))
        .expect("unrelated Hangar mineral type in SDE");

    let uid = 98_931_301_u64;
    let char_id = 98_931_301_i64;
    let corp_id = 98_000_001_i64;
    let bp_item_id = 198_931_310_i64;
    let unrelated_item_id = 198_931_399_i64;

    ctx.db.with_mut_snapshot(|snap| {
        seed_account(snap, uid);
        let mut ch = snap.characters.values().next().cloned().expect("seed char");
        ch.character_id = char_id;
        ch.user_id = uid;
        ch.corporation_id = corp_id;
        ch.station_id = Some(station_id);
        ch.structure_id = None;
        ch.solar_system_id = sys_id;
        ch.balance = IskCents::from_isk(100_000_000);
        snap.characters.insert(char_id, ch);

        snap.items.insert(
            bp_item_id,
            InventoryItemRecord {
                item_id: bp_item_id,
                type_id: bp_type_id,
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        // Seed mat[0] with exact required qty (will be deleted -> 0 in OnIndustryMaterials),
        // and mat[1..] with +250 excess (will be mutated -> 250).
        for (idx, &(mtid, mqty)) in materials.iter().enumerate() {
            let iid = 198_931_320_i64 + idx as i64;
            let seeded_qty = if idx == 0 { mqty } else { mqty + 250 };
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: mtid,
                    owner_id: char_id,
                    location_id: station_id,
                    flag: InventoryFlag::Hangar,
                    quantity: seeded_qty,
                    singleton: false,
                    version: 1,
                    ..Default::default()
                },
            );
        }
        snap.items.insert(
            unrelated_item_id,
            InventoryItemRecord {
                item_id: unrelated_item_id,
                type_id: unrelated_tid,
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 777,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );

        for sk in [3_380_u32, 3_387, 3_388] {
            snap.skills.entry(char_id).or_default().insert(
                sk,
                TrainedSkillRecord {
                    type_id: sk,
                    level: 5,
                    skill_points: 256_000.0,
                },
            );
        }
        train_type_req_skills(snap, &ctx.sde, char_id, bp_type_id);
        if let Some(act) = ctx.sde.blueprints.get(&bp_type_id).and_then(|b| {
            b.activities
                .get("manufacturing")
                .or_else(|| b.activities.get("1"))
        }) {
            for rs in &act.skills {
                snap.skills.entry(char_id).or_default().insert(
                    rs.type_id,
                    TrainedSkillRecord {
                        type_id: rs.type_id,
                        level: rs.level.max(1),
                        skill_points: 256_000.0,
                    },
                );
            }
        }
    });
    let now = ctx.now_unix();
    ctx.db
        .acquire_online_lock(uid, uid, Some(char_id), true, now)
        .expect("online lock");

    let mut sess = make_session(uid, char_id, corp_id, None, Some(station_id), Some(sys_id));
    let kw = HashMap::new();

    let conn_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "industryMonitor",
            None,
            "ConnectJob",
            &[PyRep::keyval([
                ("ownerID", PyRep::Integer(char_id)),
                ("locationID", PyRep::Integer(station_id)),
                ("inputFlagID", PyRep::Integer(InventoryFlag::Hangar as i64)),
            ])],
            &kw,
            None,
        )
        .await
        .expect("ConnectJob must succeed");
    let monitor_id = match conn_rep {
        PyRep::Tuple(parts) => parts[0].as_i64().expect("monitor_id int"),
        other => panic!("unexpected ConnectJob reply: {other:?}"),
    };
    let _ = ctx.drain_notifications_for_char(char_id);

    ctx.dispatch_rpc(
        &mut sess,
        "industryManager",
        None,
        "InstallJob",
        &[PyRep::keyval([
            ("blueprintID", PyRep::Integer(bp_item_id)),
            ("facilityID", PyRep::Integer(station_id)),
            ("activityID", PyRep::Integer(1)),
            ("runs", PyRep::Integer(1)),
        ])],
        &kw,
        None,
    )
    .await
    .expect("InstallJob must succeed");

    let notifs = ctx.drain_notifications_for_char(char_id);
    let mat_notifs: Vec<_> = notifs
        .iter()
        .filter(|n| n.event == "OnIndustryMaterials")
        .collect();
    assert_eq!(
        mat_notifs.len(),
        1,
        "InstallJob with multiple consumed materials must emit exactly 1 coalesced OnIndustryMaterials, got {mat_notifs:?}"
    );
    assert_eq!(
        mat_notifs[0].args.first().and_then(|v| v.as_i64()),
        Some(monitor_id),
        "expected OnIndustryMaterials monitorID={monitor_id}, got {:?}",
        mat_notifs[0].args
    );
    let Some(PyRep::Dict(mat_dict)) = mat_notifs[0].args.get(1) else {
        panic!("expected OnIndustryMaterials materials Dict at args[1], got {:?}", mat_notifs[0].args);
    };
    let mat_map: BTreeMap<i64, i64> = mat_dict
        .iter()
        .filter_map(|(k, v)| Some((k.as_i64()?, v.as_i64()?)))
        .collect();
    assert_eq!(
        mat_map.get(&(materials[0].0 as i64)).copied(),
        Some(0),
        "fully consumed material must map to 0 in coalesced OnIndustryMaterials: {mat_map:?}"
    );
    for (mtid, _) in materials.iter().skip(1) {
        assert_eq!(
            mat_map.get(&(*mtid as i64)).copied(),
            Some(250),
            "partially consumed material {mtid} must map to 250 in coalesced OnIndustryMaterials: {mat_map:?}"
        );
    }
    assert_eq!(
        mat_map.get(&(unrelated_tid as i64)).copied(),
        Some(777),
        "coalesced OnIndustryMaterials must carry complete Hangar availability including unrelated mineral: {mat_map:?}"
    );
}

fn setup_fit_ships_e2e_ctx() -> (ServerContext, SessionState, i64, u32) {
    let ctx = ServerContext::new();
    set_item_compare_override_for_test(&ctx, Some(true), Some(None));
    let station_id = 60_003_760_i64;
    let sys_id = ctx
        .sde
        .stations
        .get(&station_id)
        .map(|st| st.solar_system_id)
        .unwrap_or(30_000_142);

    // Pick a small published LoSlot module that fits a Rifter cleanly without needing special hull tags.
    let lo_mod_tid = [2_048_u32, 1_317, 1_319, 440]
        .into_iter()
        .find(|tid| ctx.sde.types.contains_key(tid))
        .expect("fittable module in SDE");

    let uid = 98_931_401_u64;
    let char_id = 98_931_401_i64;
    let corp_id = 98_000_001_i64;
    let ship_id = 198_931_410_i64;

    ctx.db.with_mut_snapshot(|snap| {
        seed_account(snap, uid);
        let mut ch = snap.characters.values().next().cloned().expect("seed char");
        ch.character_id = char_id;
        ch.user_id = uid;
        ch.corporation_id = corp_id;
        ch.station_id = Some(station_id);
        ch.structure_id = None;
        ch.solar_system_id = sys_id;
        ch.active_ship_id = ship_id;
        snap.characters.insert(char_id, ch);

        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        for (iid, tid, qty, sgl) in [
            (198_931_420_i64, 34_u32, 500, false),
            (198_931_421_i64, 35_u32, 50, false),
            (198_931_422_i64, 36_u32, 300, false),
            (198_931_423_i64, 37_u32, 40, false),
            (198_931_424_i64, lo_mod_tid, 1, false),
        ] {
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: tid,
                    owner_id: char_id,
                    location_id: station_id,
                    flag: InventoryFlag::Hangar,
                    quantity: qty,
                    singleton: sgl,
                    version: 1,
                    ..Default::default()
                },
            );
        }
        for sk in [3_413_u32, 3_426, 3_318, 3_392] {
            snap.skills.entry(char_id).or_default().insert(
                sk,
                TrainedSkillRecord {
                    type_id: sk,
                    level: 5,
                    skill_points: 256_000.0,
                },
            );
        }
        train_type_req_skills(snap, &ctx.sde, char_id, lo_mod_tid);
        train_type_req_skills(snap, &ctx.sde, char_id, 587);
    });
    let now = ctx.now_unix();
    ctx.db
        .acquire_online_lock(uid, uid, Some(char_id), true, now)
        .expect("online lock");

    let sess = make_session(
        uid,
        char_id,
        corp_id,
        Some(ship_id),
        Some(station_id),
        Some(sys_id),
    );
    let _ = ctx.drain_notifications_for_char(char_id);
    (ctx, sess, char_id, lo_mod_tid)
}

#[tokio::test]
async fn fit_ships_dedupes_on_multiple_item_change_with_zero_compare_drift() {
    let (ctx, mut sess, char_id, lo_mod_tid) = setup_fit_ships_e2e_ctx();
    assert!(ctx.projected_item_switches().char_same_scene_item_fitted);
    let kw = HashMap::new();

    // 1. Mixed FitShips under default-ON Class "fitted": Cargo stacks AND LoSlot0 module (198_931_424)
    //    are all commit-projected, so FitShips's batch OnMultipleItemChange drops cleanly (0 rows left)
    //    and only single-row commit companions remain with zero duplicate item_ids.
    let mixed_fit = PyRep::List(vec![
        PyRep::Tuple(vec![PyRep::Integer(34), PyRep::Integer(5), PyRep::Integer(100)]),
        PyRep::Tuple(vec![PyRep::Integer(35), PyRep::Integer(5), PyRep::Integer(50)]),
        PyRep::Tuple(vec![
            PyRep::Integer(lo_mod_tid as i64),
            PyRep::Integer(11),
            PyRep::Integer(1),
        ]),
    ]);
    ctx.dispatch_rpc(&mut sess, "ship", None, "FitShips", &[mixed_fit], &kw, None)
        .await
        .expect("mixed FitShips succeeds");

    let mixed_notifs = ctx.drain_notifications_for_char(char_id);
    let item_change_ids: HashSet<i64> = mixed_notifs
        .iter()
        .filter_map(extract_item_change_item_id)
        .collect();
    assert!(
        item_change_ids.contains(&198_931_424),
        "mixed FitShips under default-ON fitted must commit-project LoSlot0 198_931_424: {item_change_ids:?}"
    );
    let multi_notifs: Vec<_> = mixed_notifs
        .iter()
        .filter(|n| n.event == "OnMultipleItemChange")
        .collect();
    let all_multi_ids: Vec<i64> = multi_notifs
        .iter()
        .flat_map(|n| extract_multi_item_change_ids(n))
        .collect();
    let unique_multi: BTreeSet<i64> = all_multi_ids.iter().copied().collect();
    assert_eq!(
        all_multi_ids.len(),
        unique_multi.len(),
        "OnMultipleItemChange must contain zero duplicate item_ids across batch & commit companions: {multi_notifs:?}"
    );
    let fit_batch_notifs: Vec<_> = multi_notifs
        .iter()
        .filter(|n| {
            extract_multi_item_change_ids(n)
                .iter()
                .any(|id| !item_change_ids.contains(id))
        })
        .collect();
    assert_eq!(
        fit_batch_notifs.len(),
        0,
        "mixed FitShips under default-ON fitted must drop batch OnMultipleItemChange completely, got {fit_batch_notifs:?}"
    );
    assert!(
        multi_notifs.iter().all(|n| {
            let ids = extract_multi_item_change_ids(n);
            ids.len() == 1 && item_change_ids.contains(&ids[0])
        }),
        "all OnMultipleItemChange notifications must be single-row commit companions for OnItemChange items: {multi_notifs:?}"
    );

    // 2. Cargo-only FitShips: ALL rows belong to commit-projected first_set, so FitShips's
    // batch OnMultipleItemChange becomes empty after dedupe and is dropped completely
    // (only the single-row commit-time paired companions remain, with zero duplicate item_ids).
    let cargo_only_fit = PyRep::List(vec![
        PyRep::Tuple(vec![PyRep::Integer(36), PyRep::Integer(5), PyRep::Integer(100)]),
        PyRep::Tuple(vec![PyRep::Integer(37), PyRep::Integer(5), PyRep::Integer(40)]),
    ]);
    ctx.dispatch_rpc(&mut sess, "ship", None, "FitShips", &[cargo_only_fit], &kw, None)
        .await
        .expect("cargo-only FitShips succeeds");

    let cargo_notifs = ctx.drain_notifications_for_char(char_id);
    let cargo_item_change_ids: HashSet<i64> = cargo_notifs
        .iter()
        .filter_map(extract_item_change_item_id)
        .collect();
    assert!(
        !cargo_item_change_ids.is_empty(),
        "cargo-only FitShips must still emit commit-time OnItemChange"
    );
    let cargo_multi_notifs: Vec<_> = cargo_notifs
        .iter()
        .filter(|n| n.event == "OnMultipleItemChange")
        .collect();
    let cargo_multi_ids: Vec<i64> = cargo_multi_notifs
        .iter()
        .flat_map(|n| extract_multi_item_change_ids(n))
        .collect();
    let cargo_unique_multi: BTreeSet<i64> = cargo_multi_ids.iter().copied().collect();
    assert_eq!(
        cargo_multi_ids.len(),
        cargo_unique_multi.len(),
        "cargo-only FitShips must emit zero duplicate OnMultipleItemChange item_ids: {cargo_multi_notifs:?}"
    );
    assert!(
        cargo_multi_notifs.iter().all(|n| {
            let ids = extract_multi_item_change_ids(n);
            ids.len() == 1 && cargo_item_change_ids.contains(&ids[0])
        }),
        "cargo-only FitShips must drop its batch OnMultipleItemChange so only commit companions remain: {cargo_multi_notifs:?}"
    );

    let cmp = flush_item_compare_summary(&ctx);
    assert_eq!(
        cmp.item_change.hand_written_only, 0,
        "FitShips must have zero hand_written_only compare drift: {cmp:?}"
    );
    let sw = projected_item_switch_summary(&ctx);
    assert_eq!(
        sw.replaced, 5,
        "FitShips must dedupe all 5 commit-projected Cargo/Hangar + LoSlot0 OnMultipleItemChange rows: {sw:?}"
    );
    assert_eq!(
        sw.non_first_set_emitted, 0,
        "FitShips under default-ON fitted must have zero unprojected OnMultipleItemChange rows: {sw:?}"
    );
    assert_eq!(
        sw.helper_without_token, 0,
        "FitShips must have zero helper_without_token misses: {sw:?}"
    );
}

#[tokio::test]
async fn fit_ships_partial_dedupe_with_fitted_kill_switch_off() {
    let (ctx, mut sess, char_id, lo_mod_tid) = setup_fit_ships_e2e_ctx();
    ctx.set_projected_item_switches(
        ProjectedItemSwitchConfig::enabled().with_char_same_scene_item_fitted(false),
    );
    let kw = HashMap::new();

    // With Class "fitted" kill-switch OFF, Cargo stacks (partial + whole) come from commit-time
    // OnItemChange ONLY while FitShips's batch OnMultipleItemChange retains only non-first-set LoSlot0.
    let mixed_fit = PyRep::List(vec![
        PyRep::Tuple(vec![PyRep::Integer(34), PyRep::Integer(5), PyRep::Integer(100)]),
        PyRep::Tuple(vec![PyRep::Integer(35), PyRep::Integer(5), PyRep::Integer(50)]),
        PyRep::Tuple(vec![
            PyRep::Integer(lo_mod_tid as i64),
            PyRep::Integer(11),
            PyRep::Integer(1),
        ]),
    ]);
    ctx.dispatch_rpc(&mut sess, "ship", None, "FitShips", &[mixed_fit], &kw, None)
        .await
        .expect("mixed FitShips succeeds with fitted=false");

    let mixed_notifs = ctx.drain_notifications_for_char(char_id);
    let item_change_ids: HashSet<i64> = mixed_notifs
        .iter()
        .filter_map(extract_item_change_item_id)
        .collect();
    assert!(!item_change_ids.contains(&198_931_424));
    let multi_notifs: Vec<_> = mixed_notifs
        .iter()
        .filter(|n| n.event == "OnMultipleItemChange")
        .collect();
    let all_multi_ids: Vec<i64> = multi_notifs
        .iter()
        .flat_map(|n| extract_multi_item_change_ids(n))
        .collect();
    let unique_multi: BTreeSet<i64> = all_multi_ids.iter().copied().collect();
    assert_eq!(all_multi_ids.len(), unique_multi.len());

    let fit_batch_notifs: Vec<_> = multi_notifs
        .iter()
        .filter(|n| {
            extract_multi_item_change_ids(n)
                .iter()
                .any(|id| !item_change_ids.contains(id))
        })
        .collect();
    assert_eq!(
        fit_batch_notifs.len(),
        1,
        "fitted=false mixed FitShips must keep non-first-set LoSlot0 in 1 OnMultipleItemChange, got {multi_notifs:?}"
    );
    assert_eq!(
        extract_multi_item_change_ids(fit_batch_notifs[0]),
        vec![198_931_424_i64],
        "FitShips OnMultipleItemChange must strip Cargo rows and keep only LoSlot0 module: {fit_batch_notifs:?}"
    );
    assert!(
        multi_notifs
            .iter()
            .filter(|n| n.seq_id != fit_batch_notifs[0].seq_id)
            .all(|n| {
                let ids = extract_multi_item_change_ids(n);
                ids.len() == 1 && item_change_ids.contains(&ids[0])
            }),
        "all other OnMultipleItemChange notifications must be single-row commit companions: {multi_notifs:?}"
    );

    let sw = projected_item_switch_summary(&ctx);
    assert_eq!(sw.replaced, 2, "expected 2 Cargo/Hangar rows replaced: {sw:?}");
    assert_eq!(sw.non_first_set_emitted, 1, "expected LoSlot0 preserved once: {sw:?}");
    assert_eq!(sw.helper_without_token, 0, "sw={sw:?}");
}
