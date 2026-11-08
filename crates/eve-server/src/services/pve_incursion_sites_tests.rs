//! Unit tests for Sansha Incursion sites, payout scaling, multi-wave destruction,
//! Mothership finale, and RPC handlers (`pve_incursion_sites`).

use super::*;
use crate::router::ServerContext;
use crate::services::pve_incursion_lifecycle::eligible_incursion_constellations;
use crate::services::pve_special_sites::with_pve_special_reg;
use eve_db::clock::VirtualServerClock;
use eve_db::{CharacterRecord, IskCents, NeuralAttributes, WorldRepository};
use eve_sde::SdeRegistry;
use glam::DVec3;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

#[cfg(test)]
#[path = "pve_incursion_rpc_handlers_tests.rs"]
mod pve_incursion_rpc_handlers_tests;

pub(super) fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed()))
        .clone()
}

pub(super) fn build_test_ctx() -> Arc<ServerContext> {
    let db = Arc::new(WorldRepository::new_empty());
    db.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_800_000_000)));
    Arc::new(ServerContext::with_sde_and_repository(shared_sde(), db))
}

pub(super) fn insert_in_space_pilot(
    ctx: &ServerContext,
    char_id: i64,
    sys_id: SolarSystemId,
    fleet_id: Option<i64>,
    pos: DVec3,
    sec_status: f64,
) -> i64 {
    let sys_rec = ctx.sde.solar_systems.get(&sys_id).unwrap();
    let ship_id = 500_000 + char_id;
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.insert(
            char_id,
            CharacterRecord {
                activity_status: None,
                character_id: char_id,
                user_id: char_id as u64,
                name: format!("Pilot-{char_id}"),
                corporation_id: 1_000_006,
                alliance_id: None,
                war_faction_id: None,
                station_id: None,
                structure_id: None,
                solar_system_id: sys_id,
                constellation_id: sys_rec.constellation_id,
                region_id: sys_rec.region_id,
                active_ship_id: ship_id,
                balance: IskCents::from_isk(1_000_000),
                plex_balance: 0,
                loyalty_points: HashMap::new(),
                standings: HashMap::new(),
                security_status: sec_status,
                clone_grade: 2,
                home_station_id: 60_003_760,
                race_id: 1,
                bloodline_id: 1,
                ancestry_id: 11,
                school_id: 17,
                neural: NeuralAttributes::default(),
                skill_points: 10_000_000.0,
                free_skill_points: 0.0,
                last_clone_jump_unix: 0,
                jump_fatigue_minutes: 0.0,
                jump_cooldown_until_unix: 0,
                jump_fatigue_until_unix: 0,
                description: String::new(),
                title: String::new(),
                gender: 0,
                create_date_unix: 1_700_000_000,
                fleet_id,
                autopilot_waypoints: Vec::new(),
                version: 1,
                system_epoch: 1,
            },
        );
        snap.item_positions.insert(ship_id, [pos.x, pos.y, pos.z]);
    });
    ship_id
}

