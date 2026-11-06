//! Deep branch-covering & mutation-killing tests for `space_warp_jump`
//! and `character_skills::character_gm_tools`.

#![forbid(unsafe_code)]

use super::character_skills::character_gm_tools::{
    bloodline_name_pools, handle_char_unbound, handle_gm_qa_rpcs, handle_gm_slash_ext,
    is_valid_character_name_syntax,
};
use super::space_warp_jump::{
    break_location_change_invulnerability, deactivate_ship_modules_on_leave_space,
    grant_location_change_invulnerability, handle as handle_warp_jump,
    is_location_change_invulnerable, SOLAR_SYSTEM_ZARZAKH,
};
use crate::router::{BoundObjectEntry, ServerContext};
use eve_db::{
    BookmarkRecord, InventoryFlag, InventoryItemRecord, UpwellStructureRecord,
    VirtualServerClock, WorldRepository,
};
use eve_destiny::{Ball, CombatState, MovementMode, SlimItemMeta};
use eve_machonet::{service_roles, SessionState};
use eve_proto::PyRep;
use eve_sde::{InvType, SdeRegistry};
use glam::DVec3;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

#[allow(clippy::too_many_arguments)]
fn custom_type(
    sde: &SdeRegistry,
    type_id: u32,
    group_id: u32,
    category_id: u32,
    volume: f64,
    capacity: f64,
    name: &str,
    attrs: &[(u16, f64)],
) -> InvType {
    let mut t = sde.types.get(&587).expect("Rifter seed").clone();
    t.type_id = type_id;
    t.group_id = group_id;
    t.category_id = category_id;
    t.volume = volume;
    t.capacity = capacity;
    t.name = name.into();
    t.default_attributes.clear();
    for &(k, v) in attrs {
        t.default_attributes.insert(k, v);
    }
    t
}

fn make_ctx() -> ServerContext {
    let mut sde = (*shared_sde()).clone();
    let specs: &[(u32, u32, u32, f64, f64, &str, &[(u16, f64)])] = &[
        (996_001, 25, 6, 2_500.0, 200.0, "Warp Frigate", &[(600, 5.0), (153, 5e-5), (482, 500.0), (4, 1_000_000.0)]),
        (996_002, 513, 6, 500_000.0, 200_000.0, "Obelisk Freighter", &[(600, 1.5), (153, 2e-5), (482, 2_000.0)]),
        (996_003, 941, 6, 250_000.0, 40_000.0, "Orca", &[(600, 2.0), (153, 3e-5), (482, 2_000.0)]),
        (996_004, 485, 6, 500_000.0, 10_000.0, "Naglfar Capital", &[(600, 1.5), (153, 2e-5), (482, 5_000.0)]),
        (996_005, 193, 7, 5.0, 0.0, "Covert Ops Cloaking Device II", &[(1034, 1.0)]),
        (996_006, 330, 7, 5.0, 0.0, "Improved Cloaking Device II", &[]),
        (996_007, 46, 7, 5.0, 0.0, "1MN Afterburner II", &[(12, 1.0)]),
        (996_008, 60, 7, 5.0, 0.0, "Damage Control II", &[(11, 1.0)]),
        (996_009, 773, 7, 5.0, 0.0, "Small Trimark Armor Pump I", &[(2663, 1.0)]),
        (996_010, 954, 7, 5.0, 0.0, "Tengu Offensive", &[(3772, 1.0)]),
        (996_011, 100, 18, 5.0, 0.0, "Hobgoblin II", &[]),
    ];
    for &(tid, gid, cat, vol, cap, name, attrs) in specs {
        sde.types.insert(tid, custom_type(&sde, tid, gid, cat, vol, cap, name, attrs));
    }
    let db = Arc::new(WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(Arc::new(sde), db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_800_000_000)));
    ctx
}

