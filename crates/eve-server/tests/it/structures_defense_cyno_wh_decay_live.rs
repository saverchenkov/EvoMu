//! Live integration test suite covering structure boarding/gunner control,
//! structure defense settings, mobile cynosural inhibitor deployment and suppression,
//! and canonical wormhole decay stage descriptions and age progression:
//! - `feat:structures.board-structure`
//! - `feat:structures.defense-settings`
//! - `feat:structures.mobile-cyno-inhibitor`
//! - `feat:wormholes.lifetime-decay-stages`

#![forbid(unsafe_code)]

use crate::common::{self, LivePilot};
use eve_db::types::InventoryFlag;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use eve_server::services::mobile_deployables::{
    can_deployable_be_scooped, GROUP_MOBILE_CYNO_INHIBITOR, TYPE_MOBILE_CYNO_INHIBITOR,
};
use eve_server::services::space_destiny::{
    compute_wormhole_stages_with_sde, derive_wormhole_buckets_raw,
};
use eve_server::services::structure_acl_defense_rules::{
    is_cyno_inhibited, MOBILE_CYNO_INHIBITOR_RANGE_M,
};
use glam::DVec3;
use std::collections::HashMap;
use std::sync::Arc;

const PERIMETER: u32 = 30_000_144;
const RANCER: u32 = 30_002_718;
const CHARON_TYPE: u32 = 20185;
const ASTRAHUS_TYPE: u32 = 35832;
const ASTRAHUS_CORE_TYPE: u32 = 56201;

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

fn kv_bool(rep: &PyRep, key: &str) -> bool {
    rep.dict_get(key).and_then(|v| v.as_bool()).unwrap_or(false)
}

