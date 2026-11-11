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
    let ctx = if let Some(sde) = SHARED_SDE.get() {
        ServerContext::with_sde_and_repository(
            Arc::clone(sde),
            Arc::new(eve_db::WorldRepository::with_demo_seed()),
        )
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
    } else {
        let ctx = ServerContext::new().with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED);
        let _ = SHARED_SDE.set(Arc::clone(&ctx.sde));
        ctx
    };
    ctx.set_clock(Arc::new(eve_db::VirtualServerClock::with_fixed_base(
        1_800_000_000,
    )));
    ctx
}

fn find_null_system_with_reagent_planet(ctx: &ServerContext) -> (u32, u32, u32, [f64; 3]) {
    ctx.sde
        .solar_systems
        .values()
        .filter(|sys| {
            crate::services::sov_gameplay::is_claimable_nullsec_system(&ctx.sde, sys.solar_system_id)
        })
        .flat_map(|sys| {
            let sid = sys.solar_system_id;
            ctx.sde
                .celestials_in_system(sid)
                .iter()
                .filter_map(move |p| {
                    (p.group_id == 7)
                        .then(|| planet_reagent_type(p.type_id))
                        .flatten()
                        .map(|rtid| (sid, p.celestial_id as u32, rtid, [p.position.x, p.position.y, p.position.z]))
                })
                .collect::<Vec<_>>()
        })
        .min_by_key(|&(sid, cid, _, _)| (sid, cid))
        .unwrap_or((30_000_142, 40_009_077, TYPE_MAGMATIC_GAS, [1.0e9, 0.0, 0.0]))
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
            advance_skyhook_reagents_in_snap(snap, &ctx.sde, skyhook_id, t0 + 12 * 3_600).unwrap();
        assert_eq!(sec1, 100 + 120 + 300);
        assert_eq!(insec1, 120 + 300);

        // Advance another 24 hours: all 420 insecure mature + 1200 harvested (600 sec, 600 insec)
        let (_, sec2, insec2) =
            advance_skyhook_reagents_in_snap(snap, &ctx.sde, skyhook_id, t0 + 36 * 3_600).unwrap();
        assert_eq!(sec2, sec1 + 420 + 600);
        assert_eq!(insec2, 600);
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
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
            .or_insert_with(|| { let mut __corp = eve_db::CorporationRecord::default();
                __corp.corporation_id = cid;
                __corp.ceo_id = char_id;
                __corp.alliance_id = Some(ally_id);
                __corp });
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
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
            .or_insert_with(|| { let mut __corp = eve_db::CorporationRecord::default();
                __corp.corporation_id = cid;
                __corp.ceo_id = char_id;
                __corp.name = "SkyhookOwnerCorp".into();
                __corp.ticker = "SKOC".into();
                __corp })
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

    // Damage shields below 10% (12,250 raw * 0.8 SDE shield resonance = 9,800 effective -> 200 HP remaining)
    let dmg = apply_skyhook_weapon_damage(&ctx, skyhook_id, 12_250.0).unwrap();
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tick_skyhooks_transitions_expired_armor_reinforcement_to_vulnerable() {
    let ctx = ServerContext::new();
    let sys_id = 30_004_001_u32;
    let skyhook_id = 1_090_777_001_i64;
    let now = 1_750_000_000_u64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            skyhook_id,
            eve_db::UpwellStructureRecord {
                structure_id: skyhook_id,
                type_id: crate::services::sov_gameplay::TYPE_ORBITAL_SKYHOOK,
                owner_corp_id: 98_000_001,
                solar_system_id: sys_id,
                name: "Reinforced Skyhook".into(),
                position: [0.0, 0.0, 0.0],
                state: "ArmorReinforced".into(),
                reinforced_until_unix: now + 60,
                shield_hp: 0.0,
                armor_hp: 100.0,
                hull_hp: 100.0,
                ..Default::default()
            },
        );
    });

    // Before expiry: remains ArmorReinforced
    tick_skyhooks(&ctx, now + 59).await;
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.structures[&skyhook_id].state, "ArmorReinforced");
    });

    // At expiry: transitions to ArmorVulnerable and clears reinforced_until_unix
    tick_skyhooks(&ctx, now + 60).await;
    ctx.db.with_snapshot(|snap| {
        let s = &snap.structures[&skyhook_id];
        assert_eq!(s.state, "ArmorVulnerable");
        assert_eq!(s.reinforced_until_unix, 0);
    });
}

