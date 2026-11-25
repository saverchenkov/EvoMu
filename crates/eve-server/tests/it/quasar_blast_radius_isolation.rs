//! Regression tests for GH #1120 Track 1: blast-radius isolation (`isolate_side_effect`),
//! fallible `ProtoBuilder`/Quasar notices, and MachoNet RPC resilience when the Quasar
//! descriptor registry is empty (`QuasarDescriptorRegistry::empty_with_builtins()`).

use eve_db::{
    AccountRecord, CharacterRecord, InventoryFlag, InventoryItemRecord, IskCents,
    NeuralAttributes,
};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::quasar::QuasarDescriptorRegistry;
use eve_proto::PyRep;
use eve_server::quasar_gateway::{
    isolate_side_effect, isolate_side_effect_result, NoticeScope, PublicGatewayRouter,
};
use eve_server::router::ServerContext;
use eve_server::services::quasar_cosmetics_live::install_macho_notify_sink;
use glam::DVec3;
use std::collections::HashMap;
use std::sync::Arc;

fn make_context_with_empty_quasar_descriptors() -> ServerContext {
    let mut ctx = ServerContext::new();
    ctx.quasar_gateway = Arc::new(PublicGatewayRouter::new(
        Arc::new(QuasarDescriptorRegistry::empty_with_builtins()),
        Arc::clone(&ctx.db),
        Arc::clone(&ctx.sde),
    ));
    install_macho_notify_sink(&ctx);
    ctx
}

#[tokio::test]
async fn test_empty_quasar_registry_does_not_break_login_select_achievements_and_logoff() {
    let ctx = make_context_with_empty_quasar_descriptors();
    let kw = HashMap::new();
    let mut sess = SessionState::new(88_001);

    ctx.dispatch_rpc(
        &mut sess,
        "authentication",
        None,
        "Login",
        &[
            PyRep::String("demo".into()),
            PyRep::String("demo-secret".into()),
        ],
        &kw,
        None,
    )
    .await
    .expect("Login must succeed with empty Quasar descriptor registry");

    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_001)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID must succeed when Quasar descriptors are empty");
    assert_eq!(sess.character_id, Some(90_000_001));

    let counters = PyRep::Dict(vec![(
        PyRep::String("client_openAgency".into()),
        PyRep::Integer(3),
    )]);
    ctx.dispatch_rpc(
        &mut sess,
        "monolithTaskTrackerMgr",
        None,
        "UpdateClientAchievmentsAndCounters",
        &[
            PyRep::List(vec![PyRep::Integer(102), PyRep::Integer(60)]),
            counters,
        ],
        &kw,
        None,
    )
    .await
    .expect("UpdateClientAchievmentsAndCounters must succeed when Quasar descriptors are empty");

    let logoff = ctx
        .dispatch_rpc(
            &mut sess,
            "userSvc",
            None,
            "UserLogOffCharacter",
            &[],
            &kw,
            None,
        )
        .await
        .expect("UserLogOffCharacter must succeed when Quasar descriptors are empty");
    assert_eq!(logoff, PyRep::Bool(true));
    assert_eq!(sess.character_id, None);
}

