//! Deep branch-coverage and mutation-killing unit tests for `services::notify_emitters_skills_trade_godma`.

#![forbid(unsafe_code)]

use super::notify_emitters_skills_trade_godma::*;
use crate::router::ServerContext;
use crate::services::agents_missions::RndResearchEntry;
use eve_db::clock::VirtualServerClock;
use eve_db::{InventoryFlag, InventoryItemRecord, IskCents, WorldRepository};
use eve_destiny::{Ball, CombatState, MovementMode, SlimItemMeta};
use eve_machonet::{service_roles, SessionState};
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::{attr_ids, SdeRegistry};
use glam::DVec3;
use std::collections::{BTreeMap, HashSet};
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn make_ctx(base_unix: u64) -> (ServerContext, i64) {
    let db = Arc::new(WorldRepository::with_demo_seed());
    let char_id = 90_000_001_i64;
    db.acquire_online_lock(1, 601, Some(char_id), true, base_unix)
        .expect("online lock");
    let ctx = ServerContext::with_sde_and_repository(shared_sde(), db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(base_unix)));
    (ctx, char_id)
}

fn call_browser_lockdown(
    ctx: &ServerContext,
    session: &SessionState,
    method: &str,
) -> Option<Result<PyRep, BridgeRpcError>> {
    handle_misc_cluster_localization(ctx, session, "browserLockdownSvc", method, &[])
}

#[test]
fn alpha_injector_sweep_omega_and_cooldown_boundary_conditions() {
    let (ctx, char_id) = make_ctx(1_775_400_000);
    let now_ft = ctx.now_blueos_filetime_ticks();
    let cd_ticks = crate::services::implant_plugging::ALPHA_INJECTOR_COOLDOWN_TICKS;

    // 1. Empty cooldowns map -> no-op
    sweep_due_login_milestones_and_alpha_injectors(&ctx, 1_775_400_000);
    assert!(ctx.drain_notifications_for_char(char_id).is_empty());

    // 2. Omega character (clone_grade == 2) with expired cooldown -> skipped
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().clone_grade = 2;
        crate::services::implant_plugging::save_alpha_injector_cooldown(
            snap,
            char_id,
            now_ft - cd_ticks - 100,
        );
    });
    sweep_due_login_milestones_and_alpha_injectors(&ctx, 1_775_400_000);
    assert!(ctx.drain_notifications_for_char(char_id).is_empty());

    // 3. Alpha character (clone_grade == 1) 1 tick before cooldown expires -> skipped
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().clone_grade = 1;
        crate::services::implant_plugging::save_alpha_injector_cooldown(
            snap,
            char_id,
            now_ft - cd_ticks + 1,
        );
    });
    sweep_due_login_milestones_and_alpha_injectors(&ctx, 1_775_400_000);
    assert!(ctx.drain_notifications_for_char(char_id).is_empty());

    // 4. Exact boundary (now_ft == last_ft + cd_ticks) -> emits OnAlphaInjectionAvailable
    ctx.db.with_mut_snapshot(|snap| {
        crate::services::implant_plugging::save_alpha_injector_cooldown(
            snap,
            char_id,
            now_ft - cd_ticks,
        );
    });
    sweep_due_login_milestones_and_alpha_injectors(&ctx, 1_775_400_000);
    let notifs = ctx.drain_notifications_for_char(char_id);
    assert_eq!(notifs.len(), 1);
    assert_eq!(notifs[0].event, "OnAlphaInjectionAvailable");

    // 5. Clearing latch allows re-emission on next sweep
    ctx.db.with_mut_snapshot(|snap| {
        clear_alpha_avail_notified(snap, char_id);
        clear_alpha_avail_notified(snap, 999_999_999); // absent char_id is a clean no-op
    });
    sweep_due_login_milestones_and_alpha_injectors(&ctx, 1_775_400_000);
    assert_eq!(ctx.drain_notifications_for_char(char_id).len(), 1);
}

