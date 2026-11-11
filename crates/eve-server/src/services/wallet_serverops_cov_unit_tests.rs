//! Focused unit tests for uncovered branches in `wallet_serverops`
//! (`tax_collector_id`, `create_bill`, `process_due_automatic_bills`, `billMgr`,
//! `account.GiveCash`, `account.SetContactCost`, `account.GetDefaultContactCost`,
//! `machoNet`, `pingService`, `localizationServer`, and `clientStatLogger`).

use super::*;
use eve_db::clock::VirtualServerClock;
use eve_db::{CorporationRecord, IskCents, WorldRepository};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

fn full_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::clone(&ServerContext::new().sde)))
}

fn setup_wallet_ops_world() -> (ServerContext, i64, i64, i64, i64) {
    let ctx = ServerContext::with_sde_and_repository(
        full_sde(),
        Arc::new(WorldRepository::with_demo_seed()),
    );
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_700_000_000)));

    let ceo_id = 96_910_001i64;
    let member_id = 96_910_002i64;
    let corp_a = 98_910_001i64;
    let corp_b = 98_910_002i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().expect("seed char");
        ch.character_id = ceo_id;
        ch.user_id = 9301;
        ch.name = "BillCEO".into();
        ch.corporation_id = corp_a;
        ch.balance = IskCents::from_f64_isk(50_000_000.0);
        snap.characters.insert(ceo_id, ch.clone());

        let mut mem = ch;
        mem.character_id = member_id;
        mem.user_id = 9302;
        mem.name = "BillMember".into();
        mem.balance = IskCents::from_f64_isk(10_000_000.0);
        snap.characters.insert(member_id, mem);

        let mut ca = { let mut __corp = CorporationRecord::default();
            __corp.corporation_id = corp_a;
            __corp.name = "Alpha Industry".into();
            __corp.ticker = "ALPI".into();
            __corp.ceo_id = ceo_id;
            __corp.set_division_balance(1, IskCents::from_f64_isk(100_000_000.0));
            __corp };
        ca.ensure_wallet_divisions();
        ca.wallet_divisions[0].balance = IskCents::from_f64_isk(100_000_000.0);
        ca.wallet_divisions[1].balance = IskCents::from_f64_isk(25_000_000.0);
        snap.corporations.insert(corp_a, ca);

        let mut cb = { let mut __corp = CorporationRecord::default();
            __corp.corporation_id = corp_b;
            __corp.name = "Beta Creditor".into();
            __corp.ticker = "BETC".into();
            __corp.ceo_id = 96_910_099;
            __corp.set_division_balance(1, IskCents::from_f64_isk(10_000_000.0));
            __corp };
        cb.ensure_wallet_divisions();
        snap.corporations.insert(corp_b, cb);
    });

    (ctx, ceo_id, member_id, corp_a, corp_b)
}

fn make_session(char_id: i64, corp_id: i64, role: u64) -> SessionState {
    SessionState {
        user_id: Some(9301),
        character_id: Some(char_id),
        corporation_id: Some(corp_id),
        role,
        ..Default::default()
    }
}

