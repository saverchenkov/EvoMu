//! Unit tests for `quasar_goal_tale_notices`.

#![forbid(unsafe_code)]

use super::*;
use crate::quasar_gateway::PublicGatewayRouter;
use crate::services::quasar_achievements::{AchievementConditionDef, AchievementDef};
use crate::services::quasar_progression::new_progression_managers;
use crate::services::quasar_progression_extra::CommunityTrackState;
use eve_db::WorldRepository;
use eve_machonet::{service_roles, SessionState};
use eve_proto::quasar::{QuasarDescriptorRegistry, WireMessage};
use eve_sde::SdeRegistry;
use std::collections::HashSet;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

#[test]
fn goal_and_tale_notice_wire_builders_and_scopes() {
    assert_eq!(CORP_GOAL_UUID_PREFIX, b"CORPGOAL");
    assert_eq!(GOAL_STATE_ACTIVE, 1);
    assert_eq!(GOAL_STATE_CLOSED, 2);
    assert_eq!(GOAL_STATE_COMPLETED, 3);

    assert_eq!(
        goal_scope(Some(98_000_001), 90_000_001),
        NoticeScope::Corporation(98_000_001)
    );
    assert_eq!(goal_scope(Some(0), 90_000_001), NoticeScope::Character(90_000_001));
    assert_eq!(goal_scope(None, 90_000_001), NoticeScope::Character(90_000_001));
    assert_eq!(tale_scope(), NoticeScope::Broadcast);

    let uuid = [0x42u8; 16];
    let active_view = GoalAttributesView {
        created_unix: 1_760_000_000,
        creator_id: 90_000_001,
        corp_id: Some(98_000_001),
        name: "Mine Veldspar",
        description: "Deliver 1000 units",
        desired: 1_000,
        current: 250,
    };
    let created_active = goal_created(&uuid, &active_view);
    assert!(created_active.type_url.ends_with("eve_public.goal.api.CreatedNotice"));
    assert!(WireMessage::decode(&created_active.value).is_ok());

    let done_view = GoalAttributesView {
        current: 1_000,
        corp_id: None,
        ..active_view
    };
    let created_done = goal_created(&uuid, &done_view);
    assert!(!created_done.value.is_empty());

    assert!(goal_deleted(&uuid).type_url.ends_with("eve_public.goal.api.DeletedNotice"));
    assert!(goal_closed(&uuid, 90_000_001).type_url.ends_with("eve_public.goal.api.ClosedNotice"));
    assert!(goal_completed(&uuid).type_url.ends_with("eve_public.goal.api.CompletedNotice"));
    assert!(goal_progressed(&uuid, 500).type_url.ends_with("eve_public.goal.api.ProgressedNotice"));
    assert!(goal_name_changed(&uuid, "NewName").type_url.ends_with("eve_public.goal.api.NameChangedNotice"));
    assert!(
        goal_description_changed(&uuid, "NewDesc")
            .type_url
            .ends_with("eve_public.goal.api.DescriptionChangedNotice")
    );
    assert!(
        goal_contributed(&uuid, 90_000_002, 250, 500, 1_760_000_050)
            .type_url
            .ends_with("eve_public.goal.contribution.api.ContributedNotice")
    );

    let t_start = tale_lifecycle(true, 10, 200, 30_000_142);
    assert!(t_start.type_url.ends_with("eve_public.tale.api.StartedNotice"));
    let t_stop = tale_lifecycle(false, 10, 200, 30_000_142);
    assert!(t_stop.type_url.ends_with("eve_public.tale.api.StoppedNotice"));
}

