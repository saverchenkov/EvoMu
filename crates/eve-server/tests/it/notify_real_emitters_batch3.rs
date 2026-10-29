//! Real server-side emission tests for Batch 3:
//! GM slash commands, RPC handlers, combat damage updates, session changes, and faction warfare.

use crate::common;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::HashMap;

const P1: i64 = 90_000_001;
const P2: i64 = 90_000_002;

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
    ctx.dispatch_rpc(&mut s, "charUnboundMgr", None, "SelectCharacterID", &args, &HashMap::new(), None)
        .await
        .expect("SelectCharacterID");
    ctx.advance_secs(12);
    ctx.drain_notifications_for_char(char_id);
    s
}

fn events(ctx: &ServerContext, char_id: i64, event: &str) -> Vec<Vec<PyRep>> {
    let all = ctx.drain_notifications_for_char(char_id);
    let mut matched = Vec::new();
    let mut keep = Vec::new();
    for n in all {
        if n.event == event {
            matched.push(n.args);
        } else {
            keep.push(n);
        }
    }
    if !keep.is_empty() {
        if let Ok(mut guard) = ctx.pending_notifications.write() {
            guard.entry(char_id).or_default().extend(keep);
        }
    }
    matched
}

#[tokio::test]
async fn test_batch3_gm_and_admin_rpc_notifications() {
    // parity: notify:OnAchievementsReset, notify:OnCharacterPermanentlyBanned, notify:OnCharacterTemporarilyBanned
    // parity: notify:OnCharacterUnbanned, notify:OnCharacterPokedByGM, notify:OnClusterShutdownCancelled
    // parity: notify:OnGlobalConfigUpdated, notify:OnFastCheckoutEnabledChanged, notify:OnCampaignEnrollmentChanged
    // parity: notify:OnModuleAttributeChange, notify:OnLocationAttributeChange
    eve_testkit::attest(&[
        "notify:OnAchievementsReset",
        "notify:OnCharacterPermanentlyBanned",
        "notify:OnCharacterTemporarilyBanned",
        "notify:OnCharacterUnbanned",
        "notify:OnCharacterPokedByGM",
        "notify:OnClusterShutdownCancelled",
        "notify:OnGlobalConfigUpdated",
        "notify:OnFastCheckoutEnabledChanged",
        "notify:OnCampaignEnrollmentChanged",
        "notify:OnModuleAttributeChange",
        "notify:OnLocationAttributeChange",
    ]);
    let ctx = new_ctx();
    let mut s1 = pilot(&ctx, 801, "demo", "demo-secret", P1).await;
    let mut s2 = pilot(&ctx, 802, "other", "other-secret", P2).await;

    // Give s1 GM roles for GM slash and admin commands
    s1.role = eve_machonet::service_roles::ROLE_GMH | eve_machonet::service_roles::ROLE_ADMIN;

    // 1. OnAchievementsReset via slash.SlashCmd with state seeding and reset verification
    ctx.quasar_gateway.progression.achievements.unlock_achievement(P1 as u32, 2);
    ctx.quasar_gateway.progression.achievements.set_category_score(P1 as u32, 2, 50);
    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = eve_server::services::cosmetics_ui_ops::load_cosmetics_ui_state(snap);
        ext.achievements_by_char.entry(P1).or_default().push(101);
        ext.achievement_counters_by_char.entry(P1).or_default().insert("jumps".into(), 5);
        eve_server::services::cosmetics_ui_ops::save_cosmetics_ui_state(snap, &ext);
    });
    assert!(ctx.quasar_gateway.progression.achievements.character_achievements.read().unwrap().contains_key(&(P1 as u32)));
    assert!(ctx.quasar_gateway.progression.achievements.category_scores.read().unwrap().contains_key(&(P1 as u32, 2)));
    assert!(ctx.quasar_gateway.progression.achievements.total_character_score(P1 as u32) > 0);

    let slash_ach = ctx
        .dispatch_rpc(&mut s1, "slash", None, "SlashCmd", &[PyRep::String("/achievements reset 90000001".into())], &HashMap::new(), None)
        .await
        .expect("SlashCmd /achievements reset");
    assert!(matches!(slash_ach, PyRep::KeyVal(_)));
    let ev = events(&ctx, P1, "OnAchievementsReset");
    assert_eq!(ev.len(), 1);
    assert_eq!(ev[0].len(), 0, "OnAchievementsReset payload expects 0 args");

    // Backing state is completely cleared
    assert!(ctx.quasar_gateway.progression.achievements.character_achievements.read().unwrap().get(&(P1 as u32)).is_none());
    assert!(ctx.quasar_gateway.progression.achievements.category_scores.read().unwrap().get(&(P1 as u32, 2)).is_none());
    assert_eq!(ctx.quasar_gateway.progression.achievements.total_character_score(P1 as u32), 0);
    let machonet_achs = ctx
        .dispatch_rpc(&mut s1, "monolithTaskTrackerMgr", None, "GetCompletedAchievementsAndClientEventCount", &[], &HashMap::new(), None)
        .await
        .expect("monolithTaskTrackerMgr.GetCompletedAchievementsAndClientEventCount");
    let completed_dict = machonet_achs.dict_get("completedDict").expect("completedDict");
    assert_eq!(completed_dict, &PyRep::Dict(vec![]));

    // 2. OnCharacterPermanentlyBanned via /chatban perm
    let ban_perm = ctx
        .dispatch_rpc(&mut s1, "slash", None, "SlashCmd", &[PyRep::String("/chatban 90000001 perm".into())], &HashMap::new(), None)
        .await
        .expect("/chatban perm");
    assert!(matches!(ban_perm, PyRep::KeyVal(_)));
    let ev = events(&ctx, P1, "OnCharacterPermanentlyBanned");
    assert_eq!(ev.len(), 1);
    assert_eq!(ev[0].len(), 0, "OnCharacterPermanentlyBanned payload expects 0 args");

    // 3. OnCharacterTemporarilyBanned via /chatban 3600
    let ban_temp_start = ctx.now_blueos_filetime_ticks();
    let ban_temp = ctx
        .dispatch_rpc(&mut s1, "slash", None, "SlashCmd", &[PyRep::String("/chatban 90000001 3600".into())], &HashMap::new(), None)
        .await
        .expect("/chatban temp");
    assert!(matches!(ban_temp, PyRep::KeyVal(_)));
    let ev = events(&ctx, P1, "OnCharacterTemporarilyBanned");
    assert_eq!(ev.len(), 1);
    assert_eq!(ev[0].len(), 1, "OnCharacterTemporarilyBanned payload expects 1 arg [expiry_ft]");
    assert_eq!(ev[0][0], PyRep::Integer(ban_temp_start + (3600 * 10_000_000)));

    // 4. OnCharacterUnbanned via /chatunban
    let unban = ctx
        .dispatch_rpc(&mut s1, "slash", None, "SlashCmd", &[PyRep::String("/chatunban 90000001".into())], &HashMap::new(), None)
        .await
        .expect("/chatunban");
    assert!(matches!(unban, PyRep::KeyVal(_)));
    let ev = events(&ctx, P1, "OnCharacterUnbanned");
    assert_eq!(ev.len(), 1);
    assert_eq!(ev[0].len(), 0, "OnCharacterUnbanned payload expects 0 args");

    // 5. OnCharacterPokedByGM via mailMgr.PokePlayerAboutChatMsgGm with non-GM negative test
    let neg_poke = ctx
        .dispatch_rpc(&mut s2, "mailMgr", None, "PokePlayerAboutChatMsgGm", &[PyRep::Integer(P1), PyRep::String("help".into())], &HashMap::new(), None)
        .await
        .unwrap_err();
    assert_eq!(neg_poke.code, "UserError");

    ctx.dispatch_rpc(&mut s1, "mailMgr", None, "PokePlayerAboutChatMsgGm", &[PyRep::Integer(P2), PyRep::String("ChannelPolicy".into())], &HashMap::new(), None)
        .await
        .expect("PokePlayerAboutChatMsgGm");
    let ev = events(&ctx, P2, "OnCharacterPokedByGM");
    assert_eq!(ev.len(), 1);
    assert_eq!(ev[0].len(), 2, "OnCharacterPokedByGM expects 2 args [gm_id, channel_name]");
    assert_eq!(ev[0][0], PyRep::Integer(P1));
    assert_eq!(ev[0][1], PyRep::String("ChannelPolicy".into()));

    // 6. OnClusterShutdownCancelled via /shutdown cancel
    ctx.dispatch_rpc(&mut s1, "slash", None, "SlashCmd", &[PyRep::String("/shutdown cancel UI/Shared/Delayed".into())], &HashMap::new(), None)
        .await
        .expect("/shutdown cancel");
    let ev = events(&ctx, P1, "OnClusterShutdownCancelled");
    assert_eq!(ev.len(), 1);
    assert_eq!(ev[0].len(), 1);
    assert_eq!(ev[0][0], PyRep::String("UI/Shared/Delayed".into()));

    // 7. OnGlobalConfigUpdated via machoNet.SetGlobalConfigValue with non-GM negative test and V24 kwargs
    let mut neg_cfg_kw = HashMap::new();
    neg_cfg_kw.insert("key".into(), PyRep::String("fleet_broadcast_cap".into()));
    neg_cfg_kw.insert("value".into(), PyRep::String("50".into()));
    let neg_cfg = ctx
        .dispatch_rpc(&mut s2, "machoNet", None, "SetGlobalConfigValue", &[], &neg_cfg_kw, None)
        .await
        .unwrap_err();
    assert_eq!(neg_cfg.code, "InsufficientRole");

    let mut gm_cfg_kw = HashMap::new();
    gm_cfg_kw.insert("key".into(), PyRep::String("fleet_broadcast_cap".into()));
    gm_cfg_kw.insert("value".into(), PyRep::String("50".into()));
    ctx.dispatch_rpc(&mut s1, "machoNet", None, "SetGlobalConfigValue", &[], &gm_cfg_kw, None)
        .await
        .expect("SetGlobalConfigValue kwargs");
    let ev = events(&ctx, P1, "OnGlobalConfigUpdated");
    assert_eq!(ev.len(), 1);
    assert_eq!(ev[0].len(), 2, "OnGlobalConfigUpdated expects [key, val]");
    assert_eq!(ev[0][0], PyRep::String("fleet_broadcast_cap".into()));
    assert_eq!(ev[0][1], PyRep::String("50".into()));

    // Verify PyRep::None is preserved (not converted to empty string)
    let mut null_cfg_kw = HashMap::new();
    null_cfg_kw.insert("key".into(), PyRep::String("null_key".into()));
    null_cfg_kw.insert("value".into(), PyRep::None);
    ctx.dispatch_rpc(&mut s1, "machoNet", None, "SetGlobalConfigValue", &[], &null_cfg_kw, None)
        .await
        .expect("SetGlobalConfigValue None");
    let ev_null = events(&ctx, P1, "OnGlobalConfigUpdated");
    assert_eq!(ev_null.len(), 1);
    assert_eq!(ev_null[0].len(), 2);
    assert_eq!(ev_null[0][0], PyRep::String("null_key".into()));
    assert_eq!(ev_null[0][1], PyRep::None);

    // 8. OnFastCheckoutEnabledChanged via /fastcheckout with state verification
    let fc_init = ctx
        .dispatch_rpc(&mut s1, "FastCheckoutService", None, "IsFastCheckoutEnabledForUser", &[], &HashMap::new(), None)
        .await
        .expect("IsFastCheckoutEnabledForUser initial");
    assert_eq!(fc_init, PyRep::Bool(false));

    ctx.dispatch_rpc(&mut s1, "slash", None, "SlashCmd", &[PyRep::String("/fastcheckout 1".into())], &HashMap::new(), None)
        .await
        .expect("/fastcheckout 1");
    let ev = events(&ctx, P1, "OnFastCheckoutEnabledChanged");
    assert_eq!(ev.len(), 1);
    assert_eq!(ev[0].len(), 1);
    assert_eq!(ev[0][0], PyRep::Bool(true));
    let fc_after = ctx
        .dispatch_rpc(&mut s1, "FastCheckoutService", None, "IsFastCheckoutEnabledForUser", &[], &HashMap::new(), None)
        .await
        .expect("IsFastCheckoutEnabledForUser after enable");
    assert_eq!(fc_after, PyRep::Bool(true));

    ctx.dispatch_rpc(&mut s1, "slash", None, "SlashCmd", &[PyRep::String("/fastcheckout 0".into())], &HashMap::new(), None)
        .await
        .expect("/fastcheckout 0");
    let ev_off = events(&ctx, P1, "OnFastCheckoutEnabledChanged");
    assert_eq!(ev_off.len(), 1);
    assert_eq!(ev_off[0], vec![PyRep::Bool(false)]);
    let fc_disabled = ctx
        .dispatch_rpc(&mut s1, "FastCheckoutService", None, "IsFastCheckoutEnabledForUser", &[], &HashMap::new(), None)
        .await
        .expect("IsFastCheckoutEnabledForUser after disable");
    assert_eq!(fc_disabled, PyRep::Bool(false));

    // 9. OnCampaignEnrollmentChanged via /campaign enroll 2
    ctx.dispatch_rpc(&mut s1, "slash", None, "SlashCmd", &[PyRep::String("/campaign enroll 2".into())], &HashMap::new(), None)
        .await
        .expect("/campaign enroll");
    let ev = events(&ctx, P1, "OnCampaignEnrollmentChanged");
    assert_eq!(ev.len(), 1);
    assert_eq!(ev[0].len(), 0);

    // 10 & 11. OnModuleAttributeChange & OnLocationAttributeChange via /setattr with strict non-GM negative test
    let neg_setattr = ctx
        .dispatch_rpc(&mut s2, "slash", None, "SlashCmd", &[PyRep::String("/setattr 7001 9 175.0".into())], &HashMap::new(), None)
        .await
        .unwrap_err();
    assert_eq!(neg_setattr.code, "InsufficientRole");

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            7001,
            eve_db::InventoryItemRecord {
                item_id: 7001,
                type_id: 587,
                owner_id: P1,
                location_id: 60003760,
                flag: eve_db::InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });

    let res_setattr = ctx
        .dispatch_rpc(&mut s1, "slash", None, "SlashCmd", &[PyRep::String("/setattr 7001 9 175.0".into())], &HashMap::new(), None)
        .await
        .expect("/setattr");
    assert!(matches!(res_setattr, PyRep::KeyVal(_)));

    let now_ft = ctx.now_blueos_filetime_ticks();
    let ev_mod = events(&ctx, P1, "OnModuleAttributeChange");
    assert_eq!(ev_mod.len(), 1);
    assert_eq!(ev_mod[0].len(), 8, "OnModuleAttributeChange expects 8 args");
    assert_eq!(ev_mod[0][0], PyRep::Integer(P1));
    assert_eq!(ev_mod[0][1], PyRep::Integer(7001));
    assert_eq!(ev_mod[0][2], PyRep::Integer(9));
    assert_eq!(ev_mod[0][3], PyRep::Integer(now_ft));
    assert_eq!(ev_mod[0][4], PyRep::Real(175.0));
    assert_eq!(ev_mod[0][5], PyRep::Real(350.0)); // Rifter base hp is 350.0
    assert_eq!(ev_mod[0][6], PyRep::Integer(now_ft));
    assert_eq!(ev_mod[0][7], PyRep::Bool(true));

    let ev_loc = events(&ctx, P1, "OnLocationAttributeChange");
    assert_eq!(ev_loc.len(), 1);
    assert_eq!(ev_loc[0].len(), 7, "OnLocationAttributeChange expects 7 args");
    assert_eq!(ev_loc[0][0], PyRep::Integer(P1));
    assert_eq!(ev_loc[0][1], PyRep::Integer(60003760));
    assert_eq!(ev_loc[0][2], PyRep::Integer(7001));
    assert_eq!(ev_loc[0][3], PyRep::Integer(9));
    assert_eq!(ev_loc[0][4], PyRep::Integer(now_ft));
    assert_eq!(ev_loc[0][5], PyRep::Real(175.0));
    assert_eq!(ev_loc[0][6], PyRep::Real(350.0));
}

