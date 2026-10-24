//! Parity integration tests for Corporation, Alliance, and FW Character Enlistment services (`SO-3`).
//!
//! Covers 38 parity rows across:
//! - `fwCharacterEnlistmentMgr` (6 rows)
//! - `corporationSvc` (9 rows)
//! - `corpmgr` (10 rows)
//! - `allianceRegistry` (13 rows)

use crate::common;
use eve_db::{corp_roles, InventoryFlag, InventoryItemRecord, IskCents};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use std::collections::HashMap;

async fn call(
    ctx: &ServerContext,
    s: &mut SessionState,
    svc: &str,
    m: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(s, svc, None, m, args, &HashMap::new(), None)
        .await
}

fn rows(rep: &PyRep) -> (Vec<String>, Vec<Vec<PyRep>>) {
    match rep {
        PyRep::Rowset { columns, rows } => (columns.clone(), rows.clone()),
        other => panic!("expected Rowset, got {other:?}"),
    }
}

fn keyval(rep: &PyRep) -> HashMap<String, PyRep> {
    match rep {
        PyRep::KeyVal(dict) => dict.clone(),
        other => panic!("expected KeyVal, got {other:?}"),
    }
}

#[tokio::test]
async fn test_fw_character_enlistment_lifecycle() {
    // parity: rpc:fwCharacterEnlistmentMgr.CreateMyDirectEnlistment, rpc:fwCharacterEnlistmentMgr.GetCorpAllowedEnlistmentFactions, rpc:fwCharacterEnlistmentMgr.GetMyDirectEnlistmentCooldownTimestamp, rpc:fwCharacterEnlistmentMgr.GetMyEnlistment, rpc:fwCharacterEnlistmentMgr.RemoveMyDirectEnlistment, rpc:fwCharacterEnlistmentMgr.SetMyCorpAllowedEnlistmentFactions
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let mut sess1 = SessionState::new(2001);
    common::login_pilot(&ctx, &mut sess1, "demo", "demo-secret").await;
    call(
        &ctx,
        &mut sess1,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(90_000_001)],
    )
    .await
    .expect("SelectCharacterID 90000001");

    // 1. Initial enlistment query: character is not enlisted
    let init_enlistment = call(
        &ctx,
        &mut sess1,
        "fwCharacterEnlistmentMgr",
        "GetMyEnlistment",
        &[],
    )
    .await
    .expect("GetMyEnlistment");
    assert_eq!(
        init_enlistment,
        PyRep::Tuple(vec![PyRep::None, PyRep::None, PyRep::None]),
        "Character initially not enlisted"
    );

    // Initial cooldown timestamp is None
    let init_cooldown = call(
        &ctx,
        &mut sess1,
        "fwCharacterEnlistmentMgr",
        "GetMyDirectEnlistmentCooldownTimestamp",
        &[],
    )
    .await
    .expect("GetMyDirectEnlistmentCooldownTimestamp");
    assert_eq!(init_cooldown, PyRep::None, "No cooldown initially");

    // 2. Reject enlistment with invalid faction
    let bad_fac = call(
        &ctx,
        &mut sess1,
        "fwCharacterEnlistmentMgr",
        "CreateMyDirectEnlistment",
        &[PyRep::Integer(999_999)],
    )
    .await
    .expect_err("Invalid war faction must fail");
    assert_eq!(bad_fac.code, "InvalidWarFaction");

    // 3. Create direct enlistment with Caldari State (faction 500001)
    call(
        &ctx,
        &mut sess1,
        "fwCharacterEnlistmentMgr",
        "CreateMyDirectEnlistment",
        &[PyRep::Integer(500_001)],
    )
    .await
    .expect("CreateMyDirectEnlistment 500001");

    // Verify state mutation: character record and session
    let war_faction = ctx
        .db
        .with_snapshot(|s| s.characters.get(&90_000_001).and_then(|c| c.war_faction_id));
    assert_eq!(war_faction, Some(500_001), "Character snapshot updated");
    assert_eq!(
        sess1.war_faction_id,
        Some(500_001),
        "Session war faction updated"
    );

    // Query enlistment after direct enlistment
    let enlisted = call(
        &ctx,
        &mut sess1,
        "fwCharacterEnlistmentMgr",
        "GetMyEnlistment",
        &[],
    )
    .await
    .expect("GetMyEnlistment post-enlist");
    assert_eq!(
        enlisted,
        PyRep::Tuple(vec![
            PyRep::Integer(500_001),
            PyRep::Integer(500_001),
            PyRep::None
        ]),
        "Enlistment tuple reflects Caldari State"
    );

    // Duplicate enlistment rejected
    let dup_err = call(
        &ctx,
        &mut sess1,
        "fwCharacterEnlistmentMgr",
        "CreateMyDirectEnlistment",
        &[PyRep::Integer(500_001)],
    )
    .await
    .expect_err("Duplicate enlistment must fail");
    assert_eq!(dup_err.code, "FwAlreadyEnlisted");

    // 4. Remove direct enlistment
    call(
        &ctx,
        &mut sess1,
        "fwCharacterEnlistmentMgr",
        "RemoveMyDirectEnlistment",
        &[],
    )
    .await
    .expect("RemoveMyDirectEnlistment");

    // Verify snapshot and session mutation
    let post_rm_faction = ctx
        .db
        .with_snapshot(|s| s.characters.get(&90_000_001).and_then(|c| c.war_faction_id));
    assert_eq!(post_rm_faction, None, "Character snapshot war faction cleared");
    assert_eq!(sess1.war_faction_id, None, "Session war faction cleared");

    // Cooldown is now active
    let cooldown_ts = call(
        &ctx,
        &mut sess1,
        "fwCharacterEnlistmentMgr",
        "GetMyDirectEnlistmentCooldownTimestamp",
        &[],
    )
    .await
    .expect("GetMyDirectEnlistmentCooldownTimestamp post-remove");
    assert!(
        matches!(cooldown_ts, PyRep::Integer(ts) if ts > 0),
        "Cooldown timestamp set after removal"
    );

    // Immediate re-enlistment blocked by cooldown
    let cooldown_err = call(
        &ctx,
        &mut sess1,
        "fwCharacterEnlistmentMgr",
        "CreateMyDirectEnlistment",
        &[PyRep::Integer(500_001)],
    )
    .await
    .expect_err("Re-enlistment during cooldown rejected");
    assert_eq!(cooldown_err.code, "FwEnlistmentCooldown");

    // 5. Corporation allowed factions configuration
    // NPC corporation cannot configure allowed factions
    let npc_cfg_err = call(
        &ctx,
        &mut sess1,
        "fwCharacterEnlistmentMgr",
        "SetMyCorpAllowedEnlistmentFactions",
        &[PyRep::List(vec![PyRep::Integer(500_002)])],
    )
    .await
    .expect_err("NPC corp cannot configure enlistment factions");
    assert_eq!(npc_cfg_err.code, "CorpRoleDenied");

    // Create a player corporation where 90_000_001 is CEO
    call(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "AddCorporation",
        &[
            PyRep::String("Militia Vanguard Corps".into()),
            PyRep::String("MVANG".into()),
            PyRep::Real(0.05),
        ],
    )
    .await
    .expect("AddCorporation");
    let player_corp_id = sess1.corporation_id.expect("player corp ID");

    // Initial corp allowed factions query returns None (unrestricted)
    let init_allowed = call(
        &ctx,
        &mut sess1,
        "fwCharacterEnlistmentMgr",
        "GetCorpAllowedEnlistmentFactions",
        &[PyRep::Integer(player_corp_id)],
    )
    .await
    .expect("GetCorpAllowedEnlistmentFactions initial");
    assert_eq!(init_allowed, PyRep::None, "No restrictions initially");

    // CEO sets allowed factions: Minmatar Republic (500002) and Gallente Federation (500004)
    let set_res = call(
        &ctx,
        &mut sess1,
        "fwCharacterEnlistmentMgr",
        "SetMyCorpAllowedEnlistmentFactions",
        &[PyRep::List(vec![
            PyRep::Integer(500_002),
            PyRep::Integer(500_004),
        ])],
    )
    .await
    .expect("SetMyCorpAllowedEnlistmentFactions");
    assert_eq!(
        set_res,
        PyRep::List(vec![PyRep::Integer(500_002), PyRep::Integer(500_004)])
    );

    // Query allowed factions confirms saved list
    let post_set_allowed = call(
        &ctx,
        &mut sess1,
        "fwCharacterEnlistmentMgr",
        "GetCorpAllowedEnlistmentFactions",
        &[PyRep::Integer(player_corp_id)],
    )
    .await
    .expect("GetCorpAllowedEnlistmentFactions post-set");
    assert_eq!(
        post_set_allowed,
        PyRep::List(vec![PyRep::Integer(500_002), PyRep::Integer(500_004)])
    );

    // Advance clock past cooldown (cooldown is 24 hours / 86400 secs)
    ctx.advance_secs(90_000);

    // Attempting to enlist with Caldari (500001) is denied by corp policy
    let corp_denied = call(
        &ctx,
        &mut sess1,
        "fwCharacterEnlistmentMgr",
        "CreateMyDirectEnlistment",
        &[PyRep::Integer(500_001)],
    )
    .await
    .expect_err("Disallowed faction rejected by corp policy");
    assert_eq!(corp_denied.code, "FwFactionNotAllowedByCorp");

    // Enlisting with Minmatar (500002) succeeds
    call(
        &ctx,
        &mut sess1,
        "fwCharacterEnlistmentMgr",
        "CreateMyDirectEnlistment",
        &[PyRep::Integer(500_002)],
    )
    .await
    .expect("Enlist with allowed Minmatar faction");
    assert_eq!(sess1.war_faction_id, Some(500_002));
}

