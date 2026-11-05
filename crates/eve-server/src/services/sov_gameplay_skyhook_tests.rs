//! Unit tests for `sov_gameplay_skyhook` (reagent harvesting, 24h maturity, anchoring, silo linking, extraction, ticks).

#![forbid(unsafe_code)]

use super::*;
use crate::router::ServerContext;
use crate::services::sov_services::{
    TYPE_MAGMATIC_GAS, TYPE_ORBITAL_SKYHOOK, TYPE_SOVEREIGNTY_HUB, TYPE_SUPERIONIC_ICE,
};
use eve_db::{InventoryFlag, InventoryItemRecord, UpwellStructureRecord};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use glam::DVec3;
use std::sync::{Arc, OnceLock};

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn test_ctx() -> ServerContext {
    if let Some(sde) = SHARED_SDE.get() {
        ServerContext::with_sde_and_repository(
            Arc::clone(sde),
            Arc::new(eve_db::WorldRepository::with_demo_seed()),
        )
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
    } else {
        let ctx = ServerContext::new();
        let _ = SHARED_SDE.set(Arc::clone(&ctx.sde));
        ctx
    }
}

fn find_null_system_with_reagent_planet(ctx: &ServerContext) -> (u32, u32, u32, [f64; 3]) {
    for sys in ctx.sde.solar_systems.values() {
        if crate::services::sov_gameplay::is_claimable_nullsec_system(&ctx.sde, sys.solar_system_id)
        {
            for p in ctx.sde.celestials_in_system(sys.solar_system_id) {
                if p.group_id == 7 {
                    if let Some(rtid) = planet_reagent_type(p.type_id) {
                        return (
                            sys.solar_system_id,
                            p.celestial_id as u32,
                            rtid,
                            [p.position.x, p.position.y, p.position.z],
                        );
                    }
                }
            }
        }
    }
    (30_000_142, 40_009_077, TYPE_MAGMATIC_GAS, [1.0e9, 0.0, 0.0])
}

#[test]
fn planet_reagent_mapping_and_linear_reagent_maturity_math() {
    assert_eq!(planet_reagent_type(12), Some(TYPE_SUPERIONIC_ICE));
    assert_eq!(planet_reagent_type(2014), Some(TYPE_SUPERIONIC_ICE));
    assert_eq!(planet_reagent_type(2015), Some(TYPE_MAGMATIC_GAS));
    assert_eq!(planet_reagent_type(11), None);

    let kv = PyRep::keyval([("planetID", PyRep::Integer(40_001_234))]);
    assert_eq!(extract_extra_var(Some(&kv), "planetid"), Some(40_001_234));
    let dict = PyRep::Dict(vec![(
        PyRep::String("planetID".into()),
        PyRep::Integer(40_005_678),
    )]);
    assert_eq!(extract_extra_var(Some(&dict), "planetID"), Some(40_005_678));
    assert_eq!(extract_extra_var(None, "planetID"), None);

    let ctx = test_ctx();
    let skyhook_id = 1_070_000_001_i64;
    let t0 = 1_750_000_000_u64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.sov_runtime
            .skyhook_reagent_bays
            .insert(skyhook_id, (TYPE_MAGMATIC_GAS, 100, 240, t0));
        snap.sov_runtime.skyhook_last_harvest_unix.insert(skyhook_id, t0);

        // Advance 12 hours (half of 24h cycle): 120 insecure units mature -> 220 secure, 120 insecure
        // PLUS 12 hours * 50/hr = 600 harvested (300 to secure, 300 to insecure)
        let (_, sec1, insec1) =
            advance_skyhook_reagents_in_snap(snap, skyhook_id, t0 + 12 * 3_600).unwrap();
        assert_eq!(sec1, 100 + 120 + 300);
        assert_eq!(insec1, 120 + 300);

        // Advance another 24 hours: all 420 insecure mature + 1200 harvested (600 sec, 600 insec)
        let (_, sec2, insec2) =
            advance_skyhook_reagents_in_snap(snap, skyhook_id, t0 + 36 * 3_600).unwrap();
        assert_eq!(sec2, sec1 + 420 + 600);
        assert_eq!(insec2, 600);
    });
}

