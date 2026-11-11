//! Unit tests for `notify_emitters_skills_trade_godma`.

use super::*;
use crate::router::ServerContext;
use crate::services::agents_missions::RndResearchEntry;
use eve_db::clock::VirtualServerClock;
use eve_db::{InventoryFlag, InventoryItemRecord, IskCents, OwnerNoteRecord, WorldRepository};
use eve_machonet::{service_roles, SessionState};
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn make_online_ctx() -> (ServerContext, i64) {
    let db = Arc::new(WorldRepository::with_demo_seed());
    let char_id = 90_000_001;
    db.acquire_online_lock(1, 501, Some(char_id), true, 1_775_400_000)
        .expect("online lock");
    let ctx = ServerContext::with_sde_and_repository(shared_sde(), db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_775_400_000)));
    (ctx, char_id)
}

#[test]
fn skill_alpha_injector_subscription_and_trade_repackage_emitters() {
    let (ctx, char_id) = make_online_ctx();

    // Non-positive added_qty is a no-op
    emit_non_diminishing_injections_added(&ctx, char_id, 0);
    emit_non_diminishing_injections_added(&ctx, char_id, -1);
    emit_non_diminishing_injections_added(&ctx, char_id, 50_000);
    emit_non_diminishing_injections_removed(&ctx, char_id);
    emit_skill_bundle_injected(&ctx, char_id, 5, 250_000);
    emit_skill_forced_refresh(&ctx, char_id);
    emit_multiple_characters_training_updated(&ctx, 1);
    emit_login_milestone_timer_started(&ctx, char_id, 600);
    emit_redeeming_queue_updated_for_user(&ctx, 1, Some(char_id));
    emit_redeeming_queue_updated_for_user(&ctx, 1, None);
    emit_pod_or_corvette_assembly(&ctx, char_id, 900_001, 670);
    emit_player_state_updated(&ctx, char_id, PyRep::Integer(1));
    emit_subscription_changed_server(&ctx, char_id, 2);
    emit_subscription_time_update_server(&ctx, 1, 0);
    emit_subscription_time_update_server(&ctx, 1, 1_800_000_000);
    emit_allow_war_updated(&ctx, 1_000_170, true);
    emit_nes_intro_state_changed(&ctx, char_id, 2);

    // Priming needed for trade items & sell repackage errors
    emit_priming_needed_for_trade_items(&ctx, char_id, &[]);
    emit_priming_needed_for_trade_items(&ctx, 0, &[100]);
    emit_priming_needed_for_trade_items(&ctx, char_id, &[100, 101]);

    emit_sell_repackage_errors(&ctx, char_id, "CantRepackageDamagedItem", 0);
    emit_sell_repackage_errors(&ctx, char_id, "OtherError", 2);
    emit_sell_repackage_errors(&ctx, char_id, "CantRepackageDamagedItem", 3);

    emit_structure_cfg_data_changed(&ctx, 30_000_142, 1_030_000_001, "Jita Hub", [1.0, 2.0, 3.0]);
    emit_server_message_broadcast(&ctx, "   ");
    emit_server_message_broadcast(&ctx, "  Downtime in 15m  ");
    emit_effect_message(&ctx, char_id, "Jam", &[("k", PyRep::Integer(1))]);
    emit_plex_deposit_failed(&ctx, char_id, "ErrDep", PyRep::Integer(11));
    emit_plex_withdraw_failed(&ctx, char_id, "ErrWdr", PyRep::Integer(22));

    let drained = ctx.drain_notifications_for_char(char_id);
    let events: HashSet<&str> = drained.iter().map(|n| n.event.as_str()).collect();
    assert!(events.contains("OnNonDiminishingInjectionsAdded"));
    assert!(events.contains("OnNonDiminishingInjectionsRemoved"));
    assert!(events.contains("OnSkillBundleInjected"));
    assert!(events.contains("OnSkillForcedRefresh"));
    assert!(events.contains("OnMultipleCharactersTrainingUpdated"));
    assert!(events.contains("OnMilestoneTimerStarted"));
    assert!(events.contains("OnRedeemingQueueUpdated"));
    assert!(events.contains("OnPodOrCorvetteAssembly"));
    assert!(events.contains("OnPlayerStateUpdated"));
    assert!(events.contains("OnSubscriptionChangedServer"));
    assert!(events.contains("OnSubscriptionTimeUpdateServer"));
    assert!(events.contains("OnAllowWarUpdated"));
    assert!(events.contains("OnNesIntroStateChanged"));
    assert!(events.contains("OnPrimingNeededForTradeItems"));
    assert!(events.contains("OnSellRepackageErrors"));
    assert!(events.contains("OnCfgDataChanged"));
    assert!(events.contains("OnServerMessage"));
    assert!(events.contains("OnEffectMessage"));
    assert!(events.contains("OnPlexDepositFailed"));
    assert!(events.contains("OnPlexWithdrawFailed"));

    // Alpha injector cooldown sweep
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.clone_grade = 1;
        }
        let mut cd = HashMap::new();
        cd.insert(char_id, 100_i64);
        snap.item_custom_names.insert(
            crate::services::implant_plugging::ALPHA_INJECTOR_COOLDOWNS_KEY,
            serde_json::to_string(&cd).unwrap(),
        );
    });
    sweep_due_login_milestones_and_alpha_injectors(&ctx, 1_775_400_000);
    let alpha_notifs = ctx.drain_notifications_for_char(char_id);
    assert_eq!(alpha_notifs.len(), 1);
    assert_eq!(alpha_notifs[0].event, "OnAlphaInjectionAvailable");
    // Second sweep without clearing latch does not re-emit
    sweep_due_login_milestones_and_alpha_injectors(&ctx, 1_775_400_000);
    assert!(ctx.drain_notifications_for_char(char_id).is_empty());
    ctx.db.with_mut_snapshot(|snap| clear_alpha_avail_notified(snap, char_id));
}

