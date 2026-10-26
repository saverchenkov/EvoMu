//! Integration tests for verified solid RPC parity rows:
//! `charMgr.SaveCharacterSetting` and `charMgr.DeleteCharacterSetting`.

#![forbid(unsafe_code)]

use crate::common;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use std::collections::HashMap;

async fn setup_pilot(ctx: &ServerContext, sid: u64, char_id: i64) -> SessionState {
    let kw = HashMap::new();
    let mut sess = SessionState::new(sid);
    common::login_pilot(ctx, &mut sess, "demo", "demo-secret").await;
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");
    sess
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

#[tokio::test]
// parity: feat:ui.server-settings-storage
// parity: rpc:charMgr.DeleteCharacterSetting
// parity: rpc:charMgr.SaveCharacterSetting
async fn test_rpc_solid_charmgr_settings_persistence() {
    eve_testkit::attest(&[
        "feat:ui.server-settings-storage",
        "rpc:charMgr.DeleteCharacterSetting",
        "rpc:charMgr.SaveCharacterSetting",
    ]);

    let ctx = ServerContext::new();
    let char_id = 90_000_001i64;
    let mut sess = setup_pilot(&ctx, 41_101, char_id).await;

    // 1. Initial settings empty
    let init_settings = rpc(&ctx, &mut sess, "charMgr", "GetCharacterSettings", &[])
        .await
        .expect("GetCharacterSettings initial");
    assert_eq!(init_settings, PyRep::Dict(vec![]));

    // 2. Save settings
    let saved1 = rpc(
        &ctx,
        &mut sess,
        "charMgr",
        "SaveCharacterSetting",
        &[PyRep::String("overview_tab".into()), PyRep::Integer(3)],
    )
    .await
    .expect("SaveCharacterSetting 1");
    assert_eq!(saved1, PyRep::Bool(true));

    let saved2 = rpc(
        &ctx,
        &mut sess,
        "charMgr",
        "SaveCharacterSetting",
        &[PyRep::String("audio_mute".into()), PyRep::Bool(false)],
    )
    .await
    .expect("SaveCharacterSetting 2");
    assert_eq!(saved2, PyRep::Bool(true));

    // 3. Verify retrieved settings contain both
    let cur_settings = rpc(&ctx, &mut sess, "charMgr", "GetCharacterSettings", &[])
        .await
        .expect("GetCharacterSettings after save");
    assert_eq!(cur_settings.dict_get("overview_tab"), Some(&PyRep::Integer(3)));
    assert_eq!(cur_settings.dict_get("audio_mute"), Some(&PyRep::Bool(false)));

    // 4. Persistence round-trip across new context state load
    let loaded = eve_server::services::cluster_macho_misc_services::ClusterMiscRuntimeState::from_db(&ctx.db);
    assert_eq!(
        loaded.char_settings.get(&char_id).and_then(|m| m.get("overview_tab")),
        Some(&PyRep::Integer(3))
    );

    // 5. Delete one setting
    let deleted = rpc(
        &ctx,
        &mut sess,
        "charMgr",
        "DeleteCharacterSetting",
        &[PyRep::String("overview_tab".into())],
    )
    .await
    .expect("DeleteCharacterSetting");
    assert_eq!(deleted, PyRep::Bool(true));

    // 6. Verify setting deleted and other setting preserved
    let updated_settings = rpc(&ctx, &mut sess, "charMgr", "GetCharacterSettings", &[])
        .await
        .expect("GetCharacterSettings after delete");
    assert_eq!(updated_settings.dict_get("overview_tab"), None);
    assert_eq!(updated_settings.dict_get("audio_mute"), Some(&PyRep::Bool(false)));

    // 7. Error validation paths
    let err_empty_save = rpc(
        &ctx,
        &mut sess,
        "charMgr",
        "SaveCharacterSetting",
        &[PyRep::String("".into()), PyRep::Integer(1)],
    )
    .await
    .expect_err("Empty key save should fail");
    assert_eq!(err_empty_save.code, "CustomError");

    let err_empty_del = rpc(
        &ctx,
        &mut sess,
        "charMgr",
        "DeleteCharacterSetting",
        &[PyRep::String("".into())],
    )
    .await
    .expect_err("Empty key delete should fail");
    assert_eq!(err_empty_del.code, "CustomError");

    let long_key = "k".repeat(200);
    let err_long_key = rpc(
        &ctx,
        &mut sess,
        "charMgr",
        "SaveCharacterSetting",
        &[PyRep::String(long_key), PyRep::Integer(1)],
    )
    .await
    .expect_err("Overly long key should fail");
    assert_eq!(err_long_key.code, "CustomError");

    let huge_val = "v".repeat(5_000);
    let err_huge_val = rpc(
        &ctx,
        &mut sess,
        "charMgr",
        "SaveCharacterSetting",
        &[PyRep::String("valid_key".into()), PyRep::String(huge_val)],
    )
    .await
    .expect_err("Overly large value should fail");
    assert_eq!(err_huge_val.code, "CustomError");

    // 8. Unauthenticated call rejects cleanly
    let mut unauth_sess = SessionState::new(41_102);
    let err_unauth = rpc(
        &ctx,
        &mut unauth_sess,
        "charMgr",
        "SaveCharacterSetting",
        &[PyRep::String("k".into()), PyRep::Integer(1)],
    )
    .await
    .expect_err("Unauthenticated session should fail");
    assert_eq!(err_unauth.code, "Unauthorized");

    // 9. Verify settings restored upon new session login
    let mut sess2 = setup_pilot(&ctx, 41_103, char_id).await;
    let login_settings = rpc(&ctx, &mut sess2, "charMgr", "GetCharacterSettings", &[])
        .await
        .expect("GetCharacterSettings restored on login");
    assert_eq!(login_settings.dict_get("audio_mute"), Some(&PyRep::Bool(false)));
    assert_eq!(login_settings.dict_get("overview_tab"), None);
}