#[test]
fn skyhook_theft_window_reschedules_every_three_to_four_days() {
    let ctx = ServerContext::new();
    let sys_id = 30_000_142u32;
    let corp_id = 98_000_001i64;
    let skyhook_id = 900_991_101i64;
    let t0 = 1_735_732_800u64;
    let initial_window = (t0, t0 + 7_200);

    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            skyhook_id,
            UpwellStructureRecord {
                structure_id: skyhook_id,
                type_id: TYPE_ORBITAL_SKYHOOK,
                owner_corp_id: corp_id,
                solar_system_id: sys_id,
                name: "Cycle Test Skyhook".to_string(),
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".to_string(),
                shield_hp: SKYHOOK_BASE_SHIELD_HP,
                ..Default::default()
            },
        );
        snap.corp_governance.entry(corp_id).or_default().reinforce_hour = 20;
        snap.sov_runtime.skyhook_active.insert(skyhook_id, true);
        snap.sov_runtime
            .skyhook_reagent_bays
            .insert(skyhook_id, (TYPE_SUPERIONIC_ICE, 0, 0, t0));
        snap.sov_runtime
            .skyhook_theft_windows
            .insert(skyhook_id, initial_window);
        snap.sov_runtime.skyhook_hackable.insert(skyhook_id, true);

        // Inside active window -> unchanged
        let _ = advance_skyhook_reagents_in_snap(snap, &ctx.sde, skyhook_id, t0 + 3_600);
        assert_eq!(snap.sov_runtime.skyhook_theft_windows[&skyhook_id], initial_window);

        // After 2h window ends -> advances 3..=4 days (72h..=96h) and clears stale hackable flag
        let _ = advance_skyhook_reagents_in_snap(snap, &ctx.sde, skyhook_id, t0 + 7_201);
        let (w2_start, w2_end) = snap.sov_runtime.skyhook_theft_windows[&skyhook_id];
        assert_eq!(w2_end - w2_start, 7_200);
        let delta = w2_start - t0;
        assert!(
            (259_200..=345_600).contains(&delta),
            "theft window cadence must be 3-4 days (72h..=96h), got {delta}s"
        );
        assert!(!snap.sov_runtime.skyhook_hackable[&skyhook_id]);
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tick_skyhooks_schedules_missing_theft_window_three_to_four_days_out() {
    let ctx = ServerContext::new();
    let sys_id = 30_000_142u32;
    let corp_id = 98_000_001i64;
    let skyhook_id = 900_991_102i64;
    let t0 = 1_735_732_800u64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            skyhook_id,
            UpwellStructureRecord {
                structure_id: skyhook_id,
                type_id: TYPE_ORBITAL_SKYHOOK,
                owner_corp_id: corp_id,
                solar_system_id: sys_id,
                name: "First Tick Skyhook".to_string(),
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".to_string(),
                shield_hp: SKYHOOK_BASE_SHIELD_HP,
                ..Default::default()
            },
        );
        snap.corp_governance.entry(corp_id).or_default().reinforce_hour = 20;
        snap.sov_runtime.skyhook_active.insert(skyhook_id, true);
        snap.sov_runtime
            .skyhook_reagent_bays
            .insert(skyhook_id, (TYPE_SUPERIONIC_ICE, 0, 0, t0));
        // Deliberately omit `skyhook_theft_windows` entry so tick_skyhooks initializes it.
    });

    tick_skyhooks(&ctx, t0).await;

    ctx.db.with_snapshot(|snap| {
        let (start, end) = snap
            .sov_runtime
            .skyhook_theft_windows
            .get(&skyhook_id)
            .copied()
            .expect("first tick must schedule theft window");
        assert_eq!(end - start, 7_200);
        let delta = start.saturating_sub(t0);
        assert!(
            (259_200..=345_600).contains(&delta),
            "first tick must schedule theft window 3-4 days ahead (72h..=96h), got {delta}s"
        );
        assert!(
            !snap
                .sov_runtime
                .skyhook_hackable
                .get(&skyhook_id)
                .copied()
                .unwrap_or(false)
        );
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn skyhook_anchor_component_error_guards_and_tick_branches() {
    let ctx = test_ctx();
    let (sys_id, planet_id, rtid, p_pos) = find_null_system_with_reagent_planet(&ctx);
    let char_id = 90_000_001_i64;
    let non_dir_char = 90_000_002_i64;
    let ship_cruiser = 100_888_001_i64;
    let ship_frig = 100_888_002_i64;
    let cruiser_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.group_id == 26)
        .map(|t| t.type_id)
        .unwrap_or(620);
    let now = ctx.now_unix();

    assert_eq!(planet_reagent_type(11), None);
    assert_eq!(extract_extra_var(Some(&PyRep::Dict(vec![(PyRep::String("planetID".into()), PyRep::Integer(42))])), "planetid"), Some(42));
    assert_eq!(extract_extra_var(Some(&PyRep::keyval([("planetID", PyRep::Integer(7))])), "planetID"), Some(7));
    assert_eq!(extract_extra_var(None, "planetID"), None);

    // Non-finite damage checks
    assert_eq!(apply_skyhook_weapon_damage(&ctx, 1, f64::NAN).unwrap_err().code, "InvalidDamage");
    assert_eq!(apply_skyhook_weapon_damage_typed(&ctx, 1, f64::INFINITY, 0.0, 0.0, 0.0).unwrap_err().code, "InvalidDamage");
    assert_eq!(apply_skyhook_weapon_damage_typed(&ctx, 999_999, 100.0, 0.0, 0.0, 0.0).unwrap_err().code, "StructureNotFound");

    let corp_id = ctx.db.with_mut_snapshot(|snap| {
        let cid = snap.characters[&char_id].corporation_id;
        {
            let c = snap.corporations.entry(cid).or_default();
            c.corporation_id = cid;
            c.ceo_id = char_id;
            c.alliance_id = Some(99_888_001);
        }
        let mut c2 = snap.characters[&char_id].clone();
        c2.character_id = non_dir_char;
        c2.corporation_id = cid;
        snap.characters.insert(non_dir_char, c2);
        snap.system_states.entry(sys_id).or_default().sov_alliance_id = Some(99_888_001);
        snap.items.insert(
            ship_cruiser,
            InventoryItemRecord {
                item_id: ship_cruiser,
                type_id: cruiser_tid,
                owner_id: char_id,
                location_id: i64::from(sys_id),
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            ship_frig,
            InventoryItemRecord {
                item_id: ship_frig,
                type_id: 587,
                owner_id: char_id,
                location_id: i64::from(sys_id),
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        cid
    });

    // Anchor RPC error guards: docked, no system, non-nullsec, ship not in space, corp role denied, too far / missing item
    let mut s_docked = SessionState::new(901);
    s_docked.user_id = Some(1);
    s_docked.character_id = Some(char_id);
    s_docked.corporation_id = Some(corp_id);
    s_docked.corp_role = eve_db::corp_roles::ROLE_DIRECTOR;
    s_docked.station_id = Some(60_003_760);
    assert_eq!(handle_anchor_skyhook_rpc(&ctx, &mut s_docked, &[]).await.unwrap_err().code, "ShipNotInSpace");

    let mut s_nosys = SessionState::new(902);
    s_nosys.user_id = Some(1);
    s_nosys.character_id = Some(char_id);
    assert_eq!(handle_anchor_skyhook_rpc(&ctx, &mut s_nosys, &[]).await.unwrap_err().code, "NoSolarSystem");

    let mut s_hi = SessionState::new(903);
    s_hi.user_id = Some(1);
    s_hi.character_id = Some(char_id);
    s_hi.corporation_id = Some(corp_id);
    s_hi.corp_role = eve_db::corp_roles::ROLE_DIRECTOR;
    s_hi.solar_system_id = Some(30_000_142);
    s_hi.ship_id = Some(ship_cruiser);
    let (jita_planet, jita_pos) = ctx
        .sde
        .celestials_in_system(30_000_142)
        .into_iter()
        .find(|c| c.group_id == 7)
        .map(|c| (i64::from(c.celestial_id as u32), c.position))
        .unwrap_or((40_000_002, DVec3::ZERO));
    let hi_h = ctx.systems.get_or_spawn(30_000_142);
    let _ = hi_h.spawn_ball(Ball {
        id: ship_cruiser,
        type_id: cruiser_tid,
        position: DVec3::new(jita_pos.x + 5_000.0, jita_pos.y, jita_pos.z),
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 1.0,
        mass: 1.0e7,
        radius: 120.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    }).await;
    assert_eq!(handle_anchor_skyhook_rpc(&ctx, &mut s_hi, &[PyRep::Integer(jita_planet)]).await.unwrap_err().code, "CannotAnchorSkyhookInEmpireSpace");

    let mut s_ok = SessionState::new(904);
    s_ok.user_id = Some(1);
    s_ok.character_id = Some(char_id);
    s_ok.corporation_id = Some(corp_id);
    s_ok.corp_role = eve_db::corp_roles::ROLE_DIRECTOR;
    s_ok.solar_system_id = Some(sys_id);
    s_ok.ship_id = Some(ship_cruiser);
    assert_eq!(handle_anchor_skyhook_rpc(&ctx, &mut s_ok, &[PyRep::Integer(i64::from(planet_id))]).await.unwrap_err().code, "ShipNotInSpace");

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_handle.spawn_ball(Ball {
        id: ship_cruiser,
        type_id: cruiser_tid,
        position: DVec3::new(p_pos[0] + 750_000.0, p_pos[1], p_pos[2]),
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 1.0,
        mass: 1.0e7,
        radius: 120.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    }).await;

    let mut s_nondir = s_ok.clone();
    s_nondir.corp_role = 0;
    s_nondir.character_id = Some(non_dir_char);
    assert_eq!(handle_anchor_skyhook_rpc(&ctx, &mut s_nondir, &[]).await.unwrap_err().code, "InsufficientCorpRoles");

    // Ship >500km from planet & anchor point -> TooFarFromPlanet
    assert_eq!(
        handle_anchor_skyhook_rpc(&ctx, &mut s_ok, &[PyRep::Integer(i64::from(planet_id))]).await.unwrap_err().code,
        "TooFarFromPlanet"
    );

    // Move ship close to planet, no item in cargo -> MissingSkyhookItem
    let _ = sys_handle.remove_ball(ship_cruiser).await;
    let _ = sys_handle.spawn_ball(Ball {
        id: ship_cruiser,
        type_id: cruiser_tid,
        position: DVec3::new(p_pos[0] + 5_000.0, p_pos[1], p_pos[2]),
        velocity: DVec3::ZERO,
        max_velocity: 1_500.0,
        inertia: 1.0,
        mass: 1.0e7,
        radius: 120.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    }).await;
    assert_eq!(
        handle_anchor_skyhook_rpc(&ctx, &mut s_ok, &[PyRep::Integer(i64::from(TYPE_ORBITAL_SKYHOOK)), PyRep::Integer(i64::from(planet_id))]).await.unwrap_err().code,
        "MissingSkyhookItem"
    );

    // Skyhook component errors + damage state transitions + tick_skyhooks window rollover & broken/completed link
    let sh_id = 1_089_000_001_i64;
    let sh_dead = 1_089_000_002_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(sh_id, UpwellStructureRecord {
            structure_id: sh_id,
            type_id: TYPE_ORBITAL_SKYHOOK,
            owner_corp_id: corp_id,
            solar_system_id: sys_id,
            name: "Edge Skyhook".into(),
            position: p_pos,
            state: "ShieldVulnerable".into(),
            shield_hp: SKYHOOK_BASE_SHIELD_HP,
            armor_hp: SKYHOOK_BASE_SHIELD_HP,
            hull_hp: SKYHOOK_BASE_SHIELD_HP,
            ..Default::default()
        });
        snap.structures.insert(sh_dead, UpwellStructureRecord {
            structure_id: sh_dead,
            type_id: TYPE_ORBITAL_SKYHOOK,
            owner_corp_id: corp_id,
            solar_system_id: sys_id,
            name: "Dead Skyhook".into(),
            position: p_pos,
            state: "Destroyed".into(),
            ..Default::default()
        });
        snap.sov_runtime.skyhook_planets.insert(sh_id, i64::from(planet_id));
        snap.sov_runtime.skyhook_active.insert(sh_id, false);
        snap.sov_runtime.skyhook_reagent_bays.insert(sh_id, (rtid, 0, 200, now.saturating_sub(100)));
        snap.sov_runtime.skyhook_theft_windows.insert(sh_id, (now.saturating_sub(20_000), now.saturating_sub(10_000)));
    });

    assert_eq!(apply_skyhook_weapon_damage(&ctx, sh_dead, 100.0).unwrap_err().code, "StructureDestroyed");
    // Ship with item in cargo on already-hosted planet -> SkyhookAlreadyAnchoredOnPlanet
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(888_111, InventoryItemRecord {
            item_id: 888_111,
            type_id: TYPE_ORBITAL_SKYHOOK,
            owner_id: char_id,
            location_id: ship_cruiser,
            flag: InventoryFlag::Cargo,
            quantity: 1,
            ..Default::default()
        });
    });
    assert_eq!(
        handle_anchor_skyhook_rpc(&ctx, &mut s_ok, &[PyRep::Integer(888_111), PyRep::Integer(i64::from(planet_id))]).await.unwrap_err().code,
        "SkyhookAlreadyExistsAtPlanet"
    );

    // Component guards: ShipNotInSpace, SkyhookNotFound, TargetNotInSameSystem, OutOfSkyhookLinkRange, IneligibleShipForSkyhookLink, SkyhookSiloShieldTooHigh, SkyhookNotVulnerableToTheft, Extract errors, Unsupported
    assert_eq!(handle_skyhook_component_call(&ctx, &s_ok, sys_id, char_id, 999_777, sh_id, "orbitalSkyhook", "is_character_authorised", &[]).await.unwrap_err().code, "ShipNotInSpace");
    assert_eq!(handle_skyhook_component_call(&ctx, &s_ok, sys_id, char_id, ship_cruiser, 999_666, "orbitalSkyhook", "is_character_authorised", &[]).await.unwrap_err().code, "SkyhookNotFound");
    assert_eq!(handle_skyhook_component_call(&ctx, &s_ok, sys_id + 1, char_id, ship_cruiser, sh_id, "orbitalSkyhook", "is_character_authorised", &[]).await.unwrap_err().code, "ShipNotInSpace");

    // Move ship_cruiser within 1 km of skyhook at p_pos; keep ship_frig far for OutOfSkyhookLinkRange then move close for IneligibleShipForSkyhookLink
    let _ = sys_handle.remove_ball(ship_cruiser).await;
    let _ = sys_handle.spawn_ball(Ball {
        id: ship_cruiser,
        type_id: cruiser_tid,
        position: DVec3::new(p_pos[0] + 1_000.0, p_pos[1], p_pos[2]),
        velocity: DVec3::ZERO,
        max_velocity: 1_500.0,
        inertia: 1.0,
        mass: 1.0e7,
        radius: 120.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    }).await;
    let _ = sys_handle.spawn_ball(Ball {
        id: ship_frig,
        type_id: 587,
        position: DVec3::new(p_pos[0] + 50_000.0, p_pos[1], p_pos[2]),
        velocity: DVec3::ZERO,
        max_velocity: 400.0,
        inertia: 1.0,
        mass: 1.0e6,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    }).await;
    assert_eq!(handle_skyhook_component_call(&ctx, &s_ok, sys_id, char_id, ship_frig, sh_id, "linkWithShip", "InitiateLink", &[]).await.unwrap_err().code, "OutOfSkyhookLinkRange");

    let _ = sys_handle.remove_ball(ship_frig).await;
    let _ = sys_handle.spawn_ball(Ball {
        id: ship_frig,
        type_id: 587,
        position: DVec3::new(p_pos[0] + 1_000.0, p_pos[1], p_pos[2]),
        velocity: DVec3::ZERO,
        max_velocity: 400.0,
        inertia: 1.0,
        mass: 1.0e6,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    }).await;
    assert_eq!(handle_skyhook_component_call(&ctx, &s_ok, sys_id, char_id, ship_frig, sh_id, "linkWithShip", "InitiateLink", &[]).await.unwrap_err().code, "IneligibleShipForSkyhookLink");

    ctx.db.with_mut_snapshot(|snap| { snap.structures.get_mut(&sh_id).unwrap().shield_hp = 5_000_000.0; });
    assert_eq!(handle_skyhook_component_call(&ctx, &s_ok, sys_id, char_id, ship_cruiser, sh_id, "linkWithShip", "InitiateLink", &[]).await.unwrap_err().code, "SkyhookSiloShieldTooHigh");

    ctx.db.with_mut_snapshot(|snap| { snap.structures.get_mut(&sh_id).unwrap().shield_hp = 0.0; });
    assert_eq!(handle_skyhook_component_call(&ctx, &s_ok, sys_id, char_id, ship_cruiser, sh_id, "linkWithShip", "InitiateLink", &[]).await.unwrap_err().code, "SkyhookNotVulnerableToTheft");

    ctx.db.with_mut_snapshot(|snap| { snap.sov_runtime.skyhook_theft_windows.insert(sh_id, (now - 10, now + 3_600)); });
    assert_eq!(handle_skyhook_component_call(&ctx, &s_nondir, sys_id, non_dir_char, ship_cruiser, sh_id, "orbitalSkyhook", "ExtractReagentsToShip", &[]).await.unwrap_err().code, "NotAuthorisedForSkyhookExtraction");
    assert_eq!(handle_skyhook_component_call(&ctx, &s_ok, sys_id, char_id, ship_cruiser, sh_id, "orbitalSkyhook", "ExtractReagentsToShip", &[]).await.unwrap_err().code, "NoSecuredReagentsAvailable");
    assert_eq!(handle_skyhook_component_call(&ctx, &s_ok, sys_id, char_id, ship_cruiser, sh_id, "orbitalSkyhook", "unknownMethod", &[]).await.unwrap_err().code, "UnsupportedComponentCall");

    // Deplete shield outside window -> ArmorReinforced -> StructureInvulnerable on subsequent hit
    ctx.db.with_mut_snapshot(|snap| {
        snap.sov_runtime.skyhook_theft_windows.insert(sh_id, (now + 10_000, now + 17_200));
        snap.structures.get_mut(&sh_id).unwrap().shield_hp = 500.0;
    });
    let reinforced = apply_skyhook_weapon_damage(&ctx, sh_id, 5_000.0).unwrap();
    assert_eq!(reinforced.dict_get("state").and_then(PyRep::as_str), Some("ArmorReinforced"));
    assert_eq!(apply_skyhook_weapon_damage(&ctx, sh_id, 500.0).unwrap_err().code, "StructureInvulnerable");

    // tick_skyhooks: expired window rolls forward, dead link removed, broken link (cloaked) restored, completed link drops jetcan!
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.get_mut(&sh_id).unwrap().state = "ShieldVulnerable".into();
        snap.structures.get_mut(&sh_id).unwrap().shield_hp = 500.0;
        snap.sov_runtime.skyhook_theft_windows.insert(sh_id, (now.saturating_sub(400_000), now.saturating_sub(392_800)));
        snap.sov_runtime.skyhook_silo_links.insert(sh_dead, (char_id, ship_cruiser, now + 60, 1_500.0));
        snap.sov_runtime.skyhook_silo_links.insert(sh_id, (char_id, ship_cruiser, now + 100, 1_500.0));
    });
    let _ = sys_handle.set_combat_state(ship_cruiser, eve_destiny::CombatState { is_cloaked: true, ..Default::default() }).await;
    tick_skyhooks(&ctx, now + 10).await;
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.sov_runtime.skyhook_silo_links.contains_key(&sh_id));
        assert!(!snap.sov_runtime.skyhook_silo_links.contains_key(&sh_dead));
        assert!(snap.sov_runtime.skyhook_theft_windows[&sh_id].1 > now + 10);
    });

    // Active in-progress link clamps ship max velocity during tick_skyhooks
    let _ = sys_handle.set_ball_max_velocity(ship_cruiser, 1_500.0).await;
    let _ = sys_handle.set_combat_state(ship_cruiser, eve_destiny::CombatState { is_cloaked: false, ..Default::default() }).await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.sov_runtime.skyhook_silo_links.insert(sh_id, (char_id, ship_cruiser, now + 100, 1_500.0));
        snap.sov_runtime.skyhook_theft_windows.insert(sh_id, (now, now + 7_200));
    });
    tick_skyhooks(&ctx, now + 10).await;
    let capped_vel = sys_handle.get_ball_snapshot(ship_cruiser).await.unwrap().unwrap().1.max_velocity;
    assert!(capped_vel <= SKYHOOK_SILO_LINK_MAX_SPEED_MS + 1e-6);

    // Uncloak & complete link -> spawns loot container
    ctx.db.with_mut_snapshot(|snap| {
        snap.sov_runtime.skyhook_silo_links.insert(sh_id, (char_id, ship_cruiser, now + 5, 1_500.0));
    });
    tick_skyhooks(&ctx, now + 10).await;
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.sov_runtime.skyhook_silo_links.contains_key(&sh_id));
        assert_eq!(snap.sov_runtime.skyhook_reagent_bays[&sh_id].2, 0);
    });

    // Theft window fallback to skyhook_hackable flag, plus missing reagent bay on ExtractSecuredReagents
    ctx.db.with_mut_snapshot(|snap| {
        snap.sov_runtime.skyhook_silo_links.remove(&sh_id);
        snap.sov_runtime.skyhook_theft_windows.remove(&sh_id);
        snap.sov_runtime.skyhook_hackable.insert(sh_id, true);
        if let Some(st) = snap.structures.get_mut(&sh_id) {
            st.shield_hp = 500.0;
        }
        snap.sov_runtime.skyhook_reagent_bays.insert(sh_id, (81143, 100, 50, now));
    });
    let _ = sys_handle.set_ball_position(ship_cruiser, DVec3::new(p_pos[0] + 1_000.0, p_pos[1], p_pos[2])).await;
    let sess_dir = s_ok.clone();
    assert!(handle_skyhook_component_call(&ctx, &sess_dir, sys_id, char_id, ship_cruiser, sh_id, "linkWithShip", "InitiateLink", &[]).await.is_ok());

    ctx.db.with_mut_snapshot(|snap| {
        snap.sov_runtime.skyhook_reagent_bays.remove(&sh_id);
    });
    assert_eq!(
        handle_skyhook_component_call(&ctx, &sess_dir, sys_id, char_id, ship_cruiser, sh_id, "orbitalSkyhook", "ExtractReagentsToShip", &[])
            .await
            .unwrap_err()
            .code,
        "SkyhookReagentBayNotFound"
    );
}

