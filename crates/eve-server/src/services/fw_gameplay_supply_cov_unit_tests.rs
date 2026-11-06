//! Comprehensive unit and mutation-killing tests for `fw_gameplay_supply`.
//!
//! Exercises SDE type/HP/radius resolution, warzone tactical objective site seeding and
//! deduplication, militia loot table mapping, damage spillover across shield/armor/hull,
//! negative/abuse rejections, container ball spawning, and system advantage shifts.

#![forbid(unsafe_code)]

use super::*;
use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::{
    make_fighter_ctx_and_session, make_fighter_type,
};
use eve_db::snapshot_ext::FwSystemOccupationRecord;
use eve_db::{CharacterRecord, IskCents};
use eve_destiny::{Ball, MovementMode};
use eve_sde::SdeRegistry;
use glam::DVec3;
use std::sync::Arc;

fn seed_militia_pilot(
    snap: &mut WorldStateSnapshot,
    char_id: i64,
    war_faction_id: Option<i64>,
    sys_id: u32,
) {
    let mut ch: CharacterRecord = snap.characters.values().next().expect("demo pilot").clone();
    ch.character_id = char_id;
    ch.user_id = char_id as u64;
    ch.name = format!("SupplyPilot_{char_id}");
    ch.corporation_id = 1_000_180;
    ch.war_faction_id = war_faction_id;
    ch.solar_system_id = sys_id;
    ch.station_id = None;
    ch.structure_id = None;
    ch.balance = IskCents::from_isk(25_000_000);
    snap.characters.insert(char_id, ch);
}

#[test]
fn resolve_supply_site_sde_spec_and_loot_types_cover_all_factions_kinds_and_boundaries() {
    let empty_sde = SdeRegistry::default();
    let fallback_tid = 888_001_u32;

    // 1. All 5 supply_loot_type arms (Amarr, Minmatar, Caldari, Gallente, wildcard)
    assert_eq!(supply_loot_type(500_003), 15_607);
    assert_eq!(supply_loot_type(500_002), 15_625);
    assert_eq!(supply_loot_type(500_001), 15_596);
    assert_eq!(supply_loot_type(500_004), 15_587);
    assert_eq!(supply_loot_type(500_999), 15_596);
    assert_eq!(supply_loot_type(0), 15_596);

    // 2. Empty SDE falls back to fallback_type_id and default HP (3000.0 for SupplyDepot, 1500.0 otherwise) + 150.0 radius
    assert_eq!(
        resolve_supply_site_sde_spec(&empty_sde, 500_001, "SupplyDepot", fallback_tid),
        (fallback_tid, 3_000.0, 3_000.0, 3_000.0, 150.0)
    );
    assert_eq!(
        resolve_supply_site_sde_spec(&empty_sde, 500_001, "SupplyCache", fallback_tid),
        (fallback_tid, 1_500.0, 1_500.0, 1_500.0, 150.0)
    );
    assert_eq!(
        resolve_supply_site_sde_spec(&empty_sde, 500_001, "Propaganda", fallback_tid),
        (fallback_tid, 1_500.0, 1_500.0, 1_500.0, 150.0)
    );
    assert_eq!(
        resolve_supply_site_sde_spec(&empty_sde, 500_001, "ListeningPost", fallback_tid),
        (fallback_tid, 1_500.0, 1_500.0, 1_500.0, 150.0)
    );

    // 3. Populate exact SDE types for Propaganda (4 empires + fallback), ListeningPost (4 empires + fallback),
    //    SupplyDepot (354_772), and SupplyCache (93_877) with custom attributes and boundary values
    let mut sde = SdeRegistry::default();
    for tid in [
        73_225_u32, 73_226, 73_227, 73_228, 73_270, 73_271, 73_272, 73_273, 354_772, 93_877,
    ] {
        let mut inv = make_fighter_type(tid, 15, 2, 1_000.0, vec![]);
        inv.radius = 420.0;
        inv.default_attributes.insert(263, 4_100.0);
        inv.default_attributes.insert(265, 5_200.0);
        inv.default_attributes.insert(9, 6_300.0);
        sde.types.insert(tid, inv);
    }

    for (fac, exp_prop, exp_listen) in [
        (500_003_i64, 73_225_u32, 73_270_u32),
        (500_002, 73_226, 73_271),
        (500_001, 73_227, 73_272),
        (500_004, 73_228, 73_273),
        (500_777, 73_227, 73_272),
    ] {
        assert_eq!(
            resolve_supply_site_sde_spec(&sde, fac, "Propaganda", fallback_tid),
            (exp_prop, 4_100.0, 5_200.0, 6_300.0, 420.0)
        );
        assert_eq!(
            resolve_supply_site_sde_spec(&sde, fac, "ListeningPost", fallback_tid),
            (exp_listen, 4_100.0, 5_200.0, 6_300.0, 420.0)
        );
    }
    assert_eq!(
        resolve_supply_site_sde_spec(&sde, 500_001, "SupplyDepot", fallback_tid).0,
        354_772
    );
    assert_eq!(
        resolve_supply_site_sde_spec(&sde, 500_001, "SupplyCache", fallback_tid).0,
        93_877
    );

    // 4. Boundary check on <= 0.0 attributes and <= 0.0 radius: 0.0 and negative values fall back!
    let zero_depot = sde.types.get_mut(&354_772).unwrap();
    zero_depot.radius = 0.0;
    zero_depot.default_attributes.insert(263, 0.0);
    zero_depot.default_attributes.insert(265, -10.0);
    zero_depot.default_attributes.remove(&9);
    assert_eq!(
        resolve_supply_site_sde_spec(&sde, 500_001, "SupplyDepot", fallback_tid),
        (354_772, 3_000.0, 3_000.0, 3_000.0, 150.0)
    );

    let neg_cache = sde.types.get_mut(&93_877).unwrap();
    neg_cache.radius = -5.0;
    neg_cache.default_attributes.insert(263, -1.0);
    neg_cache.default_attributes.insert(265, 0.0);
    neg_cache.default_attributes.insert(9, 0.0);
    assert_eq!(
        resolve_supply_site_sde_spec(&sde, 500_001, "SupplyCache", fallback_tid),
        (93_877, 1_500.0, 1_500.0, 1_500.0, 150.0)
    );
}

