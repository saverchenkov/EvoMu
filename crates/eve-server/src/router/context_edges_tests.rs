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
    use std::sync::atomic::Ordering;
    let ctx_a = crate::router::TestContext::from_server(edge_ctx(1_750_000_000));
    let ctx_b = crate::router::TestContext::from_server(edge_ctx(1_750_000_000));
    assert!(!ctx_a.is_rpc_rate_budget_disabled());
    assert!(!ctx_b.is_rpc_rate_budget_disabled());

    let t0 = std::time::Instant::now();
    let sid_unit = 9_501_u64;
    let burst = RPC_RATE_BURST_TOKENS as usize;
    for i in 0..burst {
        assert!(
            ctx_a.try_consume_rpc_token_at(sid_unit, t0, false),
            "default context must allow burst token {i}"
        );
    }
    assert!(
        !ctx_a.try_consume_rpc_token_at(sid_unit, t0, false),
        "default context must reject token consumption past burst"
    );

    let char_id = 90_000_001_i64;
    let (uid, corp_id, sol_id, ship_id) = ctx_a.db.with_snapshot(|snap| {
        let ch = &snap.characters[&char_id];
        (ch.user_id, ch.corporation_id, ch.solar_system_id, ch.active_ship_id)
    });
    let make_sess = |ctx: &ServerContext, session_id: u64| {
        let mut s = SessionState {
            session_id,
            user_id: Some(uid),
            character_id: Some(char_id),
            corporation_id: Some(corp_id),
            solar_system_id: Some(sol_id),
            station_id: Some(60_003_760),
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
    assert!(saw_rate_limited, "default context must enforce RateLimited in dispatch_rpc");

    let buckets_before = ctx_a.rpc_rate_bucket_count();
    ctx_a.disable_rpc_rate_budget();
    assert!(ctx_a.is_rpc_rate_budget_disabled());

    // Exhausted bucket and previously unseen sessions both bypass without inserting buckets.
    assert!(ctx_a.try_consume_rpc_token_at(sid_unit, t0, false));
    assert!(ctx_a.try_consume_rpc_token_at(9_599, t0, false));
    assert_eq!(ctx_a.rpc_rate_bucket_count(), buckets_before);

    for _ in 0..50 {
        ctx_a
            .dispatch_rpc(&mut sess_a, "account", None, "GetCashBalance", &[], &kw, None)
            .await
            .expect("disabled rate budget must allow dispatch_rpc beyond burst");
    }

    // Weak downgrade/upgrade preserves the disabled fast-path state.
    let upgraded = ctx_a.downgrade().upgrade().expect("upgrade ServerContext");
    assert!(upgraded.rpc_rate_budget_disabled.load(Ordering::Acquire));
    assert!(upgraded.try_consume_rpc_token_at(sid_unit, t0, false));

    // Separate ServerContext remains rate-limited by default.
    assert!(!ctx_b.is_rpc_rate_budget_disabled());
    for _ in 0..burst {
        assert!(ctx_b.try_consume_rpc_token_at(sid_unit, t0, false));
    }
    assert!(!ctx_b.try_consume_rpc_token_at(sid_unit, t0, false));
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
    assert_eq!(
        sim_0.as_ticks() - wall_0.as_ticks(),
        offset_ms * FILETIME_TICKS_PER_MS,
        "SimFileTime must trail WallFileTime by -300_000 ms even at TiDi = 1.0"
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
}
