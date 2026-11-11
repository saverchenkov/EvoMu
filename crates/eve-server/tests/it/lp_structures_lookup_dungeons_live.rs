//! Live integration tests for LP Store, Structure Docking ACLs, Warable Lookup,
//! NPC-NPC Standings, Homefront Site Instances, and Escalating Path Details.

use crate::common::{self, LivePilot};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use std::collections::HashMap;

const JITA: u32 = 30_000_142;
const PERIMETER: u32 = 30_000_144;
const CALDARI_NAVY_CORP: i64 = 1_000_035;
const ASTRAHUS_TYPE: u32 = 35_832;
const VELATOR_TYPE: u32 = 606;
const CHARON_TYPE: u32 = 20_185;

fn paused_ctx() -> ServerContext {
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    ctx
}

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, svc, None, method, args, &HashMap::new(), None).await
}

async fn ok(ctx: &ServerContext, p: &mut LivePilot, svc: &str, method: &str, args: &[PyRep]) -> PyRep {
    rpc(ctx, &mut p.sess, svc, method, args)
        .await
        .unwrap_or_else(|e| panic!("{svc}.{method} failed: {e:?}"))
}

fn id_from(rep: &PyRep, key: &str) -> i64 {
    rep.as_i64()
        .or_else(|| rep.dict_get(key).and_then(|v| v.as_i64()))
        .unwrap_or_else(|| panic!("Expected integer or dict[{key}] in {rep:?}"))
}

fn extract_rowset_rows(rep: &PyRep) -> (&[String], &[Vec<PyRep>]) {
    match rep {
        PyRep::Rowset { columns, rows } => (columns.as_slice(), rows.as_slice()),
        other => panic!("Expected PyRep::Rowset, got {other:?}"),
    }
}

