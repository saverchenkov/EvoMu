//! Real production server-to-client `notify:On*` / `notify:Do*` emission integration tests (Batch 10, Part 5).
//!
//! Covers `DoDestinyUpdates`, `DoSimClockRebase`, `OnDestinationChange` (via ESI HTTP),
//! `OnServerMapDataPush`, `OnSystemActivityUpdated` (via `industryManager.InstallJob`),
//! `OnCharacterEnergyChanged` (via CRAB beacon `beyonce.CallComponentFromClient`), and
//! `OnAnalysisKreditsChange` (via `ProjectDiscovery.post_classification`).

#![forbid(unsafe_code)]

use eve_db::{InventoryFlag, InventoryItemRecord, IskCents, KillmailRecord};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::esi::{EsiHttpRequest, EsiRouter};
use eve_server::router::ServerContext;
use glam::DVec3;
use std::collections::HashMap;
use std::sync::Arc;

const PILOT_A: i64 = 90_000_001;
const PILOT_B: i64 = 90_000_002;
const SYS_JITA: u32 = 30_000_142;

fn make_item(
    item_id: i64,
    type_id: u32,
    owner_id: i64,
    location_id: i64,
    flag: InventoryFlag,
    quantity: i32,
    singleton: bool,
) -> InventoryItemRecord {
    InventoryItemRecord {
        item_id,
        type_id,
        owner_id,
        location_id,
        flag,
        quantity,
        singleton,
        version: 1,
        ..Default::default()
    }
}

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

fn drain_all(
    ctx: &ServerContext,
    sess: &mut SessionState,
    char_id: i64,
) -> Vec<(String, Vec<PyRep>)> {
    let mut out = sess.drain_notifications();
    for n in ctx.drain_notifications_for_char(char_id) {
        out.push((n.event, n.args));
    }
    out
}

fn find_notif<'a>(notifs: &'a [(String, Vec<PyRep>)], event: &str) -> &'a [PyRep] {
    notifs
        .iter()
        .find(|(ev, _)| ev == event)
        .map(|(_, args)| args.as_slice())
        .unwrap_or_else(|| {
            let seen: Vec<&str> = notifs.iter().map(|(e, _)| e.as_str()).collect();
            panic!("expected notification {event}, got {seen:?}")
        })
}

fn has_notif(notifs: &[(String, Vec<PyRep>)], event: &str) -> bool {
    notifs.iter().any(|(ev, _)| ev == event)
}

async fn setup_pilot_in_space(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    ship_type_id: u32,
    sys_id: u32,
) -> SessionState {
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = ship_id;
        }
        snap.items.insert(
            ship_id,
            make_item(
                ship_id,
                ship_type_id,
                char_id,
                i64::from(sys_id),
                InventoryFlag::None,
                1,
                true,
            ),
        );
    });
    let sys = ctx.systems.get_or_spawn(sys_id);
    let _ = sys
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: ship_type_id,
                position: DVec3::new(1000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 0.5,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            Default::default(),
        )
        .await;
    let mut sess = SessionState::new(char_id as u64);
    crate::common::login_pilot(ctx, &mut sess, "demo", "demo-secret").await;
    rpc(
        ctx,
        &mut sess,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
    )
    .await
    .expect("SelectCharacterID");
    sess.solar_system_id = Some(sys_id);
    sess.station_id = None;
    sess.structure_id = None;
    sess.ship_id = Some(ship_id);
    let _ = drain_all(ctx, &mut sess, char_id);
    sess
}

