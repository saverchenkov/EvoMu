pub(super) use super::super::combat_fighters::combat_fighters_tests::{
    make_fighter_ctx_and_session, make_fighter_item, make_fighter_type,
};
use super::*;
use crate::services::gm_soak_commands::combat_ephemeral_test_helpers::{
    contains_self_destruct, seed_self_destruct, self_destruct_count,
};
use eve_destiny::{Ball, CombatState, DamageVector, MovementMode};
use glam::DVec3;
use std::collections::HashMap;
use std::sync::Arc;

#[cfg(test)]
#[path = "combat_missiles_bomb_tests.rs"]
mod combat_missiles_bomb_tests;

pub(super) fn py_val<'a>(rep: &'a PyRep, key: &str) -> Option<&'a PyRep> {
    match rep {
        PyRep::Dict(entries) => entries
            .iter()
            .find(|(k, _)| matches!(k, PyRep::String(s) if s == key))
            .map(|(_, v)| v),
        PyRep::KeyVal(entries) => entries.get(key),
        _ => None,
    }
}

pub(super) async fn spawn_missile_target_ball(
    ctx: &ServerContext,
    sys_id: u32,
    id: i64,
    type_id: u32,
    pos: DVec3,
    shield: f64,
    armor: f64,
    hull: f64,
    cap: f64,
) {
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    sys_handle
        .spawn_ball_with_combat(
            Ball {
                id,
                type_id,
                position: pos,
                velocity: DVec3::ZERO,
                max_velocity: 250.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState {
                shield_hp: shield,
                max_shield_hp: shield.max(100.0),
                armor_hp: armor,
                max_armor_hp: armor.max(100.0),
                structure_hp: hull,
                max_structure_hp: hull.max(100.0),
                capacitor: cap,
                max_capacitor: cap.max(100.0),
                signature_radius: 120.0,
                base_signature_radius: Some(120.0),
                ..CombatState::default()
            },
        )
        .await
        .unwrap();
}

pub(super) fn make_missile_and_aoe_ctx() -> (ServerContext, SessionState, i64, i64, u32) {
    let (mut ctx, session, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    let mut sde = (*ctx.sde).clone();

    // Frigate/cruiser 587
    sde.types.insert(
        587,
        make_fighter_type(587, 25, 6, 10_000.0, vec![(263, 500.0), (265, 500.0), (9, 500.0)]),
    );
    // Capsule pod 670
    sde.types.insert(
        670,
        make_fighter_type(670, 29, 6, 500.0, vec![(9, 400.0)]),
    );
    // Wreck / container 26470 (non-ship cat 2)
    sde.types.insert(
        26_470,
        make_fighter_type(26_470, 186, 2, 100.0, vec![]),
    );
    // Light Missile Launcher 2410 (cat 7, group 509)
    sde.types.insert(
        2_410,
        make_fighter_type(2_410, 509, 7, 5.0, vec![(51, 3_000.0)]),
    );
    // Scourge Light Missile 209 (cat 8, group 384: vel 3750, flight 6000ms, hp 75, expVel 170, expRad 40, drf 0.604)
    sde.types.insert(
        209,
        make_fighter_type(
            209,
            384,
            8,
            0.015,
            vec![
                (37, 3_750.0),
                (281, 6_000.0),
                (9, 75.0),
                (653, 170.0),
                (654, 40.0),
                (1353, 0.604),
            ],
        ),
    );
    // Breacher Pod charge 85001 (cat 8, group 4070: dur 12000ms, flat 150, pct 2.0%)
    sde.types.insert(
        85_001,
        make_fighter_type(
            85_001,
            4070,
            8,
            0.1,
            vec![
                (37, 5_000.0),
                (281, 6_000.0),
                (5735, 30_000.0),
                (5736, 150.0),
                (5737, 5.0),
            ],
        ),
    );
    // Smartbomb 12219 (cat 7, group 72: range 5000, dmg 250 all, cap 30)
    sde.types.insert(
        12_219,
        make_fighter_type(
            12_219,
            72,
            7,
            5.0,
            vec![(99, 5_000.0), (114, 250.0), (116, 250.0), (117, 250.0), (118, 250.0), (6, 30.0)],
        ),
    );
    // Bomb Launcher 27912 (cat 7, group 862: flight 3000ms, vel 2000)
    sde.types.insert(
        27_912,
        make_fighter_type(27_912, 862, 7, 10.0, vec![(281, 3_000.0), (37, 2_000.0)]),
    );
    // Damage Bomb 27916 (group 90, flight 3000ms, vel 2000, blast 15_000, expRad 400, 6400 therm)
    sde.types.insert(
        27_916,
        make_fighter_type(
            27_916,
            90,
            8,
            75.0,
            vec![(281, 3_000.0), (37, 2_000.0), (99, 15_000.0), (654, 400.0), (118, 6_400.0)],
        ),
    );
    // Focused Void Bomb 27920 (group 864, flight 3000ms, vel 2000, neut 2500)
    sde.types.insert(
        27_920,
        make_fighter_type(
            27_920,
            864,
            8,
            75.0,
            vec![(281, 3_000.0), (37, 2_000.0), (99, 15_000.0), (654, 400.0), (97, 2_500.0)],
        ),
    );
    // Lockbreaker Bomb 27924 (group 863, flight 3000ms, vel 2000)
    sde.types.insert(
        27_924,
        make_fighter_type(27_924, 863, 8, 75.0, vec![(281, 3_000.0), (37, 2_000.0), (99, 15_000.0), (654, 400.0)]),
    );
    // Defender Launcher 1599 (cat 7, group 512) + Defender Missile 267 (group 88)
    sde.types.insert(1_599, make_fighter_type(1_599, 512, 7, 5.0, vec![]));
    sde.types.insert(267, make_fighter_type(267, 88, 8, 0.015, vec![]));
    // Scanner probe 30013 (cat 8, group 479)
    sde.types.insert(30_013, make_fighter_type(30_013, 479, 8, 1.0, vec![]));

    ctx.sde = Arc::new(sde);
    (ctx, session, char_id, ship_id, sys_id)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn non_ship_ball_cleanup_fails_courier_and_npc_destruction_clears_state() {
    let (ctx, _, char_id, ship_id, sys_id) = make_missile_and_aoe_ctx();
    let container_id = 660_000_001i64;
    let nested_cargo_id = 660_000_002i64;

    assert!(is_combatant_ship_or_entity_ball(&ctx, ship_id, 587));
    assert!(!is_combatant_ship_or_entity_ball(&ctx, container_id, 26_470));
    assert_eq!(active_ship_victim_char_id(&ctx, ship_id), Some(char_id));
    assert_eq!(active_ship_victim_char_id(&ctx, container_id), None);

    spawn_missile_target_ball(&ctx, sys_id, container_id, 26_470, DVec3::ZERO, 100.0, 0.0, 50.0, 0.0).await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            container_id,
            make_fighter_item(container_id, 26_470, char_id, sys_id as i64, InventoryFlag::None, 1),
        );
        snap.items.insert(
            nested_cargo_id,
            make_fighter_item(nested_cargo_id, 587, char_id, container_id, InventoryFlag::Cargo, 1),
        );
        snap.contracts.insert(
            99_001,
            eve_db::ContractRecord {
                contract_id: 99_001,
                issuer_id: char_id,
                acceptor_id: Some(char_id),
                contract_type: "Courier".into(),
                status: "InProgress".into(),
                start_station_id: 60_003_760,
                end_station_id: 60_003_761,
                reward: eve_db::IskCents(100_000_000),
                collateral: eve_db::IskCents(500_000_000),
                item_ids: vec![nested_cargo_id],
                expiration_unix: ctx.now_unix() + 86_400,
                assignee_id: char_id,
                date_issued_unix: ctx.now_unix(),
                title: "Courier Test".into(),
                ..Default::default()
            },
        );
    });

    handle_destroyed_ball_from_missile_or_aoe(
        &ctx,
        sys_id,
        char_id,
        container_id,
        26_470,
        DVec3::ZERO,
        ctx.now_unix(),
    )
    .await;

    let (cargo_gone, contract_failed) = ctx.db.with_snapshot(|snap| {
        (
            !snap.items.contains_key(&nested_cargo_id),
            snap.contracts.get(&99_001).map(|c| c.status.clone()),
        )
    });
    assert!(cargo_gone, "nested cargo inside destroyed jetcan must be deleted");
    assert_eq!(contract_failed.as_deref(), Some("Failed"));

    // NPC entity destruction cleans runtime tables
    let npc_id = 1_895_000_042i64;
    crate::combat::npc_ai::npc_test_helpers::seed_npc_entity(
        &ctx,
        ExtendedNpcEntity {
            ball_id: npc_id,
            type_id: 587,
            faction_id: 500_010,
            archetype: "raider".into(),
            solar_system_id: sys_id,
            spawn_group_id: 1,
            target_id: None,
            aggression_threat: HashMap::new(),
            threat_table: HashMap::new(),
            bounty_isk: 0.0,
            optimal_range: 5_000.0,
            orbit_range: 2_500.0,
            damage_vector: DamageVector::new(0.0, 10.0, 0.0, 0.0),
            entropic_cycles: 0,
            drifter_overshield_hp: 0.0,
            drifter_doomsday_fired: false,
            web_speed_factor: 0.0,
            web_range: 0.0,
            scram_strength: 0.0,
            scram_range: 0.0,
            neut_amount: 0.0,
            ecm_jammed: false,
            remote_shield_rep: 0.0,
            remote_armor_rep: 0.0,
            remote_rep_range: 0.0,
            tag_drop_type_id: None,
            spawn_position: DVec3::ZERO,
            sleeper_wh_class: 0,
            custom_cargo_loot: None,
        },
    );
    handle_destroyed_ball_from_missile_or_aoe(&ctx, sys_id, char_id, npc_id, 587, DVec3::ZERO, ctx.now_unix()).await;
    assert!(!crate::combat::npc_ai::npc_test_helpers::has_npc_entity(&ctx, npc_id));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn missile_launch_breacher_dot_expiration_and_self_destruct_step() {
    let (ctx, _, char_id, ship_id, sys_id) = make_missile_and_aoe_ctx();
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let target_id = 300_000_301i64;

    spawn_missile_target_ball(&ctx, sys_id, ship_id, 587, DVec3::ZERO, 500.0, 500.0, 500.0, 500.0).await;
    spawn_missile_target_ball(
        &ctx,
        sys_id,
        target_id,
        587,
        DVec3::new(1_000.0, 0.0, 0.0),
        400.0,
        200.0,
        200.0,
        500.0,
    )
    .await;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(710_001, make_fighter_item(710_001, 2_410, char_id, ship_id, InventoryFlag::HiSlot0, 1));
    });
    let fit = ctx.get_or_rebuild_ship_fitting(char_id, ship_id).unwrap();
    let fitted_launcher = eve_dogma::FittedModule {
        item_id: 710_001,
        type_id: 2_410,
        flag_id: 27,
        state: eve_dogma::ModuleState::Active,
        damage_hp: 0.0,
        max_hp: 40.0,
        charge_type_id: None,
        charge_quantity: 0,
        dynamic_attribute_overrides: HashMap::new(),
    };
    let missile_t = ctx.sde.types.get(&209).unwrap();
    let breacher_t = ctx.sde.types.get(&85_001).unwrap();
    let (_, attacker_ball) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();

    // 1. Standard missile immediate impact
    let res_std = activate_missile_or_breacher_launcher(
        &ctx,
        char_id,
        ship_id,
        sys_id,
        ctx.now_unix(),
        710_001,
        &fitted_launcher,
        &fit,
        209,
        missile_t,
        &attacker_ball,
        target_id,
        DamageVector::new(0.0, 0.0, 180.0, 0.0),
        false,
        false,
        true,
    )
    .await
    .unwrap();
    assert_eq!(py_val(&res_std, "isFoF").and_then(|v| v.as_bool()), Some(false));
    assert!(sys_handle.get_combat_state(target_id).await.unwrap().unwrap().shield_hp < 400.0);

    // 2. Breacher pod impact installs DoT and steps damage to lethal kill
    let res_br = activate_missile_or_breacher_launcher(
        &ctx,
        char_id,
        ship_id,
        sys_id,
        ctx.now_unix(),
        710_001,
        &fitted_launcher,
        &fit,
        85_001,
        breacher_t,
        &attacker_ball,
        target_id,
        DamageVector::new(0.0, 0.0, 50.0, 0.0),
        false,
        true,
        true,
    )
    .await
    .unwrap();
    assert_eq!(py_val(&res_br, "isBreacherPod").and_then(|v| v.as_bool()), Some(true));
    assert!(py_val(&res_br, "breacherTickDPS").and_then(|v| v.as_f64()).unwrap() > 0.0);

    let cs_before_dot = sys_handle.get_combat_state(target_id).await.unwrap().unwrap();
    let hp_before_dot = cs_before_dot.shield_hp + cs_before_dot.armor_hp + cs_before_dot.structure_hp;
    let _ = step_combat_system(&ctx, sys_id, 2.0).await;
    let cs_after_dot = sys_handle.get_combat_state(target_id).await.unwrap().unwrap();
    assert!(
        cs_after_dot.shield_hp + cs_after_dot.armor_hp + cs_after_dot.structure_hp < hp_before_dot
    );

    // Exhaust target HP via multi-second DoT ticks when target is on low structure HP
    if let Ok(Some(mut cs)) = sys_handle.get_combat_state(target_id).await {
        cs.shield_hp = 0.0;
        cs.armor_hp = 0.0;
        cs.structure_hp = 15.0;
        let _ = sys_handle.set_combat_state(target_id, cs).await;
    }
    let _ = step_missiles_and_breachers(&ctx, sys_id, 10.0).await;
    assert!(sys_handle.get_ball_snapshot(target_id).await.unwrap().is_none(), "lethal Breacher DoT should destroy target");

    // 3. Deferred FoF missile outruns flight time in step_missiles_and_breachers -> expired == true
    let outrun_target = 300_000_302i64;
    spawn_missile_target_ball(&ctx, sys_id, outrun_target, 587, DVec3::new(200_000.0, 0.0, 0.0), 500.0, 500.0, 500.0, 500.0).await;
    let _ = activate_missile_or_breacher_launcher(
        &ctx,
        char_id,
        ship_id,
        sys_id,
        ctx.now_unix(),
        710_001,
        &fitted_launcher,
        &fit,
        209,
        missile_t,
        &attacker_ball,
        outrun_target,
        DamageVector::new(0.0, 0.0, 120.0, 0.0),
        true,
        false,
        true,
    )
    .await
    .unwrap();
    let updated = step_missiles_and_breachers(&ctx, sys_id, 20.0).await;
    assert!(updated.iter().any(|m| m.expired && m.target_ball_id == outrun_target));

    // 4. Elapsed self-destruct countdown in step_missiles_and_breachers detonate ship
    let sd_ship = 200_000_999i64;
    spawn_missile_target_ball(&ctx, sys_id, sd_ship, 587, DVec3::new(500.0, 0.0, 0.0), 100.0, 100.0, 100.0, 100.0).await;
    seed_self_destruct(
        &ctx,
        sd_ship,
        SelfDestructCountdown {
            ship_id: sd_ship,
            char_id,
            solar_system_id: sys_id,
            started_unix: ctx.now_unix().saturating_sub(120),
            detonate_at_unix: ctx.now_unix(),
        },
    );
    let _ = step_missiles_and_breachers(&ctx, sys_id, 1.0).await;
    assert!(!contains_self_destruct(&ctx, sd_ship));
}

