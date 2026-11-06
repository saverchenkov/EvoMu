//! Live integration tests for probe scanning notifications, route waypoints,
//! Zarzakh gravity hazard safe zones, and wormhole statics/info buckets/decay stages.

#![forbid(unsafe_code)]

use eve_db::{
    CosmicSignatureRecord, InventoryFlag, InventoryItemRecord, TrainedSkillRecord,
    VirtualServerClock, WorldRepository,
};
use eve_machonet::{service_roles, SessionState};
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::space_destiny::{
    derive_wormhole_buckets_raw, ensure_system_signatures_and_anomalies,
};
use eve_server::services::space_navigation_advanced::step_zarzakh_gravity_hazard_in_ballpark;
use eve_server::services::wormhole_lifecycle_ops::statics_for_system_sde;
use glam::DVec3;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::common;

#[allow(dead_code)]
const CHAR_PLAYER: i64 = 90_000_001; // demo (ROLE_PLAYER_DEFAULT)
#[allow(dead_code)]
const CHAR_OTHER: i64 = 90_000_002; // other (ROLE_PLAYER_DEFAULT)

#[allow(dead_code)]
const JITA_SYSTEM: u32 = 30_000_142;
#[allow(dead_code)]
const PERIMETER_SYSTEM: u32 = 30_000_144;
#[allow(dead_code)]
const AMARR_SYSTEM: u32 = 30_002_187;
#[allow(dead_code)]
const ZARZAKH_SYSTEM: u32 = 30_100_000;

#[allow(dead_code)]
const LAUNCHER_CORE_I: u32 = 17_938;
#[allow(dead_code)]
const PROBE_CORE_I: u32 = 30_013;

#[allow(dead_code)]
struct TempSqliteDb {
    path: PathBuf,
}

#[allow(dead_code)]
impl TempSqliteDb {
    fn new(tag: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "evomu_{tag}_{}_{}.sqlite",
            std::process::id(),
            nanos
        ));
        let _ = std::fs::remove_file(&path);
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

#[allow(dead_code)]
impl Drop for TempSqliteDb {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
        let _ = std::fs::remove_file(format!("{}-wal", self.path.display()));
        let _ = std::fs::remove_file(format!("{}-shm", self.path.display()));
    }
}

#[allow(dead_code)]
fn open_sqlite_ctx(path: &Path, now_unix: u64) -> Arc<ServerContext> {
    let repo = Arc::new(WorldRepository::open_sqlite(path).expect("open_sqlite"));
    repo.with_mut_snapshot(|snap| {
        if snap.accounts.is_empty() {
            *snap = WorldRepository::with_demo_seed().with_snapshot(|s| s.clone());
        }
    });
    let ctx = Arc::new(ServerContext::with_repository(repo));
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(now_unix)));
    ctx
}

#[allow(dead_code)]
async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, eve_proto::BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None)
        .await
}

#[allow(dead_code)]
async fn login_non_gm_player(
    ctx: &ServerContext,
    sess_id: u64,
    user: &str,
    pass: &str,
    char_id: i64,
) -> SessionState {
    let mut sess = SessionState::new(sess_id);
    common::login_pilot(ctx, &mut sess, user, pass).await;
    assert_eq!(sess.role & service_roles::ROLE_GM_MASK, 0);
    rpc(
        ctx,
        &mut sess,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
    )
    .await
    .expect("SelectCharacterID");
    assert_eq!(sess.role & service_roles::ROLE_GM_MASK, 0);
    sess
}

