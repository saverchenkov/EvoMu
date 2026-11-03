//! End-to-end integration tests for Pochven loot drop, corporation project contribution
//! methods, corporation member role audit, and corporation structure reinforce default.
//!
//! Parity rows:
//! - `feat:combat.pochven-loot-drop`
//! - `feat:corporation.corp-project-contribution-methods`
//! - `feat:corporation.member-audit`
//! - `feat:corporation.structure-reinforce-default`

#![forbid(unsafe_code)]

use std::collections::HashMap;

use crate::common;
use eve_db::{InventoryFlag, InventoryItemRecord, IskCents, SystemDynamicState};
use eve_destiny::{Ball, DamageVector, MovementMode};
use eve_machonet::SessionState;
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, EvePublicRequest,
    EvePublicResponse, GoogleAny, GoogleTimestamp, QuasarMessage, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_proto::PyRep;
use eve_server::quasar_gateway::STATUS_OK;
use eve_server::router::ServerContext;
use eve_server::services::combat_crimewatch::{
    destroy_specialized_npc_and_queue_bounty, spawn_specialized_npc_entity, with_combat_runtime,
};
use glam::DVec3;

pub(super) const PILOT_1: i64 = 90_000_001;
pub(super) const PILOT_2: i64 = 90_000_002;

pub(super) async fn select_pilot(
    ctx: &ServerContext,
    session_id: u64,
    username: &str,
    password: &str,
    char_id: i64,
) -> SessionState {
    let mut sess = SessionState::new(session_id);
    common::login_pilot(ctx, &mut sess, username, password).await;
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &HashMap::new(),
        None,
    )
    .await
    .expect("SelectCharacterID");
    ctx.advance_secs(12);
    sess
}

pub(super) async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, eve_proto::BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None)
        .await
}

async fn quasar_rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    type_url: &str,
    payload_value: Vec<u8>,
    corr: &[u8],
) -> EvePublicResponse {
    let req = EvePublicRequest {
        issued: Some(GoogleTimestamp {
            seconds: ctx.now_unix() as i64,
            nanos: 0,
        }),
        correlation_uuid: corr.to_vec(),
        payload: Some(GoogleAny {
            type_url: if type_url.starts_with(TYPE_GOOGLEAPIS_PREFIX) {
                type_url.to_string()
            } else {
                format!("{TYPE_GOOGLEAPIS_PREFIX}{type_url}")
            },
            value: payload_value,
        }),
        external_origin: "eve-client".into(),
        application_instance_uuid: b"app-corp-combat-01".to_vec(),
        authoritative_context: None,
        trace_context: None,
    };
    let rep = rpc(
        ctx,
        sess,
        "publicGatewaySvc",
        "SendRequest",
        &[PyRep::Bytes(req.encode_proto())],
    )
    .await
    .expect("publicGatewaySvc.SendRequest");
    if let PyRep::Bytes(b) = field(&rep, "response_proto") {
        return EvePublicResponse::decode_proto(b).expect("decode EvePublicResponse");
    }
    panic!("Expected response_proto in SendRequest reply, got {rep:?}");
}

pub(super) fn field<'a>(rep: &'a PyRep, key: &str) -> &'a PyRep {
    let inner = match rep {
        PyRep::Object { args, .. } => args.as_ref(),
        other => other,
    };
    match inner {
        PyRep::KeyVal(map) => {
            if let Some(v) = map.get(key) {
                return v;
            }
        }
        PyRep::Dict(entries) => {
            for (k, v) in entries {
                if k == &PyRep::String(key.into()) {
                    return v;
                }
            }
        }
        _ => {}
    }
    panic!("Missing field '{key}' in {rep:?}");
}

