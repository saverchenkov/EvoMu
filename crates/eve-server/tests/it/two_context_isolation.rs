//! Smoke tests for side-by-side [`crate::common::TwoServerContexts`] isolation (`#1583` Track 1).

use crate::common::TwoServerContexts;
use eve_sde::SdeRegistry;
use std::sync::Arc;

#[test]
fn two_server_contexts_isolate_repositories_and_sdes() {
    let contexts = TwoServerContexts::new();
    let shared_char_id = 90_000_001_i64;

    let right_sec_before = contexts
        .right()
        .db
        .with_snapshot(|s| s.characters.get(&shared_char_id).map(|c| c.security_status));
    let right_epoch_before = contexts.right().db.state_epoch();

    contexts
        .left()
        .db
        .tx("two_context_left_isolation", |tx| {
            let ch = tx
                .snap
                .characters
                .get_mut(&shared_char_id)
                .expect("demo seed character 90_000_001 must exist");
            ch.security_status = 4.25;
            Ok::<(), eve_db::TxError>(())
        })
        .expect("left context tx commit");

    assert_ne!(right_sec_before, Some(4.25));

    assert_eq!(
        contexts
            .left()
            .db
            .with_snapshot(|s| s.characters.get(&shared_char_id).map(|c| c.security_status)),
        Some(4.25)
    );
    assert_eq!(
        contexts
            .right()
            .db
            .with_snapshot(|s| s.characters.get(&shared_char_id).map(|c| c.security_status)),
        right_sec_before
    );
    assert_eq!(contexts.right().db.state_epoch(), right_epoch_before);
    assert!(contexts.left().db.state_epoch() > right_epoch_before);
    contexts.assert_teardown_invariants();

    let empty = TwoServerContexts::new_empty();
    assert_ne!(empty.left().instance_id, empty.right().instance_id);
    assert_ne!(
        empty.left().db.instance_id(),
        empty.right().db.instance_id()
    );

    let custom = TwoServerContexts::with_sdes(
        Arc::new(SdeRegistry::default()),
        Arc::new(SdeRegistry::default()),
    );
    assert!(!Arc::ptr_eq(&custom.left().sde, &custom.right().sde));
    assert!(!Arc::ptr_eq(&custom.left().db, &custom.right().db));
}

