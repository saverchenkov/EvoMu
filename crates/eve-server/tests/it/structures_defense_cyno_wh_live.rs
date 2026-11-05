#![forbid(unsafe_code)]

use crate::common::{self, LivePilot};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use eve_server::services::space_destiny::wormhole_item_id_for_signature;
use eve_server::services::station_structure::structure_lifecycle::sync_all_structures;
use eve_server::services::wormhole_lifecycle_ops::wormhole_exit_item_id_for_signature;
use eve_server::sp6_lifecycle::tick_background_cluster;
use glam::DVec3;
use std::collections::HashMap;

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, svc, None, method, args, &HashMap::new(), None).await
}

async fn setup_corp_pilot_in_space(
    ctx: &ServerContext,
    sess_id: u64,
    char_id: i64,
    ship_type_id: u32,
    sys_id: u32,
    corp_name: &str,
    ticker: &str,
) -> LivePilot {
    let user = format!("live{char_id}");
    let pass = format!("{user}-secret");
    ctx.db.provision_gm_account(&user, &pass, char_id, &format!("Live Pilot {char_id}"));
    let mut sess = SessionState::new(sess_id);
    common::login_pilot(ctx, &mut sess, &user, &pass).await;
    rpc(ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)])
        .await
        .expect("SelectCharacterID");
    let created = common::slash_cmd(ctx, &mut sess, &format!("/createitem {ship_type_id} 1")).await;
    let ship_id = created.dict_get("itemID").and_then(PyRep::as_i64).expect("ship itemID");
    let skills: Vec<u32> = ctx
        .sde
        .types
        .get(&ship_type_id)
        .map(|t| {
            [182_u16, 183, 184, 1285, 1289, 1290]
                .iter()
                .filter_map(|a| t.default_attributes.get(a))
                .map(|v| v.round() as u32)
                .filter(|&s| s > 0)
                .collect()
        })
        .unwrap_or_default();
    for sid in skills {
        common::slash_cmd(ctx, &mut sess, &format!("/giveskill {sid} 5")).await;
    }
    for skill_id in [3363_u32, 3368, 11584] {
        common::slash_cmd(ctx, &mut sess, &format!("/giveskill {skill_id} 5")).await;
    }
    ctx.advance_secs(15);
    rpc(
        ctx,
        &mut sess,
        "corpRegistry",
        "AddCorporation",
        &[
            PyRep::String(corp_name.into()),
            PyRep::String(ticker.into()),
            PyRep::String("Test".into()),
            PyRep::String("https://evomu.local".into()),
            PyRep::Real(0.05),
        ],
    )
    .await
    .expect("AddCorporation");
    ctx.advance_secs(15);
    rpc(ctx, &mut sess, "ship", "Board", &[PyRep::Integer(ship_id)])
        .await
        .expect("ship.Board");
    ctx.advance_secs(31);
    rpc(ctx, &mut sess, "stationSvc", "Undock", &[])
        .await
        .expect("stationSvc.Undock");
    common::slash_cmd(ctx, &mut sess, &format!("/tr {sys_id}")).await;
    sess.ship_id = Some(ship_id);
    let bind = rpc(
        ctx,
        &mut sess,
        "beyonce",
        "MachoBindObject",
        &[PyRep::Integer(i64::from(sys_id)), PyRep::None],
    )
    .await
    .expect("MachoBindObject");
    let PyRep::Tuple(parts) = bind else { panic!("expected tuple") };
    let beyonce = parts[0].as_i64().expect("beyonce") as u64;
    ctx.advance_secs(31);
    let _ = sess.drain_notifications();
    let _ = ctx.drain_notifications_for_char(char_id);
    LivePilot {
        sess,
        char_id,
        ship_id,
        beyonce,
    }
}

