//! Comprehensive branch-coverage and mutation-killing tests for `fitting_dogma_im.rs`.

#![forbid(unsafe_code)]

use crate::router::{BoundObjectEntry, ServerContext};
use crate::services::combat_crimewatch::{with_mut_combat_runtime, DroneSpaceState};
use crate::services::inventory_fitting::fitting_dogma_im::handle_dogma_im;
use eve_db::{
    AccountRecord, InventoryFlag, InventoryItemRecord, WorldRepository,
};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::SdeRegistry;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

pub(super) fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| {
        let mut sde = SdeRegistry::with_canonical_seed();
        let mut pt = sde.types.values().min_by_key(|t| t.type_id).cloned().unwrap();
        pt.type_id = 581;
        pt.group_id = 82;
        pt.category_id = 7;
        pt.default_attributes.clear();
        pt.effect_ids = vec![13];
        sde.types.insert(581, pt);
        Arc::new(sde)
    }))
}

pub(super) fn make_session(
    char_id: i64,
    corp_id: i64,
    sys_id: Option<u32>,
    station_id: Option<i64>,
    ship_id: Option<i64>,
) -> SessionState {
    let mut s = SessionState::default();
    s.user_id = Some(char_id as u64);
    s.character_id = Some(char_id);
    s.corporation_id = Some(corp_id);
    s.solar_system_id = sys_id;
    s.station_id = station_id;
    s.ship_id = ship_id;
    s
}

pub(super) async fn call_dogma_im(
    ctx: &ServerContext,
    session: &mut SessionState,
    bound: Option<&BoundObjectEntry>,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let svc = ["dogmaIM"][0];
    handle_dogma_im(ctx, session, bound, svc, method, args, &HashMap::new()).await.unwrap()
}

pub(super) async fn call_ship_stance_mgr(
    ctx: &ServerContext,
    session: &mut SessionState,
    bound: Option<&BoundObjectEntry>,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let svc = ["shipStanceMgr"][0];
    handle_dogma_im(ctx, session, bound, svc, method, args, &HashMap::new()).await.unwrap()
}

pub(super) async fn call_char_fitting_mgr(
    ctx: &ServerContext,
    session: &mut SessionState,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let svc = ["charFittingMgr"][0];
    handle_dogma_im(ctx, session, None, svc, method, args, &HashMap::new()).await.unwrap()
}

pub(super) async fn call_invbroker(
    ctx: &ServerContext,
    session: &mut SessionState,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let svc = ["invbroker"][0];
    handle_dogma_im(ctx, session, None, svc, method, args, &HashMap::new()).await.unwrap()
}

#[tokio::test]
async fn item_get_info_space_drone_charges_ship_attributes_and_ship_stances() {
    let ctx = ServerContext::with_sde_and_repository(
        shared_sde(),
        Arc::new(WorldRepository::with_demo_seed()),
    );
    let char_id = 96_400_001i64;
    let sys_id = 30_000_142u32;
    let ship_id = 96_400_100i64; // Confessor (34317)
    let charge_id = 96_400_101i64;
    let offline_mod_id = 96_400_102i64;
    let drone_ball_id = 96_400_500i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.accounts.insert(
            "u_96401".into(),
            AccountRecord {
                user_id: char_id as u64,
                username: "u_96401".into(),
                password_token: String::new(),
                role: 0,
                banned_until_unix: None,
                ban_reason: None,
            },
        );
        let mut ch = snap.characters.values().next().cloned().unwrap();
        ch.character_id = char_id;
        ch.user_id = char_id as u64;
        ch.corporation_id = 1_000_009;
        ch.solar_system_id = sys_id;
        ch.station_id = Some(60_003_760);
        ch.active_ship_id = ship_id;
        snap.characters.insert(char_id, ch);

        for (iid, tid, loc, flg, sgl) in [
            (ship_id, 34317, 60_003_760, InventoryFlag::Hangar, true),
            (charge_id, 242, ship_id, InventoryFlag::HiSlot0, false),
            (offline_mod_id, 484, ship_id, InventoryFlag::HiSlot1, true),
        ] {
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: tid,
                    owner_id: char_id,
                    location_id: loc,
                    flag: flg,
                    quantity: 1,
                    singleton: sgl,
                    version: 1,
                    ..Default::default()
                },
            );
        }
        snap.item_custom_names.insert(ship_id, "NamedConfessor".into());
        snap.module_online_states.insert(offline_mod_id, false);
    });

    with_mut_combat_runtime(&ctx, |rt| {
        rt.active_drones_in_space.insert(
            drone_ball_id,
            DroneSpaceState {
                ball_id: drone_ball_id,
                drone_item_id: drone_ball_id,
                type_id: 2454,
                group_id: 100,
                owner_char_id: char_id,
                original_owner_char_id: char_id,
                parent_ship_id: ship_id,
                solar_system_id: sys_id,
                target_id: None,
                activity_state: 0,
                assist_char_id: None,
                guard_char_id: None,
                is_abandoned: false,
                is_sentry: false,
                bandwidth_used: 5.0,
                max_velocity: 300.0,
                optimal_range: 5_000.0,
                damage_vector: eve_destiny::DamageVector::default(),
                shield_repair_amount: 0.0,
                armor_repair_amount: 0.0,
                hull_repair_amount: 0.0,
                web_speed_factor: 0.0,
                neut_amount: 0.0,
                ecm_strength: 0.0,
                tp_sig_bonus_pct: 0.0,
            },
        );
    });

    let mut session =
        make_session(char_id, 1_000_009, Some(sys_id), Some(60_003_760), Some(ship_id));

    assert_eq!(
        call_dogma_im(&ctx, &mut session, None, "ItemGetInfo", &[]).await.unwrap_err().code,
        "InvalidArgument"
    );
    assert_eq!(
        call_dogma_im(&ctx, &mut session, None, "ItemGetInfo", &[PyRep::Integer(999_999_999)])
            .await
            .unwrap_err()
            .code,
        "TypeNotFound"
    );

    let drone_info =
        call_dogma_im(&ctx, &mut session, None, "ItemGetInfo", &[PyRep::Integer(drone_ball_id)])
            .await
            .unwrap();
    assert_eq!(drone_info.dict_get("typeID"), Some(&PyRep::Integer(2454)));
    assert_eq!(drone_info.dict_get("ownerID"), Some(&PyRep::Integer(char_id)));

    let charge_info =
        call_dogma_im(&ctx, &mut session, None, "GetDynamicItemInfo", &[PyRep::Integer(charge_id)])
            .await
            .unwrap();
    assert_eq!(charge_info.dict_get("categoryID"), Some(&PyRep::Integer(8)));

    let ship_info = call_invbroker(&ctx, &mut session, "GetItemInfo", &[PyRep::Integer(ship_id)])
        .await
        .unwrap();
    assert_eq!(ship_info.dict_get("customName"), Some(&PyRep::String("NamedConfessor".into())));

    // GetAllInfo with bound_entry and offline module
    let bound = BoundObjectEntry {
        bound_id: 1,
        session_id: 1,
        user_id: char_id as u64,
        character_id: char_id,
        service: String::new(),
        object_id: ship_id,
        oid_str: "N=1:1".into(),
        ref_id: 1,
    };
    let ship_attrs =
        call_dogma_im(&ctx, &mut session, Some(&bound), "GetAllInfo", &[]).await.unwrap();
    assert!(ship_attrs.dict_get("shipInfo").is_some());

    // shipStanceMgr.SetShipStance branches
    assert_eq!(
        call_ship_stance_mgr(&ctx, &mut session, None, "SetShipStance", &[])
            .await
            .unwrap_err()
            .code,
        "InvalidShipStance"
    );
    assert_eq!(
        call_ship_stance_mgr(
            &ctx,
            &mut session,
            None,
            "SetShipStance",
            &[PyRep::Integer(ship_id), PyRep::Integer(587)]
        )
        .await
        .unwrap_err()
        .code,
        "InvalidShipStance"
    );
    let st_ok = call_ship_stance_mgr(
        &ctx,
        &mut session,
        None,
        "SetShipStance",
        &[PyRep::Integer(ship_id), PyRep::Integer(34321)],
    )
    .await
    .unwrap();
    assert_eq!(st_ok, PyRep::Integer(34321));

    assert_eq!(
        call_ship_stance_mgr(&ctx, &mut session, None, "SetShipStance", &[PyRep::Integer(34323)])
            .await
            .unwrap_err()
            .code,
        "StanceSwitchCooldown"
    );
}