#[test]
fn test_encounter_composition_payout_bands_and_overstack() {
    let ctx = build_test_ctx();

    let (ms_boss, ms_esc) = get_sansha_encounter_composition(100, 5158, "Mothership");
    assert_eq!(ms_boss, 2192);
    assert_eq!(ms_esc.len(), 9);
    assert_eq!(get_sansha_encounter_composition(100, 5175, "Headquarters").0, 2845);
    assert_eq!(get_sansha_encounter_composition(100, 5171, "Headquarters").0, 2935);
    assert_eq!(get_sansha_encounter_composition(100, 5168, "Assault").0, 3492);
    assert_eq!(get_sansha_encounter_composition(100, 5167, "Assault").0, 2933);
    assert_eq!(get_sansha_encounter_composition(100, 5165, "Vanguard").0, 2933);
    assert_eq!(get_sansha_encounter_composition(100, 5166, "Vanguard").0, 3492);
    assert_eq!(get_sansha_encounter_composition(100, 5164, "Staging").0, 3492);

    // High-sec vs Low/Null-sec payout bands
    let lowsec = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| s.solar_system_id < 30_100_000 && s.security_status < 0.45)
        .unwrap()
        .solar_system_id;
    assert_eq!(
        incursion_site_payout_band(&ctx.sde, 30_000_142, "Staging"),
        (1, 5, 3_500_000, 400)
    );
    assert_eq!(
        incursion_site_payout_band(&ctx.sde, 30_000_142, "Vanguard"),
        (5, 10, 10_395_000, 1_400)
    );
    assert_eq!(
        incursion_site_payout_band(&ctx.sde, lowsec, "Vanguard"),
        (5, 15, 15_000_000, 2_000)
    );
    assert_eq!(
        incursion_site_payout_band(&ctx.sde, 30_000_142, "Assault"),
        (1, 20, 18_200_000, 3_500)
    );
    assert_eq!(
        incursion_site_payout_band(&ctx.sde, lowsec, "Assault"),
        (1, 30, 26_000_000, 5_000)
    );
    assert_eq!(
        incursion_site_payout_band(&ctx.sde, 30_000_142, "Headquarters"),
        (20, 40, 31_500_000, 7_000)
    );
    assert_eq!(
        incursion_site_payout_band(&ctx.sde, lowsec, "Headquarters"),
        (20, 60, 45_000_000, 10_000)
    );
    assert_eq!(
        incursion_site_payout_band(&ctx.sde, 30_000_142, "Mothership"),
        (40, 80, 63_000_000, 14_000)
    );
    assert_eq!(
        incursion_site_payout_band(&ctx.sde, lowsec, "Mothership"),
        (40, 120, 90_000_000, 20_000)
    );

    assert_eq!(incursion_overstack_ratio(0), 1.0);
    assert_eq!(incursion_overstack_ratio(1), 0.925);
    assert_eq!(incursion_overstack_ratio(2), 0.845);
    assert_eq!(incursion_overstack_ratio(3), 0.76);
    assert_eq!(incursion_overstack_ratio(4), 0.0);

    // Derive site position in Jita and unknown system fallback
    let p0 = derive_incursion_site_position(&ctx.sde, 30_000_142, 0);
    let p1 = derive_incursion_site_position(&ctx.sde, 30_000_142, 1);
    assert_ne!(p0, p1);
    let fallback = derive_incursion_site_position(&ctx.sde, 99_999_999, 0);
    assert_eq!(fallback, DVec3::new(100_000_000.0, 50_000_000.0, -80_000_000.0));
}

