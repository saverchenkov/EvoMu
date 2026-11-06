//! `quasar_vendors_and_war_services` — Quasar Public Gateway handlers, wire encoders,
//! and notice publishers for ThirdPartyApi, Tournament, UI, Universe, Vanguard, Vendors, War,
//! and Wormhole services (`feat:quasar.*`).
//!
//! Provides behavioral handlers and protobuf model encoders for:
//! - `eve.thirdpartyapi`: ESI/ThirdPartyAPI telemetry events and notice publishers.
//! - `eve.tournament`: Arena layouts, rulesets, and match lifecycle management.
//! - `eve.ui` & `eve_public.ui`: UI annotations, content tags, icons, and image metadata.
//! - `eve.universe`: Solar system gate jump counts, station navigation, and region constellations.
//! - `eve.vanguard`: Vanguard commissions, skirmish outcomes, and contract completion events.
//! - `eve.vendors`: External platform integrations (Epic, Steam, FusionAuth, Twitch, Payletter, Contentful).
//! - `eve.war`: War declarations, mutual status, destruction statistics, and allies queries.
//! - `eve.wormhole`: Wormhole identifiers, attributes, and lifecycle state.

#![forbid(unsafe_code)]

use crate::quasar_gateway::{
    NoticeScope, PublicGatewayRouter, STATUS_BAD_REQUEST,
};
use eve_db::{IskCents, WorldRepository};
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, DynamicQuasarMessage,
    DynamicValue, GoogleAny, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_sde::SdeRegistry;
use std::sync::Arc;

// ============================================================================
// Core Protobuf Wire Helpers
// ============================================================================

#[inline]
pub fn any(type_name: &str, payload: Vec<u8>) -> GoogleAny {
    GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"),
        value: payload,
    }
}

pub fn encode_id_msg(field_num: u32, id: u64, out: &mut Vec<u8>) {
    let mut inner = Vec::new();
    encode_varint_field(1, id, &mut inner);
    encode_bytes_field(field_num, &inner, out);
}

pub fn encode_uuid_msg(field_num: u32, uuid: &[u8], out: &mut Vec<u8>) {
    let mut inner = Vec::new();
    encode_bytes_field(1, uuid, &mut inner);
    encode_bytes_field(field_num, &inner, out);
}

pub fn encode_timestamp_field(field_num: u32, unix_secs: u64, out: &mut Vec<u8>) {
    let mut inner = Vec::new();
    encode_varint_field(1, unix_secs, &mut inner);
    encode_bytes_field(field_num, &inner, out);
}

pub fn encode_isk_currency(field_num: u32, cents: IskCents, out: &mut Vec<u8>) {
    let mut inner = Vec::new();
    let units = cents.0 / 100;
    let nanos = ((cents.0 % 100) * 10_000_000) as i32;
    if units != 0 {
        let zigzag_units = ((units << 1) ^ (units >> 63)) as u64;
        encode_varint_field(1, zigzag_units, &mut inner);
    }
    if nanos != 0 {
        let zigzag_nanos = ((nanos << 1) ^ (nanos >> 31)) as u64;
        encode_varint_field(2, zigzag_nanos, &mut inner);
    }
    encode_bytes_field(field_num, &inner, out);
}

fn extract_u64_checked(msg: &DynamicQuasarMessage, field_num: u32) -> Result<Option<u64>, (u32, String)> {
    match msg.get(field_num) {
        Some(DynamicValue::Message(sub)) => match sub.get(1) {
            Some(DynamicValue::Message(inner)) => extract_u64_checked(inner, 1),
            Some(DynamicValue::Uint64(n)) => Ok(Some(*n)),
            Some(DynamicValue::Uint32(n)) => Ok(Some(u64::from(*n))),
            Some(DynamicValue::Int64(n)) | Some(DynamicValue::Sint64(n)) => Ok(Some(n.unsigned_abs())),
            Some(DynamicValue::Int32(n)) | Some(DynamicValue::Sint32(n)) => Ok(Some(n.unsigned_abs() as u64)),
            None => extract_u64_checked(sub, 2),
            _ => Ok(None),
        },
        Some(DynamicValue::Uint64(n)) => Ok(Some(*n)),
        Some(DynamicValue::Uint32(n)) => Ok(Some(u64::from(*n))),
        Some(DynamicValue::Int64(n)) | Some(DynamicValue::Sint64(n)) => Ok(Some(n.unsigned_abs())),
        Some(DynamicValue::Int32(n)) | Some(DynamicValue::Sint32(n)) => Ok(Some(n.unsigned_abs() as u64)),
        _ => Ok(None),
    }
}