#[test]
fn damaged_sell_items_rnd_datacores_browser_lockdown_and_localization_qa() {
    let (ctx, char_id) = make_online_ctx();
    let station_id = 60_003_760_i64;
    let dmg_item_id = 910_001_i64;
    let ship_with_mod_dmg = 910_002_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut attrs = HashMap::new();
        attrs.insert(eve_sde::attr_ids::DAMAGE, 12.5);
        snap.items.insert(
            dmg_item_id,
            InventoryItemRecord {
                item_id: dmg_item_id,
                type_id: 587,
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: attrs,
                ..Default::default()
            },
        );
        snap.items.insert(
            ship_with_mod_dmg,
            InventoryItemRecord {
                item_id: ship_with_mod_dmg,
                type_id: 587,
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });

    let count = ctx.db.with_snapshot(|snap| {
        let mod_dmg = HashSet::from([ship_with_mod_dmg]);
        let parsed = vec![
            (station_id, 587, dmg_item_id, IskCents(100), 1, 0),
            (station_id, 587, ship_with_mod_dmg, IskCents(100), 1, 0),
        ];
        count_damaged_sell_items(snap, &mod_dmg, char_id, 1_000_009, false, &parsed)
    });
    assert_eq!(count, 2);

    // Browser lockdown & localization QA wrap
    let mut session = SessionState::new(501);
    session.user_id = Some(1);
    session.character_id = Some(char_id);
    session.station_id = Some(station_id);

    ctx.db.with_mut_snapshot(|snap| {
        set_browser_lockdown_policy(
            snap,
            true,
            "https://community.eveonline.com/",
            vec![("https://bad.example".into(), 1)],
        );
        let mut overrides = BTreeMap::new();
        overrides.insert(5001_i64, "Hello Capsuleer".to_string());
        snap.owner_notes.insert(
            LOCALIZATION_NOTE_ID,
            OwnerNoteRecord {
                note_id: LOCALIZATION_NOTE_ID,
                character_id: 0,
                item_id: 0,
                label: "loc".into(),
                note: serde_json::to_string(&overrides).unwrap(),
                timestamp_unix: 0,
            },
        );
    });

    assert_eq!(
        handle_misc_cluster_localization(&ctx, &session, "browserLockdownSvc", "IsBrowserInLockdown", &[])
            .unwrap()
            .unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(
        handle_misc_cluster_localization(&ctx, &session, "browserLockdownSvc", "GetDefaultHomePage", &[])
            .unwrap()
            .unwrap(),
        PyRep::String("https://community.eveonline.com/".into())
    );
    assert!(matches!(
        handle_misc_cluster_localization(&ctx, &session, "browserLockdownSvc", "GetFlaggedSitesList", &[])
            .unwrap()
            .unwrap(),
        PyRep::List(_)
    ));
    assert!(matches!(
        handle_misc_cluster_localization(&ctx, &session, "browserLockdownSvc", "GetFlaggedSitesHash", &[])
            .unwrap()
            .unwrap(),
        PyRep::String(_)
    ));

    // Localization QA wrap requires QA/GM role
    assert_eq!(
        handle_update_localization_qa_wrap(&ctx, &session, &[PyRep::Bool(true)])
            .expect_err("no qa role")
            .code,
        "RoleRequired"
    );
    session.role = service_roles::ROLE_QA;
    assert_eq!(
        handle_update_localization_qa_wrap(&ctx, &session, &[PyRep::Bool(true)]).unwrap(),
        PyRep::Bool(true)
    );
    let changes = handle_get_all_text_changes(&ctx, &session, &[]).unwrap();
    assert_eq!(
        changes,
        PyRep::Dict(vec![(
            PyRep::Integer(5001),
            PyRep::String("[QA:5001] Hello Capsuleer".into()),
        )])
    );

    // R&D datacore purchase validation & execution
    let (&agent_id, agent_info) = ctx
        .sde
        .agents
        .iter()
        .find(|(_, a)| a.station_id.is_some())
        .expect("seeded agent");
    let agent_station = agent_info.station_id.unwrap();
    let agent_sys = agent_info.solar_system_id.unwrap_or(30_000_142);
    session.station_id = Some(agent_station);

    assert_eq!(
        handle_rnd_buy_datacores(&ctx, &session, char_id, agent_id, agent_sys, &[PyRep::None, PyRep::None, PyRep::Integer(0)])
            .expect_err("qty=0")
            .code,
        "InvalidDatacoreQuantity"
    );
    ctx.db.with_mut_snapshot(|snap| {
        crate::services::agents_missions::save_rnd_research_entries(
            snap,
            char_id,
            &[RndResearchEntry {
                agent_id,
                research_type_id: 11442,
                points_per_day: 50.0,
                initial_points: 500.0,
                started_unix: 1_775_400_000,
            }],
        );
    });
    let bought = handle_rnd_buy_datacores(
        &ctx,
        &session,
        char_id,
        agent_id,
        agent_sys,
        &[PyRep::Integer(agent_id), PyRep::Integer(14), PyRep::Integer(2)],
    )
    .expect("buy 2 datacores");
    assert!(!matches!(bought, PyRep::None));
    assert_eq!(bought.dict_get("quantity"), Some(&PyRep::Integer(2)));
    assert_eq!(bought.dict_get("remainingResearchPoints"), Some(&PyRep::Real(300.0)));
}

#[test]
fn rnd_datacores_all_error_and_default_qty_and_damaged_sell_and_qa_eviction_branches() {
    let (ctx, char_id) = make_online_ctx();
    let (&agent_id, agent_info) = ctx
        .sde
        .agents
        .iter()
        .find(|(_, a)| a.station_id.is_some())
        .expect("seeded agent");
    let agent_station = agent_info.station_id.unwrap();
    let agent_sys = agent_info.solar_system_id.unwrap_or(30_000_142);

    let mut session = SessionState::default();
    session.user_id = Some(1);
    session.character_id = Some(char_id);
    session.solar_system_id = Some(agent_sys);

    // Qty > 100_000 -> InvalidDatacoreQuantity
    assert_eq!(
        handle_rnd_buy_datacores(
            &ctx,
            &session,
            char_id,
            agent_id,
            agent_sys,
            &[PyRep::None, PyRep::None, PyRep::Integer(100_001)],
        )
        .unwrap_err()
        .code,
        "InvalidDatacoreQuantity"
    );

    // UnknownAgent
    assert_eq!(
        handle_rnd_buy_datacores(&ctx, &session, char_id, 9_999_999, agent_sys, &[])
            .unwrap_err()
            .code,
        "UnknownAgent"
    );

    // AgentNotCoLocated when undocked or wrong station or wrong system
    assert_eq!(
        handle_rnd_buy_datacores(&ctx, &session, char_id, agent_id, agent_sys, &[])
            .unwrap_err()
            .code,
        "AgentNotCoLocated"
    );
    session.station_id = Some(agent_station + 1);
    assert_eq!(
        handle_rnd_buy_datacores(&ctx, &session, char_id, agent_id, agent_sys, &[])
            .unwrap_err()
            .code,
        "AgentNotCoLocated"
    );
    session.station_id = Some(agent_station);
    assert_eq!(
        handle_rnd_buy_datacores(&ctx, &session, char_id, agent_id, agent_sys + 1, &[])
            .unwrap_err()
            .code,
        "AgentNotCoLocated"
    );

    // ResearchNotActive when no entry exists
    assert_eq!(
        handle_rnd_buy_datacores(&ctx, &session, char_id, agent_id, agent_sys, &[])
            .unwrap_err()
            .code,
        "ResearchNotActive"
    );

    // InvalidResearchSkill
    ctx.db.with_mut_snapshot(|snap| {
        crate::services::agents_missions::save_rnd_research_entries(
            snap,
            char_id,
            &[RndResearchEntry {
                agent_id,
                research_type_id: 999_999,
                points_per_day: 0.0,
                initial_points: 500.0,
                started_unix: 1_775_400_000,
            }],
        );
    });
    assert_eq!(
        handle_rnd_buy_datacores(&ctx, &session, char_id, agent_id, agent_sys, &[])
            .unwrap_err()
            .code,
        "InvalidResearchSkill"
    );

    // InsufficientResearchPoints
    ctx.db.with_mut_snapshot(|snap| {
        crate::services::agents_missions::save_rnd_research_entries(
            snap,
            char_id,
            &[RndResearchEntry {
                agent_id,
                research_type_id: 11442,
                points_per_day: 10.0,
                initial_points: 50.0,
                started_unix: 1_775_400_000,
            }],
        );
    });
    assert_eq!(
        handle_rnd_buy_datacores(&ctx, &session, char_id, agent_id, agent_sys, &[])
            .unwrap_err()
            .code,
        "InsufficientResearchPoints"
    );

    // InsufficientFunds & default qty=1 happy path checking exact journal/wallet/notif
    ctx.db.with_mut_snapshot(|snap| {
        crate::services::agents_missions::save_rnd_research_entries(
            snap,
            char_id,
            &[RndResearchEntry {
                agent_id,
                research_type_id: 11442,
                points_per_day: 100.0,
                initial_points: 150.0,
                started_unix: 1_775_400_000,
            }],
        );
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.balance = IskCents::from_isk(9_999);
        }
    });
    assert_eq!(
        handle_rnd_buy_datacores(&ctx, &session, char_id, agent_id, agent_sys, &[])
            .unwrap_err()
            .code,
        "InsufficientFunds"
    );

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.balance = IskCents::from_isk(25_000);
        }
    });
    let _ = ctx.drain_notifications_for_char(char_id);
    let bought1 = handle_rnd_buy_datacores(&ctx, &session, char_id, agent_id, agent_sys, &[])
        .expect("default qty=1");
    assert_eq!(bought1.dict_get("quantity"), Some(&PyRep::Integer(1)));
    assert_eq!(bought1.dict_get("remainingResearchPoints"), Some(&PyRep::Real(50.0)));
    let dc_id = bought1.dict_get("datacoreItemID").and_then(|v| v.as_i64()).unwrap();
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.characters[&char_id].balance, IskCents::from_isk(15_000));
        assert_eq!(snap.items[&dc_id].location_id, agent_station);
        assert_eq!(snap.items[&dc_id].flag, InventoryFlag::Hangar);
        assert_eq!(snap.items[&dc_id].quantity, 1);
    });
    let notifs = ctx.drain_notifications_for_char(char_id);
    assert!(notifs.iter().any(|n| n.event == "OnDatacoreBought"));
    assert!(notifs.iter().any(|n| n.event == "OnAccountChange"));

    // Exact payload assertions for emitters
    emit_structure_cfg_data_changed(&ctx, 30_000_142, 1_030_000_009, "Fortizar Alpha", [10.0, 20.0, 30.0]);
    emit_godma_effect_info_for_module(&ctx, char_id, 700_001, 700_002, 5973, "moduleBonusAfterburner", 10.5, Some(700_099));
    emit_godma_effect_info_for_module(&ctx, char_id, 700_001, 700_003, 5973, "online", 0.0, None);
    emit_subscription_time_update_server(&ctx, 1, 0);
    emit_subscription_time_update_server(&ctx, 1, 2_000_000_000);
    let n2 = ctx.drain_notifications_for_char(char_id);
    let cfg = n2.iter().find(|n| n.event == "OnCfgDataChanged").expect("OnCfgDataChanged");
    assert_eq!(cfg.id_type, "solarsystemid");
    assert_eq!(
        cfg.args,
        vec![
            PyRep::String("evelocations".into()),
            PyRep::List(vec![
                PyRep::Integer(1_030_000_009),
                PyRep::String("Fortizar Alpha".into()),
                PyRep::Integer(30_000_142),
                PyRep::Real(10.0),
                PyRep::Real(20.0),
                PyRep::Real(30.0),
                PyRep::None,
            ]),
        ]
    );
    let effect_infos: Vec<_> = n2.iter().filter(|n| n.event == "OnGodmaEffectInfo").collect();
    assert_eq!(effect_infos.len(), 2);
    assert_eq!(effect_infos[0].args.len(), 4);
    assert_eq!(effect_infos[1].args.len(), 4);
    assert_ne!(effect_infos[0].args, effect_infos[1].args);

    // count_damaged_sell_items matrix (ARMOR_DAMAGE, raw_item_id==0 char vs corp vs Locked vs non-singleton)
    ctx.db.with_mut_snapshot(|snap| {
        let mut arm_attrs = HashMap::new();
        arm_attrs.insert(eve_sde::attr_ids::ARMOR_DAMAGE, 5.0);
        snap.items.insert(880_010, InventoryItemRecord {
            item_id: 880_010,
            type_id: 587,
            owner_id: char_id,
            location_id: agent_station,
            flag: InventoryFlag::Hangar,
            quantity: 1,
            singleton: true,
            dynamic_attributes: arm_attrs.clone(),
            ..Default::default()
        });
        snap.items.insert(880_011, InventoryItemRecord {
            item_id: 880_011,
            type_id: 588,
            owner_id: 1_000_170,
            location_id: agent_station,
            flag: InventoryFlag::Hangar,
            quantity: 1,
            singleton: true,
            dynamic_attributes: arm_attrs.clone(),
            ..Default::default()
        });
        snap.items.insert(880_012, InventoryItemRecord {
            item_id: 880_012,
            type_id: 589,
            owner_id: 1_000_170,
            location_id: agent_station,
            flag: InventoryFlag::Locked,
            quantity: 1,
            singleton: true,
            dynamic_attributes: arm_attrs,
            ..Default::default()
        });
        snap.items.insert(880_013, InventoryItemRecord {
            item_id: 880_013,
            type_id: 590,
            owner_id: char_id,
            location_id: agent_station,
            flag: InventoryFlag::Hangar,
            quantity: 2,
            singleton: false,
            ..Default::default()
        });
    });
    let damaged_fit = HashSet::new();
    let cnt_char = ctx.db.with_snapshot(|snap| {
        count_damaged_sell_items(
            snap,
            &damaged_fit,
            char_id,
            1_000_170,
            false,
            &[
                (agent_station, 587, 0, IskCents(1), 1, 0),
                (agent_station, 588, 0, IskCents(1), 1, 0),
                (agent_station, 589, 0, IskCents(1), 1, 0),
                (agent_station, 590, 880_013, IskCents(1), 1, 0),
            ],
        )
    });
    assert_eq!(cnt_char, 1);
    let cnt_corp = ctx.db.with_snapshot(|snap| {
        count_damaged_sell_items(
            snap,
            &damaged_fit,
            char_id,
            1_000_170,
            true,
            &[
                (agent_station, 587, 0, IskCents(1), 1, 0),
                (agent_station, 588, 0, IskCents(1), 1, 0),
                (agent_station, 589, 0, IskCents(1), 1, 0),
            ],
        )
    });
    assert_eq!(cnt_corp, 2);

    // sweep_due_login_milestones_and_alpha_injectors omega / unelapsed / already-notified / re-arm
    ctx.db.with_mut_snapshot(|snap| {
        let mut cd = HashMap::new();
        cd.insert(char_id, 100_i64);
        snap.item_custom_names.insert(
            crate::services::implant_plugging::ALPHA_INJECTOR_COOLDOWNS_KEY,
            serde_json::to_string(&cd).unwrap(),
        );
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.clone_grade = 1;
        }
        clear_alpha_avail_notified(snap, char_id);
    });
    let _ = ctx.drain_notifications_for_char(char_id);
    sweep_due_login_milestones_and_alpha_injectors(&ctx, 1_775_400_000);
    assert_eq!(
        ctx.drain_notifications_for_char(char_id)
            .iter()
            .filter(|n| n.event == "OnAlphaInjectionAvailable")
            .count(),
        1
    );
    // Second sweep does not duplicate while latched
    sweep_due_login_milestones_and_alpha_injectors(&ctx, 1_775_400_005);
    assert!(ctx
        .drain_notifications_for_char(char_id)
        .iter()
        .all(|n| n.event != "OnAlphaInjectionAvailable"));
    // Omega char skipped even after clearing latch
    ctx.db.with_mut_snapshot(|snap| {
        clear_alpha_avail_notified(snap, char_id);
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.clone_grade = 2;
        }
    });
    sweep_due_login_milestones_and_alpha_injectors(&ctx, 1_775_400_010);
    assert!(ctx
        .drain_notifications_for_char(char_id)
        .iter()
        .all(|n| n.event != "OnAlphaInjectionAvailable"));

    // browserLockdownSvc unauthenticated errors + default policy + unknown method
    let unauth_sess = SessionState::default();
    for m in [
        "IsBrowserInLockdown",
        "GetDefaultHomePage",
        "GetFlaggedSitesList",
        "GetFlaggedSitesHash",
    ] {
        assert!(handle_misc_cluster_localization(&ctx, &unauth_sess, "browserLockdownSvc", m, &[])
            .unwrap()
            .is_err());
    }
    assert!(handle_misc_batch9(&ctx, &session, "browserLockdownSvc", "Unknown", &[]).is_none());

    let (fresh_ctx, _) = make_online_ctx();
    let mut auth_sess = SessionState::default();
    auth_sess.user_id = Some(1);
    auth_sess.character_id = Some(char_id);
    assert_eq!(
        handle_misc_cluster_localization(&fresh_ctx, &auth_sess, "browserLockdownSvc", "IsBrowserInLockdown", &[])
            .unwrap()
            .unwrap(),
        PyRep::Bool(false)
    );
    assert_eq!(
        handle_misc_cluster_localization(&fresh_ctx, &auth_sess, "browserLockdownSvc", "GetDefaultHomePage", &[])
            .unwrap()
            .unwrap(),
        PyRep::String("https://www.eveonline.com/".into())
    );
    assert_eq!(
        handle_misc_cluster_localization(&fresh_ctx, &auth_sess, "browserLockdownSvc", "GetFlaggedSitesList", &[])
            .unwrap()
            .unwrap(),
        PyRep::List(vec![])
    );
    assert_eq!(
        handle_misc_cluster_localization(&fresh_ctx, &auth_sess, "browserLockdownSvc", "GetFlaggedSitesHash", &[])
            .unwrap()
            .unwrap(),
        PyRep::String("cbf29ce484222325".into())
    );

    // localizationServer: unauth, empty overrides -> None, RoleRequired, UnimplementedRPC, 256-entry eviction & disable
    assert!(handle_get_all_text_changes(&fresh_ctx, &unauth_sess, &[]).is_err());
    assert_eq!(handle_get_all_text_changes(&fresh_ctx, &auth_sess, &[]).unwrap(), PyRep::None);

    let err_role = handle_update_localization_qa_wrap(&fresh_ctx, &auth_sess, &[PyRep::Bool(true)]).unwrap_err();
    assert_eq!(err_role.code, "RoleRequired");
    auth_sess.role = service_roles::ROLE_QA;
    let err_arg = handle_update_localization_qa_wrap(&fresh_ctx, &auth_sess, &[PyRep::String("yes".into())]).unwrap_err();
    assert_eq!(err_arg.code, "UnimplementedRPC");

    assert_eq!(
        handle_update_localization_qa_wrap(&fresh_ctx, &auth_sess, &[PyRep::Integer(1)]).unwrap(),
        PyRep::Bool(true)
    );
    for uid in 10_u64..=266 {
        let mut s = SessionState::default();
        s.user_id = Some(uid);
        s.character_id = Some(char_id);
        s.role = service_roles::ROLE_QA;
        assert_eq!(
            handle_update_localization_qa_wrap(&fresh_ctx, &s, &[PyRep::Bool(true)]).unwrap(),
            PyRep::Bool(true)
        );
    }
    fresh_ctx.db.with_snapshot(|snap| {
        let map = &snap.cluster_and_runtime.localization_qa_wrap_users;
        assert_eq!(map.len(), 256);
        assert!(!map.contains_key(&1));
        assert!(map.contains_key(&266));
    });
    assert_eq!(
        handle_update_localization_qa_wrap(&fresh_ctx, &auth_sess, &[PyRep::Integer(0)]).unwrap(),
        PyRep::Bool(false)
    );
    fresh_ctx.db.with_mut_snapshot(|snap| {
        snap.owner_notes.insert(-8_960_000_000_000, OwnerNoteRecord {
            note_id: -8_960_000_000_000,
            character_id: -8_960_000_000_000,
            item_id: 0,
            label: "localization".into(),
            note: r#"{"77":"Plain Text"}"#.into(),
            timestamp_unix: 1_700_000_000,
        });
    });
    assert_eq!(
        handle_get_all_text_changes(&fresh_ctx, &auth_sess, &[]).unwrap(),
        PyRep::Dict(vec![(PyRep::Integer(77), PyRep::String("Plain Text".into()))])
    );
}

