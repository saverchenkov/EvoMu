//! Unit tests for `notify_emitters_missing2`.

use super::*;
use crate::router::{NotificationTarget, ServerContext};
use eve_db::clock::VirtualServerClock;
use eve_db::WorldRepository;
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use std::sync::Arc;

fn make_online_ctx() -> (ServerContext, i64) {
    let sde = Arc::new(SdeRegistry::default());
    let db = Arc::new(WorldRepository::with_demo_seed());
    let char_id = 90_000_001;
    db.acquire_online_lock(1, 501, Some(char_id), true, 1_775_400_000)
        .expect("online lock");
    let ctx = ServerContext::with_sde_and_repository(sde, db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_775_400_000)));
    (ctx, char_id)
}

#[test]
fn emit_offer_received_default_and_custom_payloads() {
    let (ctx, char_id) = make_online_ctx();
    let expected_now = ctx.now_blueos_filetime_ticks();
    let expected_end = expected_now + 7 * 86_400 * 10_000_000;

    // Default offer dict
    emit_offer_received(&ctx, char_id, 555, None);
    // Custom offer dict
    let custom = PyRep::Dict(vec![(
        PyRep::String("customKey".into()),
        PyRep::Integer(99),
    )]);
    emit_offer_received(&ctx, char_id, 556, Some(custom.clone()));

    let drained = ctx.drain_notifications_for_char(char_id);
    assert_eq!(drained.len(), 2);
    assert_eq!(drained[0].event, "OnOfferReceived");
    assert_eq!(drained[0].id_type, "charid");
    assert_eq!(drained[0].args[0], PyRep::Integer(555));
    let PyRep::Dict(ref d0) = drained[0].args[1] else {
        panic!("expected default offer dict");
    };
    assert!(d0.contains(&(PyRep::String("offerID".into()), PyRep::Integer(555))));
    assert!(d0.contains(&(PyRep::String("validFrom".into()), PyRep::Integer(expected_now))));
    assert!(d0.contains(&(PyRep::String("validTo".into()), PyRep::Integer(expected_end))));
    assert!(d0.iter().any(|(k, _)| k.as_str() == Some("price")));
    assert!(d0.iter().any(|(k, _)| k.as_str() == Some("purchaseStep")));
    assert!(d0.iter().any(|(k, _)| k.as_str() == Some("notification")));
    assert!(d0.iter().any(|(k, _)| k.as_str() == Some("tooltip")));

    assert_eq!(drained[1].args, vec![PyRep::Integer(556), custom]);
}

#[test]
fn emit_progression_recommendations_brain_conversation_and_service_mapping() {
    let (ctx, char_id) = make_online_ctx();

    emit_progression_task_widget_update(&ctx, char_id, 10, 20, PyRep::String("active".into()));
    emit_recommendations_changed(&ctx, char_id);
    emit_server_brain_updated(&ctx, char_id, 7);
    emit_start_conversation(&ctx, char_id, 3001, true);
    emit_start_conversation(&ctx, char_id, 3002, false);
    emit_update_localization_text_cache(&ctx, NotificationTarget::Character(char_id), None);
    emit_update_localization_text_cache(
        &ctx,
        NotificationTarget::Character(char_id),
        Some(PyRep::Integer(123)),
    );
    emit_update_service_mapping(
        &ctx,
        NotificationTarget::Character(char_id),
        "beyonce",
        888_444,
    );

    let drained = ctx.drain_notifications_for_char(char_id);
    assert_eq!(drained.len(), 8);

    assert_eq!(drained[0].event, "OnProgressionTaskWidgetUpdate");
    assert_eq!(
        drained[0].args,
        vec![
            PyRep::Integer(10),
            PyRep::Integer(20),
            PyRep::String("active".into())
        ]
    );

    assert_eq!(drained[1].event, "OnRecommendationsChanged");
    assert!(drained[1].args.is_empty());

    assert_eq!(drained[2].event, "OnServerBrainUpdated");
    assert_eq!(
        drained[2].args,
        vec![PyRep::Tuple(vec![PyRep::Integer(7), PyRep::None])]
    );

    assert_eq!(drained[3].event, "OnStartConversation");
    assert_eq!(
        drained[3].args,
        vec![PyRep::Integer(3001), PyRep::Bool(true)]
    );
    assert_eq!(
        drained[4].args,
        vec![PyRep::Integer(3002), PyRep::Bool(false)]
    );

    assert_eq!(drained[5].event, "OnUpdateLocalizationTextCache");
    assert_eq!(
        drained[5].args,
        vec![PyRep::Tuple(vec![
            PyRep::Dict(Vec::new()),
            PyRep::Dict(Vec::new()),
            PyRep::Dict(Vec::new()),
        ])]
    );
    assert_eq!(drained[6].args, vec![PyRep::Integer(123)]);

    assert_eq!(drained[7].event, "OnUpdateServiceMapping");
    assert_eq!(
        drained[7].args,
        vec![PyRep::String("beyonce".into()), PyRep::Integer(888_444)]
    );
}

#[test]
fn emit_server_map_data_push_formats_tricolors_and_override_colors() {
    let (ctx, char_id) = make_online_ctx();

    emit_server_map_data_push(
        &ctx,
        NotificationTarget::Character(char_id),
        vec![(0.1, 0.2, 0.3), (0.4, 0.5, 0.6)],
        vec![
            (30_000_142, 1.5, 2.0, "Jita".into(), Some((1.0, 0.0, 0.5))),
            (30_002_187, 0.8, 1.0, "Amarr".into(), None),
        ],
    );

    let drained = ctx.drain_notifications_for_char(char_id);
    assert_eq!(drained.len(), 1);
    assert_eq!(drained[0].event, "OnServerMapDataPush");
    assert_eq!(drained[0].id_type, "charid");
    assert_eq!(
        drained[0].args[0],
        PyRep::List(vec![
            PyRep::Tuple(vec![PyRep::Real(0.1), PyRep::Real(0.2), PyRep::Real(0.3)]),
            PyRep::Tuple(vec![PyRep::Real(0.4), PyRep::Real(0.5), PyRep::Real(0.6)]),
        ])
    );
    assert_eq!(
        drained[0].args[1],
        PyRep::List(vec![
            PyRep::Tuple(vec![
                PyRep::Integer(30_000_142),
                PyRep::Real(1.5),
                PyRep::Real(2.0),
                PyRep::String("Jita".into()),
                PyRep::Tuple(vec![PyRep::Real(1.0), PyRep::Real(0.0), PyRep::Real(0.5)]),
            ]),
            PyRep::Tuple(vec![
                PyRep::Integer(30_002_187),
                PyRep::Real(0.8),
                PyRep::Real(1.0),
                PyRep::String("Amarr".into()),
                PyRep::Tuple(vec![PyRep::None, PyRep::None, PyRep::None]),
            ]),
        ])
    );
}