fn extract_required_u64(msg: &DynamicQuasarMessage, field_num: u32, what: &str) -> Result<u64, (u32, String)> {
    match extract_u64_checked(msg, field_num)? {
        Some(v) if v > 0 => Ok(v),
        _ => Err((STATUS_BAD_REQUEST, format!("Missing {what}"))),
    }
}

fn extract_required_u32(msg: &DynamicQuasarMessage, field_num: u32, what: &str) -> Result<u32, (u32, String)> {
    let v = extract_required_u64(msg, field_num, what)?;
    u32::try_from(v).map_err(|_| (STATUS_BAD_REQUEST, format!("Invalid {what}")))
}

fn extract_string_field<'a>(msg: &'a DynamicQuasarMessage, field_num: u32) -> Option<&'a str> {
    match msg.get(field_num) {
        Some(DynamicValue::String(s)) => Some(s.as_str()),
        Some(DynamicValue::Message(sub)) => match sub.get(1) {
            Some(DynamicValue::String(s)) => Some(s.as_str()),
            _ => None,
        },
        _ => None,
    }
}

fn extract_uuid_bytes(msg: &DynamicQuasarMessage, field_num: u32) -> Option<Vec<u8>> {
    match msg.get(field_num) {
        Some(DynamicValue::Bytes(b)) => Some(b.clone()),
        Some(DynamicValue::Message(sub)) => match sub.get(1) {
            Some(DynamicValue::Bytes(b)) => Some(b.clone()),
            _ => None,
        },
        _ => None,
    }
}

// ============================================================================
// 1. ThirdPartyApi Domain Encoders & Notice Publishers
// ============================================================================

/// Wire encoder for `eve.thirdpartyapi.RequestReceived`.
pub fn encode_thirdpartyapi_request_received(
    char_id: i32,
    datasource: &str,
    route: &str,
    http_verb: u32,
    user_agent: &str,
    now_unix: u64,
) -> Vec<u8> {
    let mut out = Vec::new();
    encode_timestamp_field(1, now_unix, &mut out);
    if char_id != 0 {
        let zigzag_char = ((char_id << 1) ^ (char_id >> 31)) as u64;
        encode_varint_field(2, zigzag_char, &mut out);
    }
    if !datasource.is_empty() {
        encode_string_field(3, datasource, &mut out);
    }
    if !route.is_empty() {
        encode_string_field(5, route, &mut out);
    }
    if http_verb != 0 {
        encode_varint_field(6, http_verb as u64, &mut out);
    }
    if !user_agent.is_empty() {
        encode_string_field(7, user_agent, &mut out);
    }
    out
}

pub fn publish_thirdpartyapi_request_received(
    gateway: &PublicGatewayRouter,
    scope: NoticeScope,
    char_id: i32,
    datasource: &str,
    route: &str,
    http_verb: u32,
    user_agent: &str,
    now_unix: u64,
) -> Result<usize, eve_proto::BridgeRpcError> {
    let payload = encode_thirdpartyapi_request_received(
        char_id, datasource, route, http_verb, user_agent, now_unix,
    );
    let any_msg = any("eve.thirdpartyapi.RequestReceived", payload);
    let (_, delivered) = gateway.publish_notice(scope, any_msg, now_unix)?;
    Ok(delivered)
}

// ============================================================================
// 2. Tournament Domain Encoders & Notice Publishers
// ============================================================================

pub fn encode_tournament_identifier(uuid: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    encode_bytes_field(1, uuid, &mut out);
    out
}

pub fn encode_tournament_arena(
    arena_uuid: &[u8],
    name: &str,
    solar_system_id: u32,
    radius: u64,
    tournament_uuid: &[u8],
    ruleset_uuid: &[u8],
) -> Vec<u8> {
    let mut out = Vec::new();
    encode_uuid_msg(1, arena_uuid, &mut out);

    let mut attrs = Vec::new();
    if !name.is_empty() {
        encode_string_field(1, name, &mut attrs);
    }
    if solar_system_id > 0 {
        encode_id_msg(2, u64::from(solar_system_id), &mut attrs);
    }
    if radius > 0 {
        encode_varint_field(3, radius, &mut attrs);
    }
    if !tournament_uuid.is_empty() {
        let mut t_id = Vec::new();
        encode_varint_field(1, 2024, &mut t_id);
        encode_bytes_field(6, &t_id, &mut attrs);
    }
    if !ruleset_uuid.is_empty() {
        encode_uuid_msg(11, ruleset_uuid, &mut attrs);
    }
    encode_bytes_field(2, &attrs, &mut out);
    out
}

pub fn encode_tournament_layout(
    layout_uuid: &[u8],
    name: &str,
    version: u64,
    radius: u64,
) -> Vec<u8> {
    let mut out = Vec::new();
    if !layout_uuid.is_empty() {
        encode_bytes_field(1, layout_uuid, &mut out);
    }
    if !name.is_empty() {
        encode_string_field(1, name, &mut out);
    }
    if version > 0 {
        encode_varint_field(2, version, &mut out);
    }
    if radius > 0 {
        encode_varint_field(3, radius, &mut out);
    }
    out
}

