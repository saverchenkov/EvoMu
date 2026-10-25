//! Dedicated integration tests for Petitioner Services (`petitioner`) - Part 1.
//!
//! Validates client contract parity for petition creation, chat messaging,
//! GM queue management, claims, escalations, cancellations, and ratings.

use crate::common;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::HashMap;

const PILOT_1: i64 = 90_000_001;
const PILOT_2: i64 = 90_000_002;
const GM_PILOT: i64 = 90_000_003;

async fn login_char(
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
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, eve_proto::BridgeRpcError> {
    ctx.dispatch_rpc(sess, "petitioner", None, method, args, &HashMap::new(), None)
        .await
}

#[tokio::test]
async fn test_petitioner_lifecycle_create_chat_ratings() {
    // parity: rpc:petitioner.CreatePetition, rpc:petitioner.MayPetition, rpc:petitioner.HasOpenTickets
    // parity: rpc:petitioner.PetitionerChat, rpc:petitioner.PetitioneeChat, rpc:petitioner.ClosePetition
    // parity: rpc:petitioner.AddPetitionRating, rpc:petitioner.UpdatePetitionRating
    let ctx = ServerContext::new();
    let mut p1 = login_char(&ctx, 61_001, "demo", "demo-secret", PILOT_1).await;
    let mut p2 = login_char(&ctx, 61_002, "other", "other-secret", PILOT_2).await;

    ctx.db.provision_gm_account("gm_pet_admin", "gm-pass", GM_PILOT, "GM Senior Staff");
    let mut gm = login_char(&ctx, 61_003, "gm_pet_admin", "gm-pass", GM_PILOT).await;

    // 1. MayPetition & HasOpenTickets checks before any petitions exist
    let may = rpc(&ctx, &mut p1, "MayPetition", &[]).await.expect("MayPetition");
    assert_eq!(may, PyRep::Bool(true));

    let open_init = rpc(&ctx, &mut p1, "HasOpenTickets", &[]).await.expect("HasOpenTickets initial");
    assert_eq!(open_init, PyRep::Bool(false));

    // 2. CreatePetition validations: empty subject and invalid category rejected
    let empty_sub = rpc(&ctx, &mut p1, "CreatePetition", &[PyRep::String("  ".into())]).await;
    assert_eq!(empty_sub.unwrap_err().code, "InvalidPetitionSubject");

    let bad_cat = rpc(
        &ctx,
        &mut p1,
        "CreatePetition",
        &[
            PyRep::String("Valid Subject".into()),
            PyRep::String("Valid Description".into()),
            PyRep::Integer(999_999),
        ],
    )
    .await;
    assert_eq!(bad_cat.unwrap_err().code, "InvalidPetitionCategory");

    // 3. Valid CreatePetition call
    let created = rpc(
        &ctx,
        &mut p1,
        "CreatePetition",
        &[
            PyRep::String("Stuck in wormhole geometry".into()),
            PyRep::String("Ship geometry clipped into celestial structure".into()),
            PyRep::Integer(1),
        ],
    )
    .await
    .expect("CreatePetition");
    let PyRep::KeyVal(created_kv) = created else { panic!("expected KeyVal"); };
    let pet_id = created_kv.get("petitionID").and_then(|v| v.as_i64()).expect("petitionID");
    assert!(pet_id > 0);
    assert_eq!(created_kv.get("status").and_then(|v| v.as_str()), Some("open"));
    assert_eq!(created_kv.get("categoryID").and_then(|v| v.as_i64()), Some(1));

    let has_open = rpc(&ctx, &mut p1, "HasOpenTickets", &[]).await.expect("HasOpenTickets after create");
    assert_eq!(has_open, PyRep::Bool(true));

    // 4. PetitionerChat: other player cannot post, owner pilot succeeds
    let unauth_chat = rpc(
        &ctx,
        &mut p2,
        "PetitionerChat",
        &[PyRep::Integer(pet_id), PyRep::String("Sneaky intrusion".into())],
    )
    .await;
    assert_eq!(unauth_chat.unwrap_err().code, "AccessDenied");

    let p1_chat = rpc(
        &ctx,
        &mut p1,
        "PetitionerChat",
        &[PyRep::Integer(pet_id), PyRep::String("Thrusters are offline as well".into())],
    )
    .await
    .expect("PetitionerChat");
    let PyRep::KeyVal(p1_kv) = p1_chat else { panic!("expected KeyVal"); };
    assert_eq!(p1_kv.get("petitionID").and_then(|v| v.as_i64()), Some(pet_id));
    assert_eq!(p1_kv.get("messageCount").and_then(|v| v.as_i64()), Some(2));

    // 5. PetitioneeChat: non-GM rejected, GM succeeds and sets status to Responded
    let non_gm_reply = rpc(
        &ctx,
        &mut p1,
        "PetitioneeChat",
        &[PyRep::Integer(pet_id), PyRep::String("Unauthorized reply".into())],
    )
    .await;
    assert_eq!(non_gm_reply.unwrap_err().code, "InsufficientRole");

    let gm_reply = rpc(
        &ctx,
        &mut gm,
        "PetitioneeChat",
        &[PyRep::Integer(pet_id), PyRep::String("GM reviewing coordinate grid mesh".into())],
    )
    .await
    .expect("PetitioneeChat");
    let PyRep::KeyVal(gm_kv) = gm_reply else { panic!("expected KeyVal"); };
    assert_eq!(gm_kv.get("messageCount").and_then(|v| v.as_i64()), Some(3));

    // 6. ClosePetition: non-GM rejected, GM succeeds
    let non_gm_close = rpc(&ctx, &mut p1, "ClosePetition", &[PyRep::Integer(pet_id)]).await;
    assert_eq!(non_gm_close.unwrap_err().code, "InsufficientRole");

    let gm_close = rpc(
        &ctx,
        &mut gm,
        "ClosePetition",
        &[
            PyRep::Integer(pet_id),
            PyRep::String("Relocated vessel to system star beacon".into()),
        ],
    )
    .await
    .expect("ClosePetition");
    let PyRep::KeyVal(close_kv) = gm_close else { panic!("expected KeyVal"); };
    assert_eq!(close_kv.get("status").and_then(|v| v.as_str()), Some("closed"));

    // 7. Closed petition rejects further chat messages
    let closed_chat = rpc(
        &ctx,
        &mut p1,
        "PetitionerChat",
        &[PyRep::Integer(pet_id), PyRep::String("Too late message".into())],
    )
    .await;
    assert_eq!(closed_chat.unwrap_err().code, "PetitionClosed");

    // 8. AddPetitionRating: V24 client SetRating passes 6 args:
    // (petitionID, responseTimeRating, helpfulnessRating, attitudeRating, newComment, ratingtime)
    let non_owner_rate = rpc(
        &ctx,
        &mut p2,
        "AddPetitionRating",
        &[
            PyRep::Integer(pet_id),
            PyRep::Integer(5),
            PyRep::Integer(5),
            PyRep::Integer(5),
            PyRep::String("Invalid rater".into()),
            PyRep::Integer(123456789),
        ],
    )
    .await;
    assert_eq!(non_owner_rate.unwrap_err().code, "CharacterOwnershipMismatch");

    let add_rate = rpc(
        &ctx,
        &mut p1,
        "AddPetitionRating",
        &[
            PyRep::Integer(pet_id),
            PyRep::Integer(9),
            PyRep::Integer(10),
            PyRep::Integer(10),
            PyRep::String("Swift emergency response, thank you!".into()),
            PyRep::Integer(123456789),
        ],
    )
    .await
    .expect("AddPetitionRating");
    let PyRep::KeyVal(rate_kv) = add_rate else { panic!("expected KeyVal"); };
    assert_eq!(rate_kv.get("petitionID").and_then(|v| v.as_i64()), Some(pet_id));
    assert_eq!(rate_kv.get("rating").and_then(|v| v.as_i64()), Some(9));
    assert_eq!(
        rate_kv.get("comment").and_then(|v| v.as_str()),
        Some("Swift emergency response, thank you!")
    );

    // 9. UpdatePetitionRating: V24 client SetRating passes 5 args:
    // (petitionID, responseTimeRating, helpfulnessRating, attitudeRating, newComment)
    let update_rate = rpc(
        &ctx,
        &mut p1,
        "UpdatePetitionRating",
        &[
            PyRep::Integer(pet_id),
            PyRep::Integer(10),
            PyRep::Integer(10),
            PyRep::Integer(10),
            PyRep::String("Updated rating: exemplary support!".into()),
        ],
    )
    .await
    .expect("UpdatePetitionRating");
    let PyRep::KeyVal(u_rate_kv) = update_rate else { panic!("expected KeyVal"); };
    assert_eq!(u_rate_kv.get("rating").and_then(|v| v.as_i64()), Some(10));
    assert_eq!(
        u_rate_kv.get("comment").and_then(|v| v.as_str()),
        Some("Updated rating: exemplary support!")
    );
}

#[tokio::test]
async fn test_petitioner_cancellation_and_queues() {
    // parity: rpc:petitioner.CancelPetition, rpc:petitioner.DeletePetition, rpc:petitioner.GetQueues
    // parity: rpc:petitioner.GetPetitionQueue, rpc:petitioner.ClaimPetition, rpc:petitioner.UnClaimPetition
    // parity: rpc:petitioner.GetClaimedPetitions, rpc:petitioner.EscalatePetition
    let ctx = ServerContext::new();
    let mut p1 = login_char(&ctx, 61_004, "demo", "demo-secret", PILOT_1).await;
    let mut p2 = login_char(&ctx, 61_005, "other", "other-secret", PILOT_2).await;

    ctx.db.provision_gm_account("gm_queue_admin", "gm-pass", GM_PILOT, "GM Operations");
    let mut gm = login_char(&ctx, 61_006, "gm_queue_admin", "gm-pass", GM_PILOT).await;

    // 1. GetQueues: non-GM rejected, GM retrieves standard queue list
    let non_gm_q = rpc(&ctx, &mut p1, "GetQueues", &[]).await;
    assert_eq!(non_gm_q.unwrap_err().code, "InsufficientRole");

    let queues = rpc(&ctx, &mut gm, "GetQueues", &[]).await.expect("GetQueues");
    let PyRep::List(q_list) = queues else { panic!("expected List"); };
    assert!(q_list.iter().any(|q| matches!(q, PyRep::KeyVal(kv) if kv.get("queueID") == Some(&PyRep::Integer(1)) && kv.get("queueName") == Some(&PyRep::String("Default".into())))));
    assert!(q_list.iter().any(|q| matches!(q, PyRep::KeyVal(kv) if kv.get("queueID") == Some(&PyRep::Integer(2)) && kv.get("queueName") == Some(&PyRep::String("Escalated".into())))));

    // 2. Create two petitions for queue and lifecycle testing
    let pet_a = rpc(
        &ctx,
        &mut p1,
        "CreatePetition",
        &[
            PyRep::String("Issue Alpha".into()),
            PyRep::String("Alpha details".into()),
            PyRep::Integer(1),
        ],
    )
    .await
    .expect("Create Alpha")
    .dict_get("petitionID")
    .and_then(|v| v.as_i64())
    .unwrap();

    let pet_b = rpc(
        &ctx,
        &mut p1,
        "CreatePetition",
        &[
            PyRep::String("Issue Beta".into()),
            PyRep::String("Beta details".into()),
            PyRep::Integer(2),
        ],
    )
    .await
    .expect("Create Beta")
    .dict_get("petitionID")
    .and_then(|v| v.as_i64())
    .unwrap();

    // 3. GetPetitionQueue: GM inspects queue 1 and verifies presence of both petitions
    let q1_before = rpc(&ctx, &mut gm, "GetPetitionQueue", &[PyRep::Integer(1)]).await.expect("GetPetitionQueue 1");
    let PyRep::List(q1_list) = q1_before else { panic!("expected List"); };
    assert!(q1_list.iter().any(|p| matches!(p, PyRep::KeyVal(kv) if kv.get("petitionID") == Some(&PyRep::Integer(pet_a)))));
    assert!(q1_list.iter().any(|p| matches!(p, PyRep::KeyVal(kv) if kv.get("petitionID") == Some(&PyRep::Integer(pet_b)))));

    let q2_before = rpc(&ctx, &mut gm, "GetPetitionQueue", &[PyRep::Integer(2)]).await.expect("GetPetitionQueue 2");
    let PyRep::List(q2_list) = q2_before else { panic!("expected List"); };
    assert!(!q2_list.iter().any(|p| matches!(p, PyRep::KeyVal(kv) if kv.get("petitionID") == Some(&PyRep::Integer(pet_a)))));

    // 4. ClaimPetition: GM claims petition Alpha
    let non_gm_claim = rpc(&ctx, &mut p1, "ClaimPetition", &[PyRep::Integer(pet_a)]).await;
    assert_eq!(non_gm_claim.unwrap_err().code, "InsufficientRole");

    let claim_res = rpc(&ctx, &mut gm, "ClaimPetition", &[PyRep::Integer(pet_a)]).await.expect("ClaimPetition Alpha");
    let PyRep::KeyVal(claim_kv) = claim_res else { panic!("expected KeyVal"); };
    assert_eq!(claim_kv.get("status").and_then(|v| v.as_str()), Some("claimed"));

    // 5. GetClaimedPetitions: GM lists claimed petitions
    let non_gm_claimed = rpc(&ctx, &mut p1, "GetClaimedPetitions", &[]).await;
    assert_eq!(non_gm_claimed.unwrap_err().code, "InsufficientRole");

    let claimed_list = rpc(&ctx, &mut gm, "GetClaimedPetitions", &[]).await.expect("GetClaimedPetitions");
    let PyRep::List(claimed_rows) = claimed_list else { panic!("expected List"); };
    assert!(claimed_rows.iter().any(|p| matches!(p, PyRep::KeyVal(kv) if kv.get("petitionID") == Some(&PyRep::Integer(pet_a)))));

    // 6. EscalatePetition: invalid queue rejected; escalating moves petition to queue 2,
    // clears claimedBy, and sets escalated flag
    let bad_q = rpc(
        &ctx,
        &mut gm,
        "EscalatePetition",
        &[PyRep::Integer(pet_a), PyRep::Integer(9999)],
    )
    .await;
    assert_eq!(bad_q.unwrap_err().code, "InvalidPetitionQueue");

    let escalated = rpc(
        &ctx,
        &mut gm,
        "EscalatePetition",
        &[
            PyRep::Integer(pet_a),
            PyRep::Integer(2),
            PyRep::String("Requires database engineer investigation".into()),
        ],
    )
    .await
    .expect("Escalate Alpha");
    let PyRep::KeyVal(esc_kv) = escalated else { panic!("expected KeyVal"); };
    assert_eq!(esc_kv.get("status").and_then(|v| v.as_str()), Some("escalated"));

    // Alpha is no longer claimed by GM
    let claimed_after_esc = rpc(&ctx, &mut gm, "GetClaimedPetitions", &[]).await.expect("GetClaimedPetitions after esc");
    let PyRep::List(claimed_after_rows) = claimed_after_esc else { panic!("expected List"); };
    assert!(!claimed_after_rows.iter().any(|p| matches!(p, PyRep::KeyVal(kv) if kv.get("petitionID") == Some(&PyRep::Integer(pet_a)))));

    // Alpha is now in queue 2, not queue 1
    let q1_after_esc = rpc(&ctx, &mut gm, "GetPetitionQueue", &[PyRep::Integer(1)]).await.expect("GetPetitionQueue 1");
    let PyRep::List(q1_after_list) = q1_after_esc else { panic!("expected List"); };
    assert!(!q1_after_list.iter().any(|p| matches!(p, PyRep::KeyVal(kv) if kv.get("petitionID") == Some(&PyRep::Integer(pet_a)))));

    let q2_after_esc = rpc(&ctx, &mut gm, "GetPetitionQueue", &[PyRep::Integer(2)]).await.expect("GetPetitionQueue 2");
    let PyRep::List(q2_after_list) = q2_after_esc else { panic!("expected List"); };
    assert!(q2_after_list.iter().any(|p| matches!(p, PyRep::KeyVal(kv) if kv.get("petitionID") == Some(&PyRep::Integer(pet_a)) && kv.get("escalated") == Some(&PyRep::Bool(true)))));

    // 7. UnClaimPetition: claim Beta then unclaim, returning it to open status
    rpc(&ctx, &mut gm, "ClaimPetition", &[PyRep::Integer(pet_b)]).await.expect("Claim Beta");
    let unclaim = rpc(&ctx, &mut gm, "UnClaimPetition", &[PyRep::Integer(pet_b)]).await.expect("UnClaim Beta");
    let PyRep::KeyVal(unclaim_kv) = unclaim else { panic!("expected KeyVal"); };
    assert_eq!(unclaim_kv.get("status").and_then(|v| v.as_str()), Some("open"));

    // 8. CancelPetition: other pilot cannot cancel; petitioner cancels Beta
    let unauth_cancel = rpc(&ctx, &mut p2, "CancelPetition", &[PyRep::Integer(pet_b)]).await;
    assert_eq!(unauth_cancel.unwrap_err().code, "InsufficientRole");

    let cancel_res = rpc(&ctx, &mut p1, "CancelPetition", &[PyRep::Integer(pet_b)]).await.expect("Cancel Beta");
    assert_eq!(cancel_res, PyRep::Bool(true));

    // Cancelled petition is excluded from active queue 1
    let q1_after_cancel = rpc(&ctx, &mut gm, "GetPetitionQueue", &[PyRep::Integer(1)]).await.expect("GetPetitionQueue 1");
    let PyRep::List(q1_cancelled_list) = q1_after_cancel else { panic!("expected List"); };
    assert!(!q1_cancelled_list.iter().any(|p| matches!(p, PyRep::KeyVal(kv) if kv.get("petitionID") == Some(&PyRep::Integer(pet_b)))));

    // 9. DeletePetition: petitioner deletes Alpha, removing it from queue 2
    let delete_res = rpc(&ctx, &mut p1, "DeletePetition", &[PyRep::Integer(pet_a)]).await.expect("Delete Alpha");
    assert_eq!(delete_res, PyRep::Bool(true));

    let q2_after_del = rpc(&ctx, &mut gm, "GetPetitionQueue", &[PyRep::Integer(2)]).await.expect("GetPetitionQueue 2");
    let PyRep::List(q2_del_list) = q2_after_del else { panic!("expected List"); };
    assert!(!q2_del_list.iter().any(|p| matches!(p, PyRep::KeyVal(kv) if kv.get("petitionID") == Some(&PyRep::Integer(pet_a)))));
}
