//! Semantic unit tests split from `sov_services_extra_unit_tests`.

#![forbid(unsafe_code)]

use super::sov_services_extra_unit_tests::*;
use crate::services::sov_services::*;
use eve_db::{
    ContractRecord, CorporationRecord, InventoryFlag, InventoryItemRecord,
    IskCents,
};
use eve_proto::PyRep;

#[test]
fn clone_jump_reset_and_contract_helpers_kill_mutations() {
    let ctx = make_ctx();
    let char_id = 90_000_001_i64;
    let player_corp = 98_000_555_i64;
    let station_id = 60_003_760_i64;
    let mut sess = make_session(char_id, 1_000_125, Some(30_000_142));
    sess.station_id = Some(station_id);

    assert_eq!(handle_reset_last_clone_jump_time(&ctx, &sess).unwrap_err().code, "GMRoleRequired");
    sess.role = eve_machonet::service_roles::ROLE_GMH;
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().last_clone_jump_unix = 123_456;
    });
    assert_eq!(handle_reset_last_clone_jump_time(&ctx, &sess).unwrap(), PyRep::Integer(0));
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.characters[&char_id].last_clone_jump_unix, 0);
    });

    assert_eq!(
        handle_contract_delete_notif(&ctx, &sess, "DeleteNotification", &[PyRep::Integer(0)])
            .unwrap_err()
            .code,
        "InvalidArgument"
    );
    assert_eq!(
        handle_contract_delete_notif(&ctx, &sess, "DeleteNotification", &[PyRep::Integer(1), PyRep::Bool(true)])
            .unwrap_err()
            .code,
        "CrpAccessDenied"
    );

    sess.corporation_id = Some(player_corp);
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.insert(
            player_corp,
            { let mut __corp = CorporationRecord::default();
                __corp.corporation_id = player_corp;
                __corp.ceo_id = 90_000_999;
                __corp.name = "PlayerCorp".into();
                __corp.ticker = "PCRP".into();
                __corp },
        );
    });
    assert_eq!(
        handle_contract_delete_notif(&ctx, &sess, "DeleteNotification", &[PyRep::Integer(1), PyRep::Integer(1)])
            .unwrap_err()
            .code,
        "CrpAccessDenied"
    );

    sess.corp_role = eve_db::corp_roles::ROLE_CONTRACT_MANAGER;
    assert_eq!(
        handle_contract_delete_notif(&ctx, &sess, "DeleteNotification", &[PyRep::Integer(999_999), PyRep::Bool(true)])
            .unwrap_err()
            .code,
        "ContractNotFound"
    );

    let (unrelated_cid, corp_cid) = (770_001_i64, 770_002_i64);
    ctx.db.with_mut_snapshot(|snap| {
        snap.contracts.insert(
            unrelated_cid,
            ContractRecord {
                contract_id: unrelated_cid,
                issuer_id: 90_000_888,
                issuer_corp_id: 98_000_888,
                status: "Outstanding".into(),
                ..Default::default()
            },
        );
        snap.contracts.insert(
            corp_cid,
            ContractRecord {
                contract_id: corp_cid,
                issuer_id: 90_000_888,
                issuer_corp_id: player_corp,
                for_corp: true,
                status: "Expired".into(),
                start_station_id: station_id,
                reward: IskCents(0),
                ..Default::default()
            },
        );
    });
    assert_eq!(
        handle_contract_delete_notif(
            &ctx,
            &sess,
            "DeleteNotification",
            &[PyRep::Integer(unrelated_cid), PyRep::Bool(false)]
        )
        .unwrap_err()
        .code,
        "ContractAccessDenied"
    );
    assert_eq!(
        handle_contract_delete_notif(
            &ctx,
            &sess,
            "DeleteNotification",
            &[PyRep::Integer(corp_cid), PyRep::Bool(true)]
        )
        .unwrap(),
        PyRep::Bool(true)
    );

    assert_eq!(handle_contract_delete_multiple(&ctx, &sess, &[]).unwrap_err().code, "InvalidArgument");
    let del_expired = handle_contract_delete_multiple(&ctx, &sess, &[PyRep::Integer(corp_cid)]).unwrap();
    if let PyRep::Tuple(parts) = del_expired {
        assert_eq!(parts[0], PyRep::List(vec![]));
        assert_eq!(parts[1], PyRep::List(vec![PyRep::Integer(corp_cid)]));
    } else {
        panic!("expected Tuple");
    }
    let corp_cid_out = 770_003_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.contracts.insert(
            corp_cid_out,
            ContractRecord {
                contract_id: corp_cid_out,
                issuer_id: 90_000_888,
                issuer_corp_id: player_corp,
                for_corp: true,
                status: "Outstanding".into(),
                start_station_id: station_id,
                reward: IskCents(0),
                ..Default::default()
            },
        );
    });
    let del_single = handle_contract_delete_multiple(&ctx, &sess, &[PyRep::Integer(corp_cid_out)]).unwrap();
    if let PyRep::Tuple(parts) = del_single {
        assert_eq!(parts[0], PyRep::List(vec![PyRep::Integer(corp_cid_out)]));
        assert_eq!(parts[1], PyRep::List(vec![]));
    } else {
        panic!("expected Tuple");
    }

    let (bpc_id, stack_id) = (880_601_i64, 880_602_i64);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            bpc_id,
            InventoryItemRecord {
                item_id: bpc_id,
                type_id: 687,
                owner_id: player_corp,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                is_blueprint_copy: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            stack_id,
            InventoryItemRecord {
                item_id: stack_id,
                type_id: 34,
                owner_id: player_corp,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 42,
                singleton: false,
                is_blueprint_copy: false,
                ..Default::default()
            },
        );
    });
    let corp_items =
        handle_contract_get_items_in_dockable(&ctx, &sess, &[PyRep::None, PyRep::Bool(true)]).unwrap();
    if let PyRep::List(rows) = corp_items {
        let bpc_row = rows
            .iter()
            .find(|r| r.dict_get("itemID") == Some(&PyRep::Integer(bpc_id)))
            .expect("bpc row");
        assert_eq!(bpc_row.dict_get("singleton"), Some(&PyRep::Integer(2)));
        assert_eq!(bpc_row.dict_get("quantity"), Some(&PyRep::Integer(-2)));

        let stack_row = rows
            .iter()
            .find(|r| r.dict_get("itemID") == Some(&PyRep::Integer(stack_id)))
            .expect("stack row");
        assert_eq!(stack_row.dict_get("singleton"), Some(&PyRep::Integer(0)));
        assert_eq!(stack_row.dict_get("quantity"), Some(&PyRep::Integer(42)));
    } else {
        panic!("expected List");
    }
}

