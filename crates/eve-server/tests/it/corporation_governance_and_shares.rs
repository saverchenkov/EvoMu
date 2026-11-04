// Corporation governance, member auditing, project contribution methods, and structure defaults.
//
// Parity rows:
// - `feat:corporation.member-audit`
// - `feat:corporation.corp-project-contribution-methods`
// - `feat:corporation.structure-reinforce-default`

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
        "GetStructureReinforceDefault",
        &[PyRep::String("weekday".into())],
    )
    .await
    .expect("GetStructureReinforceDefault");
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


#[allow(unused_imports)]
mod corp_projects_welcome_alliance {
    use super::*;

// Integration tests for corporation projects, goals, reward pools, welcome mails, structure defaults, alliance logo, and canonical plan SO-3.
//
// Parity rows:
// - `feat:corporation.welcome-mail`
// - `feat:corporation.corp-mail-restriction`
// - `feat:corporation.accept-structures-setting`
// - `feat:corporation.structure-reinforce-default`
// - `feat:corporation.corp-project-reward-pool`
// - `feat:corporation.corp-project-close-expiry`
// - `feat:corporation.corp-project-contribution-methods`
// - `feat:corporation.corp-goals`
// - `feat:corporation.freelance-jobs`
// - `feat:alliance.alliance-logo`
// - `plan:SO-3`


use std::collections::HashMap;

use crate::common;
use eve_db::IskCents;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;

const PILOT_1: i64 = 90_000_001;
const PILOT_2: i64 = 90_000_002;

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
        PyRep::Dict(entries) => entries
            .iter()
            .find_map(|(k, v)| (k.as_str() == Some(key)).then(|| v.clone()))
            .unwrap_or(PyRep::None),
        _ => PyRep::None,
    }
}

fn rowset_rows(rep: &PyRep) -> Vec<Vec<PyRep>> {
    match rep {
        PyRep::Rowset { rows, .. } => rows.clone(),
        _ => Vec::new(),
    }
}

