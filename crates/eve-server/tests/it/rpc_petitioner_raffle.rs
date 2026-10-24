//! Comprehensive integration tests for Petitioner Services (`petitioner`)
//! and HyperNet Relay Services (`raffleProxy`).

use crate::common;
use eve_db::InventoryFlag;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::HashMap;

const P1: i64 = 90_000_001;
const P2: i64 = 90_000_002;
const GM_ID: i64 = 90_000_003;
const JITA_STATION_ID: i64 = 60003760;

async fn select(
    ctx: &ServerContext,
    sid: u64,
    user: &str,
    pass: &str,
    char_id: i64,
) -> SessionState {
    let mut s = SessionState::new(sid);
    common::login_pilot(ctx, &mut s, user, pass).await;
    ctx.dispatch_rpc(
        &mut s,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &HashMap::new(),
        None,
    )
    .await
    .expect("SelectCharacterID");
    s
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

fn seed_item(ctx: &ServerContext, type_id: u32, owner_id: i64, qty: i32) -> i64 {
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
async fn test_petitioner_lifecycle_and_gm_escalation() {
    // parity: rpc:petitioner.ClosePetition, rpc:petitioner.DeletePetition, rpc:petitioner.EscalatePetition, rpc:petitioner.GetPetitionQueue
    // parity: rpc:petitioner.GetClaimedPetitions, rpc:petitioner.GetPetitionMessages, rpc:petitioner.HasOpenTickets
    // parity: rpc:petitioner.MarkAsRead, rpc:petitioner.MayPetition, rpc:petitioner.PetitioneeChat
    // parity: rpc:petitioner.PetitionerChat, rpc:petitioner.UnClaimPetition, rpc:petitioner.UpdatePetitionRating
    let ctx = ServerContext::new();
    let mut p1 = select(&ctx, 60_001, "demo", "demo-secret", P1).await;

    // 1. MayPetition and HasOpenTickets checks
    let may = rpc(&ctx, &mut p1, "petitioner", "MayPetition", &[])
        .await
        .expect("MayPetition");
    assert_eq!(may, PyRep::Bool(true));

    let open_initial = rpc(&ctx, &mut p1, "petitioner", "HasOpenTickets", &[])
        .await
        .expect("HasOpenTickets initial");
    assert_eq!(open_initial, PyRep::Bool(false));

    // Unknown category is rejected
    let bad_cat = rpc(&ctx, &mut p1, "petitioner", "CreatePetition", &[PyRep::String("Bad".into()), PyRep::String("Bad".into()), PyRep::Integer(999)]).await;
    assert_eq!(bad_cat.unwrap_err().code, "InvalidPetitionCategory");

    // 2. Create petition
    let created = rpc(&ctx, &mut p1, "petitioner", "CreatePetition", &[PyRep::String("Stuck in Asteroid Belt".into()), PyRep::String("Ship collided with asteroid and cannot align".into()), PyRep::Integer(1)]).await.expect("CreatePetition");
    let PyRep::KeyVal(ckv) = created else { panic!("expected KeyVal from CreatePetition"); };
    let pet_id = ckv.get("petitionID").and_then(|v| v.as_i64()).expect("petitionID");
    assert!(pet_id > 0);

    let open_after = rpc(&ctx, &mut p1, "petitioner", "HasOpenTickets", &[]).await.expect("HasOpenTickets after create");
    assert_eq!(open_after, PyRep::Bool(true));

    // 3. Petitioner sends follow-up chat message
    let chat1 = rpc(&ctx, &mut p1, "petitioner", "PetitionerChat", &[PyRep::Integer(pet_id), PyRep::String("Also my capacitor is running low".into())]).await.expect("PetitionerChat");
    assert!(matches!(chat1, PyRep::KeyVal(_)));

    // 4. GM logs in, checks queues before and after escalation
    ctx.db.provision_gm_account("gm_pet_admin", "gm-pass", GM_ID, "GM Senior Staff");
    let mut gm = select(&ctx, 60_002, "gm_pet_admin", "gm-pass", GM_ID).await;

    let q1_before = rpc(&ctx, &mut gm, "petitioner", "GetPetitionQueue", &[PyRep::Integer(1)]).await.expect("GetPetitionQueue 1");
    let PyRep::List(q1_list) = q1_before else { panic!("expected list"); };
    assert!(q1_list.iter().any(|p| match p { PyRep::KeyVal(kv) => kv.get("petitionID") == Some(&PyRep::Integer(pet_id)), _ => false }));

    let q2_before = rpc(&ctx, &mut gm, "petitioner", "GetPetitionQueue", &[PyRep::Integer(2)]).await.expect("GetPetitionQueue 2");
    let PyRep::List(q2_list) = q2_before else { panic!("expected list"); };
    assert!(!q2_list.iter().any(|p| match p { PyRep::KeyVal(kv) => kv.get("petitionID") == Some(&PyRep::Integer(pet_id)), _ => false }));

    let chat_gm = rpc(&ctx, &mut gm, "petitioner", "PetitioneeChat", &[PyRep::Integer(pet_id), PyRep::String("Investigating physics mesh right now".into())]).await.expect("PetitioneeChat");
    assert!(matches!(chat_gm, PyRep::KeyVal(_)));

    // 5. Claim before escalation to verify EscalatePetition clears claimed_by and moves queue_id
    rpc(&ctx, &mut gm, "petitioner", "ClaimPetition", &[PyRep::Integer(pet_id)]).await.expect("ClaimPetition before escalate");
    let bad_q = rpc(&ctx, &mut gm, "petitioner", "EscalatePetition", &[PyRep::Integer(pet_id), PyRep::Integer(999)]).await;
    assert_eq!(bad_q.unwrap_err().code, "InvalidPetitionQueue");
    let escalated = rpc(&ctx, &mut gm, "petitioner", "EscalatePetition", &[PyRep::Integer(pet_id), PyRep::Integer(2), PyRep::String("Escalated to senior grid physics team".into())]).await.expect("EscalatePetition");
    let PyRep::KeyVal(ekv) = escalated else { panic!("expected KeyVal from EscalatePetition"); };
    assert_eq!(ekv.get("status").and_then(|v| v.as_str()), Some("escalated"));
    assert_eq!(rpc(&ctx, &mut gm, "petitioner", "GetClaimedPetitions", &[]).await.expect("GetClaimedPetitions cleared"), PyRep::List(Vec::new()));

    let q1_after = rpc(&ctx, &mut gm, "petitioner", "GetPetitionQueue", &[PyRep::Integer(1)]).await.expect("GetPetitionQueue 1 after");
    let PyRep::List(q1_after_list) = q1_after else { panic!("expected list"); };
    assert!(!q1_after_list.iter().any(|p| matches!(p, PyRep::KeyVal(kv) if kv.get("petitionID") == Some(&PyRep::Integer(pet_id)))));

    let q2_after = rpc(&ctx, &mut gm, "petitioner", "GetPetitionQueue", &[PyRep::Integer(2)]).await.expect("GetPetitionQueue 2 after");
    let PyRep::List(q2_after_list) = q2_after else { panic!("expected list"); };
    assert!(q2_after_list.iter().any(|p| matches!(p, PyRep::KeyVal(kv) if kv.get("petitionID") == Some(&PyRep::Integer(pet_id)) && kv.get("claimedBy") == Some(&PyRep::None) && kv.get("escalated") == Some(&PyRep::Bool(true)))));

    // 6. ClaimPetition & GetClaimedPetitions, then 7. UnClaimPetition returns petition to open status
    rpc(&ctx, &mut gm, "petitioner", "ClaimPetition", &[PyRep::Integer(pet_id)]).await.expect("ClaimPetition");
    let PyRep::List(claimed) = rpc(&ctx, &mut gm, "petitioner", "GetClaimedPetitions", &[]).await.expect("GetClaimedPetitions") else { panic!("expected List"); };
    assert!(claimed.iter().any(|c| matches!(c, PyRep::KeyVal(kv) if kv.get("petitionID") == Some(&PyRep::Integer(pet_id)))));
    let PyRep::KeyVal(ukv) = rpc(&ctx, &mut gm, "petitioner", "UnClaimPetition", &[PyRep::Integer(pet_id)]).await.expect("UnClaimPetition") else { panic!("expected KeyVal"); };
    assert_eq!(ukv.get("status").and_then(|v| v.as_str()), Some("open"));

    // 8. Petitioner reads messages and marks as read (verifying unread state transition even after follow-up PetitionerChat)
    rpc(&ctx, &mut p1, "petitioner", "PetitionerChat", &[PyRep::Integer(pet_id), PyRep::String("Thanks, standing by".into())]).await.expect("PetitionerChat after GM");
    let msgs = rpc(&ctx, &mut p1, "petitioner", "GetPetitionMessages", &[PyRep::Integer(pet_id)]).await.expect("GetPetitionMessages");
    let PyRep::List(msg_list) = msgs else { panic!("expected List from GetPetitionMessages"); };
    assert!(msg_list.len() >= 3);

    let unread_before = rpc(&ctx, &mut p1, "petitioner", "GetUnreadMessages", &[]).await.expect("GetUnreadMessages before MarkAsRead");
    let PyRep::List(unread_rows) = unread_before else { panic!("expected List from GetUnreadMessages"); };
    assert_eq!(unread_rows.len(), 1);
    let PyRep::KeyVal(u_kv) = &unread_rows[0] else { panic!("expected KeyVal unread message"); };
    assert_eq!(u_kv.get("petitionID").and_then(|v| v.as_i64()), Some(pet_id));
    assert_eq!(u_kv.get("text").and_then(|v| v.as_str()), Some("Investigating physics mesh right now"));

    let bad_mark = rpc(&ctx, &mut p1, "petitioner", "MarkAsRead", &[PyRep::Integer(999_999)]).await;
    assert_eq!(bad_mark.unwrap_err().code, "PetitionNotFound");

    let mark = rpc(&ctx, &mut p1, "petitioner", "MarkAsRead", &[PyRep::Integer(pet_id)]).await.expect("MarkAsRead");
    assert_eq!(mark, PyRep::Bool(true));

    let unread_after = rpc(&ctx, &mut p1, "petitioner", "GetUnreadMessages", &[]).await.expect("GetUnreadMessages after MarkAsRead");
    assert_eq!(unread_after, PyRep::List(Vec::new()));

    // 9. GM closes the petition and verifies it is excluded from GetPetitionQueue(2)
    let PyRep::KeyVal(cl_kv) = rpc(&ctx, &mut gm, "petitioner", "ClosePetition", &[PyRep::Integer(pet_id), PyRep::String("Ship relocated safely to station".into())]).await.expect("ClosePetition") else { panic!("expected KeyVal"); };
    assert_eq!(cl_kv.get("status").and_then(|v| v.as_str()), Some("closed"));
    assert_eq!(rpc(&ctx, &mut gm, "petitioner", "GetPetitionQueue", &[PyRep::Integer(2)]).await.expect("GetPetitionQueue 2 after close"), PyRep::List(Vec::new()));

    // 10. Petitioner rates closed petition via UpdatePetitionRating
    let PyRep::KeyVal(rkv) = rpc(&ctx, &mut p1, "petitioner", "UpdatePetitionRating", &[PyRep::Integer(pet_id), PyRep::Integer(10), PyRep::String("Fast and helpful rescue!".into())]).await.expect("UpdatePetitionRating") else { panic!("expected KeyVal"); };
    assert_eq!(rkv.get("rating").and_then(|v| v.as_i64()), Some(10));
    let closed_chat_err = rpc(&ctx, &mut gm, "petitioner", "PetitioneeChat", &[PyRep::Integer(pet_id), PyRep::String("Late reply".into())]).await;
    assert_eq!(closed_chat_err.unwrap_err().code, "PetitionClosed");

    // 11. Create a second petition and DeletePetition, verifying exclusion from GetPetitionQueue(1)
    let PyRep::KeyVal(c2_kv) = rpc(&ctx, &mut p1, "petitioner", "CreatePetition", &[PyRep::String("Accidental Petition".into()), PyRep::String("Please disregard".into()), PyRep::Integer(1)]).await.expect("CreatePetition second") else { panic!("expected KeyVal"); };
    let pet2_id = c2_kv.get("petitionID").and_then(|v| v.as_i64()).unwrap();
    assert_eq!(rpc(&ctx, &mut p1, "petitioner", "DeletePetition", &[PyRep::Integer(pet2_id)]).await.expect("DeletePetition"), PyRep::Bool(true));
    assert_eq!(rpc(&ctx, &mut gm, "petitioner", "GetPetitionQueue", &[PyRep::Integer(1)]).await.expect("GetPetitionQueue 1 after cancel"), PyRep::List(Vec::new()));
    let PyRep::List(mine_list) = rpc(&ctx, &mut p1, "petitioner", "GetMyPetitionsEx", &[]).await.expect("GetMyPetitionsEx") else { panic!("expected List"); };
    let cancelled_pet = mine_list.iter().find_map(|r| match r {
        PyRep::KeyVal(kv) if kv.get("petitionID").and_then(|v| v.as_i64()) == Some(pet2_id) => kv.get("status").and_then(|v| v.as_str()),
        _ => None,
    });
    assert_eq!(cancelled_pet, Some("cancelled"));
}

#[tokio::test]
async fn test_petitioner_meta_and_client_configuration() {
    // parity: rpc:petitioner.GetCategories, rpc:petitioner.GetCategoryHierarchicalInfo, rpc:petitioner.GetCategoryProperties
    // parity: rpc:petitioner.GetClientPickerInfo, rpc:petitioner.GetQueues, rpc:petitioner.GetUnreadMessages
    // parity: rpc:petitioner.GetUserCatalogCountry, rpc:petitioner.GetZendeskJwtLink, rpc:petitioner.IsSerenity
    // parity: rpc:petitioner.IsZendeskEnabled, rpc:petitioner.IsZendeskSwapEnabled, rpc:petitioner.PropertyPopulationInfo
    let ctx = ServerContext::new();
    let mut p1 = select(&ctx, 60_003, "demo", "demo-secret", P1).await;

    // 1. GetCategories returns canonical petition category choices with exact IDs and names
    let PyRep::List(cat_list) = rpc(&ctx, &mut p1, "petitioner", "GetCategories", &[]).await.expect("GetCategories") else { panic!("expected List"); };
    assert!(cat_list.iter().any(|c| matches!(c, PyRep::KeyVal(kv) if kv.get("categoryID") == Some(&PyRep::Integer(1)) && kv.get("categoryName") == Some(&PyRep::String("Game Play Support".into())))));
    assert!(cat_list.iter().any(|c| matches!(c, PyRep::KeyVal(kv) if kv.get("categoryID") == Some(&PyRep::Integer(2)) && kv.get("categoryName") == Some(&PyRep::String("Billing & Account".into())))));
    assert!(cat_list.iter().any(|c| matches!(c, PyRep::KeyVal(kv) if kv.get("categoryID") == Some(&PyRep::Integer(102)) && kv.get("categoryName") == Some(&PyRep::String("Reimbursement".into())))));

    // 2. GetCategoryHierarchicalInfo returns 4-tuple (parentDict, childDict, descDict, billingList)
    let hier = rpc(&ctx, &mut p1, "petitioner", "GetCategoryHierarchicalInfo", &[]).await.expect("GetCategoryHierarchicalInfo");
    let PyRep::Tuple(hier_tuple) = hier else { panic!("expected 4-tuple from GetCategoryHierarchicalInfo"); };
    assert_eq!(hier_tuple.len(), 4);
    let PyRep::Dict(parent_dict) = &hier_tuple[0] else { panic!("expected parent Dict"); };
    let PyRep::Dict(child_dict) = &hier_tuple[1] else { panic!("expected child Dict"); };
    let PyRep::Dict(desc_dict) = &hier_tuple[2] else { panic!("expected desc Dict"); };
    let PyRep::List(billing_list) = &hier_tuple[3] else { panic!("expected billing List"); };
    assert_eq!(
        parent_dict.iter().find(|(k, _)| k == &PyRep::Integer(1)).map(|(_, v)| v),
        Some(&PyRep::Tuple(vec![PyRep::String("Game Play Support".into()), PyRep::String("EN".into())]))
    );
    let PyRep::Dict(cat1_children) = child_dict.iter().find(|(k, _)| k == &PyRep::Integer(1)).map(|(_, v)| v).expect("cat 1 children") else { panic!("expected child dict for cat 1"); };
    assert!(cat1_children.iter().any(|(k, v)| k == &PyRep::Integer(101) && v == &PyRep::Tuple(vec![PyRep::String("Missions & Exploration".into()), PyRep::String("EN".into())])));
    assert!(desc_dict.iter().any(|(k, v)| k == &PyRep::Integer(102) && v.as_str() == Some("Ship and module loss reimbursement requests")));
    assert!(billing_list.contains(&PyRep::Integer(2)) && billing_list.contains(&PyRep::Integer(201)));

    // 3. GetCategoryProperties
    let props = rpc(&ctx, &mut p1, "petitioner", "GetCategoryProperties", &[]).await.expect("GetCategoryProperties");
    let PyRep::Dict(prop_dict) = props else { panic!("expected Dict from GetCategoryProperties"); };
    let subcat_req = prop_dict.iter().find(|(k, _)| k.as_str() == Some("isSubcategoryRequired")).map(|(_, v)| v);
    assert_eq!(subcat_req, Some(&PyRep::Bool(false)));

    // 4. GetClientPickerInfo returns (label, item_id) tuples for caller's owned items
    let PyRep::List(picker) = rpc(&ctx, &mut p1, "petitioner", "GetClientPickerInfo", &[]).await.expect("GetClientPickerInfo") else { panic!("expected List"); };
    assert!(picker.iter().any(|p| matches!(p, PyRep::Tuple(t) if t.len() == 2 && t[1] == PyRep::Integer(100_000_001))));

    // 5. GetQueues (non-GM rejected, GM succeeds)
    let non_gm_q = rpc(&ctx, &mut p1, "petitioner", "GetQueues", &[]).await;
    assert_eq!(non_gm_q.unwrap_err().code, "InsufficientRole");
    ctx.db.provision_gm_account("gm_q_tester", "gm-pass", 60_098, "GM Queue");
    let mut gm_q = select(&ctx, 60_098, "gm_q_tester", "gm-pass", 60_098).await;
    let queues = rpc(&ctx, &mut gm_q, "petitioner", "GetQueues", &[]).await.expect("GetQueues");
    let PyRep::List(q_list) = queues else { panic!("expected List from GetQueues"); };
    assert!(q_list.iter().any(|q| matches!(q, PyRep::KeyVal(kv) if kv.get("queueID") == Some(&PyRep::Integer(1)) && kv.get("queueName") == Some(&PyRep::String("Default".into())))));
    assert!(q_list.iter().any(|q| matches!(q, PyRep::KeyVal(kv) if kv.get("queueID") == Some(&PyRep::Integer(2)) && kv.get("queueName") == Some(&PyRep::String("Escalated".into())))));

    // 6. GetUnreadMessages returns empty list initially
    let unread = rpc(&ctx, &mut p1, "petitioner", "GetUnreadMessages", &[]).await.expect("GetUnreadMessages");
    assert_eq!(unread, PyRep::List(Vec::new()));

    // 7. GetUserCatalogCountry reads from user_countries (populated on login and updated via set_user_country)
    let country = rpc(&ctx, &mut p1, "petitioner", "GetUserCatalogCountry", &[]).await.expect("GetUserCatalogCountry");
    assert_eq!(country, PyRep::String("US".into()));
    eve_server::services::account_services::set_user_country(&ctx, p1.user_id.unwrap(), "IS");
    let country_is = rpc(&ctx, &mut p1, "petitioner", "GetUserCatalogCountry", &[]).await.expect("GetUserCatalogCountry IS");
    assert_eq!(country_is, PyRep::String("IS".into()));

    // 8 & 9. IsZendeskEnabled and GetZendeskJwtLink read-only queries
    let z_enabled = rpc(&ctx, &mut p1, "petitioner", "IsZendeskEnabled", &[]).await.expect("IsZendeskEnabled default");
    assert_eq!(z_enabled, PyRep::Bool(true));
    let z_jwt = rpc(&ctx, &mut p1, "petitioner", "GetZendeskJwtLink", &[]).await.expect("GetZendeskJwtLink enabled");
    let Some(jwt_str) = z_jwt.as_str() else { panic!("expected JWT URL string when Zendesk is enabled"); };
    assert!(jwt_str.starts_with("https://support.eveonline.com/access/jwt?jwt=") && jwt_str.contains("cid_90000001"));

    let serenity = rpc(&ctx, &mut p1, "petitioner", "IsSerenity", &[]).await.expect("IsSerenity");
    assert_eq!(serenity, PyRep::Bool(false));
    let z_swap = rpc(&ctx, &mut p1, "petitioner", "IsZendeskSwapEnabled", &[]).await.expect("IsZendeskSwapEnabled");
    assert_eq!(z_swap, PyRep::Bool(false));

    // 10. PropertyPopulationInfo(categoryID, propertyKey) returns FormatForDropDown list of (label, id) 2-tuples
    let pop_ships = rpc(&ctx, &mut p1, "petitioner", "PropertyPopulationInfo", &[PyRep::Integer(2), PyRep::String("ShipType".into())]).await.expect("PropertyPopulationInfo ShipType");
    let PyRep::List(ship_opts) = pop_ships else { panic!("expected List from PropertyPopulationInfo"); };
    assert!(ship_opts.iter().any(|opt| matches!(opt, PyRep::Tuple(t) if t.len() == 2 && t[1] == PyRep::Integer(100_000_001))));

    let pop_sys = rpc(&ctx, &mut p1, "petitioner", "PropertyPopulationInfo", &[PyRep::Integer(401), PyRep::String("SolarSystem".into())]).await.expect("PropertyPopulationInfo SolarSystem");
    let PyRep::List(sys_opts) = pop_sys else { panic!("expected List from PropertyPopulationInfo SolarSystem"); };
    assert!(sys_opts.iter().any(|opt| matches!(opt, PyRep::Tuple(t) if t.len() == 2 && t[0].as_str() == Some("Jita") && t[1] == PyRep::Integer(30_000_142))));
}

#[tokio::test]
async fn test_raffle_lifecycle_creation_and_ticket_sales() {
    // parity: rpc:raffleProxy.AmIBanned, rpc:raffleProxy.AwardItem, rpc:raffleProxy.BuyRandomTickets
    // parity: rpc:raffleProxy.BuyTicket, rpc:raffleProxy.CreateRaffle, rpc:raffleProxy.GetActiveHistoricPrices
    // parity: rpc:raffleProxy.GetMyActiveTickets, rpc:raffleProxy.GetRaffle
    let ctx = ServerContext::new();
    let mut p1 = select(&ctx, 60_004, "demo", "demo-secret", P1).await;
    let mut p2 = select(&ctx, 60_005, "other", "other-secret", P2).await;

    // 1. AmIBanned check
    let banned = rpc(&ctx, &mut p1, "raffleProxy", "AmIBanned", &[])
        .await
        .expect("AmIBanned");
    assert_eq!(banned, PyRep::Bool(false));

    // 2. GetActiveHistoricPrices for Rifter type 587
    let prices = rpc(
        &ctx,
        &mut p1,
        "raffleProxy",
        "GetActiveHistoricPrices",
        &[PyRep::Integer(587)],
    )
    .await
    .expect("GetActiveHistoricPrices");
    let PyRep::Tuple(price_tuple) = prices else {
        panic!("expected Tuple from GetActiveHistoricPrices");
    };
    assert_eq!(price_tuple.len(), 2);

    // 3. Seed item in P1's hangar, give P1 HyperCores, and grant P2 ISK
    let rifter_item_id = seed_item(&ctx, 587, P1, 1);
    let hypercore_item_id = seed_item(&ctx, 52568, P1, 100);
    assert!(hypercore_item_id > 0);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c2) = snap.characters.get_mut(&P2) {
            c2.balance = eve_db::IskCents::from_isk(50_000_000); // 50M ISK
        }
    });

    // 4. CreateRaffle (8 tickets at 100k ISK each)
    let create_args = PyRep::Dict(vec![
        (
            PyRep::String("item_id".into()),
            PyRep::Integer(rifter_item_id),
        ),
        (PyRep::String("ticket_count".into()), PyRep::Integer(8)),
        (
            PyRep::String("ticket_price".into()),
            PyRep::Real(100_000.0),
        ),
    ]);
    let created = rpc(
        &ctx,
        &mut p1,
        "raffleProxy",
        "CreateRaffle",
        &[create_args],
    )
    .await
    .expect("CreateRaffle");
    let PyRep::KeyVal(rkv) = created else {
        panic!("expected KeyVal from CreateRaffle");
    };
    let raffle_id = rkv
        .get("raffle_id")
        .and_then(|v| v.as_i64())
        .expect("raffle_id");
    assert!(raffle_id > 0);

    // 5. GetRaffle verifies active listing
    let info = rpc(
        &ctx,
        &mut p2,
        "raffleProxy",
        "GetRaffle",
        &[PyRep::Integer(raffle_id)],
    )
    .await
    .expect("GetRaffle");
    let PyRep::KeyVal(ikv) = info else {
        panic!("expected KeyVal from GetRaffle");
    };
    assert_eq!(
        ikv.get("ticket_count").and_then(|v| v.as_i64()),
        Some(8)
    );
    assert_eq!(
        ikv.get("tickets_remaining").and_then(|v| v.as_i64()),
        Some(8)
    );

    // 6. BuyTicket: P2 buys ticket 0
    let buy1 = rpc(
        &ctx,
        &mut p2,
        "raffleProxy",
        "BuyTicket",
        &[PyRep::Integer(raffle_id), PyRep::Integer(0)],
    )
    .await
    .expect("BuyTicket");
    assert!(matches!(buy1, PyRep::KeyVal(_)));

    // 7. BuyRandomTickets: P2 buys 2 random tickets
    let buy_rand = rpc(
        &ctx,
        &mut p2,
        "raffleProxy",
        "BuyRandomTickets",
        &[PyRep::Integer(raffle_id), PyRep::Integer(2)],
    )
    .await
    .expect("BuyRandomTickets");
    assert!(matches!(buy_rand, PyRep::KeyVal(_)));

    // 8. GetMyActiveTickets
    let my_tickets = rpc(&ctx, &mut p2, "raffleProxy", "GetMyActiveTickets", &[])
        .await
        .expect("GetMyActiveTickets");
    let PyRep::List(tickets_list) = my_tickets else {
        panic!("expected List from GetMyActiveTickets");
    };
    assert!(tickets_list.iter().any(|r| match r {
        PyRep::KeyVal(kv) => kv.get("raffle_id").and_then(|v| v.as_i64()) == Some(raffle_id),
        _ => false,
    }));

    // 9. Buy remaining tickets to finish raffle (P2 purchases all remaining slots)
    let buy_rest = rpc(
        &ctx,
        &mut p2,
        "raffleProxy",
        "BuyRandomTickets",
        &[PyRep::Integer(raffle_id), PyRep::Integer(5)],
    )
    .await
    .expect("BuyRandomTickets rest");
    assert!(matches!(buy_rest, PyRep::KeyVal(_)));

    // Verify raffle finished and P2 won
    let finished_info = rpc(
        &ctx,
        &mut p2,
        "raffleProxy",
        "GetRaffle",
        &[PyRep::Integer(raffle_id)],
    )
    .await
    .expect("GetRaffle finished");
    let PyRep::KeyVal(fkv) = finished_info else {
        panic!("expected KeyVal");
    };
    assert_eq!(
        fkv.get("tickets_remaining").and_then(|v| v.as_i64()),
        Some(0)
    );
    assert_eq!(fkv.get("winner_id").and_then(|v| v.as_i64()), Some(P2));

    // 10. AwardItem: winner P2 claims the item
    let award = rpc(
        &ctx,
        &mut p2,
        "raffleProxy",
        "AwardItem",
        &[PyRep::Integer(raffle_id)],
    )
    .await
    .expect("AwardItem");
    assert_eq!(award, PyRep::Integer(rifter_item_id));

    // Verify physical item ownership transferred in world state
    ctx.db.with_snapshot(|snap| {
        let it = snap.items.get(&rifter_item_id).expect("rifter item");
        assert_eq!(it.owner_id, P2);
    });
}

