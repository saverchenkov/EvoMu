//! Live integration tests for Lifecycle, Omega Weekly Free SP, and Notifications.

use crate::common::{gm_pilot_in_space, pin_clock, slash_cmd, TEST_CLOCK_BASE_UNIX};
use crate::quasar_info_panels_and_plex_payments::encode_seq_id;
use crate::quasar_progression_complete::quasar_rpc;
use eve_machonet::SessionState;
use eve_proto::quasar::{encode_bytes_field, encode_string_field, encode_varint_field, EvePublicResponse};
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::quasar_gateway::STATUS_OK;
use eve_server::router::ServerContext;
use glam::DVec3;
use std::collections::HashMap;

const PERIMETER_SYS: u32 = 30_000_144;
const TYPE_VELATOR: u32 = 606;
const TYPE_RAITARU: u32 = 35_825;
const TYPE_MFG_PLANT: u32 = 35_878;
const TYPE_FUEL_BLOCK: u32 = 4051;
const TYPE_ANTIMATTER_S_BP: u32 = 1138;

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

async fn quasar_call(
    ctx: &ServerContext,
    sess: &mut SessionState,
    msg_name: &str,
    value: Vec<u8>,
) -> EvePublicResponse {
    quasar_rpc(ctx, sess, msg_name, value).await
}

fn drain_char(ctx: &ServerContext, sess: &mut SessionState, char_id: i64) -> Vec<(String, Vec<PyRep>)> {
    let mut out = sess.drain_notifications();
    for n in ctx.drain_notifications_for_char(char_id) {
        if !out.iter().any(|(e, a)| e == &n.event && a == &n.args) {
            out.push((n.event, n.args));
        }
    }
    out
}

