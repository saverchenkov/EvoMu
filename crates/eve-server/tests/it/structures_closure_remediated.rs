//! End-to-end remediation tests for Upwell anchoring restrictions (`feat:structures.anchor-restrictions`)
//! and Pochven anchoring / Customs Office standing rules (`feat:structures.pochven-anchoring-restrictions`).

use eve_db::{InventoryFlag, InventoryItemRecord, UpwellStructureRecord};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use eve_server::services::station_structure::structure_lifecycle as life;
use glam::DVec3;
use std::collections::HashMap;
use std::sync::Arc;

const OWNER: i64 = 90_000_001;
const PERIMETER: u32 = 30_000_144;
const RANCER: u32 = 30_002_718;
const HED_GP: u32 = 30_001_161;
const KINO_POCHVEN: u32 = 30_000_157;
const WH_C2: u32 = 31_000_100;

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None).await
}

fn kv_i64(rep: &PyRep, key: &str) -> i64 {
    rep.dict_get(key).and_then(|v| v.as_i64()).unwrap_or(0)
}

fn kv_str(rep: &PyRep, key: &str) -> String {
    rep.dict_get(key).and_then(|v| v.as_str()).unwrap_or("").to_string()
}

fn make_structure(
    structure_id: i64,
    type_id: u32,
    owner_corp_id: i64,
    solar_system_id: u32,
    name: &str,
    pos: DVec3,
    quantum_core_installed: bool,
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
        fuel_blocks: 1_000,
        shield_hp: 1_000_000.0,
        armor_hp: 1_000_000.0,
        hull_hp: 1_000_000.0,
        reinforced_until_unix: 0,
        last_damage_tick: 0,
        last_damage_unix: 0,
        self_destruct_at_unix: None,
    }
}

