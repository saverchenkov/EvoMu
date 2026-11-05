//! Unit tests for `fw_gameplay_insurgency` (FOB jitter, docking rules, spread, FOB siege, campaign resolution, 1 Hz tick).

#![forbid(unsafe_code)]

use super::*;
use crate::router::ServerContext;
use eve_db::durable_domain_types::PersistedInsurgencyCampaign;
use eve_db::snapshot_ext::FwSystemOccupationRecord;
use eve_db::{CharacterRecord, IskCents, UpwellStructureRecord};
use eve_sde::SdeRegistry;
use std::collections::{HashMap, HashSet};
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

fn make_insurgency_char(
    snap: &eve_db::WorldStateSnapshot,
    char_id: i64,
    fac_id: Option<i64>,
    sys_id: u32,
) -> CharacterRecord {
    let mut c = snap.characters.values().next().unwrap().clone();
    c.character_id = char_id;
    c.name = format!("Insurgent_{char_id}");
    c.corporation_id = 1_000_009;
    c.alliance_id = None;
    c.war_faction_id = fac_id;
    c.solar_system_id = sys_id;
    c.station_id = None;
    c.active_ship_id = 0;
    c.balance = IskCents(50_000_000_00);
    c.loyalty_points.clear();
    c
}

#[test]
fn fob_jitter_contributions_and_docking_enforcement() {
    let j1 = fob_reinforcement_jitter_secs(1, 3_600 * 10);
    assert!((-3_600..=3_600).contains(&j1));
    assert_eq!(j1, fob_reinforcement_jitter_secs(1, 3_600 * 10 + 120));

    let ctx = test_ctx();
    let camp_id = 42_i64;
    let pirate_fac = FACTION_GURISTAS;
    let empire_fac = 500_001_i64;
    let sys_id = 30002505_u32;
    let fob_id = PIRATE_FOB_STRUCTURE_BASE_ID + camp_id;

    let mut corr = HashMap::new();
    corr.insert(sys_id, 1_000);
    let mut supp = HashMap::new();
    supp.insert(sys_id, 200);

    ctx.db.with_mut_snapshot(|snap| {
        snap.insurgency_runtime.campaigns.insert(
            camp_id,
            PersistedInsurgencyCampaign {
                campaign_id: camp_id,
                pirate_faction_id: pirate_fac,
                target_faction_id: empire_fac,
                fob_solar_system_id: sys_id,
                systems: vec![sys_id],
                created_at_unix: 1_000_000,
                forecasting_end_unix: 1_100_000,
                active_end_unix: 1_700_000,
                winddown_end_unix: 1_900_000,
                corruption_points: corr,
                suppression_points: supp,
            },
        );
        let c1 = make_insurgency_char(snap, 101, Some(pirate_fac), sys_id);
        let c2 = make_insurgency_char(snap, 102, Some(empire_fac), sys_id);
        let c3 = make_insurgency_char(snap, 103, None, sys_id);
        snap.characters.insert(101, c1);
        snap.characters.insert(102, c2);
        snap.characters.insert(103, c3);
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
                hull_hp: 2_000.0,
                ..Default::default()
            },
        );
    });

    assert!(check_docking_allowed(&ctx, 101, sys_id, fob_id, true).is_ok());
    let fob_err = check_docking_allowed(&ctx, 102, sys_id, fob_id, true).unwrap_err();
    assert_eq!(fob_err.code, "FobDockingRestrictedToPirateMilitia");

    let fulcrum_err = check_docking_allowed(&ctx, 102, sys_id, 60_015_187, false).unwrap_err();
    assert_eq!(fulcrum_err.code, "FulcrumRefusesEmpireMilitia");
    assert!(check_docking_allowed(&ctx, 101, sys_id, 60_000_001, false).is_ok());

    record_insurgency_contribution(&ctx, camp_id, 101, pirate_fac, 300);
    record_insurgency_contribution(&ctx, camp_id, 101, pirate_fac, 200);
    record_insurgency_contribution(&ctx, camp_id, 102, empire_fac, 500);
    ctx.db.with_snapshot(|snap| {
        let entries = snap
            .insurgency_runtime
            .campaign_contributions
            .get(&camp_id)
            .unwrap();
        assert_eq!(entries.len(), 2);
        let p_pts = entries.iter().find(|e| e.0 == 101).unwrap().2;
        assert_eq!(p_pts, 500);
    });
}