#[allow(clippy::too_many_arguments)]
fn put_item(
    ctx: &ServerContext,
    item_id: i64,
    type_id: u32,
    owner_id: i64,
    location_id: i64,
    flag: InventoryFlag,
    quantity: i32,
    singleton: bool,
) {
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            item_id,
            InventoryItemRecord {
                item_id,
                type_id,
                owner_id,
                location_id,
                flag,
                quantity,
                singleton,
                version: 1,
                ..Default::default()
            },
        );
    });
}

#[allow(clippy::too_many_arguments)]
async fn spawn_ship(
    ctx: &ServerContext,
    sys_id: u32,
    ball_id: i64,
    type_id: u32,
    group_id: u32,
    owner_id: i64,
    pos: DVec3,
    cap: f64,
    scrambled: bool,
    cloaked: bool,
) {
    let sys = ctx.systems.get_or_spawn(sys_id);
    let _ = sys.remove_ball(ball_id).await;
    let _ = sys
        .spawn_ball_with_meta(
            Ball {
                id: ball_id,
                type_id,
                position: pos,
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState {
                shield_hp: 1_000.0,
                max_shield_hp: 1_000.0,
                armor_hp: 1_000.0,
                max_armor_hp: 1_000.0,
                structure_hp: 1_000.0,
                max_structure_hp: 1_000.0,
                capacitor: cap,
                max_capacitor: 1_000.0,
                warp_scramble_strength: if scrambled { 2 } else { 0 },
                is_cloaked: cloaked,
                ..Default::default()
            },
            SlimItemMeta {
                group_id,
                category_id: 6,
                owner_id,
                char_id: Some(owner_id),
                corp_id: 98_000_001,
                ..Default::default()
            },
        )
        .await;
}

#[tokio::test]
async fn space_warp_to_stuff_branches_invuln_and_cap_scaling() {
    let ctx = make_ctx();
    let sys_id = 30_000_142u32;
    let c1 = 90_000_001i64;
    let ship_id = 1_600_001i64;
    let kw = HashMap::new();

    let mut bad_bound = BoundObjectEntry {
        bound_id: 1,
        session_id: 1,
        user_id: 1,
        character_id: c1,
        service: "otherSvc".into(),
        object_id: sys_id as i64,
        oid_str: sys_id.to_string(),
        ref_id: 1,
    };
    let mut sess = SessionState {
        user_id: Some(1),
        character_id: Some(c1),
        corporation_id: Some(98_000_001),
        station_id: Some(60_003_760),
        solar_system_id: None,
        ship_id: Some(ship_id),
        ..Default::default()
    };
    assert_eq!(handle_warp_jump(&ctx, &mut sess, None, "beyonce", "CmdWarpToStuff", &[], &kw).await.unwrap().unwrap_err().code, "NotInSpace");
    sess.station_id = None;
    assert_eq!(handle_warp_jump(&ctx, &mut sess, None, "beyonce", "CmdWarpToStuff", &[], &kw).await.unwrap().unwrap_err().code, "BoundObjectRequired");
    assert_eq!(handle_warp_jump(&ctx, &mut sess, Some(&bad_bound), "beyonce", "CmdWarpToStuff", &[], &kw).await.unwrap().unwrap_err().code, "InvalidBoundService");
    bad_bound.service = "beyonce".into();
    sess.solar_system_id = Some(sys_id);

    sess.ship_id = None;
    assert_eq!(handle_warp_jump(&ctx, &mut sess, Some(&bad_bound), "beyonce", "CmdWarpToStuff", &[], &kw).await.unwrap().unwrap_err().code, "NoActiveShip");
    sess.ship_id = Some(ship_id);

    put_item(&ctx, ship_id, 996_001, c1, sys_id as i64, InventoryFlag::None, 1, true);
    assert_eq!(handle_warp_jump(&ctx, &mut sess, Some(&bad_bound), "beyonce", "CmdWarpToStuff", &[], &kw).await.unwrap().unwrap_err().code, "InvalidWarpDestination");
    assert_eq!(handle_warp_jump(&ctx, &mut sess, Some(&bad_bound), "beyonce", "CmdWarpToStuff", &[PyRep::String("item".into()), PyRep::Integer(ship_id)], &kw).await.unwrap().unwrap_err().code, "InvalidTarget");
    assert_eq!(handle_warp_jump(&ctx, &mut sess, Some(&bad_bound), "beyonce", "CmdWarpToStuff", &[PyRep::String("item".into()), PyRep::Integer(99_999), PyRep::Real(f64::NAN)], &kw).await.unwrap().unwrap_err().code, "InvalidArgument");

    assert_eq!(handle_warp_jump(&ctx, &mut sess, Some(&bad_bound), "beyonce", "CmdWarpToStuff", &[PyRep::String("item".into()), PyRep::Integer(60_003_760)], &kw).await.unwrap().unwrap_err().code, "ShipNotInSpace");

    spawn_ship(&ctx, sys_id, ship_id, 996_001, 25, c1, DVec3::ZERO, 500.0, true, false).await;
    let sys = ctx.systems.get_or_spawn(sys_id);
    ctx.db.with_mut_snapshot(|snap| {
        snap.bookmarks.insert(902, BookmarkRecord { bookmark_id: 902, owner_id: c1, label: "ZZ".into(), solar_system_id: SOLAR_SYSTEM_ZARZAKH, x: 1e8, y: 0.0, z: 0.0, item_id: Some(60_003_760), memo: String::new(), folder_id: None, is_shared: false });
    });
    let kind_bm = PyRep::String(String::from("bookmark"));
    assert_eq!(handle_warp_jump(&ctx, &mut sess, Some(&bad_bound), "beyonce", "CmdWarpToStuff", &[kind_bm.clone(), PyRep::Integer(999)], &kw).await.unwrap().unwrap_err().code, "BookmarkNotFound");
    spawn_ship(&ctx, SOLAR_SYSTEM_ZARZAKH, ship_id, 996_001, 25, c1, DVec3::ZERO, 500.0, false, false).await;
    let mut zz_bound = bad_bound.clone();
    zz_bound.object_id = SOLAR_SYSTEM_ZARZAKH as i64;
    zz_bound.oid_str = SOLAR_SYSTEM_ZARZAKH.to_string();
    assert_eq!(handle_warp_jump(&ctx, &mut sess, Some(&zz_bound), "beyonce", "CmdWarpToStuff", &[PyRep::String("bookmark".into()), PyRep::Integer(902)], &kw).await.unwrap().unwrap_err().code, "DunBlacklistCannotWarp");
    assert_eq!(handle_warp_jump(&ctx, &mut sess, Some(&bad_bound), "beyonce", "CmdWarpToStuff", &[PyRep::String("item".into()), PyRep::Integer(60_003_760)], &kw).await.unwrap().unwrap_err().code, "WarpScrambled");

    let _ = sys.set_warp_scramble_strength(ship_id, 0).await;
    zz_bound.object_id = 32_000_001;
    zz_bound.oid_str = "32000001".into();
    assert_eq!(handle_warp_jump(&ctx, &mut sess, Some(&zz_bound), "beyonce", "CmdWarpToStuff", &[PyRep::String("item".into()), PyRep::Integer(60_003_760)], &kw).await.unwrap().unwrap_err().code, "CannotWarpInAbyssalSpace");

    assert_eq!(handle_warp_jump(&ctx, &mut sess, Some(&bad_bound), "beyonce", "CmdWarpToStuff", &[], &kw).await.unwrap().unwrap_err().code, "InvalidWarpDestination");

    ctx.db.with_mut_snapshot(|snap| {
        snap.bookmarks.insert(903, BookmarkRecord { bookmark_id: 903, owner_id: c1, label: "Near".into(), solar_system_id: bad_bound.object_id as u32, x: 50_000.0, y: 0.0, z: 0.0, item_id: Some(60_003_760), memo: String::new(), folder_id: None, is_shared: false });
        snap.bookmarks.insert(904, BookmarkRecord { bookmark_id: 904, owner_id: c1, label: "Far".into(), solar_system_id: bad_bound.object_id as u32, x: 1e15, y: 0.0, z: 0.0, item_id: None, memo: String::new(), folder_id: None, is_shared: false });
    });
    assert_eq!(handle_warp_jump(&ctx, &mut sess, Some(&bad_bound), "beyonce", "CmdWarpToStuff", &[kind_bm.clone(), PyRep::Integer(903)], &kw).await.unwrap().unwrap_err().code, "WarpDistanceTooShort");

    {
        let mut bp = sys.ballpark.write().unwrap();
        if let Some(b) = bp.balls.get_mut(&ship_id) { b.mass = 1_000_000.0; }
        if let Some(cs) = bp.combat_states.get_mut(&ship_id) { cs.capacitor = 5.0; }
    }
    assert!(handle_warp_jump(&ctx, &mut sess, Some(&bad_bound), "beyonce", "CmdWarpToStuff", &[PyRep::String("bookmark".into()), PyRep::Integer(904)], &kw).await.unwrap().is_ok());

    {
        let mut bp = sys.ballpark.write().unwrap();
        if let Some(b) = bp.balls.get_mut(&ship_id) { b.mode = eve_destiny::MovementMode::Stop; }
        if let Some(cs) = bp.combat_states.get_mut(&ship_id) { cs.capacitor = 0.0; }
    }
    assert_eq!(handle_warp_jump(&ctx, &mut sess, Some(&bad_bound), "beyonce", "CmdWarpToStuff", &[PyRep::String("bookmark".into()), PyRep::Integer(904)], &kw).await.unwrap().unwrap_err().code, "NotEnoughCapacitor");

    {
        let mut bp = sys.ballpark.write().unwrap();
        if let Some(cs) = bp.combat_states.get_mut(&ship_id) { cs.capacitor = 500.0; }
    }
    assert!(handle_warp_jump(&ctx, &mut sess, Some(&bad_bound), "beyonce", "CmdWarpToStuffAutopilot", &[PyRep::Integer(60_003_760)], &kw).await.unwrap().is_ok());

    grant_location_change_invulnerability(&ctx, c1, 1_800_000_060);
    assert!(is_location_change_invulnerable(&ctx, c1, 1_800_000_000));
    break_location_change_invulnerability(&ctx, c1);
    assert!(!is_location_change_invulnerable(&ctx, c1, 1_800_000_000));
}

#[tokio::test]
async fn space_stargate_jump_pochven_zarzakh_dock_and_undock() {
    let ctx = make_ctx();
    let c1 = 90_000_001i64;
    let kw = HashMap::new();
    let mut sess = SessionState {
        user_id: Some(1),
        character_id: Some(c1),
        corporation_id: Some(98_000_001),
        ..Default::default()
    };

    let (jita_sys, dest_sys, gate_id, gate_pos) = ctx
        .sde
        .stargates()
        .values()
        .next()
        .map(|g| (g.from_solar_system_id, g.to_solar_system_id, g.stargate_id, g.position.to_array()))
        .expect("seed stargate");
    let freighter_id = 1_700_001i64;
    put_item(&ctx, freighter_id, 996_002, c1, jita_sys as i64, InventoryFlag::None, 1, true);
    sess.solar_system_id = Some(jita_sys);
    sess.ship_id = Some(freighter_id);

    assert_eq!(handle_warp_jump(&ctx, &mut sess, None, "beyonce", "CmdStargateJump", &[], &kw).await.unwrap().unwrap_err().code, "InvalidArgument");
    assert_eq!(handle_warp_jump(&ctx, &mut sess, None, "beyonce", "CmdStargateJump", &[PyRep::Integer(99_999_999), PyRep::Integer(dest_sys as i64)], &kw).await.unwrap().unwrap_err().code, "UnknownStargate");
    spawn_ship(&ctx, jita_sys, freighter_id, 996_002, 513, c1, DVec3::new(gate_pos[0] + 50_000.0, gate_pos[1], gate_pos[2]), 500.0, false, false).await;
    assert_eq!(handle_warp_jump(&ctx, &mut sess, None, "beyonce", "CmdStargateJump", &[PyRep::Integer(gate_id), PyRep::Integer(dest_sys as i64)], &kw).await.unwrap().unwrap_err().code, "OutsideStargateJumpRange");

    spawn_ship(&ctx, jita_sys, freighter_id, 996_002, 513, c1, DVec3::from_array(gate_pos), 500.0, false, false).await;
    assert!(handle_warp_jump(&ctx, &mut sess, None, "beyonce", "CmdStargateJump", &[PyRep::Integer(gate_id), PyRep::Integer(dest_sys as i64)], &kw).await.unwrap().is_ok());
    assert_eq!(sess.solar_system_id, Some(dest_sys));
    ctx.clock.read().unwrap().advance_secs(15);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&c1) {
            ch.standings.insert(500_026, 7.5);
        }
    });

    let struct_id = 1_700_050i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(struct_id, UpwellStructureRecord {
            structure_id: struct_id,
            solar_system_id: dest_sys,
            type_id: 35832,
            owner_corp_id: 98_000_001,
            name: "Astrahus".into(),
            state: "Destroyed".into(),
            position: [1_000_000.0, 0.0, 0.0],
            ..Default::default()
        });
    });
    assert_eq!(handle_warp_jump(&ctx, &mut sess, None, "beyonce", "CmdDock", &[PyRep::Integer(struct_id)], &kw).await.unwrap().unwrap_err().code, "StructureDestroyed");
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.get_mut(&struct_id).unwrap().state = "AnchorVulnerable".into();
    });
    assert_eq!(handle_warp_jump(&ctx, &mut sess, None, "beyonce", "CmdDock", &[PyRep::Integer(struct_id)], &kw).await.unwrap().unwrap_err().code, "StructureNotDockable");
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.get_mut(&struct_id).unwrap().state = "ShieldVulnerable".into();
    });
    assert_eq!(handle_warp_jump(&ctx, &mut sess, None, "beyonce", "CmdDock", &[PyRep::Integer(struct_id)], &kw).await.unwrap().unwrap_err().code, "StructureDockingAccessDenied");
    ctx.db.with_mut_snapshot(|snap| {
        let corp_id = snap.characters[&c1].corporation_id;
        snap.structures.get_mut(&struct_id).unwrap().owner_corp_id = corp_id;
    });
    assert_eq!(handle_warp_jump(&ctx, &mut sess, None, "beyonce", "CmdDock", &[PyRep::Integer(struct_id)], &kw).await.unwrap().unwrap_err().code, "OutsideDockingPerimeter");

    spawn_ship(&ctx, dest_sys, freighter_id, 996_002, 513, c1, DVec3::new(1_000_000.0, 0.0, 0.0), 500.0, false, false).await;
    assert!(handle_warp_jump(&ctx, &mut sess, None, "beyonce", "CmdDock", &[PyRep::Integer(struct_id)], &kw).await.unwrap().is_ok());
    assert_eq!(sess.structure_id, Some(struct_id));

    assert_eq!(handle_warp_jump(&ctx, &mut sess, None, "beyonce", "Undock", &[PyRep::Integer(struct_id)], &kw).await.unwrap().unwrap_err().code, "SessionChangeInProgress");
    ctx.clock.read().unwrap().advance_secs(15);
    assert!(handle_warp_jump(&ctx, &mut sess, None, "beyonce", "Undock", &[PyRep::Integer(struct_id)], &kw).await.unwrap().is_ok());
    deactivate_ship_modules_on_leave_space(&ctx, c1, freighter_id);
}

