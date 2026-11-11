//! Unit tests for `notify_emitters_fleet_combat`.

use super::*;
use crate::router::{BoundObjectEntry, NotificationTarget, ServerContext};
use eve_db::clock::VirtualServerClock;
use eve_db::{InventoryFlag, InventoryItemRecord, WorldRepository};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::{InvType, SdeRegistry};
use std::collections::HashMap;
use std::sync::Arc;

fn make_test_type(type_id: u32, group_id: u32) -> InvType {
    InvType {
        type_id,
        group_id,
        category_id: 2,
        name: format!("Container-{type_id}"),
        mass: 100.0,
        volume: 100.0,
        capacity: 1_000.0,
        portion_size: 1,
        radius: 10.0,
        base_price: 10_000.0,
        published: true,
        market_group_id: None,
        race_id: None,
        faction_id: None,
        variation_parent_type_id: None,
        description_id: None,
        icon_id: None,
        graphic_id: None,
        sound_id: None,
        meta_group_id: None,
        meta_level: None,
        tech_level: Some(1),
        default_effect_id: None,
        description: String::new(),
        effect_ids: Vec::new(),
        default_attributes: Default::default(),
    }
}

fn make_online_ctx() -> (ServerContext, i64) {
    let mut sde = SdeRegistry::default();
    // Group 340 = Secure Cargo Container, Group 448 = ALSC, Group 649 = Freight Container
    sde.types.insert(3465, make_test_type(3465, 340));
    sde.types.insert(17366, make_test_type(17366, 448));
    sde.types.insert(23003, make_test_type(23003, 649));

    let db = Arc::new(WorldRepository::with_demo_seed());
    let char_id = 90_000_001;
    db.acquire_online_lock(1, 501, Some(char_id), true, 1_775_400_000)
        .expect("online lock");
    let ctx = ServerContext::with_sde_and_repository(Arc::new(sde), db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_775_400_000)));
    (ctx, char_id)
}

fn make_item(item_id: i64, type_id: u32, owner_id: i64, location_id: i64) -> InventoryItemRecord {
    InventoryItemRecord {
        item_id,
        type_id,
        owner_id,
        location_id,
        flag: InventoryFlag::Hangar,
        quantity: 1,
        singleton: true,
        version: 1,
        dynamic_attributes: HashMap::new(),
        source_mutaplasmid_id: None,
        insured_payout_isk: 0.0,
        is_blueprint_copy: false,
        blueprint_runs_remaining: None,
    }
}

#[test]
fn emitters_format_all_fleet_combat_and_crimewatch_notifications() {
    let (ctx, char_id) = make_online_ctx();
    let sys_id = 30_000_142_u32;

    emit_operation_replayed(&ctx, char_id, 3, 14);
    emit_ui_reveal_forced(&ctx, char_id);
    emit_password_changed(&ctx, char_id, 1, 700_001, 17366);

    // Empty location_ids is a no-op
    emit_invalidate_cache_for_locations(&ctx, NotificationTarget::Character(char_id), &[]);
    emit_invalidate_cache_for_locations(
        &ctx,
        NotificationTarget::Character(char_id),
        &[60_003_760, 700_001],
    );

    // Empty events is a no-op
    emit_multi_event(&ctx, char_id, &[]);
    emit_multi_event(&ctx, char_id, &[("lock", 800_001, "target")]);

    // Equal LP points is a no-op
    emit_solar_system_lp_change(&ctx, sys_id, 100, 100);
    emit_solar_system_lp_change(&ctx, sys_id, 100, 250);

    emit_victory_point_state_updated(&ctx, sys_id, 1500, 42.5, 3000);
    emit_weapons_timer_update(&ctx, char_id, 1, PyRep::Integer(123_456));
    emit_pvp_timer_update(&ctx, char_id, 2, PyRep::Integer(234_567));
    emit_criminal_timer_update(&ctx, char_id, 3, PyRep::Integer(345_678));

    // All empty slices is a no-op
    emit_system_criminal_flag_updates(&ctx, sys_id, &[], &[], &[]);
    emit_system_criminal_flag_updates(&ctx, sys_id, &[11], &[22], &[33]);

    // Non-positive char_id is a no-op
    emit_ship_death(&ctx, 0);
    emit_ship_death(&ctx, -10);
    emit_ship_death(&ctx, char_id);

    let drained = ctx.drain_notifications_for_char(char_id);
    assert_eq!(drained.len(), 12);

    assert_eq!(drained[0].event, "OnOperationReplayed");
    assert_eq!(drained[0].args, vec![PyRep::Integer(3), PyRep::Integer(14)]);

    assert_eq!(drained[1].event, "OnUiRevealForced");
    assert!(drained[1].args.is_empty());

    assert_eq!(drained[2].event, "OnPasswordChanged");
    assert_eq!(drained[2].args[0], PyRep::Integer(1));
    assert_eq!(
        drained[2].args[1],
        PyRep::keyval([
            ("itemID", PyRep::Integer(700_001)),
            ("typeID", PyRep::Integer(17366)),
        ])
    );

    assert_eq!(drained[3].event, "OnInvalidateCacheForLocations");
    assert_eq!(
        drained[3].args,
        vec![PyRep::List(vec![
            PyRep::Integer(60_003_760),
            PyRep::Integer(700_001),
        ])]
    );

    assert_eq!(drained[4].event, "OnMultiEvent");

    assert_eq!(drained[5].event, "OnSolarSystemLPChange");
    assert_eq!(drained[5].id_type, "solarsystemid");
    assert_eq!(
        drained[5].args,
        vec![PyRep::Integer(100), PyRep::Integer(250)]
    );

    assert_eq!(drained[6].event, "OnVictoryPointStateUpdated");
    assert_eq!(drained[6].id_type, "solarsystemid");
    assert_eq!(drained[6].args[0], PyRep::Integer(i64::from(sys_id)));

    assert_eq!(drained[7].event, "OnWeaponsTimerUpdate");
    assert_eq!(
        drained[7].args,
        vec![PyRep::Integer(1), PyRep::Integer(123_456)]
    );

    assert_eq!(drained[8].event, "OnPvpTimerUpdate");
    assert_eq!(
        drained[8].args,
        vec![PyRep::Integer(2), PyRep::Integer(234_567)]
    );

    assert_eq!(drained[9].event, "OnCriminalTimerUpdate");
    assert_eq!(
        drained[9].args,
        vec![PyRep::Integer(3), PyRep::Integer(345_678)]
    );

    assert_eq!(drained[10].event, "OnSystemCriminalFlagUpdates");
    assert_eq!(drained[11].event, "OnShipDeath");
    assert!(drained[11].args.is_empty());
}

