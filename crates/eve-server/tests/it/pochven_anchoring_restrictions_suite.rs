//! Integration tests for Pochven Anchoring Restrictions (`feat:structures.pochven-anchoring-restrictions`).
//!
//! Validates:
//! 1. Customs Office Gantry (type 3962) anchoring in Pochven requires at least 1.5 Triglavian standing via `posMgr.AnchorOrbital`.
//! 2. Customs Office Gantry anchoring in Pochven resolves nearest planet with 1 argument (`itemID`) and enforces proximity.
//! 3. Upwell structures (Astrahus, Raitaru, Athanor) remain strictly blocked from anchoring in Pochven regardless of standing.
//! 4. Finished Customs Office (type 2233) cannot be directly anchored, and already-anchored structures cannot be re-anchored.
//! 5. Pre-existing seeded Upwell structures in Pochven remain operational, tick cycles, and are not unanchored or destroyed.

#![forbid(unsafe_code)]

use crate::common;
use eve_db::{
    corp_roles, CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents,
    UpwellStructureRecord,
};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::station_structure::structure_lifecycle::{
    self as life, STATE_SHIELD_VULNERABLE,
};
use glam::DVec3;
use std::collections::HashMap;

const POCHVEN_SYS_ID: u32 = 30_000_157; // Otela (Pochven, region 10000070)
const OTELA_PLANET_ID: i64 = 40_009_948; // Otela I
const TRIGLAVIAN_FACTION_ID: i64 = 500_026;
const GANTRY_TYPE_ID: u32 = 3962; // Customs Office Gantry (group 1106)
const POCO_TYPE_ID: u32 = 2233; // Customs Office (group 1025)
const ASTRAHUS_TYPE: u32 = 35832;
const RAITARU_TYPE: u32 = 35825;
const ATHANOR_TYPE: u32 = 35835;

