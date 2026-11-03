//! Integration tests closing parity for 11 MISSING notify and rpc rows:
//! - notify:OnOfferReceived
//! - notify:OnProgressionTaskWidgetUpdate
//! - notify:OnRecommendationsChanged
//! - notify:OnServerBrainUpdated
//! - notify:OnServerMapDataPush
//! - notify:OnSkillBundleInjected
//! - notify:OnStartConversation
//! - notify:OnUpdateLocalizationTextCache
//! - notify:OnUpdateServiceMapping
//! - rpc:localizationServer.ReloadFSDPickle
//! - rpc:debug.Exec

#![forbid(unsafe_code)]

use crate::common;
use eve_machonet::{service_roles::ROLE_GM_DEFAULT, SessionState};
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::{NotificationTarget, ServerContext};
use eve_server::services::notify_emitters_missing2::{
    emit_offer_received, emit_progression_task_widget_update, emit_recommendations_changed,
    emit_server_brain_updated, emit_server_map_data_push, emit_skill_bundle_injected,
    emit_start_conversation, emit_update_localization_text_cache, emit_update_service_mapping,
};
use std::collections::HashMap;

const PILOT_A: i64 = 90_000_001;
const PILOT_B: i64 = 90_000_002;

fn new_ctx() -> ServerContext {
    let sde = std::sync::Arc::new(
        eve_sde::SdeRegistry::open(&eve_sde::SdeRegistry::default_db_path()).expect("open sde"),
    );
    let db = std::sync::Arc::new(eve_db::WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(sde, db);
    ctx.set_clock_paused(true);
    ctx
}

async fn pilot(ctx: &ServerContext, sid: u64, user: &str, pass: &str, char_id: i64) -> SessionState {
    let mut s = SessionState::new(sid);
    common::login_pilot(ctx, &mut s, user, pass).await;
    let args = [PyRep::Integer(char_id)];
    ctx.dispatch_rpc(
        &mut s,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &args,
        &HashMap::new(),
        None,
    )
    .await
    .expect("SelectCharacterID");
    ctx.drain_notifications_for_char(char_id);
    s
}

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, svc, None, method, args, &HashMap::new(), None)
        .await
}