#[tokio::test]
async fn handle_set_password_validates_container_ownership_passwords_and_alsc_config() {
    let (ctx, char_id) = make_online_ctx();
    let station_id = 60_003_760_i64;
    let alsc_id = 777_001_i64;
    let freight_id = 777_002_i64;
    let remote_id = 777_003_i64;
    let corp_cont_id = 777_004_i64;
    let corp_id = 1_000_009_i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items
            .insert(alsc_id, make_item(alsc_id, 17366, char_id, station_id));
        snap.items
            .insert(freight_id, make_item(freight_id, 23003, char_id, station_id));
        snap.items
            .insert(remote_id, make_item(remote_id, 17366, char_id, 60_004_000));
        snap.items
            .insert(corp_cont_id, make_item(corp_cont_id, 17366, corp_id, station_id));
    });

    let mut session = SessionState::new(501);
    session.user_id = Some(1);
    session.character_id = Some(char_id);
    session.corporation_id = Some(corp_id);
    session.station_id = Some(station_id);
    session.solar_system_id = Some(30_000_142);

    // Invalid slot (which = 0 or 3)
    let err = handle_set_password(
        &ctx,
        &session,
        None,
        "SetPassword",
        &[PyRep::Integer(alsc_id), PyRep::Integer(3), PyRep::String("abc".into())],
        &HashMap::new(),
    )
    .await
    .expect_err("which=3 rejected");
    assert_eq!(err.code, "InvalidPasswordType");

    // Non-string password rejected
    let err = handle_set_password(
        &ctx,
        &session,
        None,
        "SetPassword",
        &[PyRep::Integer(alsc_id), PyRep::Integer(1), PyRep::Integer(999)],
        &HashMap::new(),
    )
    .await
    .expect_err("non-string password rejected");
    assert_eq!(err.code, "InvalidPassword");

    // Freight container (group 649) rejected
    let err = handle_set_password(
        &ctx,
        &session,
        None,
        "SetPassword",
        &[PyRep::Integer(freight_id), PyRep::Integer(1), PyRep::String("pw".into())],
        &HashMap::new(),
    )
    .await
    .expect_err("freight container rejected");
    assert_eq!(err.code, "ContainerDoesNotSupportPassword");

    // Remote container rejected for ColocationRequired
    let err = handle_set_password(
        &ctx,
        &session,
        None,
        "SetPassword",
        &[PyRep::Integer(remote_id), PyRep::Integer(1), PyRep::String("pw".into())],
        &HashMap::new(),
    )
    .await
    .expect_err("remote container rejected");
    assert_eq!(err.code, "ColocationRequired");

    // Corp container without director/equipment role rejected
    let err = handle_set_password(
        &ctx,
        &session,
        None,
        "SetPassword",
        &[PyRep::Integer(corp_cont_id), PyRep::Integer(1), PyRep::String("pw".into())],
        &HashMap::new(),
    )
    .await
    .expect_err("missing corp role rejected");
    assert_eq!(err.code, "CorpRoleRequired");

    // Set general password (which=1) via bound entry
    let bound = BoundObjectEntry {
        bound_id: 1,
        session_id: 501,
        user_id: 1,
        character_id: char_id,
        service: "invbroker".into(),
        object_id: alsc_id,
        oid_str: "N=888444:1".into(),
        ref_id: 1,
    };
    let ok = handle_set_password(
        &ctx,
        &session,
        Some(&bound),
        "SetPassword",
        &[PyRep::Integer(1), PyRep::String("secret1".into())],
        &HashMap::new(),
    )
    .await
    .expect("set password succeeds");
    assert_eq!(
        ok,
        PyRep::keyval([
            ("containerID", PyRep::Integer(alsc_id)),
            ("passwordSet", PyRep::Bool(true)),
        ])
    );

    // Changing existing password without matching oldPassword fails and logs ALSCActionPasswordFailure
    let err = handle_set_password(
        &ctx,
        &session,
        Some(&bound),
        "SetPassword",
        &[
            PyRep::Integer(1),
            PyRep::String("secret2".into()),
            PyRep::String("wrong".into()),
        ],
        &HashMap::new(),
    )
    .await
    .expect_err("wrong old password rejected");
    assert_eq!(err.code, "InvalidPassword");

    // Changing with matching oldPassword via ConfigureALSC updates bitmask flags
    let mut kwargs = HashMap::new();
    kwargs.insert("configBitmask".into(), PyRep::Integer(3));
    let ok2 = handle_set_password(
        &ctx,
        &session,
        Some(&bound),
        "ConfigureALSC",
        &[
            PyRep::Integer(1),
            PyRep::String("secret2".into()),
            PyRep::String("secret1".into()),
        ],
        &kwargs,
    )
    .await
    .expect("ConfigureALSC succeeds");
    assert_eq!(
        ok2,
        PyRep::keyval([
            ("containerID", PyRep::Integer(alsc_id)),
            ("passwordSet", PyRep::Bool(true)),
        ])
    );
}