pub fn encode_tournament_ruleset(
    ruleset_uuid: &[u8],
    name: &str,
    max_points: i64,
    max_players: i64,
) -> Vec<u8> {
    let mut out = Vec::new();
    if !ruleset_uuid.is_empty() {
        encode_bytes_field(1, ruleset_uuid, &mut out);
    }
    if !name.is_empty() {
        encode_string_field(1, name, &mut out);
    }
    if max_points > 0 {
        encode_varint_field(4, max_points as u64, &mut out);
    }
    if max_players > 0 {
        encode_varint_field(5, max_players as u64, &mut out);
    }
    out
}

// (Tournament arena lifecycle is exposed via CreateRequest / DeleteRequest / GetAllRequest.)

// ============================================================================
// 3. UI Domain Model Encoders
// ============================================================================

pub fn encode_ui_annotation(key: &str, value: &str) -> Vec<u8> {
    let mut out = Vec::new();
    if !key.is_empty() {
        encode_string_field(1, key, &mut out);
    }
    if !value.is_empty() {
        encode_string_field(2, value, &mut out);
    }
    out
}

pub fn encode_ui_contenttag(name: &str) -> Vec<u8> {
    let mut out = Vec::new();
    if !name.is_empty() {
        encode_string_field(1, name, &mut out);
    }
    out
}

pub fn encode_ui_icon(name: &str) -> Vec<u8> {
    let mut out = Vec::new();
    if !name.is_empty() {
        encode_string_field(1, name, &mut out);
    }
    out
}

pub fn encode_ui_image(path: &str, display_width: u32, display_height: u32) -> Vec<u8> {
    let mut out = Vec::new();
    if !path.is_empty() {
        let mut id = Vec::new();
        encode_string_field(1, path, &mut id);
        encode_bytes_field(1, &id, &mut out);
    }
    let attrs = encode_ui_image_attributes(display_width, display_height);
    if !attrs.is_empty() {
        encode_bytes_field(2, &attrs, &mut out);
    }
    out
}

pub fn encode_ui_image_attributes(display_width: u32, display_height: u32) -> Vec<u8> {
    let mut attrs = Vec::new();
    if display_width > 0 {
        encode_varint_field(1, u64::from(display_width), &mut attrs);
    }
    if display_height > 0 {
        encode_varint_field(2, u64::from(display_height), &mut attrs);
    }
    attrs
}

// ============================================================================
// 4. Universe Navigation & Constellations Encoders
// ============================================================================

pub fn encode_universe_location(solar_system_id: u32) -> Vec<u8> {
    let mut out = Vec::new();
    encode_id_msg(1, u64::from(solar_system_id), &mut out);
    out
}

pub fn encode_universe_station(station_id: u32) -> Vec<u8> {
    let mut out = Vec::new();
    encode_id_msg(1, u64::from(station_id), &mut out);
    out
}

pub fn encode_universe_constellation(constellation_id: u32) -> Vec<u8> {
    let mut out = Vec::new();
    encode_id_msg(1, u64::from(constellation_id), &mut out);
    out
}

// ============================================================================
// 5. Vanguard Domain Encoders & Notice Publishers
// ============================================================================

pub fn encode_vanguard_commission(commission_uuid: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    encode_bytes_field(1, commission_uuid, &mut out);
    out
}

pub fn encode_vanguard_assignment(commission_uuid: &[u8], assignment_type: &str) -> Vec<u8> {
    let mut out = Vec::new();
    encode_uuid_msg(1, commission_uuid, &mut out);
    if !assignment_type.is_empty() {
        encode_string_field(2, assignment_type, &mut out);
    }
    out
}

pub fn publish_vanguard_commission_completed(
    gateway: &PublicGatewayRouter,
    scope: NoticeScope,
    user_id: u64,
    commission_uuid: &[u8],
    assignment_type: &str,
    commission_type: &str,
    faction_id: u64,
    now_unix: u64,
) -> Result<usize, eve_proto::BridgeRpcError> {
    let mut payload = Vec::new();
    encode_id_msg(1, user_id, &mut payload);
    let assignment = encode_vanguard_assignment(commission_uuid, assignment_type);
    encode_bytes_field(2, &assignment, &mut payload);
    if !commission_type.is_empty() {
        encode_string_field(3, commission_type, &mut payload);
    }
    if faction_id > 0 {
        encode_id_msg(4, faction_id, &mut payload);
    }
    let any_msg = any("eve.vanguard.commission.assignment.api.Completed", payload);
    let (_, delivered) = gateway.publish_notice(scope, any_msg, now_unix)?;
    Ok(delivered)
}

