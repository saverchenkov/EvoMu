//! Integration tests remediating batch 2 of `fake_done` RPC rows.

#![forbid(unsafe_code)]

use std::collections::{HashMap, HashSet};

use eve_db::{
    AllianceRecord, CorporationRecord, InventoryFlag, InventoryItemRecord, UpwellStructureRecord,
};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use eve_server::services::cluster_macho_misc_services::with_misc_state;
use glam::DVec3;

use super::common;

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, svc, None, method, args, &HashMap::new(), None)
        .await
}

fn kv_i64(rep: &PyRep, key: &str) -> Option<i64> {
    match rep {
        PyRep::KeyVal(m) => m.get(key).and_then(|v| v.as_i64()),
        PyRep::Dict(entries) => entries
            .iter()
            .find(|(k, _)| k.as_str() == Some(key))
            .and_then(|(_, v)| v.as_i64()),
        _ => None,
    }
}

fn kv_bool(rep: &PyRep, key: &str) -> Option<bool> {
    match rep {
        PyRep::KeyVal(m) => m.get(key).and_then(|v| v.as_bool()),
        PyRep::Dict(entries) => entries
            .iter()
            .find(|(k, _)| k.as_str() == Some(key))
            .and_then(|(_, v)| v.as_bool()),
        _ => None,
    }
}

async fn setup_pilot(ctx: &ServerContext, sess_id: u64, user: &str, pass: &str, char_id: i64) -> SessionState {
    let mut sess = SessionState::new(sess_id);
    common::login_pilot(ctx, &mut sess, user, pass).await;
    rpc(
        ctx,
        &mut sess,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
    )
    .await
    .expect("SelectCharacterID");
    sess
}

