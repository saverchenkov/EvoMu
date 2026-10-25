//! Authentic state-backed integration tests for Corporation Wars, Bills, and Mutual War Invites:
//! - `billMgr`: `CharPayBill`, `GetAutomaticPaySettings`, `GetCorporationBills`, `GetCorporationBillsReceivable`, `PayCorporationBill`, `SendAutomaticPaySettings`
//! - `warRegistry`: `UnBind`, `timeDeclared`, `timeStarted`, `timeFinished`
//! - `warStatisticMgr`: `GetBaseInfo`, `GetKillMail`, `GetKills`, `GetKillsByGroup`
//! - `warsInfoMgr`: `GetPublicWarInfo`, `GetTop50`, `GetWarsByOwnerID`, `GetWarsByOwners`, `GetWarsForStructure`, `GetWarsRequiringAssistance`
//! - `peaceTreatyMgr`: `GetPeaceTreatiesForSession`
//! - `mutualWarInviteMgr`: `GetPendingInvitesForSession`, `IsCorpInvitesBlockedPlayer`, `RespondToInviteByPlayer`, `SendInviteByPlayer`, `SetInvitesBlockedByPlayer`, `WithdrawInviteByPlayer`

#![forbid(unsafe_code)]

use std::collections::HashMap;

use crate::common;
use eve_db::{IskCents, UpwellStructureRecord};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::wallet_serverops::create_bill;

const PILOT_1: i64 = 90_000_001;
const PILOT_2: i64 = 90_000_002;
const PILOT_3: i64 = 90_000_003;

async fn select_pilot(
    ctx: &ServerContext,
    sid: u64,
    user: &str,
    pass: &str,
    char_id: i64,
) -> SessionState {
    let mut sess = SessionState::new(sid);
    common::login_pilot(ctx, &mut sess, user, pass).await;
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

fn rowset_data(rep: &PyRep) -> (Vec<String>, Vec<Vec<PyRep>>) {
    match rep {
        PyRep::Rowset { columns, rows } => (columns.clone(), rows.clone()),
        _ => (Vec::new(), Vec::new()),
    }
}

fn set_corp_balance(ctx: &ServerContext, corp_id: i64, isk: f64) {
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(corp) = snap.corporations.get_mut(&corp_id) {
            corp.ensure_wallet_divisions();
            corp.wallet_divisions[0].balance = IskCents::from_f64_isk(isk);
            for d in &mut corp.wallet_divisions {
                d.balance = IskCents::from_f64_isk(isk);
            }
            corp.master_wallet = IskCents::from_f64_isk(isk);
        }
    });
}

fn insert_hqs(ctx: &ServerContext, hq1: i64, corp1: i64, hq2: i64, corp2: i64) {
    let make_struct = |id: i64, cid: i64, name: &str| UpwellStructureRecord {
        structure_id: id,
        type_id: 35832,
        solar_system_id: 30_000_142,
        owner_corp_id: cid,
        name: name.into(),
        position: [100_000.0, 0.0, 0.0],
        state: "ShieldVulnerable".into(),
        quantum_core_installed: true,
        fuel_blocks: 1_000,
        shield_hp: 10_000.0,
        armor_hp: 10_000.0,
        hull_hp: 10_000.0,
        reinforced_until_unix: 0,
        last_damage_tick: 0,
        last_damage_unix: 0, self_destruct_at_unix: None,
    };
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(hq1, make_struct(hq1, corp1, "Aggressor War HQ"));
        snap.structures.insert(hq2, make_struct(hq2, corp2, "Defender Citadel"));
    });
}