fn grant(
    ctx: &ServerContext,
    owner: i64,
    location: i64,
    type_id: u32,
    flag: InventoryFlag,
    qty: i32,
) -> i64 {
    ctx.db.with_mut_snapshot(|snap| {
        snap.next_entity_id += 1;
        let id = snap.next_entity_id;
        snap.items.insert(
            id,
            InventoryItemRecord {
                item_id: id,
                type_id,
                owner_id: owner,
                location_id: location,
                flag,
                quantity: qty,
                singleton: qty == 1,
                version: 1,
                ..Default::default()
            },
        );
        id
    })
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
    if station_id.is_some() {
        rpc(ctx, &mut sess, "structureDocking", "Undock", &[]).await.expect("Undock");
    } else {
        let ship_id = sess.ship_id.expect("ship");
        sess.station_id = None;
        sess.solar_system_id = Some(sys_id);
        let _ = ctx
            .systems
            .get_or_spawn(sys_id)
            .spawn_ball_with_combat(
                Ball {
                    id: ship_id,
                    type_id: 587,
                    position: DVec3::new(0.0, 5.0e9, 0.0),
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

async fn try_anchor(
    ctx: &ServerContext,
    sess: &mut SessionState,
    type_id: u32,
    pos_arg: Option<DVec3>,
    name: &str,
) -> Result<PyRep, BridgeRpcError> {
    let ship_id = sess.ship_id.expect("ship");
    let item_id = grant(ctx, OWNER, ship_id, type_id, InventoryFlag::Cargo, 1);
    let res = match pos_arg {
        Some(p) => {
            rpc(
                ctx,
                sess,
                "structureDeployment",
                "Anchor",
                &[
                    PyRep::Integer(item_id),
                    PyRep::List(vec![PyRep::Real(p.x), PyRep::Real(p.y), PyRep::Real(p.z)]),
                    PyRep::Integer(1),
                    PyRep::Tuple(vec![PyRep::Integer(2), PyRep::Integer(18)]),
                    PyRep::String(name.into()),
                ],
            )
            .await
        }
        None => {
            rpc(
                ctx,
                sess,
                "structureDeployment",
                "Anchor",
                &[PyRep::Integer(item_id), PyRep::String(name.into())],
            )
            .await
        }
    };
    if res.is_err() {
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.remove(&item_id);
        });
    }
    res
}

#[tokio::test]
async fn test_remediated_upwell_anchoring_restrictions_end_to_end() {
    // parity: feat:structures.anchor-restrictions
    eve_testkit::attest(&["feat:structures.anchor-restrictions"]);

    let ctx = new_ctx();
    let mut sess = login_with_new_corp(&ctx, 8101, PERIMETER, "Anchor Rules Corp", "ANCR").await;
    let open_perim = DVec3::new(0.0, 5.0e9, 0.0);
    move_pilot_to_system(&ctx, &mut sess, PERIMETER, open_perim).await;

    // 1. Corporate age < 7 days rejected with CorpTooYoungToAnchorStructure.
    assert_eq!(
        try_anchor(&ctx, &mut sess, 35832, None, "Young Corp Citadel").await.expect_err("young corp").code,
        "CorpTooYoungToAnchorStructure"
    );
    ctx.advance_virtual_clock(7 * 86_400 - 20);
    assert_eq!(
        try_anchor(&ctx, &mut sess, 35832, None, "Almost 7d Citadel").await.expect_err("6.99d corp").code,
        "CorpTooYoungToAnchorStructure"
    );
    ctx.advance_virtual_clock(25);

    // 2. Trade hub systems (Jita, Amarr, Dodixie, Rens, Hek) reject Upwell anchoring.
    for hub_sys in [30_000_142u32, 30_002_187, 30_002_659, 30_002_510, 30_002_053] {
        move_pilot_to_system(&ctx, &mut sess, hub_sys, DVec3::new(0.0, 5.0e9, 0.0)).await;
        assert_eq!(
            try_anchor(&ctx, &mut sess, 35832, None, "Trade Hub Citadel").await.expect_err("trade hub").code,
            "AnchoringDisallowedInTradeHub"
        );
    }

    // 3. Starter and career-agent systems reject Upwell anchoring.
    for starter_sys in [30_000_141u32, 30_030_141] {
        move_pilot_to_system(&ctx, &mut sess, starter_sys, DVec3::new(0.0, 5.0e9, 0.0)).await;
        assert_eq!(
            try_anchor(&ctx, &mut sess, 35832, None, "Starter Citadel").await.expect_err("starter sys").code,
            "AnchoringDisallowedInStarterOrCareerSystem"
        );
    }

    // 4. Explicit ship-to-anchor coordinate distance [50 km, 75 km] and celestial/stargate 1,000 km clearance.
    move_pilot_to_system(&ctx, &mut sess, PERIMETER, open_perim).await;
    assert_eq!(
        try_anchor(&ctx, &mut sess, 35832, Some(open_perim + DVec3::new(30_000.0, 0.0, 0.0)), "Too Close Ship")
            .await
            .expect_err("30 km < 50 km")
            .code,
        "InvalidAnchorDistanceFromShip"
    );
    assert_eq!(
        try_anchor(&ctx, &mut sess, 35832, Some(open_perim + DVec3::new(90_000.0, 0.0, 0.0)), "Too Far Ship")
            .await
            .expect_err("90 km > 75 km")
            .code,
        "InvalidAnchorDistanceFromShip"
    );

    let gate = ctx
        .sde
        .celestials_in_system(PERIMETER)
        .into_iter()
        .find(|c| c.group_id == 10)
        .expect("Perimeter stargate");
    move_pilot_to_system(&ctx, &mut sess, PERIMETER, gate.position + DVec3::new(gate.radius + 500_000.0, 0.0, 0.0)).await;
    assert_eq!(
        try_anchor(&ctx, &mut sess, 35832, None, "Near Gate Citadel").await.expect_err("near gate").code,
        "TooCloseToStructureOrStation"
    );

    move_pilot_to_system(&ctx, &mut sess, PERIMETER, open_perim).await;
    let valid_rep = try_anchor(&ctx, &mut sess, 35832, Some(open_perim + DVec3::new(60_000.0, 0.0, 0.0)), "Valid Perimeter Astrahus")
        .await
        .expect("60 km anchor succeeds");
    assert!(kv_i64(&valid_rep, "structureID") > 0);

    // 5. FLEX structures require K-space sov nullsec, system sovereignty + Sov Hub ownership, online Sov Hub upgrade, 500 km structure clearance, and 100,000 km stargate clearance (Ansiblex).
    for (bad_sys, label) in [(PERIMETER, "hs"), (RANCER, "ls"), (WH_C2, "wh")] {
        move_pilot_to_system(&ctx, &mut sess, bad_sys, DVec3::new(0.0, 5.0e9, 0.0)).await;
        assert_eq!(
            try_anchor(&ctx, &mut sess, 35841, None, label).await.expect_err("flex outside sov null").code,
            "CannotAnchorFlexOutsideSovNull"
        );
    }

    let my_corp = sess.corporation_id.expect("corp");
    let my_ally = 99_810_101;
    let foreign_corp = 98_810_999;
    let foreign_ally = 99_810_999;
    let hub_id = 880_810_101;
    let upg_id = 880_810_102;
    let hed_gate = ctx
        .sde
        .celestials_in_system(HED_GP)
        .into_iter()
        .find(|c| c.group_id == 10)
        .expect("HED-GP gate");
    move_pilot_to_system(&ctx, &mut sess, HED_GP, DVec3::new(1_000_000.0, 5.0e9, 0.0)).await;

    // 5a. Foreign alliance holds system sov + Sov Hub (with upgrade online) -> rejected with FlexAnchoringRequiresSystemSovereignty.
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.insert(
            foreign_corp,
            eve_db::CorporationRecord {
                corporation_id: foreign_corp,
                alliance_id: Some(foreign_ally),
                ..Default::default()
            },
        );
        snap.system_states.insert(
            HED_GP,
            eve_db::SystemDynamicState {
                solar_system_id: HED_GP,
                sov_alliance_id: Some(foreign_ally),
                ..Default::default()
            },
        );
        snap.structures.insert(
            hub_id,
            make_structure(hub_id, 32458, foreign_corp, HED_GP, "Foreign Sov Hub", DVec3::new(0.0, 5.0e9, 0.0), true),
        );
        snap.items.insert(
            upg_id,
            InventoryItemRecord {
                item_id: upg_id,
                type_id: 81_621,
                owner_id: foreign_corp,
                location_id: hub_id,
                flag: InventoryFlag::ServiceSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(upg_id, true);
        snap.sov_runtime.sov_hubs.insert(
            hub_id,
            eve_db::PersistedSovHubData {
                hub_id,
                upgrades: vec![eve_db::PersistedHubUpgrade { type_id: 81_621, online: true, low_power: false }],
                ..Default::default()
            },
        );
    });
    assert_eq!(
        try_anchor(&ctx, &mut sess, 35841, None, "Ansiblex Foreign Sov").await.expect_err("foreign sov").code,
        "FlexAnchoringRequiresSystemSovereignty"
    );

    // 5b. Transfer system sov & Sov Hub ownership to caller's alliance (with upgrade offline first -> RequiredSovUpgradeOffline).
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.get_mut(&my_corp).unwrap().alliance_id = Some(my_ally);
        snap.system_states.get_mut(&HED_GP).unwrap().sov_alliance_id = Some(my_ally);
        snap.structures.get_mut(&hub_id).unwrap().owner_corp_id = my_corp;
        snap.items.remove(&upg_id);
        snap.module_online_states.remove(&upg_id);
        snap.sov_runtime.sov_hubs.remove(&hub_id);
    });
    assert_eq!(
        try_anchor(&ctx, &mut sess, 35841, None, "Ansiblex No Upgrade").await.expect_err("no upgrade").code,
        "RequiredSovUpgradeOffline"
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            upg_id,
            InventoryItemRecord {
                item_id: upg_id,
                type_id: 81_621, // Advanced Logistics Network (Ansiblex upgrade)
                owner_id: my_corp,
                location_id: hub_id,
                flag: InventoryFlag::ServiceSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(upg_id, true);
        snap.sov_runtime.sov_hubs.insert(
            hub_id,
            eve_db::PersistedSovHubData {
                hub_id,
                upgrades: vec![eve_db::PersistedHubUpgrade { type_id: 81_621, online: true, low_power: false }],
                ..Default::default()
            },
        );
    });
    // 50,000 km < 100,000 km stargate clearance for Ansiblex -> rejected
    move_pilot_to_system(&ctx, &mut sess, HED_GP, hed_gate.position + DVec3::new(hed_gate.radius + 50_000_000.0, 0.0, 0.0)).await;
    assert_eq!(
        try_anchor(&ctx, &mut sess, 35841, None, "Ansiblex Too Close Gate").await.expect_err("50,000 km < 100,000 km").code,
        "TooCloseToStructureOrStation"
    );
    // 120,000 km from gate, 300 km from Sov Hub (< 500 km FLEX structure clearance) -> rejected
    move_pilot_to_system(&ctx, &mut sess, HED_GP, DVec3::new(300_000.0, 5.0e9, 0.0)).await;
    assert_eq!(
        try_anchor(&ctx, &mut sess, 35841, None, "Ansiblex Too Close Hub").await.expect_err("300 km < 500 km").code,
        "TooCloseToStructureOrStation"
    );
    // 1,000 km from Sov Hub and > 100,000 km from gates -> succeeds with 45-minute (2,700s) anchoring timer and default +60 km ship offset!
    let ans_ship_pos = DVec3::new(1_000_000.0, 5.0e9, 0.0);
    move_pilot_to_system(&ctx, &mut sess, HED_GP, ans_ship_pos).await;
    let ans_id = kv_i64(&try_anchor(&ctx, &mut sess, 35841, None, "HED-GP Ansiblex").await.expect("valid Ansiblex"), "structureID");
    let ans_pos = DVec3::from_array(ctx.db.with_snapshot(|s| s.structures[&ans_id].position));
    assert!((ans_pos.distance(ans_ship_pos) - 60_000.0).abs() < 1.0, "Default 2-arg anchor must offset 60 km from ship");
    ctx.advance_virtual_clock(life::ANCHOR_VULNERABLE_SECS + 2_700 + life::ONLINING_SECS + 5);
    life::sync_all_structures(&ctx);
    assert_eq!(ctx.db.with_snapshot(|s| s.structures[&ans_id].state.clone()), life::STATE_SHIELD_VULNERABLE);

    // 5c. V24 10-positional-arg Anchor + Strategic Index scaling on FLEX structures (2,700s * (1 + strategic_index)).
    let pharo_upg_id = 880_810_103;
    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states.get_mut(&HED_GP).unwrap().strategic_index = 2;
        snap.items.insert(pharo_upg_id, InventoryItemRecord {
            item_id: pharo_upg_id, type_id: 81_615, owner_id: my_corp, location_id: hub_id,
            flag: InventoryFlag::ServiceSlot1, quantity: 1, singleton: true, version: 1, ..Default::default()
        });
        snap.module_online_states.insert(pharo_upg_id, true);
        snap.sov_runtime.sov_hubs.get_mut(&hub_id).unwrap().upgrades.push(eve_db::PersistedHubUpgrade { type_id: 81_615, online: true, low_power: false });
    });
    let pharo_ship_pos = DVec3::new(2_000_000.0, 5.0e9, 0.0);
    move_pilot_to_system(&ctx, &mut sess, HED_GP, pharo_ship_pos).await;
    let pharo_item = grant(&ctx, OWNER, sess.ship_id.unwrap(), 35840, InventoryFlag::Cargo, 1);
    assert_eq!(
        rpc(&ctx, &mut sess, "structureDeployment", "Anchor", &[
            PyRep::Integer(pharo_item), PyRep::Real(pharo_ship_pos.x + 20_000.0), PyRep::Real(pharo_ship_pos.z),
            PyRep::Real(0.0), PyRep::Integer(0), PyRep::Integer(3), PyRep::Integer(19),
            PyRep::String("Too Close 10-Arg Pharolux".into()), PyRep::String("desc".into()), PyRep::None,
        ]).await.expect_err("10-arg 20 km < 50 km").code,
        "InvalidAnchorDistanceFromShip"
    );
    let pharo_rep = rpc(&ctx, &mut sess, "structureDeployment", "Anchor", &[
        PyRep::Integer(pharo_item), PyRep::Real(pharo_ship_pos.x + 60_000.0), PyRep::Real(pharo_ship_pos.z),
        PyRep::Real(0.0), PyRep::Integer(0), PyRep::Integer(3), PyRep::Integer(19),
        PyRep::String("HED-GP Pharolux".into()), PyRep::String("V24 10-arg FLEX beacon".into()), PyRep::None,
    ]).await.expect("V24 10-arg Anchor succeeds at 60 km");
    let pharo_id = kv_i64(&pharo_rep, "structureID");
    assert_eq!(
        rpc(&ctx, &mut sess, "structureDirectory", "GetStructureDescription", &[PyRep::Integer(pharo_id)]).await.unwrap(),
        PyRep::String("V24 10-arg FLEX beacon".into())
    );
    assert_eq!(ctx.db.with_snapshot(|s| s.structure_vulnerability_windows.get(&pharo_id).copied()), Some((19, 3)));
    ctx.advance_virtual_clock(life::ANCHOR_VULNERABLE_SECS + 2_700 + life::ONLINING_SECS + 10);
    life::sync_all_structures(&ctx);
    assert_eq!(ctx.db.with_snapshot(|s| s.structures[&pharo_id].state.clone()), life::STATE_ANCHORING, "Strategic Index 2 FLEX needs 8,100s");
    ctx.advance_virtual_clock(2 * 2_700);
    life::sync_all_structures(&ctx);
    assert_eq!(ctx.db.with_snapshot(|s| s.structures[&pharo_id].state.clone()), life::STATE_SHIELD_VULNERABLE);

    // 6. Foreign Sov Null-Sec ADM > 4.0 blocks Medium structures, and Strategic Index adds +24h per level.
    let hostile_ally = 99_810_202;
    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states.insert(
            HED_GP,
            eve_db::SystemDynamicState {
                solar_system_id: HED_GP,
                sov_alliance_id: Some(hostile_ally),
                military_index: 5,
                industrial_index: 5,
                strategic_index: 2,
                ..Default::default()
            },
        );
    });
    move_pilot_to_system(&ctx, &mut sess, HED_GP, DVec3::new(3_000_000.0, 5.0e9, 0.0)).await;
    assert_eq!(
        try_anchor(&ctx, &mut sess, 35832, None, "Blocked Hostile Medium").await.expect_err("ADM > 4.0 blocks Medium").code,
        "AdmBlocksHostileMediumStructure"
    );

    // Lower ADM <= 4.0 while keeping strategic_index = 2 -> anchoring timer becomes 24h + 2 * 24h = 72h!
    ctx.db.with_mut_snapshot(|snap| {
        let st = snap.system_states.get_mut(&HED_GP).unwrap();
        st.military_index = 1;
        st.industrial_index = 1;
        st.strategic_index = 2;
    });
    let hostile_med_id = kv_i64(
        &try_anchor(&ctx, &mut sess, 35832, None, "Hostile Sov Astrahus").await.expect("ADM <= 4.0 allows Medium"),
        "structureID",
    );
    ctx.advance_virtual_clock(life::ANCHOR_VULNERABLE_SECS + life::ANCHORING_SECS + 10);
    life::sync_all_structures(&ctx);
    assert_eq!(
        ctx.db.with_snapshot(|s| s.structures[&hostile_med_id].state.clone()),
        life::STATE_ANCHORING,
        "At 24h, Strategic Index 2 hostile sov structure must still be Anchoring (needs 72h)"
    );
    ctx.advance_virtual_clock(2 * 86_400);
    life::sync_all_structures(&ctx);
    assert_eq!(
        ctx.db.with_snapshot(|s| s.structures[&hostile_med_id].state.clone()),
        life::STATE_NO_CORE,
        "At 72h (24h + 2 * 24h), Strategic Index 2 structure finishes anchoring"
    );
}

