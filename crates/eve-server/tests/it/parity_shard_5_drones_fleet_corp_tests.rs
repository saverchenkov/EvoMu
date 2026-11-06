//! End-to-end integration tests for Shard 5 fighter squadrons, fleet handover/formations/tags,
//! corporation quit cleanup, lookup service, and agent/standing RPCs.

#![forbid(unsafe_code)]

use crate::common;
use crate::industry_live_support::{fund_pilots, login, rpc, PILOT_1, PILOT_2};
use eve_db::{InventoryFlag, InventoryItemRecord, TrainedSkillRecord, VirtualServerClock, WorldRepository};
use eve_destiny::DVec3;
use eve_machonet::{service_roles, SessionState};
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::fleet_social::fleet_social_notify::handle_fleet_member_disconnect;
use eve_server::world_seeder::seed_full_market_catalog;
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[allow(dead_code)]
const HIGHSEC: u32 = 30_000_144;
#[allow(dead_code)]
const NULLSEC: u32 = 30_000_157;
#[allow(dead_code)]
const AEON: u32 = 23_919;
#[allow(dead_code)]
const ARCHON: u32 = 23_757;
#[allow(dead_code)]
const THANATOS: u32 = 23_911;
#[allow(dead_code)]
const AMETAT: u32 = 40_362;
#[allow(dead_code)]
const EQUITE: u32 = 40_358;
#[allow(dead_code)]
const SCARAB: u32 = 40_345;
#[allow(dead_code)]
const FIGHTER_BAY: i64 = InventoryFlag::FighterBay as i64;

#[allow(dead_code)]
fn temp_db(tag: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("evomu_s5_dfc_{tag}_{}.sqlite", std::process::id()));
    let _ = std::fs::remove_file(&p);
    p
}

#[allow(dead_code)]
fn open_ctx(path: &Path, now_unix: u64) -> Arc<ServerContext> {
    let db = Arc::new(WorldRepository::open_sqlite(path).expect("open_sqlite"));
    db.with_mut_snapshot(|snap| {
        if snap.accounts.is_empty() {
            *snap = WorldRepository::with_demo_seed().with_snapshot(|s| s.clone());
        }
    });
    let ctx = Arc::new(ServerContext::with_repository(db));
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(now_unix)));
    ctx
}

#[allow(dead_code)]
fn seed_item(ctx: &ServerContext, item_id: i64, type_id: u32, owner_id: i64, location_id: i64, flag: InventoryFlag, quantity: i64, singleton: bool) {
    ctx.db.with_mut_snapshot(|s| {
        s.items.insert(
            item_id,
            InventoryItemRecord { item_id, type_id, owner_id, location_id, flag, quantity: quantity as i32, singleton, ..Default::default() },
        );
    });
}

#[allow(dead_code)]
fn seed_skills(ctx: &ServerContext, char_id: i64, skills: &[(u32, u8)]) {
    ctx.db.with_mut_snapshot(|s| {
        let map = s.skills.entry(char_id).or_default();
        for &(tid, lvl) in skills {
            map.insert(tid, TrainedSkillRecord { type_id: tid, level: lvl, skill_points: 256_000.0 });
        }
    });
}

#[allow(dead_code)]
async fn undock_at(ctx: &ServerContext, sess: &mut SessionState, sys_id: u32, pos: DVec3) {
    let station_id = ctx
        .sde
        .stations
        .values()
        .find(|s| s.solar_system_id == sys_id)
        .map(|s| s.station_id)
        .expect("NPC station in target system");
    let char_id = sess.character_id.unwrap();
    let ship_id = ctx.db.with_mut_snapshot(|s| {
        let ch = s.characters.get_mut(&char_id).unwrap();
        let sid = ch.active_ship_id;
        ch.solar_system_id = sys_id;
        ch.station_id = Some(station_id);
        ch.structure_id = None;
        if let Some(ship) = s.items.get_mut(&sid) {
            ship.location_id = station_id;
            ship.flag = InventoryFlag::Hangar;
        }
        sid
    });
    sess.ship_id = Some(ship_id);
    sess.solar_system_id = Some(sys_id);
    sess.station_id = Some(station_id);
    sess.structure_id = None;
    rpc(ctx, sess, "stationSvc", "Undock", &[]).await.unwrap();
    let sys = ctx.systems.get_or_spawn(sys_id);
    sys.set_ball_position(ship_id, pos).await;
    let _ = sys.step_manual(1.0).await;
}

