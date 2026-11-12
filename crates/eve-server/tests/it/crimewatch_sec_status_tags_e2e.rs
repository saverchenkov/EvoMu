//! Live end-to-end integration tests for Crimewatch security status, tag tier bands,
//! kill rights, and timer/flag update notifications with SQLite persistence.

#![forbid(unsafe_code)]

use eve_db::{VirtualServerClock, WorldRepository};
use eve_machonet::{service_roles, SessionState};
use eve_proto::PyRep;
use eve_server::combat::crimewatch::{
    compute_sec_status_loss, credit_specialized_npc_bounty_and_sec_status,
};
use eve_server::router::{PendingMachoNotification, ServerContext};
use eve_server::services::station_structure::npc_station_offers_service;
use glam::DVec3;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::common;

#[allow(dead_code)]
const CHAR_ALPHA_ATTACKER: i64 = 90_000_001; // demo (ROLE_PLAYER_DEFAULT)
#[allow(dead_code)]
const CHAR_BETA_VICTIM: i64 = 90_000_002; // other (ROLE_PLAYER_DEFAULT)
#[allow(dead_code)]
const CHAR_GAMMA_BUYER: i64 = 93_510_003;
#[allow(dead_code)]
const CHAR_DELTA_VICTIM: i64 = 93_510_004;

#[allow(dead_code)]
pub(crate) const JITA_SYSTEM: u32 = 30_000_142;
#[allow(dead_code)]
pub(crate) const JITA_STATION: i64 = 60_003_760;
#[allow(dead_code)]
pub(crate) const ASABONA_SYSTEM: u32 = 30_000_012;
#[allow(dead_code)]
pub(crate) const ZARZAKH_SYSTEM: u32 = 30_100_000;

#[allow(dead_code)]
const SHIP_RIFTER: u32 = 587;
#[allow(dead_code)]
pub(crate) const SHIP_VELATOR: u32 = 606;
#[allow(dead_code)]
const MODULE_AUTOCANNON_II: u32 = 2889;
#[allow(dead_code)]
const NPC_SERPENTIS_INITIATE: u32 = 3744;

#[allow(dead_code)]
pub(crate) struct TempSqliteDb {
    pub(crate) path: PathBuf,
}

