//! Real production server-to-client `notify:On*` emission integration tests (Batch 10, Part 3).
//!
//! Covers Progression & Tutorial Conversations (`operationsManager`), Sansha Incursion Tales
//! (`OnTaleData`, `OnInfluenceUpdate`, `OnTaleRemove`), and Roaming Metaliminal Storms
//! (`OnCosmicAnomalyAdded`, `OnSystemEffectStarted`, `OnSystemWideEffectStop`,
//! `OnComponentRemovedSystemEnvironment`) via live RPCs and server clock advancement.

#![forbid(unsafe_code)]

use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::{service_roles::ROLE_GM_DEFAULT, SessionState};
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use glam::DVec3;
use std::collections::HashMap;

const PILOT_A: i64 = 90_000_001;
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
    gm: bool,
) -> SessionState {
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = SYS_JITA;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = ship_id;
        }
        snap.items.insert(
            ship_id,
            make_item(
                ship_id,
                587,
                char_id,
                i64::from(SYS_JITA),
                InventoryFlag::None,
                1,
                true,
            ),
        );
    });
    let sys = ctx.systems.get_or_spawn(SYS_JITA);
    let _ = sys
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: 587,
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
    sess.solar_system_id = Some(SYS_JITA);
    sess.station_id = None;
    sess.structure_id = None;
    sess.ship_id = Some(ship_id);
    if gm {
        sess.role |= ROLE_GM_DEFAULT;
    }
    let _ = drain_all(ctx, &mut sess, char_id);
    sess
}

/// parity: notify:OnProgressionLeft
#[tokio::test]
async fn batch10_progression_and_conversations() {
    let ctx = ServerContext::new();
    let mut sess = setup_pilot_in_space(&ctx, PILOT_A, 700_401, false).await;

    // 1. Start an operation site via operationsManager.start_site (sets active_site_id)
    assert!(
        rpc(
            &ctx,
            &mut sess,
            "operationsManager",
            "start_site",
            &[PyRep::Integer(999_999_999)]
        )
        .await
        .is_err()
    );

    let valid_did = i64::from(*ctx.sde.dungeons.keys().min().expect("SDE dungeon"));
    rpc(
        &ctx,
        &mut sess,
        "operationsManager",
        "start_site",
        &[PyRep::Integer(valid_did)],
    )
    .await
    .unwrap();
    let _ = drain_all(&ctx, &mut sess, PILOT_A);

    // 2. Positive: skip_tutorial while tutorial/site is active emits OnProgressionLeft() (0 args)
    rpc(
        &ctx,
        &mut sess,
        "operationsManager",
        "process_client_event",
        &[PyRep::String("skip_tutorial".into())],
    )
    .await
    .unwrap();
    let left_notifs = drain_all(&ctx, &mut sess, PILOT_A);
    assert_eq!(find_notif(&left_notifs, "OnProgressionLeft"), &[]);

    // 3. Negative: calling skip_tutorial again when no tutorial/site is active emits zero OnProgressionLeft
    rpc(
        &ctx,
        &mut sess,
        "operationsManager",
        "process_client_event",
        &[PyRep::String("skip_tutorial".into())],
    )
    .await
    .unwrap();
    let repeat_notifs = drain_all(&ctx, &mut sess, PILOT_A);
    assert_eq!(
        repeat_notifs
            .iter()
            .filter(|(n, _)| n == "OnProgressionLeft")
            .count(),
        0
    );
}

