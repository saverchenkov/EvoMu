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

    let mut sess = setup_pilot(&ctx, 501, "demo", "demo-secret", PILOT1).await;
    sess.corp_role = eve_db::corp_roles::ROLE_DIRECTOR;
    let tower_id = 140_000_101_i64;
    seed_test_structure(&ctx, tower_id, 12235, 1_000_170, SYS_LOWSEC, "Test Control Tower", "Online");

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
    no_role_sess.corp_role = 0;
    let role_err = rpc(
        &ctx,
        &mut no_role_sess,
        "posMgr",
        "SetTowerNotifications",
        &[PyRep::Integer(tower_id), PyRep::Bool(true), PyRep::Bool(true)],
    ).await.expect_err("Member without director/operator role must fail");
    assert_eq!(role_err.code, "RoleNotAuthorized");
}

#[tokio::test]
async fn test_pos_mgr_moon_silo_and_orbital() {
    // parity: rpc:posMgr.SetTowerPassword, rpc:posMgr.GetConsumptionFactor, rpc:posMgr.GetSiloCapacityByItemID, rpc:posMgr.LinkResourceForTower, rpc:posMgr.ChangeStructureProvisionType, rpc:posMgr.UnanchorOrbital
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let mut sess = setup_pilot(&ctx, 502, "demo", "demo-secret", PILOT1).await;
    sess.corp_role = eve_db::corp_roles::ROLE_DIRECTOR;
    let tower_id = 140_000_102_i64;
    let silo_id = 140_000_201_i64;
    let orbital_id = 140_000_301_i64;
    seed_test_structure(&ctx, tower_id, 12235, 1_000_170, SYS_LOWSEC, "Moon Tower", "Online");

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
        let corp = snap.corporations.entry(1_000_170).or_default();
        corp.alliance_id = Some(99_000_001);
        snap.system_states.entry(SYS_LOWSEC).or_default().sov_alliance_id = Some(99_000_001);
    });
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

    // 5. posMgr.RunMoonProcessCycleforTower returns UnimplementedRPC
    let cycle_res = rpc(
        &ctx,
        &mut sess,
        "posMgr",
        "RunMoonProcessCycleforTower",
        &[PyRep::Integer(tower_id)],
    )
    .await
    .expect_err("posMgr.RunMoonProcessCycleforTower must return UnimplementedRPC");
    assert_eq!(cycle_res.code, "UnimplementedRPC");

    // 6. posMgr.ChangeStructureProvisionType
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

    // 7. posMgr.UnanchorOrbital
    // Seed anchored Customs Office orbital in low-sec space
    seed_test_item(
        &ctx,
        orbital_id,
        POCO_TYPE_ID,
        PILOT1,
        i64::from(SYS_LOWSEC),
        InventoryFlag::None,
        1,
    );
    seed_test_structure(
        &ctx,
        orbital_id,
        POCO_TYPE_ID,
        1_000_035,
        SYS_LOWSEC,
        "Lowsec Customs Office",
        "ShieldVulnerable",
    );
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT1) {
            ch.solar_system_id = SYS_LOWSEC;
        }
    });
    sess.solar_system_id = Some(SYS_LOWSEC);

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

    let (st_state, it_flag) = ctx.db.with_snapshot(|snap| {
        let st = snap.structures.get(&orbital_id).map(|s| s.state.clone());
        let it = snap.items.get(&orbital_id).map(|i| i.flag);
        (st, it)
    });
    assert_eq!(st_state, Some("Unanchored".into()), "UnanchorOrbital must transition structure state to Unanchored");
    assert_eq!(it_flag, Some(InventoryFlag::Cargo), "UnanchorOrbital must transfer item to ship cargo hold");

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
}
