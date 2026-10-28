//! Authentic state-backed integration tests for Upwell Structure Operations:
//! - `structureControl`: `BoardStructure`, `EjectFromStructure`, `GetStructurePilot`, `ReleaseControl`, `TakeControl`
//! - `structureDeployment`: `CancelUnanchor`, `Unanchor`
//! - `structureJumpBridgeMgr`: `CmdJumpThroughStructureStargate`, `GetJbStructureDestination`, `GetLinkedStructure`
//! - `structureAssetSafety`: `MoveCorpAssetsToSafety`, `MoveEjectTimeGM`
//! - `structureCargoDelivery`: `DropOffItems`
//! - `structureDeliveries`: `Deliver`

#![forbid(unsafe_code)]

use std::collections::HashMap;

use glam::DVec3;
use eve_db::{InventoryFlag, InventoryItemRecord, UpwellStructureRecord};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;

use crate::common;

const PILOT_1: i64 = 90_000_001;
const PILOT_2: i64 = 90_000_002;
const CORP_1: i64 = 98_000_001;

fn field<'a>(rep: &'a PyRep, key: &str) -> &'a PyRep {
    match rep {
        PyRep::KeyVal(entries) => entries
            .get(key)
            .unwrap_or_else(|| panic!("missing keyval field: {key}")),
        PyRep::Dict(entries) => entries
            .iter()
            .find(|(k, _)| k.as_str() == Some(key))
            .map(|(_, v)| v)
            .unwrap_or_else(|| panic!("missing dict field: {key}")),
        other => panic!("expected KeyVal or Dict, got {other:?}"),
    }
}

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let kw = HashMap::new();
    ctx.dispatch_rpc(sess, service, None, method, args, &kw, None).await
}

async fn login_pilot_char(
    ctx: &ServerContext,
    sess_id: u64,
    username: &str,
    password: &str,
    char_id: i64,
) -> SessionState {
    let mut sess = SessionState::new(sess_id);
    common::login_pilot(ctx, &mut sess, username, password).await;
    rpc(ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)])
        .await
        .expect("SelectCharacterID");
    sess
}

