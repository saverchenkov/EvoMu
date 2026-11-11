//! Semantic unit tests split from `station_structure_extra_unit_tests`.

#![forbid(unsafe_code)]

use super::station_structure_extra_unit_tests::*;

use super::station_structure::*;
use crate::services::fleet_social::fleet_social_fleet_ops::fleet_social_fleet_ops_tests::{
    clone_char_with, test_ctx_with_clock,
};
use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, MovementMode};
use eve_proto::PyRep;
use glam::DVec3;
use std::collections::HashMap;

#[tokio::test]
async fn pos_mgr_anchor_structure_restriction_and_warp_disruptor_container_paths() {
    let ctx = test_ctx_with_clock(1_800_000_000);
    let char_id = 920_301_i64;
    let corp_id = 98_200_301_i64;
    let ship_id = 920_302_i64;
    let null_sys = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| s.security_status <= -0.5 && s.solar_system_id < 31_000_000)
        .map(|s| s.solar_system_id)
        .unwrap();
    let jita_sys = 30_000_142_u32;
    let kw = HashMap::new();

    let find_grp = |grp: u32| ctx.sde.types.values().find(|t| t.group_id == grp).unwrap().type_id;
    let tower_tid = find_grp(365);
    let freight_cont_tid = find_grp(649);
    let bubble_tid = find_grp(361);
    let sec_cont_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.group_id == 340 && t.capacity > 0.0)
        .unwrap()
        .type_id;

    let (tower_id, freight_id, trit_id, foreign_id, bubble_id, sec_cont_id) =
        (920_310, 920_311, 920_312, 920_313, 920_314, 920_315);

    ctx.db.with_mut_snapshot(|snap| {
        let ch = clone_char_with(snap, char_id, "AnchorPilot", corp_id, null_sys, ship_id);
        snap.characters.insert(char_id, ch);
        for (id, tid, owner, loc) in [
            (tower_id, tower_tid, char_id, ship_id),
            (freight_id, freight_cont_tid, char_id, ship_id),
            (trit_id, 34, char_id, ship_id),
            (foreign_id, 33_474, 999_999, ship_id),
            (bubble_id, bubble_tid, char_id, ship_id),
            (sec_cont_id, sec_cont_tid, char_id, ship_id),
        ] {
            snap.items.insert(
                id,
                InventoryItemRecord {
                    item_id: id,
                    type_id: tid,
                    owner_id: owner,
                    location_id: loc,
                    flag: InventoryFlag::Cargo,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
        }
        // Grant Omega clone grade and anchoring skills so skill checks pass
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.clone_grade = 2;
        }
        for &(skill_attr, _) in &[
            (182u16, 277u16),
            (183, 278),
            (184, 279),
            (1285, 1286),
            (1289, 1290),
            (1291, 1292),
        ] {
            for tid in [bubble_tid, sec_cont_tid] {
                if let Some(&sk) = ctx.sde.types.get(&tid).and_then(|t| t.default_attributes.get(&skill_attr)) {
                    let rec = eve_db::TrainedSkillRecord {
                        type_id: sk as u32,
                        level: 5,
                        skill_points: 256_000.0,
                    };
                    snap.skills.entry(char_id).or_default().insert(sk as u32, rec);
                }
            }
        }
    });

    let mut sess = make_sess(char_id, corp_id, Some(null_sys), None, None, Some(ship_id));

    // Missing argument -> MissingArgument
    assert_eq!(
        handle(&ctx, &mut sess, "posMgr", "AnchorStructure", &[], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "MissingArgument"
    );

    // Control tower, freight container, non-anchorable item, foreign item
    for (id, expected_code) in [
        (tower_id, "StarbaseRemoved"),
        (freight_id, "CannotAnchorFreightContainer"),
        (trit_id, "ItemNotAnchorable"),
        (foreign_id, "NotItemOwner"),
    ] {
        assert_eq!(
            handle(&ctx, &mut sess, "posMgr", "AnchorStructure", &[PyRep::Integer(id)], &kw)
                .await
                .unwrap()
                .unwrap_err()
                .code,
            expected_code
        );
    }

    // Thera (31_000_005) & Abyssal (32_000_001) & Zarzakh (30_100_000) restrictions
    let mut thera_sess = sess.clone();
    thera_sess.solar_system_id = Some(31_000_005);
    assert_eq!(
        handle(&ctx, &mut thera_sess, "posMgr", "AnchorStructure", &[PyRep::Integer(bubble_id)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "AnchoringDisallowedInTheraOrShattered"
    );

    let mut abyss_sess = sess.clone();
    abyss_sess.solar_system_id = Some(32_000_001);
    assert_eq!(
        handle(&ctx, &mut abyss_sess, "posMgr", "AnchorStructure", &[PyRep::Integer(bubble_id)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "CannotDeployInAbyssalSpace"
    );

    let mut zarzakh_sess = sess.clone();
    zarzakh_sess.solar_system_id = Some(30_100_000);
    assert!(handle(&ctx, &mut zarzakh_sess, "posMgr", "AnchorStructure", &[PyRep::Integer(bubble_id)], &kw)
        .await
        .unwrap()
        .is_err());

    // High-sec restrictions for Mobile Warp Disruptor (361) and Secure Container (340 in >= 0.8)
    let mut hs_sess = sess.clone();
    hs_sess.solar_system_id = Some(jita_sys);
    assert_eq!(
        handle(&ctx, &mut hs_sess, "posMgr", "AnchorStructure", &[PyRep::Integer(bubble_id)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "CantAnchorInEmpire"
    );
    assert_eq!(
        handle(&ctx, &mut hs_sess, "posMgr", "AnchorStructure", &[PyRep::Integer(sec_cont_id)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "CantAnchorInHighSec08"
    );

    // Spawn ship in null_sys and anchor both Mobile Warp Disruptor (with explicit coords) and Secure Container
    let sys_h = ctx.systems.get_or_spawn(null_sys);
    let _ = sys_h
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 587,
            position: DVec3::new(300_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;

    let bubble_rep = handle(
        &ctx,
        &mut sess,
        "posMgr",
        "AnchorStructure",
        &[
            PyRep::Integer(bubble_id),
            PyRep::Tuple(vec![PyRep::Real(300_500.0), PyRep::Real(0.0), PyRep::Real(0.0)]),
        ],
        &kw,
    )
    .await
    .unwrap()
    .expect("anchor mobile warp disruptor in nullsec");
    assert!(bubble_rep.dict_get("bubbleRadius").and_then(|v| v.as_f64()).unwrap_or(0.0) > 0.0);

    // Anchoring already-anchoring bubble again -> StructureAlreadyAnchored
    assert_eq!(
        handle(&ctx, &mut sess, "posMgr", "AnchorStructure", &[PyRep::Integer(bubble_id)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "StructureAlreadyAnchored"
    );

    let cont_rep = handle(&ctx, &mut sess, "posMgr", "AnchorStructure", &[PyRep::Integer(sec_cont_id)], &kw)
        .await
        .unwrap()
        .expect("anchor secure container in nullsec");
    assert_eq!(cont_rep.dict_get("itemID"), Some(&PyRep::Integer(sec_cont_id)));
}

#[tokio::test]
async fn station_owner_item_bits_fulcrum_undock_and_dock_guard_matrix() {
    use crate::services::station_structure_handlers::*;
    use crate::services::station_structure::structure_upwell_ops::{load_ext_state, save_ext_state};
    use eve_db::{corp_roles, CorporationRecord, UpwellStructureRecord};

    let ctx = test_ctx_with_clock(1_800_000_000);
    let kw = HashMap::new();
    let char_id = 921_101_i64;
    let ceo_id = 921_102_i64;
    let corp_id = 98_211_001_i64;
    let ship_id = 921_201_i64;
    let supercap_id = 921_202_i64;
    let mod_id = 921_203_i64;
    let structure_id = 10_000_921_001_i64;
    let anchoring_sid = 10_000_921_002_i64;
    let astra_sid = 10_000_921_003_i64;
    let fort_sid = 10_000_921_004_i64;
    let keep_sid = 10_000_921_005_i64;
    let azbel_sid = 10_000_921_006_i64;
    let jita_sys = 30_000_142_u32;
    let jita_station = 60_003_760_i64;
    let fulcrum_station = 60_015_187_i64;
    let zarzakh_sys = 30_100_000_u32;
    let pochven_station = 60_000_355_i64;

    // 1. GetStationsForOwner matching owner & GetStation/GetStationItemBits with service 13
    let sample_npc = ctx
        .sde
        .stations
        .values()
        .find(|st| {
            npc_station_offers_service(&ctx.sde, st.station_id, 13)
                && !ctx
                    .sde
                    .station_operations
                    .get(&st.operation_id)
                    .is_some_and(|o| o.services.contains(&13))
        })
        .or_else(|| {
            ctx.sde
                .stations
                .values()
                .find(|st| npc_station_offers_service(&ctx.sde, st.station_id, 13))
        })
        .expect("SDE station offering repair service 13")
        .clone();
    let mut sess = make_sess(char_id, corp_id, Some(jita_sys), None, None, Some(ship_id));
    let owners_rep = handle(
        &ctx,
        &mut sess,
        "stationSvc",
        "GetStationsForOwner",
        &[PyRep::Integer(sample_npc.owner_corporation_id)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let PyRep::List(owned_list) = owners_rep else {
        panic!("expected list from GetStationsForOwner");
    };
    assert!(owned_list
        .iter()
        .any(|row| row.dict_get("stationID") == Some(&PyRep::Integer(sample_npc.station_id))));

    let station_rep = handle(
        &ctx,
        &mut sess,
        "stationSvc",
        "GetStation",
        &[PyRep::Integer(sample_npc.station_id)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(matches!(
        station_rep.dict_get("serviceMask"),
        Some(PyRep::Integer(mask)) if (*mask & (1i64 << 12)) != 0
    ));
    let bits_rep = handle(
        &ctx,
        &mut sess,
        "stationSvc",
        "GetStationItemBits",
        &[PyRep::Integer(sample_npc.station_id)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let PyRep::Tuple(bits_t) = bits_rep else {
        panic!("expected tuple from GetStationItemBits");
    };
    assert!(bits_t.len() >= 3);

    // Seed characters, corp, ship, supercap, Bastion module, and Upwell hulls
    let supercap_tid = 23_919_u32;
    let bastion_tid = 33_400_u32;

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = clone_char_with(snap, char_id, "Fulcrum Pilot", corp_id, zarzakh_sys, ship_id);
        ch.user_id = 1;
        ch.security_status = 0.0;
        ch.standings.clear();
        ch.station_id = Some(fulcrum_station);
        ch.war_faction_id = Some(500_010);
        snap.characters.insert(char_id, ch);

        let mut ceo = clone_char_with(snap, ceo_id, "Standing CEO", corp_id, jita_sys, 0);
        ceo.user_id = 1;
        ceo.security_status = 0.0;
        ceo.war_faction_id = None;
        ceo.standings.clear();
        ceo.standings.insert(500_026, -0.5);
        snap.characters.insert(ceo_id, ceo);

        let mut corp = CorporationRecord::new(corp_id, "Test Handler Corp", "THC", ceo_id);
        corp.members.insert(char_id);
        corp.members.insert(ceo_id);
        snap.corporations.insert(corp_id, corp);

        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: char_id,
                location_id: fulcrum_station,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            supercap_id,
            InventoryItemRecord {
                item_id: supercap_id,
                type_id: supercap_tid,
                owner_id: char_id,
                location_id: jita_sys as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            mod_id,
            InventoryItemRecord {
                item_id: mod_id,
                type_id: bastion_tid,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );

        let mk_st = |sid: i64, tid: u32, state: &str| UpwellStructureRecord {
            structure_id: sid,
            type_id: tid,
            owner_corp_id: corp_id,
            solar_system_id: jita_sys,
            name: format!("Upwell-{sid}"),
            position: [1_000_000.0, 0.0, 0.0],
            state: state.into(),
            quantum_core_installed: true,
            fuel_blocks: 500,
            shield_hp: 100_000.0,
            armor_hp: 100_000.0,
            hull_hp: 100_000.0,
            reinforced_until_unix: 0,
            last_damage_tick: 0,
            last_damage_unix: 0,
            self_destruct_at_unix: None,
        };
        snap.structures.insert(structure_id, mk_st(structure_id, 35_833, "ShieldVulnerable"));
        let mut anc = mk_st(anchoring_sid, 35_832, "Anchoring");
        anc.reinforced_until_unix = ctx.now_unix() + 3_600;
        snap.structures.insert(anchoring_sid, anc);
        snap.structures.insert(astra_sid, mk_st(astra_sid, 35_832, "ShieldVulnerable"));
        snap.structures.insert(fort_sid, mk_st(fort_sid, 35_833, "ShieldVulnerable"));
        snap.structures.insert(keep_sid, mk_st(keep_sid, 35_834, "ShieldVulnerable"));
        snap.structures.insert(azbel_sid, mk_st(azbel_sid, 35_826, "ShieldVulnerable"));
        snap.structures.insert(10_000_921_099, mk_st(10_000_921_099, 35_832, "Destroyed"));

        let mut ext = load_ext_state(snap);
        ext.active_pilot_by_structure.insert(structure_id, char_id);
        save_ext_state(snap, &ext);
    });

    // 2. Fulcrum Zarzakh undock gives pirate militia 0.80 inertia multiplier + structure undock releases controller
    let mut fulcrum_sess = make_sess(char_id, corp_id, Some(zarzakh_sys), Some(fulcrum_station), None, Some(ship_id));
    fulcrum_sess.user_id = Some(1);
    let _ = handle(&ctx, &mut fulcrum_sess, "stationSvc", "Undock", &[], &kw)
        .await
        .unwrap()
        .expect("undock from Fulcrum");
    let z_handle = ctx.systems.get_or_spawn(zarzakh_sys);
    let ship_combat = z_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!((ship_combat.external_inertia_multiplier.unwrap_or(1.0) - 0.80).abs() < 1e-6);

    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.solar_system_id = jita_sys;
        ch.station_id = None;
        ch.structure_id = Some(structure_id);
        snap.items.get_mut(&ship_id).unwrap().location_id = structure_id;
    });
    let mut struct_sess = make_sess(char_id, corp_id, Some(jita_sys), None, Some(structure_id), Some(ship_id));
    struct_sess.user_id = Some(1);
    let _ = handle(&ctx, &mut struct_sess, "stationSvc", "Undock", &[], &kw)
        .await
        .unwrap()
        .expect("undock from Upwell structure releases controller");
    assert!(ctx
        .db
        .with_snapshot(|s| !load_ext_state(s).active_pilot_by_structure.contains_key(&structure_id)));

    // 3. Dock guard matrix (controller ship, session change timer, immobilizing module, NPC supercap, Fulcrum militia, outlaw, nav lock, Pochven standing)
    let j_handle = ctx.systems.get_or_spawn(jita_sys);
    let jita_st_pos = ctx.sde.stations[&jita_station].position;
    j_handle
        .spawn_ball_with_meta(
            Ball {
                id: ship_id,
                type_id: 587,
                position: jita_st_pos,
                velocity: DVec3::ZERO,
                max_velocity: 350.0,
                inertia: 2.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            Default::default(),
            Default::default(),
        )
        .await
        .unwrap();
    j_handle
        .spawn_ball_with_meta(
            Ball {
                id: supercap_id,
                type_id: supercap_tid,
                position: jita_st_pos,
                velocity: DVec3::ZERO,
                max_velocity: 100.0,
                inertia: 10.0,
                mass: 1_000_000_000.0,
                radius: 1_000.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            Default::default(),
            Default::default(),
        )
        .await
        .unwrap();

    // 3a. Controlling structure blocks dock
    let mut ctrl_sess = make_sess(char_id, corp_id, Some(jita_sys), None, None, Some(structure_id));
    assert_eq!(
        handle(&ctx, &mut ctrl_sess, "stationSvc", "Dock", &[PyRep::Integer(jita_station)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "StructureCannotNavigate"
    );

    // 3b. Active session change timer blocks dock
    let mut dock_sess = make_sess(char_id, corp_id, Some(jita_sys), None, None, Some(ship_id));
    crate::services::space_destiny::record_session_change_timer(&ctx, char_id, ctx.now_unix(), jita_sys, 1, "test_gate");
    assert_eq!(
        handle(&ctx, &mut dock_sess, "stationSvc", "Dock", &[PyRep::Integer(jita_station)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "SessionChangeInProgress"
    );
    crate::services::space_destiny::clear_session_change_timer(&ctx, char_id);

    // 3c. Active Bastion module blocks dock
    ctx.db.with_mut_snapshot(|s| {
        s.module_active_states.insert(mod_id);
    });
    assert!(
        handle(&ctx, &mut dock_sess, "stationSvc", "Dock", &[PyRep::Integer(jita_station)], &kw)
            .await
            .unwrap()
            .is_err()
    );
    ctx.db.with_mut_snapshot(|s| {
        s.module_active_states.remove(&mod_id);
    });

    // 3d. Supercapital cannot dock at NPC station
    let mut cap_sess = make_sess(char_id, corp_id, Some(jita_sys), None, None, Some(supercap_id));
    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&char_id).unwrap().active_ship_id = supercap_id;
    });
    assert!(
        handle(&ctx, &mut cap_sess, "stationSvc", "Dock", &[PyRep::Integer(jita_station)], &kw)
            .await
            .unwrap()
            .is_err()
    );
    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&char_id).unwrap().active_ship_id = ship_id;
    });

    // 3e. Empire militia refused at Fulcrum
    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&char_id).unwrap().war_faction_id = Some(500_001);
    });
    assert_eq!(
        handle(&ctx, &mut dock_sess, "stationSvc", "Dock", &[PyRep::Integer(fulcrum_station)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "FulcrumRefusesEmpireMilitia"
    );
    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&char_id).unwrap().war_faction_id = None;
    });

    // 3f. Outlaw pilot refused in highsec Jita
    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&char_id).unwrap().security_status = -10.0;
    });
    assert_eq!(
        handle(&ctx, &mut dock_sess, "stationSvc", "Dock", &[PyRep::Integer(jita_station)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "FactionPoliceRefusedDocking"
    );
    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&char_id).unwrap().security_status = 0.0;
    });

    // 3g. Disruptive Lance nav lock blocks dock
    ctx.tactical_state
        .write()
        .unwrap()
        .lance_victim_debuff_until
        .insert((ctx.instance_id as usize, ship_id), ctx.now_unix() + 60);
    assert_eq!(
        handle(&ctx, &mut dock_sess, "stationSvc", "Dock", &[PyRep::Integer(jita_station)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "DisruptiveLanceDebuffActive"
    );
    ctx.tactical_state.write().unwrap().lance_victim_debuff_until.clear();

    // 3h. Pochven station docking FSD standing gate
    let pochven_st = &ctx.sde.stations[&pochven_station];
    let pochven_sys = pochven_st.solar_system_id;
    let pochven_pos = pochven_st.position;
    let p_handle = ctx.systems.get_or_spawn(pochven_sys);
    p_handle
        .spawn_ball_with_meta(
            Ball {
                id: ship_id,
                type_id: 587,
                position: pochven_pos,
                velocity: DVec3::ZERO,
                max_velocity: 350.0,
                inertia: 2.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            Default::default(),
            Default::default(),
        )
        .await
        .unwrap();
    ctx.db.with_mut_snapshot(|s| {
        let ch = s.characters.get_mut(&char_id).unwrap();
        ch.standings.insert(500_026, -0.5);
        ch.solar_system_id = pochven_sys;
    });
    let mut p_dock_sess = make_sess(char_id, corp_id, Some(pochven_sys), None, None, Some(ship_id));
    assert_eq!(
        handle(&ctx, &mut p_dock_sess, "stationSvc", "Dock", &[PyRep::Integer(pochven_station)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "StandingTooLowForStationService"
    );

    // 4. station_structure_handlers direct gates + resolve_upwell_dock_target + FSD standing restrictions
    assert_eq!(
        resolve_upwell_dock_target(&ctx, anchoring_sid, jita_sys, char_id, 587, 25).unwrap_err().code,
        "StructureNotDockable"
    );
    assert!(resolve_upwell_dock_target(&ctx, keep_sid, jita_sys, char_id, supercap_tid, 659).is_ok());
    assert!(resolve_upwell_dock_target(&ctx, fort_sid, jita_sys, char_id, supercap_tid, 659).is_err());
    assert!(resolve_upwell_dock_target(&ctx, azbel_sid, jita_sys, char_id, supercap_tid, 659).is_err());
    assert!(resolve_upwell_dock_target(&ctx, astra_sid, jita_sys, char_id, 587, 25).is_ok());

    assert_eq!(
        check_station_service_standing(&ctx, char_id, pochven_station, 24, 24, 1_000_000, Some(500_026))
            .unwrap_err()
            .code,
        "StandingTooLowForStationService"
    );
    assert!(check_station_service_standing(&ctx, char_id, pochven_station, 999, 999, 1_000_000, Some(500_026)).is_ok());
    assert_eq!(
        validate_npc_station_docking_gate(&ctx, char_id, pochven_station).unwrap_err().code,
        "StandingTooLowForStationService"
    );
    assert_eq!(
        evaluate_station_service_standing_gate(&ctx, char_id, jita_station, 1 << 30)
            .unwrap_err()
            .code,
        "StationServiceUnavailable"
    );
    assert_eq!(
        evaluate_station_service_standing_gate(&ctx, char_id, pochven_station, 23)
            .unwrap_err()
            .code,
        "StandingTooLowForStationService"
    );
    ctx.db.with_mut_snapshot(|s| {
        let ch = s.characters.get_mut(&char_id).unwrap();
        ch.standings.insert(500_026, 4.0);
        ch.war_faction_id = Some(500_010);
        s.characters.get_mut(&ceo_id).unwrap().standings.insert(500_026, 4.0);
        s.fw_runtime.direct_enlisted_chars.insert(char_id, 500_010);
    });
    assert!(check_station_service_standing(&ctx, char_id, pochven_station, 24, 24, 1_000_000, Some(500_026)).is_ok());
    assert!(validate_npc_station_docking_gate(&ctx, char_id, pochven_station).is_ok());
    assert_eq!(
        evaluate_station_service_standing_gate(&ctx, char_id, pochven_station, 23).unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(
        evaluate_station_service_standing_gate(&ctx, char_id, fulcrum_station, 23)
            .unwrap_err()
            .code,
        "StandingTooLowForStationService"
    );
    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&ceo_id).unwrap().war_faction_id = Some(500_010);
    });
    assert_eq!(
        evaluate_station_service_standing_gate(&ctx, char_id, fulcrum_station, 23)
            .expect("fulcrum service 23 gate"),
        PyRep::Bool(true)
    );

    // Hull class matrix: Fortizar (35833 Large Citadel) allows subcaps; Sotiyo (35827 XLarge EC) allows subcaps but denies supercaps; Upwell station gate
    let sotiyo_sid = 10_000_921_110_i64;
    ctx.db.with_mut_snapshot(|snap| {
        let mut sot = snap.structures[&fort_sid].clone();
        sot.structure_id = sotiyo_sid;
        sot.type_id = 35_827;
        snap.structures.insert(sotiyo_sid, sot);
    });
    assert!(resolve_upwell_dock_target(&ctx, fort_sid, jita_sys, char_id, 587, 25).is_ok());
    assert!(resolve_upwell_dock_target(&ctx, sotiyo_sid, jita_sys, char_id, 587, 25).is_ok());
    assert_eq!(
        resolve_upwell_dock_target(&ctx, sotiyo_sid, jita_sys, char_id, supercap_tid, 659)
            .unwrap_err()
            .code,
        "ShipTooLargeForStructure"
    );
    assert!(evaluate_station_service_standing_gate(&ctx, char_id, astra_sid, 1).is_ok());

    // GetStructureInfo access denial for outsider out-of-system + directory skipping destroyed
    let mut outsider_sess = make_sess(999_888, 98_999_888, Some(zarzakh_sys), None, None, None);
    assert_eq!(
        handle_get_structure_info(&ctx, &mut outsider_sess, &[PyRep::Integer(astra_sid)]).unwrap_err().code,
        "StructureAccessDenied"
    );
    let _dir_res = handle_get_structures(&ctx, &mut dock_sess, false).unwrap();
    let _ = corp_roles::ROLE_DIRECTOR;
}

#[tokio::test]
async fn pos_mgr_anchor_deployable_matrix_dispatch_and_orbital_handlers_cov() {
    use crate::services::mobile_deployables::{
        GROUP_CRAB_BEACON, GROUP_MOBILE_CYNO_BEACON, GROUP_MOBILE_CYNO_INHIBITOR,
        GROUP_MOBILE_OBSERVATORY, GROUP_MOBILE_TRACTOR,
    };
    use crate::services::station_structure_handlers::*;
    use eve_db::{CorporationRecord, TrainedSkillRecord, UpwellStructureRecord};

    let ctx = test_ctx_with_clock(1_800_000_000);
    let kw = HashMap::new();
    let char_id = 922_101_i64;
    let member_id = 922_102_i64;
    let corp_id = 98_222_001_i64;
    let ship_id = 922_201_i64;
    let jita_sys = 30_000_142_u32;
    let null_sys = ctx
        .sde
        .solar_systems
        .values()
        .filter(|s| {
            s.security_status <= -0.5
                && s.solar_system_id < 30_100_000
                && s.region_id != 10_000_070
                && ctx
                    .sde
                    .celestials_in_system(s.solar_system_id)
                    .iter()
                    .any(|c| c.group_id == 7)
        })
        .min_by_key(|s| s.solar_system_id)
        .map(|s| s.solar_system_id)
        .unwrap();
    let null_planet_pos = ctx
        .sde
        .celestials_in_system(null_sys)
        .into_iter()
        .filter(|c| c.group_id == 7)
        .min_by_key(|c| c.celestial_id)
        .expect("nullsec planet")
        .clone();

    let find_grp = |grp: u32| {
        ctx.sde
            .types
            .values()
            .filter(|t| t.group_id == grp && t.published)
            .min_by_key(|t| t.type_id)
            .or_else(|| {
                ctx.sde
                    .types
                    .values()
                    .filter(|t| t.group_id == grp)
                    .min_by_key(|t| t.type_id)
            })
            .map(|t| t.type_id)
    };
    let mtu_tid = find_grp(GROUP_MOBILE_TRACTOR).unwrap();
    let crab_tid = find_grp(GROUP_CRAB_BEACON).unwrap();
    let obs_tid = find_grp(GROUP_MOBILE_OBSERVATORY).unwrap();
    let cyno_inh_tid = find_grp(GROUP_MOBILE_CYNO_INHIBITOR).unwrap();
    let cyno_bea_tid = find_grp(GROUP_MOBILE_CYNO_BEACON).unwrap();

    let (
        unknown_id,
        bad_loc_id,
        mtu_id,
        crab_id,
        obs_id,
        inh_id,
        bea_id,
        den_item_id,
        locked_item_id,
        gantry_item_id,
        up_item_id,
        bad_up_item_id,
        locked_up_item_id,
        existing_orbital_id,
    ) = (
        922_301_i64,
        922_302_i64,
        922_303_i64,
        922_304_i64,
        922_305_i64,
        922_306_i64,
        922_307_i64,
        922_308_i64,
        922_309_i64,
        922_310_i64,
        922_311_i64,
        922_312_i64,
        922_313_i64,
        922_314_i64,
    );

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = clone_char_with(snap, char_id, "Deployer", corp_id, null_sys, ship_id);
        ch.clone_grade = 2;
        snap.characters.insert(char_id, ch);
        let mem = clone_char_with(snap, member_id, "Grumpy Member", corp_id, null_sys, ship_id);
        snap.characters.insert(member_id, mem);
        let mut corp = CorporationRecord::new(corp_id, "Orbital Corp", "ORBC", char_id);
        corp.members.insert(char_id);
        corp.members.insert(member_id);
        snap.corporations.insert(corp_id, corp);

        for (iid, tid, owner, loc, flag) in [
            (unknown_id, 999_888_777, char_id, ship_id, InventoryFlag::Cargo),
            (bad_loc_id, mtu_tid, char_id, 60_003_760, InventoryFlag::Hangar),
            (mtu_id, mtu_tid, char_id, ship_id, InventoryFlag::Cargo),
            (crab_id, crab_tid, char_id, ship_id, InventoryFlag::Cargo),
            (obs_id, obs_tid, char_id, ship_id, InventoryFlag::Cargo),
            (inh_id, cyno_inh_tid, char_id, ship_id, InventoryFlag::Cargo),
            (bea_id, cyno_bea_tid, char_id, ship_id, InventoryFlag::Cargo),
            (den_item_id, crate::services::sov_gameplay::TYPE_MERCENARY_DEN, char_id, ship_id, InventoryFlag::Cargo),
            (locked_item_id, mtu_tid, char_id, ship_id, InventoryFlag::Locked),
            (gantry_item_id, 3962, char_id, ship_id, InventoryFlag::Cargo),
            (up_item_id, 2233, char_id, ship_id, InventoryFlag::Cargo),
            (bad_up_item_id, 34, char_id, ship_id, InventoryFlag::Cargo),
            (locked_up_item_id, 2233, char_id, ship_id, InventoryFlag::Locked),
        ] {
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: tid,
                    owner_id: owner,
                    location_id: loc,
                    flag,
                    quantity: 1,
                    singleton: false,
                    version: 1,
                    ..Default::default()
                },
            );
        }
    });

    // AnchorStructure early guards
    let mut sess = make_sess(char_id, corp_id, Some(null_sys), None, None, Some(ship_id));
    sess.user_id = Some(1);
    assert!(handle(&ctx, &mut sess, "posMgr", "AnchorStructure", &[PyRep::Integer(crate::services::sov_gameplay::TYPE_MERCENARY_DEN as i64)], &kw).await.unwrap().is_err());
    assert!(handle(&ctx, &mut sess, "posMgr", "AnchorStructure", &[PyRep::Integer(den_item_id)], &kw).await.unwrap().is_err());
    assert_eq!(
        handle(&ctx, &mut sess, "posMgr", "AnchorStructure", &[PyRep::Integer(unknown_id)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "StarbaseRemoved"
    );
    let mut no_sys = make_sess(char_id, corp_id, None, None, None, Some(ship_id));
    assert_eq!(
        handle(&ctx, &mut no_sys, "posMgr", "AnchorStructure", &[PyRep::Integer(mtu_id)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "NoSolarSystem"
    );
    let mut no_ship = make_sess(char_id, corp_id, Some(null_sys), None, None, None);
    assert_eq!(
        handle(&ctx, &mut no_ship, "posMgr", "AnchorStructure", &[PyRep::Integer(mtu_id)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "NoActiveShip"
    );
    assert_eq!(
        handle(&ctx, &mut sess, "posMgr", "AnchorStructure", &[PyRep::Integer(bad_loc_id)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidItemLocation"
    );

    // Abyssal system guard
    let mut abyss_sess = make_sess(char_id, corp_id, Some(32_000_001), None, None, Some(ship_id));
    let abyss_h = ctx.systems.get_or_spawn(32_000_001);
    abyss_h
        .spawn_ball_with_meta(
            Ball {
                id: ship_id,
                type_id: 587,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 350.0,
                inertia: 2.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            Default::default(),
            Default::default(),
        )
        .await
        .unwrap();
    // Grant all anchoring skills for null_sys vs abyss test
    if let Some(req_sk) = ctx
        .sde
        .types
        .get(&mtu_tid)
        .and_then(|t| t.default_attributes.get(&182).copied())
        .map(|v| v as u32)
    {
        if req_sk > 0 {
            assert_eq!(
                handle(&ctx, &mut abyss_sess, "posMgr", "AnchorStructure", &[PyRep::Integer(mtu_id)], &kw)
                    .await
                    .unwrap()
                    .unwrap_err()
                    .code,
                "SkillMissing"
            );
            ctx.db.with_mut_snapshot(|s| {
                s.skills.entry(char_id).or_default().insert(
                    req_sk,
                    TrainedSkillRecord { type_id: req_sk, level: 5, skill_points: 256_000.0 },
                );
            });
        }
    }
    assert_eq!(
        handle(&ctx, &mut abyss_sess, "posMgr", "AnchorStructure", &[PyRep::Integer(mtu_id)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "CannotDeployInAbyssalSpace"
    );

    // Grant skills for all deployables before checking highsec cyno inhibitor location guard
    ctx.db.with_mut_snapshot(|s| {
        let m = s.skills.entry(char_id).or_default();
        for tid in [mtu_tid, crab_tid, obs_tid, cyno_inh_tid, cyno_bea_tid] {
            if let Some(t) = ctx.sde.types.get(&tid) {
                for &(sid_attr, lvl_attr) in &[
                    (182, 277),
                    (183, 278),
                    (184, 279),
                    (1285, 1286),
                    (1289, 1290),
                    (1291, 1292),
                ] {
                    if let Some(sid) = t.default_attributes.get(&sid_attr).copied().map(|v| v as u32) {
                        if sid > 0 {
                            let lvl = t.default_attributes.get(&lvl_attr).copied().unwrap_or(1.0) as u8;
                            m.insert(
                                sid,
                                TrainedSkillRecord { type_id: sid, level: lvl.max(5), skill_points: 256_000.0 },
                            );
                        }
                    }
                }
            }
        }
    });

    // Highsec Cyno Inhibitor guard
    let mut hs_sess = make_sess(char_id, corp_id, Some(jita_sys), None, None, Some(ship_id));
    assert_eq!(
        handle(&ctx, &mut hs_sess, "posMgr", "AnchorStructure", &[PyRep::Integer(inh_id)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "CantAnchorInHighSecOrWormhole"
    );

    // ShipNotInSpace before spawning ball in null_sys
    assert_eq!(
        handle(&ctx, &mut sess, "posMgr", "AnchorStructure", &[PyRep::Integer(mtu_id)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "ShipNotInSpace"
    );

    let null_h = ctx.systems.get_or_spawn(null_sys);
    null_h
        .spawn_ball_with_meta(
            Ball {
                id: ship_id,
                type_id: 587,
                position: null_planet_pos.position + DVec3::new(null_planet_pos.radius + 1_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 350.0,
                inertia: 2.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            Default::default(),
            Default::default(),
        )
        .await
        .unwrap();

    assert_eq!(
        handle(&ctx, &mut sess, "posMgr", "AnchorStructure", &[PyRep::Integer(locked_item_id)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "ItemLocked"
    );

    sess.fleet_id = Some(42_001);
    for dep_id in [mtu_id, obs_id, inh_id, bea_id] {
        assert!(
            handle(&ctx, &mut sess, "posMgr", "AnchorStructure", &[PyRep::Integer(dep_id)], &kw)
                .await
                .unwrap()
                .is_ok(),
            "deployable {dep_id} must anchor"
        );
    }
    let _ = handle(&ctx, &mut sess, "posMgr", "AnchorStructure", &[PyRep::Integer(crab_id)], &kw).await;

    // Dispatch arms at bottom of station_structure::handle
    assert!(handle(&ctx, &mut sess, "posMgr", "OnlineOrbital", &[], &kw).await.unwrap().is_err());
    assert!(handle(&ctx, &mut sess, "structure", "JettisonStructureFuel", &[], &kw).await.unwrap().is_err());
    assert!(handle(&ctx, &mut sess, "structureSettings", "CharacterHasService", &[PyRep::Integer(0), PyRep::Integer(1)], &kw).await.is_some());
    assert!(handle(&ctx, &mut sess, "structureProfiles", "GetProfiles", &[], &kw).await.is_some());

    // handle_anchor_orbital & handle_upgrade_gantry comprehensive matrices
    let docked_sess = make_sess(char_id, corp_id, Some(null_sys), Some(60_003_760), None, Some(ship_id));
    assert_eq!(handle_anchor_orbital(&ctx, &no_sys, &[PyRep::Integer(gantry_item_id)]).unwrap_err().code, "NoSolarSystem");
    let thera_sess = make_sess(char_id, corp_id, Some(31_000_005), None, None, Some(ship_id));
    assert_eq!(handle_anchor_orbital(&ctx, &thera_sess, &[PyRep::Integer(gantry_item_id)]).unwrap_err().code, "AnchoringDisallowedInTheraOrShattered");
    assert_eq!(handle_anchor_orbital(&ctx, &sess, &[]).unwrap_err().code, "MissingArgument");
    assert_eq!(handle_anchor_orbital(&ctx, &docked_sess, &[PyRep::Integer(gantry_item_id)]).unwrap_err().code, "NotInSpace");
    let npc_corp_sess = make_sess(char_id, 1_000_002, Some(null_sys), None, None, Some(ship_id));
    ctx.db.with_mut_snapshot(|s| s.characters.get_mut(&char_id).unwrap().corporation_id = 1_000_002);
    assert_eq!(handle_anchor_orbital(&ctx, &npc_corp_sess, &[PyRep::Integer(gantry_item_id)]).unwrap_err().code, "CrpAccessDenied");
    ctx.db.with_mut_snapshot(|s| s.characters.get_mut(&char_id).unwrap().corporation_id = corp_id);
    assert_eq!(handle_anchor_orbital(&ctx, &no_ship, &[PyRep::Integer(gantry_item_id)]).unwrap_err().code, "NoActiveShip");
    assert_eq!(handle_anchor_orbital(&ctx, &sess, &[PyRep::Integer(gantry_item_id), PyRep::Integer(999_999)]).unwrap_err().code, "InvalidOrbitalPlanet");
    assert_eq!(handle_anchor_orbital(&ctx, &sess, &[PyRep::Integer(888_777_666)]).unwrap_err().code, "StarbaseRemoved");
    let foreign_gantry_id = 922_399_i64;
    ctx.db.with_mut_snapshot(|s| {
        s.items.insert(
            foreign_gantry_id,
            InventoryItemRecord {
                item_id: foreign_gantry_id,
                type_id: 3962,
                owner_id: member_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });
    assert_eq!(handle_anchor_orbital(&ctx, &sess, &[PyRep::Integer(foreign_gantry_id)]).unwrap_err().code, "NotItemOwner");
    assert_eq!(handle_anchor_orbital(&ctx, &sess, &[PyRep::Integer(bad_loc_id)]).unwrap_err().code, "StarbaseRemoved");

    // Additional handle_anchor_orbital & handle_upgrade_gantry edge guards (ShipNotInSpace, CrpAccessDenied char_corp<=0, OrbitalOutOfRange)
    let unspawned_sess = make_sess(char_id, corp_id, Some(null_sys), None, None, Some(999_555));
    assert_eq!(handle_anchor_orbital(&ctx, &unspawned_sess, &[PyRep::Integer(gantry_item_id)]).unwrap_err().code, "ShipNotInSpace");
    assert_eq!(handle_upgrade_gantry(&ctx, &unspawned_sess, &[PyRep::Integer(gantry_item_id)]).unwrap_err().code, "ShipNotInSpace");
    let no_corp_id = 922_199_i64;
    ctx.db.with_mut_snapshot(|s| {
        let mut ch = clone_char_with(s, no_corp_id, "NoCorp", 0, null_sys, ship_id);
        ch.station_id = None;
        ch.structure_id = None;
        s.characters.insert(no_corp_id, ch);
    });
    let mut no_corp_sess = make_sess(no_corp_id, 0, Some(null_sys), None, None, Some(ship_id));
    no_corp_sess.corporation_id = None;
    assert_eq!(handle_upgrade_gantry(&ctx, &no_corp_sess, &[PyRep::Integer(gantry_item_id)]).unwrap_err().code, "CrpAccessDenied");

    let far_ship_id = 922_299_i64;
    let far_gantry_id = 922_395_i64;
    ctx.db.with_mut_snapshot(|s| {
        s.items.insert(
            far_gantry_id,
            InventoryItemRecord {
                item_id: far_gantry_id,
                type_id: 3962,
                owner_id: char_id,
                location_id: far_ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });
    let sys_h2 = ctx.systems.get_or_spawn(null_sys);
    sys_h2
        .spawn_ball_with_meta(
            Ball {
                id: far_ship_id,
                type_id: 587,
                position: null_planet_pos.position + DVec3::new(1_000_000_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 350.0,
                inertia: 2.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            Default::default(),
            Default::default(),
        )
        .await
        .unwrap();
    let far_sess = make_sess(char_id, corp_id, Some(null_sys), None, None, Some(far_ship_id));
    assert_eq!(
        handle_anchor_orbital(&ctx, &far_sess, &[PyRep::Integer(far_gantry_id), PyRep::Integer(null_planet_pos.celestial_id)])
            .unwrap_err()
            .code,
        "OrbitalOutOfRange"
    );

    // Auto-nearest planet anchor succeeds with omitted args[1], then AlreadyAnchored + PlanetAlreadyHasOrbital
    assert!(handle_anchor_orbital(&ctx, &sess, &[PyRep::Integer(gantry_item_id)]).is_ok());
    assert_eq!(handle_anchor_orbital(&ctx, &sess, &[PyRep::Integer(gantry_item_id)]).unwrap_err().code, "AlreadyAnchored");

    let extra_gantry_id = 922_398_i64;
    let no_st_gantry_id = 922_396_i64;
    ctx.db.with_mut_snapshot(|s| {
        for (iid, owner, loc, flag) in [
            (extra_gantry_id, char_id, ship_id, InventoryFlag::Cargo),
            (no_st_gantry_id, corp_id, null_sys as i64, InventoryFlag::None),
        ] {
            s.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: 3962,
                    owner_id: owner,
                    location_id: loc,
                    flag,
                    quantity: 1,
                    singleton: iid == no_st_gantry_id,
                    version: 1,
                    ..Default::default()
                },
            );
        }
    });
    assert_eq!(
        handle_anchor_orbital(&ctx, &sess, &[PyRep::Integer(extra_gantry_id), PyRep::Integer(null_planet_pos.celestial_id)])
            .unwrap_err()
            .code,
        "PlanetAlreadyHasOrbital"
    );

    // handle_upgrade_gantry guard matrix
    assert_eq!(handle_upgrade_gantry(&ctx, &docked_sess, &[PyRep::Integer(gantry_item_id)]).unwrap_err().code, "MustBeInSpace");
    assert_eq!(handle_upgrade_gantry(&ctx, &no_sys, &[PyRep::Integer(gantry_item_id)]).unwrap_err().code, "NoSolarSystem");
    assert_eq!(handle_upgrade_gantry(&ctx, &no_ship, &[PyRep::Integer(gantry_item_id)]).unwrap_err().code, "NoActiveShip");
    let mem_sess = make_sess(member_id, corp_id, Some(null_sys), None, None, Some(ship_id));
    assert_eq!(handle_upgrade_gantry(&ctx, &mem_sess, &[PyRep::Integer(gantry_item_id)]).unwrap_err().code, "CrpAccessDenied");
    assert_eq!(handle_upgrade_gantry(&ctx, &sess, &[PyRep::Integer(gantry_item_id), PyRep::Integer(999_888)]).unwrap_err().code, "CustomsOfficeItemRequired");
    assert_eq!(handle_upgrade_gantry(&ctx, &sess, &[PyRep::Integer(gantry_item_id), PyRep::Integer(bad_up_item_id)]).unwrap_err().code, "CustomsOfficeItemRequired");
    assert_eq!(handle_upgrade_gantry(&ctx, &sess, &[PyRep::Integer(gantry_item_id), PyRep::Integer(locked_up_item_id)]).unwrap_err().code, "CustomsOfficeItemRequired");

    ctx.db.with_mut_snapshot(|s| {
        s.structures.insert(
            existing_orbital_id,
            UpwellStructureRecord {
                structure_id: existing_orbital_id,
                type_id: 2233,
                owner_corp_id: 98_999_777,
                solar_system_id: jita_sys,
                name: "Other POCO".into(),
                position: [0.0, 0.0, 0.0],
                state: "Destroyed".into(),
                quantum_core_installed: false,
                fuel_blocks: 0,
                shield_hp: 1.0,
                armor_hp: 1.0,
                hull_hp: 1.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
        s.items.insert(
            existing_orbital_id,
            InventoryItemRecord {
                item_id: existing_orbital_id,
                type_id: 3962,
                owner_id: corp_id,
                location_id: jita_sys as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    assert_eq!(handle_upgrade_gantry(&ctx, &sess, &[PyRep::Integer(existing_orbital_id)]).unwrap_err().code, "OrbitalAlreadyUpgraded");
    ctx.db.with_mut_snapshot(|s| {
        s.structures.get_mut(&existing_orbital_id).unwrap().type_id = 3962;
    });
    assert_eq!(handle_upgrade_gantry(&ctx, &sess, &[PyRep::Integer(existing_orbital_id)]).unwrap_err().code, "OrbitalOwnershipDenied");
    ctx.db.with_mut_snapshot(|s| {
        s.structures.get_mut(&existing_orbital_id).unwrap().owner_corp_id = corp_id;
    });
    assert_eq!(handle_upgrade_gantry(&ctx, &sess, &[PyRep::Integer(existing_orbital_id)]).unwrap_err().code, "OrbitalWrongSystem");
    ctx.db.with_mut_snapshot(|s| {
        s.structures.get_mut(&existing_orbital_id).unwrap().solar_system_id = null_sys;
    });
    assert_eq!(handle_upgrade_gantry(&ctx, &sess, &[PyRep::Integer(existing_orbital_id)]).unwrap_err().code, "InvalidOrbitalState");

    // Remaining handle_upgrade_gantry guards: item ownership, unanchored orbital, range, and missing cargo 2233
    ctx.db.with_mut_snapshot(|s| {
        s.items.get_mut(&existing_orbital_id).unwrap().owner_id = 98_999_777;
    });
    assert_eq!(handle_upgrade_gantry(&ctx, &sess, &[PyRep::Integer(existing_orbital_id)]).unwrap_err().code, "OrbitalOwnershipDenied");
    ctx.db.with_mut_snapshot(|s| {
        s.items.get_mut(&existing_orbital_id).unwrap().owner_id = corp_id;
    });
    assert_eq!(handle_upgrade_gantry(&ctx, &sess, &[PyRep::Integer(no_st_gantry_id)]).unwrap_err().code, "OrbitalNotAnchored");
    assert_eq!(handle_upgrade_gantry(&ctx, &far_sess, &[PyRep::Integer(gantry_item_id)]).unwrap_err().code, "OrbitalOutOfRange");
    ctx.db.with_mut_snapshot(|s| {
        s.items.get_mut(&up_item_id).unwrap().location_id = 60_003_760;
    });
    assert_eq!(handle_upgrade_gantry(&ctx, &sess, &[PyRep::Integer(gantry_item_id)]).unwrap_err().code, "CustomsOfficeItemRequired");
    ctx.db.with_mut_snapshot(|s| {
        s.items.get_mut(&up_item_id).unwrap().location_id = ship_id;
    });

    assert!(handle_upgrade_gantry(&ctx, &sess, &[PyRep::Integer(gantry_item_id), PyRep::Integer(up_item_id)]).is_ok());
}
