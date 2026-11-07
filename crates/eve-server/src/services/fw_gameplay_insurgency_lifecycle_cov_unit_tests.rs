//! Comprehensive unit and mutation-killing tests for `fw_gameplay_insurgency_lifecycle`.
//!
//! Exercises Pirate FOB provisioning, stargate spread security/region/wormhole/priority rules,
//! Stage-5 suppression gates, Ambition-scaled victory evaluation, personal contribution LP/ISK
//! payouts, surrender I-Hub vulnerability unlocking, auto-frontline seeding, and tick transitions.

#![forbid(unsafe_code)]

use super::*;
use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::{
    make_fighter_ctx_and_session, make_fighter_type,
};
use crate::services::fw_gameplay::fw_gameplay_insurgency::record_insurgency_contribution;
use crate::services::insurgency_services::{
    PIRATE_STRONGHOLD_ANGEL_TYPE, PIRATE_STRONGHOLD_GURISTAS_TYPE,
};
use eve_db::durable_domain_types::{PersistedInsurgencyCampaign, PersistedInsurgencySiteRecord};
use eve_db::snapshot_ext::FwSystemOccupationRecord;
use eve_db::{CharacterRecord, IskCents, VirtualServerClock};
use eve_sde::{RegionInfo, SolarSystem, StargateJump};
use glam::DVec3;
use std::collections::HashMap;
use std::sync::Arc;

fn add_sys(
    sde: &mut eve_sde::SdeRegistry,
    sys_id: u32,
    region_id: u32,
    sec: f64,
    wh_class: Option<i32>,
) {
    sde.solar_systems.insert(
        sys_id,
        SolarSystem {
            solar_system_id: sys_id,
            region_id,
            constellation_id: 20_000_001,
            name: format!("Sys_{sys_id}"),
            security_status: sec,
            radius: 1.0e12,
            galactic_pos_ly: [0.0; 3],
            planet_ids: Vec::new(),
            stations: Vec::new(),
            wormhole_class_id: wh_class,
        },
    );
}

fn add_jump(sde: &mut eve_sde::SdeRegistry, gate_id: i64, from_sys: u32, to_sys: u32) {
    sde.jumps.insert(
        gate_id,
        StargateJump {
            stargate_id: gate_id,
            from_solar_system_id: from_sys,
            to_solar_system_id: to_sys,
            destination_stargate_id: gate_id + 1_000,
            position: DVec3::new(10_000.0, 0.0, 0.0),
        },
    );
}

fn seed_pilot(snap: &mut eve_db::WorldStateSnapshot, cid: i64, fac: Option<i64>, sys_id: u32) {
    let mut ch: CharacterRecord = snap.characters.values().next().expect("seed char").clone();
    ch.character_id = cid;
    ch.user_id = cid as u64;
    ch.name = format!("InsPilot_{cid}");
    ch.corporation_id = 1_000_009;
    ch.war_faction_id = fac;
    ch.solar_system_id = sys_id;
    ch.station_id = None;
    ch.structure_id = None;
    ch.balance = IskCents::from_isk(10_000_000);
    ch.loyalty_points.clear();
    snap.characters.insert(cid, ch);
}