#[tokio::test]
async fn set_password_covers_in_space_guards_station_override_corp_logs_and_clear() {
    let (ctx, char_id) = make_online_ctx();
    let corp_id = 98_000_001_i64;
    let sys_id = 30_000_142_u32;
    let station_id = 60_003_760_i64;
    let ship_id = 100_888_900_i64;
    let space_alsc_id = 888_501_i64;
    let cargo_alsc_id = 888_502_i64;
    let other_ship_alsc_id = 888_503_i64;
    let corp_alsc_id = 888_504_i64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = Some(station_id);
            ch.solar_system_id = sys_id;
            ch.corporation_id = corp_id;
            ch.active_ship_id = ship_id;
        }
        snap.items.insert(
            space_alsc_id,
            InventoryItemRecord {
                location_id: i64::from(sys_id),
                flag: InventoryFlag::None,
                ..make_item(space_alsc_id, 17366, char_id, i64::from(sys_id))
            },
        );
        snap.items.insert(
            cargo_alsc_id,
            InventoryItemRecord {
                flag: InventoryFlag::Cargo,
                ..make_item(cargo_alsc_id, 17366, char_id, ship_id)
            },
        );
        snap.items.insert(
            other_ship_alsc_id,
            InventoryItemRecord {
                flag: InventoryFlag::Cargo,
                ..make_item(other_ship_alsc_id, 17366, char_id, ship_id + 99)
            },
        );
        snap.items.insert(
            corp_alsc_id,
            make_item(corp_alsc_id, 17366, corp_id, station_id),
        );
    });

    // 1. Station-bound override: bound object == caller_loc && args[0] > 2 switches to unbound args parsing.
    let mut docked_sess = SessionState::new(501);
    docked_sess.user_id = Some(1);
    docked_sess.character_id = Some(char_id);
    docked_sess.corporation_id = Some(corp_id);
    docked_sess.corp_role = eve_db::corp_roles::ROLE_EQUIPMENT_CONFIG;
    docked_sess.station_id = Some(station_id);
    docked_sess.solar_system_id = Some(sys_id);
    docked_sess.ship_id = Some(ship_id);

    let station_bound = BoundObjectEntry {
        bound_id: 2,
        session_id: 501,
        user_id: 1,
        character_id: char_id,
        service: "invbroker".into(),
        object_id: station_id,
        oid_str: format!("N={station_id}:2"),
        ref_id: 2,
    };

    // Unrecognized/zero `which` -> InvalidPasswordType
    let bad_which = handle_set_password(
        &ctx,
        &docked_sess,
        Some(&station_bound),
        "SetPassword",
        &[PyRep::Integer(corp_alsc_id), PyRep::Integer(0), PyRep::String("pw".into())],
        &HashMap::new(),
    )
    .await
    .expect_err("which == 0 rejected");
    assert_eq!(bad_which.code, "InvalidPasswordType");

    // Corp container config password (which == 2) with ROLE_EQUIPMENT_CONFIG succeeds and records corp log,
    // without touching `unlocked_containers` (since which == 2 is config slot).
    let mut cfg_kw = HashMap::new();
    cfg_kw.insert("lockByDefault".into(), PyRep::Bool(true));
    cfg_kw.insert("passwordPerAction".into(), PyRep::Bool(false));
    let corp_res = handle_set_password(
        &ctx,
        &docked_sess,
        Some(&station_bound),
        "ALSCConfigSet",
        &[
            PyRep::Integer(corp_alsc_id),
            PyRep::Integer(2),
            PyRep::String("cfg_secret".into()),
        ],
        &cfg_kw,
    )
    .await
    .expect("corp ALSCConfigSet succeeds");
    assert_eq!(
        corp_res,
        PyRep::keyval([
            ("containerID", PyRep::Integer(corp_alsc_id)),
            ("passwordSet", PyRep::Bool(true)),
        ])
    );
    ctx.db.with_snapshot(|snap| {
        let st = fitting_containers_ships::read_container_ship_state_snap(snap);
        assert!(st.get_container_password(corp_alsc_id, 2) == Some("cfg_secret"));
        assert!(!st.unlocked_containers.contains(&(corp_alsc_id, char_id)));
        let cfg = st.container_configs.get(&corp_alsc_id).expect("config stored");
        assert!(cfg.lock_by_default);
        assert!(!cfg.password_per_action);
        let logs = snap.corp_container_logs.get(&corp_id).expect("corp log written");
        assert!(logs
            .iter()
            .any(|l| l.container_id == corp_alsc_id && l.password_type.as_deref() == Some("config")));
    });

    // Item-bound PyRep::String first arg defaults `which = 1` + PyRep::None clears password
    let item_bound = BoundObjectEntry {
        bound_id: 3,
        session_id: 501,
        user_id: 1,
        character_id: char_id,
        service: "invbroker".into(),
        object_id: corp_alsc_id,
        oid_str: format!("N={corp_alsc_id}:3"),
        ref_id: 3,
    };
    let set_gen = handle_set_password(
        &ctx,
        &docked_sess,
        Some(&item_bound),
        "SetPassword",
        &[PyRep::String("gen_pw".into())],
        &HashMap::new(),
    )
    .await
    .expect("item-bound 1-arg string sets general password");
    assert_eq!(set_gen.dict_get("passwordSet"), Some(&PyRep::Bool(true)));
    let clr_gen = handle_set_password(
        &ctx,
        &docked_sess,
        Some(&item_bound),
        "SetPassword",
        &[PyRep::None, PyRep::String("gen_pw".into())],
        &HashMap::new(),
    )
    .await
    .expect("clearing password with None sets passwordSet=false");
    assert_eq!(clr_gen.dict_get("passwordSet"), Some(&PyRep::Bool(false)));

    // 2. In-space checks: ShipNotInSpace, CannotDoWhileWarpingOrCloaked, ContainerNotInSpace,
    // OutsideOperationalRange, cargo colocation, and within-range success.
    let mut space_sess = docked_sess.clone();
    space_sess.station_id = None;
    let sys_handle = ctx.systems.get_or_spawn(sys_id);

    let no_ship = handle_set_password(
        &ctx,
        &space_sess,
        None,
        "SetPassword",
        &[PyRep::Integer(space_alsc_id), PyRep::String("pw".into())],
        &HashMap::new(),
    )
    .await
    .expect_err("ship not spawned rejected");
    assert_eq!(no_ship.code, "ShipNotInSpace");

    // In-space cargo inside active ship succeeds even without ball spawned; foreign ship cargo fails.
    let cargo_ok = handle_set_password(
        &ctx,
        &space_sess,
        None,
        "SetPassword",
        &[PyRep::Integer(cargo_alsc_id), PyRep::String("cargopw".into())],
        &HashMap::new(),
    )
    .await
    .expect("container in active ship cargo succeeds");
    assert_eq!(cargo_ok.dict_get("passwordSet"), Some(&PyRep::Bool(true)));
    let foreign_cargo = handle_set_password(
        &ctx,
        &space_sess,
        None,
        "SetPassword",
        &[PyRep::Integer(other_ship_alsc_id), PyRep::String("pw".into())],
        &HashMap::new(),
    )
    .await
    .expect_err("container inside another ship cargo rejected");
    assert_eq!(foreign_cargo.code, "ColocationRequired");

    // Spawn ship warping -> CannotDoWhileWarpingOrCloaked
    let mut ship_ball = eve_destiny::Ball {
        id: ship_id,
        type_id: 587,
        position: glam::DVec3::ZERO,
        velocity: glam::DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 1.0,
        mass: 1_000_000.0,
        radius: 10.0,
        speed_fraction: 0.0,
        mode: eve_destiny::MovementMode::Warp {
            origin: glam::DVec3::ZERO,
            destination: glam::DVec3::new(1_000_000.0, 0.0, 0.0),
            warp_speed_au_s: 3.0,
            total_distance: 1_000_000.0,
            phase: eve_destiny::WarpPhase::Cruising,
            elapsed_warp_time: 1.0,
        },
    };
    let _ = sys_handle.spawn_ball(ship_ball.clone()).await;
    let warp_err = handle_set_password(
        &ctx,
        &space_sess,
        None,
        "SetPassword",
        &[PyRep::Integer(space_alsc_id), PyRep::String("pw".into())],
        &HashMap::new(),
    )
    .await
    .expect_err("warping ship rejected");
    assert_eq!(warp_err.code, "CannotDoWhileWarpingOrCloaked");

    // Stop ship + cloak via CombatState -> CannotDoWhileWarpingOrCloaked
    ship_ball.mode = eve_destiny::MovementMode::Stop;
    let _ = sys_handle.spawn_ball(ship_ball).await;
    if let Ok(mut bp) = sys_handle.ballpark.write() {
        bp.combat_states.insert(
            ship_id,
            eve_destiny::CombatState {
                is_cloaked: true,
                ..Default::default()
            },
        );
    }
    let cloak_err = handle_set_password(
        &ctx,
        &space_sess,
        None,
        "SetPassword",
        &[PyRep::Integer(space_alsc_id), PyRep::String("pw".into())],
        &HashMap::new(),
    )
    .await
    .expect_err("cloaked ship rejected");
    assert_eq!(cloak_err.code, "CannotDoWhileWarpingOrCloaked");

    // Decloak without spawning container ball -> ContainerNotInSpace
    if let Ok(mut bp) = sys_handle.ballpark.write() {
        bp.combat_states.get_mut(&ship_id).unwrap().is_cloaked = false;
    }
    let no_cont = handle_set_password(
        &ctx,
        &space_sess,
        None,
        "SetPassword",
        &[PyRep::Integer(space_alsc_id), PyRep::String("pw".into())],
        &HashMap::new(),
    )
    .await
    .expect_err("unspawned space container rejected");
    assert_eq!(no_cont.code, "ContainerNotInSpace");

    // Spawn container at edge distance 2501 m (> 2500 m) -> OutsideOperationalRange
    let _ = sys_handle
        .spawn_ball(eve_destiny::Ball {
            id: space_alsc_id,
            type_id: 17366,
            position: glam::DVec3::new(2_521.0, 0.0, 0.0),
            velocity: glam::DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 100.0,
            radius: 10.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        })
        .await;
    let oob_err = handle_set_password(
        &ctx,
        &space_sess,
        None,
        "SetPassword",
        &[PyRep::Integer(space_alsc_id), PyRep::String("pw".into())],
        &HashMap::new(),
    )
    .await
    .expect_err("edge dist > 2500m rejected");
    assert_eq!(oob_err.code, "OutsideOperationalRange");

    // Move container within exact edge distance 2500 m -> succeeds
    let _ = sys_handle
        .spawn_ball(eve_destiny::Ball {
            id: space_alsc_id,
            type_id: 17366,
            position: glam::DVec3::new(2_520.0, 0.0, 0.0),
            velocity: glam::DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 100.0,
            radius: 10.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        })
        .await;
    let in_range_ok = handle_set_password(
        &ctx,
        &space_sess,
        None,
        "SetPassword",
        &[PyRep::Integer(space_alsc_id), PyRep::String("spacepw".into())],
        &HashMap::new(),
    )
    .await
    .expect("edge dist == 2500m succeeds");
    assert_eq!(in_range_ok.dict_get("passwordSet"), Some(&PyRep::Bool(true)));
}

