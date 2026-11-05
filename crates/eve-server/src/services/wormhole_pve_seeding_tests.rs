use super::*;
use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::make_fighter_ctx_and_session;

#[tokio::test]
async fn test_wormhole_pve_seeding_sentries_loot_and_special_systems() {
    let (ctx, _session, char_id, _ship_id, sys_id) = make_fighter_ctx_and_session();

    // Test sentry type classification and capital group helpers
    assert_eq!(sleeper_sentry_type_for_class(1), 30_460);
    assert_eq!(sleeper_sentry_type_for_class(3), 30_461);
    assert_eq!(sleeper_sentry_type_for_class(6), 30_462);
    assert!(is_sleeper_sentry_type(30_460));
    assert!(!is_sleeper_sentry_type(34));

    assert_eq!(capital_escalation_class_for_group(485), Some(true));
    assert_eq!(capital_escalation_class_for_group(547), Some(false));
    assert_eq!(capital_escalation_class_for_group(25), None);
    assert!(is_any_capital_group(30));
    assert!(!is_any_capital_group(25));

    // Test spawn_sleeper_sentry_tower and can_sentry_fire_and_record
    let sbid = spawn_sleeper_sentry_tower(&ctx, sys_id, 4, DVec3::ZERO).await;
    assert!(sbid > 0);
    assert!(can_sentry_fire_and_record(&ctx, sbid, 10.0, 100.0));
    assert!(!can_sentry_fire_and_record(&ctx, sbid, 10.0, 105.0));
    assert!(can_sentry_fire_and_record(&ctx, sbid, 10.0, 111.0));

    // Test drop_site_loot_into_space
    let loot_id = drop_site_loot_into_space(&ctx, sys_id, char_id, [100.0, 200.0, 300.0], 34, 50).await;
    assert!(loot_id > 0);

    // Test seed_special_wormhole_pve_sites for Drifter system (31_000_001), C13 system (31_002_585),
    // and standard shattered system (31_002_510), plus non-special K-space system (false)
    assert!(!seed_special_wormhole_pve_sites(&ctx, sys_id).await);
    assert!(seed_special_wormhole_pve_sites(&ctx, 31_000_001).await);
    assert!(seed_special_wormhole_pve_sites(&ctx, 31_002_585).await);
    assert!(seed_special_wormhole_pve_sites(&ctx, 31_002_510).await);

    // Verify Shattered Debris Field and Shattered Ice Field were seeded in 31_002_510
    let has_debris_and_ice = ctx.db.with_snapshot(|snap| {
        let anoms = snap.cosmic_anomalies.get(&31_002_510).cloned().unwrap_or_default();
        anoms.iter().any(|a| a.name == "Shattered Debris Field")
            && anoms.iter().any(|a| a.name == "Shattered Ice Field")
    });
    assert!(has_debris_and_ice);

    // Test build_specialized_npc_components across archetypes
    for arch in [
        "SleeperSentry",
        "TransmissionRelay",
        "Sleeper",
        "PochvenHypnosianSleeper",
        "Drifter",
        "Triglavian",
        "EDENCOM",
        "CloneSoldier",
        "Pirate",
    ] {
        let (ball, cs, ent) = build_specialized_npc_components(
            &ctx,
            sys_id,
            880_001,
            34,
            arch,
            500_010,
            DVec3::ZERO,
        );
        assert_eq!(ball.id, 880_001);
        assert!(cs.max_structure_hp > 0.0);
        assert_eq!(ent.archetype, arch);
    }
}
