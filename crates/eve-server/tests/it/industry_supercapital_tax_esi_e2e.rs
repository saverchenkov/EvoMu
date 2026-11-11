//! Live non-GM end-to-end tests for Parity Shard 1 Industry mechanics & ESI routes:
//! - Sovereign null-sec Supercapital (Titan) construction + Sov Hub upgrade (`81623`) + Jump Freighter & High-Sec gate
//! - Blueprint ME research exponential duration, material reduction, facility bonuses, and system cost index scaling
//! - Corp industry single wallet debit (no double debit), EIV adjusted prices, 0.25% NPC & Upwell facility tax, 4% SCC
//! - Service-offline job pause timing, `CompleteManyJobs`, `CancelJob`, ex-member denial, and SQLite reload

#![forbid(unsafe_code)]

use crate::common;
use crate::corp_member_audit_reinforce_and_quit::found_corp_with_member;
use crate::industry_live_support::{
    acquire, board_loaded_freighter, bom, buy,
    fund_pilots, install, int, items_at, login, move_item, nearest_system, real, rpc,
    small_mineral_bp, train_skills, upwell_cargo, JITA_STATION, PERIMETER_SYS, PILOT_1,
    PILOT_2,
};
use crate::esi_live_e2e_support::{
    assert_etag_304, cleanup_sqlite_files, esi_call,
    issue_token, reload_repo, temp_db_path,
};
use eve_db::corp_roles;
use eve_destiny::{DVec3, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::esi::{EsiRouter, OpenApiSpec};
use eve_server::router::ServerContext;
use eve_server::services::station_structure::structure_lifecycle as life;
use eve_server::world_seeder::seed_full_market_catalog;
use serde_json::json;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, OnceLock};

pub(crate) fn shared_market_base() -> &'static (Arc<eve_sde::SdeRegistry>, eve_db::WorldStateSnapshot) {
    static BASE: OnceLock<(Arc<eve_sde::SdeRegistry>, eve_db::WorldStateSnapshot)> = OnceLock::new();
    BASE.get_or_init(|| {
        let _ = (
            crate::esi_live_e2e_support::dock_station_slow,
            crate::esi_live_e2e_support::deploy_online_upwell_slow,
        );
        let sde = Arc::new(eve_sde::SdeRegistry::with_canonical_seed());
        let db = Arc::new(eve_db::WorldRepository::with_demo_seed());
        let _ = eve_server::world_seeder::seed_dual_anchor_markets(&db, &sde);
        seed_full_market_catalog(&db, &sde);
        (sde, db.with_snapshot(Clone::clone))
    })
}

pub(crate) fn fast_full_market_ctx() -> ServerContext {
    let (sde, snap) = shared_market_base();
    let repo = Arc::new(eve_db::WorldRepository::new_empty());
    repo.with_mut_snapshot(|s| *s = snap.clone());
    ServerContext::with_sde_and_repository(Arc::clone(sde), repo)
        .with_seeded_rng(eve_server::rng::TEST_MODE_DEFAULT_SEED)
}

pub(crate) async fn warp_to_fast(ctx: &ServerContext, sess: &mut SessionState, target: i64, dist: f64) {
    let sys = sess.solar_system_id.unwrap();
    let beyonce = ctx.bind_object(sess, "beyonce", sys as i64).ok();
    match ctx
        .dispatch_rpc(
            sess,
            "beyonce",
            beyonce,
            "CmdWarpToStuff",
            &[PyRep::String("item".into()), PyRep::Integer(target), PyRep::Real(dist)],
            &HashMap::new(),
            None,
        )
        .await
    {
        Ok(_) => {
            let h = ctx.systems.get_or_spawn(sys);
            let ship = sess.ship_id.unwrap();
            for _ in 0..20 {
                let _ = h.step_manual(60.0).await;
                if let Ok(Some((_, b))) = h.get_ball_snapshot(ship).await {
                    if !matches!(b.mode, MovementMode::Warp { .. }) {
                        break;
                    }
                }
            }
            if let Ok(Some((_, tb))) = h.get_ball_snapshot(target).await {
                if dist <= 1.0 {
                    let _ = h.set_movement_mode(ship, MovementMode::Stop).await;
                    let _ = h.set_ball_position(ship, tb.position).await;
                }
            }
        }
        Err(e) if e.code == "WarpDistanceTooShort" => {}
        Err(e) => panic!("warp_to_fast {target}: {e:?}"),
    }
}

