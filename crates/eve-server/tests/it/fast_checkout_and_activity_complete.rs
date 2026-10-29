//! Integration tests for `FastCheckoutService` mining & tutorial RPCs and `activityMgr` badge state.

use crate::common;
use eve_db::{MoonExtractionRecord, UpwellStructureRecord};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use std::collections::HashMap;

const CHAR_ID: i64 = 90_000_001;
const CHAR_ID_2: i64 = 90_000_002;
const CORP_ID: i64 = 98_000_001;
const OTHER_CORP_ID: i64 = 1_000_045;
const SYSTEM_ID: u32 = 30000012; // Asabona (Low-Sec)
const STRUCTURE_ID: i64 = 1_090_000_001;
const MOON_ID: i64 = 40_009_082;

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let kw = HashMap::new();
    ctx.dispatch_rpc(sess, service, None, method, args, &kw, None)
        .await
}

async fn login_pilot_char(
    ctx: &ServerContext,
    sess_id: u64,
    username: &str,
    password: &str,
    char_id: i64,
) -> SessionState {
    let mut sess = SessionState::new(sess_id);
    common::login_pilot(ctx, &mut sess, username, password).await;
    rpc(
        ctx,
        &mut sess,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
    )
    .await
    .expect("SelectCharacterID");
    sess
}

fn dict_get<'a>(dict_rep: &'a PyRep, key: &str) -> Option<&'a PyRep> {
    match dict_rep {
        PyRep::Dict(entries) => entries.iter().find_map(|(k, v)| {
            if let PyRep::String(s) | PyRep::Token(s) = k {
                if s == key {
                    Some(v)
                } else {
                    None
                }
            } else {
                None
            }
        }),
        _ => None,
    }
}

