//! Exhaustive unit tests for the declarative `Transition` teardown registry
//! (`#1190`, `#848`, `#428`, `#1122`, `#559`, `#78`, `#206`, `#881`).

#![forbid(unsafe_code)]

use std::collections::HashSet;
use std::sync::{Arc, OnceLock};

use eve_db::{
    AccountRecord, CharacterRecord, InventoryFlag, InventoryItemRecord, IskCents,
    NeuralAttributes, OwnerNoteRecord, SafeLogoffEntry, WorldRepository,
};
use eve_machonet::SessionState;
use eve_sde::SdeRegistry;

use super::*;
use crate::router::cache_notifications::PendingMachoNotification;
use crate::router::session_transition::apply_session_from_character;
use crate::router::{ServerContext, SessionTransitionSpec, TransitionKind};
use crate::services::notify_emitters_tournaments_anomalies::collect_active_system_wide_effects_in_system;
use crate::services::wallet_services_ext::TradeSessionState;

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn test_ctx() -> ServerContext {
    ServerContext::with_sde_and_repository(shared_sde(), Arc::new(WorldRepository::new_empty()))
}

fn jita_station_and_sys(ctx: &ServerContext) -> (u32, i64) {
    let station_id = *ctx
        .sde
        .stations
        .keys()
        .find(|&&id| id == 60_003_760)
        .unwrap_or_else(|| ctx.sde.stations.keys().next().expect("sde station"));
    (ctx.sde.stations[&station_id].solar_system_id, station_id)
}

fn other_kspace_sys(ctx: &ServerContext, exclude_sys: u32) -> u32 {
    ctx.sde
        .solar_systems
        .keys()
        .copied()
        .find(|&id| id != exclude_sys && id < 31_000_000)
        .expect("second SDE k-space solar system")
}

fn pick_wh_system_with_effect(ctx: &ServerContext) -> u32 {
    ctx.sde
        .solar_systems
        .keys()
        .copied()
        .find(|&sid| !collect_active_system_wide_effects_in_system(ctx, Some(sid), true).is_empty())
        .expect("at least one SDE wormhole system with info-bubble effect")
}

fn seed_item(
    ctx: &ServerContext,
    item_id: i64,
    type_id: u32,
    owner_id: i64,
    location_id: i64,
    flag: InventoryFlag,
    quantity: i32,
    singleton: bool,
) {
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            item_id,
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
            },
        );
    });
}

fn seed_safe_logoff(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    solar_system_id: u32,
    now: u64,
    completed: bool,
) {
    ctx.db.with_mut_snapshot(|snap| {
        snap.sp6_lifecycle.as_inner_mut().safe_logoffs.insert(
            char_id,
            SafeLogoffEntry {
                char_id,
                ship_id,
                solar_system_id,
                started_unix: now.saturating_sub(30),
                complete_at_unix: now,
                completed,
            },
        );
    });
}

