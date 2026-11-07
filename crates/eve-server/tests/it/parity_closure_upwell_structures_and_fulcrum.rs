//! Non-GM end-to-end integration tests for Upwell Structures —
//! Anchor Restrictions, Structure Directory, Profiles & Delayed Control Release, Defense Settings,
//! Low-Power Repair Shop Quotes & Restoration, and The Fulcrum Alignment Services in Zarzakh).

#![forbid(unsafe_code)]

use crate::parity_shard_3_closure_helpers::{
    call_rpc, new_sqlite_ctx, reload_ctx_from_sqlite, spawn_player_combat_pilot, PlayerPilot,
};
use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::station_structure::structure_lifecycle::sync_all_structures;
use eve_server::services::station_structure::structure_upwell_ops::{
    load_ext_state, PROFILE_CHANGE_DELAY_SECS,
};
use eve_server::services::structure_acl_defense_rules::load_defense_settings;
use glam::DVec3;

const HURRICANE_TYPE: u32 = 24702;
const ASTRAHUS_TYPE: u32 = 35_832;
const PHAROLUX_TYPE: u32 = 35_841;
const JITA: u32 = 30_000_142;
const PERIMETER: u32 = 30_000_144;
const KISOGO: u32 = 30_001_392;
const ZARZAKH: u32 = 30_100_000;
const FULCRUM_STRUCTURE_ID: i64 = 60_015_187;

async fn setup_corp_director(
    ctx: &ServerContext,
    sess_id: u64,
    user: &str,
    name: &str,
    corp_name: &str,
    ticker: &str,
) -> (PlayerPilot, i64) {
    let mut pilot = spawn_player_combat_pilot(ctx, sess_id, user, name, HURRICANE_TYPE, false).await;
    let sys_id = pilot.sess.solar_system_id.expect("solar_system_id");
    let station_id = ctx
        .sde
        .stations
        .values()
        .find(|s| s.solar_system_id == sys_id)
        .map(|s| s.station_id)
        .expect("station in pilot solar system");
    let st_pos = ctx.sde.stations[&station_id].position;
    ctx.systems
        .get_or_spawn(sys_id)
        .set_ball_position(pilot.ship_id, st_pos)
        .await;
    call_rpc(
        ctx,
        &mut pilot.sess,
        "stationSvc",
        "Dock",
        &[PyRep::Integer(station_id)],
    )
    .await
    .expect("stationSvc.Dock before AddCorporation");
    ctx.advance_secs(31);
    let res = call_rpc(
        ctx,
        &mut pilot.sess,
        "corpRegistry",
        "AddCorporation",
        &[PyRep::String(corp_name.into()), PyRep::String(ticker.into())],
    )
    .await
    .expect("AddCorporation");
    let corp_id = res
        .as_i64()
        .or_else(|| res.dict_get("corporationID").and_then(PyRep::as_i64))
        .unwrap_or_else(|| ctx.db.with_snapshot(|s| s.characters[&pilot.char_id].corporation_id));
    pilot.corp_id = corp_id;
    call_rpc(ctx, &mut pilot.sess, "ship", "Undock", &[])
        .await
        .expect("ship.Undock after corporation setup");
    ctx.advance_secs(31);
    (pilot, corp_id)
}

fn give_cargo_item(ctx: &ServerContext, owner_id: i64, ship_id: i64, type_id: u32) -> i64 {
    ctx.db.with_mut_snapshot(|s| {
        s.next_entity_id += 1;
        let id = s.next_entity_id;
        s.items.insert(
            id,
            InventoryItemRecord {
                item_id: id,
                type_id,
                owner_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                ..Default::default()
            },
        );
        id
    })
}

async fn place_pilot_in_space(ctx: &ServerContext, pilot: &mut PlayerPilot, sys_id: u32, pos: DVec3) {
    ctx.db.with_mut_snapshot(|s| {
        let ch = s.characters.get_mut(&pilot.char_id).unwrap();
        ch.solar_system_id = sys_id;
        ch.station_id = None;
        ch.structure_id = None;
    });
    pilot.sess.solar_system_id = Some(sys_id);
    pilot.sess.station_id = None;
    pilot.sess.structure_id = None;
    let _ = ctx
        .systems
        .get_or_spawn(sys_id)
        .spawn_ball(eve_destiny::Ball {
            id: pilot.ship_id,
            type_id: HURRICANE_TYPE,
            position: pos,
            velocity: DVec3::ZERO,
            max_velocity: 200.0,
            inertia: 3.0,
            mass: 1.5e7,
            radius: 200.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        })
        .await;
}