// parity: notify:OnOfferReceived, notify:OnProgressionTaskWidgetUpdate
// parity: notify:OnRecommendationsChanged, notify:OnServerBrainUpdated
// parity: notify:OnServerMapDataPush, notify:OnSkillBundleInjected
#[tokio::test]
async fn test_parity_notify_missing2_notifications() {
    eve_testkit::attest(&[
        "notify:OnOfferReceived",
        "notify:OnProgressionTaskWidgetUpdate",
        "notify:OnRecommendationsChanged",
        "notify:OnServerBrainUpdated",
        "notify:OnServerMapDataPush",
        "notify:OnSkillBundleInjected",
    ]);

    let ctx = new_ctx();
    let _sess_a = pilot(&ctx, 10_001, "demo", "demo-secret", PILOT_A).await;

    // 1. OnOfferReceived
    // Direct emitter with default generated dictionary
    emit_offer_received(&ctx, PILOT_A, 42, None);
    let notifs = ctx.drain_notifications_for_char(PILOT_A);
    let offer_notif = notifs
        .iter()
        .find(|n| n.event == "OnOfferReceived")
        .expect("OnOfferReceived emitted");
    assert_eq!(offer_notif.args.len(), 2);
    assert_eq!(offer_notif.args[0], PyRep::Integer(42));
    if let PyRep::Dict(entries) = &offer_notif.args[1] {
        let keys: Vec<&str> = entries.iter().filter_map(|(k, _)| k.as_str()).collect();
        assert!(keys.contains(&"offerID"));
        assert!(keys.contains(&"price"));
        assert!(keys.contains(&"purchaseStep"));
        assert!(keys.contains(&"notification"));
        assert!(keys.contains(&"tooltip"));
    } else {
        panic!("Expected Dict for offer argument");
    }

    // Direct emitter with custom offer payload
    let custom_offer = PyRep::Dict(vec![
        (PyRep::String("offerID".into()), PyRep::Integer(99)),
        (PyRep::String("custom".into()), PyRep::Bool(true)),
    ]);
    emit_offer_received(&ctx, PILOT_A, 99, Some(custom_offer.clone()));
    let notifs = ctx.drain_notifications_for_char(PILOT_A);
    assert_eq!(notifs.len(), 1);
    assert_eq!(notifs[0].event, "OnOfferReceived");
    assert_eq!(notifs[0].args[0], PyRep::Integer(99));
    assert_eq!(notifs[0].args[1], custom_offer);

    // 2. OnProgressionTaskWidgetUpdate
    let widget_state = PyRep::Dict(vec![
        (PyRep::String("counter_value".into()), PyRep::Integer(5)),
        (PyRep::String("completed".into()), PyRep::Bool(false)),
    ]);
    emit_progression_task_widget_update(&ctx, PILOT_A, 101, 202, widget_state.clone());
    let notifs = ctx.drain_notifications_for_char(PILOT_A);
    let task_notif = notifs
        .iter()
        .find(|n| n.event == "OnProgressionTaskWidgetUpdate")
        .expect("OnProgressionTaskWidgetUpdate emitted");
    assert_eq!(task_notif.args.len(), 3);
    assert_eq!(task_notif.args[0], PyRep::Integer(101));
    assert_eq!(task_notif.args[1], PyRep::Integer(202));
    assert_eq!(task_notif.args[2], widget_state);

    emit_progression_task_widget_update(&ctx, PILOT_A, 102, 203, PyRep::Integer(7));
    let notifs = ctx.drain_notifications_for_char(PILOT_A);
    assert_eq!(notifs.len(), 1);
    assert_eq!(notifs[0].event, "OnProgressionTaskWidgetUpdate");
    assert_eq!(notifs[0].args[0], PyRep::Integer(102));
    assert_eq!(notifs[0].args[1], PyRep::Integer(203));
    assert_eq!(notifs[0].args[2], PyRep::Integer(7));

    // 3. OnRecommendationsChanged
    emit_recommendations_changed(&ctx, PILOT_A);
    let notifs = ctx.drain_notifications_for_char(PILOT_A);
    let rec_notif = notifs
        .iter()
        .find(|n| n.event == "OnRecommendationsChanged")
        .expect("OnRecommendationsChanged emitted");
    assert_eq!(rec_notif.args.len(), 0);

    // 4. OnServerBrainUpdated
    // V24 client BaseDogmaLocation.ProcessBrainData unpacks (brainVersion, grayMatter) 2-tuple:
    emit_server_brain_updated(&ctx, PILOT_A, 3);
    let notifs = ctx.drain_notifications_for_char(PILOT_A);
    let brain_notif = notifs
        .iter()
        .find(|n| n.event == "OnServerBrainUpdated")
        .expect("OnServerBrainUpdated emitted");
    assert_eq!(brain_notif.args.len(), 1);
    assert_eq!(
        brain_notif.args[0],
        PyRep::Tuple(vec![PyRep::Integer(3), PyRep::None])
    );

    emit_server_brain_updated(&ctx, PILOT_A, 4);
    let notifs = ctx.drain_notifications_for_char(PILOT_A);
    assert_eq!(notifs.len(), 1);
    assert_eq!(notifs[0].event, "OnServerBrainUpdated");
    assert_eq!(
        notifs[0].args[0],
        PyRep::Tuple(vec![PyRep::Integer(4), PyRep::None])
    );

    // 5. OnServerMapDataPush
    let tricolors = vec![(1.0, 0.5, 0.25)];
    let data = vec![(30000142, 2.0, 1.5, "Activity".to_string(), None)];
    emit_server_map_data_push(
        &ctx,
        NotificationTarget::Character(PILOT_A),
        tricolors,
        data,
    );
    let notifs = ctx.drain_notifications_for_char(PILOT_A);
    let map_notif = notifs
        .iter()
        .find(|n| n.event == "OnServerMapDataPush")
        .expect("OnServerMapDataPush emitted");
    assert_eq!(map_notif.args.len(), 2);
    // Arg 0 is tricolors list of 3-tuples
    if let PyRep::List(tris) = &map_notif.args[0] {
        assert_eq!(tris.len(), 1);
        if let PyRep::Tuple(t) = &tris[0] {
            assert_eq!(t.len(), 3);
            assert_eq!(t[0], PyRep::Real(1.0));
            assert_eq!(t[1], PyRep::Real(0.5));
            assert_eq!(t[2], PyRep::Real(0.25));
        } else {
            panic!("Expected 3-tuple for tricolor item");
        }
    } else {
        panic!("Expected List for tricolors");
    }
    // Arg 1 is data list of 5-tuples: (sys_id, blobSize, colorScale, descText, overrideColor)
    if let PyRep::List(data_items) = &map_notif.args[1] {
        assert_eq!(data_items.len(), 1);
        if let PyRep::Tuple(d) = &data_items[0] {
            assert_eq!(d.len(), 5);
            assert_eq!(d[0], PyRep::Integer(30000142));
            assert_eq!(d[1], PyRep::Real(2.0));
            assert_eq!(d[2], PyRep::Real(1.5));
            assert_eq!(d[3], PyRep::String("Activity".into()));
            assert_eq!(d[4], PyRep::Tuple(vec![PyRep::None, PyRep::None, PyRep::None]));
        } else {
            panic!("Expected 5-tuple for map data item");
        }
    } else {
        panic!("Expected List for data items");
    }

    // Direct emitter with override color
    emit_server_map_data_push(
        &ctx,
        NotificationTarget::Character(PILOT_A),
        vec![(0.0, 1.0, 0.0)],
        vec![(30000143, 1.0, 1.0, "Sec".into(), Some((0.1, 0.2, 0.3)))],
    );
    let notifs = ctx.drain_notifications_for_char(PILOT_A);
    assert_eq!(notifs.len(), 1);
    assert_eq!(notifs[0].event, "OnServerMapDataPush");
    if let PyRep::List(data_items) = &notifs[0].args[1] {
        if let PyRep::Tuple(d) = &data_items[0] {
            assert_eq!(
                d[4],
                PyRep::Tuple(vec![PyRep::Real(0.1), PyRep::Real(0.2), PyRep::Real(0.3)])
            );
        }
    }

    // 6. OnSkillBundleInjected
    emit_skill_bundle_injected(&ctx, PILOT_A, 5, 1_250_000);
    let notifs = ctx.drain_notifications_for_char(PILOT_A);
    let bundle_notif = notifs
        .iter()
        .find(|n| n.event == "OnSkillBundleInjected")
        .expect("OnSkillBundleInjected emitted");
    assert_eq!(bundle_notif.args.len(), 2);
    assert_eq!(bundle_notif.args[0], PyRep::Integer(5));
    assert_eq!(bundle_notif.args[1], PyRep::Integer(1_250_000));
}