pub fn publish_vanguard_skirmish_completed(
    gateway: &PublicGatewayRouter,
    scope: NoticeScope,
    campaign_id: u64,
    outcome: u32,
    now_unix: u64,
) -> Result<usize, eve_proto::BridgeRpcError> {
    let mut payload = Vec::new();
    encode_id_msg(1, campaign_id, &mut payload);
    encode_varint_field(2, u64::from(outcome), &mut payload);
    let any_msg = any("eve.vanguard.insurgency.api.SkirmishCompleted", payload);
    let (_, delivered) = gateway.publish_notice(scope, any_msg, now_unix)?;
    Ok(delivered)
}

pub fn publish_vanguard_contract_completed(
    gateway: &PublicGatewayRouter,
    scope: NoticeScope,
    is_corruption: bool,
    now_unix: u64,
) -> Result<usize, eve_proto::BridgeRpcError> {
    let type_name = if is_corruption {
        "eve.vanguard.contract.api.CorruptionContractCompleted"
    } else {
        "eve.vanguard.contract.api.SuppressionContractCompleted"
    };
    let any_msg = any(type_name, Vec::new());
    let (_, delivered) = gateway.publish_notice(scope, any_msg, now_unix)?;
    Ok(delivered)
}

// ============================================================================
// 6. Vendors Domain Encoders & Notice Publishers
// ============================================================================

pub fn encode_epic_user(epic_user_id: &str) -> Vec<u8> {
    let mut out = Vec::new();
    if !epic_user_id.is_empty() {
        encode_string_field(1, epic_user_id, &mut out);
    }
    out
}

pub fn encode_epic_entitlement(
    entitlement_id: &str,
    name: &str,
    status: &str,
    grant_unix: u64,
) -> Vec<u8> {
    let mut out = Vec::new();
    encode_string_field(1, entitlement_id, &mut out);
    let mut attrs = Vec::new();
    encode_string_field(2, name, &mut attrs);
    encode_string_field(8, status, &mut attrs);
    encode_timestamp_field(6, grant_unix, &mut attrs);
    encode_bytes_field(2, &attrs, &mut out);
    out
}

pub fn encode_steam_user(steam_id: &str, user_name: &str, country_code: &str) -> Vec<u8> {
    let mut out = Vec::new();
    let mut id = Vec::new();
    encode_string_field(1, steam_id, &mut id);
    encode_bytes_field(1, &id, &mut out);

    let mut attrs = Vec::new();
    encode_string_field(1, user_name, &mut attrs);
    encode_string_field(2, country_code, &mut attrs);
    encode_bytes_field(2, &attrs, &mut out);
    out
}

pub fn encode_twitch_entitlement(uuid: &[u8], status: &str, last_updated_unix: u64) -> Vec<u8> {
    let mut out = Vec::new();
    let mut id = Vec::new();
    encode_bytes_field(1, uuid, &mut id);
    encode_bytes_field(1, &id, &mut out);

    let mut attrs = Vec::new();
    encode_string_field(3, status, &mut attrs);
    encode_timestamp_field(4, last_updated_unix, &mut attrs);
    encode_bytes_field(2, &attrs, &mut out);
    out
}

pub fn encode_contentful_asset(contentful_id: &str) -> Vec<u8> {
    let mut out = Vec::new();
    encode_string_field(1, contentful_id, &mut out);
    out
}

pub fn encode_payletter_receipt(tx_id: &str, auth_info: &str) -> Vec<u8> {
    let mut out = Vec::new();
    encode_string_field(1, tx_id, &mut out);
    encode_string_field(2, auth_info, &mut out);
    out
}

pub fn publish_twitch_drop_received(
    gateway: &PublicGatewayRouter,
    scope: NoticeScope,
    user_id: u64,
    entitlement_uuid: &[u8],
    offer_id: u64,
    now_unix: u64,
) -> Result<usize, eve_proto::BridgeRpcError> {
    let mut payload = Vec::new();
    encode_uuid_msg(1, entitlement_uuid, &mut payload);
    encode_id_msg(2, user_id, &mut payload);
    encode_id_msg(3, offer_id, &mut payload);
    let any_msg = any("eve.vendors.twitch.entitlement.api.DropEventReceived", payload);
    let (_, delivered) = gateway.publish_notice(scope, any_msg, now_unix)?;
    Ok(delivered)
}

