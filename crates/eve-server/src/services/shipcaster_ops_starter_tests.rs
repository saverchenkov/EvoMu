use super::*;
use crate::services::account_services::account_services_tests::{auth_session, test_ctx};
use crate::services::cluster_macho_misc_services::with_misc_state;
use eve_destiny::{Ball, MovementMode};
use glam::DVec3;

#[tokio::test]
async fn test_shipcaster_ops_starter_eligibility_vectors_proximity_and_jump() {
    let ctx = test_ctx();
    let mut sess = auth_session(1, 90_000_001, 98_000_001, 30_000_142);
    let cid = 90_000_001;
    let now = ctx.now_unix();

    // Give rookie pilot an active frigate in Jita (30000142)
    let ship_id = ctx.db.with_mut_snapshot(|snap| {
        snap.next_entity_id += 1;
        let sid = snap.next_entity_id;
        snap.items.insert(
            sid,
            eve_db::InventoryItemRecord {
                item_id: sid,
                type_id: 587, // Rifter (Frigate)
                owner_id: cid,
                location_id: 30_000_142,
                flag: eve_db::InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.create_date_unix = now.saturating_sub(86_400); // 1 day old rookie
            ch.active_ship_id = sid;
            ch.solar_system_id = 30_000_142;
        }
        sid
    });
    sess.ship_id = Some(ship_id);
    sess.solar_system_id = Some(30_000_142);

    // 1. Rookie eligibility checks
    assert!(check_starter_shipcaster_eligibility(&ctx, &sess, cid).is_ok());

    // Already used check
    with_misc_state(&ctx, |st| {
        st.used_starter_shipcaster_chars.insert(cid);
    });
    assert!(check_starter_shipcaster_eligibility(&ctx, &sess, cid).is_err());
    with_misc_state(&ctx, |st| {
        st.used_starter_shipcaster_chars.remove(&cid);
    });

    // Too old check
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.create_date_unix = now.saturating_sub(STARTER_SHIPCASTER_MAX_ROOKIE_AGE_SECS + 100);
        }
    });
    assert!(check_starter_shipcaster_eligibility(&ctx, &sess, cid).is_err());
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.create_date_unix = now.saturating_sub(86_400);
        }
    });

    // Capsule check
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(it) = snap.items.get_mut(&ship_id) {
            it.type_id = 670;
        }
    });
    assert!(check_starter_shipcaster_eligibility(&ctx, &sess, cid).is_err());
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(it) = snap.items.get_mut(&ship_id) {
            it.type_id = 587;
        }
    });

    // 2. Launch vector and arrival position math
    let v_same = calculate_shipcaster_launch_vector(
        &ctx.sde,
        30_000_142,
        DVec3::ZERO,
        30_000_142,
        DVec3::new(0.0, 1000.0, 0.0),
    );
    assert!((v_same.y - 1.0).abs() < 1e-6);

    let v_inter = calculate_shipcaster_launch_vector(
        &ctx.sde,
        30_000_142,
        DVec3::ZERO,
        30_002_187,
        DVec3::ZERO,
    );
    assert!((v_inter.length() - 1.0).abs() < 1e-6);

    let arr = calculate_shipcaster_arrival_position(
        DVec3::new(100.0, 0.0, 0.0),
        DVec3::X,
        2_500.0,
    );
    assert_eq!(arr, DVec3::new(2_600.0, 0.0, 0.0));

    // 3. Proximity, alignment FX, and execute_shipcaster_jump
    let jita_sys = ctx.systems.get_or_spawn(30_000_142);
    assert!(check_proximity(&jita_sys, ship_id, 0, 5000.0, "caster").await.is_err());
    assert!(check_proximity(&jita_sys, ship_id, 777_001, 5000.0, "caster").await.is_err());

    let _ = jita_sys
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 587,
            position: DVec3::ZERO,
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    let _ = jita_sys
        .spawn_ball(Ball {
            id: 777_001,
            type_id: 75900,
            position: DVec3::new(1000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 1.0e9,
            radius: 1000.0,
            speed_fraction: 0.0,
            mode: MovementMode::Rigid,
        })
        .await;
    assert!(check_proximity(&jita_sys, ship_id, 777_001, 5000.0, "caster").await.is_ok());
    assert!(check_proximity(&jita_sys, ship_id, 777_001, 500.0, "caster").await.is_err());

    apply_shipcaster_beacon_alignment_and_spool(&jita_sys, ship_id, 777_001, DVec3::X)
        .await
        .unwrap();
    execute_shipcaster_jump(
        &ctx,
        &mut sess,
        cid,
        ship_id,
        30_000_142,
        30_000_144,
        DVec3::new(5000.0, 0.0, 0.0),
        DVec3::X,
    )
    .await
    .unwrap();
    assert_eq!(sess.solar_system_id, Some(30_000_144));

    // 4. on_landing_pad_ball_destroyed
    on_landing_pad_ball_destroyed(&ctx, -999_999).await;
}
