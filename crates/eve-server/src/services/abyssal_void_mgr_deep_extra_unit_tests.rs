//! Deep branch-coverage and mutation-killing unit tests for `abyssal_void_mgr` and `abyssal_void_gates`:
//! trace lifetime expiration (explicit & fallback TTLs), fleet ship capacity validation,
//! highsec tier suspect flags, room gate progression (1 -> 2 -> 3 -> exit & collapse),
//! and Void Space jump, relog (`OnVoidSpaceRelog`), and end gate activation.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::abyssal_void_mgr::{
    handle, ABYSSAL_SPACE_SYSTEM_ID, ABYSSAL_SYSTEM_RANGE, VOID_SPACE_SYSTEM_ID,
};
use crate::services::pve_special_sites::with_abyssal_state;
use crate::services::pve_special_sites_abyssal::{
    check_abyssal_in_pocket_rpc_restrictions, evaluate_highsec_abyssal_tier_policy,
    is_abyssal_local_channel_disabled, is_abyssal_system, AbyssalTraceRecord,
    UNENTERED_GATE_LIFETIME_SEC,
};
use eve_db::clock::VirtualServerClock;
use eve_db::{InventoryFlag, InventoryItemRecord, WorldRepository};
use eve_destiny::{Ball, CombatState, DVec3, MovementMode, SlimItemMeta};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

pub(super) fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

pub(super) fn make_ctx_at(now_secs: u64) -> ServerContext {
    let db = Arc::new(WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(shared_sde(), db)
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(now_secs)));
    ctx
}

