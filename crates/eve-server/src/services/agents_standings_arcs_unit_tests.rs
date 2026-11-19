//! Unit regression tests for V24 Epic Arc briefing resolution and FILETIME bookmark timestamps.

#![forbid(unsafe_code)]

use super::*;
use crate::router::ServerContext;
use eve_db::WorldRepository;
use eve_proto::PyRep;
use eve_sde::{AgentInfo, EpicArcTemplate, NpcCorporationInfo, SdeRegistry};
use std::sync::Arc;

fn make_arc_ctx() -> ServerContext {
    let mut sde = SdeRegistry::default();
    sde.npc_corporations.insert(
        1_000_130,
        NpcCorporationInfo {
            corporation_id: 1_000_130,
            name: "Sisters of EVE".into(),
            ticker_name: "SOE".into(),
            faction_id: Some(500_003),
            solar_system_id: Some(30_000_142),
            station_id: Some(60_003_760),
            tax_rate: 0.0,
            ceo_id: None,
        },
    );
    sde.npc_corporations.insert(
        1_000_050,
        NpcCorporationInfo {
            corporation_id: 1_000_050,
            name: "Brutor Tribe".into(),
            ticker_name: "BRU".into(),
            faction_id: Some(500_002),
            solar_system_id: Some(30_000_142),
            station_id: Some(60_003_760),
            tax_rate: 0.0,
            ceo_id: None,
        },
    );
    sde.agents.insert(
        3_900_010,
        AgentInfo {
            agent_id: 3_900_010,
            name: "Sister Alitura".into(),
            corporation_id: 1_000_130,
            station_id: Some(60_003_760),
            solar_system_id: Some(30_000_142),
            level: 1,
            division_id: 24,
            agent_type_id: 10,
            is_locator: false,
            research_skills: Vec::new(),
        },
    );
    sde.agents.insert(
        3_900_011,
        AgentInfo {
            agent_id: 3_900_011,
            name: "Arnak".into(),
            corporation_id: 1_000_050,
            station_id: Some(60_003_760),
            solar_system_id: Some(30_000_142),
            level: 4,
            division_id: 24,
            agent_type_id: 10,
            is_locator: false,
            research_skills: Vec::new(),
        },
    );
    sde.epic_arcs.clear();
    sde.epic_arcs.insert(
        29,
        EpicArcTemplate {
            epic_arc_id: 29,
            name: "The Blood-Stained Stars".into(),
            faction_id: Some(500_003),
            restart_interval: Some(90),
            missions_count: 50,
        },
    );
    sde.epic_arcs.insert(
        52,
        EpicArcTemplate {
            epic_arc_id: 52,
            name: "Wildfire".into(),
            faction_id: Some(500_002),
            restart_interval: Some(90),
            missions_count: 14,
        },
    );
    sde.epic_arcs.insert(
        4,
        EpicArcTemplate {
            epic_arc_id: 4,
            name: "Legacy Arc 4".into(),
            faction_id: None,
            restart_interval: Some(90),
            missions_count: 3,
        },
    );
    ServerContext::with_sde_and_repository(Arc::new(sde), Arc::new(WorldRepository::with_demo_seed()))
}

#[test]
fn resolve_v24_epic_arc_for_mission_uses_v24_keys_and_preserves_legacy_agent_ids() {
    let mut ctx = make_arc_ctx();

    // Spec-mapped V24 mission (14117 -> Arc 29) resolves V24 name despite legacy agent mapping = 4.
    assert_eq!(resolve_epic_arc_id_for_agent(&ctx, 3_900_010), (4, 500_003));
    assert_eq!(
        resolve_v24_epic_arc_for_mission(&ctx, 3_900_010, 14_117),
        (29, Some("The Blood-Stained Stars".into()))
    );

    // Unmapped mission for an Epic Arc faction resolves the faction's V24 epic_arc deterministically.
    assert_eq!(resolve_epic_arc_id_for_agent(&ctx, 3_900_011), (3, 500_002));
    assert_eq!(
        resolve_v24_epic_arc_for_mission(&ctx, 3_900_011, 999_001),
        (52, Some("Wildfire".into()))
    );

    // When V24 epic_arcs entry is absent, spec mission still retains V24 arc_id (29, None) instead of 4.
    let mut sparse_sde = (*ctx.sde).clone();
    sparse_sde.epic_arcs.remove(&29);
    ctx.sde = Arc::new(sparse_sde);
    assert_eq!(resolve_v24_epic_arc_for_mission(&ctx, 3_900_010, 14_117), (29, None));
    assert_eq!(
        resolve_v24_epic_arc_for_mission(&ctx, 3_900_010, 999_002),
        (4, Some("Legacy Arc 4".into()))
    );
}

#[test]
fn unix_to_filetime_rep_handles_epoch_modern_and_saturating_timestamps() {
    assert_eq!(unix_to_filetime_rep(0), PyRep::Integer(116_444_736_000_000_000));
    assert_eq!(
        unix_to_filetime_rep(1_800_000_000),
        PyRep::Integer((1_800_000_000_i64 + 11_644_473_600) * 10_000_000)
    );
    assert_eq!(unix_to_filetime_rep(u64::MAX), PyRep::Integer(i64::MAX));
}
