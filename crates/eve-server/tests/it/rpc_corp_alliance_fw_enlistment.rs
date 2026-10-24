//! Parity integration tests for FW Character Enlistment and `corporationSvc` medals, market info, and employment records (`SO-3`).
//!
//! Covers:
//! - `fwCharacterEnlistmentMgr` (6 rows)
//! - `corporationSvc` (9 rows: `CreateMedal`, `GetAllCorpMedals`, `GetCorpInfo`, `GetEmploymentRecord`, `GetMedalDetails`, `GetMedalsReceived`, `GetRecipientsOfMedal`, `GiveMedalToCharacters`, `SetMedalStatus`)

use crate::common;
use eve_db::{IskCents, MarketOrderRecord};
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

    // 2. Reject enlistment with invalid faction and reject SetMyCorpAllowedEnlistmentFactions in NPC corp
    let npc_set_err = call(
        &ctx,
        &mut sess1,
        "fwCharacterEnlistmentMgr",
        "SetMyCorpAllowedEnlistmentFactions",
        &[PyRep::List(vec![PyRep::Integer(500_001)])],
    )
    .await
    .expect_err("NPC corp cannot set allowed enlistment factions");
    assert_eq!(npc_set_err.code, "CorpRoleDenied");

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

    assert_eq!(sess1.war_faction_id, Some(500_001));
    assert_eq!(
        ctx.db.with_snapshot(|s| s.characters.get(&90_000_001).and_then(|c| c.war_faction_id)),
        Some(500_001)
    );

    // Duplicate enlistment while already enlisted must fail
    let dup_err = call(
        &ctx,
        &mut sess1,
        "fwCharacterEnlistmentMgr",
        "CreateMyDirectEnlistment",
        &[PyRep::Integer(500_002)],
    )
    .await
    .expect_err("Duplicate enlistment must fail");
    assert_eq!(dup_err.code, "FwAlreadyEnlisted");

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
    );

    // 4. Remove direct enlistment and verify cooldown block
    call(
        &ctx,
        &mut sess1,
        "fwCharacterEnlistmentMgr",
        "RemoveMyDirectEnlistment",
        &[],
    )
    .await
    .expect("RemoveMyDirectEnlistment");
    assert_eq!(sess1.war_faction_id, None);
    assert_eq!(
        ctx.db.with_snapshot(|s| s.characters.get(&90_000_001).and_then(|c| c.war_faction_id)),
        None
    );

    let cooldown_ts = call(
        &ctx,
        &mut sess1,
        "fwCharacterEnlistmentMgr",
        "GetMyDirectEnlistmentCooldownTimestamp",
        &[],
    )
    .await
    .expect("GetMyDirectEnlistmentCooldownTimestamp post-remove");
    assert!(matches!(cooldown_ts, PyRep::Integer(ts) if ts > 0));

    let cd_err = call(
        &ctx,
        &mut sess1,
        "fwCharacterEnlistmentMgr",
        "CreateMyDirectEnlistment",
        &[PyRep::Integer(500_002)],
    )
    .await
    .expect_err("Re-enlisting during 24h cooldown must fail");
    assert_eq!(cd_err.code, "FwEnlistmentCooldown");

    // 5. Corporation allowed factions configuration
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

    ctx.advance_secs(90_000);
    let disallowed_err = call(
        &ctx,
        &mut sess1,
        "fwCharacterEnlistmentMgr",
        "CreateMyDirectEnlistment",
        &[PyRep::Integer(500_001)],
    )
    .await
    .expect_err("Enlisting in faction disallowed by corp policy must fail");
    assert_eq!(disallowed_err.code, "FwFactionNotAllowedByCorp");

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

    // 1. GetEmploymentRecord before and after founding a player corporation (reverse-chronological order)
    let emp_rep = call(
        &ctx,
        &mut sess1,
        "corporationSvc",
        "GetEmploymentRecord",
        &[PyRep::Integer(90_000_001)],
    )
    .await
    .expect("GetEmploymentRecord initial");
    let (emp_cols, emp_rows) = rows(&emp_rep);
    assert_eq!(emp_cols, vec!["corporationID", "startDate", "endDate"]);
    assert_eq!(emp_rows[0][0], PyRep::Integer(1_000_170));

    ctx.advance_secs(60);
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

    let emp_after = call(
        &ctx,
        &mut sess1,
        "corporationSvc",
        "GetEmploymentRecord",
        &[PyRep::Integer(90_000_001)],
    )
    .await
    .expect("GetEmploymentRecord after AddCorporation");
    let (_, emp_after_rows) = rows(&emp_after);
    assert!(emp_after_rows.len() >= 2);
    assert_eq!(emp_after_rows[0][0], PyRep::Integer(1_000_170));
    assert_ne!(emp_after_rows[0][2], PyRep::None);
    assert_eq!(
        emp_after_rows[1][0],
        PyRep::Integer(corp_id),
        "newly founded corporation must be recorded in employment history"
    );
    assert_eq!(emp_after_rows[1][2], PyRep::None);

    // 2. GetCorpInfo: returns corporate market order summary Rowset by typeID (even when empty) and gates access
    let empty_info = call(&ctx, &mut sess1, "corporationSvc", "GetCorpInfo", &[PyRep::Integer(corp_id)])
        .await
        .expect("GetCorpInfo empty");
    let (_, empty_rows) = rows(&empty_info);
    assert!(empty_rows.is_empty(), "GetCorpInfo must return an empty Rowset when corp has no orders");
    let outsider_info_err = call(&ctx, &mut sess2, "corporationSvc", "GetCorpInfo", &[PyRep::Integer(corp_id)])
        .await
        .expect_err("Outsider must be denied GetCorpInfo");
    assert_eq!(outsider_info_err.code, "CrpAccessDenied");

    ctx.db.with_mut_snapshot(|snap| {
        snap.market_orders.insert(
            700_009_001,
            MarketOrderRecord {
                order_id: 700_009_001,
                owner_id: corp_id,
                station_id: 60_003_760,
                region_id: 10_000_002,
                type_id: 34, // Tritanium
                is_bid: false,
                price: IskCents::from_f64_isk(6.25),
                vol_entered: 1_000,
                vol_remaining: 800,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents(0),
            },
        );
        snap.market_orders.insert(
            700_009_002,
            MarketOrderRecord {
                order_id: 700_009_002,
                owner_id: corp_id,
                station_id: 60_003_760,
                region_id: 10_000_002,
                type_id: 34,
                is_bid: true,
                price: IskCents::from_f64_isk(4.75),
                vol_entered: 500,
                vol_remaining: 500,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents::from_f64_isk(2_375.0),
            },
        );
    });
    let info_rep = call(
        &ctx,
        &mut sess1,
        "corporationSvc",
        "GetCorpInfo",
        &[PyRep::Integer(corp_id)],
    )
    .await
    .expect("GetCorpInfo");
    let (info_cols, info_rows) = rows(&info_rep);
    assert!(info_cols.contains(&"typeID".to_string()));
    assert!(info_cols.contains(&"sellPrice".to_string()));
    assert!(info_cols.contains(&"buyPrice".to_string()));
    assert_eq!(info_rows.len(), 1);
    assert_eq!(info_rows[0][0], PyRep::Integer(34));
    assert_eq!(info_rows[0][1], PyRep::Real(6.25));
    assert_eq!(info_rows[0][2], PyRep::Integer(800));
    assert_eq!(info_rows[0][5], PyRep::Real(4.75));
    assert_eq!(info_rows[0][6], PyRep::Integer(500));

    // 3. CreateMedal with custom graphic layers: requires role, deducts 5,000,000 ISK fee and records medal
    let unauth_medal_err = call(
        &ctx,
        &mut sess2,
        "corporationSvc",
        "CreateMedal",
        &[PyRep::String("Unauth".into()), PyRep::String("Denied".into()), PyRep::List(vec![])],
    )
    .await
    .expect_err("Non-director/non-PM must be denied CreateMedal");
    assert_eq!(unauth_medal_err.code, "CrpAccessDenied");

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
            PyRep::List(vec![
                PyRep::Tuple(vec![
                    PyRep::Integer(1),
                    PyRep::Integer(1),
                    PyRep::String("ribbon_gold".into()),
                    PyRep::Integer(15),
                ]),
                PyRep::Tuple(vec![
                    PyRep::Integer(1),
                    PyRep::Integer(2),
                    PyRep::String("eagle_crest".into()),
                    PyRep::Integer(25),
                ]),
            ]),
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

    let bal_after = ctx
        .db
        .with_snapshot(|s| s.characters.get(&90_000_001).unwrap().balance);
    assert_eq!(
        bal_after,
        bal_before.checked_sub(IskCents::from_isk(5_000_000)).unwrap(),
    );

    // 4. GetAllCorpMedals returns 2-Tuple (medals_rowset, graphics_rowset) and honors target corpID
    let all_medals_rep = call(
        &ctx,
        &mut sess2,
        "corporationSvc",
        "GetAllCorpMedals",
        &[PyRep::Integer(corp_id)],
    )
    .await
    .expect("GetAllCorpMedals with target corpID");
    let PyRep::Tuple(all_pair) = all_medals_rep else {
        panic!("expected 2-Tuple from GetAllCorpMedals");
    };
    assert_eq!(all_pair.len(), 2);
    let (all_cols, all_rows) = rows(&all_pair[0]);
    let (gfx_cols, gfx_rows) = rows(&all_pair[1]);
    assert!(all_cols.contains(&"numberOfRecipients".to_string()));
    assert_eq!(all_rows.len(), 1);
    assert_eq!(all_rows[0][0], PyRep::Integer(medal_id));
    assert_eq!(gfx_cols, vec!["medalID", "part", "layer", "graphic", "color"]);
    assert_eq!(gfx_rows.len(), 2);
    assert_eq!(gfx_rows[0][3], PyRep::String("ribbon_gold".into()));
    assert_eq!(gfx_rows[1][3], PyRep::String("eagle_crest".into()));

    // 5. GiveMedalToCharacters: validates caller role, medal ownership, and recipient existence
    let unauth_give_err = call(
        &ctx,
        &mut sess2,
        "corporationSvc",
        "GiveMedalToCharacters",
        &[PyRep::Integer(medal_id), PyRep::List(vec![PyRep::Integer(90_000_002)]), PyRep::String("Denied".into())],
    )
    .await
    .expect_err("Unauthorized caller must be denied GiveMedalToCharacters");
    assert_eq!(unauth_give_err.code, "CrpAccessDenied");

    let missing_medal_err = call(
        &ctx,
        &mut sess1,
        "corporationSvc",
        "GiveMedalToCharacters",
        &[PyRep::Integer(999_999), PyRep::List(vec![PyRep::Integer(90_000_002)]), PyRep::String("No medal".into())],
    )
    .await
    .expect_err("Unknown medalID must be rejected");
    assert_eq!(missing_medal_err.code, "MedalNotFound");

    let missing_char_err = call(
        &ctx,
        &mut sess1,
        "corporationSvc",
        "GiveMedalToCharacters",
        &[PyRep::Integer(medal_id), PyRep::List(vec![PyRep::Integer(999_999_999)]), PyRep::String("No char".into())],
    )
    .await
    .expect_err("Nonexistent recipient must be rejected");
    assert_eq!(missing_char_err.code, "CharNotFound");

    let give_rep = call(
        &ctx,
        &mut sess1,
        "corporationSvc",
        "GiveMedalToCharacters",
        &[
            PyRep::Integer(medal_id),
            PyRep::List(vec![PyRep::Integer(90_000_002), PyRep::Integer(90_000_001)]),
            PyRep::String("Exemplary electronic warfare support".into()),
        ],
    )
    .await
    .expect("GiveMedalToCharacters batch");
    let give_kv = keyval(&give_rep);
    assert_eq!(give_kv.get("medalID"), Some(&PyRep::Integer(medal_id)));

    // 6. GetMedalDetails returns KeyVal { info: Rowset, graphics: Rowset } across corps and errors on unknown ID
    assert!(
        call(
            &ctx,
            &mut sess2,
            "corporationSvc",
            "GetMedalDetails",
            &[PyRep::Integer(999_999)],
        )
        .await
        .is_err()
    );
    let details_rep = call(
        &ctx,
        &mut sess2,
        "corporationSvc",
        "GetMedalDetails",
        &[PyRep::Integer(medal_id)],
    )
    .await
    .expect("GetMedalDetails cross-corp");
    let det_kv = keyval(&details_rep);
    let (det_cols, det_rows) = rows(det_kv.get("info").expect("info rowset"));
    assert!(det_cols.contains(&"numberOfRecipients".to_string()));
    assert_eq!(det_rows.len(), 1);
    assert_eq!(det_rows[0][6], PyRep::Integer(2));
    let (_, det_gfx_rows) = rows(det_kv.get("graphics").expect("graphics rowset"));
    assert_eq!(det_gfx_rows.len(), 2);

    // 7. GetRecipientsOfMedal lists both recipients with recipientID and date
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
    assert!(rec_cols.contains(&"recipientID".to_string()));
    assert!(rec_cols.contains(&"date".to_string()));
    assert_eq!(rec_rows.len(), 2);

    // 8. GetMedalsReceived returns 2-Tuple (medals, graphics) and enforces privacy (private hidden from third-party)
    let rcv_third_party = call(
        &ctx,
        &mut sess1,
        "corporationSvc",
        "GetMedalsReceived",
        &[PyRep::Integer(90_000_002)],
    )
    .await
    .expect("GetMedalsReceived third-party while private");
    let PyRep::Tuple(tp_pair) = rcv_third_party else {
        panic!("expected 2-Tuple");
    };
    let (_, tp_rows) = rows(&tp_pair[0]);
    assert_eq!(
        tp_rows.len(),
        0,
        "private medal (status=2) must be hidden from third-party viewers"
    );

    let rcv_self = call(
        &ctx,
        &mut sess2,
        "corporationSvc",
        "GetMedalsReceived",
        &[PyRep::Integer(90_000_002)],
    )
    .await
    .expect("GetMedalsReceived owner while private");
    let PyRep::Tuple(self_pair) = rcv_self else {
        panic!("expected 2-Tuple");
    };
    let (_, self_rows) = rows(&self_pair[0]);
    let (_, self_gfx) = rows(&self_pair[1]);
    assert_eq!(self_rows.len(), 1);
    assert_eq!(self_gfx.len(), 2);

    // 9. SetMedalStatus updates display status to 3 (public) -> visible to third-party viewers
    call(
        &ctx,
        &mut sess2,
        "corporationSvc",
        "SetMedalStatus",
        &[PyRep::Integer(medal_id), PyRep::Integer(3)],
    )
    .await
    .expect("SetMedalStatus public");

    let rcv_public = call(
        &ctx,
        &mut sess1,
        "corporationSvc",
        "GetMedalsReceived",
        &[PyRep::Integer(90_000_002)],
    )
    .await
    .expect("GetMedalsReceived third-party after public");
    let PyRep::Tuple(pub_pair) = rcv_public else {
        panic!("expected 2-Tuple");
    };
    let (_, pub_rows) = rows(&pub_pair[0]);
    assert_eq!(pub_rows.len(), 1);
}