#[tokio::test]
async fn test_lp_store_personal_and_corp_purchase_live() {
    // parity: feat:lp.lp-store-buy
    // parity: rpc:LPStoreMgr.TakeOfferForCorporation
    eve_testkit::attest(&["feat:lp.lp-store-buy", "rpc:LPStoreMgr.TakeOfferForCorporation"]);

    let ctx = paused_ctx();
    let agent = ctx
        .sde
        .agents
        .values()
        .filter(|a| {
            a.corporation_id == CALDARI_NAVY_CORP
                && a.level == 1
                && a.agent_type_id == 2
                && a.division_id == 24
                && a.station_id.is_some()
                && a.solar_system_id.is_some()
        })
        .min_by_key(|a| a.agent_id)
        .cloned()
        .expect("Caldari Navy L1 security agent in SDE");
    let caldari_agent_id = agent.agent_id;
    let agent_sys_id = agent.solar_system_id.unwrap();
    let station_id = agent.station_id.unwrap();

    let mut pilot =
        common::gm_pilot_in_space(&ctx, 101, 91_500_001, VELATOR_TYPE, agent_sys_id).await;
    common::slash_cmd(&ctx, &mut pilot.sess, "/giveisk 500000000").await;

    let sys = ctx.systems.get_or_spawn(agent_sys_id);
    ctx.dispatch_rpc(
        &mut pilot.sess,
        "beyonce",
        Some(pilot.beyonce),
        "CmdWarpToStuff",
        &[PyRep::String("item".into()), PyRep::Integer(station_id)],
        &HashMap::new(),
        None,
    )
    .await
    .expect("CmdWarpToStuff");
    for _ in 0..120 {
        sys.step_manual(1.0).await.unwrap();
    }
    ok(&ctx, &mut pilot, "stationSvc", "Dock", &[PyRep::Integer(station_id)]).await;

    // Query real Tranquility ESI LP offers for Caldari Navy (1_000_035)
    let PyRep::List(offers) = ok(
        &ctx,
        &mut pilot,
        "LPStoreMgr",
        "GetAvailableOffersFromCorp",
        &[PyRep::Integer(CALDARI_NAVY_CORP)],
    )
    .await
    else {
        panic!("Expected List of LP offers");
    };
    assert!(!offers.is_empty(), "Caldari Navy LP store must load real offers without test seeding");

    let chosen = offers
        .iter()
        .find(|o| {
            let lp = o.dict_get("lpCost").and_then(|v| v.as_i64()).unwrap_or(i64::MAX);
            lp > 0 && lp <= 5_000
        })
        .expect("Expected at least one Caldari Navy offer with lpCost <= 5000");
    let offer_id = chosen.dict_get("offerID").and_then(|v| v.as_i64()).unwrap();
    let lp_cost = chosen.dict_get("lpCost").and_then(|v| v.as_i64()).unwrap();
    let isk_cost = chosen.dict_get("iskCost").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let reward_type_id = chosen.dict_get("typeID").and_then(|v| v.as_i64()).unwrap() as u32;
    let reward_qty = chosen.dict_get("qty").and_then(|v| v.as_i64()).unwrap_or(1) as i32;

    if let Some(PyRep::List(reqs)) = chosen.dict_get("reqItems") {
        for r in reqs {
            if let PyRep::Tuple(pair) = r {
                let (tid, qty) = (pair[0].as_i64().unwrap(), pair[1].as_i64().unwrap());
                common::slash_cmd(&ctx, &mut pilot.sess, &format!("/giveitem {tid} {qty}")).await;
            }
        }
    }

    // Earn Caldari Navy LP via real L1 agent missions
    for _ in 0..45 {
        let lp_bals = ok(&ctx, &mut pilot, "LPSvc", "GetAllMyCharacterWalletLPBalances", &[]).await;
        let cur_lp = match &lp_bals {
            PyRep::List(items) => items
                .iter()
                .find_map(|it| match it {
                    PyRep::Tuple(t) if t.first().and_then(|v| v.as_i64()) == Some(CALDARI_NAVY_CORP) => {
                        t.get(1).and_then(|v| v.as_i64())
                    }
                    _ => (it.dict_get("corpID").and_then(|v| v.as_i64()) == Some(CALDARI_NAVY_CORP))
                        .then(|| it.dict_get("lpAmount").or_else(|| it.dict_get("loyaltyPoints")).and_then(|v| v.as_i64()))
                        .flatten(),
                })
                .unwrap_or(0),
            _ => 0,
        };
        if cur_lp >= lp_cost * 3 {
            break;
        }
        let offer = ok(&ctx, &mut pilot, "agentMgr", "DoAction", &[PyRep::Integer(1), PyRep::Integer(caldari_agent_id)]).await;
        let mid = offer.dict_get("missionID").and_then(|v| v.as_i64()).unwrap();
        ok(&ctx, &mut pilot, "agentMgr", "DoAction", &[PyRep::Integer(3), PyRep::Integer(mid)]).await;
        let m_npcs: Vec<i64> = ctx.db.with_snapshot(|snap| {
            snap.active_missions
                .get(&(pilot.char_id, mid as u32))
                .map(|m| m.spawned_npc_ball_ids.clone())
                .unwrap_or_default()
        });
        for nid in m_npcs {
            let _ = sys
                .apply_damage_with_attacker(
                    nid,
                    eve_destiny::DamageVector { em: 1.0e7, thermal: 1.0e7, kinetic: 1.0e7, explosive: 1.0e7 },
                    Some(pilot.char_id),
                )
                .await;
        }
        ok(&ctx, &mut pilot, "agentMgr", "DoAction", &[PyRep::Integer(6), PyRep::Integer(mid)]).await;
    }

    let isk_before = ok(&ctx, &mut pilot, "account", "GetCashBalance", &[]).await.as_f64().unwrap();

    // 1. Personal LP store purchase (`feat:lp.lp-store-buy`)
    ok(
        &ctx,
        &mut pilot,
        "LPStoreMgr",
        "TakeOfferForCharacter",
        &[PyRep::Integer(CALDARI_NAVY_CORP), PyRep::Integer(offer_id), PyRep::Integer(1)],
    )
    .await;

    let isk_after = ok(&ctx, &mut pilot, "account", "GetCashBalance", &[]).await.as_f64().unwrap();
    assert!((isk_before - isk_after - (isk_cost as f64)).abs() < 1.0, "Personal ISK must decrease by iskCost");

    let hangar_items: i32 = ctx.db.with_snapshot(|snap| {
        snap.items
            .values()
            .filter(|i| i.owner_id == pilot.char_id && i.location_id == station_id && i.type_id == reward_type_id)
            .map(|i| i.quantity)
            .sum()
    });
    assert!(hangar_items >= reward_qty, "Delivered LP reward item must be in personal hangar");

    // 2. Corporation LP store purchase (`rpc:LPStoreMgr.TakeOfferForCorporation`)
    common::slash_cmd(&ctx, &mut pilot.sess, "/giveskill 3363 5").await;
    ctx.advance_secs(12);
    let corp_id = id_from(
        &ok(&ctx, &mut pilot, "corpRegistry", "AddCorporation", &[PyRep::String("LP Test Corp".into()), PyRep::String("LPTC".into())]).await,
        "corporationID",
    );

    ok(
        &ctx,
        &mut pilot,
        "LPSvc",
        "TransferLPFromMyWalletToOtherCorp",
        &[PyRep::Integer(corp_id), PyRep::Integer(CALDARI_NAVY_CORP), PyRep::Integer(lp_cost)],
    )
    .await;
    ok(
        &ctx,
        &mut pilot,
        "account",
        "GiveCash",
        &[PyRep::Integer(corp_id), PyRep::Real(isk_cost.max(10_000.0) * 2.0), PyRep::String("LP funding".into())],
    )
    .await;

    if let Some(PyRep::List(reqs)) = chosen.dict_get("reqItems") {
        for r in reqs {
            if let PyRep::Tuple(pair) = r {
                let (tid, qty) = (pair[0].as_i64().unwrap() as u32, pair[1].as_i64().unwrap());
                common::slash_cmd(&ctx, &mut pilot.sess, &format!("/giveitem {tid} {qty}")).await;
                let item_id = ctx.db.with_snapshot(|snap| {
                    snap.items
                        .values()
                        .find(|i| i.owner_id == pilot.char_id && i.location_id == station_id && i.type_id == tid)
                        .map(|i| i.item_id)
                        .unwrap()
                });
                ok(&ctx, &mut pilot, "invbroker", "MoveItems", &[PyRep::List(vec![PyRep::Integer(item_id)]), PyRep::Integer(station_id), PyRep::Integer(62)]).await;
            }
        }
    }

    let mut member =
        common::gm_pilot_in_space(&ctx, 102, 91_500_002, VELATOR_TYPE, agent_sys_id).await;
    ctx.dispatch_rpc(
        &mut member.sess,
        "beyonce",
        Some(member.beyonce),
        "CmdWarpToStuff",
        &[PyRep::String("item".into()), PyRep::Integer(station_id)],
        &HashMap::new(),
        None,
    )
    .await
    .expect("member CmdWarpToStuff");
    for _ in 0..120 {
        sys.step_manual(1.0).await.unwrap();
    }
    ok(&ctx, &mut member, "stationSvc", "Dock", &[PyRep::Integer(station_id)]).await;
    let app_rep = ok(
        &ctx,
        &mut member,
        "corpRegistry",
        "InsertApplication",
        &[PyRep::Integer(corp_id), PyRep::String("join".into())],
    )
    .await;
    let app_id = id_from(&app_rep, "applicationID");
    ctx.advance_secs(12);
    ok(
        &ctx,
        &mut pilot,
        "corpRegistry",
        "UpdateApplicationOffer",
        &[PyRep::Integer(app_id), PyRep::String("Accepted".into())],
    )
    .await;
    member.sess.corporation_id = Some(corp_id);
    member.sess.corp_role = 0;
    let err = rpc(
        &ctx,
        &mut member.sess,
        "LPStoreMgr",
        "TakeOfferForCorporation",
        &[PyRep::Integer(CALDARI_NAVY_CORP), PyRep::Integer(offer_id), PyRep::Integer(1)],
    )
    .await
    .expect_err("Unprivileged member must be rejected");
    assert_eq!(err.code, "InsufficientCorporationRole", "unexpected error: {err:?}");

    ok(
        &ctx,
        &mut pilot,
        "LPStoreMgr",
        "TakeOfferForCorporation",
        &[PyRep::Integer(CALDARI_NAVY_CORP), PyRep::Integer(offer_id), PyRep::Integer(1)],
    )
    .await;

    let corp_delivered: i32 = ctx.db.with_snapshot(|snap| {
        snap.items
            .values()
            .filter(|i| i.owner_id == corp_id && i.location_id == station_id && i.type_id == reward_type_id)
            .map(|i| i.quantity)
            .sum()
    });
    assert!(corp_delivered >= reward_qty, "Corporation LP reward item must be delivered to corp");
}

