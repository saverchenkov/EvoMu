//! Track 4: Wormholes, Fleets, Bookmarks, Scanning & Social Features (`SO-6`, Fleets, Bookmarks, Social).

use crate::common;

use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use glam::DVec3;
use std::collections::HashMap;

fn is_keyval(rep: &PyRep) -> bool {
    matches!(rep, PyRep::KeyVal(_))
}

fn is_rowset(rep: &PyRep) -> bool {
    matches!(rep, PyRep::Rowset { .. })
}

#[tokio::test]
async fn test_so6_wormholes_polarization_lifetime_mass_and_thera() {
    // parity: feat:navigation.wormhole-jump, feat:navigation.wormhole-polarization, feat:navigation.wormhole-lifetime, feat:navigation.wormhole-statics, feat:navigation.wormhole-thera, rpc:charUnboundMgr.SelectCharacterID, rpc:scanMgr.GetSystemScanMgr, rpc:structureDeployment.Anchor, rpc:wormholeMgr.GetWormholePolarization, rpc:wormholeMgr.WormholeJump
    eve_testkit::attest(&["feat:navigation.wormhole-jump", "feat:navigation.wormhole-polarization", "feat:navigation.wormhole-lifetime", "feat:navigation.wormhole-statics", "feat:navigation.wormhole-thera"]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();

    let mut sess = SessionState::new(601);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(90_000_001)], &kw, None).await.expect("Select Pilot 1");

    // Initialize system cosmic signatures and wormhole balls via real V24 scanMgr.GetSystemScanMgr
    let _ = ctx.dispatch_rpc(&mut sess, "scanMgr", None, "GetSystemScanMgr", &[], &kw, None).await.expect("GetSystemScanMgr");

    let (wh_sig_id, wh_pos) = ctx.db.with_snapshot(|snap| {
        let sig = snap.cosmic_signatures[&30_000_142].iter().find(|s| s.sig_group == "Wormhole").unwrap();
        (sig.signature_id.clone(), sig.position)
    });

    // Resolve wormhole item ID in Jita Ballpark (group_id = 988)
    let jita_sys = ctx.systems.get_or_spawn(30_000_142);
    let wh_item_id = eve_server::services::space_destiny::wormhole_item_id_for_signature(30_000_142, &wh_sig_id);
    let (_, wh_ball) = jita_sys.get_ball_snapshot(wh_item_id).await.unwrap().expect("Wormhole ball must be spawned in Jita Ballpark");
    assert_eq!(wh_ball.type_id, 26272);

    // Position pilot's Rifter (600_000_001) within 2,500m of the wormhole ball
    let ship_ball = Ball {
        id: 600_000_001,
        type_id: 587,
        position: DVec3::new(wh_pos[0] + 500.0, wh_pos[1], wh_pos[2]),
        velocity: DVec3::ZERO,
        max_velocity: 350.0,
        inertia: 2.5,
        mass: 1_200_000.0,
        radius: 50.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let _ = jita_sys.spawn_ball(ship_ball.clone()).await;
    sess.ship_id = Some(600_000_001);
    sess.solar_system_id = Some(30_000_142);
    sess.station_id = None;
    sess.structure_id = None;
    ctx.db.try_with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&90_000_001).unwrap();
        ch.active_ship_id = 600_000_001;
        ch.station_id = None;
        ch.structure_id = None;
        ch.solar_system_id = 30_000_142;
        Ok::<(), std::convert::Infallible>(())
    }).unwrap();
    ctx.advance_secs(12);

    // 1a. Range check: ship > 2,500m from wormhole is rejected with OutsideWormholeJumpRange
    let _ = jita_sys.spawn_ball(Ball { position: DVec3::new(wh_pos[0] + 10_000.0, wh_pos[1], wh_pos[2]), ..ship_ball.clone() }).await;
    let oor_err = ctx.dispatch_rpc(&mut sess, "wormholeMgr", None, "WormholeJump", &[PyRep::Integer(wh_item_id)], &kw, None).await.expect_err("Ship outside 2,500m wormhole range must be rejected");
    assert_eq!(oor_err.code, "OutsideWormholeJumpRange");

    // 1b. Max jump mass enforcement: ship exceeding max_jump_mass_kg (375,000,000 kg) is rejected
    let _ = jita_sys.spawn_ball(Ball { mass: 1_500_000_000.0, ..ship_ball.clone() }).await;
    let heavy_err = ctx.dispatch_rpc(&mut sess, "wormholeMgr", None, "WormholeJump", &[PyRep::Integer(wh_item_id)], &kw, None).await.expect_err("Ship exceeding max_jump_mass_kg must be rejected");
    assert_eq!(heavy_err.code, "ShipTooHeavyForWormhole");

    // 2. Jump Wormhole with active propulsion mass addition (1,200,000 + 500,000 = 1,700,000 kg) -> transits to Thera (31000005)
    let _ = jita_sys.spawn_ball(Ball { mass: 1_700_000.0, ..ship_ball.clone() }).await;
    let jump_rep = ctx.dispatch_rpc(&mut sess, "wormholeMgr", None, "WormholeJump", &[PyRep::Integer(wh_item_id)], &kw, None).await.expect("WormholeJump to Thera");
    let PyRep::KeyVal(jkv) = &jump_rep else { panic!("Expected KeyVal from WormholeJump") };
    assert_eq!(sess.solar_system_id, Some(31_000_005));
    assert_eq!(jkv["isThera"], PyRep::Bool(true));
    assert_eq!(jkv["wormholeClass"], PyRep::Integer(12));
    assert_eq!(jkv["massStage"], PyRep::String("Stable".into()));
    assert_eq!(jkv["wormholeSize"], PyRep::Real(1.0));
    assert_eq!(jkv["lifetimeStage"], PyRep::String("Healthy".into()));
    assert_eq!(jkv["wormholeAge"], PyRep::Integer(1));
    assert!(
        (jkv["remainingMassKg"].as_f64().unwrap() - (2_000_000_000.0 - 1_700_000.0)).abs() < 1.0,
        "WormholeJump must subtract jumping ship mass (including propulsion mass addition) from remaining_mass_kg"
    );

    // 3. Thera & Shattered W-space rules (`feat:navigation.wormhole-thera`)
    let _ = ctx.dispatch_rpc(&mut sess, "scanMgr", None, "GetSystemScanMgr", &[PyRep::Integer(31_000_005)], &kw, None).await.expect("seed Thera sigs");
    ctx.db.with_mut_snapshot(|snap| {
        for s in snap.cosmic_signatures.get_mut(&31_000_005).into_iter().flatten() {
            s.signal_strength = 1.0;
        }
    });
    let thera_scan = ctx.dispatch_rpc(&mut sess, "scanMgr", None, "GetSystemScanMgr", &[PyRep::Integer(31_000_005)], &kw, None).await.expect("GetSystemScanMgr in Thera");
    let PyRep::KeyVal(ts_kv) = &thera_scan else { panic!() };
    assert_eq!(ts_kv["isThera"], PyRep::Bool(true));
    let PyRep::List(thera_sigs) = &ts_kv["signatures"] else { panic!() };
    let thera_wh_count = thera_sigs.iter().filter(|s| s.dict_get("typeID").is_some_and(|v| v != &PyRep::None)).count();
    assert!(thera_wh_count >= 3, "Thera must have multiple static wormhole connections (got {thera_wh_count})");

    let thera_anchor_err = ctx.dispatch_rpc(&mut sess, "structureDeployment", None, "Anchor", &[PyRep::Integer(35832), PyRep::String("Thera Astrahus".into())], &kw, None).await.expect_err("Anchoring structures in Thera must be rejected");
    assert_eq!(thera_anchor_err.code, "AnchoringDisallowedInTheraOrShattered");

    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&90_000_001).unwrap().solar_system_id = 31_002_585;
    });
    sess.solar_system_id = Some(31_002_585);
    let shat_scan = ctx.dispatch_rpc(&mut sess, "scanMgr", None, "GetSystemScanMgr", &[PyRep::Integer(31_002_585)], &kw, None).await.unwrap();
    assert_eq!(shat_scan.dict_get("isShattered"), Some(&PyRep::Bool(true)));
    let shat_anchor_err = ctx.dispatch_rpc(&mut sess, "structureDeployment", None, "Anchor", &[PyRep::Integer(35832), PyRep::String("Shattered Astrahus".into())], &kw, None).await.unwrap_err();
    assert_eq!(shat_anchor_err.code, "AnchoringDisallowedInTheraOrShattered");

    // 4. Polarization Check (`feat:navigation.wormhole-polarization`) via wormholeMgr.GetWormholePolarization
    let pol_rep = ctx.dispatch_rpc(&mut sess, "wormholeMgr", None, "GetWormholePolarization", &[PyRep::Integer(31_000_005)], &kw, None).await.expect("GetWormholePolarization");
    assert_eq!(pol_rep.dict_get("isPolarized"), Some(&PyRep::Bool(true)));
    assert_eq!(pol_rep.dict_get("remainingSeconds"), Some(&PyRep::Integer(300)));

    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&90_000_001).unwrap().solar_system_id = 30_000_142;
    });
    let _ = jita_sys.spawn_ball(ship_ball.clone()).await;
    sess.solar_system_id = Some(30_000_142);
    ctx.advance_secs(12);
    let pol_err = ctx.dispatch_rpc(&mut sess, "wormholeMgr", None, "WormholeJump", &[PyRep::Integer(wh_item_id)], &kw, None).await.expect_err("Polarized pilot cannot jump same wormhole");
    assert_eq!(pol_err.code, "WormholePolarized");

    ctx.advance_secs(301);
    let pol_after = ctx.dispatch_rpc(&mut sess, "wormholeMgr", None, "GetWormholePolarization", &[PyRep::Integer(31_000_005)], &kw, None).await.unwrap();
    assert_eq!(pol_after.dict_get("isPolarized"), Some(&PyRep::Bool(false)));

    // 5. Intermediate mass & lifetime stages (`feat:navigation.wormhole-lifetime`)
    ctx.db.with_mut_snapshot(|snap| {
        let wh = snap.cosmic_signatures.get_mut(&30_000_142).unwrap().iter_mut().find(|s| s.signature_id == wh_sig_id).unwrap();
        wh.remaining_mass_kg = 800_000_000.0;
        wh.destination_system_id = Some(31_000_028);
    });
    let destab_jump = ctx.dispatch_rpc(&mut sess, "wormholeMgr", None, "WormholeJump", &[PyRep::Integer(wh_item_id)], &kw, None).await.expect("Jump through destabilized wormhole into Wolf-Rayet W-space");
    let PyRep::KeyVal(dkv) = &destab_jump else { panic!() };
    assert_eq!(dkv["massStage"], PyRep::String("Destabilized".into()));
    assert_eq!(dkv["wormholeSize"], PyRep::Real(0.5));
    assert_eq!(dkv["environmentalEffect"], PyRep::String("Wolf-Rayet".into()));

    let base_rifter_cs = ctx.build_ship_combat_state(90_000_001, 600_000_001, 587);
    let wr_handle = ctx.systems.get_or_spawn(31_000_028);
    let wr_cs = wr_handle.get_combat_state(600_000_001).await.unwrap().unwrap();
    assert!((wr_cs.max_armor_hp - base_rifter_cs.max_armor_hp * 1.30).abs() < 1.0);
    assert!((wr_cs.signature_radius - base_rifter_cs.signature_radius * 0.85).abs() < 1.0);

    // 6. Mass exhaustion collapse & automatic static wormhole respawn (`feat:navigation.wormhole-statics`)
    ctx.advance_secs(301);
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&90_000_001).unwrap().solar_system_id = 30_000_142;
        if let Some(sigs) = snap.cosmic_signatures.get_mut(&30_000_142) {
            if let Some(wh) = sigs.iter_mut().find(|s| s.signature_id == wh_sig_id) {
                wh.remaining_mass_kg = 800_000.0;
                wh.destination_system_id = Some(31_001_886);
            }
        }
    });
    sess.solar_system_id = Some(30_000_142);
    let _ = jita_sys.spawn_ball(ship_ball.clone()).await;

    let collapse_jump = ctx.dispatch_rpc(&mut sess, "wormholeMgr", None, "WormholeJump", &[PyRep::Integer(wh_item_id)], &kw, None).await.expect("WormholeJump collapsing mass");
    let PyRep::KeyVal(ckv) = &collapse_jump else { panic!() };
    assert_eq!(ckv["collapsed"], PyRep::Bool(true));
    assert_eq!(ckv["massStage"], PyRep::String("Collapsed_Exhausted".into()));
    assert_eq!(ckv["wormholeSize"], PyRep::Real(0.0));
    assert_eq!(ckv["environmentalEffect"], PyRep::String("Magnetar".into()));
    let replacement_sig_id = ckv["replacementSignatureID"].as_str().expect("Collapsed static wormhole must spawn a replacement static signature").to_string();
    assert_ne!(replacement_sig_id, wh_sig_id);
    assert!(jita_sys.get_ball_snapshot(wh_item_id).await.unwrap().is_none(), "Collapsed wormhole ball must be removed from Ballpark");

    let _ = ctx.dispatch_rpc(&mut sess, "scanMgr", None, "GetSystemScanMgr", &[PyRep::Integer(31_001_886)], &kw, None).await.unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        for s in snap.cosmic_signatures.get_mut(&31_001_886).into_iter().flatten() {
            s.signal_strength = 1.0;
        }
        for s in snap.cosmic_signatures.get_mut(&30_000_142).into_iter().flatten() {
            if s.signature_id == replacement_sig_id {
                s.signal_strength = 1.0;
            }
        }
    });
    let c5_scan = ctx.dispatch_rpc(&mut sess, "scanMgr", None, "GetSystemScanMgr", &[PyRep::Integer(31_001_886)], &kw, None).await.unwrap();
    let Some(PyRep::List(c5_sigs)) = c5_scan.dict_get("signatures") else { panic!() };
    assert!(c5_sigs.iter().any(|s| s.dict_get("signatureID").and_then(|v| v.as_str()).is_some_and(|id| id.starts_with("WST-")) && s.dict_get("typeID") == Some(&PyRep::Integer(30643))));
    assert!(c5_sigs.iter().any(|s| s.dict_get("signatureID").and_then(|v| v.as_str()).is_some_and(|id| id.starts_with("WND-")) && s.dict_get("typeID") == Some(&PyRep::Integer(30645))));

    // 7. Natural lifetime progression & natural expiration collapse
    let rep_wh_item_id = eve_server::services::space_destiny::wormhole_item_id_for_signature(30_000_142, &replacement_sig_id);
    let rep_pos = ctx.db.with_snapshot(|snap| {
        snap.cosmic_signatures[&30_000_142].iter().find(|s| s.signature_id == replacement_sig_id).unwrap().position
    });
    ctx.advance_secs(70_000);
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&90_000_001).unwrap().solar_system_id = 30_000_142;
    });
    sess.solar_system_id = Some(30_000_142);
    let jita_scan_eol = ctx.dispatch_rpc(&mut sess, "scanMgr", None, "GetSystemScanMgr", &[PyRep::Integer(30_000_142)], &kw, None).await.unwrap();
    let Some(PyRep::List(jita_sigs_eol)) = jita_scan_eol.dict_get("signatures") else { panic!() };
    let rep_sig_eol = jita_sigs_eol.iter().find(|s| s.dict_get("signatureID") == Some(&PyRep::String(replacement_sig_id.clone()))).expect("Replacement wormhole in EOL stage");
    assert_eq!(rep_sig_eol.dict_get("lifetimeStage"), Some(&PyRep::String("ReachingEndOfLifetime".into())));
    assert_eq!(rep_sig_eol.dict_get("wormholeAge"), Some(&PyRep::Integer(2)));

    ctx.advance_secs(20_000);
    let _ = jita_sys.spawn_ball(Ball { position: DVec3::new(rep_pos[0] + 200.0, rep_pos[1], rep_pos[2]), ..ship_ball }).await;

    let expired_err = ctx.dispatch_rpc(&mut sess, "wormholeMgr", None, "WormholeJump", &[PyRep::Integer(rep_wh_item_id)], &kw, None).await.expect_err("Naturally expired wormhole must collapse on jump attempt");
    assert_eq!(expired_err.code, "WormholeCollapsed");
}

