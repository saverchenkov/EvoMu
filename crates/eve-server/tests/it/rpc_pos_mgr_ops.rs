//! Integration tests covering open RPC parity rows for `posMgr` (15 rows).
//!
//! Covers:
//! - Starbase permissions & control: `AssumeStructureControl`, `RelinquishStructureControl`,
//!   `GetStarbasePermissions`, `SetStarbasePermissions`
//! - Tower settings & passwords: `GetTowerNotificationSettings`, `SetTowerNotifications`,
//!   `GetTowerSentrySettings`, `SetTowerSentrySettings`, `SetTowerPassword`
//! - Moon mining & silos: `GetConsumptionFactor`, `GetSiloCapacityByItemID`, `LinkResourceForTower`,
//!   `RunMoonProcessCycleforTower`, `ChangeStructureProvisionType`
//! - Orbitals: `UnanchorOrbital`

use crate::common;
use eve_db::{
    InventoryFlag, InventoryItemRecord, StarbaseDetailSettings, UpwellStructureRecord,
};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use std::collections::HashMap;

const PILOT1: i64 = 90_000_001;
const SYS_LOWSEC: u32 = 30_000_012;
const POCO_TYPE_ID: u32 = 2233;
const GANTRY_TYPE_ID: u32 = 3962;

async fn setup_pilot(
    ctx: &ServerContext,
    session_id: u64,
    username: &str,
    password: &str,
    char_id: i64,
) -> SessionState {
    let kw = HashMap::new();
    let mut sess = SessionState::new(session_id);
    common::login_pilot(ctx, &mut sess, username, password).await;
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
    sess
}

fn seed_test_item(
    ctx: &ServerContext,
    item_id: i64,
    type_id: u32,
    owner_id: i64,
    location_id: i64,
    flag: InventoryFlag,
    quantity: i32,
) {
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            item_id,
            InventoryItemRecord {
                item_id,
                type_id,
                owner_id,
                location_id,
                flag,
                quantity,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });
}

fn seed_test_structure(
    ctx: &ServerContext,
    structure_id: i64,
    type_id: u32,
    owner_corp_id: i64,
    solar_system_id: u32,
    name: &str,
    state: &str,
) {
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            structure_id,
            UpwellStructureRecord {
                structure_id,
                type_id,
                owner_corp_id,
                solar_system_id,
                name: name.to_string(),
                position: [0.0, 0.0, 0.0],
                state: state.to_string(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                shield_hp: 10_000_000.0,
                armor_hp: 2_500_000.0,
                hull_hp: 2_000_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0, self_destruct_at_unix: None,
            },
        );
    });
}

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let kw = HashMap::new();
    ctx.dispatch_rpc(sess, service, None, method, args, &kw, None)
        .await
}

#[tokio::test]
async fn test_pos_mgr_control_and_permissions() {
    // parity: rpc:posMgr.AssumeStructureControl, rpc:posMgr.RelinquishStructureControl, rpc:posMgr.GetStarbasePermissions, rpc:posMgr.SetStarbasePermissions, rpc:posMgr.GetTowerNotificationSettings, rpc:posMgr.SetTowerNotifications, rpc:posMgr.GetTowerSentrySettings, rpc:posMgr.SetTowerSentrySettings
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let corp_id = 98_000_170_i64;
    let mut sess = setup_pilot(&ctx, 501, "demo", "demo-secret", PILOT1).await;
    common::sync_legal_corp_member(
        &ctx,
        &mut sess,
        PILOT1,
        corp_id,
        "Starbase Ops Corp",
        "SBOP",
        false,
        eve_db::corp_roles::ROLE_DIRECTOR,
        None,
    );
    common::assert_it_legal_fixture(
        &ctx,
        "test_pos_mgr_control_and_permissions",
        &sess,
        true,
        eve_db::corp_roles::ROLE_DIRECTOR,
    );
    let tower_id = 140_000_101_i64;
    seed_test_structure(&ctx, tower_id, 12235, corp_id, SYS_LOWSEC, "Test Control Tower", "Online");

    // Seed initial starbase settings in world snapshot
    ctx.db.with_mut_snapshot(|snap| {
        snap.starbase_settings.insert(
            tower_id,
            StarbaseDetailSettings {
                allow_corporation_members: true,
                allow_alliance_members: true,
                use_alliance_standings: false,
                attack_if_other_security_status_dropping: false,
                attack_if_at_war: true,
                fuel_bay_view: "corp".into(),
                fuel_bay_take: "corp".into(),
                anchor: "corp".into(),
                unanchor: "corp".into(),
                online: "corp".into(),
                offline: "corp".into(),
                attack_standing_threshold: Some(0.0),
                attack_security_status_threshold: Some(0.0),
                ..Default::default()
            },
        );
    });

    // 1. posMgr.AssumeStructureControl
    let assume_res = rpc(
        &ctx,
        &mut sess,
        "posMgr",
        "AssumeStructureControl",
        &[PyRep::Integer(tower_id)],
    )
    .await
    .expect("posMgr.AssumeStructureControl");
    assert_eq!(assume_res, PyRep::None);

    let has_assume_controller = ctx.db.with_snapshot(|snap| {
        snap.starbase_settings.get(&tower_id).and_then(|s| s.active_controller_id)
    });
    assert_eq!(has_assume_controller, Some(PILOT1), "AssumeStructureControl must record active controller in starbase_settings");

    // 2. posMgr.RelinquishStructureControl
    let relinq_res = rpc(
        &ctx,
        &mut sess,
        "posMgr",
        "RelinquishStructureControl",
        &[PyRep::Integer(tower_id)],
    )
    .await
    .expect("posMgr.RelinquishStructureControl");
    assert_eq!(relinq_res, PyRep::None);

    let has_relinq_controller = ctx.db.with_snapshot(|snap| {
        snap.starbase_settings.get(&tower_id).and_then(|s| s.active_controller_id)
    });
    assert_eq!(has_relinq_controller, None, "RelinquishStructureControl must clear active controller in starbase_settings");

    // 3. posMgr.GetStarbasePermissions
    let perm_res = rpc(
        &ctx,
        &mut sess,
        "posMgr",
        "GetStarbasePermissions",
        &[PyRep::Integer(tower_id)],
    )
    .await
    .expect("posMgr.GetStarbasePermissions");
    match &perm_res {
        PyRep::Tuple(items) => {
            assert_eq!(items.len(), 2, "GetStarbasePermissions must return a 2-tuple (deployFlags, usageFlags)");
            assert!(matches!(&items[0], PyRep::Object { type_name, .. } if type_name == "util.Row"));
            assert!(matches!(&items[1], PyRep::Rowset { .. }));
        }
        other => panic!("Expected PyRep::Tuple from GetStarbasePermissions, got: {other:?}"),
    }

    // 4. posMgr.SetStarbasePermissions
    let set_perm_res = rpc(
        &ctx,
        &mut sess,
        "posMgr",
        "SetStarbasePermissions",
        &[
            PyRep::Integer(tower_id),
            PyRep::String("alliance".into()),
            PyRep::String("alliance".into()),
            PyRep::String("corp".into()),
            PyRep::String("corp".into()),
        ],
    )
    .await
    .expect("posMgr.SetStarbasePermissions");
    assert_eq!(set_perm_res, PyRep::None);

    let updated_perms = ctx.db.with_snapshot(|snap| {
        snap.starbase_settings
            .get(&tower_id)
            .map(|s| (s.anchor.clone(), s.unanchor.clone(), s.online.clone(), s.offline.clone()))
    });
    assert_eq!(
        updated_perms,
        Some(("alliance".into(), "alliance".into(), "corp".into(), "corp".into())),
        "SetStarbasePermissions must mutate starbase_settings in world snapshot"
    );

    // 5. posMgr.GetTowerNotificationSettings
    let notif_res = rpc(
        &ctx,
        &mut sess,
        "posMgr",
        "GetTowerNotificationSettings",
        &[PyRep::Integer(tower_id)],
    )
    .await
    .expect("posMgr.GetTowerNotificationSettings");
    assert!(matches!(&notif_res, PyRep::Object { type_name, .. } if type_name == "util.Row"));

    // 6. posMgr.SetTowerNotifications
    let set_notif_res = rpc(
        &ctx,
        &mut sess,
        "posMgr",
        "SetTowerNotifications",
        &[
            PyRep::Integer(tower_id),
            PyRep::Bool(true),  // showInCalendar
            PyRep::Bool(false), // sendFuelNotifications
        ],
    )
    .await
    .expect("posMgr.SetTowerNotifications");
    assert_eq!(set_notif_res, PyRep::None);

    let notif_settings = ctx.db.with_snapshot(|snap| {
        snap.starbase_settings
            .get(&tower_id)
            .map(|s| (s.notify_fuel, s.notify_calendar))
    });
    assert_eq!(
        notif_settings,
        Some((false, true)),
        "SetTowerNotifications must persist updated notification thresholds in starbase_settings"
    );

    // Verify GetTowerNotificationSettings reflects persisted values
    let notif_res2 = rpc(
        &ctx,
        &mut sess,
        "posMgr",
        "GetTowerNotificationSettings",
        &[PyRep::Integer(tower_id)],
    )
    .await
    .expect("posMgr.GetTowerNotificationSettings after update");
    if let PyRep::Object { args, .. } = notif_res2 {
        if let PyRep::Tuple(items) = *args {
            if let PyRep::List(vals) = &items[1] {
                assert_eq!(vals[0], PyRep::Bool(false), "sendFuelNotifications should be false");
                assert_eq!(vals[1], PyRep::Bool(true), "showInCalendar should be true");
            }
        }
    }

    // 7. posMgr.GetTowerSentrySettings
    let sentry_res = rpc(
        &ctx,
        &mut sess,
        "posMgr",
        "GetTowerSentrySettings",
        &[PyRep::Integer(tower_id)],
    )
    .await
    .expect("posMgr.GetTowerSentrySettings");
    assert!(matches!(&sentry_res, PyRep::Object { type_name, .. } if type_name == "util.Row"));

    // 8. posMgr.SetTowerSentrySettings
    let set_sentry_res = rpc(
        &ctx,
        &mut sess,
        "posMgr",
        "SetTowerSentrySettings",
        &[
            PyRep::Integer(tower_id),
            PyRep::Real(2.5),  // standing threshold
            PyRep::Real(-1.0), // security status threshold
            PyRep::Bool(true), // statusDrop
            PyRep::Bool(true), // corpWar
            PyRep::Bool(true), // useAllianceStandings
        ],
    )
    .await
    .expect("posMgr.SetTowerSentrySettings");
    assert_eq!(set_sentry_res, PyRep::None);

    let updated_sentry = ctx.db.with_snapshot(|snap| {
        snap.starbase_settings.get(&tower_id).cloned()
    });
    let s = updated_sentry.expect("starbase settings must exist");
    assert_eq!(s.attack_standing_threshold, Some(2.5));
    assert_eq!(s.attack_security_status_threshold, Some(-1.0));
    assert!(s.attack_if_other_security_status_dropping);
    assert!(s.attack_if_at_war);
    assert!(s.use_alliance_standings);

    // 9. Negative authorization checks: non-owner and non-director fail
    let mut unauth_sess = SessionState::new(998);
    unauth_sess.user_id = Some(999);
    unauth_sess.character_id = Some(90_000_002);
    unauth_sess.corporation_id = Some(999_999);
    let owner_err = rpc(
        &ctx,
        &mut unauth_sess,
        "posMgr",
        "SetStarbasePermissions",
        &[PyRep::Integer(tower_id), PyRep::String("corp".into()), PyRep::String("corp".into()), PyRep::String("corp".into()), PyRep::String("corp".into())],
    ).await.expect_err("Non-owner corp must fail");
    assert_eq!(owner_err.code, "NotStarbaseOwner");

    let mut no_role_sess = setup_pilot(&ctx, 503, "demo", "demo-secret", PILOT1).await;
    common::set_legal_corp_roles(&ctx, &mut no_role_sess, PILOT1, corp_id, 0);
    let role_err = rpc(
        &ctx,
        &mut no_role_sess,
        "posMgr",
        "SetTowerNotifications",
        &[PyRep::Integer(tower_id), PyRep::Bool(true), PyRep::Bool(true)],
    ).await.expect_err("Member without director/operator role must fail");
    assert_eq!(role_err.code, "RoleNotAuthorized");

    eve_testkit::attest(&[
        "rpc:posMgr.AssumeStructureControl",
        "rpc:posMgr.RelinquishStructureControl",
        "rpc:posMgr.GetStarbasePermissions",
        "rpc:posMgr.SetStarbasePermissions",
        "rpc:posMgr.GetTowerNotificationSettings",
        "rpc:posMgr.SetTowerNotifications",
        "rpc:posMgr.GetTowerSentrySettings",
        "rpc:posMgr.SetTowerSentrySettings",
    ]);
}