fn structure_pos(ctx: &ServerContext, sys_id: u32, sid: i64) -> DVec3 {
    let arr = ctx
        .db
        .with_snapshot(|snap| snap.structures.get(&sid).map(|s| s.position))
        .expect("structure record");
    let _ = sys_id;
    DVec3::from_array(arr)
}

async fn deploy_online_astrahus_with_core(
    ctx: &ServerContext,
    p: &mut LivePilot,
    sys_id: u32,
    name: &str,
) -> (i64, DVec3) {
    let ast_id = common::stock_ship_bay(ctx, p, 35832, 1, 5).await;
    let res = rpc(
        ctx,
        &mut p.sess,
        "structureDeployment",
        "Anchor",
        &[PyRep::Integer(ast_id), PyRep::String(name.into())],
    )
    .await
    .expect("Anchor Astrahus");
    let sid = res
        .dict_get("structureID")
        .and_then(PyRep::as_i64)
        .or_else(|| res.as_i64())
        .expect("structureID");
    ctx.advance_secs(87_305);
    let _ = sync_all_structures(ctx);
    let st_pos = structure_pos(ctx, sys_id, sid);
    common::fly_to(ctx, p, st_pos - DVec3::new(15_000.0, 0.0, 0.0)).await;
    rpc(
        ctx,
        &mut p.sess,
        "stationSvc",
        "Dock",
        &[PyRep::Integer(sid), PyRep::Integer(p.ship_id)],
    )
    .await
    .expect("Dock in structure");
    let core_id = common::slash_cmd(ctx, &mut p.sess, "/createitem 56201 1")
        .await
        .dict_get("itemID")
        .and_then(PyRep::as_i64)
        .expect("coreID");
    rpc(
        ctx,
        &mut p.sess,
        "invbroker",
        "Add",
        &[PyRep::Integer(core_id), PyRep::Integer(sid), PyRep::Integer(180)],
    )
    .await
    .expect("install Quantum Core");
    ctx.advance_secs(905);
    let _ = sync_all_structures(ctx);
    let _ = ctx.systems.get_or_spawn(sys_id);
    (sid, st_pos)
}

