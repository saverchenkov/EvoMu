//! `notify_emitters_missing2` — Server-side V24 notification emitters and handlers
//! for missing notification and RPC rows (Batch 2):
//! - `notify:OnOfferReceived`
//! - `notify:OnProgressionTaskWidgetUpdate`
//! - `notify:OnRecommendationsChanged`
//! - `notify:OnServerBrainUpdated`
//! - `notify:OnServerMapDataPush`
//! - `notify:OnSkillBundleInjected`
//! - `notify:OnStartConversation`
//! - `notify:OnUpdateLocalizationTextCache`
//! - `notify:OnUpdateServiceMapping`

#![forbid(unsafe_code)]

use crate::router::{NotificationTarget, ServerContext};
use eve_proto::PyRep;

pub use crate::services::notify_emitters_batch9::emit_skill_bundle_injected;

/// Emits `OnOfferReceived(offerID, offer)` (`2` args, `id_type = "charid"`).
/// Matches `contextualOffers/client/contextualOffersSvc.pyj:ContextualOfferService.OnOfferReceived`.
pub fn emit_offer_received(
    ctx: &ServerContext,
    char_id: i64,
    offer_id: i64,
    offer: Option<PyRep>,
) {
    let now_ft = ctx.now_blueos_filetime_ticks();
    let end_ft = now_ft + 7 * 86_400 * 10_000_000;
    let offer_dict = offer.unwrap_or_else(|| {
        PyRep::Dict(vec![
            (PyRep::String("offerID".into()), PyRep::Integer(offer_id)),
            (PyRep::String("validTo".into()), PyRep::Integer(end_ft)),
            (PyRep::String("validFrom".into()), PyRep::Integer(now_ft)),
            (
                PyRep::String("price".into()),
                PyRep::Dict(vec![
                    (PyRep::String("original".into()), PyRep::String("$9.99".into())),
                    (PyRep::String("offer".into()), PyRep::String("$4.99".into())),
                    (PyRep::String("percentageOff".into()), PyRep::String("-50%".into())),
                ]),
            ),
            (
                PyRep::String("purchaseStep".into()),
                PyRep::Dict(vec![
                    (PyRep::String("Title".into()), PyRep::String("Special Offer".into())),
                    (PyRep::String("Body".into()), PyRep::String("Limited Time Offer".into())),
                    (
                        PyRep::String("Background".into()),
                        PyRep::String(
                            "res:/UI/Texture/classes/contextualOffers/omegaOffer/purchase_bg.png"
                                .into(),
                        ),
                    ),
                    (PyRep::String("ButtonTitle".into()), PyRep::String("BUY".into())),
                    (
                        PyRep::String("PurchaseURL".into()),
                        PyRep::String("https://secure.eveonline.com/Specials".into()),
                    ),
                ]),
            ),
            (
                PyRep::String("notification".into()),
                PyRep::Dict(vec![
                    (
                        PyRep::String("Title".into()),
                        PyRep::String("New offer available".into()),
                    ),
                    (
                        PyRep::String("Body".into()),
                        PyRep::String("Check out this special offer".into()),
                    ),
                    (PyRep::String("Type".into()), PyRep::Integer(1)),
                ]),
            ),
            (
                PyRep::String("tooltip".into()),
                PyRep::Dict(vec![
                    (
                        PyRep::String("header".into()),
                        PyRep::String("Offer Details".into()),
                    ),
                    (
                        PyRep::String("content".into()),
                        PyRep::String("Discounted bundle".into()),
                    ),
                ]),
            ),
        ])
    });

    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnOfferReceived",
        "charid",
        vec![PyRep::Integer(offer_id), offer_dict],
    );
}

/// Emits `OnProgressionTaskWidgetUpdate(task_id, task_widget_id, widget_state)` (`3` args, `id_type = "charid"`).
/// Matches `progression/client/progressionSvc.pyj:ProgressionService.OnProgressionTaskWidgetUpdate`.
pub fn emit_progression_task_widget_update(
    ctx: &ServerContext,
    char_id: i64,
    task_id: i64,
    task_widget_id: i64,
    widget_state: PyRep,
) {
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnProgressionTaskWidgetUpdate",
        "charid",
        vec![
            PyRep::Integer(task_id),
            PyRep::Integer(task_widget_id),
            widget_state,
        ],
    );
}