fn seed_pilot(
    ctx: &ServerContext,
    char_id: i64,
    user_id: u64,
    ship_id: i64,
    ship_type_id: u32,
    solar_system_id: u32,
    station_id: Option<i64>,
) -> SessionState {
    let (const_id, reg_id) = ctx
        .sde
        .solar_systems
        .get(&solar_system_id)
        .map(|s| (s.constellation_id, s.region_id))
        .unwrap_or((20_000_020, 10_000_002));
    let home_station_id = station_id.unwrap_or_else(|| jita_station_and_sys(ctx).1);
    let (item_loc, item_flag) = match station_id {
        Some(st) => (st, InventoryFlag::Hangar),
        None => (i64::from(solar_system_id), InventoryFlag::None),
    };
    ctx.db.with_mut_snapshot(|snap| {
        snap.accounts.entry(format!("u_{user_id}")).or_insert_with(|| AccountRecord {
            user_id,
            username: format!("u_{user_id}"),
            password_token: "test".into(),
            role: eve_db::service_roles::ROLE_PLAYER,
            banned_until_unix: None,
            ban_reason: None,
        });
        snap.characters.insert(
            char_id,
            CharacterRecord {
                activity_status: None,
                character_id: char_id,
                user_id,
                name: format!("Pilot {char_id}"),
                corporation_id: 1_000_009,
                alliance_id: None,
                war_faction_id: None,
                solar_system_id,
                constellation_id: const_id,
                region_id: reg_id,
                station_id,
                structure_id: None,
                home_station_id,
                active_ship_id: ship_id,
                balance: IskCents::from_isk(1_000_000),
                plex_balance: 0,
                loyalty_points: Default::default(),
                standings: Default::default(),
                security_status: 0.0,
                clone_grade: 1,
                race_id: 1,
                bloodline_id: 1,
                ancestry_id: 1,
                school_id: 1,
                neural: NeuralAttributes {
                    intelligence: 20, memory: 20, perception: 20, willpower: 20,
                    charisma: 19, bonus_remaps: 0, last_remap_unix: None,
                    remap_cooldown_until_unix: None,
                },
                skill_points: 0.0,
                free_skill_points: 0.0,
                last_clone_jump_unix: 0,
                jump_fatigue_minutes: 0.0,
                jump_cooldown_until_unix: 0,
                jump_fatigue_until_unix: 0,
                description: String::new(),
                title: String::new(),
                gender: 1,
                create_date_unix: 0,
                fleet_id: None,
                autopilot_waypoints: Vec::new(),
                version: 1,
                system_epoch: 1,
            },
        );
    });
    seed_item(ctx, ship_id, ship_type_id, char_id, item_loc, item_flag, 1, true);
    let mut sess = SessionState::new(user_id);
    sess.user_id = Some(user_id);
    sess.character_id = Some(char_id);
    let ch = ctx.db.get_character(char_id).unwrap();
    apply_session_from_character(&mut sess, &ch, None, Some(&ctx.sde));
    ctx.db
        .acquire_online_lock(user_id, sess.session_id, Some(char_id), true, ctx.now_unix())
        .expect("acquire online lock for seeded pilot");
    sess
}

fn seed_locked_direct_trade(
    ctx: &ServerContext,
    sess_a: &SessionState,
    char_a: i64,
    char_b: i64,
    trade_station_id: i64,
    item_station_id: i64,
    trade_id: i64,
    item_id: i64,
) -> u64 {
    let note_id = -(7_300_000_000_000_i64.saturating_add(trade_id.abs()));
    let json = serde_json::to_string(&TradeSessionState {
        trade_id,
        station_id: trade_station_id,
        char_a,
        char_b,
        items_a: vec![(item_id, 1)],
        items_b: Vec::new(),
        money_a_cents: 0,
        money_b_cents: 0,
        accept_a: false,
        accept_b: false,
        completed: false,
        cancelled: false,
    })
    .unwrap();
    seed_item(ctx, item_id, 34, char_a, item_station_id, InventoryFlag::Hangar, 10, false);
    ctx.db.with_mut_snapshot(|snap| {
        snap.owner_notes.insert(
            note_id,
            OwnerNoteRecord {
                note_id,
                character_id: -73,
                item_id: 0,
                label: "trade".into(),
                note: json,
                timestamp_unix: ctx.now_unix(),
            },
        );
    });
    ctx.bind_object(sess_a, "trademgr", trade_id)
        .expect("bind trademgr object")
}

fn drain_broadcast_rx(
    rx: &mut tokio::sync::broadcast::Receiver<(i64, PendingMachoNotification)>,
) -> Vec<(i64, PendingMachoNotification)> {
    let mut out = Vec::new();
    while let Ok(item) = rx.try_recv() {
        out.push(item);
    }
    out
}