#[allow(dead_code)]
impl TempSqliteDb {
    pub(crate) fn new(tag: &str) -> Self {
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

    pub(crate) fn path(&self) -> &Path {
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
pub(crate) fn open_sqlite_ctx(path: &Path, now_unix: u64) -> Arc<ServerContext> {
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
pub(crate) async fn rpc(
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
pub(crate) async fn login_player_session(
    ctx: &ServerContext,
    sess_id: u64,
    user: &str,
    pass: &str,
    char_id: i64,
) -> SessionState {
    let mut sess = SessionState::new(sess_id);
    common::login_pilot(ctx, &mut sess, user, pass).await;
    assert_eq!(
        sess.role & service_roles::ROLE_GM_MASK,
        0,
        "player test session must not have GM role"
    );
    rpc(
        ctx,
        &mut sess,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
    )
    .await
    .expect("SelectCharacterID");
    assert_eq!(
        sess.role & service_roles::ROLE_GM_MASK,
        0,
        "selected character session must remain non-GM"
    );
    sess
}

#[allow(dead_code)]
pub(crate) async fn undock_player_to_space(
    ctx: &ServerContext,
    sess: &mut SessionState,
    sys_id: u32,
    pos: DVec3,
) -> (i64, u64) {
    let char_id = sess.character_id.expect("character_id");
    let _ = rpc(ctx, sess, "stationSvc", "Undock", &[]).await;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.solar_system_id = sys_id;
        }
        sess.station_id = None;
        sess.solar_system_id = Some(sys_id);
    });
    let bind = rpc(
        ctx,
        sess,
        "beyonce",
        "MachoBindObject",
        &[PyRep::Integer(i64::from(sys_id)), PyRep::None],
    )
    .await
    .expect("beyonce.MachoBindObject");
    let PyRep::Tuple(parts) = bind else {
        panic!("expected tuple from MachoBindObject");
    };
    let beyonce_oid = parts[0].as_i64().expect("beyonce oid") as u64;
    let ship_id = ctx
        .db
        .with_snapshot(|s| s.characters[&char_id].active_ship_id);
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_handle.set_ball_position(ship_id, pos).await;
    ctx.advance_secs(12);
    (ship_id, beyonce_oid)
}

#[allow(dead_code)]
pub(crate) async fn fire_and_destroy_target(
    ctx: &ServerContext,
    attacker_sess: &mut SessionState,
    sys_id: u32,
    gun_id: i64,
    target_ball_id: i64,
) {
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let _ = rpc(
        ctx,
        attacker_sess,
        "dogmaIM",
        "AddTarget",
        &[PyRep::Integer(target_ball_id)],
    )
    .await;
    sys_handle.step_manual(5.0).await.expect("lock step");

    for _ in 0..24 {
        let _ = rpc(
            ctx,
            attacker_sess,
            "dogmaIM",
            "Activate",
            &[
                PyRep::Integer(gun_id),
                PyRep::String("projectileFired".into()),
                PyRep::Integer(target_ball_id),
            ],
        )
        .await;
        if sys_handle
            .get_ball_snapshot(target_ball_id)
            .await
            .unwrap()
            .is_none()
        {
            break;
        }
        ctx.advance_secs(4);
        sys_handle.step_manual(4.0).await.expect("combat step");
    }
}

#[allow(dead_code)]
pub(crate) fn find_timer_notification(
    notifs: &[PendingMachoNotification],
    event_name: &str,
) -> Option<(i64, Option<i64>)> {
    notifs.iter().rev().find_map(|n| {
        if n.event != event_name {
            return None;
        }
        if let Some(PyRep::Tuple(pair)) = n.args.first() {
            let state = pair.first()?.as_i64()?;
            let expires = pair.get(1).and_then(|v| v.as_i64());
            return Some((state, expires));
        }
        let state = n.args.first()?.as_i64()?;
        let expires = n.args.get(1).and_then(|v| v.as_i64());
        Some((state, expires))
    })
}

// parity: feat:crimewatch.sec-status-loss
// parity: feat:crimewatch.sec-status-gain-caps
// parity: feat:crimewatch.sec-tag-tier-bands
// parity: feat:crimewatch.sec-status-transactions
#[tokio::test]
async fn parity_shard_5_crimewatch_sec_status_loss_caps_tags_and_tx() {
    eve_testkit::attest(&[
        "feat:crimewatch.sec-status-loss",
        "feat:crimewatch.sec-status-gain-caps",
        "feat:crimewatch.sec-tag-tier-bands",
        "feat:crimewatch.sec-status-transactions",
    ]);

    let db_guard = TempSqliteDb::new("crimewatch_sec");
    let ctx = open_sqlite_ctx(db_guard.path(), common::TEST_CLOCK_BASE_UNIX);

    ctx.db
        .provision_player_account("cw_player_delta", "cw-delta-pass");
    let mut delta_sess = SessionState::new(71_003);
    common::login_pilot(&ctx, &mut delta_sess, "cw_player_delta", "cw-delta-pass").await;
    let anc = ctx.sde.ancestries.values().find(|a| a.bloodline_id == 1).map(|a| i64::from(a.ancestry_id)).unwrap_or(1);
    let sch = ctx.sde.schools.values().find(|s| s.race_id == 1).map(|s| i64::from(s.school_id)).unwrap_or(11);
    let delta_rep = rpc(
        &ctx,
        &mut delta_sess,
        "charUnboundMgr",
        "CreateCharacterWithDoll",
        &[
            PyRep::String("CwDeltaVictim".into()),
            PyRep::Integer(1),
            PyRep::Integer(1),
            PyRep::Integer(anc),
            PyRep::Dict(vec![(PyRep::String("sculpting".into()), PyRep::List(vec![]))]),
            PyRep::Dict(vec![(
                PyRep::String("headLookTarget".into()),
                PyRep::Tuple(vec![PyRep::Real(0.0), PyRep::Real(0.0), PyRep::Real(1.0)]),
            )]),
            PyRep::Integer(sch),
        ],
    )
    .await
    .expect("CreateCharacterWithDoll");
    let delta_id = delta_rep
        .as_i64()
        .or_else(|| delta_rep.dict_get("characterID").and_then(|v| v.as_i64()))
        .expect("created char id");
    rpc(
        &ctx,
        &mut delta_sess,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(delta_id)],
    )
    .await
    .expect("select delta");

    let mut atk_sess =
        login_player_session(&ctx, 71_001, "demo", "demo-secret", CHAR_ALPHA_ATTACKER).await;
    let mut vic_sess =
        login_player_session(&ctx, 71_002, "other", "other-secret", CHAR_BETA_VICTIM).await;

    rpc(
        &ctx,
        &mut atk_sess,
        "crimewatch",
        "SetSafetyLevel",
        &[PyRep::Integer(0)],
    )
    .await
    .expect("SetSafetyLevel Red");

    let lowsec_pos_atk = DVec3::new(100_000.0, 0.0, 0.0);
    let lowsec_pos_vic = DVec3::new(102_000.0, 0.0, 0.0);
    let _ = undock_player_to_space(&ctx, &mut atk_sess, ASABONA_SYSTEM, lowsec_pos_atk).await;
    let (vic_ship_id, _) =
        undock_player_to_space(&ctx, &mut vic_sess, ASABONA_SYSTEM, lowsec_pos_vic).await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            100_999_005,
            eve_db::InventoryItemRecord {
                item_id: 100_999_005,
                type_id: 185,
                owner_id: CHAR_ALPHA_ATTACKER,
                location_id: 100_000_001,
                flag: eve_db::InventoryFlag::HiSlot0,
                quantity: 5_000,
                singleton: false,
                ..Default::default()
            },
        );
    });
    ctx.invalidate_ship_fitting(100_000_001);

    let sec_before_low = ctx
        .db
        .with_snapshot(|s| s.characters[&CHAR_ALPHA_ATTACKER].security_status);
    let low_sys_sec = ctx.sde.solar_systems[&ASABONA_SYSTEM].security_status;
    fire_and_destroy_target(
        &ctx,
        &mut atk_sess,
        ASABONA_SYSTEM,
        100_000_005,
        vic_ship_id,
    )
    .await;
    let sec_after_low_ship = ctx
        .db
        .with_snapshot(|s| s.characters[&CHAR_ALPHA_ATTACKER].security_status);
    let expected_ship_drop = compute_sec_status_loss(sec_before_low, 0.0, false, low_sys_sec);
    assert!(
        (sec_before_low - sec_after_low_ship - expected_ship_drop).abs() < 1e-6,
        "low-sec aggression must follow exact EVE security loss formula"
    );

    // High-sec aggression applies strictly steeper security drop than low-sec
    let high_pos_atk = DVec3::new(200_000.0, 0.0, 0.0);
    let high_pos_delta = DVec3::new(201_500.0, 0.0, 0.0);
    ctx.advance_secs(905);
    let _ = undock_player_to_space(&ctx, &mut atk_sess, JITA_SYSTEM, high_pos_atk).await;
    let (delta_ship_id, _) =
        undock_player_to_space(&ctx, &mut delta_sess, JITA_SYSTEM, high_pos_delta).await;
    let sec_before_high = ctx
        .db
        .with_snapshot(|s| s.characters[&CHAR_ALPHA_ATTACKER].security_status);
    fire_and_destroy_target(
        &ctx,
        &mut atk_sess,
        JITA_SYSTEM,
        100_000_005,
        delta_ship_id,
    )
    .await;
    let sec_after_high = ctx
        .db
        .with_snapshot(|s| s.characters[&CHAR_ALPHA_ATTACKER].security_status);
    let high_drop = sec_before_high - sec_after_high;
    assert!(
        high_drop > expected_ship_drop,
        "high-sec aggression penalty ({high_drop}) must exceed low-sec penalty ({expected_ship_drop})"
    );
    assert!((-10.0..=10.0).contains(&sec_after_high));

    // Dock at CONCORD Security Office (service 27) and test all 4 Clone Soldier tag tier bands
    let (concord_st_id, concord_sys_id) = ctx
        .sde
        .stations
        .values()
        .find(|s| {
            ctx.sde
                .solar_systems
                .get(&s.solar_system_id)
                .is_some_and(|sys| sys.security_status > 0.0 && sys.security_status < 0.45)
                && npc_station_offers_service(&ctx.sde, s.station_id, 27)
        })
        .map(|s| (s.station_id, s.solar_system_id))
        .expect("low-sec CONCORD Security Office station");

    ctx.advance_secs(905);
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&CHAR_ALPHA_ATTACKER).unwrap();
        ch.security_status = -9.5;
        ch.solar_system_id = concord_sys_id;
        ch.station_id = Some(concord_st_id);
        atk_sess.solar_system_id = Some(concord_sys_id);
        atk_sess.station_id = Some(concord_st_id);
        for (idx, &(tid, qty)) in
            [(33138_u32, 4_i32), (33139, 6), (33140, 6), (33141, 4)]
                .iter()
                .enumerate()
        {
            let iid = snap.next_entity_id + 10 + idx as i64;
            snap.next_entity_id = iid + 1;
            snap.items.insert(
                iid,
                eve_db::InventoryItemRecord {
                    item_id: iid,
                    type_id: tid,
                    owner_id: CHAR_ALPHA_ATTACKER,
                    location_id: concord_st_id,
                    flag: eve_db::InventoryFlag::Hangar,
                    quantity: qty,
                    singleton: false,
                    ..Default::default()
                },
            );
        }
    });

    // Wrong tag for band must reject with SecurityTagBandMismatch
    let wrong_band = rpc(
        &ctx,
        &mut atk_sess,
        "crimewatch",
        "ApplyTagsForSecStatus",
        &[
            PyRep::Real(-8.0),
            PyRep::Dict(vec![(PyRep::Integer(33141), PyRep::Integer(1))]),
        ],
    )
    .await
    .unwrap_err();
    assert_eq!(wrong_band.code, "SecurityTagBandMismatch");

    // Step through all 4 Clone Soldier security tag bands up to 0.0 cap (-9.5 -> -8.0 takes 3 tags @ 1M = 3M ISK)
    for (tag_tid, qty, target_sec, fee_isk) in [
        (33138_i64, 4_i64, -8.0_f64, 3_000_000.0_f64),
        (33139, 6, -5.0, 12_000_000.0),
        (33140, 6, -2.0, 30_000_000.0),
        (33141, 4, 0.0, 40_000_000.0),
    ] {
        let isk_before = ctx.db.with_snapshot(|s| s.characters[&CHAR_ALPHA_ATTACKER].balance.to_wire_f64());
        let rep = rpc(
            &ctx,
            &mut atk_sess,
            "crimewatch",
            "ApplyTagsForSecStatus",
            &[
                PyRep::Real(target_sec),
                PyRep::Dict(vec![(PyRep::Integer(tag_tid), PyRep::Integer(qty))]),
            ],
        )
        .await
        .expect("ApplyTagsForSecStatus");
        assert!(
            (rep.dict_get("newSecurityStatus").and_then(|v| v.as_f64()).unwrap() - target_sec)
                .abs()
                < 1e-6
        );
        let isk_after = ctx.db.with_snapshot(|s| s.characters[&CHAR_ALPHA_ATTACKER].balance.to_wire_f64());
        assert!((isk_before - isk_after - fee_isk).abs() < 1.0);
    }

    // Tag turn-in above 0.0 is rejected (0.0 cap for tags), while NPC bounty kills reach +5.0 cap
    let above_zero = rpc(
        &ctx,
        &mut atk_sess,
        "crimewatch",
        "ApplyTagsForSecStatus",
        &[
            PyRep::Real(0.2),
            PyRep::Dict(vec![(PyRep::Integer(33141), PyRep::Integer(1))]),
        ],
    )
    .await
    .unwrap_err();
    assert_eq!(above_zero.code, "InvalidTargetSecurityStatus");

    ctx.db.with_mut_snapshot(|snap| {
        snap.characters
            .get_mut(&CHAR_ALPHA_ATTACKER)
            .unwrap()
            .security_status = 4.9999;
    });
    for step in 0..8_i64 {
        credit_specialized_npc_bounty_and_sec_status(
            &ctx,
            ASABONA_SYSTEM,
            88_100_000 + step,
            CHAR_ALPHA_ATTACKER,
            250_000.0,
        );
    }
    let capped_npc_sec = ctx
        .db
        .with_snapshot(|s| s.characters[&CHAR_ALPHA_ATTACKER].security_status);
    assert!(
        (capped_npc_sec - 5.0).abs() < 1e-6,
        "NPC security status gain must cap at +5.0, got {capped_npc_sec}"
    );

    // Reload from SQLite and verify GetSecurityStatusTransactions persists across restart
    drop(atk_sess);
    drop(vic_sess);
    drop(delta_sess);
    drop(ctx);

    let ctx_reloaded = open_sqlite_ctx(db_guard.path(), common::TEST_CLOCK_BASE_UNIX + 2_000);
    let mut atk_reloaded = login_player_session(
        &ctx_reloaded,
        71_011,
        "demo",
        "demo-secret",
        CHAR_ALPHA_ATTACKER,
    )
    .await;
    let PyRep::Rowset { columns, rows } = rpc(
        &ctx_reloaded,
        &mut atk_reloaded,
        "crimewatch",
        "GetSecurityStatusTransactions",
        &[],
    )
    .await
    .expect("GetSecurityStatusTransactions after SQLite reload") else {
        panic!("expected Rowset from GetSecurityStatusTransactions");
    };
    assert_eq!(
        columns,
        vec![
            "eventDate",
            "eventTypeID",
            "locationID",
            "modification",
            "newValue",
            "otherOwnerID",
            "otherTypeID",
        ]
    );
    assert!(
        rows.len() >= 6,
        "persisted security status transactions must survive SQLite reload (got {})",
        rows.len()
    );
}

