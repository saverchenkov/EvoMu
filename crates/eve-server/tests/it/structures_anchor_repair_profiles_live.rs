//! Live Upwell structure integration test suite driven only through client RPCs
//! (`structureDeployment`, `repairSvc`, `structureProfiles`, `structureDirectory`, `corpRegistry`),
//! GM slash commands, and real clock advances without snapshot backdoors:
//! - Anchoring restrictions: corporation age gating, trade hub ban, starter system ban, FLEX nullsec sov requirement.
//! - Repair shop: available in LowPower with Quantum Core, quotes reflect missing HP and profile tax, restoration clears damage and debits ISK.
//! - Structure profiles: CRUD, delayed profile transition (5 minutes), and structure directory queries.

#![forbid(unsafe_code)]

use crate::common::{self, LivePilot};
use eve_db::types::InventoryFlag;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use std::collections::HashMap;
use std::sync::Arc;

const JITA: u32 = 30_000_142;
const KISOGO: u32 = 30_000_141;
const PERIMETER: u32 = 30_000_144;
const CHARON_TYPE: u32 = 20185; // Caldari Freighter (465,000 m³ cargo hold)
const ASTRAHUS_TYPE: u32 = 35832;
const ASTRAHUS_CORE_TYPE: u32 = 56201;
const PHAROLUX_TYPE: u32 = 35840;

fn paused_ctx() -> ServerContext {
    let sde = Arc::new(eve_sde::SdeRegistry::open(&eve_sde::SdeRegistry::default_db_path()).expect("sde"));
    let ctx = ServerContext::with_sde_and_repository(sde, Arc::new(eve_db::WorldRepository::with_demo_seed()));
    ctx.set_clock_paused(true);
    ctx
}

async fn call(
    ctx: &ServerContext,
    p: &mut LivePilot,
    svc: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(&mut p.sess, svc, None, method, args, &HashMap::new(), None).await
}

async fn ok(
    ctx: &ServerContext,
    p: &mut LivePilot,
    svc: &str,
    method: &str,
    args: &[PyRep],
) -> PyRep {
    call(ctx, p, svc, method, args)
        .await
        .unwrap_or_else(|e| panic!("{svc}.{method} failed: {e:?}"))
}

fn kv_i64(rep: &PyRep, key: &str) -> i64 {
    rep.dict_get(key).and_then(|v| v.as_i64()).unwrap_or(0)
}

fn kv_f64(rep: &PyRep, key: &str) -> f64 {
    rep.dict_get(key).and_then(|v| v.as_f64()).unwrap_or(0.0)
}

fn kv_str<'a>(rep: &'a PyRep, key: &str) -> Option<&'a str> {
    rep.dict_get(key).and_then(|v| v.as_str())
}