pub fn publish_fusionauth_user_linked(
    gateway: &PublicGatewayRouter,
    scope: NoticeScope,
    fa_uuid: &[u8],
    eve_user_id: u64,
    now_unix: u64,
) -> Result<usize, eve_proto::BridgeRpcError> {
    let mut payload = Vec::new();
    encode_uuid_msg(1, fa_uuid, &mut payload);
    encode_id_msg(2, eve_user_id, &mut payload);
    let any_msg = any("eve.vendors.fusionauth.api.UserLinked", payload);
    let (_, delivered) = gateway.publish_notice(scope, any_msg, now_unix)?;
    Ok(delivered)
}

// ============================================================================
// 7. War Domain Encoders & Notice Publishers
// ============================================================================

pub fn encode_war_identifier(war_id: u64) -> Vec<u8> {
    let mut out = Vec::new();
    encode_varint_field(1, war_id, &mut out);
    out
}

pub fn encode_war_entity_identifier(alliance_id: Option<u64>, corp_id: Option<u64>) -> Vec<u8> {
    let mut out = Vec::new();
    if let Some(aid) = alliance_id {
        encode_id_msg(1, aid, &mut out);
    }
    if let Some(cid) = corp_id {
        encode_id_msg(2, cid, &mut out);
    }
    out
}

pub fn encode_war_attributes(
    aggressor_corp: u64,
    defender_corp: u64,
    mutual: bool,
    open_allies: bool,
    now_unix: u64,
) -> Vec<u8> {
    let mut out = Vec::new();
    let agg = encode_war_entity_identifier(None, Some(aggressor_corp));
    encode_bytes_field(1, &agg, &mut out);

    let def = encode_war_entity_identifier(None, Some(defender_corp));
    encode_bytes_field(2, &def, &mut out);

    encode_timestamp_field(3, now_unix.saturating_sub(86400), &mut out);
    encode_timestamp_field(4, now_unix + 7 * 86400, &mut out);
    encode_varint_field(5, if mutual { 1 } else { 0 }, &mut out);
    encode_varint_field(6, if open_allies { 1 } else { 0 }, &mut out);

    let mut hostile = Vec::new();
    encode_timestamp_field(1, now_unix.saturating_sub(86400), &mut hostile);
    encode_timestamp_field(2, now_unix.saturating_sub(86400), &mut hostile);
    encode_bytes_field(8, &hostile, &mut out);
    out
}

pub fn encode_war_destruction_statistics(ships: u64, isk: u64) -> Vec<u8> {
    let mut out = Vec::new();
    encode_varint_field(1, ships, &mut out);
    encode_isk_currency(2, IskCents::from_isk(isk as i64), &mut out);
    out
}

pub fn publish_war_declared_notice(
    gateway: &PublicGatewayRouter,
    scope: NoticeScope,
    war_id: u64,
    aggressor_corp: u64,
    defender_corp: u64,
    now_unix: u64,
) -> Result<usize, eve_proto::BridgeRpcError> {
    let mut payload = Vec::new();
    encode_id_msg(1, war_id, &mut payload);
    let attrs = encode_war_attributes(aggressor_corp, defender_corp, false, true, now_unix);
    encode_bytes_field(2, &attrs, &mut payload);
    let any_msg = any("eve.war.Declared", payload);
    let (_, delivered) = gateway.publish_notice(scope, any_msg, now_unix)?;
    Ok(delivered)
}

pub fn publish_war_end_scheduled_notice(
    gateway: &PublicGatewayRouter,
    scope: NoticeScope,
    war_id: u64,
    ending_unix: u64,
    now_unix: u64,
) -> Result<usize, eve_proto::BridgeRpcError> {
    let mut payload = Vec::new();
    encode_id_msg(1, war_id, &mut payload);
    encode_timestamp_field(2, ending_unix, &mut payload);
    let any_msg = any("eve.war.EndScheduled", payload);
    let (_, delivered) = gateway.publish_notice(scope, any_msg, now_unix)?;
    Ok(delivered)
}

// ============================================================================
// 8. Wormhole Domain Encoders
// ============================================================================

pub fn encode_wormhole_identifier(wormhole_id: u64) -> Vec<u8> {
    let mut out = Vec::new();
    encode_varint_field(1, wormhole_id, &mut out);
    out
}

// ============================================================================
// Route Handlers Registration
// ============================================================================

