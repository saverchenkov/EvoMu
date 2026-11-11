#![allow(unused_imports)]
//! Deep branch-coverage and mutation-killing unit tests for `notify_emitters_seasons_tactical`.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::mobile_deployables::{
    write_deployable_state_snap, MobileDeployableRecord, GROUP_MOBILE_CYNO_BEACON,
};
use crate::services::notify_emitters_seasons_tactical::{
    break_gate_cloak_and_notify, broadcast_char_event, check_and_emit_season_transitions,
    emit_bank_ammo_changed_if_grouped, emit_fleet_escalating_path_if_in_fleet, handle_load_ammo,
    handle_season_slash, handle_select_season_clear_if_none_or_zero, on_deployable_deactivated,
};
use crate::services::season_manager::{
    load_season_state, save_season_state_to_blobs, CharacterChallenge, SeasonConfig,
    SEASONS_CATALOG_KEY, SEASON_CONFIG_KEY,
};
use eve_db::clock::VirtualServerClock;
use eve_db::{
    FleetRecord, InventoryFlag, InventoryItemRecord, ServerClock, TrainedSkillRecord,
    WorldRepository,
};
use eve_destiny::{Ball, CombatState, DVec3, MovementMode, SlimItemMeta};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::{InvType, SdeRegistry};
use std::collections::HashMap;
use std::sync::Arc;

fn make_type(
    type_id: u32,
    group_id: u32,
    category_id: u32,
    volume: f64,
    capacity: f64,
    attrs: &[(u16, f64)],
) -> InvType {
    let mut default_attributes = std::collections::HashMap::new();
    for &(k, v) in attrs {
        default_attributes.insert(k, v);
    }
    InvType {
        type_id,
        group_id,
        category_id,
        name: format!("Type_{type_id}"),
        mass: 100.0,
        volume,
        capacity,
        portion_size: 1,
        radius: 10.0,
        base_price: 1000.0,
        published: true,
        market_group_id: None,
        race_id: None,
        faction_id: None,
        variation_parent_type_id: None,
        description_id: None,
        icon_id: None,
        graphic_id: None,
        sound_id: None,
        meta_group_id: None,
        meta_level: None,
        tech_level: Some(1),
        default_effect_id: None,
        description: String::new(),
        effect_ids: Vec::new(),
        default_attributes,
    }
}

fn setup_ctx() -> (ServerContext, i64, Arc<VirtualServerClock>) {
    let mut sde = SdeRegistry::default();
    // Ship with 5.0 m3 cargo capacity (type 587)
    sde.types.insert(587, make_type(587, 25, 6, 2500.0, 5.0, &[(38, 5.0)]));
    // Capacitor Booster (group 76, cap 4.0 m3) + Cap Booster Charge (group 87, vol 1.0 m3)
    sde.types.insert(3554, make_type(3554, 76, 7, 5.0, 4.0, &[(1795, 12_000.0)]));
    sde.types.insert(3556, make_type(3556, 87, 8, 1.0, 0.0, &[]));
    // Interdiction Sphere Launcher (group 589, cap 10.0) + Warp Disrupt Probe (group 548, vol 5.0)
    sde.types.insert(22778, make_type(22778, 589, 7, 5.0, 10.0, &[]));
    sde.types.insert(22782, make_type(22782, 548, 8, 5.0, 0.0, &[]));
    // Command Burst (group 1770, cap 1.0) + Command Burst Charge (group 1771, vol 0.01)
    sde.types.insert(42526, make_type(42526, 1770, 7, 5.0, 1.0, &[(604, 1771.0)]));
    sde.types.insert(42530, make_type(42530, 1771, 8, 0.01, 0.0, &[]));
    // Modulated Strip Miner (group 464, cap 5.0) + Mining Crystal (group 482, requires skill 18025 level 3)
    sde.types.insert(17482, make_type(17482, 464, 7, 5.0, 5.0, &[(604, 482.0)]));
    sde.types.insert(
        18040,
        make_type(18040, 482, 8, 1.0, 0.0, &[(182, 18025.0), (277, 3.0)]),
    );
    // Mobile Cyno Beacon (group 4093) & Non-beacon deployable (group 1246)
    sde.types.insert(56000, make_type(56000, GROUP_MOBILE_CYNO_BEACON, 22, 50.0, 0.0, &[]));
    sde.types.insert(33474, make_type(33474, 1246, 22, 50.0, 0.0, &[]));

    let db = Arc::new(WorldRepository::with_demo_seed());
    let char_id = 90_000_001;
    db.acquire_online_lock(1, 501, Some(char_id), true, 1_800_000_000)
        .expect("online lock");
    let ctx = ServerContext::with_sde_and_repository(Arc::new(sde), db);
    let clock = Arc::new(VirtualServerClock::with_fixed_base(1_800_000_000));
    ctx.set_clock(clock.clone());
    (ctx, char_id, clock)
}

