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
    assert_eq!(drained[2].args.len(), 1);
    let PyRep::Tuple(brain_tuple) = &drained[2].args[0] else {
        panic!("expected brainData tuple");
    };
    assert_eq!(brain_tuple.len(), 2);
    assert_eq!(brain_tuple[0], PyRep::Integer(7));
    let PyRep::Bytes(ref gray_matter) = brain_tuple[1] else {
        panic!("expected marshaled grayMatter bytes");
    };
    assert_eq!(
        eve_proto::marshal::decode(gray_matter).expect("decode grayMatter"),
        PyRep::Tuple(vec![
            PyRep::Tuple(Vec::new()),
            PyRep::Tuple(Vec::new()),
            PyRep::Tuple(Vec::new()),
        ])
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

#[test]
fn emit_server_brain_updated_encodes_brain_effect_objectex2_and_monotonic_versions() {
    use eve_sde::{DogmaEffectType, EffectModifierSpec, InvType};
    use std::collections::HashMap;

    let mut sde = SdeRegistry::default();
    sde.types.insert(
        3426,
        InvType {
            type_id: 3426,
            group_id: 269,
            category_id: 16,
            name: "CPU Management".into(),
            mass: 0.0,
            volume: 0.01,
            capacity: 0.0,
            radius: 0.0,
            base_price: 0.0,
            published: true,
            portion_size: 1,
            market_group_id: None,
            race_id: None,
            faction_id: None,
            variation_parent_type_id: None,
            description_id: None,
            icon_id: None,
            graphic_id: None,
            sound_id: None,
            meta_group_id: None,
            meta_level: None,
            tech_level: None,
            default_effect_id: None,
            description: String::new(),
            effect_ids: vec![132, 368, 397],
            default_attributes: HashMap::from([(275, 1.0), (424, 5.0)]),
        },
    );
    sde.effects.insert(
        368,
        DogmaEffectType {
            effect_id: 368,
            name: "skillBoostCpuOutputBonus".into(),
            effect_category: 0,
            modifiers: vec![EffectModifierSpec {
                domain: "itemID".into(),
                func: "ItemModifier".into(),
                modified_attribute_id: 424,
                modifying_attribute_id: 280,
                operator: 4,
                group_id: None,
                skill_type_id: None,
                effect_id: None,
            }],
            ..Default::default()
        },
    );
    sde.effects.insert(
        397,
        DogmaEffectType {
            effect_id: 397,
            name: "cpuOutputBonusPostPercent".into(),
            effect_category: 0,
            modifiers: vec![EffectModifierSpec {
                domain: "shipID".into(),
                func: "ItemModifier".into(),
                modified_attribute_id: 48,
                modifying_attribute_id: 424,
                operator: 6,
                group_id: None,
                skill_type_id: None,
                effect_id: None,
            }],
            ..Default::default()
        },
    );

    let db = Arc::new(WorldRepository::with_demo_seed());
    let char_id = 90_000_001;
    db.acquire_online_lock(1, 501, Some(char_id), true, 1_775_400_000)
        .expect("online lock");
    db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.clone_grade = 2;
        }
        snap.skills.entry(char_id).or_default().insert(
            3426,
            eve_db::TrainedSkillRecord {
                type_id: 3426,
                level: 5,
                skill_points: 256_000.0,
            },
        );
    });
    let ctx = ServerContext::with_sde_and_repository(Arc::new(sde), db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_775_400_000)));
    assert_eq!(ctx.current_brain_version(char_id), 0);

    ctx.push_brain_update(char_id);
    ctx.push_brain_update(char_id);
    assert_eq!(ctx.current_brain_version(char_id), 2);

    let drained = ctx.drain_notifications_for_char(char_id);
    assert_eq!(drained.len(), 2);
    for (idx, expected_ver) in [(0usize, 1i64), (1usize, 2i64)] {
        assert_eq!(drained[idx].event, "OnServerBrainUpdated");
        let PyRep::Tuple(ref parts) = drained[idx].args[0] else {
            panic!("brainData must be tuple");
        };
        assert_eq!(parts[0], PyRep::Integer(expected_ver));
        let PyRep::Bytes(ref raw) = parts[1] else {
            panic!("grayMatter must be marshaled bytes");
        };
        let decoded = eve_proto::marshal::decode(raw).expect("decode grayMatter");
        let PyRep::Tuple(domains) = decoded else {
            panic!("decoded grayMatter must be 3-tuple");
        };
        assert_eq!(domains.len(), 3);
        let PyRep::Tuple(ref ship_effects) = domains[1] else {
            panic!("shipEffects must be tuple");
        };
        assert_eq!(ship_effects.len(), 1);
        let PyRep::ObjectEx {
            is_type_2: true,
            ref header,
            ..
        } = ship_effects[0]
        else {
            panic!("BrainEffect must marshal as PyObjectEx2: {:?}", ship_effects[0]);
        };
        let PyRep::Tuple(hdr_parts) = header.as_ref() else {
            panic!("ObjectEx2 header must be tuple");
        };
        assert_eq!(
            hdr_parts[0],
            PyRep::Tuple(vec![PyRep::Token(
                "eve.common.script.dogma.effect.BrainEffect".into()
            )])
        );
        let state = &hdr_parts[1];
        assert_eq!(state.dict_get("toItemID"), Some(&PyRep::String("shipID".into())));
        assert_eq!(state.dict_get("modifierType"), Some(&PyRep::String("M".into())));
        assert_eq!(state.dict_get("toAttribID"), Some(&PyRep::Integer(48)));
        assert_eq!(state.dict_get("operation"), Some(&PyRep::Integer(6)));
        assert_eq!(state.dict_get("value"), Some(&PyRep::Real(25.0)));
        assert_eq!(state.dict_get("extras"), Some(&PyRep::Tuple(Vec::new())));
        assert_eq!(
            state.dict_get("skills"),
            Some(&PyRep::List(vec![PyRep::Integer(3426)]))
        );
    }

    let char_brain =
        crate::services::inventory_fitting::fitting_module_cycles::build_character_brain_tuple(
            &ctx,
            char_id,
            ctx.current_brain_version(char_id),
        );
    let PyRep::Tuple(cb_parts) = char_brain else {
        panic!("char_brain must be 4-tuple");
    };
    assert_eq!(cb_parts[0], PyRep::Integer(2));
    assert!(matches!(cb_parts[2], PyRep::Tuple(ref v) if v.len() == 1));
}
