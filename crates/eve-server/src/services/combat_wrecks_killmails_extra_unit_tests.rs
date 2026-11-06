//! Extra unit and mutation-killing tests for `services::combat_crimewatch::combat_wrecks_killmails`.

#![forbid(unsafe_code)]

use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::{
    make_fighter_ctx_and_session, make_fighter_item, make_fighter_type,
};
use crate::services::combat_crimewatch::combat_wrecks_killmails::*;
use crate::services::combat_crimewatch::{
    with_combat_runtime, with_mut_combat_runtime, DetailedKillmail,
    KillmailAttackerEntry, KillmailItemEntry, SELF_DESTRUCT_COUNTDOWN_SECS,
};
use crate::services::inventory_fitting::fitting_containers_ships::{
    write_container_ship_state, InsuranceContractRecord,
};
use eve_db::{
    CorpWalletDivisionRecord, CorporationRecord, InventoryFlag, IskCents, KillmailRecord,
    PersistedSpaceObjectMeta,
};
use eve_destiny::{Ball, CombatState, MovementMode};
use eve_proto::PyRep;
use glam::DVec3;
use std::collections::HashMap;
use std::sync::Arc;

#[test]
fn corp_insurance_payout_contract_sync_voiding_assist_aggregation_and_destruction_value() {
    let (ctx, _session, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    let now = ctx.now_unix();
    let corp_id = 98_500_001i64;
    let repacked_ship_id = 880_101i64;
    let expired_ship_id = 880_102i64;
    let standard_ship_id = 880_103i64;

    let (ref_val, _) = compute_ship_insurance_quote_levels(&ctx, 23_915);

    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.insert(
            corp_id,
            CorporationRecord {
                corporation_id: corp_id,
                name: "Insured Corp".into(),
                ticker: "INSC".into(),
                ceo_id: char_id,
                master_wallet: IskCents::from_isk(1_000_000),
                wallet_divisions: vec![CorpWalletDivisionRecord {
                    division_id: 1,
                    name: "Master".into(),
                    balance: IskCents::from_isk(1_000_000),
                }],
                ..Default::default()
            },
        );
        // Unassembled ship with stale contract -> should be voided on sync
        let mut repacked = make_fighter_item(repacked_ship_id, 23_915, char_id, 60_003_760, InventoryFlag::Hangar, 1);
        repacked.singleton = false;
        repacked.insured_payout_isk = ref_val;
        snap.items.insert(repacked_ship_id, repacked);

        // Expired contract ship -> should be voided on sync
        let mut expired = make_fighter_item(expired_ship_id, 23_915, char_id, 60_003_760, InventoryFlag::Hangar, 1);
        expired.insured_payout_isk = ref_val;
        snap.items.insert(expired_ship_id, expired);

        // Standard-level insured ship (0.5 * ref_val < 0.99 * ref_val)
        let mut std_ship = make_fighter_item(standard_ship_id, 23_915, char_id, 60_003_760, InventoryFlag::Hangar, 1);
        std_ship.insured_payout_isk = ref_val * 0.5;
        snap.items.insert(standard_ship_id, std_ship);
    });

    write_container_ship_state(&ctx, |st| {
        st.insurance_contracts.insert(
            repacked_ship_id,
            InsuranceContractRecord {
                ship_id: repacked_ship_id,
                ship_type_id: 23_915,
                owner_id: char_id,
                level_name: "Platinum".into(),
                level_fraction: 1.0,
                cost_isk: 100.0,
                payout_isk: ref_val,
                start_unix: now - 100,
                end_unix: now + 10_000,
            },
        );
        st.insurance_contracts.insert(
            expired_ship_id,
            InsuranceContractRecord {
                ship_id: expired_ship_id,
                ship_type_id: 23_915,
                owner_id: char_id,
                level_name: "Platinum".into(),
                level_fraction: 1.0,
                cost_isk: 100.0,
                payout_isk: ref_val,
                start_unix: now - 10_000,
                end_unix: now,
            },
        );
        st.insurance_contracts.insert(
            ship_id,
            InsuranceContractRecord {
                ship_id,
                ship_type_id: 23_915,
                owner_id: corp_id,
                level_name: "Platinum".into(),
                level_fraction: 1.0,
                cost_isk: 100_000.0,
                payout_isk: 5_000_000.0,
                start_unix: now,
                end_unix: now + INSURANCE_CONTRACT_DURATION_SECS,
            },
        );
    });
    ctx.db.with_mut_snapshot(|snap| {
        let s = snap.items.get_mut(&ship_id).unwrap();
        s.owner_id = corp_id;
        s.insured_payout_isk = 5_000_000.0;
    });

    sync_ship_insurance_contracts(&ctx, 0);
    with_combat_runtime(&ctx, |rt| {
        assert!(!rt.insurance_contracts.contains_key(&repacked_ship_id));
        assert!(!rt.insurance_contracts.contains_key(&expired_ship_id));
        assert_eq!(rt.insurance_contracts[&standard_ship_id].level_name, "Standard");
        assert_eq!(rt.insurance_contracts[&ship_id].owner_id, corp_id);
    });

    // Corporate insurance payout credits corp_id master_wallet & division 1, and sends mail to victim_char_id
    let paid = process_ship_insurance_payout(&ctx, char_id, ship_id, 23_915, false);
    assert_eq!(paid, 5_000_000.0);
    ctx.db.with_snapshot(|snap| {
        assert_eq!(
            snap.corporations[&corp_id].master_wallet,
            IskCents::from_isk(6_000_000)
        );
        assert!(snap
            .wallet_journal
            .iter()
            .any(|j| j.character_id == corp_id && j.entry_type == "insurance_payout"));
    });

    // Restore ship owner to char_id
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&ship_id).unwrap().owner_id = char_id;
    });

    // Assist action aggregation + negative damage clamp + drone assist (cat 18, ship_type == weapon_type)
    record_killmail_assist_action(&ctx, ship_id, 90_000_077, 2_456, 2_456, 150.0, false, now);
    record_killmail_assist_action(&ctx, ship_id, 90_000_077, 23_915, 2_905, -50.0, true, now + 5);
    record_killmail_assist_action(&ctx, ship_id, 90_000_077, 23_915, 2_905, 200.0, false, now + 10);
    with_combat_runtime(&ctx, |rt| {
        let list = &rt.aggression_assist_log[&ship_id];
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].damage_done, 350.0);
        assert!(list[0].is_ewar);
        assert_eq!(list[0].last_action_unix, now + 10);
    });

    // process_ship_destruction_with_aggression sums hull + fitted item values
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            880_199,
            make_fighter_item(880_199, 23_915, char_id, ship_id, InventoryFlag::HiSlot0, 2),
        );
    });
    let km_id = process_ship_destruction_with_aggression(
        &ctx,
        char_id,
        ship_id,
        23_915,
        90_000_077,
        2_905,
        sys_id,
        DVec3::ZERO,
        false,
    );
    ctx.db.with_snapshot(|snap| {
        let km = &snap.killmails[&km_id];
        assert_eq!(km.victim_character_id, char_id);
        assert_eq!(km.attacker_character_id, 90_000_077);
        assert!(km.total_value_isk > 250_000.0);
    });
}

