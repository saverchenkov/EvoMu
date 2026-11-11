//! Deep branch and mutation-killing unit tests for `services::fitting_scanners_superweapons`.

#![forbid(unsafe_code)]

use super::inventory_fitting::fitting_containers_ships::read_container_ship_state;
use super::inventory_fitting::fitting_scanners_superweapons::*;
use crate::router::ServerContext;
use eve_db::{
    ContractRecord, FleetRecord, InventoryFlag, InventoryItemRecord, JumpCloneRecord,
    TrainedSkillRecord, UpwellStructureRecord, VirtualServerClock, WorldRepository,
};
use eve_destiny::{Ball, CombatState, MovementMode, SlimItemMeta};
use eve_dogma::DogmaItemState;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::{DbuffCollectionInfo, InvType, SdeRegistry};
use glam::DVec3;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn custom_inv_type(sde: &SdeRegistry, type_id: u32, group_id: u32, category_id: u32, name: &str, attrs: &[(u16, f64)]) -> InvType {
    let mut t = sde.types.get(&587).expect("seed type").clone();
    t.type_id = type_id;
    t.group_id = group_id;
    t.category_id = category_id;
    t.name = name.into();
    t.default_attributes.clear();
    for &(k, v) in attrs {
        t.default_attributes.insert(k, v);
    }
    t
}

fn test_ctx_with_custom<F: FnOnce(&mut SdeRegistry)>(mutate_sde: F) -> ServerContext {
    let mut sde = (*shared_sde()).clone();
    mutate_sde(&mut sde);
    let db = Arc::new(WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(Arc::new(sde), db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_850_000_000)));
    ctx
}

fn seed_char(ctx: &ServerContext, char_id: i64, corp_id: i64, sys_id: u32, ship_id: i64, fleet_id: Option<i64>) {
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().expect("demo char");
        ch.character_id = char_id;
        ch.user_id = char_id as u64;
        ch.name = format!("Pilot {char_id}");
        ch.corporation_id = corp_id;
        ch.station_id = None;
        ch.structure_id = None;
        ch.solar_system_id = sys_id;
        ch.active_ship_id = ship_id;
        ch.security_status = 5.0;
        ch.fleet_id = fleet_id;
        ch.clone_grade = 1;
        snap.characters.insert(char_id, ch);
        snap.skills.entry(char_id).or_default().insert(
            24242,
            TrainedSkillRecord { type_id: 24242, level: 2, skill_points: 50_000.0 },
        );
    });
}

fn insert_item(ctx: &ServerContext, item_id: i64, type_id: u32, owner_id: i64, location_id: i64, flag: InventoryFlag, quantity: i32, singleton: bool) {
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            item_id,
            InventoryItemRecord { item_id, type_id, owner_id, location_id, flag, quantity, singleton, ..Default::default() },
        );
    });
}

