//! Dedicated Exploit & Negative Security Regression Tests.
//!
//! Security Invariants Covered:
//! - Entitlement user API enforces genuine admin role check (`GrantRequest`).
//! - Quasar PLEX Vault enforces admin authorization, positive amounts, and balance bounds.
//! - Corporation governance `GetShareholders` preserves read-only invariant on unissued corps.
//! - Bounty placement rejects non-finite (NaN, Inf) and negative ISK amounts.
//! - Crimewatch state mutations are synchronized atomically with persistence snapshots.
//! - Bounty placement, kill claims, and GM reimbursements commit atomically with strict ISK conservation.
//! - System actor spawning does not acquire subsystem locks while awaiting database snapshots.
//! - In-space character reconnection preserves live position, capacitor, and tackle state.
//! - Weapon damage (missiles, smartbombs, drones, fighters) directly triggers victim capsuleer PvP logoff timers.

use eve_db::{CharacterRecord, IskCents};
use eve_destiny::{Ball, DamageVector, TargetLock};
use eve_machonet::{service_roles, SessionState};
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, EvePublicRequest,
    EvePublicResponse, GoogleAny, GoogleTimestamp, QuasarMessage, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::quasar_gateway::{STATUS_BAD_REQUEST, STATUS_FORBIDDEN, STATUS_OK};
use eve_server::router::context::ActiveFighterSquadron;
use eve_server::router::ServerContext;
use eve_server::services::combat_crimewatch::{
    activate_aoe_or_defender_module, claim_player_bounty_on_kill, gm_admin_bounty_action,
    step_combat_system, with_combat_runtime, with_mut_combat_runtime, DroneSpaceState,
    InFlightMissileState,
};
use glam::DVec3;
use std::collections::HashMap;
use std::sync::Arc;

// ============================================================================
// Shared Test Helpers
// ============================================================================

async fn call(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None).await
}

fn extract_response_proto(rep: &PyRep) -> EvePublicResponse {
    if let PyRep::KeyVal(entries) = rep {
        for (k, v) in entries {
            if k == "response_proto" {
                if let PyRep::Bytes(b) = v {
                    return EvePublicResponse::decode_proto(b).expect("decode EvePublicResponse");
                }
            }
        }
    }
    panic!("Expected KeyVal containing response_proto, got {rep:?}");
}

async fn send_quasar(
    ctx: &ServerContext,
    sess: &mut SessionState,
    type_name: &str,
    payload: Vec<u8>,
) -> EvePublicResponse {
    let req = EvePublicRequest {
        issued: Some(GoogleTimestamp { seconds: ctx.now_unix() as i64, nanos: 0 }),
        correlation_uuid: b"sec-exploit-test".to_vec(),
        payload: Some(GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"),
            value: payload,
        }),
        external_origin: "eve-sec-test".into(),
        application_instance_uuid: b"app-sec-exploit".to_vec(),
        authoritative_context: None,
        trace_context: None,
    };
    let rep = ctx
        .dispatch_rpc(
            sess,
            "publicGatewaySvc",
            None,
            "SendRequest",
            &[PyRep::Bytes(req.encode_proto())],
            &HashMap::new(),
            None,
        )
        .await
        .expect("publicGatewaySvc.SendRequest");
    extract_response_proto(&rep)
}

fn make_char(char_id: i64, user_id: u64, ship_id: i64, bal_isk: f64) -> CharacterRecord {
    CharacterRecord {
        character_id: char_id, user_id, name: format!("Pilot {char_id}"),
        corporation_id: 1_000_001, alliance_id: None, war_faction_id: None, fleet_id: None, autopilot_waypoints: Vec::new(),
        station_id: None, structure_id: None,
        solar_system_id: 30_000_142, constellation_id: 20_000_020, region_id: 10_000_002,
        active_ship_id: ship_id, balance: IskCents::from_f64_isk(bal_isk),
        plex_balance: 0, loyalty_points: HashMap::new(), standings: HashMap::new(),
        security_status: 0.0, clone_grade: 2, home_station_id: 60003760,
        race_id: 1, bloodline_id: 1, ancestry_id: 1, school_id: 1,
        neural: eve_db::NeuralAttributes::default(), skill_points: 1_000_000.0,
        activity_status: None,
            free_skill_points: 0.0, last_clone_jump_unix: 0, jump_fatigue_minutes: 0.0,
        jump_cooldown_until_unix: 0, jump_fatigue_until_unix: 0,
        description: String::new(), title: String::new(), gender: 1,
        create_date_unix: 0, version: 1, system_epoch: 1,
    }
}

async fn setup_combat_pair(ctx: &ServerContext) -> (i64, i64, i64, i64, u32, u64) {
    let attacker_id = 90_000_070i64;
    let attacker_ship_id = 80_000_070i64;
    let victim_id = 90_000_080i64;
    let victim_ship_id = 80_000_080i64;
    let sys_id = 30_000_142u32;
    let now_unix = ctx.now_unix();

    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.insert(attacker_id, make_char(attacker_id, 91, attacker_ship_id, 0.0));
        snap.characters.insert(victim_id, make_char(victim_id, 92, victim_ship_id, 0.0));
    });

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let mut att_cs = ctx.build_ship_combat_state(attacker_id, attacker_ship_id, 587);
    att_cs.capacitor = 1000.0;
    sys_handle.try_spawn_ball_with_meta(
        Ball {
            id: attacker_ship_id, type_id: 587, position: DVec3::ZERO, velocity: DVec3::ZERO,
            max_velocity: 300.0, inertia: 1.0, mass: 1_000_000.0, radius: 35.0,
            speed_fraction: 0.0, mode: eve_destiny::MovementMode::Stop,
        },
        att_cs,
        eve_destiny::SlimItemMeta {
            group_id: 25, category_id: 6, owner_id: attacker_id, char_id: Some(attacker_id),
            corp_id: 1000001, alliance_id: -1, security_status: 0.0, is_cloaked: false, ..Default::default()
        },
    );

    sys_handle.try_spawn_ball_with_meta(
        Ball {
            id: victim_ship_id, type_id: 587, position: DVec3::new(100.0, 0.0, 0.0), velocity: DVec3::ZERO,
            max_velocity: 300.0, inertia: 1.0, mass: 1_000_000.0, radius: 35.0,
            speed_fraction: 0.0, mode: eve_destiny::MovementMode::Stop,
        },
        ctx.build_ship_combat_state(victim_id, victim_ship_id, 587),
        eve_destiny::SlimItemMeta {
            group_id: 25, category_id: 6, owner_id: victim_id, char_id: Some(victim_id),
            corp_id: 1000001, alliance_id: -1, security_status: 0.0, is_cloaked: false, ..Default::default()
        },
    );
    (attacker_id, attacker_ship_id, victim_id, victim_ship_id, sys_id, now_unix)
}

fn assert_victim_pvp_timer(ctx: &ServerContext, victim_id: i64, expected_until: u64, msg: &str) {
    let vic_timer = ctx.crimewatch.read().unwrap().states.get(&victim_id).map(|s| s.pvp_timer_until_unix).unwrap_or(0);
    assert_eq!(vic_timer, expected_until, "{msg} (runtime crimewatch)");
    eve_server::session_cluster_lifecycle::with_sp6_state(&ctx.db, |st| {
        let pvp_until = st.pvp_timers_until.get(&victim_id).copied().unwrap_or(0);
        assert_eq!(pvp_until, expected_until, "{msg} (SP-6 persisted state)");
    });
}

// ============================================================================
// 1. Entitlement Grant Role Enforcement
// ============================================================================

#[tokio::test]
async fn test_entitlement_grant_admin_role_enforcement() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);

    let mut session = SessionState::new(3_001);
    session.user_id = Some(42);
    session.character_id = Some(1001);
    session.role = service_roles::ROLE_PLAYER;

    let mut ent_payload = Vec::new();
    encode_string_field(3, "omega_special_access", &mut ent_payload);
    let mut ent_outer = Vec::new();
    encode_bytes_field(1, &ent_payload, &mut ent_outer);

    // Negative exploit test: unprivileged plain user must NOT be permitted to grant entitlements (403)
    let denied = send_quasar(&ctx, &mut session, "eve.entitlement.user.api.GrantRequest", ent_outer.clone()).await;
    assert_eq!(denied.status_code, STATUS_FORBIDDEN, "Plain user call to GrantRequest must return 403 Forbidden without GM role");

    // Verify entitlement was NOT added to user
    let get_rsp = send_quasar(&ctx, &mut session, "eve_public.entitlement.user.api.GetRequest", Vec::new()).await;
    assert_eq!(get_rsp.status_code, STATUS_OK);
    let payload1 = get_rsp.payload.unwrap();
    let get_str = String::from_utf8_lossy(&payload1.value);
    assert!(!get_str.contains("omega_special_access"), "Denied unprivileged grant must not modify user entitlements");

    // Legitimate admin with GM role MUST succeed (200)
    session.role = service_roles::ROLE_GM_DEFAULT;
    let allowed = send_quasar(&ctx, &mut session, "eve.entitlement.user.api.GrantRequest", ent_outer).await;
    assert_eq!(allowed.status_code, STATUS_OK, "Admin/GM call to GrantRequest must succeed with 200 OK");

    // Verify entitlement is now present
    let get_rsp2 = send_quasar(&ctx, &mut session, "eve_public.entitlement.user.api.GetRequest", Vec::new()).await;
    let payload2 = get_rsp2.payload.unwrap();
    let get_str2 = String::from_utf8_lossy(&payload2.value);
    assert!(get_str2.contains("omega_special_access"), "Admin grant must persist into user entitlements");
}

// ============================================================================
// 2. Quasar PLEX Vault & Item Delivery Authorization Checks
// ============================================================================

#[tokio::test]
async fn test_plex_vault_and_store_fulfillment_admin_enforcement() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);

    let mut session = SessionState::new(3_002);
    session.user_id = Some(50);
    session.character_id = Some(1001);
    session.role = service_roles::ROLE_PLAYER;

    // 1. Negative exploit: Plain user attempting DepositRequest must return 403 Forbidden
    let mut dep_curr = Vec::new();
    encode_varint_field(1, 500 * 100, &mut dep_curr);
    let mut dep_body = Vec::new();
    encode_bytes_field(3, &dep_curr, &mut dep_body);

    let dep_denied = send_quasar(&ctx, &mut session, "eve.plex.vault.assetholding.operation.api.DepositRequest", dep_body.clone()).await;
    assert_eq!(dep_denied.status_code, STATUS_FORBIDDEN, "Plain user calling DepositRequest must receive 403 Forbidden");

    // 2. Negative exploit: Plain user attempting WithdrawRequest must return 403 Forbidden
    let mut with_curr = Vec::new();
    encode_varint_field(1, 100 * 100, &mut with_curr);
    let mut with_body = Vec::new();
    encode_bytes_field(3, &with_curr, &mut with_body);

    let with_denied = send_quasar(&ctx, &mut session, "eve.plex.vault.assetholding.operation.api.WithdrawRequest", with_body.clone()).await;
    assert_eq!(with_denied.status_code, STATUS_FORBIDDEN, "Plain user calling WithdrawRequest must receive 403 Forbidden");

    // 3. Negative exploit: Plain user attempting DeliverItemRequest must return 403 Forbidden
    let mut item_type = Vec::new();
    encode_varint_field(1, 44992, &mut item_type); // PLEX item
    let mut fulfill_req = Vec::new();
    encode_bytes_field(2, &item_type, &mut fulfill_req);
    encode_varint_field(3, 1000, &mut fulfill_req);

    let fulfill_denied = send_quasar(&ctx, &mut session, "eve.fulfillment.api.DeliverItemRequest", fulfill_req.clone()).await;
    assert_eq!(fulfill_denied.status_code, STATUS_FORBIDDEN, "Plain user calling DeliverItemRequest must receive 403 Forbidden");

    // 4. Elevate to GM/Admin role
    session.role = service_roles::ROLE_GM_DEFAULT;

    // GM with valid deposit succeeds
    let dep_ok = send_quasar(&ctx, &mut session, "eve.plex.vault.assetholding.operation.api.DepositRequest", dep_body).await;
    assert_eq!(dep_ok.status_code, STATUS_OK);

    // GM attempting negative withdraw must be rejected with 400 Bad Request
    let mut neg_curr = Vec::new();
    encode_varint_field(1, eve_proto::quasar::encode_zigzag64(-50000i64), &mut neg_curr);
    let mut neg_with_body = Vec::new();
    encode_bytes_field(3, &neg_curr, &mut neg_with_body);

    let neg_with_rsp = send_quasar(&ctx, &mut session, "eve.plex.vault.assetholding.operation.api.WithdrawRequest", neg_with_body).await;
    assert_eq!(neg_with_rsp.status_code, STATUS_BAD_REQUEST, "Negative withdraw must be rejected with 400 Bad Request");

    // GM attempting withdraw exceeding balance must be rejected with 400 Bad Request
    let mut huge_curr = Vec::new();
    encode_varint_field(1, 999_999 * 100, &mut huge_curr);
    let mut huge_with_body = Vec::new();
    encode_bytes_field(3, &huge_curr, &mut huge_with_body);

    let huge_with_rsp = send_quasar(&ctx, &mut session, "eve.plex.vault.assetholding.operation.api.WithdrawRequest", huge_with_body).await;
    assert_eq!(huge_with_rsp.status_code, STATUS_BAD_REQUEST, "Overdraft withdraw must be rejected with 400 Bad Request");
}

// ============================================================================
// 3. Corp Governance GetShareholders Read-Only State Invariant
// ============================================================================

#[tokio::test]
async fn test_corp_governance_get_shareholders_read_only() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);

    let mut session = SessionState::new(3_003);
    session.user_id = Some(60);
    session.character_id = Some(90_000_001);
    let empty_corp_id = 98_000_123i64;
    session.corporation_id = Some(empty_corp_id);
    session.corp_role = eve_db::corp_roles::ROLE_DIRECTOR;

    // 1. Query GetShareholders on a corp with no registered shares
    let rep = call(&ctx, &mut session, "corpRegistry", "GetShareholders", &[]).await.expect("GetShareholders");
    if let PyRep::Rowset { rows, .. } = &rep {
        assert_eq!(rows.len(), 0, "GetShareholders on corp with no shares registered must return 0 rows");
    }

    // 2. Query GetSharesByShareholder on caller
    let sh_rep = call(&ctx, &mut session, "corpRegistry", "GetSharesByShareholder", &[PyRep::Integer(90_000_001)]).await.expect("GetSharesByShareholder");
    if let PyRep::Rowset { rows, .. } = &sh_rep {
        assert_eq!(rows.len(), 0, "GetSharesByShareholder on unissued shares must return 0 rows");
    }

    // 3. Negative exploit: caller cannot transfer fabricated shares (must return InsufficientShares)
    let move_err = call(
        &ctx, &mut session, "corpRegistry", "MovePrivateShares",
        &[PyRep::Integer(90_000_002), PyRep::Integer(500)],
    ).await.expect_err("MovePrivateShares must fail when caller has no shares");
    assert_eq!(move_err.code, "InsufficientShares", "MovePrivateShares without shares must return InsufficientShares");
}

// ============================================================================
// 4. Bounty Input Sanitization
// ============================================================================

