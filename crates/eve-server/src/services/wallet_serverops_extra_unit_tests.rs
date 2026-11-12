//! Deep branch-coverage and mutation-killing unit tests for `wallet_serverops`.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::wallet_serverops::*;
use eve_db::{
    corp_roles, CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents,
    UpwellStructureRecord, WorldRepository,
};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::{service_roles, SessionState};
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::SdeRegistry;
use glam::DVec3;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn test_ctx() -> ServerContext {
    if let Some(sde) = SHARED_SDE.get() {
        ServerContext::with_sde_and_repository(
            Arc::clone(sde),
            Arc::new(WorldRepository::with_demo_seed()),
        )
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
    } else {
        let ctx = ServerContext::new();
        let _ = SHARED_SDE.set(Arc::clone(&ctx.sde));
        ctx
    }
}

fn make_session(char_id: i64, corp_id: i64) -> SessionState {
    SessionState {
        user_id: Some(1),
        character_id: Some(char_id),
        corporation_id: Some(corp_id),
        solar_system_id: Some(30_000_142),
        station_id: Some(60_003_760),
        ..Default::default()
    }
}

fn call_ws(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let kw = HashMap::new();
    handle(ctx, sess, service, method, args, &kw).expect("wallet_serverops method should be handled")
}

#[test]
fn pos_mgr_moon_lookup_permissions_sentry_notifications_password_and_links() {
    let ctx = test_ctx();
    let ceo_id = 90_000_001_i64;
    let line_id = 90_000_002_i64;
    let outsider_id = 90_000_003_i64;
    let corp_id = 98_000_001_i64;
    let other_corp_id = 98_000_099_i64;
    let tower_id = 1_000_000_600_i64;
    let item_moon_loc = 1_000_000_601_i64;
    let item_moon_attr = 1_000_000_602_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let base = snap.characters[&ceo_id].clone();
        for (cid, c_id) in [(ceo_id, corp_id), (line_id, corp_id), (outsider_id, other_corp_id)] {
            let mut ch = base.clone();
            ch.character_id = cid;
            ch.corporation_id = c_id;
            snap.characters.insert(cid, ch);
        }
        snap.corporations.insert(
            corp_id,
            { let mut __corp = CorporationRecord::default();
                __corp.corporation_id = corp_id;
                __corp.name = "Starbase Corp".into();
                __corp.ticker = "SBC".into();
                __corp.ceo_id = ceo_id;
                __corp },
        );
        snap.structures.insert(
            tower_id,
            UpwellStructureRecord {
                structure_id: tower_id,
                type_id: 12235,
                owner_corp_id: corp_id,
                solar_system_id: 30_000_142,
                name: "Control Tower".into(),
                state: "Online".into(),
                ..Default::default()
            },
        );
        snap.items.insert(
            item_moon_loc,
            InventoryItemRecord {
                item_id: item_moon_loc,
                type_id: 12235,
                owner_id: corp_id,
                location_id: 40_009_082,
                flag: InventoryFlag::Unlocked,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        let mut dyn_attrs = HashMap::new();
        dyn_attrs.insert(600, 40_009_083.0);
        snap.items.insert(
            item_moon_attr,
            InventoryItemRecord {
                item_id: item_moon_attr,
                type_id: 12235,
                owner_id: ceo_id,
                location_id: 30_000_142,
                flag: InventoryFlag::Unlocked,
                quantity: 1,
                singleton: true,
                dynamic_attributes: dyn_attrs,
                ..Default::default()
            },
        );
    });

    let mut ceo_sess = make_session(ceo_id, corp_id);
    let mut line_sess = make_session(line_id, corp_id);
    let mut outsider_sess = make_session(outsider_id, other_corp_id);

    // 1. GetMoonForTower branches: <=0 -> None; item at moon location_id; item with attr 600; structure
    assert_eq!(
        call_ws(&ctx, &mut ceo_sess, "posMgr", "GetMoonForTower", &[PyRep::Integer(0)]).unwrap(),
        PyRep::None
    );
    assert_eq!(
        call_ws(&ctx, &mut ceo_sess, "posMgr", "GetMoonForTower", &[PyRep::Integer(item_moon_loc)]).unwrap(),
        PyRep::Integer(40_009_082)
    );
    assert_eq!(
        call_ws(&ctx, &mut ceo_sess, "posMgr", "GetMoonForTower", &[PyRep::Integer(item_moon_attr)]).unwrap(),
        PyRep::Integer(40_009_083)
    );
    let _ = call_ws(&ctx, &mut ceo_sess, "posMgr", "GetMoonForTower", &[PyRep::Integer(tower_id)]).unwrap();

    // 2. SetSessionCheck, AnchorOrbital, RunMoonProcessCycleforTower, GetSiloCapacityByItemID, GetConsumptionFactor (1.0)
    assert_eq!(call_ws(&ctx, &mut ceo_sess, "posMgr", "SetSessionCheck", &[]).unwrap(), PyRep::Bool(true));
    assert_eq!(call_ws(&ctx, &mut ceo_sess, "posMgr", "AnchorOrbital", &[]).unwrap_err().code, "MissingArgument");
    assert_eq!(
        call_ws(&ctx, &mut ceo_sess, "posMgr", "AnchorOrbital", &[PyRep::Integer(item_moon_attr)])
            .unwrap_err()
            .code,
        "StarbaseRemoved"
    );
    assert_eq!(
        call_ws(&ctx, &mut ceo_sess, "posMgr", "RunMoonProcessCycleforTower", &[]).unwrap_err().code,
        "MissingArgument"
    );
    assert!(matches!(
        call_ws(&ctx, &mut ceo_sess, "posMgr", "RunMoonProcessCycleforTower", &[PyRep::Integer(tower_id)]).unwrap(),
        PyRep::List(_)
    ));
    assert_eq!(
        call_ws(&ctx, &mut ceo_sess, "posMgr", "GetSiloCapacityByItemID", &[PyRep::Integer(999_999)])
            .unwrap()
            .as_f64(),
        Some(20_000.0)
    );
    assert_eq!(
        call_ws(&ctx, &mut ceo_sess, "posMgr", "GetConsumptionFactor", &[PyRep::Integer(tower_id)])
            .unwrap()
            .as_f64(),
        Some(1.0)
    );

    // 3. check_starbase_auth failures: StructureNotFound, NotStarbaseOwner, RoleNotAuthorized
    assert_eq!(
        call_ws(&ctx, &mut ceo_sess, "posMgr", "SetTowerPassword", &[PyRep::Integer(999_888_777), PyRep::String("pw".into())])
            .unwrap_err()
            .code,
        "StructureNotFound"
    );
    assert_eq!(
        call_ws(&ctx, &mut outsider_sess, "posMgr", "SetTowerPassword", &[PyRep::Integer(tower_id), PyRep::String("pw".into())])
            .unwrap_err()
            .code,
        "NotStarbaseOwner"
    );
    assert_eq!(
        call_ws(&ctx, &mut line_sess, "posMgr", "SetTowerPassword", &[PyRep::Integer(tower_id), PyRep::String("pw".into())])
            .unwrap_err()
            .code,
        "RoleNotAuthorized"
    );

    // AssumeStructureControl with non-owner ship in space without matching forcefield password -> ForcefieldPasswordMismatch
    outsider_sess.ship_id = Some(100_000_001);
    assert_eq!(
        call_ws(&ctx, &mut outsider_sess, "posMgr", "AssumeStructureControl", &[PyRep::Integer(tower_id)])
            .unwrap_err()
            .code,
        "ForcefieldPasswordMismatch"
    );

    // 4. Tower Notifications, Sentry Settings, Password, and InvalidLinkTarget / InvalidProvisionType
    assert!(call_ws(
        &ctx,
        &mut ceo_sess,
        "posMgr",
        "SetTowerNotifications",
        &[PyRep::Integer(tower_id), PyRep::Bool(true), PyRep::Bool(false)]
    )
    .is_ok());
    let notif = call_ws(&ctx, &mut ceo_sess, "posMgr", "GetTowerNotificationSettings", &[PyRep::Integer(tower_id)]).unwrap();
    assert!(format!("{notif:?}").contains("showInCalendar"));

    assert!(call_ws(
        &ctx,
        &mut ceo_sess,
        "posMgr",
        "SetTowerSentrySettings",
        &[
            PyRep::Integer(tower_id),
            PyRep::Real(5.0),
            PyRep::Real(-2.0),
            PyRep::Bool(true),
            PyRep::Bool(false),
            PyRep::Bool(true),
        ]
    )
    .is_ok());
    let sentry = call_ws(&ctx, &mut ceo_sess, "posMgr", "GetTowerSentrySettings", &[PyRep::Integer(tower_id)]).unwrap();
    assert!(format!("{sentry:?}").contains("5.0"));

    assert!(call_ws(
        &ctx,
        &mut ceo_sess,
        "posMgr",
        "SetTowerPassword",
        &[PyRep::Integer(tower_id), PyRep::String("secret123".into()), PyRep::Bool(true), PyRep::Bool(false)]
    )
    .is_ok());

    assert_eq!(
        call_ws(&ctx, &mut ceo_sess, "posMgr", "LinkResourceForTower", &[PyRep::Integer(tower_id), PyRep::Integer(999_888_777)])
            .unwrap_err()
            .code,
        "InvalidLinkTarget"
    );
    assert_eq!(
        call_ws(
            &ctx,
            &mut ceo_sess,
            "posMgr",
            "ChangeStructureProvisionType",
            &[PyRep::Integer(tower_id), PyRep::Integer(tower_id), PyRep::Integer(-1)]
        )
        .unwrap_err()
        .code,
        "InvalidTypeID"
    );
    assert_eq!(
        call_ws(
            &ctx,
            &mut ceo_sess,
            "posMgr",
            "ChangeStructureProvisionType",
            &[PyRep::Integer(tower_id), PyRep::Integer(tower_id), PyRep::Integer(34)]
        )
        .unwrap_err()
        .code,
        "InvalidProvisionType"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unanchor_orbital_distance_system_ownership_and_ship_cargo_branches() {
    let ctx = test_ctx();
    let ceo_id = 90_000_001_i64;
    let line_id = 90_000_002_i64;
    let outsider_id = 90_000_003_i64;
    let corp_id = 98_000_001_i64;
    let orbital_id = 1_000_000_650_i64;
    let ship_id = 100_000_001_i64;
    let sys_id = 30_000_142_u32;

    ctx.db.with_mut_snapshot(|snap| {
        let base = snap.characters[&ceo_id].clone();
        for (cid, c_id) in [(ceo_id, corp_id), (line_id, corp_id), (outsider_id, 98_000_099)] {
            let mut ch = base.clone();
            ch.character_id = cid;
            ch.corporation_id = c_id;
            snap.characters.insert(cid, ch);
        }
        snap.corporations.insert(
            corp_id,
            { let mut __corp = CorporationRecord::default();
                __corp.corporation_id = corp_id;
                __corp.name = "Orbital Corp".into();
                __corp.ticker = "ORB".into();
                __corp.ceo_id = ceo_id;
                __corp },
        );
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: ceo_id,
                location_id: i64::from(sys_id),
                flag: InventoryFlag::Unlocked,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            orbital_id,
            InventoryItemRecord {
                item_id: orbital_id,
                type_id: 2233,
                owner_id: corp_id,
                location_id: i64::from(sys_id),
                flag: InventoryFlag::Unlocked,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.structures.insert(
            orbital_id,
            UpwellStructureRecord {
                structure_id: orbital_id,
                type_id: 2233,
                owner_corp_id: corp_id,
                solar_system_id: 30_002_187, // Amarr initially to test NotInSameSystem
                name: "Customs Office".into(),
                position: [0.0, 0.0, 0.0],
                state: "Online".into(),
                ..Default::default()
            },
        );
    });

    let mut ceo_sess = make_session(ceo_id, corp_id);
    ceo_sess.ship_id = Some(ship_id);

    // 1. Missing arg -> InvalidOrbitalID; no solar_system_id -> NotInSpace; unknown id -> OrbitalNotFound
    assert_eq!(call_ws(&ctx, &mut ceo_sess, "posMgr", "UnanchorOrbital", &[]).unwrap_err().code, "InvalidOrbitalID");
    ceo_sess.solar_system_id = None;
    assert_eq!(
        call_ws(&ctx, &mut ceo_sess, "posMgr", "UnanchorOrbital", &[PyRep::Integer(orbital_id)]).unwrap_err().code,
        "NotInSpace"
    );
    ceo_sess.solar_system_id = Some(sys_id);
    assert_eq!(
        call_ws(&ctx, &mut ceo_sess, "posMgr", "UnanchorOrbital", &[PyRep::Integer(999_888_777)]).unwrap_err().code,
        "OrbitalNotFound"
    );

    // 2. Structure in different system -> NotInSameSystem
    assert_eq!(
        call_ws(&ctx, &mut ceo_sess, "posMgr", "UnanchorOrbital", &[PyRep::Integer(orbital_id)]).unwrap_err().code,
        "NotInSameSystem"
    );

    // Move structure to sys_id and spawn ship at 10,000 m (> 5,000 m) -> OrbitalOutOfRange
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.get_mut(&orbital_id).unwrap().solar_system_id = sys_id;
    });
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_handle
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 587,
            position: DVec3::new(10_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    assert_eq!(
        call_ws(&ctx, &mut ceo_sess, "posMgr", "UnanchorOrbital", &[PyRep::Integer(orbital_id)]).unwrap_err().code,
        "OrbitalOutOfRange"
    );

    // Move ship within 2,000 m; test NotOrbitalOwner and RoleNotAuthorized before CEO unanchors into ship Cargo
    let _ = sys_handle.try_set_ball_position(ship_id, DVec3::new(2_000.0, 0.0, 0.0));
    let mut outsider_sess = make_session(outsider_id, 98_000_099);
    assert_eq!(
        call_ws(&ctx, &mut outsider_sess, "posMgr", "UnanchorOrbital", &[PyRep::Integer(orbital_id)]).unwrap_err().code,
        "NotOrbitalOwner"
    );
    let mut line_sess = make_session(line_id, corp_id);
    assert_eq!(
        call_ws(&ctx, &mut line_sess, "posMgr", "UnanchorOrbital", &[PyRep::Integer(orbital_id)]).unwrap_err().code,
        "RoleNotAuthorized"
    );

    let unanchored = call_ws(&ctx, &mut ceo_sess, "posMgr", "UnanchorOrbital", &[PyRep::Integer(orbital_id)]).unwrap();
    assert_eq!(unanchored.dict_get("state").and_then(PyRep::as_str), Some("Unanchored"));
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&orbital_id].location_id, ship_id);
        assert_eq!(snap.items[&orbital_id].flag, InventoryFlag::Cargo);
    });
}