#[test]
fn bills_autopay_and_tax_collector_workflow() {
    let (ctx, ceo_id, member_id, corp_a, corp_b) = setup_wallet_ops_world();
    let kw = HashMap::new();

    assert_ne!(tax_collector_id(&ctx), 0);

    let mut ceo_sess = make_session(ceo_id, corp_a, 0);
    let mut mem_sess = make_session(member_id, corp_a, 0);

    // Create personal bill (debtor = member_id), two corp payable bills (debtor = corp_a), & 1 receivable bill (creditor = corp_a)
    let personal_bill = create_bill(&ctx, 1, member_id, corp_b, 500_000.0, 1_700_010_000, 0.0, 0, 0);
    let corp_bill_1 = create_bill(&ctx, 2, corp_a, corp_b, 2_000_000.0, 1_700_010_000, 0.0, 60_003_760, 0);
    let _corp_bill_2 = create_bill(&ctx, 2, corp_a, corp_b, 1_500_000.0, 1_700_005_000, 0.0, 60_003_761, 0);
    let _corp_recv_bill = create_bill(&ctx, 2, corp_b, corp_a, 750_000.0, 1_700_010_000, 0.0, 60_003_762, 0);

    // Non-privileged member cannot query corp bills or pay corp bill
    let err = handle(&ctx, &mut mem_sess, "billMgr", "GetCorporationBills", &[], &kw)
        .expect("handled")
        .expect_err("member lacks accountant role");
    assert_eq!(err.code, "CorpRoleRequired");

    let err = handle(&ctx, &mut mem_sess, "billMgr", "PayCorporationBill", &[PyRep::Integer(corp_bill_1)], &kw)
        .expect("handled")
        .expect_err("member lacks accountant role");
    assert_eq!(err.code, "CorpRoleRequired");

    // Wrong debtor paying personal bill fails with AccessDenied
    let err = handle(&ctx, &mut ceo_sess, "billMgr", "CharPayBill", &[PyRep::Integer(personal_bill)], &kw)
        .expect("handled")
        .expect_err("ceo is not personal bill debtor");
    assert_eq!(err.code, "AccessDenied");

    // Member pays own personal bill -> character debited, corp_b credited, second attempt fails BillAlreadyPaid
    let paid = handle(&ctx, &mut mem_sess, "billMgr", "CharPayBill", &[PyRep::Integer(personal_bill)], &kw)
        .expect("handled")
        .expect("personal bill paid");
    assert_eq!(paid, PyRep::Bool(true));
    assert_eq!(
        handle(&ctx, &mut mem_sess, "billMgr", "CharPayBill", &[PyRep::Integer(personal_bill)], &kw)
            .expect("handled")
            .unwrap_err()
            .code,
        "BillAlreadyPaid"
    );

    // CEO pays corp_bill_1 from division 1001 (div 2)
    handle(
        &ctx,
        &mut ceo_sess,
        "billMgr",
        "PayCorporationBill",
        &[PyRep::Integer(corp_bill_1), PyRep::Integer(1001)],
        &kw,
    )
    .expect("handled")
    .expect("corp bill paid from div 2");
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.corporations[&corp_a].wallet_divisions[1].balance, IskCents::from_f64_isk(23_000_000.0));
    });

    // GetCorporationBills & GetCorporationBillsReceivable
    let payable = handle(&ctx, &mut ceo_sess, "billMgr", "GetCorporationBills", &[], &kw)
        .expect("handled")
        .expect("payable bills");
    assert!(matches!(payable, PyRep::Rowset { ref rows, .. } if !rows.is_empty()));
    let recv = handle(&ctx, &mut ceo_sess, "billMgr", "GetCorporationBillsReceivable", &[], &kw)
        .expect("handled")
        .expect("receivable bills");
    assert!(matches!(recv, PyRep::Rowset { ref rows, .. } if !rows.is_empty()));

    // Advance clock past corp_bill_2 dueDateTime (1_700_005_000) and enable automatic payment for bill_type 2
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_700_006_000)));
    let settings = PyRep::Dict(vec![(
        PyRep::Integer(corp_a),
        PyRep::Dict(vec![
            (PyRep::Integer(2), PyRep::Bool(true)),
            (PyRep::String("divisionID".into()), PyRep::Integer(1000)),
        ]),
    )]);
    handle(&ctx, &mut ceo_sess, "billMgr", "SendAutomaticPaySettings", &[settings], &kw)
        .expect("handled")
        .expect("autopay configured and processed");
    assert_eq!(
        ctx.db.with_snapshot(|s| s.corporations[&corp_a].master_wallet_balance()),
        IskCents::from_f64_isk(98_500_000.0)
    );

    let got_settings = handle(&ctx, &mut ceo_sess, "billMgr", "GetAutomaticPaySettings", &[], &kw)
        .expect("handled")
        .expect("get autopay settings");
    assert!(matches!(got_settings, PyRep::Dict(_)));
}