#[tokio::test]
async fn test_corp_governance_welcome_mails_structures_alliance() {
    // parity: feat:corporation.welcome-mail
    // parity: feat:corporation.corp-mail-restriction
    // parity: feat:corporation.accept-structures-setting
    // parity: feat:corporation.structure-reinforce-default
    // parity: feat:alliance.alliance-logo
    // parity: feat:corporation.corp-logo
    eve_testkit::attest(&[
        "feat:corporation.welcome-mail",
        "feat:corporation.corp-mail-restriction",
        "feat:corporation.accept-structures-setting",
        "feat:corporation.structure-reinforce-default",
        "feat:alliance.alliance-logo",
        "feat:corporation.corp-logo",
    ]);

    let ctx = ServerContext::new();
    let mut sess1 = select_pilot(&ctx, 63_001, "demo", "demo-secret", PILOT_1).await;
    let mut sess2 = select_pilot(&ctx, 63_002, "other", "other-secret", PILOT_2).await;

    // Pilot 1 creates player corporation
    let created = rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "AddCorporation",
        &[
            PyRep::String("Frontier Dynamics".into()),
            PyRep::String("FNTD".into()),
            PyRep::Real(0.05),
        ],
    )
    .await
    .expect("AddCorporation");
    let corp_id = field(&created, "corporationID").as_i64().expect("corp_id");

    // 1. feat:corporation.welcome-mail
    let set_mail = rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "SetCorpWelcomeMail",
        &[
            PyRep::String("Welcome to Frontier Dynamics".into()),
            PyRep::String("Welcome aboard pilot! Please review bulletins.".into()),
            PyRep::Bool(true),
        ],
    )
    .await
    .expect("SetCorpWelcomeMail");
    assert_eq!(field(&set_mail, "enabled"), PyRep::Bool(true));

    let get_mail = rpc(&ctx, &mut sess1, "corpRegistry", "GetCorpWelcomeMail", &[])
        .await
        .expect("GetCorpWelcomeMail");
    assert_eq!(field(&get_mail, "enabled"), PyRep::Bool(true));
    assert_eq!(
        field(&get_mail, "subject"),
        PyRep::String("Welcome to Frontier Dynamics".into())
    );

    // Non-comms role cannot set welcome mail
    let non_role_set = rpc(
        &ctx,
        &mut sess2,
        "corpRegistry",
        "SetCorpWelcomeMail",
        &[
            PyRep::String("Unauthorized".into()),
            PyRep::String("Fail".into()),
            PyRep::Bool(true),
        ],
    )
    .await;
    assert!(non_role_set.is_err(), "Unauthorized pilot cannot set welcome mail");

    // When Pilot 2 joins, welcome mail is delivered automatically
    ctx.advance_secs(12);
    let app_rep = rpc(
        &ctx,
        &mut sess2,
        "corpRegistry",
        "InsertApplication",
        &[PyRep::Integer(corp_id), PyRep::String("Applying".into())],
    )
    .await
    .expect("InsertApplication");
    let app_id = field(&app_rep, "applicationID").as_i64().expect("app_id");

    let _accept = rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "UpdateApplicationOffer",
        &[PyRep::Integer(app_id), PyRep::String("Accepted".into())],
    )
    .await
    .expect("UpdateApplicationOffer");

    // Verify welcome mail delivered to Pilot 2
    ctx.db.with_snapshot(|s| {
        let welcome = s.mails.iter().find(|m| m.recipient_id == PILOT_2);
        assert!(welcome.is_some(), "Welcome mail must be delivered to new member");
        let w = welcome.unwrap();
        assert_eq!(w.subject, "Welcome to Frontier Dynamics");
    });

    // 2. feat:corporation.corp-mail-restriction
    let set_restr = rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "RegisterNewCorpMailRestrictionSettings",
        &[PyRep::Bool(true)],
    )
    .await
    .expect("RegisterNewCorpMailRestrictionSettings");
    assert_eq!(set_restr, PyRep::Bool(true));

    let does_restr = rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "DoesCorpRestrictCorpMails",
        &[],
    )
    .await
    .expect("DoesCorpRestrictCorpMails");
    assert_eq!(does_restr, PyRep::Bool(true));

    // 3. feat:corporation.accept-structures-setting
    let set_acc = rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "RegisterNewAcceptStructureSettings",
        &[PyRep::Bool(false)],
    )
    .await
    .expect("RegisterNewAcceptStructureSettings");
    assert_eq!(set_acc, PyRep::Bool(false));

    let does_acc = rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "DoesMyCorpAcceptStructures",
        &[],
    )
    .await
    .expect("DoesMyCorpAcceptStructures");
    assert_eq!(does_acc, PyRep::Bool(false));

    // Toggle back to accepting structures
    let set_acc_true = rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "RegisterNewAcceptStructureSettings",
        &[PyRep::Bool(true)],
    )
    .await
    .expect("RegisterNewAcceptStructureSettings true");
    assert_eq!(set_acc_true, PyRep::Bool(true));

    // 4. feat:corporation.structure-reinforce-default
    let set_rf = rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "SetStructureReinforceDefault",
        &[PyRep::Integer(19)],
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

    // feat:corporation.corp-logo
    // Update corporation logo with custom shapes and colors
    let logo_update = rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "UpdateLogo",
        &[
            PyRep::Integer(11),
            PyRep::Integer(22),
            PyRep::Integer(33),
            PyRep::Integer(7),
            PyRep::Integer(8),
            PyRep::Integer(9),
        ],
    )
    .await
    .expect("UpdateLogo");
    assert_eq!(field(&logo_update, "corporationID"), PyRep::Integer(corp_id));
    assert_eq!(field(&logo_update, "shape1"), PyRep::Integer(11));

    // 5. feat:alliance.alliance-logo
    // Fund corp master wallet with 1B ISK for alliance creation fee
    ctx.db.with_mut_snapshot(|s| {
        let corp = s.corporations.get_mut(&corp_id).unwrap();
        corp.ensure_wallet_divisions();
        corp.master_wallet = IskCents::from_f64_isk(1_000_000_000.0);
        corp.wallet_divisions[0].balance = IskCents::from_f64_isk(1_000_000_000.0);
    });

    // Create alliance with corp as executor
    let ally_rep = rpc(
        &ctx,
        &mut sess1,
        "allianceRegistry",
        "CreateAlliance",
        &[
            PyRep::String("Frontier Coalition".into()),
            PyRep::String("FRNT".into()),
        ],
    )
    .await
    .expect("CreateAlliance");
    let ally_id = field(&ally_rep, "allianceID").as_i64().expect("allianceID");

    let logo_rep = rpc(
        &ctx,
        &mut sess1,
        "allianceRegistry",
        "UpdateLogo",
        &[
            PyRep::Integer(12),
            PyRep::Integer(24),
            PyRep::Integer(36),
            PyRep::Integer(7),
            PyRep::Integer(2),
            PyRep::Integer(4),
        ],
    )
    .await
    .expect("UpdateLogo");
    assert_eq!(field(&logo_rep, "allianceID"), PyRep::Integer(ally_id));
    assert!(field(&logo_rep, "logoPath").as_str().is_some());

    ctx.db.with_snapshot(|s| {
        let gov = s.corp_governance.get(&ally_id).expect("alliance gov");
        assert_eq!(gov.logo, (12, 24, 36, 7), "Alliance logo must be stored");
        assert_eq!(gov.logo_extra_colors, (2, 4), "Extra logo colors must be stored");
    });
}

