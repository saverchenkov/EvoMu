//! Edge-case behavioral tests for `router::context`.

use super::*;
use crate::services::character_expert_systems::ExpertSystemRecord;
use crate::services::combat_crimewatch::DroneSpaceState;
use crate::services::drone_test_helpers;
use crate::services::quasar_auth_user::QuasarIdentityStore;
use eve_db::{
    InventoryFlag, InventoryItemRecord, OwnerNoteRecord, SkillQueueEntry, TrainedSkillRecord,
    UpwellStructureRecord, VirtualServerClock, WorldRepository,
};
use eve_destiny::DamageVector;
use eve_dogma::attr_ids;
use eve_sde::{InvType, SdeRegistry};
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| {
        let mut sde = SdeRegistry::with_canonical_seed();
        // Ensure a predictable frigate hull (587), over-CPU hi-slot module (90001), and booster (90002)
        sde.types.entry(587).or_insert_with(|| InvType {
            type_id: 587,
            group_id: 25,
            category_id: 6,
            name: "Rifter".into(),
            mass: 1_067_000.0,
            volume: 27_000.0,
            capacity: 140.0,
            radius: 40.0,
            base_price: 500_000.0,
            published: true,
            portion_size: 1,
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
            tech_level: None,
            default_effect_id: None,
            description: String::new(),
            effect_ids: Vec::new(),
            default_attributes: HashMap::from([
                (attr_ids::LOW_SLOTS, 3.0),
                (attr_ids::MED_SLOTS, 3.0),
                (attr_ids::HI_SLOTS, 3.0),
                (attr_ids::RIG_SLOTS, 3.0),
                (attr_ids::CPU_OUTPUT, 120.0),
                (attr_ids::POWER_OUTPUT, 50.0),
            ]),
        });
        sde.types.insert(
            90001,
            InvType {
                type_id: 90001,
                group_id: 53,
                category_id: 7,
                name: "HugeCpuGun".into(),
                mass: 50.0,
                volume: 5.0,
                capacity: 0.0,
                radius: 5.0,
                base_price: 100.0,
                published: true,
                portion_size: 1,
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
                tech_level: None,
                default_effect_id: None,
                description: String::new(),
                effect_ids: vec![12],
                default_attributes: HashMap::from([
                    (attr_ids::CPU, 9999.0),
                    (attr_ids::POWER, 1.0),
                ]),
            },
        );
        Arc::new(sde)
    }))
}

fn edge_ctx(base_unix: u64) -> ServerContext {
    let ctx = ServerContext::with_sde_and_repository(
        shared_sde(),
        Arc::new(WorldRepository::with_demo_seed()),
    )
    .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(base_unix)));
    ctx
}

fn make_item(
    item_id: i64,
    type_id: u32,
    owner_id: i64,
    location_id: i64,
    flag: InventoryFlag,
) -> InventoryItemRecord {
    InventoryItemRecord {
        item_id,
        type_id,
        owner_id,
        location_id,
        flag,
        quantity: 1,
        singleton: true,
        version: 1,
        dynamic_attributes: HashMap::new(),
        source_mutaplasmid_id: None,
        insured_payout_isk: 0.0,
        is_blueprint_copy: false,
        blueprint_runs_remaining: None,
    }
}

#[test]
fn character_skill_levels_merges_active_expert_system_and_ignores_expired() {
    let ctx = edge_ctx(1_750_000_000);
    let char_id = 90_000_001;
    let now_ft = ctx.now_blueos_filetime_ticks();

    ctx.db.with_mut_snapshot(|snap| {
        let skills = snap.skills.entry(char_id).or_default();
        skills.clear();
        skills.insert(
            3300,
            TrainedSkillRecord {
                type_id: 3300,
                level: 2,
                skill_points: 1000.0,
            },
        );
        skills.insert(
            3301,
            TrainedSkillRecord {
                type_id: 3301,
                level: 4,
                skill_points: 10_000.0,
            },
        );
        let mut records = BTreeMap::new();
        // Active expert system: boosts 3300 to 4 (2->4), leaves 3301 at max(4,3)=4, adds brand-new 3302 at 3
        records.insert(
            60001_i64,
            ExpertSystemRecord {
                type_id: 60001,
                inserted_at_ft: now_ft - 1_000,
                expires_at_ft: now_ft + 1_000_000_000,
                skills_granted: BTreeMap::from([(3300, 4), (3301, 3), (3302, 3)]),
            },
        );
        // Expired expert system (expires_at_ft == now_ft): must NOT apply
        records.insert(
            60002_i64,
            ExpertSystemRecord {
                type_id: 60002,
                inserted_at_ft: now_ft - 2_000,
                expires_at_ft: now_ft,
                skills_granted: BTreeMap::from([(3300, 5), (3303, 5)]),
            },
        );
        snap.node_graph_state.expert_systems.insert(char_id, records.clone());
    });

    let levels = ctx.character_skill_levels(char_id);
    assert_eq!(levels.get(&3300), Some(&4));
    assert_eq!(levels.get(&3301), Some(&4));
    assert_eq!(levels.get(&3302), Some(&3));
    assert_eq!(levels.get(&3303), None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn module_reactivation_delay_tracks_system_sim_clock_and_cross_system_fallback() {
    let ctx = edge_ctx(1_750_000_000);
    let char_id = 90_000_001;
    let ship_id = 200_000_901;
    let mod_id = 200_000_902;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.structure_id = None;
            ch.solar_system_id = 30_000_142;
            ch.active_ship_id = ship_id;
        }
        snap.items.insert(
            ship_id,
            make_item(ship_id, 587, char_id, 30_000_142, InventoryFlag::None),
        );
        snap.items.insert(
            mod_id,
            make_item(mod_id, 90001, char_id, ship_id, InventoryFlag::HiSlot0),
        );
    });
    let _ = ctx.systems.get_or_spawn(30_000_142);

    // Zero/negative delay is a no-op
    ctx.record_module_reactivation_delay(ship_id, mod_id, 0.0);
    ctx.record_module_reactivation_delay(ship_id, mod_id, -2.0);
    assert!(!ctx.is_module_in_reactivation_delay(ship_id, mod_id));

    // Record 20s delay; shorter follow-up delay preserves longer remaining delay
    ctx.record_module_reactivation_delay(ship_id, mod_id, 20.0);
    ctx.record_module_reactivation_delay(ship_id, mod_id, 5.0);
    assert!(ctx.is_module_in_reactivation_delay(ship_id, mod_id));

    // Advance 10s without sim clock rebase -> still 10s left (does not emit DoSimClockRebase)
    let mut rx = ctx.notify_tx.subscribe();
    ctx.advance_secs_without_sim_clock_rebase(10);
    assert!(ctx.is_module_in_reactivation_delay(ship_id, mod_id));
    while let Ok((_, notif)) = rx.try_recv() {
        assert_ne!(
            notif.event, "DoSimClockRebase",
            "advance_secs_without_sim_clock_rebase must not emit DoSimClockRebase"
        );
    }

    // Move ship into a different system (entry.solar_system_id != Some(30_000_144)) -> wall fallback still tracks remaining 10s
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&ship_id).unwrap().location_id = 30_000_144;
    });
    assert!(ctx.is_module_in_reactivation_delay(ship_id, mod_id));

    // Advance another 11s -> total 21s elapsed > 20s -> delay expired
    ctx.advance_secs_without_sim_clock_rebase(11);
    assert!(!ctx.is_module_in_reactivation_delay(ship_id, mod_id));
}