pub(super) fn rowset_rows(rep: &PyRep) -> Vec<Vec<PyRep>> {
    if let PyRep::Rowset { rows, .. } = rep {
        return rows.clone();
    }
    if let PyRep::Object { args, .. } = rep {
        if let PyRep::Dict(entries) = args.as_ref() {
            for (k, v) in entries {
                if k == &PyRep::String("lines".into()) {
                    if let PyRep::List(rows) = v {
                        return rows
                            .iter()
                            .filter_map(|r| match r {
                                PyRep::List(cols) => Some(cols.clone()),
                                _ => None,
                            })
                            .collect();
                    }
                }
            }
        }
    }
    Vec::new()
}

pub(super) async fn spawn_ship_in_space(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    sys_id: u32,
    pos: DVec3,
    base_item_id: Option<i64>,
) {
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.active_ship_id = ship_id;
            c.solar_system_id = sys_id;
            c.station_id = None;
            c.structure_id = None;
        }
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587, // Rifter
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
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
        snap.item_positions.insert(ship_id, pos.to_array());

        if let Some(base_id) = base_item_id {
            for idx in 0..30i64 {
                let flag = match idx {
                    0 => InventoryFlag::HiSlot0,
                    1 => InventoryFlag::HiSlot1,
                    2 => InventoryFlag::MedSlot0,
                    3 => InventoryFlag::LoSlot0,
                    _ => InventoryFlag::Cargo,
                };
                let type_id = if idx < 4 { 2048 } else { 34 };
                let qty = if idx < 4 { 1 } else { 100 };
                snap.items.insert(
                    base_id + idx,
                    InventoryItemRecord {
                        item_id: base_id + idx,
                        type_id,
                        owner_id: char_id,
                        location_id: ship_id,
                        flag,
                        quantity: qty,
                        singleton: idx < 4,
                        version: 1,
                        dynamic_attributes: HashMap::new(),
                        source_mutaplasmid_id: None,
                        insured_payout_isk: 0.0,
                        is_blueprint_copy: false,
                        blueprint_runs_remaining: None,
                    },
                );
            }
        }
    });

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let mut cs = ctx.build_type_combat_state(587);
    if cs.max_structure_hp <= 0.0 {
        cs.max_shield_hp = 500.0;
        cs.shield_hp = 500.0;
        cs.max_armor_hp = 400.0;
        cs.armor_hp = 400.0;
        cs.max_structure_hp = 350.0;
        cs.structure_hp = 350.0;
    }
    let _ = sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: 587,
                position: pos,
                velocity: DVec3::ZERO,
                max_velocity: 350.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 30.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            cs,
        )
        .await;
}