// parity: notify:OnNewProbe
// parity: notify:OnProbeStateChanged
// parity: notify:OnProbeWarpStart
// parity: notify:OnProbeWarpEnd
// parity: notify:OnSystemScanStarted
// parity: notify:OnSystemScanStopped
#[tokio::test]
async fn parity_shard_5_probe_scanning_events_and_sqlite_reload() {
    eve_testkit::attest(&[
        "notify:OnNewProbe",
        "notify:OnProbeStateChanged",
        "notify:OnProbeWarpStart",
        "notify:OnProbeWarpEnd",
        "notify:OnSystemScanStarted",
        "notify:OnSystemScanStopped",
    ]);

    let db_guard = TempSqliteDb::new("probe_events");
    let ctx = open_sqlite_ctx(db_guard.path(), common::TEST_CLOCK_BASE_UNIX);
    let mut sess = login_non_gm_player(&ctx, 73_001, "demo", "demo-secret", CHAR_PLAYER).await;

    // Fit Core Probe Launcher + Core Scanner Probes in cargo and undock as non-GM player
    let (launcher_id, ship_id) = ctx.db.with_mut_snapshot(|snap| {
        let sk = snap.skills.entry(CHAR_PLAYER).or_default();
        for sid in [3402_u32, 3412, 3413, 3426] {
            sk.insert(
                sid,
                TrainedSkillRecord {
                    type_id: sid,
                    level: 5,
                    skill_points: 256_000.0,
                },
            );
        }
        let sid = snap.characters[&CHAR_PLAYER].active_ship_id;
        let lid = snap.next_entity_id + 1;
        let pid = lid + 1;
        snap.next_entity_id = pid + 1;
        snap.items.insert(
            lid,
            InventoryItemRecord {
                item_id: lid,
                type_id: LAUNCHER_CORE_I,
                owner_id: CHAR_PLAYER,
                location_id: sid,
                flag: InventoryFlag::HiSlot2,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            pid,
            InventoryItemRecord {
                item_id: pid,
                type_id: PROBE_CORE_I,
                owner_id: CHAR_PLAYER,
                location_id: sid,
                flag: InventoryFlag::HiSlot2,
                quantity: 8,
                singleton: false,
                ..Default::default()
            },
        );
        (lid, sid)
    });
    ctx.invalidate_ship_fitting(ship_id);

    rpc(&ctx, &mut sess, "stationSvc", "Undock", &[])
        .await
        .expect("Undock");
    ctx.advance_secs(31);
    let _ = sess.drain_notifications();
    let _ = ctx.drain_notifications_for_char(CHAR_PLAYER);

    let now_ft_before = ctx.now_blueos_filetime_ticks();
    rpc(
        &ctx,
        &mut sess,
        "dogmaIM",
        "Activate",
        &[
            PyRep::Integer(launcher_id),
            PyRep::String("launchScanners".into()),
            PyRep::None,
            PyRep::Integer(0),
        ],
    )
    .await
    .expect("launchScanners");

    let launch_notifs = ctx.drain_notifications_for_char(CHAR_PLAYER);
    let _ = sess.drain_notifications();
    let new_probes: Vec<_> = launch_notifs
        .iter()
        .filter(|n| n.event == "OnNewProbe")
        .collect();
    assert_eq!(new_probes.len(), 8, "must emit OnNewProbe for all 8 probes");

    // Verify OnNewProbe carries BlueOS FILETIME expiry and 3-tuple position
    let probe_info = &new_probes[0].args[0];
    let expiry_ft = probe_info
        .dict_get("expiry")
        .and_then(|v| v.as_i64())
        .expect("BlueOS FILETIME expiry");
    assert!(
        expiry_ft > now_ft_before + 3_500 * 10_000_000,
        "probe expiry must be BlueOS FILETIME ticks ~1h ahead"
    );
    let Some(PyRep::Tuple(pos_tup)) = probe_info.dict_get("pos") else {
        panic!("expected 3-tuple pos on OnNewProbe");
    };
    assert_eq!(pos_tup.len(), 3);

    // Move & scan probes through bound scanMgr -> OnProbeWarpStart, OnProbeWarpEnd, OnProbeStateChanged, OnSystemScanStarted, OnSystemScanStopped
    let scan_bound = rpc(&ctx, &mut sess, "scanMgr", "GetSystemScanMgr", &[])
        .await
        .expect("GetSystemScanMgr");
    let scan_oid = scan_bound
        .dict_get("boundID")
        .and_then(|v| v.as_i64())
        .map(|v| v as u64);

    let (sigs, _) = ensure_system_signatures_and_anomalies(&ctx, JITA_SYSTEM).await;
    let target_pos = sigs
        .first()
        .map(|s| DVec3::from_array(s.position))
        .unwrap_or(DVec3::new(100_000_000.0, 0.0, 0.0));
    let range_m = 4.0 * 149_597_870_700.0;
    let d = range_m * 0.4;
    let offs = [
        DVec3::ZERO,
        DVec3::X * d,
        -DVec3::X * d,
        DVec3::Y * d,
        -DVec3::Y * d,
        DVec3::Z * d,
        -DVec3::Z * d,
        DVec3::splat(d * 0.5),
    ];
    let mut probe_ids: Vec<i64> = ctx
        .active_scanner_probes
        .read()
        .unwrap()
        .values()
        .filter(|p| p.owner_char_id == CHAR_PLAYER && p.solar_system_id == JITA_SYSTEM)
        .map(|p| p.probe_id)
        .collect();
    probe_ids.sort_unstable();
    assert_eq!(probe_ids.len(), 8);

    let scan_dict = PyRep::Dict(
        probe_ids
            .iter()
            .enumerate()
            .map(|(i, &pid)| {
                let p = target_pos + offs[i % offs.len()];
                (
                    PyRep::Integer(pid),
                    PyRep::keyval([
                        (
                            "pos",
                            PyRep::Tuple(vec![
                                PyRep::Real(p.x),
                                PyRep::Real(p.y),
                                PyRep::Real(p.z),
                            ]),
                        ),
                        ("scanRange", PyRep::Real(range_m)),
                    ]),
                )
            })
            .collect(),
    );

    let scan_rows = ctx
        .dispatch_rpc(
            &mut sess,
            "scanMgr",
            scan_oid,
            "RequestScans",
            &[scan_dict],
            &HashMap::new(),
            None,
        )
        .await
        .expect("RequestScans");
    assert!(matches!(scan_rows, PyRep::List(_)));

    ctx.advance_secs(6);
    eve_server::services::space_probe_scanning::tick_probe_warp_end(&ctx);
    ctx.advance_secs(12);
    eve_server::services::space_probe_scanning::tick_probe_warp_end(&ctx);

    let scan_notifs = ctx.drain_notifications_for_char(CHAR_PLAYER);
    let _ = sess.drain_notifications();
    for expected_ev in [
        "OnProbeWarpStart",
        "OnProbeWarpEnd",
        "OnProbeStateChanged",
        "OnSystemScanStarted",
        "OnSystemScanStopped",
    ] {
        assert!(
            scan_notifs.iter().any(|n| n.event == expected_ev),
            "scan cycle must emit {expected_ev}"
        );
    }

    // Bookmark scanned location and verify SQLite reload persistence
    let _ = rpc(
        &ctx,
        &mut sess,
        "bookmark",
        "BookmarkLocation",
        &[
            PyRep::String("Scanned Hole".into()),
            PyRep::String("C4 Exit".into()),
        ],
    )
    .await
    .expect("BookmarkLocation");
    drop(sess);
    drop(ctx);

    let ctx_reloaded = open_sqlite_ctx(db_guard.path(), common::TEST_CLOCK_BASE_UNIX + 60);
    let _sess_reloaded =
        login_non_gm_player(&ctx_reloaded, 73_011, "demo", "demo-secret", CHAR_PLAYER).await;
    assert!(
        ctx_reloaded.db.with_snapshot(|s| !s.bookmarks.is_empty()),
        "scanned bookmark must persist across SQLite reload"
    );
}

// parity: feat:navigation.route-waypoints
// parity: feat:navigation.gravity-hazard-safe-zones
// parity: feat:navigation.wormhole-info-buckets
// parity: feat:navigation.wormhole-statics
// parity: feat:wormholes.lifetime-decay-stages
#[tokio::test]
async fn parity_shard_5_waypoints_gravity_hazard_and_wormhole_statics() {
    eve_testkit::attest(&[
        "feat:navigation.route-waypoints",
        "feat:navigation.gravity-hazard-safe-zones",
        "feat:navigation.wormhole-info-buckets",
        "feat:navigation.wormhole-statics",
        "feat:wormholes.lifetime-decay-stages",
    ]);

    let db_guard = TempSqliteDb::new("nav_wh");
    let ctx = open_sqlite_ctx(db_guard.path(), common::TEST_CLOCK_BASE_UNIX);
    let mut sess = login_non_gm_player(&ctx, 74_001, "other", "other-secret", CHAR_OTHER).await;

    // 1. Autopilot waypoints (`feat:navigation.route-waypoints`) + SQLite persistence
    let set_wp = rpc(
        &ctx,
        &mut sess,
        "charMgr",
        "SaveCharacterSetting",
        &[
            PyRep::String("waypoints".into()),
            PyRep::Tuple(vec![
                PyRep::Integer(i64::from(PERIMETER_SYSTEM)),
                PyRep::Integer(i64::from(AMARR_SYSTEM)),
            ]),
        ],
    )
    .await;
    assert!(set_wp.is_ok());

    drop(sess);
    drop(ctx);

    let ctx = open_sqlite_ctx(db_guard.path(), common::TEST_CLOCK_BASE_UNIX + 10);
    let sess = login_non_gm_player(&ctx, 74_002, "other", "other-secret", CHAR_OTHER).await;
    let wps = ctx
        .db
        .with_snapshot(|s| s.characters[&CHAR_OTHER].autopilot_waypoints.clone());
    assert_eq!(wps, vec![i64::from(PERIMETER_SYSTEM), i64::from(AMARR_SYSTEM)]);

    // 2. Zarzakh Jovian gravity hazard (`feat:navigation.gravity-hazard-safe-zones`)
    let fulc_pos = ctx.sde.stations[&60_015_187].position;
    let safe_ship = 9_800_001_i64;
    let hazard_ship = 9_800_002_i64;
    let mut bp = eve_destiny::Ballpark::new(ZARZAKH_SYSTEM);
    for (sid, pos) in [
        (safe_ship, fulc_pos + DVec3::new(10_000.0, 0.0, 0.0)),
        (hazard_ship, fulc_pos + DVec3::new(500_000.0, 0.0, 0.0)),
    ] {
        bp.balls.insert(
            sid,
            eve_destiny::Ball {
                id: sid,
                type_id: 606,
                position: pos,
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: eve_destiny::MovementMode::Stop,
            },
        );
        bp.combat_states
            .insert(sid, ctx.build_type_combat_state(606));
    }
    let hull_before_safe = bp.combat_states[&safe_ship].structure_hp;
    let hull_before_haz = bp.combat_states[&hazard_ship].structure_hp;
    step_zarzakh_gravity_hazard_in_ballpark(&mut bp, Some(&ctx.sde), ZARZAKH_SYSTEM);
    assert_eq!(
        bp.combat_states[&safe_ship].structure_hp, hull_before_safe,
        "ship inside 150km safe zone must take 0 Jovian hazard damage"
    );
    assert!(
        bp.combat_states[&hazard_ship].structure_hp < hull_before_haz,
        "ship outside 150km safe zone must take Jovian gravity hull damage"
    );

    // 3. Wormhole info buckets, W-space statics per class, and lifetime decay stages
    let mut class_statics_checked = 0_usize;
    for wh_class in 1_i32..=6_i32 {
        if let Some((&sys_id, _)) = ctx
            .sde
            .solar_systems
            .iter()
            .find(|(_, s)| s.wormhole_class_id == Some(wh_class))
        {
            let statics = statics_for_system_sde(&ctx.sde, sys_id);
            let (sigs, _) = ensure_system_signatures_and_anomalies(&ctx, sys_id).await;
            assert!(
                !statics.is_empty() && sigs.iter().any(|s| s.sig_group == "Wormhole"),
                "W-space class C{wh_class} must resolve SDE static wormholes"
            );
            class_statics_checked += 1;
        }
    }
    assert!(class_statics_checked >= 4);

    // Verify wormhole buckets & lifetime decay stages across full -> <24h -> <4h -> <1h and persist CosmicSignatureRecord in SQLite
    let now = ctx.now_unix();
    let total_mass = 1_000_000_000.0_f64;
    let full_life = 86_400_u64;
    let b_full = derive_wormhole_buckets_raw(JITA_SYSTEM, 20_000_000.0, total_mass * 0.80, total_mass, now, full_life + 7_200, false, now, &ctx.sde);
    assert_eq!(b_full.lifetime_bucket, ">1 day");
    let b_lt24 = derive_wormhole_buckets_raw(JITA_SYSTEM, 20_000_000.0, total_mass * 0.40, total_mass, now.saturating_sub(36_400), full_life, false, now, &ctx.sde);
    assert_eq!(b_lt24.lifetime_bucket, "<1 day");
    let b_lt4 = derive_wormhole_buckets_raw(JITA_SYSTEM, 20_000_000.0, total_mass * 0.08, total_mass, now.saturating_sub(79_200), full_life, false, now, &ctx.sde);
    assert_eq!(b_lt4.lifetime_bucket, "<4h");
    let b_lt1 = derive_wormhole_buckets_raw(JITA_SYSTEM, 20_000_000.0, total_mass * 0.05, total_mass, now.saturating_sub(84_600), full_life, false, now, &ctx.sde);
    assert_eq!(b_lt1.lifetime_bucket, "<1h");

    ctx.db.with_mut_snapshot(|snap| {
        snap.cosmic_signatures.entry(JITA_SYSTEM).or_default().push(CosmicSignatureRecord {
            signature_id: "S5W-777".into(),
            solar_system_id: JITA_SYSTEM,
            sig_group: "Wormhole".into(),
            type_name: "Wormhole K162".into(),
            position: [12_000.0, 0.0, 0.0],
            signal_strength: 1.0,
            wormhole_type_id: Some(56_026),
            destination_system_id: Some(AMARR_SYSTEM),
            created_unix: now,
            lifetime_seconds: full_life,
            max_mass_kg: total_mass,
            remaining_mass_kg: total_mass * 0.08,
            max_jump_mass_kg: 20_000_000.0,
            collapsed: false,
        });
    });

    drop(sess);
    drop(ctx);

    let ctx_reloaded = open_sqlite_ctx(db_guard.path(), now + 30);
    let mut sess_reloaded = login_non_gm_player(&ctx_reloaded, 74_009, "other", "other-secret", CHAR_OTHER).await;
    let persisted_wh = ctx_reloaded.db.with_snapshot(|s| {
        s.cosmic_signatures
            .get(&JITA_SYSTEM)
            .and_then(|v| v.iter().find(|sig| sig.signature_id == "S5W-777"))
            .cloned()
    });
    assert!(persisted_wh.is_some(), "wormhole signature must persist across SQLite reload");
    let pol = rpc(
        &ctx_reloaded,
        &mut sess_reloaded,
        "wormholeMgr",
        "GetWormholePolarization",
        &[PyRep::String("S5W-777".into())],
    )
    .await;
    assert!(
        pol.is_ok(),
        "reloaded wormhole signature must be resolved by wormholeMgr.GetWormholePolarization"
    );
}