fn all_transition_kinds(
    jita_sys: u32,
    other_sys: u32,
    jita_st: i64,
    target_ship: i64,
) -> Vec<(&'static str, TransitionKind)> {
    vec![
        ("dock", TransitionKind::Dock { dock_id: jita_st, is_structure: false }),
        ("undock", TransitionKind::Undock { solar_system_id: jita_sys }),
        ("jump", TransitionKind::JumpSystem { solar_system_id: other_sys }),
        ("board", TransitionKind::BoardShip { ship_id: target_ship }),
        ("eject", TransitionKind::EjectToPod { capsule_id: target_ship }),
        (
            "clone",
            TransitionKind::CloneJump {
                solar_system_id: jita_sys,
                dock_id: Some((jita_st, false)),
                ship_id: Some(target_ship),
            },
        ),
        (
            "death",
            TransitionKind::DeathRespawn {
                solar_system_id: jita_sys,
                dock_id: Some((jita_st, false)),
                ship_id: target_ship,
            },
        ),
        (
            "teleport",
            TransitionKind::Teleport {
                solar_system_id: other_sys,
                dock_id: None,
                ship_id: None,
            },
        ),
        (
            "login",
            TransitionKind::CharacterLogin {
                solar_system_id: jita_sys,
                dock_id: Some((jita_st, false)),
                ship_id: Some(target_ship),
            },
        ),
        ("logout", TransitionKind::Logout),
    ]
}

#[test]
fn teardown_registry_is_total() {
    let rows = all_teardown_policies();
    assert_eq!(rows.len(), TeardownSubsystem::ALL.len() * TransitionTrigger::ALL.len());

    let mut seen = HashSet::new();
    for row in &rows {
        assert!(seen.insert((row.subsystem, row.trigger)), "duplicate {:?}/{:?}", row.subsystem, row.trigger);
        assert!(!row.rationale.trim().is_empty(), "empty rationale {:?}", row.subsystem);
        assert_eq!(lookup_teardown_policy(row.subsystem, row.trigger), *row);
    }

    let mapped: HashSet<_> = all_transition_kinds(30_000_142, 30_002_187, 60_003_760, 1)
        .into_iter()
        .map(|(_, k)| TransitionTrigger::from_kind(k))
        .collect();
    assert_eq!(mapped.len(), 10);
    assert!(!mapped.contains(&TransitionTrigger::Disconnect));

    for trigger in TransitionTrigger::ALL {
        assert_eq!(
            declared_teardown_policy(TeardownSubsystem::TradeSessions, trigger).action,
            TeardownHookAction::CancelOpenTrades
        );
        assert!(matches!(
            declared_teardown_policy(TeardownSubsystem::SafeLogoffState, trigger).action,
            TeardownHookAction::CleanupOrAbortSafeLogoff { .. }
                | TeardownHookAction::ClearSafeLogoffSilent
        ));
    }
    assert_eq!(
        declared_teardown_policy(
            TeardownSubsystem::DisconnectPresenceAndControl,
            TransitionTrigger::Disconnect
        )
        .action,
        TeardownHookAction::RunDisconnectPresenceAndControlCleanup
    );
}