#[tokio::test]
async fn test_pos_mgr_moon_silo_and_orbital() {
    // parity: rpc:posMgr.SetTowerPassword, rpc:posMgr.GetConsumptionFactor, rpc:posMgr.GetSiloCapacityByItemID, rpc:posMgr.LinkResourceForTower, rpc:posMgr.ChangeStructureProvisionType, rpc:posMgr.UnanchorOrbital
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let corp_id = 98_000_170_i64;
    let mut sess = setup_pilot(&ctx, 502, "demo", "demo-secret", PILOT1).await;
    common::sync_legal_corp_member(
        &ctx,
        &mut sess,
        PILOT1,
        corp_id,
        "Starbase Ops Corp",
        "SBOP",
        false,
        eve_db::corp_roles::ROLE_DIRECTOR,
        None,
    );
    common::assert_it_legal_fixture(
        &ctx,
        "test_pos_mgr_moon_silo_and_orbital",
        &sess,
        true,
        eve_db::corp_roles::ROLE_DIRECTOR,
    );
    let tower_id = 140_000_102_i64;
    let silo_id = 140_000_201_i64;
    let orbital_id = 140_000_301_i64;
    seed_test_structure(&ctx, tower_id, 12235, corp_id, SYS_LOWSEC, "Moon Tower", "Online");

    // 1. posMgr.SetTowerPassword
    let pass_res = rpc(
        &ctx,
        &mut sess,
        "posMgr",
        "SetTowerPassword",
        &[
            PyRep::Integer(tower_id),
            PyRep::String("secure_pos_forcefield_pass".into()),
            PyRep::Bool(true),  // allowCorp
            PyRep::Bool(false), // allowAlliance
        ],
    )
    .await
    .expect("posMgr.SetTowerPassword");
    assert_eq!(pass_res, PyRep::None);

    let pass_settings = ctx.db.with_snapshot(|snap| {
        snap.starbase_settings
            .get(&tower_id)
            .map(|s| (s.password.clone(), s.allow_corporation_members, s.allow_alliance_members))
    });
    assert_eq!(
        pass_settings,
        Some((Some("secure_pos_forcefield_pass".into()), true, false)),
        "SetTowerPassword must persist password in starbase_settings"
    );

    // 2. posMgr.GetConsumptionFactor (1.0 without sovereignty, 0.75 when tower owner alliance holds sovereignty)
    let factor_res = rpc(
        &ctx,
        &mut sess,
        "posMgr",
        "GetConsumptionFactor",
        &[PyRep::Integer(tower_id)],
    )
    .await
    .expect("posMgr.GetConsumptionFactor");
    assert_eq!(factor_res, PyRep::Real(1.0));

    // When tower owner's alliance holds sovereignty in the tower's solar system:
    ctx.db.with_mut_snapshot(|snap| {
        snap.alliances.insert(
            99_000_001,
            eve_db::AllianceRecord {
                alliance_id: 99_000_001,
                name: "Sov Starbase Alliance".into(),
                short_name: "SSBA".into(),
                executor_corp_id: corp_id,
                member_corp_ids: std::collections::HashSet::from([corp_id]),
            },
        );
        snap.system_states.entry(SYS_LOWSEC).or_default().sov_alliance_id = Some(99_000_001);
    });
    common::sync_legal_corp_member(
        &ctx,
        &mut sess,
        PILOT1,
        corp_id,
        "Starbase Ops Corp",
        "SBOP",
        false,
        eve_db::corp_roles::ROLE_DIRECTOR,
        Some(99_000_001),
    );
    let sov_factor_res = rpc(
        &ctx,
        &mut sess,
        "posMgr",
        "GetConsumptionFactor",
        &[PyRep::Integer(tower_id)],
    )
    .await
    .expect("posMgr.GetConsumptionFactor with sovereignty");
    assert_eq!(sov_factor_res, PyRep::Real(0.75));

    // Seed silo item in DB
    seed_test_item(
        &ctx,
        silo_id,
        14343, // Silo type
        PILOT1,
        tower_id,
        InventoryFlag::None,
        1,
    );

    // 3. posMgr.GetSiloCapacityByItemID
    let cap_res = rpc(
        &ctx,
        &mut sess,
        "posMgr",
        "GetSiloCapacityByItemID",
        &[PyRep::Integer(silo_id)],
    )
    .await
    .expect("posMgr.GetSiloCapacityByItemID");
    match cap_res {
        PyRep::Real(cap) => assert!(cap > 0.0, "Silo capacity must be positive, got: {cap}"),
        other => panic!("Expected PyRep::Real from GetSiloCapacityByItemID, got: {other:?}"),
    }

    // 4. posMgr.LinkResourceForTower
    let link_res = rpc(
        &ctx,
        &mut sess,
        "posMgr",
        "LinkResourceForTower",
        &[
            PyRep::Integer(tower_id),
            PyRep::List(vec![PyRep::Integer(silo_id)]),
        ],
    )
    .await
    .expect("posMgr.LinkResourceForTower");
    assert_eq!(link_res, PyRep::None);

    let has_linked = ctx.db.with_snapshot(|snap| {
        snap.starbase_settings
            .get(&tower_id)
            .map(|s| s.linked_resources.contains(&silo_id))
            .unwrap_or(false)
    });
    assert!(has_linked, "LinkResourceForTower must persist linkages in starbase_settings");

    // 5. posMgr.ChangeStructureProvisionType
    let prov_res = rpc(
        &ctx,
        &mut sess,
        "posMgr",
        "ChangeStructureProvisionType",
        &[
            PyRep::Integer(tower_id),
            PyRep::Integer(silo_id),
            PyRep::Integer(16634), // Atmospheric Gases
        ],
    )
    .await
    .expect("posMgr.ChangeStructureProvisionType");
    assert_eq!(prov_res, PyRep::None);

    let prov_type = ctx.db.with_snapshot(|snap| {
        snap.orbital_provision_types.get(&silo_id).copied()
    });
    assert_eq!(prov_type, Some(16634), "ChangeStructureProvisionType must update orbital_provision_types");

    // 6. posMgr.RunMoonProcessCycleforTower executes a real starbase harvesting & fuel cycle
    let cycle_res = rpc(
        &ctx,
        &mut sess,
        "posMgr",
        "RunMoonProcessCycleforTower",
        &[PyRep::Integer(tower_id)],
    )
    .await
    .expect("posMgr.RunMoonProcessCycleforTower");
    assert!(matches!(&cycle_res, PyRep::List(entries) if entries.len() == 1));
    let (remaining_fuel, harvested_qty) = ctx.db.with_snapshot(|snap| {
        let fuel = snap.structures.get(&tower_id).map(|s| s.fuel_blocks).unwrap_or(0);
        let qty: i32 = snap
            .items
            .values()
            .filter(|it| it.location_id == silo_id && it.type_id == 16634)
            .map(|it| it.quantity)
            .sum();
        (fuel, qty)
    });
    // Amarr Control Tower (12235) base fuel is 40/hr; with 0.75 sov discount -> 30 consumed (100 - 30 = 70).
    assert_eq!(remaining_fuel, 70);
    assert_eq!(harvested_qty, 100);

    // 7. posMgr.UnanchorOrbital
    // Seed anchored Customs Office Gantry (7,600 m3) in low-sec space & Charon freighter (465,000 m3) in range
    seed_test_item(
        &ctx,
        orbital_id,
        GANTRY_TYPE_ID,
        PILOT1,
        i64::from(SYS_LOWSEC),
        InventoryFlag::None,
        1,
    );
    seed_test_structure(
        &ctx,
        orbital_id,
        GANTRY_TYPE_ID,
        1_000_035,
        SYS_LOWSEC,
        "Lowsec Customs Office Gantry",
        "ShieldVulnerable",
    );
    let lowsec_ship_id = sess.ship_id.expect("PILOT1 active ship");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT1) {
            ch.solar_system_id = SYS_LOWSEC;
        }
        if let Some(sh) = snap.items.get_mut(&lowsec_ship_id) {
            sh.type_id = 20185; // Charon freighter (465,000 m3 cargo)
            sh.location_id = i64::from(SYS_LOWSEC);
            sh.flag = InventoryFlag::None;
            sh.singleton = true;
        }
    });
    sess.solar_system_id = Some(SYS_LOWSEC);
    ctx.systems
        .get_or_spawn(SYS_LOWSEC)
        .spawn_ball(eve_destiny::Ball {
            id: lowsec_ship_id,
            type_id: 20185,
            position: glam::DVec3::new(1_000.0, 0.0, 0.0),
            velocity: glam::DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        })
        .await
        .expect("spawn Charon ball in SYS_LOWSEC");

    // Docked callers must be rejected with NotInSpace
    let docked_err = rpc(
        &ctx,
        &mut sess,
        "posMgr",
        "UnanchorOrbital",
        &[PyRep::Integer(orbital_id)],
    )
    .await
    .expect_err("Docked UnanchorOrbital must fail with NotInSpace");
    assert_eq!(docked_err.code, "NotInSpace");

    sess.station_id = None;
    sess.structure_id = None;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT1) {
            ch.station_id = None;
            ch.structure_id = None;
        }
    });
    let _ = ctx.drain_notifications_for_char(PILOT1);

    let unanchor_res = rpc(
        &ctx,
        &mut sess,
        "posMgr",
        "UnanchorOrbital",
        &[PyRep::Integer(orbital_id)],
    )
    .await
    .expect("posMgr.UnanchorOrbital");
    assert_eq!(unanchor_res.dict_get("state"), Some(&PyRep::String("Unanchored".into())));
    assert_eq!(unanchor_res.dict_get("itemID"), Some(&PyRep::Integer(orbital_id)));

    let (st_present, it_flag, it_loc) = ctx.db.with_snapshot(|snap| {
        let st_present = snap.structures.contains_key(&orbital_id);
        let it_flag = snap.items.get(&orbital_id).map(|i| i.flag);
        let it_loc = snap.items.get(&orbital_id).map(|i| i.location_id);
        (st_present, it_flag, it_loc)
    });
    assert!(
        !st_present,
        "UnanchorOrbital must remove structure record from snap.structures"
    );
    assert_eq!(it_flag, Some(InventoryFlag::Cargo), "UnanchorOrbital must transfer item to ship cargo hold");
    assert_eq!(it_loc, Some(lowsec_ship_id), "UnanchorOrbital must move item into active Charon hold");
    assert!(
        ctx.drain_notifications_for_char(PILOT1)
            .iter()
            .any(|n| n.event == "OnItemChange"),
        "UnanchorOrbital must emit OnItemChange"
    );

    // Calling UnanchorOrbital on missing orbital returns OrbitalNotFound
    let missing_err = rpc(
        &ctx,
        &mut sess,
        "posMgr",
        "UnanchorOrbital",
        &[PyRep::Integer(999_999_999)],
    )
    .await
    .expect_err("Unanchoring missing orbital must fail");
    assert_eq!(missing_err.code, "OrbitalNotFound");

    eve_testkit::attest(&[
        "rpc:posMgr.SetTowerPassword",
        "rpc:posMgr.GetConsumptionFactor",
        "rpc:posMgr.GetSiloCapacityByItemID",
        "rpc:posMgr.LinkResourceForTower",
        "rpc:posMgr.ChangeStructureProvisionType",
        "rpc:posMgr.UnanchorOrbital",
    ]);
}