#[tokio::test]
async fn test_structures_board_eject_and_defense_settings_live() {
    // parity: feat:structures.board-structure, feat:structures.defense-settings
    eve_testkit::attest(&[
        "feat:structures.board-structure",
        "feat:structures.defense-settings",
    ]);

    let ctx = paused_ctx();
    let mut pilot = common::gm_pilot_in_space(&ctx, 230, 91_600_001, CHARON_TYPE, PERIMETER).await;
    let char_id = pilot.char_id;
    let ship_id = pilot.ship_id;

    // Grant Corp Management skill and advance clock to register skill training.
    common::slash_cmd(&ctx, &mut pilot.sess, "/giveskill 3363 5").await;
    ctx.advance_secs(31);

    // 1. Create a player corporation and age it past the 7-day anchoring requirement.
    let _ = ok(
        &ctx,
        &mut pilot,
        "corpRegistry",
        "AddCorporation",
        &[
            PyRep::String("Live Defense Ops Corp".into()),
            PyRep::String("LDOP".into()),
        ],
    )
    .await;
    ctx.advance_secs(7 * 86_400 + 31);

    // 2. Deploy Astrahus in space via live anchoring RPC.
    common::stock_ship_bay(&ctx, &mut pilot, ASTRAHUS_TYPE, 1, InventoryFlag::Cargo as i64).await;
    let anchor_res = ok(
        &ctx,
        &mut pilot,
        "structureDeployment",
        "Anchor",
        &[
            PyRep::Integer(ASTRAHUS_TYPE as i64),
            PyRep::String("Live Boarding & Defense Astrahus".into()),
        ],
    )
    .await;
    let sid = kv_i64(&anchor_res, "structureID");
    assert!(sid >= 1_000_000_000_000, "Spawned valid Upwell structure ID: {sid}");

    // Advance 15m (AnchorVulnerable -> Anchoring).
    ctx.advance_secs(901);
    let _ = ok(&ctx, &mut pilot, "structureGuests", "GetGuests", &[PyRep::Integer(sid)]).await;

    // Advance 24h (Anchoring -> NoCore).
    ctx.advance_secs(86_401);
    let _ = ok(&ctx, &mut pilot, "structureGuests", "GetGuests", &[PyRep::Integer(sid)]).await;

    // 3. Move pilot to structure and dock inside before core installation.
    let st_pos = ctx.db.with_snapshot(|s| DVec3::from_array(s.structures[&sid].position));
    ctx.systems.get_or_spawn(PERIMETER).set_ball_position(ship_id, st_pos).await;
    let _ = ok(&ctx, &mut pilot, "structureDocking", "Dock", &[PyRep::Integer(sid)]).await;
    ctx.advance_secs(31);
    assert_eq!(pilot.sess.structure_id, Some(sid), "Pilot successfully docked in structure");

    // Deposit Quantum Core into StructureDeedBay while docked.
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

    // Advance 15m (Onlining -> ShieldVulnerable).
    ctx.advance_secs(901);
    let _ = ok(&ctx, &mut pilot, "structureGuests", "GetGuests", &[PyRep::Integer(sid)]).await;

    // 4. Structure Gunner Control (`TakeControl`):
    let take_res = ok(
        &ctx,
        &mut pilot,
        "structureControl",
        "TakeControl",
        &[PyRep::Integer(sid)],
    )
    .await;
    assert_eq!(kv_i64(&take_res, "structureID"), sid);
    assert_eq!(kv_i64(&take_res, "pilotID"), char_id);
    assert!(kv_bool(&take_res, "success"));
    assert_eq!(pilot.sess.ship_id, Some(sid), "Active ship transitioned to structure ID");

    // Verify gunner query reports the controlling pilot.
    let active_pilot = ok(
        &ctx,
        &mut pilot,
        "structureControl",
        "GetStructurePilot",
        &[PyRep::Integer(sid)],
    )
    .await;
    assert_eq!(active_pilot, PyRep::Integer(char_id));

    // 5. Release Gunner Control (`ReleaseControl`):
    let _ = ok(
        &ctx,
        &mut pilot,
        "structureControl",
        "ReleaseControl",
        &[PyRep::Integer(sid)],
    )
    .await;
    assert_eq!(pilot.sess.ship_id, Some(ship_id), "Active ship reverted to docked ship");

    // 6. Board Structure (`BoardStructure`):
    let board_res = ok(
        &ctx,
        &mut pilot,
        "structureControl",
        "BoardStructure",
        &[PyRep::Integer(sid)],
    )
    .await;
    assert_eq!(kv_i64(&board_res, "structureID"), sid);
    assert_eq!(kv_i64(&board_res, "pilotID"), char_id);
    assert!(kv_bool(&board_res, "boarded"));
    assert_eq!(pilot.sess.ship_id, Some(sid));

    // 7. Eject from Structure (`EjectFromStructure`):
    let eject_res = ok(
        &ctx,
        &mut pilot,
        "structureControl",
        "EjectFromStructure",
        &[PyRep::Integer(sid)],
    )
    .await;
    assert_eq!(kv_i64(&eject_res, "structureID"), sid);
    assert_eq!(eject_res.dict_get("pilotID"), Some(&PyRep::None));
    assert!(!kv_bool(&eject_res, "boarded"));

    // 8. Configure Defense Settings via `structureProfiles.SaveProfileSettings`:
    // Re-board to verify settings configuration while active.
    let _ = ok(
        &ctx,
        &mut pilot,
        "structureControl",
        "TakeControl",
        &[PyRep::Integer(sid)],
    )
    .await;

    // Create an Upwell profile and configure automated defense policy.
    let prof_rep = ok(
        &ctx,
        &mut pilot,
        "structureProfiles",
        "CreateProfile",
        &[
            PyRep::String("Citadel Defense Profile".into()),
            PyRep::String("Automated defensive grid configuration".into()),
        ],
    )
    .await;
    let prof_id = prof_rep
        .as_i64()
        .or_else(|| prof_rep.dict_get("profileID").and_then(|v| v.as_i64()))
        .expect("profileID");

    // Save defense policy: setting 17 -> (auto_defense_enabled, auto_target_attackers, auto_target_war_targets, standing_thresh, target_blocked_acl).
    let save_res = ok(
        &ctx,
        &mut pilot,
        "structureProfiles",
        "SaveProfileSettings",
        &[
            PyRep::Integer(prof_id),
            PyRep::List(vec![
                PyRep::Tuple(vec![
                    PyRep::Integer(17),
                    PyRep::Tuple(vec![
                        PyRep::Bool(true),
                        PyRep::Bool(true),
                        PyRep::Bool(false),
                        PyRep::Real(-5.0),
                        PyRep::Bool(true),
                    ]),
                    PyRep::Integer(0),
                ]),
            ]),
        ],
    )
    .await;
    assert_eq!(save_res, PyRep::Bool(true));

    // Verify persisted defense settings in extended state profile.
    let (auto_enabled, auto_attackers, standing_thresh) = ctx.db.with_snapshot(|s| {
        let ext = eve_server::services::station_structure::structure_upwell_ops::load_ext_state(s);
        let prof = &ext.profiles_by_id[&prof_id];
        let ds = prof.defense_settings.as_ref().expect("defense settings must be saved");
        (ds.auto_defense_enabled, ds.auto_target_attackers, ds.standing_threshold)
    });
    assert!(auto_enabled, "Auto-defense must be enabled");
    assert!(auto_attackers, "Auto-targeting attackers must be active");
    assert_eq!(standing_thresh, -5.0, "Defense standing threshold must be -5.0");

    // Cleanly release control at end of test.
    let _ = ok(
        &ctx,
        &mut pilot,
        "structureControl",
        "ReleaseControl",
        &[PyRep::Integer(sid)],
    )
    .await;
}

