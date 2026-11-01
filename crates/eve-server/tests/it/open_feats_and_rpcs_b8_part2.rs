//! Integration tests for open Zarzakh `feat:navigation.*` rows (batch 8, part 2):
//! - `feat:navigation.zarzakh-gate-toll`
//! - `feat:navigation.zarzakh-local-horizon-siege`

#![forbid(unsafe_code)]

use std::collections::HashMap;

use eve_db::{InventoryFlag, InventoryItemRecord, IskCents};
use eve_destiny::DVec3;
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, EvePublicRequest, GoogleAny,
    GoogleTimestamp, WireMessage, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_proto::PyRep;
use eve_server::quasar_gateway::STATUS_OK;
use eve_server::router::ServerContext;
use eve_server::services::cluster_macho_misc_services::{
    get_zarzakh_horizon_siege_state, is_zarzakh_horizon_siege_active, HorizonSiegePhase,
};
use eve_server::services::fw_gameplay::{
    apply_pirate_fob_structure_damage, ensure_campaign_pirate_fob, tick_insurgency_campaigns,
};
use eve_server::services::insurgency_services::{
    add_system_suppression, init_insurgency_campaign,
};
use eve_server::services::quasar_social_chat::on_character_local_chat_membership_refreshed;

use crate::open_feats_and_rpcs_b8::{
    gate_jump, rpc, setup_space_pilot, ALSAVOINON_GATE_ID, ALSAVOINON_SYS, ERR_ANCHORING_ZARZAKH,
    ERR_HORIZON_SIEGE_CASTER, FULCRUM_STATION_ID, TURNUR_GATE_ID, TURNUR_SYS,
    ZARZAKH_ALSAVOINON_GATE_ID, ZARZAKH_SYS, ZARZAKH_TURNUR_GATE_ID,
};
use crate::sp6_logoff_and_server_tick::persist_and_reopen_sqlite;

const ERR_HORIZON_SIEGE_GATE: &str = "HorizonSiegeGateAccessDenied";
const ERR_TOLL_PROMPT: &str = "AskConfirmZarzakhGateToll";
const ERR_TOLL_DECLINED: &str = "ZarzakhGateTollDeclined";
const ERR_TOLL_FUNDS: &str = "InsufficientFundsForZarzakhGateToll";
const ERR_CYNO_ZARZAKH: &str = "CynoDisallowedInZarzakh";
const ERR_DISALLOWED_HAZARD_MOD: &str = "ModuleDisallowedInHazardSystem";

fn make_public_req(type_name: &str, value: Vec<u8>, now_unix: u64) -> EvePublicRequest {
    EvePublicRequest {
        issued: Some(GoogleTimestamp {
            seconds: now_unix as i64,
            nanos: 0,
        }),
        correlation_uuid: b"corr-zarzakh-b8".to_vec(),
        payload: Some(GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"),
            value,
        }),
        external_origin: "eve-client".into(),
        application_instance_uuid: b"app-zarzakh-b8".to_vec(),
        authoritative_context: None,
        trace_context: None,
    }
}

