use super::*;
use crate::services::air_npe_nodegraph_runner::on_start_tutorial;
use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::make_fighter_ctx_and_session;

#[tokio::test]
async fn test_air_npe_pocket_spawn_move_ship_and_cleanup() {
    let (ctx, mut session, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    session.station_id = Some(60_003_760);

    // 1. Spawn NPE pocket and verify idempotency on second call
    let (inst_id, obj_ids, npc_ids) =
        spawn_or_get_npe_pocket(&ctx, char_id, sys_id, NPE_DUNGEON_SCENE_1_ID);
    assert!(inst_id > 0);
    assert_eq!(obj_ids.len(), 2);
    assert_eq!(npc_ids.len(), 1);

    let (inst_id2, obj_ids2, npc_ids2) =
        spawn_or_get_npe_pocket(&ctx, char_id, sys_id, NPE_DUNGEON_SCENE_1_ID);
    assert_eq!(inst_id, inst_id2);
    assert!(obj_ids2.is_empty());
    assert!(npc_ids2.is_empty());

    // 2. Spawn or move starter ship in pocket
    spawn_or_move_ship_in_pocket(&ctx, char_id, sys_id, ship_id, 596);
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let has_ship_ball = sys_handle
        .ballpark
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .balls
        .contains_key(&ship_id);
    assert!(has_ship_ball);

    // 3. Start tutorial so runner state exists, mark moved_into_pocket, then cleanup and restore UI
    on_start_tutorial(&ctx, &mut session, char_id, 1, Some(1));
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(mut r) = load_runner_state(snap, char_id) {
            r.moved_into_pocket = true;
            r.origin_station_id = Some(60_003_760);
            save_runner_state(snap, &r);
        }
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.station_id = None;
        }
    });
    session.station_id = None;

    let (stopped, stop_master) =
        cleanup_npe_pocket_and_restore_ui(&ctx, Some(&mut session), char_id);
    assert!(!stopped.is_empty());
    assert!(stop_master.is_some());
    assert_eq!(session.station_id, Some(60_003_760));
}