#[tokio::test]
async fn item_get_info_destroyed_drone_and_64bit_entity_ids_return_none_1032() {
    let ctx = ServerContext::with_sde_and_repository(
        shared_sde(),
        Arc::new(WorldRepository::with_demo_seed()),
    );
    let char_id = 96_400_901i64;
    let sys_id = 30_000_142u32;
    let ship_id = 96_400_910i64;
    let drone_id = 1_897_080_352i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            drone_id,
            InventoryItemRecord {
                item_id: drone_id,
                type_id: 2454,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    with_mut_combat_runtime(&ctx, |rt| {
        rt.active_drones_in_space.insert(
            drone_id,
            DroneSpaceState {
                ball_id: drone_id,
                drone_item_id: drone_id,
                type_id: 2454,
                group_id: 100,
                owner_char_id: char_id,
                original_owner_char_id: char_id,
                parent_ship_id: ship_id,
                solar_system_id: sys_id,
                target_id: None,
                activity_state: 1,
                assist_char_id: None,
                guard_char_id: None,
                is_abandoned: false,
                is_sentry: false,
                bandwidth_used: 5.0,
                max_velocity: 300.0,
                optimal_range: 5_000.0,
                damage_vector: eve_destiny::DamageVector::default(),
                shield_repair_amount: 0.0,
                armor_repair_amount: 0.0,
                hull_repair_amount: 0.0,
                web_speed_factor: 0.0,
                neut_amount: 0.0,
                ecm_strength: 0.0,
                tp_sig_bonus_pct: 0.0,
            },
        );
    });

    let mut session =
        make_session(char_id, 1_000_009, Some(sys_id), None, Some(ship_id));
    let before = call_dogma_im(&ctx, &mut session, None, "ItemGetInfo", &[PyRep::Integer(drone_id)])
        .await
        .expect("live drone ItemGetInfo succeeds");
    assert_eq!(before.dict_get("typeID"), Some(&PyRep::Integer(2454)));

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    assert!(crate::combat::death_pipeline::cleanup_destroyed_player_drone_sync(
        &ctx,
        &sys_handle,
        drone_id,
    ));

    let after = call_dogma_im(&ctx, &mut session, None, "ItemGetInfo", &[PyRep::Integer(drone_id)])
        .await
        .expect("destroyed drone ItemGetInfo must return None instead of TypeNotFound (#1032)");
    assert_eq!(after, PyRep::None);

    // 64-bit dynamic entity ID (> u32::MAX) must also return None instead of truncating to u32 typeID.
    let stale_64bit = i64::from(u32::MAX) + 1 + 587;
    let after_64 =
        call_dogma_im(&ctx, &mut session, None, "ItemGetInfo", &[PyRep::Integer(stale_64bit)])
            .await
            .expect("out-of-u32-range dynamic entity ID returns None");
    assert_eq!(after_64, PyRep::None);

    // Valid SDE typeID lookup and invalid synthetic typeID error remain intact.
    let valid_type = call_dogma_im(&ctx, &mut session, None, "ItemGetInfo", &[PyRep::Integer(587)])
        .await
        .expect("typeID lookup still succeeds");
    assert_eq!(valid_type.dict_get("typeID"), Some(&PyRep::Integer(587)));
    assert_eq!(
        call_dogma_im(&ctx, &mut session, None, "ItemGetInfo", &[PyRep::Integer(999_999_999)])
            .await
            .unwrap_err()
            .code,
        "TypeNotFound"
    );
    assert_eq!(
        call_dogma_im(&ctx, &mut session, None, "ItemGetInfo", &[PyRep::Integer(-1)])
            .await
            .unwrap_err()
            .code,
        "TypeNotFound"
    );
}

#[tokio::test]
async fn set_ship_stance_enforces_ownership_and_pushes_fit_projection_in_space() {
    use eve_destiny::{Ball, CombatState, MovementMode};
    use glam::DVec3;

    let ctx = new_seeded_ctx();
    let char_id = 92_100_099i64;
    let other_char_id = 92_100_100i64;
    let corp_id = 98_100_099i64;
    let sys_id: u32 = 30_000_142;
    let ship_id = 10_921_099_001i64;
    let other_ship_id = 10_921_099_002i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().expect("seed char");
        ch.character_id = char_id;
        ch.corporation_id = corp_id;
        ch.solar_system_id = sys_id;
        ch.station_id = None;
        ch.structure_id = None;
        ch.active_ship_id = ship_id;
        snap.characters.insert(char_id, ch.clone());

        ch.character_id = other_char_id;
        ch.active_ship_id = other_ship_id;
        snap.characters.insert(other_char_id, ch);

        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 34317, // Confessor
                owner_id: char_id,
                location_id: i64::from(sys_id),
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            other_ship_id,
            InventoryItemRecord {
                item_id: other_ship_id,
                type_id: 34317,
                owner_id: other_char_id,
                location_id: i64::from(sys_id),
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });

    let sys_h = ctx.systems.get_or_spawn(sys_id);
    sys_h
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 34317,
            position: DVec3::ZERO,
            velocity: DVec3::ZERO,
            max_velocity: 100.0,
            inertia: 3.0,
            mass: 1_500_000.0,
            radius: 35.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await
        .unwrap();
    let _ = sys_h.set_combat_state(ship_id, CombatState::default()).await;

    let mut no_ship_session = make_session(char_id, corp_id, Some(sys_id), None, None);
    assert_eq!(
        call_ship_stance_mgr(
            &ctx,
            &mut no_ship_session,
            None,
            "SetShipStance",
            &[PyRep::Integer(ship_id), PyRep::Integer(34321)],
        )
        .await
        .unwrap_err()
        .code,
        "NoActiveShip"
    );

    let mut session = make_session(char_id, corp_id, Some(sys_id), None, Some(ship_id));
    assert_eq!(
        call_ship_stance_mgr(
            &ctx,
            &mut session,
            None,
            "SetShipStance",
            &[PyRep::Integer(other_ship_id), PyRep::Integer(34321)],
        )
        .await
        .unwrap_err()
        .code,
        "AccessDenied"
    );
    assert!(sys_h.latest_fit_projection(other_ship_id).is_none());

    let res = call_ship_stance_mgr(
        &ctx,
        &mut session,
        None,
        "SetShipStance",
        &[PyRep::Integer(ship_id), PyRep::Integer(34321)],
    )
    .await
    .unwrap();
    assert_eq!(res, PyRep::Integer(34321));

    // Flush FIFO actor command queue so ApplyFitProjection executes.
    let _ = sys_h.get_ball_snapshot(ship_id).await.unwrap();
    let pushed = sys_h
        .latest_fit_projection(ship_id)
        .expect("SetShipStance must push FitProjection to in-space actor");
    let expected = ctx
        .build_fit_projection(char_id, ship_id, pushed.rev)
        .expect("projection rebuild");
    assert_eq!(pushed.baseline, expected.baseline);
}

#[tokio::test]
async fn set_ship_stance_rejects_bound_ship_and_invalid_ship_args_without_mutating_state() {
    let ctx = new_seeded_ctx();
    let char_a = 92_100_111i64;
    let char_b = 92_100_112i64;
    let corp_id = 98_100_111i64;
    let station_id = 60_003_760i64;
    let ship_a = 10_921_111_001i64;
    let ship_b = 10_921_111_002i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().expect("seed char");
        ch.character_id = char_a;
        ch.corporation_id = corp_id;
        ch.station_id = Some(station_id);
        ch.active_ship_id = ship_a;
        snap.characters.insert(char_a, ch.clone());

        ch.character_id = char_b;
        ch.active_ship_id = ship_b;
        snap.characters.insert(char_b, ch);

        for (sid, oid) in [(ship_a, char_a), (ship_b, char_b)] {
            snap.items.insert(
                sid,
                InventoryItemRecord {
                    item_id: sid,
                    type_id: 34317, // Confessor
                    owner_id: oid,
                    location_id: station_id,
                    flag: InventoryFlag::Hangar,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
        }
    });

    let mut sess_a = make_session(
        char_a,
        corp_id,
        Some(30_000_142),
        Some(station_id),
        Some(ship_a),
    );
    let mut sess_b = make_session(
        char_b,
        corp_id,
        Some(30_000_142),
        Some(station_id),
        Some(ship_b),
    );

    let switched = call_ship_stance_mgr(
        &ctx,
        &mut sess_a,
        None,
        "SetShipStance",
        &[PyRep::Integer(ship_a), PyRep::Integer(34321)],
    )
    .await
    .unwrap();
    assert_eq!(switched, PyRep::Integer(34321));

    // 1. 1-arg call bound to another character's ship must fail with AccessDenied.
    let bound_to_a = BoundObjectEntry {
        bound_id: 77,
        session_id: sess_b.session_id,
        user_id: sess_b.user_id.unwrap_or(42),
        character_id: char_b,
        service: "shipStanceMgr".into(),
        object_id: ship_a,
        oid_str: ship_a.to_string(),
        ref_id: 1,
    };
    let bound_err = call_ship_stance_mgr(
        &ctx,
        &mut sess_b,
        Some(&bound_to_a),
        "SetShipStance",
        &[PyRep::Integer(34323)],
    )
    .await
    .unwrap_err();
    assert_eq!(bound_err.code, "AccessDenied");

    // 2. Non-positive or non-integer ship_id in 2-arg SetShipStance must NOT fall back to active_ship_id.
    for bad_arg in [
        PyRep::Integer(0),
        PyRep::Integer(-1),
        PyRep::String("bad".into()),
    ] {
        let bad_err = call_ship_stance_mgr(
            &ctx,
            &mut sess_b,
            None,
            "SetShipStance",
            &[bad_arg, PyRep::Integer(34323)],
        )
        .await
        .unwrap_err();
        assert_eq!(bad_err.code, "AccessDenied");
    }

    // Ship A's stance and Ship B's unswitched state must both remain untouched.
    ctx.db.with_snapshot(|snap| {
        assert_eq!(
            snap.ship_fit_inputs
                .get(&ship_a)
                .and_then(|r| r.active_stance_type_id),
            Some(34321)
        );
        assert!(!snap.ship_fit_inputs.contains_key(&ship_b));
    });
}

fn new_seeded_ctx() -> ServerContext {
    ServerContext::with_sde_and_repository(
        shared_sde(),
        Arc::new(eve_db::WorldRepository::with_demo_seed()),
    )
    .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
}