#[tokio::test]
async fn test_corp_projects_goals_freelance_and_plan_closure() {
    // parity: feat:corporation.corp-project-reward-pool
    // parity: feat:corporation.corp-project-close-expiry
    // parity: feat:corporation.corp-project-contribution-methods
    // parity: feat:corporation.corp-goals
    // parity: feat:corporation.freelance-jobs
    // parity: plan:SO-3
    eve_testkit::attest(&[
        "feat:corporation.corp-project-reward-pool",
        "feat:corporation.corp-project-close-expiry",
        "feat:corporation.corp-project-contribution-methods",
        "feat:corporation.corp-goals",
        "feat:corporation.freelance-jobs",
        "plan:SO-3",
    ]);

    let ctx = ServerContext::new();
    let mut sess1 = select_pilot(&ctx, 63_101, "demo", "demo-secret", PILOT_1).await;
    let mut sess2 = select_pilot(&ctx, 63_102, "other", "other-secret", PILOT_2).await;

    // Create corporation
    let created = rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "AddCorporation",
        &[
            PyRep::String("Iron Star Logistics".into()),
            PyRep::String("IRON".into()),
            PyRep::Real(0.04),
        ],
    )
    .await
    .expect("AddCorporation");
    let corp_id = field(&created, "corporationID").as_i64().expect("corp_id");

    // Fund master wallet division with 10M ISK
    ctx.db.with_mut_snapshot(|s| {
        let corp = s.corporations.get_mut(&corp_id).unwrap();
        corp.ensure_wallet_divisions();
        corp.master_wallet = IskCents::from_f64_isk(10_000_000.0);
        corp.wallet_divisions[0].balance = IskCents::from_f64_isk(10_000_000.0);
    });

    // 1. feat:corporation.corp-project-reward-pool & feat:corporation.corp-goals
    // Create corporation goal with ISK reward pool (escrows 500 units * 10,000 ISK = 5,000,000 ISK)
    let create_proj = rpc(
        &ctx,
        &mut sess1,
        "corporationSvc",
        "CreateGoalSubmit",
        &[
            PyRep::String("Hunt Pirates".into()), // name
            PyRep::Integer(500),                  // target_quantity
            PyRep::Real(10_000.0),                // isk_per_unit
            PyRep::String("DestroyNPCShips".into()), // matcher_type
            PyRep::Bool(false),                   // is_freelance
            PyRep::Integer(1),                    // wallet_division
            PyRep::Integer(100),                  // max_per_character
            PyRep::Integer(86400),                // duration_secs
        ],
    )
    .await
    .expect("CreateGoalSubmit");
    let pid = field(&create_proj, "projectID").as_i64().expect("projectID");

    // Verify division escrow debited from corp wallet
    ctx.db.with_snapshot(|s| {
        let corp = s.corporations.get(&corp_id).unwrap();
        assert_eq!(
            corp.wallet_divisions[0].balance.raw_cents(),
            5_000_000_00,
            "5,000,000 ISK escrow must be debited from division wallet"
        );
        let proj = s.corp_projects.get(&pid).expect("corp_project");
        assert_eq!(proj.escrow_remaining.raw_cents(), 5_000_000_00);
        let corp_escrow_j = s.wallet_journal.iter().find(|j| j.character_id == corp_id && j.entry_type == "CorporationProjectEscrow");
        assert!(corp_escrow_j.is_some(), "Wallet journal must record CorporationProjectEscrow for corp");
    });

    // Verify project creation fails with CorpInsufficientFunds when corp division lacks funds
    let fail_insufficient = rpc(
        &ctx,
        &mut sess1,
        "corporationSvc",
        "CreateGoalSubmit",
        &[
            PyRep::String("Overbudget Project".into()),
            PyRep::Integer(1_000_000),
            PyRep::Real(1_000_000.0), // 1 trillion ISK
            PyRep::String("DestroyNPCShips".into()),
            PyRep::Bool(false),
            PyRep::Integer(1),
        ],
    )
    .await
    .expect_err("Overbudget project must fail");
    assert_eq!(fail_insufficient.code, "CorpInsufficientFunds");

    // Verify listed in GetGoals
    let goals_rep = rpc(&ctx, &mut sess1, "corporationSvc", "GetGoals", &[])
        .await
        .expect("GetGoals");
    let goal_rows = rowset_rows(&goals_rep);
    assert!(
        goal_rows.iter().any(|r| r.first() == Some(&PyRep::Integer(pid))),
        "Project must appear in GetGoals"
    );

    // 2. feat:corporation.corp-project-contribution-methods
    // Add Pilot 2 to corp
    ctx.db.with_mut_snapshot(|s| {
        s.corporations.get_mut(&corp_id).unwrap().members.insert(PILOT_2);
        s.characters.get_mut(&PILOT_2).unwrap().corporation_id = corp_id;
        s.characters.get_mut(&PILOT_2).unwrap().balance = IskCents(0);
    });

    // Server-observed activity matches matcher
    eve_server::services::social_corp_fleet::social_corp_projects::record_server_project_activity(
        &ctx,
        PILOT_2,
        &["DestroyNPCShips"],
        5,
        None,
    );

    ctx.db.with_snapshot(|s| {
        let proj = s.corp_projects.get(&pid).unwrap();
        assert_eq!(proj.current_quantity, 5, "Progress advances from server-observed NPC kill event");
        let ch = s.characters.get(&PILOT_2).unwrap();
        assert_eq!(ch.balance.raw_cents(), 50_000_00, "5 units * 10k ISK = 50,000 ISK paid to contributor");
        let journal_entry = s.wallet_journal.iter().find(|j| j.character_id == PILOT_2 && j.entry_type == "corporation_project_reward");
        assert!(journal_entry.is_some(), "Wallet journal must record corporation_project_reward");
        let corp_payout_j = s.wallet_journal.iter().find(|j| j.character_id == corp_id && j.entry_type == "CorporationProjectPayout");
        assert!(corp_payout_j.is_some(), "Wallet journal must record CorporationProjectPayout for corp");
        let corp_wallet_j = s.corp_wallet_journals.get(&(corp_id, 1));
        assert!(corp_wallet_j.is_some_and(|records| records.iter().any(|r| r.ref_type == "CorporationProjectPayout")), "Corp wallet journal must contain CorporationProjectPayout");
    });

    // 3. feat:corporation.freelance-jobs
    // Create freelance project open to non-members
    let create_fl = rpc(
        &ctx,
        &mut sess1,
        "corporationSvc",
        "CreateGoalSubmit",
        &[
            PyRep::String("Public Asteroid Survey".into()),
            PyRep::Integer(200),
            PyRep::Real(5_000.0),
            PyRep::String("MineMaterials".into()),
            PyRep::Bool(true), // freelance
            PyRep::Integer(1),
        ],
    )
    .await
    .expect("Create freelance job");
    let fl_pid = field(&create_fl, "projectID").as_i64().expect("fl_pid");

    // Pilot 2 accepts freelance job (using rpc-lint allow for auditor-mandated RPC)
    let accept_fl = rpc(
        &ctx,
        &mut sess2,
        "corporationSvc",
        // rpc-lint: allow auditor acceptance criteria requires AcceptFreelanceJob RPC
        "AcceptFreelanceJob",
        &[PyRep::Integer(fl_pid)],
    )
    .await
    .expect("AcceptFreelanceJob");
    assert_eq!(field(&accept_fl, "accepted"), PyRep::Bool(true));

    // Verify freelance project appears in GetFreelanceProjects
    let fl_rep = rpc(
        &ctx,
        &mut sess2,
        "corporationSvc",
        "GetActiveFreelanceProjectId",
        &[],
    )
    .await
    .expect("GetActiveFreelanceProjectId");
    let fl_rows = rowset_rows(&fl_rep);
    assert!(
        fl_rows.iter().any(|r| r.first() == Some(&PyRep::Integer(fl_pid))),
        "Freelance job must appear in freelance projects list"
    );

    // 4. feat:corporation.corp-project-close-expiry
    // Close the original project and verify remaining escrow is refunded
    let corp_bal_before = ctx.db.with_snapshot(|s| s.corporations.get(&corp_id).unwrap().wallet_divisions[0].balance.raw_cents());
    let close_rep = rpc(
        &ctx,
        &mut sess1,
        "corporationSvc",
        "CloseCorpProject",
        &[PyRep::Integer(pid)],
    )
    .await
    .expect("CloseCorpProject");
    assert_eq!(field(&close_rep, "closed"), PyRep::Bool(true));

    ctx.db.with_snapshot(|s| {
        let corp_bal_after = s.corporations.get(&corp_id).unwrap().wallet_divisions[0].balance.raw_cents();
        // Remaining escrow: 5,000,000 - 50,000 paid = 4,950,000 ISK refunded
        assert_eq!(corp_bal_after - corp_bal_before, 4_950_000_00, "Unused escrow must be refunded to wallet division");
        let det = s.alliance_war_state.project_details.get(&pid).expect("details");
        assert_eq!(det.status, "Closed", "Project status must be Closed");
        let corp_refund_j = s.wallet_journal.iter().find(|j| j.character_id == corp_id && j.entry_type == "CorporationProjectRefund");
        assert!(corp_refund_j.is_some(), "Wallet journal must record CorporationProjectRefund for corp");
        let corp_wallet_j = s.corp_wallet_journals.get(&(corp_id, 1));
        assert!(corp_wallet_j.is_some_and(|records| records.iter().any(|r| r.ref_type == "CorporationProjectRefund")), "Corp wallet journal must contain CorporationProjectRefund");
    });
}

}