/// Emits `OnRecommendationsChanged()` (`0` args, `id_type = "charid"`).
/// Matches `eve/client/script/ui/shared/infoPanels/infoPanelRecommendations.pyj:InfoPanelRecommendations.OnRecommendationsChanged`.
pub fn emit_recommendations_changed(ctx: &ServerContext, char_id: i64) {
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnRecommendationsChanged",
        "charid",
        vec![],
    );
}

/// Emits `OnServerBrainUpdated(brainData)` (`1` arg, `id_type = "charid"`).
/// Matches `eve/client/script/dogma/clientDogmaLocation.pyj:DogmaLocation.OnServerBrainUpdated`.
/// V24 client unpacks `brainVersion, grayMatter = brainData` (`eve/common/script/dogma/baseDogmaLocation.pyj:ProcessBrainData`).
pub fn emit_server_brain_updated(ctx: &ServerContext, char_id: i64, brain_version: i64) {
    let brain_data = PyRep::Tuple(vec![PyRep::Integer(brain_version), PyRep::None]);
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnServerBrainUpdated",
        "charid",
        vec![brain_data],
    );
}

/// Emits `OnServerMapDataPush(tricolors, data)` (`2` args, broadcast or targeted).
/// Matches `eve/client/script/ui/shared/maps/starMapSvc.pyj:StarMapSvc.OnServerMapDataPush`.
pub fn emit_server_map_data_push(
    ctx: &ServerContext,
    target: NotificationTarget,
    tricolors: Vec<(f64, f64, f64)>,
    data: Vec<(u32, f64, f64, String, Option<(f64, f64, f64)>)>,
) {
    let tri_reps = tricolors
        .into_iter()
        .map(|(r, g, b)| PyRep::Tuple(vec![PyRep::Real(r), PyRep::Real(g), PyRep::Real(b)]))
        .collect();
    let data_reps = data
        .into_iter()
        .map(|(sys_id, size, scale, text, override_col)| {
            let col_rep = match override_col {
                Some((r, g, b)) => PyRep::Tuple(vec![PyRep::Real(r), PyRep::Real(g), PyRep::Real(b)]),
                None => PyRep::Tuple(vec![PyRep::None, PyRep::None, PyRep::None]),
            };
            PyRep::Tuple(vec![
                PyRep::Integer(i64::from(sys_id)),
                PyRep::Real(size),
                PyRep::Real(scale),
                PyRep::String(text),
                col_rep,
            ])
        })
        .collect();
    ctx.notify(
        target,
        "OnServerMapDataPush",
        "charid",
        vec![PyRep::List(tri_reps), PyRep::List(data_reps)],
    );
}

/// Emits `OnStartConversation(conversation_id, show_continue_button)` (`2` args, `id_type = "charid"`).
/// Matches `conversations/conversationswithhighlights/conversationservice.pyj:ConversationService.OnStartConversation`.
pub fn emit_start_conversation(
    ctx: &ServerContext,
    char_id: i64,
    conversation_id: i64,
    show_continue_button: bool,
) {
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnStartConversation",
        "charid",
        vec![
            PyRep::Integer(conversation_id),
            PyRep::Bool(show_continue_button),
        ],
    );
}

/// Emits `OnUpdateLocalizationTextCache(cacheData)` (`1` arg).
/// Matches `carbon/client/script/localization/localizationClient.pyj:LocalizationClient.OnUpdateLocalizationTextCache`.
pub fn emit_update_localization_text_cache(
    ctx: &ServerContext,
    target: NotificationTarget,
    cache_data: Option<PyRep>,
) {
    let payload = cache_data.unwrap_or_else(|| {
        PyRep::Tuple(vec![
            PyRep::Dict(Vec::new()),
            PyRep::Dict(Vec::new()),
            PyRep::Dict(Vec::new()),
        ])
    });
    ctx.notify(
        target,
        "OnUpdateLocalizationTextCache",
        "charid",
        vec![payload],
    );
}

/// Emits `OnUpdateServiceMapping(svc, nodeID)` (`2` args).
/// Matches `carbon/common/script/net/ServiceCallGPCS.pyj:CoreServiceCall.OnUpdateServiceMapping`.
pub fn emit_update_service_mapping(
    ctx: &ServerContext,
    target: NotificationTarget,
    svc: &str,
    node_id: i64,
) {
    ctx.notify(
        target,
        "OnUpdateServiceMapping",
        "charid",
        vec![PyRep::String(svc.to_string()), PyRep::Integer(node_id)],
    );
}