async fn spawn_ball(ctx: &ServerContext, sys_id: u32, ball_id: i64, type_id: u32, group_id: u32, owner_id: i64, pos: DVec3) {
    let sys = ctx.systems.get_or_spawn(sys_id);
    let ball = Ball {
        id: ball_id,
        type_id,
        position: pos,
        velocity: DVec3::ZERO,
        max_velocity: 500.0,
        inertia: 1.0,
        mass: 1_000_000.0,
        radius: 100.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let mut cs = CombatState::default();
    cs.capacitor = 1_200.0;
    cs.max_capacitor = 1_500.0;
    cs.shield_hp = 800.0;
    cs.armor_hp = 700.0;
    cs.structure_hp = 600.0;
    let meta = SlimItemMeta {
        group_id,
        category_id: 6,
        owner_id,
        char_id: Some(owner_id),
        corp_id: 98_000_001,
        ..Default::default()
    };
    sys.spawn_ball_with_meta(ball, cs, meta).await.expect("spawn ball");
}

#[test]
fn hacking_unlock_dbuff_aggregation_and_attribute_modifier_operations() {
    let ctx = test_ctx_with_custom(|sde| {
        sde.types.insert(
            999_501,
            custom_inv_type(sde, 999_501, 1975, 2, "Weather Storm Beacon", &[(2468, 901.0), (2469, 2.0), (2470, 902.0), (2471, 50.0)]),
        );
        sde.types.insert(
            999_502,
            custom_inv_type(sde, 999_502, 920, 2, "Effect Beacon", &[(2472, 903.0), (2473, 777.0), (2536, 904.0), (2537, 25.0)]),
        );
        for (id, op, item_mods, loc_mods) in [
            (901u32, "PostMul", vec![263u16], vec![265u16]),
            (902, "ModAdd", vec![37], Vec::new()),
            (903, "PostAssignment", vec![552], Vec::new()),
            (904, "PostPercent", vec![64], Vec::new()),
        ] {
            sde.dbuff_collections.insert(
                id,
                DbuffCollectionInfo {
                    dbuff_id: id,
                    name: format!("dbuff_{id}"),
                    aggregate_mode: "Maximum".into(),
                    operation: op.into(),
                    display_name_id: None,
                    show_output_value_in_ui: Some("ShowNormal".into()),
                    item_modifiers: item_mods,
                    location_modifiers: loc_mods,
                    location_group_modifiers: Vec::new(),
                    location_required_skill_modifiers: Vec::new(),
                    target_attribute_ids: Vec::new(),
                },
            );
        }
    });

    let char_id = 96_100_001;
    let sys_id = 30_000_142;
    let ship_id = 1_000_700_001;
    let cont_space_id = 1_000_700_002;
    let cont_station_id = 1_000_700_003;
    let cont_contract_id = 1_000_700_004;
    let sov_hub_id = 1_000_700_005;

    seed_char(&ctx, char_id, 98_000_010, sys_id, ship_id, None);
    insert_item(&ctx, ship_id, 587, char_id, sys_id as i64, InventoryFlag::None, 1, true);
    insert_item(&ctx, cont_space_id, 17366, char_id, sys_id as i64, InventoryFlag::Locked, 1, true);
    insert_item(&ctx, cont_station_id, 17366, char_id, 60_003_760, InventoryFlag::Locked, 1, true);
    insert_item(&ctx, cont_contract_id, 17366, char_id, sys_id as i64, InventoryFlag::Locked, 1, true);

    ctx.db.with_mut_snapshot(|snap| {
        snap.contracts.insert(
            88_001,
            ContractRecord {
                contract_id: 88_001,
                issuer_id: char_id,
                issuer_corp_id: 98_000_010,
                for_corp: false,
                availability: "Public".into(),
                assignee_id: 0,
                acceptor_id: None,
                contract_type: "ItemExchange".into(),
                status: "Outstanding".into(),
                title: "Locked".into(),
                start_station_id: 60_003_760,
                end_station_id: 60_003_760,
                price: Default::default(),
                reward: Default::default(),
                collateral: Default::default(),
                date_issued_unix: 1_850_000_000,
                expiration_unix: 1_850_086_400,
                num_days: 1,
                item_ids: vec![cont_contract_id],
                requested_items: Vec::new(),
                ..Default::default()
            },
        );
        snap.structures.insert(
            sov_hub_id,
            UpwellStructureRecord {
                structure_id: sov_hub_id,
                type_id: crate::services::sov_services::TYPE_SOVEREIGNTY_HUB,
                owner_corp_id: 98_000_099,
                solar_system_id: sys_id,
                name: "Sov Hub".into(),
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                shield_hp: 1_000_000.0,
                armor_hp: 1_000_000.0,
                hull_hp: 1_000_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
    });

    {
        let mut st = scanners_superweapons_state().write().unwrap();
        st.hacking_states.insert(
            cont_space_id,
            HackingSessionState {
                container_id: cont_space_id,
                analyzer_module_id: 99,
                virus_coherence: 50.0,
                virus_strength: 20.0,
                core_hp_remaining: 35.0,
                unlocked: false,
            },
        );
    }

    for id in [cont_space_id, cont_station_id, cont_contract_id, sov_hub_id] {
        mark_hacking_container_unlocked(&ctx, char_id, id);
    }

    {
        let st = scanners_superweapons_state().read().unwrap();
        let hs = st.hacking_states.get(&cont_space_id).unwrap();
        assert_eq!(hs.core_hp_remaining, 0.0);
        assert!(hs.unlocked);
    }
    assert!(read_container_ship_state(&ctx).unlocked_containers.contains(&(cont_space_id, char_id)));
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&cont_space_id].flag, InventoryFlag::None);
        assert_eq!(snap.items[&cont_station_id].flag, InventoryFlag::Locked);
        assert_eq!(snap.items[&cont_contract_id].flag, InventoryFlag::Locked);
    });

    let now_unix = ctx.now_unix_secs();
    insert_item(&ctx, 1_000_700_010, 999_501, 1, sys_id as i64, InventoryFlag::None, 1, true);
    {
        let mut st = scanners_superweapons_state().write().unwrap();
        st.ship_dbuffs.insert(
            (ctx.instance_id, ship_id),
            vec![
                ActiveDbuffEntry { dbuff_id: 901, value: 999.0, expires_at_unix: now_unix - 1, source_module_id: 1 },
                ActiveDbuffEntry { dbuff_id: 901, value: 3.0, expires_at_unix: now_unix + 120, source_module_id: 2 },
            ],
        );
        st.system_wide_effects.insert(sys_id, 999_502);
        st.system_wide_effect_lists.insert(sys_id, vec![999_502]);
    }

    let agg = get_aggregated_ship_dbuffs(&ctx, ship_id, now_unix);
    assert_eq!(agg.get(&901), Some(&(3.0, now_unix + 3600)));
    assert_eq!(agg.get(&902), Some(&(50.0, now_unix + 3600)));
    assert_eq!(agg.get(&903), Some(&(777.0, now_unix + 3600)));
    assert_eq!(agg.get(&904), Some(&(25.0, now_unix + 3600)));

    let mut attrs = HashMap::from([(263u16, 1_000.0), (265, 2_000.0), (37, 300.0), (552, 100.0), (64, 2.0)]);
    apply_active_dbuffs_and_system_effects_to_ship_attrs(&ctx, ship_id, &mut attrs);
    assert!((attrs[&263] - 3_000.0).abs() < 1e-6);
    assert!((attrs[&265] - 6_000.0).abs() < 1e-6);
    assert!((attrs[&37] - 350.0).abs() < 1e-6);
    assert!((attrs[&552] - 777.0).abs() < 1e-6);
    assert!((attrs[&64] - 2.5).abs() < 1e-6);

    scanners_superweapons_state().write().unwrap().system_wide_effects.remove(&sys_id);
    scanners_superweapons_state().write().unwrap().system_wide_effect_lists.remove(&sys_id);
}

