//! Live fleet formations, fleet boss handover, and corporation project contribution methods.
//!
//! Features covered:
//! - `feat:fleet.boss-handover`
//! - `feat:fleet.stored-formations-and-warp-shapes`
//! - `feat:corporation.corp-project-contribution-methods`

#![forbid(unsafe_code)]

use crate::common::{self, LivePilot};
use eve_destiny::DVec3;
use eve_machonet::SessionState;
use eve_proto::quasar::{EvePublicRequest, EvePublicResponse, GoogleAny};
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::quasar_gateway::{STATUS_FORBIDDEN, STATUS_OK};
use eve_server::router::ServerContext;
use eve_server::services::fleet_social::fleet_social_notify::handle_fleet_member_disconnect;
use eve_server::services::social_corp_fleet::social_corp_projects::goal_uuid_to_project_id;
use std::collections::HashMap;
use std::sync::Arc;

const HIGHSEC: u32 = 30_000_144; // Perimeter (0.9)
const _JITA_STATION: i64 = 60_003_760; // Jita 4-4
const VEXOR: u32 = 626;
const ARCHON: u32 = 23757;
const TRITANIUM: u32 = 34;
const PYERITE: u32 = 35;

fn paused_ctx() -> ServerContext {
    let sde = Arc::new(eve_sde::SdeRegistry::open(&eve_sde::SdeRegistry::default_db_path()).expect("sde"));
    let ctx = ServerContext::with_sde_and_repository(sde, Arc::new(eve_db::WorldRepository::with_demo_seed()));
    ctx.set_clock_paused(true);
    ctx
}

fn int(v: i64) -> PyRep {
    PyRep::Integer(v)
}

async fn call(ctx: &ServerContext, p: &mut LivePilot, svc: &str, method: &str, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(&mut p.sess, svc, None, method, args, &HashMap::new(), None).await
}

async fn call_kw(ctx: &ServerContext, p: &mut LivePilot, svc: &str, method: &str, args: &[PyRep], kw: &HashMap<String, PyRep>) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(&mut p.sess, svc, None, method, args, kw, None).await
}

async fn ok(ctx: &ServerContext, p: &mut LivePilot, svc: &str, method: &str, args: &[PyRep]) -> PyRep {
    call(ctx, p, svc, method, args).await.unwrap_or_else(|e| panic!("{svc}.{method} failed: {e:?}"))
}