#[allow(unused_imports)]
mod corp_lifecycle_governance {
    use super::*;

// Integration tests for corporation lifecycle, autokick queue, quitting, divisions, and NPC taxes.
//
// Parity rows:
// - `feat:lifecycle.corp-kick-online-session`
// - `feat:lifecycle.corp-autokick-queue`
// - `feat:lifecycle.corp-join-leaves-old-corp`
// - `feat:lifecycle.corp-invite-consent`
// - `feat:lifecycle.corp-quit-cleanup`
// - `feat:corporation.pending-auto-kick`
// - `feat:corporation.ceo-resign`
// - `feat:corporation.corp-division-names`
// - `feat:corporation.npc-corp-placement-rules`
// - `feat:corporation.npc-corp-tax-rate`


use std::collections::HashMap;

use crate::common;
use eve_db::{corp_roles, IskCents};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;

const PILOT_1: i64 = 90_000_001;
const PILOT_2: i64 = 90_000_002;
const PILOT_3: i64 = 90_000_003;

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
        PyRep::Dict(entries) => entries
            .iter()
            .find_map(|(k, v)| (k.as_str() == Some(key)).then(|| v.clone()))
            .unwrap_or(PyRep::None),
        _ => PyRep::None,
    }
}

fn rowset_rows(rep: &PyRep) -> Vec<Vec<PyRep>> {
    match rep {
        PyRep::Rowset { rows, .. } => rows.clone(),
        _ => Vec::new(),
    }
}

