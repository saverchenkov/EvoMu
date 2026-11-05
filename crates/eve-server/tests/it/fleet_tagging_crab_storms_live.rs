//! Live integration tests for fleet target tagging, metaliminal storm roaming, and CRAB beacon link activation.
//!
//! Features covered:
//! - `feat:fleet.target-tagging`
//! - `feat:pve.metaliminal-storm-movement`
//! - `feat:pve.crab-link-activation`
//! - `feat:pve.crab-ceq-signal-interference`

#![forbid(unsafe_code)]

use crate::common::{self, LivePilot};
use eve_destiny::DVec3;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use eve_server::services::combat_crimewatch::combat_npc_ai::{
    destroy_specialized_npc_and_queue_bounty, spawn_specialized_npc_entity,
};
use eve_server::services::pve_crab_beacons::{
    is_ship_crab_linked, CRAB_CEQ_COST, CRAB_INTERFERENCE_DELTA, TYPE_CRAB_BEACON,
};
use eve_server::services::pve_metaliminal_storms::{
    advance_roaming_storms, compute_storm_footprint, init_roaming_storms, storm_move_interval_secs,
    sync_metaliminal_storm_effects_and_sites,
};
use eve_server::services::pve_special_sites::with_pve_special_reg;
use std::collections::HashMap;

const HIGHSEC: u32 = 30_000_144;
const NULLSEC: u32 = 30_000_208;
const VEXOR: u32 = 626;
const ARCHON: u32 = 23757;

fn int(v: i64) -> PyRep {
    PyRep::Integer(v)
}

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None)
        .await
}

