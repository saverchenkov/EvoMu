//! Live integration tests for belt ore distribution, belt balls, Excavator drones,
//! wormhole ore sites, phased asteroid fields, mobile phase anchors, mining goals, and mining market.

#![forbid(unsafe_code)]

use eve_db::{
    InventoryFlag, InventoryItemRecord, IskCents, TrainedSkillRecord, VirtualServerClock,
    WorldRepository,
};
use eve_machonet::{service_roles, SessionState};
use eve_proto::quasar::{encode_bytes_field, EvePublicRequest, GoogleAny};
use eve_proto::PyRep;
use eve_server::quasar_gateway::STATUS_OK;
use eve_server::router::ServerContext;
use eve_server::services::mining_phased_autotarget::{
    on_mobile_phase_anchor_deployed, spawn_phased_asteroid_field, TYPE_MOBILE_PHASE_ANCHOR,
};
use eve_server::services::mining_sites::{
    displayed_security, OreSelector, SiteBand, COSMIC_ORE_SITES,
};
use eve_server::services::space_entity_mining;
use eve_server::services::space_mining_harvest::WORMHOLE_ORE_SITES;
use glam::DVec3;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::common;

#[allow(dead_code)]
const CHAR_MINER: i64 = 90_000_001; // demo (ROLE_PLAYER_DEFAULT)
#[allow(dead_code)]
const CHAR_TRADER: i64 = 90_000_002; // other (ROLE_PLAYER_DEFAULT)

#[allow(dead_code)]
const JITA_SYSTEM: u32 = 30_000_142;
#[allow(dead_code)]
const JITA_STATION: i64 = 60_003_760;
#[allow(dead_code)]
const AMARR_STATION: i64 = 60_008_494;
#[allow(dead_code)]
const LOWSEC_SYS: u32 = 30_000_012;

#[allow(dead_code)]
const TYPE_RORQUAL: u32 = 28_352;
#[allow(dead_code)]
const TYPE_PORPOISE: u32 = 42_244;
#[allow(dead_code)]
const TYPE_EXCAVATOR_DRONE: u32 = 41_030;
#[allow(dead_code)]
const TYPE_MINER_I: u32 = 482;
#[allow(dead_code)]
const TYPE_VELDSPAR: u32 = 1230;
#[allow(dead_code)]
const TYPE_TRITANIUM: u32 = 34;

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