#[tokio::test]
async fn item_get_info_triage_drone_charge_and_uncached_module_dyn_attrs() {
    use eve_db::{InventoryFlag, InventoryItemRecord};
    use eve_destiny::{Ball, CombatState, MovementMode};
    use glam::DVec3;
    use std::collections::HashMap;

    let ctx = new_seeded_ctx();
    let char_id = 92_100_001i64;
    let corp_id = 98_100_002i64;
    let sys_id: u32 = 30_000_142;
    let ship_id = 10_921_000_001i64;
    let triage_mod_id = 10_921_000_002i64;
    let uncached_mod_id = 10_921_000_003i64;
    let charge_id = 10_921_000_004i64;
    let drone_id = 10_921_000_005i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut c = snap.characters.values().next().cloned().expect("seed char");
        c.character_id = char_id;
        c.name = "ItemInfoPilot".to_string();
        c.corporation_id = corp_id;
        c.solar_system_id = sys_id;
        c.station_id = None;
        c.active_ship_id = ship_id;
        snap.characters.insert(char_id, c);

        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 37604, // Apostle
                owner_id: char_id,
                location_id: i64::from(sys_id),
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            triage_mod_id,
            InventoryItemRecord {
                item_id: triage_mod_id,
                type_id: 27951, // Triage Module I
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        let mut dyn_attrs = HashMap::new();
        dyn_attrs.insert(3, 5.0);
        dyn_attrs.insert(9, 10.0);
        dyn_attrs.insert(84, 25.0);
        dyn_attrs.insert(267, 1.0);
        dyn_attrs.insert(268, 2.0);
        dyn_attrs.insert(269, 3.0);
        dyn_attrs.insert(270, 4.0);
        snap.items.insert(
            uncached_mod_id,
            InventoryItemRecord {
                item_id: uncached_mod_id,
                type_id: 484,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot1,
                quantity: 1,
                singleton: true,
                dynamic_attributes: dyn_attrs,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            charge_id,
            InventoryItemRecord {
                item_id: charge_id,
                type_id: 185,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot1,
                quantity: 100,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            drone_id,
            InventoryItemRecord {
                item_id: drone_id,
                type_id: 2454,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::DroneBay,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );

        snap.module_online_states.insert(triage_mod_id, true);
        snap.module_online_states.insert(uncached_mod_id, false);
    });

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    sys_handle.try_spawn_ball_with_combat(
        Ball {
            id: ship_id,
            type_id: 37604,
            position: DVec3::ZERO,
            velocity: DVec3::ZERO,
            max_velocity: 100.0,
            inertia: 2.0,
            mass: 10_000_000.0,
            radius: 500.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
        CombatState {
            shield_hp: 1000.0,
            max_shield_hp: 1000.0,
            armor_hp: 1000.0,
            max_armor_hp: 1000.0,
            structure_hp: 1000.0,
            max_structure_hp: 1000.0,
            capacitor: 1000.0,
            max_capacitor: 1000.0,
            ..Default::default()
        },
    );
    let _ = sys_handle
        .add_module_cycle(crate::system_actor::ActiveModuleCycle {
            ship_id,
            module_id: triage_mod_id,
            module_type_id: 27951,
            owner_char_id: Some(char_id),
            effect_name: "triage".to_string(),
            duration_s: 300.0,
            repeat: 1,
            ..Default::default()
        })
        .await;

    let mut session = make_session(char_id, corp_id, Some(sys_id), None, Some(ship_id));
    let drone_info = call_dogma_im(&ctx, &mut session, None, "ItemGetInfo", &[PyRep::Integer(drone_id)])
        .await
        .expect("drone ItemGetInfo");
    assert!(!matches!(drone_info, PyRep::None));

    let charge_info = call_dogma_im(&ctx, &mut session, None, "ItemGetInfo", &[PyRep::Integer(charge_id)])
        .await
        .expect("charge ItemGetInfo");
    assert!(!matches!(charge_info, PyRep::None));

    let mod_info = call_dogma_im(&ctx, &mut session, None, "ItemGetInfo", &[PyRep::Integer(uncached_mod_id)])
        .await
        .expect("module ItemGetInfo");
    assert!(!matches!(mod_info, PyRep::None));
}

#[tokio::test]
async fn set_session_check_all_mismatches_unowned_ship_and_station_id() {
    use eve_db::{InventoryFlag, InventoryItemRecord};
    use std::collections::HashMap;

    let ctx = new_seeded_ctx();
    let char_id = 92_100_010i64;
    let other_id = 92_100_011i64;
    let corp_id = 98_100_010i64;
    let sys_id: u32 = 30_000_142;
    let station_id = 60_003_760i64;
    let ship_id = 10_921_000_010i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut c = snap.characters.values().next().cloned().expect("seed char");
        c.character_id = char_id;
        c.corporation_id = corp_id;
        c.solar_system_id = sys_id;
        c.station_id = Some(station_id);
        c.active_ship_id = ship_id;
        snap.characters.insert(char_id, c);

        // Owned by other_id so unowned ship check hits fittingValid=false
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 593,
                owner_id: other_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });

    let mut session = make_session(char_id, corp_id, Some(sys_id), Some(station_id), Some(ship_id));
    let kw = HashMap::new();

    for (k, v) in [
        ("charid", PyRep::Integer(char_id + 1)),
        ("shipid", PyRep::Integer(ship_id + 1)),
        ("solarsystemid", PyRep::Integer(i64::from(sys_id) + 1)),
        ("solarsystemid2", PyRep::Integer(i64::from(sys_id) + 2)),
        ("stationid", PyRep::Integer(station_id + 1)),
        ("corpid", PyRep::Integer(corp_id + 1)),
    ] {
        let check_dict = PyRep::Dict(vec![(PyRep::String(k.to_string()), v)]);
        let _ = handle_dogma_im(
            &ctx,
            &mut session,
            None,
            "dogmaIM",
            "SetSessionCheck",
            std::slice::from_ref(&check_dict),
            &kw,
        )
        .await;
    }

    let mut kw_check = HashMap::new();
    kw_check.insert("shipid".to_string(), PyRep::Integer(ship_id));
    kw_check.insert("stationid".to_string(), PyRep::Integer(station_id));
    let res = handle_dogma_im(
        &ctx,
        &mut session,
        None,
        "dogmaIM",
        "SetSessionCheck",
        &[],
        &kw_check,
    )
    .await
    .expect("handled")
    .expect("ok");
    assert!(!matches!(res, PyRep::None));

    let _ = call_dogma_im(&ctx, &mut session, None, "GetAllInfo", &[PyRep::Bool(true), PyRep::Bool(true)]).await;
    let _ = call_dogma_im(&ctx, &mut session, None, "GetShipAttributes", &[]).await;
    let _ = call_dogma_im(
        &ctx,
        &mut session,
        None,
        "GetLayerDamageValuesByItems",
        &[PyRep::List(vec![PyRep::Integer(ship_id)])],
    )
    .await;
    session.ship_id = None;
    let _ = call_dogma_im(&ctx, &mut session, None, "GetShipAttributes", &[]).await;
    let _ = call_dogma_im(
        &ctx,
        &mut session,
        None,
        "GetLayerDamageValuesByItems",
        &[PyRep::List(vec![PyRep::Integer(99_999_999)])],
    )
    .await;
    let _ = call_invbroker(&ctx, &mut session, "GetDynamicItemInfo", &[PyRep::Integer(ship_id)]).await;
}

#[tokio::test]
async fn destroy_fitting_online_offline_and_weapon_bank_error_paths() {
    use eve_db::{InventoryFlag, InventoryItemRecord};
    use eve_destiny::{Ball, CombatState, MovementMode};
    use glam::DVec3;

    let ctx = new_seeded_ctx();
    let char_id = 92_100_020i64;
    let other_id = 92_100_021i64;
    let corp_id = 98_100_020i64;
    let sys_id: u32 = 30_000_142;
    let ship_id = 10_921_000_020i64;
    let mod1_id = 10_921_000_021i64;
    let cargo_mod_id = 10_921_000_022i64;
    let struct_id = 10_921_000_023i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut c = snap.characters.values().next().cloned().expect("seed char");
        c.character_id = char_id;
        c.corporation_id = corp_id;
        c.solar_system_id = sys_id;
        c.station_id = None;
        c.active_ship_id = ship_id;
        snap.characters.insert(char_id, c);

        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 593,
                owner_id: char_id,
                location_id: i64::from(sys_id),
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            mod1_id,
            InventoryItemRecord {
                item_id: mod1_id,
                type_id: 484,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            cargo_mod_id,
            InventoryItemRecord {
                item_id: cargo_mod_id,
                type_id: 484,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            struct_id,
            InventoryItemRecord {
                item_id: struct_id,
                type_id: 35832,
                owner_id: other_id,
                location_id: i64::from(sys_id),
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    sys_handle.try_spawn_ball_with_combat(
        Ball {
            id: ship_id,
            type_id: 593,
            position: DVec3::ZERO,
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 2.0,
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
            capacitor: 500.0,
            max_capacitor: 500.0,
            ..Default::default()
        },
    );
    let _ = sys_handle
        .add_module_cycle(crate::system_actor::ActiveModuleCycle {
            ship_id,
            module_id: mod1_id,
            module_type_id: 484,
            owner_char_id: Some(char_id),
            effect_name: "projectileFired".to_string(),
            duration_s: 5.0,
            repeat: 1,
            ..Default::default()
        })
        .await;

    let mut session = make_session(char_id, corp_id, Some(sys_id), None, Some(ship_id));
    assert!(call_dogma_im(&ctx, &mut session, None, "DestroyFitting", &[]).await.is_err());
    assert!(call_dogma_im(&ctx, &mut session, None, "DestroyFitting", &[PyRep::Integer(99_888_777)]).await.is_err());
    assert!(call_dogma_im(&ctx, &mut session, None, "DestroyFitting", &[PyRep::Integer(cargo_mod_id)]).await.is_err());
    assert!(call_dogma_im(&ctx, &mut session, None, "DestroyFitting", &[PyRep::Integer(mod1_id)]).await.is_err());

    let _ = sys_handle.remove_module_cycle(ship_id, mod1_id).await;
    assert!(call_dogma_im(&ctx, &mut session, None, "SetModuleOnline", &[PyRep::Integer(99_888_777)]).await.is_err());
    assert!(call_dogma_im(&ctx, &mut session, None, "TakeModuleOffline", &[PyRep::Integer(99_888_777)]).await.is_err());
    // In space without depot -> DestroyFitting rejected, then spawn Mobile Depot (33474) nearby -> succeeds
    let _ = call_dogma_im(&ctx, &mut session, None, "DestroyFitting", &[PyRep::Integer(mod1_id)]).await;
    sys_handle.try_spawn_ball_with_combat(
        Ball {
            id: 10_921_000_099,
            type_id: 33474,
            position: DVec3::new(500.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 1000.0,
            radius: 100.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
        CombatState::default(),
    );
    let _ = call_dogma_im(&ctx, &mut session, None, "DestroyFitting", &[PyRep::Integer(mod1_id)]).await;
    // Also exercise docked DestroyFitting / nanite / structure role checks
    session.station_id = Some(60_003_760);
    let _ = call_dogma_im(&ctx, &mut session, None, "DestroyFitting", &[PyRep::Integer(mod1_id)]).await;

    for method in [
        "LinkWeapons",
        "PeelAndLink",
        "MergeModuleGroups",
        "DestroyWeaponBank",
        "UnlinkAllModules",
    ] {
        let _ = call_dogma_im(&ctx, &mut session, None, method, &[]).await;
        let _ = call_dogma_im(
            &ctx,
            &mut session,
            None,
            method,
            &[PyRep::Integer(ship_id), PyRep::Integer(mod1_id), PyRep::Integer(cargo_mod_id)],
        )
        .await;
    }
}

#[tokio::test]
async fn targeting_add_remove_clear_passive_targeter_and_probes_upwell() {
    use eve_db::{InventoryFlag, InventoryItemRecord};
    use eve_destiny::{Ball, CombatState, MovementMode, TargetLock};
    use glam::DVec3;

    let ctx = new_seeded_ctx();
    let char_id = 92_100_030i64;
    let target_owner = 92_100_031i64;
    let corp_id = 98_100_030i64;
    let sys_id: u32 = 30_000_142;
    let ship_id = 10_921_000_030i64;
    let passive_mod_id = 10_921_000_031i64;
    let target_ship_id = 10_921_000_032i64;
    let probe_id = 10_921_000_033i64;
    let citadel_id = 10_921_000_034i64;
    let cloaked_id = 10_921_000_035i64;
    let far_id = 10_921_000_036i64;
    let invuln_id = 10_921_000_037i64;
    let stale_id = 10_921_000_038i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut c = snap.characters.values().next().cloned().expect("seed char");
        c.character_id = char_id;
        c.corporation_id = corp_id;
        c.solar_system_id = sys_id;
        c.station_id = None;
        c.active_ship_id = ship_id;
        snap.characters.insert(char_id, c);

        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 593,
                owner_id: char_id,
                location_id: i64::from(sys_id),
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            passive_mod_id,
            InventoryItemRecord {
                item_id: passive_mod_id,
                type_id: 581, // Passive Targeter I (group 82)
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::MedSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(passive_mod_id, true);
    });

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let spawn_with = |id: i64, type_id: u32, x: f64, radius: f64, cs: CombatState| {
        sys_handle.try_spawn_ball_with_combat(
            Ball {
                id,
                type_id,
                position: DVec3::new(x, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 2.0,
                mass: 1_000_000.0,
                radius,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            cs,
        );
    };
    let base_cs = CombatState {
        shield_hp: 500.0,
        max_shield_hp: 500.0,
        armor_hp: 500.0,
        max_armor_hp: 500.0,
        structure_hp: 500.0,
        max_structure_hp: 500.0,
        capacitor: 500.0,
        max_capacitor: 500.0,
        ..Default::default()
    };

    spawn_with(ship_id, 593, 0.0, 40.0, base_cs.clone());
    let _ = sys_handle
        .add_target_lock(
            ship_id,
            TargetLock {
                target_id: stale_id,
                lock_time_ms: 1000.0,
                remaining_lock_ms: 500.0,
                locked: false,
                max_range_m: None,
            },
        )
        .await;
    let _ = sys_handle
        .add_module_cycle(crate::system_actor::ActiveModuleCycle {
            ship_id,
            module_id: passive_mod_id,
            module_type_id: 581,
            owner_char_id: Some(char_id),
            effect_name: "passiveTargeting".to_string(),
            duration_s: 30.0,
            repeat: 1,
            ..Default::default()
        })
        .await;

    spawn_with(target_ship_id, 593, 1000.0, 40.0, base_cs.clone());
    spawn_with(probe_id, 22778, 500.0, 10.0, base_cs.clone());
    spawn_with(citadel_id, 35832, 2000.0, 5000.0, base_cs.clone());

    let mut cloaked_cs = base_cs.clone();
    cloaked_cs.is_cloaked = true;
    spawn_with(cloaked_id, 593, 1000.0, 40.0, cloaked_cs);

    spawn_with(far_id, 593, 500_000.0, 40.0, base_cs.clone());

    let mut invuln_cs = base_cs.clone();
    invuln_cs.gate_cloak_remaining_ticks = 20;
    invuln_cs.undock_invulnerable_remaining_ticks = 20;
    spawn_with(invuln_id, 593, 1000.0, 40.0, invuln_cs);

    let mut session = make_session(char_id, corp_id, Some(sys_id), None, Some(ship_id));
    let _ = call_dogma_im(&ctx, &mut session, None, "AddTarget", &[PyRep::Integer(ship_id)]).await;
    assert!(call_dogma_im(&ctx, &mut session, None, "AddTarget", &[PyRep::Integer(88_888_888)]).await.is_err());
    assert!(call_dogma_im(&ctx, &mut session, None, "AddTarget", &[PyRep::Integer(probe_id)]).await.is_err());
    assert!(call_dogma_im(&ctx, &mut session, None, "AddTarget", &[PyRep::Integer(cloaked_id)]).await.is_err());
    assert!(call_dogma_im(&ctx, &mut session, None, "AddTarget", &[PyRep::Integer(far_id)]).await.is_err());
    let _ = call_dogma_im(&ctx, &mut session, None, "AddTarget", &[PyRep::Integer(invuln_id)]).await;

    let _ = target_owner;
    let add_ok = call_dogma_im(&ctx, &mut session, None, "AddTarget", &[PyRep::Integer(target_ship_id)])
        .await
        .expect("AddTarget ok");
    assert!(!matches!(add_ok, PyRep::None));

    let _ = call_dogma_im(&ctx, &mut session, None, "AddTarget", &[PyRep::Integer(target_ship_id)]).await;
    let _ = call_dogma_im(&ctx, &mut session, None, "AddTarget", &[PyRep::Integer(citadel_id)]).await;

    // Jammed shooter check
    let _ = sys_handle.apply_ecm_jam(ship_id, 10.0).await;
    let _ = call_dogma_im(&ctx, &mut session, None, "AddTarget", &[PyRep::Integer(citadel_id)]).await;

    let _ = call_dogma_im(&ctx, &mut session, None, "GetTargets", &[]).await;
    let _ = call_dogma_im(&ctx, &mut session, None, "GetTargeters", &[]).await;
    let _ = call_dogma_im(&ctx, &mut session, None, "RemoveTarget", &[PyRep::Integer(target_ship_id)]).await;
    let _ = call_dogma_im(&ctx, &mut session, None, "RemoveTarget", &[PyRep::Integer(99_999_111)]).await;
    let _ = call_dogma_im(&ctx, &mut session, None, "ClearTargets", &[]).await;

    let mut docked_sess = make_session(char_id, corp_id, None, Some(60_003_760), Some(ship_id));
    let _ = call_dogma_im(&ctx, &mut docked_sess, None, "GetTargets", &[]).await;
    let _ = call_dogma_im(&ctx, &mut docked_sess, None, "GetTargeters", &[]).await;
    let _ = call_dogma_im(&ctx, &mut docked_sess, None, "ClearTargets", &[]).await;
    assert!(call_dogma_im(&ctx, &mut docked_sess, None, "AddTarget", &[PyRep::Integer(target_ship_id)]).await.is_err());
}

#[tokio::test]
async fn destroy_fitting_t3_struct_roles_and_target_lock_guards() {
    let sde = shared_sde();
    let ship_type = sde
        .types
        .values()
        .filter(|t| t.category_id == 6 && t.group_id == 25 && t.capacity > 0.0)
        .min_by_key(|t| t.type_id)
        .map(|t| t.type_id)
        .unwrap_or(593);
    let turret_type = sde
        .types
        .values()
        .filter(|t| t.category_id == 7 && t.group_id == 55)
        .min_by_key(|t| t.type_id)
        .map(|t| t.type_id)
        .unwrap_or(581);

    let ctx = ServerContext::with_sde_and_repository(
        Arc::clone(&sde),
        Arc::new(WorldRepository::with_demo_seed()),
    )
    .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED);

    let (base_char, sys_id) = ctx.db.with_snapshot(|snap| {
        let ch = snap.characters.values().min_by_key(|c| c.character_id).cloned().expect("demo char");
        (ch, 30_000_142u32)
    });
    let char_id = 91_800_001i64;
    let other_char_id = 91_800_002i64;
    let corp_id = 98_800_001i64;
    let other_corp_id = 98_800_002i64;
    let ship_id = 1_918_000_010i64;
    let other_ship_id = 1_918_000_011i64;
    let hi0_id = 1_918_000_020i64;
    let hi1_id = 1_918_000_021i64;
    let hi2_id = 1_918_000_022i64;
    let hi3_id = 1_918_000_023i64;
    let sub0_id = 1_918_000_024i64;
    let cargo_mod_id = 1_918_000_025i64;
    let active_mod_id = 1_918_000_026i64;
    let struct_id = 1_918_000_500i64;
    let struct_mod_id = 1_918_000_501i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut c1 = base_char.clone();
        c1.character_id = char_id;
        c1.corporation_id = corp_id;
        c1.solar_system_id = sys_id;
        c1.station_id = None;
        c1.structure_id = None;
        c1.active_ship_id = ship_id;
        snap.characters.insert(char_id, c1);

        let mut c2 = base_char.clone();
        c2.character_id = other_char_id;
        c2.corporation_id = other_corp_id;
        c2.solar_system_id = sys_id;
        c2.station_id = None;
        c2.structure_id = None;
        c2.active_ship_id = other_ship_id;
        snap.characters.insert(other_char_id, c2);

        let mut corp1 = eve_db::CorporationRecord::default();
        corp1.corporation_id = corp_id;
        corp1.ceo_id = 99_999_999; // non-CEO, no director role
        snap.corporations.insert(corp_id, corp1);

        let mut st = eve_db::UpwellStructureRecord {
            structure_id: struct_id,
            type_id: 35832,
            owner_corp_id: other_corp_id,
            solar_system_id: sys_id,
            name: "Dogma Target Citadel".into(),
            position: [1_000.0, 0.0, 0.0],
            state: "ShieldVulnerable".into(),
            quantum_core_installed: true,
            fuel_blocks: 100,
            shield_hp: 100_000.0,
            armor_hp: 100_000.0,
            hull_hp: 100_000.0,
            reinforced_until_unix: 0,
            last_damage_tick: 0,
            last_damage_unix: 0,
            self_destruct_at_unix: None,
        };
        snap.structures.insert(struct_id, st.clone());

        let mk = |item_id, type_id, owner_id, location_id, flag| InventoryItemRecord {
            item_id,
            type_id,
            owner_id,
            location_id,
            flag,
            quantity: 1,
            singleton: true,
            dynamic_attributes: HashMap::new(),
            version: 1,
            ..Default::default()
        };
        snap.items.insert(ship_id, mk(ship_id, ship_type, char_id, sys_id as i64, InventoryFlag::None));
        snap.items.insert(other_ship_id, mk(other_ship_id, ship_type, other_char_id, sys_id as i64, InventoryFlag::None));
        snap.items.insert(hi0_id, mk(hi0_id, turret_type, char_id, ship_id, InventoryFlag::HiSlot0));
        snap.items.insert(hi1_id, mk(hi1_id, turret_type, char_id, ship_id, InventoryFlag::HiSlot1));
        snap.items.insert(hi2_id, mk(hi2_id, turret_type, char_id, ship_id, InventoryFlag::HiSlot2));
        snap.items.insert(hi3_id, mk(hi3_id, turret_type, char_id, ship_id, InventoryFlag::HiSlot3));
        snap.items.insert(sub0_id, mk(sub0_id, turret_type, char_id, ship_id, InventoryFlag::SubSystemSlot0));
        snap.items.insert(cargo_mod_id, mk(cargo_mod_id, turret_type, char_id, ship_id, InventoryFlag::Cargo));
        snap.items.insert(active_mod_id, mk(active_mod_id, turret_type, char_id, ship_id, InventoryFlag::HiSlot4));
        snap.module_active_states.insert(active_mod_id);
        snap.items.insert(struct_mod_id, mk(struct_mod_id, turret_type, other_corp_id, struct_id, InventoryFlag::HiSlot0));
        st.owner_corp_id = other_corp_id;
        let _ = st;
    });

    let sys = ctx.systems.get_or_spawn(sys_id);
    let mk_ball = |id, type_id, x| eve_destiny::Ball {
        id,
        type_id,
        position: glam::DVec3::new(x, 0.0, 0.0),
        velocity: glam::DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 2.0,
        mass: 1_000_000.0,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: eve_destiny::MovementMode::Stop,
    };
    let _ = sys.spawn_ball(mk_ball(ship_id, ship_type, 0.0)).await;
    let _ = sys.spawn_ball(mk_ball(other_ship_id, ship_type, 1_500.0)).await;
    let _ = sys.spawn_ball(mk_ball(struct_id, 35832, 2_000.0)).await;

    let mut sess = make_session(char_id, corp_id, Some(sys_id), None, Some(ship_id));

    // DestroyFitting: CannotUnfitInSpace first -> then dock in Upwell structure -> ownership violation -> corp roles -> cargo -> active -> T3 subsystem -> nonexistent
    assert_eq!(
        call_dogma_im(&ctx, &mut sess, None, "DestroyFitting", &[PyRep::Integer(struct_mod_id)])
            .await
            .unwrap_err()
            .code,
        "CannotUnfitInSpace"
    );
    sess.structure_id = Some(struct_id);
    assert_eq!(
        call_dogma_im(&ctx, &mut sess, None, "DestroyFitting", &[PyRep::Integer(struct_mod_id)])
            .await
            .unwrap_err()
            .code,
        "StructureOwnershipViolation"
    );
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(st) = snap.structures.get_mut(&struct_id) {
            st.owner_corp_id = corp_id;
        }
        if let Some(it) = snap.items.get_mut(&struct_mod_id) {
            it.owner_id = corp_id;
        }
    });
    assert_eq!(
        call_dogma_im(&ctx, &mut sess, None, "DestroyFitting", &[PyRep::Integer(struct_mod_id)])
            .await
            .unwrap_err()
            .code,
        "InsufficientCorpRoles"
    );
    assert_eq!(
        call_dogma_im(&ctx, &mut sess, None, "DestroyFitting", &[PyRep::Integer(active_mod_id)])
            .await
            .unwrap_err()
            .code,
        "ModuleIsActive"
    );
    assert_eq!(
        call_dogma_im(&ctx, &mut sess, None, "DestroyFitting", &[PyRep::Integer(sub0_id)])
            .await
            .unwrap_err()
            .code,
        "SubsystemHasFittedModules"
    );
    assert!(call_dogma_im(&ctx, &mut sess, None, "DestroyFitting", &[PyRep::Integer(99_999_998)]).await.is_ok());

    // DestroyFitting on hi3 with active ewar profile triggers tp_sig_multiplier update
    ctx.active_ewar_starts
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .insert((ship_id, hi3_id), vec![(other_ship_id, "targetPainter")]);
    assert!(call_dogma_im(&ctx, &mut sess, None, "DestroyFitting", &[PyRep::Integer(hi3_id)]).await.is_ok());

    // Undocked near a Mobile Depot: CannotRefitWithWeaponsTimer -> ItemNotOwnedByCharacter -> ItemNotFittedToActiveShip
    sess.structure_id = None;
    {
        let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
        cw.states.entry(char_id).or_default().weapon_timer_until_unix = ctx.now_unix().saturating_add(60);
    }
    assert_eq!(
        call_dogma_im(&ctx, &mut sess, None, "DestroyFitting", &[PyRep::Integer(cargo_mod_id)])
            .await
            .unwrap_err()
            .code,
        "CannotRefitWithWeaponsTimer"
    );
    {
        let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
        cw.states.entry(char_id).or_default().weapon_timer_until_unix = 0;
    }
    let depot_type = ctx
        .sde
        .types
        .values()
        .filter(|t| t.group_id == 1246)
        .min_by_key(|t| t.type_id)
        .map(|t| t.type_id)
        .unwrap_or(33_474);
    let depot_id = 1_918_000_600i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            depot_id,
            InventoryItemRecord {
                item_id: depot_id,
                type_id: depot_type,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });
    let _ = sys.spawn_ball(mk_ball(depot_id, depot_type, 200.0)).await;
    assert_eq!(
        call_dogma_im(&ctx, &mut sess, None, "DestroyFitting", &[PyRep::Integer(other_ship_id)])
            .await
            .unwrap_err()
            .code,
        "ItemNotOwnedByCharacter"
    );
    assert_eq!(
        call_dogma_im(&ctx, &mut sess, None, "DestroyFitting", &[PyRep::Integer(cargo_mod_id)])
            .await
            .unwrap_err()
            .code,
        "ItemNotFittedToActiveShip"
    );

    // Weapon grouping & unlinking guards and happy paths
    assert_eq!(
        call_dogma_im(&ctx, &mut sess, None, "LinkWeapons", &[PyRep::Integer(other_ship_id), PyRep::Integer(hi0_id), PyRep::Integer(hi1_id)])
            .await
            .unwrap_err()
            .code,
        "ShipOwnershipViolation"
    );
    assert_eq!(
        call_dogma_im(&ctx, &mut sess, None, "LinkWeapons", &[PyRep::Integer(ship_id), PyRep::Integer(hi0_id), PyRep::Integer(999_888)])
            .await
            .unwrap_err()
            .code,
        "InvalidWeaponGroup"
    );
    assert_eq!(
        call_dogma_im(&ctx, &mut sess, None, "LinkWeapons", &[PyRep::Integer(ship_id), PyRep::Integer(hi0_id), PyRep::Integer(cargo_mod_id)])
            .await
            .unwrap_err()
            .code,
        "InvalidWeaponGroup"
    );
    assert!(call_dogma_im(&ctx, &mut sess, None, "LinkWeapons", &[PyRep::Integer(ship_id), PyRep::Integer(hi0_id), PyRep::Integer(hi1_id)]).await.is_ok());
    assert!(call_dogma_im(&ctx, &mut sess, None, "PeelAndLink", &[PyRep::Integer(ship_id), PyRep::Integer(hi2_id), PyRep::Integer(hi1_id)]).await.is_ok());
    assert!(call_dogma_im(&ctx, &mut sess, None, "MergeModuleGroups", &[PyRep::Integer(ship_id), PyRep::Integer(hi0_id), PyRep::Integer(hi2_id)]).await.is_ok());
    assert!(call_dogma_im(&ctx, &mut sess, None, "UnlinkModule", &[PyRep::Integer(ship_id), PyRep::Integer(hi1_id)]).await.is_ok());
    assert!(call_dogma_im(&ctx, &mut sess, None, "DestroyWeaponBank", &[PyRep::Integer(ship_id), PyRep::Integer(hi0_id)]).await.is_ok());
    assert!(call_dogma_im(&ctx, &mut sess, None, "LinkAllWeapons", &[PyRep::Integer(ship_id)]).await.is_ok());
    assert!(call_dogma_im(&ctx, &mut sess, None, "UnlinkAllModules", &[PyRep::Integer(ship_id)]).await.is_ok());
    assert_eq!(
        call_dogma_im(&ctx, &mut sess, None, "UnlinkAllModules", &[PyRep::Integer(other_ship_id)])
            .await
            .unwrap_err()
            .code,
        "ShipOwnershipViolation"
    );

    // GetLayerDamageValuesByItems variants (Tuple, Integer, empty fallback)
    assert!(call_dogma_im(&ctx, &mut sess, None, "GetLayerDamageValuesByItems", &[PyRep::Tuple(vec![PyRep::Integer(ship_id)])]).await.is_ok());
    assert!(call_dogma_im(&ctx, &mut sess, None, "GetLayerDamageValuesByItems", &[PyRep::Integer(ship_id)]).await.is_ok());
    let mut no_ship_sess = make_session(char_id, corp_id, Some(sys_id), None, None);
    assert!(call_dogma_im(&ctx, &mut no_ship_sess, None, "GetLayerDamageValuesByItems", &[]).await.is_ok());

    // AddTarget guards: leave structure back into space, then tethered -> invuln -> jammed -> cloaked -> range -> lock
    sess.structure_id = None;
    let _ = sys.try_update_combat_state(other_ship_id, |cs| cs.is_tethered = true);
    assert_eq!(
        call_dogma_im(&ctx, &mut sess, None, "AddTarget", &[PyRep::Integer(other_ship_id)])
            .await
            .unwrap_err()
            .code,
        "TargetInvulnerable"
    );
    let _ = sys.try_update_combat_state(other_ship_id, |cs| cs.is_tethered = false);

    crate::services::space_destiny::grant_location_change_invulnerability(&ctx, other_char_id, ctx.now_unix().saturating_add(30));
    assert_eq!(
        call_dogma_im(&ctx, &mut sess, None, "AddTarget", &[PyRep::Integer(other_ship_id)])
            .await
            .unwrap_err()
            .code,
        "TargetInvulnerable"
    );
    crate::services::space_destiny::break_location_change_invulnerability(&ctx, other_char_id);

    let _ = sys.apply_ecm_jam(ship_id, 5.0).await;
    assert_eq!(
        call_dogma_im(&ctx, &mut sess, None, "AddTarget", &[PyRep::Integer(other_ship_id)])
            .await
            .unwrap_err()
            .code,
        "ShipJammed"
    );
    let _ = sys.try_update_combat_state(ship_id, |cs| cs.ecm_jammed_remaining_s = 0.0);

    let _ = sys.try_update_combat_state(ship_id, |cs| cs.is_cloaked = true);
    assert_eq!(
        call_dogma_im(&ctx, &mut sess, None, "AddTarget", &[PyRep::Integer(other_ship_id)])
            .await
            .unwrap_err()
            .code,
        "TargetingFailed"
    );
    let _ = sys.try_update_combat_state(ship_id, |cs| cs.is_cloaked = false);

    let _ = sys.set_ball_position(other_ship_id, glam::DVec3::new(9_999_999.0, 0.0, 0.0)).await;
    assert_eq!(
        call_dogma_im(&ctx, &mut sess, None, "AddTarget", &[PyRep::Integer(other_ship_id)])
            .await
            .unwrap_err()
            .code,
        "TargetingFailed"
    );
    let _ = sys.set_ball_position(other_ship_id, glam::DVec3::new(1_500.0, 0.0, 0.0)).await;

    // Lock Upwell structure & player ship, then unlock & clear
    assert!(call_dogma_im(&ctx, &mut sess, None, "AddTarget", &[PyRep::Integer(struct_id)]).await.is_ok());
    assert!(call_dogma_im(&ctx, &mut sess, None, "AddTarget", &[PyRep::Integer(other_ship_id)]).await.is_ok());
    assert!(call_dogma_im(&ctx, &mut sess, None, "RemoveTarget", &[PyRep::Integer(other_ship_id)]).await.is_ok());
    let _ = sys
        .add_target_lock(
            ship_id,
            eve_destiny::TargetLock {
                target_id: other_ship_id,
                lock_time_ms: 100.0,
                remaining_lock_ms: 0.0,
                locked: true,
                max_range_m: Some(50_000.0),
            },
        )
        .await;
    assert!(call_dogma_im(&ctx, &mut sess, None, "ClearTargets", &[]).await.is_ok());
}

#[tokio::test]
async fn mutants_cov_item_get_info_triage_targeting_passive_and_weapon_banks() {
    use eve_db::{InventoryFlag, InventoryItemRecord, TrainedSkillRecord};
    use eve_destiny::{Ball, CombatState, MovementMode, TargetLock};
    use eve_dogma::{DogmaItemState, ProjectedEwarEffect};
    use eve_sde::attr_ids;
    use glam::DVec3;

    let mut sde = (*shared_sde()).clone();
    // Custom frigate hull with MAX_LOCKED_TARGETS = 1 and long targeting range for deterministic boundary tests.
    let mut base_ship = sde.types.get(&587).expect("Rifter").clone();
    base_ship.type_id = 999_601;
    base_ship.default_attributes.insert(attr_ids::MAX_LOCKED_TARGETS, 1.0);
    base_ship.default_attributes.insert(76, 500_000.0); // maxTargetRange
    base_ship.default_attributes.insert(564, 500.0); // scanResolution
    base_ship.default_attributes.insert(attr_ids::CPU_OUTPUT, 1_000.0);
    base_ship.default_attributes.insert(attr_ids::POWER_OUTPUT, 1_000.0);
    base_ship.default_attributes.insert(attr_ids::HI_SLOTS, 4.0);
    base_ship.default_attributes.insert(attr_ids::MED_SLOTS, 4.0);
    sde.types.insert(999_601, base_ship);

    // Custom frigate hull with high MAX_LOCKED_TARGETS = 10 to test character skill_char_max bottleneck.
    let mut high_lock_ship = sde.types.get(&999_601).unwrap().clone();
    high_lock_ship.type_id = 999_602;
    high_lock_ship.default_attributes.insert(attr_ids::MAX_LOCKED_TARGETS, 10.0);
    sde.types.insert(999_602, high_lock_ship);

    // Clean Triage I (27951) and Triage II (4294) without auto-applied ship modifiers so unapplied_triage_ship_bonus (+4/+5) fires.
    let mut mod_tmpl = sde.types.get(&484).expect("turret seed").clone();
    mod_tmpl.default_attributes.clear();
    mod_tmpl.default_attributes.insert(attr_ids::CPU, 1.0);
    mod_tmpl.default_attributes.insert(attr_ids::POWER, 1.0);
    mod_tmpl.effect_ids = vec![12]; // hiPower slot effect only; no auto triage ship modifiers
    for tid in [27951u32, 4294u32] {
        let mut t = mod_tmpl.clone();
        t.type_id = tid;
        t.group_id = 772;
        sde.types.insert(tid, t);
    }

    // Custom Target Painter (group 379, attr 554 = 30.0%) and Passive Targeter (581 already in shared_sde, low CPU/PG).
    if let Some(pt) = sde.types.get_mut(&581) {
        pt.default_attributes.insert(attr_ids::CPU, 1.0);
        pt.default_attributes.insert(attr_ids::POWER, 1.0);
    }
    let mut tp_t = mod_tmpl.clone();
    tp_t.type_id = 999_603;
    tp_t.group_id = 379;
    tp_t.default_attributes.insert(554, 30.0);
    tp_t.default_attributes.insert(attr_ids::MAX_RANGE, 100_000.0);
    sde.types.insert(999_603, tp_t);

    let ctx = ServerContext::with_sde_and_repository(
        Arc::new(sde),
        Arc::new(WorldRepository::with_demo_seed()),
    )
    .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED);

    let char_id = 96_500_001i64;
    let target_char_id = 96_500_002i64;
    let other_char_id = 96_500_003i64;
    let corp_id = 98_500_001i64;
    let sys_id = 30_000_142u32;
    let ship_id = 96_501_001i64;
    let skill_ship_id = 96_501_002i64;
    let triage_id = 96_501_010i64;
    let passive_id = 96_501_011i64;
    let tp1_id = 96_501_012i64;
    let tp2_id = 96_501_013i64;
    let gun1_id = 96_501_014i64;
    let gun2_id = 96_501_015i64;
    let depot_id = 96_501_090i64;
    let tp_target_id = 96_502_001i64;
    let tgt_base = 96_502_100i64;

    ctx.db.with_mut_snapshot(|snap| {
        let base_c = snap.characters.values().next().cloned().unwrap();
        for (cid, sid) in [(char_id, ship_id), (target_char_id, tp_target_id), (other_char_id, tgt_base)] {
            snap.accounts.insert(
                format!("u_{cid}"),
                eve_db::AccountRecord {
                    user_id: cid as u64,
                    username: format!("u_{cid}"),
                    password_token: String::new(),
                    role: 0,
                    banned_until_unix: None,
                    ban_reason: None,
                },
            );
            let mut ch = base_c.clone();
            ch.character_id = cid;
            ch.user_id = cid as u64;
            ch.corporation_id = corp_id;
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = sid;
            snap.characters.insert(cid, ch);
        }
        // Train Target Management (3429) & Advanced Target Management (3430) to 5 so ship_max limits char_id.
        for sid_skill in [3429u32, 3430u32] {
            snap.skills.entry(char_id).or_default().insert(
                sid_skill,
                TrainedSkillRecord { type_id: sid_skill, level: 5, skill_points: 256_000.0 },
            );
        }
        // target_char_id has lvl 1 Target Management, lvl 0 Adv Target Management -> skill_char_max = 2 + 1 + 0 = 3.
        snap.skills.entry(target_char_id).or_default().insert(
            3429,
            TrainedSkillRecord { type_id: 3429, level: 1, skill_points: 1_000.0 },
        );

        let mut ship_rec = InventoryItemRecord {
            item_id: ship_id,
            type_id: 999_601,
            owner_id: char_id,
            location_id: i64::from(sys_id),
            flag: InventoryFlag::None,
            quantity: 1,
            singleton: true,
            dynamic_attributes: HashMap::from([(18u16, 42.5), (265u16, 777.0), (266u16, 333.0)]),
            version: 1,
            ..Default::default()
        };
        snap.items.insert(ship_id, ship_rec.clone());
        ship_rec.item_id = skill_ship_id;
        ship_rec.type_id = 999_602;
        ship_rec.owner_id = target_char_id;
        ship_rec.dynamic_attributes.clear();
        snap.items.insert(skill_ship_id, ship_rec);

        for (iid, tid, owner, loc, flag) in [
            (triage_id, 27951u32, char_id, ship_id, InventoryFlag::HiSlot0),
            (gun1_id, 484, char_id, ship_id, InventoryFlag::HiSlot1),
            (gun2_id, 484, char_id, ship_id, InventoryFlag::HiSlot2),
            (passive_id, 581, char_id, ship_id, InventoryFlag::MedSlot0),
            (tp1_id, 999_603, char_id, ship_id, InventoryFlag::MedSlot1),
            (tp2_id, 999_603, char_id, ship_id, InventoryFlag::MedSlot2),
            (tp_target_id, 999_601, target_char_id, i64::from(sys_id), InventoryFlag::None),
            (depot_id, ctx.sde.types.values().filter(|t| t.group_id == 1246).min_by_key(|t| t.type_id).map(|t| t.type_id).unwrap_or(33_474), char_id, i64::from(sys_id), InventoryFlag::None),
        ] {
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: tid,
                    owner_id: owner,
                    location_id: loc,
                    flag,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
        }
        // Triage & Passive start offline until explicitly activated below.
        snap.module_online_states.insert(triage_id, false);
        snap.module_online_states.insert(passive_id, false);
        snap.module_online_states.insert(tp1_id, true);
        snap.module_online_states.insert(tp2_id, true);
    });

    let sys = ctx.systems.get_or_spawn(sys_id);
    let spawn = |id: i64, tid: u32, pos: DVec3, mode: MovementMode, cs: CombatState| {
        sys.try_spawn_ball_with_combat(
            Ball {
                id,
                type_id: tid,
                position: pos,
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 2.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode,
            },
            cs,
        );
    };
    let base_cs = CombatState {
        shield_hp: 500.0,
        max_shield_hp: 500.0,
        armor_hp: 500.0,
        max_armor_hp: 500.0,
        structure_hp: 500.0,
        max_structure_hp: 500.0,
        capacitor: 500.0,
        max_capacitor: 500.0,
        ..Default::default()
    };
    let depot_type = ctx.sde.types.values().filter(|t| t.group_id == 1246).min_by_key(|t| t.type_id).map(|t| t.type_id).unwrap_or(33_474);
    spawn(ship_id, 999_601, DVec3::ZERO, MovementMode::Stop, base_cs.clone());
    spawn(skill_ship_id, 999_602, DVec3::new(100.0, 0.0, 0.0), MovementMode::Stop, base_cs.clone());
    spawn(depot_id, depot_type, DVec3::new(500.0, 0.0, 0.0), MovementMode::Stop, base_cs.clone());
    spawn(tp_target_id, 999_601, DVec3::new(1_000.0, 0.0, 0.0), MovementMode::Stop, base_cs.clone());
    for i in 0..8 {
        spawn(tgt_base + i, 587, DVec3::new(2_000.0 + (i as f64) * 100.0, 0.0, 0.0), MovementMode::Stop, base_cs.clone());
    }

    // 1. ItemGetInfo on ship_id preserves dyn_attrs for 18/265/266 and GetAllInfo / GetShipAttributes / GetLayerDamageValuesByItems return populated dicts
    let mut sess = make_session(char_id, corp_id, Some(sys_id), None, Some(ship_id));
    let ship_info = call_dogma_im(&ctx, &mut sess, None, "ItemGetInfo", &[PyRep::Integer(ship_id)]).await.unwrap();
    let ship_attrs = ship_info.dict_get("attributes").expect("attributes dict");
    assert_eq!(ship_attrs.dict_get("18").or_else(|| {
        if let PyRep::Dict(entries) = ship_attrs {
            entries.iter().find(|(k, _)| k.as_i64() == Some(18)).map(|(_, v)| v)
        } else { None }
    }).and_then(PyRep::as_f64), Some(42.5));
    let all_info = call_dogma_im(&ctx, &mut sess, None, "GetAllInfo", &[PyRep::Bool(true), PyRep::Bool(true)]).await.unwrap();
    assert!(all_info.dict_get("shipInfo").is_some());
    assert!(all_info.dict_get("charInfo").is_some());
    let ship_only_attrs = call_dogma_im(&ctx, &mut sess, None, "GetShipAttributes", &[]).await.unwrap();
    assert!(!matches!(ship_only_attrs, PyRep::None));
    let dmg_map = call_dogma_im(&ctx, &mut sess, None, "GetLayerDamageValuesByItems", &[PyRep::List(vec![PyRep::Integer(ship_id)])]).await.unwrap();
    if let PyRep::Dict(entries) = &dmg_map {
        let ship_dmg = &entries.iter().find(|(k, _)| k.as_i64() == Some(ship_id)).expect("ship dmg").1;
        assert!(ship_dmg.dict_get("shieldInfo").is_some());
        assert!(ship_dmg.dict_get("armorDamage").is_some());
        assert!(ship_dmg.dict_get("hullDamage").is_some());
    } else {
        panic!("expected Dict from GetLayerDamageValuesByItems");
    }

    // 2. AddTarget without passive targeter -> yellow-box notification sent, passiveLock=false, max_locked=1 enforced
    let _ = ctx.db.acquire_online_lock(target_char_id as u64, 1, Some(target_char_id), false, ctx.now_unix());
    ctx.pending_notifications.write().unwrap().remove(&target_char_id);
    let first_lock = call_dogma_im(&ctx, &mut sess, None, "AddTarget", &[PyRep::Integer(tp_target_id)]).await.unwrap();
    assert_eq!(first_lock.dict_get("locked").and_then(PyRep::as_bool), Some(false));
    assert_eq!(first_lock.dict_get("passiveLock").and_then(PyRep::as_bool), Some(false));
    assert!(
        ctx.pending_notifications.read().unwrap().get(&target_char_id).is_some_and(|v| !v.is_empty()),
        "non-passive AddTarget must notify target"
    );
    // Duplicate AddTarget while locking returns locked=false & positive lockTimeMs
    let relock_pending = call_dogma_im(&ctx, &mut sess, None, "AddTarget", &[PyRep::Integer(tp_target_id)]).await.unwrap();
    assert_eq!(relock_pending.dict_get("locked").and_then(PyRep::as_bool), Some(false));
    // Complete lock -> duplicate AddTarget returns locked=true, targetIDs containing target, lockTimeMs=0.0
    let _ = sys.add_target_lock(ship_id, TargetLock { target_id: tp_target_id, lock_time_ms: 1000.0, remaining_lock_ms: 0.0, locked: true, max_range_m: None }).await;
    let relock_done = call_dogma_im(&ctx, &mut sess, None, "AddTarget", &[PyRep::Integer(tp_target_id)]).await.unwrap();
    assert_eq!(relock_done.dict_get("locked").and_then(PyRep::as_bool), Some(true));
    assert_eq!(relock_done.dict_get("lockTimeMs").and_then(PyRep::as_f64), Some(0.0));
    assert_eq!(
        call_dogma_im(&ctx, &mut sess, None, "AddTarget", &[PyRep::Integer(tgt_base)]).await.unwrap_err().code,
        "TargetingFailed"
    );

    // 3. Triage I (27951) adds +4 max targets (1 + 4 = 5); Triage II (4294) adds +5 (1 + 5 = 6)
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(triage_id, true);
        snap.module_active_states.insert(triage_id);
    });
    ctx.invalidate_ship_fitting(ship_id);
    for i in 0..4 {
        assert!(call_dogma_im(&ctx, &mut sess, None, "AddTarget", &[PyRep::Integer(tgt_base + i)]).await.is_ok());
    }
    assert_eq!(
        call_dogma_im(&ctx, &mut sess, None, "AddTarget", &[PyRep::Integer(tgt_base + 4)]).await.unwrap_err().code,
        "TargetingFailed"
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&triage_id).unwrap().type_id = 4294;
    });
    ctx.invalidate_ship_fitting(ship_id);
    assert!(call_dogma_im(&ctx, &mut sess, None, "AddTarget", &[PyRep::Integer(tgt_base + 4)]).await.is_ok());
    assert_eq!(
        call_dogma_im(&ctx, &mut sess, None, "AddTarget", &[PyRep::Integer(tgt_base + 5)]).await.unwrap_err().code,
        "TargetingFailed"
    );
    let _ = call_dogma_im(&ctx, &mut sess, None, "ClearTargets", &[]).await.unwrap();

    // 4. Active Passive Targeter (group 82) -> passiveLock=true and suppresses target OnCombatTargetsChanged notification
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(passive_id, true);
        snap.module_active_states.insert(passive_id);
    });
    ctx.invalidate_ship_fitting(ship_id);
    ctx.pending_notifications.write().unwrap().remove(&target_char_id);
    let passive_res = call_dogma_im(&ctx, &mut sess, None, "AddTarget", &[PyRep::Integer(tp_target_id)]).await.unwrap();
    assert_eq!(passive_res.dict_get("passiveLock").and_then(PyRep::as_bool), Some(true));
    assert!(
        !ctx.pending_notifications.read().unwrap().get(&target_char_id).is_some_and(|v| !v.is_empty()),
        "passive targeter must suppress yellow-box notification"
    );

    // 5. Stale lock pruning in AddTarget: warp-exit invuln alone, MovementMode::Warp, exact max_range_m boundary (== retained vs > pruned)
    let id_warp_exit = 96_503_001i64;
    let id_warping = 96_503_002i64;
    let id_exact_edge = 96_503_003i64;
    let id_over_edge = 96_503_004i64;
    let mut warp_exit_cs = base_cs.clone();
    warp_exit_cs.sync_invuln_with_undock_timer = false;
    warp_exit_cs.warp_exit_invulnerable_remaining_ticks = 10;
    spawn(id_warp_exit, 587, DVec3::new(3_000.0, 0.0, 0.0), MovementMode::Stop, warp_exit_cs);
    spawn(id_warping, 587, DVec3::new(3_100.0, 0.0, 0.0), MovementMode::new_warp(DVec3::ZERO, DVec3::new(1e7, 0.0, 0.0), 3.0), base_cs.clone());
    spawn(id_exact_edge, 587, DVec3::new(50_000.0, 0.0, 0.0), MovementMode::Stop, base_cs.clone());
    spawn(id_over_edge, 587, DVec3::new(50_001.0, 0.0, 0.0), MovementMode::Stop, base_cs.clone());
    for (tid, mr) in [(id_warp_exit, None), (id_warping, None), (id_exact_edge, Some(50_000.0)), (id_over_edge, Some(50_000.0))] {
        let _ = sys.add_target_lock(ship_id, TargetLock { target_id: tid, lock_time_ms: 1000.0, remaining_lock_ms: 0.0, locked: true, max_range_m: mr }).await;
    }
    assert!(call_dogma_im(&ctx, &mut sess, None, "AddTarget", &[PyRep::Integer(tgt_base)]).await.is_ok());
    let locks_after = sys.get_target_locks(ship_id).await.unwrap();
    assert!(locks_after.iter().any(|l| l.target_id == id_exact_edge));
    assert!(!locks_after.iter().any(|l| l.target_id == id_warp_exit));
    assert!(!locks_after.iter().any(|l| l.target_id == id_warping));
    assert!(!locks_after.iter().any(|l| l.target_id == id_over_edge));

    // 6. Character skill_char_max bottleneck (Target Management 1 + Adv 0 => max 3 on 10-slot ship)
    let mut skill_sess = make_session(target_char_id, corp_id, Some(sys_id), None, Some(skill_ship_id));
    for i in 0..3 {
        assert!(call_dogma_im(&ctx, &mut skill_sess, None, "AddTarget", &[PyRep::Integer(tgt_base + i)]).await.is_ok());
    }
    assert_eq!(
        call_dogma_im(&ctx, &mut skill_sess, None, "AddTarget", &[PyRep::Integer(tgt_base + 3)]).await.unwrap_err().code,
        "TargetingFailed"
    );

    // 7. DestroyFitting tp_sig_multiplier transition: 2 TPs -> destroy 1 leaves Some -> destroy 2nd clears to None
    let tp1_state = DogmaItemState::from_sde(char_id, 999_603, &ctx.sde);
    let tp2_state = DogmaItemState::from_sde(char_id, 999_603, &ctx.sde);
    ctx.upsert_projected_ewar_on_ship(ship_id, tp_target_id, ProjectedEwarEffect {
        source_module_id: tp1_id,
        source_module_type_id: 999_603,
        charge_type_id: None,
        distance: 1_000.0,
        overloaded: false,
        effective_module: tp1_state,
    });
    ctx.upsert_projected_ewar_on_ship(ship_id, tp_target_id, ProjectedEwarEffect {
        source_module_id: tp2_id,
        source_module_type_id: 999_603,
        charge_type_id: None,
        distance: 1_000.0,
        overloaded: false,
        effective_module: tp2_state,
    });
    {
        let mut active = ctx.active_ewar_starts.write().unwrap();
        active.insert((ship_id, tp1_id), vec![(tp_target_id, "targetPainter")]);
        active.insert((ship_id, tp2_id), vec![(tp_target_id, "targetPainter")]);
    }
    call_dogma_im(&ctx, &mut sess, None, "DestroyFitting", &[PyRep::Integer(tp1_id)]).await.unwrap();
    let cs_after_one = sys.get_combat_state(tp_target_id).await.unwrap().unwrap();
    assert!(cs_after_one.tp_sig_multiplier.is_some_and(|m| m > 1.0));
    call_dogma_im(&ctx, &mut sess, None, "DestroyFitting", &[PyRep::Integer(tp2_id)]).await.unwrap();
    let cs_after_two = sys.get_combat_state(tp_target_id).await.unwrap().unwrap();
    assert_eq!(cs_after_two.tp_sig_multiplier, None);

    // 8. Weapon grouping ownership checks + state/notification verification
    for (m, a) in [
        ("LinkWeapons", vec![PyRep::Integer(skill_ship_id), PyRep::Integer(gun1_id), PyRep::Integer(gun2_id)]),
        ("PeelAndLink", vec![PyRep::Integer(skill_ship_id), PyRep::Integer(gun1_id), PyRep::Integer(gun2_id)]),
        ("MergeModuleGroups", vec![PyRep::Integer(skill_ship_id), PyRep::Integer(gun1_id), PyRep::Integer(gun2_id)]),
        ("UnlinkModule", vec![PyRep::Integer(skill_ship_id), PyRep::Integer(gun1_id)]),
        ("DestroyWeaponBank", vec![PyRep::Integer(skill_ship_id), PyRep::Integer(gun1_id)]),
        ("UnlinkAllModules", vec![PyRep::Integer(skill_ship_id)]),
    ] {
        assert_eq!(
            call_dogma_im(&ctx, &mut sess, None, m, &a).await.unwrap_err().code,
            "ShipOwnershipViolation"
        );
    }
    let link_all = call_dogma_im(&ctx, &mut sess, None, "LinkAllWeapons", &[PyRep::Integer(ship_id)]).await.unwrap();
    assert!(!matches!(link_all, PyRep::None));
    ctx.db.with_snapshot(|snap| assert!(!snap.weapon_banks.get(&ship_id).unwrap().is_empty()));
}