#[tokio::test]
async fn next_skyhook_theft_window_and_tick_boundaries_are_exact() {
    // 1. Direct formula & boundary verification on `next_skyhook_theft_window` and `advance_skyhook_theft_window_in_snap`
    let now = 86_400 * 10 + 18 * 3_600; // Day 10, 18:00 UTC
    for sid in 1_i64..=10 {
        for rh in [None, Some(20_u8), Some(30_u8)] {
            let (s, e) = next_skyhook_theft_window(sid, now, rh, now);
            assert_eq!(e - s, 7_200);
            assert!((259_200..=345_600).contains(&s.saturating_sub(now)));
            // Verify exact boundary when `prev_start + 7_200 == now_unix`: loop advances one more cycle so `next_end > now_unix`
            let (s_catchup, e_catchup) = next_skyhook_theft_window(sid, s, rh, s + 7_200);
            assert!(s_catchup > s + 7_200 && e_catchup == s_catchup + 7_200);
        }
    }

    // 2. Exact boundary verification on `tick_skyhooks`:
    //    - `reinforced_until_unix == 0` stays `ArmorReinforced`
    //    - `now == rf_until - 1` stays `ArmorReinforced`
    //    - `now == rf_until` transitions to `ArmorVulnerable` and resets `reinforced_until_unix = 0`
    //    - `now == w_start` (with low shield_hp) sets `skyhook_hackable = true`
    //    - `now == w_end - 1` keeps `skyhook_hackable = true`
    //    - `now == w_end` sets `skyhook_hackable = false` and reschedules window
    let ctx = ServerContext::new();
    let sh_rf0 = 1_900_000_201i64;
    let sh_rf = 1_900_000_202i64;
    let rf_until = 200_000u64;
    let w_start = 300_000u64;
    let w_end = 307_200u64;

    ctx.db.with_mut_snapshot(|snap| {
        for (id, rf) in [(sh_rf0, 0u64), (sh_rf, rf_until)] {
            snap.structures.insert(
                id,
                UpwellStructureRecord {
                    structure_id: id,
                    type_id: TYPE_ORBITAL_SKYHOOK,
                    owner_corp_id: 98_000_001,
                    solar_system_id: 30_004_759,
                    name: format!("Boundary Skyhook {id}"),
                    state: "ArmorReinforced".to_string(),
                    reinforced_until_unix: rf,
                    shield_hp: SKYHOOK_BASE_SHIELD_HP * 0.05,
                    ..Default::default()
                },
            );
            snap.sov_runtime.skyhook_active.insert(id, true);
            snap.sov_runtime
                .skyhook_theft_windows
                .insert(id, (w_start, w_end));
        }
    });

    // At rf_until - 1: neither transitions
    tick_skyhooks(&ctx, rf_until - 1).await;
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.structures[&sh_rf0].state, "ArmorReinforced");
        assert_eq!(snap.structures[&sh_rf].state, "ArmorReinforced");
    });

    // At exact rf_until: sh_rf transitions to ArmorVulnerable, sh_rf0 (reinforced_until_unix == 0) remains ArmorReinforced
    tick_skyhooks(&ctx, rf_until).await;
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.structures[&sh_rf0].state, "ArmorReinforced");
        assert_eq!(snap.structures[&sh_rf].state, "ArmorVulnerable");
        assert_eq!(snap.structures[&sh_rf].reinforced_until_unix, 0);
    });

    // At w_start - 1: not hackable
    tick_skyhooks(&ctx, w_start - 1).await;
    assert!(!ctx.db.with_snapshot(|s| *s.sov_runtime.skyhook_hackable.get(&sh_rf).unwrap_or(&false)));

    // At exact w_start: hackable = true
    tick_skyhooks(&ctx, w_start).await;
    assert!(ctx.db.with_snapshot(|s| s.sov_runtime.skyhook_hackable[&sh_rf]));

    // At w_end - 1: still hackable = true
    tick_skyhooks(&ctx, w_end - 1).await;
    assert!(ctx.db.with_snapshot(|s| s.sov_runtime.skyhook_hackable[&sh_rf]));

    // At exact w_end: hackable = false and theft window rescheduled past w_end
    tick_skyhooks(&ctx, w_end).await;
    ctx.db.with_snapshot(|s| {
        assert!(!s.sov_runtime.skyhook_hackable[&sh_rf]);
        let (new_s, new_e) = s.sov_runtime.skyhook_theft_windows[&sh_rf];
        assert!(new_s > w_end);
        assert_eq!(new_e - new_s, 7_200);
    });
}
