//! Unit tests for `structure_anchoring_rules` (spatial clearances, trade hubs, FLEX sov, corp age, ADM, Pochven CO standing).

#![forbid(unsafe_code)]

use super::*;
use crate::services::fleet_social::fleet_social_fleet_ops::fleet_social_fleet_ops_tests::{
    clone_char_with, test_ctx_with_clock,
};
use crate::services::station_structure::structure_lifecycle::structure_profile;
use eve_db::{
    CorpGovState, CorporationRecord, SystemDynamicState, TrainedSkillRecord,
    UpwellStructureRecord,
};

#[test]
fn trade_hub_and_anchor_position_restrictions() {
    let ctx = test_ctx_with_clock(1_800_000_000);
    for &th in &TRADE_HUB_SYSTEM_IDS {
        assert!(is_trade_hub_system(th));
    }
    assert!(!is_trade_hub_system(30_000_144));

    let astra_prof = structure_profile(&ctx, 35_832).expect("Astrahus profile");
    let flex_prof = structure_profile(&ctx, 35_841).expect("Ansiblex profile");
    let keep_prof = structure_profile(&ctx, 35_834).expect("Keepstar profile");
    let palatine_prof = structure_profile(&ctx, 40_340).expect("Palatine Keepstar profile");
    let mut tatara_prof = structure_profile(&ctx, 35_836).expect("Tatara profile (disallow in highsec)");
    tatara_prof.disallow_in_highsec = true;

    let null_sys = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| s.security_status < -0.2 && s.solar_system_id < 31_000_000 && s.region_id != POCHVEN_REGION_ID)
        .map(|s| s.solar_system_id)
        .unwrap();
    let poch_sys = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| s.region_id == POCHVEN_REGION_ID)
        .map(|s| s.solar_system_id)
        .unwrap();
    let deep_pos = DVec3::new(5.0e11, 5.0e11, 5.0e11);

    ctx.db.with_mut_snapshot(|snap| {
        // Non-finite or out-of-bounds
        assert_eq!(
            check_anchor_position(&ctx, snap, &astra_prof, null_sys, DVec3::new(f64::NAN, 0.0, 0.0))
                .unwrap_err()
                .code,
            "InvalidAnchorCoordinates"
        );
        assert_eq!(
            check_anchor_position(&ctx, snap, &astra_prof, null_sys, DVec3::new(2.0e14, 0.0, 0.0))
                .unwrap_err()
                .code,
            "InvalidAnchorCoordinates"
        );

        // Thera (31000005), Zarzakh (30100000), Pochven, Trade Hub (Jita 30000142)
        assert_eq!(
            check_anchor_position(&ctx, snap, &astra_prof, 31_000_005, deep_pos).unwrap_err().code,
            "AnchoringDisallowedInTheraOrShattered"
        );
        assert_eq!(
            check_anchor_position(&ctx, snap, &astra_prof, 30_100_000, deep_pos).unwrap_err().code,
            "AnchoringDisallowedInZarzakh"
        );
        assert_eq!(
            check_anchor_position(&ctx, snap, &astra_prof, poch_sys, deep_pos).unwrap_err().code,
            "AnchoringDisallowedInPochven"
        );
        assert_eq!(
            check_anchor_position(&ctx, snap, &astra_prof, 30_000_142, deep_pos).unwrap_err().code,
            "AnchoringDisallowedInTradeHub"
        );

        // FLEX outside sov null and Tatara in highsec (Perimeter 30000144)
        assert_eq!(
            check_anchor_position(&ctx, snap, &flex_prof, 30_000_144, deep_pos).unwrap_err().code,
            "CannotAnchorFlexOutsideSovNull"
        );
        assert_eq!(
            check_anchor_position(&ctx, snap, &tatara_prof, 30_000_144, deep_pos).unwrap_err().code,
            "CannotAnchorInHighSec"
        );

        // Valid deep space in nullsec succeeds
        assert!(check_anchor_position(&ctx, snap, &astra_prof, null_sys, deep_pos).is_ok());

        // Existing structure at deep_pos blocks anchoring within 1,000 km
        snap.structures.insert(
            1_000_000_900_001,
            UpwellStructureRecord {
                structure_id: 1_000_000_900_001,
                type_id: 35_834,
                owner_corp_id: 98_000_001,
                solar_system_id: null_sys,
                name: "Existing Keepstar".into(),
                position: deep_pos.to_array(),
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 500,
                shield_hp: 1.0,
                armor_hp: 1.0,
                hull_hp: 1.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
        assert_eq!(
            check_anchor_position(&ctx, snap, &astra_prof, null_sys, deep_pos + DVec3::new(100_000.0, 0.0, 0.0))
                .unwrap_err()
                .code,
            "TooCloseToStructureOrStation"
        );
        assert_eq!(
            check_anchor_position(&ctx, snap, &keep_prof, null_sys, deep_pos + DVec3::new(5_000_000.0, 0.0, 0.0))
                .unwrap_err()
                .code,
            "KeepstarPerSystemLimit"
        );

        // Palatine Keepstar cluster limit and FLEX per-system limit
        snap.structures.insert(
            1_000_000_900_002,
            UpwellStructureRecord {
                structure_id: 1_000_000_900_002,
                type_id: 40_340,
                owner_corp_id: 98_000_001,
                solar_system_id: 30_000_144,
                name: "Palatine".into(),
                position: [0.0; 3],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 500,
                shield_hp: 1.0,
                armor_hp: 1.0,
                hull_hp: 1.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
        assert_eq!(
            check_anchor_position(&ctx, snap, &palatine_prof, null_sys, deep_pos + DVec3::new(5_000_000.0, 0.0, 0.0))
                .unwrap_err()
                .code,
            "PalatineKeepstarLimitReached"
        );

        snap.structures.insert(
            1_000_000_900_003,
            UpwellStructureRecord {
                structure_id: 1_000_000_900_003,
                type_id: 35_841,
                owner_corp_id: 98_000_001,
                solar_system_id: null_sys,
                name: "Ansiblex".into(),
                position: (deep_pos + DVec3::new(10_000_000.0, 0.0, 0.0)).to_array(),
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 500,
                shield_hp: 1.0,
                armor_hp: 1.0,
                hull_hp: 1.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
        assert_eq!(
            check_anchor_position(&ctx, snap, &flex_prof, null_sys, deep_pos + DVec3::new(20_000_000.0, 0.0, 0.0))
                .unwrap_err()
                .code,
            "FlexStructurePerSystemLimit"
        );
    });
}

#[test]
fn flex_sovereignty_corp_age_adm_and_pochven_customs_standing() {
    let ctx = test_ctx_with_clock(1_800_000_000);
    let astra_prof = structure_profile(&ctx, 35_832).unwrap();
    let fort_prof = structure_profile(&ctx, 35_833).unwrap();
    let flex_prof = structure_profile(&ctx, 35_841).unwrap();
    let null_sys = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| s.security_status < -0.2 && s.solar_system_id < 31_000_000 && s.region_id != POCHVEN_REGION_ID)
        .map(|s| s.solar_system_id)
        .unwrap();
    let poch_sys = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| s.region_id == POCHVEN_REGION_ID)
        .map(|s| s.solar_system_id)
        .unwrap();
    let ship_pos = DVec3::new(6.0e11, 6.0e11, 6.0e11);
    let corp_id = 98_500_001_i64;
    let ally_id = 99_500_001_i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.insert(
            corp_id,
            { let mut __corp = CorporationRecord::default();
                __corp.corporation_id = corp_id;
                __corp.alliance_id = Some(ally_id);
                __corp },
        );
        // Young corporation (< 7 days) rejected
        snap.corp_governance.insert(
            corp_id,
            CorpGovState {
                created_unix: 1_800_000_000 - 86_400,
                ..Default::default()
            },
        );
        assert_eq!(
            validate_and_resolve_anchor(&ctx, snap, &astra_prof, null_sys, corp_id, ship_pos, None, 1_800_000_000)
                .unwrap_err()
                .code,
            "CorporationTooYoungToAnchor"
        );

        // Age corporation to > 7 days; test ship distance boundaries (< 50km and > 75km rejected)
        snap.corp_governance.get_mut(&corp_id).unwrap().created_unix = 1_800_000_000 - 10 * 86_400;
        assert_eq!(
            validate_and_resolve_anchor(
                &ctx,
                snap,
                &astra_prof,
                null_sys,
                corp_id,
                ship_pos,
                Some(ship_pos + DVec3::new(20_000.0, 0.0, 0.0)),
                1_800_000_000,
            )
            .unwrap_err()
            .code,
            "InvalidAnchorDistanceFromShip"
        );

        // FLEX requires sovereignty + Sov Hub (32458)
        assert_eq!(
            check_flex_system_sovereignty(snap, null_sys, corp_id, 35_841).unwrap_err().code,
            "FlexAnchoringRequiresSystemSovereignty"
        );
        snap.system_states.insert(
            null_sys,
            SystemDynamicState {
                solar_system_id: null_sys,
                sov_alliance_id: Some(ally_id),
                strategic_index: 2,
                military_index: 5,
                industrial_index: 5,
                ..Default::default()
            },
        );
        snap.structures.insert(
            1_000_000_910_001,
            UpwellStructureRecord {
                structure_id: 1_000_000_910_001,
                type_id: crate::services::sov_services::TYPE_SOVEREIGNTY_HUB,
                owner_corp_id: corp_id,
                solar_system_id: null_sys,
                name: "Sov Hub".into(),
                position: [0.0; 3],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 500,
                shield_hp: 1.0,
                armor_hp: 1.0,
                hull_hp: 1.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
        let (resolved_pos, flex_dur) = validate_and_resolve_anchor(
            &ctx,
            snap,
            &flex_prof,
            null_sys,
            corp_id,
            ship_pos,
            None,
            1_800_000_000,
        )
        .expect("FLEX in friendly sov");
        assert!((resolved_pos.distance(ship_pos) - DEFAULT_ANCHOR_SHIP_OFFSET_M).abs() < 1e-3);
        assert_eq!(flex_dur, ANSIBLEX_BASE_ANCHORING_SECS * 3);

        // Hostile sov with high ADM (> 4.0) blocks Medium structure (Astrahus), extends Large (Fortizar)
        snap.system_states.get_mut(&null_sys).unwrap().sov_alliance_id = Some(99_999_999);
        assert_eq!(
            validate_and_resolve_anchor(&ctx, snap, &astra_prof, null_sys, corp_id, ship_pos, None, 1_800_000_000)
                .unwrap_err()
                .code,
            "AdmBlocksHostileMediumStructure"
        );
        let (_, fort_dur) = validate_and_resolve_anchor(
            &ctx,
            snap,
            &fort_prof,
            null_sys,
            corp_id,
            ship_pos,
            None,
            1_800_000_000,
        )
        .expect("Fortizar allowed in hostile high-ADM sov with penalty");
        assert_eq!(fort_dur, ANCHORING_SECS + 2 * 86_400);

        // Pochven Customs Office standing check
        let cid = 910_301_i64;
        let ch = clone_char_with(snap, cid, "Pochven PI", corp_id, poch_sys, 100_000_001);
        snap.characters.insert(cid, ch);
        assert!(validate_pochven_customs_office_standing(snap, cid, null_sys, &ctx.sde).is_ok());
        assert_eq!(
            validate_pochven_customs_office_standing(snap, cid, poch_sys, &ctx.sde).unwrap_err().code,
            "InsufficientTriglavianStanding"
        );

        // Raw standing 1.0 + Connections V (3359) -> effective 2.8 >= 1.5
        snap.characters.get_mut(&cid).unwrap().standings.insert(500_026, 1.0);
        snap.skills.entry(cid).or_default().insert(
            3_359,
            TrainedSkillRecord {
                type_id: 3_359,
                level: 5,
                skill_points: 256_000.0,
            },
        );
        assert!(validate_pochven_customs_office_standing(snap, cid, poch_sys, &ctx.sde).is_ok());

        // Negative raw standing -0.2 + Diplomacy V (3357) -> effective 1.84 >= 1.5
        snap.characters.get_mut(&cid).unwrap().standings.insert(500_026, -0.2);
        snap.skills.entry(cid).or_default().insert(
            3_357,
            TrainedSkillRecord {
                type_id: 3_357,
                level: 5,
                skill_points: 256_000.0,
            },
        );
        assert!(validate_pochven_customs_office_standing(snap, cid, poch_sys, &ctx.sde).is_ok());
    });
}
