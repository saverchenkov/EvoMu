//! End-to-end remediation tests for structure boarding/control (`feat:structures.board-structure`,
//! `rpc:structureControl.{BoardStructure,EjectFromStructure,GetStructurePilot,ReleaseControl,TakeControl}`)
//! and Upwell defense settings & automated Standup weapon/PDB firing (`feat:structures.defense-settings`,
//! `feat:structures.profiles`).

use eve_db::{InventoryFlag, InventoryItemRecord, UpwellStructureRecord};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use eve_server::services::station_structure::structure_lifecycle as life;
use eve_server::services::station_structure::structure_upwell_ops::{load_ext_state, save_ext_state};
use glam::DVec3;
use std::collections::HashMap;
use std::sync::Arc;

const OWNER: i64 = 90_000_001;
const DIRECTOR_2: i64 = 90_000_002;
const GRUNT: i64 = 90_000_003;
const HOSTILE: i64 = 90_000_004;
const PERIMETER: u32 = 30_000_144;
const RANCER: u32 = 30_002_718;

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None).await
}

fn kv_bool(rep: &PyRep, key: &str) -> bool {
    rep.dict_get(key).and_then(|v| v.as_bool()).unwrap_or(false)
}

fn make_structure(
    structure_id: i64,
    type_id: u32,
    owner_corp_id: i64,
    solar_system_id: u32,
    name: &str,
    pos: DVec3,
    quantum_core_installed: bool,
    fuel_blocks: u32,
) -> UpwellStructureRecord {
    UpwellStructureRecord {
        structure_id,
        type_id,
        owner_corp_id,
        solar_system_id,
        name: name.into(),
        state: life::STATE_SHIELD_VULNERABLE.into(),
        position: pos.to_array(),
        quantum_core_installed,
        fuel_blocks,
        shield_hp: 1_000_000.0,
        armor_hp: 1_000_000.0,
        hull_hp: 1_000_000.0,
        reinforced_until_unix: 0,
        last_damage_tick: 0,
        last_damage_unix: 0,
        self_destruct_at_unix: None,
    }
}

fn new_ctx() -> Arc<ServerContext> {
    let ctx = Arc::new(ServerContext::new());
    ctx.set_clock(Arc::new(eve_db::VirtualServerClock::with_fixed_base(1_800_000_000)));
    ctx
}

async fn login_with_new_corp(
    ctx: &ServerContext,
    sess_id: u64,
    sys_id: u32,
    corp_name: &str,
    ticker: &str,
) -> SessionState {
    let station_id = ctx
        .sde
        .stations
        .values()
        .find(|s| s.solar_system_id == sys_id)
        .map(|s| s.station_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&OWNER) {
            let ship_id = ch.active_ship_id;
            ch.solar_system_id = sys_id;
            ch.station_id = station_id;
            if let Some(sh) = snap.items.get_mut(&ship_id) {
                sh.location_id = station_id.unwrap_or(i64::from(sys_id));
            }
        }
    });
    let mut sess = SessionState::new(sess_id);
    rpc(ctx, &mut sess, "authentication", "Login", &[PyRep::String("demo".into()), PyRep::String("demo-secret".into())])
        .await
        .expect("Login");
    rpc(ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(OWNER)])
        .await
        .expect("SelectCharacterID");
    ctx.advance_secs(12);
    rpc(
        ctx,
        &mut sess,
        "corpRegistry",
        "AddCorporation",
        &[PyRep::String(corp_name.into()), PyRep::String(ticker.into())],
    )
    .await
    .expect("AddCorporation");
    ctx.advance_virtual_clock(15);
    if station_id.is_some() {
        rpc(ctx, &mut sess, "structureDocking", "Undock", &[]).await.expect("Undock");
    }
    sess
}

async fn move_pilot_to_system(ctx: &ServerContext, sess: &mut SessionState, sys_id: u32, pos: DVec3) {
    let ship_id = sess.ship_id.expect("ship");
    sess.solar_system_id = Some(sys_id);
    sess.station_id = None;
    sess.structure_id = None;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&OWNER) {
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
        }
        if let Some(sh) = snap.items.get_mut(&ship_id) {
            sh.location_id = i64::from(sys_id);
        }
    });
    let sys = ctx.systems.get_or_spawn(sys_id);
    let _ = sys
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: 587,
                position: pos,
                velocity: DVec3::ZERO,
                radius: 40.0,
                mass: 1.0e6,
                mode: MovementMode::Stop,
                speed_fraction: 0.0,
                max_velocity: 300.0,
                inertia: 1.0,
            },
            ctx.build_type_combat_state(587),
        )
        .await;
    let _ = sys.set_ball_position(ship_id, pos).await;
}