#[test]
fn trade_session_closed_on_every_transition_kind() {
    let ctx = test_ctx();
    let (jita_sys, jita_st) = jita_station_and_sys(&ctx);
    let other_sys = other_kspace_sys(&ctx, jita_sys);
    let mut next_trade_id = 710_001_i64;
    let mut next_item_id = 720_001_i64;

    for (label, kind) in all_transition_kinds(jita_sys, other_sys, jita_st, 880_002) {
        let modes = if matches!(kind, TransitionKind::CharacterLogin { .. } | TransitionKind::Logout) {
            vec![false]
        } else {
            vec![true, false]
        };

        for apply_db in modes {
            let char_a = 800_001_i64 + next_trade_id;
            let char_b = char_a + 50_000;
            let start_station = match kind {
                TransitionKind::Dock { .. } | TransitionKind::JumpSystem { .. } => None,
                _ => Some(jita_st),
            };
            let mut sess_a = seed_pilot(&ctx, char_a, (char_a as u64) + 1, char_a + 10, 587, jita_sys, start_station);
            seed_pilot(&ctx, char_b, (char_b as u64) + 1, char_b + 10, 587, jita_sys, Some(jita_st));
            let target_loc = start_station.unwrap_or(i64::from(jita_sys));
            let target_flag = if start_station.is_some() { InventoryFlag::Hangar } else { InventoryFlag::None };
            seed_item(&ctx, 880_002, 670, char_a, target_loc, target_flag, 1, true);

            next_trade_id += 1;
            next_item_id += 1;
            let trade_id = next_trade_id;
            let item_id = next_item_id;
            let trade_bid = seed_locked_direct_trade(
                &ctx,
                &sess_a,
                char_a,
                char_b,
                start_station.unwrap_or(0),
                jita_st,
                trade_id,
                item_id,
            );
            assert!(ctx.db.with_snapshot(|s| s.is_item_locked_excluding_trade(item_id, None)));
            ctx.drain_notifications_for_char(char_a);
            ctx.drain_notifications_for_char(char_b);

            if !apply_db && !matches!(kind, TransitionKind::CharacterLogin { .. } | TransitionKind::Logout) {
                ctx.db.with_mut_snapshot(|snap| {
                    crate::router::apply_character_transition_in_snap(
                        snap, &ctx.sde, char_a, kind.destination_location(), kind.target_ship_id(), true, false,
                    )
                    .expect("apply_character_transition_in_snap");
                });
            }

            let spec = SessionTransitionSpec::for_kind(char_a, kind, label).with_apply_db(apply_db);
            ctx.execute_session_transition_sync(Some(&mut sess_a), spec)
                .unwrap_or_else(|e| panic!("{label} (apply_db={apply_db}) failed: {e:?}"));

            assert!(!ctx.db.with_snapshot(|s| s.is_item_locked_excluding_trade(item_id, None)));
            assert!(ctx.verify_bound_object(&sess_a, trade_bid).is_err());
            assert!(ctx.drain_notifications_for_char(char_a).iter().any(|n| n.event == "OnTradeCancel"));
            assert!(ctx.drain_notifications_for_char(char_b).iter().any(|n| n.event == "OnTradeCancel"));
        }
    }

    // Disconnect (`logoff_character_sync`) closes open trades and broadcasts OnTradeCancel (#848).
    let (char_a, char_b) = (809_001_i64, 809_002_i64);
    let mut sess_a = seed_pilot(&ctx, char_a, 809_001, 809_101, 587, jita_sys, Some(jita_st));
    seed_pilot(&ctx, char_b, 809_002, 809_102, 587, jita_sys, Some(jita_st));
    let trade_bid = seed_locked_direct_trade(&ctx, &sess_a, char_a, char_b, jita_st, jita_st, 809_777, 809_888);
    ctx.drain_notifications_for_char(char_a);
    ctx.drain_notifications_for_char(char_b);
    let mut rx = ctx.notify_tx.subscribe();

    ctx.logoff_character_sync(char_a, Some(&mut sess_a));

    assert!(!ctx.db.with_snapshot(|s| s.is_item_locked_excluding_trade(809_888, None)));
    assert!(ctx.verify_bound_object(&sess_a, trade_bid).is_err());
    assert!(ctx.drain_notifications_for_char(char_a).is_empty());
    let live = drain_broadcast_rx(&mut rx);
    assert!(live.iter().any(|(cid, n)| *cid == char_a && n.event == "OnTradeCancel"));
    assert!(ctx.drain_notifications_for_char(char_b).iter().any(|n| n.event == "OnTradeCancel"));

    // Opt-out flag `.with_close_active_trades(false)` preserves open trade when requested.
    let (char_c, char_d) = (809_010_i64, 809_011_i64);
    let mut sess_c = seed_pilot(&ctx, char_c, 809_010, 809_110, 587, jita_sys, Some(jita_st));
    seed_pilot(&ctx, char_d, 809_011, 809_111, 587, jita_sys, Some(jita_st));
    let opt_bid = seed_locked_direct_trade(&ctx, &sess_c, char_c, char_d, jita_st, jita_st, 809_779, 809_889);
    ctx.drain_notifications_for_char(char_c);
    ctx.drain_notifications_for_char(char_d);
    ctx.execute_session_transition_sync(
        Some(&mut sess_c),
        SessionTransitionSpec::for_kind(char_c, TransitionKind::Undock { solar_system_id: jita_sys }, "undock")
            .with_close_active_trades(false),
    )
    .expect("undock opt-out succeeds");
    assert!(ctx.db.with_snapshot(|s| s.owner_notes.contains_key(&(-(7_300_000_000_000_i64 + 809_779)))));
    assert!(ctx.verify_bound_object(&sess_c, opt_bid).is_ok());
    assert!(!ctx.drain_notifications_for_char(char_c).iter().any(|n| n.event == "OnTradeCancel"));
    assert!(!ctx.drain_notifications_for_char(char_d).iter().any(|n| n.event == "OnTradeCancel"));
}