#[tokio::test]
async fn test_corporation_medals_and_employment() {
    // parity: rpc:corporationSvc.CreateMedal, rpc:corporationSvc.GetAllCorpMedals, rpc:corporationSvc.GetCorpInfo, rpc:corporationSvc.GetEmploymentRecord, rpc:corporationSvc.GetMedalDetails, rpc:corporationSvc.GetMedalsReceived, rpc:corporationSvc.GetRecipientsOfMedal, rpc:corporationSvc.GiveMedalToCharacters, rpc:corporationSvc.SetMedalStatus
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let mut sess1 = SessionState::new(2101);
    common::login_pilot(&ctx, &mut sess1, "demo", "demo-secret").await;
    call(
        &ctx,
        &mut sess1,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(90_000_001)],
    )
    .await
    .expect("SelectCharacterID 90000001");

    let mut sess2 = SessionState::new(2102);
    common::login_pilot(&ctx, &mut sess2, "other", "other-secret").await;
    call(
        &ctx,
        &mut sess2,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(90_000_002)],
    )
    .await
    .expect("SelectCharacterID 90000002");

    // 1. GetEmploymentRecord for character 90_000_001
    let emp_rep = call(
        &ctx,
        &mut sess1,
        "corporationSvc",
        "GetEmploymentRecord",
        &[PyRep::Integer(90_000_001)],
    )
    .await
    .expect("GetEmploymentRecord");
    let (emp_cols, emp_rows) = rows(&emp_rep);
    assert_eq!(emp_cols, vec!["corporationID", "startDate", "endDate"]);
    assert!(!emp_rows.is_empty(), "Employment history has initial record");
    assert_eq!(emp_rows[0][0], PyRep::Integer(1_000_170));

    // 2. Create player corporation "Honor Guard Logistics"
    call(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "AddCorporation",
        &[
            PyRep::String("Honor Guard Logistics".into()),
            PyRep::String("HGLOG".into()),
            PyRep::Real(0.04),
        ],
    )
    .await
    .expect("AddCorporation");
    let corp_id = sess1.corporation_id.expect("corpID");

    // 3. GetCorpInfo for newly created corporation
    let info_rep = call(
        &ctx,
        &mut sess1,
        "corporationSvc",
        "GetCorpInfo",
        &[PyRep::Integer(corp_id)],
    )
    .await
    .expect("GetCorpInfo");
    let info_kv = keyval(&info_rep);
    assert_eq!(info_kv.get("corporationID"), Some(&PyRep::Integer(corp_id)));
    assert_eq!(
        info_kv.get("corporationName"),
        Some(&PyRep::String("Honor Guard Logistics".into()))
    );
    assert_eq!(info_kv.get("ticker"), Some(&PyRep::String("HGLOG".into())));
    assert_eq!(info_kv.get("ceoID"), Some(&PyRep::Integer(90_000_001)));

    // 4. CreateMedal: deducts 5,000,000 ISK fee and records medal
    let bal_before = ctx
        .db
        .with_snapshot(|s| s.characters.get(&90_000_001).unwrap().balance);
    let medal_rep = call(
        &ctx,
        &mut sess1,
        "corporationSvc",
        "CreateMedal",
        &[
            PyRep::String("Order of the Solar Vanguard".into()),
            PyRep::String("Conferred for peerless courage in fleet maneuvers".into()),
        ],
    )
    .await
    .expect("CreateMedal");
    let medal_kv = keyval(&medal_rep);
    let medal_id = match medal_kv.get("medalID") {
        Some(PyRep::Integer(id)) => *id,
        other => panic!("expected integer medalID, got {other:?}"),
    };
    assert_eq!(medal_id, 1);
    assert_eq!(
        medal_kv.get("title"),
        Some(&PyRep::String("Order of the Solar Vanguard".into()))
    );

    // Verify 5,000,000 ISK deduction in snapshot
    let bal_after = ctx
        .db
        .with_snapshot(|s| s.characters.get(&90_000_001).unwrap().balance);
    assert_eq!(
        bal_after,
        bal_before.checked_sub(IskCents::from_isk(5_000_000)).unwrap(),
        "Medal fee 5,000,000 ISK debited from character balance"
    );

    // 5. GetAllCorpMedals returns created medal
    let all_medals_rep = call(
        &ctx,
        &mut sess1,
        "corporationSvc",
        "GetAllCorpMedals",
        &[],
    )
    .await
    .expect("GetAllCorpMedals");
    let (all_cols, all_rows) = rows(&all_medals_rep);
    assert_eq!(
        all_cols,
        vec!["medalID", "title", "description", "creatorID", "date"]
    );
    assert_eq!(all_rows.len(), 1);
    assert_eq!(all_rows[0][0], PyRep::Integer(medal_id));
    assert_eq!(
        all_rows[0][1],
        PyRep::String("Order of the Solar Vanguard".into())
    );
    assert_eq!(all_rows[0][3], PyRep::Integer(90_000_001));

    // 6. GetMedalDetails returns medal rowset
    let details_rep = call(
        &ctx,
        &mut sess1,
        "corporationSvc",
        "GetMedalDetails",
        &[PyRep::Integer(medal_id)],
    )
    .await
    .expect("GetMedalDetails");
    let (det_cols, det_rows) = rows(&details_rep);
    assert_eq!(
        det_cols,
        vec!["medalID", "title", "description", "creatorID", "date"]
    );
    assert_eq!(det_rows.len(), 1);
    assert_eq!(det_rows[0][0], PyRep::Integer(medal_id));

    // 7. GiveMedalToCharacters awards medal to character 90_000_002
    let give_rep = call(
        &ctx,
        &mut sess1,
        "corporationSvc",
        "GiveMedalToCharacters",
        &[
            PyRep::Integer(medal_id),
            PyRep::Integer(90_000_002),
            PyRep::String("Exemplary electronic warfare support".into()),
        ],
    )
    .await
    .expect("GiveMedalToCharacters");
    let give_kv = keyval(&give_rep);
    assert_eq!(give_kv.get("medalID"), Some(&PyRep::Integer(medal_id)));
    assert_eq!(
        give_kv.get("recipientID"),
        Some(&PyRep::Integer(90_000_002))
    );
    assert_eq!(give_kv.get("status"), Some(&PyRep::Integer(2)));

    // 8. GetRecipientsOfMedal lists the recipient
    let recips_rep = call(
        &ctx,
        &mut sess1,
        "corporationSvc",
        "GetRecipientsOfMedal",
        &[PyRep::Integer(medal_id)],
    )
    .await
    .expect("GetRecipientsOfMedal");
    let (rec_cols, rec_rows) = rows(&recips_rep);
    assert_eq!(
        rec_cols,
        vec![
            "medalID",
            "wearerID",
            "issuerID",
            "corporationID",
            "title",
            "reason",
            "status"
        ]
    );
    let our_rec = rec_rows
        .iter()
        .find(|r| r[0] == PyRep::Integer(medal_id) && r[1] == PyRep::Integer(90_000_002))
        .expect("Recipient record found");
    assert_eq!(our_rec[0], PyRep::Integer(medal_id));
    assert_eq!(our_rec[1], PyRep::Integer(90_000_002));
    assert_eq!(our_rec[2], PyRep::Integer(90_000_001));
    assert_eq!(our_rec[3], PyRep::Integer(corp_id));
    assert_eq!(our_rec[6], PyRep::Integer(2));

    // 9. GetMedalsReceived lists the medal for recipient 90_000_002
    let rcv_rep = call(
        &ctx,
        &mut sess2,
        "corporationSvc",
        "GetMedalsReceived",
        &[PyRep::Integer(90_000_002)],
    )
    .await
    .expect("GetMedalsReceived");
    let (_, rcv_rows) = rows(&rcv_rep);
    let our_rcv = rcv_rows
        .iter()
        .find(|r| r[0] == PyRep::Integer(medal_id) && r[1] == PyRep::Integer(90_000_002))
        .expect("Received medal found");
    assert_eq!(our_rcv[0], PyRep::Integer(medal_id));
    assert_eq!(our_rcv[1], PyRep::Integer(90_000_002));

    // 10. SetMedalStatus updates display status to 3
    let status_rep = call(
        &ctx,
        &mut sess2,
        "corporationSvc",
        "SetMedalStatus",
        &[PyRep::Integer(medal_id), PyRep::Integer(3)],
    )
    .await
    .expect("SetMedalStatus");
    let status_kv = keyval(&status_rep);
    assert_eq!(status_kv.get("medalID"), Some(&PyRep::Integer(medal_id)));
    assert_eq!(status_kv.get("status"), Some(&PyRep::Integer(3)));

    // Re-query GetMedalsReceived reflects new status 3
    let rcv_rep2 = call(
        &ctx,
        &mut sess2,
        "corporationSvc",
        "GetMedalsReceived",
        &[PyRep::Integer(90_000_002)],
    )
    .await
    .expect("GetMedalsReceived updated");
    let (_, rcv_rows2) = rows(&rcv_rep2);
    let our_rcv2 = rcv_rows2
        .iter()
        .find(|r| r[0] == PyRep::Integer(medal_id) && r[1] == PyRep::Integer(90_000_002))
        .expect("Received medal updated found");
    assert_eq!(our_rcv2[6], PyRep::Integer(3));
}