#[test]
fn opportunity_events_and_server_opportunity_recording() {
    let db = Arc::new(WorldRepository::with_demo_seed());
    let sde = shared_sde();
    let mgrs = new_progression_managers(Arc::clone(&db), Arc::clone(&sde));
    let desc = QuasarDescriptorRegistry::open_default_or_empty();
    let now = 1_760_000_000u64;
    let cid = 90_000_001u32;

    let mut sess = SessionState::new(1);
    sess.user_id = Some(1);
    sess.character_id = Some(cid as i64);
    sess.role = service_roles::ROLE_PLAYER_DEFAULT;

    // 1. Client Discovered event
    let mut disc_inner = Vec::new();
    encode_string_field(1, "opp_mining_1", &mut disc_inner);
    let mut disc_opp = Vec::new();
    encode_bytes_field(1, &disc_inner, &mut disc_opp);
    let mut disc_req = Vec::new();
    encode_bytes_field(1, &disc_opp, &mut disc_req);
    handle_opportunity_event(
        &mgrs,
        &desc,
        &sess,
        &GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.opportunity.api.Discovered"),
            value: disc_req,
        },
        now,
    );

    // 2. Client Tracked -> Viewed -> Untracked events
    let mut ch_msg = Vec::new();
    encode_varint_field(1, cid as u64, &mut ch_msg);
    let mut opp_var = Vec::new();
    encode_varint_field(1, 777, &mut opp_var);
    let mut opp_msg = Vec::new();
    encode_bytes_field(1, &opp_var, &mut opp_msg);
    let mut track_req = Vec::new();
    encode_bytes_field(1, &ch_msg, &mut track_req);
    encode_bytes_field(2, &opp_msg, &mut track_req);

    for ev in [
        "eve_public.opportunity.api.Tracked",
        "eve_public.opportunity.api.Viewed",
        "eve_public.opportunity.api.Untracked",
    ] {
        handle_opportunity_event(
            &mgrs,
            &desc,
            &sess,
            &GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{ev}"),
                value: track_req.clone(),
            },
            now,
        );
    }

    // 3. Server-observed opportunity completion
    let defs = vec![AchievementDef {
        id: 5001,
        name: "Destroy 5 Pirates".into(),
        description: "Destroy 5 NPC pirates".into(),
        notification_text: String::new(),
        category_id: 1,
        target: 5,
        score: 10,
        milestone_targets: vec![],
        reward_isk: 0,
        reward_sp: 0,
        is_client_achievement: false,
        conditions: vec![AchievementConditionDef {
            condition_id: 1,
            statistic_event_id: 1,
            event_name: "npc_killed".into(),
            target_value: 5,
        }],
    }];
    record_server_opportunity_event(&db, &defs, 0, &["npc_killed"], 3, now);
    record_server_opportunity_event(&db, &defs, cid, &["npc_killed"], 3, now);
    record_server_opportunity_event(&db, &defs, cid, &["npc_killed"], 3, now + 10);

    let blob: serde_json::Value = db.with_snapshot(|s| {
        serde_json::from_str(s.quasar_state_blobs.get("quasar_opportunities").expect("opp blob")).unwrap()
    });
    let ch_obj = &blob[cid.to_string()];
    assert!(ch_obj["completed"].as_array().unwrap().contains(&serde_json::json!(5001)));
    assert_eq!(ch_obj["progress"]["5001:1"].as_u64(), Some(5));
}

#[test]
fn tale_matching_and_community_track_progression() {
    let sde = shared_sde();
    let mut event_tpls = HashSet::new();
    event_tpls.insert(8888);

    let inc_tpl = crate::services::pve_incursion_sites::INCURSION_TALE_TEMPLATE_ID as u32;
    assert!(tale_matches_completed_encounter(Some(&sde), &event_tpls, inc_tpl, "Incursion", 0));
    assert!(tale_matches_completed_encounter(Some(&sde), &event_tpls, 400, "Triglavian", 400));
    assert!(tale_matches_completed_encounter(Some(&sde), &event_tpls, 500, "Data", 500));
    assert!(!tale_matches_completed_encounter(Some(&sde), &event_tpls, 500, "Data", 501));
    assert!(tale_matches_completed_encounter(Some(&sde), &event_tpls, 8888, "CombatAnomaly", 0));
    assert!(!tale_matches_completed_encounter(Some(&sde), &event_tpls, 8888, "UnknownSite", 8888));

    let db = Arc::new(WorldRepository::with_demo_seed());
    let router = PublicGatewayRouter::new(
        QuasarDescriptorRegistry::open_default_or_empty(),
        Arc::clone(&db),
        Arc::clone(&sde),
    );
    let mgr = &router.progression.extra;
    mgr.active_tales.write().unwrap().insert(1, (8888, 30_000_142, 0));
    mgr.community_tracks.write().unwrap().insert(
        77,
        CommunityTrackState {
            track_id: 77,
            template_id: 77,
            name: "Community Track".into(),
            current_progress: 10,
            target_progress: 100,
            active: true,
            tale_templates: vec![8888],
        },
    );
    let progressed = complete_system_tale_encounter(
        mgr,
        30_000_142,
        "Incursion",
        8888,
        25,
        1_760_000_000,
        &router,
    );
    assert_eq!(progressed.len(), 1);
    assert_eq!(progressed[0].current_progress, 35);
}
