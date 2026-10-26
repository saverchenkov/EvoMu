//! `quasar_cosmetics_services` — Quasar Ship Cosmetics, SKINR, Titles, Heraldry & App Services (`Q-9`).
//!
//! Implements server-side Quasar RPC handlers and notice publishing for:
//! - `eve.cosmetic.ship.logo.DisplayRequest`, `ClearRequest`
//! - `eve.cosmetic.ship.api.request.GetRequest`, `GetAllInBubbleRequest`
//! - `eve.cosmetic.ship.skin.thirdparty.api.requests.GetRequest`
//! - `eve.cosmetic.ship.skin.thirdparty.sequencing.api.requests.GetTierPricingRequest`,
//!   `GetTierDurationsForCharacterRequest`, `GetEarlyCompletionPricingRequest`,
//!   `GetSkillBasedJobsCapacityRequest`
//! - `eve.cosmetic.ship.skin.thirdparty.license.api.requests.GetOwnedRequest`, `GetRequest`
//! - `eve_public.cosmetic.ship.skin.thirdparty.draft.api.requests.GetAllSavedRequest`,
//!   `GetRequest`, `SaveRequest`, `UpdateRequest`, `DeleteRequest`, `GetSaveCapacityRequest`
//! - `eve.cosmetic.corporation.palette.api.requests.GetRequest`, `GetOwnRequest`, `SetRequest`
//! - `eve_public.cosmetic.character.title.api.requests.GetOwnedRequest`,
//!   `GetEquippedForCharacterRequest`, `EquipRequest`, `UnequipRequest`
//! - `eve_public.cosmetic.structure.paintwork.api.GetRequest`, `GetAllInSolarSystemRequest`
//! - `eve_public.cosmetic.structure.paintwork.license.api.GetAllOwnedByCorporationRequest`
//! - `eve.quasar.config.api.requests.GetConfigRequest`
//! - App telemetry & lifecycle events acknowledgment

#![forbid(unsafe_code)]

use super::quasar_space_services::{
    effective_roles, encode_domain_message, get_int_field, set_domain_attributes, set_domain_field,
};
use crate::quasar_gateway::{
    require_admin_role, NoticeScope, PublicGatewayRouter, STATUS_FORBIDDEN,
};
use eve_db::WorldRepository;
use eve_proto::quasar::{
    encode_bytes_field, encode_varint_field, GoogleAny, GoogleEmpty, QuasarMessage,
    TYPE_GOOGLEAPIS_PREFIX,
};
use eve_sde::SdeRegistry;
use std::sync::{Arc, RwLock};
use std::collections::HashMap;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
pub struct QuasarCosmeticsStateData {
    pub ship_logos: HashMap<u64, (u32, u32)>,
    #[serde(default)]
    pub skinr_drafts: HashMap<u64, (u32, String)>,
    #[serde(default)]
    pub saved_drafts: HashMap<u32, HashMap<u64, super::quasar_cosmetics_drafts::SkinrDraftRecord>>,
    #[serde(default)]
    pub owned_titles: HashMap<u32, std::collections::HashSet<u64>>,
    pub equipped_titles: HashMap<u32, u64>,
    #[serde(default)]
    pub corp_palettes: HashMap<u32, String>,
    #[serde(default)]
    pub custom_capacity: HashMap<u32, u64>,
    #[serde(default)]
    pub custom_endpoints: HashMap<String, (u64, String)>,
}

pub struct QuasarCosmeticsState {
    pub db: Arc<WorldRepository>,
    pub data: RwLock<QuasarCosmeticsStateData>,
}

impl QuasarCosmeticsState {
    pub fn from_db(db: Arc<WorldRepository>) -> Arc<Self> {
        let loaded = db.with_snapshot(|snap| {
            snap.quasar_state_blobs
                .get("quasar_cosmetics")
                .and_then(|s| serde_json::from_str::<QuasarCosmeticsStateData>(s).ok())
                .unwrap_or_default()
        });
        Arc::new(Self {
            db,
            data: RwLock::new(loaded),
        })
    }

    pub fn sync_to_db(&self) {
        let data = self.data.read().unwrap_or_else(|e| e.into_inner());
        if let Ok(json) = serde_json::to_string(&*data) {
            let _ = self.db.try_with_mut_snapshot(|snap| {
                snap.quasar_state_blobs.insert("quasar_cosmetics".into(), json);
                Ok::<(), String>(())
            });
        }
    }
}