// parity: feat:structures.anchor-restrictions
// parity: feat:structures.structure-directory
#[tokio::test]
async fn test_e2e_structures_anchor_restrictions_and_directory() {
    eve_testkit::attest(&[
        "feat:structures.anchor-restrictions",
        "feat:structures.structure-directory",
    ]);

    let (db_path, ctx) = new_sqlite_ctx("struct_anchor_dir");
    crate::common::pin_clock(&ctx);

    let (mut pilot, corp_id) =
        setup_corp_director(&ctx, 83_001, "st_dir_p1", "Upwell Architect", "Citadel Corp", "CITC").await;

    // 1. Negative: young corporation (< 7 days) rejected
    let young_item = give_cargo_item(&ctx, pilot.char_id, pilot.ship_id, ASTRAHUS_TYPE);
    let young_err = call_rpc(
        &ctx,
        &mut pilot.sess,
        "structureDeployment",
        "Anchor",
        &[PyRep::Integer(young_item), PyRep::String("Young Corp Citadel".into())],
    )
    .await
    .expect_err("Corp < 7 days rejected");
    assert_eq!(young_err.code, "CorporationTooYoungToAnchor");

    ctx.advance_secs(7 * 86_400 + 60);

    // 2. Negative: trade hub (Jita) and starter system (Kisogo) rejected
    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&pilot.char_id).unwrap().solar_system_id = JITA;
    });
    pilot.sess.solar_system_id = Some(JITA);
    let jita_h = ctx.systems.get_or_spawn(JITA);
    let _ = jita_h
        .spawn_ball(eve_destiny::Ball {
            id: pilot.ship_id,
            type_id: HURRICANE_TYPE,
            position: DVec3::new(120_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 200.0,
            inertia: 3.0,
            mass: 1.5e7,
            radius: 200.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        })
        .await;
    let jita_err = call_rpc(
        &ctx,
        &mut pilot.sess,
        "structureDeployment",
        "Anchor",
        &[PyRep::Integer(young_item), PyRep::String("Jita Hub".into())],
    )
    .await
    .expect_err("Jita trade hub rejected");
    assert_eq!(jita_err.code, "AnchoringDisallowedInTradeHub");

    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&pilot.char_id).unwrap().solar_system_id = KISOGO;
    });
    pilot.sess.solar_system_id = Some(KISOGO);
    let k_h = ctx.systems.get_or_spawn(KISOGO);
    let _ = k_h
        .spawn_ball(eve_destiny::Ball {
            id: pilot.ship_id,
            type_id: HURRICANE_TYPE,
            position: DVec3::new(120_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 200.0,
            inertia: 3.0,
            mass: 1.5e7,
            radius: 200.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        })
        .await;
    let starter_err = call_rpc(
        &ctx,
        &mut pilot.sess,
        "structureDeployment",
        "Anchor",
        &[PyRep::Integer(young_item), PyRep::String("Kisogo Starter".into())],
    )
    .await
    .expect_err("Starter system rejected");
    assert_eq!(starter_err.code, "AnchoringDisallowedInStarterOrCareerSystem");

    // 3. Move to Perimeter: reject Pharolux FLEX in highsec, then validly anchor Astrahus & install Quantum Core!
    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&pilot.char_id).unwrap().solar_system_id = PERIMETER;
    });
    pilot.sess.solar_system_id = Some(PERIMETER);
    let p_h = ctx.systems.get_or_spawn(PERIMETER);
    let _ = p_h
        .spawn_ball(eve_destiny::Ball {
            id: pilot.ship_id,
            type_id: HURRICANE_TYPE,
            position: DVec3::new(150_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 200.0,
            inertia: 3.0,
            mass: 1.5e7,
            radius: 200.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        })
        .await;
    let flex_item = give_cargo_item(&ctx, pilot.char_id, pilot.ship_id, PHAROLUX_TYPE);
    let flex_err = call_rpc(
        &ctx,
        &mut pilot.sess,
        "structureDeployment",
        "Anchor",
        &[PyRep::Integer(flex_item), PyRep::String("Perimeter Pharolux".into())],
    )
    .await
    .expect_err("FLEX outside sov null rejected");
    assert_eq!(flex_err.code, "CannotAnchorFlexOutsideSovNull");

    let anc_res = call_rpc(
        &ctx,
        &mut pilot.sess,
        "structureDeployment",
        "Anchor",
        &[PyRep::Integer(young_item), PyRep::String("Perimeter Freezone".into())],
    )
    .await
    .expect("Anchor Astrahus in Perimeter");
    let sid = anc_res.dict_get("structureID").and_then(PyRep::as_i64).expect("sid");
    ctx.advance_secs(87_400);
    sync_all_structures(&ctx);
    ctx.db.with_mut_snapshot(|s| {
        if let Some(st) = s.structures.get_mut(&sid) {
            st.quantum_core_installed = true;
            st.state = "ShieldVulnerable".into();
        }
    });

    // Query structureDirectory RPCs
    let dockable = call_rpc(
        &ctx,
        &mut pilot.sess,
        "structureDirectory",
        "GetMyDockableStructures",
        &[PyRep::Integer(i64::from(PERIMETER))],
    )
    .await
    .expect("GetMyDockableStructures");
    let PyRep::List(dock_list) = dockable else { panic!("expected list") };
    assert!(dock_list.contains(&PyRep::Integer(sid)), "Anchored Astrahus visible in GetMyDockableStructures");
    let corp_structs = call_rpc(
        &ctx,
        &mut pilot.sess,
        "structureDirectory",
        "GetMyCorporationStructures",
        &[],
    )
    .await
    .expect("GetMyCorporationStructures");
    assert!(
        format!("{corp_structs:?}").contains(&sid.to_string()),
        "Anchored Astrahus visible in GetMyCorporationStructures"
    );

    let ctx2 = reload_ctx_from_sqlite(&ctx, &db_path);
    assert_eq!(
        ctx2.db.with_snapshot(|s| s.structures[&sid].owner_corp_id),
        corp_id,
        "Structure directory ownership persists across SQLite reload"
    );
}