// parity: feat:structures.board-structure
#[tokio::test]
async fn test_structures_board_structure_control_and_eject_live() {
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    const PERIMETER: u32 = 30_000_144;
    let mut p1 = setup_corp_pilot_in_space(&ctx, 7601, 93_600_001, 20_185, PERIMETER, "Board Corp", "BDRC").await;
    ctx.advance_secs(7 * 86_400 + 10);

    let (sid, st_pos) = deploy_online_astrahus_with_core(&ctx, &mut p1, PERIMETER, "Control Citadel").await;
    let original_ship = p1.ship_id;

    // 1. Pilot 1 boards/takes control of structure -> GetStructurePilot returns p1.char_id while active ship record stays original_ship
    rpc(&ctx, &mut p1.sess, "structureControl", "BoardStructure", &[PyRep::Integer(sid)])
        .await
        .expect("BoardStructure");
    let pilot_now = rpc(&ctx, &mut p1.sess, "structureControl", "GetStructurePilot", &[PyRep::Integer(sid)])
        .await
        .expect("GetStructurePilot");
    assert_eq!(pilot_now, PyRep::Integer(p1.char_id));

    // 2. While controlling structure, ship navigation is blocked (StructureCannotNavigate)
    let err_warp = rpc(
        &ctx,
        &mut p1.sess,
        "beyonce",
        "CmdSetSpeedFraction",
        &[PyRep::Real(0.5)],
    )
    .await
    .expect_err("Navigation while controlling structure must be rejected");
    assert_eq!(err_warp.code, "StructureCannotNavigate");

    // 3. Second pilot is created docked at Jita 4-4, joins corp, receives Director role, undocks to Perimeter, docks in structure,
    //    and attempts BoardStructure -> StructureAlreadyControlled
    let corp_id = p1.sess.corporation_id.expect("p1 corp_id");
    let (mut p2_sess, p2_ship) =
        crate::structures_lookup_raffle_live::gm_docked_in_jita(&ctx, 7602, 93_600_002, 587).await;
    let app_kv = rpc(
        &ctx,
        &mut p2_sess,
        "corpRegistry",
        "InsertApplication",
        &[PyRep::Integer(corp_id), PyRep::String("Co-director".into())],
    )
    .await
    .expect("InsertApplication");
    let app_id = app_kv.dict_get("applicationID").and_then(PyRep::as_i64).expect("applicationID");
    rpc(
        &ctx,
        &mut p1.sess,
        "corpRegistry",
        "UpdateApplicationOffer",
        &[PyRep::Integer(app_id), PyRep::Integer(93_600_002), PyRep::Integer(corp_id), PyRep::Integer(4)],
    )
    .await
    .expect("accept application");
    rpc(
        &ctx,
        &mut p1.sess,
        "corpRegistry",
        "UpdateMember",
        &[
            PyRep::Integer(93_600_002),
            PyRep::Integer(i64::try_from(eve_db::corp_roles::ROLE_DIRECTOR).expect("director role")),
        ],
    )
    .await
    .expect("grant Director role");
    p2_sess.corporation_id = Some(corp_id);
    p2_sess.corp_role = eve_db::corp_roles::ROLE_DIRECTOR;
    let mut p2 =
        crate::structures_lookup_raffle_live::undock_to_system(&ctx, &mut p2_sess, 93_600_002, p2_ship, PERIMETER).await;
    common::fly_to(&ctx, &mut p2, st_pos - DVec3::new(15_000.0, 0.0, 0.0)).await;
    rpc(&ctx, &mut p2.sess, "stationSvc", "Dock", &[PyRep::Integer(sid), PyRep::Integer(p2.ship_id)])
        .await
        .expect("p2 dock in structure");
    let err_busy = rpc(&ctx, &mut p2.sess, "structureControl", "BoardStructure", &[PyRep::Integer(sid)])
        .await
        .expect_err("Second pilot cannot board already-controlled structure");
    assert_eq!(err_busy.code, "StructureAlreadyControlled");

    // 4. Pilot 1 ejects from structure -> returns to ship docked inside structure; TakeControl/ReleaseControl/Undock work
    let _ = original_ship;
    rpc(&ctx, &mut p1.sess, "structureControl", "EjectFromStructure", &[PyRep::Integer(sid)])
        .await
        .expect("EjectFromStructure");
    let pilot_after = rpc(&ctx, &mut p1.sess, "structureControl", "GetStructurePilot", &[PyRep::Integer(sid)])
        .await
        .expect("GetStructurePilot after eject");
    assert_eq!(pilot_after, PyRep::None);

    rpc(&ctx, &mut p1.sess, "structureControl", "TakeControl", &[PyRep::Integer(sid)])
        .await
        .expect("TakeControl");
    assert_eq!(
        rpc(&ctx, &mut p1.sess, "structureControl", "GetStructurePilot", &[PyRep::Integer(sid)])
            .await
            .unwrap(),
        PyRep::Integer(p1.char_id)
    );
    rpc(&ctx, &mut p1.sess, "structureControl", "ReleaseControl", &[PyRep::Integer(sid)])
        .await
        .expect("ReleaseControl");
    assert_eq!(
        rpc(&ctx, &mut p1.sess, "structureControl", "GetStructurePilot", &[PyRep::Integer(sid)])
            .await
            .unwrap(),
        PyRep::None
    );

    // After releasing control and undocking, rebind beyonce in space and verify ship movement succeeds
    rpc(&ctx, &mut p1.sess, "stationSvc", "Undock", &[])
        .await
        .expect("Undock after ReleaseControl");
    let rebind = rpc(
        &ctx,
        &mut p1.sess,
        "beyonce",
        "MachoBindObject",
        &[PyRep::Integer(i64::from(PERIMETER)), PyRep::None],
    )
    .await
    .expect("rebind beyonce after Undock");
    let PyRep::Tuple(rebind_parts) = rebind else { panic!("expected tuple") };
    let new_beyonce = rebind_parts[0].as_i64().expect("new beyonce") as u64;
    ctx.dispatch_rpc(
        &mut p1.sess,
        "beyonce",
        Some(new_beyonce),
        "CmdSetSpeedFraction",
        &[PyRep::Real(0.5)],
        &HashMap::new(),
        None,
    )
    .await
    .expect("CmdSetSpeedFraction after ReleaseControl");
}

