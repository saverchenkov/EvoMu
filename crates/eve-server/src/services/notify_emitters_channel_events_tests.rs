//! Unit tests for `notify_emitters_channel_events`.

use super::*;
use crate::router::ServerContext;
use crate::services::pve_dungeon_runtime::DungeonTriggerAction;
use eve_db::clock::VirtualServerClock;
use eve_db::WorldRepository;
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use tokio::sync::broadcast;

fn make_online_ctx() -> (ServerContext, i64) {
    let sde = Arc::new(SdeRegistry::default());
    let db = Arc::new(WorldRepository::with_demo_seed());
    let char_id = 90_000_001;
    db.acquire_online_lock(1, 501, Some(char_id), true, 1_775_400_000)
        .expect("online lock");
    let ctx = ServerContext::with_sde_and_repository(sde, db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_775_400_000)));
    (ctx, char_id)
}

#[test]
fn operations_and_forced_decloak_notifications_match_v24_shapes() {
    let (ctx, char_id) = make_online_ctx();

    emit_operation_deactivated(&ctx, char_id);
    emit_operation_skipped(&ctx, char_id, 5, 19);
    emit_operations_progress_reset(&ctx, char_id, &[101, 102], &[201], 1, 2, 3);
    emit_operations_server_fsd_reload(&ctx, char_id);
    emit_cloak_module_force_deactivated(&ctx, char_id, "DecloakedByProximity", 1_450.5);

    let drained = ctx.drain_notifications_for_char(char_id);
    assert_eq!(drained.len(), 5);

    assert_eq!(drained[0].event, "OnOperationDeactivated");
    assert_eq!(drained[0].id_type, "charid");
    assert!(drained[0].args.is_empty());

    assert_eq!(drained[1].event, "OnOperationSkipped");
    assert_eq!(
        drained[1].args,
        vec![PyRep::Integer(5), PyRep::Integer(19)]
    );

    assert_eq!(drained[2].event, "OnOperationsProgressReset");
    assert_eq!(
        drained[2].args,
        vec![
            PyRep::List(vec![PyRep::Integer(101), PyRep::Integer(102)]),
            PyRep::List(vec![PyRep::Integer(201)]),
            PyRep::Tuple(vec![
                PyRep::Integer(1),
                PyRep::Integer(2),
                PyRep::Integer(3),
            ]),
        ]
    );

    assert_eq!(drained[3].event, "OnOperationsServerFSDReload");
    assert!(drained[3].args.is_empty());

    assert_eq!(drained[4].event, "OnCloakModuleForceDeactivated");
    assert_eq!(
        drained[4].args,
        vec![
            PyRep::String("DecloakedByProximity".into()),
            PyRep::Dict(vec![(
                PyRep::String("distance".into()),
                PyRep::Real(1_450.5)
            )]),
        ]
    );
}

#[test]
fn dungeon_trigger_core_and_dispatcher_handle_queue_cap_and_fallback_targets() {
    let (ctx, char_id) = make_online_ctx();
    let pend: RwLock<HashMap<i64, Vec<PendingMachoNotification>>> = RwLock::new(HashMap::new());
    let (tx, mut rx) = broadcast::channel(300);

    // Non-positive char_id does nothing
    emit_dungeon_camera_interest_core(&ctx.db, Some(&pend), Some(&tx), 0, 10, 1.0, 2.0);
    emit_dungeon_camera_interest_core(&ctx.db, Some(&pend), Some(&tx), -1, 10, 1.0, 2.0);
    assert!(pend.read().unwrap().is_empty());
    assert!(rx.try_recv().is_err());

    // Queue cap at 256
    for i in 0..257_i64 {
        emit_dungeon_trigger_music_core(&ctx.db, Some(&pend), None, char_id, 500, i);
    }
    {
        let g = pend.read().unwrap();
        let q = g.get(&char_id).expect("queue");
        assert_eq!(q.len(), 256);
        assert_eq!(q[0].args, vec![PyRep::Integer(500), PyRep::Integer(1)]);
        assert_eq!(q[255].args, vec![PyRep::Integer(500), PyRep::Integer(256)]);
    }
    pend.write().unwrap().clear();

    // Dispatch all DungeonTriggerAction variants including item_id > 0 and item_id == 0 fallback
    let actions = vec![
        DungeonTriggerAction::CameraInterest {
            item_id: 9001,
            speed_bits: 1.5_f64.to_bits(),
            duration_bits: 4.0_f64.to_bits(),
        },
        DungeonTriggerAction::CameraInterest {
            item_id: 0,
            speed_bits: 2.0_f64.to_bits(),
            duration_bits: 5.0_f64.to_bits(),
        },
        DungeonTriggerAction::TriggerAmbientSound {
            sound_id: 77,
            entity_item_id: 8001,
            emitter_id: 33,
        },
        DungeonTriggerAction::TriggerAudio {
            audio_url: "res:/Audio/boss.ogg".into(),
        },
        DungeonTriggerAction::TriggerMessage {
            message_type: 2,
            message_id: 4444,
        },
        DungeonTriggerAction::TriggerMusic {
            music_trigger_id: 888,
        },
    ];
    dispatch_dungeon_trigger_notify_actions(
        &ctx.db,
        Some(&pend),
        Some(&tx),
        char_id,
        650,
        &[777_001, 777_002],
        actions,
    );

    let g = pend.read().unwrap();
    let q = g.get(&char_id).expect("dispatched actions");
    assert_eq!(q.len(), 6);
    assert_eq!(q[0].event, "OnDungeonCameraInterest");
    assert_eq!(
        q[0].args,
        vec![PyRep::Integer(9001), PyRep::Real(1.5), PyRep::Real(4.0)]
    );
    // item_id == 0 falls back to obj_ball_ids[0] = 777_001
    assert_eq!(
        q[1].args,
        vec![PyRep::Integer(777_001), PyRep::Real(2.0), PyRep::Real(5.0)]
    );
    assert_eq!(q[2].event, "OnDungeonTriggerAmbientSound");
    assert_eq!(
        q[2].args,
        vec![PyRep::Integer(650), PyRep::Integer(77), PyRep::Integer(8001)]
    );
    assert_eq!(q[3].event, "OnDungeonTriggerAudio");
    assert_eq!(
        q[3].args,
        vec![
            PyRep::Integer(650),
            PyRep::String("res:/Audio/boss.ogg".into())
        ]
    );
    assert_eq!(q[4].event, "OnDungeonTriggerMessage");
    assert_eq!(q[4].args, vec![PyRep::Integer(2), PyRep::Integer(4444)]);
    assert_eq!(q[5].event, "OnDungeonTriggerMusic");
    assert_eq!(q[5].args, vec![PyRep::Integer(650), PyRep::Integer(888)]);
}