#[test]
fn insurgency_spread_fob_siege_and_campaign_payout_resolution() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let ctx = test_ctx();
    let camp_id = 77_i64;
    let fob_sys = 30002506_u32;
    let neighbor_sys = ctx
        .sde
        .jumps
        .values()
        .find(|j| j.from_solar_system_id == fob_sys && j.to_solar_system_id != fob_sys)
        .map(|j| j.to_solar_system_id)
        .unwrap_or(30002506);
    let fob_id = PIRATE_FOB_STRUCTURE_BASE_ID + camp_id;
    let now = ctx.now_unix();

    let mut supp = HashMap::new();
    let extra_systems: Vec<u32> = vec![fob_sys, 30000142, 30000143, 30000144, 30000145];
    for &s in &extra_systems[1..] {
        supp.insert(s, 1_000);
    }

    ctx.db.with_mut_snapshot(|snap| {
        snap.insurgency_runtime.campaigns.insert(
            camp_id,
            PersistedInsurgencyCampaign {
                campaign_id: camp_id,
                pirate_faction_id: FACTION_ANGEL_CARTEL,
                target_faction_id: 500_003,
                fob_solar_system_id: fob_sys,
                systems: extra_systems.clone(),
                created_at_unix: now - 300_000,
                forecasting_end_unix: now - 100_000,
                active_end_unix: now + 500_000,
                winddown_end_unix: now + 700_000,
                corruption_points: HashMap::from([(fob_sys, 600)]),
                suppression_points: supp,
            },
        );
        snap.fw_system_occupations.insert(
            neighbor_sys,
            FwSystemOccupationRecord {
                solar_system_id: neighbor_sys,
                owner_faction_id: 500_003,
                occupier_faction_id: 500_003,
                victory_points: 0,
                victory_points_threshold: 3_000,
            },
        );
        snap.structures.insert(
            fob_id,
            UpwellStructureRecord {
                structure_id: fob_id,
                solar_system_id: fob_sys,
                type_id: PIRATE_STRONGHOLD_ANGEL_TYPE,
                owner_corp_id: 1_000_436,
                state: "ShieldVulnerable".into(),
                shield_hp: 2_000.0,
                armor_hp: 2_000.0,
                hull_hp: 2_000.0,
                position: [10_000.0, 0.0, 0.0],
                ..Default::default()
            },
        );
        let c1 = make_insurgency_char(snap, 201, Some(500_003), fob_sys);
        let c2 = make_insurgency_char(snap, 202, Some(FACTION_ANGEL_CARTEL), fob_sys);
        snap.characters.insert(201, c1);
        snap.characters.insert(202, c2);
    });

    let spread_res = try_spread_insurgency_from_system(&ctx, camp_id, fob_sys);
    assert!(spread_res.is_some());
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.insurgency_runtime.campaigns.get_mut(&camp_id) {
            c.suppression_points.insert(fob_sys, 1_000);
        }
        snap.insurgency_runtime.campaign_contributions.insert(
            camp_id,
            vec![(201, 500_003, 500), (202, FACTION_ANGEL_CARTEL, 200)],
        );
    });

    let err_friendly = rt
        .block_on(apply_pirate_fob_structure_damage(&ctx, 202, fob_id, 500.0))
        .unwrap_err();
    assert_eq!(err_friendly.code, "CannotAttackFriendlyStructure");

    let reinf_res = rt
        .block_on(apply_pirate_fob_structure_damage(&ctx, 201, fob_id, 4_500.0))
        .expect("FOB enters armor reinforcement");
    assert_eq!(
        reinf_res.dict_get("state").and_then(|v| v.as_str()),
        Some("ArmorReinforced")
    );
    let until = reinf_res
        .dict_get("reinforcedUntilUnix")
        .and_then(|v| v.as_i64())
        .expect("reinforcedUntilUnix");
    assert!(until > 0);

    let err_reinf = rt
        .block_on(apply_pirate_fob_structure_damage(&ctx, 201, fob_id, 1_000.0))
        .unwrap_err();
    assert_eq!(err_reinf.code, "FobArmorReinforced");

    ctx.db.with_mut_snapshot(|snap| {
        snap.insurgency_runtime
            .fob_armor_reinforced_until
            .insert(camp_id, 1);
    });
    let kill_res = rt
        .block_on(apply_pirate_fob_structure_damage(&ctx, 201, fob_id, 3_000.0))
        .expect("destroy FOB hull");
    assert_eq!(
        kill_res.dict_get("state").and_then(|v| v.as_str()),
        Some("Destroyed")
    );

    let (outcome, emp_mod, wallet_after) = ctx.db.with_snapshot(|snap| {
        let out = snap
            .insurgency_runtime
            .campaign_outcomes
            .get(&camp_id)
            .cloned();
        let em = snap
            .insurgency_runtime
            .empire_ambition_modifier
            .get(&FACTION_ANGEL_CARTEL)
            .copied()
            .unwrap_or(0);
        let w = snap.characters.get(&201).unwrap().balance.0;
        (out, em, w)
    });
    assert_eq!(outcome.as_deref(), Some("EmpireWin"));
    assert_eq!(emp_mod, 1);
    assert!(wallet_after > 50_000_000_00);

    // Ensure campaign FOB helper + WindDown PirateWin expiry + post-surrender vulnerability unlock
    let camp2_id = 88_i64;
    let cur_now = ctx.now_unix();
    let mut corr2 = HashMap::new();
    for &s in &extra_systems {
        corr2.insert(s, 1_000);
    }
    ctx.db.with_mut_snapshot(|snap| {
        snap.insurgency_runtime.campaigns.insert(
            camp2_id,
            PersistedInsurgencyCampaign {
                campaign_id: camp2_id,
                pirate_faction_id: FACTION_ANGEL_CARTEL,
                target_faction_id: 500_003,
                fob_solar_system_id: fob_sys,
                systems: extra_systems.clone(),
                created_at_unix: cur_now.saturating_sub(900_000),
                forecasting_end_unix: cur_now.saturating_sub(700_000),
                active_end_unix: cur_now.saturating_sub(200_000),
                winddown_end_unix: cur_now.saturating_sub(1),
                corruption_points: corr2,
                suppression_points: HashMap::new(),
            },
        );
        snap.insurgency_runtime
            .active_state_notified
            .insert(camp2_id);
        snap.insurgency_runtime.campaign_contributions.insert(
            camp2_id,
            vec![
                (202, FACTION_ANGEL_CARTEL, 800),
                (201, 500_003, 200),
            ],
        );
        snap.fw_system_occupations.insert(
            fob_sys,
            FwSystemOccupationRecord {
                solar_system_id: fob_sys,
                owner_faction_id: 500_003,
                occupier_faction_id: FACTION_ANGEL_CARTEL,
                victory_points: 0,
                victory_points_threshold: 3_000,
            },
        );
    });
    let fob2_id = rt.block_on(ensure_campaign_pirate_fob(&ctx, camp2_id)).expect("spawn FOB");
    assert_eq!(fob2_id, PIRATE_FOB_STRUCTURE_BASE_ID + camp2_id);
    rt.block_on(tick_insurgency_campaigns(&ctx, cur_now));
    ctx.db.with_snapshot(|snap| {
        assert_eq!(
            snap.insurgency_runtime
                .campaign_outcomes
                .get(&camp2_id)
                .map(String::as_str),
            Some("PirateWin")
        );
        assert!(snap
            .insurgency_runtime
            .surrender_ihub_vulnerable_unix
            .contains_key(&fob_sys));
        let empty_set: HashSet<i64> = HashSet::new();
        assert!(!snap
            .insurgency_runtime
            .active_state_notified
            .contains(&(empty_set.len() as i64)));
    });

    rt.block_on(tick_insurgency_campaigns(
        &ctx,
        cur_now + SURRENDER_IHUB_VULNERABILITY_DELAY_SECS + 10,
    ));
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.fw_runtime.ihub_vulnerable.get(&fob_sys), Some(&true));
    });
}