// parity: feat:structures.defense-settings
#[tokio::test]
async fn test_structures_defense_settings_and_standup_weapons_live() {
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    const RANCER: u32 = 30_002_718;
    let mut director = setup_corp_pilot_in_space(&ctx, 7701, 93_700_001, 20_185, RANCER, "Defense Corp", "DEFC").await;
    ctx.advance_secs(7 * 86_400 + 10);

    let (sid, struct_pos) = deploy_online_astrahus_with_core(&ctx, &mut director, RANCER, "Auto-Defense Citadel").await;

    // Fit Standup Multirole Missile Launcher I (35922) + compatible Standup missile charge at flag 27
    let wep_id = common::slash_cmd(&ctx, &mut director.sess, "/createitem 35922 1")
        .await
        .dict_get("itemID")
        .and_then(PyRep::as_i64)
        .expect("Standup weapon itemID");
    rpc(&ctx, &mut director.sess, "invbroker", "Add", &[PyRep::Integer(wep_id), PyRep::Integer(sid), PyRep::Integer(27)])
        .await
        .expect("fit Standup weapon in HiSlot0 (27)");

    let charge_group = ctx
        .sde
        .types
        .get(&35922)
        .and_then(|t| t.default_attributes.get(&604))
        .map(|v| *v as u32)
        .expect("chargeGroup1 on 35922");
    let ammo_tid = ctx
        .sde
        .types
        .values()
        .filter(|t| t.group_id == charge_group)
        .filter(|t| {
            t.default_attributes.get(&114).copied().unwrap_or(0.0)
                + t.default_attributes.get(&116).copied().unwrap_or(0.0)
                + t.default_attributes.get(&117).copied().unwrap_or(0.0)
                + t.default_attributes.get(&118).copied().unwrap_or(0.0)
                > 0.0
        })
        .min_by_key(|t| t.type_id)
        .map(|t| t.type_id)
        .expect("SDE missile charge type with damage");
    let ammo_id = common::slash_cmd(&ctx, &mut director.sess, &format!("/createitem {ammo_tid} 100"))
        .await
        .dict_get("itemID")
        .and_then(PyRep::as_i64)
        .expect("Standup ammo itemID");
    rpc(&ctx, &mut director.sess, "invbroker", "Add", &[PyRep::Integer(ammo_id), PyRep::Integer(sid), PyRep::Integer(27)])
        .await
        .expect("load Standup missile charge into HiSlot0 (27)");

    // Create & save defense profile enabling auto-defense for standing < -2.0 / attackers
    let prof_res = rpc(
        &ctx,
        &mut director.sess,
        "structureProfiles",
        "CreateProfile",
        &[PyRep::String("Hostile Standing Auto-Defense".into())],
    )
    .await
    .expect("CreateProfile");
    let profile_id = prof_res
        .dict_get("profileID")
        .and_then(PyRep::as_i64)
        .or_else(|| prof_res.as_i64())
        .expect("profile_id");

    rpc(
        &ctx,
        &mut director.sess,
        "structureProfiles",
        "SaveProfileSettings",
        &[
            PyRep::Integer(profile_id),
            PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::Integer(17),
                PyRep::Tuple(vec![
                    PyRep::Bool(true),  // auto_defense_enabled
                    PyRep::Bool(true),  // auto_target_attackers
                    PyRep::Bool(true),  // auto_target_war_targets
                    PyRep::Real(-2.0),  // standing_threshold
                    PyRep::Bool(true),  // target_blocked_acl
                ]),
                PyRep::Integer(0),
            ])]),
        ],
    )
    .await
    .expect("SaveProfileSettings");

    rpc(
        &ctx,
        &mut director.sess,
        "structureProfiles",
        "ChangeProfiles",
        &[PyRep::List(vec![PyRep::Integer(sid)]), PyRep::Integer(profile_id)],
    )
    .await
    .expect("ChangeProfiles");
    ctx.advance_secs(10_805);
    let _ = sync_all_structures(&ctx);

    // Spawn hostile Drake (24698) and neutral Drake outside 55km tether bubble (~65km from center) and break post-warp invulnerability
    let mut hostile = common::gm_pilot_in_space(&ctx, 7702, 93_700_002, 24_698, RANCER).await;
    common::fly_to(&ctx, &mut hostile, struct_pos - DVec3::new(65_000.0, 0.0, 0.0)).await;
    ctx.dispatch_rpc(
        &mut hostile.sess,
        "beyonce",
        Some(hostile.beyonce),
        "CmdSetSpeedFraction",
        &[PyRep::Real(0.1)],
        &HashMap::new(),
        None,
    )
    .await
    .expect("hostile breaks post-warp invulnerability");

    let mut friendly = common::gm_pilot_in_space(&ctx, 7703, 93_700_003, 24_698, RANCER).await;
    common::fly_to(&ctx, &mut friendly, struct_pos - DVec3::new(65_000.0, -15_000.0, 0.0)).await;
    ctx.dispatch_rpc(
        &mut friendly.sess,
        "beyonce",
        Some(friendly.beyonce),
        "CmdSetSpeedFraction",
        &[PyRep::Real(0.1)],
        &HashMap::new(),
        None,
    )
    .await
    .expect("friendly breaks post-warp invulnerability");

    // Step simulation so structure locks (~25s for Drake) and fires Standup missile launcher at hostile pilot
    ctx.advance_secs(5);
    let sys_handle = ctx.systems.get_or_spawn(RANCER);
    let hostile_before = sys_handle.get_combat_state(hostile.ship_id).await.unwrap().unwrap().shield_hp;
    let friendly_before = sys_handle.get_combat_state(friendly.ship_id).await.unwrap().unwrap().shield_hp;

    rpc(
        &ctx,
        &mut director.sess,
        "corpRegistry",
        "AddCorporateContact",
        &[PyRep::Integer(hostile.char_id), PyRep::Real(-5.0)],
    )
    .await
    .expect("AddCorporateContact -5.0");

    for _ in 0..90 {
        sys_handle.step_manual(1.0).await.unwrap();
    }

    let hostile_after = sys_handle
        .get_combat_state(hostile.ship_id)
        .await
        .unwrap()
        .map(|cs| cs.shield_hp);
    let friendly_after = sys_handle.get_combat_state(friendly.ship_id).await.unwrap().unwrap().shield_hp;
    assert!(
        hostile_after.is_none_or(|hp| hp < hostile_before),
        "Standup auto-defense must lock and damage or destroy hostile pilot (before={hostile_before}, after={hostile_after:?})"
    );
    assert!(
        (friendly_after - friendly_before).abs() < 1e-6,
        "Standup auto-defense must NOT fire on neutral/friendly pilot"
    );
}