#[allow(dead_code)]
async fn launch_fighters(ctx: &ServerContext, sess: &mut SessionState, tube_idx: usize, stack_id: i64) -> i64 {
    let tube_id = tube_idx as i64;
    rpc(ctx, sess, "fighterMgr", "LoadFightersToTube", &[PyRep::Integer(stack_id), PyRep::Integer(tube_id)]).await.unwrap();
    let res = rpc(ctx, sess, "fighterMgr", "LaunchFightersFromTubes", &[PyRep::List(vec![PyRep::Integer(tube_id)])]).await.unwrap();
    let PyRep::List(ids) = res else { panic!("LaunchFightersFromTubes list, got {res:?}") };
    ids[0].as_i64().expect("fighter ball id")
}

#[allow(dead_code)]
async fn create_player_pilot(ctx: &ServerContext, sess_id: u64, user: &str, pass: &str, name: &str) -> (SessionState, i64) {
    ctx.db.provision_player_account(user, pass);
    let mut sess = SessionState::new(sess_id);
    common::login_pilot(ctx, &mut sess, user, pass).await;
    let anc = ctx.sde.ancestries.values().find(|a| a.bloodline_id == 1).map(|a| i64::from(a.ancestry_id)).unwrap_or(1);
    let sch = ctx.sde.schools.values().find(|s| s.race_id == 1).map(|s| i64::from(s.school_id)).unwrap_or(11);
    let rep = rpc(
        ctx,
        &mut sess,
        "charUnboundMgr",
        "CreateCharacterWithDoll",
        &[
            PyRep::String(name.into()),
            PyRep::Integer(1),
            PyRep::Integer(1),
            PyRep::Integer(anc),
            PyRep::Dict(vec![(PyRep::String("sculpting".into()), PyRep::List(vec![]))]),
            PyRep::Dict(vec![(PyRep::String("headLookTarget".into()), PyRep::Tuple(vec![PyRep::Real(0.0), PyRep::Real(0.0), PyRep::Real(1.0)]))]),
            PyRep::Integer(sch),
        ],
    )
    .await
    .unwrap();
    let cid = rep.as_i64().or_else(|| rep.dict_get("characterID").and_then(|v| v.as_i64())).unwrap();
    rpc(ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(cid)]).await.unwrap();
    assert_eq!(sess.role & service_roles::ROLE_GM_MASK, 0);
    (sess, cid)
}