#[test]
fn rnd_buy_datacores_all_validation_and_settlement_branches() {
    let (ctx, char_id) = make_ctx(1_775_400_000);
    let (&agent_id, agent_info) = ctx
        .sde
        .agents
        .iter()
        .find(|(_, a)| a.station_id.is_some() && a.solar_system_id.is_some())
        .expect("seeded agent");
    let st_id = agent_info.station_id.unwrap();
    let sys_id = agent_info.solar_system_id.unwrap();

    let mut sess = SessionState::new(601);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.station_id = Some(st_id);
    sess.solar_system_id = Some(sys_id);

    // 1. Quantity bounds: negative, zero, > 100_000 -> InvalidDatacoreQuantity
    for bad_qty in [-5_i64, 0, 100_001] {
        let err = handle_rnd_buy_datacores(
            &ctx,
            &sess,
            char_id,
            agent_id,
            sys_id,
            &[PyRep::Integer(agent_id), PyRep::Integer(14), PyRep::Integer(bad_qty)],
        )
        .unwrap_err();
        assert_eq!(err.code, "InvalidDatacoreQuantity");
    }

    // 2. Unknown agent -> UnknownAgent
    assert_eq!(
        handle_rnd_buy_datacores(&ctx, &sess, char_id, 999_999_999, sys_id, &[])
            .unwrap_err()
            .code,
        "UnknownAgent"
    );

    // 3. Not docked, wrong system, or wrong station -> AgentNotCoLocated
    let mut undocked = sess.clone();
    undocked.station_id = None;
    assert_eq!(
        handle_rnd_buy_datacores(&ctx, &undocked, char_id, agent_id, sys_id, &[])
            .unwrap_err()
            .code,
        "AgentNotCoLocated"
    );
    assert_eq!(
        handle_rnd_buy_datacores(&ctx, &sess, char_id, agent_id, sys_id + 1, &[])
            .unwrap_err()
            .code,
        "AgentNotCoLocated"
    );
    let mut wrong_st = sess.clone();
    wrong_st.station_id = Some(st_id + 1);
    assert_eq!(
        handle_rnd_buy_datacores(&ctx, &wrong_st, char_id, agent_id, sys_id, &[])
            .unwrap_err()
            .code,
        "AgentNotCoLocated"
    );

    // 4. No active research with agent -> ResearchNotActive
    assert_eq!(
        handle_rnd_buy_datacores(&ctx, &sess, char_id, agent_id, sys_id, &[])
            .unwrap_err()
            .code,
        "ResearchNotActive"
    );

    // 5. Invalid research skill (does not map to a datacore) -> InvalidResearchSkill
    ctx.db.with_mut_snapshot(|snap| {
        crate::services::agents_missions::save_rnd_research_entries(
            snap,
            char_id,
            &[RndResearchEntry {
                agent_id,
                research_type_id: 3300, // Gunnery -> not an R&D datacore skill
                points_per_day: 50.0,
                initial_points: 500.0,
                started_unix: 1_775_400_000,
            }],
        );
    });
    assert_eq!(
        handle_rnd_buy_datacores(&ctx, &sess, char_id, agent_id, sys_id, &[])
            .unwrap_err()
            .code,
        "InvalidResearchSkill"
    );

    // 6. Insufficient research points (99.0 RP < 100.0 RP for default qty=1) -> InsufficientResearchPoints
    ctx.db.with_mut_snapshot(|snap| {
        crate::services::agents_missions::save_rnd_research_entries(
            snap,
            char_id,
            &[RndResearchEntry {
                agent_id,
                research_type_id: 11442,
                points_per_day: 50.0,
                initial_points: 99.0,
                started_unix: 1_775_400_000,
            }],
        );
    });
    assert_eq!(
        handle_rnd_buy_datacores(&ctx, &sess, char_id, agent_id, sys_id, &[])
            .unwrap_err()
            .code,
        "InsufficientResearchPoints"
    );

    // 7. Insufficient ISK (< 10,000 ISK per datacore) -> InsufficientFunds
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().balance = IskCents::from_isk(9_999);
        crate::services::agents_missions::save_rnd_research_entries(
            snap,
            char_id,
            &[RndResearchEntry {
                agent_id,
                research_type_id: 11442,
                points_per_day: 100.0,
                initial_points: 50.0,
                started_unix: 1_775_400_000 - 86_400, // 50 + 100*1 = 150 RP
            }],
        );
    });
    assert_eq!(
        handle_rnd_buy_datacores(&ctx, &sess, char_id, agent_id, sys_id, &[])
            .unwrap_err()
            .code,
        "InsufficientFunds"
    );

    // 8. Default qty=1 succeeds when args has <3 elements, deducts 10,000 ISK and 100 RP -> 50 RP left
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().balance = IskCents::from_isk(25_000);
    });
    let _ = ctx.drain_notifications_for_char(char_id);
    let ok_rep = handle_rnd_buy_datacores(&ctx, &sess, char_id, agent_id, sys_id, &[]).unwrap();
    assert_eq!(ok_rep.dict_get("quantity"), Some(&PyRep::Integer(1)));
    let rem_rp = ok_rep
        .dict_get("remainingResearchPoints")
        .and_then(|v| v.as_f64())
        .unwrap();
    assert!((rem_rp - 50.0).abs() < 1e-5);
    let dc_notifs = ctx.drain_notifications_for_char(char_id);
    assert!(dc_notifs.iter().any(|n| n.event == "OnDatacoreBought"));
}