#[tokio::test]
async fn bubble_decay_precursor_deactivate_loot_all_split_and_killmail_variants() {
    let (mut ctx, mut session, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    let kw = HashMap::new();
    let now = ctx.now_unix();

    let mut sde = (*ctx.sde).clone();
    // Group 361: Mobile Warp Disruptor
    sde.types.insert(12_198, make_fighter_type(12_198, 361, 22, 100.0, vec![]));
    // Group 2018: Precursor armor repairer with base attr 84 = 120.0
    sde.types.insert(49_730, make_fighter_type(49_730, 2018, 7, 5.0, vec![(84, 120.0)]));
    // Group 375: Mining crystal with hp attr 9 = 1.0
    sde.types.insert(18_068, make_fighter_type(18_068, 375, 8, 1.0, vec![(9, 1.0)]));
    // Group 1246: Mobile Depot; Group 340: Secure Container with effect 649
    sde.types.insert(33_474, make_fighter_type(33_474, 1246, 22, 50.0, vec![]));
    let mut sec_cont = make_fighter_type(17_366, 340, 2, 100.0, vec![]);
    sec_cont.capacity = 1_000.0;
    sec_cont.effect_ids = vec![649];
    sde.types.insert(17_366, sec_cont);
    sde.types.insert(26_557, make_fighter_type(26_557, 186, 2, 100.0, vec![]));
    sde.types.insert(23, make_fighter_type(23, 12, 2, 100.0, vec![]));
    sde.types.insert(34, make_fighter_type(34, 18, 4, 100.0, vec![])); // 100 m3 per unit
    // Set active ship capacity to 250 m3 so a 5-unit stack of 100 m3 splits 2 looted + 3 remaining!
    if let Some(ship_t) = sde.types.get_mut(&23_915) {
        ship_t.capacity = 250.0;
    }
    ctx.sde = Arc::new(sde);

    // 1. Secure container lifetime + Mobile Warp Disruptor decay + child item cleanup
    let sec_cont_id = 890_001i64;
    let bubble_id = 890_002i64;
    let bubble_child_id = 890_003i64;
    register_container_lifetime(&ctx, sec_cont_id, sys_id, 17_366, char_id, now, false);
    with_combat_runtime(&ctx, |rt| {
        assert_eq!(
            rt.container_lifetimes[&sec_cont_id].lifetime_secs,
            crate::sp6_lifecycle::SECURE_CONTAINER_DECAY_SECS
        );
    });
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            bubble_id,
            make_fighter_item(bubble_id, 12_198, char_id, sys_id as i64, InventoryFlag::None, 1),
        );
        snap.items.insert(
            bubble_child_id,
            make_fighter_item(bubble_child_id, 34, char_id, bubble_id, InventoryFlag::Cargo, 1),
        );
        snap.sp6_lifecycle.space_object_meta.insert(
            bubble_id,
            PersistedSpaceObjectMeta {
                anchor_start_unix: Some(100),
                created_unix: Some(100),
                ..Default::default()
            },
        );
    });
    let expired = step_wreck_and_jetcan_decay(&ctx, sys_id, now).await;
    assert!(expired.contains(&bubble_id));
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&bubble_id));
        assert!(!snap.items.contains_key(&bubble_child_id));
    });

    // 2. dogmaIM.Deactivate on unowned module vs Precursor repairer (group 2018) + group 375 crystal damage
    assert_eq!(
        handle_wrecks_killmails(&ctx, &mut session, "dogmaIM", "Deactivate", &[PyRep::Integer(999_999)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "ModuleNotOwnedByShip"
    );
    let prec_mod_id = 890_010i64;
    let min_crys_id = 890_011i64;
    ctx.db.with_mut_snapshot(|snap| {
        let mut prec = make_fighter_item(prec_mod_id, 49_730, char_id, ship_id, InventoryFlag::LoSlot0, 1);
        prec.dynamic_attributes.insert(84, 300.0); // spooled up
        snap.items.insert(prec_mod_id, prec);
        let mut mc = make_fighter_item(min_crys_id, 18_068, char_id, ship_id, InventoryFlag::Cargo, 1);
        mc.dynamic_attributes.insert(3, 5.0); // 5.0 / 1.0 capped at 1.0!
        snap.items.insert(min_crys_id, mc);
    });
    handle_wrecks_killmails(&ctx, &mut session, "dogmaIM", "Deactivate", &[PyRep::Integer(prec_mod_id)], &kw)
        .await
        .unwrap()
        .unwrap();
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&prec_mod_id].dynamic_attributes.get(&84), Some(&120.0));
    });
    let crys_dict = handle_wrecks_killmails(&ctx, &mut session, "invbroker", "GetDamageForCrystals", &[], &kw)
        .await
        .unwrap()
        .unwrap();
    if let PyRep::Dict(entries) = crys_dict {
        assert!(entries.contains(&(PyRep::Integer(min_crys_id), PyRep::Real(1.0))));
    }

    // 3. LootAll error branches, password-locked container, foreign depot, stack split, and empty jetcan despawn
    assert_eq!(
        handle_wrecks_killmails(&ctx, &mut session, "invbroker", "LootAll", &[], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidArgument"
    );
    assert_eq!(
        handle_wrecks_killmails(&ctx, &mut session, "invbroker", "LootAll", &[PyRep::Integer(999_888)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "ContainerNotFound"
    );
    // Active ship is category 6 -> InvalidContainerType
    assert_eq!(
        handle_wrecks_killmails(&ctx, &mut session, "invbroker", "LootAll", &[PyRep::Integer(ship_id)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidContainerType"
    );

    let depot_id = 890_020i64;
    let locked_sc_id = 890_021i64;
    let split_wreck_id = 890_022i64;
    let split_stack_id = 890_023i64;
    let jetcan_id = 890_024i64;
    let jetcan_loot_id = 890_025i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            depot_id,
            make_fighter_item(depot_id, 33_474, 90_000_999, sys_id as i64, InventoryFlag::None, 1),
        );
        snap.items.insert(
            locked_sc_id,
            make_fighter_item(locked_sc_id, 17_366, 90_000_999, sys_id as i64, InventoryFlag::None, 1),
        );
        snap.items.insert(
            split_wreck_id,
            make_fighter_item(split_wreck_id, 26_557, 0, sys_id as i64, InventoryFlag::None, 1), // owner_id 0 -> abandoned
        );
        snap.items.insert(
            split_stack_id,
            make_fighter_item(split_stack_id, 34, 0, split_wreck_id, InventoryFlag::Cargo, 5), // 5 * 100 m3 = 500 m3 > 250 m3
        );
        snap.items.insert(
            jetcan_id,
            make_fighter_item(jetcan_id, 23, char_id, sys_id as i64, InventoryFlag::None, 1),
        );
        snap.items.insert(
            jetcan_loot_id,
            make_fighter_item(jetcan_loot_id, 18_068, char_id, jetcan_id, InventoryFlag::Cargo, 1), // 1 m3
        );
    });
    write_container_ship_state(&ctx, |st| {
        st.container_passwords.insert(locked_sc_id, "secret".into());
    });

    assert_eq!(
        handle_wrecks_killmails(&ctx, &mut session, "invbroker", "LootAll", &[PyRep::Integer(depot_id)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "AccessDenied"
    );
    assert_eq!(
        handle_wrecks_killmails(&ctx, &mut session, "invbroker", "LootAll", &[PyRep::Integer(locked_sc_id)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "ContainerLocked"
    );

    // Out-of-range check when ship and wreck balls are > 2500m apart
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
    sys_handle
        .spawn_ball(Ball {
            id: split_wreck_id,
            type_id: 26_557,
            position: DVec3::new(3_000.0, 0.0, 0.0),
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
    assert_eq!(
        handle_wrecks_killmails(&ctx, &mut session, "invbroker", "LootAll", &[PyRep::Integer(split_wreck_id)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "OutOfRange"
    );

    // Move wreck within 2500m -> partial stack split (2 units looted, 3 units remain) & abandoned = true
    let _ = sys_handle.set_ball_position(split_wreck_id, DVec3::new(1_000.0, 0.0, 0.0)).await;
    let split_res = handle_wrecks_killmails(&ctx, &mut session, "invbroker", "LootAll", &[PyRep::Integer(split_wreck_id)], &kw)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(split_res.dict_get("abandoned"), Some(&PyRep::Bool(true)));
    assert_eq!(split_res.dict_get("suspectFlagTriggered"), Some(&PyRep::Bool(false)));
    assert_eq!(
        split_res.dict_get("remainingItemIDs"),
        Some(&PyRep::List(vec![PyRep::Integer(split_stack_id)]))
    );
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&split_stack_id].quantity, 3);
    });

    // Looting all items from jetcan (type 23) despawns the empty jetcan
    sys_handle
        .spawn_ball(Ball {
            id: jetcan_id,
            type_id: 23,
            position: DVec3::new(500.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 100.0,
            radius: 10.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await
        .unwrap();
    handle_wrecks_killmails(&ctx, &mut session, "invbroker", "LootAll", &[PyRep::Integer(jetcan_id)], &kw)
        .await
        .unwrap()
        .unwrap();
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&jetcan_id));
    });

    // 4. SelfDestruct while docked vs in-progress countdown + GetKillMail basic & detailed with items
    session.station_id = Some(60_003_760);
    assert_eq!(
        handle_wrecks_killmails(&ctx, &mut session, "ship", "SelfDestruct", &[], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "CannotSelfDestructWhileDocked"
    );
    session.station_id = None;

    let sd1 = handle_wrecks_killmails(&ctx, &mut session, "ship", "SelfDestruct", &[], &kw)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        sd1.dict_get("remainingSeconds"),
        Some(&PyRep::Integer(SELF_DESTRUCT_COUNTDOWN_SECS as i64))
    );
    // Calling again while now_unix < detonate_at_unix returns existing countdown
    let sd2 = handle_wrecks_killmails(&ctx, &mut session, "ship", "SelfDestruct", &[], &kw)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(sd2.dict_get("detonated"), Some(&PyRep::Bool(false)));

    // Basic killmail fallback in GetKillMail + DetailedKillmail with dropped/destroyed items
    let basic_km_id = 777_001i64;
    let det_km_id = 777_002i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.killmails.insert(
            basic_km_id,
            KillmailRecord {
                kill_id: basic_km_id,
                victim_character_id: char_id,
                attacker_character_id: 90_000_077,
                solar_system_id: sys_id,
                ship_type_id: 23_915,
                total_value_isk: 500_000.0,
                killed_unix: now,
                ..Default::default()
            },
        );
    });
    let basic_hash = compute_killmail_esi_hash(char_id, 90_000_077, 23_915, now);
    assert_eq!(
        handle_wrecks_killmails(
            &ctx,
            &mut session,
            "warStatisticMgr",
            "GetKillMail",
            &[PyRep::Integer(basic_km_id), PyRep::String("bad".into())],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidKillmailHash"
    );
    let basic_rep = handle_wrecks_killmails(
        &ctx,
        &mut session,
        "warStatisticMgr",
        "GetKillMail",
        &[PyRep::Integer(basic_km_id), PyRep::String(basic_hash)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(basic_rep.dict_get("killID"), Some(&PyRep::Integer(basic_km_id)));

    with_mut_combat_runtime(&ctx, |rt| {
        rt.detailed_killmails.insert(
            det_km_id,
            DetailedKillmail {
                kill_id: det_km_id,
                kill_time_unix: now,
                solar_system_id: sys_id,
                killmail_hash: "dethash".into(),
                victim_character_id: char_id,
                victim_corporation_id: 1_000_009,
                victim_alliance_id: Some(99_000_001),
                victim_ship_type_id: 23_915,
                victim_damage_taken: 1_500.0,
                victim_position: [1.0, 2.0, 3.0],
                attackers: vec![KillmailAttackerEntry {
                    character_id: 90_000_077,
                    corporation_id: 1_000_009,
                    alliance_id: Some(99_000_001),
                    ship_type_id: 23_915,
                    weapon_type_id: 2_905,
                    damage_done: 1_500.0,
                    final_blow: true,
                    security_status: 1.5,
                }],
                dropped_items: vec![KillmailItemEntry {
                    type_id: 34,
                    flag: 5,
                    qty_dropped: 10,
                    qty_destroyed: 0,
                    singleton: false,
                }],
                destroyed_items: vec![KillmailItemEntry {
                    type_id: 34,
                    flag: 5,
                    qty_dropped: 0,
                    qty_destroyed: 5,
                    singleton: false,
                }],
                total_value_isk: 750_000.0,
            },
        );
    });
    let det_rep = handle_wrecks_killmails(
        &ctx,
        &mut session,
        "warStatisticMgr",
        "GetKillMail",
        &[PyRep::Integer(det_km_id), PyRep::String("dethash".into())],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(det_rep.dict_get("victimAllianceID"), Some(&PyRep::Integer(99_000_001)));
}
