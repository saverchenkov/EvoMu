//! Edge-case behavioral tests for `router::dispatch`.

use crate::router::ServerContext;
use eve_db::{VirtualServerClock, WorldRepository};
use eve_machonet::{service_roles, SessionState};
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn edge_ctx() -> ServerContext {
    let ctx = ServerContext::with_sde_and_repository(
        shared_sde(),
        Arc::new(WorldRepository::with_demo_seed()),
    )
    .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_750_000_000)));
    ctx
}

fn demo_session(ctx: &ServerContext, session_id: u64, role: u64) -> SessionState {
    let char_id = 90_000_001;
    let (uid, corp_id, sol_id, ship_id) = ctx.db.with_snapshot(|snap| {
        let ch = &snap.characters[&char_id];
        (
            ch.user_id,
            ch.corporation_id,
            ch.solar_system_id,
            ch.active_ship_id,
        )
    });
    let mut s = SessionState {
        session_id,
        user_id: Some(uid),
        character_id: Some(char_id),
        corporation_id: Some(corp_id),
        solar_system_id: Some(sol_id),
        station_id: Some(60_003_760),
        ship_id: Some(ship_id),
        role,
        ..Default::default()
    };
    ctx.sync_session(&mut s);
    s
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_gate_exemptions_gm_bypass_and_session_wipe() {
    let ctx = edge_ctx();
    let kw = HashMap::new();

    // Mark cluster shutting down
    ctx.cluster_state.write().unwrap().shutdown_in_progress = true;

    // Exempt calls bypass shutdown gate (not rejected with ClusterShuttingDown)
    let mut player = demo_session(&ctx, 10, service_roles::ROLE_PLAYER);
    let ping_err = ctx
        .dispatch_rpc(&mut player, "authentication", None, "Ping", &[], &kw, None)
        .await
        .unwrap_err();
    assert_eq!(ping_err.code, "UnimplementedRPC");
    let may_pet = ctx
        .dispatch_rpc(&mut player, "petitioner", None, "MayPetition", &[], &kw, None)
        .await;
    assert!(
        may_pet.is_ok(),
        "petitioner.MayPetition must stay exempt during shutdown: {may_pet:?}"
    );

    // GM session bypasses shutdown gate
    let mut gm = demo_session(
        &ctx,
        11,
        service_roles::ROLE_PLAYER | service_roles::ROLE_GML,
    );
    let gm_res = ctx
        .dispatch_rpc(
            &mut gm,
            "timeControl",
            None,
            "ClearSessionTimer",
            &[],
            &kw,
            None,
        )
        .await;
    assert!(gm_res.is_ok(), "GM must bypass shutdown gate: {gm_res:?}");

    // Non-GM gameplay call fails with ClusterShuttingDown and wipes session identity/location fields
    let err = ctx
        .dispatch_rpc(&mut player, "skillMgr2", None, "GetSkills", &[], &kw, None)
        .await
        .unwrap_err();
    assert_eq!(err.code, "ClusterShuttingDown");
    assert!(player.user_id.is_none());
    assert!(player.character_id.is_none());
    assert!(player.corporation_id.is_none());
    assert!(player.station_id.is_none());
    assert!(player.solar_system_id.is_none());
    assert!(player.ship_id.is_none());
    assert_eq!(player.role, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn slash_qa_and_gm_role_boundary_checks() {
    let ctx = edge_ctx();
    let kw = HashMap::new();

    // Non-GM/non-QA player can run slash.SlashCmd("/") (passes role check, returns SlashError "Commands: []")
    // but NOT other slash commands (InsufficientRole)
    let mut player = demo_session(&ctx, 20, service_roles::ROLE_PLAYER);
    let slash_list = ctx
        .dispatch_rpc(
            &mut player,
            "slash",
            None,
            "SlashCmd",
            &[PyRep::String("/".into())],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(slash_list.code, "SlashError");
    assert_eq!(slash_list.message, "Commands: []");
    let slash_denied = ctx
        .dispatch_rpc(
            &mut player,
            "slash",
            None,
            "SlashCmd",
            &[PyRep::String("/help".into())],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(slash_denied.code, "InsufficientRole");

    // ROLE_QA passes central slash/QA gate (returns non-empty command list on '/' and is allowed past dispatch.rs),
    // yet is still denied GM-only services like timeControl and GM-only /help
    let mut qa = demo_session(&ctx, 21, service_roles::ROLE_PLAYER | service_roles::ROLE_QA);
    let qa_list = ctx
        .dispatch_rpc(
            &mut qa,
            "slash",
            None,
            "SlashCmd",
            &[PyRep::String("/".into())],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(qa_list.code, "SlashError");
    assert_ne!(
        qa_list.message, "Commands: []",
        "ROLE_QA should see QA commands in slash list, got: {qa_list:?}"
    );

    let mut gm = demo_session(&ctx, 22, service_roles::ROLE_PLAYER | service_roles::ROLE_GML);
    let gm_help = ctx
        .dispatch_rpc(
            &mut gm,
            "slash",
            None,
            "SlashCmd",
            &[PyRep::String("/help".into())],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(
        gm_help.code, "UnknownSlashCommand",
        "ROLE_GML must pass role gate into slash dispatcher, got: {gm_help:?}"
    );

    let qa_gm_denied = ctx
        .dispatch_rpc(
            &mut qa,
            "timeControl",
            None,
            "ClearSessionTimer",
            &[],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(qa_gm_denied.code, "InsufficientRole");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn macho_bind_validation_fleet_bound_wrap_quasar_and_location_transition() {
    let ctx = edge_ctx();
    let kw = HashMap::new();
    let mut player = demo_session(&ctx, 30, service_roles::ROLE_PLAYER);

    // MachoBindObject invalid empty-string bind key rejected with InvalidArgument
    let empty_bind_err = ctx
        .dispatch_rpc(
            &mut player,
            "beyonce",
            None,
            "MachoBindObject",
            &[PyRep::String(String::new())],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(empty_bind_err.code, "InvalidArgument");

    // MachoBindObject with non-string piggybacked method name rejected with InvalidArgument
    let bad_piggy = ctx
        .dispatch_rpc(
            &mut player,
            "beyonce",
            None,
            "MachoBindObject",
            &[
                PyRep::Integer(30_000_142),
                PyRep::Tuple(vec![PyRep::Integer(12345), PyRep::Tuple(vec![])]),
            ],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(bad_piggy.code, "InvalidArgument");

    // publicGatewaySvc with unknown Quasar domain returns UnknownQuasarDomain
    let bad_quasar = ctx
        .dispatch_rpc(
            &mut player,
            "publicGatewaySvc",
            None,
            "SendRequest",
            &[PyRep::String("nonexistent.invalid.domain".into())],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(bad_quasar.code, "UnknownQuasarDomain");

    // fleetObjectHandler.CreateFleet with machoNet:26000 wraps returned fleet_id as bound object
    let bound_fleet = ctx
        .dispatch_rpc(
            &mut player,
            "fleetObjectHandler",
            None,
            "CreateFleet",
            &[],
            &kw,
            Some("machoNet:26000"),
        )
        .await
        .expect("CreateFleet succeeds");
    let has_bound = matches!(&bound_fleet, PyRep::SubStruct(_))
        || matches!(&bound_fleet, PyRep::Tuple(t) if !t.is_empty())
        || matches!(&bound_fleet, PyRep::Object { .. });
    assert!(
        has_bound,
        "expected bound object wrapper from CreateFleet with machoNet:26000, got {bound_fleet:?}"
    );

    // Location transition during sync_session invalidates previously bound objects
    let bind_id = ctx
        .bind_verified_sub_object(&player, "beyonce", 30_000_142)
        .expect("bind beyonce");
    assert!(ctx.bound_objects.read().unwrap().contains_key(&bind_id));

    // Change character's persisted station_id -> next dispatch_rpc triggers transition invalidation
    let char_id = player.character_id.unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().station_id = Some(60_003_761);
    });
    let _ = ctx
        .dispatch_rpc(&mut player, "skillMgr2", None, "GetSkills", &[], &kw, Some("machoNet:26000"))
        .await
        .expect("GetSkills succeeds");
    assert!(
        !ctx.bound_objects.read().unwrap().contains_key(&bind_id),
        "stale bound object should be invalidated across station change"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn skill_mgr2_get_skills_live_machonet_converts_rowset_to_dict() {
    let ctx = edge_ctx();
    let kw = HashMap::new();
    let mut player = demo_session(&ctx, 40, service_roles::ROLE_PLAYER);

    let rep = ctx
        .dispatch_rpc(
            &mut player,
            "skillMgr2",
            None,
            "GetSkills",
            &[],
            &kw,
            Some("machoNet:26000"),
        )
        .await
        .expect("GetSkills succeeds");
    match rep {
        PyRep::Dict(entries) => {
            assert!(!entries.is_empty(), "expected non-empty skills dict");
            let (_, first_val) = &entries[0];
            assert!(
                first_val.dict_get("skillLevel").is_some()
                    && first_val.dict_get("skillRank").is_some()
                    && first_val.dict_get("effectiveSkillLevel").is_some(),
                "expected V24 client skill attributes in dict value, got {first_val:?}"
            );
        }
        other => panic!("expected Dict for machoNet:26000 GetSkills, got {other:?}"),
    }
}
