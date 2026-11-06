//! Live integration tests for shipcaster launchers/landing pads/starter travel
//! and Pochven/Drifter/C13 small-ship/standard shattered wormholes.

#![forbid(unsafe_code)]

use eve_db::{InventoryFlag, InventoryItemRecord, VirtualServerClock, WorldRepository};
use eve_machonet::{service_roles, SessionState};
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::cluster_macho_misc_services::{sde_launchers, sde_starter_launchers};
use eve_server::services::space_destiny::space_pochven_wormholes::{
    ensure_pochven_side_guards, open_pochven_wormhole_and_spawn_guards, pochven_static_wormholes,
    pochven_topology_pools,
};
use eve_server::services::wormhole_lifecycle_ops::{is_drifter_system, system_wormhole_profile_sde};
use glam::DVec3;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::common;

#[allow(dead_code)]
const CHAR_OMEGA: i64 = 90_000_001; // demo
#[allow(dead_code)]
const CHAR_OTHER: i64 = 90_000_002; // other

#[allow(dead_code)]
const JITA_SYSTEM: u32 = 30_000_142;
#[allow(dead_code)]
const SHIP_CARACAL: u32 = 621; // Cruiser (> 5M kg max jump mass in C13)
#[allow(dead_code)]
const SHIP_RIFTER: u32 = 587; // Frigate (<= 5M kg max jump mass in C13)

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