#[test]
fn safe_logoff_completed_and_active_cleared_across_transitions_and_disconnect() {
    let ctx = test_ctx();
    let (jita_sys, jita_st) = jita_station_and_sys(&ctx);
    let other_sys = other_kspace_sys(&ctx, jita_sys);
    let now = ctx.now_unix();

    for (idx, (label, kind)) in all_transition_kinds(jita_sys, other_sys, jita_st, 890_002)
        .into_iter()
        .enumerate()
    {
        for completed in [true, false] {
            let char_id = 810_000 + (idx as i64) * 10 + i64::from(completed);
            let ship_id = char_id + 1_000;
            let start_station = match kind {
                TransitionKind::Dock { .. } | TransitionKind::JumpSystem { .. } => None,
                _ => Some(jita_st),
            };
            let mut sess = seed_pilot(&ctx, char_id, char_id as u64, ship_id, 587, jita_sys, start_station);
            let target_loc = start_station.unwrap_or(i64::from(jita_sys));
            let target_flag = if start_station.is_some() { InventoryFlag::Hangar } else { InventoryFlag::None };
            seed_item(&ctx, 890_002, 670, char_id, target_loc, target_flag, 1, true);
            seed_safe_logoff(&ctx, char_id, ship_id, jita_sys, now, completed);
            ctx.drain_notifications_for_char(char_id);

            ctx.execute_session_transition_sync(
                Some(&mut sess),
                SessionTransitionSpec::for_kind(char_id, kind, label),
            )
            .unwrap_or_else(|e| panic!("{label} failed: {e:?}"));

            let still_present =
                crate::sp6_lifecycle::with_sp6_state(&ctx.db, |st| st.safe_logoffs.contains_key(&char_id));
            assert!(!still_present, "{label} (completed={completed}) must remove safe_logoffs entry");
            let aborted_emitted = ctx
                .drain_notifications_for_char(char_id)
                .iter()
                .any(|n| n.event == "OnSafeLogoffAborted");
            let is_silent = matches!(kind, TransitionKind::CharacterLogin { .. } | TransitionKind::Logout);
            assert_eq!(aborted_emitted, !completed && !is_silent, "{label} abort notification mismatch");
        }
    }

    // Disconnect silently removes completed safe-logoff state without OnSafeLogoffAborted (#428).
    let dc_char = 819_500_i64;
    let mut dc_sess = seed_pilot(&ctx, dc_char, 819_500, 819_600, 587, jita_sys, Some(jita_st));
    seed_safe_logoff(&ctx, dc_char, 819_600, jita_sys, now, true);
    let mut rx = ctx.notify_tx.subscribe();
    ctx.logoff_character_sync(dc_char, Some(&mut dc_sess));
    assert!(!crate::sp6_lifecycle::with_sp6_state(&ctx.db, |st| st.safe_logoffs.contains_key(&dc_char)));
    assert!(!drain_broadcast_rx(&mut rx)
        .iter()
        .any(|(cid, n)| *cid == dc_char && n.event == "OnSafeLogoffAborted"));

    // Opt-out `.with_clear_completed_safe_logoff(false)` preserves entry untouched.
    let keep_char = 819_601_i64;
    let mut keep_sess = seed_pilot(&ctx, keep_char, 819_601, 819_701, 587, jita_sys, Some(jita_st));
    seed_safe_logoff(&ctx, keep_char, 819_701, jita_sys, now, true);
    ctx.execute_session_transition_sync(
        Some(&mut keep_sess),
        SessionTransitionSpec::for_kind(keep_char, TransitionKind::Undock { solar_system_id: jita_sys }, "undock")
            .with_clear_completed_safe_logoff(false),
    )
    .expect("undock succeeds");
    assert!(crate::sp6_lifecycle::with_sp6_state(&ctx.db, |st| st.safe_logoffs.contains_key(&keep_char)));
}

