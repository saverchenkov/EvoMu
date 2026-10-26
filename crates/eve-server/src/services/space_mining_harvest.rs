//! `space_mining_harvest` — Gameplay progression event dispatch on mining harvest / yield.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::quasar_campaign_state::CampaignEvent;

/// Concrete authentic contribution method for harvesting a type of SDE group `group_id`:
/// Ice (465) -> `MineIce`, Harvestable Cloud (711) -> `MineGas`, moon ore groups (1884, 1920-1923)
/// -> `MineMoon`, any other ore -> `MineAsteroid`.
pub fn mining_contribution_method(group_id: u32) -> &'static str {
    match group_id {
        465 => "MineIce",
        711 => "MineGas",
        1884 | 1920..=1923 => "MineMoon",
        _ => "MineAsteroid",
    }
}

pub fn dispatch_mining_gameplay_events(ctx: &ServerContext, char_id: i64, units: u64, sys_id: u32, ore_type_id: u32) {
    if units > 0 {
        ctx.quasar_gateway
            .progression
            .daily_goals
            .on_gameplay_event(char_id as u32, 2, units, &ctx.quasar_gateway);
        ctx.quasar_gateway
            .progression
            .career_goals
            .on_gameplay_event(char_id as u32, "mining", units, &ctx.quasar_gateway);
        ctx.quasar_gateway
            .progression
            .extra
            .on_campaign_gameplay_event(
                char_id as u32,
                CampaignEvent {
                    method: mining_contribution_method(ctx.sde.types.get(&ore_type_id).map(|t| t.group_id).unwrap_or(0)),
                    type_id: Some(ore_type_id),
                    solar_system_id: Some(sys_id),
                    faction_id: None,
                },
                units,
                ctx.now_unix(),
                &ctx.quasar_gateway,
            );
    }
}

pub fn deposit_mined_ore_and_record_ledger(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    sys_id: u32,
    target_flag: eve_db::InventoryFlag,
    ore_type_id: u32,
    mined_units: u64,
) {
    if mined_units > 0
        && ctx
            .db
            .add_or_stack_item(char_id, ship_id, target_flag, ore_type_id, mined_units as i64)
            .is_ok()
    {
        let now_unix = ctx.now_unix_secs();
        let _ = ctx.db.record_mining_ledger_entry(
            char_id,
            sys_id,
            ore_type_id,
            mined_units as i64,
            now_unix,
        );
        dispatch_mining_gameplay_events(ctx, char_id, mined_units, sys_id, ore_type_id);
    }
}