#[tokio::test]
async fn test_batch2_cache_setting_and_load_service() {
    // parity: rpc:cache.Setting
    // parity: rpc:loadService.StartLoad
    // parity: rpc:loadService.StopLoad
    // parity: rpc:loadService.SetConfig
    // parity: rpc:loadService.GetTotalStats
    eve_testkit::attest(&[
        "rpc:cache.Setting",
        "rpc:loadService.StartLoad",
        "rpc:loadService.StopLoad",
        "rpc:loadService.SetConfig",
        "rpc:loadService.GetTotalStats",
    ]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let mut player = setup_pilot(&ctx, 41_001, "demo", "demo-secret", 90_000_001).await;
    let mut gm = setup_pilot(&ctx, 41_002, "other", "other-secret", 90_000_002).await;
    gm.role |= eve_db::service_roles::ROLE_ADMIN;

    // 1. cache.Setting: invalid args rejected, non-GM write rejected, GM write/read/clear persisted
    let bad_args = rpc(&ctx, &mut gm, "cache", "Setting", &[])
        .await
        .expect_err("empty args must fail");
    assert_eq!(bad_args.code, "InvalidArguments");

    let unauth_write = rpc(
        &ctx,
        &mut player,
        "cache",
        "Setting",
        &[
            PyRep::String("zsystem".into()),
            PyRep::String("Batch2Setting".into()),
            PyRep::String("enabled".into()),
        ],
    )
    .await
    .expect_err("non-GM cache.Setting write must fail");
    assert_eq!(unauth_write.code, "UserError");

    let written = rpc(
        &ctx,
        &mut gm,
        "cache",
        "Setting",
        &[
            PyRep::String("zsystem".into()),
            PyRep::String("Batch2Setting".into()),
            PyRep::String("enabled".into()),
        ],
    )
    .await
    .expect("GM cache.Setting write");
    assert_eq!(written, PyRep::String("enabled".into()));

    let read_val = rpc(
        &ctx,
        &mut player,
        "cache",
        "Setting",
        &[
            PyRep::String("zsystem".into()),
            PyRep::String("Batch2Setting".into()),
        ],
    )
    .await
    .expect("cache.Setting read");
    assert_eq!(read_val, PyRep::String("enabled".into()));

    let cleared = rpc(
        &ctx,
        &mut gm,
        "cache",
        "Setting",
        &[
            PyRep::String("zsystem".into()),
            PyRep::String("Batch2Setting".into()),
            PyRep::None,
        ],
    )
    .await
    .expect("GM cache.Setting clear");
    assert_eq!(cleared, PyRep::None);

    let after_clear = rpc(
        &ctx,
        &mut player,
        "cache",
        "Setting",
        &[
            PyRep::String("zsystem".into()),
            PyRep::String("Batch2Setting".into()),
        ],
    )
    .await
    .expect("cache.Setting read after clear");
    assert_eq!(after_clear, PyRep::String(String::new()));

    // 2. loadService: non-GM rejected, invalid solar system rejected, SetConfig/StartLoad/GetTotalStats/StopLoad
    let unauth_load = rpc(&ctx, &mut player, "loadService", "StartLoad", &[PyRep::Integer(2)])
        .await
        .expect_err("non-GM StartLoad must fail");
    assert_eq!(unauth_load.code, "UserError");

    let bad_sys = rpc(
        &ctx,
        &mut gm,
        "loadService",
        "StartLoad",
        &[PyRep::Dict(vec![
            (PyRep::String("solar_system_id".into()), PyRep::Integer(99_999_999)),
            (PyRep::String("ticks".into()), PyRep::Integer(2)),
        ])],
    )
    .await
    .expect_err("invalid solar_system_id must fail");
    assert_eq!(bad_sys.code, "UserError");

    rpc(
        &ctx,
        &mut gm,
        "loadService",
        "SetConfig",
        &[PyRep::Dict(vec![(
            PyRep::String("mode".into()),
            PyRep::String("stress".into()),
        )])],
    )
    .await
    .expect("SetConfig");

    rpc(
        &ctx,
        &mut gm,
        "loadService",
        "StartLoad",
        &[PyRep::Dict(vec![
            (PyRep::String("solar_system_id".into()), PyRep::Integer(30_000_142)),
            (PyRep::String("ticks".into()), PyRep::Integer(3)),
        ])],
    )
    .await
    .expect("StartLoad");

    let stats_active = rpc(&ctx, &mut gm, "loadService", "GetTotalStats", &[])
        .await
        .expect("GetTotalStats active");
    assert_eq!(kv_bool(&stats_active, "active"), Some(true));
    assert_eq!(kv_i64(&stats_active, "runs_started"), Some(1));
    assert_eq!(kv_i64(&stats_active, "ticks_executed"), Some(3));
    assert_eq!(kv_i64(&stats_active, "systems_stepped"), Some(1));

    rpc(&ctx, &mut gm, "loadService", "StopLoad", &[])
        .await
        .expect("StopLoad");

    let stats_stopped = rpc(&ctx, &mut gm, "loadService", "GetTotalStats", &[])
        .await
        .expect("GetTotalStats stopped");
    assert_eq!(kv_bool(&stats_stopped, "active"), Some(false));
    assert_eq!(kv_i64(&stats_stopped, "total_requests"), Some(3));
}

#[tokio::test]
async fn test_batch2_lookup_warable_and_sov_structures() {
    // parity: rpc:lookupSvc.LookupWarableCorporationsOrAlliances
    // parity: rpc:sovMgr.GetSovStructuresInfoForSolarSystem
    eve_testkit::attest(&[
        "rpc:lookupSvc.LookupWarableCorporationsOrAlliances",
        "rpc:sovMgr.GetSovStructuresInfoForSolarSystem",
    ]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let mut sess = setup_pilot(&ctx, 41_003, "demo", "demo-secret", 90_000_001).await;

    let caller_corp = 98_000_001_i64;
    let warable_corp = 98_000_701_i64;
    let non_warable_corp = 98_000_702_i64;
    let warable_alliance = 99_000_701_i64;
    let sys_id = 30_000_142_u32;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.corporation_id = caller_corp;
        }
        snap.corporations.insert(
            warable_corp,
            CorporationRecord {
                corporation_id: warable_corp,
                name: "WarEligible Vanguard".into(),
                ticker: "WEVG".into(),
                ceo_id: 90_000_002,
                ..Default::default()
            },
        );
        snap.corporations.insert(
            non_warable_corp,
            CorporationRecord {
                corporation_id: non_warable_corp,
                name: "WarEligible Pacifists".into(),
                ticker: "WEPF".into(),
                ceo_id: 90_000_003,
                ..Default::default()
            },
        );
        snap.alliances.insert(
            warable_alliance,
            AllianceRecord {
                alliance_id: warable_alliance,
                name: "WarEligible Coalition".into(),
                short_name: "WEC".into(),
                executor_corp_id: warable_corp,
                member_corp_ids: HashSet::from([warable_corp]),
                ..Default::default()
            },
        );
        // Upwell Astrahus (non-sov) owned by warable_corp
        snap.structures.insert(
            1_000_000_901_001,
            UpwellStructureRecord {
                structure_id: 1_000_000_901_001,
                type_id: 35832,
                solar_system_id: sys_id,
                owner_corp_id: warable_corp,
                state: "ShieldVulnerable".into(),
                ..Default::default()
            },
        );
        // Sovereignty Hub (sov structure, type 81080) owned by warable_corp
        snap.structures.insert(
            1_000_000_901_002,
            UpwellStructureRecord {
                structure_id: 1_000_000_901_002,
                type_id: 81080,
                solar_system_id: sys_id,
                owner_corp_id: warable_corp,
                state: "ShieldVulnerable".into(),
                ..Default::default()
            },
        );
    });
    sess.corporation_id = Some(caller_corp);

    // 1. LookupWarableCorporationsOrAlliances: returns warable_corp & warable_alliance, excludes non_warable_corp
    let lookup_rep = rpc(
        &ctx,
        &mut sess,
        "lookupSvc",
        "LookupWarableCorporationsOrAlliances",
        &[PyRep::String("WarEligible".into()), PyRep::Bool(false)],
    )
    .await
    .expect("LookupWarableCorporationsOrAlliances");
    let PyRep::List(rows) = lookup_rep else {
        panic!("expected List from LookupWarableCorporationsOrAlliances");
    };
    let owner_ids: Vec<i64> = rows.iter().filter_map(|r| kv_i64(r, "ownerID")).collect();
    assert!(owner_ids.contains(&warable_corp), "warable corp must be returned");
    assert!(owner_ids.contains(&warable_alliance), "warable alliance must be returned");
    assert!(!owner_ids.contains(&non_warable_corp), "non-warable corp must be excluded");
    assert!(!owner_ids.contains(&caller_corp), "caller corp must be excluded");

    // 2. GetSovStructuresInfoForSolarSystem: returns only sov structures (81080), not Astrahus (35832)
    let sov_rep = rpc(
        &ctx,
        &mut sess,
        "sovMgr",
        "GetSovStructuresInfoForSolarSystem",
        &[PyRep::Integer(i64::from(sys_id))],
    )
    .await
    .expect("GetSovStructuresInfoForSolarSystem");
    let PyRep::List(sov_list) = sov_rep else {
        panic!("expected List from GetSovStructuresInfoForSolarSystem");
    };
    let struct_ids: Vec<i64> = sov_list.iter().filter_map(|r| kv_i64(r, "structureID")).collect();
    assert!(struct_ids.contains(&1_000_000_901_002), "Sov Hub must be included");
    assert!(!struct_ids.contains(&1_000_000_901_001), "Astrahus must be excluded");

    // Negative case: invalid solar system
    let bad_sov = rpc(
        &ctx,
        &mut sess,
        "sovMgr",
        "GetSovStructuresInfoForSolarSystem",
        &[PyRep::Integer(99_999_999)],
    )
    .await
    .expect_err("invalid solar system must fail");
    assert_eq!(bad_sov.code, "InvalidSolarSystemID");
}