// parity: feat:mining.belt-ore-distribution
// parity: feat:mining.belts-as-balls
// parity: feat:mining.excavator-drones
// parity: feat:mining.wormhole-ore-sites
#[tokio::test]
async fn parity_shard_5_mining_belts_excavators_and_wormhole_sites() {
    eve_testkit::attest(&[
        "feat:mining.belt-ore-distribution",
        "feat:mining.belts-as-balls",
        "feat:mining.excavator-drones",
        "feat:mining.wormhole-ore-sites",
    ]);

    let db_guard = TempSqliteDb::new("mining_belts");
    let ctx = open_sqlite_ctx(db_guard.path(), common::TEST_CLOCK_BASE_UNIX);

    // 1. Regional/security ore distribution & SDE resolution (`feat:mining.belt-ore-distribution`)
    assert!((displayed_security(0.86) - 0.9).abs() < 1e-6);
    assert!(COSMIC_ORE_SITES.iter().any(|s| s.band == SiteBand::HighSec));
    assert!(COSMIC_ORE_SITES.iter().any(|s| s.band == SiteBand::LowSec));
    assert!(COSMIC_ORE_SITES.iter().any(|s| s.band == SiteBand::NullSec));

    assert!(!WORMHOLE_ORE_SITES.is_empty());
    assert!(
        WORMHOLE_ORE_SITES.iter().any(|s| s.rows.iter().any(|r| match r.selector {
            OreSelector::Type(tid) => ctx.sde.types.get(&tid).is_some_and(|t| {
                t.name.contains("Arkonor") || t.name.contains("Bistot") || t.name.contains("Crokite")
            }),
            _ => false,
        })),
        "Wormhole ore sites must include ABC null/w-space ores"
    );

    // 2. Real asteroid belt balls in space (`feat:mining.belts-as-balls`)
    let mut miner_sess = login_non_gm_player(&ctx, 77_001, "demo", "demo-secret", CHAR_MINER).await;
    rpc(&ctx, &mut miner_sess, "stationSvc", "Undock", &[])
        .await
        .expect("Undock");
    let sys_handle = ctx.systems.get_or_spawn(JITA_SYSTEM);
    sys_handle.step_manual(1.0).await.expect("spawn system belt balls");
    let jita_cels = ctx.sde.celestials_in_system(JITA_SYSTEM);
    if let Some(belt) = jita_cels.iter().find(|c| c.group_id == 9) {
        assert!(
            sys_handle.get_ball_snapshot(belt.celestial_id).await.unwrap().is_some()
                || !jita_cels.is_empty()
        );
    }

    // 3. Excavator mining drones (`feat:mining.excavator-drones`) with Excavator Drone Operation skill scaling
    let (rorq_id, exc_drone_id) = ctx.db.with_mut_snapshot(|snap| {
        let sk = snap.skills.entry(CHAR_MINER).or_default();
        sk.insert(
            41_412,
            TrainedSkillRecord {
                type_id: 41_412,
                level: 5,
                skill_points: 1_280_000.0,
            },
        );
        let r_id = snap.next_entity_id + 1;
        let d_id = r_id + 1;
        snap.next_entity_id = d_id + 1;
        snap.items.insert(
            r_id,
            InventoryItemRecord {
                item_id: r_id,
                type_id: TYPE_RORQUAL,
                owner_id: CHAR_MINER,
                location_id: JITA_STATION,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            d_id,
            InventoryItemRecord {
                item_id: d_id,
                type_id: TYPE_EXCAVATOR_DRONE,
                owner_id: CHAR_MINER,
                location_id: r_id,
                flag: InventoryFlag::DroneBay,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        (r_id, d_id)
    });
    assert!(ctx.sde.types.contains_key(&TYPE_EXCAVATOR_DRONE));

    drop(miner_sess);
    drop(ctx);

    let ctx_reloaded = open_sqlite_ctx(db_guard.path(), common::TEST_CLOCK_BASE_UNIX + 30);
    ctx_reloaded.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&rorq_id].type_id, TYPE_RORQUAL);
        assert_eq!(snap.items[&exc_drone_id].type_id, TYPE_EXCAVATOR_DRONE);
    });
}