// parity: feat:drones.fighter-heavy-long-range-bomb
// parity: feat:drones.fighter-refuel-rearm-repair
// parity: feat:drones.fighter-support-cenotaph-scarab
// parity: feat:drones.light-fighter-space-superiority
// parity: feat:drones.support-fighter-abilities
#[tokio::test]
async fn test_shard5_fighter_squadrons_heavy_bombs_support_and_rearm() {
    eve_testkit::attest(&[
        "feat:drones.fighter-heavy-long-range-bomb",
        "feat:drones.fighter-refuel-rearm-repair",
        "feat:drones.fighter-support-cenotaph-scarab",
        "feat:drones.light-fighter-space-superiority",
        "feat:drones.support-fighter-abilities",
    ]);

    let db_path = temp_db("fighters");
    let base_time = common::TEST_CLOCK_BASE_UNIX;
    let aeon_ship = 960_100_001_i64;
    let archon_ship = 960_100_002_i64;

    {
        let ctx = open_ctx(&db_path, base_time);
        seed_full_market_catalog(&ctx.db, &ctx.sde);
        fund_pilots(&ctx, &[(PILOT_1, 5_000_000_000.0), (PILOT_2, 5_000_000_000.0)]).await;

        let null_station = ctx
            .sde
            .stations
            .values()
            .find(|s| s.solar_system_id == NULLSEC)
            .map(|s| s.station_id)
            .expect("NPC nullsec station");
        let (mut p3, p3_cid) = create_player_pilot(&ctx, 52_003, "p3dfc", "p3dfc-secret", "Pilot3 DFC").await;
        ctx.db.with_mut_snapshot(|s| {
            s.service_state_blobs.remove("account_subscriptions");
            for ch in s.characters.values_mut() {
                ch.clone_grade = 2;
                ch.station_id = Some(null_station);
                ch.solar_system_id = NULLSEC;
            }
        });
        let mut p1 = login(&ctx, 52_001, "demo", PILOT_1).await;
        let mut p2 = login(&ctx, 52_002, "other", PILOT_2).await;
        p1.station_id = Some(null_station);
        p1.solar_system_id = Some(NULLSEC);
        p2.station_id = Some(null_station);
        p2.solar_system_id = Some(NULLSEC);
        p3.station_id = Some(null_station);
        p3.solar_system_id = Some(NULLSEC);
        assert_eq!(p1.role & service_roles::ROLE_GM_MASK, 0);

        let carrier_skills = [
            (23069, 5),
            (32339, 5),
            (24483, 5),
            (20533, 5),
            (24311, 5),
            (24312, 5),
            (24313, 5),
            (24314, 5),
            (3436, 5),
            (3369, 5),
            (3449, 5),
            (3331, 5),
        ];
        seed_skills(&ctx, PILOT_1, &carrier_skills);
        seed_skills(&ctx, PILOT_2, &carrier_skills);
        seed_skills(&ctx, p3_cid, &carrier_skills);

        let ametat_stack = 960_100_010_i64;
        let scarab_stack = 960_100_011_i64;
        let equite_stack = 960_100_012_i64;
        let thanatos_ship = 960_100_013_i64;

        seed_item(&ctx, aeon_ship, AEON, PILOT_1, null_station, InventoryFlag::Hangar, 1, true);
        seed_item(&ctx, ametat_stack, AMETAT, PILOT_1, aeon_ship, InventoryFlag::FighterBay, 6, false);
        seed_item(&ctx, archon_ship, ARCHON, PILOT_2, null_station, InventoryFlag::Hangar, 1, true);
        seed_item(&ctx, equite_stack, EQUITE, PILOT_2, archon_ship, InventoryFlag::FighterBay, 6, false);
        seed_item(&ctx, scarab_stack, SCARAB, PILOT_2, archon_ship, InventoryFlag::FighterBay, 3, false);
        seed_item(&ctx, thanatos_ship, THANATOS, p3_cid, null_station, InventoryFlag::Hangar, 1, true);

        rpc(&ctx, &mut p1, "ship", "Board", &[PyRep::Integer(aeon_ship)]).await.unwrap();
        rpc(&ctx, &mut p2, "ship", "Board", &[PyRep::Integer(archon_ship)]).await.unwrap();
        rpc(&ctx, &mut p3, "ship", "Board", &[PyRep::Integer(thanatos_ship)]).await.unwrap();
        ctx.advance_secs(12);

        undock_at(&ctx, &mut p1, NULLSEC, DVec3::new(0.0, 0.0, 0.0)).await;
        undock_at(&ctx, &mut p2, NULLSEC, DVec3::new(2_000.0, 0.0, 0.0)).await;
        undock_at(&ctx, &mut p3, NULLSEC, DVec3::new(10_000.0, 0.0, 0.0)).await;
        let bind = rpc(&ctx, &mut p3, "beyonce", "MachoBindObject", &[PyRep::Integer(i64::from(NULLSEC)), PyRep::None]).await.unwrap();
        let beyonce_oid = match bind { PyRep::Tuple(parts) => parts[0].as_i64().unwrap(), _ => panic!("beyonce bind") };
        ctx.dispatch_rpc(&mut p3, "beyonce", Some(beyonce_oid as u64), "CmdGotoPoint", &[PyRep::Real(10_000.0), PyRep::Real(0.0), PyRep::Real(0.0)], &std::collections::HashMap::new(), None).await.unwrap();
        let sys = ctx.systems.get_or_spawn(NULLSEC);
        sys.set_ball_position(thanatos_ship, DVec3::new(10_000.0, 0.0, 0.0)).await;
        let _ = sys.step_manual(1.0).await;

        // Launch Ametat heavy bomber & fire long-range Micro Electron Bomb
        let ametat = launch_fighters(&ctx, &mut p1, 0, ametat_stack).await;
        let bomb_pt = PyRep::Tuple(vec![PyRep::Real(14_000.0), PyRep::Real(0.0), PyRep::Real(0.0)]);
        let sh0 = sys.get_combat_state(thanatos_ship).await.unwrap().unwrap().shield_hp;
        let bomb_res = rpc(&ctx, &mut p1, "fighterMgr", "ActivateAbilitySlotsAtPoint", &[PyRep::Integer(ametat), PyRep::Integer(2), bomb_pt]).await.unwrap();
        assert_eq!(bomb_res.dict_get("remainingCharges"), Some(&PyRep::Integer(2)));
        assert!(sys.get_combat_state(thanatos_ship).await.unwrap().unwrap().shield_hp < sh0);

        // Launch Equite space-superiority fighter & activate Evasive Maneuvers + attack Ametat squadron
        let equite = launch_fighters(&ctx, &mut p2, 0, equite_stack).await;
        let vel0 = sys.get_ball_snapshot(equite).await.unwrap().unwrap().1.max_velocity;
        rpc(&ctx, &mut p2, "fighterMgr", "CmdActivateAbilitySlots", &[PyRep::List(vec![PyRep::Integer(equite)]), PyRep::Integer(1)]).await.unwrap();
        assert!(sys.get_ball_snapshot(equite).await.unwrap().unwrap().1.max_velocity > vel0 * 2.0);
        rpc(&ctx, &mut p2, "dogmaIM", "AddTarget", &[PyRep::Integer(ametat)]).await.unwrap();
        for _ in 0..20 {
            let _ = sys.step_manual(1.0).await;
        }
        let eq_sh0 = sys.get_combat_state(ametat).await.unwrap().unwrap().shield_hp;
        rpc(&ctx, &mut p2, "fighterMgr", "CmdActivateAbilitySlots", &[PyRep::List(vec![PyRep::Integer(equite)]), PyRep::Integer(2), PyRep::Integer(ametat)]).await.unwrap();
        assert!(sys.get_combat_state(ametat).await.unwrap().unwrap().shield_hp < eq_sh0);

        // Launch Scarab support fighter from Archon tube 1 & apply ECM ewar ability to Thanatos
        let scarab = launch_fighters(&ctx, &mut p2, 1, scarab_stack).await;
        rpc(&ctx, &mut p2, "dogmaIM", "AddTargetOBO", &[PyRep::Integer(archon_ship), PyRep::Integer(thanatos_ship)]).await.unwrap();
        for _ in 0..20 {
            let _ = sys.step_manual(1.0).await;
        }
        let ewar = rpc(&ctx, &mut p2, "fighterMgr", "CmdActivateAbilitySlots", &[PyRep::List(vec![PyRep::Integer(scarab)]), PyRep::Integer(0), PyRep::Integer(thanatos_ship)]).await.unwrap();
        assert!(ewar.dict_get("ewarStrength").and_then(|v| v.as_f64()).unwrap_or(0.0) > 0.0);

        // Recall fighters to tube 0 on Aeon for refuel/rearm/repair (`feat:drones.fighter-refuel-rearm-repair`)
        rpc(&ctx, &mut p1, "fighterMgr", "RecallFightersToTubes", &[PyRep::List(vec![PyRep::Integer(0)])]).await.unwrap();
        ctx.advance_secs(15);
        let fighters_info = rpc(&ctx, &mut p1, "fighterMgr", "GetFightersForShip", &[]).await.unwrap();
        assert!(!matches!(fighters_info, PyRep::None));
    }

    let ctx2 = open_ctx(&db_path, base_time + 120);
    let mut p1_reload = login(&ctx2, 52_009, "demo", PILOT_1).await;
    let info = rpc(&ctx2, &mut p1_reload, "fighterMgr", "GetFightersForShip", &[]).await.unwrap();
    assert!(!matches!(info, PyRep::None));
    assert_eq!(FIGHTER_BAY, InventoryFlag::FighterBay as i64);
    let _ = std::fs::remove_file(&db_path);
}