#[tokio::test]
async fn test_bounty_nan_inf_negative_rejected() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);

    let char_id = 90_000_010i64;
    let target_id = 90_000_020i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.insert(char_id, make_char(char_id, 70, 80_000_010, 1_000_000.0));
        snap.characters.insert(target_id, make_char(target_id, 71, 80_000_020, 0.0));
    });

    let mut session = SessionState::new(3_004);
    session.user_id = Some(70);
    session.character_id = Some(char_id);

    // Negative exploit 1: NaN ISK must be rejected with BountyAmountTooLow
    let nan_err = call(&ctx, &mut session, "bountyProxy", "AddToBounty", &[PyRep::Integer(target_id), PyRep::Real(f64::NAN)]).await.expect_err("NaN");
    assert_eq!(nan_err.code, "BountyAmountTooLow", "NaN bounty placement must be rejected with BountyAmountTooLow");

    // Negative exploit 2: Infinity ISK must be rejected
    let inf_err = call(&ctx, &mut session, "bountyProxy", "AddToBounty", &[PyRep::Integer(target_id), PyRep::Real(f64::INFINITY)]).await.expect_err("Inf");
    assert_eq!(inf_err.code, "BountyAmountTooLow", "Infinity bounty placement must be rejected with BountyAmountTooLow");

    // Negative exploit 3: Negative ISK must be rejected
    let neg_err = call(&ctx, &mut session, "bountyProxy", "AddToBounty", &[PyRep::Integer(target_id), PyRep::Real(-100_000.0)]).await.expect_err("Negative");
    assert_eq!(neg_err.code, "BountyAmountTooLow", "Negative bounty placement must be rejected with BountyAmountTooLow");

    // Verify wallet balance was NOT touched
    let bal = ctx.db.with_snapshot(|s| s.characters.get(&char_id).unwrap().balance.to_wire_f64());
    assert_eq!(bal, 1_000_000.0, "Rejected bounty placements must not debit wallet");

    // Verify bounty pool remains clean (zero)
    let pool = with_combat_runtime(&ctx, |rt| rt.player_bounty_pool.get(&target_id).copied().unwrap_or(0.0));
    assert_eq!(pool, 0.0, "Bounty pool must remain clean after rejected inputs");
}

// ============================================================================
// 5. Atomic Bounty Placement, Payout & Reversal with ISK Conservation
// ============================================================================

#[tokio::test]
async fn test_atomic_bounty_transactions_and_reimbursement() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);

    let char_id = 90_000_030i64;
    let target_id = 90_000_040i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.insert(char_id, make_char(char_id, 80, 80_000_030, 500_000.0));
        snap.characters.insert(target_id, make_char(target_id, 81, 80_000_040, 0.0));
    });

    let mut session = SessionState::new(3_005);
    session.user_id = Some(80);
    session.character_id = Some(char_id);

    // 1. Valid bounty placement: 200,000 ISK
    let _ = call(&ctx, &mut session, "bountyProxy", "AddToBounty", &[PyRep::Integer(target_id), PyRep::Real(200_000.0)]).await.expect("AddToBounty");

    // Verify atomic update in DB snapshot
    ctx.db.with_snapshot(|snap| {
        let ch = snap.characters.get(&char_id).unwrap();
        assert_eq!(ch.balance.to_wire_f64(), 300_000.0, "Wallet debited atomically");
        let pool = snap.combat_crimewatch_state.player_bounty_pool.get(&target_id).copied().unwrap_or(0.0);
        assert_eq!(pool, 200_000.0, "Bounty pool credited atomically in snapshot");
    });

    // 2. Atomic claim on kill
    let killer_id = 90_000_050i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.insert(killer_id, make_char(killer_id, 82, 80_000_050, 0.0));
    });

    // Rifter type ID 587
    let claimed = claim_player_bounty_on_kill(&ctx, killer_id, target_id, 587);
    assert!(claimed > 0.0, "Bounty payout should be credited");

    // Verify killer balance and pool updated atomically
    ctx.db.with_snapshot(|snap| {
        let k = snap.characters.get(&killer_id).unwrap();
        assert_eq!(k.balance.to_wire_f64(), claimed, "Killer wallet credited atomically");
        let remaining_pool = snap.combat_crimewatch_state.player_bounty_pool.get(&target_id).copied().unwrap_or(0.0);
        assert_eq!(remaining_pool, 200_000.0 - claimed, "Pool deducted atomically");
    });

    // 3. Atomic GM reimburse with strict ISK conservation
    let mut gm_session = SessionState::new(3_006);
    gm_session.role = service_roles::ROLE_GM_DEFAULT;

    let reimbursed = gm_admin_bounty_action(&ctx, &gm_session, "reimburse", target_id, 0.0).expect("GM reimburse");
    assert_eq!(reimbursed, 200_000.0 - claimed, "Reimburse must be capped at remaining pool to strictly conserve ISK");

    // Verify pool is cleared and contributor balance is refunded in single atomic commit
    ctx.db.with_snapshot(|snap| {
        let pool = snap.combat_crimewatch_state.player_bounty_pool.get(&target_id);
        assert!(pool.is_none(), "Pool cleared after reimburse");
        let ch = snap.characters.get(&char_id).unwrap();
        assert_eq!(ch.balance.to_wire_f64(), 300_000.0 + reimbursed, "Refund applied atomically with ISK conservation");
    });
}

// ============================================================================
// 6. Deadlock Prevention in System Actor Spawn & Tick
// ============================================================================

#[tokio::test]
async fn test_no_opposite_order_lock_deadlock() {
    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);

    let sys_id = 30_000_142u32;
    let handle = ctx.systems.get_or_spawn(sys_id);

    // Run under tokio timeout to verify lock acquisition hierarchy:
    // A thread holding DB write snapshot while accessing systems.read()
    // does not deadlock with get_or_spawn querying DB before acquiring systems.write().
    let res = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let (tx1, rx1) = std::sync::mpsc::channel();
        let (tx2, rx2) = std::sync::mpsc::channel();

        let c1 = Arc::clone(&ctx);
        let h1 = handle.clone();
        let t1 = tokio::task::spawn_blocking(move || {
            c1.db.with_mut_snapshot(|_| {
                let _ = tx1.send(());
                let _ = rx2.recv_timeout(std::time::Duration::from_millis(50));
                let _ = h1.is_ball_scrambled_sync(1);
            });
        });

        let c2 = Arc::clone(&ctx);
        let t2 = tokio::spawn(async move {
            let _ = rx1.recv();
            let _ = tx2.send(());
            let _ = c2.systems.get_or_spawn(30_000_143);
        });

        let _ = tokio::join!(t1, t2);
    })
    .await;

    assert!(res.is_ok(), "Concurrent DB write and get_or_spawn must complete without lock inversion deadlock");
}

// ============================================================================
// 7. In-Space Character Reconnect Preserves Position & Combat State
// ============================================================================

#[tokio::test]
async fn test_in_space_reconnect_preserves_position_and_combat_state() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);

    let char_id = 90_000_060i64;
    let ship_id = 80_000_060i64;
    let sys_id = 30_000_142u32;
    let initial_pos = DVec3::new(150_000.0, -250_000.0, 750_000.0);

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = make_char(char_id, 90, ship_id, 10_000.0);
        ch.station_id = None;
        ch.structure_id = None;
        ch.solar_system_id = sys_id;
        snap.characters.insert(char_id, ch);
        snap.item_positions.insert(ship_id, [initial_pos.x, initial_pos.y, initial_pos.z]);
    });

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let ball = Ball {
        id: ship_id, type_id: 606, position: initial_pos, velocity: DVec3::ZERO,
        max_velocity: 300.0, inertia: 0.5, mass: 1_000_000.0, radius: 50.0,
        speed_fraction: 0.0, mode: eve_destiny::MovementMode::Stop,
    };

    let mut cs = ctx.build_ship_combat_state(char_id, ship_id, 606);
    cs.capacitor = 123.45; // Partial capacitor
    cs.warp_scramble_strength = 2; // Tackled!

    sys_handle.try_spawn_ball_with_meta(
        ball,
        cs,
        eve_destiny::SlimItemMeta {
            group_id: 25, category_id: 6, owner_id: char_id, char_id: Some(char_id),
            corp_id: 1000001, alliance_id: -1, security_status: 0.0, is_cloaked: false, ..Default::default()
        },
    );

    // Call reconnect_character_in_space_sync (triggered when player re-selects character while in space)
    let reconnected = eve_server::session_cluster_lifecycle::reconnect_character_in_space_sync(&ctx, char_id);
    assert!(reconnected.is_some());
    let (_, rec_ship_id, rec_pos) = reconnected.unwrap();
    assert_eq!(rec_ship_id, ship_id);

    // In-space character reconnect must retain live position, not reset to origin [0,0,0]
    assert_eq!(rec_pos, initial_pos, "Live position in space must be preserved upon reconnect");

    // Verify combat state was preserved (capacitor and tackle status NOT wiped)
    let current_cs = sys_handle.get_combat_state(ship_id).await.unwrap();
    assert!(current_cs.is_some(), "Combat state must exist for ship in space");
    let state = current_cs.unwrap();
    assert_eq!(state.capacitor, 123.45, "Capacitor must NOT be refilled upon in-space reconnect");
    assert_eq!(state.warp_scramble_strength, 2, "Tackle status must NOT be cleared upon in-space reconnect");
}

// ============================================================================
// 8. Weapon Aggression Paths Renew Victim PvP Logoff Timer
// ============================================================================

#[tokio::test]
async fn test_weapon_damage_renews_victim_pvp_timer() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);

    let (attacker_id, attacker_ship_id, victim_id, victim_ship_id, sys_id, now_unix) =
        setup_combat_pair(&ctx).await;
    let sys_handle = ctx.systems.get_or_spawn(sys_id);

    // --- Path A: In-flight missile impact ---
    let missile_ball_id = 80_000_090i64;
    sys_handle.spawn_ball(Ball {
        id: missile_ball_id, type_id: 266, position: DVec3::new(50.0, 0.0, 0.0), velocity: DVec3::ZERO,
        max_velocity: 1000.0, inertia: 1.0, mass: 100.0, radius: 2.0, speed_fraction: 1.0, mode: eve_destiny::MovementMode::Stop,
    }).await.unwrap();

    with_mut_combat_runtime(&ctx, |rt| {
        rt.in_flight_missiles.insert(
            missile_ball_id,
            InFlightMissileState {
                missile_ball_id, solar_system_id: sys_id, attacker_char_id: attacker_id, attacker_ship_id,
                launcher_module_id: 70_000_001, launcher_type_id: 2889, missile_type_id: 266,
                target_ball_id: victim_ship_id, max_velocity: 2000.0, flight_time_s: 10.0, remaining_flight_s: 10.0,
                hp: 10.0, max_hp: 10.0, base_damage: DamageVector { kinetic: 150.0, thermal: 50.0, em: 0.0, explosive: 0.0 },
                explosion_radius: 40.0, explosion_velocity: 250.0, drf: 1.0, is_fof: false, is_breacher_pod: false,
                breacher_dot_duration_s: 0.0, breacher_flat_cap: 0.0, breacher_pct_cap: 0.0, destroyed: false,
                expired: false, impacted: false, applied_damage: 0.0,
            },
        );
    });

    step_combat_system(&ctx, sys_id, 1.0).await;
    assert_victim_pvp_timer(&ctx, victim_id, now_unix + 900, "Missile impact must renew victim PvP timer");

    // --- Path B: Smartbomb AoE damage ---
    ctx.advance_secs(10);
    let t_sb = ctx.now_unix();
    let sb_type = ctx.sde.types.get(&3993).expect("Large EMP Smartbomb I");
    let fitted_sb = eve_dogma::FittedModule {
        item_id: 70_002,
        type_id: 3993,
        flag_id: 11,
        state: eve_dogma::ModuleState::Active,
        damage_hp: 0.0,
        max_hp: 40.0,
        charge_type_id: None,
        charge_quantity: 0,
        dynamic_attribute_overrides: std::collections::HashMap::new(),
    };
    let (_, att_ball) = sys_handle.get_ball_snapshot(attacker_ship_id).await.unwrap().unwrap();
    let _ = activate_aoe_or_defender_module(
        &ctx, attacker_id, attacker_ship_id, sys_id, 0.4, t_sb,
        70_002, &fitted_sb, sb_type, &att_ball, None, &HashMap::new(),
    ).await;
    assert_victim_pvp_timer(&ctx, victim_id, t_sb + 900, "Smartbomb AoE damage must renew victim PvP timer");

    // --- Path C: Combat drone engagement ---
    ctx.advance_secs(10);
    let t_drone = ctx.now_unix();
    let drone_ball_id = 80_000_095i64;
    sys_handle.spawn_ball(Ball {
        id: drone_ball_id, type_id: 2464, position: DVec3::new(100.0, 0.0, 0.0), velocity: DVec3::ZERO,
        max_velocity: 1000.0, inertia: 1.0, mass: 5_000.0, radius: 10.0, speed_fraction: 0.0, mode: eve_destiny::MovementMode::Stop,
    }).await.unwrap();

    with_mut_combat_runtime(&ctx, |rt| {
        rt.active_drones_in_space.insert(
            drone_ball_id,
            DroneSpaceState {
                ball_id: drone_ball_id, drone_item_id: 60_001, type_id: 2464, group_id: 100,
                owner_char_id: attacker_id, original_owner_char_id: attacker_id, parent_ship_id: attacker_ship_id,
                solar_system_id: sys_id, target_id: Some(victim_ship_id), activity_state: 0, assist_char_id: None, guard_char_id: None,
                is_abandoned: false, is_sentry: false, bandwidth_used: 5.0, max_velocity: 1000.0, optimal_range: 5000.0,
                damage_vector: DamageVector { thermal: 50.0, kinetic: 50.0, em: 0.0, explosive: 0.0 },
                shield_repair_amount: 0.0, armor_repair_amount: 0.0, hull_repair_amount: 0.0, web_speed_factor: 0.0,
                neut_amount: 0.0, ecm_strength: 0.0, tp_sig_bonus_pct: 0.0,
            },
        );
    });

    let mut att_sess = SessionState::new(3_007);
    att_sess.user_id = Some(91);
    att_sess.character_id = Some(attacker_id);
    att_sess.ship_id = Some(attacker_ship_id);
    att_sess.solar_system_id = Some(sys_id);
    att_sess.role = service_roles::ROLE_PLAYER | service_roles::ROLE_LOGIN;
    let _ = call(&ctx, &mut att_sess, "entity", "CmdEngage", &[PyRep::Integer(victim_ship_id)]).await.expect("CmdEngage");
    assert_victim_pvp_timer(&ctx, victim_id, t_drone + 900, "Drone engagement must renew victim PvP timer");

    // --- Path D: Fighter squadron ability ---
    ctx.advance_secs(10);
    let t_fighter = ctx.now_unix();
    let fighter_ball_id = 80_000_098i64;
    ctx.active_fighters.write().unwrap().insert(
        fighter_ball_id,
        ActiveFighterSquadron {
            ball_id: fighter_ball_id, item_id: 910_010, type_id: 23057, owner_char_id: attacker_id,
            parent_ship_id: attacker_ship_id, tube_index: 0, squadron_size: 9, target_id: Some(victim_ship_id),
            is_abandoned: false, mwd_active: false,
        },
    );
    sys_handle.spawn_ball(Ball {
        id: fighter_ball_id, type_id: 23057, position: DVec3::new(100.0, 0.0, 0.0), velocity: DVec3::ZERO,
        max_velocity: 1000.0, inertia: 1.0, mass: 10_000.0, radius: 20.0, speed_fraction: 0.0, mode: eve_destiny::MovementMode::Stop,
    }).await.unwrap();
    sys_handle.add_target_lock(attacker_ship_id, TargetLock {
        target_id: victim_ship_id, lock_time_ms: 100.0, remaining_lock_ms: 0.0, locked: true, max_range_m: None,
    }).await.unwrap();

    let _ = call(&ctx, &mut att_sess, "fighterMgr", "CmdActivateAbilitySlots", &[
        PyRep::List(vec![PyRep::Integer(fighter_ball_id)]), PyRep::Integer(0), PyRep::Integer(victim_ship_id),
    ]).await.expect("CmdActivateAbilitySlots");
    assert_victim_pvp_timer(&ctx, victim_id, t_fighter + 900, "Fighter ability attack must renew victim PvP timer");
}