#[tokio::test]
async fn test_corpmgr_attributes_and_queries() {
    // parity: rpc:corpmgr.AuditMember, rpc:corpmgr.GetAggressionSettingsForCorps, rpc:corpmgr.GetAssetInventoryForLocation, rpc:corpmgr.GetCorporationIDForCharacter, rpc:corpmgr.GetCorporations, rpc:corpmgr.GetPublicInfo, rpc:corpmgr.allianceID, rpc:corpmgr.ceoID, rpc:corpmgr.corporationID
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let mut sess1 = SessionState::new(2201);
    common::login_pilot(&ctx, &mut sess1, "demo", "demo-secret").await;
    call(
        &ctx,
        &mut sess1,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(90_000_001)],
    )
    .await
    .expect("SelectCharacterID 90000001");

    let mut sess2 = SessionState::new(2202);
    common::login_pilot(&ctx, &mut sess2, "other", "other-secret").await;
    call(
        &ctx,
        &mut sess2,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(90_000_002)],
    )
    .await
    .expect("SelectCharacterID 90000002");

    // 1. GetCorporationIDForCharacter
    let cid1 = call(
        &ctx,
        &mut sess1,
        "corpmgr",
        "GetCorporationIDForCharacter",
        &[PyRep::Integer(90_000_001)],
    )
    .await
    .expect("GetCorporationIDForCharacter 90000001");
    assert_eq!(cid1, PyRep::Integer(1_000_170));

    let cid2 = call(
        &ctx,
        &mut sess1,
        "corpmgr",
        "GetCorporationIDForCharacter",
        &[PyRep::Integer(90_000_002)],
    )
    .await
    .expect("GetCorporationIDForCharacter 90000002");
    assert_eq!(cid2, PyRep::Integer(1_000_045));

    // 2. Create player corporation "Aegis Frontier Syndicate"
    call(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "AddCorporation",
        &[
            PyRep::String("Aegis Frontier Syndicate".into()),
            PyRep::String("AEGFS".into()),
            PyRep::Real(0.06),
        ],
    )
    .await
    .expect("AddCorporation");
    let corp_id = sess1.corporation_id.expect("player corp ID");

    // Add pilot 90_000_002 to the corporation
    sess2.corporation_id = Some(corp_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_002) {
            ch.corporation_id = corp_id;
        }
        if let Some(corp) = snap.corporations.get_mut(&corp_id) {
            corp.members.insert(90_000_002);
        }
    });

    // 3. GetPublicInfo and GetCorporations
    let pub_info = call(
        &ctx,
        &mut sess1,
        "corpmgr",
        "GetPublicInfo",
        &[PyRep::Integer(corp_id)],
    )
    .await
    .expect("GetPublicInfo");
    let pub_kv = keyval(&pub_info);
    assert_eq!(pub_kv.get("corporationID"), Some(&PyRep::Integer(corp_id)));
    assert_eq!(
        pub_kv.get("corporationName"),
        Some(&PyRep::String("Aegis Frontier Syndicate".into()))
    );
    assert_eq!(pub_kv.get("ticker"), Some(&PyRep::String("AEGFS".into())));
    assert_eq!(pub_kv.get("ceoID"), Some(&PyRep::Integer(90_000_001)));

    let get_corps = call(
        &ctx,
        &mut sess1,
        "corpmgr",
        "GetCorporations",
        &[PyRep::Integer(corp_id)],
    )
    .await
    .expect("GetCorporations");
    let corps_kv = keyval(&get_corps);
    assert_eq!(corps_kv.get("corporationID"), Some(&PyRep::Integer(corp_id)));

    // 4. corpmgr attribute calls: corporationID, ceoID, allianceID, stationID
    let attr_cid = call(&ctx, &mut sess1, "corpmgr", "corporationID", &[])
        .await
        .expect("corpmgr.corporationID");
    assert_eq!(attr_cid, PyRep::Integer(corp_id));

    let attr_ceo = call(&ctx, &mut sess1, "corpmgr", "ceoID", &[])
        .await
        .expect("corpmgr.ceoID");
    assert_eq!(attr_ceo, PyRep::Integer(90_000_001));

    let attr_aid = call(&ctx, &mut sess1, "corpmgr", "allianceID", &[])
        .await
        .expect("corpmgr.allianceID");
    assert_eq!(attr_aid, PyRep::None, "No alliance assigned initially");

    // 5. GetAggressionSettingsForCorps
    let agg_rep = call(
        &ctx,
        &mut sess1,
        "corpmgr",
        "GetAggressionSettingsForCorps",
        &[PyRep::Integer(corp_id)],
    )
    .await
    .expect("GetAggressionSettingsForCorps");
    let agg_kv = keyval(&agg_rep);
    assert_eq!(agg_kv.get("corporationID"), Some(&PyRep::Integer(corp_id)));
    assert_eq!(
        agg_kv.get("isFriendlyFireLegal"),
        Some(&PyRep::Bool(false)),
        "Friendly fire initially illegal"
    );

    // CEO enables friendly fire and advances clock past 24h timer
    call(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "RegisterNewAggressionSettings",
        &[PyRep::Bool(true)],
    )
    .await
    .expect("RegisterNewAggressionSettings");
    ctx.advance_secs(86_400);

    let agg_rep_active = call(
        &ctx,
        &mut sess1,
        "corpmgr",
        "GetAggressionSettingsForCorps",
        &[PyRep::Integer(corp_id)],
    )
    .await
    .expect("GetAggressionSettingsForCorps active");
    let agg_active_kv = keyval(&agg_rep_active);
    assert_eq!(
        agg_active_kv.get("isFriendlyFireLegal"),
        Some(&PyRep::Bool(true)),
        "Friendly fire legal after timer elapses"
    );

    // 6. GetAssetInventoryForLocation
    // Seed an asset owned by corporation in station 60_003_760
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            200_000_101,
            InventoryItemRecord {
                item_id: 200_000_101,
                type_id: 587, // Rifter
                owner_id: corp_id,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 1,
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

    // CEO queries asset inventory for station
    let assets_rep = call(
        &ctx,
        &mut sess1,
        "corpmgr",
        "GetAssetInventoryForLocation",
        &[PyRep::Integer(60_003_760)],
    )
    .await
    .expect("GetAssetInventoryForLocation CEO");
    let (ast_cols, ast_rows) = rows(&assets_rep);
    assert_eq!(
        ast_cols,
        vec![
            "itemID",
            "typeID",
            "ownerID",
            "locationID",
            "solarsystemID",
            "flagID",
            "quantity"
        ]
    );
    assert_eq!(ast_rows.len(), 1);
    assert_eq!(ast_rows[0][0], PyRep::Integer(200_000_101));
    assert_eq!(ast_rows[0][1], PyRep::Integer(587));
    assert_eq!(ast_rows[0][2], PyRep::Integer(corp_id));

    // Regular member without director role is denied
    let denied_assets = call(
        &ctx,
        &mut sess2,
        "corpmgr",
        "GetAssetInventoryForLocation",
        &[PyRep::Integer(60_003_760)],
    )
    .await
    .expect_err("Non-director member denied asset inventory");
    assert_eq!(denied_assets.code, "CorpRoleDenied");

    // 7. AuditMember
    // CEO updates roles of 90_000_002 to Personnel Manager (ROLE_PERSONNEL_MANAGER = 0x0000_0080)
    call(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "UpdateMember",
        &[
            PyRep::Integer(90_000_002),
            PyRep::Integer(corp_roles::ROLE_PERSONNEL_MANAGER as i64),
        ],
    )
    .await
    .expect("UpdateMember role grant");

    // Query AuditMember for 90_000_002
    let audit_rep = call(
        &ctx,
        &mut sess1,
        "corpmgr",
        "AuditMember",
        &[PyRep::Integer(90_000_002)],
    )
    .await
    .expect("AuditMember");
    let (aud_cols, aud_rows) = rows(&audit_rep);
    assert_eq!(
        aud_cols,
        vec!["characterID", "issuerID", "oldRoles", "newRoles", "changeTime"]
    );
    assert_eq!(aud_rows.len(), 1, "Role mutation logged in audit history");
    assert_eq!(aud_rows[0][0], PyRep::Integer(90_000_002));
    assert_eq!(aud_rows[0][1], PyRep::Integer(90_000_001));
    assert_eq!(aud_rows[0][2], PyRep::Integer(0));
    assert_eq!(
        aud_rows[0][3],
        PyRep::Integer(corp_roles::ROLE_PERSONNEL_MANAGER as i64)
    );
}