// parity: notify:OnStartConversation, notify:OnUpdateLocalizationTextCache
// parity: notify:OnUpdateServiceMapping, rpc:localizationServer.ReloadFSDPickle
// parity: rpc:debug.Exec
#[tokio::test]
async fn test_parity_notify_missing2_rpcs_and_cluster() {
    eve_testkit::attest(&[
        "notify:OnStartConversation",
        "notify:OnUpdateLocalizationTextCache",
        "notify:OnUpdateServiceMapping",
        "rpc:localizationServer.ReloadFSDPickle",
        "rpc:debug.Exec",
    ]);

    let ctx = new_ctx();
    let mut sess_a = pilot(&ctx, 20_001, "demo", "demo-secret", PILOT_A).await;
    let mut sess_b = pilot(&ctx, 20_002, "other", "other-secret", PILOT_B).await;

    // Pilot A is GM; Pilot B is normal player
    sess_a.role |= ROLE_GM_DEFAULT;

    // 7. OnStartConversation
    emit_start_conversation(&ctx, PILOT_A, 701, true);
    let notifs = ctx.drain_notifications_for_char(PILOT_A);
    let conv_notif = notifs
        .iter()
        .find(|n| n.event == "OnStartConversation")
        .expect("OnStartConversation emitted");
    assert_eq!(conv_notif.args.len(), 2);
    assert_eq!(conv_notif.args[0], PyRep::Integer(701));
    assert_eq!(conv_notif.args[1], PyRep::Bool(true));

    emit_start_conversation(&ctx, PILOT_A, 702, false);
    let notifs = ctx.drain_notifications_for_char(PILOT_A);
    assert_eq!(notifs.len(), 1);
    assert_eq!(notifs[0].event, "OnStartConversation");
    assert_eq!(notifs[0].args[0], PyRep::Integer(702));
    assert_eq!(notifs[0].args[1], PyRep::Bool(false));

    // 8. OnUpdateLocalizationTextCache & 10. rpc:localizationServer.ReloadFSDPickle
    // Negative: Non-GM pilot calling ReloadFSDPickle fails with RoleRequired
    let non_gm_res = rpc(&ctx, &mut sess_b, "localizationServer", "ReloadFSDPickle", &[]).await;
    assert!(non_gm_res.is_err());
    assert_eq!(non_gm_res.unwrap_err().code, "RoleRequired");

    // Negative (existing test regression check): GM calling ReloadFSDPickle with filename arg returns UnimplementedRPC
    let gm_arg_res = rpc(
        &ctx,
        &mut sess_a,
        "localizationServer",
        "ReloadFSDPickle",
        &[PyRep::String("bad.pickle".into())],
    )
    .await;
    assert!(gm_arg_res.is_err());
    assert_eq!(gm_arg_res.unwrap_err().code, "UnimplementedRPC");

    // Positive: GM calling ReloadFSDPickle with 0 args succeeds and broadcasts OnUpdateLocalizationTextCache
    let _ = ctx.drain_notifications_for_char(PILOT_A);
    let reload_res = rpc(&ctx, &mut sess_a, "localizationServer", "ReloadFSDPickle", &[]).await;
    assert_eq!(reload_res.unwrap(), PyRep::Bool(true));
    let notifs = ctx.drain_notifications_for_char(PILOT_A);
    let loc_notif = notifs
        .iter()
        .find(|n| n.event == "OnUpdateLocalizationTextCache")
        .expect("OnUpdateLocalizationTextCache emitted");
    assert_eq!(loc_notif.args.len(), 1);
    if let PyRep::Tuple(elements) = &loc_notif.args[0] {
        // Unpacks (messagePerLanguage, metaDataPerLanguage, labelsDict)
        assert_eq!(elements.len(), 3);
        assert!(matches!(elements[0], PyRep::Dict(_)));
        assert!(matches!(elements[1], PyRep::Dict(_)));
        assert!(matches!(elements[2], PyRep::Dict(_)));
    } else {
        panic!("Expected 3-tuple for localization text cache argument");
    }

    // Direct emitter
    emit_update_localization_text_cache(&ctx, NotificationTarget::Character(PILOT_A), None);
    let notifs = ctx.drain_notifications_for_char(PILOT_A);
    assert_eq!(notifs.len(), 1);
    assert_eq!(notifs[0].event, "OnUpdateLocalizationTextCache");

    // 9. OnUpdateServiceMapping
    emit_update_service_mapping(&ctx, NotificationTarget::Character(PILOT_A), "chatServer", 1001);
    let notifs = ctx.drain_notifications_for_char(PILOT_A);
    let svc_notif = notifs
        .iter()
        .find(|n| n.event == "OnUpdateServiceMapping")
        .expect("OnUpdateServiceMapping emitted");
    assert_eq!(svc_notif.args.len(), 2);
    assert_eq!(svc_notif.args[0], PyRep::String("chatServer".into()));
    assert_eq!(svc_notif.args[1], PyRep::Integer(1001));

    emit_update_service_mapping(&ctx, NotificationTarget::Character(PILOT_A), "marketProxy", 2002);
    let notifs = ctx.drain_notifications_for_char(PILOT_A);
    assert_eq!(notifs.len(), 1);
    assert_eq!(notifs[0].event, "OnUpdateServiceMapping");
    assert_eq!(notifs[0].args[0], PyRep::String("marketProxy".into()));
    assert_eq!(notifs[0].args[1], PyRep::Integer(2002));

    // 11. rpc:debug.Exec
    // Negative: Non-GM pilot calling debug.Exec fails
    let non_gm_debug_res = rpc(&ctx, &mut sess_b, "debug", "Exec", &[PyRep::String("ping".into())]).await;
    assert!(non_gm_debug_res.is_err());
    assert!(non_gm_debug_res.unwrap_err().code.contains("UserError"));

    // Negative (existing test regression check): GM calling status or self.stats() returns UnimplementedRPC
    let gm_status_res = rpc(&ctx, &mut sess_a, "debug", "Exec", &[PyRep::String("status".into())]).await;
    assert!(gm_status_res.is_err());
    assert_eq!(gm_status_res.unwrap_err().code, "UnimplementedRPC");

    let gm_stats_res = rpc(&ctx, &mut sess_a, "debug", "Exec", &[PyRep::String("self.stats()".into())]).await;
    assert!(gm_stats_res.is_err());
    assert_eq!(gm_stats_res.unwrap_err().code, "UnimplementedRPC");

    // Positive: GM calling dev command returns compliant reply shape: iterable of tuples with [1] and [2]
    let ping_res = rpc(&ctx, &mut sess_a, "debug", "Exec", &[PyRep::String("ping".into())]).await;
    assert!(ping_res.is_ok());
    if let PyRep::List(entries) = ping_res.unwrap() {
        assert!(!entries.is_empty());
        if let PyRep::Tuple(row) = &entries[0] {
            assert!(row.len() >= 3);
            assert_eq!(row[0], PyRep::String("server".into()));
            assert!(matches!(row[1], PyRep::Integer(_)));
            assert!(matches!(row[2], PyRep::String(_)));
        } else {
            panic!("Expected Tuple in debug.Exec reply list");
        }
    } else {
        panic!("Expected List reply from debug.Exec");
    }
}