#[tokio::test]
async fn cleanup_destroyed_non_ship_ball_fails_courier_contracts_and_removes_nested_items() {
    let (ctx, _session, char_id, _ship_id, sys_id) = make_missile_and_aoe_ctx();
    let jetcan_id = 300_000_701i64;
    let wrap_id = 300_000_702i64;
    let cargo_id = 300_000_703i64;
    let sibling_id = 300_000_704i64;
    let active_cid = 880_001i64;
    let outstanding_cid = 880_002i64;
    let can_pos = DVec3::new(1_500.0, 250.0, -100.0);

    spawn_missile_target_ball(&ctx, sys_id, jetcan_id, 26_470, can_pos, 0.0, 0.0, 50.0, 0.0).await;

    let balance_before = ctx.db.with_mut_snapshot(|snap| {
        let bal_before = snap.characters.get(&char_id).expect("issuer char").balance;
        snap.items.insert(
            jetcan_id,
            make_fighter_item(jetcan_id, 26_470, char_id, sys_id as i64, InventoryFlag::None, 1),
        );
        snap.item_positions.insert(jetcan_id, can_pos.to_array());
        snap.items.insert(
            wrap_id,
            make_fighter_item(wrap_id, 3468, char_id, jetcan_id, InventoryFlag::Cargo, 1),
        );
        snap.items.insert(
            cargo_id,
            make_fighter_item(cargo_id, 587, char_id, wrap_id, InventoryFlag::Cargo, 1),
        );
        snap.item_positions.insert(cargo_id, can_pos.to_array());
        snap.items.insert(
            sibling_id,
            make_fighter_item(sibling_id, 209, char_id, jetcan_id, InventoryFlag::Cargo, 50),
        );
        snap.contracts.insert(
            active_cid,
            eve_db::types::ContractRecord {
                contract_id: active_cid,
                issuer_id: char_id,
                acceptor_id: Some(char_id + 1),
                contract_type: "Courier".into(),
                status: "InProgress".into(),
                start_station_id: 60_003_760,
                end_station_id: 60_003_761,
                reward: eve_db::types::IskCents(5_000),
                collateral: eve_db::types::IskCents(25_000),
                item_ids: vec![cargo_id],
                ..Default::default()
            },
        );
        snap.contracts.insert(
            outstanding_cid,
            eve_db::types::ContractRecord {
                contract_id: outstanding_cid,
                issuer_id: char_id,
                contract_type: "Courier".into(),
                status: "Outstanding".into(),
                start_station_id: 60_003_760,
                end_station_id: 60_003_761,
                item_ids: vec![sibling_id],
                ..Default::default()
            },
        );
        bal_before
    });

    cleanup_destroyed_non_ship_ball(&ctx, sys_id, jetcan_id).await;

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    assert!(sys_handle.get_ball_snapshot(jetcan_id).await.unwrap().is_none());

    ctx.db.with_snapshot(|snap| {
        for removed_id in [jetcan_id, wrap_id, cargo_id, sibling_id] {
            assert!(!snap.items.contains_key(&removed_id), "item {removed_id} must be deleted");
            assert!(!snap.item_positions.contains_key(&removed_id), "pos {removed_id} must be deleted");
        }
        assert_eq!(snap.contracts.get(&active_cid).unwrap().status, "Failed");
        assert_eq!(snap.contracts.get(&outstanding_cid).unwrap().status, "Outstanding");
        let bal_after = snap.characters.get(&char_id).unwrap().balance;
        assert_eq!(bal_after.0, balance_before.0 + 30_000);
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mto_site_ball_excluded_from_capsuleer_death_and_completed_on_missile_or_aoe_kill() {
    let (mut ctx, _, char_id, ship_id, sys_id) = make_missile_and_aoe_ctx();
    let mut sde = (*ctx.sde).clone();
    // Harbinger (`type_id == 24_690`, `category_id == 6`) used for MTO site balls (`#1304`).
    sde.types.insert(
        24_690,
        make_fighter_type(
            24_690,
            419,
            6,
            15_000.0,
            vec![(263, 3_500.0), (265, 4_000.0), (9, 3_200.0)],
        ),
    );
    ctx.sde = Arc::new(sde);

    let now = ctx.now_unix();
    let den_id = 1_090_000_010i64;
    let mto_ball_id = 1_990_000_901i64;
    let npc_hull_id = 1_990_000_902i64;
    let unpiloted_player_hull = 1_990_000_903i64;
    let act_uuid = vec![0xAB; 16];
    let act_hex = "ab".repeat(16);
    let pos = DVec3::new(5_000.0, 0.0, 0.0);

    let (bal_before, km_before, npc_kills_before) = ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            mto_ball_id,
            make_fighter_item(
                mto_ball_id,
                24_690,
                500_010,
                i64::from(sys_id),
                eve_db::InventoryFlag::None,
                1,
            ),
        );
        snap.item_positions.insert(mto_ball_id, [pos.x, pos.y, pos.z]);
        snap.sp6_lifecycle
            .space_object_meta
            .insert(mto_ball_id, eve_db::PersistedSpaceObjectMeta::default());
        snap.sp6_lifecycle
            .ship_combat_hp
            .insert(mto_ball_id, eve_db::PersistedCombatHp::default());
        snap.items.insert(
            npc_hull_id,
            make_fighter_item(
                npc_hull_id,
                24_690,
                500_010,
                i64::from(sys_id),
                eve_db::InventoryFlag::None,
                1,
            ),
        );
        snap.item_positions.insert(npc_hull_id, [pos.x, pos.y, pos.z]);
        snap.sp6_lifecycle
            .space_object_meta
            .insert(npc_hull_id, eve_db::PersistedSpaceObjectMeta::default());
        snap.sp6_lifecycle
            .ship_combat_hp
            .insert(npc_hull_id, eve_db::PersistedCombatHp::default());
        snap.items.insert(
            unpiloted_player_hull,
            make_fighter_item(
                unpiloted_player_hull,
                587,
                char_id,
                i64::from(sys_id),
                eve_db::InventoryFlag::None,
                1,
            ),
        );
        snap.sov_runtime.mercenary_dens.insert(
            den_id,
            eve_db::PersistedMercenaryDen {
                den_id,
                type_id: crate::services::sov_gameplay::TYPE_MERCENARY_DEN,
                owner_char_id: char_id,
                skyhook_id: 1_960_000_099,
                solar_system_id: sys_id,
                planet_id: 40_000_001,
                enabled: true,
                cargo_extraction_enabled: true,
                development_level: 40,
                anarchy_level: 40,
                infomorphs: 10,
                simulated_at_unix: now,
                next_activity_generation_unix: now + 3_600,
            },
        );
        snap.sov_runtime.mercenary_activities.insert(
            act_hex.clone(),
            eve_db::PersistedMercenaryActivity {
                uuid: act_uuid,
                den_id,
                solar_system_id: sys_id,
                dungeon_id: 1,
                started: true,
                started_by: char_id,
                expiry_unix: now + 3_600,
                development_impact: 10,
                anarchy_impact: -10,
                infomorph_bonus: 25,
            },
        );
        snap.sov_runtime
            .mto_site_balls
            .insert(mto_ball_id, act_hex.clone());
        let npc_kills_before: u32 = snap
            .system_kill_events
            .iter()
            .filter(|e| e.solar_system_id == sys_id)
            .map(|e| e.npc_kills)
            .sum();
        (snap.characters[&char_id].balance, snap.killmails.len(), npc_kills_before)
    });

    assert_eq!(active_ship_victim_char_id(&ctx, ship_id), Some(char_id));
    assert_eq!(
        active_ship_victim_char_id(&ctx, unpiloted_player_hull),
        Some(char_id)
    );
    assert_eq!(active_ship_victim_char_id(&ctx, mto_ball_id), None);
    assert_eq!(active_ship_victim_char_id(&ctx, npc_hull_id), None);

    spawn_missile_target_ball(&ctx, sys_id, mto_ball_id, 24_690, pos, 100.0, 100.0, 100.0, 100.0)
        .await;
    spawn_missile_target_ball(&ctx, sys_id, npc_hull_id, 24_690, pos, 100.0, 100.0, 100.0, 100.0)
        .await;

    handle_destroyed_ball_from_missile_or_aoe(
        &ctx,
        sys_id,
        char_id,
        mto_ball_id,
        24_690,
        pos,
        now,
    )
    .await;
    handle_destroyed_ball_from_missile_or_aoe(
        &ctx,
        sys_id,
        char_id,
        npc_hull_id,
        24_690,
        pos,
        now,
    )
    .await;

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    assert!(sys_handle.get_ball_snapshot(mto_ball_id).await.unwrap().is_none());
    assert!(sys_handle.get_ball_snapshot(npc_hull_id).await.unwrap().is_none());

    ctx.db.with_snapshot(|snap| {
        assert!(!snap.sov_runtime.mto_site_balls.contains_key(&mto_ball_id));
        assert!(!snap.sov_runtime.mercenary_activities.contains_key(&act_hex));
        assert!(!snap.items.contains_key(&mto_ball_id));
        assert!(!snap.item_positions.contains_key(&mto_ball_id));
        assert!(!snap.sp6_lifecycle.space_object_meta.contains_key(&mto_ball_id));
        assert!(!snap.sp6_lifecycle.ship_combat_hp.contains_key(&mto_ball_id));
        assert!(!snap.items.contains_key(&npc_hull_id));
        assert!(!snap.item_positions.contains_key(&npc_hull_id));
        assert!(!snap.sp6_lifecycle.space_object_meta.contains_key(&npc_hull_id));
        assert!(!snap.sp6_lifecycle.ship_combat_hp.contains_key(&npc_hull_id));
        let den = snap.sov_runtime.mercenary_dens.get(&den_id).unwrap();
        assert_eq!(den.development_level, 50);
        assert_eq!(den.anarchy_level, 30);
        // Sub-max Development (`40 < 100`) MTO completion grants +Development (`40 -> 50`) rather
        // than bonus Encrypted Infomorphs (`#1233`).
        assert_eq!(den.infomorphs, 10);
        assert_eq!(
            snap.characters[&char_id].balance,
            bal_before
                .checked_add(eve_db::types::IskCents::from_isk(7_500_000))
                .unwrap()
        );
        assert_eq!(snap.killmails.len(), km_before);
        let npc_kills_after: u32 = snap
            .system_kill_events
            .iter()
            .filter(|e| e.solar_system_id == sys_id)
            .map(|e| e.npc_kills)
            .sum();
        assert_eq!(npc_kills_after, npc_kills_before + 1);
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unpiloted_corp_ship_destroyed_by_missile_or_aoe_creates_corp_killmail_and_wreck() {
    let (ctx, _, char_id, ship_id, sys_id) = make_missile_and_aoe_ctx();
    let now = ctx.now_unix();
    let corp_id = 98_000_077i64;
    let ally_id = 99_000_077i64;
    let corp_ship_id = 1_990_000_941i64;
    let corp_cargo_id = 1_990_000_942i64;
    let pos = DVec3::new(6_000.0, 0.0, 0.0);

    let km_before = ctx.db.with_mut_snapshot(|snap| {
        let mut corp = eve_db::CorporationRecord::default();
        corp.corporation_id = corp_id;
        corp.name = "AOE Victim Corp".to_string();
        corp.ticker = "AOEV".to_string();
        corp.alliance_id = Some(ally_id);
        snap.corporations.insert(corp_id, corp);
        snap.items.insert(
            corp_ship_id,
            make_fighter_item(
                corp_ship_id,
                587,
                corp_id,
                i64::from(sys_id),
                eve_db::InventoryFlag::None,
                1,
            ),
        );
        snap.item_positions
            .insert(corp_ship_id, [pos.x, pos.y, pos.z]);
        snap.sp6_lifecycle
            .space_object_meta
            .insert(corp_ship_id, eve_db::PersistedSpaceObjectMeta::default());
        snap.sp6_lifecycle
            .ship_combat_hp
            .insert(corp_ship_id, eve_db::PersistedCombatHp::default());
        snap.items.insert(
            corp_cargo_id,
            make_fighter_item(
                corp_cargo_id,
                34,
                corp_id,
                corp_ship_id,
                eve_db::InventoryFlag::Cargo,
                50,
            ),
        );
        snap.killmails.len()
    });

    assert_eq!(active_ship_victim_char_id(&ctx, corp_ship_id), None);
    assert!(is_corp_owned_ship_ball(&ctx, corp_ship_id));

    spawn_missile_target_ball(&ctx, sys_id, corp_ship_id, 587, pos, 100.0, 100.0, 100.0, 100.0)
        .await;

    handle_destroyed_ball_from_missile_or_aoe(
        &ctx,
        sys_id,
        char_id,
        corp_ship_id,
        587,
        pos,
        now,
    )
    .await;

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    assert!(sys_handle.get_ball_snapshot(corp_ship_id).await.unwrap().is_none());

    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&corp_ship_id));
        assert!(!snap.item_positions.contains_key(&corp_ship_id));
        assert!(
            !snap
                .sp6_lifecycle
                .space_object_meta
                .contains_key(&corp_ship_id)
        );
        assert!(
            !snap
                .sp6_lifecycle
                .ship_combat_hp
                .contains_key(&corp_ship_id)
        );
        assert_eq!(snap.killmails.len(), km_before + 1);
        let km = snap
            .killmails
            .values()
            .max_by_key(|k| k.kill_id)
            .expect("corp ship killmail created");
        assert_eq!(km.victim_character_id, 0);
        assert_eq!(km.victim_corp_id, corp_id);
        assert_eq!(km.victim_alliance_id, Some(ally_id));
        assert_eq!(km.attacker_character_id, char_id);
        assert_eq!(km.ship_type_id, 587);
        assert_eq!(snap.characters[&char_id].active_ship_id, ship_id);
        assert!(
            snap.items
                .values()
                .any(|it| it.owner_id == corp_id && it.location_id == i64::from(sys_id)),
            "corp wreck spawned in space owned by victim corp"
        );
    });
}