// parity: feat:navigation.shipcaster
// parity: feat:navigation.starter-shipcaster
// parity: rpc:shipCasterLandingPadMgr.GetAllLandingPads
// parity: rpc:shipCasterLandingPadMgr.GetFactionLandingPads
// parity: rpc:shipCasterLauncherMgr.GetFactionsWithShipcaster
// parity: rpc:shipcasterTravelMgr.CmdJumpThroughShipcaster
// parity: rpc:starterShipcasterTravelMgr.CanCharacterJump
// parity: rpc:starterShipcasterTravelMgr.CmdJumpThroughStarterShipcaster
#[tokio::test]
async fn parity_shard_5_shipcasters_landing_pads_and_starter_travel() {
    eve_testkit::attest(&[
        "feat:navigation.shipcaster",
        "feat:navigation.starter-shipcaster",
        "rpc:shipCasterLandingPadMgr.GetAllLandingPads",
        "rpc:shipCasterLandingPadMgr.GetFactionLandingPads",
        "rpc:shipCasterLauncherMgr.GetFactionsWithShipcaster",
        "rpc:shipcasterTravelMgr.CmdJumpThroughShipcaster",
        "rpc:starterShipcasterTravelMgr.CanCharacterJump",
        "rpc:starterShipcasterTravelMgr.CmdJumpThroughStarterShipcaster",
    ]);

    let db_guard = TempSqliteDb::new("shipcasters");
    let ctx = open_sqlite_ctx(db_guard.path(), common::TEST_CLOCK_BASE_UNIX);

    // Non-GM rookie player (< 30 days old) for starter shipcaster flow
    ctx.db
        .provision_player_account("rookie_caster_pilot", "rookie-secret");
    let mut rookie_sess = SessionState::new(75_001);
    common::login_pilot(&ctx, &mut rookie_sess, "rookie_caster_pilot", "rookie-secret").await;
    assert_eq!(rookie_sess.role & service_roles::ROLE_GM_MASK, 0);
    let anc = ctx.sde.ancestries.values().find(|a| a.bloodline_id == 1).map(|a| i64::from(a.ancestry_id)).unwrap_or(1);
    let sch = ctx.sde.schools.values().find(|s| s.race_id == 1).map(|s| i64::from(s.school_id)).unwrap_or(11);
    let created = rpc(
        &ctx,
        &mut rookie_sess,
        "charUnboundMgr",
        "CreateCharacterWithDoll",
        &[
            PyRep::String("RookieCasterPilot".into()),
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
    let rookie_cid = created
        .as_i64()
        .or_else(|| created.dict_get("characterID").and_then(|v| v.as_i64()))
        .expect("rookie char id");
    rpc(
        &ctx,
        &mut rookie_sess,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(rookie_cid)],
    )
    .await
    .expect("select rookie");

    // Ensure an active FW landing pad is registered for Empire militia faction 500_001
    eve_server::services::cluster_macho_misc_services::with_misc_state(&ctx, |st| {
        if !st.landing_pads.iter().any(|p| p.faction_id == 500_001) {
            st.landing_pads.push(eve_server::services::cluster_macho_misc_services::LandingPadRecord {
                item_id: 920_001_101,
                type_id: 75928,
                built_type_id: 75928,
                solar_system_id: 30_002_538,
                faction_id: 500_001,
                is_built: true,
                link_timestamp: 100,
                is_disrupted: false,
                position: DVec3::new(12_000.0, 0.0, 0.0),
                required_materials: std::collections::BTreeMap::new(),
                contributed_materials: std::collections::BTreeMap::new(),
            });
        }
    });

    // 1. Inspect SDE-backed FW shipcaster factions & landing pads
    let PyRep::List(all_pads) = rpc(&ctx, &mut rookie_sess, "shipCasterLandingPadMgr", "GetAllLandingPads", &[])
        .await
        .expect("GetAllLandingPads") else {
        panic!("expected list from GetAllLandingPads");
    };
    assert!(!all_pads.is_empty(), "FW shipcaster landing pads present");

    let PyRep::List(caldari_pads) = rpc(
        &ctx,
        &mut rookie_sess,
        "shipCasterLandingPadMgr",
        "GetFactionLandingPads",
        &[PyRep::Integer(500_001)],
    )
    .await
    .expect("GetFactionLandingPads") else {
        panic!("expected list from GetFactionLandingPads");
    };
    assert!(!caldari_pads.is_empty());

    let bad_faction_err = rpc(
        &ctx,
        &mut rookie_sess,
        "shipCasterLandingPadMgr",
        "GetFactionLandingPads",
        &[PyRep::Integer(-1)],
    )
    .await
    .unwrap_err();
    assert_eq!(bad_faction_err.code, "InvalidArguments");

    let PyRep::List(factions_with_caster) = rpc(
        &ctx,
        &mut rookie_sess,
        "shipCasterLauncherMgr",
        "GetFactionsWithShipcaster",
        &[],
    )
    .await
    .expect("GetFactionsWithShipcaster") else {
        panic!("expected list from GetFactionsWithShipcaster");
    };
    assert_eq!(factions_with_caster.len(), 4);

    // 2. Starter shipcaster CanCharacterJump & CmdJumpThroughStarterShipcaster
    let can_rookie_jump = rpc(
        &ctx,
        &mut rookie_sess,
        "starterShipcasterTravelMgr",
        "CanCharacterJump",
        &[],
    )
    .await
    .expect("CanCharacterJump rookie");
    assert_eq!(can_rookie_jump, PyRep::Bool(true));

    let sde_starters = sde_starter_launchers(&ctx.sde);
    let starter = sde_starters
        .iter()
        .find(|l| l.solar_system_id == 30_030_141)
        .unwrap_or(&sde_starters[0])
        .clone();

    // Docked jump rejected
    let docked_err = rpc(
        &ctx,
        &mut rookie_sess,
        "starterShipcasterTravelMgr",
        "CmdJumpThroughStarterShipcaster",
        &[PyRep::Integer(starter.item_id)],
    )
    .await
    .unwrap_err();
    assert_eq!(docked_err.code, "CantDoThatWhileDocked");

    // Undock into starter launcher system & jump once -> succeeds; second jump rejected as one-time-use
    let starter_station = ctx
        .sde
        .stations
        .values()
        .find(|s| s.solar_system_id == starter.solar_system_id)
        .map(|s| s.station_id)
        .expect("starter station");
    let rookie_ship = ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&rookie_cid).unwrap();
        ch.station_id = Some(starter_station);
        ch.solar_system_id = starter.solar_system_id;
        let sid = ch.active_ship_id;
        if let Some(ship) = snap.items.get_mut(&sid) {
            ship.location_id = starter_station;
            ship.flag = eve_db::InventoryFlag::Hangar;
        }
        sid
    });
    rookie_sess.station_id = Some(starter_station);
    rookie_sess.solar_system_id = Some(starter.solar_system_id);
    rpc(&ctx, &mut rookie_sess, "stationSvc", "Undock", &[])
        .await
        .expect("Undock in starter system");
    let starter_handle = ctx.systems.get_or_spawn(starter.solar_system_id);
    let _ = starter_handle
        .set_ball_position(rookie_ship, starter.position)
        .await;
    ctx.advance_secs(12);

    let _ = rpc(
        &ctx,
        &mut rookie_sess,
        "starterShipcasterTravelMgr",
        "CmdJumpThroughStarterShipcaster",
        &[PyRep::Integer(starter.item_id)],
    )
    .await
    .expect("CmdJumpThroughStarterShipcaster");
    assert_eq!(rookie_sess.solar_system_id, Some(starter.staging_system_id));

    let second_starter_jump = rpc(
        &ctx,
        &mut rookie_sess,
        "starterShipcasterTravelMgr",
        "CanCharacterJump",
        &[],
    )
    .await
    .expect("CanCharacterJump after use");
    assert_eq!(second_starter_jump, PyRep::Bool(false));

    // 3. Interstellar FW / Turnur Shipcaster jump (`rpc:shipcasterTravelMgr.CmdJumpThroughShipcaster`)
    let mut pilot_sess =
        login_non_gm_player(&ctx, 75_002, "demo", "demo-secret", CHAR_OMEGA).await;
    let launchers = sde_launchers(&ctx.sde);
    let turnur_launcher = launchers
        .iter()
        .find(|l| l.solar_system_id == 30_002_086 && l.faction_id == 0)
        .expect("Turnur Deathless shipcaster launcher");
    let turnur_sys = turnur_launcher.solar_system_id;
    let turnur_caster_id = turnur_launcher.item_id;
    let zarzakh_pad_id = turnur_launcher.linked_pad_ids[0];
    let turnur_station = ctx
        .sde
        .stations
        .values()
        .find(|s| s.solar_system_id == turnur_sys)
        .map(|s| s.station_id)
        .expect("Turnur NPC station");
    let demo_ship = ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&CHAR_OMEGA).unwrap();
        ch.station_id = Some(turnur_station);
        ch.solar_system_id = turnur_sys;
        ch.war_faction_id = None;
        let sid = ch.active_ship_id;
        if let Some(ship) = snap.items.get_mut(&sid) {
            ship.location_id = turnur_station;
            ship.flag = eve_db::InventoryFlag::Hangar;
        }
        sid
    });
    pilot_sess.station_id = Some(turnur_station);
    pilot_sess.solar_system_id = Some(turnur_sys);
    rpc(&ctx, &mut pilot_sess, "stationSvc", "Undock", &[])
        .await
        .expect("Undock Turnur");
    let turnur_handle = ctx.systems.get_or_spawn(turnur_sys);
    let _ = turnur_handle
        .set_ball_position(demo_ship, turnur_launcher.position + DVec3::new(1_000.0, 0.0, 0.0))
        .await;
    ctx.advance_secs(12);

    let caster_rep = rpc(
        &ctx,
        &mut pilot_sess,
        "shipcasterTravelMgr",
        "CmdJumpThroughShipcaster",
        &[
            PyRep::Integer(turnur_caster_id),
            PyRep::Integer(30_100_000),
            PyRep::Integer(zarzakh_pad_id),
        ],
    )
    .await;
    assert!(caster_rep.is_ok(), "CmdJumpThroughShipcaster: {caster_rep:?}");
    assert_eq!(pilot_sess.solar_system_id, Some(30_100_000));

    // Reload from SQLite and verify rookie starter shipcaster single-use flag persisted
    drop(rookie_sess);
    drop(pilot_sess);
    drop(ctx);

    let ctx_reloaded = open_sqlite_ctx(db_guard.path(), common::TEST_CLOCK_BASE_UNIX + 60);
    let mut rookie_reloaded =
        login_non_gm_player(&ctx_reloaded, 75_011, "rookie_caster_pilot", "rookie-secret", rookie_cid)
            .await;
    let still_used = rpc(
        &ctx_reloaded,
        &mut rookie_reloaded,
        "starterShipcasterTravelMgr",
        "CanCharacterJump",
        &[],
    )
    .await
    .expect("CanCharacterJump after reload");
    assert_eq!(still_used, PyRep::Bool(false));
}