async fn pos(ctx: &ServerContext, ship_id: i64) -> DVec3 {
    ctx.systems.get_or_spawn(HIGHSEC).get_ball_snapshot(ship_id).await.unwrap().expect("ball").1.position
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

fn boss(ctx: &ServerContext, fid: i64) -> i64 {
    ctx.db.with_snapshot(|s| s.fleets.get(&fid).map(|f| f.boss_id).unwrap_or(0))
}

async fn create_fleet(ctx: &ServerContext, p: &mut LivePilot) -> i64 {
    let fid = ok(ctx, p, "fleetObjectHandler", "CreateFleet", &[]).await.as_i64().expect("fleetID");
    p.sess.fleet_id = Some(fid);
    fid
}

async fn join_fleet(ctx: &ServerContext, fc: &mut LivePilot, member: &mut LivePilot, fid: i64) {
    ok(ctx, fc, "fleetObjectHandler", "Invite", &[int(member.char_id), PyRep::None, PyRep::None, int(4)]).await;
    ok(ctx, member, "fleetObjectHandler", "AcceptInvite", &[int(fid)]).await;
    member.sess.fleet_id = Some(fid);
}

async fn step(ctx: &ServerContext, secs: usize) {
    let sys = ctx.systems.get_or_spawn(HIGHSEC);
    for _ in 0..secs {
        sys.step_manual(1.0).await.unwrap();
    }
}

// --- Quasar protobuf serialization helpers ---

fn encode_varint(mut val: u64, buf: &mut Vec<u8>) {
    while val >= 0x80 {
        buf.push(((val as u8) & 0x7f) | 0x80);
        val >>= 7;
    }
    buf.push(val as u8);
}

fn encode_tag(field_num: u32, wire_type: u8, buf: &mut Vec<u8>) {
    encode_varint(((field_num as u64) << 3) | (wire_type as u64), buf);
}

fn encode_varint_field(field_num: u32, val: u64, buf: &mut Vec<u8>) {
    encode_tag(field_num, 0, buf);
    encode_varint(val, buf);
}

fn encode_bytes_field(field_num: u32, bytes: &[u8], buf: &mut Vec<u8>) {
    encode_tag(field_num, 2, buf);
    encode_varint(bytes.len() as u64, buf);
    buf.extend_from_slice(bytes);
}

fn encode_string_field(field_num: u32, s: &str, buf: &mut Vec<u8>) {
    encode_bytes_field(field_num, s.as_bytes(), buf);
}

pub(crate) fn quasar_call(ctx: &ServerContext, sess: &SessionState, type_name: &str, value: Vec<u8>) -> EvePublicResponse {
    let req = EvePublicRequest {
        payload: Some(GoogleAny {
            type_url: format!("type.googleapis.com/{type_name}"),
            value,
        }),
        ..Default::default()
    };
    ctx.quasar_gateway.handle_public_request(sess, &req, None, false, ctx.now_unix())
}

fn wrap_id(id: u64, path: &[u32]) -> Vec<u8> {
    let mut cur = Vec::new();
    encode_varint_field(1, id, &mut cur);
    for &tag in path {
        let mut outer = Vec::new();
        encode_bytes_field(tag, &cur, &mut outer);
        cur = outer;
    }
    cur
}

pub(crate) fn build_project_create_msg(name: &str, target_qty: u64, matcher: &str, filter: i64) -> Vec<u8> {
    let (cfg_field, sub_bytes) = match matcher {
        "DestroyNPCShips" => (31, wrap_id(filter as u64, &[1, 4, 2, 1])),
        "DeliverItem" => (37, wrap_id(filter as u64, &[1, 3, 2, 1])),
        "MineMaterials" => (25, wrap_id(filter as u64, &[1, 3, 2, 1])),
        "ManufactureItems" => (38, wrap_id(filter as u64, &[1, 3, 2, 1])),
        other => panic!("unsupported matcher {other}"),
    };
    let mut req = Vec::new();
    let mut cfg = Vec::new();
    encode_string_field(1, name, &mut req);
    encode_varint_field(3, target_qty, &mut req);
    encode_bytes_field(cfg_field, &sub_bytes, &mut cfg);
    encode_bytes_field(4, &cfg, &mut req);
    req
}

async fn gm_pilot_docked(ctx: &ServerContext, sess_id: u64, char_id: i64) -> SessionState {
    let kw = HashMap::new();
    let user = format!("live{char_id}");
    let pass = format!("{user}-secret");
    ctx.db.provision_gm_account(&user, &pass, char_id, &format!("Live Pilot {char_id}"));
    let mut sess = SessionState::new(sess_id);
    common::login_pilot(ctx, &mut sess, &user, &pass).await;
    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[int(char_id)], &kw, None)
        .await
        .expect("SelectCharacterID");
    sess
}