// parity: feat:structures.mobile-cyno-inhibitor
#[tokio::test]
async fn test_structures_mobile_cyno_inhibitor_lifecycle_live() {
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    const RANCER: u32 = 30_002_718; // Lowsec (sec < 0.45)

    // Pilot 1 anchors MCI (33476)
    let mut p_anchor = common::gm_pilot_in_space(&ctx, 7801, 93_800_001, 20_185, RANCER).await;
    common::slash_cmd(&ctx, &mut p_anchor.sess, "/giveskill 11584 5").await;
    common::slash_cmd(&ctx, &mut p_anchor.sess, "/fit me 438").await;
    let mci_origin = common::fly_to(&ctx, &mut p_anchor, DVec3::new(80_000.0, 0.0, 0.0)).await;
    let mci_item_id = common::stock_ship_bay(&ctx, &mut p_anchor, 33476, 1, 5).await;

    let t0 = ctx.now_unix();
    let anchor_kv = rpc(&ctx, &mut p_anchor.sess, "posMgr", "AnchorStructure", &[PyRep::Integer(mci_item_id)])
        .await
        .expect("AnchorStructure MCI");
    let mci_id = anchor_kv.dict_get("itemID").and_then(PyRep::as_i64).expect("MCI itemID");
    assert_eq!(anchor_kv.dict_get("anchoringDelaySeconds").and_then(PyRep::as_f64), Some(120.0));
    assert_eq!(anchor_kv.dict_get("despawnUnix").and_then(PyRep::as_i64), Some((t0 + 3_600) as i64));

    // Second MCI within 200 km fails with MobileCynoInhibitorOverlap
    let mci2_id = common::stock_ship_bay(&ctx, &mut p_anchor, 33476, 1, 5).await;
    let err_overlap = rpc(&ctx, &mut p_anchor.sess, "posMgr", "AnchorStructure", &[PyRep::Integer(mci2_id)])
        .await
        .expect_err("Second MCI within 200 km must fail");
    assert_eq!(err_overlap.code, "MobileCynoInhibitorOverlap");

    // Helper closure to provision a Force Recon (11957 = Rapier) with Cyno + Liquid Ozone in fleet
    // Pilots spawn at (10_000, 0, 0), which is 70 km (< 100 km radius) from mci_origin (80_000, 0, 0).
    async fn provision_cyno_recon(
        ctx: &ServerContext,
        sess_id: u64,
        char_id: i64,
        sys_id: u32,
        cyno_type: u32,
    ) -> (LivePilot, i64) {
        let mut p = common::gm_pilot_in_space(ctx, sess_id, char_id, 11_957, sys_id).await;
        common::slash_cmd(ctx, &mut p.sess, "/giveskill 21603 5").await;
        rpc(ctx, &mut p.sess, "fleetObjectHandler", "CreateFleet", &[])
            .await
            .expect("CreateFleet");
        let cyno_mod = common::slash_cmd(ctx, &mut p.sess, &format!("/fit me {cyno_type}"))
            .await
            .dict_get("itemID")
            .and_then(PyRep::as_i64)
            .expect("fit cyno module");
        common::stock_ship_bay(ctx, &mut p, 16_273, 500, 5).await; // Liquid Ozone
        (p, cyno_mod)
    }

    // 1. During 120s activation delay (t0 + 5s), standard Cyno at 70 km succeeds (inhibitor still spooling up)
    let (mut p_spool, cyno_spool) = provision_cyno_recon(&ctx, 7802, 93_800_002, RANCER, 21_096).await;
    ctx.advance_secs(5);
    rpc(&ctx, &mut p_spool.sess, "dogmaIM", "Activate", &[PyRep::Integer(cyno_spool)])
        .await
        .expect("Standard cyno during 120s spool-up delay must succeed");

    // Advance past 120s spool-up (t0 + 130s): MCI is now online and active!
    ctx.advance_secs(125);
    tick_background_cluster(&ctx).await;

    // 2. Standard Cyno at 70 km (< 100 km radius) fails with CynoInhibited
    let (mut p_inhibited, cyno_inhibited) = provision_cyno_recon(&ctx, 7803, 93_800_003, RANCER, 21_096).await;
    let err_inhib = rpc(&ctx, &mut p_inhibited.sess, "dogmaIM", "Activate", &[PyRep::Integer(cyno_inhibited)])
        .await
        .expect_err("Standard cyno within 100 km of online MCI must be inhibited");
    assert_eq!(err_inhib.code, "CynoInhibited");

    // 3. Covert Cyno (28646) at 70 km succeeds even inside 100 km suppression radius
    let (mut p_covert, cyno_covert) = provision_cyno_recon(&ctx, 7804, 93_800_004, RANCER, 28_646).await;
    rpc(&ctx, &mut p_covert.sess, "dogmaIM", "Activate", &[PyRep::Integer(cyno_covert)])
        .await
        .expect("Covert cyno within 100 km of online MCI must succeed");

    // 4. Standard Cyno at 130 km (> 100 km radius) succeeds
    let (mut p_outside, cyno_outside) = provision_cyno_recon(&ctx, 7805, 93_800_005, RANCER, 21_096).await;
    common::slash_cmd(&ctx, &mut p_outside.sess, "/fit me 438").await;
    common::fly_to(&ctx, &mut p_outside, mci_origin - DVec3::new(130_000.0, 0.0, 0.0)).await;
    rpc(&ctx, &mut p_outside.sess, "dogmaIM", "Activate", &[PyRep::Integer(cyno_outside)])
        .await
        .expect("Standard cyno outside 100 km radius must succeed");

    // 5. After 1-hour lifetime (t0 + 3605s), MCI despawns and standard Cyno at 50 km succeeds again
    let rem = (t0 + 3_605).saturating_sub(ctx.now_unix());
    ctx.advance_secs(rem);
    tick_background_cluster(&ctx).await;

    let sys_handle = ctx.systems.get_or_spawn(RANCER);
    assert!(
        sys_handle.get_ball_snapshot(mci_id).await.unwrap().is_none(),
        "Mobile Cynosural Inhibitor ball must despawn after 3600s lifetime"
    );
    rpc(&ctx, &mut p_inhibited.sess, "dogmaIM", "Activate", &[PyRep::Integer(cyno_inhibited)])
        .await
        .expect("Standard cyno at 50 km must succeed once expired MCI despawns");
}