#[test]
fn count_damaged_sell_items_armor_corp_and_auto_lookup_branches() {
    let (ctx, char_id) = make_ctx(1_775_400_000);
    let corp_id = 98_000_001_i64;
    let st_id = 60_003_760_i64;

    ctx.db.with_mut_snapshot(|snap| {
        // Item 1: Armor damaged singleton owned by corp (raw_item_id == 0 lookup with use_corp = true)
        let mut corp_ship = InventoryItemRecord {
            item_id: 920_001,
            type_id: 587,
            owner_id: corp_id,
            location_id: st_id,
            flag: InventoryFlag::CorpSAG1,
            quantity: 1,
            singleton: true,
            version: 1,
            ..Default::default()
        };
        corp_ship
            .dynamic_attributes
            .insert(attr_ids::ARMOR_DAMAGE, 5.0);
        snap.items.insert(920_001, corp_ship);

        // Item 2: Non-singleton item with damage attr -> NOT counted because !singleton
        let mut stack_item = InventoryItemRecord {
            item_id: 920_002,
            type_id: 588,
            owner_id: char_id,
            location_id: st_id,
            flag: InventoryFlag::Hangar,
            quantity: 5,
            singleton: false,
            version: 1,
            ..Default::default()
        };
        stack_item.dynamic_attributes.insert(attr_ids::DAMAGE, 10.0);
        snap.items.insert(920_002, stack_item);
    });

    ctx.db.with_snapshot(|snap| {
        let parsed = vec![
            (st_id, 587, 0, IskCents(100), 1, 0),
            (st_id, 588, 920_002, IskCents(100), 1, 0),
        ];
        // Without use_corp, corp ship is not matched -> 0
        assert_eq!(
            count_damaged_sell_items(snap, &HashSet::new(), char_id, corp_id, false, &parsed),
            0
        );
        // With use_corp = true, armor-damaged corp ship is matched -> 1
        assert_eq!(
            count_damaged_sell_items(snap, &HashSet::new(), char_id, corp_id, true, &parsed),
            1
        );
    });
}