#[tokio::test]
async fn test_structures_docking_acl_live() {
    // parity: feat:structures.docking-acl
    eve_testkit::attest(&["feat:structures.docking-acl"]);

    let ctx = paused_ctx();
    let mut owner = common::gm_pilot_in_space(&ctx, 201, 91_500_011, CHARON_TYPE, PERIMETER).await;
    let mut visitor = common::gm_pilot_in_space(&ctx, 202, 91_500_012, VELATOR_TYPE, PERIMETER).await;

    common::slash_cmd(&ctx, &mut owner.sess, "/giveskill 3363 5").await;
    let saved_owner_st = owner.sess.station_id;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&owner.char_id) {
            ch.station_id = Some(60_003_760);
        }
    });
    owner.sess.station_id = Some(60_003_760);
    ok(&ctx, &mut owner, "corpRegistry", "AddCorporation", &[PyRep::String("ACL Structure Corp".into()), PyRep::String("ACLS".into())]).await;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&owner.char_id) {
            ch.station_id = saved_owner_st;
        }
    });
    owner.sess.station_id = saved_owner_st;
    ctx.advance_secs(7 * 86_400 + 30);

    common::stock_ship_bay(&ctx, &mut owner, ASTRAHUS_TYPE, 1, 5).await;
    let sid = id_from(
        &ok(&ctx, &mut owner, "structureDeployment", "Anchor", &[PyRep::Integer(ASTRAHUS_TYPE as i64), PyRep::String("ACL Test Astrahus".into())]).await,
        "structureID",
    );
    ctx.advance_secs(25 * 3_600);

    let group_id = id_from(
        &ok(&ctx, &mut owner, "ownerGroupManager", "Create", &[PyRep::String("Docking Whitelist".into()), PyRep::String("Allowed".into())]).await,
        "groupID",
    );
    let owner_id = owner.char_id;
    ok(
        &ctx,
        &mut owner,
        "ownerGroupManager",
        "AddMembers",
        &[PyRep::Integer(group_id), PyRep::List(vec![PyRep::Integer(owner_id)]), PyRep::Integer(1)],
    )
    .await;
    let profile_id = ok(
        &ctx,
        &mut owner,
        "structureProfiles",
        "CreateProfile",
        &[PyRep::String("Restricted Docking Profile".into()), PyRep::String("Whitelist only".into())],
    )
    .await
    .as_i64()
    .unwrap();

    let patch = PyRep::Dict(vec![(
        PyRep::Integer(1),
        PyRep::List(vec![PyRep::keyval([
            ("groupID", PyRep::Integer(group_id)),
            ("value", PyRep::Integer(1)),
        ])]),
    )]);
    ok(&ctx, &mut owner, "structureProfiles", "SaveProfileSettings", &[PyRep::Integer(profile_id), patch]).await;
    ok(&ctx, &mut owner, "structureProfiles", "ChangeProfiles", &[PyRep::List(vec![PyRep::Integer(sid)]), PyRep::Integer(profile_id)]).await;
    ctx.advance_secs(305);

    let st_pos = ctx.db.with_snapshot(|snap| glam::DVec3::from_array(snap.structures.get(&sid).unwrap().position));
    common::fly_to(&ctx, &mut visitor, st_pos).await;

    let err_denied = rpc(&ctx, &mut visitor.sess, "stationSvc", "Dock", &[PyRep::Integer(sid)])
        .await
        .expect_err("Visitor not in docking ACL must be denied by stationSvc.Dock");
    assert_eq!(err_denied.code, "StructureDockingAccessDenied");

    ok(
        &ctx,
        &mut owner,
        "ownerGroupManager",
        "AddMembers",
        &[PyRep::Integer(group_id), PyRep::List(vec![PyRep::Integer(visitor.char_id)]), PyRep::Integer(1)],
    )
    .await;

    ok(&ctx, &mut visitor, "stationSvc", "Dock", &[PyRep::Integer(sid)]).await;
    assert_eq!(visitor.sess.structure_id, Some(sid));
}