#[tokio::test]
async fn test_pos_mgr_towers_moon_cycle_and_target_obo() {
    // parity: rpc:posMgr.GetControlTowers, rpc:posMgr.GetMoonForTower, notify:OnTargetOBO, feat:removed.pos-towers
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let corp_id = 98_000_170_i64;
    let mut sess = setup_pilot(&ctx, 504, "demo", "demo-secret", PILOT1).await;
    common::sync_legal_corp_member(
        &ctx,
        &mut sess,
        PILOT1,
        corp_id,
        "Starbase Ops Corp",
        "SBOP",
        false,
        eve_db::corp_roles::ROLE_DIRECTOR,
        None,
    );
    common::assert_it_legal_fixture(
        &ctx,
        "test_pos_mgr_towers_moon_cycle_and_target_obo",
        &sess,
        true,
        eve_db::corp_roles::ROLE_DIRECTOR,
    );
    sess.solar_system_id = Some(SYS_LOWSEC);
    sess.station_id = None;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT1) {
            ch.solar_system_id = SYS_LOWSEC;
            ch.station_id = None;
        }
    });

    let tower_id = 140_000_401_i64;
    let silo_id = 140_000_402_i64;
    let fuel_item_id = 140_000_403_i64;
    let target_ship_id = 140_000_404_i64;

    seed_test_structure(&ctx, tower_id, 12235, corp_id, SYS_LOWSEC, "Amarr Starbase Tower", "Online");
    // Seed 200 Helium Fuel Blocks (4247) inside the tower and an anchored Silo (14343)
    seed_test_item(&ctx, fuel_item_id, 4247, corp_id, tower_id, InventoryFlag::Unlocked, 200);
    seed_test_item(&ctx, silo_id, 14343, corp_id, tower_id, InventoryFlag::Unlocked, 1);

    // 1. posMgr.GetControlTowers
    let towers_res = rpc(&ctx, &mut sess, "posMgr", "GetControlTowers", &[])
        .await
        .expect("posMgr.GetControlTowers");
    if let PyRep::List(towers) = &towers_res {
        assert_eq!(towers.len(), 1);
        let PyRep::Tuple(tup) = &towers[0] else {
            panic!("Expected 3-tuple element in GetControlTowers, got {:?}", towers[0]);
        };
        assert_eq!(tup.len(), 3);
        assert_eq!(tup[0], PyRep::Integer(tower_id));
        assert_eq!(tup[1], PyRep::Integer(12235));
        assert_eq!(tup[2].dict_get("structureID"), Some(&PyRep::Integer(tower_id)));
        assert_eq!(tup[2].dict_get("typeID"), Some(&PyRep::Integer(12235)));
        assert_eq!(tup[2].dict_get("solarSystemID"), Some(&PyRep::Integer(i64::from(SYS_LOWSEC))));
        assert_eq!(tup[2].dict_get("state"), Some(&PyRep::String("Online".into())));
    } else {
        panic!("Expected PyRep::List from GetControlTowers, got {towers_res:?}");
    }

    // 2. posMgr.GetMoonForTower
    let moon_res = rpc(
        &ctx,
        &mut sess,
        "posMgr",
        "GetMoonForTower",
        &[PyRep::Integer(tower_id)],
    )
    .await
    .expect("posMgr.GetMoonForTower");
    let moon_id = moon_res.as_i64().expect("GetMoonForTower must return integer moon ID");
    assert!(
        (40_000_000..=49_999_999).contains(&moon_id),
        "GetMoonForTower must resolve a valid celestial moon ID, got {moon_id}"
    );

    // 3. Link silo, configure moon material (16635 Evaporite Deposits), query process info, and run cycle
    rpc(
        &ctx,
        &mut sess,
        "posMgr",
        "ChangeStructureProvisionType",
        &[
            PyRep::Integer(tower_id),
            PyRep::Integer(silo_id),
            PyRep::Integer(16635),
        ],
    )
    .await
    .expect("ChangeStructureProvisionType");
    rpc(
        &ctx,
        &mut sess,
        "posMgr",
        "LinkResourceForTower",
        &[
            PyRep::Integer(tower_id),
            PyRep::List(vec![PyRep::Integer(silo_id)]),
        ],
    )
    .await
    .expect("LinkResourceForTower");

    let tower_proc_info = rpc(
        &ctx,
        &mut sess,
        "posMgr",
        "GetMoonProcessInfoForTower",
        &[PyRep::Integer(tower_id)],
    )
    .await
    .expect("GetMoonProcessInfoForTower(tower_id)");
    assert!(
        matches!(&tower_proc_info, PyRep::List(rows) if rows.len() == 1),
        "GetMoonProcessInfoForTower on a tower with linked resources must return cycle summary list, got {tower_proc_info:?}"
    );

    let cycle_res = rpc(
        &ctx,
        &mut sess,
        "posMgr",
        "RunMoonProcessCycleforTower",
        &[PyRep::Integer(tower_id)],
    )
    .await
    .expect("RunMoonProcessCycleforTower");
    if let PyRep::List(rows) = &cycle_res {
        assert_eq!(rows.len(), 1);
        if let PyRep::Tuple(cols) = &rows[0] {
            assert_eq!(cols[0], PyRep::Integer(silo_id));
            assert_eq!(cols[1], PyRep::Integer(16635));
            assert_eq!(cols[2], PyRep::Integer(100)); // harvested
            assert_eq!(cols[3], PyRep::Integer(100)); // total in silo
            assert_eq!(cols[4], PyRep::Integer(40));  // 40 Helium Fuel Blocks consumed
            assert_eq!(cols[5], PyRep::Integer(60));  // 100 - 40 = 60 remaining on structure record
        } else {
            panic!("Expected 6-tuple in RunMoonProcessCycleforTower entry");
        }
    } else {
        panic!("Expected PyRep::List from RunMoonProcessCycleforTower");
    }

    let fuel_item_remaining = ctx
        .db
        .with_snapshot(|snap| snap.items.get(&fuel_item_id).map(|it| it.quantity));
    assert_eq!(fuel_item_remaining, Some(160), "200 - 40 Helium Fuel Blocks must leave 160 in tower fuel bay");

    // 4. Assume structure control and lock/unlock target on behalf of tower (`notify:OnTargetOBO`)
    let sys = ctx.systems.get_or_spawn(SYS_LOWSEC);
    let _ = sys
        .spawn_ball(eve_destiny::Ball {
            id: tower_id,
            type_id: 12235,
            position: glam::DVec3::new(0.0, 0.0, 0.0),
            velocity: glam::DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 100_000_000.0,
            radius: 2_000.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        })
        .await;
    let _ = sys
        .spawn_ball(eve_destiny::Ball {
            id: target_ship_id,
            type_id: 587,
            position: glam::DVec3::new(5_000.0, 0.0, 0.0),
            velocity: glam::DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 2.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        })
        .await;

    let _ = ctx.drain_notifications_for_char(PILOT1);
    rpc(
        &ctx,
        &mut sess,
        "posMgr",
        "AssumeStructureControl",
        &[PyRep::Integer(tower_id)],
    )
    .await
    .expect("AssumeStructureControl");

    rpc(
        &ctx,
        &mut sess,
        "dogmaIM",
        "AddTargetOBO",
        &[PyRep::Integer(tower_id), PyRep::Integer(target_ship_id)],
    )
    .await
    .expect("dogmaIM.AddTargetOBO");

    rpc(
        &ctx,
        &mut sess,
        "dogmaIM",
        "RemoveTargetOBO",
        &[PyRep::Integer(tower_id), PyRep::Integer(target_ship_id)],
    )
    .await
    .expect("dogmaIM.RemoveTargetOBO");

    rpc(
        &ctx,
        &mut sess,
        "posMgr",
        "RelinquishStructureControl",
        &[PyRep::Integer(tower_id)],
    )
    .await
    .expect("RelinquishStructureControl");

    let obo_notifs: Vec<Vec<PyRep>> = ctx
        .drain_notifications_for_char(PILOT1)
        .into_iter()
        .filter(|n| n.event == "OnTargetOBO")
        .map(|n| n.args)
        .collect();
    assert_eq!(obo_notifs.len(), 3, "Expected add, lost, and clear OnTargetOBO notifications");
    assert_eq!(
        obo_notifs[0],
        vec![
            PyRep::String("add".into()),
            PyRep::Integer(tower_id),
            PyRep::Integer(target_ship_id),
            PyRep::None,
        ]
    );
    assert_eq!(
        obo_notifs[1],
        vec![
            PyRep::String("lost".into()),
            PyRep::Integer(tower_id),
            PyRep::Integer(target_ship_id),
            PyRep::String("unlocked".into()),
        ]
    );
    assert_eq!(
        obo_notifs[2],
        vec![
            PyRep::String("clear".into()),
            PyRep::Integer(tower_id),
            PyRep::None,
            PyRep::None,
        ]
    );

    eve_testkit::attest(&[
        "rpc:posMgr.GetControlTowers",
        "rpc:posMgr.GetMoonForTower",
        "notify:OnTargetOBO",
        "feat:removed.pos-towers",
    ]);
}

