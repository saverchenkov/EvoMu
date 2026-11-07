//! Deep branch-coverage and mutation-killing unit tests for `fitting_conduit_jump`.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::inventory_fitting::fitting_cyno_portal::{
    handle_cyno_generator_activation, handle_cyno_portal, is_ship_cyno_active,
    on_cyno_ship_destroyed, sweep_expired_cyno_beacons,
};
use eve_db::{
    AccountRecord, FleetMemberRecord, FleetRecord, FleetRole, InventoryFlag, InventoryItemRecord,
    TrainedSkillRecord, VirtualServerClock, WorldRepository,
};
use eve_destiny::{Ball, CombatState, DVec3, MovementMode, SlimItemMeta};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::{InvType, SdeRegistry, SolarSystem, TypeListInfo};
use std::sync::{Arc, OnceLock};

#[cfg(test)]
#[path = "fitting_group_conduit_jump_unit_tests.rs"]
mod fitting_group_conduit_jump_unit_tests;

pub(super) fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

pub(super) fn custom_inv_type(
    sde: &SdeRegistry,
    type_id: u32,
    group_id: u32,
    category_id: u32,
    mass: f64,
    name: &str,
    attrs: &[(u16, f64)],
) -> InvType {
    let mut t = sde.types.get(&587).expect("seed type").clone();
    t.type_id = type_id;
    t.group_id = group_id;
    t.category_id = category_id;
    t.mass = mass;
    t.name = name.into();
    t.default_attributes.clear();
    for &(k, v) in attrs {
        t.default_attributes.insert(k, v);
    }
    t
}

pub(super) fn custom_sys(
    sde: &SdeRegistry,
    sys_id: u32,
    region_id: u32,
    sec: f64,
    pos_ly: [f64; 3],
) -> SolarSystem {
    let mut s = sde.solar_systems.get(&30_002_505).expect("seed sys").clone();
    s.solar_system_id = sys_id;
    s.region_id = region_id;
    s.security_status = sec;
    s.galactic_pos_ly = pos_ly;
    s
}

pub(super) fn build_ctx() -> ServerContext {
    let mut sde = (*shared_sde()).clone();
    sde.solar_systems.insert(30_002_505, custom_sys(&sde, 30_002_505, 10_000_030, -0.5, [0.0, 0.0, 0.0]));
    sde.solar_systems.insert(30_002_506, custom_sys(&sde, 30_002_506, 10_000_030, -0.5, [2.0, 0.0, 0.0]));
    sde.solar_systems.insert(30_002_507, custom_sys(&sde, 30_002_507, 10_000_030, -0.5, [50.0, 0.0, 0.0]));
    sde.solar_systems.insert(30_000_142, custom_sys(&sde, 30_000_142, 10_000_002, 0.9, [1.0, 0.0, 0.0]));
    sde.solar_systems.insert(30_002_508, custom_sys(&sde, 30_002_508, 10_000_070, -1.0, [1.0, 0.0, 0.0]));

    let mut std_beacons: Vec<u32> = sde
        .type_lists
        .values()
        .flat_map(|l| l.included_type_ids.iter().copied())
        .chain([21096, 52694, 57425])
        .filter(|&tid| {
            !matches!(tid, 28646 | 28650 | 56488 | 59630 | 59631)
                && sde
                    .types
                    .get(&tid)
                    .is_none_or(|t| t.default_attributes.get(&1252).copied().unwrap_or(0.0) <= 0.0)
        })
        .collect();
    std_beacons.sort_unstable();
    std_beacons.dedup();
    sde.type_lists.insert(
        99_401,
        TypeListInfo {
            type_list_id: 99_401,
            name: "Standard Test Beacons".into(),
            included_type_ids: std_beacons,
            ..Default::default()
        },
    );
    sde.types.insert(57425, custom_inv_type(&sde, 57425, 650, 2, 1.0, "Cynosural Field", &[]));
    sde.types.insert(59631, custom_inv_type(&sde, 59631, 650, 2, 1.0, "Covert Cynosural Field", &[(1252, 1.0)]));
    sde.types.insert(
        28646,
        custom_inv_type(&sde, 28646, 658, 7, 10.0, "Covert Cynosural Field Generator I", &[(73, 60_000.0), (713, 16273.0), (714, 10.0), (1252, 1.0)]),
    );
    sde.types.insert(
        21096,
        custom_inv_type(&sde, 21096, 658, 7, 10.0, "Cynosural Field Generator I", &[(73, 60_000.0), (713, 16273.0), (714, 100.0)]),
    );
    sde.types.insert(
        999_801,
        custom_inv_type(
            &sde,
            999_801,
            30,
            6,
            100_000_000.0,
            "Test Avatar",
            &[(861, 1.0), (866, 17888.0), (867, 6.0), (868, 1000.0), (898, 0.20), (3125, 1.0), (3317, 99_401.0)],
        ),
    );
    sde.types.insert(
        999_802,
        custom_inv_type(
            &sde,
            999_802,
            883,
            6,
            50_000_000.0,
            "Test Rorqual",
            &[(861, 1.0), (866, 17888.0), (867, 6.0), (868, 800.0), (898, 0.25), (3125, 1.0), (3131, 150.0), (3317, 99_401.0)],
        ),
    );
    sde.types.insert(
        999_803,
        custom_inv_type(&sde, 999_803, 547, 6, 20_000_000.0, "NoConduitCarrier", &[(861, 1.0), (866, 17888.0), (867, 6.0), (3317, 99_401.0)]),
    );
    sde.types.insert(
        999_804,
        custom_inv_type(
            &sde,
            999_804,
            657,
            7,
            100.0,
            "Jump Portal Generator I",
            &[(73, 60_000.0), (713, 16274.0), (714, 500.0), (1001, 0.000001), (1002, 60_000.0), (1298, 30.0), (3125, 1.0), (3318, 861.0)],
        ),
    );
    sde.types.insert(28583, custom_inv_type(&sde, 28583, 515, 7, 100.0, "Industrial Core I", &[]));
    sde.types.insert(999_805, custom_inv_type(&sde, 999_805, 463, 6, 2_000_000.0, "Test Procurer", &[(3324, 1.0)]));

    let db = Arc::new(WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(Arc::new(sde), db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_800_000_000)));
    ctx
}

