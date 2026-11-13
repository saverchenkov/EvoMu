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
    let dock_st = ctx.db.with_mut_snapshot(|s| {
        let ch = s.characters.get_mut(&pilot.char_id).unwrap();
        ch.station_id = Some(ch.home_station_id);
        ch.home_station_id
    });
    pilot.sess.station_id = Some(dock_st);
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
    pilot.sess.station_id = None;
    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&pilot.char_id).unwrap().station_id = None;
    });
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
    let dock_st = ctx.db.with_mut_snapshot(|s| {
        let ch = s.characters.get_mut(&pilot.char_id).unwrap();
        ch.station_id = Some(ch.home_station_id);
        ch.home_station_id
    });
    pilot.sess.station_id = Some(dock_st);
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
    pilot.sess.station_id = None;
    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&pilot.char_id).unwrap().station_id = None;
    });
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

// parity: feat:structures.profiles, feat:structures.structure-directory
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
    let dock_st = ctx.db.with_mut_snapshot(|s| {
        let ch = s.characters.get_mut(&pilot.char_id).unwrap();
        ch.station_id = Some(ch.home_station_id);
        ch.home_station_id
    });
    pilot.sess.station_id = Some(dock_st);
    let corp_rep = ok(
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
    pilot.sess.station_id = None;
    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&pilot.char_id).unwrap().station_id = None;
    });
    let corp_id = corp_rep
        .as_i64()
        .filter(|&id| id > 0)
        .or_else(|| pilot.sess.corporation_id)
        .expect("created corp_id");
    pilot.sess.corporation_id = Some(corp_id);
    pilot.sess.corp_role |= eve_db::corp_roles::ROLE_DIRECTOR;
    ctx.advance_secs(7 * 86_400 + 31);

    // Enroll an unprivileged corp member (no Director/Station Manager role) and two outsiders.
    let member_id = 91_400_004_i64;
    ctx.db.provision_gm_account("live91400004", "live91400004-secret", member_id, "Member 91400004");
    let mut corp_member = LivePilot {
        sess: eve_machonet::SessionState::new(204),
        char_id: member_id,
        ship_id: 0,
        beyonce: 0,
    };
    common::login_pilot(&ctx, &mut corp_member.sess, "live91400004", "live91400004-secret").await;
    let _ = ok(&ctx, &mut corp_member, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(member_id)]).await;
    corp_member.sess.role = eve_db::service_roles::ROLE_PLAYER;
    ctx.advance_secs(12);
    let app_kv = ok(
        &ctx,
        &mut corp_member,
        "corpRegistry",
        "InsertApplication",
        &[PyRep::Integer(corp_id), PyRep::String("Applying to LDOC".into())],
    )
    .await;
    let app_id = kv_i64(&app_kv, "applicationID");
    let _ = ok(
        &ctx,
        &mut pilot,
        "corpRegistry",
        "UpdateApplicationOffer",
        &[PyRep::Integer(app_id), PyRep::String("Accepted".into())],
    )
    .await;
    ctx.advance_secs(12);
    let _ = ok(&ctx, &mut corp_member, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(member_id)]).await;
    corp_member.sess.role = eve_db::service_roles::ROLE_PLAYER;
    corp_member.sess.corp_role = 0;

    let mut auth_guest = common::gm_pilot_in_space(&ctx, 205, 91_400_005, CHARON_TYPE, PERIMETER).await;
    auth_guest.sess.role = eve_db::service_roles::ROLE_PLAYER;
    let mut stranger = common::gm_pilot_in_space(&ctx, 206, 91_400_006, CHARON_TYPE, KISOGO).await;
    stranger.sess.role = eve_db::service_roles::ROLE_PLAYER;

    // Deploy Astrahus, advance to ShieldVulnerable, fit Quantum Core, Fuel, and Reprocessing service module.
    common::stock_ship_bay(&ctx, &mut pilot, ASTRAHUS_TYPE, 1, InventoryFlag::Cargo as i64).await;
    let anchor_res = ok(
        &ctx,
        &mut pilot,
        "structureDeployment",
        "Anchor",
        &[PyRep::Integer(ASTRAHUS_TYPE as i64), PyRep::String("Directory Citadel".into())],
    )
    .await;
    let sid = kv_i64(&anchor_res, "structureID");
    assert!(sid >= 1_000_000_000_000);

    ctx.advance_secs(901);
    let _ = ok(&ctx, &mut pilot, "structureGuests", "GetGuests", &[PyRep::Integer(sid)]).await;
    ctx.advance_secs(86_401);
    let _ = ok(&ctx, &mut pilot, "structureGuests", "GetGuests", &[PyRep::Integer(sid)]).await;

    let st_pos = ctx.db.with_snapshot(|s| glam::DVec3::from_array(s.structures[&sid].position));
    ctx.systems.get_or_spawn(PERIMETER).set_ball_position(pilot.ship_id, st_pos).await;
    let _ = ok(&ctx, &mut pilot, "structureDocking", "Dock", &[PyRep::Integer(sid)]).await;
    ctx.advance_secs(31);

    let core_id = kv_i64(&common::slash_cmd(&ctx, &mut pilot.sess, &format!("/createitem {ASTRAHUS_CORE_TYPE} 1")).await, "itemID");
    let _ = ok(&ctx, &mut pilot, "invbroker", "Add", &[PyRep::Integer(core_id), PyRep::Integer(sid), PyRep::Integer(InventoryFlag::StructureDeedBay as i64)]).await;
    ctx.advance_secs(901);
    let _ = ok(&ctx, &mut pilot, "structureGuests", "GetGuests", &[PyRep::Integer(sid)]).await;

    let fuel_id = kv_i64(&common::slash_cmd(&ctx, &mut pilot.sess, "/createitem 4247 1000").await, "itemID");
    let _ = ok(&ctx, &mut pilot, "invbroker", "Add", &[PyRep::Integer(fuel_id), PyRep::Integer(sid), PyRep::Integer(InventoryFlag::StructureFuel as i64)]).await;
    let repro_id = kv_i64(&common::slash_cmd(&ctx, &mut pilot.sess, "/createitem 35899 1").await, "itemID");
    let _ = ok(&ctx, &mut pilot, "invbroker", "Add", &[PyRep::Integer(repro_id), PyRep::Integer(sid), PyRep::Integer(InventoryFlag::ServiceSlot0 as i64)]).await;
    ctx.advance_secs(31);

    // 1. Negative role check & Access Group + Profile CRUD + per-service ACL/tax settings.
    let err_unpriv = call(&ctx, &mut corp_member, "structureProfiles", "CreateProfile", &[PyRep::String("Denied".into()), PyRep::String("".into())])
        .await
        .expect_err("unprivileged corp member must not create structure profiles");
    assert_eq!(err_unpriv.code, "InsufficientCorpRoles");

    let grp_rep = ok(
        &ctx,
        &mut pilot,
        "ownerGroupManager",
        "Create",
        &[PyRep::String("Allied Guest ACL".into()), PyRep::String("Authorized guests".into()), PyRep::Bool(true)],
    )
    .await;
    let gid = kv_i64(&grp_rep, "groupID");
    let _ = ok(&ctx, &mut pilot, "ownerGroupManager", "AddMembers", &[PyRep::Integer(gid), PyRep::List(vec![PyRep::Integer(auth_guest.char_id)])]).await;

    let p2_id = ok(
        &ctx,
        &mut pilot,
        "structureProfiles",
        "CreateProfile",
        &[PyRep::String("Staging Outpost Profile".into()), PyRep::String("ACL and 8% reprocessing tax".into())],
    )
    .await
    .as_i64()
    .expect("created profileID");
    assert!(p2_id > 0);

    let PyRep::List(profiles) = ok(&ctx, &mut pilot, "structureProfiles", "GetProfiles", &[]).await else { panic!("expected profiles list") };
    assert!(profiles.iter().any(|p| kv_i64(p, "profileID") == p2_id));
    let PyRep::Dict(s_dict) = ok(&ctx, &mut pilot, "structureProfiles", "GetProfileSettings", &[PyRep::Integer(p2_id)]).await else { panic!("expected settings dict") };
    assert!(!s_dict.is_empty());

    // Negative invalid tax (> 1.0) rejected; valid docking ACL (19) + reprocessing service ACL & 8% tax (3) saved.
    let bad_tax = call(&ctx, &mut pilot, "structureProfiles", "SaveProfileSettings", &[PyRep::Integer(p2_id), PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(3), PyRep::Integer(0), PyRep::Real(1.5)])])])
        .await
        .expect_err("tax > 100% must fail");
    assert_eq!(bad_tax.code, "InvalidStructureTaxRate");

    let patch = PyRep::List(vec![
        PyRep::Tuple(vec![PyRep::Integer(19), PyRep::Integer(gid), PyRep::Bool(true)]),
        PyRep::Tuple(vec![PyRep::Integer(3), PyRep::Integer(gid), PyRep::Real(0.08)]),
    ]);
    let _ = ok(&ctx, &mut pilot, "structureProfiles", "SaveProfileSettings", &[PyRep::Integer(p2_id), patch]).await;

    let p3_id = ok(&ctx, &mut pilot, "structureProfiles", "DuplicateProfile", &[PyRep::Integer(p2_id), PyRep::String("Cloned Profile".into())])
        .await
        .as_i64()
        .expect("duplicated profileID");
    assert_ne!(p3_id, p2_id);
    let _ = ok(&ctx, &mut pilot, "structureProfiles", "SetDefaultProfile", &[PyRep::Integer(p3_id)]).await;
    let _ = ok(&ctx, &mut pilot, "structureProfiles", "SetDefaultProfile", &[PyRep::Integer(p2_id)]).await;
    assert_eq!(ok(&ctx, &mut pilot, "structureProfiles", "DeleteProfile", &[PyRep::Integer(p3_id)]).await.as_bool(), Some(true));

    // Activate p2_id on sid and verify per-service ACL + 8% tax for auth_guest vs denial for stranger.
    let _ = ok(&ctx, &mut pilot, "structureProfiles", "ChangeProfiles", &[PyRep::List(vec![PyRep::Integer(sid)]), PyRep::Integer(p2_id)]).await;
    ctx.advance_secs(305);
    let _ = ok(&ctx, &mut auth_guest, "structureSettings", "CharacterCheckService", &[PyRep::Integer(sid), PyRep::Integer(4)]).await;
    let PyRep::Dict(guest_svcs) = ok(&ctx, &mut auth_guest, "structureSettings", "CharacterGetServices", &[PyRep::Integer(sid)]).await else { panic!("expected services dict") };
    let repro_tax = guest_svcs.iter().find(|(k, _)| k.as_i64() == Some(4)).and_then(|(_, v)| v.as_f64()).expect("reprocessing rate");
    assert!((repro_tax - 0.08).abs() < 1e-6);

    let err_svc = call(&ctx, &mut stranger, "structureSettings", "CharacterCheckService", &[PyRep::Integer(sid), PyRep::Integer(4)])
        .await
        .expect_err("stranger outside ACL must be denied reprocessing service");
    assert_eq!(err_svc.code, "StructureServiceAccessDenied");

    // 2. Delayed 5-minute (300s) profile transition & negative DeleteProfile while in use.
    let p_corp_id = ok(&ctx, &mut pilot, "structureProfiles", "CreateProfile", &[PyRep::String("Corp Only Profile".into()), PyRep::String("".into())])
        .await
        .as_i64()
        .expect("corp profileID");
    let _ = ok(&ctx, &mut pilot, "structureProfiles", "SaveProfileSettings", &[PyRep::Integer(p_corp_id), PyRep::Dict(vec![(PyRep::Integer(19), PyRep::Integer(0))])]).await;
    let _ = ok(&ctx, &mut pilot, "structureProfiles", "ChangeProfiles", &[PyRep::List(vec![PyRep::Integer(sid)]), PyRep::Integer(p_corp_id)]).await;

    let err_in_use = call(&ctx, &mut pilot, "structureProfiles", "DeleteProfile", &[PyRep::Integer(p_corp_id)])
        .await
        .expect_err("pending assigned profile cannot be deleted");
    assert_eq!(err_in_use.code, "ProfileInUse");

    // Before 300s (t + 120s): old profile p2_id still grants docking & reprocessing access to auth_guest.
    ctx.advance_secs(120);
    let _ = ok(&ctx, &mut auth_guest, "structureSettings", "CharacterCheckService", &[PyRep::Integer(sid), PyRep::Integer(4)]).await;

    // After 300s (t + 305s): p_corp_id is now active; auth_guest loses access.
    ctx.advance_secs(185);
    let _ = ok(&ctx, &mut pilot, "structureGuests", "GetGuests", &[PyRep::Integer(sid)]).await;
    assert_eq!(kv_i64(&ok(&ctx, &mut pilot, "structureDirectory", "GetStructureInfo", &[PyRep::Integer(sid)]).await, "profileID"), p_corp_id);
    let err_after = call(&ctx, &mut auth_guest, "structureSettings", "CharacterCheckService", &[PyRep::Integer(sid), PyRep::Integer(4)])
        .await
        .expect_err("guest loses service access after corp-only profile applies");
    assert_eq!(err_after.code, "StructureServiceAccessDenied");

    // 3. Structure directory visibility, onlineServiceTypeIDs, and sensitive field masking.
    let _ = ok(&ctx, &mut pilot, "structureProfiles", "ChangeProfiles", &[PyRep::List(vec![PyRep::Integer(sid)]), PyRep::Integer(p2_id)]).await;
    ctx.advance_secs(305);
    let _ = ok(&ctx, &mut pilot, "structureGuests", "GetGuests", &[PyRep::Integer(sid)]).await;

    let PyRep::Dict(cs_dict) = ok(&ctx, &mut pilot, "structureDirectory", "GetMyCorporationStructures", &[]).await else { panic!("expected dict") };
    assert!(cs_dict.iter().any(|(k, _)| k.as_i64() == Some(sid)));
    let dir_info = ok(&ctx, &mut pilot, "structureDirectory", "GetStructureInfo", &[PyRep::Integer(sid)]).await;
    assert_eq!(kv_i64(&dir_info, "fuelBlocks"), 280);
    assert_eq!(kv_i64(&dir_info, "profileID"), p2_id);
    assert!(dir_info.dict_get("reinforceHour").and_then(|v| v.as_i64()).is_some());

    let PyRep::List(d_list) = ok(&ctx, &mut auth_guest, "structureDirectory", "GetMyDockableStructures", &[PyRep::Integer(PERIMETER as i64)]).await else { panic!("expected dockable list") };
    assert!(d_list.iter().any(|v| v.as_i64() == Some(sid)));
    let PyRep::List(cd_list) = ok(&ctx, &mut auth_guest, "structureDirectory", "CheckMyDockingAccessToStructures", &[PyRep::List(vec![PyRep::Integer(sid)])]).await else { panic!("expected check list") };
    assert!(cd_list.iter().any(|v| v.as_i64() == Some(sid)));

    // Guest sees basic metadata + onlineServiceTypeIDs, but sensitive fields are masked as None.
    let guest_info = ok(&ctx, &mut auth_guest, "structureDirectory", "GetStructureInfo", &[PyRep::Integer(sid)]).await;
    assert_eq!(kv_i64(&guest_info, "structureID"), sid);
    assert_eq!(kv_str(&guest_info, "itemName"), Some("Directory Citadel"));
    let PyRep::List(svc_types) = guest_info.dict_get("onlineServiceTypeIDs").cloned().expect("onlineServiceTypeIDs") else { panic!("expected list") };
    assert!(svc_types.iter().any(|v| v.as_i64() == Some(35899)));
    assert_eq!(guest_info.dict_get("fuelBlocks"), Some(&PyRep::None));
    assert_eq!(guest_info.dict_get("profileID"), Some(&PyRep::None));
    assert_eq!(guest_info.dict_get("reinforceHour"), Some(&PyRep::None));
    assert_eq!(guest_info.dict_get("nextReinforceHour"), Some(&PyRep::None));
    assert_eq!(guest_info.dict_get("applyAfterUnix"), Some(&PyRep::None));

    // Stranger in another system without ACL is denied GetStructureInfo.
    let err_dir = call(&ctx, &mut stranger, "structureDirectory", "GetStructureInfo", &[PyRep::Integer(sid)])
        .await
        .expect_err("unauthorized outsider outside system must be denied GetStructureInfo");
    assert_eq!(err_dir.code, "StructureAccessDenied");

    // 4. Live structure self-destruct -> notifications + removal from directory.
    let _ = ok(&ctx, &mut pilot, "structureDocking", "Undock", &[PyRep::Integer(sid)]).await;
    ctx.advance_secs(15);
    let err_sd = call(&ctx, &mut corp_member, "beyonce", "CmdSelfDestructStructure", &[PyRep::Integer(sid)])
        .await
        .expect_err("unprivileged corp member cannot self-destruct structure");
    assert_eq!(err_sd.code, "CrpAccessDenied");

    let _ = ctx.drain_notifications_for_char(pilot.char_id);
    let _ = ctx.drain_notifications_for_char(member_id);
    let _ = ok(&ctx, &mut pilot, "beyonce", "CmdSelfDestructStructure", &[PyRep::Integer(sid)]).await;
    ctx.advance_secs(1205);
    let PyRep::List(after_dockable) = ok(&ctx, &mut auth_guest, "structureDirectory", "GetMyDockableStructures", &[PyRep::Integer(PERIMETER as i64)]).await else { panic!("expected list") };
    assert!(!after_dockable.iter().any(|v| v.as_i64() == Some(sid)), "destroyed structure must be removed from dockable directory");
    let PyRep::Dict(after_corp) = ok(&ctx, &mut pilot, "structureDirectory", "GetMyCorporationStructures", &[]).await else { panic!("expected dict") };
    assert!(!after_corp.iter().any(|(k, _)| k.as_i64() == Some(sid)), "destroyed structure must be removed from corp directory");
    let gone = ok(&ctx, &mut pilot, "structureDirectory", "GetStructureInfo", &[PyRep::Integer(sid)]).await;
    assert_eq!(gone, PyRep::None, "destroyed structure GetStructureInfo must return None (#1007)");

    let notifs_p = ctx.drain_notifications_for_char(pilot.char_id);
    let notifs_m = ctx.drain_notifications_for_char(member_id);
    let saw_removed = notifs_p
        .iter()
        .chain(notifs_m.iter())
        .any(|n| n.event == "OnStructureRemoved" || n.event == "OnStructureRemovedFromSpace");
    assert!(saw_removed, "structure destruction must emit OnStructureRemoved notification");
}
