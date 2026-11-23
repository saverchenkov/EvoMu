//! Unit and mutation-killing tests for `combat_wrecks_killmails`.

#[cfg(test)]
#[path = "combat_wrecks_loot_ops_cov_tests.rs"]
mod combat_wrecks_loot_ops_cov_tests;

use super::*;
use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::{
    make_fighter_ctx_and_session, make_fighter_item, make_fighter_type,
};
use eve_db::InventoryFlag;
use eve_destiny::{Ball, CombatState, MovementMode};
use eve_proto::PyRep;
use glam::DVec3;
use std::collections::HashMap;

#[test]
fn insurance_quotes_contracts_voiding_payouts_and_killmail_sha1_hash() {
    let (ctx, _session, char_id, ship_id, _sys_id) = make_fighter_ctx_and_session();

    let (ref_val, levels) = compute_ship_insurance_quote_levels(&ctx, 23_915);
    assert!(ref_val >= 250_000.0);
    assert_eq!(levels.len(), 6);
    assert!((levels[0].0 - 0.5).abs() < 1e-6);
    assert!((levels[5].0 - 1.0).abs() < 1e-6);
    assert_eq!(levels[5].1, "Platinum");

    // Set insured_payout_isk on ship and sync contract
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&ship_id).unwrap().insured_payout_isk = ref_val;
    });
    sync_ship_insurance_contracts(&ctx, char_id);
    ctx.db.with_snapshot(|snap| {
        let c = snap.insurance_contracts.get(&ship_id).unwrap();
        assert_eq!(c.level_name, "Platinum");
        assert!((c.payout_isk - ref_val).abs() < 1e-2);
    });

    // Void contract
    assert!(void_ship_insurance_contract(&ctx, ship_id));
    assert!(!void_ship_insurance_contract(&ctx, ship_id));

    // CONCORD criminal kill and capsule (670) pay 0.0 insurance
    assert_eq!(
        process_ship_insurance_payout(&ctx, char_id, ship_id, 23_915, true),
        0.0
    );
    assert_eq!(
        process_ship_insurance_payout(&ctx, char_id, ship_id, 670, false),
        0.0
    );

    // Uninsured legitimate destruction pays 40% Basic payout, records journal and sends mail
    let basic_payout = process_ship_insurance_payout(&ctx, char_id, ship_id, 23_915, false);
    assert!((basic_payout - compute_insurance_quote(ref_val, 0.40).1).abs() < 1e-2);
    ctx.db.with_snapshot(|snap| {
        assert!(snap
            .wallet_journal
            .iter()
            .any(|j| j.character_id == char_id && j.entry_type == "insurance_payout"));
        assert!(snap
            .mails
            .iter()
            .any(|m| m.recipient_id == char_id && m.sender_id == 1_000_132));
    });

    // SHA-1 ESI killmail hash: 40 hex chars, differs when victim/attacker/time changes
    let h1 = compute_killmail_esi_hash(char_id, 90_000_002, 23_915, 1_750_000_000);
    let h2 = compute_killmail_esi_hash(0, 0, 23_915, 1_750_000_000);
    assert_eq!(h1.len(), 40);
    assert_eq!(h2.len(), 40);
    assert_ne!(h1, h2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn container_decay_crystal_damage_deactivate_loot_all_and_self_destruct_killmail() {
    let (mut ctx, mut session, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    let kwargs = HashMap::new();
    let now = ctx.now_unix();

    let mut sde = (*ctx.sde).clone();
    // Frequency crystal (group 86, type 86010, hp attr 9 = 2.0)
    sde.types.insert(
        86_010,
        make_fighter_type(86_010, 86, 8, 1.0, vec![(9, 2.0)]),
    );
    // Wreck (type 26557, cat 2) and Jetcan (type 23, cat 2) and Cargo item (type 34, vol 10.0)
    sde.types
        .insert(26_557, make_fighter_type(26_557, 186, 2, 100.0, vec![]));
    sde.types
        .insert(23, make_fighter_type(23, 12, 2, 100.0, vec![]));
    sde.types
        .insert(34, make_fighter_type(34, 18, 4, 10.0, vec![]));
    ctx.sde = std::sync::Arc::new(sde);

    // 1. Container decay: jetcan (3600s) vs wreck (7200s)
    let jetcan_id = 850_001_i64;
    let wreck_id = 850_002_i64;
    let foreign_wreck_id = 850_003_i64;
    let loot_in_foreign = 850_004_i64;
    let crystal_id = 850_005_i64;
    let mod_id = 850_006_i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            jetcan_id,
            make_fighter_item(jetcan_id, 23, char_id, sys_id as i64, InventoryFlag::None, 1),
        );
        snap.items.insert(
            wreck_id,
            make_fighter_item(wreck_id, 26_557, char_id, sys_id as i64, InventoryFlag::None, 1),
        );
        snap.items.insert(
            foreign_wreck_id,
            make_fighter_item(
                foreign_wreck_id,
                26_557,
                90_000_009,
                sys_id as i64,
                InventoryFlag::None,
                1,
            ),
        );
        snap.items.insert(
            loot_in_foreign,
            make_fighter_item(
                loot_in_foreign,
                34,
                90_000_009,
                foreign_wreck_id,
                InventoryFlag::Cargo,
                5,
            ),
        );
        let mut crys = make_fighter_item(crystal_id, 86_010, char_id, ship_id, InventoryFlag::Cargo, 1);
        crys.dynamic_attributes.insert(3, 1.5); // 1.5 / 2.0 = 0.75 damage fraction
        snap.items.insert(crystal_id, crys);
        snap.items.insert(
            mod_id,
            make_fighter_item(mod_id, 86_010, char_id, ship_id, InventoryFlag::HiSlot0, 1),
        );
        snap.module_active_states.insert(mod_id);
    });

    register_container_lifetime(&ctx, jetcan_id, sys_id, 23, char_id, now, false);
    register_container_lifetime(&ctx, wreck_id, sys_id, 26_557, char_id, now, false);

    assert!(step_wreck_and_jetcan_decay(&ctx, sys_id, now + 3_599).await.is_empty());
    let dec1 = step_wreck_and_jetcan_decay(&ctx, sys_id, now + 3_600).await;
    assert_eq!(dec1, vec![jetcan_id]);
    let dec2 = step_wreck_and_jetcan_decay(&ctx, sys_id, now + 7_200).await;
    assert_eq!(dec2, vec![wreck_id]);

    // 2. GetDamageForCrystals and dogmaIM.Deactivate
    let crys_res = handle_wrecks_killmails(
        &ctx,
        &mut session,
        "invbroker",
        "GetDamageForCrystals",
        &[],
        &kwargs,
    )
    .await
    .unwrap()
    .unwrap();
    if let PyRep::Dict(entries) = crys_res {
        assert!(entries.contains(&(PyRep::Integer(crystal_id), PyRep::Real(0.75))));
    } else {
        panic!("expected dict");
    }

    let deact_res = handle_wrecks_killmails(
        &ctx,
        &mut session,
        "dogmaIM",
        "Deactivate",
        &[PyRep::Integer(mod_id)],
        &kwargs,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(deact_res.dict_get("active"), Some(&PyRep::Bool(false)));

    // 3. LootAll on foreign wreck: blocked on SafetyLevel::Green, triggers suspect flag on SafetyLevel::Yellow/Red
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: 23_915,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 200.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState {
                shield_hp: 500.0,
                max_shield_hp: 500.0,
                armor_hp: 500.0,
                max_armor_hp: 500.0,
                structure_hp: 500.0,
                max_structure_hp: 500.0,
                ..CombatState::default()
            },
        )
        .await
        .unwrap();
    sys_handle
        .spawn_ball(Ball {
            id: foreign_wreck_id,
            type_id: 26_557,
            position: DVec3::new(1_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 1_000.0,
            radius: 20.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await
        .unwrap();
    with_mut_combat_runtime(&ctx, |rt| {
        rt.safety_levels.insert(char_id, SafetyLevel::Green);
    });
    let green_err = handle_wrecks_killmails(
        &ctx,
        &mut session,
        "invbroker",
        "LootAll",
        &[PyRep::Integer(foreign_wreck_id)],
        &kwargs,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert!(green_err.message.contains("CantDoInCurrentSafetyLevel"));

    with_mut_combat_runtime(&ctx, |rt| {
        rt.safety_levels.insert(char_id, SafetyLevel::Red);
    });
    let loot_res = handle_wrecks_killmails(
        &ctx,
        &mut session,
        "invbroker",
        "LootAll",
        &[PyRep::Integer(foreign_wreck_id)],
        &kwargs,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        loot_res.dict_get("suspectFlagTriggered"),
        Some(&PyRep::Bool(true))
    );
    assert_eq!(
        loot_res.dict_get("lootedItemIDs"),
        Some(&PyRep::List(vec![PyRep::Integer(loot_in_foreign)]))
    );

    // 4. Self-destruct start -> abort -> restart & detonate -> GetKillMail
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: 23_915,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 200.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState::default(),
        )
        .await
        .unwrap();

    let sd_start = handle_wrecks_killmails(
        &ctx,
        &mut session,
        "ship",
        "SelfDestruct",
        &[],
        &kwargs,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(sd_start.dict_get("countdownStarted"), Some(&PyRep::Bool(true)));
    assert_eq!(sd_start.dict_get("detonated"), Some(&PyRep::Bool(false)));

    let sd_abort = handle_wrecks_killmails(
        &ctx,
        &mut session,
        "ship",
        "AbortSelfDestruct",
        &[],
        &kwargs,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(sd_abort.dict_get("aborted"), Some(&PyRep::Bool(true)));

    // Record an assist, set expired countdown, and detonate
    record_killmail_assist_action(&ctx, ship_id, 90_000_009, 23_915, 2_905, 450.0, true, now);
    with_mut_combat_runtime(&ctx, |rt| {
        rt.self_destruct_countdowns.insert(
            ship_id,
            SelfDestructCountdown {
                ship_id,
                char_id,
                solar_system_id: sys_id,
                started_unix: now.saturating_sub(120),
                detonate_at_unix: now,
            },
        );
    });
    let sd_det = handle_wrecks_killmails(
        &ctx,
        &mut session,
        "ship",
        "SelfDestruct",
        &[],
        &kwargs,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(sd_det.dict_get("detonated"), Some(&PyRep::Bool(true)));
    let km_id = sd_det.dict_get("killmailID").and_then(|v| v.as_i64()).unwrap();
    let km_hash = ctx.db.with_snapshot(|snap| {
        snap.combat_crimewatch_state
            .detailed_killmails
            .get(&km_id)
            .unwrap()
            .killmail_hash
            .clone()
    });

    // Query warStatisticMgr.GetKillMail with wrong hash vs valid hash
    let bad_hash = handle_wrecks_killmails(
        &ctx,
        &mut session,
        "warStatisticMgr",
        "GetKillMail",
        &[PyRep::Integer(km_id), PyRep::String("wronghash".into())],
        &kwargs,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(bad_hash.code, "InvalidKillmailHash");

    let km_rep = handle_wrecks_killmails(
        &ctx,
        &mut session,
        "warStatisticMgr",
        "GetKillMail",
        &[PyRep::Integer(km_id), PyRep::String(km_hash.clone())],
        &kwargs,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(km_rep.dict_get("killID"), Some(&PyRep::Integer(km_id)));
    assert_eq!(
        km_rep.dict_get("finalCharacterID"),
        Some(&PyRep::Integer(90_000_009))
    );

    // Uninvolved third party (even sharing an NPC corp) cannot read killmail without a valid hash
    let mut outsider = session.clone();
    outsider.session_id = 99_901;
    outsider.user_id = Some(99_901);
    outsider.character_id = Some(90_099_901);
    let denied_no_hash = handle_wrecks_killmails(
        &ctx,
        &mut outsider,
        "warStatisticMgr",
        "GetKillMail",
        &[PyRep::Integer(km_id)],
        &kwargs,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(denied_no_hash.code, "KillmailAccessDenied");

    let denied_empty_hash = handle_wrecks_killmails(
        &ctx,
        &mut outsider,
        "warStatisticMgr",
        "GetKillMail",
        &[PyRep::Integer(km_id), PyRep::String(String::new())],
        &kwargs,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(denied_empty_hash.code, "KillmailAccessDenied");

    // Passing valid shareable killmail hash grants read access to outsider
    let granted_with_hash = handle_wrecks_killmails(
        &ctx,
        &mut outsider,
        "warStatisticMgr",
        "GetKillMail",
        &[PyRep::Integer(km_id), PyRep::String(km_hash)],
        &kwargs,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(granted_with_hash.dict_get("killID"), Some(&PyRep::Integer(km_id)));
}

#[test]
fn insurance_contract_persists_end_unix_across_reload_and_expires() {
    let (ctx, _session, char_id, ship_id, _sys_id) = make_fighter_ctx_and_session();
    let now = 1_750_000_000_u64;
    ctx.set_clock(std::sync::Arc::new(eve_db::VirtualServerClock::with_fixed_base(now)));

    let (ref_val, _) = compute_ship_insurance_quote_levels(&ctx, 23_915);
    let saved_start = now - 30 * 86_400;
    let saved_end = now + 54 * 86_400;

    // Seed an existing Gold insurance contract in WorldStateSnapshot (as loaded from SQLite on restart).
    let cloned_snap = ctx.db.with_mut_snapshot(|snap| {
        let ship = snap.items.get_mut(&ship_id).unwrap();
        ship.insured_payout_isk = ref_val;
        snap.insurance_contracts.insert(
            ship_id,
            eve_db::PersistedInsuranceContractRecord {
                ship_id,
                ship_type_id: ship.type_id,
                owner_id: char_id,
                level_name: "Gold".into(),
                level_fraction: 0.80,
                cost_isk: 50_000.0,
                payout_isk: ref_val * 0.80,
                start_unix: saved_start,
                end_unix: saved_end,
            },
        );
        snap.clone()
    });

    // Simulate fresh server restart with empty runtime map.
    let repo_restarted = std::sync::Arc::new(eve_db::WorldRepository::new_empty());
    repo_restarted.with_mut_snapshot(|s| *s = cloned_snap);
    let ctx_restarted = crate::router::ServerContext::with_sde_and_repository(
        ctx.sde.clone(),
        repo_restarted,
    );
    ctx_restarted.set_clock(std::sync::Arc::new(eve_db::VirtualServerClock::with_fixed_base(now)));

    sync_ship_insurance_contracts(&ctx_restarted, char_id);
    ctx_restarted.db.with_snapshot(|snap| {
        let c = snap.insurance_contracts.get(&ship_id).expect("persisted contract kept");
        assert_eq!(c.start_unix, saved_start);
        assert_eq!(c.end_unix, saved_end, "restart must NOT reset end_unix to +84 days");
        assert_eq!(c.level_name, "Gold");
    });

    // Once now >= saved_end, sweep expires the persisted contract and clears ship payout.
    sweep_expired_insurance_contracts(&ctx_restarted, saved_end);
    ctx_restarted.db.with_snapshot(|snap| {
        assert!(!snap.insurance_contracts.contains_key(&ship_id));
        assert_eq!(snap.items[&ship_id].insured_payout_isk, 0.0);
    });

    // Repackaged (!singleton) ship voids persisted insurance contract on sync_ship_insurance_contracts
    ctx.db.with_mut_snapshot(|snap| {
        let ship = snap.items.get_mut(&ship_id).unwrap();
        ship.singleton = false;
        ship.insured_payout_isk = ref_val;
        snap.insurance_contracts.insert(
            ship_id,
            eve_db::PersistedInsuranceContractRecord {
                ship_id,
                ship_type_id: ship.type_id,
                owner_id: char_id,
                level_name: "Gold".into(),
                level_fraction: 0.80,
                cost_isk: 50_000.0,
                payout_isk: ref_val * 0.80,
                start_unix: saved_start,
                end_unix: saved_end,
            },
        );
    });
    sync_ship_insurance_contracts(&ctx, char_id);
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.insurance_contracts.contains_key(&ship_id));
    });
}

#[cfg(test)]
#[path = "combat_wrecks_killmails_extra_unit_tests.rs"]
mod combat_wrecks_killmails_extra_unit_tests;

#[tokio::test]
async fn self_destruct_rpc_aborts_if_cancelled_or_docked_before_detonate_commit_gh_1174() {
    let (ctx, mut session, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    let base_now = 1_770_000_000u64;
    ctx.set_clock(std::sync::Arc::new(
        eve_db::VirtualServerClock::with_fixed_base(base_now),
    ));
    let ship_type = ctx.db.with_snapshot(|snap| snap.items[&ship_id].type_id);
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: ship_type,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 200.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState {
                shield_hp: 500.0,
                max_shield_hp: 500.0,
                armor_hp: 500.0,
                max_armor_hp: 500.0,
                structure_hp: 500.0,
                max_structure_hp: 500.0,
                ..CombatState::default()
            },
        )
        .await
        .unwrap();

    let km_before = ctx.db.with_snapshot(|snap| snap.killmails.len());
    let kwargs = HashMap::new();

    // 1. Start countdown via RPC, cancel via RPC (`AbortSelfDestruct`), advance past due time,
    //    then confirm guarded execution & RPC both refuse to detonate and ship survives.
    let start_res = ctx
        .dispatch_rpc(&mut session, "ship", None, "SelfDestruct", &[], &kwargs, None)
        .await
        .expect("start SelfDestruct");
    assert_eq!(start_res.dict_get("countdownStarted"), Some(&PyRep::Bool(true)));

    let abort_res = ctx
        .dispatch_rpc(
            &mut session,
            "ship",
            None,
            "AbortSelfDestruct",
            &[],
            &kwargs,
            None,
        )
        .await
        .expect("AbortSelfDestruct");
    assert_eq!(abort_res.dict_get("aborted"), Some(&PyRep::Bool(true)));

    ctx.advance_secs(125);
    let now_due = ctx.now_unix();
    let res_after_abort = crate::combat::death_pipeline::execute_self_destruct_guarded(
        &ctx,
        sys_id,
        char_id,
        ship_id,
        DVec3::ZERO,
        now_due,
        now_due,
        None,
    )
    .expect("guarded RPC self-destruct returns Ok(None) after abort");
    assert!(res_after_abort.is_none());

    // 2. Re-arm, advance until due, then dock character concurrently before detonation commits.
    let start_res_2 = ctx
        .dispatch_rpc(&mut session, "ship", None, "SelfDestruct", &[], &kwargs, None)
        .await
        .expect("restart SelfDestruct");
    assert_eq!(
        start_res_2.dict_get("countdownStarted"),
        Some(&PyRep::Bool(true))
    );
    ctx.clock().advance_secs(125);
    let station_id = 60_003_760i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = Some(station_id);
        }
        if let Some(it) = snap.items.get_mut(&ship_id) {
            it.location_id = station_id;
        }
    });

    let docked_err = handle_wrecks_killmails(
        &ctx,
        &mut session,
        "ship",
        "SelfDestruct",
        &[],
        &kwargs,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(docked_err.code, "CannotSelfDestructWhileDocked");
    ctx.db.with_snapshot(|snap| {
        assert_eq!(
            snap.items.get(&ship_id).map(|i| (i.owner_id, i.location_id)),
            Some((char_id, station_id))
        );
        assert_eq!(snap.killmails.len(), km_before);
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn self_destruct_rpc_cancel_concurrent_with_lifecycle_has_zero_post_cancel_destructions_gh_1174() {
    let (ctx_owned, base_session, char_id, orig_ship_id, sys_id) = make_fighter_ctx_and_session();
    let base_now = 1_770_000_000u64;
    ctx_owned.set_clock(std::sync::Arc::new(
        eve_db::VirtualServerClock::with_fixed_base(base_now),
    ));
    let ctx = std::sync::Arc::new(ctx_owned);
    let ship_type = ctx.db.with_snapshot(|snap| snap.items[&orig_ship_id].type_id);
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let kwargs = HashMap::new();

    // Helper closure to prepare a fresh undocked in-space ship and ball for `char_id`.
    let prepare_ship = |ship_id: i64| {
        let km_before = ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                ship_id,
                make_fighter_item(
                    ship_id,
                    ship_type,
                    char_id,
                    i64::from(sys_id),
                    InventoryFlag::None,
                    1,
                ),
            );
            let ch = snap
                .characters
                .get_mut(&char_id)
                .expect("character present in test snapshot");
            ch.station_id = None;
            ch.structure_id = None;
            ch.solar_system_id = sys_id;
            ch.active_ship_id = ship_id;
            snap.killmails.len()
        });
        let mut sess = base_session.clone();
        sess.ship_id = Some(ship_id);
        sess.solar_system_id = Some(sys_id);
        (km_before, sess)
    };

    let spawn_ball_for = |ship_id: i64| {
        let sys_h = sys_handle.clone();
        async move {
            sys_h
                .spawn_ball_with_combat(
                    Ball {
                        id: ship_id,
                        type_id: ship_type,
                        position: DVec3::ZERO,
                        velocity: DVec3::ZERO,
                        max_velocity: 200.0,
                        inertia: 1.0,
                        mass: 1_000_000.0,
                        radius: 40.0,
                        speed_fraction: 0.0,
                        mode: MovementMode::Stop,
                    },
                    CombatState {
                        shield_hp: 500.0,
                        max_shield_hp: 500.0,
                        armor_hp: 500.0,
                        max_armor_hp: 500.0,
                        structure_hp: 500.0,
                        max_structure_hp: 500.0,
                        ..CombatState::default()
                    },
                )
                .await
                .unwrap();
        }
    };

    // 0. Control: an un-aborted countdown started via RPC detonates when lifecycle advances past due.
    let ctrl_ship_id = 910_000i64;
    let (ctrl_km_before, mut ctrl_sess) = prepare_ship(ctrl_ship_id);
    spawn_ball_for(ctrl_ship_id).await;
    let ctrl_start = ctx
        .dispatch_rpc(
            &mut ctrl_sess,
            "ship",
            None,
            "SelfDestruct",
            &[],
            &kwargs,
            None,
        )
        .await
        .expect("control SelfDestruct starts");
    assert_eq!(
        ctrl_start.dict_get("countdownStarted"),
        Some(&PyRep::Bool(true))
    );
    ctx.clock().advance_secs(125);
    crate::sp6_lifecycle::on_advance_secs(&ctx, ctx.now_unix(), 125);
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&ctrl_ship_id));
        assert_eq!(snap.killmails.len(), ctrl_km_before + 1);
    });

    let mut post_cancel_destructions = 0usize;

    // 1. Pre-due boundary race (24 iterations): cancel via `("ship", "AbortSelfDestruct")` RPC
    //    concurrently with cluster lifecycle & AoE stepping before advancing past `detonate_at_unix`.
    for i in 0..24i64 {
        let ship_id = 911_000 + i;
        let (km_before, mut start_sess) = prepare_ship(ship_id);
        spawn_ball_for(ship_id).await;
        let sd_start = ctx
            .dispatch_rpc(
                &mut start_sess,
                "ship",
                None,
                "SelfDestruct",
                &[],
                &kwargs,
                None,
            )
            .await
            .expect("SelfDestruct start");
        assert_eq!(
            sd_start.dict_get("countdownStarted"),
            Some(&PyRep::Bool(true))
        );

        ctx.clock().advance_secs(119);
        let t_now = ctx.now_unix();
        let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(3));

        let ctx_abort = std::sync::Arc::clone(&ctx);
        let b_abort = std::sync::Arc::clone(&barrier);
        let mut abort_sess = start_sess.clone();
        abort_sess.session_id = 10_000 + i as u64;
        let abort_task = tokio::spawn(async move {
            b_abort.wait().await;
            if i % 3 != 0 {
                tokio::task::yield_now().await;
            }
            let res = ctx_abort
                .dispatch_rpc(
                    &mut abort_sess,
                    "ship",
                    None,
                    "AbortSelfDestruct",
                    &[],
                    &HashMap::new(),
                    None,
                )
                .await
                .expect("AbortSelfDestruct RPC succeeds");
            res.dict_get("aborted") == Some(&PyRep::Bool(true))
        });

        let ctx_cluster = std::sync::Arc::clone(&ctx);
        let b_cluster = std::sync::Arc::clone(&barrier);
        let cluster_task = tokio::spawn(async move {
            b_cluster.wait().await;
            crate::sp6_lifecycle::on_advance_secs(&ctx_cluster, t_now, 1);
        });

        let ctx_aoe = std::sync::Arc::clone(&ctx);
        let b_aoe = std::sync::Arc::clone(&barrier);
        let aoe_task = tokio::spawn(async move {
            b_aoe.wait().await;
            let _ = crate::services::combat_crimewatch::combat_missiles_aoe::step_missiles_and_breachers(
                &ctx_aoe, sys_id, 1.0,
            )
            .await;
        });

        let aborted = abort_task.await.unwrap();
        cluster_task.await.unwrap();
        aoe_task.await.unwrap();
        assert!(aborted, "pre-due AbortSelfDestruct RPC must cancel countdown on iter {i}");

        // Advance well past original `detonate_at_unix` and run all lifecycle steppers.
        ctx.advance_secs(15);
        crate::sp6_lifecycle::on_advance_secs(&ctx, ctx.now_unix(), 15);
        crate::sp6_lifecycle::tick_background_cluster(&ctx).await;
        let _ = crate::services::combat_crimewatch::combat_missiles_aoe::step_missiles_and_breachers(
            &ctx, sys_id, 1.0,
        )
        .await;

        let survived = ctx.db.with_snapshot(|snap| {
            snap.items.contains_key(&ship_id)
                && snap.characters[&char_id].active_ship_id == ship_id
                && snap.killmails.len() == km_before
                && snap.sp6_lifecycle.self_destructs.is_empty()
        });
        if !survived {
            post_cancel_destructions += 1;
        }
        assert!(sys_handle.get_ball_snapshot(ship_id).await.unwrap().is_some());
    }

    // 2. Simultaneous due-time race (32 iterations): race `("ship", "AbortSelfDestruct")` RPC
    //    directly against `on_advance_secs`, `step_missiles_and_breachers`, and due `SelfDestruct`.
    for i in 0..32i64 {
        let ship_id = 912_000 + i;
        let (km_before, mut start_sess) = prepare_ship(ship_id);
        spawn_ball_for(ship_id).await;
        let sd_start = ctx
            .dispatch_rpc(
                &mut start_sess,
                "ship",
                None,
                "SelfDestruct",
                &[],
                &kwargs,
                None,
            )
            .await
            .expect("SelfDestruct start");
        assert_eq!(
            sd_start.dict_get("countdownStarted"),
            Some(&PyRep::Bool(true))
        );

        let expected_generation =
            with_combat_runtime(&ctx, |rt| rt.self_destruct_generation(ship_id));
        ctx.clock().advance_secs(120);
        let t_due = ctx.now_unix();
        let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(4));

        let ctx_abort = std::sync::Arc::clone(&ctx);
        let b_abort = std::sync::Arc::clone(&barrier);
        let mut abort_sess = start_sess.clone();
        abort_sess.session_id = 20_000 + i as u64;
        let abort_task = tokio::spawn(async move {
            b_abort.wait().await;
            if i % 4 != 0 {
                tokio::task::yield_now().await;
            }
            let res = ctx_abort
                .dispatch_rpc(
                    &mut abort_sess,
                    "ship",
                    None,
                    "AbortSelfDestruct",
                    &[],
                    &HashMap::new(),
                    None,
                )
                .await
                .expect("AbortSelfDestruct RPC succeeds");
            res.dict_get("aborted") == Some(&PyRep::Bool(true))
        });

        let ctx_cluster = std::sync::Arc::clone(&ctx);
        let b_cluster = std::sync::Arc::clone(&barrier);
        let cluster_task = tokio::spawn(async move {
            b_cluster.wait().await;
            if i % 4 == 1 {
                tokio::task::yield_now().await;
            }
            crate::sp6_lifecycle::on_advance_secs(&ctx_cluster, t_due, 1);
            crate::sp6_lifecycle::tick_background_cluster(&ctx_cluster).await;
        });

        let ctx_aoe = std::sync::Arc::clone(&ctx);
        let b_aoe = std::sync::Arc::clone(&barrier);
        let aoe_task = tokio::spawn(async move {
            b_aoe.wait().await;
            if i % 4 == 2 {
                tokio::task::yield_now().await;
            }
            let _ = crate::services::combat_crimewatch::combat_missiles_aoe::step_missiles_and_breachers(
                &ctx_aoe, sys_id, 1.0,
            )
            .await;
        });

        let ctx_det = std::sync::Arc::clone(&ctx);
        let b_det = std::sync::Arc::clone(&barrier);
        let rpc_det_task = tokio::spawn(async move {
            b_det.wait().await;
            if i % 4 == 3 {
                tokio::task::yield_now().await;
            }
            let _ = crate::combat::death_pipeline::execute_self_destruct_guarded(
                &ctx_det,
                sys_id,
                char_id,
                ship_id,
                DVec3::ZERO,
                t_due,
                t_due,
                Some(expected_generation),
            );
        });

        let aborted = abort_task.await.unwrap();
        cluster_task.await.unwrap();
        aoe_task.await.unwrap();
        rpc_det_task.await.unwrap();

        // Run follow-up lifecycle & AoE ticks after the concurrent race settles.
        ctx.advance_secs(5);
        crate::sp6_lifecycle::on_advance_secs(&ctx, ctx.now_unix(), 5);
        let _ = crate::services::combat_crimewatch::combat_missiles_aoe::step_missiles_and_breachers(
            &ctx, sys_id, 1.0,
        )
        .await;

        let ball_alive = sys_handle.get_ball_snapshot(ship_id).await.unwrap().is_some();
        let db_ok = ctx.db.with_snapshot(|snap| {
            let clean_sd = !snap.sp6_lifecycle.self_destructs.contains_key(&ship_id);
            if aborted {
                clean_sd
                    && snap.items.contains_key(&ship_id)
                    && snap.characters[&char_id].active_ship_id == ship_id
                    && snap.killmails.len() == km_before
            } else {
                clean_sd
            }
        });
        if aborted && (!db_ok || !ball_alive) {
            post_cancel_destructions += 1;
        }
        assert!(db_ok, "self-destruct state remained consistent on iter {i}");
    }

    assert_eq!(post_cancel_destructions, 0);
    assert_eq!(
        crate::sp6_lifecycle::with_sp6_state(&ctx.db, |st| st.self_destructs.len()),
        0
    );
    assert_eq!(
        with_combat_runtime(&ctx, |rt| rt.self_destruct_countdowns.len()),
        0
    );
}