#[tokio::test]
async fn anchor_skyhook_rpc_validation_and_deployment() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let (null_sys, planet_id, _expected_reagent, planet_pos) =
        find_null_system_with_reagent_planet(&ctx);
    let ally_id = 99_000_020_i64;

    let (corp_id, item_id, ship_id) = ctx.db.with_mut_snapshot(|snap| {
        let cid = snap.characters[&char_id].corporation_id;
        let corp = snap
            .corporations
            .entry(cid)
            .or_insert_with(|| eve_db::CorporationRecord {
                corporation_id: cid,
                ceo_id: char_id,
                alliance_id: Some(ally_id),
                ..Default::default()
            });
        corp.ceo_id = char_id;
        corp.alliance_id = Some(ally_id);
        snap.system_states.entry(null_sys).or_default().solar_system_id = null_sys;
        snap.system_states
            .entry(null_sys)
            .or_default()
            .sov_alliance_id = Some(ally_id);

        // Anchor Sov Hub in null_sys
        snap.next_entity_id += 1;
        let hub_id = snap.next_entity_id;
        snap.structures.insert(
            hub_id,
            UpwellStructureRecord {
                structure_id: hub_id,
                type_id: TYPE_SOVEREIGNTY_HUB,
                owner_corp_id: cid,
                solar_system_id: null_sys,
                name: "System Hub".to_string(),
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".to_string(),
                fuel_blocks: 200,
                ..Default::default()
            },
        );

        // Active industrial ship & skyhook item in cargo
        snap.next_entity_id += 1;
        let sid = snap.next_entity_id;
        snap.items.insert(
            sid,
            InventoryItemRecord {
                item_id: sid,
                type_id: 648, // Badger
                owner_id: char_id,
                location_id: i64::from(null_sys),
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.next_entity_id += 1;
        let sky_item = snap.next_entity_id;
        snap.items.insert(
            sky_item,
            InventoryItemRecord {
                item_id: sky_item,
                type_id: TYPE_ORBITAL_SKYHOOK,
                owner_id: char_id,
                location_id: sid,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        (cid, sky_item, sid)
    });

    let sys_handle = ctx.systems.get_or_spawn(null_sys);
    let _ = sys_handle
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 648,
            position: DVec3::from_array(planet_pos) + DVec3::new(150_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 200.0,
            inertia: 1.0,
            mass: 1.0e6,
            radius: 50.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;

    let mut sess = SessionState::new(9450);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(corp_id);
    sess.corp_role = eve_db::corp_roles::ROLE_DIRECTOR;
    sess.solar_system_id = Some(null_sys);
    sess.ship_id = Some(ship_id);

    let res = handle_anchor_skyhook_rpc(
        &ctx,
        &mut sess,
        &[
            PyRep::Integer(item_id),
            PyRep::Integer(i64::from(planet_id)),
        ],
    )
    .await
    .unwrap();

    let _skyhook_id = res.dict_get("structureID").and_then(PyRep::as_i64).unwrap();
    assert_eq!(
        res.dict_get("typeID").and_then(PyRep::as_i64),
        Some(i64::from(TYPE_ORBITAL_SKYHOOK))
    );

    // Anchoring a second skyhook on the same planet fails
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            item_id,
            InventoryItemRecord {
                item_id,
                type_id: TYPE_ORBITAL_SKYHOOK,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });
    let dup_err = handle_anchor_skyhook_rpc(
        &ctx,
        &mut sess,
        &[
            PyRep::Integer(item_id),
            PyRep::Integer(i64::from(planet_id)),
        ],
    )
    .await
    .unwrap_err();
    assert_eq!(dup_err.code, "SkyhookAlreadyExistsAtPlanet");
}

#[tokio::test]
async fn skyhook_weapon_damage_silo_link_theft_and_owner_extraction() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let thief_id = 90_000_002_i64;
    let sys_id = 30_000_142_u32;
    let skyhook_id = 1_070_000_050_i64;
    let owner_ship = 1_070_000_051_i64;
    let thief_ship = 1_070_000_052_i64;
    let now = ctx.now_unix();

    let corp_id = ctx.db.with_mut_snapshot(|snap| {
        let cid = snap.characters[&char_id].corporation_id;
        snap.corporations
            .entry(cid)
            .or_insert_with(|| eve_db::CorporationRecord {
                corporation_id: cid,
                ceo_id: char_id,
                name: "SkyhookOwnerCorp".into(),
                ticker: "SKOC".into(),
                ..Default::default()
            })
            .ceo_id = char_id;
        let mut thief = snap.characters[&char_id].clone();
        thief.character_id = thief_id;
        thief.user_id = 2;
        thief.corporation_id = cid + 1;
        snap.characters.insert(thief_id, thief);

        snap.structures.insert(
            skyhook_id,
            UpwellStructureRecord {
                structure_id: skyhook_id,
                type_id: TYPE_ORBITAL_SKYHOOK,
                owner_corp_id: cid,
                solar_system_id: sys_id,
                name: "Target Skyhook".to_string(),
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".to_string(),
                shield_hp: SKYHOOK_BASE_SHIELD_HP,
                ..Default::default()
            },
        );
        snap.sov_runtime
            .skyhook_reagent_bays
            .insert(skyhook_id, (TYPE_MAGMATIC_GAS, 100, 80, now));
        snap.sov_runtime.skyhook_last_harvest_unix.insert(skyhook_id, now);
        snap.sov_runtime
            .skyhook_theft_windows
            .insert(skyhook_id, (now.saturating_sub(60), now + 3_600));

        // Cruiser hulls (620 = Osprey, group 26 Cruiser -> eligible for silo link)
        for sid in [owner_ship, thief_ship] {
            snap.items.insert(
                sid,
                InventoryItemRecord {
                    item_id: sid,
                    type_id: 620,
                    owner_id: if sid == owner_ship { char_id } else { thief_id },
                    location_id: i64::from(sys_id),
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
        }
        cid
    });

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    for sid in [owner_ship, thief_ship] {
        let _ = sys_handle
            .spawn_ball(Ball {
                id: sid,
                type_id: 620,
                position: DVec3::new(3_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 2_500.0,
                inertia: 1.0,
                mass: 1.0e6,
                radius: 100.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            })
            .await;
    }

    let mut thief_sess = SessionState::new(9451);
    thief_sess.character_id = Some(thief_id);
    thief_sess.corporation_id = Some(corp_id + 1);
    thief_sess.solar_system_id = Some(sys_id);
    thief_sess.ship_id = Some(thief_ship);

    // InitiateLink rejected while shield > 10%
    let shield_err = handle_skyhook_component_call(
        &ctx,
        &thief_sess,
        sys_id,
        thief_id,
        thief_ship,
        skyhook_id,
        "linkWithShip",
        "InitiateLink",
        &[],
    )
    .await
    .unwrap_err();
    assert_eq!(shield_err.code, "SkyhookSiloShieldTooHigh");

    // Damage shields below 10% (200 HP remaining)
    let dmg = apply_skyhook_weapon_damage(&ctx, skyhook_id, 9_800.0).unwrap();
    assert_eq!(dmg.dict_get("shieldHP").and_then(PyRep::as_f64), Some(200.0));
    assert_eq!(dmg.dict_get("hackable").and_then(PyRep::as_bool), Some(true));

    // Thief initiates 10-minute silo link
    let link_res = handle_skyhook_component_call(
        &ctx,
        &thief_sess,
        sys_id,
        thief_id,
        thief_ship,
        skyhook_id,
        "linkWithShip",
        "InitiateLink",
        &[],
    )
    .await
    .unwrap();
    assert_eq!(link_res.dict_get("linked").and_then(PyRep::as_bool), Some(true));
    assert!(is_ship_skyhook_linked(&ctx, thief_ship, now));

    // Complete silo link via tick_skyhooks at now + 601
    tick_skyhooks(&ctx, now + SKYHOOK_SILO_LINK_DURATION_SECS + 1).await;
    assert!(!is_ship_skyhook_linked(&ctx, thief_ship, now + 601));
    ctx.db.with_snapshot(|s| {
        let stolen_item = s
            .items
            .values()
            .find(|i| i.owner_id == thief_id && i.type_id == TYPE_MAGMATIC_GAS)
            .expect("stolen reagents placed in thief cargo");
        assert!(stolen_item.quantity >= 80);
    });

    // Owner extracts secured reagents to owner_ship
    let mut owner_sess = SessionState::new(9452);
    owner_sess.user_id = Some(1);
    owner_sess.character_id = Some(char_id);
    owner_sess.corporation_id = Some(corp_id);
    owner_sess.corp_role = eve_db::corp_roles::ROLE_DIRECTOR;
    owner_sess.solar_system_id = Some(sys_id);
    owner_sess.ship_id = Some(owner_ship);

    let auth = handle_skyhook_component_call(
        &ctx,
        &owner_sess,
        sys_id,
        char_id,
        owner_ship,
        skyhook_id,
        "orbitalSkyhook",
        "is_character_authorised",
        &[],
    )
    .await
    .unwrap();
    assert_eq!(auth, PyRep::Bool(true));

    let ext = handle_skyhook_component_call(
        &ctx,
        &owner_sess,
        sys_id,
        char_id,
        owner_ship,
        skyhook_id,
        "orbitalSkyhook",
        "ExtractReagentsToShip",
        &[],
    )
    .await
    .unwrap();
    assert_eq!(
        ext.dict_get("extractedQuantity").and_then(PyRep::as_i64),
        Some(100)
    );
    assert_eq!(
        ext.dict_get("remainingSecureQuantity").and_then(PyRep::as_i64),
        Some(0)
    );
}