#[tokio::test]
async fn self_destruct_abort_before_aoe_tick_persist_not_resurrected_gh_1174() {
    let (ctx, _session, char_id, _orig_ship_id, sys_id) = make_missile_and_aoe_ctx();
    let base_now = 1_770_000_000u64;
    ctx.set_clock(Arc::new(eve_db::VirtualServerClock::with_fixed_base(base_now)));
    let target_ship_id = 200_000_998i64;

    let km_before = ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            target_ship_id,
            make_fighter_item(
                target_ship_id,
                587,
                char_id,
                i64::from(sys_id),
                eve_db::InventoryFlag::None,
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
        ch.active_ship_id = target_ship_id;
        snap.killmails.len()
    });
    spawn_missile_target_ball(
        &ctx,
        sys_id,
        target_ship_id,
        587,
        DVec3::new(500.0, 0.0, 0.0),
        100.0,
        100.0,
        100.0,
        100.0,
    )
    .await;

    // 1. Start countdown, step & snapshot as `step_missiles_and_breachers` does, abort, then persist.
    crate::sp6_lifecycle::register_self_destruct_countdown(
        &ctx,
        target_ship_id,
        char_id,
        sys_id,
        base_now,
        base_now + 120,
    );
    let (rt_snapshot, snap_now) = step_and_snapshot_self_destructs_for_test(&ctx, sys_id, 1.0);
    assert_eq!(rt_snapshot.len(), 1);
    assert_eq!(rt_snapshot[0].entry.ship_id, target_ship_id);

    assert!(crate::sp6_lifecycle::abort_self_destruct_for_ship(
        &ctx,
        target_ship_id
    ));
    let due_after_abort =
        persist_and_collect_due_self_destructs_for_test(&ctx, &rt_snapshot, sys_id, snap_now);
    assert_eq!(due_after_abort.len(), 0);
    assert_eq!(
        crate::sp6_lifecycle::with_sp6_state(&ctx.db, |st| st.self_destructs.len()),
        0
    );
    assert_eq!(
        self_destruct_count(&ctx),
        0
    );

    // 2. Advance past `detonate_at_unix` and run `step_missiles_and_breachers`: ship survives intact.
    ctx.advance_secs(125);
    let _ = step_missiles_and_breachers(&ctx, sys_id, 1.0).await;

    // 3. Also test snapshot taken while countdown is already due (`detonate_at_unix <= now_unix`)
    //    followed by abort before AoE persistence/due-collection runs.
    let now2 = ctx.now_unix();
    crate::sp6_lifecycle::register_self_destruct_countdown(
        &ctx,
        target_ship_id,
        char_id,
        sys_id,
        now2.saturating_sub(120),
        now2,
    );
    let (due_rt_snapshot, due_snap_now) =
        step_and_snapshot_self_destructs_for_test(&ctx, sys_id, 1.0);
    assert_eq!(due_rt_snapshot.len(), 1);
    assert!(crate::sp6_lifecycle::abort_self_destruct_for_ship(
        &ctx,
        target_ship_id
    ));
    let due_after_due_abort = persist_and_collect_due_self_destructs_for_test(
        &ctx,
        &due_rt_snapshot,
        sys_id,
        due_snap_now,
    );
    assert_eq!(due_after_due_abort.len(), 0);

    let _ = step_missiles_and_breachers(&ctx, sys_id, 1.0).await;

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    assert!(sys_handle
        .get_ball_snapshot(target_ship_id)
        .await
        .unwrap()
        .is_some());
    assert_eq!(
        crate::sp6_lifecycle::with_sp6_state(&ctx.db, |st| st.self_destructs.len()),
        0
    );
    assert_eq!(
        self_destruct_count(&ctx),
        0
    );
    ctx.db.with_snapshot(|snap| {
        assert_eq!(
            snap.items
                .get(&target_ship_id)
                .map(|i| (i.owner_id, i.location_id)),
            Some((char_id, i64::from(sys_id)))
        );
        let ch = &snap.characters[&char_id];
        assert_eq!(ch.active_ship_id, target_ship_id);
        assert_eq!(ch.station_id, None);
        assert_eq!(snap.killmails.len(), km_before);
    });
}
