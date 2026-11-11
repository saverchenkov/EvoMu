#![forbid(unsafe_code)]

use super::*;
use crate::router::ServerContext;
use eve_db::{
    FleetRecord, InventoryFlag, InventoryItemRecord, UpwellStructureRecord, VirtualServerClock,
    WorldRepository,
};
use eve_destiny::{Ball, CombatState, MovementMode, SlimItemMeta};
use eve_dogma::DogmaItemState;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::{InvType, SdeRegistry};
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
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_800_000_000)));
    ctx
}

fn ensure_char(ctx: &ServerContext, char_id: i64, corp_id: i64, sys_id: u32, ship_id: i64, fleet_id: Option<i64>) {
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().expect("demo seed character");
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
        snap.characters.entry(char_id).or_insert(ch);
        snap.skills.entry(char_id).or_default().insert(
            24242,
            eve_db::TrainedSkillRecord {
                type_id: 24242,
                level: 5,
                skill_points: 256_000.0,
            },
        );
        snap.accounts.insert(
            format!("user_{char_id}"),
            eve_db::AccountRecord {
                user_id: char_id as u64,
                username: format!("user_{char_id}"),
                password_token: String::new(),
                role: 0,
                banned_until_unix: None,
                ban_reason: None,
            },
        );
    });
    let _ = ctx
        .db
        .acquire_online_lock(char_id as u64, char_id as u64, Some(char_id), true, ctx.now_unix_secs());
}

fn insert_item(
    ctx: &ServerContext,
    item_id: i64,
    type_id: u32,
    owner_id: i64,
    location_id: i64,
    flag: InventoryFlag,
    quantity: i32,
    singleton: bool,
) {
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            item_id,
            InventoryItemRecord {
                item_id,
                type_id,
                owner_id,
                location_id,
                flag,
                quantity,
                singleton,
                ..Default::default()
            },
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
    let meta = SlimItemMeta {
        group_id,
        category_id: 6,
        owner_id,
        char_id: Some(owner_id),
        corp_id: 98_000_001,
        alliance_id: 0,
        security_status: 5.0,
        is_cloaked: false,
        ..Default::default()
    };
    sys.spawn_ball_with_meta(ball, CombatState::default(), meta)
        .await
        .expect("spawn ball");
}

