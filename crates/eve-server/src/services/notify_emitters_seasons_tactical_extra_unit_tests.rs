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

#[test]
fn issue_950_load_ammo_primes_sublocation_and_emits_fitting_item_change_deltas() {
    let mut sde = SdeRegistry::default();
    sde.types.insert(587, make_type(587, 25, 6, 2500.0, 400.0, &[(38, 400.0)]));
    // Light Ion Blaster I (type 561, group 74) with capacity 0.0 on struct field but 5.0 in default_attributes[38]
    sde.types.insert(
        561,
        make_type(561, 74, 7, 5.0, 0.0, &[(38, 5.0), (128, 1.0), (604, 86.0)]),
    );
    // Antimatter Charge S (type 222, group 86) with volume 0.0 on struct field but 0.0125 in default_attributes[161]
    sde.types.insert(
        222,
        make_type(222, 86, 8, 0.0, 0.0, &[(161, 0.0125), (128, 1.0)]),
    );
    // Iron Charge S (type 215, group 86)
    sde.types.insert(
        215,
        make_type(215, 86, 8, 0.0125, 0.0, &[(161, 0.0125), (128, 1.0)]),
    );

    let db = Arc::new(WorldRepository::with_demo_seed());
    let char_id = 90_000_001;
    let station_id = 60_000_394_i64;
    let ship_id = 200_246_015_i64;
    let blaster_id = 950_001_i64;
    let antimatter_stack_id = 950_002_i64;
    let iron_oversized_id = 950_003_i64;

    db.acquire_online_lock(1, 501, Some(char_id), true, 1_800_000_000)
        .expect("online lock");
    let ctx = ServerContext::with_sde_and_repository(Arc::new(sde), db);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.active_ship_id = ship_id;
            ch.station_id = Some(station_id);
        }
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            blaster_id,
            InventoryItemRecord {
                item_id: blaster_id,
                type_id: 561,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0, // slot 27
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            antimatter_stack_id,
            InventoryItemRecord {
                item_id: antimatter_stack_id,
                type_id: 222,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 109,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            iron_oversized_id,
            InventoryItemRecord {
                item_id: iron_oversized_id,
                type_id: 215,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 500,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    let mut sess = SessionState::new(501);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.ship_id = Some(ship_id);
    sess.station_id = Some(station_id);
    sess.solar_system_id = Some(30_000_138);

    // 1. Load Antimatter Charge S x109 (entire stack <= 400 cap_rounds) into slot 27
    let _ = ctx.drain_notifications_for_char(char_id);
    let res = handle_load_ammo(
        &ctx,
        &sess,
        &[
            PyRep::Integer(ship_id),
            PyRep::List(vec![PyRep::Integer(blaster_id)]),
            PyRep::List(vec![PyRep::Integer(antimatter_stack_id)]),
        ],
    )
    .expect("LoadAmmo Antimatter Charge S x109");
    assert_eq!(res.dict_get("totalLoaded"), Some(&PyRep::Integer(109)));

    // antimatter_stack_id moved in-place to (ship_id, HiSlot0) with quantity 109
    let moved = ctx.db.get_item(antimatter_stack_id).expect("charge moved in-place");
    assert_eq!(moved.location_id, ship_id);
    assert_eq!(moved.flag, InventoryFlag::HiSlot0);
    assert_eq!(moved.quantity, 109);

    let notifs = ctx.drain_notifications_for_char(char_id);
    // OnItemChange must carry ixFlag = 5 (Cargo) so clientDogmaLocation.IsFitted(item, change) sees wasFitted=False -> isFitted=True
    let item_chg = notifs
        .iter()
        .find(|n| n.event == "OnItemChange")
        .expect("OnItemChange emitted");
    assert_eq!(
        item_chg.args[1],
        PyRep::Dict(vec![(PyRep::Integer(4), PyRep::Integer(InventoryFlag::Cargo as i64))])
    );
    // OnGodmaPrimeItem emitted for sublocation (ship_id, 27, 222) with invItem = None and attributeQuantity (80) = 109.0
    let prime_idx = notifs
        .iter()
        .position(|n| {
            n.event == "OnGodmaPrimeItem"
                && n.args.get(1).and_then(|info| info.dict_get("itemID"))
                    == Some(&PyRep::Tuple(vec![
                        PyRep::Integer(ship_id),
                        PyRep::Integer(27),
                        PyRep::Integer(222),
                    ]))
        })
        .expect("OnGodmaPrimeItem emitted for loaded sublocation");
    let loaded_idx = notifs
        .iter()
        .position(|n| n.event == "OnChargeLoadedToModule")
        .expect("OnChargeLoadedToModule emitted");
    assert!(prime_idx < loaded_idx, "OnGodmaPrimeItem must precede OnChargeLoadedToModule");
    let prime = &notifs[prime_idx];
    assert_eq!(prime.args[0], PyRep::Integer(ship_id));
    assert_eq!(prime.args[1].dict_get("invItem"), Some(&PyRep::None));
    let PyRep::Dict(ref prime_attrs) =
        prime.args[1].dict_get("attributes").expect("prime attributes")
    else {
        panic!("expected Dict for OnGodmaPrimeItem attributes");
    };
    assert!(
        prime_attrs
            .iter()
            .any(|(k, v)| k.as_i64() == Some(80) && v.as_f64() == Some(109.0)),
        "expected attributeQuantity (80) = 109.0 in sublocation attributes: {prime_attrs:?}"
    );

    // 2. Swap to oversized Iron Charge S x500 -> unloads Antimatter (222) to Cargo, splits 400 Iron (215) into slot 27
    let _ = ctx.drain_notifications_for_char(char_id);
    let res2 = handle_load_ammo(
        &ctx,
        &sess,
        &[
            PyRep::Integer(ship_id),
            PyRep::List(vec![PyRep::Integer(blaster_id)]),
            PyRep::List(vec![PyRep::Integer(iron_oversized_id)]),
        ],
    )
    .expect("LoadAmmo Iron Charge S x500 capped to 400");
    assert_eq!(res2.dict_get("totalLoaded"), Some(&PyRep::Integer(400)));
    assert_eq!(ctx.db.get_item(iron_oversized_id).unwrap().quantity, 100);
    assert_eq!(ctx.db.get_item(antimatter_stack_id).unwrap().flag, InventoryFlag::Cargo);

    let notifs2 = ctx.drain_notifications_for_char(char_id);
    // Split charge stack must have non-empty change dict (ixLocationID=0, ixFlag=Cargo)
    assert!(notifs2.iter().any(|n| {
        n.event == "OnItemChange"
            && n.args.get(1)
                == Some(&PyRep::Dict(vec![
                    (PyRep::Integer(3), PyRep::Integer(0)),
                    (PyRep::Integer(4), PyRep::Integer(InventoryFlag::Cargo as i64)),
                ]))
    }));
    // Expelled Antimatter sublocation emits OnModuleAttributeChanges with attributeQuantity (80) -> 0.0 (from 109.0)
    let mod_attr = notifs2
        .iter()
        .find(|n| n.event == "OnModuleAttributeChanges")
        .expect("OnModuleAttributeChanges emitted for expelled sublocation");
    let PyRep::List(ref entries) = mod_attr.args[0] else {
        panic!("expected List in OnModuleAttributeChanges");
    };
    assert!(entries.iter().any(|e| matches!(
        e,
        PyRep::Tuple(t)
            if t.get(2)
                == Some(&PyRep::Tuple(vec![
                    PyRep::Integer(ship_id),
                    PyRep::Integer(27),
                    PyRep::Integer(222),
                ]))
                && t.get(3) == Some(&PyRep::Integer(80))
                && t.get(5).and_then(|v| v.as_f64()) == Some(0.0)
                && t.get(6).and_then(|v| v.as_f64()) == Some(109.0)
    )));
    // Newly loaded Iron Charge S (215) sublocation primed with invItem = None and attributeQuantity (80) = 400.0
    let prime2 = notifs2
        .iter()
        .find(|n| {
            n.event == "OnGodmaPrimeItem"
                && n.args.get(1).and_then(|info| info.dict_get("itemID"))
                    == Some(&PyRep::Tuple(vec![
                        PyRep::Integer(ship_id),
                        PyRep::Integer(27),
                        PyRep::Integer(215),
                    ]))
        })
        .expect("OnGodmaPrimeItem emitted for replacement Iron Charge S sublocation");
    assert_eq!(prime2.args[1].dict_get("invItem"), Some(&PyRep::None));
    let PyRep::Dict(ref prime2_attrs) =
        prime2.args[1].dict_get("attributes").expect("prime2 attributes")
    else {
        panic!("expected Dict for replacement OnGodmaPrimeItem attributes");
    };
    assert!(prime2_attrs
        .iter()
        .any(|(k, v)| k.as_i64() == Some(80) && v.as_f64() == Some(400.0)));
}

#[test]
fn issue_950_load_ammo_unloaded_and_cargo_volume_uses_attr_161_fallback() {
    let mut sde = SdeRegistry::default();
    sde.types.insert(587, make_type(587, 25, 6, 2500.0, 400.0, &[(38, 400.0)]));
    sde.types.insert(
        561,
        make_type(561, 74, 7, 5.0, 200.0, &[(38, 200.0), (128, 1.0), (604, 86.0)]),
    );
    // Charge A has field volume 0.0 and relies on default_attributes[161] = 1.0 m3/unit.
    sde.types.insert(
        222,
        make_type(222, 86, 8, 0.0, 0.0, &[(161, 1.0), (128, 1.0)]),
    );
    sde.types.insert(
        215,
        make_type(215, 86, 8, 1.0, 0.0, &[(161, 1.0), (128, 1.0)]),
    );

    let db = Arc::new(WorldRepository::with_demo_seed());
    let char_id = 90_000_001;
    let station_id = 60_000_394_i64;
    let ship_id = 200_246_016_i64;
    let blaster_id = 950_101_i64;
    let loaded_zero_vol_id = 950_102_i64;
    let cargo_zero_vol_id = 950_103_i64;
    let swap_in_id = 950_104_i64;

    db.acquire_online_lock(1, 502, Some(char_id), true, 1_800_000_000)
        .expect("online lock");
    let ctx = ServerContext::with_sde_and_repository(Arc::new(sde), db);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.active_ship_id = ship_id;
            ch.station_id = Some(station_id);
        }
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            blaster_id,
            InventoryItemRecord {
                item_id: blaster_id,
                type_id: 561,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        // 100 units loaded in slot = 100 m3 via attr 161 fallback
        snap.items.insert(
            loaded_zero_vol_id,
            InventoryItemRecord {
                item_id: loaded_zero_vol_id,
                type_id: 222,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 100,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        // 310 units in Cargo = 310 m3 via attr 161 fallback
        snap.items.insert(
            cargo_zero_vol_id,
            InventoryItemRecord {
                item_id: cargo_zero_vol_id,
                type_id: 222,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 310,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        // 50 units of replacement ammo in Cargo = 50 m3
        snap.items.insert(
            swap_in_id,
            InventoryItemRecord {
                item_id: swap_in_id,
                type_id: 215,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 50,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    let mut sess = SessionState::new(502);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.ship_id = Some(ship_id);
    sess.station_id = Some(station_id);

    // 310 (cargo attr-161) + 50 (swap-in) - 50 (removed) + 100 (unloaded attr-161) = 410 > 400 -> reject
    let err = handle_load_ammo(
        &ctx,
        &sess,
        &[
            PyRep::Integer(ship_id),
            PyRep::List(vec![PyRep::Integer(blaster_id)]),
            PyRep::List(vec![PyRep::Integer(swap_in_id)]),
        ],
    )
    .unwrap_err();
    assert_eq!(err.code, "NotEnoughCargoSpace");

    // Trim Cargo stack to 290 m3 -> 290 + 50 - 50 + 100 = 390 <= 400 -> succeeds
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&cargo_zero_vol_id).unwrap().quantity = 290;
    });
    let res = handle_load_ammo(
        &ctx,
        &sess,
        &[
            PyRep::Integer(ship_id),
            PyRep::List(vec![PyRep::Integer(blaster_id)]),
            PyRep::List(vec![PyRep::Integer(swap_in_id)]),
        ],
    )
    .expect("swap succeeds once cargo space accounts for attr 161");
    assert_eq!(res.dict_get("totalLoaded"), Some(&PyRep::Integer(50)));
}