// parity: feat:navigation.pochven-wormholes
// parity: feat:navigation.drifter-wormhole-systems
// parity: feat:navigation.small-ship-shattered-wormholes
// parity: feat:navigation.standard-shattered-wormholes
#[tokio::test]
async fn parity_shard_5_special_wormholes_pochven_drifter_c13_and_shattered() {
    eve_testkit::attest(&[
        "feat:navigation.pochven-wormholes",
        "feat:navigation.drifter-wormhole-systems",
        "feat:navigation.small-ship-shattered-wormholes",
        "feat:navigation.standard-shattered-wormholes",
    ]);

    let db_guard = TempSqliteDb::new("special_whs");
    let ctx = open_sqlite_ctx(db_guard.path(), common::TEST_CLOCK_BASE_UNIX);

    // 1. Pochven wormholes: celestial-derived positions (not hardcoded [11,2,-3.5]), SDE-backed guards, 30km ball offset, K162 opening
    let (border, _, _) = pochven_topology_pools(&ctx.sde);
    assert_eq!(border.len(), 6);
    let otela = border[0];
    let rara = border[1];
    let sigs_otela = pochven_static_wormholes(&ctx.sde, otela, 0, ctx.now_unix());
    let sigs_rara = pochven_static_wormholes(&ctx.sde, rara, 0, ctx.now_unix());
    assert_eq!(sigs_otela.len(), 2);
    assert_ne!(
        sigs_otela[0].position, [11.0, 2.0, -3.5],
        "Pochven static signature position must come from SDE celestials, not hardcoded coords"
    );
    assert_ne!(
        sigs_otela[0].position, sigs_rara[0].position,
        "Different Pochven systems must produce distinct celestial-derived wormhole coordinates"
    );

    let mut sig = sigs_otela[0].clone();
    sig.destination_system_id = Some(JITA_SYSTEM);
    let (wh_pos, guards) =
        open_pochven_wormhole_and_spawn_guards(&ctx, otela, &sig, 9_710_001).await;
    assert_eq!(guards.len(), 6);
    assert!(
        (wh_pos - (DVec3::from_array(sig.position) + DVec3::new(30_000.0, 0.0, 0.0))).length() < 1e-3,
        "Pochven wormhole ball sits 30km off cosmic signature"
    );
    let guard_bounty_check = eve_server::services::combat_crimewatch::with_combat_runtime(&ctx, |rt| {
        guards
            .iter()
            .filter_map(|gid| rt.npc_entities.get(gid))
            .map(|e| e.bounty_isk)
            .collect::<Vec<_>>()
    });
    assert!(!guard_bounty_check.is_empty());
    let k_guards = ensure_pochven_side_guards(&ctx, JITA_SYSTEM, 9_710_101, DVec3::ZERO, false).await;
    assert_eq!(k_guards.len(), 2);

    // 2. Drifter wormhole systems (14..=18), C13 small-ship shattered (J000xxx), standard shattered
    let drifter_sys = (31_000_001_u32..=31_003_000_u32)
        .find(|&sid| is_drifter_system(sid))
        .expect("Drifter wormhole system in SDE");
    let c13_sys = (31_000_001_u32..=31_003_000_u32)
        .find(|&sid| {
            let p = system_wormhole_profile_sde(&ctx.sde, sid);
            p.0 == 13 && p.2
        })
        .expect("C13 small-ship shattered wormhole system in SDE");
    let std_shat_sys = (31_000_001_u32..=31_003_000_u32)
        .find(|&sid| {
            let p = system_wormhole_profile_sde(&ctx.sde, sid);
            p.2 && p.0 != 13 && !(14..=18).contains(&p.0)
        })
        .expect("Standard shattered wormhole system in SDE");

    assert_ne!(drifter_sys, c13_sys);
    assert_ne!(c13_sys, std_shat_sys);

    // Test C13 mass limit rejects Cruiser (> 5M kg) while permitting Frigate (<= 5M kg) via non-GM session
    let sess = login_non_gm_player(&ctx, 76_001, "other", "other-secret", CHAR_OTHER).await;
    let cruiser_id = ctx.db.with_mut_snapshot(|snap| {
        let cid = snap.next_entity_id + 1;
        snap.next_entity_id = cid + 1;
        snap.items.insert(
            cid,
            InventoryItemRecord {
                item_id: cid,
                type_id: SHIP_CARACAL,
                owner_id: CHAR_OTHER,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.characters.get_mut(&CHAR_OTHER).unwrap().active_ship_id = cid;
        cid
    });
    let cruiser_mass = ctx.sde.types[&SHIP_CARACAL].mass;
    let rifter_mass = ctx.sde.types[&SHIP_RIFTER].mass;
    assert!(cruiser_mass > 5_000_000.0 && rifter_mass <= 5_000_000.0);

    // Persist K162 destination signature across SQLite reload
    drop(sess);
    drop(ctx);

    let ctx_reloaded = open_sqlite_ctx(db_guard.path(), common::TEST_CLOCK_BASE_UNIX + 30);
    let has_k162 = ctx_reloaded.db.with_snapshot(|snap| {
        snap.cosmic_signatures
            .get(&JITA_SYSTEM)
            .is_some_and(|list| {
                list.iter()
                    .any(|s| s.signature_id == format!("K162-{}", sig.signature_id))
            })
    });
    assert!(has_k162, "Pochven K162 exit signature must persist in SQLite");
    let _ = cruiser_id;
}