#[tokio::test]
async fn dogma_set_module_online_and_offline_burnout_capacitor_and_reactivation_delay() {
    let (ctx, char_id) = make_ctx(1_775_400_000);
    let sys_id = 30_000_142_u32;
    let ship_id = 100_000_001_i64; // Seeded Rifter owned by char_id
    let mod_id = 100_000_002_i64;  // Seeded module fitted on 100_000_001

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(sh) = snap.items.get_mut(&ship_id) {
            sh.location_id = sys_id as i64;
            sh.flag = InventoryFlag::None;
        }
        if let Some(m_rec) = snap.items.get_mut(&mod_id) {
            m_rec.dynamic_attributes.insert(attr_ids::HP_STRUCT, 40.0);
            m_rec.dynamic_attributes.insert(attr_ids::DAMAGE, 40.0); // Burned out!
        }
        snap.module_online_states.insert(mod_id, false);
    });
    ctx.invalidate_ship_fitting(ship_id);

    let mut sess = SessionState::new(601);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.ship_id = Some(ship_id);
    sess.solar_system_id = Some(sys_id);
    sess.station_id = None;

    // 1. Ship not owned by character -> ShipOwnershipViolation
    assert_eq!(
        handle_dogma_set_module_online(&ctx, &mut sess, &[PyRep::Integer(999_999), PyRep::Integer(mod_id)])
            .await
            .unwrap_err()
            .code,
        "ShipOwnershipViolation"
    );
    assert_eq!(
        handle_dogma_take_module_offline(&ctx, &mut sess, &[PyRep::Integer(999_999), PyRep::Integer(mod_id)])
            .await
            .unwrap_err()
            .code,
        "ShipOwnershipViolation"
    );

    // 2. Burned-out module in space -> ModuleBurnedOut on both online and offline
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

    // 3. Repair module damage, spawn ship in space with < 75% capacitor -> NotEnoughCapacitorToOnline
    ctx.db.with_mut_snapshot(|snap| {
        snap.items
            .get_mut(&mod_id)
            .unwrap()
            .dynamic_attributes
            .insert(attr_ids::DAMAGE, 0.0);
    });
    let sys = ctx.systems.get_or_spawn(sys_id);
    let ball = Ball {
        id: ship_id,
        type_id: 641,
        position: DVec3::ZERO,
        velocity: DVec3::ZERO,
        max_velocity: 100.0,
        inertia: 1.0,
        mass: 1_000_000.0,
        radius: 100.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let mut cs = CombatState::default();
    cs.max_capacitor = 1_000.0;
    cs.capacitor = 740.0; // 74% < 75%
    sys.spawn_ball_with_meta(ball, cs.clone(), SlimItemMeta::default())
        .await
        .unwrap();

    assert_eq!(
        handle_dogma_set_module_online(&ctx, &mut sess, &[PyRep::Integer(ship_id), PyRep::Integer(mod_id)])
            .await
            .unwrap_err()
            .code,
        "NotEnoughCapacitorToOnline"
    );

    // 4. Raise capacitor to 75% (750.0 / 1000.0) -> succeeds in space, then offline succeeds
    cs.capacitor = 750.0;
    sys.set_combat_state(ship_id, cs).await.unwrap();
    let mut fit = eve_dogma::ShipFittingEngine::from_sde(ship_id, 587, &ctx.sde);
    fit.modules.insert(
        mod_id,
        eve_dogma::FittedModule {
            item_id: mod_id,
            type_id: 440,
            flag_id: InventoryFlag::MedSlot0 as u16,
            state: eve_dogma::ModuleState::Offline,
            charge_type_id: None,
            charge_quantity: 0,
            damage_hp: 0.0,
            max_hp: 40.0,
            dynamic_attribute_overrides: std::collections::HashMap::new(),
        },
    );
    ctx.store_ship_fitting(ship_id, fit.clone());
    let on_rep = handle_dogma_set_module_online(&ctx, &mut sess, &[PyRep::Integer(ship_id), PyRep::Integer(mod_id)])
        .await
        .unwrap();
    assert_eq!(on_rep.dict_get("online"), Some(&PyRep::Bool(true)));

    fit.modules.get_mut(&mod_id).unwrap().state = eve_dogma::ModuleState::Online;
    ctx.store_ship_fitting(ship_id, fit);
    let off_rep = handle_dogma_take_module_offline(&ctx, &mut sess, &[PyRep::Integer(ship_id), PyRep::Integer(mod_id)])
        .await
        .unwrap();
    assert_eq!(off_rep.dict_get("online"), Some(&PyRep::Bool(false)));
}