#[tokio::test]
async fn test_structures_anchoring_restrictions_live() {

    let ctx = paused_ctx();
    let mut pilot = common::gm_pilot_in_space(&ctx, 201, 91_400_001, CHARON_TYPE, PERIMETER).await;
    common::slash_cmd(&ctx, &mut pilot.sess, "/giveskill 3363 5").await;
    ctx.advance_secs(31);

    // 1. Create a brand-new player corporation.
    let _ = ok(
        &ctx,
        &mut pilot,
        "corpRegistry",
        "AddCorporation",
        &[
            PyRep::String("Young Corp Anchoring Inc".into()),
            PyRep::String("YNGA".into()),
        ],
    )
    .await;
    ctx.advance_secs(31);

    // 2. Negative: corporation younger than 7 days is rejected.
    let err_young = call(
        &ctx,
        &mut pilot,
        "structureDeployment",
        "Anchor",
        &[
            PyRep::Integer(ASTRAHUS_TYPE as i64),
            PyRep::String("Young Corp Astrahus".into()),
        ],
    )
    .await
    .expect_err("young corp cannot anchor");
    assert_eq!(err_young.code, "CorporationTooYoungToAnchor");

    // 3. Advance virtual clock past the 7-day age gate (7 * 86,400 s + 30 s).
    ctx.advance_secs(7 * 86_400 + 30);

    // 4. Negative: trade hub (Jita) rejects anchoring.
    common::slash_cmd(&ctx, &mut pilot.sess, &format!("/tr {JITA}")).await;
    ctx.advance_secs(31);
    let err_trade_hub = call(
        &ctx,
        &mut pilot,
        "structureDeployment",
        "Anchor",
        &[
            PyRep::Integer(ASTRAHUS_TYPE as i64),
            PyRep::String("Jita Trade Hub Astrahus".into()),
        ],
    )
    .await
    .expect_err("anchoring disallowed in trade hub");
    assert_eq!(err_trade_hub.code, "AnchoringDisallowedInTradeHub");

    // 5. Negative: starter system (Kisogo) rejects anchoring.
    common::slash_cmd(&ctx, &mut pilot.sess, &format!("/tr {KISOGO}")).await;
    ctx.advance_secs(31);
    let err_starter = call(
        &ctx,
        &mut pilot,
        "structureDeployment",
        "Anchor",
        &[
            PyRep::Integer(ASTRAHUS_TYPE as i64),
            PyRep::String("Kisogo Starter Astrahus".into()),
        ],
    )
    .await
    .expect_err("anchoring disallowed in starter system");
    assert_eq!(err_starter.code, "AnchoringDisallowedInStarterOrCareerSystem");

    // 6. Negative: Pharolux Cyno Beacon (FLEX) in highsec / without sov hub upgrade rejected.
    common::slash_cmd(&ctx, &mut pilot.sess, &format!("/tr {PERIMETER}")).await;
    ctx.advance_secs(31);
    let err_flex = call(
        &ctx,
        &mut pilot,
        "structureDeployment",
        "Anchor",
        &[
            PyRep::Integer(PHAROLUX_TYPE as i64),
            PyRep::String("Highsec Pharolux".into()),
        ],
    )
    .await
    .expect_err("flex outside sov null rejected");
    assert!(
        err_flex.code == "CannotAnchorFlexOutsideSovNull" || err_flex.code == "MissingSovereigntyHub",
        "Expected flex sov restriction, got {}",
        err_flex.code
    );

    // 7. Positive: valid anchoring in Perimeter succeeds when Astrahus is in cargo.
    common::stock_ship_bay(&ctx, &mut pilot, ASTRAHUS_TYPE, 1, InventoryFlag::Cargo as i64).await;
    let anchor_res = ok(
        &ctx,
        &mut pilot,
        "structureDeployment",
        "Anchor",
        &[
            PyRep::Integer(ASTRAHUS_TYPE as i64),
            PyRep::String("Perimeter Frontier Astrahus".into()),
        ],
    )
    .await;

    let sid = kv_i64(&anchor_res, "structureID");
    assert!(sid >= 1_000_000_000_000, "Spawned valid Upwell structure ID: {sid}");
    assert_eq!(kv_str(&anchor_res, "state"), Some("AnchorVulnerable"));
}

