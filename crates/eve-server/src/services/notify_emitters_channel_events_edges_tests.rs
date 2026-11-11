//! Edge-case tests for `notify_emitters_channel_events`.

use super::*;
use crate::services::pve_dungeon_runtime::DungeonTriggerAction;
use eve_db::clock::VirtualServerClock;
use eve_db::WorldRepository;
use eve_proto::PyRep;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

#[test]
fn core_emitters_with_none_sinks_and_wildcard_trigger_actions() {
    let db = Arc::new(WorldRepository::with_demo_seed());
    db.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_775_400_000)));
    let char_id = 90_000_001_i64;

    // None for both pending_notifications and notify_tx (exercises false branches in push_char_notification_core)
    emit_dungeon_camera_interest_core(&db, None, None, char_id, 555, 1.0, 2.0);
    emit_dungeon_trigger_ambient_sound_core(&db, None, None, char_id, 10, 20, 30, 40);
    emit_dungeon_trigger_audio_core(&db, None, None, char_id, 10, "res:/Audio/ambient.ogg");
    emit_dungeon_trigger_message_core(&db, None, None, char_id, 1, 2);
    emit_dungeon_trigger_music_core(&db, None, None, char_id, 10, 99);

    let pend: RwLock<HashMap<i64, Vec<PendingMachoNotification>>> = RwLock::new(HashMap::new());
    // CameraInterest with item_id == 0 and empty obj_ball_ids -> falls back to 0;
    // UnlockGate / SpawnNpcGroup / CompleteDungeon exercise the `_ => {}` wildcard arm.
    dispatch_dungeon_trigger_notify_actions(
        &db,
        Some(&pend),
        None,
        char_id,
        700,
        &[],
        vec![
            DungeonTriggerAction::UnlockGate,
            DungeonTriggerAction::SpawnNpcGroup(1),
            DungeonTriggerAction::CompleteDungeon,
            DungeonTriggerAction::CameraInterest {
                item_id: 0,
                speed_bits: 1.0_f64.to_bits(),
                duration_bits: 3.0_f64.to_bits(),
            },
        ],
    );

    let guard = pend.read().unwrap();
    let q = guard.get(&char_id).expect("queued fallback notification");
    assert_eq!(q.len(), 1);
    assert_eq!(q[0].event, "OnDungeonCameraInterest");
    assert_eq!(
        q[0].args,
        vec![PyRep::Integer(0), PyRep::Real(1.0), PyRep::Real(3.0)]
    );
}
