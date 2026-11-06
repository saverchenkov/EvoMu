//! Additional branch-coverage and mutation-killing unit tests for `petitioner_ops.rs`.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::account_services::account_services_tests::{auth_session, test_ctx};
use crate::services::account_services::with_store;
use crate::services::petitioner_ops::*;
use eve_db::clock::VirtualServerClock;
use eve_db::{InventoryFlag, InventoryItemRecord, ServerClock};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::HashMap;
use std::sync::Arc;

fn call_pet_rpc(
    ctx: &ServerContext,
    session: &mut SessionState,
    method: &str,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    handle(ctx, session, "petitioner", method, args, &HashMap::new())
}

fn make_gm(user_id: u64, char_id: i64) -> SessionState {
    let mut s = auth_session(user_id, char_id, 98_000_001, 30_000_142);
    s.role |= eve_machonet::service_roles::ROLE_GML | eve_machonet::service_roles::ROLE_GMH;
    s
}

#[test]
fn may_petition_all_return_codes_and_zendesk_country_branches_kill_mutations() {
    let ctx = test_ctx();
    let clock = Arc::new(VirtualServerClock::with_fixed_base(1_775_500_000));
    ctx.set_clock(clock.clone());

    let cid = 90_000_001_i64;
    let uid = 1_u64;
    let mut player = auth_session(uid, cid, 98_000_001, 30_000_142);

    // 1. Character <= 0 or missing in DB -> -6
    let mut zero_char = player.clone();
    zero_char.character_id = Some(0);
    assert_eq!(
        call_pet_rpc(&ctx, &mut zero_char, "MayPetition", &[])
            .unwrap()
            .unwrap(),
        PyRep::Integer(-6)
    );
    let mut missing_char = player.clone();
    missing_char.character_id = Some(99_999_999);
    assert_eq!(
        call_pet_rpc(&ctx, &mut missing_char, "MayPetition", &[])
            .unwrap()
            .unwrap(),
        PyRep::Integer(-6)
    );

    // 2. Underage user -> -1
    with_store(&ctx, |_, store| {
        store.underage_users.insert(cid as u64);
    });
    assert_eq!(
        call_pet_rpc(&ctx, &mut player, "MayPetition", &[PyRep::Integer(1)])
            .unwrap()
            .unwrap(),
        PyRep::Integer(-1)
    );
    with_store(&ctx, |_, store| {
        store.underage_users.clear();
    });

    // 3. Account banned_until_unix > now -> -1
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(acc) = snap.accounts.values_mut().find(|a| a.user_id == uid) {
            acc.banned_until_unix = Some(1_775_600_000);
        }
    });
    assert_eq!(
        call_pet_rpc(&ctx, &mut player, "MayPetition", &[PyRep::Integer(1)])
            .unwrap()
            .unwrap(),
        PyRep::Integer(-1)
    );
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(acc) = snap.accounts.values_mut().find(|a| a.user_id == uid) {
            acc.banned_until_unix = None;
        }
    });

    // 4. Disabled category / sub_id / zendesk_disabled -> -3, and GetZendeskJwtLink / IsZendeskEnabled
    with_store(&ctx, |_, store| {
        store.disabled_categories.insert(101);
        store.user_countries.insert(uid, "IS".into());
    });
    assert_eq!(
        call_pet_rpc(&ctx, &mut player, "GetUserCatalogCountry", &[])
            .unwrap()
            .unwrap(),
        PyRep::String("IS".into())
    );
    assert_eq!(
        call_pet_rpc(
            &ctx,
            &mut player,
            "MayPetition",
            &[PyRep::Integer(1), PyRep::Integer(101)]
        )
        .unwrap()
        .unwrap(),
        PyRep::Integer(-3)
    );

    with_store(&ctx, |_, store| {
        store.disabled_categories.clear();
        store.zendesk_disabled = true;
    });
    assert_eq!(
        call_pet_rpc(&ctx, &mut player, "IsZendeskEnabled", &[])
            .unwrap()
            .unwrap(),
        PyRep::Bool(false)
    );
    assert_eq!(
        call_pet_rpc(&ctx, &mut player, "GetZendeskJwtLink", &[])
            .unwrap()
            .unwrap(),
        PyRep::None
    );
    assert_eq!(
        call_pet_rpc(&ctx, &mut player, "MayPetition", &[PyRep::Integer(1)])
            .unwrap()
            .unwrap(),
        PyRep::Integer(-3)
    );
    with_store(&ctx, |_, store| {
        store.zendesk_disabled = false;
    });

    // 5. Cooldown (-7) vs ready (1) vs >= 10 open tickets (-2)
    let created = call_pet_rpc(
        &ctx,
        &mut player,
        "CreatePetition",
        &[PyRep::String("Ticket 1".into())],
    )
    .unwrap()
    .unwrap();
    assert_eq!(created.dict_get("categoryID"), Some(&PyRep::Integer(1)));

    // Within 60 seconds -> -7
    assert_eq!(
        call_pet_rpc(&ctx, &mut player, "MayPetition", &[PyRep::Integer(1)])
            .unwrap()
            .unwrap(),
        PyRep::Integer(-7)
    );

    // Advance clock by 60 seconds -> 1 (allowed)
    clock.advance_secs(60);
    assert_eq!(
        call_pet_rpc(&ctx, &mut player, "MayPetition", &[PyRep::Integer(1)])
            .unwrap()
            .unwrap(),
        PyRep::Integer(1)
    );

    // Seed 9 more open petitions (total 10 open) -> -2
    with_store(&ctx, |_, store| {
        for _ in 0..9 {
            let id = store.alloc_id();
            store.petitions.insert(
                id,
                PetitionEntry {
                    petition_id: id,
                    petitioner_cid: cid,
                    subject: "Open".into(),
                    description: "Desc".into(),
                    category_id: 1,
                    queue_id: 1,
                    status: "open".into(),
                    claimed_by: None,
                    escalated: false,
                    unread: false,
                    last_unread_message_id: 0,
                    rating: None,
                    response_time_rating: None,
                    helpfulness_rating: None,
                    attitude_rating: None,
                    rating_comment: String::new(),
                    messages: vec![],
                    logs: vec![],
                },
            );
        }
    });
    assert_eq!(
        call_pet_rpc(&ctx, &mut player, "MayPetition", &[PyRep::Integer(1)])
            .unwrap()
            .unwrap(),
        PyRep::Integer(-2)
    );
}