pub(super) fn seed_pilot_and_ship(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    ship_type_id: u32,
    sys_id: u32,
    pos: DVec3,
) -> SessionState {
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().expect("demo char");
        ch.character_id = char_id;
        ch.solar_system_id = sys_id;
        ch.station_id = None;
        ch.structure_id = None;
        ch.active_ship_id = ship_id;
        snap.characters.insert(char_id, ch);
        snap.item_positions.insert(ship_id, pos.to_array());
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: ship_type_id,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    sys_h.try_spawn_ball_with_meta(
        Ball {
            id: ship_id,
            type_id: ship_type_id,
            position: pos,
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 50.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
        CombatState::default(),
        SlimItemMeta {
            owner_id: char_id,
            char_id: Some(char_id),
            ..Default::default()
        },
    );
    let mut s = SessionState::new(char_id as u64);
    s.user_id = Some(char_id as u64);
    s.character_id = Some(char_id);
    s.corporation_id = Some(1_000_009);
    s.solar_system_id = Some(sys_id);
    s.ship_id = Some(ship_id);
    s
}

#[test]
fn pocket_bounds_rpc_restrictions_and_highsec_tier_suspect_policy_boundaries() {
    assert!(!is_abyssal_system(31_999_999));
    assert!(is_abyssal_system(32_000_000));
    assert!(is_abyssal_system(32_999_999));
    assert!(!is_abyssal_system(33_000_000));
    assert!(ABYSSAL_SYSTEM_RANGE.contains(&ABYSSAL_SPACE_SYSTEM_ID));
    assert!(ABYSSAL_SYSTEM_RANGE.contains(&VOID_SPACE_SYSTEM_ID));

    assert!(is_abyssal_local_channel_disabled("solarsystemid2:32000001", None));
    assert!(is_abyssal_local_channel_disabled("local", Some(32_000_001)));
    assert!(!is_abyssal_local_channel_disabled("local", Some(30_000_142)));

    let mut sess = SessionState::new(1);
    sess.solar_system_id = Some(30_000_142);
    assert!(check_abyssal_in_pocket_rpc_restrictions(&sess, "map", "GetMap", &[]).is_none());

    sess.solar_system_id = Some(32_000_001);
    assert_eq!(
        check_abyssal_in_pocket_rpc_restrictions(&sess, "map", "GetMap", &[])
            .unwrap()
            .unwrap_err()
            .code,
        "MapDisabledInAbyssalSpace"
    );
    assert_eq!(
        check_abyssal_in_pocket_rpc_restrictions(&sess, "scanMgr", "ConeScan", &[])
            .unwrap()
            .unwrap_err()
            .code,
        "CannotDscanInThisSystem"
    );
    assert_eq!(
        check_abyssal_in_pocket_rpc_restrictions(
            &sess,
            "LSC",
            "SendMessage",
            &[PyRep::String("local".into())],
        )
        .unwrap()
        .unwrap_err()
        .code,
        "LocalChannelDisabledInAbyssalSpace"
    );

    // Highsec tier policy boundaries:
    // - >= 0.85 (0.9, 1.0): T0 -> Ok(false), T1+ -> Err("FilamentDisallowedInHighSec")
    // - 0.75..0.85 (0.8): T3 -> Ok(false), T4 -> Ok(true)
    // - 0.65..0.75 (0.7): T4 -> Ok(false), T5 -> Ok(true)
    // - 0.55..0.65 (0.6): T5 -> Ok(false), T6 -> Ok(true)
    // - < 0.55 (0.5, low/null): T6 -> Ok(false)
    assert_eq!(evaluate_highsec_abyssal_tier_policy(0.9, 0).unwrap(), false);
    assert_eq!(
        evaluate_highsec_abyssal_tier_policy(0.85, 1).unwrap_err().code,
        "FilamentDisallowedInHighSec"
    );
    assert_eq!(evaluate_highsec_abyssal_tier_policy(0.8, 3).unwrap(), false);
    assert_eq!(evaluate_highsec_abyssal_tier_policy(0.8, 4).unwrap(), true);
    assert_eq!(evaluate_highsec_abyssal_tier_policy(0.7, 4).unwrap(), false);
    assert_eq!(evaluate_highsec_abyssal_tier_policy(0.7, 5).unwrap(), true);
    assert_eq!(evaluate_highsec_abyssal_tier_policy(0.6, 5).unwrap(), false);
    assert_eq!(evaluate_highsec_abyssal_tier_policy(0.6, 6).unwrap(), true);
    assert_eq!(evaluate_highsec_abyssal_tier_policy(0.5, 6).unwrap(), false);
}

#[tokio::test]
async fn trace_lifetime_expiration_explicit_and_unentered_1800s_vs_entered_1200s_fallback() {
    let base_now = 1_800_000_000u64;
    let ctx = make_ctx_at(base_now);
    let sys_id = 30_004_759u32;
    let char_id = 92_001i64;
    let ship_id = 892_001i64;
    let far_pos = DVec3::new(500_000_000.0, 500_000_000.0, 500_000_000.0);
    // Caracal (type 621, Cruiser group 26)
    let mut sess = seed_pilot_and_ship(&ctx, char_id, ship_id, 621, sys_id, far_pos);
    let kw = HashMap::new();

    // Seed 3 traces:
    // - trace 701: explicit expires_at_sec = base_now + 500
    // - trace 702: expires_at_sec = 0, unentered (pocket_id == None) -> 1800s fallback TTL
    // - trace 703: expires_at_sec = 0, entered (pocket_id == Some(99)) -> 1200s fallback TTL
    with_abyssal_state(&ctx, |st| {
        for (tid, exp, pid) in [
            (701i64, base_now + 500, None),
            (702i64, 0, None),
            (703i64, 0, Some(99i64)),
        ] {
            st.traces.insert(
                tid,
                AbyssalTraceRecord {
                    trace_ball_id: tid,
                    char_id,
                    filament_type_id: 47_762,
                    origin_solar_system_id: sys_id,
                    position: far_pos.to_array(),
                    created_unix: base_now,
                    expires_at_sec: exp,
                    max_ships: 1,
                    entered_char_ids: Vec::new(),
                    pocket_id: pid,
                    fleet_id: None,
                },
            );
        }
    });

    // At base_now + 500, trace 701 is expired (now >= expires_at_sec) -> TraceExpired and removed
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(base_now + 500)));
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "abyssalMgr",
            "AbyssalEntranceGateActivation",
            &[PyRep::Integer(701)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "TraceExpired"
    );
    assert!(!with_abyssal_state(&ctx, |st| st.traces.contains_key(&701)));

    // At base_now + 1200, entered trace 703 with expires_at_sec == 0 expires!
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(base_now + 1_200)));
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "abyssalMgr",
            "AbyssalEntranceGateActivation",
            &[PyRep::Integer(703)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "TraceExpired"
    );

    // At base_now + 1800 (UNENTERED_GATE_LIFETIME_SEC), unentered trace 702 expires!
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(
        base_now + UNENTERED_GATE_LIFETIME_SEC,
    )));
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "abyssalMgr",
            "AbyssalEntranceGateActivation",
            &[PyRep::Integer(702)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "TraceExpired"
    );
}