#[tokio::test]
async fn data_relic_analyzer_cargo_scanner_and_ship_scanner_activation() {
    let ctx = test_ctx_with_custom(|sde| {
        sde.types.insert(
            999_401,
            custom_inv_type(sde, 999_401, 1202, 6, "Test Prorator", &[(1886, 1.0)]),
        );
        sde.types.insert(
            999_402,
            custom_inv_type(sde, 999_402, 538, 7, "Data Analyzer I", &[(54, 15_000.0)]),
        );
        sde.types.insert(
            999_403,
            custom_inv_type(sde, 999_403, 47, 7, "Cargo Scanner I", &[(54, 50_000.0)]),
        );
        sde.types.insert(
            999_404,
            custom_inv_type(sde, 999_404, 48, 7, "Ship Scanner I", &[(125, 50_000.0)]),
        );
        sde.types.insert(
            484,
            custom_inv_type(sde, 484, 55, 7, "Light Neutron Blaster I", &[(54, 2_000.0)]),
        );
    });

    let char_id = 96_000_001;
    let target_owner_id = 96_000_002;
    let sys_id = 30_002_505;
    let ship_id = 1_000_600_001;
    let container_id = 1_000_600_002;
    let target_ship_id = 1_000_600_003;
    let blockade_runner_id = 1_000_600_004;
    let analyzer_id = 1_000_600_010;
    let cargo_scan_id = 1_000_600_011;
    let ship_scan_id = 1_000_600_012;
    let gun_id = 1_000_600_013;
    let cargo_item_id = 1_000_600_014;
    let own_structure_id = 1_000_600_020;

    ensure_char(&ctx, char_id, 98_000_010, sys_id, ship_id, None);
    ensure_char(&ctx, target_owner_id, 98_000_020, sys_id, target_ship_id, None);
    insert_item(&ctx, ship_id, 587, char_id, sys_id as i64, InventoryFlag::None, 1, true);
    insert_item(&ctx, container_id, 17366, target_owner_id, sys_id as i64, InventoryFlag::Locked, 1, true);
    insert_item(&ctx, target_ship_id, 587, target_owner_id, sys_id as i64, InventoryFlag::None, 1, true);
    insert_item(&ctx, gun_id, 484, target_owner_id, target_ship_id, InventoryFlag::HiSlot0, 1, true);
    insert_item(&ctx, cargo_item_id, 34, target_owner_id, target_ship_id, InventoryFlag::Cargo, 500, false);
    insert_item(&ctx, blockade_runner_id, 999_401, target_owner_id, sys_id as i64, InventoryFlag::None, 1, true);
    insert_item(&ctx, 1_000_600_015, 34, target_owner_id, blockade_runner_id, InventoryFlag::Cargo, 999, false);

    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            own_structure_id,
            UpwellStructureRecord {
                structure_id: own_structure_id,
                type_id: 35832,
                owner_corp_id: 98_000_010,
                solar_system_id: sys_id,
                name: "Own Astrahus".into(),
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
    spawn_ball(&ctx, sys_id, container_id, 17366, 448, target_owner_id, DVec3::new(1_000.0, 0.0, 0.0)).await;
    spawn_ball(&ctx, sys_id, target_ship_id, 587, 25, target_owner_id, DVec3::new(2_000.0, 0.0, 0.0)).await;
    spawn_ball(&ctx, sys_id, blockade_runner_id, 999_401, 1202, target_owner_id, DVec3::new(2_500.0, 0.0, 0.0)).await;
    spawn_ball(&ctx, sys_id, own_structure_id, 35832, 1657, char_id, DVec3::new(1_000.0, 0.0, 0.0)).await;

    let sys = ctx.systems.get_or_spawn(sys_id);

    // 1. Data Analyzer starts hacking and mark_hacking_container_unlocked unlocks Locked flag
    let analyzer_type = ctx.sde.types.get(&999_402).unwrap().clone();
    let analyzer_state = DogmaItemState::from_sde(char_id, 999_402, &ctx.sde);

    assert_eq!(
        handle_scanner_analyzer_entosis_activation(&ctx, &sys, char_id, ship_id, analyzer_id, &analyzer_type, &analyzer_state, None)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "TargetRequired"
    );
    assert_eq!(
        handle_scanner_analyzer_entosis_activation(&ctx, &sys, char_id, ship_id, analyzer_id, &analyzer_type, &analyzer_state, Some(own_structure_id))
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "CannotHackOwnStructure"
    );

    assert!(
        handle_scanner_analyzer_entosis_activation(&ctx, &sys, char_id, ship_id, analyzer_id, &analyzer_type, &analyzer_state, Some(container_id))
            .await
            .unwrap()
            .is_ok()
    );
    assert_eq!(active_hacking_target_for_char(char_id), Some(container_id));
    mark_hacking_container_unlocked(&ctx, char_id, container_id);
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items.get(&container_id).unwrap().flag, InventoryFlag::None);
    });

    // 2. Cargo Scanner on normal ship vs Blockade Runner
    let cs_type = ctx.sde.types.get(&999_403).unwrap().clone();
    let cs_state = DogmaItemState::from_sde(char_id, 999_403, &ctx.sde);

    let scan_ok = handle_scanner_analyzer_entosis_activation(
        &ctx,
        &sys,
        char_id,
        ship_id,
        cargo_scan_id,
        &cs_type,
        &cs_state,
        Some(target_ship_id),
    )
    .await
    .unwrap()
    .expect("cargo scan normal ship");
    assert_eq!(scan_ok.dict_get("totalScannedQuantity").and_then(|v| v.as_i64()), Some(500));

    let immune_scan = handle_scanner_analyzer_entosis_activation(
        &ctx,
        &sys,
        char_id,
        ship_id,
        cargo_scan_id,
        &cs_type,
        &cs_state,
        Some(blockade_runner_id),
    )
    .await
    .unwrap()
    .expect("cargo scan blockade runner");
    assert_eq!(immune_scan.dict_get("cargoScanImmune").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(immune_scan.dict_get("totalScannedQuantity").and_then(|v| v.as_i64()), Some(0));

    // 3. Ship Scanner reveals fitted modules on target_ship_id
    let ss_type = ctx.sde.types.get(&999_404).unwrap().clone();
    let ss_state = DogmaItemState::from_sde(char_id, 999_404, &ctx.sde);
    let ss_ok = handle_scanner_analyzer_entosis_activation(
        &ctx,
        &sys,
        char_id,
        ship_id,
        ship_scan_id,
        &ss_type,
        &ss_state,
        Some(target_ship_id),
    )
    .await
    .unwrap()
    .expect("ship scan");
    assert!(matches!(ss_ok.dict_get("fittedModules"), Some(PyRep::List(m)) if !m.is_empty()));
}