#[tokio::test]
async fn test_incursion_site_completion_scaling_and_mothership_finale() {
    let ctx = build_test_ctx();
    let cid = eligible_incursion_constellations(&ctx)[0].0;
    let inc = with_pve_special_reg(&ctx, |reg| {
        reg.initialized = true;
        seed_incursion_for_constellation(&ctx, reg, cid);
        reg.incursions[&cid].clone()
    });
    assert!(get_incursion_effects_for_system(&ctx, inc.staging_solar_system_id).is_some());

    let vg_site = inc.sites.iter().find(|s| s.tier == "Vanguard").unwrap().clone();
    let ms_site = inc
        .sites
        .iter()
        .find(|s| s.tier == "Mothership")
        .unwrap()
        .clone();

    // Mothership is locked while influence > 0.0
    let err = process_incursion_site_completion_and_payout(
        &ctx,
        Some(92_000_001),
        None,
        ms_site.solar_system_id,
        &ms_site.site_id,
        None,
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "MothershipSiteLocked");

    // Not found error
    let err = process_incursion_site_completion_and_payout(
        &ctx,
        Some(92_000_001),
        None,
        vg_site.solar_system_id,
        "INC-NONE-9999",
        None,
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "IncursionSiteNotFound");

    // Vanguard site fails while wave < total_waves
    let err = process_incursion_site_completion_and_payout(
        &ctx,
        Some(92_000_001),
        None,
        vg_site.solar_system_id,
        &vg_site.site_id,
        None,
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "IncursionSiteNpcsRemaining");

    // Advance waves to 3, mark escorts destroyed, test mark_incursion_boss_destroyed
    with_pve_special_reg(&ctx, |reg| {
        reg.incursion_site_waves
            .get_mut(&vg_site.site_id)
            .unwrap()
            .current_wave = 3;
        let s = reg
            .incursions
            .get_mut(&cid)
            .unwrap()
            .sites
            .iter_mut()
            .find(|s| s.site_id == vg_site.site_id)
            .unwrap();
        for e in &mut s.escort_spawns {
            e.destroyed = true;
        }
    });
    assert!(mark_incursion_boss_destroyed(
        &ctx,
        vg_site.solar_system_id,
        vg_site.boss_npc_ball_id
    ));

    // Set killer_char_id to pilot 1 and test KillerAttributionMismatch when pilot 2 calls
    with_pve_special_reg(&ctx, |reg| {
        let s = reg
            .incursions
            .get_mut(&cid)
            .unwrap()
            .sites
            .iter_mut()
            .find(|s| s.site_id == vg_site.site_id)
            .unwrap();
        s.killer_char_id = Some(92_000_001);
        s.killer_fleet_id = Some(777);
    });
    let err = process_incursion_site_completion_and_payout(
        &ctx,
        Some(92_000_002),
        Some(888),
        vg_site.solar_system_id,
        &vg_site.site_id,
        None,
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "KillerAttributionMismatch");

    // Insert 5 on-grid fleet pilots + 1 off-grid fleet pilot + 1 non-fleet pilot
    let fleet_id = 777i64;
    for i in 0..5 {
        insert_in_space_pilot(
            &ctx,
            92_000_001 + i,
            vg_site.solar_system_id,
            Some(fleet_id),
            DVec3::from_array(vg_site.position) + DVec3::new(1_000.0 * (i as f64), 0.0, 0.0),
            0.0,
        );
    }
    // Off-grid pilot (> 500km away)
    insert_in_space_pilot(
        &ctx,
        92_000_050,
        vg_site.solar_system_id,
        Some(fleet_id),
        DVec3::from_array(vg_site.position) + DVec3::new(900_000.0, 0.0, 0.0),
        0.0,
    );
    // Non-fleet pilot on-grid
    insert_in_space_pilot(
        &ctx,
        92_000_051,
        vg_site.solar_system_id,
        None,
        DVec3::from_array(vg_site.position),
        0.0,
    );

    let res = process_incursion_site_completion_and_payout(
        &ctx,
        Some(92_000_001),
        Some(fleet_id),
        vg_site.solar_system_id,
        &vg_site.site_id,
        None,
    )
    .await
    .unwrap();
    assert_eq!(res.fleet_size, 5);
    assert_eq!(res.fleet_scale_factor, 1.0);
    assert_eq!(res.isk_per_pilot, vg_site.base_isk);
    assert_eq!(res.concord_lp_per_pilot, vg_site.base_concord_lp);
    assert!((res.influence - 0.95).abs() < 1e-6);
    assert!(!res.incursion_ended);

    // Verify off-grid & non-fleet pilots got 0 payout while fleet pilots got full payout
    ctx.db.with_snapshot(|snap| {
        let p1 = &snap.characters[&92_000_001];
        assert_eq!(p1.balance, IskCents::from_isk(1_000_000 + vg_site.base_isk));
        assert_eq!(p1.loyalty_points[&1_000_125], vg_site.base_concord_lp);
        assert_eq!(snap.characters[&92_000_050].balance, IskCents::from_isk(1_000_000));
        assert_eq!(snap.characters[&92_000_051].balance, IskCents::from_isk(1_000_000));
    });

    // Test overstack scaling (+1 pilot above max_optimal_pilots = 11 pilots -> 0.925)
    for i in 5..11 {
        insert_in_space_pilot(
            &ctx,
            92_000_001 + i,
            vg_site.solar_system_id,
            Some(fleet_id),
            DVec3::from_array(vg_site.position),
            0.0,
        );
    }
    with_pve_special_reg(&ctx, |reg| {
        reg.incursion_site_waves
            .get_mut(&vg_site.site_id)
            .unwrap()
            .current_wave = 3;
        let s = reg
            .incursions
            .get_mut(&cid)
            .unwrap()
            .sites
            .iter_mut()
            .find(|s| s.site_id == vg_site.site_id)
            .unwrap();
        for e in &mut s.escort_spawns {
            e.destroyed = true;
        }
        s.boss_destroyed = true;
    });
    let over_res = process_incursion_site_completion_and_payout(
        &ctx,
        Some(92_000_001),
        Some(fleet_id),
        vg_site.solar_system_id,
        &vg_site.site_id,
        None,
    )
    .await
    .unwrap();
    assert_eq!(over_res.fleet_size, 11);
    assert!((over_res.fleet_scale_factor - 0.925).abs() < 1e-6);

    // Drop influence to 0.03 and complete one Vanguard site -> unlocks Mothership!
    with_pve_special_reg(&ctx, |reg| {
        reg.incursions.get_mut(&cid).unwrap().influence = 0.03;
        reg.incursion_site_waves
            .get_mut(&vg_site.site_id)
            .unwrap()
            .current_wave = 3;
        let s = reg
            .incursions
            .get_mut(&cid)
            .unwrap()
            .sites
            .iter_mut()
            .find(|s| s.site_id == vg_site.site_id)
            .unwrap();
        for e in &mut s.escort_spawns {
            e.destroyed = true;
        }
        s.boss_destroyed = true;
    });
    let unlock_res = process_incursion_site_completion_and_payout(
        &ctx,
        Some(92_000_001),
        Some(fleet_id),
        vg_site.solar_system_id,
        &vg_site.site_id,
        Some(&[92_000_001]),
    )
    .await
    .unwrap();
    assert_eq!(unlock_res.influence, 0.0);
    assert!(unlock_res.mothership_spawned);
    assert_eq!(unlock_res.state, "Mobilizing");
    // Under-sized 1-pilot fleet in Vanguard (min 5) gets 1/5 = 0.2 payout
    assert!((unlock_res.fleet_scale_factor - 0.2).abs() < 1e-6);

    // Now defeat Mothership HQ via on_incursion_boss_destroyed: first clear all escort waves, then boss!
    let early_boss = on_incursion_boss_destroyed(
        &ctx,
        ms_site.solar_system_id,
        ms_site.boss_npc_ball_id,
        Some(92_000_001),
    )
    .await
    .unwrap();
    assert_eq!(early_boss.unwrap_err().code, "IncursionSiteNpcsRemaining");

    let sys_handle = ctx.systems.get_or_spawn(ms_site.solar_system_id);
    let mut ms_ended = false;
    loop {
        let next_esc = with_pve_special_reg(&ctx, |reg| {
            reg.incursions[&cid]
                .sites
                .iter()
                .find(|s| s.site_id == ms_site.site_id)
                .and_then(|s| s.escort_spawns.iter().find(|e| !e.destroyed))
                .map(|e| e.npc_ball_id)
        });
        let Some(esc_bid) = next_esc else {
            break;
        };
        let _ = sys_handle.remove_ball(esc_bid).await;
        if let Some(Ok(fin)) = on_incursion_boss_destroyed(
            &ctx,
            ms_site.solar_system_id,
            esc_bid,
            Some(92_000_001),
        )
        .await
        {
            assert!(fin.incursion_ended);
            assert_eq!(fin.state, "Withdrawing");
            ms_ended = true;
            break;
        }
    }
    if !ms_ended {
        let _ = sys_handle.remove_ball(ms_site.boss_npc_ball_id).await;
        let fin = on_incursion_boss_destroyed(
            &ctx,
            ms_site.solar_system_id,
            ms_site.boss_npc_ball_id,
            Some(92_000_001),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(fin.incursion_ended);
        assert_eq!(fin.state, "Withdrawing");
    }
    with_pve_special_reg(&ctx, |reg| {
        let inc_after = &reg.incursions[&cid];
        assert!(inc_after.mothership_defeated);
        assert_eq!(inc_after.state, "Withdrawing");
    });
    // Verify security status transaction was recorded for killer 92_000_001
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.combat_crimewatch_state.sec_status_transactions[&92_000_001].is_empty());
    });
}
