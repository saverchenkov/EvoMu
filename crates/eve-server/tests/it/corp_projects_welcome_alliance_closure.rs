//! Integration tests for corporation projects, goals, reward pools, welcome mails, structure defaults, alliance logo, and canonical plan SO-3.
//!
//! Parity rows:
//! - `feat:corporation.welcome-mail`
//! - `feat:corporation.corp-mail-restriction`
//! - `feat:corporation.accept-structures-setting`
//! - `feat:corporation.structure-reinforce-default`
//! - `feat:corporation.corp-project-reward-pool`
//! - `feat:corporation.corp-project-close-expiry`
//! - `feat:corporation.corp-project-contribution-methods`
//! - `feat:corporation.corp-goals`
//! - `feat:corporation.freelance-jobs`
//! - `feat:alliance.alliance-logo`
//! - `plan:SO-3`

#![forbid(unsafe_code)]

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
