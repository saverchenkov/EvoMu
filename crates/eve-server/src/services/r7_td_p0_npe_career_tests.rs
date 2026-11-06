use crate::quasar_gateway::STATUS_FORBIDDEN;
use crate::router::ServerContext;
use crate::services::air_npe_nodegraph_pocket::NPE_POCKET_ANCHOR_POS;
use crate::services::air_npe_nodegraph_runner::{
    complete_air_npe_checked, load_runner_state, on_client_graph_message, on_skip_air_npe,
    on_start_tutorial, save_runner_state, transition_character_into_npe_pocket,
    AIR_NPE_NODE_GRAPH_ID, AIR_NPE_STATE_ACTIVE,
};
use crate::services::operations_manager::{self, mark_new_character_tutorial_pending};
use crate::services::quasar_career_goals::{
    career_milestone_uuid, register_career_goal_handlers, CareerGoalManager,
};
use crate::services::season_manager;
use eve_db::{InventoryFlag, InventoryItemRecord, MiningLedgerRecord, NpeProgressRecord};
use eve_destiny::CombatState;
use eve_machonet::SessionState;
use eve_proto::quasar::{AuthoritativeContext, DynamicQuasarMessage, DynamicValue, GoogleAny};
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::HashMap;
use std::sync::Arc;

fn setup_pilot_in_system(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    sys_id: u32,
    station_id: Option<i64>,
    pending_tutorial: bool,
) -> SessionState {
    let now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.get(&90_000_001).cloned().expect("seed char");
        ch.character_id = char_id;
        ch.user_id = char_id as u64;
        ch.name = format!("Pilot {char_id}");
        ch.solar_system_id = sys_id;
        ch.station_id = station_id;
        ch.structure_id = None;
        ch.home_station_id = station_id.unwrap_or(60_003_760);
        ch.active_ship_id = ship_id;
        ch.free_skill_points = 0.0;
        snap.characters.insert(char_id, ch);

        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 596,
                owner_id: char_id,
                location_id: station_id.unwrap_or(i64::from(sys_id)),
                flag: if station_id.is_some() {
                    InventoryFlag::Hangar
                } else {
                    InventoryFlag::None
                },
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        if pending_tutorial {
            mark_new_character_tutorial_pending(snap, char_id, now);
            snap.npe_progress.insert(
                char_id,
                NpeProgressRecord {
                    character_id: char_id,
                    node_graph_id: 473,
                    current_node_id: 1,
                    completed: false,
                    career_goal_progress: Default::default(),
                    claimed_goals: Default::default(),
                },
            );
        } else {
            snap.npe_progress.remove(&char_id);
        }
    });

    SessionState {
        session_id: char_id as u64,
        user_id: Some(char_id as u64),
        character_id: Some(char_id),
        corporation_id: Some(1_000_009),
        ship_id: Some(ship_id),
        station_id,
        solar_system_id: Some(sys_id),
        ..Default::default()
    }
}

fn call_ops(
    ctx: &ServerContext,
    sess: &mut SessionState,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    operations_manager::handle(ctx, sess, "operationsManager", method, args, &HashMap::new())
        .expect("operationsManager route")
}

fn call_season(
    ctx: &ServerContext,
    sess: &mut SessionState,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    season_manager::handle_season_manager(ctx, sess, "seasonManager", method, args, &HashMap::new())
        .expect("seasonManager route")
}

fn claim_quasar_milestone(
    ctx: &ServerContext,
    char_id: u32,
    milestone_id: u32,
    type_name: &str,
) -> Result<(), (u32, String)> {
    if !ctx
        .quasar_gateway
        .handlers
        .read()
        .unwrap()
        .contains_key(type_name)
    {
        register_career_goal_handlers(
            &ctx.quasar_gateway,
            Arc::clone(&ctx.db),
            Arc::clone(&ctx.quasar_gateway.progression.career_goals),
        );
    }
    let mut ms_sub = DynamicQuasarMessage::new("Id");
    ms_sub.fields.insert(
        1,
        DynamicValue::Bytes(career_milestone_uuid(milestone_id).to_vec()),
    );
    let mut ms_claim_req = DynamicQuasarMessage::new("Claim");
    ms_claim_req
        .fields
        .insert(1, DynamicValue::Message(Box::new(ms_sub)));
    let auth_ctx = AuthoritativeContext::for_session(
        Some(char_id as i32),
        Some(char_id),
        Some(1_000_009),
        0,
        "tranquility",
        vec![1, 2],
    );
    let handler = Arc::clone(&ctx.quasar_gateway.handlers.read().unwrap()[type_name]);
    handler(&auth_ctx, &GoogleAny::default(), &ms_claim_req).map(|_| ())
}

