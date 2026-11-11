//! Extra unit and mutation-killing tests for `fw_gameplay_insurgency.rs`.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::fw_gameplay::fw_gameplay_insurgency::*;
use crate::services::insurgency_services::{
    FACTION_ANGEL_CARTEL, FACTION_GURISTAS, PIRATE_STRONGHOLD_ANGEL_TYPE,
    PIRATE_STRONGHOLD_GURISTAS_TYPE,
};
use eve_db::durable_domain_types::PersistedInsurgencyCampaign;
use eve_db::snapshot_ext::FwSystemOccupationRecord;
use eve_db::{CharacterRecord, IskCents, UpwellStructureRecord};
use eve_sde::SdeRegistry;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn test_ctx() -> ServerContext {
    if let Some(sde) = SHARED_SDE.get() {
        ServerContext::with_sde_and_repository(
            Arc::clone(sde),
            Arc::new(eve_db::WorldRepository::with_demo_seed()),
        )
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
    } else {
        let ctx = ServerContext::new();
        let _ = SHARED_SDE.set(Arc::clone(&ctx.sde));
        ctx
    }
}

fn make_char(
    snap: &eve_db::WorldStateSnapshot,
    char_id: i64,
    fac_id: Option<i64>,
    sys_id: u32,
) -> CharacterRecord {
    let mut c = snap.characters.values().next().unwrap().clone();
    c.character_id = char_id;
    c.name = format!("Pilot_{char_id}");
    c.corporation_id = 1_000_009;
    c.alliance_id = None;
    c.war_faction_id = fac_id;
    c.solar_system_id = sys_id;
    c.station_id = None;
    c.active_ship_id = 0;
    c.balance = IskCents(10_000_000_00);
    c.loyalty_points.clear();
    c
}

#[tokio::test]
async fn ensure_pirate_fob_variants_and_destroyed_guard() {
    let ctx = test_ctx();
    assert_eq!(ensure_campaign_pirate_fob(&ctx, 999_999).await, None);

    let sys_id = 30002505_u32;
    ctx.db.with_mut_snapshot(|snap| {
        for (cid, fac) in [(10_i64, FACTION_GURISTAS), (11_i64, FACTION_ANGEL_CARTEL)] {
            snap.insurgency_runtime.campaigns.insert(
                cid,
                PersistedInsurgencyCampaign {
                    campaign_id: cid,
                    pirate_faction_id: fac,
                    target_faction_id: 500_001,
                    fob_solar_system_id: sys_id,
                    systems: vec![sys_id],
                    created_at_unix: 1_000,
                    forecasting_end_unix: 2_000,
                    active_end_unix: 9_000,
                    winddown_end_unix: 10_000,
                    corruption_points: HashMap::new(),
                    suppression_points: HashMap::new(),
                },
            );
        }
    });

    let guristas_fob = ensure_campaign_pirate_fob(&ctx, 10).await.unwrap();
    let angel_fob = ensure_campaign_pirate_fob(&ctx, 11).await.unwrap();
    assert_eq!(guristas_fob, PIRATE_FOB_STRUCTURE_BASE_ID + 10);
    assert_eq!(angel_fob, PIRATE_FOB_STRUCTURE_BASE_ID + 11);

    ctx.db.with_mut_snapshot(|snap| {
        assert_eq!(
            snap.structures[&guristas_fob].type_id,
            PIRATE_STRONGHOLD_GURISTAS_TYPE
        );
        assert_eq!(
            snap.structures[&angel_fob].type_id,
            PIRATE_STRONGHOLD_ANGEL_TYPE
        );
        snap.structures.get_mut(&guristas_fob).unwrap().state = "Destroyed".into();
    });
    assert_eq!(ensure_campaign_pirate_fob(&ctx, 10).await, None);
}

