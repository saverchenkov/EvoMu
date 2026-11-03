//! Runtime integration tests closing open `rpc:*` parity rows:
//! - `rpc:localizationServer.UpdateLocalizationQAWrap`

use crate::notify_real_emitters_batch7::{new_ctx, rpc, JITA_STATION, PILOT_A, PILOT_B};
use crate::notify_real_emitters_batch9::login_docked;
use eve_db::OwnerNoteRecord;
use eve_machonet::SessionState;
use eve_proto::PyRep;

const LOCALIZATION_NOTE_ID: i64 = -8_960_000_000_000;

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