async fn wire_rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    bound_id: Option<u64>,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, eve_testkit::client_harness::ClientErrorResponse> {
    let kw = HashMap::new();
    common::borrow_wire_harness(ctx, sess)
        .call(service, bound_id, method, args, &kw)
        .await
}

#[tokio::test]
async fn test_pos_mgr_unanchor_orbital_e2e_lifecycle_and_security() {
    // parity: rpc:posMgr.AnchorOrbital, rpc:posMgr.OnlineOrbital, rpc:posMgr.UnanchorOrbital
    const RANCER_SYS: u32 = 30_002_718;
    const GANTRY_TYPE_ID: u32 = 3962;

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let planet = ctx
        .sde
        .celestials_in_system(RANCER_SYS)
        .into_iter()
        .find(|c| c.group_id == 7)
        .expect("Rancer planet");
    let near_planet = planet.position + glam::DVec3::new(planet.radius + 5_000.0, 0.0, 0.0);

    let mut dir_sess = setup_pilot(&ctx, 505, "demo", "demo-secret", PILOT1).await;
    let ship_id = dir_sess.ship_id.expect("Active ship");
    let corp_id = 98_555_001_i64;
    let other_corp_id = 98_555_002_i64;
    let line_char_id = 90_099_102_i64;
    let outsider_char_id = 90_099_103_i64;
    let far_ship_id = 890_555_101_i64;
    let non_orbital_id = 890_555_102_i64;
    let gantry_id = 890_555_201_i64;
    let poco_upgrade_id = 890_555_202_i64;
    let gantry2_id = 890_555_203_i64;
    let small_ship_id = 890_555_103_i64;
    let no_ball_ship_id = 890_555_104_i64;
    let hangar_gantry_id = 890_555_204_i64;
    let locked_gantry_id = 890_555_205_i64;
    let contract_gantry_id = 890_555_206_i64;
    let skyhook_id = 890_555_207_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut corp = eve_db::CorporationRecord::default();
        corp.corporation_id = corp_id;
        corp.name = "Orbital Test Corp".into();
        corp.ticker = "ORBT".into();
        corp.ceo_id = PILOT1;
        snap.corporations.insert(corp_id, corp);

        let mut other_corp = eve_db::CorporationRecord::default();
        other_corp.corporation_id = other_corp_id;
        other_corp.name = "Foreign Corp".into();
        other_corp.ticker = "FRGN".into();
        other_corp.ceo_id = outsider_char_id;
        snap.corporations.insert(other_corp_id, other_corp);

        let base_char = snap.characters.get(&PILOT1).cloned().expect("PILOT1");
        if let Some(ch) = snap.characters.get_mut(&PILOT1) {
            ch.corporation_id = corp_id;
            ch.solar_system_id = RANCER_SYS;
            ch.station_id = None;
            ch.structure_id = None;
        }
        let mut line_ch = base_char.clone();
        line_ch.character_id = line_char_id;
        line_ch.user_id = 99_502;
        line_ch.name = "Orbital Line".into();
        line_ch.corporation_id = corp_id;
        line_ch.solar_system_id = RANCER_SYS;
        line_ch.station_id = None;
        line_ch.structure_id = None;
        line_ch.active_ship_id = ship_id;
        snap.characters.insert(line_char_id, line_ch);

        let mut out_ch = base_char;
        out_ch.character_id = outsider_char_id;
        out_ch.user_id = 99_503;
        out_ch.name = "Orbital Outsider".into();
        out_ch.corporation_id = other_corp_id;
        out_ch.solar_system_id = RANCER_SYS;
        out_ch.station_id = None;
        out_ch.structure_id = None;
        snap.characters.insert(outsider_char_id, out_ch);

        if let Some(sh) = snap.items.get_mut(&ship_id) {
            sh.type_id = 20185; // Charon freighter (465,000 m3 cargo)
            sh.location_id = i64::from(RANCER_SYS);
            sh.flag = InventoryFlag::None;
            sh.singleton = true;
        }
        for (sid, tid) in [
            (far_ship_id, 20185_u32),
            (small_ship_id, 587_u32), // Rifter (140 m3 < 7,600 m3 gantry)
            (no_ball_ship_id, 20185_u32),
        ] {
            snap.items.insert(
                sid,
                InventoryItemRecord {
                    item_id: sid,
                    type_id: tid,
                    owner_id: PILOT1,
                    location_id: i64::from(RANCER_SYS),
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
        }
        snap.items.insert(
            non_orbital_id,
            InventoryItemRecord {
                item_id: non_orbital_id,
                type_id: 34, // Tritanium
                owner_id: corp_id,
                location_id: i64::from(RANCER_SYS),
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: false,
                ..Default::default()
            },
        );
        snap.items.insert(
            gantry_id,
            InventoryItemRecord {
                item_id: gantry_id,
                type_id: GANTRY_TYPE_ID,
                owner_id: PILOT1,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: false,
                ..Default::default()
            },
        );
        snap.items.insert(
            poco_upgrade_id,
            InventoryItemRecord {
                item_id: poco_upgrade_id,
                type_id: POCO_TYPE_ID,
                owner_id: PILOT1,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: false,
                ..Default::default()
            },
        );
        snap.items.insert(
            gantry2_id,
            InventoryItemRecord {
                item_id: gantry2_id,
                type_id: GANTRY_TYPE_ID,
                owner_id: PILOT1,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: false,
                ..Default::default()
            },
        );
        snap.items.insert(
            hangar_gantry_id,
            InventoryItemRecord {
                item_id: hangar_gantry_id,
                type_id: GANTRY_TYPE_ID,
                owner_id: PILOT1,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            locked_gantry_id,
            InventoryItemRecord {
                item_id: locked_gantry_id,
                type_id: GANTRY_TYPE_ID,
                owner_id: corp_id,
                location_id: i64::from(RANCER_SYS),
                flag: InventoryFlag::Locked,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.structures.insert(
            locked_gantry_id,
            UpwellStructureRecord {
                structure_id: locked_gantry_id,
                type_id: GANTRY_TYPE_ID,
                owner_corp_id: corp_id,
                solar_system_id: RANCER_SYS,
                name: "ALSC Locked Gantry".into(),
                position: near_planet.to_array(),
                state: "ShieldVulnerable".into(),
                ..Default::default()
            },
        );
        snap.items.insert(
            contract_gantry_id,
            InventoryItemRecord {
                item_id: contract_gantry_id,
                type_id: GANTRY_TYPE_ID,
                owner_id: corp_id,
                location_id: i64::from(RANCER_SYS),
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.structures.insert(
            contract_gantry_id,
            UpwellStructureRecord {
                structure_id: contract_gantry_id,
                type_id: GANTRY_TYPE_ID,
                owner_corp_id: corp_id,
                solar_system_id: RANCER_SYS,
                name: "Escrowed Gantry".into(),
                position: near_planet.to_array(),
                state: "ShieldVulnerable".into(),
                ..Default::default()
            },
        );
        snap.contracts.insert(
            890_999_001,
            eve_db::ContractRecord {
                contract_id: 890_999_001,
                issuer_id: PILOT1,
                contract_type: "ItemExchange".into(),
                status: "Outstanding".into(),
                start_station_id: 60_003_760,
                end_station_id: 60_003_760,
                item_ids: vec![contract_gantry_id],
                ..Default::default()
            },
        );
        snap.items.insert(
            skyhook_id,
            InventoryItemRecord {
                item_id: skyhook_id,
                type_id: 81_080, // Orbital Skyhook (8,000 m3)
                owner_id: corp_id,
                location_id: i64::from(RANCER_SYS),
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.structures.insert(
            skyhook_id,
            UpwellStructureRecord {
                structure_id: skyhook_id,
                type_id: 81_080,
                owner_corp_id: corp_id,
                solar_system_id: RANCER_SYS,
                name: format!("Orbital Skyhook:{}", planet.celestial_id),
                position: near_planet.to_array(),
                state: "ShieldVulnerable".into(),
                ..Default::default()
            },
        );
    });

    dir_sess.corporation_id = Some(corp_id);
    dir_sess.corp_role = eve_db::corp_roles::ROLE_DIRECTOR;
    dir_sess.solar_system_id = Some(RANCER_SYS);
    dir_sess.station_id = None;
    dir_sess.structure_id = None;

    let mut line_sess = SessionState::new(506);
    line_sess.user_id = Some(99_502);
    line_sess.character_id = Some(line_char_id);
    line_sess.corporation_id = Some(corp_id);
    line_sess.corp_role = 0;
    line_sess.solar_system_id = Some(RANCER_SYS);
    line_sess.ship_id = Some(ship_id);

    let mut outsider_sess = SessionState::new(507);
    outsider_sess.user_id = Some(99_503);
    outsider_sess.character_id = Some(outsider_char_id);
    outsider_sess.corporation_id = Some(other_corp_id);
    outsider_sess.corp_role = eve_db::corp_roles::ROLE_DIRECTOR;
    outsider_sess.solar_system_id = Some(RANCER_SYS);

    let sys_handle = ctx.systems.get_or_spawn(RANCER_SYS);
    for (bid, btid, bpos) in [
        (ship_id, 20185_u32, near_planet),
        (far_ship_id, 20185_u32, near_planet + glam::DVec3::new(12_000.0, 0.0, 0.0)),
        (small_ship_id, 587_u32, near_planet),
    ] {
        sys_handle
            .spawn_ball(eve_destiny::Ball {
                id: bid,
                type_id: btid,
                position: bpos,
                velocity: glam::DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: eve_destiny::MovementMode::Stop,
            })
            .await
            .expect("spawn test ship ball");
    }

    let bound_pos = ctx.bind_object(&dir_sess, "posMgr", i64::from(RANCER_SYS)).ok();

    // 1. Anchor Gantry (3962, 7,600 m3) and spawn orbital ball
    let anchored = wire_rpc(
        &ctx,
        &mut dir_sess,
        "posMgr",
        bound_pos,
        "AnchorOrbital",
        &[PyRep::Integer(gantry_id), PyRep::Integer(planet.celestial_id)],
    )
    .await
    .expect("AnchorOrbital gantry");
    assert_eq!(anchored.dict_get("state"), Some(&PyRep::String("Anchored".into())));

    sys_handle
        .spawn_ball(eve_destiny::Ball {
            id: gantry_id,
            type_id: GANTRY_TYPE_ID,
            position: near_planet,
            velocity: glam::DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 1e9,
            radius: 2_000.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        })
        .await
        .expect("spawn orbital ball");

    // 2. Negative authorization, type, lock, missing ship/ball, range, capacity, and state guards
    let non_orb_err = wire_rpc(
        &ctx,
        &mut dir_sess,
        "posMgr",
        bound_pos,
        "UnanchorOrbital",
        &[PyRep::Integer(non_orbital_id)],
    )
    .await
    .expect_err("non-orbital must fail");
    assert_eq!(non_orb_err.code, "InvalidOrbitalType");

    let hangar_err = wire_rpc(
        &ctx,
        &mut dir_sess,
        "posMgr",
        bound_pos,
        "UnanchorOrbital",
        &[PyRep::Integer(hangar_gantry_id)],
    )
    .await
    .expect_err("remote hangar unanchored gantry must fail");
    assert_eq!(hangar_err.code, "OrbitalNotAnchored");

    let locked_err = wire_rpc(
        &ctx,
        &mut dir_sess,
        "posMgr",
        bound_pos,
        "UnanchorOrbital",
        &[PyRep::Integer(locked_gantry_id)],
    )
    .await
    .expect_err("ALSC-locked gantry must fail");
    assert_eq!(locked_err.code, "ItemLocked");

    let contract_err = wire_rpc(
        &ctx,
        &mut dir_sess,
        "posMgr",
        bound_pos,
        "UnanchorOrbital",
        &[PyRep::Integer(contract_gantry_id)],
    )
    .await
    .expect_err("contract-escrow gantry must fail");
    assert_eq!(contract_err.code, "ItemLocked");

    let outsider_err = wire_rpc(
        &ctx,
        &mut outsider_sess,
        "posMgr",
        None,
        "UnanchorOrbital",
        &[PyRep::Integer(gantry_id)],
    )
    .await
    .expect_err("outsider corp must fail");
    assert_eq!(outsider_err.code, "NotOrbitalOwner");

    let line_err = wire_rpc(
        &ctx,
        &mut line_sess,
        "posMgr",
        None,
        "UnanchorOrbital",
        &[PyRep::Integer(gantry_id)],
    )
    .await
    .expect_err("corp line member without Director/StationManager must fail");
    assert_eq!(line_err.code, "RoleNotAuthorized");

    let mut no_ship_sess = dir_sess.clone();
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT1) {
            ch.active_ship_id = 0;
        }
    });
    no_ship_sess.ship_id = None;
    let no_ship_err = wire_rpc(
        &ctx,
        &mut no_ship_sess,
        "posMgr",
        None,
        "UnanchorOrbital",
        &[PyRep::Integer(gantry_id)],
    )
    .await
    .expect_err("missing active ship must fail");
    assert_eq!(no_ship_err.code, "NoActiveShip");

    let mut no_ball_sess = dir_sess.clone();
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT1) {
            ch.active_ship_id = no_ball_ship_id;
        }
    });
    no_ball_sess.ship_id = Some(no_ball_ship_id);
    let no_ball_err = wire_rpc(
        &ctx,
        &mut no_ball_sess,
        "posMgr",
        None,
        "UnanchorOrbital",
        &[PyRep::Integer(gantry_id)],
    )
    .await
    .expect_err("active ship without ball in space must fail");
    assert_eq!(no_ball_err.code, "ShipNotInSpace");

    let mut small_sess = dir_sess.clone();
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT1) {
            ch.active_ship_id = small_ship_id;
        }
    });
    small_sess.ship_id = Some(small_ship_id);
    let cap_err = wire_rpc(
        &ctx,
        &mut small_sess,
        "posMgr",
        None,
        "UnanchorOrbital",
        &[PyRep::Integer(gantry_id)],
    )
    .await
    .expect_err("small cargo hold (140 m3 < 7,600 m3 gantry) must fail");
    assert_eq!(cap_err.code, "NotEnoughCargoSpace");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT1) {
            ch.active_ship_id = ship_id;
        }
    });

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT1) {
            ch.active_ship_id = far_ship_id;
        }
    });
    let mut far_sess = dir_sess.clone();
    far_sess.ship_id = Some(far_ship_id);
    let range_err = wire_rpc(
        &ctx,
        &mut far_sess,
        "posMgr",
        None,
        "UnanchorOrbital",
        &[PyRep::Integer(gantry_id)],
    )
    .await
    .expect_err(">5000m out of range must fail");
    assert_eq!(range_err.code, "OrbitalOutOfRange");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT1) {
            ch.active_ship_id = ship_id;
        }
    });

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(st) = snap.structures.get_mut(&gantry_id) {
            st.state = "Reinforced".into();
        }
    });
    let reinf_err = wire_rpc(
        &ctx,
        &mut dir_sess,
        "posMgr",
        bound_pos,
        "UnanchorOrbital",
        &[PyRep::Integer(gantry_id)],
    )
    .await
    .expect_err("Reinforced orbital cannot be unanchored");
    assert_eq!(reinf_err.code, "InvalidOrbitalState");

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(st) = snap.structures.get_mut(&gantry_id) {
            st.state = "ShieldVulnerable".into();
        }
    });
    let _ = ctx.drain_notifications_for_char(PILOT1);

    // 3. Live UnanchorOrbital cleans structure state/fuel/notes, moves item to Charon cargo, emits OnItemChange, removes ball
    let mut dir_harness = common::borrow_wire_harness(&ctx, &mut dir_sess);
    let unanchored = dir_harness
        .call_ok(
            "posMgr",
            bound_pos,
            "UnanchorOrbital",
            &[PyRep::Integer(gantry_id)],
            &HashMap::new(),
        )
        .await;
    let wire_notifs = dir_harness.drain_notifications();
    assert!(
        wire_notifs.iter().any(|(ev, _)| ev == "OnItemChange"),
        "UnanchorOrbital must emit OnItemChange on the wire, got: {:?}",
        wire_notifs.iter().map(|(ev, _)| ev.as_str()).collect::<Vec<_>>()
    );
    drop(dir_harness);
    assert_eq!(unanchored.dict_get("itemID"), Some(&PyRep::Integer(gantry_id)));
    assert_eq!(unanchored.dict_get("typeID"), Some(&PyRep::Integer(i64::from(GANTRY_TYPE_ID))));
    assert_eq!(unanchored.dict_get("ownerID"), Some(&PyRep::Integer(PILOT1)));
    assert_eq!(unanchored.dict_get("state"), Some(&PyRep::String("Unanchored".into())));

    assert!(
        ctx.systems.get_ball_position_sync(RANCER_SYS, gantry_id).is_none(),
        "UnanchorOrbital must remove orbital ball from solar system"
    );
    ctx.db.with_snapshot(|snap| {
        assert!(
            !snap.structures.contains_key(&gantry_id),
            "unanchored gantry structure record must be removed from snap.structures"
        );
        let note_id = eve_server::services::industry_pi::orbital_settings_note_id(gantry_id);
        assert!(!snap.owner_notes.contains_key(&note_id));
        let it = snap.items.get(&gantry_id).expect("unanchored item");
        assert_eq!(it.location_id, ship_id);
        assert_eq!(it.flag, InventoryFlag::Cargo);
        assert_eq!(it.owner_id, PILOT1);
        assert!(!it.singleton);
    });

    // Double unanchor rejected
    let repeat_err = wire_rpc(
        &ctx,
        &mut dir_sess,
        "posMgr",
        bound_pos,
        "UnanchorOrbital",
        &[PyRep::Integer(gantry_id)],
    )
    .await
    .expect_err("already Unanchored orbital cannot unanchor twice");
    assert_eq!(repeat_err.code, "InvalidOrbitalState");

    // Orbital Skyhook (81080, group 4736) is NOT handled by legacy posMgr.UnanchorOrbital
    let skyhook_err = wire_rpc(
        &ctx,
        &mut dir_sess,
        "posMgr",
        bound_pos,
        "UnanchorOrbital",
        &[PyRep::Integer(skyhook_id)],
    )
    .await
    .expect_err("posMgr.UnanchorOrbital must reject group 4736 Orbital Skyhook");
    assert_eq!(skyhook_err.code, "InvalidOrbitalType");
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.structures[&skyhook_id].state, "ShieldVulnerable");
        assert_eq!(snap.items[&skyhook_id].location_id, i64::from(RANCER_SYS));
    });

    // 4. Re-anchoring unanchored gantry + OnlineOrbital upgrade to POCO (2233, 100M m3 -> NotEnoughCargoSpace)
    let anchored2 = wire_rpc(
        &ctx,
        &mut dir_sess,
        "posMgr",
        bound_pos,
        "AnchorOrbital",
        &[PyRep::Integer(gantry2_id), PyRep::Integer(planet.celestial_id)],
    )
    .await
    .expect("AnchorOrbital on planet after gantry unanchored");
    assert_eq!(anchored2.dict_get("state"), Some(&PyRep::String("Anchored".into())));

    wire_rpc(
        &ctx,
        &mut dir_sess,
        "posMgr",
        bound_pos,
        "UnanchorOrbital",
        &[PyRep::Integer(gantry2_id)],
    )
    .await
    .expect("UnanchorOrbital on second gantry");

    let reanchored2 = wire_rpc(
        &ctx,
        &mut dir_sess,
        "posMgr",
        bound_pos,
        "AnchorOrbital",
        &[PyRep::Integer(gantry2_id), PyRep::Integer(planet.celestial_id)],
    )
    .await
    .expect("Re-anchor unanchored gantry");
    assert_eq!(reanchored2.dict_get("state"), Some(&PyRep::String("Anchored".into())));

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(u) = snap.items.get_mut(&poco_upgrade_id) {
            u.location_id = ship_id;
            u.flag = InventoryFlag::Cargo;
        }
    });
    let onlined = wire_rpc(
        &ctx,
        &mut dir_sess,
        "posMgr",
        bound_pos,
        "OnlineOrbital",
        &[PyRep::Integer(gantry2_id), PyRep::Integer(poco_upgrade_id)],
    )
    .await
    .expect("OnlineOrbital to POCO");
    assert_eq!(onlined.dict_get("typeID"), Some(&PyRep::Integer(i64::from(POCO_TYPE_ID))));
    assert_eq!(onlined.dict_get("state"), Some(&PyRep::String("ShieldVulnerable".into())));

    // Full Customs Office (2233) has 100,000,000 m3 SDE volume and cannot fit in Charon cargo (465,000 m3)
    assert_eq!(
        ctx.db.get_item(gantry2_id).unwrap().owner_id,
        corp_id,
        "AnchorOrbital/OnlineOrbital must assign and preserve player corp ownership on orbital item"
    );
    let poco_cap_err = wire_rpc(
        &ctx,
        &mut dir_sess,
        "posMgr",
        bound_pos,
        "UnanchorOrbital",
        &[PyRep::Integer(gantry2_id)],
    )
    .await
    .expect_err("100M m3 Customs Office exceeds even Charon cargo hold");
    assert_eq!(poco_cap_err.code, "NotEnoughCargoSpace");

    // Ex-member / lost-roles regression (`BF-1277-R2-01`): anchor gantry at second planet so volume fits,
    // verify ex-director (role loss or corp departure) cannot unanchor via posMgr or delete via planetMgr,
    // and verify POCO hangar cargo at planet_id is excluded from GetMyLaunchesDetails & DeleteLaunch.
    let planet2 = ctx
        .sde
        .celestials_in_system(RANCER_SYS)
        .into_iter()
        .find(|c| c.group_id == 7 && c.celestial_id != planet.celestial_id)
        .expect("second Rancer planet");
    let near_planet2 = planet2.position + glam::DVec3::new(planet2.radius + 5_000.0, 0.0, 0.0);
    let _ = sys_handle.try_set_ball_position(ship_id, near_planet2);
    let gantry3_anchored = wire_rpc(
        &ctx,
        &mut dir_sess,
        "posMgr",
        bound_pos,
        "AnchorOrbital",
        &[PyRep::Integer(gantry_id), PyRep::Integer(planet2.celestial_id)],
    )
    .await
    .expect("AnchorOrbital gantry for cross-corp security check");
    assert_eq!(gantry3_anchored.dict_get("state"), Some(&PyRep::String("Anchored".into())));
    assert_eq!(ctx.db.get_item(gantry_id).unwrap().owner_id, corp_id);

    let poco_hangar_stack_id = 890_555_301_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            poco_hangar_stack_id,
            InventoryItemRecord {
                item_id: poco_hangar_stack_id,
                type_id: 3645,
                owner_id: PILOT1,
                location_id: planet.celestial_id,
                flag: InventoryFlag::Hangar,
                quantity: 50,
                ..Default::default()
            },
        );
        if let Some(c) = snap.corporations.get_mut(&corp_id) {
            c.ceo_id = line_char_id;
            c.member_roles.insert(PILOT1, 0);
        }
    });
    // Stale dir_sess.corp_role = ROLE_DIRECTOR cannot bypass DB role revocation
    let revoked_err = wire_rpc(
        &ctx,
        &mut dir_sess,
        "posMgr",
        bound_pos,
        "UnanchorOrbital",
        &[PyRep::Integer(gantry_id)],
    )
    .await
    .expect_err("Member with revoked roles cannot unanchor corp gantry even with stale session roles");
    assert_eq!(revoked_err.code, "RoleNotAuthorized");

    // Pilot leaves Corp A (even keeping stale session.corporation_id or updating to NPC corp / rival Corp B)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT1) {
            ch.corporation_id = other_corp_id;
        }
    });
    let ex_member_stale_err = wire_rpc(
        &ctx,
        &mut dir_sess,
        "posMgr",
        bound_pos,
        "UnanchorOrbital",
        &[PyRep::Integer(gantry_id)],
    )
    .await
    .expect_err("Ex-member with stale session corp cannot unanchor former corp orbital");
    assert_eq!(ex_member_stale_err.code, "NotOrbitalOwner");

    dir_sess.corporation_id = Some(other_corp_id);
    let ex_member_poco_err = wire_rpc(
        &ctx,
        &mut dir_sess,
        "posMgr",
        bound_pos,
        "UnanchorOrbital",
        &[PyRep::Integer(gantry2_id)],
    )
    .await
    .expect_err("Ex-member in rival corp cannot unanchor former corp Customs Office");
    assert_eq!(ex_member_poco_err.code, "NotOrbitalOwner");

    // Even if an orbital item carried legacy personal owner_id == PILOT1 while st.owner_corp_id == corp_id,
    // ex-member cannot bypass corp ownership on UnanchorOrbital or delete it via planetMgr.DeleteLaunch.
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(it) = snap.items.get_mut(&gantry_id) {
            it.owner_id = PILOT1;
        }
    });
    let legacy_owner_bypass_err = wire_rpc(
        &ctx,
        &mut dir_sess,
        "posMgr",
        bound_pos,
        "UnanchorOrbital",
        &[PyRep::Integer(gantry_id)],
    )
    .await
    .expect_err("Legacy personal item.owner_id must not bypass st.owner_corp_id");
    assert_eq!(legacy_owner_bypass_err.code, "NotOrbitalOwner");

    let launches_rep = rpc(&ctx, &mut dir_sess, "planetMgr", "GetMyLaunchesDetails", &[])
        .await
        .expect("GetMyLaunchesDetails");
    let PyRep::List(launch_items) = launches_rep else {
        panic!("expected List from GetMyLaunchesDetails");
    };
    assert!(
        launch_items.is_empty(),
        "Anchored orbitals and POCO Hangar stacks must not appear in GetMyLaunchesDetails: {launch_items:?}"
    );
    for (target_id, label) in [
        (gantry_id, "anchored gantry even with personal item owner"),
        (poco_hangar_stack_id, "POCO hangar commodity stack"),
    ] {
        let del_err = rpc(&ctx, &mut dir_sess, "planetMgr", "DeleteLaunch", &[PyRep::Integer(target_id)])
            .await
            .expect_err(&format!("planetMgr.DeleteLaunch must reject {label}"));
        assert_eq!(del_err.code, "InvalidLaunchID", "{label}");
    }
    let del_corp_poco_err = rpc(&ctx, &mut dir_sess, "planetMgr", "DeleteLaunch", &[PyRep::Integer(gantry2_id)])
        .await
        .expect_err("planetMgr.DeleteLaunch must reject corp Customs Office");
    assert_eq!(del_corp_poco_err.code, "LaunchAccessDenied");
}

