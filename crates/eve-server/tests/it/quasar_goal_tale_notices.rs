//! Corporation project (goal) and tale notices emitted by real server state transitions:
//! goal create / progress / rename / close / delete Quasar requests, NPC-kill contributions,
//! and the admin tale lifecycle (`eve.tale.api.StartTaleRequest` / `StopTalesRequest`).

use crate::quasar_progression_complete::{build_sess, quasar_rpc};
use eve_machonet::SessionState;
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, EvePublicNotice, WireMessage,
};
use eve_proto::PyRep;
use eve_server::quasar_gateway::STATUS_OK;
use eve_server::router::ServerContext;
use eve_server::services::combat_crimewatch::combat_npc_ai::{
    destroy_specialized_npc_and_queue_bounty, spawn_specialized_npc_entity,
};
use std::collections::HashMap;
use tokio::sync::mpsc::Receiver;

const CORP: u32 = 98_000_777;
const OTHER_CORP: u32 = 98_000_778;

pub(crate) async fn subscribe(ctx: &ServerContext, sess: &mut SessionState) -> Receiver<EvePublicNotice> {
    let kw: HashMap<String, PyRep> = HashMap::new();
    ctx.dispatch_rpc(sess, "publicGatewaySvc", None, "Subscribe", &[], &kw, None).await.expect("Subscribe");
    let mut rx = ctx.quasar_gateway.take_stream_receiver(sess.session_id).expect("stream receiver");
    while rx.try_recv().is_ok() {}
    rx
}

/// Drains the stream and returns `(type name, payload)` of every notice in `prefix`.
pub(crate) fn drain(rx: &mut Receiver<EvePublicNotice>, prefix: &str) -> Vec<(String, WireMessage)> {
    let mut out = Vec::new();
    while let Ok(n) = rx.try_recv() {
        let p = n.payload.expect("notice payload");
        let name = p.normalized_type_name().to_string();
        if name.starts_with(prefix) {
            out.push((name, WireMessage::decode(&p.value).expect("decode notice")));
        }
    }
    out
}

pub(crate) fn sub(wm: &WireMessage, field: u32) -> WireMessage {
    let f = wm.fields.iter().find(|f| f.field_number == field).unwrap_or_else(|| panic!("field {field}"));
    WireMessage::decode(f.value.as_bytes().expect("bytes")).expect("nested")
}
pub(crate) fn varint(wm: &WireMessage, field: u32) -> Option<u64> {
    wm.fields.iter().find(|f| f.field_number == field).and_then(|f| f.value.as_varint())
}
pub(crate) fn bytes(wm: &WireMessage, field: u32) -> Vec<u8> {
    wm.fields.iter().find(|f| f.field_number == field).and_then(|f| f.value.as_bytes()).expect("bytes").to_vec()
}
fn text(wm: &WireMessage, field: u32) -> String {
    String::from_utf8(bytes(wm, field)).expect("utf8")
}

fn goal_req(uuid: &[u8], extra: impl FnOnce(&mut Vec<u8>)) -> Vec<u8> {
    let (mut out, mut id) = (Vec::new(), Vec::new());
    encode_bytes_field(1, uuid, &mut id);
    encode_bytes_field(1, &id, &mut out);
    extra(&mut out);
    out
}

pub(crate) fn seq(field: u32, id: u64, out: &mut Vec<u8>) {
    let mut b = Vec::new();
    encode_varint_field(1, id, &mut b);
    encode_bytes_field(field, &b, out);
}

async fn kill_rat(ctx: &ServerContext, killer: i64) {
    let npc = spawn_specialized_npc_entity(ctx, 30_000_142, 10035, "BeltRat", 1, Default::default()).await;
    destroy_specialized_npc_and_queue_bounty(ctx, 30_000_142, npc, killer).await;
}