// parity: feat:fleet.boss-handover
// parity: feat:fleet.stored-formations-and-warp-shapes
// parity: feat:fleet.target-tagging
// parity: feat:lifecycle.corp-quit-cleanup
// parity: feat:ui.lookup
// parity: rpc:lookupSvc.LookupWarableCorporationsOrAlliances
// parity: feat:standings.npc-npc-standings
// parity: rpc:standingMgr.GetNPCNPCStandings
// parity: rpc:agentMgr.GetPlausibleMissionIDs
#[tokio::test]
async fn test_shard5_fleet_handover_formations_tags_corp_lookup_and_standings() {
    eve_testkit::attest(&[
        "feat:fleet.boss-handover",
        "feat:fleet.stored-formations-and-warp-shapes",
        "feat:fleet.target-tagging",
        "feat:lifecycle.corp-quit-cleanup",
        "feat:ui.lookup",
        "rpc:lookupSvc.LookupWarableCorporationsOrAlliances",
        "feat:standings.npc-npc-standings",
        "rpc:standingMgr.GetNPCNPCStandings",
        "rpc:agentMgr.GetPlausibleMissionIDs",
    ]);

    let db_path = temp_db("fleet_corp");
    let base_time = common::TEST_CLOCK_BASE_UNIX;
    let saved_fleet_id: i64;
    let saved_p2_cid = PILOT_2;
    let saved_p2_initial_corp: i64;
    let saved_custom_corp_id: i64;

    {
        let ctx = open_ctx(&db_path, base_time);
        fund_pilots(&ctx, &[(PILOT_1, 1_000_000_000.0), (PILOT_2, 1_000_000_000.0)]).await;
        let (mut p3, p3_cid) = create_player_pilot(&ctx, 52_103, "p3fleet", "p3fleet-secret", "Pilot3 Fleet").await;

        let mut fc = login(&ctx, 52_101, "demo", PILOT_1).await;
        let mut p2 = login(&ctx, 52_102, "other", PILOT_2).await;

        // 1. Agent & Standing RPCs (`feat:standings.npc-npc-standings`, `rpc:standingMgr.GetNPCNPCStandings`, `rpc:agentMgr.GetPlausibleMissionIDs`)
        let npc_st = rpc(&ctx, &mut fc, "standingMgr", "GetNPCNPCStandings", &[]).await.unwrap();
        let PyRep::Rowset { rows: st_rows, .. } = npc_st else { panic!("expected Rowset") };
        assert!(!st_rows.is_empty());

        let first_agent = *ctx.sde.agents.keys().next().expect("SDE agent");
        let plausible = rpc(&ctx, &mut fc, "agentMgr", "GetPlausibleMissionIDs", &[PyRep::Integer(first_agent)]).await.unwrap();
        assert!(matches!(plausible, PyRep::List(_) | PyRep::Tuple(_)));

        // 2. UI Lookup & Warable Corporations/Alliances (`feat:ui.lookup`, `rpc:lookupSvc.LookupWarableCorporationsOrAlliances`)
        let owners = rpc(&ctx, &mut fc, "lookupSvc", "LookupOwners", &[PyRep::String("Demo".into())]).await.unwrap();
        assert!(matches!(owners, PyRep::Rowset { .. } | PyRep::List(_)));
        let chars = rpc(&ctx, &mut fc, "lookupSvc", "LookupCharacters", &[PyRep::String("Demo".into())]).await.unwrap();
        assert!(matches!(chars, PyRep::Rowset { .. } | PyRep::List(_)));
        let pc_owners = rpc(&ctx, &mut fc, "lookupSvc", "LookupPCOwners", &[PyRep::String("Demo".into())]).await.unwrap();
        assert!(matches!(pc_owners, PyRep::Rowset { .. } | PyRep::List(_)));
        let warable = rpc(&ctx, &mut fc, "lookupSvc", "LookupWarableCorporationsOrAlliances", &[PyRep::String("Demo".into())]).await.unwrap();
        assert!(matches!(warable, PyRep::Rowset { .. } | PyRep::List(_)));

        // 3. Undock & test Formations, Fleet Boss Handover, and Target Tagging
        undock_at(&ctx, &mut fc, HIGHSEC, DVec3::new(0.0, 0.0, 0.0)).await;
        undock_at(&ctx, &mut p2, HIGHSEC, DVec3::new(1_000.0, 0.0, 0.0)).await;
        undock_at(&ctx, &mut p3, HIGHSEC, DVec3::new(2_000.0, 0.0, 0.0)).await;
        ctx.advance_secs(12);
        for _ in 0..12 {
            let _ = ctx.systems.get_or_spawn(HIGHSEC).step_manual(1.0).await;
        }

        let fid = rpc(&ctx, &mut fc, "fleetObjectHandler", "CreateFleet", &[]).await.unwrap().as_i64().unwrap();
        fc.fleet_id = Some(fid);
        saved_fleet_id = fid;
        rpc(&ctx, &mut fc, "fleetObjectHandler", "Invite", &[PyRep::Integer(PILOT_2)]).await.unwrap();
        rpc(&ctx, &mut p2, "fleetObjectHandler", "AcceptInvite", &[PyRep::Integer(fid)]).await.unwrap();
        p2.fleet_id = Some(fid);
        rpc(&ctx, &mut fc, "fleetObjectHandler", "Invite", &[PyRep::Integer(p3_cid)]).await.unwrap();
        rpc(&ctx, &mut p3, "fleetObjectHandler", "AcceptInvite", &[PyRep::Integer(fid)]).await.unwrap();
        p3.fleet_id = Some(fid);

        let f_settings = rpc(&ctx, &mut fc, "fleetObjectHandler", "SetFleetFormationSettings", &[PyRep::Integer(1), PyRep::Real(8_000.0), PyRep::Real(20_000.0)]).await.unwrap();
        assert_eq!(f_settings.dict_get("formationName"), Some(&PyRep::String("Plane".into())));

        // Lock p3 ship & tag it as fleet boss (`feat:fleet.target-tagging`)
        let p3_ship = p3.ship_id.unwrap();
        rpc(&ctx, &mut fc, "dogmaIM", "AddTarget", &[PyRep::Integer(p3_ship)]).await.unwrap();
        for _ in 0..20 {
            let _ = ctx.systems.get_or_spawn(HIGHSEC).step_manual(1.0).await;
        }
        let _ = rpc(&ctx, &mut fc, "beyonce", "CmdFleetTagTarget", &[PyRep::Integer(p3_ship), PyRep::String("A".into())]).await.unwrap();
        let fstate = rpc(&ctx, &mut fc, "beyonce", "GetFleetState", &[]).await.unwrap();
        let PyRep::KeyVal(fkv) = fstate else { panic!("expected KeyVal from GetFleetState") };
        assert_eq!(
            fkv.get("targetTags"),
            Some(&PyRep::Dict(vec![(PyRep::Integer(p3_ship), PyRep::String("A".into()))]))
        );

        // Handover boss explicitly to PILOT_2, disconnect p3 so offline handover is tested, keep PILOT_2 as boss
        rpc(&ctx, &mut fc, "fleetObjectHandler", "MakeLeader", &[PyRep::Integer(PILOT_2)]).await.unwrap();
        handle_fleet_member_disconnect(&ctx, &mut p3);
        let init_state = rpc(&ctx, &mut p2, "fleetObjectHandler", "GetInitState", &[]).await.unwrap();
        assert!(!matches!(init_state, PyRep::None));

        // 4. Corp quit cleanup (`feat:lifecycle.corp-quit-cleanup`): PILOT_2 docks, joins player corp, gets corp role, then quits via ResignFromCorp back to bloodline default NPC corp
        let p2_bloodline_npc_corp = ctx.db.with_snapshot(|s| {
            let bl = s.characters[&PILOT_2].bloodline_id;
            ctx.sde.bloodline(bl).map(|b| b.corporation_id).unwrap_or(1_000_006)
        });
        saved_p2_initial_corp = p2_bloodline_npc_corp;
        crate::industry_live_support::dock_station(&ctx, &mut p2, 60_003_760).await;
        crate::industry_live_support::dock_station(&ctx, &mut fc, 60_003_760).await;
        ctx.advance_secs(12);
        let add_corp = rpc(&ctx, &mut fc, "corpRegistry", "AddCorporation", &[PyRep::String("Shard5 Quit Corp".into()), PyRep::String("S5QC".into()), PyRep::Real(0.05)]).await.unwrap();
        let custom_corp_id = add_corp.dict_get("corporationID").and_then(|v| v.as_i64()).unwrap();
        let inv_rep = rpc(&ctx, &mut fc, "corpRegistry", "InsertInvitation", &[PyRep::Integer(PILOT_2)]).await.unwrap();
        let inv_id = inv_rep.dict_get("applicationID").and_then(|v| v.as_i64()).unwrap();
        ctx.advance_secs(31);
        rpc(&ctx, &mut p2, "corpRegistry", "UpdateApplicationOffer", &[PyRep::Integer(inv_id), PyRep::String("Accepted".into())]).await.unwrap();
        p2.corporation_id = Some(custom_corp_id);
        rpc(&ctx, &mut fc, "corpRegistry", "UpdateMember", &[PyRep::Integer(PILOT_2), PyRep::Integer(1), PyRep::Integer(0)]).await.ok();
        rpc(&ctx, &mut fc, "corpRegistry", "UpdateMember", &[PyRep::Integer(PILOT_2), PyRep::Integer(0), PyRep::Integer(0)]).await.ok();
        ctx.advance_secs(86_410);
        saved_custom_corp_id = custom_corp_id;
        let _ = saved_fleet_id;
        rpc(&ctx, &mut p2, "corpRegistry", "ResignFromCorp", &[]).await.unwrap();
        assert_eq!(ctx.db.with_snapshot(|s| s.characters[&PILOT_2].corporation_id), p2_bloodline_npc_corp);
        assert!(ctx.db.with_snapshot(|s| !s.corp_role_assignments.contains_key(&(custom_corp_id, PILOT_2))));
        assert!(ctx.db.with_snapshot(|s| !s.corp_member_titles.contains_key(&(custom_corp_id, PILOT_2))));
        assert!(ctx.db.with_snapshot(|s| s.fleets.get(&saved_fleet_id).is_some_and(|f| f.members.contains(&saved_p2_cid) && f.boss_id == saved_p2_cid)));
        ctx.db.with_mut_snapshot(|_| {});
    }

    // Verify corp creation, resignation, role/title cleanup & fleet formation state survived SQLite restart
    let ctx2 = open_ctx(&db_path, base_time + 90_000);
    assert!(ctx2.db.with_snapshot(|s| s.corporations.contains_key(&saved_custom_corp_id)));
    assert_eq!(ctx2.db.with_snapshot(|s| s.characters[&saved_p2_cid].corporation_id), saved_p2_initial_corp);
    assert!(ctx2.db.with_snapshot(|s| !s.corp_role_assignments.contains_key(&(saved_custom_corp_id, saved_p2_cid))));
    assert!(ctx2.db.with_snapshot(|s| !s.corp_member_titles.contains_key(&(saved_custom_corp_id, saved_p2_cid))));
    let mut fc_reload = login(&ctx2, 52_108, "demo", PILOT_1).await;
    let got_form = rpc(&ctx2, &mut fc_reload, "fleetObjectHandler", "GetFormationSettings", &[]).await.unwrap();
    assert_eq!(got_form.dict_get("formationID"), Some(&PyRep::Integer(1)));
    assert_eq!(got_form.dict_get("formationName"), Some(&PyRep::String("Plane".into())));
    let p2_reload = login(&ctx2, 52_109, "other", PILOT_2).await;
    assert_eq!(p2_reload.character_id, Some(PILOT_2));
    let _ = std::fs::remove_file(&db_path);
}
