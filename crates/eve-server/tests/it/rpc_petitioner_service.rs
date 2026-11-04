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
    assert!(matches!(may, PyRep::Integer(code) if code > 0));

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
    assert_eq!(rate_kv.get("responseTimeRating").and_then(|v| v.as_i64()), Some(9));
    assert_eq!(rate_kv.get("helpfulnessRating").and_then(|v| v.as_i64()), Some(10));
    assert_eq!(rate_kv.get("attitudeRating").and_then(|v| v.as_i64()), Some(10));
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
    assert_eq!(u_rate_kv.get("responseTimeRating").and_then(|v| v.as_i64()), Some(10));
    assert_eq!(u_rate_kv.get("helpfulnessRating").and_then(|v| v.as_i64()), Some(10));
    assert_eq!(u_rate_kv.get("attitudeRating").and_then(|v| v.as_i64()), Some(10));
    assert_eq!(
        u_rate_kv.get("comment").and_then(|v| v.as_str()),
        Some("Updated rating: exemplary support!")
    );

    // 10. CancelPetition on closed petition fails with PetitionClosed
    let cancel_closed = rpc(&ctx, &mut p1, "CancelPetition", &[PyRep::Integer(pet_id)]).await;
    assert_eq!(cancel_closed.unwrap_err().code, "PetitionClosed");
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
    assert_eq!(claim_res, PyRep::Integer(1));
    assert!(claim_res.as_i64() > Some(0));

    // Claiming already claimed petition by another GM fails
    ctx.db.provision_gm_account("gm_other", "gm-pass", 61_007, "GM Other");
    let mut gm2 = login_char(&ctx, 61_008, "gm_other", "gm-pass", 61_007).await;
    let claim_conflict = rpc(&ctx, &mut gm2, "ClaimPetition", &[PyRep::Integer(pet_a)]).await;
    assert_eq!(claim_conflict.unwrap_err().code, "PetitionAlreadyClaimed");

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
    let claim_b = rpc(&ctx, &mut gm, "ClaimPetition", &[PyRep::Integer(pet_b)]).await.expect("Claim Beta");
    assert_eq!(claim_b, PyRep::Integer(1));
    let unclaim = rpc(&ctx, &mut gm, "UnClaimPetition", &[PyRep::Integer(pet_b)]).await.expect("UnClaim Beta");
    let PyRep::KeyVal(unclaim_kv) = unclaim else { panic!("expected KeyVal"); };
    assert_eq!(unclaim_kv.get("status").and_then(|v| v.as_str()), Some("open"));

    // 8. CancelPetition: other pilot cannot cancel; petitioner cancels Beta
    let unauth_cancel = rpc(&ctx, &mut p2, "CancelPetition", &[PyRep::Integer(pet_b)]).await;
    assert_eq!(unauth_cancel.unwrap_err().code, "InsufficientRole");

    let cancel_res = rpc(&ctx, &mut p1, "CancelPetition", &[PyRep::Integer(pet_b)]).await.expect("Cancel Beta");
    assert_eq!(cancel_res, PyRep::Bool(true));

    // Claiming cancelled petition fails with PetitionClosed
    let claim_cancelled = rpc(&ctx, &mut gm, "ClaimPetition", &[PyRep::Integer(pet_b)]).await;
    assert_eq!(claim_cancelled.unwrap_err().code, "PetitionClosed");

    // Cancelled petition is excluded from active queue 1
    let q1_after_cancel = rpc(&ctx, &mut gm, "GetPetitionQueue", &[PyRep::Integer(1)]).await.expect("GetPetitionQueue 1");
    let PyRep::List(q1_cancelled_list) = q1_after_cancel else { panic!("expected List"); };
    assert!(!q1_cancelled_list.iter().any(|p| matches!(p, PyRep::KeyVal(kv) if kv.get("petitionID") == Some(&PyRep::Integer(pet_b)))));

    // 9. DeletePetition: deleting active petition (Alpha) is rejected with PetitionNotClosed
    let del_active_err = rpc(&ctx, &mut p1, "DeletePetition", &[PyRep::Integer(pet_a)]).await;
    assert_eq!(del_active_err.unwrap_err().code, "PetitionNotClosed");

    // Delete cancelled Beta succeeds and removes it from GetMyPetitionsEx
    let delete_res = rpc(&ctx, &mut p1, "DeletePetition", &[PyRep::Integer(pet_b)]).await.expect("Delete Beta");
    assert_eq!(delete_res, PyRep::Bool(true));

    let PyRep::List(p1_pets) = rpc(&ctx, &mut p1, "GetMyPetitionsEx", &[]).await.expect("GetMyPetitionsEx") else { panic!("expected List"); };
    assert!(!p1_pets.iter().any(|p| matches!(p, PyRep::KeyVal(kv) if kv.get("petitionID") == Some(&PyRep::Integer(pet_b)))));
}