#[tokio::test]
async fn test_zarzakh_gate_toll_mass_scaling_decline_and_pirates() {
    // parity: feat:navigation.zarzakh-gate-toll
    let ctx = ServerContext::new();
    let char_id = 90_000_001_i64;
    let ship_id = 100_000_001_i64;
    let turnur_gate_pos = ctx.sde.jumps[&TURNUR_GATE_ID].position;
    let alsa_gate_pos = ctx.sde.jumps[&ALSAVOINON_GATE_ID].position;

    // 1. Two-step confirmation challenge: first 3-arg call quotes toll (`AskConfirmZarzakhGateToll`),
    //    debits 0 ISK, and declining (`confirmToll: false`) aborts jump with `ZarzakhGateTollDeclined`
    let mut sess = setup_space_pilot(
        &ctx,
        884_401,
        char_id,
        ship_id,
        670, // Capsule (10,000 kg -> 10,000 ISK floor)
        TURNUR_SYS,
        turnur_gate_pos + DVec3::new(200.0, 0.0, 0.0),
        None,
    )
    .await;
    let bal_init = ctx.db.with_snapshot(|s| s.characters[&char_id].balance);
    let quote_err = gate_jump(&ctx, &mut sess, TURNUR_GATE_ID, ZARZAKH_TURNUR_GATE_ID, ship_id)
        .await
        .expect_err("Unconfirmed 3-arg Zarzakh gate jump must return AskConfirmZarzakhGateToll");
    assert_eq!(quote_err.code, ERR_TOLL_PROMPT);
    assert!(quote_err.message.contains("10000 ISK"));
    assert_eq!(ctx.db.with_snapshot(|s| s.characters[&char_id].balance), bal_init);
    assert_eq!(ctx.db.with_snapshot(|s| s.characters[&char_id].solar_system_id), TURNUR_SYS);

    let mut decline_kw = HashMap::new();
    decline_kw.insert("confirmToll".into(), PyRep::Bool(false));
    let dec_err = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            None,
            "CmdStargateJump",
            &[PyRep::Integer(TURNUR_GATE_ID), PyRep::Integer(ZARZAKH_TURNUR_GATE_ID), PyRep::Integer(ship_id)],
            &decline_kw,
            None,
        )
        .await
        .expect_err("Declining Zarzakh gate toll must abort jump");
    assert_eq!(dec_err.code, ERR_TOLL_DECLINED);
    assert_eq!(sess.solar_system_id, Some(TURNUR_SYS));
    assert_eq!(ctx.db.with_snapshot(|s| s.characters[&char_id].balance), bal_init);

    // 2. Insufficient funds after quote confirmation aborts jump BEFORE mutating character system state
    ctx.db.with_mut_snapshot(|s| s.characters.get_mut(&char_id).unwrap().balance = IskCents::from_isk(5_000));
    let _ = gate_jump(&ctx, &mut sess, TURNUR_GATE_ID, ZARZAKH_TURNUR_GATE_ID, ship_id).await;
    let broke_err = gate_jump(&ctx, &mut sess, TURNUR_GATE_ID, ZARZAKH_TURNUR_GATE_ID, ship_id)
        .await
        .expect_err("Insufficient ISK must abort confirmed Zarzakh gate jump");
    assert_eq!(broke_err.code, ERR_TOLL_FUNDS);
    assert_eq!(ctx.db.with_snapshot(|s| s.characters[&char_id].solar_system_id), TURNUR_SYS);

    // 3. Real SDE hull mass scaling (zero `ball.mass` overrides):
    //    Capsule (670, 10k kg) -> 10,000 ISK floor; Caracal (621, 11.91M kg) -> 7,940,000 ISK;
    //    Raven (638, 99.3M kg) -> 66,200,000 ISK; Orca (28606, 150M kg) -> 100,000,000 ISK cap
    for (idx, (type_id, gate_id, to_gate_id, sys_id, gate_pos, expected_toll_isk)) in [
        (670_u32, TURNUR_GATE_ID, ZARZAKH_TURNUR_GATE_ID, TURNUR_SYS, turnur_gate_pos, 10_000_i64),
        (621_u32, ALSAVOINON_GATE_ID, ZARZAKH_ALSAVOINON_GATE_ID, ALSAVOINON_SYS, alsa_gate_pos, 7_940_000_i64),
        (638_u32, TURNUR_GATE_ID, ZARZAKH_TURNUR_GATE_ID, TURNUR_SYS, turnur_gate_pos, 66_200_000_i64),
        (28_606_u32, ALSAVOINON_GATE_ID, ZARZAKH_ALSAVOINON_GATE_ID, ALSAVOINON_SYS, alsa_gate_pos, 100_000_000_i64),
    ]
    .into_iter()
    .enumerate()
    {
        let mut hull_sess = setup_space_pilot(
            &ctx,
            884_402 + idx as u64,
            char_id,
            ship_id + idx as i64,
            type_id,
            sys_id,
            gate_pos + DVec3::new(200.0, 0.0, 0.0),
            None,
        )
        .await;
        ctx.db.with_mut_snapshot(|s| s.characters.get_mut(&char_id).unwrap().balance = IskCents::from_isk(200_000_000));
        let _ = ctx.drain_notifications_for_char(char_id);
        let q_err = gate_jump(&ctx, &mut hull_sess, gate_id, to_gate_id, ship_id + idx as i64)
            .await
            .expect_err("Step 1 of 2-step Zarzakh toll must return quote");
        assert_eq!(q_err.code, ERR_TOLL_PROMPT);
        gate_jump(&ctx, &mut hull_sess, gate_id, to_gate_id, ship_id + idx as i64)
            .await
            .expect("Step 2 confirmed Zarzakh toll jump must succeed");
        assert_eq!(
            ctx.db.with_snapshot(|s| s.characters[&char_id].balance),
            IskCents::from_isk(200_000_000 - expected_toll_isk)
        );
        let notes = ctx.drain_notifications_for_char(char_id);
        assert!(
            notes.iter().any(|n| n.event == "OnAccountChange"),
            "Zarzakh gate toll debit must emit OnAccountChange"
        );
    }

    // 4. Both Angel Cartel (500_010) and Guristas (500_011) pirate militias pay 0 ISK without toll prompt
    for (idx, pirate_faction) in [500_010_i64, 500_011_i64].into_iter().enumerate() {
        let mut p_sess = setup_space_pilot(
            &ctx,
            884_410 + idx as u64,
            char_id,
            ship_id,
            621,
            TURNUR_SYS,
            turnur_gate_pos + DVec3::new(200.0, 0.0, 0.0),
            Some(pirate_faction),
        )
        .await;
        let p_before = ctx.db.with_snapshot(|s| s.characters[&char_id].balance);
        gate_jump(&ctx, &mut p_sess, TURNUR_GATE_ID, ZARZAKH_TURNUR_GATE_ID, ship_id)
            .await
            .expect("Pirate militia pilot must jump into Zarzakh free of toll");
        assert_eq!(ctx.db.with_snapshot(|s| s.characters[&char_id].balance), p_before);
    }

    // 5. SQLite reload preserves journaled `zarzakh_gate_toll` entries
    let reloaded = persist_and_reopen_sqlite(&ctx, "b8_gate_toll");
    let toll_entries = reloaded.db.with_snapshot(|s| {
        s.wallet_journal
            .iter()
            .filter(|j| j.character_id == char_id && j.entry_type == "zarzakh_gate_toll")
            .count()
    });
    assert_eq!(toll_entries, 4, "All 4 confirmed non-pirate SDE hull jumps must be journaled");
}

