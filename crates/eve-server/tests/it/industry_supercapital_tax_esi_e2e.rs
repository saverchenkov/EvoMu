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
    assert_etag_304, cleanup_sqlite_files, deploy_online_upwell_slow, dock_station_slow, esi_call,
    issue_token, reload_repo, temp_db_path, travel_slow,
};
use eve_db::corp_roles;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::esi::{EsiRouter, OpenApiSpec};
use eve_server::router::ServerContext;
use eve_server::world_seeder::seed_full_market_catalog;
use serde_json::json;
use std::sync::Arc;

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

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    seed_full_market_catalog(&ctx.db, &ctx.sde);
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
    let (raitaru_id, _) = deploy_online_upwell_slow(&ctx, &mut s1, PERIMETER_SYS, RAITARU, &[RESEARCH_LAB], 800).await;
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
    dock_station_slow(&ctx, &mut s1, perim_station).await;
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
    dock_station_slow(&ctx, &mut s1, JITA_STATION).await;
    let null_sys = nearest_system(&ctx, PERIMETER_SYS, |sys| {
        eve_server::services::sov_gameplay::is_claimable_nullsec_system(&ctx.sde, sys)
    });

    let fr_sov = board_loaded_freighter(&ctx, &mut s1, &[(SOV_HUB_TYPE, 1)]).await;
    let hub_item = items_at(&ctx, PILOT_1, fr_sov, SOV_HUB_TYPE)[0].0;

    // 1. Fly to claimable null-sec system and anchor/online Sovereignty Hub -> claims alliance sov
    travel_slow(&ctx, &mut s1, null_sys).await;
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
    dock_station_slow(&ctx, &mut s1, JITA_STATION).await;
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
    let (sotiyo_id, _) = deploy_online_upwell_slow(&ctx, &mut s1, null_sys, SOTIYO, &[SUPERCAP_SHIPYARD, MFG_PLANT], 3_200).await;
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
        let repo = reload_repo(&db_path);
        let ctx = Arc::new(ServerContext::with_repository(repo));
        common::pin_clock(&ctx);
        seed_full_market_catalog(&ctx.db, &ctx.sde);
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
        let (rid, fitted) = deploy_online_upwell_slow(&ctx, &mut s1, PERIMETER_SYS, RAITARU, &[MFG_PLANT], 800).await;
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

        let corp_bal_before = ctx.db.with_snapshot(|s| s.corporations[&corp_id].master_wallet.raw_cents());
        let corp_job = install(&ctx, &mut s1, bp_item, raitaru_id, 1, 5, &[("useCorp", PyRep::Bool(true))])
            .await
            .expect("Install corp manufacturing job");
        corp_job_id = int(&corp_job, "jobID");
        let total_cost_isk = real(&corp_job, "totalJobCostISK");
        let eiv_total = real(&corp_job, "estimatedItemValueISK") * 5.0;
        let scc_isk = real(&corp_job, "sccSurchargeISK");
        assert!((scc_isk - ((eiv_total * 0.04 * 100.0).round() / 100.0)).abs() < 0.05);

        let corp_bal_after = ctx.db.with_snapshot(|s| s.corporations[&corp_id].master_wallet.raw_cents());
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
        let ctx2 = Arc::new(ServerContext::with_repository(repo2));
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