#[tokio::test]
async fn test_pochven_anchoring_restrictions_customs_office_and_upwell_blocks() {
    // parity: feat:structures.pochven-anchoring-restrictions
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let char_id = 90_000_001_i64;
    let ship_id = 100_000_501_i64;
    let corp_id = 98_000_101_i64;
    let gantry_item_id = 200_000_501_i64;
    let poco_upgrade_id = 200_000_502_i64;
    let astrahus_item_id = 200_000_503_i64;

    let mut sess = SessionState::new(885_001);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");
    ctx.advance_secs(11);

    let planet_pos = ctx
        .sde
        .celestials_in_system(POCHVEN_SYS_ID)
        .into_iter()
        .find(|c| c.celestial_id == OTELA_PLANET_ID)
        .map(|c| c.position)
        .unwrap_or(DVec3::new(27_660_817_438.0, -327_544_042.0, 66_850_617_032.0));
    let ship_pos_near_planet = planet_pos + DVec3::new(10_000_000.0, 0.0, 0.0);

    // Setup player corporation with Director and Station Manager roles
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.insert(
            corp_id,
            CorporationRecord {
                corporation_id: corp_id,
                name: "Pochven Pioneer Corp".into(),
                ticker: "PPIO".into(),
                ceo_id: char_id,
                ..Default::default()
            },
        );
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = POCHVEN_SYS_ID;
            ch.corporation_id = corp_id;
            ch.active_ship_id = ship_id;
            ch.balance = IskCents::from_isk(100_000_000);
            ch.station_id = None;
            ch.structure_id = None;
            // Initially low Triglavian standing (0.5 < 1.5)
            ch.standings.insert(TRIGLAVIAN_FACTION_ID, 0.5);
        }
        // Spawn pilot's ship (Mammoth industrial)
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 652, // Mammoth
                owner_id: char_id,
                location_id: POCHVEN_SYS_ID as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        // Put Customs Office Gantry (type 3962) in ship cargo
        snap.items.insert(
            gantry_item_id,
            InventoryItemRecord {
                item_id: gantry_item_id,
                type_id: GANTRY_TYPE_ID,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        // Put Customs Office upgrade item (type 2233) in ship cargo
        snap.items.insert(
            poco_upgrade_id,
            InventoryItemRecord {
                item_id: poco_upgrade_id,
                type_id: POCO_TYPE_ID,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        // Put Astrahus in ship cargo
        snap.items.insert(
            astrahus_item_id,
            InventoryItemRecord {
                item_id: astrahus_item_id,
                type_id: ASTRAHUS_TYPE,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });

    sess.solar_system_id = Some(POCHVEN_SYS_ID);
    sess.ship_id = Some(ship_id);
    sess.corporation_id = Some(corp_id);
    sess.corp_role = corp_roles::ROLE_DIRECTOR | corp_roles::ROLE_STATION_MANAGER;
    sess.station_id = None;
    sess.structure_id = None;

    let p_handle = ctx.systems.get_or_spawn(POCHVEN_SYS_ID);
    let _ = p_handle
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 652,
            position: ship_pos_near_planet,
            velocity: DVec3::ZERO,
            max_velocity: 150.0,
            inertia: 0.5,
            mass: 20_000_000.0,
            radius: 120.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;

    // 1. Upwell structures (Astrahus, Raitaru, Athanor) remain strictly blocked in Pochven
    // via structureDeployment.Anchor regardless of standing
    for &upwell_type in &[ASTRAHUS_TYPE, RAITARU_TYPE, ATHANOR_TYPE] {
        let upwell_err = ctx
            .dispatch_rpc(
                &mut sess,
                "structureDeployment",
                None,
                "Anchor",
                &[PyRep::Integer(upwell_type as i64)],
                &kw,
                None,
            )
            .await
            .expect_err("Upwell structure anchoring in Pochven must be blocked");
        assert_eq!(upwell_err.code, "AnchoringDisallowedInPochven");
    }

    // 2. Customs Office Gantry anchoring via posMgr.AnchorOrbital (1 argument: itemID)
    // With standing 0.5 < 1.5: rejected with InsufficientTriglavianStanding
    let low_standing_err = ctx
        .dispatch_rpc(
            &mut sess,
            "posMgr",
            None,
            "AnchorOrbital",
            &[PyRep::Integer(gantry_item_id)],
            &kw,
            None,
        )
        .await
        .expect_err("Gantry anchoring in Pochven must fail when Triglavian standing < 1.5");
    assert_eq!(
        low_standing_err.code,
        "InsufficientTriglavianStanding",
        "Pochven orbital anchoring requires >= 1.5 Triglavian standing"
    );

    // 3. Proximity check (R-POCH-04): If ship is too far from planet, anchoring fails
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters
            .get_mut(&char_id)
            .unwrap()
            .standings
            .insert(TRIGLAVIAN_FACTION_ID, 3.0);
    });
    let mut far_ball = p_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap().1;
    far_ball.position = planet_pos + DVec3::new(500_000_000.0, 0.0, 0.0); // 500,000 km away
    let _ = p_handle.spawn_ball(far_ball).await;

    let out_of_range_err = ctx
        .dispatch_rpc(
            &mut sess,
            "posMgr",
            None,
            "AnchorOrbital",
            &[PyRep::Integer(gantry_item_id)],
            &kw,
            None,
        )
        .await
        .expect_err("Gantry anchoring must fail when too far from planet");
    assert_eq!(out_of_range_err.code, "OrbitalOutOfRange");

    // Move ship back near planet
    let mut near_ball = p_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap().1;
    near_ball.position = ship_pos_near_planet;
    let _ = p_handle.spawn_ball(near_ball).await;

    // 4. Positive test: Customs Office Gantry anchoring succeeds with 1 argument (R-POCH-04)
    // Standing >= 1.5, nearest planet automatically resolved to OTELA_PLANET_ID
    let gantry_res = ctx
        .dispatch_rpc(
            &mut sess,
            "posMgr",
            None,
            "AnchorOrbital",
            &[PyRep::Integer(gantry_item_id)],
            &kw,
            None,
        )
        .await
        .expect("Anchor Customs Office Gantry in Pochven with standing >= 1.5");

    assert_eq!(
        gantry_res.dict_get("orbitalID"),
        Some(&PyRep::Integer(gantry_item_id))
    );
    assert_eq!(
        gantry_res.dict_get("planetID"),
        Some(&PyRep::Integer(OTELA_PLANET_ID))
    );
    assert_eq!(
        gantry_res.dict_get("typeID"),
        Some(&PyRep::Integer(GANTRY_TYPE_ID as i64))
    );
    assert_eq!(
        gantry_res.dict_get("state"),
        Some(&PyRep::String("Anchored".into()))
    );

    // Verify orbital structure record was inserted into DB snapshot
    let struct_rec = ctx
        .db
        .with_snapshot(|snap| snap.structures.get(&gantry_item_id).cloned())
        .expect("Gantry structure record created in snapshot");
    assert_eq!(struct_rec.type_id, GANTRY_TYPE_ID);
    assert_eq!(struct_rec.solar_system_id, POCHVEN_SYS_ID);
    assert_eq!(struct_rec.owner_corp_id, corp_id);
    assert_eq!(struct_rec.state, "ShieldVulnerable");

    // 5. Re-anchoring protection (R-POCH-03): Already-anchored structure cannot be re-anchored
    let reanchor_err = ctx
        .dispatch_rpc(
            &mut sess,
            "posMgr",
            None,
            "AnchorOrbital",
            &[PyRep::Integer(gantry_item_id)],
            &kw,
            None,
        )
        .await
        .expect_err("Already anchored orbital cannot be re-anchored");
    assert_eq!(reanchor_err.code, "AlreadyAnchored");

    // 6. Direct POCO anchoring forbidden (R-POCH-03): Finished Customs Office (type 2233)
    // cannot be directly anchored via AnchorOrbital without gantry deployment
    let direct_poco_err = ctx
        .dispatch_rpc(
            &mut sess,
            "posMgr",
            None,
            "AnchorOrbital",
            &[PyRep::Integer(poco_upgrade_id)],
            &kw,
            None,
        )
        .await
        .expect_err("Finished Customs Office cannot be directly anchored via AnchorOrbital");
    assert_eq!(direct_poco_err.code, "StarbaseRemoved");

    // 7. Online / upgrade Gantry to finished Customs Office via posMgr.OnlineOrbital
    let online_res = ctx
        .dispatch_rpc(
            &mut sess,
            "posMgr",
            None,
            "OnlineOrbital",
            &[
                PyRep::Integer(gantry_item_id),
                PyRep::Integer(poco_upgrade_id),
            ],
            &kw,
            None,
        )
        .await
        .expect("Online/upgrade Gantry to POCO");
    assert_eq!(
        online_res.dict_get("typeID"),
        Some(&PyRep::Integer(POCO_TYPE_ID as i64)),
        "Gantry upgraded to finished Customs Office"
    );
    assert_eq!(
        online_res.dict_get("state"),
        Some(&PyRep::String("ShieldVulnerable".into())),
        "Customs Office is online with shield vulnerable"
    );
}

#[tokio::test]
async fn test_pochven_existing_structures_survive_and_remain_operational() {
    // parity: feat:structures.pochven-anchoring-restrictions
    let ctx = ServerContext::new();
    let corp_id = 98_000_102_i64;
    let existing_struct_id = 1_000_000_500_001_i64;
    let struct_pos = [100_000_000.0, 200_000_000.0, 300_000_000.0];

    // Seed pre-existing Upwell Citadel in Pochven (Otela) from before the invasion
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.insert(
            corp_id,
            CorporationRecord {
                corporation_id: corp_id,
                name: "Legacy Pochven Holdout".into(),
                ticker: "HOLD".into(),
                ..Default::default()
            },
        );
        snap.structures.insert(
            existing_struct_id,
            UpwellStructureRecord {
                structure_id: existing_struct_id,
                type_id: ASTRAHUS_TYPE,
                owner_corp_id: corp_id,
                solar_system_id: POCHVEN_SYS_ID,
                name: "Legacy Astrahus Otela".into(),
                position: struct_pos,
                state: STATE_SHIELD_VULNERABLE.into(),
                quantum_core_installed: true,
                fuel_blocks: 5_000,
                shield_hp: 4_800_000.0,
                armor_hp: 4_800_000.0,
                hull_hp: 4_800_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
    });

    // 1. Verify structure is present in snapshot
    assert!(
        ctx.db.with_snapshot(|snap| snap.structures.contains_key(&existing_struct_id)),
        "Pre-existing Astrahus exists in Pochven snapshot"
    );

    // 2. Advance time by multiple hours and run structure lifecycle sync
    ctx.advance_secs(7200);
    let gone = life::sync_all_structures(&ctx);

    // Structure must NOT be removed, unanchored, or destroyed by Pochven region check
    assert!(
        !gone.iter().any(|(id, _)| *id == existing_struct_id),
        "Pochven legacy structure must not be removed by lifecycle sync"
    );

    let struct_after = ctx
        .db
        .with_snapshot(|snap| snap.structures.get(&existing_struct_id).cloned())
        .expect("Legacy structure still present in DB after 2 hours");
    assert_eq!(struct_after.state, STATE_SHIELD_VULNERABLE);
    assert_eq!(struct_after.solar_system_id, POCHVEN_SYS_ID);
    assert!(struct_after.quantum_core_installed);

    // 3. Ensure structure ball is materialized and active in space
    eve_server::services::station_structure::structure_lifecycle_rpc::ensure_structure_ball_in_space(&ctx, POCHVEN_SYS_ID, existing_struct_id).await;
    let p_handle = ctx.systems.get_or_spawn(POCHVEN_SYS_ID);
    let ball_snap = p_handle
        .get_ball_snapshot(existing_struct_id)
        .await
        .expect("get_ball_snapshot")
        .expect("Ball snapshot present in Pochven ballpark");
    assert_eq!(ball_snap.1.type_id, ASTRAHUS_TYPE);
}
