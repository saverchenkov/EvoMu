//! Corporation governance, member auditing, project contribution methods, and structure defaults.
//!
//! Parity rows:
//! - `feat:corporation.member-audit`
//! - `feat:corporation.corp-project-contribution-methods`
//! - `feat:corporation.structure-reinforce-default`

#![forbid(unsafe_code)]

use std::collections::HashMap;

use crate::common;
use eve_db::{CorpProjectRecord, IskCents};
use eve_destiny::{Ball, DamageVector, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use glam::DVec3;

const CEO_PILOT: i64 = 90_000_001;
const MEMBER_PILOT: i64 = 90_000_002;
const CORP_ID: i64 = 98_000_001;

async fn select_pilot(
    ctx: &ServerContext,
    session_id: u64,
    username: &str,
    password: &str,
    char_id: i64,
) -> SessionState {
    let mut sess = SessionState::new(session_id);
    common::login_pilot(ctx, &mut sess, username, password).await;
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &HashMap::new(),
        None,
    )
    .await
    .expect("SelectCharacterID");
    ctx.advance_secs(12);
    sess
}

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, eve_proto::BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None)
        .await
}

fn field(rep: &PyRep, key: &str) -> PyRep {
    match rep {
        PyRep::KeyVal(map) => map.get(key).cloned().unwrap_or(PyRep::None),
        _ => PyRep::None,
    }
}

#[tokio::test]
async fn test_corporation_member_roles_and_audit_log_tracking() {
    // parity: feat:corporation.member-audit
    eve_testkit::attest(&["feat:corporation.member-audit"]);

    let ctx = common::test_context().await;
    let mut sess1 = select_pilot(&ctx, 60_031, "testpilot1", "testpass1", CEO_PILOT).await;
    let mut sess2 = select_pilot(&ctx, 60_032, "testpilot2", "testpass2", MEMBER_PILOT).await;

    // Ensure member is in corporation
    ctx.db.with_mut_snapshot(|s| {
        if let Some(corp) = s.corporations.get_mut(&CORP_ID) {
            corp.members.insert(MEMBER_PILOT);
        }
        if let Some(ch) = s.characters.get_mut(&MEMBER_PILOT) {
            ch.corporation_id = CORP_ID;
        }
    });

    // 1. Non-director / regular member calling corpmgr.AuditMember is denied with CorpRoleDenied
    let audit_err = rpc(&ctx, &mut sess2, "corpmgr", "AuditMember", &[PyRep::Integer(CEO_PILOT)])
        .await
        .expect_err("Regular member AuditMember must fail");
    assert_eq!(audit_err.code, "CorpRoleDenied");

    // 2. Director/CEO updates member roles twice via corpRegistry.UpdateMember
    rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "UpdateMember",
        &[
            PyRep::Integer(MEMBER_PILOT),
            PyRep::String("Accountant".into()),
            PyRep::Integer(eve_db::corp_roles::ROLE_ACCOUNTANT as i64),
            PyRep::Integer(0),
            PyRep::Integer(0),
            PyRep::Integer(0),
            PyRep::Integer(0),
            PyRep::Integer(0),
            PyRep::Integer(0),
            PyRep::Integer(0),
            PyRep::Integer(0),
            PyRep::Integer(0),
        ],
    )
    .await
    .expect("UpdateMember 1");

    ctx.advance_secs(10);

    rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "UpdateMember",
        &[
            PyRep::Integer(MEMBER_PILOT),
            PyRep::String("Security Officer".into()),
            PyRep::Integer((eve_db::corp_roles::ROLE_ACCOUNTANT | eve_db::corp_roles::ROLE_SECURITY_OFFICER) as i64),
            PyRep::Integer(0),
            PyRep::Integer(0),
            PyRep::Integer(0),
            PyRep::Integer(0),
            PyRep::Integer(0),
            PyRep::Integer(0),
            PyRep::Integer(0),
            PyRep::Integer(0),
            PyRep::Integer(0),
        ],
    )
    .await
    .expect("UpdateMember 2");

    // 3. Director/CEO audits member role change history
    let audit = rpc(&ctx, &mut sess1, "corpmgr", "AuditMember", &[PyRep::Integer(MEMBER_PILOT)])
        .await
        .expect("AuditMember");

    let PyRep::Rowset { rows, .. } = audit else {
        panic!("expected Rowset from AuditMember, got {audit:?}");
    };
    assert!(rows.len() >= 2, "Expected at least 2 audit entries, got {}", rows.len());

    // Verify first entry: target is MEMBER_PILOT, issuer is CEO_PILOT, newRoles has ROLE_ACCOUNTANT
    let has_accountant = rows.iter().any(|r| {
        r.first() == Some(&PyRep::Integer(MEMBER_PILOT))
            && r.get(1) == Some(&PyRep::Integer(CEO_PILOT))
            && r.get(3) == Some(&PyRep::Integer(eve_db::corp_roles::ROLE_ACCOUNTANT as i64))
    });
    assert!(has_accountant, "AuditMember must include accountant role change entry");

    // Verify second entry: target is MEMBER_PILOT, issuer is CEO_PILOT, newRoles has ROLE_SECURITY_OFFICER
    let has_security = rows.iter().any(|r| {
        r.first() == Some(&PyRep::Integer(MEMBER_PILOT))
            && r.get(1) == Some(&PyRep::Integer(CEO_PILOT))
            && r.get(3) == Some(&PyRep::Integer((eve_db::corp_roles::ROLE_ACCOUNTANT | eve_db::corp_roles::ROLE_SECURITY_OFFICER) as i64))
    });
    assert!(has_security, "AuditMember must include security officer role change entry");
}