#[tokio::test]
async fn test_zarzakh_local_horizon_siege_chat_anchoring_and_cyno() {
    // parity: feat:navigation.zarzakh-local-horizon-siege
    let ctx = ServerContext::new();
    let char_a = 90_000_001_i64;
    let char_b = 90_000_502_i64;
    let ship_a = 100_000_501_i64;
    let ship_b = 100_000_502_i64;

    // 1. Delayed local chat in Zarzakh (Quasar `eve_public.chat.local.api.*` AND `LSC.GetMembers`)
    let mut sess_a = setup_space_pilot(&ctx, 884_501, char_a, ship_a, 11_957, ZARZAKH_SYS, DVec3::ZERO, None).await;
    let sess_b = setup_space_pilot(
        &ctx,
        884_502,
        char_b,
        ship_b,
        587,
        ZARZAKH_SYS,
        DVec3::new(1_000.0, 0.0, 0.0),
        None,
    )
    .await;
    let now = ctx.now_unix();
    ctx.quasar_gateway.subscribe_session(&sess_a, vec![], now);
    ctx.quasar_gateway.subscribe_session(&sess_b, vec![], now);
    let _ = ctx.quasar_gateway.poll_notices(sess_a.session_id);
    ctx.sync_character_local_chat(char_b, Some(TURNUR_SYS), ZARZAKH_SYS);

    let entry_notices = ctx.quasar_gateway.poll_notices(sess_a.session_id);
    assert!(
        !entry_notices
            .iter()
            .any(|n| n.payload.as_ref().is_some_and(|p| p.type_url.contains("JoinNotice"))),
        "Entering Zarzakh must not emit JoinNotice before speaking"
    );
    let mem_req = make_public_req("eve_public.chat.local.api.GetMembershipListRequest", Vec::new(), now);
    let mem_rsp = ctx.quasar_gateway.handle_public_request(&sess_a, &mem_req, None, false, now);
    assert_eq!(mem_rsp.status_code, STATUS_OK);
    let mem_wm = WireMessage::decode(&mem_rsp.payload.unwrap().value).unwrap();
    assert_eq!(
        mem_wm.fields.iter().filter(|f| f.field_number == 1).count(),
        0,
        "Silent Zarzakh pilots must stay hidden"
    );

    let z_chan = format!("solarsystemid2:{ZARZAKH_SYS}");
    let lsc_before = rpc(&ctx, &mut sess_a, "LSC", "GetMembers", &[PyRep::String(z_chan)])
        .await
        .expect("LSC.GetMembers in Zarzakh");
    if let PyRep::Dict(entries) = lsc_before {
        assert!(!entries.iter().any(|(k, _)| k.as_i64() == Some(char_b)));
    }

    // Silent pilot B must NOT leak presence via `on_character_local_chat_membership_refreshed` or `MuteRequest`
    let ref_delivered = on_character_local_chat_membership_refreshed(&ctx.quasar_gateway, &ctx.db, char_b as u32, now)
        .expect("on_character_local_chat_membership_refreshed for silent Zarzakh pilot");
    assert_eq!(ref_delivered, 0, "Silent Zarzakh pilot must not emit MembershipRefreshedNotice");
    let mut mute_payload = Vec::new();
    let mut char_id_msg = Vec::new();
    encode_varint_field(1, char_b as u64, &mut char_id_msg);
    encode_bytes_field(1, &char_id_msg, &mut mute_payload);
    let mute_req = make_public_req("eve_public.chat.local.api.admin.MuteRequest", mute_payload, now);
    let _ = ctx.quasar_gateway.handle_public_request(&sess_a, &mute_req, None, true, now);
    let mute_notices = ctx.quasar_gateway.poll_notices(sess_a.session_id);
    assert!(
        !mute_notices
            .iter()
            .any(|n| n.payload.as_ref().is_some_and(|p| p.type_url.contains("MembershipRefreshedNotice"))),
        "MuteRequest on silent Zarzakh pilot must not leak MembershipRefreshedNotice"
    );

    // Pilot B speaks -> JoinNotice + MessageBroadcastNotice emitted and Pilot B appears in Zarzakh local
    let mut chat_msg = Vec::new();
    encode_string_field(1, "o7 from Zarzakh", &mut chat_msg);
    let bcast_req = make_public_req("eve_public.chat.local.api.BroadcastMessageRequest", chat_msg, now + 400);
    let bcast_rsp = ctx.quasar_gateway.handle_public_request(&sess_b, &bcast_req, None, false, now + 400);
    assert_eq!(bcast_rsp.status_code, STATUS_OK);
    let mem_rsp_after = ctx.quasar_gateway.handle_public_request(&sess_a, &mem_req, None, false, now + 400);
    let mem_wm_after = WireMessage::decode(&mem_rsp_after.payload.unwrap().value).unwrap();
    assert_eq!(mem_wm_after.fields.iter().filter(|f| f.field_number == 1).count(), 1);

    // 2. Capsuleer Upwell structure + mobile deployable anchoring/launching prohibited in Zarzakh
    let (depot_id, cyno_mod_id, cov_cyno_mod_id, ind_cyno_mod_id, mjfg_mod_id) = (
        200_000_510_i64,
        200_000_511_i64,
        200_000_512_i64,
        200_000_513_i64,
        200_000_514_i64,
    );
    ctx.db.with_mut_snapshot(|s| {
        s.items.insert(
            depot_id,
            InventoryItemRecord {
                item_id: depot_id,
                type_id: 33_474,
                owner_id: char_a,
                location_id: ship_a,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                ..Default::default()
            },
        );
        for (mid, tid, flag) in [
            (cyno_mod_id, 21_096_u32, InventoryFlag::HiSlot0),     // Cynosural Field Generator I
            (cov_cyno_mod_id, 28_646_u32, InventoryFlag::HiSlot1), // Covert Cynosural Field Generator I
            (ind_cyno_mod_id, 52_694_u32, InventoryFlag::HiSlot2), // Industrial Cynosural Field Generator I
            (mjfg_mod_id, 4_248_u32, InventoryFlag::HiSlot3),      // Warp Disruption Field Generator II (attr 5561=1)
        ] {
            s.items.insert(
                mid,
                InventoryItemRecord {
                    item_id: mid,
                    type_id: tid,
                    owner_id: char_a,
                    location_id: ship_a,
                    flag,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
        }
    });
    ctx.invalidate_ship_fitting(ship_a);
    let upwell_err = rpc(&ctx, &mut sess_a, "structureDeployment", "Anchor", &[PyRep::Integer(35_832)])
        .await
        .expect_err("Upwell structure anchoring must be disallowed in Zarzakh");
    assert_eq!(upwell_err.code, ERR_ANCHORING_ZARZAKH);
    let depot_anchor_err = rpc(&ctx, &mut sess_a, "posMgr", "AnchorStructure", &[PyRep::Integer(depot_id)])
        .await
        .expect_err("Deployable anchoring must be disallowed in Zarzakh");
    assert_eq!(depot_anchor_err.code, ERR_ANCHORING_ZARZAKH);
    let depot_launch_err = rpc(
        &ctx,
        &mut sess_a,
        "ship",
        "Drop",
        &[PyRep::List(vec![PyRep::Integer(depot_id)])],
    )
    .await
    .expect_err("Launching deployables from ship bay must be disallowed in Zarzakh");
    assert_eq!(depot_launch_err.code, ERR_ANCHORING_ZARZAKH);

    // 3. Cynosural fields (standard 21096, covert 28646, industrial 52694) & hazard-banned modules (37480) prohibited
    for mid in [cyno_mod_id, cov_cyno_mod_id, ind_cyno_mod_id] {
        let cyno_err = rpc(&ctx, &mut sess_a, "dogmaIM", "Activate", &[PyRep::Integer(mid)])
            .await
            .expect_err("Cynosural field activation must be disallowed in Zarzakh");
        assert_eq!(cyno_err.code, ERR_CYNO_ZARZAKH);
    }
    let mjfg_err = rpc(&ctx, &mut sess_a, "dogmaIM", "Activate", &[PyRep::Integer(mjfg_mod_id)])
        .await
        .expect_err("Micro Jump Field Generator (attr 5561=1) must be disallowed in Zarzakh");
    assert_eq!(mjfg_err.code, ERR_DISALLOWED_HAZARD_MOD);

    // 4. Live Horizon Siege state machine: Insurgency FOB armor reinforcement -> Warning (300s) -> Active (3600s) -> Stabilizing -> Normal
    let camp_id = 777_001_i64;
    let camp_systems = vec![TURNUR_SYS, ALSAVOINON_SYS, 30_002_538, 30_045_338, 30_002_056];
    init_insurgency_campaign(&ctx, camp_id, 500_010, 500_002, TURNUR_SYS, camp_systems.clone());
    ctx.advance_secs(48 * 3_600 + 10);
    for &sys in &camp_systems {
        let _ = add_system_suppression(&ctx, camp_id, sys, 1_000);
    }
    let fob_id = ensure_campaign_pirate_fob(&ctx, camp_id).await.expect("spawn pirate FOB");
    apply_pirate_fob_structure_damage(&ctx, char_a, fob_id, 25_000_000.0)
        .await
        .expect("Reinforcing Pirate FOB armor must schedule Zarzakh Horizon Siege Warning");
    let warning_st = get_zarzakh_horizon_siege_state(&ctx);
    assert_eq!(warning_st.phase, HorizonSiegePhase::Warning);
    assert!(!is_zarzakh_horizon_siege_active(&ctx));

    ctx.advance_secs(305);
    tick_insurgency_campaigns(&ctx, ctx.now_unix()).await;
    let active_st = get_zarzakh_horizon_siege_state(&ctx);
    assert_eq!(active_st.phase, HorizonSiegePhase::Active);
    assert!(active_st.fulcrum_restricted_to_pirates);
    assert!(is_zarzakh_horizon_siege_active(&ctx));

    let fulcrum_pos = ctx.sde.stations[&FULCRUM_STATION_ID].position;
    let mut neutral_fulcrum = setup_space_pilot(
        &ctx,
        884_508,
        char_b,
        ship_b,
        587,
        ZARZAKH_SYS,
        fulcrum_pos + DVec3::new(100.0, 0.0, 0.0),
        None,
    )
    .await;
    let fulcrum_siege_err = rpc(&ctx, &mut neutral_fulcrum, "beyonce", "CmdDock", &[PyRep::Integer(FULCRUM_STATION_ID)])
        .await
        .expect_err("Non-pirate must be refused Fulcrum docking during active Horizon Siege");
    assert_eq!(fulcrum_siege_err.code, "HorizonSiegeFulcrumLockdown");

    let reloaded = persist_and_reopen_sqlite(&ctx, "b8_horizon_siege");
    assert!(is_zarzakh_horizon_siege_active(&reloaded));

    let z_turnur_pos = reloaded.sde.jumps[&ZARZAKH_TURNUR_GATE_ID].position;
    let mut siege_exit = setup_space_pilot(
        &reloaded,
        884_510,
        char_a,
        ship_a,
        621,
        ZARZAKH_SYS,
        z_turnur_pos + DVec3::new(200.0, 0.0, 0.0),
        Some(500_010),
    )
    .await;
    let gate_siege_err = gate_jump(&reloaded, &mut siege_exit, ZARZAKH_TURNUR_GATE_ID, TURNUR_GATE_ID, ship_a)
        .await
        .expect_err("Horizon Siege must block Zarzakh stargates after SQLite reload");
    assert_eq!(gate_siege_err.code, ERR_HORIZON_SIEGE_GATE);

    reloaded
        .systems
        .get_or_spawn(ZARZAKH_SYS)
        .ballpark
        .write()
        .unwrap()
        .balls
        .get_mut(&ship_a)
        .unwrap()
        .position = DVec3::new(250_000.0, 0.0, 0.0);
    let caster_siege_err = rpc(
        &reloaded,
        &mut siege_exit,
        "shipcasterTravelMgr",
        "CmdJumpThroughShipcaster",
        &[PyRep::Integer(105_000_010), PyRep::Integer(30_002_056), PyRep::Integer(104_000_020)],
    )
    .await
    .expect_err("Horizon Siege must block Zarzakh Deathless Shipcaster after SQLite reload");
    assert_eq!(caster_siege_err.code, ERR_HORIZON_SIEGE_CASTER);

    let after_siege_ts = get_zarzakh_horizon_siege_state(&reloaded).active_until_unix + 5;
    tick_insurgency_campaigns(&reloaded, after_siege_ts).await;
    assert_eq!(get_zarzakh_horizon_siege_state(&reloaded).phase, HorizonSiegePhase::Stabilizing);
    assert!(!is_zarzakh_horizon_siege_active(&reloaded));
    let after_cooldown_ts = get_zarzakh_horizon_siege_state(&reloaded).cooldown_until_unix + 5;
    tick_insurgency_campaigns(&reloaded, after_cooldown_ts).await;
    assert_eq!(get_zarzakh_horizon_siege_state(&reloaded).phase, HorizonSiegePhase::Normal);
}