#[test]
fn test_r7_td_457_blocks_veteran_and_non_starter_npe_pocket_entry() {
    let ctx = ServerContext::new();
    let jita_sys = 30_000_142_u32;
    let null_sys = 30_002_904_u32;
    let non_starter_sys = 30_002_187_u32;

    // 1. Veteran character in Jita without pending tutorial cannot start tutorial or enter pocket
    let vet_id = 94_001_001_i64;
    let vet_ship = 240_001_001_i64;
    let mut vet_sess =
        setup_pilot_in_system(&ctx, vet_id, vet_ship, jita_sys, Some(60_003_760), false);
    assert!(call_ops(
        &ctx,
        &mut vet_sess,
        "process_client_event",
        &[PyRep::String("start_tutorial".into())],
    )
    .is_err());
    for ev in [
        "new_eden_cinematic_ended",
        "cinematic_finished",
        "enter_tutorial_pocket",
        "move_to_dungeon",
        "complete_air_npe",
    ] {
        assert!(
            call_ops(
                &ctx,
                &mut vet_sess,
                "process_client_event",
                &[PyRep::String(ev.into())],
            )
            .is_err(),
            "veteran event {ev} must be rejected"
        );
    }
    assert!(transition_character_into_npe_pocket(&ctx, Some(&mut vet_sess), vet_id).is_err());
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.characters[&vet_id].station_id, Some(60_003_760));
        assert!(!snap.item_positions.contains_key(&vet_ship));
    });

    // 2. Pending tutorial character in nullsec or non-starter highsec cannot enter pocket
    for (idx, bad_sys) in [null_sys, non_starter_sys].into_iter().enumerate() {
        let cid = 94_002_010_i64 + idx as i64;
        let sid = 240_002_010_i64 + idx as i64;
        let orig_pos = [12_345.0, 6_789.0, -4_321.0];
        let mut sess = setup_pilot_in_system(&ctx, cid, sid, bad_sys, None, true);
        ctx.db.with_mut_snapshot(|snap| {
            snap.item_positions.insert(sid, orig_pos);
        });
        assert!(call_ops(
            &ctx,
            &mut sess,
            "process_client_event",
            &[PyRep::String("start_tutorial".into())],
        )
        .is_err());
        assert!(call_ops(
            &ctx,
            &mut sess,
            "process_client_event",
            &[PyRep::String("move_to_dungeon".into())],
        )
        .is_err());
        assert!(!on_client_graph_message(
            &ctx,
            &mut sess,
            cid,
            AIR_NPE_NODE_GRAPH_ID,
            12,
            "move_to_dungeon",
            &PyRep::None,
        ));
        ctx.db.with_snapshot(|snap| {
            assert_eq!(snap.item_positions.get(&sid), Some(&orig_pos));
            assert_ne!(snap.item_positions.get(&sid), Some(&NPE_POCKET_ANCHOR_POS));
        });
    }
}

