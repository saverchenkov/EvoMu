//! Shared non-GM ROLE_PLAYER test helpers for Shard 3 parity integration tests.
#![forbid(unsafe_code)]
#![allow(dead_code)]
use eve_db::{InventoryFlag, InventoryItemRecord, IskCents, WorldRepository};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use glam::DVec3;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub(crate) const CALDARI: i64 = 500_001;
pub(crate) const GALLENTE: i64 = 500_004;
pub(crate) const GURISTAS: i64 = 500_010;
pub(crate) const HURRICANE_TYPE: u32 = 24702;
pub(crate) const AUTOCANNON_425_TYPE: u32 = 2929;
pub(crate) const GYRO_TYPE: u32 = 520;
pub(crate) const FUSION_M_AMMO: u32 = 199;
pub(crate) const ENTOSIS_LINK_I_TYPE: u32 = 34_593;
pub(crate) const STRONTIUM_TYPE: u32 = 16_275;

#[derive(Clone)]
pub(crate) struct PlayerPilot {
    pub sess: SessionState,
    pub char_id: i64,
    pub corp_id: i64,
    pub ship_id: i64,
}

pub(crate) async fn call_rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None)
        .await
}

pub(crate) async fn call_bound_rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    bound_id: u64,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(
        sess,
        service,
        Some(bound_id),
        method,
        args,
        &HashMap::new(),
        None,
    )
    .await
}

pub(crate) fn bound_id_of(rep: &PyRep) -> u64 {
    match rep {
        PyRep::Tuple(t) if !t.is_empty() => t[0].as_i64().unwrap_or(1) as u64,
        other => other.as_i64().unwrap_or(1) as u64,
    }
}

pub(crate) fn grant_skill_tree(
    ctx: &ServerContext,
    char_id: i64,
    _root_type_ids: &[u32],
    _extra_skills: &[u32],
) {
    let skill_ids: Vec<u32> = ctx
        .sde
        .types
        .values()
        .filter(|t| t.category_id == 16)
        .map(|t| t.type_id)
        .collect();
    ctx.db.with_mut_snapshot(|snap| {
        let map = snap.skills.entry(char_id).or_default();
        for sk in skill_ids {
            map.insert(
                sk,
                eve_db::TrainedSkillRecord {
                    type_id: sk,
                    level: 5,
                    skill_points: 256_000.0,
                },
            );
        }
    });
}

/// Provisions a genuine `ROLE_PLAYER` account (`eve_machonet::ROLE_PLAYER`, non-GM), creates and selects a
/// character, grants requested skills/ISK, fits an active cruiser/battlecruiser via `invbroker`
/// + `dogmaIM`, and undocks into space.
pub(crate) fn sde_type_by_name(ctx: &ServerContext, name: &str) -> u32 {
    ctx.sde
        .types
        .values()
        .find(|t| t.name == name)
        .unwrap_or_else(|| panic!("SDE type '{name}' not found"))
        .type_id
}