#[test]
fn spawn_warzone_supply_sites_offsets_deduplication_and_destroyed_respawn() {
    let (mut ctx, _sess, _char_id, _ship_id, sys_id) = make_fighter_ctx_and_session();
    let mut sde = (*ctx.sde).clone();
    let mut depot_t = make_fighter_type(354_772, 15, 2, 1_000.0, vec![]);
    depot_t.radius = 275.0;
    depot_t.default_attributes.insert(263, 3_200.0);
    depot_t.default_attributes.insert(265, 3_400.0);
    depot_t.default_attributes.insert(9, 3_600.0);
    sde.types.insert(354_772, depot_t);
    ctx.sde = Arc::new(sde);

    let base_pos =
        crate::services::space_destiny::space_docking_cloaking::pick_system_celestial_position(
            &ctx, sys_id, 5,
        )
        .0;
    let fallback_tid = 900_123_u32;
    let occ_fac = 500_004_i64;
    let occ_corp = 1_000_181_i64;

    let mut first_balls = Vec::new();
    ctx.db.with_mut_snapshot(|snap| {
        // Pre-seed a supply site in a DIFFERENT system to verify ssys == sys_id filtering
        snap.next_entity_id += 1;
        let other_sid = snap.next_entity_id;
        snap.fw_runtime
            .supply_sites
            .insert(other_sid, (sys_id + 1, "SupplyCache".into(), occ_fac));
        snap.structures.insert(
            other_sid,
            UpwellStructureRecord {
                structure_id: other_sid,
                type_id: fallback_tid,
                owner_corp_id: occ_corp,
                solar_system_id: sys_id + 1,
                name: "Other System Cache".into(),
                state: "ShieldVulnerable".into(),
                ..Default::default()
            },
        );

        spawn_warzone_supply_sites_in_snap(
            &ctx,
            snap,
            sys_id,
            occ_fac,
            occ_corp,
            fallback_tid,
            &mut first_balls,
        );
    });

    assert_eq!(first_balls.len(), 4);
    assert_eq!(
        first_balls[0].2,
        [base_pos[0], base_pos[1] + 20_000.0, base_pos[2]]
    );
    assert_eq!(
        first_balls[1].2,
        [base_pos[0] + 5_000.0, base_pos[1] - 20_000.0, base_pos[2]]
    );
    assert_eq!(first_balls[1].1, 354_772);
    assert_eq!(first_balls[1].3, 275.0);
    assert_eq!(
        first_balls[2].2,
        [base_pos[0] + 10_000.0, base_pos[1] + 25_000.0, base_pos[2]]
    );
    assert_eq!(
        first_balls[3].2,
        [base_pos[0] + 15_000.0, base_pos[1] - 25_000.0, base_pos[2]]
    );

    // Calling again when all 4 sites are intact reuses the exact same structure IDs
    let mut second_balls = Vec::new();
    let entity_before = ctx.db.with_snapshot(|s| s.next_entity_id);
    ctx.db.with_mut_snapshot(|snap| {
        spawn_warzone_supply_sites_in_snap(
            &ctx,
            snap,
            sys_id,
            occ_fac,
            occ_corp,
            fallback_tid,
            &mut second_balls,
        );
    });
    let entity_after = ctx.db.with_snapshot(|s| s.next_entity_id);
    assert_eq!(entity_before, entity_after);
    assert_eq!(
        first_balls.iter().map(|b| b.0).collect::<Vec<_>>(),
        second_balls.iter().map(|b| b.0).collect::<Vec<_>>()
    );

    // Mark SupplyDepot Destroyed and remove Propaganda structure record -> respawns ONLY those 2!
    let depot_id = first_balls[1].0;
    let prop_id = first_balls[3].0;
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.get_mut(&depot_id).unwrap().state = "Destroyed".into();
        snap.structures.remove(&prop_id);
    });

    let mut third_balls = Vec::new();
    ctx.db.with_mut_snapshot(|snap| {
        spawn_warzone_supply_sites_in_snap(
            &ctx,
            snap,
            occ_fac as u32, // sanity check different sys_id first
            occ_fac,
            occ_corp,
            fallback_tid,
            &mut Vec::new(),
        );
        spawn_warzone_supply_sites_in_snap(
            &ctx,
            snap,
            sys_id,
            occ_fac,
            occ_corp,
            fallback_tid,
            &mut third_balls,
        );
    });
    assert_eq!(third_balls[0].0, first_balls[0].0);
    assert_ne!(third_balls[1].0, depot_id);
    assert_eq!(third_balls[2].0, first_balls[2].0);
    assert_ne!(third_balls[3].0, prop_id);
    ctx.db.with_snapshot(|snap| {
        let new_depot = &snap.structures[&third_balls[1].0];
        assert_eq!(new_depot.name, "Militia SupplyDepot");
        assert_eq!(new_depot.owner_corp_id, occ_corp);
        assert_eq!(new_depot.shield_hp, 3_200.0);
        assert_eq!(new_depot.armor_hp, 3_400.0);
        assert_eq!(new_depot.hull_hp, 3_600.0);
    });
}