#[tokio::test]
async fn test_r7_td_457_blocks_combat_and_warp_scramble_pocket_entry_skip_and_complete() {
    let ctx = ServerContext::new();
    let jita_sys = 30_000_142_u32;
    let cid = 94_003_001_i64;
    let sid = 240_003_001_i64;
    let mut sess = setup_pilot_in_system(&ctx, cid, sid, jita_sys, Some(60_003_760), true);

    on_start_tutorial(&ctx, &mut sess, cid, AIR_NPE_STATE_ACTIVE, Some(2));
    transition_character_into_npe_pocket(&ctx, Some(&mut sess), cid).expect("enter pocket");

    // 1. Active weapons / PvP combat timer blocks skip and complete
    let now = ctx.now_unix();
    crate::sp6_lifecycle::with_mut_sp6_state(&ctx.db, |sp6, _| {
        sp6.weapon_timers_until.insert(cid, now + 60);
    });
    assert!(call_ops(
        &ctx,
        &mut sess,
        "process_client_event",
        &[PyRep::String("skip_tutorial".into())],
    )
    .is_err());
    assert!(call_ops(
        &ctx,
        &mut sess,
        "process_client_event",
        &[PyRep::String("complete_air_npe".into())],
    )
    .is_err());
    assert!(on_skip_air_npe(&ctx, Some(&mut sess), cid, now).is_err());
    assert!(complete_air_npe_checked(&ctx, Some(&mut sess), cid, now).is_err());
    ctx.db.with_snapshot(|snap| {
        assert!(snap.characters[&cid].station_id.is_none());
    });

    // Clear combat timer, apply warp scramble to ship ball in space
    crate::sp6_lifecycle::with_mut_sp6_state(&ctx.db, |sp6, _| {
        sp6.weapon_timers_until.remove(&cid);
    });
    let sys_h = ctx.systems.get_or_spawn(jita_sys);
    {
        let mut bp = sys_h.ballpark.write().unwrap_or_else(|e| e.into_inner());
        bp.combat_states.entry(sid).or_insert_with(CombatState::default).warp_scramble_strength = 3;
    }
    assert!(on_skip_air_npe(&ctx, Some(&mut sess), cid, now).is_err());
    assert!(complete_air_npe_checked(&ctx, Some(&mut sess), cid, now).is_err());
    assert!(!on_client_graph_message(
        &ctx,
        &mut sess,
        cid,
        AIR_NPE_NODE_GRAPH_ID,
        24,
        "complete_air_npe",
        &PyRep::None,
    ));

    // Remove warp scramble and verify completion docks cleanly
    {
        let mut bp = sys_h.ballpark.write().unwrap_or_else(|e| e.into_inner());
        if let Some(cs) = bp.combat_states.get_mut(&sid) {
            cs.warp_scramble_strength = 0;
        }
    }
    complete_air_npe_checked(&ctx, Some(&mut sess), cid, now).expect("complete after scramble clears");
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.characters[&cid].station_id, Some(60_003_760));
        assert!(snap.npe_progress[&cid].completed);
    });
}

#[tokio::test]
async fn test_r7_td_457_docking_updates_system_constellation_region_and_blocks_cross_universe() {
    let ctx = ServerContext::new();
    let kisogo_sys = 30_000_141_u32;
    let kisogo_station = 60_015_157_i64;
    let jita_sys = 30_000_142_u32;
    let jita_station = 60_003_760_i64;

    let cid = 94_004_001_i64;
    let sid = 240_004_001_i64;
    let mut sess =
        setup_pilot_in_system(&ctx, cid, sid, kisogo_sys, Some(kisogo_station), true);
    on_start_tutorial(&ctx, &mut sess, cid, AIR_NPE_STATE_ACTIVE, Some(2));
    transition_character_into_npe_pocket(&ctx, Some(&mut sess), cid).expect("enter pocket");

    // Set destination origin station in another starter system (Jita)
    ctx.db.with_mut_snapshot(|snap| {
        let mut r = load_runner_state(snap, cid).unwrap();
        r.origin_station_id = Some(jita_station);
        save_runner_state(snap, &r);
    });
    complete_air_npe_checked(&ctx, Some(&mut sess), cid, ctx.now_unix()).expect("complete NPE");

    let jita_meta = ctx.sde.stations.get(&jita_station).expect("Jita in SDE");
    ctx.db.with_snapshot(|snap| {
        let ch = &snap.characters[&cid];
        assert_eq!(ch.station_id, Some(jita_station));
        assert_eq!(ch.solar_system_id, jita_sys);
        assert_eq!(ch.constellation_id, jita_meta.constellation_id);
        assert_eq!(ch.region_id, jita_meta.region_id);
    });
    assert!(!ctx
        .systems
        .get_or_spawn(kisogo_sys)
        .ballpark
        .read()
        .unwrap()
        .balls
        .contains_key(&sid));

    // Malicious cross-universe home_station_id in non-starter system cannot pull pilot out of starter space
    let null_station = ctx
        .sde
        .stations
        .values()
        .find(|s| {
            !crate::services::air_npe_nodegraph_pocket::is_starter_or_school_solar_system(
                &ctx,
                s.solar_system_id,
            )
        })
        .expect("non-starter SDE station")
        .station_id;
    let cid2 = 94_004_002_i64;
    let sid2 = 240_004_002_i64;
    let mut sess2 =
        setup_pilot_in_system(&ctx, cid2, sid2, kisogo_sys, None, true);
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&cid2).unwrap().home_station_id = null_station;
    });
    on_start_tutorial(&ctx, &mut sess2, cid2, AIR_NPE_STATE_ACTIVE, Some(1));
    transition_character_into_npe_pocket(&ctx, Some(&mut sess2), cid2).expect("enter pocket");
    complete_air_npe_checked(&ctx, Some(&mut sess2), cid2, ctx.now_unix()).expect("complete NPE");
    ctx.db.with_snapshot(|snap| {
        let ch2 = &snap.characters[&cid2];
        assert_eq!(ch2.solar_system_id, kisogo_sys);
        assert_ne!(ch2.station_id, Some(null_station));
        assert_eq!(ch2.station_id, Some(kisogo_station));
    });
}