#[tokio::test]
async fn test_pos_mgr_anchor_orbital_stacked_and_locked_gantry() {
    const RANCER_SYS: u32 = 30_002_718;
    const GANTRY_TYPE_ID: u32 = 3962;

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let planet = ctx
        .sde
        .celestials_in_system(RANCER_SYS)
        .into_iter()
        .find(|c| c.group_id == 7)
        .expect("Rancer planet");
    let near_planet = planet.position + glam::DVec3::new(planet.radius + 5_000.0, 0.0, 0.0);

    let mut dir_sess = setup_pilot(&ctx, 508, "demo", "demo-secret", PILOT1).await;
    let ship_id = dir_sess.ship_id.expect("Active ship");
    let corp_id = 98_556_001_i64;
    let stack_gantry_id = 890_556_201_i64;
    let locked_flag_gantry_id = 890_556_202_i64;
    let contract_locked_gantry_id = 890_556_203_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut corp = eve_db::CorporationRecord::default();
        corp.corporation_id = corp_id;
        corp.name = "Stacked Orbital Corp".into();
        corp.ticker = "STCK".into();
        corp.ceo_id = PILOT1;
        snap.corporations.insert(corp_id, corp);

        if let Some(ch) = snap.characters.get_mut(&PILOT1) {
            ch.corporation_id = corp_id;
            ch.solar_system_id = RANCER_SYS;
            ch.station_id = None;
            ch.structure_id = None;
        }
        if let Some(sh) = snap.items.get_mut(&ship_id) {
            sh.type_id = 20185; // Charon freighter (465,000 m3 cargo)
            sh.location_id = i64::from(RANCER_SYS);
            sh.flag = InventoryFlag::None;
            sh.singleton = true;
        }
        snap.items.insert(
            stack_gantry_id,
            InventoryItemRecord {
                item_id: stack_gantry_id,
                type_id: GANTRY_TYPE_ID,
                owner_id: PILOT1,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 2,
                singleton: false,
                ..Default::default()
            },
        );
        snap.items.insert(
            locked_flag_gantry_id,
            InventoryItemRecord {
                item_id: locked_flag_gantry_id,
                type_id: GANTRY_TYPE_ID,
                owner_id: PILOT1,
                location_id: ship_id,
                flag: InventoryFlag::Locked,
                quantity: 2,
                singleton: false,
                ..Default::default()
            },
        );
        snap.items.insert(
            contract_locked_gantry_id,
            InventoryItemRecord {
                item_id: contract_locked_gantry_id,
                type_id: GANTRY_TYPE_ID,
                owner_id: PILOT1,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 2,
                singleton: false,
                ..Default::default()
            },
        );
        snap.contracts.insert(
            890_556_901,
            eve_db::ContractRecord {
                contract_id: 890_556_901,
                issuer_id: PILOT1,
                contract_type: "ItemExchange".into(),
                status: "Outstanding".into(),
                start_station_id: 60_003_760,
                end_station_id: 60_003_760,
                item_ids: vec![contract_locked_gantry_id],
                ..Default::default()
            },
        );
    });

    dir_sess.corporation_id = Some(corp_id);
    dir_sess.corp_role = eve_db::corp_roles::ROLE_DIRECTOR;
    dir_sess.solar_system_id = Some(RANCER_SYS);
    dir_sess.station_id = None;
    dir_sess.structure_id = None;

    let sys_handle = ctx.systems.get_or_spawn(RANCER_SYS);
    sys_handle
        .spawn_ball(eve_destiny::Ball {
            id: ship_id,
            type_id: 20185,
            position: near_planet,
            velocity: glam::DVec3::ZERO,
            max_velocity: 100.0,
            inertia: 1.0,
            mass: 1e8,
            radius: 500.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        })
        .await
        .expect("spawn ship ball");

    let bound_pos = Some(
        ctx.bind_object(&dir_sess, "posMgr", i64::from(RANCER_SYS))
            .expect("bind posMgr"),
    );

    // Locked flag or contract-locked stack must be rejected with ItemLocked without decrementing quantity.
    for (locked_id, label) in [
        (locked_flag_gantry_id, "Locked flag gantry stack"),
        (contract_locked_gantry_id, "Contract-locked gantry stack"),
    ] {
        let locked_err = wire_rpc(
            &ctx,
            &mut dir_sess,
            "posMgr",
            bound_pos,
            "AnchorOrbital",
            &[PyRep::Integer(locked_id), PyRep::Integer(planet.celestial_id)],
        )
        .await
        .expect_err(&format!("{label} must fail AnchorOrbital"));
        assert_eq!(locked_err.code, "ItemLocked", "{label}");
        assert_eq!(ctx.db.get_item(locked_id).unwrap().quantity, 2);
    }

    // Anchoring a stacked Customs Office Gantry (quantity == 2) splits 1 singleton unit off the stack.
    let _ = ctx.drain_notifications_for_char(PILOT1);
    let mut dir_harness = common::borrow_wire_harness(&ctx, &mut dir_sess);
    let anchored = dir_harness
        .call_ok(
            "posMgr",
            bound_pos,
            "AnchorOrbital",
            &[PyRep::Integer(stack_gantry_id), PyRep::Integer(planet.celestial_id)],
            &HashMap::new(),
        )
        .await;
    let anchor_notifs = dir_harness.drain_notifications();
    drop(dir_harness);
    assert!(
        anchor_notifs.iter().any(|(ev, _)| ev == "OnItemChange"),
        "Stacked AnchorOrbital must emit OnItemChange, got: {:?}",
        anchor_notifs.iter().map(|(ev, _)| ev.as_str()).collect::<Vec<_>>()
    );

    let orbital_id = anchored
        .dict_get("orbitalID")
        .and_then(|v| v.as_i64())
        .expect("orbitalID in AnchorOrbital reply");
    assert_eq!(
        anchored.dict_get("itemID").and_then(|v| v.as_i64()),
        Some(orbital_id)
    );
    assert_ne!(
        orbital_id, stack_gantry_id,
        "Anchoring from a quantity > 1 stack must allocate a new singleton orbitalID"
    );

    ctx.db.with_snapshot(|snap| {
        let cargo_stack = snap.items.get(&stack_gantry_id).expect("cargo stack remains");
        assert_eq!(cargo_stack.location_id, ship_id);
        assert_eq!(cargo_stack.flag, InventoryFlag::Cargo);
        assert_eq!(cargo_stack.owner_id, PILOT1);
        assert_eq!(cargo_stack.quantity, 1);
        assert!(!cargo_stack.singleton);
        assert!(!snap.structures.contains_key(&stack_gantry_id));

        let orbital_item = snap.items.get(&orbital_id).expect("spawned orbital item");
        assert_eq!(orbital_item.owner_id, corp_id);
        assert_eq!(orbital_item.location_id, planet.celestial_id);
        assert_eq!(orbital_item.flag, InventoryFlag::None);
        assert_eq!(orbital_item.quantity, 1);
        assert!(orbital_item.singleton);

        let st = snap.structures.get(&orbital_id).expect("anchored orbital structure");
        assert_eq!(st.owner_corp_id, corp_id);
        assert_eq!(st.state, "ShieldVulnerable");
    });

    // Newly split orbital_id can subsequently be unanchored cleanly back into ship cargo.
    let unanchored = wire_rpc(
        &ctx,
        &mut dir_sess,
        "posMgr",
        bound_pos,
        "UnanchorOrbital",
        &[PyRep::Integer(orbital_id)],
    )
    .await
    .expect("Unanchor split orbital_id");
    assert_eq!(unanchored.dict_get("itemID"), Some(&PyRep::Integer(orbital_id)));
    assert_eq!(unanchored.dict_get("state"), Some(&PyRep::String("Unanchored".into())));
    ctx.db.with_snapshot(|snap| {
        assert!(
            !snap.structures.contains_key(&orbital_id),
            "unanchored split orbital must be removed from snap.structures"
        );
        let unanchored_item = snap.items.get(&orbital_id).expect("unanchored item");
        assert_eq!(unanchored_item.location_id, ship_id);
        assert_eq!(unanchored_item.flag, InventoryFlag::Cargo);
        assert_eq!(unanchored_item.owner_id, PILOT1);
        assert_eq!(unanchored_item.quantity, 1);
        assert!(!unanchored_item.singleton);
    });
}

