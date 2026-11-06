//! Deep branch-coverage and mutation-killing unit tests for `pve_ess_reserve_banks`,
//! `pve_dungeon_runtime`, `pve_dungeon_runtime_rpc`, and `insurgency_complex_ops`.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::fw_services::{HQ_SYSTEM_AMO, HQ_SYSTEM_INTAKI};
use crate::services::insurgency_complex_ops::{
    activate_outpost_raid_gate_in_space, append_insurgency_npc_wreck_loot,
    check_insurgency_laundering_access, is_battlecruiser_group, is_named_insurgency_roamer,
    is_named_pirate_commander_roamer, is_outpost_raid_gate,
    is_warp_into_outpost_raid_pocket_blocked, is_warzone_opposing_empire, load_ops_state,
    named_insurgency_roamer_loot_drop, on_insurgency_laundering_trade_completed, save_ops_state,
    share_site_lp_up_to_5, OutpostRaidInstance, OutpostRaidSize, TYPE_ANGEL_INSURGENCY_DATA_SEQ,
    TYPE_FEDERATION_CI_OUTPOST_HUB, TYPE_GURISTAS_INSURGENCY_DATA_SEQ,
    TYPE_GURISTAS_LAUNDERING_FACILITY, TYPE_STATE_CI_OUTPOST_HUB, TYPE_UPWELL_CONSORTIUM_DATA_CACHE,
};
use crate::services::insurgency_services::{FACTION_ANGEL_CARTEL, FACTION_GURISTAS};
use crate::services::pve_dungeon_runtime::{
    check_deadspace_mwd_allowed, check_deadspace_warp_allowed, despawn_completed_dungeon,
    find_active_dungeon_id_at_position, is_ship_in_deadspace_pocket, on_dungeon_npc_killed,
    on_warp_start_leave_dungeon_if_needed, with_dungeon_runtime_store, DungeonNpcGroup,
    DungeonRuntimeInstance, DungeonRuntimeObject, DungeonRuntimeRoom, DungeonTrigger,
    DungeonTriggerAction, DungeonTriggerKind,
};
use crate::services::pve_dungeon_runtime_rpc::{
    check_deadspace_mwd_disallowed, check_warp_target_blocked_by_gated_dungeon_room,
    get_dungeon_instance_state, on_dungeon_approach, on_dungeon_item_looted,
    try_activate_dungeon_runtime_gate,
};
use crate::services::pve_ess_reserve_banks::{
    check_ess_intrusion_bubble_restriction, check_ess_warp_destination_allowed,
    compute_ess_reserve_pulse_payout_cents, compute_rnd_rp_per_day,
    credit_ballpark_npc_bounty_with_ess_split, ensure_ess_intrusion_site_balls,
    ensure_ess_reserve_bank_not_already_unlocked, ess_reserve_pulse_weight,
    has_completed_mission_with_agent, is_ess_gate_eligible_hull, is_interbus_or_militia_corp,
    is_storyline_qualifying_agent_type, max_affordable_datacores, record_agent_mission_completed,
    record_ess_main_bank_contribution, resolve_datacore_for_research_skill,
    tick_ess_main_bank_autopayout, try_activate_ess_acceleration_gate, ESS_GATE_ID_BASE,
};
use eve_db::{
    InventoryFlag, InventoryItemRecord, PersistedInsurgencyCampaign, ServerClock,
    VirtualServerClock, WorldRepository,
};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use glam::DVec3;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| {
        let base = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let p = [
            SdeRegistry::default_db_path(),
            base.join("../../data/evomu_sde.sqlite"),
            base.join("../../../../data/evomu_sde.sqlite"),
        ]
        .into_iter()
        .find(|cand| cand.exists())
        .unwrap_or_else(SdeRegistry::default_db_path);
        Arc::new(SdeRegistry::open(&p).expect("open default SDE"))
    }))
}

fn fast_test_context(base_unix: u64) -> (Arc<ServerContext>, Arc<VirtualServerClock>) {
    let db = Arc::new(WorldRepository::with_demo_seed());
    let ctx = Arc::new(ServerContext::with_sde_and_repository(shared_sde(), db));
    let clock = Arc::new(VirtualServerClock::with_fixed_base(base_unix));
    ctx.set_clock(Arc::clone(&clock) as Arc<dyn ServerClock>);
    (ctx, clock)
}

