//! Unit tests for `structure_control_ops` (`TakeControl`, `BoardStructure`, `ReleaseControl`, `EjectFromStructure`, `GetStructurePilot`, undock release, controller revocation, and auto-defense ticks).

#![forbid(unsafe_code)]

use super::*;
use crate::router::ServerContext;
use crate::services::station_structure::structure_upwell_ops::{
    load_ext_state, save_ext_state, StructureLifecycleState,
};
use crate::services::structure_acl_defense_rules::{save_defense_settings, StructureDefenseSettings};
use eve_db::{CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents, UpwellStructureRecord};
use eve_destiny::{Ball, CombatState, MovementMode, SlimItemMeta};
use eve_machonet::{service_roles, SessionState};
use eve_proto::PyRep;
use eve_sde::{InvType, SdeRegistry};
use glam::DVec3;
use std::sync::{Arc, OnceLock};

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn test_ctx() -> ServerContext {
    if let Some(sde) = SHARED_SDE.get() {
        ServerContext::with_sde_and_repository(
            Arc::clone(sde),
            Arc::new(eve_db::WorldRepository::with_demo_seed()),
        )
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
    } else {
        let ctx = ServerContext::new();
        let _ = SHARED_SDE.set(Arc::clone(&ctx.sde));
        ctx
    }
}

fn clone_type_with(
    sde: &mut Arc<SdeRegistry>,
    base_tid: u32,
    new_tid: u32,
    group_id: u32,
    category_id: u32,
    attrs: &[(u16, f64)],
) {
    let reg = Arc::make_mut(sde);
    let mut rec: InvType = reg.types.values().next().cloned().expect("base sde type");
    if let Some(b) = reg.types.get(&base_tid) {
        rec = b.clone();
    }
    rec.type_id = new_tid;
    rec.group_id = group_id;
    rec.category_id = category_id;
    rec.name = format!("CtrlType_{new_tid}");
    rec.radius = 5_000.0;
    rec.mass = 1.0e11;
    rec.default_attributes.clear();
    for &(k, v) in attrs {
        rec.default_attributes.insert(k, v);
    }
    reg.types.insert(new_tid, rec);
}

fn make_session(
    char_id: i64,
    corp_id: i64,
    sys_id: u32,
    ship_id: Option<i64>,
    structure_id: Option<i64>,
    role: u64,
) -> SessionState {
    let mut s = SessionState::new(720);
    s.user_id = Some(char_id as u64);
    s.character_id = Some(char_id);
    s.corporation_id = Some(corp_id);
    s.solar_system_id = Some(sys_id);
    s.ship_id = ship_id;
    s.structure_id = structure_id;
    s.corp_role = role;
    s
}

fn seed_pilot_and_structure(
    ctx: &ServerContext,
    char_id: i64,
    corp_id: i64,
    sys_id: u32,
    ship_id: i64,
    sid: i64,
    st_tid: u32,
    is_ceo: bool,
) {
    ctx.db.with_mut_snapshot(|snap| {
        let corp = snap.corporations.entry(corp_id).or_insert_with(|| { let mut __corp = CorporationRecord::default();
            __corp.corporation_id = corp_id;
            __corp.name = format!("Corp {corp_id}");
            __corp.ticker = "CTL".into();
            __corp.ceo_id = if is_ceo { char_id } else { char_id + 500 };
            __corp.hq_station_id = Some(60003760);
            __corp.tax_rate = 0.0;
            __corp.members = std::collections::HashSet::from([char_id]);
            __corp.set_division_balance(1, IskCents(0));
            __corp });
        corp.members.insert(char_id);
        let mut ch = snap.characters.values().next().cloned().expect("demo char");
        ch.character_id = char_id;
        ch.user_id = char_id as u64;
        ch.name = format!("Pilot {char_id}");
        ch.corporation_id = corp_id;
        ch.alliance_id = None;
        ch.solar_system_id = sys_id;
        ch.station_id = None;
        ch.structure_id = Some(sid);
        ch.active_ship_id = ship_id;
        ch.home_station_id = 60003760;
        ch.balance = IskCents(100_000);
        snap.characters.insert(char_id, ch);
        let uid = char_id as u64;
        if !snap.accounts.values().any(|a| a.user_id == uid) {
            let uname = format!("u{uid}");
            snap.accounts.insert(
                uname.clone(),
                eve_db::AccountRecord {
                    user_id: uid,
                    username: uname,
                    password_token: String::new(),
                    role: 0,
                    banned_until_unix: None,
                    ban_reason: None,
                },
            );
        }
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 670,
                owner_id: char_id,
                location_id: sid,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.structures.entry(sid).or_insert_with(|| UpwellStructureRecord {
            structure_id: sid,
            type_id: st_tid,
            owner_corp_id: corp_id,
            solar_system_id: sys_id,
            name: "Control Citadel".into(),
            position: [1_000_000_000.0, 0.0, 0.0],
            state: "ShieldVulnerable".into(),
            quantum_core_installed: true,
            fuel_blocks: 100,
            shield_hp: 500_000.0,
            armor_hp: 500_000.0,
            hull_hp: 500_000.0,
            reinforced_until_unix: 0,
            last_damage_tick: 0,
            last_damage_unix: 0,
            self_destruct_at_unix: None,
        });
    });
}