#[tokio::test]
async fn apply_supply_site_damage_rejections_zero_boundary_drops_and_advantage_sides() {
    let (ctx, _sess, _char_id, _ship_id, sys_id) = make_fighter_ctx_and_session();
    let unenlisted = 950_100_i64;
    let cal_pilot = 950_101_i64; // 500_001
    let gal_pilot = 950_102_i64; // 500_004
    let min_pilot = 950_103_i64; // 500_002

    ctx.db.with_mut_snapshot(|snap| {
        seed_militia_pilot(snap, unenlisted, None, sys_id);
        seed_militia_pilot(snap, cal_pilot, Some(500_001), sys_id);
        seed_militia_pilot(snap, gal_pilot, Some(500_004), sys_id);
        seed_militia_pilot(snap, min_pilot, Some(500_002), sys_id);
        snap.fw_system_occupations.insert(
            sys_id,
            FwSystemOccupationRecord {
                solar_system_id: sys_id,
                owner_faction_id: 500_001,
                occupier_faction_id: 500_001,
                victory_points: 0,
                victory_points_threshold: 3_000,
            },
        );
    });

    // 1. Negative / abuse cases: missing pilot, unenlisted pilot, friendly fire, missing structure, destroyed structure
    assert_eq!(
        apply_supply_site_damage(&ctx, 999_999_999, 1, 50.0, sys_id, "SupplyCache".into(), 500_004, 1_000)
            .await
            .unwrap_err()
            .code,
        "NotEnlistedInMilitia"
    );
    assert_eq!(
        apply_supply_site_damage(&ctx, unenlisted, 1, 50.0, sys_id, "SupplyCache".into(), 500_004, 1_000)
            .await
            .unwrap_err()
            .code,
        "NotEnlistedInMilitia"
    );
    assert_eq!(
        apply_supply_site_damage(&ctx, cal_pilot, 1, 50.0, sys_id, "SupplyCache".into(), 500_001, 1_000)
            .await
            .unwrap_err()
            .code,
        "CannotAttackFriendlyStructure"
    );
    assert_eq!(
        apply_supply_site_damage(&ctx, cal_pilot, 777_888, 50.0, sys_id, "SupplyCache".into(), 500_004, 1_000)
            .await
            .unwrap_err()
            .code,
        "StructureNotFound"
    );

    // 2. Zero and negative damage boundary: does NOT update last_damage_unix or reduce HP;
    //    positive damage updates last_damage_unix and spills shield -> armor -> hull;
    //    exact lethal hull_hp == 0.0 triggers destruction!
    let depot_sid = 700_001_i64;
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_h
        .spawn_ball(Ball {
            id: depot_sid,
            type_id: 354_772,
            position: DVec3::new(1_500.0, -2_500.0, 500.0),
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 0.0,
            mass: 10_000.0,
            radius: 150.0,
            speed_fraction: 0.0,
            mode: MovementMode::Rigid,
        })
        .await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.fw_runtime
            .supply_sites
            .insert(depot_sid, (sys_id, "SupplyDepot".into(), 500_004));
        snap.structures.insert(
            depot_sid,
            UpwellStructureRecord {
                structure_id: depot_sid,
                solar_system_id: sys_id,
                type_id: 354_772,
                owner_corp_id: 1_000_181,
                state: "ShieldVulnerable".into(),
                position: [1_500.0, -2_500.0, 500.0],
                shield_hp: 100.0,
                armor_hp: 100.0,
                hull_hp: 100.0,
                last_damage_unix: 42,
                ..Default::default()
            },
        );
    });

    for non_pos in [0.0, -75.0] {
        let r = apply_supply_site_damage(
            &ctx,
            cal_pilot,
            depot_sid,
            non_pos,
            sys_id,
            "SupplyDepot".into(),
            500_004,
            9_999,
        )
        .await
        .unwrap();
        assert_eq!(r.dict_get("shieldHP"), Some(&PyRep::Real(100.0)));
        assert_eq!(
            ctx.db.with_snapshot(|s| s.structures[&depot_sid].last_damage_unix),
            42
        );
    }

    // Spill 250.0 across shield(100) + armor(100) + hull(50 -> 50 remaining), updates last_damage_unix = 5_000
    let mid = apply_supply_site_damage(
        &ctx,
        cal_pilot,
        depot_sid,
        250.0,
        sys_id,
        "SupplyDepot".into(),
        500_004,
        5_000,
    )
    .await
    .unwrap();
    assert_eq!(mid.dict_get("state").and_then(|v| v.as_str()), Some("ShieldVulnerable"));
    assert_eq!(mid.dict_get("shieldHP"), Some(&PyRep::Real(0.0)));
    assert_eq!(mid.dict_get("armorHP"), Some(&PyRep::Real(0.0)));
    assert_eq!(mid.dict_get("hullHP"), Some(&PyRep::Real(50.0)));
    assert_eq!(
        ctx.db.with_snapshot(|s| s.structures[&depot_sid].last_damage_unix),
        5_000
    );

    // Exact 50.0 remaining hull damage -> destroys SupplyDepot (defender Caldari kills enemy Gallente site: +5.0 occ advantage)
    let dead = apply_supply_site_damage(
        &ctx,
        cal_pilot,
        depot_sid,
        50.0,
        sys_id,
        "SupplyDepot".into(),
        500_004,
        5_010,
    )
    .await
    .unwrap();
    assert_eq!(dead.dict_get("state").and_then(|v| v.as_str()), Some("Destroyed"));
    assert_eq!(dead.dict_get("friendlyAdvantage"), Some(&PyRep::Real(5.0)));
    assert_eq!(dead.dict_get("enemyAdvantage"), Some(&PyRep::Real(0.0)));
    let can_id = dead.dict_get("lootContainerID").and_then(|v| v.as_i64()).unwrap();
    assert!(sys_h.get_ball_snapshot(depot_sid).await.unwrap().is_none());
    assert!(sys_h.get_ball_snapshot(can_id).await.unwrap().is_some());

    ctx.db.with_snapshot(|snap| {
        assert!(!snap.fw_runtime.supply_sites.contains_key(&depot_sid));
        assert_eq!(snap.items[&can_id].type_id, 23);
        assert_eq!(snap.item_positions[&can_id], [1_500.0, -2_500.0, 500.0]);
        let insig = snap
            .items
            .values()
            .find(|i| i.location_id == can_id && i.type_id == 15_587)
            .expect("Gallente insignia");
        let sup = snap
            .items
            .values()
            .find(|i| i.location_id == can_id && i.type_id == 88_103)
            .expect("supply item");
        assert_eq!(insig.quantity, 5);
        assert_eq!(sup.quantity, 10);
    });

    // Attacking already-destroyed structure returns StructureDestroyed
    assert_eq!(
        apply_supply_site_damage(&ctx, cal_pilot, depot_sid, 10.0, sys_id, "SupplyDepot".into(), 500_004, 5_020)
            .await
            .unwrap_err()
            .code,
        "StructureDestroyed"
    );

    // 3. Test ListeningPost (+5.0), Propaganda (+4.0), and SupplyCache (+2.0) across attacker (is_defender == false),
    //    third-party militia hitting opposing site (fw_opposing_faction(occ_fac) == Some(site_fac) -> is_defender == true),
    //    and unoccupied system fallback!
    let unocc_sys = sys_id + 77;
    for (sid, s_sys, kind, s_fac, att, exp_friendly, exp_enemy, exp_insig_tid) in [
        // Attacker Gallente destroys occupier Caldari Propaganda in sys_id -> is_defender == false (+4.0 att_adv)
        (700_010_i64, sys_id, "Propaganda", 500_001_i64, gal_pilot, 4.0_f64, 5.0_f64, 15_596_u32),
        // Minmatar (500_002 != occ 500_001) destroys Gallente (500_004 == opposing(500_001)) ListeningPost -> is_defender == true (+5.0 occ_adv -> 10.0)
        (700_011, sys_id, "ListeningPost", 500_004, min_pilot, 10.0, 4.0, 15_587),
        // Unoccupied system (occ_fac falls back to site_fac 500_003 Amarr) attacked by Minmatar -> is_defender == false (+2.0 att_adv)
        (700_012, unocc_sys, "SupplyCache", 500_003, min_pilot, 2.0, 0.0, 15_607),
    ] {
        ctx.db.with_mut_snapshot(|snap| {
            snap.fw_runtime.supply_sites.insert(sid, (s_sys, kind.into(), s_fac));
            snap.structures.insert(
                sid,
                UpwellStructureRecord {
                    structure_id: sid,
                    solar_system_id: s_sys,
                    type_id: 93_877,
                    owner_corp_id: 1_000_180,
                    state: "ShieldVulnerable".into(),
                    shield_hp: 50.0,
                    armor_hp: 50.0,
                    hull_hp: 50.0,
                    ..Default::default()
                },
            );
        });
        let res = apply_supply_site_damage(&ctx, att, sid, 200.0, s_sys, kind.into(), s_fac, 6_000)
            .await
            .unwrap();
        assert_eq!(res.dict_get("friendlyAdvantage"), Some(&PyRep::Real(exp_friendly)));
        assert_eq!(res.dict_get("enemyAdvantage"), Some(&PyRep::Real(exp_enemy)));
        let cid = res.dict_get("lootContainerID").and_then(|v| v.as_i64()).unwrap();
        ctx.db.with_snapshot(|snap| {
            let insig = snap
                .items
                .values()
                .find(|i| i.location_id == cid && i.type_id == exp_insig_tid)
                .unwrap();
            let sup = snap
                .items
                .values()
                .find(|i| i.location_id == cid && i.type_id == 88_103)
                .unwrap();
            assert_eq!(insig.quantity, 2);
            assert_eq!(sup.quantity, 5);
        });
    }
}