#[tokio::test]
async fn scanner_range_errors_relic_upwell_guard_and_ship_scanner_charges() {
    let ctx = test_ctx_with_custom(|sde| {
        sde.types.insert(22177, custom_inv_type(sde, 22177, 538, 7, "Relic Analyzer I", &[(54, 5_000.0), (1909, 40.0), (1910, 20.0)]));
        sde.types.insert(999_510, custom_inv_type(sde, 999_510, 47, 7, "Cargo Scanner I", &[(126, 5_000.0)]));
        sde.types.insert(999_511, custom_inv_type(sde, 999_511, 48, 7, "Ship Scanner I", &[(125, 5_000.0)]));
        sde.types.insert(999_512, custom_inv_type(sde, 999_512, 85, 8, "Antimatter Charge S", &[]));
    });

    let char_id = 96_100_010;
    let enemy_id = 96_100_011;
    let sys_id = 30_002_505;
    let ship_id = 1_000_700_101;
    let near_ship_id = 1_000_700_102;
    let far_ship_id = 1_000_700_103;
    let enemy_struct_id = 1_000_700_104;

    seed_char(&ctx, char_id, 98_000_010, sys_id, ship_id, None);
    seed_char(&ctx, enemy_id, 98_000_020, sys_id, near_ship_id, None);
    insert_item(&ctx, ship_id, 587, char_id, sys_id as i64, InventoryFlag::None, 1, true);
    insert_item(&ctx, near_ship_id, 587, enemy_id, sys_id as i64, InventoryFlag::None, 1, true);
    insert_item(&ctx, far_ship_id, 587, enemy_id, sys_id as i64, InventoryFlag::None, 1, true);
    insert_item(&ctx, 1_000_700_110, 22177, enemy_id, near_ship_id, InventoryFlag::HiSlot0, 1, true);
    insert_item(&ctx, 1_000_700_111, 999_512, enemy_id, near_ship_id, InventoryFlag::HiSlot0, 40, false);
    insert_item(&ctx, 1_000_700_112, 34, enemy_id, near_ship_id, InventoryFlag::Cargo, 120, false);

    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            enemy_struct_id,
            UpwellStructureRecord {
                structure_id: enemy_struct_id,
                type_id: 35832,
                owner_corp_id: 98_000_020,
                solar_system_id: sys_id,
                name: "Enemy Astrahus".into(),
                position: [1_000.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                shield_hp: 1_000_000.0,
                armor_hp: 1_000_000.0,
                hull_hp: 1_000_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
    });

    spawn_ball(&ctx, sys_id, ship_id, 587, 25, char_id, DVec3::ZERO).await;
    spawn_ball(&ctx, sys_id, near_ship_id, 587, 25, enemy_id, DVec3::new(1_000.0, 0.0, 0.0)).await;
    spawn_ball(&ctx, sys_id, far_ship_id, 587, 25, enemy_id, DVec3::new(25_000.0, 0.0, 0.0)).await;
    spawn_ball(&ctx, sys_id, enemy_struct_id, 35832, 1657, enemy_id, DVec3::new(1_000.0, 0.0, 0.0)).await;
    let sys = ctx.systems.get_or_spawn(sys_id);

    let relic_t = ctx.sde.types.get(&22177).unwrap().clone();
    let relic_st = DogmaItemState::from_sde(char_id, 22177, &ctx.sde);
    assert_eq!(
        handle_scanner_analyzer_entosis_activation(&ctx, &sys, char_id, ship_id, 501, &relic_t, &relic_st, Some(enemy_struct_id))
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "DataAnalyzerRequired"
    );

    let cs_t = ctx.sde.types.get(&999_510).unwrap().clone();
    let cs_st = DogmaItemState::from_sde(char_id, 999_510, &ctx.sde);
    for (tgt, expected) in [(None, "TargetRequired"), (Some(999_999_999), "TargetNotInSpace"), (Some(far_ship_id), "TargetOutOfRange")] {
        assert_eq!(
            handle_scanner_analyzer_entosis_activation(&ctx, &sys, char_id, ship_id, 502, &cs_t, &cs_st, tgt)
                .await
                .unwrap()
                .unwrap_err()
                .code,
            expected
        );
    }

    let ss_t = ctx.sde.types.get(&999_511).unwrap().clone();
    let ss_st = DogmaItemState::from_sde(char_id, 999_511, &ctx.sde);
    for (tgt, expected) in [(None, "TargetRequired"), (Some(far_ship_id), "TargetOutOfRange")] {
        assert_eq!(
            handle_scanner_analyzer_entosis_activation(&ctx, &sys, char_id, ship_id, 503, &ss_t, &ss_st, tgt)
                .await
                .unwrap()
                .unwrap_err()
                .code,
            expected
        );
    }

    let ss_res = handle_scanner_analyzer_entosis_activation(&ctx, &sys, char_id, ship_id, 503, &ss_t, &ss_st, Some(near_ship_id))
        .await
        .unwrap()
        .expect("ship scan");
    assert!(matches!(ss_res.dict_get("fittedModules"), Some(PyRep::List(m)) if m.len() == 1));
    assert!(matches!(ss_res.dict_get("charges"), Some(PyRep::List(c)) if c.len() == 1));
    assert_eq!(ss_res.dict_get("capacitor").and_then(|v| v.as_f64()), Some(1_200.0));

    let unk_t = ctx.sde.types.get(&587).unwrap().clone();
    let unk_st = DogmaItemState::from_sde(char_id, 587, &ctx.sde);
    assert!(handle_scanner_analyzer_entosis_activation(&ctx, &sys, char_id, ship_id, 504, &unk_t, &unk_st, Some(near_ship_id)).await.is_none());
}