#[tokio::test]
async fn test_lookup_warable_and_npc_standings_live() {
    // parity: feat:ui.lookup
    // parity: rpc:standingMgr.GetNPCNPCStandings
    eve_testkit::attest(&["feat:ui.lookup", "rpc:standingMgr.GetNPCNPCStandings"]);

    let ctx = paused_ctx();
    let mut pilot_a = common::gm_pilot_in_space(&ctx, 301, 91_500_021, CHARON_TYPE, PERIMETER).await;
    let mut pilot_b = common::gm_pilot_in_space(&ctx, 302, 91_500_022, CHARON_TYPE, PERIMETER).await;
    common::slash_cmd(&ctx, &mut pilot_a.sess, "/giveskill 3363 5").await;
    common::slash_cmd(&ctx, &mut pilot_b.sess, "/giveskill 3363 5").await;

    let saved_a = pilot_a.sess.station_id;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&pilot_a.char_id) {
            ch.station_id = Some(60_003_760);
        }
    });
    pilot_a.sess.station_id = Some(60_003_760);
    let corp_a = id_from(
        &ok(&ctx, &mut pilot_a, "corpRegistry", "AddCorporation", &[PyRep::String("Alpha War Corp".into()), PyRep::String("ALPH".into())]).await,
        "corporationID",
    );
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&pilot_a.char_id) {
            ch.station_id = saved_a;
        }
    });
    pilot_a.sess.station_id = saved_a;
    let saved_b = pilot_b.sess.station_id;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&pilot_b.char_id) {
            ch.station_id = Some(60_003_760);
        }
    });
    pilot_b.sess.station_id = Some(60_003_760);
    let corp_b = id_from(
        &ok(&ctx, &mut pilot_b, "corpRegistry", "AddCorporation", &[PyRep::String("Beta Target Corp".into()), PyRep::String("BETA".into())]).await,
        "corporationID",
    );
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&pilot_b.char_id) {
            ch.station_id = saved_b;
        }
    });
    pilot_b.sess.station_id = saved_b;

    let PyRep::List(before_list) = ok(
        &ctx,
        &mut pilot_a,
        "lookupSvc",
        "LookupWarableCorporationsOrAlliances",
        &[PyRep::String("Beta".into()), PyRep::Bool(false)],
    )
    .await
    else {
        panic!("Expected List");
    };
    assert!(before_list.is_empty(), "Corp without Upwell structure must not be warable");

    ctx.advance_secs(7 * 86_400 + 30);
    common::stock_ship_bay(&ctx, &mut pilot_b, ASTRAHUS_TYPE, 1, 5).await;
    ok(&ctx, &mut pilot_b, "structureDeployment", "Anchor", &[PyRep::Integer(ASTRAHUS_TYPE as i64), PyRep::String("Beta Fort".into())]).await;

    let PyRep::List(after_list) = ok(
        &ctx,
        &mut pilot_a,
        "lookupSvc",
        "LookupWarableCorporationsOrAlliances",
        &[PyRep::String("Beta".into()), PyRep::Bool(false)],
    )
    .await
    else {
        panic!("Expected List");
    };
    assert!(
        after_list.iter().any(|r| r.dict_get("ownerID").and_then(|v| v.as_i64()) == Some(corp_b)),
        "War-eligible Corp B must be returned"
    );

    let PyRep::List(self_list) = ok(
        &ctx,
        &mut pilot_a,
        "lookupSvc",
        "LookupWarableCorporationsOrAlliances",
        &[PyRep::String("Alpha".into()), PyRep::Bool(false)],
    )
    .await
    else {
        panic!("Expected List");
    };
    assert!(
        !self_list.iter().any(|r| r.dict_get("ownerID").and_then(|v| v.as_i64()) == Some(corp_a)),
        "Caller's own corporation must be excluded"
    );

    let PyRep::List(char_list) = ok(
        &ctx,
        &mut pilot_a,
        "lookupSvc",
        "LookupCharacters",
        &[PyRep::String("".into()), PyRep::Bool(false)],
    )
    .await
    else {
        panic!("Expected List from LookupCharacters");
    };
    assert!(
        char_list.iter().any(|r| r.dict_get("characterID").and_then(|v| v.as_i64()) == Some(pilot_a.char_id)),
        "Live character must appear in LookupCharacters"
    );

    let PyRep::List(corp_list) = ok(
        &ctx,
        &mut pilot_a,
        "lookupSvc",
        "LookupCorporations",
        &[PyRep::String("Beta Target".into()), PyRep::Bool(false)],
    )
    .await
    else {
        panic!("Expected List from LookupCorporations");
    };
    assert_eq!(
        corp_list.first().and_then(|r| r.dict_get("corporationID")).and_then(|v| v.as_i64()),
        Some(corp_b)
    );

    let PyRep::List(owner_list) = ok(
        &ctx,
        &mut pilot_a,
        "lookupSvc",
        "LookupOwners",
        &[PyRep::String("Beta Target".into()), PyRep::Bool(false)],
    )
    .await
    else {
        panic!("Expected List from LookupOwners");
    };
    assert!(
        owner_list.iter().any(|r| r.dict_get("ownerID").and_then(|v| v.as_i64()) == Some(corp_b)),
        "Live corporation must appear in LookupOwners"
    );

    let PyRep::List(loc_list) = ok(
        &ctx,
        &mut pilot_a,
        "lookupSvc",
        "LookupLocationsByGroup",
        &[PyRep::Integer(5), PyRep::String("Jita".into())],
    )
    .await
    else {
        panic!("Expected List from LookupLocationsByGroup");
    };
    assert!(
        loc_list.iter().any(|r| r.dict_get("itemID").and_then(|v| v.as_i64()) == Some(JITA as i64)),
        "Jita must appear in LookupLocationsByGroup(5, 'Jita')"
    );

    let mut unauth = SessionState::new(399);
    rpc(&ctx, &mut unauth, "standingMgr", "GetNPCNPCStandings", &[])
        .await
        .expect_err("Unauthenticated GetNPCNPCStandings must fail");

    let st_rep = ok(&ctx, &mut pilot_a, "standingMgr", "GetNPCNPCStandings", &[]).await;
    let (cols, rows) = extract_rowset_rows(&st_rep);
    assert_eq!(cols, &["fromID", "toID", "standing"]);
    assert!(rows.len() >= 50, "Expected >= 50 NPC-NPC standing rows");
    let amarr_caldari = rows.iter().find(|r| matches!((r.first(), r.get(1)), (Some(PyRep::Integer(500_001)), Some(PyRep::Integer(500_003)))));
    assert!(
        matches!(amarr_caldari.and_then(|r| r.get(2)), Some(PyRep::Real(v)) if (*v - 7.0).abs() < 1e-6),
        "Expected Amarr -> Caldari standing +7.0"
    );
}

