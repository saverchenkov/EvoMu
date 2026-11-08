//! Semantic unit tests split from `social_corp_fleet_extra_unit_tests`.

#![forbid(unsafe_code)]

use super::social_corp_fleet_extra_unit_tests::*;
use eve_db::{
    InventoryFlag, InventoryItemRecord, WarRecord,
};
use eve_proto::PyRep;

#[tokio::test]
async fn alliance_getters_kick_war_and_last_corp_close_and_auxiliary_rpcs() {
    let (ctx, mut exec, memb) = setup_sandbox();
    let created = call_social(
        &ctx,
        &mut exec,
        "allianceRegistry",
        "CreateAlliance",
        vec![PyRep::String("Star Fraction".into()), PyRep::String("STAR".into())],
    )
    .await
    .unwrap();
    let aid = created.dict_get("allianceID").unwrap().as_i64().unwrap();

    // 1. Direct property getters on allianceRegistry + GetAlliancePublicInfo + GetAllianceMembersOlderThan + GetDaysInAlliance
    assert_eq!(
        call_social(&ctx, &mut exec, "allianceRegistry", "allianceID", vec![])
            .await
            .unwrap(),
        PyRep::Integer(aid)
    );
    assert_eq!(
        call_social(&ctx, &mut exec, "allianceRegistry", "shortName", vec![])
            .await
            .unwrap(),
        PyRep::String("STAR".into())
    );
    assert_eq!(
        call_social(&ctx, &mut exec, "allianceRegistry", "executorCorpID", vec![])
            .await
            .unwrap(),
        PyRep::Integer(98_100_001)
    );
    assert_eq!(
        call_social(&ctx, &mut exec, "allianceRegistry", "creatorCorpID", vec![])
            .await
            .unwrap(),
        PyRep::Integer(98_100_001)
    );
    assert_eq!(
        call_social(&ctx, &mut exec, "allianceRegistry", "creatorCharID", vec![])
            .await
            .unwrap(),
        PyRep::Integer(91_000_001)
    );
    assert_eq!(
        call_social(&ctx, &mut exec, "allianceRegistry", "allowWar", vec![])
            .await
            .unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(
        call_social(
            &ctx,
            &mut exec,
            "allianceRegistry",
            "GetAlliancePublicInfo",
            vec![PyRep::Integer(aid)],
        )
        .await
        .unwrap()
        .dict_get("shortName"),
        Some(&PyRep::String("STAR".into()))
    );
    assert_eq!(
        rs_rows(
            &call_social(
                &ctx,
                &mut exec,
                "allianceRegistry",
                "GetAllianceMembersOlderThan",
                vec![PyRep::Integer(aid)],
            )
            .await
            .unwrap()
        )
        .len(),
        1
    );
    assert_eq!(
        call_social(&ctx, &mut exec, "allianceRegistry", "GetDaysInAlliance", vec![])
            .await
            .unwrap(),
        PyRep::Integer(0)
    );

    // 2. Contacts (alliance, character, update existing) & AssignLabels scalar arg ordering & auxiliary alliance RPCs
    call_social(
        &ctx,
        &mut exec,
        "allianceRegistry",
        "AddAllianceContact",
        vec![PyRep::Integer(99_000_999), PyRep::Real(5.0)],
    )
    .await
    .unwrap();
    call_social(
        &ctx,
        &mut exec,
        "allianceRegistry",
        "EditAllianceContact",
        vec![PyRep::Integer(91_000_002), PyRep::Real(10.0)],
    )
    .await
    .unwrap();
    call_social(
        &ctx,
        &mut exec,
        "allianceRegistry",
        "EditContactsRelationshipID",
        vec![PyRep::Integer(91_000_002), PyRep::Real(-5.0)],
    )
    .await
    .unwrap();
    // AssignLabels with (contact_id, label_mask) scalar ordering creates/updates contact
    call_social(
        &ctx,
        &mut exec,
        "allianceRegistry",
        "AssignLabels",
        vec![PyRep::Integer(98_100_002), PyRep::Integer(4)],
    )
    .await
    .unwrap();

    assert_eq!(
        call_social(
            &ctx,
            &mut exec,
            "allianceRegistry",
            "AddBulletin",
            vec![PyRep::String("".into()), PyRep::String("Body".into())],
        )
        .await
        .unwrap_err()
        .code,
        "BulletinTitleRequired"
    );
    assert_eq!(
        call_social(
            &ctx,
            &mut exec,
            "allianceRegistry",
            "UpdateBulletin",
            vec![PyRep::Integer(999), PyRep::String("T".into()), PyRep::String("B".into())],
        )
        .await
        .unwrap_err()
        .code,
        "BulletinNotFound"
    );

    // Test real allianceRegistry governance, sovereignty, bills, bulletin ordering, and GM force-accept semantics.
    let bulletins_empty = call_social(&ctx, &mut exec, "allianceRegistry", "GetAllianceBulletins", vec![])
        .await
        .unwrap();
    assert!(matches!(bulletins_empty, PyRep::Rowset { .. }));
    let b1 = call_social(
        &ctx,
        &mut exec,
        "allianceRegistry",
        "AddBulletin",
        vec![PyRep::String("First".into()), PyRep::String("Body 1".into())],
    )
    .await
    .unwrap();
    let b2 = call_social(
        &ctx,
        &mut exec,
        "allianceRegistry",
        "AddBulletin",
        vec![PyRep::String("Second".into()), PyRep::String("Body 2".into())],
    )
    .await
    .unwrap();
    let b1_id = b1.dict_get("bulletinID").and_then(|v| v.as_i64()).unwrap();
    let b2_id = b2.dict_get("bulletinID").and_then(|v| v.as_i64()).unwrap();
    assert_eq!(
        call_social(
            &ctx,
            &mut exec,
            "allianceRegistry",
            "UpdateBulletinOrder",
            vec![PyRep::List(vec![PyRep::Integer(b2_id), PyRep::Integer(b1_id)])],
        )
        .await
        .unwrap(),
        PyRep::Bool(true)
    );
    let ordered = call_social(&ctx, &mut exec, "allianceRegistry", "GetAllianceBulletins", vec![])
        .await
        .unwrap();
    if let PyRep::Rowset { rows, .. } = ordered {
        assert_eq!(rows.first().and_then(|r| r.first()).and_then(|v| v.as_i64()), Some(b2_id));
    } else {
        panic!("expected Rowset from GetAllianceBulletins");
    }

    let aid = exec.alliance_id.unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            1_050_000_001,
            eve_db::UpwellStructureRecord {
                structure_id: 1_050_000_001,
                type_id: 81_242,
                owner_corp_id: 98_100_001,
                solar_system_id: 30_000_142,
                name: "Alliance Sov Hub".into(),
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                shield_hp: 1.0,
                armor_hp: 1.0,
                hull_hp: 1.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
        snap.sov_runtime.adm_vulnerability_windows.insert(1_050_000_001, (1_700_001_000, 1_700_004_600));
        snap.sov_runtime.command_node_scores.insert(1_050_000_001, vec![(aid, 0.75)]);
    });
    let sov_info = call_social(
        &ctx,
        &mut exec,
        "allianceRegistry",
        "GetAllianceSovereigntyStructuresInfo",
        vec![],
    )
    .await
    .unwrap();
    if let PyRep::Tuple(parts) = sov_info {
        assert_eq!(parts.len(), 3);
        assert!(matches!(&parts[0], PyRep::List(l) if l.len() == 1));
        assert!(matches!(&parts[2], PyRep::List(l) if l.len() == 1));
    } else {
        panic!("expected 3-tuple from GetAllianceSovereigntyStructuresInfo");
    }

    assert_eq!(
        call_social(&ctx, &mut exec, "allianceRegistry", "SetCapitalSystem", vec![PyRep::Integer(30_000_142)])
            .await
            .unwrap(),
        PyRep::Bool(true)
    );
    let cap_pending = call_social(&ctx, &mut exec, "allianceRegistry", "GetCapitalSystemInfo", vec![])
        .await
        .unwrap();
    assert_eq!(cap_pending.dict_get("newCapitalSystem"), Some(&PyRep::Integer(30_000_142)));
    assert_eq!(
        call_social(&ctx, &mut exec, "allianceRegistry", "CancelCapitalSystemTransition", vec![])
            .await
            .unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(
        call_social(&ctx, &mut exec, "allianceRegistry", "CancelCapitalSystemTransition", vec![])
            .await
            .unwrap_err()
            .code,
        "NoCapitalSystemTransition"
    );

    assert_eq!(
        call_social(&ctx, &mut exec, "allianceRegistry", "SetPrimeHour", vec![PyRep::Integer(21)])
            .await
            .unwrap(),
        PyRep::Bool(true)
    );
    let prime_info = call_social(&ctx, &mut exec, "allianceRegistry", "GetPrimeTimeInfo", vec![])
        .await
        .unwrap();
    assert_eq!(prime_info.dict_get("newPrimeHour"), Some(&PyRep::Integer(21)));
    assert_eq!(
        call_social(&ctx, &mut exec, "allianceRegistry", "SetPrimeHour", vec![PyRep::Integer(25)])
            .await
            .unwrap_err()
            .code,
        "InvalidPrimeHour"
    );

    let bill_id = 77_001_i64;
    let creditor_corp_id = 98_100_099_i64;
    ctx.db.with_mut_snapshot(|snap| {
        let mut cred = snap.corporations.get(&98_100_001).unwrap().clone();
        cred.corporation_id = creditor_corp_id;
        cred.alliance_id = None;
        cred.master_wallet = eve_db::IskCents(0);
        cred.wallet_divisions = eve_db::CorporationRecord::default_wallet_divisions(eve_db::IskCents(0));
        snap.corporations.insert(creditor_corp_id, cred);
        let bill = crate::services::wallet_services_ext::CorpBillRecord {
            bill_id,
            bill_type_id: 2,
            debtor_id: aid,
            creditor_id: creditor_corp_id,
            amount_cents: eve_db::IskCents::from_isk(2_500_000).0,
            due_unix: 1_700_100_000,
            interest: 0.0,
            external_id: aid,
            paid: false,
            external_id2: -1,
            paid_by_division: None,
        };
        let nid = crate::services::wallet_services_ext::bill_note_id(bill_id);
        snap.owner_notes.insert(
            nid,
            eve_db::OwnerNoteRecord {
                note_id: nid,
                character_id: -71,
                item_id: bill_id,
                label: format!("bill:{bill_id}"),
                note: serde_json::to_string(&bill).unwrap(),
                timestamp_unix: 1_700_000_000,
            },
        );
    });
    assert_eq!(
        call_social(&ctx, &mut exec, "allianceRegistry", "GetBillBalance", vec![PyRep::Integer(bill_id)])
            .await
            .unwrap(),
        PyRep::Real(2_500_000.0)
    );
    let bills_rs = call_social(&ctx, &mut exec, "allianceRegistry", "GetBills", vec![]).await.unwrap();
    assert!(matches!(bills_rs, PyRep::Rowset { ref rows, .. } if rows.len() == 1));
    assert!(matches!(
        call_social(&ctx, &mut exec, "allianceRegistry", "GetBillsReceivable", vec![]).await.unwrap(),
        PyRep::Rowset { .. }
    ));
    assert_eq!(
        call_social(&ctx, &mut exec, "allianceRegistry", "PayBill", vec![PyRep::Integer(bill_id), PyRep::Integer(1000)])
            .await
            .unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(
        call_social(&ctx, &mut exec, "allianceRegistry", "GetBillBalance", vec![PyRep::Integer(bill_id)])
            .await
            .unwrap(),
        PyRep::Real(0.0)
    );
    assert_eq!(
        call_social(&ctx, &mut exec, "allianceRegistry", "PayBill", vec![PyRep::Integer(bill_id), PyRep::Integer(1000)])
            .await
            .unwrap_err()
            .code,
        "BillAlreadyPaid"
    );

    assert_eq!(
        call_social(
            &ctx,
            &mut exec,
            "allianceRegistry",
            "GMForceAcceptApplication",
            vec![PyRep::Integer(98_100_002)],
        )
        .await
        .unwrap_err()
        .code,
        "GMRoleRequired"
    );

    // 3. CannotLeaveAllianceDuringWar when active war exists; removing war and deleting last member closes alliance
    ctx.db.with_mut_snapshot(|snap| {
        snap.wars.insert(
            501,
            WarRecord {
                war_id: 501,
                aggressor_corp_id: aid,
                defender_corp_id: 98_100_002,
                mutual: true,
                active: true,
                started_unix: 1_000,
            },
        );
    });
    assert_eq!(
        call_social(
            &ctx,
            &mut exec,
            "allianceRegistry",
            "DeleteMember",
            vec![PyRep::Integer(98_100_001)],
        )
        .await
        .unwrap_err()
        .code,
        "CannotLeaveAllianceDuringWar"
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.wars.clear();
    });
    let pending_del = call_social(
        &ctx,
        &mut exec,
        "allianceRegistry",
        "DeleteMember",
        vec![PyRep::Integer(98_100_001)],
    )
    .await
    .unwrap();
    assert_eq!(pending_del.dict_get("allianceClosed"), Some(&PyRep::Bool(false)));

    let closed = call_social(
        &ctx,
        &mut exec,
        "allianceRegistry",
        "KickMember",
        vec![PyRep::Integer(98_100_001)],
    )
    .await
    .unwrap();
    assert_eq!(closed.dict_get("allianceClosed"), Some(&PyRep::Bool(true)));
    let _ = memb;
}

#[tokio::test]
async fn deliver_to_corp_hangar_and_member_error_guards() {
    let (ctx, mut exec, _memb) = setup_sandbox();

    // 1. NotDocked guard
    exec.station_id = None;
    assert_eq!(
        call_social(
            &ctx,
            &mut exec,
            "invbroker",
            "DeliverToCorpHangar",
            vec![PyRep::Integer(1)],
        )
        .await
        .unwrap_err()
        .code,
        "NotDocked"
    );
    exec.station_id = Some(60_003_760);

    // 2. Empty item list -> ItemNotFound
    assert_eq!(
        call_social(
            &ctx,
            &mut exec,
            "invbroker",
            "DeliverToCorpHangar",
            vec![PyRep::List(vec![])],
        )
        .await
        .unwrap_err()
        .code,
        "ItemNotFound"
    );

    // 3. TargetNotInCorporation for DeliverToCorpMember
    let valid_item = 66_100_001_i64;
    let container_id = 66_100_002_i64;
    let inside_item = 66_100_003_i64;
    let courier_item = 66_100_004_i64;
    let fitted_item = 66_100_005_i64;
    let corp_div_item = 66_100_006_i64;
    let active_ship = 55_000_000 + 91_000_001;

    ctx.db.with_mut_snapshot(|snap| {
        for (id, owner, loc, flag, typ) in [
            (valid_item, 91_000_001, 60_003_760, InventoryFlag::Hangar, 34_u32),
            (container_id, 91_000_001, 60_003_760, InventoryFlag::Hangar, 3_293),
            (inside_item, 91_000_001, container_id, InventoryFlag::Cargo, 34),
            (courier_item, 91_000_001, 60_003_760, InventoryFlag::Hangar, 24_445),
            (fitted_item, 91_000_001, 60_003_760, InventoryFlag::HiSlot0, 34),
            (corp_div_item, 98_100_001, 60_003_760, InventoryFlag::CorpSAG2, 34),
            (active_ship, 91_000_001, 60_003_760, InventoryFlag::Hangar, 606),
        ] {
            snap.items.insert(
                id,
                InventoryItemRecord {
                    item_id: id,
                    type_id: typ,
                    owner_id: owner,
                    location_id: loc,
                    flag,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
        }
        snap.item_custom_names
            .insert(courier_item, "courier_cargo:test".into());
    });

    assert_eq!(
        call_social(
            &ctx,
            &mut exec,
            "invbroker",
            "DeliverToCorpMember",
            vec![PyRep::Integer(valid_item), PyRep::Integer(91_000_002)],
        )
        .await
        .unwrap_err()
        .code,
        "TargetNotInCorporation"
    );

    // 4. ItemInsideContainer, ItemCannotBeTransferred (courier & fitted), CannotDeliverActiveShip
    for (item_id, expected_code) in [
        (inside_item, "ItemInsideContainer"),
        (courier_item, "ItemCannotBeTransferred"),
        (active_ship, "CannotDeliverActiveShip"),
        (fitted_item, "ItemCannotBeTransferred"),
    ] {
        assert_eq!(
            call_social(
                &ctx,
                &mut exec,
                "invbroker",
                "DeliverToCorpHangar",
                vec![PyRep::Integer(item_id)],
            )
            .await
            .unwrap_err()
            .code,
            expected_code
        );
    }

    // 5. Line member without Trader/division-take role cannot deliver corp-owned item
    let mut line_sess = exec.clone();
    line_sess.character_id = Some(91_000_003);
    line_sess.corp_role = 0;
    assert_eq!(
        call_social(
            &ctx,
            &mut line_sess,
            "invbroker",
            "DeliverToCorpMember",
            vec![PyRep::Integer(corp_div_item), PyRep::Integer(91_000_001)],
        )
        .await
        .unwrap_err()
        .code,
        "CorpRoleDenied"
    );
}
