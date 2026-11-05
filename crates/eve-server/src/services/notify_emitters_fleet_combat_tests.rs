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
