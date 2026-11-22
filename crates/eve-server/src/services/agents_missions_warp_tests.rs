use super::super::{
    deterministic_mission_site_origin, find_accepted_mission_for_warp, handle,
    parse_agent_warp_args, resolve_agent_warp_target, ActiveMissionRecord, MissionLocationRef,
    WarpTarget,
};
use super::test_ctx;
use crate::router::{BoundObjectEntry, ServerContext};
use crate::system_actor::SystemHandle;
use eve_db::{
    BookmarkRecord, FleetMemberRecord, FleetRecord, FleetRole, InventoryFlag, InventoryItemRecord,
};
use eve_destiny::{Ball, CombatState, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use glam::DVec3;
use std::collections::HashMap;

struct AgentFleetWarpFixture {
    ctx: ServerContext,
    sys_handle: SystemHandle,
    agent_id: i64,
    char_fc: i64,
    ship_fc: i64,
    char_wing: i64,
    ship_wing: i64,
    sess: SessionState,
}

async fn setup_agent_fleet_warp_fixture(seed_base: i64) -> AgentFleetWarpFixture {
    let ctx = test_ctx();
    let sys_id = 30_000_142_u32;
    let char_fc = seed_base;
    let char_wing = seed_base + 1;
    let ship_fc = seed_base * 10 + 1;
    let ship_wing = seed_base * 10 + 2;
    let fleet_id = seed_base * 100 + 7;
    let mission_id = (seed_base % 100_000) as u32 + 90_000;
    let bm_id = seed_base * 100 + 99;

    let agent_id = ctx
        .sde
        .agents
        .values()
        .find(|a| a.solar_system_id == Some(sys_id))
        .expect("Jita must have an SDE agent")
        .agent_id;
    let start_pos = DVec3::new(1_000_000.0, 0.0, 0.0);
    let pocket_pos = DVec3::new(10_000_000.0, 0.0, 0.0);

    ctx.db.with_mut_snapshot(|snap| {
        snap.active_missions.retain(|(cid, _), _| *cid != char_fc);
        let template_ch = snap.characters.values().next().unwrap().clone();
        let template_acc = snap.accounts.values().next().unwrap().clone();
        for (idx, (cid, sid)) in [(char_fc, ship_fc), (char_wing, ship_wing)]
            .into_iter()
            .enumerate()
        {
            let uid = seed_base as u64 + 500 + idx as u64;
            let mut acc = template_acc.clone();
            acc.user_id = uid;
            acc.username = format!("agent_warp_1493_{seed_base}_{idx}");
            snap.accounts.insert(acc.username.clone(), acc);
            snap.items.insert(
                sid,
                InventoryItemRecord {
                    item_id: sid,
                    type_id: 587,
                    owner_id: cid,
                    location_id: sys_id as i64,
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
            let mut ch = snap
                .characters
                .get(&cid)
                .cloned()
                .unwrap_or_else(|| template_ch.clone());
            ch.character_id = cid;
            ch.user_id = uid;
            ch.active_ship_id = sid;
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
            snap.characters.insert(cid, ch);
        }
        snap.bookmarks.insert(
            bm_id,
            BookmarkRecord {
                bookmark_id: bm_id,
                owner_id: char_fc,
                solar_system_id: sys_id,
                label: "Mission Site".into(),
                x: pocket_pos.x,
                y: pocket_pos.y,
                z: pocket_pos.z,
                item_id: None,
                memo: "Mission Pocket".into(),
                folder_id: None,
                is_shared: false,
            },
        );
        snap.active_missions.insert(
            (char_fc, mission_id),
            ActiveMissionRecord {
                character_id: char_fc,
                mission_id,
                agent_id,
                solar_system_id: sys_id,
                state: "Accepted".into(),
                current_room_id: 1,
                npcs_remaining: 0,
                spawned_npc_ball_ids: Vec::new(),
                bookmark_id: Some(bm_id),
            },
        );
        let mut member_details = HashMap::new();
        member_details.insert(
            char_fc,
            FleetMemberRecord {
                character_id: char_fc,
                role: FleetRole::FleetCommander,
                takes_fleet_warp: true,
                ..Default::default()
            },
        );
        member_details.insert(
            char_wing,
            FleetMemberRecord {
                character_id: char_wing,
                role: FleetRole::SquadMember,
                takes_fleet_warp: true,
                ..Default::default()
            },
        );
        snap.fleets.insert(
            fleet_id,
            FleetRecord {
                fleet_id,
                boss_id: char_fc,
                members: vec![char_fc, char_wing],
                member_details,
                ..Default::default()
            },
        );
    });

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    for (sid, offset) in [(ship_fc, DVec3::ZERO), (ship_wing, DVec3::new(1_000.0, 0.0, 0.0))] {
        let mut cs = CombatState::default();
        cs.capacitor = 250.0;
        cs.max_capacitor = 250.0;
        sys_handle
            .spawn_ball_with_combat(
                Ball {
                    id: sid,
                    type_id: 587,
                    position: start_pos + offset,
                    velocity: DVec3::ZERO,
                    max_velocity: 300.0,
                    inertia: 1.0,
                    mass: 1_000_000.0,
                    radius: 40.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                cs,
            )
            .await
            .unwrap();
    }

    let sess = SessionState {
        user_id: Some(seed_base as u64 + 500),
        character_id: Some(char_fc),
        corporation_id: Some(1_000_009),
        solar_system_id: Some(sys_id),
        station_id: None,
        ship_id: Some(ship_fc),
        fleet_id: Some(fleet_id),
        ..Default::default()
    };

    AgentFleetWarpFixture {
        ctx,
        sys_handle,
        agent_id,
        char_fc,
        ship_fc,
        char_wing,
        ship_wing,
        sess,
    }
}

async fn call_agent_fleet_warp(fx: &mut AgentFleetWarpFixture) -> Result<PyRep, eve_proto::BridgeRpcError> {
    let kw = HashMap::new();
    handle(
        &fx.ctx,
        &mut fx.sess,
        "agentMgr",
        "WarpToLocation",
        &[
            PyRep::String("dungeon".into()),
            PyRep::Integer(0),
            PyRep::Real(0.0),
            PyRep::Bool(true),
            PyRep::Integer(fx.agent_id),
        ],
        &kw,
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn agent_fleet_warp_rejects_docked_cloaked_scrambled_and_too_close_members() {
    // 1. Docked caller -> ShipDocked; docked wingmate is skipped cleanly while FC warps.
    {
        let mut fx = setup_agent_fleet_warp_fixture(91_001_100).await;
        fx.sess.station_id = Some(60_003_760);
        assert_eq!(call_agent_fleet_warp(&mut fx).await.unwrap_err().code, "ShipDocked");
        fx.sess.station_id = None;

        fx.ctx.db.with_mut_snapshot(|snap| {
            snap.characters.get_mut(&fx.char_wing).unwrap().station_id = Some(60_003_760);
        });
        assert_eq!(call_agent_fleet_warp(&mut fx).await.unwrap(), PyRep::Bool(true));
        let (_, fc_b) = fx.sys_handle.get_ball_snapshot(fx.ship_fc).await.unwrap().unwrap();
        let (_, wing_b) = fx.sys_handle.get_ball_snapshot(fx.ship_wing).await.unwrap().unwrap();
        assert!(matches!(fc_b.mode, MovementMode::Warp { .. }));
        assert!(matches!(wing_b.mode, MovementMode::Stop));
    }

    // 2. Non-covert cloaked wingmate (both CombatState and DB module) skipped cleanly while FC warps.
    {
        let mut fx = setup_agent_fleet_warp_fixture(91_001_200).await;
        fx.sys_handle
            .modify_combat_state(fx.ship_wing, |cs| {
                cs.is_cloaked = true;
                cs.is_covert_cloak = false;
            })
            .await
            .unwrap();
        assert_eq!(call_agent_fleet_warp(&mut fx).await.unwrap(), PyRep::Bool(true));
        assert!(matches!(
            fx.sys_handle.get_ball_snapshot(fx.ship_fc).await.unwrap().unwrap().1.mode,
            MovementMode::Warp { .. }
        ));
        assert!(matches!(
            fx.sys_handle.get_ball_snapshot(fx.ship_wing).await.unwrap().unwrap().1.mode,
            MovementMode::Stop
        ));
        assert!(fx.sys_handle.get_combat_state(fx.ship_wing).await.unwrap().unwrap().is_cloaked);
    }

    // 3. Active DB non-covert cloak module on FC + scrambled wingmate -> 0 warped -> Bool(false).
    {
        let mut fx = setup_agent_fleet_warp_fixture(91_001_300).await;
        let cloak_id = 91_880_001_i64;
        fx.ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                cloak_id,
                InventoryItemRecord {
                    item_id: cloak_id,
                    type_id: 11370,
                    owner_id: fx.char_fc,
                    location_id: fx.ship_fc,
                    flag: InventoryFlag::HiSlot0,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
            snap.module_active_states.insert(cloak_id);
        });
        fx.sys_handle.set_warp_scramble_strength(fx.ship_wing, 1).await.unwrap();

        assert_eq!(call_agent_fleet_warp(&mut fx).await.unwrap(), PyRep::Bool(false));
        assert!(matches!(
            fx.sys_handle.get_ball_snapshot(fx.ship_fc).await.unwrap().unwrap().1.mode,
            MovementMode::Stop
        ));
        assert!(matches!(
            fx.sys_handle.get_ball_snapshot(fx.ship_wing).await.unwrap().unwrap().1.mode,
            MovementMode::Stop
        ));
    }

    // 4. Too-close wingmate (< 150 km from pocket, while within 8,000 km grid of FC) skipped cleanly while FC warps.
    {
        let mut fx = setup_agent_fleet_warp_fixture(91_001_400).await;
        fx.sys_handle
            .set_ball_position(fx.ship_fc, DVec3::new(9_500_000.0, 0.0, 0.0))
            .await;
        fx.sys_handle
            .set_ball_position(fx.ship_wing, DVec3::new(9_900_000.0, 0.0, 0.0))
            .await;
        assert_eq!(call_agent_fleet_warp(&mut fx).await.unwrap(), PyRep::Bool(true));
        assert!(matches!(
            fx.sys_handle.get_ball_snapshot(fx.ship_fc).await.unwrap().unwrap().1.mode,
            MovementMode::Warp { .. }
        ));
        assert!(matches!(
            fx.sys_handle.get_ball_snapshot(fx.ship_wing).await.unwrap().unwrap().1.mode,
            MovementMode::Stop
        ));
    }

    // 5. All members inside origin warp bubble (blocked in `validate_ship_movement`) -> 0 warped -> Bool(false).
    {
        let mut fx = setup_agent_fleet_warp_fixture(91_001_500).await;
        fx.sys_handle
            .add_warp_bubble(91_770_001, DVec3::new(1_000_500.0, 0.0, 0.0), 20_000.0)
            .await
            .unwrap();
        assert_eq!(call_agent_fleet_warp(&mut fx).await.unwrap(), PyRep::Bool(false));
        assert!(matches!(
            fx.sys_handle.get_ball_snapshot(fx.ship_fc).await.unwrap().unwrap().1.mode,
            MovementMode::Stop
        ));
        assert!(matches!(
            fx.sys_handle.get_ball_snapshot(fx.ship_wing).await.unwrap().unwrap().1.mode,
            MovementMode::Stop
        ));
    }
}

#[tokio::test]
async fn warp_target_enum_replaces_loc_lt_64_heuristics_and_resolves_typed_variants() {
    let ctx = test_ctx();
    let sys_id = 30_000_142_u32;
    let char_id = 91_002_001_i64;
    let ship_id = 91_200_001_i64;
    let mission_id = 42_u32; // Intentionally < 64 to prove MissionOrAgentId no longer collides with ordinal < 64 guesses.
    let agent = ctx
        .sde
        .agents
        .values()
        .find(|a| a.solar_system_id == Some(sys_id) && a.station_id.is_some())
        .expect("Jita must have stationed SDE agent");
    let st_id = agent.station_id.expect("stationed agent");
    let kw = HashMap::new();

    // Seed 66 mission pocket bookmarks so ordinal index 65 (>= 64!) is valid while 66 is out-of-range.
    ctx.db.with_mut_snapshot(|snap| {
        snap.active_missions.retain(|(cid, _), _| *cid != char_id);
        let template_ch = snap.characters.values().next().unwrap().clone();
        let mut ch = template_ch;
        ch.character_id = char_id;
        ch.active_ship_id = ship_id;
        ch.solar_system_id = sys_id;
        ch.station_id = None;
        ch.structure_id = None;
        snap.characters.insert(char_id, ch);

        for idx in 0_i64..66_i64 {
            let bmid = 990_000 + idx;
            snap.bookmarks.insert(
                bmid,
                BookmarkRecord {
                    bookmark_id: bmid,
                    owner_id: char_id,
                    solar_system_id: sys_id,
                    label: format!("Pocket #{idx}"),
                    x: 20_000_000.0 + (idx as f64) * 50_000.0,
                    y: 0.0,
                    z: 0.0,
                    item_id: None,
                    memo: "Mission Pocket".into(),
                    folder_id: None,
                    is_shared: false,
                },
            );
        }
        snap.active_missions.insert(
            (char_id, mission_id),
            ActiveMissionRecord {
                character_id: char_id,
                mission_id,
                agent_id: agent.agent_id,
                solar_system_id: sys_id,
                state: "Accepted".into(),
                current_room_id: 1,
                npcs_remaining: 0,
                spawned_npc_ball_ids: Vec::new(),
                bookmark_id: Some(990_000),
            },
        );
    });

    // 1. Journal ordinal >= 64 (`65`) resolves cleanly, while out-of-range ordinal (`66`) returns None.
    let sel_65 = find_accepted_mission_for_warp(&ctx, char_id, None, Some(65), Some(agent.agent_id), &kw)
        .expect("ordinal 65 must resolve when mission has 66 bookmarks");
    assert_eq!(sel_65.matched_by, MissionLocationRef::JournalOrdinal { index: 65 });
    assert_eq!(sel_65.bookmark_id, Some(990_065));
    assert!(find_accepted_mission_for_warp(&ctx, char_id, None, Some(66), Some(agent.agent_id), &kw).is_none());

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let sess = SessionState {
        user_id: Some(91_002_001),
        character_id: Some(char_id),
        corporation_id: Some(1_000_009),
        solar_system_id: Some(sys_id),
        station_id: None,
        ship_id: Some(ship_id),
        ..Default::default()
    };

    // Resolve `WarpTarget::Dungeon` for ordinal 65.
    let dungeon_args = [
        PyRep::String("dungeon".into()),
        PyRep::Integer(65),
        PyRep::Real(0.0),
        PyRep::Bool(false),
        PyRep::Integer(agent.agent_id),
    ];
    let parsed_dungeon = parse_agent_warp_args("WarpToLocation", &dungeon_args, &kw).unwrap();
    let target_dungeon =
        resolve_agent_warp_target(&ctx, &sess, &sys_handle, char_id, sys_id, None, "WarpToLocation", &parsed_dungeon, &kw)
            .await
            .unwrap();
    assert_eq!(
        target_dungeon,
        WarpTarget::Dungeon {
            mission_id,
            bookmark_id: Some(990_065),
            solar_system_id: sys_id,
            anchor_pos: DVec3::new(20_000_000.0 + 65.0 * 50_000.0, 0.0, 0.0),
        }
    );

    // 2. When mission only has 1 bookmark (`bms.len() == 1`), selector `42` (< 64!) does NOT pretend to be ordinal 42;
    //    instead it matches `MissionLocationRef::MissionOrAgentId { id: 42 }` cleanly!
    ctx.db.with_mut_snapshot(|snap| {
        snap.bookmarks.retain(|bid, _| *bid == 990_000);
    });
    let sel_mid = find_accepted_mission_for_warp(&ctx, char_id, None, Some(42), None, &kw)
        .expect("mission_id=42 (<64) must match by MissionOrAgentId when bms.len()==1");
    assert_eq!(sel_mid.matched_by, MissionLocationRef::MissionOrAgentId { id: 42 });

    // And bare numeric WarpToLocation(42) resolves `WarpTarget::Dungeon` for mission 42.
    let bare_mid_args = [PyRep::Integer(42)];
    let parsed_bare = parse_agent_warp_args("WarpToLocation", &bare_mid_args, &kw).unwrap();
    let target_bare =
        resolve_agent_warp_target(&ctx, &sess, &sys_handle, char_id, sys_id, None, "WarpToLocation", &parsed_bare, &kw)
            .await
            .unwrap();
    assert!(matches!(target_bare, WarpTarget::Dungeon { mission_id: 42, .. }));

    // Deterministic dungeon fallback still places anchor in Jita when bookmark is absent.
    let fallback_origin = deterministic_mission_site_origin(&ctx, sys_id, mission_id);
    assert!(fallback_origin.length() > 150_000.0);

    // 3. Explicit bookmark (`WarpTarget::Bookmark`), station (`WarpTarget::StationOrCelestial`), and agent-in-space (`WarpTarget::AgentInSpace`).
    let custom_bm_id = 990_777_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.bookmarks.insert(
            custom_bm_id,
            BookmarkRecord {
                bookmark_id: custom_bm_id,
                owner_id: char_id,
                solar_system_id: sys_id,
                label: "Safe Spot".into(),
                x: 88_000_000.0,
                y: 12_000_000.0,
                z: -4_000_000.0,
                item_id: None,
                memo: "Safe".into(),
                folder_id: None,
                is_shared: false,
            },
        );
    });
    let bm_args = [PyRep::String("bookmark".into()), PyRep::Integer(custom_bm_id)];
    let parsed_bm = parse_agent_warp_args("WarpToLocation", &bm_args, &kw).unwrap();
    let target_bm =
        resolve_agent_warp_target(&ctx, &sess, &sys_handle, char_id, sys_id, None, "WarpToLocation", &parsed_bm, &kw)
            .await
            .unwrap();
    assert_eq!(
        target_bm,
        WarpTarget::Bookmark {
            bookmark_id: custom_bm_id,
            solar_system_id: sys_id,
            anchor_pos: DVec3::new(88_000_000.0, 12_000_000.0, -4_000_000.0),
        }
    );
    assert_eq!(target_bm.solar_system_id(), sys_id);

    let st_args = [PyRep::String("agenthomebase".into()), PyRep::Integer(agent.agent_id)];
    let parsed_st = parse_agent_warp_args("WarpToLocation", &st_args, &kw).unwrap();
    let target_st =
        resolve_agent_warp_target(&ctx, &sess, &sys_handle, char_id, sys_id, None, "WarpToLocation", &parsed_st, &kw)
            .await
            .unwrap();
    assert!(matches!(
        target_st,
        WarpTarget::StationOrCelestial { location_id, solar_system_id, .. } if location_id == st_id && solar_system_id == sys_id
    ));
    let st_land = target_st.landing_position(&ctx, DVec3::ZERO);
    assert!(st_land.is_finite());

    if let Some(space_ag) = ctx
        .sde
        .agents
        .values()
        .filter(|a| a.station_id.is_none() && a.solar_system_id.is_some())
        .min_by_key(|a| a.agent_id)
    {
        let ag_sys = space_ag.solar_system_id.unwrap();
        let ag_sess = SessionState {
            user_id: Some(91_002_001),
            character_id: Some(char_id),
            solar_system_id: Some(ag_sys),
            ship_id: Some(ship_id),
            ..Default::default()
        };
        let ag_handle = ctx.systems.get_or_spawn(ag_sys);
        let ag_pos = DVec3::new(42_000_000.0, 1_000_000.0, 0.0);
        let _ = ag_handle
            .spawn_ball(Ball {
                id: space_ag.agent_id,
                type_id: 24,
                position: ag_pos,
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 200.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            })
            .await;
        let bound = BoundObjectEntry {
            bound_id: 1,
            session_id: 1,
            user_id: 91_002_001,
            character_id: char_id,
            service: "agentMgr".into(),
            object_id: space_ag.agent_id,
            oid_str: format!("N=1:{}", space_ag.agent_id),
            ref_id: 1,
        };
        let ag_args = [PyRep::String("agent".into())];
        let parsed_ag = parse_agent_warp_args("WarpToLocation", &ag_args, &kw).unwrap();
        let target_ag = resolve_agent_warp_target(
            &ctx,
            &ag_sess,
            &ag_handle,
            char_id,
            ag_sys,
            Some(&bound),
            "WarpToLocation",
            &parsed_ag,
            &kw,
        )
        .await
        .unwrap();
        assert_eq!(
            target_ag,
            WarpTarget::AgentInSpace {
                agent_id: space_ag.agent_id,
                solar_system_id: ag_sys,
                anchor_pos: ag_pos,
            }
        );

        // Cross-system query resolves `WarpTarget::SpacePoint` with agent's system ID so handler emits CrossSystemWarp.
        let other_sys = if ag_sys == 30_000_142 { 30_000_144 } else { 30_000_142 };
        let other_handle = ctx.systems.get_or_spawn(other_sys);
        let target_cross = resolve_agent_warp_target(
            &ctx,
            &ag_sess,
            &other_handle,
            char_id,
            other_sys,
            Some(&bound),
            "WarpToLocation",
            &parsed_ag,
            &kw,
        )
        .await
        .unwrap();
        assert!(matches!(
            target_cross,
            WarpTarget::SpacePoint { solar_system_id, .. } if solar_system_id == ag_sys
        ));
    }
}
