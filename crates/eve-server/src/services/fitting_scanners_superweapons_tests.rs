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