#[test]
fn advance_skill_queues_edges_alpha_cap_omega_lapse_and_no_ops() {
    let base = 1_750_000_000;
    let ctx = edge_ctx(base);
    let char_id = 90_000_001;
    let alt_char_id = 90_000_002;
    let user_id = ctx
        .db
        .with_snapshot(|s| s.characters[&char_id].user_id);

    // Zero, negative, NaN elapsed_secs and empty queues are clean no-ops
    ctx.advance_skill_queues(0.0);
    ctx.advance_skill_queues(-10.0);
    ctx.advance_skill_queues(f64::NAN);

    // Paused queue character is skipped even with queue items present
    ctx.db.with_mut_snapshot(|snap| {
        snap.paused_skill_queues.insert(char_id);
        snap.skill_queues.insert(
            char_id,
            vec![SkillQueueEntry {
                type_id: 3300,
                target_level: 1,
                start_sp: 0.0,
                end_sp: 250.0,
                duration_seconds: 500.0,
                started_at_unix: Some(base),
            }],
        );
    });
    let sp_before = ctx.db.with_snapshot(|s| s.characters[&char_id].skill_points);
    ctx.advance_secs_without_sim_clock_rebase(120);
    assert_eq!(
        ctx.db.with_snapshot(|s| s.characters[&char_id].skill_points),
        sp_before
    );

    // Unpause, set Alpha with >= 5_000_000 SP -> pauses immediately & emits OnSkillQueuePausedServer
    ctx.db
        .acquire_online_lock(user_id, 9_001, Some(char_id), true, base)
        .unwrap();
    ctx.pending_notifications.write().unwrap().clear();
    ctx.db.with_mut_snapshot(|snap| {
        snap.paused_skill_queues.remove(&char_id);
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.clone_grade = 1;
        ch.skill_points = 5_000_000.0;
    });
    ctx.advance_secs_without_sim_clock_rebase(60);
    assert!(ctx
        .db
        .with_snapshot(|s| s.paused_skill_queues.contains(&char_id)));
    let events: Vec<String> = ctx
        .pending_notifications
        .read()
        .unwrap()
        .get(&char_id)
        .map(|v| v.iter().map(|n| n.event.clone()).collect())
        .unwrap_or_default();
    assert!(events.contains(&"OnSkillQueuePausedServer".to_string()));

    // Omega lapse mid-interval: expires at base + 100 while advancing 300s
    // Both char_id and alt_char_id on user_id downgrade clone_grade to 1 and pause
    let ctx2 = edge_ctx(base);
    ctx2.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.clone_grade = 2;
            ch.skill_points = 10_000.0;
        }
        let mut alt = snap.characters[&char_id].clone();
        alt.character_id = alt_char_id;
        alt.user_id = user_id;
        alt.clone_grade = 2;
        snap.characters.insert(alt_char_id, alt);

        let mut store = QuasarIdentityStore::default();
        store.omega_subscriptions.insert(user_id, base + 100);
        snap.owner_notes.insert(
            -8_500_000_000_000,
            OwnerNoteRecord {
                note_id: -8_500_000_000_000,
                character_id: -85,
                item_id: 0,
                label: "quasar_identity".into(),
                note: serde_json::to_string(&store).unwrap(),
                timestamp_unix: base,
            },
        );
        snap.skill_queues.insert(
            char_id,
            vec![SkillQueueEntry {
                type_id: 3300,
                target_level: 3,
                start_sp: 0.0,
                end_sp: 8_000.0,
                duration_seconds: 1000.0,
                started_at_unix: Some(base),
            }],
        );
    });
    ctx2.advance_secs_without_sim_clock_rebase(300);
    ctx2.db.with_snapshot(|snap| {
        assert_eq!(snap.characters[&char_id].clone_grade, 1);
        assert_eq!(snap.characters[&alt_char_id].clone_grade, 1);
        assert!(snap.paused_skill_queues.contains(&char_id));
        assert!(snap.paused_skill_queues.contains(&alt_char_id));
        // Only trained ~100s (not 300s) before Omega lapsed
        let gained = snap.characters[&char_id].skill_points - 10_000.0;
        assert!(gained > 0.0 && gained < 2_000.0, "gained={gained}");
    });
}

#[test]
fn get_or_rebuild_ship_fitting_cache_drones_structure_and_auto_offline() {
    let ctx = edge_ctx(1_750_000_000);
    let char_id = 90_000_001;
    let ship_id = 200_000_910;
    let hi_mod_id = 200_000_911;
    let drone_id = 200_000_912;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ship_id,
            make_item(ship_id, 587, char_id, 60_003_760, InventoryFlag::Hangar),
        );
        // Over-CPU module (needs 9999 CPU on Rifter 120 CPU) will auto-offline during rebuild
        snap.items.insert(
            hi_mod_id,
            make_item(hi_mod_id, 90001, char_id, ship_id, InventoryFlag::HiSlot0),
        );
        snap.module_online_states.insert(hi_mod_id, true);
        snap.module_active_states.insert(hi_mod_id);
    });

    // Active vs abandoned space drones + avoiding duplicate drone_item_id
    drone_test_helpers::seed_active_drones(
        &ctx,
        [
            DroneSpaceState {
                ball_id: 9_001,
                drone_item_id: drone_id,
                type_id: 2454,
                group_id: 100,
                owner_char_id: char_id,
                original_owner_char_id: char_id,
                parent_ship_id: ship_id,
                solar_system_id: 30_000_142,
                target_id: None,
                activity_state: 0,
                assist_char_id: None,
                guard_char_id: None,
                is_abandoned: false,
                is_sentry: false,
                bandwidth_used: 5.0,
                max_velocity: 1000.0,
                optimal_range: 5000.0,
                damage_vector: DamageVector::default(),
                shield_repair_amount: 0.0,
                armor_repair_amount: 0.0,
                hull_repair_amount: 0.0,
                web_speed_factor: 1.0,
                neut_amount: 0.0,
                ecm_strength: 0.0,
                tp_sig_bonus_pct: 0.0,
            },
            DroneSpaceState {
                ball_id: 9_002,
                drone_item_id: 200_000_913,
                type_id: 2454,
                group_id: 100,
                owner_char_id: char_id,
                original_owner_char_id: char_id,
                parent_ship_id: ship_id,
                solar_system_id: 30_000_142,
                target_id: None,
                activity_state: 0,
                assist_char_id: None,
                guard_char_id: None,
                is_abandoned: true,
                is_sentry: false,
                bandwidth_used: 5.0,
                max_velocity: 1000.0,
                optimal_range: 5000.0,
                damage_vector: DamageVector::default(),
                shield_repair_amount: 0.0,
                armor_repair_amount: 0.0,
                hull_repair_amount: 0.0,
                web_speed_factor: 1.0,
                neut_amount: 0.0,
                ecm_strength: 0.0,
                tp_sig_bonus_pct: 0.0,
            },
        ],
    );

    let fit = ctx.get_or_rebuild_ship_fitting(char_id, ship_id).unwrap();
    // Over-CPU module was auto-offlined & deactivated in snapshot
    assert_eq!(
        fit.modules[&hi_mod_id].state,
        eve_dogma::ModuleState::Offline
    );
    ctx.db.with_snapshot(|s| {
        assert_eq!(s.module_online_states.get(&hi_mod_id), Some(&false));
        assert!(!s.module_active_states.contains(&hi_mod_id));
    });
    // Non-abandoned drone added by both item_id and ball_id; abandoned drone excluded
    assert!(fit.drones.iter().any(|&(id, _)| id == drone_id));
    assert!(fit.drones.iter().any(|&(id, _)| id == 9_001));
    assert!(!fit.drones.iter().any(|&(id, _)| id == 9_002));

    // Cache hit with booster item change triggers rebuild
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            200_000_920,
            make_item(200_000_920, 90002, char_id, char_id, InventoryFlag::Booster),
        );
    });
    let rebuilt = ctx.get_or_rebuild_ship_fitting(char_id, ship_id).unwrap();
    assert_eq!(rebuilt.boosters, vec![90002]);

    // Structure fallback: Upwell structure owned by corp resolves even when owner_id != char_id
    let struct_id = 1_000_555_001;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            struct_id,
            make_item(struct_id, 587, 98_000_001, 30_000_142, InventoryFlag::None),
        );
        snap.structures.insert(
            struct_id,
            UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 587,
                owner_corp_id: 98_000_001,
                solar_system_id: 30_000_142,
                name: "TestCitadel".into(),
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                shield_hp: 1000.0,
                armor_hp: 1000.0,
                hull_hp: 1000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
    });
    let sfit = ctx.get_or_rebuild_ship_fitting(char_id, struct_id).unwrap();
    assert_eq!(sfit.ship.item_id, struct_id);

    ctx.invalidate_ship_fitting(struct_id);
    assert!(!ctx.ship_fittings.read().unwrap().contains_key(&struct_id));
}