#[tokio::test]
async fn test_live_lifecycle_corp_quit_cleanup() {
    // parity: feat:lifecycle.corp-quit-cleanup
    eve_testkit::attest(&["feat:lifecycle.corp-quit-cleanup"]);

    let ctx = ServerContext::new();
    pin_clock(&ctx);
    let mut ceo = gm_pilot_in_space(&ctx, 51, 93_500_001, TYPE_VELATOR, PERIMETER_SYS).await;
    let mut member = gm_pilot_in_space(&ctx, 52, 93_500_002, TYPE_VELATOR, PERIMETER_SYS).await;
    let m_bloodline_npc = ctx.sde.bloodlines[&1].corporation_id;

    // Dock CEO to found player corporation
    let st_id = ctx
        .sde
        .stations
        .values()
        .find(|s| s.solar_system_id == PERIMETER_SYS)
        .map(|s| s.station_id)
        .unwrap();
    let sys = ctx.systems.get_or_spawn(PERIMETER_SYS);
    sys.set_ball_position(ceo.ship_id, ctx.sde.stations[&st_id].position + DVec3::new(300.0, 0.0, 0.0)).await;
    sys.step_manual(0.1).await.unwrap();
    rpc(&ctx, &mut ceo.sess, "stationSvc", "Dock", &[PyRep::Integer(st_id)]).await.unwrap();
    slash_cmd(&ctx, &mut ceo.sess, "/giveisk 50000000").await;
    ctx.advance_secs(12);

    slash_cmd(&ctx, &mut ceo.sess, "/giveskill 3363 1").await;
    let corp_res = rpc(
        &ctx,
        &mut ceo.sess,
        "corpRegistry",
        "AddCorporation",
        &[
            PyRep::String("Cleanup Test Corp".into()),
            PyRep::String("CLNUP".into()),
            PyRep::String("Live quit test".into()),
        ],
    )
    .await
    .expect("AddCorporation");
    let corp_id = corp_res
        .dict_get("corporationID")
        .or_else(|| corp_res.dict_get("corpID"))
        .and_then(|v| v.as_i64())
        .unwrap_or_else(|| ceo.sess.corporation_id.unwrap());
    ceo.sess.corporation_id = Some(corp_id);
    ctx.advance_secs(12);

    // Member docks at station and joins player corp, receives title + pending 24h role grant + custom account key + corp chat
    let st_p = ctx.sde.stations.values().find(|s| s.solar_system_id == PERIMETER_SYS).map(|s| s.station_id).unwrap();
    let sys_p = ctx.systems.get_or_spawn(PERIMETER_SYS);
    sys_p.set_ball_position(member.ship_id, ctx.sde.stations[&st_p].position + DVec3::new(300.0, 0.0, 0.0)).await;
    sys_p.step_manual(0.1).await.unwrap();
    rpc(&ctx, &mut member.sess, "stationSvc", "Dock", &[PyRep::Integer(st_p)]).await.expect("member dock");
    let app1 = rpc(&ctx, &mut member.sess, "corpRegistry", "InsertApplication", &[PyRep::Integer(corp_id), PyRep::String("join".into())])
        .await
        .expect("InsertApplication");
    let app1_id = app1.dict_get("applicationID").and_then(|v| v.as_i64()).unwrap();
    ctx.advance_secs(12);
    rpc(&ctx, &mut ceo.sess, "corpRegistry", "UpdateApplicationOffer", &[PyRep::Integer(app1_id), PyRep::String("Accepted".into())])
        .await
        .expect("accept application");
    member.sess.corporation_id = Some(corp_id);
    ctx.advance_secs(12);

    rpc(&ctx, &mut ceo.sess, "corpRegistry", "UpdateTitle", &[PyRep::Integer(1), PyRep::String("Logistics Officer".into())])
        .await
        .expect("UpdateTitle");
    // Grant immediate ROLE_ACCOUNTANT bit 8 + bit 21 (24h pending hangar role grant) + title bit 1
    rpc(&ctx, &mut ceo.sess, "corpRegistry", "UpdateMember", &[PyRep::Integer(member.char_id), PyRep::Integer((1_i64 << 8) | (1_i64 << 21)), PyRep::Integer(1)])
        .await
        .expect("UpdateMember title + pending grant");
    rpc(&ctx, &mut member.sess, "corpRegistry", "SetAccountKey", &[PyRep::Integer(1001)])
        .await
        .expect("SetAccountKey 1001");
    ctx.advance_secs(12);
    let _ = rpc(&ctx, &mut member.sess, "LSC", "JoinChannel", &[PyRep::String(format!("corpid:{corp_id}"))]).await;

    // CEO kick refused while pending 24h role grant is active
    let blocked = rpc(&ctx, &mut ceo.sess, "corpRegistry", "KickOutMember", &[PyRep::Integer(member.char_id)])
        .await
        .unwrap_err();
    assert_eq!(blocked.code, "MemberHasRoles");

    // Member self-quits via ResignFromCorp -> moved to SDE bloodline NPC corp, roles/pending/titles/account key/channels cleaned
    let quit = rpc(&ctx, &mut member.sess, "corpRegistry", "ResignFromCorp", &[])
        .await
        .expect("ResignFromCorp");
    assert_eq!(quit.dict_get("removed").and_then(|v| v.as_bool()), Some(true));
    ctx.advance_secs(12);

    let mem_info = rpc(&ctx, &mut ceo.sess, "corpRegistry", "GetMember", &[PyRep::Integer(member.char_id)]).await;
    assert!(mem_info.is_err() || mem_info.as_ref().ok().is_some_and(|r| matches!(r, PyRep::None)));
    let pub_char = rpc(&ctx, &mut member.sess, "charMgr", "GetPublicInfo", &[PyRep::Integer(member.char_id)]).await.unwrap();
    assert_eq!(pub_char.dict_get("corporationID").and_then(|v| v.as_i64()), Some(m_bloodline_npc));
    assert_eq!(member.sess.corporation_id, Some(m_bloodline_npc));

    let chans = rpc(&ctx, &mut member.sess, "LSC", "GetChannels", &[]).await.unwrap();
    let chans_dbg = format!("{chans:?}");
    assert!(!chans_dbg.contains(&format!("corpid:{corp_id}")));
    assert!(chans_dbg.contains(&format!("corpid:{m_bloodline_npc}")));
}