// parity: feat:fleet.boss-handover, feat:fleet.stored-formations-and-warp-shapes
#[tokio::test]
async fn test_live_fleet_boss_handover_and_formations() {
    eve_testkit::attest(&[
        "feat:fleet.boss-handover",
        "feat:fleet.stored-formations-and-warp-shapes",
    ]);
    let ctx = paused_ctx();
    let mut fc = common::gm_pilot_in_space(&ctx, 31, 91_300_001, ARCHON, HIGHSEC).await;
    let mut p2 = common::gm_pilot_in_space(&ctx, 32, 91_300_002, VEXOR, HIGHSEC).await;
    let mut p3 = common::gm_pilot_in_space(&ctx, 33, 91_300_003, VEXOR, HIGHSEC).await;
    let start = pos(&ctx, fc.ship_id).await;

    // 1. Formation query and fleet formation settings (`feat:fleet.stored-formations-and-warp-shapes`)
    let PyRep::Tuple(formations) = ok(&ctx, &mut fc, "beyonce", "GetFormations", &[]).await else { panic!("formations") };
    assert_eq!(formations.len(), 2, "GetFormations returns native _destiny LoadFormations tuple");

    let fid = create_fleet(&ctx, &mut fc).await;
    join_fleet(&ctx, &mut fc, &mut p2, fid).await;
    join_fleet(&ctx, &mut fc, &mut p3, fid).await;

    // Stored formation settings: Plane (id 1, spacing 8000m, size 20000m)
    let stored = ok(&ctx, &mut fc, "fleetObjectHandler", "SetFleetFormationSettings", &[int(1), PyRep::Real(8_000.0), PyRep::Real(20_000.0)]).await;
    assert_eq!(stored.dict_get("formationName"), Some(&PyRep::String("Plane".into())));
    assert_eq!(ok(&ctx, &mut fc, "fleetObjectHandler", "GetFormationSettings", &[]).await.dict_get("formationID"), Some(&int(1)));

    // Live fleet warp along +Z axis
    let wall_dest = [start.x, start.y, start.z + 500_000_000.0];
    ok(&ctx, &mut fc, "fleetObjectHandler", "FleetWarp", &[PyRep::Real(wall_dest[0]), PyRep::Real(wall_dest[1]), PyRep::Real(wall_dest[2]), PyRep::Real(0.0)]).await;
    step(&ctx, 90).await;

    let (w1, w2, _w3) = (pos(&ctx, fc.ship_id).await, pos(&ctx, p2.ship_id).await, pos(&ctx, p3.ship_id).await);
    assert!(w1.distance(start) > 400_000_000.0 && w2.distance(start) > 400_000_000.0, "both ships must complete warp");
    assert!(w1.distance(w2) >= 2_500.0 && w1.distance(w2) <= 25_000.0, "deterministic formation distance prevents bumps");
    assert!((w2.z - w1.z).abs() < 500.0, "plane formation spreads laterally in X/Y perpendicular to +Z warp");

    // Per-warp Arrow override (kwargs) along +X
    let arrow_kw = HashMap::from([("formationID".into(), int(3)), ("formationSpacing".into(), PyRep::Real(6_000.0))]);
    let arrow_args = [PyRep::Real(w1.x + 500_000_000.0), PyRep::Real(w1.y), PyRep::Real(w1.z), PyRep::Real(0.0)];
    call_kw(&ctx, &mut fc, "fleetObjectHandler", "FleetWarp", &arrow_args, &arrow_kw).await.expect("FleetWarp Arrow");
    step(&ctx, 90).await;

    let (a1, a2, a3) = (pos(&ctx, fc.ship_id).await, pos(&ctx, p2.ship_id).await, pos(&ctx, p3.ship_id).await);
    assert!(a2.x < a1.x - 4_000.0 && a3.x < a1.x - 4_000.0, "arrow wingmen trail FC along warp axis");
    assert!(a2.distance(a3) > 8_000.0, "arrow wingmen split to opposite sides");

    // 2. Fleet boss handover (`feat:fleet.boss-handover`)
    let _ = drain(&ctx, &mut p2);
    // Explicit transfer via MakeLeader
    ok(&ctx, &mut fc, "fleetObjectHandler", "MakeLeader", &[int(p2.char_id)]).await;
    assert_eq!(boss(&ctx, fid), p2.char_id, "MakeLeader transfers boss to p2");
    assert!(drain(&ctx, &mut p2).iter().any(|(ev, _)| ev == "OnFleetMemberChanged"));

    // Transfer back
    ok(&ctx, &mut p2, "fleetObjectHandler", "MakeLeader", &[int(fc.char_id)]).await;
    assert_eq!(boss(&ctx, fid), fc.char_id);

    // Disconnected member cannot receive boss role
    let p2_id = p2.char_id;
    handle_fleet_member_disconnect(&ctx, &mut p2.sess);
    let offline_err = call(&ctx, &mut fc, "fleetObjectHandler", "MakeLeader", &[int(p2_id)]).await.expect_err("offline target");
    assert_eq!(offline_err.code, "TargetMemberOffline");

    // Boss leaves fleet -> automatic handover to the remaining connected member p3
    let _ = drain(&ctx, &mut p3);
    ok(&ctx, &mut fc, "fleetObjectHandler", "LeaveFleet", &[]).await;
    assert_eq!(boss(&ctx, fid), p3.char_id, "LeaveFleet automatically assigns boss to connected member p3");
    assert!(drain(&ctx, &mut p3).iter().any(|(ev, _)| ev == "OnFleetMemberChanged"));

    // Boss socket disconnect -> automatic handover
    let mut p4 = common::gm_pilot_in_space(&ctx, 34, 91_300_004, VEXOR, HIGHSEC).await;
    join_fleet(&ctx, &mut p3, &mut p4, fid).await;
    assert_eq!(boss(&ctx, fid), p3.char_id);

    let _ = drain(&ctx, &mut p4);
    handle_fleet_member_disconnect(&ctx, &mut p3.sess);
    assert_eq!(boss(&ctx, fid), p4.char_id, "boss disconnect automatically reassigns boss to p4");
    assert!(drain(&ctx, &mut p4).iter().any(|(ev, a)| ev == "OnChangeFleetBoss" && a.get(1) == Some(&int(p4.char_id))));
}