fn build_insurgency_ctx(now_unix: u64) -> ServerContext {
    let (mut ctx, _, _, _, _) = make_fighter_ctx_and_session();
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(now_unix)));
    let mut sde = (*ctx.sde).clone();
    sde.solar_systems.clear();
    sde.jumps.clear();

    // Empire region 10_001 (Caldari 500_001), Ammatar region 10_007 (500_007), Pirate region 10_099 (500_010)
    for (rid, fid) in [(10_001_u32, Some(500_001)), (10_007, Some(500_007)), (10_099, Some(500_010))] {
        sde.regions.insert(
            rid,
            RegionInfo {
                region_id: rid,
                name: format!("Reg_{rid}"),
                center: [0.0; 3],
                faction_id: fid,
                wormhole_class_id: None,
            },
        );
    }

    // Add FOB SDE types with positive and zero default_attributes to exercise base_hp closure
    let mut gur_fob = make_fighter_type(PIRATE_STRONGHOLD_GURISTAS_TYPE, 4644, 65, 1_000.0, vec![]);
    gur_fob.default_attributes.insert(263, 9_999.0);
    gur_fob.default_attributes.insert(265, 0.0);
    sde.types.insert(PIRATE_STRONGHOLD_GURISTAS_TYPE, gur_fob);

    let mut ang_fob = make_fighter_type(PIRATE_STRONGHOLD_ANGEL_TYPE, 4644, 65, 1_000.0, vec![]);
    ang_fob.default_attributes.insert(9, 8_888.0);
    sde.types.insert(PIRATE_STRONGHOLD_ANGEL_TYPE, ang_fob);

    // Core topology: 30_001_001..=30_001_015
    add_sys(&mut sde, 30_001_001, 10_001, 0.3, Some(7)); // low-sec frontline A
    add_sys(&mut sde, 30_001_002, 10_001, 0.4, None); // low-sec frontline B (enemy)
    add_sys(&mut sde, 30_001_003, 10_001, 0.2, None); // low-sec rearguard C
    add_sys(&mut sde, 30_001_004, 10_007, 0.5, Some(8)); // exact 0.5 high-sec in Ammatar
    add_sys(&mut sde, 30_001_005, 10_001, 0.0, Some(9)); // exact 0.0 boundary valid for spread
    add_sys(&mut sde, 30_001_006, 10_001, 0.84, None); // < 0.85 valid for spread
    // Invalid spread neighbors from 30_001_001:
    add_sys(&mut sde, 30_001_007, 10_001, -0.01, None); // negative sec rejected
    add_sys(&mut sde, 30_001_008, 10_001, 0.85, None); // >= 0.85 sec rejected
    add_sys(&mut sde, 30_001_009, 10_001, 0.4, Some(3)); // wormhole class 3 rejected
    add_sys(&mut sde, 30_001_010, 10_099, 0.4, None); // pirate region rejected
    add_sys(&mut sde, 30_100_000, 10_001, 0.4, None); // >= 30_100_000 rejected

    let mut gid = 50_001_i64;
    for &(a, b) in &[
        (30_001_001, 30_001_002),
        (30_001_002, 30_001_001),
        (30_001_001, 30_001_003),
        (30_001_003, 30_001_001),
        (30_001_001, 30_001_004),
        (30_001_004, 30_001_005),
        (30_001_005, 30_001_006),
        (30_001_001, 30_001_007),
        (30_001_001, 30_001_008),
        (30_001_001, 30_001_009),
        (30_001_001, 30_001_010),
        (30_001_001, 30_100_000),
    ] {
        add_jump(&mut sde, gid, a, b);
        gid += 1;
    }

    ctx.sde = Arc::new(sde);
    ctx
}

