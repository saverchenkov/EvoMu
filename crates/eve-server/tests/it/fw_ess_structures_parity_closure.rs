//! `fw_ess_structures_parity_closure` — Live RPC integration tests for Faction Warfare
//! system capture, Pirate Insurgency Ice Heist, and Pirate Forward Operating Bases.

use crate::common::login_pilot;
use eve_db::{IskCents, UpwellStructureRecord};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::fw_gameplay::apply_fw_or_insurgency_structure_damage;
use eve_server::services::fw_services::{
    self, FACTION_AMARR, FACTION_MINMATAR, HQ_SYSTEM_AMO,
};
use eve_server::services::insurgency_ice_heist_fob::{
    destroy_pirate_fob, escort_ice_heist_tanker, spawn_ice_heist_checked,
    spawn_pirate_fob_in_system, step_ice_heist_thunderchild_guards,
    trigger_fob_defense_response, FACTION_BLOOD_RAIDERS, ICE_HEIST_RESPAWN_SECS,
    ICE_HEIST_WIN_LP, PIRATE_FOB_BOUNTY_ISK,
};
use glam::DVec3;
use std::collections::HashMap;
use std::sync::Arc;

#[tokio::test]
async fn test_fw_system_capture_ihub_siege_frontline_and_persistence() {
    // parity: feat:fw.system-capture
    let db_path = std::env::temp_dir().join(format!(
        "fw_capture_{}_{}.sqlite",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let ctx = Arc::new(ServerContext::new());
    let mut sess = SessionState::new(20_001);
    login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let char_id = 90_000_001;
    let kw = HashMap::new();

    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");

    // Pick a Minmatar-occupied warzone system
    let sys_id = ctx.db.with_snapshot(|snap| {
        snap.fw_system_occupations
            .iter()
            .find(|(&sid, o)| sid != HQ_SYSTEM_AMO && o.occupier_faction_id == FACTION_MINMATAR)
            .map(|(&sid, _)| sid)
            .expect("Minmatar warzone system")
    });
    let ihub_id = 1_890_000_000 + i64::from(sys_id);
    sess.solar_system_id = Some(sys_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.war_faction_id = Some(FACTION_AMARR);
        }
        snap.structures.insert(
            ihub_id,
            UpwellStructureRecord {
                structure_id: ihub_id,
                type_id: 32226,
                owner_corp_id: 1_000_182,
                solar_system_id: sys_id,
                name: "FW Infrastructure Hub".into(),
                state: "ShieldVulnerable".into(),
                position: [10_000.0, 0.0, 10_000.0],
                quantum_core_installed: true,
                fuel_blocks: 1_000,
                shield_hp: 5_000.0,
                armor_hp: 5_000.0,
                hull_hp: 8_000.0,
                reinforced_until_unix: 0,
                last_damage_unix: 0,
                last_damage_tick: 0,
                self_destruct_at_unix: None,
            },
        );
    });

    // Negative: attacking I-Hub before 100% contested fails with IHubInvulnerable
    let err = apply_fw_or_insurgency_structure_damage(&ctx, char_id, ihub_id, 20_000.0)
        .await
        .unwrap_err();
    assert_eq!(err.code, "IHubInvulnerable");

    // Drive system to 100% contested and mark I-Hub vulnerable
    let threshold = ctx.db.with_snapshot(|snap| {
        snap.fw_system_occupations
            .get(&sys_id)
            .map(|o| o.victory_points_threshold)
            .unwrap_or(10_000)
    });
    fw_services::add_victory_points(&ctx, sys_id, threshold);
    ctx.db.with_mut_snapshot(|snap| {
        snap.fw_runtime.ihub_vulnerable.insert(sys_id, true);
    });

    // Bash I-Hub to 0 HP to capture system for Amarr
    let cap_res = apply_fw_or_insurgency_structure_damage(&ctx, char_id, ihub_id, 20_000.0)
        .await
        .expect("capture I-Hub");
    assert_eq!(cap_res.dict_get("state"), Some(&PyRep::String("Captured".into())));
    assert_eq!(
        cap_res.dict_get("newOccupierFactionID"),
        Some(&PyRep::Integer(FACTION_AMARR))
    );

    // Verify GetLocalOccupationState via live RPC
    let occ_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "fwWarzoneSolarsystem",
            None,
            "GetLocalOccupationState",
            &[],
            &kw,
            None,
        )
        .await
        .expect("GetLocalOccupationState");
    let PyRep::Tuple(t) = occ_rep else {
        panic!("expected Tuple");
    };
    assert_eq!(
        t[1].dict_get("occupierFactionID"),
        Some(&PyRep::Integer(FACTION_AMARR))
    );
    assert_eq!(t[1].dict_get("contestedPercentage"), Some(&PyRep::Real(0.0)));

    // Verify SQLite persistence across reload
    ctx.db.save_to_sqlite(&db_path).expect("save_to_sqlite");
    let ctx2 = Arc::new(ServerContext::from_sqlite(&db_path).expect("reopen_sqlite"));
    ctx2.db.with_snapshot(|snap| {
        let occ = snap.fw_system_occupations.get(&sys_id).expect("occ");
        assert_eq!(occ.occupier_faction_id, FACTION_AMARR);
        assert_eq!(occ.victory_points, 0);
    });
    let _ = std::fs::remove_file(&db_path);
}