async fn setup_two_corps(
    sid_base: u64,
    c1_name: &str,
    c1_tick: &str,
    c2_name: &str,
    c2_tick: &str,
) -> (ServerContext, SessionState, SessionState, i64, i64) {
    let ctx = ServerContext::new();
    let mut s1 = select_pilot(&ctx, sid_base + 1, "demo", "demo-secret", PILOT_1).await;
    let mut s2 = select_pilot(&ctx, sid_base + 2, "other", "other-secret", PILOT_2).await;

    let c1 = rpc(&ctx, &mut s1, "corpRegistry", "AddCorporation", &[
        PyRep::String(c1_name.into()), PyRep::String(c1_tick.into()), PyRep::Real(0.05),
    ]).await.expect("AddCorp 1");
    let cid1 = field(&c1, "corporationID").as_i64().expect("cid1");

    let c2 = rpc(&ctx, &mut s2, "corpRegistry", "AddCorporation", &[
        PyRep::String(c2_name.into()), PyRep::String(c2_tick.into()), PyRep::Real(0.05),
    ]).await.expect("AddCorp 2");
    let cid2 = field(&c2, "corporationID").as_i64().expect("cid2");

    set_corp_balance(&ctx, cid1, 500_000_000.0);
    set_corp_balance(&ctx, cid2, 500_000_000.0);
    (ctx, s1, s2, cid1, cid2)
}

#[tokio::test]
async fn test_corp_bills_and_war_registry_timestamps() {
    // parity: rpc:billMgr.CharPayBill
    // parity: rpc:billMgr.GetAutomaticPaySettings
    // parity: rpc:billMgr.GetCorporationBills
    // parity: rpc:billMgr.GetCorporationBillsReceivable
    // parity: rpc:billMgr.PayCorporationBill
    // parity: rpc:billMgr.SendAutomaticPaySettings
    // parity: rpc:warRegistry.timeDeclared
    // parity: rpc:warRegistry.timeStarted
    // parity: rpc:warRegistry.timeFinished

    let (ctx, mut sess1, mut sess2, corp1_id, corp2_id) =
        setup_two_corps(63_000, "Vanguard Vanguard", "VVG", "Bastion Bastion", "BST").await;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_1) {
            ch.balance = IskCents::from_f64_isk(50_000_000.0);
        }
    });

    let now = ctx.now_unix();
    let p_bill = create_bill(&ctx, 1, PILOT_1, corp2_id, 250_000.0, now + 3600, 0.0, 0, 0);
    let c_bill = create_bill(&ctx, 2, corp1_id, corp2_id, 1_000_000.0, now + 3600, 0.0, 60_003_760, 0);

    // 1. GetCorporationBills & GetCorporationBillsReceivable
    let bills_rep = rpc(&ctx, &mut sess1, "billMgr", "GetCorporationBills", &[]).await.expect("GetCorporationBills");
    let (_, b_rows) = rowset_data(&bills_rep);
    assert!(b_rows.iter().any(|r| r.first() == Some(&PyRep::Integer(p_bill))));
    assert!(b_rows.iter().any(|r| r.first() == Some(&PyRep::Integer(c_bill))));

    let rec_rep = rpc(&ctx, &mut sess2, "billMgr", "GetCorporationBillsReceivable", &[]).await.expect("GetCorporationBillsReceivable");
    let (_, rec_rows) = rowset_data(&rec_rep);
    assert!(rec_rows.iter().any(|r| r.first() == Some(&PyRep::Integer(p_bill))));
    assert!(rec_rows.iter().any(|r| r.first() == Some(&PyRep::Integer(c_bill))));

    // BF-BILLS-01: querying foreign corporation bills is denied
    let foreign_err = rpc(&ctx, &mut sess1, "billMgr", "GetCorporationBills", &[PyRep::Integer(corp2_id)]).await;
    assert_eq!(foreign_err.unwrap_err().code, "CorpAccessDenied");

    // 2. CharPayBill pays personal bill
    let p1_before = ctx.db.get_character(PILOT_1).unwrap().balance;
    let pay_p = rpc(&ctx, &mut sess1, "billMgr", "CharPayBill", &[PyRep::Integer(p_bill)]).await.expect("CharPayBill");
    assert_eq!(pay_p, PyRep::Bool(true));
    let p1_after = ctx.db.get_character(PILOT_1).unwrap().balance;
    assert_eq!(p1_before.0 - p1_after.0, 25_000_000);

    // 3. PayCorporationBill pays corp bill from division 3 (key 1002)
    let pay_c = rpc(&ctx, &mut sess1, "billMgr", "PayCorporationBill", &[PyRep::Integer(c_bill), PyRep::Integer(1002)]).await.expect("PayCorporationBill");
    assert_eq!(pay_c, PyRep::Bool(true));

    // 4. Automatic pay settings
    let auto_dict = PyRep::Dict(vec![
        (PyRep::Integer(2), PyRep::Bool(true)),
        (PyRep::String("divisionID".into()), PyRep::Integer(1004)),
    ]);
    let set_ap = rpc(&ctx, &mut sess1, "billMgr", "SendAutomaticPaySettings", &[PyRep::Dict(vec![(PyRep::Integer(corp1_id), auto_dict)])]).await.expect("SendAutomaticPaySettings");
    assert_eq!(set_ap, PyRep::Bool(true));

    let get_ap = rpc(&ctx, &mut sess1, "billMgr", "GetAutomaticPaySettings", &[]).await.expect("GetAutomaticPaySettings");
    let PyRep::Dict(ap_entries) = get_ap else { panic!("expected Dict"); };
    assert!(ap_entries.iter().any(|(k, _)| k.as_i64() == Some(corp1_id)));

    // 5. War declaration and warRegistry timestamps
    let hq1 = 995_000_301;
    let hq2 = 995_000_302;
    insert_hqs(&ctx, hq1, corp1_id, hq2, corp2_id);

    let war_res = rpc(&ctx, &mut sess1, "warRegistry", "DeclareWarAgainst", &[PyRep::Integer(corp2_id), PyRep::Integer(hq1)]).await.expect("DeclareWarAgainst");
    let war_id = field(&war_res, "warID").as_i64().expect("warID");
    assert!(war_id > 0);

    let t_decl = rpc(&ctx, &mut sess1, "warRegistry", "timeDeclared", &[PyRep::Integer(war_id)]).await.expect("timeDeclared");
    assert_eq!(t_decl, field(&war_res, "timeDeclared"));

    let t_start = rpc(&ctx, &mut sess1, "warRegistry", "timeStarted", &[PyRep::Integer(war_id)]).await.expect("timeStarted");
    assert_eq!(t_start, field(&war_res, "timeStarted"));

    let t_fin_none = rpc(&ctx, &mut sess1, "warRegistry", "timeFinished", &[PyRep::Integer(war_id)]).await.expect("timeFinished ongoing");
    assert_eq!(t_fin_none, PyRep::None);

    let surr = rpc(&ctx, &mut sess1, "warsInfoMgr", "CreateSurrenderNegotiation", &[
        PyRep::Integer(war_id), PyRep::Real(5_000_000.0), PyRep::String("Surrender truce".into()),
    ]).await.expect("CreateSurrenderNegotiation");
    let neg_id = field(&surr, "negotiationID").as_i64().expect("neg_id");

    let accept = rpc(&ctx, &mut sess2, "warsInfoMgr", "AcceptSurrender", &[PyRep::Integer(neg_id)]).await.expect("AcceptSurrender");
    let fin_time = field(&accept, "timeFinished").as_i64().expect("timeFinished");

    let t_fin_val = rpc(&ctx, &mut sess1, "warRegistry", "timeFinished", &[PyRep::Integer(war_id)]).await.expect("timeFinished finished");
    assert_eq!(t_fin_val, PyRep::Integer(fin_time));
}