fn grant_item(
    ctx: &ServerContext,
    owner_id: i64,
    location_id: i64,
    type_id: u32,
    flag: InventoryFlag,
    quantity: i32,
) -> i64 {
    ctx.db.with_mut_snapshot(|snap| {
        snap.next_entity_id += 1;
        let id = snap.next_entity_id;
        snap.items.insert(
            id,
            InventoryItemRecord {
                item_id: id,
                type_id,
                owner_id,
                location_id,
                flag,
                quantity,
                singleton: quantity == 1,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        id
    })
}

#[tokio::test]
async fn test_structure_control_and_unanchoring() {
    // parity: rpc:structureControl.BoardStructure
    // parity: rpc:structureControl.EjectFromStructure
    // parity: rpc:structureControl.GetStructurePilot
    // parity: rpc:structureControl.ReleaseControl
    // parity: rpc:structureControl.TakeControl
    // parity: rpc:structureDeployment.CancelUnanchor
    // parity: rpc:structureDeployment.Unanchor

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let struct_id = 991_000_101;
    let sys_id = 30_000_142; // Jita

    let mut sess1 = login_pilot_char(&ctx, 30_001, "demo", "demo-secret", PILOT_1).await;
    sess1.corporation_id = Some(CORP_1);
    sess1.solar_system_id = Some(sys_id);

    // Insert active Fortizar belonging to CORP_1 with PILOT_1 as CEO
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_1) {
            ch.corporation_id = CORP_1;
        }
        snap.corporations.entry(CORP_1).or_default().ceo_id = PILOT_1;
        snap.structures.insert(
            struct_id,
            UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 35833, // Fortizar
                owner_corp_id: CORP_1,
                solar_system_id: sys_id,
                name: "Citadel Prime".into(),
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 1000,
                shield_hp: 50000.0,
                armor_hp: 50000.0,
                hull_hp: 50000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0, self_destruct_at_unix: None,
            },
        );
    });

    // 1. structureControl: initially unpiloted
    let pilot_none = rpc(&ctx, &mut sess1, "structureControl", "GetStructurePilot", &[PyRep::Integer(struct_id)])
        .await
        .expect("GetStructurePilot initially None");
    assert_eq!(pilot_none, PyRep::None);

    // 2. structureControl: TakeControl
    sess1.structure_id = Some(struct_id);
    sess1.station_id = None;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_1) {
            ch.structure_id = Some(struct_id);
            ch.station_id = None;
        }
    });
    let control_rep = rpc(&ctx, &mut sess1, "structureControl", "TakeControl", &[PyRep::Integer(struct_id)])
        .await
        .expect("TakeControl");
    assert_eq!(field(&control_rep, "pilotID"), &PyRep::Integer(PILOT_1));
    assert_eq!(field(&control_rep, "structureID"), &PyRep::Integer(struct_id));

    // Verify GetStructurePilot reflects PILOT_1
    let pilot_rep = rpc(&ctx, &mut sess1, "structureControl", "GetStructurePilot", &[PyRep::Integer(struct_id)])
        .await
        .expect("GetStructurePilot after TakeControl");
    assert_eq!(pilot_rep, PyRep::Integer(PILOT_1));

    // 3. structureControl: ReleaseControl
    let rel_rep = rpc(&ctx, &mut sess1, "structureControl", "ReleaseControl", &[PyRep::Integer(struct_id)])
        .await
        .expect("ReleaseControl");
    assert_eq!(rel_rep, PyRep::None);

    // 4. structureControl: BoardStructure
    let board_rep = rpc(&ctx, &mut sess1, "structureControl", "BoardStructure", &[PyRep::Integer(struct_id)])
        .await
        .expect("BoardStructure");
    assert_eq!(field(&board_rep, "pilotID"), &PyRep::Integer(PILOT_1));
    assert_eq!(field(&board_rep, "boarded"), &PyRep::Bool(true));

    // 5. structureControl: EjectFromStructure
    let eject_rep = rpc(&ctx, &mut sess1, "structureControl", "EjectFromStructure", &[PyRep::Integer(struct_id)])
        .await
        .expect("EjectFromStructure");
    assert_eq!(field(&eject_rep, "boarded"), &PyRep::Bool(false));

    // 6. structureDeployment: Unanchor (requires CEO / Director / Station Manager)
    let mut sess_member = login_pilot_char(&ctx, 30_099, "other", "other-secret", PILOT_2).await;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_2) {
            ch.corporation_id = CORP_1;
        }
    });
    sess_member.corporation_id = Some(CORP_1);
    sess_member.corp_role = 0;
    assert_eq!(
        rpc(&ctx, &mut sess_member, "structureDeployment", "Unanchor", &[PyRep::Integer(struct_id)]).await.unwrap_err().code,
        "InsufficientCorpRoles"
    );

    let unanchor_rep = rpc(&ctx, &mut sess1, "structureDeployment", "Unanchor", &[PyRep::Integer(struct_id)])
        .await
        .expect("Unanchor");
    let complete_time = field(&unanchor_rep, "unanchorCompleteUnix").as_i64().expect("complete timestamp");
    assert_eq!(complete_time, ctx.now_unix() as i64 + 7 * 86_400);

    // Verify unanchoring twice returns error
    let unanchor_err = rpc(&ctx, &mut sess1, "structureDeployment", "Unanchor", &[PyRep::Integer(struct_id)])
        .await
        .expect_err("Unanchor already active");
    assert_eq!(unanchor_err.code, "StructureAlreadyUnanchoring");

    // 7. structureDeployment: CancelUnanchor (zero-role corp member rejected, CEO allowed)
    assert_eq!(
        rpc(&ctx, &mut sess_member, "structureDeployment", "CancelUnanchor", &[PyRep::Integer(struct_id)]).await.unwrap_err().code,
        "InsufficientCorpRoles"
    );
    let cancel_rep = rpc(&ctx, &mut sess1, "structureDeployment", "CancelUnanchor", &[PyRep::Integer(struct_id)])
        .await
        .expect("CancelUnanchor");
    assert_eq!(field(&cancel_rep, "unanchoringCancelled"), &PyRep::Bool(true));
}