// parity: feat:structures.profiles
// parity: feat:structures.defense-settings
#[tokio::test]
async fn test_e2e_structures_profiles_and_defense_settings() {
    eve_testkit::attest(&[
        "feat:structures.profiles",
        "feat:structures.defense-settings",
    ]);

    let (db_path, ctx) = new_sqlite_ctx("struct_profiles_def");
    crate::common::pin_clock(&ctx);

    let (mut director, corp_id) =
        setup_corp_director(&ctx, 83_002, "st_prof_p1", "Profile Director", "Defense Guild", "DEFG").await;
    ctx.advance_secs(7 * 86_400 + 60);
    place_pilot_in_space(&ctx, &mut director, PERIMETER, DVec3::new(150_000.0, 0.0, 0.0)).await;

    let ast_item = give_cargo_item(&ctx, director.char_id, director.ship_id, ASTRAHUS_TYPE);
    let anc_res = call_rpc(
        &ctx,
        &mut director.sess,
        "structureDeployment",
        "Anchor",
        &[PyRep::Integer(ast_item), PyRep::String("Fortress One".into())],
    )
    .await
    .expect("Anchor Astrahus");
    let sid = anc_res.dict_get("structureID").and_then(PyRep::as_i64).expect("sid");
    ctx.advance_secs(87_400);
    sync_all_structures(&ctx);

    // 1. Create custom Structure Profile via structureProfiles.CreateProfile
    let created_prof = call_rpc(
        &ctx,
        &mut director.sess,
        "structureProfiles",
        "CreateProfile",
        &[PyRep::String("High-Alert Defense Profile".into())],
    )
    .await
    .expect("CreateProfile");
    let profile_id = created_prof
        .as_i64()
        .or_else(|| created_prof.dict_get("profileID").and_then(PyRep::as_i64))
        .expect("profileID");

    // 2. Configure defense settings on profile via SaveProfileSettings (settingID 17 = Defense Control)
    call_rpc(
        &ctx,
        &mut director.sess,
        "structureProfiles",
        "SaveProfileSettings",
        &[
            PyRep::Integer(profile_id),
            PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::Integer(17),
                PyRep::Tuple(vec![
                    PyRep::Bool(true),
                    PyRep::Bool(true),
                    PyRep::Bool(true),
                    PyRep::Real(-2.5),
                    PyRep::Bool(true),
                ]),
                PyRep::Integer(0),
            ])]),
        ],
    )
    .await
    .expect("SaveProfileSettings with defense policy");

    // 3. Assign profile to structure via ChangeProfiles & advance past delayed profile application
    call_rpc(
        &ctx,
        &mut director.sess,
        "structureProfiles",
        "ChangeProfiles",
        &[PyRep::List(vec![PyRep::Integer(sid)]), PyRep::Integer(profile_id)],
    )
    .await
    .expect("ChangeProfiles");
    ctx.advance_secs(PROFILE_CHANGE_DELAY_SECS + 5);
    let _ = call_rpc(
        &ctx,
        &mut director.sess,
        "structureGuests",
        "GetGuests",
        &[PyRep::Integer(sid)],
    )
    .await;

    let (applied_pid, def_standing, auto_enabled) = ctx.db.with_snapshot(|s| {
        let ext = load_ext_state(s);
        let pid = ext.structure_profile_id.get(&sid).copied();
        let ds = load_defense_settings(s, sid);
        (pid, ds.standing_threshold, ds.auto_defense_enabled)
    });
    assert_eq!(applied_pid, Some(profile_id), "Delayed profile application must persist profile_id to DB");
    assert_eq!(auto_enabled, true, "Defense settings auto_defense_enabled persisted");
    assert_eq!(def_standing, -2.5, "Defense settings standing_threshold (-2.5) persisted");

    let ctx2 = reload_ctx_from_sqlite(&ctx, &db_path);
    let persisted_standing = ctx2.db.with_snapshot(|s| {
        assert_eq!(s.structures[&sid].owner_corp_id, corp_id);
        load_defense_settings(s, sid).standing_threshold
    });
    assert_eq!(persisted_standing, -2.5, "Profile & defense settings persist across SQLite reload");
}