pub(super) async fn call_beyonce(
    ctx: &ServerContext,
    session: &mut SessionState,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    handle_cyno_portal(ctx, session, "beyonce", method, args).await.expect("handled")
}

pub(super) fn seed_pilot_and_ship(
    ctx: &ServerContext,
    char_id: i64,
    corp_id: i64,
    sys_id: u32,
    ship_id: i64,
    ship_tid: u32,
    fleet_id: Option<i64>,
) {
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().expect("seed char");
        ch.character_id = char_id;
        ch.user_id = char_id as u64;
        ch.clone_grade = 2;
        ch.name = format!("Pilot_{char_id}");
        ch.corporation_id = corp_id;
        ch.station_id = None;
        ch.structure_id = None;
        ch.solar_system_id = sys_id;
        ch.active_ship_id = ship_id;
        ch.fleet_id = fleet_id;
        snap.characters.insert(char_id, ch);
        snap.accounts.insert(
            format!("u_{char_id}"),
            AccountRecord {
                user_id: char_id as u64,
                username: format!("u_{char_id}"),
                password_token: String::new(),
                role: 0,
                banned_until_unix: None,
                ban_reason: None,
            },
        );
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: ship_tid,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });
    let _ = ctx.db.acquire_online_lock(char_id as u64, char_id as u64, Some(char_id), true, ctx.now_unix_secs());
}

pub(super) async fn spawn_ship_ball(
    ctx: &ServerContext,
    sys_id: u32,
    ship_id: i64,
    type_id: u32,
    owner_id: i64,
    pos: DVec3,
    cap: f64,
) {
    let sys = ctx.systems.get_or_spawn(sys_id);
    let ball = Ball {
        id: ship_id,
        type_id,
        position: pos,
        velocity: DVec3::ZERO,
        max_velocity: 1_000.0,
        inertia: 1.0,
        mass: 5_000_000.0,
        radius: 500.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let mut cs = CombatState::default();
    cs.capacitor = cap;
    cs.max_capacitor = cap.max(1000.0);
    let meta = SlimItemMeta {
        group_id: 30,
        category_id: 6,
        owner_id,
        char_id: Some(owner_id),
        corp_id: 98_001,
        ..Default::default()
    };
    let _ = sys.spawn_ball_with_meta(ball, cs, meta).await;
}