#[tokio::test]
async fn test_live_lifecycle_industry_pause_and_many() {
    // parity: feat:lifecycle.industry-pause-and-many
    eve_testkit::attest(&["feat:lifecycle.industry-pause-and-many"]);

    let ctx = ServerContext::new();
    pin_clock(&ctx);
    let mut builder = gm_pilot_in_space(&ctx, 61, 93_600_001, TYPE_VELATOR, PERIMETER_SYS).await;
    let mut co_worker = gm_pilot_in_space(&ctx, 62, 93_600_002, TYPE_VELATOR, PERIMETER_SYS).await;

    // Found corp & recruit co_worker
    let st_id = ctx.sde.stations.values().find(|s| s.solar_system_id == PERIMETER_SYS).map(|s| s.station_id).unwrap();
    let sys = ctx.systems.get_or_spawn(PERIMETER_SYS);
    sys.set_ball_position(builder.ship_id, ctx.sde.stations[&st_id].position + DVec3::new(300.0, 0.0, 0.0)).await;
    sys.step_manual(0.1).await.unwrap();
    rpc(&ctx, &mut builder.sess, "stationSvc", "Dock", &[PyRep::Integer(st_id)]).await.unwrap();
    slash_cmd(&ctx, &mut builder.sess, "/giveisk 500000000").await;
    slash_cmd(&ctx, &mut co_worker.sess, "/giveisk 500000000").await;
    slash_cmd(&ctx, &mut builder.sess, "/giveskill 3363 1").await;
    ctx.advance_secs(12);
    let add_c = rpc(&ctx, &mut builder.sess, "corpRegistry", "AddCorporation", &[PyRep::String("Indy Test Corp".into()), PyRep::String("INDYT".into()), PyRep::String("Indy".into())]).await.unwrap();
    let corp_id = add_c.dict_get("corporationID").and_then(|v| v.as_i64()).unwrap();
    builder.sess.corporation_id = Some(corp_id);
    ctx.advance_secs(12);
    sys.set_ball_position(co_worker.ship_id, ctx.sde.stations[&st_id].position + DVec3::new(300.0, 0.0, 0.0)).await;
    sys.step_manual(0.1).await.unwrap();
    rpc(&ctx, &mut co_worker.sess, "stationSvc", "Dock", &[PyRep::Integer(st_id)]).await.unwrap();
    let app2 = rpc(&ctx, &mut co_worker.sess, "corpRegistry", "InsertApplication", &[PyRep::Integer(corp_id), PyRep::String("hi".into())]).await.unwrap();
    let app2_id = app2.dict_get("applicationID").and_then(|v| v.as_i64()).unwrap();
    ctx.advance_secs(12);
    rpc(&ctx, &mut builder.sess, "corpRegistry", "UpdateApplicationOffer", &[PyRep::Integer(app2_id), PyRep::String("Accepted".into())]).await.unwrap();
    co_worker.sess.corporation_id = Some(corp_id);
    ctx.advance_secs(7 * 86_400 + 60);
    rpc(&ctx, &mut builder.sess, "stationSvc", "Undock", &[]).await.unwrap();

    // Anchor Upwell Raitaru, install Core + Fuel + Standup Manufacturing Plant I
    let open_pos = DVec3::new(1_200_000_000_000.0, 0.0, 0.0);
    sys.set_ball_position(builder.ship_id, open_pos).await;
    sys.step_manual(0.1).await.unwrap();
    let r_item = slash_cmd(&ctx, &mut builder.sess, &format!("/createitem {TYPE_RAITARU} 1")).await.dict_get("itemID").and_then(|v| v.as_i64()).unwrap();
    let anchor_pos = PyRep::Tuple(vec![PyRep::Real(open_pos.x + 60_000.0), PyRep::Real(0.0), PyRep::Real(0.0)]);
    let anc = rpc(&ctx, &mut builder.sess, "structureDeployment", "Anchor", &[PyRep::Integer(r_item), anchor_pos, PyRep::String("Indy Raitaru".into())]).await.expect("Anchor Raitaru");
    let raitaru_id = anc.dict_get("structureID").and_then(|v| v.as_i64()).unwrap();
    ctx.advance_secs(905);
    let _ = rpc(&ctx, &mut builder.sess, "structureDirectory", "GetMyDockableStructures", &[]).await;
    ctx.advance_secs(86_405);
    let _ = rpc(&ctx, &mut builder.sess, "structureDirectory", "GetMyDockableStructures", &[]).await;

    // Dock in Raitaru & install Core + Fuel + Standup Manufacturing Plant I + stock BPOs + Tritanium
    sys.set_ball_position(builder.ship_id, open_pos + DVec3::new(60_500.0, 0.0, 0.0)).await;
    sys.step_manual(0.1).await.unwrap();
    rpc(&ctx, &mut builder.sess, "stationSvc", "Dock", &[PyRep::Integer(raitaru_id)]).await.unwrap();
    let core_tid = anc.dict_get("quantumCoreTypeID").and_then(|v| v.as_i64()).unwrap_or(56203);
    let core_id = slash_cmd(&ctx, &mut builder.sess, &format!("/createitem {core_tid} 1")).await.dict_get("itemID").and_then(|v| v.as_i64()).unwrap();
    rpc(&ctx, &mut builder.sess, "invbroker", "Add", &[PyRep::Integer(core_id), PyRep::Integer(raitaru_id), PyRep::Integer(180)]).await.unwrap();
    ctx.advance_secs(905);
    let _ = rpc(&ctx, &mut builder.sess, "structureDirectory", "GetMyDockableStructures", &[]).await;
    let fuel_id = slash_cmd(&ctx, &mut builder.sess, &format!("/createitem {TYPE_FUEL_BLOCK} 2000")).await.dict_get("itemID").and_then(|v| v.as_i64()).unwrap();
    rpc(&ctx, &mut builder.sess, "invbroker", "Add", &[PyRep::Integer(fuel_id), PyRep::Integer(raitaru_id), PyRep::Integer(172)]).await.unwrap();
    let mfg_id = slash_cmd(&ctx, &mut builder.sess, &format!("/createitem {TYPE_MFG_PLANT} 1")).await.dict_get("itemID").and_then(|v| v.as_i64()).unwrap();
    rpc(&ctx, &mut builder.sess, "invbroker", "Add", &[PyRep::Integer(mfg_id), PyRep::Integer(raitaru_id), PyRep::Integer(164)]).await.unwrap();
    rpc(&ctx, &mut builder.sess, "dogmaIM", "SetModuleOnline", &[PyRep::Integer(raitaru_id), PyRep::Integer(mfg_id)]).await.expect("online mfg");
    slash_cmd(&ctx, &mut builder.sess, "/giveskill 3380 5").await; // Industry 5
    slash_cmd(&ctx, &mut builder.sess, "/giveskill 3387 5").await; // Mass Production 5 (6 mfg slots)
    for mat in &ctx.sde.blueprints[&TYPE_ANTIMATTER_S_BP].materials {
        let mid = slash_cmd(&ctx, &mut builder.sess, &format!("/createitem {} {}", mat.type_id, u64::from(mat.quantity.max(1)) * 100)).await.dict_get("itemID").and_then(|v| v.as_i64()).unwrap();
        rpc(&ctx, &mut builder.sess, "invbroker", "Add", &[PyRep::Integer(mid), PyRep::Integer(raitaru_id), PyRep::Integer(4)]).await.unwrap();
    }

    let mut bps = Vec::new();
    for _ in 0..4 {
        let bp = slash_cmd(&ctx, &mut builder.sess, &format!("/createitem {TYPE_ANTIMATTER_S_BP} 1")).await.dict_get("itemID").and_then(|v| v.as_i64()).unwrap();
        rpc(&ctx, &mut builder.sess, "invbroker", "Add", &[PyRep::Integer(bp), PyRep::Integer(raitaru_id), PyRep::Integer(4)]).await.unwrap();
        bps.push(bp);
    }

    // Install short Job A (1 run) & long Job B (25 runs) + Job C (1 run)
    let install_one = |bp_id: i64, runs: i64| {
        PyRep::keyval([
            ("blueprintID", PyRep::Integer(bp_id)),
            ("blueprintTypeID", PyRep::Integer(i64::from(TYPE_ANTIMATTER_S_BP))),
            ("activityID", PyRep::Integer(1)),
            ("runs", PyRep::Integer(runs)),
            ("facilityID", PyRep::Integer(raitaru_id)),
        ])
    };
    let ja = rpc(&ctx, &mut builder.sess, "industryManager", "InstallJob", &[install_one(bps[0], 1)]).await.expect("Job A");
    let jb = rpc(&ctx, &mut builder.sess, "industryManager", "InstallJob", &[install_one(bps[1], 25)]).await.expect("Job B");
    let jc = rpc(&ctx, &mut builder.sess, "industryManager", "InstallJob", &[install_one(bps[2], 1)]).await.expect("Job C");
    let ja_id = ja.dict_get("jobID").and_then(|v| v.as_i64()).unwrap();
    let jb_id = jb.dict_get("jobID").and_then(|v| v.as_i64()).unwrap();
    let jc_id = jc.dict_get("jobID").and_then(|v| v.as_i64()).unwrap();
    let dur_a = ja.dict_get("durationSeconds").and_then(|v| v.as_i64()).unwrap() as u64;
    let dur_b = jb.dict_get("durationSeconds").and_then(|v| v.as_i64()).unwrap();

    // Advance until Job A & C finish (end_time <= t_offline), then offline service at t_offline
    ctx.advance_secs(dur_a + 10);
    let t_offline = ctx.now_unix() as i64;
    rpc(&ctx, &mut builder.sess, "dogmaIM", "TakeModuleOffline", &[PyRep::Integer(raitaru_id), PyRep::Integer(mfg_id)]).await.expect("offline service");

    // Job A & C ended before offline -> NOT paused; Job B -> immediately Paused at t_offline
    let ga = rpc(&ctx, &mut builder.sess, "industryManager", "GetJob", &[PyRep::Integer(ja_id)]).await.unwrap();
    assert_ne!(ga.dict_get("status").and_then(|v| v.as_str()), Some("Paused"));
    let gb = rpc(&ctx, &mut builder.sess, "industryManager", "GetJob", &[PyRep::Integer(jb_id)]).await.unwrap();
    assert_eq!(gb.dict_get("status").and_then(|v| v.as_str()), Some("Paused"));
    assert_eq!(gb.dict_get("pauseUnix").and_then(|v| v.as_i64()), Some(t_offline));

    // Advance 600s offline -> re-online -> duration extended by exact 600s -> complete all 3 with CompleteManyJobs
    ctx.advance_secs(600);
    rpc(&ctx, &mut builder.sess, "dogmaIM", "SetModuleOnline", &[PyRep::Integer(raitaru_id), PyRep::Integer(mfg_id)]).await.expect("online service");
    let gb2 = rpc(&ctx, &mut builder.sess, "industryManager", "GetJob", &[PyRep::Integer(jb_id)]).await.unwrap();
    assert_eq!(gb2.dict_get("status").and_then(|v| v.as_str()), Some("Active"));
    assert_eq!(gb2.dict_get("durationSeconds").and_then(|v| v.as_i64()), Some(dur_b + 600));

    ctx.advance_secs((dur_b as u64) + 50);
    let PyRep::List(delivered) = rpc(
        &ctx,
        &mut builder.sess,
        "industryManager",
        "CompleteManyJobs",
        &[PyRep::List(vec![PyRep::Integer(ja_id), PyRep::Integer(jb_id), PyRep::Integer(jc_id)])],
    )
    .await
    .expect("CompleteManyJobs")
    else {
        panic!("expected delivered list");
    };
    assert_eq!(delivered.len(), 3);
    let got_ids: Vec<i64> = delivered.iter().filter_map(|d| d.dict_get("jobID").and_then(|v| v.as_i64())).collect();
    assert_eq!(got_ids, vec![ja_id, jb_id, jc_id]);
    for jid in got_ids {
        let st = rpc(&ctx, &mut builder.sess, "industryManager", "GetJob", &[PyRep::Integer(jid)]).await.unwrap();
        assert_eq!(st.dict_get("status").and_then(|v| v.as_str()), Some("Delivered"));
    }

    // Ex-member cannot CancelJob, CompleteJob, or CompleteManyJobs on corp job
    slash_cmd(&ctx, &mut co_worker.sess, "/giveskill 3380 5").await;
    let _ = rpc(&ctx, &mut co_worker.sess, "stationSvc", "Undock", &[]).await;
    ctx.advance_secs(31);
    slash_cmd(&ctx, &mut co_worker.sess, &format!("/tr {PERIMETER_SYS}")).await;
    ctx.advance_secs(31);
    sys.set_ball_position(co_worker.ship_id, open_pos + DVec3::new(60_500.0, 0.0, 0.0)).await;
    sys.step_manual(0.1).await.unwrap();
    rpc(&ctx, &mut co_worker.sess, "stationSvc", "Dock", &[PyRep::Integer(raitaru_id)]).await.unwrap();
    let corp_job_req = PyRep::keyval([
        ("blueprintID", PyRep::Integer(bps[3])),
        ("blueprintTypeID", PyRep::Integer(i64::from(TYPE_ANTIMATTER_S_BP))),
        ("activityID", PyRep::Integer(1)),
        ("runs", PyRep::Integer(1)),
        ("facilityID", PyRep::Integer(raitaru_id)),
        ("useCorp", PyRep::Bool(true)),
    ]);
    let corp_job = rpc(&ctx, &mut builder.sess, "industryManager", "InstallJob", &[corp_job_req]).await.expect("corp job");
    let corp_job_id = corp_job.dict_get("jobID").and_then(|v| v.as_i64()).unwrap();

    ctx.advance_secs(12);
    rpc(&ctx, &mut builder.sess, "corpRegistry", "KickOutMember", &[PyRep::Integer(co_worker.char_id)]).await.unwrap();
    assert_eq!(rpc(&ctx, &mut co_worker.sess, "industryManager", "CancelJob", &[PyRep::Integer(corp_job_id)]).await.unwrap_err().code, "JobOwnershipViolation");
    ctx.advance_secs(5_000);
    assert_eq!(rpc(&ctx, &mut co_worker.sess, "industryManager", "CompleteJob", &[PyRep::Integer(corp_job_id)]).await.unwrap_err().code, "JobOwnershipViolation");
    assert_eq!(rpc(&ctx, &mut co_worker.sess, "industryManager", "CompleteManyJobs", &[PyRep::List(vec![PyRep::Integer(corp_job_id)])]).await.unwrap_err().code, "JobOwnershipViolation");
}