#[tokio::test]
async fn dbuffs_system_wide_effects_and_ship_clone_vat_installation() {
    let ctx = test_ctx_with_custom(|sde| {
        sde.types.insert(671, custom_inv_type(sde, 671, 30, 6, "Erebus", &[]));
        sde.types.insert(
            23735,
            custom_inv_type(sde, 23735, 815, 7, "Clone Vat Bay I", &[(713, 16273.0), (714, 150.0)]),
        );
    });
    let offerer_id = 96_000_010;
    let receiver_id = 96_000_011;
    let fleet_id = 700_040;
    let sys_id = 30_002_505;
    let titan_id = 1_000_600_101;
    let vat_bay_id = 1_000_600_102;
    let receiver_ship_id = 1_000_600_103;
    let ozone_id = 1_000_600_104;

    ensure_char(&ctx, offerer_id, 98_000_010, sys_id, titan_id, Some(fleet_id));
    ensure_char(&ctx, receiver_id, 98_000_010, sys_id, receiver_ship_id, Some(fleet_id));
    // 671 = Erebus (Titan), 23735 = Clone Vat Bay I
    insert_item(&ctx, titan_id, 671, offerer_id, sys_id as i64, InventoryFlag::None, 1, true);
    insert_item(&ctx, vat_bay_id, 23735, offerer_id, titan_id, InventoryFlag::HiSlot0, 1, true);
    insert_item(&ctx, ozone_id, 16273, offerer_id, titan_id, InventoryFlag::Cargo, 500, false);
    insert_item(&ctx, receiver_ship_id, 587, receiver_id, sys_id as i64, InventoryFlag::None, 1, true);
    spawn_ball(&ctx, sys_id, titan_id, 671, 30, offerer_id, DVec3::ZERO).await;

    ctx.db.with_mut_snapshot(|snap| {
        snap.fleets.insert(
            fleet_id,
            FleetRecord {
                fleet_id,
                boss_id: offerer_id,
                members: vec![offerer_id, receiver_id],
                ..Default::default()
            },
        );
    });

    let kw = HashMap::new();
    let mut non_gm_sess = SessionState {
        user_id: Some(offerer_id as u64),
        character_id: Some(offerer_id),
        corporation_id: Some(98_000_010),
        solar_system_id: Some(sys_id),
        ship_id: Some(titan_id),
        ..Default::default()
    };

    // 1. ApplyBuffsToItems requires GM role; GM succeeds and populates dbuffs
    assert_eq!(
        handle_superweapons_dbuffs_and_clones(
            &ctx,
            &mut non_gm_sess,
            None,
            "dogmaIM",
            "ApplyBuffsToItems",
            &[PyRep::Integer(receiver_ship_id), PyRep::Integer(12), PyRep::Real(25.0)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "GMRoleRequired"
    );

    let mut gm_sess = non_gm_sess.clone();
    gm_sess.role = 0x7FFF_FFFF_FFFF_FFFF;
    gm_sess.ship_id = Some(receiver_ship_id);
    let apply_ok = handle_superweapons_dbuffs_and_clones(
        &ctx,
        &mut gm_sess,
        None,
        "dogmaIM",
        "ApplyBuffsToItems",
        &[
            PyRep::List(vec![PyRep::Integer(receiver_ship_id)]),
            PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(12), PyRep::Real(25.0)])]),
            PyRep::Real(120.0),
        ],
        &kw,
    )
    .await
    .unwrap()
    .expect("GM ApplyBuffsToItems");
    assert_eq!(apply_ok.dict_get("activeCount").and_then(|v| v.as_i64()), Some(1));

    let state_rep = handle_superweapons_dbuffs_and_clones(
        &ctx,
        &mut non_gm_sess,
        None,
        "dogmaIM",
        "GetDbuffStateForClient",
        &[PyRep::Integer(receiver_ship_id)],
        &kw,
    )
    .await
    .unwrap()
    .expect("GetDbuffStateForClient");
    assert_eq!(state_rep.dict_get("activeCount").and_then(|v| v.as_i64()), Some(1));

    let mut ship_attrs = HashMap::from([(263u16, 1_000.0), (265, 1_000.0)]);
    apply_active_dbuffs_and_system_effects_to_ship_attrs(&ctx, receiver_ship_id, &mut ship_attrs);

    // 2. System-wide effects RPCs (including Abyssal weather system 32_000_001)
    let mut abyss_sess = non_gm_sess.clone();
    abyss_sess.solar_system_id = Some(32_000_001);
    let swe = handle_superweapons_dbuffs_and_clones(
        &ctx,
        &mut abyss_sess,
        None,
        "dogmaIM",
        "GetSystemWideEffectsOnShip",
        &[PyRep::Integer(receiver_ship_id)],
        &kw,
    )
    .await
    .unwrap()
    .expect("GetSystemWideEffectsOnShip");
    assert!(swe.dict_get("solarSystemID").is_some());

    // 3. Activate Ship Clone Vat Bay, then offer -> accept flow
    let sys = ctx.systems.get_or_spawn(sys_id);
    let vat_type = ctx.sde.types.get(&23735).unwrap().clone();
    let vat_state = DogmaItemState::from_sde(offerer_id, 23735, &ctx.sde);
    let _ = handle_scanner_analyzer_entosis_activation(
        &ctx,
        &sys,
        offerer_id,
        titan_id,
        vat_bay_id,
        &vat_type,
        &vat_state,
        None,
    )
    .await
    .unwrap()
    .expect("activate clone vat bay");

    let offer_rep = handle_superweapons_dbuffs_and_clones(
        &ctx,
        &mut non_gm_sess,
        None,
        "jumpCloneSvc",
        "OfferShipCloneInstallation",
        &[PyRep::Integer(receiver_id), PyRep::Integer(titan_id)],
        &kw,
    )
    .await
    .unwrap()
    .expect("OfferShipCloneInstallation");
    assert_eq!(offer_rep.dict_get("offered").and_then(|v| v.as_bool()), Some(true));

    let mut receiver_sess = SessionState {
        user_id: Some(receiver_id as u64),
        character_id: Some(receiver_id),
        corporation_id: Some(98_000_010),
        solar_system_id: Some(sys_id),
        ship_id: Some(receiver_ship_id),
        ..Default::default()
    };
    let accept_rep = handle_superweapons_dbuffs_and_clones(
        &ctx,
        &mut receiver_sess,
        None,
        "jumpCloneSvc",
        "AcceptShipCloneInstallation",
        &[PyRep::Integer(titan_id)],
        &kw,
    )
    .await
    .unwrap()
    .expect("AcceptShipCloneInstallation");
    assert_eq!(accept_rep.dict_get("installed").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(accept_rep.dict_get("locationID").and_then(|v| v.as_i64()), Some(titan_id));
}