#[tokio::test]
async fn goal_notices_follow_project_lifecycle_and_contributions() {
    // parity: feat:quasar.goal, quasar:eveProto/generated/eve_public/goal/api/notices_pb2.pyj, quasar:eveProto/generated/eve_public/goal/contribution/api/notices_pb2.pyj
    eve_testkit::attest(&[
        "feat:quasar.goal",
        "quasar:eveProto/generated/eve_public/goal/api/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/goal/contribution/api/notices_pb2.pyj",
    ]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    ctx.db.with_mut_snapshot(|s| {
        let template = s.characters[&90_000_001].clone();
        for (cid, corp) in [(90_000_001i64, CORP), (90_000_002, CORP), (90_000_003, OTHER_CORP)] {
            let ch = s.characters.entry(cid).or_insert_with(|| template.clone());
            ch.character_id = cid;
            ch.corporation_id = corp as i64;
        }
    });
    let mut creator = build_sess(51_001, 90_000_001, CORP);
    let mut mate = build_sess(51_002, 90_000_002, CORP);
    let mut outsider = build_sess(51_003, 90_000_003, OTHER_CORP);
    outsider.role = 0;
    let mut rx_creator = subscribe(&ctx, &mut creator).await;
    let mut rx_mate = subscribe(&ctx, &mut mate).await;
    let mut rx_out = subscribe(&ctx, &mut outsider).await;

    // CreateRequest { name = 1, description = 2, desired_progress = 3 }
    let mut create = Vec::new();
    encode_string_field(1, "Belt Clearance", &mut create);
    encode_string_field(2, "Clear the belts of rats", &mut create);
    encode_varint_field(3, 3, &mut create);
    let rsp = quasar_rpc(&ctx, &mut creator, "eve_public.goal.api.CreateRequest", create).await;
    assert_eq!(rsp.status_code, STATUS_OK, "{}", rsp.status_message);
    let uuid = bytes(&sub(&WireMessage::decode(&rsp.payload.unwrap().value).unwrap(), 1), 1);
    assert_eq!(uuid.len(), 16);

    let created = drain(&mut rx_mate, "eve_public.goal");
    assert_eq!(created.len(), 1, "corp mate receives exactly the CreatedNotice");
    assert_eq!(created[0].0, "eve_public.goal.api.CreatedNotice");
    assert_eq!(bytes(&sub(&created[0].1, 1), 1), uuid);
    let attrs = sub(&created[0].1, 2);
    assert_eq!(text(&attrs, 2), "Belt Clearance");
    assert_eq!(varint(&sub(&attrs, 4), 1), Some(90_000_001), "creator character");
    assert_eq!((varint(&sub(&attrs, 5), 1), varint(&sub(&attrs, 5), 2)), (Some(3), Some(0)), "desired 3, current 0");
    assert_eq!(varint(&sub(&sub(&attrs, 9), 1), 1), Some(CORP as u64), "organization = owning corporation");
    assert!(drain(&mut rx_out, "eve_public.goal").is_empty(), "other corporations never see the project");

    // Manual progress set -> ProgressedNotice (no completion below target).
    let set = goal_req(&uuid, |b| encode_varint_field(3, 1, b));
    let set_rsp = quasar_rpc(&ctx, &mut creator, "eve_public.goal.api.SetCurrentProgressRequest", set).await;
    assert_eq!(set_rsp.status_code, STATUS_OK);
    assert_eq!(set_rsp.payload.as_ref().map(|p| p.normalized_type_name()), Some("eve_public.goal.api.SetCurrentProgressResponse"));
    let progressed = drain(&mut rx_mate, "eve_public.goal");
    let names: Vec<&str> = progressed.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, ["eve_public.goal.api.ProgressedNotice"]);
    assert_eq!(varint(&progressed[0].1, 2), Some(1));
    let stale_cas = goal_req(&uuid, |b| { encode_varint_field(2, 0, b); encode_varint_field(3, 2, b); });
    assert_eq!(quasar_rpc(&ctx, &mut creator, "eve_public.goal.api.SetCurrentProgressRequest", stale_cas).await.status_code, eve_server::quasar_gateway::STATUS_CONFLICT);
    let same = goal_req(&uuid, |b| { encode_varint_field(2, 1, b); encode_varint_field(3, 1, b); });
    assert_eq!(quasar_rpc(&ctx, &mut creator, "eve_public.goal.api.SetCurrentProgressRequest", same).await.status_code, STATUS_OK);
    assert!(drain(&mut rx_mate, "eve_public.goal").is_empty(), "re-setting an unchanged value emits no ProgressedNotice");

    // Two server-observed NPC kills by a corp mate contribute 1 -> 2 -> 3 and complete the goal.
    drain(&mut rx_creator, "eve_public.goal");
    kill_rat(&ctx, 90_000_002).await;
    kill_rat(&ctx, 90_000_002).await;
    let contrib = drain(&mut rx_creator, "eve_public.goal");
    let names: Vec<&str> = contrib.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, [
        "eve_public.goal.contribution.api.ContributedNotice",
        "eve_public.goal.contribution.api.ContributedNotice",
        "eve_public.goal.api.CompletedNotice",
    ]);
    let (c1, c2) = (&contrib[0].1, &contrib[1].1);
    assert_eq!(varint(&sub(c1, 2), 1), Some(90_000_002), "contributor is the killer");
    assert_eq!(bytes(&sub(c1, 3), 1), uuid);
    assert_eq!((varint(c1, 4), varint(c1, 5)), (Some(1), Some(2)));
    assert_eq!((varint(c2, 4), varint(c2, 5)), (Some(2), Some(3)));
    assert_eq!(bytes(&sub(&contrib[2].1, 1), 1), uuid);
    kill_rat(&ctx, 90_000_002).await;
    assert!(drain(&mut rx_creator, "eve_public.goal").is_empty(), "a completed goal takes no further contributions");
    assert_eq!(drain(&mut rx_mate, "eve_public.goal").len(), 3, "the contributing corp mate sees the same 3 notices");
    assert!(drain(&mut rx_out, "eve_public.goal").is_empty());

    // Contributor summary queries on eve_public.goal.contribution.api reflect the 2 contributed kills.
    let g_sum = quasar_rpc(&ctx, &mut creator, "eve_public.goal.contribution.api.GetContributorSummariesForGoalRequest", goal_req(&uuid, |_| {})).await;
    assert_eq!(g_sum.status_code, STATUS_OK);
    let g_sum_wm = WireMessage::decode(&g_sum.payload.unwrap().value).unwrap();
    assert_eq!(varint(&sub(&g_sum_wm, 1), 2), Some(2), "corp mate added 2 progress");
    assert_eq!(quasar_rpc(&ctx, &mut outsider, "eve_public.goal.contribution.api.GetContributorSummariesForGoalRequest", goal_req(&uuid, |_| {})).await.status_code, 403);
    let my_sum = quasar_rpc(&ctx, &mut mate, "eve_public.goal.contribution.api.GetMyContributorSummaryForGoalRequest", goal_req(&uuid, |_| {})).await;
    assert_eq!(varint(&sub(&WireMessage::decode(&my_sum.payload.unwrap().value).unwrap(), 1), 2), Some(2));
    let my_all = quasar_rpc(&ctx, &mut mate, "eve_public.goal.contribution.api.GetMyContributorSummariesRequest", Vec::new()).await;
    assert_eq!(varint(&sub(&WireMessage::decode(&my_all.payload.unwrap().value).unwrap(), 1), 2), Some(2));

    // Admin rename / re-describe -> NameChangedNotice / DescriptionChangedNotice.
    let rename = goal_req(&uuid, |b| encode_string_field(2, "Belt Clearance II", b));
    assert_eq!(quasar_rpc(&ctx, &mut creator, "eve.goal.api.SetNameRequest", rename).await.status_code, STATUS_OK);
    let redesc = goal_req(&uuid, |b| encode_string_field(2, "Second sweep", b));
    assert_eq!(quasar_rpc(&ctx, &mut creator, "eve.goal.api.SetDescriptionRequest", redesc).await.status_code, STATUS_OK);
    let renamed = drain(&mut rx_mate, "eve_public.goal");
    assert_eq!(renamed.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(), [
        "eve_public.goal.api.NameChangedNotice",
        "eve_public.goal.api.DescriptionChangedNotice",
    ]);
    assert_eq!(text(&renamed[0].1, 2), "Belt Clearance II");
    assert_eq!(text(&renamed[1].1, 2), "Second sweep");

    // Close (once) -> ClosedNotice naming the closer; Delete -> DeletedNotice.
    for _ in 0..2 {
        let close = quasar_rpc(&ctx, &mut creator, "eve_public.goal.api.CloseRequest", goal_req(&uuid, |_| {})).await;
        assert_eq!(close.status_code, STATUS_OK);
    }
    let del = quasar_rpc(&ctx, &mut creator, "eve_public.goal.api.DeleteRequest", goal_req(&uuid, |_| {})).await;
    assert_eq!(del.status_code, STATUS_OK);
    let ended = drain(&mut rx_mate, "eve_public.goal");
    assert_eq!(ended.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(), [
        "eve_public.goal.api.ClosedNotice",
        "eve_public.goal.api.DeletedNotice",
    ]);
    assert_eq!(varint(&sub(&ended[0].1, 2), 1), Some(90_000_001), "closer");
    assert_eq!(bytes(&sub(&ended[1].1, 1), 1), uuid);
    assert!(drain(&mut rx_out, "eve_public.goal").is_empty());
}