#[test]
fn spread_insurgency_stage5_suppressed_and_fallback_neighbor() {
    let ctx = test_ctx();
    let sys_id = 30002506_u32;
    let camp_id = 20_i64;

    assert_eq!(try_spread_insurgency_from_system(&ctx, 999, sys_id), None);

    ctx.db.with_mut_snapshot(|snap| {
        let mut corr = HashMap::new();
        corr.insert(sys_id, 600); // Stage 3+
        let mut supp = HashMap::new();
        supp.insert(sys_id, 1_000); // Stage 5 suppressed -> blocks spread!
        snap.insurgency_runtime.campaigns.insert(
            camp_id,
            PersistedInsurgencyCampaign {
                campaign_id: camp_id,
                pirate_faction_id: FACTION_GURISTAS,
                target_faction_id: 500_001,
                fob_solar_system_id: sys_id,
                systems: vec![sys_id],
                created_at_unix: 1_000,
                forecasting_end_unix: 2_000,
                active_end_unix: 9_000,
                winddown_end_unix: 10_000,
                corruption_points: corr,
                suppression_points: supp,
            },
        );
    });
    assert_eq!(try_spread_insurgency_from_system(&ctx, camp_id, sys_id), None);

    // Lower suppression below Stage 5 and test fallback when source_sys_id has no direct free neighbor
    let fake_source = 99_999_999_u32;
    ctx.db.with_mut_snapshot(|snap| {
        let camp = snap.insurgency_runtime.campaigns.get_mut(&camp_id).unwrap();
        camp.suppression_points.insert(sys_id, 0);
        camp.corruption_points.insert(fake_source, 600);
        camp.systems.push(fake_source);
    });
    let spread = try_spread_insurgency_from_system(&ctx, camp_id, fake_source);
    assert!(spread.is_some());
}

#[test]
fn docking_allowed_fulcrum_and_pirate_fob_edge_cases() {
    let ctx = test_ctx();
    let now = ctx.now_unix();
    let sys_id = 30002505_u32;
    let fulcrum_id = 60_015_187_i64;

    // Invalid inputs into record_insurgency_contribution are ignored cleanly
    record_insurgency_contribution(&ctx, 1, 0, FACTION_GURISTAS, 100);
    record_insurgency_contribution(&ctx, 1, 101, FACTION_GURISTAS, 0);

    ctx.db.with_mut_snapshot(|snap| {
        for cid in [201_i64, 202, 203, 204, 205] {
            let fac = if cid == 205 { Some(FACTION_GURISTAS) } else { None };
            snap.characters.insert(cid, make_char(snap, cid, fac, sys_id));
        }
        // 201 is direct-enlisted in Empire FW via fw_runtime.direct_enlisted_chars
        snap.fw_runtime.direct_enlisted_chars.insert(201, 500_001);
        // 202 is in a corporation enlisted in Empire FW via fw_runtime.corp_war_factions
        snap.characters.get_mut(&202).unwrap().corporation_id = 98_222_001;
        snap.fw_runtime.corp_war_factions.insert(98_222_001, 500_002);
    });

    // 203 has an active disapproval flag timer in combat_runtime
    crate::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| {
        rt.disapproval_flags.insert(203, now + 300);
    });

    assert_eq!(
        check_docking_allowed(&ctx, 201, sys_id, fulcrum_id, false).unwrap_err().code,
        "FulcrumRefusesEmpireMilitia"
    );
    assert_eq!(
        check_docking_allowed(&ctx, 202, sys_id, fulcrum_id, false).unwrap_err().code,
        "FulcrumRefusesEmpireMilitia"
    );
    assert_eq!(
        check_docking_allowed(&ctx, 203, sys_id, fulcrum_id, false).unwrap_err().code,
        "FulcrumRefusesTimerActive"
    );

    // 204 has active weapons timer in crimewatch
    ctx.crimewatch
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .trigger_weapon_timer(204, now);
    assert_eq!(
        check_docking_allowed(&ctx, 204, sys_id, fulcrum_id, false).unwrap_err().code,
        "FulcrumRefusesTimerActive"
    );
    ctx.crimewatch
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .states
        .remove(&204);

    // Horizon Siege event locks Fulcrum for non-pirate 204, allows pirate 205
    let _ = crate::services::cluster_macho_misc_services::trigger_zarzakh_horizon_siege(
        &ctx,
        Some(1),
        3_600,
    );
    assert_eq!(
        check_docking_allowed(&ctx, 204, sys_id, fulcrum_id, false).unwrap_err().code,
        "HorizonSiegeFulcrumLockdown"
    );
    assert!(check_docking_allowed(&ctx, 205, sys_id, fulcrum_id, false).is_ok());

    // Non-FOB Upwell structure allows neutral docking
    let citadel_id = 1_044_000_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            citadel_id,
            UpwellStructureRecord {
                structure_id: citadel_id,
                solar_system_id: sys_id,
                type_id: 35_832, // Astrahus
                owner_corp_id: 1_000_009,
                state: "ShieldVulnerable".into(),
                ..Default::default()
            },
        );
    });
    assert!(check_docking_allowed(&ctx, 204, sys_id, citadel_id, true).is_ok());
}