// parity: feat:wormholes.lifetime-decay-stages
#[tokio::test]
async fn test_wormholes_lifetime_decay_stages_and_despawn_live() {
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let long_wh_sys = (31_000_007_u32..=31_002_605)
        .find(|&sid| {
            eve_server::services::wormhole_lifecycle_ops::wspace_static_specs_for_system(&ctx.sde, sid)
                .iter()
                .any(|(_, _, tid, _)| ctx.sde.wormhole_types.get(tid).is_some_and(|w| w.max_stable_time_min > 1_440.0))
        })
        .unwrap_or(31_002_000);
    const C3_SYS: u32 = 31_000_900;
    const CHAR_LONG: i64 = 93_900_001;
    const CHAR_C3: i64 = 93_900_002;
    let mut pilot_long = common::gm_pilot_in_space(&ctx, 7901, CHAR_LONG, 606, long_wh_sys).await;
    let mut pilot_c3 = common::gm_pilot_in_space(&ctx, 7902, CHAR_C3, 606, C3_SYS).await;

    let mut wh_sigs: Vec<(u32, String, u32)> = Vec::new();
    for (sys_id, char_id, p) in [(long_wh_sys, CHAR_LONG, &mut pilot_long), (C3_SYS, CHAR_C3, &mut pilot_c3)] {
        let init_scan = rpc(&ctx, &mut p.sess, "scanMgr", "GetSystemScanMgr", &[])
            .await
            .expect("scanMgr.GetSystemScanMgr");
        let PyRep::List(init_sigs) = init_scan.dict_get("signatures").cloned().unwrap() else { panic!("signatures list"); };
        for s in &init_sigs {
            if let Some(sig_id) = s.dict_get("signatureID").and_then(PyRep::as_str) {
                ctx.record_character_scan_progress(char_id, sys_id, sig_id, 1.0);
            }
        }
        let resolved_init = rpc(&ctx, &mut p.sess, "scanMgr", "GetSystemScanMgr", &[])
            .await
            .expect("resolved scanMgr.GetSystemScanMgr");
        let PyRep::List(resolved_list) = resolved_init.dict_get("signatures").cloned().unwrap() else { panic!("signatures list"); };
        for s in &resolved_list {
            if s.dict_get("group").and_then(PyRep::as_str) == Some("Wormhole") {
                if let (Some(sid), Some(dest)) = (
                    s.dict_get("signatureID").and_then(PyRep::as_str),
                    s.dict_get("destinationSystemID").and_then(PyRep::as_i64),
                ) {
                    wh_sigs.push((sys_id, sid.to_string(), dest as u32));
                }
            }
        }
    }
    assert!(!wh_sigs.is_empty(), "W-space systems must spawn live wormholes");

    // Step in 30-minute increments across the full 24h lifetime (48 * 1800s = 24h) so every single decay stage (<1 day, <4h, <1h) is observed
    let (mut saw_lt_1d, mut saw_lt_4h, mut saw_lt_1h) = (false, false, false);
    for step_idx in 0..48_u32 {
        if step_idx > 0 {
            ctx.advance_secs(1_800);
        }
        for (sys_id, p) in [(long_wh_sys, &mut pilot_long), (C3_SYS, &mut pilot_c3)] {
            let scan = rpc(&ctx, &mut p.sess, "scanMgr", "GetSystemScanMgr", &[])
                .await
                .expect("scanMgr.GetSystemScanMgr");
            let PyRep::List(rows) = scan.dict_get("signatures").cloned().unwrap() else { panic!("signatures list"); };
            for row in &rows {
                let Some(sig_id) = row.dict_get("signatureID").and_then(PyRep::as_str) else { continue };
                if !wh_sigs.iter().any(|(s, id, _)| *s == sys_id && id == sig_id) { continue; }
                let bucket = row.dict_get("lifetimeBucket").and_then(PyRep::as_str).unwrap_or("");
                let desc = row.dict_get("lifetimeDescription").and_then(PyRep::as_str).unwrap_or("");
                let stage = row.dict_get("lifetimeStage").and_then(PyRep::as_str).unwrap_or("");
                match bucket {
                    "<1 day" => {
                        assert_eq!(stage, "BeginningToDecay");
                        assert_eq!(desc, "This wormhole is beginning to decay, and probably won't last another day");
                        saw_lt_1d = true;
                    }
                    "<4h" => {
                        assert_eq!(stage, "ReachingEndOfLifetime");
                        assert_eq!(desc, "This wormhole is reaching the end of its natural lifetime and is likely to collapse very soon");
                        saw_lt_4h = true;
                    }
                    "<1h" => {
                        assert_eq!(stage, "ReachingEndOfLifetime");
                        assert_eq!(desc, "This wormhole is reaching the end of its natural lifetime and is likely to collapse very soon");
                        saw_lt_1h = true;
                    }
                    _ => {}
                }
            }
        }
    }
    assert!(
        saw_lt_1d && saw_lt_4h && saw_lt_1h,
        "Must observe all 3 wormhole decay stages (<1 day, <4h, <1h)"
    );

    ctx.advance_secs(5 * 3_600);
    tick_background_cluster(&ctx).await;

    for (orig_sys, orig_sig_id, dest_sys) in &wh_sigs {
        let ent_id = wormhole_item_id_for_signature(*orig_sys, orig_sig_id);
        let exit_id = wormhole_exit_item_id_for_signature(*dest_sys, orig_sig_id);
        assert!(ctx.systems.get_or_spawn(*orig_sys).get_ball_snapshot(ent_id).await.unwrap().is_none());
        assert!(ctx.systems.get_or_spawn(*dest_sys).get_ball_snapshot(exit_id).await.unwrap().is_none());
    }
}