#[test]
fn account_give_cash_contact_cost_and_utility_endpoints() {
    let (ctx, ceo_id, member_id, corp_a, _) = setup_wallet_ops_world();
    let kw = HashMap::new();
    let mut ceo_sess = make_session(ceo_id, corp_a, eve_machonet::service_roles::ROLE_PROGRAMMER);

    // GiveCash character-to-character & character-to-corporation
    let start_ceo = ctx.db.with_snapshot(|s| s.characters[&ceo_id].balance.0);
    let start_mem = ctx.db.with_snapshot(|s| s.characters[&member_id].balance.0);
    handle(
        &ctx,
        &mut ceo_sess,
        "account",
        "GiveCash",
        &[PyRep::Integer(member_id), PyRep::Real(1_250_000.0), PyRep::String("Ops stipend".into())],
        &kw,
    )
    .expect("handled")
    .expect("give cash to character");
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.characters[&ceo_id].balance.0, start_ceo - IskCents::from_f64_isk(1_250_000.0).0);
        assert_eq!(snap.characters[&member_id].balance.0, start_mem + IskCents::from_f64_isk(1_250_000.0).0);
    });

    // GiveCash to corp wallet division 1002
    handle(
        &ctx,
        &mut ceo_sess,
        "account",
        "GiveCash",
        &[PyRep::Integer(corp_a), PyRep::Real(500_000.0), PyRep::String("Corp donation".into()), PyRep::Integer(1002)],
        &kw,
    )
    .expect("handled")
    .expect("give cash to corp");

    // SetContactCost & GetDefaultContactCost
    handle(&ctx, &mut ceo_sess, "account", "SetContactCost", &[PyRep::Real(250_000.0)], &kw)
        .expect("handled")
        .expect("set contact cost");
    let cost = handle(&ctx, &mut ceo_sess, "account", "GetDefaultContactCost", &[], &kw)
        .expect("handled")
        .expect("get contact cost")
        .as_f64()
        .unwrap();
    assert!((cost - 250_000.0).abs() < 0.01);

    // machoNet.GetClusterGameStatisticsForClient & pingService.Ping & clientStatLogger.LogString & localizationServer.ReloadFSDPickle
    let stats = handle(&ctx, &mut ceo_sess, "machoNet", "GetClusterGameStatisticsForClient", &[], &kw)
        .expect("handled")
        .expect("cluster stats");
    assert!(matches!(stats, PyRep::Tuple(ref t) if t.len() == 3));

    let pong = handle(&ctx, &mut ceo_sess, "pingService", "Ping", &[], &kw)
        .expect("handled")
        .expect("ping");
    assert!(pong.as_i64().unwrap() > 0);

    handle(&ctx, &mut ceo_sess, "clientStatLogger", "LogString", &[PyRep::String("client telemetry sample".into())], &kw)
        .expect("handled")
        .expect("log string");
    let reload = handle(&ctx, &mut ceo_sess, "localizationServer", "ReloadFSDPickle", &[], &kw)
        .expect("handled")
        .expect("reload pickle with programmer role");
    assert_eq!(reload, PyRep::Bool(true));
}