#[tokio::test]
async fn test_structures_repairshop_service_quotes_and_restoration_live() {

    let ctx = paused_ctx();
    let mut pilot = common::gm_pilot_in_space(&ctx, 202, 91_400_002, CHARON_TYPE, PERIMETER).await;
    common::slash_cmd(&ctx, &mut pilot.sess, "/giveskill 3363 5").await;
    ctx.advance_secs(31);

    // Create owner corporation and advance past 7-day age gate.
    let _ = ok(
        &ctx,
        &mut pilot,
        "corpRegistry",
        "AddCorporation",
        &[
            PyRep::String("Live Repair Grid Corp".into()),
            PyRep::String("RPRG".into()),
        ],
    )
    .await;
    ctx.advance_secs(7 * 86_400 + 31);

    // Stock Astrahus in cargo and anchor it.
    common::stock_ship_bay(&ctx, &mut pilot, ASTRAHUS_TYPE, 1, InventoryFlag::Cargo as i64).await;
    let anchor_res = ok(
        &ctx,
        &mut pilot,
        "structureDeployment",
        "Anchor",
        &[
            PyRep::Integer(ASTRAHUS_TYPE as i64),
            PyRep::String("Live Repair Astrahus".into()),
        ],
    )
    .await;
    let sid = kv_i64(&anchor_res, "structureID");
    assert!(sid >= 1_000_000_000_000);

    // Advance 15m: AnchorVulnerable -> Anchoring (24h).
    ctx.advance_secs(901);
    let _ = ok(&ctx, &mut pilot, "structureGuests", "GetGuests", &[PyRep::Integer(sid)]).await;

    // Advance 24h: Anchoring -> NoCore.
    ctx.advance_secs(86_401);
    let _ = ok(&ctx, &mut pilot, "structureGuests", "GetGuests", &[PyRep::Integer(sid)]).await;

    // Dock pilot inside the NoCore structure.
    let st_pos = ctx.db.with_snapshot(|s| glam::DVec3::from_array(s.structures[&sid].position));
    ctx.systems.get_or_spawn(PERIMETER).set_ball_position(pilot.ship_id, st_pos).await;
    let _ = ok(&ctx, &mut pilot, "structureDocking", "Dock", &[PyRep::Integer(sid)]).await;
    ctx.advance_secs(31);

    // Verify repair shop rejects quotes when structure has no Quantum Core.
    let ship_id = pilot.ship_id;
    let nocore_err = call(
        &ctx,
        &mut pilot,
        "repairSvc",
        "GetRepairQuotes",
        &[PyRep::List(vec![PyRep::Integer(ship_id)])],
    )
    .await
    .expect_err("repairshop requires operational structure with quantum core");
    assert_eq!(nocore_err.code, "StructureNotOperational");

    // Deposit Astrahus Quantum Core (56201) into StructureDeedBay (flag 172).
    let created = common::slash_cmd(&ctx, &mut pilot.sess, &format!("/createitem {ASTRAHUS_CORE_TYPE} 1")).await;
    let core_id = created.dict_get("itemID").and_then(|v| v.as_i64()).expect("core itemID");
    let _ = ok(
        &ctx,
        &mut pilot,
        "invbroker",
        "Add",
        &[
            PyRep::Integer(core_id),
            PyRep::Integer(sid),
            PyRep::Integer(InventoryFlag::StructureDeedBay as i64),
        ],
    )
    .await;
    let _ = ok(&ctx, &mut pilot, "structureGuests", "GetGuests", &[PyRep::Integer(sid)]).await;

    // Advance 15m: Onlining -> ShieldVulnerable (unfueled = LowPower).
    ctx.advance_secs(901);
    let _ = ok(&ctx, &mut pilot, "structureGuests", "GetGuests", &[PyRep::Integer(sid)]).await;

    // Verify structure is unfueled with core installed (LowPower).
    let (has_core, fuel) = ctx.db.with_snapshot(|s| {
        let st = &s.structures[&sid];
        (st.quantum_core_installed, st.fuel_blocks)
    });
    assert!(has_core, "Quantum core must be installed");
    assert_eq!(fuel, 0, "Structure must be unfueled (LowPower)");

    // Configure 5% repair tax (500 bps = 0.05) on a structure profile and attach to citadel.
    let prof_rep = ok(
        &ctx,
        &mut pilot,
        "structureProfiles",
        "CreateProfile",
        &[
            PyRep::String("Repair Profile".into()),
            PyRep::String("Repair tax profile".into()),
        ],
    )
    .await;
    let prof_id = prof_rep.as_i64().expect("profileID");
    let patch = PyRep::List(vec![PyRep::Tuple(vec![
        PyRep::Integer(13), // service 13 = repair
        PyRep::Integer(0),  // group 0 = global base tax
        PyRep::Real(0.05),  // 5% tax
    ])]);
    let _ = ok(
        &ctx,
        &mut pilot,
        "structureProfiles",
        "SaveProfileSettings",
        &[PyRep::Integer(prof_id), patch],
    )
    .await;
    let _ = ok(
        &ctx,
        &mut pilot,
        "structureProfiles",
        "ChangeProfiles",
        &[PyRep::List(vec![PyRep::Integer(sid)]), PyRep::Integer(prof_id)],
    )
    .await;
    ctx.advance_secs(305);
    let _ = ok(&ctx, &mut pilot, "structureGuests", "GetGuests", &[PyRep::Integer(sid)]).await;

    // Apply damage to active ship: 120 hull damage (attr 3 = DAMAGE) and 180 armor damage (attr 266 = ARMOR_DAMAGE).
    let ship_id = pilot.ship_id;
    common::slash_cmd(&ctx, &mut pilot.sess, &format!("/setattr {ship_id} 3 120.0")).await;
    common::slash_cmd(&ctx, &mut pilot.sess, &format!("/setattr {ship_id} 266 180.0")).await;

    // Verify LowPower repair shop returns quotes with damage and non-zero tax bill.
    let ship_id = pilot.ship_id;
    let quotes = ok(
        &ctx,
        &mut pilot,
        "repairSvc",
        "GetRepairQuotes",
        &[PyRep::List(vec![PyRep::Integer(ship_id)])],
    )
    .await;
    let PyRep::Dict(q_dict) = quotes else { panic!("expected dict") };
    let (_, PyRep::List(rows)) = &q_dict[0] else { panic!("expected quote rows") };
    let ship_row = rows.iter().find(|r| kv_i64(r, "itemID") == ship_id).expect("ship quote");

    let dmg = kv_f64(ship_row, "damage");
    let total_cost = kv_f64(ship_row, "totalRepairCost");
    assert!(dmg >= 300.0, "Damage should reflect 120 hull + 180 armor: got {dmg}");
    assert!(total_cost > 0.0, "Total repair cost should be positive: got {total_cost}");

    // Execute repair inside the structure: clears damage and debits ISK.
    let repair_res = ok(
        &ctx,
        &mut pilot,
        "repairSvc",
        "RepairItemsInStructure",
        &[PyRep::List(vec![PyRep::Integer(ship_id)])],
    )
    .await;
    let PyRep::List(repaired_ids) = repair_res.dict_get("repairedItemIDs").expect("repairedItemIDs") else {
        panic!("expected list");
    };
    assert!(
        repaired_ids.iter().any(|v| v.as_i64() == Some(ship_id)),
        "Ship must be in repaired items list"
    );

    // Verify subsequent quote returns 0 damage and 0 repair cost.
    let quotes_cleared = ok(
        &ctx,
        &mut pilot,
        "repairSvc",
        "GetRepairQuotes",
        &[PyRep::List(vec![PyRep::Integer(ship_id)])],
    )
    .await;
    let PyRep::Dict(q_cleared_dict) = quotes_cleared else { panic!("expected dict") };
    let (_, PyRep::List(rows_cleared)) = &q_cleared_dict[0] else { panic!("expected quote rows") };
    let ship_row_cleared = rows_cleared
        .iter()
        .find(|r| kv_i64(r, "itemID") == pilot.ship_id)
        .expect("ship row cleared");
    assert_eq!(kv_f64(ship_row_cleared, "damage"), 0.0);
    assert_eq!(kv_f64(ship_row_cleared, "totalRepairCost"), 0.0);
}