#[tokio::test]
async fn now_system_sim_filetime_ticks_stays_client_anchored_with_tidi_and_advancetime() {
    use crate::services::serverops_cluster::{
        clear_manual_system_tidi_override, emit_sim_clock_rebase_on_entry_if_needed,
        set_system_tidi_factor,
    };

    let base = 1_750_000_000_u64;
    let ctx = edge_ctx(base);
    let char_id = 90_000_001_i64;
    let sys_a = 30_000_142_u32;
    let sys_b = 30_000_144_u32;
    let user_id = ctx.db.with_snapshot(|s| s.characters[&char_id].user_id);
    ctx.db
        .acquire_online_lock(user_id, 9_101, Some(char_id), true, base)
        .unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.structure_id = None;
            ch.solar_system_id = sys_a;
        }
    });
    let _ = ctx.systems.get_or_spawn(sys_a);
    let _ = ctx.systems.get_or_spawn(sys_b);

    let max_skew_ticks: i64 = 5 * 10_000_000;
    let day_ticks: i64 = 86_400 * 10_000_000;
    let jump_secs = 1_119_223_u64; // ~12.95 days (matches live #1143/#1144 skew)

    // Order A: `/advancetime` first, then TiDi tighten -> relax leaves `sim_clock_anchoring` at 1.0x.
    ctx.advance_secs_without_sim_clock_rebase(jump_secs);
    let _ = ctx.drain_notifications_for_char(char_id);
    set_system_tidi_factor(&ctx, sys_a, 0.50);
    clear_manual_system_tidi_override(&ctx, sys_a);
    let _ = emit_sim_clock_rebase_on_entry_if_needed(&ctx, char_id, sys_a);

    let client_ft = ctx.now_machonet_wallclock_filetime_ticks();
    assert!(
        ctx.now_blueos_filetime_ticks() - client_ft > 12 * day_ticks,
        "server virtual clock must lead client clock by ~12.95 days"
    );
    let sim_a_1 = ctx.now_system_sim_filetime_ticks(Some(sys_a));
    let sim_a_2 = ctx.now_system_sim_filetime_ticks(Some(sys_a));
    assert!(
        (sim_a_1 - client_ft).abs() < max_skew_ticks && sim_a_1 - client_ft < day_ticks,
        "anchored sim clock in sys_a drifted from client clock: delta_ms={}",
        (sim_a_1 - client_ft) / 10_000
    );
    assert!(sim_a_2 > sim_a_1, "sim activation timestamps must stay monotonic");
    let rebases: Vec<_> = ctx
        .drain_notifications_for_char(char_id)
        .into_iter()
        .filter(|n| n.event == "DoSimClockRebase")
        .collect();
    assert!(!rebases.is_empty(), "expected DoSimClockRebase notifications for sys_a");
    for n in rebases {
        if let Some(eve_proto::PyRep::Tuple(times)) = n.args.first() {
            for t in times {
                let ts = t.as_i64().expect("DoSimClockRebase FILETIME");
                assert!(
                    (ts - client_ft).abs() < max_skew_ticks && ts - client_ft < day_ticks,
                    "DoSimClockRebase timestamp drifted >24h into client future"
                );
            }
        }
    }

    // Order B: TiDi anchored first on sys_b, then additional `/advancetime` jump afterwards.
    set_system_tidi_factor(&ctx, sys_b, 0.50);
    clear_manual_system_tidi_override(&ctx, sys_b);
    ctx.advance_secs_without_sim_clock_rebase(jump_secs);
    let client_ft_after = ctx.now_machonet_wallclock_filetime_ticks();
    let sim_b_1 = ctx.now_system_sim_filetime_ticks(Some(sys_b));
    let sim_b_2 = ctx.now_system_sim_filetime_ticks(Some(sys_b));
    assert!(
        (sim_b_1 - client_ft_after).abs() < max_skew_ticks && sim_b_1 - client_ft_after < day_ticks,
        "pre-anchored sys_b drifted across /advancetime: delta_ms={}",
        (sim_b_1 - client_ft_after) / 10_000
    );
    assert!(sim_b_2 > sim_b_1);
}