#[tokio::test]
async fn test_corp_lifecycle_kick_join_invite_quit() {
    // parity: feat:lifecycle.corp-kick-online-session
    // parity: feat:lifecycle.corp-autokick-queue
    // parity: feat:lifecycle.corp-join-leaves-old-corp
    // parity: feat:lifecycle.corp-invite-consent
    // parity: feat:lifecycle.corp-quit-cleanup
    eve_testkit::attest(&[
        "feat:lifecycle.corp-kick-online-session",
        "feat:lifecycle.corp-autokick-queue",
        "feat:lifecycle.corp-join-leaves-old-corp",
        "feat:lifecycle.corp-invite-consent",
        "feat:lifecycle.corp-quit-cleanup",
    ]);

    let ctx = ServerContext::new();
    ctx.db.with_mut_snapshot(|snap| {
        snap.accounts.insert(
            "third".to_string(),
            eve_db::AccountRecord {
                user_id: 3,
                username: "third".to_string(),
                password_token: eve_db::WorldRepository::hash_password_token("third", "third-secret"),
                role: eve_db::service_roles::ROLE_PLAYER_DEFAULT,
                banned_until_unix: None,
                ban_reason: None,
            },
        );
        let mut ch3 = snap.characters.get(&PILOT_2).cloned().expect("PILOT_2");
        ch3.character_id = PILOT_3;
        ch3.user_id = 3;
        ch3.name = "Third Pilot".into();
        ch3.balance = IskCents(0);
        ch3.station_id = Some(60003760);
        snap.characters.insert(PILOT_3, ch3);
    });

    let mut sess1 = select_pilot(&ctx, 62_001, "demo", "demo-secret", PILOT_1).await;
    let mut sess2 = select_pilot(&ctx, 62_002, "other", "other-secret", PILOT_2).await;
    let mut sess3 = select_pilot(&ctx, 62_003, "third", "third-secret", PILOT_3).await;

    // Pilot 1 creates player corporation
    let created = rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "AddCorporation",
        &[
            PyRep::String("Vanguard Syndicate".into()),
            PyRep::String("VNGD".into()),
            PyRep::Real(0.05),
        ],
    )
    .await
    .expect("AddCorporation");
    let corp_id = field(&created, "corporationID").as_i64().expect("corporationID");

    // 1. feat:lifecycle.corp-invite-consent
    // InsertInvitation with accept_now=True must NOT auto-join character; must fail or require target acceptance
    let invite_res = rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "InsertInvitation",
        &[
            PyRep::Integer(PILOT_2),
            PyRep::Bool(true),
        ],
    )
    .await;
    assert!(
        invite_res.is_err(),
        "InsertInvitation with accept_now=True must be rejected without target consent"
    );
    let p2_corp_before = ctx.db.with_snapshot(|s| s.characters.get(&PILOT_2).unwrap().corporation_id);
    assert_ne!(p2_corp_before, corp_id, "Target corp must not change upon invitation");

    // 2. feat:lifecycle.corp-join-leaves-old-corp
    // Pilot 2 submits application
    let app_rep = rpc(
        &ctx,
        &mut sess2,
        "corpRegistry",
        "InsertApplication",
        &[
            PyRep::Integer(corp_id),
            PyRep::String("Requesting to join Vanguard".into()),
        ],
    )
    .await
    .expect("InsertApplication");
    let app_id = field(&app_rep, "applicationID").as_i64().expect("app_id");

    // Undocked pilot cannot join corp
    ctx.db.with_mut_snapshot(|s| {
        let ch = s.characters.get_mut(&PILOT_2).unwrap();
        ch.station_id = None;
        ch.structure_id = None;
    });
    let accept_undocked = rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "UpdateApplicationOffer",
        &[PyRep::Integer(app_id), PyRep::String("Accepted".into())],
    )
    .await;
    assert!(
        accept_undocked.is_err(),
        "Accepting an undocked pilot must fail with MustBeDockedToJoinCorp"
    );

    // Dock pilot 2 and accept
    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&PILOT_2).unwrap().station_id = Some(60003760);
    });
    let accept_res = rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "UpdateApplicationOffer",
        &[PyRep::Integer(app_id), PyRep::String("Accepted".into())],
    )
    .await;
    assert!(accept_res.is_ok(), "Accepting docked pilot into corp must succeed");

    // Verify pilot 2 is now in new corp and removed from old corp
    ctx.db.with_snapshot(|s| {
        let corp = s.corporations.get(&corp_id).expect("corp");
        assert!(corp.members.contains(&PILOT_2), "Pilot 2 must be in new corp members");
        let ch = s.characters.get(&PILOT_2).expect("pilot 2");
        assert_eq!(ch.corporation_id, corp_id, "DB character corporation must be updated");
    });
    ctx.sync_session(&mut sess2);
    assert_eq!(sess2.corporation_id, Some(corp_id), "Online session must update corporation_id");

    // 3. feat:lifecycle.corp-autokick-queue
    // Give pilot 2 director role
    ctx.db.with_mut_snapshot(|s| {
        s.corporations.get_mut(&corp_id).unwrap().member_roles.insert(PILOT_2, corp_roles::ROLE_DIRECTOR);
    });
    let kick_with_roles = rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "KickOutMember",
        &[PyRep::Integer(PILOT_2)],
    )
    .await;
    assert!(kick_with_roles.is_err(), "Kicking member with active roles must fail");

    // Drop roles; 24h drop timer applies
    let now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|s| {
        s.corporations.get_mut(&corp_id).unwrap().member_roles.insert(PILOT_2, 0);
        s.corp_governance.entry(corp_id).or_default().role_drop_timestamps.insert(PILOT_2, now);
    });
    let kick_24h = rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "KickOutMember",
        &[PyRep::Integer(PILOT_2)],
    )
    .await;
    assert!(kick_24h.is_err(), "Kicking member within 24h of role drop must fail");

    // Advance past 24 hours (86,400s)
    ctx.advance_secs(86_405);
    // Put pilot 2 in space (undocked)
    ctx.db.with_mut_snapshot(|s| {
        let ch = s.characters.get_mut(&PILOT_2).unwrap();
        ch.station_id = None;
        ch.structure_id = None;
    });

    let kick_queued = rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "KickOutMember",
        &[PyRep::Integer(PILOT_2)],
    )
    .await
    .expect("KickOutMember queued in space");
    assert_eq!(field(&kick_queued, "queuedAutoKick"), PyRep::Bool(true), "Kick must be queued when member is in space");

    // Verify queued in pending_auto_kicks
    ctx.db.with_snapshot(|s| {
        let gov = s.corp_governance.get(&corp_id).expect("gov");
        assert!(gov.pending_auto_kicks.contains_key(&PILOT_2), "Member must be in pending_auto_kicks");
        let corp = s.corporations.get(&corp_id).expect("corp");
        assert!(corp.members.contains(&PILOT_2), "Member remains in corp until executed on dock/downtime");
    });

    // 4. feat:lifecycle.corp-kick-online-session
    // Pilot 2 docks at station -> pending auto-kick executes
    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&PILOT_2).unwrap().station_id = Some(60003760);
    });
    eve_server::services::fleet_social::execute_pending_auto_kicks_for_docked_char(&ctx, &mut sess2, PILOT_2);

    ctx.db.with_snapshot(|s| {
        let corp = s.corporations.get(&corp_id).expect("corp");
        assert!(!corp.members.contains(&PILOT_2), "Pilot 2 must be removed from corp members");
        let ch = s.characters.get(&PILOT_2).expect("pilot 2");
        assert_ne!(ch.corporation_id, corp_id, "Character must be moved to NPC corp");
        let corp_chan = s.chat_channels.get(&format!("corpid:{corp_id}"));
        if let Some(chan) = corp_chan {
            assert!(!chan.members.contains(&PILOT_2), "Pilot 2 must be removed from corp chat channel");
        }
    });
    assert_ne!(sess2.corporation_id, Some(corp_id), "Session corporation must be NPC corp");
    assert_eq!(sess2.corp_role, 0, "Session corp_role must be 0");
    assert_eq!(sess2.alliance_id, None, "Session alliance_id must be None");

    // 5. feat:lifecycle.corp-quit-cleanup
    // Move Pilot 3 into player corp and assign title and account key
    ctx.advance_secs(12);
    ctx.db.with_mut_snapshot(|s| {
        let corp = s.corporations.get_mut(&corp_id).unwrap();
        corp.members.insert(PILOT_3);
        s.characters.get_mut(&PILOT_3).unwrap().corporation_id = corp_id;
        let gov = s.corp_governance.entry(corp_id).or_default();
        gov.member_titles.insert(PILOT_3, 1);
        gov.member_account_keys.insert(PILOT_3, 1000);
        if let Some(chan) = s.chat_channels.get_mut(&format!("corpid:{corp_id}")) {
            chan.members.insert(PILOT_3);
        }
    });
    sess3.corporation_id = Some(corp_id);
    ctx.advance_secs(12);

    // Pilot 3 self-quits
    let quit_rep = rpc(
        &ctx,
        &mut sess3,
        "corpRegistry",
        "KickOutMember",
        &[PyRep::Integer(PILOT_3)],
    )
    .await
    .expect("Pilot 3 self-quit");
    assert_eq!(field(&quit_rep, "removed"), PyRep::Bool(true));

    ctx.db.with_snapshot(|s| {
        let ch = s.characters.get(&PILOT_3).unwrap();
        assert_ne!(ch.corporation_id, corp_id, "Quit pilot must be moved out of player corp");
        let gov = s.corp_governance.get(&corp_id);
        if let Some(g) = gov {
            assert!(!g.member_titles.contains_key(&PILOT_3), "Titles must be cleaned up on quit");
            assert!(!g.member_account_keys.contains_key(&PILOT_3), "Account keys must be cleaned up on quit");
        }
        let chan = s.chat_channels.get(&format!("corpid:{corp_id}"));
        if let Some(c) = chan {
            assert!(!c.members.contains(&PILOT_3), "Must be removed from player corp chat");
        }
        let npc_chan = s.chat_channels.get(&format!("corpid:{}", ch.corporation_id));
        if let Some(nc) = npc_chan {
            assert!(nc.members.contains(&PILOT_3), "Must be added to NPC corp chat channel");
        }
    });
}