#[test]
fn system_wide_effect_stop_emitted_on_every_exit() {
    let ctx = test_ctx();
    let (jita_sys, jita_st) = jita_station_and_sys(&ctx);
    let wh_sys = pick_wh_system_with_effect(&ctx);
    let expected_effects = collect_active_system_wide_effects_in_system(&ctx, Some(wh_sys), true);
    assert!(!expected_effects.is_empty());

    let wh_struct_id = 1_000_000_820_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            wh_struct_id,
            eve_db::UpwellStructureRecord {
                structure_id: wh_struct_id,
                type_id: 35_832,
                owner_corp_id: 98_000_001,
                solar_system_id: wh_sys,
                name: "WH Astrahus".into(),
                position: [1_000.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                ..Default::default()
            },
        );
    });

    // Bystander staying in WH space must NEVER receive char-scoped OnSystemWideEffectStop.
    let bystander = 820_999_i64;
    seed_pilot(&ctx, bystander, 820_999, 820_899, 587, wh_sys, None);
    ctx.drain_notifications_for_char(bystander);

    let exit_cases: Vec<(&'static str, TransitionKind, bool, bool)> = vec![
        ("dock_apply_db_false", TransitionKind::Dock { dock_id: wh_struct_id, is_structure: true }, false, false),
        ("jump_out", TransitionKind::JumpSystem { solar_system_id: jita_sys }, true, true),
        ("jump_out_silent", TransitionKind::JumpSystem { solar_system_id: jita_sys }, false, false),
        (
            "clone_jump_out",
            TransitionKind::CloneJump { solar_system_id: jita_sys, dock_id: Some((jita_st, false)), ship_id: Some(820_501) },
            true,
            true,
        ),
        (
            "death_respawn_out",
            TransitionKind::DeathRespawn { solar_system_id: jita_sys, dock_id: Some((jita_st, false)), ship_id: 820_502 },
            true,
            true,
        ),
        ("teleport_out", TransitionKind::Teleport { solar_system_id: jita_sys, dock_id: None, ship_id: None }, true, true),
        ("logout_from_wh", TransitionKind::Logout, false, false),
    ];

    for (idx, (label, kind, apply_db, emit_sess_changed)) in exit_cases.into_iter().enumerate() {
        let char_id = 820_000 + (idx as i64) * 10;
        let ship_id = char_id + 500;
        let mut sess = seed_pilot(&ctx, char_id, char_id as u64, ship_id, 587, wh_sys, None);
        if let TransitionKind::CloneJump { ship_id: Some(pod), .. } | TransitionKind::DeathRespawn { ship_id: pod, .. } = kind {
            seed_item(&ctx, pod, 670, char_id, jita_st, InventoryFlag::Hangar, 1, true);
        }
        if !apply_db && !matches!(kind, TransitionKind::Logout) {
            ctx.db.with_mut_snapshot(|snap| {
                crate::router::apply_character_transition_in_snap(
                    snap, &ctx.sde, char_id, kind.destination_location(), kind.target_ship_id(), true, false,
                )
                .unwrap();
            });
        }
        ctx.drain_notifications_for_char(char_id);
        ctx.drain_notifications_for_char(bystander);

        ctx.execute_session_transition_sync(
            Some(&mut sess),
            SessionTransitionSpec::for_kind(char_id, kind, label)
                .with_apply_db(apply_db)
                .with_emit_on_session_changed(emit_sess_changed),
        )
        .unwrap_or_else(|e| panic!("{label} failed: {e:?}"));

        let notifs = ctx.drain_notifications_for_char(char_id);
        let stops: Vec<_> = notifs.iter().filter(|n| n.event == "OnSystemWideEffectStop").collect();
        assert_eq!(stops.len(), expected_effects.len(), "{label} stop count mismatch");
        assert!(stops.iter().all(|sn| sn.id_type == "charid" && sn.args.len() == 3));
        assert!(notifs.iter().any(|n| n.event == "OnUpdateSystemWideEffectsInfo"));
        assert!(!ctx.drain_notifications_for_char(bystander).iter().any(|n| n.event == "OnSystemWideEffectStop"));
    }

    // Disconnect from WH space broadcasts char-scoped OnSystemWideEffectStop before purging queue.
    let dc_char = 820_777_i64;
    let mut dc_sess = seed_pilot(&ctx, dc_char, 820_777, 820_778, 587, wh_sys, None);
    ctx.drain_notifications_for_char(dc_char);
    let mut rx = ctx.notify_tx.subscribe();
    ctx.logoff_character_sync(dc_char, Some(&mut dc_sess));
    assert!(ctx.drain_notifications_for_char(dc_char).is_empty());
    let live = drain_broadcast_rx(&mut rx);
    assert!(live.iter().any(|(cid, n)| *cid == dc_char && n.event == "OnSystemWideEffectStop" && n.id_type == "charid"));
    assert!(!live.iter().any(|(cid, n)| *cid == bystander && n.event == "OnSystemWideEffectStop"));

    // Undocking inside WH must NOT emit OnSystemWideEffectStop.
    let undock_char = 820_888_i64;
    let mut undock_sess = seed_pilot(&ctx, undock_char, 820_888, 820_889, 587, wh_sys, Some(jita_st));
    ctx.drain_notifications_for_char(undock_char);
    ctx.execute_session_transition_sync(
        Some(&mut undock_sess),
        SessionTransitionSpec::for_kind(undock_char, TransitionKind::Undock { solar_system_id: wh_sys }, "undock")
            .with_validate_destination(false),
    )
    .expect("undock succeeds");
    assert!(!ctx.drain_notifications_for_char(undock_char).iter().any(|n| n.event == "OnSystemWideEffectStop"));
}