#[tokio::test]
// parity: rpc:FastCheckoutService.RequestMiningCycleOutput, rpc:FastCheckoutService.RequestMiningDetails, rpc:FastCheckoutService.get_tutorial_completion_status, rpc:FastCheckoutService.initialize_tutorial_status, rpc:FastCheckoutService.is_enabled, rpc:FastCheckoutService.reset_tutorial, rpc:FastCheckoutService.skip_tutorial, rpc:activityMgr.ActivitySeen, rpc:activityMgr.GetActivitiesPopState, rpc:activityMgr.GetActivityBadgeStatuses, rpc:activityMgr.SetActivitiesPopState
async fn test_fast_checkout_and_activity_complete() {
    eve_testkit::attest(&[
        "rpc:FastCheckoutService.RequestMiningCycleOutput",
        "rpc:FastCheckoutService.RequestMiningDetails",
        "rpc:FastCheckoutService.get_tutorial_completion_status",
        "rpc:FastCheckoutService.initialize_tutorial_status",
        "rpc:FastCheckoutService.is_enabled",
        "rpc:FastCheckoutService.reset_tutorial",
        "rpc:FastCheckoutService.skip_tutorial",
        "rpc:activityMgr.ActivitySeen",
        "rpc:activityMgr.GetActivitiesPopState",
        "rpc:activityMgr.GetActivityBadgeStatuses",
        "rpc:activityMgr.SetActivitiesPopState",
    ]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let mut sess1 = login_pilot_char(&ctx, 1201, "demo", "demo-secret", CHAR_ID).await;
    let mut sess2 = login_pilot_char(&ctx, 1202, "other", "other-secret", CHAR_ID_2).await;
    let now = ctx.now_unix();

    // 1. Setup Metenox Moon Drill structure and active moon extraction in world state
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c1) = snap.characters.get_mut(&CHAR_ID) {
            c1.corporation_id = CORP_ID;
        }
        if let Some(c2) = snap.characters.get_mut(&CHAR_ID_2) {
            c2.corporation_id = OTHER_CORP_ID;
        }

        snap.structures.insert(
            STRUCTURE_ID,
            UpwellStructureRecord {
                structure_id: STRUCTURE_ID,
                type_id: 81826, // Metenox Moon Drill
                owner_corp_id: CORP_ID,
                solar_system_id: SYSTEM_ID,
                name: "Test Metenox Moon Drill".into(),
                position: [200_000.0, 0.0, -100_000.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 150,
                shield_hp: 50_000.0,
                armor_hp: 50_000.0,
                hull_hp: 50_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0, self_destruct_at_unix: None,
            },
        );

        snap.moon_extractions.insert(
            STRUCTURE_ID,
            MoonExtractionRecord {
                structure_id: STRUCTURE_ID,
                moon_id: MOON_ID,
                extraction_start_unix: now.saturating_sub(3600),
                chunk_arrival_unix: now + 72_000,
                natural_decay_unix: now + 82_800,
                status: "Started".to_string(),
                auto_fracture: false,
                event_id: 42,
                moon_ores: vec![(45494, 0.60), (45495, 0.40)], // Cobaltite & Euxenite
                spawned_asteroid_ids: vec![],
            },
        );
    });

    // ------------------------------------------------------------------------
    // Part 1: FastCheckoutService Mining RPCs & Auth
    // ------------------------------------------------------------------------
    // 1.1 FastCheckoutService.RequestMiningCycleOutput (authorized pilot 1)
    let cycle_rep = rpc(
        &ctx,
        &mut sess1,
        "FastCheckoutService",
        "RequestMiningCycleOutput",
        &[PyRep::Integer(STRUCTURE_ID)],
    )
    .await
    .expect("RequestMiningCycleOutput for authorized character");

    let materials_list = match cycle_rep {
        PyRep::List(ref list) => {
            assert!(
                !list.is_empty(),
                "RequestMiningCycleOutput should return non-empty materials list"
            );
            for item in list {
                match item {
                    PyRep::Tuple(tuple) => {
                        assert_eq!(
                            tuple.len(),
                            3,
                            "Material tuple must have 3 elements (type_id, quantity, volume)"
                        );
                        assert!(matches!(tuple[0], PyRep::Integer(_)));
                        assert!(matches!(tuple[1], PyRep::Integer(_)));
                        assert!(matches!(tuple[2], PyRep::Real(_)));
                    }
                    other => panic!("Expected tuple in material list, got {other:?}"),
                }
            }
            list.clone()
        }
        other => panic!("Expected PyRep::List from RequestMiningCycleOutput, got {other:?}"),
    };

    // 1.2 FastCheckoutService.RequestMiningDetails (authorized pilot 1)
    let details_rep = rpc(
        &ctx,
        &mut sess1,
        "FastCheckoutService",
        "RequestMiningDetails",
        &[PyRep::Integer(STRUCTURE_ID)],
    )
    .await
    .expect("RequestMiningDetails for authorized character");

    match details_rep {
        PyRep::Dict(_) => {
            let fuel = dict_get(&details_rep, "availableFuel")
                .expect("availableFuel key")
                .as_i64()
                .expect("availableFuel is integer");
            assert_eq!(fuel, 150);

            let moon = dict_get(&details_rep, "moonID")
                .expect("moonID key")
                .as_i64()
                .expect("moonID is integer");
            assert_eq!(moon, MOON_ID);

            let next_harvest = dict_get(&details_rep, "nextHarvestTime")
                .expect("nextHarvestTime key")
                .as_i64()
                .expect("nextHarvestTime is integer");
            let expected_ft = ((now + 72_000) as i64 + 11_644_473_600) * 10_000_000;
            assert_eq!(next_harvest, expected_ft);

            let rem_capacity = match dict_get(&details_rep, "remainingOutputBayCapacity")
                .expect("remainingOutputBayCapacity key")
            {
                PyRep::Real(v) => *v,
                other => panic!("Expected Real remainingOutputBayCapacity, got {other:?}"),
            };
            assert!(rem_capacity >= 0.0);

            let mats_in_details = dict_get(&details_rep, "outputMaterials")
                .expect("outputMaterials key");
            assert_eq!(mats_in_details, &PyRep::List(materials_list));

            // Verify BF-MINING-DETAILS-FUEL-03: SpecializedFuelBay overrides st.fuel_blocks without double-counting,
            // and moon output bay volume uses SpecializedMineralHold / CorpHangar (not general pilot Hangar)
            let cap_before = rem_capacity;
            ctx.db.with_mut_snapshot(|snap| {
                snap.items.insert(770001, eve_db::InventoryItemRecord {
                    item_id: 770001,
                    type_id: 670,
                    owner_id: CHAR_ID,
                    location_id: STRUCTURE_ID,
                    flag: eve_db::InventoryFlag::Hangar,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    dynamic_attributes: HashMap::new(),
                    source_mutaplasmid_id: None,
                    insured_payout_isk: 0.0,
                    is_blueprint_copy: false,
                    blueprint_runs_remaining: None,
                });
                snap.items.insert(770002, eve_db::InventoryItemRecord {
                    item_id: 770002,
                    type_id: 4051,
                    owner_id: CORP_ID,
                    location_id: STRUCTURE_ID,
                    flag: eve_db::InventoryFlag::SpecializedFuelBay,
                    quantity: 200,
                    singleton: false,
                    version: 1,
                    dynamic_attributes: HashMap::new(),
                    source_mutaplasmid_id: None,
                    insured_payout_isk: 0.0,
                    is_blueprint_copy: false,
                    blueprint_runs_remaining: None,
                });
                snap.items.insert(770003, eve_db::InventoryItemRecord {
                    item_id: 770003,
                    type_id: 34,
                    owner_id: CORP_ID,
                    location_id: STRUCTURE_ID,
                    flag: eve_db::InventoryFlag::SpecializedMineralHold,
                    quantity: 500,
                    singleton: false,
                    version: 1,
                    dynamic_attributes: HashMap::new(),
                    source_mutaplasmid_id: None,
                    insured_payout_isk: 0.0,
                    is_blueprint_copy: false,
                    blueprint_runs_remaining: None,
                });
            });

            let details_rep2 = rpc(&ctx, &mut sess1, "FastCheckoutService", "RequestMiningDetails", &[PyRep::Integer(STRUCTURE_ID)]).await.expect("RequestMiningDetails 2");
            let fuel2 = dict_get(&details_rep2, "availableFuel").unwrap().as_i64().unwrap();
            assert_eq!(fuel2, 200, "Fuel must reflect SpecializedFuelBay (200) without adding st.fuel_blocks (150)");

            let cap2 = match dict_get(&details_rep2, "remainingOutputBayCapacity").unwrap() {
                PyRep::Real(v) => *v,
                _ => panic!("Expected Real"),
            };
            assert!((cap_before - cap2 - 5.0).abs() < 1e-4, "Output bay capacity must reduce only by moon mineral hold items (5.0 m3), not general hangar");
        }
        other => panic!("Expected PyRep::Dict from RequestMiningDetails, got {other:?}"),
    }

    // 1.3 Authorization checks: pilot 2 (unauthorized corp) receives AccessDenied
    let denied_details = rpc(
        &ctx,
        &mut sess2,
        "FastCheckoutService",
        "RequestMiningDetails",
        &[PyRep::Integer(STRUCTURE_ID)],
    )
    .await
    .expect_err("Unauthorized character should be denied mining details");
    assert_eq!(denied_details.code, "AccessDenied");

    let denied_cycle = rpc(
        &ctx,
        &mut sess2,
        "FastCheckoutService",
        "RequestMiningCycleOutput",
        &[PyRep::Integer(STRUCTURE_ID)],
    )
    .await
    .expect_err("Unauthorized character should be denied mining cycle output");
    assert_eq!(denied_cycle.code, "AccessDenied");

    // 1.4 Nonexistent structure lookup returns StructureNotFound
    let not_found_details = rpc(
        &ctx,
        &mut sess1,
        "FastCheckoutService",
        "RequestMiningDetails",
        &[PyRep::Integer(9_999_999)],
    )
    .await
    .expect_err("Nonexistent structure should return StructureNotFound");
    assert_eq!(not_found_details.code, "StructureNotFound");

    let not_found_cycle = rpc(
        &ctx,
        &mut sess1,
        "FastCheckoutService",
        "RequestMiningCycleOutput",
        &[PyRep::Integer(9_999_999)],
    )
    .await
    .expect_err("Nonexistent structure should return StructureNotFound");
    assert_eq!(not_found_cycle.code, "StructureNotFound");

    // ------------------------------------------------------------------------
    // Part 2: FastCheckoutService Tutorial RPCs & Multi-Character Isolation
    // ------------------------------------------------------------------------
    // 2.1 FastCheckoutService.is_enabled (driven via live machoNet.SetGlobalConfigValue)
    assert_eq!(rpc(&ctx, &mut sess1, "FastCheckoutService", "is_enabled", &[]).await.expect("is_enabled"), PyRep::Bool(true));
    assert_eq!(rpc(&ctx, &mut sess1, "ProjectDiscovery", "is_enabled", &[]).await.expect("PD is_enabled"), PyRep::Bool(true));
    let non_gm_err = rpc(&ctx, &mut sess1, "machoNet", "SetGlobalConfigValue", &[PyRep::String("projectDiscoveryEnabled".into()), PyRep::Bool(false)]).await.expect_err("non-GM must fail");
    assert_eq!(non_gm_err.code, "InsufficientRole");
    let mut gm_sess = sess1.clone();
    gm_sess.role |= eve_machonet::service_roles::ROLE_GML;
    rpc(&ctx, &mut gm_sess, "machoNet", "SetGlobalConfigValue", &[PyRep::String("projectDiscoveryEnabled".into()), PyRep::Bool(false)]).await.expect("GM disable ProjectDiscovery");
    assert_eq!(rpc(&ctx, &mut sess1, "FastCheckoutService", "is_enabled", &[]).await.expect("is_enabled disabled"), PyRep::Bool(false));
    assert_eq!(rpc(&ctx, &mut sess2, "ProjectDiscovery", "is_enabled", &[]).await.expect("PD is_enabled disabled"), PyRep::Bool(false));
    rpc(&ctx, &mut gm_sess, "machoNet", "SetGlobalConfigValue", &[PyRep::String("projectDiscoveryEnabled".into()), PyRep::Bool(true)]).await.expect("GM enable ProjectDiscovery");
    assert_eq!(rpc(&ctx, &mut sess1, "FastCheckoutService", "is_enabled", &[]).await.expect("is_enabled re-enabled"), PyRep::Bool(true));

    // 2.2 FastCheckoutService.initialize_tutorial_status (new char 1: incomplete -> Bool(false))
    let init_res_1 = rpc(
        &ctx,
        &mut sess1,
        "FastCheckoutService",
        "initialize_tutorial_status",
        &[],
    )
    .await
    .expect("initialize_tutorial_status char 1");
    assert_eq!(init_res_1, PyRep::Bool(false));

    // 2.3 FastCheckoutService.get_tutorial_completion_status (initially incomplete)
    let status_before = rpc(
        &ctx,
        &mut sess1,
        "FastCheckoutService",
        "get_tutorial_completion_status",
        &[],
    )
    .await
    .expect("get_tutorial_completion_status char 1");
    assert_eq!(status_before, PyRep::Bool(false));

    // 2.4 FastCheckoutService.skip_tutorial (char 1)
    let skip_res = rpc(
        &ctx,
        &mut sess1,
        "FastCheckoutService",
        "skip_tutorial",
        &[],
    )
    .await
    .expect("skip_tutorial char 1");
    assert_eq!(skip_res, PyRep::Bool(true));

    // 2.5 FastCheckoutService.get_tutorial_completion_status (now completed for char 1)
    let status_after_skip = rpc(
        &ctx,
        &mut sess1,
        "FastCheckoutService",
        "get_tutorial_completion_status",
        &[],
    )
    .await
    .expect("get_tutorial_completion_status after skip char 1");
    assert_eq!(status_after_skip, PyRep::Bool(true));

    // 2.6 initialize_tutorial_status on char 1 now returns Bool(true)
    let init_res_after_skip = rpc(
        &ctx,
        &mut sess1,
        "FastCheckoutService",
        "initialize_tutorial_status",
        &[],
    )
    .await
    .expect("initialize_tutorial_status after skip char 1");
    assert_eq!(init_res_after_skip, PyRep::Bool(true));

    // 2.7 Character isolation: Character 2 tutorial remains incomplete!
    let init_res_2 = rpc(
        &ctx,
        &mut sess2,
        "FastCheckoutService",
        "initialize_tutorial_status",
        &[],
    )
    .await
    .expect("initialize_tutorial_status char 2");
    assert_eq!(init_res_2, PyRep::Bool(false));

    let status_char2 = rpc(
        &ctx,
        &mut sess2,
        "FastCheckoutService",
        "get_tutorial_completion_status",
        &[],
    )
    .await
    .expect("get_tutorial_completion_status char 2");
    assert_eq!(status_char2, PyRep::Bool(false));

    // 2.8 FastCheckoutService.reset_tutorial (char 1)
    let reset_res = rpc(
        &ctx,
        &mut sess1,
        "FastCheckoutService",
        "reset_tutorial",
        &[],
    )
    .await
    .expect("reset_tutorial char 1");
    assert_eq!(reset_res, PyRep::Bool(true));

    // 2.9 FastCheckoutService.get_tutorial_completion_status (incomplete again after reset)
    let status_after_reset = rpc(
        &ctx,
        &mut sess1,
        "FastCheckoutService",
        "get_tutorial_completion_status",
        &[],
    )
    .await
    .expect("get_tutorial_completion_status after reset char 1");
    assert_eq!(status_after_reset, PyRep::Bool(false));

    // ------------------------------------------------------------------------
    // Part 3: activityMgr RPCs & Multi-Character Isolation
    // ------------------------------------------------------------------------
    // 3.1 activityMgr.GetActivityBadgeStatuses returns persisted seen activity IDs as PySet (initially empty)
    let badges_char1 = rpc(
        &ctx,
        &mut sess1,
        "activityMgr",
        "GetActivityBadgeStatuses",
        &[],
    )
    .await
    .expect("GetActivityBadgeStatuses char 1");
    assert_eq!(badges_char1, PyRep::PySet(vec![]));

    let badges_char2 = rpc(
        &ctx,
        &mut sess2,
        "activityMgr",
        "GetActivityBadgeStatuses",
        &[],
    )
    .await
    .expect("GetActivityBadgeStatuses char 2");
    assert_eq!(badges_char2, PyRep::PySet(vec![]));

    // 3.2 activityMgr.ActivitySeen rejects invalid/non-SDE IDs and records SDE newactivities IDs (42, 43) for Character 1 only
    assert_eq!(rpc(&ctx, &mut sess1, "activityMgr", "ActivitySeen", &[]).await.expect_err("missing arg").code, "InvalidArgument");
    assert_eq!(rpc(&ctx, &mut sess1, "activityMgr", "ActivitySeen", &[PyRep::Integer(0)]).await.expect_err("zero id").code, "InvalidArgument");
    assert_eq!(rpc(&ctx, &mut sess1, "activityMgr", "ActivitySeen", &[PyRep::Integer(-1)]).await.expect_err("negative id").code, "InvalidArgument");
    assert_eq!(rpc(&ctx, &mut sess1, "activityMgr", "ActivitySeen", &[PyRep::String("bad".into())]).await.expect_err("non-int").code, "InvalidArgument");
    assert_eq!(rpc(&ctx, &mut sess1, "activityMgr", "ActivitySeen", &[PyRep::Integer(46)]).await.expect_err("gap id 46").code, "InvalidActivityID");
    assert_eq!(rpc(&ctx, &mut sess1, "activityMgr", "ActivitySeen", &[PyRep::Integer(999_999)]).await.expect_err("unknown id").code, "InvalidActivityID");
    let seen_res = rpc(
        &ctx,
        &mut sess1,
        "activityMgr",
        "ActivitySeen",
        &[PyRep::Integer(42)],
    )
    .await
    .expect("ActivitySeen 42 char 1");
    assert_eq!(seen_res, PyRep::Bool(true));

    let seen_res_2 = rpc(
        &ctx,
        &mut sess1,
        "activityMgr",
        "ActivitySeen",
        &[PyRep::Integer(43)],
    )
    .await
    .expect("ActivitySeen 43 char 1");
    assert_eq!(seen_res_2, PyRep::Bool(true));
    assert_eq!(rpc(&ctx, &mut sess1, "activityMgr", "ActivitySeen", &[PyRep::Integer(42)]).await.expect("ActivitySeen 42 dup"), PyRep::Bool(true));

    let badges_char1_after_seen = rpc(
        &ctx,
        &mut sess1,
        "activityMgr",
        "GetActivityBadgeStatuses",
        &[],
    )
    .await
    .expect("GetActivityBadgeStatuses char 1 after ActivitySeen");
    assert_eq!(
        badges_char1_after_seen,
        PyRep::PySet(vec![PyRep::Integer(42), PyRep::Integer(43)])
    );

    // Multi-character isolation: Character 2 still has an empty seen set!
    let badges_char2_after_seen = rpc(
        &ctx,
        &mut sess2,
        "activityMgr",
        "GetActivityBadgeStatuses",
        &[],
    )
    .await
    .expect("GetActivityBadgeStatuses char 2 after char 1 ActivitySeen");
    assert_eq!(badges_char2_after_seen, PyRep::PySet(vec![]));

    // 3.3 activityMgr.GetActivitiesPopState (initially empty dict for both characters)
    let pop_char1_initial = rpc(
        &ctx,
        &mut sess1,
        "activityMgr",
        "GetActivitiesPopState",
        &[],
    )
    .await
    .expect("GetActivitiesPopState char 1");
    assert_eq!(pop_char1_initial, PyRep::Dict(vec![]));

    let pop_char2_initial = rpc(
        &ctx,
        &mut sess2,
        "activityMgr",
        "GetActivitiesPopState",
        &[],
    )
    .await
    .expect("GetActivitiesPopState char 2");
    assert_eq!(pop_char2_initial, PyRep::Dict(vec![]));

    // 3.4 activityMgr.SetActivitiesPopState input validation: rejects non-dict
    let err_invalid_pop = rpc(
        &ctx,
        &mut sess1,
        "activityMgr",
        "SetActivitiesPopState",
        &[PyRep::Integer(42)],
    )
    .await
    .expect_err("Non-dict pop state must return InvalidArgument");
    assert_eq!(err_invalid_pop.code, "InvalidArgument");

    // 3.5 activityMgr.SetActivitiesPopState for Character 1
    let pop_state_data = PyRep::Dict(vec![
        (PyRep::String("activity_101".into()), PyRep::Bool(true)),
        (PyRep::String("activity_103".into()), PyRep::Integer(1)),
    ]);
    let set_pop_res = rpc(
        &ctx,
        &mut sess1,
        "activityMgr",
        "SetActivitiesPopState",
        &[pop_state_data.clone()],
    )
    .await
    .expect("SetActivitiesPopState char 1");
    assert_eq!(set_pop_res, PyRep::Bool(true));

    // Character 1 returns the persisted pop state
    let pop_char1_after_set = rpc(
        &ctx,
        &mut sess1,
        "activityMgr",
        "GetActivitiesPopState",
        &[],
    )
    .await
    .expect("GetActivitiesPopState char 1 after Set");
    assert_eq!(pop_char1_after_set, pop_state_data);

    // Multi-character isolation: Character 2's pop state remains unaffected (empty dict)
    let pop_char2_after_set = rpc(
        &ctx,
        &mut sess2,
        "activityMgr",
        "GetActivitiesPopState",
        &[],
    )
    .await
    .expect("GetActivitiesPopState char 2 after char 1 Set");
    assert_eq!(pop_char2_after_set, PyRep::Dict(vec![]));
}