#[tokio::test]
async fn test_live_skills_omega_weekly_free_sp() {
    // parity: feat:skills.omega-weekly-free-sp
    eve_testkit::attest(&["feat:skills.omega-weekly-free-sp"]);

    let ctx = ServerContext::new();
    pin_clock(&ctx);
    let mut gm = gm_pilot_in_space(&ctx, 71, 93_700_001, TYPE_VELATOR, PERIMETER_SYS).await;
    let pilot = gm_pilot_in_space(&ctx, 72, 93_700_002, TYPE_VELATOR, PERIMETER_SYS).await;
    let char_id = pilot.char_id;
    let user_id = pilot.sess.user_id.unwrap();
    slash_cmd(&ctx, &mut gm.sess, &format!("/setomega {user_id} 1")).await;
    let mut sess = pilot.sess;
    let _ = drain_char(&ctx, &mut sess, char_id);

    // 1. Admin creates category, 15,000 Free SP product, and 0-PLEX Omega weekly offer in NES catalog
    fn read_varint(b: &mut &[u8]) -> u64 {
        let (mut v, mut s) = (0_u64, 0);
        while let Some((&byte, rest)) = b.split_first() {
            *b = rest;
            v |= u64::from(byte & 0x7f) << s;
            if byte & 0x80 == 0 {
                break;
            }
            s += 7;
        }
        v
    }
    let nested_id = |resp: EvePublicResponse| -> u64 {
        let raw = resp.payload.expect("payload").value;
        let mut s = raw.as_slice();
        let _ = read_varint(&mut s);
        let len = read_varint(&mut s) as usize;
        let mut inner = &s[..len];
        let _ = read_varint(&mut inner);
        read_varint(&mut inner)
    };

    let (mut cat_a, mut cat_r) = (Vec::new(), Vec::new());
    encode_string_field(1, "Pilot Services", &mut cat_a);
    encode_bytes_field(1, &cat_a, &mut cat_r);
    let cat_resp = quasar_call(&ctx, &mut gm.sess, "eve.store.admin.api.CreateCategoryRequest", cat_r).await;
    assert_eq!(cat_resp.status_code, STATUS_OK);
    let cat_id = nested_id(cat_resp);

    let (mut item_m, mut prod_a, mut prod_r) = (Vec::new(), Vec::new(), Vec::new());
    encode_bytes_field(1, &encode_seq_id(0), &mut item_m);
    encode_varint_field(2, 15_000, &mut item_m);
    encode_string_field(1, "15,000 Free Skill Points", &mut prod_a);
    encode_bytes_field(2, &item_m, &mut prod_a);
    encode_varint_field(1, 1, &mut prod_r);
    encode_bytes_field(2, &prod_a, &mut prod_r);
    let prod_resp = quasar_call(&ctx, &mut gm.sess, "eve.store.admin.api.CreateProductRequest", prod_r).await;
    assert_eq!(prod_resp.status_code, STATUS_OK);
    let prod_id = nested_id(prod_resp);

    let (mut price_m, mut link_m, mut off_a, mut off_r) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    encode_varint_field(1, 0, &mut price_m);
    encode_bytes_field(1, &encode_seq_id(prod_id), &mut link_m);
    encode_string_field(1, "Omega Weekly 15,000 SP", &mut off_a);
    encode_bytes_field(5, &price_m, &mut off_a);
    encode_string_field(16, "omega-weekly-free-sp", &mut off_a);
    encode_bytes_field(17, &encode_seq_id(cat_id), &mut off_a);
    encode_bytes_field(18, &link_m, &mut off_a);
    encode_bytes_field(1, &off_a, &mut off_r);
    let off_resp = quasar_call(&ctx, &mut gm.sess, "eve.store.admin.api.CreateOfferRequest", off_r).await;
    assert_eq!(off_resp.status_code, STATUS_OK);

    let PyRep::List(offers) = rpc(&ctx, &mut sess, "storeManager", "get_offers", &[]).await.unwrap() else {
        panic!("offers list");
    };
    let offer_id = offers
        .iter()
        .find(|o| o.dict_get("name").and_then(|v| v.as_str()) == Some("Omega Weekly 15,000 SP"))
        .and_then(|o| o.dict_get("offerID").and_then(|v| v.as_i64()))
        .expect("Omega Weekly 15,000 SP offer in catalog");

    // 2. Alpha account is refused
    let can_alpha = rpc(&ctx, &mut sess, "storeManager", "can_purchase_offer", &[PyRep::Integer(offer_id)]).await.unwrap().as_bool().unwrap();
    assert!(!can_alpha);
    let err_alpha = rpc(&ctx, &mut sess, "storeManager", "buy_offer", &[PyRep::Integer(offer_id), PyRep::String("PLX".into()), PyRep::Integer(1)]).await.unwrap_err();
    assert_eq!(err_alpha.code, "OmegaRequired");

    // 3. Upgrade to Omega via /setomega (zero snapshot mutation) & claim 15,000 unallocated SP
    let exp_unix = ctx.now_unix() + 30 * 86_400;
    slash_cmd(&ctx, &mut gm.sess, &format!("/setomega {user_id} {exp_unix}")).await;
    let sp0 = rpc(&ctx, &mut sess, "skillMgr2", "GetFreeSkillPoints", &[]).await.unwrap().as_f64().unwrap_or(0.0);
    assert!(rpc(&ctx, &mut sess, "storeManager", "can_purchase_offer", &[PyRep::Integer(offer_id)]).await.unwrap().as_bool().unwrap());

    let claim1 = rpc(&ctx, &mut sess, "storeManager", "buy_offer", &[PyRep::Integer(offer_id), PyRep::String("PLX".into()), PyRep::Integer(1)]).await.expect("first claim");
    assert_eq!(claim1.dict_get("freeSkillPointsGranted").and_then(|v| v.as_i64()), Some(15_000));
    let sp1 = rpc(&ctx, &mut sess, "skillMgr2", "GetFreeSkillPoints", &[]).await.unwrap().as_f64().unwrap();
    assert!((sp1 - (sp0 + 15_000.0)).abs() < 1e-6);
    assert!(drain_char(&ctx, &mut sess, char_id).iter().any(|(e, _)| e == "OnFreeSkillPointsChanged"));

    // Duplicate claim in same week rejected
    assert!(!rpc(&ctx, &mut sess, "storeManager", "can_purchase_offer", &[PyRep::Integer(offer_id)]).await.unwrap().as_bool().unwrap());
    let dup = rpc(&ctx, &mut sess, "storeManager", "buy_offer", &[PyRep::Integer(offer_id), PyRep::String("PLX".into()), PyRep::Integer(1)]).await.unwrap_err();
    assert_eq!(dup.code, "WeeklyOfferAlreadyClaimed");

    // 4. Exact Tuesday 11:00 UTC downtime reset boundary (471,600s offset in 604,800s epoch week)
    let now = ctx.now_unix();
    let rem_in_week = 604_800 - ((now.saturating_add(604_800 - 471_600)) % 604_800);
    ctx.advance_secs(rem_in_week.saturating_sub(1));
    assert!(!rpc(&ctx, &mut sess, "storeManager", "can_purchase_offer", &[PyRep::Integer(offer_id)]).await.unwrap().as_bool().unwrap());

    ctx.advance_secs(2);
    assert!(rpc(&ctx, &mut sess, "storeManager", "can_purchase_offer", &[PyRep::Integer(offer_id)]).await.unwrap().as_bool().unwrap());
    let claim2 = rpc(&ctx, &mut sess, "storeManager", "buy_offer", &[PyRep::Integer(offer_id), PyRep::String("PLX".into()), PyRep::Integer(1)]).await.expect("second week claim");
    assert_eq!(claim2.dict_get("freeSkillPointsGranted").and_then(|v| v.as_i64()), Some(15_000));
    let sp2 = rpc(&ctx, &mut sess, "skillMgr2", "GetFreeSkillPoints", &[]).await.unwrap().as_f64().unwrap();
    assert!((sp2 - (sp1 + 15_000.0)).abs() < 1e-6);
}

