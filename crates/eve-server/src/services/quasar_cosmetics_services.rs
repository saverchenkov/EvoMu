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
    require_admin_role, NoticeScope, PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_FORBIDDEN,
};
use eve_db::{IskCents, WorldRepository};
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

    // 1. eve_public.cosmetic.ship.logo handlers (emblem display/clear)
    {
        let d = Arc::clone(&desc);
        let st = Arc::clone(&state);
        let db_logo = Arc::clone(&db);
        router.register_handler(
            "eve_public.cosmetic.ship.logo.DisplayRequest",
            Arc::new(move |ctx, _any, _dyn_msg| {
                let char_id = match ctx.effective_character_id() {
                    Some(c) => c,
                    None => return Err((STATUS_FORBIDDEN, "Active character required".into())),
                };
                let corp_id = ctx.effective_corporation_id().unwrap_or(98_000_001);
                let ship_id = db_logo.with_snapshot(|snap| {
                    snap.characters.get(&(char_id as i64)).map(|c| c.active_ship_id as u64).unwrap_or(1_000_001)
                });
                {
                    let mut data = st.data.write().unwrap_or_else(|e| e.into_inner());
                    data.ship_logos.insert(ship_id, (char_id, corp_id));
                }
                st.sync_to_db();
                encode_domain_message(
                    &d,
                    "eve_public.cosmetic.ship.logo.DisplayResponse",
                    |reg, resp| {
                        set_domain_attributes(reg, resp, char_id as u64, corp_id as u64, "ShipLogoDisplayed");
                    },
                )
            }),
        );
    }
    {
        let d = Arc::clone(&desc);
        let st = Arc::clone(&state);
        let db_logo = Arc::clone(&db);
        router.register_handler(
            "eve_public.cosmetic.ship.logo.ClearRequest",
            Arc::new(move |ctx, _any, _dyn_msg| {
                let char_id = match ctx.effective_character_id() {
                    Some(c) => c,
                    None => return Err((STATUS_FORBIDDEN, "Active character required".into())),
                };
                let corp_id = ctx.effective_corporation_id().unwrap_or(98_000_001);
                let ship_id = db_logo.with_snapshot(|snap| {
                    snap.characters.get(&(char_id as i64)).map(|c| c.active_ship_id as u64).unwrap_or(1_000_001)
                });
                {
                    let mut data = st.data.write().unwrap_or_else(|e| e.into_inner());
                    data.ship_logos.remove(&ship_id);
                }
                st.sync_to_db();
                encode_domain_message(&d, "eve_public.cosmetic.ship.logo.ClearResponse", |reg, resp| {
                    set_domain_attributes(reg, resp, char_id as u64, corp_id as u64, "ShipLogoCleared");
                })
            }),
        );
    }

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

    // 3. eve.cosmetic.ship.skin.thirdparty.api.requests.GetRequest
    {
        let d = Arc::clone(&desc);
        router.register_handler(
            "eve.cosmetic.ship.skin.thirdparty.api.requests.GetRequest",
            Arc::new(move |_ctx, _any, dyn_msg| {
                let design_id = get_int_field(dyn_msg, 1).unwrap_or(50_001) as u64;
                encode_domain_message(
                    &d,
                    "eve.cosmetic.ship.skin.thirdparty.api.requests.GetResponse",
                    |reg, resp| {
                        set_domain_field(reg, resp, 1, design_id, "");
                    },
                )
            }),
        );
    }

    // 4. eve.cosmetic.ship.skin.thirdparty.sequencing.api.requests handlers
    {
        let d = Arc::clone(&desc);
        router.register_handler(
            "eve.cosmetic.ship.skin.thirdparty.sequencing.api.GetTierPricingRequest",
            Arc::new(move |_ctx, _any, _dyn_msg| {
                encode_domain_message(
                    &d,
                    "eve.cosmetic.ship.skin.thirdparty.sequencing.api.GetTierPricingResponse",
                    |reg, resp| {
                        set_domain_attributes(reg, resp, 1, 50, "Tier1PlexPricing");
                    },
                )
            }),
        );
    }
    {
        let d = Arc::clone(&desc);
        router.register_handler(
            "eve.cosmetic.ship.skin.thirdparty.sequencing.api.GetTierDurationsForCharacterRequest",
            Arc::new(move |_ctx, _any, _dyn_msg| {
                encode_domain_message(
                    &d,
                    "eve.cosmetic.ship.skin.thirdparty.sequencing.api.GetTierDurationsForCharacterResponse",
                    |reg, resp| {
                        set_domain_attributes(reg, resp, 1, 3_600, "Tier1DurationSeconds");
                    },
                )
            }),
        );
    }
    {
        let d = Arc::clone(&desc);
        router.register_handler(
            "eve.cosmetic.ship.skin.thirdparty.sequencing.api.GetEarlyCompletionPricingRequest",
            Arc::new(move |_ctx, _any, _dyn_msg| {
                encode_domain_message(
                    &d,
                    "eve.cosmetic.ship.skin.thirdparty.sequencing.api.GetEarlyCompletionPricingResponse",
                    |reg, resp| {
                        set_domain_attributes(reg, resp, 1, 25, "EarlyCompletionPlexPricing");
                    },
                )
            }),
        );
    }
    {
        let d = Arc::clone(&desc);
        router.register_handler(
            "eve.cosmetic.ship.skin.thirdparty.sequencing.api.GetSkillBasedJobsCapacityRequest",
            Arc::new(move |_ctx, _any, _dyn_msg| {
                encode_domain_message(
                    &d,
                    "eve.cosmetic.ship.skin.thirdparty.sequencing.api.GetSkillBasedJobsCapacityResponse",
                    |reg, resp| {
                        set_domain_field(reg, resp, 1, 5, "");
                    },
                )
            }),
        );
    }

    // 5. eve.cosmetic.ship.skin.thirdparty.license.api.requests handlers
    {
        let d = Arc::clone(&desc);
        router.register_handler(
            "eve.cosmetic.ship.skin.thirdparty.license.api.GetOwnedRequest",
            Arc::new(move |ctx, _any, _dyn_msg| {
                let char_id = match ctx.effective_character_id() {
                    Some(c) => c,
                    None => return Err((STATUS_FORBIDDEN, "Active character required".into())),
                };
                encode_domain_message(
                    &d,
                    "eve.cosmetic.ship.skin.thirdparty.license.api.GetOwnedResponse",
                    |reg, resp| {
                        set_domain_field(reg, resp, 1, char_id as u64, "");
                    },
                )
            }),
        );
    }
    {
        let d = Arc::clone(&desc);
        router.register_handler(
            "eve.cosmetic.ship.skin.thirdparty.license.api.GetRequest",
            Arc::new(move |_ctx, _any, dyn_msg| {
                let license_id = get_int_field(dyn_msg, 1).unwrap_or(200_001) as u64;
                encode_domain_message(
                    &d,
                    "eve.cosmetic.ship.skin.thirdparty.license.api.GetResponse",
                    |reg, resp| {
                        set_domain_field(reg, resp, 1, license_id, "");
                    },
                )
            }),
        );
    }

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



    // 9. eve_public.cosmetic.structure.paintwork.api handlers
    {
        let d = Arc::clone(&desc);
        router.register_handler(
            "eve_public.cosmetic.structure.paintwork.api.GetRequest",
            Arc::new(move |_ctx, _any, dyn_msg| {
                let structure_id = get_int_field(dyn_msg, 1).unwrap_or(1_000_000_001) as u64;
                encode_domain_message(
                    &d,
                    "eve_public.cosmetic.structure.paintwork.api.GetResponse",
                    |reg, resp| {
                        set_domain_field(reg, resp, 1, structure_id, "");
                    },
                )
            }),
        );
    }
    {
        let d = Arc::clone(&desc);
        router.register_handler(
            "eve_public.cosmetic.structure.paintwork.api.GetAllInSolarSystemRequest",
            Arc::new(move |_ctx, _any, dyn_msg| {
                let sys_id = get_int_field(dyn_msg, 1).unwrap_or(30_000_142) as u64;
                encode_domain_message(
                    &d,
                    "eve_public.cosmetic.structure.paintwork.api.GetAllInSolarSystemResponse",
                    |reg, resp| {
                        set_domain_field(reg, resp, 1, sys_id, "");
                    },
                )
            }),
        );
    }
    {
        let d = Arc::clone(&desc);
        router.register_handler(
            "eve_public.cosmetic.structure.paintwork.license.api.GetAllOwnedByCorporationRequest",
            Arc::new(move |_ctx, _any, dyn_msg| {
                let corp_id = get_int_field(dyn_msg, 1).unwrap_or(98_000_001) as u64;
                encode_domain_message(
                    &d,
                    "eve_public.cosmetic.structure.paintwork.license.api.GetAllOwnedByCorporationResponse",
                    |reg, resp| {
                        set_domain_field(reg, resp, 1, corp_id, "");
                    },
                )
            }),
        );
    }

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
            "eve_public.cosmetic.corporation.palette.api.GetRequest",
            "eve_public.cosmetic.corporation.palette.api.GetResponse",
        ),
        (
            "eve_public.cosmetic.corporation.palette.api.GetOwnRequest",
            "eve_public.cosmetic.corporation.palette.api.GetOwnResponse",
        ),
        (
            "eve_public.cosmetic.corporation.palette.api.SetRequest",
            "eve_public.cosmetic.corporation.palette.api.SetResponse",
        ),
        (
            "eve_public.cosmetic.corporation.palette.api.CanEditRequest",
            "eve_public.cosmetic.corporation.palette.api.CanEditResponse",
        ),
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
        (
            "eve_public.cosmetic.ship.api.GetAllInBubbleRequest",
            "eve_public.cosmetic.ship.api.GetAllInBubbleResponse",
        ),
        (
            "eve_public.cosmetic.ship.api.GetRequest",
            "eve_public.cosmetic.ship.api.GetResponse",
        ),
        (
            "eve_public.cosmetic.ship.skin.thirdparty.api.GetRequest",
            "eve_public.cosmetic.ship.skin.thirdparty.api.GetResponse",
        ),
        (
            "eve_public.cosmetic.ship.skin.thirdparty.component.license.api.GetOwnedRequest",
            "eve_public.cosmetic.ship.skin.thirdparty.component.license.api.GetOwnedResponse",
        ),
        (
            "eve_public.cosmetic.ship.skin.thirdparty.component.license.finite.api.admin.GrantRequest",
            "eve_public.cosmetic.ship.skin.thirdparty.component.license.finite.api.admin.GrantResponse",
        ),
        (
            "eve_public.cosmetic.ship.skin.thirdparty.component.license.finite.api.admin.RevokeRequest",
            "eve_public.cosmetic.ship.skin.thirdparty.component.license.finite.api.admin.RevokeResponse",
        ),
        (
            "eve_public.cosmetic.ship.skin.thirdparty.component.license.finite.api.GetRequest",
            "eve_public.cosmetic.ship.skin.thirdparty.component.license.finite.api.GetResponse",
        ),
        (
            "eve_public.cosmetic.ship.skin.thirdparty.component.license.infinite.api.admin.GrantRequest",
            "eve_public.cosmetic.ship.skin.thirdparty.component.license.infinite.api.admin.GrantResponse",
        ),
        (
            "eve_public.cosmetic.ship.skin.thirdparty.component.license.infinite.api.admin.RevokeRequest",
            "eve_public.cosmetic.ship.skin.thirdparty.component.license.infinite.api.admin.RevokeResponse",
        ),
        (
            "eve_public.cosmetic.ship.skin.thirdparty.component.license.infinite.api.GetRequest",
            "eve_public.cosmetic.ship.skin.thirdparty.component.license.infinite.api.GetResponse",
        ),
        (
            "eve_public.cosmetic.ship.skin.thirdparty.component.license.item.api.ConsumeRequest",
            "eve_public.cosmetic.ship.skin.thirdparty.component.license.item.api.ConsumeResponse",
        ),
        (
            "eve_public.cosmetic.ship.skin.thirdparty.license.api.admin.GrantRequest",
            "eve_public.cosmetic.ship.skin.thirdparty.license.api.admin.GrantResponse",
        ),
        (
            "eve_public.cosmetic.ship.skin.thirdparty.license.api.admin.RevokeRequest",
            "eve_public.cosmetic.ship.skin.thirdparty.license.api.admin.RevokeResponse",
        ),
        (
            "eve_public.cosmetic.ship.skin.thirdparty.license.api.GetOwnedRequest",
            "eve_public.cosmetic.ship.skin.thirdparty.license.api.GetOwnedResponse",
        ),
        (
            "eve_public.cosmetic.ship.skin.thirdparty.license.api.GetRequest",
            "eve_public.cosmetic.ship.skin.thirdparty.license.api.GetResponse",
        ),
        (
            "eve_public.cosmetic.ship.skin.thirdparty.license.api.ActivateRequest",
            "eve_public.cosmetic.ship.skin.thirdparty.license.api.ActivateResponse",
        ),
        (
            "eve_public.cosmetic.ship.skin.thirdparty.license.api.ApplyRequest",
            "eve_public.cosmetic.ship.skin.thirdparty.license.api.ApplyResponse",
        ),
        (
            "eve_public.cosmetic.ship.skin.thirdparty.license.api.UnapplyRequest",
            "eve_public.cosmetic.ship.skin.thirdparty.license.api.UnapplyResponse",
        ),
        (
            "eve_public.cosmetic.ship.skin.thirdparty.sequencing.api.GetTierPricingRequest",
            "eve_public.cosmetic.ship.skin.thirdparty.sequencing.api.GetTierPricingResponse",
        ),
        (
            "eve_public.cosmetic.ship.skin.thirdparty.sequencing.api.GetEarlyCompletionPricingRequest",
            "eve_public.cosmetic.ship.skin.thirdparty.sequencing.api.GetEarlyCompletionPricingResponse",
        ),
        (
            "eve_public.cosmetic.ship.skin.thirdparty.sequencing.api.GetTierDurationsRequest",
            "eve_public.cosmetic.ship.skin.thirdparty.sequencing.api.GetTierDurationsResponse",
        ),
        (
            "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.GetAllActiveRequest",
            "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.GetAllActiveResponse",
        ),
        (
            "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.GetRequest",
            "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.GetResponse",
        ),
        (
            "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.InitiateRequest",
            "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.InitiateResponse",
        ),
        (
            "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.CompleteEarlyRequest",
            "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.CompleteEarlyResponse",
        ),
        (
            "eve_public.cosmetic.structure.paintwork.license.api.admin.IssueRequest",
            "eve_public.cosmetic.structure.paintwork.license.api.admin.IssueResponse",
        ),
        (
            "eve_public.cosmetic.structure.paintwork.license.api.admin.RevokeRequest",
            "eve_public.cosmetic.structure.paintwork.license.api.admin.RevokeResponse",
        ),
        (
            "eve_public.cosmetic.structure.paintwork.api.admin.IssueRequest",
            "eve_public.cosmetic.structure.paintwork.license.api.admin.IssueResponse",
        ),
        (
            "eve_public.cosmetic.structure.paintwork.api.admin.RevokeRequest",
            "eve_public.cosmetic.structure.paintwork.license.api.admin.RevokeResponse",
        ),
    ] {
        let d = Arc::clone(&desc);
        let db_cosm = Arc::clone(&db);
        let sde_cosm = Arc::clone(&sde);
        let pending = Arc::clone(&router.pending_notices);
        let st = Arc::clone(&state);
        router.register_handler(
            req_name,
            Arc::new(move |ctx, any, _dyn_msg| {
                if req_name.contains(".admin.") {
                    require_admin_role(ctx)?;
                }
                let cid = if req_name.contains(".sequencing.job.api.") {
                    let Some(cid_u64) = ctx.effective_character_id() else {
                        return Err((STATUS_FORBIDDEN, "Character required".into()));
                    };
                    cid_u64 as i64
                } else {
                    ctx.effective_character_id().unwrap_or(90_000_001) as i64
                };
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
                if req_name.ends_with(".sequencing.job.api.InitiateRequest") {
                    let seq_key = format!("active_seq_{cid}");
                    let active_jobs = st
                        .data
                        .read()
                        .unwrap_or_else(|e| e.into_inner())
                        .custom_endpoints
                        .get(&seq_key)
                        .and_then(|(_, v)| v.parse::<u64>().ok())
                        .unwrap_or(0);
                    if active_jobs >= 5 {
                        return Err((STATUS_BAD_REQUEST, "Maximum active sequencing jobs reached".into()));
                    }

                    // Check and debit fee BEFORE incrementing active jobs or granting EverMarks
                    let fee = IskCents::from_isk(10_000);
                    let mut insufficient_funds = false;
                    db_cosm.with_mut_snapshot(|s| {
                        if let Some(ch) = s.characters.get_mut(&cid) {
                            if let Some(rem) = ch.balance.checked_sub(fee) {
                                ch.balance = rem;
                                let cur = ch.loyalty_points.entry(1_000_419).or_insert(0);
                                *cur = cur.saturating_add(500);
                            } else {
                                insufficient_funds = true;
                                return;
                            }
                            crate::services::lp_store::record_lp_reward_log(
                                s,
                                cid,
                                1_000_419,
                                500,
                                "SKINR sequencing EverMarks reward".into(),
                                0,
                            );
                        }
                    });
                    if insufficient_funds {
                        return Err((STATUS_BAD_REQUEST, "Insufficient funds for SKINR sequencing".into()));
                    }

                    st.data
                        .write()
                        .unwrap_or_else(|e| e.into_inner())
                        .custom_endpoints
                        .insert(seq_key, (cid as u64, (active_jobs + 1).to_string()));
                    st.sync_to_db();

                    let mut payload = Vec::new();
                    encode_id_field(1, 1001, &mut payload);
                    pending.write().unwrap_or_else(|e| e.into_inner()).push((
                        NoticeScope::Character(cid as u32),
                        GoogleAny {
                            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.StartedNotice"),
                            value: payload,
                        },
                    ));
                } else if req_name.ends_with(".sequencing.job.api.CompleteEarlyRequest") {
                    let seq_key = format!("active_seq_{cid}");
                    let active_jobs = st
                        .data
                        .read()
                        .unwrap_or_else(|e| e.into_inner())
                        .custom_endpoints
                        .get(&seq_key)
                        .and_then(|(_, v)| v.parse::<u64>().ok())
                        .unwrap_or(0);
                    if active_jobs > 0 {
                        st.data
                            .write()
                            .unwrap_or_else(|e| e.into_inner())
                            .custom_endpoints
                            .insert(seq_key, (cid as u64, (active_jobs - 1).to_string()));
                        st.sync_to_db();
                    }

                    let mut payload = Vec::new();
                    encode_id_field(1, 1001, &mut payload);
                    pending.write().unwrap_or_else(|e| e.into_inner()).push((
                        NoticeScope::Character(cid as u32),
                        GoogleAny {
                            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.CompletedNotice"),
                            value: payload,
                        },
                    ));
                } else if req_name.ends_with(".title.api.EquipRequest") || req_name.ends_with(".title.api.UnequipRequest") {
                    let mut payload = Vec::new();
                    encode_id_field(1, cid as u64, &mut payload);
                    encode_id_field(2, 42, &mut payload);
                    encode_varint_field(3, if req_name.ends_with(".UnequipRequest") { 1 } else { 0 }, &mut payload);
                    pending.write().unwrap_or_else(|e| e.into_inner()).push((
                        NoticeScope::Character(cid as u32),
                        GoogleAny {
                            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.cosmetic.character.title.api.SetNotice"),
                            value: payload,
                        },
                    ));
                } else if req_name.ends_with(".draft.api.requests.SaveRequest") || req_name.ends_with(".license.api.requests.SaveRequest") {
                    let mut payload = Vec::new();
                    encode_id_field(1, 501, &mut payload);
                    pending.write().unwrap_or_else(|e| e.into_inner()).push((
                        NoticeScope::Character(cid as u32),
                        GoogleAny {
                            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.cosmetic.ship.skin.thirdparty.license.api.AddedNotice"),
                            value: payload,
                        },
                    ));
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

/// Helper for broadcasting `CheckVersionNotice` (`eve_public.cosmetic.character.title.definition.version.api.CheckVersionNotice`).
pub fn publish_cosmetic_title_version_notice(
    gateway: &PublicGatewayRouter,
    character_id: u32,
    major: u32,
    minor: u32,
    patch: u32,
    now_unix: u64,
) -> Result<usize, eve_proto::BridgeRpcError> {
    let mut ver_buf = Vec::new();
    encode_varint_field(1, major as u64, &mut ver_buf);
    encode_varint_field(2, minor as u64, &mut ver_buf);
    encode_varint_field(3, patch as u64, &mut ver_buf);
    let mut payload = Vec::new();
    encode_bytes_field(1, &ver_buf, &mut payload);
    let any = GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.cosmetic.character.title.definition.version.api.CheckVersionNotice"),
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

/// Helper for broadcasting `SetNotice` (`eve_public.cosmetic.ship.api.SetNotice`).
pub fn publish_cosmetic_ship_set_notice(
    gateway: &PublicGatewayRouter,
    character_id: u32,
    ship_id: u64,
    skin_id: u64,
    now_unix: u64,
) -> Result<usize, eve_proto::BridgeRpcError> {
    let mut state_buf = Vec::new();
    encode_id_field(1, ship_id, &mut state_buf);
    encode_id_field(2, skin_id, &mut state_buf);
    let mut payload = Vec::new();
    encode_bytes_field(1, &state_buf, &mut payload);
    let any = GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.cosmetic.ship.api.SetNotice"),
        value: payload,
    };
    gateway.publish_notice(NoticeScope::Character(character_id), any, now_unix).map(|(_, n)| n)
}

/// Helper for broadcasting `GrantedNotice` (`eve_public.cosmetic.ship.skin.thirdparty.component.license.api.GrantedNotice`).
pub fn publish_cosmetic_component_license_granted_notice(
    gateway: &PublicGatewayRouter,
    character_id: u32,
    kind_id: u64,
    now_unix: u64,
) -> Result<usize, eve_proto::BridgeRpcError> {
    let mut payload = Vec::new();
    encode_id_field(1, kind_id, &mut payload);
    let any = GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.cosmetic.ship.skin.thirdparty.component.license.api.GrantedNotice"),
        value: payload,
    };
    gateway.publish_notice(NoticeScope::Character(character_id), any, now_unix).map(|(_, n)| n)
}

/// Helper for broadcasting `AddedNotice` (`eve_public.cosmetic.ship.skin.thirdparty.license.api.AddedNotice`).
pub fn publish_cosmetic_license_added_notice(
    gateway: &PublicGatewayRouter,
    character_id: u32,
    license_id: u64,
    now_unix: u64,
) -> Result<usize, eve_proto::BridgeRpcError> {
    let mut payload = Vec::new();
    encode_id_field(1, license_id, &mut payload);
    let any = GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.cosmetic.ship.skin.thirdparty.license.api.AddedNotice"),
        value: payload,
    };
    gateway.publish_notice(NoticeScope::Character(character_id), any, now_unix).map(|(_, n)| n)
}

/// Helper for broadcasting `StartedNotice` (`eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.StartedNotice`).
pub fn publish_cosmetic_sequencing_job_started_notice(
    gateway: &PublicGatewayRouter,
    character_id: u32,
    job_id: u64,
    now_unix: u64,
) -> Result<usize, eve_proto::BridgeRpcError> {
    let mut payload = Vec::new();
    encode_id_field(1, job_id, &mut payload);
    let any = GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.StartedNotice"),
        value: payload,
    };
    gateway.publish_notice(NoticeScope::Character(character_id), any, now_unix).map(|(_, n)| n)
}