#[tokio::test]
async fn test_war_registry_unbind_stats_and_wars_info() {
    // parity: rpc:warRegistry.UnBind
    // parity: rpc:warStatisticMgr.GetBaseInfo
    // parity: rpc:warStatisticMgr.GetKillMail
    // parity: rpc:warStatisticMgr.GetKills
    // parity: rpc:warStatisticMgr.GetKillsByGroup
    // parity: rpc:warsInfoMgr.GetPublicWarInfo
    // parity: rpc:warsInfoMgr.GetTop50
    // parity: rpc:warsInfoMgr.GetWarsByOwnerID
    // parity: rpc:warsInfoMgr.GetWarsByOwners

    let (ctx, mut sess1, mut sess2, corp1_id, corp2_id) =
        setup_two_corps(64_000, "Apex Security Div", "ASD", "Titan Syndicate", "TSC").await;

    // 1. warRegistry Bind & UnBind
    let bound = rpc(&ctx, &mut sess1, "warRegistry", "Bind", &[PyRep::Integer(corp1_id)]).await.expect("Bind warRegistry");
    let bound_id = bound.as_i64().expect("bound_id");
    assert!(bound_id > 0);

    // BF-WARS-01: unbinding another session's bound object is denied
    let unbind_foreign = rpc(&ctx, &mut sess2, "warRegistry", "UnBind", &[PyRep::Integer(bound_id)]).await;
    assert_eq!(unbind_foreign.unwrap_err().code, "BoundObjectOwnershipViolation");

    let unbind = rpc(&ctx, &mut sess1, "warRegistry", "UnBind", &[PyRep::Integer(bound_id)]).await.expect("UnBind warRegistry");
    assert_eq!(unbind, PyRep::None);

    // 2. Declare War and advance 24h warmup so war is active
    let hq1 = 995_000_401;
    let hq2 = 995_000_402;
    insert_hqs(&ctx, hq1, corp1_id, hq2, corp2_id);

    let war_res = rpc(&ctx, &mut sess1, "warRegistry", "DeclareWarAgainst", &[PyRep::Integer(corp2_id), PyRep::Integer(hq1)]).await.expect("DeclareWarAgainst");
    let war_id = field(&war_res, "warID").as_i64().expect("warID");
    ctx.advance_secs(86_400);

    // 3. Record combat ship destruction via combat_wrecks_killmails pipeline
    let km_id = eve_server::services::combat_crimewatch::process_ship_destruction_with_aggression(
        &ctx,
        PILOT_2,
        100_000_002,
        587,
        PILOT_1,
        587,
        30_000_142,
        glam::DVec3::new(1_000.0, 0.0, 0.0),
        false,
    );
    assert!(km_id > 0);

    // 4. warStatisticMgr: Bind + GetBaseInfo (bound & explicit), GetKills, GetKillsByGroup, GetKillMail
    let ws_bound = rpc(&ctx, &mut sess1, "warStatisticMgr", "Bind", &[PyRep::Integer(war_id)]).await.expect("Bind warStatisticMgr");
    assert!(ws_bound.as_i64().unwrap_or(0) > 0);
    let base_info = rpc(&ctx, &mut sess1, "warStatisticMgr", "GetBaseInfo", &[]).await.expect("GetBaseInfo bound");
    assert_eq!(field(&base_info, "warID"), PyRep::Integer(war_id));
    assert_eq!(field(&base_info, "declaredByID"), PyRep::Integer(corp1_id));
    assert_eq!(field(&base_info, "againstID"), PyRep::Integer(corp2_id));
    assert_eq!(field(&base_info, "killsForAggressor"), PyRep::Integer(1));
    assert_eq!(field(&base_info, "killsForDefender"), PyRep::Integer(0));
    assert!(field(&base_info, "iskDestroyedByAggressor").as_f64().unwrap_or(0.0) > 0.0);
    assert!(matches!(field(&base_info, "allies"), PyRep::Dict(_)));

    let kills_rep = rpc(&ctx, &mut sess1, "warStatisticMgr", "GetKills", &[PyRep::Integer(war_id)]).await.expect("GetKills");
    let (k_cols, k_rows) = rowset_data(&kills_rep);
    assert!(k_cols.contains(&"killID".to_string()));
    assert!(k_rows.iter().any(|r| r.first() == Some(&PyRep::Integer(km_id))));

    let grp_rep = rpc(&ctx, &mut sess1, "warStatisticMgr", "GetKillsByGroup", &[PyRep::Integer(war_id)]).await.expect("GetKillsByGroup");
    let (g_cols, g_rows) = rowset_data(&grp_rep);
    assert!(g_cols.contains(&"groupID".to_string()));
    assert!(!g_rows.is_empty());

    let km_rep = rpc(&ctx, &mut sess1, "warStatisticMgr", "GetKillMail", &[PyRep::Integer(km_id)]).await.expect("GetKillMail");
    assert_eq!(field(&km_rep, "killID"), PyRep::Integer(km_id));
    assert_eq!(field(&km_rep, "victimCharacterID"), PyRep::Integer(PILOT_2));
    assert_eq!(field(&km_rep, "finalCharacterID"), PyRep::Integer(PILOT_1));

    // 5. warsInfoMgr: GetPublicWarInfo, GetTop50 (active-only & ranked by kills/ISK), GetWarsByOwnerID, GetWarsByOwners
    ctx.db.provision_gm_account("p3_top50", "p3-secret", PILOT_3, "Pilot Three");
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(PILOT_3).or_default().insert(3363, eve_db::TrainedSkillRecord { type_id: 3363, level: 1, skill_points: 250.0 });
    });
    let mut sess3 = select_pilot(&ctx, 64_003, "p3_top50", "p3-secret", PILOT_3).await;
    let c3 = rpc(&ctx, &mut sess3, "corpRegistry", "AddCorporation", &[
        PyRep::String("Zero Kill Corp".into()), PyRep::String("ZKC".into()), PyRep::Real(0.05),
    ]).await.expect("AddCorp 3");
    let corp3_id = field(&c3, "corporationID").as_i64().expect("corp3_id");
    set_corp_balance(&ctx, corp3_id, 500_000_000.0);
    insert_hqs(&ctx, hq1, corp1_id, 995_000_403, corp3_id);
    let war2_res = rpc(&ctx, &mut sess1, "warRegistry", "DeclareWarAgainst", &[PyRep::Integer(corp3_id), PyRep::Integer(hq1)]).await.expect("DeclareWar 2");
    let war2_id = field(&war2_res, "warID").as_i64().expect("war2_id");
    let inactive_war_id = 999_404;
    ctx.db.with_mut_snapshot(|snap| {
        snap.wars.insert(inactive_war_id, eve_db::WarRecord {
            war_id: inactive_war_id,
            aggressor_corp_id: corp2_id,
            defender_corp_id: corp3_id,
            mutual: false,
            started_unix: 200,
            active: false,
        });
    });

    let pub_info = rpc(&ctx, &mut sess1, "warsInfoMgr", "GetPublicWarInfo", &[PyRep::Integer(war_id)]).await.expect("GetPublicWarInfo");
    assert_eq!(field(&pub_info, "warID"), PyRep::Integer(war_id));
    assert_eq!(field(&pub_info, "declaredByID"), PyRep::Integer(corp1_id));
    assert_eq!(field(&pub_info, "againstID"), PyRep::Integer(corp2_id));

    let top50 = rpc(&ctx, &mut sess1, "warsInfoMgr", "GetTop50", &[]).await.expect("GetTop50");
    let (_, top_rows) = rowset_data(&top50);
    let pos_w1 = top_rows.iter().position(|r| r.first() == Some(&PyRep::Integer(war_id))).expect("war1 in Top50");
    let pos_w2 = top_rows.iter().position(|r| r.first() == Some(&PyRep::Integer(war2_id))).expect("war2 in Top50");
    assert!(pos_w1 < pos_w2, "war with kills must rank above 0-kill war in GetTop50");
    assert!(!top_rows.iter().any(|r| r.first() == Some(&PyRep::Integer(inactive_war_id))), "inactive war must be excluded from GetTop50");

    let by_owner = rpc(&ctx, &mut sess1, "warsInfoMgr", "GetWarsByOwnerID", &[PyRep::Integer(corp1_id)]).await.expect("GetWarsByOwnerID");
    let (_, owner_rows) = rowset_data(&by_owner);
    assert!(owner_rows.iter().any(|r| r.first() == Some(&PyRep::Integer(war_id))));

    let by_owners = rpc(&ctx, &mut sess1, "warsInfoMgr", "GetWarsByOwners", &[PyRep::List(vec![
        PyRep::Integer(corp1_id), PyRep::Integer(corp2_id),
    ])]).await.expect("GetWarsByOwners");
    let (_, owners_rows) = rowset_data(&by_owners);
    assert!(owners_rows.iter().any(|r| r.first() == Some(&PyRep::Integer(war_id))));
}