#[tokio::test]
async fn test_raffle_queries_and_subscriptions() {
    // parity: rpc:raffleProxy.FilteredGrab, rpc:raffleProxy.GetCreatedParticipated, rpc:raffleProxy.GetMyRaffleHistory
    // parity: rpc:raffleProxy.Grab, rpc:raffleProxy.SubscribeToRaffle, rpc:raffleProxy.SubscribeToTickets
    // parity: rpc:raffleProxy.UnSubscribeFromTickets, rpc:raffleProxy.UnsubscribeFromRaffle
    let ctx = ServerContext::new();
    let mut p1 = select(&ctx, 60_006, "demo", "demo-secret", P1).await;
    let mut p2 = select(&ctx, 60_007, "other", "other-secret", P2).await;

    let item_a = seed_item(&ctx, 587, P1, 1);
    let item_b = seed_item(&ctx, 597, P1, 1);
    let item_c = seed_item(&ctx, 593, P1, 1);
    let _hc = seed_item(&ctx, 52568, P1, 500);
    let hc_b = seed_item(&ctx, 52568, P1, 500);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(it) = snap.items.get_mut(&item_b) {
            it.location_id = 60_008_494; // Amarr VIII (solar system 30002187, region 10000043)
        }
        if let Some(hc) = snap.items.get_mut(&hc_b) {
            hc.location_id = 60_008_494;
        }
        if let Some(c2) = snap.characters.get_mut(&P2) {
            c2.balance = eve_db::IskCents::from_isk(50_000_000);
        }
    });

    let make_raffle = |item_id: i64, cnt: i64, price: f64| {
        PyRep::Dict(vec![
            (PyRep::String("item_id".into()), PyRep::Integer(item_id)),
            (PyRep::String("ticket_count".into()), PyRep::Integer(cnt)),
            (PyRep::String("ticket_price".into()), PyRep::Real(price)),
        ])
    };
    let PyRep::KeyVal(r_a) = rpc(&ctx, &mut p1, "raffleProxy", "CreateRaffle", &[make_raffle(item_a, 8, 100_000.0)]).await.expect("CreateRaffle A") else { panic!("expected KeyVal"); };
    let raf_a = r_a.get("raffle_id").and_then(|v| v.as_i64()).unwrap();
    ctx.db.with_mut_snapshot(|snap| { if let Some(ch) = snap.characters.get_mut(&P1) { ch.station_id = Some(60_008_494); } });
    let PyRep::KeyVal(r_b) = rpc(&ctx, &mut p1, "raffleProxy", "CreateRaffle", &[make_raffle(item_b, 16, 500_000.0)]).await.expect("CreateRaffle B") else { panic!("expected KeyVal"); };
    let raf_b = r_b.get("raffle_id").and_then(|v| v.as_i64()).unwrap();
    ctx.db.with_mut_snapshot(|snap| { if let Some(ch) = snap.characters.get_mut(&P1) { ch.station_id = Some(60_003_760); } });
    let PyRep::KeyVal(r_c) = rpc(&ctx, &mut p1, "raffleProxy", "CreateRaffle", &[make_raffle(item_c, 8, 50_000.0)]).await.expect("CreateRaffle C") else { panic!("expected KeyVal"); };
    let raf_c = r_c.get("raffle_id").and_then(|v| v.as_i64()).unwrap();

    // Complete Raffle C by purchasing all 8 tickets with P2
    rpc(&ctx, &mut p2, "raffleProxy", "BuyRandomTickets", &[PyRep::Integer(raf_c), PyRep::Integer(8)]).await.expect("Buy all C");

    // 1. Subscription notifications return None
    assert_eq!(rpc(&ctx, &mut p1, "raffleProxy", "SubscribeToRaffle", &[PyRep::Integer(raf_a)]).await.expect("SubscribeToRaffle"), PyRep::None);
    assert_eq!(rpc(&ctx, &mut p1, "raffleProxy", "UnsubscribeFromRaffle", &[PyRep::Integer(raf_a)]).await.expect("UnsubscribeFromRaffle"), PyRep::None);
    assert_eq!(rpc(&ctx, &mut p1, "raffleProxy", "SubscribeToTickets", &[PyRep::Integer(raf_a)]).await.expect("SubscribeToTickets"), PyRep::None);
    assert_eq!(rpc(&ctx, &mut p1, "raffleProxy", "UnSubscribeFromTickets", &[PyRep::Integer(raf_a)]).await.expect("UnSubscribeFromTickets"), PyRep::None);

    // 2. Grab returns active raffles (raf_a, raf_b) and excludes finished raf_c
    let PyRep::List(grab_list) = rpc(&ctx, &mut p2, "raffleProxy", "Grab", &[]).await.expect("Grab") else { panic!("expected List"); };
    let grab_ids: Vec<i64> = grab_list.iter().filter_map(|r| if let PyRep::KeyVal(kv) = r { kv.get("raffle_id").and_then(|v| v.as_i64()) } else { None }).collect();
    assert!(grab_ids.contains(&raf_a) && grab_ids.contains(&raf_b), "Grab must return active raffles");
    assert!(!grab_ids.contains(&raf_c), "Grab must exclude finished raffles");

    // 3. FilteredGrab honors type_id, solar_system_id, min_price, and ticket_count filters
    let filter_type = PyRep::Dict(vec![(PyRep::String("type_id".into()), PyRep::Integer(587))]);
    let PyRep::List(fg_type) = rpc(&ctx, &mut p2, "raffleProxy", "FilteredGrab", &[filter_type]).await.expect("FilteredGrab type_id") else { panic!("expected List"); };
    assert_eq!(fg_type.len(), 1);
    assert!(matches!(&fg_type[0], PyRep::KeyVal(kv) if kv.get("raffle_id") == Some(&PyRep::Integer(raf_a))));

    let filter_price = PyRep::Dict(vec![
        (PyRep::String("min_price".into()), PyRep::Real(200_000.0)),
        (PyRep::String("ticket_count".into()), PyRep::Integer(16)),
        (PyRep::String("solar_system_id".into()), PyRep::Integer(30_002_187)),
    ]);
    let PyRep::List(fg_price) = rpc(&ctx, &mut p2, "raffleProxy", "FilteredGrab", &[filter_price]).await.expect("FilteredGrab price+sys") else { panic!("expected List"); };
    assert_eq!(fg_price.len(), 1);
    assert!(matches!(&fg_price[0], PyRep::KeyVal(kv) if kv.get("raffle_id") == Some(&PyRep::Integer(raf_b))));

    // 4. GetCreatedParticipated & GetMyRaffleHistory
    let PyRep::List(created_part) = rpc(&ctx, &mut p1, "raffleProxy", "GetCreatedParticipated", &[]).await.expect("GetCreatedParticipated") else { panic!("expected List"); };
    assert!(created_part.len() >= 2);
    let PyRep::List(history) = rpc(&ctx, &mut p2, "raffleProxy", "GetMyRaffleHistory", &[]).await.expect("GetMyRaffleHistory") else { panic!("expected List"); };
    assert!(history.iter().any(|r| matches!(r, PyRep::KeyVal(kv) if kv.get("raffle_id") == Some(&PyRep::Integer(raf_c)))));
}
