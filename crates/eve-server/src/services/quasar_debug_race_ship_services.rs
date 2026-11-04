//! `quasar_debug_race_ship_services` — Quasar Debug, Core Quasar Admin/Routes,
//! Race, Raffle, School, Search, SecurityStatus, and Ship domain services.

#![forbid(unsafe_code)]

use crate::quasar_gateway::{
    require_admin_role, NoticeScope, PublicGatewayRouter,
};
use eve_db::WorldRepository;
use eve_proto::quasar::{
    decode_zigzag32, decode_zigzag64, encode_bytes_field, encode_string_field, encode_varint_field,
    encode_zigzag32, encode_zigzag64, DynamicValue, GoogleAny, QuasarError, TYPE_GOOGLEAPIS_PREFIX,
    WireMessage,
};
use eve_proto::BridgeRpcError;
use eve_sde::SdeRegistry;
use std::sync::Arc;

fn any(type_name: &str, value: Vec<u8>) -> GoogleAny {
    GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"),
        value,
    }
}

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
            let mut out = Vec::new();
            encode_bytes_field(1, &req_data, &mut out);
            Ok(any("eve_public.debug.Response", out))
        }),
    );

    // 2. eve.quasar.admin.IsAuthorizedRequest -> eve.quasar.admin.IsAuthorizedResponse
    router.register_handler(
        "eve.quasar.admin.IsAuthorizedRequest",
        Arc::new(move |ctx, _any, _dyn_msg| {
            let is_authorized = require_admin_role(ctx).is_ok() || ctx.effective_user_id().is_some();
            let mut out = Vec::new();
            encode_varint_field(1, if is_authorized { 1 } else { 0 }, &mut out);
            Ok(any("eve.quasar.admin.IsAuthorizedResponse", out))
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
            let mut out = Vec::new();
            let mut races: Vec<_> = sde_race.races.values().collect();
            races.sort_by_key(|r| r.race_id);
            for race in races {
                if playable_only && !matches!(race.race_id, 1 | 2 | 4 | 8) {
                    continue;
                }
                let mut id_sub = Vec::new();
                encode_varint_field(1, race.race_id as u64, &mut id_sub);

                let mut attr_sub = Vec::new();
                encode_string_field(1, &race.name, &mut attr_sub);
                encode_string_field(2, &race.description, &mut attr_sub);
                if let Some(corvette) = race.corvette_type_id {
                    let mut ship_id_sub = Vec::new();
                    encode_varint_field(1, corvette as u64, &mut ship_id_sub);
                    encode_bytes_field(3, &ship_id_sub, &mut attr_sub);
                }

                let mut race_sub = Vec::new();
                encode_bytes_field(1, &id_sub, &mut race_sub);
                encode_bytes_field(2, &attr_sub, &mut race_sub);

                encode_bytes_field(1, &race_sub, &mut out);
            }
            Ok(any("eve.race.GetAllResponse", out))
        }),
    );
}

/// Publishes an `eve_public.debug.Notice` to the specified gateway scope.
pub fn publish_debug_notice(
    gateway: &PublicGatewayRouter,
    scope: NoticeScope,
    uuid: &[u8],
) -> Result<usize, BridgeRpcError> {
    let mut out = Vec::new();
    encode_bytes_field(1, uuid, &mut out);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    gateway
        .publish_notice(scope, any("eve_public.debug.Notice", out), now)
        .map(|(_, count)| count)
}

/// Publishes an `eve.debug.monolith.event.EventEmitted` notice to the specified gateway scope.
pub fn publish_monolith_event_notice(
    gateway: &PublicGatewayRouter,
    scope: NoticeScope,
    tracer: &[u8],
) -> Result<usize, BridgeRpcError> {
    let mut out = Vec::new();
    encode_bytes_field(1, tracer, &mut out);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    gateway
        .publish_notice(scope, any("eve.debug.monolith.event.EventEmitted", out), now)
        .map(|(_, count)| count)
}

/// Publishes an `eve.quasar.CharacterRouteEstablished` notice to the specified gateway scope.
pub fn publish_character_route_established(
    gateway: &PublicGatewayRouter,
    scope: NoticeScope,
    character_id: u64,
) -> Result<usize, BridgeRpcError> {
    let mut id_sub = Vec::new();
    encode_varint_field(1, character_id, &mut id_sub);
    let mut out = Vec::new();
    encode_bytes_field(1, &id_sub, &mut out);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    gateway
        .publish_notice(scope, any("eve.quasar.CharacterRouteEstablished", out), now)
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
    let mut char_sub = Vec::new();
    encode_varint_field(1, char_id, &mut char_sub);
    let mut item_sub = Vec::new();
    encode_varint_field(1, item_id, &mut item_sub);
    let mut st_sub = Vec::new();
    encode_varint_field(1, station_id, &mut st_sub);

    let mut out = Vec::new();
    encode_bytes_field(1, &char_sub, &mut out);
    encode_varint_field(2, raffle_id, &mut out);
    encode_varint_field(3, ticket_count, &mut out);
    encode_varint_field(4, ticket_price_cents, &mut out);
    encode_bytes_field(5, &item_sub, &mut out);
    encode_bytes_field(6, &st_sub, &mut out);
    encode_varint_field(7, if is_private { 1 } else { 0 }, &mut out);

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    gateway
        .publish_notice(scope, any("eve.raffle.ItemRaffleCreated", out), now)
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
    let mut id_sub = Vec::new();
    encode_varint_field(1, char_id, &mut id_sub);
    let mut out = Vec::new();
    encode_varint_field(1, raffle_id, &mut out);
    encode_varint_field(2, ticket_number, &mut out);
    encode_bytes_field(3, &id_sub, &mut out);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    gateway
        .publish_notice(scope, any("eve.raffle.TicketBought", out), now)
        .map(|(_, count)| count)
}