#[tokio::test]
async fn tale_notices_follow_admin_tale_lifecycle() {
    // parity: quasar:eveProto/generated/eve_public/tale/api/notices_pb2.pyj
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut gm = build_sess(52_001, 90_000_001, CORP);
    let mut pilot = build_sess(52_002, 90_000_002, OTHER_CORP);
    pilot.role = 0;
    let mut rx_pilot = subscribe(&ctx, &mut pilot).await;

    let start = |template: u64, system: Option<u64>| {
        let mut b = Vec::new();
        seq(1, template, &mut b);
        if let Some(s) = system {
            seq(2, s, &mut b);
        }
        encode_varint_field(4, 1, &mut b); // immediate_start
        b
    };
    let count = |template: u64| {
        let mut b = Vec::new();
        seq(1, template, &mut b);
        b
    };

    for system in [30_000_142u64, 30_000_144] {
        let rsp = quasar_rpc(&ctx, &mut gm, "eve.tale.api.StartTaleRequest", start(5158, Some(system))).await;
        assert_eq!(rsp.status_code, STATUS_OK, "{}", rsp.status_message);
    }
    assert_eq!(quasar_rpc(&ctx, &mut gm, "eve.tale.api.StartTaleRequest", start(5158, None)).await.status_code, 400);
    assert_eq!(quasar_rpc(&ctx, &mut gm, "eve.tale.api.StartTaleRequest", start(5158, Some(1))).await.status_code, 404);
    assert_eq!(quasar_rpc(&ctx, &mut pilot, "eve.tale.api.StartTaleRequest", start(5158, Some(30_000_142))).await.status_code, 403);

    let started = drain(&mut rx_pilot, "eve_public.tale");
    assert_eq!(started.len(), 2, "every subscribed pilot learns of both started tales");
    let mut tale_ids = Vec::new();
    for ((name, wm), system) in started.iter().zip([30_000_142u64, 30_000_144]) {
        assert_eq!(name, "eve_public.tale.api.StartedNotice");
        assert_eq!(varint(&sub(wm, 2), 1), Some(5158), "spawn template");
        assert_eq!(varint(&sub(wm, 3), 1), Some(system), "solar system");
        tale_ids.push(varint(&sub(wm, 1), 1).expect("tale id"));
    }
    assert_ne!(tale_ids[0], tale_ids[1], "each tale gets its own identifier");

    let active = |rsp: eve_proto::quasar::EvePublicResponse| varint(&WireMessage::decode(&rsp.payload.unwrap().value).unwrap(), 1).unwrap_or(0);
    assert_eq!(active(quasar_rpc(&ctx, &mut gm, "eve.tale.api.GetActiveTalesCountRequest", count(5158)).await), 2);
    assert_eq!(active(quasar_rpc(&ctx, &mut gm, "eve.tale.api.GetActiveTalesCountRequest", count(7001)).await), 0);

    // Stopping a template with no running tales emits nothing; stopping 5158 stops both.
    assert_eq!(quasar_rpc(&ctx, &mut gm, "eve.tale.api.StopTalesRequest", count(7001)).await.status_code, STATUS_OK);
    assert!(drain(&mut rx_pilot, "eve_public.tale").is_empty());
    assert_eq!(quasar_rpc(&ctx, &mut gm, "eve.tale.api.StopTalesRequest", count(5158)).await.status_code, STATUS_OK);
    let mut stopped: Vec<(u64, u64)> = drain(&mut rx_pilot, "eve_public.tale")
        .iter()
        .map(|(name, wm)| {
            assert_eq!(name, "eve_public.tale.api.StoppedNotice");
            assert_eq!(varint(&sub(wm, 2), 1), Some(5158));
            (varint(&sub(wm, 1), 1).unwrap(), varint(&sub(wm, 3), 1).unwrap())
        })
        .collect();
    stopped.sort_unstable();
    let mut expected = vec![(tale_ids[0], 30_000_142), (tale_ids[1], 30_000_144)];
    expected.sort_unstable();
    assert_eq!(stopped, expected);
    assert_eq!(active(quasar_rpc(&ctx, &mut gm, "eve.tale.api.GetActiveTalesCountRequest", count(5158)).await), 0);
}

