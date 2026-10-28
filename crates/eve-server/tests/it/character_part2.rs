//! Container contents ownership and range enforcement integration tests (`character` part 2).

#![forbid(unsafe_code)]

use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::HashMap;

#[tokio::test]
async fn test_r50_p1_1_container_contents_ownership_and_range_enforcement() {
    // parity: rpc:authentication.Login, rpc:charUnboundMgr.SelectCharacterID, rpc:invbroker.GetContainerContents
    use eve_destiny::{Ball, MovementMode};
    use glam::DVec3;

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut sess = SessionState::new(981);
    let kw = HashMap::new();

    ctx.dispatch_rpc(
        &mut sess,
        "authentication",
        None,
        "Login",
        &[
            PyRep::String("demo".into()),
            PyRep::String("demo-secret".into()),
        ],
        &kw,
        None,
    )
    .await
    .expect("Login");
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_001)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");

    // Put character 90_000_001 in space in Jita (30000142) aboard ship 100_000_001 at (0, 0, 0)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&90_000_001) {
            c.station_id = None;
            c.solar_system_id = 30_000_142;
        }
        // 1) Another player's ship in Jita (category 6, type 587) -> must NEVER be inspectable
        snap.items.insert(
            990_000_001,
            eve_db::InventoryItemRecord {
                item_id: 990_000_001,
                type_id: 587,
                owner_id: 90_000_002,
                location_id: 30_000_142,
                flag: eve_db::InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                dynamic_attributes: HashMap::new(),
            },
        );
        // 2) Cargo container (type 23, group 12) in another solar system (Perimeter = 30000144)
        snap.items.insert(
            990_000_002,
            eve_db::InventoryItemRecord {
                item_id: 990_000_002,
                type_id: 23,
                owner_id: 90_000_002,
                location_id: 30_000_144,
                flag: eve_db::InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                dynamic_attributes: HashMap::new(),
            },
        );
        // 3) Cargo container (type 23, group 12) in Jita (30000142)
        snap.items.insert(
            990_000_003,
            eve_db::InventoryItemRecord {
                item_id: 990_000_003,
                type_id: 23,
                owner_id: 90_000_002,
                location_id: 30_000_142,
                flag: eve_db::InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                dynamic_attributes: HashMap::new(),
            },
        );
        // Loot item inside container 990_000_003
        snap.items.insert(
            990_000_004,
            eve_db::InventoryItemRecord {
                item_id: 990_000_004,
                type_id: 34,
                owner_id: 90_000_002,
                location_id: 990_000_003,
                flag: eve_db::InventoryFlag::Cargo,
                quantity: 500,
                singleton: false,
                version: 1,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                dynamic_attributes: HashMap::new(),
            },
        );
    });

    let jita = ctx.systems.get_or_spawn(30_000_142);
    jita.spawn_ball(Ball {
        id: 100_000_001,
        type_id: 606,
        position: DVec3::ZERO,
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 1.0,
        mass: 1_000_000.0,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    })
    .await
    .expect("spawn player ship");

    jita.spawn_ball(Ball {
        id: 990_000_001,
        type_id: 587,
        position: DVec3::new(500.0, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 1.0,
        mass: 1_000_000.0,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    })
    .await
    .expect("spawn other player ship");

    // 1. Another player's in-space ship is rejected even at 500m
    let ship_err = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            None,
            "GetContainerContents",
            &[PyRep::Integer(990_000_001)],
            &kw,
            None,
        )
        .await
        .expect_err("another player's ship must not be inspectable");
    assert_eq!(ship_err.code, "CantDoThatWithSomeoneElsesStuff");

    // 2. Container in another solar system (30000144) is rejected
    let sys_err = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            None,
            "GetContainerContents",
            &[PyRep::Integer(990_000_002)],
            &kw,
            None,
        )
        .await
        .expect_err("container in another system must be rejected");
    assert_eq!(sys_err.code, "CantDoThatWithSomeoneElsesStuff");

    // 3. Container in same solar system but 10,000m away (> 2,500m) is rejected
    jita.spawn_ball(Ball {
        id: 990_000_003,
        type_id: 23,
        position: DVec3::new(10_000.0, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 0.0,
        inertia: 1.0,
        mass: 1_000.0,
        radius: 10.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    })
    .await
    .expect("spawn far container");

    let range_err = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            None,
            "GetContainerContents",
            &[PyRep::Integer(990_000_003)],
            &kw,
            None,
        )
        .await
        .expect_err("container beyond 2,500m must be rejected");
    assert_eq!(range_err.code, "CantDoThatWithSomeoneElsesStuff");

    // 4. Move container within 1,500m (<= 2,500m) in same system -> accepted!
    jita.remove_ball(990_000_003).await.expect("remove far ball");
    jita.spawn_ball(Ball {
        id: 990_000_003,
        type_id: 23,
        position: DVec3::new(1_500.0, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 0.0,
        inertia: 1.0,
        mass: 1_000.0,
        radius: 10.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    })
    .await
    .expect("spawn near container");

    let near_ok = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            None,
            "GetContainerContents",
            &[PyRep::Integer(990_000_003)],
            &kw,
            None,
        )
        .await
        .expect("in-range cargo container in same system should succeed");
    assert!(near_ok.is_crowset());
    let rows = near_ok.crowset_rows().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].dbrow_get("itemID"), Some(&PyRep::Integer(990_000_004)));
}