pub(crate) mod crimewatch_states_timer_updates {
    // Integration tests for crimewatch timer notifications and drone lifecycle/control.


    use crate::common;
    use eve_db::{InventoryFlag, InventoryItemRecord};
    use eve_destiny::{Ball, DVec3, DamageVector, MovementMode};
    use eve_machonet::SessionState;
    use eve_proto::PyRep;
    use eve_server::router::ServerContext;
    use eve_server::services::combat_crimewatch::{
        with_combat_runtime, with_mut_combat_runtime, DroneSpaceState,
    };
    use std::collections::HashMap;

    fn new_ctx() -> ServerContext {
        let sde = std::sync::Arc::new(
            eve_sde::SdeRegistry::open(&eve_sde::SdeRegistry::default_db_path())
                .expect("open evomu_sde.sqlite"),
        );
        let db = std::sync::Arc::new(eve_db::WorldRepository::with_demo_seed());
        let ctx = ServerContext::with_sde_and_repository(sde, db);
        common::pin_clock(&ctx);
        ctx
    }

    fn seed_item(
        ctx: &ServerContext, item_id: i64, type_id: u32, owner_id: i64,
        location_id: i64, flag: InventoryFlag, quantity: i32, singleton: bool,
    ) {
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(item_id, InventoryItemRecord {
                item_id, type_id, owner_id, location_id, flag, quantity, singleton, ..Default::default()
            });
        });
    }

    #[tokio::test]
    async fn test_crimewatch_states_timer_updates() {
        let ctx = new_ctx();
        let char_id = 90_000_001i64;
        let now = ctx.now_unix();
        let unix_to_ft = |u: u64| -> i64 { (u as i64 + 11_644_473_600) * 10_000_000 };
        let _ = ctx.db.acquire_online_lock(1, 7111, Some(char_id), true, now);

        {
            let mut cw = ctx.crimewatch.write().unwrap();
            let st = cw.get_or_create(char_id);
            st.weapon_timer_until_unix = now + 60;
            st.pvp_timer_until_unix = now + 300;
            st.criminal_flag_until_unix = now + 900;
        }

        let _ = ctx.drain_notifications_for_char(char_id);
        eve_server::services::combat_crimewatch::crimewatch_timers::emit_crimewatch_timer_notifications(
            &ctx, char_id, now,
        );

        let notifs = ctx.drain_notifications_for_char(char_id);
        assert_eq!(notifs.len(), 6);
        assert!(notifs.iter().any(|n| n.event == "OnWeaponsTimerUpdate" && n.args == vec![PyRep::Integer(101), PyRep::Integer(unix_to_ft(now + 60))]));
        assert!(notifs.iter().any(|n| n.event == "OnPvpTimerUpdate" && n.args == vec![PyRep::Integer(201), PyRep::Integer(unix_to_ft(now + 300))]));
        assert!(notifs.iter().any(|n| n.event == "OnNpcTimerUpdate" && n.args == vec![PyRep::Integer(400), PyRep::None]));
        assert!(notifs.iter().any(|n| n.event == "OnCriminalTimerUpdate" && n.args == vec![PyRep::Integer(301), PyRep::Integer(unix_to_ft(now + 900))]));
        assert!(notifs.iter().any(|n| n.event == "OnDisapprovalTimerUpdate" && n.args == vec![PyRep::Integer(500), PyRep::None]));
        assert!(notifs.iter().any(|n| n.event == "OnSystemCriminalFlagUpdates" && n.args == vec![PyRep::List(vec![]), PyRep::List(vec![]), PyRep::List(vec![PyRep::Integer(char_id)])]));

        // Re-sweep at same timestamp must be de-duplicated (no redundant notification queueing)
        eve_server::services::combat_crimewatch::crimewatch_timers::emit_crimewatch_timer_notifications(&ctx, char_id, now);
        assert!(ctx.drain_notifications_for_char(char_id).is_empty(), "Redundant timer sweep must be de-duplicated");

        // Advance time past all timers -> active to idle transition
        let exp_now = now + 1000;
        eve_server::services::combat_crimewatch::crimewatch_timers::emit_crimewatch_timer_notifications(&ctx, char_id, exp_now);
        let exp_notifs = ctx.drain_notifications_for_char(char_id);
        assert_eq!(exp_notifs.len(), 4);
        assert!(exp_notifs.iter().any(|n| n.event == "OnWeaponsTimerUpdate" && n.args == vec![PyRep::Integer(100), PyRep::None]));
        assert!(exp_notifs.iter().any(|n| n.event == "OnPvpTimerUpdate" && n.args == vec![PyRep::Integer(200), PyRep::None]));
        assert!(exp_notifs.iter().any(|n| n.event == "OnCriminalTimerUpdate" && n.args == vec![PyRep::Integer(300), PyRep::None]));
        assert!(exp_notifs.iter().any(|n| n.event == "OnSystemCriminalFlagUpdates" && n.args == vec![PyRep::List(vec![PyRep::Integer(char_id)]), PyRep::List(vec![]), PyRep::List(vec![])]));

        // Pilot must be pruned from crimewatch_timer_states once all timers are idle
        let is_tracked = with_combat_runtime(&ctx, |rt| rt.crimewatch_timer_states.contains_key(&char_id));
        assert!(!is_tracked, "Pilot state must be pruned when all timers return to idle");

        // Subsequent sweeps for an idle pilot must NOT recreate or re-notify
        eve_server::services::combat_crimewatch::crimewatch_timers::emit_crimewatch_timer_notifications(&ctx, char_id, exp_now + 10);
        assert!(ctx.drain_notifications_for_char(char_id).is_empty());
        assert!(!with_combat_runtime(&ctx, |rt| rt.crimewatch_timer_states.contains_key(&char_id)));
    }

    #[tokio::test]
    async fn test_drones_recall_reconnect_abandon_control() {
        // parity: feat:drones.recall-drones, feat:drones.reconnect-drones, rpc:entity.CmdAbandonDrone, rpc:entity.CmdReconnectToDrones
        eve_testkit::attest(&[
            "feat:drones.recall-drones",
            "feat:drones.reconnect-drones",
            "rpc:entity.CmdAbandonDrone",
            "rpc:entity.CmdReconnectToDrones",
        ]);

        let ctx = new_ctx();
        let kw = HashMap::new();
        let mut sess = SessionState::new(7110);
        common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;

        let char_id = 90_000_001i64;
        sess.character_id = Some(char_id);
        let ship_id = 100_000_001i64;
        let sys_id = 30_000_142u32;
        let sys_handle = ctx.systems.get_or_spawn(sys_id);

        sess.solar_system_id = Some(sys_id);
        sess.station_id = None;
        sess.ship_id = Some(ship_id);
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&char_id) {
                ch.station_id = None;
                ch.solar_system_id = sys_id;
                ch.active_ship_id = ship_id;
            }
        });
        seed_item(&ctx, ship_id, 626, char_id, sys_id as i64, InventoryFlag::None, 1, true);

        let _ = sys_handle
            .spawn_ball(Ball {
                id: ship_id,
                type_id: 626,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 0.5,
                mass: 1_000_000.0,
                radius: 30.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            })
            .await;

        let d1 = 850_001i64;
        let d2 = 850_002i64;
        let d3 = 850_003i64;
        for &did in &[d1, d2, d3] {
            seed_item(&ctx, did, 2464, char_id, sys_id as i64, InventoryFlag::None, 1, true);
            let _ = sys_handle
                .spawn_ball(Ball {
                    id: did,
                    type_id: 2464,
                    position: DVec3::new(100.0, 0.0, 0.0),
                    velocity: DVec3::ZERO,
                    max_velocity: 1000.0,
                    inertia: 0.5,
                    mass: 10_000.0,
                    radius: 10.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                })
                .await;
            with_mut_combat_runtime(&ctx, |rt| {
                rt.active_drones_in_space.insert(
                    did,
                    DroneSpaceState {
                        ball_id: did,
                        drone_item_id: did,
                        type_id: 2464,
                        group_id: 100,
                        owner_char_id: char_id,
                        original_owner_char_id: char_id,
                        parent_ship_id: ship_id,
                        solar_system_id: sys_id,
                        target_id: None,
                        activity_state: 0,
                        assist_char_id: None,
                        guard_char_id: None,
                        is_abandoned: false,
                        is_sentry: false,
                        bandwidth_used: 5.0,
                        max_velocity: 1000.0,
                        optimal_range: 5000.0,
                        damage_vector: DamageVector::default(),
                        shield_repair_amount: 0.0,
                        armor_repair_amount: 0.0,
                        hull_repair_amount: 0.0,
                        web_speed_factor: 0.0,
                        neut_amount: 0.0,
                        ecm_strength: 0.0,
                        tp_sig_bonus_pct: 0.0,
                    },
                );
            });
        }

        // 1. Abandon ONLY d2
        let abandon_res = ctx
            .dispatch_rpc(
                &mut sess,
                "entity",
                None,
                "CmdAbandonDrone",
                &[PyRep::List(vec![PyRep::Integer(d2)])],
                &kw,
                None,
            )
            .await
            .expect("CmdAbandonDrone");
        assert_eq!(abandon_res, PyRep::Dict(Vec::new()));

        with_combat_runtime(&ctx, |rt| {
            assert!(!rt.active_drones_in_space.get(&d1).unwrap().is_abandoned);
            assert!(rt.active_drones_in_space.get(&d2).unwrap().is_abandoned);
            assert!(!rt.active_drones_in_space.get(&d3).unwrap().is_abandoned);
        });

        // 2. Reconnect ONLY d2
        let reconn_res = ctx
            .dispatch_rpc(
                &mut sess,
                "entity",
                None,
                "CmdReconnectToDrones",
                &[PyRep::List(vec![PyRep::Integer(d2)])],
                &kw,
                None,
            )
            .await
            .expect("CmdReconnectToDrones");
        assert_eq!(reconn_res, PyRep::Dict(vec![]));

        with_combat_runtime(&ctx, |rt| {
            assert!(!rt.active_drones_in_space.get(&d2).unwrap().is_abandoned);
        });

        // 3. Stargate jump / dock abandons drones in space
        eve_server::session_cluster_lifecycle::abandon_drones_and_fighters_for_ship_sync(&ctx, char_id, ship_id, sys_id);
        with_combat_runtime(&ctx, |rt| {
            assert!(rt.active_drones_in_space.get(&d1).unwrap().is_abandoned);
            assert!(rt.active_drones_in_space.get(&d2).unwrap().is_abandoned);
            assert!(rt.active_drones_in_space.get(&d3).unwrap().is_abandoned);
            assert_eq!(rt.active_drones_in_space.get(&d1).unwrap().original_owner_char_id, char_id);
        });

        // Reconnect d1
        let reconn_d1 = ctx
            .dispatch_rpc(
                &mut sess,
                "entity",
                None,
                "CmdReconnectToDrones",
                &[PyRep::List(vec![PyRep::Integer(d1)])],
                &kw,
                None,
            )
            .await
            .expect("CmdReconnectToDrones d1");
        assert_eq!(reconn_d1, PyRep::Dict(vec![]));
        with_combat_runtime(&ctx, |rt| {
            assert!(!rt.active_drones_in_space.get(&d1).unwrap().is_abandoned);
            assert!(rt.active_drones_in_space.get(&d2).unwrap().is_abandoned);
        });

        // 4. Return to bay
        let recall_res = ctx
            .dispatch_rpc(
                &mut sess,
                "entity",
                None,
                "CmdReturnBay",
                &[PyRep::List(vec![PyRep::Integer(d1)])],
                &kw,
                None,
            )
            .await
            .expect("CmdReturnBay");
        assert_eq!(recall_res, PyRep::Dict(vec![]));
        with_combat_runtime(&ctx, |rt| {
            assert!(!rt.active_drones_in_space.contains_key(&d1));
        });
    }

    #[tokio::test]
    async fn test_drone_mine_repeatedly_and_shipping_storage() {
        // parity: rpc:entity.CmdMineRepeatedly, rpc:entity.DisableBehaviorDebugging, rpc:entity.OpenShippingUnitStorage
        eve_testkit::attest(&[
            "rpc:entity.CmdMineRepeatedly",
            "rpc:entity.DisableBehaviorDebugging",
            "rpc:entity.OpenShippingUnitStorage",
        ]);

        let ctx = new_ctx();
        let kw = HashMap::new();
        let mut sess = SessionState::new(7111);
        common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;

        let char_id = 90_000_001i64;
        sess.character_id = Some(char_id);
        let other_char_id = 90_000_002i64;
        let ship_id = 100_000_001i64;
        let sys_id = 30_000_142u32;
        let sys_handle = ctx.systems.get_or_spawn(sys_id);

        sess.solar_system_id = Some(sys_id);
        sess.station_id = None;
        sess.ship_id = Some(ship_id);
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&char_id) {
                ch.station_id = None;
                ch.solar_system_id = sys_id;
                ch.active_ship_id = ship_id;
            }
        });
        seed_item(&ctx, ship_id, 626, char_id, sys_id as i64, InventoryFlag::None, 1, true);

        let _ = sys_handle
            .spawn_ball(Ball {
                id: ship_id,
                type_id: 626,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 0.5,
                mass: 1_000_000.0,
                radius: 30.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            })
            .await;

        // 1. Behavior debugging GM check
        let err_non_gm = ctx
            .dispatch_rpc(&mut sess, "entity", None, "EnableBehaviorDebugging", &[], &kw, None)
            .await
            .expect_err("Non-GM must fail");
        assert_eq!(err_non_gm.code, "InsufficientRole");

        sess.role |= eve_db::service_roles::ROLE_GMS;
        let ok_gm = ctx
            .dispatch_rpc(&mut sess, "entity", None, "EnableBehaviorDebugging", &[], &kw, None)
            .await
            .expect("GM can enable behavior debugging");
        assert_eq!(ok_gm.dict_get("behaviorDebuggingEnabled").and_then(|v| v.as_bool()), Some(true));

        let disable_res = ctx
            .dispatch_rpc(
                &mut sess,
                "entity",
                None,
                "DisableBehaviorDebugging",
                &[PyRep::Integer(char_id)],
                &kw,
                None,
            )
            .await
            .expect("GM can disable behavior debugging");
        assert_eq!(disable_res.dict_get("behaviorDebuggingEnabled").and_then(|v| v.as_bool()), Some(false));

        // 2. Shipping storage
        let unit_id = 860_001i64;
        let cargo_item = 860_002i64;
        seed_item(&ctx, unit_id, 24445, other_char_id, sys_id as i64, InventoryFlag::None, 1, true);
        seed_item(&ctx, cargo_item, 34, other_char_id, unit_id, InventoryFlag::Cargo, 100, false);

        let _ = sys_handle
            .spawn_ball(Ball {
                id: unit_id,
                type_id: 24445,
                position: DVec3::new(2000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 1.0,
                mass: 50_000.0,
                radius: 20.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            })
            .await;

        sess.role = 0;
        let access_err = ctx
            .dispatch_rpc(
                &mut sess,
                "entity",
                None,
                "OpenShippingUnitStorage",
                &[PyRep::Integer(unit_id)],
                &kw,
                None,
            )
            .await
            .expect_err("Unauthorized caller denied");
        assert_eq!(access_err.code, "AccessDenied");

        sess.role |= eve_db::service_roles::ROLE_GMS;
        let open_res = ctx
            .dispatch_rpc(
                &mut sess,
                "entity",
                None,
                "OpenShippingUnitStorage",
                &[PyRep::Integer(unit_id)],
                &kw,
                None,
            )
            .await
            .expect("Authorized open");
        assert_eq!(open_res.dict_get("storageOpened").and_then(|v| v.as_bool()), Some(true));

        let bind = ctx.dispatch_rpc(
            &mut sess, "invbroker", None, "MachoBindObject",
            &[PyRep::Tuple(vec![PyRep::Integer(sys_id as i64), PyRep::Integer(15)]), PyRep::None], &kw, None,
        ).await.expect("bind invbroker");
        let mgr = match bind {
            PyRep::Tuple(parts) => parts[0].as_i64().unwrap() as u64,
            PyRep::Integer(i) => i as u64,
            _ => panic!("unexpected bind rep"),
        };
        let inv_rep = ctx.dispatch_rpc(
            &mut sess, "invbroker", Some(mgr), "GetInventoryFromId",
            &[PyRep::Integer(unit_id), PyRep::Integer(0)], &kw, None,
        ).await.expect("GetInventoryFromId");
        let inv_bid = inv_rep.as_i64().expect("inv bound id") as u64;
        let unit_items = ctx.dispatch_rpc(&mut sess, "invbroker", Some(inv_bid), "List", &[], &kw, None)
            .await.expect("List shipping unit storage");
        assert!(matches!(unit_items, PyRep::ObjectEx { .. } | PyRep::List(_)));

        // 3. CmdMineRepeatedly
        let miner_drone_id = 860_010i64;
        let asteroid_id = 860_020i64;
        seed_item(&ctx, miner_drone_id, 10250, char_id, sys_id as i64, InventoryFlag::None, 1, true);
        with_mut_combat_runtime(&ctx, |rt| {
            rt.active_drones_in_space.insert(
                miner_drone_id,
                DroneSpaceState {
                    ball_id: miner_drone_id,
                    drone_item_id: miner_drone_id,
                    type_id: 10250,
                    group_id: 101,
                    owner_char_id: char_id,
                    original_owner_char_id: char_id,
                    parent_ship_id: ship_id,
                    solar_system_id: sys_id,
                    target_id: None,
                    activity_state: 0,
                    assist_char_id: None,
                    guard_char_id: None,
                    is_abandoned: false,
                    is_sentry: false,
                    bandwidth_used: 5.0,
                    max_velocity: 1000.0,
                    optimal_range: 5000.0,
                    damage_vector: DamageVector::default(),
                    shield_repair_amount: 0.0,
                    armor_repair_amount: 0.0,
                    hull_repair_amount: 0.0,
                    web_speed_factor: 0.0,
                    neut_amount: 0.0,
                    ecm_strength: 0.0,
                    tp_sig_bonus_pct: 0.0,
                },
            );
        });

        let _ = sys_handle
            .spawn_ball(Ball {
                id: miner_drone_id,
                type_id: 10250,
                position: DVec3::new(100.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 1000.0,
                inertia: 0.5,
                mass: 10_000.0,
                radius: 10.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            })
            .await;

        ctx.db.spawn_asteroid(eve_db::AsteroidRecord {
            asteroid_id,
            solar_system_id: sys_id,
            belt_id: 40_000_001,
            type_id: 1230,
            quantity: 100_000,
            initial_quantity: 100_000,
            position: (5000.0, 0.0, 0.0),
            radius: 100.0,
        });
        let _ = sys_handle
            .spawn_asteroid(
                eve_server::system_actor::AsteroidState {
                    asteroid_id,
                    belt_id: 40_000_001,
                    type_id: 1230,
                    initial_units: 100_000,
                    remaining_units: 100_000,
                    unit_volume: 0.1,
                    depleted: false,
                    respawn_at_tick: None,
                },
                Ball {
                    id: asteroid_id,
                    type_id: 1230,
                    position: DVec3::new(5000.0, 0.0, 0.0),
                    velocity: DVec3::ZERO,
                    max_velocity: 0.0,
                    inertia: 1.0,
                    mass: 1_000_000.0,
                    radius: 100.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
            )
            .await;

        let alien_drone_id = 860_011i64;
        seed_item(&ctx, alien_drone_id, 2464, other_char_id, sys_id as i64, InventoryFlag::None, 1, true);
        with_mut_combat_runtime(&ctx, |rt| {
            rt.active_drones_in_space.insert(
                alien_drone_id,
                DroneSpaceState {
                    ball_id: alien_drone_id,
                    drone_item_id: alien_drone_id,
                    type_id: 2464,
                    group_id: 100,
                    owner_char_id: other_char_id,
                    original_owner_char_id: other_char_id,
                    parent_ship_id: ship_id,
                    solar_system_id: sys_id,
                    target_id: None,
                    activity_state: 0,
                    assist_char_id: None,
                    guard_char_id: None,
                    is_abandoned: false,
                    is_sentry: false,
                    bandwidth_used: 5.0,
                    max_velocity: 1000.0,
                    optimal_range: 5000.0,
                    damage_vector: DamageVector::default(),
                    shield_repair_amount: 0.0,
                    armor_repair_amount: 0.0,
                    hull_repair_amount: 0.0,
                    web_speed_factor: 0.0,
                    neut_amount: 0.0,
                    ecm_strength: 0.0,
                    tp_sig_bonus_pct: 0.0,
                },
            );
        });

        let alien_err = ctx
            .dispatch_rpc(
                &mut sess,
                "entity",
                None,
                "CmdMineRepeatedly",
                &[PyRep::List(vec![PyRep::Integer(alien_drone_id)]), PyRep::Integer(asteroid_id)],
                &kw,
                None,
            )
            .await
            .expect_err("Non-owned drone rejected");
        assert_eq!(alien_err.code, "InvalidDroneOwner");

        let mine_res = ctx
            .dispatch_rpc(
                &mut sess,
                "entity",
                None,
                "CmdMineRepeatedly",
                &[PyRep::List(vec![PyRep::Integer(miner_drone_id)]), PyRep::Integer(asteroid_id)],
                &kw,
                None,
            )
            .await
            .expect("CmdMineRepeatedly");
        assert_eq!(mine_res, PyRep::Dict(vec![]));

        let (initial_ast_qty, next_tick) = with_combat_runtime(&ctx, |rt| {
            let miner_st = rt.active_drone_miners.get(&miner_drone_id).unwrap();
            assert_eq!(miner_st.asteroid_id, asteroid_id);
            assert_eq!(rt.active_drones_in_space.get(&miner_drone_id).unwrap().activity_state, 2);
            (
                ctx.db.with_snapshot(|s| s.asteroids.get(&asteroid_id).map(|a| a.quantity)).unwrap_or(0),
                miner_st.next_cycle_unix,
            )
        });

        // Step repeating cycle 1
        eve_server::session_cluster_lifecycle::step_repeating_drone_mining(&ctx, next_tick).await;
        let after_cycle1_qty = ctx.db.with_snapshot(|s| s.asteroids.get(&asteroid_id).map(|a| a.quantity)).unwrap_or(0);
        assert!(after_cycle1_qty < initial_ast_qty, "Asteroid quantity must decrement in snapshot");
        assert!(!ctx.db.get_mining_ledger(char_id).is_empty(), "Mining ledger must record mined ore");

        // Step repeating cycle 2
        let next_tick_2 = with_combat_runtime(&ctx, |rt| {
            rt.active_drone_miners.get(&miner_drone_id).map(|m| m.next_cycle_unix)
        }).unwrap_or(0);
        assert!(next_tick_2 > next_tick);
        eve_server::session_cluster_lifecycle::step_repeating_drone_mining(&ctx, next_tick_2).await;
        let after_cycle2_qty = ctx.db.with_snapshot(|s| s.asteroids.get(&asteroid_id).map(|a| a.quantity)).unwrap_or(0);
        assert!(after_cycle2_qty < after_cycle1_qty);
    }

    #[tokio::test]
    async fn test_drone_mine_repeatedly_duplicate_ids_dedup_and_reject_active() {
        let ctx = new_ctx();
        let kw = HashMap::new();
        let mut sess = SessionState::new(7119);
        common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;

        let char_id = 90_000_001i64;
        sess.character_id = Some(char_id);
        let ship_id = 100_000_001i64;
        let sys_id = 30_000_142u32;
        let sys_handle = ctx.systems.get_or_spawn(sys_id);
        sess.solar_system_id = Some(sys_id);
        sess.ship_id = Some(ship_id);
        seed_item(&ctx, ship_id, 626, char_id, sys_id as i64, InventoryFlag::None, 1, true);

        let _ = sys_handle.spawn_ball(Ball {
            id: ship_id, type_id: 626, position: DVec3::ZERO, velocity: DVec3::ZERO,
            max_velocity: 300.0, inertia: 0.5, mass: 1_000_000.0, radius: 30.0, speed_fraction: 0.0, mode: MovementMode::Stop,
        }).await;

        let drone_id = 860_050i64;
        let ast_id = 860_060i64;
        seed_item(&ctx, drone_id, 10250, char_id, sys_id as i64, InventoryFlag::None, 1, true);
        with_mut_combat_runtime(&ctx, |rt| {
            rt.active_drones_in_space.insert(drone_id, DroneSpaceState {
                ball_id: drone_id, drone_item_id: drone_id, type_id: 10250, group_id: 101,
                owner_char_id: char_id, original_owner_char_id: char_id, parent_ship_id: ship_id,
                solar_system_id: sys_id, target_id: None, activity_state: 0, assist_char_id: None,
                guard_char_id: None, is_abandoned: false, is_sentry: false, bandwidth_used: 5.0,
                max_velocity: 1000.0, optimal_range: 5000.0, damage_vector: DamageVector::default(),
                shield_repair_amount: 0.0, armor_repair_amount: 0.0, hull_repair_amount: 0.0,
                web_speed_factor: 0.0, neut_amount: 0.0, ecm_strength: 0.0, tp_sig_bonus_pct: 0.0,
            });
        });

        let _ = sys_handle.spawn_ball(Ball {
            id: drone_id, type_id: 10250, position: DVec3::new(100.0, 0.0, 0.0), velocity: DVec3::ZERO,
            max_velocity: 1000.0, inertia: 0.5, mass: 10_000.0, radius: 10.0, speed_fraction: 0.0, mode: MovementMode::Stop,
        }).await;

        ctx.db.spawn_asteroid(eve_db::AsteroidRecord {
            asteroid_id: ast_id, solar_system_id: sys_id, belt_id: 40_000_001, type_id: 1230,
            quantity: 50_000, initial_quantity: 50_000, position: (3000.0, 0.0, 0.0), radius: 50.0,
        });
        let _ = sys_handle.spawn_asteroid(
            eve_server::system_actor::AsteroidState {
                asteroid_id: ast_id, belt_id: 40_000_001, type_id: 1230, initial_units: 50_000,
                remaining_units: 50_000, unit_volume: 0.1, depleted: false, respawn_at_tick: None,
            },
            Ball {
                id: ast_id, type_id: 1230, position: DVec3::new(3000.0, 0.0, 0.0), velocity: DVec3::ZERO,
                max_velocity: 0.0, inertia: 1.0, mass: 1_000_000.0, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop,
            },
        ).await;

        // Call with duplicate IDs [drone_id, drone_id, drone_id]
        let res = ctx.dispatch_rpc(
            &mut sess, "entity", None, "CmdMineRepeatedly",
            &[PyRep::List(vec![PyRep::Integer(drone_id), PyRep::Integer(drone_id), PyRep::Integer(drone_id)]), PyRep::Integer(ast_id)],
            &kw, None,
        ).await.expect("CmdMineRepeatedly with duplicate IDs");
        assert_eq!(res, PyRep::Dict(vec![]));

        assert_eq!(with_combat_runtime(&ctx, |rt| rt.active_drone_miners.len()), 1, "Duplicate drone IDs must be de-duplicated to 1 miner");

        // Calling again while already mining must fail
        let err = ctx.dispatch_rpc(
            &mut sess, "entity", None, "CmdMineRepeatedly",
            &[PyRep::List(vec![PyRep::Integer(drone_id)]), PyRep::Integer(ast_id)],
            &kw, None,
        ).await.expect_err("Already mining drone must be rejected");
        assert_eq!(err.code, "DroneAlreadyMining");
    }
}

pub(crate) mod drones_return_bay_prevalidation_tick {
    // Integration tests for drone return-to-bay scoop tick / prevalidation (`BF-3`) and shipping storage proximity (`BF-4`).


    use crate::common;
    use eve_db::{InventoryFlag, InventoryItemRecord};
    use eve_destiny::{Ball, DVec3, DamageVector, MovementMode};
    use eve_machonet::SessionState;
    use eve_proto::PyRep;
    use eve_server::router::ServerContext;
    use eve_server::services::combat_crimewatch::{with_combat_runtime, with_mut_combat_runtime, DroneSpaceState};
    use std::collections::HashMap;

    fn new_ctx() -> ServerContext {
        let sde = std::sync::Arc::new(
            eve_sde::SdeRegistry::open(&eve_sde::SdeRegistry::default_db_path())
                .expect("open evomu_sde.sqlite"),
        );
        let db = std::sync::Arc::new(eve_db::WorldRepository::with_demo_seed());
        let ctx = ServerContext::with_sde_and_repository(sde, db);
        common::pin_clock(&ctx);
        ctx
    }

    fn seed_item(
        ctx: &ServerContext, item_id: i64, type_id: u32, owner_id: i64,
        location_id: i64, flag: InventoryFlag, quantity: i32, singleton: bool,
    ) {
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                item_id,
                InventoryItemRecord {
                    item_id, type_id, owner_id, location_id, flag, quantity, singleton, ..Default::default()
                },
            );
        });
    }

    #[tokio::test]
    async fn test_drones_return_bay_prevalidation_and_tick_scoop() {
        // parity: rpc:entity.CmdReturnBay
        eve_testkit::attest(&["rpc:entity.CmdReturnBay"]);

        let ctx = new_ctx();
        let kw = HashMap::new();
        let mut sess = SessionState::new(7115);
        common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;

        let char_id = 90_000_001i64;
        sess.character_id = Some(char_id);
        let ship_id = 100_000_001i64;
        let sys_id = 30_000_142u32;
        sess.solar_system_id = Some(sys_id);
        sess.ship_id = Some(ship_id);
        sess.station_id = None;
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&char_id) {
                ch.station_id = None;
                ch.structure_id = None;
                ch.solar_system_id = sys_id;
                ch.active_ship_id = ship_id;
            }
        });
        let _ = ctx.db.acquire_online_lock(1, sess.session_id, Some(char_id), true, ctx.now_unix());

        seed_item(&ctx, ship_id, 626, char_id, sys_id as i64, InventoryFlag::None, 1, true); // Vexor
        let sys_handle = ctx.systems.get_or_spawn(sys_id);
        let _ = sys_handle
            .spawn_ball(Ball {
                id: ship_id,
                type_id: 626,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 195.0,
                inertia: 0.56,
                mass: 10_000_000.0,
                radius: 145.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            })
            .await;

        // Drone 1: mobile drone close (1,000 m)
        let mobile_close = 891_001i64;
        seed_item(&ctx, mobile_close, 2464, char_id, sys_id as i64, InventoryFlag::None, 1, true);
        let _ = sys_handle
            .spawn_ball(Ball {
                id: mobile_close,
                type_id: 2464,
                position: DVec3::new(1000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 1000.0,
                inertia: 0.5,
                mass: 10_000.0,
                radius: 10.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            })
            .await;

        // Drone 2: sentry drone far away (10,000 m)
        let sentry_far = 891_002i64;
        seed_item(&ctx, sentry_far, 28213, char_id, sys_id as i64, InventoryFlag::None, 1, true);
        let _ = sys_handle
            .spawn_ball(Ball {
                id: sentry_far,
                type_id: 28213,
                position: DVec3::new(10_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 1.0,
                mass: 50_000.0,
                radius: 20.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            })
            .await;

        with_mut_combat_runtime(&ctx, |rt| {
            rt.active_drones_in_space.insert(
                mobile_close,
                DroneSpaceState {
                    ball_id: mobile_close,
                    drone_item_id: mobile_close,
                    type_id: 2464,
                    group_id: 100,
                    owner_char_id: char_id,
                    original_owner_char_id: char_id,
                    parent_ship_id: ship_id,
                    solar_system_id: sys_id,
                    target_id: None,
                    activity_state: 0,
                    assist_char_id: None,
                    guard_char_id: None,
                    is_abandoned: false,
                    is_sentry: false,
                    bandwidth_used: 5.0,
                    max_velocity: 1000.0,
                    optimal_range: 5000.0,
                    damage_vector: DamageVector::default(),
                    shield_repair_amount: 0.0,
                    armor_repair_amount: 0.0,
                    hull_repair_amount: 0.0,
                    web_speed_factor: 0.0,
                    neut_amount: 0.0,
                    ecm_strength: 0.0,
                    tp_sig_bonus_pct: 0.0,
                },
            );
            rt.active_drones_in_space.insert(
                sentry_far,
                DroneSpaceState {
                    ball_id: sentry_far,
                    drone_item_id: sentry_far,
                    type_id: 28213,
                    group_id: 100,
                    owner_char_id: char_id,
                    original_owner_char_id: char_id,
                    parent_ship_id: ship_id,
                    solar_system_id: sys_id,
                    target_id: None,
                    activity_state: 0,
                    assist_char_id: None,
                    guard_char_id: None,
                    is_abandoned: false,
                    is_sentry: true,
                    bandwidth_used: 25.0,
                    max_velocity: 0.0,
                    optimal_range: 50000.0,
                    damage_vector: DamageVector::default(),
                    shield_repair_amount: 0.0,
                    armor_repair_amount: 0.0,
                    hull_repair_amount: 0.0,
                    web_speed_factor: 0.0,
                    neut_amount: 0.0,
                    ecm_strength: 0.0,
                    tp_sig_bonus_pct: 0.0,
                },
            );
        });

        // 1. Pre-validation: Calling CmdReturnBay with both drones must fail because sentry is out of range,
        // and mobile drone must NOT be scooped!
        let pre_err = ctx
            .dispatch_rpc(
                &mut sess,
                "entity",
                None,
                "CmdReturnBay",
                &[PyRep::List(vec![PyRep::Integer(mobile_close), PyRep::Integer(sentry_far)])],
                &kw,
                None,
            )
            .await
            .expect_err("Mixed sentry request must fail prevalidation");
        assert_eq!(pre_err.code, "SentryDroneOutOfScoopRange");

        // Both drones must still be in space
        with_combat_runtime(&ctx, |rt| {
            assert!(rt.active_drones_in_space.contains_key(&mobile_close), "Mobile drone must not be scooped on error");
            assert!(rt.active_drones_in_space.contains_key(&sentry_far), "Sentry drone must not be scooped");
        });
        ctx.db.with_snapshot(|snap| {
            assert_ne!(snap.items.get(&mobile_close).unwrap().flag, InventoryFlag::DroneBay);
        });

        // 2. Returning mobile drone from > 2,500 m
        // Move mobile drone to 6,000 m
        let _ = sys_handle
            .spawn_ball(Ball {
                id: mobile_close,
                type_id: 2464,
                position: DVec3::new(6000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 1000.0,
                inertia: 0.5,
                mass: 10_000.0,
                radius: 10.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            })
            .await;

        let _ = ctx.drain_notifications_for_char(char_id);
        let ret_res = ctx
            .dispatch_rpc(
                &mut sess,
                "entity",
                None,
                "CmdReturnBay",
                &[PyRep::List(vec![PyRep::Integer(mobile_close)])],
                &kw,
                None,
            )
            .await
            .expect("Mobile drone returns");
        // Not immediately scooped -> empty dict in RPC return
        assert_eq!(ret_res, PyRep::Dict(vec![]));

        // Drone state must be returning (activity_state = 4)
        with_combat_runtime(&ctx, |rt| {
            assert_eq!(rt.active_drones_in_space.get(&mobile_close).unwrap().activity_state, 4);
        });
        let notifs = ctx.drain_notifications_for_char(char_id);
        assert!(notifs.iter().any(|n| n.event == "OnDroneStateChange"));

        // 3. Cluster tick scoops returning drone when within 2,500 m
        // Move drone ball within 2,000 m
        let _ = sys_handle
            .spawn_ball(Ball {
                id: mobile_close,
                type_id: 2464,
                position: DVec3::new(2000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 1000.0,
                inertia: 0.5,
                mass: 10_000.0,
                radius: 10.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            })
            .await;

        eve_server::session_cluster_lifecycle::step_returning_drones(&ctx).await;

        // Drone must now be scooped into bay
        with_combat_runtime(&ctx, |rt| {
            assert!(!rt.active_drones_in_space.contains_key(&mobile_close), "Returning drone must be scooped on tick");
        });
        ctx.db.with_snapshot(|snap| {
            assert_eq!(snap.items.get(&mobile_close).unwrap().flag, InventoryFlag::DroneBay);
            assert_eq!(snap.items.get(&mobile_close).unwrap().location_id, ship_id);
        });

        let tick_notifs = ctx.drain_notifications_for_char(char_id);
        assert!(tick_notifs.iter().any(|n| n.event == "OnDroneStateChange"));
        assert!(tick_notifs.iter().any(|n| n.event == "OnItemChange"));

        // 4. Drones left behind on warp stay owned and cannot be stolen by third parties (R-DRN-01)
        let warp_drone = 892_020i64;
        seed_item(&ctx, warp_drone, 2464, char_id, sys_id as i64, InventoryFlag::None, 1, true);
        with_mut_combat_runtime(&ctx, |rt| {
            rt.active_drones_in_space.insert(warp_drone, DroneSpaceState {
                ball_id: warp_drone, drone_item_id: warp_drone, type_id: 2464, group_id: 100,
                owner_char_id: char_id, original_owner_char_id: char_id, parent_ship_id: ship_id,
                solar_system_id: sys_id, target_id: None, activity_state: 1, assist_char_id: None,
                guard_char_id: None, is_abandoned: false, is_sentry: false, bandwidth_used: 5.0,
                max_velocity: 1000.0, optimal_range: 5000.0, damage_vector: DamageVector::default(),
                shield_repair_amount: 0.0, armor_repair_amount: 0.0, hull_repair_amount: 0.0,
                web_speed_factor: 0.0, neut_amount: 0.0, ecm_strength: 0.0, tp_sig_bonus_pct: 0.0,
            });
        });
        let _ = sys_handle.spawn_ball(Ball {
            id: warp_drone, type_id: 2464, position: DVec3::new(100.0, 0.0, 0.0), velocity: DVec3::ZERO,
            max_velocity: 1000.0, inertia: 0.5, mass: 10_000.0, radius: 10.0, speed_fraction: 0.0, mode: MovementMode::Stop,
        }).await;

        eve_server::session_cluster_lifecycle::disconnect_drones_for_ship_on_warp_sync(&ctx, char_id, ship_id, sys_id);

        with_combat_runtime(&ctx, |rt| {
            let d = rt.active_drones_in_space.get(&warp_drone).expect("drone still in space");
            assert!(!d.is_abandoned, "Drone must NOT be abandoned on warp");
            assert_eq!(d.owner_char_id, char_id, "Drone owner must be preserved");
            assert_eq!(d.parent_ship_id, 0, "Drone parent ship must be cleared");
        });

        let third_char = 90_000_003i64;
        let third_ship = 100_000_003i64;
        let mut third_sess = SessionState::new(7129);
        common::login_pilot(&ctx, &mut third_sess, "other", "other-secret").await;
        third_sess.character_id = Some(third_char);
        third_sess.ship_id = Some(third_ship);
        third_sess.solar_system_id = Some(sys_id);
        seed_item(&ctx, third_ship, 587, third_char, sys_id as i64, InventoryFlag::None, 1, true);
        let _ = sys_handle.spawn_ball(Ball {
            id: third_ship, type_id: 587, position: DVec3::new(100.0, 0.0, 0.0), velocity: DVec3::ZERO,
            max_velocity: 365.0, inertia: 3.2, mass: 1_250_000.0, radius: 35.0, speed_fraction: 0.0, mode: MovementMode::Stop,
        }).await;

        let scoop_err = ctx.dispatch_rpc(
            &mut third_sess, "ship", None, "ScoopToDroneBay", &[PyRep::Integer(warp_drone)], &kw, None,
        ).await.expect_err("Third party scoop must be rejected");
        assert_eq!(scoop_err.code, "DroneOwnedByOther");

        // 5. Owner can reconnect to warp-disconnected drones (R-DRN-02)
        let rec_res = ctx.dispatch_rpc(
            &mut sess, "entity", None, "CmdReconnectToDrones", &[], &kw, None,
        ).await.expect("CmdReconnectToDrones by owner must succeed");
        assert!(matches!(rec_res, PyRep::Dict(_)));
        with_combat_runtime(&ctx, |rt| {
            let d = rt.active_drones_in_space.get(&warp_drone).expect("drone reconnected");
            assert_eq!(d.parent_ship_id, ship_id, "Drone must be reconnected to ship");
        });

        // 6. Owner can recall drones to drone bay (R-DRN-02)
        let ret_res = ctx.dispatch_rpc(
            &mut sess, "entity", None, "CmdReturnBay", &[PyRep::List(vec![PyRep::Integer(warp_drone)])], &kw, None,
        ).await.expect("CmdReturnBay by owner must succeed");
        assert!(matches!(ret_res, PyRep::Dict(_)));
        ctx.db.with_snapshot(|snap| {
            let it = snap.items.get(&warp_drone).expect("drone item in DB");
            assert_eq!(it.location_id, ship_id);
            assert_eq!(it.flag, InventoryFlag::DroneBay);
        });

        // Recalling a disconnected drone directly (parent_ship_id == 0) also succeeds (R-DRN-02)
        let warp_drone2 = 892_021i64;
        seed_item(&ctx, warp_drone2, 2464, char_id, sys_id as i64, InventoryFlag::None, 1, true);
        with_mut_combat_runtime(&ctx, |rt| {
            rt.active_drones_in_space.insert(warp_drone2, DroneSpaceState {
                ball_id: warp_drone2, drone_item_id: warp_drone2, type_id: 2464, group_id: 100,
                owner_char_id: char_id, original_owner_char_id: char_id, parent_ship_id: 0,
                solar_system_id: sys_id, target_id: None, activity_state: 0, assist_char_id: None,
                guard_char_id: None, is_abandoned: false, is_sentry: false, bandwidth_used: 5.0,
                max_velocity: 1000.0, optimal_range: 5000.0, damage_vector: DamageVector::default(),
                shield_repair_amount: 0.0, armor_repair_amount: 0.0, hull_repair_amount: 0.0,
                web_speed_factor: 0.0, neut_amount: 0.0, ecm_strength: 0.0, tp_sig_bonus_pct: 0.0,
            });
        });
        let _ = sys_handle.spawn_ball(Ball {
            id: warp_drone2, type_id: 2464, position: DVec3::new(50.0, 0.0, 0.0), velocity: DVec3::ZERO,
            max_velocity: 1000.0, inertia: 0.5, mass: 10_000.0, radius: 10.0, speed_fraction: 0.0, mode: MovementMode::Stop,
        }).await;
        let ret_res2 = ctx.dispatch_rpc(
            &mut sess, "entity", None, "CmdReturnBay", &[PyRep::List(vec![PyRep::Integer(warp_drone2)])], &kw, None,
        ).await.expect("CmdReturnBay for disconnected drone must succeed");
        assert!(matches!(ret_res2, PyRep::Dict(_)));
        ctx.db.with_snapshot(|snap| {
            let it = snap.items.get(&warp_drone2).expect("drone2 item in DB");
            assert_eq!(it.location_id, ship_id);
            assert_eq!(it.flag, InventoryFlag::DroneBay);
        });
    }

    #[tokio::test]
    async fn test_shipping_storage_proximity_and_fleetmate_transfer() {
        // parity: rpc:entity.OpenShippingUnitStorage
        eve_testkit::attest(&["rpc:entity.OpenShippingUnitStorage"]);

        let ctx = new_ctx();
        let kw = HashMap::new();
        let mut sess = SessionState::new(7116);
        common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;

        let char_id = 90_000_001i64;
        sess.character_id = Some(char_id);
        let ship_id = 100_000_001i64;
        let sys_id = 30_000_142u32;
        sess.solar_system_id = Some(sys_id);
        sess.ship_id = Some(ship_id);
        sess.station_id = None;
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&char_id) {
                ch.station_id = None;
                ch.structure_id = None;
                ch.solar_system_id = sys_id;
                ch.active_ship_id = ship_id;
            }
        });
        let _ = ctx.db.acquire_online_lock(1, sess.session_id, Some(char_id), true, ctx.now_unix());

        seed_item(&ctx, ship_id, 587, char_id, sys_id as i64, InventoryFlag::None, 1, true);
        let sys_handle = ctx.systems.get_or_spawn(sys_id);
        let _ = sys_handle
            .spawn_ball(Ball {
                id: ship_id,
                type_id: 587,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 365.0,
                inertia: 3.2,
                mass: 1_250_000.0,
                radius: 35.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            })
            .await;

        // 1. OpenShippingUnitStorage on a ship (587) must fail with InvalidTarget
        let other_char_id = 90_000_002i64;
        let ship_target = 892_001i64;
        seed_item(&ctx, ship_target, 587, other_char_id, sys_id as i64, InventoryFlag::None, 1, true);
        let _ = sys_handle
            .spawn_ball(Ball {
                id: ship_target,
                type_id: 587,
                position: DVec3::new(1000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 365.0,
                inertia: 3.2,
                mass: 1_250_000.0,
                radius: 35.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            })
            .await;

        let target_err = ctx
            .dispatch_rpc(
                &mut sess,
                "entity",
                None,
                "OpenShippingUnitStorage",
                &[PyRep::Integer(ship_target)],
                &kw,
                None,
            )
            .await
            .expect_err("Calling OpenShippingUnitStorage on ship must fail");
        assert_eq!(target_err.code, "InvalidTarget");

        // 2. Open container (Giant Freight Container, type 24445) at 2,000 m
        let container_id = 892_002i64;
        seed_item(&ctx, container_id, 24445, other_char_id, sys_id as i64, InventoryFlag::None, 1, true);
        let _ = sys_handle
            .spawn_ball(Ball {
                id: container_id,
                type_id: 24445,
                position: DVec3::new(2000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 1.0,
                mass: 50_000.0,
                radius: 20.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            })
            .await;

        // Put them in the same fleet so caller is authorized
        ctx.db.with_mut_snapshot(|snap| {
            snap.fleets.insert(
                5001,
                eve_db::FleetRecord {
                    fleet_id: 5001,
                    boss_id: other_char_id,
                    members: vec![char_id, other_char_id],
                    ..Default::default()
                },
            );
        });

        let open_ok = ctx
            .dispatch_rpc(
                &mut sess,
                "entity",
                None,
                "OpenShippingUnitStorage",
                &[PyRep::Integer(container_id)],
                &kw,
                None,
            )
            .await
            .expect("Fleetmate open container succeeds");
        assert_eq!(open_ok.dict_get("storageOpened").and_then(|v| v.as_bool()), Some(true));

        // Bind invbroker
        let bind = ctx.dispatch_rpc(
            &mut sess, "invbroker", None, "MachoBindObject",
            &[PyRep::Tuple(vec![PyRep::Integer(sys_id as i64), PyRep::Integer(15)]), PyRep::None], &kw, None,
        ).await.expect("bind invbroker");
        let mgr = match bind {
            PyRep::Tuple(parts) => parts[0].as_i64().unwrap() as u64,
            PyRep::Integer(i) => i as u64,
            _ => panic!("unexpected bind rep"),
        };

        // Access within 2,000 m succeeds
        let inv_ok = ctx.dispatch_rpc(
            &mut sess, "invbroker", Some(mgr), "GetInventoryFromId",
            &[PyRep::Integer(container_id), PyRep::Integer(0)], &kw, None,
        ).await.expect("GetInventoryFromId in range succeeds");
        assert!(inv_ok.as_i64().is_some());

        // 3. Move caller > 5,000 m away (to 10,000 m): GetInventoryFromId must fail
        let _ = sys_handle.spawn_ball(Ball {
            id: ship_id, type_id: 587, position: DVec3::new(10_000.0, 0.0, 0.0), velocity: DVec3::ZERO,
            max_velocity: 365.0, inertia: 3.2, mass: 1_250_000.0, radius: 35.0, speed_fraction: 0.0, mode: MovementMode::Stop,
        }).await;

        let range_err = ctx.dispatch_rpc(
            &mut sess, "invbroker", Some(mgr), "GetInventoryFromId",
            &[PyRep::Integer(container_id), PyRep::Integer(0)], &kw, None,
        ).await.expect_err("Out of range inventory access must fail");
        assert_eq!(range_err.code, "InventoryNotAccessible");

        // 4. Move ship back in range (2,000 m)
        let _ = sys_handle
            .spawn_ball(Ball {
                id: ship_id,
                type_id: 587,
                position: DVec3::new(2000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 365.0,
                inertia: 3.2,
                mass: 1_250_000.0,
                radius: 35.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            })
            .await;

        // Seed item in caller ship cargo
        let test_item_id = 892_005i64;
        seed_item(&ctx, test_item_id, 34, char_id, ship_id, InventoryFlag::Cargo, 100, false);

        // Moving item into container while in fleet succeeds
        let add_to_container = ctx
            .dispatch_rpc(
                &mut sess,
                "invbroker",
                Some(mgr),
                "Add",
                &[PyRep::Integer(test_item_id), PyRep::Integer(container_id), PyRep::Integer(0), PyRep::Integer(100)],
                &kw,
                None,
            )
            .await
            .expect("Moving item into shipping unit while in fleet must succeed");
        assert!(matches!(add_to_container, PyRep::KeyVal(_) | PyRep::Dict(_) | PyRep::None));
        ctx.db.with_snapshot(|snap| {
            let it = snap.items.get(&test_item_id).expect("item in DB");
            assert_eq!(it.location_id, container_id);
        });

        // Moving item out of container back to ship while in fleet succeeds
        let take_from_container = ctx
            .dispatch_rpc(
                &mut sess,
                "invbroker",
                Some(mgr),
                "Add",
                &[PyRep::Integer(test_item_id), PyRep::Integer(ship_id), PyRep::Integer(5), PyRep::Integer(100)],
                &kw,
                None,
            )
            .await
            .expect("Taking item from shipping unit while in fleet must succeed");
        assert!(matches!(take_from_container, PyRep::KeyVal(_) | PyRep::Dict(_) | PyRep::None));
        ctx.db.with_snapshot(|snap| {
            let it = snap.items.get(&test_item_id).expect("item in DB");
            assert_eq!(it.location_id, ship_id);
        });

        // Move back into container for leave-fleet test
        let _ = ctx
            .dispatch_rpc(
                &mut sess,
                "invbroker",
                Some(mgr),
                "Add",
                &[PyRep::Integer(test_item_id), PyRep::Integer(container_id), PyRep::Integer(0), PyRep::Integer(100)],
                &kw,
                None,
            )
            .await
            .expect("Put item back into container");

        // NF-DEADLOCK-ALSC: Fleetmate take from group-448 ALSC in space must not deadlock and logs action
        let alsc_id = 892_010i64;
        let alsc_item = 892_011i64;
        seed_item(&ctx, alsc_id, 17365, other_char_id, sys_id as i64, InventoryFlag::None, 1, true);
        seed_item(&ctx, alsc_item, 34, other_char_id, alsc_id, InventoryFlag::None, 25, false);
        let _ = sys_handle.spawn_ball(Ball {
            id: alsc_id, type_id: 17365, position: DVec3::new(1500.0, 0.0, 0.0), velocity: DVec3::ZERO,
            max_velocity: 0.0, inertia: 1.0, mass: 1_000_000.0, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop,
        }).await;
        let _ = ctx.dispatch_rpc(&mut sess, "entity", None, "OpenShippingUnitStorage", &[PyRep::Integer(alsc_id)], &kw, None).await.expect("open ALSC");
        let alsc_take = ctx.dispatch_rpc(
            &mut sess, "invbroker", Some(mgr), "Add",
            &[PyRep::Integer(alsc_item), PyRep::Integer(ship_id), PyRep::Integer(5), PyRep::Integer(25)], &kw, None,
        ).await.expect("Fleetmate take from ALSC must succeed without deadlock");
        assert!(matches!(alsc_take, PyRep::KeyVal(_) | PyRep::Dict(_) | PyRep::None));
        let log_res = ctx
            .dispatch_rpc(&mut sess, "invbroker", Some(mgr), "ALSCLogGet", &[PyRep::Integer(alsc_id)], &kw, None)
            .await
            .expect("ALSCLogGet");
        let Some(PyRep::List(entries)) = log_res.dict_get("entries") else { panic!("expected entries list"); };
        assert!(entries.iter().any(|e| e.dict_get("action").and_then(|v| v.as_str()) == Some("ALSCActionTake") && e.dict_get("actorID").and_then(|v| v.as_i64()) == Some(char_id)));

        // NF-FLEET-SHIP-CARGO-THEFT: Fleetmate cannot steal cargo from fleetmate ship (flag 5)
        let alt_cargo_item = 892_012i64;
        seed_item(&ctx, alt_cargo_item, 34, other_char_id, ship_target, InventoryFlag::Cargo, 10, false);
        let theft_err = ctx.dispatch_rpc(
            &mut sess, "invbroker", Some(mgr), "Add",
            &[PyRep::Integer(alt_cargo_item), PyRep::Integer(ship_id), PyRep::Integer(5), PyRep::Integer(10)], &kw, None,
        ).await.expect_err("Taking cargo from fleetmate ship cargo must fail");
        assert_eq!(theft_err.code, "ItemOwnershipViolation");

        // 5. Caller leaves the fleet: authorization must be revoked
        sess.fleet_id = Some(5001);
        let _ = ctx.dispatch_rpc(&mut sess, "fleetMgr", None, "LeaveFleet", &[], &kw, None).await.expect("Leave fleet");

        // Attempting to take item out of container after leaving fleet must be rejected
        let take_err = ctx.dispatch_rpc(
            &mut sess, "invbroker", Some(mgr), "Add",
            &[PyRep::Integer(test_item_id), PyRep::Integer(ship_id), PyRep::Integer(5), PyRep::Integer(100)], &kw, None,
        ).await.expect_err("Taking item from container after leaving fleet must fail");
        assert_eq!(take_err.code, "ItemOwnershipViolation");

        // Attempting to add item into container after leaving fleet must be rejected
        let seed_item2 = 892_006i64;
        seed_item(&ctx, seed_item2, 34, char_id, ship_id, InventoryFlag::Cargo, 50, false);
        let add_err = ctx.dispatch_rpc(
            &mut sess, "invbroker", Some(mgr), "Add",
            &[PyRep::Integer(seed_item2), PyRep::Integer(container_id), PyRep::Integer(0), PyRep::Integer(50)], &kw, None,
        ).await.expect_err("Adding item into container after leaving fleet must fail");
        assert_eq!(add_err.code, "DestinationAccessDenied");

        // Binding inventory after leaving fleet must also fail
        let inv_leave_err = ctx.dispatch_rpc(
            &mut sess, "invbroker", Some(mgr), "GetInventoryFromId",
            &[PyRep::Integer(container_id), PyRep::Integer(0)], &kw, None,
        ).await.expect_err("GetInventoryFromId after leaving fleet must fail");
        assert_eq!(inv_leave_err.code, "InventoryNotAccessible");
    }
}

pub(crate) mod fitting_saved_multifit_community_fittings {
    // Integration tests for saved fitting legality, multifit, community fittings, and weapon linking.


    use crate::common;
    use eve_db::{InventoryFlag, InventoryItemRecord};
    use eve_destiny::{Ball, MovementMode};
    use eve_machonet::SessionState;
    use eve_proto::PyRep;
    use eve_server::router::ServerContext;
    use glam::DVec3;
    use std::collections::HashMap;

    fn new_ctx() -> ServerContext {
        let sde = std::sync::Arc::new(
            eve_sde::SdeRegistry::open(&eve_sde::SdeRegistry::default_db_path())
                .expect("open evomu_sde.sqlite"),
        );
        let db = std::sync::Arc::new(eve_db::WorldRepository::with_demo_seed());
        let ctx = ServerContext::with_sde_and_repository(sde, db);
        common::pin_clock(&ctx);
        ctx
    }

    fn seed_item(
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
                    ..Default::default()
                },
            );
        });
    }

    fn mk_fit(fid: i64, name: &str, ship_type: u32, owner: i64, items: Vec<(u32, InventoryFlag, i32)>) -> eve_db::SavedFittingRecord {
        eve_db::SavedFittingRecord { fitting_id: fid, name: name.into(), ship_type_id: ship_type, description: name.into(), owner_id: owner, items }
    }

    #[tokio::test]
    async fn test_fitting_saved_multifit_and_community_fittings() {
        // parity: feat:fitting.fit-from-saved, rpc:corpFittingMgr.GetCommunityFittings
        eve_testkit::attest(&[
            "feat:fitting.fit-from-saved",
            "rpc:corpFittingMgr.GetCommunityFittings",
        ]);

        let ctx = new_ctx();
        let kw: HashMap<String, PyRep> = HashMap::new();
        let mut sess = SessionState::new(7112);
        common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;

        let char_id = 90_000_001i64;
        sess.character_id = Some(char_id);
        let _ = ctx.db.acquire_online_lock(1, sess.session_id, Some(char_id), true, ctx.now_unix());
        let station_id = 60_003_760i64;
        sess.station_id = Some(station_id);

        // 1. Curated community fittings
        ctx.db.with_mut_snapshot(|snap| {
            snap.saved_fittings.insert(
                101,
                eve_db::SavedFittingRecord {
                    fitting_id: 101,
                    name: "Community Starter Rifter".into(),
                    ship_type_id: 587,
                    description: "Curated community fit".into(),
                    owner_id: 1, // Curated community
                    items: vec![(486, InventoryFlag::HiSlot0, 1)],
                },
            );
            snap.saved_fittings.insert(
                102,
                eve_db::SavedFittingRecord {
                    fitting_id: 102,
                    name: "Corp Secret Fit".into(),
                    ship_type_id: 587,
                    description: "Corp fit".into(),
                    owner_id: sess.corporation_id.unwrap_or(1000001),
                    items: vec![(486, InventoryFlag::HiSlot0, 1)],
                },
            );
        });

        let comm_fittings = ctx
            .dispatch_rpc(&mut sess, "corpFittingMgr", None, "GetCommunityFittings", &[], &kw, None)
            .await
            .expect("GetCommunityFittings");

        let comm_str = format!("{comm_fittings:?}");
        assert!(comm_str.contains("Community Starter Rifter"));
        assert!(!comm_str.contains("Corp Secret Fit"), "Corp fittings must not appear in community store");

        // 2. Saved fitting: Rifter setup
        let fit_id = 201i64;
        let gun = 486u32; // 200mm AutoCannon I
        ctx.db.with_mut_snapshot(|snap| {
            snap.saved_fittings.insert(
                fit_id,
                eve_db::SavedFittingRecord {
                    fitting_id: fit_id,
                    name: "Dual Gun Rifter".into(),
                    ship_type_id: 587,
                    description: "Dual autocannons".into(),
                    owner_id: char_id,
                    items: vec![
                        (gun, InventoryFlag::HiSlot0, 1),
                        (gun, InventoryFlag::HiSlot1, 1),
                    ],
                },
            );
        });

        // Hull type mismatch error
        let merlin_id = 870_001i64;
        seed_item(&ctx, merlin_id, 603, char_id, station_id, InventoryFlag::Hangar, 1, true); // 603 = Merlin
        let mismatch_err = ctx
            .dispatch_rpc(
                &mut sess,
                "ship",
                None,
                "FitShips",
                &[PyRep::List(vec![PyRep::Integer(merlin_id)]), PyRep::Integer(fit_id)],
                &kw,
                None,
            )
            .await
            .expect_err("Hull type mismatch must fail");
        assert_eq!(mismatch_err.code, "UserError");
        assert!(mismatch_err.message.contains("ShipTypeMismatch"));

        // Hangar quantity summed across multiple stacks & multifit assembling packaged hulls
        let clean_hull1 = 870_011i64;
        let clean_hull2 = 870_012i64;
        seed_item(&ctx, clean_hull1, 587, char_id, station_id, InventoryFlag::Hangar, 1, false); // packaged
        seed_item(&ctx, clean_hull2, 587, char_id, station_id, InventoryFlag::Hangar, 1, false); // packaged

        // Seed 4 guns spread across 2 stacks in hangar
        let stack1 = 870_021i64;
        let stack2 = 870_022i64;
        seed_item(&ctx, stack1, gun, char_id, station_id, InventoryFlag::Hangar, 2, false);
        seed_item(&ctx, stack2, gun, char_id, station_id, InventoryFlag::Hangar, 2, false);

        let _ = ctx.drain_notifications_for_char(char_id);
        let mfit_res = ctx
            .dispatch_rpc(
                &mut sess,
                "ship",
                None,
                "FitShips",
                &[
                    PyRep::List(vec![PyRep::Integer(clean_hull1), PyRep::Integer(clean_hull2)]),
                    PyRep::Integer(fit_id),
                ],
                &kw,
                None,
            )
            .await
            .expect("Multifit must succeed");
        assert_eq!(mfit_res.dict_get("fittedCount").and_then(|v| v.as_i64()), Some(2));

        // Both hulls must be assembled
        ctx.db.with_snapshot(|snap| {
            assert!(snap.items.get(&clean_hull1).unwrap().singleton);
            assert!(snap.items.get(&clean_hull2).unwrap().singleton);
        });

        // Notifications emitted: canonical OnMultipleItemChange with old location/flag
        let notifs = ctx.drain_notifications_for_char(char_id);
        let multi_notif = notifs.iter().find(|n| n.event == "OnMultipleItemChange").expect("OnMultipleItemChange emitted");
        let PyRep::List(ref moved_list) = multi_notif.args[0] else { panic!("expected items list"); };
        assert!(!moved_list.is_empty());
        for it in moved_list {
            assert_eq!(it.dict_get("ownerID").and_then(|v| v.as_i64()), Some(char_id));
        }
        let PyRep::Dict(ref change_dict) = multi_notif.args[1] else { panic!("expected change dict"); };
        assert!(change_dict.iter().any(|(k, v)| k.as_i64() == Some(3) && v.as_i64() == Some(station_id)));
        assert!(change_dict.iter().any(|(k, v)| k.as_i64() == Some(4) && v.as_i64() == Some(eve_db::InventoryFlag::Hangar as i64)));

        // Duplicate ship ID in target_ship_ids must be deduplicated: fittedCount == 1 and stock consumed once (NF-FIT-CLAIM)
        let dup_hull = 870_041i64;
        seed_item(&ctx, dup_hull, 587, char_id, station_id, InventoryFlag::Hangar, 1, false);
        let dup_mod = 870_042i64;
        seed_item(&ctx, dup_mod, gun, char_id, station_id, InventoryFlag::Hangar, 1, false);
        let dup_res = ctx
            .dispatch_rpc(
                &mut sess, "ship", None, "FitShips",
                &[PyRep::List(vec![PyRep::Integer(dup_hull), PyRep::Integer(dup_hull)]), PyRep::Integer(fit_id)],
                &kw, None,
            ).await.expect("Duplicate ship ID FitShips succeeds");
        assert_eq!(dup_res.dict_get("fittedCount").and_then(|v| v.as_i64()), Some(1));
        ctx.db.with_snapshot(|snap| {
            let remaining_stock = snap.items.values().filter(|i| i.location_id == station_id && i.type_id == gun).map(|i| i.quantity).sum::<i32>();
            assert_eq!(remaining_stock, 0, "Stock must be consumed only once for deduplicated ship ID");
        });

        // Occupied slot rejection
        let occupied_hull = 870_031i64;
        seed_item(&ctx, occupied_hull, 587, char_id, station_id, InventoryFlag::Hangar, 1, true);
        let existing_mod = 870_032i64;
        seed_item(&ctx, existing_mod, gun, char_id, occupied_hull, InventoryFlag::HiSlot0, 1, true);
        let occ_err = ctx
            .dispatch_rpc(
                &mut sess,
                "ship",
                None,
                "FitShips",
                &[PyRep::List(vec![PyRep::Integer(occupied_hull)]), PyRep::Integer(fit_id)],
                &kw,
                None,
            )
            .await
            .expect_err("Occupied slot must fail");
        assert_eq!(occ_err.code, "UserError");
        assert!(occ_err.message.contains("SlotAlreadyOccupied"));

        // Exceeding powergrid leaves module fitted offline
        let big_gun_fit_id = 202i64;
        let big_gun = 491u32; // 1400mm Howitzer Artillery (140 PG > 41 PG on Rifter)
        ctx.db.with_mut_snapshot(|snap| {
            snap.saved_fittings.insert(
                big_gun_fit_id,
                eve_db::SavedFittingRecord {
                    fitting_id: big_gun_fit_id,
                    name: "Overpower Rifter".into(),
                    ship_type_id: 587,
                    description: "Overpower".into(),
                    owner_id: char_id,
                    items: vec![(big_gun, InventoryFlag::HiSlot0, 1)],
                },
            );
        });
        let over_hull = 870_041i64;
        let big_gun_item = 870_042i64;
        seed_item(&ctx, over_hull, 587, char_id, station_id, InventoryFlag::Hangar, 1, false);
        seed_item(&ctx, big_gun_item, big_gun, char_id, station_id, InventoryFlag::Hangar, 1, false);

        let over_res = ctx
            .dispatch_rpc(
                &mut sess,
                "ship",
                None,
                "FitShips",
                &[PyRep::List(vec![PyRep::Integer(over_hull)]), PyRep::Integer(big_gun_fit_id)],
                &kw,
                None,
            )
            .await
            .expect("FitShips with power overflow must succeed fitting offline");
        assert_eq!(over_res.dict_get("fittedCount").and_then(|v| v.as_i64()), Some(1));
        ctx.db.with_snapshot(|snap| {
            assert_eq!(snap.module_online_states.get(&big_gun_item), Some(&false), "Exceeding powergrid module must be offline");
        });

        // Exceeding hull slot counts rejection
        let invalid_slot_fit_id = 203i64;
        ctx.db.with_mut_snapshot(|snap| {
            snap.saved_fittings.insert(
                invalid_slot_fit_id,
                eve_db::SavedFittingRecord {
                    fitting_id: invalid_slot_fit_id,
                    name: "Too Many Slots".into(),
                    ship_type_id: 587,
                    description: "HiSlot7".into(),
                    owner_id: char_id,
                    items: vec![(gun, InventoryFlag::HiSlot7, 1)],
                },
            );
        });
        let slot_hull = 870_051i64;
        let slot_gun = 870_052i64;
        seed_item(&ctx, slot_hull, 587, char_id, station_id, InventoryFlag::Hangar, 1, false);
        seed_item(&ctx, slot_gun, gun, char_id, station_id, InventoryFlag::Hangar, 1, false);
        let slot_err = ctx
            .dispatch_rpc(
                &mut sess,
                "ship",
                None,
                "FitShips",
                &[PyRep::List(vec![PyRep::Integer(slot_hull)]), PyRep::Integer(invalid_slot_fit_id)],
                &kw,
                None,
            )
            .await
            .expect_err("Exceeding slot count must fail");
        assert_eq!(slot_err.code, "UserError");
        assert!(slot_err.message.contains("SlotIndexExceedsHullSlots"));
    }

    #[tokio::test]
    async fn test_fitting_saved_sde_slot_effects_hardpoints_and_capacity_validation() {
        // parity: feat:fitting.fit-from-saved
        eve_testkit::attest(&["feat:fitting.fit-from-saved"]);

        let ctx = new_ctx();
        let kw: HashMap<String, PyRep> = HashMap::new();
        let mut sess = SessionState::new(7114);
        common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;

        let char_id = 90_000_001i64;
        sess.character_id = Some(char_id);
        let _ = ctx.db.acquire_online_lock(1, sess.session_id, Some(char_id), true, ctx.now_unix());
        let station_id = 60_003_760i64;
        sess.station_id = Some(station_id);

        let test_hull = 890_001i64;
        seed_item(&ctx, test_hull, 587, char_id, station_id, InventoryFlag::Hangar, 1, false);

        // 1. Slot effect mismatch: med shield extender (3831, medPower 13) in low slot (flag 11)
        let bad_slot_fit = 301i64;
        ctx.db.with_mut_snapshot(|s| { s.saved_fittings.insert(bad_slot_fit, mk_fit(bad_slot_fit, "Bad Slot", 587, char_id, vec![(3831, InventoryFlag::LoSlot0, 1)])); });
        let err = ctx.dispatch_rpc(&mut sess, "ship", None, "FitShips", &[PyRep::List(vec![PyRep::Integer(test_hull)]), PyRep::Integer(bad_slot_fit)], &kw, None).await.expect_err("Slot effect mismatch");
        assert_eq!(err.code, "UserError");
        assert!(err.message.contains("SlotEffectMismatch"));

        // 2. Rig size mismatch: Medium rig (4395, rigSize 2.0) on Small Rifter (rigSize 1.0)
        let bad_rig_fit = 302i64;
        ctx.db.with_mut_snapshot(|s| { s.saved_fittings.insert(bad_rig_fit, mk_fit(bad_rig_fit, "Bad Rig Size", 587, char_id, vec![(4395, InventoryFlag::RigSlot0, 1)])); });
        let err = ctx.dispatch_rpc(&mut sess, "ship", None, "FitShips", &[PyRep::List(vec![PyRep::Integer(test_hull)]), PyRep::Integer(bad_rig_fit)], &kw, None).await.expect_err("Rig size mismatch");
        assert_eq!(err.code, "UserError");
        assert!(err.message.contains("RigSizeMismatch"));

        // 3. Rig calibration budget exceeded: 2x Drone Damage Rig II (26927, 300 cal each = 600 > 400 hull upgradeCapacity)
        let over_cal_fit = 303i64;
        ctx.db.with_mut_snapshot(|s| { s.saved_fittings.insert(over_cal_fit, mk_fit(over_cal_fit, "Over Cal", 587, char_id, vec![(26927, InventoryFlag::RigSlot0, 1), (26927, InventoryFlag::RigSlot1, 1)])); });
        let err = ctx.dispatch_rpc(&mut sess, "ship", None, "FitShips", &[PyRep::List(vec![PyRep::Integer(test_hull)]), PyRep::Integer(over_cal_fit)], &kw, None).await.expect_err("Over calibration");
        assert_eq!(err.code, "UserError");
        assert!(err.message.contains("UpgradeCapacityExceeded"));

        // 4. Turret hardpoints limit exceeded: 3x 200mm AutoCannon I (486) on Probe (2 turret slots, 3 hi slots)
        let probe_hull = 890_002i64;
        seed_item(&ctx, probe_hull, 586, char_id, station_id, InventoryFlag::Hangar, 1, false);
        let over_turret_fit = 304i64;
        ctx.db.with_mut_snapshot(|s| { s.saved_fittings.insert(over_turret_fit, mk_fit(over_turret_fit, "Over Turrets", 586, char_id, vec![(486, InventoryFlag::HiSlot0, 1), (486, InventoryFlag::HiSlot1, 1), (486, InventoryFlag::HiSlot2, 1)])); });
        let err = ctx.dispatch_rpc(&mut sess, "ship", None, "FitShips", &[PyRep::List(vec![PyRep::Integer(probe_hull)]), PyRep::Integer(over_turret_fit)], &kw, None).await.expect_err("Over turrets");
        assert_eq!(err.code, "UserError");
        assert!(err.message.contains("InsufficientTurretHardpoints"));

        // 5. Launcher hardpoints limit exceeded: 3x Light Missile Launcher I (499) on Rifter (2 launcher slots)
        let over_launcher_fit = 305i64;
        ctx.db.with_mut_snapshot(|s| { s.saved_fittings.insert(over_launcher_fit, mk_fit(over_launcher_fit, "Over Launchers", 587, char_id, vec![(499, InventoryFlag::HiSlot0, 1), (499, InventoryFlag::HiSlot1, 1), (499, InventoryFlag::HiSlot2, 1)])); });
        let err = ctx.dispatch_rpc(&mut sess, "ship", None, "FitShips", &[PyRep::List(vec![PyRep::Integer(test_hull)]), PyRep::Integer(over_launcher_fit)], &kw, None).await.expect_err("Over launchers");
        assert_eq!(err.code, "UserError");
        assert!(err.message.contains("InsufficientLauncherHardpoints"));

        // 6. MaxGroupFitted exceeded: 2x Damage Control II (2048, maxGroupFitted 1544 = 1)
        let over_dc_fit = 306i64;
        ctx.db.with_mut_snapshot(|s| { s.saved_fittings.insert(over_dc_fit, mk_fit(over_dc_fit, "Dual DC", 587, char_id, vec![(2048, InventoryFlag::LoSlot0, 1), (2048, InventoryFlag::LoSlot1, 1)])); });
        let err = ctx.dispatch_rpc(&mut sess, "ship", None, "FitShips", &[PyRep::List(vec![PyRep::Integer(test_hull)]), PyRep::Integer(over_dc_fit)], &kw, None).await.expect_err("Dual DC");
        assert_eq!(err.code, "UserError");
        assert!(err.message.contains("MaxGroupFittedExceeded"));

        // 7. Cargo capacity exceeded: 500 units of item 34 (1m3 each = 500m3 > 140m3 Rifter cargo)
        let over_cargo_fit = 307i64;
        ctx.db.with_mut_snapshot(|s| { s.saved_fittings.insert(over_cargo_fit, mk_fit(over_cargo_fit, "Over Cargo", 587, char_id, vec![(34, InventoryFlag::Cargo, 50_000)])); });
        let err = ctx.dispatch_rpc(&mut sess, "ship", None, "FitShips", &[PyRep::List(vec![PyRep::Integer(test_hull)]), PyRep::Integer(over_cargo_fit)], &kw, None).await.expect_err("Over cargo");
        assert_eq!(err.code, "CapacityExceeded");

        // 8. Drone bay category check: non-drone item (ammunition 34) in DroneBay (flag 87)
        let bad_drone_cat_fit = 308i64;
        ctx.db.with_mut_snapshot(|s| { s.saved_fittings.insert(bad_drone_cat_fit, mk_fit(bad_drone_cat_fit, "Ammo in Drone Bay", 587, char_id, vec![(34, InventoryFlag::DroneBay, 5)])); });
        let err = ctx.dispatch_rpc(&mut sess, "ship", None, "FitShips", &[PyRep::List(vec![PyRep::Integer(test_hull)]), PyRep::Integer(bad_drone_cat_fit)], &kw, None).await.expect_err("Ammo in drone bay");
        assert_eq!(err.code, "ItemNotADrone");

        // 9. Drone bay capacity exceeded: Rifter has droneCapacity == 0
        let over_drone_fit = 309i64;
        ctx.db.with_mut_snapshot(|s| { s.saved_fittings.insert(over_drone_fit, mk_fit(over_drone_fit, "Drones on Rifter", 587, char_id, vec![(2464, InventoryFlag::DroneBay, 1)])); });
        let err = ctx.dispatch_rpc(&mut sess, "ship", None, "FitShips", &[PyRep::List(vec![PyRep::Integer(test_hull)]), PyRep::Integer(over_drone_fit)], &kw, None).await.expect_err("Drones on Rifter");
        assert_eq!(err.code, "CapacityExceeded");
    }

    #[tokio::test]
    async fn test_dogma_link_all_weapons_selective_grouping() {
        // parity: rpc:dogmaIM.LinkAllWeapons
        eve_testkit::attest(&["rpc:dogmaIM.LinkAllWeapons"]);

        let ctx = new_ctx();
        let kw: HashMap<String, PyRep> = HashMap::new();
        let mut sess = SessionState::new(7113);
        common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;

        let char_id = 90_000_001i64;
        sess.character_id = Some(char_id);
        let ship_id = 100_000_001i64;
        seed_item(&ctx, ship_id, 587, char_id, 60_003_760, InventoryFlag::Hangar, 1, true);

        // Fit 2x 200mm AutoCannon II (2889), 2x Light Missile Launcher I (499), 1x Small Energy Neutralizer I (533)
        let ac1 = 880_001i64;
        let ac2 = 880_002i64;
        let lml1 = 880_003i64;
        let lml2 = 880_004i64;
        let neut1 = 880_005i64;

        seed_item(&ctx, ac1, 2889, char_id, ship_id, InventoryFlag::HiSlot0, 1, true);
        seed_item(&ctx, ac2, 2889, char_id, ship_id, InventoryFlag::HiSlot1, 1, true);
        seed_item(&ctx, lml1, 499, char_id, ship_id, InventoryFlag::HiSlot2, 1, true);
        seed_item(&ctx, lml2, 499, char_id, ship_id, InventoryFlag::HiSlot3, 1, true);
        seed_item(&ctx, neut1, 533, char_id, ship_id, InventoryFlag::HiSlot4, 1, true);

        let link_res = ctx
            .dispatch_rpc(
                &mut sess,
                "dogmaIM",
                None,
                "LinkAllWeapons",
                &[PyRep::Integer(ship_id)],
                &kw,
                None,
            )
            .await
            .expect("LinkAllWeapons");

        let PyRep::Dict(banks_dict) = link_res else {
            panic!("LinkAllWeapons must return dict, got {link_res:?}");
        };

        // Exactly 2 banks formed: AutoCannons and Launchers. Neut is not grouped.
        assert_eq!(banks_dict.len(), 2, "Only 2 banks should be created");

        let masters: Vec<i64> = banks_dict.iter().filter_map(|(k, _)| k.as_i64()).collect();
        assert!(masters.contains(&ac1) || masters.contains(&ac2));
        assert!(masters.contains(&lml1) || masters.contains(&lml2));
        assert!(!masters.contains(&neut1), "Neutralizer must not be grouped");

        ctx.db.with_snapshot(|snap| {
            let banks = snap.weapon_banks.get(&ship_id).expect("Persisted weapon banks");
            assert_eq!(banks.len(), 2);
        });
    }

    #[tokio::test]
    async fn test_fitships_fitdata_flag_whitelist_and_specialized_holds() {
        let ctx = new_ctx();
        let mut sess = SessionState::new(7128);
        common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;

        let char_id = 90_000_001i64;
        sess.character_id = Some(char_id);
        let station_id = 60_003_760i64;
        sess.station_id = Some(station_id);
        sess.solar_system_id = Some(30_000_142);
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&char_id) {
                ch.station_id = Some(station_id);
                ch.structure_id = None;
                ch.solar_system_id = 30_000_142;
            }
        });

        let ship_id = 870_050i64; // Venture (type 32880, has Ore Hold flag 134 capacity 5,000 m3)
        seed_item(&ctx, ship_id, 32880, char_id, station_id, InventoryFlag::Hangar, 1, true);

        // Helper to build fitData kw
        let fit_kw = |flg: i64, tid: i64, qty: i64| {
            let mut kw = HashMap::new();
            kw.insert("fitData".to_string(), PyRep::List(vec![
                PyRep::Tuple(vec![PyRep::Integer(tid), PyRep::Integer(flg), PyRep::Integer(qty)]),
            ]));
            kw
        };

        // 1. Disallowed flag in fitData (flag 4 = Hangar) must fail with InvalidFittingFlag
        let err_flag = ctx.dispatch_rpc(&mut sess, "ship", None, "FitShips", &[PyRep::List(vec![PyRep::Integer(ship_id)])], &fit_kw(4, 2889, 1), None).await.expect_err("flag 4 must fail");
        assert_eq!(err_flag.code, "InvalidFittingFlag");

        // 2. Disallowed flag (flag 7) must fail with InvalidFittingFlag
        let err_flag7 = ctx.dispatch_rpc(&mut sess, "ship", None, "FitShips", &[PyRep::List(vec![PyRep::Integer(ship_id)])], &fit_kw(7, 2889, 1), None).await.expect_err("flag 7 must fail");
        assert_eq!(err_flag7.code, "InvalidFittingFlag");

        // 3. Incompatible item into specialized ore hold (flag 134 with module 2889) must fail with InvalidItemTypeForHold
        let err_type = ctx.dispatch_rpc(&mut sess, "ship", None, "FitShips", &[PyRep::List(vec![PyRep::Integer(ship_id)])], &fit_kw(134, 2889, 1), None).await.expect_err("Module in ore hold must fail");
        assert_eq!(err_type.code, "InvalidItemTypeForHold");

        // 4. Volume exceeding specialized hold capacity (ore volume 0.1 m3, qty 100,000 = 10,000 m3 > 5,000 m3 hold)
        let ore_tid = 1230; // Veldspar (category 25)
        let err_cap = ctx.dispatch_rpc(&mut sess, "ship", None, "FitShips", &[PyRep::List(vec![PyRep::Integer(ship_id)])], &fit_kw(134, ore_tid as i64, 100_000), None).await.expect_err("Excess ore volume must fail");
        assert_eq!(err_cap.code, "CapacityExceeded");

        // 5. Compatible specialized hold item fitting within capacity succeeds
        seed_item(&ctx, 870_051, ore_tid, char_id, station_id, InventoryFlag::Hangar, 1000, false);
        let ok_res = ctx.dispatch_rpc(&mut sess, "ship", None, "FitShips", &[PyRep::List(vec![PyRep::Integer(ship_id)])], &fit_kw(134, ore_tid as i64, 1000), None).await.expect("Fit ore within capacity");
        assert_eq!(ok_res.dict_get("fittedCount").and_then(|v| v.as_i64()), Some(1));

        // 6. Tritanium (category 4) into flag 158 (FighterBay) must fail (NF-FIT-FLAG-BYPASS)
        let err_fighter = ctx.dispatch_rpc(&mut sess, "ship", None, "FitShips", &[PyRep::List(vec![PyRep::Integer(ship_id)])], &fit_kw(158, 34, 1), None).await.expect_err("Tritanium into FighterBay must fail");
        assert!(err_fighter.code == "ShipHasNoFighterBay" || err_fighter.code == "ItemNotAFighter");

        // 7. Structure service slot (flag 164) in fitData must fail with InvalidFittingFlag
        let err_service = ctx.dispatch_rpc(&mut sess, "ship", None, "FitShips", &[PyRep::List(vec![PyRep::Integer(ship_id)])], &fit_kw(164, 34, 1), None).await.expect_err("Structure service slot must fail");
        assert_eq!(err_service.code, "InvalidFittingFlag");
    }

    #[tokio::test]
    async fn test_structure_fuel_bay_remote_proximity_and_fuel_type() {
        let ctx = new_ctx();
        let kw: HashMap<String, PyRep> = HashMap::new();
        let mut sess = SessionState::new(7135);
        common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;

        let char_id = 90_000_001i64;
        sess.character_id = Some(char_id);
        let sys_id = 30_000_142u32;
        sess.solar_system_id = Some(sys_id);
        sess.station_id = None;
        sess.structure_id = None;

        let ship_id = 870_060i64;
        sess.ship_id = Some(ship_id);
        seed_item(&ctx, ship_id, 587, char_id, sys_id as i64, InventoryFlag::None, 1, true);

        let struct_id = 1_000_000_000_050i64;
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&char_id) {
                ch.station_id = None;
                ch.structure_id = None;
                ch.solar_system_id = sys_id;
                ch.active_ship_id = ship_id;
            }
            snap.structures.insert(struct_id, eve_db::UpwellStructureRecord {
                structure_id: struct_id, type_id: 35832, owner_corp_id: 2001, solar_system_id: sys_id,
                name: "Citadel".into(), position: [0.0, 0.0, 0.0], state: "ONLINE".into(),
                quantum_core_installed: true, fuel_blocks: 0, shield_hp: 100.0, armor_hp: 100.0,
                hull_hp: 100.0, reinforced_until_unix: 0, last_damage_tick: 0, last_damage_unix: 0, self_destruct_at_unix: None,
            });
        });

        let sys_handle = ctx.systems.get_or_spawn(sys_id);
        let _ = sys_handle.spawn_ball(Ball {
            id: struct_id, type_id: 35832, position: DVec3::ZERO, velocity: DVec3::ZERO,
            max_velocity: 0.0, inertia: 1.0, mass: 10_000_000.0, radius: 500.0, speed_fraction: 0.0, mode: MovementMode::Stop,
        }).await;
        // Ship spawned at 10,000m (out of 5,000m range)
        let _ = sys_handle.spawn_ball(Ball {
            id: ship_id, type_id: 587, position: DVec3::new(10_000.0, 0.0, 0.0), velocity: DVec3::ZERO,
            max_velocity: 365.0, inertia: 3.2, mass: 1_250_000.0, radius: 35.0, speed_fraction: 0.0, mode: MovementMode::Stop,
        }).await;

        let fuel_block = 870_061i64;
        seed_item(&ctx, fuel_block, 4051, char_id, ship_id, InventoryFlag::Cargo, 10, false);
        let non_fuel = 870_062i64;
        seed_item(&ctx, non_fuel, 34, char_id, ship_id, InventoryFlag::Cargo, 100, false);

        // 1. Moving fuel into structure fuel bay while out of range (> 5,000m) must fail (NF-FUELBAY-REMOTE)
        let err_range = ctx.dispatch_rpc(
            &mut sess, "invbroker", None, "Add",
            &[PyRep::Integer(fuel_block), PyRep::Integer(struct_id), PyRep::Integer(133), PyRep::Integer(10)],
            &kw, None,
        ).await.expect_err("Out of range fuel move must fail");
        assert_eq!(err_range.code, "DestinationAccessDenied");

        // 2. Move ship into range (2,000m <= 5,000m)
        let _ = sys_handle.spawn_ball(Ball {
            id: ship_id, type_id: 587, position: DVec3::new(2000.0, 0.0, 0.0), velocity: DVec3::ZERO,
            max_velocity: 365.0, inertia: 3.2, mass: 1_250_000.0, radius: 35.0, speed_fraction: 0.0, mode: MovementMode::Stop,
        }).await;

        // 3. Moving non-fuel into structure fuel bay must fail with InvalidInventoryFlag (NF-FUELBAY-REMOTE)
        let err_type = ctx.dispatch_rpc(
            &mut sess, "invbroker", None, "Add",
            &[PyRep::Integer(non_fuel), PyRep::Integer(struct_id), PyRep::Integer(133), PyRep::Integer(100)],
            &kw, None,
        ).await.expect_err("Non-fuel into structure fuel bay must fail");
        assert_eq!(err_type.code, "InvalidInventoryFlag");

        // 4. Moving fuel into structure fuel bay while in range succeeds
        let ok_fuel = ctx.dispatch_rpc(
            &mut sess, "invbroker", None, "Add",
            &[PyRep::Integer(fuel_block), PyRep::Integer(struct_id), PyRep::Integer(133), PyRep::Integer(10)],
            &kw, None,
        ).await.expect("In-range fuel move must succeed");
        assert!(matches!(ok_fuel, PyRep::KeyVal(_) | PyRep::Dict(_) | PyRep::None));
    }

    #[tokio::test]
    async fn test_fit_ships_dual_prop_allowed_while_max_group_and_type_fitted_enforced() {
        let ctx = new_ctx();
        let kw = HashMap::new();
        let char_id = 90_000_001i64;
        let station_id = 60_003_760i64;
        let ship_id = 870_090i64;
        let mut sess = SessionState::new(7199);
        common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
        sess.character_id = Some(char_id);
        sess.station_id = Some(station_id);
        sess.solar_system_id = Some(30_000_142);
        sess.ship_id = Some(ship_id);

        // Seed Caracal (621, 5 med slots, 4 low slots) + AB (438) + MWD (440) + 2x Damage Control I (2048, maxGroupFitted=1)
        seed_item(&ctx, ship_id, 621, char_id, station_id, InventoryFlag::Hangar, 1, true);
        seed_item(&ctx, 870_091, 438, char_id, station_id, InventoryFlag::Hangar, 1, true);
        seed_item(&ctx, 870_092, 440, char_id, station_id, InventoryFlag::Hangar, 1, true);
        seed_item(&ctx, 870_093, 2048, char_id, station_id, InventoryFlag::Hangar, 1, true);
        seed_item(&ctx, 870_094, 2048, char_id, station_id, InventoryFlag::Hangar, 1, true);
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&char_id) {
                ch.station_id = Some(station_id);
                ch.structure_id = None;
                ch.active_ship_id = ship_id;
            }
            let sk = snap.skills.entry(char_id).or_default();
            for &stid in &[3327u32, 3332, 3449, 3450, 3451, 3392, 3413, 3426] {
                sk.insert(stid, eve_db::TrainedSkillRecord { type_id: stid, level: 5, skill_points: 256_000.0 });
            }
        });

        // 1. Dual-prop (AB 438 + MWD 440, group 46 maxGroupActive=1, no maxGroupFitted) must succeed via FitShips
        ctx.dispatch_rpc(
            &mut sess,
            "ship",
            None,
            "FitShips",
            &[
                PyRep::List(vec![PyRep::Integer(ship_id)]),
                PyRep::List(vec![
                    PyRep::Tuple(vec![PyRep::Integer(438), PyRep::Integer(InventoryFlag::MedSlot0 as i64), PyRep::Integer(1)]),
                    PyRep::Tuple(vec![PyRep::Integer(440), PyRep::Integer(InventoryFlag::MedSlot1 as i64), PyRep::Integer(1)]),
                ]),
            ],
            &kw,
            None,
        )
        .await
        .expect("Dual-prop AB + MWD must be allowed by ship.FitShips");

        ctx.db.with_snapshot(|snap| {
            let fitted_props: Vec<_> = snap
                .items
                .values()
                .filter(|i| i.location_id == ship_id && matches!(i.flag, InventoryFlag::MedSlot0 | InventoryFlag::MedSlot1))
                .collect();
            assert_eq!(fitted_props.len(), 2, "Both AB and MWD must be fitted to med slots");
            let online_cnt = fitted_props
                .iter()
                .filter(|i| snap.module_online_states.get(&i.item_id).copied().unwrap_or(false))
                .count();
            assert_eq!(online_cnt, 1, "At most one propulsion module may be online due to maxGroupOnline/maxGroupActive");
        });

        // 2. Two Damage Control I (2048, maxGroupFitted=1) must still be rejected with MaxGroupFittedExceeded
        let dc_err = ctx
            .dispatch_rpc(
                &mut sess,
                "ship",
                None,
                "FitShips",
                &[
                    PyRep::List(vec![PyRep::Integer(ship_id)]),
                    PyRep::List(vec![
                        PyRep::Tuple(vec![PyRep::Integer(2048), PyRep::Integer(InventoryFlag::LoSlot0 as i64), PyRep::Integer(1)]),
                        PyRep::Tuple(vec![PyRep::Integer(2048), PyRep::Integer(InventoryFlag::LoSlot1 as i64), PyRep::Integer(1)]),
                    ]),
                ],
                &kw,
                None,
            )
            .await
            .expect_err("Fitting 2x Damage Control must fail MaxGroupFittedExceeded");
        assert!(dc_err.message.contains("MaxGroupFittedExceeded"), "unexpected error: {}", dc_err.message);
    }
}
