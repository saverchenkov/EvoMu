use super::*;
use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::make_fighter_ctx_and_session;
use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, MovementMode};

#[tokio::test]
async fn test_abyssal_void_mgr_dispatch_and_filament_activation() {
    let (mut ctx, mut session, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    std::sync::Arc::make_mut(&mut ctx.sde)
        .solar_systems
        .get_mut(&sys_id)
        .unwrap()
        .security_status = 0.7;
    let kw = HashMap::new();

    // Ensure ship is a Caracal (621) in space far from celestials, and seed a Calm Electrical Filament (47762)
    let fil_id = 910_001;
    let far_pos = [500_000_000.0, 500_000_000.0, 500_000_000.0];
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(sh) = snap.items.get_mut(&ship_id) {
            sh.type_id = 621;
        }
        snap.item_positions.insert(ship_id, far_pos);
        snap.items.insert(
            fil_id,
            InventoryItemRecord {
                item_id: fil_id,
                type_id: 47_762,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 2,
                singleton: false,
                ..Default::default()
            },
        );
    });

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_handle
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 621,
            position: DVec3::from_array(far_pos),
            velocity: DVec3::ZERO,
            max_velocity: 200.0,
            inertia: 1.0,
            mass: 10_000_000.0,
            radius: 100.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;

    // Missing argument checks
    assert!(handle(
        &ctx,
        &mut session,
        "abyssalMgr",
        "AbyssalEntranceDeployment",
        &[],
        &kw
    )
    .await
    .unwrap()
    .is_err());
    assert!(handle(
        &ctx,
        &mut session,
        "abyssalMgr",
        "AbyssalEntranceGateActivation",
        &[],
        &kw
    )
    .await
    .unwrap()
    .is_err());

    // Direct inventory filament activation via AbyssalEntranceGateActivation
    let enter = handle(
        &ctx,
        &mut session,
        "abyssalMgr",
        "AbyssalEntranceGateActivation",
        &[PyRep::Integer(fil_id)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        enter.dict_get("solarSystemID").and_then(|v| v.as_i64()),
        Some(ABYSSAL_SPACE_SYSTEM_ID as i64)
    );
    assert_eq!(session.solar_system_id, Some(ABYSSAL_SPACE_SYSTEM_ID));

    // ClientIsReady inside Abyssal space
    let ready = handle(&ctx, &mut session, "abyssalMgr", "ClientIsReady", &[], &kw)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(ready.dict_get("ready").and_then(|v| v.as_bool()), Some(true));

    // ClientIsReady for voidSpaceMgr
    session.solar_system_id = Some(VOID_SPACE_SYSTEM_ID);
    let vready = handle(&ctx, &mut session, "voidSpaceMgr", "ClientIsReady", &[], &kw)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(vready.dict_get("ready").and_then(|v| v.as_bool()), Some(true));
}