#[tokio::test]
async fn handle_set_password_covers_binding_inspace_corp_logs_and_clear_paths() {
    let (ctx, char_id) = make_online_ctx();
    let station_id = 60_003_760_i64;
    let sys_id = 30_000_142_u32;
    let ship_id = 888_100_i64;
    let space_near_id = 888_201_i64;
    let space_far_id = 888_202_i64;
    let space_unspawned_id = 888_203_i64;
    let cargo_cont_id = 888_204_i64;
    let hangar_cont_id = 888_205_i64;
    let corp_cont_id = 888_206_i64;
    let other_char_cont_id = 888_207_i64;
    let corp_id = 98_000_001_i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items
            .insert(space_near_id, make_item(space_near_id, 3465, char_id, i64::from(sys_id)));
        snap.items
            .insert(space_far_id, make_item(space_far_id, 3465, char_id, i64::from(sys_id)));
        snap.items.insert(
            space_unspawned_id,
            make_item(space_unspawned_id, 3465, char_id, i64::from(sys_id)),
        );
        snap.items
            .insert(cargo_cont_id, make_item(cargo_cont_id, 17366, char_id, ship_id));
        snap.items
            .insert(hangar_cont_id, make_item(hangar_cont_id, 17366, char_id, station_id));
        let mut corp_item = make_item(corp_cont_id, 17366, corp_id, station_id);
        corp_item.flag = InventoryFlag::CorpSAG1;
        snap.items.insert(corp_cont_id, corp_item);
        snap.items
            .insert(other_char_cont_id, make_item(other_char_cont_id, 17366, 90_000_999, station_id));
    });

    // 1. Station-bound override + unbound args/kwargs variants + ItemNotFound + InvalidPasswordType
    let mut station_sess = SessionState::new(502);
    station_sess.user_id = Some(1);
    station_sess.character_id = Some(char_id);
    station_sess.corporation_id = Some(corp_id);
    station_sess.station_id = Some(station_id);
    station_sess.solar_system_id = Some(sys_id);

    let station_bound = BoundObjectEntry {
        bound_id: 2,
        session_id: 502,
        user_id: 1,
        character_id: char_id,
        service: "invbroker".into(),
        object_id: station_id,
        oid_str: format!("N=888445:{station_id}"),
        ref_id: 2,
    };
    let mut kw_which = HashMap::new();
    kw_which.insert("which".into(), PyRep::Integer(2)); // config password slot
    kw_which.insert("newPassword".into(), PyRep::String("cfgpw".into()));
    let cfg_ok = handle_set_password(
        &ctx,
        &station_sess,
        Some(&station_bound),
        "ALSCConfigSet",
        &[PyRep::Integer(hangar_cont_id)],
        &kw_which,
    )
    .await
    .expect("station-bound which=2 succeeds");
    assert_eq!(
        cfg_ok,
        PyRep::keyval([
            ("containerID", PyRep::Integer(hangar_cont_id)),
            ("passwordSet", PyRep::Bool(true)),
        ])
    );
    // ALSCConfigSet with lockByDefault / passwordPerAction kwargs does NOT emit OnPasswordChanged
    let mut kw_flags = HashMap::new();
    kw_flags.insert("lockByDefault".into(), PyRep::Bool(true));
    kw_flags.insert("passwordPerAction".into(), PyRep::Bool(true));
    kw_flags.insert("which".into(), PyRep::Integer(2));
    kw_flags.insert("password".into(), PyRep::String("cfgpw2".into()));
    kw_flags.insert("oldPassword".into(), PyRep::String("cfgpw".into()));
    let _ = ctx.drain_notifications_for_char(char_id);
    handle_set_password(
        &ctx,
        &station_sess,
        Some(&station_bound),
        "ALSCConfigSet",
        &[PyRep::Integer(hangar_cont_id)],
        &kw_flags,
    )
    .await
    .expect("ALSCConfigSet flags succeed");
    let alsc_notes = ctx.drain_notifications_for_char(char_id);
    assert!(!alsc_notes.iter().any(|n| n.event == "OnPasswordChanged"));
    assert!(alsc_notes.iter().any(|n| n.event == "OnInvalidateCacheForLocations"));
    // Slot 2 does not populate unlocked_containers
    let st_after_cfg =
        crate::services::inventory_fitting::fitting_containers_ships::read_container_ship_state(&ctx);
    assert!(!st_after_cfg.unlocked_containers.contains(&(hangar_cont_id, char_id)));

    // Item-bound string/None syntax (args = [String/None]) + clearing password emits OnPasswordChanged
    let item_bound = BoundObjectEntry {
        bound_id: 3,
        session_id: 502,
        user_id: 1,
        character_id: char_id,
        service: "invbroker".into(),
        object_id: hangar_cont_id,
        oid_str: format!("N=888446:{hangar_cont_id}"),
        ref_id: 3,
    };
    handle_set_password(
        &ctx,
        &station_sess,
        Some(&item_bound),
        "SetPassword",
        &[PyRep::String("genpw".into())],
        &HashMap::new(),
    )
    .await
    .expect("item-bound single string sets slot 1");
    let set_notes = ctx.drain_notifications_for_char(char_id);
    assert!(set_notes.iter().any(|n| n.event == "OnPasswordChanged"));

    let clr_res = handle_set_password(
        &ctx,
        &station_sess,
        None,
        "SetPassword",
        &[PyRep::Integer(hangar_cont_id), PyRep::None, PyRep::String("genpw".into())],
        &HashMap::new(),
    )
    .await
    .expect("unbound [cid, None, old] clears password");
    assert_eq!(
        clr_res,
        PyRep::keyval([
            ("containerID", PyRep::Integer(hangar_cont_id)),
            ("passwordSet", PyRep::Bool(false)),
        ])
    );

    // Item-bound empty args -> which = 0 -> InvalidPasswordType
    let err_zero = handle_set_password(
        &ctx,
        &station_sess,
        Some(&item_bound),
        "SetPassword",
        &[],
        &HashMap::new(),
    )
    .await
    .expect_err("empty args invalid slot");
    assert_eq!(err_zero.code, "InvalidPasswordType");

    // Missing or non-owned item -> ItemNotFound
    assert_eq!(
        handle_set_password(
            &ctx,
            &station_sess,
            None,
            "SetPassword",
            &[PyRep::Integer(999_999_999), PyRep::String("pw".into())],
            &HashMap::new(),
        )
        .await
        .expect_err("missing item")
        .code,
        "ItemNotFound"
    );
    assert_eq!(
        handle_set_password(
            &ctx,
            &station_sess,
            None,
            "SetPassword",
            &[PyRep::Integer(other_char_cont_id), PyRep::String("pw".into())],
            &HashMap::new(),
        )
        .await
        .expect_err("other char item")
        .code,
        "ItemNotFound"
    );

    // Corp container WITH ROLE_EQUIPMENT_CONFIG succeeds and records corp_container_logs
    station_sess.corp_role = eve_db::corp_roles::ROLE_EQUIPMENT_CONFIG;
    handle_set_password(
        &ctx,
        &station_sess,
        None,
        "SetPassword",
        &[PyRep::Integer(corp_cont_id), PyRep::Integer(2), PyRep::String("corp-cfg".into())],
        &HashMap::new(),
    )
    .await
    .expect("corp container with ROLE_EQUIPMENT_CONFIG succeeds");
    ctx.db.with_snapshot(|snap| {
        let logs = snap.corp_container_logs.get(&corp_id).expect("corp log written");
        assert!(logs.iter().any(|l| {
            l.container_id == corp_cont_id
                && l.action == "set_password"
                && l.password_type.as_deref() == Some("config")
        }));
    });

    // 2. In-space container password guards
    let mut space_sess = SessionState::new(503);
    space_sess.user_id = Some(1);
    space_sess.character_id = Some(char_id);
    space_sess.corporation_id = Some(corp_id);
    space_sess.solar_system_id = None;
    space_sess.ship_id = Some(ship_id);

    // solar_system_id == None: hangar container rejected, cargo container inside ship_id allowed
    assert_eq!(
        handle_set_password(
            &ctx,
            &space_sess,
            None,
            "SetPassword",
            &[PyRep::Integer(hangar_cont_id), PyRep::String("pw".into())],
            &HashMap::new(),
        )
        .await
        .expect_err("hangar not colocated when solar_system_id is None")
        .code,
        "ColocationRequired"
    );
    handle_set_password(
        &ctx,
        &space_sess,
        None,
        "SetPassword",
        &[PyRep::Integer(cargo_cont_id), PyRep::String("pw".into())],
        &HashMap::new(),
    )
    .await
    .expect("cargo container on ship works even if solar_system_id is None");

    space_sess.solar_system_id = Some(sys_id);
    // Hangar container when in space -> ColocationRequired (`else if session.ship_id != Some(item.location_id)`)
    assert_eq!(
        handle_set_password(
            &ctx,
            &space_sess,
            None,
            "SetPassword",
            &[PyRep::Integer(hangar_cont_id), PyRep::String("pw".into())],
            &HashMap::new(),
        )
        .await
        .expect_err("hangar rejected while in space")
        .code,
        "ColocationRequired"
    );

    // Ship not spawned / ship_id == None -> ShipNotInSpace
    space_sess.ship_id = None;
    assert_eq!(
        handle_set_password(
            &ctx,
            &space_sess,
            None,
            "SetPassword",
            &[PyRep::Integer(space_near_id), PyRep::String("pw".into())],
            &HashMap::new(),
        )
        .await
        .expect_err("no ship_id -> ShipNotInSpace")
        .code,
        "ShipNotInSpace"
    );
    space_sess.ship_id = Some(ship_id);
    assert_eq!(
        handle_set_password(
            &ctx,
            &space_sess,
            None,
            "SetPassword",
            &[PyRep::Integer(space_near_id), PyRep::String("pw".into())],
            &HashMap::new(),
        )
        .await
        .expect_err("unspawned ship -> ShipNotInSpace")
        .code,
        "ShipNotInSpace"
    );

    // Spawn ship + near/far containers in space
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let ship_ball = eve_destiny::Ball {
        id: ship_id,
        type_id: 621,
        position: glam::DVec3::new(10_000.0, 0.0, 0.0),
        velocity: glam::DVec3::ZERO,
        max_velocity: 230.0,
        inertia: 0.47,
        mass: 10_000_000.0,
        radius: 50.0,
        speed_fraction: 0.0,
        mode: eve_destiny::MovementMode::Stop,
    };
    let mut cs = eve_destiny::CombatState::default();
    cs.is_cloaked = true;
    let _ = sys_h
        .spawn_ball_with_meta(ship_ball.clone(), cs, eve_destiny::SlimItemMeta::default())
        .await;
    let _ = sys_h
        .spawn_ball(eve_destiny::Ball {
            id: space_near_id,
            type_id: 3465,
            position: glam::DVec3::new(11_000.0, 0.0, 0.0),
            velocity: glam::DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 100.0,
            radius: 10.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        })
        .await;
    let _ = sys_h
        .spawn_ball(eve_destiny::Ball {
            id: space_far_id,
            type_id: 3465,
            position: glam::DVec3::new(20_000.0, 0.0, 0.0),
            velocity: glam::DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 100.0,
            radius: 10.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        })
        .await;

    // Cloaked -> CannotDoWhileWarpingOrCloaked
    assert_eq!(
        handle_set_password(
            &ctx,
            &space_sess,
            None,
            "SetPassword",
            &[PyRep::Integer(space_near_id), PyRep::String("pw".into())],
            &HashMap::new(),
        )
        .await
        .expect_err("cloaked ship blocked")
        .code,
        "CannotDoWhileWarpingOrCloaked"
    );

    // Cloaked via ball_slim_meta only -> CannotDoWhileWarpingOrCloaked
    {
        let mut bp = sys_h.ballpark.write().unwrap();
        if let Some(cs) = bp.combat_states.get_mut(&ship_id) {
            cs.is_cloaked = false;
        }
        if let Some(meta) = bp.ball_slim_meta.get_mut(&ship_id) {
            meta.is_cloaked = true;
        }
    }
    assert_eq!(
        handle_set_password(
            &ctx,
            &space_sess,
            None,
            "SetPassword",
            &[PyRep::Integer(space_near_id), PyRep::String("pw".into())],
            &HashMap::new(),
        )
        .await
        .expect_err("slim meta cloaked ship blocked")
        .code,
        "CannotDoWhileWarpingOrCloaked"
    );

    // Uncloak but warp -> CannotDoWhileWarpingOrCloaked
    {
        let mut bp = sys_h.ballpark.write().unwrap();
        if let Some(meta) = bp.ball_slim_meta.get_mut(&ship_id) {
            meta.is_cloaked = false;
        }
        if let Some(b) = bp.balls.get_mut(&ship_id) {
            b.mode = eve_destiny::MovementMode::new_warp(
                ship_ball.position,
                glam::DVec3::new(500_000.0, 0.0, 0.0),
                3.0,
            );
        }
    }
    assert_eq!(
        handle_set_password(
            &ctx,
            &space_sess,
            None,
            "SetPassword",
            &[PyRep::Integer(space_near_id), PyRep::String("pw".into())],
            &HashMap::new(),
        )
        .await
        .expect_err("warping ship blocked")
        .code,
        "CannotDoWhileWarpingOrCloaked"
    );

    // Stop ship: unspawned container -> ContainerNotInSpace, far -> OutsideOperationalRange, near -> Ok
    {
        let mut bp = sys_h.ballpark.write().unwrap();
        if let Some(b) = bp.balls.get_mut(&ship_id) {
            b.mode = eve_destiny::MovementMode::Stop;
        }
    }
    assert_eq!(
        handle_set_password(
            &ctx,
            &space_sess,
            None,
            "SetPassword",
            &[PyRep::Integer(space_unspawned_id), PyRep::String("pw".into())],
            &HashMap::new(),
        )
        .await
        .expect_err("unspawned container rejected")
        .code,
        "ContainerNotInSpace"
    );
    assert_eq!(
        handle_set_password(
            &ctx,
            &space_sess,
            None,
            "SetPassword",
            &[PyRep::Integer(space_far_id), PyRep::String("pw".into())],
            &HashMap::new(),
        )
        .await
        .expect_err("far container rejected")
        .code,
        "OutsideOperationalRange"
    );
    handle_set_password(
        &ctx,
        &space_sess,
        None,
        "SetPassword",
        &[PyRep::Integer(space_near_id), PyRep::String("spacepw".into())],
        &HashMap::new(),
    )
    .await
    .expect("in-range space container succeeds");
}