#[tokio::test(flavor = "current_thread")]
async fn docked_take_control_release_eject_and_pilot_lookup_roundtrip() {
    let mut ctx = test_ctx();
    let st_tid = 930_001u32;
    clone_type_with(&mut ctx.sde, 35832, st_tid, 1657, 65, &[(1547, 2.0)]);

    let sid = 10_000_950_001i64;
    seed_pilot_and_structure(&ctx, 5101, 98510, 30000142, 61001, sid, st_tid, true);
    seed_pilot_and_structure(&ctx, 5102, 98510, 30000142, 61002, sid, st_tid, false);
    ctx.db
        .acquire_online_lock(5101, 720, Some(5101), true, ctx.now_unix())
        .unwrap();
    ctx.db
        .acquire_online_lock(5102, 720, Some(5102), true, ctx.now_unix())
        .unwrap();

    // Non-director without ACL cannot control
    let mut rogue = make_session(5102, 98510, 30000142, Some(61002), Some(sid), 0);
    let denied = handle_take_control(&ctx, &mut rogue, &[PyRep::Integer(sid)])
        .await
        .unwrap_err();
    assert_eq!(denied.code, "CorpRoleDenied");

    // CEO docked in structure takes control
    let mut ceo = make_session(5101, 98510, 30000142, Some(61001), Some(sid), 1);
    let took = handle_take_control(&ctx, &mut ceo, &[PyRep::Integer(sid)])
        .await
        .expect("take control");
    assert!(matches!(took, PyRep::KeyVal(_)));
    assert_eq!(ceo.ship_id, Some(sid));
    assert!(is_structure_vessel(&ctx, sid));

    // Reject navigation while controlling structure
    assert_eq!(
        reject_if_controlling_structure(&ctx, sid, "warp").unwrap_err().code,
        "StructureCannotNavigate"
    );

    // Second pilot with Starbase Operator (non-manager) takes control after CEO releases; then CEO (manager) overtakes
    let _ = handle_release_control(&ctx, &mut ceo, &[PyRep::Integer(sid)]).expect("ceo release");
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations
            .get_mut(&98510)
            .unwrap()
            .member_roles
            .insert(5102, eve_db::corp_roles::ROLE_STARBASE_OPERATOR);
    });
    rogue.corp_role = eve_db::corp_roles::ROLE_STARBASE_OPERATOR;
    let _ = handle_take_control(&ctx, &mut rogue, &[PyRep::Integer(sid)])
        .await
        .expect("rogue take");
    assert_eq!(
        handle_get_structure_pilot(&ctx, &rogue, &[PyRep::Integer(sid)]).expect("get pilot"),
        PyRep::Integer(5102)
    );

    let _ = rogue.drain_notifications();
    let _ = ctx.drain_notifications_for_char(5102);
    let _ = ceo.drain_notifications();
    let _ = ctx.drain_notifications_for_char(5101);
    let overtaken = handle_take_control(&ctx, &mut ceo, &[PyRep::Integer(sid)])
        .await
        .expect("ceo overtake");
    assert!(matches!(overtaken, PyRep::KeyVal(_)));
    let ceo_assume: Vec<_> = ceo
        .drain_notifications()
        .into_iter()
        .chain(
            ctx.drain_notifications_for_char(5101)
                .into_iter()
                .map(|n| (n.event, n.args)),
        )
        .collect();
    assert!(
        ceo_assume
            .iter()
            .any(|(ev, args)| ev == "OnAssumeStructureControl" && args.is_empty()),
        "TakeControl must emit 0-arg OnAssumeStructureControl: {ceo_assume:?}"
    );
    let rogue_relinquish = ctx.drain_notifications_for_char(5102);
    assert!(
        rogue_relinquish
            .iter()
            .any(|n| n.event == "OnRelinquishStructureControl" && n.args.is_empty()),
        "Overtake must emit 0-arg OnRelinquishStructureControl to displaced pilot: {rogue_relinquish:?}"
    );
    assert!(
        rogue_relinquish
            .iter()
            .any(|n| {
                n.event == "OnSessionChanged"
                    && matches!(
                        n.args.get(1),
                        Some(PyRep::KeyVal(kv))
                            if kv.get("solarsystemid2").and_then(|v| v.as_i64()).is_some()
                                && kv.get("constellationid").and_then(|v| v.as_i64()).is_some()
                                && kv.get("regionid").and_then(|v| v.as_i64()).is_some()
                                && kv.get("raceID").and_then(|v| v.as_i64()).is_some()
                                && kv.get("bloodlineID").and_then(|v| v.as_i64()).is_some()
                                && kv.get("genderID").and_then(|v| v.as_i64()).is_some()
                    )
            }),
        "Displaced OnSessionChanged must carry full session KeyVal (hierarchy + bloodline/race/gender): {rogue_relinquish:?}"
    );

    // Non-pilot cannot release
    assert_eq!(
        handle_release_control(&ctx, &mut rogue, &[PyRep::Integer(sid)]).unwrap_err().code,
        "NotControllingStructure"
    );

    // Release control restores ship
    let released = handle_release_control(&ctx, &mut ceo, &[PyRep::Integer(sid)]).expect("release");
    assert_eq!(released, PyRep::None);
    assert_eq!(ceo.ship_id, Some(61001));
    let ceo_release: Vec<_> = ceo
        .drain_notifications()
        .into_iter()
        .chain(
            ctx.drain_notifications_for_char(5101)
                .into_iter()
                .map(|n| (n.event, n.args)),
        )
        .collect();
    assert!(
        ceo_release
            .iter()
            .any(|(ev, args)| ev == "OnRelinquishStructureControl" && args.is_empty()),
        "ReleaseControl must emit 0-arg OnRelinquishStructureControl: {ceo_release:?}"
    );
    assert!(
        ceo_release
            .iter()
            .any(|(ev, args)| ev == "OnSessionChanged" && matches!(args.get(1), Some(PyRep::KeyVal(kv)) if kv.contains_key("solarsystemid2") && kv.contains_key("userid"))),
        "ReleaseControl OnSessionChanged must carry full session KeyVal: {ceo_release:?}"
    );
    assert_eq!(
        handle_get_structure_pilot(&ctx, &ceo, &[PyRep::Integer(sid)]).expect("none"),
        PyRep::None
    );

    // Take control again then eject
    let _ = handle_take_control(&ctx, &mut ceo, &[PyRep::Integer(sid)])
        .await
        .expect("re-take");
    ceo.role = service_roles::ROLE_GMH;
    let ejected = handle_eject_from_structure(&ctx, &mut ceo, &[PyRep::Integer(sid)]).expect("eject");
    assert!(matches!(ejected, PyRep::KeyVal(_)));
    assert_eq!(ceo.ship_id, Some(61001));
}

