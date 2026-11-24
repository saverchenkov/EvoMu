use super::combat_weapons_spool_tests::{make_fighter_item, make_spool_test_ctx, spawn_spool_ball, InventoryFlag};
use super::*;
use crate::services::combat_crimewatch::combat_fighters::fighter_test_helpers;
use glam::DVec3;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn handle_destruction_non_ship_npc_bounty_and_battleship_with_escape_bay() {
    let (ctx, _, attacker_id, attacker_ship, sys_id) = make_spool_test_ctx();
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let now_unix = ctx.now_unix();

    // 1. Non-ship object returns zeroed tuple immediately
    let non_ship = 660_010_001i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            non_ship,
            make_fighter_item(non_ship, 2_456, attacker_id, sys_id as i64, InventoryFlag::None, 1),
        );
    });
    let non_ship_ball = spawn_spool_ball(&ctx, sys_id, non_ship, 2_456, DVec3::ZERO, 100.0, 0.0, 100.0, 0.0).await;
    let res_ns = handle_ship_or_npc_destruction(
        &ctx,
        &sys_handle,
        attacker_id,
        attacker_ship,
        47_271,
        47_240,
        non_ship,
        &non_ship_ball,
        None,
        sys_id,
        150.0,
        now_unix,
    )
    .await;
    assert_eq!(res_ns.kill_id, 0);
    assert_eq!(res_ns.wreck_id, 0);
    assert_eq!(res_ns.corpse_id, 0);
    assert!(res_ns.killmail_hash.is_empty());

    // 2. NPC pirate destruction credits pending bounty, updates sec status, and records killmail
    let npc_id = 1_895_000_123i64;
    let npc_ball = spawn_spool_ball(&ctx, sys_id, npc_id, 500_001, DVec3::new(1_000.0, 0.0, 0.0), 0.0, 0.0, 10.0, 0.0).await;
    let before_sec = ctx.db.with_snapshot(|s| s.characters.get(&attacker_id).map(|c| c.security_status).unwrap());
    let ShipOrNpcDestructionResult {
        kill_id: npc_km,
        wreck_id: npc_wreck,
        corpse_id: npc_corpse,
        killmail_hash: npc_hash,
    } = handle_ship_or_npc_destruction(
        &ctx,
        &sys_handle,
        attacker_id,
        attacker_ship,
        47_271,
        47_240,
        npc_id,
        &npc_ball,
        None,
        sys_id,
        1_250.0,
        now_unix,
    )
    .await;
    assert!(npc_km > 0 && npc_wreck > 0 && npc_corpse == 0 && !npc_hash.is_empty());
    let after_sec = ctx.db.with_snapshot(|s| s.characters.get(&attacker_id).map(|c| c.security_status).unwrap());
    assert!(after_sec >= before_sec);

    // 3. Player Battleship (24690) with Frigate Escape Bay (11393), fitted module, gift wrap (3468), and courier contract
    let victim_id = 90_000_002i64;
    let victim_bs = 200_000_777i64;
    let esc_frig = 200_000_778i64;
    let cargo_in_bs = 750_000_001i64;
    let wrap_in_bs = 750_000_002i64;
    let wrapped_item = 750_000_003i64;
    let hi_mod = 750_000_004i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(victim_bs, make_fighter_item(victim_bs, 24_690, victim_id, sys_id as i64, InventoryFlag::None, 1));
        snap.items.insert(esc_frig, make_fighter_item(esc_frig, 11_393, victim_id, victim_bs, InventoryFlag::from_u16(177), 1));
        snap.items.insert(cargo_in_bs, make_fighter_item(cargo_in_bs, 587, victim_id, victim_bs, InventoryFlag::Cargo, 1));
        snap.items.insert(wrap_in_bs, make_fighter_item(wrap_in_bs, 3_468, victim_id, victim_bs, InventoryFlag::Cargo, 1));
        snap.items.insert(wrapped_item, make_fighter_item(wrapped_item, 587, victim_id, wrap_in_bs, InventoryFlag::Cargo, 1));
        snap.items.insert(hi_mod, make_fighter_item(hi_mod, 47_240, victim_id, victim_bs, InventoryFlag::HiSlot0, 1));
        snap.contracts.insert(
            99_777,
            eve_db::ContractRecord {
                contract_id: 99_777,
                issuer_id: victim_id,
                acceptor_id: Some(victim_id),
                contract_type: "Courier".into(),
                status: "InProgress".into(),
                start_station_id: 60_003_760,
                end_station_id: 60_003_761,
                reward: eve_db::IskCents(50_000_000),
                collateral: eve_db::IskCents(100_000_000),
                item_ids: vec![cargo_in_bs],
                expiration_unix: now_unix + 86_400,
                assignee_id: victim_id,
                date_issued_unix: now_unix,
                title: "BS Courier".into(),
                ..Default::default()
            },
        );
        if let Some(ch) = snap.characters.get_mut(&victim_id) {
            ch.active_ship_id = victim_bs;
            ch.solar_system_id = sys_id;
        }
    });
    let bs_ball = spawn_spool_ball(&ctx, sys_id, victim_bs, 24_690, DVec3::new(3_000.0, 0.0, 0.0), 10.0, 10.0, 10.0, 100.0).await;
    let ShipOrNpcDestructionResult {
        kill_id: bs_km,
        wreck_id: bs_wreck,
        corpse_id: bs_corpse,
        ..
    } = handle_ship_or_npc_destruction(
        &ctx,
        &sys_handle,
        attacker_id,
        attacker_ship,
        47_271,
        47_240,
        victim_bs,
        &bs_ball,
        Some((victim_id, victim_bs)),
        sys_id,
        9_500.0,
        now_unix,
    )
    .await;
    assert!(bs_km > 0 && bs_wreck > 0 && bs_corpse == 0);
    // Victim now pilots escape frigate (esc_frig) & courier contract is Failed
    let (active_after_bs, bs_contract_status) = ctx.db.with_snapshot(|s| {
        (
            s.characters.get(&victim_id).map(|c| c.active_ship_id),
            s.contracts.get(&99_777).map(|c| c.status.clone()),
        )
    });
    assert_eq!(active_after_bs, Some(esc_frig));
    assert_eq!(bs_contract_status.as_deref(), Some("Failed"));

    // 4. Capsule pod (670) destruction destroys implants, spawns corpse (type 25), and respawns pod at home station
    let pod_id = 200_000_888i64;
    let imp_item = 750_000_010i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(pod_id, make_fighter_item(pod_id, 670, victim_id, sys_id as i64, InventoryFlag::None, 1));
        snap.items.insert(imp_item, make_fighter_item(imp_item, 9_941, victim_id, victim_id, InventoryFlag::Implant, 1));
        snap.active_implants.insert(victim_id, vec![9_942]);
        if let Some(ch) = snap.characters.get_mut(&victim_id) {
            ch.active_ship_id = pod_id;
            ch.solar_system_id = sys_id;
        }
    });
    let pod_ball = spawn_spool_ball(&ctx, sys_id, pod_id, 670, DVec3::new(4_000.0, 0.0, 0.0), 0.0, 0.0, 5.0, 0.0).await;
    let ShipOrNpcDestructionResult {
        kill_id: pod_km,
        wreck_id: pod_wreck,
        corpse_id: pod_corpse,
        ..
    } = handle_ship_or_npc_destruction(
        &ctx,
        &sys_handle,
        attacker_id,
        attacker_ship,
        47_271,
        47_240,
        pod_id,
        &pod_ball,
        Some((victim_id, pod_id)),
        sys_id,
        800.0,
        now_unix,
    )
    .await;
    assert!(pod_km > 0 && pod_wreck == 0 && pod_corpse > 0);
    let (corpse_exists, imps_cleared) = ctx.db.with_snapshot(|s| {
        (
            s.items.get(&pod_corpse).is_some_and(|i| i.type_id == 25),
            !s.items.contains_key(&imp_item),
        )
    });
    assert!(corpse_exists && imps_cleared);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn weapon_rpc_ship_and_capsule_kills_route_through_death_pipeline_with_single_atomic_insurance() {
    let (ctx, _, attacker_id, attacker_ship, sys_id) = make_spool_test_ctx();
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let now_unix = ctx.now_unix();
    let victim_id = 90_000_002i64;
    let victim_ship = 660_020_001i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            victim_ship,
            make_fighter_item(victim_ship, 587, victim_id, sys_id as i64, InventoryFlag::None, 1),
        );
        if let Some(ch) = snap.characters.get_mut(&victim_id) {
            ch.active_ship_id = victim_ship;
            ch.solar_system_id = sys_id;
        }
        snap.insurance_contracts.insert(
            victim_ship,
            eve_db::PersistedInsuranceContractRecord {
                ship_id: victim_ship,
                ship_type_id: 587,
                owner_id: victim_id,
                level_name: "Platinum".into(),
                level_fraction: 1.0,
                cost_isk: 150_000.0,
                payout_isk: 654_321.0,
                start_unix: now_unix.saturating_sub(60),
                end_unix: now_unix + 3_600,
            },
        );
    });
    let ship_ball = spawn_spool_ball(
        &ctx,
        sys_id,
        victim_ship,
        587,
        DVec3::new(2_500.0, 0.0, 0.0),
        0.0,
        0.0,
        10.0,
        0.0,
    )
    .await;

    let (bal_before, journal_before, mails_before, km_before) = ctx.db.with_snapshot(|snap| {
        (
            snap.characters[&victim_id].balance,
            snap.wallet_journal.len(),
            snap.mails.len(),
            snap.killmails.len(),
        )
    });

    let ship_res = handle_ship_or_npc_destruction(
        &ctx,
        &sys_handle,
        attacker_id,
        attacker_ship,
        47_271,
        47_240,
        victim_ship,
        &ship_ball,
        Some((victim_id, victim_ship)),
        sys_id,
        1_200.0,
        now_unix,
    )
    .await;
    assert!(ship_res.kill_id > 0);
    assert!(ship_res.wreck_id > 0);
    assert_eq!(ship_res.corpse_id, 0);
    assert!(!ship_res.killmail_hash.is_empty());

    let pod_id = ctx.db.with_snapshot(|snap| {
        assert_eq!(
            snap.characters[&victim_id].balance,
            bal_before
                .checked_add(eve_db::IskCents::from_f64_isk(654_321.0))
                .unwrap()
        );
        assert!(!snap.items.contains_key(&victim_ship));
        assert!(!snap.insurance_contracts.contains_key(&victim_ship));
        assert!(snap
            .committed_idempotency_keys
            .contains(&(victim_ship, "ship_insurance_payout".to_string())));
        assert_eq!(snap.wallet_journal.len(), journal_before + 1);
        assert_eq!(snap.mails.len(), mails_before + 1);
        assert_eq!(snap.killmails.len(), km_before + 1);
        let active = snap.characters[&victim_id].active_ship_id;
        assert_ne!(active, victim_ship);
        assert_eq!(snap.items[&active].type_id, 670);
        active
    });

    // Duplicate kill call for the already-destroyed ship is a no-op with zero double payout.
    let dup_res = handle_ship_or_npc_destruction(
        &ctx,
        &sys_handle,
        attacker_id,
        attacker_ship,
        47_271,
        47_240,
        victim_ship,
        &ship_ball,
        Some((victim_id, victim_ship)),
        sys_id,
        1_200.0,
        now_unix,
    )
    .await;
    assert_eq!(dup_res.kill_id, 0);
    assert_eq!(dup_res.wreck_id, 0);
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.killmails.len(), km_before + 1);
        assert_eq!(snap.wallet_journal.len(), journal_before + 1);
    });

    // Kill the ejected capsule through the same entry point; routes via death_pipeline with 0 insurance.
    let pod_ball = sys_handle
        .get_ball_snapshot(pod_id)
        .await
        .ok()
        .flatten()
        .map(|(_, b)| b)
        .expect("ejected capsule ball spawned by death_pipeline");
    let bal_after_ship = ctx.db.with_snapshot(|s| s.characters[&victim_id].balance);
    let pod_res = handle_ship_or_npc_destruction(
        &ctx,
        &sys_handle,
        attacker_id,
        attacker_ship,
        47_271,
        47_240,
        pod_id,
        &pod_ball,
        Some((victim_id, pod_id)),
        sys_id,
        500.0,
        now_unix,
    )
    .await;
    assert!(pod_res.kill_id > 0);
    assert_eq!(pod_res.wreck_id, 0);
    assert!(pod_res.corpse_id > 0);
    assert!(!pod_res.killmail_hash.is_empty());
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.characters[&victim_id].balance, bal_after_ship);
        assert_eq!(snap.items.get(&pod_res.corpse_id).map(|i| i.type_id), Some(25));
        assert!(snap.characters[&victim_id].station_id.is_some());
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn weapon_activate_destroys_mobile_warp_disruptor_mobile_depot_and_fighter() {
    use crate::services::combat_crimewatch::combat_weapons::handle_weapons;
    use crate::services::combat_crimewatch::SafetyLevel;
    use crate::services::mobile_deployables::{
        read_deployable_state_snap, write_deployable_state_snap, MobileDeployableRecord,
        GROUP_MOBILE_DEPOT,
    };
    use eve_destiny::{CombatState, TargetLock};

    let (mut ctx, mut session, attacker_id, attacker_ship, sys_id) = make_spool_test_ctx();
    let victim_id = 90_000_002i64;
    let mwd_type_id = 12_198u32; // Mobile Small Warp Disruptor I (cat 22, group 361)
    let depot_type_id = 33_474u32; // Mobile Depot (cat 22, group 1246)
    let fighter_type_id = 23_057u32; // Templar I (cat 65, group 1652)
    let gun_type_id = 47_240u32;

    let mut sde = (*ctx.sde).clone();
    // Make 47_240 a civilian-capacity turret with huge damage multiplier for clean single-shot kills.
    if let Some(t) = sde.types.get_mut(&gun_type_id) {
        t.capacity = 0.0;
        t.default_attributes.insert(64, 500.0); // damageMultiplier
        t.default_attributes.insert(114, 100.0); // emDamage fallback
        t.default_attributes.insert(54, 50_000.0);
        t.default_attributes.insert(158, 25_000.0);
        t.default_attributes.insert(160, 1.0);
        t.default_attributes.insert(51, 50.0); // duration 50ms
    }
    sde.types.insert(
        mwd_type_id,
        super::combat_weapons_spool_tests::make_fighter_type(
            mwd_type_id,
            361,
            22,
            2_500_000.0,
            vec![(263, 10.0), (265, 10.0), (9, 10.0)],
        ),
    );
    sde.types.insert(
        depot_type_id,
        super::combat_weapons_spool_tests::make_fighter_type(
            depot_type_id,
            GROUP_MOBILE_DEPOT,
            22,
            1_500_000.0,
            vec![(263, 10.0), (265, 10.0), (9, 10.0)],
        ),
    );
    sde.types.insert(
        fighter_type_id,
        super::combat_weapons_spool_tests::make_fighter_type(
            fighter_type_id,
            1652,
            65,
            100_000.0,
            vec![(9, 10.0)],
        ),
    );
    ctx.sde = Arc::new(sde);

    let now_unix = ctx.now_unix();
    let gun_id = 770_001i64;
    let gun_depot_id = 770_002i64;
    let gun_fighter_id = 770_003i64;
    let mwd_id = 770_101i64;
    let mwd_cargo_id = 770_102i64;
    let depot_id = 770_201i64;
    let fighter_id = 770_301i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            attacker_ship,
            make_fighter_item(attacker_ship, 47_271, attacker_id, sys_id as i64, InventoryFlag::None, 1),
        );
        snap.items.insert(
            gun_id,
            make_fighter_item(gun_id, gun_type_id, attacker_id, attacker_ship, InventoryFlag::HiSlot0, 1),
        );
        snap.items.insert(
            gun_depot_id,
            make_fighter_item(gun_depot_id, gun_type_id, attacker_id, attacker_ship, InventoryFlag::HiSlot1, 1),
        );
        snap.items.insert(
            gun_fighter_id,
            make_fighter_item(gun_fighter_id, gun_type_id, attacker_id, attacker_ship, InventoryFlag::HiSlot2, 1),
        );
        // Mobile Warp Disruptor (cat 22, group 361) owned by victim player
        snap.items.insert(
            mwd_id,
            make_fighter_item(mwd_id, mwd_type_id, victim_id, sys_id as i64, InventoryFlag::None, 1),
        );
        snap.item_positions.insert(mwd_id, [1_000.0, 0.0, 0.0]);
        snap.items.insert(
            mwd_cargo_id,
            make_fighter_item(mwd_cargo_id, 587, victim_id, mwd_id, InventoryFlag::Cargo, 1),
        );
        // Anchoring Mobile Depot (cat 22, group 1246) owned by victim player (unreinforced, directly killable)
        snap.items.insert(
            depot_id,
            make_fighter_item(depot_id, depot_type_id, victim_id, sys_id as i64, InventoryFlag::None, 1),
        );
        snap.item_positions.insert(depot_id, [2_000.0, 0.0, 0.0]);
        write_deployable_state_snap(snap, |st| {
            st.deployables.insert(
                depot_id,
                MobileDeployableRecord {
                    item_id: depot_id,
                    type_id: depot_type_id,
                    group_id: GROUP_MOBILE_DEPOT,
                    owner_id: victim_id,
                    solar_system_id: sys_id,
                    anchor_start_unix: now_unix,
                    anchor_duration_secs: 60,
                    anchor_completed: false,
                    is_unanchored: true,
                    expires_at_unix: now_unix + 86_400,
                    last_interaction_unix: now_unix,
                    reinforced_until_unix: None,
                    last_ping_unix: None,
                    position: [2_000.0, 0.0, 0.0],
                    deployed_fleet_id: None,
                },
            );
        });
        // Player Fighter Squadron item (cat 65) owned by victim player
        snap.items.insert(
            fighter_id,
            make_fighter_item(fighter_id, fighter_type_id, victim_id, sys_id as i64, InventoryFlag::None, 9),
        );
    });
    crate::services::combat_crimewatch::set_char_safety_level(&ctx, attacker_id, SafetyLevel::Red);
    fighter_test_helpers::insert_active_fighter(
        &ctx,
        fighter_id,
        crate::router::ActiveFighterSquadron {
            ball_id: fighter_id,
            item_id: fighter_id,
            type_id: fighter_type_id,
            owner_char_id: victim_id,
            parent_ship_id: 200_000_002,
            tube_index: 0,
            squadron_size: 9,
            target_id: None,
            is_abandoned: false,
            mwd_active: false,
        },
    );
    fighter_test_helpers::set_fighter_tube_load(&ctx, 200_000_002, 0, fighter_id);

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let _ = spawn_spool_ball(&ctx, sys_id, attacker_ship, 47_271, DVec3::ZERO, 100.0, 100.0, 100.0, 1_000.0).await;
    let _ = spawn_spool_ball(&ctx, sys_id, mwd_id, mwd_type_id, DVec3::new(1_000.0, 0.0, 0.0), 5.0, 5.0, 5.0, 0.0).await;
    sys_handle
        .schedule_mobile_warp_disruptor(mwd_id, mwd_type_id, DVec3::new(1_000.0, 0.0, 0.0), 20_000.0, 3_600.0)
        .await
        .unwrap();
    let _ = spawn_spool_ball(&ctx, sys_id, depot_id, depot_type_id, DVec3::new(2_000.0, 0.0, 0.0), 5.0, 5.0, 5.0, 0.0).await;
    let _ = spawn_spool_ball(&ctx, sys_id, fighter_id, fighter_type_id, DVec3::new(1_500.0, 0.0, 0.0), 0.0, 0.0, 5.0, 0.0).await;
    let _ = sys_handle.try_update_combat_state(mwd_id, |cs| {
        *cs = CombatState {
            shield_hp: 5.0, max_shield_hp: 5.0,
            armor_hp: 5.0, max_armor_hp: 5.0,
            structure_hp: 5.0, max_structure_hp: 5.0,
            signature_radius: 1_000.0, base_signature_radius: Some(1_000.0),
            ..CombatState::default()
        };
    });
    let _ = sys_handle.try_update_combat_state(depot_id, |cs| {
        *cs = CombatState {
            shield_hp: 5.0, max_shield_hp: 5.0,
            armor_hp: 5.0, max_armor_hp: 5.0,
            structure_hp: 5.0, max_structure_hp: 5.0,
            signature_radius: 1_000.0, base_signature_radius: Some(1_000.0),
            ..CombatState::default()
        };
    });
    let _ = sys_handle.try_update_combat_state(fighter_id, |cs| {
        cs.structure_hp = 5.0;
        cs.max_structure_hp = 5.0;
        cs.signature_radius = 1_000.0;
        cs.base_signature_radius = Some(1_000.0);
    });

    for tid in [mwd_id, depot_id, fighter_id] {
        sys_handle
            .add_target_lock(
                attacker_ship,
                TargetLock {
                    target_id: tid,
                    lock_time_ms: 1_000.0,
                    remaining_lock_ms: 0.0,
                    locked: true,
                    max_range_m: None,
                },
            )
            .await
            .unwrap();
    }
    ctx.invalidate_ship_fitting(attacker_ship);
    let kw = std::collections::HashMap::new();

    // 1. Destroy Mobile Warp Disruptor (cat 22) via dogmaIM.Activate
    let mwd_out = handle_weapons(
        &ctx,
        &mut session,
        "dogmaIM",
        "Activate",
        &[
            PyRep::Integer(gun_id),
            PyRep::String("projectileFired".into()),
            PyRep::Integer(mwd_id),
        ],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(mwd_out.dict_get("destroyed"), Some(&PyRep::Bool(true)));
    let mwd_kill = match mwd_out.dict_get("killID") {
        Some(PyRep::Integer(k)) => *k,
        other => panic!("expected killID for Mobile Warp Disruptor, got {other:?}"),
    };
    assert!(mwd_kill > 0);
    assert!(
        sys_handle.get_ball_snapshot(mwd_id).await.unwrap().is_none(),
        "0-HP Mobile Warp Disruptor ball must be removed from Ballpark"
    );
    assert!(
        !sys_handle.remove_warp_bubble(mwd_id).await.unwrap(),
        "Mobile Warp Disruptor bubble must be removed from warp_bubble_mgr once destroyed"
    );
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&mwd_id));
        assert!(snap.killmails.contains_key(&mwd_kill));
    });
    assert!(with_combat_runtime(&ctx, |rt| rt.is_recently_destroyed_item_id(mwd_id)));

    // 2. Destroy Mobile Depot (cat 22, actor-cleaned) via dogmaIM.Activate
    let depot_out = handle_weapons(
        &ctx,
        &mut session,
        "dogmaIM",
        "Activate",
        &[
            PyRep::Integer(gun_depot_id),
            PyRep::String("projectileFired".into()),
            PyRep::Integer(depot_id),
        ],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(depot_out.dict_get("destroyed"), Some(&PyRep::Bool(true)));
    let depot_kill = match depot_out.dict_get("killID") {
        Some(PyRep::Integer(k)) => *k,
        other => panic!("expected killID for Mobile Depot, got {other:?}"),
    };
    assert!(depot_kill > 0);
    assert!(sys_handle.get_ball_snapshot(depot_id).await.unwrap().is_none());
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&depot_id));
        assert!(!read_deployable_state_snap(snap).deployables.contains_key(&depot_id));
        assert!(snap.killmails.contains_key(&depot_kill));
    });
    assert!(with_combat_runtime(&ctx, |rt| rt.is_recently_destroyed_item_id(depot_id)));

    // 3. Destroy Fighter Squadron (cat 65) via dogmaIM.Activate
    let fighter_out = handle_weapons(
        &ctx,
        &mut session,
        "dogmaIM",
        "Activate",
        &[
            PyRep::Integer(gun_fighter_id),
            PyRep::String("projectileFired".into()),
            PyRep::Integer(fighter_id),
        ],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(fighter_out.dict_get("destroyed"), Some(&PyRep::Bool(true)));
    assert!(
        sys_handle.get_ball_snapshot(fighter_id).await.unwrap().is_none(),
        "0-HP Fighter Squadron ball must be removed from Ballpark"
    );
    assert!(!fighter_test_helpers::has_active_fighter(&ctx, fighter_id));
    assert!(fighter_test_helpers::loaded_fighter_in_tube(&ctx, 200_000_002, 0).is_none());
    ctx.db.with_snapshot(|snap| assert!(!snap.items.contains_key(&fighter_id)));
}