#[tokio::test]
async fn ensure_pirate_fob_idempotence_attributes_and_spread_priority_and_boundaries() {
    let ctx = build_insurgency_ctx(1_750_000_000);
    let fob_sys = 30_001_001_u32;

    assert_eq!(ensure_campaign_pirate_fob(&ctx, 404).await, None);

    ctx.db.with_mut_snapshot(|snap| {
        for (cid, fac) in [(1_i64, FACTION_GURISTAS), (2_i64, FACTION_ANGEL_CARTEL)] {
            snap.insurgency_runtime.campaigns.insert(
                cid,
                PersistedInsurgencyCampaign {
                    campaign_id: cid,
                    pirate_faction_id: fac,
                    target_faction_id: 500_001,
                    fob_solar_system_id: fob_sys,
                    systems: vec![fob_sys],
                    created_at_unix: 1_000,
                    forecasting_end_unix: 2_000,
                    active_end_unix: 9_000,
                    winddown_end_unix: 10_000,
                    corruption_points: HashMap::new(),
                    suppression_points: HashMap::new(),
                },
            );
        }
        // Mark 30_001_002 as an occupied FW warzone system to test HS vs WZ ranking
        snap.fw_system_occupations.insert(
            30_001_002,
            FwSystemOccupationRecord {
                solar_system_id: 30_001_002,
                owner_faction_id: 500_004,
                occupier_faction_id: 500_004,
                victory_points: 0,
                victory_points_threshold: 3_000,
            },
        );
    });

    // First call spawns FOB structure & ball; second call hits existing-ball branch cleanly
    let fob1 = ensure_campaign_pirate_fob(&ctx, 1).await.unwrap();
    assert_eq!(ensure_campaign_pirate_fob(&ctx, 1).await, Some(fob1));
    let fob2 = ensure_campaign_pirate_fob(&ctx, 2).await.unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        let s1 = &snap.structures[&fob1];
        assert_eq!(s1.name, "Guristas Forward Operating Base");
        assert_eq!((s1.shield_hp, s1.armor_hp, s1.hull_hp), (2_500.0, 2_500.0, 3_500.0));
        assert_eq!(snap.structures[&fob2].name, "Angel Cartel Forward Operating Base");
        snap.structures.get_mut(&fob2).unwrap().state = "Destroyed".into();
    });
    assert_eq!(ensure_campaign_pirate_fob(&ctx, 2).await, None);

    // Stage-5 suppression gate on system_states (stage == 5 blocks; stage == 4 allows!)
    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states.entry(fob_sys).or_default().insurgency_suppression_stage = 5;
    });
    assert_eq!(try_spread_insurgency_from_system(&ctx, 1, fob_sys), None);
    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states.entry(fob_sys).or_default().insurgency_suppression_stage = 4;
        // Stage 4 points (800) does NOT block; Stage 5 points (801) DOES block!
        snap.insurgency_runtime
            .campaigns
            .get_mut(&1)
            .unwrap()
            .suppression_points
            .insert(fob_sys, 801);
    });
    assert_eq!(try_spread_insurgency_from_system(&ctx, 1, fob_sys), None);
    ctx.db.with_mut_snapshot(|snap| {
        snap.insurgency_runtime
            .campaigns
            .get_mut(&1)
            .unwrap()
            .suppression_points
            .insert(fob_sys, 800);
    });

    // 1st spread from 30_001_001 (low-sec -> has_hs_covered == false):
    // Picks HS system 30_001_004 (sec 0.5) BEFORE WZ system 30_001_002, rejecting all 5 invalid neighbors!
    assert_eq!(
        try_spread_insurgency_from_system(&ctx, 1, fob_sys),
        Some(30_001_004)
    );
    // 2nd spread from 30_001_001 (now has_hs_covered == true):
    // Picks WZ neighbor 30_001_002 BEFORE non-WZ neighbor 30_001_003!
    assert_eq!(
        try_spread_insurgency_from_system(&ctx, 1, fob_sys),
        Some(30_001_002)
    );
    // 3rd spread from 30_001_002 (no uncovered direct neighbors since 30_001_001 is covered):
    // Suppress 30_001_001 to Stage 5 so fallback MUST skip 30_001_001 and spread from 30_001_004 -> 30_001_005 (sec 0.0)!
    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states.entry(fob_sys).or_default().insurgency_suppression_stage = 5;
    });
    assert_eq!(
        try_spread_insurgency_from_system(&ctx, 1, 30_001_002),
        Some(30_001_005)
    );
    // Next fallback spreads 30_001_005 -> 30_001_006 (sec 0.84), and once exhausted returns None!
    assert_eq!(
        try_spread_insurgency_from_system(&ctx, 1, 30_001_002),
        Some(30_001_006)
    );
    assert_eq!(try_spread_insurgency_from_system(&ctx, 1, 30_001_002), None);
}

