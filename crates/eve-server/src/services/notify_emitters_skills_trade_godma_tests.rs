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
        PyRep::String("d751713988987e9331980363e24189ce".into())
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
    assert!(
        !on_notifs
            .iter()
            .any(|n| n.event == "OnGodmaEffectInfo" || n.event == "OnGodmaPrimeItem"),
        "SetModuleOnline in space must not emit OnGodmaEffectInfo or OnGodmaPrimeItem (#1073): {on_notifs:?}"
    );

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

#[tokio::test]
async fn set_module_online_and_take_module_offline_enforce_active_space_cloak_zero_mutation() {
    use eve_destiny::{Ball, CombatState, MovementMode, SlimItemMeta};
    use glam::DVec3;

    let low_mod_tid = 996_011_u32;
    let cloak_tid = 996_330_u32;
    let mut sde = (*shared_sde()).clone();
    let mut low_mod = sde.types.get(&587).expect("seed type").clone();
    low_mod.type_id = low_mod_tid;
    low_mod.group_id = 60;
    low_mod.category_id = 7;
    low_mod.name = "Test Low Module I".into();
    low_mod.default_attributes.clear();
    low_mod.default_attributes.insert(eve_sde::attr_ids::CPU, 1.0);
    low_mod.default_attributes.insert(eve_sde::attr_ids::POWER, 1.0);
    low_mod.default_attributes.insert(669, 5_000.0);
    low_mod.effect_ids = vec![11, 16];
    sde.types.insert(low_mod_tid, low_mod);

    let mut cloak_mod = sde.types.get(&587).expect("seed type").clone();
    cloak_mod.type_id = cloak_tid;
    cloak_mod.group_id = 330;
    cloak_mod.category_id = 7;
    cloak_mod.name = "Test Cloaking Device I".into();
    cloak_mod.default_attributes.clear();
    cloak_mod.default_attributes.insert(eve_sde::attr_ids::CPU, 1.0);
    cloak_mod.default_attributes.insert(eve_sde::attr_ids::POWER, 1.0);
    cloak_mod.effect_ids = vec![12, 16];
    sde.types.insert(cloak_tid, cloak_mod);

    let db = Arc::new(WorldRepository::with_demo_seed());
    let char_id = 90_990_001_i64;
    let ctx = ServerContext::with_sde_and_repository(Arc::new(sde), db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_775_400_000)));

    let sys_id = 30_002_505_u32;
    let ship_id = 910_990_001_i64;
    let offline_mod_id = 910_990_002_i64;
    let online_mod_id = 910_990_003_i64;
    let cloak_mod_id = 910_990_004_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().expect("seed char");
        ch.character_id = char_id;
        ch.user_id = char_id as u64;
        ch.solar_system_id = sys_id;
        ch.station_id = None;
        ch.structure_id = None;
        ch.active_ship_id = ship_id;
        snap.characters.insert(char_id, ch);
        snap.accounts.insert(
            format!("user_{char_id}"),
            eve_db::AccountRecord {
                user_id: char_id as u64,
                username: format!("user_{char_id}"),
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
                type_id: 587,
                owner_id: char_id,
                location_id: i64::from(sys_id),
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        for (iid, tid, flag) in [
            (offline_mod_id, low_mod_tid, InventoryFlag::LoSlot0),
            (online_mod_id, low_mod_tid, InventoryFlag::LoSlot1),
            (cloak_mod_id, cloak_tid, InventoryFlag::HiSlot0),
        ] {
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: tid,
                    owner_id: char_id,
                    location_id: ship_id,
                    flag,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
        }
        snap.module_online_states.insert(offline_mod_id, false);
        snap.module_online_states.insert(online_mod_id, true);
        snap.module_online_states.insert(cloak_mod_id, true);
        snap.module_active_states.insert(online_mod_id);
    });
    ctx.db
        .acquire_online_lock(char_id as u64, 501, Some(char_id), true, 1_775_400_000)
        .expect("online lock");
    ctx.invalidate_ship_fitting(ship_id);

    let sys = ctx.systems.get_or_spawn(sys_id);
    let mut cs = CombatState::default();
    cs.capacitor = 500.0;
    cs.max_capacitor = 500.0;
    cs.is_cloaked = true;
    cs.gate_cloak_remaining_ticks = 0;
    sys.spawn_ball_with_meta(
        Ball {
            id: ship_id,
            type_id: 587,
            position: DVec3::ZERO,
            velocity: DVec3::ZERO,
            max_velocity: 365.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
        cs,
        SlimItemMeta {
            group_id: 25,
            category_id: 6,
            owner_id: char_id,
            char_id: Some(char_id),
            corp_id: 1_000_009,
            is_cloaked: true,
            ..Default::default()
        },
    )
    .await
    .unwrap();

    let mut space_sess = SessionState {
        user_id: Some(char_id as u64),
        character_id: Some(char_id),
        corporation_id: Some(1_000_009),
        solar_system_id: Some(sys_id),
        ship_id: Some(ship_id),
        ..Default::default()
    };
    let _ = ctx.drain_notifications_for_char(char_id);

    // 1. Actively cloaked in space: SetModuleOnline and TakeModuleOffline reject with 0 mutations.
    let on_err = handle_dogma_set_module_online(
        &ctx,
        &mut space_sess,
        &[PyRep::Integer(ship_id), PyRep::Integer(offline_mod_id)],
    )
    .await
    .expect_err("actively cloaked SetModuleOnline must reject");
    assert_eq!(on_err.code, "CannotActivateCloaked");
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.module_online_states.get(&offline_mod_id), Some(&false));
    });
    assert!(ctx.drain_notifications_for_char(char_id).is_empty());

    for off_target in [online_mod_id, cloak_mod_id] {
        let off_err = handle_dogma_take_module_offline(
            &ctx,
            &mut space_sess,
            &[PyRep::Integer(ship_id), PyRep::Integer(off_target)],
        )
        .await
        .expect_err("actively cloaked TakeModuleOffline must reject");
        assert_eq!(off_err.code, "CannotActivateCloaked");
        assert!(!ctx.is_module_in_reactivation_delay(ship_id, off_target));
    }
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.module_online_states.get(&online_mod_id), Some(&true));
        assert_eq!(snap.module_online_states.get(&cloak_mod_id), Some(&true));
        assert!(snap.module_active_states.contains(&online_mod_id));
    });
    assert!(ctx.drain_notifications_for_char(char_id).is_empty());

    // 2. DB-active cloak fallback (solar_system_id = None, undocked) also blocks both calls.
    let mut no_sys_sess = space_sess.clone();
    no_sys_sess.solar_system_id = None;
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_active_states.insert(cloak_mod_id);
    });
    assert_eq!(
        handle_dogma_set_module_online(
            &ctx,
            &mut no_sys_sess,
            &[PyRep::Integer(ship_id), PyRep::Integer(offline_mod_id)],
        )
        .await
        .expect_err("DB-active cloak fallback must block SetModuleOnline")
        .code,
        "CannotActivateCloaked"
    );
    assert_eq!(
        handle_dogma_take_module_offline(
            &ctx,
            &mut no_sys_sess,
            &[PyRep::Integer(ship_id), PyRep::Integer(online_mod_id)],
        )
        .await
        .expect_err("DB-active cloak fallback must block TakeModuleOffline")
        .code,
        "CannotActivateCloaked"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_active_states.remove(&cloak_mod_id);
    });

    // 3. Docked session allows SetModuleOnline / TakeModuleOffline cleanly even if hull had cloaked state.
    let mut docked_sess = space_sess.clone();
    docked_sess.station_id = Some(60_003_760);
    let docked_on = handle_dogma_set_module_online(
        &ctx,
        &mut docked_sess,
        &[PyRep::Integer(ship_id), PyRep::Integer(offline_mod_id)],
    )
    .await
    .expect("docked SetModuleOnline must succeed");
    assert_eq!(docked_on.dict_get("online"), Some(&PyRep::Bool(true)));
    let docked_off = handle_dogma_take_module_offline(
        &ctx,
        &mut docked_sess,
        &[PyRep::Integer(ship_id), PyRep::Integer(offline_mod_id)],
    )
    .await
    .expect("docked TakeModuleOffline must succeed");
    assert_eq!(docked_off.dict_get("online"), Some(&PyRep::Bool(false)));

    // 4. Gate-cloaked in space (gate_cloak_remaining_ticks > 0) allows SetModuleOnline / TakeModuleOffline.
    let mut gate_cs = sys.get_combat_state(ship_id).await.unwrap().unwrap();
    gate_cs.is_cloaked = true;
    gate_cs.gate_cloak_remaining_ticks = 20;
    sys.set_combat_state(ship_id, gate_cs).await.unwrap();

    let gate_on = handle_dogma_set_module_online(
        &ctx,
        &mut space_sess,
        &[PyRep::Integer(ship_id), PyRep::Integer(offline_mod_id)],
    )
    .await
    .expect("gate-cloaked SetModuleOnline must succeed");
    assert_eq!(gate_on.dict_get("online"), Some(&PyRep::Bool(true)));

    let gate_off = handle_dogma_take_module_offline(
        &ctx,
        &mut space_sess,
        &[PyRep::Integer(ship_id), PyRep::Integer(online_mod_id)],
    )
    .await
    .expect("gate-cloaked TakeModuleOffline must succeed");
    assert_eq!(gate_off.dict_get("online"), Some(&PyRep::Bool(false)));
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.module_online_states.get(&online_mod_id), Some(&false));
        assert!(!snap.module_active_states.contains(&online_mod_id));
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn take_module_offline_and_deactivate_online_stop_mining_and_entosis_external_active_modules() {
    use crate::services::inventory_fitting::fitting_mining_lasers::handle_mining_and_deactivate;
    use crate::services::tactical_modules::{db_ptr_key, handle_entosis_link_activation};
    use crate::system_actor::AsteroidState;
    use eve_db::{AsteroidRecord, TrainedSkillRecord};
    use eve_destiny::{Ball, MovementMode, SlimItemMeta, TargetLock};
    use eve_sde::{DogmaEffectType, InvType};
    use glam::DVec3;

    let mut sde = (*shared_sde()).clone();
    let mk_custom = |base: &InvType, type_id: u32, group_id: u32, category_id: u32, name: &str, volume: f64, default_effect_id: Option<u16>, attrs: &[(u16, f64)]| {
        let mut t = base.clone();
        t.type_id = type_id;
        t.group_id = group_id;
        t.category_id = category_id;
        t.name = name.into();
        t.volume = volume;
        t.default_effect_id = default_effect_id;
        t.default_attributes.clear();
        for &(k, v) in attrs {
            t.default_attributes.insert(k, v);
        }
        t
    };
    let seed = sde.types.get(&587).expect("seed type").clone();
    sde.effects.insert(
        67,
        DogmaEffectType {
            effect_id: 67,
            name: "miningLaser".into(),
            effect_category: 1,
            is_offensive: true,
            is_assistance: false,
            is_warp_safe: false,
            disallow_auto_repeat: false,
            electronic_chance: false,
            propulsion_chance: false,
            range_chance: false,
            guid: None,
            published: true,
            modifiers: Vec::new(),
        },
    );
    sde.types.insert(3386, mk_custom(&seed, 3386, 255, 16, "Mining", 0.01, None, &[]));
    sde.types.insert(1230, mk_custom(&seed, 1230, 462, 25, "Veldspar", 0.1, None, &[(182, 3386.0)]));
    if let Some(ship_t) = sde.types.get_mut(&587) {
        ship_t.default_attributes.insert(eve_dogma::attr_ids::HI_SLOTS, 6.0);
        ship_t.default_attributes.insert(eve_dogma::attr_ids::CPU_OUTPUT, 1_000.0);
        ship_t.default_attributes.insert(eve_dogma::attr_ids::POWER_OUTPUT, 1_000.0);
        ship_t.default_attributes.insert(eve_dogma::attr_ids::CAPACITOR_CAPACITY, 1_000.0);
        ship_t.default_attributes.insert(eve_dogma::attr_ids::RECHARGE_RATE, 1_000_000.0);
        ship_t.default_attributes.insert(eve_dogma::attr_ids::MAX_VELOCITY, 365.0);
        ship_t.default_attributes.insert(eve_dogma::attr_ids::MAX_LOCKED_TARGETS, 6.0);
    }
    let mut ent_t = mk_custom(
        &seed,
        999_194,
        1313,
        7,
        "Entosis Link II",
        5.0,
        None,
        &[(50, 1.0), (30, 1.0), (73, 50.0), (713, 1.0), (2112, 120_000.0), (2113, 120_000.0)],
    );
    ent_t.effect_ids = vec![12, 16];
    sde.types.insert(999_194, ent_t);
    let mut miner_t = mk_custom(
        &seed,
        17482,
        54,
        7,
        "Strip Miner I",
        50.0,
        Some(67),
        &[(50, 30.0), (30, 1.0), (77, 100.0), (73, 180_000.0), (54, 15_000.0), (6, 40.0), (182, 3386.0)],
    );
    miner_t.effect_ids = vec![12, 16, 67];
    sde.types.insert(17482, miner_t);

    let db = Arc::new(WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(Arc::new(sde), db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_775_400_000)));

    let char_id = 90_990_201_i64;
    let sys_id = 30_004_759_u32;
    let ship_id = 910_990_201_i64;
    let ent_off_id = 910_990_202_i64;
    let ent_deact_id = 910_990_203_i64;
    let mine_off_id = 910_990_204_i64;
    let mine_deact_id = 910_990_205_i64;
    let ozone_id = 910_990_206_i64;
    let node_id = 910_990_207_i64;
    let ast_id = 910_990_208_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().expect("seed char");
        ch.character_id = char_id;
        ch.user_id = char_id as u64;
        ch.corporation_id = 98_000_001;
        ch.solar_system_id = sys_id;
        ch.station_id = None;
        ch.structure_id = None;
        ch.active_ship_id = ship_id;
        snap.characters.insert(char_id, ch);
        snap.accounts.insert(
            format!("user_{char_id}"),
            eve_db::AccountRecord {
                user_id: char_id as u64,
                username: format!("user_{char_id}"),
                password_token: String::new(),
                role: 0,
                banned_until_unix: None,
                ban_reason: None,
            },
        );
        for sk in [3350, 3351, 3386] {
            snap.skills.entry(char_id).or_default().insert(
                sk,
                TrainedSkillRecord {
                    type_id: sk,
                    level: 5,
                    skill_points: 256_000.0,
                },
            );
        }
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: char_id,
                location_id: i64::from(sys_id),
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        for (iid, tid, flag) in [
            (ent_off_id, 999_194_u32, InventoryFlag::HiSlot0),
            (ent_deact_id, 999_194_u32, InventoryFlag::HiSlot1),
            (mine_off_id, 17482_u32, InventoryFlag::HiSlot2),
            (mine_deact_id, 17482_u32, InventoryFlag::HiSlot3),
        ] {
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: tid,
                    owner_id: char_id,
                    location_id: ship_id,
                    flag,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
            snap.module_online_states.insert(iid, true);
        }
        snap.items.insert(
            ozone_id,
            InventoryItemRecord {
                item_id: ozone_id,
                type_id: 16275,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 50,
                singleton: false,
                ..Default::default()
            },
        );
        snap.items.insert(
            node_id,
            InventoryItemRecord {
                item_id: node_id,
                type_id: 2977,
                owner_id: 98_000_009,
                location_id: i64::from(sys_id),
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.asteroids.insert(
            ast_id,
            AsteroidRecord {
                asteroid_id: ast_id,
                solar_system_id: sys_id,
                belt_id: 1,
                type_id: 1230,
                quantity: 10_000,
                initial_quantity: 10_000,
                position: (2_000.0, 0.0, 0.0),
                radius: 100.0,
            },
        );
    });
    ctx.db
        .acquire_online_lock(char_id as u64, 501, Some(char_id), true, 1_775_400_000)
        .expect("online lock");
    ctx.invalidate_ship_fitting(ship_id);

    let sys = ctx.systems.get_or_spawn(sys_id);
    let mut cs = ctx.build_ship_combat_state(char_id, ship_id, 587);
    cs.capacitor = 1_000.0;
    cs.max_capacitor = 1_000.0;
    cs.is_cloaked = false;
    cs.gate_cloak_remaining_ticks = 0;
    cs.undock_invulnerable_remaining_ticks = 0;
    sys.spawn_ball_with_meta(
        Ball {
            id: ship_id,
            type_id: 587,
            position: DVec3::ZERO,
            velocity: DVec3::ZERO,
            max_velocity: 365.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
        cs,
        SlimItemMeta {
            group_id: 25,
            category_id: 6,
            owner_id: char_id,
            char_id: Some(char_id),
            corp_id: 98_000_001,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    sys.spawn_asteroid(
        AsteroidState {
            asteroid_id: ast_id,
            belt_id: 1,
            type_id: 1230,
            initial_units: 10_000,
            remaining_units: 10_000,
            unit_volume: 0.1,
            depleted: false,
            respawn_at_tick: None,
        },
        Ball {
            id: ast_id,
            type_id: 1230,
            position: DVec3::new(2_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 50_000.0,
            radius: 100.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
    )
    .await
    .unwrap();
    sys.add_target_lock(
        ship_id,
        TargetLock {
            target_id: ast_id,
            lock_time_ms: 100.0,
            remaining_lock_ms: 0.0,
            locked: true,
            max_range_m: None,
        },
    )
    .await
    .unwrap();

    let mut sess = SessionState {
        user_id: Some(char_id as u64),
        character_id: Some(char_id),
        corporation_id: Some(98_000_001),
        solar_system_id: Some(sys_id),
        ship_id: Some(ship_id),
        ..Default::default()
    };
    let db_key = db_ptr_key(&ctx.db);
    let ent_type = ctx.sde.types.get(&999_194).unwrap().clone();
    let assert_ship_clean = |mod_id: i64| {
        assert!(!sys.is_module_active_sync(ship_id, mod_id));
        assert!(!sys.active_module_ships.read().unwrap().contains(&ship_id));
        assert!(!sys.view().has_active_module(ship_id));
        assert!(!ctx
            .tactical_state
            .read()
            .unwrap()
            .entosis_active_links
            .contains_key(&(db_key, ship_id)));
        assert!(!ctx.mining_cycles.read().unwrap().contains_key(&mod_id));
    };

    // A. Entosis Link -> TakeModuleOffline
    handle_entosis_link_activation(&ctx, &sys, char_id, ship_id, ent_off_id, &ent_type, Some(node_id))
        .await
        .expect("activate entosis 1");
    assert!(sys.is_module_active_sync(ship_id, ent_off_id));
    assert!(sys.active_module_ships.read().unwrap().contains(&ship_id));
    assert!(sys.view().has_active_module(ship_id));
    handle_dogma_take_module_offline(&ctx, &mut sess, &[PyRep::Integer(ship_id), PyRep::Integer(ent_off_id)])
        .await
        .expect("TakeModuleOffline entosis");
    assert_ship_clean(ent_off_id);

    // B. Entosis Link -> Deactivate(..., "online")
    handle_entosis_link_activation(&ctx, &sys, char_id, ship_id, ent_deact_id, &ent_type, Some(node_id))
        .await
        .expect("activate entosis 2");
    assert!(sys.is_module_active_sync(ship_id, ent_deact_id));
    handle_mining_and_deactivate(
        &ctx,
        &mut sess,
        None,
        "dogmaIM",
        "Deactivate",
        &[PyRep::Integer(ent_deact_id), PyRep::String("online".into())],
        &HashMap::new(),
    )
    .await
    .expect("handled Deactivate online")
    .expect("Deactivate online entosis succeeds");
    assert_ship_clean(ent_deact_id);

    // C. Mining module -> TakeModuleOffline
    crate::services::mining_cycle::activate(&ctx, char_id, ship_id, sys_id, mine_off_id, ast_id)
        .await
        .expect("activate miner 1");
    assert!(sys.is_module_active_sync(ship_id, mine_off_id));
    assert!(sys.active_module_ships.read().unwrap().contains(&ship_id));
    assert!(sys.view().has_active_module(ship_id));
    handle_dogma_take_module_offline(&ctx, &mut sess, &[PyRep::Integer(ship_id), PyRep::Integer(mine_off_id)])
        .await
        .expect("TakeModuleOffline miner");
    assert_ship_clean(mine_off_id);

    // D. Mining module -> Deactivate(..., "online")
    crate::services::mining_cycle::activate(&ctx, char_id, ship_id, sys_id, mine_deact_id, ast_id)
        .await
        .expect("activate miner 2");
    assert!(sys.is_module_active_sync(ship_id, mine_deact_id));
    handle_mining_and_deactivate(
        &ctx,
        &mut sess,
        None,
        "dogmaIM",
        "Deactivate",
        &[PyRep::Integer(mine_deact_id), PyRep::String("online".into())],
        &HashMap::new(),
    )
    .await
    .expect("handled Deactivate online")
    .expect("Deactivate online miner succeeds");
    assert_ship_clean(mine_deact_id);
}

