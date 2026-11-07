//! Unit tests for `fleet_social_fleet_ops` broadcasts, warps, MOTD, options, and preferences.
use super::*;
use eve_db::{
    CharacterRecord, CorporationRecord, FleetRecord, FleetSquadRecord, FleetWingRecord,
    InventoryFlag, InventoryItemRecord, IskCents, VirtualServerClock, WorldStateSnapshot,
};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

#[cfg(test)]
#[path = "fleet_social_fleet_ops_preferences_tests.rs"]
mod fleet_social_fleet_ops_preferences_tests;


#[cfg(test)]
#[path = "fleet_social_fleet_ops_warp_tests.rs"]
mod fleet_social_fleet_ops_warp_tests;

pub(crate) fn test_ctx_and_clock(ts: u64) -> (ServerContext, Arc<VirtualServerClock>) {
    let clock = Arc::new(VirtualServerClock::with_fixed_base(ts));
    let ctx = ServerContext::new();
    ctx.set_clock(clock.clone());
    (ctx, clock)
}

pub(crate) fn test_ctx_with_clock(ts: u64) -> ServerContext {
    test_ctx_and_clock(ts).0
}

pub(crate) fn clone_char_with(
    snap: &WorldStateSnapshot,
    cid: i64,
    name: &str,
    corp_id: i64,
    sys_id: u32,
    ship_id: i64,
) -> CharacterRecord {
    let mut ch = snap.characters.values().next().expect("demo char seeded").clone();
    ch.character_id = cid;
    ch.user_id = cid as u64;
    ch.name = name.to_string();
    ch.corporation_id = corp_id;
    ch.alliance_id = None;
    ch.station_id = None;
    ch.structure_id = None;
    ch.solar_system_id = sys_id;
    ch.active_ship_id = ship_id;
    ch.balance = IskCents::from_isk(100_000_000);
    ch
}