#[test]
fn browser_lockdown_legacy_entries_hash_and_localization_qa_wrap_eviction() {
    let (ctx, char_id) = make_ctx(1_775_400_000);
    let unauth = SessionState::new(1);

    // Unauthenticated rejected across all browserLockdownSvc methods
    for m in [
        "IsBrowserInLockdown",
        "GetDefaultHomePage",
        "GetFlaggedSitesList",
        "GetFlaggedSitesHash",
    ] {
        assert!(call_browser_lockdown(&ctx, &unauth, m).unwrap().is_err());
    }
    assert!(call_browser_lockdown(&ctx, &unauth, "GetCharacterToSelect").is_none());

    let mut sess = SessionState::new(601);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.role = service_roles::ROLE_PROGRAMMER;

    // Seed legacy string URL alongside structured entry in browser_lockdown_policy
    ctx.db.with_mut_snapshot(|snap| {
        snap.service_state_blobs.insert(
            "browser_lockdown_policy".into(),
            serde_json::json!({
                "lockdown": false,
                "default_home_page": "https://eve.example/",
                "flagged_sites": [
                    "https://legacy-phish.example",
                    {"site_url": "https://malware.example", "site_flag": 5}
                ]
            })
            .to_string(),
        );
    });
    let list_rep = call_browser_lockdown(&ctx, &sess, "GetFlaggedSitesList")
        .unwrap()
        .unwrap();
    let PyRep::List(items) = list_rep else {
        panic!("expected list");
    };
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].dict_get("siteFlag"), Some(&PyRep::Integer(2)));
    assert_eq!(items[1].dict_get("siteFlag"), Some(&PyRep::Integer(5)));

    let h1 = call_browser_lockdown(&ctx, &sess, "GetFlaggedSitesHash")
        .unwrap()
        .unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        set_browser_lockdown_policy(
            snap,
            true,
            "https://eve.example/",
            vec![("https://legacy-phish.example".into(), 3)],
        );
    });
    let h2 = call_browser_lockdown(&ctx, &sess, "GetFlaggedSitesHash")
        .unwrap()
        .unwrap();
    assert_ne!(h1, h2);

    // Localization QA wrap: integer 0/1 args, invalid arg error, and >=256 map eviction
    assert!(handle_update_localization_qa_wrap(&ctx, &sess, &[PyRep::String("bad".into())]).is_err());
    assert!(handle_update_localization_qa_wrap(&ctx, &sess, &[]).is_err());
    ctx.db.with_mut_snapshot(|snap| {
        let mut full_map = BTreeMap::new();
        for uid in 100..356_i64 {
            full_map.insert(uid, true);
        }
        snap.service_state_blobs.insert(
            "localization:qa_wrap_users".into(),
            serde_json::to_string(&full_map).unwrap(),
        );
    });
    assert_eq!(
        handle_update_localization_qa_wrap(&ctx, &sess, &[PyRep::Integer(1)]).unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(
        handle_update_localization_qa_wrap(&ctx, &sess, &[PyRep::Integer(0)]).unwrap(),
        PyRep::Bool(false)
    );
    // Empty owner_notes -> GetAllTextChanges returns PyRep::None
    assert_eq!(
        handle_get_all_text_changes(&ctx, &sess, &[]).unwrap(),
        PyRep::None
    );
}