/// Registers all Q-9 cosmetics, heraldry, and app service handlers on the gateway router.
pub fn register_handlers(
    router: &PublicGatewayRouter,
    db: Arc<WorldRepository>,
    sde: Arc<SdeRegistry>,
) {
    let state = QuasarCosmeticsState::from_db(Arc::clone(&db));
    let desc = Arc::clone(&router.descriptors);

    // 1. Ship logo display/clear: see `quasar_cosmetics_live` (state-backed handlers).

    // 2. eve.cosmetic.ship.api.request.GetRequest & GetAllInBubbleRequest
    {
        let d = Arc::clone(&desc);
        let st = Arc::clone(&state);
        router.register_handler(
            "eve.cosmetic.ship.api.request.GetRequest",
            Arc::new(move |_ctx, _any, dyn_msg| {
                let ship_id = get_int_field(dyn_msg, 1).unwrap_or(1_000_001) as u64;
                let (char_id, corp_id) = {
                    let data = st.data.read().unwrap_or_else(|e| e.into_inner());
                    data.ship_logos.get(&ship_id).copied().unwrap_or((90_000_001, 98_000_001))
                };
                encode_domain_message(&d, "eve.cosmetic.ship.api.request.GetResponse", |reg, resp| {
                    set_domain_attributes(reg, resp, char_id as u64, corp_id as u64, "ShipLogoConfig");
                })
            }),
        );
    }
    {
        let d = Arc::clone(&desc);
        let sde_skin = Arc::clone(&sde);
        router.register_handler(
            "eve.cosmetic.ship.api.request.GetAllInBubbleRequest",
            Arc::new(move |_ctx, _any, _dyn_msg| {
                let skin_id = sde_skin.skins.keys().next().copied().unwrap_or(1) as u64;
                encode_domain_message(
                    &d,
                    "eve.cosmetic.ship.api.request.GetAllInBubbleResponse",
                    |reg, resp| {
                        set_domain_attributes(reg, resp, 1_000_001, skin_id, "BubbleShipCosmetics");
                    },
                )
            }),
        );
    }

    // 3..5. 3rd-party SKINR custom SKINs, component licenses, skin licenses, sequencing, and title definitions:
    // delegated to `quasar_cosmetics_skinr` and `quasar_cosmetics_skinr_seq`.

    // 6. eve_public.cosmetic.ship.skin.thirdparty.draft.api & title handlers (delegated to quasar_cosmetics_drafts)
    super::quasar_cosmetics_drafts::register_draft_and_title_handlers(
        router,
        Arc::clone(&state),
        Arc::clone(&desc),
    );

    // 7. eve.cosmetic.corporation.palette.api.requests handlers
    {
        let d = Arc::clone(&desc);
        let st = Arc::clone(&state);
        router.register_handler(
            "eve.cosmetic.corporation.palette.api.GetRequest",
            Arc::new(move |_ctx, _any, dyn_msg| {
                let corp_id = get_int_field(dyn_msg, 1).unwrap_or(98_000_001) as u64;
                let palette = st
                    .data
                    .read()
                    .unwrap_or_else(|e| e.into_inner())
                    .corp_palettes
                    .get(&(corp_id as u32))
                    .cloned()
                    .unwrap_or_default();
                encode_domain_message(
                    &d,
                    "eve.cosmetic.corporation.palette.api.GetResponse",
                    |reg, resp| {
                        set_domain_field(reg, resp, 1, corp_id, &palette);
                    },
                )
            }),
        );
    }
    {
        let d = Arc::clone(&desc);
        let st = Arc::clone(&state);
        router.register_handler(
            "eve.cosmetic.corporation.palette.api.GetOwnRequest",
            Arc::new(move |ctx, _any, _dyn_msg| {
                let corp_id = ctx.effective_corporation_id().unwrap_or(98_000_001) as u64;
                let palette = st
                    .data
                    .read()
                    .unwrap_or_else(|e| e.into_inner())
                    .corp_palettes
                    .get(&(corp_id as u32))
                    .cloned()
                    .unwrap_or_default();
                encode_domain_message(
                    &d,
                    "eve.cosmetic.corporation.palette.api.GetResponse",
                    |reg, resp| {
                        set_domain_field(reg, resp, 1, corp_id, &palette);
                    },
                )
            }),
        );
    }
    {
        let d = Arc::clone(&desc);
        let st = Arc::clone(&state);
        router.register_handler(
            "eve.cosmetic.corporation.palette.api.SetRequest",
            Arc::new(move |ctx, _any, _dyn_msg| {
                let role = effective_roles(ctx);
                if role & 0x01 == 0 && role & 0x02 == 0 {
                    return Err((
                        STATUS_FORBIDDEN,
                        "Corporation director role required to set corporation palette".into(),
                    ));
                }
                let corp_id = ctx.effective_corporation_id().unwrap_or(98_000_001) as u64;
                st.data
                    .write()
                    .unwrap_or_else(|e| e.into_inner())
                    .corp_palettes
                    .insert(corp_id as u32, "UpdatedPalette".into());
                st.sync_to_db();
                encode_domain_message(
                    &d,
                    "eve.cosmetic.corporation.palette.api.GetResponse",
                    |reg, resp| {
                        set_domain_attributes(reg, resp, corp_id, 1, "UpdatedPalette");
                    },
                )
            }),
        );
    }



    // 9. Structure paintwork + licenses: see `quasar_cosmetics_live`.

    // 10. eve.quasar.config.api.GetRequest
    {
        let d = Arc::clone(&desc);
        router.register_handler(
            "eve.quasar.config.api.GetRequest",
            Arc::new(move |_ctx, _any, _dyn_msg| {
                encode_domain_message(&d, "eve.quasar.config.api.GetResponse", |reg, resp| {
                    set_domain_attributes(reg, resp, 1, 2401, "tranquility");
                })
            }),
        );
    }

    // 11. App telemetry lifecycle events acknowledgment
    router.register_handler(
        "eve_public.app.launcher.distribution.api.LauncherDistributionClientStartRequest",
        Arc::new(|_ctx, _any, _dyn_msg| Ok(GoogleEmpty.to_any())),
    );
    router.register_handler(
        "eve_public.app.launcher.eve_client.GameClientStartRequest",
        Arc::new(|_ctx, _any, _dyn_msg| Ok(GoogleEmpty.to_any())),
    );
    router.register_handler(
        "eve_public.app.launcher.eve_client.GameClientStopRequest",
        Arc::new(|_ctx, _any, _dyn_msg| Ok(GoogleEmpty.to_any())),
    );

    // 12. Public eve_public.cosmetic.* request packages (market, SKINR, licenses, sequencing, palette)
    for (req_name, rsp_name) in [
        (
            "eve_public.cosmetic.market.api.GetBrokerFeesRequest",
            "eve_public.cosmetic.market.api.GetBrokerFeesResponse",
        ),
        (
            "eve_public.cosmetic.market.api.GetTaxRateRequest",
            "eve_public.cosmetic.market.api.GetTaxRateResponse",
        ),
        (
            "eve_public.cosmetic.market.api.GetBuyerFeeRequest",
            "eve_public.cosmetic.market.api.GetBuyerFeeResponse",
        ),
        (
            "eve_public.cosmetic.market.api.GetBuyerFeesDefinitionRequest",
            "eve_public.cosmetic.market.api.GetBuyerFeesDefinitionResponse",
        ),
        (
            "eve_public.cosmetic.market.component.listing.api.GetAllRequest",
            "eve_public.cosmetic.market.component.listing.api.GetAllResponse",
        ),
        (
            "eve_public.cosmetic.market.component.listing.api.InitiatePurchaseListingRequest",
            "eve_public.cosmetic.market.component.listing.api.InitiatePurchaseListingResponse",
        ),
        (
            "eve_public.cosmetic.market.skin.listing.api.admin.SetExpirationRequest",
            "eve_public.cosmetic.market.skin.listing.api.admin.SetExpirationResponse",
        ),
        (
            "eve_public.cosmetic.market.skin.listing.api.GetAllRequest",
            "eve_public.cosmetic.market.skin.listing.api.GetAllResponse",
        ),
        (
            "eve_public.cosmetic.market.skin.listing.api.GetAllOwnedRequest",
            "eve_public.cosmetic.market.skin.listing.api.GetAllOwnedResponse",
        ),
        (
            "eve_public.cosmetic.market.skin.listing.api.GetRequest",
            "eve_public.cosmetic.market.skin.listing.api.GetResponse",
        ),
        (
            "eve_public.cosmetic.market.skin.listing.api.InitiatePurchaseListingRequest",
            "eve_public.cosmetic.market.skin.listing.api.InitiatePurchaseListingResponse",
        ),
        (
            "eve_public.cosmetic.market.skin.listing.api.CancelListingRequest",
            "eve_public.cosmetic.market.skin.listing.api.CancelListingResponse",
        ),
        (
            "eve_public.cosmetic.market.skin.listing.api.InitiateCreateListingRequest",
            "eve_public.cosmetic.market.skin.listing.api.InitiateCreateListingResponse",
        ),
    ] {
        let d = Arc::clone(&desc);
        let db_cosm = Arc::clone(&db);
        let sde_cosm = Arc::clone(&sde);
        let st = Arc::clone(&state);
        router.register_handler(
            req_name,
            Arc::new(move |ctx, any, _dyn_msg| {
                if req_name.contains(".admin.") {
                    require_admin_role(ctx)?;
                }
                let cid = ctx.effective_character_id().unwrap_or(90_000_001) as i64;
                let domain_key = req_name.rsplit_once(".api.").map(|(p, _)| p).unwrap_or(req_name);
                if !req_name.contains(".Get") && !req_name.contains(".List") && !req_name.contains(".Search") {
                    let hex_payload: String = if any.value.is_empty() {
                        "SKINRParagon".to_string()
                    } else {
                        any.value.iter().map(|b| format!("{b:02x}")).collect()
                    };
                    st.data
                        .write()
                        .unwrap_or_else(|e| e.into_inner())
                        .custom_endpoints
                        .insert(domain_key.to_string(), (cid as u64, hex_payload));
                    st.sync_to_db();
                }
                let corp_id = db_cosm.with_snapshot(|s| {
                    s.characters
                        .get(&cid)
                        .map(|c| c.corporation_id as u64)
                        .unwrap_or(98_000_001)
                });
                let skin_id = sde_cosm.skins.keys().next().copied().unwrap_or(1) as u64;
                let custom_record = st
                    .data
                    .read()
                    .unwrap_or_else(|e| e.into_inner())
                    .custom_endpoints
                    .get(domain_key)
                    .cloned();
                let label = custom_record
                    .as_ref()
                    .map(|(_, l)| l.as_str())
                    .unwrap_or("SKINRParagon");
                encode_domain_message(&d, rsp_name, |reg, resp| {
                    set_domain_attributes(reg, resp, skin_id, corp_id, label);
                })
            }),
        );
    }

    // 13. State-backed ship logo / ship SKIN state / palette / structure paintwork handlers.
    super::quasar_cosmetics_live::register(
        router,
        super::quasar_cosmetics_live::CosmeticsLiveState::load(Arc::clone(&db), Arc::clone(&sde)),
    );

    // 14. State-backed 3rd-party SKINR component licenses, custom SKINs, sequencing jobs & title definitions.
    let skinr_state = super::quasar_cosmetics_skinr::SkinrCosmeticsState::load(
        Arc::clone(&db),
        Arc::clone(&sde),
    );
    super::quasar_cosmetics_skinr::register(router, Arc::clone(&skinr_state));
    super::quasar_cosmetics_skinr_seq::register(router, skinr_state);
}