pub(crate) async fn travel_fast(ctx: &ServerContext, sess: &mut SessionState, dest_sys: u32) {
    if sess.structure_id.is_some() {
        rpc(ctx, sess, "structureDocking", "Undock", &[]).await.expect("structure Undock");
    } else if sess.station_id.is_some() {
        rpc(ctx, sess, "stationSvc", "Undock", &[]).await.expect("station Undock");
    }
    ctx.advance_secs(15);
    let ship_id = sess.ship_id.unwrap();
    while sess.solar_system_id != Some(dest_sys) {
        let cur = sess.solar_system_id.unwrap();
        let mut prev: HashMap<u32, (u32, i64)> = HashMap::new();
        let mut q = VecDeque::from([cur]);
        let mut seen = HashSet::from([cur]);
        while let Some(s) = q.pop_front() {
            if s == dest_sys {
                break;
            }
            let mut gates: Vec<(&i64, &eve_sde::StargateJump)> =
                ctx.sde.jumps.iter().filter(|(_, j)| j.from_solar_system_id == s).collect();
            gates.sort_by_key(|g| *g.0);
            for (&gid, j) in gates {
                if seen.insert(j.to_solar_system_id) {
                    prev.insert(j.to_solar_system_id, (s, gid));
                    q.push_back(j.to_solar_system_id);
                }
            }
        }
        let mut next_sys = dest_sys;
        while prev[&next_sys].0 != cur {
            next_sys = prev[&next_sys].0;
        }
        let gate = prev[&next_sys].1;
        let h = ctx.systems.get_or_spawn(cur);
        let _ = h.set_movement_mode(ship_id, MovementMode::Stop).await;
        let _ = h.set_ball_position(ship_id, ctx.sde.stargates()[&gate].position).await;
        let beyonce = ctx.bind_object(sess, "beyonce", cur as i64).ok();
        ctx.dispatch_rpc(sess, "beyonce", beyonce, "CmdStargateJump", &[PyRep::Integer(gate)], &HashMap::new(), None)
            .await
            .unwrap_or_else(|e| panic!("jump {gate}: {e:?}"));
        ctx.advance_secs(15);
    }
}

pub(crate) async fn dock_station_fast(ctx: &ServerContext, sess: &mut SessionState, station_id: i64) {
    let sys = ctx.sde.stations[&station_id].solar_system_id;
    travel_fast(ctx, sess, sys).await;
    let ship_id = sess.ship_id.unwrap();
    let h = ctx.systems.get_or_spawn(sys);
    let _ = h.set_movement_mode(ship_id, MovementMode::Stop).await;
    let _ = h.set_ball_position(ship_id, ctx.sde.stations[&station_id].position).await;
    rpc(ctx, sess, "stationSvc", "Dock", &[PyRep::Integer(station_id)])
        .await
        .unwrap_or_else(|e| panic!("Dock {station_id}: {e:?}"));
    ctx.advance_secs(15);
}

pub(crate) async fn dock_structure_fast(ctx: &ServerContext, sess: &mut SessionState, structure_id: i64) {
    let (sys, st_pos) = ctx.db.with_snapshot(|s| {
        let st = &s.structures[&structure_id];
        (st.solar_system_id, DVec3::from_array(st.position))
    });
    if sess.solar_system_id != Some(sys) || sess.station_id.is_some() || sess.structure_id.is_some() {
        travel_fast(ctx, sess, sys).await;
    }
    let ship_id = sess.ship_id.unwrap();
    let h = ctx.systems.get_or_spawn(sys);
    let _ = h.set_movement_mode(ship_id, MovementMode::Stop).await;
    let _ = h.set_ball_position(ship_id, st_pos).await;
    ctx.advance_secs(15);
    rpc(ctx, sess, "structureDocking", "Dock", &[PyRep::Integer(structure_id)])
        .await
        .unwrap_or_else(|e| panic!("Dock {structure_id}: {e:?}"));
    ctx.advance_secs(15);
}

