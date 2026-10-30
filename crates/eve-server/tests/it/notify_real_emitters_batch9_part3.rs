//! Real server-side emission tests for Notify Batch 9 (Part 3: Diplomacy, Fittings, Offers, NES, Agents & Datacores).

use crate::notify_real_emitters_batch7::{
    drain_all, find_notif, new_ctx, rpc, JITA_STATION, PILOT_A, PILOT_B,
};
use crate::notify_real_emitters_batch9::login_docked;
use eve_proto::PyRep;

// parity: notify:OnAllowWarUpdated, notify:OnCommunityFittingsUpdated, notify:OnContextualOfferPurchased, notify:OnNesIntroStateChanged
#[tokio::test]
async fn notify_batch9_diplomacy_fittings_offers_and_nes() {
    eve_testkit::attest(&[
        "notify:OnAllowWarUpdated",
        "notify:OnCommunityFittingsUpdated",
        "notify:OnContextualOfferPurchased",
        "notify:OnNesIntroStateChanged",
    ]);

    let ctx = new_ctx();
    let mut sess_a = login_docked(&ctx, 9_301, "demo", "demo-secret", PILOT_A, JITA_STATION).await;
    let mut sess_b = login_docked(&ctx, 9_302, "other", "other-secret", PILOT_B, JITA_STATION).await;
    let corp_id = 98_900_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.insert(
            corp_id,
            eve_db::CorporationRecord {
                corporation_id: corp_id,
                name: "Test Diplo Corp".into(),
                ticker: "TDC".into(),
                ceo_id: PILOT_A,
                ..Default::default()
            },
        );
        if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
            ch.corporation_id = corp_id;
        }
        if let Some(ch) = snap.characters.get_mut(&PILOT_B) {
            ch.corporation_id = corp_id;
        }
    });
    sess_a.corporation_id = Some(corp_id);
    sess_b.corporation_id = Some(corp_id);
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
    let _ = drain_all(&ctx, &mut sess_b, PILOT_B);

    // 1. OnAllowWarUpdated
    // Negative 1a: Non-CEO/non-Director calling UpdateCorporation fails with CorpRoleDenied
    assert!(rpc(
        &ctx,
        &mut sess_b,
        "corpRegistry",
        "UpdateCorporation",
        &[
            PyRep::String("Desc".into()),
            PyRep::String("https://example.com".into()),
            PyRep::Real(0.05),
            PyRep::Bool(true),
        ],
    )
    .await
    .is_err());
    assert!(!drain_all(&ctx, &mut sess_a, PILOT_A)
        .iter()
        .any(|(e, _)| e == "OnAllowWarUpdated"));

    // Negative 1b: Character with no corporation fails with NoCorporation
    let mut no_corp_sess = sess_a.clone();
    no_corp_sess.corporation_id = None;
    assert!(rpc(
        &ctx,
        &mut no_corp_sess,
        "corpRegistry",
        "UpdateCorporation",
        &[
            PyRep::String("Desc".into()),
            PyRep::String("https://example.com".into()),
            PyRep::Real(0.05),
            PyRep::Bool(true),
        ],
    )
    .await
    .is_err());

    // Positive: CEO calling UpdateCorporation with allowWar=true emits OnAllowWarUpdated([corp_id, true]) to corp
    let _ = rpc(
        &ctx,
        &mut sess_a,
        "corpRegistry",
        "UpdateCorporation",
        &[
            PyRep::String("Desc".into()),
            PyRep::String("https://example.com".into()),
            PyRep::Real(0.05),
            PyRep::Bool(true),
        ],
    )
    .await
    .expect("UpdateCorporation");
    let war_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert_eq!(
        find_notif(&war_notifs, "OnAllowWarUpdated"),
        &[PyRep::Integer(corp_id), PyRep::Bool(true)]
    );

    // 2. OnCommunityFittingsUpdated
    let fit_id = 777_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.saved_fittings.insert(
            fit_id,
            eve_db::SavedFittingRecord {
                fitting_id: fit_id,
                owner_id: corp_id,
                name: "Community Rifter".into(),
                description: "Corp doctrine".into(),
                ship_type_id: 587,
                items: vec![],
            },
        );
    });
    // Negative 2a: Non-director member (PILOT_B) calling DeleteFitting for corp fails
    assert!(rpc(
        &ctx,
        &mut sess_b,
        "corpFittingMgr",
        "DeleteFitting",
        &[PyRep::Integer(corp_id), PyRep::Integer(fit_id)],
    )
    .await
    .is_err());
    assert!(!drain_all(&ctx, &mut sess_a, PILOT_A)
        .iter()
        .any(|(e, _)| e == "OnCommunityFittingsUpdated"));

    // Negative 2b: Deleting non-existent corp fitting returns false and does not emit OnCommunityFittingsUpdated
    let _ = rpc(
        &ctx,
        &mut sess_a,
        "corpFittingMgr",
        "DeleteFitting",
        &[PyRep::Integer(corp_id), PyRep::Integer(999_999)],
    )
    .await
    .expect("DeleteFitting missing");
    assert!(!drain_all(&ctx, &mut sess_a, PILOT_A)
        .iter()
        .any(|(e, _)| e == "OnCommunityFittingsUpdated"));

    // Positive: CEO deleting corp fitting emits OnCommunityFittingsUpdated
    let _ = rpc(
        &ctx,
        &mut sess_a,
        "corpFittingMgr",
        "DeleteFitting",
        &[PyRep::Integer(corp_id), PyRep::Integer(fit_id)],
    )
    .await
    .expect("DeleteFitting corp");
    let fit_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert!(fit_notifs.iter().any(|(e, _)| e == "OnCommunityFittingsUpdated"));

    // 3. OnContextualOfferPurchased
    // Negative 3a: TrackOfferPurchased with invalid offerID (<= 0) fails with InvalidOfferID
    assert!(rpc(&ctx, &mut sess_a, "contextualOfferMgr", "TrackOfferPurchased", &[PyRep::Integer(0)])
        .await
        .is_err());
    assert!(!drain_all(&ctx, &mut sess_a, PILOT_A)
        .iter()
        .any(|(e, _)| e == "OnContextualOfferPurchased"));

    // Negative 3b: TrackOfferSeen does NOT emit OnContextualOfferPurchased
    let _ = rpc(&ctx, &mut sess_a, "contextualOfferMgr", "TrackOfferSeen", &[PyRep::Integer(42)])
        .await
        .expect("TrackOfferSeen");
    assert!(!drain_all(&ctx, &mut sess_a, PILOT_A)
        .iter()
        .any(|(e, _)| e == "OnContextualOfferPurchased"));

    // Positive: TrackOfferPurchased(42) emits OnContextualOfferPurchased([42, Dict{"offerID": 42}])
    let _ = rpc(&ctx, &mut sess_a, "contextualOfferMgr", "TrackOfferPurchased", &[PyRep::Integer(42)])
        .await
        .expect("TrackOfferPurchased");
    let offer_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    let offer_args = find_notif(&offer_notifs, "OnContextualOfferPurchased");
    assert_eq!(offer_args.len(), 2);
    assert_eq!(offer_args[0], PyRep::Integer(42));
    assert!(matches!(&offer_args[1], PyRep::Dict(entries) if entries.iter().any(|(k, v)| k.as_str() == Some("offerID") && *v == PyRep::Integer(42))));

    // 4. OnNesIntroStateChanged
    // Negative 4a: Unauthenticated skip_nes_intro fails
    let mut unauth = eve_machonet::SessionState::new(996);
    assert!(rpc(&ctx, &mut unauth, "nes_intro", "skip_nes_intro", &[]).await.is_err());

    // Negative 4b: get_nes_intro_state is read-only and does NOT emit OnNesIntroStateChanged
    let _ = rpc(&ctx, &mut sess_a, "nes_intro", "get_nes_intro_state", &[])
        .await
        .expect("get_nes_intro_state");
    assert!(!drain_all(&ctx, &mut sess_a, PILOT_A)
        .iter()
        .any(|(e, _)| e == "OnNesIntroStateChanged"));

    // Positive: skip_nes_intro emits OnNesIntroStateChanged([1])
    let _ = rpc(&ctx, &mut sess_a, "nes_intro", "skip_nes_intro", &[])
        .await
        .expect("skip_nes_intro");
    let nes_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert_eq!(
        find_notif(&nes_notifs, "OnNesIntroStateChanged"),
        &[PyRep::Integer(PILOT_A), PyRep::Integer(1)]
    );
}

