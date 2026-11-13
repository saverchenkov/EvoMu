//! `quasar_debug_race_ship_services` — Quasar Debug, Core Quasar Admin/Routes,
//! Race, Raffle, School, Search, SecurityStatus, and Ship domain services.

#![forbid(unsafe_code)]

use crate::quasar_gateway::{
    require_admin_role, NoticeScope, PublicGatewayRouter,
};
use eve_db::WorldRepository;
use eve_proto::pb;
use eve_proto::quasar::{
    decode_zigzag32, decode_zigzag64, DynamicValue, QuasarError, WireMessage,
};
use eve_proto::BridgeRpcError;
use eve_sde::SdeRegistry;
use std::sync::Arc;

/// Registers Quasar gateway RPC handlers for debug, quasar admin, and race services.
pub fn register_handlers(
    router: &PublicGatewayRouter,
    _db: Arc<WorldRepository>,
    sde: Arc<SdeRegistry>,
) {
    // 1. eve_public.debug.Request -> eve_public.debug.Response (echo payload data)
    router.register_handler(
        "eve_public.debug.Request",
        Arc::new(move |_ctx, _any, dyn_msg| {
            let req_data = match dyn_msg.get(1) {
                Some(DynamicValue::Bytes(b)) => b.clone(),
                _ => Vec::new(),
            };
            Ok(pb::message("eve_public.debug.Response")
                .set("data", req_data)
                .must_to_any())
        }),
    );

    // 2. eve.quasar.admin.IsAuthorizedRequest -> eve.quasar.admin.IsAuthorizedResponse
    router.register_handler(
        "eve.quasar.admin.IsAuthorizedRequest",
        Arc::new(move |ctx, _any, _dyn_msg| {
            let is_authorized = require_admin_role(ctx).is_ok() || ctx.effective_user_id().is_some();
            Ok(pb::message("eve.quasar.admin.IsAuthorizedResponse")
                .set("authorized", is_authorized)
                .must_to_any())
        }),
    );

    // 3. eve.race.GetAllRequest -> eve.race.GetAllResponse
    let sde_race = Arc::clone(&sde);
    router.register_handler(
        "eve.race.GetAllRequest",
        Arc::new(move |_ctx, _any, dyn_msg| {
            let playable_only = match dyn_msg.get(1) {
                Some(DynamicValue::Bool(b)) => *b,
                _ => false,
            };
            let mut out = pb::message("eve.race.GetAllResponse");
            let mut races: Vec<_> = sde_race.races.values().collect();
            races.sort_by_key(|r| r.race_id);
            for race in races {
                if playable_only && !matches!(race.race_id, 1 | 2 | 4 | 8) {
                    continue;
                }
                out = out.add_with("races", |r| {
                    r.set("identifier.sequential", race.race_id as u32)
                        .set("attributes.name", &race.name)
                        .set("attributes.description", &race.description)
                        .when(race.corvette_type_id.is_some(), |r| {
                            r.set(
                                "attributes.ship_type.sequential",
                                race.corvette_type_id.unwrap_or_default() as u64,
                            )
                        })
                });
            }
            Ok(out.must_to_any())
        }),
    );
}

/// Publishes an `eve_public.debug.Notice` to the specified gateway scope.
pub fn publish_debug_notice(
    gateway: &PublicGatewayRouter,
    scope: NoticeScope,
    uuid: &[u8],
) -> Result<usize, BridgeRpcError> {
    let notice = pb::message("eve_public.debug.Notice")
        .set("uuid", uuid)
        .must_to_any();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    gateway
        .publish_notice(scope, notice, now)
        .map(|(_, count)| count)
}

/// Publishes an `eve.debug.monolith.event.EventEmitted` notice to the specified gateway scope.
pub fn publish_monolith_event_notice(
    gateway: &PublicGatewayRouter,
    scope: NoticeScope,
    tracer: &[u8],
) -> Result<usize, BridgeRpcError> {
    let notice = pb::message("eve.debug.monolith.event.EventEmitted")
        .set("tracer", String::from_utf8_lossy(tracer).into_owned())
        .must_to_any();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    gateway
        .publish_notice(scope, notice, now)
        .map(|(_, count)| count)
}

/// Publishes an `eve.quasar.CharacterRouteEstablished` notice to the specified gateway scope.
pub fn publish_character_route_established(
    gateway: &PublicGatewayRouter,
    scope: NoticeScope,
    character_id: u64,
) -> Result<usize, BridgeRpcError> {
    let notice = pb::message("eve.quasar.CharacterRouteEstablished")
        .set("character.sequential", character_id as u32)
        .must_to_any();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    gateway
        .publish_notice(scope, notice, now)
        .map(|(_, count)| count)
}

/// Publishes an `eve.raffle.ItemRaffleCreated` notice.
pub fn publish_raffle_created_notice(
    gateway: &PublicGatewayRouter,
    scope: NoticeScope,
    char_id: u64,
    raffle_id: u64,
    ticket_count: u64,
    ticket_price_cents: u64,
    item_id: u64,
    station_id: u64,
    is_private: bool,
) -> Result<usize, BridgeRpcError> {
    let units = (ticket_price_cents / 100) as i64;
    let nanos = ((ticket_price_cents % 100) * 10_000_000) as i32;
    let notice = pb::message("eve.raffle.ItemRaffleCreated")
        .set("character.sequential", char_id as u32)
        .set("raffle_id", raffle_id.to_string())
        .set("ticket_count", ticket_count as i32)
        .set("ticket_price.units", units)
        .when(nanos != 0, |m| m.set("ticket_price.nanos", nanos))
        .set("item.sequential", item_id)
        .set("station.sequential", station_id)
        .set("is_private", is_private)
        .must_to_any();

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    gateway
        .publish_notice(scope, notice, now)
        .map(|(_, count)| count)
}

