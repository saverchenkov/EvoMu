//! Runtime integration tests closing open `rpc:*` parity rows:
//! - `rpc:debug.Exec`
//! - `rpc:localizationServer.ReloadFSDPickle`
//! - `rpc:localizationServer.UpdateLocalizationQAWrap`

use crate::notify_real_emitters_batch7::{new_ctx, rpc, JITA_STATION, PILOT_A, PILOT_B};
use crate::notify_real_emitters_batch9::login_docked;
use eve_db::OwnerNoteRecord;
use eve_machonet::SessionState;
use eve_proto::PyRep;

const LOCALIZATION_NOTE_ID: i64 = -8_960_000_000_000;

// parity: rpc:debug.Exec
#[tokio::test]
async fn parity_rpc_debug_exec() {
    eve_testkit::attest(&["rpc:debug.Exec"]);

    let ctx = new_ctx();

    // 1. Unauthenticated caller is rejected.
    let mut unauth = SessionState::new(90_001);
    let err_unauth = rpc(&ctx, &mut unauth, "debug", "Exec", &[PyRep::String("1".into())])
        .await
        .expect_err("unauthenticated debug.Exec must fail");
    assert_eq!(err_unauth.code, "Unauthorized");

    // 2. Authenticated player without GM/Admin/Programmer role is rejected.
    let mut player_sess =
        login_docked(&ctx, 90_002, "demo", "demo-secret", PILOT_A, JITA_STATION).await;
    let err_role = rpc(
        &ctx,
        &mut player_sess,
        "debug",
        "Exec",
        &[PyRep::String("1".into())],
    )
    .await
    .expect_err("non-privileged debug.Exec must fail");
    assert_eq!(err_role.code, "UserError");
    assert_eq!(err_role.message, "NotAuthorized");

    // 3. Privileged Programmer/GM caller can assign variables, evaluate expressions, print stdout, and inspect stderr.
    let mut gm_sess =
        login_docked(&ctx, 90_003, "other", "other-secret", PILOT_B, JITA_STATION).await;
    gm_sess.role |= eve_machonet::service_roles::ROLE_PROGRAMMER;

    let res_assign = rpc(
        &ctx,
        &mut gm_sess,
        "debug",
        "Exec",
        &[
            PyRep::String("answer = 42".into()),
            PyRep::String("console_alpha".into()),
        ],
    )
    .await
    .expect("assignment in debug.Exec");
    assert_eq!(
        res_assign,
        PyRep::Tuple(vec![
            PyRep::None,
            PyRep::String(String::new()),
            PyRep::String(String::new()),
        ])
    );

    let res_eval = rpc(
        &ctx,
        &mut gm_sess,
        "debug",
        "Exec",
        &[
            PyRep::String("answer".into()),
            PyRep::String("console_alpha".into()),
        ],
    )
    .await
    .expect("eval variable in debug.Exec");
    assert_eq!(
        res_eval,
        PyRep::Tuple(vec![
            PyRep::Integer(42),
            PyRep::String(String::new()),
            PyRep::String(String::new()),
        ])
    );

    let res_print = rpc(
        &ctx,
        &mut gm_sess,
        "debug",
        "Exec",
        &[
            PyRep::String("print('hello evomu')".into()),
            PyRep::String("console_alpha".into()),
        ],
    )
    .await
    .expect("print in debug.Exec");
    assert_eq!(
        res_print,
        PyRep::Tuple(vec![
            PyRep::None,
            PyRep::String("hello evomu\n".into()),
            PyRep::String(String::new()),
        ])
    );

    let res_cluster = rpc(
        &ctx,
        &mut gm_sess,
        "debug",
        "Exec",
        &[PyRep::String("cluster.name".into())],
    )
    .await
    .expect("cluster.name in debug.Exec");
    assert_eq!(
        res_cluster,
        PyRep::Tuple(vec![
            PyRep::String("TRANQUILITY".into()),
            PyRep::String(String::new()),
            PyRep::String(String::new()),
        ])
    );

    let res_err = rpc(
        &ctx,
        &mut gm_sess,
        "debug",
        "Exec",
        &[PyRep::String("missing_symbol".into())],
    )
    .await
    .expect("unknown symbol returns stderr in debug.Exec");
    assert_eq!(
        res_err,
        PyRep::Tuple(vec![
            PyRep::None,
            PyRep::String(String::new()),
            PyRep::String("NameError: name 'missing_symbol' is not defined\n".into()),
        ])
    );

    // 4. Verify persistent console state and exec history are recorded.
    ctx.db.with_snapshot(|snap| {
        assert!(snap.service_state_blobs.contains_key("debug:consoles"));
        assert!(snap.service_state_blobs.contains_key("debug:exec_history"));
    });
}