#[test]
fn evaluate_campaign_tie_underdog_modifiers_and_cached_outcome() {
    let ctx = test_ctx();
    let sys_id = 30002505_u32;

    assert_eq!(
        evaluate_and_resolve_insurgency_campaign(&ctx, 888_888, None)
            .unwrap_err()
            .code,
        "CampaignNotFound"
    );

    // 1. Tie outcome: 0 systems reach stage 5, FOB not destroyed -> "Tie", 0 LP payout
    let tie_camp = 301_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.insert(301, make_char(snap, 301, Some(FACTION_GURISTAS), sys_id));
        snap.insurgency_runtime.campaigns.insert(
            tie_camp,
            PersistedInsurgencyCampaign {
                campaign_id: tie_camp,
                pirate_faction_id: FACTION_GURISTAS,
                target_faction_id: 500_001,
                fob_solar_system_id: sys_id,
                systems: vec![sys_id],
                created_at_unix: 1_000,
                forecasting_end_unix: 2_000,
                active_end_unix: 3_000,
                winddown_end_unix: 4_000,
                corruption_points: HashMap::from([(sys_id, 200)]),
                suppression_points: HashMap::from([(sys_id, 200)]),
            },
        );
    });
    record_insurgency_contribution(&ctx, tie_camp, 301, FACTION_GURISTAS, 5_000);
    let (outcome, _, _, payouts) =
        evaluate_and_resolve_insurgency_campaign(&ctx, tie_camp, None).unwrap();
    assert_eq!(outcome, "Tie");
    assert_eq!(payouts[0].2, 0);

    // Calling again hits the cached outcome fast path
    let (cached_outcome, _, _, cached_payouts) =
        evaluate_and_resolve_insurgency_campaign(&ctx, tie_camp, None).unwrap();
    assert_eq!(cached_outcome, "Tie");
    assert!(cached_payouts.is_empty());

    // 2. Pirate underdog win when empire_ambition_modifier > 0 decrements empire_ambition_modifier
    let underdog_camp = 302_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.insurgency_runtime
            .empire_ambition_modifier
            .insert(FACTION_GURISTAS, 2);
        snap.insurgency_runtime
            .pirate_ambition_modifier
            .insert(FACTION_GURISTAS, 0);
        snap.insurgency_runtime.campaigns.insert(
            underdog_camp,
            PersistedInsurgencyCampaign {
                campaign_id: underdog_camp,
                pirate_faction_id: FACTION_GURISTAS,
                target_faction_id: 500_001,
                fob_solar_system_id: sys_id,
                systems: vec![sys_id],
                created_at_unix: 1_000,
                forecasting_end_unix: 2_000,
                active_end_unix: 3_000,
                winddown_end_unix: 4_000,
                corruption_points: HashMap::new(),
                suppression_points: HashMap::new(),
            },
        );
    });
    let (ud_out, p_mod, e_mod, _) =
        evaluate_and_resolve_insurgency_campaign(&ctx, underdog_camp, Some("PirateWin")).unwrap();
    assert_eq!(ud_out, "PirateWin");
    assert_eq!(p_mod, 0);
    assert_eq!(e_mod, 1);

    // 3. Empire underdog win when pirate_ambition_modifier > 0 decrements pirate_ambition_modifier
    let emp_underdog = 303_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.insurgency_runtime
            .pirate_ambition_modifier
            .insert(FACTION_GURISTAS, 3);
        snap.insurgency_runtime
            .empire_ambition_modifier
            .insert(FACTION_GURISTAS, 0);
        snap.insurgency_runtime.campaigns.insert(
            emp_underdog,
            PersistedInsurgencyCampaign {
                campaign_id: emp_underdog,
                pirate_faction_id: FACTION_GURISTAS,
                target_faction_id: 500_001,
                fob_solar_system_id: sys_id,
                systems: vec![sys_id],
                created_at_unix: 1_000,
                forecasting_end_unix: 2_000,
                active_end_unix: 3_000,
                winddown_end_unix: 4_000,
                corruption_points: HashMap::new(),
                suppression_points: HashMap::new(),
            },
        );
    });
    let (emp_out, p_mod2, e_mod2, _) =
        evaluate_and_resolve_insurgency_campaign(&ctx, emp_underdog, Some("EmpireWin")).unwrap();
    assert_eq!(emp_out, "EmpireWin");
    assert_eq!(p_mod2, 2);
    assert_eq!(e_mod2, 0);
}