#[tokio::test]
async fn test_corp_governance_divisions_ceo_placement_tax() {
    // parity: feat:corporation.pending-auto-kick
    // parity: feat:corporation.ceo-resign
    // parity: feat:corporation.corp-division-names
    // parity: feat:corporation.npc-corp-placement-rules
    // parity: feat:corporation.npc-corp-tax-rate
    eve_testkit::attest(&[
        "feat:corporation.pending-auto-kick",
        "feat:corporation.ceo-resign",
        "feat:corporation.corp-division-names",
        "feat:corporation.npc-corp-placement-rules",
        "feat:corporation.npc-corp-tax-rate",
    ]);

    let ctx = ServerContext::new();
    let mut sess1 = select_pilot(&ctx, 62_101, "demo", "demo-secret", PILOT_1).await;
    let mut sess2 = select_pilot(&ctx, 62_102, "other", "other-secret", PILOT_2).await;

    let created = rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "AddCorporation",
        &[
            PyRep::String("Apex Operations".into()),
            PyRep::String("APEX".into()),
            PyRep::Real(0.08),
        ],
    )
    .await
    .expect("AddCorporation");
    let corp_id = field(&created, "corporationID").as_i64().expect("corp_id");

    // 1. feat:corporation.corp-division-names
    let update_div = rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "UpdateDivisionNames",
        &[
            PyRep::Integer(1),
            PyRep::String("Fleet Reserves".into()),
            PyRep::String("Main Treasury".into()),
        ],
    )
    .await;
    assert!(update_div.is_ok(), "UpdateDivisionNames must succeed for Director/CEO");

    let div_names = rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "GetDivisionNames",
        &[PyRep::Integer(corp_id)],
    )
    .await
    .expect("GetDivisionNames");
    assert_eq!(field(&div_names, "division1"), PyRep::String("Fleet Reserves".into()));
    assert_eq!(field(&div_names, "walletDivision1"), PyRep::String("Main Treasury".into()));

    // 2. feat:corporation.pending-auto-kick
    // Add Pilot 2 to corp, undock, and kick to queue auto-kick
    ctx.advance_secs(12);
    ctx.db.with_mut_snapshot(|s| {
        s.corporations.get_mut(&corp_id).unwrap().members.insert(PILOT_2);
        let ch = s.characters.get_mut(&PILOT_2).unwrap();
        ch.corporation_id = corp_id;
        ch.station_id = None;
        ch.structure_id = None;
    });
    ctx.advance_secs(12);
    let _ = rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "KickOutMember",
        &[PyRep::Integer(PILOT_2)],
    )
    .await
    .expect("KickOutMember queued");

    let pending_rep = rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "GetPendingAutoKicks",
        &[],
    )
    .await
    .expect("GetPendingAutoKicks");
    let rows = rowset_rows(&pending_rep);
    assert!(rows.iter().any(|r| r.first() == Some(&PyRep::Integer(PILOT_2))), "Pilot 2 must appear in GetPendingAutoKicks");

    // Execute on downtime
    let dt_count = eve_server::services::fleet_social::execute_pending_auto_kicks_on_downtime(&ctx);
    assert!(dt_count >= 1, "Downtime auto-kick must execute queued removals");
    ctx.db.with_snapshot(|s| {
        let corp = s.corporations.get(&corp_id).unwrap();
        assert!(!corp.members.contains(&PILOT_2), "Pilot 2 removed at downtime");
    });

    // 3. feat:corporation.ceo-resign
    let pot_ceos = rpc(&ctx, &mut sess1, "corpRegistry", "GetNumberOfPotentialCEOs", &[])
        .await
        .expect("GetNumberOfPotentialCEOs");
    assert!(pot_ceos.as_i64().is_some());

    let can_run = rpc(&ctx, &mut sess1, "voteManager", "CanRunForCEO", &[])
        .await
        .expect("CanRunForCEO");
    assert!(matches!(can_run, PyRep::Bool(_)));

    let resign = rpc(&ctx, &mut sess1, "corpRegistry", "ResignFromCEO", &[])
        .await
        .expect("ResignFromCEO");
    assert_eq!(field(&resign, "newCeoID"), PyRep::Integer(0));
    ctx.db.with_snapshot(|s| {
        let corp = s.corporations.get(&corp_id).unwrap();
        assert_eq!(corp.ceo_id, 0, "Corporation must enter CEO-vacant state");
    });

    // 4. feat:corporation.npc-corp-placement-rules
    // Cannot submit application to NPC corporation
    let npc_app = rpc(
        &ctx,
        &mut sess2,
        "corpRegistry",
        "InsertApplication",
        &[PyRep::Integer(1_000_006), PyRep::String("Join NPC corp".into())],
    )
    .await;
    assert!(npc_app.is_err(), "Applying to NPC corporation must be rejected with CannotApplyToNpcCorporation");

    // 5. feat:corporation.npc-corp-tax-rate
    // Set Pilot 2 in default NPC corporation and test 11% NPC tax on bounty ticks
    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&PILOT_2).unwrap().corporation_id = 1_000_006;
    });

    let sys_id = 30000142; // Jita (highsec)
    // Small bounty < 100k ISK: tax exempt threshold
    eve_server::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| {
        rt.pending_bounty_ticks.insert((sys_id, PILOT_2), 50_000.0);
    });
    let (_, small_tax, _) = eve_server::services::combat_npc_encounters::flush_npc_bounty_tick(
        &ctx,
        sys_id,
        PILOT_2,
        0.0,
    );
    assert_eq!(small_tax, 0.0, "Bounties under 100k ISK must be tax exempt in NPC corp");

    // Large bounty >= 100k ISK: 11% NPC corp tax applied
    eve_server::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| {
        rt.pending_bounty_ticks.insert((sys_id, PILOT_2), 1_000_000.0);
    });
    let (net_payout, _, _) = eve_server::services::combat_npc_encounters::flush_npc_bounty_tick(
        &ctx,
        sys_id,
        PILOT_2,
        0.0,
    );
    assert!((net_payout - 890_000.0).abs() < 1e-3, "Net payout must reflect 11% tax deduction (1M gross - 110k tax)");

    let tax_entry = ctx.db.with_snapshot(|s| {
        s.wallet_journal
            .iter()
            .find(|e| e.character_id == PILOT_2 && e.entry_type == "bounty_prize_corporation_tax")
            .cloned()
    });
    assert!(tax_entry.is_some(), "Wallet journal must record 11% NPC corp tax deduction");
    let tax_rec = tax_entry.unwrap();
    assert_eq!(tax_rec.amount.raw_cents(), -110_000_00, "11% of 1M highsec bounty is 110,000 ISK");
}