#[tokio::test]
async fn character_name_syntax_bloodlines_and_char_unbound_mgr() {
    assert!(!is_valid_character_name_syntax("Al"));
    assert!(!is_valid_character_name_syntax(&"A".repeat(38)));
    assert!(!is_valid_character_name_syntax(" CCP Test"));
    assert!(!is_valid_character_name_syntax("John  Doe"));
    assert!(!is_valid_character_name_syntax("CCP Helper"));
    assert!(is_valid_character_name_syntax("Selene O'Neill-Stark"));

    for bl in [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 99] {
        let (first, last) = bloodline_name_pools(bl);
        assert!(!first.is_empty() && !last.is_empty());
    }

    let ctx = make_ctx();
    let kw = HashMap::new();
    let mut sess = SessionState { user_id: Some(1), ..Default::default() };

    let list_rep = handle_char_unbound(&ctx, &mut sess, "charUnboundMgr", "GetCharactersToSelect", &[], &kw).unwrap().unwrap();
    assert!(!matches!(list_rep, PyRep::None));
    assert_eq!(handle_char_unbound(&ctx, &mut sess, "charUnboundMgr", "ValidateNameEx", &[PyRep::String("Valid Pilot 99".into())], &kw).unwrap().unwrap(), PyRep::Bool(true));
    assert_eq!(handle_char_unbound(&ctx, &mut sess, "charUnboundMgr", "ValidateNameEx", &[PyRep::String("x".into())], &kw).unwrap().unwrap(), PyRep::Bool(false));
    assert!(handle_char_unbound(&ctx, &mut sess, "charUnboundMgr", "GetValidRandomName", &[PyRep::Integer(1)], &kw).unwrap().is_ok());
    assert_eq!(handle_char_unbound(&ctx, &mut sess, "charUnboundMgr", "GetValidRandomName", &[PyRep::Integer(999_999)], &kw).unwrap().unwrap_err().code, "InvalidBloodline");

    let doll = PyRep::Dict(vec![(PyRep::String("pose".into()), PyRep::Integer(1))]);
    assert_eq!(handle_char_unbound(&ctx, &mut sess, "charUnboundMgr", "CreateCharacterWithDoll", &[], &kw).unwrap().unwrap_err().code, "InvalidCharacterName");
    assert_eq!(handle_char_unbound(&ctx, &mut sess, "charUnboundMgr", "CreateCharacterWithDoll", &[PyRep::String("Valid Pilot Name".into())], &kw).unwrap().unwrap_err().code, "InvalidArgument");
    assert_eq!(handle_char_unbound(&ctx, &mut sess, "charUnboundMgr", "CreateCharacterWithDoll", &[
        PyRep::String("New Pilot Z".into()), PyRep::Integer(1), PyRep::Integer(1), PyRep::Integer(5), PyRep::Integer(1),
        PyRep::Dict(vec![]), PyRep::Dict(vec![]), PyRep::Integer(11),
    ], &kw).unwrap().unwrap_err().code, "InvalidDollInfo");
    assert_eq!(handle_char_unbound(&ctx, &mut sess, "charUnboundMgr", "CreateCharacterWithDoll", &[
        PyRep::String("New Pilot Z".into()), PyRep::Integer(1), PyRep::Integer(1), PyRep::Integer(5), PyRep::Integer(1),
        doll.clone(), doll.clone(), PyRep::Integer(11),
    ], &kw).unwrap().unwrap_err().code, "InvalidGender");
    assert_eq!(handle_char_unbound(&ctx, &mut sess, "charUnboundMgr", "CreateCharacterWithDoll", &[
        PyRep::String("New Pilot Z".into()), PyRep::Integer(1), PyRep::Integer(999_999), PyRep::Integer(1), PyRep::Integer(1),
        doll.clone(), doll.clone(), PyRep::Integer(11),
    ], &kw).unwrap().unwrap_err().code, "CharacterCreationFailed");

    let anc = ctx.sde.ancestries.values().next().expect("seed ancestry");
    let bl_race = ctx.sde.bloodlines[&anc.bloodline_id].race_id;
    let school_id = ctx.sde.schools.values().find(|s| s.race_id == bl_race).expect("seed school").school_id;
    let new_cid = handle_char_unbound(&ctx, &mut sess, "charUnboundMgr", "CreateCharacterWithDoll", &[
        PyRep::String("New Pilot Z".into()), PyRep::Integer(1), PyRep::Integer(anc.bloodline_id as i64), PyRep::Integer(1), PyRep::Integer(anc.ancestry_id as i64),
        doll.clone(), doll, PyRep::Integer(school_id as i64), PyRep::None, PyRep::Integer(30_000_142),
    ], &kw).unwrap().unwrap().as_i64().unwrap();
    assert!(new_cid > 0);

    assert!(handle_char_unbound(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(new_cid)], &kw).unwrap().is_ok());
    assert_eq!(sess.character_id, Some(new_cid));
}