#[tokio::test]
async fn pve_ess_reserve_banks_formulas_gate_guards_and_autopayout_branches() {
    let base = 1_800_000_000_u64;
    let (ctx, _) = fast_test_context(base);
    let (char_id, ship_id) = (90_000_001_i64, 100_000_001_i64);

    // 1. Hull eligibility, agent mission recording, R&D RP, datacores, reserve pulse weights
    assert!(!is_ess_gate_eligible_hull(&ctx.sde, 670)); // Capsule ineligible
    assert!(!is_ess_gate_eligible_hull(&ctx.sde, 999_999_999)); // Unknown type ineligible
    if let Some((&cruiser_tid, _)) = ctx.sde.types.iter().find(|(_, t)| t.group_id == 26) {
        assert!(is_ess_gate_eligible_hull(&ctx.sde, cruiser_tid));
    }
    assert!(is_storyline_qualifying_agent_type(2) && !is_storyline_qualifying_agent_type(99));
    ctx.db.with_mut_snapshot(|mut snap| {
        assert!(!has_completed_mission_with_agent(snap, char_id, 3008416));
        record_agent_mission_completed(&mut snap, char_id, 3008416);
        record_agent_mission_completed(&mut snap, char_id, 3008416); // duplicate idempotent
        assert!(has_completed_mission_with_agent(snap, char_id, 3008416));
    });

    assert_eq!(max_affordable_datacores(-5.0), 0);
    assert_eq!(max_affordable_datacores(99.9), 0);
    assert_eq!(max_affordable_datacores(250.0), 2);
    ctx.db.with_snapshot(|snap| {
        assert!(compute_rnd_rp_per_day(&ctx, snap, char_id, 3008416, 4, 5) > compute_rnd_rp_per_day(&ctx, snap, char_id, 3008416, 1, 0));
    });
    let militia_corp = ctx
        .sde
        .factions
        .values()
        .find_map(|f| f.militia_corporation_id)
        .unwrap();
    assert!(is_interbus_or_militia_corp(&ctx.sde, militia_corp));
    assert!(!is_interbus_or_militia_corp(&ctx.sde, 999_999));
    let _ = resolve_datacore_for_research_skill(&ctx.sde, 3402);

    assert_eq!(ess_reserve_pulse_weight(1, 10), 100);
    assert_eq!(ess_reserve_pulse_weight(8, 10), 300);
    assert_eq!(ess_reserve_pulse_weight(10, 10), 150);
    assert_eq!(ess_reserve_pulse_weight(1, 1), 300);
    assert!(compute_ess_reserve_pulse_payout_cents(100_000_000, 10, 3) > 0);

    // 2. Find a K-space nullsec system (sec <= 0.0 and system_id < 31_000_000)
    let null_sys = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| s.solar_system_id < 31_000_000 && s.security_status <= 0.0)
        .map(|s| s.solar_system_id)
        .unwrap_or(30_002_904);
    let wrong_gate = 999_000_001_i64;
    let ess_gate = ESS_GATE_ID_BASE + i64::from(null_sys);
    assert!(try_activate_ess_acceleration_gate(&ctx, char_id, null_sys, ship_id, wrong_gate).await.is_none());

    // ShipNotInSpace
    assert_eq!(
        try_activate_ess_acceleration_gate(&ctx, char_id, null_sys, ship_id, ess_gate)
            .await.unwrap().unwrap_err().code,
        "ShipNotInSpace"
    );

    // Seed ship ball in warp -> ShipInWarp
    let sys_handle = ctx.systems.get_or_spawn(null_sys);
    let _ = sys_handle.spawn_ball_with_meta(
        Ball {
            id: ship_id, type_id: 587, position: DVec3::new(1e7, 0.0, 0.0), velocity: DVec3::ZERO,
            max_velocity: 300.0, inertia: 2.0, mass: 1e6, radius: 40.0, speed_fraction: 1.0,
            mode: MovementMode::new_warp(DVec3::ZERO, DVec3::new(1e9, 0.0, 0.0), 3.0),
        },
        ctx.build_ship_combat_state(char_id, ship_id, 587),
        ctx.build_ship_slim_meta(char_id, 587),
    ).await;
    assert_eq!(
        try_activate_ess_acceleration_gate(&ctx, char_id, null_sys, ship_id, ess_gate).await.unwrap().unwrap_err().code,
        "ShipInWarp"
    );

    // Stop warp, add scramble -> WarpScrambled
    let _ = sys_handle.set_movement_mode(ship_id, MovementMode::Stop).await;
    if let Ok(mut bp) = sys_handle.ballpark.write() {
        bp.combat_states.entry(ship_id).or_default().warp_scramble_strength = 3;
    }
    assert_eq!(
        try_activate_ess_acceleration_gate(&ctx, char_id, null_sys, ship_id, ess_gate).await.unwrap().unwrap_err().code,
        "WarpScrambled"
    );

    // Clear scramble, stay > 2500m from gate -> OutsideGateActivationRange
    if let Ok(mut bp) = sys_handle.ballpark.write() {
        bp.combat_states.entry(ship_id).or_default().warp_scramble_strength = 0;
    }
    assert_eq!(
        try_activate_ess_acceleration_gate(&ctx, char_id, null_sys, ship_id, ess_gate).await.unwrap().unwrap_err().code,
        "OutsideGateActivationRange"
    );

    // Move ship within 500m of gate -> Rifter (587, frigate) hits ShipRestrictedByEssGate!
    ensure_ess_intrusion_site_balls(&ctx, null_sys).await;
    let gate_pos = sys_handle.get_ball_position_sync(ess_gate).unwrap();
    let _ = sys_handle.set_ball_position(ship_id, gate_pos + DVec3::new(400.0, 0.0, 0.0)).await;
    assert_eq!(
        try_activate_ess_acceleration_gate(&ctx, char_id, null_sys, ship_id, ess_gate).await.unwrap().unwrap_err().code,
        "ShipRestrictedByEssGate"
    );

    // Check intrusion bubble restriction when ship is inside bubble (within 75km of beacon)
    let beacon_pos = sys_handle.get_ball_position_sync(820_000_000 + i64::from(null_sys)).unwrap();
    let _ = sys_handle.set_ball_position(ship_id, beacon_pos + DVec3::new(0.0, 10_000.0, 0.0)).await;
    assert!(check_ess_intrusion_bubble_restriction(&ctx, null_sys, ship_id, "mwd").await.is_err());
    let warp_dest_err = check_ess_warp_destination_allowed(&ctx, null_sys, DVec3::ZERO, beacon_pos + DVec3::new(0.0, 10_000.0, 0.0)).await;
    assert_eq!(warp_dest_err.unwrap_err().code, "EssIntrusionBubbleWarpBlocked");

    // 3. Reserve bank unlock guard, main bank contribution, autopayout, and bounty split
    ctx.db.with_mut_snapshot(|mut snap| {
        assert!(ensure_ess_reserve_bank_not_already_unlocked(snap, null_sys, base, 0).is_ok());
        assert_eq!(
            ensure_ess_reserve_bank_not_already_unlocked(snap, null_sys, base, base + 300).unwrap_err().code,
            "EssReserveBankAlreadyUnlocked"
        );
        let st = snap.system_states.entry(null_sys).or_default();
        st.ess_main_bank = eve_db::IskCents(1_000_000);
        record_ess_main_bank_contribution(st, char_id, 25_000, base);
        credit_ballpark_npc_bounty_with_ess_split(
            &ctx.sde,
            &mut snap,
            null_sys,
            char_id,
            100_000.0,
            100_000.0,
            1.0,
            10_001,
            base,
        );
    });
    let _ = tick_ess_main_bank_autopayout(&ctx, base + 20_000);
}