#[tokio::test]
async fn test_mobile_cyno_inhibitor_deployment_and_suppression_live() {
    // parity: feat:structures.mobile-cyno-inhibitor
    eve_testkit::attest(&["feat:structures.mobile-cyno-inhibitor"]);

    let ctx = paused_ctx();
    // Spawn pilot in lowsec Rancer (30002718) in a Falcon cruiser.
    let falcon_type = 11957;
    let mut pilot = common::gm_pilot_in_space(&ctx, 231, 91_600_002, falcon_type, RANCER).await;

    // Grant Anchoring skill (11584) level 5.
    common::slash_cmd(&ctx, &mut pilot.sess, "/giveskill 11584 5").await;
    ctx.advance_secs(31);

    // Verify Cyno Inhibitor group is not scoopable per EVE Online design.
    assert!(
        !can_deployable_be_scooped(GROUP_MOBILE_CYNO_INHIBITOR),
        "Mobile Cynosural Inhibitor (group 1249) must not be scoopable"
    );

    // Stock Mobile Cynosural Inhibitor (type 33476) in cargo.
    let created_mci = common::slash_cmd(&ctx, &mut pilot.sess, &format!("/createitem {TYPE_MOBILE_CYNO_INHIBITOR} 1")).await;
    let mci_item_id = created_mci.dict_get("itemID").and_then(|v| v.as_i64()).expect("MCI itemID");

    // Stock a second MCI to verify the 200 km deployment exclusion ban.
    let created_mci2 = common::slash_cmd(&ctx, &mut pilot.sess, &format!("/createitem {TYPE_MOBILE_CYNO_INHIBITOR} 1")).await;
    let mci2_item_id = created_mci2.dict_get("itemID").and_then(|v| v.as_i64()).expect("MCI2 itemID");

    // 1. Deploy the Mobile Cynosural Inhibitor via posMgr.AnchorStructure.
    let t0 = ctx.now_unix();
    let anchor_res = ok(
        &ctx,
        &mut pilot,
        "posMgr",
        "AnchorStructure",
        &[PyRep::Integer(mci_item_id)],
    )
    .await;

    let delay = kv_f64(&anchor_res, "anchoringDelaySeconds");
    assert_eq!(delay, 120.0, "MCI anchoring delay must be 120 seconds");
    let despawn = kv_i64(&anchor_res, "despawnUnix");
    assert_eq!(despawn, (t0 + 3_600) as i64, "MCI lifetime must be 1 hour");

    // 2. Second MCI within 200 km must be rejected with MobileCynoInhibitorOverlap.
    let overlap_err = call(
        &ctx,
        &mut pilot,
        "posMgr",
        "AnchorStructure",
        &[PyRep::Integer(mci2_item_id)],
    )
    .await
    .expect_err("Second MCI inside 200km must fail");
    assert_eq!(overlap_err.code, "MobileCynoInhibitorOverlap");

    // 3. During initial 120s anchoring delay (t0 + 60s): inhibitor is spooling up -> cyno is NOT yet inhibited.
    ctx.advance_secs(60);
    let mci_pos = ctx.db.with_snapshot(|s| DVec3::from_array(s.item_positions[&mci_item_id]));
    let pilot_50km = mci_pos + DVec3::new(50_000.0, 0.0, 0.0);
    let pilot_150km = mci_pos + DVec3::new(150_000.0, 0.0, 0.0);

    let is_inhib_spooling = ctx.db.with_snapshot(|snap| {
        is_cyno_inhibited(&ctx, snap, RANCER, Some(pilot_50km))
    });
    assert!(!is_inhib_spooling, "Cyno must NOT be inhibited during 120s anchoring countdown");

    // 4. After 120s anchoring timer completes (t0 + 125s): inhibitor is online and active!
    ctx.advance_secs(65);

    // Within 100 km radius (50 km away): cyno activation is inhibited.
    let is_inhib_active = ctx.db.with_snapshot(|snap| {
        is_cyno_inhibited(&ctx, snap, RANCER, Some(pilot_50km))
    });
    assert!(is_inhib_active, "Cyno must be inhibited within 100 km of online inhibitor");

    // Outside 100 km radius (150 km away): cyno activation is permitted.
    let is_inhib_outside = ctx.db.with_snapshot(|snap| {
        is_cyno_inhibited(&ctx, snap, RANCER, Some(pilot_150km))
    });
    assert!(!is_inhib_outside, "Cyno must NOT be inhibited outside 100 km radius");
    assert_eq!(MOBILE_CYNO_INHIBITOR_RANGE_M, 100_000.0);
}