#[tokio::test]
async fn test_so_fleets_motd_options_wings_squads_broadcasts_watchlist_formations_boost_bridge_disband() {
    // parity: feat:fleet.wings-squads, feat:fleet.roles-positions, feat:fleet.fleet-motd, feat:fleet.fleet-broadcasts, feat:fleet.fleet-warp-cmd, feat:fleet.fleet-composition, feat:fleet.fleet-watchlist, feat:fleet.fleet-advert, feat:fleet.fleet-free-move, feat:fleet.fleet-formations, feat:fleet.fleet-disband, feat:fleet.fleet-max-size, feat:fleet.fleet-boost-scope, feat:fleet.fleet-bridge, feat:fleet.fleet-registration, rpc:beyonce.GetFormations, rpc:charUnboundMgr.SelectCharacterID, rpc:fleetMgr.AddToWatchlist, rpc:fleetMgr.RegisterForDamageUpdates, rpc:fleetMgr.RemoveFromWatchlist, rpc:fleetObjectHandler.AcceptInvite, rpc:fleetObjectHandler.ChangeSquadName, rpc:fleetObjectHandler.ChangeWingName, rpc:fleetObjectHandler.CreateSquad, rpc:fleetObjectHandler.CreateWing, rpc:fleetObjectHandler.DeleteSquad, rpc:fleetObjectHandler.DeleteWing, rpc:fleetObjectHandler.DisbandFleet, rpc:fleetObjectHandler.GetFleetComposition, rpc:fleetObjectHandler.GetFleetMaxSize, rpc:fleetObjectHandler.GetInitState, rpc:fleetObjectHandler.GetMotd, rpc:fleetObjectHandler.MoveMember, rpc:fleetObjectHandler.SetFleetMaxSize, rpc:fleetObjectHandler.SetMotdEx, rpc:fleetObjectHandler.SetOptions, rpc:fleetObjectHandler.SetTakesFleetWarp, rpc:fleetProxy.AddFleetFinderAdvert, rpc:fleetProxy.ApplyToJoinFleet, rpc:fleetProxy.GetAvailableFleetAds, rpc:fleetProxy.UpdateAdvertAllowedEntities
    eve_testkit::attest(&["feat:fleet.wings-squads", "feat:fleet.roles-positions", "feat:fleet.fleet-motd", "feat:fleet.fleet-broadcasts", "feat:fleet.fleet-warp-cmd", "feat:fleet.fleet-composition", "feat:fleet.fleet-watchlist", "feat:fleet.fleet-advert", "feat:fleet.fleet-free-move", "feat:fleet.fleet-formations", "feat:fleet.fleet-disband", "feat:fleet.fleet-max-size", "feat:fleet.fleet-boost-scope", "feat:fleet.fleet-bridge", "feat:fleet.fleet-registration"]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();

    let mut sess1 = SessionState::new(801);
    common::login_pilot(&ctx, &mut sess1, "demo", "demo-secret").await;
    ctx.dispatch_rpc(&mut sess1, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(90_000_001)], &kw, None).await.unwrap();

    let mut sess2 = SessionState::new(802);
    common::login_pilot(&ctx, &mut sess2, "other", "other-secret").await;
    ctx.dispatch_rpc(&mut sess2, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(90_000_002)], &kw, None).await.unwrap();

    let p3_uid = ctx.db.provision_player_account("fleet_pilot3", "fp3-secret");
    let p3_char = ctx.db.create_character(p3_uid, "Fleet Pilot Three", 1, 1, 11, 17, &ctx.sde).unwrap().character_id;
    let mut sess3 = SessionState::new(803);
    common::login_pilot(&ctx, &mut sess3, "fleet_pilot3", "fp3-secret").await;
    ctx.dispatch_rpc(&mut sess3, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(p3_char)], &kw, None).await.unwrap();

    // 1. Fleet MOTD, Free-Move, Registration, Max Size, Wings/Squads, Roles/Positions, Broadcasts, Watchlist, Formations, Boost Scope, Bridge & Disband
    let create_fl = ctx.dispatch_rpc(&mut sess1, "fleetMgr", None, "CreateFleet", &[PyRep::String("Apex Strike".into()), PyRep::String("Initial MOTD".into())], &kw, None).await.unwrap();
    assert!(is_keyval(&create_fl));
    let fid = sess1.fleet_id.unwrap();

    let motd_rep = ctx.dispatch_rpc(&mut sess1, "fleetObjectHandler", None, "SetMotdEx", &[PyRep::String("Anchor on FC, prop mod on".into())], &kw, None).await.unwrap();
    assert!(is_keyval(&motd_rep));
    let got_motd = ctx.dispatch_rpc(&mut sess1, "fleetObjectHandler", None, "GetMotd", &[], &kw, None).await.unwrap();
    assert_eq!(got_motd, PyRep::String("Anchor on FC, prop mod on".into()));

    let opt_rep = ctx.dispatch_rpc(&mut sess1, "fleetObjectHandler", None, "SetOptions", &[PyRep::Bool(true), PyRep::Bool(true)], &kw, None).await.unwrap();
    let PyRep::KeyVal(opt_kv) = &opt_rep else { panic!() };
    assert_eq!(opt_kv["isFreeMove"], PyRep::Bool(true));
    assert_eq!(opt_kv["isRegistered"], PyRep::Bool(true));

    // Max size cap enforcement (`feat:fleet.fleet-max-size`): cap at 1 blocks Invite with FleetFull, then raise to 50
    ctx.dispatch_rpc(&mut sess1, "fleetObjectHandler", None, "SetFleetMaxSize", &[PyRep::Integer(1)], &kw, None).await.unwrap();
    let full_err = ctx.dispatch_rpc(&mut sess1, "fleetObjectHandler", None, "Invite", &[PyRep::Integer(90_000_002)], &kw, None).await.expect_err("Invite beyond maxSize must fail");
    assert_eq!(full_err.code, "FleetFull");
    ctx.dispatch_rpc(&mut sess1, "fleetObjectHandler", None, "SetFleetMaxSize", &[PyRep::Integer(50)], &kw, None).await.unwrap();
    assert_eq!(ctx.dispatch_rpc(&mut sess1, "fleetObjectHandler", None, "GetFleetMaxSize", &[], &kw, None).await.unwrap(), PyRep::Integer(50));

    // Wings & Squads (`feat:fleet.wings-squads`): CreateWing, ChangeWingName, CreateSquad, ChangeSquadName, DeleteSquad, DeleteWing
    let w2 = ctx.dispatch_rpc(&mut sess1, "fleetObjectHandler", None, "CreateWing", &[PyRep::String("Wing 2".into())], &kw, None).await.unwrap();
    let wing2_id = w2.dict_get("wingID").and_then(|v| v.as_i64()).unwrap();
    ctx.dispatch_rpc(&mut sess1, "fleetObjectHandler", None, "ChangeWingName", &[PyRep::Integer(wing2_id), PyRep::String("Recon Wing".into())], &kw, None).await.unwrap();
    let s2 = ctx.dispatch_rpc(&mut sess1, "fleetObjectHandler", None, "CreateSquad", &[PyRep::Integer(wing2_id), PyRep::String("Squad 2".into())], &kw, None).await.unwrap();
    let squad2_id = s2.dict_get("squadID").and_then(|v| v.as_i64()).unwrap();
    ctx.dispatch_rpc(&mut sess1, "fleetObjectHandler", None, "ChangeSquadName", &[PyRep::Integer(squad2_id), PyRep::String("CovOps Squad".into())], &kw, None).await.unwrap();
    let w3 = ctx.dispatch_rpc(&mut sess1, "fleetObjectHandler", None, "CreateWing", &[PyRep::String("Temp Wing".into())], &kw, None).await.unwrap();
    let wing3_id = w3.dict_get("wingID").and_then(|v| v.as_i64()).unwrap();
    let s3 = ctx.dispatch_rpc(&mut sess1, "fleetObjectHandler", None, "CreateSquad", &[PyRep::Integer(wing3_id), PyRep::String("Temp Squad".into())], &kw, None).await.unwrap();
    let squad3_id = s3.dict_get("squadID").and_then(|v| v.as_i64()).unwrap();
    ctx.dispatch_rpc(&mut sess1, "fleetObjectHandler", None, "DeleteSquad", &[PyRep::Integer(squad3_id)], &kw, None).await.unwrap();
    ctx.dispatch_rpc(&mut sess1, "fleetObjectHandler", None, "DeleteWing", &[PyRep::Integer(wing3_id)], &kw, None).await.unwrap();

    // Invite & AcceptInvite (`feat:fleet.fleet-motd`): joining member receives OnFleetMotdChanged on join and on subsequent SetMotdEx
    let _ = ctx.drain_notifications_for_char(90_000_002);
    ctx.dispatch_rpc(&mut sess1, "fleetObjectHandler", None, "Invite", &[PyRep::Integer(90_000_002)], &kw, None).await.unwrap();
    ctx.advance_secs(12);
    ctx.dispatch_rpc(&mut sess2, "fleetObjectHandler", None, "AcceptInvite", &[PyRep::Integer(fid)], &kw, None).await.unwrap();
    let join_notifs = ctx.drain_notifications_for_char(90_000_002);
    assert!(join_notifs.iter().any(|n| n.event == "OnFleetMotdChanged"), "Joining member must receive OnFleetMotdChanged");
    ctx.dispatch_rpc(&mut sess1, "fleetObjectHandler", None, "SetMotdEx", &[PyRep::String("Updated MOTD Live".into())], &kw, None).await.unwrap();
    assert!(ctx.drain_notifications_for_char(90_000_002).iter().any(|n| n.event == "OnFleetMotdChanged"), "Members must receive OnFleetMotdChanged on change");

    // Fleet Advert & Auto-Join (`feat:fleet.fleet-advert`, `feat:fleet.fleet-free-move`): AddFleetFinderAdvert, UpdateAdvertAllowedEntities, GetAvailableFleetAds, ApplyToJoinFleet
    ctx.dispatch_rpc(&mut sess1, "fleetProxy", None, "AddFleetFinderAdvert", &[PyRep::String("Apex Public".into()), PyRep::String("Autojoin".into()), PyRep::Integer(3), PyRep::Bool(true)], &kw, None).await.unwrap();
    ctx.dispatch_rpc(&mut sess1, "fleetProxy", None, "UpdateAdvertAllowedEntities", &[PyRep::List(vec![PyRep::Integer(90_000_002)])], &kw, None).await.unwrap();
    let denied_ads = ctx.dispatch_rpc(&mut sess3, "fleetProxy", None, "GetAvailableFleetAds", &[], &kw, None).await.unwrap();
    if let PyRep::Rowset { rows, .. } = &denied_ads { assert!(rows.is_empty(), "Unlisted entity must not see restricted advert"); }
    assert_eq!(ctx.dispatch_rpc(&mut sess3, "fleetProxy", None, "ApplyToJoinFleet", &[PyRep::Integer(fid)], &kw, None).await.unwrap_err().code, "FleetJoinDenied");
    ctx.dispatch_rpc(&mut sess1, "fleetProxy", None, "UpdateAdvertAllowedEntities", &[PyRep::List(vec![PyRep::Integer(p3_char)])], &kw, None).await.unwrap();
    let p3_join = ctx.dispatch_rpc(&mut sess3, "fleetProxy", None, "ApplyToJoinFleet", &[PyRep::Integer(fid)], &kw, None).await.unwrap();
    assert_eq!(p3_join.dict_get("joined"), Some(&PyRep::Bool(true)));
    sess3.fleet_id = Some(fid);

    // Roles & Positions (`feat:fleet.roles-positions`, `feat:fleet.fleet-free-move`, `feat:fleet.fleet-registration`)
    ctx.dispatch_rpc(&mut sess1, "fleetObjectHandler", None, "MoveMember", &[PyRep::Integer(90_000_002), PyRep::Integer(wing2_id), PyRep::Integer(0), PyRep::Integer(2)], &kw, None).await.unwrap();
    // Wing 2 Commander (Pilot 2) cannot kick or move Pilot 3 (who is in Wing 1)
    assert_eq!(ctx.dispatch_rpc(&mut sess2, "fleetObjectHandler", None, "KickMember", &[PyRep::Integer(p3_char)], &kw, None).await.unwrap_err().code, "FleetPermissionDenied");
    assert_eq!(ctx.dispatch_rpc(&mut sess2, "fleetObjectHandler", None, "MoveMember", &[PyRep::Integer(p3_char), PyRep::Integer(wing2_id), PyRep::Integer(squad2_id), PyRep::Integer(4)], &kw, None).await.unwrap_err().code, "FleetPermissionDenied");
    let occ_err = ctx.dispatch_rpc(&mut sess1, "fleetObjectHandler", None, "MoveMember", &[PyRep::Integer(p3_char), PyRep::Integer(wing2_id), PyRep::Integer(0), PyRep::Integer(2)], &kw, None).await.unwrap_err();
    assert_eq!(occ_err.code, "PositionOccupied");
    ctx.advance_secs(12);
    let fm_move = ctx.dispatch_rpc(&mut sess2, "fleetObjectHandler", None, "MoveMember", &[PyRep::Integer(90_000_002), PyRep::Integer(wing2_id), PyRep::Integer(squad2_id), PyRep::Integer(4)], &kw, None).await.unwrap();
    assert_eq!(fm_move.dict_get("squadID"), Some(&PyRep::Integer(squad2_id)));
    let init_st = ctx.dispatch_rpc(&mut sess1, "fleetObjectHandler", None, "GetInitState", &[], &kw, None).await.unwrap();
    assert_eq!(init_st.dict_get("isRegistered"), Some(&PyRep::Bool(true)));
    assert_eq!(init_st.dict_get("isFreeMove"), Some(&PyRep::Bool(true)));

    // Fleet Composition (`feat:fleet.fleet-composition`): SquadMember denied with FleetRoleDenied; FC receives composition rowset
    assert_eq!(ctx.dispatch_rpc(&mut sess2, "fleetObjectHandler", None, "GetFleetComposition", &[], &kw, None).await.unwrap_err().code, "FleetRoleDenied");
    assert!(is_rowset(&ctx.dispatch_rpc(&mut sess1, "fleetObjectHandler", None, "GetFleetComposition", &[], &kw, None).await.unwrap()));

    // Spawn ships in Jita (30000142) for Pilot 1 (at origin) and Pilot 2 (1,000m away), and Pilot 3 in Perimeter (30000144)
    let jita_sys = ctx.systems.get_or_spawn(30_000_142);
    let perim_sys = ctx.systems.get_or_spawn(30_000_144);
    let _ = jita_sys.spawn_ball(Ball { id: 800_000_001, type_id: 587, position: DVec3::ZERO, velocity: DVec3::ZERO, max_velocity: 350.0, inertia: 2.0, mass: 1_000_000.0, radius: 40.0, speed_fraction: 0.0, mode: MovementMode::Stop }).await;
    let _ = jita_sys.spawn_ball(Ball { id: 800_000_002, type_id: 587, position: DVec3::new(1_000.0, 0.0, 0.0), velocity: DVec3::ZERO, max_velocity: 350.0, inertia: 2.0, mass: 1_000_000.0, radius: 40.0, speed_fraction: 0.0, mode: MovementMode::Stop }).await;
    let _ = perim_sys.spawn_ball(Ball { id: 800_000_003, type_id: 587, position: DVec3::ZERO, velocity: DVec3::ZERO, max_velocity: 350.0, inertia: 2.0, mass: 1_000_000.0, radius: 40.0, speed_fraction: 0.0, mode: MovementMode::Stop }).await;
    sess1.ship_id = Some(800_000_001); sess1.station_id = None; sess1.solar_system_id = Some(30_000_142);
    sess2.ship_id = Some(800_000_002); sess2.station_id = None; sess2.solar_system_id = Some(30_000_142);
    sess3.ship_id = Some(800_000_003); sess3.station_id = None; sess3.solar_system_id = Some(30_000_144);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) { ch.active_ship_id = 800_000_001; ch.station_id = None; ch.solar_system_id = 30_000_142; }
        if let Some(ch) = snap.characters.get_mut(&90_000_002) { ch.active_ship_id = 800_000_002; ch.station_id = None; ch.solar_system_id = 30_000_142; }
        if let Some(ch) = snap.characters.get_mut(&p3_char) { ch.active_ship_id = 800_000_003; ch.station_id = None; ch.solar_system_id = 30_000_144; }
    });

    // Fleet Broadcast Scopes (`feat:fleet.fleet-broadcasts`)
    let _ = ctx.drain_notifications_for_char(90_000_002);
    let _ = ctx.drain_notifications_for_char(p3_char);
    ctx.dispatch_rpc(&mut sess1, "fleetObjectHandler", None, "BroadcastToSystem", &[PyRep::String("InPosition".into()), PyRep::Integer(800_000_001)], &kw, None).await.unwrap();
    assert!(ctx.drain_notifications_for_char(90_000_002).iter().any(|n| n.event == "OnFleetBroadcast"));
    assert!(!ctx.drain_notifications_for_char(p3_char).iter().any(|n| n.event == "OnFleetBroadcast"), "Out-of-system member must not receive BroadcastToSystem");
    ctx.dispatch_rpc(&mut sess1, "fleetObjectHandler", None, "BroadcastToBubble", &[PyRep::String("HealArmor".into()), PyRep::Integer(800_000_001)], &kw, None).await.unwrap();
    assert!(ctx.drain_notifications_for_char(90_000_002).iter().any(|n| n.event == "OnFleetBroadcast"));

    // Fleet Watchlist (`feat:fleet.fleet-watchlist`)
    let mut p2_cs = eve_destiny::CombatState::default();
    p2_cs.shield_hp = 250.0; p2_cs.max_shield_hp = 500.0; p2_cs.armor_hp = 300.0; p2_cs.max_armor_hp = 400.0; p2_cs.structure_hp = 350.0; p2_cs.max_structure_hp = 350.0;
    let _ = jita_sys.set_combat_state(800_000_002, p2_cs).await;
    let _ = ctx.drain_notifications_for_char(90_000_001);
    let wl_rep = ctx.dispatch_rpc(&mut sess1, "fleetMgr", None, "AddToWatchlist", &[PyRep::Integer(90_000_002)], &kw, None).await.unwrap();
    assert_eq!(wl_rep.dict_get("shieldRatio"), Some(&PyRep::Real(0.5)));
    assert_eq!(wl_rep.dict_get("armorRatio"), Some(&PyRep::Real(0.75)));
    assert!(ctx.drain_notifications_for_char(90_000_001).iter().any(|n| n.event == "OnFleetStateChange"));
    ctx.dispatch_rpc(&mut sess1, "fleetMgr", None, "RegisterForDamageUpdates", &[PyRep::List(vec![PyRep::Integer(90_000_002)])], &kw, None).await.unwrap();
    ctx.dispatch_rpc(&mut sess1, "fleetMgr", None, "RemoveFromWatchlist", &[PyRep::Integer(90_000_002)], &kw, None).await.unwrap();

    // Fleet Formations & Fleet Warp (`feat:fleet.fleet-formations`, `feat:fleet.fleet-warp-cmd`)
    let formations = ctx.dispatch_rpc(&mut sess1, "beyonce", None, "GetFormations", &[], &kw, None).await.unwrap();
    let PyRep::Tuple(form_items) = &formations else { panic!("beyonce.GetFormations must return a 2-tuple of formations") };
    assert_eq!(form_items.len(), 2);
    let fw_dest = DVec3::new(300_000_000.0, 0.0, 0.0);
    let fw_rep = ctx.dispatch_rpc(&mut sess1, "fleetObjectHandler", None, "FleetWarp", &[PyRep::Real(fw_dest.x), PyRep::Real(fw_dest.y), PyRep::Real(fw_dest.z), PyRep::Real(0.0), PyRep::Integer(1), PyRep::Real(5_000.0)], &kw, None).await.unwrap();
    assert_eq!(fw_rep.dict_get("warpedMembers"), Some(&PyRep::Integer(2)));
    let _ = jita_sys.set_ball_position(800_000_001, DVec3::ZERO).await;
    let _ = jita_sys.set_ball_position(800_000_002, DVec3::new(1_000.0, 0.0, 0.0)).await;
    ctx.dispatch_rpc(&mut sess2, "fleetObjectHandler", None, "SetTakesFleetWarp", &[PyRep::Bool(false)], &kw, None).await.unwrap();
    let fw_exempt = ctx.dispatch_rpc(&mut sess1, "fleetObjectHandler", None, "FleetWarp", &[PyRep::Real(fw_dest.x), PyRep::Real(fw_dest.y), PyRep::Real(fw_dest.z), PyRep::Real(0.0)], &kw, None).await.unwrap();
    assert_eq!(fw_exempt.dict_get("warpedMembers"), Some(&PyRep::Integer(1)), "Exempt member must not be warped by FleetWarp");

    // Fleet Boost Scope (`feat:fleet.fleet-boost-scope`)
    let _ = jita_sys.set_ball_position(800_000_001, DVec3::ZERO).await;
    let _ = jita_sys.set_ball_position(800_000_002, DVec3::new(2_000.0, 0.0, 0.0)).await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(880_101, eve_db::InventoryItemRecord { item_id: 880_101, type_id: 43552, owner_id: 90_000_001, location_id: 800_000_001, flag: eve_db::InventoryFlag::HiSlot0, quantity: 1, singleton: true, version: 1, ..Default::default() });
        snap.items.insert(880_102, eve_db::InventoryItemRecord { item_id: 880_102, type_id: 42836, owner_id: 90_000_001, location_id: 800_000_001, flag: eve_db::InventoryFlag::HiSlot0, quantity: 100, singleton: false, version: 1, ..Default::default() });
        snap.module_online_states.insert(880_101, true);
    });
    ctx.invalidate_ship_fitting(800_000_001);
    let burst_rep = ctx.dispatch_rpc(&mut sess1, "dogmaIM", None, "Activate", &[PyRep::Integer(880_101)], &kw, None).await.unwrap();
    let affected = burst_rep.dict_get("affectedMemberIDs").and_then(|v| match v { PyRep::List(l) => Some(l.clone()), _ => None }).unwrap_or_default();
    assert!(affected.contains(&PyRep::Integer(90_000_002)), "Fleet member in range in another wing/squad must receive command burst");
    assert!(!affected.contains(&PyRep::Integer(p3_char)), "Fleet member in another system must not receive command burst");

    // Fleet Bridge (`feat:fleet.fleet-bridge`)
    let bridge_err = ctx.dispatch_rpc(&mut sess2, "beyonce", None, "CmdJumpThroughFleet", &[PyRep::Integer(p3_char), PyRep::Integer(800_000_003)], &kw, None).await.unwrap_err();
    assert_eq!(bridge_err.code, "NoActiveBridge");

    // Disband fleet via `DisbandFleet` (`feat:fleet.fleet-disband`)
    let disband_rep = ctx.dispatch_rpc(&mut sess1, "fleetObjectHandler", None, "DisbandFleet", &[], &kw, None).await.unwrap();
    assert_eq!(disband_rep.dict_get("disbanded"), Some(&PyRep::Bool(true)));
    assert_eq!(sess1.fleet_id, None);
    assert!(ctx.db.with_snapshot(|s| !s.chat_channels.contains_key(&format!("fleetid:{fid}"))), "DisbandFleet must destroy fleet chat channel");

    sess2.fleet_id = None;
    let non_fleet_err = ctx.dispatch_rpc(&mut sess2, "beyonce", None, "CmdJumpThroughFleet", &[PyRep::Integer(p3_char), PyRep::Integer(800_000_003)], &kw, None).await.unwrap_err();
    assert_eq!(non_fleet_err.code, "FleetNotInFleet");
}