#[tokio::test]
async fn superweapons_system_wide_effects_and_ship_clone_lifecycle_errors() {
    let ctx = test_ctx_with_custom(|sde| {
        sde.types.insert(671, custom_inv_type(sde, 671, 30, 6, "Erebus", &[]));
        sde.types.insert(23735, custom_inv_type(sde, 23735, 815, 7, "Clone Vat Bay I", &[(713, 16273.0), (714, 150.0)]));
        sde.types.insert(999_520, custom_inv_type(sde, 999_520, 920, 2, "Custom Phenomenon", &[(263, 1.25)]));
    });

    let offerer_id = 96_100_020;
    let target_id = 96_100_021;
    let outsider_id = 96_100_022;
    let fleet_id = 700_099;
    let sys_id = 30_002_505;
    let titan_id = 1_000_700_201;
    let vat_bay_id = 1_000_700_202;
    let other_ship_id = 1_000_700_203;

    seed_char(&ctx, offerer_id, 98_000_010, sys_id, titan_id, Some(fleet_id));
    seed_char(&ctx, target_id, 98_000_010, sys_id, other_ship_id, Some(fleet_id));
    seed_char(&ctx, outsider_id, 98_000_099, sys_id, other_ship_id, None);
    insert_item(&ctx, titan_id, 671, offerer_id, sys_id as i64, InventoryFlag::None, 1, true);
    insert_item(&ctx, vat_bay_id, 23735, offerer_id, titan_id, InventoryFlag::HiSlot0, 1, true);
    insert_item(&ctx, 1_000_700_204, 16273, offerer_id, titan_id, InventoryFlag::Cargo, 500, false);
    insert_item(&ctx, other_ship_id, 587, target_id, sys_id as i64, InventoryFlag::None, 1, true);
    spawn_ball(&ctx, sys_id, titan_id, 671, 30, offerer_id, DVec3::ZERO).await;

    ctx.db.with_mut_snapshot(|snap| {
        snap.fleets.insert(fleet_id, FleetRecord { fleet_id, boss_id: offerer_id, members: vec![offerer_id, target_id], ..Default::default() });
    });

    let kw = HashMap::new();
    let mut sess = SessionState {
        user_id: Some(offerer_id as u64),
        character_id: Some(offerer_id),
        corporation_id: Some(98_000_010),
        solar_system_id: Some(sys_id),
        ship_id: Some(titan_id),
        ..Default::default()
    };

    let mut docked_sess = sess.clone();
    docked_sess.station_id = Some(60_003_760);
    assert_eq!(
        handle_superweapons_dbuffs_and_clones(&ctx, &mut docked_sess, None, "superWeaponMgr", "ActivateSlashModule", &[], &kw).await.unwrap().unwrap_err().message,
        "CantDoThatWhileDocked"
    );

    let mut no_ship_sess = sess.clone();
    no_ship_sess.ship_id = None;
    assert_eq!(
        handle_superweapons_dbuffs_and_clones(&ctx, &mut no_ship_sess, None, "superWeaponMgr", "ActivateSinglePointTargetedModule", &[], &kw).await.unwrap().unwrap_err().code,
        "NoActiveShip"
    );

    let mut no_sys_sess = sess.clone();
    no_sys_sess.solar_system_id = None;
    assert_eq!(
        handle_superweapons_dbuffs_and_clones(&ctx, &mut no_sys_sess, None, "superWeaponMgr", "ActivateSinglePointTargetedModule", &[], &kw).await.unwrap().unwrap_err().code,
        "NotInSpace"
    );

    assert_eq!(
        handle_superweapons_dbuffs_and_clones(&ctx, &mut sess, None, "dogmaIM", "RefreshSystemWideEnvironment", &[], &kw).await.unwrap().unwrap_err().code,
        "GMRoleRequired"
    );
    assert_eq!(
        handle_superweapons_dbuffs_and_clones(&ctx, &mut sess, None, "dogmaIM", "ResetSystemWideEffects", &[], &kw).await.unwrap().unwrap_err().code,
        "GMRoleRequired"
    );

    let mut gm_sess = sess.clone();
    gm_sess.role = 0x7FFF_FFFF_FFFF_FFFF;
    let refresh_rep = handle_superweapons_dbuffs_and_clones(
        &ctx,
        &mut gm_sess,
        None,
        "dogmaIM",
        "RefreshSystemWideEnvironment",
        &[PyRep::Integer(sys_id as i64), PyRep::Integer(999_520)],
        &kw,
    )
    .await
    .unwrap()
    .expect("RefreshSystemWideEnvironment");
    assert_eq!(refresh_rep.dict_get("active").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(refresh_rep.dict_get("effectTypeID").and_then(|v| v.as_i64()), Some(999_520));

    let reset_rep = handle_superweapons_dbuffs_and_clones(&ctx, &mut gm_sess, None, "dogmaIM", "ResetSystemWideEffects", &[PyRep::Integer(sys_id as i64)], &kw)
        .await
        .unwrap()
        .expect("ResetSystemWideEffects");
    assert_eq!(reset_rep.dict_get("active").and_then(|v| v.as_bool()), Some(false));

    assert_eq!(
        handle_superweapons_dbuffs_and_clones(&ctx, &mut sess, None, "jumpCloneSvc", "OfferShipCloneInstallation", &[PyRep::Integer(target_id), PyRep::Integer(other_ship_id)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "MustPilotHostShip"
    );
    assert_eq!(
        handle_superweapons_dbuffs_and_clones(&ctx, &mut sess, None, "jumpCloneSvc", "OfferShipCloneInstallation", &[PyRep::Integer(target_id), PyRep::Integer(titan_id)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "NoActiveCloneVatBay"
    );

    let sys = ctx.systems.get_or_spawn(sys_id);
    let vat_t = ctx.sde.types.get(&23735).unwrap().clone();
    let vat_st = DogmaItemState::from_sde(offerer_id, 23735, &ctx.sde);
    handle_scanner_analyzer_entosis_activation(&ctx, &sys, offerer_id, titan_id, vat_bay_id, &vat_t, &vat_st, None).await.unwrap().expect("vat bay active");

    assert_eq!(
        handle_superweapons_dbuffs_and_clones(&ctx, &mut sess, None, "jumpCloneSvc", "OfferShipCloneInstallation", &[PyRep::Integer(outsider_id), PyRep::Integer(titan_id)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "TargetNotInSameFleetOrSystem"
    );

    handle_superweapons_dbuffs_and_clones(&ctx, &mut sess, None, "jumpCloneSvc", "OfferShipCloneInstallation", &[PyRep::Integer(target_id), PyRep::Integer(titan_id)], &kw)
        .await
        .unwrap()
        .expect("offer");
    assert_eq!(
        handle_superweapons_dbuffs_and_clones(&ctx, &mut sess, None, "jumpCloneSvc", "CancelShipCloneInstallation", &[], &kw).await.unwrap().unwrap(),
        PyRep::Bool(true)
    );

    let mut target_sess = SessionState {
        user_id: Some(target_id as u64),
        character_id: Some(target_id),
        corporation_id: Some(98_000_010),
        solar_system_id: Some(sys_id),
        ship_id: Some(other_ship_id),
        ..Default::default()
    };
    assert_eq!(
        handle_superweapons_dbuffs_and_clones(&ctx, &mut target_sess, None, "jumpCloneSvc", "AcceptShipCloneInstallation", &[], &kw).await.unwrap().unwrap_err().code,
        "NoCloneOfferFound"
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.jump_clones.insert(
            target_id,
            vec![
                JumpCloneRecord { jump_clone_id: 1, character_id: target_id, location_id: 60_003_760, implants: Vec::new() },
                JumpCloneRecord { jump_clone_id: 2, character_id: target_id, location_id: 60_003_760, implants: Vec::new() },
            ],
        );
    });
    handle_superweapons_dbuffs_and_clones(&ctx, &mut sess, None, "jumpCloneSvc", "OfferShipCloneInstallation", &[PyRep::Integer(target_id), PyRep::Integer(titan_id)], &kw)
        .await
        .unwrap()
        .expect("re-offer");
    assert_eq!(
        handle_superweapons_dbuffs_and_clones(&ctx, &mut target_sess, None, "jumpCloneSvc", "AcceptShipCloneInstallation", &[], &kw).await.unwrap().unwrap_err().code,
        "JumpCloneLimitReached"
    );

    assert!(handle_superweapons_dbuffs_and_clones(&ctx, &mut sess, None, "jumpCloneSvc", "GetShipCloneState", &[], &kw).await.unwrap().is_ok());
    assert!(handle_superweapons_dbuffs_and_clones(&ctx, &mut sess, None, "jumpCloneSvc", "", &[], &kw).await.is_none());
}

#[tokio::test]
async fn mutants_cov_clone_offers_dbuff_expiry_abyssal_weather_and_rpcs() {
    use crate::services::pve_special_sites_abyssal::{
        read_abyssal_state, with_abyssal_state, AbyssalWeatherKind,
    };
    use eve_db::{AbyssalPocketSession, AbyssalRoomRecord};
    use eve_machonet::service_roles;

    let ctx = test_ctx_with_custom(|sde| {
        sde.types.insert(
            999_701,
            custom_inv_type(sde, 999_701, 30, 6, "Test Avatar", &[]),
        );
        sde.types.insert(
            23_735,
            custom_inv_type(sde, 23_735, 725, 7, "Clone Vat Bay I", &[]),
        );
        sde.dbuff_collections.insert(
            911,
            DbuffCollectionInfo {
                dbuff_id: 911,
                name: "dbuff_911".into(),
                aggregate_mode: "Maximum".into(),
                operation: "PostMul".into(),
                display_name_id: None,
                show_output_value_in_ui: Some("ShowNormal".into()),
                item_modifiers: vec![263],
                location_modifiers: Vec::new(),
                location_group_modifiers: Vec::new(),
                location_required_skill_modifiers: Vec::new(),
                target_attribute_ids: Vec::new(),
            },
        );
        sde.dbuff_collections.insert(
            912,
            DbuffCollectionInfo {
                dbuff_id: 912,
                name: "dbuff_912".into(),
                aggregate_mode: "Maximum".into(),
                operation: "ModAdd".into(),
                display_name_id: None,
                show_output_value_in_ui: Some("ShowNormal".into()),
                item_modifiers: vec![37],
                location_modifiers: Vec::new(),
                location_group_modifiers: Vec::new(),
                location_required_skill_modifiers: Vec::new(),
                target_attribute_ids: Vec::new(),
            },
        );
    });

    let char_id = 96_700_001i64;
    let target_id = 96_700_002i64;
    let corp_id = 98_700_001i64;
    let sys_id = 30_000_142u32;
    let titan_id = 1_000_750_001i64;
    let vat_mod_id = 1_000_750_002i64;
    let target_ship_id = 1_000_750_003i64;
    let fleet_id = 1_000_750_901i64;
    let kw = HashMap::new();

    seed_char(&ctx, char_id, corp_id, sys_id, titan_id, Some(fleet_id));
    seed_char(&ctx, target_id, corp_id, sys_id, target_ship_id, Some(fleet_id));
    insert_item(&ctx, titan_id, 999_701, char_id, i64::from(sys_id), InventoryFlag::None, 1, true);
    insert_item(&ctx, vat_mod_id, 23_735, char_id, titan_id, InventoryFlag::HiSlot0, 1, true);
    insert_item(&ctx, target_ship_id, 587, target_id, i64::from(sys_id), InventoryFlag::None, 1, true);

    let now_unix = ctx.now_unix_secs();
    ctx.db.with_mut_snapshot(|snap| {
        snap.fleets.insert(
            fleet_id,
            FleetRecord {
                fleet_id,
                boss_id: char_id,
                motd: String::new(),
                free_move: false,
                is_registered: false,
                members: vec![char_id, target_id],
                ..Default::default()
            },
        );
        snap.module_online_states.insert(vat_mod_id, true);
        snap.module_active_states.insert(vat_mod_id);
        let mut vat_state = crate::services::character_clone_jump_rules::load_clone_vat_bay_state(snap);
        vat_state.active_by_ship.insert(titan_id, (vat_mod_id, now_unix + 600));
        crate::services::character_clone_jump_rules::save_clone_vat_bay_state(snap, &vat_state);
    });

    let mut host_sess = SessionState {
        user_id: Some(char_id as u64),
        character_id: Some(char_id),
        corporation_id: Some(corp_id),
        solar_system_id: Some(sys_id),
        ship_id: Some(titan_id),
        fleet_id: Some(fleet_id),
        role: service_roles::ROLE_GML,
        ..Default::default()
    };
    let mut target_sess = SessionState {
        user_id: Some(target_id as u64),
        character_id: Some(target_id),
        corporation_id: Some(corp_id),
        solar_system_id: Some(sys_id),
        ship_id: Some(target_ship_id),
        fleet_id: Some(fleet_id),
        ..Default::default()
    };

    // 1. load_ship_clone_offers / save_ship_clone_offers via Offer -> Cancel -> Accept failure -> Offer -> Accept success -> duplicate Accept failure
    let offered = handle_superweapons_dbuffs_and_clones(
        &ctx, &mut host_sess, None, "jumpCloneSvc", "OfferShipCloneInstallation",
        &[PyRep::Integer(target_id), PyRep::Integer(titan_id)], &kw,
    ).await.unwrap().unwrap();
    assert_eq!(offered.dict_get("offered").and_then(PyRep::as_bool), Some(true));
    assert_eq!(offered.dict_get("hostShipID").and_then(PyRep::as_i64), Some(titan_id));
    assert_eq!(offered.dict_get("targetCharID").and_then(PyRep::as_i64), Some(target_id));

    let clone_state_before = handle_superweapons_dbuffs_and_clones(
        &ctx, &mut target_sess, None, "jumpCloneSvc", "GetShipCloneState", &[], &kw,
    ).await.unwrap().unwrap();
    assert!(!matches!(clone_state_before, PyRep::None));

    // Cancel clears persisted offer
    assert_eq!(
        handle_superweapons_dbuffs_and_clones(
            &ctx, &mut target_sess, None, "jumpCloneSvc", "CancelShipCloneInstallation", &[], &kw,
        ).await.unwrap().unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(
        handle_superweapons_dbuffs_and_clones(
            &ctx, &mut target_sess, None, "jumpCloneSvc", "AcceptShipCloneInstallation",
            &[PyRep::Integer(titan_id)], &kw,
        ).await.unwrap().unwrap_err().code,
        "NoCloneOfferFound"
    );

    // Re-offer -> Accept succeeds & removes offer & persists JumpCloneRecord
    handle_superweapons_dbuffs_and_clones(
        &ctx, &mut host_sess, None, "jumpCloneSvc", "OfferShipCloneInstallation",
        &[PyRep::Integer(target_id), PyRep::Integer(titan_id)], &kw,
    ).await.unwrap().unwrap();
    let accepted = handle_superweapons_dbuffs_and_clones(
        &ctx, &mut target_sess, None, "jumpCloneSvc", "AcceptShipCloneInstallation",
        &[PyRep::Integer(titan_id)], &kw,
    ).await.unwrap().unwrap();
    assert_eq!(accepted.dict_get("installed").and_then(PyRep::as_bool), Some(true));
    assert_eq!(accepted.dict_get("characterID").and_then(PyRep::as_i64), Some(target_id));
    assert_eq!(accepted.dict_get("locationID").and_then(PyRep::as_i64), Some(titan_id));
    let jc_id = accepted.dict_get("jumpCloneID").and_then(PyRep::as_i64).unwrap();
    ctx.db.with_snapshot(|snap| {
        let clones = snap.jump_clones.get(&target_id).expect("jump clones");
        assert!(clones.iter().any(|c| c.jump_clone_id == jc_id && c.location_id == titan_id));
    });
    assert_eq!(
        handle_superweapons_dbuffs_and_clones(
            &ctx, &mut target_sess, None, "jumpCloneSvc", "AcceptShipCloneInstallation",
            &[PyRep::Integer(titan_id)], &kw,
        ).await.unwrap().unwrap_err().code,
        "NoCloneOfferFound"
    );

    // 2. active_ship_dbuff_gang_bursts exact expires_at_unix == now_unix pruning boundary
    assert!(active_ship_dbuff_gang_bursts(&ctx, titan_id, now_unix).is_empty());
    {
        let mut st = scanners_superweapons_state().write().unwrap();
        st.ship_dbuffs.insert(
            (ctx.instance_id, titan_id),
            vec![
                ActiveDbuffEntry { dbuff_id: 911, value: 2.0, expires_at_unix: now_unix - 1, source_module_id: 75 },
                ActiveDbuffEntry { dbuff_id: 911, value: 3.0, expires_at_unix: now_unix, source_module_id: 76 },
                ActiveDbuffEntry { dbuff_id: 912, value: 42.0, expires_at_unix: now_unix + 1, source_module_id: 77 },
            ],
        );
    }
    let bursts = active_ship_dbuff_gang_bursts(&ctx, titan_id, now_unix);
    assert_eq!(bursts.len(), 1);
    assert_eq!(bursts[0].source_item_id, 77);
    assert_eq!(bursts[0].dbuff_id, Some(912));
    assert_eq!(bursts[0].value, 42.0);

    // 3. apply_active_dbuffs_and_system_effects_to_ship_attrs_excluding: exclusion + Abyssal pocket matching (fleet_ship_ids, completed/collapsed skip, abyssal_solar_system_id fallback)
    scanners_superweapons_state().write().unwrap().ship_dbuffs.insert(
        (ctx.instance_id, target_ship_id),
        vec![
            ActiveDbuffEntry { dbuff_id: 911, value: 4.0, expires_at_unix: now_unix + 100, source_module_id: 81 },
            ActiveDbuffEntry { dbuff_id: 912, value: 25.0, expires_at_unix: now_unix + 100, source_module_id: 82 },
        ],
    );
    let mut excl_attrs: HashMap<u16, f64> = HashMap::from([(263u16, 100.0), (37u16, 200.0)]);
    apply_active_dbuffs_and_system_effects_to_ship_attrs_excluding(&ctx, target_ship_id, &[911], &mut excl_attrs);
    assert!((excl_attrs[&263] - 100.0).abs() < 1e-6);
    assert!((excl_attrs[&37] - 225.0).abs() < 1e-6);

    let ab_sys_completed = 32_990_101u32;
    let ab_sys_active = 32_990_102u32;
    let completed_ship_id = 1_000_750_101i64;
    let fleet_ship_id = 1_000_750_102i64;
    let solo_in_ab_sys_ship_id = 1_000_750_103i64;
    let collapsed_in_ab_sys_ship_id = 1_000_750_104i64;
    let leader_ship_id = 1_000_750_105i64;
    let ab_sys_collapsed = 32_990_103u32;
    insert_item(&ctx, completed_ship_id, 587, 96_700_010, i64::from(sys_id), InventoryFlag::None, 1, true);
    insert_item(&ctx, fleet_ship_id, 587, 96_700_011, i64::from(sys_id), InventoryFlag::None, 1, true);
    insert_item(&ctx, solo_in_ab_sys_ship_id, 587, 96_700_012, i64::from(ab_sys_active), InventoryFlag::None, 1, true);
    insert_item(&ctx, collapsed_in_ab_sys_ship_id, 587, 96_700_013, i64::from(ab_sys_collapsed), InventoryFlag::None, 1, true);

    let dummy_rooms = |pid: i64| -> [AbyssalRoomRecord; 3] {
        [1u8, 2, 3].map(|r| AbyssalRoomRecord {
            room_index: r,
            dungeon_id: 6046,
            dungeon_name: "Test".into(),
            npc_ball_ids: Vec::new(),
            cache_item_id: 1_896_000_000 + pid * 10 + i64::from(r),
            cleared: false,
            gate_unlocked_at_sec: None,
        })
    };
    with_abyssal_state(&ctx, |st| {
        st.pockets.insert(
            96_700_010,
            AbyssalPocketSession {
                pocket_id: 901,
                character_id: 96_700_010,
                ship_id: 1_000_750_199,
                origin_solar_system_id: sys_id,
                abyssal_solar_system_id: ab_sys_completed,
                origin_position: [0.0; 3],
                grid_offset: [0.0; 3],
                filament_type_id: 47765,
                tier: 1,
                weather: AbyssalWeatherKind::Electrical,
                weather_penalty_pct: 50.0,
                weather_bonus_pct: 50.0,
                npc_difficulty_mult: 1.0,
                current_room: 1,
                rooms: dummy_rooms(901),
                started_at_sec: now_unix,
                collapse_at_sec: now_unix + 1200,
                completed: true,
                collapsed_destroyed: false,
                trace_ball_id: 0,
                fleet_ship_ids: vec![completed_ship_id],
                ship_room: HashMap::new(),
                max_ships: 3,
            },
        );
        st.pockets.insert(
            96_700_013,
            AbyssalPocketSession {
                pocket_id: 902,
                character_id: 96_700_013,
                ship_id: 1_000_750_198,
                origin_solar_system_id: sys_id,
                abyssal_solar_system_id: ab_sys_collapsed,
                origin_position: [0.0; 3],
                grid_offset: [0.0; 3],
                filament_type_id: 47765,
                tier: 1,
                weather: AbyssalWeatherKind::Electrical,
                weather_penalty_pct: 50.0,
                weather_bonus_pct: 50.0,
                npc_difficulty_mult: 1.0,
                current_room: 1,
                rooms: dummy_rooms(902),
                started_at_sec: now_unix,
                collapse_at_sec: now_unix + 1200,
                completed: false,
                collapsed_destroyed: true,
                trace_ball_id: 0,
                fleet_ship_ids: Vec::new(),
                ship_room: HashMap::new(),
                max_ships: 1,
            },
        );
        st.pockets.insert(
            96_700_011,
            AbyssalPocketSession {
                pocket_id: 903,
                character_id: 96_700_011,
                ship_id: leader_ship_id,
                origin_solar_system_id: sys_id,
                abyssal_solar_system_id: ab_sys_active,
                origin_position: [0.0; 3],
                grid_offset: [0.0; 3],
                filament_type_id: 47765,
                tier: 1,
                weather: AbyssalWeatherKind::Electrical,
                weather_penalty_pct: 50.0,
                weather_bonus_pct: 50.0,
                npc_difficulty_mult: 1.0,
                current_room: 1,
                rooms: dummy_rooms(903),
                started_at_sec: now_unix,
                collapse_at_sec: now_unix + 1200,
                completed: false,
                collapsed_destroyed: false,
                trace_ball_id: 0,
                fleet_ship_ids: vec![leader_ship_id, fleet_ship_id],
                ship_room: HashMap::new(),
                max_ships: 3,
            },
        );
    });
    assert!(read_abyssal_state(&ctx, |st| st.pockets.contains_key(&96_700_011)));

    // Completed & collapsed pockets must NOT apply weather
    let mut comp_attrs = HashMap::from([(271u16, 0.5)]);
    apply_active_dbuffs_and_system_effects_to_ship_attrs(&ctx, completed_ship_id, &mut comp_attrs);
    assert!((comp_attrs[&271] - 0.5).abs() < 1e-6);

    let mut coll_attrs = HashMap::from([(271u16, 0.5)]);
    apply_active_dbuffs_and_system_effects_to_ship_attrs(&ctx, collapsed_in_ab_sys_ship_id, &mut coll_attrs);
    assert!((coll_attrs[&271] - 0.5).abs() < 1e-6);

    // Active pocket applies via fleet_ship_ids (when ship_id != leader_ship_id) AND via abyssal_solar_system_id fallback
    let mut fleet_attrs = HashMap::from([(271u16, 0.5)]);
    apply_active_dbuffs_and_system_effects_to_ship_attrs(&ctx, fleet_ship_id, &mut fleet_attrs);
    assert!((fleet_attrs[&271] - 0.75).abs() < 1e-6);

    let mut solo_ab_attrs = HashMap::from([(271u16, 0.5)]);
    apply_active_dbuffs_and_system_attrs_helper(&ctx, solo_in_ab_sys_ship_id, &mut solo_ab_attrs);
    assert!((solo_ab_attrs[&271] - 0.75).abs() < 1e-6);

    // 4. Non-default RPC return assertions for ApplyBuffsToItems, GetDbuffStateForClient, RefreshSystemWideEnvironment, GetSystemWideEffectsOnShip, ResetSystemWideEffects
    let applied = handle_superweapons_dbuffs_and_clones(
        &ctx, &mut host_sess, None, "dogmaIM", "ApplyBuffsToItems",
        &[PyRep::Integer(titan_id), PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(911), PyRep::Real(2.0)])]), PyRep::Real(60.0)],
        &kw,
    ).await.unwrap().unwrap();
    assert_eq!(applied.dict_get("shipID").and_then(PyRep::as_i64), Some(titan_id));
    assert!(applied.dict_get("activeCount").and_then(PyRep::as_i64).unwrap_or(0) >= 1);

    let mapped = handle_superweapons_dbuffs_and_clones(
        &ctx, &mut host_sess, None, "dogmaIM", "GetDbuffStateForClient", &[PyRep::Integer(titan_id)], &kw,
    ).await.unwrap().unwrap();
    assert_eq!(mapped.dict_get("shipID").and_then(PyRep::as_i64), Some(titan_id));
    assert!(mapped.dict_get("dbuffs").is_some());

    let reset = handle_superweapons_dbuffs_and_clones(
        &ctx, &mut host_sess, None, "dogmaIM", "ResetSystemWideEffects", &[PyRep::Integer(i64::from(sys_id))], &kw,
    ).await.unwrap().unwrap();
    assert_eq!(reset.dict_get("solarSystemID").and_then(PyRep::as_i64), Some(i64::from(sys_id)));
    assert_eq!(reset.dict_get("active").and_then(PyRep::as_bool), Some(false));

    let sys_info = handle_superweapons_dbuffs_and_clones(
        &ctx, &mut host_sess, None, "dogmaIM", "GetSystemWideEffectsOnShip", &[], &kw,
    ).await.unwrap().unwrap();
    assert_eq!(sys_info.dict_get("hudVisible").and_then(PyRep::as_bool), Some(false));

    with_abyssal_state(&ctx, |st| {
        st.pockets.remove(&96_700_010);
        st.pockets.remove(&96_700_011);
        st.pockets.remove(&96_700_013);
    });
}

fn apply_active_dbuffs_and_system_attrs_helper(
    ctx: &ServerContext,
    ship_id: i64,
    attrs: &mut HashMap<u16, f64>,
) {
    apply_active_dbuffs_and_system_effects_to_ship_attrs_excluding(ctx, ship_id, &[], attrs);
}