#[test]
fn demoted_or_expelled_director_cannot_use_stale_session_corp_role_on_contract_helpers() {
    let ctx = make_ctx();
    let ceo_id = 90_000_001_i64;
    let ex_dir_id = 90_000_002_i64;
    let real_corp = 98_000_777_i64;
    let station_id = 60_003_760_i64;
    let cid = 770_901_i64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&ex_dir_id) {
            ch.corporation_id = real_corp;
        }
        let mut corp = CorporationRecord::default();
        corp.corporation_id = real_corp;
        corp.ceo_id = ceo_id;
        corp.name = "RealCorp".into();
        corp.ticker = "RCRP".into();
        corp.members = [ceo_id, ex_dir_id].into_iter().collect();
        corp.member_roles = [
            (ceo_id, u64::MAX),
            (
                ex_dir_id,
                eve_db::corp_roles::ROLE_DIRECTOR | eve_db::corp_roles::ROLE_CONTRACT_MANAGER,
            ),
        ]
        .into_iter()
        .collect();
        snap.corporations.insert(real_corp, corp);
        snap.contracts.insert(
            cid,
            ContractRecord {
                contract_id: cid,
                issuer_id: ceo_id,
                issuer_corp_id: real_corp,
                for_corp: true,
                status: "Outstanding".into(),
                start_station_id: station_id,
                reward: IskCents(0),
                ..Default::default()
            },
        );
    });

    let mut stale_sess = make_session(ex_dir_id, real_corp, Some(30_000_142));
    stale_sess.corp_role =
        eve_db::corp_roles::ROLE_DIRECTOR | eve_db::corp_roles::ROLE_CONTRACT_MANAGER;

    // 1) Demote ex_dir_id in DB (`member_roles` set to 0) -> stale session must be denied.
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations
            .get_mut(&real_corp)
            .unwrap()
            .member_roles
            .insert(ex_dir_id, 0);
        snap.corp_governance
            .entry(real_corp)
            .or_default()
            .role_drop_timestamps
            .insert(ex_dir_id, ctx.now_unix());
    });
    assert_eq!(
        handle_contract_delete_notif(
            &ctx,
            &stale_sess,
            "DeleteContractNotification",
            &[PyRep::Integer(cid), PyRep::Bool(true)],
        )
        .unwrap_err()
        .code,
        "CrpAccessDenied"
    );
    let bulk_demoted =
        handle_contract_delete_multiple(&ctx, &stale_sess, &[PyRep::Integer(cid)]).unwrap();
    if let PyRep::Tuple(parts) = bulk_demoted {
        assert_eq!(parts[0], PyRep::List(vec![]));
        assert_eq!(parts[1], PyRep::List(vec![PyRep::Integer(cid)]));
    } else {
        panic!("expected Tuple from DeleteMultipleContracts");
    }

    // 2) Expel ex_dir_id (`members` / `member_roles` / `role_drop_timestamps` removed and >24h elapsed)
    // -> stale session must STILL be denied.
    ctx.db.with_mut_snapshot(|snap| {
        let corp = snap.corporations.get_mut(&real_corp).unwrap();
        corp.members.remove(&ex_dir_id);
        corp.member_roles.remove(&ex_dir_id);
        if let Some(gov) = snap.corp_governance.get_mut(&real_corp) {
            gov.role_drop_timestamps.remove(&ex_dir_id);
        }
    });
    ctx.advance_secs(90_000);

    assert_eq!(
        handle_contract_delete_notif(
            &ctx,
            &stale_sess,
            "DeleteContractNotification",
            &[PyRep::Integer(cid), PyRep::Bool(true)],
        )
        .unwrap_err()
        .code,
        "CrpAccessDenied"
    );
    let bulk_expelled =
        handle_contract_delete_multiple(&ctx, &stale_sess, &[PyRep::Integer(cid)]).unwrap();
    if let PyRep::Tuple(parts) = bulk_expelled {
        assert_eq!(parts[0], PyRep::List(vec![]));
        assert_eq!(parts[1], PyRep::List(vec![PyRep::Integer(cid)]));
    } else {
        panic!("expected Tuple from DeleteMultipleContracts");
    }
    ctx.db.with_snapshot(|snap| {
        assert!(snap.contracts.contains_key(&cid), "demoted/expelled director must not delete corp contract");
    });
}
