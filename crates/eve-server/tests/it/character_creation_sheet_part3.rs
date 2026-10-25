//! Integration tests for Jump Clones (`S-4`).

use crate::common;
use eve_db::{InventoryFlag, InventoryItemRecord, TrainedSkillRecord};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::formulas::clone_jump_cooldown_seconds;
use eve_server::router::ServerContext;
use std::collections::HashMap;

#[tokio::test]
async fn test_s4_jump_clones_implants_and_cooldown() {
    // parity: plan:S-4, feat:clones.jump-clone-install, feat:clones.jump-clone-jump, feat:clones.jump-clone-destroy, rpc:charUnboundMgr.SelectCharacterID, rpc:jumpCloneSvc.CloneJump, rpc:jumpCloneSvc.DestroyInstalledClone, rpc:jumpCloneSvc.GetPriceForClone, rpc:jumpCloneSvc.InstallCloneInStation, rpc:jumpCloneSvc.InstallCloneInStructure
    eve_testkit::attest(&["plan:S-4", "feat:clones.jump-clone-install", "feat:clones.jump-clone-jump", "feat:clones.jump-clone-destroy"]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();
    let mut sess = SessionState::new(937);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;

    let char_id = 90_000_001;
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

    // 1. Verify jump clone install fee: 900,000 ISK
    let price_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "jumpCloneSvc",
            None,
            "GetPriceForClone",
            &[],
            &kw,
            None,
        )
        .await
        .expect("GetPriceForClone");
    assert_eq!(price_rep, PyRep::Real(900_000.0));

    // Untrained Infomorph Psychology (24242, 0 slots) rejected on InstallCloneInStation
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = Some(60_003_760);
        }
        if let Some(skills) = snap.skills.get_mut(&char_id) {
            skills.remove(&24242);
            skills.remove(&33407);
            skills.remove(&73910);
            skills.remove(&55025);
        }
    });
    let untrained_err = ctx
        .dispatch_rpc(
            &mut sess,
            "jumpCloneSvc",
            None,
            "InstallCloneInStation",
            &[],
            &kw,
            None,
        )
        .await;
    assert_eq!(
        untrained_err.unwrap_err().code,
        "InfomorphPsychologyRequired",
        "Installing a jump clone without Infomorph Psychology (24242) must fail"
    );

    // Station without clone bay rejected
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = Some(60_000_361); // Station without clone bay
        }
    });
    let no_bay_err = ctx
        .dispatch_rpc(
            &mut sess,
            "jumpCloneSvc",
            None,
            "InstallCloneInStation",
            &[],
            &kw,
            None,
        )
        .await;
    assert_eq!(no_bay_err.unwrap_err().code, "StationHasNoCloneBay");

    // Station with clone bay (Jita 4-4, 60003760, owner 1000035 Caldari Navy), Infomorph Psychology I (24242), and Infomorph Synchronizing I (33399)
    // Negative standing (< 0.0) with station owner corporation must be rejected with InsufficientStanding
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = Some(60_003_760);
            ch.standings.insert(1_000_035, -1.5);
        }
        let skills = snap.skills.entry(char_id).or_default();
        skills.insert(
            24242,
            TrainedSkillRecord {
                type_id: 24242,
                level: 1,
                skill_points: 250.0,
            },
        );
        skills.insert(
            33399,
            TrainedSkillRecord {
                type_id: 33399,
                level: 1,
                skill_points: 250.0,
            },
        );
    });
    let low_standing_err = ctx
        .dispatch_rpc(
            &mut sess,
            "jumpCloneSvc",
            None,
            "InstallCloneInStation",
            &[],
            &kw,
            None,
        )
        .await;
    assert_eq!(
        low_standing_err.unwrap_err().code,
        "InsufficientStanding",
        "NPC station jump clone install must enforce standing threshold with station owner"
    );

    // Restore standing with station owner to 0.0
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.standings.insert(1_000_035, 0.0);
        }
    });

    let bal_before = ctx.db.with_snapshot(|s| s.characters[&char_id].balance.to_wire_f64());

    // Install 1st jump clone in NPC station (feat:clones.jump-clone-install)
    let install_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "jumpCloneSvc",
            None,
            "InstallCloneInStation",
            &[],
            &kw,
            None,
        )
        .await
        .expect("InstallCloneInStation");
    let PyRep::KeyVal(install_kv) = install_rep else {
        panic!("Expected KeyVal from InstallCloneInStation");
    };
    let jc_id = install_kv.get("jumpCloneID").unwrap().as_i64().unwrap();

    // Verify 900,000 ISK fee deducted
    let bal_after = ctx.db.with_snapshot(|s| s.characters[&char_id].balance.to_wire_f64());
    assert_eq!(bal_before - bal_after, 900_000.0);

    // Dock at a 2nd NPC station with clone bay (Perimeter II - Moon 1 - Caldari Navy Assembly Plant, 60003754):
    // with Infomorph Psychology I (1 slot) already used, 2nd station install must fail with MaxJumpClonesReached
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = Some(60_003_754);
        }
    });
    sess.station_id = Some(60_003_754);
    let second_station_limit_err = ctx
        .dispatch_rpc(
            &mut sess,
            "jumpCloneSvc",
            None,
            "InstallCloneInStation",
            &[],
            &kw,
            None,
        )
        .await;
    assert_eq!(
        second_station_limit_err.unwrap_err().code,
        "MaxJumpClonesReached",
        "2nd station install with Infomorph Psychology I (1 slot) must fail with MaxJumpClonesReached"
    );

    // Train Infomorph Psychology (24242) to level 2 (2 slots) and verify 2nd station install succeeds
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(char_id).or_default().insert(
            24242,
            TrainedSkillRecord {
                type_id: 24242,
                level: 2,
                skill_points: 1415.0,
            },
        );
    });
    let second_station_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "jumpCloneSvc",
            None,
            "InstallCloneInStation",
            &[],
            &kw,
            None,
        )
        .await
        .expect("2nd InstallCloneInStation after training Infomorph Psychology II");
    let PyRep::KeyVal(second_station_kv) = second_station_rep else {
        panic!("Expected KeyVal from 2nd InstallCloneInStation");
    };
    assert_eq!(
        second_station_kv.get("locationID").and_then(|v| v.as_i64()),
        Some(60_003_754)
    );
    let bal_after_second = ctx.db.with_snapshot(|s| s.characters[&char_id].balance.to_wire_f64());
    assert_eq!(bal_after - bal_after_second, 900_000.0);

    // Verify MaxJumpClonesReached when Infomorph Psychology II (2 slots) is full on structure install
    let clone_struct_id = 1_030_000_099_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            clone_struct_id,
            eve_db::UpwellStructureRecord {
                structure_id: clone_struct_id,
                type_id: 35832,
                owner_corp_id: 1_000_001,
                solar_system_id: 30_000_142,
                name: "Jita Clone Bay Astrahus".into(),
                position: [1000.0, 2000.0, 3000.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 500,
                shield_hp: 5000.0,
                armor_hp: 5000.0,
                hull_hp: 5000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0, self_destruct_at_unix: None,
            },
        );
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.structure_id = Some(clone_struct_id);
        }
    });
    sess.station_id = None;
    sess.structure_id = Some(clone_struct_id);

    // Bare Astrahus lacking Standup Cloning Center I service module must fail with NoCloneBayService (feat:clones.jump-clone-install)
    let bare_struct_err = ctx
        .dispatch_rpc(
            &mut sess,
            "jumpCloneSvc",
            None,
            "InstallCloneInStructure",
            &[],
            &kw,
            None,
        )
        .await;
    assert_eq!(
        bare_struct_err.unwrap_err().code,
        "NoCloneBayService",
        "Bare Upwell structure lacking Standup Cloning Center module must be rejected with NoCloneBayService"
    );

    // Fit Standup Cloning Center I (type_id 35894) in ServiceSlot0 and set online
    let clone_bay_item_id = 100_000_999_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            clone_bay_item_id,
            InventoryItemRecord {
                item_id: clone_bay_item_id,
                type_id: 35894,
                owner_id: 1_000_001,
                location_id: clone_struct_id,
                flag: InventoryFlag::ServiceSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        snap.module_online_states.insert(clone_bay_item_id, true);
    });

    let max_clones_err = ctx
        .dispatch_rpc(
            &mut sess,
            "jumpCloneSvc",
            None,
            "InstallCloneInStructure",
            &[],
            &kw,
            None,
        )
        .await;
    assert_eq!(
        max_clones_err.unwrap_err().code,
        "MaxJumpClonesReached",
        "Must enforce max jump clone slot limit from Infomorph Psychology"
    );

    // Grant Advanced Infomorph Psychology I (33407) to unlock a 2nd jump clone slot, then install in Upwell structure
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(char_id).or_default().insert(
            33407,
            TrainedSkillRecord {
                type_id: 33407,
                level: 1,
                skill_points: 750.0,
            },
        );
    });
    let struct_install_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "jumpCloneSvc",
            None,
            "InstallCloneInStructure",
            &[],
            &kw,
            None,
        )
        .await
        .expect("InstallCloneInStructure with Advanced Infomorph Psychology I");
    let PyRep::KeyVal(struct_jc_kv) = struct_install_rep else {
        panic!("Expected KeyVal from InstallCloneInStructure");
    };
    assert_eq!(
        struct_jc_kv.get("locationID").and_then(|v| v.as_i64()),
        Some(clone_struct_id)
    );

    // Return pilot to Jita 4-4 (60003760) for clone jump test
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = Some(60_003_760);
            ch.structure_id = None;
        }
    });
    sess.station_id = Some(60_003_760);
    sess.structure_id = None;

    // 2. Clone Jump & Implant swap (feat:clones.jump-clone-jump)
    // Setup destination clone at another station (60008494) with implant 9941
    // Character active implant is 9899
    ctx.db.with_mut_snapshot(|snap| {
        snap.active_implants.insert(char_id, vec![9899]);
        if let Some(clones) = snap.jump_clones.get_mut(&char_id) {
            if let Some(c) = clones.iter_mut().find(|c| c.jump_clone_id == jc_id) {
                c.location_id = 60_008_494;
                c.implants = vec![9941];
            }
        }
    });

    let jump_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "jumpCloneSvc",
            None,
            "CloneJump",
            &[PyRep::Integer(jc_id)],
            &kw,
            None,
        )
        .await
        .expect("CloneJump");
    let PyRep::KeyVal(jump_kv) = jump_rep else {
        panic!("Expected KeyVal from CloneJump");
    };
    assert_eq!(
        jump_kv.get("locationID").unwrap().as_i64(),
        Some(60_008_494)
    );

    // Verify cooldown timer is 23h (24h - 1h * Infomorph Sync 1)
    let expected_cooldown = clone_jump_cooldown_seconds(1);
    assert_eq!(expected_cooldown, 23 * 3600);
    assert_eq!(
        jump_kv.get("cooldownSeconds").unwrap().as_i64(),
        Some(expected_cooldown as i64)
    );

    // Verify implant swap: active implants now [9941], left behind clone has [9899]
    ctx.db.with_snapshot(|snap| {
        let active = &snap.active_implants[&char_id];
        assert_eq!(active, &vec![9941], "Active implants must be swapped");

        let left_behind = snap.jump_clones[&char_id]
            .iter()
            .find(|c| c.jump_clone_id == jc_id)
            .expect("Left-behind jump clone");
        assert_eq!(left_behind.location_id, 60_003_760);
        assert_eq!(
            left_behind.implants,
            vec![9899],
            "Previous active implants stored in left-behind clone"
        );
    });

    // 3. Attempting clone jump before 24h cooldown expires must fail
    let early_jump = ctx
        .dispatch_rpc(
            &mut sess,
            "jumpCloneSvc",
            None,
            "CloneJump",
            &[PyRep::Integer(jc_id)],
            &kw,
            None,
        )
        .await;
    assert_eq!(early_jump.unwrap_err().code, "CloneJumpCooldownActive");

    // 4. Destroy installed clone (feat:clones.jump-clone-destroy)
    let destroy_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "jumpCloneSvc",
            None,
            "DestroyInstalledClone",
            &[PyRep::Integer(jc_id)],
            &kw,
            None,
        )
        .await
        .expect("DestroyInstalledClone");
    assert_eq!(destroy_rep, PyRep::Bool(true));

    ctx.db.with_snapshot(|snap| {
        let clones = &snap.jump_clones[&char_id];
        assert!(
            clones.iter().all(|c| c.jump_clone_id != jc_id),
            "Destroyed jump clone must be removed"
        );
    });
}