#[tokio::test]
async fn test_pos_mgr_unanchor_orbital_clears_war_directory_search_clearance_and_pi() {
    use eve_server::services::industry_pi::pi_helpers::resolve_orbital_and_planet;
    use eve_server::services::station_structure::structure_lifecycle::is_in_space_structure_state;

    const RANCER_SYS: u32 = 30_002_718;
    const PERIMETER_SYS: u32 = 30_000_144;
    const ASTRAHUS_TYPE_ID: u32 = 35_832;

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let celestials = ctx.sde.celestials_in_system(RANCER_SYS);
    let (planet, clear_pos) = celestials
        .iter()
        .filter(|c| c.group_id == 7)
        .find_map(|p| {
            let offset_d = p.radius + 5_000_000.0;
            [
                glam::DVec3::new(0.0, offset_d, 0.0),
                glam::DVec3::new(offset_d, 0.0, 0.0),
                glam::DVec3::new(-offset_d, 0.0, 0.0),
            ]
            .into_iter()
            .find_map(|off| {
                let pos = p.position + off;
                let stn_clear = ctx
                    .sde
                    .stations
                    .values()
                    .filter(|s| s.solar_system_id == RANCER_SYS)
                    .all(|s| s.position.distance(pos) >= 1_500_000.0);
                let cel_clear = celestials
                    .iter()
                    .filter(|c| matches!(c.group_id, 7 | 8 | 9 | 10))
                    .all(|c| (c.position.distance(pos) - c.radius).max(0.0) >= 1_500_000.0);
                (stn_clear && cel_clear).then_some((p.clone(), pos))
            })
        })
        .expect("Rancer planet with clear Upwell anchor offset within orbital range");

    let mut def_sess = setup_pilot(&ctx, 508, "demo", "demo-secret", PILOT1).await;
    let def_ship_id = def_sess.ship_id.expect("PILOT1 active ship");
    let def_corp = 98_557_001_i64;
    let agg_char = 90_557_002_i64;
    let agg_corp = 98_557_002_i64;
    let agg_hq_id = 1_090_557_900_i64;
    let agg_ship_id = 890_557_202_i64;
    let gantry_item_id = 890_557_101_i64;
    let astra_item_id = 890_557_301_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut base_char = snap.characters.get(&PILOT1).cloned().expect("PILOT1");
        if let Some(ch) = snap.characters.get_mut(&PILOT1) {
            ch.corporation_id = def_corp;
            ch.solar_system_id = RANCER_SYS;
            ch.station_id = None;
            ch.structure_id = None;
        }
        base_char.character_id = agg_char;
        base_char.user_id = 509;
        base_char.name = "Agg Unanchor Pilot".into();
        base_char.corporation_id = agg_corp;
        base_char.solar_system_id = RANCER_SYS;
        base_char.station_id = None;
        base_char.structure_id = None;
        base_char.active_ship_id = agg_ship_id;
        snap.characters.insert(agg_char, base_char);

        let mut d_corp = eve_db::CorporationRecord::default();
        d_corp.corporation_id = def_corp;
        d_corp.name = "DefUnanchorWarCorp".into();
        d_corp.ticker = "DUWC".into();
        d_corp.ceo_id = PILOT1;
        d_corp.hq_station_id = Some(60_003_760);
        d_corp.members = [PILOT1].into_iter().collect();
        d_corp.member_roles = [(PILOT1, u64::MAX)].into_iter().collect();
        snap.corporations.insert(def_corp, d_corp);

        let mut a_corp = eve_db::CorporationRecord::default();
        a_corp.corporation_id = agg_corp;
        a_corp.name = "AggUnanchorWarCorp".into();
        a_corp.ticker = "AUWC".into();
        a_corp.ceo_id = agg_char;
        a_corp.hq_station_id = Some(60_003_760);
        a_corp.members = [agg_char].into_iter().collect();
        a_corp.member_roles = [(agg_char, u64::MAX)].into_iter().collect();
        a_corp.set_division_balance(1, eve_db::IskCents::from_isk(1_000));
        a_corp.ensure_wallet_divisions();
        snap.corporations.insert(agg_corp, a_corp);

        snap.structures.insert(
            agg_hq_id,
            UpwellStructureRecord {
                structure_id: agg_hq_id,
                type_id: ASTRAHUS_TYPE_ID,
                owner_corp_id: agg_corp,
                solar_system_id: PERIMETER_SYS,
                name: "AUWC Perimeter HQ".into(),
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                hull_hp: 500_000.0,
                ..Default::default()
            },
        );

        if let Some(sh) = snap.items.get_mut(&def_ship_id) {
            sh.type_id = 20185; // Charon freighter
            sh.location_id = i64::from(RANCER_SYS);
            sh.flag = InventoryFlag::None;
            sh.singleton = true;
        }
        snap.items.insert(
            agg_ship_id,
            InventoryItemRecord {
                item_id: agg_ship_id,
                type_id: 20185,
                owner_id: agg_char,
                location_id: i64::from(RANCER_SYS),
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            gantry_item_id,
            InventoryItemRecord {
                item_id: gantry_item_id,
                type_id: GANTRY_TYPE_ID,
                owner_id: PILOT1,
                location_id: def_ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            astra_item_id,
            InventoryItemRecord {
                item_id: astra_item_id,
                type_id: ASTRAHUS_TYPE_ID,
                owner_id: agg_char,
                location_id: agg_ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    def_sess.corporation_id = Some(def_corp);
    def_sess.corp_role = u64::MAX;
    def_sess.solar_system_id = Some(RANCER_SYS);
    def_sess.station_id = None;
    def_sess.structure_id = None;

    let mut agg_sess = SessionState::new(509);
    agg_sess.user_id = Some(509);
    agg_sess.character_id = Some(agg_char);
    agg_sess.corporation_id = Some(agg_corp);
    agg_sess.corp_role = u64::MAX;
    agg_sess.solar_system_id = Some(RANCER_SYS);
    agg_sess.ship_id = Some(agg_ship_id);

    let sys_handle = ctx.systems.get_or_spawn(RANCER_SYS);
    for (sid, owner_pos) in [(def_ship_id, clear_pos), (agg_ship_id, clear_pos)] {
        sys_handle
            .spawn_ball(eve_destiny::Ball {
                id: sid,
                type_id: 20185,
                position: owner_pos,
                velocity: glam::DVec3::ZERO,
                max_velocity: 100.0,
                inertia: 1.0,
                mass: 1e8,
                radius: 500.0,
                speed_fraction: 0.0,
                mode: eve_destiny::MovementMode::Stop,
            })
            .await
            .expect("spawn freighter ball");
    }

    let bound_pos = Some(
        ctx.bind_object(&def_sess, "posMgr", i64::from(RANCER_SYS))
            .expect("bind posMgr"),
    );
    let anchored = wire_rpc(
        &ctx,
        &mut def_sess,
        "posMgr",
        bound_pos,
        "AnchorOrbital",
        &[PyRep::Integer(gantry_item_id), PyRep::Integer(planet.celestial_id)],
    )
    .await
    .expect("AnchorOrbital");
    let orbital_id = anchored
        .dict_get("orbitalID")
        .and_then(|v| v.as_i64())
        .expect("orbitalID");
    let orbital_name = format!("Orbital:{}", planet.celestial_id);

    // Before unanchoring: gantry makes def_corp war-eligible, visible in directory/search,
    // blocks Upwell anchoring within 1,000 km, and resolves via pi_helpers.
    let warable_before = rpc(
        &ctx,
        &mut agg_sess,
        "lookupSvc",
        "LookupWarableCorporationsOrAlliances",
        &[PyRep::String("DefUnanchorWarCorp".into()), PyRep::Bool(false)],
    )
    .await
    .expect("LookupWarableCorporationsOrAlliances before");
    assert!(matches!(
        &warable_before,
        PyRep::List(entries) if entries.iter().any(|e| e.dict_get("ownerID") == Some(&PyRep::Integer(def_corp)))
    ));
    let war_err_before = rpc(
        &ctx,
        &mut agg_sess,
        "warRegistry",
        "DeclareWarAgainst",
        &[PyRep::Integer(def_corp)],
    )
    .await
    .expect_err("DeclareWarAgainst with <100M ISK still passes war-eligibility before unanchor");
    assert_eq!(war_err_before.code, "InsufficientFunds");

    let dir_before = rpc(&ctx, &mut def_sess, "structureDirectory", "GetStructures", &[])
        .await
        .expect("GetStructures before");
    assert!(matches!(
        &dir_before,
        PyRep::Dict(entries) if entries.iter().any(|(k, _)| k == &PyRep::Integer(orbital_id))
    ));
    let info_before = rpc(
        &ctx,
        &mut def_sess,
        "structureDirectory",
        "GetStructureInfo",
        &[PyRep::Integer(orbital_id)],
    )
    .await
    .expect("GetStructureInfo before");
    assert_ne!(info_before, PyRep::None);

    let search_before = rpc(
        &ctx,
        &mut def_sess,
        "search",
        "QuickQuery",
        &[PyRep::String(orbital_name.clone()), PyRep::List(vec![PyRep::Integer(1657)])],
    )
    .await
    .expect("search.QuickQuery before");
    assert!(matches!(
        &search_before,
        PyRep::List(ids) if ids.contains(&PyRep::Integer(orbital_id))
    ));

    let too_close_err = rpc(
        &ctx,
        &mut agg_sess,
        "structureDeployment",
        "Anchor",
        &[PyRep::Integer(astra_item_id)],
    )
    .await
    .expect_err("Upwell structure cannot anchor at anchored gantry coordinates");
    assert_eq!(too_close_err.code, "TooCloseToStructureOrStation");

    ctx.db.with_snapshot(|snap| {
        assert_eq!(resolve_orbital_and_planet(snap, orbital_id), (Some(orbital_id), planet.celestial_id));
        assert_eq!(
            resolve_orbital_and_planet(snap, planet.celestial_id),
            (Some(orbital_id), planet.celestial_id)
        );
    });

    // Unanchor gantry back into ship cargo.
    wire_rpc(
        &ctx,
        &mut def_sess,
        "posMgr",
        bound_pos,
        "UnanchorOrbital",
        &[PyRep::Integer(orbital_id)],
    )
    .await
    .expect("UnanchorOrbital");

    // Raise attacker wallet so war declaration only checks target war-eligibility.
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.corporations.get_mut(&agg_corp) {
            c.set_division_balance(1, eve_db::IskCents::from_isk(500_000_000));
            c.wallet_divisions.clear();
            c.ensure_wallet_divisions();
        }
    });

    // After unanchoring: gantry is removed from snap.structures & all 4 subsystems exclude it.
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.structures.contains_key(&orbital_id));
        assert!(!snap.sp6_lifecycle.space_object_meta.contains_key(&orbital_id));
        assert_eq!(resolve_orbital_and_planet(snap, orbital_id), (None, orbital_id));
        assert_eq!(
            resolve_orbital_and_planet(snap, planet.celestial_id),
            (None, planet.celestial_id)
        );
    });

    let warable_after = rpc(
        &ctx,
        &mut agg_sess,
        "lookupSvc",
        "LookupWarableCorporationsOrAlliances",
        &[PyRep::String("DefUnanchorWarCorp".into()), PyRep::Bool(false)],
    )
    .await
    .expect("LookupWarableCorporationsOrAlliances after");
    assert!(!matches!(
        &warable_after,
        PyRep::List(entries) if entries.iter().any(|e| e.dict_get("ownerID") == Some(&PyRep::Integer(def_corp)))
    ));
    let war_err_after = rpc(
        &ctx,
        &mut agg_sess,
        "warRegistry",
        "DeclareWarAgainst",
        &[PyRep::Integer(def_corp)],
    )
    .await
    .expect_err("Corp whose only orbital was unanchored must not be war-eligible");
    assert_eq!(war_err_after.code, "DefenderNotWarEligible");

    let dir_after = rpc(&ctx, &mut def_sess, "structureDirectory", "GetStructures", &[])
        .await
        .expect("GetStructures after");
    assert!(!matches!(
        &dir_after,
        PyRep::Dict(entries) if entries.iter().any(|(k, _)| k == &PyRep::Integer(orbital_id))
    ));
    let info_after = rpc(
        &ctx,
        &mut def_sess,
        "structureDirectory",
        "GetStructureInfo",
        &[PyRep::Integer(orbital_id)],
    )
    .await
    .expect("GetStructureInfo after");
    assert_eq!(info_after, PyRep::None);

    let search_after = rpc(
        &ctx,
        &mut def_sess,
        "search",
        "QuickQuery",
        &[PyRep::String(orbital_name.clone()), PyRep::List(vec![PyRep::Integer(1657)])],
    )
    .await
    .expect("search.QuickQuery after");
    assert!(!matches!(
        &search_after,
        PyRep::List(ids) if ids.contains(&PyRep::Integer(orbital_id))
    ));

    // Verify defensive `is_in_space_structure_state` guards reject a synthetic stale `"Unanchored"`
    // record across PI resolution, war eligibility, structure directory, search, AND Upwell anchoring!
    assert!(!is_in_space_structure_state("Unanchored"));
    assert!(!is_in_space_structure_state("unanchored"));
    assert!(!is_in_space_structure_state("Destroyed"));
    assert!(is_in_space_structure_state("ShieldVulnerable"));
    let stale_id = 890_557_999_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            stale_id,
            UpwellStructureRecord {
                structure_id: stale_id,
                type_id: GANTRY_TYPE_ID,
                owner_corp_id: def_corp,
                solar_system_id: RANCER_SYS,
                name: orbital_name.clone(),
                position: clear_pos.to_array(),
                state: "Unanchored".into(),
                quantum_core_installed: false,
                ..Default::default()
            },
        );
    });
    ctx.db.with_snapshot(|snap| {
        assert_eq!(resolve_orbital_and_planet(snap, stale_id), (None, stale_id));
        assert_eq!(
            resolve_orbital_and_planet(snap, planet.celestial_id),
            (None, planet.celestial_id)
        );
    });
    let warable_stale = rpc(
        &ctx,
        &mut agg_sess,
        "lookupSvc",
        "LookupWarableCorporationsOrAlliances",
        &[PyRep::String("DefUnanchorWarCorp".into()), PyRep::Bool(false)],
    )
    .await
    .expect("LookupWarableCorporationsOrAlliances stale");
    assert!(!matches!(
        &warable_stale,
        PyRep::List(entries) if entries.iter().any(|e| e.dict_get("ownerID") == Some(&PyRep::Integer(def_corp)))
    ));
    let war_err_stale = rpc(
        &ctx,
        &mut agg_sess,
        "warRegistry",
        "DeclareWarAgainst",
        &[PyRep::Integer(def_corp)],
    )
    .await
    .expect_err("Synthetic Unanchored structure must not grant war-eligibility");
    assert_eq!(war_err_stale.code, "DefenderNotWarEligible");
    let info_stale = rpc(
        &ctx,
        &mut def_sess,
        "structureDirectory",
        "GetStructureInfo",
        &[PyRep::Integer(stale_id)],
    )
    .await
    .expect("GetStructureInfo stale");
    assert_eq!(info_stale, PyRep::None);
    let search_stale = rpc(
        &ctx,
        &mut def_sess,
        "search",
        "QuickQuery",
        &[PyRep::String(orbital_name), PyRep::List(vec![PyRep::Integer(1657)])],
    )
    .await
    .expect("search.QuickQuery stale");
    assert!(!matches!(
        &search_stale,
        PyRep::List(ids) if ids.contains(&PyRep::Integer(stale_id))
    ));

    // Even with the synthetic `"Unanchored"` record present at `clear_pos`, Upwell anchoring succeeds!
    let anchor_after = rpc(
        &ctx,
        &mut agg_sess,
        "structureDeployment",
        "Anchor",
        &[PyRep::Integer(astra_item_id)],
    )
    .await
    .expect("Upwell structure must be anchorable once gantry is unanchored");
    assert!(anchor_after.dict_get("structureID").and_then(|v| v.as_i64()).is_some());
}