#[tokio::test]
async fn apply_pirate_fob_damage_phase_suppression_and_partial_damage_branches() {
    let ctx = test_ctx();
    let now = ctx.now_unix();
    let sys_id = 30002505_u32;
    let camp_id = 401_i64;
    let fob_id = PIRATE_FOB_STRUCTURE_BASE_ID + camp_id;

    assert_eq!(
        apply_pirate_fob_structure_damage(&ctx, 101, 123_456_789, 500.0)
            .await
            .unwrap_err()
            .code,
        "StructureNotFound"
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            fob_id,
            UpwellStructureRecord {
                structure_id: fob_id,
                solar_system_id: sys_id,
                type_id: PIRATE_STRONGHOLD_GURISTAS_TYPE,
                owner_corp_id: 1_000_437,
                state: "ShieldVulnerable".into(),
                shield_hp: 2_000.0,
                armor_hp: 2_000.0,
                hull_hp: 3_000.0,
                ..Default::default()
            },
        );
    });
    assert_eq!(
        apply_pirate_fob_structure_damage(&ctx, 101, fob_id, 500.0)
            .await
            .unwrap_err()
            .code,
        "CampaignNotFound"
    );

    let systems: Vec<u32> = vec![sys_id, 30000142, 30000143, 30000144, 30000145];
    // Forecasting phase -> FobInvulnerableInPhase
    ctx.db.with_mut_snapshot(|snap| {
        snap.insurgency_runtime.campaigns.insert(
            camp_id,
            PersistedInsurgencyCampaign {
                campaign_id: camp_id,
                pirate_faction_id: FACTION_GURISTAS,
                target_faction_id: 500_001,
                fob_solar_system_id: sys_id,
                systems: systems.clone(),
                created_at_unix: now - 10,
                forecasting_end_unix: now + 3_600,
                active_end_unix: now + 7_200,
                winddown_end_unix: now + 10_800,
                corruption_points: HashMap::new(),
                suppression_points: HashMap::new(),
            },
        );
    });
    assert_eq!(
        apply_pirate_fob_structure_damage(&ctx, 101, fob_id, 500.0)
            .await
            .unwrap_err()
            .code,
        "FobInvulnerableInPhase"
    );

    // Active phase without 5 Stage-5 suppressed systems -> SuppressionGoalNotReached
    ctx.db.with_mut_snapshot(|snap| {
        let camp = snap.insurgency_runtime.campaigns.get_mut(&camp_id).unwrap();
        camp.forecasting_end_unix = now - 100;
    });
    assert_eq!(
        apply_pirate_fob_structure_damage(&ctx, 101, fob_id, 500.0)
            .await
            .unwrap_err()
            .code,
        "SuppressionGoalNotReached"
    );

    // Populate 5 Stage-5 suppressed systems, apply partial shield/armor damage (< 4000)
    ctx.db.with_mut_snapshot(|snap| {
        let camp = snap.insurgency_runtime.campaigns.get_mut(&camp_id).unwrap();
        for &s in &systems {
            camp.suppression_points.insert(s, 1_000);
        }
    });
    let partial = apply_pirate_fob_structure_damage(&ctx, 101, fob_id, 2_500.0)
        .await
        .unwrap();
    assert_eq!(partial.dict_get("shieldHP"), Some(&eve_proto::PyRep::Real(0.0)));
    assert_eq!(partial.dict_get("armorHP"), Some(&eve_proto::PyRep::Real(1_500.0)));

    // Reinforce armor, advance past timer, then apply partial hull damage (< 3000)
    let _ = apply_pirate_fob_structure_damage(&ctx, 101, fob_id, 2_000.0)
        .await
        .unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        snap.insurgency_runtime
            .fob_armor_reinforced_until
            .insert(camp_id, now - 1);
    });
    let hull_partial = apply_pirate_fob_structure_damage(&ctx, 101, fob_id, 1_000.0)
        .await
        .unwrap();
    assert_eq!(
        hull_partial.dict_get("state"),
        Some(&eve_proto::PyRep::String("HullVulnerable".into()))
    );
    assert_eq!(hull_partial.dict_get("hullHP"), Some(&eve_proto::PyRep::Real(2_000.0)));
}