#[tokio::test]
async fn test_structure_jumpbridge_safety_and_deliveries() {
    // parity: rpc:structureJumpBridgeMgr.CmdJumpThroughStructureStargate
    // parity: rpc:structureJumpBridgeMgr.GetJbStructureDestination
    // parity: rpc:structureJumpBridgeMgr.GetLinkedStructure
    // parity: rpc:structureAssetSafety.MoveCorpAssetsToSafety
    // parity: rpc:structureAssetSafety.MoveEjectTimeGM
    // parity: rpc:structureCargoDelivery.DropOffItems
    // parity: rpc:structureDeliveries.Deliver

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let sys_a = 30_004_712u32; // NOL-M9 (Delve sov null-sec)
    let sys_b = 30_004_713u32; // O-IOAI (Delve sov null-sec, ~1.10 LY)
    let gate_a_id = 992_000_201;
    let gate_b_id = 992_000_202;
    let ship_id = 100_000_001;

    let mut sess1 = login_pilot_char(&ctx, 30_002, "demo", "demo-secret", PILOT_1).await;
    sess1.corporation_id = Some(CORP_1);
    sess1.solar_system_id = Some(sys_a);
    sess1.ship_id = Some(ship_id);
    sess1.station_id = None;
    sess1.structure_id = None;

    // Set up Ansiblex jump bridge pair
    let gate_a_pos = [10_500.0, 0.0, 0.0];
    let gate_b_pos = [20_000.0, 0.0, 0.0];

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_1) {
            ch.solar_system_id = sys_a;
            ch.station_id = None;
            ch.structure_id = None;
            ch.corporation_id = CORP_1;
            ch.active_ship_id = ship_id;
        }
        snap.corporations.entry(CORP_1).or_default().ceo_id = PILOT_1;

        snap.structures.insert(
            gate_a_id,
            UpwellStructureRecord {
                structure_id: gate_a_id,
                type_id: 35841, // Ansiblex Jump Gate
                owner_corp_id: CORP_1,
                solar_system_id: sys_a,
                name: "Ansiblex NOL-M9 Alpha".into(),
                position: gate_a_pos,
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                shield_hp: 10000.0,
                armor_hp: 10000.0,
                hull_hp: 10000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0, self_destruct_at_unix: None,
            },
        );
        snap.structures.insert(
            gate_b_id,
            UpwellStructureRecord {
                structure_id: gate_b_id,
                type_id: 35841, // Ansiblex Jump Gate
                owner_corp_id: CORP_1,
                solar_system_id: sys_b,
                name: "Ansiblex O-IOAI Beta".into(),
                position: gate_b_pos,
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                shield_hp: 10000.0,
                armor_hp: 10000.0,
                hull_hp: 10000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0, self_destruct_at_unix: None,
            },
        );
        let mut ext = eve_server::services::station_structure::structure_upwell_ops::load_ext_state(snap);
        ext.jump_bridge_links.insert(gate_a_id, gate_b_id);
        ext.jump_bridge_links.insert(gate_b_id, gate_a_id);
        eve_server::services::station_structure::structure_upwell_ops::save_ext_state(snap, &ext);

        // Add 1,000 units of Liquid Ozone fuel to Gate A
        snap.items.insert(
            1_200_000_101,
            InventoryItemRecord {
                item_id: 1_200_000_101,
                type_id: 16273, // Liquid Ozone
                owner_id: CORP_1,
                location_id: gate_a_id,
                flag: InventoryFlag::StructureFuel,
                quantity: 1_000,
                singleton: false,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });

    sess1.solar_system_id = Some(sys_a);
    sess1.station_id = None;
    sess1.structure_id = None;
    sess1.ship_id = Some(ship_id);

    let handle_a = ctx.systems.get_or_spawn(sys_a);
    let _ = handle_a
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: 601, // Ibis
                position: DVec3::new(10_000.0, 0.0, 0.0), // 0m from gate A
                velocity: DVec3::ZERO,
                max_velocity: 1000.0,
                inertia: 1.0,
                mass: 100_000.0,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_ship_combat_state(PILOT_1, ship_id, 601),
        )
        .await;

    // 1. structureJumpBridgeMgr: GetLinkedStructure
    let linked = rpc(&ctx, &mut sess1, "structureJumpBridgeMgr", "GetLinkedStructure", &[PyRep::Integer(gate_a_id)])
        .await
        .expect("GetLinkedStructure");
    assert_eq!(linked, PyRep::Tuple(vec![PyRep::Integer(gate_b_id), PyRep::Integer(sys_b as i64)]));

    // 2. structureJumpBridgeMgr: GetJbStructureDestination
    let dest = rpc(&ctx, &mut sess1, "structureJumpBridgeMgr", "GetJbStructureDestination", &[PyRep::Integer(gate_a_id)])
        .await
        .expect("GetJbStructureDestination");
    assert_eq!(dest, PyRep::Integer(sys_b as i64));

    // 3. structureJumpBridgeMgr: CmdJumpThroughStructureStargate
    ctx.advance_secs(12);
    let jump_res = rpc(&ctx, &mut sess1, "structureJumpBridgeMgr", "CmdJumpThroughStructureStargate", &[PyRep::Integer(gate_a_id)])
        .await
        .expect("CmdJumpThroughStructureStargate");
    let jump_sys = field(&jump_res, "solarSystemID").as_i64().expect("solarSystemID");
    assert_eq!(jump_sys, sys_b as i64);

    // 4. structureAssetSafety: MoveCorpAssetsToSafety
    let _corp_item = grant_item(&ctx, CORP_1, gate_b_id, 34, InventoryFlag::Hangar, 500); // 500 Tritanium
    let safety_rep = rpc(
        &ctx,
        &mut sess1,
        "structureAssetSafety",
        "MoveCorpAssetsToSafety",
        &[PyRep::Integer(sys_b as i64), PyRep::Integer(gate_b_id)],
    )
    .await
    .expect("MoveCorpAssetsToSafety");
    assert_eq!(field(&safety_rep, "ownerID"), &PyRep::Integer(CORP_1));
    let wrap_id = field(&safety_rep, "wrapID").as_i64().expect("wrapID");
    assert!(wrap_id > 0);

    // 5. structureAssetSafety: MoveEjectTimeGM (requires GM role)
    sess1.role = eve_machonet::service_roles::ROLE_GMS;
    let gm_rep = rpc(&ctx, &mut sess1, "structureAssetSafety", "MoveEjectTimeGM", &[PyRep::Integer(wrap_id), PyRep::Integer(3600)])
        .await
        .expect("MoveEjectTimeGM");
    assert_eq!(gm_rep, PyRep::Bool(true));
    let deliverable_after = ctx.db.with_snapshot(|snap| {
        eve_server::services::station_structure::structure_upwell_ops::load_ext_state(snap)
            .asset_safety_wraps
            .get(&wrap_id)
            .map(|w| w.deliverable_after_unix)
    });
    assert_eq!(deliverable_after, Some(3600));
    sess1.role = eve_machonet::service_roles::ROLE_PLAYER;

    // 6. structureDeliveries: Deliver
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_1) {
            ch.structure_id = Some(gate_b_id);
        }
    });
    let hangar_item = grant_item(&ctx, PILOT_1, gate_b_id, 35, InventoryFlag::Hangar, 20); // 20 Pyerite
    let deliver_rep = rpc(
        &ctx,
        &mut sess1,
        "structureDeliveries",
        "Deliver",
        &[PyRep::Integer(gate_b_id), PyRep::Integer(hangar_item), PyRep::Integer(PILOT_2)],
    )
    .await
    .expect("Deliver item");
    assert_eq!(deliver_rep, PyRep::None);

    ctx.db.with_snapshot(|snap| {
        let it = snap.items.get(&hangar_item).expect("delivered item");
        assert_eq!(it.owner_id, PILOT_2);
        assert_eq!(it.flag, InventoryFlag::Deliveries);
    });

    // 7. structureCargoDelivery: DropOffItems
    // Pilot undocked near gate_b in sys_b
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_1) {
            ch.solar_system_id = sys_b;
            ch.station_id = None;
            ch.structure_id = None;
        }
    });
    sess1.solar_system_id = Some(sys_b);
    sess1.station_id = None;
    sess1.structure_id = None;
    let drop_item = grant_item(&ctx, PILOT_1, ship_id, 36, InventoryFlag::Cargo, 100);

    let handle_b = ctx.systems.get_or_spawn(sys_b);
    let _ = handle_b
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: 601,
                position: DVec3::new(20_000.0, 0.0, 0.0), // 0m from gate B
                velocity: DVec3::ZERO,
                max_velocity: 1000.0,
                inertia: 1.0,
                mass: 100_000.0,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_ship_combat_state(PILOT_1, ship_id, 601),
        )
        .await;

    let drop_rep = rpc(
        &ctx,
        &mut sess1,
        "structureCargoDelivery",
        "DropOffItems",
        &[PyRep::Integer(gate_b_id), PyRep::List(vec![PyRep::Integer(drop_item)])],
    )
    .await
    .expect("DropOffItems");
    assert_eq!(drop_rep, PyRep::None);

    ctx.db.with_snapshot(|snap| {
        let it = snap.items.get(&drop_item).expect("dropped off item");
        assert_eq!(it.location_id, gate_b_id);
        assert_eq!(it.flag, InventoryFlag::Deliveries);
    });
}