#[tokio::test]
async fn test_feat_combat_pochven_loot_drop_e2e() {
    // parity: feat:combat.pochven-loot-drop
    eve_testkit::attest(&["feat:combat.pochven-loot-drop"]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let pochven_sys = 30_000_157u32; // Otela (region 10_000_070 Pochven)
    let jita_sys = 30_000_142u32; // Jita (region 10_000_002 The Forge)

    let attacker_pochven = PILOT_1;
    let victim_pochven = PILOT_2;
    let victim_ship_pochven = 104_000_102i64;

    let attacker_jita = PILOT_1;
    let victim_jita = PILOT_2;
    let victim_ship_jita = 104_000_202i64;

    // Identical item IDs for both ships isolate the 60% Pochven drop threshold vs 50% Jita threshold.
    spawn_ship_in_space(
        &ctx,
        victim_pochven,
        victim_ship_pochven,
        pochven_sys,
        DVec3::new(2_000.0, 0.0, 0.0),
        Some(210_000_000),
    )
    .await;
    let pochven_handle = ctx.systems.get_or_spawn(pochven_sys);
    let lethal = DamageVector {
        em: 50_000.0,
        thermal: 50_000.0,
        kinetic: 50_000.0,
        explosive: 50_000.0,
    };
    let hit_p = eve_server::services::station_structure::structure_lifecycle_rpc::apply_damage_structure_or_ball(
        &ctx, &pochven_handle, pochven_sys, attacker_pochven, victim_ship_pochven, lethal, 1.0, ctx.now_unix(),
    ).await.expect("damage pochven ship").expect("hit pochven");
    assert!(hit_p.1, "Pochven victim ship must be destroyed by lethal combat damage");
    eve_server::services::combat_crimewatch::combat_missiles_aoe::handle_destroyed_ball_from_missile_or_aoe(
        &ctx, pochven_sys, attacker_pochven, victim_ship_pochven, 587, DVec3::new(2_000.0, 0.0, 0.0), ctx.now_unix(),
    ).await;

    let (pochven_km_id, pochven_wreck_items) = ctx.db.with_snapshot(|s| {
        let km = s
            .killmails
            .values()
            .find(|k| k.solar_system_id == pochven_sys && k.victim_character_id == victim_pochven)
            .cloned()
            .expect("Pochven killmail must be recorded");
        let wreck = s
            .items
            .values()
            .find(|i| i.location_id == pochven_sys as i64 && i.owner_id == victim_pochven && i.item_id != victim_ship_pochven && s.items.values().any(|c| c.location_id == i.item_id))
            .map(|w| w.item_id)
            .expect("Pochven wreck must be spawned");
        let dropped: Vec<i64> = s
            .items
            .values()
            .filter(|i| i.location_id == wreck)
            .map(|i| i.item_id)
            .collect();
        (km.kill_id, dropped)
    });
    let pochven_km_det = with_combat_runtime(&ctx, |rt| rt.detailed_killmails.get(&pochven_km_id).cloned())
        .expect("Pochven detailed killmail");

    spawn_ship_in_space(&ctx, victim_jita, victim_ship_jita, jita_sys, DVec3::new(2_000.0, 0.0, 0.0), Some(210_000_000)).await;
    let jita_handle = ctx.systems.get_or_spawn(jita_sys);
    let hit_j = eve_server::services::station_structure::structure_lifecycle_rpc::apply_damage_structure_or_ball(
        &ctx, &jita_handle, jita_sys, attacker_jita, victim_ship_jita, lethal, 1.0, ctx.now_unix(),
    ).await.expect("damage jita ship").expect("hit jita");
    assert!(hit_j.1, "Jita victim ship must be destroyed by lethal combat damage");
    eve_server::services::combat_crimewatch::combat_missiles_aoe::handle_destroyed_ball_from_missile_or_aoe(
        &ctx, jita_sys, attacker_jita, victim_ship_jita, 587, DVec3::new(2_000.0, 0.0, 0.0), ctx.now_unix(),
    ).await;

    let (jita_km_id, jita_wreck_items) = ctx.db.with_snapshot(|s| {
        let km = s
            .killmails
            .values()
            .find(|k| k.solar_system_id == jita_sys && k.victim_character_id == victim_jita)
            .cloned()
            .expect("Jita killmail must be recorded");
        let wreck = s
            .items
            .values()
            .find(|i| i.location_id == jita_sys as i64 && i.owner_id == victim_jita && i.item_id != victim_ship_jita && i.item_id != pochven_km_id && s.items.values().any(|c| c.location_id == i.item_id))
            .map(|w| w.item_id)
            .expect("Jita wreck must be spawned");
        let dropped: Vec<i64> = s
            .items
            .values()
            .filter(|i| i.location_id == wreck)
            .map(|i| i.item_id)
            .collect();
        (km.kill_id, dropped)
    });
    let jita_km_det = with_combat_runtime(&ctx, |rt| rt.detailed_killmails.get(&jita_km_id).cloned())
        .expect("Jita detailed killmail");

    assert!(
        pochven_wreck_items.len() > jita_wreck_items.len(),
        "Pochven 60% drop ({}) must drop strictly more items than Jita 50% drop ({})",
        pochven_wreck_items.len(),
        jita_wreck_items.len()
    );
    assert_eq!(
        pochven_km_det.dropped_items.len(),
        pochven_wreck_items.len(),
        "Pochven killmail dropped_items must match wreck contents"
    );
    assert_eq!(
        pochven_km_det.dropped_items.len() + pochven_km_det.destroyed_items.len(),
        30,
        "Pochven killmail must account for all 30 fitted + cargo items"
    );
    assert_eq!(
        jita_km_det.dropped_items.len(),
        jita_wreck_items.len(),
        "Jita killmail dropped_items must match wreck contents"
    );
}

#[tokio::test]
async fn test_feat_corporation_corp_project_contribution_methods_e2e() {
    // parity: feat:corporation.corp-project-contribution-methods
    eve_testkit::attest(&["feat:corporation.corp-project-contribution-methods"]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let mut sess1 = select_pilot(&ctx, 64_101, "demo", "demo-secret", PILOT_1).await;
    let mut sess2 = select_pilot(&ctx, 64_102, "other", "other-secret", PILOT_2).await;

    let created = rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "AddCorporation",
        &[
            PyRep::String("Pochven Logistics Co".into()),
            PyRep::String("PLC".into()),
            PyRep::Real(0.05),
        ],
    )
    .await
    .expect("AddCorporation");
    let corp_id = field(&created, "corporationID").as_i64().expect("corp_id");

    let jita_sys = 30_000_142u32;
    let jita_station = 60_003_760i64;
    ctx.db.with_mut_snapshot(|s| {
        let corp = s.corporations.get_mut(&corp_id).unwrap();
        corp.ensure_wallet_divisions();
        corp.master_wallet = IskCents::from_f64_isk(500_000_000.0);
        corp.wallet_divisions[0].balance = IskCents::from_f64_isk(500_000_000.0);
        corp.members.insert(PILOT_2);
        let p2 = s.characters.get_mut(&PILOT_2).unwrap();
        p2.corporation_id = corp_id;
        p2.balance = IskCents::from_f64_isk(500_000_000.0);
    });
    sess2.corporation_id = Some(corp_id);

    let (&bp_type_id, bp_def) = ctx
        .sde
        .blueprints
        .iter()
        .find(|(_, b)| {
            b.activities
                .get("manufacturing")
                .is_some_and(|a| !a.products.is_empty() && a.time <= 86_400)
        })
        .expect("SDE manufacturing blueprint");
    let mfg_act = &bp_def.activities["manufacturing"];
    let mfg_prod_type_id = mfg_act.products[0].type_id;

    let pid_npc = create_proj(&ctx, &mut sess1, "Kill Jita Rats", 10, "DestroyNPCShips", jita_sys as i64).await;
    let pid_mine = create_proj(&ctx, &mut sess1, "Mine Veldspar", 100, "MineMaterials", 1230).await;
    let pid_mfg = create_proj(&ctx, &mut sess1, "Build Item", 5, "ManufactureItems", mfg_prod_type_id as i64).await;
    let pid_deliv = create_proj(&ctx, &mut sess1, "Deliver Tritanium", 100, "DeliverItem", 34).await;
    let fw_sys = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| s.security_status > 0.0 && s.security_status < 0.45 && s.region_id != 10_000_070)
        .expect("lowsec system")
        .solar_system_id;
    let pid_fw = create_proj(&ctx, &mut sess1, "Capture FW", 5, "CaptureFWComplexes", fw_sys as i64).await;
    let pid_dmg = create_proj(&ctx, &mut sess1, "Damage Ships", 10_000, "DamageShips", jita_sys as i64).await;
    let pid_lp = create_proj(&ctx, &mut sess1, "Earn Caldari LP", 5_000, "EarnLoyaltyPoints", 1_000_180).await;

    // Client-reported GoalProgress must be rejected for server-tracked matchers
    let spoof_err = rpc(
        &ctx,
        &mut sess2,
        "corporationSvc",
        "GoalProgress",
        &[PyRep::Integer(pid_npc), PyRep::Integer(5)],
    )
    .await
    .expect_err("Client-reported count on server-tracked matcher must fail");
    assert_eq!(spoof_err.code, "ServerTrackedMatcher");

    // 1. Kill NPCs: matching system advances, non-matching system does not
    let npc_type_id = ctx.sde.types.iter().find(|(_, t)| t.category_id == 11 && t.published).map(|(&id, _)| id).unwrap_or(10_000_001);
    let npc_jita = spawn_specialized_npc_entity(&ctx, jita_sys, npc_type_id, "Pirate", 4001, DVec3::new(1_000.0, 0.0, 0.0)).await;
    let _ = destroy_specialized_npc_and_queue_bounty(&ctx, jita_sys, npc_jita, PILOT_2).await;
    let npc_other = spawn_specialized_npc_entity(&ctx, 30_000_144, npc_type_id, "Pirate", 4001, DVec3::new(1_000.0, 0.0, 0.0)).await;
    let _ = destroy_specialized_npc_and_queue_bounty(&ctx, 30_000_144, npc_other, PILOT_2).await;

    // 2. Mine ore: matching ore (1230 Veldspar) advances, Scordite (1228) does not
    eve_server::services::space_mining_harvest::dispatch_mining_gameplay_events(&ctx, PILOT_2, 35, jita_sys, 1230);
    eve_server::services::space_mining_harvest::dispatch_mining_gameplay_events(&ctx, PILOT_2, 35, jita_sys, 1228);

    // 3. Manufacture items: install & complete manufacturing job from SDE blueprint
    let (bpo_id, ship_p2) = ctx.db.with_mut_snapshot(|s| {
        let ship_id = s.characters.get(&PILOT_2).unwrap().active_ship_id;
        let ch = s.characters.get_mut(&PILOT_2).unwrap();
        ch.station_id = Some(jita_station);
        ch.solar_system_id = jita_sys;
        s.next_entity_id += 1;
        let bpo = s.next_entity_id;
        s.items.insert(bpo, InventoryItemRecord {
            item_id: bpo, type_id: bp_type_id, owner_id: PILOT_2, location_id: jita_station,
            flag: InventoryFlag::Hangar, quantity: 1, singleton: true, version: 1,
            dynamic_attributes: HashMap::new(), source_mutaplasmid_id: None,
            insured_payout_isk: 0.0, is_blueprint_copy: false, blueprint_runs_remaining: None,
        });
        for mat in &mfg_act.materials {
            s.next_entity_id += 1;
            let mid = s.next_entity_id;
            s.items.insert(mid, InventoryItemRecord {
                item_id: mid, type_id: mat.type_id, owner_id: PILOT_2, location_id: jita_station,
                flag: InventoryFlag::Hangar, quantity: (mat.quantity as i32) * 10, singleton: false, version: 1,
                dynamic_attributes: HashMap::new(), source_mutaplasmid_id: None,
                insured_payout_isk: 0.0, is_blueprint_copy: false, blueprint_runs_remaining: None,
            });
        }
        s.next_entity_id += 1;
        let trit_id = s.next_entity_id;
        s.items.insert(trit_id, InventoryItemRecord {
            item_id: trit_id, type_id: 34, owner_id: PILOT_2, location_id: jita_station,
            flag: InventoryFlag::Hangar, quantity: 10_000, singleton: false, version: 1,
            dynamic_attributes: HashMap::new(), source_mutaplasmid_id: None,
            insured_payout_isk: 0.0, is_blueprint_copy: false, blueprint_runs_remaining: None,
        });
        (bpo, ship_id)
    });
    sess2.station_id = Some(jita_station);
    sess2.solar_system_id = Some(jita_sys);
    let mut job_kv = HashMap::new();
    job_kv.insert("facilityID".to_string(), PyRep::Integer(jita_station));
    job_kv.insert("blueprintItemID".to_string(), PyRep::Integer(bpo_id));
    job_kv.insert("blueprintTypeID".to_string(), PyRep::Integer(bp_type_id as i64));
    job_kv.insert("activityID".to_string(), PyRep::Integer(1));
    job_kv.insert("runs".to_string(), PyRep::Integer(2));
    let job_rep = rpc(
        &ctx,
        &mut sess2,
        "industryManager",
        "InstallJob",
        &[PyRep::KeyVal(job_kv)],
    )
    .await
    .expect("InstallJob");
    let job_id = job_rep.as_i64().unwrap_or_else(|| field(&job_rep, "jobID").as_i64().expect("jobID"));
    ctx.advance_secs(2 * 86_400);
    rpc(&ctx, &mut sess2, "industryManager", "CompleteJob", &[PyRep::Integer(job_id)])
        .await
        .expect("CompleteJob");

    // 4. Deliver items: MachoNet GoalProgress + Quasar freelance DeliverRequest
    rpc(&ctx, &mut sess2, "corporationSvc", "GoalProgress", &[PyRep::Integer(pid_deliv), PyRep::Integer(40)])
        .await
        .expect("GoalProgress DeliverItem");

    let mut fl_create = Vec::new();
    encode_string_field(1, "Freelance Tritanium", &mut fl_create);
    encode_string_field(2, "Deliver Tritanium", &mut fl_create);
    encode_varint_field(3, 10, &mut fl_create);
    encode_varint_field(4, 100, &mut fl_create);
    let mut cfg = Vec::new();
    for val_str in ["34", "5"] {
        let mut om = Vec::new();
        encode_string_field(1, val_str, &mut om);
        let mut pm = Vec::new();
        encode_bytes_field(3, &om, &mut pm);
        encode_bytes_field(3, &pm, &mut cfg);
    }
    encode_bytes_field(6, &cfg, &mut fl_create);
    let fl_rsp = quasar_rpc(&ctx, &mut sess1, "eve_public.freelance.project.api.CreateRequest", fl_create, b"fl-create-1").await;
    assert_eq!(fl_rsp.status_code, STATUS_OK);
    let fl_uuid = fl_rsp.payload.unwrap().value[6..22].to_vec();
    ctx.db.with_mut_snapshot(|s| {
        if let Some(c2) = s.characters.get_mut(&PILOT_2) {
            c2.station_id = Some(jita_station);
            c2.solar_system_id = jita_sys;
        }
        s.items.insert(106_500_001, InventoryItemRecord {
            item_id: 106_500_001, type_id: 34, owner_id: PILOT_2, location_id: jita_station,
            flag: InventoryFlag::Hangar, quantity: 20, singleton: false, version: 1,
            dynamic_attributes: HashMap::new(), source_mutaplasmid_id: None,
            insured_payout_isk: 0.0, is_blueprint_copy: false, blueprint_runs_remaining: None,
        });
    });
    let mut id_msg = Vec::new();
    encode_bytes_field(1, &fl_uuid, &mut id_msg);
    let mut fl_commit = Vec::new();
    encode_bytes_field(1, &id_msg, &mut fl_commit);
    let commit_rsp = quasar_rpc(&ctx, &mut sess2, "eve_public.freelance.project.api.CommitRequest", fl_commit, b"fl-commit-1").await;
    assert_eq!(commit_rsp.status_code, STATUS_OK);
    let mut fl_deliv = Vec::new();
    encode_bytes_field(1, &id_msg, &mut fl_deliv);
    encode_varint_field(4, 10, &mut fl_deliv);
    let deliv_rsp = quasar_rpc(&ctx, &mut sess2, "eve_public.freelance.contributionmethod.itemdelivery.api.DeliverRequest", fl_deliv, b"fl-deliv-1").await;
    assert_eq!(deliv_rsp.status_code, STATUS_OK);

    // 5. Capture FW complex + 7. Earn LP
    ctx.db.with_mut_snapshot(|s| {
        if let Some(c2) = s.characters.get_mut(&PILOT_2) {
            c2.war_faction_id = Some(500_001);
        }
        s.system_states.insert(fw_sys, SystemDynamicState {
            solar_system_id: fw_sys, dbs_multiplier: 1.0, ess_main_bank: IskCents(0), ess_reserve_bank: IskCents(0),
            ess_linked_character_id: None, ess_link_started_tick: None, sov_alliance_id: None,
            military_index: 1, industrial_index: 1, strategic_index: 1, fw_contested_pct: 50.0,
            insurgency_corruption_stage: 1, insurgency_suppression_stage: 0, wormhole_remaining_mass_kg: 0.0,
            wormhole_destination_system_id: None, signature_position: None,
            fw_complex_position: Some([1_000.0, 0.0, 0.0]), fw_capture_seconds_elapsed: 595,
            fw_capture_duration_seconds: 600, last_fw_capture_tick: 0, last_sov_claim_tick: 0,
            ess_cycle_started_unix: 0, ess_main_bank_contributors: Default::default(),
        });
    });
    spawn_ship_in_space(&ctx, PILOT_2, ship_p2, fw_sys, DVec3::new(1_000.0, 0.0, 0.0), None).await;
    eve_server::services::insurgency_services::step_system_insurgency_tick_delta(&ctx, fw_sys, Some(10)).await;
    ctx.db.with_mut_snapshot(|s| {
        eve_server::services::lp_store::record_lp_reward_log(s, PILOT_2, 1_000_180, 250, "FW Complex".into(), ctx.now_unix());
    });

    // 6. Damage ships
    let target_ship = 104_000_999i64;
    spawn_ship_in_space(&ctx, PILOT_1, target_ship, jita_sys, DVec3::new(3_000.0, 0.0, 0.0), None).await;
    let jita_handle = ctx.systems.get_or_spawn(jita_sys);
    eve_server::services::station_structure::structure_lifecycle_rpc::apply_damage_structure_or_ball(
        &ctx, &jita_handle, jita_sys, PILOT_2, target_ship,
        DamageVector { em: 200.0, thermal: 0.0, kinetic: 0.0, explosive: 0.0 },
        1.0, ctx.now_unix(),
    ).await.expect("damage ship");

    ctx.db.with_snapshot(|s| {
        assert_eq!(s.corp_projects.get(&pid_npc).unwrap().current_quantity, 1, "DestroyNPCShips must advance only for matching system");
        assert_eq!(s.corp_projects.get(&pid_mine).unwrap().current_quantity, 35, "MineMaterials must advance only for matching ore type");
        assert_eq!(s.corp_projects.get(&pid_mfg).unwrap().current_quantity, 2, "ManufactureItems must advance by completed job runs");
        assert_eq!(s.corp_projects.get(&pid_deliv).unwrap().current_quantity, 40, "DeliverItem must advance by delivered inventory");
        assert_eq!(s.corp_projects.get(&pid_fw).unwrap().current_quantity, 1, "CaptureFWComplexes must advance on complex capture");
        assert!(s.corp_projects.get(&pid_dmg).unwrap().current_quantity >= 100, "DamageShips must advance from server-observed ship damage");
        assert_eq!(s.corp_projects.get(&pid_lp).unwrap().current_quantity, 250, "EarnLoyaltyPoints must advance from server-awarded LP");
    });
}

async fn create_proj(
    ctx: &ServerContext,
    sess: &mut SessionState,
    name: &str,
    target: i64,
    matcher: &str,
    filter: i64,
) -> i64 {
    let rep = rpc(
        ctx,
        sess,
        "corporationSvc",
        "CreateGoalSubmit",
        &[
            PyRep::String(name.into()),
            PyRep::Integer(target),
            PyRep::Real(1_000.0),
            PyRep::String(matcher.into()),
            PyRep::Bool(false),
            PyRep::Integer(1),
            PyRep::Integer(target),
            PyRep::Integer(7 * 86_400),
            PyRep::Integer(filter),
        ],
    )
    .await
    .expect("CreateGoalSubmit");
    field(&rep, "projectID").as_i64().expect("projectID")
}