#[tokio::test]
async fn test_wormhole_decay_stages_and_show_info_live() {
    // parity: feat:wormholes.lifetime-decay-stages
    eve_testkit::attest(&["feat:wormholes.lifetime-decay-stages"]);

    let ctx = paused_ctx();
    let now = ctx.now_unix();

    // SDE Wormhole B041 (type 30645): 48 hours lifespan (2880 minutes).
    let b041 = ctx.sde.wormhole_types.get(&30_645).expect("Wormhole B041 in SDE");
    let lifetime_48h = (b041.max_stable_time_min * 60.0) as u64;

    // Stage 0: > 24 hours remaining (elapsed = 12h, 36h left)
    // Canonical EVE Online: Healthy, wormholeAge = 0
    let fresh_buckets = derive_wormhole_buckets_raw(
        30_000_142,
        b041.max_jump_mass,
        b041.max_stable_mass,
        b041.max_stable_mass,
        now,
        lifetime_48h,
        false,
        now + 12 * 3_600,
        &ctx.sde,
    );
    assert_eq!(fresh_buckets.lifetime_bucket, ">1 day");
    assert_eq!(
        fresh_buckets.lifetime_description,
        "This wormhole has not yet begun its natural cycle of decay and is not likely to disappear for some time"
    );

    let mut mock_wh = eve_db::types::CosmicSignatureRecord {
        signature_id: "WH-TEST-48H".into(),
        solar_system_id: 31_002_000,
        sig_group: "Wormhole".into(),
        type_name: "Wormhole B041".into(),
        position: [0.0, 0.0, 0.0],
        signal_strength: 1.0,
        wormhole_type_id: Some(30_645),
        destination_system_id: Some(30_000_142),
        created_unix: now,
        lifetime_seconds: lifetime_48h,
        max_mass_kg: b041.max_stable_mass,
        remaining_mass_kg: b041.max_stable_mass,
        max_jump_mass_kg: b041.max_jump_mass,
        collapsed: false,
    };

    let (_, _, fresh_stage, fresh_age, _) =
        compute_wormhole_stages_with_sde(&mock_wh, now + 12 * 3_600, Some(&ctx.sde));
    assert_eq!(fresh_stage, "Healthy", "Remaining >24h must report Healthy stage");
    assert_eq!(fresh_age, 0, "Remaining >24h must report wormholeAge 0");

    // Stage 1: 4h ..= 24h remaining (elapsed = 36h, 12h left)
    // Canonical EVE Online: BeginningToDecay, wormholeAge = 1
    let decaying_buckets = derive_wormhole_buckets_raw(
        30_000_142,
        b041.max_jump_mass,
        b041.max_stable_mass,
        b041.max_stable_mass,
        now,
        lifetime_48h,
        false,
        now + 36 * 3_600,
        &ctx.sde,
    );
    assert_eq!(decaying_buckets.lifetime_bucket, "<1 day");
    assert_eq!(
        decaying_buckets.lifetime_description,
        "This wormhole is beginning to decay, and probably won't last another day"
    );

    let (_, _, dec_stage, dec_age, _) =
        compute_wormhole_stages_with_sde(&mock_wh, now + 36 * 3_600, Some(&ctx.sde));
    assert_eq!(dec_stage, "BeginningToDecay", "Remaining 4h-24h must report BeginningToDecay");
    assert_eq!(dec_age, 1, "Remaining 4h-24h must report wormholeAge 1");

    // Stage 2: < 4h remaining (elapsed = 46h, 2h left)
    // Canonical EVE Online: ReachingEndOfLifetime, wormholeAge = 2
    let eol_buckets = derive_wormhole_buckets_raw(
        30_000_142,
        b041.max_jump_mass,
        b041.max_stable_mass,
        b041.max_stable_mass,
        now,
        lifetime_48h,
        false,
        now + 46 * 3_600,
        &ctx.sde,
    );
    assert_eq!(eol_buckets.lifetime_bucket, "<4h");
    assert_eq!(
        eol_buckets.lifetime_description,
        "This wormhole is reaching the end of its natural lifetime and is likely to collapse very soon"
    );

    let (_, _, eol_stage, eol_age, _) =
        compute_wormhole_stages_with_sde(&mock_wh, now + 46 * 3_600, Some(&ctx.sde));
    assert_eq!(eol_stage, "ReachingEndOfLifetime", "Remaining <4h must report ReachingEndOfLifetime");
    assert_eq!(eol_age, 2, "Remaining <4h must report wormholeAge 2");

    // Stage 3: Expired / Collapsed
    let (_, _, col_stage, col_age, is_col) =
        compute_wormhole_stages_with_sde(&mock_wh, now + 49 * 3_600, Some(&ctx.sde));
    assert_eq!(col_stage, "Collapsed_Expired");
    assert_eq!(col_age, 3);
    assert!(is_col);

    // Verify 16h wormhole progression across stages 1 and 2:
    mock_wh.lifetime_seconds = 16 * 3_600;
    let (_, _, w16_stage1, w16_age1, _) =
        compute_wormhole_stages_with_sde(&mock_wh, now + 2 * 3_600, Some(&ctx.sde));
    assert_eq!(w16_stage1, "BeginningToDecay", "16h wormhole with 14h left must be BeginningToDecay");
    assert_eq!(w16_age1, 1);

    let (_, _, w16_stage2, w16_age2, _) =
        compute_wormhole_stages_with_sde(&mock_wh, now + 14 * 3_600, Some(&ctx.sde));
    assert_eq!(w16_stage2, "ReachingEndOfLifetime", "16h wormhole with 2h left must be ReachingEndOfLifetime");
    assert_eq!(w16_age2, 2);
}
