//! Live end-to-end integration tests for Industry EIV/SCI job costs, NPC vs Upwell facility taxes,
//! wallet journal accounting, service-offline pause/resume, batch completion, and ex-corp guards:
//! - `feat:lifecycle.industry-pause-and-many`
//! - `plan:E-4`
//!
//! Industrialists are regular player sessions: blueprints and materials come from the Jita market,
//! cargo is hauled by freighter through stargates, and the Raitaru is anchored/fueled/fitted live.

#![forbid(unsafe_code)]

use crate::common;
use crate::industry_live_support::{
    acquire, board_loaded_freighter, bom, buy, deploy_online_upwell, dock_station, dock_structure, fuel_cargo, fund_pilots,
    install, int, items_at, login, move_item, real, rpc, small_mineral_bp, split_off, train_skills, upwell_cargo,
    JITA_STATION, PERIMETER_SYS, PILOT_1, PILOT_2,
};
use eve_db::corp_roles;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::world_seeder::seed_full_market_catalog;

const RIFTER_BP: u32 = 691;
const RAITARU: u32 = 35_825;
const MFG_PLANT: u32 = 35_878;
const RESEARCH_LAB: u32 = 35_891;

fn perimeter_station(ctx: &ServerContext) -> i64 {
    ctx.sde.stations.values().filter(|s| s.solar_system_id == PERIMETER_SYS).map(|s| s.station_id).min().expect("Perimeter NPC station")
}

/// Unloads everything of `type_id` from the active ship's hold into the docked hangar.
async fn unload(ctx: &ServerContext, sess: &mut eve_machonet::SessionState, type_id: u32, hangar: i64) {
    let (owner, ship) = (sess.character_id.unwrap(), sess.ship_id.unwrap());
    for (item, _) in items_at(ctx, owner, ship, type_id) {
        move_item(ctx, sess, item, hangar, 4).await;
    }
}

