//! Unit and mutation-killing tests for `router::session`.

use crate::router::context::{BoundObjectEntry, ServerContext};
use eve_db::WorldRepository;
use eve_machonet::SessionState;
use eve_sde::SdeRegistry;
use std::sync::{Arc, OnceLock};

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn test_ctx() -> ServerContext {
    if let Some(sde) = SHARED_SDE.get() {
        ServerContext::with_sde_and_repository(
            Arc::clone(sde),
            Arc::new(WorldRepository::with_demo_seed()),
        )
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
    } else {
        let ctx = ServerContext::new();
        let _ = SHARED_SDE.set(Arc::clone(&ctx.sde));
        ctx
    }
}

#[test]
fn is_shutting_down_reflects_cluster_state_flag() {
    let ctx = test_ctx();
    assert!(!ctx.is_shutting_down());

    ctx.cluster_state
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .shutdown_in_progress = true;
    assert!(ctx.is_shutting_down());

    ctx.cluster_state
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .shutdown_in_progress = false;
    assert!(!ctx.is_shutting_down());
}

#[test]
fn require_auth_rejects_unauthenticated_and_returns_user_id_when_authenticated() {
    let mut session = SessionState::new(1);
    session.user_id = None;

    let err =
        ServerContext::require_auth(&session).expect_err("unauthenticated session must fail");
    let err_dbg = format!("{err:?}");
    assert!(
        err_dbg.contains("Unauthorized") && err_dbg.contains("Authentication required"),
        "unexpected error: {err_dbg}"
    );

    session.user_id = Some(42_001);
    let uid =
        ServerContext::require_auth(&session).expect("authenticated session must succeed");
    assert_eq!(uid, 42_001);
}

#[test]
fn require_gm_role_checks_auth_and_gm_bitmask() {
    let mut session = SessionState::new(2);
    session.user_id = None;
    session.role = u64::MAX;

    let unauth_err =
        ServerContext::require_gm_role(&session).expect_err("unauthenticated GM must fail");
    assert!(format!("{unauth_err:?}").contains("Unauthorized"));

    session.user_id = Some(77_777);
    session.role = 0;
    let non_gm_err = ServerContext::require_gm_role(&session).expect_err("non-GM user must fail");
    let non_gm_dbg = format!("{non_gm_err:?}");
    assert!(
        non_gm_dbg.contains("CustomError") && non_gm_dbg.contains("GM/Admin role required"),
        "unexpected non-GM error: {non_gm_dbg}"
    );

    session.role = u64::MAX;
    let uid = ServerContext::require_gm_role(&session).expect("GM user must succeed");
    assert_eq!(uid, 77_777);
}

#[test]
fn require_character_validates_both_user_and_character_id() {
    let mut session = SessionState::new(3);
    session.user_id = None;
    session.character_id = Some(90_000_001);

    let unauth_err =
        ServerContext::require_character(&session).expect_err("missing user_id must fail");
    assert!(format!("{unauth_err:?}").contains("Unauthorized"));

    session.user_id = Some(1001);
    session.character_id = None;
    let no_char_err =
        ServerContext::require_character(&session).expect_err("missing character_id must fail");
    let no_char_dbg = format!("{no_char_err:?}");
    assert!(
        no_char_dbg.contains("CharacterNotSelected")
            && no_char_dbg.contains("Character selection required"),
        "unexpected no-char error: {no_char_dbg}"
    );

    session.character_id = Some(90_000_001);
    let (uid, cid) =
        ServerContext::require_character(&session).expect("valid character session must succeed");
    assert_eq!(uid, 1001);
    assert_eq!(cid, 90_000_001);
}

#[test]
fn require_bound_system_validates_presence_and_service_name_and_returns_system_id() {
    let missing_err = ServerContext::require_bound_system(None, "bound_svc_a")
        .expect_err("missing bound object must fail");
    assert_eq!(missing_err.code.as_bytes(), b"BoundObjectRequired");
    assert!(missing_err.message.contains("bound_svc_a"));

    let entry = BoundObjectEntry {
        bound_id: 100,
        session_id: 10,
        user_id: 1001,
        character_id: 90_000_001,
        service: "bound_svc_b".to_string(),
        object_id: 30_000_142,
        oid_str: "N=1:100".to_string(),
        ref_id: 1,
    };

    let mismatch_err = ServerContext::require_bound_system(Some(&entry), "bound_svc_a")
        .expect_err("mismatched bound service must fail");
    assert_eq!(mismatch_err.code.as_bytes(), b"InvalidBoundService");
    assert!(mismatch_err.message.contains("bound_svc_b"));
    assert!(mismatch_err.message.contains("bound_svc_a"));

    let sys_id = ServerContext::require_bound_system(Some(&entry), "bound_svc_b")
        .expect("matching bound service must succeed");
    assert_eq!(sys_id, 30_000_142);
}

#[test]
fn is_shutting_down_recovers_poisoned_lock_and_role_service_boundaries() {
    use eve_machonet::service_roles::{
        ROLE_ADMIN, ROLE_GMH, ROLE_GML, ROLE_PLAYER, ROLE_QA,
    };

    let ctx = test_ctx();
    ctx.cluster_state.write().unwrap().shutdown_in_progress = true;
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _guard = ctx.cluster_state.write().unwrap();
        panic!("deliberately poison cluster_state rwlock");
    }));
    assert!(ctx.cluster_state.is_poisoned());
    assert!(
        !ctx.is_shutting_down(),
        "is_shutting_down falls back to false on poisoned RwLock via unwrap_or(false)"
    );

    let mut sess = SessionState::new(8_900);
    sess.user_id = Some(99);
    for gm_mask in [ROLE_GML, ROLE_GMH, ROLE_ADMIN] {
        sess.role = gm_mask;
        assert_eq!(ServerContext::require_gm_role(&sess).unwrap(), 99);
    }
    sess.role = ROLE_PLAYER | ROLE_QA;
    let qa_err = ServerContext::require_gm_role(&sess).unwrap_err();
    assert_eq!(qa_err.code, "CustomError");

    // Case-sensitive bound service match and boundary object_id values
    let entry = BoundObjectEntry {
        bound_id: 77,
        session_id: 8_900,
        user_id: 99,
        character_id: 90_000_001,
        service: "beyonce".to_string(),
        object_id: 30_000_142,
        oid_str: "30000142".to_string(),
        ref_id: 0,
    };
    let case_err = ServerContext::require_bound_system(Some(&entry), "BEYONCE").unwrap_err(); // rpc-lint: allow test fixture
    assert_eq!(case_err.code, "InvalidBoundService");
    assert_eq!(
        ServerContext::require_bound_system(Some(&entry), "beyonce").unwrap(),
        30_000_142
    );
}
