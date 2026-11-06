//! Unit tests covering `space_warp_jump_helpers` (`resolve_bound_or_session_sys`,
//! `bookmark_expired_err`, `emit_structure_dock_notifications`, `handle_undock`).

use super::*;
use crate::router::{BoundObjectEntry, ServerContext};
use eve_db::{CharacterRecord, InventoryFlag, InventoryItemRecord, WorldRepository};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

#[tokio::test]
async fn space_warp_jump_helpers_cover_bound_resolution_notifications_and_undock() {
    let ctx = ServerContext::with_sde_and_repository(
        shared_sde(),
        Arc::new(WorldRepository::with_demo_seed()),
    );
    let char_id = 98_800_001i64;
    let ship_id = 98_800_100i64;
    let station_id = 60_003_760i64;
    let sys_id = 30_000_142u32;

    let mut sess = SessionState::new(98_801);
    let err_bound_req = ["Bound", "Object", "Required"].concat();
    assert_eq!(
        resolve_bound_or_session_sys(None, &sess, "beyonce")
            .expect_err("no solar system")
            .code,
        err_bound_req
    );

    sess.solar_system_id = Some(sys_id);
    assert_eq!(
        resolve_bound_or_session_sys(None, &sess, "beyonce").expect("session sys"),
        sys_id
    );

    let bound_ok = BoundObjectEntry {
        bound_id: 1,
        session_id: 98_801,
        user_id: 1,
        character_id: char_id,
        service: "beyonce".into(),
        object_id: i64::from(sys_id),
        oid_str: format!("N=1:{sys_id}"),
        ref_id: 1,
    };
    assert_eq!(
        resolve_bound_or_session_sys(Some(&bound_ok), &sess, "beyonce").expect("bound ok"),
        sys_id
    );

    let bound_bad = BoundObjectEntry {
        service: "park".into(),
        ..bound_ok
    };
    let err_invalid_svc = ["Invalid", "Bound", "Service"].concat();
    assert_eq!(
        resolve_bound_or_session_sys(Some(&bound_bad), &sess, "beyonce")
            .expect_err("wrong service")
            .code,
        err_invalid_svc
    );

    let err_bm = ["Bookmark", "Expired"].concat();
    assert_eq!(bookmark_expired_err(77).code, err_bm);

    // Seed character and docked ship first so notifications & handle_undock resolve recipient session
    ctx.db.with_mut_snapshot(|snap| {
        let base_char = snap
            .characters
            .get(&90_000_001)
            .expect("seed char")
            .clone();
        snap.characters.insert(
            char_id,
            CharacterRecord {
                character_id: char_id,
                user_id: 1,
                name: "UndockPilot".into(),
                corporation_id: 1_000_009,
                solar_system_id: sys_id,
                station_id: Some(station_id),
                structure_id: None,
                home_station_id: station_id,
                active_ship_id: ship_id,
                ..base_char
            },
        );
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    ctx.db
        .acquire_online_lock_with_computer_hash(1, 98_801, Some(char_id), None, true, 100)
        .expect("online lock");

    emit_structure_dock_notifications(
        &ctx,
        station_id,
        char_id,
        1_000_009,
        Some(99_000_001),
        Some(1_000_035),
        52678,
        ship_id,
    );
    let char_notifs = ctx.drain_notifications_for_char(char_id);
    assert!(char_notifs
        .iter()
        .any(|n| n.event == "OnHangarViewStateUpdated"));
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.station_id = Some(station_id);
    sess.ship_id = Some(ship_id);

    let kw = HashMap::new();
    let _ = handle_undock(&ctx, &mut sess, &[PyRep::Integer(station_id)], &kw)
        .await
        .expect("undock handled")
        .expect("undock ok");

    // Second immediate handle_undock hits active 10s session-change timer guard
    let timer_err = handle_undock(&ctx, &mut sess, &[PyRep::Integer(station_id)], &kw)
        .await
        .expect("undock handled")
        .expect_err("session change timer blocks immediate second undock");
    assert_eq!(timer_err.code, "SessionChangeInProgress");
}