pub(crate) async fn spawn_player_combat_pilot(
    ctx: &ServerContext,
    sess_id: u64,
    username: &str,
    char_name: &str,
    hull_type_id: u32,
    fit_entosis: bool,
) -> PlayerPilot {
    ctx.db.provision_player_account(username, "pw-player");
    let mut sess = SessionState::new(sess_id);
    crate::common::login_pilot(ctx, &mut sess, username, "pw-player").await;
    let ancestry_id = ctx
        .sde
        .ancestries
        .values()
        .find(|a| a.bloodline_id == 1)
        .map(|a| i64::from(a.ancestry_id))
        .unwrap_or(1);
    let cid_rep = call_rpc(
        ctx,
        &mut sess,
        "charUnboundMgr",
        "CreateCharacterWithDoll",
        &[
            PyRep::String(char_name.into()),
            PyRep::Integer(1),
            PyRep::Integer(1),
            PyRep::Integer(ancestry_id),
            PyRep::String(format!("doll:{username}")),
            PyRep::String(format!("portrait:{username}")),
            PyRep::Integer(17),
        ],
    )
    .await
    .expect("CreateCharacterWithDoll");
    let char_id = cid_rep.as_i64().expect("character id");
    call_rpc(
        ctx,
        &mut sess,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
    )
    .await
    .expect("SelectCharacterID");

    let gun_tid = sde_type_by_name(ctx, "425mm AutoCannon II");
    let gyro_tid = sde_type_by_name(ctx, "Gyrostabilizer II");
    let ammo_tid = sde_type_by_name(ctx, "EMP M");
    let entosis_tid = sde_type_by_name(ctx, "Entosis Link I");
    grant_skill_tree(
        ctx,
        char_id,
        &[hull_type_id, gun_tid, gyro_tid, entosis_tid],
        &[3426, 3413, 3318, 11207, 3429, 3363, 11584, 3300, 3304],
    );

    let (corp_id, station_id, ship_id, gun_ids, all_mod_ids, ammo_id) =
        ctx.db.with_mut_snapshot(|snap| {
            let corp_id = snap.characters[&char_id].corporation_id;
            let station_id = snap.characters[&char_id].station_id.expect("station_id");
            let uid = snap.characters[&char_id].user_id;
            if let Some(acc) = snap.accounts.values().find(|a| a.user_id == uid) {
                assert_eq!(acc.role, 0x6000_0000_0000_0000, "account must remain non-GM ROLE_PLAYER");
            }
            if let Some(ch) = snap.characters.get_mut(&char_id) {
                ch.clone_grade = 2;
                ch.balance = IskCents::from_isk(5_000_000_000);
            }
            snap.next_entity_id += 1;
            let ship_id = snap.next_entity_id;
            snap.items.insert(
                ship_id,
                InventoryItemRecord {
                    item_id: ship_id,
                    type_id: hull_type_id,
                    owner_id: char_id,
                    location_id: station_id,
                    flag: InventoryFlag::Hangar,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
            let mut gun_ids = Vec::new();
            let mut mod_ids = Vec::new();
            let hi_flags = [
                InventoryFlag::HiSlot0,
                InventoryFlag::HiSlot1,
                InventoryFlag::HiSlot2,
                InventoryFlag::HiSlot3,
                InventoryFlag::HiSlot4,
                InventoryFlag::HiSlot5,
            ];
            for (idx, flag) in hi_flags.into_iter().enumerate() {
                snap.next_entity_id += 1;
                let gid = snap.next_entity_id;
                let tid = if fit_entosis && idx == 5 {
                    entosis_tid
                } else {
                    gun_tid
                };
                snap.items.insert(
                    gid,
                    InventoryItemRecord {
                        item_id: gid,
                        type_id: tid,
                        owner_id: char_id,
                        location_id: station_id,
                        flag: InventoryFlag::Hangar,
                        quantity: 1,
                        singleton: true,
                        ..Default::default()
                    },
                );
                if tid == gun_tid {
                    gun_ids.push((gid, flag));
                }
                mod_ids.push((gid, flag));
            }
            for flag in [
                InventoryFlag::LoSlot0,
                InventoryFlag::LoSlot1,
                InventoryFlag::LoSlot2,
                InventoryFlag::LoSlot3,
            ] {
                snap.next_entity_id += 1;
                let mid = snap.next_entity_id;
                snap.items.insert(
                    mid,
                    InventoryItemRecord {
                        item_id: mid,
                        type_id: gyro_tid,
                        owner_id: char_id,
                        location_id: station_id,
                        flag: InventoryFlag::Hangar,
                        quantity: 1,
                        singleton: true,
                        ..Default::default()
                    },
                );
                mod_ids.push((mid, flag));
            }
            snap.next_entity_id += 1;
            let ammo_id = snap.next_entity_id;
            snap.items.insert(
                ammo_id,
                InventoryItemRecord {
                    item_id: ammo_id,
                    type_id: ammo_tid,
                    owner_id: char_id,
                    location_id: station_id,
                    flag: InventoryFlag::Hangar,
                    quantity: 8_000,
                    singleton: false,
                    ..Default::default()
                },
            );
            if fit_entosis {
                snap.next_entity_id += 1;
                let stront_id = snap.next_entity_id;
                snap.items.insert(
                    stront_id,
                    InventoryItemRecord {
                        item_id: stront_id,
                        type_id: STRONTIUM_TYPE,
                        owner_id: char_id,
                        location_id: ship_id,
                        flag: InventoryFlag::Cargo,
                        quantity: 50,
                        singleton: false,
                        ..Default::default()
                    },
                );
            }
            (corp_id, station_id, ship_id, gun_ids, mod_ids, ammo_id)
        });

    call_rpc(
        ctx,
        &mut sess,
        "invbroker",
        "Add",
        &[
            PyRep::Integer(ammo_id),
            PyRep::Integer(ship_id),
            PyRep::Integer(InventoryFlag::Cargo as i64),
        ],
    )
    .await
    .expect("load ammo to cargo");
    for &(mid, flag) in &all_mod_ids {
        call_rpc(
            ctx,
            &mut sess,
            "invbroker",
            "Add",
            &[
                PyRep::Integer(mid),
                PyRep::Integer(ship_id),
                PyRep::Integer(flag as i64),
            ],
        )
        .await
        .expect("fit module");
    }
    ctx.advance_secs(31);
    call_rpc(
        ctx,
        &mut sess,
        "ship",
        "Board",
        &[PyRep::Integer(ship_id), PyRep::Integer(station_id)],
    )
    .await
    .expect("Board fitted hull");
    for &(mid, _) in &all_mod_ids {
        call_rpc(
            ctx,
            &mut sess,
            "dogmaIM",
            "SetModuleOnline",
            &[PyRep::Integer(ship_id), PyRep::Integer(mid)],
        )
        .await
        .expect("SetModuleOnline");
    }
    let gun_reps: Vec<PyRep> = gun_ids.iter().map(|(g, _)| PyRep::Integer(*g)).collect();
    let ship_ammo_id = ctx.db.with_snapshot(|s| {
        s.items
            .values()
            .find(|i| i.location_id == ship_id && i.type_id == ammo_tid)
            .map(|i| i.item_id)
            .expect("ammo in ship cargo")
    });
    call_rpc(
        ctx,
        &mut sess,
        "dogmaIM",
        "LoadAmmo",
        &[
            PyRep::Integer(ship_id),
            PyRep::List(gun_reps),
            PyRep::List(vec![PyRep::Integer(ship_ammo_id)]),
        ],
    )
    .await
    .expect("LoadAmmo");
    call_rpc(ctx, &mut sess, "ship", "Undock", &[])
        .await
        .expect("ship.Undock");
    ctx.advance_secs(31);

    PlayerPilot {
        sess,
        char_id,
        corp_id,
        ship_id,
    }
}

pub(crate) async fn move_player_near_ball(
    ctx: &ServerContext,
    sess: &mut SessionState,
    sys_id: u32,
    ship_id: i64,
    target_id: i64,
    offset_m: f64,
) {
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let target_pos = sys_h
        .get_ball_snapshot(target_id)
        .await
        .unwrap()
        .expect("target ball")
        .1
        .position;
    sys_h
        .set_ball_position(ship_id, target_pos + DVec3::new(offset_m, 0.0, 0.0))
        .await;
    let _ = call_rpc(ctx, sess, "beyonce", "CmdStop", &[]).await;
    let _ = sys_h
        .set_movement_mode(ship_id, eve_destiny::MovementMode::Stop)
        .await;
    if let Ok(Some(mut cs)) = sys_h.get_combat_state(ship_id).await {
        cs.warp_exit_invulnerable_remaining_ticks = 0;
        cs.decloak_targeting_delay_remaining_s = 0.0;
        let _ = sys_h.set_combat_state(ship_id, cs).await;
    }
}

pub(crate) async fn lock_player_target(
    ctx: &ServerContext,
    sess: &mut SessionState,
    sys_id: u32,
    ship_id: i64,
    target_id: i64,
) {
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let already = sys_h
        .get_target_locks(ship_id)
        .await
        .unwrap_or_default()
        .into_iter()
        .find(|l| l.target_id == target_id);
    if already.is_none() {
        call_rpc(
            ctx,
            sess,
            "dogmaIM",
            "AddTarget",
            &[PyRep::Integer(target_id)],
        )
        .await
        .expect("dogmaIM.AddTarget");
    }
    for _ in 0..30 {
        let locks = sys_h.get_target_locks(ship_id).await.unwrap_or_default();
        if locks.iter().any(|l| l.target_id == target_id && l.locked) {
            return;
        }
        let _ = sys_h.step_manual(1.0).await;
        ctx.advance_secs(1);
    }
    panic!("Failed to complete target lock on {target_id}");
}

pub(crate) async fn destroy_ball_with_player_weapons(
    ctx: &ServerContext,
    pilot: &mut PlayerPilot,
    sys_id: u32,
    target_id: i64,
) {
    move_player_near_ball(ctx, &mut pilot.sess, sys_id, pilot.ship_id, target_id, 1_000.0).await;
    lock_player_target(ctx, &mut pilot.sess, sys_id, pilot.ship_id, target_id).await;
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let guns: Vec<i64> = ctx.db.with_snapshot(|s| {
        s.items
            .values()
            .filter(|i| {
                i.location_id == pilot.ship_id
                    && ctx.sde.types.get(&i.type_id).is_some_and(|t| t.group_id == 55)
            })
            .map(|i| i.item_id)
            .collect()
    });
    for _ in 0..80 {
        let destroyed_struct = ctx
            .db
            .with_snapshot(|s| s.structures.get(&target_id).is_some_and(|st| st.state == "Destroyed"));
        let alive_ball = sys_h.get_ball_snapshot(target_id).await.ok().flatten().is_some();
        if destroyed_struct || !alive_ball {
            break;
        }
        let mut reloading = false;
        for &gid in &guns {
            let gone = ctx
                .db
                .with_snapshot(|s| s.structures.get(&target_id).is_some_and(|st| st.state == "Destroyed"))
                || sys_h.get_ball_snapshot(target_id).await.ok().flatten().is_none();
            if gone {
                return;
            }
            if let Err(e) = call_rpc(
                ctx,
                &mut pilot.sess,
                "dogmaIM",
                "Activate",
                &[
                    PyRep::Integer(gid),
                    PyRep::String("targetAttack".into()),
                    PyRep::Integer(target_id),
                ],
            )
            .await
            {
                match e.code.as_str() {
                    "TargetNotFound"
                    | "TargetNotInSpace"
                    | "StructureAlreadyDestroyed"
                    | "StructureDestroyed" => return,
                    "ModuleIsReloading" => reloading = true,
                    "ModuleAlreadyActive" => {}
                    "TargetNotLocked" => {
                        let still_alive = ctx.db.with_snapshot(|s| {
                            !s.structures.get(&target_id).is_some_and(|st| st.state == "Destroyed")
                        }) && sys_h.get_ball_snapshot(target_id).await.ok().flatten().is_some();
                        if !still_alive {
                            return;
                        }
                        lock_player_target(ctx, &mut pilot.sess, sys_id, pilot.ship_id, target_id).await;
                    }
                    _ => panic!("Unexpected weapon Activate error on {target_id}: {e:?}"),
                }
            }
        }
        let step_secs = if reloading { 11 } else { 5 };
        for _ in 0..step_secs {
            let _ = sys_h.step_manual(1.0).await;
            ctx.advance_secs(1);
        }
    }
}

pub(crate) fn new_sqlite_ctx(label: &str) -> (PathBuf, Arc<ServerContext>) {
    let dir = std::env::temp_dir().join(format!(
        "evomu_shard3_{label}_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let db_path = dir.join(format!("{label}.sqlite"));
    let repo = Arc::new(WorldRepository::open_sqlite(&db_path).expect("open sqlite"));
    let ctx = Arc::new(ServerContext::with_repository(repo).with_seeded_rng(eve_server::rng::TEST_MODE_DEFAULT_SEED));
    (db_path, ctx)
}

pub(crate) fn reload_ctx_from_sqlite(ctx: &ServerContext, path: &Path) -> Arc<ServerContext> {
    ctx.db.save_to_sqlite(path).expect("flush sqlite world");
    let repo = Arc::new(WorldRepository::open_sqlite(path).expect("reopen sqlite world"));
    let ctx2 = Arc::new(ServerContext::with_repository(repo).with_seeded_rng(eve_server::rng::TEST_MODE_DEFAULT_SEED));
    crate::common::pin_clock(&ctx2);
    ctx2
}

