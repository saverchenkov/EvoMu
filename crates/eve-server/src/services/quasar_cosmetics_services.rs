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
//! - `eve_public.app.eveonline.career.goal.GetAllRequest`, `ClaimRewardRequest`
//! - App telemetry & lifecycle events acknowledgment

#![forbid(unsafe_code)]

use super::quasar_space_services::{
    build_dynamic_response, effective_roles, get_int_field, get_string_field,
};
use crate::quasar_gateway::{PublicGatewayRouter, STATUS_FORBIDDEN};
use eve_db::WorldRepository;
use eve_proto::quasar::{DynamicValue, GoogleEmpty, QuasarMessage};
use std::sync::Arc;

/// Registers all Q-9 cosmetics, heraldry, and app service handlers on the gateway router.
pub fn register_handlers(router: &PublicGatewayRouter, _db: Arc<WorldRepository>) {
    let desc = Arc::clone(&router.descriptors);

    // 1. eve_public.cosmetic.ship.logo handlers (emblem display/clear)
    {
        let d = Arc::clone(&desc);
        router.register_handler(
            "eve_public.cosmetic.ship.logo.DisplayRequest",
            Arc::new(move |ctx, _any, _dyn_msg| {
                let _char_id = match ctx.effective_character_id() {
                    Some(c) => c,
                    None => return Err((STATUS_FORBIDDEN, "Active character required".into())),
                };
                build_dynamic_response(&d, "eve_public.cosmetic.ship.logo.DisplayResponse", |_resp| {})
            }),
        );
    }
    {
        let d = Arc::clone(&desc);
        router.register_handler(
            "eve_public.cosmetic.ship.logo.ClearRequest",
            Arc::new(move |ctx, _any, _dyn_msg| {
                let _char_id = match ctx.effective_character_id() {
                    Some(c) => c,
                    None => return Err((STATUS_FORBIDDEN, "Active character required".into())),
                };
                build_dynamic_response(&d, "eve_public.cosmetic.ship.logo.ClearResponse", |_resp| {})
            }),
        );
    }

    // 2. eve.cosmetic.ship.api.request.GetRequest & GetAllInBubbleRequest
    {
        let d = Arc::clone(&desc);
        router.register_handler(
            "eve.cosmetic.ship.api.request.GetRequest",
            Arc::new(move |_ctx, _any, dyn_msg| {
                let ship_id = get_int_field(dyn_msg, 1).unwrap_or(1_000_001);
                build_dynamic_response(&d, "eve.cosmetic.ship.api.request.GetResponse", |resp| {
                    resp.set(1, DynamicValue::Int64(ship_id));
                })
            }),
        );
    }
    {
        let d = Arc::clone(&desc);
        router.register_handler(
            "eve.cosmetic.ship.api.request.GetAllInBubbleRequest",
            Arc::new(move |_ctx, _any, _dyn_msg| {
                build_dynamic_response(
                    &d,
                    "eve.cosmetic.ship.api.request.GetAllInBubbleResponse",
                    |_resp| {},
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
                let design_id = get_int_field(dyn_msg, 1).unwrap_or(50_001);
                build_dynamic_response(
                    &d,
                    "eve.cosmetic.ship.skin.thirdparty.api.requests.GetResponse",
                    |resp| {
                        resp.set(1, DynamicValue::Int64(design_id));
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
                build_dynamic_response(
                    &d,
                    "eve.cosmetic.ship.skin.thirdparty.sequencing.api.GetTierPricingResponse",
                    |_resp| {},
                )
            }),
        );
    }
    {
        let d = Arc::clone(&desc);
        router.register_handler(
            "eve.cosmetic.ship.skin.thirdparty.sequencing.api.GetTierDurationsForCharacterRequest",
            Arc::new(move |_ctx, _any, _dyn_msg| {
                build_dynamic_response(
                    &d,
                    "eve.cosmetic.ship.skin.thirdparty.sequencing.api.GetTierDurationsForCharacterResponse",
                    |_resp| {},
                )
            }),
        );
    }
    {
        let d = Arc::clone(&desc);
        router.register_handler(
            "eve.cosmetic.ship.skin.thirdparty.sequencing.api.GetEarlyCompletionPricingRequest",
            Arc::new(move |_ctx, _any, _dyn_msg| {
                build_dynamic_response(
                    &d,
                    "eve.cosmetic.ship.skin.thirdparty.sequencing.api.GetEarlyCompletionPricingResponse",
                    |_resp| {},
                )
            }),
        );
    }
    {
        let d = Arc::clone(&desc);
        router.register_handler(
            "eve.cosmetic.ship.skin.thirdparty.sequencing.api.GetSkillBasedJobsCapacityRequest",
            Arc::new(move |_ctx, _any, _dyn_msg| {
                build_dynamic_response(
                    &d,
                    "eve.cosmetic.ship.skin.thirdparty.sequencing.api.GetSkillBasedJobsCapacityResponse",
                    |resp| {
                        resp.set(1, DynamicValue::Uint32(5)); // Max concurrent sequencing jobs
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
                build_dynamic_response(
                    &d,
                    "eve.cosmetic.ship.skin.thirdparty.license.api.GetOwnedResponse",
                    |resp| {
                        resp.set(1, DynamicValue::Uint32(char_id));
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
                let license_id = get_int_field(dyn_msg, 1).unwrap_or(200_001);
                build_dynamic_response(
                    &d,
                    "eve.cosmetic.ship.skin.thirdparty.license.api.GetResponse",
                    |resp| {
                        resp.set(1, DynamicValue::Int64(license_id));
                    },
                )
            }),
        );
    }

    // 6. eve_public.cosmetic.ship.skin.thirdparty.draft.api.requests handlers
    {
        let d = Arc::clone(&desc);
        router.register_handler(
            "eve_public.cosmetic.ship.skin.thirdparty.draft.api.GetAllSavedRequest",
            Arc::new(move |ctx, _any, _dyn_msg| {
                let _char_id = match ctx.effective_character_id() {
                    Some(c) => c,
                    None => return Err((STATUS_FORBIDDEN, "Active character required".into())),
                };
                build_dynamic_response(
                    &d,
                    "eve_public.cosmetic.ship.skin.thirdparty.draft.api.GetAllSavedResponse",
                    |_resp| {},
                )
            }),
        );
    }
    {
        let d = Arc::clone(&desc);
        router.register_handler(
            "eve_public.cosmetic.ship.skin.thirdparty.draft.api.GetRequest",
            Arc::new(move |_ctx, _any, dyn_msg| {
                let draft_id = get_int_field(dyn_msg, 1).unwrap_or(1);
                build_dynamic_response(
                    &d,
                    "eve_public.cosmetic.ship.skin.thirdparty.draft.api.GetResponse",
                    |resp| {
                        resp.set(1, DynamicValue::Int64(draft_id));
                    },
                )
            }),
        );
    }
    {
        let d = Arc::clone(&desc);
        router.register_handler(
            "eve_public.cosmetic.ship.skin.thirdparty.draft.api.SaveRequest",
            Arc::new(move |_ctx, _any, dyn_msg| {
                let name = get_string_field(dyn_msg, 1).unwrap_or_else(|| "My SKINR Draft".into());
                build_dynamic_response(
                    &d,
                    "eve_public.cosmetic.ship.skin.thirdparty.draft.api.SaveResponse",
                    |resp| {
                        resp.set(1, DynamicValue::Int64(12_345)); // New draft ID
                        resp.set(2, DynamicValue::String(name));
                    },
                )
            }),
        );
    }
    {
        let d = Arc::clone(&desc);
        router.register_handler(
            "eve_public.cosmetic.ship.skin.thirdparty.draft.api.UpdateRequest",
            Arc::new(move |_ctx, _any, dyn_msg| {
                let draft_id = get_int_field(dyn_msg, 1).unwrap_or(1);
                build_dynamic_response(
                    &d,
                    "eve_public.cosmetic.ship.skin.thirdparty.draft.api.UpdateResponse",
                    |resp| {
                        resp.set(1, DynamicValue::Int64(draft_id));
                    },
                )
            }),
        );
    }
    {
        let d = Arc::clone(&desc);
        router.register_handler(
            "eve_public.cosmetic.ship.skin.thirdparty.draft.api.DeleteRequest",
            Arc::new(move |_ctx, _any, dyn_msg| {
                let draft_id = get_int_field(dyn_msg, 1).unwrap_or(1);
                build_dynamic_response(
                    &d,
                    "eve_public.cosmetic.ship.skin.thirdparty.draft.api.DeleteResponse",
                    |resp| {
                        resp.set(1, DynamicValue::Int64(draft_id));
                    },
                )
            }),
        );
    }
    {
        let d = Arc::clone(&desc);
        router.register_handler(
            "eve_public.cosmetic.ship.skin.thirdparty.draft.api.GetSaveCapacityRequest",
            Arc::new(move |_ctx, _any, _dyn_msg| {
                build_dynamic_response(
                    &d,
                    "eve_public.cosmetic.ship.skin.thirdparty.draft.api.GetSaveCapacityResponse",
                    |resp| {
                        resp.set(1, DynamicValue::Uint32(20)); // Maximum 20 saved drafts
                    },
                )
            }),
        );
    }

    // 7. eve.cosmetic.corporation.palette.api.requests handlers
    {
        let d = Arc::clone(&desc);
        router.register_handler(
            "eve.cosmetic.corporation.palette.api.GetRequest",
            Arc::new(move |_ctx, _any, dyn_msg| {
                let corp_id = get_int_field(dyn_msg, 1).unwrap_or(98_000_001) as u32;
                build_dynamic_response(
                    &d,
                    "eve.cosmetic.corporation.palette.api.GetResponse",
                    |resp| {
                        resp.set(1, DynamicValue::Uint32(corp_id));
                    },
                )
            }),
        );
    }
    {
        let d = Arc::clone(&desc);
        router.register_handler(
            "eve.cosmetic.corporation.palette.api.GetOwnRequest",
            Arc::new(move |ctx, _any, _dyn_msg| {
                let corp_id = ctx.effective_corporation_id().unwrap_or(98_000_001);
                build_dynamic_response(
                    &d,
                    "eve.cosmetic.corporation.palette.api.GetResponse",
                    |resp| {
                        resp.set(1, DynamicValue::Uint32(corp_id));
                    },
                )
            }),
        );
    }
    {
        let d = Arc::clone(&desc);
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
                build_dynamic_response(
                    &d,
                    "eve.cosmetic.corporation.palette.api.GetResponse",
                    |_resp| {},
                )
            }),
        );
    }

    // 8. eve_public.cosmetic.character.title.api.requests handlers
    {
        let d = Arc::clone(&desc);
        router.register_handler(
            "eve_public.cosmetic.character.title.api.GetOwnedRequest",
            Arc::new(move |ctx, _any, _dyn_msg| {
                let char_id = match ctx.effective_character_id() {
                    Some(c) => c,
                    None => return Err((STATUS_FORBIDDEN, "Active character required".into())),
                };
                build_dynamic_response(
                    &d,
                    "eve_public.cosmetic.character.title.api.GetOwnedResponse",
                    |resp| {
                        resp.set(1, DynamicValue::Uint32(char_id));
                    },
                )
            }),
        );
    }
    {
        let d = Arc::clone(&desc);
        router.register_handler(
            "eve_public.cosmetic.character.title.api.GetEquippedForCharacterRequest",
            Arc::new(move |_ctx, _any, dyn_msg| {
                let char_id = get_int_field(dyn_msg, 1).unwrap_or(90_000_001) as u32;
                build_dynamic_response(
                    &d,
                    "eve_public.cosmetic.character.title.api.GetEquippedForCharacterResponse",
                    |resp| {
                        resp.set(1, DynamicValue::Uint32(char_id));
                    },
                )
            }),
        );
    }
    {
        let d = Arc::clone(&desc);
        router.register_handler(
            "eve_public.cosmetic.character.title.api.EquipRequest",
            Arc::new(move |ctx, _any, dyn_msg| {
                let char_id = match ctx.effective_character_id() {
                    Some(c) => c,
                    None => return Err((STATUS_FORBIDDEN, "Active character required".into())),
                };
                let title_id = get_int_field(dyn_msg, 1).unwrap_or(1);
                build_dynamic_response(
                    &d,
                    "eve_public.cosmetic.character.title.api.EquipResponse",
                    |resp| {
                        resp.set(1, DynamicValue::Uint32(char_id));
                        resp.set(2, DynamicValue::Int64(title_id));
                    },
                )
            }),
        );
    }
    {
        let d = Arc::clone(&desc);
        router.register_handler(
            "eve_public.cosmetic.character.title.api.UnequipRequest",
            Arc::new(move |ctx, _any, _dyn_msg| {
                let char_id = match ctx.effective_character_id() {
                    Some(c) => c,
                    None => return Err((STATUS_FORBIDDEN, "Active character required".into())),
                };
                build_dynamic_response(
                    &d,
                    "eve_public.cosmetic.character.title.api.UnequipResponse",
                    |resp| {
                        resp.set(1, DynamicValue::Uint32(char_id));
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
                let structure_id = get_int_field(dyn_msg, 1).unwrap_or(1_000_000_001);
                build_dynamic_response(
                    &d,
                    "eve_public.cosmetic.structure.paintwork.api.GetResponse",
                    |resp| {
                        resp.set(1, DynamicValue::Int64(structure_id));
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
                let sys_id = get_int_field(dyn_msg, 1).unwrap_or(30_000_142) as u32;
                build_dynamic_response(
                    &d,
                    "eve_public.cosmetic.structure.paintwork.api.GetAllInSolarSystemResponse",
                    |resp| {
                        resp.set(1, DynamicValue::Uint32(sys_id));
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
                let corp_id = get_int_field(dyn_msg, 1).unwrap_or(98_000_001) as u32;
                build_dynamic_response(
                    &d,
                    "eve_public.cosmetic.structure.paintwork.license.api.GetAllOwnedByCorporationResponse",
                    |resp| {
                        resp.set(1, DynamicValue::Uint32(corp_id));
                    },
                )
            }),
        );
    }

    // 10. eve.quasar.config.api.requests.GetConfigRequest
    {
        let d = Arc::clone(&desc);
        router.register_handler(
            "eve.quasar.config.api.GetRequest",
            Arc::new(move |_ctx, _any, _dyn_msg| {
                build_dynamic_response(&d, "eve.quasar.config.api.GetResponse", |_resp| {})
            }),
        );
    }

    // 11. eve_public.app.eveonline.career.goal handlers
    {
        let d = Arc::clone(&desc);
        router.register_handler(
            "eve_public.app.eveonline.career.goal.GetAllRequest",
            Arc::new(move |ctx, _any, _dyn_msg| {
                let char_id = match ctx.effective_character_id() {
                    Some(c) => c,
                    None => return Err((STATUS_FORBIDDEN, "Active character required".into())),
                };
                build_dynamic_response(
                    &d,
                    "eve_public.app.eveonline.career.goal.GetAllResponse",
                    |resp| {
                        resp.set(1, DynamicValue::Uint32(char_id));
                    },
                )
            }),
        );
    }
    {
        let d = Arc::clone(&desc);
        router.register_handler(
            "eve_public.app.eveonline.career.goal.ClaimRewardRequest",
            Arc::new(move |ctx, _any, dyn_msg| {
                let _char_id = match ctx.effective_character_id() {
                    Some(c) => c,
                    None => return Err((STATUS_FORBIDDEN, "Active character required".into())),
                };
                let goal_id = get_int_field(dyn_msg, 1).unwrap_or(1);
                build_dynamic_response(
                    &d,
                    "eve_public.app.eveonline.career.goal.ClaimRewardResponse",
                    |resp| {
                        resp.set(1, DynamicValue::Int64(goal_id));
                        resp.set(2, DynamicValue::Bool(true)); // Reward claimed
                    },
                )
            }),
        );
    }

    // 12. App telemetry lifecycle events acknowledgment
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
}
