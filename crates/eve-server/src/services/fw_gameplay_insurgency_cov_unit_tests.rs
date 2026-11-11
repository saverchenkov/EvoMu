//! Comprehensive unit tests for `fw_gameplay_insurgency` covering Fulcrum/FOB docking rules,
//! ambition-scaled FOB siege, roamer/enforcer/CI-sentry NPC sync & patrol stepping, and ended-campaign cleanup.

#![forbid(unsafe_code)]

use super::*;
use crate::router::ServerContext;
use crate::services::combat_crimewatch::{with_combat_runtime, with_mut_combat_ephemeral, with_mut_combat_runtime};
use eve_db::clock::{ServerClock, VirtualServerClock};
use eve_db::durable_domain_types::{
    PersistedCrimeWatchState, PersistedIceHeistRecord, PersistedInsurgencyCampaign,
    PersistedInsurgencySiteRecord,
};
use eve_db::{CharacterRecord, IskCents, UpwellStructureRecord};
use eve_destiny::MovementMode;
use eve_sde::SdeRegistry;
use glam::DVec3;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn shared_sde() -> Arc<SdeRegistry> {
    Arc::clone(SHARED_SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn set_clock_to(clock: &VirtualServerClock, target: u64) {
    let cur = clock.now_unix_secs();
    if target > cur {
        clock.advance_secs(target - cur);
    }
}

fn test_ctx_with_clock(now_unix: u64) -> (ServerContext, Arc<VirtualServerClock>) {
    let clock = Arc::new(VirtualServerClock::with_fixed_base(now_unix));
    let db = Arc::new(eve_db::WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(shared_sde(), db)
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED);
    ctx.set_clock(Arc::clone(&clock) as Arc<dyn ServerClock>);
    (ctx, clock)
}

fn make_char(
    snap: &eve_db::WorldStateSnapshot,
    char_id: i64,
    fac_id: Option<i64>,
    sys_id: u32,
) -> CharacterRecord {
    let mut c = snap.characters.values().next().unwrap().clone();
    c.character_id = char_id;
    c.name = format!("CovInsurg_{char_id}");
    c.corporation_id = 1_000_009;
    c.alliance_id = None;
    c.war_faction_id = fac_id;
    c.solar_system_id = sys_id;
    c.station_id = None;
    c.structure_id = None;
    c.active_ship_id = 0;
    c.balance = IskCents::from_isk(10_000_000);
    c.loyalty_points.clear();
    c
}

#[test]
fn docking_fulcrum_timers_custom_fob_group_and_contribution_guards() {
    let mut sde = (*shared_sde()).clone();
    let mut custom_fob = sde
        .types
        .get(&PIRATE_STRONGHOLD_GURISTAS_TYPE)
        .or_else(|| sde.types.values().next())
        .unwrap()
        .clone();
    custom_fob.type_id = 992_444;
    custom_fob.group_id = 4644;
    sde.types.insert(992_444, custom_fob);

    let clock = Arc::new(VirtualServerClock::with_fixed_base(50_000));
    let db = Arc::new(eve_db::WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(Arc::new(sde), db);
    ctx.set_clock(Arc::clone(&clock) as Arc<dyn ServerClock>);
    let sys_id = 30002505_u32;

    // Zero/negative char_id or points ignored by record_insurgency_contribution
    record_insurgency_contribution(&ctx, 77, 0, FACTION_GURISTAS, 100);
    record_insurgency_contribution(&ctx, 77, -1, FACTION_GURISTAS, 100);
    record_insurgency_contribution(&ctx, 77, 101, FACTION_GURISTAS, 0);
    record_insurgency_contribution(&ctx, 77, 101, FACTION_GURISTAS, -50);
    assert!(ctx.db.with_snapshot(|s| !s.insurgency_runtime.campaign_contributions.contains_key(&77)));

    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.insert(101, make_char(snap, 101, None, sys_id));
        snap.characters.insert(102, make_char(snap, 102, Some(FACTION_ANGEL_CARTEL), sys_id));
        snap.characters.insert(103, make_char(snap, 103, Some(FACTION_GURISTAS), sys_id));
        // Pilot 104 enlisted via corp_war_factions
        let mut c4 = make_char(snap, 104, None, sys_id);
        c4.corporation_id = 98_111_222;
        snap.characters.insert(104, c4);
        snap.fw_runtime.corp_war_factions.insert(98_111_222, 500_002);
        // Pilot 105 enlisted via direct_enlisted_chars
        snap.characters.insert(105, make_char(snap, 105, None, sys_id));
        snap.fw_runtime.direct_enlisted_chars.insert(105, 500_003);

        // Angel FOB (78260), Custom group-4644 FOB (992444), and non-FOB Upwell (35832)
        snap.structures.insert(
            701,
            UpwellStructureRecord {
                structure_id: 701,
                solar_system_id: sys_id,
                type_id: PIRATE_STRONGHOLD_ANGEL_TYPE,
                state: "ShieldVulnerable".into(),
                ..Default::default()
            },
        );
        snap.structures.insert(
            702,
            UpwellStructureRecord {
                structure_id: 702,
                solar_system_id: sys_id,
                type_id: 992_444,
                state: "ShieldVulnerable".into(),
                ..Default::default()
            },
        );
        snap.structures.insert(
            703,
            UpwellStructureRecord {
                structure_id: 703,
                solar_system_id: sys_id,
                type_id: 35_832,
                state: "ShieldVulnerable".into(),
                ..Default::default()
            },
        );
    });

    // Fulcrum (60_015_187): corp & direct empire enlistment refused
    assert_eq!(
        check_docking_allowed(&ctx, 104, sys_id, 60_015_187, false).unwrap_err().code,
        "FulcrumRefusesEmpireMilitia"
    );
    assert_eq!(
        check_docking_allowed(&ctx, 105, sys_id, 60_015_187, false).unwrap_err().code,
        "FulcrumRefusesEmpireMilitia"
    );

    // Fulcrum weapons timer boundary (50_001 active vs 50_000 expired) & retribution timer boundary (50_001 active vs 50_000 expired)
    ctx.crimewatch.write().unwrap().states.insert(
        101,
        PersistedCrimeWatchState {
            character_id: 101,
            weapon_timer_until_unix: 50_001,
            ..Default::default()
        },
    );
    assert_eq!(
        check_docking_allowed(&ctx, 101, sys_id, 60_015_187, false).unwrap_err().code,
        "FulcrumRefusesTimerActive"
    );
    ctx.crimewatch.write().unwrap().states.get_mut(&101).unwrap().weapon_timer_until_unix = 50_000;
    assert!(check_docking_allowed(&ctx, 101, sys_id, 60_015_187, false).is_ok());

    with_mut_combat_runtime(&ctx, |rt| {
        rt.disapproval_flags.insert(101, 50_001);
    });
    assert_eq!(
        check_docking_allowed(&ctx, 101, sys_id, 60_015_187, false).unwrap_err().code,
        "FulcrumRefusesTimerActive"
    );
    set_clock_to(&clock, 50_001);
    assert!(check_docking_allowed(&ctx, 101, sys_id, 60_015_187, false).is_ok());

    // Angel FOB allows Angel (102), rejects Guristas (103); non-FOB (703) allows anyone
    assert!(check_docking_allowed(&ctx, 102, sys_id, 701, true).is_ok());
    assert_eq!(
        check_docking_allowed(&ctx, 103, sys_id, 701, true).unwrap_err().code,
        "FobDockingRestrictedToPirateMilitia"
    );
    assert!(check_docking_allowed(&ctx, 101, sys_id, 703, true).is_ok());

    // Custom group-4644 FOB: without campaign defaults to FACTION_GURISTAS; with Angel campaign requires FACTION_ANGEL_CARTEL
    assert!(check_docking_allowed(&ctx, 103, sys_id, 702, true).is_ok());
    assert!(check_docking_allowed(&ctx, 102, sys_id, 702, true).is_err());
    ctx.db.with_mut_snapshot(|snap| {
        snap.insurgency_runtime.campaigns.insert(
            9,
            PersistedInsurgencyCampaign {
                campaign_id: 9,
                pirate_faction_id: FACTION_ANGEL_CARTEL,
                target_faction_id: 500_002,
                fob_solar_system_id: sys_id,
                systems: vec![sys_id],
                created_at_unix: 1_000,
                forecasting_end_unix: 2_000,
                active_end_unix: 90_000,
                winddown_end_unix: 99_000,
                corruption_points: HashMap::new(),
                suppression_points: HashMap::new(),
            },
        );
    });
    assert!(check_docking_allowed(&ctx, 102, sys_id, 702, true).is_ok());
    assert!(check_docking_allowed(&ctx, 103, sys_id, 702, true).is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pirate_fob_siege_non_base_id_ambition_scaling_and_reinforcement_boundaries() {
    let (ctx, clock) = test_ctx_with_clock(10_000);
    let sys_id = 30002505_u32;
    let custom_fob_id = 55_001_i64; // not `PIRATE_FOB_STRUCTURE_BASE_ID + camp_id` -> resolves by `fob_solar_system_id == st_sys`
    let camp_id = 33_i64;
    let all_systems: Vec<u32> = (0..10).map(|i| sys_id + i).collect();

    assert_eq!(
        apply_pirate_fob_structure_damage(&ctx, 201, 999_888, 100.0).await.unwrap_err().code,
        "StructureNotFound"
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.insert(201, make_char(snap, 201, Some(500_001), sys_id));
        snap.characters.insert(202, make_char(snap, 202, Some(FACTION_GURISTAS), sys_id));
        snap.structures.insert(
            custom_fob_id,
            UpwellStructureRecord {
                structure_id: custom_fob_id,
                solar_system_id: sys_id,
                type_id: PIRATE_STRONGHOLD_GURISTAS_TYPE,
                state: "ShieldVulnerable".into(),
                shield_hp: 500.0,
                armor_hp: 500.0,
                hull_hp: 1_000.0,
                ..Default::default()
            },
        );
    });
    assert_eq!(
        apply_pirate_fob_structure_damage(&ctx, 201, custom_fob_id, 100.0).await.unwrap_err().code,
        "CampaignNotFound"
    );

    let mut supp: HashMap<u32, i64> = all_systems.iter().take(6).map(|&s| (s, 1_000)).collect();
    ctx.db.with_mut_snapshot(|snap| {
        snap.insurgency_runtime.empire_ambition_modifier.insert(FACTION_GURISTAS, 2); // supp_goal = 5 + 2 = 7
        snap.insurgency_runtime.campaigns.insert(
            camp_id,
            PersistedInsurgencyCampaign {
                campaign_id: camp_id,
                pirate_faction_id: FACTION_GURISTAS,
                target_faction_id: 500_001,
                fob_solar_system_id: sys_id,
                systems: all_systems.clone(),
                created_at_unix: 1_000,
                forecasting_end_unix: 12_000, // Forecasting at t = 10_000
                active_end_unix: 80_000,
                winddown_end_unix: 90_000,
                corruption_points: HashMap::new(),
                suppression_points: supp.clone(),
            },
        );
    });

    assert_eq!(
        apply_pirate_fob_structure_damage(&ctx, 202, custom_fob_id, 100.0).await.unwrap_err().code,
        "CannotAttackFriendlyStructure"
    );
    assert_eq!(
        apply_pirate_fob_structure_damage(&ctx, 201, custom_fob_id, 100.0).await.unwrap_err().code,
        "FobInvulnerableInPhase"
    );

    set_clock_to(&clock, 15_000);
    // 6 Stage-5 systems < supp_goal (7) -> SuppressionGoalNotReached
    assert_eq!(
        apply_pirate_fob_structure_damage(&ctx, 201, custom_fob_id, 100.0).await.unwrap_err().code,
        "SuppressionGoalNotReached"
    );

    // Add 7th Stage-5 system -> partial shield damage succeeds, then shield+armor triggers ArmorReinforced
    supp.insert(all_systems[6], 1_000);
    ctx.db.with_mut_snapshot(|snap| {
        snap.insurgency_runtime.campaigns.get_mut(&camp_id).unwrap().suppression_points = supp;
    });
    apply_pirate_fob_structure_damage(&ctx, 201, custom_fob_id, 200.0).await.unwrap();
    apply_pirate_fob_structure_damage(&ctx, 201, custom_fob_id, 900.0).await.unwrap();
    let until = ctx.db.with_snapshot(|s| s.insurgency_runtime.fob_armor_reinforced_until[&camp_id]);

    // Boundary now = until - 1 -> FobArmorReinforced; now = until -> HullVulnerable partial hull hit!
    set_clock_to(&clock, until - 1);
    assert_eq!(
        apply_pirate_fob_structure_damage(&ctx, 201, custom_fob_id, 400.0).await.unwrap_err().code,
        "FobArmorReinforced"
    );
    set_clock_to(&clock, until);
    apply_pirate_fob_structure_damage(&ctx, 201, custom_fob_id, 400.0).await.unwrap();
    assert_eq!(
        ctx.db.with_snapshot(|s| s.structures[&custom_fob_id].hull_hp),
        600.0
    );

    // Pre-Destroyed guard check
    ctx.db.with_mut_snapshot(|s| s.structures.get_mut(&custom_fob_id).unwrap().state = "Destroyed".into());
    assert_eq!(
        apply_pirate_fob_structure_damage(&ctx, 201, custom_fob_id, 100.0).await.unwrap_err().code,
        "StructureDestroyed"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn roamer_selection_waypoints_npc_sync_patrol_and_ended_campaign_cleanup() {
    let (ctx, clock) = test_ctx_with_clock(30_000);
    let sys_id = 30002505_u32;

    // select_insurgency_roamer_type & system_roamer_waypoints (real system vs synthetic empty system)
    assert!(select_insurgency_roamer_type(&ctx, 999_999, None, 0, 3).is_none());
    let p_t0 = select_insurgency_roamer_type(&ctx, 4637, Some(FACTION_GURISTAS as u32), 0, 3);
    let p_t3 = select_insurgency_roamer_type(&ctx, 4637, Some(FACTION_GURISTAS as u32), 3, 3);
    let e_t0 = select_insurgency_roamer_type(&ctx, 4638, None, 0, 2);
    assert!(p_t0.is_some() && p_t3.is_some() && e_t0.is_some());

    let real_wps = system_roamer_waypoints(&ctx, sys_id);
    assert!(real_wps.len() >= 2);
    let synth_wps = system_roamer_waypoints(&ctx, 99_999_999);
    assert_eq!(synth_wps.len(), 3);
    assert_eq!(synth_wps[0], DVec3::new(12_000.0, 0.0, 4_000.0));

    // Spawn Pirate roamer (c_st=3), Enforcer (s_st=5), and CI Sentry (79_507 at s_st=5)
    sync_insurgency_system_npcs(&ctx, FACTION_GURISTAS, 500_001, sys_id, 3, 5).await;
    let (pir_bid, enf_bid, ci_bid) = with_combat_runtime(&ctx, |rt| {
        let mut p = 0;
        let mut e = 0;
        let mut c = 0;
        for (&bid, npc) in &rt.npc_entities {
            if npc.solar_system_id == sys_id {
                if npc.type_id == 79_507 {
                    c = bid;
                } else if ctx.sde.types.get(&npc.type_id).map(|t| t.group_id) == Some(4637) {
                    p = bid;
                } else if ctx.sde.types.get(&npc.type_id).map(|t| t.group_id) == Some(4638) {
                    e = bid;
                }
            }
        }
        (p, e, c)
    });
    assert!(pir_bid > 0 && enf_bid > 0 && ci_bid > 0);

    // Move pirate roamer > 15 km from spawn_position to exercise idle patrol stepping (`arrived_positions` + warp/goto)
    let sh = ctx.systems.get_or_spawn(sys_id);
    assert!(sh.set_ball_position(pir_bid, real_wps[1] + DVec3::new(12_000.0, 0.0, 4_000.0)).await);
    sh.set_movement_mode(enf_bid, MovementMode::new_warp(DVec3::ZERO, DVec3::new(500_000.0, 0.0, 0.0), 3.0))
        .await
        .unwrap();
    sync_insurgency_system_npcs(&ctx, FACTION_GURISTAS, 500_001, sys_id, 3, 5).await;

    // Drop stages to (0, 0) -> despawns stale Pirate & Enforcer roamers and clears `ci_sentry_deployed_systems`, keeping 79_507!
    sync_insurgency_system_npcs(&ctx, FACTION_GURISTAS, 500_001, sys_id, 0, 0).await;
    // Second call at (0, 0) exercises fast-path early return (`stale_balls.is_empty() && idle_roamers.is_empty() && ...`)
    sync_insurgency_system_npcs(&ctx, FACTION_GURISTAS, 500_001, sys_id, 0, 0).await;
    with_combat_runtime(&ctx, |rt| {
        assert!(!rt.npc_entities.contains_key(&pir_bid));
        assert!(!rt.npc_entities.contains_key(&enf_bid));
        assert!(rt.npc_entities.contains_key(&ci_bid));
        assert!(!rt.ci_sentry_deployed_systems.contains(&sys_id));
    });

    // sync_campaign_system_npcs: missing campaign, Forecasting phase, non-member system, and Active phase
    sync_campaign_system_npcs(&ctx, 999, sys_id).await;
    ctx.db.with_mut_snapshot(|snap| {
        let st = snap.system_states.entry(sys_id).or_default();
        st.solar_system_id = sys_id;
        st.insurgency_corruption_stage = 2;
        st.insurgency_suppression_stage = 3;
        snap.insurgency_runtime.campaigns.insert(
            88,
            PersistedInsurgencyCampaign {
                campaign_id: 88,
                pirate_faction_id: FACTION_ANGEL_CARTEL,
                target_faction_id: 500_002,
                fob_solar_system_id: sys_id,
                systems: vec![sys_id],
                created_at_unix: 20_000,
                forecasting_end_unix: 35_000,
                active_end_unix: 60_000,
                winddown_end_unix: 70_000,
                corruption_points: HashMap::new(),
                suppression_points: HashMap::new(),
            },
        );
    });
    // Forecasting at 30_000 -> no-op
    sync_campaign_system_npcs(&ctx, 88, sys_id).await;
    set_clock_to(&clock, 40_000);
    // Non-member system -> no-op; member system -> spawns roamers
    sync_campaign_system_npcs(&ctx, 88, 30000142).await;
    sync_campaign_system_npcs(&ctx, 88, sys_id).await;

    // cleanup_ended_campaign_sites_and_roamers: test PirateStronghold retention, CounterInsurgencyOutpost war HQ rule,
    // ice_heist_instances (< 860_001 kept vs >= 860_001 pruned), and 4637/4638 despawn while 79_507 stays!
    let angel_hq = 30_002_974_u32;
    ctx.db.with_mut_snapshot(|snap| {
        snap.insurgency_runtime.campaigns.remove(&88);
        snap.insurgency_runtime.insurgency_instances = vec![
            PersistedInsurgencySiteRecord {
                instance_id: 1,
                solar_system_id: sys_id,
                site_type: "PirateStronghold".into(),
                name: "Stronghold".into(),
                corruption_reward: 0.0,
                completed: false,
            },
            PersistedInsurgencySiteRecord {
                instance_id: 2,
                solar_system_id: angel_hq,
                site_type: "CounterInsurgencyOutpost".into(),
                name: "CI Outpost".into(),
                corruption_reward: 0.0,
                completed: false,
            },
            PersistedInsurgencySiteRecord {
                instance_id: 3,
                solar_system_id: sys_id,
                site_type: "MiningAmbush".into(),
                name: "Ambush".into(),
                corruption_reward: 50.0,
                completed: false,
            },
        ];
        snap.insurgency_runtime.ice_heist_instances = vec![
            PersistedIceHeistRecord {
                instance_id: 860_000,
                solar_system_id: sys_id,
                pirate_tankers_saved: 0,
                empire_tankers_saved: 0,
                completed: false,
            },
            PersistedIceHeistRecord {
                instance_id: 860_001,
                solar_system_id: sys_id,
                pirate_tankers_saved: 0,
                empire_tankers_saved: 0,
                completed: false,
            },
        ];
    });
    with_mut_combat_ephemeral(&ctx, |rt| {
        rt.ci_sentry_deployed_systems.insert(sys_id);
    });
    cleanup_ended_campaign_sites_and_roamers(&ctx, FACTION_ANGEL_CARTEL, &[sys_id, angel_hq]).await;
    ctx.db.with_snapshot(|snap| {
        let kept_ids: Vec<i64> = snap.insurgency_runtime.insurgency_instances.iter().map(|s| s.instance_id).collect();
        assert_eq!(kept_ids, vec![1]);
        let heist_ids: Vec<i64> = snap.insurgency_runtime.ice_heist_instances.iter().map(|h| h.instance_id).collect();
        assert_eq!(heist_ids, vec![860_000]);
    });
    with_combat_runtime(&ctx, |rt| {
        assert!(rt.npc_entities.contains_key(&ci_bid));
        assert!(!rt.npc_entities.values().any(|n| n.solar_system_id == sys_id && n.type_id != 79_507));
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actor_fob_damage_phases_and_idle_roamer_patrol_branches() {
    let (ctx, clock) = test_ctx_with_clock(20_000);
    let sys_id = 30002510_u32;
    let camp_id = 77_i64;
    let fob_id = PIRATE_FOB_STRUCTURE_BASE_ID + camp_id;
    let all_systems: Vec<u32> = (0..6).map(|i| sys_id + i).collect();

    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            fob_id,
            UpwellStructureRecord {
                structure_id: fob_id,
                solar_system_id: sys_id,
                type_id: PIRATE_STRONGHOLD_GURISTAS_TYPE,
                state: "ShieldVulnerable".into(),
                shield_hp: 2_500.0,
                armor_hp: 2_500.0,
                hull_hp: 3_500.0,
                ..Default::default()
            },
        );
        snap.insurgency_runtime.campaigns.insert(
            camp_id,
            PersistedInsurgencyCampaign {
                campaign_id: camp_id,
                pirate_faction_id: FACTION_GURISTAS,
                target_faction_id: 500_001,
                fob_solar_system_id: sys_id,
                systems: all_systems.clone(),
                created_at_unix: 10_000,
                forecasting_end_unix: 11_000,
                active_end_unix: 90_000,
                winddown_end_unix: 99_000,
                corruption_points: HashMap::new(),
                suppression_points: HashMap::new(),
            },
        );
    });

    let sys_h = ctx.systems.get_or_spawn(sys_id);
    {
        let mut bp = sys_h.ballpark.write().unwrap();
        let fob_ball = eve_destiny::Ball {
            id: fob_id,
            type_id: PIRATE_STRONGHOLD_GURISTAS_TYPE,
            position: glam::DVec3::new(10_000.0, 0.0, 0.0),
            velocity: glam::DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 1e9,
            radius: 500.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        };
        bp.add_ball(fob_ball);

        // 1. Invulnerable (suppression < stage 5) -> deals 0.0
        let invuln = handle_fob_ball_damage_in_actor(
            Some(&ctx.db),
            sys_id,
            fob_id,
            eve_destiny::DamageVector { em: 100.0, thermal: 0.0, kinetic: 0.0, explosive: 0.0 },
            &mut bp,
            20_000,
        )
        .unwrap();
        assert_eq!(invuln.0, 0.0);
        assert!(!invuln.1);
    }

    // Push all systems to suppression stage 5 (supp >= 500 in >= 5 systems)
    for &s in &all_systems[..5] {
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(c) = snap.insurgency_runtime.campaigns.get_mut(&camp_id) {
                c.suppression_points.insert(s, 1_000);
            }
        });
    }

    {
        let mut bp = sys_h.ballpark.write().unwrap();
        // 2. Partial ShieldPhase damage stripping shield -> ArmorVulnerable
        let p_arm = handle_fob_ball_damage_in_actor(
            Some(&ctx.db),
            sys_id,
            fob_id,
            eve_destiny::DamageVector { em: 1_500.0, thermal: 1_500.0, kinetic: 0.0, explosive: 0.0 },
            &mut bp,
            20_100,
        )
        .unwrap();
        assert!(p_arm.0 > 0.0);
        assert!(!p_arm.1);
    }
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.structures.get(&fob_id).unwrap().state, "ArmorVulnerable");
    });

    {
        let mut bp = sys_h.ballpark.write().unwrap();
        // 3. Wipe remaining armor -> enters ArmorReinforced with timer
        let reinf = handle_fob_ball_damage_in_actor(
            Some(&ctx.db),
            sys_id,
            fob_id,
            eve_destiny::DamageVector { em: 0.0, thermal: 0.0, kinetic: 5_000.0, explosive: 0.0 },
            &mut bp,
            20_200,
        )
        .unwrap();
        assert!(!reinf.1);
    }
    let until = ctx.db.with_snapshot(|snap| snap.insurgency_runtime.fob_armor_reinforced_until[&camp_id]);
    assert!(until > 20_200);

    // Advance past reinforcement -> ArmorPhase partial -> HullVulnerable, then fatal -> Destroyed
    set_clock_to(&clock, until + 10);
    {
        let mut bp = sys_h.ballpark.write().unwrap();
        let p_hull = handle_fob_ball_damage_in_actor(
            Some(&ctx.db),
            sys_id,
            fob_id,
            eve_destiny::DamageVector { em: 0.0, thermal: 0.0, kinetic: 0.0, explosive: 1_000.0 },
            &mut bp,
            until + 10,
        )
        .unwrap();
        assert!(!p_hull.1);
    }
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.structures.get(&fob_id).unwrap().state, "HullVulnerable");
    });
    {
        let mut bp = sys_h.ballpark.write().unwrap();
        let killed = handle_fob_ball_damage_in_actor(
            Some(&ctx.db),
            sys_id,
            fob_id,
            eve_destiny::DamageVector { em: 5_000.0, thermal: 5_000.0, kinetic: 5_000.0, explosive: 5_000.0 },
            &mut bp,
            until + 20,
        )
        .unwrap();
        assert!(killed.1);
    }
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.structures.get(&fob_id).unwrap().state, "Destroyed");
    });

    // Tick roamers at corruption stage 2 + suppression stage 2, then a second tick so existing idle roamers navigate waypoints (warp & goto)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.insurgency_runtime.campaigns.get_mut(&camp_id) {
            c.corruption_points.insert(sys_id, 200);
            c.suppression_points.insert(sys_id, 200);
        }
    });
    sync_insurgency_system_npcs(&ctx, FACTION_GURISTAS, 500_001, sys_id, 2, 3).await;
    // Move an idle roamer far from its spawn_position so second tick updates spawn_position and warps/goes to next waypoint
    let moved_bid = with_combat_runtime(&ctx, |rt| rt.npc_entities.keys().next().copied());
    if let Some(bid) = moved_bid {
        let mut bp = sys_h.ballpark.write().unwrap();
        if let Some(b) = bp.balls.get_mut(&bid) {
            b.position = glam::DVec3::new(1.0e12, 0.0, 0.0);
        }
    }
    sync_insurgency_system_npcs(&ctx, FACTION_GURISTAS, 500_001, sys_id, 2, 3).await;
}