#[test]
fn test_r7_td_459_blocks_500_ore_bypass_and_syncs_dual_system_claims() {
    let ctx = ServerContext::new();
    let jita_sys = 30_000_142_u32;

    // 1. Mining 5,000 units of Veldspar without 750 Industrialist Career Points cannot claim milestone 2
    let cid1 = 94_005_001_i64;
    let sid1 = 240_005_001_i64;
    let mut sess1 =
        setup_pilot_in_system(&ctx, cid1, sid1, jita_sys, Some(60_003_760), false);
    ctx.db.with_mut_snapshot(|snap| {
        snap.mining_ledger.push(MiningLedgerRecord {
            character_id: cid1,
            solar_system_id: jita_sys,
            type_id: 1230,
            quantity: 5_000,
            date: "2026-11-06".into(),
            timestamp_unix: 1_800_000_000,
        });
    });
    let err_incomplete = call_season(&ctx, &mut sess1, "claim_goal_reward", &[PyRep::Integer(2)])
        .expect_err("500+ ore without 750 CP must not unlock Industrialist Graduation");
    assert_eq!(err_incomplete.code, "GoalProgressIncomplete");
    assert_eq!(
        ctx.db.with_snapshot(|s| s.characters[&cid1].free_skill_points),
        0.0
    );

    // Earn >= 750 Industrialist CP -> claim via seasonManager -> subsequent Quasar claim fails
    let mut cp_ind = 0u32;
    for g in ctx
        .quasar_gateway
        .progression
        .career_goals
        .get_or_init_goals(cid1 as u32)
    {
        if g.def.career_id == 2 && cp_ind < 750 {
            let _ = ctx
                .quasar_gateway
                .progression
                .career_goals
                .mark_completed(cid1 as u32, &g.uuid, 1_800_000_000);
            cp_ind += g.def.career_points;
        }
    }
    let claim_sm = call_season(&ctx, &mut sess1, "claim_goal_reward", &[PyRep::Integer(2)])
        .expect("claim milestone 2 via seasonManager with 750+ CP");
    assert_eq!(
        claim_sm.dict_get("spReward").and_then(|v| v.as_f64()),
        Some(100_000.0)
    );
    let quasar_dup = claim_quasar_milestone(
        &ctx,
        cid1 as u32,
        2,
        "eve_public.career.goal.api.ClaimRewardRequest",
    )
    .expect_err("Quasar ClaimRewardRequest must reject milestone already claimed via seasonManager");
    assert_eq!(quasar_dup.0, STATUS_FORBIDDEN);

    let reloaded = CareerGoalManager::new(Some(ctx.db.clone()));
    assert!(reloaded
        .get_character_milestones(cid1 as u32)
        .iter()
        .any(|(m, _, claimed)| m.milestone_id == 2 && *claimed));

    // 2. Claim via Quasar ClaimRewardRequest -> subsequent seasonManager.claim_goal_reward fails
    let cid2 = 94_005_002_i64;
    let sid2 = 240_005_002_i64;
    let mut sess2 =
        setup_pilot_in_system(&ctx, cid2, sid2, jita_sys, Some(60_003_760), false);
    let mut cp_exp = 0u32;
    for g in ctx
        .quasar_gateway
        .progression
        .career_goals
        .get_or_init_goals(cid2 as u32)
    {
        if g.def.career_id == 1 && cp_exp < 750 {
            let _ = ctx
                .quasar_gateway
                .progression
                .career_goals
                .mark_completed(cid2 as u32, &g.uuid, 1_800_000_000);
            cp_exp += g.def.career_points;
        }
    }
    claim_quasar_milestone(
        &ctx,
        cid2 as u32,
        1,
        "eve_public.app.eveonline.career.goal.ClaimRewardRequest",
    )
    .expect("claim Explorer Graduation via Quasar");
    assert_eq!(
        ctx.db.with_snapshot(|s| s.characters[&cid2].free_skill_points),
        100_000.0
    );
    let sm_dup = call_season(&ctx, &mut sess2, "claim_goal_reward", &[PyRep::Integer(1)])
        .expect_err("seasonManager.claim_goal_reward must reject milestone already claimed via Quasar");
    assert_eq!(sm_dup.code, "GoalAlreadyClaimed");
    assert_eq!(
        ctx.db.with_snapshot(|s| s.characters[&cid2].free_skill_points),
        100_000.0
    );
}