#[tokio::test]
async fn test_insurgency_ice_heist_spawn_escort_lp_and_respawn_persistence() {
    // parity: feat:pve.insurgency-ice-heist
    let db_path = std::env::temp_dir().join(format!(
        "ice_heist_{}_{}.sqlite",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let ctx = Arc::new(ServerContext::new());
    let mut sess = SessionState::new(20_002);
    login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let char_id = 90_000_001;
    let kw = HashMap::new();
    let sys_id = 30_002_055;
    sess.character_id = Some(char_id);
    sess.solar_system_id = Some(sys_id);

    // Negative: Corruption Stage 1 cannot spawn Ice Heist
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(st) = snap.system_states.get_mut(&sys_id) {
            st.insurgency_corruption_stage = 1;
        }
    });
    let err = spawn_ice_heist_checked(&ctx, sys_id).await.unwrap_err();
    assert_eq!(err.code, "InvalidCorruptionStage");

    // Set Corruption Stage 3 and spawn Ice Heist
    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states
            .entry(sys_id)
            .or_default()
            .insurgency_corruption_stage = 3;
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.war_faction_id = Some(FACTION_MINMATAR);
        }
    });
    let instance_id = spawn_ice_heist_checked(&ctx, sys_id)
        .await
        .expect("spawn_ice_heist_checked");

    // Verify Thunderchild guard destroys nearby pirate ship
    let pirate_ship_id = 999_111_222;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_002) {
            ch.active_ship_id = pirate_ship_id;
            ch.war_faction_id = Some(FACTION_BLOOD_RAIDERS);
        }
    });
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_handle
        .spawn_ball(Ball {
            id: pirate_ship_id,
            type_id: 587,
            position: DVec3::new(122_500.0, 0.0, 60_000.0),
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1.0e6,
            radius: 50.0,
            speed_fraction: 0.0,
            mode: MovementMode::Rigid,
        })
        .await;
    assert_eq!(step_ice_heist_thunderchild_guards(&ctx, sys_id).await, 1);

    // Escort 10 tankers for Empire -> wins 150,000 LP and sets 4h respawn
    let score_rep = escort_ice_heist_tanker(&ctx, char_id, instance_id, "empire", 10)
        .expect("escort_ice_heist_tanker");
    assert_eq!(score_rep.dict_get("completed"), Some(&PyRep::Bool(true)));
    assert_eq!(
        score_rep.dict_get("lpAwarded"),
        Some(&PyRep::Integer(ICE_HEIST_WIN_LP))
    );

    // Negative: respawning immediately fails with IceHeistRespawnCooldown
    let cd_err = spawn_ice_heist_checked(&ctx, sys_id).await.unwrap_err();
    assert_eq!(cd_err.code, "IceHeistRespawnCooldown");

    // Verify GetPirateInsurgencyIceHeistInstances and SQLite persistence
    let list_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "dungeonInstanceCacheMgr",
            None,
            "GetPirateInsurgencyIceHeistInstances",
            &[PyRep::Integer(i64::from(sys_id))],
            &kw,
            None,
        )
        .await
        .expect("GetPirateInsurgencyIceHeistInstances");
    let PyRep::List(items) = list_rep else {
        panic!("expected List");
    };
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].dict_get("completed"), Some(&PyRep::Bool(true)));
    let respawn_at = items[0]
        .dict_get("respawnAtUnix")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    assert!(respawn_at >= ICE_HEIST_RESPAWN_SECS as i64);

    ctx.db.save_to_sqlite(&db_path).expect("save_to_sqlite");
    let ctx2 = Arc::new(ServerContext::from_sqlite(&db_path).expect("reopen_sqlite"));
    ctx2.db.with_snapshot(|snap| {
        assert_eq!(snap.insurgency_runtime.ice_heist_instances.len(), 1);
        assert!(snap.insurgency_runtime.ice_heist_instances[0].completed);
    });
    let _ = std::fs::remove_file(&db_path);
}

#[tokio::test]
async fn test_pirate_fob_spawn_defense_scaling_loot_and_bounty() {
    // parity: feat:pve.pirate-fob-forward-operating-base
    let ctx = Arc::new(ServerContext::new());
    let mut sess = SessionState::new(20_003);
    login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let char_id = 90_000_001;
    sess.character_id = Some(char_id);

    // Negative: null-sec system rejected for Pirate FOB
    let null_err = spawn_pirate_fob_in_system(&ctx, 30_004_759, FACTION_BLOOD_RAIDERS)
        .await
        .unwrap_err();
    assert_eq!(null_err.code, "InvalidFobSystem");

    // Spawn Blood Raider FOB in high-sec Jita (30000142)
    let fob_id = spawn_pirate_fob_in_system(&ctx, 30_000_142, FACTION_BLOOD_RAIDERS)
        .await
        .expect("spawn_pirate_fob_in_system");

    // Trigger scaled defense response for 3-ship attacking fleet
    let defenders = trigger_fob_defense_response(&ctx, fob_id, &[587, 587, 587])
        .await
        .expect("trigger_fob_defense_response");
    assert_eq!(defenders.len(), 6);

    let bal_before = ctx
        .db
        .with_snapshot(|snap| snap.characters.get(&char_id).map(|c| c.balance.0).unwrap_or(0));

    // Destroy FOB -> drops wreck loot and credits 50M ISK bounty
    let (wreck_id, bounty_isk) = destroy_pirate_fob(&ctx, fob_id, char_id)
        .await
        .expect("destroy_pirate_fob");
    assert_eq!(bounty_isk, PIRATE_FOB_BOUNTY_ISK);

    ctx.db.with_snapshot(|snap| {
        let bal_after = snap.characters.get(&char_id).map(|c| c.balance.0).unwrap_or(0);
        assert_eq!(
            bal_after - bal_before,
            IskCents::from_isk(PIRATE_FOB_BOUNTY_ISK).0
        );
        assert!(snap
            .items
            .values()
            .any(|i| i.location_id == wreck_id && i.type_id == 56201));
    });
}