#[tokio::test]
async fn test_empty_quasar_registry_does_not_break_warp_and_stargate_jump() {
    let ctx = make_context_with_empty_quasar_descriptors();
    let kw = HashMap::new();

    let jita_sys = 30_000_142u32;
    let jita_gate = ctx
        .sde
        .jumps
        .values()
        .find(|j| j.from_solar_system_id == jita_sys)
        .cloned()
        .expect("Jita stargate");
    let dest_sys = jita_gate.to_solar_system_id;

    let (user_id, char_id, corp_id, ship_id, ship_type_id) =
        (88_002u64, 96_880_002i64, 1_000_125i64, 86_880_002i64, 628u32);
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.insert(
            char_id,
            CharacterRecord {
                character_id: char_id,
                user_id,
                name: format!("Isolation Pilot {char_id}"),
                corporation_id: corp_id,
                alliance_id: None,
                war_faction_id: None,
                station_id: None,
                structure_id: None,
                solar_system_id: jita_sys,
                constellation_id: 20_000_020,
                region_id: 10_000_002,
                active_ship_id: ship_id,
                balance: IskCents::from_isk(10_000_000),
                plex_balance: 0,
                loyalty_points: HashMap::new(),
                standings: HashMap::new(),
                security_status: 0.0,
                clone_grade: 2,
                home_station_id: 60_003_760,
                race_id: 1,
                bloodline_id: 1,
                ancestry_id: 1,
                school_id: 1,
                neural: NeuralAttributes::default(),
                skill_points: 1_000_000.0,
                activity_status: None,
                free_skill_points: 0.0,
                last_clone_jump_unix: 0,
                jump_fatigue_minutes: 0.0,
                jump_cooldown_until_unix: 0,
                jump_fatigue_until_unix: 0,
                description: String::new(),
                title: String::new(),
                gender: 1,
                create_date_unix: 1_700_000_000,
                fleet_id: None,
                autopilot_waypoints: Vec::new(),
                version: 1,
                system_epoch: 0,
            },
        );
        snap.accounts.insert(
            format!("user_{user_id}"),
            AccountRecord {
                user_id,
                username: format!("user_{user_id}"),
                password_token: String::new(),
                role: 0,
                banned_until_unix: None,
                ban_reason: None,
            },
        );
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: ship_type_id,
                owner_id: char_id,
                location_id: jita_sys as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    let _ = ctx
        .db
        .acquire_online_lock(user_id, 88_002, Some(char_id), true, ctx.now_unix());

    let sys_handle = ctx.systems.get_or_spawn(jita_sys);
    let mut cs = ctx.build_type_combat_state(ship_type_id);
    cs.capacitor = 5_000.0;
    cs.max_capacitor = 5_000.0;
    let _ = sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: ship_type_id,
                position: jita_gate.position + DVec3::new(1_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 2.5,
                mass: 10_000_000.0,
                radius: 120.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            cs,
        )
        .await;

    let mut sess = SessionState::new(88_002);
    sess.user_id = Some(user_id);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(corp_id);
    sess.ship_id = Some(ship_id);
    sess.solar_system_id = Some(jita_sys);
    sess.constellation_id = Some(20_000_020);
    sess.region_id = Some(10_000_002);

    ctx.advance_secs(12);
    let beyonce_jita = ctx.bind_object(&sess, "beyonce", jita_sys as i64).unwrap();
    ctx.dispatch_rpc(
        &mut sess,
        "beyonce",
        Some(beyonce_jita),
        "CmdStargateJump",
        &[PyRep::Integer(jita_gate.stargate_id)],
        &kw,
        None,
    )
    .await
    .expect("CmdStargateJump must succeed when Quasar descriptors are empty");
    assert_eq!(sess.solar_system_id, Some(dest_sys));

    ctx.advance_secs(12);
    let far_celestial = ctx
        .sde
        .celestials_in_system(dest_sys)
        .into_iter()
        .find(|c| c.position.length() > 1_000_000.0)
        .expect("far celestial in destination system")
        .celestial_id as i64;
    let beyonce_dest = ctx.bind_object(&sess, "beyonce", dest_sys as i64).unwrap();
    ctx.dispatch_rpc(
        &mut sess,
        "beyonce",
        Some(beyonce_dest),
        "CmdWarpToStuff",
        &[
            PyRep::String("item".into()),
            PyRep::Integer(far_celestial),
            PyRep::Real(0.0),
        ],
        &kw,
        None,
    )
    .await
    .expect("CmdWarpToStuff must succeed when Quasar descriptors are empty");
}

#[test]
fn test_isolate_side_effect_and_thread_local_empty_registry_never_unwind() {
    assert_eq!(isolate_side_effect("it.ok", || 99_u32), Some(99));
    assert_eq!(
        isolate_side_effect("it.panic", || -> u32 {
            panic!("simulated side-effect failure");
        }),
        None
    );
    assert_eq!(
        isolate_side_effect_result("it.err", || Err::<u32, _>("non-fatal error")),
        None
    );

    let ctx = make_context_with_empty_quasar_descriptors();
    assert!(ctx
        .quasar_gateway
        .publish_pb_notice(
            NoticeScope::Broadcast,
            "it.empty_notice",
            eve_proto::pb::message_with_registry(
                Arc::clone(&ctx.quasar_gateway.descriptors),
                "eve_public.goal.api.ProgressedNotice",
            ),
            1_700_000_000,
        )
        .is_none());
    ctx.quasar_gateway
        .progression
        .achievements
        .on_gameplay_event_by_name(
            90_000_001,
            &["destroyHostileNPC"],
            1,
            1_700_000_000,
            &ctx.quasar_gateway,
        );
    ctx.quasar_gateway
        .progression
        .daily_goals
        .on_gameplay_event(90_000_001, 1, 1, &ctx.quasar_gateway);
    ctx.quasar_gateway
        .progression
        .career_goals
        .on_gameplay_event(90_000_001, "combat", 1, &ctx.quasar_gateway);
}