// parity: feat:structures.repairshop-service-quotes-and-restoration
// parity: feat:structures.fulcrum-alignment-services
#[tokio::test]
async fn test_e2e_structures_repairshop_quotes_and_fulcrum_alignment_services() {
    eve_testkit::attest(&[
        "feat:structures.repairshop-service-quotes-and-restoration",
        "feat:structures.fulcrum-alignment-services",
    ]);

    let (db_path, ctx) = new_sqlite_ctx("repair_fulcrum");
    crate::common::pin_clock(&ctx);

    let (mut pilot, _corp_id) =
        setup_corp_director(&ctx, 83_003, "st_rep_p1", "Hull Mechanic", "Repair Guild", "REPG").await;
    ctx.advance_secs(7 * 86_400 + 60);
    place_pilot_in_space(&ctx, &mut pilot, PERIMETER, DVec3::new(250_000.0, 0.0, 0.0)).await;

    // 1. Anchor Astrahus with Quantum Core in LowPower (0 fuel, no service module) & verify LowPower Repair Shop works!
    let ast_item = give_cargo_item(&ctx, pilot.char_id, pilot.ship_id, ASTRAHUS_TYPE);
    let anc_res = call_rpc(
        &ctx,
        &mut pilot.sess,
        "structureDeployment",
        "Anchor",
        &[PyRep::Integer(ast_item), PyRep::String("LowPower Drydock".into())],
    )
    .await
    .expect("Anchor Astrahus");
    let sid = anc_res.dict_get("structureID").and_then(PyRep::as_i64).expect("sid");
    ctx.advance_secs(87_400);
    sync_all_structures(&ctx);

    let rep_target_id = ctx.db.with_mut_snapshot(|s| {
        let st = s.structures.get_mut(&sid).unwrap();
        st.quantum_core_installed = true;
        st.fuel_blocks = 0;
        st.state = "ShieldVulnerable".into();
        // Record thermal damage on pilot's fitted module (modules carry CPU > 0 for repair quotes & billing)
        let mod_id = s
            .items
            .values()
            .find(|i| i.location_id == pilot.ship_id && i.flag == eve_db::InventoryFlag::HiSlot0)
            .map(|i| i.item_id)
            .unwrap_or(pilot.ship_id);
        if let Some(item) = s.items.get_mut(&mod_id) {
            item.dynamic_attributes.insert(eve_sde::attr_ids::DAMAGE, 200.0);
        }
        mod_id
    });

    // Configure 5% repair shop tax (settingID 13) on LowPower Astrahus, dock & verify quote + ISK deduction + restoration
    let rep_prof = call_rpc(&ctx, &mut pilot.sess, "structureProfiles", "CreateProfile", &[PyRep::String("Taxed Repair Profile".into())])
        .await
        .expect("CreateProfile for repair tax");
    let rep_pid = rep_prof.as_i64().or_else(|| rep_prof.dict_get("profileID").and_then(PyRep::as_i64)).expect("rep_pid");
    call_rpc(
        &ctx,
        &mut pilot.sess,
        "structureProfiles",
        "SaveProfileSettings",
        &[
            PyRep::Integer(rep_pid),
            PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::Integer(13),
                PyRep::Real(0.05),
                PyRep::Integer(0),
            ])]),
        ],
    )
    .await
    .expect("SaveProfileSettings repair tax");
    call_rpc(
        &ctx,
        &mut pilot.sess,
        "structureProfiles",
        "ChangeProfiles",
        &[PyRep::List(vec![PyRep::Integer(sid)]), PyRep::Integer(rep_pid)],
    )
    .await
    .expect("ChangeProfiles on LowPower Astrahus");
    ctx.advance_secs(PROFILE_CHANGE_DELAY_SECS + 10);

    let sys_id = pilot.sess.solar_system_id.unwrap();
    let st_pos = ctx.db.with_snapshot(|s| DVec3::from_array(s.structures[&sid].position));
    ctx.systems.get_or_spawn(sys_id).set_ball_position(pilot.ship_id, st_pos).await;
    call_rpc(&ctx, &mut pilot.sess, "structureDocking", "Dock", &[PyRep::Integer(sid)])
        .await
        .expect("Dock in LowPower Astrahus");

    let quotes = call_rpc(
        &ctx,
        &mut pilot.sess,
        "repairSvc",
        "GetRepairQuotes",
        &[PyRep::List(vec![PyRep::Integer(pilot.ship_id)])],
    )
    .await
    .expect("LowPower structure repair shop must provide repair quotes");
    let isk_before_rep = ctx.db.with_snapshot(|s| s.characters[&pilot.char_id].balance.0);
    assert!(
        format!("{quotes:?}").contains("cost") || format!("{quotes:?}").contains("itemID"),
        "Repair quote returns item repair quote: {quotes:?}"
    );

    call_rpc(
        &ctx,
        &mut pilot.sess,
        "repairSvc",
        "RepairItemsInStructure",
        &[PyRep::List(vec![PyRep::Integer(pilot.ship_id)])],
    )
    .await
    .expect("RepairItemsInStructure in LowPower citadel");
    let (isk_after_rep, rem_dmg) = ctx.db.with_snapshot(|s| {
        let bal = s.characters[&pilot.char_id].balance.0;
        let dmg = s.items[&rep_target_id]
            .dynamic_attributes
            .get(&eve_sde::attr_ids::DAMAGE)
            .copied()
            .unwrap_or(0.0);
        (bal, dmg)
    });
    assert!(isk_after_rep < isk_before_rep, "Repairing damaged ship deducts quoted ISK");
    assert_eq!(rem_dmg, 0.0, "Repairing restores ship hull damage to 0.0");

    // 2. Test The Fulcrum Alignment Services in Zarzakh (refuses Empire militia, allows Pirate / neutral)!
    let _ = call_rpc(&ctx, &mut pilot.sess, "structureDocking", "Undock", &[]).await;
    pilot.sess.structure_id = None;
    pilot.sess.station_id = None;
    call_rpc(&ctx, &mut pilot.sess, "facWarMgr", "JoinFactionAsCharacter", &[PyRep::Integer(500_001)])
        .await
        .expect("Join Caldari Militia");
    let fulcrum_pos = ctx
        .sde
        .stations
        .get(&FULCRUM_STRUCTURE_ID)
        .map(|st| st.position)
        .unwrap_or(DVec3::ZERO);
    place_pilot_in_space(&ctx, &mut pilot, ZARZAKH, fulcrum_pos).await;
    pilot.sess.structure_id = None;
    pilot.sess.station_id = None;
    ctx.advance_secs(15);

    let empire_dock_err = call_rpc(&ctx, &mut pilot.sess, "stationSvc", "Dock", &[PyRep::Integer(FULCRUM_STRUCTURE_ID)])
        .await
        .expect_err("Empire militia pilot must be refused docking at The Fulcrum");
    assert_eq!(empire_dock_err.code, "FulcrumRefusesEmpireMilitia");

    // Pirate militia pilot docks at The Fulcrum successfully
    let mut pirate = spawn_player_combat_pilot(&ctx, 83_004, "fulcrum_pir_p1", "Deathless Guest", HURRICANE_TYPE, false).await;
    call_rpc(&ctx, &mut pirate.sess, "facWarMgr", "JoinFactionAsCharacter", &[PyRep::Integer(500_010)])
        .await
        .expect("Join Guristas Militia");
    place_pilot_in_space(&ctx, &mut pirate, ZARZAKH, fulcrum_pos).await;
    call_rpc(&ctx, &mut pirate.sess, "stationSvc", "Dock", &[PyRep::Integer(FULCRUM_STRUCTURE_ID)])
        .await
        .expect("Pirate militia pilot docks at The Fulcrum");
    assert_eq!(pirate.sess.station_id, Some(FULCRUM_STRUCTURE_ID));

    let ctx2 = reload_ctx_from_sqlite(&ctx, &db_path);
    assert_eq!(
        ctx2.db.with_snapshot(|s| s.characters[&pirate.char_id].station_id),
        Some(FULCRUM_STRUCTURE_ID),
        "Fulcrum docked state persists across SQLite reload"
    );
}