#[tokio::test]
async fn test_remediated_pochven_anchoring_and_customs_office_standing() {
    // parity: feat:structures.pochven-anchoring-restrictions
    eve_testkit::attest(&["feat:structures.pochven-anchoring-restrictions"]);

    let ctx = new_ctx();
    let mut sess = login_with_new_corp(&ctx, 8102, PERIMETER, "Pochven Ops Corp", "POCH").await;
    ctx.advance_virtual_clock(7 * 86_400 + 25);

    let planet = ctx
        .sde
        .celestials_in_system(KINO_POCHVEN)
        .into_iter()
        .find(|c| c.group_id == 7)
        .expect("Kino planet");
    let near_planet = planet.position + DVec3::new(planet.radius + 5_000_000.0, 0.0, 0.0);
    move_pilot_to_system(&ctx, &mut sess, KINO_POCHVEN, near_planet).await;

    // 1. Upwell structures cannot be anchored in Pochven.
    assert_eq!(
        try_anchor(&ctx, &mut sess, 35832, None, "Pochven Astrahus").await.expect_err("Upwell in Pochven").code,
        "AnchoringDisallowedInPochven"
    );
    assert_eq!(
        try_anchor(&ctx, &mut sess, 2233, None, "Pochven POCO via Upwell").await.expect_err("POCO via Upwell RPC").code,
        "AnchoringDisallowedInPochven"
    );

    // 2. Pre-existing Upwell structure in Pochven survives sync, allows docking, and blocks Quantum Core insertion.
    let existing_id = 880_810_201;
    let my_corp = sess.corporation_id.expect("corp");
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            existing_id,
            make_structure(existing_id, 35832, my_corp, KINO_POCHVEN, "Legacy Kino Citadel", near_planet, false),
        );
    });
    life::sync_all_structures(&ctx);
    assert!(ctx.db.with_snapshot(|s| s.structures.contains_key(&existing_id)), "Pre-existing Pochven Upwell structure must survive");
    rpc(&ctx, &mut sess, "structureDocking", "Dock", &[PyRep::Integer(existing_id)])
        .await
        .expect("Dock at pre-existing Pochven Upwell structure");
    let core_item = grant(&ctx, OWNER, sess.ship_id.unwrap(), 56201, InventoryFlag::Cargo, 1);
    assert_eq!(
        rpc(&ctx, &mut sess, "invbroker", "Add", &[PyRep::Integer(core_item), PyRep::Integer(existing_id), PyRep::Integer(172)])
            .await
            .expect_err("Quantum core insertion blocked in Pochven")
            .code,
        "QuantumCoreDisallowedInPochven"
    );
    rpc(&ctx, &mut sess, "structureDocking", "Undock", &[]).await.expect("Undock");
    move_pilot_to_system(&ctx, &mut sess, KINO_POCHVEN, near_planet).await;

    // 3. Customs Office Gantry (3962) & upgrade to Customs Office (2233) require >= 1.5 Triglavian standing (500026).
    let ship_id = sess.ship_id.unwrap();
    let gantry_id = grant(&ctx, OWNER, ship_id, 3962, InventoryFlag::Cargo, 1);
    let poco_item_id = grant(&ctx, OWNER, ship_id, 2233, InventoryFlag::Cargo, 1);
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&OWNER).unwrap().standings.insert(500_026, 1.49);
    });
    assert_eq!(
        rpc(&ctx, &mut sess, "posMgr", "AnchorOrbital", &[PyRep::Integer(gantry_id), PyRep::Integer(planet.celestial_id)])
            .await
            .expect_err("1.49 standing < 1.5")
            .code,
        "InsufficientTriglavianStanding"
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&OWNER).unwrap().standings.insert(500_026, 1.50);
    });
    let orb_rep = rpc(&ctx, &mut sess, "posMgr", "AnchorOrbital", &[PyRep::Integer(gantry_id), PyRep::Integer(planet.celestial_id)])
        .await
        .expect("1.50 standing allows AnchorOrbital");
    assert_eq!(kv_i64(&orb_rep, "orbitalID"), gantry_id);
    assert_eq!(kv_str(&orb_rep, "state"), "Anchored");

    // Drop standing below 1.5 before OnlineOrbital -> rejected with InsufficientTriglavianStanding.
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&OWNER).unwrap().standings.insert(500_026, 1.25);
    });
    assert_eq!(
        rpc(&ctx, &mut sess, "posMgr", "OnlineOrbital", &[PyRep::Integer(gantry_id), PyRep::Integer(planet.celestial_id)])
            .await
            .expect_err("OnlineOrbital requires >= 1.5 standing")
            .code,
        "InsufficientTriglavianStanding"
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&OWNER).unwrap().standings.insert(500_026, 1.75);
    });
    let upg_rep = rpc(&ctx, &mut sess, "posMgr", "OnlineOrbital", &[PyRep::Integer(gantry_id), PyRep::Integer(planet.celestial_id)])
        .await
        .expect("OnlineOrbital upgrades Gantry to Customs Office");
    assert_eq!(kv_i64(&upg_rep, "typeID"), 2233);
    assert_eq!(kv_str(&upg_rep, "state"), "ShieldVulnerable");
    assert!(ctx.db.get_item(poco_item_id).is_none(), "Customs Office upgrade item consumed");
}
