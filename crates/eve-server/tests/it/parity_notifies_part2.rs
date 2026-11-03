//! Runtime integration tests for open `notify:*` parity rows (Part 2).

#![forbid(unsafe_code)]

use crate::common;
use eve_db::{AsteroidRecord, InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::{service_roles::ROLE_GM_DEFAULT, SessionState};
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use eve_server::system_actor::AsteroidState;
use glam::DVec3;
use std::collections::HashMap;

const PILOT_A: i64 = 90_000_001;

fn new_ctx() -> ServerContext {
    let sde = std::sync::Arc::new(
        eve_sde::SdeRegistry::open(&eve_sde::SdeRegistry::default_db_path()).expect("open sde"),
    );
    let db = std::sync::Arc::new(eve_db::WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(sde, db);
    ctx.set_clock_paused(true);
    ctx
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

fn drain_all(ctx: &ServerContext, sess: &mut SessionState, char_id: i64) -> Vec<(String, Vec<PyRep>)> {
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

async fn setup_pilot(ctx: &ServerContext, sid: u64) -> SessionState {
    let mut sess = SessionState::new(sid);
    common::login_pilot(ctx, &mut sess, "demo", "demo-secret").await;
    rpc(
        ctx,
        &mut sess,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(PILOT_A)],
    )
    .await
    .expect("SelectCharacterID");
    ctx.advance_secs(12);
    let _ = drain_all(ctx, &mut sess, PILOT_A);
    sess
}

#[tokio::test]
async fn test_parity_notifies_seasons_campaigns_and_skills() {
    // parity: notify:OnSeasonOmegaRewardsClaimed, notify:OnSeasonalGoalCompleted, notify:OnSeasonalGoalsReset
    eve_testkit::attest(&[
        "notify:OnSeasonOmegaRewardsClaimed",
        "notify:OnSeasonalGoalCompleted",
        "notify:OnSeasonalGoalsReset",
    ]);
    let ctx = new_ctx();
    let mut sess = setup_pilot(&ctx, 1201).await;
    sess.role = ROLE_GM_DEFAULT;

    let mut target_ch_id = 1_005_i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
            ch.clone_grade = 2;
        }
        if let Some(mut def) =
            eve_server::services::season_fsd::default_seasonal_challenge_definitions()
                .into_iter()
                .next()
        {
            target_ch_id = def.challenge_id;
            def.client_event_driven = true;
            def.event_conditions.clear();
            def.max_progress = 2;
            def.points = 20;
            snap.service_state_blobs.insert(
                eve_server::services::season_fsd::SEASONAL_CHALLENGES_CONFIG_KEY.into(),
                serde_json::to_string(&vec![def]).unwrap(),
            );
        }
    });
    let _ = rpc(&ctx, &mut sess, "seasonManager", "fill_up_and_return_challenges_for_character", &[])
        .await
        .expect("fill challenges");
    for _ in 0..2 {
        rpc(
            &ctx,
            &mut sess,
            "seasonManager",
            "process_client_event",
            &[PyRep::Integer(target_ch_id)],
        )
        .await
        .expect("complete challenge");
    }
    let _ = drain_all(&ctx, &mut sess, PILOT_A);
    let claim = rpc(&ctx, &mut sess, "seasonManager", "claim_challenge_rewards", &[PyRep::Integer(target_ch_id)])
        .await
        .expect("claim_challenge_rewards");
    assert_eq!(claim.dict_get("claimed").and_then(|v| v.as_bool()), Some(true));
    let claim_n = drain_all(&ctx, &mut sess, PILOT_A);
    assert_eq!(find_notif(&claim_n, "OnSeasonalGoalCompleted")[0], PyRep::Integer(0));
    assert_eq!(
        find_notif(&claim_n, "OnSeasonOmegaRewardsClaimed"),
        &[PyRep::List(vec![PyRep::Integer(0), PyRep::Integer(1)])]
    );

    rpc(&ctx, &mut sess, "slash", "SlashCmd", &[PyRep::String(format!("/season reset_goals {PILOT_A}"))])
        .await
        .expect("/season reset_goals");
    let reset_n = drain_all(&ctx, &mut sess, PILOT_A);
    assert_eq!(find_notif(&reset_n, "OnSeasonalGoalsReset").len(), 0);
}