async fn ok(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> PyRep {
    rpc(ctx, sess, service, method, args)
        .await
        .unwrap_or_else(|e| panic!("{service}.{method} failed: {e:?}"))
}

fn drain(ctx: &ServerContext, p: &mut LivePilot) -> Vec<(String, Vec<PyRep>)> {
    let mut out = p.sess.drain_notifications();
    for n in ctx.drain_notifications_for_char(p.char_id) {
        if !out.iter().any(|(e, a)| e == &n.event && a == &n.args) {
            out.push((n.event, n.args));
        }
    }
    out
}

async fn lock_target(
    ctx: &ServerContext,
    p: &mut LivePilot,
    target_id: i64,
) {
    let ship_id = p.sess.ship_id.unwrap();
    let sys_id = p.sess.solar_system_id.unwrap();
    let sys = ctx.systems.get_or_spawn(sys_id);
    if sys
        .get_target_locks(ship_id)
        .await
        .unwrap()
        .iter()
        .any(|l| l.target_id == target_id && l.locked)
    {
        return;
    }
    rpc(ctx, &mut p.sess, "dogmaIM", "AddTarget", &[int(target_id)])
        .await
        .expect("dogmaIM.AddTarget");
    for _ in 0..120 {
        if sys
            .get_target_locks(ship_id)
            .await
            .unwrap()
            .iter()
            .any(|l| l.target_id == target_id && l.locked)
        {
            break;
        }
        let _ = sys.step_manual(1.0).await;
    }
    assert!(
        sys.get_target_locks(ship_id)
            .await
            .unwrap()
            .iter()
            .any(|l| l.target_id == target_id && l.locked),
        "Target lock must complete"
    );
}

// parity: feat:fleet.target-tagging
#[tokio::test]
async fn test_live_fleet_target_tagging() {
    eve_testkit::attest(&["feat:fleet.target-tagging"]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    // Boss in Archon, Wingman in Vexor in HIGHSEC
    let mut boss = common::gm_pilot_in_space(&ctx, 201, 93_100_001, ARCHON, HIGHSEC).await;
    let mut wingman = common::gm_pilot_in_space(&ctx, 202, 93_100_002, VEXOR, HIGHSEC).await;

    // Form fleet
    let fid = ok(&ctx, &mut boss.sess, "fleetObjectHandler", "CreateFleet", &[])
        .await
        .as_i64()
        .expect("fleetID");
    boss.sess.fleet_id = Some(fid);

    ok(
        &ctx,
        &mut boss.sess,
        "fleetObjectHandler",
        "Invite",
        &[int(wingman.char_id)],
    )
    .await;
    ok(
        &ctx,
        &mut wingman.sess,
        "fleetObjectHandler",
        "AcceptInvite",
        &[int(fid)],
    )
    .await;
    wingman.sess.fleet_id = Some(fid);

    // Spawn an NPC target on grid with Boss
    let npc_type_id = ctx
        .sde
        .types
        .iter()
        .find(|(_, t)| t.category_id == 11 && t.published)
        .map(|(&id, _)| id)
        .unwrap_or(10_000_001);
    let target_ball = spawn_specialized_npc_entity(
        &ctx,
        HIGHSEC,
        npc_type_id,
        "Pirate",
        4001,
        DVec3::new(1_000.0, 0.0, 0.0),
    )
    .await;

    // 1. Tagging without locking target must be rejected with TargetNotLocked
    let not_locked_err = rpc(
        &ctx,
        &mut boss.sess,
        "beyonce",
        "CmdFleetTagTarget",
        &[int(target_ball), PyRep::String("1".into())],
    )
    .await
    .expect_err("Tagging unlocked target must fail");
    assert_eq!(not_locked_err.code, "TargetNotLocked");

    // Lock target via authentic dogmaIM.AddTarget
    lock_target(&ctx, &mut boss, target_ball).await;

    // 2. Invalid tag (must be single char A-Z or 1-9) rejected with InvalidTargetTag
    let invalid_tag_err = rpc(
        &ctx,
        &mut boss.sess,
        "beyonce",
        "CmdFleetTagTarget",
        &[int(target_ball), PyRep::String("XYZ".into())],
    )
    .await
    .expect_err("Multi-char tag must fail");
    assert_eq!(invalid_tag_err.code, "InvalidTargetTag");

    // 3. Wingman (non-commander member) tagging must be rejected with FleetAccessDenied
    lock_target(&ctx, &mut wingman, target_ball).await;
    let denied_err = rpc(
        &ctx,
        &mut wingman.sess,
        "beyonce",
        "CmdFleetTagTarget",
        &[int(target_ball), PyRep::String("2".into())],
    )
    .await
    .expect_err("Non-commander tagging must fail");
    assert_eq!(denied_err.code, "FleetAccessDenied");

    // Drain initial notifications
    let _ = drain(&ctx, &mut wingman);

    // 4. Boss authentic target tagging with "1" broadcasts OnFleetStateChange to fleet
    let tag_res = rpc(
        &ctx,
        &mut boss.sess,
        "beyonce",
        "CmdFleetTagTarget",
        &[int(target_ball), PyRep::String("1".into())],
    )
    .await;
    assert!(tag_res.is_ok(), "Boss CmdFleetTagTarget('1') succeeds: {tag_res:?}");

    // Wingman receives OnFleetStateChange containing targetTags dict mapping target_ball -> "1"
    let wingman_events = drain(&ctx, &mut wingman);
    let state_event = wingman_events
        .iter()
        .find(|(ev, _)| ev == "OnFleetStateChange")
        .expect("Wingman must receive OnFleetStateChange");
    let state_dict = &state_event.1[0];
    let tags_dict = state_dict
        .dict_get("targetTags")
        .expect("targetTags dict in fleet state");
    match tags_dict {
        PyRep::Dict(entries) => {
            let found = entries
                .iter()
                .any(|(k, v)| k.as_i64() == Some(target_ball) && v.as_str() == Some("1"));
            assert!(found, "Target tag '1' must be present in targetTags dict: {entries:?}");
        }
        other => panic!("Expected Dict for targetTags, got {other:?}"),
    }

    // 5. Boss updates tag from "1" to "A"
    let _ = rpc(
        &ctx,
        &mut boss.sess,
        "beyonce",
        "CmdFleetTagTarget",
        &[int(target_ball), PyRep::String("A".into())],
    )
    .await;
    let wingman_events2 = drain(&ctx, &mut wingman);
    let state_event2 = wingman_events2
        .iter()
        .find(|(ev, _)| ev == "OnFleetStateChange")
        .expect("Wingman must receive updated OnFleetStateChange");
    match state_event2.1[0].dict_get("targetTags").unwrap() {
        PyRep::Dict(entries) => {
            let found_a = entries
                .iter()
                .any(|(k, v)| k.as_i64() == Some(target_ball) && v.as_str() == Some("A"));
            assert!(found_a, "Target tag updated to 'A': {entries:?}");
        }
        other => panic!("Expected Dict for targetTags, got {other:?}"),
    }

    // 6. Boss clears target tag with empty string ""
    let _ = rpc(
        &ctx,
        &mut boss.sess,
        "beyonce",
        "CmdFleetTagTarget",
        &[int(target_ball), PyRep::String("".into())],
    )
    .await;
    let wingman_events3 = drain(&ctx, &mut wingman);
    let state_event3 = wingman_events3
        .iter()
        .find(|(ev, _)| ev == "OnFleetStateChange")
        .expect("Wingman must receive tag cleared OnFleetStateChange");
    match state_event3.1[0].dict_get("targetTags").unwrap() {
        PyRep::Dict(entries) => {
            let has_tag = entries.iter().any(|(k, _)| k.as_i64() == Some(target_ball));
            assert!(!has_tag, "Target tag must be removed when cleared: {entries:?}");
        }
        other => panic!("Expected Dict for targetTags, got {other:?}"),
    }

    // 7. Retag target with "9", then destroy target -> clear_destroyed_target_tag removes it
    let _ = rpc(
        &ctx,
        &mut boss.sess,
        "beyonce",
        "CmdFleetTagTarget",
        &[int(target_ball), PyRep::String("9".into())],
    )
    .await;
    let _ = destroy_specialized_npc_and_queue_bounty(&ctx, HIGHSEC, target_ball, boss.char_id).await;

    // Fleet state targetTags queried via GetInitState no longer contains destroyed target
    let fleet_state = ok(&ctx, &mut boss.sess, "fleetObjectHandler", "GetInitState", &[]).await;
    let tags_after_death = fleet_state.dict_get("targetTags").expect("targetTags in GetInitState");
    match tags_after_death {
        PyRep::Dict(entries) => {
            let has_tag = entries.iter().any(|(k, _)| k.as_i64() == Some(target_ball));
            assert!(!has_tag, "Destroyed target tag must be cleared automatically: {entries:?}");
        }
        other => panic!("Expected Dict for targetTags, got {other:?}"),
    }
}

#[tokio::test]
async fn test_live_crab_beacon_activation_interference_and_metaliminal_storms() {

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let mut cap_pilot = common::gm_pilot_in_space(&ctx, 301, 94_100_001, ARCHON, NULLSEC).await;
    let mut sub_pilot = common::gm_pilot_in_space(&ctx, 302, 94_100_002, VEXOR, NULLSEC).await;

    // --- Part 1: Metaliminal Storms Movement (`feat:pve.metaliminal-storm-movement`) ---
    with_pve_special_reg(&ctx, |reg| init_roaming_storms(&ctx, reg));

    // Verify roaming storms list returned by map.GetAllRoamingWeatherSystems
    let weather_rsp = ok(&ctx, &mut cap_pilot.sess, "map", "GetAllRoamingWeatherSystems", &[]).await;
    let PyRep::List(storms_list) = weather_rsp else { panic!("Expected List for storms") };
    assert!(!storms_list.is_empty(), "Must have roaming weather systems in list");

    // Footprint test: core is center + 1-jump null-sec neighbors; periphery is 2-3 jumps away
    let (core, periph) = compute_storm_footprint(&ctx, NULLSEC);
    assert!(!core.is_empty(), "Core systems must include center");
    assert_eq!(core[0], NULLSEC, "Center system must be first in core");
    assert!(!periph.is_empty(), "Periphery must contain 2-3 jump neighbors");

    // Movement interval is strictly within the 24-48h window (25h..=47h)
    let move_interval = storm_move_interval_secs(1, 0);
    assert!(
        (90_000..=169_200).contains(&move_interval),
        "Storm move cadence must be within 24-48h: {move_interval}s"
    );

    // Advance clock by 36 hours and advance storms
    ctx.advance_secs(36 * 3_600);
    with_pve_special_reg(&ctx, |reg| {
        advance_roaming_storms(&ctx, reg, 36 * 3_600);
    });
    sync_metaliminal_storm_effects_and_sites(&ctx);

    // --- Part 2: CRAB Beacon Link & Signal Interference (`feat:pve.crab-link-activation`, `feat:pve.crab-ceq-signal-interference`) ---
    // Initial CEQ energy is 100
    let energy_before = eve_server::services::character_energy::get_character_energy(&ctx, cap_pilot.char_id).current_energy;
    assert_eq!(energy_before, 100, "Pilot starts with 100 CEQ");

    // Create CRAB beacon item in capital pilot's ship cargo
    let crab_item = common::slash_cmd(&ctx, &mut cap_pilot.sess, &format!("/createitem {TYPE_CRAB_BEACON} 1")).await;
    let beacon_item_id = crab_item.dict_get("itemID").and_then(|v| v.as_i64()).expect("beacon itemID");

    // Train Anchoring skill (11584) to anchor deployables
    common::slash_cmd(&ctx, &mut cap_pilot.sess, "/giveskill 11584 5").await;

    // Deploy and anchor CRAB beacon in space in NULLSEC via posMgr.AnchorStructure
    ok(
        &ctx,
        &mut cap_pilot.sess,
        "posMgr",
        "AnchorStructure",
        &[int(beacon_item_id)],
    )
    .await;

    // Initial system signal interference state: 12 activations remaining, 0% interference
    let initial_interf = ok(
        &ctx,
        &mut cap_pilot.sess,
        "solarsystemInterferenceMgr",
        "GetLocalInterferenceState",
        &[int(NULLSEC as i64)],
    )
    .await;
    assert_eq!(
        initial_interf.dict_get("crabActivationsRemaining").and_then(|v| v.as_i64()),
        Some(12),
        "Initial system interference allows 12 CRAB activations"
    );

    // Subcapital ship (Vexor) attempting InitiateLink is rejected with IneligibleShipForCrabLink
    let subcap_err = rpc(
        &ctx,
        &mut sub_pilot.sess,
        "beyonce",
        "CallComponentFromClient",
        &[
            int(beacon_item_id),
            PyRep::String("linkWithShip".into()),
            PyRep::String("InitiateLink".into()),
        ],
    )
    .await
    .expect_err("Subcapital must not be able to link to CRAB beacon");
    assert_eq!(subcap_err.code, "IneligibleShipForCrabLink");

    // Capital pilot (Archon) calls InitiateLink
    let link_res = rpc(
        &ctx,
        &mut cap_pilot.sess,
        "beyonce",
        "CallComponentFromClient",
        &[
            int(beacon_item_id),
            PyRep::String("linkWithShip".into()),
            PyRep::String("InitiateLink".into()),
        ],
    )
    .await;
    assert!(link_res.is_ok(), "Capital ship InitiateLink succeeds: {link_res:?}");

    // Verify capital ship is now CRAB linked and 30 CEQ was deducted
    let cap_ship_id = cap_pilot.sess.ship_id.unwrap();
    assert!(
        is_ship_crab_linked(&ctx, cap_ship_id, ctx.now_unix()),
        "Capital ship must be active in CRAB link state"
    );
    let energy_after = eve_server::services::character_energy::get_character_energy(&ctx, cap_pilot.char_id).current_energy;
    assert_eq!(
        energy_after,
        100 - CRAB_CEQ_COST,
        "CRAB link must consume {CRAB_CEQ_COST} CEQ"
    );

    // System signal interference increased: activations remaining decreased from 12 to 11
    let interf_after = ok(
        &ctx,
        &mut cap_pilot.sess,
        "solarsystemInterferenceMgr",
        "GetLocalInterferenceState",
        &[int(NULLSEC as i64)],
    )
    .await;
    assert_eq!(
        interf_after.dict_get("crabActivationsRemaining").and_then(|v| v.as_i64()),
        Some(11),
        "CRAB link must consume 1 system interference activation slot"
    );
    let level = interf_after.dict_get("interferenceLevel").and_then(|v| v.as_f64()).expect("interferenceLevel");
    assert!(
        (level - CRAB_INTERFERENCE_DELTA).abs() < 1e-4,
        "Interference level must increment by CRAB_INTERFERENCE_DELTA ({CRAB_INTERFERENCE_DELTA}): {level}"
    );

    // Double-link refusal: calling InitiateLink again while already linked is refused
    let double_err = rpc(
        &ctx,
        &mut cap_pilot.sess,
        "beyonce",
        "CallComponentFromClient",
        &[
            int(beacon_item_id),
            PyRep::String("linkWithShip".into()),
            PyRep::String("InitiateLink".into()),
        ],
    )
    .await
    .expect_err("Double link must be refused");
    assert!(
        matches!(double_err.code.as_str(), "ShipAlreadyLinkedToCrab" | "CrabBeaconAlreadyActive"),
        "Double link returns appropriate refusal error: {}",
        double_err.code
    );
}