#[tokio::test]
async fn tick_insurgency_campaigns_auto_seeding_roamers_and_cleanup() {
    let ctx = test_ctx();
    let now = ctx.now_unix();
    let sys_id = 30002505_u32;

    // Empty campaigns + occupied FW system + pirate pilot in system -> auto-seeds a frontline campaign
    ctx.db.with_mut_snapshot(|snap| {
        snap.insurgency_runtime.campaigns.clear();
        snap.characters.insert(501, make_char(snap, 501, Some(FACTION_GURISTAS), sys_id));
        snap.fw_system_occupations.insert(
            sys_id,
            FwSystemOccupationRecord {
                solar_system_id: sys_id,
                occupier_faction_id: 500_001,
                owner_faction_id: 500_002,
                victory_points: 1_000,
                victory_points_threshold: 3_000,
            },
        );
    });
    tick_insurgency_campaigns(&ctx, now).await;

    let seeded_id = ctx.db.with_snapshot(|snap| {
        *snap
            .insurgency_runtime
            .campaigns
            .keys()
            .next()
            .expect("auto-seeded campaign")
    });

    // Move campaign into Active phase with Stage 5 suppression & Stage 3 corruption -> spawns roamers + CI Sentry 79_507
    let camp_sys = ctx.db.with_mut_snapshot(|snap| {
        let camp = snap.insurgency_runtime.campaigns.get_mut(&seeded_id).unwrap();
        let s = camp.fob_solar_system_id;
        camp.forecasting_end_unix = now - 10;
        camp.active_end_unix = now + 3_600;
        camp.winddown_end_unix = now + 7_200;
        let st = snap.system_states.entry(s).or_default();
        st.insurgency_corruption_stage = 3;
        st.insurgency_suppression_stage = 5;
        s
    });
    sync_campaign_system_npcs(&ctx, seeded_id, camp_sys).await;
    assert!(crate::services::combat_crimewatch::with_combat_ephemeral(&ctx, |rt| {
        rt.ci_sentry_deployed_systems.contains(&camp_sys)
    }));

    // Expire winddown_end_unix and tick -> resolves Tie and cleans up ended campaign roamers
    ctx.db.with_mut_snapshot(|snap| {
        let camp = snap.insurgency_runtime.campaigns.get_mut(&seeded_id).unwrap();
        camp.active_end_unix = now - 5;
        camp.winddown_end_unix = now - 1;
    });
    tick_insurgency_campaigns(&ctx, now).await;
    assert!(ctx.db.with_snapshot(|snap| !snap.insurgency_runtime.campaigns.contains_key(&seeded_id)));
}