#[tokio::test]
async fn character_gm_slash_commands_and_qa_tools_deep() {
    let ctx = make_ctx();
    let sys_id = 30_000_142u32;
    let c1 = 90_000_001i64;
    let ship_id = 1_800_001i64;
    put_item(&ctx, ship_id, 996_001, c1, sys_id as i64, InventoryFlag::None, 1, true);
    ctx.db.with_mut_snapshot(|snap| snap.characters.get_mut(&c1).unwrap().active_ship_id = ship_id);
    spawn_ship(&ctx, sys_id, ship_id, 996_001, 25, c1, DVec3::ZERO, 50.0, false, false).await;

    let mut sess = SessionState {
        user_id: Some(1),
        character_id: Some(c1),
        corporation_id: Some(98_000_001),
        solar_system_id: Some(sys_id),
        ship_id: Some(ship_id),
        ..Default::default()
    };

    assert_eq!(handle_gm_qa_rpcs(&ctx, &mut sess, "publicQaToolsServer", "SlashCmd", &[PyRep::String("/heal".into())]).await.unwrap().unwrap_err().code, "InsufficientRole");
    assert_eq!(handle_gm_qa_rpcs(&ctx, &mut sess, "publicQaToolsServer", "CanGiveItemForMultifit", &[]).await.unwrap().unwrap(), PyRep::Bool(false));

    sess.role |= service_roles::ROLE_GML | service_roles::ROLE_ADMIN | service_roles::ROLE_QA | service_roles::ROLE_PROGRAMMER;
    assert_eq!(handle_gm_qa_rpcs(&ctx, &mut sess, "publicQaToolsServer", "CanGiveItemForMultifit", &[]).await.unwrap().unwrap(), PyRep::Bool(true));

    assert!(handle_gm_slash_ext(&ctx, &mut sess, c1, "/heal").await.unwrap().is_ok());
    let spawn_rep = handle_gm_slash_ext(&ctx, &mut sess, c1, "/spawn 996001 1").await.unwrap().unwrap();
    let spawned_id = spawn_rep.dict_get("spawnedID").or_else(|| spawn_rep.dict_get("itemID")).and_then(|v| v.as_i64()).unwrap_or(0);
    if spawned_id > 0 {
        let _ = handle_gm_slash_ext(&ctx, &mut sess, c1, &format!("/kill {spawned_id}")).await;
    }

    assert!(handle_gm_slash_ext(&ctx, &mut sess, c1, "/setattr me 37 555.0").await.unwrap().is_ok());
    assert!(handle_gm_slash_ext(&ctx, &mut sess, c1, "/getattr me 37").await.unwrap().is_ok());
    for tid in [996_007u32, 996_008, 996_009, 996_010, 996_011] {
        assert!(handle_gm_slash_ext(&ctx, &mut sess, c1, &format!("/fit me {tid}")).await.unwrap().is_ok());
    }
    assert!(handle_gm_slash_ext(&ctx, &mut sess, c1, "/location").await.unwrap().is_ok());
    assert!(handle_gm_slash_ext(&ctx, &mut sess, c1, "/online").await.unwrap().is_ok());
    assert!(handle_gm_slash_ext(&ctx, &mut sess, c1, "/tidi 0.5").await.unwrap().is_ok());
    assert!(handle_gm_slash_ext(&ctx, &mut sess, c1, "/reinforce").await.unwrap().is_ok());
    assert!(handle_gm_slash_ext(&ctx, &mut sess, c1, "/autotidi 250").await.unwrap().is_ok());
    assert!(handle_gm_slash_ext(&ctx, &mut sess, c1, "/energy 100").await.unwrap().is_ok());
    assert!(handle_gm_slash_ext(&ctx, &mut sess, c1, "/secstatus 2.5").await.unwrap().is_ok());
}
