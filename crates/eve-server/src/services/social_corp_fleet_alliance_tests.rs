//! Unit tests for `social_corp_fleet` alliance contacts, bulletins, logo, deliveries, and info queries.

use super::{call_social, kv, rs_rows, setup_alliance_sandbox};
use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_proto::PyRep;

#[tokio::test]
async fn social_alliance_contacts_bulletins_logo_and_deliveries() {
    let (ctx, _clock, mut exec, memb, _) = setup_alliance_sandbox();

    let created = call_social(
        &ctx,
        &mut exec,
        "allianceRegistry",
        "CreateAlliance",
        vec![
            PyRep::String("Southern League".into()),
            PyRep::String("SLEA".into()),
            PyRep::String("Desc".into()),
            PyRep::String("https://slea.eve".into()),
        ],
    )
    .await
    .unwrap();
    let aid = kv(&created, "allianceID").as_i64().unwrap();

    let bad_st = call_social(
        &ctx,
        &mut exec,
        "allianceRegistry",
        "AddAllianceContact",
        vec![PyRep::Integer(98_100_002), PyRep::Real(25.0)],
    )
    .await
    .unwrap_err();
    assert_eq!(bad_st.code, "InvalidStanding");

    let lbl = call_social(
        &ctx,
        &mut exec,
        "allianceRegistry",
        "CreateLabel",
        vec![PyRep::String("Frontline".into()), PyRep::Integer(0xFF0000)],
    )
    .await
    .unwrap();
    let lid = kv(&lbl, "labelID").as_i64().unwrap();
    let _ = call_social(
        &ctx,
        &mut exec,
        "allianceRegistry",
        "EditLabel",
        vec![PyRep::Integer(lid), PyRep::String("Vanguard".into()), PyRep::Integer(0x00FF00)],
    )
    .await
    .unwrap();
    let labels = call_social(&ctx, &mut exec, "allianceRegistry", "GetLabels", vec![])
        .await
        .unwrap();
    assert_eq!(rs_rows(&labels).len(), 1);

    call_social(
        &ctx,
        &mut exec,
        "allianceRegistry",
        "AddAllianceContact",
        vec![PyRep::Integer(98_100_002), PyRep::Real(-10.0), PyRep::Integer(lid)],
    )
    .await
    .unwrap();
    call_social(
        &ctx,
        &mut exec,
        "allianceRegistry",
        "AssignLabels",
        vec![PyRep::Integer(lid), PyRep::List(vec![PyRep::Integer(98_100_002)])],
    )
    .await
    .unwrap();
    call_social(
        &ctx,
        &mut exec,
        "allianceRegistry",
        "RemoveLabels",
        vec![PyRep::Integer(lid), PyRep::List(vec![PyRep::Integer(98_100_002)])],
    )
    .await
    .unwrap();
    let contacts = call_social(&ctx, &mut exec, "allianceRegistry", "GetAllianceContacts", vec![])
        .await
        .unwrap();
    let PyRep::IndexRowset {
        key_column: ally_key_col,
        columns: ally_cols,
        items: ally_items,
    } = contacts
    else {
        panic!("expected IndexRowset for GetAllianceContacts");
    };
    assert_eq!(ally_key_col, "contactID");
    assert_eq!(ally_cols, vec!["contactID", "relationshipID", "labelMask"]);
    assert_eq!(ally_items.len(), 1);
    assert_eq!(ally_items[0].0, PyRep::Integer(98_100_002));
    assert_eq!(ally_items[0].1[0], PyRep::Integer(98_100_002));
    call_social(
        &ctx,
        &mut exec,
        "allianceRegistry",
        "RemoveAllianceContacts",
        vec![PyRep::Integer(98_100_002)],
    )
    .await
    .unwrap();
    let empty_ally_contacts =
        call_social(&ctx, &mut exec, "allianceRegistry", "GetAllianceContacts", vec![])
            .await
            .unwrap();
    assert!(matches!(
        empty_ally_contacts,
        PyRep::IndexRowset { ref key_column, ref items, .. }
            if key_column == "contactID" && items.is_empty()
    ));
    call_social(
        &ctx,
        &mut exec,
        "allianceRegistry",
        "DeleteLabel",
        vec![PyRep::Integer(lid)],
    )
    .await
    .unwrap();

    let bul = call_social(
        &ctx,
        &mut exec,
        "allianceRegistry",
        "AddBulletin",
        vec![PyRep::String("CTA Tonight".into()), PyRep::String("Undock at 20:00".into())],
    )
    .await
    .unwrap();
    let bid = kv(&bul, "bulletinID").as_i64().unwrap();
    call_social(
        &ctx,
        &mut exec,
        "allianceRegistry",
        "UpdateBulletin",
        vec![PyRep::Integer(bid), PyRep::String("CTA Updated".into()), PyRep::String("Formup Jita".into())],
    )
    .await
    .unwrap();
    let bulletins = call_social(&ctx, &mut exec, "allianceRegistry", "GetBulletins", vec![])
        .await
        .unwrap();
    assert_eq!(rs_rows(&bulletins).len(), 1);
    assert_eq!(
        call_social(&ctx, &mut exec, "allianceRegistry", "DeleteBulletin", vec![PyRep::Integer(bid)])
            .await
            .unwrap()
            .as_bool(),
        Some(true)
    );

    let upd_ally = call_social(
        &ctx,
        &mut exec,
        "allianceRegistry",
        "UpdateAlliance",
        vec![
            PyRep::String("Updated Desc".into()),
            PyRep::String("https://updated.eve".into()),
            PyRep::Bool(false),
            PyRep::Bool(true),
        ],
    )
    .await
    .unwrap();
    assert_eq!(kv(&upd_ally, "dictatorial").as_bool(), Some(true));

    let logo = call_social(
        &ctx,
        &mut exec,
        "allianceRegistry",
        "UpdateLogo",
        vec![
            PyRep::Integer(11),
            PyRep::Integer(22),
            PyRep::Integer(33),
            PyRep::Integer(4),
            PyRep::Integer(5),
            PyRep::Integer(6),
        ],
    )
    .await
    .unwrap();
    assert_eq!(
        kv(&logo, "logoPath").as_str(),
        Some(format!("res:/UI/Texture/AllianceLogos/{aid}_128.png").as_str())
    );

    let cap = call_social(&ctx, &mut exec, "allianceRegistry", "GetCapitalSystemInfo", vec![])
        .await
        .unwrap();
    assert_eq!(kv(&cap, "currentCapitalSystem").as_i64(), Some(30_000_142));
    let prime = call_social(&ctx, &mut exec, "allianceRegistry", "GetPrimeTimeInfo", vec![])
        .await
        .unwrap();
    assert_eq!(kv(&prime, "currentPrimeHour").as_i64(), Some(18));
    assert_eq!(
        call_social(&ctx, &mut exec, "allianceRegistry", "GetBillBalance", vec![])
            .await
            .unwrap()
            .as_f64(),
        Some(0.0)
    );
    assert_eq!(
        call_social(&ctx, &mut exec, "allianceRegistry", "SetCapitalSystem", vec![PyRep::Integer(30_000_142)])
            .await
            .unwrap()
            .as_bool(),
        Some(true)
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            66_000_111,
            InventoryItemRecord {
                item_id: 66_000_111,
                type_id: 34,
                owner_id: 91_000_001,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 500,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });
    let deliv = call_social(
        &ctx,
        &mut exec,
        "invbroker",
        "DeliverToCorpHangar",
        vec![PyRep::Integer(66_000_111), PyRep::Integer(2)],
    )
    .await
    .unwrap();
    assert_eq!(kv(&deliv, "flagID").as_i64(), Some(117));

    let deliv_mem = call_social(
        &ctx,
        &mut exec,
        "invbroker",
        "DeliverToCorpMember",
        vec![PyRep::Integer(66_000_111), PyRep::Integer(91_000_001)],
    )
    .await
    .unwrap();
    assert_eq!(kv(&deliv_mem, "ownerID").as_i64(), Some(91_000_001));
    let _ = memb;
}