/// Publishes an `eve.raffle.Finished` notice.
pub fn publish_raffle_finished_notice(
    gateway: &PublicGatewayRouter,
    scope: NoticeScope,
    raffle_id: u64,
    winner_char_id: u64,
) -> Result<usize, BridgeRpcError> {
    let mut winner_sub = Vec::new();
    encode_varint_field(1, winner_char_id, &mut winner_sub);
    let mut out = Vec::new();
    encode_varint_field(1, raffle_id, &mut out);
    encode_bytes_field(2, &winner_sub, &mut out);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    gateway
        .publish_notice(scope, any("eve.raffle.Finished", out), now)
        .map(|(_, count)| count)
}

/// Publishes an `eve.raffle.Expired` notice.
pub fn publish_raffle_expired_notice(
    gateway: &PublicGatewayRouter,
    scope: NoticeScope,
    raffle_id: u64,
) -> Result<usize, BridgeRpcError> {
    let mut out = Vec::new();
    encode_varint_field(1, raffle_id, &mut out);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    gateway
        .publish_notice(scope, any("eve.raffle.Expired", out), now)
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
    let mut char_sub = Vec::new();
    encode_varint_field(1, char_id, &mut char_sub);

    let mut chal_id = Vec::new();
    encode_varint_field(1, challenge_id, &mut chal_id);

    let mut prog_sub = Vec::new();
    encode_bytes_field(1, &chal_id, &mut prog_sub);
    encode_varint_field(2, amount, &mut prog_sub);

    let mut out = Vec::new();
    encode_bytes_field(1, &char_sub, &mut out);
    encode_bytes_field(2, &prog_sub, &mut out);

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    gateway
        .publish_notice(
            scope,
            any("eve.seasonalevent.challenge.ChallengeProgressed", out),
            now,
        )
        .map(|(_, count)| count)
}

/// Encodes a security status float (-10.0 to +10.0) into `sint64 units = 1` and `sint32 nanos = 2`.
pub fn encode_security_status_value(sec_status: f64) -> Vec<u8> {
    let units = sec_status.trunc() as i64;
    let fract = sec_status.fract();
    let nanos = (fract * 1_000_000_000.0).round() as i32;
    let mut out = Vec::new();
    encode_varint_field(1, encode_zigzag64(units), &mut out);
    if nanos != 0 {
        encode_varint_field(2, encode_zigzag32(nanos) as u64, &mut out);
    }
    out
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
    let mut out = Vec::new();
    encode_varint_field(1, school_id, &mut out);
    out
}

/// Encodes a localized `eve.search.Tag` (`LocalizedKeyValue localized = 1`).
pub fn encode_localized_search_tag(key: &str, lang: u32, val: &str) -> Vec<u8> {
    let mut lkv = Vec::new();
    encode_string_field(1, key, &mut lkv);
    encode_varint_field(2, lang as u64, &mut lkv);
    encode_string_field(3, val, &mut lkv);
    let mut out = Vec::new();
    encode_bytes_field(1, &lkv, &mut out);
    out
}

/// Encodes an unlocalized `eve.search.Tag` (`KeyValue unlocalized = 2`).
pub fn encode_unlocalized_search_tag(key: &str, val: &str) -> Vec<u8> {
    let mut ukv = Vec::new();
    encode_string_field(1, key, &mut ukv);
    encode_string_field(5, val, &mut ukv); // string_value = 5
    let mut out = Vec::new();
    encode_bytes_field(2, &ukv, &mut out);
    out
}

/// Encodes an `eve_public.ship.Identifier` or `eve.ship.Identifier` (`uint32 sequential = 1`).
pub fn encode_ship_identifier(ship_id: u64) -> Vec<u8> {
    let mut out = Vec::new();
    encode_varint_field(1, ship_id, &mut out);
    out
}

/// Encodes an `eve.ship.Attributes` message (`type = 1`, `owner = 4`).
pub fn encode_ship_attributes(type_id: u64, owner_id: u64) -> Vec<u8> {
    let mut type_sub = Vec::new();
    encode_varint_field(1, type_id, &mut type_sub);

    let mut char_id_sub = Vec::new();
    encode_varint_field(1, owner_id, &mut char_id_sub);

    let mut player_sub = Vec::new();
    encode_bytes_field(1, &char_id_sub, &mut player_sub);

    let mut owner_sub = Vec::new();
    encode_bytes_field(1, &player_sub, &mut owner_sub);

    let mut out = Vec::new();
    encode_bytes_field(1, &type_sub, &mut out);
    encode_bytes_field(4, &owner_sub, &mut out);
    out
}