#[tokio::test]
async fn rpc_rate_budget_default_enabled_and_test_disable_toggle() {
    use eve_machonet::{service_roles, SessionState};
    let edge_default = edge_ctx(1_750_000_000);
    assert_eq!(edge_default.rpc_rate_limit_policy(), RpcRateLimitPolicy::Default);
    let default_server = ServerContext::new();
    assert_eq!(default_server.rpc_rate_limit_policy(), RpcRateLimitPolicy::Default);

    let ctx_a = crate::router::TestContext::from_server(edge_default);
    let ctx_b = crate::router::TestContext::new();
    assert!(!ctx_a.is_rpc_rate_budget_disabled());
    assert!(!ctx_b.is_rpc_rate_budget_disabled());
    assert_eq!(ctx_b.rpc_rate_limit_policy(), RpcRateLimitPolicy::Default);

    let t0 = std::time::Instant::now();
    let sid_unit = 9_501_u64;
    let burst = RPC_RATE_BURST_TOKENS as usize;
    for i in 0..burst {
        assert!(
            ctx_a.try_consume_rpc_token_at(sid_unit, t0, false),
            "default context must allow burst token {i}"
        );
        assert!(
            ctx_b.try_consume_rpc_token_at(sid_unit, t0, false),
            "default TestContext::new() must allow burst token {i}"
        );
    }
    assert!(
        !ctx_a.try_consume_rpc_token_at(sid_unit, t0, false),
        "default edge_ctx must reject token consumption past burst"
    );
    assert!(
        !ctx_b.try_consume_rpc_token_at(sid_unit, t0, false),
        "default TestContext::new() must reject token consumption past burst"
    );

    let char_id = 90_000_001_i64;
    let make_sess = |ctx: &ServerContext, session_id: u64| {
        let (uid, corp_id, sol_id, station_id, ship_id) = ctx.db.with_snapshot(|snap| {
            let ch = &snap.characters[&char_id];
            (
                ch.user_id,
                ch.corporation_id,
                ch.solar_system_id,
                ch.station_id,
                ch.active_ship_id,
            )
        });
        let mut s = SessionState {
            session_id,
            user_id: Some(uid),
            character_id: Some(char_id),
            corporation_id: Some(corp_id),
            solar_system_id: Some(sol_id),
            station_id,
            ship_id: Some(ship_id),
            role: service_roles::ROLE_PLAYER,
            ..Default::default()
        };
        ctx.sync_session(&mut s);
        s
    };

    let kw = HashMap::new();
    let mut sess_a = make_sess(&ctx_a, 9_502);
    let mut saw_rate_limited = false;
    for _ in 0..(burst + 150) {
        if let Err(err) = ctx_a
            .dispatch_rpc(&mut sess_a, "account", None, "GetCashBalance", &[], &kw, None)
            .await
        {
            assert_eq!(err.code, "RateLimited");
            saw_rate_limited = true;
            break;
        }
    }
    assert!(saw_rate_limited, "default edge_ctx must enforce RateLimited in dispatch_rpc");

    let mut sess_b = make_sess(&ctx_b, 9_503);
    let mut saw_rate_limited_b = false;
    for _ in 0..(burst + 150) {
        if let Err(err) = ctx_b
            .dispatch_rpc(&mut sess_b, "account", None, "GetCashBalance", &[], &kw, None)
            .await
        {
            assert_eq!(err.code, "RateLimited");
            saw_rate_limited_b = true;
            break;
        }
    }
    assert!(saw_rate_limited_b, "default TestContext::new() must enforce RateLimited in dispatch_rpc");

    // Construction-time Unlimited TestContexts bypass rate budgeting without inserting buckets.
    let ctx_unlim = crate::router::TestContext::new_with_rpc_rate_budget_disabled();
    let ctx_unlim_sde = crate::router::TestContext::with_sde_and_repository_rate_budget_disabled(
        shared_sde(),
        Arc::new(WorldRepository::with_demo_seed()),
    );
    assert!(ctx_unlim.is_rpc_rate_budget_disabled());
    assert!(ctx_unlim_sde.is_rpc_rate_budget_disabled());
    assert_eq!(ctx_unlim.rpc_rate_limit_policy(), RpcRateLimitPolicy::Unlimited);
    assert_eq!(ctx_unlim_sde.rpc_rate_limit_policy(), RpcRateLimitPolicy::Unlimited);
    assert_eq!(ctx_unlim.rpc_rate_bucket_count(), 0);
    assert_eq!(ctx_unlim_sde.rpc_rate_bucket_count(), 0);

    for i in 0..(burst + 100) {
        assert!(
            ctx_unlim.try_consume_rpc_token_at(sid_unit, t0, false),
            "unlimited TestContext must allow token {i} past burst"
        );
        assert!(
            ctx_unlim_sde.try_consume_rpc_token_at(9_599, t0, false),
            "unlimited with_sde TestContext must allow token {i} past burst"
        );
    }
    assert_eq!(
        ctx_unlim.rpc_rate_bucket_count(),
        0,
        "Unlimited policy must not allocate or insert rate buckets"
    );
    assert_eq!(
        ctx_unlim_sde.rpc_rate_bucket_count(),
        0,
        "Unlimited policy must not allocate or insert rate buckets"
    );

    let mut sess_unlim = make_sess(&ctx_unlim, 9_504);
    for _ in 0..(burst + 50) {
        ctx_unlim
            .dispatch_rpc(&mut sess_unlim, "account", None, "GetCashBalance", &[], &kw, None)
            .await
            .expect("Unlimited TestContext must allow dispatch_rpc beyond burst");
    }
    assert_eq!(ctx_unlim.rpc_rate_bucket_count(), 0);

    // Weak downgrade/upgrade preserves the immutable Unlimited policy and bypasses buckets.
    let upgraded = ctx_unlim.downgrade().upgrade().expect("upgrade ServerContext");
    assert_eq!(upgraded.rpc_rate_limit_policy(), RpcRateLimitPolicy::Unlimited);
    assert!(upgraded.try_consume_rpc_token_at(sid_unit, t0, false));
    assert_eq!(upgraded.rpc_rate_bucket_count(), 0);
    upgraded
        .dispatch_rpc(&mut sess_unlim, "account", None, "GetCashBalance", &[], &kw, None)
        .await
        .expect("upgraded Weak ServerContext must preserve Unlimited rate policy");

    let upgraded_sde = ctx_unlim_sde
        .downgrade()
        .upgrade()
        .expect("upgrade sde ServerContext");
    assert_eq!(upgraded_sde.rpc_rate_limit_policy(), RpcRateLimitPolicy::Unlimited);
    assert!(upgraded_sde.try_consume_rpc_token_at(9_600, t0, false));
    assert_eq!(upgraded_sde.rpc_rate_bucket_count(), 0);

    // Sibling default TestContext constructed alongside Unlimited remains strictly rate-limited.
    let ctx_sibling_default = crate::router::TestContext::new();
    let ctx_explicit_default =
        crate::router::TestContext::new_with_rpc_rate_limit_policy(RpcRateLimitPolicy::Default);
    let ctx_sde_default = crate::router::TestContext::with_sde_and_repository_rate_limit(
        shared_sde(),
        Arc::new(WorldRepository::with_demo_seed()),
        RpcRateLimitPolicy::Default,
    );
    assert!(!ctx_sibling_default.is_rpc_rate_budget_disabled());
    assert!(!ctx_explicit_default.is_rpc_rate_budget_disabled());
    assert!(!ctx_sde_default.is_rpc_rate_budget_disabled());
    assert_eq!(ctx_sibling_default.rpc_rate_limit_policy(), RpcRateLimitPolicy::Default);
    assert_eq!(ctx_explicit_default.rpc_rate_limit_policy(), RpcRateLimitPolicy::Default);
    assert_eq!(ctx_sde_default.rpc_rate_limit_policy(), RpcRateLimitPolicy::Default);
    for _ in 0..burst {
        assert!(ctx_sibling_default.try_consume_rpc_token_at(sid_unit, t0, false));
    }
    assert!(!ctx_sibling_default.try_consume_rpc_token_at(sid_unit, t0, false));
}