#[tokio::test]
async fn self_destruct_and_abort_emit_v24_remote_messages_to_pilot_and_observers() {
    use eve_db::ServerClock;
    const BLUETIME_TICKS_PER_SEC: i64 = 10_000_000;
    let (ctx, mut session, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    let vclock = std::sync::Arc::new(eve_db::VirtualServerClock::with_fixed_base(1_767_225_600));
    ctx.set_clock(vclock.clone());
    let now = ctx.now_unix();

    let obs_char_id = 90_000_002_i64;
    let obs_ship_id = 100_000_002_i64;
    session.session_id = 92_101;
    let (pilot_uid, obs_uid) = ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            obs_ship_id,
            make_fighter_item(
                obs_ship_id,
                23_915,
                obs_char_id,
                sys_id as i64,
                InventoryFlag::None,
                1,
            ),
        );
        if let Some(ch) = snap.characters.get_mut(&obs_char_id) {
            ch.active_ship_id = obs_ship_id;
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
        }
        snap.item_positions.insert(ship_id, [0.0, 0.0, 0.0]);
        snap.item_positions.insert(obs_ship_id, [5_000.0, 0.0, 0.0]);
        (
            snap.characters[&char_id].user_id,
            snap.characters[&obs_char_id].user_id,
        )
    });
    ctx.db
        .acquire_online_lock(pilot_uid, session.session_id, Some(char_id), true, now)
        .unwrap();
    ctx.db
        .acquire_online_lock(obs_uid, 92_102, Some(obs_char_id), true, now)
        .unwrap();

    let sys_h = ctx.systems.get_or_spawn(sys_id);
    for (bid, pos) in [
        (ship_id, DVec3::ZERO),
        (obs_ship_id, DVec3::new(5_000.0, 0.0, 0.0)),
    ] {
        sys_h
            .spawn_ball_with_combat(
                Ball {
                    id: bid,
                    type_id: 23_915,
                    position: pos,
                    velocity: DVec3::ZERO,
                    max_velocity: 120.0,
                    inertia: 1.0,
                    mass: 1_000_000.0,
                    radius: 40.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                CombatState::default(),
            )
            .await
            .unwrap();
    }

    let kw = HashMap::new();
    let _ = ctx.drain_notifications_for_char(char_id);
    let _ = ctx.drain_notifications_for_char(obs_char_id);

    // 0. Idle abort and docked SelfDestruct emit no OnRemoteMessage notices.
    let idle_abort =
        handle_wrecks_killmails(&ctx, &mut session, "ship", "AbortSelfDestruct", &[], &kw)
            .await
            .unwrap()
            .unwrap();
    assert_eq!(idle_abort.dict_get("aborted"), Some(&PyRep::Bool(false)));
    assert!(ctx
        .drain_notifications_for_char(char_id)
        .iter()
        .all(|n| n.event != "OnRemoteMessage"));
    assert!(ctx
        .drain_notifications_for_char(obs_char_id)
        .iter()
        .all(|n| n.event != "OnRemoteMessage"));

    session.station_id = Some(60_003_760);
    let docked_err = handle_wrecks_killmails(&ctx, &mut session, "ship", "SelfDestruct", &[], &kw)
        .await
        .unwrap()
        .unwrap_err();
    assert!(!docked_err.code.is_empty());
    session.station_id = None;
    assert!(ctx
        .drain_notifications_for_char(char_id)
        .iter()
        .all(|n| n.event != "OnRemoteMessage"));
    assert!(ctx
        .drain_notifications_for_char(obs_char_id)
        .iter()
        .all(|n| n.event != "OnRemoteMessage"));

    // 1. Fresh ship.SelfDestruct emits SelfDestructInitiated (pilot) + SelfDestructInitiatedOther (observer).
    let sd_start = handle_wrecks_killmails(&ctx, &mut session, "ship", "SelfDestruct", &[], &kw)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(sd_start.dict_get("detonated"), Some(&PyRep::Bool(false)));
    assert_eq!(
        sd_start.dict_get("remainingSeconds"),
        Some(&PyRep::Integer(SELF_DESTRUCT_COUNTDOWN_SECS as i64))
    );

    let pilot_start: Vec<_> = ctx
        .drain_notifications_for_char(char_id)
        .into_iter()
        .filter(|n| n.event == "OnRemoteMessage")
        .collect();
    assert_eq!(pilot_start.len(), 1);
    assert_eq!(pilot_start[0].id_type, "charid");
    assert_eq!(pilot_start[0].args.len(), 2);
    assert_eq!(
        pilot_start[0].args[0],
        PyRep::String("SelfDestructInitiated".into())
    );
    assert_eq!(
        pilot_start[0].args[1].dict_get("what"),
        Some(&PyRep::Tuple(vec![
            PyRep::Integer(4),
            PyRep::Integer(23_915),
        ]))
    );
    assert_eq!(
        pilot_start[0].args[1].dict_get("when"),
        Some(&PyRep::Integer(SELF_DESTRUCT_COUNTDOWN_SECS as i64))
    );

    let obs_start: Vec<_> = ctx
        .drain_notifications_for_char(obs_char_id)
        .into_iter()
        .filter(|n| n.event == "OnRemoteMessage")
        .collect();
    assert_eq!(obs_start.len(), 1);
    assert_eq!(obs_start[0].id_type, "charid");
    assert_eq!(obs_start[0].args.len(), 2);
    assert_eq!(
        obs_start[0].args[0],
        PyRep::String("SelfDestructInitiatedOther".into())
    );
    assert_eq!(
        obs_start[0].args[1].dict_get("charID"),
        Some(&PyRep::Integer(char_id))
    );
    assert_eq!(
        obs_start[0].args[1].dict_get("shipTypeID"),
        Some(&PyRep::Integer(23_915))
    );
    assert_eq!(
        obs_start[0].args[1].dict_get("timeRemaining"),
        Some(&PyRep::Integer(
            (SELF_DESTRUCT_COUNTDOWN_SECS as i64) * BLUETIME_TICKS_PER_SEC
        ))
    );

    // 2. Repeat ship.SelfDestruct while active emits no duplicate initiation notices.
    vclock.advance_secs(10);
    let expected_rem_secs = (SELF_DESTRUCT_COUNTDOWN_SECS - 10) as i64;
    let sd_repeat = handle_wrecks_killmails(&ctx, &mut session, "ship", "SelfDestruct", &[], &kw)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(sd_repeat.dict_get("detonated"), Some(&PyRep::Bool(false)));
    assert_eq!(
        sd_repeat.dict_get("remainingSeconds"),
        Some(&PyRep::Integer(expected_rem_secs))
    );
    assert!(ctx
        .drain_notifications_for_char(char_id)
        .iter()
        .all(|n| n.event != "OnRemoteMessage"));
    assert!(ctx
        .drain_notifications_for_char(obs_char_id)
        .iter()
        .all(|n| n.event != "OnRemoteMessage"));

    // 3. ship.AbortSelfDestruct emits SelfDestructAborted2 (pilot) + SelfDestructAbortedOther2 (observer).
    let sd_abort =
        handle_wrecks_killmails(&ctx, &mut session, "ship", "AbortSelfDestruct", &[], &kw)
            .await
            .unwrap()
            .unwrap();
    assert_eq!(sd_abort.dict_get("aborted"), Some(&PyRep::Bool(true)));

    let pilot_abort: Vec<_> = ctx
        .drain_notifications_for_char(char_id)
        .into_iter()
        .filter(|n| n.event == "OnRemoteMessage")
        .collect();
    assert_eq!(pilot_abort.len(), 1);
    assert_eq!(
        pilot_abort[0].args[0],
        PyRep::String("SelfDestructAborted2".into())
    );
    assert_eq!(
        pilot_abort[0].args[1].dict_get("timeLeft"),
        Some(&PyRep::Integer(expected_rem_secs * BLUETIME_TICKS_PER_SEC))
    );

    let obs_abort: Vec<_> = ctx
        .drain_notifications_for_char(obs_char_id)
        .into_iter()
        .filter(|n| n.event == "OnRemoteMessage")
        .collect();
    assert_eq!(obs_abort.len(), 1);
    assert_eq!(
        obs_abort[0].args[0],
        PyRep::String("SelfDestructAbortedOther2".into())
    );
    assert_eq!(
        obs_abort[0].args[1].dict_get("charID"),
        Some(&PyRep::Integer(char_id))
    );
    assert_eq!(
        obs_abort[0].args[1].dict_get("timeLeft"),
        Some(&PyRep::Integer(expected_rem_secs * BLUETIME_TICKS_PER_SEC))
    );

    // 4. ship.SelfDestruct restart + expiry detonation emits SelfDestructImmediate / SelfDestructImmediateOther.
    let cmd_start =
        handle_wrecks_killmails(&ctx, &mut session, "ship", "SelfDestruct", &[], &kw)
            .await
            .unwrap()
            .unwrap();
    assert_eq!(cmd_start.dict_get("detonated"), Some(&PyRep::Bool(false)));
    assert!(ctx
        .drain_notifications_for_char(char_id)
        .iter()
        .any(|n| n.event == "OnRemoteMessage"
            && n.args.first() == Some(&PyRep::String("SelfDestructInitiated".into()))));
    assert!(ctx
        .drain_notifications_for_char(obs_char_id)
        .iter()
        .any(|n| n.event == "OnRemoteMessage"
            && n.args.first() == Some(&PyRep::String("SelfDestructInitiatedOther".into()))));

    vclock.advance_secs(SELF_DESTRUCT_COUNTDOWN_SECS);
    let cmd_detonate =
        handle_wrecks_killmails(&ctx, &mut session, "ship", "SelfDestruct", &[], &kw)
            .await
            .unwrap()
            .unwrap();
    assert_eq!(cmd_detonate.dict_get("detonated"), Some(&PyRep::Bool(true)));

    let pilot_det: Vec<_> = ctx
        .drain_notifications_for_char(char_id)
        .into_iter()
        .filter(|n| n.event == "OnRemoteMessage")
        .collect();
    assert_eq!(pilot_det.len(), 1);
    assert_eq!(
        pilot_det[0].args[0],
        PyRep::String("SelfDestructImmediate".into())
    );
    assert_eq!(
        pilot_det[0].args[1].dict_get("shipTypeID"),
        Some(&PyRep::Integer(23_915))
    );

    let obs_det: Vec<_> = ctx
        .drain_notifications_for_char(obs_char_id)
        .into_iter()
        .filter(|n| n.event == "OnRemoteMessage")
        .collect();
    assert_eq!(obs_det.len(), 1);
    assert_eq!(
        obs_det[0].args[0],
        PyRep::String("SelfDestructImmediateOther".into())
    );
    assert_eq!(
        obs_det[0].args[1].dict_get("charID"),
        Some(&PyRep::Integer(char_id))
    );
    assert_eq!(
        obs_det[0].args[1].dict_get("shipTypeID"),
        Some(&PyRep::Integer(23_915))
    );
}