#[test]
fn two_server_contexts_stay_in_memory_under_env_path_overrides() {
    struct EnvRestore(Vec<(&'static str, Option<std::ffi::OsString>)>);
    impl Drop for EnvRestore {
        fn drop(&mut self) {
            for (k, v) in self.0.drain(..) {
                match v {
                    Some(val) => std::env::set_var(k, val),
                    None => std::env::remove_var(k),
                }
            }
        }
    }

    struct TempDirGuard(std::path::PathBuf);
    impl Drop for TempDirGuard {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    let _lock = eve_db::lock_test_env_write();
    let keys = [
        "EVOMU_DB_PATH",
        "EVOMU_SNAPSHOT_PATH",
        "EVOMU_DISABLE_DEMO_SEED",
        "EVOMU_SDE_PATH",
    ];
    let _restore = EnvRestore(keys.iter().map(|&k| (k, std::env::var_os(k))).collect());

    let tmp_dir = std::env::temp_dir().join(format!("eve_it_two_ctx_env_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp_dir);
    std::fs::create_dir_all(&tmp_dir).unwrap();
    let _tmp_guard = TempDirGuard(tmp_dir.clone());
    let db_path = tmp_dir.join("shared.sqlite");
    let snap_path = tmp_dir.join("shared_snap.json");
    let missing_sde_path = tmp_dir.join("missing_sde.sqlite");
    std::env::set_var("EVOMU_DB_PATH", &db_path);
    std::env::set_var("EVOMU_SNAPSHOT_PATH", &snap_path);
    std::env::set_var("EVOMU_DISABLE_DEMO_SEED", "1");

    let empty = TwoServerContexts::new_empty();
    empty.left().db.provision_player_account("left_only", "pw");
    assert!(empty.left().db.with_snapshot(|s| s.accounts.contains_key("left_only")));
    assert!(empty.right().db.with_snapshot(|s| !s.accounts.contains_key("left_only")));
    assert!(!db_path.exists());

    let seeded = TwoServerContexts::new();
    seeded.left().db.provision_player_account("seeded_left", "pw");
    assert!(seeded.left().db.with_snapshot(|s| s.characters.contains_key(&90_000_001)));
    assert!(seeded.right().db.with_snapshot(|s| !s.accounts.contains_key("seeded_left")));
    assert!(!snap_path.exists());

    // Poison EVOMU_SDE_PATH and prove TwoServerContexts::with_sdes ignores it and seeds each
    // context's repository exclusively from its caller-supplied SdeRegistry.
    std::env::set_var("EVOMU_SDE_PATH", &missing_sde_path);
    let make_race2_sde = |skill_id: u32, level: u8| {
        let mut sde = SdeRegistry::default();
        sde.races.insert(
            2,
            eve_sde::RaceInfo {
                race_id: 2,
                name: "Minmatar".to_string(),
                corvette_type_id: None,
                starter_skills: std::collections::HashMap::from([(skill_id, level)]),
                description: String::new(),
            },
        );
        Arc::new(sde)
    };
    let custom = TwoServerContexts::with_sdes(
        make_race2_sde(999_001, 2),
        make_race2_sde(999_002, 4),
    );
    assert!(!Arc::ptr_eq(&custom.left().sde, &custom.right().sde));
    assert!(!Arc::ptr_eq(&custom.left().db, &custom.right().db));
    assert_eq!(
        custom
            .left()
            .db
            .with_snapshot(|s| s.skills[&90_000_001].get(&999_001).map(|sk| sk.level)),
        Some(2)
    );
    assert!(!custom
        .left()
        .db
        .with_snapshot(|s| s.skills[&90_000_001].contains_key(&999_002)));
    assert_eq!(
        custom
            .right()
            .db
            .with_snapshot(|s| s.skills[&90_000_001].get(&999_002).map(|sk| sk.level)),
        Some(4)
    );
    assert!(!custom
        .right()
        .db
        .with_snapshot(|s| s.skills[&90_000_001].contains_key(&999_001)));
}

#[tokio::test]
async fn two_server_contexts_isolate_scanners_superweapons_and_system_effects() {
    use eve_db::{InventoryFlag, InventoryItemRecord, TrainedSkillRecord};
    use eve_destiny::{Ball, DVec3, MovementMode};
    use eve_machonet::{service_roles::ROLE_GMH, SessionState};
    use eve_proto::PyRep;
    use eve_sde::attr_ids;
    use eve_server::router::teardown_registry::run_disconnect_teardown_sync;
    use eve_server::router::ServerContext;
    use eve_server::services::inventory_fitting::fitting_scanners_superweapons::{
        active_hacking_target_for_char, mark_hacking_container_unlocked,
        purge_expired_ship_dbuffs_all,
    };
    use eve_server::services::notify_emitters_tournaments_anomalies::collect_active_system_wide_effects_in_system;
    use std::collections::HashMap;

    // Load the SDE directly via default_db_path() under test env read guard so concurrent env-var poisoning tests cannot race.
    let base_sde = {
        let _env_guard = eve_db::lock_test_env_read();
        SdeRegistry::open(&SdeRegistry::default_db_path()).expect("open evomu_sde.sqlite")
    };
    let pair = TwoServerContexts::with_sdes(Arc::new(base_sde.clone()), Arc::new(base_sde));
    let (left, right) = pair.both();
    crate::common::pin_clock(left);
    crate::common::pin_clock(right);

    // Identical IDs across both independent ServerContext instances in the same process.
    const SYS_ID: u32 = 30_000_142; // Jita
    const CHAR_ID: i64 = 90_000_001;
    const OTHER_CHAR_ID: i64 = 90_000_002;
    const SHIP_ID: i64 = 9_900_101;
    const DATA_MOD_ID: i64 = 9_900_102;
    const CONTAINER_ID: i64 = 9_900_202;
    let kw: HashMap<String, PyRep> = HashMap::new();

    async fn setup_pilot_in_jita_with_analyzer(ctx: &ServerContext, sess_id: u64) -> Option<SessionState> {
        let mut required_skills = vec![3405_u32, 3408_u32];
        for tid in [22175_u32, 37483_u32] {
            if let Some(inv_t) = ctx.sde.types.get(&tid) {
                for s_attr in [
                    attr_ids::REQUIRED_SKILL_1,
                    attr_ids::REQUIRED_SKILL_2,
                    attr_ids::REQUIRED_SKILL_3,
                ] {
                    if let Some(&sid_f) = inv_t.default_attributes.get(&s_attr) {
                        let sid = sid_f as u32;
                        if sid > 0 && !required_skills.contains(&sid) {
                            required_skills.push(sid);
                        }
                    }
                }
            }
        }
        ctx.db.with_mut_snapshot(|snap| {
            let sk = snap.skills.entry(CHAR_ID).or_default();
            for sid in required_skills {
                sk.insert(
                    sid,
                    TrainedSkillRecord { type_id: sid, level: 5, skill_points: 256_000.0 },
                );
            }
            snap.items.insert(
                SHIP_ID,
                InventoryItemRecord {
                    item_id: SHIP_ID,
                    type_id: 37483, // Stratios
                    owner_id: CHAR_ID,
                    location_id: SYS_ID as i64,
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
            snap.items.insert(
                DATA_MOD_ID,
                InventoryItemRecord {
                    item_id: DATA_MOD_ID,
                    type_id: 22175, // Data Analyzer I
                    owner_id: CHAR_ID,
                    location_id: SHIP_ID,
                    flag: InventoryFlag::MedSlot0,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
            snap.items.insert(
                CONTAINER_ID,
                InventoryItemRecord {
                    item_id: CONTAINER_ID,
                    type_id: 23, // Cargo Container
                    owner_id: OTHER_CHAR_ID,
                    location_id: SYS_ID as i64,
                    flag: InventoryFlag::Locked,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
            snap.module_online_states.insert(DATA_MOD_ID, true);
            if let Some(ch) = snap.characters.get_mut(&CHAR_ID) {
                ch.station_id = None;
                ch.structure_id = None;
                ch.solar_system_id = SYS_ID;
                ch.active_ship_id = SHIP_ID;
            }
        });
        crate::common::grant_legal_active_ship_skills(ctx, CHAR_ID);
        ctx.invalidate_ship_fitting(SHIP_ID);

        let sys = ctx.systems.get_or_spawn(SYS_ID);
        sys.spawn_ball(Ball {
            id: SHIP_ID,
            type_id: 37483,
            position: DVec3::ZERO,
            velocity: DVec3::ZERO,
            max_velocity: 350.0,
            inertia: 1.0,
            mass: 1_350_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await
        .expect("spawn player ball");
        sys.spawn_ball(Ball {
            id: CONTAINER_ID,
            type_id: 23,
            position: DVec3::new(2_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 1_000.0,
            radius: 25.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await
        .expect("spawn container ball");

        let kw: HashMap<String, PyRep> = HashMap::new();
        let mut sess = SessionState::new(sess_id);
        crate::common::login_pilot(ctx, &mut sess, "demo", "demo-secret").await;
        let sel_rep = crate::common::borrow_wire_harness(ctx, &mut sess)
            .call_ok("charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(CHAR_ID)], &kw)
            .await;
        assert_eq!(sel_rep.dict_get("characterID").and_then(|v| v.as_i64()), Some(CHAR_ID));
        assert_eq!(sel_rep.dict_get("solarSystemID").and_then(|v| v.as_i64()), Some(SYS_ID as i64));
        sess.role |= ROLE_GMH;
        sess.station_id = None;
        sess.structure_id = None;
        sess.solar_system_id = Some(SYS_ID);
        sess.ship_id = Some(SHIP_ID);
        Some(sess)
    }

    let (sys_id, char_id, ship_id, data_mod_id, container_id) =
        (SYS_ID, CHAR_ID, SHIP_ID, DATA_MOD_ID, CONTAINER_ID);
    let mut sess_l = setup_pilot_in_jita_with_analyzer(left, 91_001).await.expect("left setup");
    let mut sess_r = setup_pilot_in_jita_with_analyzer(right, 91_002).await.expect("right setup");

    // 1. system_wide_effects + system_wide_effect_lists isolation on identical solar_system_id.
    let refresh_l = crate::common::borrow_wire_harness(left, &mut sess_l)
        .call_ok(
            "dogmaIM",
            None,
            "RefreshSystemWideEnvironment",
            &[PyRep::Integer(sys_id as i64), PyRep::Integer(30844)],
            &kw,
        )
        .await;
    assert_eq!(refresh_l.dict_get("effectTypeID").and_then(|v| v.as_i64()), Some(30844));
    assert_eq!(left.systems.get_system_wide_effects(sys_id), (Some(30844), vec![]));
    assert_eq!(right.systems.get_system_wide_effects(sys_id), (None, vec![]));

    let sys_info_r = crate::common::borrow_wire_harness(right, &mut sess_r)
        .call_ok("dogmaIM", None, "GetSystemWideEffectsInfo", &[PyRep::Integer(sys_id as i64)], &kw)
        .await;
    assert_eq!(sys_info_r.dict_get("effectTypeID").and_then(|v| v.as_i64()), None);
    assert!(collect_active_system_wide_effects_in_system(left, Some(sys_id), true)
        .iter()
        .any(|&(_, _, tid)| tid == 30844));
    assert!(collect_active_system_wide_effects_in_system(right, Some(sys_id), true).is_empty());

    left.systems.with_system_wide_effects_mut(|sw| {
        sw.effect_lists.insert(sys_id, vec![30844, 30846]);
    });
    assert_eq!(left.systems.get_system_wide_effects(sys_id), (Some(30844), vec![30844, 30846]));
    assert_eq!(right.systems.get_system_wide_effects(sys_id), (None, vec![]));

    let refresh_r = crate::common::borrow_wire_harness(right, &mut sess_r)
        .call_ok(
            "dogmaIM",
            None,
            "RefreshSystemWideEnvironment",
            &[PyRep::Integer(sys_id as i64), PyRep::Integer(30846)],
            &kw,
        )
        .await;
    assert_eq!(refresh_r.dict_get("effectTypeID").and_then(|v| v.as_i64()), Some(30846));
    let reset_l = crate::common::borrow_wire_harness(left, &mut sess_l)
        .call_ok("dogmaIM", None, "ResetSystemWideEffects", &[PyRep::Integer(sys_id as i64)], &kw)
        .await;
    assert_eq!(reset_l.dict_get("active").and_then(|v| v.as_bool()), Some(false));
    assert_eq!(left.systems.get_system_wide_effects(sys_id), (None, vec![]));
    assert_eq!(right.systems.get_system_wide_effects(sys_id), (Some(30846), vec![]));

    // 2. ship_dbuffs isolation on identical ship_id.
    let apply_l = crate::common::borrow_wire_harness(left, &mut sess_l)
        .call_ok(
            "dogmaIM",
            None,
            "ApplyBuffsToItems",
            &[
                PyRep::List(vec![PyRep::Integer(ship_id)]),
                PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(12), PyRep::Real(15.0)])]),
                PyRep::Real(120.0),
            ],
            &kw,
        )
        .await;
    assert_eq!(apply_l.dict_get("activeCount").and_then(|v| v.as_i64()), Some(1));

    let dbuff_state_l = crate::common::borrow_wire_harness(left, &mut sess_l)
        .call_ok("dogmaIM", None, "GetDbuffStateForClient", &[PyRep::Integer(ship_id)], &kw)
        .await;
    assert_eq!(dbuff_state_l.dict_get("activeCount").and_then(|v| v.as_i64()), Some(1));
    let dbuff_state_r = crate::common::borrow_wire_harness(right, &mut sess_r)
        .call_ok("dogmaIM", None, "GetDbuffStateForClient", &[PyRep::Integer(ship_id)], &kw)
        .await;
    assert_eq!(dbuff_state_r.dict_get("activeCount").and_then(|v| v.as_i64()), Some(0));
    right.scanners_superweapons.with_ship_dbuffs(|db| {
        assert!(!db.contains_key(&ship_id));
    });

    let apply_r = crate::common::borrow_wire_harness(right, &mut sess_r)
        .call_ok(
            "dogmaIM",
            None,
            "ApplyBuffsToItems",
            &[
                PyRep::List(vec![PyRep::Integer(ship_id)]),
                PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(12), PyRep::Real(5.0)])]),
                PyRep::Real(600.0),
            ],
            &kw,
        )
        .await;
    assert_eq!(apply_r.dict_get("activeCount").and_then(|v| v.as_i64()), Some(1));
    let purge_at = left.now_unix_secs() + 200;
    purge_expired_ship_dbuffs_all(left, purge_at);
    left.scanners_superweapons.with_ship_dbuffs(|db| {
        assert!(!db.contains_key(&ship_id));
    });
    right.scanners_superweapons.with_ship_dbuffs(|db| {
        let list = db.get(&ship_id).expect("right ship dbuff must survive left purge");
        assert_eq!(list.len(), 1);
        assert!((list[0].value - 5.0).abs() < 1e-6);
    });

    // 3. active_hacking_target_by_char + hacking_states isolation on identical char_id & container_id.
    let hack_l = crate::common::borrow_wire_harness(left, &mut sess_l)
        .call_ok(
            "dogmaIM",
            None,
            "Activate",
            &[
                PyRep::Integer(data_mod_id),
                PyRep::String("dataHacking".into()),
                PyRep::Integer(container_id),
            ],
            &kw,
        )
        .await;
    assert_eq!(hack_l.dict_get("containerUnlocked").and_then(|v| v.as_bool()), Some(false));
    assert_eq!(active_hacking_target_for_char(left, char_id), Some(container_id));
    assert_eq!(active_hacking_target_for_char(right, char_id), None);
    left.scanners_superweapons.with_hacking(|h| {
        assert!(h.hacking_states.contains_key(&container_id));
    });
    right.scanners_superweapons.with_hacking(|h| {
        assert!(!h.hacking_states.contains_key(&container_id));
    });

    let hack_r = crate::common::borrow_wire_harness(right, &mut sess_r)
        .call_ok(
            "dogmaIM",
            None,
            "Activate",
            &[
                PyRep::Integer(data_mod_id),
                PyRep::String("dataHacking".into()),
                PyRep::Integer(container_id),
            ],
            &kw,
        )
        .await;
    assert_eq!(hack_r.dict_get("containerUnlocked").and_then(|v| v.as_bool()), Some(false));
    assert_eq!(active_hacking_target_for_char(right, char_id), Some(container_id));

    mark_hacking_container_unlocked(left, char_id, container_id);
    run_disconnect_teardown_sync(left, char_id, Some(&sess_l));
    assert_eq!(active_hacking_target_for_char(left, char_id), None);
    left.scanners_superweapons.with_hacking(|h| {
        assert!(!h.hacking_states.contains_key(&container_id));
    });
    assert_eq!(
        left.db.with_snapshot(|s| s.items.get(&container_id).map(|i| i.flag)),
        Some(InventoryFlag::None)
    );

    assert_eq!(active_hacking_target_for_char(right, char_id), Some(container_id));
    right.scanners_superweapons.with_hacking(|h| {
        let st = h.hacking_states.get(&container_id).expect("right hacking state preserved");
        assert!(!st.unlocked);
    });
    assert!(
        right.db.with_snapshot(|s| s.items.get(&container_id).map(|i| i.flag))
            == Some(InventoryFlag::Locked)
    );

    // 4. Per-WorldRepository cyno portal beacons/bridges isolation (`#1583` Track 14).
    use eve_server::services::inventory_fitting::fitting_cyno_portal::{
        is_ship_cyno_active, on_cyno_ship_destroyed,
    };
    const CYNO_SHIP_ID: i64 = 9_900_301;
    const CYNO_MOD_ID: i64 = 9_900_302;
    const CYNO_FUEL_ID: i64 = 9_900_303;
    const CYNO_FLEET_ID: i64 = 9_900_901;
    const REDEEMER_TYPE_ID: u32 = 22_428;
    const CYNO_GEN_TYPE_ID: u32 = 21_096;
    const LIQUID_OZONE_TYPE_ID: u32 = 16_273;

    run_disconnect_teardown_sync(right, char_id, Some(&sess_r));
    let lowsec_sys = left
        .sde
        .lowest_matching_solar_system(|s| {
            s.security_status < 0.45
                && s.region_id != 10_000_070
                && s.solar_system_id < 31_000_000
                && s.solar_system_id != 30_100_000
        })
        .map(|s| s.solar_system_id)
        .unwrap_or(30_002_813);

    async fn setup_cyno_pilot_in_lowsec(
        ctx: &ServerContext,
        sess_id: u64,
        lowsec_sys: u32,
    ) -> SessionState {
        let mut req_skills = vec![21_603_u32];
        for tid in [REDEEMER_TYPE_ID, CYNO_GEN_TYPE_ID] {
            if let Some(inv_t) = ctx.sde.types.get(&tid) {
                for s_attr in [
                    attr_ids::REQUIRED_SKILL_1,
                    attr_ids::REQUIRED_SKILL_2,
                    attr_ids::REQUIRED_SKILL_3,
                ] {
                    if let Some(&sid_f) = inv_t.default_attributes.get(&s_attr) {
                        let sid = sid_f as u32;
                        if sid > 0 && !req_skills.contains(&sid) {
                            req_skills.push(sid);
                        }
                    }
                }
            }
        }
        ctx.db.with_mut_snapshot(|snap| {
            let sk = snap.skills.entry(CHAR_ID).or_default();
            for sid in req_skills {
                sk.insert(
                    sid,
                    TrainedSkillRecord { type_id: sid, level: 5, skill_points: 256_000.0 },
                );
            }
            snap.items.insert(
                CYNO_SHIP_ID,
                InventoryItemRecord {
                    item_id: CYNO_SHIP_ID,
                    type_id: REDEEMER_TYPE_ID,
                    owner_id: CHAR_ID,
                    location_id: i64::from(lowsec_sys),
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
            snap.items.insert(
                CYNO_MOD_ID,
                InventoryItemRecord {
                    item_id: CYNO_MOD_ID,
                    type_id: CYNO_GEN_TYPE_ID,
                    owner_id: CHAR_ID,
                    location_id: CYNO_SHIP_ID,
                    flag: InventoryFlag::HiSlot0,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
            snap.items.insert(
                CYNO_FUEL_ID,
                InventoryItemRecord {
                    item_id: CYNO_FUEL_ID,
                    type_id: LIQUID_OZONE_TYPE_ID,
                    owner_id: CHAR_ID,
                    location_id: CYNO_SHIP_ID,
                    flag: InventoryFlag::Cargo,
                    quantity: 5_000,
                    singleton: false,
                    version: 1,
                    ..Default::default()
                },
            );
            snap.module_online_states.insert(CYNO_MOD_ID, true);
            if let Some(ch) = snap.characters.get_mut(&CHAR_ID) {
                ch.station_id = None;
                ch.structure_id = None;
                ch.solar_system_id = lowsec_sys;
                ch.active_ship_id = CYNO_SHIP_ID;
            }
        });
        crate::common::grant_legal_active_ship_skills(ctx, CHAR_ID);
        ctx.invalidate_ship_fitting(CYNO_SHIP_ID);

        let sys = ctx.systems.get_or_spawn(lowsec_sys);
        let t = &ctx.sde.types[&REDEEMER_TYPE_ID];
        let mut cs = ctx.build_type_combat_state(REDEEMER_TYPE_ID);
        cs.capacitor = 10_000.0;
        cs.max_capacitor = 10_000.0;
        let meta = ctx.build_ship_slim_meta(CHAR_ID, REDEEMER_TYPE_ID);
        sys.spawn_ball_with_meta(
            Ball {
                id: CYNO_SHIP_ID,
                type_id: REDEEMER_TYPE_ID,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 200.0,
                inertia: 1.5,
                mass: t.mass.max(10_000_000.0),
                radius: t.radius.max(100.0),
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            cs,
            meta,
        )
        .await
        .expect("spawn cyno ship");

        let mut sess = SessionState::new(sess_id);
        crate::common::login_pilot(ctx, &mut sess, "demo", "demo-secret").await;
        crate::common::borrow_wire_harness(ctx, &mut sess)
            .call_ok(
                "charUnboundMgr",
                None,
                "SelectCharacterID",
                &[PyRep::Integer(CHAR_ID)],
                &HashMap::new(),
            )
            .await;
        // Seed fleet after `SelectCharacterID` so `on_character_select` does not purge offline fleet state.
        ctx.db.with_mut_snapshot(|snap| {
            snap.fleets.insert(
                CYNO_FLEET_ID,
                eve_db::FleetRecord {
                    fleet_id: CYNO_FLEET_ID,
                    boss_id: CHAR_ID,
                    members: vec![CHAR_ID],
                    ..Default::default()
                },
            );
            if let Some(ch) = snap.characters.get_mut(&CHAR_ID) {
                ch.fleet_id = Some(CYNO_FLEET_ID);
            }
        });
        sess.role |= ROLE_GMH;
        sess.station_id = None;
        sess.structure_id = None;
        sess.solar_system_id = Some(lowsec_sys);
        sess.ship_id = Some(CYNO_SHIP_ID);
        sess.fleet_id = Some(CYNO_FLEET_ID);
        sess
    }

    let mut cyno_sess_l = setup_cyno_pilot_in_lowsec(left, 91_003, lowsec_sys).await;
    let mut cyno_sess_r = setup_cyno_pilot_in_lowsec(right, 91_004, lowsec_sys).await;
    assert!(!is_ship_cyno_active(left, CYNO_SHIP_ID));
    assert!(!is_ship_cyno_active(right, CYNO_SHIP_ID));

    let cyno_rep_l = crate::common::borrow_wire_harness(left, &mut cyno_sess_l)
        .call_ok(
            "dogmaIM",
            None,
            "Activate",
            &[PyRep::Integer(CYNO_MOD_ID), PyRep::String("cynosuralGeneration".into())],
            &kw,
        )
        .await;
    assert_eq!(cyno_rep_l.dict_get("solarSystemID").and_then(|v| v.as_i64()), Some(i64::from(lowsec_sys)));
    assert!(is_ship_cyno_active(left, CYNO_SHIP_ID));
    assert!(!is_ship_cyno_active(right, CYNO_SHIP_ID));

    let cyno_rep_r = crate::common::borrow_wire_harness(right, &mut cyno_sess_r)
        .call_ok(
            "dogmaIM",
            None,
            "Activate",
            &[PyRep::Integer(CYNO_MOD_ID), PyRep::String("cynosuralGeneration".into())],
            &kw,
        )
        .await;
    assert_eq!(cyno_rep_r.dict_get("solarSystemID").and_then(|v| v.as_i64()), Some(i64::from(lowsec_sys)));
    assert!(is_ship_cyno_active(left, CYNO_SHIP_ID));
    assert!(is_ship_cyno_active(right, CYNO_SHIP_ID));

    on_cyno_ship_destroyed(left, CYNO_SHIP_ID);
    assert!(!is_ship_cyno_active(left, CYNO_SHIP_ID));
    assert!(is_ship_cyno_active(right, CYNO_SHIP_ID));
}