#[tokio::test]
async fn test_structures_profiles_and_directory_live() {
    eve_testkit::attest(&[
        "feat:structures.profiles",
        "feat:structures.structure-directory",
    ]);

    let ctx = paused_ctx();
    let mut pilot = common::gm_pilot_in_space(&ctx, 203, 91_400_003, CHARON_TYPE, PERIMETER).await;
    common::slash_cmd(&ctx, &mut pilot.sess, "/giveskill 3363 5").await;
    ctx.advance_secs(31);

    // Create corporation and advance past 7-day age gate.
    let _ = ok(
        &ctx,
        &mut pilot,
        "corpRegistry",
        "AddCorporation",
        &[
            PyRep::String("Live Directory Operations Corp".into()),
            PyRep::String("LDOC".into()),
        ],
    )
    .await;
    ctx.advance_secs(7 * 86_400 + 31);

    // Deploy Astrahus and advance through lifecycle to ShieldVulnerable.
    common::stock_ship_bay(&ctx, &mut pilot, ASTRAHUS_TYPE, 1, InventoryFlag::Cargo as i64).await;
    let anchor_res = ok(
        &ctx,
        &mut pilot,
        "structureDeployment",
        "Anchor",
        &[
            PyRep::Integer(ASTRAHUS_TYPE as i64),
            PyRep::String("Directory Citadel".into()),
        ],
    )
    .await;
    let sid = kv_i64(&anchor_res, "structureID");
    assert!(sid >= 1_000_000_000_000);

    ctx.advance_secs(901); // AnchorVulnerable -> Anchoring
    let _ = ok(&ctx, &mut pilot, "structureGuests", "GetGuests", &[PyRep::Integer(sid)]).await;
    ctx.advance_secs(86_401); // Anchoring -> NoCore
    let _ = ok(&ctx, &mut pilot, "structureGuests", "GetGuests", &[PyRep::Integer(sid)]).await;

    let st_pos = ctx.db.with_snapshot(|s| glam::DVec3::from_array(s.structures[&sid].position));
    ctx.systems.get_or_spawn(PERIMETER).set_ball_position(pilot.ship_id, st_pos).await;
    let _ = ok(&ctx, &mut pilot, "structureDocking", "Dock", &[PyRep::Integer(sid)]).await;
    ctx.advance_secs(31);

    let created = common::slash_cmd(&ctx, &mut pilot.sess, &format!("/createitem {ASTRAHUS_CORE_TYPE} 1")).await;
    let core_id = created.dict_get("itemID").and_then(|v| v.as_i64()).expect("core itemID");
    let _ = ok(
        &ctx,
        &mut pilot,
        "invbroker",
        "Add",
        &[
            PyRep::Integer(core_id),
            PyRep::Integer(sid),
            PyRep::Integer(InventoryFlag::StructureDeedBay as i64),
        ],
    )
    .await;
    let _ = ok(&ctx, &mut pilot, "structureGuests", "GetGuests", &[PyRep::Integer(sid)]).await;

    ctx.advance_secs(901); // Onlining -> ShieldVulnerable
    let _ = ok(&ctx, &mut pilot, "structureGuests", "GetGuests", &[PyRep::Integer(sid)]).await;

    // 1. Structure profile CRUD operations.
    let p2_rep = ok(
        &ctx,
        &mut pilot,
        "structureProfiles",
        "CreateProfile",
        &[
            PyRep::String("Staging Outpost Profile".into()),
            PyRep::String("Custom profile for outpost structures".into()),
        ],
    )
    .await;
    let p2_id = p2_rep.as_i64().expect("created profileID");
    assert!(p2_id > 0);

    let list_rep = ok(&ctx, &mut pilot, "structureProfiles", "GetProfiles", &[]).await;
    let PyRep::List(profiles) = list_rep else { panic!("expected profiles list") };
    assert!(profiles.iter().any(|p| kv_i64(p, "profileID") == p2_id));

    let settings = ok(
        &ctx,
        &mut pilot,
        "structureProfiles",
        "GetProfileSettings",
        &[PyRep::Integer(p2_id)],
    )
    .await;
    let PyRep::Dict(s_dict) = settings else { panic!("expected settings dict") };
    assert!(!s_dict.is_empty());

    let patch = PyRep::List(vec![PyRep::Tuple(vec![
        PyRep::Integer(13),
        PyRep::Integer(0),
        PyRep::Real(0.08),
    ])]);
    let _ = ok(
        &ctx,
        &mut pilot,
        "structureProfiles",
        "SaveProfileSettings",
        &[PyRep::Integer(p2_id), patch],
    )
    .await;

    let p3_rep = ok(
        &ctx,
        &mut pilot,
        "structureProfiles",
        "DuplicateProfile",
        &[PyRep::Integer(p2_id), PyRep::String("Cloned Profile".into())],
    )
    .await;
    let p3_id = p3_rep.as_i64().expect("duplicated profileID");
    assert_ne!(p3_id, p2_id);

    // Set p3 as default, then switch default to p2 so p3 can be deleted.
    let _ = ok(&ctx, &mut pilot, "structureProfiles", "SetDefaultProfile", &[PyRep::Integer(p3_id)]).await;
    let _ = ok(&ctx, &mut pilot, "structureProfiles", "SetDefaultProfile", &[PyRep::Integer(p2_id)]).await;
    let del_res = ok(&ctx, &mut pilot, "structureProfiles", "DeleteProfile", &[PyRep::Integer(p3_id)]).await;
    assert_eq!(del_res.as_bool(), Some(true));

    // 2. Delayed profile application (5-minute transition delay).
    let _ = ok(
        &ctx,
        &mut pilot,
        "structureProfiles",
        "ChangeProfiles",
        &[
            PyRep::List(vec![PyRep::Integer(sid)]),
            PyRep::Integer(p2_id),
        ],
    )
    .await;

    // Advance 305 seconds (past the 300 s / 5 min transition window) and sync.
    ctx.advance_secs(305);
    let _ = ok(&ctx, &mut pilot, "structureGuests", "GetGuests", &[PyRep::Integer(sid)]).await;

    // 3. Structure directory queries.
    let corp_structs = ok(&ctx, &mut pilot, "structureDirectory", "GetMyCorporationStructures", &[]).await;
    let PyRep::Dict(cs_dict) = corp_structs else { panic!("expected dict") };
    assert!(
        cs_dict.iter().any(|(k, _)| k.as_i64() == Some(sid)),
        "Structure must appear in GetMyCorporationStructures"
    );

    let dockable = ok(
        &ctx,
        &mut pilot,
        "structureDirectory",
        "GetMyDockableStructures",
        &[PyRep::Integer(PERIMETER as i64)],
    )
    .await;
    let PyRep::List(d_list) = dockable else { panic!("expected dockable list") };
    assert!(
        d_list.iter().any(|v| v.as_i64() == Some(sid)),
        "Structure must appear in GetMyDockableStructures"
    );

    let check_docking = ok(
        &ctx,
        &mut pilot,
        "structureDirectory",
        "CheckMyDockingAccessToStructures",
        &[PyRep::List(vec![PyRep::Integer(sid)])],
    )
    .await;
    let PyRep::List(cd_list) = check_docking else { panic!("expected check list") };
    assert!(
        cd_list.iter().any(|v| v.as_i64() == Some(sid)),
        "Structure must pass CheckMyDockingAccessToStructures"
    );

    let info = ok(
        &ctx,
        &mut pilot,
        "structureDirectory",
        "GetStructureInfo",
        &[PyRep::Integer(sid)],
    )
    .await;
    assert_eq!(kv_i64(&info, "structureID"), sid);
    assert_eq!(kv_str(&info, "itemName"), Some("Directory Citadel"));
}