#[test]
fn evaluate_and_resolve_campaign_ambition_boundaries_payout_math_and_antipirate_outcome() {
    let now = 1_750_010_000_u64;
    let ctx = build_insurgency_ctx(now);
    let fob_sys = 30_001_001_u32;
    let systems = vec![30_001_001_u32, 30_001_002, 30_001_003, 30_001_004, 30_001_005];

    let p_lead = 801_i64;
    let p_half = 802_i64;
    let e_loser = 803_i64;
    ctx.db.with_mut_snapshot(|snap| {
        seed_pilot(snap, p_lead, Some(FACTION_GURISTAS), fob_sys);
        seed_pilot(snap, p_half, Some(FACTION_ANGEL_CARTEL), fob_sys);
        seed_pilot(snap, e_loser, Some(500_001), fob_sys);
        snap.fw_system_occupations.insert(
            fob_sys,
            FwSystemOccupationRecord {
                solar_system_id: fob_sys,
                owner_faction_id: 500_001,
                occupier_faction_id: 500_001,
                victory_points: 500,
                victory_points_threshold: 3_000,
            },
        );
    });

    // 1. Exact Stage-5 count boundary: 4 Stage-5 corrupted + 1 Stage-4 (800 pts) -> Tie (not PirateWin!)
    ctx.db.with_mut_snapshot(|snap| {
        let mut corr = HashMap::new();
        for &s in &systems[..4] {
            corr.insert(s, 801);
        }
        corr.insert(systems[4], 800); // Stage 4
        snap.insurgency_runtime.campaigns.insert(
            10,
            PersistedInsurgencyCampaign {
                campaign_id: 10,
                pirate_faction_id: FACTION_GURISTAS,
                target_faction_id: 500_001,
                fob_solar_system_id: fob_sys,
                systems: systems.clone(),
                created_at_unix: now - 500,
                forecasting_end_unix: now + 100,
                active_end_unix: now + 5_000,
                winddown_end_unix: now + 9_000,
                corruption_points: corr,
                suppression_points: HashMap::new(),
            },
        );
    });
    let (tie_out, p0, e0, _) = evaluate_and_resolve_insurgency_campaign(&ctx, 10, None).unwrap();
    assert_eq!((tie_out.as_str(), p0, e0), ("Tie", 0, 0));
    // Early resolution clamps forecasting_end_unix, sets active_end_unix = now, winddown = now + WINDDOWN_DURATION_SECS
    ctx.db.with_snapshot(|snap| {
        let c = &snap.insurgency_runtime.campaigns[&10];
        assert_eq!(c.forecasting_end_unix, now);
        assert_eq!(c.active_end_unix, now);
        assert_eq!(c.winddown_end_unix, now + WINDDOWN_DURATION_SECS);
    });

    // 2. Natural PirateWin (5 Stage-5 corrupted with p_mod == 0 -> p_mod increments to 1):
    //    Pirate shares: p_lead = 775/1000 (>= 0.45 -> 1.0x = 1,500,000 LP & 150M ISK),
    //                   p_half = 225/1000 (0.225 / 0.45 = 0.5 -> 750,000 LP & 75M ISK),
    //    Empire loser:  e_loser = 100/100 (1.0 * 0.4 = 600,000 LP & 60M ISK).
    ctx.db.with_mut_snapshot(|snap| {
        let mut corr = HashMap::new();
        for &s in &systems {
            corr.insert(s, 801);
        }
        snap.insurgency_runtime.campaigns.insert(
            11,
            PersistedInsurgencyCampaign {
                campaign_id: 11,
                pirate_faction_id: FACTION_GURISTAS,
                target_faction_id: 500_001,
                fob_solar_system_id: fob_sys,
                systems: systems.clone(),
                created_at_unix: now - 1_000,
                forecasting_end_unix: now - 500,
                active_end_unix: now + 5_000,
                winddown_end_unix: now + 9_000,
                corruption_points: corr,
                suppression_points: HashMap::new(),
            },
        );
    });
    record_insurgency_contribution(&ctx, 11, p_lead, FACTION_GURISTAS, 775);
    record_insurgency_contribution(&ctx, 11, p_half, FACTION_ANGEL_CARTEL, 225);
    record_insurgency_contribution(&ctx, 11, e_loser, 500_001, 100);

    let (pw_out, p1, e1, payouts) =
        evaluate_and_resolve_insurgency_campaign(&ctx, 11, None).unwrap();
    assert_eq!((pw_out.as_str(), p1, e1), ("PirateWin", 1, 0));
    let pay_map: HashMap<i64, i64> = payouts.into_iter().map(|(cid, _, lp)| (cid, lp)).collect();
    assert_eq!(pay_map[&p_lead], 1_500_000);
    assert_eq!(pay_map[&p_half], 750_000);
    assert_eq!(pay_map[&e_loser], 600_000);

    ctx.db.with_snapshot(|snap| {
        assert_eq!(
            snap.insurgency_runtime.surrender_ihub_vulnerable_unix.get(&fob_sys).copied(),
            Some(now + SURRENDER_IHUB_VULNERABILITY_DELAY_SECS)
        );
        assert_eq!(snap.fw_system_occupations[&fob_sys].occupier_faction_id, FACTION_GURISTAS);
        assert_eq!(snap.fw_runtime.ihub_vulnerable.get(&fob_sys).copied(), Some(false));
        // Pilot balance increased by 150,000,000 ISK on top of 10,000,000 seed
        assert_eq!(snap.characters[&p_lead].balance, IskCents::from_isk(160_000_000));
        assert!(snap
            .wallet_journal
            .iter()
            .any(|j| j.character_id == p_lead && j.entry_type == "InsurgencyContributionPayout"));
    });

    // 3. Natural EmpireWin (5 Stage-5 suppressed on Angel Cartel where e_mod == 5 -> stays capped at 5)
    //    and forced "AntiPirateWin" branch triggering Zarzakh Horizon Siege!
    ctx.db.with_mut_snapshot(|snap| {
        snap.insurgency_runtime
            .empire_ambition_modifier
            .insert(FACTION_ANGEL_CARTEL, 5);
        let mut supp = HashMap::new();
        for &s in &systems {
            supp.insert(s, 900);
        }
        // Add 5 more systems at Stage 5 so suppressed_5 == 10 >= 5 + e_mod(5)
        for extra in 30_002_001_u32..=30_002_005 {
            supp.insert(extra, 900);
        }
        snap.insurgency_runtime.campaigns.insert(
            12,
            PersistedInsurgencyCampaign {
                campaign_id: 12,
                pirate_faction_id: FACTION_ANGEL_CARTEL,
                target_faction_id: 500_002,
                fob_solar_system_id: fob_sys,
                systems: systems.clone(),
                created_at_unix: now - 1_000,
                forecasting_end_unix: now - 500,
                active_end_unix: now - 10,
                winddown_end_unix: now + 1_000,
                corruption_points: HashMap::new(),
                suppression_points: supp,
            },
        );
        snap.insurgency_runtime.campaigns.insert(
            13,
            PersistedInsurgencyCampaign {
                campaign_id: 13,
                pirate_faction_id: FACTION_ANGEL_CARTEL,
                target_faction_id: 500_002,
                fob_solar_system_id: fob_sys,
                systems: vec![fob_sys],
                created_at_unix: now - 1_000,
                forecasting_end_unix: now - 500,
                active_end_unix: now - 10,
                winddown_end_unix: now + 1_000,
                corruption_points: HashMap::new(),
                suppression_points: HashMap::new(),
            },
        );
    });
    record_insurgency_contribution(&ctx, 12, e_loser, 500_002, 500);
    record_insurgency_contribution(&ctx, 12, p_lead, FACTION_ANGEL_CARTEL, 500);
    let (ew_out, p2, e2, ew_pay) =
        evaluate_and_resolve_insurgency_campaign(&ctx, 12, None).unwrap();
    assert_eq!((ew_out.as_str(), p2, e2), ("EmpireWin", 0, 5));
    let ew_map: HashMap<i64, i64> = ew_pay.into_iter().map(|(cid, _, lp)| (cid, lp)).collect();
    assert_eq!(ew_map[&e_loser], 1_500_000);
    assert_eq!(ew_map[&p_lead], 600_000);

    let (ap_out, _, _, _) =
        evaluate_and_resolve_insurgency_campaign(&ctx, 13, Some("AntiPirateWin")).unwrap();
    assert_eq!(ap_out, "AntiPirateWin");
    assert!(crate::services::cluster_macho_misc_services::is_zarzakh_horizon_siege_active(&ctx));
}

