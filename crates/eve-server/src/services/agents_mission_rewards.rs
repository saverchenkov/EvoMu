#![forbid(unsafe_code)]

use crate::router::{NotificationTarget, ServerContext};
use eve_db::{InventoryFlag, InventoryItemRecord, WorldStateSnapshot};
use eve_proto::PyRep;
use eve_sde::{AgentInfo, MissionTemplate};

#[derive(Debug, Clone, Copy)]
pub struct MintedMissionItemReward {
    pub item_id: i64,
    pub type_id: u32,
    pub quantity: i32,
    pub station_id: i64,
}

/// Resolves an SDE-backed item reward `(type_id, quantity)` for Career Agent (`agent_type_id == 12`),
/// Storyline (`5`), COSMOS (`8`), and Epic Arc (`10`) missions (`feat:pve.mission-complete`).
pub fn resolve_mission_item_reward(
    ctx: &ServerContext,
    tmpl: &MissionTemplate,
    agent: Option<&AgentInfo>,
) -> Option<(u32, i32)> {
    let ag = agent?;
    let candidates: &[(u32, i32)] = match ag.agent_type_id {
        12 => match ag.division_id {
            19 => &[
                (30_013, 8), // Core Scanner Probe I
                (30_832, 1), // Data Analyzer I
                (30_834, 1), // Relic Analyzer I
                (605, 1),    // Heron
            ],
            22 | 23 | 25 => &[
                (483, 1),    // Miner I
                (439, 1),    // 1MN Afterburner I
                (32_880, 1), // Venture
                (650, 1),    // Nereus
            ],
            _ => &[
                (439, 1),   // 1MN Afterburner I
                (561, 1),   // Light Electron Blaster I
                (222, 100), // Antimatter Charge S
                (587, 1),   // Rifter
            ],
        },
        5 => &[
            (9_899, 1), // Ocular Filter - Basic
            (9_941, 1), // Memory Augmentation - Basic
            (9_942, 1), // Neural Boost - Basic
            (9_943, 1), // Cybernetic Subprocessor - Basic
            (9_950, 1), // Social Adaptation Chip - Basic
        ],
        8 | 10 => &[
            (9_899, 1), // Ocular Filter - Basic
            (439, 1),   // 1MN Afterburner I
        ],
        _ => return None,
    };

    let idx = (tmpl.mission_id as usize) % candidates.len();
    for offset in 0..candidates.len() {
        let (tid, qty) = candidates[(idx + offset) % candidates.len()];
        if ctx.sde.types.contains_key(&tid) {
            return Some((tid, qty));
        }
    }
    None
}

pub fn mint_mission_item_reward_in_snap(
    ctx: &ServerContext,
    snap: &mut WorldStateSnapshot,
    char_id: i64,
    station_id: i64,
    tmpl: &MissionTemplate,
    agent: Option<&AgentInfo>,
) -> Option<MintedMissionItemReward> {
    let (type_id, quantity) = resolve_mission_item_reward(ctx, tmpl, agent)?;
    let is_ship = ctx
        .sde
        .types
        .get(&type_id)
        .is_some_and(|t| t.category_id == 6);
    snap.next_entity_id += 1;
    let item_id = snap.next_entity_id;
    snap.items.insert(
        item_id,
        InventoryItemRecord {
            item_id,
            type_id,
            owner_id: char_id,
            location_id: station_id,
            flag: InventoryFlag::Hangar,
            quantity,
            singleton: is_ship,
            ..Default::default()
        },
    );
    Some(MintedMissionItemReward {
        item_id,
        type_id,
        quantity,
        station_id,
    })
}

pub fn emit_mission_item_reward_notification(
    ctx: &ServerContext,
    char_id: i64,
    reward: MintedMissionItemReward,
) {
    let item_row = PyRep::keyval([
        ("itemID", PyRep::Integer(reward.item_id)),
        ("typeID", PyRep::Integer(reward.type_id as i64)),
        ("ownerID", PyRep::Integer(char_id)),
        ("locationID", PyRep::Integer(reward.station_id)),
        ("flagID", PyRep::Integer(InventoryFlag::Hangar as i64)),
        ("quantity", PyRep::Integer(reward.quantity as i64)),
        ("singleton", PyRep::Bool(false)),
    ]);
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnItemChange",
        "charid",
        vec![item_row, PyRep::Dict(Vec::new())],
    );
}

#[cfg(test)]
#[path = "agents_mission_rewards_tests.rs"]
mod agents_mission_rewards_tests;

