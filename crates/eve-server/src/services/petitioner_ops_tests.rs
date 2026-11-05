use super::*;
use crate::services::account_services::account_services_tests::{auth_session, test_ctx};
use std::collections::HashMap;

fn gm_session(user_id: u64, char_id: i64) -> SessionState {
    let mut s = auth_session(user_id, char_id, 98_000_001, 30_000_142);
    s.role |= eve_machonet::service_roles::ROLE_GML | eve_machonet::service_roles::ROLE_GMH;
    s
}

#[test]
fn test_petitioner_lifecycle_chat_queues_and_ratings() {
    let ctx = test_ctx();
    let mut player = auth_session(1, 90_000_001, 98_000_001, 30_000_142);
    let mut gm = gm_session(2, 90_000_002);
    let mut other = auth_session(3, 90_000_003, 98_000_001, 30_000_142);
    let kw = HashMap::new();

    let unknown_method = "UnknownMethod";
    assert!(handle(&ctx, &mut player, "other", "CreatePetition", &[], &kw).is_none());
    assert!(handle(&ctx, &mut player, "petitioner", unknown_method, &[], &kw).is_none());
    assert_eq!(default_petition_queue_id(), 1);

    // Categories, hierarchy, properties, picker, country, zendesk flags
    let cats = handle(&ctx, &mut player, "petitioner", "GetCategories", &[], &kw)
        .unwrap()
        .unwrap();
    if let PyRep::List(items) = cats {
        assert_eq!(items.len(), CANONICAL_PETITION_CATEGORIES.len());
    } else {
        panic!("expected List");
    }
    let hier = handle(&ctx, &mut player, "petitioner", "GetCategoryHierarchicalInfo", &[], &kw)
        .unwrap()
        .unwrap();
    assert!(matches!(hier, PyRep::Tuple(ref t) if t.len() == 4));

    for cid in [4, 401, 102, 101, 2, 201, 202, 3] {
        let props = handle(
            &ctx,
            &mut player,
            "petitioner",
            "GetCategoryProperties",
            &[PyRep::Integer(cid)],
            &kw,
        )
        .unwrap()
        .unwrap();
        assert!(matches!(props, PyRep::List(ref l) if !l.is_empty()));
    }
    assert_eq!(
        handle(&ctx, &mut player, "petitioner", "GetCategoryProperties", &[], &kw)
            .unwrap()
            .unwrap(),
        PyRep::List(vec![])
    );

    let picker = handle(
        &ctx,
        &mut player,
        "petitioner",
        "GetClientPickerInfo",
        &[PyRep::String("".into())],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert!(matches!(picker, PyRep::List(_)));

    assert_eq!(
        handle(&ctx, &mut player, "petitioner", "GetUserCatalogCountry", &[], &kw)
            .unwrap()
            .unwrap(),
        PyRep::String("US".into())
    );
    assert_eq!(
        handle(&ctx, &mut player, "petitioner", "IsSerenity", &[], &kw).unwrap().unwrap(),
        PyRep::Bool(false)
    );
    assert_eq!(
        handle(&ctx, &mut player, "petitioner", "IsZendeskEnabled", &[], &kw).unwrap().unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(
        handle(&ctx, &mut player, "petitioner", "IsZendeskSwapEnabled", &[], &kw)
            .unwrap()
            .unwrap(),
        PyRep::Bool(false)
    );
    let jwt = handle(
        &ctx,
        &mut player,
        "petitioner",
        "GetZendeskJwtLink",
        &[PyRep::String("hc/en-us".into())],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert!(jwt.as_str().unwrap_or("").contains("https://support.eveonline.com/access/jwt"));

    for arg in [
        PyRep::String("SolarSystem".into()),
        PyRep::String("Character".into()),
        PyRep::String("ShipType".into()),
    ] {
        let pop = handle(&ctx, &mut player, "petitioner", "PropertyPopulationInfo", &[arg], &kw)
            .unwrap()
            .unwrap();
        assert!(matches!(pop, PyRep::List(_)));
    }

    // MayPetition checks
    let mut unauth = SessionState::default();
    assert_eq!(
        handle(&ctx, &mut unauth, "petitioner", "MayPetition", &[], &kw).unwrap().unwrap(),
        PyRep::Integer(-6)
    );
    assert_eq!(
        handle(
            &ctx,
            &mut player,
            "petitioner",
            "MayPetition",
            &[PyRep::Integer(999999)],
            &kw
        )
        .unwrap()
        .unwrap(),
        PyRep::Integer(-4)
    );

    // CreatePetition validation & success
    assert!(handle(
        &ctx,
        &mut player,
        "petitioner",
        "CreatePetition",
        &[PyRep::String("".into())],
        &kw
    )
    .unwrap()
    .is_err());
    assert!(handle(
        &ctx,
        &mut player,
        "petitioner",
        "CreatePetition",
        &[PyRep::String("Subj".into()), PyRep::String("Desc".into()), PyRep::Integer(999)],
        &kw
    )
    .unwrap()
    .is_err());

    let created = handle(
        &ctx,
        &mut player,
        "petitioner",
        "CreatePetition",
        &[
            PyRep::String("Stuck ship".into()),
            PyRep::String("Need rescue".into()),
            PyRep::Integer(4),
        ],
        &kw,
    )
    .unwrap()
    .unwrap();
    let pet_id = created.dict_get("petitionID").and_then(|v| v.as_i64()).unwrap();
    assert!(pet_id > 0);

    assert_eq!(
        handle(&ctx, &mut player, "petitioner", "HasOpenTickets", &[], &kw).unwrap().unwrap(),
        PyRep::Bool(true)
    );
    let my_pets = handle(&ctx, &mut player, "petitioner", "GetMyPetitionsEx", &[], &kw)
        .unwrap()
        .unwrap();
    assert!(matches!(my_pets, PyRep::List(ref l) if l.len() == 1));

    // Chat & GM Claim/Escalate/UnClaim/Close/Rating
    assert!(handle(
        &ctx,
        &mut other,
        "petitioner",
        "PetitionerChat",
        &[PyRep::Integer(pet_id), PyRep::String("hijack".into())],
        &kw
    )
    .unwrap()
    .is_err());
    let chat1 = handle(
        &ctx,
        &mut player,
        "petitioner",
        "PetitionerChat",
        &[PyRep::Integer(pet_id), PyRep::String("More details".into())],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(chat1.dict_get("messageCount").and_then(|v| v.as_i64()), Some(2));

    // GM Claim, Queue, Chat
    assert!(handle(&ctx, &mut player, "petitioner", "GetQueues", &[], &kw).unwrap().is_err());
    let queues = handle(&ctx, &mut gm, "petitioner", "GetQueues", &[], &kw).unwrap().unwrap();
    assert!(matches!(queues, PyRep::List(ref l) if l.len() == CANONICAL_PETITION_QUEUES.len()));

    assert_eq!(
        handle(&ctx, &mut gm, "petitioner", "ClaimPetition", &[PyRep::Integer(pet_id)], &kw)
            .unwrap()
            .unwrap(),
        PyRep::Integer(1)
    );
    let claimed = handle(&ctx, &mut gm, "petitioner", "GetClaimedPetitions", &[], &kw)
        .unwrap()
        .unwrap();
    assert!(matches!(claimed, PyRep::List(ref l) if l.len() == 1));

    let chat_gm = handle(
        &ctx,
        &mut gm,
        "petitioner",
        "PetitioneeChat",
        &[PyRep::Integer(pet_id), PyRep::String("Hello pilot".into())],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(chat_gm.dict_get("messageCount").and_then(|v| v.as_i64()), Some(3));

    let unread = handle(&ctx, &mut player, "petitioner", "GetUnreadMessages", &[], &kw)
        .unwrap()
        .unwrap();
    assert!(matches!(unread, PyRep::List(ref l) if l.len() == 1));

    assert_eq!(
        handle(&ctx, &mut player, "petitioner", "MarkAsRead", &[PyRep::Integer(pet_id)], &kw)
            .unwrap()
            .unwrap(),
        PyRep::Bool(true)
    );

    // UnClaim & Escalate & Close
    let uncl = handle(
        &ctx,
        &mut gm,
        "petitioner",
        "UnClaimPetition",
        &[PyRep::Integer(pet_id)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(uncl.dict_get("status").and_then(|v| v.as_str()), Some("open"));

    let esc = handle(
        &ctx,
        &mut gm,
        "petitioner",
        "EscalatePetition",
        &[PyRep::Integer(pet_id), PyRep::Integer(2), PyRep::String("Senior GM".into())],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(esc.dict_get("status").and_then(|v| v.as_str()), Some("escalated"));

    let q2 = handle(
        &ctx,
        &mut gm,
        "petitioner",
        "GetPetitionQueue",
        &[PyRep::Integer(2)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert!(matches!(q2, PyRep::List(ref l) if !l.is_empty()));

    let closed = handle(
        &ctx,
        &mut gm,
        "petitioner",
        "ClosePetition",
        &[PyRep::Integer(pet_id), PyRep::String("Resolved".into())],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(closed.dict_get("status").and_then(|v| v.as_str()), Some("closed"));

    // Messages, Logs, Rating, Delete
    let msgs = handle(
        &ctx,
        &mut player,
        "petitioner",
        "GetPetitionMessages",
        &[PyRep::Integer(pet_id)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert!(matches!(msgs, PyRep::List(ref l) if l.len() == 4));
    let logs = handle(
        &ctx,
        &mut player,
        "petitioner",
        "GetLog",
        &[PyRep::Integer(pet_id)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert!(matches!(logs, PyRep::List(ref l) if l.len() >= 4));

    let rated = handle(
        &ctx,
        &mut player,
        "petitioner",
        "AddPetitionRating",
        &[
            PyRep::Integer(pet_id),
            PyRep::Integer(9),
            PyRep::Integer(8),
            PyRep::Integer(10),
            PyRep::String("Great help".into()),
        ],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(rated.dict_get("rating").and_then(|v| v.as_i64()), Some(9));
    assert_eq!(rated.dict_get("helpfulnessRating").and_then(|v| v.as_i64()), Some(8));
    assert_eq!(rated.dict_get("attitudeRating").and_then(|v| v.as_i64()), Some(10));

    assert_eq!(
        handle(&ctx, &mut player, "petitioner", "DeletePetition", &[PyRep::Integer(pet_id)], &kw)
            .unwrap()
            .unwrap(),
        PyRep::Bool(true)
    );
}