pub(crate) async fn deploy_online_upwell_fast(
    ctx: &ServerContext,
    sess: &mut SessionState,
    sys_id: u32,
    hull_tid: u32,
    services: &[u32],
    fuel_qty: i64,
) -> (i64, Vec<i64>) {
    let owner = sess.character_id.unwrap();
    travel_fast(ctx, sess, sys_id).await;
    let sun = ctx.sde.celestials_in_system(sys_id).into_iter().find(|c| c.group_id == 6).expect("sun").celestial_id;
    warp_to_fast(ctx, sess, sun, 100_000.0).await;
    let anc = rpc(
        ctx,
        sess,
        "structureDeployment",
        "Anchor",
        &[PyRep::Integer(i64::from(hull_tid)), PyRep::String("Live Industry Complex".into())],
    )
    .await
    .expect("Anchor Upwell structure");
    let st_id = int(&anc, "structureID");
    ctx.advance_virtual_clock(life::ANCHOR_VULNERABLE_SECS + life::ANCHORING_SECS + 10);
    let _ = rpc(ctx, sess, "structureGuests", "GetGuests", &[PyRep::Integer(st_id)]).await;
    dock_structure_fast(ctx, sess, st_id).await;
    let ship = sess.ship_id.unwrap();
    let core = ctx.sde.types[&hull_tid].default_attributes[&life::ATTR_REQUIRES_DEED_TYPE] as u32;
    let core_item = items_at(ctx, owner, ship, core)[0].0;
    rpc(ctx, sess, "invbroker", "Add", &[PyRep::Integer(core_item), PyRep::Integer(st_id), PyRep::Integer(172)])
        .await
        .expect("Insert Quantum Core");
    let mut fuel_left = fuel_qty;
    for tid in [4_051_u32, 4_246, 4_247, 4_312] {
        for (item, qty) in items_at(ctx, owner, ship, tid) {
            let take = qty.min(fuel_left);
            if take > 0 {
                rpc(
                    ctx,
                    sess,
                    "invbroker",
                    "Add",
                    &[PyRep::Integer(item), PyRep::Integer(st_id), PyRep::Integer(133), PyRep::Integer(take)],
                )
                .await
                .expect("Load Fuel Bay");
                fuel_left -= take;
            }
        }
    }
    assert_eq!(fuel_left, 0, "freighter must carry {fuel_qty} fuel blocks");
    rpc(ctx, sess, "structureDocking", "Undock", &[]).await.expect("Undock for onlining");
    ctx.advance_virtual_clock(life::ONLINING_SECS + 15);
    dock_structure_fast(ctx, sess, st_id).await;
    let mut fitted = Vec::new();
    for (idx, &stid) in services.iter().enumerate() {
        let item = items_at(ctx, owner, ship, stid)[0].0;
        let fit = rpc(
            ctx,
            sess,
            "invbroker",
            "Add",
            &[PyRep::Integer(item), PyRep::Integer(st_id), PyRep::Integer(164 + idx as i64)],
        )
        .await
        .expect("Fit & online service module");
        assert_eq!(fit.dict_get("online").and_then(|v| v.as_bool()), Some(true));
        fitted.push(int(&fit, "itemID"));
    }
    (st_id, fitted)
}

const RIFTER_BP: u32 = 691;
const RAITARU: u32 = 35_825;
const SOTIYO: u32 = 35_827;
const MFG_PLANT: u32 = 35_878;
const RESEARCH_LAB: u32 = 35_891;
const SUPERCAP_SHIPYARD: u32 = 35_877;
const SOV_HUB_TYPE: u32 = 32_458;
const SUPERCAP_UPGRADE_TYPE: u32 = 81_623;