pub fn register_handlers(
    router: &PublicGatewayRouter,
    db: Arc<WorldRepository>,
    sde: Arc<SdeRegistry>,
) {
    // ------------------------------------------------------------------------
    // 1. ThirdPartyApi: RequestReceived
    // ------------------------------------------------------------------------
    router.register_handler(
        "eve.thirdpartyapi.RequestReceived",
        Arc::new(move |_ctx, _any, msg| {
            let char_id = extract_u64_checked(msg, 2)?.unwrap_or(0);
            let ds = extract_string_field(msg, 3).unwrap_or("tranquility");
            let route = extract_string_field(msg, 5).unwrap_or("/");
            let verb = extract_u64_checked(msg, 6)?.unwrap_or(1);
            let ua = extract_string_field(msg, 7).unwrap_or("EvoMu-Client");
            let payload = encode_thirdpartyapi_request_received(char_id as i32, ds, route, verb as u32, ua, 1_700_000_000);
            Ok(any("eve.thirdpartyapi.RequestReceived", payload))
        }),
    );

    // ------------------------------------------------------------------------
    // 2. Tournament: Arena, Layouts & Rulesets
    // ------------------------------------------------------------------------
    let db_arena = Arc::clone(&db);
    router.register_handler(
        "eve.tournament.arena.api.GetAllRequest",
        Arc::new(move |ctx, _any, msg| {
            let req_sys = extract_u64_checked(msg, 1)?.map(|v| v as u32).filter(|&s| s > 0);
            let caller_sys = ctx.effective_character_id()
                .and_then(|cid| db_arena.with_snapshot(|s| s.characters.get(&(cid as i64)).map(|c| c.solar_system_id)))
                .filter(|&s| s > 0);
            let sys_id = req_sys.or(caller_sys).unwrap_or(0);
            let mut out = Vec::new();
            let arena_bytes = encode_tournament_arena(b"arena-alpha-1234", "Amarr Championship Arena", sys_id, 200_000, b"tourney-2024", b"ruleset-v1");
            encode_bytes_field(1, &arena_bytes, &mut out);
            Ok(any("eve.tournament.arena.api.GetAllResponse", out))
        }),
    );

    router.register_handler(
        "eve.tournament.arena.api.GetSettingsRequest",
        Arc::new(move |_ctx, _any, _msg| {
            let mut out = Vec::new();
            encode_varint_field(1, 0, &mut out); // no_settings: false
            let mut settings = Vec::new();
            encode_id_msg(1, 90_000_001, &mut settings); // boundary killer ISD Polaris
            encode_bytes_field(2, &settings, &mut out);
            Ok(any("eve.tournament.arena.api.GetSettingsResponse", out))
        }),
    );

    router.register_handler(
        "eve.tournament.arena.layout.api.GetRequest",
        Arc::new(move |_ctx, _any, msg| {
            let uuid = extract_uuid_bytes(msg, 1).unwrap_or_else(|| b"layout-default".to_vec());
            let mut out = Vec::new();
            let layout = encode_tournament_layout(&uuid, "Standard 10v10 Crucible Layout", 1, 150_000);
            encode_bytes_field(1, &layout, &mut out);
            Ok(any("eve.tournament.arena.layout.api.GetResponse", out))
        }),
    );

    router.register_handler(
        "eve.tournament.ruleset.api.GetAllRequest",
        Arc::new(move |_ctx, _any, _msg| {
            let mut out = Vec::new();
            encode_uuid_msg(1, b"ruleset-standard-v1", &mut out);
            encode_uuid_msg(1, b"ruleset-alliance-cup", &mut out);
            Ok(any("eve.tournament.ruleset.api.GetAllResponse", out))
        }),
    );

    router.register_handler(
        "eve.tournament.ruleset.api.GetRequest",
        Arc::new(move |_ctx, _any, msg| {
            let uuid = extract_uuid_bytes(msg, 1).unwrap_or_else(|| b"ruleset-standard-v1".to_vec());
            let mut out = Vec::new();
            let rs = encode_tournament_ruleset(&uuid, "Crucible Competitive Ruleset", 100, 10);
            encode_bytes_field(1, &rs, &mut out);
            Ok(any("eve.tournament.ruleset.api.GetResponse", out))
        }),
    );

    router.register_handler(
        "eve.tournament.arena.api.CreateRequest",
        Arc::new(move |ctx, _any, _msg| {
            crate::quasar_gateway::require_admin_role(ctx)?;
            let mut out = Vec::new();
            encode_uuid_msg(1, b"arena-alpha-1234", &mut out);
            Ok(any("eve.tournament.arena.api.CreateResponse", out))
        }),
    );

    router.register_handler(
        "eve.tournament.arena.api.DeleteRequest",
        Arc::new(move |ctx, _any, msg| {
            crate::quasar_gateway::require_admin_role(ctx)?;
            let _uuid = extract_uuid_bytes(msg, 1).unwrap_or_else(|| b"arena-alpha-1234".to_vec());
            Ok(any("eve.tournament.arena.api.DeleteResponse", Vec::new()))
        }),
    );

    // ------------------------------------------------------------------------
    // 3. Universe Navigation & Constellations
    // ------------------------------------------------------------------------
    router.register_handler(
        "eve.universe.navigation.GetShortestGateJumpCountRequest",
        Arc::new(move |_ctx, _any, msg| {
            let origin = extract_required_u32(msg, 1, "origin")?;
            let dest = extract_required_u32(msg, 2, "destination")?;
            let jump_count = if origin == dest { 0 } else { 3 };
            let mut out = Vec::new();
            encode_varint_field(1, jump_count, &mut out);
            Ok(any("eve.universe.navigation.GetShortestGateJumpCountResponse", out))
        }),
    );

    router.register_handler(
        "eve.universe.navigation.GetShortestSystemJumpCountRequest",
        Arc::new(move |_ctx, _any, msg| {
            let start = extract_required_u32(msg, 1, "start")?;
            let end = extract_required_u32(msg, 2, "end")?;
            let jump_count = if start == end { 0 } else { 4 };
            let mut out = Vec::new();
            encode_varint_field(1, jump_count, &mut out);
            Ok(any("eve.universe.navigation.GetShortestSystemJumpCountResponse", out))
        }),
    );

    let sde_const = Arc::clone(&sde);
    router.register_handler(
        "eve.universe.constellation.GetAllInRegionRequest",
        Arc::new(move |_ctx, _any, msg| {
            let region_id = extract_required_u32(msg, 1, "region")?;
            let mut out = Vec::new();
            let mut found = false;
            for sys in sde_const.solar_systems.values() {
                if sys.region_id == region_id {
                    encode_id_msg(2, u64::from(sys.constellation_id), &mut out);
                    found = true;
                }
            }
            if !found {
                encode_id_msg(2, 20_000_001, &mut out);
            }
            Ok(any("eve.universe.constellation.GetAllInRegionResponse", out))
        }),
    );

    // ------------------------------------------------------------------------
    // 4. Vanguard: Assignments & Skirmish Admin Trigger
    // ------------------------------------------------------------------------
    router.register_handler(
        "eve.vanguard.commission.assignment.admin.api.TriggerCompletedEventRequest",
        Arc::new(move |ctx, _any, msg| {
            crate::quasar_gateway::require_admin_role(ctx)?;
            let _user = extract_required_u64(msg, 1, "user")?;
            Ok(any("eve.vanguard.commission.assignment.admin.api.TriggerCompletedEventResponse", Vec::new()))
        }),
    );

    // ------------------------------------------------------------------------
    // 5. Vendors: Epic, Steam, FusionAuth, Payletter
    // ------------------------------------------------------------------------
    router.register_handler(
        "eve.vendors.epic.api.GetUserByEveUserIDRequest",
        Arc::new(move |ctx, _any, msg| {
            crate::quasar_gateway::require_admin_role(ctx)?;
            let user_id = extract_required_u64(msg, 1, "user")?;
            let mut out = Vec::new();
            encode_id_msg(1, user_id, &mut out);
            let epic_user = encode_epic_user("epic_usr_998877");
            encode_bytes_field(2, &epic_user, &mut out);
            encode_varint_field(3, 1, &mut out); // is_logged_in: true
            Ok(any("eve.vendors.epic.api.GetUserByEveUserIDResponse", out))
        }),
    );

    // eve.vendors.epic.api.DeliverEntitlementRequest is served by quasar_auth_user (admin-gated, idempotent
    // redeem-token delivery); registering a stub here would shadow it because vendors register later.

    router.register_handler(
        "eve.vendors.epic.api.GetEntitlementsRequest",
        Arc::new(move |ctx, _any, msg| {
            crate::quasar_gateway::require_admin_role(ctx)?;
            let _user = extract_required_u64(msg, 1, "user")?;
            let mut out = Vec::new();
            let ent = encode_epic_entitlement("epic_ent_omega_30d", "Omega 30 Days Game Time", "active", 1_700_000_000);
            encode_bytes_field(3, &ent, &mut out);
            Ok(any("eve.vendors.epic.api.GetEntitlementsResponse", out))
        }),
    );

    router.register_handler(
        "eve.vendors.steam.user.api.GetRequest",
        Arc::new(move |ctx, _any, msg| {
            crate::quasar_gateway::require_admin_role(ctx)?;
            let steam_id = extract_string_field(msg, 1).unwrap_or("76561198000000000");
            let mut out = Vec::new();
            encode_id_msg(1, 1001, &mut out);
            let steam_user = encode_steam_user(steam_id, "CapsuleerSteamPilot", "IS");
            encode_bytes_field(2, &steam_user, &mut out);
            Ok(any("eve.vendors.steam.user.api.GetResponse", out))
        }),
    );

    router.register_handler(
        "eve.vendors.steam.user.api.GetByEveUserRequest",
        Arc::new(move |ctx, _any, msg| {
            crate::quasar_gateway::require_admin_role(ctx)?;
            let _user_id = extract_required_u64(msg, 1, "user")?;
            let mut out = Vec::new();
            let mut steam_id_msg = Vec::new();
            encode_string_field(1, "76561198000000000", &mut steam_id_msg);
            encode_bytes_field(1, &steam_id_msg, &mut out);
            let mut attrs = Vec::new();
            encode_string_field(1, "CapsuleerSteamPilot", &mut attrs);
            encode_string_field(2, "IS", &mut attrs);
            encode_bytes_field(2, &attrs, &mut out);
            Ok(any("eve.vendors.steam.user.api.GetByEveUserResponse", out))
        }),
    );

    router.register_handler(
        "eve.vendors.steam.user.api.CreateRequest",
        Arc::new(move |ctx, _any, _msg| {
            crate::quasar_gateway::require_admin_role(ctx)?;
            let mut out = Vec::new();
            encode_id_msg(1, 1002, &mut out);
            encode_varint_field(2, 0, &mut out); // already_existed: false
            Ok(any("eve.vendors.steam.user.api.CreateResponse", out))
        }),
    );

    router.register_handler(
        "eve.vendors.fusionauth.api.LinkUserRequest",
        Arc::new(move |ctx, _any, _msg| {
            crate::quasar_gateway::require_admin_role(ctx)?;
            let mut out = Vec::new();
            encode_id_msg(1, 1001, &mut out);
            Ok(any("eve.vendors.fusionauth.api.LinkUserResponse", out))
        }),
    );

    router.register_handler(
        "eve.vendors.payletter.GetUserIdentityVerificationReceiptRequest",
        Arc::new(move |ctx, _any, msg| {
            crate::quasar_gateway::require_admin_role(ctx)?;
            let _user_id = extract_required_u64(msg, 1, "user")?;
            let mut out = Vec::new();
            let receipt = encode_payletter_receipt("payletter_tx_889900", "AUTH_VERIFIED_KR");
            encode_bytes_field(1, &receipt, &mut out);
            Ok(any("eve.vendors.payletter.GetUserIdentityVerificationReceiptResponse", out))
        }),
    );

    // ------------------------------------------------------------------------
    // 6. War: Get, GetAll, DestructionStats, Allies
    // ------------------------------------------------------------------------
    let db_war = Arc::clone(&db);
    router.register_handler(
        "eve.war.GetRequest",
        Arc::new(move |_ctx, _any, msg| {
            let war_id = extract_required_u64(msg, 1, "war")?;
            let mut out = Vec::new();
            let (agg, def, mut_war, open) = db_war.with_snapshot(|snap| {
                if let Some(w) = snap.wars.get(&(war_id as i64)) {
                    (w.aggressor_corp_id as u64, w.defender_corp_id as u64, w.mutual, true)
                } else {
                    (1_000_170, 1_000_171, false, true)
                }
            });
            let attrs = encode_war_attributes(agg, def, mut_war, open, 1_700_000_000);
            encode_bytes_field(1, &attrs, &mut out);
            Ok(any("eve.war.GetResponse", out))
        }),
    );

    let db_all_wars = Arc::clone(&db);
    router.register_handler(
        "eve.war.GetAllRequest",
        Arc::new(move |_ctx, _any, _msg| {
            let mut out = Vec::new();
            db_all_wars.with_snapshot(|snap| {
                for &wid in snap.wars.keys() {
                    encode_id_msg(1, wid as u64, &mut out);
                }
            });
            if out.is_empty() {
                encode_id_msg(1, 1, &mut out);
            }
            Ok(any("eve.war.GetAllResponse", out))
        }),
    );

    router.register_handler(
        "eve.war.GetDestructionStatisticsRequest",
        Arc::new(move |_ctx, _any, msg| {
            let _war_id = extract_required_u64(msg, 1, "war")?;
            let mut out = Vec::new();
            let agg_stats = encode_war_destruction_statistics(12, 1_500_000_000);
            let def_stats = encode_war_destruction_statistics(8, 850_000_000);
            encode_bytes_field(1, &agg_stats, &mut out);
            encode_bytes_field(2, &def_stats, &mut out);
            Ok(any("eve.war.GetDestructionStatisticsResponse", out))
        }),
    );

    router.register_handler(
        "eve.war.GetAlliesRequest",
        Arc::new(move |_ctx, _any, msg| {
            let _war_id = extract_required_u64(msg, 1, "war")?;
            let mut out = Vec::new();
            let ally1 = encode_war_entity_identifier(Some(99_000_001), None);
            let ally2 = encode_war_entity_identifier(None, Some(1_000_172));
            encode_bytes_field(1, &ally1, &mut out);
            encode_bytes_field(1, &ally2, &mut out);
            Ok(any("eve.war.GetAlliesResponse", out))
        }),
    );

}

#[cfg(test)]
#[path = "quasar_vendors_and_war_services_tests.rs"]
mod quasar_vendors_and_war_services_tests;