#[tokio::test]
async fn test_parity_notifies_plex_dogma_and_brain() {
    // parity: notify:OnEffectMessage, notify:OnPlexDepositFailed, notify:OnPlexWithdrawFailed
    eve_testkit::attest(&[
        "notify:OnEffectMessage",
        "notify:OnPlexDepositFailed",
        "notify:OnPlexWithdrawFailed",
    ]);
    let ctx = new_ctx();
    let mut sess = setup_pilot(&ctx, 1202).await;

    // 1. OnPlexDepositFailed via invbroker.DepositPlexToVault with reference
    let jita_station = 60_003_760_i64;
    let trit_id = 880_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
            ch.station_id = Some(jita_station);
        }
        snap.adjust_plex_vault(PILOT_A, 50);
        snap.items.insert(
            trit_id,
            InventoryItemRecord {
                item_id: trit_id,
                type_id: 34,
                owner_id: PILOT_A,
                location_id: jita_station,
                flag: InventoryFlag::Hangar,
                quantity: 100,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });
    sess.station_id = Some(jita_station);
    let _ = drain_all(&ctx, &mut sess, PILOT_A);
    assert!(rpc(
        &ctx,
        &mut sess,
        "invbroker",
        "DepositPlexToVault",
        &[
            PyRep::Integer(jita_station),
            PyRep::Integer(trit_id),
            PyRep::Integer(10),
            PyRep::Integer(4242),
        ],
    )
    .await
    .is_err());
    let dep_n = drain_all(&ctx, &mut sess, PILOT_A);
    assert_eq!(
        find_notif(&dep_n, "OnPlexDepositFailed"),
        &[PyRep::String("NotPlex".into()), PyRep::Integer(4242)]
    );

    // 2. OnPlexWithdrawFailed via invbroker.WithdrawPlexFromVault with reference
    let mut wd_kwargs = HashMap::new();
    wd_kwargs.insert("reference".to_string(), PyRep::Integer(5353));
    assert!(ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            None,
            "WithdrawPlexFromVault",
            &[PyRep::Integer(99_999)],
            &wd_kwargs,
            None,
        )
        .await
        .is_err());
    let wd_n = drain_all(&ctx, &mut sess, PILOT_A);
    assert_eq!(
        find_notif(&wd_n, "OnPlexWithdrawFailed"),
        &[PyRep::String("NotEnoughPlex".into()), PyRep::Integer(5353)]
    );

    // 3. OnEffectMessage via mining_cycle::process_mining_cycles when capacitor runs dry
    let sys_jita = 30_000_142_u32;
    let miner_id = 880_011_i64;
    let rock_id = 880_012_i64;
    let miner_type = 483_u32;
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(PILOT_A).or_default().insert(
            3386,
            eve_db::TrainedSkillRecord {
                type_id: 3386,
                level: 5,
                skill_points: 256_000.0,
            },
        );
        let active_ship = snap.characters.get(&PILOT_A).map(|c| c.active_ship_id).unwrap_or(0);
        if let Some(sh) = snap.items.get_mut(&active_ship) {
            sh.type_id = 32880;
        }
        snap.items.retain(|_, i| i.location_id != active_ship);
        snap.module_online_states.insert(miner_id, true);
        snap.items.insert(
            miner_id,
            InventoryItemRecord {
                item_id: miner_id,
                type_id: miner_type,
                owner_id: PILOT_A,
                location_id: active_ship,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    let undock = rpc(&ctx, &mut sess, "stationSvc", "Undock", &[])
        .await
        .expect("Undock");
    let ship_id = undock.dict_get("shipID").and_then(|v| v.as_i64()).expect("shipID");
    ctx.invalidate_ship_fitting(ship_id);
    let beyonce_id = undock
        .dict_get("beyonceBoundID")
        .and_then(|v| v.as_i64())
        .map(|v| v as u64);
    ctx.dispatch_rpc(&mut sess, "beyonce", beyonce_id, "CmdStop", &[], &HashMap::new(), None)
        .await
        .expect("CmdStop");

    let sys = ctx.systems.get_or_spawn(sys_jita);
    let (_, ship_ball) = sys.get_ball_snapshot(ship_id).await.unwrap().expect("ship ball");
    let rock_pos = ship_ball.position + DVec3::new(2_000.0, 0.0, 0.0);
    ctx.db.spawn_asteroid(AsteroidRecord {
        asteroid_id: rock_id,
        solar_system_id: sys_jita,
        belt_id: 40_000_001,
        type_id: 1230,
        quantity: 10_000,
        initial_quantity: 10_000,
        position: (rock_pos.x, rock_pos.y, rock_pos.z),
        radius: 100.0,
    });
    let _ = sys
        .spawn_asteroid(
            AsteroidState {
                asteroid_id: rock_id,
                belt_id: 40_000_001,
                type_id: 1230,
                initial_units: 10_000,
                remaining_units: 10_000,
                unit_volume: 0.1,
                depleted: false,
                respawn_at_tick: None,
            },
            Ball {
                id: rock_id,
                type_id: 1230,
                position: rock_pos,
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 0.0,
                mass: 1.0e10,
                radius: 100.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
        )
        .await;
    rpc(&ctx, &mut sess, "dogmaIM", "AddTarget", &[PyRep::Integer(rock_id)])
        .await
        .expect("AddTarget");
    for _ in 0..30 {
        if sys
            .get_target_locks(ship_id)
            .await
            .unwrap()
            .iter()
            .any(|l| l.target_id == rock_id && l.locked)
        {
            break;
        }
        sys.step_manual(1.0).await.unwrap();
    }
    rpc(
        &ctx,
        &mut sess,
        "dogmaIM",
        "Activate",
        &[
            PyRep::Integer(miner_id),
            PyRep::String("miningLaser".into()),
            PyRep::Integer(rock_id),
        ],
    )
    .await
    .expect("Activate Miner I");
    let _ = drain_all(&ctx, &mut sess, PILOT_A);
    ctx.advance_secs(65);
    let mut cs = ctx.build_ship_combat_state(PILOT_A, ship_id, 32880);
    cs.max_capacitor = 0.0;
    cs.capacitor = 0.0;
    let _ = sys.set_combat_state(ship_id, cs).await;
    let _ = eve_server::services::mining_cycle::process_mining_cycles(&ctx).await;
    let eff_n = drain_all(&ctx, &mut sess, PILOT_A);
    assert_eq!(
        find_notif(&eff_n, "OnEffectMessage")[0],
        PyRep::String("EffectDeactivatedOutOfCap".into())
    );
}