#[tokio::test]
async fn test_remediated_structure_control_and_boarding_lifecycle() {
    // parity: feat:structures.board-structure, rpc:structureControl.BoardStructure, rpc:structureControl.EjectFromStructure, rpc:structureControl.GetStructurePilot, rpc:structureControl.ReleaseControl, rpc:structureControl.TakeControl
    eve_testkit::attest(&[
        "feat:structures.board-structure",
        "rpc:structureControl.BoardStructure",
        "rpc:structureControl.EjectFromStructure",
        "rpc:structureControl.GetStructurePilot",
        "rpc:structureControl.ReleaseControl",
        "rpc:structureControl.TakeControl",
    ]);

    let ctx = new_ctx();
    let mut sess = login_with_new_corp(&ctx, 8103, PERIMETER, "Control Ops Corp", "CTRL").await;
    let corp_id = sess.corporation_id.expect("corp");
    let orig_ship = sess.ship_id.expect("ship");
    let struct_id = 880_810_301;
    let st_pos = DVec3::new(0.0, 5.0e9, 0.0);

    let mut dir2_sess = SessionState::new(8104);
    dir2_sess.user_id = Some(2);
    dir2_sess.character_id = Some(DIRECTOR_2);
    dir2_sess.corporation_id = Some(corp_id);
    dir2_sess.solar_system_id = Some(PERIMETER);
    dir2_sess.structure_id = Some(struct_id);
    dir2_sess.corp_role = eve_db::corp_roles::ROLE_DIRECTOR;

    let mut grunt_sess = SessionState::new(8105);
    grunt_sess.user_id = Some(3);
    grunt_sess.character_id = Some(GRUNT);
    grunt_sess.corporation_id = Some(corp_id);
    grunt_sess.solar_system_id = Some(PERIMETER);
    grunt_sess.structure_id = Some(struct_id);
    grunt_sess.corp_role = 0;

    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            struct_id,
            make_structure(struct_id, 35832, corp_id, PERIMETER, "Command Astrahus", st_pos, true, 1_000),
        );
        let mut c2 = snap.characters.get(&OWNER).cloned().unwrap();
        c2.character_id = DIRECTOR_2;
        c2.corporation_id = corp_id;
        c2.station_id = None;
        c2.structure_id = Some(struct_id);
        snap.characters.insert(DIRECTOR_2, c2);

        let mut c3 = snap.characters.get(&OWNER).cloned().unwrap();
        c3.character_id = GRUNT;
        c3.corporation_id = corp_id;
        c3.station_id = None;
        c3.structure_id = Some(struct_id);
        snap.characters.insert(GRUNT, c3);

        let corp = snap.corporations.get_mut(&corp_id).unwrap();
        corp.members.insert(DIRECTOR_2);
        corp.members.insert(GRUNT);
        corp.member_roles.insert(DIRECTOR_2, eve_db::corp_roles::ROLE_DIRECTOR);
    });
    life::sync_all_structures(&ctx);
    eve_server::services::station_structure::structure_lifecycle_rpc::ensure_structure_ball_in_space(&ctx, PERIMETER, struct_id).await;

    // 1. Undocked pilot cannot board/take control of a dockable structure.
    move_pilot_to_system(&ctx, &mut sess, PERIMETER, st_pos + DVec3::new(1_000.0, 0.0, 0.0)).await;
    assert_eq!(
        rpc(&ctx, &mut sess, "structureControl", "BoardStructure", &[PyRep::Integer(struct_id)])
            .await
            .expect_err("undocked BoardStructure")
            .code,
        "NotDockedInStructure"
    );
    assert_eq!(
        rpc(&ctx, &mut sess, "structureControl", "TakeControl", &[PyRep::Integer(struct_id)])
            .await
            .expect_err("undocked TakeControl")
            .code,
        "NotDockedInStructure"
    );

    // 2. Docked director takes control via BoardStructure; unprivileged grunt and second director are denied.
    rpc(&ctx, &mut sess, "structureDocking", "Dock", &[PyRep::Integer(struct_id)]).await.expect("Dock");
    assert_eq!(
        rpc(&ctx, &mut grunt_sess, "structureControl", "BoardStructure", &[PyRep::Integer(struct_id)])
            .await
            .expect_err("grunt without defense role")
            .code,
        "CorpRoleDenied"
    );
    let _ = sess.drain_notifications();
    let board_rep = rpc(&ctx, &mut sess, "structureControl", "BoardStructure", &[PyRep::Integer(struct_id)])
        .await
        .expect("BoardStructure");
    assert!(kv_bool(&board_rep, "boarded"));
    assert_eq!(sess.ship_id, Some(struct_id));
    assert_eq!(
        rpc(&ctx, &mut sess, "structureControl", "GetStructurePilot", &[PyRep::Integer(struct_id)]).await.unwrap(),
        PyRep::Integer(OWNER)
    );
    assert!(sess.drain_notifications().iter().any(|(ev, _)| ev == "OnSessionChanged"));
    assert_eq!(
        rpc(&ctx, &mut dir2_sess, "structureControl", "TakeControl", &[PyRep::Integer(struct_id)])
            .await
            .expect_err("already controlled")
            .code,
        "StructureAlreadyControlled"
    );

    // 3. EjectFromStructure restores original ship and allows second director to TakeControl; Undock auto-releases control.
    let eject_rep = rpc(&ctx, &mut sess, "structureControl", "EjectFromStructure", &[PyRep::Integer(struct_id)])
        .await
        .expect("EjectFromStructure");
    assert!(!kv_bool(&eject_rep, "boarded"));
    assert_eq!(sess.ship_id, Some(orig_ship));
    assert_eq!(
        rpc(&ctx, &mut sess, "structureControl", "GetStructurePilot", &[PyRep::Integer(struct_id)]).await.unwrap(),
        PyRep::None
    );

    let take2 = rpc(&ctx, &mut dir2_sess, "structureControl", "TakeControl", &[PyRep::Integer(struct_id)])
        .await
        .expect("Director 2 TakeControl");
    assert!(kv_bool(&take2, "success"));
    assert_eq!(
        rpc(&ctx, &mut sess, "structureControl", "GetStructurePilot", &[PyRep::Integer(struct_id)]).await.unwrap(),
        PyRep::Integer(DIRECTOR_2)
    );
    rpc(&ctx, &mut dir2_sess, "structureControl", "ReleaseControl", &[PyRep::Integer(struct_id)])
        .await
        .expect("ReleaseControl");
    assert_eq!(
        rpc(&ctx, &mut sess, "structureControl", "GetStructurePilot", &[PyRep::Integer(struct_id)]).await.unwrap(),
        PyRep::None
    );

    rpc(&ctx, &mut sess, "structureControl", "TakeControl", &[PyRep::Integer(struct_id)]).await.expect("TakeControl before Undock");
    // Failed Undock must NOT prematurely release structure control!
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&orig_ship).unwrap().location_id = 0;
    });
    assert_eq!(
        rpc(&ctx, &mut sess, "structureDocking", "Undock", &[]).await.expect_err("failed undock").code,
        "UndockFailed"
    );
    assert_eq!(sess.ship_id, Some(struct_id));
    assert_eq!(
        rpc(&ctx, &mut sess, "structureControl", "GetStructurePilot", &[PyRep::Integer(struct_id)]).await.unwrap(),
        PyRep::Integer(OWNER)
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&orig_ship).unwrap().location_id = struct_id;
    });
    rpc(&ctx, &mut sess, "structureDocking", "Undock", &[]).await.expect("Undock auto-releases structure control");
    assert_eq!(sess.ship_id, Some(orig_ship));
    assert_eq!(
        rpc(&ctx, &mut sess, "structureControl", "GetStructurePilot", &[PyRep::Integer(struct_id)]).await.unwrap(),
        PyRep::None
    );

    // 4. FLEX structure (Ansiblex 35841) boards in space within radius + 2,500 m without docking.
    let flex_id = 880_810_302;
    let flex_pos = DVec3::new(10_000_000.0, 5.0e9, 0.0);
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            flex_id,
            make_structure(flex_id, 35841, corp_id, PERIMETER, "Command Ansiblex", flex_pos, false, 1_000),
        );
    });
    life::sync_all_structures(&ctx);
    eve_server::services::station_structure::structure_lifecycle_rpc::ensure_structure_ball_in_space(&ctx, PERIMETER, flex_id).await;
    move_pilot_to_system(&ctx, &mut sess, PERIMETER, flex_pos + DVec3::new(50_000.0, 0.0, 0.0)).await;
    assert_eq!(
        rpc(&ctx, &mut sess, "structureControl", "BoardStructure", &[PyRep::Integer(flex_id)])
            .await
            .expect_err("out of FLEX boarding range")
            .code,
        "OutOfBoardingRange"
    );
    move_pilot_to_system(&ctx, &mut sess, PERIMETER, flex_pos + DVec3::new(1_000.0, 0.0, 0.0)).await;
    let flex_board = rpc(&ctx, &mut sess, "structureControl", "BoardStructure", &[PyRep::Integer(flex_id)])
        .await
        .expect("Board FLEX within 2,500m");
    assert!(kv_bool(&flex_board, "boarded"));
    assert_eq!(sess.ship_id, Some(flex_id));
    rpc(&ctx, &mut sess, "structureControl", "EjectFromStructure", &[PyRep::Integer(flex_id)])
        .await
        .expect("Eject from FLEX");
    assert_eq!(sess.ship_id, Some(orig_ship));
}