#[tokio::test]
async fn test_so_bookmarks_expiry_copy_move_corp_bookmarks_and_scanning_features() {
    // parity: feat:bookmarks.copy-move, feat:bookmarks.warp-to-bm, feat:bookmarks.bm-expiry, feat:bookmarks.corp-bookmarks, feat:scanning.probe-formations, feat:scanning.signatures, feat:scanning.anomalies, feat:scanning.structure-signals, feat:scanning.system-scan, feat:scanning.sig-despawn, feat:scanning.scan-skills, rpc:accessGroupBookmarkMgr.AddFolder, rpc:accessGroupBookmarkMgr.CopyBookmarksToFolderAndSubfolder, rpc:accessGroupBookmarkMgr.MoveBookmarksToFolderAndSubfolder, rpc:charUnboundMgr.SelectCharacterID, rpc:corpRegistry.AddCorporation, rpc:corpRegistry.InsertApplication, rpc:corpRegistry.UpdateApplicationOffer, rpc:scanMgr.SignalTrackerRegister
    eve_testkit::attest(&["feat:bookmarks.copy-move", "feat:bookmarks.warp-to-bm", "feat:bookmarks.bm-expiry", "feat:bookmarks.corp-bookmarks", "feat:scanning.probe-formations", "feat:scanning.signatures", "feat:scanning.anomalies", "feat:scanning.structure-signals", "feat:scanning.system-scan", "feat:scanning.sig-despawn", "feat:scanning.scan-skills"]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();
    let mut sess1 = SessionState::new(811);
    common::login_pilot(&ctx, &mut sess1, "demo", "demo-secret").await;
    ctx.dispatch_rpc(&mut sess1, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(90_000_001)], &kw, None).await.unwrap();
    let mut sess2 = SessionState::new(812);
    common::login_pilot(&ctx, &mut sess2, "other", "other-secret").await;
    ctx.dispatch_rpc(&mut sess2, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(90_000_002)], &kw, None).await.unwrap();
    let jita_sys = ctx.systems.get_or_spawn(30_000_142);
    let _ = jita_sys.spawn_ball(Ball { id: 800_000_001, type_id: 587, position: DVec3::ZERO, velocity: DVec3::ZERO, max_velocity: 350.0, inertia: 2.0, mass: 1_000_000.0, radius: 40.0, speed_fraction: 0.0, mode: MovementMode::Stop }).await;
    sess1.ship_id = Some(800_000_001); sess1.station_id = None; sess1.solar_system_id = Some(30_000_142);
    sess2.ship_id = Some(800_000_002); sess2.station_id = Some(60_003_760); sess2.solar_system_id = Some(30_000_142);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) { ch.active_ship_id = 800_000_001; ch.station_id = None; ch.solar_system_id = 30_000_142; }
        if let Some(ch) = snap.characters.get_mut(&90_000_002) { ch.active_ship_id = 800_000_002; ch.station_id = Some(60_003_760); ch.solar_system_id = 30_000_142; }
        snap.skills.entry(90_000_001).or_default().insert(3363, eve_db::TrainedSkillRecord { type_id: 3363, level: 5, skill_points: 256_000.0 });
    });
    ctx.advance_secs(12);
    ctx.dispatch_rpc(&mut sess1, "corpRegistry", None, "AddCorporation", &[PyRep::String("Bookmark Corp".into()), PyRep::String("BMCP".into()), PyRep::Real(0.05)], &kw, None).await.unwrap();
    let corp_id = sess1.corporation_id.unwrap();
    let app = ctx.dispatch_rpc(&mut sess2, "corpRegistry", None, "InsertApplication", &[PyRep::Integer(corp_id), PyRep::String("Joining".into())], &kw, None).await.unwrap();
    ctx.dispatch_rpc(&mut sess1, "corpRegistry", None, "UpdateApplicationOffer", &[app.dict_get("applicationID").unwrap().clone(), PyRep::String("Accepted".into())], &kw, None).await.unwrap();
    sess2.corporation_id = Some(corp_id);
    sess2.station_id = None;
    ctx.db.with_mut_snapshot(|snap| { if let Some(ch) = snap.characters.get_mut(&90_000_002) { ch.station_id = None; } });

    let shared_f = ctx.dispatch_rpc(&mut sess1, "accessGroupBookmarkMgr", None, "AddFolder", &[PyRep::String("Corp Shared".into()), PyRep::Bool(true)], &kw, None).await.unwrap();
    let shared_folder_id = shared_f.dict_get("folderID").and_then(|v| v.as_i64()).unwrap();

    // Create temporary bookmark with 3h expiry (`expiry_hours = 3`) in shared corp folder (`feat:bookmarks.corp-bookmarks`, `feat:bookmarks.bm-expiry`)
    let bm_target_pos = DVec3::new(500_000_000.0, 0.0, 0.0);
    let _ = jita_sys.set_ball_position(800_000_001, bm_target_pos).await;
    let _ = ctx.drain_notifications_for_char(90_000_002);
    let temp_bm = ctx.dispatch_rpc(&mut sess1, "bookmark", None, "BookmarkLocation", &[PyRep::String("Temp Safe".into()), PyRep::Integer(800_000_001), PyRep::String("3h safe".into()), PyRep::Integer(shared_folder_id), PyRep::None, PyRep::Integer(3)], &kw, None).await.unwrap();
    let temp_bm_id = temp_bm.dict_get("bookmarkID").and_then(|v| v.as_i64()).unwrap();
    assert!(ctx.drain_notifications_for_char(90_000_002).iter().any(|n| n.event == "OnSharedBookmarksFolderUpdated"), "Corp member must receive OnSharedBookmarksFolderUpdated on shared bookmark creation");

    // Copy and Move bookmarks across folders (`feat:bookmarks.copy-move`) including >20 batch limit check
    let p_folder = ctx.dispatch_rpc(&mut sess1, "accessGroupBookmarkMgr", None, "AddFolder", &[PyRep::String("Personal Tactical".into())], &kw, None).await.unwrap();
    let p_folder_id = p_folder.dict_get("folderID").and_then(|v| v.as_i64()).unwrap();
    let batch21: Vec<PyRep> = (0..21).map(|_| PyRep::Integer(temp_bm_id)).collect();
    assert_eq!(ctx.dispatch_rpc(&mut sess1, "accessGroupBookmarkMgr", None, "CopyBookmarksToFolderAndSubfolder", &[PyRep::Integer(p_folder_id), PyRep::None, PyRep::List(batch21), PyRep::None], &kw, None).await.unwrap_err().code, "BookmarkCopyBatchLimitExceeded");
    let copied = ctx.dispatch_rpc(&mut sess1, "accessGroupBookmarkMgr", None, "CopyBookmarksToFolderAndSubfolder", &[PyRep::Integer(p_folder_id), PyRep::None, PyRep::List(vec![PyRep::Integer(temp_bm_id)]), PyRep::None], &kw, None).await.unwrap();
    let copied_bm_id = match &copied {
        PyRep::Tuple(parts) => match &parts[0] {
            PyRep::Dict(entries) => entries.first().and_then(|(k, _)| k.as_i64()).unwrap(),
            _ => panic!("expected dict in CopyBookmarksToFolderAndSubfolder[0]"),
        },
        PyRep::KeyVal(_) => match copied.dict_get("bookmarkIDs") {
            Some(PyRep::List(l)) => l[0].as_i64().unwrap(),
            _ => panic!(),
        },
        _ => panic!("unexpected CopyBookmarksToFolderAndSubfolder shape"),
    };
    // While copied_bm_id is in Pilot 1's personal folder, Pilot 2 cannot warp to it
    let _ = jita_sys.spawn_ball(Ball { id: 800_000_002, type_id: 587, position: DVec3::ZERO, velocity: DVec3::ZERO, max_velocity: 350.0, inertia: 2.0, mass: 1_000_000.0, radius: 40.0, speed_fraction: 0.0, mode: MovementMode::Stop }).await;
    // rpc-lint: allow error code check
    assert_eq!(ctx.dispatch_rpc(&mut sess2, "beyonce", None, "CmdWarpToStuff", &[PyRep::String("bookmark".into()), PyRep::Integer(copied_bm_id), PyRep::Real(0.0)], &kw, None).await.unwrap_err().code, "BookmarkNotFound");

    ctx.dispatch_rpc(&mut sess1, "accessGroupBookmarkMgr", None, "MoveBookmarksToFolderAndSubfolder", &[PyRep::Integer(shared_folder_id), PyRep::None, PyRep::List(vec![PyRep::Integer(copied_bm_id)]), PyRep::None], &kw, None).await.unwrap();

    // Warp to Bookmark (`feat:bookmarks.warp-to-bm`): Pilot 2 warps to the shared corp bookmark (`copied_bm_id`) and lands at `bm_target_pos`
    ctx.dispatch_rpc(&mut sess2, "beyonce", None, "CmdWarpToStuff", &[PyRep::String("bookmark".into()), PyRep::Integer(copied_bm_id), PyRep::Real(0.0)], &kw, None).await.unwrap();
    for _ in 0..30 {
        let _ = jita_sys.step_manual(1.0).await;
    }
    let (_, b2_bm_landed) = jita_sys.get_ball_snapshot(800_000_002).await.unwrap().unwrap();
    assert!(b2_bm_landed.position.distance(bm_target_pos) < 2_500.0, "CmdWarpToStuff('bookmark') must land at bookmark coordinates");

    // Advance virtual clock 4 hours -> temporary bookmark (3h expiry) is pruned from `GetBookmarks` and warping to it fails with `BookmarkExpired` (`feat:bookmarks.bm-expiry`)
    ctx.advance_secs(4 * 3600);
    let bms_after_exp = ctx.dispatch_rpc(&mut sess1, "bookmark", None, "GetBookmarks", &[], &kw, None).await.unwrap();
    let PyRep::Rowset { rows: bm_rows, .. } = &bms_after_exp else { panic!() };
    assert!(!bm_rows.iter().any(|r| r[0] == PyRep::Integer(temp_bm_id)), "Expired 3h bookmark must be filtered out");
    let warp_stuff_kind = "bookmark";
    assert_eq!(ctx.dispatch_rpc(&mut sess1, "beyonce", None, "CmdWarpToStuff", &[PyRep::String(warp_stuff_kind.into()), PyRep::Integer(temp_bm_id), PyRep::Real(0.0)], &kw, None).await.unwrap_err().code, "BookmarkExpired");

    // 3. Scanning: Anomalies, Structure Signals, System Scan, Probe Formations, Scan Skills & Signature Despawn
    let _ = ctx.dispatch_rpc(&mut sess1, "scanMgr", None, "GetSignatures", &[PyRep::Integer(30_000_142)], &kw, None).await.unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(990_000_777, eve_db::UpwellStructureRecord {
            structure_id: 990_000_777, type_id: 35832, owner_corp_id: corp_id, solar_system_id: 30_000_142,
            name: "Jita Trade Citadel".into(), state: "ShieldVulnerable".into(), position: [100_000.0, 0.0, 0.0],
            quantum_core_installed: true, fuel_blocks: 1_000, shield_hp: 5_000.0, armor_hp: 5_000.0, hull_hp: 5_000.0,
            reinforced_until_unix: 0, last_damage_tick: 0, last_damage_unix: 0, self_destruct_at_unix: None,
        });
        snap.items.insert(880_201, eve_db::InventoryItemRecord {
            item_id: 880_201, type_id: 35_832, owner_id: 90_000_001, location_id: 800_000_001,
            flag: eve_db::InventoryFlag::Cargo, quantity: 1, singleton: false, version: 1, ..Default::default()
        });
    });
    let _ = ctx.drain_notifications_for_char(90_000_001);
    let tracker_rep = ctx.dispatch_rpc(&mut sess1, "scanMgr", None, "SignalTrackerRegister", &[PyRep::Integer(30_000_142)], &kw, None).await.unwrap();
    let PyRep::KeyVal(tr_kv) = &tracker_rep else { panic!() };
    let PyRep::Rowset { rows: struct_rows, .. } = &tr_kv["structures"] else { panic!() };
    assert!(struct_rows.iter().any(|r| r[0] == PyRep::Integer(990_000_777)), "SignalTrackerRegister must return structure signals");
    assert!(!ctx.drain_notifications_for_char(90_000_001).iter().any(|n| matches!(n.event.as_str(), "OnSignalTrackerSignatureUpdate" | "OnSignalTrackerAnomalyUpdate" | "OnSignalTrackerStructureUpdate")), "SignalTrackerRegister must not emit empty initial spam");

    // Trigger real anomaly, structure, and expired-signature deltas (`feat:scanning.anomalies`, `feat:scanning.structure-signals`, `feat:scanning.sig-despawn`)
    eve_server::services::pve_exploration_seeding::ensure_system_pve_sites_seeded(&ctx, 30_000_142).await;
    let _ = jita_sys.set_ball_position(800_000_001, DVec3::new(0.0, 5.0e9, 0.0)).await;
    let anchor_rep = ctx.dispatch_rpc(&mut sess1, "structureDeployment", None, "Anchor", &[PyRep::Integer(35_832), PyRep::String("Jita Scanner Citadel".into())], &kw, None).await.unwrap();
    let anchored_id = anchor_rep.dict_get("structureID").and_then(|v| v.as_i64()).unwrap();
    let expired_sig_id = ctx.db.with_mut_snapshot(|snap| {
        let relic = snap.cosmic_signatures.get_mut(&30_000_142).unwrap().iter_mut().find(|s| s.sig_group != "Wormhole").unwrap();
        relic.created_unix = 1;
        relic.lifetime_seconds = 1;
        relic.signature_id.clone()
    });
    ctx.advance_secs(10);
    let sigs_after = ctx.dispatch_rpc(&mut sess1, "scanMgr", None, "GetSignatures", &[PyRep::Integer(30_000_142)], &kw, None).await.unwrap();
    if let PyRep::Rowset { rows: s_rows, .. } = &sigs_after {
        assert!(!s_rows.iter().any(|r| r[0] == PyRep::String(expired_sig_id.clone())), "Expired signature must despawn from GetSignatures");
    }
    let tr_notifs = ctx.drain_notifications_for_char(90_000_001);
    assert!(tr_notifs.iter().any(|n| n.event == "OnSignalTrackerSignatureUpdate" && n.args.len() == 3 && n.args[0] == PyRep::Integer(30_000_142) && matches!(&n.args[2], PyRep::List(rem) if rem.contains(&PyRep::String(expired_sig_id.clone())))));
    assert!(tr_notifs.iter().any(|n| n.event == "OnSignalTrackerAnomalyUpdate" && n.args.len() == 3 && n.args[0] == PyRep::Integer(30_000_142) && matches!(&n.args[1], PyRep::Dict(d) if !d.is_empty()) && n.args[2] == PyRep::List(vec![])));
    assert!(tr_notifs.iter().any(|n| n.event == "OnSignalTrackerStructureUpdate" && n.args.len() == 3 && n.args[0] == PyRep::Integer(30_000_142) && matches!(&n.args[1], PyRep::Dict(d) if d.iter().any(|(k, _)| k == &PyRep::Integer(anchored_id))) && n.args[2] == PyRep::List(vec![])));

    // Probe Formations & Scan Skills (`feat:scanning.probe-formations`, `feat:scanning.scan-skills`)
    ctx.db.with_mut_snapshot(|snap| {
        let sk = snap.skills.entry(90_000_001).or_default();
        sk.insert(3412, eve_db::TrainedSkillRecord { type_id: 3412, level: 2, skill_points: 1_415.0 });
        sk.insert(25810, eve_db::TrainedSkillRecord { type_id: 25810, level: 4, skill_points: 90_510.0 });
        sk.insert(25811, eve_db::TrainedSkillRecord { type_id: 25811, level: 4, skill_points: 90_510.0 });
        sk.insert(25812, eve_db::TrainedSkillRecord { type_id: 25812, level: 4, skill_points: 90_510.0 });
    });
    assert_eq!(ctx.dispatch_rpc(&mut sess1, "scanMgr", None, "LaunchProbes", &[PyRep::Integer(8), PyRep::Integer(0)], &kw, None).await.unwrap_err().code, "MaxProbesExceeded");
    let lp_spread = ctx.dispatch_rpc(&mut sess1, "scanMgr", None, "LaunchProbes", &[PyRep::Integer(6), PyRep::Integer(1), PyRep::List(vec![PyRep::Real(1_000.0), PyRep::Real(2_000.0), PyRep::Real(3_000.0)])], &kw, None).await.unwrap();
    assert_eq!(lp_spread.dict_get("formationName"), Some(&PyRep::String("Spread".into())));
    assert_eq!(lp_spread.dict_get("maxProbesAllowed"), Some(&PyRep::Integer(6)));
    let lp_custom = ctx.dispatch_rpc(&mut sess1, "scanMgr", None, "LaunchProbes", &[PyRep::Integer(2), PyRep::Integer(2), PyRep::List(vec![PyRep::Real(0.0), PyRep::Real(0.0), PyRep::Real(0.0)]), PyRep::List(vec![PyRep::List(vec![PyRep::Real(100.0), PyRep::Real(0.0), PyRep::Real(0.0)]), PyRep::List(vec![PyRep::Real(-100.0), PyRep::Real(0.0), PyRep::Real(0.0)])])], &kw, None).await.unwrap();
    assert_eq!(lp_custom.dict_get("formationName"), Some(&PyRep::String("Custom".into())));
}