async fn complete_job(ctx: &ServerContext, sess: &mut SessionState, job: &PyRep) -> PyRep {
    let dur = int(job, "durationSeconds") as u64;
    ctx.advance_virtual_clock(dur + 2);
    rpc(ctx, sess, "industryManager", "CompleteJob", &[PyRep::Integer(int(job, "jobID"))])
        .await
        .expect("CompleteJob")
}

#[tokio::test]
async fn test_shard1_live_supercapital_and_me_research_scaling() {
    // parity: feat:industry.capital-construction-restrictions, feat:industry.blueprint-me-research
    eve_testkit::attest(&[
        "feat:industry.capital-construction-restrictions",
        "feat:industry.blueprint-me-research",
    ]);

    let ctx = fast_full_market_ctx();
    common::pin_clock(&ctx);
    fund_pilots(&ctx, &[(PILOT_1, 250_000_000_000.0), (PILOT_2, 5_000_000_000.0)]).await;
    let mut s1 = login(&ctx, 9401, "demo", PILOT_1).await;
    let mut s2 = login(&ctx, 9402, "other", PILOT_2).await;

    let titan_bp = *ctx
        .sde
        .blueprints
        .iter()
        .filter(|(tid, b)| {
            ctx.sde.types.get(tid).is_some_and(|t| t.published && t.market_group_id.is_some())
                && ctx.sde.types.get(&b.product_type_id).is_some_and(|t| t.group_id == 30)
        })
        .map(|(tid, _)| tid)
        .min()
        .expect("published Titan blueprint");

    train_skills(
        &ctx,
        &mut s1,
        &[3318, 3363, 3380, 3387, 3388, 3394, 3403, 3406, 3409, 3449, 3453, 3456, 11_574, 20_185, 1319, SOTIYO, SUPERCAP_SHIPYARD, RAITARU, RESEARCH_LAB, MFG_PLANT, titan_bp],
        true,
    )
    .await;

    // Part A: Blueprint ME research facility bonuses (Raitaru + Standup Research Lab) & System Cost Index scaling
    let bpo_station = buy(&ctx, &mut s1, RIFTER_BP, 1).await[0];
    let r1 = install(&ctx, &mut s1, bpo_station, JITA_STATION, 4, 1, &[]).await.expect("ME 0->1 at Jita");
    let cost_jita = real(&r1, "totalJobCostISK");
    let dur_jita = int(&r1, "durationSeconds");
    assert_eq!(dur_jita, 1_339);
    complete_job(&ctx, &mut s1, &r1).await;

    // Second ME research job in Jita raises Research SCI -> higher installation cost for higher ME tier
    let r2 = install(&ctx, &mut s1, bpo_station, JITA_STATION, 4, 1, &[]).await.expect("ME 1->2 at Jita");
    let sci_jita_1 = real(&r1, "systemCostIndex");
    let sci_jita_2 = real(&r2, "systemCostIndex");
    assert!(sci_jita_2 >= sci_jita_1 && real(&r2, "totalJobCostISK") > cost_jita);
    complete_job(&ctx, &mut s1, &r2).await;

    // Deploy Upwell Raitaru with Standup Research Lab I in Perimeter -> compares fresh BPO ME 0->1 against Jita baseline
    found_corp_with_member(&ctx, &mut s1, &mut s2, "SCAP").await;
    ctx.advance_virtual_clock(8 * 86_400);
    let corp_id_sc = s1.corporation_id.unwrap();
    rpc(
        &ctx,
        &mut s1,
        "account",
        "GiveCash",
        &[PyRep::Integer(corp_id_sc), PyRep::Real(5_000_000_000.0), PyRep::String("ally fee".into())],
    )
    .await
    .expect("fund corp for alliance");
    rpc(&ctx, &mut s1, "corpRegistry", "CreateAlliance", &[PyRep::String("Supercap Vanguard".into()), PyRep::String("SCVG".into())])
        .await
        .expect("CreateAlliance");

    let mut raitaru_manifest = upwell_cargo(&ctx, RAITARU, &[RESEARCH_LAB]);
    raitaru_manifest.extend([(RIFTER_BP, 2), (4_246, 800)]);
    let fr1 = board_loaded_freighter(&ctx, &mut s1, &raitaru_manifest).await;
    let bpos_perim = items_at(&ctx, PILOT_1, fr1, RIFTER_BP);
    let bpo_upwell = bpos_perim[0].0;
    let bpo_perim_npc = bpos_perim[1].0;
    let (raitaru_id, _) = deploy_online_upwell_fast(&ctx, &mut s1, PERIMETER_SYS, RAITARU, &[RESEARCH_LAB], 800).await;
    rpc(
        &ctx,
        &mut s1,
        "facilityManager",
        "SetFacilityTaxes",
        &[PyRep::Integer(raitaru_id), PyRep::Real(0.0)],
    )
    .await
    .expect("0% tax on Raitaru");
    move_item(&ctx, &mut s1, bpo_upwell, raitaru_id, 4).await;

    let r_upwell = install(&ctx, &mut s1, bpo_upwell, raitaru_id, 4, 1, &[])
        .await
        .expect("ME 0->1 at Raitaru + Research Lab");
    assert!(
        int(&r_upwell, "durationSeconds") < dur_jita,
        "Raitaru + Standup Research Lab must reduce ME research duration ({} < {})",
        int(&r_upwell, "durationSeconds"),
        dur_jita
    );
    complete_job(&ctx, &mut s1, &r_upwell).await;

    let perim_station = ctx
        .sde
        .stations
        .values()
        .find(|st| st.solar_system_id == PERIMETER_SYS)
        .expect("Perimeter NPC station")
        .station_id;
    dock_station_fast(&ctx, &mut s1, perim_station).await;
    move_item(&ctx, &mut s1, bpo_perim_npc, perim_station, 4).await;
    let r_perim_npc = install(&ctx, &mut s1, bpo_perim_npc, perim_station, 4, 1, &[])
        .await
        .expect("ME 0->1 at Perimeter NPC station");
    assert!(
        real(&r_upwell, "totalJobCostISK") < real(&r_perim_npc, "totalJobCostISK"),
        "0%-tax Raitaru + Research Lab must cost less than NPC station in same system ({} < {})",
        real(&r_upwell, "totalJobCostISK"),
        real(&r_perim_npc, "totalJobCostISK")
    );
    complete_job(&ctx, &mut s1, &r_perim_npc).await;

    // Part B: Sovereign Null-Sec Supercapital (Titan) construction at Sotiyo + Standup Supercapital Shipyard I + Sov Upgrade 81623
    dock_station_fast(&ctx, &mut s1, JITA_STATION).await;
    let null_sys = nearest_system(&ctx, PERIMETER_SYS, |sys| {
        eve_server::services::sov_gameplay::is_claimable_nullsec_system(&ctx.sde, sys)
    });

    let fr_sov = board_loaded_freighter(&ctx, &mut s1, &[(SOV_HUB_TYPE, 1)]).await;
    let hub_item = items_at(&ctx, PILOT_1, fr_sov, SOV_HUB_TYPE)[0].0;

    // 1. Fly to claimable null-sec system and anchor/online Sovereignty Hub -> claims alliance sov
    travel_fast(&ctx, &mut s1, null_sys).await;
    let hub_rep = rpc(
        &ctx,
        &mut s1,
        "structureDeployment",
        "Anchor",
        &[PyRep::Integer(hub_item), PyRep::String("Null Supercap Hub".into())],
    )
    .await
    .expect("Anchor Sovereignty Hub in null-sec");
    let hub_id = int(&hub_rep, "structureID");
    assert!(hub_id > 0);
    ctx.advance_secs(905);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;

    // 2. Fly back to Jita 4-4 for Sotiyo + Standup Supercapital Shipyard I load, then deploy in null-sec
    dock_station_fast(&ctx, &mut s1, JITA_STATION).await;
    let fr2 = buy(&ctx, &mut s1, 20_185, 1).await[0];
    rpc(&ctx, &mut s1, "ship", "AssembleShip", &[PyRep::Integer(fr2)]).await.unwrap();
    rpc(&ctx, &mut s1, "ship", "Board", &[PyRep::Integer(fr2)]).await.unwrap();
    for idx in 0..3_i64 {
        let low_mod = buy(&ctx, &mut s1, 1319, 1).await[0];
        move_item(&ctx, &mut s1, low_mod, fr2, 11 + idx).await;
    }
    let mut null_manifest = upwell_cargo(&ctx, SOTIYO, &[SUPERCAP_SHIPYARD, MFG_PLANT]);
    null_manifest.extend([
        (SUPERCAP_UPGRADE_TYPE, 1),
        (titan_bp, 1),
        (4_051, 1_000),
        (4_246, 200),
        (4_247, 1_000),
        (4_312, 1_000),
    ]);
    for &(tid, qty) in &null_manifest {
        for item in acquire(&ctx, &mut s1, tid, qty).await {
            move_item(&ctx, &mut s1, item, fr2, 5).await;
        }
    }
    let upg_item = items_at(&ctx, PILOT_1, fr2, SUPERCAP_UPGRADE_TYPE)[0].0;
    let null_titan_bpo = items_at(&ctx, PILOT_1, fr2, titan_bp)[0].0;
    let (sotiyo_id, _) = deploy_online_upwell_fast(&ctx, &mut s1, null_sys, SOTIYO, &[SUPERCAP_SHIPYARD, MFG_PLANT], 3_200).await;
    move_item(&ctx, &mut s1, null_titan_bpo, sotiyo_id, 4).await;

    // Before activating Supercapital Construction Facilities upgrade (81623) in system, Titan job fails closed!
    let err_no_upg = install(&ctx, &mut s1, null_titan_bpo, sotiyo_id, 1, 1, &[])
        .await
        .expect_err("Titan construction must require online Supercapital Construction Facilities upgrade");
    assert_eq!(err_no_upg.code, "SupercapitalRequiresSovereignNullSec");

    // Place Supercapital Construction Facilities upgrade (81623) into null-sec structure -> Titan construction gate now passes!
    move_item(&ctx, &mut s1, upg_item, sotiyo_id, 4).await;
    let titan_gate = install(&ctx, &mut s1, null_titan_bpo, sotiyo_id, 1, 1, &[])
        .await
        .expect_err("Supercapital Construction Facilities active: gate passes to material check");
    assert_eq!(titan_gate.code, "InsufficientManufacturingMaterials");
}

