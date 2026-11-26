//! Deep branch and mutation-killing unit tests for `structure_lifecycle_rpc`.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::station_structure::structure_lifecycle as life;
use crate::services::station_structure::structure_lifecycle_rpc::*;
use crate::services::station_structure::structure_upwell_ops::{load_ext_state, save_ext_state};
use eve_db::{
    CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents, UpwellStructureRecord,
};
use eve_destiny::{Ball, CombatState, MovementMode, SlimItemMeta};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::{InvType, SdeRegistry};
use glam::DVec3;
use std::collections::HashSet;
use std::sync::{Arc, OnceLock};

pub(super) static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

pub(super) fn test_ctx() -> ServerContext {
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

pub(super) fn clone_type_with(
    sde: &mut Arc<SdeRegistry>,
    new_tid: u32,
    group_id: u32,
    category_id: u32,
    volume: f64,
    capacity: f64,
    attrs: &[(u16, f64)],
) {
    let reg = Arc::make_mut(sde);
    let mut rec: InvType = reg
        .types
        .get(&35832)
        .cloned()
        .unwrap_or_else(|| reg.types.values().next().cloned().expect("base sde type"));
    rec.type_id = new_tid;
    rec.group_id = group_id;
    rec.category_id = category_id;
    rec.name = format!("ExtraStructRpc_{new_tid}");
    rec.volume = volume;
    rec.capacity = capacity;
    rec.radius = 5_000.0;
    rec.mass = 1.0e11;
    rec.default_attributes.clear();
    rec.effect_ids.clear();
    for &(k, v) in attrs {
        rec.default_attributes.insert(k, v);
    }
    reg.types.insert(new_tid, rec);
}

pub(super) fn seed_astrahus_type(ctx: &mut ServerContext, tid: u32) {
    let attrs = [
        (life::ATTR_RIG_SIZE, 2.0),
        (life::ATTR_SHIELD_CAPACITY, 500_000.0),
        (life::ATTR_ARMOR_HP, 500_000.0),
        (life::ATTR_HP, 500_000.0),
        (life::ATTR_HI_SLOTS, 3.0),
        (life::ATTR_MED_SLOTS, 3.0),
        (life::ATTR_LOW_SLOTS, 3.0),
        (life::ATTR_RIG_SLOTS, 3.0),
        (life::ATTR_SERVICE_SLOTS, 3.0),
    ];
    clone_type_with(&mut ctx.sde, tid, 1657, life::CATEGORY_STRUCTURE, 80_000.0, 0.0, &attrs);
}

pub(super) fn make_session(char_id: i64, corp_id: i64, sys_id: u32, ship_id: Option<i64>, role: u64) -> SessionState {
    let mut s = SessionState::new(715);
    s.user_id = Some(char_id as u64);
    s.character_id = Some(char_id);
    s.corporation_id = Some(corp_id);
    s.solar_system_id = Some(sys_id);
    s.ship_id = ship_id;
    s.corp_role = role;
    s
}

pub(super) fn seed_char_corp_ship(ctx: &ServerContext, char_id: i64, corp_id: i64, sys_id: u32, ship_id: i64) {
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.insert(
            corp_id,
            { let mut __corp = CorporationRecord::default();
                __corp.corporation_id = corp_id;
                __corp.name = format!("Corp {corp_id}");
                __corp.ticker = "EXT".into();
                __corp.ceo_id = char_id;
                __corp.hq_station_id = Some(60003760);
                __corp.tax_rate = 0.05;
                __corp.members = HashSet::from([char_id]);
                __corp.set_division_balance(1, IskCents(10_000_000));
                __corp },
        );
        let mut ch = snap.characters.values().next().cloned().expect("demo char");
        ch.character_id = char_id;
        ch.name = format!("Pilot {char_id}");
        ch.corporation_id = corp_id;
        ch.alliance_id = None;
        ch.solar_system_id = sys_id;
        ch.station_id = None;
        ch.structure_id = None;
        ch.active_ship_id = ship_id;
        ch.balance = IskCents(10_000_000);
        snap.characters.insert(char_id, ch);
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 670,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
}

pub(super) fn seed_structure(
    ctx: &ServerContext,
    structure_id: i64,
    type_id: u32,
    corp_id: i64,
    sys_id: u32,
    state: &str,
    fuel_blocks: u32,
) {
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            structure_id,
            UpwellStructureRecord {
                structure_id,
                type_id,
                owner_corp_id: corp_id,
                solar_system_id: sys_id,
                name: format!("Citadel {structure_id}"),
                position: [2_000_000_000.0, 0.0, 0.0],
                state: state.to_string(),
                quantum_core_installed: true,
                fuel_blocks,
                shield_hp: 50_000_000.0,
                armor_hp: 50_000_000.0,
                hull_hp: 50_000_000.0,
                ..Default::default()
            },
        );
    });
}