#[test]
fn check_starbase_auth_all_role_bitshifts_and_ownership_combinations() {
    let (ctx, ceo_id, member_id, corp_a, corp_b) = setup_wallet_ops_world();
    let tower_corp = 96_920_001_i64;
    let tower_char = 96_920_002_i64;
    let unreg_corp = 98_999_777_i64;
    let member_unreg = 96_910_003_i64;
    let tower_unreg = 96_920_003_i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            tower_corp,
            eve_db::UpwellStructureRecord {
                structure_id: tower_corp,
                type_id: 16_213,
                owner_corp_id: corp_a,
                solar_system_id: 30_000_142,
                ..Default::default()
            },
        );
        snap.items.insert(
            tower_char,
            eve_db::InventoryItemRecord {
                item_id: tower_char,
                type_id: 16_213,
                owner_id: member_id,
                location_id: 30_000_142,
                ..Default::default()
            },
        );
        let mut mem_u = snap.characters[&member_id].clone();
        mem_u.character_id = member_unreg;
        mem_u.user_id = 9_399;
        mem_u.corporation_id = unreg_corp;
        snap.characters.insert(member_unreg, mem_u);
        snap.items.insert(
            tower_unreg,
            eve_db::InventoryItemRecord {
                item_id: tower_unreg,
                type_id: 16_213,
                owner_id: unreg_corp,
                location_id: 30_000_142,
                ..Default::default()
            },
        );
    });

    let foreign_sess = make_session(member_id, corp_b, 0);
    let gm_sess = make_session(member_id, corp_b, eve_db::service_roles::ROLE_GMH);
    let mut mem_sess = make_session(member_id, corp_a, 0);

    ctx.db.with_mut_snapshot(|snap| {
        // GM bypasses foreign check
        assert!(check_starbase_auth(snap, &gm_sess, member_id, tower_corp).is_ok());
        // Missing structure -> StructureNotFound
        assert_eq!(check_starbase_auth(snap, &mem_sess, member_id, 999_888_111).unwrap_err().code, "StructureNotFound");
        // Foreign corp -> NotStarbaseOwner
        assert_eq!(check_starbase_auth(snap, &foreign_sess, member_id, tower_corp).unwrap_err().code, "NotStarbaseOwner");
        // Personal tower owned by member_id even if session is in corp_b (or corp_a) -> Ok(())
        assert!(check_starbase_auth(snap, &foreign_sess, member_id, tower_char).is_ok());
        // CEO with 0 bitmask -> Ok(())
        let ceo_zero = make_session(ceo_id, corp_a, 0);
        assert!(check_starbase_auth(snap, &ceo_zero, ceo_id, tower_corp).is_ok());
    });

    // Test every allowed bitshift individually in session.corp_role (with eff_roles == 0): 1<<0, 1<<24, 1<<25, 1<<40
    for shift in [0_u32, 24, 25, 40] {
        mem_sess.corp_role = 1_u64 << shift;
        ctx.db.with_snapshot(|snap| {
            assert!(
                check_starbase_auth(snap, &mem_sess, member_id, tower_corp).is_ok(),
                "session shift {shift} must authorize"
            );
        });
    }
    // Every neighboring non-starbase bit must FAIL in session.corp_role when eff_roles == 0
    for bad_shift in [1_u32, 2, 23, 26, 39, 41] {
        mem_sess.corp_role = 1_u64 << bad_shift;
        ctx.db.with_snapshot(|snap| {
            assert_eq!(
                check_starbase_auth(snap, &mem_sess, member_id, tower_corp).unwrap_err().code,
                "RoleNotAuthorized",
                "bad session shift {bad_shift} must reject"
            );
        });
    }

    // Now test session.corp_role == 0 and DB member_roles containing each allowed bit individually!
    mem_sess.corp_role = 0;
    for shift in [0_u32, 24, 25, 40] {
        ctx.db.with_mut_snapshot(|snap| {
            snap.corporations.get_mut(&corp_a).unwrap().member_roles.insert(member_id, 1_u64 << shift);
            assert!(
                check_starbase_auth(snap, &mem_sess, member_id, tower_corp).is_ok(),
                "DB eff_roles shift {shift} must authorize"
            );
        });
    }
    for bad_shift in [1_u32, 2, 23, 26, 39, 41] {
        ctx.db.with_mut_snapshot(|snap| {
            snap.corporations.get_mut(&corp_a).unwrap().member_roles.insert(member_id, 1_u64 << bad_shift);
            assert_eq!(
                check_starbase_auth(snap, &mem_sess, member_id, tower_corp).unwrap_err().code,
                "RoleNotAuthorized",
                "bad DB shift {bad_shift} must reject"
            );
        });
    }

    // Unregistered corp (not in snap.corporations) only authorizes via session.corp_role
    let mut unreg_sess = make_session(member_unreg, unreg_corp, 0);
    ctx.db.with_snapshot(|snap| {
        assert_eq!(check_starbase_auth(snap, &unreg_sess, member_unreg, tower_unreg).unwrap_err().code, "RoleNotAuthorized");
    });
    unreg_sess.corp_role = 1_u64 << 25;
    ctx.db.with_snapshot(|snap| {
        assert!(check_starbase_auth(snap, &unreg_sess, member_unreg, tower_unreg).is_ok());
    });
}