#[tokio::test]
async fn departing_space_hooks_cloak_concord_and_self_destruct_run_via_registry() {
    let ctx = test_ctx();
    let (jita_sys, jita_st) = jita_station_and_sys(&ctx);
    let lowsec_sys = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| s.security_status > 0.0 && s.security_status < 0.45 && s.solar_system_id < 31_000_000)
        .map(|s| s.solar_system_id)
        .expect("lowsec system");
    let now = ctx.now_unix();

    // 1. Self-destruct, cloak, warp-stopped timestamp, and ALSC cleanup clear on Dock.
    let (char_a, ship_a) = (830_001_i64, 830_101_i64);
    let mut sess_a = seed_pilot(&ctx, char_a, 830_001, ship_a, 587, jita_sys, None);
    crate::sp6_lifecycle::register_self_destruct_countdown(&ctx, ship_a, char_a, jita_sys, now, now + 120);
    crate::services::mobile_deployables_ext::record_ship_cloak_activated(&ctx, ship_a);
    let sys_handle = ctx.systems.get_or_spawn(jita_sys);
    sys_handle.warp_stopped_at.write().unwrap().insert(ship_a, tokio::time::Instant::now());
    crate::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| {
        rt.authorized_shipping_storage.insert((999_001, char_a));
    });

    ctx.execute_session_transition_sync(
        Some(&mut sess_a),
        SessionTransitionSpec::for_kind(char_a, TransitionKind::Dock { dock_id: jita_st, is_structure: false }, "dock"),
    )
    .expect("dock succeeds");

    assert!(!crate::sp6_lifecycle::with_sp6_state(&ctx.db, |st| st.self_destructs.contains_key(&ship_a)));
    assert!(!crate::services::combat_crimewatch::with_combat_runtime(&ctx, |rt| {
        rt.self_destruct_countdowns.contains_key(&ship_a)
            || rt.authorized_shipping_storage.contains(&(999_001, char_a))
    }));
    assert!(!crate::services::mobile_deployables::read_deployable_state(&ctx)
        .cloak_activated_unix
        .contains_key(&ship_a));
    assert!(!sys_handle.warp_stopped_at.read().unwrap().contains_key(&ship_a));

    // 2. EjectToPod aborts active self-destruct countdown on departed ship (#78).
    let (char_b, ship_b, pod_b) = (830_002_i64, 830_102_i64, 830_103_i64);
    let mut sess_b = seed_pilot(&ctx, char_b, 830_002, ship_b, 587, jita_sys, Some(jita_st));
    seed_item(&ctx, pod_b, 670, char_b, jita_st, InventoryFlag::Hangar, 1, true);
    crate::sp6_lifecycle::register_self_destruct_countdown(&ctx, ship_b, char_b, jita_sys, now, now + 120);
    ctx.execute_session_transition_sync(
        Some(&mut sess_b),
        SessionTransitionSpec::for_kind(char_b, TransitionKind::EjectToPod { capsule_id: pod_b }, "eject"),
    )
    .expect("eject succeeds");
    assert!(!crate::sp6_lifecycle::with_sp6_state(&ctx.db, |st| st.self_destructs.contains_key(&ship_b)));

    // 3. Criminal entering high-sec space via non-stargate Teleport schedules CONCORD (#559).
    let (crim_char, crim_ship) = (830_003_i64, 830_104_i64);
    let mut crim_sess = seed_pilot(&ctx, crim_char, 830_003, crim_ship, 587, lowsec_sys, None);
    ctx.crimewatch.write().unwrap().get_or_create(crim_char).criminal_flag_until_unix = now + 900;
    ctx.execute_session_transition_sync(
        Some(&mut crim_sess),
        SessionTransitionSpec::for_kind(
            crim_char,
            TransitionKind::Teleport { solar_system_id: jita_sys, dock_id: None, ship_id: None },
            "cyno_jump",
        ),
    )
    .expect("teleport to highsec succeeds");
    assert!(ctx.crimewatch.read().unwrap().states[&crim_char].pending_concord_arrival.is_some());
}