// parity: notify:OnOfferReceived, notify:OnIncomingAgentMessage, notify:OnIncomingTransmission, notify:OnEpicJournalChange, notify:OnDatacoreBought
#[tokio::test]
async fn notify_batch9_agents_epic_journal_and_datacores() {
    eve_testkit::attest(&[
        "notify:OnOfferReceived",
        "notify:OnIncomingAgentMessage",
        "notify:OnIncomingTransmission",
        "notify:OnEpicJournalChange",
        "notify:OnDatacoreBought",
    ]);

    let ctx = new_ctx();
    let mut sess_a = login_docked(&ctx, 9_311, "demo", "demo-secret", PILOT_A, JITA_STATION).await;
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);

    // Find an Epic Arc agent (agent_type_id == 10, level 1) or fallback to any level 1 agent
    let (epic_agent_id, epic_sys_id, epic_station_id) = ctx
        .sde
        .agents
        .values()
        .find(|a| a.agent_type_id == 10 && a.level == 1 && a.station_id.is_some())
        .or_else(|| ctx.sde.agents.values().find(|a| a.level == 1 && a.station_id.is_some()))
        .map(|a| (a.agent_id, a.solar_system_id.unwrap_or(30_000_142), a.station_id.unwrap_or(JITA_STATION)))
        .expect("SDE must have a level 1 agent");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
            ch.solar_system_id = epic_sys_id;
            ch.station_id = Some(epic_station_id);
        }
    });
    sess_a.solar_system_id = Some(epic_sys_id);
    sess_a.station_id = Some(epic_station_id);

    // 5-8. OnOfferReceived, OnIncomingTransmission, OnIncomingAgentMessage, OnEpicJournalChange
    // Negative 5a: Action 3 (Accept) before Offer fails with MissionNotOffered and emits nothing
    assert!(rpc(
        &ctx,
        &mut sess_a,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(3), PyRep::Integer(epic_agent_id)],
    )
    .await
    .is_err());
    assert!(!drain_all(&ctx, &mut sess_a, PILOT_A)
        .iter()
        .any(|(e, _)| e == "OnIncomingAgentMessage"));

    // Negative 5b: Unknown agent ID fails
    assert!(rpc(
        &ctx,
        &mut sess_a,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(2), PyRep::Integer(999_999_999)],
    )
    .await
    .is_err());

    // Positive Offer (action 2) -> emits OnIncomingTransmission
    let _ = rpc(
        &ctx,
        &mut sess_a,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(2), PyRep::Integer(epic_agent_id)],
    )
    .await
    .expect("Agent Offer");
    let offer_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert!(offer_notifs.iter().any(|(e, a)| e == "OnIncomingTransmission" && a.len() == 2));

    // Positive Accept (action 3) -> emits OnIncomingAgentMessage, OnIncomingTransmission, and (if epic arc) OnEpicJournalChange
    let _ = rpc(
        &ctx,
        &mut sess_a,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(3), PyRep::Integer(epic_agent_id)],
    )
    .await
    .expect("Agent Accept");
    let accept_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert!(accept_notifs.iter().any(|(e, a)| e == "OnIncomingAgentMessage" && a.len() == 2));
    assert!(accept_notifs.iter().any(|(e, _)| e == "OnEpicJournalChange"));

    // TrackOfferDelivered (contextualOfferMgr) -> emits OnOfferReceived([offerID, offerKeyVal])
    let _ = rpc(
        &ctx,
        &mut sess_a,
        "contextualOfferMgr",
        "TrackOfferDelivered",
        &[PyRep::Integer(77)],
    )
    .await
    .expect("TrackOfferDelivered");
    let recv_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    let recv_args = find_notif(&recv_notifs, "OnOfferReceived");
    assert_eq!(recv_args.len(), 2);
    assert_eq!(recv_args[0], PyRep::Integer(77));

    // Negative 6b: TrackOfferDelivered with offerID <= 0 fails
    assert!(rpc(
        &ctx,
        &mut sess_a,
        "contextualOfferMgr",
        "TrackOfferDelivered",
        &[PyRep::Integer(0)],
    )
    .await
    .is_err());

    // 9. OnDatacoreBought: R&D agent action 14 (Buy Datacores)
    let rnd_agent_id = ctx
        .sde
        .agents
        .values()
        .find(|a| a.agent_type_id == 4)
        .map(|a| a.agent_id)
        .unwrap_or(epic_agent_id);
    let datacore_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.group_id == 333)
        .map(|t| t.type_id)
        .unwrap_or(20410);

    // Negative 9a: Buying datacores without active research fails and does not emit OnDatacoreBought
    assert!(rpc(
        &ctx,
        &mut sess_a,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(rnd_agent_id), PyRep::Integer(14), PyRep::Integer(datacore_tid as i64), PyRep::Integer(1)],
    )
    .await
    .is_err());
    assert!(!drain_all(&ctx, &mut sess_a, PILOT_A)
        .iter()
        .any(|(e, _)| e == "OnDatacoreBought"));

    // Seed active research with RP for rnd_agent_id, then test Negative 9b (insufficient RP) and Positive
    ctx.db.with_mut_snapshot(|snap| {
        eve_server::services::agents_missions::save_rnd_research_entries(
            snap,
            PILOT_A,
            &[eve_server::services::agents_missions::RndResearchEntry {
                agent_id: rnd_agent_id,
                research_type_id: 11442,
                points_per_day: 100.0,
                initial_points: 500.0,
                started_unix: ctx.now_unix(),
            }],
        );
    });

    // Negative 9b: Requesting 100 datacores (10,000 RP) when only 500 RP available fails
    assert!(rpc(
        &ctx,
        &mut sess_a,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(rnd_agent_id), PyRep::Integer(14), PyRep::Integer(datacore_tid as i64), PyRep::Integer(100)],
    )
    .await
    .is_err());

    // Positive: Buying 2 datacores (200 RP) succeeds and emits OnDatacoreBought
    let (rd_sys, rd_sta) = ctx
        .sde
        .agents
        .get(&rnd_agent_id)
        .map(|a| (a.solar_system_id.unwrap_or(30_000_142), a.station_id.unwrap_or(JITA_STATION)))
        .unwrap_or((30_000_142, JITA_STATION));
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
            ch.solar_system_id = rd_sys;
            ch.station_id = Some(rd_sta);
        }
    });
    sess_a.solar_system_id = Some(rd_sys);
    sess_a.station_id = Some(rd_sta);
    let _ = rpc(
        &ctx,
        &mut sess_a,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(14), PyRep::Integer(rnd_agent_id), PyRep::Integer(2), PyRep::Integer(datacore_tid as i64)],
    )
    .await
    .expect("Buy Datacores");
    let dc_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert!(dc_notifs.iter().any(|(e, a)| e == "OnDatacoreBought" && a.len() == 3));
}