#[tokio::test]
async fn test_wars_structure_allies_treaties_and_mutual_invites() {
    // parity: rpc:warsInfoMgr.GetWarsForStructure
    // parity: rpc:warsInfoMgr.GetWarsRequiringAssistance
    // parity: rpc:peaceTreatyMgr.GetPeaceTreatiesForSession
    // parity: rpc:mutualWarInviteMgr.GetPendingInvitesForSession
    // parity: rpc:mutualWarInviteMgr.IsCorpInvitesBlockedPlayer
    // parity: rpc:mutualWarInviteMgr.RespondToInviteByPlayer
    // parity: rpc:mutualWarInviteMgr.SendInviteByPlayer
    // parity: rpc:mutualWarInviteMgr.SetInvitesBlockedByPlayer
    // parity: rpc:mutualWarInviteMgr.WithdrawInviteByPlayer

    let (ctx, mut sess1, mut sess2, corp1_id, corp2_id) =
        setup_two_corps(65_000, "Omega Command Fed", "OCF", "Solaris Defence", "SDM").await;

    ctx.db.provision_gm_account("pilot3", "pilot3-secret", PILOT_3, "Pilot Three");
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(PILOT_3).or_default().insert(
            3363,
            eve_db::TrainedSkillRecord {
                type_id: 3363,
                level: 1,
                skill_points: 250.0,
            },
        );
    });
    let mut sess3 = select_pilot(&ctx, 65_003, "pilot3", "pilot3-secret", PILOT_3).await;

    let c3 = rpc(&ctx, &mut sess3, "corpRegistry", "AddCorporation", &[
        PyRep::String("Nebula Trade Net".into()), PyRep::String("NTN".into()), PyRep::Real(0.05),
    ]).await.expect("AddCorp 3");
    let corp3_id = field(&c3, "corporationID").as_i64().expect("corp3_id");
    set_corp_balance(&ctx, corp3_id, 500_000_000.0);

    // 1. Declare war with War HQ
    let hq1 = 995_000_501;
    let hq2 = 995_000_502;
    insert_hqs(&ctx, hq1, corp1_id, hq2, corp2_id);

    let war_res = rpc(&ctx, &mut sess1, "warRegistry", "DeclareWarAgainst", &[PyRep::Integer(corp2_id), PyRep::Integer(hq1)]).await.expect("DeclareWarAgainst");
    let war_id = field(&war_res, "warID").as_i64().expect("warID");

    // 2. warsInfoMgr: GetWarsForStructure & GetWarsRequiringAssistance
    let s_wars = rpc(&ctx, &mut sess1, "warsInfoMgr", "GetWarsForStructure", &[PyRep::Integer(hq1)]).await.expect("GetWarsForStructure");
    let (_, s_rows) = rowset_data(&s_wars);
    assert!(s_rows.iter().any(|r| r.first() == Some(&PyRep::Integer(war_id))));

    let open_res = rpc(&ctx, &mut sess2, "warsInfoMgr", "SetOpenForAllies", &[PyRep::Integer(war_id), PyRep::Bool(true)]).await.expect("SetOpenForAllies");
    assert_eq!(field(&open_res, "openForAllies"), PyRep::Bool(true));

    let assist = rpc(&ctx, &mut sess1, "warsInfoMgr", "GetWarsRequiringAssistance", &[]).await.expect("GetWarsRequiringAssistance");
    let (_, a_rows) = rowset_data(&assist);
    assert!(a_rows.iter().any(|r| r.first() == Some(&PyRep::Integer(war_id))));

    // 3. peaceTreatyMgr: GetPeaceTreatiesForSession
    let surr = rpc(&ctx, &mut sess1, "warsInfoMgr", "CreateSurrenderNegotiation", &[
        PyRep::Integer(war_id), PyRep::Real(1_000_000.0), PyRep::String("Truce".into()),
    ]).await.expect("CreateSurrenderNegotiation");
    let neg_id = field(&surr, "negotiationID").as_i64().expect("neg_id");

    let accept = rpc(&ctx, &mut sess2, "warsInfoMgr", "AcceptSurrender", &[PyRep::Integer(neg_id)]).await.expect("AcceptSurrender");
    assert_eq!(field(&accept, "status"), PyRep::String("Accepted".into()));

    let treaties = rpc(&ctx, &mut sess1, "peaceTreatyMgr", "GetPeaceTreatiesForSession", &[]).await.expect("GetPeaceTreatiesForSession");
    let (_, treaty_rows) = rowset_data(&treaties);
    assert!(treaty_rows.iter().any(|r| r.first() == Some(&PyRep::Integer(war_id))));

    // 4. mutualWarInviteMgr: IsCorpInvitesBlockedPlayer & SetInvitesBlockedByPlayer
    let unblk = rpc(&ctx, &mut sess3, "mutualWarInviteMgr", "IsCorpInvitesBlockedPlayer", &[]).await.expect("IsCorpInvitesBlockedPlayer initial");
    assert_eq!(unblk, PyRep::Bool(false));

    let set_blk = rpc(&ctx, &mut sess3, "mutualWarInviteMgr", "SetInvitesBlockedByPlayer", &[PyRep::Bool(true)]).await.expect("SetInvitesBlockedByPlayer true");
    assert_eq!(set_blk, PyRep::Bool(true));

    let is_blk = rpc(&ctx, &mut sess3, "mutualWarInviteMgr", "IsCorpInvitesBlockedPlayer", &[]).await.expect("IsCorpInvitesBlockedPlayer true");
    assert_eq!(is_blk, PyRep::Bool(true));

    assert!(rpc(&ctx, &mut sess1, "mutualWarInviteMgr", "SendInviteByPlayer", &[PyRep::Integer(corp3_id)]).await.is_err());

    rpc(&ctx, &mut sess3, "mutualWarInviteMgr", "SetInvitesBlockedByPlayer", &[PyRep::Bool(false)]).await.expect("SetInvitesBlockedByPlayer false");

    // 5. mutualWarInviteMgr: SendInviteByPlayer, GetPendingInvitesForSession, WithdrawInviteByPlayer
    let inv1 = rpc(&ctx, &mut sess1, "mutualWarInviteMgr", "SendInviteByPlayer", &[PyRep::Integer(corp3_id)]).await.expect("SendInviteByPlayer");
    assert_eq!(field(&inv1, "fromID"), PyRep::Integer(corp1_id));
    assert_eq!(field(&inv1, "toID"), PyRep::Integer(corp3_id));

    let pending1 = rpc(&ctx, &mut sess3, "mutualWarInviteMgr", "GetPendingInvitesForSession", &[]).await.expect("GetPendingInvitesForSession 1");
    let (_, p_rows1) = rowset_data(&pending1);
    assert!(p_rows1.iter().any(|r| r.first() == Some(&PyRep::Integer(corp1_id)) && r.get(1) == Some(&PyRep::Integer(corp3_id))));

    let w_res = rpc(&ctx, &mut sess1, "mutualWarInviteMgr", "WithdrawInviteByPlayer", &[PyRep::Integer(corp3_id)]).await.expect("WithdrawInviteByPlayer");
    assert_eq!(w_res, PyRep::Bool(true));

    let pending2 = rpc(&ctx, &mut sess3, "mutualWarInviteMgr", "GetPendingInvitesForSession", &[]).await.expect("GetPendingInvitesForSession 2");
    let (_, p_rows2) = rowset_data(&pending2);
    assert!(!p_rows2.iter().any(|r| r.first() == Some(&PyRep::Integer(corp1_id)) && r.get(1) == Some(&PyRep::Integer(corp3_id))));

    // 6. Send again and RespondToInviteByPlayer
    let _ = rpc(&ctx, &mut sess1, "mutualWarInviteMgr", "SendInviteByPlayer", &[PyRep::Integer(corp3_id)]).await.expect("SendInviteByPlayer second");

    let resp = rpc(&ctx, &mut sess3, "mutualWarInviteMgr", "RespondToInviteByPlayer", &[PyRep::Integer(corp1_id), PyRep::Bool(true)]).await.expect("RespondToInviteByPlayer");
    assert_eq!(field(&resp, "accepted"), PyRep::Bool(true));
    assert_eq!(field(&resp, "mutual"), PyRep::Bool(true));
    let mutual_war_id = field(&resp, "warID").as_i64().expect("warID");
    assert!(mutual_war_id > 0);
}