#[tokio::test]
async fn test_batch2_raffle_character_seeding_and_create() {
    // parity: rpc:raffleMgr.character_Seeding
    // parity: rpc:raffleProxy.CreateRaffle
    eve_testkit::attest(&[
        "rpc:raffleMgr.character_Seeding",
        "rpc:raffleProxy.CreateRaffle",
    ]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let mut player = setup_pilot(&ctx, 41_004, "demo", "demo-secret", 90_000_001).await;
    let mut gm = setup_pilot(&ctx, 41_005, "other", "other-secret", 90_000_002).await;
    gm.role |= eve_db::service_roles::ROLE_ADMIN;

    let station_id = 60_003_760_i64;
    player.station_id = Some(station_id);
    player.structure_id = None;

    // 1. character_Seeding: non-GM rejected, invalid ticket count rejected, valid seeding for target char
    let unauth_seed = rpc(
        &ctx,
        &mut player,
        "raffleMgr",
        "character_Seeding",
        &[PyRep::Integer(90_000_001), PyRep::Integer(1)],
    )
    .await
    .expect_err("non-GM character_Seeding must fail");
    assert_eq!(unauth_seed.code, "UserError");

    let bad_tickets = rpc(
        &ctx,
        &mut gm,
        "raffleMgr",
        "character_Seeding",
        &[
            PyRep::Integer(90_000_001),
            PyRep::Integer(1),
            PyRep::Integer(587),
            PyRep::Integer(12),
        ],
    )
    .await
    .expect_err("invalid ticket count must fail");
    assert_eq!(bad_tickets.code, "InvalidTicketCount");

    let seeded = rpc(
        &ctx,
        &mut gm,
        "raffleMgr",
        "character_Seeding",
        &[
            PyRep::Integer(90_000_001),
            PyRep::Integer(2),
            PyRep::Integer(587),
            PyRep::Integer(16),
            PyRep::Real(50_000.0),
        ],
    )
    .await
    .expect("character_Seeding");
    let PyRep::List(seeded_list) = seeded else {
        panic!("expected List from character_Seeding");
    };
    assert_eq!(seeded_list.len(), 2);
    assert_eq!(kv_i64(&seeded_list[0], "owner_id"), Some(90_000_001));
    assert_eq!(kv_i64(&seeded_list[0], "ticket_count"), Some(16));

    // 2. raffleProxy.CreateRaffle: escrow item + consume HyperCores, and reject undocked
    let raffle_item_id = 880_000_001_i64;
    let cores_item_id = 880_000_002_i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.station_id = Some(station_id);
            ch.structure_id = None;
        }
        snap.items.insert(
            raffle_item_id,
            InventoryItemRecord {
                item_id: raffle_item_id,
                type_id: 587,
                owner_id: 90_000_001,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            cores_item_id,
            InventoryItemRecord {
                item_id: cores_item_id,
                type_id: 52_568,
                owner_id: 90_000_001,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 50,
                ..Default::default()
            },
        );
    });

    let created = rpc(
        &ctx,
        &mut player,
        "raffleProxy",
        "CreateRaffle",
        &[
            PyRep::Integer(raffle_item_id),
            PyRep::Integer(8),
            PyRep::Real(100_000.0),
        ],
    )
    .await
    .expect("CreateRaffle");
    assert_eq!(kv_i64(&created, "item_id"), Some(raffle_item_id));
    assert_eq!(kv_i64(&created, "owner_id"), Some(90_000_001));

    ctx.db.with_snapshot(|snap| {
        let escrowed = snap.items.get(&raffle_item_id).expect("escrowed item");
        assert_eq!(escrowed.owner_id, -52_568);
        assert_eq!(escrowed.flag, InventoryFlag::Locked);
        let rem_cores = snap.items.get(&cores_item_id).map(|i| i.quantity).unwrap_or(0);
        assert!(rem_cores < 50, "HyperCores must be consumed");
    });

    // Negative case: undocked CreateRaffle fails
    player.station_id = None;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.station_id = None;
        }
    });
    let undocked_err = rpc(
        &ctx,
        &mut player,
        "raffleProxy",
        "CreateRaffle",
        &[
            PyRep::Integer(raffle_item_id),
            PyRep::Integer(8),
            PyRep::Real(100_000.0),
        ],
    )
    .await
    .expect_err("undocked CreateRaffle must fail");
    assert_eq!(undocked_err.code, "InvalidRaffleLocation");
}

