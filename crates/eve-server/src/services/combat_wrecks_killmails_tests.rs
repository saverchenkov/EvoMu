//! Unit and mutation-killing tests for `combat_wrecks_killmails`.

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
    with_combat_runtime(&ctx, |rt| {
        let c = rt.insurance_contracts.get(&ship_id).unwrap();
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

#[tokio::test]
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
    let km_hash = with_combat_runtime(&ctx, |rt| {
        rt.detailed_killmails.get(&km_id).unwrap().killmail_hash.clone()
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
        &[PyRep::Integer(km_id), PyRep::String(km_hash)],
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
}