#[tokio::test]
async fn handle_set_password_corp_ownership_zero_corp_and_role_bitmask_mutants() {
    let (ctx, char_id) = make_online_ctx();
    let station_id = 60_003_760_i64;
    let test_corp_id = 98_777_001_i64;
    let other_corp_id = 98_777_002_i64;
    let personal_cont_id = 888_901_i64;
    let zero_owner_cont_id = 888_902_i64;
    let corp_cont_id = 888_903_i64;
    let other_corp_cont_id = 888_904_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut base_corp = snap.corporations.values().next().unwrap().clone();
        base_corp.corporation_id = test_corp_id;
        base_corp.ceo_id = 90_000_999; // NOT char_id
        base_corp.member_roles.clear();
        snap.corporations.insert(test_corp_id, base_corp);

        snap.items.insert(personal_cont_id, make_item(personal_cont_id, 17366, char_id, station_id));
        snap.items.insert(zero_owner_cont_id, make_item(zero_owner_cont_id, 17366, 0, station_id));
        let mut c_own = make_item(corp_cont_id, 17366, test_corp_id, station_id);
        c_own.flag = InventoryFlag::CorpSAG1;
        snap.items.insert(corp_cont_id, c_own);
        let mut c_other = make_item(other_corp_cont_id, 17366, other_corp_id, station_id);
        c_other.flag = InventoryFlag::CorpSAG1;
        snap.items.insert(other_corp_cont_id, c_other);
    });

    let mut sess = SessionState::new(599);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.station_id = Some(station_id);
    sess.solar_system_id = Some(30_000_142);

    // 1. corporation_id == None (corp_id == 0) trying to access item with owner_id == 0 -> ItemNotFound
    //    (kills `corp_id == 0 || item.owner_id != corp_id` -> `&&`)
    sess.corporation_id = None;
    assert_eq!(
        handle_set_password(
            &ctx,
            &sess,
            None,
            "SetPassword",
            &[PyRep::Integer(zero_owner_cont_id), PyRep::String("pw".into())],
            &HashMap::new(),
        )
        .await
        .unwrap_err()
        .code,
        "ItemNotFound"
    );

    // 2. Non-zero corp_id accessing personal container (owner_id == char_id) without corp roles -> Ok
    //    (kills `if item.owner_id != char_id` -> `==`)
    sess.corporation_id = Some(test_corp_id);
    sess.corp_role = 0;
    let p_ok = handle_set_password(
        &ctx,
        &sess,
        None,
        "SetPassword",
        &[PyRep::Integer(personal_cont_id), PyRep::String("pw".into())],
        &HashMap::new(),
    )
    .await
    .expect("personal container bypasses corp check");
    assert_eq!(p_ok.dict_get("passwordSet"), Some(&PyRep::Bool(true)));

    // 3. Non-zero corp_id accessing different corp's container -> ItemNotFound
    //    (kills `corp_id == 0 || item.owner_id != corp_id` -> `!= 0` / `== corp_id`)
    assert_eq!(
        handle_set_password(
            &ctx,
            &sess,
            None,
            "SetPassword",
            &[PyRep::Integer(other_corp_cont_id), PyRep::String("pw".into())],
            &HashMap::new(),
        )
        .await
        .unwrap_err()
        .code,
        "ItemNotFound"
    );

    // 4. Corp container: DB corp_roles == 0, session.corp_role == ROLE_DIRECTOR -> Ok
    //    (kills `session.corp_role | corp_roles` -> `&` because `ROLE_DIRECTOR & 0 == 0`)
    sess.corp_role = eve_db::corp_roles::ROLE_DIRECTOR;
    handle_set_password(
        &ctx,
        &sess,
        None,
        "SetPassword",
        &[PyRep::Integer(corp_cont_id), PyRep::String("pw1".into())],
        &HashMap::new(),
    )
    .await
    .expect("session role alone grants access via `|`");

    // 5. Corp container: DB corp_roles == ROLE_DIRECTOR, session.corp_role == ROLE_DIRECTOR -> Ok
    //    (kills `session.corp_role | corp_roles` -> `^` because `ROLE_DIRECTOR ^ ROLE_DIRECTOR == 0`)
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations
            .get_mut(&test_corp_id)
            .unwrap()
            .member_roles
            .insert(char_id, eve_db::corp_roles::ROLE_DIRECTOR);
    });
    handle_set_password(
        &ctx,
        &sess,
        None,
        "SetPassword",
        &[
            PyRep::Integer(corp_cont_id),
            PyRep::String("pw2".into()),
            PyRep::String("pw1".into()),
        ],
        &HashMap::new(),
    )
    .await
    .expect("identical session+DB roles grant access via `|` (would cancel under `^`)");
}
