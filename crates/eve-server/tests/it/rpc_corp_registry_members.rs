//! Authentic state-backed integration tests for `corpRegistry` corporation lifecycle and member directory RPCs:
//! - Corporation lifecycle: `AddCorporation`, `GetCorporation`, `GetInfoWindowDataForChar`, `UpdateCorporation`, `UpdateLogo`, `UpdateDivisionNames`
//! - Member listings: `GetMember`, `GetMembersPaged`, `GetMembersByIds`, `GetMemberIDsByQuery`

#![forbid(unsafe_code)]

use std::collections::HashMap;

use crate::common;
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

#[tokio::test]
async fn test_corp_creation_info_branding_and_members() {
    // parity: rpc:corpRegistry.AddCorporation
    // parity: rpc:corpRegistry.GetCorporation
    // parity: rpc:corpRegistry.GetInfoWindowDataForChar
    // parity: rpc:corpRegistry.UpdateCorporation
    // parity: rpc:corpRegistry.UpdateLogo
    // parity: rpc:corpRegistry.UpdateDivisionNames
    // parity: rpc:corpRegistry.GetMember
    // parity: rpc:corpRegistry.GetMembersPaged
    // parity: rpc:corpRegistry.GetMembersByIds
    // parity: rpc:corpRegistry.GetMemberIDsByQuery

    let ctx = ServerContext::new();
    let mut sess = select_pilot(&ctx, 60_001, "demo", "demo-secret", PILOT_1).await;

    // 1. AddCorporation (V24 13-arg signature with description, url, taxRate, logo shapes/colors, applicationsEnabled)
    let created = rpc(
        &ctx,
        &mut sess,
        "corpRegistry",
        "AddCorporation",
        &[
            PyRep::String("Astra Vanguard Holdings".into()),
            PyRep::String("ASTRA".into()),
            PyRep::String("High-sec industrial and defense conglomerate".into()),
            PyRep::String("https://astra.example".into()),
            PyRep::Real(0.075),
            PyRep::Integer(415),
            PyRep::Integer(416),
            PyRep::Integer(417),
            PyRep::Integer(671),
            PyRep::Integer(672),
            PyRep::Integer(673),
            PyRep::None,
            PyRep::Bool(false),
        ],
    )
    .await
    .expect("AddCorporation");
    let corp_id = field(&created, "corporationID")
        .as_i64()
        .expect("corporationID");
    assert!(corp_id >= 98_000_000);
    assert_eq!(sess.corporation_id, Some(corp_id));

    // Verify registration fee wallet journal entry and AlreadyCEO guard
    let has_reg_journal = ctx.db.with_snapshot(|snap| {
        snap.wallet_journal.iter().any(|j| {
            j.character_id == PILOT_1
                && j.entry_type == "corporation_registration_fee"
                && (j.amount.to_f64_isk() - (-1_599_800.0)).abs() < 0.01
        })
    });
    assert!(has_reg_journal, "expected corporation_registration_fee journal entry");
    let dup_ceo = rpc(
        &ctx,
        &mut sess,
        "corpRegistry",
        "AddCorporation",
        &[
            PyRep::String("Second Corp".into()),
            PyRep::String("SECND".into()),
            PyRep::Real(0.05),
        ],
    )
    .await;
    assert!(dup_ceo.is_err(), "active CEO cannot found a second corporation");

    // 2. GetCorporation
    let corp_info = rpc(
        &ctx,
        &mut sess,
        "corpRegistry",
        "GetCorporation",
        &[PyRep::Integer(corp_id)],
    )
    .await
    .expect("GetCorporation");
    assert_eq!(field(&corp_info, "corporationID"), PyRep::Integer(corp_id));
    assert_eq!(
        field(&corp_info, "corporationName"),
        PyRep::String("Astra Vanguard Holdings".into())
    );
    assert_eq!(field(&corp_info, "ticker"), PyRep::String("ASTRA".into()));
    assert_eq!(field(&corp_info, "ceoID"), PyRep::Integer(PILOT_1));
    assert_eq!(field(&corp_info, "taxRate"), PyRep::Real(0.075));
    assert_eq!(
        field(&corp_info, "description"),
        PyRep::String("High-sec industrial and defense conglomerate".into())
    );
    assert_eq!(
        field(&corp_info, "url"),
        PyRep::String("https://astra.example".into())
    );
    assert_eq!(field(&corp_info, "shape1"), PyRep::Integer(415));
    assert_eq!(field(&corp_info, "color2"), PyRep::Integer(672));
    assert_eq!(field(&corp_info, "color3"), PyRep::Integer(673));
    assert_eq!(field(&corp_info, "applicationsEnabled"), PyRep::Bool(false));

    // 3. GetInfoWindowDataForChar (assign title 1 and 2, verify active title and title1..title16)
    rpc(
        &ctx,
        &mut sess,
        "corpRegistry",
        "UpdateTitle",
        &[
            PyRep::Integer(1),
            PyRep::String("High Admiral".into()),
            PyRep::Integer(0),
        ],
    )
    .await
    .expect("UpdateTitle 1");
    ctx.db.with_mut_snapshot(|snap| {
        snap.corp_governance
            .entry(corp_id)
            .or_default()
            .member_titles
            .insert(PILOT_1, 1);
    });
    let char_info = rpc(
        &ctx,
        &mut sess,
        "corpRegistry",
        "GetInfoWindowDataForChar",
        &[PyRep::Integer(PILOT_1)],
    )
    .await
    .expect("GetInfoWindowDataForChar");
    assert_eq!(field(&char_info, "corpID"), PyRep::Integer(corp_id));
    assert_eq!(field(&char_info, "corporationID"), PyRep::Integer(corp_id));
    assert_eq!(field(&char_info, "ticker"), PyRep::String("ASTRA".into()));
    assert_eq!(
        field(&char_info, "title"),
        PyRep::String("High Admiral".into())
    );
    assert_eq!(
        field(&char_info, "title1"),
        PyRep::String("High Admiral".into())
    );
    assert_eq!(field(&char_info, "title2"), PyRep::String(String::new()));

    // 4. UpdateCorporation
    let upd_corp = rpc(
        &ctx,
        &mut sess,
        "corpRegistry",
        "UpdateCorporation",
        &[
            PyRep::String("Astra Vanguard - Sovereign Operations".into()),
            PyRep::String("https://astra.vanguard".into()),
            PyRep::Real(0.045),
        ],
    )
    .await
    .expect("UpdateCorporation");
    assert_eq!(field(&upd_corp, "corporationID"), PyRep::Integer(corp_id));
    assert_eq!(
        field(&upd_corp, "description"),
        PyRep::String("Astra Vanguard - Sovereign Operations".into())
    );
    assert_eq!(
        field(&upd_corp, "url"),
        PyRep::String("https://astra.vanguard".into())
    );
    assert_eq!(field(&upd_corp, "taxRate"), PyRep::Real(0.045));

    // 5. UpdateLogo
    let logo_res = rpc(
        &ctx,
        &mut sess,
        "corpRegistry",
        "UpdateLogo",
        &[
            PyRep::Integer(14),
            PyRep::Integer(28),
            PyRep::Integer(42),
            PyRep::Integer(8),
        ],
    )
    .await
    .expect("UpdateLogo");
    assert_eq!(field(&logo_res, "corporationID"), PyRep::Integer(corp_id));
    assert_eq!(field(&logo_res, "shape1"), PyRep::Integer(14));
    assert_eq!(field(&logo_res, "shape2"), PyRep::Integer(28));
    assert_eq!(field(&logo_res, "shape3"), PyRep::Integer(42));
    assert_eq!(field(&logo_res, "color1"), PyRep::Integer(8));

    // 6. UpdateDivisionNames (V24 14-arg signature + diff-only OnCorporationChanged notification)
    let _ = ctx.drain_notifications_for_char(PILOT_1);
    let div14_res = rpc(
        &ctx,
        &mut sess,
        "corpRegistry",
        "UpdateDivisionNames",
        &[
            PyRep::String("Strategic Hangar".into()),
            PyRep::String("2nd Division".into()),
            PyRep::String("3rd Division".into()),
            PyRep::String("4th Division".into()),
            PyRep::String("5th Division".into()),
            PyRep::String("6th Division".into()),
            PyRep::String("7th Division".into()),
            PyRep::String("Master Treasury".into()),
            PyRep::String("Division 2".into()),
            PyRep::String("Division 3".into()),
            PyRep::String("Division 4".into()),
            PyRep::String("Division 5".into()),
            PyRep::String("Division 6".into()),
            PyRep::String("Division 7".into()),
        ],
    )
    .await
    .expect("UpdateDivisionNames 14-arg");
    assert_eq!(
        field(&div14_res, "name"),
        PyRep::String("Strategic Hangar".into())
    );
    let div_notifs = ctx.drain_notifications_for_char(PILOT_1);
    let corp_change = div_notifs
        .iter()
        .find(|n| n.event == "OnCorporationChanged")
        .expect("OnCorporationChanged after UpdateDivisionNames");
    if let Some(PyRep::Dict(entries)) = corp_change.args.get(1) {
        assert_eq!(
            entries.len(),
            2,
            "only division1 and walletDivision1 changed"
        );
    }
    let corp_after_div = rpc(
        &ctx,
        &mut sess,
        "corpRegistry",
        "GetCorporation",
        &[PyRep::Integer(corp_id)],
    )
    .await
    .expect("GetCorporation after UpdateDivisionNames");
    assert_eq!(
        field(&corp_after_div, "division1"),
        PyRep::String("Strategic Hangar".into())
    );
    assert_eq!(
        field(&corp_after_div, "walletDivision1"),
        PyRep::String("Master Treasury".into())
    );

    // Add additional members to test multi-member corporate directory, pagination, query and filtering
    ctx.db.with_mut_snapshot(|snap| {
        let corp = snap.corporations.get_mut(&corp_id).unwrap();
        corp.members.insert(PILOT_2);
        corp.member_roles.insert(PILOT_2, 0);
        corp.members.insert(PILOT_3);
        corp.member_roles.insert(PILOT_3, 0);
        if let Some(ch2) = snap.characters.get_mut(&PILOT_2) {
            ch2.corporation_id = corp_id;
            ch2.name = "Bravo Pilot".into();
        }
        if let Some(mut ch3) = snap.characters.get(&PILOT_2).cloned() {
            ch3.character_id = PILOT_3;
            ch3.corporation_id = corp_id;
            ch3.name = "Charlie Scout".into();
            snap.characters.insert(PILOT_3, ch3);
        }
    });

    // 7. GetMember (single character KeyVal with full V24 fields + corporation rowset + non-member error)
    let single_mem = rpc(
        &ctx,
        &mut sess,
        "corpRegistry",
        "GetMember",
        &[PyRep::Integer(PILOT_1)],
    )
    .await
    .expect("GetMember PILOT_1");
    assert_eq!(field(&single_mem, "characterID"), PyRep::Integer(PILOT_1));
    assert_eq!(field(&single_mem, "corporationID"), PyRep::Integer(corp_id));
    assert_eq!(field(&single_mem, "titleMask"), PyRep::Integer(1));
    assert_eq!(field(&single_mem, "accountKey"), PyRep::Integer(1000));
    assert!(field(&single_mem, "startDateTime").as_i64().unwrap_or(0) > 0);
    assert!(field(&single_mem, "baseID").as_i64().unwrap_or(0) > 0);

    let mem2_res = rpc(
        &ctx,
        &mut sess,
        "corpRegistry",
        "GetMember",
        &[PyRep::Integer(PILOT_2)],
    )
    .await
    .expect("GetMember PILOT_2");
    assert_eq!(field(&mem2_res, "characterID"), PyRep::Integer(PILOT_2));
    assert_eq!(field(&mem2_res, "corporationID"), PyRep::Integer(corp_id));

    assert!(
        rpc(
            &ctx,
            &mut sess,
            "corpRegistry",
            "GetMember",
            &[PyRep::Integer(99_999_999)],
        )
        .await
        .is_err()
    );

    // 8. GetMembersPaged
    let paged1 = rpc(
        &ctx,
        &mut sess,
        "corpRegistry",
        "GetMembersPaged",
        &[
            PyRep::Integer(corp_id),
            PyRep::Integer(1),
            PyRep::Integer(2),
        ],
    )
    .await
    .expect("GetMembersPaged page 1");
    let (_, p1_rows) = rowset_data(&paged1);
    assert_eq!(p1_rows.len(), 2);

    let paged2 = rpc(
        &ctx,
        &mut sess,
        "corpRegistry",
        "GetMembersPaged",
        &[
            PyRep::Integer(corp_id),
            PyRep::Integer(2),
            PyRep::Integer(2),
        ],
    )
    .await
    .expect("GetMembersPaged page 2");
    let (_, p2_rows) = rowset_data(&paged2);
    assert_eq!(p2_rows.len(), 1);
    assert_eq!(p2_rows[0][0], PyRep::Integer(PILOT_3));

    // 9. GetMembersByIds
    let by_ids = rpc(
        &ctx,
        &mut sess,
        "corpRegistry",
        "GetMembersByIds",
        &[
            PyRep::Integer(corp_id),
            PyRep::List(vec![PyRep::Integer(PILOT_2)]),
        ],
    )
    .await
    .expect("GetMembersByIds");
    let (_, i_rows) = rowset_data(&by_ids);
    assert_eq!(i_rows.len(), 1);
    assert_eq!(i_rows[0][0], PyRep::Integer(PILOT_2));

    // 10. GetMemberIDsByQuery
    let by_q_bravo = rpc(
        &ctx,
        &mut sess,
        "corpRegistry",
        "GetMemberIDsByQuery",
        &[PyRep::Integer(corp_id), PyRep::String("bravo".into())],
    )
    .await
    .expect("GetMemberIDsByQuery bravo");
    let (_, q_bravo_rows) = rowset_data(&by_q_bravo);
    assert_eq!(q_bravo_rows.len(), 1);
    assert_eq!(q_bravo_rows[0][0], PyRep::Integer(PILOT_2));
}