#[tokio::test]
async fn tick_insurgency_surrender_unlock_auto_frontline_phases_and_ambition_triggers() {
    let now = 1_750_050_000_u64;
    let ctx = build_insurgency_ctx(now);
    let fl_a = 30_001_001_u32;
    let fl_b = 30_001_002_u32;
    let rear_c = 30_001_003_u32;

    // 1. Surrender I-Hub unlock: fl_a due at `now` (occupier is pirate 500_010 -> restored to owner 500_001,
    //    VP maxed, fw_contested_pct = 100.0, ihub_vulnerable = true); fl_b due at `now + 10` (stays locked!).
    //    Also place a pirate pilot in REARGUARD rear_c so auto-seeding exercises the fallback frontline lookup
    //    and 3-pass BFS expansion to 4 systems + stale insurgency_instances pruning!
    ctx.db.with_mut_snapshot(|snap| {
        snap.insurgency_runtime.campaigns.clear();
        seed_pilot(snap, 901, Some(FACTION_GURISTAS), rear_c);
        for (sys, occ_fac, own_fac) in [
            (fl_a, FACTION_GURISTAS, 500_001_i64),
            (fl_b, 500_004, 500_004),
            (rear_c, 500_001, 500_001),
        ] {
            snap.fw_system_occupations.insert(
                sys,
                FwSystemOccupationRecord {
                    solar_system_id: sys,
                    occupier_faction_id: occ_fac,
                    owner_faction_id: own_fac,
                    victory_points: 100,
                    victory_points_threshold: 3_000,
                },
            );
            snap.system_states.entry(sys).or_default();
        }
        snap.fw_runtime.ihub_vulnerable.insert(fl_a, false);
        snap.fw_runtime.ihub_vulnerable.insert(fl_b, false);
        snap.insurgency_runtime.surrender_ihub_vulnerable_unix.insert(fl_a, now);
        snap.insurgency_runtime.surrender_ihub_vulnerable_unix.insert(fl_b, now + 10);

        // Stale non-PirateStronghold site in fl_a should be pruned, while PirateStronghold is kept
        snap.insurgency_runtime
            .insurgency_instances
            .push(PersistedInsurgencySiteRecord {
                instance_id: 1,
                solar_system_id: fl_a,
                site_type: "MiningAmbush".into(),
                name: "Mining Ambush".into(),
                corruption_reward: 100.0,
                completed: false,
            });
    });

    // Tick 1: unlocks fl_a I-Hub, auto-seeds campaign in Forecasting phase (4 systems via BFS)
    tick_insurgency_campaigns(&ctx, now).await;

    let cid = ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.fw_system_occupations[&fl_a].occupier_faction_id, 500_001);
        assert_eq!(snap.fw_system_occupations[&fl_a].victory_points, 3_000);
        assert_eq!(snap.system_states[&fl_a].fw_contested_pct, 100.0);
        assert_eq!(snap.fw_runtime.ihub_vulnerable.get(&fl_a).copied(), Some(true));
        assert_eq!(snap.fw_runtime.ihub_vulnerable.get(&fl_b).copied(), Some(false));
        let (&id, camp) = snap.insurgency_runtime.campaigns.iter().next().unwrap();
        assert_eq!(snap.insurgency_runtime.insurgency_instances.len(), 1);
        assert_eq!(
            snap.insurgency_runtime.insurgency_instances[0].site_type,
            "PirateStronghold"
        );
        assert_eq!(
            snap.insurgency_runtime.insurgency_instances[0].instance_id,
            PIRATE_FOB_STRUCTURE_BASE_ID + id
        );
        assert_eq!(camp.systems.len(), 4);
        assert!(!snap.insurgency_runtime.active_state_notified.contains(&id));
        id
    });

    // 2. Move into PHASE_ACTIVE with 5 Stage-5 suppressed systems while FOB is NOT destroyed:
    //    notifies active state, does NOT resolve yet; then destroy FOB -> resolves EmpireWin on next tick!
    let fob_id = PIRATE_FOB_STRUCTURE_BASE_ID + cid;
    ctx.db.with_mut_snapshot(|snap| {
        let camp = snap.insurgency_runtime.campaigns.get_mut(&cid).unwrap();
        camp.forecasting_end_unix = now - 100;
        camp.active_end_unix = now + 5_000;
        camp.winddown_end_unix = now + 10_000;
        if !camp.systems.contains(&30_001_005) {
            camp.systems.push(30_001_005);
        }
        for &s in &camp.systems {
            camp.suppression_points.insert(s, 900);
        }
    });
    tick_insurgency_campaigns(&ctx, now + 1).await;
    ctx.db.with_snapshot(|snap| {
        assert!(snap.insurgency_runtime.active_state_notified.contains(&cid));
        assert!(!snap.insurgency_runtime.campaign_outcomes.contains_key(&cid));
    });

    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.get_mut(&fob_id).unwrap().state = "Destroyed".into();
    });
    tick_insurgency_campaigns(&ctx, now + 2).await;
    assert_eq!(
        ctx.db.with_snapshot(|s| s.insurgency_runtime.campaign_outcomes.get(&cid).cloned()),
        Some("EmpireWin".into())
    );

    // 3. Also verify PHASE_WINDDOWN unresolved -> Resolve branch, and End cleanup!
    let wd_cid = 777_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.insurgency_runtime.campaigns.insert(
            wd_cid,
            PersistedInsurgencyCampaign {
                campaign_id: wd_cid,
                pirate_faction_id: FACTION_GURISTAS,
                target_faction_id: 500_001,
                fob_solar_system_id: fl_a,
                systems: vec![fl_a],
                created_at_unix: now - 5_000,
                forecasting_end_unix: now - 4_000,
                active_end_unix: now - 100,
                winddown_end_unix: now + 500,
                corruption_points: HashMap::new(),
                suppression_points: HashMap::new(),
            },
        );
    });
    tick_insurgency_campaigns(&ctx, now + 3).await;
    assert_eq!(
        ctx.db.with_snapshot(|s| s.insurgency_runtime.campaign_outcomes.get(&wd_cid).cloned()),
        Some("Tie".into())
    );

    // Advance past winddown_end_unix for both campaigns -> ends and removes them
    tick_insurgency_campaigns(&ctx, now + 200_000).await;
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.insurgency_runtime.campaigns.contains_key(&cid));
        assert!(!snap.insurgency_runtime.campaigns.contains_key(&wd_cid));
    });
}