#[tokio::test]
async fn dbuffs_apply_expired_pruning_storm_system_wide_and_godma_flush() {
    let storm_tid = 92_975u32;
    let custom_ship_tid = 92_587u32;
    let ctx = test_ctx_with_custom(|sde| {
        sde.effects.insert(
            59_920,
            eve_sde::DogmaEffectType {
                effect_id: 59_920,
                name: "TestStormEffect".into(),
                modifiers: vec![
                    eve_sde::EffectModifierSpec {
                        domain: "shipID".into(),
                        func: "ItemModifier".into(),
                        modified_attribute_id: 552,
                        modifying_attribute_id: 2469,
                        operator: 4,
                        group_id: None,
                        skill_type_id: None,
                        effect_id: None,
                    },
                    eve_sde::EffectModifierSpec {
                        domain: "shipID".into(),
                        func: "ItemModifier".into(),
                        modified_attribute_id: 9,
                        modifying_attribute_id: 2471,
                        operator: 2,
                        group_id: None,
                        skill_type_id: None,
                        effect_id: None,
                    },
                ],
                ..Default::default()
            },
        );
        let mut storm_t = custom_inv_type(
            sde,
            storm_tid,
            920,
            2,
            "Test Electrical Storm",
            &[(2469, 1.5), (2471, 250.0)],
        );
        storm_t.effect_ids = vec![59_920];
        sde.types.insert(storm_tid, storm_t);
        sde.types.insert(
            custom_ship_tid,
            custom_inv_type(sde, custom_ship_tid, 25, 6, "Test Ship", &[(9, 500.0), (552, 40.0), (37, 300.0)]),
        );
        sde.dbuff_collections.insert(
            9_001,
            eve_sde::DbuffCollectionInfo {
                dbuff_id: 9_001,
                name: "TestPostMul".into(),
                operation: "PostMul".into(),
                aggregate_mode: "Maximum".into(),
                display_name_id: None,
                show_output_value_in_ui: None,
                item_modifiers: vec![552],
                location_modifiers: Vec::new(),
                location_group_modifiers: Vec::new(),
                location_required_skill_modifiers: Vec::new(),
                target_attribute_ids: vec![552],
            },
        );
        sde.dbuff_collections.insert(
            9_002,
            eve_sde::DbuffCollectionInfo {
                dbuff_id: 9_002,
                name: "TestModAdd".into(),
                operation: "ModAdd".into(),
                aggregate_mode: "Maximum".into(),
                display_name_id: None,
                show_output_value_in_ui: None,
                item_modifiers: vec![9],
                location_modifiers: Vec::new(),
                location_group_modifiers: Vec::new(),
                location_required_skill_modifiers: Vec::new(),
                target_attribute_ids: vec![9],
            },
        );
        sde.dbuff_collections.insert(
            9_003,
            eve_sde::DbuffCollectionInfo {
                dbuff_id: 9_003,
                name: "TestPostPercent".into(),
                operation: "PostPercent".into(),
                aggregate_mode: "Maximum".into(),
                display_name_id: None,
                show_output_value_in_ui: None,
                item_modifiers: vec![37],
                location_modifiers: Vec::new(),
                location_group_modifiers: Vec::new(),
                location_required_skill_modifiers: Vec::new(),
                target_attribute_ids: vec![37],
            },
        );
    });

    let sys_id = 30_000_142u32;
    let char_id = 92_100_001i64;
    let ship1 = 92_100_011i64;
    let ship2 = 92_100_012i64;
    let storm_item_id = 92_100_099i64;
    let kw = HashMap::new();

    ensure_char(&ctx, char_id, 98_000_001, sys_id, ship1, None);
    insert_item(&ctx, ship1, custom_ship_tid, char_id, sys_id as i64, InventoryFlag::None, 1, true);
    insert_item(&ctx, ship2, custom_ship_tid, char_id, sys_id as i64, InventoryFlag::None, 1, true);

    let mut non_gm = SessionState {
        user_id: Some(char_id as u64),
        character_id: Some(char_id),
        solar_system_id: Some(sys_id),
        ship_id: Some(ship1),
        ..Default::default()
    };
    let mut gm = non_gm.clone();
    gm.role = u64::MAX;

    // Empty dbuffs on ship2 triggers OnGodmaFlushLocation notification.
    let _ = handle_superweapons_dbuffs_and_clones(
        &ctx,
        &mut non_gm,
        None,
        "dogmaIM",
        "GetMappedDbuffsForItem",
        &[PyRep::Integer(ship2)],
        &kw,
    )
    .await
    .unwrap()
    .expect("GetMappedDbuffsForItem empty");
    let flushed = ctx
        .pending_notifications
        .read()
        .unwrap()
        .get(&char_id)
        .is_some_and(|list| {
            list.iter()
                .any(|n| n.event == "OnGodmaFlushLocation" && n.args == vec![PyRep::Integer(ship2)])
        });
    assert!(flushed, "expected OnGodmaFlushLocation for empty dbuff ship");

    // ApplyBuffsToItems requires GM + validates W2 RpcArgs.
    assert_eq!(
        handle_superweapons_dbuffs_and_clones(
            &ctx,
            &mut non_gm,
            None,
            "dogmaIM",
            "ApplyBuffsToItems",
            &[PyRep::List(vec![PyRep::Integer(ship1)]), PyRep::Integer(9_003), PyRep::Real(25.0)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "GMRoleRequired"
    );
    assert_eq!(
        handle_superweapons_dbuffs_and_clones(
            &ctx,
            &mut gm,
            None,
            "dogmaIM",
            "ApplyBuffsToItems",
            &[PyRep::List(vec![PyRep::Integer(ship1)]), PyRep::Integer(-1), PyRep::Real(10.0)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidArgument"
    );
    assert_eq!(
        handle_superweapons_dbuffs_and_clones(
            &ctx,
            &mut gm,
            None,
            "dogmaIM",
            "ApplyBuffsToItems",
            &[PyRep::List(vec![PyRep::Integer(ship1)]), PyRep::Integer(9_003), PyRep::Real(f64::NAN)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidArgument"
    );

    // Apply batch runtime dbuff 9003 (+25% speed), seed storm object 92975 (PostMul 1.5x sig, ModAdd +250 hull).
    handle_superweapons_dbuffs_and_clones(
        &ctx,
        &mut gm,
        None,
        "dogmaIM",
        "ApplyBuffsToItems",
        &[
            PyRep::List(vec![PyRep::Integer(ship1), PyRep::Integer(ship2)]),
            PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(9_003), PyRep::Real(25.0)])]),
        ],
        &kw,
    )
    .await
    .unwrap()
    .expect("ApplyBuffsToItems batch");
    insert_item(&ctx, storm_item_id, storm_tid, 1, sys_id as i64, InventoryFlag::None, 1, true);

    let mut attrs = HashMap::from([(552u16, 40.0), (9u16, 500.0), (37u16, 300.0)]);
    apply_active_dbuffs_and_system_effects_to_ship_attrs(&ctx, ship1, &mut attrs);
    assert!((attrs[&552] - 60.0).abs() < 1e-6, "PostMul 40*1.5=60, got {}", attrs[&552]);
    assert!((attrs[&9] - 750.0).abs() < 1e-6, "ModAdd 500+250=750, got {}", attrs[&9]);
    assert!((attrs[&37] - 375.0).abs() < 1e-6, "PostPercent 300*1.25=375, got {}", attrs[&37]);

    // Advancing clock expires runtime dbuffs while retaining system-wide storm item effects.
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_800_005_000)));
    let mut attrs_after_exp = HashMap::from([(552u16, 40.0), (9u16, 500.0), (37u16, 300.0)]);
    apply_active_dbuffs_and_system_effects_to_ship_attrs(&ctx, ship1, &mut attrs_after_exp);
    assert!((attrs_after_exp[&552] - 60.0).abs() < 1e-6);
    assert!((attrs_after_exp[&9] - 750.0).abs() < 1e-6);
    assert!((attrs_after_exp[&37] - 300.0).abs() < 1e-6);

    // RefreshSystemWideEnvironment & ResetSystemWideEffects require GM and update active state.
    assert_eq!(
        handle_superweapons_dbuffs_and_clones(&ctx, &mut non_gm, None, "dogmaIM", "ResetSystemWideEffects", &[PyRep::Integer(sys_id as i64)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "GMRoleRequired"
    );
    let ref_rep = handle_superweapons_dbuffs_and_clones(
        &ctx,
        &mut gm,
        None,
        "dogmaIM",
        "RefreshSystemWideEnvironment",
        &[PyRep::Integer(sys_id as i64), PyRep::String("Electrical Storm".into())],
        &kw,
    )
    .await
    .unwrap()
    .expect("RefreshSystemWideEnvironment");
    assert_eq!(ref_rep.dict_get("solarSystemID"), Some(&PyRep::Integer(sys_id as i64)));

    let reset_rep = handle_superweapons_dbuffs_and_clones(
        &ctx,
        &mut gm,
        None,
        "dogmaIM",
        "ResetSystemWideEffects",
        &[PyRep::Integer(sys_id as i64)],
        &kw,
    )
    .await
    .unwrap()
    .expect("ResetSystemWideEffects");
    assert_eq!(reset_rep.dict_get("active"), Some(&PyRep::Bool(false)));
}

#[tokio::test]
async fn superweapons_rpc_args_and_jump_clone_cancel_offerer_target_state_edges() {
    let ctx = test_ctx_with_custom(|_| {});
    let sys_id = 30_000_142u32;
    let host_id = 92_200_001i64;
    let guest_id = 92_200_002i64;
    let titan_id = 92_200_011i64;
    let guest_ship_id = 92_200_012i64;
    let vat_bay_id = 92_200_021i64;
    let kw = HashMap::new();

    ensure_char(&ctx, host_id, 98_000_001, sys_id, titan_id, Some(92_900_001));
    ensure_char(&ctx, guest_id, 98_000_001, sys_id, guest_ship_id, Some(92_900_001));
    ctx.db.with_mut_snapshot(|snap| {
        snap.fleets.insert(
            92_900_001,
            eve_db::FleetRecord {
                fleet_id: 92_900_001,
                boss_id: host_id,
                members: vec![host_id, guest_id],
                ..Default::default()
            },
        );
        snap.skills.entry(guest_id).or_default().insert(
            24242,
            eve_db::TrainedSkillRecord {
                type_id: 24242,
                level: 1,
                skill_points: 250.0,
            },
        );
    });
    // Avatar (11567) Titan + Clone Vat Bay (23735) + Liquid Ozone (16273)
    insert_item(&ctx, titan_id, 11567, host_id, sys_id as i64, InventoryFlag::None, 1, true);
    insert_item(&ctx, guest_ship_id, 587, guest_id, sys_id as i64, InventoryFlag::None, 1, true);
    insert_item(&ctx, vat_bay_id, 23735, host_id, titan_id, InventoryFlag::HiSlot0, 1, true);
    insert_item(&ctx, vat_bay_id + 1, 16273, host_id, titan_id, InventoryFlag::Cargo, 500, false);
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(vat_bay_id, true);
    });
    spawn_ball(&ctx, sys_id, titan_id, 11567, 30, host_id, DVec3::ZERO).await;
    spawn_ball(&ctx, sys_id, guest_ship_id, 587, 25, guest_id, DVec3::new(1_000.0, 0.0, 0.0)).await;

    // superWeaponMgr precondition & W2 RpcArgs guards.
    let mut docked_sess = SessionState {
        user_id: Some(host_id as u64),
        character_id: Some(host_id),
        station_id: Some(60_003_760),
        solar_system_id: Some(sys_id),
        ship_id: Some(titan_id),
        ..Default::default()
    };
    assert_eq!(
        handle_superweapons_dbuffs_and_clones(
            &ctx,
            &mut docked_sess,
            None,
            "superWeaponMgr",
            "ActivateSinglePointTargetedModule",
            &[PyRep::Integer(1)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "CantDoThatWhileDocked"
    );

    let mut no_ship_sess = SessionState {
        user_id: Some(host_id as u64),
        character_id: Some(host_id),
        solar_system_id: Some(sys_id),
        ship_id: None,
        ..Default::default()
    };
    assert_eq!(
        handle_superweapons_dbuffs_and_clones(
            &ctx,
            &mut no_ship_sess,
            None,
            "superWeaponMgr",
            "ActivateSinglePointTargetedModule",
            &[PyRep::Integer(1)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "NoActiveShip"
    );

    let mut no_sys_sess = SessionState {
        user_id: Some(host_id as u64),
        character_id: Some(host_id),
        solar_system_id: None,
        ship_id: Some(titan_id),
        ..Default::default()
    };
    assert_eq!(
        handle_superweapons_dbuffs_and_clones(
            &ctx,
            &mut no_sys_sess,
            None,
            "superWeaponMgr",
            "ActivateSlashModule",
            &[PyRep::Integer(1)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "NotInSpace"
    );

    let mut host_sess = SessionState {
        user_id: Some(host_id as u64),
        character_id: Some(host_id),
        solar_system_id: Some(sys_id),
        ship_id: Some(titan_id),
        ..Default::default()
    };
    for method in ["ActivateSinglePointTargetedModule", "ActivateSlashModule"] {
        assert_eq!(
            handle_superweapons_dbuffs_and_clones(
                &ctx,
                &mut host_sess,
                None,
                "superWeaponMgr",
                method,
                &[PyRep::Integer(0)],
                &kw,
            )
            .await
            .unwrap()
            .unwrap_err()
            .code,
            "InvalidArgument" // rpc-lint: allow test fixture
        );
    }

    // jumpCloneSvc: Offer error checks before vat bay activation.
    assert_eq!(
        handle_superweapons_dbuffs_and_clones(
            &ctx,
            &mut host_sess,
            None,
            "jumpCloneSvc",
            "OfferShipCloneInstallation",
            &[PyRep::Integer(guest_id), PyRep::Integer(titan_id + 999)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "MustPilotHostShip"
    );
    assert_eq!(
        handle_superweapons_dbuffs_and_clones(
            &ctx,
            &mut host_sess,
            None,
            "jumpCloneSvc",
            "OfferShipCloneInstallation",
            &[PyRep::Integer(guest_id), PyRep::Integer(titan_id)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "NoActiveCloneVatBay"
    );

    // Activate Clone Vat Bay -> out-of-fleet check -> offer & target-side cancel -> re-offer & offerer-side cancel -> accept & clone limit reached.
    let sys = ctx.systems.get_or_spawn(sys_id);
    let vat_type = ctx.sde.types.get(&23735).unwrap().clone();
    let vat_state = DogmaItemState::from_sde(host_id, 23735, &ctx.sde);
    handle_scanner_analyzer_entosis_activation(&ctx, &sys, host_id, titan_id, vat_bay_id, &vat_type, &vat_state, None)
        .await
        .unwrap()
        .expect("vat bay activation");

    assert_eq!(
        handle_superweapons_dbuffs_and_clones(
            &ctx,
            &mut host_sess,
            None,
            "jumpCloneSvc",
            "OfferShipCloneInstallation",
            &[PyRep::Integer(99_999_888), PyRep::Integer(titan_id)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "TargetNotInSameFleetOrSystem"
    );

    handle_superweapons_dbuffs_and_clones(
        &ctx,
        &mut host_sess,
        None,
        "jumpCloneSvc",
        "OfferShipCloneInstallation",
        &[PyRep::Integer(guest_id), PyRep::Integer(titan_id)],
        &kw,
    )
    .await
    .unwrap()
    .expect("OfferShipCloneInstallation 1");

    let mut guest_sess = SessionState {
        user_id: Some(guest_id as u64),
        character_id: Some(guest_id),
        solar_system_id: Some(sys_id),
        ship_id: Some(guest_ship_id),
        ..Default::default()
    };
    assert_eq!(
        handle_superweapons_dbuffs_and_clones(
            &ctx,
            &mut guest_sess,
            None,
            "jumpCloneSvc",
            "CancelShipCloneInstallation",
            &[],
            &kw,
        )
        .await
        .unwrap()
        .unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(
        handle_superweapons_dbuffs_and_clones(
            &ctx,
            &mut guest_sess,
            None,
            "jumpCloneSvc",
            "AcceptShipCloneInstallation",
            &[PyRep::Integer(titan_id)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "NoCloneOfferFound"
    );

    // Re-offer and cancel from offerer side.
    handle_superweapons_dbuffs_and_clones(
        &ctx,
        &mut host_sess,
        None,
        "jumpCloneSvc",
        "OfferShipCloneInstallation",
        &[PyRep::Integer(guest_id), PyRep::Integer(titan_id)],
        &kw,
    )
    .await
    .unwrap()
    .expect("OfferShipCloneInstallation 2");
    assert_eq!(
        handle_superweapons_dbuffs_and_clones(
            &ctx,
            &mut host_sess,
            None,
            "jumpCloneSvc",
            "CancelShipCloneInstallation",
            &[],
            &kw,
        )
        .await
        .unwrap()
        .unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(
        handle_superweapons_dbuffs_and_clones(
            &ctx,
            &mut guest_sess,
            None,
            "jumpCloneSvc",
            "AcceptShipCloneInstallation",
            &[PyRep::Integer(titan_id)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "NoCloneOfferFound"
    );

    // Accept once -> GetShipCloneState reflects clone -> 2nd Accept hits JumpCloneLimitReached.
    handle_superweapons_dbuffs_and_clones(
        &ctx,
        &mut host_sess,
        None,
        "jumpCloneSvc",
        "OfferShipCloneInstallation",
        &[PyRep::Integer(guest_id), PyRep::Integer(titan_id)],
        &kw,
    )
    .await
    .unwrap()
    .expect("OfferShipCloneInstallation 3");
    handle_superweapons_dbuffs_and_clones(
        &ctx,
        &mut guest_sess,
        None,
        "jumpCloneSvc",
        "AcceptShipCloneInstallation",
        &[PyRep::Integer(titan_id)],
        &kw,
    )
    .await
    .unwrap()
    .expect("Accept 1");
    let state_rep = handle_superweapons_dbuffs_and_clones(
        &ctx,
        &mut guest_sess,
        None,
        "jumpCloneSvc",
        "GetShipCloneState",
        &[],
        &kw,
    )
    .await
    .unwrap()
    .expect("GetShipCloneState");
    assert_ne!(state_rep, PyRep::None);

    handle_superweapons_dbuffs_and_clones(
        &ctx,
        &mut host_sess,
        None,
        "jumpCloneSvc",
        "OfferShipCloneInstallation",
        &[PyRep::Integer(guest_id), PyRep::Integer(titan_id)],
        &kw,
    )
    .await
    .unwrap()
    .expect("OfferShipCloneInstallation 4");
    assert_eq!(
        handle_superweapons_dbuffs_and_clones(
            &ctx,
            &mut guest_sess,
            None,
            "jumpCloneSvc",
            "AcceptShipCloneInstallation",
            &[PyRep::Integer(titan_id)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "JumpCloneLimitReached"
    );
}

