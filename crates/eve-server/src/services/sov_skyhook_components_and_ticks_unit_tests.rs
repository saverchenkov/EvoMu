//! Semantic unit tests split from `sov_gameplay_skyhook_extra_unit_tests`.

#![forbid(unsafe_code)]

use super::*;
use crate::services::sov_services::{TYPE_MAGMATIC_GAS, TYPE_ORBITAL_SKYHOOK};
use eve_db::{InventoryFlag, InventoryItemRecord, UpwellStructureRecord};
use eve_destiny::{Ball, CombatState, MovementMode};
use eve_machonet::SessionState;
use glam::DVec3;

#[tokio::test]
async fn skyhook_component_calls_error_branches_and_tick_link_break_restores_velocity() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let sys_id = 30_000_142_u32;
    let skyhook_id = 1_072_000_050_i64;
    let ship_id = 1_072_000_051_i64;
    let now = ctx.now_unix();
    let corp_id = ctx.db.with_snapshot(|s| s.characters[&char_id].corporation_id);

    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            skyhook_id,
            UpwellStructureRecord {
                structure_id: skyhook_id,
                type_id: TYPE_ORBITAL_SKYHOOK,
                owner_corp_id: corp_id,
                solar_system_id: sys_id,
                name: "Skyhook".into(),
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                shield_hp: 500.0, // <= 10%
                ..Default::default()
            },
        );
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 620, // Osprey Cruiser
                owner_id: char_id,
                location_id: i64::from(sys_id),
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });

    let mut sess = SessionState::new(9502);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(corp_id);
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(ship_id);

    // 1. ShipNotInSpace before ball spawned
    assert_eq!(
        handle_skyhook_component_call(
            &ctx,
            &sess,
            sys_id,
            char_id,
            ship_id,
            skyhook_id,
            "linkWithShip",
            "InitiateLink",
            &[]
        )
        .await
        .unwrap_err()
        .code,
        "ShipNotInSpace"
    );

    // 2. Cloaked -> CannotLinkWhileCloaked
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_handle
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 587, // Rifter (Frigate group 25 -> ineligible for silo link)
            position: DVec3::new(30_000.0, 0.0, 0.0), // > 15km initially
            velocity: DVec3::ZERO,
            max_velocity: 2_500.0,
            inertia: 1.0,
            mass: 1e6,
            radius: 50.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    let _ = sys_handle
        .set_combat_state(
            ship_id,
            CombatState {
                is_cloaked: true,
                ..Default::default()
            },
        )
        .await;
    assert_eq!(
        handle_skyhook_component_call(
            &ctx,
            &sess,
            sys_id,
            char_id,
            ship_id,
            skyhook_id,
            "linkWithShip",
            "InitiateLink",
            &[]
        )
        .await
        .unwrap_err()
        .code,
        "CannotLinkWhileCloaked"
    );
    let _ = sys_handle
        .set_combat_state(ship_id, CombatState::default())
        .await;

    // 3. SkyhookNotFound & TargetNotInSameSystem & OutOfSkyhookLinkRange
    assert_eq!(
        handle_skyhook_component_call(
            &ctx,
            &sess,
            sys_id,
            char_id,
            ship_id,
            999_888,
            "linkWithShip",
            "InitiateLink",
            &[]
        )
        .await
        .unwrap_err()
        .code,
        "SkyhookNotFound"
    );
    assert_eq!(
        handle_skyhook_component_call(
            &ctx,
            &sess,
            sys_id,
            char_id,
            ship_id,
            skyhook_id,
            "linkWithShip",
            "InitiateLink",
            &[]
        )
        .await
        .unwrap_err()
        .code,
        "OutOfSkyhookLinkRange"
    );

    // 4. Move within 15km, Frigate hull 587 -> IneligibleShipForSkyhookLink
    let _ = sys_handle
        .set_ball_position(ship_id, DVec3::new(2_000.0, 0.0, 0.0))
        .await;
    assert_eq!(
        handle_skyhook_component_call(
            &ctx,
            &sess,
            sys_id,
            char_id,
            ship_id,
            skyhook_id,
            "linkWithShip",
            "InitiateLink",
            &[]
        )
        .await
        .unwrap_err()
        .code,
        "IneligibleShipForSkyhookLink"
    );

    // 5. Respawn as Cruiser 620: test SkyhookNotVulnerableToTheft, InsecureReagentBayEmpty, SkyhookAlreadyLinked
    let _ = sys_handle
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 620,
            position: DVec3::new(2_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 2_500.0,
            inertia: 1.0,
            mass: 1e6,
            radius: 50.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    assert_eq!(
        handle_skyhook_component_call(
            &ctx,
            &sess,
            sys_id,
            char_id,
            ship_id,
            skyhook_id,
            "linkWithShip",
            "InitiateLinkQuick",
            &[]
        )
        .await
        .unwrap_err()
        .code,
        "SkyhookNotVulnerableToTheft"
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.sov_runtime
            .skyhook_theft_windows
            .insert(skyhook_id, (now.saturating_sub(60), now + 3_600));
        snap.sov_runtime
            .skyhook_reagent_bays
            .insert(skyhook_id, (TYPE_MAGMATIC_GAS, 50, 0, now)); // 0 insecure
    });
    assert_eq!(
        handle_skyhook_component_call(
            &ctx,
            &sess,
            sys_id,
            char_id,
            ship_id,
            skyhook_id,
            "linkWithShip",
            "InitiateLinkQuick",
            &[]
        )
        .await
        .unwrap_err()
        .code,
        "InsecureReagentBayEmpty"
    );

    // Add insecure reagents, initiate link (caps velocity to 1000 m/s), verify duplicate link rejected
    ctx.db.with_mut_snapshot(|snap| {
        snap.sov_runtime
            .skyhook_reagent_bays
            .insert(skyhook_id, (TYPE_MAGMATIC_GAS, 50, 40, now));
    });
    handle_skyhook_component_call(
        &ctx,
        &sess,
        sys_id,
        char_id,
        ship_id,
        skyhook_id,
        "linkWithShip",
        "InitiateLinkQuick",
        &[],
    )
    .await
    .unwrap();
    assert_eq!(
        handle_skyhook_component_call(
            &ctx,
            &sess,
            sys_id,
            char_id,
            ship_id,
            skyhook_id,
            "linkWithShip",
            "InitiateLink",
            &[]
        )
        .await
        .unwrap_err()
        .code,
        "SkyhookAlreadyLinked"
    );
    assert_eq!(
        handle_skyhook_component_call(
            &ctx,
            &sess,
            sys_id,
            char_id,
            ship_id,
            skyhook_id,
            "unknownComp",
            "Method",
            &[]
        )
        .await
        .unwrap_err()
        .code,
        "UnsupportedComponentCall"
    );

    // Break active link by moving ship > 15km and calling tick_skyhooks -> link removed and max_velocity restored to 2500.0
    let _ = sys_handle
        .set_ball_position(ship_id, DVec3::new(25_000.0, 0.0, 0.0))
        .await;
    tick_skyhooks(&ctx, now + 10).await;
    assert!(!is_ship_skyhook_linked(&ctx, ship_id, now + 10));
    let (_, restored_ball) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    assert!((restored_ball.max_velocity - 2_500.0).abs() < 1e-6);
}