#[tokio::test]
async fn sync_campaign_system_npcs_roamer_waypoint_fallback_boundaries_0_1_2_3_celestials() {
    use glam::DVec3;

    let mut custom_sde = (*test_ctx().sde).clone();
    let mk_planet = |cid: i64, sys_id: u32, x: f64| eve_sde::CelestialInfo {
        celestial_id: cid,
        solar_system_id: sys_id,
        type_id: 11,
        group_id: 7,
        name: format!("Planet {cid}"),
        radius: 10_000.0,
        position: DVec3::new(x, 0.0, 0.0),
        orbit_id: None,
        orbit_index: None,
        celestial_index: None,
    };
    let sys_0 = 99_100_000_u32;
    let sys_1 = 99_100_001_u32;
    let sys_2 = 99_100_002_u32;
    let sys_3 = 99_100_003_u32;

    custom_sde.insert_celestial(mk_planet(40_900_101, sys_1, 200_000.0));

    custom_sde.insert_celestial(mk_planet(40_900_201, sys_2, 100_000.0));
    custom_sde.insert_celestial(mk_planet(40_900_202, sys_2, 400_000.0));

    custom_sde.insert_celestial(mk_planet(40_900_301, sys_3, 100_000.0));
    custom_sde.insert_celestial(mk_planet(40_900_302, sys_3, 400_000.0));
    custom_sde.insert_celestial(mk_planet(40_900_303, sys_3, 700_000.0));

    let ctx = ServerContext::with_sde_and_repository(
        Arc::new(custom_sde),
        Arc::new(eve_db::WorldRepository::with_demo_seed()),
    )
    .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED);

    let now = ctx.now_unix();
    let camp_id = 77_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.insurgency_runtime.campaigns.insert(
            camp_id,
            PersistedInsurgencyCampaign {
                campaign_id: camp_id,
                pirate_faction_id: FACTION_GURISTAS,
                target_faction_id: 500_001,
                fob_solar_system_id: sys_0,
                systems: vec![sys_0, sys_1, sys_2, sys_3],
                created_at_unix: now - 1_000,
                forecasting_end_unix: now - 10,
                active_end_unix: now + 3_600,
                winddown_end_unix: now + 7_200,
                corruption_points: HashMap::new(),
                suppression_points: HashMap::new(),
            },
        );
        for &s in &[sys_0, sys_1, sys_2, sys_3] {
            let st = snap.system_states.entry(s).or_default();
            st.insurgency_corruption_stage = 2;
            st.insurgency_suppression_stage = 0;
        }
    });

    let pirate_offset = DVec3::new(12_000.0, 0.0, 4_000.0);
    let fallback0 = DVec3::new(12_000.0, 0.0, 4_000.0) + pirate_offset;

    let find_pirate_bid = |sys: u32| -> i64 {
        crate::services::combat_crimewatch::with_combat_ephemeral(&ctx, |rt| {
            rt.npc_entities
                .iter()
                .find(|(_, npc)| {
                    npc.solar_system_id == sys
                        && ctx.sde.types.get(&npc.type_id).map(|t| t.group_id) == Some(4637)
                })
                .map(|(&bid, _)| bid)
                .expect("pirate roamer present")
        })
    };

    let next_idle_dest = |sys: u32, bid: i64, stop_at: DVec3| -> DVec3 {
        let sys_h = ctx.systems.get_or_spawn(sys);
        {
            let mut bp = sys_h.ballpark.write().unwrap();
            let b = bp.balls.get_mut(&bid).unwrap();
            b.position = stop_at;
            b.mode = eve_destiny::MovementMode::Stop;
        }
        crate::services::combat_crimewatch::with_mut_combat_ephemeral(&ctx, |rt| {
            rt.npc_entities.get_mut(&bid).unwrap().spawn_position = stop_at;
        });
        stop_at
    };

    // 0 celestials: spawns at fallback0 = (24_000, 0, 8_000) (kills `vec![]`, `vec![Default::default()]`, `< 2 -> == 2 / > 2`)
    sync_campaign_system_npcs(&ctx, camp_id, sys_0).await;
    let bid0 = find_pirate_bid(sys_0);
    let pos0 = ctx.systems.get_or_spawn(sys_0).ballpark.read().unwrap().balls[&bid0].position;
    assert_eq!(pos0, fallback0);

    // 1 celestial: spawns at wp0 (200_000 + 60_000 + 12_000 = 272_000); next idle patrol MUST go to fallback0 (len == 4, not len == 1!)
    let c_wp = |x: f64| DVec3::new(x + 10_000.0 + 50_000.0, 0.0, 0.0) + pirate_offset;
    sync_campaign_system_npcs(&ctx, camp_id, sys_1).await;
    let bid1 = find_pirate_bid(sys_1);
    let pos1 = ctx.systems.get_or_spawn(sys_1).ballpark.read().unwrap().balls[&bid1].position;
    assert_eq!(pos1, c_wp(200_000.0));
    sync_campaign_system_npcs(&ctx, camp_id, sys_1).await;
    let dest1 = match &ctx.systems.get_or_spawn(sys_1).ballpark.read().unwrap().balls[&bid1].mode {
        eve_destiny::MovementMode::Warp { destination, .. } => *destination,
        other => panic!("expected Warp toward fallback0 from wp0, got {other:?}"),
    };
    assert_eq!(dest1, fallback0, "1 celestial must append fallback waypoints (`wps.len() < 2`)");

    // 2 celestials: from wp1 (400k), next idle patrol MUST wrap back to wp0 (len == 2), NOT fallback0 (`< 2 -> <= 2 / == 2`)
    sync_campaign_system_npcs(&ctx, camp_id, sys_2).await;
    let bid2 = find_pirate_bid(sys_2);
    let _ = next_idle_dest(sys_2, bid2, c_wp(400_000.0));
    sync_campaign_system_npcs(&ctx, camp_id, sys_2).await;
    let dest2 = match &ctx.systems.get_or_spawn(sys_2).ballpark.read().unwrap().balls[&bid2].mode {
        eve_destiny::MovementMode::Warp { destination, .. } => *destination,
        other => panic!("expected Warp back to wp0, got {other:?}"),
    };
    assert_eq!(dest2, c_wp(100_000.0), "2 celestials must wrap 1 -> 0 without fallback waypoints");

    // 3 celestials: from wp2 (700k), next idle patrol MUST wrap back to wp0 (len == 3), NOT fallback0 (`< 2 -> > 2`)
    sync_campaign_system_npcs(&ctx, camp_id, sys_3).await;
    let bid3 = find_pirate_bid(sys_3);
    let _ = next_idle_dest(sys_3, bid3, c_wp(700_000.0));
    sync_campaign_system_npcs(&ctx, camp_id, sys_3).await;
    let dest3 = match &ctx.systems.get_or_spawn(sys_3).ballpark.read().unwrap().balls[&bid3].mode {
        eve_destiny::MovementMode::Warp { destination, .. } => *destination,
        other => panic!("expected Warp back to wp0, got {other:?}"),
    };
    assert_eq!(dest3, c_wp(100_000.0), "3 celestials must wrap 2 -> 0 without fallback waypoints");
}