// parity: rpc:localizationServer.ReloadFSDPickle
#[tokio::test]
async fn parity_rpc_localization_reload_fsd_pickle() {
    eve_testkit::attest(&["rpc:localizationServer.ReloadFSDPickle"]);

    let ctx = new_ctx();

    // 1. Unauthenticated caller is rejected.
    let mut unauth = SessionState::new(90_011);
    let err_unauth = rpc(&ctx, &mut unauth, "localizationServer", "ReloadFSDPickle", &[])
        .await
        .expect_err("unauthenticated ReloadFSDPickle must fail");
    assert_eq!(err_unauth.code, "Unauthorized");

    // 2. Non-QA/non-GM caller is rejected.
    let mut player_sess =
        login_docked(&ctx, 90_012, "demo", "demo-secret", PILOT_A, JITA_STATION).await;
    let err_role = rpc(
        &ctx,
        &mut player_sess,
        "localizationServer",
        "ReloadFSDPickle",
        &[],
    )
    .await
    .expect_err("non-QA ReloadFSDPickle must fail");
    assert_eq!(err_role.code, "RoleRequired");

    // 3. QA/GM caller reloads localization pickle and increments persistent generation counter.
    let mut qa_sess =
        login_docked(&ctx, 90_013, "other", "other-secret", PILOT_B, JITA_STATION).await;
    qa_sess.role |= eve_machonet::service_roles::ROLE_QA;

    let r1 = rpc(
        &ctx,
        &mut qa_sess,
        "localizationServer",
        "ReloadFSDPickle",
        &[],
    )
    .await
    .expect("QA ReloadFSDPickle #1");
    assert_eq!(r1, PyRep::Bool(true));

    let r2 = rpc(
        &ctx,
        &mut qa_sess,
        "localizationServer",
        "ReloadFSDPickle",
        &[],
    )
    .await
    .expect("QA ReloadFSDPickle #2");
    assert_eq!(r2, PyRep::Bool(true));

    ctx.db.with_snapshot(|snap| {
        assert_eq!(
            snap.service_state_blobs
                .get("localization:reload_generation")
                .map(String::as_str),
            Some("2")
        );
        assert!(snap
            .service_state_blobs
            .contains_key("localization:last_reload_unix"));
    });

    // 4. Malformed override JSON is rejected with InvalidLocalizationPickle.
    ctx.db.with_mut_snapshot(|snap| {
        snap.owner_notes.insert(
            LOCALIZATION_NOTE_ID,
            OwnerNoteRecord {
                note_id: LOCALIZATION_NOTE_ID,
                character_id: 0,
                item_id: 0,
                label: "localization_overrides".into(),
                note: "{not-valid-json".into(),
                timestamp_unix: 0,
            },
        );
    });
    let err_bad = rpc(
        &ctx,
        &mut qa_sess,
        "localizationServer",
        "ReloadFSDPickle",
        &[],
    )
    .await
    .expect_err("malformed localization pickle must fail");
    assert_eq!(err_bad.message, "InvalidLocalizationPickle");
}

// parity: rpc:localizationServer.UpdateLocalizationQAWrap
#[tokio::test]
async fn parity_rpc_localization_update_qa_wrap() {
    eve_testkit::attest(&["rpc:localizationServer.UpdateLocalizationQAWrap"]);

    let ctx = new_ctx();

    // 1. Unauthenticated caller is rejected.
    let mut unauth = SessionState::new(90_021);
    let err_unauth = rpc(
        &ctx,
        &mut unauth,
        "localizationServer",
        "UpdateLocalizationQAWrap",
        &[PyRep::Bool(true)],
    )
    .await
    .expect_err("unauthenticated UpdateLocalizationQAWrap must fail");
    assert_eq!(err_unauth.code, "Unauthorized");

    // 2. Non-QA/non-GM caller is rejected.
    let mut player_sess =
        login_docked(&ctx, 90_022, "demo", "demo-secret", PILOT_A, JITA_STATION).await;
    let err_role = rpc(
        &ctx,
        &mut player_sess,
        "localizationServer",
        "UpdateLocalizationQAWrap",
        &[PyRep::Bool(true)],
    )
    .await
    .expect_err("non-QA UpdateLocalizationQAWrap must fail");
    assert_eq!(err_role.code, "RoleRequired");

    // 3. Seed a localization override and toggle QA wrap on/off for QA user.
    ctx.db.with_mut_snapshot(|snap| {
        snap.owner_notes.insert(
            LOCALIZATION_NOTE_ID,
            OwnerNoteRecord {
                note_id: LOCALIZATION_NOTE_ID,
                character_id: 0,
                item_id: 0,
                label: "localization_overrides".into(),
                note: r#"{"1001":"Welcome to New Eden"}"#.into(),
                timestamp_unix: 0,
            },
        );
    });

    let mut qa_sess =
        login_docked(&ctx, 90_023, "other", "other-secret", PILOT_B, JITA_STATION).await;
    qa_sess.role |= eve_machonet::service_roles::ROLE_QA;

    let enabled = rpc(
        &ctx,
        &mut qa_sess,
        "localizationServer",
        "UpdateLocalizationQAWrap",
        &[PyRep::Bool(true)],
    )
    .await
    .expect("enable QA wrap");
    assert_eq!(enabled, PyRep::Bool(true));

    let wrapped_changes = rpc(
        &ctx,
        &mut qa_sess,
        "localizationServer",
        "GetAllTextChanges",
        &[],
    )
    .await
    .expect("GetAllTextChanges with QA wrap enabled");
    assert_eq!(
        wrapped_changes,
        PyRep::Dict(vec![(
            PyRep::Integer(1001),
            PyRep::String("[QA:1001] Welcome to New Eden".into()),
        )])
    );

    let disabled = rpc(
        &ctx,
        &mut qa_sess,
        "localizationServer",
        "UpdateLocalizationQAWrap",
        &[PyRep::Bool(false)],
    )
    .await
    .expect("disable QA wrap");
    assert_eq!(disabled, PyRep::Bool(false));

    let plain_changes = rpc(
        &ctx,
        &mut qa_sess,
        "localizationServer",
        "GetAllTextChanges",
        &[],
    )
    .await
    .expect("GetAllTextChanges with QA wrap disabled");
    assert_eq!(
        plain_changes,
        PyRep::Dict(vec![(
            PyRep::Integer(1001),
            PyRep::String("Welcome to New Eden".into()),
        )])
    );
}
