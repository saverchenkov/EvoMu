//! Unit tests for `notify_emitters_seasons_tactical`.

use super::*;
use crate::router::ServerContext;
use eve_db::clock::VirtualServerClock;
use eve_db::{FleetRecord, InventoryFlag, InventoryItemRecord, WorldRepository};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::{InvType, SdeRegistry};
use std::collections::HashSet;
use std::sync::Arc;

fn make_test_type(type_id: u32, group_id: u32, category_id: u32, volume: f64, capacity: f64) -> InvType {
    InvType {
        type_id,
        group_id,
        category_id,
        name: format!("Type-{type_id}"),
        mass: 100.0,
        volume,
        capacity,
        portion_size: 1,
        radius: 10.0,
        base_price: 1000.0,
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

fn make_online_ctx() -> (ServerContext, i64, Arc<VirtualServerClock>) {
    let mut sde = SdeRegistry::default();
    // Ancillary Armor Repairer (group 1199, cap 8.0) + Nanite Repair Paste (type 28668, vol 0.01)
    sde.types.insert(32772, make_test_type(32772, 1199, 7, 5.0, 0.08));
    sde.types.insert(28668, make_test_type(28668, 916, 8, 0.01, 0.0));
    //Incompatible ammo (type 200, group 85)
    sde.types.insert(200, make_test_type(200, 85, 8, 0.01, 0.0));
    // Mobile Cyno Beacon (group 4093)
    sde.types.insert(
        56000,
        make_test_type(
            56000,
            crate::services::mobile_deployables::GROUP_MOBILE_CYNO_BEACON,
            22,
            50.0,
            0.0,
        ),
    );

    let db = Arc::new(WorldRepository::with_demo_seed());
    let char_id = 90_000_001;
    db.acquire_online_lock(1, 501, Some(char_id), true, 1_775_400_000)
        .expect("online lock");
    let ctx = ServerContext::with_sde_and_repository(Arc::new(sde), db);
    let clock = Arc::new(VirtualServerClock::with_fixed_base(1_775_400_000));
    ctx.set_clock(clock.clone());
    (ctx, char_id, clock)
}

#[test]
fn season_slash_commands_transitions_and_challenge_expiry() {
    let (ctx, char_id, _clock) = make_online_ctx();

    assert!(handle_season_slash(&ctx, char_id, "/other").is_none());
    assert_eq!(
        handle_season_slash(&ctx, char_id, "/season unknown")
            .unwrap()
            .expect_err("unknown")
            .code,
        "InvalidSlashArgument"
    );
    assert_eq!(
        handle_season_slash(&ctx, char_id, "/season start 0")
            .unwrap()
            .expect_err("sid=0")
            .code,
        "InvalidSlashArgument"
    );
    assert_eq!(
        handle_season_slash(&ctx, char_id, "/season start 10 0")
            .unwrap()
            .expect_err("days=0")
            .code,
        "InvalidSlashArgument"
    );

    // Start season 10 for 14 days
    let started = handle_season_slash(&ctx, char_id, "/season start 10 14")
        .unwrap()
        .expect("start season 10");
    assert!(!matches!(started, PyRep::None));
    let notifs = ctx.drain_notifications_for_char(char_id);
    assert!(notifs.iter().any(|n| n.event == "OnSeasonStarted"));

    // Select season 10 for char_id in blobs so ending season 10 clears it
    ctx.db.with_mut_snapshot(|snap| {
        let mut st = load_season_state_from_blobs(snap, char_id);
        st.selected_season_id = 10;
        save_season_state_to_blobs(snap, char_id, &st);
    });

    // End season 10 -> emits OnSeasonEnded and OnSeasonSelectionCleared
    let ended = handle_season_slash(&ctx, char_id, "/season end 10")
        .unwrap()
        .expect("end season 10");
    assert_eq!(
        ended,
        PyRep::keyval([
            ("seasonID", PyRep::Integer(10)),
            ("ended", PyRep::Bool(true)),
            ("clearedCharacters", PyRep::Integer(1)),
        ])
    );
    let end_notifs = ctx.drain_notifications_for_char(char_id);
    assert!(end_notifs.iter().any(|n| n.event == "OnSeasonEnded"));
    assert!(end_notifs
        .iter()
        .any(|n| n.event == "OnSeasonSelectionCleared"));

    // Clear selection and reset goals
    assert!(handle_select_season_clear_if_none_or_zero(&ctx, char_id, Some(&PyRep::Integer(5))).is_none());
    assert_eq!(
        handle_season_slash(&ctx, char_id, "/season clear_selection")
            .unwrap()
            .expect("clear"),
        PyRep::Bool(true)
    );
    assert_eq!(
        handle_season_slash(&ctx, char_id, "/season clear_selection 999999999")
            .unwrap()
            .expect_err("missing char")
            .code,
        "CharacterNotFound"
    );
    assert_eq!(
        handle_season_slash(&ctx, char_id, "/season reset_goals")
            .unwrap()
            .expect("reset"),
        PyRep::keyval([
            ("characterID", PyRep::Integer(char_id)),
            ("reset", PyRep::Bool(true)),
            ("seasonPoints", PyRep::Integer(0)),
        ])
    );
    let _ = ctx.drain_notifications_for_char(char_id);

    // Reconcile FSD reload and challenge expiration
    reconcile_seasons_on_fsd_reload(&ctx, char_id);
    emit_expired_challenges(&ctx, char_id, vec![(77, PyRep::Integer(88))]);
    let fsd_notifs = ctx.drain_notifications_for_char(char_id);
    assert!(fsd_notifs
        .iter()
        .any(|n| n.event == "OnSeasonsFsdDataChanged"));
    assert!(fsd_notifs.iter().any(|n| n.event == "OnChallengeExpired"
        && n.args == vec![PyRep::Integer(77), PyRep::Integer(88)]));
}

#[test]
fn tactical_fleet_weapon_bank_and_load_ammo_handlers() {
    let (ctx, char_id, _) = make_online_ctx();
    let ship_id = 100_000_001_i64;
    let station_id = 60_003_760_i64;
    let mod1_id = 880_001_i64;
    let mod2_id = 880_002_i64;
    let paste_id = 880_003_i64;
    let bad_ammo_id = 880_004_i64;
    let beacon_id = 880_005_i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.weapon_banks
            .entry(ship_id)
            .or_default()
            .insert(mod1_id, vec![mod1_id, mod2_id]);
        snap.fleets.insert(
            5001,
            FleetRecord {
                fleet_id: 5001,
                boss_id: char_id,
                members: vec![char_id],
                motd: String::new(),
                free_move: false,
                is_registered: false,
                ..Default::default()
            },
        );
        snap.items.insert(
            mod1_id,
            InventoryItemRecord {
                item_id: mod1_id,
                type_id: 32772,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::LoSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            mod2_id,
            InventoryItemRecord {
                item_id: mod2_id,
                type_id: 32772,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::LoSlot1,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            paste_id,
            InventoryItemRecord {
                item_id: paste_id,
                type_id: 28668,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 50,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            bad_ammo_id,
            InventoryItemRecord {
                item_id: bad_ammo_id,
                type_id: 200,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 50,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            beacon_id,
            InventoryItemRecord {
                item_id: beacon_id,
                type_id: 56000,
                owner_id: char_id,
                location_id: 30_000_142,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });

    assert_eq!(char_fleet_id(&ctx, char_id), Some(5001));
    emit_restoring_invuln_updated(&ctx, char_id, ship_id, 300_000_000);
    emit_fleet_escalating_path_if_in_fleet(&ctx, char_id, 444);
    on_deployable_deactivated(&ctx, Some(5001), beacon_id);

    let mut session = SessionState::new(501);
    session.user_id = Some(1);
    session.character_id = Some(char_id);
    session.ship_id = Some(ship_id);
    session.station_id = Some(station_id);
    session.solar_system_id = Some(30_000_142);

    // Incompatible ammo rejected
    let err = handle_load_ammo(
        &ctx,
        &session,
        &[
            PyRep::Integer(ship_id),
            PyRep::List(vec![PyRep::Integer(mod1_id)]),
            PyRep::List(vec![PyRep::Integer(bad_ammo_id)]),
        ],
    )
    .expect_err("incompatible ammo");
    assert_eq!(err.code, "IncompatibleAmmo");

    // Valid AAR paste load expands across grouped weapon bank (mod1 + mod2, 8 rounds each = 16 total)
    let res = handle_load_ammo(
        &ctx,
        &session,
        &[
            PyRep::Integer(ship_id),
            PyRep::List(vec![PyRep::Integer(mod1_id)]),
            PyRep::List(vec![PyRep::Integer(paste_id)]),
        ],
    )
    .expect("load paste succeeds");
    assert_eq!(
        res,
        PyRep::keyval([
            ("reloadTimeMs", PyRep::Real(10_000.0)),
            ("totalLoaded", PyRep::Integer(16)),
        ])
    );

    let notifs = ctx.drain_notifications_for_char(char_id);
    let events: HashSet<&str> = notifs.iter().map(|n| n.event.as_str()).collect();
    assert!(events.contains("OnRestoringInvulnUpdated"));
    assert!(events.contains("OnFleetEscalatingPathMessage"));
    assert!(events.contains("OnFleetJumpBeaconDeployableChange"));
    assert!(events.contains("OnChargeBeingLoadedToModule"));
    assert!(events.contains("OnChargeLoadedToModule"));
    assert!(events.contains("OnAmmoInBankChanged"));
}