#[tokio::test]
async fn test_live_job_cost_sci_progression_facility_taxes_and_wallet_journal() {
    // parity: plan:E-4
    eve_testkit::attest(&["plan:E-4"]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    seed_full_market_catalog(&ctx.db, &ctx.sde);
    fund_pilots(&ctx, &[(PILOT_1, 80_000_000_000.0), (PILOT_2, 2_000_000_000.0)]).await;
    let mut s1 = login(&ctx, 8803, "demo", PILOT_1).await;
    let mut s2 = login(&ctx, 8804, "other", PILOT_2).await;
    train_skills(&ctx, &mut s1, &[3363, 3380, 3387, 26252, 20_185, RAITARU, MFG_PLANT], true).await;

    // A live Jita job adds universe-wide manufacturing work for the SCI ratio.
    let jita_bpo = acquire(&ctx, &mut s1, RIFTER_BP, 1).await[0];
    for (tid, qty) in bom(&ctx, RIFTER_BP, 3) {
        acquire(&ctx, &mut s1, tid, qty).await;
    }
    install(&ctx, &mut s1, jita_bpo, JITA_STATION, 1, 3, &[]).await.expect("Jita InstallJob");

    // Haul two BPOs, 4 runs of minerals and the Raitaru kit to Perimeter.
    buy(&ctx, &mut s1, RIFTER_BP, 2).await;
    let mut cargo = upwell_cargo(&ctx, RAITARU, &[MFG_PLANT]);
    cargo.extend(fuel_cargo());
    cargo.push((RIFTER_BP, 2));
    cargo.extend(bom(&ctx, RIFTER_BP, 4));
    let freighter = board_loaded_freighter(&ctx, &mut s1, &cargo).await;
    let npc_station = perimeter_station(&ctx);
    dock_station(&ctx, &mut s1, npc_station).await;
    let bp_stack = items_at(&ctx, PILOT_1, freighter, RIFTER_BP)[0].0;
    let npc_bpo1 = split_off(&ctx, &mut s1, bp_stack, npc_station, 4, 1).await;
    let npc_bpo2 = split_off(&ctx, &mut s1, items_at(&ctx, PILOT_1, freighter, RIFTER_BP)[0].0, npc_station, 4, 1).await;
    for (tid, _) in bom(&ctx, RIFTER_BP, 1) {
        unload(&ctx, &mut s1, tid, npc_station).await;
    }
    let bal_before = ctx.db.with_snapshot(|s| s.characters[&PILOT_1].balance.to_wire_f64());

    let nj1 = install(&ctx, &mut s1, npc_bpo1, npc_station, 1, 2, &[]).await.expect("NPC InstallJob 1");
    let eiv1 = real(&nj1, "estimatedItemValue") * 2.0;
    let fac_tax1 = real(&nj1, "facilityTaxISK");
    let sci1 = real(&nj1, "systemCostIndex");
    assert!((fac_tax1 - (eiv1 * 0.0025 * 100.0).round() / 100.0).abs() < 0.02, "NPC facility tax must be 0.25% of EIV");
    let bal_after = ctx.db.with_snapshot(|s| s.characters[&PILOT_1].balance.to_wire_f64());
    assert!(((bal_before - bal_after) - real(&nj1, "totalJobCost")).abs() < 0.02, "wallet debit must equal totalJobCost");

    let days = (ctx.now_unix() / 86_400) as i64 + 719_468;
    let era = days.div_euclid(146_097);
    let doe = days - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    let journal = rpc(&ctx, &mut s1, "account", "GetTransactions", &[PyRep::Integer(1000), PyRep::Integer(y), PyRep::Integer(m), PyRep::Bool(false)])
        .await
        .expect("account.GetTransactions");
    let j_dbg = format!("{journal:?}");
    assert!(
        j_dbg.contains("Base job cost for industry job")
            && j_dbg.contains("Facility tax for industry job")
            && j_dbg.contains("SCC surcharge for industry job"),
        "Wallet journal must record industry fee entries: {j_dbg}"
    );
    let nj2 = install(&ctx, &mut s1, npc_bpo2, npc_station, 1, 2, &[]).await.expect("NPC InstallJob 2");
    let sci2 = real(&nj2, "systemCostIndex");
    assert!(sci2 > sci1, "28d sqrt(sys_work/univ_work) SCI must increase after live job ({sci2} > {sci1})");

    rpc(&ctx, &mut s1, "corpRegistry", "AddCorporation", &[PyRep::String("Tax Owner Corp".into()), PyRep::String("TXOW".into())])
        .await
        .expect("AddCorporation");
    let corp_id = s1.corporation_id.unwrap();
    ctx.advance_virtual_clock(8 * 86_400);
    let (raitaru_id, _) = deploy_online_upwell(&ctx, &mut s1, PERIMETER_SYS, RAITARU, &[MFG_PLANT], 1_500).await;

    let tax = |rate: f64| [PyRep::Integer(raitaru_id), PyRep::Dict(vec![(PyRep::Integer(1), PyRep::Real(rate))])];
    let err_cap = rpc(&ctx, &mut s1, "facilityManager", "SetFacilityTaxes", &tax(0.15)).await.expect_err(">10% Upwell tax must be rejected");
    assert_eq!(err_cap.code, "InvalidFacilityTaxRate");
    rpc(&ctx, &mut s1, "facilityManager", "SetFacilityTaxes", &tax(0.065)).await.expect("SetFacilityTaxes 6.5%");
    let pub_prof = rpc(&ctx, &mut s1, "structureProfiles", "CreateProfile", &[PyRep::String("Public Mfg".into()), PyRep::String("".into())])
        .await
        .unwrap()
        .as_i64()
        .unwrap();
    rpc(&ctx, &mut s1, "structureProfiles", "ChangeProfiles", &[PyRep::List(vec![PyRep::Integer(raitaru_id)]), PyRep::Integer(pub_prof)])
        .await
        .expect("ChangeProfiles Public Mfg");
    ctx.advance_virtual_clock(900);

    // PILOT_2 (an Alpha clone) buys a small module blueprint + minerals, flies them in its frigate hold.
    let small_bp = small_mineral_bp(&ctx);
    let ship2 = s2.ship_id.unwrap();
    let alpha_bpo = buy(&ctx, &mut s2, small_bp, 1).await[0];
    move_item(&ctx, &mut s2, alpha_bpo, ship2, 5).await;
    for (tid, qty) in bom(&ctx, small_bp, 2) {
        for item in acquire(&ctx, &mut s2, tid, qty).await {
            move_item(&ctx, &mut s2, item, ship2, 5).await;
        }
    }
    dock_structure(&ctx, &mut s2, raitaru_id).await;
    move_item(&ctx, &mut s2, alpha_bpo, raitaru_id, 4).await;
    for (tid, _) in bom(&ctx, small_bp, 1) {
        unload(&ctx, &mut s2, tid, raitaru_id).await;
    }
    let corp_bal_0 = ctx.db.with_snapshot(|s| s.corporations[&corp_id].master_wallet.to_wire_f64());

    let aj = install(&ctx, &mut s2, alpha_bpo, raitaru_id, 1, 2, &[]).await.expect("Alpha InstallJob at Upwell Raitaru");
    let tot_eiv = real(&aj, "estimatedItemValue") * 2.0;
    let (aj_base, aj_ftax, aj_scc) = (real(&aj, "baseJobCost"), real(&aj, "facilityTaxISK"), real(&aj, "sccSurchargeISK"));
    let aj_alpha = real(&aj, "alphaCloneTaxISK");
    assert!(aj_base < tot_eiv * real(&aj, "systemCostIndex"), "Raitaru hull cost bonus must discount baseJobCost");
    assert!((aj_ftax - (tot_eiv * 0.065 * 100.0).round() / 100.0).abs() < 0.02);
    assert!((aj_scc - (tot_eiv * 0.04 * 100.0).round() / 100.0).abs() < 0.02);
    assert!((aj_alpha - (tot_eiv * 0.0025 * 100.0).round() / 100.0).abs() < 0.02);
    assert!((real(&aj, "totalJobCost") - (aj_base + aj_ftax + aj_scc + aj_alpha)).abs() < 0.02);
    let corp_bal_1 = ctx.db.with_snapshot(|s| s.corporations[&corp_id].master_wallet.to_wire_f64());
    assert!(((corp_bal_1 - corp_bal_0) - aj_ftax).abs() < 0.02, "structure owner corp receives the facility tax");
}

#[tokio::test]
async fn test_live_industry_service_offline_pause_resume_batch_complete_and_ex_corp_guard() {
    // parity: feat:lifecycle.industry-pause-and-many, plan:E-4
    eve_testkit::attest(&["feat:lifecycle.industry-pause-and-many", "plan:E-4"]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    seed_full_market_catalog(&ctx.db, &ctx.sde);
    fund_pilots(&ctx, &[(PILOT_1, 80_000_000_000.0), (PILOT_2, 2_000_000_000.0)]).await;
    let mut s1 = login(&ctx, 8805, "demo", PILOT_1).await;
    let mut s2 = login(&ctx, 8806, "other", PILOT_2).await;
    train_skills(&ctx, &mut s1, &[3363, 3380, 3387, 3402, 3406, 20_185, RAITARU, MFG_PLANT, RESEARCH_LAB], true).await;
    train_skills(&ctx, &mut s2, &[3380, 3403, 3409], false).await;

    rpc(&ctx, &mut s1, "corpRegistry", "AddCorporation", &[PyRep::String("Lifecycle Ind Corp".into()), PyRep::String("LIND".into())])
        .await
        .expect("AddCorporation");
    let corp_id = s1.corporation_id.unwrap();
    rpc(&ctx, &mut s1, "account", "GiveCash", &[PyRep::Integer(corp_id), PyRep::Real(500_000_000.0), PyRep::String("seed".into())])
        .await
        .expect("GiveCash corp");
    ctx.advance_virtual_clock(8 * 86_400);

    buy(&ctx, &mut s1, RIFTER_BP, 2).await;
    let mut cargo = upwell_cargo(&ctx, RAITARU, &[MFG_PLANT, RESEARCH_LAB]);
    cargo.extend(fuel_cargo());
    cargo.push((RIFTER_BP, 3));
    cargo.extend(bom(&ctx, RIFTER_BP, 7));
    let freighter = board_loaded_freighter(&ctx, &mut s1, &cargo).await;
    let (raitaru_id, fitted) = deploy_online_upwell(&ctx, &mut s1, PERIMETER_SYS, RAITARU, &[MFG_PLANT, RESEARCH_LAB], 2_500).await;
    let mfg_mod_id = fitted[0];
    let mut bpos = Vec::new();
    for _ in 0..3 {
        let stack = items_at(&ctx, PILOT_1, freighter, RIFTER_BP)[0].0;
        bpos.push(split_off(&ctx, &mut s1, stack, raitaru_id, 4, 1).await);
    }
    for (tid, _) in bom(&ctx, RIFTER_BP, 1) {
        unload(&ctx, &mut s1, tid, raitaru_id).await;
    }

    let ja = install(&ctx, &mut s1, bpos[0], raitaru_id, 1, 1, &[]).await.expect("job A");
    let jb = install(&ctx, &mut s1, bpos[1], raitaru_id, 1, 3, &[]).await.expect("job B");
    let jc = install(&ctx, &mut s1, bpos[2], raitaru_id, 1, 3, &[]).await.expect("job C");
    let (ja_id, jb_id, jc_id) = (int(&ja, "jobID"), int(&jb, "jobID"), int(&jc, "jobID"));
    let ja_dur = int(&ja, "durationSeconds") as u64;
    let jb_dur_0 = int(&jb, "durationSeconds") as u64;

    // Job A finishes before the Manufacturing Plant goes offline; B and C pause at the offline moment.
    ctx.advance_virtual_clock(ja_dur + 10);
    let t_offline = ctx.now_unix();
    rpc(&ctx, &mut s1, "dogmaIM", "TakeModuleOffline", &[PyRep::Integer(raitaru_id), PyRep::Integer(mfg_mod_id)])
        .await
        .expect("Offline 35878 at t_offline");
    ctx.advance_virtual_clock(1_800);
    let ja_info = rpc(&ctx, &mut s1, "industryManager", "GetJob", &[PyRep::Integer(ja_id)]).await.unwrap();
    let jb_info = rpc(&ctx, &mut s1, "industryManager", "GetJob", &[PyRep::Integer(jb_id)]).await.unwrap();
    assert_ne!(ja_info.dict_get("status").and_then(|v| v.as_str()), Some("Paused"));
    assert_eq!(jb_info.dict_get("status").and_then(|v| v.as_str()), Some("Paused"));
    assert_eq!(jb_info.dict_get("pauseUnix").and_then(|v| v.as_i64()), Some(t_offline as i64));

    rpc(&ctx, &mut s1, "dogmaIM", "SetModuleOnline", &[PyRep::Integer(raitaru_id), PyRep::Integer(mfg_mod_id)])
        .await
        .expect("Re-online 35878");
    let jb_resumed = rpc(&ctx, &mut s1, "industryManager", "GetJob", &[PyRep::Integer(jb_id)]).await.unwrap();
    assert_eq!(jb_resumed.dict_get("status").and_then(|v| v.as_str()), Some("Active"));
    assert_eq!(jb_resumed.dict_get("durationSeconds").and_then(|v| v.as_i64()), Some((jb_dur_0 + 1_800) as i64));

    let elapsed = ja_dur + 10 + 1_800;
    if jb_dur_0 + 500 > elapsed {
        ctx.advance_virtual_clock((jb_dur_0 + 500) - elapsed);
        let too_early = rpc(&ctx, &mut s1, "industryManager", "CompleteJob", &[PyRep::Integer(jb_id)])
            .await
            .expect_err("Paused job must not complete before extended end time");
        assert_eq!(too_early.code, "JobNotYetComplete");
    }
    ctx.advance_virtual_clock(2_000);
    let batch = rpc(&ctx, &mut s1, "industryManager", "CompleteManyJobs", &[PyRep::List(vec![PyRep::Integer(ja_id), PyRep::Integer(jb_id), PyRep::Integer(jc_id)])])
        .await
        .expect("CompleteManyJobs");
    match batch {
        PyRep::List(done) => assert_eq!(done.len(), 3),
        other => panic!("Expected List from CompleteManyJobs, got {other:?}"),
    }

    // Corporate jobs at a Jita office installed by a Factory Manager who is later expelled.
    dock_station(&ctx, &mut s1, JITA_STATION).await;
    let rent = rpc(&ctx, &mut s1, "officeManager", "RentOffice", &[PyRep::Integer(JITA_STATION)]).await.expect("RentOffice");
    let office_id = rent.dict_get("officeFolderID").or_else(|| rent.dict_get("officeID")).and_then(|v| v.as_i64()).expect("officeID");
    let app = rpc(&ctx, &mut s2, "corpRegistry", "InsertApplication", &[PyRep::Integer(corp_id), PyRep::String("ind".into())]).await.unwrap();
    ctx.advance_secs(12);
    rpc(&ctx, &mut s1, "corpRegistry", "UpdateApplicationOffer", &[PyRep::Integer(int(&app, "applicationID")), PyRep::String("Accepted".into())])
        .await
        .expect("Accept application");
    let mgr_roles = corp_roles::ROLE_FACTORY_MANAGER | corp_roles::ROLE_HANGAR_CAN_TAKE[0];
    rpc(&ctx, &mut s1, "corpRegistry", "UpdateMember", &[PyRep::Integer(PILOT_2), PyRep::Integer(mgr_roles as i64)])
        .await
        .expect("Grant Factory Manager");
    s2.corporation_id = Some(corp_id);
    s2.corp_role = mgr_roles;
    // Hangar-division Take roles are granted with the 24h corporation role delay.
    ctx.advance_virtual_clock(86_400 + 60);

    let small_bp = small_mineral_bp(&ctx);
    let mfg_bp = buy(&ctx, &mut s1, small_bp, 1).await[0];
    let corp_mfg_bp = split_off(&ctx, &mut s1, mfg_bp, office_id, 116, 1).await;
    let res_bp = buy(&ctx, &mut s1, RIFTER_BP, 1).await[0];
    let corp_res_bp = split_off(&ctx, &mut s1, res_bp, office_id, 116, 1).await;
    for (tid, qty) in bom(&ctx, small_bp, 1) {
        for item in acquire(&ctx, &mut s1, tid, qty).await {
            move_item(&ctx, &mut s1, item, office_id, 116).await;
        }
    }
    let corp_flag = [("isCorpJob", PyRep::Bool(true))];
    let cj_done = install(&ctx, &mut s2, corp_mfg_bp, JITA_STATION, 1, 1, &corp_flag).await.expect("corp manufacturing job");
    let cj_active = install(&ctx, &mut s2, corp_res_bp, JITA_STATION, 4, 10, &corp_flag).await.expect("corp ME 0->10 research job");
    assert!(int(&cj_active, "durationSeconds") > 25 * 3_600, "ME 0->10 research outlives the 24h role-drop cooldown");

    rpc(&ctx, &mut s1, "corpRegistry", "UpdateMember", &[PyRep::Integer(PILOT_2), PyRep::Integer(0)]).await.expect("Revoke roles");
    s2.corp_role = 0;
    ctx.advance_virtual_clock(86_400 + 60);
    rpc(&ctx, &mut s1, "corpRegistry", "KickOutMember", &[PyRep::Integer(PILOT_2)]).await.expect("KickOutMember");
    s2.corporation_id = ctx.db.with_snapshot(|s| Some(s.characters[&PILOT_2].corporation_id));
    assert_ne!(s2.corporation_id, Some(corp_id));

    let cancel_err = rpc(&ctx, &mut s2, "industryManager", "CancelJob", &[PyRep::Integer(int(&cj_active, "jobID"))])
        .await
        .expect_err("Ex-corp member must not CancelJob");
    assert_eq!(cancel_err.code, "JobOwnershipViolation");
    let comp_err = rpc(&ctx, &mut s2, "industryManager", "CompleteJob", &[PyRep::Integer(int(&cj_done, "jobID"))])
        .await
        .expect_err("Ex-corp member must not CompleteJob");
    assert_eq!(comp_err.code, "JobOwnershipViolation");
}