#[tokio::test]
async fn test_batch2_shipcaster_pads_launchers_and_travel() {
    // parity: rpc:shipCasterLandingPadMgr.GetAllLandingPads
    // parity: rpc:shipCasterLandingPadMgr.GetFactionLandingPads
    // parity: rpc:shipCasterLauncherMgr.GetFactionsWithShipcaster
    // parity: rpc:shipcasterTravelMgr.CmdJumpThroughShipcaster
    // parity: rpc:starterShipcasterTravelMgr.CanCharacterJump
    // parity: rpc:starterShipcasterTravelMgr.CmdJumpThroughStarterShipcaster
    eve_testkit::attest(&[
        "rpc:shipCasterLandingPadMgr.GetAllLandingPads",
        "rpc:shipCasterLandingPadMgr.GetFactionLandingPads",
        "rpc:shipCasterLauncherMgr.GetFactionsWithShipcaster",
        "rpc:shipcasterTravelMgr.CmdJumpThroughShipcaster",
        "rpc:starterShipcasterTravelMgr.CanCharacterJump",
        "rpc:starterShipcasterTravelMgr.CmdJumpThroughStarterShipcaster",
    ]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let mut sess = setup_pilot(&ctx, 41_006, "demo", "demo-secret", 90_000_001).await;
    ctx.advance_secs(15);
    let char_id = 90_000_001_i64;
    let ship_id = sess.ship_id.unwrap_or(100_000_001);

    // 1. GetFactionsWithShipcaster, GetAllLandingPads, GetFactionLandingPads
    let factions = rpc(&ctx, &mut sess, "shipCasterLauncherMgr", "GetFactionsWithShipcaster", &[])
        .await
        .expect("GetFactionsWithShipcaster");
    let PyRep::List(fac_list) = factions else {
        panic!("expected List from GetFactionsWithShipcaster");
    };
    assert!(fac_list.contains(&PyRep::Integer(500_001)));

    let all_pads = rpc(&ctx, &mut sess, "shipCasterLandingPadMgr", "GetAllLandingPads", &[])
        .await
        .expect("GetAllLandingPads");
    assert!(matches!(all_pads, PyRep::List(_)));

    let bad_fac_pads = rpc(&ctx, &mut sess, "shipCasterLandingPadMgr", "GetFactionLandingPads", &[])
        .await
        .expect_err("missing factionID must fail");
    assert_eq!(bad_fac_pads.code, "InvalidArguments");

    // 2. Starter shipcaster: CanCharacterJump + CmdJumpThroughStarterShipcaster + one-time use
    let now_unix = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.create_date_unix = now_unix;
        }
    });
    let can_jump = rpc(&ctx, &mut sess, "starterShipcasterTravelMgr", "CanCharacterJump", &[])
        .await
        .expect("CanCharacterJump");
    assert_eq!(can_jump, PyRep::Bool(true));

    let (starter_id, starter_sys, starter_pos, dest_sys) = with_misc_state(&ctx, |st| {
        let l = st.starter_launchers.first().expect("starter launcher");
        (l.item_id, l.solar_system_id, l.position, l.staging_system_id)
    });

    sess.station_id = None;
    sess.structure_id = None;
    sess.ship_id = Some(ship_id);
    sess.solar_system_id = Some(starter_sys);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = starter_sys;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = ship_id;
        }
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            ship.location_id = i64::from(starter_sys);
        }
    });
    let sys_handle = ctx.systems.get_or_spawn(starter_sys);
    let _ = sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: 587,
                position: starter_pos + DVec3::new(1_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 1000.0,
                inertia: 1.0,
                mass: 100_000.0,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_ship_combat_state(char_id, ship_id, 587),
        )
        .await;

    rpc(
        &ctx,
        &mut sess,
        "starterShipcasterTravelMgr",
        "CmdJumpThroughStarterShipcaster",
        &[PyRep::Integer(starter_id)],
    )
    .await
    .expect("CmdJumpThroughStarterShipcaster");
    assert_eq!(sess.solar_system_id, Some(dest_sys));

    let can_jump_after = rpc(&ctx, &mut sess, "starterShipcasterTravelMgr", "CanCharacterJump", &[])
        .await
        .expect("CanCharacterJump after use");
    assert_eq!(can_jump_after, PyRep::Bool(false));

    // 3. Interstellar shipcaster: negative case (unknown launcher)
    let bad_jump = rpc(
        &ctx,
        &mut sess,
        "shipcasterTravelMgr",
        "CmdJumpThroughShipcaster",
        &[
            PyRep::Integer(999_999_999),
            PyRep::Integer(30_000_142),
            PyRep::Integer(888_888_888),
        ],
    )
    .await
    .expect_err("unknown launcher must fail");
    assert_eq!(bad_jump.code, "LauncherNotFound");
}