#[test]
fn apply_db_false_resolves_departing_scene_from_live_session() {
    let ctx = test_ctx();
    let (_jita_sys, jita_st) = jita_station_and_sys(&ctx);
    let wh_sys = pick_wh_system_with_effect(&ctx);
    let (char_id, ship_id) = (840_001_i64, 840_101_i64);
    let mut sess = seed_pilot(&ctx, char_id, 840_001, ship_id, 587, wh_sys, None);
    let prev_sess_snap = sess.clone();

    // Simulate caller mutating DB to docked BEFORE calling execute_session_transition_sync(.with_apply_db(false)).
    let mut docked_ch = ctx.db.get_character(char_id).unwrap();
    docked_ch.station_id = Some(jita_st);
    let spec = SessionTransitionSpec::for_kind(
        char_id,
        TransitionKind::Dock { dock_id: jita_st, is_structure: false },
        "dock",
    )
    .with_apply_db(false);

    let departing = resolve_departing_scene(&prev_sess_snap, Some(&docked_ch), true, &spec);
    assert_eq!(departing.sys_id, wh_sys);
    assert_eq!(departing.dock_id, None);
    assert!(departing.in_space);
    assert_eq!(departing.ship_id, ship_id);

    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.insert(char_id, docked_ch);
    });
    ctx.drain_notifications_for_char(char_id);
    ctx.execute_session_transition_sync(Some(&mut sess), spec)
        .expect("apply_db(false) dock succeeds");
    assert!(ctx.drain_notifications_for_char(char_id).iter().any(|n| n.event == "OnSystemWideEffectStop"));
}