fn encode_id_field(field_num: u32, id: u64, out: &mut Vec<u8>) {
    let mut id_buf = Vec::new();
    encode_varint_field(1, id, &mut id_buf);
    encode_bytes_field(field_num, &id_buf, out);
}

/// Helper for broadcasting `GrantedNotice` (`eve_public.cosmetic.character.title.api.GrantedNotice`).
pub fn publish_cosmetic_title_granted_notice(
    gateway: &PublicGatewayRouter,
    character_id: u32,
    title_id: u64,
    now_unix: u64,
) -> Result<usize, eve_proto::BridgeRpcError> {
    let mut payload = Vec::new();
    encode_id_field(1, title_id, &mut payload);
    let any = GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.cosmetic.character.title.api.GrantedNotice"),
        value: payload,
    };
    gateway.publish_notice(NoticeScope::Character(character_id), any, now_unix).map(|(_, n)| n)
}

/// Helper for broadcasting `TransactionSucceededNotice` (`eve_public.cosmetic.market.transaction.api.TransactionSucceededNotice`).
pub fn publish_cosmetic_market_transaction_notice(
    gateway: &PublicGatewayRouter,
    character_id: u32,
    transaction_id: u64,
    now_unix: u64,
) -> Result<usize, eve_proto::BridgeRpcError> {
    let mut payload = Vec::new();
    encode_id_field(1, transaction_id, &mut payload);
    let any = GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.cosmetic.market.transaction.api.TransactionSucceededNotice"),
        value: payload,
    };
    gateway.publish_notice(NoticeScope::Character(character_id), any, now_unix).map(|(_, n)| n)
}