#[tokio::test]
async fn anchor_system_sim_clock_offset_diverges_wall_and_sim_domains_at_tidi_1() {
    use crate::services::serverops_cluster::set_system_tidi_factor;
    use eve_db::clock::{SimFileTime, WallFileTime, FILETIME_TICKS_PER_MS};

    let base = 1_750_000_000_u64;
    let ctx = edge_ctx(base);
    let char_id = 90_000_001_i64;
    let sys_id = 30_000_142_u32;
    let user_id = ctx.db.with_snapshot(|s| s.characters[&char_id].user_id);
    ctx.db
        .acquire_online_lock(user_id, 9_102, Some(char_id), true, base)
        .unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.structure_id = None;
            ch.solar_system_id = sys_id;
        }
    });

    // Anchor a non-zero (sim_time - wallclock) offset (-300_000 ms) while TiDi remains 1.0x.
    let offset_ms = -300_000_i64;
    let sim_clock = ctx.anchor_system_sim_clock_with_offset_ms(sys_id, offset_ms, 1.0);
    assert_eq!(sim_clock.sim_minus_wall_offset_ms(), Some(offset_ms));
    assert!((sim_clock.tidi_factor() - 1.0).abs() < 1e-9);

    let wall_0: WallFileTime = ctx.now_machonet_wall_filetime();
    let sim_0: SimFileTime = ctx
        .peek_system_sim_filetime(sys_id)
        .expect("anchored system must expose peek_system_sim_filetime");
    assert_eq!(sim_clock.last_wall_filetime(), Some(wall_0));
    assert_eq!(sim_clock.sim_filetime(), sim_0);
    assert_eq!(
        sim_0.as_ticks() - wall_0.as_ticks(),
        offset_ms * FILETIME_TICKS_PER_MS,
        "SimFileTime must trail WallFileTime by -300_000 ms even at TiDi = 1.0"
    );

    // Anchoring with a non-1.0 TiDi factor synchronizes manual_tidi_overrides and live SystemHandle.
    let tidi_sys = 30_000_144_u32;
    let tidi_clock = ctx.anchor_system_sim_clock_with_offset_ms(tidi_sys, offset_ms, 0.25);
    assert_eq!(tidi_clock.last_wall_filetime(), Some(wall_0));
    assert_eq!(
        Some(tidi_clock.sim_filetime()),
        ctx.peek_system_sim_filetime(tidi_sys)
    );
    assert!((tidi_clock.tidi_factor() - 0.25).abs() < 1e-9);
    assert!(
        (crate::services::serverops_cluster::get_system_tidi_factor(&ctx, tidi_sys) - 0.25).abs()
            < 1e-9
    );
    assert!(
        (ctx.systems.get_or_spawn(tidi_sys).tidi_factor() - 0.25).abs() < 1e-9,
        "lazy-spawned SystemHandle must inherit 0.25 TiDi from anchored manual_tidi_overrides"
    );
    // Re-anchoring an already-spawned system updates its live SystemHandle TiDi factor cleanly.
    let _ = ctx.anchor_system_sim_clock_with_offset_ms(tidi_sys, offset_ms, 0.50);
    assert!(
        (ctx.systems.get_handle(tidi_sys).unwrap().tidi_factor() - 0.50).abs() < 1e-9
    );

    // Advance 10 real seconds at TiDi = 1.0: both clocks advance +10s preserving the -300_000 ms offset.
    ctx.advance_secs(10);
    let wall_1 = ctx.now_machonet_wall_filetime();
    let sim_1 = ctx.now_system_sim_filetime_for_char(char_id);
    assert_eq!(wall_1, wall_0.saturating_add_secs(10));
    assert_eq!(sim_1, sim_0.saturating_add_secs(10));
    assert_eq!(sim_1.as_ticks() - wall_1.as_ticks(), offset_ms * FILETIME_TICKS_PER_MS);

    // Unrebased /advancetime keeps MachoNet wallclock steady while translating virtual expiry into
    // client-aligned WallFileTime vs anchored SimFileTime domains without mixing them.
    ctx.advance_secs_without_sim_clock_rebase(60);
    assert_eq!(ctx.now_machonet_wall_filetime(), wall_1);
    let virtual_expiry = ctx.now_unix_secs() + 45;
    let wall_exp = ctx.virtual_unix_to_machonet_wall_filetime(virtual_expiry);
    let sim_exp = ctx.virtual_unix_expiry_to_sim_filetime_for_char(char_id, virtual_expiry);
    assert_eq!(wall_exp, wall_1.saturating_add_secs(45));
    assert_eq!(sim_exp, sim_1.saturating_add_secs(45));
    assert_eq!(sim_exp.as_ticks() - wall_exp.as_ticks(), offset_ms * FILETIME_TICKS_PER_MS);

    // Lowering TiDi to 0.50 advances SimFileTime by half wall time while preserving monotonicity.
    let applied = set_system_tidi_factor(&ctx, sys_id, 0.50);
    assert!((applied - 0.50).abs() < 1e-9);
    ctx.advance_secs(20);
    let wall_2 = ctx.now_machonet_wall_filetime();
    let sim_2_peek = ctx.peek_system_sim_filetime(sys_id).expect("anchored sim clock");
    let sim_2 = ctx.now_system_sim_filetime_for_char(char_id);
    assert_eq!(wall_2, wall_1.saturating_add_secs(20));
    assert_eq!(sim_2_peek, sim_1.saturating_add_secs(10));
    assert_eq!(sim_2, sim_2_peek);
    assert_eq!(
        sim_2.as_ticks() - wall_2.as_ticks(),
        (offset_ms - 10_000) * FILETIME_TICKS_PER_MS
    );
    // At 0.50x TiDi, remaining wall duration (+40s) maps to +20s in SimFileTime.
    let sim_exp_tidi =
        ctx.virtual_unix_expiry_to_sim_filetime_for_char(char_id, ctx.now_unix_secs() + 40);
    assert_eq!(sim_exp_tidi, sim_2.saturating_add_secs(20));
}