#[tokio::test]
async fn test_homefront_sites_and_escalations_live() {
    // parity: rpc:dungeonExplorationMgr.GetMyEscalatingPathDetails
    eve_testkit::attest(&["rpc:dungeonExplorationMgr.GetMyEscalatingPathDetails"]);

    let ctx = paused_ctx();
    let mut pilot = common::gm_pilot_in_space(&ctx, 401, 91_500_031, VELATOR_TYPE, JITA).await;

    let PyRep::Dict(sys_map) = ok(
        &ctx,
        &mut pilot,
        "dungeonInstanceCacheMgr",
        "GetHomefrontSiteInstances",
        &[PyRep::Integer(JITA as i64)],
    )
    .await
    else {
        panic!("Expected Dict");
    };
    let PyRep::List(d_map) = sys_map.iter().find(|(k, _)| k.as_i64() == Some(JITA as i64)).map(|(_, v)| v).unwrap() else {
        panic!("Expected inner List");
    };
    assert!(!d_map.is_empty(), "Jita must have active SDE archetype-70 Homefront site instances");

    let PyRep::Dict(other_map) = ok(
        &ctx,
        &mut pilot,
        "dungeonInstanceCacheMgr",
        "GetHomefrontSiteInstances",
        &[PyRep::Integer(PERIMETER as i64)],
    )
    .await
    else {
        panic!("Expected Dict");
    };
    let inner_other = other_map
        .iter()
        .find(|(k, _)| k.as_i64() == Some(PERIMETER as i64))
        .and_then(|(_, v)| match v {
            PyRep::List(m) => Some(m.len()),
            _ => None,
        })
        .unwrap_or(0);
    assert_eq!(inner_other, 0, "Non-homefront system must not lazily seed fixtures");

    let esc_empty = ok(&ctx, &mut pilot, "dungeonExplorationMgr", "GetMyEscalatingPathDetails", &[]).await;
    let (_, empty_rows) = extract_rowset_rows(&esc_empty);
    assert!(empty_rows.is_empty(), "Fresh pilot must have 0 escalations without auto-seeded fixtures");

    let PyRep::List(anoms) = ok(&ctx, &mut pilot, "scanSvc", "GetCombatAnomalyInstances", &[]).await else {
        panic!("Expected List of anomalies");
    };
    let site_id = anoms
        .first()
        .and_then(|a| a.dict_get("siteID").or_else(|| a.dict_get("id")).and_then(|v| v.as_str()))
        .expect("At least one combat anomaly in Jita")
        .to_string();

    ctx.set_mechanic_rng_seed(Some(3));
    let sys_h = ctx.systems.get_or_spawn(JITA);
    for _ in 0..8 {
        let cur_anoms = ok(&ctx, &mut pilot, "scanSvc", "GetCombatAnomalyInstances", &[]).await;
        let site_opt = match cur_anoms {
            PyRep::List(list) => list.into_iter().find(|a| {
                a.dict_get("siteID").or_else(|| a.dict_get("id")).and_then(|v| v.as_str()) == Some(site_id.as_str())
            }),
            _ => None,
        };
        let Some(site_dict) = site_opt else { break };
        let npc_ids: Vec<i64> = match site_dict.dict_get("activeNpcBallIDs") {
            Some(PyRep::List(ids)) => ids.iter().filter_map(|v| v.as_i64()).collect(),
            _ => Vec::new(),
        };
        if npc_ids.is_empty() {
            if rpc(&ctx, &mut pilot.sess, "scanSvc", "EnterCombatAnomaly", &[PyRep::String(site_id.clone()), PyRep::Bool(true)])
                .await
                .is_err()
            {
                break;
            }
            continue;
        }
        for nid in npc_ids {
            let _ = sys_h
                .apply_damage_with_attacker(
                    nid,
                    eve_destiny::DamageVector { em: 1.0e7, thermal: 1.0e7, kinetic: 1.0e7, explosive: 1.0e7 },
                    Some(pilot.char_id),
                )
                .await;
        }
    }

    let esc_active = ok(&ctx, &mut pilot, "dungeonExplorationMgr", "GetMyEscalatingPathDetails", &[]).await;
    let (_, active_rows) = extract_rowset_rows(&esc_active);
    assert!(!active_rows.is_empty(), "Clearing combat anomaly must trigger a real escalation");
}