#[tokio::test]
async fn test_so_social_corp_mail_mailing_lists_cspa_calendar_and_chat_features() {
    // parity: feat:social.mail-corp-alliance, feat:social.mailing-lists, feat:social.mailing-list-admin, feat:social.mailing-list-welcome, feat:social.cspa, feat:social.calendar-personal, feat:social.calendar-corp-alliance, feat:social.calendar-responses, feat:social.calendar-ccp-events, feat:social.calendar-auto-events, feat:social.chat-help-rookie, feat:social.chat-gm-mute, feat:social.chat-motd, rpc:XmppChatMgr.GMMute, rpc:XmppChatMgr.GMUnmute, rpc:XmppChatMgr.GetHelpChannel, rpc:XmppChatMgr.GetRookieChannel, rpc:XmppChatMgr.SetMotd, rpc:calendarMgr.CreateCorporationEvent, rpc:calendarMgr.CreatePersonalEvent, rpc:calendarMgr.DeleteEvent, rpc:calendarMgr.EditCorporationEvent, rpc:calendarMgr.EditPersonalEvent, rpc:calendarMgr.GetResponsesForCharacter, rpc:calendarMgr.GetResponsesToEvent, rpc:calendarMgr.SendEventResponse, rpc:charMgr.GetSettingsInfo, rpc:charMgr.LogSettings, rpc:charUnboundMgr.SelectCharacterID, rpc:corpRegistry.AddCorporation, rpc:corpRegistry.InsertApplication, rpc:corpRegistry.UpdateApplicationOffer, rpc:mailingListsMgr.ClearEntityAccess, rpc:mailingListsMgr.ClearWelcomeMail, rpc:mailingListsMgr.Create, rpc:mailingListsMgr.GetJoinedLists, rpc:mailingListsMgr.GetMembers, rpc:mailingListsMgr.GetWelcomeMail, rpc:mailingListsMgr.Join, rpc:mailingListsMgr.KickMembers, rpc:mailingListsMgr.SaveWelcomeMail, rpc:mailingListsMgr.SetMembersMuted, rpc:mailingListsMgr.SetMembersOperator
    eve_testkit::attest(&["feat:social.mail-corp-alliance", "feat:social.mailing-lists", "feat:social.mailing-list-admin", "feat:social.mailing-list-welcome", "feat:social.cspa", "feat:social.calendar-personal", "feat:social.calendar-corp-alliance", "feat:social.calendar-responses", "feat:social.calendar-ccp-events", "feat:social.calendar-auto-events", "feat:social.chat-help-rookie", "feat:social.chat-gm-mute", "feat:social.chat-motd"]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();
    let mut sess1 = SessionState::new(821);
    common::login_pilot(&ctx, &mut sess1, "demo", "demo-secret").await;
    ctx.dispatch_rpc(&mut sess1, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(90_000_001)], &kw, None).await.unwrap();
    let mut sess2 = SessionState::new(822);
    common::login_pilot(&ctx, &mut sess2, "other", "other-secret").await;
    ctx.dispatch_rpc(&mut sess2, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(90_000_002)], &kw, None).await.unwrap();
    let p3_uid = ctx.db.provision_player_account("social_pilot3", "sp3-secret");
    let p3_char = ctx.db.create_character(p3_uid, "Social Pilot Three", 1, 1, 11, 17, &ctx.sde).unwrap().character_id;
    let mut sess3 = SessionState::new(823);
    common::login_pilot(&ctx, &mut sess3, "social_pilot3", "sp3-secret").await;
    ctx.dispatch_rpc(&mut sess3, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(p3_char)], &kw, None).await.unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(90_000_001).or_default().insert(3363, eve_db::TrainedSkillRecord { type_id: 3363, level: 5, skill_points: 256_000.0 });
    });
    ctx.advance_secs(12);
    ctx.dispatch_rpc(&mut sess1, "corpRegistry", None, "AddCorporation", &[PyRep::String("Social Corp".into()), PyRep::String("SOCP".into()), PyRep::Real(0.05)], &kw, None).await.unwrap();
    let corp_id = sess1.corporation_id.unwrap();
    let app = ctx.dispatch_rpc(&mut sess2, "corpRegistry", None, "InsertApplication", &[PyRep::Integer(corp_id), PyRep::String("Joining".into())], &kw, None).await.unwrap();
    ctx.dispatch_rpc(&mut sess1, "corpRegistry", None, "UpdateApplicationOffer", &[app.dict_get("applicationID").unwrap().clone(), PyRep::String("Accepted".into())], &kw, None).await.unwrap();
    sess2.corporation_id = Some(corp_id);
    sess2.station_id = None;
    ctx.db.with_mut_snapshot(|snap| { if let Some(ch) = snap.characters.get_mut(&90_000_002) { ch.station_id = None; } });

    // 4. Social: Corp/Alliance Mail, Mailing List Admin & Welcome Mail, CSPA, Calendar Personal/Corp/CCP/Auto Events, Chat Mute, Rookie Help & MOTD
    let _ = ctx.drain_notifications_for_char(90_000_002);
    let corp_mail_id = ctx.dispatch_rpc(&mut sess1, "mailMgr", None, "SendMail", &[PyRep::List(vec![]), PyRep::Integer(0), PyRep::Integer(corp_id), PyRep::String("Corp CTA".into()), PyRep::String("Form up in Jita".into())], &kw, None).await.unwrap();
    assert!(corp_mail_id.as_i64().is_some());
    assert!(ctx.drain_notifications_for_char(90_000_002).iter().any(|n| n.event == "OnMailSent"), "Corp mail must reach all corp members");

    // CSPA (`feat:social.cspa`)
    ctx.dispatch_rpc(&mut sess3, "charMgr", None, "LogSettings", &[PyRep::Real(50_000.0)], &kw, None).await.unwrap();
    assert_eq!(ctx.dispatch_rpc(&mut sess3, "charMgr", None, "GetSettingsInfo", &[], &kw, None).await.unwrap(), eve_server::services::client_login::settings_info_blob());
    ctx.db.with_mut_snapshot(|snap| snap.characters.get_mut(&90_000_002).unwrap().balance = eve_db::IskCents::from_isk(100));
    assert_eq!(ctx.dispatch_rpc(&mut sess2, "charMgr", None, "CSPAChargedAction", &[PyRep::Integer(p3_char)], &kw, None).await.unwrap_err().code, "InsufficientFundsForCSPA");
    assert_eq!(ctx.dispatch_rpc(&mut sess2, "mailMgr", None, "SendMail", &[PyRep::Integer(p3_char), PyRep::String("Hi".into()), PyRep::String("Body".into())], &kw, None).await.unwrap_err().code, "InsufficientFundsForCSPA");
    ctx.db.with_mut_snapshot(|snap| snap.characters.get_mut(&90_000_002).unwrap().balance = eve_db::IskCents::from_isk(100_000));
    ctx.dispatch_rpc(&mut sess2, "mailMgr", None, "SendMail", &[PyRep::Integer(p3_char), PyRep::String("Paid CSPA".into()), PyRep::String("Body".into())], &kw, None).await.unwrap();
    assert_eq!(ctx.db.with_snapshot(|s| s.characters[&90_000_002].balance.to_wire_f64()), 50_000.0);

    // Mailing Lists, Admin & Welcome Mail (`feat:social.mailing-lists`, `feat:social.mailing-list-admin`, `feat:social.mailing-list-welcome`)
    let ml_rep = ctx.dispatch_rpc(&mut sess1, "mailingListsMgr", None, "Create", &[PyRep::String("IntelList".into())], &kw, None).await.unwrap();
    let list_id = ml_rep.as_i64().or_else(|| ml_rep.dict_get("listID").and_then(|v| v.as_i64())).unwrap();
    ctx.dispatch_rpc(&mut sess1, "mailingListsMgr", None, "SaveWelcomeMail", &[PyRep::Integer(list_id), PyRep::String("Welcome to IntelList".into()), PyRep::String("Rules inside".into())], &kw, None).await.unwrap();
    let wm_rep = ctx.dispatch_rpc(&mut sess1, "mailingListsMgr", None, "GetWelcomeMail", &[PyRep::Integer(list_id)], &kw, None).await.unwrap();
    let PyRep::List(ref wm_list) = wm_rep else { panic!("expected List from GetWelcomeMail") };
    assert_eq!(wm_list[0].dict_get("title"), Some(&PyRep::String("Welcome to IntelList".into())));
    let _ = ctx.drain_notifications_for_char(90_000_002);
    ctx.dispatch_rpc(&mut sess2, "mailingListsMgr", None, "Join", &[PyRep::Integer(list_id)], &kw, None).await.unwrap();
    assert!(ctx.drain_notifications_for_char(90_000_002).iter().any(|n| n.event == "OnMailSent"), "Joining mailing list must deliver welcome mail");
    assert_eq!(ctx.dispatch_rpc(&mut sess2, "mailingListsMgr", None, "KickMembers", &[PyRep::Integer(list_id), PyRep::List(vec![PyRep::Integer(90_000_001)])], &kw, None).await.unwrap_err().code, "UserError");
    ctx.dispatch_rpc(&mut sess1, "mailingListsMgr", None, "SetMembersOperator", &[PyRep::Integer(list_id), PyRep::List(vec![PyRep::Integer(90_000_002)])], &kw, None).await.unwrap();
    ctx.dispatch_rpc(&mut sess1, "mailingListsMgr", None, "SetMembersMuted", &[PyRep::Integer(list_id), PyRep::List(vec![PyRep::Integer(90_000_002)])], &kw, None).await.unwrap();
    ctx.dispatch_rpc(&mut sess1, "mailingListsMgr", None, "SetEntityAccess", &[PyRep::Integer(list_id), PyRep::Integer(90_000_002), PyRep::Integer(2)], &kw, None).await.unwrap();
    ctx.dispatch_rpc(&mut sess1, "mailingListsMgr", None, "ClearEntityAccess", &[PyRep::Integer(list_id), PyRep::Integer(90_000_002)], &kw, None).await.unwrap();
    ctx.dispatch_rpc(&mut sess1, "mailingListsMgr", None, "ClearWelcomeMail", &[PyRep::Integer(list_id)], &kw, None).await.unwrap();
    assert!(matches!(ctx.dispatch_rpc(&mut sess1, "mailingListsMgr", None, "GetJoinedLists", &[], &kw, None).await.unwrap(), PyRep::Dict(_)));
    assert!(matches!(ctx.dispatch_rpc(&mut sess1, "mailingListsMgr", None, "GetMembers", &[PyRep::Integer(list_id)], &kw, None).await.unwrap(), PyRep::Dict(_)));

    // Calendar: Personal Event, role-gated Corporation Event, Responses, CCP Event & Automated Event (`feat:social.calendar-*`)
    let pers_ev = ctx.dispatch_rpc(&mut sess1, "calendarMgr", None, "CreatePersonalEvent", &[PyRep::Integer(135_500_000_000_000_000), PyRep::Integer(60), PyRep::String("Solo Roam".into()), PyRep::String("Bring nanogang".into()), PyRep::Integer(0), PyRep::List(vec![PyRep::Integer(90_000_002)])], &kw, None).await.unwrap();
    let pers_ev_id = pers_ev.dict_get("eventID").and_then(|v| v.as_i64()).unwrap();
    ctx.dispatch_rpc(&mut sess1, "calendarMgr", None, "EditPersonalEvent", &[PyRep::Integer(pers_ev_id), PyRep::Integer(135_500_000_000_000_000), PyRep::Integer(90), PyRep::String("Updated Solo Roam".into()), PyRep::String("Updated desc".into())], &kw, None).await.unwrap();
    ctx.dispatch_rpc(&mut sess2, "calendarMgr", None, "SendEventResponse", &[PyRep::Integer(pers_ev_id), PyRep::Integer(90_000_001), PyRep::Integer(4)], &kw, None).await.unwrap();
    assert!(is_rowset(&ctx.dispatch_rpc(&mut sess1, "calendarMgr", None, "GetResponsesToEvent", &[PyRep::Integer(pers_ev_id)], &kw, None).await.unwrap()));
    assert!(is_rowset(&ctx.dispatch_rpc(&mut sess2, "calendarMgr", None, "GetResponsesForCharacter", &[], &kw, None).await.unwrap()));
    assert_eq!(ctx.dispatch_rpc(&mut sess2, "calendarMgr", None, "CreateCorporationEvent", &[PyRep::Integer(135_500_000_000_000_000), PyRep::Integer(120), PyRep::String("Unauth".into()), PyRep::String("No role".into()), PyRep::Integer(1)], &kw, None).await.unwrap_err().code, "CalendarAccessDenied");
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.get_mut(&corp_id).unwrap().member_roles.insert(90_000_002, 1u64 << 55);
    });
    let corp_ev = ctx.dispatch_rpc(&mut sess2, "calendarMgr", None, "CreateCorporationEvent", &[PyRep::Integer(135_500_000_000_000_000), PyRep::Integer(120), PyRep::String("Corp Moon Mining".into()), PyRep::String("Bring barges".into()), PyRep::Integer(1)], &kw, None).await.unwrap();
    let corp_ev_id = corp_ev.dict_get("eventID").and_then(|v| v.as_i64()).unwrap();
    ctx.dispatch_rpc(&mut sess2, "calendarMgr", None, "EditCorporationEvent", &[PyRep::Integer(corp_ev_id), PyRep::Integer(135_500_000_000_000_000), PyRep::Integer(120), PyRep::String("Updated Corp Moon Mining".into()), PyRep::String("Updated text".into())], &kw, None).await.unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(990_000_888, eve_db::UpwellStructureRecord {
            structure_id: 990_000_888, type_id: 35835, name: "Social Corp Refinery".into(), owner_corp_id: corp_id,
            solar_system_id: 30_000_142, position: [100_000.0, 0.0, 0.0], state: "ShieldVulnerable".into(),
            quantum_core_installed: true, fuel_blocks: 1_000, shield_hp: 5_000.0, armor_hp: 5_000.0, hull_hp: 5_000.0,
            reinforced_until_unix: 0, last_damage_tick: 0, last_damage_unix: 0, self_destruct_at_unix: None,
        });
    });
    let ext_rep = ctx.dispatch_rpc(&mut sess1, "moonExtractions", None, "StartNewExtraction", &[PyRep::Integer(990_000_888), PyRep::Integer(518_400)], &kw, None).await.expect("StartNewExtraction");
    let auto_ev_id = ext_rep.dict_get("eventID").and_then(|v| v.as_i64()).unwrap();
    let arrival_unix = ext_rep.dict_get("chunkArrivalTime").and_then(|v| v.as_i64()).unwrap();
    let arrival_ft = (arrival_unix + 11_644_473_600) * 10_000_000;
    let moon_id = ext_rep.dict_get("moonID").and_then(|v| v.as_i64()).unwrap();
    let expected_moon_name = ctx.sde.celestials_in_system(30_000_142).into_iter().find(|c| c.celestial_id == moon_id).map(|c| c.name).unwrap();
    assert_eq!(ctx.dispatch_rpc(&mut sess2, "slash", None, "SlashCmd", &[PyRep::String(format!("/createccpevent {arrival_ft} 1440 2 CCP Crimson Harvest | Global event"))], &kw, None).await.unwrap_err().code, "InsufficientRole");
    let prev_role = sess1.role;
    sess1.role |= eve_machonet::service_roles::ROLE_GMH;
    let ccp_rep = ctx.dispatch_rpc(&mut sess1, "slash", None, "SlashCmd", &[PyRep::String(format!("/createccpevent {arrival_ft} 1440 2 CCP Crimson Harvest | Global event"))], &kw, None).await.expect("slash /createccpevent");
    sess1.role = prev_role;
    let ccp_ev_id = ccp_rep.dict_get("eventID").and_then(|v| v.as_i64()).unwrap();
    let ev_list = ctx.dispatch_rpc(&mut sess1, "calendarProxy", None, "GetEventList", &[PyRep::Integer(1), PyRep::Integer(2027)], &kw, None).await.unwrap();
    let PyRep::List(ev_rowsets) = &ev_list else { panic!("expected List of rowsets from calendarProxy.GetEventList") };
    let PyRep::Rowset { rows: ev_rows, .. } = &ev_rowsets[0] else { panic!() };
    assert!(ev_rows.iter().any(|r| r[0] == PyRep::Integer(ccp_ev_id) && r[1] == PyRep::Integer(1) && r[2] == PyRep::Integer(arrival_ft) && r[3] == PyRep::Integer(1440) && r[5] == PyRep::String("CCP Crimson Harvest".into()) && r[6] == PyRep::Integer(0)), "calendarProxy.GetEventList must include GM-created global CCP event");
    assert!(ev_rows.iter().any(|r| r[0] == PyRep::Integer(auto_ev_id) && r[1] == PyRep::Integer(corp_id) && r[2] == PyRep::Integer(arrival_ft) && r[3] == PyRep::Integer(2880) && r[8] == PyRep::Bool(true) && r[5].as_str().is_some_and(|t| t.contains(&expected_moon_name))), "calendarProxy.GetEventList must include automated moon extraction event with SDE moon name");
    ctx.dispatch_rpc(&mut sess1, "calendarMgr", None, "DeleteEvent", &[PyRep::Integer(pers_ev_id)], &kw, None).await.unwrap();

    // Chat: Rookie Help, MOTD & GM Mute enforcement (`feat:social.chat-help-rookie`, `feat:social.chat-motd`, `feat:social.chat-gm-mute`)
    let now_u = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&90_000_001).unwrap().create_date_unix = now_u - 5 * 86_400;
        snap.characters.get_mut(&90_000_002).unwrap().create_date_unix = now_u - 45 * 86_400;
    });
    assert_eq!(ctx.dispatch_rpc(&mut sess1, "XmppChatMgr", None, "GetRookieChannel", &[], &kw, None).await.unwrap(), PyRep::String("rookie_help".into()));
    assert_eq!(ctx.dispatch_rpc(&mut sess2, "XmppChatMgr", None, "GetRookieChannel", &[], &kw, None).await.unwrap(), PyRep::None);
    assert_eq!(ctx.dispatch_rpc(&mut sess1, "XmppChatMgr", None, "GetHelpChannel", &[], &kw, None).await.unwrap(), PyRep::String("help_en".into()));
    assert_eq!(ctx.dispatch_rpc(&mut sess1, "XmppChatMgr", None, "GetHelpChannel", &[PyRep::String("de".into())], &kw, None).await.unwrap(), PyRep::String("help_de".into()));

    ctx.dispatch_rpc(&mut sess1, "LSC", None, "JoinChannel", &[PyRep::String("channel:moderation_test".into())], &kw, None).await.unwrap();
    ctx.dispatch_rpc(&mut sess2, "LSC", None, "JoinChannel", &[PyRep::String("channel:moderation_test".into())], &kw, None).await.unwrap();
    let set_motd = ctx.dispatch_rpc(&mut sess1, "XmppChatMgr", None, "SetMotd", &[PyRep::String("channel:moderation_test".into()), PyRep::String("Be polite in chat".into())], &kw, None).await.unwrap();
    assert_eq!(set_motd.dict_get("motd"), Some(&PyRep::String("Be polite in chat".into())));

    assert_eq!(ctx.dispatch_rpc(&mut sess2, "XmppChatMgr", None, "GMMute", &[PyRep::Integer(90_000_001), PyRep::String("channel:moderation_test".into()), PyRep::Integer(600), PyRep::String("Noop".into())], &kw, None).await.unwrap_err().code, "ChatAccessDenied");
    ctx.dispatch_rpc(&mut sess1, "XmppChatMgr", None, "GMMute", &[PyRep::Integer(90_000_002), PyRep::String("channel:moderation_test".into()), PyRep::Integer(600), PyRep::String("Spamming".into())], &kw, None).await.unwrap();
    assert_eq!(ctx.dispatch_rpc(&mut sess2, "XmppChatMgr", None, "GMUnmute", &[PyRep::Integer(90_000_002), PyRep::String("channel:moderation_test".into())], &kw, None).await.unwrap_err().code, "ChatAccessDenied");
    let muted_err = ctx.dispatch_rpc(&mut sess2, "LSC", None, "SendMessage", &[PyRep::String("channel:moderation_test".into()), PyRep::String("Can anyone hear me?".into())], &kw, None).await.expect_err("Muted user must be blocked from sending chat messages");
    assert_eq!(muted_err.code, "MutedInChannel");
    ctx.dispatch_rpc(&mut sess1, "XmppChatMgr", None, "GMUnmute", &[PyRep::Integer(90_000_002), PyRep::String("channel:moderation_test".into())], &kw, None).await.unwrap();
    assert!(is_keyval(&ctx.dispatch_rpc(&mut sess2, "LSC", None, "SendMessage", &[PyRep::String("channel:moderation_test".into()), PyRep::String("Unmuted now!".into())], &kw, None).await.unwrap()));
}