#[tokio::test(flavor = "current_thread")]
async fn flex_board_proximity_and_undock_ship_prep_work() {
    let mut ctx = test_ctx();
    let flex_tid = 930_010u32;
    clone_type_with(
        &mut ctx.sde,
        35841,
        flex_tid,
        1406,
        65,
        &[
            (crate::services::station_structure::structure_lifecycle::ATTR_HI_SLOTS, 2.0),
        ],
    );
    let sid = 10_000_960_001i64;
    let ship_id = 62001i64;
    seed_pilot_and_structure(&ctx, 5201, 98520, 30000142, ship_id, sid, flex_tid, true);
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&5201).unwrap().structure_id = None;
        snap.items.get_mut(&ship_id).unwrap().location_id = 30000142;
        snap.items.get_mut(&ship_id).unwrap().flag = InventoryFlag::None;
    });

    let sys = ctx.systems.get_or_spawn(30000142);
    let _ = sys
        .spawn_ball_with_meta(
            Ball {
                id: ship_id,
                type_id: 670,
                position: DVec3::new(1_000_100_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 2.0,
                mass: 1e6,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState::new(),
            SlimItemMeta::default(),
        )
        .await;

    let mut sess = make_session(5201, 98520, 30000142, Some(ship_id), None, 1);
    // Out of range (> radius 5000 + 2500)
    let too_far = handle_board_structure(&ctx, &mut sess, &[PyRep::Integer(sid)])
        .await
        .unwrap_err();
    assert_eq!(too_far.code, "OutOfBoardingRange");

    // Move within proximity -> boards FLEX in space
    let _ = sys
        .spawn_ball_with_meta(
            Ball {
                id: ship_id,
                type_id: 670,
                position: DVec3::new(1_000_006_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 2.0,
                mass: 1e6,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState::new(),
            SlimItemMeta::default(),
        )
        .await;
    let boarded = handle_board_structure(&ctx, &mut sess, &[PyRep::Integer(sid)])
        .await
        .expect("board flex");
    assert!(matches!(boarded, PyRep::KeyVal(_)));
    assert_eq!(sess.ship_id, Some(sid));

    // Release control back to ship in space
    let rel = handle_release_control(&ctx, &mut sess, &[PyRep::Integer(sid)]).expect("rel flex");
    assert_eq!(rel, PyRep::None);
    assert_eq!(sess.ship_id, Some(ship_id));

    // Re-board then test prepare_ship_for_undock_while_controlling & rollback & release_on_undock
    let _ = handle_board_structure(&ctx, &mut sess, &[PyRep::Integer(sid)])
        .await
        .expect("reboard");
    let prepped = prepare_ship_for_undock_while_controlling(&ctx, sid, 5201)
        .expect("prep undock");
    assert_eq!(prepped, ship_id);
    rollback_ship_after_failed_undock(&ctx, sid, 5201);
    assert_eq!(ctx.db.with_snapshot(|s| s.characters[&5201].active_ship_id), sid);

    release_structure_control_on_undock(&ctx, &mut sess, sid, 5201);
    assert_eq!(sess.ship_id, Some(ship_id));
    release_all_structure_control_for_character(&ctx, Some(&mut sess), 5201);
}

#[tokio::test(flavor = "current_thread")]
async fn unauthorized_controller_revocation_and_auto_defense_tick_work() {
    let mut ctx = test_ctx();
    let st_tid = 930_020u32;
    let pdb_tid = 930_021u32;
    let launcher_tid = 930_022u32;
    clone_type_with(&mut ctx.sde, 35832, st_tid, 1657, 65, &[]);
    clone_type_with(
        &mut ctx.sde,
        35926,
        pdb_tid,
        1330,
        66,
        &[
            (73, 1000.0),   // duration ms
            (54, 25_000.0), // maxRange
            (64, 50.0),     // dmgMult
            (114, 100.0),   // emDamage
        ],
    );
    clone_type_with(
        &mut ctx.sde,
        35924,
        launcher_tid,
        511,
        66,
        &[(73, 1000.0), (54, 300_000.0), (64, 10.0)],
    );

    let sid = 10_000_970_001i64;
    seed_pilot_and_structure(&ctx, 5301, 98530, 30000142, 63001, sid, st_tid, false);

    // Assign unauthorized pilot 5301 as active controller and verify revocation
    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        ext.active_pilot_by_structure.insert(sid, 5301);
        ext.previous_ship_by_structure.insert(sid, 63001);
        save_ext_state(snap, &ext);
    });
    let released = release_unauthorized_structure_controllers(&ctx);
    assert_eq!(released, vec![(sid, 5301, 30000142, 63001)]);
    notify_released_structure_controllers(&ctx, &released);

    // Weapon cycle computation and auto-defense tick with online PDB + hostile ball on grid
    let (cycles, _) = compute_weapon_cycles(0.0, 2.0, 1.0);
    assert_eq!(cycles, 2);

    ctx.db.with_mut_snapshot(|snap| {
        let mut hostile = snap.characters[&5301].clone();
        hostile.character_id = 9901;
        hostile.corporation_id = 98999;
        hostile.structure_id = None;
        hostile.station_id = None;
        hostile.active_ship_id = 63099;
        snap.characters.insert(9901, hostile);
        snap.items.insert(
            75001,
            InventoryItemRecord {
                item_id: 75001,
                type_id: pdb_tid,
                owner_id: 98530,
                location_id: sid,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        let mut ext = load_ext_state(snap);
        ext.lifecycle.insert(
            sid,
            StructureLifecycleState {
                online_modules: vec![75001],
                ..Default::default()
            },
        );
        save_ext_state(snap, &ext);
        let mut ds = StructureDefenseSettings::default();
        ds.structure_id = sid;
        ds.auto_defense_enabled = true;
        ds.record_attacker(9901, ctx.now_unix());
        save_defense_settings(snap, &ds);
    });

    let sys = ctx.systems.get_or_spawn(30000142);
    let mut host_cs = CombatState::new();
    host_cs.shield_hp = 50_000.0;
    host_cs.max_shield_hp = 50_000.0;
    host_cs.armor_hp = 50_000.0;
    host_cs.max_armor_hp = 50_000.0;
    host_cs.structure_hp = 50_000.0;
    host_cs.max_structure_hp = 50_000.0;
    let _ = sys
        .spawn_ball_with_meta(
            Ball {
                id: 63099,
                type_id: 670,
                position: DVec3::new(1_000_010_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 2.0,
                mass: 1e6,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            host_cs,
            SlimItemMeta {
                owner_id: 9901,
                char_id: Some(9901),
                corp_id: 98999,
                alliance_id: -1,
                group_id: 25,
                category_id: 6,
                security_status: -5.0,
                is_cloaked: false,
                ..Default::default()
            },
        )
        .await;

    step_structure_auto_defense(&ctx, 30000142, 2.0).await;
    let after_cs = sys.get_combat_state(63099).await.expect("ok").expect("cs");
    assert!(after_cs.shield_hp < 50_000.0);
}

#[tokio::test(flavor = "current_thread")]
async fn get_structure_pilot_enforces_db_authz_and_is_read_pure_before_due_profile() {
    let ctx = test_ctx();
    let (corp_id, rival_corp_id) = (98_651_i64, 98_652_i64);
    let (sid, sid_due, dead_sid, missing_sid) = (
        10_000_650_001_i64,
        10_000_650_002_i64,
        10_000_650_003_i64,
        10_000_650_999_i64,
    );
    let (
        ceo_id,
        gunner_id,
        roleless_id,
        starbase_op_id,
        docked_guest_id,
        outsider_id,
        acl_gunner_id,
        gm_id,
    ) = (
        65_101_i64,
        65_102_i64,
        65_103_i64,
        65_104_i64,
        65_105_i64,
        65_106_i64,
        65_107_i64,
        65_108_i64,
    );

    seed_pilot_and_structure(&ctx, ceo_id, corp_id, 30_000_142, 65_201, sid, 35_832, true);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&ceo_id) {
            ch.structure_id = None;
        }
        let corp = snap.corporations.get_mut(&corp_id).unwrap();
        corp.members.extend([ceo_id, gunner_id, roleless_id, starbase_op_id]);
        corp.member_roles
            .insert(gunner_id, eve_db::corp_roles::ROLE_STARBASE_OPERATOR);
        corp.member_roles.insert(roleless_id, 0);
        corp.member_roles
            .insert(starbase_op_id, eve_db::corp_roles::ROLE_STARBASE_OPERATOR);

        let mut rival = CorporationRecord::default();
        rival.corporation_id = rival_corp_id;
        rival.name = "Rival Control Corp".into();
        rival.ticker = "RVC".into();
        rival.ceo_id = outsider_id;
        rival.members = std::collections::HashSet::from([docked_guest_id, outsider_id, acl_gunner_id, gm_id]);
        snap.corporations.insert(rival_corp_id, rival);

        let base_ch = snap.characters[&ceo_id].clone();
        for (cid, c_corp, c_struct, ship_id) in [
            (gunner_id, corp_id, Some(sid), 65_202_i64),
            (roleless_id, corp_id, None, 65_203_i64),
            (starbase_op_id, corp_id, None, 65_204_i64),
            (docked_guest_id, rival_corp_id, Some(sid), 65_205_i64),
            (outsider_id, rival_corp_id, None, 65_206_i64),
            (acl_gunner_id, rival_corp_id, None, 65_207_i64),
            (gm_id, rival_corp_id, None, 65_208_i64),
        ] {
            let mut ch = base_ch.clone();
            ch.character_id = cid;
            ch.user_id = cid as u64;
            ch.name = format!("CtrlAuth {cid}");
            ch.corporation_id = c_corp;
            ch.structure_id = c_struct;
            ch.active_ship_id = ship_id;
            snap.characters.insert(cid, ch);
            let uid = cid as u64;
            if !snap.accounts.values().any(|a| a.user_id == uid) {
                let uname = format!("u{uid}");
                snap.accounts.insert(
                    uname.clone(),
                    eve_db::AccountRecord {
                        user_id: uid,
                        username: uname,
                        password_token: String::new(),
                        role: 0,
                        banned_until_unix: None,
                        ban_reason: None,
                    },
                );
            }
            snap.items.insert(
                ship_id,
                InventoryItemRecord {
                    item_id: ship_id,
                    type_id: 670,
                    owner_id: cid,
                    location_id: c_struct.unwrap_or(30_000_142),
                    flag: if c_struct.is_some() {
                        InventoryFlag::Hangar
                    } else {
                        InventoryFlag::None
                    },
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
        }

        let mut st_due = snap.structures[&sid].clone();
        st_due.structure_id = sid_due;
        snap.structures.insert(sid_due, st_due);

        let mut st_dead = snap.structures[&sid].clone();
        st_dead.structure_id = dead_sid;
        st_dead.state = "Destroyed".into();
        snap.structures.insert(dead_sid, st_dead);

        let mut prof_allow = ops::StructureAccessProfile::default();
        prof_allow.profile_id = 901;
        prof_allow.owner_corp_id = corp_id;
        prof_allow.defense_allowed_characters.insert(acl_gunner_id);

        let mut prof_revoke = ops::StructureAccessProfile::default();
        prof_revoke.profile_id = 902;
        prof_revoke.owner_corp_id = corp_id;
        prof_revoke.corp_only = true;
        prof_revoke.defense_blocked_characters.insert(acl_gunner_id);

        let mut ext = load_ext_state(snap);
        ext.active_pilot_by_structure.insert(sid, gunner_id);
        ext.active_pilot_by_structure.insert(sid_due, acl_gunner_id);
        ext.profiles_by_id.insert(901, prof_allow.clone());
        ext.profiles_by_id.insert(902, prof_revoke);
        ext.structure_profile_id.insert(sid, 901);
        ext.access_profiles.insert(sid, prof_allow.clone());
        ext.structure_profile_id.insert(sid_due, 901);
        ext.access_profiles.insert(sid_due, prof_allow);
        ext.pending_profile_applications.insert(sid_due, (902, 1));
        ext.pending_profile_applications.insert(dead_sid, (902, 1));
        save_ext_state(snap, &ext);
    });
    ctx.db
        .acquire_online_lock(
            acl_gunner_id as u64,
            921,
            Some(acl_gunner_id),
            true,
            ctx.now_unix(),
        )
        .unwrap();
    let _ = ctx.drain_notifications_for_char(acl_gunner_id);

    let commits_before = ctx.db.tx_commit_count();
    let epoch_before = ctx.db.state_epoch();

    // Unauthenticated & missing structureID arg rejected read-purely.
    let unauth = SessionState::new(920);
    assert!(handle_get_structure_pilot(&ctx, &unauth, &[PyRep::Integer(sid)]).is_err());
    let undocked_ceo = make_session(ceo_id, corp_id, 30_000_142, Some(65_201), None, 0);
    assert_eq!(
        handle_get_structure_pilot(&ctx, &undocked_ceo, &[])
            .unwrap_err()
            .code,
        "MissingStructureId"
    );

    // Cross-corp outsider with spoofed session fields, undocked roleless member, and revoked ACL pilot
    // are ALL rejected read-purely even when `pending_profile_applications` is due on `sid_due` & `dead_sid`.
    let spoof = make_session(
        outsider_id,
        corp_id,
        30_000_142,
        Some(65_206),
        Some(sid_due),
        u64::MAX,
    );
    let roleless_sess = make_session(roleless_id, corp_id, 30_000_142, Some(65_203), None, u64::MAX);
    let revoked_acl_sess = make_session(acl_gunner_id, rival_corp_id, 30_000_142, Some(65_207), None, 0);
    for (sess_ref, target) in [
        (&spoof, sid_due),
        (&spoof, dead_sid),
        (&roleless_sess, sid_due),
        (&revoked_acl_sess, sid_due),
    ] {
        let err = handle_get_structure_pilot(&ctx, sess_ref, &[PyRep::Integer(target)])
            .expect_err("unauthorized GetStructurePilot must reject");
        assert_eq!(err.code, "DefenseAccessDenied");
        assert_eq!(
            err.client_error.as_deref(),
            Some(&eve_proto::ClientError::user(eve_proto::UserErrorKey::AccessDenied))
        );
    }

    // Destroyed structure rejects authorized CEO read-purely (does not apply due profile), and missing/bad ID rejects.
    assert_eq!(
        handle_get_structure_pilot(&ctx, &undocked_ceo, &[PyRep::Integer(dead_sid)])
            .unwrap_err()
            .code,
        "StructureDestroyed"
    );
    assert_eq!(
        handle_get_structure_pilot(&ctx, &undocked_ceo, &[PyRep::Integer(missing_sid)])
            .unwrap_err()
            .code,
        "StructureNotFound"
    );

    // Authorized read-only checks on `sid`: DB-docked rival guest, undocked StarbaseOperator, undocked CEO,
    // undocked non-revoked ACL gunner, and GM all succeed cleanly.
    let docked_guest_sess = make_session(docked_guest_id, rival_corp_id, 30_000_142, Some(65_205), None, 0);
    let starbase_op_sess = make_session(starbase_op_id, corp_id, 30_000_142, Some(65_204), None, 0);
    let acl_gunner_sess = make_session(acl_gunner_id, rival_corp_id, 30_000_142, Some(65_207), None, 0);
    let mut gm_sess = make_session(gm_id, rival_corp_id, 30_000_142, Some(65_208), None, 0);
    gm_sess.role = service_roles::ROLE_GM_DEFAULT;
    for sess_ref in [
        &docked_guest_sess,
        &starbase_op_sess,
        &undocked_ceo,
        &acl_gunner_sess,
        &gm_sess,
    ] {
        assert_eq!(
            handle_get_structure_pilot(&ctx, sess_ref, &[PyRep::Integer(sid)])
                .expect("authorized GetStructurePilot must succeed"),
            PyRep::Integer(gunner_id)
        );
    }

    // Confirm zero DB mutations and zero controller-release notifications occurred across all checks above.
    assert_eq!(ctx.db.tx_commit_count(), commits_before);
    assert_eq!(ctx.db.state_epoch(), epoch_before);
    assert!(ctx.drain_notifications_for_char(acl_gunner_id).is_empty());
    ctx.db.with_snapshot(|snap| {
        let ext = load_ext_state(snap);
        assert!(ext.pending_profile_applications.contains_key(&sid_due));
        assert!(ext.pending_profile_applications.contains_key(&dead_sid));
        assert_eq!(ext.active_pilot_by_structure.get(&sid_due), Some(&acl_gunner_id));
    });

    // Authorized CEO calling GetStructurePilot on `sid_due` applies due profile 902 and releases revoked `acl_gunner_id`.
    assert_eq!(
        handle_get_structure_pilot(&ctx, &undocked_ceo, &[PyRep::Integer(sid_due)])
            .expect("authorized CEO triggers due profile application"),
        PyRep::None
    );
    assert!(ctx.db.tx_commit_count() > commits_before);
    ctx.db.with_snapshot(|snap| {
        let ext = load_ext_state(snap);
        assert_eq!(ext.structure_profile_id.get(&sid_due), Some(&902));
        assert!(!ext.pending_profile_applications.contains_key(&sid_due));
        assert!(!ext.active_pilot_by_structure.contains_key(&sid_due));
    });
    assert!(
        ctx.drain_notifications_for_char(acl_gunner_id)
            .iter()
            .any(|n| n.event == "OnSessionChanged")
    );
}

#[cfg(test)]
#[path = "structure_control_ops_extra_unit_tests.rs"]
mod extra_unit_tests;