#[tokio::test]
async fn unanchored_and_anchored_invuln_notifications_track_virtual_server_clock() {
    use eve_db::clock::FILETIME_TICKS_PER_MS;
    use eve_machonet::SessionState;

    let base = 1_800_000_000_u64;
    let ctx = edge_ctx(base);
    ctx.advance_secs(10);

    // 1. Unanchored system: both OnInvulunOnUndockingUpdated and OnRestoringInvulnUpdated
    // track VirtualServerClock (now_machonet_wall_filetime) instead of host SystemTime::now().
    let sys_unanchored = 30_000_142_u32;
    let station_a = 60_003_760_i64;
    let char_a = 90_000_001_i64;
    let ship_a = 910_000_101_i64;
    let (user_a, corp_a) = ctx.db.with_snapshot(|s| {
        let c = &s.characters[&char_a];
        (c.user_id, c.corporation_id)
    });
    ctx.db
        .acquire_online_lock(user_a, 9_201, Some(char_a), true, ctx.now_unix_secs())
        .unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_a) {
            ch.station_id = Some(station_a);
            ch.structure_id = None;
            ch.solar_system_id = sys_unanchored;
            ch.active_ship_id = ship_a;
        }
        snap.items.insert(
            ship_a,
            make_item(ship_a, 587, char_a, station_a, InventoryFlag::Hangar),
        );
    });
    let mut sess_a = SessionState::new(9_201);
    sess_a.user_id = Some(user_a);
    sess_a.character_id = Some(char_a);
    sess_a.corporation_id = Some(corp_a);
    sess_a.solar_system_id = Some(sys_unanchored);
    sess_a.station_id = Some(station_a);
    sess_a.ship_id = Some(ship_a);

    crate::services::station_structure::handle(
        &ctx,
        &mut sess_a,
        "stationSvc",
        "Undock",
        &[],
        &HashMap::new(),
    )
    .await
    .expect("handled")
    .expect("unanchored undock ok");

    let wall_a0 = ctx.now_machonet_wall_filetime();
    let expected_undock_end_a = wall_a0.saturating_add_secs(30).as_ticks();
    let notifs_a = ctx.drain_notifications_for_char(char_a);
    let restoring_a0 = notifs_a
        .iter()
        .find(|n| n.event == "OnRestoringInvulnUpdated")
        .expect("unanchored OnRestoringInvulnUpdated");
    assert_eq!(restoring_a0.args[1].as_i64(), Some(expected_undock_end_a));
    let undocking_a0 = notifs_a
        .iter()
        .find(|n| n.event == "OnInvulunOnUndockingUpdated")
        .expect("unanchored OnInvulunOnUndockingUpdated");
    assert_eq!(undocking_a0.args[1].as_i64(), Some(expected_undock_end_a));

    ctx.advance_secs(25);
    let wall_a1 = ctx.now_machonet_wall_filetime();
    assert_eq!(wall_a1, wall_a0.saturating_add_secs(25));
    crate::services::notify_emitters_seasons_tactical::emit_restoring_invuln_updated(
        &ctx,
        char_a,
        ship_a,
        150_000_000,
    );
    let notifs_a1 = ctx.drain_notifications_for_char(char_a);
    let restoring_a1 = notifs_a1
        .iter()
        .find(|n| n.event == "OnRestoringInvulnUpdated")
        .expect("unanchored OnRestoringInvulnUpdated after advance_secs");
    assert_eq!(
        restoring_a1.args[1].as_i64(),
        Some(wall_a1.as_ticks() + 150_000_000)
    );

    ctx.advance_secs_without_sim_clock_rebase(300);
    assert_eq!(ctx.now_machonet_wall_filetime(), wall_a1);
    crate::services::notify_emitters_seasons_tactical::emit_restoring_invuln_updated(
        &ctx,
        char_a,
        ship_a,
        150_000_000,
    );
    let notifs_a2 = ctx.drain_notifications_for_char(char_a);
    let restoring_a2 = notifs_a2
        .iter()
        .find(|n| n.event == "OnRestoringInvulnUpdated")
        .expect("unanchored OnRestoringInvulnUpdated after unrebased advance");
    assert_eq!(
        restoring_a2.args[1].as_i64(),
        Some(wall_a1.as_ticks() + 150_000_000)
    );

    // 2. Offset-anchored system (-300_000 ms): both notifications shift by the system's anchored
    // SimFileTime offset and track subsequent VirtualServerClock progression.
    let sys_anchored = 30_000_144_u32;
    let station_b = 60_003_761_i64;
    let char_b = 90_000_777_i64;
    let ship_b = 910_000_777_i64;
    let offset_ms = -300_000_i64;
    let _ = ctx.anchor_system_sim_clock_with_offset_ms(sys_anchored, offset_ms, 1.0);
    ctx.advance_secs(1);
    ctx.db.with_mut_snapshot(|snap| {
        let mut acc = snap
            .accounts
            .values()
            .find(|a| a.user_id == user_a)
            .expect("user_a account")
            .clone();
        acc.user_id = char_b as u64;
        acc.username = format!("invuln_{char_b}");
        snap.accounts.insert(acc.username.clone(), acc);

        let mut ch = snap.characters[&char_a].clone();
        ch.character_id = char_b;
        ch.user_id = char_b as u64;
        ch.name = "Anchored Invuln Pilot".into();
        ch.station_id = Some(station_b);
        ch.structure_id = None;
        ch.solar_system_id = sys_anchored;
        ch.active_ship_id = ship_b;
        snap.characters.insert(char_b, ch);
        snap.items.insert(
            ship_b,
            make_item(ship_b, 587, char_b, station_b, InventoryFlag::Hangar),
        );
    });
    ctx.db
        .acquire_online_lock(char_b as u64, 9_202, Some(char_b), true, ctx.now_unix_secs())
        .unwrap();
    let mut sess_b = SessionState::new(9_202);
    sess_b.user_id = Some(char_b as u64);
    sess_b.character_id = Some(char_b);
    sess_b.corporation_id = Some(corp_a);
    sess_b.solar_system_id = Some(sys_anchored);
    sess_b.station_id = Some(station_b);
    sess_b.ship_id = Some(ship_b);

    crate::services::station_structure::handle(
        &ctx,
        &mut sess_b,
        "stationSvc",
        "Undock",
        &[],
        &HashMap::new(),
    )
    .await
    .expect("handled")
    .expect("anchored undock ok");

    let wall_b0 = ctx.now_machonet_wall_filetime();
    let sim_b0 = ctx
        .peek_system_sim_filetime(sys_anchored)
        .expect("anchored sys sim time");
    assert_eq!(
        sim_b0.as_ticks() - wall_b0.as_ticks(),
        offset_ms * FILETIME_TICKS_PER_MS
    );
    let expected_undock_end_b = sim_b0.saturating_add_secs(30).as_ticks();
    let notifs_b = ctx.drain_notifications_for_char(char_b);
    let restoring_b0 = notifs_b
        .iter()
        .find(|n| n.event == "OnRestoringInvulnUpdated")
        .expect("anchored OnRestoringInvulnUpdated");
    assert_eq!(restoring_b0.args[1].as_i64(), Some(expected_undock_end_b));
    let undocking_b0 = notifs_b
        .iter()
        .find(|n| n.event == "OnInvulunOnUndockingUpdated")
        .expect("anchored OnInvulunOnUndockingUpdated");
    // Same-tick monotonic #1081 clamp increments +1 tick per same-millisecond sim-clock read in Undock.
    let undocking_b0_ts = undocking_b0
        .args[1]
        .as_i64()
        .expect("anchored OnInvulunOnUndockingUpdated timestamp");
    assert!(
        (expected_undock_end_b..=expected_undock_end_b + 5).contains(&undocking_b0_ts),
        "anchored OnInvulunOnUndockingUpdated ({undocking_b0_ts}) must match sim_b0+30s ({expected_undock_end_b}) within same-tick #1081 monotonic clamp"
    );

    ctx.advance_secs(12);
    let sim_b1 = ctx
        .peek_system_sim_filetime(sys_anchored)
        .expect("anchored sys sim time after advance");
    assert_eq!(sim_b1, sim_b0.saturating_add_secs(12));
    crate::services::notify_emitters_seasons_tactical::emit_restoring_invuln_updated(
        &ctx,
        char_b,
        ship_b,
        200_000_000,
    );
    let notifs_b1 = ctx.drain_notifications_for_char(char_b);
    let restoring_b1 = notifs_b1
        .iter()
        .find(|n| n.event == "OnRestoringInvulnUpdated")
        .expect("anchored OnRestoringInvulnUpdated after advance");
    assert_eq!(
        restoring_b1.args[1].as_i64(),
        Some(sim_b1.as_ticks() + 200_000_000)
    );
}

#[tokio::test]
async fn pure_1x_offset_anchor_does_not_freeze_reactivation_cooldowns_after_step_manual() {
    let base = 1_800_000_000_u64;
    let ctx = edge_ctx(base);
    let char_id = 90_000_001_i64;
    let sys_id = 30_000_142_u32;
    let ship_id = 910_196_101_i64;
    let mod_id = 910_196_102_i64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.structure_id = None;
            ch.solar_system_id = sys_id;
            ch.active_ship_id = ship_id;
        }
        snap.items.insert(
            ship_id,
            make_item(ship_id, 587, char_id, i64::from(sys_id), InventoryFlag::None),
        );
        snap.items.insert(
            mod_id,
            make_item(mod_id, 90001, char_id, ship_id, InventoryFlag::HiSlot0),
        );
    });

    // Anchor default ScenarioBuilder -300_000 ms offset at 1.0x and advance physics once (`step_manual`)
    // so `cur_sys_sim_s > 0.0`, then record a 10s reactivation delay.
    let _ = ctx.anchor_system_sim_clock_with_offset_ms(sys_id, -300_000, 1.0);
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    sys_h.step_manual(1.0).await.unwrap();
    assert_eq!(
        ctx.resolve_sys_tidi_mode(Some(sys_id)),
        (1.0, false),
        "pure 1.0x offset anchor must not mark tidi_history_systems"
    );

    ctx.record_module_reactivation_delay(ship_id, mod_id, 10.0);
    assert!(ctx.is_module_in_reactivation_delay(ship_id, mod_id));

    // Advance virtual time via ctx.advance_secs (without stepping physics): remaining cooldown must shrink
    // and expire after 11s instead of freezing or pushing expiry forward.
    let exp_initial = ctx
        .resolve_active_reactivation_cooldown(ship_id, mod_id)
        .expect("initial active cooldown");
    ctx.advance_secs(5);
    assert!(ctx.is_module_in_reactivation_delay(ship_id, mod_id));
    let exp_after_5s = ctx
        .resolve_active_reactivation_cooldown(ship_id, mod_id)
        .expect("remaining cooldown at +5s");
    assert!(
        (exp_after_5s - exp_initial).abs() < 1e-6,
        "expiry timestamp must stay constant across ctx.advance_secs instead of drifting forward"
    );
    ctx.advance_secs(6);
    assert!(
        !ctx.is_module_in_reactivation_delay(ship_id, mod_id),
        "reactivation cooldown must expire across ctx.advance_secs on a 1.0x offset-anchored system"
    );

    // Genuine TiDi (< 1.0) followed by clearing back to 1.0x STILL records tidi_history_systems = true.
    crate::services::serverops_cluster::set_system_tidi_factor(&ctx, sys_id, 0.50);
    assert_eq!(ctx.resolve_sys_tidi_mode(Some(sys_id)), (0.50, true));
    crate::services::serverops_cluster::clear_manual_system_tidi_override(&ctx, sys_id);
    assert_eq!(ctx.resolve_sys_tidi_mode(Some(sys_id)), (1.0, true));
}

