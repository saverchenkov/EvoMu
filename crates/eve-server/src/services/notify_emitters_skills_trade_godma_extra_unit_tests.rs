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
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(char_id).or_default().insert(
            3454,
            eve_db::TrainedSkillRecord {
                type_id: 3454,
                level: 3,
                skill_points: 24_000.0,
            },
        );
    });
    let mut fit = eve_dogma::ShipFittingEngine::from_sde(ship_id, 587, &ctx.sde);
    fit.set_character_skills(std::collections::HashMap::from([(3454, 3)]));
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

    // Seed structured browser_lockdown_policy directly in typed ClusterAndRuntimeStore
    ctx.db.with_mut_snapshot(|snap| {
        snap.cluster_and_runtime.browser_lockdown_policy = Some(eve_db::BrowserLockdownPolicyState {
            lockdown: false,
            default_home_page: "https://eve.example/".into(),
            flagged_sites: vec![
                eve_db::FlaggedSiteEntry {
                    site_url: "https://legacy-phish.example".into(),
                    site_flag: 2,
                },
                eve_db::FlaggedSiteEntry {
                    site_url: "https://malware.example".into(),
                    site_flag: 5,
                },
            ],
        });
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
        snap.cluster_and_runtime.localization_qa_wrap_users = full_map;
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

#[tokio::test]
async fn dogma_online_cargo_docked_burnout_zero_type_tp_ewar_and_lockdown_text_mutants() {
    let mut sde = (*shared_sde()).clone();
    let tp_tid = 95_379_u32;
    let mut tp_type = sde.types.get(&440).cloned().expect("seed module 440");
    tp_type.type_id = tp_tid;
    tp_type.group_id = 379; // Target Painter
    tp_type.category_id = 7;
    tp_type.default_attributes.insert(attr_ids::CPU, 10.0);
    tp_type.default_attributes.insert(attr_ids::POWER, 1.0);
    tp_type.default_attributes.insert(554, 30.0); // signatureRadiusBonus
    tp_type.default_attributes.insert(attr_ids::MAX_RANGE, 50_000.0);
    tp_type.default_attributes.insert(158, 20_000.0); // falloff
    sde.types.insert(tp_tid, tp_type);

    let base_unix = 1_775_500_000_u64;
    let db = Arc::new(WorldRepository::with_demo_seed());
    let char_id = 90_000_001_i64;
    db.acquire_online_lock(1, 601, Some(char_id), true, base_unix)
        .expect("online lock");
    let ctx = ServerContext::with_sde_and_repository(Arc::new(sde), db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(base_unix)));

    let sys_id = 30_000_142_u32;
    let station_id = 60_003_760_i64;
    let st_id = 1_000_000_955_001_i64;
    let ship_id = 100_000_001_i64;
    let other_ship_id = 100_000_991_i64;
    let mod_other_loc = 955_101_i64;
    let mod_cargo = 955_102_i64;
    let mod_ghost = 955_103_i64;
    let mod_burned = 955_104_i64;
    let mod_just_under_burn = 955_105_i64;
    let tp_mod_1 = 955_201_i64;
    let tp_mod_2 = 955_202_i64;
    let target_ship_id = 955_301_i64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = Some(st_id);
        }
        if let Some(sh) = snap.items.get_mut(&ship_id) {
            sh.location_id = sys_id as i64;
            sh.flag = InventoryFlag::None;
        }
        snap.skills.entry(char_id).or_default().insert(
            3454,
            eve_db::TrainedSkillRecord {
                type_id: 3454,
                level: 3,
                skill_points: 24_000.0,
            },
        );
        for (item_id, type_id, loc, flag, dyn_attrs) in [
            (other_ship_id, 587_u32, sys_id as i64, InventoryFlag::None, vec![]),
            (mod_other_loc, 440, other_ship_id, InventoryFlag::HiSlot0, vec![]),
            (mod_cargo, 440, ship_id, InventoryFlag::Cargo, vec![]),
            (
                mod_burned,
                440,
                ship_id,
                InventoryFlag::MedSlot0,
                vec![(attr_ids::HP_STRUCT, 40.0), (attr_ids::DAMAGE, 40.0)],
            ),
            (
                mod_just_under_burn,
                440,
                ship_id,
                InventoryFlag::MedSlot3,
                vec![(attr_ids::HP_STRUCT, 40.0), (attr_ids::DAMAGE, 39.0)],
            ),
            (tp_mod_1, tp_tid, ship_id, InventoryFlag::MedSlot1, vec![]),
            (tp_mod_2, tp_tid, ship_id, InventoryFlag::MedSlot2, vec![]),
            (target_ship_id, 587, sys_id as i64, InventoryFlag::None, vec![]),
        ] {
            let mut rec = InventoryItemRecord {
                item_id,
                type_id,
                owner_id: char_id,
                location_id: loc,
                flag,
                quantity: 1,
                singleton: true,
                ..Default::default()
            };
            for (k, v) in dyn_attrs {
                rec.dynamic_attributes.insert(k, v);
            }
            snap.items.insert(item_id, rec);
        }
        snap.module_online_states.insert(mod_burned, false);
        snap.module_online_states.insert(mod_just_under_burn, false);
        snap.module_online_states.insert(tp_mod_1, true);
        snap.module_online_states.insert(tp_mod_2, true);
    });

    let sys = ctx.systems.get_or_spawn(sys_id);
    for (id, tid) in [(ship_id, 587_u32), (target_ship_id, 587_u32)] {
        let mut cs = CombatState::default();
        cs.max_capacitor = 1_000.0;
        cs.capacitor = 1_000.0;
        if id == target_ship_id {
            cs.tp_sig_multiplier = Some(1.3);
        }
        sys.spawn_ball_with_meta(
            Ball {
                id,
                type_id: tid,
                position: DVec3::new(if id == target_ship_id { 1_000.0 } else { 0.0 }, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            cs,
            SlimItemMeta::default(),
        )
        .await
        .unwrap();
    }

    let make_mod = |item_id: i64, type_id: u32, flag_id: u16, state: eve_dogma::ModuleState| {
        eve_dogma::FittedModule {
            item_id,
            type_id,
            flag_id,
            state,
            charge_type_id: None,
            charge_quantity: 0,
            damage_hp: 0.0,
            max_hp: 40.0,
            dynamic_attribute_overrides: std::collections::HashMap::new(),
        }
    };

    let mut fit = eve_dogma::ShipFittingEngine::from_sde(ship_id, 587, &ctx.sde);
    fit.set_character_skills(std::collections::HashMap::from([(3454, 3)]));
    fit.modules.insert(mod_other_loc, make_mod(mod_other_loc, 440, InventoryFlag::MedSlot0 as u16, eve_dogma::ModuleState::Offline));
    fit.modules.insert(mod_cargo, make_mod(mod_cargo, 440, InventoryFlag::MedSlot1 as u16, eve_dogma::ModuleState::Offline));
    fit.modules.insert(mod_ghost, make_mod(mod_ghost, 440, InventoryFlag::MedSlot2 as u16, eve_dogma::ModuleState::Offline));

    let mut sess = SessionState::new(601);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.ship_id = Some(ship_id);
    sess.solar_system_id = Some(sys_id);
    sess.station_id = Some(station_id);

    // 1. Module on other ship OR in Cargo (`was_fitted_in_db` must be false) -> succeeds when present in fit
    ctx.store_ship_fitting(ship_id, fit.clone());
    assert_eq!(
        handle_dogma_set_module_online(&ctx, &mut sess, &[PyRep::Integer(ship_id), PyRep::Integer(mod_other_loc)])
            .await
            .unwrap()
            .dict_get("online"),
        Some(&PyRep::Bool(true))
    );
    ctx.store_ship_fitting(ship_id, fit.clone());
    assert_eq!(
        handle_dogma_set_module_online(&ctx, &mut sess, &[PyRep::Integer(ship_id), PyRep::Integer(mod_cargo)])
            .await
            .unwrap()
            .dict_get("online"),
        Some(&PyRep::Bool(true))
    );

    // 2. Ghost module absent from snap.items (`eff_tid == 0`) emits NO OnGodmaShipEffect;
    //    real module (`eff_tid > 0`) DOES emit OnGodmaShipEffect
    let _ = ctx.drain_notifications_for_char(char_id);
    ctx.store_ship_fitting(ship_id, fit.clone());
    assert!(
        handle_dogma_set_module_online(&ctx, &mut sess, &[PyRep::Integer(ship_id), PyRep::Integer(mod_ghost)])
            .await
            .is_ok()
    );
    let ghost_notifs = ctx.drain_notifications_for_char(char_id);
    assert!(
        !ghost_notifs.iter().any(|n| n.event == "OnGodmaShipEffect"),
        "eff_tid == 0 must not emit OnGodmaShipEffect: {ghost_notifs:?}"
    );

    // 3. Stale docked session (`session.station_id = Some(station_id)` passes early guard) + stale cached fit
    //    vs DB character state:
    //    (a) ch.station_id = None, ch.structure_id = Some(st_id) -> ch_docked = true -> burned module succeeds
    //    (b) ch.station_id = None, ch.structure_id = None -> ch_docked = false -> burned (`40 == 40`) fails ModuleBurnedOut,
    //        while damage `39.0 < 40.0 - 1e-6` succeeds AND emits OnGodmaShipEffect
    let mut burned_stale_fit = eve_dogma::ShipFittingEngine::from_sde(ship_id, 587, &ctx.sde);
    burned_stale_fit.set_character_skills(std::collections::HashMap::from([(3454, 3)]));
    burned_stale_fit.modules.insert(
        mod_burned,
        make_mod(mod_burned, 440, InventoryFlag::MedSlot0 as u16, eve_dogma::ModuleState::Offline),
    );
    ctx.store_ship_fitting(ship_id, burned_stale_fit.clone());
    assert!(
        handle_dogma_set_module_online(&ctx, &mut sess, &[PyRep::Integer(ship_id), PyRep::Integer(mod_burned)])
            .await
            .is_ok()
    );
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.station_id = None;
        ch.structure_id = None;
        snap.module_online_states.insert(mod_burned, false);
    });
    ctx.store_ship_fitting(ship_id, burned_stale_fit);
    assert_eq!(
        handle_dogma_set_module_online(&ctx, &mut sess, &[PyRep::Integer(ship_id), PyRep::Integer(mod_burned)])
            .await
            .unwrap_err()
            .code,
        "ModuleBurnedOut"
    );
    let _ = ctx.drain_notifications_for_char(char_id);
    // Rifter 587 has 3 med slots (MedSlot0..=MedSlot2); move mod_burned out so mod_just_under_burn fits MedSlot0 on rebuild.
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(m) = snap.items.get_mut(&mod_burned) {
            m.flag = InventoryFlag::Cargo;
        }
        if let Some(m) = snap.items.get_mut(&mod_just_under_burn) {
            m.flag = InventoryFlag::MedSlot0;
        }
    });
    ctx.invalidate_ship_fitting(ship_id);
    assert!(
        handle_dogma_set_module_online(&ctx, &mut sess, &[PyRep::Integer(ship_id), PyRep::Integer(mod_just_under_burn)])
            .await
            .is_ok()
    );
    let real_notifs = ctx.drain_notifications_for_char(char_id);
    assert!(
        real_notifs.iter().any(|n| n.event == "OnGodmaShipEffect"),
        "eff_tid > 0 must emit OnGodmaShipEffect: {real_notifs:?}"
    );

    // 4. Target Painter offline recomputes target `tp_sig_multiplier`:
    //    2 active TPs -> taking 1 offline keeps Some(> 1.0); taking 2nd offline clears to None
    sess.station_id = None;
    let mut eff1 = eve_dogma::DogmaItemState::from_sde(tp_mod_1, tp_tid, &ctx.sde);
    eff1.base_attributes.insert(554, 30.0);
    eff1.base_attributes.insert(attr_ids::MAX_RANGE, 50_000.0);
    eff1.base_attributes.insert(158, 20_000.0);
    let mut eff2 = eff1.clone();
    eff2.item_id = tp_mod_2;
    {
        let mut src = ctx.projected_ewar_sources.write().unwrap();
        src.insert(tp_mod_1, ship_id);
        src.insert(tp_mod_2, ship_id);
    }
    ctx.projected_ewar.write().unwrap().insert(
        target_ship_id,
        vec![
            eve_dogma::ProjectedEwarEffect {
                source_module_id: tp_mod_1,
                source_module_type_id: tp_tid,
                charge_type_id: None,
                distance: 1_000.0,
                overloaded: false,
                effective_module: eff1,
            },
            eve_dogma::ProjectedEwarEffect {
                source_module_id: tp_mod_2,
                source_module_type_id: tp_tid,
                charge_type_id: None,
                distance: 1_000.0,
                overloaded: false,
                effective_module: eff2,
            },
        ],
    );
    let mut tp_fit = eve_dogma::ShipFittingEngine::from_sde(ship_id, 587, &ctx.sde);
    tp_fit.set_character_skills(std::collections::HashMap::from([(3454, 3)]));
    tp_fit.modules.insert(tp_mod_1, make_mod(tp_mod_1, tp_tid, InventoryFlag::MedSlot1 as u16, eve_dogma::ModuleState::Online));
    tp_fit.modules.insert(tp_mod_2, make_mod(tp_mod_2, tp_tid, InventoryFlag::MedSlot2 as u16, eve_dogma::ModuleState::Online));
    ctx.store_ship_fitting(ship_id, tp_fit);

    handle_dogma_take_module_offline(&ctx, &mut sess, &[PyRep::Integer(ship_id), PyRep::Integer(tp_mod_1)])
        .await
        .expect("take first TP offline");
    let cs_one_left = sys.get_combat_state(target_ship_id).await.unwrap().unwrap();
    assert!(
        cs_one_left.tp_sig_multiplier.is_some_and(|m| m > 1.0 + 1e-6),
        "expected remaining TP sig multiplier > 1.0, got {:?}",
        cs_one_left.tp_sig_multiplier
    );

    handle_dogma_take_module_offline(&ctx, &mut sess, &[PyRep::Integer(ship_id), PyRep::Integer(tp_mod_2)])
        .await
        .expect("take second TP offline");
    let cs_none_left = sys.get_combat_state(target_ship_id).await.unwrap().unwrap();
    assert_eq!(cs_none_left.tp_sig_multiplier, None);

    // 5. emit_structure_cfg_data_changed emits OnCfgDataChanged("evelocations", [id, name, sys, x, y, z, None])
    let _ = ctx.drain_notifications_for_char(char_id);
    emit_structure_cfg_data_changed(&ctx, sys_id, st_id, "Alpha Keepstar", [10.0, 20.0, 30.0]);
    let cfg_notifs = ctx.drain_notifications_for_char(char_id);
    assert!(
        cfg_notifs.iter().any(|n| {
            n.event == "OnCfgDataChanged"
                && n.args
                    == vec![
                        PyRep::String("evelocations".into()),
                        PyRep::List(vec![
                            PyRep::Integer(st_id),
                            PyRep::String("Alpha Keepstar".into()),
                            PyRep::Integer(i64::from(sys_id)),
                            PyRep::Real(10.0),
                            PyRep::Real(20.0),
                            PyRep::Real(30.0),
                            PyRep::None,
                        ]),
                    ]
        }),
        "expected OnCfgDataChanged payload, got {cfg_notifs:?}"
    );

    // 6. browserLockdownSvc & Localization QA wrap non-empty / role checks
    ctx.db.with_mut_snapshot(|snap| {
        set_browser_lockdown_policy(
            snap,
            false,
            "https://custom.home/",
            vec![("https://a.example".into(), 1), ("https://b.example".into(), 2)],
        );
        snap.owner_notes.insert(
            -8_960_000_000_000,
            eve_db::OwnerNoteRecord {
                note_id: -8_960_000_000_000,
                character_id: 0,
                item_id: 0,
                label: "loc".into(),
                note: r#"{"101":"Hello"}"#.into(),
                timestamp_unix: 0,
            },
        );
    });
    assert_eq!(
        call_browser_lockdown(&ctx, &sess, "IsBrowserInLockdown").unwrap().unwrap(),
        PyRep::Bool(false)
    );
    assert_eq!(
        call_browser_lockdown(&ctx, &sess, "GetDefaultHomePage").unwrap().unwrap(),
        PyRep::String("https://custom.home/".into())
    );
    let PyRep::List(flagged) = call_browser_lockdown(&ctx, &sess, "GetFlaggedSitesList").unwrap().unwrap() else {
        panic!("expected flagged sites list");
    };
    assert_eq!(flagged.len(), 2);

    ctx.db.with_mut_snapshot(|snap| {
        set_browser_lockdown_policy(snap, true, "https://locked.home/", vec![]);
    });
    assert_eq!(
        call_browser_lockdown(&ctx, &sess, "IsBrowserInLockdown").unwrap().unwrap(),
        PyRep::Bool(true)
    );

    let mut unpriv = sess.clone();
    unpriv.role = 0;
    assert_eq!(
        handle_update_localization_qa_wrap(&ctx, &unpriv, &[PyRep::Integer(1)])
            .unwrap_err()
            .code,
        "RoleRequired"
    );

    sess.role = service_roles::ROLE_QA;
    assert_eq!(
        handle_update_localization_qa_wrap(&ctx, &sess, &[PyRep::Integer(1)]).unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(
        handle_get_all_text_changes(&ctx, &sess, &[]).unwrap(),
        PyRep::Dict(vec![(PyRep::Integer(101), PyRep::String("[QA:101] Hello".into()))])
    );
    assert_eq!(
        handle_update_localization_qa_wrap(&ctx, &sess, &[PyRep::Integer(0)]).unwrap(),
        PyRep::Bool(false)
    );
    assert_eq!(
        handle_get_all_text_changes(&ctx, &sess, &[]).unwrap(),
        PyRep::Dict(vec![(PyRep::Integer(101), PyRep::String("Hello".into()))])
    );
}