#[tokio::test(flavor = "current_thread")]
async fn anchor_guards_palatine_zarzakh_thera_npc_corp_profile_and_stack_quantity() {
    let mut ctx = test_ctx();
    let tid = 918_001_u32;
    let palatine_tid = 40340_u32;
    seed_astrahus_type(&mut ctx, tid);
    seed_astrahus_type(&mut ctx, palatine_tid);

    let sys_id = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| {
            s.security_status >= 0.5
                && !crate::services::structure_anchoring_rules::is_trade_hub_system(s.solar_system_id)
                && !crate::services::structure_anchoring_rules::is_starter_or_career_system(&ctx.sde, s.solar_system_id)
        })
        .map(|s| s.solar_system_id)
        .unwrap_or(30000144);
    let char_id = 918_101_i64;
    let corp_id = 98_918_101_i64;
    let ship_id = 918_201_i64;
    seed_char_corp_ship(&ctx, char_id, corp_id, sys_id, ship_id);

    // 1. NoActiveShip
    let mut no_ship_sess = make_session(char_id, corp_id, sys_id, None, 1);
    assert_eq!(
        handle_anchor(&ctx, &mut no_ship_sess, &[PyRep::Integer(i64::from(tid))])
            .await
            .unwrap_err()
            .code,
        "NoActiveShip"
    );

    // 2. ItemNotFound (negative item ID not in items) & InvalidStructureType
    let mut sess = make_session(char_id, corp_id, sys_id, Some(ship_id), 1);
    assert_eq!(
        handle_anchor(&ctx, &mut sess, &[])
            .await
            .unwrap_err()
            .code,
        "InvalidStructureType"
    );
    assert_eq!(
        handle_anchor(&ctx, &mut sess, &[PyRep::Integer(-999)])
            .await
            .unwrap_err()
            .code,
        "ItemNotFound"
    );
    assert_eq!(
        handle_anchor(&ctx, &mut sess, &[PyRep::Integer(587)])
            .await
            .unwrap_err()
            .code,
        "InvalidStructureType"
    );

    // 3. Thera (31000005) & Zarzakh (30100000) anchoring prohibitions
    {
        let sde = Arc::make_mut(&mut ctx.sde);
        let mut z = sde.solar_systems.get(&sys_id).unwrap().clone();
        z.solar_system_id = 30_100_000;
        sde.solar_systems.insert(30_100_000, z.clone());
        z.solar_system_id = 31_000_005;
        sde.solar_systems.insert(31_000_005, z);
    }
    sess.solar_system_id = Some(30_100_000);
    assert_eq!(
        handle_anchor(&ctx, &mut sess, &[PyRep::Integer(i64::from(tid))])
            .await
            .unwrap_err()
            .code,
        "AnchoringDisallowedInZarzakh"
    );
    sess.solar_system_id = Some(31_000_005);
    assert_eq!(
        handle_anchor(&ctx, &mut sess, &[PyRep::Integer(i64::from(tid))])
            .await
            .unwrap_err()
            .code,
        "AnchoringDisallowedInTheraOrShattered"
    );

    // 4. Palatine Keepstar limit (40340 already anchored) & ShipNotInSpace guard
    sess.solar_system_id = Some(sys_id);
    assert_eq!(
        handle_anchor(&ctx, &mut sess, &[PyRep::Integer(i64::from(tid))])
            .await
            .unwrap_err()
            .code,
        "ShipNotInSpace"
    );
    ctx.systems
        .get_or_spawn(sys_id)
        .spawn_ball_with_meta(
            Ball {
                id: ship_id,
                type_id: 670,
                position: DVec3::new(8.0e9 - 10_000.0, 1.0e9, 2.0e9),
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1.0e6,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState::new(),
            SlimItemMeta::default(),
        )
        .await
        .unwrap();
    seed_structure(&ctx, 10_000_999_001, palatine_tid, corp_id, sys_id, life::STATE_SHIELD_VULNERABLE, 100);
    assert_eq!(
        handle_anchor(&ctx, &mut sess, &[PyRep::Integer(i64::from(palatine_tid))])
            .await
            .unwrap_err()
            .code,
        "PalatineKeepstarLimitReached"
    );

    // 5. NPC Corp cannot anchor
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().corporation_id = 1_000_009;
    });
    sess.corporation_id = Some(1_000_009);
    assert_eq!(
        handle_anchor(&ctx, &mut sess, &[PyRep::Integer(i64::from(tid))])
            .await
            .unwrap_err()
            .code,
        "NpcCorpCannotAnchorStructure"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().corporation_id = corp_id;
    });
    sess.corporation_id = Some(corp_id);

    // 6. Foreign profile ownership violation & stack quantity > 1 decrement
    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        let mut prof = ext.profiles_by_id.values().next().cloned().unwrap_or_default();
        prof.profile_id = 777;
        prof.owner_corp_id = corp_id + 1;
        ext.profiles_by_id.insert(777, prof);
        save_ext_state(snap, &ext);
        snap.items.insert(
            55_901,
            InventoryItemRecord {
                item_id: 55_901,
                type_id: tid,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 2,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });
    let bad_prof_args = vec![
        PyRep::Integer(55_901),
        PyRep::None,
        PyRep::None,
        PyRep::Real(0.0),
        PyRep::Integer(777),
        PyRep::Integer(1),
        PyRep::Integer(18),
        PyRep::String("My Astrahus".into()),
    ];
    assert_eq!(
        handle_anchor(&ctx, &mut sess, &bad_prof_args)
            .await
            .unwrap_err()
            .code,
        "StructureOwnershipViolation"
    );

    let ok_args = vec![
        PyRep::Integer(55_901),
        PyRep::None,
        PyRep::None,
        PyRep::Real(0.0),
        PyRep::Integer(1),
        PyRep::Integer(1),
        PyRep::Integer(18),
        PyRep::String("My Astrahus".into()),
    ];
    assert!(handle_anchor(&ctx, &mut sess, &ok_args).await.is_ok());
    assert_eq!(ctx.db.with_snapshot(|s| s.items[&55_901].quantity), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn record_ship_damage_progression_routes_capsuleer_damage_and_guards_npc_drone_and_self() {
    let mut ctx = test_ctx();
    let day0_ts = 1_735_732_800_u64;
    ctx.db
        .set_clock(Arc::new(eve_db::VirtualServerClock::with_fixed_base(day0_ts)));

    let ship_tid = 918_501_u32;
    let drone_tid = 918_502_u32;
    clone_type_with(&mut ctx.sde, ship_tid, 25, 6, 2_500.0, 100.0, &[]);
    clone_type_with(&mut ctx.sde, drone_tid, 100, 18, 5.0, 0.0, &[]);

    let sys_id = 30_000_142_u32;
    let attacker_char_id = 918_301_i64;
    let victim_char_id = 918_302_i64;
    let corp_id = 98_918_301_i64;
    let attacker_ship_id = 918_401_i64;
    let victim_ship_id = 918_402_i64;
    let npc_ship_id = 918_403_i64;
    let drone_id = 918_404_i64;
    let project_id = 918_601_i64;

    seed_char_corp_ship(&ctx, attacker_char_id, corp_id, sys_id, attacker_ship_id);
    seed_char_corp_ship(&ctx, victim_char_id, corp_id + 1, sys_id, victim_ship_id);

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&attacker_ship_id).unwrap().type_id = ship_tid;
        snap.items.get_mut(&victim_ship_id).unwrap().type_id = ship_tid;
        snap.items.insert(
            npc_ship_id,
            InventoryItemRecord {
                item_id: npc_ship_id,
                type_id: ship_tid,
                owner_id: 500_999, // NPC faction/owner not in `snap.characters`
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            drone_id,
            InventoryItemRecord {
                item_id: drone_id,
                type_id: drone_tid,
                owner_id: victim_char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.corp_projects.insert(
            project_id,
            eve_db::CorpProjectRecord {
                project_id,
                corporation_id: corp_id,
                name: "Damage Capsuleers".to_string(),
                target_quantity: 10_000,
                current_quantity: 0,
                isk_per_unit: IskCents(0),
                escrow_remaining: IskCents(0),
            },
        );
        snap.alliance_war_state
            .project_meta
            .insert(project_id, ("DamageShips".to_string(), false, false));
        let mut det = eve_db::PersistedProjectDetails::default();
        det.status = "Active".to_string();
        snap.alliance_war_state
            .project_details
            .insert(project_id, det);
    });

    let sys = ctx.systems.get_or_spawn(sys_id);
    for (ball_id, tid, pos_x) in [
        (attacker_ship_id, ship_tid, 0.0),
        (victim_ship_id, ship_tid, 5_000.0),
        (npc_ship_id, ship_tid, 6_000.0),
        (drone_id, drone_tid, 7_000.0),
    ] {
        sys.spawn_ball_with_meta(
            Ball {
                id: ball_id,
                type_id: tid,
                position: DVec3::new(pos_x, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1.0e6,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState::new(),
            SlimItemMeta::default(),
        )
        .await
        .unwrap();
    }

    let daily_damage_prog = || {
        ctx.quasar_gateway
            .progression
            .daily_goals
            .get_or_init_goals_at(attacker_char_id as u32, day0_ts)
            .into_iter()
            .find(|g| g.template.category == 7)
            .expect("DamageShip daily goal category 7")
            .current_progress
    };
    let proj_prog = || ctx.db.with_snapshot(|s| s.corp_projects[&project_id].current_quantity);

    assert_eq!(daily_damage_prog(), 0);
    assert_eq!(proj_prog(), 0);

    // 1. Direct helper: capsuleer ship increments both daily goal 7 & corp DamageShips
    record_ship_damage_progression(&ctx, sys_id, attacker_char_id, victim_ship_id, 120.0);
    assert_eq!(daily_damage_prog(), 120);
    assert_eq!(proj_prog(), 120);

    // 2. Guards: NPC ship only increments corp DamageShips (never daily goal 7);
    //    drone (category != 6), self-ship, and non-character attacker increment neither daily goal 7 nor wrong counts.
    record_ship_damage_progression(&ctx, sys_id, attacker_char_id, npc_ship_id, 300.0);
    assert_eq!(daily_damage_prog(), 120);
    assert_eq!(proj_prog(), 420);

    record_ship_damage_progression(&ctx, sys_id, attacker_char_id, drone_id, 200.0);
    record_ship_damage_progression(&ctx, sys_id, attacker_char_id, attacker_ship_id, 150.0);
    record_ship_damage_progression(&ctx, sys_id, 9_000_000_001, victim_ship_id, 250.0);
    assert_eq!(daily_damage_prog(), 120);
    assert_eq!(proj_prog(), 570); // only self-ship added to DamageShips; drone and non-char attacker ignored

    // 3. apply_damage_structure_or_ball routes capsuleer ship hits (+100) and guards NPC hits (+0 daily)
    apply_damage_structure_or_ball(
        &ctx,
        &sys,
        sys_id,
        attacker_char_id,
        victim_ship_id,
        eve_destiny::DamageVector::new(100.0, 0.0, 0.0, 0.0),
        5.0,
        day0_ts,
    )
    .await
    .unwrap();
    assert_eq!(daily_damage_prog(), 220);
    assert_eq!(proj_prog(), 670);

    apply_damage_structure_or_ball(
        &ctx,
        &sys,
        sys_id,
        attacker_char_id,
        npc_ship_id,
        eve_destiny::DamageVector::new(100.0, 0.0, 0.0, 0.0),
        5.0,
        day0_ts,
    )
    .await
    .unwrap();
    assert_eq!(daily_damage_prog(), 220);
    assert_eq!(proj_prog(), 770);

    // 4. execute_pending_cycle_actions: LanceBeamFired + pre-notified CycleWeaponDamage
    sys.test_with_actor_state_mut_async(move |st| {
        st.pending_cycle_actions
            .push(crate::system_actor::PendingCycleAction::LanceBeamFired {
                attacker_char_id,
                attacker_ship_id,
                attacker_ship_type_id: ship_tid,
                module_id: 918_701,
                module_type_id: 484,
                hits: vec![(victim_ship_id, 50.0), (npc_ship_id, 60.0)],
            });
        st.pending_cycle_actions
            .push(crate::system_actor::PendingCycleAction::CycleWeaponDamage {
                attacker_char_id: Some(attacker_char_id),
                attacker_ship_id,
                attacker_ship_type_id: ship_tid,
                target_id: victim_ship_id,
                target_char_id: Some(victim_char_id),
                module_type_id: 484,
                applied_dmg: 75.0,
                notified: true,
            });
    })
    .await;
    crate::services::inventory_fitting::fitting_module_cycles::execute_pending_cycle_actions(
        &ctx, sys_id,
    )
    .await;
    assert_eq!(daily_damage_prog(), 345); // 220 + 50 (LanceBeam victim) + 75 (CycleWeaponDamage)
    assert_eq!(proj_prog(), 955); // 770 + 50 + 60 + 75
}