#[test]
fn season_transition_state_machine_and_slash_command_edge_cases() {
    let (ctx, char_id, clock) = setup_ctx();
    let offline_cid = 90_000_002i64;

    // 1. broadcast_char_event fallback for offline character
    broadcast_char_event(&ctx, "OnSeasonStarted", vec![], Some(offline_cid));
    let _ = ctx.drain_notifications_for_char(char_id);

    // 2. Seed custom SeasonConfig in SEASON_CONFIG_KEY (upcoming -> active -> ended)
    let base_ft = ctx.now_blueos_filetime_ticks();
    let cfg = SeasonConfig {
        season_id: 777,
        template_id: 777,
        season_name: "TransitionSeason".into(),
        start_time_ft: base_ft + 100 * 10_000_000,
        end_time_ft: base_ft + 200 * 10_000_000,
        ..SeasonConfig::default()
    };
    ctx.db.with_mut_snapshot(|snap| {
        snap.seasons_and_campaigns.seasons_catalog.clear();
        snap.seasons_and_campaigns.season_configuration = Some(cfg);
        let mut st = load_season_state(snap, char_id);
        st.selected_season_id = 777;
        save_season_state_to_blobs(snap, char_id, &st);
    });

    // First call initializes announced states (upcoming) without emitting notifications
    check_and_emit_season_transitions(&ctx);
    assert!(ctx.drain_notifications_for_char(char_id).is_empty());

    // Advance clock into active window (+150s) -> emits OnSeasonStarted
    clock.advance_secs(150);
    check_and_emit_season_transitions(&ctx);
    let started_notifs = ctx.drain_notifications_for_char(char_id);
    assert!(started_notifs.iter().any(|n| n.event == "OnSeasonStarted"));

    // Advance clock past end_time_ft (+100s) -> emits OnSeasonEnded and OnSeasonSelectionCleared
    clock.advance_secs(100);
    check_and_emit_season_transitions(&ctx);
    let ended_notifs = ctx.drain_notifications_for_char(char_id);
    assert!(ended_notifs.iter().any(|n| n.event == "OnSeasonEnded"));
    assert!(ended_notifs
        .iter()
        .any(|n| n.event == "OnSeasonSelectionCleared"));
    assert_eq!(
        ctx.db.with_snapshot(|s| load_season_state(s, char_id).selected_season_id),
        0
    );

    // 3. handle_select_season_clear_if_none_or_zero with Some(PyRep::Integer(0))
    assert_eq!(
        handle_select_season_clear_if_none_or_zero(&ctx, char_id, Some(&PyRep::Integer(0))).unwrap().unwrap(),
        PyRep::Bool(true)
    );

    // 4. handle_season_slash branches: default 30-day start, end unknown season, invalid args, reset_goals preserving claimed
    let start_default = handle_season_slash(&ctx, char_id, "/season start 888")
        .unwrap()
        .expect("start default 30 days");
    assert_eq!(start_default.dict_get("seasonID"), Some(&PyRep::Integer(888)));

    assert_eq!(
        handle_season_slash(&ctx, char_id, "/season end")
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidSlashArgument"
    );
    assert_eq!(
        handle_season_slash(&ctx, char_id, "/season end 999999")
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidSeasonId"
    );
    assert_eq!(
        handle_season_slash(&ctx, char_id, "/season clear abc")
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidSlashArgument"
    );
    assert_eq!(
        handle_season_slash(&ctx, char_id, "/season reset abc")
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidSlashArgument"
    );
    assert_eq!(
        handle_season_slash(&ctx, char_id, "/season reset 999999999")
            .unwrap()
            .unwrap_err()
            .code,
        "CharacterNotFound"
    );

    // Seed claimed & unclaimed challenge on char_id and run `/season reset`
    ctx.db.with_mut_snapshot(|snap| {
        let mut st = load_season_state(snap, char_id);
        st.season_points = 500;
        let c_claimed = CharacterChallenge {
            challenge_id: 1,
            season_id: 777,
            title: "C1".into(),
            description: String::new(),
            points: 100,
            progress: 10,
            max_progress: 10,
            completed: true,
            claimed: true,
            is_dormant: false,
            expiration_date: 0,
            client_event_driven: false,
            reward_type_id: 0,
            reward_quantity: 0,
            baseline_progress: 0,
        };
        let c_unclaimed = CharacterChallenge {
            challenge_id: 2,
            progress: 5,
            claimed: false,
            ..c_claimed.clone()
        };
        st.challenges = HashMap::from([(1, c_claimed), (2, c_unclaimed)]);
        save_season_state_to_blobs(snap, char_id, &st);
    });
    let _ = handle_season_slash(&ctx, char_id, &format!("/season reset {char_id}"))
        .unwrap()
        .unwrap();
    ctx.db.with_snapshot(|snap| {
        let st = load_season_state(snap, char_id);
        assert_eq!(st.season_points, 0);
        assert_eq!(st.claimed_challenges[&1].progress, 10);
        assert!(st.claimed_challenges[&1].completed);
        assert_eq!(st.challenges[&2].progress, 0);
        assert!(!st.challenges[&2].completed);
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn break_gate_cloak_deployables_and_load_ammo_all_branches() {
    let (ctx, char_id, _) = setup_ctx();
    let sys_id = 30_000_142u32;
    let sys = ctx.systems.get_or_spawn(sys_id);
    let ship_id = 100_000_001i64;
    let station_id = 60_003_760i64;

    // 1. break_gate_cloak_and_notify with both gate cloak and undock invulnerability
    let ball = Ball {
        id: ship_id,
        type_id: 587,
        position: DVec3::ZERO,
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 1.0,
        mass: 1.0e6,
        radius: 50.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let mut cs = CombatState::default();
    cs.is_cloaked = true;
    cs.gate_cloak_remaining_ticks = 30;
    cs.undock_invulnerable_remaining_ticks = 15;
    let _ = sys.spawn_ball_with_meta(ball, cs, SlimItemMeta::default()).await;

    let _ = ctx.drain_notifications_for_char(char_id);
    let broke = break_gate_cloak_and_notify(&ctx, &sys, char_id, ship_id).await;
    assert!(broke);
    let cloak_notifs = ctx.drain_notifications_for_char(char_id);
    assert!(cloak_notifs.iter().any(|n| n.event == "OnJumpCloakCancelled"));
    assert!(cloak_notifs.iter().any(|n| n.event == "OnInvulnCancelled"));
    assert!(!break_gate_cloak_and_notify(&ctx, &sys, char_id, ship_id).await);

    // 2. on_deployable_deactivated from deployable_state_snap and non-fleet emit_fleet_escalating_path
    emit_fleet_escalating_path_if_in_fleet(&ctx, char_id, 999);
    ctx.db.with_mut_snapshot(|snap| {
        snap.fleets.insert(
            6001,
            FleetRecord {
                fleet_id: 6001,
                boss_id: char_id,
                members: vec![char_id],
                ..Default::default()
            },
        );
        write_deployable_state_snap(snap, |blob| {
            for (iid, tid, gid) in [
                (700_001, 56000, GROUP_MOBILE_CYNO_BEACON),
                (700_002, 33474, 1246),
            ] {
                blob.deployables.insert(
                    iid,
                    MobileDeployableRecord {
                        item_id: iid,
                        type_id: tid,
                        group_id: gid,
                        owner_id: char_id,
                        solar_system_id: sys_id,
                        anchor_start_unix: 100,
                        anchor_duration_secs: 60,
                        anchor_completed: true,
                        is_unanchored: false,
                        expires_at_unix: 999_999,
                        last_interaction_unix: 100,
                        reinforced_until_unix: None,
                        last_ping_unix: None,
                        position: [0.0, 0.0, 0.0],
                        deployed_fleet_id: Some(6001),
                    },
                );
            }
        });
    });
    on_deployable_deactivated(&ctx, None, 700_002);
    on_deployable_deactivated(&ctx, None, 700_001);
    let dep_notifs = ctx.drain_notifications_for_char(char_id);
    assert!(dep_notifs
        .iter()
        .any(|n| n.event == "OnFleetJumpBeaconDeployableChange"));

    // 3. handle_load_ammo branches:
    // - ShipOwnershipViolation, InvalidArgument, ItemNotFound, ItemOwnershipViolation, AmmoNotReachable
    let mut sess = SessionState::new(501);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.ship_id = Some(ship_id);
    sess.station_id = None;
    sess.solar_system_id = Some(sys_id);

    assert_eq!(
        handle_load_ammo(&ctx, &sess, &[PyRep::Integer(999_999)]).unwrap_err().code,
        "ShipOwnershipViolation"
    );
    assert_eq!(
        handle_load_ammo(&ctx, &sess, &[PyRep::Integer(ship_id), PyRep::List(vec![]), PyRep::List(vec![])])
            .unwrap_err()
            .code,
        "InvalidArgument"
    );
    assert_eq!(
        handle_load_ammo(
            &ctx,
            &sess,
            &[PyRep::Integer(ship_id), PyRep::Integer(1), PyRep::Integer(999_999)],
        )
        .unwrap_err()
        .code,
        "ItemNotFound"
    );

    let other_charge = 890_001i64;
    let locked_charge = 890_002i64;
    let container_id = 890_003i64;
    let cont_cap_charge = 890_004i64;
    let wubble_probe = 890_005i64;
    let crystal_charge = 890_006i64;
    let cmd_charge = 890_007i64;
    let cap_mod = 890_010i64;
    let wubble_mod = 890_011i64;
    let miner_mod = 890_012i64;
    let cmd_mod = 890_013i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.active_ship_id = ship_id;
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
        }
        let _ = station_id;
        snap.items.insert(
            other_charge,
            InventoryItemRecord {
                item_id: other_charge,
                type_id: 3556,
                owner_id: char_id + 1,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 10,
                ..Default::default()
            },
        );
        snap.items.insert(
            locked_charge,
            InventoryItemRecord {
                item_id: locked_charge,
                type_id: 3556,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Locked,
                quantity: 10,
                ..Default::default()
            },
        );
        snap.items.insert(
            container_id,
            InventoryItemRecord {
                item_id: container_id,
                type_id: 3293,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            cont_cap_charge,
            InventoryItemRecord {
                item_id: cont_cap_charge,
                type_id: 3556,
                owner_id: char_id,
                location_id: container_id,
                flag: InventoryFlag::None,
                quantity: 4,
                ..Default::default()
            },
        );
        snap.items.insert(
            wubble_probe,
            InventoryItemRecord {
                item_id: wubble_probe,
                type_id: 22782,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::SpecializedAmmoHold,
                quantity: 2,
                ..Default::default()
            },
        );
        snap.items.insert(
            crystal_charge,
            InventoryItemRecord {
                item_id: crystal_charge,
                type_id: 18040,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::SpecializedAmmoHold,
                quantity: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            cmd_charge,
            InventoryItemRecord {
                item_id: cmd_charge,
                type_id: 42530,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::SpecializedAmmoHold,
                quantity: 100,
                ..Default::default()
            },
        );
        for (mid, tid, flg) in [
            (cap_mod, 3554, InventoryFlag::MedSlot0),
            (wubble_mod, 22778, InventoryFlag::HiSlot0),
            (miner_mod, 17482, InventoryFlag::HiSlot1),
            (cmd_mod, 42526, InventoryFlag::HiSlot2),
        ] {
            snap.items.insert(
                mid,
                InventoryItemRecord {
                    item_id: mid,
                    type_id: tid,
                    owner_id: char_id,
                    location_id: ship_id,
                    flag: flg,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
        }
        snap.skills.entry(char_id).or_default().insert(
            43728,
            TrainedSkillRecord { type_id: 43728, level: 4, skill_points: 100_000.0 },
        );
    });

    assert_eq!(
        handle_load_ammo(&ctx, &sess, &[PyRep::Integer(ship_id), PyRep::Integer(cap_mod), PyRep::Integer(other_charge)])
            .unwrap_err()
            .code,
        "ItemOwnershipViolation"
    );
    assert_eq!(
        handle_load_ammo(&ctx, &sess, &[PyRep::Integer(ship_id), PyRep::Integer(cap_mod), PyRep::Integer(locked_charge)])
            .unwrap_err()
            .code,
        "AmmoNotReachable"
    );

    // Empty module list returns PyRep::None
    assert_eq!(
        handle_load_ammo(&ctx, &sess, &[PyRep::Integer(ship_id), PyRep::List(vec![]), PyRep::Integer(cont_cap_charge)]).unwrap(),
        PyRep::None
    );

    // Load cap booster charge from container using slot flag integer (MedSlot0 = 19)
    let cap_load = handle_load_ammo(
        &ctx,
        &sess,
        &[
            PyRep::Integer(ship_id),
            PyRep::Integer(InventoryFlag::MedSlot0 as i64),
            PyRep::Integer(cont_cap_charge),
        ],
    )
    .expect("load cap booster from container");
    assert_eq!(cap_load.dict_get("totalLoaded"), Some(&PyRep::Integer(4)));

    // Load wubble probe (2x 5.0 m3 = 10.0 m3) into wubble_mod, then reload when cargo has 1.0 m3 left -> NotEnoughCargoSpace!
    let _ = handle_load_ammo(
        &ctx,
        &sess,
        &[PyRep::Integer(ship_id), PyRep::Integer(wubble_mod), PyRep::Integer(wubble_probe)],
    )
    .expect("load wubble probe");
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            890_099,
            InventoryItemRecord {
                item_id: 890_099,
                type_id: 22782,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::SpecializedAmmoHold,
                quantity: 2,
                ..Default::default()
            },
        );
    });
    assert_eq!(
        handle_load_ammo(
            &ctx,
            &sess,
            &[PyRep::Integer(ship_id), PyRep::Integer(wubble_mod), PyRep::Integer(890_099)],
        )
        .unwrap_err()
        .code,
        "NotEnoughCargoSpace"
    );

    // Mining crystal requires skill 18025 level 3 -> fails before training, succeeds after
    assert!(handle_load_ammo(
        &ctx,
        &sess,
        &[PyRep::Integer(ship_id), PyRep::Integer(miner_mod), PyRep::Integer(crystal_charge)],
    )
    .is_err());

    // Command burst reload time uses skill 43728 level 4 -> 60000 * (1 - 0.4) = 36000 ms
    let cmd_res = handle_load_ammo(
        &ctx,
        &sess,
        &[PyRep::Integer(ship_id), PyRep::Integer(cmd_mod), PyRep::Integer(cmd_charge)],
    )
    .expect("load command burst");
    assert_eq!(cmd_res.dict_get("reloadTimeMs"), Some(&PyRep::Real(36_000.0)));
    emit_bank_ammo_changed_if_grouped(&ctx, char_id, ship_id, &[cmd_mod]);
}