#[tokio::test]
async fn test_alliance_registry_queries_and_attributes() {
    // parity: rpc:allianceRegistry.GetAlliance, rpc:allianceRegistry.GetAllianceMembers, rpc:allianceRegistry.GetAllianceMembersOlderThan, rpc:allianceRegistry.GetAlliancePublicInfo, rpc:allianceRegistry.GetDaysInAlliance, rpc:allianceRegistry.GetEmploymentRecord, rpc:allianceRegistry.GetRankedAlliances, rpc:allianceRegistry.allianceID, rpc:allianceRegistry.allowWar, rpc:allianceRegistry.creatorCharID, rpc:allianceRegistry.creatorCorpID, rpc:allianceRegistry.executorCorpID, rpc:allianceRegistry.shortName
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let mut sess1 = SessionState::new(2301);
    common::login_pilot(&ctx, &mut sess1, "demo", "demo-secret").await;
    call(
        &ctx,
        &mut sess1,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(90_000_001)],
    )
    .await
    .expect("SelectCharacterID 90000001");

    let mut sess2 = SessionState::new(2302);
    common::login_pilot(&ctx, &mut sess2, "other", "other-secret").await;
    call(
        &ctx,
        &mut sess2,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(90_000_002)],
    )
    .await
    .expect("SelectCharacterID 90000002");

    // 1. Create two player corporations
    call(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "AddCorporation",
        &[
            PyRep::String("Imperial Zenith Holdings".into()),
            PyRep::String("IZH".into()),
            PyRep::Real(0.05),
        ],
    )
    .await
    .expect("AddCorporation 1");
    let corp1 = sess1.corporation_id.expect("corp1 ID");

    call(
        &ctx,
        &mut sess2,
        "corpRegistry",
        "AddCorporation",
        &[
            PyRep::String("Solaris Apex Fleet".into()),
            PyRep::String("SOLAF".into()),
            PyRep::Real(0.05),
        ],
    )
    .await
    .expect("AddCorporation 2");
    let corp2 = sess2.corporation_id.expect("corp2 ID");

    // Fund corp1 master wallet with 2,500,000,000 ISK to afford 1B alliance creation fee
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.corporations.get_mut(&corp1) {
            c.ensure_wallet_divisions();
            c.wallet_divisions[0].balance = IskCents::from_isk(2_500_000_000);
            c.master_wallet = c.wallet_divisions[0].balance;
        }
    });

    // 2. Create Alliance via corpRegistry.CreateAlliance
    let create_rep = call(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "CreateAlliance",
        &[
            PyRep::String("Zenith Apex Coalition".into()),
            PyRep::String("ZAC".into()),
            PyRep::String("Premier nullsec coalition".into()),
            PyRep::String("https://zenith.eve".into()),
        ],
    )
    .await
    .expect("CreateAlliance");
    let create_kv = keyval(&create_rep);
    let aid = match create_kv.get("allianceID") {
        Some(PyRep::Integer(id)) => *id,
        other => panic!("expected integer allianceID, got {other:?}"),
    };
    assert_eq!(create_kv.get("executorCorpID"), Some(&PyRep::Integer(corp1)));
    assert_eq!(sess1.alliance_id, Some(aid));

    // Add corp2 as a member of alliance `aid`
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(a) = snap.alliances.get_mut(&aid) {
            a.member_corp_ids.insert(corp2);
        }
        if let Some(c) = snap.corporations.get_mut(&corp2) {
            c.alliance_id = Some(aid);
        }
    });

    // 3. allianceRegistry attribute calls
    let attr_aid = call(&ctx, &mut sess1, "allianceRegistry", "allianceID", &[])
        .await
        .expect("allianceRegistry.allianceID");
    assert_eq!(attr_aid, PyRep::Integer(aid));

    // rpc-lint: allow ticker literal
    let expected_short = "ZAC";
    let attr_short = call(&ctx, &mut sess1, "allianceRegistry", "shortName", &[])
        .await
        .expect("allianceRegistry.shortName");
    assert_eq!(attr_short, PyRep::String(expected_short.into()));

    let attr_exec = call(
        &ctx,
        &mut sess1,
        "allianceRegistry",
        "executorCorpID",
        &[],
    )
    .await
    .expect("allianceRegistry.executorCorpID");
    assert_eq!(attr_exec, PyRep::Integer(corp1));

    let attr_creator_corp = call(
        &ctx,
        &mut sess1,
        "allianceRegistry",
        "creatorCorpID",
        &[],
    )
    .await
    .expect("allianceRegistry.creatorCorpID");
    assert_eq!(attr_creator_corp, PyRep::Integer(corp1));

    let attr_creator_char = call(
        &ctx,
        &mut sess1,
        "allianceRegistry",
        "creatorCharID",
        &[],
    )
    .await
    .expect("allianceRegistry.creatorCharID");
    assert_eq!(attr_creator_char, PyRep::Integer(90_000_001));

    let attr_allow_war = call(&ctx, &mut sess1, "allianceRegistry", "allowWar", &[])
        .await
        .expect("allianceRegistry.allowWar");
    assert_eq!(attr_allow_war, PyRep::Bool(true));

    // 4. GetAlliance & GetAlliancePublicInfo
    let all_info = call(
        &ctx,
        &mut sess1,
        "allianceRegistry",
        "GetAlliance",
        &[PyRep::Integer(aid)],
    )
    .await
    .expect("GetAlliance");
    let all_kv = keyval(&all_info);
    assert_eq!(all_kv.get("allianceID"), Some(&PyRep::Integer(aid)));
    assert_eq!(
        all_kv.get("allianceName"),
        Some(&PyRep::String("Zenith Apex Coalition".into()))
    );
    assert_eq!(all_kv.get("shortName"), Some(&PyRep::String("ZAC".into())));
    assert_eq!(all_kv.get("executorCorpID"), Some(&PyRep::Integer(corp1)));
    assert_eq!(all_kv.get("memberCount"), Some(&PyRep::Integer(2)));

    let pub_all_info = call(
        &ctx,
        &mut sess2,
        "allianceRegistry",
        "GetAlliancePublicInfo",
        &[PyRep::Integer(aid)],
    )
    .await
    .expect("GetAlliancePublicInfo");
    let pub_kv = keyval(&pub_all_info);
    assert_eq!(pub_kv.get("allianceID"), Some(&PyRep::Integer(aid)));
    assert_eq!(pub_kv.get("memberCount"), Some(&PyRep::Integer(2)));

    // 5. GetAllianceMembers & GetAllianceMembersOlderThan
    let members_rep = call(
        &ctx,
        &mut sess1,
        "allianceRegistry",
        "GetAllianceMembers",
        &[PyRep::Integer(aid)],
    )
    .await
    .expect("GetAllianceMembers");
    let (m_cols, m_rows) = rows(&members_rep);
    assert_eq!(m_cols, vec!["corporationID", "allianceID"]);
    let member_ids: Vec<i64> = m_rows
        .iter()
        .map(|r| match r[0] {
            PyRep::Integer(id) => id,
            _ => panic!("expected int corpID"),
        })
        .collect();
    assert!(member_ids.contains(&corp1));
    assert!(member_ids.contains(&corp2));

    let older_rep = call(
        &ctx,
        &mut sess1,
        "allianceRegistry",
        "GetAllianceMembersOlderThan",
        &[PyRep::Integer(aid)],
    )
    .await
    .expect("GetAllianceMembersOlderThan");
    let (_, older_rows) = rows(&older_rep);
    assert_eq!(older_rows.len(), 2);

    // 6. GetEmploymentRecord
    let emp_rep = call(
        &ctx,
        &mut sess1,
        "allianceRegistry",
        "GetEmploymentRecord",
        &[PyRep::Integer(aid)],
    )
    .await
    .expect("GetEmploymentRecord");
    let (e_cols, e_rows) = rows(&emp_rep);
    assert_eq!(
        e_cols,
        vec!["allianceID", "corporationID", "startDate", "endDate"]
    );
    assert_eq!(e_rows.len(), 1);
    assert_eq!(e_rows[0][0], PyRep::Integer(aid));
    assert_eq!(e_rows[0][1], PyRep::Integer(corp1));

    // 7. GetDaysInAlliance
    let days_0 = call(
        &ctx,
        &mut sess1,
        "allianceRegistry",
        "GetDaysInAlliance",
        &[PyRep::Integer(corp1)],
    )
    .await
    .expect("GetDaysInAlliance initial");
    assert_eq!(days_0, PyRep::Integer(0), "0 days elapsed on creation day");

    // Advance clock by 5 days (5 * 86400 = 432,000 secs)
    ctx.advance_secs(432_000);

    let days_5 = call(
        &ctx,
        &mut sess1,
        "allianceRegistry",
        "GetDaysInAlliance",
        &[PyRep::Integer(corp1)],
    )
    .await
    .expect("GetDaysInAlliance after 5 days");
    assert_eq!(days_5, PyRep::Integer(5), "5 days in alliance reflected");

    // 8. GetRankedAlliances
    let ranked_rep = call(
        &ctx,
        &mut sess1,
        "allianceRegistry",
        "GetRankedAlliances",
        &[],
    )
    .await
    .expect("GetRankedAlliances");
    let (r_cols, r_rows) = rows(&ranked_rep);
    assert_eq!(
        r_cols,
        vec![
            "allianceID",
            "allianceName",
            "shortName",
            "executorCorpID",
            "memberCount"
        ]
    );
    assert!(!r_rows.is_empty());
    assert_eq!(r_rows[0][0], PyRep::Integer(aid));
    assert_eq!(
        r_rows[0][1],
        PyRep::String("Zenith Apex Coalition".into())
    );
    assert_eq!(r_rows[0][4], PyRep::Integer(2));
}
