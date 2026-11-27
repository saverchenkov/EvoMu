//! Discriminating end-to-end gameplay-path verification tests for `#1495` Track 2c-1:
//! - `OnCargoDelivered` net-delta anti-exploit via `invbroker.Add` / `MultiAdd` / `LootAll`
//! - `OnStructureDestroyed` for non-container & container structures via single death path
//! - `CompoundAnd` (including cargo withdrawal latch reset), `I1` un-entered guard + replay,
//!   `I2` finished-instance & `0` write-tx read purity, and pending escalation handoff
//! - Wiki v2 JSON `cargo_delivered` / `structure_destroyed` translation & object classification
//!   (plus fail-closed rejection on `can_hacked` / `ore_depleted` / unknown)

#![forbid(unsafe_code)]

use crate::router::context::BoundObjectEntry;
use crate::router::ServerContext;
use crate::services::combat_crimewatch::with_combat_runtime;
use crate::services::pve_dungeon_content::{
    build_wiki_deadspace_rooms, install_wiki_pve_catalog_for_test, parse_and_validate_wiki_pve_json,
};
use crate::services::pve_dungeon_runtime::{
    materialize_dungeon_room_entities, on_dungeon_item_looted, on_dungeon_room_entered,
    read_dungeon_runtime_store, reconcile_and_rematerialize_dungeon_instance,
    with_dungeon_runtime_store, DEFAULT_CONTAINER_TYPE_ID,
};
use crate::services::pve_dungeon_runtime_hooks::{
    is_destructible_dungeon_object, is_dungeon_object_ball, on_dungeon_container_cargo_changed,
    on_dungeon_structure_destroyed_sync,
};
use crate::services::pve_exploration::with_pve_exploration_reg;
use eve_db::clock::VirtualServerClock;
use eve_db::{
    DungeonNpcGroup, DungeonRuntimeInstance, DungeonRuntimeObject, DungeonRuntimeRoom,
    DungeonTemplateId, DungeonTrigger, DungeonTriggerAction, DungeonTriggerKind, InventoryFlag,
    InventoryItemRecord, IskCents, ServerClock, WorldRepository,
};
use eve_destiny::{Ball, DamageVector, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::{
    CategoryId, EffectId, GroupId, InvType, SdeRegistry, SolarSystemId, TypeId,
};
use glam::DVec3;
use std::collections::HashMap;
use std::sync::Arc;

fn mk_inv_type(
    type_id: TypeId,
    group_id: GroupId,
    category_id: CategoryId,
    name: &str,
    volume: f64,
    capacity: f64,
    default_effect_id: Option<EffectId>,
    attrs: &[(u16, f64)],
) -> InvType {
    InvType {
        type_id,
        group_id,
        category_id,
        name: name.to_string(),
        mass: 1_000.0,
        volume,
        capacity,
        radius: 100.0,
        base_price: 100.0,
        published: true,
        portion_size: 100,
        market_group_id: None,
        race_id: None,
        faction_id: None,
        variation_parent_type_id: None,
        description_id: None,
        icon_id: None,
        graphic_id: None,
        sound_id: None,
        meta_group_id: None,
        meta_level: None,
        tech_level: None,
        default_effect_id,
        description: String::new(),
        effect_ids: {
            let mut ids: Vec<EffectId> = default_effect_id.into_iter().collect();
            if default_effect_id.is_some() {
                ids.push(12);
            }
            ids
        },
        default_attributes: attrs.iter().copied().collect(),
    }
}

fn build_hooks_test_sde() -> Arc<SdeRegistry> {
    let mut sde = SdeRegistry::with_canonical_seed();
    sde.types.insert(
        23,
        mk_inv_type(
            23,
            12,
            2,
            "Cargo Container",
            1_000.0,
            27_500.0,
            None,
            &[(9, 500.0), (263, 500.0), (265, 500.0), (1556, 27_500.0)],
        ),
    );
    sde.types
        .entry(34)
        .or_insert_with(|| mk_inv_type(34, 18, 4, "Tritanium", 0.01, 0.0, None, &[]));
    sde.types
        .entry(35)
        .or_insert_with(|| mk_inv_type(35, 18, 4, "Pyerite", 0.01, 0.0, None, &[]));
    sde.types.insert(
        16_757,
        mk_inv_type(
            16_757,
            319,
            11,
            "Angel Relay Bunker",
            5_000.0,
            0.0,
            None,
            &[(9, 400.0), (263, 200.0), (265, 200.0)],
        ),
    );
    sde.types.insert(
        52_361,
        mk_inv_type(
            52_361,
            1997,
            11,
            "Xordazh World Ark",
            50_000_000.0,
            0.0,
            None,
            &[(9, 10_000.0), (263, 10_000.0), (265, 10_000.0)],
        ),
    );
    sde.types.insert(
        486,
        mk_inv_type(
            486,
            55,
            7,
            "125mm Gatling AutoCannon I",
            5.0,
            1.0,
            None,
            &[(12, 1.0), (14, 1.0), (50, 1.0), (128, 1.0), (602, 83.0)],
        ),
    );
    sde.types.insert(
        179,
        mk_inv_type(179, 83, 8, "Fusion S", 0.0025, 0.0, None, &[(128, 1.0)]),
    );
    sde.types.insert(
        33_474,
        mk_inv_type(33_474, 1246, 23, "Mobile Depot", 50.0, 3_000.0, None, &[]),
    );
    Arc::new(sde)
}

async fn make_space_pilot_ctx(
    sde: Arc<SdeRegistry>,
    sys_id: SolarSystemId,
    char_id: i64,
    user_tag: &str,
    ship_type_id: TypeId,
    pos: DVec3,
) -> (ServerContext, Arc<VirtualServerClock>, SessionState, i64) {
    let repo = Arc::new(WorldRepository::new_empty());
    repo.provision_gm_account(user_tag, "pw", char_id, user_tag);
    let (uid, ship_id) = repo.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.solar_system_id = sys_id;
        ch.station_id = None;
        ch.structure_id = None;
        let sid = ch.active_ship_id;
        let uid = ch.user_id;
        if let Some(it) = snap.items.get_mut(&sid) {
            it.type_id = ship_type_id;
            it.location_id = i64::from(sys_id);
            it.flag = InventoryFlag::None;
        }
        (uid, sid)
    });
    let _ = repo.acquire_online_lock_with_computer_hash(
        uid,
        char_id as u64,
        Some(char_id),
        None,
        true,
        1_785_100_000,
    );
    let clock = Arc::new(VirtualServerClock::with_fixed_base(1_785_100_000));
    let ctx = ServerContext::with_sde_and_repository(sde, repo);
    ctx.set_clock(Arc::clone(&clock) as Arc<dyn ServerClock>);
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    sys_h
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: ship_type_id,
                position: pos,
                velocity: DVec3::ZERO,
                max_velocity: 2_500.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_ship_combat_state(char_id, ship_id, ship_type_id),
        )
        .await
        .expect("spawn pilot ship ball");
    let mut sess = SessionState::new(char_id as u64);
    sess.user_id = Some(uid);
    sess.character_id = Some(char_id);
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(ship_id);
    (ctx, clock, sess, ship_id)
}

fn bound_invbroker(char_id: i64, object_id: i64) -> BoundObjectEntry {
    BoundObjectEntry {
        bound_id: 1,
        session_id: char_id as u64,
        user_id: char_id as u64,
        character_id: char_id,
        service: "invbroker".into(),
        object_id,
        oid_str: format!("N=1:{object_id}"),
        ref_id: 1,
    }
}

fn make_room(
    room_index: usize,
    room_id: i64,
    pos: [f64; 3],
    objects: Vec<DungeonRuntimeObject>,
    triggers: Vec<DungeonTrigger>,
) -> DungeonRuntimeRoom {
    DungeonRuntimeRoom {
        room_index,
        room_id,
        room_name: format!("Pocket {room_index}"),
        position: pos,
        gate_destination_pos: pos,
        gated: true,
        gate_locked: true,
        objects,
        triggers,
        ..Default::default()
    }
}

fn make_trig(
    trigger_id: u32,
    kind: DungeonTriggerKind,
    action: DungeonTriggerAction,
    repeatable: bool,
) -> DungeonTrigger {
    DungeonTrigger {
        trigger_id,
        kind,
        action,
        repeatable,
        ..Default::default()
    }
}

fn insert_ship_item(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    item_id: i64,
    type_id: u32,
    flag: InventoryFlag,
    qty: i32,
    singleton: bool,
) {
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            item_id,
            InventoryItemRecord {
                item_id,
                type_id,
                owner_id: char_id,
                location_id: ship_id,
                flag,
                quantity: qty,
                singleton,
                version: 1,
                ..Default::default()
            },
        );
    });
}

async fn inv_add_to(
    ctx: &ServerContext,
    sess: &mut SessionState,
    char_id: i64,
    dst_id: i64,
    item_id: i64,
    src_id: i64,
    qty: Option<i64>,
) {
    let b = bound_invbroker(char_id, dst_id);
    let mut kw = HashMap::new();
    if let Some(q) = qty {
        kw.insert("qty".to_string(), PyRep::Integer(q));
    }
    crate::services::inventory_fitting::handle(
        ctx,
        sess,
        Some(&b),
        "invbroker",
        "Add",
        &[
            PyRep::Integer(item_id),
            PyRep::Integer(src_id),
            PyRep::Integer(InventoryFlag::Cargo as i64),
        ],
        &kw,
    )
    .await
    .expect("invbroker.Add handled")
    .expect("invbroker.Add succeeds");
}