#[test]
fn bills_receivable_access_denied_roles_reload_fsd_and_cluster_stats() {
    let ctx = test_ctx();
    let ceo_id = 90_000_001_i64;
    let line_id = 90_000_002_i64;
    let corp_id = 98_000_001_i64;
    let other_corp_id = 98_000_002_i64;

    assert!(tax_collector_id(&ctx) > 0);

    ctx.db.with_mut_snapshot(|snap| {
        let base = snap.characters[&ceo_id].clone();
        for (cid, bal) in [(ceo_id, 100.0), (line_id, 100.0)] {
            let mut ch = base.clone();
            ch.character_id = cid;
            ch.corporation_id = corp_id;
            ch.balance = IskCents::from_f64_isk(bal);
            snap.characters.insert(cid, ch);
        }
        let mut corp = { let mut __corp = CorporationRecord::default();
            __corp.corporation_id = corp_id;
            __corp.name = "Bill Corp".into();
            __corp.ticker = "BLC".into();
            __corp.ceo_id = ceo_id;
            __corp.set_division_balance(1, IskCents::from_f64_isk(100.0));
            __corp };
        corp.ensure_wallet_divisions();
        snap.corporations.insert(corp_id, corp);
    });

    let big_char_bill = create_bill(&ctx, 2, ceo_id, other_corp_id, 50_000.0, 1000, 0.0, 1, 2);
    let big_corp_bill = create_bill(&ctx, 2, corp_id, other_corp_id, 50_000.0, 1000, 0.0, 1, 2);
    let recv_bill = create_bill(&ctx, 5, other_corp_id, corp_id, 5_000.0, 1000, 0.0, 1, 2);

    let mut ceo_sess = make_session(ceo_id, corp_id);
    let mut line_sess = make_session(line_id, corp_id);

    // 1. GetCorporationBills / GetCorporationBillsReceivable access control & receivable filtering
    assert_eq!(
        call_ws(&ctx, &mut ceo_sess, "billMgr", "GetCorporationBills", &[PyRep::Integer(other_corp_id)])
            .unwrap_err()
            .code,
        "CorpAccessDenied"
    );
    assert_eq!(call_ws(&ctx, &mut line_sess, "billMgr", "GetCorporationBills", &[]).unwrap_err().code, "CorpRoleRequired");
    let recv = call_ws(&ctx, &mut ceo_sess, "billMgr", "GetCorporationBillsReceivable", &[]).unwrap();
    assert!(format!("{recv:?}").contains(&recv_bill.to_string()));

    // 2. CharPayBill & PayCorporationBill error branches
    assert_eq!(call_ws(&ctx, &mut ceo_sess, "billMgr", "CharPayBill", &[]).unwrap_err().code, "InvalidArgument");
    assert_eq!(
        call_ws(&ctx, &mut ceo_sess, "billMgr", "CharPayBill", &[PyRep::Integer(999_999)]).unwrap_err().code,
        "BillNotFound"
    );
    assert_eq!(
        call_ws(&ctx, &mut ceo_sess, "billMgr", "CharPayBill", &[PyRep::Integer(big_char_bill)]).unwrap_err().code,
        "NotEnoughMoney"
    );

    assert_eq!(call_ws(&ctx, &mut ceo_sess, "billMgr", "PayCorporationBill", &[]).unwrap_err().code, "InvalidArgument");
    assert_eq!(
        call_ws(&ctx, &mut ceo_sess, "billMgr", "PayCorporationBill", &[PyRep::Integer(recv_bill)]).unwrap_err().code,
        "AccessDenied"
    );
    assert_eq!(
        call_ws(&ctx, &mut line_sess, "billMgr", "PayCorporationBill", &[PyRep::Integer(big_corp_bill)]).unwrap_err().code,
        "CorpRoleRequired"
    );
    assert_eq!(
        call_ws(&ctx, &mut ceo_sess, "billMgr", "PayCorporationBill", &[PyRep::Integer(big_corp_bill)]).unwrap_err().code,
        "NotEnoughMoney"
    );
    assert_eq!(call_ws(&ctx, &mut line_sess, "billMgr", "SendAutomaticPaySettings", &[]).unwrap_err().code, "CorpRoleRequired");

    // Nested dict format in SendAutomaticPaySettings + Accountant role in DB
    line_sess.corp_role = corp_roles::ROLE_JUNIOR_ACCOUNTANT;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(corp) = snap.corporations.get_mut(&corp_id) {
            corp.member_roles.insert(
                line_id,
                corp_roles::ROLE_ACCOUNTANT | corp_roles::ROLE_ACCOUNT_CAN_TAKE[2],
            );
        }
    });
    assert!(call_ws(
        &ctx,
        &mut line_sess,
        "billMgr",
        "SendAutomaticPaySettings",
        &[PyRep::Dict(vec![(
            PyRep::Integer(corp_id),
            PyRep::Dict(vec![
                (PyRep::Integer(2), PyRep::Bool(true)),
                (PyRep::String("divisionID".into()), PyRep::Integer(1002)),
            ]),
        )])]
    )
    .is_ok());

    // 3. localizationServer.ReloadFSDPickle, eveguard_report, clientStatLogger ring buffer trim
    assert_eq!(
        call_ws(&ctx, &mut line_sess, "localizationServer", "ReloadFSDPickle", &[]).unwrap_err().code,
        "RoleRequired"
    );
    ceo_sess.role = service_roles::ROLE_PROGRAMMER;
    assert_eq!(
        call_ws(&ctx, &mut ceo_sess, "localizationServer", "ReloadFSDPickle", &[]).unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(
        call_ws(&ctx, &mut ceo_sess, "localizationServer", "ReloadFSDPickle", &[PyRep::String("arg".into())])
            .unwrap_err()
            .code,
        "UnimplementedRPC"
    );
    assert_eq!(call_ws(&ctx, &mut ceo_sess, "eveguard_report", "heartbeat_started", &[]).unwrap(), PyRep::None);

    ctx.db.with_mut_snapshot(|snap| {
        let full: Vec<String> = (0..260).map(|i| format!("log-{i}")).collect();
        snap.cluster_and_runtime.client_stat_logger_strings = full;
    });
    assert_eq!(
        call_ws(&ctx, &mut ceo_sess, "clientStatLogger", "LogString", &[PyRep::String("new-entry".into())]).unwrap(),
        PyRep::None
    );
    ctx.db.with_snapshot(|snap| {
        let list = &snap.cluster_and_runtime.client_stat_logger_strings;
        assert_eq!(list.len(), 256);
    });
}