#[tokio::test]
async fn self_destruct_server_timer_expiry_emits_immediate_notices_without_second_rpc() {
    use eve_db::ServerClock;

    let (ctx, mut session, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    let vclock = std::sync::Arc::new(eve_db::VirtualServerClock::with_fixed_base(1_767_225_600));
    ctx.set_clock(vclock.clone());
    let now = ctx.now_unix();

    let obs_char_id = 90_000_002_i64;
    let obs_ship_id = 100_000_002_i64;
    session.session_id = 92_201;
    let (pilot_uid, obs_uid) = ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            obs_ship_id,
            make_fighter_item(
                obs_ship_id,
                23_915,
                obs_char_id,
                sys_id as i64,
                InventoryFlag::None,
                1,
            ),
        );
        if let Some(ch) = snap.characters.get_mut(&obs_char_id) {
            ch.active_ship_id = obs_ship_id;
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
        }
        snap.item_positions.insert(ship_id, [0.0, 0.0, 0.0]);
        snap.item_positions.insert(obs_ship_id, [5_000.0, 0.0, 0.0]);
        (
            snap.characters[&char_id].user_id,
            snap.characters[&obs_char_id].user_id,
        )
    });
    ctx.db
        .acquire_online_lock(pilot_uid, session.session_id, Some(char_id), true, now)
        .unwrap();
    ctx.db
        .acquire_online_lock(obs_uid, 92_202, Some(obs_char_id), true, now)
        .unwrap();

    let sys_h = ctx.systems.get_or_spawn(sys_id);
    for (bid, pos) in [
        (ship_id, DVec3::ZERO),
        (obs_ship_id, DVec3::new(5_000.0, 0.0, 0.0)),
    ] {
        sys_h
            .spawn_ball_with_combat(
                Ball {
                    id: bid,
                    type_id: 23_915,
                    position: pos,
                    velocity: DVec3::ZERO,
                    max_velocity: 120.0,
                    inertia: 1.0,
                    mass: 1_000_000.0,
                    radius: 40.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                CombatState::default(),
            )
            .await
            .unwrap();
    }

    let kw = HashMap::new();
    let _ = ctx.drain_notifications_for_char(char_id);
    let _ = ctx.drain_notifications_for_char(obs_char_id);

    // 1. Start 120s countdown via ONE `ship.SelfDestruct([ship_id])` RPC (no second RPC ever sent).
    let start_res = handle_wrecks_killmails(
        &ctx,
        &mut session,
        "ship",
        "SelfDestruct",
        &[PyRep::Integer(ship_id)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(start_res.dict_get("countdownStarted"), Some(&PyRep::Bool(true)));
    let _ = ctx.drain_notifications_for_char(char_id);
    let _ = ctx.drain_notifications_for_char(obs_char_id);

    // 2. Advancing 119s via 1 Hz lifecycle tick does not detonate yet or emit immediate notices.
    let t0 = ctx.now_unix();
    vclock.advance_secs(119);
    crate::sp6_lifecycle::on_advance_secs(&ctx, t0, 119);
    assert!(ctx.db.with_snapshot(|s| s.items.contains_key(&ship_id)));
    for cid in [char_id, obs_char_id] {
        assert!(!ctx
            .drain_notifications_for_char(cid)
            .iter()
            .any(|n| n.event == "OnRemoteMessage"
                && matches!(n.args.first(), Some(PyRep::String(s)) if s.starts_with("SelfDestructImmediate"))));
    }

    // 3. Advancing past T+120s triggers 1 Hz timer expiry -> emits SelfDestructImmediate + SelfDestructImmediateOther once.
    let t119 = ctx.now_unix();
    vclock.advance_secs(1);
    crate::sp6_lifecycle::on_advance_secs(&ctx, t119, 1);

    let cap_id = ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&ship_id));
        let ch = &snap.characters[&char_id];
        assert_eq!(snap.items[&ch.active_ship_id].type_id, 670);
        ch.active_ship_id
    });

    let pilot_notifs = ctx.drain_notifications_for_char(char_id);
    assert!(pilot_notifs.iter().any(|n| n.event == "OnSessionChanged"));
    let pilot_det: Vec<_> = pilot_notifs
        .into_iter()
        .filter(|n| n.event == "OnRemoteMessage")
        .collect();
    assert_eq!(pilot_det.len(), 1);
    assert_eq!(
        pilot_det[0].args[0],
        PyRep::String("SelfDestructImmediate".into())
    );
    assert_eq!(
        pilot_det[0].args[1].dict_get("shipTypeID"),
        Some(&PyRep::Integer(23_915))
    );

    let obs_det: Vec<_> = ctx
        .drain_notifications_for_char(obs_char_id)
        .into_iter()
        .filter(|n| n.event == "OnRemoteMessage")
        .collect();
    assert_eq!(obs_det.len(), 1);
    assert_eq!(
        obs_det[0].args[0],
        PyRep::String("SelfDestructImmediateOther".into())
    );
    assert_eq!(
        obs_det[0].args[1].dict_get("charID"),
        Some(&PyRep::Integer(char_id))
    );
    assert_eq!(
        obs_det[0].args[1].dict_get("shipTypeID"),
        Some(&PyRep::Integer(23_915))
    );

    // 4. Subsequent lifecycle tick emits zero duplicate detonation notices; aborted countdown also emits zero.
    let t120 = ctx.now_unix();
    vclock.advance_secs(1);
    crate::sp6_lifecycle::on_advance_secs(&ctx, t120, 1);
    for cid in [char_id, obs_char_id] {
        assert!(!ctx
            .drain_notifications_for_char(cid)
            .iter()
            .any(|n| n.event == "OnRemoteMessage"
                && matches!(n.args.first(), Some(PyRep::String(s)) if s.starts_with("SelfDestructImmediate"))));
    }

    let cap_ch = ctx.db.get_character(char_id).unwrap();
    crate::router::apply_session_from_character(&mut session, &cap_ch, None, Some(&ctx.sde));
    assert_eq!(session.ship_id, Some(cap_id));
    let _ = handle_wrecks_killmails(
        &ctx,
        &mut session,
        "ship",
        "SelfDestruct",
        &[PyRep::Integer(cap_id)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let _ = handle_wrecks_killmails(
        &ctx,
        &mut session,
        "ship",
        "AbortSelfDestruct",
        &[PyRep::Integer(cap_id)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let _ = ctx.drain_notifications_for_char(char_id);
    let _ = ctx.drain_notifications_for_char(obs_char_id);

    let t_abort = ctx.now_unix();
    vclock.advance_secs(120);
    crate::sp6_lifecycle::on_advance_secs(&ctx, t_abort, 120);
    assert!(ctx.db.with_snapshot(|s| s.items.contains_key(&cap_id)));
    for cid in [char_id, obs_char_id] {
        assert!(!ctx
            .drain_notifications_for_char(cid)
            .iter()
            .any(|n| n.event == "OnRemoteMessage"
                && matches!(n.args.first(), Some(PyRep::String(s)) if s.starts_with("SelfDestructImmediate"))));
    }
}

#[tokio::test]
async fn self_destruct_and_abort_validate_optional_pickid() {
    let (ctx, mut session, char_id, ship_id, _sys_id) = make_fighter_ctx_and_session();
    let kw = HashMap::new();
    let other_ship_id = ship_id + 999;

    // Mismatched ship ID rejects cleanly without creating a countdown.
    for (svc, meth) in [
        ("ship", "SelfDestruct"),
        ("ship", "AbortSelfDestruct"),
    ] {
        let err = handle_wrecks_killmails(
            &ctx,
            &mut session,
            svc,
            meth,
            &[PyRep::Integer(other_ship_id)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err();
        assert_eq!(err.code, "InvalidShip", "{svc}.{meth} mismatch error");
        assert!(err.client_error.is_some(), "{svc}.{meth} typed ClientError");
    }

    // Malformed / non-positive pickid rejects with InvalidArgument.
    for bad in [PyRep::Integer(0), PyRep::Integer(-5), PyRep::String("bad".into())] {
        let err_sd = handle_wrecks_killmails(
            &ctx,
            &mut session,
            "ship",
            "SelfDestruct",
            &[bad.clone()],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err();
        assert_eq!(err_sd.code, "InvalidArgument");

        let err_abort = handle_wrecks_killmails(
            &ctx,
            &mut session,
            "ship",
            "AbortSelfDestruct",
            &[bad],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err();
        assert_eq!(err_abort.code, "InvalidArgument");
    }

    // Matching pickid (positional & kwarg) starts countdown, mismatched Abort leaves countdown active, matching Abort cancels it.
    let ok_sd = handle_wrecks_killmails(
        &ctx,
        &mut session,
        "ship",
        "SelfDestruct",
        &[PyRep::Integer(ship_id)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(ok_sd.dict_get("countdownStarted"), Some(&PyRep::Bool(true)));

    let err_abort_mismatch = handle_wrecks_killmails(
        &ctx,
        &mut session,
        "ship",
        "AbortSelfDestruct",
        &[PyRep::Integer(other_ship_id)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err_abort_mismatch.code, "InvalidShip");
    assert!(with_combat_runtime(&ctx, |rt| {
        rt.self_destruct_countdowns.contains_key(&ship_id)
    }));

    let abort_ok = handle_wrecks_killmails(
        &ctx,
        &mut session,
        "ship",
        "AbortSelfDestruct",
        &[PyRep::Integer(ship_id)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(abort_ok.dict_get("aborted"), Some(&PyRep::Bool(true)));

    // `pickid` kwarg + omitted/None compatibility all work cleanly.
    let mut pick_kw = HashMap::new();
    pick_kw.insert("pickid".to_string(), PyRep::Integer(ship_id));
    let ok_kw = handle_wrecks_killmails(
        &ctx,
        &mut session,
        "ship",
        "SelfDestruct",
        &[],
        &pick_kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(ok_kw.dict_get("countdownStarted"), Some(&PyRep::Bool(true)));

    let abort_none = handle_wrecks_killmails(
        &ctx,
        &mut session,
        "ship",
        "AbortSelfDestruct",
        &[PyRep::None],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(abort_none.dict_get("aborted"), Some(&PyRep::Bool(true)));
    assert_eq!(ctx.db.get_character(char_id).unwrap().active_ship_id, ship_id);
}

#[tokio::test]
async fn cloaked_self_destruct_and_docked_abort_reject_without_mutating_db_or_runtime() {
    let (ctx, mut session, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    let ship_type = ctx.db.with_snapshot(|snap| snap.items[&ship_id].type_id);
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    sys_h
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: ship_type,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 200.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState::default(),
        )
        .await
        .unwrap();
    let kw = HashMap::new();
    let expected_ship_cloaked_ce =
        BridgeRpcError::from_user_key(eve_proto::UserErrorKey::ShipCloaked).client_error;

    // 1. Active module cloak (both normal and covert) rejects SelfDestruct
    //    without mutating DB epoch, commit count, runtime countdowns, or cloak state.
    for is_covert_cloak in [false, true] {
        sys_h
            .set_combat_state(
                ship_id,
                CombatState {
                    is_cloaked: true,
                    gate_cloak_remaining_ticks: 0,
                    is_covert_cloak,
                    ..CombatState::default()
                },
            )
            .await
            .unwrap();
        let epoch_0 = ctx.db.state_epoch();
        let tx_0 = ctx.db.tx_commit_count();
        let err = handle_wrecks_killmails(&ctx, &mut session, "ship", "SelfDestruct", &[], &kw)
            .await
            .unwrap()
            .expect_err("ship.SelfDestruct must reject active module cloak");
        assert_eq!(err.code, "ShipCloaked");
        assert_eq!(err.client_error, expected_ship_cloaked_ce);
        assert_eq!(ctx.db.state_epoch(), epoch_0);
        assert_eq!(ctx.db.tx_commit_count(), tx_0);
        assert!(ctx.db.with_snapshot(|s| s.sp6_lifecycle.self_destructs.is_empty()));
        assert!(with_combat_runtime(&ctx, |rt| rt.self_destruct_countdowns.is_empty()));
        assert!(sys_h.get_combat_state(ship_id).await.unwrap().unwrap().is_cloaked);
    }

    // 1b. Unsynced active cloak module in `snap.module_active_states` (`CombatState.is_cloaked == false`)
    //     must ALSO reject both SelfDestruct and CmdSelfDestruct with UserErrorKey::ShipCloaked.
    sys_h
        .set_combat_state(ship_id, CombatState::default())
        .await
        .unwrap();
    let cloak_mod_id = 99_500_777_i64;
    let cloak_tid = ctx
        .sde
        .types
        .iter()
        .find(|(_, t)| t.group_id == 330)
        .map(|(&tid, _)| tid)
        .unwrap_or(11_577);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            cloak_mod_id,
            eve_db::InventoryItemRecord {
                item_id: cloak_mod_id,
                type_id: cloak_tid,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.module_active_states.insert(cloak_mod_id);
    });
    let epoch_unsynced = ctx.db.state_epoch();
    let tx_unsynced = ctx.db.tx_commit_count();
    let err = handle_wrecks_killmails(&ctx, &mut session, "ship", "SelfDestruct", &[], &kw)
        .await
        .unwrap()
        .expect_err("unsynced active cloak module must reject self-destruct");
    assert_eq!(err.code, "ShipCloaked");
    assert_eq!(err.client_error, expected_ship_cloaked_ce);
    assert_eq!(ctx.db.state_epoch(), epoch_unsynced);
    assert_eq!(ctx.db.tx_commit_count(), tx_unsynced);
    assert!(ctx.db.with_snapshot(|s| s.sp6_lifecycle.self_destructs.is_empty()));
    assert!(with_combat_runtime(&ctx, |rt| rt.self_destruct_countdowns.is_empty()));
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_active_states.remove(&cloak_mod_id);
        snap.items.remove(&cloak_mod_id);
    });

    // 2. Gate cloak (`gate_cloak_remaining_ticks > 0`) permits SelfDestruct, and mismatched
    //    AbortSelfDestruct pickid rejects cleanly without cancelling countdown or bumping DB.
    sys_h
        .set_combat_state(
            ship_id,
            CombatState {
                is_cloaked: true,
                gate_cloak_remaining_ticks: 5,
                is_covert_cloak: false,
                ..CombatState::default()
            },
        )
        .await
        .unwrap();
    let sd_ok = handle_wrecks_killmails(&ctx, &mut session, "ship", "SelfDestruct", &[], &kw)
        .await
        .unwrap()
        .expect("gate-cloaked SelfDestruct must succeed");
    assert_eq!(sd_ok.dict_get("countdownStarted"), Some(&PyRep::Bool(true)));

    let epoch_armed = ctx.db.state_epoch();
    let tx_armed = ctx.db.tx_commit_count();
    let bad_abort = handle_wrecks_killmails(
        &ctx,
        &mut session,
        "ship",
        "AbortSelfDestruct",
        &[PyRep::Integer(ship_id + 99)],
        &kw,
    )
    .await
    .unwrap()
    .expect_err("mismatched AbortSelfDestruct pickid must reject");
    assert_eq!(bad_abort.code, "InvalidShip");
    assert_eq!(ctx.db.state_epoch(), epoch_armed);
    assert_eq!(ctx.db.tx_commit_count(), tx_armed);

    // 3. Docked session OR DB-docked character must reject SelfDestruct and
    //    AbortSelfDestruct cleanly before mutating DB or cancelling armed state.
    let expected_docked_code = "CannotSelfDestructWhileDocked";
    session.station_id = Some(60_003_760);
    for meth in ["SelfDestruct", "AbortSelfDestruct"] {
        let dock_err = handle_wrecks_killmails(&ctx, &mut session, "ship", meth, &[], &kw)
            .await
            .unwrap()
            .expect_err(&format!("docked {meth} must fail"));
        assert_eq!(dock_err.code, expected_docked_code);
        assert_eq!(ctx.db.state_epoch(), epoch_armed);
        assert_eq!(ctx.db.tx_commit_count(), tx_armed);
    }
    session.station_id = None;

    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().station_id = Some(60_003_760);
    });
    let epoch_db_dock = ctx.db.state_epoch();
    let tx_db_dock = ctx.db.tx_commit_count();
    for (svc, meth) in [("ship", "SelfDestruct"), ("ship", "AbortSelfDestruct")] {
        let db_dock_err = handle_wrecks_killmails(&ctx, &mut session, svc, meth, &[], &kw)
            .await
            .unwrap()
            .expect_err(&format!("DB-docked ({svc}, {meth}) must fail"));
        assert_eq!(db_dock_err.code, expected_docked_code);
        assert_eq!(ctx.db.state_epoch(), epoch_db_dock);
        assert_eq!(ctx.db.tx_commit_count(), tx_db_dock);
    }

    // 4. Undock in DB -> AbortSelfDestruct cancels the armed countdown.
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().station_id = None;
    });
    let abort_ok = handle_wrecks_killmails(&ctx, &mut session, "ship", "AbortSelfDestruct", &[], &kw)
        .await
        .unwrap()
        .expect("in-space AbortSelfDestruct must succeed");
    assert_eq!(abort_ok.dict_get("aborted"), Some(&PyRep::Bool(true)));
}