#[tokio::test]
async fn pve_dungeon_runtime_and_rpc_pocket_guards_triggers_and_despawn() {
    let base = 1_800_000_000_u64;
    let (ctx, _) = fast_test_context(base);
    let char_id = 90_000_001_i64;
    let ship_id = 100_000_001_i64;
    let sys_id = 30_000_142_u32;

    let room1_pos = [2_000_000_000.0, 0.0, 0.0];
    let room2_pos = [2_000_005_000_000.0, 0.0, 0.0];
    let gate_pos = [2_000_005_000.0, 0.0, 0.0];

    let inst_id = 99_500_i64;
    let entry_gate_id = 99_510_i64;
    let gate_id = 99_501_i64;
    let npc_ball_id = 99_502_i64;
    let loot_obj_id = 99_503_i64;

    with_dungeon_runtime_store(&ctx, |store| {
        store.instances.insert(
            inst_id,
            DungeonRuntimeInstance {
                instance_id: inst_id,
                dungeon_id: 42,
                dungeon_name: "Deep Unit Deadspace".into(),
                category: "combat".into(),
                archetype: "pirate".into(),
                solar_system_id: sys_id,
                owner_char_id: char_id,
                base_position: room1_pos,
                disallow_mwd: true,
                entry_gate_ball_id: Some(entry_gate_id),
                entry_gate_pos: Some(gate_pos),
                entry_gate_locked: false,
                current_room_index: 0,
                rooms: vec![
                    DungeonRuntimeRoom {
                        room_index: 0,
                        room_id: 1,
                        room_name: "Room 1".into(),
                        position: room1_pos,
                        gate_destination_pos: room2_pos,
                        gated: true,
                        disallow_mwd: Some(true),
                        gate_ball_id: Some(gate_id),
                        gate_locked: true,
                        objects: vec![DungeonRuntimeObject {
                            ball_id: loot_obj_id,
                            type_id: 23,
                            name: "Objective Container".into(),
                            position: room1_pos,
                            radius: 500.0,
                            is_container: true,
                            loot_items: vec![(34, 100)],
                            spawned: true,
                        }],
                        npc_groups: vec![DungeonNpcGroup {
                            group_id: 1,
                            wave: 1,
                            npc_specs: vec![(10_001, 1)],
                            npc_ball_ids: vec![npc_ball_id],
                            spawn_immediately: true,
                            spawned: true,
                            cleared: false,
                        }],
                        triggers: vec![DungeonTrigger {
                            trigger_id: 1,
                            kind: DungeonTriggerKind::OnKill {
                                target_npc_group: Some(1),
                                target_ball_id: Some(npc_ball_id),
                            },
                            action: DungeonTriggerAction::UnlockGate,
                            fired: false,
                        }],
                        spawned: true,
                        cleared: false,
                    },
                    DungeonRuntimeRoom {
                        room_index: 1, room_id: 2, room_name: "Final Room".into(),
                        position: room2_pos, gate_destination_pos: room2_pos, gated: false,
                        disallow_mwd: Some(false), gate_ball_id: None, gate_locked: false,
                        objects: Vec::new(), npc_groups: Vec::new(), triggers: Vec::new(),
                        spawned: true, cleared: true,
                    },
                ],
                completed: false,
                despawned: false,
                active_objective_id: 1,
                last_completed_task_id: None,
                completed_task_ids: Vec::new(),
                created_unix: base,
            },
        );
    });

    // 1. State query + position check + deadspace MWD & warp guards
    assert!(get_dungeon_instance_state(&ctx, inst_id).is_ok());
    assert!(get_dungeon_instance_state(&ctx, 777_777).is_err());
    assert_eq!(find_active_dungeon_id_at_position(&ctx, sys_id, room1_pos), Some(42));
    assert!(is_ship_in_deadspace_pocket(&ctx, sys_id, room1_pos));
    assert!(check_deadspace_mwd_allowed(&ctx, sys_id, room1_pos).is_err());
    assert!(check_deadspace_warp_allowed(&ctx, sys_id, room1_pos, [0.0; 3], room2_pos).is_err());

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_handle.spawn_ball_with_meta(
        Ball {
            id: ship_id, type_id: 587, position: DVec3::from_array(room1_pos), velocity: DVec3::ZERO,
            max_velocity: 300.0, inertia: 2.0, mass: 1e6, radius: 40.0, speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
        ctx.build_ship_combat_state(char_id, ship_id, 587),
        ctx.build_ship_slim_meta(char_id, 587),
    ).await;
    let _ = sys_handle.spawn_ball_with_meta(
        Ball {
            id: gate_id, type_id: 17831, position: DVec3::from_array(gate_pos), velocity: DVec3::ZERO,
            max_velocity: 0.0, inertia: 1.0, mass: 1e7, radius: 500.0, speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
        eve_destiny::CombatState::default(),
        eve_destiny::SlimItemMeta::default(),
    ).await;

    assert!(check_deadspace_mwd_disallowed(&ctx, &sys_handle, sys_id, ship_id).await.is_ok());
    with_dungeon_runtime_store(&ctx, |st| {
        if let Some(i) = st.instances.get_mut(&inst_id) {
            i.current_room_index = 1;
        }
    });
    assert!(check_deadspace_mwd_disallowed(&ctx, &sys_handle, sys_id, ship_id).await.unwrap_err().code.starts_with("MicroWarpdriveDisallowed"));
    with_dungeon_runtime_store(&ctx, |st| {
        if let Some(i) = st.instances.get_mut(&inst_id) {
            i.current_room_index = 0;
        }
    });
    assert_eq!(
        check_warp_target_blocked_by_gated_dungeon_room(&ctx, sys_id, DVec3::from_array(room2_pos), None).unwrap_err().code,
        "WarpBlockedByDeadspacePocket"
    );

    // 2. Gate activation outside range -> OutsideGateActivationRange
    let mut sess = SessionState::new(5);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.ship_id = Some(ship_id);
    sess.solar_system_id = Some(sys_id);
    let _ = sys_handle.set_ball_position(ship_id, DVec3::from_array(room2_pos)).await;
    assert_eq!(
        try_activate_dungeon_runtime_gate(&ctx, &mut sess, sys_id, &[PyRep::Integer(gate_id)]).await.unwrap().unwrap_err().code,
        "OutsideGateActivationRange"
    );

    // Move within 500m of gate: enter Room 1 via entry gate -> locked Room 1 gate returns GateLocked -> kill NPC -> enter Room 2!
    let _ = sys_handle.set_ball_position(ship_id, DVec3::from_array(gate_pos)).await;
    assert!(try_activate_dungeon_runtime_gate(&ctx, &mut sess, sys_id, &[PyRep::Integer(entry_gate_id)]).await.unwrap().is_ok());
    let _ = sys_handle.set_ball_position(ship_id, DVec3::from_array(gate_pos)).await;
    assert_eq!(
        try_activate_dungeon_runtime_gate(&ctx, &mut sess, sys_id, &[PyRep::Integer(gate_id)]).await.unwrap().unwrap_err().code,
        "GateLocked"
    );

    // 3. Kill room NPC -> unlocks gate -> activate gate -> transitions to Room 2!
    on_dungeon_npc_killed(&ctx, sys_id, npc_ball_id, char_id).await;
    assert!(try_activate_dungeon_runtime_gate(&ctx, &mut sess, sys_id, &[PyRep::Integer(gate_id)]).await.unwrap().is_ok());

    on_dungeon_approach(&ctx, char_id, sys_id, ship_id, Some(loot_obj_id)).await;
    on_dungeon_item_looted(&ctx, char_id, Some(sys_id), loot_obj_id, 34).await;
    on_warp_start_leave_dungeon_if_needed(&ctx, char_id, sys_id, room1_pos, [0.0; 3]);
    despawn_completed_dungeon(&ctx, sys_id, inst_id, char_id).await;
}

#[tokio::test]
async fn insurgency_complex_ops_laundering_roamers_lp_sharing_and_raid_gates() {
    let base = 1_800_000_000_u64;
    let (ctx, _) = fast_test_context(base);
    let (char_id, ship_id) = (90_000_001_i64, 100_000_001_i64);

    // 1. OutpostRaidSize properties & ship group filters
    for size in [OutpostRaidSize::Small, OutpostRaidSize::Medium, OutpostRaidSize::Moderate, OutpostRaidSize::LargeAdv] {
        let _ = (
            size.slot_index(), size.beacon_type_id(), size.base_capture_duration_secs(),
            size.contribution_percent(), size.contribution_subpoints(), size.base_lp_reward(),
            size.respawn_delay_secs(), size.gate_position(), size.pocket_position(),
            size.mordus_defender_type_id(), size.pirate_defender_type_id(FACTION_GURISTAS),
            size.pirate_defender_type_id(FACTION_ANGEL_CARTEL),
        );
    }
    assert!(OutpostRaidSize::Small.allows_ship_group(25)); // Frigate
    assert!(!OutpostRaidSize::Small.allows_ship_group(27)); // Battleship rejected
    assert!(OutpostRaidSize::LargeAdv.allows_ship_group(27));
    assert!(is_battlecruiser_group(419) && !is_battlecruiser_group(25));

    // 2. LP sharing up to 5 pilots & roamer checks
    assert_eq!(share_site_lp_up_to_5(10_000, 0), 0);
    assert_eq!(share_site_lp_up_to_5(10_000, 1), 10_000);
    assert_eq!(share_site_lp_up_to_5(10_000, 5), 10_000);
    assert!(share_site_lp_up_to_5(10_000, 6) < 10_000);

    assert!(is_warzone_opposing_empire(FACTION_ANGEL_CARTEL, 500_002));
    assert!(is_warzone_opposing_empire(FACTION_GURISTAS, 500_001));
    assert!(!is_warzone_opposing_empire(FACTION_GURISTAS, 500_002));
    assert!(!is_named_pirate_commander_roamer(1));
    assert!(!is_named_insurgency_roamer(1));
    assert_eq!(named_insurgency_roamer_loot_drop(1), None);
    let mut wreck_loot = Vec::new();
    append_insurgency_npc_wreck_loot(1, &mut wreck_loot);

    // 3. Laundering access & completion (Pirate FOB vs Empire War HQ)
    assert!(check_insurgency_laundering_access(&ctx, char_id, HQ_SYSTEM_INTAKI, 34).is_ok()); // non-trader ignored
    assert_eq!(
        check_insurgency_laundering_access(&ctx, char_id, HQ_SYSTEM_INTAKI, TYPE_GURISTAS_LAUNDERING_FACILITY)
            .unwrap_err()
            .code,
        "FactionNotAligned"
    );
    assert_eq!(
        check_insurgency_laundering_access(&ctx, char_id, HQ_SYSTEM_AMO, TYPE_FEDERATION_CI_OUTPOST_HUB)
            .unwrap_err()
            .code,
        "InvalidWarHQLocation"
    );
    assert_eq!(
        check_insurgency_laundering_access(&ctx, char_id, HQ_SYSTEM_INTAKI, TYPE_FEDERATION_CI_OUTPOST_HUB)
            .unwrap_err()
            .code,
        "FactionNotAligned"
    );

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.war_faction_id = Some(500_004);
        }
    });
    assert!(check_insurgency_laundering_access(&ctx, char_id, HQ_SYSTEM_INTAKI, TYPE_FEDERATION_CI_OUTPOST_HUB).is_ok());
    on_insurgency_laundering_trade_completed(&ctx, char_id, HQ_SYSTEM_INTAKI, TYPE_FEDERATION_CI_OUTPOST_HUB, 2);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.war_faction_id = Some(FACTION_GURISTAS);
        }
        snap.insurgency_runtime.campaigns.insert(
            701,
            PersistedInsurgencyCampaign {
                campaign_id: 701, pirate_faction_id: FACTION_GURISTAS, target_faction_id: 500_001,
                fob_solar_system_id: HQ_SYSTEM_AMO, systems: vec![HQ_SYSTEM_AMO],
                created_at_unix: base - 7_200, forecasting_end_unix: base - 1,
                active_end_unix: base + 86_400, winddown_end_unix: base + 90_000,
                corruption_points: HashMap::new(), suppression_points: HashMap::new(),
            },
        );
    });
    assert_eq!(
        check_insurgency_laundering_access(&ctx, char_id, HQ_SYSTEM_INTAKI, TYPE_GURISTAS_LAUNDERING_FACILITY)
            .unwrap_err()
            .code,
        "InvalidLaunderingLocation"
    );
    assert!(check_insurgency_laundering_access(&ctx, char_id, HQ_SYSTEM_AMO, TYPE_GURISTAS_LAUNDERING_FACILITY).is_ok());
    on_insurgency_laundering_trade_completed(&ctx, char_id, HQ_SYSTEM_AMO, TYPE_GURISTAS_LAUNDERING_FACILITY, 1);

    // 4. Outpost Raid Gate guards: GateNotFound, NotEnlistedInMilitia, WarpCoreStabilizerRestricted, ShipRestrictedForComplex, ShipNotInSpace
    let raid_gate_id = 88_900_001_i64;
    assert_eq!(
        activate_outpost_raid_gate_in_space(&ctx, char_id, ship_id, HQ_SYSTEM_AMO, raid_gate_id)
            .await
            .unwrap_err()
            .code,
        "GateNotFound"
    );
    ctx.db.with_mut_snapshot(|mut snap| {
        let mut ops = load_ops_state(snap);
        ops.outpost_raids.insert(
            raid_gate_id,
            OutpostRaidInstance {
                gate_id: raid_gate_id,
                beacon_id: 88_900_002,
                campaign_id: 701,
                solar_system_id: HQ_SYSTEM_AMO,
                pirate_faction_id: FACTION_GURISTAS,
                target_faction_id: 500_001,
                size: OutpostRaidSize::Small,
                is_static: false,
                occupants: Vec::new(),
                mordus_defender_ball_id: None,
                pirate_defender_ball_id: None,
                mordus_defender_cleared: false,
                pirate_defender_cleared: false,
                weighted_elapsed_secs: 0.0,
                last_step_unix: base,
                completed_on_cooldown: false,
                respawn_at_unix: 0,
            },
        );
        save_ops_state(&mut snap, &ops);
        assert!(is_outpost_raid_gate(snap, raid_gate_id));
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.war_faction_id = Some(500_003); // Third-party militia -> NotEnlistedInMilitia
        }
    });
    assert!(!is_warp_into_outpost_raid_pocket_blocked(
        &ctx, HQ_SYSTEM_AMO, [1e11, 0.0, 0.0], [0.0; 3], [0.0; 3],
    ));
    assert_eq!(
        activate_outpost_raid_gate_in_space(&ctx, char_id, ship_id, HQ_SYSTEM_AMO, raid_gate_id).await.unwrap_err().code,
        "NotEnlistedInMilitia"
    );
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.war_faction_id = Some(FACTION_GURISTAS);
        }
    });
    assert_eq!(
        activate_outpost_raid_gate_in_space(&ctx, char_id, ship_id, HQ_SYSTEM_AMO, raid_gate_id).await.unwrap_err().code,
        "ShipNotInSpace"
    );

    // Spawn ship far from gate -> TooFarFromGate; move onto gate + fit WCS -> WarpCoreStabilizerRestricted
    let amo_sys = ctx.systems.get_or_spawn(HQ_SYSTEM_AMO);
    let _ = amo_sys.spawn_ball_with_meta(
        Ball {
            id: ship_id, type_id: 587, position: DVec3::ZERO, velocity: DVec3::ZERO,
            max_velocity: 300.0, inertia: 2.0, mass: 1e6, radius: 40.0, speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
        ctx.build_ship_combat_state(char_id, ship_id, 587),
        ctx.build_ship_slim_meta(char_id, 587),
    ).await;
    assert_eq!(
        activate_outpost_raid_gate_in_space(&ctx, char_id, ship_id, HQ_SYSTEM_AMO, raid_gate_id).await.unwrap_err().code,
        "TooFarFromGate"
    );
    let _ = amo_sys.set_ball_position(ship_id, OutpostRaidSize::Small.gate_position()).await;

    let wcs_tid = ctx.sde.types.values().find(|t| t.group_id == 315).map(|t| t.type_id).unwrap_or(10998);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            88_900_010,
            InventoryItemRecord {
                item_id: 88_900_010, type_id: wcs_tid, owner_id: char_id, location_id: ship_id,
                flag: InventoryFlag::LoSlot0, quantity: 1, singleton: true, version: 1,
                dynamic_attributes: HashMap::new(), source_mutaplasmid_id: None,
                insured_payout_isk: 0.0, is_blueprint_copy: false, blueprint_runs_remaining: None,
            },
        );
    });
    assert_eq!(
        activate_outpost_raid_gate_in_space(&ctx, char_id, ship_id, HQ_SYSTEM_AMO, raid_gate_id)
            .await.unwrap_err().code,
        "WarpCoreStabilizerRestricted"
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.remove(&88_900_010);
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            ship.type_id = 670;
        }
    });
    assert_eq!(
        activate_outpost_raid_gate_in_space(&ctx, char_id, ship_id, HQ_SYSTEM_AMO, raid_gate_id)
            .await.unwrap_err().code,
        "ShipRestrictedForComplex"
    );
    let _ = (
        TYPE_ANGEL_INSURGENCY_DATA_SEQ, TYPE_GURISTAS_INSURGENCY_DATA_SEQ,
        TYPE_STATE_CI_OUTPOST_HUB, TYPE_UPWELL_CONSORTIUM_DATA_CACHE,
    );
}