#[tokio::test]
async fn issue_830_get_layer_damage_values_and_load_ammo_accept_python_sets() {
    let ctx = ServerContext::with_sde_and_repository(
        shared_sde(),
        Arc::new(WorldRepository::with_demo_seed()),
    );
    let char_id = 96_830_001i64;
    let corp_id = 1_000_009i64;
    let sys_id = 30_000_142u32;
    let ship_id = 96_830_100i64;
    let drone_id = 96_830_101i64;
    let cap_mod_id = 96_830_102i64;
    let cap_charge_id = 96_830_103i64;
    let cap_mod_id2 = 96_830_104i64;
    let cap_charge_id2 = 96_830_105i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.accounts.insert(
            "u830".into(),
            AccountRecord {
                user_id: char_id as u64,
                username: "u830".into(),
                password_token: String::new(),
                role: 0,
                banned_until_unix: None,
                ban_reason: None,
            },
        );
        let mut ch = snap.characters.values().next().cloned().unwrap();
        ch.user_id = char_id as u64;
        ch.character_id = char_id;
        ch.corporation_id = corp_id;
        ch.solar_system_id = sys_id;
        ch.station_id = None;
        ch.structure_id = None;
        ch.active_ship_id = ship_id;
        snap.characters.insert(char_id, ch);
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            drone_id,
            InventoryItemRecord {
                item_id: drone_id,
                type_id: 2454,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::DroneBay,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            cap_mod_id,
            InventoryItemRecord {
                item_id: cap_mod_id,
                type_id: 3556,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::MedSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            cap_charge_id,
            InventoryItemRecord {
                item_id: cap_charge_id,
                type_id: 3554,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 4,
                singleton: false,
                ..Default::default()
            },
        );
        snap.items.insert(
            cap_mod_id2,
            InventoryItemRecord {
                item_id: cap_mod_id2,
                type_id: 3556,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::MedSlot1,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            cap_charge_id2,
            InventoryItemRecord {
                item_id: cap_charge_id2,
                type_id: 3554,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 4,
                singleton: false,
                ..Default::default()
            },
        );
    });

    let _ = ctx.db.acquire_online_lock(char_id as u64, 1, Some(char_id), false, ctx.now_unix());

    let mut sess = make_session(char_id, corp_id, Some(sys_id), None, Some(ship_id));

    let extract_dict_keys = |rep: PyRep| -> Vec<i64> {
        let PyRep::Dict(entries) = rep else {
            panic!("expected PyRep::Dict from GetLayerDamageValuesByItems, got {rep:?}");
        };
        for (_, val) in &entries {
            assert!(val.dict_get("shieldInfo").is_some(), "missing shieldInfo");
            assert!(val.dict_get("armorDamage").is_some(), "missing armorDamage");
            assert!(val.dict_get("hullDamage").is_some(), "missing hullDamage");
        }
        entries.iter().filter_map(|(k, _)| k.as_i64()).collect()
    };

    // 1. PyRep::PySet returns exact drone_id (not ship_id fallback)
    let pyset_rep = call_dogma_im(
        &ctx,
        &mut sess,
        None,
        "GetLayerDamageValuesByItems",
        &[PyRep::PySet(vec![PyRep::Integer(drone_id)])],
    )
    .await
    .unwrap();
    assert_eq!(extract_dict_keys(pyset_rep), vec![drone_id]);

    // 2. PyRep::Set returns exact drone_id
    let set_rep = call_dogma_im(
        &ctx,
        &mut sess,
        None,
        "GetLayerDamageValuesByItems",
        &[PyRep::Set(vec![PyRep::Integer(drone_id)])],
    )
    .await
    .unwrap();
    assert_eq!(extract_dict_keys(set_rep), vec![drone_id]);

    // 3. PyRep::ObjectEx (builtins.set) returns exact drone_id
    let obj_ex_set = PyRep::ObjectEx {
        header: Box::new(PyRep::Tuple(vec![
            PyRep::String("builtins.set".into()),
            PyRep::Tuple(vec![PyRep::List(vec![PyRep::Integer(drone_id)])]),
        ])),
        list: vec![],
        dict: vec![],
        is_type_2: false,
    };
    let objex_rep = call_dogma_im(
        &ctx,
        &mut sess,
        None,
        "GetLayerDamageValuesByItems",
        &[obj_ex_set],
    )
    .await
    .unwrap();
    assert_eq!(extract_dict_keys(objex_rep), vec![drone_id]);

    // 4. Empty PySet falls back to session.ship_id
    let empty_set_rep = call_dogma_im(
        &ctx,
        &mut sess,
        None,
        "GetLayerDamageValuesByItems",
        &[PyRep::PySet(vec![])],
    )
    .await
    .unwrap();
    assert_eq!(extract_dict_keys(empty_set_rep), vec![ship_id]);

    // 5. Non-positive IDs inside PySet are filtered out (only non-positive -> ship_id fallback; mixed -> only positive IDs)
    let only_non_pos_rep = call_dogma_im(
        &ctx,
        &mut sess,
        None,
        "GetLayerDamageValuesByItems",
        &[PyRep::PySet(vec![PyRep::Integer(0), PyRep::Integer(-5)])],
    )
    .await
    .unwrap();
    assert_eq!(extract_dict_keys(only_non_pos_rep), vec![ship_id]);

    let mixed_rep = call_dogma_im(
        &ctx,
        &mut sess,
        None,
        "GetLayerDamageValuesByItems",
        &[PyRep::PySet(vec![
            PyRep::Integer(0),
            PyRep::Integer(-7),
            PyRep::Integer(drone_id),
        ])],
    )
    .await
    .unwrap();
    assert_eq!(extract_dict_keys(mixed_rep), vec![drone_id]);

    // 6. dogmaIM.LoadAmmo / handle_load_ammo also accepts Python sets for module IDs and charge IDs
    ctx.pending_notifications.write().unwrap().remove(&char_id);
    let direct_load = crate::services::notify_emitters_seasons_tactical::handle_load_ammo(
        &ctx,
        &sess,
        &[
            PyRep::Integer(ship_id),
            PyRep::PySet(vec![PyRep::Integer(cap_mod_id)]),
            PyRep::PySet(vec![PyRep::Integer(cap_charge_id)]),
        ],
    )
    .expect("handle_load_ammo accepts PySet module and charge IDs");
    assert_eq!(direct_load.dict_get("totalLoaded"), Some(&PyRep::Integer(2)));
    ctx.db.with_snapshot(|snap| {
        let charge_item = snap
            .items
            .values()
            .find(|i| i.location_id == ship_id && i.flag == InventoryFlag::MedSlot0 && i.type_id == 3554)
            .expect("charge loaded into MedSlot0");
        assert_eq!(charge_item.quantity, 2);
    });
    let events: Vec<String> = ctx
        .pending_notifications
        .read()
        .unwrap()
        .get(&char_id)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|n| n.event)
        .collect();
    assert!(
        events.iter().any(|e| e == "OnChargeBeingLoadedToModule"),
        "expected OnChargeBeingLoadedToModule in {events:?}"
    );
    assert!(
        events.iter().any(|e| e == "OnChargeLoadedToModule"),
        "expected OnChargeLoadedToModule in {events:?}"
    );

    // Verify dispatch through dogmaIM service with PyRep::Set and PyRep::ObjectEx sets
    let rpc_load = call_dogma_im(
        &ctx,
        &mut sess,
        None,
        "LoadAmmo",
        &[
            PyRep::Integer(ship_id),
            PyRep::Set(vec![PyRep::Integer(cap_mod_id2)]),
            PyRep::ObjectEx {
                header: Box::new(PyRep::Tuple(vec![
                    PyRep::String("builtins.set".into()),
                    PyRep::Tuple(vec![PyRep::List(vec![PyRep::Integer(cap_charge_id2)])]),
                ])),
                list: vec![],
                dict: vec![],
                is_type_2: false,
            },
        ],
    )
    .await
    .expect("dogmaIM.LoadAmmo accepts Set and ObjectEx");
    assert_eq!(rpc_load.dict_get("totalLoaded"), Some(&PyRep::Integer(2)));
    ctx.db.with_snapshot(|snap| {
        let charge_item = snap
            .items
            .values()
            .find(|i| i.location_id == ship_id && i.flag == InventoryFlag::MedSlot1 && i.type_id == 3554)
            .expect("charge loaded into MedSlot1");
        assert_eq!(charge_item.quantity, 2);
    });

    // 7. dogmaIM.UnloadAmmo accepts PySet and ObjectEx sets and moves charges back to Cargo
    ctx.pending_notifications.write().unwrap().remove(&char_id);
    let unload_1 = call_dogma_im(
        &ctx,
        &mut sess,
        None,
        "UnloadAmmo",
        &[PyRep::Integer(ship_id), PyRep::PySet(vec![PyRep::Integer(cap_mod_id)])],
    )
    .await
    .expect("dogmaIM.UnloadAmmo accepts PySet");
    assert_eq!(unload_1, PyRep::None);

    let unload_2 = call_dogma_im(
        &ctx,
        &mut sess,
        None,
        "UnloadAmmo",
        &[
            PyRep::Integer(ship_id),
            PyRep::ObjectEx {
                header: Box::new(PyRep::Tuple(vec![
                    PyRep::String("builtins.set".into()),
                    PyRep::Tuple(vec![PyRep::List(vec![PyRep::Integer(cap_mod_id2)])]),
                ])),
                list: vec![],
                dict: vec![],
                is_type_2: false,
            },
        ],
    )
    .await
    .expect("dogmaIM.UnloadAmmo accepts ObjectEx");
    assert_eq!(unload_2, PyRep::None);

    ctx.db.with_snapshot(|snap| {
        assert!(
            !snap.items.values().any(|i| i.location_id == ship_id
                && matches!(i.flag, InventoryFlag::MedSlot0 | InventoryFlag::MedSlot1)
                && i.type_id == 3554),
            "charges should be unloaded from MedSlot0 and MedSlot1"
        );
        let cargo_qty: i32 = snap
            .items
            .values()
            .filter(|i| i.location_id == ship_id && i.flag == InventoryFlag::Cargo && i.type_id == 3554)
            .map(|i| i.quantity)
            .sum();
        assert_eq!(cargo_qty, 8);
    });
    let unload_events: Vec<String> = ctx
        .pending_notifications
        .read()
        .unwrap()
        .get(&char_id)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|n| n.event)
        .collect();
    assert!(
        unload_events.iter().any(|e| e == "OnItemChange"),
        "expected OnItemChange after UnloadAmmo in {unload_events:?}"
    );
}