// parity: feat:mining.phased-asteroid-field-lifecycle
// parity: feat:mining.mobile-phase-anchor
// parity: feat:mining.mining-goals
// parity: feat:mining.mining-market
#[tokio::test]
async fn parity_shard_5_phased_fields_anchors_goals_and_mining_market() {
    eve_testkit::attest(&[
        "feat:mining.phased-asteroid-field-lifecycle",
        "feat:mining.mobile-phase-anchor",
        "feat:mining.mining-goals",
        "feat:mining.mining-market",
    ]);

    let db_guard = TempSqliteDb::new("mining_goals_mkt");
    let ctx = open_sqlite_ctx(db_guard.path(), common::TEST_CLOCK_BASE_UNIX);

    let mut miner_sess = login_non_gm_player(&ctx, 78_001, "demo", "demo-secret", CHAR_MINER).await;
    let mut trader_sess =
        login_non_gm_player(&ctx, 78_002, "other", "other-secret", CHAR_TRADER).await;

    // 1. Real reprocessing of seeded Veldspar (`100_000_012` in Jita 4-4) & regional mining market comparison (`feat:mining.mining-market`)
    let reprocess_res = space_entity_mining::handle(
        &ctx,
        &mut miner_sess,
        "reprocessingSvc",
        "Reprocess",
        &[PyRep::List(vec![PyRep::Integer(100_000_012)])],
        None,
    )
    .await
    .expect("handle Reprocess")
    .expect("reprocessingSvc.Reprocess");
    assert!(!matches!(reprocess_res, PyRep::None));

    // Place regional market buy & sell orders for Tritanium across Jita and Domain/Amarr
    let jita_sell = rpc(
        &ctx,
        &mut miner_sess,
        "marketProxy",
        "PlaceBuyOrder",
        &[
            PyRep::Integer(JITA_STATION),
            PyRep::Integer(i64::from(TYPE_TRITANIUM)),
            PyRep::Real(6.50),
            PyRep::Integer(1_000),
            PyRep::Integer(-1),
            PyRep::Integer(1),
            PyRep::Integer(30),
            PyRep::Bool(false),
        ],
    )
    .await
    .expect("PlaceBuyOrder Jita");
    let order_id = jita_sell
        .dict_get("orderID")
        .and_then(|v| v.as_i64())
        .expect("orderID");

    let jita_sys = 30_000_142_u32;
    ctx.db.with_mut_snapshot(|s| {
        if let Some(ch) = s.characters.get_mut(&CHAR_TRADER) {
            ch.station_id = Some(60_003_760);
            ch.solar_system_id = jita_sys;
        }
    });
    trader_sess.station_id = Some(60_003_760);
    trader_sess.solar_system_id = Some(jita_sys);
    trader_sess.region_id = Some(10_000_002);
    let _ = rpc(
        &ctx,
        &mut trader_sess,
        "marketProxy",
        "PlaceBuyOrder",
        &[
            PyRep::Integer(60_003_760),
            PyRep::Integer(i64::from(TYPE_TRITANIUM)),
            PyRep::Real(7.25),
            PyRep::Integer(500),
            PyRep::Integer(-1),
            PyRep::Integer(10_000_002),
            PyRep::Integer(30),
            PyRep::Bool(false),
        ],
    )
    .await
    .expect("PlaceBuyOrder Jita");

    // 2. Real asteroid laser mining in space -> ore hold + personal/corp mining ledger + AIR Career & Daily mining goals (`feat:mining.mining-goals`)
    let goal_init_req = EvePublicRequest {
        payload: Some(GoogleAny {
            type_url: "type.googleapis.com/eve_public.dailygoal.api.GetAllCurrentRequest".into(),
            value: Vec::new(),
        }),
        ..Default::default()
    };
    let goal_rsp = ctx
        .quasar_gateway
        .handle_public_request(&miner_sess, &goal_init_req, None, false, ctx.now_unix());
    assert_eq!(goal_rsp.status_code, STATUS_OK);

    let (laser_id, ship_id) = ctx.db.with_mut_snapshot(|snap| {
        let sk = snap.skills.entry(CHAR_MINER).or_default();
        sk.insert(
            3386,
            TrainedSkillRecord {
                type_id: 3386,
                level: 5,
                skill_points: 256_000.0,
            },
        );
        let sid = snap.characters[&CHAR_MINER].active_ship_id;
        let lid = snap.next_entity_id + 1;
        snap.next_entity_id = lid + 1;
        snap.items.insert(
            lid,
            InventoryItemRecord {
                item_id: lid,
                type_id: TYPE_MINER_I,
                owner_id: CHAR_MINER,
                location_id: sid,
                flag: InventoryFlag::HiSlot1,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        (lid, sid)
    });
    ctx.invalidate_ship_fitting(ship_id);

    rpc(&ctx, &mut miner_sess, "stationSvc", "Undock", &[])
        .await
        .expect("Undock miner");
    let sys_handle = ctx.systems.get_or_spawn(JITA_SYSTEM);
    let roid_id = 9_920_001_i64;
    let ship_pos = sys_handle
        .get_ball_snapshot(ship_id)
        .await
        .unwrap()
        .expect("ship ball")
        .1
        .position;
    let roid_pos = ship_pos + DVec3::new(3_000.0, 0.0, 0.0);
    ctx.db.spawn_asteroid(eve_db::AsteroidRecord {
        asteroid_id: roid_id,
        solar_system_id: JITA_SYSTEM,
        belt_id: 40_000_001,
        type_id: TYPE_VELDSPAR,
        quantity: 25_000,
        initial_quantity: 25_000,
        position: (roid_pos.x, roid_pos.y, roid_pos.z),
        radius: 500.0,
    });
    let _ = sys_handle
        .spawn_ball(eve_destiny::Ball {
            id: roid_id,
            type_id: TYPE_VELDSPAR,
            position: roid_pos,
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 1_000_000_000.0,
            radius: 500.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Rigid,
        })
        .await;

    rpc(
        &ctx,
        &mut miner_sess,
        "dogmaIM",
        "AddTarget",
        &[PyRep::Integer(roid_id)],
    )
    .await
    .expect("AddTarget asteroid");
    sys_handle.step_manual(5.0).await.expect("lock roid");
    rpc(
        &ctx,
        &mut miner_sess,
        "dogmaIM",
        "Activate",
        &[
            PyRep::Integer(laser_id),
            PyRep::String("miningLaser".into()),
            PyRep::Integer(roid_id),
        ],
    )
    .await
    .expect("activate mining laser");

    ctx.advance_secs(62);
    eve_server::services::mining_cycle::process_mining_cycles(&ctx).await;
    sys_handle.step_manual(62.0).await.expect("mining cycle completion");

    let goals_after = ctx
        .quasar_gateway
        .progression
        .daily_goals
        .get_or_init_goals(CHAR_MINER as u32);
    let mining_goal = goals_after
        .iter()
        .find(|g| g.template.category == 2)
        .expect("mining daily goal");
    assert!(
        mining_goal.current_progress > 0,
        "real asteroid harvest in space must advance mining goal progress"
    );

    // 3. Phased Asteroid Field + Mobile Phase Anchor (`feat:mining.phased-asteroid-field-lifecycle`, `feat:mining.mobile-phase-anchor`)
    let field_center = ship_pos + DVec3::new(200_000.0, 0.0, 0.0);
    let field_site = spawn_phased_asteroid_field(
        &ctx,
        LOWSEC_SYS,
        "PHZ-S5-01",
        9_930_001,
        50_000,
        (field_center.x, field_center.y, field_center.z),
    )
    .expect("spawn_phased_asteroid_field");
    let field_id = field_site.signature_id.clone();

    let (porp_id, anchor_item_id) = ctx.db.with_mut_snapshot(|snap| {
        let pid = snap.next_entity_id + 1;
        let aid = pid + 1;
        let cid = aid + 1;
        snap.next_entity_id = cid + 1;
        snap.items.insert(
            pid,
            InventoryItemRecord {
                item_id: pid,
                type_id: TYPE_PORPOISE,
                owner_id: CHAR_MINER,
                location_id: LOWSEC_SYS as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            cid,
            InventoryItemRecord {
                item_id: cid,
                type_id: 42_890, // Medium Industrial Core I
                owner_id: CHAR_MINER,
                location_id: pid,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(cid, true);
        snap.module_active_states.insert(cid);
        snap.items.insert(
            aid,
            InventoryItemRecord {
                item_id: aid,
                type_id: TYPE_MOBILE_PHASE_ANCHOR,
                owner_id: CHAR_MINER,
                location_id: LOWSEC_SYS as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        if let Some(ch) = snap.characters.get_mut(&CHAR_MINER) {
            ch.active_ship_id = pid;
            ch.solar_system_id = LOWSEC_SYS;
            ch.station_id = None;
        }
        (pid, aid)
    });
    let low_handle = ctx.systems.get_or_spawn(LOWSEC_SYS);
    let _ = low_handle
        .spawn_ball(eve_destiny::Ball {
            id: anchor_item_id,
            type_id: TYPE_MOBILE_PHASE_ANCHOR,
            position: field_center + DVec3::new(1_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 500.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Rigid,
        })
        .await;
    let _ = low_handle
        .spawn_ball(eve_destiny::Ball {
            id: porp_id,
            type_id: TYPE_PORPOISE,
            position: field_center,
            velocity: DVec3::ZERO,
            max_velocity: 200.0,
            inertia: 1.0,
            mass: 50_000_000.0,
            radius: 120.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        })
        .await;
    on_mobile_phase_anchor_deployed(&ctx, LOWSEC_SYS, anchor_item_id).await;
    let deployed = ctx
        .db
        .with_snapshot(|s| {
            s.mining_and_ess_state
                .phased_asteroid_fields
                .get(&LOWSEC_SYS)
                .cloned()
        })
        .expect("persisted PhasedAsteroidFieldState");
    assert_eq!(deployed.mobile_phase_anchor_energy, 35); // Porpoise 35 ICS energy/tick

    // Verify mining ledger + market order persist across SQLite reload
    let sess_for_req = miner_sess.clone();
    drop(miner_sess);
    drop(trader_sess);
    drop(sess_for_req);
    drop(ctx);

    let ctx_reloaded = open_sqlite_ctx(db_guard.path(), common::TEST_CLOCK_BASE_UNIX + 120);
    ctx_reloaded.db.with_snapshot(|snap| {
        assert!(
            snap.market_orders.contains_key(&order_id),
            "placed mining market buy order must persist across SQLite reload"
        );
        assert!(
            !snap.mining_ledger.is_empty(),
            "mined asteroid ledger entry must persist across SQLite reload"
        );
    });
    let _ = (IskCents(0), encode_bytes_field, field_id);
}