#[tokio::test]
async fn test_shard1_live_industry_costs_corp_debit_pause_and_esi_jobs() {
    // parity: esi:GET /characters/{character_id}/industry/jobs, esi:GET /corporations/{corporation_id}/industry/jobs
    // parity: feat:esi.industry.char-jobs, feat:esi.industry.corp-jobs
    // parity: feat:industry.eiv-adjusted-prices, feat:industry.facility-tax, feat:industry.job-cost
    // parity: feat:lifecycle.industry-pause-and-many
    eve_testkit::attest(&[
        "esi:GET /characters/{character_id}/industry/jobs",
        "esi:GET /corporations/{corporation_id}/industry/jobs",
        "feat:esi.industry.char-jobs",
        "feat:esi.industry.corp-jobs",
        "feat:industry.eiv-adjusted-prices",
        "feat:industry.facility-tax",
        "feat:industry.job-cost",
        "feat:lifecycle.industry-pause-and-many",
    ]);

    let db_path = temp_db_path("industry_jobs");
    let corp_id;
    let raitaru_id;
    let mfg_plant_item;
    let corp_job_id;
    let char_job_id;

    {
        let (sde, snap) = shared_market_base();
        let repo = reload_repo(&db_path);
        repo.with_mut_snapshot(|s| *s = snap.clone());
        let ctx = Arc::new(ServerContext::with_sde_and_repository(Arc::clone(sde), repo));
        common::pin_clock(&ctx);
        fund_pilots(&ctx, &[(PILOT_1, 80_000_000_000.0), (PILOT_2, 10_000_000_000.0)]).await;

        let mut s1 = login(&ctx, 9411, "demo", PILOT_1).await;
        let mut s2 = login(&ctx, 9412, "other", PILOT_2).await;
        let bp_tid = small_mineral_bp(&ctx);
        train_skills(&ctx, &mut s1, &[3363, 3380, 3387, 3388, 3403, 3406, 20_185, RAITARU, MFG_PLANT, bp_tid], true).await;

        corp_id = found_corp_with_member(&ctx, &mut s1, &mut s2, "IND1").await;
        ctx.advance_virtual_clock(8 * 86_400);
        rpc(&ctx, &mut s1, "account", "GiveCash", &[PyRep::Integer(corp_id), PyRep::Real(5_000_000_000.0), PyRep::String("corp ops".into())])
            .await
            .expect("fund corp wallet");

        let mats = bom(&ctx, bp_tid, 12);
        let mut cargo = upwell_cargo(&ctx, RAITARU, &[MFG_PLANT]);
        cargo.push((bp_tid, 1));
        for &(m_tid, m_qty) in &mats {
            cargo.push((m_tid, m_qty));
        }
        cargo.extend([(4_246, 800)]);
        let fr = board_loaded_freighter(&ctx, &mut s1, &cargo).await;
        let bp_item = items_at(&ctx, PILOT_1, fr, bp_tid)[0].0;
        let (rid, fitted) = deploy_online_upwell_fast(&ctx, &mut s1, PERIMETER_SYS, RAITARU, &[MFG_PLANT], 800).await;
        raitaru_id = rid;
        mfg_plant_item = fitted[0];

        rpc(
            &ctx,
            &mut s1,
            "facilityManager",
            "SetFacilityTaxes",
            &[PyRep::Integer(raitaru_id), PyRep::Dict(vec![(PyRep::Integer(1), PyRep::Real(0.03))])],
        )
        .await
        .expect("SetFacilityTaxes 3%");

        // Move blueprint & minerals into CorpSAG1 (flag 4) and install a CORP manufacturing job (useCorp = true)
        move_item(&ctx, &mut s1, bp_item, raitaru_id, 4).await;
        for &(m_tid, _) in &mats {
            for (item, _) in items_at(&ctx, PILOT_1, fr, m_tid) {
                move_item(&ctx, &mut s1, item, raitaru_id, 4).await;
            }
        }

        let corp_bal_before = ctx.db.with_snapshot(|s| s.corporations[&corp_id].master_wallet_balance().raw_cents());
        let corp_job = install(&ctx, &mut s1, bp_item, raitaru_id, 1, 5, &[("useCorp", PyRep::Bool(true))])
            .await
            .expect("Install corp manufacturing job");
        corp_job_id = int(&corp_job, "jobID");
        let total_cost_isk = real(&corp_job, "totalJobCostISK");
        let eiv_total = real(&corp_job, "estimatedItemValueISK") * 5.0;
        let scc_isk = real(&corp_job, "sccSurchargeISK");
        assert!((scc_isk - ((eiv_total * 0.04 * 100.0).round() / 100.0)).abs() < 0.05);

        let corp_bal_after = ctx.db.with_snapshot(|s| s.corporations[&corp_id].master_wallet_balance().raw_cents());
        let expected_debit_cents = (total_cost_isk * 100.0).round() as i64;
        assert_eq!(
            corp_bal_before - corp_bal_after,
            expected_debit_cents,
            "Corp job installation must debit totalJobCostISK ONCE (no double debit)"
        );

        // Complete corp job so completed_date, completed_character_id, and successful_runs populate
        complete_job(&ctx, &mut s1, &corp_job).await;

        // Now move blueprint & remaining minerals to personal hangar at Raitaru and start a personal job
        let corp_bp_in_hangar = ctx.db.with_snapshot(|s| {
            s.items.values().find(|i| i.location_id == raitaru_id && i.type_id == bp_tid).unwrap().item_id
        });
        let char_job = install(&ctx, &mut s1, corp_bp_in_hangar, raitaru_id, 1, 2, &[])
            .await
            .expect("Install active personal job at Raitaru");
        char_job_id = int(&char_job, "jobID");

        // Offline Standup Manufacturing Plant I -> freezes active job as paused (pause_date set)
        rpc(&ctx, &mut s1, "dogmaIM", "TakeModuleOffline", &[PyRep::Integer(raitaru_id), PyRep::Integer(mfg_plant_item)])
            .await
            .expect("Offline Manufacturing Plant");

        let router = EsiRouter::new(Arc::clone(&ctx));
        let spec = OpenApiSpec::pinned();
        let tok_member = issue_token(&ctx, PILOT_2, &["esi-industry.read_corporation_jobs.v1"]);
        esi_call(&router, &spec, "GET", &format!("/v1/corporations/{corp_id}/industry/jobs/"), Some(&tok_member), None, 403);

        // Grant Factory Manager to PILOT_2 and query corp jobs ESI
        rpc(
            &ctx,
            &mut s1,
            "corpRegistry",
            "UpdateMember",
            &[
                PyRep::Integer(PILOT_2),
                PyRep::Integer(corp_roles::ROLE_FACTORY_MANAGER as i64),
            ],
        )
        .await
        .expect("Grant Factory Manager");

        assert_etag_304(
            &router,
            &spec,
            &format!("/v1/corporations/{corp_id}/industry/jobs/?include_completed=true"),
            Some(&tok_member),
        );
        let (corp_jobs, _) = esi_call(
            &router,
            &spec,
            "GET",
            &format!("/v1/corporations/{corp_id}/industry/jobs/?include_completed=true"),
            Some(&tok_member),
            None,
            200,
        );
        let cj = corp_jobs
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["job_id"] == corp_job_id)
            .expect("completed corp job in ESI");
        assert_eq!(cj["status"], json!("delivered"));
        assert_eq!(cj["successful_runs"], json!(5));
        assert_eq!(cj["completed_character_id"], json!(PILOT_1));
        assert_eq!(cj["location_id"], json!(raitaru_id));
        assert!(cj["completed_date"].as_str().is_some());
        assert!(cj["cost"].as_f64().unwrap() > 0.0);
    }

    // Reopen SQLite repository from scratch and verify persisted jobs & pause state via ESI
    {
        let repo2 = reload_repo(&db_path);
        let ctx2 = Arc::new(ServerContext::with_sde_and_repository(Arc::clone(&shared_market_base().0), repo2));
        let router2 = EsiRouter::new(Arc::clone(&ctx2));
        let spec2 = OpenApiSpec::pinned();
        let tok1 = issue_token(&ctx2, PILOT_1, &["esi-industry.read_character_jobs.v1"]);
        let tok2 = issue_token(&ctx2, PILOT_2, &["esi-industry.read_corporation_jobs.v1"]);

        let (char_jobs, _) = esi_call(
            &router2,
            &spec2,
            "GET",
            &format!("/v1/characters/{PILOT_1}/industry/jobs/?include_completed=true"),
            Some(&tok1),
            None,
            200,
        );
        let pj = char_jobs
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["job_id"] == char_job_id)
            .expect("paused job persisted across SQLite reload");
        assert_eq!(pj["status"], json!("paused"));
        assert!(pj["pause_date"].as_str().is_some());

        let (corp_jobs, _) = esi_call(
            &router2,
            &spec2,
            "GET",
            &format!("/v1/corporations/{corp_id}/industry/jobs/?include_completed=true"),
            Some(&tok2),
            None,
            200,
        );
        assert!(corp_jobs.as_array().unwrap().iter().any(|r| r["job_id"] == corp_job_id && r["status"] == "delivered"));
    }

    cleanup_sqlite_files(&db_path);
}