#[test]
fn pos_mgr_all_arms_bill_mgr_autopay_count_give_cash_contact_cost_and_telemetry_limits() {
    let (ctx, ceo_id, member_id, corp_a, corp_b) = setup_wallet_ops_world();
    let kw = HashMap::new();
    let ally_id = 99_910_001_i64;
    let tower_id = 96_930_001_i64;
    let silo_id = 96_930_002_i64;
    let moon_item_id = 96_930_003_i64;
    let orbital_id = 96_930_004_i64;
    let orbital_no_ship = 96_930_005_i64;
    let ship_id = 96_930_100_i64;
    let sys_id = 30_000_142_u32;

    let moon_mat_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.group_id == 427)
        .map(|t| t.type_id)
        .unwrap_or(16_634);
    let moon_pos = ctx
        .sde
        .celestials_in_system(sys_id)
        .iter()
        .find(|c| c.group_id == 8)
        .map(|c| [c.position.x, c.position.y, c.position.z])
        .unwrap_or([1_000.0, 0.0, 0.0]);

    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.get_mut(&corp_a).unwrap().alliance_id = Some(ally_id);
        snap.system_states.entry(sys_id).or_default().sov_alliance_id = Some(ally_id);
        snap.characters.get_mut(&ceo_id).unwrap().solar_system_id = sys_id;
        snap.characters.get_mut(&ceo_id).unwrap().active_ship_id = ship_id;
        snap.items.insert(
            ship_id,
            eve_db::InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: ceo_id,
                location_id: i64::from(sys_id),
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.structures.insert(
            tower_id,
            eve_db::UpwellStructureRecord {
                structure_id: tower_id,
                type_id: 16_213,
                owner_corp_id: corp_a,
                solar_system_id: sys_id,
                position: moon_pos,
                state: "Online".into(),
                ..Default::default()
            },
        );
        snap.items.insert(
            silo_id,
            eve_db::InventoryItemRecord {
                item_id: silo_id,
                type_id: 14_343, // POS Silo
                owner_id: corp_a,
                location_id: tower_id,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            moon_item_id,
            eve_db::InventoryItemRecord {
                item_id: moon_item_id,
                type_id: 16_213,
                owner_id: corp_a,
                location_id: i64::from(sys_id),
                dynamic_attributes: HashMap::from([(600, 40_123_456.0)]),
                ..Default::default()
            },
        );
        snap.items.insert(
            orbital_id,
            eve_db::InventoryItemRecord {
                item_id: orbital_id,
                type_id: 2233,
                owner_id: corp_a,
                location_id: i64::from(sys_id),
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.structures.insert(
            orbital_id,
            eve_db::UpwellStructureRecord {
                structure_id: orbital_id,
                type_id: 2233,
                owner_corp_id: corp_a,
                solar_system_id: sys_id,
                position: [1_000.0, 0.0, 0.0],
                state: "Online".into(),
                ..Default::default()
            },
        );
        snap.items.insert(
            orbital_no_ship,
            eve_db::InventoryItemRecord {
                item_id: orbital_no_ship,
                type_id: 2233,
                owner_id: ceo_id,
                location_id: i64::from(sys_id),
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });

    let mut ceo_sess = make_session(ceo_id, corp_a, 0);
    ceo_sess.solar_system_id = Some(sys_id);
    ceo_sess.ship_id = Some(ship_id);

    let mut mem_sess = make_session(member_id, corp_a, 0);
    mem_sess.solar_system_id = Some(sys_id);
    mem_sess.ship_id = Some(ship_id);

    // posMgr.GetMoonForTower (structure, dynamic attr 600, item in moon 40..49M, negative <= 0 -> None)
    assert!(handle(&ctx, &mut ceo_sess, "posMgr", "GetMoonForTower", &[PyRep::Integer(tower_id)], &kw).unwrap().is_ok());
    assert_eq!(
        handle(&ctx, &mut ceo_sess, "posMgr", "GetMoonForTower", &[PyRep::Integer(moon_item_id)], &kw).unwrap().unwrap(),
        PyRep::Integer(40_123_456)
    );
    assert_eq!(
        handle(&ctx, &mut ceo_sess, "posMgr", "GetMoonForTower", &[PyRep::Integer(0)], &kw).unwrap().unwrap(),
        PyRep::None
    );

    // posMgr.GetMoonProcessInfoForTower (absent -> None; after ChangeStructureProvisionType -> Some)
    assert_eq!(
        handle(&ctx, &mut ceo_sess, "posMgr", "GetMoonProcessInfoForTower", &[PyRep::Integer(silo_id)], &kw).unwrap().unwrap(),
        PyRep::None
    );
    assert_eq!(
        handle(&ctx, &mut ceo_sess, "posMgr", "ChangeStructureProvisionType", &[PyRep::Integer(tower_id), PyRep::Integer(silo_id), PyRep::Integer(-5)], &kw).unwrap().unwrap_err().code,
        "InvalidTypeID"
    );
    assert_eq!(
        handle(&ctx, &mut ceo_sess, "posMgr", "ChangeStructureProvisionType", &[PyRep::Integer(tower_id), PyRep::Integer(silo_id), PyRep::Integer(34)], &kw).unwrap().unwrap_err().code,
        "InvalidProvisionType"
    );
    handle(&ctx, &mut ceo_sess, "posMgr", "ChangeStructureProvisionType", &[PyRep::Integer(tower_id), PyRep::Integer(silo_id), PyRep::Integer(i64::from(moon_mat_tid))], &kw).unwrap().unwrap();
    assert_eq!(
        handle(&ctx, &mut ceo_sess, "posMgr", "GetMoonProcessInfoForTower", &[PyRep::Integer(silo_id)], &kw).unwrap().unwrap(),
        PyRep::Integer(i64::from(moon_mat_tid))
    );

    // posMgr.SetSessionCheck -> true
    assert_eq!(
        handle(&ctx, &mut ceo_sess, "posMgr", "SetSessionCheck", &[], &kw).unwrap().unwrap(),
        PyRep::Bool(true)
    );

    // posMgr.AssumeStructureControl & RelinquishStructureControl
    let mut foreign_sess = make_session(member_id, corp_b, 0);
    foreign_sess.ship_id = Some(ship_id);
    assert_eq!(
        handle(&ctx, &mut foreign_sess, "posMgr", "AssumeStructureControl", &[PyRep::Integer(tower_id)], &kw).unwrap().unwrap_err().code,
        "ForcefieldPasswordMismatch"
    );
    handle(&ctx, &mut ceo_sess, "posMgr", "AssumeStructureControl", &[PyRep::Integer(tower_id)], &kw).unwrap().unwrap();
    ctx.db.with_snapshot(|snap| assert_eq!(snap.starbase_settings[&tower_id].active_controller_id, Some(ceo_id)));
    handle(&ctx, &mut ceo_sess, "posMgr", "RelinquishStructureControl", &[PyRep::Integer(tower_id)], &kw).unwrap().unwrap();
    ctx.db.with_snapshot(|snap| assert_eq!(snap.starbase_settings[&tower_id].active_controller_id, None));

    // posMgr.SetStarbasePermissions & GetStarbasePermissions ("alliance" = 2, "starbase" = 3, "corp" = 1)
    handle(
        &ctx,
        &mut ceo_sess,
        "posMgr",
        "SetStarbasePermissions",
        &[
            PyRep::Integer(tower_id),
            PyRep::String("alliance".into()),
            PyRep::String("starbase".into()),
            PyRep::String("corp".into()),
            PyRep::String("alliance".into()),
        ],
        &kw,
    ).unwrap().unwrap();
    let perms = handle(&ctx, &mut ceo_sess, "posMgr", "GetStarbasePermissions", &[PyRep::Integer(tower_id)], &kw).unwrap().unwrap();
    assert!(matches!(perms, PyRep::Tuple(ref v) if v.len() == 2));

    // posMgr.SetTowerNotifications & GetTowerNotificationSettings
    handle(&ctx, &mut ceo_sess, "posMgr", "SetTowerNotifications", &[PyRep::Integer(tower_id), PyRep::Bool(true), PyRep::Bool(false)], &kw).unwrap().unwrap();
    let t_notif = handle(&ctx, &mut ceo_sess, "posMgr", "GetTowerNotificationSettings", &[PyRep::Integer(tower_id)], &kw).unwrap().unwrap();
    assert!(format!("{t_notif:?}").contains("false") && format!("{t_notif:?}").contains("true"));

    // posMgr.SetTowerSentrySettings & GetTowerSentrySettings (default & updated)
    assert!(handle(&ctx, &mut ceo_sess, "posMgr", "GetTowerSentrySettings", &[PyRep::Integer(tower_id)], &kw).unwrap().is_ok());
    handle(
        &ctx,
        &mut ceo_sess,
        "posMgr",
        "SetTowerSentrySettings",
        &[PyRep::Integer(tower_id), PyRep::Real(-2.5), PyRep::Real(-5.0), PyRep::Bool(true), PyRep::Bool(false), PyRep::Bool(true)],
        &kw,
    ).unwrap().unwrap();
    let sentry = handle(&ctx, &mut ceo_sess, "posMgr", "GetTowerSentrySettings", &[PyRep::Integer(tower_id)], &kw).unwrap().unwrap();
    assert!(format!("{sentry:?}").contains("-2.5"));

    // posMgr.SetTowerPassword
    handle(&ctx, &mut ceo_sess, "posMgr", "SetTowerPassword", &[PyRep::Integer(tower_id), PyRep::String("secret123".into()), PyRep::Bool(true), PyRep::Bool(false)], &kw).unwrap().unwrap();

    // posMgr.GetConsumptionFactor: sov_ally == tower_ally (> 0) -> 0.75; mismatch -> 1.0
    assert_eq!(
        handle(&ctx, &mut ceo_sess, "posMgr", "GetConsumptionFactor", &[PyRep::Integer(tower_id)], &kw).unwrap().unwrap(),
        PyRep::Real(0.75)
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states.get_mut(&sys_id).unwrap().sov_alliance_id = Some(ally_id + 1);
    });
    assert_eq!(
        handle(&ctx, &mut ceo_sess, "posMgr", "GetConsumptionFactor", &[PyRep::Integer(tower_id)], &kw).unwrap().unwrap(),
        PyRep::Real(1.0)
    );

    // posMgr.GetSiloCapacityByItemID & RunMoonProcessCycleforTower & LinkResourceForTower
    assert!(handle(&ctx, &mut ceo_sess, "posMgr", "GetSiloCapacityByItemID", &[PyRep::Integer(silo_id)], &kw).unwrap().is_ok());
    assert_eq!(
        handle(&ctx, &mut ceo_sess, "posMgr", "GetSiloCapacityByItemID", &[PyRep::Integer(999_888_777)], &kw).unwrap().unwrap(),
        PyRep::Real(20_000.0)
    );
    assert_eq!(
        handle(&ctx, &mut ceo_sess, "posMgr", "RunMoonProcessCycleforTower", &[], &kw).unwrap().unwrap_err().code,
        "UnimplementedRPC"
    );
    assert_eq!(
        handle(&ctx, &mut ceo_sess, "posMgr", "LinkResourceForTower", &[PyRep::Integer(tower_id), PyRep::Integer(moon_item_id)], &kw).unwrap().unwrap_err().code,
        "InvalidLinkTarget"
    );
    handle(&ctx, &mut ceo_sess, "posMgr", "LinkResourceForTower", &[PyRep::Integer(tower_id), PyRep::List(vec![PyRep::Integer(silo_id)])], &kw).unwrap().unwrap();
    ctx.db.with_snapshot(|snap| assert!(snap.starbase_settings[&tower_id].linked_resources.contains(&silo_id)));

    // posMgr.UnanchorOrbital error gates + distance + RoleNotAuthorized + cargo move vs hangar move
    assert_eq!(
        handle(&ctx, &mut mem_sess, "posMgr", "UnanchorOrbital", &[PyRep::Integer(orbital_id)], &kw).unwrap().unwrap_err().code,
        "RoleNotAuthorized"
    );
    let mut wrong_sys_sess = ceo_sess.clone();
    wrong_sys_sess.solar_system_id = Some(30_000_144);
    assert_eq!(
        handle(&ctx, &mut wrong_sys_sess, "posMgr", "UnanchorOrbital", &[PyRep::Integer(orbital_id)], &kw).unwrap().unwrap_err().code,
        "NotInSameSystem"
    );
    let kv_ship = handle(&ctx, &mut ceo_sess, "posMgr", "UnanchorOrbital", &[PyRep::Integer(orbital_id)], &kw).unwrap().unwrap();
    assert_eq!(kv_ship.dict_get("state"), Some(&PyRep::String("Unanchored".into())));
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&orbital_id].location_id, ship_id);
        assert_eq!(snap.items[&orbital_id].flag, InventoryFlag::Cargo);
    });
    let mut no_ship_sess = ceo_sess.clone();
    no_ship_sess.ship_id = None;
    handle(&ctx, &mut no_ship_sess, "posMgr", "UnanchorOrbital", &[PyRep::Integer(orbital_no_ship)], &kw).unwrap().unwrap();
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&orbital_no_ship].location_id, ceo_id);
        assert_eq!(snap.items[&orbital_no_ship].flag, InventoryFlag::Hangar);
    });

    // billMgr: foreign CorpAccessDenied, empty/non-corp fallback (corp_id == 0 -> skips role check), process_due_automatic_bills == 2
    assert_eq!(
        handle(&ctx, &mut ceo_sess, "billMgr", "GetCorporationBills", &[PyRep::Integer(corp_b)], &kw).unwrap().unwrap_err().code,
        "CorpAccessDenied"
    );
    let b_due1 = create_bill(&ctx, 2, corp_a, corp_b, 1_000_000.0, 1_699_999_000, 0.0, 1, 0);
    let b_due2 = create_bill(&ctx, 2, corp_a, corp_b, 1_000_000.0, 1_699_999_500, 0.0, 2, 0);
    assert!(b_due1 != b_due2);
    assert_eq!(process_due_automatic_bills(&ctx), 0); // autopay not enabled yet
    handle(
        &ctx,
        &mut ceo_sess,
        "billMgr",
        "SendAutomaticPaySettings",
        &[PyRep::Dict(vec![(PyRep::Integer(2), PyRep::Bool(true)), (PyRep::String("divisionID".into()), PyRep::Integer(1000))])],
        &kw,
    ).unwrap().unwrap();
    // Both due bills paid cleanly when autopay was saved! Also create two more & run direct helper count == 2
    let _b_due3 = create_bill(&ctx, 2, corp_a, corp_b, 100_000.0, 1_699_999_000, 0.0, 3, 0);
    let _b_due4 = create_bill(&ctx, 2, corp_a, corp_b, 100_000.0, 1_699_999_500, 0.0, 4, 0);
    assert_eq!(process_due_automatic_bills(&ctx), 2);

    // account.GiveCash error guards & SetContactCost(0.0) removal -> None
    assert_eq!(
        handle(&ctx, &mut ceo_sess, "account", "GiveCash", &[PyRep::Integer(ceo_id), PyRep::Real(100.0), PyRep::String(String::new())], &kw).unwrap().unwrap_err().code,
        "InvalidArgument"
    );
    assert_eq!(
        handle(&ctx, &mut ceo_sess, "account", "GiveCash", &[PyRep::Integer(999_777_666), PyRep::Real(100.0), PyRep::String(String::new())], &kw).unwrap().unwrap_err().code,
        "InvalidRecipient"
    );
    assert_eq!(
        handle(&ctx, &mut ceo_sess, "account", "GiveCash", &[PyRep::Integer(member_id), PyRep::Real(999_000_000_000.0), PyRep::String("huge".into())], &kw).unwrap().unwrap_err().code,
        "NotEnoughMoney"
    );
    handle(&ctx, &mut ceo_sess, "account", "SetContactCost", &[PyRep::Real(100.0)], &kw).unwrap().unwrap();
    assert_eq!(handle(&ctx, &mut ceo_sess, "account", "GetDefaultContactCost", &[], &kw).unwrap().unwrap(), PyRep::Real(100.0));
    handle(&ctx, &mut ceo_sess, "account", "SetContactCost", &[PyRep::Real(0.0)], &kw).unwrap().unwrap();
    assert_eq!(handle(&ctx, &mut ceo_sess, "account", "GetDefaultContactCost", &[], &kw).unwrap().unwrap(), PyRep::None);

    // clientStatLogger.LogString trims > 256 down to 256, and eveguard_report returns None
    for i in 0..260 {
        handle(&ctx, &mut ceo_sess, "clientStatLogger", "LogString", &[PyRep::String(format!("entry-{i}"))], &kw).unwrap().unwrap();
    }
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.cluster_and_runtime.client_stat_logger_strings.len(), 256);
        assert_eq!(snap.cluster_and_runtime.client_stat_logger_strings.first().map(String::as_str), Some("entry-4"));
    });
    assert_eq!(handle(&ctx, &mut ceo_sess, "eveguard_report", "heartbeat_started", &[], &kw).unwrap().unwrap(), PyRep::None);
    assert_eq!(handle(&ctx, &mut ceo_sess, "eveguard_report", "heartbeat_stopped", &[], &kw).unwrap().unwrap(), PyRep::None);
}

