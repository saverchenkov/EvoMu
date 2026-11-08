//! Integration tests for `FastCheckoutService` mining & tutorial RPCs and `activityMgr` badge state.

use crate::common;
use eve_db::{VirtualServerClock, WorldRepository};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use std::collections::HashMap;
use std::sync::Arc;

const CHAR_ID: i64 = 90_000_001;
const CHAR_ID_2: i64 = 90_000_002;
const SYSTEM_ID: u32 = 30000012; // Asabona (Low-Sec)

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
// parity: rpc:FastCheckoutService.RequestMiningDetails, rpc:FastCheckoutService.get_tutorial_completion_status, rpc:FastCheckoutService.initialize_tutorial_status, rpc:FastCheckoutService.is_enabled, rpc:FastCheckoutService.reset_tutorial, rpc:FastCheckoutService.skip_tutorial, rpc:activityMgr.ActivitySeen, rpc:activityMgr.GetActivitiesPopState, rpc:activityMgr.GetActivityBadgeStatuses, rpc:activityMgr.SetActivitiesPopState
async fn test_fast_checkout_and_activity_complete() {
    eve_testkit::attest(&[
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
    // Clear login 10s session-change timer before corp creation
    ctx.advance_secs(11);

    // 1. Setup Metenox Moon Drill via genuine live RPCs (corp creation, anchoring,
    //    lifecycle transitions, approach, fuel/gas loading, and hourly mining tick)
    let saved_role = sess1.role;
    sess1.role = eve_machonet::service_roles::ROLE_PLAYER_DEFAULT
        | eve_machonet::service_roles::ROLE_GML
        | eve_machonet::service_roles::ROLE_GMH;
    rpc(
        &ctx,
        &mut sess1,
        "slash",
        "SlashCmd",
        &[PyRep::String("/giveskill 3363 1".into())],
    )
    .await
    .expect("/giveskill Corporation Management");
    rpc(
        &ctx,
        &mut sess1,
        "slash",
        "SlashCmd",
        &[PyRep::String("/giveisk 5000000000".into())],
    )
    .await
    .expect("/giveisk");
    sess1.role = saved_role;

    rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "AddCorporation",
        &[
            PyRep::String("Metenox Checkout Mining".into()),
            PyRep::String("MTXC".into()),
        ],
    )
    .await
    .expect("AddCorporation");
    // Advance past 7-day corp age anchoring restriction
    ctx.advance_secs(7 * 86_400 + 10);

    sess1.role = eve_machonet::service_roles::ROLE_PLAYER_DEFAULT
        | eve_machonet::service_roles::ROLE_GML
        | eve_machonet::service_roles::ROLE_GMH;
    rpc(
        &ctx,
        &mut sess1,
        "slash",
        "SlashCmd",
        &[PyRep::String(format!("/tr {SYSTEM_ID}"))],
    )
    .await
    .expect("/tr lowsec Asabona");
    let create_drill = rpc(
        &ctx,
        &mut sess1,
        "slash",
        "SlashCmd",
        &[PyRep::String("/createitem 81826 1".into())],
    )
    .await
    .expect("/createitem Metenox");
    let create_fuel = rpc(
        &ctx,
        &mut sess1,
        "slash",
        "SlashCmd",
        &[PyRep::String("/createitem 4051 200".into())],
    )
    .await
    .expect("/createitem Fuel Blocks");
    let create_gas = rpc(
        &ctx,
        &mut sess1,
        "slash",
        "SlashCmd",
        &[PyRep::String("/createitem 81143 100".into())],
    )
    .await
    .expect("/createitem Magmatic Gas");
    sess1.role = saved_role;

    let metenox_item_id = create_drill
        .dict_get("itemID")
        .and_then(|v| v.as_i64())
        .expect("Metenox itemID");
    let fuel_item_id = create_fuel
        .dict_get("itemID")
        .and_then(|v| v.as_i64())
        .expect("Fuel Blocks itemID");
    let gas_item_id = create_gas
        .dict_get("itemID")
        .and_then(|v| v.as_i64())
        .expect("Magmatic Gas itemID");

    let anchor_res = rpc(
        &ctx,
        &mut sess1,
        "structureDeployment",
        "Anchor",
        &[
            PyRep::Integer(metenox_item_id),
            PyRep::String("Asabona Metenox".into()),
        ],
    )
    .await
    .expect("structureDeployment.Anchor Metenox");
    assert_eq!(
        anchor_res.dict_get("quantumCoreInstalled").and_then(|v| match v {
            PyRep::Bool(b) => Some(*b),
            _ => None,
        }),
        Some(true),
        "Metenox pre-fit integrated quantum core (attr 5701 / type 81920) must be active"
    );
    let structure_id = anchor_res
        .dict_get("structureID")
        .and_then(|v| v.as_i64())
        .expect("anchored structureID");

    // Advance past AnchorVulnerable (900s) + Anchoring (86400s) + Onlining (900s) -> ShieldVulnerable
    ctx.advance_secs(900 + 86_400 + 900 + 10);
    let info_rep = rpc(
        &ctx,
        &mut sess1,
        "structureDirectory",
        "GetStructureInfo",
        &[PyRep::Integer(structure_id)],
    )
    .await
    .expect("GetStructureInfo after anchoring/onlining");
    assert_eq!(
        info_rep.dict_get("state").and_then(|v| v.as_str()),
        Some("ShieldVulnerable")
    );

    // Fly ship within 5,000 m of the anchored Metenox and load fuel + magmatic gas via invbroker.Add
    let PyRep::Tuple(bound_bey) = rpc(
        &ctx,
        &mut sess1,
        "beyonce",
        "MachoBindObject",
        &[PyRep::Integer(i64::from(SYSTEM_ID)), PyRep::None],
    )
    .await
    .expect("bind beyonce") else {
        panic!("MachoBindObject tuple expected");
    };
    let bey_bound_id = bound_bey[0].as_i64().unwrap() as u64;
    let kw = HashMap::new();
    ctx.dispatch_rpc(
        &mut sess1,
        "beyonce",
        Some(bey_bound_id),
        "CmdFollowBall",
        &[PyRep::Integer(structure_id), PyRep::Real(0.0)],
        &kw,
        None,
    )
    .await
    .expect("CmdFollowBall toward Metenox");
    ctx.dispatch_rpc(
        &mut sess1,
        "beyonce",
        Some(bey_bound_id),
        "CmdSetSpeedFraction",
        &[PyRep::Real(1.0)],
        &kw,
        None,
    )
    .await
    .expect("CmdSetSpeedFraction");
    let sys_handle = ctx.systems.get_or_spawn(SYSTEM_ID);
    let mut fueled = false;
    for _ in 0..80 {
        for _ in 0..15 {
            let _ = sys_handle.step_manual(1.0).await;
        }
        match rpc(
            &ctx,
            &mut sess1,
            "invbroker",
            "Add",
            &[
                PyRep::Integer(fuel_item_id),
                PyRep::Integer(structure_id),
                PyRep::Integer(133), // SpecializedFuelBay
            ],
        )
        .await
        {
            Ok(_) => {
                fueled = true;
                break;
            }
            Err(e) if e.code == "DestinationAccessDenied" => continue,
            Err(e) => panic!("invbroker.Add fuel blocks unexpected error: {e:?}"),
        }
    }
    assert!(fueled, "Ship must reach Metenox 5 km fuel bay transfer range");
    rpc(
        &ctx,
        &mut sess1,
        "invbroker",
        "Add",
        &[
            PyRep::Integer(gas_item_id),
            PyRep::Integer(structure_id),
            PyRep::Integer(133), // SpecializedFuelBay
        ],
    )
    .await
    .expect("invbroker.Add Magmatic Gas into SpecializedFuelBay");

    // ------------------------------------------------------------------------
    // Part 1: FastCheckoutService Mining RPCs & Auth
    // ------------------------------------------------------------------------
    // 1.1 FastCheckoutService.RequestMiningCycleOutput (authorized pilot 1)
    let cycle_rep = rpc(
        &ctx,
        &mut sess1,
        "FastCheckoutService",
        "RequestMiningCycleOutput",
        &[PyRep::Integer(structure_id)],
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

    // 1.2 FastCheckoutService.RequestMiningDetails before and after live 1-hour Metenox tick
    let details_rep = rpc(
        &ctx,
        &mut sess1,
        "FastCheckoutService",
        "RequestMiningDetails",
        &[PyRep::Integer(structure_id)],
    )
    .await
    .expect("RequestMiningDetails for authorized character");

    match details_rep {
        PyRep::Dict(_) => {
            let fuel = dict_get(&details_rep, "availableFuel")
                .expect("availableFuel key")
                .as_i64()
                .expect("availableFuel is integer");
            assert_eq!(fuel, 200);

            let moon = dict_get(&details_rep, "moonID")
                .expect("moonID key")
                .as_i64()
                .expect("moonID is integer");
            assert!(moon > 0, "Anchored Asabona Metenox must resolve a real moonID");

            let next_harvest = dict_get(&details_rep, "nextHarvestTime")
                .expect("nextHarvestTime key")
                .as_i64()
                .expect("nextHarvestTime is integer");
            assert!(next_harvest > 0);

            let cap_before = match dict_get(&details_rep, "remainingOutputBayCapacity")
                .expect("remainingOutputBayCapacity key")
            {
                PyRep::Real(v) => *v,
                other => panic!("Expected Real remainingOutputBayCapacity, got {other:?}"),
            };
            assert!(
                (cap_before - 500_000.0).abs() < 1e-4,
                "Fresh Metenox output bay capacity should match SDE attr 5693 (500,000 m3), got {cap_before}"
            );

            let mats_in_details = dict_get(&details_rep, "outputMaterials")
                .expect("outputMaterials key");
            assert_eq!(mats_in_details, &PyRep::List(materials_list));

            // Advance server clock by 1 hour to run a genuine live Metenox mining cycle
            ctx.advance_secs(3600);

            let details_after = rpc(
                &ctx,
                &mut sess1,
                "FastCheckoutService",
                "RequestMiningDetails",
                &[PyRep::Integer(structure_id)],
            )
            .await
            .expect("RequestMiningDetails after 1h cycle");
            let fuel_after = dict_get(&details_after, "availableFuel")
                .unwrap()
                .as_i64()
                .unwrap();
            assert_eq!(
                fuel_after, 195,
                "Live 1h Metenox cycle must consume 5 fuel blocks from SpecializedFuelBay (200 -> 195)"
            );

            let cap_after = match dict_get(&details_after, "remainingOutputBayCapacity").unwrap() {
                PyRep::Real(v) => *v,
                _ => panic!("Expected Real remainingOutputBayCapacity"),
            };
            assert!(
                cap_after < cap_before,
                "Harvested moon materials in SpecializedMineralHold must reduce remainingOutputBayCapacity ({cap_before} -> {cap_after})"
            );
        }
        other => panic!("Expected PyRep::Dict from RequestMiningDetails, got {other:?}"),
    }

    // 1.3 Authorization checks: pilot 2 (unauthorized corp) receives AccessDenied
    let denied_details = rpc(
        &ctx,
        &mut sess2,
        "FastCheckoutService",
        "RequestMiningDetails",
        &[PyRep::Integer(structure_id)],
    )
    .await
    .expect_err("Unauthorized character should be denied mining details");
    assert_eq!(denied_details.code, "AccessDenied");

    let denied_cycle = rpc(
        &ctx,
        &mut sess2,
        "FastCheckoutService",
        "RequestMiningCycleOutput",
        &[PyRep::Integer(structure_id)],
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
    let db_path = std::env::temp_dir().join(format!(
        "evomu_fast_checkout_reload_{}.sqlite",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&db_path);
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let mut sess = login_pilot_char(&ctx, 1205, "demo", "demo-secret", CHAR_ID).await;
    let user_id = sess.user_id.expect("user_id");

    common::seed_fast_checkout_plex_offer(&ctx, user_id, 1001, 110, 499);
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

    // Persist to SQLite and reopen repository (which executes fold_legacy_plex_balances on load)
    ctx.db.save_to_sqlite(&db_path).expect("save_to_sqlite");
    let now_unix = ctx.now_unix();
    drop(ctx);

    let db_reloaded = Arc::new(WorldRepository::open_sqlite(&db_path).expect("reopen_sqlite"));
    let ctx_reloaded = ServerContext::with_repository(db_reloaded);
    ctx_reloaded.set_clock(Arc::new(VirtualServerClock::with_fixed_base(now_unix)));
    let vault_after_reload = ctx_reloaded
        .db
        .with_snapshot(|snap| snap.account_plex_vault.get(&user_id).copied().unwrap_or(0));
    assert_eq!(vault_after_reload, vault_after, "SQLite reload must not double credit PLEX");
    drop(ctx_reloaded);
    let _ = std::fs::remove_file(&db_path);
}