#[tokio::test]
async fn test_fast_checkout_buy_offer_payload_and_vault_reload() {
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let mut sess = login_pilot_char(&ctx, 1205, "demo", "demo-secret", CHAR_ID).await;
    let user_id = sess.user_id.expect("user_id");

    eve_server::services::fast_checkout_and_activity::set_user_fast_checkout_enabled(&ctx, user_id, true);

    let offers_rep = rpc(&ctx, &mut sess, "FastCheckoutService", "GetOffersForUser", &[]).await.expect("GetOffersForUser");
    let PyRep::Dict(offers_dict) = offers_rep else { panic!("Expected dict"); };
    let plex_offers = offers_dict.iter().find_map(|(k, v)| if k == &PyRep::String("plex".into()) { Some(v) } else { None }).expect("plex list");
    let PyRep::List(list) = plex_offers else { panic!("Expected list"); };
    let first_offer = list.first().expect("first offer").clone();

    let vault_before = ctx.db.with_snapshot(|snap| snap.account_plex_vault.get(&user_id).copied().unwrap_or(0));
    let plex_legacy_before = ctx.db.get_character(CHAR_ID).expect("character").plex_balance;

    // Call BuyOffer with offer Dict payload
    let mut kwargs = HashMap::new();
    kwargs.insert("offer".into(), first_offer);
    kwargs.insert("password".into(), PyRep::String("demo-secret".into()));
    kwargs.insert("journeyID".into(), PyRep::String("test-journey".into()));
    kwargs.insert("purchaseTraceID".into(), PyRep::String("test-trace".into()));

    let buy_res = ctx.dispatch_rpc(&mut sess, "FastCheckoutService", None, "BuyOffer", &[], &kwargs, None).await.expect("BuyOffer with Dict payload");
    let PyRep::Dict(res_dict) = buy_res else { panic!("Expected dict"); };
    assert_eq!(res_dict.iter().find_map(|(k, v)| if k == &PyRep::String("status".into()) { v.as_str() } else { None }), Some("CONFIRMED"));

    let vault_after = ctx.db.with_snapshot(|snap| snap.account_plex_vault.get(&user_id).copied().unwrap_or(0));
    assert_eq!(vault_after, vault_before + 110);
    let plex_legacy_after = ctx.db.get_character(CHAR_ID).expect("character").plex_balance;
    assert_eq!(plex_legacy_after, plex_legacy_before, "ch.plex_balance must not be directly incremented");

    // Verify snapshot reload / fold_legacy_plex_balances does not double-credit PLEX
    ctx.db.with_mut_snapshot(|snap| {
        snap.fold_legacy_plex_balances();
    });
    let vault_after_fold = ctx.db.with_snapshot(|snap| snap.account_plex_vault.get(&user_id).copied().unwrap_or(0));
    assert_eq!(vault_after_fold, vault_after, "Snapshot reload / fold must not double credit PLEX");
}
