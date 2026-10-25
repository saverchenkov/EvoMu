//! Parity test suite for Full RPC & Notification Closure (`Z-1` & `Z-2`).

#![forbid(unsafe_code)]

use crate::common;

use eve_machonet::{MachoPacket, MachoPacketType, SessionState};
use eve_proto::{marshal, PyRep};
use eve_server::router::{
    cache_notifications::{NotificationTarget, MAX_PENDING_NOTIFICATIONS_PER_CHAR},
    ServerContext,
};
use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;

fn root_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

#[tokio::test]
// parity: plan:P-2, plan:P-3, plan:P-4
async fn test_z1_systematic_rpc_catalog_dispatch_closure_and_wire_formatting() {
    // parity: rpc:charUnboundMgr.GetCharacterInfo, rpc:charUnboundMgr.SelectCharacterID, rpc:corpRegistry.AddCorporation, rpc:account.GetWalletDivisionsInfo, rpc:marketProxy.GetCharOrders, rpc:marketProxy.GetOrders, rpc:slash.SlashCmd, rpc:stationSvc.GetStation
    let ctx = ServerContext::new();
    ctx.seed_demo_world_fixtures();
    let empty_kw = HashMap::new();

    // 1. Load the authoritative RPC service catalog (216 machoNet services)
    let catalog_path = root_dir().join("docs/index/rpc_service_catalog.json");
    let catalog_raw = std::fs::read_to_string(&catalog_path)
        .unwrap_or_else(|_| panic!("Failed to read {}", catalog_path.display()));
    let catalog: serde_json::Value =
        serde_json::from_str(&catalog_raw).expect("Parse rpc_service_catalog.json");
    let catalog_map = catalog.as_object().expect("Catalog JSON must be an object");

    assert_eq!(
        catalog_map.len(),
        216,
        "Authoritative catalog must contain exactly 216 services"
    );

    // 2. Setup an authenticated pilot session (character 90000001 in solar system 30000142, docked at 60003760)
    let mut sess = SessionState::new(101);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_001)],
        &empty_kw,
        Some("test:Z-1:SelectCharacterID"),
    )
    .await
    .expect("SelectCharacterID");

    assert_eq!(sess.character_id, Some(90_000_001));
    assert_eq!(sess.solar_system_id, Some(30_000_142));

    // 3. Systematically sweep every catalog service and every client candidate method (1,230 methods across 183 services)
    let mut total_candidate_methods = 0usize;
    let mut total_services_tested = 0usize;
    let mut legacy_services_tested = 0usize;
    let mut concrete_ok_methods = 0usize;
    let mut domain_error_methods = 0usize;
    let mut unimplemented_methods = 0usize;

    for (service_name, service_meta) in catalog_map {
        total_services_tested += 1;
        let surface_cat = service_meta
            .get("surface_category")
            .and_then(|v| v.as_str())
            .unwrap_or("v24_active");
        let is_legacy = surface_cat == "crucible_legacy_only";
        if is_legacy {
            legacy_services_tested += 1;
        }

        let cands: Vec<String> = service_meta
            .get("v24_client_candidate_methods")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|s| s.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();

        for method in &cands {
            total_candidate_methods += 1;

            // Dispatch each candidate method with empty arguments on an authenticated session copy
            let mut sweep_sess = sess.clone();
            let res = ctx
                .dispatch_rpc(
                    &mut sweep_sess,
                    service_name,
                    None,
                    method,
                    &[],
                    &empty_kw,
                    Some("test:Z-1:SystematicSweep"),
                )
                .await;

            match res {
                Ok(pyrep) => {
                    concrete_ok_methods += 1;
                    // Verify wire-level MachoPacket CallRsp encoding and binary marshal round-trip
                    let (pkt, marshaled) = ServerContext::build_macho_call_response_packet(
                        Some(service_name),
                        101,
                        total_candidate_methods as u64,
                        sess.user_id,
                        pyrep.clone(),
                    );
                    assert_eq!(pkt.packet_type, MachoPacketType::CallRsp);
                    let decoded_rep = marshal::decode(&marshaled).expect("Decode CallRsp wire bytes");
                    let decoded_pkt =
                        MachoPacket::from_pyrep(&decoded_rep).expect("Parse CallRsp MachoPacket");
                    assert_eq!(decoded_pkt.packet_type, MachoPacketType::CallRsp);
                    assert_eq!(decoded_pkt.user_id, sess.user_id);
                }
                Err(err) => {
                    assert!(
                        !err.code.is_empty(),
                        "RPC error code must not be empty for {service_name}.{method}"
                    );
                    assert!(
                        !err.message.is_empty(),
                        "RPC error message must not be empty for {service_name}.{method}"
                    );

                    if err.code == "UnimplementedRPC" {
                        unimplemented_methods += 1;
                    } else {
                        domain_error_methods += 1;
                    }

                    // For legacy Crucible services, verify they strictly return UnimplementedRPC
                    if is_legacy {
                        assert_eq!(
                            err.code, "UnimplementedRPC",
                            "Legacy service {service_name}.{method} must return UnimplementedRPC"
                        );
                    }

                    // Verify wire-level MachoPacket ErrorResponse encoding and binary marshal round-trip
                    let (pkt, marshaled) = ServerContext::build_macho_error_response_packet(
                        Some(service_name),
                        101,
                        total_candidate_methods as u64,
                        sess.user_id,
                        &err.code,
                        &err.message,
                    );
                    assert_eq!(pkt.packet_type, MachoPacketType::ErrorResponse);
                    let decoded_rep =
                        marshal::decode(&marshaled).expect("Decode ErrorResponse wire bytes");
                    let decoded_pkt = MachoPacket::from_pyrep(&decoded_rep)
                        .expect("Parse ErrorResponse MachoPacket");
                    assert_eq!(decoded_pkt.packet_type, MachoPacketType::ErrorResponse);
                }
            }
        }
    }

    assert_eq!(total_services_tested, 216);
    assert_eq!(legacy_services_tested, 23);
    assert_eq!(
        total_candidate_methods, 1230,
        "Must systematically sweep all 1,230 cataloged candidate methods"
    );
    assert!(concrete_ok_methods > 0, "Must have concrete handled RPC methods");
    assert!(domain_error_methods > 0, "Must have domain error RPC methods");
    assert!(unimplemented_methods > 0, "Must have structured UnimplementedRPC fallback methods");

    // Re-dock pilot at Jita 4-4 for station-bound service testing following the systematic navigation sweep
    let _ = ctx.db.dock_character(90_000_001, 60_003_760, false); ctx.advance_secs(12);

    // 4. Concrete functional verification across major domain services:
    // a) charUnboundMgr.GetCharactersToSelect
    let chars_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "charUnboundMgr",
            None,
            "GetCharactersToSelect",
            &[],
            &empty_kw,
            None,
        )
        .await
        .expect("charUnboundMgr.GetCharactersToSelect");
    assert!(matches!(
        chars_rep,
        PyRep::Rowset { .. } | PyRep::List(_) | PyRep::Object { .. } | PyRep::ObjectEx { .. }
    ));

    // b) charUnboundMgr.GetCharacterInfo
    let char_info = ctx
        .dispatch_rpc(
            &mut sess,
            "charUnboundMgr",
            None,
            "GetCharacterInfo",
            &[PyRep::Integer(90_000_001)],
            &empty_kw,
            None,
        )
        .await
        .expect("charUnboundMgr.GetCharacterInfo");
    assert!(matches!(char_info, PyRep::KeyVal(_) | PyRep::Object { .. }));

    // c) stationSvc.GetStation
    let st_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "stationSvc",
            None,
            "GetStation",
            &[PyRep::Integer(60_003_760)],
            &empty_kw,
            None,
        )
        .await
        .expect("stationSvc.GetStation");
    assert!(matches!(st_rep, PyRep::KeyVal(_)));

    // d) fleetMgr.CreateFleet & GetFleet
    let fleet_rep = ctx
        .dispatch_rpc(&mut sess, "fleetMgr", None, "CreateFleet", &[], &empty_kw, None)
        .await
        .expect("fleetMgr.CreateFleet");
    assert!(matches!(fleet_rep, PyRep::KeyVal(_)));
    let get_fleet = ctx
        .dispatch_rpc(&mut sess, "fleetMgr", None, "GetFleet", &[], &empty_kw, None)
        .await
        .expect("fleetMgr.GetFleet");
    assert!(matches!(get_fleet, PyRep::KeyVal(_)));

    // e) corpmgr.GetMembers, corpRegistry.AddCorporation, & account.GetWalletDivisionsInfo
    let corp_members = ctx
        .dispatch_rpc(&mut sess, "corpmgr", None, "GetMembers", &[], &empty_kw, None)
        .await
        .expect("corpmgr.GetMembers");
    assert!(matches!(corp_members, PyRep::Rowset { .. }));

    let create_corp = ctx
        .dispatch_rpc(
            &mut sess,
            "corpRegistry",
            None,
            "AddCorporation",
            &[
                PyRep::String("Z1 Closure Corp".into()),
                PyRep::String("Z1CC".into()),
                PyRep::String("Z1 Corp Description".into()),
            ],
            &empty_kw,
            None,
        )
        .await
        .expect("corpRegistry.AddCorporation");
    assert!(matches!(create_corp, PyRep::KeyVal(_)));

    let corp_wallets = ctx
        .dispatch_rpc(
            &mut sess,
            "account",
            None,
            "GetWalletDivisionsInfo",
            &[],
            &empty_kw,
            None,
        )
        .await
        .expect("account.GetWalletDivisionsInfo");
    assert!(matches!(corp_wallets, PyRep::Rowset { .. }));

    // f) LSC.JoinChannel & SendMessage
    let join_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "LSC",
            None,
            "JoinChannel",
            &[PyRep::String("test_chan".into())],
            &empty_kw,
            None,
        )
        .await
        .expect("LSC.JoinChannel");
    assert!(matches!(join_rep, PyRep::KeyVal(_)));

    let msg_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "LSC",
            None,
            "SendMessage",
            &[
                PyRep::String("test_chan".into()),
                PyRep::String("Hello from Z-1!".into()),
            ],
            &empty_kw,
            None,
        )
        .await
        .expect("LSC.SendMessage");
    assert!(matches!(msg_rep, PyRep::KeyVal(_)));

    // g) mailMgr.GetHeaders
    let mail_headers = ctx
        .dispatch_rpc(&mut sess, "mailMgr", None, "GetHeaders", &[], &empty_kw, None)
        .await
        .expect("mailMgr.GetHeaders");
    assert!(matches!(mail_headers, PyRep::Rowset { .. }));

    // h) bookmark.GetBookmarks
    let bms = ctx
        .dispatch_rpc(&mut sess, "bookmark", None, "GetBookmarks", &[], &empty_kw, None)
        .await
        .expect("bookmark.GetBookmarks");
    assert!(matches!(bms, PyRep::Rowset { .. }));

    // i) marketProxy.GetOrders & marketProxy.GetCharOrders
    let orders = ctx
        .dispatch_rpc(
            &mut sess,
            "marketProxy",
            None,
            "GetOrders",
            &[PyRep::Integer(34)],
            &empty_kw,
            None,
        )
        .await
        .expect("marketProxy.GetOrders");
    assert!(matches!(orders, PyRep::Tuple(ref items) if items.len() == 2));

    let char_orders = ctx
        .dispatch_rpc(
            &mut sess,
            "marketProxy",
            None,
            "GetCharOrders",
            &[],
            &empty_kw,
            None,
        )
        .await
        .expect("marketProxy.GetCharOrders");
    assert!(matches!(char_orders, PyRep::Rowset { .. }));

    // j) charFittingMgr.GetFittings
    let fittings = ctx
        .dispatch_rpc(
            &mut sess,
            "charFittingMgr",
            None,
            "GetFittings",
            &[],
            &empty_kw,
            None,
        )
        .await
        .expect("charFittingMgr.GetFittings");
    assert!(matches!(fittings, PyRep::Rowset { .. }));

    // 5. Test MachoBindObject with piggybacked method call
    let bind_res = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            None,
            "MachoBindObject",
            &[
                PyRep::Integer(60_003_760),
                PyRep::Tuple(vec![
                    PyRep::String("List".into()),
                    PyRep::Tuple(vec![]),
                    PyRep::Dict(vec![]),
                ]),
            ],
            &empty_kw,
            None,
        )
        .await
        .expect("MachoBindObject piggyback call");
    assert!(matches!(bind_res, PyRep::Tuple(ref items) if items.len() == 2));

    // 6. Test security & access controls:
    // a) Unauthenticated call to protected service
    let mut unauth_sess = SessionState::new(999);
    let unauth_err = ctx
        .dispatch_rpc(
            &mut unauth_sess,
            "charUnboundMgr",
            None,
            "GetCharacterInfo",
            &[],
            &empty_kw,
            None,
        )
        .await
        .expect_err("Unauthenticated protected call must fail");
    assert_eq!(unauth_err.code, "Unauthorized");

    // b) Non-GM call to GM slash command
    let gm_err = ctx
        .dispatch_rpc(
            &mut sess,
            "slash",
            None,
            "SlashCmd",
            &[PyRep::String("/giveisk 1000000".into())],
            &empty_kw,
            None,
        )
        .await
        .expect_err("Non-GM slash command call must fail");
    assert_eq!(gm_err.code, "InsufficientRole");
}