/// parity: notify:DoDestinyUpdates
/// parity: notify:DoSimClockRebase
/// parity: notify:OnDestinationChange
/// parity: notify:OnServerMapDataPush
/// parity: notify:OnSystemActivityUpdated
/// parity: notify:OnCharacterEnergyChanged
/// parity: notify:OnAnalysisKreditsChange
#[tokio::test]
async fn batch10_destiny_clock_esi_map_industry_energy_and_discovery() {
    let ctx = Arc::new(ServerContext::new());
    let mut sess = setup_pilot_in_space(&ctx, PILOT_A, 700_701, 587, SYS_JITA).await;

    // 1. DoDestinyUpdates via live beyonce.UpdateStateRequest
    // Negative: docked pilot calling UpdateStateRequest does NOT emit DoDestinyUpdates
    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&PILOT_A).unwrap().station_id = Some(60_003_760);
    });
    sess.station_id = Some(60_003_760);
    let _ = rpc(&ctx, &mut sess, "beyonce", "UpdateStateRequest", &[]).await;
    assert!(!has_notif(
        &drain_all(&ctx, &mut sess, PILOT_A),
        "DoDestinyUpdates"
    ));

    // Positive: undocked pilot in space receives DoDestinyUpdates([(stamp, ("SetState", KeyVal))])
    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&PILOT_A).unwrap().station_id = None;
    });
    sess.station_id = None;
    rpc(&ctx, &mut sess, "beyonce", "UpdateStateRequest", &[])
        .await
        .unwrap();
    let dest_notifs = drain_all(&ctx, &mut sess, PILOT_A);
    let du_args = find_notif(&dest_notifs, "DoDestinyUpdates");
    assert_eq!(du_args.len(), 1);
    match &du_args[0] {
        PyRep::List(updates) => {
            assert!(!updates.is_empty());
            match &updates[0] {
                PyRep::Tuple(pair) => {
                    assert_eq!(pair.len(), 2);
                    match &pair[1] {
                        PyRep::Tuple(cmd) => assert_eq!(cmd[0], PyRep::String("SetState".into())),
                        other => panic!("expected (\"SetState\", payload), got {other:?}"),
                    }
                }
                other => panic!("expected (stamp, cmd) tuple, got {other:?}"),
            }
        }
        other => panic!("expected List of updates, got {other:?}"),
    }

    // 2. DoSimClockRebase via live ctx.advance_secs
    // Negative: advancing 0 seconds does NOT emit DoSimClockRebase
    ctx.advance_secs(0);
    assert!(!has_notif(
        &drain_all(&ctx, &mut sess, PILOT_A),
        "DoSimClockRebase"
    ));

    // Positive: advancing 15 seconds emits 1-arg 2-tuple ((old_ft, new_ft),) with exact 15s filetime delta
    let before_ft = ctx.now_blueos_filetime_ticks();
    ctx.advance_secs(15);
    let rebase_notifs = drain_all(&ctx, &mut sess, PILOT_A);
    let rb_args = find_notif(&rebase_notifs, "DoSimClockRebase");
    assert_eq!(rb_args.len(), 1);
    match &rb_args[0] {
        PyRep::Tuple(times) => {
            assert_eq!(times.len(), 2);
            let old_t = times[0].as_i64().unwrap();
            let new_t = times[1].as_i64().unwrap();
            assert_eq!(old_t / 10_000_000, before_ft / 10_000_000);
            assert_eq!((new_t - old_t) / 10_000_000, 15);
        }
        other => panic!("expected 2-tuple rebaseTimes, got {other:?}"),
    }

    // 3. OnDestinationChange via live ESI HTTP POST /v2/ui/autopilot/waypoint/
    let esi = EsiRouter::new(Arc::clone(&ctx));
    let now_u = ctx.now_unix();
    let code = ctx
        .db
        .authorize_pkce(
            "demo",
            "demo-secret",
            PILOT_A,
            "evomu-batch10",
            "https://localhost/callback",
            vec!["esi-ui.write_waypoint.v1".to_string()],
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM",
            "S256",
            now_u,
        )
        .expect("authorize_pkce");
    let token = ctx
        .db
        .exchange_pkce_code(
            &code,
            "evomu-batch10",
            "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk",
            now_u,
        )
        .expect("exchange_pkce_code")
        .access_token;
    // Negative: invalid destination_id rejected with 4xx and emits zero OnDestinationChange
    let bad_resp = esi.handle_request(
        &EsiHttpRequest::new(
            "POST",
            "/v2/ui/autopilot/waypoint/?destination_id=999999999&add_to_beginning=false&clear_other_waypoints=true",
        )
        .with_bearer_token(&token),
    );
    assert_ne!(bad_resp.status_code, 204);
    assert!(!has_notif(
        &drain_all(&ctx, &mut sess, PILOT_A),
        "OnDestinationChange"
    ));

    // Positive: valid solar system Amarr (30002187) returns 204 and emits OnDestinationChange(30002187, true, false)
    let ok_resp = esi.handle_request(
        &EsiHttpRequest::new(
            "POST",
            "/v2/ui/autopilot/waypoint/?destination_id=30002187&add_to_beginning=false&clear_other_waypoints=true",
        )
        .with_bearer_token(&token),
    );
    assert_eq!(ok_resp.status_code, 204);
    let wp_notifs = drain_all(&ctx, &mut sess, PILOT_A);
    assert_eq!(
        find_notif(&wp_notifs, "OnDestinationChange"),
        &[
            PyRep::Integer(30_002_187),
            PyRep::Bool(true),
            PyRep::Bool(false),
        ]
    );

    // 4. OnServerMapDataPush via live map.GetHistory([1, 24])
    // Negative: when killmails table is empty, map.GetHistory returns empty rowset and emits zero OnServerMapDataPush
    ctx.db.with_mut_snapshot(|snap| snap.killmails.clear());
    rpc(
        &ctx,
        &mut sess,
        "map",
        "GetHistory",
        &[PyRep::Integer(1), PyRep::Integer(24)],
    )
    .await
    .unwrap();
    assert!(!has_notif(
        &drain_all(&ctx, &mut sess, PILOT_A),
        "OnServerMapDataPush"
    ));

    // Positive: with recorded killmail in SYS_JITA, map.GetHistory([1, 24]) pushes TriColors + 5-tuples
    ctx.db.with_mut_snapshot(|snap| {
        snap.killmails.insert(
            1,
            KillmailRecord {
                kill_id: 1,
                victim_character_id: PILOT_B,
                attacker_character_id: PILOT_A,
                solar_system_id: SYS_JITA,
                ship_type_id: 587,
                total_value_isk: 1_500_000.0,
                killed_unix: now_u,
                victim_corp_id: 1_000_009,
                victim_alliance_id: None,
                attacker_corp_id: 1_000_009,
                attacker_alliance_id: None,
            },
        );
    });
    rpc(
        &ctx,
        &mut sess,
        "map",
        "GetHistory",
        &[PyRep::Integer(1), PyRep::Integer(24)],
    )
    .await
    .unwrap();
    let map_notifs = drain_all(&ctx, &mut sess, PILOT_A);
    let mdp = find_notif(&map_notifs, "OnServerMapDataPush");
    assert_eq!(mdp.len(), 2);
    match &mdp[0] {
        PyRep::List(colors) => assert_eq!(colors.len(), 3),
        other => panic!("expected 3 TriColor tuples, got {other:?}"),
    }
    match &mdp[1] {
        PyRep::List(rows) => {
            assert_eq!(rows.len(), 1);
            match &rows[0] {
                PyRep::Tuple(cols) => {
                    assert_eq!(cols.len(), 5);
                    assert_eq!(cols[0], PyRep::Integer(i64::from(SYS_JITA)));
                    assert_eq!(cols[1], PyRep::Integer(1));
                    assert_eq!(cols[2], PyRep::Integer(0));
                    assert_eq!(cols[3], PyRep::Real(1_500_000.0));
                }
                other => panic!("expected 5-tuple map row, got {other:?}"),
            }
        }
        other => panic!("expected List of 5-tuples, got {other:?}"),
    }

    // 5. OnSystemActivityUpdated via live industryManager.InstallJob
    // Negative: undocked pilot calling InstallJob fails with NotDocked and emits zero OnSystemActivityUpdated
    let bp_tid = *ctx
        .sde
        .blueprints
        .iter()
        .find(|(&id, bp)| bp.manufacturing_time_sec > 0 && ctx.sde.types.contains_key(&id))
        .map(|(id, _)| id)
        .expect("SDE blueprint");
    assert_eq!(
        rpc(
            &ctx,
            &mut sess,
            "industryManager",
            "InstallJob",
            &[PyRep::Integer(i64::from(bp_tid)), PyRep::Integer(3), PyRep::Integer(1)]
        )
        .await
        .unwrap_err()
        .code,
        "NotDocked"
    );
    assert!(!has_notif(
        &drain_all(&ctx, &mut sess, PILOT_A),
        "OnSystemActivityUpdated"
    ));

    // Positive: docked at Jita 4-4 (60003760) with owned BPO in Hangar & ISK -> InstallJob emits OnSystemActivityUpdated()
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
            ch.station_id = Some(60_003_760);
            ch.balance = IskCents::from_isk(100_000_000);
        }
        snap.items.insert(
            700_720,
            make_item(700_720, bp_tid, PILOT_A, 60_003_760, InventoryFlag::Hangar, 1, true),
        );
    });
    sess.station_id = Some(60_003_760);
    rpc(
        &ctx,
        &mut sess,
        "industryManager",
        "InstallJob",
        &[PyRep::keyval([
            ("blueprintID", PyRep::Integer(700_720)),
            ("blueprintTypeID", PyRep::Integer(i64::from(bp_tid))),
            ("activityID", PyRep::Integer(3)),
            ("runs", PyRep::Integer(1)),
            ("facilityID", PyRep::Integer(60_003_760)),
        ])],
    )
    .await
    .expect("InstallJob TE research");
    let ind_notifs = drain_all(&ctx, &mut sess, PILOT_A);
    assert_eq!(find_notif(&ind_notifs, "OnSystemActivityUpdated"), &[]);

    // 6. OnCharacterEnergyChanged via live CRAB beacon beyonce.CallComponentFromClient("linkWithShip", "InitiateLink")
    let null_sys = ctx
        .sde
        .solar_systems
        .iter()
        .find(|(_, s)| s.region_id == 10_000_003 && s.security_status <= 0.0)
        .map(|(&id, _)| id)
        .unwrap_or(30_000_201);
    let dread_tid = *ctx
        .sde
        .types
        .iter()
        .find(|(_, t)| t.category_id == 6 && t.group_id == 485 && t.published)
        .map(|(id, _)| id)
        .unwrap_or(&19_720);
    let crab_id = 700_750_i64;
    let dread_id = 700_751_i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
            ch.solar_system_id = null_sys;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = dread_id;
        }
        snap.items.insert(
            dread_id,
            make_item(dread_id, dread_tid, PILOT_A, i64::from(null_sys), InventoryFlag::None, 1, true),
        );
        snap.items.insert(
            crab_id,
            make_item(
                crab_id,
                eve_server::services::pve_crab_beacons::TYPE_CRAB_BEACON,
                PILOT_A,
                i64::from(null_sys),
                InventoryFlag::None,
                1,
                true,
            ),
        );
        snap.item_positions.insert(crab_id, [2_000.0, 0.0, 0.0]);
    });
    // Deduct 90 CEQ so PILOT_A has 10 CEQ (< 30 cost) for negative InsufficientEnergy test
    let _ = eve_server::services::character_energy::consume_character_energy(&ctx, PILOT_A, 90);
    sess.solar_system_id = Some(null_sys);
    sess.station_id = None;
    sess.ship_id = Some(dread_id);
    let null_handle = ctx.systems.get_or_spawn(null_sys);
    let _ = null_handle
        .spawn_ball_with_combat(
            Ball {
                id: dread_id,
                type_id: dread_tid,
                position: DVec3::new(1_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 100.0,
                inertia: 1.0,
                mass: 1_000_000_000.0,
                radius: 1_200.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            Default::default(),
        )
        .await;
    let _ = null_handle
        .spawn_ball_with_combat(
            Ball {
                id: crab_id,
                type_id: eve_server::services::pve_crab_beacons::TYPE_CRAB_BEACON,
                position: DVec3::new(2_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 1.0,
                mass: 50_000.0,
                radius: 400.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            Default::default(),
        )
        .await;
    let _ = drain_all(&ctx, &mut sess, PILOT_A);

    // Negative: only 10 CEQ (< 30 required) -> InitiateLink rejected and zero OnCharacterEnergyChanged
    assert_eq!(
        rpc(
            &ctx,
            &mut sess,
            "beyonce",
            "CallComponentFromClient",
            &[
                PyRep::Integer(crab_id),
                PyRep::String("linkWithShip".into()),
                PyRep::String("InitiateLink".into()),
            ]
        )
        .await
        .unwrap_err()
        .code,
        "InsufficientEnergy"
    );
    assert!(!has_notif(
        &drain_all(&ctx, &mut sess, PILOT_A),
        "OnCharacterEnergyChanged"
    ));

    // Positive: advance 25 hours so CEQ regenerates to 100, then InitiateLink deducts 30 -> 70 CEQ
    eve_server::services::pve_special_sites::with_pve_special_reg(&ctx, |reg| {
        if let Some(b) = reg.crab_beacons.get_mut(&crab_id) {
            b.deployed_at_sec = 0;
        }
    });
    ctx.advance_secs(90_000);
    let _ = rpc(&ctx, &mut sess, "characterEnergyMgr", "GetMyEnergyState", &[]).await.unwrap();
    let _ = drain_all(&ctx, &mut sess, PILOT_A);
    let crab_res = rpc(
        &ctx,
        &mut sess,
        "beyonce",
        "CallComponentFromClient",
        &[
            PyRep::Integer(crab_id),
            PyRep::String("linkWithShip".into()),
            PyRep::String("InitiateLink".into()),
        ],
    )
    .await
    .expect("InitiateLink CRAB beacon");
    assert_eq!(
        crab_res.dict_get("ceqRemaining").and_then(|v| v.as_i64()),
        Some(70)
    );
    let ceq_notifs = drain_all(&ctx, &mut sess, PILOT_A);
    let ceq_args = find_notif(&ceq_notifs, "OnCharacterEnergyChanged");
    assert_eq!(ceq_args.len(), 2);
    assert_eq!(ceq_args[0], PyRep::Integer(PILOT_A));
    assert_eq!(
        ceq_args[1]
            .dict_get("currentEnergy")
            .and_then(|v| v.as_i64()),
        Some(70)
    );
    assert_eq!(
        ceq_args[1].dict_get("maxEnergy").and_then(|v| v.as_i64()),
        Some(100)
    );

    // 7. OnAnalysisKreditsChange via live ProjectDiscovery.post_classification
    let task_rep = rpc(&ctx, &mut sess, "ProjectDiscovery", "get_new_task", &[])
        .await
        .unwrap();
    assert!(!has_notif(
        &drain_all(&ctx, &mut sess, PILOT_A),
        "OnAnalysisKreditsChange"
    ));

    ctx.db.with_mut_snapshot(|snap| {
        eve_server::services::project_discovery::configure_project_discovery_mmos(
            snap, 100.0, 50.0, 25.0, 5.0, 10,
        );
        eve_server::services::project_discovery::set_project_discovery_task_consensus(
            snap, 1, true,
        );
    });
    let task_id = task_rep
        .dict_get("task_id")
        .cloned()
        .unwrap_or(PyRep::Integer(1));
    rpc(
        &ctx,
        &mut sess,
        "ProjectDiscovery",
        "post_classification",
        &[task_id, PyRep::List(vec![PyRep::Integer(1)])],
    )
    .await
    .unwrap();
    let pd_notifs = drain_all(&ctx, &mut sess, PILOT_A);
    assert_eq!(find_notif(&pd_notifs, "OnAnalysisKreditsChange"), &[]);
}