#[tokio::test]
async fn dogma_set_and_take_module_offline_cover_all_guards_structure_and_notifications() {
    let (ctx, char_id) = make_online_ctx();
    let sys_id = 30_000_142_u32;
    let station_id = 60_003_760_i64;
    let ship_id = 910_100_i64;
    let other_ship_id = 910_101_i64;
    let structure_id = 1_030_100_001_i64;

    // Locate small onlineable low/med module + oversized fitting-failing module from seeded SDE
    let no_ship_restriction = |t: &eve_sde::InvType| {
        !t.effect_ids.contains(&2663)
            && !t.default_attributes.keys().any(|aid| {
                matches!(
                    *aid,
                    1298..=1305
                        | 1380
                        | 1544
                        | 978
                        | 1872
                        | 1879..=1881
                        | 1944
                        | 2065
                        | 2103
                        | 2396
                        | 2463
                        | 2476..=2488
                        | 2758
                        | 5948
                )
            })
    };
    let low_mod_type = ctx
        .sde
        .types
        .values()
        .filter(|t| {
            t.published
                && t.category_id == 7
                && t.effect_ids.contains(&11)
                && t.effect_ids.contains(&16)
                && t.default_attributes.get(&eve_sde::attr_ids::CPU).copied().unwrap_or(0.0) <= 10.0
                && t.default_attributes.get(&eve_sde::attr_ids::POWER).copied().unwrap_or(0.0) <= 10.0
                && no_ship_restriction(t)
        })
        .min_by_key(|t| t.type_id)
        .expect("small low module")
        .type_id;
    let huge_mod_type = ctx
        .sde
        .types
        .values()
        .filter(|t| {
            t.published
                && t.category_id == 7
                && t.effect_ids.contains(&11)
                && t.effect_ids.contains(&16)
                && t.type_id != low_mod_type
                && (t.default_attributes.get(&eve_sde::attr_ids::CPU).copied().unwrap_or(0.0) > 500.0
                    || t.default_attributes.get(&eve_sde::attr_ids::POWER).copied().unwrap_or(0.0) > 500.0)
                && no_ship_restriction(t)
        })
        .min_by_key(|t| t.type_id)
        .map(|t| t.type_id);

    let mod_id = 910_200_i64;
    let hangared_mod_id = 910_201_i64;
    let huge_mod_id = 910_202_i64;
    let st_mod_id = 910_203_i64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(mod_t) = ctx.sde.types.get(&low_mod_type) {
            let entry = snap.skills.entry(char_id).or_default();
            for req_attr in [182u16, 183, 184, 1285, 1289, 1290] {
                if let Some(&req_sk) = mod_t.default_attributes.get(&req_attr) {
                    let sk_id = req_sk as u32;
                    if sk_id > 0 {
                        entry.insert(sk_id, eve_db::TrainedSkillRecord { type_id: sk_id, skill_points: 256_000.0, level: 5 });
                    }
                }
            }
        }
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = ship_id;
        }
        snap.items.insert(ship_id, InventoryItemRecord {
            item_id: ship_id,
            type_id: 587,
            owner_id: char_id,
            location_id: i64::from(sys_id),
            flag: InventoryFlag::None,
            quantity: 1,
            singleton: true,
            ..Default::default()
        });
        snap.items.insert(other_ship_id, InventoryItemRecord {
            item_id: other_ship_id,
            type_id: 587,
            owner_id: char_id + 99,
            location_id: i64::from(sys_id),
            flag: InventoryFlag::None,
            quantity: 1,
            singleton: true,
            ..Default::default()
        });
        let mut burned_attrs = HashMap::new();
        burned_attrs.insert(eve_sde::attr_ids::DAMAGE, 40.0);
        snap.items.insert(mod_id, InventoryItemRecord {
            item_id: mod_id,
            type_id: low_mod_type,
            owner_id: char_id,
            location_id: ship_id,
            flag: InventoryFlag::LoSlot0,
            quantity: 1,
            singleton: true,
            dynamic_attributes: burned_attrs,
            ..Default::default()
        });
        snap.items.insert(hangared_mod_id, InventoryItemRecord {
            item_id: hangared_mod_id,
            type_id: low_mod_type,
            owner_id: char_id,
            location_id: ship_id,
            flag: InventoryFlag::Cargo,
            quantity: 1,
            singleton: true,
            ..Default::default()
        });
        if let Some(htid) = huge_mod_type {
            snap.items.insert(huge_mod_id, InventoryItemRecord {
                item_id: huge_mod_id,
                type_id: htid,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::LoSlot1,
                quantity: 1,
                singleton: true,
                ..Default::default()
            });
        }
        snap.module_online_states.insert(mod_id, false);
        snap.structures.insert(structure_id, eve_db::UpwellStructureRecord {
            structure_id,
            type_id: 35832,
            owner_corp_id: 1_000_170,
            solar_system_id: sys_id,
            name: "Dogma Struct".into(),
            state: "ShieldVulnerable".into(),
            ..Default::default()
        });
        snap.items.insert(st_mod_id, InventoryItemRecord {
            item_id: st_mod_id,
            type_id: low_mod_type,
            owner_id: 1_000_170,
            location_id: structure_id,
            flag: InventoryFlag::HiSlot0,
            quantity: 1,
            singleton: true,
            ..Default::default()
        });
    });

    let mut sess = SessionState::default();
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(ship_id);

    // ShipOwnershipViolation on both online and offline
    assert_eq!(
        handle_dogma_set_module_online(&ctx, &mut sess, &[PyRep::Integer(other_ship_id), PyRep::Integer(mod_id)])
            .await
            .unwrap_err()
            .code,
        "ShipOwnershipViolation"
    );
    assert_eq!(
        handle_dogma_take_module_offline(&ctx, &mut sess, &[PyRep::Integer(other_ship_id), PyRep::Integer(mod_id)])
            .await
            .unwrap_err()
            .code,
        "ShipOwnershipViolation"
    );

    // ModuleBurnedOut in space on both online and offline
    assert_eq!(
        handle_dogma_set_module_online(&ctx, &mut sess, &[PyRep::Integer(ship_id), PyRep::Integer(mod_id)])
            .await
            .unwrap_err()
            .code,
        "ModuleBurnedOut"
    );
    assert_eq!(
        handle_dogma_take_module_offline(&ctx, &mut sess, &[PyRep::Integer(ship_id), PyRep::Integer(mod_id)])
            .await
            .unwrap_err()
            .code,
        "ModuleBurnedOut"
    );

    // While docked, burned module passes space-only burn/cap checks
    sess.station_id = Some(station_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = Some(station_id);
        }
        if let Some(m) = snap.items.get_mut(&mod_id) {
            m.dynamic_attributes.clear();
        }
    });

    // Clear burn & test NotEnoughCapacitorToOnline in space when < 75% cap
    sess.station_id = None;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
        }
    });
    let sys = ctx.systems.get_or_spawn(sys_id);
    let mut cs = eve_destiny::CombatState::default();
    cs.max_capacitor = 100.0;
    cs.capacitor = 74.0;
    sys.set_combat_state(ship_id, cs.clone()).await.unwrap();
    assert_eq!(
        handle_dogma_set_module_online(&ctx, &mut sess, &[PyRep::Integer(ship_id), PyRep::Integer(mod_id)])
            .await
            .unwrap_err()
            .code,
        "NotEnoughCapacitorToOnline"
    );

    // Non-fitted cargo item -> FittingValidationFailed (not in fitted slot)
    cs.capacitor = 80.0;
    sys.set_combat_state(ship_id, cs).await.unwrap();
    assert_eq!(
        handle_dogma_set_module_online(&ctx, &mut sess, &[PyRep::Integer(ship_id), PyRep::Integer(hangared_mod_id)])
            .await
            .unwrap_err()
            .code,
        "FittingValidationFailed"
    );
    if huge_mod_type.is_some() {
        assert_eq!(
            handle_dogma_set_module_online(&ctx, &mut sess, &[PyRep::Integer(ship_id), PyRep::Integer(huge_mod_id)])
                .await
                .unwrap_err()
                .code,
            "FittingValidationFailed"
        );
    }

    // Happy path online -> sets module_online_states=true, emits notifications
    let _ = ctx.drain_notifications_for_char(char_id);
    let on_res = handle_dogma_set_module_online(&ctx, &mut sess, &[PyRep::Integer(ship_id), PyRep::Integer(mod_id)])
        .await
        .expect("online in space at 80% cap");
    assert_eq!(on_res.dict_get("online"), Some(&PyRep::Bool(true)));
    ctx.db.with_snapshot(|snap| assert_eq!(snap.module_online_states.get(&mod_id), Some(&true)));
    let on_notifs = ctx.drain_notifications_for_char(char_id);
    assert!(on_notifs.iter().any(|n| n.event == "OnModuleAttributeChanges"));
    assert!(on_notifs.iter().any(|n| n.event == "OnGodmaShipEffect"));

    // Mark active with reactivation delay attr 669 & offline -> records delay, removes cycle, emits offline notifs
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_active_states.insert(mod_id);
        if let Some(m) = snap.items.get_mut(&mod_id) {
            m.dynamic_attributes.insert(669, 5_000.0);
        }
    });
    if let Ok(mut fit) = ctx.get_or_rebuild_ship_fitting(char_id, ship_id) {
        if let Some(m) = fit.modules.get_mut(&mod_id) {
            m.state = eve_dogma::ModuleState::Active;
        }
        ctx.store_ship_fitting(ship_id, fit);
    }
    let off_res = handle_dogma_take_module_offline(&ctx, &mut sess, &[PyRep::Integer(ship_id), PyRep::Integer(mod_id)])
        .await
        .expect("take offline");
    assert_eq!(off_res.dict_get("online"), Some(&PyRep::Bool(false)));
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.module_online_states.get(&mod_id), Some(&false));
        assert!(!snap.module_active_states.contains(&mod_id));
    });
    let off_notifs = ctx.drain_notifications_for_char(char_id);
    assert!(off_notifs.iter().any(|n| n.event == "OnGodmaShipEffect"));

    // Structure delegation branch exercised on both handlers
    let _ = handle_dogma_set_module_online(&ctx, &mut sess, &[PyRep::Integer(structure_id), PyRep::Integer(st_mod_id)]).await;
    let _ = handle_dogma_take_module_offline(&ctx, &mut sess, &[PyRep::Integer(structure_id), PyRep::Integer(st_mod_id)]).await;
}