#[tokio::test]
async fn anchor_tidi_on_occupied_system_preserves_pre_anchor_delta_pins_autoscaler_and_entry_reemits_timers() {
    use crate::services::combat_crimewatch::crimewatch_timers::emit_crimewatch_timer_notifications;
    use crate::services::serverops_cluster::{
        emit_sim_clock_rebase_on_entry_if_needed, set_system_tidi_factor,
    };
    use eve_db::clock::FILETIME_TICKS_PER_MS;
    use eve_db::clock::FILETIME_TICKS_PER_SEC;
    use eve_proto::PyRep;

    let base = 1_800_000_000_u64;
    let ctx = edge_ctx(base);
    let char_a = 90_000_001_i64;
    let sys_a = 30_000_142_u32;
    let user_a = ctx.db.with_snapshot(|s| s.characters[&char_a].user_id);
    ctx.db
        .acquire_online_lock(user_a, 9_301, Some(char_a), true, base)
        .unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_a) {
            ch.station_id = None;
            ch.structure_id = None;
            ch.solar_system_id = sys_a;
        }
    });

    let pvp_exp = base + 900;
    {
        let mut cw = ctx.crimewatch.write().unwrap();
        cw.get_or_create(char_a).pvp_timer_until_unix = pvp_exp;
    }
    emit_crimewatch_timer_notifications(&ctx, char_a, ctx.now_unix());
    let _ = ctx.drain_notifications_for_char(char_a);

    ctx.advance_secs(5);
    let _ = ctx.drain_notifications_for_char(char_a);
    let pre_sim_ft = ctx.now_machonet_wallclock_filetime_ticks();

    // Anchor occupied system with -300_000 ms offset + 0.25 TiDi:
    // must emit true non-zero DoSimClockRebase((pre_sim_ft, pre_sim_ft - 300s)), pin autoscaler,
    // and re-emit scaled OnPvpTimerUpdate.
    let offset_ms = -300_000_i64;
    let sim_clock = ctx.anchor_system_sim_clock_with_offset_ms(sys_a, offset_ms, 0.25);
    assert_eq!(sim_clock.sim_filetime().as_ticks(), pre_sim_ft + offset_ms * FILETIME_TICKS_PER_MS);
    {
        let cn = ctx.systems.cluster_nodes.read().unwrap();
        let auto = cn
            .system_tidi_autoscalers
            .get(&sys_a)
            .expect("autoscaler entry must be pinned");
        assert_eq!(auto.pinned_factor, Some(0.25));
    }
    let notifs_a = ctx.drain_notifications_for_char(char_a);
    let rebase_pos = notifs_a
        .iter()
        .position(|n| n.event == "DoSimClockRebase")
        .expect("DoSimClockRebase when anchoring occupied system");
    let pvp_pos = notifs_a
        .iter()
        .position(|n| n.event == "OnPvpTimerUpdate")
        .expect("re-emitted OnPvpTimerUpdate after anchor rebase");
    assert!(rebase_pos < pvp_pos);
    let PyRep::Tuple(times) = &notifs_a[rebase_pos].args[0] else {
        panic!("DoSimClockRebase tuple");
    };
    assert_eq!(times[0].as_i64(), Some(pre_sim_ft));
    assert_eq!(
        times[1].as_i64(),
        Some(pre_sim_ft + offset_ms * FILETIME_TICKS_PER_MS)
    );
    let expected_pvp_a =
        sim_clock.sim_filetime().as_ticks() + ((895 * FILETIME_TICKS_PER_SEC) / 4);
    assert_eq!(notifs_a[pvp_pos].args[1].as_i64(), Some(expected_pvp_a));

    // Occupied-system anchor broadcast must mark `char_a` rebased at the committed generation so
    // an immediate UpdateStateRequest / entry check emits 0 duplicate DoSimClockRebase shifts.
    assert!(
        !emit_sim_clock_rebase_on_entry_if_needed(&ctx, char_a, sys_a),
        "already-broadcast in-space pilot must not receive a duplicate entry DoSimClockRebase"
    );
    assert_eq!(
        ctx.drain_notifications_for_char(char_a)
            .into_iter()
            .filter(|n| n.event == "DoSimClockRebase")
            .count(),
        0
    );

    // Subsequent TiDi transition via `emit_sim_clock_rebase` must likewise keep `char_rebased_system_gen` synced.
    let _ = set_system_tidi_factor(&ctx, sys_a, 0.50);
    let tidi_notifs_a = ctx.drain_notifications_for_char(char_a);
    assert_eq!(
        tidi_notifs_a
            .iter()
            .filter(|n| n.event == "DoSimClockRebase")
            .count(),
        1
    );
    assert!(
        !emit_sim_clock_rebase_on_entry_if_needed(&ctx, char_a, sys_a),
        "TiDi broadcast must also mark in-space pilot rebased at the new generation"
    );
    assert_eq!(
        ctx.drain_notifications_for_char(char_a)
            .into_iter()
            .filter(|n| n.event == "DoSimClockRebase")
            .count(),
        0
    );

    // Entry rebase into a dilated 0.25x system also re-emits scaled active CrimeWatch timers.
    let sys_b = 30_000_144_u32;
    let char_b = 90_000_888_i64;
    set_system_tidi_factor(&ctx, sys_b, 0.25);
    ctx.db.with_mut_snapshot(|snap| {
        let mut acc = snap
            .accounts
            .values()
            .find(|a| a.user_id == user_a)
            .expect("user_a account")
            .clone();
        acc.user_id = char_b as u64;
        acc.username = format!("entry_{char_b}");
        snap.accounts.insert(acc.username.clone(), acc);

        let mut ch = snap.characters[&char_a].clone();
        ch.character_id = char_b;
        ch.user_id = char_b as u64;
        ch.name = "Entry TiDi Pilot".into();
        ch.station_id = Some(60_003_760);
        ch.structure_id = None;
        ch.solar_system_id = sys_b;
        snap.characters.insert(char_b, ch);
    });
    ctx.db
        .acquire_online_lock(char_b as u64, 9_302, Some(char_b), true, ctx.now_unix())
        .unwrap();
    {
        let mut cw = ctx.crimewatch.write().unwrap();
        cw.get_or_create(char_b).pvp_timer_until_unix = ctx.now_unix_secs() + 400;
    }
    emit_crimewatch_timer_notifications(&ctx, char_b, ctx.now_unix());
    let _ = ctx.drain_notifications_for_char(char_b);

    // Move char_b into space in dilated `sys_b` and trigger entry rebase.
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_b) {
            ch.station_id = None;
            ch.structure_id = None;
            ch.solar_system_id = sys_b;
        }
    });
    assert!(emit_sim_clock_rebase_on_entry_if_needed(&ctx, char_b, sys_b));
    let entry_notifs = ctx.drain_notifications_for_char(char_b);
    let r_pos = entry_notifs
        .iter()
        .position(|n| n.event == "DoSimClockRebase")
        .expect("entry DoSimClockRebase");
    let p_pos = entry_notifs
        .iter()
        .position(|n| n.event == "OnPvpTimerUpdate")
        .expect("entry OnPvpTimerUpdate re-emitted after entry DoSimClockRebase");
    assert!(r_pos < p_pos);
    let sim_b = ctx.peek_system_sim_filetime(sys_b).unwrap();
    assert_eq!(
        entry_notifs[p_pos].args[1].as_i64(),
        Some(sim_b.as_ticks() + 100 * FILETIME_TICKS_PER_SEC)
    );
}