#[tokio::test]
// parity: plan:P-5
async fn test_z2_systematic_notification_emission_pipelines_and_scope_multicast() {
    // parity: rpc:charUnboundMgr.SelectCharacterID
    let ctx = ServerContext::new();
    ctx.seed_demo_world_fixtures();
    let empty_kw = HashMap::new();

    // 1. Provision & login 3 distinct pilots for multi-target multicast testing:
    // Pilot 1 (Demo, char 90000001, corp 98000001, sys 30000142, station 60003760)
    let mut sess1 = SessionState::new(201);
    common::login_pilot(&ctx, &mut sess1, "demo", "demo-secret").await;
    ctx.dispatch_rpc(
        &mut sess1,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_001)],
        &empty_kw,
        None,
    )
    .await
    .expect("Select Pilot 1");

    // Pilot 2 (Rival/Other, char 90000002, corp 98000001, sys 30000142, station 60003760)
    let mut sess2 = SessionState::new(202);
    common::login_pilot(&ctx, &mut sess2, "other", "other-secret").await;
    ctx.dispatch_rpc(
        &mut sess2,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_002)],
        &empty_kw,
        None,
    )
    .await
    .expect("Select Pilot 2");

    // Pilot 3 (Third pilot, char 90000003 in DIFFERENT solar system 30000144 and different corp 98000002)
    let p3_uid = ctx
        .db
        .provision_player_account("pilot3", "pilot3-secret");
    let p3_char = ctx
        .db
        .create_character(
            p3_uid,
            "Pilot Three",
            1,
            1,
            11,
            17,
            &ctx.sde,
        )
        .expect("create character for pilot3");
    let p3_char_id = p3_char.character_id;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&p3_char_id) {
            ch.solar_system_id = 30_000_144;
            ch.corporation_id = 98_000_002;
            ch.station_id = Some(60_003_754);
        }
    });

    let mut sess3 = SessionState::new(203);
    common::login_pilot(&ctx, &mut sess3, "pilot3", "pilot3-secret").await;
    ctx.dispatch_rpc(
        &mut sess3,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(p3_char_id)],
        &empty_kw,
        None,
    )
    .await
    .expect("Select Pilot 3");

    // Align Pilot 1 and Pilot 2 to Corporation 98000001 (Pilot 3 is in 98000002)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.corporation_id = 98_000_001;
            ch.station_id = Some(60_003_760);
        }
        if let Some(ch) = snap.characters.get_mut(&90_000_002) {
            ch.corporation_id = 98_000_001;
            ch.station_id = Some(60_003_760);
        }
    });

    // Drain any bootstrap notifications
    let _ = ctx.drain_notifications_for_char(90_000_001);
    let _ = ctx.drain_notifications_for_char(90_000_002);
    let _ = ctx.drain_notifications_for_char(p3_char_id);

    // =========================================================================
    // Pipeline 1: OnAccountChange
    // =========================================================================
    let notifs1 = ctx.notify_account_change(90_000_001, "cash", 90_000_001, 75_000_000.0);
    assert_eq!(notifs1.len(), 1);
    assert_eq!(notifs1[0].event, "OnAccountChange");
    assert_eq!(notifs1[0].id_type, "charid");
    assert_eq!(notifs1[0].args.len(), 3);
    assert_eq!(notifs1[0].args[0], PyRep::String("cash".into()));
    assert_eq!(notifs1[0].args[1], PyRep::Integer(90_000_001));
    assert_eq!(notifs1[0].args[2], PyRep::Real(75_000_000.0));

    // Verify binary packet marshal round-trip
    let rep1 = marshal::decode(&notifs1[0].marshaled_packet).expect("decode OnAccountChange");
    let pkt1 = MachoPacket::from_pyrep(&rep1).expect("parse OnAccountChange packet");
    assert_eq!(pkt1.packet_type, MachoPacketType::Notification);

    // =========================================================================
    // Pipeline 2: OnItemChange
    // =========================================================================
    let sample_item = PyRep::Tuple(vec![
        PyRep::Integer(100_001),
        PyRep::Integer(34),
        PyRep::Integer(90_000_001),
        PyRep::Integer(60_003_760),
        PyRep::Integer(4),
        PyRep::Integer(500),
    ]);
    let notifs2 = ctx.notify_item_change(
        90_000_001,
        sample_item.clone(),
        vec![(5, PyRep::Integer(600))],
    );
    assert_eq!(notifs2.len(), 1);
    assert_eq!(notifs2[0].event, "OnItemChange");
    assert_eq!(notifs2[0].id_type, "charid");
    assert_eq!(notifs2[0].args.len(), 2);
    assert_eq!(notifs2[0].args[0], sample_item);
    let rep2 = marshal::decode(&notifs2[0].marshaled_packet).expect("decode OnItemChange");
    let pkt2 = MachoPacket::from_pyrep(&rep2).expect("parse OnItemChange packet");
    assert_eq!(pkt2.packet_type, MachoPacketType::Notification);

    // =========================================================================
    // Pipeline 3: OnSkillTrained
    // =========================================================================
    let notifs3 = ctx.notify_skill_trained(90_000_001, 501, 3300, 5);
    assert_eq!(notifs3.len(), 1);
    assert_eq!(notifs3[0].event, "OnSkillTrained");
    assert_eq!(notifs3[0].id_type, "charid");
    assert_eq!(
        notifs3[0].args,
        vec![
            PyRep::Integer(501),
            PyRep::Integer(3300),
            PyRep::Integer(5),
        ]
    );

    // =========================================================================
    // Pipeline 4: OnDamageStateChange (SolarSystem multicast scope)
    // =========================================================================
    let notifs4 = ctx.notify_damage_state_change(
        NotificationTarget::SolarSystem(30_000_142),
        100_001,
        0.80,
        0.95,
        1.0,
    );
    // Pilot 1 and Pilot 2 are in 30000142, Pilot 3 is in 30000144
    assert_eq!(
        notifs4.len(),
        2,
        "Both pilots in system 30000142 must receive OnDamageStateChange"
    );
    let recipients4: BTreeSet<i64> = notifs4.iter().map(|n| n.recipient_char_id).collect();
    assert!(recipients4.contains(&90_000_001));
    assert!(recipients4.contains(&90_000_002));
    assert!(!recipients4.contains(&p3_char_id), "Pilot 3 in other system must NOT receive it");
    assert_eq!(notifs4[0].event, "OnDamageStateChange");
    assert_eq!(notifs4[0].id_type, "itemid");

    // =========================================================================
    // Pipeline 5: OnGodmaShipEffect
    // =========================================================================
    let notifs5 = ctx.notify_godma_ship_effect(
        NotificationTarget::Character(90_000_001),
        100_001,
        16,
        true,
        6000,
        1,
    );
    assert_eq!(notifs5.len(), 1);
    assert_eq!(notifs5[0].event, "OnGodmaShipEffect");
    assert_eq!(notifs5[0].id_type, "charid");
    assert_eq!(notifs5[0].args.len(), 11);
    assert_eq!(notifs5[0].args[0], PyRep::Integer(100_001));
    assert_eq!(notifs5[0].args[1], PyRep::Integer(16));
    assert_eq!(notifs5[0].args[3], PyRep::Integer(1)); // start = 1
    assert_eq!(notifs5[0].args[4], PyRep::Integer(1)); // active = 1
    assert_eq!(notifs5[0].args[7], PyRep::Integer(6000)); // duration_ms = 6000
    assert_eq!(notifs5[0].args[8], PyRep::Integer(1)); // repeat = 1

    // =========================================================================
    // Pipeline 6: OnFleetBroadcast (Fleet multicast scope)
    // =========================================================================
    // Create a fleet with Pilot 1 (Commander) and invite Pilot 2 (Member)
    let fid = ctx.db.with_mut_snapshot(|snap| {
        snap.next_entity_id += 1;
        let id = snap.next_entity_id;
        snap.fleets.insert(
            id,
            eve_db::FleetRecord {
                fleet_id: id,
                boss_id: 90_000_001,
                members: vec![90_000_001, 90_000_002],
                ..Default::default()
            },
        );
        id
    });

    let notifs6 = ctx.notify_fleet_broadcast(fid, "Target", 90_000_001, 7001);
    assert_eq!(notifs6.len(), 2, "Both fleet members must receive OnFleetBroadcast");
    let recipients6: BTreeSet<i64> = notifs6.iter().map(|n| n.recipient_char_id).collect();
    assert!(recipients6.contains(&90_000_001));
    assert!(recipients6.contains(&90_000_002));
    assert!(!recipients6.contains(&p3_char_id), "Non-fleet pilot must NOT receive it");
    assert_eq!(notifs6[0].event, "OnFleetBroadcast");
    assert_eq!(notifs6[0].id_type, "fleetid");
    assert_eq!(
        notifs6[0].args,
        vec![
            PyRep::String("Target".into()),
            PyRep::Integer(90_000_001),
            PyRep::Integer(7001),
            PyRep::Integer(fid),
        ]
    );

    // =========================================================================
    // Pipeline 7: OnClientEvent_ChatMessageSent (Chat broadcast message)
    // =========================================================================
    let notifs7 = ctx.notify_lsc_message(
        NotificationTarget::SolarSystem(30_000_142),
        "solarsystemid2:30000142",
        90_000_001,
        "Local broadcast test",
    );
    assert_eq!(notifs7.len(), 2);
    assert_eq!(notifs7[0].event, "OnClientEvent_ChatMessageSent");
    assert_eq!(notifs7[0].id_type, "channelid");
    assert_eq!(
        notifs7[0].args,
        vec![
            PyRep::String("solarsystemid2:30000142".into()),
            PyRep::Integer(90_000_001),
            PyRep::String("Local broadcast test".into()),
        ]
    );

    // =========================================================================
    // Pipeline 8: OnWarpToAsteroidBelt
    // =========================================================================
    let notifs8 = ctx.notify_warp_to_asteroid_belt(90_000_001, 40_000_001);
    assert_eq!(notifs8.len(), 1);
    assert_eq!(notifs8[0].event, "OnWarpToAsteroidBelt");
    assert_eq!(notifs8[0].id_type, "charid");
    assert_eq!(
        notifs8[0].args,
        vec![PyRep::Integer(90_000_001), PyRep::Integer(40_000_001)]
    );

    // =========================================================================
    // Pipeline 9: OnAsteroidTerminated
    // =========================================================================
    let notifs9 = ctx.notify_asteroid_terminated(30_000_142, 55_000_001);
    assert_eq!(notifs9.len(), 2);
    assert_eq!(notifs9[0].event, "OnAsteroidTerminated");
    assert_eq!(notifs9[0].id_type, "solarsystemid");
    assert_eq!(notifs9[0].args, vec![PyRep::Integer(55_000_001)]);

    // =========================================================================
    // Pipeline 10: OnMiningLedgerGraphSettingChanged
    // =========================================================================
    let notifs10 = ctx.notify_mining_ledger_graph_setting_changed(
        90_000_001,
        "groupBy",
        PyRep::String("oreType".into()),
    );
    assert_eq!(notifs10.len(), 1);
    assert_eq!(notifs10[0].event, "OnMiningLedgerGraphSettingChanged");
    assert_eq!(notifs10[0].id_type, "charid");
    assert_eq!(
        notifs10[0].args,
        vec![
            PyRep::String("groupBy".into()),
            PyRep::String("oreType".into()),
        ]
    );

    // =========================================================================
    // Target Scope Multicast Checks: Corporation, Alliance, Station, All
    // =========================================================================
    // Corporation scope (Pilot 1 and Pilot 2 are in corp 98000001)
    let corp_notifs = ctx.notify(
        NotificationTarget::Corporation(98_000_001),
        "OnCorporationChanged",
        "corpid",
        vec![PyRep::String("Corp announcement".into())],
    );
    assert_eq!(corp_notifs.len(), 2);
    let corp_recips: BTreeSet<i64> = corp_notifs.iter().map(|n| n.recipient_char_id).collect();
    assert!(corp_recips.contains(&90_000_001));
    assert!(corp_recips.contains(&90_000_002));
    assert!(!corp_recips.contains(&p3_char_id));

    // Station scope (Pilot 1 and Pilot 2 are in station 60003760)
    let station_notifs = ctx.notify(
        NotificationTarget::Station(60_003_760),
        "OnCharNowInStation",
        "stationid",
        vec![PyRep::String("Station notice".into())],
    );
    assert_eq!(station_notifs.len(), 2);

    // All scope (all 3 online pilots)
    let all_notifs = ctx.notify(
        NotificationTarget::All,
        "OnServerMessage",
        "all",
        vec![PyRep::String("Maintenance in 15 minutes".into())],
    );
    assert_eq!(all_notifs.len(), 3);

    // =========================================================================
    // Queue Overflow Protection & Drain Verification
    // =========================================================================
    // Exceed MAX_PENDING_NOTIFICATIONS_PER_CHAR (128)
    for i in 0..150 {
        ctx.notify_account_change(90_000_001, "cash", 90_000_001, i as f64);
    }
    let p1_queue = ctx.drain_notifications_for_char(90_000_001);
    assert_eq!(
        p1_queue.len(),
        MAX_PENDING_NOTIFICATIONS_PER_CHAR,
        "Pending notification queue must be capped at MAX_PENDING_NOTIFICATIONS_PER_CHAR (128)"
    );

    // Second drain must be empty
    let empty_queue = ctx.drain_notifications_for_char(90_000_001);
    assert!(empty_queue.is_empty(), "Queue must be empty after drain");
}