/// Publishes an `eve.raffle.TicketBought` notice.
pub fn publish_raffle_ticket_bought_notice(
    gateway: &PublicGatewayRouter,
    scope: NoticeScope,
    raffle_id: u64,
    ticket_number: u64,
    char_id: u64,
) -> Result<usize, BridgeRpcError> {
    let notice = pb::message("eve.raffle.TicketBought")
        .set("raffle_id", raffle_id.to_string())
        .set("ticket_number", ticket_number.to_string())
        .set("character.sequential", char_id as u32)
        .must_to_any();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    gateway
        .publish_notice(scope, notice, now)
        .map(|(_, count)| count)
}

/// Publishes an `eve.raffle.Finished` notice.
pub fn publish_raffle_finished_notice(
    gateway: &PublicGatewayRouter,
    scope: NoticeScope,
    raffle_id: u64,
    winner_char_id: u64,
) -> Result<usize, BridgeRpcError> {
    let notice = pb::message("eve.raffle.Finished")
        .set("raffle_id", raffle_id.to_string())
        .set("winner.sequential", winner_char_id as u32)
        .must_to_any();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    gateway
        .publish_notice(scope, notice, now)
        .map(|(_, count)| count)
}

/// Publishes an `eve.raffle.Expired` notice.
pub fn publish_raffle_expired_notice(
    gateway: &PublicGatewayRouter,
    scope: NoticeScope,
    raffle_id: u64,
) -> Result<usize, BridgeRpcError> {
    let notice = pb::message("eve.raffle.Expired")
        .set("raffle_id", raffle_id.to_string())
        .must_to_any();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    gateway
        .publish_notice(scope, notice, now)
        .map(|(_, count)| count)
}

/// Publishes an `eve.seasonalevent.challenge.ChallengeProgressed` notice.
pub fn publish_challenge_progressed_notice(
    gateway: &PublicGatewayRouter,
    scope: NoticeScope,
    char_id: u64,
    challenge_id: u64,
    amount: u64,
) -> Result<usize, BridgeRpcError> {
    let notice = pb::message("eve.seasonalevent.challenge.ChallengeProgressed")
        .set("character.sequential", char_id as u32)
        .set("progress.challenge.sequential", challenge_id)
        .set("progress.amount", amount)
        .must_to_any();

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    gateway
        .publish_notice(scope, notice, now)
        .map(|(_, count)| count)
}

/// Encodes a security status float (-10.0 to +10.0) into `sint64 units = 1` and `sint32 nanos = 2`.
pub fn encode_security_status_value(sec_status: f64) -> Vec<u8> {
    let units = sec_status.trunc() as i64;
    let fract = sec_status.fract();
    let nanos = (fract * 1_000_000_000.0).round() as i32;
    pb::message("eve_public.securitystatus.Value")
        .set("units", units)
        .when(nanos != 0, |m| m.set("nanos", nanos))
        .must_encode()
}

/// Decodes an `eve_public.securitystatus.Value` / `eve.securitystatus.Value` wire message.
pub fn decode_security_status_value(bytes: &[u8]) -> Result<f64, QuasarError> {
    let wm = WireMessage::decode(bytes)?;
    let mut units = 0i64;
    let mut nanos = 0i32;
    for f in &wm.fields {
        match f.field_number {
            1 => {
                if let Some(v) = f.value.as_varint() {
                    units = decode_zigzag64(v);
                }
            }
            2 => {
                if let Some(v) = f.value.as_varint() {
                    nanos = decode_zigzag32(v as u32);
                }
            }
            _ => {}
        }
    }
    Ok(units as f64 + (nanos as f64 / 1_000_000_000.0))
}

/// Encodes an `eve.school.Identifier` (`uint32 sequential = 1`).
pub fn encode_school_identifier(school_id: u64) -> Vec<u8> {
    pb::message("eve.school.Identifier")
        .set("sequential", school_id as u32)
        .must_encode()
}

/// Encodes a localized `eve.search.Tag` (`LocalizedKeyValue localized = 1`).
pub fn encode_localized_search_tag(key: &str, lang: u32, val: &str) -> Vec<u8> {
    pb::message("eve.search.Tag")
        .set("localized.key", key)
        .set("localized.language", lang as i32)
        .set("localized.value", val)
        .must_encode()
}

/// Encodes an unlocalized `eve.search.Tag` (`KeyValue unlocalized = 2`).
pub fn encode_unlocalized_search_tag(key: &str, val: &str) -> Vec<u8> {
    pb::message("eve.search.Tag")
        .set("unlocalized.key", key)
        .set("unlocalized.string_value", val)
        .must_encode()
}

/// Encodes an `eve_public.ship.Identifier` or `eve.ship.Identifier` (`uint32 sequential = 1`).
pub fn encode_ship_identifier(ship_id: u64) -> Vec<u8> {
    pb::message("eve.ship.Identifier")
        .set("sequential", ship_id)
        .must_encode()
}

/// Encodes an `eve.ship.Attributes` message (`type = 1`, `owner = 4`).
pub fn encode_ship_attributes(type_id: u64, owner_id: u64) -> Vec<u8> {
    pb::message("eve.ship.Attributes")
        .set("type.sequential", type_id)
        .set("owner.player.character.sequential", owner_id as u32)
        .must_encode()
}

#[cfg(test)]
#[path = "quasar_debug_race_ship_services_tests.rs"]
mod quasar_debug_race_ship_services_tests;
#[cfg(test)]
#[path = "quasar_debug_race_ship_services_edges_tests.rs"]
mod quasar_debug_race_ship_services_edges_tests;