#[tokio::test]
async fn test_live_corp_project_contribution_methods() {
    let ctx = paused_ctx();

    // 1. Client spoofing attempt: non-admin SetCurrentProgressRequest must be rejected with STATUS_FORBIDDEN
    let mut non_gm = SessionState::new(41);
    common::login_pilot(&ctx, &mut non_gm, "other", "other-secret").await;
    ctx.dispatch_rpc(&mut non_gm, "charUnboundMgr", None, "SelectCharacterID", &[int(90_000_002)], &HashMap::new(), None)
        .await
        .expect("SelectCharacterID non-GM");

    let mut spoof_req = Vec::new();
    let mut fake_uuid = Vec::new();
    encode_bytes_field(1, &[0xAA; 16], &mut fake_uuid);
    encode_bytes_field(1, &fake_uuid, &mut spoof_req);
    encode_varint_field(3, 999, &mut spoof_req);
    let spoof_rsp = quasar_call(&ctx, &non_gm, "eve_public.goal.api.SetCurrentProgressRequest", spoof_req);
    assert_eq!(spoof_rsp.status_code, STATUS_FORBIDDEN, "Client-reported progress must be forbidden");

    // 2. DeliverItem contribution method via invbroker.DeliverToCorpHangar
    let mut corp_pilot = gm_pilot_docked(&ctx, 42, 91_300_021).await;
    common::slash_cmd(&ctx, &mut corp_pilot, "/giveskill 3363 5").await;
    ctx.advance_secs(11);
    let add_corp = ctx.dispatch_rpc(
        &mut corp_pilot,
        "corpRegistry",
        None,
        "AddCorporation",
        &[PyRep::String("Live Corp Projects Co".into()), PyRep::String("LCP".into()), PyRep::Real(0.05)],
        &HashMap::new(),
        None,
    ).await.expect("AddCorporation");
    let corp_id = add_corp.dict_get("corporationID").and_then(|v| v.as_i64()).expect("corporationID");
    corp_pilot.corporation_id = Some(corp_id);

    // Create DeliverItem corp project for Tritanium (type 34), target 100 units
    let deliv_msg = build_project_create_msg("Deliver Tritanium to Corp", 100, "DeliverItem", TRITANIUM as i64);
    let deliv_rsp = quasar_call(&ctx, &corp_pilot, "eve_public.goal.api.CreateRequest", deliv_msg);
    assert_eq!(deliv_rsp.status_code, STATUS_OK, "Create DeliverItem project");
    let pid_deliv = goal_uuid_to_project_id(&deliv_rsp.payload.unwrap().value[4..20]).expect("project id");

    // Deliver non-matching item (Pyerite, type 35) -> progress does NOT advance
    let pyer_item = common::slash_cmd(&ctx, &mut corp_pilot, &format!("/createitem {PYERITE} 25")).await;
    let pyer_id = pyer_item.dict_get("itemID").and_then(|v| v.as_i64()).expect("pyerite itemID");
    ctx.dispatch_rpc(&mut corp_pilot, "invbroker", None, "DeliverToCorpHangar", &[int(pyer_id), int(1)], &HashMap::new(), None)
        .await
        .expect("Deliver non-matching item to corp hangar");
    assert_eq!(
        ctx.db.with_snapshot(|s| s.corp_projects.get(&pid_deliv).unwrap().current_quantity),
        0,
        "Non-matching item delivery must not advance Tritanium project"
    );

    // Deliver matching item (Tritanium, type 34, 50 units) -> progress advances by 50
    let trit_item = common::slash_cmd(&ctx, &mut corp_pilot, &format!("/createitem {TRITANIUM} 50")).await;
    let trit_id = trit_item.dict_get("itemID").and_then(|v| v.as_i64()).expect("trit itemID");
    ctx.dispatch_rpc(&mut corp_pilot, "invbroker", None, "DeliverToCorpHangar", &[int(trit_id), int(1)], &HashMap::new(), None)
        .await
        .expect("Deliver Tritanium to corp hangar");
    assert_eq!(
        ctx.db.with_snapshot(|s| s.corp_projects.get(&pid_deliv).unwrap().current_quantity),
        50,
        "Authentic invbroker.DeliverToCorpHangar must advance project by 50"
    );

    // 3. DestroyNPCShips contribution method via live combat
    // Create DestroyNPCShips corp project filtered to HIGHSEC (30_000_144), target 3
    let npc_msg = build_project_create_msg("Destroy Highsec Pirate NPCs", 3, "DestroyNPCShips", HIGHSEC as i64);
    let npc_rsp = quasar_call(&ctx, &corp_pilot, "eve_public.goal.api.CreateRequest", npc_msg);
    assert_eq!(npc_rsp.status_code, STATUS_OK, "Create DestroyNPCShips project");
    let pid_npc = goal_uuid_to_project_id(&npc_rsp.payload.unwrap().value[4..20]).expect("npc project id");

    // Spawn and destroy live NPC in non-matching system (30_000_142)
    let npc_type_id = ctx.sde.types.iter().find(|(_, t)| t.category_id == 11 && t.published).map(|(&id, _)| id).unwrap_or(10_000_001);
    let npc_other = eve_server::services::combat_crimewatch::combat_npc_ai::spawn_specialized_npc_entity(
        &ctx, 30_000_142, npc_type_id, "Pirate", 4001, DVec3::new(1_000.0, 0.0, 0.0)
    ).await;
    let _ = eve_server::services::combat_crimewatch::combat_npc_ai::destroy_specialized_npc_and_queue_bounty(
        &ctx, 30_000_142, npc_other, corp_pilot.character_id.unwrap()
    ).await;

    assert_eq!(
        ctx.db.with_snapshot(|s| s.corp_projects.get(&pid_npc).unwrap().current_quantity),
        0,
        "NPC kill in non-matching solar system must not advance project progress"
    );

    // Spawn and destroy live NPC in matching system (HIGHSEC 30_000_144)
    let npc_matching = eve_server::services::combat_crimewatch::combat_npc_ai::spawn_specialized_npc_entity(
        &ctx, HIGHSEC, npc_type_id, "Pirate", 4001, DVec3::new(1_000.0, 0.0, 0.0)
    ).await;
    let _ = eve_server::services::combat_crimewatch::combat_npc_ai::destroy_specialized_npc_and_queue_bounty(
        &ctx, HIGHSEC, npc_matching, corp_pilot.character_id.unwrap()
    ).await;

    assert_eq!(
        ctx.db.with_snapshot(|s| s.corp_projects.get(&pid_npc).unwrap().current_quantity),
        1,
        "Authentic NPC destruction in matching system must increment project progress by 1"
    );
}
