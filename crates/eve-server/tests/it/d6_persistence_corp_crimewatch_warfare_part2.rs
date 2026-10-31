//! SQLite restart durability and transactional rollback integration tests for `D-6` Batch 3 (Part 2):
//! Warfare / FW / Sov / Insurgency (`feat:lifecycle.warfare-durable`).

#![forbid(unsafe_code)]

use crate::d6_persistence_corp_crimewatch_warfare::{
    esi_get, login_and_select, make_structure, open_ctx, rpc, temp_sqlite_path, OTHER, PILOT,
};
use eve_db::{IskCents, TrainedSkillRecord};
use eve_proto::PyRep;
use eve_server::services::{fw_services, insurgency_services, sov_services};

#[tokio::test]
async fn test_d6_warfare_fw_sov_and_insurgency_persist_and_rollback() {
    // parity: feat:lifecycle.warfare-durable
    let db_path = temp_sqlite_path("warfare");
    let t0 = 1_775_200_000u64;
    let fw_sys = fw_services::HQ_SYSTEM_AMO;
    let sov_sys = 30_004_759u32;
    let ins_sys = 30_002_537u32;

    let (corp_a, corp_b, alliance_id, war_id, neg_id, hub_id, campaign_id) = {
        let (ctx, _clock) = open_ctx(&db_path, t0);
        ctx.db.with_mut_snapshot(|snap| {
            let sk = TrainedSkillRecord { type_id: 3363, level: 5, skill_points: 256_000.0 };
            snap.skills.entry(PILOT).or_default().insert(3363, sk.clone());
            snap.skills.entry(OTHER).or_default().insert(3363, sk);
            snap.characters.get_mut(&PILOT).unwrap().balance = IskCents::from_isk(5_000_000_000);
            snap.characters.get_mut(&OTHER).unwrap().balance = IskCents::from_isk(5_000_000_000);
            snap.fw_system_occupations.insert(
                fw_sys,
                eve_db::snapshot_ext::FwSystemOccupationRecord {
                    solar_system_id: fw_sys,
                    owner_faction_id: fw_services::FACTION_MINMATAR,
                    occupier_faction_id: fw_services::FACTION_MINMATAR,
                    victory_points: 0,
                    victory_points_threshold: 3000,
                },
            );
        });
        let mut sess_a = login_and_select(&ctx, "demo", "demo-secret", PILOT, 8201).await;
        let mut sess_b = login_and_select(&ctx, "other", "other-secret", OTHER, 8202).await;

        let ca = rpc(&ctx, &mut sess_a, "corpRegistry", "AddCorporation", &[PyRep::String("Aggressor Armada".into()), PyRep::String("AGGR".into()), PyRep::Real(0.05)]).await;
        let corp_a = ca.dict_get("corporationID").and_then(|v| v.as_i64()).or_else(|| ca.as_i64()).unwrap();

        let cb = rpc(&ctx, &mut sess_b, "corpRegistry", "AddCorporation", &[PyRep::String("Defender Bastion".into()), PyRep::String("DEFB".into()), PyRep::Real(0.05)]).await;
        let corp_b = cb.dict_get("corporationID").and_then(|v| v.as_i64()).or_else(|| cb.as_i64()).unwrap();

        let hq_id = 700_000_901i64;
        let def_struct_id = 700_000_902i64;
        let hub_id = 700_000_903i64;
        let skyhook_id = 700_000_999i64;
        ctx.db.with_mut_snapshot(|snap| {
            for cid in [corp_a, corp_b] {
                if let Some(c) = snap.corporations.get_mut(&cid) {
                    c.ensure_wallet_divisions();
                    c.wallet_divisions[0].balance = IskCents::from_isk(3_000_000_000);
                    c.master_wallet = c.wallet_divisions[0].balance;
                }
            }
            snap.structures.insert(hq_id, make_structure(hq_id, corp_a, 30_000_142, 35832, "Aggressor Astrahus HQ"));
            snap.structures.insert(def_struct_id, make_structure(def_struct_id, corp_b, 30_000_142, 35832, "Defender Astrahus"));
            snap.structures.insert(hub_id, make_structure(hub_id, corp_a, sov_sys, sov_services::TYPE_SOVEREIGNTY_HUB, "Sov Hub Prime"));
            snap.structures.insert(skyhook_id, make_structure(skyhook_id, corp_a, sov_sys, sov_services::TYPE_ORBITAL_SKYHOOK, "Skyhook Prime"));
            snap.sov_prospecting_levels.insert(sov_sys, 4);
        });

        let al_res = rpc(&ctx, &mut sess_a, "corpRegistry", "CreateAlliance", &[PyRep::String("D6 Sovereign Pact".into()), PyRep::String("D6SP".into()), PyRep::String("Durable Alliance".into()), PyRep::String("https://d6.example".into())]).await;
        let alliance_id = al_res.dict_get("allianceID").and_then(|v| v.as_i64()).or_else(|| al_res.as_i64()).unwrap();

        rpc(&ctx, &mut sess_b, "corpRegistry", "ApplyToJoinAlliance", &[PyRep::Integer(alliance_id), PyRep::String("Joining D6SP".into())]).await;
        rpc(&ctx, &mut sess_a, "mutualWarInviteMgr", "SendInviteByPlayer", &[PyRep::Integer(corp_b)]).await;

        let war_res = rpc(&ctx, &mut sess_a, "corpRegistry", "DeclareWarAgainst", &[PyRep::Integer(corp_b), PyRep::Integer(hq_id)]).await;
        let war_id = war_res.dict_get("warID").and_then(|v| v.as_i64()).or_else(|| war_res.as_i64()).unwrap();

        rpc(&ctx, &mut sess_b, "warsInfoMgr", "SetOpenForAllies", &[PyRep::Integer(war_id), PyRep::Bool(true)]).await;
        let neg = rpc(&ctx, &mut sess_b, "warsInfoMgr", "CreateSurrenderNegotiation", &[PyRep::Integer(war_id), PyRep::Real(25_000_000.0), PyRep::String("Peace terms".into())]).await;
        let neg_id = neg.dict_get("negotiationID").and_then(|v| v.as_i64()).unwrap_or(1);
        rpc(&ctx, &mut sess_a, "warsInfoMgr", "AcceptSurrender", &[PyRep::Integer(neg_id)]).await;

        rpc(&ctx, &mut sess_a, "fwCharacterEnlistmentMgr", "CreateMyDirectEnlistment", &[PyRep::Integer(fw_services::FACTION_MINMATAR)]).await;
        fw_services::add_victory_points(&ctx, fw_sys, 1_480);
        fw_services::update_system_advantage(&ctx, fw_sys, 0.25, 0.10);
        fw_services::award_fw_kill_lp(&ctx, PILOT, fw_sys, 100_000_000.0);
        fw_services::spawn_battlefield_site(&ctx, fw_sys, fw_services::FACTION_MINMATAR, "Amo Siege");

        rpc(&ctx, &mut sess_a, "sovMgr", "SetSovHubFuelAccessGroup", &[PyRep::Integer(hub_id), PyRep::Integer(42)]).await;
        sov_services::set_skyhook_hackable(&ctx, sov_sys, skyhook_id, true);
        sov_services::record_hub_hack(&ctx, sov_sys, hub_id, 3_600);

        let campaign_id = 9001i64;
        insurgency_services::init_insurgency_campaign(&ctx, campaign_id, insurgency_services::FACTION_GURISTAS, fw_services::FACTION_CALDARI, ins_sys, vec![ins_sys]);
        insurgency_services::add_system_corruption(&ctx, campaign_id, ins_sys, 450);
        insurgency_services::add_system_suppression(&ctx, campaign_id, ins_sys, 220);
        insurgency_services::spawn_ice_heist(&ctx, ins_sys);

        (corp_a, corp_b, alliance_id, war_id, neg_id, hub_id, campaign_id)
    };

    {
        let (ctx2, _clock2) = open_ctx(&db_path, t0 + 120);
        let mut sess_a2 = login_and_select(&ctx2, "demo", "demo-secret", PILOT, 8203).await;
        assert_eq!(sess_a2.corporation_id, Some(corp_a));
        assert_eq!(sess_a2.alliance_id, Some(alliance_id));

        let wars_rep = rpc(&ctx2, &mut sess_a2, "warsInfoMgr", "GetWarsByOwnerID", &[PyRep::Integer(corp_a)]).await;
        assert!(format!("{wars_rep:?}").contains(&war_id.to_string()) && format!("{wars_rep:?}").contains(&corp_b.to_string()));
        let apps_rep = rpc(&ctx2, &mut sess_a2, "corpRegistry", "GetAllianceApplications", &[]).await;
        assert!(format!("{apps_rep:?}").contains(&corp_b.to_string()), "alliance applications must persist: {apps_rep:?}");

        let invites = rpc(&ctx2, &mut sess_a2, "mutualWarInviteMgr", "GetPendingInvitesForSession", &[]).await;
        assert!(format!("{invites:?}").contains(&corp_b.to_string()), "mutual war invites must persist: {invites:?}");

        let surr = rpc(&ctx2, &mut sess_a2, "warsInfoMgr", "GetSurrenderNegotiations", &[PyRep::Integer(war_id)]).await;
        assert!(format!("{surr:?}").contains(&neg_id.to_string()) && format!("{surr:?}").contains("Accepted") && format!("{surr:?}").contains("Peace terms"));

        let treaties = rpc(&ctx2, &mut sess_a2, "peaceTreatyMgr", "GetPeaceTreatiesForSession", &[]).await;
        assert!(format!("{treaties:?}").contains(&war_id.to_string()), "peace treaties must persist: {treaties:?}");

        let vp_states = rpc(&ctx2, &mut sess_a2, "fwVictoryPointMgr", "GetAllVictoryPointStates", &[]).await;
        assert!(format!("{vp_states:?}").contains("1500"), "FW victory points must persist via RPC: {vp_states:?}");
        assert_eq!(ctx2.db.with_snapshot(|s| s.fw_runtime.system_advantage.get(&fw_sys).copied()), Some((0.25, 0.10)));

        let bf = rpc(&ctx2, &mut sess_a2, "dungeonInstanceCacheMgr", "GetFactionWarfareBattlefieldInstances", &[PyRep::Integer(fw_sys as i64)]).await;
        assert!(format!("{bf:?}").contains("Amo Siege"));

        let fuel_grp = rpc(&ctx2, &mut sess_a2, "sovMgr", "GetSovHubFuelAccessGroup", &[PyRep::Integer(hub_id)]).await;
        assert_eq!(fuel_grp, PyRep::Integer(42));

        let skyhooks = rpc(&ctx2, &mut sess_a2, "colonyResourcesMgr", "GetMyCorporationSkyhooks", &[]).await;
        assert!(format!("{skyhooks:?}").contains("500") && format!("{skyhooks:?}").contains("700000999"));

        let ihub = rpc(&ctx2, &mut sess_a2, "sovMgr", "GetInfrastructureHubInfo", &[PyRep::Integer(sov_sys as i64)]).await;
        assert_eq!(ihub.dict_get("prospectingLevel"), Some(&PyRep::Integer(4)));

        let vis_camps = rpc(&ctx2, &mut sess_a2, "insurgencySolarsystem", "GetAllVisibleCampaigns", &[]).await;
        assert!(format!("{vis_camps:?}").contains(&campaign_id.to_string()));
        ctx2.db.set_character_solar_system(PILOT, ins_sys, &ctx2.sde).unwrap();
        let PyRep::Tuple(ins_loc) = rpc(&ctx2, &mut sess_a2, "insurgencySolarsystem", "GetLocalCampaignClientSnapshot", &[]).await else { panic!("expected tuple") };
        assert_eq!(ins_loc[1].dict_get("phase"), Some(&PyRep::String("Forecasting".into())));
        assert_eq!(ins_loc[1].dict_get("corruptionPoints"), Some(&PyRep::Integer(450)));
        assert_eq!(ins_loc[1].dict_get("suppressionPoints"), Some(&PyRep::Integer(220)));
        assert_eq!(ins_loc[1].dict_get("corruptionStage"), Some(&PyRep::Integer(3)));
        assert_eq!(ins_loc[1].dict_get("suppressionStage"), Some(&PyRep::Integer(2)));
        let heist = rpc(&ctx2, &mut sess_a2, "dungeonInstanceCacheMgr", "GetPirateInsurgencyIceHeistInstances", &[PyRep::Integer(ins_sys as i64)]).await;
        assert!(format!("{heist:?}").contains("800001"));

        // Verify ESI endpoints for Alliance, War, FW, and Sovereignty after restart
        let esi_al = esi_get(&ctx2, "/alliances/{alliance_id}", &[("alliance_id", alliance_id)]);
        assert_eq!(esi_al["creator_id"], PILOT);
        assert_eq!(esi_al["creator_corporation_id"], corp_a);
        let esi_war = esi_get(&ctx2, "/wars/{war_id}", &[("war_id", war_id)]);
        assert_eq!(esi_war["open_for_allies"], true);
        assert!(esi_war.get("finished").is_some(), "surrender finished timestamp must appear on ESI /wars/{{id}}: {esi_war}");
        let esi_fw_sys = esi_get(&ctx2, "/fw/systems", &[]);
        assert!(esi_fw_sys.to_string().contains(&fw_sys.to_string()) && esi_fw_sys.to_string().contains("1500"));
        let esi_fw_char = esi_get(&ctx2, "/characters/{character_id}/fw/stats", &[("character_id", PILOT)]);
        assert_eq!(esi_fw_char["kills"]["total"], 1);
        assert_eq!(esi_fw_char["victory_points"]["total"], 20);
        let esi_sov_camps = esi_get(&ctx2, "/sovereignty/campaigns", &[]);
        assert!(esi_sov_camps.to_string().contains(&hub_id.to_string()) && esi_sov_camps.to_string().contains("0.6"));

        let abort_res: Result<(), String> = ctx2.db.try_with_mut_snapshot(|snap| {
            snap.war_state.wars.clear();
            snap.alliance_war_state.alliances.clear();
            snap.fw_runtime.system_victory_points.clear();
            snap.sov_runtime.hacked_hubs.clear();
            snap.insurgency_runtime.campaigns.clear();
            Err("simulated warfare abort".to_string())
        });
        assert!(abort_res.is_err());
        ctx2.db.with_snapshot(|snap| {
            assert!(snap.war_state.wars.contains_key(&war_id));
            assert!(snap.alliance_war_state.alliances.contains_key(&alliance_id));
            assert_eq!(snap.fw_runtime.system_victory_points.get(&fw_sys).copied(), Some(1_500));
            assert!(snap.sov_runtime.hacked_hubs.contains_key(&hub_id));
            assert!(snap.insurgency_runtime.campaigns.contains_key(&campaign_id));
        });
    }

    let _ = std::fs::remove_file(&db_path);
}