pub(crate) async fn seed_multi_member_fleet(ctx: &ServerContext) -> (i64, SessionState) {
    let fleet_id = 700_001;
    let wing1 = 710_001;
    let wing2 = 710_002;
    let squad1 = 720_001;
    let squad2 = 720_002;
    let jita = ctx.systems.get_or_spawn(30_000_142);
    let perimeter = ctx.systems.get_or_spawn(30_000_144);

    let chars = [
        (90_000_001_i64, "FC Boss", 30_000_142_u32, 100_000_001_i64, 587_u32, DVec3::new(0.0, 0.0, 0.0)),
        (90_000_002_i64, "WC Bubble", 30_000_142_u32, 100_000_002_i64, 588_u32, DVec3::new(15_000.0, 0.0, 0.0)),
        (90_000_003_i64, "SC Far", 30_000_142_u32, 100_000_003_i64, 589_u32, DVec3::new(400_000.0, 0.0, 0.0)),
        (90_000_004_i64, "Mem Wing2", 30_000_142_u32, 100_000_004_i64, 590_u32, DVec3::new(25_000.0, 0.0, 0.0)),
        (90_000_005_i64, "Mem Perim", 30_000_144_u32, 100_000_005_i64, 591_u32, DVec3::new(5_000.0, 0.0, 0.0)),
    ];

    for &(_, _, sys_id, ship_id, type_id, pos) in &chars {
        let handle = if sys_id == 30_000_142 { &jita } else { &perimeter };
        let _ = handle
            .spawn_ball(Ball {
                id: ship_id,
                type_id,
                position: pos,
                velocity: DVec3::ZERO,
                max_velocity: 350.0,
                inertia: 2.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            })
            .await;
    }

    ctx.db.with_mut_snapshot(|snap| {
        for &(cid, name, sys_id, ship_id, type_id, pos) in &chars {
            let ch = clone_char_with(snap, cid, name, 98_000_001, sys_id, ship_id);
            snap.characters.insert(cid, ch);
            snap.items.insert(
                ship_id,
                InventoryItemRecord {
                    item_id: ship_id,
                    type_id,
                    owner_id: cid,
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
        }
        snap.corporations.insert(
            98_000_001,
            CorporationRecord {
                corporation_id: 98_000_001,
                name: "Test Corp".into(),
                ticker: "TSTC".into(),
                ceo_id: 90_000_001,
                tax_rate: 0.05,
                master_wallet: IskCents::from_isk(100_000_000),
                members: chars.iter().map(|c| c.0).collect(),
                ..Default::default()
            },
        );
        let mut details = HashMap::new();
        details.insert(
            90_000_001,
            FleetMemberRecord {
                character_id: 90_000_001,
                role: FleetRole::FleetCommander,
                wing_id: Some(wing1),
                squad_id: Some(squad1),
                takes_fleet_warp: true,
                accepts_regroup: true,
                accepts_conduit_jumps: true,
                job: notify::FLEET_JOB_CREATOR,
                role_name: "Fleet Commander".into(),
                join_time_unix: 1_700_000_000,
            },
        );
        details.insert(
            90_000_002,
            FleetMemberRecord {
                character_id: 90_000_002,
                role: FleetRole::WingCommander,
                wing_id: Some(wing1),
                squad_id: Some(squad1),
                takes_fleet_warp: true,
                accepts_regroup: true,
                accepts_conduit_jumps: true,
                job: 0,
                role_name: "Wing Commander".into(),
                join_time_unix: 1_700_000_000,
            },
        );
        details.insert(
            90_000_003,
            FleetMemberRecord {
                character_id: 90_000_003,
                role: FleetRole::SquadCommander,
                wing_id: Some(wing1),
                squad_id: Some(squad1),
                takes_fleet_warp: true,
                accepts_regroup: true,
                accepts_conduit_jumps: true,
                job: 0,
                role_name: "Squad Commander".into(),
                join_time_unix: 1_700_000_000,
            },
        );
        details.insert(
            90_000_004,
            FleetMemberRecord {
                character_id: 90_000_004,
                role: FleetRole::SquadMember,
                wing_id: Some(wing2),
                squad_id: Some(squad2),
                takes_fleet_warp: true,
                accepts_regroup: true,
                accepts_conduit_jumps: true,
                job: 0,
                role_name: "Squad Member".into(),
                join_time_unix: 1_700_000_000,
            },
        );
        details.insert(
            90_000_005,
            FleetMemberRecord {
                character_id: 90_000_005,
                role: FleetRole::SquadMember,
                wing_id: Some(wing2),
                squad_id: Some(squad2),
                takes_fleet_warp: true,
                accepts_regroup: true,
                accepts_conduit_jumps: true,
                job: 0,
                role_name: "Squad Member".into(),
                join_time_unix: 1_700_000_000,
            },
        );
        let mut wing_details = HashMap::new();
        wing_details.insert(
            wing1,
            FleetWingRecord {
                wing_id: wing1,
                name: "Wing 1".into(),
                commander_id: Some(90_000_002),
                squad_ids: vec![squad1],
            },
        );
        wing_details.insert(
            wing2,
            FleetWingRecord {
                wing_id: wing2,
                name: "Wing 2".into(),
                commander_id: None,
                squad_ids: vec![squad2],
            },
        );
        let mut squad_details = HashMap::new();
        squad_details.insert(
            squad1,
            FleetSquadRecord {
                squad_id: squad1,
                wing_id: wing1,
                name: "Squad 1".into(),
                commander_id: Some(90_000_003),
            },
        );
        squad_details.insert(
            squad2,
            FleetSquadRecord {
                squad_id: squad2,
                wing_id: wing2,
                name: "Squad 2".into(),
                commander_id: None,
            },
        );
        snap.fleets.insert(
            fleet_id,
            FleetRecord {
                fleet_id,
                boss_id: 90_000_001,
                members: chars.iter().map(|c| c.0).collect(),
                wings: vec![wing1, wing2],
                squads: vec![squad1, squad2],
                free_move: false,
                is_registered: false,
                wing_details,
                squad_details,
                member_details: details,
                invites: HashSet::new(),
                motd: String::new(),
            },
        );
    });

    let mut session = SessionState::new(1);
    session.user_id = Some(90_000_001);
    session.character_id = Some(90_000_001);
    session.corporation_id = Some(98_000_001);
    session.solar_system_id = Some(30_000_142);
    session.ship_id = Some(100_000_001);
    session.fleet_id = Some(fleet_id);
    (fleet_id, session)
}