#[tokio::test]
async fn test_corporation_project_contribution_methods_and_milestones() {
    // parity: feat:corporation.corp-project-contribution-methods
    eve_testkit::attest(&["feat:corporation.corp-project-contribution-methods"]);

    let ctx = common::test_context().await;
    let _sess1 = select_pilot(&ctx, 60_041, "testpilot1", "testpass1", CEO_PILOT).await;
    let mut sess2 = select_pilot(&ctx, 60_042, "testpilot2", "testpass2", MEMBER_PILOT).await;

    ctx.db.with_mut_snapshot(|s| {
        if let Some(corp) = s.corporations.get_mut(&CORP_ID) {
            corp.members.insert(MEMBER_PILOT);
        }
        if let Some(ch) = s.characters.get_mut(&MEMBER_PILOT) {
            ch.corporation_id = CORP_ID;
            ch.balance = IskCents(100_000_00); // 100,000 ISK
        }
    });

    let now = ctx.now_unix();

    // 1. Create a Mining project (MineMaterials, 50 units target, 100 ISK/unit)
    let p_mining_id = 880_001;
    ctx.db.with_mut_snapshot(|s| {
        s.corp_projects.insert(p_mining_id, CorpProjectRecord {
            project_id: p_mining_id,
            corporation_id: CORP_ID,
            name: "Mining Project".into(),
            description: "Mine ore".into(),
            target_quantity: 50,
            current_quantity: 0,
            isk_per_unit: IskCents(100_00),
            escrow_remaining: IskCents(50_000_00),
            created_at_unix: now,
            expires_at_unix: now + 86400 * 7,
            career: "industry".into(),
        });
        s.alliance_war_state.project_meta.insert(p_mining_id, ("MineMaterials".into(), false, false));
        let det = s.alliance_war_state.project_details.entry(p_mining_id).or_default();
        det.status = "Active".into();
    });

    // 2. Create a Damage project (DamageShips, 500 dmg target, 10 ISK/unit)
    let p_dmg_id = 880_002;
    ctx.db.with_mut_snapshot(|s| {
        s.corp_projects.insert(p_dmg_id, CorpProjectRecord {
            project_id: p_dmg_id,
            corporation_id: CORP_ID,
            name: "Fleet Combat Damage".into(),
            description: "Damage hostile ships".into(),
            target_quantity: 500,
            current_quantity: 0,
            isk_per_unit: IskCents(10_00),
            escrow_remaining: IskCents(50_000_00),
            created_at_unix: now,
            expires_at_unix: now + 86400 * 7,
            career: "soldier".into(),
        });
        s.alliance_war_state.project_meta.insert(p_dmg_id, ("DamageShips".into(), false, false));
        let det = s.alliance_war_state.project_details.entry(p_dmg_id).or_default();
        det.status = "Active".into();
    });

    // 3. Create a Salvage project (SalvageWrecks, 1 wreck target, 5,000 ISK/unit)
    let p_salvage_id = 880_003;
    ctx.db.with_mut_snapshot(|s| {
        s.corp_projects.insert(p_salvage_id, CorpProjectRecord {
            project_id: p_salvage_id,
            corporation_id: CORP_ID,
            name: "Salvage Operation".into(),
            description: "Salvage battlefield wrecks".into(),
            target_quantity: 1,
            current_quantity: 0,
            isk_per_unit: IskCents(5_000_00),
            escrow_remaining: IskCents(10_000_00),
            created_at_unix: now,
            expires_at_unix: now + 86400 * 7,
            career: "explorer".into(),
        });
        s.alliance_war_state.project_meta.insert(p_salvage_id, ("SalvageWrecks".into(), false, false));
        let det = s.alliance_war_state.project_details.entry(p_salvage_id).or_default();
        det.status = "Active".into();
    });

    let sys_id = 30_000_142;
    let ship_id = 99_003_001;
    sess2.solar_system_id = Some(sys_id);
    sess2.ship_id = Some(ship_id);

    // Live Event 1: Mining harvest event (deposit mined ore)
    ctx.db.with_mut_snapshot(|s| {
        s.items.insert(ship_id, eve_db::InventoryItemRecord {
            item_id: ship_id,
            type_id: 606,
            owner_id: MEMBER_PILOT,
            location_id: sys_id as i64,
            flag: eve_db::InventoryFlag::None,
            quantity: 1,
            singleton: true,
            version: 1,
            dynamic_attributes: HashMap::new(),
        });
    });

    eve_server::services::space_mining_harvest::deposit_mined_ore_and_record_ledger(
        &ctx,
        MEMBER_PILOT,
        ship_id,
        sys_id,
        eve_db::InventoryFlag::CargoHold,
        1230, // Veldspar
        50,
    );

    // Verify Mining Project progress and reward
    let proj_mining = ctx.db.with_snapshot(|s| s.corp_projects.get(&p_mining_id).cloned()).unwrap();
    assert_eq!(proj_mining.current_quantity, 50);
    let mining_status = ctx.db.with_snapshot(|s| s.alliance_war_state.project_details.get(&p_mining_id).map(|d| d.status.clone())).unwrap();
    assert_eq!(mining_status, "Completed");

    // Live Event 2: Damage dealt in combat
    let target_ship_id = 99_003_002;
    ctx.db.with_mut_snapshot(|s| {
        s.items.insert(target_ship_id, eve_db::InventoryItemRecord {
            item_id: target_ship_id,
            type_id: 606, // Category 6 = Ship
            owner_id: 90_000_009,
            location_id: sys_id as i64,
            flag: eve_db::InventoryFlag::None,
            quantity: 1,
            singleton: true,
            version: 1,
            dynamic_attributes: HashMap::new(),
        });
    });

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    sys_handle.spawn_ball(Ball {
        id: target_ship_id,
        type_id: 606,
        position: DVec3::new(1000.0, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 500.0,
        inertia: 1.0,
        mass: 100_000.0,
        radius: 50.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    }).await.unwrap();

    let dmg = DamageVector {
        em: 250.0,
        thermal: 250.0,
        kinetic: 0.0,
        explosive: 0.0,
    };
    eve_server::services::structure_lifecycle_rpc::apply_damage_structure_or_ball(
        &ctx,
        &sys_handle,
        sys_id,
        MEMBER_PILOT,
        target_ship_id,
        dmg,
        1.0,
        now,
    )
    .await
    .expect("apply_damage_structure_or_ball");

    // Verify Damage Project progress
    let proj_dmg = ctx.db.with_snapshot(|s| s.corp_projects.get(&p_dmg_id).cloned()).unwrap();
    assert_eq!(proj_dmg.current_quantity, 500);
    let dmg_status = ctx.db.with_snapshot(|s| s.alliance_war_state.project_details.get(&p_dmg_id).map(|d| d.status.clone())).unwrap();
    assert_eq!(dmg_status, "Completed");

    // Live Event 3: Salvaging a wreck
    let wreck_id = 99_003_003;
    ctx.db.with_mut_snapshot(|s| {
        s.items.insert(wreck_id, eve_db::InventoryItemRecord {
            item_id: wreck_id,
            type_id: 26581, // Wreck
            owner_id: MEMBER_PILOT,
            location_id: sys_id as i64,
            flag: eve_db::InventoryFlag::None,
            quantity: 1,
            singleton: true,
            version: 1,
            dynamic_attributes: HashMap::new(),
        });
    });

    sys_handle.spawn_ball(Ball {
        id: wreck_id,
        type_id: 26581,
        position: DVec3::new(200.0, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 0.0,
        inertia: 1.0,
        mass: 10_000.0,
        radius: 20.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    }).await.unwrap();

    let salvage_res = eve_server::combat::drones_salvage_tractor::attempt_salvage_wreck(
        &ctx,
        MEMBER_PILOT,
        ship_id,
        sys_id,
        wreck_id,
        1.0, // 100% bonus guarantee success
    )
    .await
    .expect("attempt_salvage_wreck");
    assert!(matches!(salvage_res, eve_server::combat::drones_salvage_tractor::SalvageOutcome::Success { .. }));

    // Verify Salvage Project progress and completion
    let proj_salvage = ctx.db.with_snapshot(|s| s.corp_projects.get(&p_salvage_id).cloned()).unwrap();
    assert_eq!(proj_salvage.current_quantity, 1);
    let salvage_status = ctx.db.with_snapshot(|s| s.alliance_war_state.project_details.get(&p_salvage_id).map(|d| d.status.clone())).unwrap();
    assert_eq!(salvage_status, "Completed");

    // Verify Member ISK rewards credited
    let bal = ctx.db.with_snapshot(|s| s.characters.get(&MEMBER_PILOT).map(|c| c.balance)).unwrap();
    assert!(bal > IskCents(100_000_00), "Member balance must increase from project rewards");
}

#[tokio::test]
async fn test_corporation_structure_default_reinforcement_and_anchoring_inheritance() {
    // parity: feat:corporation.structure-reinforce-default
    eve_testkit::attest(&["feat:corporation.structure-reinforce-default"]);

    let ctx = common::test_context().await;
    let mut sess1 = select_pilot(&ctx, 60_051, "testpilot1", "testpass1", CEO_PILOT).await;
    let mut sess2 = select_pilot(&ctx, 60_052, "testpilot2", "testpass2", MEMBER_PILOT).await;

    ctx.db.with_mut_snapshot(|s| {
        if let Some(corp) = s.corporations.get_mut(&CORP_ID) {
            corp.members.insert(MEMBER_PILOT);
        }
        if let Some(ch) = s.characters.get_mut(&MEMBER_PILOT) {
            ch.corporation_id = CORP_ID;
        }
    });

    // 1. Non-director / regular member calling SetStructureReinforceDefault is denied
    let non_dir_err = rpc(
        &ctx,
        &mut sess2,
        "corpRegistry",
        "SetStructureReinforceDefault",
        &[PyRep::Integer(19), PyRep::Integer(4)],
    )
    .await
    .expect_err("Non-director SetStructureReinforceDefault must fail");
    assert_eq!(non_dir_err.code, "CorpRoleDenied");

    // 2. Director sets default reinforcement hour to 19 and weekday to 4
    let set_rf = rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "SetStructureReinforceDefault",
        &[PyRep::Integer(19), PyRep::Integer(4)],
    )
    .await
    .expect("SetStructureReinforceDefault");
    assert_eq!(set_rf, PyRep::Integer(19));

    let get_rf = rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "GetStructureReinforceDefault",
        &[],
    )
    .await
    .expect("GetStructureReinforceDefault");
    assert_eq!(get_rf, PyRep::Integer(19));

    let get_rf_day = rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "GetStructureReinforceWeekdayDefault",
        &[],
    )
    .await
    .expect("GetStructureReinforceWeekdayDefault");
    assert_eq!(get_rf_day, PyRep::Integer(4));

    // 3. Anchor a new Upwell structure for the corporation
    let sys_id = 30_000_142;
    let ship_id = 99_004_001;
    let struct_item_id = 99_004_002;
    let astrahus_type_id = 35832;

    sess1.solar_system_id = Some(sys_id);
    sess1.ship_id = Some(ship_id);

    ctx.db.with_mut_snapshot(|s| {
        s.items.insert(ship_id, eve_db::InventoryItemRecord {
            item_id: ship_id,
            type_id: 606,
            owner_id: CEO_PILOT,
            location_id: sys_id as i64,
            flag: eve_db::InventoryFlag::None,
            quantity: 1,
            singleton: true,
            version: 1,
            dynamic_attributes: HashMap::new(),
        });
        s.items.insert(struct_item_id, eve_db::InventoryItemRecord {
            item_id: struct_item_id,
            type_id: astrahus_type_id,
            owner_id: CEO_PILOT,
            location_id: ship_id,
            flag: eve_db::InventoryFlag::CargoHold,
            quantity: 1,
            singleton: true,
            version: 1,
            dynamic_attributes: HashMap::new(),
        });
    });

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    sys_handle.spawn_ball(Ball {
        id: ship_id,
        type_id: 606,
        position: DVec3::new(100_000.0, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 500.0,
        inertia: 1.0,
        mass: 100_000.0,
        radius: 50.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    }).await.unwrap();

    let anchor_res = rpc(
        &ctx,
        &mut sess1,
        "structureDeployment",
        "Anchor",
        &[PyRep::Integer(struct_item_id), PyRep::String("Corp Astrahus".into())],
    )
    .await
    .expect("structureDeployment.Anchor");

    let new_structure_id = match anchor_res {
        PyRep::Integer(id) => id,
        other => match field(&other, "structureID") {
            PyRep::Integer(id) => id,
            _ => panic!("Expected structureID from Anchor, got {other:?}"),
        },
    };

    // 4. Verify the newly anchored structure inherited the corp default reinforce hour 19 and weekday 4
    let vuln = ctx.db.with_snapshot(|s| s.structure_vulnerability_windows.get(&new_structure_id).copied()).unwrap();
    assert_eq!(vuln.0, 19, "Newly anchored structure must inherit corp default reinforce hour 19");
    assert_eq!(vuln.1, 4, "Newly anchored structure must inherit corp default reinforce weekday 4");
}