#[test]
fn petition_cancel_delete_claim_unclaim_escalate_close_and_mark_read_edges_kill_mutations() {
    let ctx = test_ctx();
    let cid = 90_000_001_i64;
    let gm1_cid = 90_000_002_i64;
    let gm2_cid = 90_000_003_i64;
    let mut player = auth_session(1, cid, 98_000_001, 30_000_142);
    let mut gm1 = make_gm(2, gm1_cid);
    let mut gm2 = make_gm(3, gm2_cid);

    // Whitespace-only subject rejected
    assert_eq!(
        call_pet_rpc(
            &ctx,
            &mut player,
            "CreatePetition",
            &[PyRep::String("   ".into())]
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidPetitionSubject"
    );

    let p1 = call_pet_rpc(
        &ctx,
        &mut player,
        "CreatePetition",
        &[PyRep::String("First Ticket".into())],
    )
    .unwrap()
    .unwrap()
    .dict_get("petitionID")
    .and_then(PyRep::as_i64)
    .unwrap();

    // Active petition cannot be deleted -> PetitionNotClosed
    assert_eq!(
        call_pet_rpc(&ctx, &mut player, "DeletePetition", &[PyRep::Integer(p1)])
            .unwrap()
            .unwrap_err()
            .code,
        "PetitionNotClosed"
    );

    // Non-owner non-GM cannot cancel or delete
    let mut stranger = auth_session(4, 90_000_004, 98_000_001, 30_000_142);
    assert_eq!(
        call_pet_rpc(&ctx, &mut stranger, "CancelPetition", &[PyRep::Integer(p1)])
            .unwrap()
            .unwrap_err()
            .code,
        "InsufficientRole"
    );

    // CancelPetition succeeds, second CancelPetition fails with PetitionClosed, HasOpenTickets becomes false
    assert_eq!(
        call_pet_rpc(&ctx, &mut player, "CancelPetition", &[PyRep::Integer(p1)])
            .unwrap()
            .unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(
        call_pet_rpc(&ctx, &mut player, "CancelPetition", &[PyRep::Integer(p1)])
            .unwrap()
            .unwrap_err()
            .code,
        "PetitionClosed"
    );
    assert_eq!(
        call_pet_rpc(&ctx, &mut player, "HasOpenTickets", &[])
            .unwrap()
            .unwrap(),
        PyRep::Bool(false)
    );

    // Cancelled petition cannot be rated (requires status == "closed")
    assert_eq!(
        call_pet_rpc(
            &ctx,
            &mut player,
            "AddPetitionRating",
            &[PyRep::Integer(p1), PyRep::Integer(5)]
        )
        .unwrap()
        .unwrap_err()
        .code,
        "PetitionNotClosed"
    );

    // Non-owner non-GM cannot delete cancelled petition, but GM can!
    assert_eq!(
        call_pet_rpc(&ctx, &mut stranger, "DeletePetition", &[PyRep::Integer(p1)])
            .unwrap()
            .unwrap_err()
            .code,
        "InsufficientRole"
    );
    assert_eq!(
        call_pet_rpc(&ctx, &mut gm1, "DeletePetition", &[PyRep::Integer(p1)])
            .unwrap()
            .unwrap(),
        PyRep::Bool(true)
    );

    // Create second petition for GM workflow edge cases
    let p2 = call_pet_rpc(
        &ctx,
        &mut player,
        "CreatePetition",
        &[PyRep::String("Second Ticket".into())],
    )
    .unwrap()
    .unwrap()
    .dict_get("petitionID")
    .and_then(PyRep::as_i64)
    .unwrap();

    // UnClaimPetition on unclaimed petition -> PetitionNotClaimed
    assert_eq!(
        call_pet_rpc(&ctx, &mut gm1, "UnClaimPetition", &[PyRep::Integer(p2)])
            .unwrap()
            .unwrap_err()
            .code,
        "PetitionNotClaimed"
    );

    // EscalatePetition with unknown queue 99 -> InvalidPetitionQueue; empty note logs "Escalated"
    assert_eq!(
        call_pet_rpc(
            &ctx,
            &mut gm1,
            "EscalatePetition",
            &[PyRep::Integer(p2), PyRep::Integer(99)]
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidPetitionQueue"
    );
    assert!(call_pet_rpc(
        &ctx,
        &mut gm1,
        "EscalatePetition",
        &[PyRep::Integer(p2), PyRep::Integer(3)]
    )
    .unwrap()
    .is_ok());

    // Claim by gm1 succeeds; re-claim by gm1 succeeds; claim by gm2 fails with PetitionAlreadyClaimed
    assert!(call_pet_rpc(&ctx, &mut gm1, "ClaimPetition", &[PyRep::Integer(p2)])
        .unwrap()
        .is_ok());
    assert!(call_pet_rpc(&ctx, &mut gm1, "ClaimPetition", &[PyRep::Integer(p2)])
        .unwrap()
        .is_ok());
    assert_eq!(
        call_pet_rpc(&ctx, &mut gm2, "ClaimPetition", &[PyRep::Integer(p2)])
            .unwrap()
            .unwrap_err()
            .code,
        "PetitionAlreadyClaimed"
    );

    // PetitioneeChat by non-GM rejected; by gm1 sets status="Responded", unread=true; MarkAsRead restores "claimed" when claimed_by is Some
    assert_eq!(
        call_pet_rpc(
            &ctx,
            &mut player,
            "PetitioneeChat",
            &[PyRep::Integer(p2), PyRep::String("nope".into())]
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InsufficientRole"
    );
    assert!(call_pet_rpc(
        &ctx,
        &mut gm1,
        "PetitioneeChat",
        &[PyRep::Integer(p2), PyRep::String("Working on it".into())]
    )
    .unwrap()
    .is_ok());
    assert_eq!(
        call_pet_rpc(&ctx, &mut stranger, "MarkAsRead", &[PyRep::Integer(p2)])
            .unwrap()
            .unwrap_err()
            .code,
        "AccessDenied"
    );
    assert!(call_pet_rpc(&ctx, &mut player, "MarkAsRead", &[PyRep::Integer(p2)])
        .unwrap()
        .is_ok());
    with_store(&ctx, |_, store| {
        assert_eq!(store.petitions[&p2].status, "claimed");
    });

    // When unclaimed and Responded, MarkAsRead restores "open"
    assert!(call_pet_rpc(&ctx, &mut gm1, "UnClaimPetition", &[PyRep::Integer(p2)])
        .unwrap()
        .is_ok());
    assert!(call_pet_rpc(
        &ctx,
        &mut gm1,
        "PetitioneeChat",
        &[PyRep::Integer(p2), PyRep::String("Follow-up".into())]
    )
    .unwrap()
    .is_ok());
    assert!(call_pet_rpc(&ctx, &mut player, "MarkAsRead", &[PyRep::Integer(p2)])
        .unwrap()
        .is_ok());
    with_store(&ctx, |_, store| {
        assert_eq!(store.petitions[&p2].status, "open");
    });

    // Claim by gm1, then ClosePetition by gm2 with empty note (no extra message added)
    assert!(call_pet_rpc(&ctx, &mut gm1, "ClaimPetition", &[PyRep::Integer(p2)])
        .unwrap()
        .is_ok());
    let msg_count_before = ctx.db.with_snapshot(|s| {
        crate::services::account_services::load_store(s).petitions[&p2]
            .messages
            .len()
    });
    assert!(call_pet_rpc(&ctx, &mut gm2, "ClosePetition", &[PyRep::Integer(p2)])
        .unwrap()
        .is_ok());
    let msg_count_after = ctx.db.with_snapshot(|s| {
        crate::services::account_services::load_store(s).petitions[&p2]
            .messages
            .len()
    });
    assert_eq!(msg_count_before, msg_count_after);

    // Closed petition rejects Claim, UnClaim, Escalate, Close, and Chat with PetitionClosed
    for m in [
        "ClaimPetition",
        "UnClaimPetition",
        "EscalatePetition",
        "ClosePetition",
        "PetitionerChat",
        "PetitioneeChat",
    ] {
        assert_eq!(
            call_pet_rpc(&ctx, &mut gm1, m, &[PyRep::Integer(p2)])
                .unwrap()
                .unwrap_err()
                .code,
            "PetitionClosed",
            "{m} on closed petition must return PetitionClosed"
        );
    }

    // UpdatePetitionRating: non-owner rejected with CharacterOwnershipMismatch; owner rating clamps to 0..=10
    assert_eq!(
        call_pet_rpc(
            &ctx,
            &mut stranger,
            "UpdatePetitionRating",
            &[PyRep::Integer(p2), PyRep::Integer(5)]
        )
        .unwrap()
        .unwrap_err()
        .code,
        "CharacterOwnershipMismatch"
    );
    let rated = call_pet_rpc(
        &ctx,
        &mut player,
        "UpdatePetitionRating",
        &[
            PyRep::Integer(p2),
            PyRep::Integer(99),
            PyRep::Integer(-5),
            PyRep::Integer(7),
            PyRep::String("Clamped".into()),
        ],
    )
    .unwrap()
    .unwrap();
    assert_eq!(rated.dict_get("rating"), Some(&PyRep::Integer(10)));
    assert_eq!(
        rated.dict_get("helpfulnessRating"),
        Some(&PyRep::Integer(0))
    );
    assert_eq!(rated.dict_get("attitudeRating"), Some(&PyRep::Integer(7)));

    // GetLog & GetPetitionMessages: stranger denied, GM allowed, verifies eventID mapping 1..=8
    assert_eq!(
        call_pet_rpc(&ctx, &mut stranger, "GetLog", &[PyRep::Integer(p2)])
            .unwrap()
            .unwrap_err()
            .code,
        "AccessDenied"
    );
    with_store(&ctx, |_, store| {
        store
            .petitions
            .get_mut(&p2)
            .unwrap()
            .logs
            .push(("Cancelled".into(), 888_888, 1_775_500_000));
    });
    let logs = call_pet_rpc(&ctx, &mut gm1, "GetLog", &[PyRep::Integer(p2)])
        .unwrap()
        .unwrap();
    if let PyRep::List(entries) = logs {
        let eids: Vec<i64> = entries
            .iter()
            .filter_map(|e| e.dict_get("eventID").and_then(PyRep::as_i64))
            .collect();
        for expected_eid in [1, 2, 3, 4, 5, 6, 7, 8] {
            assert!(
                eids.contains(&expected_eid),
                "missing eventID {expected_eid} in {eids:?}"
            );
        }
        let fallback_user = entries
            .last()
            .unwrap()
            .dict_get("userName")
            .and_then(PyRep::as_str);
        assert_eq!(fallback_user, Some("char_888888"));
    } else {
        panic!("expected List");
    }
}

#[test]
fn client_picker_and_property_population_info_branches_kill_mutations() {
    let ctx = test_ctx();
    let cid = 90_000_001_i64;
    let mut player = auth_session(1, cid, 98_000_001, 30_000_142);

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            990_801,
            InventoryItemRecord {
                item_id: 990_801,
                type_id: 587, // Rifter (category 6 Ship)
                owner_id: cid,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });

    // GetClientPickerInfo matching "rifter" vs non-matching "zzzz_no_match" (falls back to solar system)
    let matched = call_pet_rpc(
        &ctx,
        &mut player,
        "GetClientPickerInfo",
        &[PyRep::String("rifter".into())],
    )
    .unwrap()
    .unwrap();
    if let PyRep::List(rows) = matched {
        assert!(!rows.is_empty());
    } else {
        panic!("expected List");
    }

    let fallback_sys = call_pet_rpc(
        &ctx,
        &mut player,
        "GetClientPickerInfo",
        &[PyRep::String("zzzz_no_match_xyz".into())],
    )
    .unwrap()
    .unwrap();
    if let PyRep::List(rows) = fallback_sys {
        assert_eq!(rows.len(), 1);
    } else {
        panic!("expected List");
    }

    // PropertyPopulationInfo by numeric prop_id (2 = SolarSystem, 4 = Character, 3 = ShipType)
    for id in [2_i64, 4_i64, 3_i64] {
        let res = call_pet_rpc(
            &ctx,
            &mut player,
            "PropertyPopulationInfo",
            &[PyRep::Integer(id)],
        )
        .unwrap()
        .unwrap();
        assert!(matches!(res, PyRep::List(ref l) if !l.is_empty()));
    }
}