/// parity: notify:OnTaleData
/// parity: notify:OnInfluenceUpdate
/// parity: notify:OnTaleRemove
/// parity: notify:OnCosmicAnomalyAdded
/// parity: notify:OnSystemEffectStarted
/// parity: notify:OnSystemWideEffectStop
/// parity: notify:OnComponentRemovedSystemEnvironment
#[tokio::test]
async fn batch10_tales_storms_and_cosmic_anomalies() {
    let ctx = ServerContext::new();
    let mut sess = setup_pilot_in_space(&ctx, PILOT_A, 700_410, false).await;

    // 1. Seed world event tales via live taleMgr.GetGlobalWorldEventTales RPC
    rpc(&ctx, &mut sess, "taleMgr", "GetGlobalWorldEventTales", &[])
        .await
        .unwrap();
    let (inc_cid, tale_id, staging_sys, vg_site_sys, vg_site_id, vg_boss_bid) =
        eve_server::services::pve_special_sites::with_pve_special_reg(&ctx, |reg| {
            let inc = reg
                .incursions
                .values_mut()
                .find(|i| i.state != "Ended" && i.staging_solar_system_id != SYS_JITA)
                .expect("seeded incursion");
            let vg = inc
                .sites
                .iter_mut()
                .find(|s| !s.locked_until_low_influence)
                .expect("vanguard/assault incursion site");
            for e in &mut vg.escort_spawns {
                e.destroyed = true;
            }
            vg.boss_destroyed = true;
            let sid = vg.site_id.clone();
            let bbid = vg.boss_npc_ball_id;
            let ssys = vg.solar_system_id;
            let icid = inc.constellation_id;
            let tid = inc.tale_id;
            let stsys = inc.staging_solar_system_id;
            if let Some(w) = reg.incursion_site_waves.get_mut(&sid) {
                w.current_wave = w.total_waves;
            }
            (icid, tid, stsys, ssys, sid, bbid)
        });
    let _ = ctx.systems.get_or_spawn(vg_site_sys).remove_ball(vg_boss_bid).await;
    let _ = drain_all(&ctx, &mut sess, PILOT_A);

    // Negative: UpdateStateRequest in non-incursion system (SYS_JITA) emits zero OnTaleData
    rpc(&ctx, &mut sess, "beyonce", "UpdateStateRequest", &[])
        .await
        .unwrap();
    assert!(!has_notif(
        &drain_all(&ctx, &mut sess, PILOT_A),
        "OnTaleData"
    ));

    // Positive: pilot enters incursion staging system and calls beyonce.UpdateStateRequest -> OnTaleData
    let _ = ctx
        .db
        .set_character_solar_system(PILOT_A, staging_sys, &ctx.sde);
    sess.solar_system_id = Some(staging_sys);
    ctx.sync_session(&mut sess);
    let _ = ctx
        .systems
        .get_or_spawn(staging_sys)
        .spawn_ball_with_combat(
            Ball {
                id: 700_410,
                type_id: 587,
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
    rpc(&ctx, &mut sess, "beyonce", "UpdateStateRequest", &[])
        .await
        .unwrap();
    let td_notifs = drain_all(&ctx, &mut sess, PILOT_A);
    let td_args = find_notif(&td_notifs, "OnTaleData");
    assert_eq!(td_args.len(), 2);
    assert_eq!(td_args[0], PyRep::Integer(i64::from(staging_sys)));
    match &td_args[1] {
        PyRep::Dict(entries) => {
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0].0, PyRep::Integer(tale_id));
            assert_eq!(
                entries[0].1.dict_get("taleID").and_then(|v| v.as_i64()),
                Some(tale_id)
            );
        }
        other => panic!("expected Dict of active tales, got {other:?}"),
    }

    // 2. OnInfluenceUpdate via live keeper.EnterNextDungeonRoom on an incursion site
    let _ = ctx
        .db
        .set_character_solar_system(PILOT_A, vg_site_sys, &ctx.sde);
    sess.solar_system_id = Some(vg_site_sys);
    ctx.sync_session(&mut sess);
    let _ = drain_all(&ctx, &mut sess, PILOT_A);

    // Negative: invalid incursion site ID fails and emits zero OnInfluenceUpdate
    assert!(
        rpc(
            &ctx,
            &mut sess,
            "keeper",
            "EnterNextDungeonRoom",
            &[PyRep::String("INC-NONEXISTENT".into())]
        )
        .await
        .is_err()
    );
    assert!(!has_notif(
        &drain_all(&ctx, &mut sess, PILOT_A),
        "OnInfluenceUpdate"
    ));

    // Positive: completing real vanguard site reduces influence and emits OnInfluenceUpdate
    rpc(
        &ctx,
        &mut sess,
        "keeper",
        "EnterNextDungeonRoom",
        &[PyRep::String(vg_site_id)],
    )
    .await
    .unwrap();
    let inf_notifs = drain_all(&ctx, &mut sess, PILOT_A);
    let inf_args = find_notif(&inf_notifs, "OnInfluenceUpdate");
    assert_eq!(inf_args.len(), 2);
    assert_eq!(inf_args[0], PyRep::Integer(tale_id));
    let new_inf = inf_args[1]
        .dict_get("influence")
        .and_then(|v| v.as_f64())
        .expect("influence float");
    assert!((0.0..1.0).contains(&new_inf));
    assert!(
        inf_args[1]
            .dict_get("lastUpdated")
            .and_then(|v| v.as_i64())
            .unwrap_or(0)
            > 0
    );
    assert!(!has_notif(&inf_notifs, "OnTaleRemove"));

    // 3. OnTaleRemove when incursion withdrawal expires on server clock advancement
    let now_u = ctx.now_unix();
    eve_server::services::pve_special_sites::with_pve_special_reg(&ctx, |reg| {
        if let Some(inc) = reg.incursions.get_mut(&inc_cid) {
            inc.state = "Withdrawing".into();
        }
        if let Some(meta) = reg.incursion_lifecycle.get_mut(&inc_cid) {
            meta.withdraw_until_sec = Some(now_u + 30);
        }
    });
    // Negative: advancing 5s (before withdraw_until_sec) does NOT emit OnTaleRemove
    ctx.clock().advance_secs(5);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    assert!(!has_notif(
        &drain_all(&ctx, &mut sess, PILOT_A),
        "OnTaleRemove"
    ));

    // Positive: advancing 35s crosses withdrawal expiry -> ends incursion and emits OnTaleRemove
    ctx.clock().advance_secs(35);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    let rem_notifs = drain_all(&ctx, &mut sess, PILOT_A);
    let tr_args = find_notif(&rem_notifs, "OnTaleRemove");
    assert_eq!(
        tr_args,
        &[
            PyRep::Integer(tale_id),
            PyRep::Integer(1),
            PyRep::Integer(
                eve_server::services::pve_incursion_sites::INCURSION_TALE_TEMPLATE_ID as i64
            ),
        ]
    );

    // 4. Roaming Metaliminal Storms via live tick_background_cluster -> on_server_clock_advanced
    // Position storm #0 on a neighbor of SYS_JITA so its next step enters SYS_JITA, then moves onward.
    let storm_dest: u32 = 30_000_208;
    let now2 = ctx.now_unix();
    eve_server::services::pve_special_sites::with_pve_special_reg(&ctx, |reg| {
        reg.roaming_storms.truncate(1);
        let s = &mut reg.roaming_storms[0];
        s.center_solar_system_id = storm_dest;
        s.strong_core_systems.clear();
        s.weak_periphery_systems.clear();
        s.recent_centers.clear();
        s.next_move_at_sec = now2 + 60;
    });
    eve_server::services::pve_metaliminal_storms::sync_metaliminal_storm_effects_and_sites(&ctx);
    let _ = ctx
        .db
        .set_character_solar_system(PILOT_A, storm_dest, &ctx.sde);
    sess.solar_system_id = Some(storm_dest);
    ctx.sync_session(&mut sess);
    let _ = drain_all(&ctx, &mut sess, PILOT_A);

    // Negative: advancing 10s (before next_move_at_sec) does NOT move storm or emit storm notifications
    ctx.clock().advance_secs(10);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    let pre_storm = drain_all(&ctx, &mut sess, PILOT_A);
    assert!(!has_notif(&pre_storm, "OnSystemEffectStarted"));
    assert!(!has_notif(&pre_storm, "OnCosmicAnomalyAdded"));

    // Positive: advancing 60s moves storm into neighbor so storm_dest enters strong core -> emits OnSystemEffectStarted + OnCosmicAnomalyAdded
    ctx.clock().advance_secs(60);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    let storm_in = drain_all(&ctx, &mut sess, PILOT_A);
    let eff_start = find_notif(&storm_in, "OnSystemEffectStarted");
    assert_eq!(eff_start.len(), 1);
    let beacon_tid = match &eff_start[0] {
        PyRep::List(ids) => {
            assert_eq!(ids.len(), 1);
            ids[0].as_i64().expect("beacon typeID")
        }
        other => panic!("expected List([beacon_type_id]), got {other:?}"),
    };
    assert!(beacon_tid > 0);
    let anom_added = find_notif(&storm_in, "OnCosmicAnomalyAdded");
    assert_eq!(anom_added.len(), 1);
    assert!(
        anom_added[0]
            .dict_get("targetID")
            .and_then(|v| v.as_str())
            .is_some_and(|s| s.starts_with("DNG-904"))
    );

    // Move storm to a distant null-sec region on its next scheduled jump so storm_dest leaves core + periphery
    let far_sys: u32 = 30_000_238;
    let now3 = ctx.now_unix();
    eve_server::services::pve_special_sites::with_pve_special_reg(&ctx, |reg| {
        let s = &mut reg.roaming_storms[0];
        s.center_solar_system_id = far_sys;
        s.recent_centers.clear();
        s.next_move_at_sec = now3 + 60;
    });
    // Negative: 10s before scheduled jump -> storm effect still active
    ctx.clock().advance_secs(10);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    assert!(!has_notif(
        &drain_all(&ctx, &mut sess, PILOT_A),
        "OnSystemWideEffectStop"
    ));

    // Positive: 60s advances storm to far_sys -> removes beacon & environment from storm_dest
    ctx.clock().advance_secs(60);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    let storm_out = drain_all(&ctx, &mut sess, PILOT_A);
    let stop_args = find_notif(&storm_out, "OnSystemWideEffectStop");
    assert_eq!(stop_args.len(), 3);
    assert!(stop_args[0].as_i64().unwrap_or(0) > 0);
    assert!(stop_args[1].as_i64().unwrap_or(0) > 0);
    assert_eq!(stop_args[2], PyRep::Integer(beacon_tid));
    assert_eq!(
        find_notif(&storm_out, "OnComponentRemovedSystemEnvironment"),
        &[PyRep::Integer(i64::from(storm_dest))]
    );
}