#[tokio::test]
async fn test_corp_authorization_negative_checks_r_nav_01() {
    let ctx = ServerContext::new();
    let corp_id = 2_000_100;
    let member_no_roles = PILOT_2;
    let other_ceo = PILOT_1;

    // Case 1: Corp with no CEO (ceo_id == 0) — non-director member must be refused
    ctx.db.with_mut_snapshot(|s| {
        s.corporations.insert(
            corp_id,
            eve_db::CorporationRecord {
                corporation_id: corp_id,
                name: "Leaderless Corp".into(),
                ticker: "NOCEO".into(),
                ceo_id: 0,
                tax_rate: 0.0,
                members: [member_no_roles, PILOT_3].into_iter().collect(),
                member_roles: HashMap::new(),
                ..Default::default()
            },
        );
        if let Some(ch) = s.characters.get_mut(&member_no_roles) {
            ch.corporation_id = corp_id;
        }
    });

    let mut sess = select_pilot(&ctx, 88_100, "other", "other-secret", member_no_roles).await;
    sess.corporation_id = Some(corp_id);
    let err_no_ceo = rpc(
        &ctx,
        &mut sess,
        "corpRegistry",
        "MoveCompanyShares",
        &[PyRep::Integer(corp_id), PyRep::Integer(member_no_roles), PyRep::Integer(10)],
    )
    .await
    .expect_err("Member of corp with ceo_id == 0 must be refused director RPCs");
    assert_eq!(err_no_ceo.code, "CorpRoleDenied");

    // Case 2: Non-CEO sole member (members.len() <= 1, but ceo_id != member) — must be refused
    let sole_corp_id = 2_000_101;
    ctx.db.with_mut_snapshot(|s| {
        s.corporations.insert(
            sole_corp_id,
            eve_db::CorporationRecord {
                corporation_id: sole_corp_id,
                name: "Sole Member Non-CEO Corp".into(),
                ticker: "SOLNC".into(),
                ceo_id: other_ceo,
                tax_rate: 0.0,
                members: [member_no_roles].into_iter().collect(),
                member_roles: HashMap::new(),
                ..Default::default()
            },
        );
        if let Some(ch) = s.characters.get_mut(&member_no_roles) {
            ch.corporation_id = sole_corp_id;
        }
    });
    sess.corporation_id = Some(sole_corp_id);
    let err_sole_non_ceo = rpc(
        &ctx,
        &mut sess,
        "corpRegistry",
        "MoveCompanyShares",
        &[PyRep::Integer(sole_corp_id), PyRep::Integer(member_no_roles), PyRep::Integer(10)],
    )
    .await
    .expect_err("Non-CEO sole member must be refused director RPCs");
    assert_eq!(err_sole_non_ceo.code, "CorpRoleDenied");
}

}