async fn inv_multi_add_to(
    ctx: &ServerContext,
    sess: &mut SessionState,
    char_id: i64,
    dst_id: i64,
    item_ids: Vec<i64>,
) {
    let b = bound_invbroker(char_id, dst_id);
    let kw = HashMap::new();
    let list = item_ids.into_iter().map(PyRep::Integer).collect();
    crate::services::inventory_fitting::handle(
        ctx,
        sess,
        Some(&b),
        "invbroker",
        "MultiAdd",
        &[
            PyRep::List(list),
            PyRep::Integer(dst_id),
            PyRep::Integer(InventoryFlag::Cargo as i64),
        ],
        &kw,
    )
    .await
    .expect("invbroker.MultiAdd handled")
    .expect("invbroker.MultiAdd succeeds");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn on_cargo_delivered_anti_exploit_via_invbroker_and_loot_all() {
    let (sys_id, char_id, inst_id) = (30_000_142u32, 90_070_001i64, 995_001i64);
    let r_pos = [50_000_000.0, 0.0, 0.0];
    let (ctx, _clock, mut sess, ship_id) = make_space_pilot_ctx(
        build_hooks_test_sde(),
        sys_id,
        char_id,
        "cargo_pilot",
        587,
        DVec3::new(r_pos[0] + 200.0, r_pos[1], r_pos[2]),
    )
    .await;
    let bal0 = ctx.db.with_snapshot(|s| s.characters[&char_id].balance);

    // Pre-existing 25 units in cont0 MUST NOT satisfy OnCargoDelivered(required_quantity=10).
    let room = make_room(
        1,
        9601,
        r_pos,
        vec![
            DungeonRuntimeObject {
                type_id: DEFAULT_CONTAINER_TYPE_ID,
                name: "Delivery Cache".into(),
                position: r_pos,
                radius: 150.0,
                is_container: true,
                loot_items: vec![(34, 25)],
                ..Default::default()
            },
            DungeonRuntimeObject {
                type_id: DEFAULT_CONTAINER_TYPE_ID,
                name: "Secondary Cache".into(),
                position: [r_pos[0] + 400.0, r_pos[1], r_pos[2]],
                radius: 150.0,
                is_container: true,
                ..Default::default()
            },
        ],
        vec![
            DungeonTrigger {
                trigger_id: 1,
                kind: DungeonTriggerKind::OnCargoDelivered {
                    container_ball_id: None,
                    target_object_index: Some(0),
                    item_type_id: Some(34),
                    required_quantity: 10,
                },
                action: DungeonTriggerAction::UnlockGate,
                extra_actions: vec![DungeonTriggerAction::GrantReward {
                    isk: 100_000,
                    lp: 0,
                    corp_id: 0,
                    faction_id: 0,
                    standing_milli: 0,
                }],
                ..Default::default()
            },
            make_trig(
                2,
                DungeonTriggerKind::OnCargoDelivered {
                    container_ball_id: None,
                    target_object_index: Some(1),
                    item_type_id: Some(34),
                    required_quantity: 6,
                },
                DungeonTriggerAction::TriggerMessage {
                    message_type: 1,
                    message_id: 7701,
                },
                false,
            ),
        ],
    );
    with_dungeon_runtime_store(&ctx, |s| {
        s.instances.insert(
            inst_id,
            DungeonRuntimeInstance {
                instance_id: inst_id,
                dungeon_id: 1,
                solar_system_id: sys_id,
                owner_char_id: char_id,
                base_position: r_pos,
                rooms: vec![room],
                created_unix: 1_785_100_000,
                ..Default::default()
            },
        );
    });
    let mat = materialize_dungeon_room_entities(&ctx, sys_id, inst_id, 0)
        .await
        .expect("materialize room");
    let (cont0_id, cont1_id) = (mat.object_ids[0], mat.object_ids[1]);

    // I1 guard: moving cargo into container BEFORE room.entered == true must NOT accumulate or fire.
    let pre_stack = 7_100_001i64;
    insert_ship_item(&ctx, char_id, ship_id, pre_stack, 34, InventoryFlag::Cargo, 15, false);
    inv_add_to(&ctx, &mut sess, char_id, cont0_id, pre_stack, ship_id, Some(10)).await;
    read_dungeon_runtime_store(&ctx, |s| {
        let r = &s.instances[&inst_id].rooms[0];
        assert!(
            r.gate_locked && r.cargo_delivered_net.is_empty(),
            "I1: pre-entry cargo move must not track or fire"
        );
    });

    on_dungeon_room_entered(&ctx, sys_id, inst_id, 0, char_id).await;
    let (sa, sb, sc) = (7_100_010i64, 7_100_011i64, 7_100_012i64);
    insert_ship_item(&ctx, char_id, ship_id, sa, 34, InventoryFlag::Cargo, 20, false);
    insert_ship_item(&ctx, char_id, ship_id, sb, 34, InventoryFlag::Cargo, 5, false);
    insert_ship_item(&ctx, char_id, ship_id, sc, 34, InventoryFlag::Cargo, 5, false);

    // 1. Deposit 5 -> net = 5 (< 10).
    inv_add_to(&ctx, &mut sess, char_id, cont0_id, sa, ship_id, Some(5)).await;
    read_dungeon_runtime_store(&ctx, |s| {
        let r = &s.instances[&inst_id].rooms[0];
        assert_eq!(r.cargo_delivered_net.get(&format!("{cont0_id}:34")), Some(&5));
        assert!(r.gate_locked);
    });

    // 2. Withdraw 5 -> net = 0.
    let cont_stack_34 = ctx.db.with_snapshot(|snap| {
        snap.items
            .values()
            .find(|it| it.location_id == cont0_id && it.type_id == 34 && it.quantity >= 5)
            .map(|it| it.item_id)
            .unwrap()
    });
    inv_add_to(&ctx, &mut sess, char_id, ship_id, cont_stack_34, cont0_id, Some(5)).await;
    read_dungeon_runtime_store(&ctx, |s| {
        assert_eq!(
            s.instances[&inst_id].rooms[0]
                .cargo_delivered_net
                .get(&format!("{cont0_id}:34")),
            None
        );
    });

    // 3. Re-deposit 5 -> net = 5 (anti-exploit: MUST NOT fire from 5+5 cycling!).
    inv_add_to(&ctx, &mut sess, char_id, cont0_id, sa, ship_id, Some(5)).await;
    read_dungeon_runtime_store(&ctx, |s| {
        let r = &s.instances[&inst_id].rooms[0];
        assert_eq!(r.cargo_delivered_net.get(&format!("{cont0_id}:34")), Some(&5));
        assert!(r.gate_locked, "anti-exploit: net 5 < 10");
    });

    // 4. Deposit 5 via MultiAdd -> net = 10 -> FIRES ONCE!
    inv_multi_add_to(&ctx, &mut sess, char_id, cont0_id, vec![sb]).await;
    read_dungeon_runtime_store(&ctx, |s| {
        let r = &s.instances[&inst_id].rooms[0];
        assert_eq!(r.cargo_delivered_net.get(&format!("{cont0_id}:34")), Some(&10));
        assert!(!r.gate_locked && r.triggers[0].fired);
    });
    assert_eq!(
        ctx.db.with_snapshot(|s| s.characters[&char_id].balance),
        bal0.checked_add(IskCents::from_isk(100_000)).unwrap()
    );

    // 5. Extra deposit does NOT re-fire (I3).
    inv_add_to(&ctx, &mut sess, char_id, cont0_id, sc, ship_id, None).await;
    assert_eq!(
        ctx.db.with_snapshot(|s| s.characters[&char_id].balance),
        bal0.checked_add(IskCents::from_isk(100_000)).unwrap()
    );

    // 6. Verify LootAll decrements cargo_delivered_net when taking items out of cont1.
    let (s1, s2, s3) = (7_100_020i64, 7_100_021i64, 7_100_022i64);
    insert_ship_item(&ctx, char_id, ship_id, s1, 34, InventoryFlag::Cargo, 4, false);
    insert_ship_item(&ctx, char_id, ship_id, s2, 34, InventoryFlag::Cargo, 4, false);
    insert_ship_item(&ctx, char_id, ship_id, s3, 34, InventoryFlag::Cargo, 2, false);
    inv_multi_add_to(&ctx, &mut sess, char_id, cont1_id, vec![s1]).await;
    crate::services::combat_crimewatch::combat_wrecks_killmails::handle_wrecks_killmails(
        &ctx,
        &mut sess,
        "invbroker",
        "LootAll",
        &[PyRep::Integer(cont1_id)],
        &HashMap::new(),
    )
    .await
    .expect("LootAll handled")
    .expect("LootAll drains cont1");
    read_dungeon_runtime_store(&ctx, |s| {
        assert_eq!(
            s.instances[&inst_id].rooms[0]
                .cargo_delivered_net
                .get(&format!("{cont1_id}:34")),
            None
        );
    });
    inv_multi_add_to(&ctx, &mut sess, char_id, cont1_id, vec![s2]).await;
    read_dungeon_runtime_store(&ctx, |s| assert!(!s.instances[&inst_id].rooms[0].triggers[1].fired));
    inv_multi_add_to(&ctx, &mut sess, char_id, cont1_id, vec![s3]).await;
    read_dungeon_runtime_store(&ctx, |s| assert!(s.instances[&inst_id].rooms[0].triggers[1].fired));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn on_structure_destroyed_container_and_prop_via_death_paths() {
    let (sys_id, char_id, inst_id) = (30_000_142u32, 90_070_030i64, 995_004i64);
    let r_pos = [53_000_000.0, 0.0, 0.0];
    let (ctx, _clock, _sess, ship_id) = make_space_pilot_ctx(
        build_hooks_test_sde(),
        sys_id,
        char_id,
        "struct_killer",
        587,
        DVec3::from_array(r_pos),
    )
    .await;
    let bal0 = ctx.db.with_snapshot(|s| s.characters[&char_id].balance);

    let mut room = make_room(
        1,
        9604,
        r_pos,
        vec![
            DungeonRuntimeObject {
                type_id: 16_757,
                name: "Angel Relay Bunker".into(),
                position: [r_pos[0] + 1_200.0, r_pos[1], r_pos[2]],
                radius: 250.0,
                destructible: true,
                tag: Some("relay".into()),
                ..Default::default()
            },
            DungeonRuntimeObject {
                type_id: DEFAULT_CONTAINER_TYPE_ID,
                name: "Volatile Crate".into(),
                position: [r_pos[0] + 1_800.0, r_pos[1], r_pos[2]],
                radius: 150.0,
                is_container: true,
                destructible: true,
                loot_items: vec![(34, 10)],
                ..Default::default()
            },
            DungeonRuntimeObject {
                type_id: 16_757,
                name: "Secondary Tower".into(),
                position: [r_pos[0] + 2_100.0, r_pos[1], r_pos[2]],
                radius: 150.0,
                destructible: true,
                tag: Some("tower".into()),
                ..Default::default()
            },
            DungeonRuntimeObject {
                type_id: DEFAULT_CONTAINER_TYPE_ID,
                name: "Indestructible Beacon".into(),
                position: [r_pos[0] + 2_400.0, r_pos[1], r_pos[2]],
                radius: 150.0,
                destructible: false,
                ..Default::default()
            },
        ],
        vec![
            DungeonTrigger {
                trigger_id: 1,
                kind: DungeonTriggerKind::OnStructureDestroyed {
                    target_ball_id: None,
                    target_object_index: None,
                    target_tag: Some("relay".into()),
                    target_type_id: Some(16_757),
                },
                action: DungeonTriggerAction::UnlockGate,
                extra_actions: vec![DungeonTriggerAction::SpawnNpcGroup(2)],
                ..Default::default()
            },
            make_trig(
                2,
                DungeonTriggerKind::OnStructureDestroyed {
                    target_ball_id: None,
                    target_object_index: Some(1),
                    target_tag: None,
                    target_type_id: None,
                },
                DungeonTriggerAction::GrantReward {
                    isk: 75_000,
                    lp: 0,
                    corp_id: 0,
                    faction_id: 0,
                    standing_milli: 0,
                },
                false,
            ),
            make_trig(
                3,
                DungeonTriggerKind::OnStructureDestroyed {
                    target_ball_id: None,
                    target_object_index: Some(2),
                    target_tag: Some("tower".into()),
                    target_type_id: None,
                },
                DungeonTriggerAction::TriggerMessage {
                    message_type: 1,
                    message_id: 903,
                },
                false,
            ),
        ],
    );
    room.entered = true;
    room.entered_unix = Some(1_785_100_000);
    room.npc_groups = vec![DungeonNpcGroup {
        group_id: 2,
        wave: 2,
        npc_specs: vec![(48_090, 1)],
        spawn_immediately: false,
        ..Default::default()
    }];
    with_dungeon_runtime_store(&ctx, |s| {
        s.instances.insert(
            inst_id,
            DungeonRuntimeInstance {
                instance_id: inst_id,
                dungeon_id: 1,
                solar_system_id: sys_id,
                owner_char_id: char_id,
                base_position: r_pos,
                rooms: vec![room],
                created_unix: 1_785_100_000,
                ..Default::default()
            },
        );
    });
    let mat = materialize_dungeon_room_entities(&ctx, sys_id, inst_id, 0)
        .await
        .expect("materialize room");
    let (relay_id, crate_id, tower_id, beacon_id) = (
        mat.object_ids[0],
        mat.object_ids[1],
        mat.object_ids[2],
        mat.object_ids[3],
    );
    let sys_h = ctx.systems.get_or_spawn(sys_id);

    // Indestructible beacon spawns without tether (`F-03`), is not destructible, and survives death pipeline intact.
    assert!(sys_h.view().contains_ball(beacon_id) && !sys_h.view().tethered_balls.contains(&beacon_id));
    assert!(!is_destructible_dungeon_object(&ctx, sys_id, beacon_id));
    crate::combat::death_pipeline::on_ball_destroyed_sync(
        &ctx,
        sys_id,
        beacon_id,
        Some(char_id),
        DVec3::from_array(r_pos),
    )
    .unwrap();
    assert!(sys_h.view().contains_ball(beacon_id));
    read_dungeon_runtime_store(&ctx, |s| {
        assert!(!s.instances[&inst_id].rooms[0].objects[3].destroyed)
    });

    // 1. Live weapon damage -> npc_death_hook -> death_pipeline -> sync UnlockGate + SpawnNpcGroup.
    assert!(sys_h
        .view()
        .combat_states
        .get(&relay_id)
        .is_some_and(|cs| cs.structure_hp > 0.0));
    let (_, relay_dead, _) = sys_h
        .apply_damage_with_attacker(
            relay_id,
            DamageVector::new(100_000.0, 100_000.0, 100_000.0, 100_000.0),
            Some(char_id),
        )
        .await
        .unwrap()
        .unwrap();
    assert!(relay_dead && !sys_h.view().contains_ball(relay_id));
    let spawned_npc_id = read_dungeon_runtime_store(&ctx, |s| {
        let r = &s.instances[&inst_id].rooms[0];
        assert!(r.objects[0].destroyed && r.triggers[0].fired && !r.gate_locked && r.npc_groups[0].spawned);
        r.npc_groups[0].npc_ball_ids[0]
    });
    assert!(
        sys_h.view().contains_ball(spawned_npc_id)
            && with_combat_runtime(&ctx, |rt| rt.npc_entities.contains_key(&spawned_npc_id))
    );

    // 2. Missile/AOE path destroys destructible container + cleans child items.
    crate::services::combat_crimewatch::combat_missiles_aoe::handle_destroyed_ball_from_missile_or_aoe(
        &ctx,
        sys_id,
        char_id,
        crate_id,
        DEFAULT_CONTAINER_TYPE_ID,
        DVec3::from_array(r_pos),
        ctx.now_unix(),
    )
    .await;
    assert!(!sys_h.view().contains_ball(crate_id));
    assert!(ctx
        .db
        .with_snapshot(|s| !s.items.contains_key(&crate_id) && !s.items.values().any(|i| i.location_id == crate_id)));
    read_dungeon_runtime_store(&ctx, |s| {
        assert!(s.instances[&inst_id].rooms[0].objects[1].destroyed && s.instances[&inst_id].rooms[0].triggers[1].fired)
    });
    assert_eq!(
        ctx.db.with_snapshot(|s| s.characters[&char_id].balance),
        bal0.checked_add(IskCents::from_isk(75_000)).unwrap()
    );

    // 3. Spool/turret destruction path forwards into single death pipeline without bogus wreck.
    let (_, tower_ball) = sys_h.get_ball_snapshot(tower_id).await.unwrap().unwrap();
    let res = crate::services::combat_crimewatch::combat_weapons_spool::handle_ship_or_npc_destruction(
        &ctx,
        &sys_h,
        char_id,
        ship_id,
        587,
        486,
        tower_id,
        &tower_ball,
        None,
        sys_id,
        100.0,
        ctx.now_unix(),
    )
    .await;
    assert_eq!(res.wreck_id, 0);
    assert!(!sys_h.view().contains_ball(tower_id));
    read_dungeon_runtime_store(&ctx, |s| {
        assert!(s.instances[&inst_id].rooms[0].objects[2].destroyed && s.instances[&inst_id].rooms[0].triggers[2].fired)
    });
    assert_eq!(reconcile_and_rematerialize_dungeon_instance(&ctx, sys_id, inst_id).await, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn compound_and_finished_instance_invariants_and_world_ark_exclusion() {
    let (sys_id, char_id, inst_a, inst_b, inst_wk) = (
        30_000_142u32,
        90_070_040i64,
        995_005i64,
        995_006i64,
        995_007i64,
    );
    let r_pos = [54_000_000.0, 0.0, 0.0];
    let (ctx, _clock, _sess, ship_id) = make_space_pilot_ctx(
        build_hooks_test_sde(),
        sys_id,
        char_id,
        "comp_pilot",
        587,
        DVec3::from_array(r_pos),
    )
    .await;

    let mut ra = make_room(
        1,
        9605,
        r_pos,
        vec![
            DungeonRuntimeObject {
                type_id: DEFAULT_CONTAINER_TYPE_ID,
                name: "Sec Console".into(),
                position: r_pos,
                radius: 150.0,
                is_container: true,
                loot_items: vec![(34, 1)],
                ..Default::default()
            },
            DungeonRuntimeObject {
                type_id: 16_757,
                name: "Power Core".into(),
                position: [r_pos[0] + 500.0, r_pos[1], r_pos[2]],
                radius: 150.0,
                destructible: true,
                ..Default::default()
            },
        ],
        vec![make_trig(
            1,
            DungeonTriggerKind::CompoundAnd {
                clauses: vec![
                    DungeonTriggerKind::OnLoot {
                        container_ball_id: None,
                        target_object_index: Some(0),
                        item_type_id: Some(34),
                    },
                    DungeonTriggerKind::OnStructureDestroyed {
                        target_ball_id: None,
                        target_object_index: Some(1),
                        target_tag: None,
                        target_type_id: None,
                    },
                ],
            },
            DungeonTriggerAction::UnlockGate,
            false,
        )],
    );
    ra.entered = true;
    ra.entered_unix = Some(1_785_100_000);
    with_dungeon_runtime_store(&ctx, |s| {
        s.instances.insert(
            inst_a,
            DungeonRuntimeInstance {
                instance_id: inst_a,
                dungeon_id: 1,
                solar_system_id: sys_id,
                owner_char_id: char_id,
                base_position: r_pos,
                rooms: vec![ra],
                created_unix: 1_785_100_000,
                ..Default::default()
            },
        );
    });
    let mat_a = materialize_dungeon_room_entities(&ctx, sys_id, inst_a, 0)
        .await
        .unwrap();
    let (cont_a, core_a) = (mat_a.object_ids[0], mat_a.object_ids[1]);

    on_dungeon_item_looted(&ctx, char_id, Some(sys_id), cont_a, 34).await;
    read_dungeon_runtime_store(&ctx, |s| assert!(s.instances[&inst_a].rooms[0].gate_locked));

    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_h
        .apply_damage_with_attacker(
            core_a,
            DamageVector::new(100_000.0, 100_000.0, 100_000.0, 100_000.0),
            Some(char_id),
        )
        .await
        .unwrap()
        .unwrap();
    read_dungeon_runtime_store(&ctx, |s| {
        let r = &s.instances[&inst_a].rooms[0];
        assert!(!r.gate_locked && r.triggers[0].fired);
    });

    // Flashpoint World Ark NPC (category_id == 11, destructible == false, is_container == false)
    // stored in room.objects must NEVER be treated as an indestructible dungeon prop object.
    let ark_ball_id = 8_880_099i64;
    let rw = make_room(
        1,
        9607,
        r_pos,
        vec![DungeonRuntimeObject {
            ball_id: ark_ball_id,
            type_id: 52_361,
            name: "Xordazh World Ark".into(),
            position: r_pos,
            radius: 15_000.0,
            is_container: false,
            destructible: false,
            spawned: true,
            ..Default::default()
        }],
        vec![],
    );
    with_dungeon_runtime_store(&ctx, |s| {
        s.instances.insert(
            inst_wk,
            DungeonRuntimeInstance {
                instance_id: inst_wk,
                dungeon_id: 1,
                category: "Flashpoint".into(),
                solar_system_id: sys_id,
                owner_char_id: char_id,
                rooms: vec![rw],
                ..Default::default()
            },
        );
    });
    assert!(!is_destructible_dungeon_object(&ctx, sys_id, ark_ball_id));
    assert!(!is_dungeon_object_ball(&ctx, sys_id, ark_ball_id));

    // I2 + read-purity check: finished instance and unrelated IDs perform 0 DB write tx.
    let mut rb = make_room(
        1,
        9606,
        r_pos,
        vec![
            DungeonRuntimeObject {
                ball_id: 8_880_001,
                type_id: DEFAULT_CONTAINER_TYPE_ID,
                is_container: true,
                spawned: true,
                ..Default::default()
            },
            DungeonRuntimeObject {
                ball_id: 8_880_003,
                type_id: 16_757,
                destructible: true,
                spawned: true,
                ..Default::default()
            },
        ],
        vec![],
    );
    rb.entered = true;
    with_dungeon_runtime_store(&ctx, |s| {
        s.instances.insert(
            inst_b,
            DungeonRuntimeInstance {
                instance_id: inst_b,
                dungeon_id: 1,
                solar_system_id: sys_id,
                owner_char_id: char_id,
                completed: true,
                rooms: vec![rb],
                ..Default::default()
            },
        );
    });
    let tx0 = ctx.db.tx_commit_count();
    on_dungeon_container_cargo_changed(&ctx, char_id, Some(sys_id), ship_id, 8_880_001, 34, 10)
        .await;
    assert!(!on_dungeon_structure_destroyed_sync(
        &ctx, &sys_h, sys_id, 8_880_003, char_id
    ));
    on_dungeon_container_cargo_changed(&ctx, char_id, Some(sys_id), ship_id, 9_999_999, 34, 10)
        .await;
    assert_eq!(
        ctx.db.tx_commit_count(),
        tx0,
        "I2 & fast-skip must do zero write transactions"
    );
}

#[test]
fn wiki_content_trigger_kinds_and_object_classification_round_trip() {
    let sde = build_hooks_test_sde();
    let ctx = ServerContext::with_sde_and_repository(
        Arc::clone(&sde),
        Arc::new(WorldRepository::new_empty()),
    );
    let did = 995_101u32;
    let payload = format!(
        r#"{{
  "header": {{"schema_version": 2, "source": "test", "license": "CC BY-SA 4.0"}},
  "entries": [
    {{
      "entry_id": "cargo_and_structure_triggers",
      "base_title": "Industrial Delivery & Relay Complex",
      "sde_matches": {{"dungeon_ids": [{did}], "mission_ids": []}},
      "rooms": [{{
        "room_index": 1, "room_name": "Chamber", "has_acceleration_gate": true,
        "waves": [{{ "wave_index": 1, "timing": "static_structures", "spawns": [
          {{ "raw_class": "Container", "raw_name": "Supply Hopper", "count": {{"min": 1, "max": 1}}, "resolution": {{"status": "resolved_exact", "type_ids": [23], "candidate_type_ids": [], "had_ellipsis": false}}, "trigger": {{"kind": "cargo_delivered", "item_type_id": 34, "required_quantity": 12, "action": {{"kind": "trigger_message", "message_type": 1, "message_id": 701}}}} }},
          {{ "raw_class": "Structure", "raw_name": "Relay Tower", "tag": "bunker", "destructible": true, "count": {{"min": 1, "max": 1}}, "resolution": {{"status": "resolved_exact", "type_ids": [16757], "candidate_type_ids": [], "had_ellipsis": false}}, "trigger": {{"kind": "structure_destroyed", "target_tag": "bunker", "unlocks_gate": true}} }}
        ] }}]
      }}]
    }},
    {{ "entry_id": "bad_can_trigger", "base_title": "Rejected Can", "sde_matches": {{"dungeon_ids": [995102], "mission_ids": []}}, "rooms": [{{ "room_index": 1, "waves": [{{ "wave_index": 1, "timing": "sequential_wave", "wave_trigger": {{ "kind": "can_hacked" }}, "spawns": [] }}] }}] }},
    {{ "entry_id": "bad_ore_trigger", "base_title": "Rejected Ore", "sde_matches": {{"dungeon_ids": [995103], "mission_ids": []}}, "rooms": [{{ "room_index": 1, "waves": [{{ "wave_index": 1, "timing": "sequential_wave", "wave_trigger": {{ "kind": "ore_depleted" }}, "spawns": [] }}] }}] }},
    {{ "entry_id": "bad_future_trigger", "base_title": "Rejected Unknown", "sde_matches": {{"dungeon_ids": [995104], "mission_ids": []}}, "rooms": [{{ "room_index": 1, "waves": [{{ "wave_index": 1, "timing": "sequential_wave", "wave_trigger": {{ "kind": "quantum_warp_storm" }}, "spawns": [] }}] }}] }}
  ]
}}"#
    );

    let cat = Arc::new(parse_and_validate_wiki_pve_json(&payload).expect("valid wiki v2 json"));
    assert_eq!(cat.entries.len(), 1);
    assert_eq!(cat.stats.rejected_unsupported_trigger_entries, 3);
    assert_eq!(cat.stats.rejected_by_trigger_kind.get("can_hacked"), Some(&1));
    assert_eq!(cat.stats.rejected_by_trigger_kind.get("ore_depleted"), Some(&1));
    assert_eq!(cat.stats.rejected_by_trigger_kind.get("quantum_warp_storm"), Some(&1));

    install_wiki_pve_catalog_for_test(&ctx, cat);
    let built = build_wiki_deadspace_rooms(
        &ctx,
        30_000_142,
        DungeonTemplateId::new(did),
        "Industrial Delivery & Relay Complex",
        [10_000_000.0, 0.0, 0.0],
        false,
    )
    .expect("build wiki rooms");
    let r = &built.rooms[0];
    assert_eq!(r.objects.len(), 2);
    assert!(r.objects[0].is_container && !r.objects[0].destructible);
    assert!(
        r.objects[1].destructible
            && !r.objects[1].is_container
            && r.objects[1].tag.as_deref() == Some("bunker")
    );

    assert!(r.triggers.iter().any(|t| matches!(
        t.kind,
        DungeonTriggerKind::OnCargoDelivered {
            item_type_id: Some(34),
            required_quantity: 12,
            target_object_index: Some(0),
            ..
        }
    )));
    assert!(r.triggers.iter().any(|t| matches!(
        (&t.kind, &t.action),
        (
            DungeonTriggerKind::OnStructureDestroyed {
                target_tag: Some(tag),
                ..
            },
            DungeonTriggerAction::CompleteDungeon
        ) if tag == "bunker"
    )));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn on_cargo_delivered_preexisting_loot_and_compound_withdraw_anti_exploit() {
    let (sys_id, char_id, inst_id, inst_comp) =
        (30_000_142u32, 90_070_101i64, 995_110i64, 995_111i64);
    let r_pos = [55_000_000.0, 0.0, 0.0];
    let (ctx, _clock, mut sess, ship_id) = make_space_pilot_ctx(
        build_hooks_test_sde(),
        sys_id,
        char_id,
        "cargo_anti_exploit",
        587,
        DVec3::new(r_pos[0] + 200.0, r_pos[1], r_pos[2]),
    )
    .await;
    let bal0 = ctx.db.with_snapshot(|s| s.characters[&char_id].balance);

    // Trigger uses item_type_id: None so any signed negative pre-existing balance offsets other deposits too.
    let mut room = make_room(
        1,
        9610,
        r_pos,
        vec![DungeonRuntimeObject {
            type_id: DEFAULT_CONTAINER_TYPE_ID,
            name: "Prestocked Cache".into(),
            position: r_pos,
            radius: 150.0,
            is_container: true,
            loot_items: vec![(34, 25)],
            ..Default::default()
        }],
        vec![DungeonTrigger {
            trigger_id: 1,
            kind: DungeonTriggerKind::OnCargoDelivered {
                container_ball_id: None,
                target_object_index: Some(0),
                item_type_id: None,
                required_quantity: 10,
            },
            action: DungeonTriggerAction::UnlockGate,
            extra_actions: vec![DungeonTriggerAction::GrantReward {
                isk: 75_000,
                lp: 0,
                corp_id: 0,
                faction_id: 0,
                standing_milli: 0,
            }],
            ..Default::default()
        }],
    );
    room.entered = true;
    room.entered_unix = Some(1_785_100_000);
    with_dungeon_runtime_store(&ctx, |s| {
        s.instances.insert(
            inst_id,
            DungeonRuntimeInstance {
                instance_id: inst_id,
                dungeon_id: 1,
                solar_system_id: sys_id,
                owner_char_id: char_id,
                base_position: r_pos,
                rooms: vec![room],
                created_unix: 1_785_100_000,
                ..Default::default()
            },
        );
    });
    let mat = materialize_dungeon_room_entities(&ctx, sys_id, inst_id, 0)
        .await
        .expect("materialize room");
    let cont_id = mat.object_ids[0];

    // Withdraw all 25 pre-existing Tritanium via LootAll -> net MUST become -25 (never clamped to 0!).
    crate::services::combat_crimewatch::combat_wrecks_killmails::handle_wrecks_killmails(
        &ctx,
        &mut sess,
        "invbroker",
        "LootAll",
        &[PyRep::Integer(cont_id)],
        &HashMap::new(),
    )
    .await
    .expect("LootAll handled")
    .expect("LootAll succeeds");
    read_dungeon_runtime_store(&ctx, |s| {
        assert_eq!(
            s.instances[&inst_id].rooms[0]
                .cargo_delivered_net
                .get(&format!("{cont_id}:34")),
            Some(&-25)
        );
    });

    // Re-deposit the exact 25 withdrawn units -> net returns to 0 (key removed), MUST NOT fire!
    let withdrawn_stack = ctx.db.with_snapshot(|snap| {
        snap.items
            .values()
            .find(|it| it.location_id == ship_id && it.type_id == 34 && it.quantity == 25)
            .map(|it| it.item_id)
            .unwrap()
    });
    inv_add_to(
        &ctx,
        &mut sess,
        char_id,
        cont_id,
        withdrawn_stack,
        ship_id,
        Some(25),
    )
    .await;
    read_dungeon_runtime_store(&ctx, |s| {
        let r = &s.instances[&inst_id].rooms[0];
        assert_eq!(r.cargo_delivered_net.get(&format!("{cont_id}:34")), None);
        assert!(r.gate_locked && !r.triggers[0].fired);
    });
    assert_eq!(ctx.db.with_snapshot(|s| s.characters[&char_id].balance), bal0);

    // Also test signed sum when item_type_id=None: take 25 again (-25 on type 34) + deposit 25 of type 35 -> net sum 0 < 10.
    crate::services::combat_crimewatch::combat_wrecks_killmails::handle_wrecks_killmails(
        &ctx,
        &mut sess,
        "invbroker",
        "LootAll",
        &[PyRep::Integer(cont_id)],
        &HashMap::new(),
    )
    .await
    .expect("LootAll 2")
    .expect("LootAll 2 ok");
    insert_ship_item(&ctx, char_id, ship_id, 7_500_001, 35, InventoryFlag::Cargo, 25, false);
    inv_add_to(&ctx, &mut sess, char_id, cont_id, 7_500_001, ship_id, Some(25)).await;
    read_dungeon_runtime_store(&ctx, |s| {
        let r = &s.instances[&inst_id].rooms[0];
        assert!(
            r.gate_locked && !r.triggers[0].fired,
            "negative net on type 34 must offset type 35 deposit"
        );
    });

    // Deliver genuine +15 external units -> total signed net = 15 >= 10 -> FIRES!
    insert_ship_item(&ctx, char_id, ship_id, 7_500_002, 34, InventoryFlag::Cargo, 15, false);
    inv_add_to(&ctx, &mut sess, char_id, cont_id, 7_500_002, ship_id, Some(15)).await;
    read_dungeon_runtime_store(&ctx, |s| {
        let r = &s.instances[&inst_id].rooms[0];
        assert!(!r.gate_locked && r.triggers[0].fired);
    });

    // CompoundAnd([OnCargoDelivered, OnStructureDestroyed]): withdrawing cargo MUST clear the __and latch.
    let mut rc = make_room(
        1,
        9611,
        [r_pos[0] + 5_000.0, 0.0, 0.0],
        vec![
            DungeonRuntimeObject {
                type_id: DEFAULT_CONTAINER_TYPE_ID,
                name: "Relay Box".into(),
                position: [r_pos[0] + 5_000.0, 0.0, 0.0],
                radius: 150.0,
                is_container: true,
                ..Default::default()
            },
            DungeonRuntimeObject {
                type_id: 16_757,
                name: "Relay Core".into(),
                position: [r_pos[0] + 5_300.0, 0.0, 0.0],
                radius: 150.0,
                destructible: true,
                tag: Some("core".into()),
                ..Default::default()
            },
        ],
        vec![make_trig(
            1,
            DungeonTriggerKind::CompoundAnd {
                clauses: vec![
                    DungeonTriggerKind::OnCargoDelivered {
                        container_ball_id: None,
                        target_object_index: Some(0),
                        item_type_id: Some(34),
                        required_quantity: 10,
                    },
                    DungeonTriggerKind::OnStructureDestroyed {
                        target_ball_id: None,
                        target_object_index: None,
                        target_tag: Some("core".into()),
                        target_type_id: None,
                    },
                ],
            },
            DungeonTriggerAction::UnlockGate,
            false,
        )],
    );
    rc.entered = true;
    rc.entered_unix = Some(1_785_100_000);
    with_dungeon_runtime_store(&ctx, |s| {
        s.instances.insert(
            inst_comp,
            DungeonRuntimeInstance {
                instance_id: inst_comp,
                dungeon_id: 1,
                solar_system_id: sys_id,
                owner_char_id: char_id,
                base_position: [r_pos[0] + 5_000.0, 0.0, 0.0],
                rooms: vec![rc],
                created_unix: 1_785_100_000,
                ..Default::default()
            },
        );
    });
    let mat_c = materialize_dungeon_room_entities(&ctx, sys_id, inst_comp, 0)
        .await
        .expect("mat comp");
    let (comp_cont, comp_core) = (mat_c.object_ids[0], mat_c.object_ids[1]);
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    assert!(sys_h
        .set_ball_position(ship_id, DVec3::new(r_pos[0] + 5_100.0, 0.0, 0.0))
        .await);
    insert_ship_item(&ctx, char_id, ship_id, 7_500_010, 34, InventoryFlag::Cargo, 10, false);
    inv_add_to(&ctx, &mut sess, char_id, comp_cont, 7_500_010, ship_id, Some(10)).await;
    // Withdraw the 10 units right back -> CompoundAnd latch MUST clear!
    let cstack = ctx.db.with_snapshot(|snap| {
        snap.items
            .values()
            .find(|i| i.location_id == comp_cont && i.type_id == 34)
            .unwrap()
            .item_id
    });
    inv_add_to(&ctx, &mut sess, char_id, ship_id, cstack, comp_cont, Some(10)).await;
    // Destroy structure while cargo is absent -> MUST NOT fire!
    let _ = crate::combat::death_pipeline::on_ball_destroyed_sync(
        &ctx,
        sys_id,
        comp_core,
        Some(char_id),
        DVec3::ZERO,
    )
    .unwrap();
    read_dungeon_runtime_store(&ctx, |s| {
        let r = &s.instances[&inst_comp].rooms[0];
        assert!(
            r.gate_locked && !r.triggers[0].fired,
            "CompoundAnd must not stay latched after cargo withdrawal"
        );
    });
    // Now re-deposit genuine 10 units -> BOTH clauses hold (core already destroyed, cargo present) -> FIRES!
    insert_ship_item(&ctx, char_id, ship_id, 7_500_011, 34, InventoryFlag::Cargo, 10, false);
    inv_add_to(&ctx, &mut sess, char_id, comp_cont, 7_500_011, ship_id, Some(10)).await;
    read_dungeon_runtime_store(&ctx, |s| {
        let r = &s.instances[&inst_comp].rooms[0];
        assert!(!r.gate_locked && r.triggers[0].fired);
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn on_structure_destroyed_pre_entry_replay_and_escalation() {
    let (sys_id, char_id, inst_sync, inst_async) =
        (30_000_142u32, 90_070_104i64, 995_114i64, 995_115i64);
    let r_pos = [58_000_000.0, 0.0, 0.0];
    let mut sde_mut = (*build_hooks_test_sde()).clone();
    let esc_did = 995_199u32;
    sde_mut.dungeons.insert(
        esc_did,
        eve_sde::DungeonTemplate {
            dungeon_id: esc_did,
            name: "Bunker Escalation Site".into(),
            faction_id: Some(500_010),
            archetype_id: Some(20),
            difficulty: Some(2),
        },
    );
    let (ctx, _clock, _sess, _ship_id) = make_space_pilot_ctx(
        Arc::new(sde_mut),
        sys_id,
        char_id,
        "esc_struct_pilot",
        587,
        DVec3::from_array(r_pos),
    )
    .await;

    // 1. Pre-entry destruction of bunker0 in unentered room MUST replay on room entry + flush escalation.
    let room0 = make_room(
        1,
        9614,
        r_pos,
        vec![DungeonRuntimeObject {
            type_id: 16_757,
            name: "Relay Alpha".into(),
            position: [r_pos[0] + 400.0, 0.0, 0.0],
            radius: 150.0,
            destructible: true,
            tag: Some("alpha_bunker".into()),
            ..Default::default()
        }],
        vec![DungeonTrigger {
            trigger_id: 1,
            kind: DungeonTriggerKind::OnStructureDestroyed {
                target_ball_id: None,
                target_object_index: None,
                target_tag: Some("alpha_bunker".into()),
                target_type_id: None,
            },
            action: DungeonTriggerAction::UnlockGate,
            extra_actions: vec![DungeonTriggerAction::StartEscalation {
                target_dungeon_id: Some(esc_did),
                chance_bps: 10_000,
                target_title: Some("Phase 2 Citadel".into()),
            }],
            ..Default::default()
        }],
    );
    with_dungeon_runtime_store(&ctx, |s| {
        s.instances.insert(
            inst_sync,
            DungeonRuntimeInstance {
                instance_id: inst_sync,
                dungeon_id: 1,
                solar_system_id: sys_id,
                owner_char_id: char_id,
                base_position: r_pos,
                rooms: vec![room0],
                created_unix: 1_785_100_000,
                ..Default::default()
            },
        );
    });
    let mat0 = materialize_dungeon_room_entities(&ctx, sys_id, inst_sync, 0)
        .await
        .unwrap();
    let bunker0 = mat0.object_ids[0];
    let _ = crate::combat::death_pipeline::on_ball_destroyed_sync(
        &ctx,
        sys_id,
        bunker0,
        Some(char_id),
        DVec3::ZERO,
    )
    .unwrap();
    read_dungeon_runtime_store(&ctx, |s| {
        let r = &s.instances[&inst_sync].rooms[0];
        assert!(r.objects[0].destroyed && r.gate_locked && !r.triggers[0].fired);
    });

    on_dungeon_room_entered(&ctx, sys_id, inst_sync, 0, char_id).await;
    read_dungeon_runtime_store(&ctx, |s| {
        let r = &s.instances[&inst_sync].rooms[0];
        assert!(
            !r.gate_locked
                && r.triggers[0].fired
                && s.instances[&inst_sync].escalation_instance_id.is_some()
        );
    });
    with_pve_exploration_reg(&ctx, |reg| {
        assert_eq!(reg.escalations_by_char[&char_id].len(), 1);
    });

    // 2. Sync death flush in entered room enqueues pending_escalations and tick_background_dungeon_runtime drains it.
    let inst_bg = 995_116i64;
    let mut room2 = make_room(
        1,
        9616,
        [r_pos[0] + 12_000.0, 0.0, 0.0],
        vec![DungeonRuntimeObject {
            type_id: 16_757,
            name: "Relay Gamma".into(),
            position: [r_pos[0] + 12_400.0, 0.0, 0.0],
            radius: 150.0,
            destructible: true,
            tag: Some("gamma_bunker".into()),
            ..Default::default()
        }],
        vec![make_trig(
            1,
            DungeonTriggerKind::OnStructureDestroyed {
                target_ball_id: None,
                target_object_index: None,
                target_tag: Some("gamma_bunker".into()),
                target_type_id: None,
            },
            DungeonTriggerAction::StartEscalation {
                target_dungeon_id: Some(esc_did),
                chance_bps: 10_000,
                target_title: Some("Phase 2b Citadel".into()),
            },
            false,
        )],
    );
    room2.entered = true;
    room2.entered_unix = Some(1_785_100_000);
    with_dungeon_runtime_store(&ctx, |s| {
        s.instances.insert(
            inst_bg,
            DungeonRuntimeInstance {
                instance_id: inst_bg,
                dungeon_id: 1,
                solar_system_id: sys_id,
                owner_char_id: char_id,
                base_position: [r_pos[0] + 12_000.0, 0.0, 0.0],
                rooms: vec![room2],
                created_unix: 1_785_100_000,
                ..Default::default()
            },
        );
    });
    let mat2 = materialize_dungeon_room_entities(&ctx, sys_id, inst_bg, 0)
        .await
        .unwrap();
    let bunker2 = mat2.object_ids[0];
    let _ = crate::combat::death_pipeline::on_ball_destroyed_sync(
        &ctx,
        sys_id,
        bunker2,
        Some(char_id),
        DVec3::ZERO,
    )
    .unwrap();
    read_dungeon_runtime_store(&ctx, |s| assert_eq!(s.pending_escalations.len(), 1));
    crate::services::pve_dungeon_runtime::tick_background_dungeon_runtime(&ctx).await;
    read_dungeon_runtime_store(&ctx, |s| {
        assert!(s.pending_escalations.is_empty());
        assert!(s.instances[&inst_bg].escalation_instance_id.is_some());
    });
    with_pve_exploration_reg(&ctx, |reg| {
        assert_eq!(reg.escalations_by_char[&char_id].len(), 2);
    });

    // 3. Async `on_ball_destroyed` flushes inline without waiting for tick.
    let mut room1 = make_room(
        1,
        9615,
        [r_pos[0] + 8_000.0, 0.0, 0.0],
        vec![DungeonRuntimeObject {
            type_id: 16_757,
            name: "Relay Beta".into(),
            position: [r_pos[0] + 8_400.0, 0.0, 0.0],
            radius: 150.0,
            destructible: true,
            tag: Some("beta_bunker".into()),
            ..Default::default()
        }],
        vec![make_trig(
            1,
            DungeonTriggerKind::OnStructureDestroyed {
                target_ball_id: None,
                target_object_index: None,
                target_tag: Some("beta_bunker".into()),
                target_type_id: None,
            },
            DungeonTriggerAction::StartEscalation {
                target_dungeon_id: Some(esc_did),
                chance_bps: 10_000,
                target_title: Some("Phase 3 Citadel".into()),
            },
            false,
        )],
    );
    room1.entered = true;
    room1.entered_unix = Some(1_785_100_000);
    with_dungeon_runtime_store(&ctx, |s| {
        s.instances.insert(
            inst_async,
            DungeonRuntimeInstance {
                instance_id: inst_async,
                dungeon_id: 1,
                solar_system_id: sys_id,
                owner_char_id: char_id,
                base_position: [r_pos[0] + 8_000.0, 0.0, 0.0],
                rooms: vec![room1],
                created_unix: 1_785_100_000,
                ..Default::default()
            },
        );
    });
    let mat1 = materialize_dungeon_room_entities(&ctx, sys_id, inst_async, 0)
        .await
        .unwrap();
    let bunker1 = mat1.object_ids[0];
    let _ = crate::combat::death_pipeline::on_ball_destroyed(
        &ctx,
        sys_id,
        bunker1,
        Some(char_id),
        DVec3::ZERO,
    )
    .await
    .unwrap();
    read_dungeon_runtime_store(&ctx, |s| {
        assert!(s.pending_escalations.is_empty());
        assert!(s.instances[&inst_async].rooms[0].triggers[0].fired);
        assert!(s.instances[&inst_async].escalation_instance_id.is_some());
    });
    with_pve_exploration_reg(&ctx, |reg| {
        assert_eq!(reg.escalations_by_char[&char_id].len(), 3);
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bf1_live_damage_followed_by_weapon_and_missile_handlers_no_wreck_loot_km_and_inline_esc() {
    let (sys_id, char_id, inst_id) = (30_000_142u32, 90_070_201i64, 995_201i64);
    let r_pos = [60_000_000.0, 0.0, 0.0];
    let mut sde_mut = (*build_hooks_test_sde()).clone();
    let esc_did = 995_299u32;
    sde_mut.dungeons.insert(
        esc_did,
        eve_sde::DungeonTemplate {
            dungeon_id: esc_did,
            name: "Relay Escalation Site".into(),
            faction_id: Some(500_010),
            archetype_id: Some(20),
            difficulty: Some(2),
        },
    );
    let (ctx, _clock, _sess, ship_id) = make_space_pilot_ctx(
        Arc::new(sde_mut),
        sys_id,
        char_id,
        "bf1_combat_pilot",
        587,
        DVec3::from_array(r_pos),
    )
    .await;

    let mut room = make_room(
        1,
        9620,
        r_pos,
        vec![
            DungeonRuntimeObject {
                type_id: 16_757,
                name: "Bunker Turret Target".into(),
                position: [r_pos[0] + 600.0, 0.0, 0.0],
                radius: 200.0,
                destructible: true,
                tag: Some("bunker_t".into()),
                ..Default::default()
            },
            DungeonRuntimeObject {
                type_id: 16_757,
                name: "Bunker Missile Target".into(),
                position: [r_pos[0] + 1_200.0, 0.0, 0.0],
                radius: 200.0,
                is_container: true,
                destructible: true,
                tag: Some("bunker_m".into()),
                ..Default::default()
            },
        ],
        vec![
            make_trig(
                1,
                DungeonTriggerKind::OnStructureDestroyed {
                    target_ball_id: None,
                    target_object_index: None,
                    target_tag: Some("bunker_t".into()),
                    target_type_id: Some(16_757),
                },
                DungeonTriggerAction::StartEscalation {
                    target_dungeon_id: Some(esc_did),
                    chance_bps: 10_000,
                    target_title: Some("Turret Escalation".into()),
                },
                false,
            ),
            make_trig(
                2,
                DungeonTriggerKind::OnStructureDestroyed {
                    target_ball_id: None,
                    target_object_index: None,
                    target_tag: Some("bunker_m".into()),
                    target_type_id: Some(16_757),
                },
                DungeonTriggerAction::StartEscalation {
                    target_dungeon_id: Some(esc_did),
                    chance_bps: 10_000,
                    target_title: Some("Missile Escalation".into()),
                },
                false,
            ),
        ],
    );
    room.entered = true;
    room.entered_unix = Some(1_785_100_000);
    with_dungeon_runtime_store(&ctx, |s| {
        s.instances.insert(
            inst_id,
            DungeonRuntimeInstance {
                instance_id: inst_id,
                dungeon_id: 1,
                solar_system_id: sys_id,
                owner_char_id: char_id,
                base_position: r_pos,
                rooms: vec![room],
                created_unix: 1_785_100_000,
                ..Default::default()
            },
        );
    });
    let mat = materialize_dungeon_room_entities(&ctx, sys_id, inst_id, 0)
        .await
        .expect("materialize room");
    let (bunker_t, bunker_m) = (mat.object_ids[0], mat.object_ids[1]);

    // Seed nested grandchild item & container_lifetime entry on bunker_m (Note #2 hygiene).
    let (outer_pack_id, inner_ore_id) = (7_800_001i64, 7_800_002i64);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            outer_pack_id,
            InventoryItemRecord {
                item_id: outer_pack_id,
                type_id: DEFAULT_CONTAINER_TYPE_ID,
                owner_id: char_id,
                location_id: bunker_m,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            inner_ore_id,
            InventoryItemRecord {
                item_id: inner_ore_id,
                type_id: 34,
                owner_id: char_id,
                location_id: outer_pack_id,
                flag: InventoryFlag::Cargo,
                quantity: 100,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });
    crate::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| {
        rt.container_lifetimes.insert(
            bunker_m,
            crate::services::combat_crimewatch::ContainerLifetimeEntry {
                item_id: bunker_m,
                solar_system_id: sys_id,
                type_id: 16_757,
                owner_id: char_id,
                created_unix: 1_785_100_000,
                lifetime_secs: 7_200,
                is_abandoned: false,
            },
        );
    });

    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let (_, turret_ball) = sys_h.get_ball_snapshot(bunker_t).await.unwrap().unwrap();
    let (_, missile_ball) = sys_h.get_ball_snapshot(bunker_m).await.unwrap().unwrap();
    let lethal = DamageVector::new(100_000.0, 100_000.0, 100_000.0, 100_000.0);

    // 1. Turret path: apply_damage_with_attacker (triggers npc_death_hook) THEN handle_ship_or_npc_destruction.
    let (_, dead_t, _) = sys_h
        .apply_damage_with_attacker(bunker_t, lethal, Some(char_id))
        .await
        .unwrap()
        .unwrap();
    assert!(dead_t);
    let turret_res =
        crate::services::combat_crimewatch::combat_weapons_spool::handle_ship_or_npc_destruction(
            &ctx,
            &sys_h,
            char_id,
            ship_id,
            587,
            486,
            bunker_t,
            &turret_ball,
            None,
            sys_id,
            100.0,
            ctx.now_unix(),
        )
        .await;
    assert_eq!(turret_res.wreck_id, 0, "BF-1: no bogus NPC wreck");
    assert_eq!(turret_res.kill_id, 0, "BF-1: no bogus killmail");
    ctx.db.with_snapshot(|s| {
        assert!(s.killmails.is_empty());
        assert!(s.standing_transactions.is_empty());
        assert!(s.dungeon_runtime.pending_escalations.is_empty());
        assert!(s.dungeon_runtime.instances[&inst_id].rooms[0].triggers[0].fired);
        assert!(
            !s.items.values().any(|i| i.location_id == i64::from(sys_id) && i.item_id != ship_id && i.item_id != bunker_m),
            "BF-1: no bogus NPC wreck item spawned in space"
        );
    });
    with_pve_exploration_reg(&ctx, |reg| {
        assert_eq!(reg.escalations_by_char[&char_id].len(), 1);
    });

    // 2. Missile/AOE path: apply_damage_with_attacker THEN handle_destroyed_ball_from_missile_or_aoe.
    let (_, dead_m, _) = sys_h
        .apply_damage_with_attacker(bunker_m, lethal, Some(char_id))
        .await
        .unwrap()
        .unwrap();
    assert!(dead_m);
    crate::services::combat_crimewatch::combat_missiles_aoe::handle_destroyed_ball_from_missile_or_aoe(
        &ctx,
        sys_id,
        char_id,
        bunker_m,
        16_757,
        missile_ball.position,
        ctx.now_unix(),
    )
    .await;
    ctx.db.with_snapshot(|s| {
        assert!(s.killmails.is_empty());
        assert!(s.standing_transactions.is_empty());
        assert!(s.dungeon_runtime.pending_escalations.is_empty());
        assert!(!s.items.contains_key(&bunker_m));
        assert!(!s.items.contains_key(&outer_pack_id));
        assert!(!s.items.contains_key(&inner_ore_id), "Note #2: grandchild item cleaned up");
        assert!(
            !s.items.values().any(|i| i.location_id == i64::from(sys_id) && i.item_id != ship_id),
            "BF-1: no bogus NPC wreck from missile/AOE path"
        );
    });
    assert!(!with_combat_runtime(&ctx, |rt| rt.container_lifetimes.contains_key(&bunker_m)));
    with_pve_exploration_reg(&ctx, |reg| {
        assert_eq!(reg.escalations_by_char[&char_id].len(), 2);
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bf2_compound_and_with_compound_or_on_cargo_delivered_clears_latch_on_withdrawal() {
    let (sys_id, char_id, inst_a, inst_b) =
        (30_000_142u32, 90_070_202i64, 995_202i64, 995_203i64);
    let r_pos = [61_000_000.0, 0.0, 0.0];
    let (ctx, _clock, mut sess, ship_id) = make_space_pilot_ctx(
        build_hooks_test_sde(),
        sys_id,
        char_id,
        "bf2_compound_or",
        587,
        DVec3::from_array(r_pos),
    )
    .await;

    let build_inst = |iid: i64, rid: i64, offset_x: f64| {
        let pos = [r_pos[0] + offset_x, 0.0, 0.0];
        let mut room = make_room(
            1,
            rid,
            pos,
            vec![
                DungeonRuntimeObject {
                    type_id: DEFAULT_CONTAINER_TYPE_ID,
                    name: "Or Cargo Box".into(),
                    position: pos,
                    radius: 150.0,
                    is_container: true,
                    ..Default::default()
                },
                DungeonRuntimeObject {
                    type_id: 16_757,
                    name: "Or Target Bunker".into(),
                    position: [pos[0] + 400.0, 0.0, 0.0],
                    radius: 150.0,
                    destructible: true,
                    tag: Some("or_core".into()),
                    ..Default::default()
                },
            ],
            vec![make_trig(
                1,
                DungeonTriggerKind::CompoundAnd {
                    clauses: vec![
                        DungeonTriggerKind::CompoundOr {
                            clauses: vec![
                                DungeonTriggerKind::OnCargoDelivered {
                                    container_ball_id: None,
                                    target_object_index: Some(0),
                                    item_type_id: Some(34),
                                    required_quantity: 10,
                                },
                                DungeonTriggerKind::OnKillCount {
                                    target_npc_group: Some(1),
                                    required_kills: 1,
                                },
                            ],
                        },
                        DungeonTriggerKind::OnStructureDestroyed {
                            target_ball_id: None,
                            target_object_index: None,
                            target_tag: Some("or_core".into()),
                            target_type_id: None,
                        },
                    ],
                },
                DungeonTriggerAction::UnlockGate,
                false,
            )],
        );
        room.entered = true;
        room.entered_unix = Some(1_785_100_000);
        room.npc_groups = vec![DungeonNpcGroup {
            group_id: 1,
            wave: 1,
            npc_specs: vec![(48_090, 1)],
            spawn_immediately: true,
            ..Default::default()
        }];
        DungeonRuntimeInstance {
            instance_id: iid,
            dungeon_id: 1,
            solar_system_id: sys_id,
            owner_char_id: char_id,
            base_position: pos,
            rooms: vec![room],
            created_unix: 1_785_100_000,
            ..Default::default()
        }
    };

    with_dungeon_runtime_store(&ctx, |s| {
        s.instances.insert(inst_a, build_inst(inst_a, 9621, 0.0));
        s.instances.insert(inst_b, build_inst(inst_b, 9622, 10_000.0));
    });
    let mat_a = materialize_dungeon_room_entities(&ctx, sys_id, inst_a, 0)
        .await
        .unwrap();
    let (cont_a, core_a, npc_a) = (mat_a.object_ids[0], mat_a.object_ids[1], mat_a.npc_ids[0]);
    let sys_h = ctx.systems.get_or_spawn(sys_id);

    // 1. Deposit 10 Tritanium (0 NPCs killed) -> withdraw all 10 Tritanium -> destroy structure.
    // BF-2: CompoundOr sibling OnKillCount must NOT inherit already_satisfied=true!
    insert_ship_item(&ctx, char_id, ship_id, 7_900_001, 34, InventoryFlag::Cargo, 10, false);
    inv_add_to(&ctx, &mut sess, char_id, cont_a, 7_900_001, ship_id, Some(10)).await;
    let stack_a = ctx.db.with_snapshot(|snap| {
        snap.items
            .values()
            .find(|i| i.location_id == cont_a && i.type_id == 34)
            .unwrap()
            .item_id
    });
    inv_add_to(&ctx, &mut sess, char_id, ship_id, stack_a, cont_a, Some(10)).await;
    let _ = crate::combat::death_pipeline::on_ball_destroyed(
        &ctx,
        sys_id,
        core_a,
        Some(char_id),
        DVec3::ZERO,
    )
    .await
    .unwrap();
    read_dungeon_runtime_store(&ctx, |s| {
        let r = &s.instances[&inst_a].rooms[0];
        assert!(
            r.gate_locked && !r.triggers[0].fired,
            "BF-2: withdrawing cargo must clear CompoundAnd+CompoundOr latch when OnKillCount never succeeded"
        );
    });

    // Now actually satisfy OnKillCount while OnStructureDestroyed remains latched -> FIRES!
    let _ = sys_h
        .apply_damage_with_attacker(
            npc_a,
            DamageVector::new(100_000.0, 100_000.0, 100_000.0, 100_000.0),
            Some(char_id),
        )
        .await;
    crate::services::pve_dungeon_runtime::on_dungeon_npc_killed(&ctx, sys_id, npc_a, char_id).await;
    read_dungeon_runtime_store(&ctx, |s| {
        let r = &s.instances[&inst_a].rooms[0];
        assert!(!r.gate_locked && r.triggers[0].fired);
    });

    // 2. When BOTH CompoundOr branches succeed (deposit cargo AND kill NPC), withdrawing cargo
    // still preserves the genuinely satisfied OnKillCount branch.
    let mat_b = materialize_dungeon_room_entities(&ctx, sys_id, inst_b, 0)
        .await
        .unwrap();
    let (cont_b, core_b, npc_b) = (mat_b.object_ids[0], mat_b.object_ids[1], mat_b.npc_ids[0]);
    assert!(sys_h
        .set_ball_position(ship_id, DVec3::new(r_pos[0] + 10_050.0, 0.0, 0.0))
        .await);
    insert_ship_item(&ctx, char_id, ship_id, 7_900_002, 34, InventoryFlag::Cargo, 10, false);
    inv_add_to(&ctx, &mut sess, char_id, cont_b, 7_900_002, ship_id, Some(10)).await;
    crate::services::pve_dungeon_runtime::on_dungeon_npc_killed(&ctx, sys_id, npc_b, char_id).await;
    let stack_b = ctx.db.with_snapshot(|snap| {
        snap.items
            .values()
            .find(|i| i.location_id == cont_b && i.type_id == 34)
            .unwrap()
            .item_id
    });
    inv_add_to(&ctx, &mut sess, char_id, ship_id, stack_b, cont_b, Some(10)).await;
    let _ = crate::combat::death_pipeline::on_ball_destroyed(
        &ctx,
        sys_id,
        core_b,
        Some(char_id),
        DVec3::ZERO,
    )
    .await
    .unwrap();
    read_dungeon_runtime_store(&ctx, |s| {
        let r = &s.instances[&inst_b].rooms[0];
        assert!(
            !r.gate_locked && r.triggers[0].fired,
            "legitimately satisfied OnKillCount branch in CompoundOr must remain latched after cargo withdrawal"
        );
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bf3_on_dungeon_room_entered_sync_replays_pre_struct_destroy_escalation() {
    let (sys_id, char_id, inst_id) = (30_000_142u32, 90_070_203i64, 995_204i64);
    let r_pos = [62_000_000.0, 0.0, 0.0];
    let mut sde_mut = (*build_hooks_test_sde()).clone();
    let esc_did = 995_298u32;
    sde_mut.dungeons.insert(
        esc_did,
        eve_sde::DungeonTemplate {
            dungeon_id: esc_did,
            name: "Sync Replay Escalation Site".into(),
            faction_id: Some(500_010),
            archetype_id: Some(20),
            difficulty: Some(2),
        },
    );
    let (ctx, _clock, _sess, _ship_id) = make_space_pilot_ctx(
        Arc::new(sde_mut),
        sys_id,
        char_id,
        "bf3_sync_replay",
        587,
        DVec3::from_array(r_pos),
    )
    .await;

    let room0 = make_room(
        1,
        9623,
        r_pos,
        vec![DungeonRuntimeObject {
            type_id: 16_757,
            name: "Sync Replay Bunker".into(),
            position: [r_pos[0] + 400.0, 0.0, 0.0],
            radius: 150.0,
            destructible: true,
            tag: Some("sync_bunker".into()),
            ..Default::default()
        }],
        vec![DungeonTrigger {
            trigger_id: 1,
            kind: DungeonTriggerKind::OnStructureDestroyed {
                target_ball_id: None,
                target_object_index: None,
                target_tag: Some("sync_bunker".into()),
                target_type_id: None,
            },
            action: DungeonTriggerAction::UnlockGate,
            extra_actions: vec![DungeonTriggerAction::StartEscalation {
                target_dungeon_id: Some(esc_did),
                chance_bps: 10_000,
                target_title: Some("Sync Replay Citadel".into()),
            }],
            ..Default::default()
        }],
    );
    with_dungeon_runtime_store(&ctx, |s| {
        s.instances.insert(
            inst_id,
            DungeonRuntimeInstance {
                instance_id: inst_id,
                dungeon_id: 1,
                solar_system_id: sys_id,
                owner_char_id: char_id,
                base_position: r_pos,
                rooms: vec![room0],
                created_unix: 1_785_100_000,
                ..Default::default()
            },
        );
    });
    let mat = materialize_dungeon_room_entities(&ctx, sys_id, inst_id, 0)
        .await
        .unwrap();
    let _ = crate::combat::death_pipeline::on_ball_destroyed_sync(
        &ctx,
        sys_id,
        mat.object_ids[0],
        Some(char_id),
        DVec3::ZERO,
    )
    .unwrap();
    read_dungeon_runtime_store(&ctx, |s| {
        assert!(s.instances[&inst_id].rooms[0].objects[0].destroyed);
        assert!(s.instances[&inst_id].rooms[0].gate_locked);
        assert!(s.pending_escalations.is_empty());
    });

    // Synchronous room-entry replay MUST preserve StartEscalation into pending_escalations.
    crate::services::pve_dungeon_runtime::on_dungeon_room_entered_sync(
        &ctx, sys_id, inst_id, 0, char_id,
    );
    read_dungeon_runtime_store(&ctx, |s| {
        let r = &s.instances[&inst_id].rooms[0];
        assert!(!r.gate_locked && r.triggers[0].fired);
        assert_eq!(s.pending_escalations.len(), 1, "BF-3: sync replay must queue escalation");
    });

    crate::services::pve_dungeon_runtime::tick_background_dungeon_runtime(&ctx).await;
    read_dungeon_runtime_store(&ctx, |s| {
        assert!(s.pending_escalations.is_empty());
        assert!(s.instances[&inst_id].escalation_instance_id.is_some());
    });
    with_pve_exploration_reg(&ctx, |reg| {
        assert_eq!(reg.escalations_by_char[&char_id].len(), 1);
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn note3_unfitted_module_charges_moved_to_dungeon_container_credit_cargo_delivered() {
    let (sys_id, char_id, inst_id) = (30_000_142u32, 90_070_204i64, 995_205i64);
    let r_pos = [63_000_000.0, 0.0, 0.0];
    let (ctx, _clock, mut sess, ship_id) = make_space_pilot_ctx(
        build_hooks_test_sde(),
        sys_id,
        char_id,
        "note3_charges_pilot",
        587,
        DVec3::from_array(r_pos),
    )
    .await;

    let mut room = make_room(
        1,
        9624,
        r_pos,
        vec![DungeonRuntimeObject {
            type_id: DEFAULT_CONTAINER_TYPE_ID,
            name: "Ordnance Hopper".into(),
            position: r_pos,
            radius: 150.0,
            is_container: true,
            ..Default::default()
        }],
        vec![make_trig(
            1,
            DungeonTriggerKind::OnCargoDelivered {
                container_ball_id: None,
                target_object_index: Some(0),
                item_type_id: Some(179),
                required_quantity: 20,
            },
            DungeonTriggerAction::UnlockGate,
            false,
        )],
    );
    room.entered = true;
    room.entered_unix = Some(1_785_100_000);
    with_dungeon_runtime_store(&ctx, |s| {
        s.instances.insert(
            inst_id,
            DungeonRuntimeInstance {
                instance_id: inst_id,
                dungeon_id: 1,
                solar_system_id: sys_id,
                owner_char_id: char_id,
                base_position: r_pos,
                rooms: vec![room],
                created_unix: 1_785_100_000,
                ..Default::default()
            },
        );
    });
    let mat = materialize_dungeon_room_entities(&ctx, sys_id, inst_id, 0)
        .await
        .unwrap();
    let cont_id = mat.object_ids[0];

    let (depot_id, turret_id, charge_id) = (7_950_000i64, 7_950_001i64, 7_950_002i64);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            depot_id,
            InventoryItemRecord {
                item_id: depot_id,
                type_id: 33_474,
                owner_id: char_id,
                location_id: i64::from(sys_id),
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_h
        .spawn_ball(Ball {
            id: depot_id,
            type_id: 33_474,
            position: DVec3::new(r_pos[0] + 100.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 1_000.0,
            radius: 100.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    insert_ship_item(&ctx, char_id, ship_id, turret_id, 486, InventoryFlag::HiSlot0, 1, true);
    insert_ship_item(&ctx, char_id, ship_id, charge_id, 179, InventoryFlag::HiSlot0, 20, false);

    inv_add_to(&ctx, &mut sess, char_id, cont_id, turret_id, ship_id, None).await;
    read_dungeon_runtime_store(&ctx, |s| {
        let r = &s.instances[&inst_id].rooms[0];
        assert_eq!(r.cargo_delivered_net.get(&format!("{cont_id}:179")), Some(&20));
        assert!(!r.gate_locked && r.triggers[0].fired);
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn own01_live_damage_complete_and_fail_dungeon_keep_single_settlement_no_wreck_km() {
    let sys_id = 30_000_142u32;
    let r_base = [64_000_000.0, 0.0, 0.0];
    for (idx, (is_fail, via_missile)) in [
        (false, false),
        (false, true),
        (true, false),
        (true, true),
    ]
    .into_iter()
    .enumerate()
    {
        let char_id = 90_070_300 + i64::try_from(idx).unwrap();
        let inst_id = 995_300 + i64::try_from(idx).unwrap();
        let r_pos = [r_base[0] + (idx as f64) * 50_000.0, 0.0, 0.0];
        let (ctx, _clock, _sess, ship_id) = make_space_pilot_ctx(
            build_hooks_test_sde(),
            sys_id,
            char_id,
            &format!("own01_p_{idx}"),
            587,
            DVec3::from_array(r_pos),
        )
        .await;

        let action = if is_fail {
            DungeonTriggerAction::FailDungeon {
                reason: "Bunker breached".into(),
            }
        } else {
            DungeonTriggerAction::CompleteDungeon
        };
        let mut room = make_room(
            1,
            9630 + i64::try_from(idx).unwrap(),
            r_pos,
            vec![DungeonRuntimeObject {
                type_id: 16_757,
                name: "Terminal Relay Bunker".into(),
                position: [r_pos[0] + 600.0, 0.0, 0.0],
                radius: 200.0,
                is_container: true,
                destructible: true,
                loot_items: vec![(34, 15)],
                tag: Some("terminal_bunker".into()),
                ..Default::default()
            }],
            vec![make_trig(
                1,
                DungeonTriggerKind::OnStructureDestroyed {
                    target_ball_id: None,
                    target_object_index: None,
                    target_tag: Some("terminal_bunker".into()),
                    target_type_id: Some(16_757),
                },
                action,
                false,
            )],
        );
        room.entered = true;
        room.entered_unix = Some(1_785_100_000);
        with_dungeon_runtime_store(&ctx, |s| {
            s.instances.insert(
                inst_id,
                DungeonRuntimeInstance {
                    instance_id: inst_id,
                    dungeon_id: 1,
                    solar_system_id: sys_id,
                    owner_char_id: char_id,
                    base_position: r_pos,
                    rooms: vec![room],
                    created_unix: 1_785_100_000,
                    ..Default::default()
                },
            );
        });
        let mat = materialize_dungeon_room_entities(&ctx, sys_id, inst_id, 0)
            .await
            .unwrap();
        let bid = mat.object_ids[0];
        let sys_h = ctx.systems.get_or_spawn(sys_id);
        let (_, pre_ball) = sys_h.get_ball_snapshot(bid).await.unwrap().unwrap();

        let lethal = DamageVector::new(100_000.0, 100_000.0, 100_000.0, 100_000.0);
        let (_, dead, _) = sys_h
            .apply_damage_with_attacker(bid, lethal, Some(char_id))
            .await
            .unwrap()
            .unwrap();
        assert!(dead);

        // OWN-01: must STILL be recognized as owned destructible dungeon object after Complete/Fail!
        assert!(
            crate::services::pve_dungeon_runtime_hooks::is_owned_destructible_dungeon_object(
                &ctx, sys_id, bid,
            ),
            "OWN-01: terminal bunker must stay recognized (is_fail={is_fail}, via_missile={via_missile})"
        );
        read_dungeon_runtime_store(&ctx, |s| {
            let inst = &s.instances[&inst_id];
            assert_eq!(inst.completed, !is_fail);
            assert_eq!(inst.failed, is_fail);
            let o = &inst.rooms[0].objects[0];
            assert!(o.destroyed && !o.spawned && o.ball_id == bid);
        });
        // Reconcile must never re-spawn the destroyed object.
        assert_eq!(
            reconcile_and_rematerialize_dungeon_instance(&ctx, sys_id, inst_id).await,
            0
        );
        assert!(sys_h.get_ball_snapshot(bid).await.unwrap().is_none());

        if via_missile {
            crate::services::combat_crimewatch::combat_missiles_aoe::handle_destroyed_ball_from_missile_or_aoe(
                &ctx,
                sys_id,
                char_id,
                bid,
                16_757,
                pre_ball.position,
                ctx.now_unix(),
            )
            .await;
        } else {
            let res = crate::services::combat_crimewatch::combat_weapons_spool::handle_ship_or_npc_destruction(
                &ctx,
                &sys_h,
                char_id,
                ship_id,
                587,
                486,
                bid,
                &pre_ball,
                None,
                sys_id,
                100.0,
                ctx.now_unix(),
            )
            .await;
            assert_eq!(res.wreck_id, 0, "OWN-01: no bogus NPC wreck on turret terminal kill");
            assert_eq!(res.kill_id, 0, "OWN-01: no bogus killmail on turret terminal kill");
        }

        ctx.db.with_snapshot(|s| {
            assert!(s.killmails.is_empty());
            assert!(s.standing_transactions.is_empty());
            assert!(
                !s.items
                    .values()
                    .any(|i| i.location_id == i64::from(sys_id) && i.item_id != ship_id),
                "OWN-01: no bogus NPC wreck/container left in space (is_fail={is_fail}, via_missile={via_missile})"
            );
        });
        assert!(!with_combat_runtime(&ctx, |rt| rt.container_lifetimes.contains_key(&bid)));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn own02_compound_and_or_with_on_kill_on_approach_and_on_npc_damaged_plus_read_purity() {
    let (sys_id, char_id, inst_id) = (30_000_142u32, 90_070_310i64, 995_310i64);
    let r_pos = [66_000_000.0, 0.0, 0.0];
    let mut sde_mut = (*build_hooks_test_sde()).clone();
    sde_mut.types.insert(
        16_981,
        mk_inv_type(
            16_981,
            550,
            11,
            "Angel Raider",
            2_500.0,
            0.0,
            None,
            &[(9, 500.0), (263, 500.0), (265, 500.0)],
        ),
    );
    let (ctx, clock, _sess, ship_id) = make_space_pilot_ctx(
        Arc::new(sde_mut),
        sys_id,
        char_id,
        "own02_pilot",
        587,
        DVec3::new(r_pos[0] + 15_000.0, 0.0, 0.0),
    )
    .await;

    let mut room = make_room(
        1,
        9640,
        r_pos,
        vec![
            DungeonRuntimeObject {
                type_id: DEFAULT_CONTAINER_TYPE_ID,
                name: "Loot Crate".into(),
                position: [r_pos[0] + 300.0, 0.0, 0.0],
                radius: 150.0,
                is_container: true,
                loot_items: vec![(34, 10)],
                ..Default::default()
            },
            DungeonRuntimeObject {
                type_id: 16_757,
                name: "Bunker Dmg".into(),
                position: [r_pos[0] + 600.0, 0.0, 0.0],
                radius: 200.0,
                destructible: true,
                tag: Some("bunker_dmg".into()),
                ..Default::default()
            },
            DungeonRuntimeObject {
                type_id: 16_757,
                name: "Bunker Or".into(),
                position: [r_pos[0] + 900.0, 0.0, 0.0],
                radius: 200.0,
                destructible: true,
                tag: Some("bunker_or".into()),
                ..Default::default()
            },
        ],
        vec![
            make_trig(
                1,
                DungeonTriggerKind::CompoundAnd {
                    clauses: vec![
                        DungeonTriggerKind::OnKill {
                            target_npc_group: Some(1),
                            target_ball_id: None,
                        },
                        DungeonTriggerKind::OnLoot {
                            container_ball_id: None,
                            target_object_index: Some(0),
                            item_type_id: Some(34),
                        },
                    ],
                },
                DungeonTriggerAction::TriggerMessage {
                    message_type: 1,
                    message_id: 7101,
                },
                false,
            ),
            make_trig(
                2,
                DungeonTriggerKind::CompoundAnd {
                    clauses: vec![
                        DungeonTriggerKind::OnApproach {
                            target_ball_id: None,
                            target_object_index: None,
                            anchor_pos: r_pos,
                            radius_m: 3_000.0,
                        },
                        DungeonTriggerKind::OnTimer {
                            delay_secs: 5,
                            timer_id: None,
                            after_group_cleared: None,
                        },
                    ],
                },
                DungeonTriggerAction::TriggerMessage {
                    message_type: 1,
                    message_id: 7102,
                },
                false,
            ),
            make_trig(
                3,
                DungeonTriggerKind::CompoundAnd {
                    clauses: vec![
                        DungeonTriggerKind::OnNpcDamaged {
                            target_npc_group: Some(2),
                            target_tag: None,
                            target_ball_id: None,
                            hull_or_shield_below_pct: 60,
                            on_first_attack: false,
                        },
                        DungeonTriggerKind::OnStructureDestroyed {
                            target_ball_id: None,
                            target_object_index: None,
                            target_tag: Some("bunker_dmg".into()),
                            target_type_id: None,
                        },
                    ],
                },
                DungeonTriggerAction::TriggerMessage {
                    message_type: 1,
                    message_id: 7103,
                },
                false,
            ),
            make_trig(
                4,
                DungeonTriggerKind::CompoundAnd {
                    clauses: vec![
                        DungeonTriggerKind::CompoundOr {
                            clauses: vec![
                                DungeonTriggerKind::OnCargoDelivered {
                                    container_ball_id: None,
                                    target_object_index: Some(0),
                                    item_type_id: Some(34),
                                    required_quantity: 5,
                                },
                                DungeonTriggerKind::OnKill {
                                    target_npc_group: Some(3),
                                    target_ball_id: None,
                                },
                            ],
                        },
                        DungeonTriggerKind::OnStructureDestroyed {
                            target_ball_id: None,
                            target_object_index: None,
                            target_tag: Some("bunker_or".into()),
                            target_type_id: None,
                        },
                    ],
                },
                DungeonTriggerAction::UnlockGate,
                false,
            ),
        ],
    );
    room.npc_groups = vec![
        DungeonNpcGroup {
            group_id: 1,
            wave: 1,
            spawn_immediately: true,
            npc_entries: vec![eve_db::DungeonNpcSpawnEntry {
                type_id: 16_981,
                count: 1,
                ..Default::default()
            }],
            ..Default::default()
        },
        DungeonNpcGroup {
            group_id: 2,
            wave: 1,
            spawn_immediately: true,
            npc_entries: vec![eve_db::DungeonNpcSpawnEntry {
                type_id: 16_981,
                count: 1,
                ..Default::default()
            }],
            ..Default::default()
        },
        DungeonNpcGroup {
            group_id: 3,
            wave: 1,
            spawn_immediately: true,
            npc_entries: vec![eve_db::DungeonNpcSpawnEntry {
                type_id: 16_981,
                count: 1,
                ..Default::default()
            }],
            ..Default::default()
        },
    ];
    room.entered = true;
    room.entered_unix = Some(1_785_100_000);
    with_dungeon_runtime_store(&ctx, |s| {
        s.instances.insert(
            inst_id,
            DungeonRuntimeInstance {
                instance_id: inst_id,
                dungeon_id: 1,
                solar_system_id: sys_id,
                owner_char_id: char_id,
                base_position: r_pos,
                rooms: vec![room],
                created_unix: 1_785_100_000,
                ..Default::default()
            },
        );
    });
    let mat = materialize_dungeon_room_entities(&ctx, sys_id, inst_id, 0)
        .await
        .unwrap();
    let (cont_id, bunker_dmg_id, bunker_or_id) = (mat.object_ids[0], mat.object_ids[1], mat.object_ids[2]);
    let (npc1, npc2, npc3) = read_dungeon_runtime_store(&ctx, |s| {
        let g = &s.instances[&inst_id].rooms[0].npc_groups;
        (g[0].npc_ball_ids[0], g[1].npc_ball_ids[0], g[2].npc_ball_ids[0])
    });
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let lethal = DamageVector::new(50_000.0, 50_000.0, 50_000.0, 50_000.0);

    // 1. CompoundAnd([OnKill, OnLoot]): OnKill latches, then OnLoot completes trigger 0.
    let _ = sys_h.apply_damage_with_attacker(npc1, lethal, Some(char_id)).await.unwrap();
    read_dungeon_runtime_store(&ctx, |s| assert!(!s.instances[&inst_id].rooms[0].triggers[0].fired));
    on_dungeon_item_looted(&ctx, char_id, Some(sys_id), cont_id, 34).await;
    read_dungeon_runtime_store(&ctx, |s| assert!(s.instances[&inst_id].rooms[0].triggers[0].fired));

    // 2. CompoundAnd([OnApproach, OnTimer]) + read-purity after leaving approach radius.
    assert!(sys_h.set_ball_position(ship_id, DVec3::from_array(r_pos)).await);
    let _ = crate::services::pve_dungeon_runtime_rpc::on_dungeon_approach(&ctx, char_id, sys_id, ship_id, None).await;
    read_dungeon_runtime_store(&ctx, |s| {
        let r = &s.instances[&inst_id].rooms[0];
        assert!(!r.triggers[1].fired);
        assert!(r.armed_timers.contains_key("__and:0:1:root.0"));
    });
    // Leave radius: idle approach + immature timer tick MUST be 100% read-pure (0 write tx) and keep latch!
    assert!(sys_h.set_ball_position(ship_id, DVec3::new(r_pos[0] + 15_000.0, 0.0, 0.0)).await);
    let commits_out = ctx.db.tx_commit_count();
    assert_eq!(crate::services::pve_dungeon_runtime_rpc::on_dungeon_approach(&ctx, char_id, sys_id, ship_id, None).await, 0);
    crate::services::pve_dungeon_runtime::tick_dungeon_runtime_system_step(&ctx, sys_id).await;
    assert_eq!(ctx.db.tx_commit_count(), commits_out, "OWN-02: idle out-of-range eval must be read-pure");
    clock.advance_secs(6);
    crate::services::pve_dungeon_runtime::tick_dungeon_runtime_system_step(&ctx, sys_id).await;
    read_dungeon_runtime_store(&ctx, |s| assert!(s.instances[&inst_id].rooms[0].triggers[1].fired));

    // 3. CompoundAnd([OnNpcDamaged, OnStructureDestroyed]): damage below 60% HP latches, then bunker destroy fires.
    let mid_dmg = DamageVector::new(700.0, 0.0, 0.0, 0.0);
    let _ = sys_h.apply_damage_with_attacker(npc2, mid_dmg, Some(char_id)).await.unwrap();
    sys_h.step_manual(1.0).await.unwrap();
    read_dungeon_runtime_store(&ctx, |s| {
        let r = &s.instances[&inst_id].rooms[0];
        assert!(!r.triggers[2].fired);
        assert!(r.armed_timers.contains_key("__and:0:2:root.0"));
    });
    let _ = sys_h.apply_damage_with_attacker(bunker_dmg_id, lethal, Some(char_id)).await.unwrap();
    read_dungeon_runtime_store(&ctx, |s| assert!(s.instances[&inst_id].rooms[0].triggers[2].fired));

    // 4. CompoundAnd([CompoundOr([OnCargoDelivered, OnKill]), OnStructureDestroyed]):
    // kill satisfies OnKill inside CompoundOr; withdrawing cargo must NOT unlatch CompoundOr/CompoundAnd.
    let _ = sys_h.apply_damage_with_attacker(npc3, lethal, Some(char_id)).await.unwrap();
    read_dungeon_runtime_store(&ctx, |s| {
        let r = &s.instances[&inst_id].rooms[0];
        assert!(!r.triggers[3].fired);
        assert!(r.armed_timers.contains_key("__and:0:3:root.0"));
        assert!(r.armed_timers.contains_key("__or:0:3:root.0.1"));
    });
    on_dungeon_container_cargo_changed(&ctx, char_id, Some(sys_id), cont_id, ship_id, 34, 5).await;
    read_dungeon_runtime_store(&ctx, |s| {
        let r = &s.instances[&inst_id].rooms[0];
        assert!(r.armed_timers.contains_key("__and:0:3:root.0"), "OnKill branch must stay latched");
        assert!(r.armed_timers.contains_key("__or:0:3:root.0.1"));
    });
    let _ = sys_h.apply_damage_with_attacker(bunker_or_id, lethal, Some(char_id)).await.unwrap();
    read_dungeon_runtime_store(&ctx, |s| {
        let r = &s.instances[&inst_id].rooms[0];
        assert!(r.triggers[3].fired && !r.gate_locked);
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn p2_pre_entry_struct_destroy_semantics_rpc_zero_type_filter_and_same_room_terminal_transfer() {
    // P2-2: RPC trigger parser filters 0 type IDs to None.
    let cargo_zero = crate::services::pve_dungeon_runtime_rpc::parse_dungeon_trigger_kind(
        &PyRep::keyval([
            ("kind", PyRep::String("cargo_delivered".into())),
            ("itemTypeID", PyRep::Integer(0)),
            ("requiredQuantity", PyRep::Integer(5)),
        ]),
        &[],
        [0.0; 3],
        0,
    );
    assert!(matches!(
        cargo_zero,
        DungeonTriggerKind::OnCargoDelivered { item_type_id: None, required_quantity: 5, .. }
    ));
    let struct_zero = crate::services::pve_dungeon_runtime_rpc::parse_dungeon_trigger_kind(
        &PyRep::keyval([
            ("kind", PyRep::String("structure_destroyed".into())),
            ("targetTypeID", PyRep::Integer(0)),
            ("targetTag", PyRep::String("relay".into())),
        ]),
        &[],
        [0.0; 3],
        0,
    );
    assert!(matches!(
        struct_zero,
        DungeonTriggerKind::OnStructureDestroyed { target_type_id: None, target_tag: Some(t), .. } if t == "relay"
    ));

    // P2-1 & P2-4: Pre-entry TriggerMessage structure destruction preserves gate lock without UnlockGate,
    // and same-room src->dst container transfer debits src_cont net while crediting dst_cont to complete.
    let (sys_id, char_id, inst_a, inst_b) = (30_000_142u32, 90_070_320i64, 995_320i64, 995_321i64);
    let r_pos = [68_000_000.0, 0.0, 0.0];
    let (ctx, _clock, _sess, _ship_id) = make_space_pilot_ctx(
        build_hooks_test_sde(),
        sys_id,
        char_id,
        "p2_notes_pilot",
        587,
        DVec3::from_array(r_pos),
    )
    .await;

    let room_pre = make_room(
        1,
        9650,
        r_pos,
        vec![DungeonRuntimeObject {
            type_id: 16_757,
            name: "Pre-Entry Prop".into(),
            position: [r_pos[0] + 400.0, 0.0, 0.0],
            radius: 150.0,
            destructible: true,
            tag: Some("msg_prop".into()),
            ..Default::default()
        }],
        vec![make_trig(
            1,
            DungeonTriggerKind::OnStructureDestroyed {
                target_ball_id: None,
                target_object_index: None,
                target_tag: Some("msg_prop".into()),
                target_type_id: None,
            },
            DungeonTriggerAction::TriggerMessage {
                message_type: 1,
                message_id: 7201,
            },
            false,
        )],
    );
    let mut room_term = make_room(
        1,
        9651,
        [r_pos[0] + 20_000.0, 0.0, 0.0],
        vec![
            DungeonRuntimeObject {
                type_id: DEFAULT_CONTAINER_TYPE_ID,
                name: "Src Cache".into(),
                position: [r_pos[0] + 20_100.0, 0.0, 0.0],
                radius: 150.0,
                is_container: true,
                ..Default::default()
            },
            DungeonRuntimeObject {
                type_id: DEFAULT_CONTAINER_TYPE_ID,
                name: "Dst Cache".into(),
                position: [r_pos[0] + 20_300.0, 0.0, 0.0],
                radius: 150.0,
                is_container: true,
                ..Default::default()
            },
        ],
        vec![make_trig(
            1,
            DungeonTriggerKind::OnCargoDelivered {
                container_ball_id: None,
                target_object_index: Some(1),
                item_type_id: Some(34),
                required_quantity: 5,
            },
            DungeonTriggerAction::CompleteDungeon,
            false,
        )],
    );
    room_term.entered = true;
    room_term.entered_unix = Some(1_785_100_000);
    with_dungeon_runtime_store(&ctx, |s| {
        s.instances.insert(
            inst_a,
            DungeonRuntimeInstance {
                instance_id: inst_a,
                dungeon_id: 1,
                solar_system_id: sys_id,
                owner_char_id: char_id,
                base_position: r_pos,
                rooms: vec![room_pre],
                created_unix: 1_785_100_000,
                ..Default::default()
            },
        );
        s.instances.insert(
            inst_b,
            DungeonRuntimeInstance {
                instance_id: inst_b,
                dungeon_id: 1,
                solar_system_id: sys_id,
                owner_char_id: char_id,
                base_position: [r_pos[0] + 20_000.0, 0.0, 0.0],
                rooms: vec![room_term],
                created_unix: 1_785_100_000,
                ..Default::default()
            },
        );
    });

    let mat_a = materialize_dungeon_room_entities(&ctx, sys_id, inst_a, 0).await.unwrap();
    let _ = crate::combat::death_pipeline::on_ball_destroyed_sync(
        &ctx, sys_id, mat_a.object_ids[0], Some(char_id), DVec3::ZERO,
    );
    on_dungeon_room_entered(&ctx, sys_id, inst_a, 0, char_id).await;
    read_dungeon_runtime_store(&ctx, |s| {
        let r = &s.instances[&inst_a].rooms[0];
        assert!(r.triggers[0].fired);
        assert!(r.gate_locked, "P2-1: TriggerMessage on pre-entry struct destroy must NOT auto-unlock gate");
    });

    let mat_b = materialize_dungeon_room_entities(&ctx, sys_id, inst_b, 0).await.unwrap();
    let (src_cont, dst_cont) = (mat_b.object_ids[0], mat_b.object_ids[1]);
    with_dungeon_runtime_store(&ctx, |s| {
        s.instances
            .get_mut(&inst_b)
            .unwrap()
            .rooms[0]
            .cargo_delivered_net
            .insert(format!("{src_cont}:34"), 10);
    });
    // Transferring 5 from src_cont -> dst_cont in same room updates src net (10->5) and credits dst_cont (5) to complete.
    on_dungeon_container_cargo_changed(&ctx, char_id, Some(sys_id), src_cont, dst_cont, 34, 5).await;
    read_dungeon_runtime_store(&ctx, |s| {
        let inst = &s.instances[&inst_b];
        assert!(inst.completed && inst.rooms[0].triggers[0].fired);
        assert_eq!(inst.rooms[0].cargo_delivered_net.get(&format!("{src_cont}:34")), Some(&5));
        assert_eq!(inst.rooms[0].cargo_delivered_net.get(&format!("{dst_cont}:34")), Some(&5));
    });
}

#[tokio::test]
async fn own_diff_p1_02_dogma_activate_on_destructible_dungeon_container_has_no_pvp_safety_or_timers_and_fails_nested_courier(
) {
    use crate::services::combat_crimewatch::combat_weapons::handle_weapons;
    use crate::services::combat_crimewatch::{is_npc_timer_active, set_char_safety_level, SafetyLevel};
    use eve_db::ContractRecord;
    use eve_destiny::{CombatState, TargetLock};

    let (sys_id, owner_char, fleet_char, inst_id) =
        (30_000_142u32, 90_070_401i64, 90_070_402i64, 995_401i64);
    let r_pos = [72_000_000.0, 0.0, 0.0];

    let mut sde = (*build_hooks_test_sde()).clone();
    sde.types.insert(
        55_401,
        mk_inv_type(
            55_401,
            55,
            7,
            "Test AutoCannon",
            5.0,
            1.0,
            None,
            &[
                (6, 0.0),
                (51, 500.0),
                (54, 15_000.0),
                (64, 1.0),
                (128, 1.0),
                (158, 5_000.0),
                (160, 100.0),
                (604, 83.0),
                (620, 40_000.0),
            ],
        ),
    );
    sde.types.insert(
        83_401,
        mk_inv_type(
            83_401,
            83,
            8,
            "Test Projectile Charge",
            0.01,
            0.0,
            None,
            &[
                (114, 60.0),
                (116, 60.0),
                (117, 60.0),
                (118, 60.0),
                (120, 1.0),
                (128, 1.0),
            ],
        ),
    );

    let (ctx, clock, mut owner_sess, owner_ship) = make_space_pilot_ctx(
        Arc::new(sde),
        sys_id,
        owner_char,
        "p102_owner",
        587,
        DVec3::from_array([r_pos[0] + 1_000.0, 0.0, 0.0]),
    )
    .await;
    assert!(
        ctx.sde
            .solar_systems
            .get(&sys_id)
            .is_some_and(|s| s.security_status >= 0.45),
        "test must run in high-sec so Green/Yellow safety gates are active"
    );

    // Seed Green-safety fleetmate pilot in the same solar system.
    ctx.db
        .provision_gm_account("p102_fleetmate", "pw", fleet_char, "p102_fleetmate");
    let (fleet_uid, fleet_ship) = ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&fleet_char).unwrap();
        ch.solar_system_id = sys_id;
        ch.station_id = None;
        ch.structure_id = None;
        let sid = ch.active_ship_id;
        let uid = ch.user_id;
        if let Some(it) = snap.items.get_mut(&sid) {
            it.type_id = 587;
            it.location_id = i64::from(sys_id);
            it.flag = InventoryFlag::None;
        }
        (uid, sid)
    });
    let _ = ctx.db.acquire_online_lock_with_computer_hash(
        fleet_uid,
        fleet_char as u64,
        Some(fleet_char),
        None,
        true,
        1_785_100_000,
    );
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    sys_h
        .spawn_ball_with_combat(
            Ball {
                id: fleet_ship,
                type_id: 587,
                position: DVec3::from_array([r_pos[0] + 1_200.0, 0.0, 0.0]),
                velocity: DVec3::ZERO,
                max_velocity: 2_500.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState {
                shield_hp: 500.0,
                max_shield_hp: 500.0,
                armor_hp: 500.0,
                max_armor_hp: 500.0,
                structure_hp: 500.0,
                max_structure_hp: 500.0,
                capacitor: 500.0,
                max_capacitor: 500.0,
                is_tethered: false,
                ..CombatState::default()
            },
        )
        .await
        .unwrap();
    let mut fleet_sess = SessionState::new(fleet_char as u64);
    fleet_sess.user_id = Some(fleet_uid);
    fleet_sess.character_id = Some(fleet_char);
    fleet_sess.solar_system_id = Some(sys_id);
    fleet_sess.ship_id = Some(fleet_ship);

    // Ensure both ships are untethered with active projectile turrets and charges.
    let mut owner_cs = sys_h.get_combat_state(owner_ship).await.unwrap().unwrap();
    owner_cs.is_tethered = false;
    owner_cs.capacitor = 500.0;
    owner_cs.max_capacitor = 500.0;
    sys_h.set_combat_state(owner_ship, owner_cs).await.unwrap();

    let (owner_gun, owner_ammo, fleet_gun, fleet_ammo) =
        (881_001i64, 881_002i64, 881_003i64, 881_004i64);
    ctx.db.with_mut_snapshot(|snap| {
        for (char_id, ship_id, gun_id, ammo_id) in [
            (owner_char, owner_ship, owner_gun, owner_ammo),
            (fleet_char, fleet_ship, fleet_gun, fleet_ammo),
        ] {
            snap.items.insert(
                gun_id,
                InventoryItemRecord {
                    item_id: gun_id,
                    type_id: 55_401,
                    owner_id: char_id,
                    location_id: ship_id,
                    flag: InventoryFlag::HiSlot0,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
            snap.items.insert(
                ammo_id,
                InventoryItemRecord {
                    item_id: ammo_id,
                    type_id: 83_401,
                    owner_id: char_id,
                    location_id: ship_id,
                    flag: InventoryFlag::HiSlot0,
                    quantity: 50,
                    singleton: false,
                    version: 1,
                    ..Default::default()
                },
            );
            snap.module_active_charges.insert(gun_id, 83_401);
        }
    });
    ctx.invalidate_ship_fitting(owner_ship);
    ctx.invalidate_ship_fitting(fleet_ship);
    set_char_safety_level(&ctx, owner_char, SafetyLevel::Red);
    set_char_safety_level(&ctx, fleet_char, SafetyLevel::Green);

    // Destructible container (16_757, is_container: true, destructible: true) with OnStructureDestroyed -> CompleteDungeon.
    let mut room = make_room(
        1,
        9710,
        r_pos,
        vec![DungeonRuntimeObject {
            type_id: 16_757,
            name: "Angel Relay Cache".into(),
            position: [r_pos[0] + 400.0, 0.0, 0.0],
            radius: 150.0,
            is_container: true,
            destructible: true,
            ..Default::default()
        }],
        vec![make_trig(
            1,
            DungeonTriggerKind::OnStructureDestroyed {
                target_ball_id: None,
                target_object_index: Some(0),
                target_tag: None,
                target_type_id: None,
            },
            DungeonTriggerAction::CompleteDungeon,
            false,
        )],
    );
    room.entered = true;
    room.entered_unix = Some(1_785_100_000);
    with_dungeon_runtime_store(&ctx, |s| {
        s.instances.insert(
            inst_id,
            DungeonRuntimeInstance {
                instance_id: inst_id,
                dungeon_id: 1,
                solar_system_id: sys_id,
                owner_char_id: owner_char,
                base_position: r_pos,
                rooms: vec![room],
                created_unix: 1_785_100_000,
                ..Default::default()
            },
        );
    });

    let mat = materialize_dungeon_room_entities(&ctx, sys_id, inst_id, 0)
        .await
        .unwrap();
    let cont_id = mat.object_ids[0];

    // Seed a child item & nested courier contract inside the container to verify P2-3 cleanup sharing.
    let nested_cargo_id = 881_999i64;
    let contract_id = 771_999i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            nested_cargo_id,
            InventoryItemRecord {
                item_id: nested_cargo_id,
                type_id: 34,
                owner_id: owner_char,
                location_id: cont_id,
                flag: InventoryFlag::Cargo,
                quantity: 10,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.contracts.insert(
            contract_id,
            ContractRecord {
                contract_id,
                issuer_id: owner_char,
                acceptor_id: Some(fleet_char),
                contract_type: "Courier".into(),
                status: "InProgress".into(),
                start_station_id: 60_003_760,
                reward: IskCents(1_000),
                collateral: IskCents(2_000),
                item_ids: vec![nested_cargo_id],
                ..Default::default()
            },
        );
    });

    for ship_id in [owner_ship, fleet_ship] {
        sys_h
            .add_target_lock(
                ship_id,
                TargetLock {
                    target_id: cont_id,
                    lock_time_ms: 0.0,
                    remaining_lock_ms: 0.0,
                    locked: true,
                    max_range_m: Some(50_000.0),
                },
            )
            .await
            .unwrap();
    }

    let kwargs = HashMap::new();
    // 1. Solo owner (Red safety) shoots destructible dungeon container: must hit with NPC timer ONLY, 0 PvP timers.
    let owner_shot = handle_weapons(
        &ctx,
        &mut owner_sess,
        "dogmaIM",
        "Activate",
        &[
            PyRep::Integer(owner_gun),
            PyRep::String("projectileFired".into()),
            PyRep::Integer(cont_id),
        ],
        &kwargs,
    )
    .await
    .unwrap()
    .expect("owner dogmaIM.Activate must succeed");
    assert!(
        owner_shot
            .dict_get("appliedDamage")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0)
            > 0.0
    );

    // 2. Green-safety fleetmate fires repeatedly via dogmaIM.Activate to finish off the container:
    // must NEVER be rejected by CantDoInCurrentSafetyLevel and must apply NPC aggression + OnStructureDestroyed.
    let mut cont_destroyed = false;
    for shot_idx in 0..8 {
        clock.advance_secs(1);
        ctx.clear_all_reactivation_delays();
        let fleet_shot = handle_weapons(
            &ctx,
            &mut fleet_sess,
            "dogmaIM",
            "Activate",
            &[
                PyRep::Integer(fleet_gun),
                PyRep::String("projectileFired".into()),
                PyRep::Integer(cont_id),
            ],
            &kwargs,
        )
        .await
        .unwrap()
        .unwrap_or_else(|e| panic!("Green-safety fleetmate shot {shot_idx} must not fail: {e:?}"));
        if fleet_shot
            .dict_get("destroyed")
            .and_then(|v| v.as_bool())
            == Some(true)
        {
            cont_destroyed = true;
            break;
        }
    }
    assert!(cont_destroyed, "Green-safety fleetmate dogmaIM.Activate shots must destroy container");

    let now_unix = ctx.now_unix();
    for pilot in [owner_char, fleet_char] {
        assert!(
            is_npc_timer_active(&ctx, pilot, now_unix),
            "attacking destructible dungeon container must set NPC timer for {pilot}"
        );
        let mut cw = ctx.crimewatch.write().unwrap();
        let st = cw.get_or_create(pilot);
        assert_eq!(st.pvp_timer_until_unix, 0, "no PvP timer on dungeon container");
        assert_eq!(st.suspect_flag_until_unix, 0, "no Suspect flag on dungeon container");
        assert_eq!(st.criminal_flag_until_unix, 0, "no Criminal flag on dungeon container");
    }

    read_dungeon_runtime_store(&ctx, |s| {
        let inst = &s.instances[&inst_id];
        assert!(inst.completed && inst.rooms[0].triggers[0].fired, "OnStructureDestroyed must complete dungeon");
    });
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&cont_id));
        assert!(!snap.items.contains_key(&nested_cargo_id));
        assert_eq!(
            snap.contracts.get(&contract_id).map(|c| c.status.as_str()),
            Some("Failed"),
            "P2-3: destroying container holding courier cargo must fail the courier contract"
        );
    });
}

#[tokio::test]
async fn own_diff_p1_03_repeatable_cargo_threshold_and_wildcard_compound_and_latches() {
    let (sys_id, char_id, inst_rep, inst_wild) =
        (30_000_142u32, 90_070_410i64, 995_410i64, 995_411i64);
    let r_pos = [75_000_000.0, 0.0, 0.0];
    let (ctx, _clock, _sess, _ship_id) = make_space_pilot_ctx(
        build_hooks_test_sde(),
        sys_id,
        char_id,
        "p103_pilot",
        587,
        DVec3::from_array(r_pos),
    )
    .await;

    let mut room_rep = make_room(
        1,
        9720,
        r_pos,
        vec![DungeonRuntimeObject {
            type_id: DEFAULT_CONTAINER_TYPE_ID,
            name: "Repeatable Dropoff".into(),
            position: [r_pos[0] + 200.0, 0.0, 0.0],
            radius: 150.0,
            is_container: true,
            ..Default::default()
        }],
        vec![make_trig(
            1,
            DungeonTriggerKind::OnCargoDelivered {
                container_ball_id: None,
                target_object_index: Some(0),
                item_type_id: Some(34),
                required_quantity: 10,
            },
            DungeonTriggerAction::GrantReward {
                isk: 1_000_000,
                lp: 0,
                corp_id: 0,
                faction_id: 0,
                standing_milli: 0,
            },
            true,
        )],
    );
    room_rep.entered = true;
    room_rep.entered_unix = Some(1_785_100_000);

    let mut room_wild = make_room(
        1,
        9721,
        [r_pos[0] + 20_000.0, 0.0, 0.0],
        vec![
            DungeonRuntimeObject {
                type_id: DEFAULT_CONTAINER_TYPE_ID,
                name: "Wildcard Cont 1".into(),
                position: [r_pos[0] + 20_100.0, 0.0, 0.0],
                radius: 150.0,
                is_container: true,
                ..Default::default()
            },
            DungeonRuntimeObject {
                type_id: DEFAULT_CONTAINER_TYPE_ID,
                name: "Wildcard Cont 2".into(),
                position: [r_pos[0] + 20_300.0, 0.0, 0.0],
                radius: 150.0,
                is_container: true,
                ..Default::default()
            },
            DungeonRuntimeObject {
                type_id: 16_757,
                name: "Guard Pylon".into(),
                position: [r_pos[0] + 20_500.0, 0.0, 0.0],
                radius: 150.0,
                destructible: true,
                ..Default::default()
            },
        ],
        vec![make_trig(
            1,
            DungeonTriggerKind::CompoundAnd {
                clauses: vec![
                    DungeonTriggerKind::OnCargoDelivered {
                        container_ball_id: None,
                        target_object_index: None,
                        item_type_id: Some(34),
                        required_quantity: 10,
                    },
                    DungeonTriggerKind::OnStructureDestroyed {
                        target_ball_id: None,
                        target_object_index: Some(2),
                        target_tag: None,
                        target_type_id: None,
                    },
                ],
            },
            DungeonTriggerAction::CompleteDungeon,
            false,
        )],
    );
    room_wild.entered = true;
    room_wild.entered_unix = Some(1_785_100_000);

    let start_isk = ctx.db.with_snapshot(|s| s.characters[&char_id].balance);
    with_dungeon_runtime_store(&ctx, |s| {
        s.instances.insert(
            inst_rep,
            DungeonRuntimeInstance {
                instance_id: inst_rep,
                dungeon_id: 1,
                solar_system_id: sys_id,
                owner_char_id: char_id,
                base_position: r_pos,
                rooms: vec![room_rep],
                created_unix: 1_785_100_000,
                ..Default::default()
            },
        );
        s.instances.insert(
            inst_wild,
            DungeonRuntimeInstance {
                instance_id: inst_wild,
                dungeon_id: 1,
                solar_system_id: sys_id,
                owner_char_id: char_id,
                base_position: [r_pos[0] + 20_000.0, 0.0, 0.0],
                rooms: vec![room_wild],
                created_unix: 1_785_100_000,
                ..Default::default()
            },
        );
    });

    // Part A: Repeatable OnCargoDelivered (15 -> 14 -> 15 must NOT re-fire; 15 -> 9 -> 10 DOES re-fire).
    let mat_rep = materialize_dungeon_room_entities(&ctx, sys_id, inst_rep, 0)
        .await
        .unwrap();
    let rep_cont = mat_rep.object_ids[0];
    on_dungeon_container_cargo_changed(&ctx, char_id, Some(sys_id), 9_001, rep_cont, 34, 15).await;
    assert_eq!(
        ctx.db.with_snapshot(|s| s.characters[&char_id].balance),
        IskCents(start_isk.0 + IskCents::from_isk(1_000_000).0),
        "first delivery 15 >= 10 fires GrantReward once"
    );
    // Withdraw 1 (net 14 >= 10) then re-deposit 1 (net 15): latch must stay set, no second GrantReward.
    on_dungeon_container_cargo_changed(&ctx, char_id, Some(sys_id), rep_cont, 9_001, 34, 1).await;
    on_dungeon_container_cargo_changed(&ctx, char_id, Some(sys_id), 9_001, rep_cont, 34, 1).await;
    assert_eq!(
        ctx.db.with_snapshot(|s| s.characters[&char_id].balance),
        IskCents(start_isk.0 + IskCents::from_isk(1_000_000).0),
        "1-unit bounce above required_quantity (15 -> 14 -> 15) must NOT re-fire repeatable trigger"
    );
    // Withdraw 6 (net 9 < 10) clears latch, then re-deposit 1 (net 10 >= 10) legitimately re-fires!
    on_dungeon_container_cargo_changed(&ctx, char_id, Some(sys_id), rep_cont, 9_001, 34, 6).await;
    on_dungeon_container_cargo_changed(&ctx, char_id, Some(sys_id), 9_001, rep_cont, 34, 1).await;
    assert_eq!(
        ctx.db.with_snapshot(|s| s.characters[&char_id].balance),
        IskCents(start_isk.0 + 2 * IskCents::from_isk(1_000_000).0),
        "dropping below required_quantity (15 -> 9) and re-delivering to 10 must re-fire repeatable trigger"
    );

    // Part B: Two-container wildcard CompoundAnd: deposit into C1 & C2, drain C1 then C2,
    // re-deposit into C1 + destroy Guard Pylon -> must complete without stale C1 latch!
    let mat_wild = materialize_dungeon_room_entities(&ctx, sys_id, inst_wild, 0)
        .await
        .unwrap();
    let (c1, c2, pylon) = (mat_wild.object_ids[0], mat_wild.object_ids[1], mat_wild.object_ids[2]);
    on_dungeon_container_cargo_changed(&ctx, char_id, Some(sys_id), 9_001, c1, 34, 10).await;
    on_dungeon_container_cargo_changed(&ctx, char_id, Some(sys_id), 9_001, c2, 34, 10).await;
    on_dungeon_container_cargo_changed(&ctx, char_id, Some(sys_id), c1, 9_001, 34, 10).await;
    on_dungeon_container_cargo_changed(&ctx, char_id, Some(sys_id), c2, 9_001, 34, 10).await;
    read_dungeon_runtime_store(&ctx, |s| {
        let r = &s.instances[&inst_wild].rooms[0];
        assert!(
            !r.armed_timers
                .keys()
                .any(|k| k.starts_with("__latch:") || k.starts_with("__and:")),
            "emptying both containers in wildcard CompoundAnd must leave zero stale __latch/__and keys: {:?}",
            r.armed_timers
        );
    });
    on_dungeon_container_cargo_changed(&ctx, char_id, Some(sys_id), 9_001, c1, 34, 10).await;
    let _ = crate::combat::death_pipeline::on_ball_destroyed_sync(
        &ctx,
        sys_id,
        pylon,
        Some(char_id),
        DVec3::ZERO,
    );
    read_dungeon_runtime_store(&ctx, |s| {
        let inst = &s.instances[&inst_wild];
        assert!(
            inst.completed && inst.rooms[0].triggers[0].fired,
            "re-depositing into C1 after draining C1 & C2 plus destroying pylon must complete CompoundAnd"
        );
    });
}

#[tokio::test]
async fn own_diff_p1_04_cmd_engage_drones_on_destructible_dungeon_containers_and_cross_room_transfer_order(
) {
    use crate::services::combat_crimewatch::combat_missiles_aoe::is_combatant_ship_or_entity_ball;
    use crate::services::combat_crimewatch::combat_npcs_drones::handle_npcs_drones;
    use crate::services::combat_crimewatch::{is_npc_timer_active, set_char_safety_level, SafetyLevel};
    use eve_destiny::{CombatState, TargetLock};

    let (sys_id, owner_char, green_char, red_char, inst_id, inst_xroom) = (
        30_000_142u32,
        90_070_420i64,
        90_070_421i64,
        90_070_422i64,
        995_420i64,
        995_421i64,
    );
    let r_pos = [80_000_000.0, 0.0, 0.0];

    let mut sde = (*build_hooks_test_sde()).clone();
    sde.types.insert(
        587,
        mk_inv_type(
            587,
            25,
            6,
            "Rifter",
            28_000.0,
            100.0,
            None,
            &[(1271, 50.0), (283, 100.0), (458, 20_000.0)],
        ),
    );
    sde.types.insert(
        2_456,
        mk_inv_type(
            2_456,
            100,
            18,
            "Hobgoblin II",
            5.0,
            0.0,
            None,
            &[
                (1272, 5.0),
                (37, 2_600.0),
                (54, 2_000.0),
                (158, 3_000.0),
                (160, 2.2),
                (64, 1.5),
                (118, 24.0),
                (51, 4_000.0),
            ],
        ),
    );

    let (ctx, _clock, _owner_sess, _owner_ship) = make_space_pilot_ctx(
        Arc::new(sde),
        sys_id,
        owner_char,
        "p104_owner",
        587,
        DVec3::from_array([r_pos[0] + 1_000.0, 0.0, 0.0]),
    )
    .await;
    assert!(
        ctx.sde
            .solar_systems
            .get(&sys_id)
            .is_some_and(|s| s.security_status >= 0.45),
        "test must run in high-sec so Green/Yellow/Red safety & CrimeWatch rules apply"
    );

    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let spawn_fleet_drone_pilot = async |char_id: i64, user_tag: &str, drone_id: i64, offset_x: f64, safety: SafetyLevel| {
        ctx.db.provision_gm_account(user_tag, "pw", char_id, user_tag);
        let (uid, sid) = ctx.db.with_mut_snapshot(|snap| {
            let ch = snap.characters.get_mut(&char_id).unwrap();
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
            let sid = ch.active_ship_id;
            let uid = ch.user_id;
            if let Some(it) = snap.items.get_mut(&sid) {
                it.type_id = 587;
                it.location_id = i64::from(sys_id);
                it.flag = InventoryFlag::None;
            }
            snap.items.insert(
                drone_id,
                InventoryItemRecord {
                    item_id: drone_id,
                    type_id: 2_456,
                    owner_id: char_id,
                    location_id: sid,
                    flag: InventoryFlag::DroneBay,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
            snap.skills.entry(char_id).or_default().insert(
                3436,
                eve_db::TrainedSkillRecord {
                    type_id: 3436,
                    level: 5,
                    skill_points: 256_000.0,
                },
            );
            (uid, sid)
        });
        let _ = ctx.db.acquire_online_lock_with_computer_hash(
            uid,
            char_id as u64,
            Some(char_id),
            None,
            true,
            1_785_100_000,
        );
        sys_h
            .spawn_ball_with_combat(
                Ball {
                    id: sid,
                    type_id: 587,
                    position: DVec3::from_array([r_pos[0] + offset_x, 0.0, 0.0]),
                    velocity: DVec3::ZERO,
                    max_velocity: 2_500.0,
                    inertia: 1.0,
                    mass: 1_000_000.0,
                    radius: 40.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                CombatState {
                    shield_hp: 500.0,
                    max_shield_hp: 500.0,
                    armor_hp: 500.0,
                    max_armor_hp: 500.0,
                    structure_hp: 500.0,
                    max_structure_hp: 500.0,
                    capacitor: 500.0,
                    max_capacitor: 500.0,
                    is_tethered: false,
                    ..CombatState::default()
                },
            )
            .await
            .unwrap();
        ctx.invalidate_ship_fitting(sid);
        set_char_safety_level(&ctx, char_id, safety);
        let mut sess = SessionState::new(char_id as u64);
        sess.user_id = Some(uid);
        sess.character_id = Some(char_id);
        sess.solar_system_id = Some(sys_id);
        sess.ship_id = Some(sid);
        let kwargs = HashMap::new();
        let PyRep::List(bids) = handle_npcs_drones(
            &ctx,
            &mut sess,
            "ship",
            "LaunchDrones",
            &[PyRep::List(vec![PyRep::Integer(drone_id)])],
            &kwargs,
        )
        .await
        .unwrap()
        .expect("LaunchDrones must succeed") else {
            panic!("expected launched drone list");
        };
        (sess, sid, bids[0].as_i64().unwrap())
    };

    let (mut green_sess, green_ship, green_drone_bid) =
        spawn_fleet_drone_pilot(green_char, "p104_green", 882_001, 1_100.0, SafetyLevel::Green).await;
    let (mut red_sess, red_ship, red_drone_bid) =
        spawn_fleet_drone_pilot(red_char, "p104_red", 882_002, 1_200.0, SafetyLevel::Red).await;

    // Two high-HP destructible dungeon containers (16_757 and 23, both is_container: true, destructible: true).
    let mut room = make_room(
        1,
        9730,
        r_pos,
        vec![
            DungeonRuntimeObject {
                type_id: 16_757,
                name: "Angel Relay Cache".into(),
                position: [r_pos[0] + 400.0, 0.0, 0.0],
                radius: 150.0,
                is_container: true,
                destructible: true,
                ..Default::default()
            },
            DungeonRuntimeObject {
                type_id: DEFAULT_CONTAINER_TYPE_ID,
                name: "Volatile Dungeon Box".into(),
                position: [r_pos[0] + 500.0, 0.0, 0.0],
                radius: 150.0,
                is_container: true,
                destructible: true,
                ..Default::default()
            },
        ],
        Vec::new(),
    );
    room.entered = true;
    room.entered_unix = Some(1_785_100_000);

    // P2-NEW-02 2-room instance: dst_cont in room[0] (CompleteDungeon at 10) vs src_cont in room[1] (net=10).
    let mut xr0 = make_room(
        1,
        9731,
        [r_pos[0] + 20_000.0, 0.0, 0.0],
        vec![DungeonRuntimeObject {
            type_id: DEFAULT_CONTAINER_TYPE_ID,
            name: "Room 0 Dropoff".into(),
            position: [r_pos[0] + 20_100.0, 0.0, 0.0],
            radius: 150.0,
            is_container: true,
            ..Default::default()
        }],
        vec![make_trig(
            1,
            DungeonTriggerKind::OnCargoDelivered {
                container_ball_id: None,
                target_object_index: Some(0),
                item_type_id: Some(34),
                required_quantity: 10,
            },
            DungeonTriggerAction::CompleteDungeon,
            false,
        )],
    );
    xr0.entered = true;
    xr0.entered_unix = Some(1_785_100_000);
    let mut xr1 = make_room(
        2,
        9732,
        [r_pos[0] + 30_000.0, 0.0, 0.0],
        vec![DungeonRuntimeObject {
            type_id: DEFAULT_CONTAINER_TYPE_ID,
            name: "Room 1 Source".into(),
            position: [r_pos[0] + 30_100.0, 0.0, 0.0],
            radius: 150.0,
            is_container: true,
            ..Default::default()
        }],
        Vec::new(),
    );
    xr1.entered = true;
    xr1.entered_unix = Some(1_785_100_000);

    with_dungeon_runtime_store(&ctx, |s| {
        s.instances.insert(
            inst_id,
            DungeonRuntimeInstance {
                instance_id: inst_id,
                dungeon_id: 1,
                solar_system_id: sys_id,
                owner_char_id: owner_char,
                base_position: r_pos,
                rooms: vec![room],
                created_unix: 1_785_100_000,
                ..Default::default()
            },
        );
        s.instances.insert(
            inst_xroom,
            DungeonRuntimeInstance {
                instance_id: inst_xroom,
                dungeon_id: 1,
                solar_system_id: sys_id,
                owner_char_id: owner_char,
                base_position: [r_pos[0] + 20_000.0, 0.0, 0.0],
                rooms: vec![xr0, xr1],
                created_unix: 1_785_100_000,
                ..Default::default()
            },
        );
    });

    let mat = materialize_dungeon_room_entities(&ctx, sys_id, inst_id, 0)
        .await
        .unwrap();
    let (cont_16757, cont_23) = (mat.object_ids[0], mat.object_ids[1]);

    // P2-NEW-01: AoE ball predicate includes live destructible type_id=23 dungeon containers,
    // while non-dungeon regular type_id=23 jetcans stay false!
    assert!(
        is_combatant_ship_or_entity_ball(&ctx, cont_23, DEFAULT_CONTAINER_TYPE_ID),
        "P2-NEW-01: live destructible dungeon container (type 23) must be eligible for AoE combat"
    );
    let plain_jetcan_id = 882_998i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            plain_jetcan_id,
            InventoryItemRecord {
                item_id: plain_jetcan_id,
                type_id: DEFAULT_CONTAINER_TYPE_ID,
                owner_id: owner_char,
                location_id: i64::from(sys_id),
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    assert!(
        !is_combatant_ship_or_entity_ball(&ctx, plain_jetcan_id, DEFAULT_CONTAINER_TYPE_ID),
        "regular non-dungeon group 12 Cargo Container must remain ineligible for AoE combat"
    );

    for ship_id in [green_ship, red_ship] {
        for tid in [cont_16757, cont_23] {
            sys_h
                .add_target_lock(
                    ship_id,
                    TargetLock {
                        target_id: tid,
                        lock_time_ms: 0.0,
                        remaining_lock_ms: 0.0,
                        locked: true,
                        max_range_m: Some(50_000.0),
                    },
                )
                .await
                .unwrap();
        }
    }

    let balls_before: std::collections::HashSet<i64> = sys_h
        .list_balls()
        .await
        .unwrap()
        .into_iter()
        .map(|b| b.id)
        .collect();
    let kwargs = HashMap::new();
    for (label, sess, drone_bid, char_id) in [
        ("Green", &mut green_sess, green_drone_bid, green_char),
        ("Red", &mut red_sess, red_drone_bid, red_char),
    ] {
        for (cid_label, cont_id) in [("16757", cont_16757), ("23", cont_23)] {
            handle_npcs_drones(
                &ctx,
                sess,
                "entity",
                "CmdEngage",
                &[PyRep::List(vec![PyRep::Integer(drone_bid)]), PyRep::Integer(cont_id)],
                &kwargs,
            )
            .await
            .unwrap()
            .unwrap_or_else(|e| panic!("{label} CmdEngage on dungeon container {cid_label} must succeed: {e:?}"));
            let _ = handle_npcs_drones(
                &ctx,
                sess,
                "entity",
                "CmdReturnAndOrbit",
                &[PyRep::List(vec![PyRep::Integer(drone_bid)])],
                &kwargs,
            )
            .await;
            if let Ok(Some(mut cs)) = sys_h.get_combat_state(cont_id).await {
                cs.shield_hp = 500.0;
                let _ = sys_h.set_combat_state(cont_id, cs).await;
            }
        }
        let now_unix = ctx.now_unix();
        assert!(
            is_npc_timer_active(&ctx, char_id, now_unix),
            "{label} drone CmdEngage on destructible dungeon container must set NPC timer"
        );
        let mut cw = ctx.crimewatch.write().unwrap();
        let st = cw.get_or_create(char_id);
        assert_eq!(st.pvp_timer_until_unix, 0, "{label} CmdEngage must NOT set PvP timer");
        assert_eq!(st.suspect_flag_until_unix, 0, "{label} CmdEngage must NOT set Suspect flag");
        assert_eq!(st.criminal_flag_until_unix, 0, "{label} CmdEngage must NOT set Criminal flag");
    }
    ctx.db.with_snapshot(|snap| {
        assert!(snap.kill_rights.is_empty(), "drone engagement on dungeon container must create zero kill rights");
    });
    let spawned_during_engage: Vec<i64> = sys_h
        .list_balls()
        .await
        .unwrap()
        .into_iter()
        .map(|b| b.id)
        .filter(|id| !balls_before.contains(id))
        .collect();
    assert!(
        spawned_during_engage.is_empty(),
        "no CONCORD response balls may spawn when fleetmates engage destructible dungeon containers: {spawned_during_engage:?}"
    );

    // P2-NEW-02: cross-room src (room 1) -> dst (room 0, CompleteDungeon) transfer debits room 1 BEFORE completing room 0.
    let xr_m0 = materialize_dungeon_room_entities(&ctx, sys_id, inst_xroom, 0)
        .await
        .unwrap();
    let xr_m1 = materialize_dungeon_room_entities(&ctx, sys_id, inst_xroom, 1)
        .await
        .unwrap();
    let (dst_r0, src_r1) = (xr_m0.object_ids[0], xr_m1.object_ids[0]);
    on_dungeon_container_cargo_changed(&ctx, owner_char, Some(sys_id), 9_001, src_r1, 34, 10).await;
    on_dungeon_container_cargo_changed(&ctx, owner_char, Some(sys_id), src_r1, dst_r0, 34, 10).await;
    read_dungeon_runtime_store(&ctx, |s| {
        let inst = &s.instances[&inst_xroom];
        assert!(inst.completed && inst.rooms[0].triggers[0].fired);
        assert!(
            !inst.rooms[1].cargo_delivered_net.contains_key(&format!("{src_r1}:34")),
            "P2-NEW-02: room[1] source debit must run before room[0] CompleteDungeon"
        );
        assert_eq!(inst.rooms[0].cargo_delivered_net.get(&format!("{dst_r0}:34")), Some(&10));
    });
}

#[tokio::test]
async fn pre_exist_p1_01_and_p2_note_01_lowsec_disruptive_lance_and_indestructible_dungeon_can_zero_pvp_flags(
) {
    use crate::services::combat_crimewatch::combat_weapons::handle_weapons;
    use crate::services::combat_crimewatch::{is_npc_timer_active, set_char_safety_level, SafetyLevel};
    use crate::services::r6_t1_p0_helpers::execute_lancer_beam_attack;
    use eve_destiny::{CombatState, TargetLock};

    let (lowsec_sys, highsec_sys, owner_char, lancer_char, green_char, inst_low, inst_high) = (
        30_002_813u32,
        30_000_142u32,
        90_070_430i64,
        90_070_431i64,
        90_070_432i64,
        995_430i64,
        995_431i64,
    );

    let mut sde = (*build_hooks_test_sde()).clone();
    sde.types.insert(
        37_901,
        mk_inv_type(37_901, 379, 7, "Target Painter I", 5.0, 0.0, None, &[(554, 25.0)]),
    );
    let (ctx, _clock, _owner_sess, _owner_ship) = make_space_pilot_ctx(
        Arc::new(sde),
        lowsec_sys,
        owner_char,
        "p101_owner",
        587,
        DVec3::ZERO,
    )
    .await;

    ctx.db.with_mut_snapshot(|snap| {
        snap.combat_crimewatch_state
            .modified_system_securities
            .insert(lowsec_sys, 0.30);
    });

    // Seed Red-safety Lancer shooter in low-sec and Green-safety fleetmate in high-sec.
    let spawn_pilot = async |sys_id: u32, char_id: i64, user_tag: &str, safety: SafetyLevel, tp_id: Option<i64>| {
        ctx.db.provision_gm_account(user_tag, "pw", char_id, user_tag);
        let (uid, sid) = ctx.db.with_mut_snapshot(|snap| {
            let ch = snap.characters.get_mut(&char_id).unwrap();
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
            let sid = ch.active_ship_id;
            let uid = ch.user_id;
            if let Some(it) = snap.items.get_mut(&sid) {
                it.type_id = 587;
                it.location_id = i64::from(sys_id);
                it.flag = InventoryFlag::None;
            }
            if let Some(mod_id) = tp_id {
                snap.items.insert(
                    mod_id,
                    InventoryItemRecord {
                        item_id: mod_id,
                        type_id: 37_901,
                        owner_id: char_id,
                        location_id: sid,
                        flag: InventoryFlag::MedSlot0,
                        quantity: 1,
                        singleton: true,
                        version: 1,
                        ..Default::default()
                    },
                );
            }
            (uid, sid)
        });
        let _ = ctx.db.acquire_online_lock_with_computer_hash(
            uid,
            char_id as u64,
            Some(char_id),
            None,
            true,
            1_785_100_000,
        );
        let sys_h = ctx.systems.get_or_spawn(sys_id);
        sys_h
            .spawn_ball_with_combat(
                Ball {
                    id: sid,
                    type_id: 587,
                    position: DVec3::ZERO,
                    velocity: DVec3::ZERO,
                    max_velocity: 2_500.0,
                    inertia: 1.0,
                    mass: 1_000_000.0,
                    radius: 40.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                CombatState {
                    shield_hp: 500.0,
                    max_shield_hp: 500.0,
                    armor_hp: 500.0,
                    max_armor_hp: 500.0,
                    structure_hp: 500.0,
                    max_structure_hp: 500.0,
                    capacitor: 500.0,
                    max_capacitor: 500.0,
                    is_tethered: false,
                    ..CombatState::default()
                },
            )
            .await
            .unwrap();
        ctx.invalidate_ship_fitting(sid);
        set_char_safety_level(&ctx, char_id, safety);
        let mut sess = SessionState::new(char_id as u64);
        sess.user_id = Some(uid);
        sess.character_id = Some(char_id);
        sess.solar_system_id = Some(sys_id);
        sess.ship_id = Some(sid);
        (sess, sid)
    };

    let (_lancer_sess, lancer_ship) =
        spawn_pilot(lowsec_sys, lancer_char, "p101_lancer", SafetyLevel::Red, None).await;
    let tp_mod_id = 883_001i64;
    let (mut green_sess, green_ship) =
        spawn_pilot(highsec_sys, green_char, "p101_green", SafetyLevel::Green, Some(tp_mod_id)).await;

    let mut r_low = make_room(
        1,
        9740,
        [0.0, 0.0, 0.0],
        vec![DungeonRuntimeObject {
            type_id: DEFAULT_CONTAINER_TYPE_ID,
            name: "Lowsec Destructible Can".into(),
            position: [10_000.0, 0.0, 0.0],
            radius: 150.0,
            is_container: true,
            destructible: true,
            ..Default::default()
        }],
        Vec::new(),
    );
    r_low.entered = true;
    r_low.entered_unix = Some(1_785_100_000);

    let mut r_high = make_room(
        1,
        9741,
        [0.0, 0.0, 0.0],
        vec![DungeonRuntimeObject {
            type_id: DEFAULT_CONTAINER_TYPE_ID,
            name: "Highsec Indestructible Can".into(),
            position: [2_000.0, 0.0, 0.0],
            radius: 150.0,
            is_container: true,
            destructible: false,
            ..Default::default()
        }],
        Vec::new(),
    );
    r_high.entered = true;
    r_high.entered_unix = Some(1_785_100_000);

    with_dungeon_runtime_store(&ctx, |s| {
        s.instances.insert(
            inst_low,
            DungeonRuntimeInstance {
                instance_id: inst_low,
                dungeon_id: 1,
                solar_system_id: lowsec_sys,
                owner_char_id: owner_char,
                base_position: [0.0, 0.0, 0.0],
                rooms: vec![r_low],
                created_unix: 1_785_100_000,
                ..Default::default()
            },
        );
        s.instances.insert(
            inst_high,
            DungeonRuntimeInstance {
                instance_id: inst_high,
                dungeon_id: 1,
                solar_system_id: highsec_sys,
                owner_char_id: owner_char,
                base_position: [0.0, 0.0, 0.0],
                rooms: vec![r_high],
                created_unix: 1_785_100_000,
                ..Default::default()
            },
        );
    });

    // 1. Low-sec Disruptive Lance cone hits destructible dungeon container (23):
    // must hit without Suspect/Criminal/PvP flags and set only NPC aggression timer.
    let mat_low = materialize_dungeon_room_entities(&ctx, lowsec_sys, inst_low, 0)
        .await
        .unwrap();
    let cont_low = mat_low.object_ids[0];
    let low_sys_h = ctx.systems.get_or_spawn(lowsec_sys);
    let (_, cont_ball) = low_sys_h.get_ball_snapshot(cont_low).await.unwrap().unwrap();
    let now_unix = ctx.now_unix();
    let hits = execute_lancer_beam_attack(
        &ctx,
        &low_sys_h,
        lowsec_sys,
        lancer_char,
        lancer_ship,
        77_281,
        77_398,
        &[cont_ball],
        DVec3::ZERO,
        DVec3::X,
        100_000.0,
        5_000.0,
        5_000.0,
        DamageVector::new(100.0, 0.0, 0.0, 0.0),
        100.0,
        60,
        now_unix,
    )
    .await
    .expect("low-sec Disruptive Lance on destructible dungeon container must succeed");
    assert!(hits.contains(&cont_low));
    assert!(is_npc_timer_active(&ctx, lancer_char, now_unix));
    {
        let mut cw = ctx.crimewatch.write().unwrap();
        let st = cw.get_or_create(lancer_char);
        assert_eq!(st.suspect_flag_until_unix, 0, "PRE-EXIST-P1-01: Disruptive Lance on dungeon container must NOT incur Suspect flag");
        assert_eq!(st.criminal_flag_until_unix, 0, "must NOT incur Criminal flag");
        assert_eq!(st.pvp_timer_until_unix, 0, "must NOT incur PvP timer");
    }

    // 2. P2-NOTE-01: Green-safety fleetmate activating Target Painter (dogmaIM.Activate) on an
    // indestructible dungeon container (is_container=true, destructible=false) in high-sec must NOT reject.
    let mat_high = materialize_dungeon_room_entities(&ctx, highsec_sys, inst_high, 0)
        .await
        .unwrap();
    let cont_high = mat_high.object_ids[0];
    let high_sys_h = ctx.systems.get_or_spawn(highsec_sys);
    high_sys_h
        .add_target_lock(
            green_ship,
            TargetLock {
                target_id: cont_high,
                lock_time_ms: 0.0,
                remaining_lock_ms: 0.0,
                locked: true,
                max_range_m: Some(50_000.0),
            },
        )
        .await
        .unwrap();
    let kwargs = HashMap::new();
    handle_weapons(
        &ctx,
        &mut green_sess,
        "dogmaIM",
        "Activate",
        &[
            PyRep::Integer(tp_mod_id),
            PyRep::String("targetAttack".into()),
            PyRep::Integer(cont_high),
        ],
        &kwargs,
    )
    .await
    .unwrap()
    .expect("P2-NOTE-01: Green-safety EWAR on indestructible dungeon container must not reject");
    {
        let mut cw = ctx.crimewatch.write().unwrap();
        let st = cw.get_or_create(green_char);
        assert_eq!(st.suspect_flag_until_unix, 0);
        assert_eq!(st.criminal_flag_until_unix, 0);
        assert_eq!(st.pvp_timer_until_unix, 0);
    }
}


#[tokio::test]
async fn own_diff_p1_08_and_09_trash_and_launch_from_container_preserve_dungeon_cans_and_net_cargo() {
    let mut sde = build_hooks_test_sde();
    Arc::get_mut(&mut sde).unwrap().types.insert(
        340_001,
        mk_inv_type(340_001, 340, 2, "Small Secure Container", 5.0, 50.0, None, &[]),
    );
    let sys_id = 30_000_142;
    let char_id = 95_008_701;
    let inst_id = 8_701_001;
    let r_pos = [12_000.0, 0.0, 0.0];
    let (ctx, _clock, mut sess, ship_id) =
        make_space_pilot_ctx(sde, sys_id, char_id, "r7_trash_launch", 587, DVec3::new(r_pos[0] + 50.0, 0.0, 0.0)).await;
    let mut room = make_room(
        0,
        9_701,
        r_pos,
        vec![
            DungeonRuntimeObject {
                type_id: DEFAULT_CONTAINER_TYPE_ID,
                name: "Supply Can".into(),
                position: [r_pos[0] + 100.0, 0.0, 0.0],
                radius: 150.0,
                is_container: true,
                ..Default::default()
            },
            DungeonRuntimeObject {
                type_id: 16_757,
                name: "Relay Bunker".into(),
                position: [r_pos[0] + 300.0, 0.0, 0.0],
                radius: 150.0,
                is_container: true,
                destructible: true,
                tag: Some("bunker".into()),
                ..Default::default()
            },
        ],
        vec![
            make_trig(
                1,
                DungeonTriggerKind::CompoundAnd {
                    clauses: vec![
                        DungeonTriggerKind::OnCargoDelivered {
                            container_ball_id: None,
                            target_object_index: Some(0),
                            item_type_id: Some(34),
                            required_quantity: 10,
                        },
                        DungeonTriggerKind::OnStructureDestroyed {
                            target_ball_id: None,
                            target_object_index: None,
                            target_tag: Some("bunker".into()),
                            target_type_id: None,
                        },
                    ],
                },
                DungeonTriggerAction::UnlockGate,
                false,
            ),
            make_trig(
                2,
                DungeonTriggerKind::OnCargoDelivered {
                    container_ball_id: None,
                    target_object_index: Some(1),
                    item_type_id: Some(340_001),
                    required_quantity: 2,
                },
                DungeonTriggerAction::CompleteDungeon,
                false,
            ),
        ],
    );
    room.entered = true;
    room.entered_unix = Some(1_785_100_000);
    with_dungeon_runtime_store(&ctx, |s| {
        s.instances.insert(
            inst_id,
            DungeonRuntimeInstance {
                instance_id: inst_id,
                dungeon_id: 1,
                solar_system_id: sys_id,
                owner_char_id: char_id,
                base_position: r_pos,
                rooms: vec![room],
                created_unix: 1_785_100_000,
                ..Default::default()
            },
        );
    });
    let mat = materialize_dungeon_room_entities(&ctx, sys_id, inst_id, 0).await.expect("mat");
    let (cont_23, cont_bunker) = (mat.object_ids[0], mat.object_ids[1]);
    let sys_h = ctx.systems.get_or_spawn(sys_id);

    // OWN-DIFF-P1-08(a): Direct TrashItems on dungeon objects is rejected cleanly.
    for obj_id in [cont_23, cont_bunker] {
        let err = crate::services::inventory_fitting::fitting_ships_mastery::handle_ships_mastery_and_fitting_ext(
            &ctx, &mut sess, None, "invbroker", "TrashItems", &[PyRep::List(vec![PyRep::Integer(obj_id)])], &HashMap::new(),
        ).await.expect("handled").unwrap_err();
        assert_eq!(err.code, "DeleteFailed");
        assert!(err.message.contains("dungeon object"));
        assert!(ctx.db.with_snapshot(|s| s.items.contains_key(&obj_id)));
    }

    // OWN-DIFF-P1-08(b): Deliver 10 Tritanium -> TrashItems deletes stack, preserves empty type_id=23 cont_23, debits net cargo & unlatches CompoundAnd.
    insert_ship_item(&ctx, char_id, ship_id, 8_701_101, 34, InventoryFlag::Cargo, 10, false);
    inv_add_to(&ctx, &mut sess, char_id, cont_23, 8_701_101, ship_id, Some(10)).await;
    let trit_id = ctx.db.with_snapshot(|s| s.items.values().find(|i| i.location_id == cont_23 && i.type_id == 34).unwrap().item_id);
    crate::services::inventory_fitting::fitting_ships_mastery::handle_ships_mastery_and_fitting_ext(
        &ctx, &mut sess, None, "invbroker", "TrashItems", &[PyRep::List(vec![PyRep::Integer(trit_id)])], &HashMap::new(),
    ).await.expect("handled").expect("TrashItems cargo ok");
    assert!(ctx.db.with_snapshot(|s| !s.items.contains_key(&trit_id) && s.items.contains_key(&cont_23)));
    assert!(sys_h.get_ball_snapshot(cont_23).await.unwrap().is_some(), "empty dungeon cont_23 must stay in space");
    read_dungeon_runtime_store(&ctx, |s| {
        let r = &s.instances[&inst_id].rooms[0];
        assert!(r.cargo_delivered_net.get(&format!("{cont_23}:34")).copied().unwrap_or(0) == 0);
        assert!(!r.armed_timers.keys().any(|k| k.starts_with("__latch:") || k.starts_with("__and:")));
    });
    // OWN-DIFF-P1-09: Add -> LaunchFromContainer -> Scoop -> Add loop stays capped at net 1 (never reaches 2).
    let player_can = 8_701_102;
    insert_ship_item(&ctx, char_id, ship_id, player_can, 340_001, InventoryFlag::Cargo, 1, true);
    inv_add_to(&ctx, &mut sess, char_id, cont_bunker, player_can, ship_id, Some(1)).await;
    read_dungeon_runtime_store(&ctx, |s| assert_eq!(s.instances[&inst_id].rooms[0].cargo_delivered_net.get(&format!("{cont_bunker}:340001")).copied(), Some(1)));
    crate::services::inventory_fitting::fitting_ships_mastery::handle_ships_mastery_and_fitting_ext(
        &ctx, &mut sess, None, "ship", "LaunchFromContainer", &[PyRep::Integer(player_can)], &HashMap::new(),
    ).await.expect("handled").expect("LaunchFromContainer ok");
    read_dungeon_runtime_store(&ctx, |s| assert_eq!(s.instances[&inst_id].rooms[0].cargo_delivered_net.get(&format!("{cont_bunker}:340001")).copied().unwrap_or(0), 0));
    crate::services::inventory_fitting::fitting_ships_mastery::handle_ships_mastery_and_fitting_ext(
        &ctx, &mut sess, None, "ship", "Scoop", &[PyRep::Integer(player_can)], &HashMap::new(),
    ).await.expect("handled").expect("Scoop ok");
    inv_add_to(&ctx, &mut sess, char_id, cont_bunker, player_can, ship_id, Some(1)).await;
    read_dungeon_runtime_store(&ctx, |s| {
        let inst = &s.instances[&inst_id];
        assert_eq!(inst.rooms[0].cargo_delivered_net.get(&format!("{cont_bunker}:340001")).copied(), Some(1));
        assert!(!inst.completed && !inst.rooms[0].triggers[1].fired);
    });
    let _ = crate::combat::death_pipeline::on_ball_destroyed_sync(&ctx, sys_id, cont_bunker, Some(char_id), DVec3::ZERO).unwrap();
    read_dungeon_runtime_store(&ctx, |s| assert!(!s.instances[&inst_id].rooms[0].triggers[0].fired && s.instances[&inst_id].rooms[0].gate_locked));
}

#[tokio::test]
async fn own_diff_p1_10_and_11_mtu_tractor_and_scoop_cannot_touch_dungeon_containers() {
    use crate::services::combat_crimewatch::combat_npcs_drones::precheck_tractor_beam_activation;
    use crate::services::inventory_fitting::fitting_ships_mastery::handle_ships_mastery_and_fitting_ext;
    use crate::services::mobile_deployables::{
        apply_mobile_deployable_in_snap, register_mobile_deployable,
        step_mobile_deployables_in_actor_collecting, write_deployable_state, MobileDeployableDbOp,
        CATEGORY_DEPLOYABLE, GROUP_MOBILE_TRACTOR, TYPE_MTU_STANDARD,
    };
    use crate::system_actor::post_unlock::{PostUnlockIntent, PostUnlockOps};

    let mut sde = build_hooks_test_sde();
    let tractor_type = mk_inv_type(650_001, 650, 7, "Small Tractor Beam I", 5.0, 0.0, None, &[(54, 20_000.0)]);
    {
        let sm = Arc::get_mut(&mut sde).unwrap();
        sm.types.insert(TYPE_MTU_STANDARD, mk_inv_type(TYPE_MTU_STANDARD, GROUP_MOBILE_TRACTOR, CATEGORY_DEPLOYABLE, "Mobile Tractor Unit", 100.0, 27_000.0, None, &[(1556, 27_000.0), (54, 125_000.0)]));
        sm.types.insert(340_002, mk_inv_type(340_002, 340, 2, "Dungeon Secure Can", 1_000.0, 3_900.0, None, &[]));
        sm.types.insert(448_002, mk_inv_type(448_002, 448, 2, "Dungeon Station Can", 1_000.0, 100_000.0, None, &[]));
        sm.types.insert(650_001, tractor_type.clone());
    }
    let sys_id = 30_000_142;
    let char_id = 95_008_801;
    let inst_id = 8_801_001;
    let r_pos = [24_000.0, 0.0, 0.0];
    let (ctx, _clock, mut sess, ship_id) =
        make_space_pilot_ctx(sde, sys_id, char_id, "r8_mtu_scoop", 587, DVec3::new(r_pos[0] + 50.0, 0.0, 0.0)).await;
    let mut room = make_room(
        0,
        9_801,
        r_pos,
        vec![
            DungeonRuntimeObject { type_id: DEFAULT_CONTAINER_TYPE_ID, name: "Can 23".into(), position: [r_pos[0] + 100.0, 0.0, 0.0], radius: 150.0, is_container: true, loot_items: vec![(35, 5)], ..Default::default() },
            DungeonRuntimeObject { type_id: 340_002, name: "Can 340".into(), position: [r_pos[0] + 4_000.0, 0.0, 0.0], radius: 150.0, is_container: true, loot_items: vec![(34, 5)], ..Default::default() },
            DungeonRuntimeObject { type_id: 448_002, name: "Can 448".into(), position: [r_pos[0] + 5_000.0, 0.0, 0.0], radius: 150.0, is_container: true, loot_items: vec![(34, 5)], ..Default::default() },
            DungeonRuntimeObject { type_id: 16_757, name: "Bunker".into(), position: [r_pos[0] + 150.0, 0.0, 0.0], radius: 150.0, is_container: true, destructible: true, ..Default::default() },
        ],
        vec![make_trig(
            1,
            DungeonTriggerKind::OnCargoDelivered { container_ball_id: None, target_object_index: Some(0), item_type_id: Some(34), required_quantity: 2 },
            DungeonTriggerAction::CompleteDungeon,
            false,
        )],
    );
    room.entered = true;
    room.entered_unix = Some(1_785_100_000);
    with_dungeon_runtime_store(&ctx, |s| {
        s.instances.insert(inst_id, DungeonRuntimeInstance { instance_id: inst_id, dungeon_id: 1, solar_system_id: sys_id, owner_char_id: char_id, base_position: r_pos, rooms: vec![room], created_unix: 1_785_100_000, ..Default::default() });
    });
    let mat = materialize_dungeon_room_entities(&ctx, sys_id, inst_id, 0).await.expect("mat");
    let (cont_23, cont_340, cont_448, cont_16757) = (mat.object_ids[0], mat.object_ids[1], mat.object_ids[2], mat.object_ids[3]);
    let sys_h = ctx.systems.get_or_spawn(sys_id);

    // OWN-DIFF-P1-11: Scoop / ScoopToFleetHangar reject non-23 dungeon container (empty & non-empty).
    for step_fill in [false, true] {
        if step_fill {
            insert_ship_item(&ctx, char_id, ship_id, 8_801_201, 34, InventoryFlag::Cargo, 5, false);
            inv_add_to(&ctx, &mut sess, char_id, cont_16757, 8_801_201, ship_id, Some(5)).await;
        }
        for method in ["Scoop", "ScoopToFleetHangar"] {
            let err = handle_ships_mastery_and_fitting_ext(&ctx, &mut sess, None, "ship", method, &[PyRep::Integer(cont_16757)], &HashMap::new())
                .await.expect("handled").unwrap_err();
            assert_eq!(err.code, "CannotScoop");
            assert!(ctx.db.with_snapshot(|s| s.items.get(&cont_16757).is_some_and(|i| i.location_id == i64::from(sys_id))));
            assert!(sys_h.get_ball_snapshot(cont_16757).await.unwrap().is_some());
        }
    }

    // OWN-DIFF-P1-10: Deliver 1 Tritanium into cont_23, deploy MTU + active control jetcan, and verify MTU/Tractor reject dungeon cans.
    insert_ship_item(&ctx, char_id, ship_id, 8_801_202, 34, InventoryFlag::Cargo, 1, false);
    inv_add_to(&ctx, &mut sess, char_id, cont_23, 8_801_202, ship_id, Some(1)).await;
    let (mtu_id, player_can, player_loot) = (8_801_301, 8_801_302, 8_801_303);
    let mtu_pos = DVec3::new(r_pos[0] + 120.0, 0.0, 0.0);
    let can_pos = DVec3::new(r_pos[0] + 140.0, 0.0, 0.0);
    ctx.db.with_mut_snapshot(|s| {
        for (id, tid, loc, pos) in [(mtu_id, TYPE_MTU_STANDARD, i64::from(sys_id), Some(mtu_pos)), (player_can, 23, i64::from(sys_id), Some(can_pos)), (player_loot, 35, player_can, None)] {
            s.items.insert(id, InventoryItemRecord { item_id: id, type_id: tid, owner_id: char_id, location_id: loc, flag: InventoryFlag::None, quantity: 1, singleton: pos.is_some(), version: 1, ..Default::default() });
            if let Some(p) = pos { s.item_positions.insert(id, [p.x, p.y, p.z]); }
        }
    });
    for (id, tid, pos) in [(mtu_id, TYPE_MTU_STANDARD, mtu_pos), (player_can, 23, can_pos)] {
        sys_h.spawn_ball(Ball { id, type_id: tid, position: pos, velocity: DVec3::ZERO, max_velocity: 0.0, inertia: 1.0, mass: 1_000.0, radius: 100.0, speed_fraction: 0.0, mode: MovementMode::Stop }).await.unwrap();
    }
    register_mobile_deployable(&ctx, mtu_id, TYPE_MTU_STANDARD, GROUP_MOBILE_TRACTOR, char_id, sys_id, 0, mtu_pos);
    write_deployable_state(&ctx, |st| { st.deployables.get_mut(&mtu_id).unwrap().anchor_completed = true; });
    let (sde_c, db_c, rng_c) = (Arc::clone(&ctx.sde), Arc::clone(&ctx.db), Arc::clone(&ctx.mechanic_rng));
    let intents = sys_h.test_with_ballpark_and_state_mut_async(move |bp, st| {
        let mut ops = PostUnlockOps::default();
        step_mobile_deployables_in_actor_collecting(bp, &mut st.active_module_cycles, &rng_c, Some(&sde_c), Some(&db_c), Some(&rng_c), sys_id, 1.0, 1_785_100_005, None, None, &mut ops);
        assert_eq!(bp.balls[&cont_340].position, DVec3::new(r_pos[0] + 4_000.0, 0.0, 0.0));
        assert_eq!(bp.balls[&cont_448].position, DVec3::new(r_pos[0] + 5_000.0, 0.0, 0.0));
        ops.intents
    }).await;
    assert!(intents.iter().any(|i| matches!(i, PostUnlockIntent::MobileDeployable(MobileDeployableDbOp::LootMtuCargo { target_id, .. }) if *target_id == player_can)));
    assert!(!intents.iter().any(|i| matches!(i, PostUnlockIntent::MobileDeployable(MobileDeployableDbOp::LootMtuCargo { target_id, .. } | MobileDeployableDbOp::UpdateMtuTargetPosition { target_id, .. }) if [cont_23, cont_340, cont_448, cont_16757].contains(target_id))));

    // Defensive DB-op guard + Tractor Beam precheck + net cargo anti-recycle invariant:
    ctx.db.with_mut_snapshot(|s| {
        apply_mobile_deployable_in_snap(s, Some(&ctx.sde), &MobileDeployableDbOp::LootMtuCargo { mtu_item_id: mtu_id, target_id: cont_23, mtu_capacity: 27_000.0 });
        apply_mobile_deployable_in_snap(s, Some(&ctx.sde), &MobileDeployableDbOp::UpdateMtuTargetPosition { target_id: cont_23, new_pos: [0.0, 0.0, 0.0] });
        assert!(s.items.values().any(|i| i.location_id == cont_23 && i.type_id == 34));
        assert!(s.items.values().any(|i| i.location_id == cont_23 && i.type_id == 35));
        assert!(!s.items.values().any(|i| i.location_id == mtu_id));
        assert_eq!(s.item_positions[&cont_23], [r_pos[0] + 100.0, 0.0, 0.0]);
    });
    assert_eq!(precheck_tractor_beam_activation(&ctx, &sys_h, char_id, ship_id, 0, &tractor_type, Some(cont_23)).await.unwrap_err().code, "TractorBeamDungeonObjectRestricted");
    read_dungeon_runtime_store(&ctx, |s| {
        let inst = &s.instances[&inst_id];
        assert_eq!(inst.rooms[0].cargo_delivered_net.get(&format!("{cont_23}:34")).copied(), Some(1));
        assert!(!inst.completed && !inst.rooms[0].triggers[0].fired);
    });
}
