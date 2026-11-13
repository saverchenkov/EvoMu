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
use eve_proto::pb::{self, ProtoBuilder};
use eve_proto::quasar::{
    DynamicQuasarMessage, DynamicValue, GoogleAny, GoogleTimestamp, WireField, WireMessage, WireValue,
    TYPE_GOOGLEAPIS_PREFIX,
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

fn wrap_wire_bytes(field_num: u32, inner: Vec<u8>, out: &mut Vec<u8>) {
    let msg = WireMessage {
        fields: vec![WireField { field_number: field_num, value: WireValue::LengthDelimited(inner) }],
    };
    out.extend_from_slice(&msg.encode());
}

fn ts_unix(unix_secs: u64) -> GoogleTimestamp {
    GoogleTimestamp::from_unix(unix_secs as i64, 0)
}

fn isk_currency_builder(cents: IskCents) -> ProtoBuilder {
    let units = cents.0 / 100;
    let nanos = ((cents.0 % 100) * 10_000_000) as i32;
    pb::message("eve.isk.Currency")
        .when(units != 0, |b| b.set("units", units))
        .when(nanos != 0, |b| b.set("nanos", nanos))
}

pub fn encode_id_msg(field_num: u32, id: u64, out: &mut Vec<u8>) {
    wrap_wire_bytes(field_num, pb::message("eve.solarsystem.Identifier").set("sequential", id).must_encode(), out);
}

pub fn encode_uuid_msg(field_num: u32, uuid: &[u8], out: &mut Vec<u8>) {
    wrap_wire_bytes(field_num, pb::message("eve.tournament.arena.Identifier").set("uuid", uuid).must_encode(), out);
}

pub fn encode_timestamp_field(field_num: u32, unix_secs: u64, out: &mut Vec<u8>) {
    wrap_wire_bytes(
        field_num,
        WireMessage { fields: vec![WireField { field_number: 1, value: WireValue::Varint(unix_secs) }] }.encode(),
        out,
    );
}

pub fn encode_isk_currency(field_num: u32, cents: IskCents, out: &mut Vec<u8>) {
    wrap_wire_bytes(field_num, isk_currency_builder(cents).must_encode(), out);
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
    pb::message("eve.thirdpartyapi.RequestReceived")
        .set("timestamp", ts_unix(now_unix))
        .when(char_id != 0, |b| b.set("character_id", char_id))
        .when(!datasource.is_empty(), |b| b.set("datasource", datasource))
        .when(!route.is_empty(), |b| b.set("route", route))
        .when(http_verb != 0, |b| b.set("http_verb", http_verb as i32))
        .when(!user_agent.is_empty(), |b| b.set("user_agent", user_agent))
        .must_encode()
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
    pb::message("eve.tournament.arena.Identifier").set("uuid", uuid).must_encode()
}

fn tournament_arena_builder(
    arena_uuid: &[u8],
    name: &str,
    solar_system_id: u32,
    radius: u64,
    tournament_uuid: &[u8],
    ruleset_uuid: &[u8],
) -> ProtoBuilder {
    pb::message("eve.tournament.arena.api.GetAllResponse.Arena")
        .set("arena.uuid", arena_uuid)
        .set_with("attributes", |attrs| {
            attrs
                .when(!name.is_empty(), |a| a.set("name", name))
                .when(solar_system_id > 0, |a| a.set("solar_system.sequential", u64::from(solar_system_id)))
                .when(radius > 0, |a| a.set("radius", radius))
                .when(!tournament_uuid.is_empty(), |a| a.set("tournament.sequential", 2024u64))
                .when(!ruleset_uuid.is_empty(), |a| a.set("ruleset.uuid", ruleset_uuid))
        })
}

pub fn encode_tournament_arena(
    arena_uuid: &[u8],
    name: &str,
    solar_system_id: u32,
    radius: u64,
    tournament_uuid: &[u8],
    ruleset_uuid: &[u8],
) -> Vec<u8> {
    tournament_arena_builder(arena_uuid, name, solar_system_id, radius, tournament_uuid, ruleset_uuid).must_encode()
}

fn tournament_layout_builder(name: &str, version: u64, radius: u64) -> ProtoBuilder {
    pb::message("eve.tournament.arena.layout.Attributes")
        .when(!name.is_empty(), |b| b.set("name", name))
        .when(version > 0, |b| b.set("version", version))
        .when(radius > 0, |b| b.set("radius", radius))
}

pub fn encode_tournament_layout(_layout_uuid: &[u8], name: &str, version: u64, radius: u64) -> Vec<u8> {
    tournament_layout_builder(name, version, radius).must_encode()
}

fn tournament_ruleset_builder(name: &str, max_points: i64, max_players: i64) -> ProtoBuilder {
    pb::message("eve.tournament.ruleset.Attributes")
        .when(!name.is_empty(), |b| b.set("name", name))
        .when(max_points > 0, |b| b.set("max_points_per_team", max_points))
        .when(max_players > 0, |b| b.set("max_players_per_team", max_players))
}

pub fn encode_tournament_ruleset(
    _ruleset_uuid: &[u8],
    name: &str,
    max_points: i64,
    max_players: i64,
) -> Vec<u8> {
    tournament_ruleset_builder(name, max_points, max_players).must_encode()
}

// (Tournament arena lifecycle is exposed via CreateRequest / DeleteRequest / GetAllRequest.)

// ============================================================================
// 3. UI Domain Model Encoders
// ============================================================================

pub fn encode_ui_annotation(key: &str, value: &str) -> Vec<u8> {
    pb::message("eve.ui.Annotation")
        .when(!key.is_empty(), |b| b.set("key", key))
        .when(!value.is_empty(), |b| b.set("value", value))
        .must_encode()
}

pub fn encode_ui_contenttag(name: &str) -> Vec<u8> {
    pb::message("eve.ui.contenttag.Identifier")
        .when(!name.is_empty(), |b| b.set("name", name))
        .must_encode()
}

pub fn encode_ui_icon(name: &str) -> Vec<u8> {
    pb::message("eve.ui.icon.Identifier")
        .when(!name.is_empty(), |b| b.set("name", name))
        .must_encode()
}

/// Synthetic UI image pair wrapper (no outer `eve.ui.image.Image` message in descriptors;
/// bundles `1: eve.ui.image.Identifier` + `2: eve.ui.image.Attributes`).
pub fn encode_ui_image(path: &str, display_width: u32, display_height: u32) -> Vec<u8> {
    let mut out = Vec::new();
    if !path.is_empty() {
        let id = pb::message("eve.ui.image.Identifier").set("path", path).must_encode();
        wrap_wire_bytes(1, id, &mut out);
    }
    let attrs = encode_ui_image_attributes(display_width, display_height);
    if !attrs.is_empty() {
        wrap_wire_bytes(2, attrs, &mut out);
    }
    out
}

pub fn encode_ui_image_attributes(display_width: u32, display_height: u32) -> Vec<u8> {
    pb::message("eve.ui.image.Attributes")
        .when(display_width > 0, |b| b.set("display_width", display_width))
        .when(display_height > 0, |b| b.set("display_height", display_height))
        .must_encode()
}

// ============================================================================
// 4. Universe Navigation & Constellations Encoders
// ============================================================================

/// Synthetic single-field wrapper around `eve.solarsystem.Identifier` (no outer `eve.universe.Location` in descriptors).
pub fn encode_universe_location(solar_system_id: u32) -> Vec<u8> {
    let mut out = Vec::new();
    wrap_wire_bytes(
        1,
        pb::message("eve.solarsystem.Identifier").set("sequential", u64::from(solar_system_id)).must_encode(),
        &mut out,
    );
    out
}

/// Synthetic single-field wrapper around `eve.station.Identifier`.
pub fn encode_universe_station(station_id: u32) -> Vec<u8> {
    let mut out = Vec::new();
    wrap_wire_bytes(
        1,
        pb::message("eve.station.Identifier").set("sequential", u64::from(station_id)).must_encode(),
        &mut out,
    );
    out
}

/// Synthetic single-field wrapper around `eve.constellation.Identifier`.
pub fn encode_universe_constellation(constellation_id: u32) -> Vec<u8> {
    let mut out = Vec::new();
    wrap_wire_bytes(
        1,
        pb::message("eve.constellation.Identifier").set("sequential", u64::from(constellation_id)).must_encode(),
        &mut out,
    );
    out
}

// ============================================================================
// 5. Vanguard Domain Encoders & Notice Publishers
// ============================================================================

pub fn encode_vanguard_commission(commission_uuid: &[u8]) -> Vec<u8> {
    pb::message("eve.vanguard.commission.Identifier").set("uuid", commission_uuid).must_encode()
}

fn vanguard_assignment_builder(commission_uuid: &[u8], assignment_type: &str) -> ProtoBuilder {
    pb::message("eve.vanguard.commission.assignment.Identifier")
        .set("commission.uuid", commission_uuid)
        .when(!assignment_type.is_empty(), |b| b.set("assignment_type", assignment_type))
}

pub fn encode_vanguard_assignment(commission_uuid: &[u8], assignment_type: &str) -> Vec<u8> {
    vanguard_assignment_builder(commission_uuid, assignment_type).must_encode()
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
    let any_msg = pb::message("eve.vanguard.commission.assignment.api.Completed")
        .set("user.sequential", user_id as i32)
        .set("assignment", vanguard_assignment_builder(commission_uuid, assignment_type))
        .when(!commission_type.is_empty(), |b| b.set("commission_type", commission_type))
        .when(faction_id > 0, |b| b.set("enlisted_faction.sequential", faction_id))
        .must_to_any();
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
    let any_msg = pb::message("eve.vanguard.insurgency.api.SkirmishCompleted")
        .set("campaign.sequential", campaign_id)
        .set("outcome", outcome as i32)
        .must_to_any();
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
    let any_msg = pb::message(type_name).must_to_any();
    let (_, delivered) = gateway.publish_notice(scope, any_msg, now_unix)?;
    Ok(delivered)
}

// ============================================================================
// 6. Vendors Domain Encoders & Notice Publishers
// ============================================================================

pub fn encode_epic_user(epic_user_id: &str) -> Vec<u8> {
    pb::message("eve.vendors.epic.user.Identifier")
        .when(!epic_user_id.is_empty(), |b| b.set("epic_user_id", epic_user_id))
        .must_encode()
}

fn epic_entitlement_builder(
    entitlement_id: &str,
    name: &str,
    status: &str,
    grant_unix: u64,
) -> ProtoBuilder {
    pb::message("eve.vendors.epic.api.GetEntitlementsResponse.Entitlement")
        .set("entitlement.entitlement_id", entitlement_id)
        .set_with("entitlement_attributes", |attrs| {
            attrs
                .set("entitlement_name", name)
                .set("status", status)
                .set("grant_date", ts_unix(grant_unix))
        })
}

pub fn encode_epic_entitlement(
    entitlement_id: &str,
    name: &str,
    status: &str,
    grant_unix: u64,
) -> Vec<u8> {
    epic_entitlement_builder(entitlement_id, name, status, grant_unix).must_encode()
}

pub fn encode_steam_user(steam_id: &str, user_name: &str, country_code: &str) -> Vec<u8> {
    pb::message("eve.vendors.steam.user.api.GetByEveUserResponse")
        .set("steam_user.steam_id", steam_id)
        .set("steam_user_attributes.user_name", user_name)
        .set("steam_user_attributes.country_code", country_code)
        .must_encode()
}

/// Synthetic wrapper around `eve.vendors.twitch.entitlement.{Identifier,Attributes}` (no combined message in descriptors).
pub fn encode_twitch_entitlement(uuid: &[u8], status: &str, last_updated_unix: u64) -> Vec<u8> {
    let mut out = Vec::new();
    let id = pb::message("eve.vendors.twitch.entitlement.Identifier").set("uuid", uuid).must_encode();
    wrap_wire_bytes(1, id, &mut out);
    let attrs = pb::message("eve.vendors.twitch.entitlement.Attributes")
        .set("fulfillment_status", status)
        .set("last_updated", ts_unix(last_updated_unix))
        .must_encode();
    wrap_wire_bytes(2, attrs, &mut out);
    out
}

pub fn encode_contentful_asset(contentful_id: &str) -> Vec<u8> {
    pb::message("eve.vendors.contentful.asset.Identifier")
        .set("contentful_id", contentful_id)
        .must_encode()
}

fn payletter_receipt_builder(tx_id: &str, auth_info: &str) -> ProtoBuilder {
    pb::message("eve.vendors.payletter.IdentityVerificationReceipt")
        .set("transaction_id", tx_id)
        .set("auth_info", auth_info)
}

pub fn encode_payletter_receipt(tx_id: &str, auth_info: &str) -> Vec<u8> {
    payletter_receipt_builder(tx_id, auth_info).must_encode()
}

pub fn publish_twitch_drop_received(
    gateway: &PublicGatewayRouter,
    scope: NoticeScope,
    user_id: u64,
    entitlement_uuid: &[u8],
    offer_id: u64,
    now_unix: u64,
) -> Result<usize, eve_proto::BridgeRpcError> {
    let any_msg = pb::message("eve.vendors.twitch.entitlement.api.DropEventReceived")
        .set("entitlement.uuid", entitlement_uuid)
        .set("user.sequential", user_id as i32)
        .set("offer.sequential", offer_id as u32)
        .must_to_any();
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
    let any_msg = pb::message("eve.vendors.fusionauth.api.UserLinked")
        .set("user.uuid", fa_uuid)
        .set("eve_user.sequential", eve_user_id as i32)
        .must_to_any();
    let (_, delivered) = gateway.publish_notice(scope, any_msg, now_unix)?;
    Ok(delivered)
}

// ============================================================================
// 7. War Domain Encoders & Notice Publishers
// ============================================================================

pub fn encode_war_identifier(war_id: u64) -> Vec<u8> {
    pb::message("eve.war.Identifier").set("sequential", war_id).must_encode()
}

fn war_entity_identifier_builder(alliance_id: Option<u64>, corp_id: Option<u64>) -> ProtoBuilder {
    let mut b = pb::message("eve.war.entity.Identifier");
    if let Some(aid) = alliance_id {
        b = b.set("alliance.sequential", aid as i32);
    }
    if let Some(cid) = corp_id {
        b = b.set("corporation.sequential", cid as u32);
    }
    b
}

pub fn encode_war_entity_identifier(alliance_id: Option<u64>, corp_id: Option<u64>) -> Vec<u8> {
    war_entity_identifier_builder(alliance_id, corp_id).must_encode()
}

fn war_attributes_builder(
    aggressor_corp: u64,
    defender_corp: u64,
    mutual: bool,
    open_allies: bool,
    now_unix: u64,
) -> ProtoBuilder {
    let started_ts = ts_unix(now_unix.saturating_sub(86400));
    pb::message("eve.war.Attributes")
        .set("aggressor", war_entity_identifier_builder(None, Some(aggressor_corp)))
        .set("defender", war_entity_identifier_builder(None, Some(defender_corp)))
        .set("declared_deprecated", started_ts)
        .set("ending_deprecated", ts_unix(now_unix + 7 * 86400))
        .set("mutual", mutual)
        .set("open_for_allies", open_allies)
        .set_with("hostile", |h| h.set("declared", started_ts).set("started", started_ts))
}

pub fn encode_war_attributes(
    aggressor_corp: u64,
    defender_corp: u64,
    mutual: bool,
    open_allies: bool,
    now_unix: u64,
) -> Vec<u8> {
    war_attributes_builder(aggressor_corp, defender_corp, mutual, open_allies, now_unix).must_encode()
}

fn war_destruction_statistics_builder(ships: u64, isk: u64) -> ProtoBuilder {
    pb::message("eve.war.DestructionStatistics")
        .set("ships", ships)
        .set("assets", isk_currency_builder(IskCents::from_isk(isk as i64)))
}

pub fn encode_war_destruction_statistics(ships: u64, isk: u64) -> Vec<u8> {
    war_destruction_statistics_builder(ships, isk).must_encode()
}

pub fn publish_war_declared_notice(
    gateway: &PublicGatewayRouter,
    scope: NoticeScope,
    war_id: u64,
    aggressor_corp: u64,
    defender_corp: u64,
    now_unix: u64,
) -> Result<usize, eve_proto::BridgeRpcError> {
    let any_msg = pb::message("eve.war.Declared")
        .set("war.sequential", war_id)
        .set("war_info", war_attributes_builder(aggressor_corp, defender_corp, false, true, now_unix))
        .must_to_any();
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
    let any_msg = pb::message("eve.war.EndScheduled")
        .set("war.sequential", war_id)
        .set("ending", ts_unix(ending_unix))
        .must_to_any();
    let (_, delivered) = gateway.publish_notice(scope, any_msg, now_unix)?;
    Ok(delivered)
}

// ============================================================================
// 8. Wormhole Domain Encoders
// ============================================================================

pub fn encode_wormhole_identifier(wormhole_id: u64) -> Vec<u8> {
    pb::message("eve.wormhole.Identifier").set("sequential", wormhole_id).must_encode()
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
            Ok(pb::message("eve.tournament.arena.api.GetAllResponse")
                .add("arenas", tournament_arena_builder(b"arena-alpha-1234", "Amarr Championship Arena", sys_id, 200_000, b"tourney-2024", b"ruleset-v1"))
                .must_to_any())
        }),
    );

    router.register_handler(
        "eve.tournament.arena.api.GetSettingsRequest",
        Arc::new(move |_ctx, _any, _msg| {
            Ok(pb::message("eve.tournament.arena.api.GetSettingsResponse")
                .set("tenant_settings.boundary_violation_killer.sequential", 90_000_001u32)
                .must_to_any())
        }),
    );

    router.register_handler(
        "eve.tournament.arena.layout.api.GetRequest",
        Arc::new(move |_ctx, _any, msg| {
            let _uuid = extract_uuid_bytes(msg, 1).unwrap_or_else(|| b"layout-default".to_vec());
            Ok(pb::message("eve.tournament.arena.layout.api.GetResponse")
                .set("layout", tournament_layout_builder("Standard 10v10 Crucible Layout", 1, 150_000))
                .must_to_any())
        }),
    );

    router.register_handler(
        "eve.tournament.ruleset.api.GetAllRequest",
        Arc::new(move |_ctx, _any, _msg| {
            Ok(pb::message("eve.tournament.ruleset.api.GetAllResponse")
                .add_with("rulesets", |r| r.set("uuid", b"ruleset-standard-v1"))
                .add_with("rulesets", |r| r.set("uuid", b"ruleset-alliance-cup"))
                .must_to_any())
        }),
    );

    router.register_handler(
        "eve.tournament.ruleset.api.GetRequest",
        Arc::new(move |_ctx, _any, msg| {
            let _uuid = extract_uuid_bytes(msg, 1).unwrap_or_else(|| b"ruleset-standard-v1".to_vec());
            Ok(pb::message("eve.tournament.ruleset.api.GetResponse")
                .set("ruleset", tournament_ruleset_builder("Crucible Competitive Ruleset", 100, 10))
                .must_to_any())
        }),
    );

    router.register_handler(
        "eve.tournament.arena.api.CreateRequest",
        Arc::new(move |ctx, _any, _msg| {
            crate::quasar_gateway::require_admin_role(ctx)?;
            Ok(pb::message("eve.tournament.arena.api.CreateResponse")
                .set("arena.uuid", b"arena-alpha-1234")
                .must_to_any())
        }),
    );

    router.register_handler(
        "eve.tournament.arena.api.DeleteRequest",
        Arc::new(move |ctx, _any, msg| {
            crate::quasar_gateway::require_admin_role(ctx)?;
            let _uuid = extract_uuid_bytes(msg, 1).unwrap_or_else(|| b"arena-alpha-1234".to_vec());
            Ok(pb::message("eve.tournament.arena.api.DeleteResponse").must_to_any())
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
            let jump_count = if origin == dest { 0u64 } else { 3u64 };
            Ok(pb::message("eve.universe.navigation.GetShortestGateJumpCountResponse")
                .set("jump_count", jump_count)
                .must_to_any())
        }),
    );

    router.register_handler(
        "eve.universe.navigation.GetShortestSystemJumpCountRequest",
        Arc::new(move |_ctx, _any, msg| {
            let start = extract_required_u32(msg, 1, "start")?;
            let end = extract_required_u32(msg, 2, "end")?;
            let jump_count = if start == end { 0u64 } else { 4u64 };
            Ok(pb::message("eve.universe.navigation.GetShortestSystemJumpCountResponse")
                .set("jump_count", jump_count)
                .must_to_any())
        }),
    );

    let sde_const = Arc::clone(&sde);
    router.register_handler(
        "eve.universe.constellation.GetAllInRegionRequest",
        Arc::new(move |_ctx, _any, msg| {
            let region_id = extract_required_u32(msg, 1, "region")?;
            let mut b = pb::message("eve.universe.constellation.GetAllInRegionResponse");
            let mut found = false;
            for sys in sde_const.solar_systems.values() {
                if sys.region_id == region_id {
                    b = b.add_with("constellations", |c| c.set("sequential", u64::from(sys.constellation_id)));
                    found = true;
                }
            }
            if !found {
                b = b.add_with("constellations", |c| c.set("sequential", 20_000_001u64));
            }
            Ok(b.must_to_any())
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
            Ok(pb::message("eve.vanguard.commission.assignment.admin.api.TriggerCompletedEventResponse").must_to_any())
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
            Ok(pb::message("eve.vendors.epic.api.GetUserByEveUserIDResponse")
                .set("user.sequential", user_id as i32)
                .set("epic_user.epic_user_id", "epic_usr_998877")
                .set("is_logged_in", true)
                .must_to_any())
        }),
    );

    // eve.vendors.epic.api.DeliverEntitlementRequest is served by quasar_auth_user (admin-gated, idempotent
    // redeem-token delivery); registering a stub here would shadow it because vendors register later.

    router.register_handler(
        "eve.vendors.epic.api.GetEntitlementsRequest",
        Arc::new(move |ctx, _any, msg| {
            crate::quasar_gateway::require_admin_role(ctx)?;
            let _user = extract_required_u64(msg, 1, "user")?;
            Ok(pb::message("eve.vendors.epic.api.GetEntitlementsResponse")
                .add("entitlements", epic_entitlement_builder("epic_ent_omega_30d", "Omega 30 Days Game Time", "active", 1_700_000_000))
                .must_to_any())
        }),
    );

    router.register_handler(
        "eve.vendors.steam.user.api.GetRequest",
        Arc::new(move |ctx, _any, msg| {
            crate::quasar_gateway::require_admin_role(ctx)?;
            let _steam_id = extract_string_field(msg, 1).unwrap_or("76561198000000000");
            Ok(pb::message("eve.vendors.steam.user.api.GetResponse")
                .set("user.sequential", 1001i32)
                .set("steam_user_attributes.user_name", "CapsuleerSteamPilot")
                .set("steam_user_attributes.country_code", "IS")
                .must_to_any())
        }),
    );

    router.register_handler(
        "eve.vendors.steam.user.api.GetByEveUserRequest",
        Arc::new(move |ctx, _any, msg| {
            crate::quasar_gateway::require_admin_role(ctx)?;
            let _user_id = extract_required_u64(msg, 1, "user")?;
            Ok(pb::message("eve.vendors.steam.user.api.GetByEveUserResponse")
                .set("steam_user.steam_id", "76561198000000000")
                .set("steam_user_attributes.user_name", "CapsuleerSteamPilot")
                .set("steam_user_attributes.country_code", "IS")
                .must_to_any())
        }),
    );

    router.register_handler(
        "eve.vendors.steam.user.api.CreateRequest",
        Arc::new(move |ctx, _any, _msg| {
            crate::quasar_gateway::require_admin_role(ctx)?;
            Ok(pb::message("eve.vendors.steam.user.api.CreateResponse")
                .set("user.sequential", 1002i32)
                .set("already_existed", false)
                .must_to_any())
        }),
    );

    router.register_handler(
        "eve.vendors.fusionauth.api.LinkUserRequest",
        Arc::new(move |ctx, _any, _msg| {
            crate::quasar_gateway::require_admin_role(ctx)?;
            Ok(pb::message("eve.vendors.fusionauth.api.LinkUserResponse")
                .set("user.sequential", 1001i32)
                .must_to_any())
        }),
    );

    router.register_handler(
        "eve.vendors.payletter.GetUserIdentityVerificationReceiptRequest",
        Arc::new(move |ctx, _any, msg| {
            crate::quasar_gateway::require_admin_role(ctx)?;
            let _user_id = extract_required_u64(msg, 1, "user")?;
            Ok(pb::message("eve.vendors.payletter.GetUserIdentityVerificationReceiptResponse")
                .set("receipt", payletter_receipt_builder("payletter_tx_889900", "AUTH_VERIFIED_KR"))
                .must_to_any())
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
            let (agg, def, mut_war, open) = db_war.with_snapshot(|snap| {
                if let Some(w) = snap.wars.get(&(war_id as i64)) {
                    (w.aggressor_corp_id as u64, w.defender_corp_id as u64, w.mutual, true)
                } else {
                    (1_000_170, 1_000_171, false, true)
                }
            });
            Ok(pb::message("eve.war.GetResponse")
                .set("war_info", war_attributes_builder(agg, def, mut_war, open, 1_700_000_000))
                .must_to_any())
        }),
    );

    let db_all_wars = Arc::clone(&db);
    router.register_handler(
        "eve.war.GetAllRequest",
        Arc::new(move |_ctx, _any, _msg| {
            let war_ids: Vec<u64> = db_all_wars.with_snapshot(|snap| {
                snap.wars.keys().map(|&wid| wid as u64).collect()
            });
            let mut b = pb::message("eve.war.GetAllResponse");
            if war_ids.is_empty() {
                b = b.add_with("wars", |w| w.set("sequential", 1u64));
            } else {
                for wid in war_ids {
                    b = b.add_with("wars", |w| w.set("sequential", wid));
                }
            }
            Ok(b.must_to_any())
        }),
    );

    router.register_handler(
        "eve.war.GetDestructionStatisticsRequest",
        Arc::new(move |_ctx, _any, msg| {
            let _war_id = extract_required_u64(msg, 1, "war")?;
            Ok(pb::message("eve.war.GetDestructionStatisticsResponse")
                .set("by_aggressor", war_destruction_statistics_builder(12, 1_500_000_000))
                .set("by_defender", war_destruction_statistics_builder(8, 850_000_000))
                .must_to_any())
        }),
    );

    router.register_handler(
        "eve.war.GetAlliesRequest",
        Arc::new(move |_ctx, _any, msg| {
            let _war_id = extract_required_u64(msg, 1, "war")?;
            Ok(pb::message("eve.war.GetAlliesResponse")
                .add("allies", war_entity_identifier_builder(Some(99_000_001), None))
                .add("allies", war_entity_identifier_builder(None, Some(1_000_172)))
                .must_to_any())
        }),
    );

}

#[cfg(test)]
#[path = "quasar_vendors_and_war_services_tests.rs"]
mod quasar_vendors_and_war_services_tests;

#[cfg(test)]
#[path = "quasar_vendors_and_war_services_edges_tests.rs"]
mod quasar_vendors_and_war_services_edges_tests;