#[tokio::test]
async fn test_live_notify_chat_bans_and_cluster_shutdown() {

    let ctx = ServerContext::new();
    pin_clock(&ctx);
    assert_eq!(ctx.now_unix(), TEST_CLOCK_BASE_UNIX);
    let mut gm = gm_pilot_in_space(&ctx, 81, 93_800_001, TYPE_VELATOR, PERIMETER_SYS).await;
    let mut target = gm_pilot_in_space(&ctx, 82, 93_800_002, TYPE_VELATOR, PERIMETER_SYS).await;
    let _ = drain_char(&ctx, &mut target.sess, target.char_id);
    let _ = gm.sess.drain_notifications();

    let chan = PyRep::String(format!("solarsystemid2:{PERIMETER_SYS}"));
    let _ = rpc(&ctx, &mut target.sess, "LSC", "JoinChannel", &[chan.clone()]).await;

    // 1. Permanent chat ban -> OnCharacterPermanentlyBanned + chat blocked
    slash_cmd(&ctx, &mut gm.sess, &format!("/chatban {} perm", target.char_id)).await;
    assert!(drain_char(&ctx, &mut target.sess, target.char_id).iter().any(|(e, _)| e == "OnCharacterPermanentlyBanned"));
    let muted_err = rpc(&ctx, &mut target.sess, "LSC", "SendMessage", &[chan.clone(), PyRep::String("hi".into())])
        .await
        .unwrap_err();
    assert!(muted_err.code.contains("Mute") || muted_err.code.contains("Ban"), "unexpected error: {muted_err:?}");

    // 2. Unban -> OnCharacterUnbanned + chat restored
    slash_cmd(&ctx, &mut gm.sess, &format!("/chatunban {}", target.char_id)).await;
    assert!(drain_char(&ctx, &mut target.sess, target.char_id).iter().any(|(e, _)| e == "OnCharacterUnbanned"));
    rpc(&ctx, &mut target.sess, "LSC", "SendMessage", &[chan.clone(), PyRep::String("restored".into())])
        .await
        .expect("chat restored after unban");

    // 3. Temporary ban -> OnCharacterTemporarilyBanned with exact FILETIME expiry + automatic lapse after expiry
    let expected_ft = ctx.now_blueos_filetime_ticks() + 1_800 * 10_000_000;
    slash_cmd(&ctx, &mut gm.sess, &format!("/chatban {} 1800", target.char_id)).await;
    let temp_evs = drain_char(&ctx, &mut target.sess, target.char_id);
    assert!(temp_evs.iter().any(|(e, args)| e == "OnCharacterTemporarilyBanned" && args.first().and_then(|v| v.as_i64()) == Some(expected_ft)));
    assert!(rpc(&ctx, &mut target.sess, "LSC", "SendMessage", &[chan.clone(), PyRep::String("blocked".into())]).await.is_err());

    ctx.advance_secs(1_801);
    rpc(&ctx, &mut target.sess, "LSC", "SendMessage", &[chan, PyRep::String("lapsed".into())])
        .await
        .expect("temporary ban lapses after expiry");

    // 4. Cluster shutdown initiated -> OnClusterShutdownInitiated with label, FILETIME target, countdown seconds
    let shut_ft = ctx.now_blueos_filetime_ticks() + 180 * 10_000_000;
    slash_cmd(&ctx, &mut gm.sess, "/shutdown 180").await;
    let gm_evs = gm.sess.drain_notifications();
    assert!(gm_evs.iter().any(|(e, args)| {
        e == "OnClusterShutdownInitiated"
            && args.len() == 3
            && args[0] == PyRep::String("UI/Shared/ClusterShutdownInitiated".into())
            && args[1] == PyRep::Integer(shut_ft)
            && args[2] == PyRep::Integer(180)
    }));
}