#[allow(unused_imports, dead_code, unused_variables, unused_mut)]
mod rpc_petitioner_service_ext_b {
    use super::*;

// Dedicated integration tests for Petitioner Services (`petitioner`) - Part 2.
//
// Validates client contract parity for petition messages, activity logs,
// unread notifications, category hierarchies, client picker info, and Zendesk SSO.

use crate::common;
use eve_db::InventoryFlag;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::HashMap;

const PILOT_1: i64 = 90_000_001;
const PILOT_2: i64 = 90_000_002;
const GM_PILOT: i64 = 90_000_003;
const JITA_STATION_ID: i64 = 60003760;

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

fn seed_test_item(ctx: &ServerContext, type_id: u32, owner_id: i64, qty: i32) -> i64 {
    ctx.db.with_mut_snapshot(|snap| {
        snap.next_entity_id += 1;
        let id = snap.next_entity_id;
        snap.items.insert(
            id,
            eve_db::InventoryItemRecord {
                item_id: id,
                type_id,
                owner_id,
                location_id: JITA_STATION_ID,
                flag: InventoryFlag::Hangar,
                quantity: qty,
                singleton: false,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        id
    })
}

#[tokio::test]
async fn test_petitioner_messages_logs_and_unread() {
    // parity: rpc:petitioner.GetMyPetitionsEx, rpc:petitioner.GetPetitionMessages, rpc:petitioner.GetUnreadMessages
    // parity: rpc:petitioner.MarkAsRead, rpc:petitioner.GetLog, rpc:petitioner.GetCategories
    // parity: rpc:petitioner.GetCategoryHierarchicalInfo
    let ctx = ServerContext::new();
    let mut p1 = login_char(&ctx, 62_001, "demo", "demo-secret", PILOT_1).await;
    let mut p2 = login_char(&ctx, 62_002, "other", "other-secret", PILOT_2).await;

    ctx.db.provision_gm_account("gm_msg_admin", "gm-pass", GM_PILOT, "GM Communications");
    let mut gm = login_char(&ctx, 62_003, "gm_msg_admin", "gm-pass", GM_PILOT).await;

    // 1. GetCategories returns canonical category list with categoryID and categoryName
    let cats = rpc(&ctx, &mut p1, "GetCategories", &[]).await.expect("GetCategories");
    let PyRep::List(cat_list) = cats else { panic!("expected List from GetCategories"); };
    assert!(cat_list.iter().any(|c| matches!(c, PyRep::KeyVal(kv) if kv.get("categoryID") == Some(&PyRep::Integer(1)) && kv.get("categoryName") == Some(&PyRep::String("Game Play Support".into())))));
    assert!(cat_list.iter().any(|c| matches!(c, PyRep::KeyVal(kv) if kv.get("categoryID") == Some(&PyRep::Integer(2)) && kv.get("categoryName") == Some(&PyRep::String("Billing & Account".into())))));
    assert!(cat_list.iter().any(|c| matches!(c, PyRep::KeyVal(kv) if kv.get("categoryID") == Some(&PyRep::Integer(102)) && kv.get("categoryName") == Some(&PyRep::String("Reimbursement".into())))));

    // 2. GetCategoryHierarchicalInfo returns 4-tuple: (parentDict, childDict, descDict, billingList)
    let hier = rpc(&ctx, &mut p1, "GetCategoryHierarchicalInfo", &[]).await.expect("GetCategoryHierarchicalInfo");
    let PyRep::Tuple(hier_tuple) = hier else { panic!("expected 4-tuple"); };
    assert_eq!(hier_tuple.len(), 4);
    let PyRep::Dict(parent_dict) = &hier_tuple[0] else { panic!("expected parentDict"); };
    let PyRep::Dict(child_dict) = &hier_tuple[1] else { panic!("expected childDict"); };
    let PyRep::Dict(desc_dict) = &hier_tuple[2] else { panic!("expected descDict"); };
    let PyRep::List(billing_list) = &hier_tuple[3] else { panic!("expected billingList"); };

    assert_eq!(
        parent_dict.iter().find(|(k, _)| k == &PyRep::Integer(1)).map(|(_, v)| v),
        Some(&PyRep::Tuple(vec![PyRep::String("Game Play Support".into()), PyRep::String("EN".into())]))
    );
    let PyRep::Dict(cat1_children) = child_dict.iter().find(|(k, _)| k == &PyRep::Integer(1)).map(|(_, v)| v).expect("cat 1 children") else { panic!("expected child dict"); };
    assert!(cat1_children.iter().any(|(k, v)| k == &PyRep::Integer(101) && v == &PyRep::Tuple(vec![PyRep::String("Missions & Exploration".into()), PyRep::String("EN".into())])));
    assert!(desc_dict.iter().any(|(k, v)| k == &PyRep::Integer(102) && v.as_str() == Some("Ship and module loss reimbursement requests")));
    assert!(billing_list.contains(&PyRep::Integer(2)) && billing_list.contains(&PyRep::Integer(201)));

    // 3. Create petition for pilot 1
    let pet_res = rpc(
        &ctx,
        &mut p1,
        "CreatePetition",
        &[
            PyRep::String("Reimbursement inquiry".into()),
            PyRep::String("Lost mining barge due to node crash".into()),
            PyRep::Integer(102),
        ],
    )
    .await
    .expect("CreatePetition");
    let pet_id = pet_res.dict_get("petitionID").and_then(|v| v.as_i64()).unwrap();

    // 4. GetMyPetitionsEx returns full list for owner, empty for other pilot
    let p1_pets = rpc(&ctx, &mut p1, "GetMyPetitionsEx", &[]).await.expect("GetMyPetitionsEx P1");
    let PyRep::List(p1_list) = p1_pets else { panic!("expected List"); };
    assert!(p1_list.iter().any(|r| matches!(r, PyRep::KeyVal(kv) if kv.get("petitionID") == Some(&PyRep::Integer(pet_id)) && kv.get("subject") == Some(&PyRep::String("Reimbursement inquiry".into())) && kv.get("status") == Some(&PyRep::String("open".into())))));

    let p2_pets = rpc(&ctx, &mut p2, "GetMyPetitionsEx", &[]).await.expect("GetMyPetitionsEx P2");
    assert_eq!(p2_pets, PyRep::List(Vec::new()));

    // 5. GM replies to petition via PetitioneeChat, setting unread = true
    let gm_msg_text = "Checking cluster killmail records and node timestamp logs.";
    rpc(
        &ctx,
        &mut gm,
        "PetitioneeChat",
        &[PyRep::Integer(pet_id), PyRep::String(gm_msg_text.into())],
    )
    .await
    .expect("PetitioneeChat");

    // 6. GetUnreadMessages returns unread petition message for petitioner
    let unread_res = rpc(&ctx, &mut p1, "GetUnreadMessages", &[]).await.expect("GetUnreadMessages");
    let PyRep::List(unread_list) = unread_res else { panic!("expected List"); };
    assert_eq!(unread_list.len(), 1);
    let PyRep::KeyVal(u_kv) = &unread_list[0] else { panic!("expected KeyVal"); };
    assert_eq!(u_kv.get("petitionID").and_then(|v| v.as_i64()), Some(pet_id));
    assert_eq!(u_kv.get("text").and_then(|v| v.as_str()), Some(gm_msg_text));
    assert!(u_kv.get("messageID").and_then(|v| v.as_i64()).unwrap_or(0) > 0);

    // 7. GetPetitionMessages: pilot 2 rejected, pilot 1 retrieves messages with senderID
    let unauth_msgs = rpc(&ctx, &mut p2, "GetPetitionMessages", &[PyRep::Integer(pet_id)]).await;
    assert_eq!(unauth_msgs.unwrap_err().code, "AccessDenied");

    let p1_msgs = rpc(&ctx, &mut p1, "GetPetitionMessages", &[PyRep::Integer(pet_id)]).await.expect("GetPetitionMessages");
    let PyRep::List(msg_items) = p1_msgs else { panic!("expected List"); };
    assert_eq!(msg_items.len(), 2); // Initial description + GM reply
    assert!(msg_items.iter().any(|m| matches!(m, PyRep::KeyVal(kv) if kv.get("senderID") == Some(&PyRep::Integer(PILOT_1)))));
    assert!(msg_items.iter().any(|m| matches!(m, PyRep::KeyVal(kv) if kv.get("senderID") == Some(&PyRep::Integer(GM_PILOT)) && kv.get("text") == Some(&PyRep::String(gm_msg_text.into())))));

    // 8. GetLog: verifies exact client attributes (characterID, eventID, logDateTime, userName)
    // as well as backward-compatible attributes (event, actorID, timestamp)
    let unauth_log = rpc(&ctx, &mut p2, "GetLog", &[PyRep::Integer(pet_id)]).await;
    assert_eq!(unauth_log.unwrap_err().code, "AccessDenied");

    let p1_log = rpc(&ctx, &mut p1, "GetLog", &[PyRep::Integer(pet_id)]).await.expect("GetLog");
    let PyRep::List(log_entries) = p1_log else { panic!("expected List"); };
    assert!(log_entries.len() >= 2);
    for entry in &log_entries {
        let PyRep::KeyVal(lkv) = entry else { panic!("expected KeyVal log entry"); };
        assert!(lkv.contains_key("characterID"));
        assert!(lkv.contains_key("eventID"));
        assert!(lkv.contains_key("logDateTime"));
        assert!(lkv.contains_key("userName"));
        assert!(lkv.contains_key("event"));
        assert!(lkv.contains_key("eventText"));
        assert!(lkv.contains_key("actorID"));
        assert!(lkv.contains_key("timestamp"));
    }
    assert!(log_entries.iter().any(|e| matches!(e, PyRep::KeyVal(kv) if kv.get("eventID") == Some(&PyRep::Integer(1)) && kv.get("event") == Some(&PyRep::String("Created".into())))));
    assert!(log_entries.iter().any(|e| matches!(e, PyRep::KeyVal(kv) if kv.get("eventID") == Some(&PyRep::Integer(6)) && kv.get("eventText").and_then(|v| v.as_str()).map_or(false, |s| s.starts_with("GMComment:")))));

    // 9. MarkAsRead: other pilot rejected; petitioner marks read, clearing unread messages
    let unauth_mark = rpc(&ctx, &mut p2, "MarkAsRead", &[PyRep::Integer(pet_id)]).await;
    assert_eq!(unauth_mark.unwrap_err().code, "AccessDenied");

    let mark_res = rpc(&ctx, &mut p1, "MarkAsRead", &[PyRep::Integer(pet_id)]).await.expect("MarkAsRead");
    assert_eq!(mark_res, PyRep::Bool(true));

    let unread_cleared = rpc(&ctx, &mut p1, "GetUnreadMessages", &[]).await.expect("GetUnreadMessages cleared");
    assert_eq!(unread_cleared, PyRep::List(Vec::new()));

    // Petition status reverted from Responded to open
    let p1_pets_after = rpc(&ctx, &mut p1, "GetMyPetitionsEx", &[]).await.expect("GetMyPetitionsEx after read");
    let PyRep::List(after_list) = p1_pets_after else { panic!("expected List"); };
    let status = after_list.iter().find_map(|r| match r {
        PyRep::KeyVal(kv) if kv.get("petitionID") == Some(&PyRep::Integer(pet_id)) => kv.get("status").and_then(|v| v.as_str()),
        _ => None,
    });
    assert_eq!(status, Some("open"));
}

#[tokio::test]
async fn test_petitioner_client_config_and_zendesk() {
    // parity: rpc:petitioner.GetCategoryProperties, rpc:petitioner.GetClientPickerInfo, rpc:petitioner.PropertyPopulationInfo
    // parity: rpc:petitioner.GetUserCatalogCountry, rpc:petitioner.GetZendeskJwtLink, rpc:petitioner.IsSerenity
    // parity: rpc:petitioner.IsZendeskEnabled, rpc:petitioner.IsZendeskSwapEnabled
    let ctx = ServerContext::new();
    let mut p1 = login_char(&ctx, 62_004, "demo", "demo-secret", PILOT_1).await;

    // 1. GetCategoryProperties returns property descriptors matching client expectations
    let cat_props = rpc(&ctx, &mut p1, "GetCategoryProperties", &[PyRep::Integer(1)]).await.expect("GetCategoryProperties");
    let PyRep::List(props) = cat_props else { panic!("expected List"); };
    assert!(!props.is_empty(), "Properties list should not be empty");
    assert!(props.iter().any(|p| p.dict_get("propertyName").and_then(|v| v.as_str()) == Some("SolarSystem")));
    assert!(props.iter().any(|p| p.dict_get("propertyName").and_then(|v| v.as_str()) == Some("Character")));

    // 2. GetClientPickerInfo: returns (label, item_id) tuples for owned items
    let seed_id = seed_test_item(&ctx, 587, PILOT_1, 1);
    let picker_all = rpc(&ctx, &mut p1, "GetClientPickerInfo", &[]).await.expect("GetClientPickerInfo all");
    let PyRep::List(picker_items) = picker_all else { panic!("expected List"); };
    assert!(picker_items.iter().any(|it| matches!(it, PyRep::Tuple(t) if t.len() == 2 && t[1] == PyRep::Integer(seed_id))));

    // Filter by item name
    let picker_filtered = rpc(
        &ctx,
        &mut p1,
        "GetClientPickerInfo",
        &[PyRep::String("Rifter".into())],
    )
    .await
    .expect("GetClientPickerInfo filtered");
    let PyRep::List(filtered_items) = picker_filtered else { panic!("expected List"); };
    assert!(filtered_items.iter().any(|it| matches!(it, PyRep::Tuple(t) if t.len() == 2 && t[1] == PyRep::Integer(seed_id))));

    // 3. PropertyPopulationInfo: FormatForDropDown list of (label, id) tuples
    // Test SolarSystem property
    let pop_sys = rpc(
        &ctx,
        &mut p1,
        "PropertyPopulationInfo",
        &[PyRep::Integer(1), PyRep::String("SolarSystem".into())],
    )
    .await
    .expect("PropertyPopulationInfo SolarSystem");
    let PyRep::List(sys_opts) = pop_sys else { panic!("expected List"); };
    assert!(sys_opts.iter().any(|opt| matches!(opt, PyRep::Tuple(t) if t.len() == 2 && t[0].as_str() == Some("Jita") && t[1] == PyRep::Integer(30_000_142))));

    // Test Character property
    let pop_char = rpc(
        &ctx,
        &mut p1,
        "PropertyPopulationInfo",
        &[PyRep::Integer(1), PyRep::String("Character".into())],
    )
    .await
    .expect("PropertyPopulationInfo Character");
    let PyRep::List(char_opts) = pop_char else { panic!("expected List"); };
    assert!(char_opts.iter().any(|opt| matches!(opt, PyRep::Tuple(t) if t.len() == 2 && t[1] == PyRep::Integer(PILOT_1))));

    // 4. GetUserCatalogCountry: default US, updates dynamically when account country changed
    let country_default = rpc(&ctx, &mut p1, "GetUserCatalogCountry", &[]).await.expect("GetUserCatalogCountry default");
    assert_eq!(country_default, PyRep::String("US".into()));

    let uid = p1.user_id.expect("user_id");
    eve_server::services::account_services::set_user_country(&ctx, uid, "DE");
    let country_updated = rpc(&ctx, &mut p1, "GetUserCatalogCountry", &[]).await.expect("GetUserCatalogCountry DE");
    assert_eq!(country_updated, PyRep::String("DE".into()));

    // 5. IsZendeskEnabled & GetZendeskJwtLink
    let z_enabled = rpc(&ctx, &mut p1, "IsZendeskEnabled", &[]).await.expect("IsZendeskEnabled");
    assert_eq!(z_enabled, PyRep::Bool(true));

    let z_jwt = rpc(&ctx, &mut p1, "GetZendeskJwtLink", &[]).await.expect("GetZendeskJwtLink");
    let Some(jwt_str) = z_jwt.as_str() else { panic!("expected JWT link string"); };
    assert!(jwt_str.starts_with("https://support.eveonline.com/access/jwt?jwt="));
    assert!(jwt_str.contains("cid_90000001"));

    // 6. Cluster flags: IsSerenity & IsZendeskSwapEnabled
    let serenity = rpc(&ctx, &mut p1, "IsSerenity", &[]).await.expect("IsSerenity");
    assert_eq!(serenity, PyRep::Bool(false));

    let z_swap = rpc(&ctx, &mut p1, "IsZendeskSwapEnabled", &[]).await.expect("IsZendeskSwapEnabled");
    assert_eq!(z_swap, PyRep::Bool(false));
}

#[tokio::test]
async fn test_r161_petitioner_remediations() {
    // parity: rpc:petitioner.UnClaimPetition, rpc:petitioner.EscalatePetition, rpc:petitioner.ClosePetition
    // parity: rpc:petitioner.MarkAsRead, rpc:petitioner.GetLog
    let ctx = ServerContext::new();
    let mut p1 = login_char(&ctx, 63_001, "demo", "demo-secret", PILOT_1).await;
    ctx.db.provision_gm_account("gm_p1_fix", "gm-pass", GM_PILOT, "GM Pet Remediation");
    let mut gm = login_char(&ctx, 63_002, "gm_p1_fix", "gm-pass", GM_PILOT).await;

    // --- R161-P1-1: UnClaim on unclaimed open petition rejected with PetitionNotClaimed ---
    let pet_res = rpc(
        &ctx,
        &mut p1,
        "CreatePetition",
        &[
            PyRep::String("P1 test ticket".into()),
            PyRep::String("Testing P1 guards".into()),
            PyRep::Integer(1),
        ],
    )
    .await
    .expect("CreatePetition");
    let pet_id = pet_res.dict_get("petitionID").and_then(|v| v.as_i64()).unwrap();

    let unclaim_unclaimed = rpc(&ctx, &mut gm, "UnClaimPetition", &[PyRep::Integer(pet_id)]).await;
    assert_eq!(unclaim_unclaimed.unwrap_err().code, "PetitionNotClaimed");

    // --- R161-P1-2: MarkAsRead preserves "claimed" status if claimed_by is Some ---
    // Claim petition by GM
    rpc(&ctx, &mut gm, "ClaimPetition", &[PyRep::Integer(pet_id)]).await.expect("ClaimPetition");
    // GM sends chat message -> status becomes "Responded"
    rpc(
        &ctx,
        &mut gm,
        "PetitioneeChat",
        &[PyRep::Integer(pet_id), PyRep::String("GM answering".into())],
    )
    .await
    .expect("PetitioneeChat");

    // Pilot marks petition as read
    let mark_claimed = rpc(&ctx, &mut p1, "MarkAsRead", &[PyRep::Integer(pet_id)]).await.expect("MarkAsRead claimed");
    assert_eq!(mark_claimed, PyRep::Bool(true));

    // Verify petition status is "claimed", NOT "open"
    let p1_pets = rpc(&ctx, &mut p1, "GetMyPetitionsEx", &[]).await.expect("GetMyPetitionsEx");
    let PyRep::List(p1_list) = p1_pets else { panic!("expected List"); };
    let status = p1_list.iter().find_map(|r| match r {
        PyRep::KeyVal(kv) if kv.get("petitionID") == Some(&PyRep::Integer(pet_id)) => kv.get("status").and_then(|v| v.as_str()),
        _ => None,
    });
    assert_eq!(status, Some("claimed"));

    // --- R161-P1-3: GetLog resolves character names from snapshot ---
    let logs_rep = rpc(&ctx, &mut p1, "GetLog", &[PyRep::Integer(pet_id)]).await.expect("GetLog");
    let PyRep::List(logs) = logs_rep else { panic!("expected List"); };
    // Find creation log (actor = PILOT_1 -> "Aura Capsuleer")
    let p1_log = logs.iter().find(|e| matches!(e, PyRep::KeyVal(kv) if kv.get("actorID") == Some(&PyRep::Integer(PILOT_1)))).expect("pilot log entry");
    let PyRep::KeyVal(p1_kv) = p1_log else { panic!("KeyVal"); };
    assert_eq!(p1_kv.get("userName").and_then(|v| v.as_str()), Some("Aura Capsuleer"));

    // Find GM chat log (actor = GM_PILOT -> "GM Pet Remediation")
    let gm_log = logs.iter().find(|e| matches!(e, PyRep::KeyVal(kv) if kv.get("actorID") == Some(&PyRep::Integer(GM_PILOT)))).expect("gm log entry");
    let PyRep::KeyVal(gm_kv) = gm_log else { panic!("KeyVal"); };
    assert_eq!(gm_kv.get("userName").and_then(|v| v.as_str()), Some("GM Pet Remediation"));

    // --- R161-P1-1: Terminal-state guards on UnClaim, Escalate, and Close ---
    // Close the petition
    rpc(&ctx, &mut gm, "ClosePetition", &[PyRep::Integer(pet_id), PyRep::String("Issue resolved".into())]).await.expect("ClosePetition");

    // ClosePetition on already closed petition fails with PetitionClosed
    let close_again = rpc(&ctx, &mut gm, "ClosePetition", &[PyRep::Integer(pet_id)]).await;
    assert_eq!(close_again.unwrap_err().code, "PetitionClosed");

    // EscalatePetition on closed petition fails with PetitionClosed
    let esc_closed = rpc(&ctx, &mut gm, "EscalatePetition", &[PyRep::Integer(pet_id)]).await;
    assert_eq!(esc_closed.unwrap_err().code, "PetitionClosed");

    // UnClaimPetition on closed petition fails with PetitionClosed
    let unclaim_closed = rpc(&ctx, &mut gm, "UnClaimPetition", &[PyRep::Integer(pet_id)]).await;
    assert_eq!(unclaim_closed.unwrap_err().code, "PetitionClosed");

    // Create another petition and cancel it to test cancelled terminal state
    let pet_c_res = rpc(
        &ctx,
        &mut p1,
        "CreatePetition",
        &[
            PyRep::String("Cancelled ticket".into()),
            PyRep::String("To be cancelled".into()),
            PyRep::Integer(1),
        ],
    )
    .await
    .expect("Create cancelled");
    let pet_c_id = pet_c_res.dict_get("petitionID").and_then(|v| v.as_i64()).unwrap();
    rpc(&ctx, &mut p1, "CancelPetition", &[PyRep::Integer(pet_c_id)]).await.expect("CancelPetition");

    // ClosePetition on cancelled petition fails with PetitionClosed
    let close_cancelled = rpc(&ctx, &mut gm, "ClosePetition", &[PyRep::Integer(pet_c_id)]).await;
    assert_eq!(close_cancelled.unwrap_err().code, "PetitionClosed");

    // EscalatePetition on cancelled petition fails with PetitionClosed
    let esc_cancelled = rpc(&ctx, &mut gm, "EscalatePetition", &[PyRep::Integer(pet_c_id)]).await;
    assert_eq!(esc_cancelled.unwrap_err().code, "PetitionClosed");

    // UnClaimPetition on cancelled petition fails with PetitionClosed
    let unclaim_cancelled = rpc(&ctx, &mut gm, "UnClaimPetition", &[PyRep::Integer(pet_c_id)]).await;
    assert_eq!(unclaim_cancelled.unwrap_err().code, "PetitionClosed");
}

}