#[tokio::test]
async fn test_remediated_structure_defense_settings_and_auto_fire_ballpark() {
    // parity: feat:structures.defense-settings, feat:structures.profiles
    eve_testkit::attest(&["feat:structures.defense-settings", "feat:structures.profiles"]);

    let ctx = new_ctx();
    let mut sess = login_with_new_corp(&ctx, 8106, RANCER, "Defense Grid Corp", "DEFG").await;
    let corp_id = sess.corporation_id.expect("corp");
    let struct_id = 880_810_601;
    let st_pos = DVec3::new(0.0, 5.0e9, 0.0);
    let friendly_ship = 880_810_610;
    let hostile_close_ship = 880_810_611;
    let hostile_far_ship = 880_810_612;

    let prof_id = rpc(&ctx, &mut sess, "structureProfiles", "CreateProfile", &[PyRep::String("Perimeter Defense Profile".into()), PyRep::String("Auto defense".into())])
        .await
        .expect("CreateProfile")
        .as_i64()
        .unwrap();

    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            struct_id,
            make_structure(struct_id, 35832, corp_id, RANCER, "Auto-Defense Astrahus", st_pos, true, 2_000),
        );
        let hostile_2 = 90_000_077;
        let mut h_char = snap.characters.get(&OWNER).cloned().unwrap();
        h_char.character_id = HOSTILE;
        h_char.corporation_id = 98_999_666;
        h_char.station_id = None;
        h_char.solar_system_id = RANCER;
        h_char.active_ship_id = hostile_close_ship;
        snap.characters.insert(HOSTILE, h_char);

        let mut h2_char = snap.characters.get(&OWNER).cloned().unwrap();
        h2_char.character_id = hostile_2;
        h2_char.corporation_id = 98_999_667;
        h2_char.station_id = None;
        h2_char.solar_system_id = RANCER;
        h2_char.active_ship_id = hostile_far_ship;
        snap.characters.insert(hostile_2, h2_char);
        snap.corp_contacts.entry(corp_id).or_default().push(eve_db::SocialContactRecord {
            contact_id: hostile_2,
            contact_type: "character".into(),
            standing: -5.0,
            is_watched: false,
            is_blocked: false,
            label_ids: Vec::new(),
        });

        let mut g_char = snap.characters.get(&OWNER).cloned().unwrap();
        g_char.character_id = GRUNT;
        g_char.corporation_id = corp_id;
        g_char.station_id = None;
        g_char.solar_system_id = RANCER;
        g_char.active_ship_id = friendly_ship;
        snap.characters.insert(GRUNT, g_char);

        for (sid, owner_id) in [
            (friendly_ship, GRUNT),
            (hostile_close_ship, HOSTILE),
            (hostile_far_ship, hostile_2),
        ] {
            snap.items.insert(
                sid,
                InventoryItemRecord {
                    item_id: sid,
                    type_id: 587,
                    owner_id,
                    location_id: i64::from(RANCER),
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
        }
        // Fit Standup Multirole Missile Launcher I (35922) + Standup Heavy Missile (37847)
        // and Standup Point Defense Battery I (35926) + Standup Flak Round I (63195).
        let launcher_id = 880_810_620;
        let charge_id = 880_810_621;
        let pdb_id = 880_810_622;
        let pdb_charge_id = 880_810_623;
        for (iid, tid, flag, qty, singleton) in [
            (launcher_id, 35922u32, InventoryFlag::HiSlot0, 1i32, true),
            (charge_id, 37847, InventoryFlag::HiSlot0, 20, false),
            (pdb_id, 35926, InventoryFlag::HiSlot1, 1, true),
            (pdb_charge_id, 63195, InventoryFlag::HiSlot1, 20, false),
        ] {
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: tid,
                    owner_id: corp_id,
                    location_id: struct_id,
                    flag,
                    quantity: qty,
                    singleton,
                    version: 1,
                    ..Default::default()
                },
            );
        }
        snap.module_online_states.insert(launcher_id, true);
        snap.module_online_states.insert(pdb_id, true);
    });
    life::sync_all_structures(&ctx);
    eve_server::services::station_structure::structure_lifecycle_rpc::ensure_structure_ball_in_space(&ctx, RANCER, struct_id).await;

    // Non-client defense RPCs must fall through to UnimplementedRPC.
    let expected_err = "UnimplementedRPC";
    for bad_method in ["GetDefenseSettings", "SetDefenseSettings", "EvaluateDefenseTarget"] {
        assert_eq!(
            rpc(&ctx, &mut sess, "structureControl", bad_method, &[PyRep::Integer(struct_id)])
                .await
                .expect_err(bad_method)
                .code,
            expected_err
        );
    }

    // Assign profile and configure defenseSettings via V24 structureProfiles RPCs.
    rpc(&ctx, &mut sess, "structureProfiles", "ChangeProfiles", &[PyRep::List(vec![PyRep::Integer(struct_id)]), PyRep::Integer(prof_id)])
        .await
        .expect("ChangeProfiles");
    assert_eq!(
        rpc(
            &ctx,
            &mut sess,
            "structureProfiles",
            "SaveProfileSettings",
            &[
                PyRep::Integer(prof_id),
                PyRep::Dict(vec![(
                    PyRep::String("defenseSettings".into()),
                    PyRep::Dict(vec![(PyRep::String("defenseBlocked".into()), PyRep::List(vec![PyRep::Integer(999_999_999)]))]),
                )]),
            ],
        )
        .await
        .expect_err("unknown defenseBlocked entity")
        .code,
        "InvalidAccessEntity"
    );
    let save_def = |auto_atk: bool, auto_war: bool, target_acl: bool, blocked: Vec<PyRep>| {
        let def_dict = PyRep::Dict(vec![
            (PyRep::String("autoDefenseEnabled".into()), PyRep::Bool(true)),
            (PyRep::String("autoTargetAttackers".into()), PyRep::Bool(auto_atk)),
            (PyRep::String("autoTargetWarTargets".into()), PyRep::Bool(auto_war)),
            (PyRep::String("targetBlockedAcl".into()), PyRep::Bool(target_acl)),
            (PyRep::String("standingThreshold".into()), PyRep::Real(-1.0)),
            (PyRep::String("defenseBlocked".into()), PyRep::List(blocked)),
        ]);
        PyRep::Dict(vec![
            (PyRep::String("corpOnly".into()), PyRep::Bool(false)),
            (PyRep::String("defenseSettings".into()), def_dict),
        ])
    };
    rpc(&ctx, &mut sess, "structureProfiles", "SaveProfileSettings", &[PyRep::Integer(prof_id), save_def(false, false, true, vec![PyRep::Integer(HOSTILE)])])
        .await
        .expect("SaveProfileSettings");

    // Spawn friendly ship at 15 km, hostile_close at 15 km (inside PDB 25 km), and hostile_far at 60 km (outside PDB 25 km).
    let sys = ctx.systems.get_or_spawn(RANCER);
    let spawn_ship = |bid: i64, pos: DVec3| {
        let sys_c = sys.clone();
        let mut cs = ctx.build_type_combat_state(587);
        cs.shield_hp = 5_000.0;
        cs.max_shield_hp = 5_000.0;
        cs.armor_hp = 5_000.0;
        cs.max_armor_hp = 5_000.0;
        async move {
            let _ = sys_c
                .spawn_ball_with_combat(
                    Ball {
                        id: bid,
                        type_id: 587,
                        position: pos,
                        velocity: DVec3::ZERO,
                        radius: 40.0,
                        mass: 1.0e6,
                        mode: MovementMode::Stop,
                        speed_fraction: 0.0,
                        max_velocity: 300.0,
                        inertia: 1.0,
                    },
                    cs,
                )
                .await;
        }
    };
    spawn_ship(friendly_ship, st_pos + DVec3::new(15_000.0, 0.0, 0.0)).await;
    spawn_ship(hostile_close_ship, st_pos + DVec3::new(0.0, 15_000.0, 0.0)).await;
    spawn_ship(hostile_far_ship, st_pos + DVec3::new(0.0, 0.0, 60_000.0)).await;

    // 1. After 5s (< 55.39s lock time): lock is still acquiring (locked=false), directed missiles have NOT fired or consumed ammo,
    // while untargeted PDB fires AoE at 15 km and consumes Flak Rounds!
    let _ = sys.step_manual(5.0).await.expect("step_5s");
    let locks_5 = sys.get_target_locks(struct_id).await.unwrap_or_default();
    assert!(locks_5.iter().any(|l| !l.locked && l.target_id == hostile_close_ship));
    assert_eq!(ctx.db.with_snapshot(|s| s.items[&880_810_621].quantity), 20, "Missiles must not fire before target lock completes");
    assert!(ctx.db.with_snapshot(|s| s.items[&880_810_623].quantity) < 20, "PDB must consume Flak Rounds when firing");
    let friendly_cs = sys.get_combat_state(friendly_ship).await.unwrap().unwrap();
    let close_cs_5 = sys.get_combat_state(hostile_close_ship).await.unwrap().unwrap();
    let far_cs_5 = sys.get_combat_state(hostile_far_ship).await.unwrap().unwrap();
    assert!((friendly_cs.shield_hp - 5_000.0).abs() < 1e-6, "Friendly corp ship must take zero auto-defense damage");
    assert!((far_cs_5.shield_hp - 5_000.0).abs() < 1e-6, "Secondary hostile at 60 km must not be hit by 25 km PDB AoE");
    assert!(close_cs_5.shield_hp < 5_000.0, "Close hostile within 25 km takes PDB AoE damage");

    // 2. Step 56s more (>= 55.40s SDE lock time) -> lock completes and directed Standup missiles fire & consume ammo!
    let _ = sys.step_manual(56.0).await.expect("step_60s");
    let locks_60 = sys.get_target_locks(struct_id).await.unwrap_or_default();
    assert!(locks_60.iter().any(|l| l.locked && l.target_id == hostile_close_ship));
    let close_cs_60 = sys.get_combat_state(hostile_close_ship).await.unwrap().unwrap();
    assert!(close_cs_60.shield_hp < close_cs_5.shield_hp, "Directed missile launcher fires once lock completes");
    assert!(ctx.db.with_snapshot(|s| s.items[&880_810_621].quantity) < 20, "Missile charges must be consumed");

    // 3. Remove close hostile, leave only 1 missile in launcher, and verify launcher stops firing once empty!
    let _ = sys.remove_ball(hostile_close_ship).await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&880_810_621).unwrap().quantity = 1;
    });
    let _ = sys.step_manual(1.0).await.expect("step_far_lock_start");
    let _ = sys.step_manual(60.0).await.expect("step_far_lock_and_last_shot");
    let far_cs_1 = sys.get_combat_state(hostile_far_ship).await.unwrap().unwrap();
    assert!(far_cs_1.shield_hp < 5_000.0, "Far hostile at 60 km takes 1 missile shot");
    assert!(ctx.db.with_snapshot(|s| !s.items.contains_key(&880_810_621)), "Empty missile stack must be removed");
    let _ = sys.step_manual(6.0).await.expect("step_empty_launcher");
    let far_cs_2 = sys.get_combat_state(hostile_far_ship).await.unwrap().unwrap();
    assert!(far_cs_2.shield_hp >= far_cs_1.shield_hp - 1e-6, "Empty launcher must not deal phantom ammo damage");

    // 4. Reload launcher and test autoTargetAttackers (positive, negative, TTL expiry) and autoTargetWarTargets (positive, negative)!
    let _ = sys.remove_ball(hostile_far_ship).await;
    let attacker_pilot = 90_000_088;
    let war_pilot = 90_000_089;
    let attacker_ship = 880_810_613;
    let war_ship = 880_810_614;
    let war_corp = 98_999_689;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            880_810_624,
            InventoryItemRecord {
                item_id: 880_810_624,
                type_id: 37847,
                owner_id: corp_id,
                location_id: struct_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 50,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        for (cid, ccorp, shid) in [(attacker_pilot, 98_999_688, attacker_ship), (war_pilot, war_corp, war_ship)] {
            let mut ch = snap.characters.get(&OWNER).cloned().unwrap();
            ch.character_id = cid;
            ch.corporation_id = ccorp;
            ch.station_id = None;
            ch.solar_system_id = RANCER;
            ch.active_ship_id = shid;
            snap.characters.insert(cid, ch);
        }
        snap.wars.insert(
            770_810_601,
            eve_db::WarRecord {
                war_id: 770_810_601,
                aggressor_corp_id: war_corp,
                defender_corp_id: corp_id,
                mutual: false,
                started_unix: ctx.now_unix().saturating_sub(90_000),
                active: true,
            },
        );
    });
    spawn_ship(attacker_ship, st_pos + DVec3::new(80_000.0, 0.0, 0.0)).await;
    spawn_ship(war_ship, st_pos + DVec3::new(0.0, 90_000.0, 0.0)).await;

    // attacker_pilot damages the structure, recording an active attacker entry.
    let _ = eve_server::services::station_structure::structure_lifecycle_rpc::apply_live_combat_damage_to_upwell_structure(
        &ctx,
        &sys,
        RANCER,
        attacker_pilot,
        struct_id,
        eve_destiny::DamageVector::new(100.0, 0.0, 0.0, 0.0),
        1.0,
        ctx.now_unix(),
    )
    .await
    .expect("attacker hits structure");

    // 4a. Both flags OFF -> neither attacker nor war target is engaged.
    rpc(&ctx, &mut sess, "structureProfiles", "SaveProfileSettings", &[PyRep::Integer(prof_id), save_def(false, false, false, Vec::new())])
        .await
        .expect("disable attacker & war targeting");
    let _ = sys.step_manual(5.0).await.expect("step_flags_off");
    assert!(sys.get_target_locks(struct_id).await.unwrap_or_default().is_empty(), "No locks when flags are off");

    // 4b. autoTargetAttackers ON, autoTargetWarTargets OFF -> engages attacker_ship, ignores war_ship!
    rpc(&ctx, &mut sess, "structureProfiles", "SaveProfileSettings", &[PyRep::Integer(prof_id), save_def(true, false, false, Vec::new())])
        .await
        .expect("enable autoTargetAttackers");
    let _ = sys.step_manual(1.0).await.expect("step_attacker_lock_start");
    let _ = sys.step_manual(60.0).await.expect("step_attacker_on");
    assert!(sys.get_combat_state(attacker_ship).await.unwrap().unwrap().shield_hp < 5_000.0, "Attacker must be damaged when autoTargetAttackers=true");
    assert!((sys.get_combat_state(war_ship).await.unwrap().unwrap().shield_hp - 5_000.0).abs() < 1e-6, "War target must not be damaged when autoTargetWarTargets=false");

    // 4c. Advance clock past STRUCTURE_ATTACKER_TTL_SECS (900s) -> attacker expires and is no longer targeted!
    ctx.advance_virtual_clock(eve_server::services::structure_acl_defense_rules::STRUCTURE_ATTACKER_TTL_SECS + 5);
    let _ = sys.remove_target_lock(struct_id, attacker_ship).await;
    let _ = sys.step_manual(5.0).await.expect("step_attacker_expired");
    assert!(sys.get_target_locks(struct_id).await.unwrap_or_default().is_empty(), "Expired attacker must no longer be targeted");

    // 4d. autoTargetWarTargets ON, autoTargetAttackers OFF -> engages war_ship!
    rpc(&ctx, &mut sess, "structureProfiles", "SaveProfileSettings", &[PyRep::Integer(prof_id), save_def(false, true, false, Vec::new())])
        .await
        .expect("enable autoTargetWarTargets");
    let _ = sys.step_manual(1.0).await.expect("step_war_lock_start");
    let _ = sys.step_manual(60.0).await.expect("step_war_on");
    assert!(sys.get_combat_state(war_ship).await.unwrap().unwrap().shield_hp < 5_000.0, "War target must be damaged when autoTargetWarTargets=true");

    let _ = ctx.db.with_mut_snapshot(|snap| {
        let ext = load_ext_state(snap);
        save_ext_state(snap, &ext);
    });
}
