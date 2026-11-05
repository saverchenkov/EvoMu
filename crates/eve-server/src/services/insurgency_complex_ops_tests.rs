#![forbid(unsafe_code)]

use super::*;
use crate::services::account_services::account_services_tests::test_ctx;

#[tokio::test]
async fn insurgency_complex_ops_helpers_and_lifecycle_mutations() {
    assert_eq!(OutpostRaidSize::Small.slot_index(), 1);
    assert_eq!(OutpostRaidSize::Medium.slot_index(), 2);
    assert_eq!(OutpostRaidSize::Moderate.slot_index(), 3);
    assert_eq!(OutpostRaidSize::LargeAdv.slot_index(), 4);

    assert_eq!(OutpostRaidSize::Small.beacon_type_id(), TYPE_OUTPOST_BEACON_SMALL);
    assert_eq!(OutpostRaidSize::Medium.beacon_type_id(), TYPE_OUTPOST_BEACON_MEDIUM);
    assert_eq!(OutpostRaidSize::Moderate.beacon_type_id(), TYPE_OUTPOST_BEACON_MODERATE);
    assert_eq!(OutpostRaidSize::LargeAdv.beacon_type_id(), TYPE_OUTPOST_BEACON_LARGE_ADV);

    assert_eq!(OutpostRaidSize::Small.base_capture_duration_secs(), 600);
    assert_eq!(OutpostRaidSize::Medium.base_capture_duration_secs(), 900);
    assert_eq!(OutpostRaidSize::Moderate.base_capture_duration_secs(), 900);
    assert_eq!(OutpostRaidSize::LargeAdv.base_capture_duration_secs(), 900);

    assert!((OutpostRaidSize::Small.contribution_percent() - 0.25).abs() < 1e-6);
    assert!((OutpostRaidSize::Medium.contribution_percent() - 0.5).abs() < 1e-6);
    assert!((OutpostRaidSize::Moderate.contribution_percent() - 1.5).abs() < 1e-6);
    assert!((OutpostRaidSize::LargeAdv.contribution_percent() - 1.0).abs() < 1e-6);

    assert_eq!(OutpostRaidSize::Small.contribution_subpoints(), 250);
    assert_eq!(OutpostRaidSize::Medium.contribution_subpoints(), 500);
    assert_eq!(OutpostRaidSize::Moderate.contribution_subpoints(), 1_500);
    assert_eq!(OutpostRaidSize::LargeAdv.contribution_subpoints(), 1_000);

    assert_eq!(OutpostRaidSize::Small.base_lp_reward(), 29_000);
    assert_eq!(OutpostRaidSize::Medium.base_lp_reward(), 41_500);
    assert_eq!(OutpostRaidSize::Moderate.base_lp_reward(), 52_500);
    assert_eq!(OutpostRaidSize::LargeAdv.base_lp_reward(), 52_500);

    assert_eq!(OutpostRaidSize::Small.respawn_delay_secs(), STANDARD_OUTPOST_RESPAWN_SECS);
    assert_eq!(OutpostRaidSize::LargeAdv.respawn_delay_secs(), LARGE_ADV_OUTPOST_RESPAWN_SECS);

    assert!(OutpostRaidSize::Small.allows_ship_group(25));
    assert!(!OutpostRaidSize::Small.allows_ship_group(26));
    assert!(OutpostRaidSize::Medium.allows_ship_group(26));
    assert!(!OutpostRaidSize::Medium.allows_ship_group(419));
    assert!(OutpostRaidSize::Moderate.allows_ship_group(419));
    assert!(!OutpostRaidSize::Moderate.allows_ship_group(27));
    assert!(OutpostRaidSize::LargeAdv.allows_ship_group(27));

    assert_eq!(OutpostRaidSize::Small.mordus_defender_type_id(), 79_545);
    assert_eq!(OutpostRaidSize::Small.pirate_defender_type_id(FACTION_ANGEL_CARTEL), 79_515);
    assert_eq!(OutpostRaidSize::Small.pirate_defender_type_id(FACTION_GURISTAS), 79_529);

    assert!(is_warzone_opposing_empire(FACTION_ANGEL_CARTEL, 500_002));
    assert!(is_warzone_opposing_empire(FACTION_ANGEL_CARTEL, 500_003));
    assert!(!is_warzone_opposing_empire(FACTION_ANGEL_CARTEL, 500_001));
    assert!(is_warzone_opposing_empire(FACTION_GURISTAS, 500_001));
    assert!(is_warzone_opposing_empire(FACTION_GURISTAS, 500_004));

    assert!(is_battlecruiser_group(419));
    assert!(is_battlecruiser_group(540));
    assert!(is_battlecruiser_group(1201));
    assert!(!is_battlecruiser_group(25));

    assert_eq!(share_site_lp_up_to_5(10_000, 0), 0);
    assert_eq!(share_site_lp_up_to_5(0, 3), 0);
    assert_eq!(share_site_lp_up_to_5(10_000, 1), 10_000);
    assert_eq!(share_site_lp_up_to_5(10_000, 5), 10_000);
    assert_eq!(share_site_lp_up_to_5(10_000, 10), 5_000);

    assert!(is_named_pirate_commander_roamer(79_329));
    assert!(is_named_pirate_commander_roamer(79_358));
    assert!(!is_named_pirate_commander_roamer(79_389));
    assert!(is_named_insurgency_roamer(79_389));

    assert_eq!(
        named_insurgency_roamer_loot_drop(79_329),
        Some((TYPE_ANGEL_INSURGENCY_DATA_SEQ, 1))
    );
    assert_eq!(
        named_insurgency_roamer_loot_drop(79_358),
        Some((TYPE_GURISTAS_INSURGENCY_DATA_SEQ, 1))
    );
    assert_eq!(
        named_insurgency_roamer_loot_drop(79_389),
        Some((TYPE_UPWELL_CONSORTIUM_DATA_CACHE, 1))
    );
    assert_eq!(
        named_insurgency_roamer_loot_drop(TYPE_ORE_RORQUAL),
        Some((TYPE_UPWELL_CONSORTIUM_DATA_CACHE, 3))
    );

    let mut loot = Vec::new();
    append_insurgency_npc_wreck_loot(TYPE_ORE_RORQUAL, &mut loot);
    assert_eq!(loot.len(), 2);

    let ctx = test_ctx();
    // Non-insurgency trader always passes access check
    assert!(check_insurgency_laundering_access(&ctx, 90_000_001, 30000142, 1234).is_ok());
    // Unaligned character fails pirate laundering access
    assert!(check_insurgency_laundering_access(&ctx, 90_000_001, 30000142, TYPE_GURISTAS_LAUNDERING_FACILITY).is_err());
    // Wrong system fails War HQ CI hub access
    assert!(check_insurgency_laundering_access(&ctx, 90_000_001, 30000142, TYPE_STATE_CI_OUTPOST_HUB).is_err());

    assert!(!is_warp_into_outpost_raid_pocket_blocked(
        &ctx,
        30000142,
        [0.0, 0.0, 0.0],
        [1000.0, 0.0, 0.0],
        [1000.0, 0.0, 0.0]
    ));

    assert!(active_insurgency_pocket_dbuffs(&ctx, 90_000_001, 1, 587).is_empty());

    let err = activate_outpost_raid_gate_in_space(&ctx, 90_000_001, 1, 30000142, 999_999)
        .await
        .unwrap_err();
    assert_eq!(err.code, "GateNotFound");
}
