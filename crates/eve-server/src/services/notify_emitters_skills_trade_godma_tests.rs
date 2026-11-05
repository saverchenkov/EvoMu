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
}