#[tokio::test]
async fn backward_reanchor_resets_last_sim_activation_clamp_to_match_peek_sim_filetime() {
    use crate::services::serverops_cluster::set_system_tidi_factor;

    let base = 1_800_000_000_u64;
    let ctx = edge_ctx(base);
    let char_id = 90_000_001_i64;
    let sys_id = 30_000_142_u32;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.structure_id = None;
            ch.solar_system_id = sys_id;
        }
    });

    // Seed higher `last_sim_activation_ft_by_system` via TiDi and same-tick read, then re-anchor backward (-300_000 ms).
    let _ = set_system_tidi_factor(&ctx, sys_id, 0.50);
    let wall_0 = ctx.now_machonet_wall_filetime();
    let _ = ctx.now_system_sim_filetime(Some(sys_id));

    let sim_clock = ctx.anchor_system_sim_clock_with_offset_ms(sys_id, -300_000, 0.25);
    let peek_0 = ctx
        .peek_system_sim_filetime(sys_id)
        .expect("backward re-anchored sim clock");
    assert_eq!(sim_clock.sim_filetime(), peek_0);
    assert_eq!(
        peek_0.as_ticks(),
        wall_0.saturating_sub_secs(300).as_ticks(),
        "peek_system_sim_filetime must trail wall_0 by 300s"
    );

    // `now_system_sim_filetime` must NOT stay clamped +300s ahead at the pre-anchor activation stamp.
    let now_0 = ctx.now_system_sim_filetime(Some(sys_id));
    assert!(
        now_0.as_ticks() >= peek_0.as_ticks() && now_0.as_ticks() <= peek_0.as_ticks() + 1,
        "now_system_sim_filetime ({now_0:?}) must align with peek_0 ({peek_0:?}), not stale wall_0 ({wall_0:?})"
    );
    assert!(now_0.as_ticks() < wall_0.as_ticks());
    let now_char = ctx.now_system_sim_filetime_for_char(char_id);
    assert!(
        now_char.as_ticks() >= peek_0.as_ticks() && now_char.as_ticks() <= peek_0.as_ticks() + 2
    );
    let exp_sim = ctx.virtual_unix_expiry_to_sim_filetime_for_char(char_id, ctx.now_unix_secs() + 40);
    assert_eq!(exp_sim, peek_0.saturating_add_secs(10));

    ctx.advance_secs(4);
    let peek_1 = ctx.peek_system_sim_filetime(sys_id).unwrap();
    assert_eq!(peek_1, peek_0.saturating_add_secs(1));
    assert_eq!(ctx.now_system_sim_filetime(Some(sys_id)), peek_1);

    // Pure 1.0x backward anchor bumps rebase_generation_by_system once (not 1 -> 2) and avoids stale clamp.
    let pure_sys = 30_000_144_u32;
    let pure_clock = ctx.anchor_system_sim_clock_with_offset_ms(pure_sys, -300_000, 1.0);
    let pure_peek = ctx.peek_system_sim_filetime(pure_sys).unwrap();
    assert_eq!(pure_clock.sim_filetime(), pure_peek);
    let pure_now = ctx.now_system_sim_filetime(Some(pure_sys));
    assert!(
        pure_now.as_ticks() >= pure_peek.as_ticks() && pure_now.as_ticks() <= pure_peek.as_ticks() + 1
    );
    let gen = ctx
        .systems
        .cluster_nodes
        .read()
        .unwrap()
        .rebase_generation_by_system
        .get(&pure_sys)
        .copied();
    assert_eq!(gen, Some(1), "single anchor must bump rebase generation once");
}

#[tokio::test]
async fn character_jump_clock_does_not_leak_across_non_stargate_relocations() {
    use crate::services::serverops_cluster::sync_jump_clock_and_emit_rebase_if_needed;

    let ctx = edge_ctx(1_800_000_000);
    let char_id = 90_000_001_i64;
    let sys_a = 30_000_142_u32;
    let sys_b = 30_000_144_u32;
    let sys_c = 30_000_145_u32;
    let sys_d = 30_000_146_u32;

    // Jump into dilated `sys_b`, then advance wallclock while dilated anchor sits on `sys_b`.
    assert!(sync_jump_clock_and_emit_rebase_if_needed(&ctx, char_id, sys_a, sys_b, 1.0, 0.25).is_some());
    ctx.advance_secs(16);
    let _ = ctx.drain_notifications_for_char(char_id);

    // Non-stargate relocation into unanchored 1.0x `sys_c`, followed by stargate jump `sys_c -> sys_d`,
    // must ignore stale `(sys_b, ...)` `character_jump_clocks` and emit no spurious rebase.
    assert_eq!(
        sync_jump_clock_and_emit_rebase_if_needed(&ctx, char_id, sys_c, sys_d, 1.0, 1.0),
        None
    );
    assert_eq!(
        ctx.drain_notifications_for_char(char_id)
            .into_iter()
            .filter(|n| n.event == "DoSimClockRebase")
            .count(),
        0
    );
}

#[tokio::test]
async fn clear_manual_system_tidi_override_resets_unspawned_anchored_sim_clock_to_1x() {
    use crate::services::serverops_cluster::{
        clear_manual_system_tidi_override, get_system_tidi_factor,
    };

    let ctx = edge_ctx(1_800_000_000);
    let sys_id = 30_000_144_u32;
    assert!(ctx.systems.get_handle(sys_id).is_none());

    let clock_0 = ctx.anchor_system_sim_clock_with_offset_ms(sys_id, -300_000, 0.25);
    assert!(
        ctx.systems.get_handle(sys_id).is_none(),
        "anchor_system_sim_clock_with_offset_ms must not spawn SystemHandle"
    );
    ctx.advance_secs(20);
    let sim_at_clear = ctx.peek_system_sim_filetime(sys_id).unwrap();
    assert_eq!(sim_at_clear, clock_0.sim_filetime().saturating_add_secs(5));

    // Clearing the manual override while still unspawned must re-anchor `sim_clock_anchoring` to 1.0x.
    assert!((clear_manual_system_tidi_override(&ctx, sys_id) - 1.0).abs() < 1e-9);
    assert!(ctx.systems.get_handle(sys_id).is_none());
    assert!((get_system_tidi_factor(&ctx, sys_id) - 1.0).abs() < 1e-9);
    {
        let cn = ctx.systems.cluster_nodes.read().unwrap();
        let (_, _, anchored_tidi) = cn
            .sim_clock_anchoring
            .get(&sys_id)
            .copied()
            .expect("sim_clock_anchoring preserved at 1.0x after clear");
        assert!((anchored_tidi - 1.0).abs() < 1e-9);
    }

    ctx.advance_secs(10);
    let sim_after_10s = ctx.peek_system_sim_filetime(sys_id).unwrap();
    assert_eq!(
        sim_after_10s,
        sim_at_clear.saturating_add_secs(10),
        "unspawned cleared system must advance at 1.0x, not stale 0.25x"
    );
    assert_eq!(ctx.now_system_sim_filetime(Some(sys_id)), sim_after_10s);

    // Subsequent lazy spawn must inherit 1.0x and keep advancing consistently at 1.0x.
    let handle = ctx.systems.get_or_spawn(sys_id);
    assert!((handle.tidi_factor() - 1.0).abs() < 1e-9);
    ctx.advance_secs(10);
    let sim_after_spawn = ctx.peek_system_sim_filetime(sys_id).unwrap();
    assert_eq!(sim_after_spawn, sim_after_10s.saturating_add_secs(10));
    assert_eq!(ctx.now_system_sim_filetime(Some(sys_id)), sim_after_spawn);

    // Clearing when unspawned system was only anchored at 1.0x (-300_000 ms) preserves offset and stays 1.0x.
    let pure_sys = 30_000_145_u32;
    let pure_0 = ctx.anchor_system_sim_clock_with_offset_ms(pure_sys, -300_000, 1.0);
    assert!((clear_manual_system_tidi_override(&ctx, pure_sys) - 1.0).abs() < 1e-9);
    ctx.advance_secs(8);
    assert_eq!(
        ctx.peek_system_sim_filetime(pure_sys).unwrap(),
        pure_0.sim_filetime().saturating_add_secs(8)
    );
}