#[tokio::test]
async fn incursion_tale_notices_follow_seeding_and_mothership_defeat() {
    // parity: quasar:eveProto/generated/eve_public/tale/api/notices_pb2.pyj
    use eve_destiny::DamageVector;
    use eve_server::services::pve_special_sites::{ensure_special_sites_spawned, with_pve_special_reg};
    let ctx = ServerContext::new();
    let mut pilot = build_sess(53_001, 90_000_001, CORP);
    let mut rx = subscribe(&ctx, &mut pilot).await;

    // The first map RPC that needs world events seeds the incursions -> one StartedNotice each.
    let kw: HashMap<String, PyRep> = HashMap::new();
    ctx.dispatch_rpc(&mut pilot, "map", None, "GetEdencomFortressSystems", &[], &kw, None).await.expect("map RPC");
    let mut seeded: Vec<(u64, u64)> = with_pve_special_reg(&ctx, |reg| {
        reg.incursions.values().map(|i| (i.tale_id as u64, i.staging_solar_system_id as u64)).collect()
    });
    seeded.sort_unstable();
    let mut started: Vec<(u64, u64)> = drain(&mut rx, "eve_public.tale")
        .iter()
        .map(|(name, wm)| {
            assert_eq!(name, "eve_public.tale.api.StartedNotice");
            assert_eq!(varint(&sub(wm, 2), 1), Some(5158), "incursion spawn template");
            (varint(&sub(wm, 1), 1).unwrap(), varint(&sub(wm, 3), 1).unwrap())
        })
        .collect();
    started.sort_unstable();
    assert!(!seeded.is_empty());
    assert_eq!(started, seeded, "one StartedNotice per seeded incursion tale at its staging system");

    // Defeat one incursion: 4 HQ waves drain influence, then the Kundalini Manifest dies.
    ensure_special_sites_spawned(&ctx).await;
    assert!(drain(&mut rx, "eve_public.tale").is_empty(), "re-initialisation does not restart tales");
    let (cid, tale_id, staging, hq_sys) = with_pve_special_reg(&ctx, |reg| {
        let (cid, inc) = reg.incursions.iter().next().unwrap();
        (*cid, inc.tale_id as u64, inc.staging_solar_system_id as u64, inc.hq_solar_system_id)
    });
    let hq = ctx.systems.get_or_spawn(hq_sys);
    let boss_of = |tier: &'static str| {
        with_pve_special_reg(&ctx, |reg| reg.incursions[&cid].sites.iter().find(|s| s.tier == tier).unwrap().boss_npc_ball_id)
    };
    let dmg = DamageVector { em: 100_000.0, thermal: 100_000.0, kinetic: 100_000.0, explosive: 100_000.0 };
    for _ in 0..4 {
        let (_, dead, _) = hq.apply_damage_with_attacker(boss_of("Headquarters"), dmg, Some(90_000_001)).await.unwrap().unwrap();
        assert!(dead);
    }
    assert!(drain(&mut rx, "eve_public.tale").is_empty(), "the tale keeps running until the mothership dies");
    let (_, dead, _) = hq.apply_damage_with_attacker(boss_of("Mothership"), dmg, Some(90_000_001)).await.unwrap().unwrap();
    assert!(dead);
    let stopped = drain(&mut rx, "eve_public.tale");
    assert_eq!(stopped.len(), 1);
    assert_eq!(stopped[0].0, "eve_public.tale.api.StoppedNotice");
    assert_eq!(varint(&sub(&stopped[0].1, 1), 1), Some(tale_id));
    assert_eq!(varint(&sub(&stopped[0].1, 2), 1), Some(5158));
    assert_eq!(varint(&sub(&stopped[0].1, 3), 1), Some(staging));

    // A second completion report for the same ended incursion (e.g. ctx wrapper after the death
    // hook) must not announce the stop again.
    let ms_site = with_pve_special_reg(&ctx, |reg| reg.incursions[&cid].sites.iter().find(|s| s.tier == "Mothership").unwrap().site_id.clone());
    let again = eve_server::services::pve_incursion_sites::IncursionCompletionResult {
        site_id: ms_site, tier: "Mothership".into(), fleet_size: 1, fleet_scale_factor: 1.0, isk_per_pilot: 0,
        concord_lp_per_pilot: 0, influence: 0.0, state: "Ended".into(), mothership_spawned: true, incursion_ended: true,
    };
    eve_server::services::pve_incursion_sites::publish_incursion_tale_stopped_if_ended(&ctx.quasar_gateway, &ctx.db, &again);
    assert!(drain(&mut rx, "eve_public.tale").is_empty(), "StoppedNotice is published at most once per incursion");
}
