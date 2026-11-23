use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::Arc;

use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};

use super::*;

fn dummy_sync_handler(
    _ctx: &ServerContext,
    _session: &mut SessionState,
    _service: &str,
    _method: &str,
    _args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    Some(Ok(PyRep::None))
}

fn dummy_none_handler(
    _ctx: &ServerContext,
    _session: &mut SessionState,
    _service: &str,
    _method: &str,
    _args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    None
}

#[test]
fn default_compiled_registry_builds_without_duplicates_and_looks_up_entries() {
    let registry = RpcRegistry::default_compiled();
    let entries = registry.all_entries();
    assert!(
        entries.len() >= 190,
        "expected >= 190 Track 2A compiled entries, got {}",
        entries.len()
    );

    for (svc, meth) in [
        ("characterEnergyMgr", "GetMyEnergyState"),
        ("lookupSvc", "LookupCharacters"),
        ("dogmaIM", "Activate"),
        ("dogmaIM", "Overload"),
        ("tourneyMgr", "TeleportToArena"),
        ("seasonManager", "reload_fsd_data"),
        ("ProjectDiscovery", "fix_task_id"),
    ] {
        let entry = registry
            .lookup(svc, meth)
            .unwrap_or_else(|| panic!("missing compiled entry {svc}.{meth}"));
        assert_eq!(entry.service, svc);
        assert_eq!(entry.method, meth);
    }

    assert!(registry.lookup("characterEnergyMgr", "NoSuchMethod").is_none()); // rpc-lint: allow negative lookup test fixture
    assert!(registry.lookup("nonexistentService", "Foo").is_none()); // rpc-lint: allow negative lookup test fixture
}

#[test]
fn duplicate_service_method_returns_err() {
    static SLICE_A: &[RpcEntry] = &[rpc_entry(
        "testSvc", // rpc-lint: allow synthetic unit test RPC fixture
        "Echo",
        RpcDispatchHandler::Sync(dummy_sync_handler),
    )];
    static SLICE_B: &[RpcEntry] = &[rpc_entry(
        "testSvc", // rpc-lint: allow synthetic unit test RPC fixture
        "Echo",
        RpcDispatchHandler::Sync(dummy_sync_handler),
    )];

    let err = RpcRegistry::try_from_slices(&[SLICE_A, SLICE_B])
        .expect_err("duplicate (service, method) must fail validation");
    assert!(
        err.contains("testSvc.Echo"),
        "error must identify duplicate pair, got: {err}"
    );
}

#[test]
#[should_panic(expected = "duplicate compiled RPC registration: testSvc.DupMethod")]
fn duplicate_service_method_panics_on_from_slices() {
    static SLICE: &[RpcEntry] = &[
        rpc_entry(
            "testSvc", // rpc-lint: allow synthetic unit test RPC fixture
            "DupMethod",
            RpcDispatchHandler::Sync(dummy_sync_handler),
        ),
        rpc_entry(
            "testSvc", // rpc-lint: allow synthetic unit test RPC fixture
            "DupMethod",
            RpcDispatchHandler::Sync(dummy_sync_handler),
        ),
    ];

    let _ = RpcRegistry::from_slices(&[SLICE]);
}

#[test]
fn empty_service_or_method_rejected() {
    static EMPTY_SVC: &[RpcEntry] = &[rpc_entry(
        "",
        "Foo",
        RpcDispatchHandler::Sync(dummy_sync_handler),
    )];
    static EMPTY_METHOD: &[RpcEntry] = &[rpc_entry(
        "testSvc",
        "",
        RpcDispatchHandler::Sync(dummy_sync_handler),
    )];

    assert!(RpcRegistry::try_from_slices(&[EMPTY_SVC]).is_err());
    assert!(RpcRegistry::try_from_slices(&[EMPTY_METHOD]).is_err());
}

#[test]
fn compiled_rpc_module_files_match_disk_and_have_non_empty_entries() {
    let src_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut seen_files = BTreeSet::new();
    for module in COMPILED_RPC_MODULES {
        assert!(
            seen_files.insert(module.file),
            "duplicate module path in COMPILED_RPC_MODULES: {}",
            module.file
        );
        assert!(
            !module.rpcs.is_empty(),
            "migrated module {} must export at least one RPCS entry",
            module.file
        );
        let full = src_root.join(module.file);
        assert!(
            full.is_file(),
            "compiled module file missing on disk: {}",
            full.display()
        );
    }
    assert_eq!(compiled_rpc_pairs().len(), RpcRegistry::default_compiled().all_entries().len());
}

#[tokio::test]
async fn server_context_routes_compiled_rpcs_and_rejects_unregistered_migrated_methods() {
    let ctx = ServerContext::with_repository(Arc::new(eve_db::WorldRepository::with_demo_seed()));
    assert!(ctx
        .rpc_registry
        .lookup("characterEnergyMgr", "GetMyEnergyState")
        .is_some());

    let mut sess = SessionState::new(98_101);
    sess.user_id = Some(1);
    sess.character_id = Some(90_000_001);
    sess.station_id = Some(60_003_760);
    sess.solar_system_id = Some(30_000_142);
    let kw = HashMap::new();

    // SyncNoKwargs path: characterEnergyMgr.GetMyEnergyState
    let energy_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "characterEnergyMgr",
            None,
            "GetMyEnergyState",
            &[],
            &kw,
            None,
        )
        .await
        .expect("characterEnergyMgr.GetMyEnergyState succeeds");
    assert!(!matches!(energy_rep, PyRep::None));

    // SyncNoService path: ingamereport.report_other
    let report_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "ingamereport",
            None,
            "report_other",
            &[PyRep::Integer(90_000_002)],
            &kw,
            None,
        )
        .await
        .expect("ingamereport.report_other succeeds");
    assert_eq!(report_rep, PyRep::None);

    // SyncReadOnlySession path: lookupSvc.LookupCharacters
    let lookup_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "lookupSvc",
            None,
            "LookupCharacters",
            &[PyRep::String("Demo".into()), PyRep::Bool(false)],
            &kw,
            None,
        )
        .await
        .expect("lookupSvc.LookupCharacters succeeds");
    assert!(matches!(lookup_rep, PyRep::List(_)));

    // Async path via submodule tourney_arena: tourneyMgr.GetPotentialMatches
    let tourney_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "tourneyMgr",
            None,
            "GetPotentialMatches",
            &[],
            &kw,
            None,
        )
        .await
        .expect("tourneyMgr.GetPotentialMatches succeeds");
    assert!(matches!(tourney_rep, PyRep::List(_)));

    // Unregistered methods on migrated services must fail with UnimplementedRPC
    // and never fall through to removed handlers.
    let unregistered_cases = [
        ("characterEnergyMgr", "UnregisteredEnergyMethod"), // rpc-lint: allow negative dispatch test fixture
        ("expertSystemMgr", "UnregisteredExpertMethod"), // rpc-lint: allow negative dispatch test fixture
        ("node_graph", "unregistered_graph_method"), // rpc-lint: allow negative dispatch test fixture
        ("certificateMgr", "UnregisteredCertMethod"), // rpc-lint: allow negative dispatch test fixture
        ("lookupSvc", "UnregisteredLookupMethod"), // rpc-lint: allow negative dispatch test fixture
        ("bannedwords", "unregistered_banned_method"), // rpc-lint: allow negative dispatch test fixture
        ("search", "UnregisteredSearchMethod"), // rpc-lint: allow negative dispatch test fixture
        ("ingamereport", "unregistered_report"), // rpc-lint: allow negative dispatch test fixture
        ("FastCheckoutService", "UnregisteredCheckoutMethod"), // rpc-lint: allow negative dispatch test fixture
        ("activityMgr", "UnregisteredActivityMethod"), // rpc-lint: allow negative dispatch test fixture
        ("LPStoreMgr", "UnregisteredLpMethod"), // rpc-lint: allow negative dispatch test fixture
        ("LPSvc", "UnregisteredLpSvcMethod"), // rpc-lint: allow negative dispatch test fixture
        ("rewardMgr", "UnregisteredRewardMethod"), // rpc-lint: allow negative dispatch test fixture
        ("RWManager", "unregistered_rw_method"), // rpc-lint: allow negative dispatch test fixture
        ("operationsManager", "unregistered_operation"), // rpc-lint: allow negative dispatch test fixture
        ("survey", "UnregisteredSurveyMethod"), // rpc-lint: allow negative dispatch test fixture
        ("eventLog", "UnregisteredTelemetryMethod"), // rpc-lint: allow negative dispatch test fixture
        ("pvpFilamentMgr", "UnregisteredFilamentMethod"), // rpc-lint: allow negative dispatch test fixture
        ("ProjectDiscovery", "unregistered_pd_method"), // rpc-lint: allow negative dispatch test fixture
        ("seasonManager", "unregistered_season_method"), // rpc-lint: allow negative dispatch test fixture
        ("tourneyMgr", "UnregisteredTourneyMethod"), // rpc-lint: allow negative dispatch test fixture
        ("moonExtractions", "UnregisteredMoonMethod"), // rpc-lint: allow negative dispatch test fixture
        ("autoMoonMining", "UnregisteredAutoMoonMethod"), // rpc-lint: allow negative dispatch test fixture
        ("inSpaceCompressionMgr", "UnregisteredCompression"), // rpc-lint: allow negative dispatch test fixture
        ("structureCompressionMgr", "UnregisteredStructureCompression"), // rpc-lint: allow negative dispatch test fixture
        ("corpMiningLedger", "UnregisteredCorpLedger"), // rpc-lint: allow negative dispatch test fixture
        ("characterMiningLedger", "UnregisteredCharLedger"), // rpc-lint: allow negative dispatch test fixture
        ("dogmaIM", "UnregisteredDogmaMethod"), // rpc-lint: allow negative dispatch test fixture
    ];

    for (svc, method) in unregistered_cases {
        let err = ctx
            .dispatch_rpc(&mut sess, svc, None, method, &[], &kw, None)
            .await
            .expect_err("unregistered method on migrated service must fail");
        assert_eq!(
            err.code, "UnimplementedRPC",
            "expected UnimplementedRPC for {svc}.{method}, got {}",
            err.code
        );
    }

    // Legacy method on `operationsManager` still served by unmigrated `client_login`
    // must continue to dispatch until Track 2B/2C migrates `client_login`.
    let tutorial_ok = ctx
        .dispatch_rpc(
            &mut sess,
            "operationsManager",
            None,
            "can_character_play_the_tutorial",
            &[],
            &kw,
            None,
        )
        .await;
    assert!(
        tutorial_ok.is_ok(),
        "operationsManager.can_character_play_the_tutorial must remain callable via client_login fallback: {tutorial_ok:?}"
    );
}

fn make_dogma_test_type(
    type_id: u32,
    group_id: u32,
    category_id: u32,
    effect_ids: Vec<u16>,
    attrs: Vec<(u16, f64)>,
) -> eve_sde::InvType {
    eve_sde::InvType {
        type_id,
        group_id,
        category_id,
        name: format!("RegistryTest-{type_id}"),
        mass: 1_000_000.0,
        volume: 10_000.0,
        capacity: 500.0,
        portion_size: 1,
        radius: 40.0,
        base_price: 100_000.0,
        published: true,
        market_group_id: None,
        race_id: Some(2),
        faction_id: None,
        variation_parent_type_id: None,
        description_id: None,
        icon_id: None,
        graphic_id: None,
        sound_id: None,
        meta_group_id: None,
        meta_level: None,
        tech_level: Some(1),
        default_effect_id: None,
        description: String::new(),
        effect_ids,
        default_attributes: attrs.into_iter().collect(),
    }
}

async fn setup_dogma_space_ship_ctx(
    extra_types: Vec<eve_sde::InvType>,
    fitted_modules: Vec<(i64, u32, eve_db::InventoryFlag)>,
    extra_space_items: Vec<(i64, u32)>,
) -> (ServerContext, SessionState, i64, u32) {
    use eve_db::{InventoryFlag, InventoryItemRecord, WorldRepository};
    use eve_destiny::{Ball, CombatState, MovementMode};
    use eve_sde::SdeRegistry;
    use glam::DVec3;

    let mut sde = SdeRegistry::with_canonical_seed();
    let hull_type_id = 98_587_u32;
    sde.types.insert(
        hull_type_id,
        make_dogma_test_type(hull_type_id, 25, 6, Vec::new(), vec![(14, 4.0), (13, 3.0), (12, 3.0)]),
    );
    for t in extra_types {
        sde.types.insert(t.type_id, t);
    }

    let ctx = ServerContext::with_sde_and_repository(
        Arc::new(sde),
        Arc::new(WorldRepository::with_demo_seed()),
    );
    let char_id = 90_000_001_i64;
    let ship_id = 99_880_001_i64;
    let sys_id = 30_000_142_u32;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.structure_id = None;
            ch.solar_system_id = sys_id;
            ch.active_ship_id = ship_id;
        }
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: hull_type_id,
                owner_id: char_id,
                location_id: i64::from(sys_id),
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        for (item_id, type_id, flag) in fitted_modules {
            snap.items.insert(
                item_id,
                InventoryItemRecord {
                    item_id,
                    type_id,
                    owner_id: char_id,
                    location_id: ship_id,
                    flag,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
        }
        for (item_id, type_id) in extra_space_items {
            snap.items.insert(
                item_id,
                InventoryItemRecord {
                    item_id,
                    type_id,
                    owner_id: char_id,
                    location_id: i64::from(sys_id),
                    flag: InventoryFlag::Hangar,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
        }
    });
    ctx.invalidate_ship_fitting(ship_id);

    ctx.systems
        .get_or_spawn(sys_id)
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: hull_type_id,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState {
                shield_hp: 500.0,
                max_shield_hp: 500.0,
                armor_hp: 500.0,
                max_armor_hp: 500.0,
                structure_hp: 500.0,
                max_structure_hp: 500.0,
                capacitor: 500.0,
                max_capacitor: 500.0,
                signature_radius: 40.0,
                base_signature_radius: Some(40.0),
                ..CombatState::default()
            },
        )
        .await
        .expect("spawn attacker ball");

    let mut sess = SessionState::new(98_102);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(ship_id);
    (ctx, sess, ship_id, sys_id)
}

#[tokio::test]
async fn dogma_im_activate_single_owner_wrapper_sequences_fitting_then_weapons() {
    let turret_type_id = 98_484_u32;
    let turret_id = 99_880_002_i64;
    let target_id = 99_880_003_i64;
    let (ctx, mut sess, _, _) = setup_dogma_space_ship_ctx(
        vec![make_dogma_test_type(
            turret_type_id,
            55,
            7,
            vec![12, 16],
            vec![(54, 10_000.0), (51, 2_500.0), (114, 1.0), (116, 1.0)],
        )],
        vec![(turret_id, turret_type_id, eve_db::InventoryFlag::HiSlot0)],
        Vec::new(),
    )
    .await;
    let kw = HashMap::new();

    // 1-arg `dogmaIM.Activate(target_id)` is declined by `inventory_fitting::handle` and must be
    // handed directly to `combat_legacy_ess` inside `dispatch_dogma_module_rpc` (never returning `None`).
    let err = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "Activate",
            &[PyRep::Integer(target_id)],
            &kw,
            None,
        )
        .await
        .expect_err("unloaded 1-arg weapon target must route to combat_legacy_ess NoAmmoLoaded");
    assert_eq!(err.code, "NoAmmoLoaded");
}

#[tokio::test]
async fn non_weapon_module_targeting_container_or_convoy_cycles_without_returning_none() {
    let shield_booster_type_id = 98_490_u32;
    let mod_id = 99_880_010_i64;
    let kw = HashMap::new();

    for (idx, container_group) in [340_u32, 448, 4644].into_iter().enumerate() {
        let container_type_id = 98_500 + idx as u32;
        let container_id = 99_880_100 + idx as i64;
        let (ctx, mut sess, ship_id, sys_id) = setup_dogma_space_ship_ctx(
            vec![
                make_dogma_test_type(
                    shield_booster_type_id,
                    38,
                    7,
                    vec![16],
                    vec![(73, 5_000.0), (6, 10.0)],
                ),
                make_dogma_test_type(container_type_id, container_group, 2, Vec::new(), Vec::new()),
            ],
            vec![(mod_id, shield_booster_type_id, eve_db::InventoryFlag::MedSlot0)],
            vec![(container_id, container_type_id)],
        )
        .await;
        ctx.systems
            .get_or_spawn(sys_id)
            .add_target_lock(
                ship_id,
                eve_destiny::TargetLock {
                    target_id: container_id,
                    lock_time_ms: 100.0,
                    remaining_lock_ms: 0.0,
                    locked: true,
                    max_range_m: None,
                },
            )
            .await
            .expect("lock container target");

        let res = ctx
            .dispatch_rpc(
                &mut sess,
                "dogmaIM",
                None,
                "Activate",
                &[
                    PyRep::Integer(mod_id),
                    PyRep::String("activate".into()),
                    PyRep::Integer(container_id),
                ],
                &kw,
                None,
            )
            .await;
        assert!(
            res.is_ok(),
            "non-weapon module targeting container group {container_group} must cycle without hitting handle_weapons None: {res:?}"
        );
    }

    // Living World convoy target (convoy 800_000_001 in solar system 30_000_142).
    let (ctx, mut sess, ship_id, sys_id) = setup_dogma_space_ship_ctx(
        vec![make_dogma_test_type(
            shield_booster_type_id,
            38,
            7,
            vec![16],
            vec![(73, 5_000.0), (6, 10.0)],
        )],
        vec![(mod_id, shield_booster_type_id, eve_db::InventoryFlag::MedSlot0)],
        Vec::new(),
    )
    .await;
    ctx.living_world.seed_default_convoys();
    let convoy_id = 800_000_001_i64;
    ctx.systems
        .get_or_spawn(sys_id)
        .add_target_lock(
            ship_id,
            eve_destiny::TargetLock {
                target_id: convoy_id,
                lock_time_ms: 100.0,
                remaining_lock_ms: 0.0,
                locked: true,
                max_range_m: None,
            },
        )
        .await
        .expect("lock convoy target");

    let convoy_res = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "Activate",
            &[
                PyRep::Integer(mod_id),
                PyRep::String("activate".into()),
                PyRep::Integer(convoy_id),
            ],
            &kw,
            None,
        )
        .await;
    assert!(
        convoy_res.is_ok(),
        "non-weapon module targeting Living World convoy must cycle cleanly: {convoy_res:?}"
    );
}

#[tokio::test]
async fn standup_weapon_group_on_non_structure_ship_returns_domain_error_without_none() {
    let standup_gbl_type_id = 98_520_u32;
    let standup_mod_id = 99_880_200_i64;
    let kw = HashMap::new();

    for group_id in [1328_u32, 1330, 1333] {
        let (ctx, mut sess, _, _) = setup_dogma_space_ship_ctx(
            vec![make_dogma_test_type(
                standup_gbl_type_id,
                group_id,
                7,
                vec![16],
                vec![(73, 5_000.0)],
            )],
            vec![(standup_mod_id, standup_gbl_type_id, eve_db::InventoryFlag::HiSlot0)],
            Vec::new(),
        )
        .await;

        let err = ctx
            .dispatch_rpc(
                &mut sess,
                "dogmaIM",
                None,
                "Activate",
                &[PyRep::Integer(standup_mod_id)],
                &kw,
                None,
            )
            .await
            .expect_err("standup weapon on non-structure ship must return domain error");
        assert_eq!(
            err.code, "StructureNotFound",
            "expected StructureNotFound (never InternalError/UnimplementedRPC) for group {group_id}, got {:?}",
            err
        );
    }
}

fn setup_none_returning_tutorial_registry_ctx() -> (ServerContext, SessionState, HashMap<String, PyRep>) {
    static SLICE: &[RpcEntry] = &[rpc_entry(
        "operationsManager",
        "can_character_play_the_tutorial",
        RpcDispatchHandler::Sync(dummy_none_handler),
    )];

    let mut ctx = ServerContext::with_repository(Arc::new(eve_db::WorldRepository::with_demo_seed()));
    ctx.rpc_registry = Arc::new(RpcRegistry::from_slices(&[SLICE]));

    let mut sess = SessionState::new(98_103);
    sess.user_id = Some(1);
    sess.character_id = Some(90_000_001);
    (ctx, sess, HashMap::new())
}

#[cfg(debug_assertions)]
#[tokio::test]
#[should_panic(expected = "returned None")]
async fn table_hit_returning_none_panics_and_never_falls_through_in_debug() {
    let (ctx, mut sess, kw) = setup_none_returning_tutorial_registry_ctx();
    let _ = ctx
        .dispatch_rpc_inner(
            &mut sess,
            "operationsManager",
            None,
            "can_character_play_the_tutorial",
            &[],
            &kw,
            None,
        )
        .await;
}

#[cfg(debug_assertions)]
#[tokio::test]
async fn table_hit_returning_none_returns_internal_error_without_fallthrough_via_dispatch_rpc() {
    let (ctx, mut sess, kw) = setup_none_returning_tutorial_registry_ctx();
    let err = ctx
        .dispatch_rpc(
            &mut sess,
            "operationsManager",
            None,
            "can_character_play_the_tutorial",
            &[],
            &kw,
            None,
        )
        .await
        .expect_err("CatchUnwindFuture must turn debug_assert into InternalError, never falling through to client_login");
    assert_eq!(err.code, "InternalError");
    assert!(
        err.message.contains("returned None"),
        "expected panic payload mention of 'returned None', got: {}",
        err.message
    );
}

#[cfg(not(debug_assertions))]
#[tokio::test]
async fn table_hit_returning_none_returns_unimplemented_without_fallthrough_in_release() {
    let (ctx, mut sess, kw) = setup_none_returning_tutorial_registry_ctx();
    let err = ctx
        .dispatch_rpc(
            &mut sess,
            "operationsManager",
            None,
            "can_character_play_the_tutorial",
            &[],
            &kw,
            None,
        )
        .await
        .expect_err("table hit returning None must fail with UnimplementedRPC");
    assert_eq!(err.code, "UnimplementedRPC"); // rpc-lint: allow MachoNet error code assertion
}

#[test]
fn track_2b_registry_registers_all_modules_without_duplicates() {
    let files: BTreeSet<&str> = compiled_rpc_module_files().into_iter().collect();
    let expected_track_2b = [
        ("services/campaigns_and_milestones.rs", 11_usize),
        ("services/cosmetics_events.rs", 1),
        ("services/cosmetics_ui_ops.rs", 30),
        ("services/config_service.rs", 20),
        ("services/hypernet_raffle_ops.rs", 16),
        ("services/implant_plugging.rs", 3),
        ("services/pve_incursion_sites.rs", 4),
        ("services/security_mgr.rs", 1),
        ("services/jump_clone_names.rs", 2),
        ("services/fw_enlistment.rs", 6),
        ("services/fw_gameplay_rpc.rs", 28),
        ("services/insurgency_services.rs", 4),
        ("services/sov_warzone_dynamic.rs", 11),
        ("services/industry_facilities_bps.rs", 12),
        ("services/pi_colonies.rs", 8),
        ("services/pi_orbital_registry.rs", 4),
        ("services/pi_orbitals_gm.rs", 9),
        ("services/pi_orbitals.rs", 16),
        ("services/market_orders_escrow.rs", 5),
        ("services/fitting_dogma_ops.rs", 10),
        ("services/fitting_mgr_service.rs", 22),
        ("services/fitting_saved_multifit.rs", 17),
        ("services/combat_fighters.rs", 11),
        ("services/combat_npcs_drones.rs", 14),
        ("services/fleet_social_access_group_bookmarks.rs", 17),
        ("services/fleet_social_finder_tactical.rs", 33),
        ("services/fleet_social_bookmarks.rs", 19),
        ("services/inventory_stacking_ops.rs", 4),
    ];
    for (file, expected_len) in expected_track_2b {
        assert!(files.contains(file), "missing Track 2B module {file}");
        let mod_entry = COMPILED_RPC_MODULES
            .iter()
            .find(|m| m.file == file)
            .expect("module exists");
        assert_eq!(
            mod_entry.rpcs.len(),
            expected_len,
            "unexpected RPC count for {file}"
        );
    }

    let registry =
        RpcRegistry::try_from_modules(COMPILED_RPC_MODULES).expect("zero duplicate entries");
    assert!(
        COMPILED_RPC_MODULES.len() >= 44,
        "expected Track 2A + 2B module count >= 44, got {}",
        COMPILED_RPC_MODULES.len()
    );
    assert!(
        registry.all_entries().len() >= 526,
        "expected Track 2A + 2B entries >= 526, got {}",
        registry.all_entries().len()
    );

    for (svc, method) in [
        ("loginCampaignManager", "get_client_campaign_state"),
        ("shipCosmeticsMgr", "GiveSkin"),
        ("shipCosmeticsMgr", "ActivateSkinLicense"),
        ("config", "GetMap"),
        ("raffleProxy", "AmIBanned"),
        ("raffleProxy", "Grab"),
        ("skillMgr2", "CheckAndSendNotifications"),
        ("incursion", "GetIncursionLPLog"),
        ("securityMgr", "get_modified_systems"),
        ("publicQaToolsServer", "MoveMeTo"),
        ("fwCharacterEnlistmentMgr", "GetMyEnlistment"),
        ("facWarMgr", "GetSolarSystemLPs"),
        ("insurgencySolarsystem", "GetAllVisibleCampaigns"),
        ("dynamicResourceCacheMgr", "GetDynamicResourceSettings"),
        ("blueprintManager", "GetLimits"),
        ("planetMgr", "GetColony"),
        ("planetOrbitalRegistryBroker", "GetSettingsInfo"),
        ("planetMgr", "GMGetCompleteResource"),
        ("planetMgr", "GetProgramResultInfo"),
        ("marketProxy", "GetOldPriceHistory"),
        ("dogmaIM", "ShipOnlineModules"),
        ("charFittingMgr", "GetFittings"),
        ("insuranceSvc", "GetInsurancePrices"),
        ("fighterMgr", "CmdScoopAbandonedFighterFromSpace"),
        ("ship", "LaunchDrones"),
        ("accessGroupBookmarkMgr", "GetMyActiveBookmarks"),
        ("fleetProxy", "GetAvailableFleetAds"),
        ("fleetObjectHandler", "GetFleetID"),
        ("bookmark", "GetBookmarks"),
        ("invbroker", "SplitStack"),
    ] {
        assert!(
            registry.lookup(svc, method).is_some(),
            "missing Track 2B registry lookup for {svc}.{method}"
        );
    }
}

#[tokio::test]
async fn track_2b_dispatch_routes_and_preserves_hooks_and_rejects_unregistered() {
    let ctx = ServerContext::with_repository(Arc::new(eve_db::WorldRepository::with_demo_seed()));
    let mut sess = SessionState::new(98_202);
    sess.user_id = Some(1);
    sess.character_id = Some(90_000_001);
    sess.station_id = Some(60_003_760);
    sess.solar_system_id = Some(30_000_142);
    sess.role = ServerContext::ROLE_GM_CANONICAL_MASK;
    let kw = HashMap::new();

    // Sync path: config.GetMap, blueprintManager.GetLimits
    assert!(ctx
        .dispatch_rpc(&mut sess, "config", None, "GetMap", &[], &kw, None)
        .await
        .is_ok());
    assert!(ctx
        .dispatch_rpc(&mut sess, "blueprintManager", None, "GetLimits", &[], &kw, None)
        .await
        .is_ok());

    // SyncReadOnlySession path: securityMgr.get_modified_systems
    assert!(matches!(
        ctx.dispatch_rpc(
            &mut sess,
            "securityMgr",
            None,
            "get_modified_systems",
            &[],
            &kw,
            None,
        )
        .await,
        Ok(PyRep::Dict(_))
    ));

    // SyncNoKwargs path: fwCharacterEnlistmentMgr.GetMyEnlistment
    assert!(ctx
        .dispatch_rpc(
            &mut sess,
            "fwCharacterEnlistmentMgr",
            None,
            "GetMyEnlistment",
            &[],
            &kw,
            None,
        )
        .await
        .is_ok());

    // Async path + single-owner split check: shipCosmeticsMgr.ActivateSkinLicense belongs to cosmetics_ui_ops
    let skin_err = ctx
        .dispatch_rpc(
            &mut sess,
            "shipCosmeticsMgr",
            None,
            "ActivateSkinLicense",
            &[],
            &kw,
            None,
        )
        .await
        .expect_err("missing typeID produces domain error");
    assert_eq!(skin_err.code, "InvalidSkinLicenseType");

    // BoundAsync sync-wrapped path: planetOrbitalRegistryBroker.GetSettingsInfo
    assert!(ctx
        .dispatch_rpc(
            &mut sess,
            "planetOrbitalRegistryBroker",
            None,
            "GetSettingsInfo",
            &[PyRep::Integer(40_009_082)],
            &kw,
            None,
        )
        .await
        .is_ok());

    // Preserved abyssal pocket restriction in industry_facilities_bps::dispatch_rpc
    assert!(ctx
        .dispatch_rpc(
            &mut sess,
            "map",
            None,
            "GetIndustryCostModifier",
            &[PyRep::Integer(30_000_142)],
            &kw,
            None,
        )
        .await
        .is_ok());
    let prev_char_loc = ctx.db.with_mut_snapshot(|snap| {
        let ch = snap
            .characters
            .get_mut(&90_000_001)
            .expect("demo character 90_000_001 exists");
        let prev = (ch.solar_system_id, ch.station_id, ch.structure_id);
        ch.solar_system_id = 32_000_001;
        ch.station_id = None;
        ch.structure_id = None;
        prev
    });
    let mut abyss_sess = sess.clone();
    abyss_sess.solar_system_id = Some(32_000_001);
    abyss_sess.station_id = None;
    abyss_sess.structure_id = None;
    let abyss_err = ctx
        .dispatch_rpc(
            &mut abyss_sess,
            "map",
            None,
            "GetIndustryCostModifier",
            &[PyRep::Integer(30_000_142)],
            &kw,
            None,
        )
        .await
        .expect_err("abyssal map restriction must block map.GetIndustryCostModifier");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            (ch.solar_system_id, ch.station_id, ch.structure_id) = prev_char_loc;
        }
    });
    assert_eq!(abyss_err.code, "MapDisabledInAbyssalSpace");

    // Unregistered methods on newly migrated top-level services must fail with UnimplementedRPC
    for (svc, method) in [
        ("shipCosmeticsMgr", "UnregisteredCosmeticMethod"), // rpc-lint: allow negative dispatch test fixture
        ("overviewPresetMgr", "UnregisteredOverviewMethod"), // rpc-lint: allow negative dispatch test fixture
        ("sharedSettingsMgr", "UnregisteredSharedSetting"), // rpc-lint: allow negative dispatch test fixture
        ("monolithTaskTrackerMgr", "UnregisteredTracker"), // rpc-lint: allow negative dispatch test fixture
        ("contextualOfferMgr", "UnregisteredOfferMethod"), // rpc-lint: allow negative dispatch test fixture
        ("nes_intro", "unregistered_nes_method"), // rpc-lint: allow negative dispatch test fixture
        ("crateService", "UnregisteredCrateMethod"), // rpc-lint: allow negative dispatch test fixture
        ("loginCampaignManager", "unregistered_login_campaign"), // rpc-lint: allow negative dispatch test fixture
        ("seasonalLoginCampaignManager", "unregistered_seasonal"), // rpc-lint: allow negative dispatch test fixture
        ("infoGatheringMgr", "UnregisteredTelemetryMethod"), // rpc-lint: allow negative dispatch test fixture
        ("milestoneMgr", "UnregisteredMilestoneMethod"), // rpc-lint: allow negative dispatch test fixture
        ("communityTrackMgr", "unregistered_community_track"), // rpc-lint: allow negative dispatch test fixture
    ] {
        let err = ctx
            .dispatch_rpc(&mut sess, svc, None, method, &[], &kw, None)
            .await
            .expect_err("unregistered migrated method must fail with UnimplementedRPC");
        assert_eq!(err.code, "UnimplementedRPC"); // rpc-lint: allow MachoNet error code assertion
    }
}

#[tokio::test]
async fn public_qa_tools_migrated_methods_bypass_gm_qa_precheck_and_hit_rpc_registry() {
    let ctx = ServerContext::with_repository(Arc::new(eve_db::WorldRepository::with_demo_seed()));
    let kw = HashMap::new();

    let mut gm_sess = SessionState::new(98_301);
    gm_sess.user_id = Some(1);
    gm_sess.character_id = Some(90_000_001);
    gm_sess.corporation_id = Some(98_000_001);
    gm_sess.solar_system_id = Some(30_000_142);
    gm_sess.station_id = Some(60_003_760);
    gm_sess.role = eve_machonet::service_roles::ROLE_ADMIN | eve_machonet::service_roles::ROLE_QA;

    // Migrated `publicQaToolsServer` methods must be registered in `RpcRegistry` and
    // must NOT be intercepted by `handle_gm_qa_rpcs` before `self.rpc_registry.lookup`.
    assert!(ctx.rpc_registry.lookup("publicQaToolsServer", "MoveMeTo").is_some());
    assert!(ctx
        .rpc_registry
        .lookup("publicQaToolsServer", "CanGiveItemForMultifit")
        .is_some());
    assert!(ctx.rpc_registry.lookup("publicQaToolsServer", "SlashCmd").is_none());
    for method in ["MoveMeTo", "CanGiveItemForMultifit"] {
        assert!(
            crate::services::character_skills::character_contacts_gm::handle_gm_qa_rpcs(
                &ctx,
                &mut gm_sess,
                "publicQaToolsServer",
                method,
                &[],
            )
            .await
            .is_none(),
            "handle_gm_qa_rpcs must not shadow publicQaToolsServer.{method}"
        );
    }

    // Live `dispatch_rpc` routes `CanGiveItemForMultifit` and `MoveMeTo` via `RpcRegistry`,
    // while `publicQaToolsServer.SlashCmd` still routes cleanly through `handle_gm_qa_rpcs`.
    let can_give = ctx
        .dispatch_rpc(
            &mut gm_sess,
            "publicQaToolsServer",
            None,
            "CanGiveItemForMultifit",
            &[],
            &kw,
            None,
        )
        .await
        .expect("publicQaToolsServer.CanGiveItemForMultifit succeeds via RpcRegistry");
    assert_eq!(can_give, PyRep::Bool(true));

    let dest_sys_id = ctx
        .sde
        .solar_systems
        .keys()
        .copied()
        .find(|&id| id == 30_002_537)
        .or_else(|| ctx.sde.solar_systems.keys().copied().next())
        .expect("SDE must contain at least one solar system");
    let move_rep = ctx
        .dispatch_rpc(
            &mut gm_sess,
            "publicQaToolsServer",
            None,
            "MoveMeTo",
            &[PyRep::Integer(i64::from(dest_sys_id))],
            &kw,
            None,
        )
        .await
        .expect("publicQaToolsServer.MoveMeTo succeeds via RpcRegistry");
    assert_eq!(move_rep.dict_get("moved"), Some(&PyRep::Bool(true)));
    assert_eq!(
        move_rep.dict_get("solarSystemID"),
        Some(&PyRep::Integer(i64::from(dest_sys_id)))
    );

    let slash_rep = ctx
        .dispatch_rpc(
            &mut gm_sess,
            "publicQaToolsServer",
            None,
            "SlashCmd",
            &[PyRep::String("/whoami".into())],
            &kw,
            None,
        )
        .await
        .expect("publicQaToolsServer.SlashCmd continues to route via handle_gm_qa_rpcs");
    assert!(!matches!(slash_rep, PyRep::None));

    // Central QA role gate still blocks non-GM/QA sessions, and unregistered methods
    // on `publicQaToolsServer` return `UnimplementedRPC`.
    let mut player_sess = gm_sess.clone();
    player_sess.role = eve_machonet::service_roles::ROLE_PLAYER_DEFAULT;
    for method in ["MoveMeTo", "CanGiveItemForMultifit"] {
        let denied = ctx
            .dispatch_rpc(&mut player_sess, "publicQaToolsServer", None, method, &[], &kw, None)
            .await
            .expect_err("non-GM/QA session must fail central QA gate");
        assert_eq!(denied.code, "InsufficientRole"); // rpc-lint: allow MachoNet error code assertion
    }

    let unimpl = ctx
        .dispatch_rpc(
            &mut gm_sess,
            "publicQaToolsServer",
            None,
            "UnregisteredQaMethod", // rpc-lint: allow negative dispatch test fixture
            &[],
            &kw,
            None,
        )
        .await
        .expect_err("unregistered publicQaToolsServer method must fail with UnimplementedRPC");
    assert_eq!(unimpl.code, "UnimplementedRPC"); // rpc-lint: allow MachoNet error code assertion
}

#[test]
fn track_2c_registry_registers_all_modules_without_duplicates() {
    let files: std::collections::HashSet<&str> =
        COMPILED_RPC_MODULES.iter().map(|m| m.file).collect();
    let expected_track_2c = [
        ("services/character_skill_ops.rs", 5usize),
        ("services/space_entity_mining.rs", 5),
        ("services/contracts_service.rs", 20),
        ("services/map_service.rs", 16),
        ("services/corp_registry_ops.rs", 10),
        ("services/shipcaster_ops.rs", 6),
        ("services/abyssal_void_mgr.rs", 8),
        ("services/market_contracts.rs", 26),
        ("services/wallet_serverops.rs", 35),
        ("services/notify_emitters_skills_trade_godma.rs", 4),
        ("services/structure_profiles_directory.rs", 7),
        ("services/structure_profiles_rpc.rs", 16),
        ("services/structure_upwell_ops.rs", 40),
        ("services/crimewatch_bounty.rs", 24),
        ("services/combat_wrecks_killmails.rs", 4),
        ("services/character_clones_implants.rs", 22),
        ("services/character_skills_helpers.rs", 18),
        ("services/character_contacts.rs", 41),
        ("services/fitting_dogma_im.rs", 33),
        ("services/fitting_ships_mastery.rs", 33),
        ("services/fitting_containers_ships_ops.rs", 13),
        ("services/fleet_social_fleet_ops.rs", 47),
        ("services/fleet_social_fleets.rs", 24),
        ("services/fleet_social_chat_auth_ops.rs", 6),
        ("services/fleet_social_mail.rs", 42),
        ("services/mailing_lists_ops.rs", 27),
        ("services/space_cyno_filaments.rs", 7),
        ("services/space_destiny.rs", 21),
    ];
    for (file, expected_len) in expected_track_2c {
        assert!(files.contains(file), "missing Track 2C module {file}");
        let mod_entry = COMPILED_RPC_MODULES
            .iter()
            .find(|m| m.file == file)
            .expect("module exists");
        assert_eq!(
            mod_entry.rpcs.len(),
            expected_len,
            "unexpected RPC count for {file}"
        );
    }

    let registry =
        RpcRegistry::try_from_modules(COMPILED_RPC_MODULES).expect("zero duplicate entries");
    assert!(
        COMPILED_RPC_MODULES.len() >= 72,
        "expected Track 2A + 2B + 2C module count >= 72, got {}",
        COMPILED_RPC_MODULES.len()
    );
    assert!(
        registry.all_entries().len() >= 1095,
        "expected Track 2A + 2B + 2C entries >= 1095, got {}",
        registry.all_entries().len()
    );

    for (svc, method) in [
        ("skillMgr2", "GetSkillChangesForISIS"),
        ("reprocessingSvc", "GetQuotes"),
        ("contractProxy", "GetLoginInfo"),
        ("contractProxy", "NumOutstandingContracts"),
        ("marketProxy", "GetPlexOrders"),
        ("account", "GetCashBalance"),
        ("vaultManager", "get_account_balance"),
        ("map", "GetBeaconCount"),
        ("corpRegistry", "GetSuggestedTickerNames"),
        ("shipCasterLauncherMgr", "GetFactionsWithShipcaster"),
        ("abyssalMgr", "ClientIsReady"),
        ("voidSpaceMgr", "ClientIsReady"),
        ("posMgr", "SetSessionCheck"),
        ("billMgr", "GetCorporationBills"),
        ("account", "GetDefaultContactCost"),
        ("machoNet", "GetClusterGameStatisticsForClient"),
        ("pingService", "Ping"),
        ("browserLockdownSvc", "IsBrowserInLockdown"),
        ("structureDirectory", "GetMyCharacterStructures"),
        ("structureProfiles", "GetProfiles"),
        ("structureDeployment", "Unanchor"),
        ("structureSettings", "CharacterHasService"),
        ("bountyProxy", "GetMyBounties"),
        ("ship", "AbortSelfDestruct"),
        ("charUnboundMgr", "GetCharacterInfo"),
        ("skillMgr2", "GetImplants"),
        ("charMgr", "GetContactList"),
        ("charMgr", "GetSettingsInfo"),
        ("mailMgr", "GetLabels"),
        ("mailMgr", "MoveAllToTrash"),
        ("mailingListsMgr", "GetJoinedLists"),
        ("calendarMgr", "GetEventList"),
        ("dogmaIM", "GetTargets"),
        ("dogmaIM", "GetAllInfo"),
        ("charFittingMgr", "LinkWeapons"),
        ("ship", "GetDirtTimestamp"),
        ("invbroker", "FitFitting"),
        ("fleetObjectHandler", "GetMotd"),
        ("fleetMgr", "CreateFleet"),
        ("wormholeMgr", "WormholeJump"),
        ("jumpTimers", "GetTimers"),
        ("beyonce", "CallComponentFromClient"),
    ] {
        assert!(
            registry.lookup(svc, method).is_some(),
            "missing Track 2C registry lookup for {svc}.{method}"
        );
    }

    // Conditional fallthrough, shadowed, deferred, and pre/post-hook entries intentionally left in fallback.
    for (svc, method) in [
        ("skillMgr2", "GetRespecInfo"),
        ("skillMgr2", "AbortTraining"),
        ("jumpCloneSvc", "GetShipCloneState"),
        ("killRightMgr", "SellKillRight"),
        ("fleetObjectHandler", "CreateFleet"),
        ("miningScanMgr", "perform_scan"),
        ("userSvc", "GetRedeemTokens"),
        ("account", "GiveCashFromCorpAccount"),
        ("dungeonInstanceCacheMgr", "GetFactionWarfareBattlefieldInstances"),
        ("map", "GetAllRoamingWeatherSystems"),
    ] {
        assert!(
            registry.lookup(svc, method).is_none(),
            "expected {svc}.{method} to stay outside compiled registry"
        );
    }
}

#[tokio::test]
async fn track_2c_dispatch_routes_and_preserves_hooks_and_rejects_unregistered() {
    let ctx = ServerContext::with_repository(Arc::new(eve_db::WorldRepository::with_demo_seed()));
    let mut sess = SessionState::new(98_401);
    sess.user_id = Some(1);
    sess.character_id = Some(90_000_001);
    sess.station_id = Some(60_003_760);
    sess.solar_system_id = Some(30_000_142);
    sess.role = ServerContext::ROLE_GM_CANONICAL_MASK;
    let kw = HashMap::new();

    // Sync path: contracts_service, character_skill_ops, corp_registry_ops, market_contracts,
    // wallet_serverops, fleet_social_chat_auth_ops, fleet_social_mail, mailing_lists_ops
    assert!(ctx
        .dispatch_rpc(&mut sess, "contractProxy", None, "GetLoginInfo", &[], &kw, None)
        .await
        .is_ok());
    assert!(ctx
        .dispatch_rpc(
            &mut sess,
            "contractProxy",
            None,
            "NumOutstandingContracts",
            &[],
            &kw,
            None,
        )
        .await
        .is_ok());
    assert!(ctx
        .dispatch_rpc(&mut sess, "account", None, "GetCashBalance", &[], &kw, None)
        .await
        .is_ok());
    assert!(ctx
        .dispatch_rpc(
            &mut sess,
            "vaultManager",
            None,
            "get_account_balance",
            &[],
            &kw,
            None,
        )
        .await
        .is_ok());
    assert!(ctx
        .dispatch_rpc(
            &mut sess,
            "skillMgr2",
            None,
            "GetSkillChangesForISIS",
            &[],
            &kw,
            None,
        )
        .await
        .is_ok());
    assert!(ctx
        .dispatch_rpc(
            &mut sess,
            "corpRegistry",
            None,
            "GetSuggestedTickerNames",
            &[PyRep::String("Alpha Beta".into())],
            &kw,
            None,
        )
        .await
        .is_ok());
    assert!(matches!(
        ctx.dispatch_rpc(&mut sess, "pingService", None, "Ping", &[], &kw, None)
            .await,
        Ok(PyRep::Integer(_))
    ));
    assert!(matches!(
        ctx.dispatch_rpc(&mut sess, "posMgr", None, "SetSessionCheck", &[], &kw, None)
            .await,
        Ok(PyRep::Bool(true))
    ));
    assert!(ctx
        .dispatch_rpc(&mut sess, "charMgr", None, "GetSettingsInfo", &[], &kw, None)
        .await
        .is_ok());
    assert!(matches!(
        ctx.dispatch_rpc(&mut sess, "mailMgr", None, "GetLabels", &[], &kw, None)
            .await,
        Ok(PyRep::Dict(_))
    ));
    assert!(matches!(
        ctx.dispatch_rpc(
            &mut sess,
            "mailingListsMgr",
            None,
            "GetJoinedLists",
            &[],
            &kw,
            None,
        )
        .await,
        Ok(PyRep::Dict(_))
    ));

    // Preserved mailMgr.SendMail recipient block check in fleet_social_mail::dispatch_rpc
    let blocker_id = 90_000_002i64;
    ctx.social_extras
        .lock()
        .unwrap()
        .blocked
        .entry(blocker_id)
        .or_default()
        .insert(90_000_001);
    let block_err = ctx
        .dispatch_rpc(
            &mut sess,
            "mailMgr",
            None,
            "SendMail",
            &[
                PyRep::List(vec![PyRep::Integer(blocker_id)]),
                PyRep::String("Blocked".into()),
                PyRep::String("Body".into()),
            ],
            &kw,
            None,
        )
        .await
        .expect_err("blocked recipient must reject mailMgr.SendMail");
    assert_eq!(block_err.code, "ContactBlocked");

    // SyncReadOnlySession path: map_service, notify_emitters_skills_trade_godma
    assert!(matches!(
        ctx.dispatch_rpc(&mut sess, "map", None, "GetBeaconCount", &[], &kw, None)
            .await,
        Ok(PyRep::Dict(_))
    ));
    assert!(matches!(
        ctx.dispatch_rpc(
            &mut sess,
            "browserLockdownSvc",
            None,
            "IsBrowserInLockdown",
            &[],
            &kw,
            None,
        )
        .await,
        Ok(PyRep::Bool(_))
    ));

    // SyncNoKwargs path: crimewatch_bounty
    assert!(ctx
        .dispatch_rpc(&mut sess, "bountyProxy", None, "GetMyBounties", &[], &kw, None)
        .await
        .is_ok());

    // Async path: abyssal_void_mgr, shipcaster_ops, structure_profiles_rpc
    let abyss_gate_err = ctx
        .dispatch_rpc(
            &mut sess,
            "abyssalMgr",
            None,
            "ClientIsReady",
            &[],
            &kw,
            None,
        )
        .await
        .expect_err("abyssalMgr.ClientIsReady outside abyssal space returns NotInAbyssalSpace");
    assert_eq!(abyss_gate_err.code, "NotInAbyssalSpace");
    assert!(ctx
        .dispatch_rpc(
            &mut sess,
            "voidSpaceMgr",
            None,
            "ClientIsReady",
            &[],
            &kw,
            None,
        )
        .await
        .is_ok());
    assert!(ctx
        .dispatch_rpc(
            &mut sess,
            "shipCasterLauncherMgr",
            None,
            "GetFactionsWithShipcaster",
            &[],
            &kw,
            None,
        )
        .await
        .is_ok());
    assert!(ctx
        .dispatch_rpc(&mut sess, "structureProfiles", None, "GetProfiles", &[], &kw, None)
        .await
        .is_ok());

    // BoundAsync path: space_destiny, fitting_dogma_im, space_entity_mining
    assert!(ctx
        .dispatch_rpc(&mut sess, "jumpTimers", None, "GetTimers", &[], &kw, None)
        .await
        .is_ok());
    assert!(ctx
        .dispatch_rpc(&mut sess, "dogmaIM", None, "GetTargets", &[], &kw, None)
        .await
        .is_ok());
    let quote_res = ctx
        .dispatch_rpc(&mut sess, "reprocessingSvc", None, "GetQuotes", &[], &kw, None)
        .await
        .expect("reprocessingSvc.GetQuotes should succeed while docked");
    assert!(
        quote_res.dict_get("stationBaseYield").is_some()
            || matches!(quote_res, PyRep::Tuple(_))
    );

    // Preserved abyssal pocket restriction in map_service::dispatch_rpc
    let prev_char_loc = ctx.db.with_mut_snapshot(|snap| {
        let ch = snap
            .characters
            .get_mut(&90_000_001)
            .expect("demo character 90_000_001 exists");
        let prev = (ch.solar_system_id, ch.station_id, ch.structure_id);
        ch.solar_system_id = 32_000_001;
        ch.station_id = None;
        ch.structure_id = None;
        prev
    });
    let mut abyss_sess = sess.clone();
    abyss_sess.solar_system_id = Some(32_000_001);
    abyss_sess.station_id = None;
    abyss_sess.structure_id = None;
    let abyss_err = ctx
        .dispatch_rpc(&mut abyss_sess, "map", None, "GetBeaconCount", &[], &kw, None)
        .await
        .expect_err("abyssal map restriction must block map.GetBeaconCount");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            (ch.solar_system_id, ch.station_id, ch.structure_id) = prev_char_loc;
        }
    });
    assert_eq!(abyss_err.code, "MapDisabledInAbyssalSpace");

    // Excluded conditional fallthrough & shadowed handlers preserve fallback routing
    let scan_err = ctx
        .dispatch_rpc(&mut sess, "miningScanMgr", None, "perform_scan", &[], &kw, None)
        .await
        .expect_err("empty miningScanMgr.perform_scan must remain UnimplementedRPC");
    assert_eq!(scan_err.code, "UnimplementedRPC"); // rpc-lint: allow MachoNet error code assertion
    assert!(ctx
        .dispatch_rpc(&mut sess, "userSvc", None, "GetRedeemTokens", &[], &kw, None)
        .await
        .is_ok());
    assert!(ctx
        .dispatch_rpc(
            &mut sess,
            "map",
            None,
            "GetAllRoamingWeatherSystems",
            &[],
            &kw,
            None,
        )
        .await
        .is_ok());
    assert!(ctx
        .dispatch_rpc(&mut sess, "skillMgr2", None, "GetRespecInfo", &[], &kw, None)
        .await
        .is_ok());

    // Unregistered methods on newly migrated top-level services must fail with UnimplementedRPC
    for (svc, method) in [
        ("contractProxy", "UnregisteredContractMethod"), // rpc-lint: allow negative dispatch test fixture
        ("map", "UnregisteredMapMethod"), // rpc-lint: allow negative dispatch test fixture
        ("reprocessingSvc", "UnregisteredReprocessMethod"), // rpc-lint: allow negative dispatch test fixture
        ("shipCasterLauncherMgr", "UnregisteredShipcasterMethod"), // rpc-lint: allow negative dispatch test fixture
        ("abyssalMgr", "UnregisteredAbyssalMethod"), // rpc-lint: allow negative dispatch test fixture
        ("voidSpaceMgr", "UnregisteredVoidSpaceMethod"), // rpc-lint: allow negative dispatch test fixture
        ("mailingListsMgr", "UnregisteredMailingListMethod"), // rpc-lint: allow negative dispatch test fixture
    ] {
        let err = ctx
            .dispatch_rpc(&mut sess, svc, None, method, &[], &kw, None)
            .await
            .expect_err("unregistered migrated method must fail with UnimplementedRPC");
        assert_eq!(err.code, "UnimplementedRPC"); // rpc-lint: allow MachoNet error code assertion
    }
}

#[tokio::test]
async fn track_2c_structure_rpcs_run_lazy_lifecycle_sync_prehook() {
    use crate::services::station_structure::structure_upwell_ops::{load_ext_state, save_ext_state};

    let ctx = ServerContext::with_repository(Arc::new(eve_db::WorldRepository::with_demo_seed()));
    let now = ctx.now_unix();
    let char_id = 90_000_001_i64;
    let corp_id = ctx
        .db
        .with_snapshot(|snap| snap.characters.get(&char_id).map(|c| c.corporation_id))
        .unwrap_or(98_000_001);
    let mut sess = SessionState::new(98_402);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(corp_id);
    sess.solar_system_id = Some(30_000_142);
    sess.role = ServerContext::ROLE_GM_CANONICAL_MASK;
    let kw = HashMap::new();

    let make_structure = |structure_id: i64, state: &str, reinforced_until_unix: u64| {
        eve_db::UpwellStructureRecord {
            structure_id,
            type_id: 35_832,
            owner_corp_id: corp_id,
            solar_system_id: 30_000_142,
            name: format!("Sync Test {structure_id}"),
            position: [10_000.0, 0.0, 0.0],
            state: state.into(),
            quantum_core_installed: true,
            fuel_blocks: 500,
            shield_hp: 1.0,
            armor_hp: 1.0,
            hull_hp: 1.0,
            reinforced_until_unix,
            last_damage_tick: 0,
            last_damage_unix: 0,
            self_destruct_at_unix: None,
        }
    };

    // 1) structure_profiles_directory: expired Onlining timer transitions via GetMyCharacterStructures
    let s_dir_id = 9_900_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures
            .insert(s_dir_id, make_structure(s_dir_id, "Onlining", now.saturating_sub(10)));
    });
    assert!(ctx
        .dispatch_rpc(&mut sess, "structureDirectory", None, "GetMyCharacterStructures", &[], &kw, None)
        .await
        .is_ok());
    let s_dir_state = ctx
        .db
        .with_snapshot(|snap| snap.structures.get(&s_dir_id).map(|s| s.state.clone()));
    assert_eq!(s_dir_state.as_deref(), Some("ShieldVulnerable"));

    // 2) structure_profiles_rpc: expired Onlining timer transitions via structureProfiles.GetProfiles
    let s_prof_id = 9_900_002_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures
            .insert(s_prof_id, make_structure(s_prof_id, "Onlining", now.saturating_sub(10)));
    });
    assert!(ctx
        .dispatch_rpc(&mut sess, "structureProfiles", None, "GetProfiles", &[], &kw, None)
        .await
        .is_ok());
    let s_prof_state = ctx
        .db
        .with_snapshot(|snap| snap.structures.get(&s_prof_id).map(|s| s.state.clone()));
    assert_eq!(s_prof_state.as_deref(), Some("ShieldVulnerable"));

    // 3) structure_upwell_ops: elapsed unanchor timer packages structure before CancelUnanchor runs
    let s_unanchor_id = 9_900_003_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures
            .insert(s_unanchor_id, make_structure(s_unanchor_id, "ShieldVulnerable", 0));
        let mut ext = load_ext_state(snap);
        ext.unanchoring_until_unix
            .insert(s_unanchor_id, now.saturating_sub(10));
        save_ext_state(snap, &ext);
    });
    let cancel_err = ctx
        .dispatch_rpc(
            &mut sess,
            "structureDeployment",
            None,
            "CancelUnanchor",
            &[PyRep::Integer(s_unanchor_id)],
            &kw,
            None,
        )
        .await
        .expect_err("elapsed unanchor timer must package structure before CancelUnanchor runs");
    assert_eq!(cancel_err.code, "StructureNotFound");
    let remaining = ctx
        .db
        .with_snapshot(|snap| snap.structures.get(&s_unanchor_id).cloned());
    assert!(
        remaining.is_none(),
        "elapsed unanchor timer must have removed structure from space"
    );
}

#[tokio::test]
async fn track_2c_abyssal_in_pocket_guards_and_macho_net_metrics_preserved() {
    let ctx = ServerContext::with_repository(Arc::new(eve_db::WorldRepository::with_demo_seed()));
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap
            .characters
            .get_mut(&90_000_001)
            .expect("demo character 90_000_001 exists");
        ch.solar_system_id = 32_000_001;
        ch.station_id = None;
        ch.structure_id = None;
    });
    let mut abyss_sess = SessionState::new(98_403);
    abyss_sess.user_id = Some(1);
    abyss_sess.character_id = Some(90_000_001);
    abyss_sess.solar_system_id = Some(32_000_001);
    let kw = HashMap::new();

    let xmpp_err = ctx
        .dispatch_rpc(
            &mut abyss_sess,
            "XmppChatMgr",
            None,
            "AddCharacterToChannel",
            &[PyRep::String("solarsystemid2:32000001".into())],
            &kw,
            None,
        )
        .await
        .expect_err("abyssal guard must block XmppChatMgr.AddCharacterToChannel on local");
    assert_eq!(xmpp_err.code, "LocalChannelDisabledInAbyssalSpace");

    let sov_err = ctx
        .dispatch_rpc(&mut abyss_sess, "map", None, "GetCurrentSovData", &[], &kw, None)
        .await
        .expect_err("abyssal guard must block map.GetCurrentSovData");
    assert_eq!(sov_err.code, "MapDisabledInAbyssalSpace");

    let dscan_err = ctx
        .dispatch_rpc(&mut abyss_sess, "scanSvc", None, "DirectionalScan", &[], &kw, None)
        .await
        .expect_err("abyssal guard must block scanSvc.DirectionalScan");
    assert_eq!(dscan_err.code, "CannotDscanInThisSystem");

    let before_calls = ctx
        .cluster_state
        .read()
        .expect("cluster_state rwlock")
        .calls_received
        .get("machoNet")
        .copied()
        .unwrap_or(0);
    assert!(ctx
        .dispatch_rpc(
            &mut abyss_sess,
            "machoNet",
            None,
            "GetClusterGameStatisticsForClient",
            &[],
            &kw,
            None,
        )
        .await
        .is_ok());
    let after_calls = ctx
        .cluster_state
        .read()
        .expect("cluster_state rwlock")
        .calls_received
        .get("machoNet")
        .copied()
        .unwrap_or(0);
    assert_eq!(after_calls, before_calls + 1);
}

#[test]
fn track_2d_registry_registers_all_modules_without_duplicates() {
    let files: std::collections::HashSet<&str> =
        COMPILED_RPC_MODULES.iter().map(|m| m.file).collect();
    let expected_track_2d = [
        ("services/cluster_macho_misc_services.rs", 2usize),
        ("services/macho_cluster_mgmt.rs", 30),
        ("services/serverops_cluster.rs", 12),
        ("services/market_trading.rs", 11),
        ("services/wallet_ledger.rs", 4),
        ("services/agents_missions.rs", 20),
        ("services/structure_acl_defense_rules.rs", 8),
        ("services/station_structure.rs", 21),
        ("services/social_corp_fleet.rs", 58),
        ("services/social_wars.rs", 62),
        ("services/fleet_social_corp_gov.rs", 55),
        ("services/fleet_social_corp_tail_dispatch.rs", 23),
        ("services/fleet_social.rs", 12),
        ("services/fleet_social_chat.rs", 46),
        ("services/character_lifecycle.rs", 16),
    ];
    let mut track_2d_total = 0usize;
    for (file, expected_len) in expected_track_2d {
        assert!(files.contains(file), "missing Track 2D module {file}");
        let mod_entry = COMPILED_RPC_MODULES
            .iter()
            .find(|m| m.file == file)
            .expect("module exists");
        assert_eq!(
            mod_entry.rpcs.len(),
            expected_len,
            "unexpected RPC count for {file}"
        );
        track_2d_total += expected_len;
    }
    assert_eq!(track_2d_total, 380);

    let registry =
        RpcRegistry::try_from_modules(COMPILED_RPC_MODULES).expect("zero duplicate entries");
    assert_eq!(COMPILED_RPC_MODULES.len(), 87);
    assert!(
        registry.all_entries().len() >= 1475,
        "expected Track 2A..2D entries >= 1475, got {}",
        registry.all_entries().len()
    );

    let unique_pairs: BTreeSet<(&str, &str)> = registry
        .all_entries()
        .iter()
        .map(|entry| (entry.service, entry.method))
        .collect();
    assert_eq!(unique_pairs.len(), registry.all_entries().len());

    for (svc, method) in [
        ("charMgr", "SaveCharacterSetting"),
        ("machoNet", "GetGlobalConfig"),
        ("cluster", "SetServiceState"),
        ("marketProxy", "GetMarketGroups"),
        ("account", "GetTransactions"),
        ("invbroker", "DepositPlexToVault"),
        ("agentMgr", "GetAgents"),
        ("ownerGroupManager", "GetMyGroups"),
        ("stationSvc", "GetAllianceSystems"),
        ("posMgr", "AnchorOrbital"),
        ("posMgr", "UnanchorOrbital"),
        ("fleetSvc", "InitFleet"),
        ("allianceRegistry", "CreateAlliance"),
        ("corpRegistry", "DeclareWarAgainst"),
        ("warRegistry", "DeclareWarAgainst"),
        ("corpRegistry", "MoveCompanyShares"),
        ("corpRegistry", "GetPendingAutoKicks"),
        ("officeManager", "GetPriceQuote"),
        ("fleetObjectHandler", "MassMoveMembers"),
        ("chatAuthenticationService", "GetAuthenticationToken"),
        ("XmppChatMgr", "AddCharacterToChannel"),
        ("LSC", "GetChannels"),
        ("charMgr", "GetCharacterCreationDate"),
        ("charUnboundMgr", "GetNumCharacters"),
    ] {
        assert!(
            registry.lookup(svc, method).is_some(),
            "missing Track 2D registry lookup for {svc}.{method}"
        );
    }

    // Omitted, shadowed, and conditional-fallthrough methods must remain outside COMPILED_RPC_MODULES.
    for (svc, method) in [
        ("stationSvc", "Dock"),
        ("structureDocking", "Dock"),
        ("account", "GiveCashFromCorpAccount"),
        ("beyonce", "GetFormations"),
        ("fleetObjectHandler", "MakeLeader"),
        ("fleetMgr", "MakeLeader"),
        ("charUnboundMgr", "CreateCharacterWithDoll"),
        ("charUnboundMgr", "SelectCharacterID"),
        ("corpmgr", "allianceID"),
        ("corpRegistry", "UpdateLogo"),
    ] {
        assert!(
            registry.lookup(svc, method).is_none(),
            "expected {svc}.{method} to stay outside compiled registry"
        );
    }
}

#[tokio::test]
async fn track_2d_dispatch_routes_and_preserves_hooks_and_rejects_unregistered() {
    let ctx = ServerContext::with_repository(Arc::new(eve_db::WorldRepository::with_demo_seed()));
    let corp_id = ctx
        .db
        .with_snapshot(|snap| snap.characters.get(&90_000_001).map(|c| c.corporation_id))
        .unwrap_or(98_000_001);
    let mut sess = SessionState::new(98_501);
    sess.user_id = Some(1);
    sess.character_id = Some(90_000_001);
    sess.corporation_id = Some(corp_id);
    sess.station_id = Some(60_003_760);
    sess.solar_system_id = Some(30_000_142);
    sess.role = ServerContext::ROLE_GM_CANONICAL_MASK;
    let kw = HashMap::new();

    // Cluster call-counter pre-hook preserved on both macho_cluster_mgmt and serverops_cluster.
    let before_macho = ctx
        .cluster_state
        .read()
        .expect("cluster_state rwlock")
        .calls_received
        .get("machoNet")
        .copied()
        .unwrap_or(0);
    let before_cluster = ctx
        .cluster_state
        .read()
        .expect("cluster_state rwlock")
        .calls_received
        .get("cluster")
        .copied()
        .unwrap_or(0);
    assert!(ctx
        .dispatch_rpc(&mut sess, "machoNet", None, "GetGlobalConfig", &[], &kw, None)
        .await
        .is_ok());
    assert!(ctx
        .dispatch_rpc(&mut sess, "cluster", None, "RebalanceNodes", &[], &kw, None)
        .await
        .is_ok());

    let mut player_sess = SessionState::new(98_502);
    player_sess.user_id = Some(1);
    player_sess.character_id = Some(90_000_001);
    player_sess.corporation_id = Some(corp_id);
    player_sess.station_id = Some(60_003_760);
    player_sess.solar_system_id = Some(30_000_142);
    let admin_err = ctx
        .dispatch_rpc(&mut player_sess, "cluster", None, "RebalanceNodes", &[], &kw, None)
        .await
        .expect_err("non-GM caller must fail serverops_cluster.RebalanceNodes");
    assert_eq!(admin_err.code, "InsufficientRole");

    let (after_macho, after_cluster) = {
        let cs = ctx.cluster_state.read().expect("cluster_state rwlock");
        (
            cs.calls_received.get("machoNet").copied().unwrap_or(0),
            cs.calls_received.get("cluster").copied().unwrap_or(0),
        )
    };
    assert_eq!(after_macho, before_macho + 1);
    assert_eq!(after_cluster, before_cluster + 2);

    // Sync, Async, and BoundAsync Track 2D module routing.
    assert!(ctx
        .dispatch_rpc(
            &mut sess,
            "charMgr",
            None,
            "SaveCharacterSetting",
            &[PyRep::String("t2d_test_key".into()), PyRep::Integer(1)],
            &kw,
            None,
        )
        .await
        .is_ok());
    assert!(ctx
        .dispatch_rpc(
            &mut sess,
            "charMgr",
            None,
            "DeleteCharacterSetting",
            &[PyRep::String("t2d_test_key".into())],
            &kw,
            None,
        )
        .await
        .is_ok());
    assert!(ctx
        .dispatch_rpc(&mut sess, "marketProxy", None, "GetMarketGroups", &[], &kw, None)
        .await
        .is_ok());
    assert!(ctx
        .dispatch_rpc(&mut sess, "account", None, "GetEntryTypes", &[], &kw, None)
        .await
        .is_ok());
    assert!(ctx
        .dispatch_rpc(&mut sess, "agentMgr", None, "GetAgents", &[], &kw, None)
        .await
        .is_ok());
    assert!(ctx
        .dispatch_rpc(&mut sess, "ownerGroupManager", None, "GetMyGroups", &[], &kw, None)
        .await
        .is_ok());
    assert!(ctx
        .dispatch_rpc(&mut sess, "stationSvc", None, "GetAllianceSystems", &[], &kw, None)
        .await
        .is_ok());
    assert!(ctx
        .dispatch_rpc(&mut sess, "fleetSvc", None, "InitFleet", &[], &kw, None)
        .await
        .is_ok());
    assert!(ctx
        .dispatch_rpc(&mut sess, "warRegistry", None, "GetWars", &[], &kw, None)
        .await
        .is_ok());
    assert!(ctx
        .dispatch_rpc(
            &mut sess,
            "corpRecProxy",
            None,
            "GetRecruitmentAdsByCorpID",
            &[PyRep::Integer(corp_id)],
            &kw,
            None,
        )
        .await
        .is_ok());
    assert!(ctx
        .dispatch_rpc(&mut sess, "corpmgr", None, "GetDivisionNames", &[], &kw, None)
        .await
        .is_ok());
    assert!(ctx
        .dispatch_rpc(
            &mut sess,
            "officeManager",
            None,
            "GetEmptyOfficeCount",
            &[PyRep::Integer(60_003_760)],
            &kw,
            None,
        )
        .await
        .is_ok());
    assert!(ctx
        .dispatch_rpc(&mut sess, "LSC", None, "GetChannels", &[], &kw, None)
        .await
        .is_ok());
    assert!(ctx
        .dispatch_rpc(&mut sess, "charMgr", None, "GetCharacterCreationDate", &[], &kw, None)
        .await
        .is_ok());
    assert!(ctx
        .dispatch_rpc(&mut sess, "charUnboundMgr", None, "GetNumCharacters", &[], &kw, None)
        .await
        .is_ok());

    // Preserved station_structure::sync_all_structures pre-hook for structure* services.
    let now = ctx.now_unix();
    let structure_id = 9_900_201_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            structure_id,
            eve_db::UpwellStructureRecord {
                structure_id,
                type_id: 35_832,
                owner_corp_id: corp_id,
                solar_system_id: 30_000_142,
                name: format!("Track 2D Sync {structure_id}"),
                position: [12_000.0, 0.0, 0.0],
                state: "Onlining".into(),
                quantum_core_installed: true,
                fuel_blocks: 500,
                shield_hp: 1.0,
                armor_hp: 1.0,
                hull_hp: 1.0,
                reinforced_until_unix: now.saturating_sub(10),
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
    });
    let jettison_err = ctx
        .dispatch_rpc(
            &mut sess,
            "structure",
            None,
            "JettisonStructureFuel",
            &[],
            &kw,
            None,
        )
        .await
        .expect_err("JettisonStructureFuel without structureID fails after sync");
    assert_eq!(jettison_err.code, "MissingStructureId");
    let transitioned_state = ctx
        .db
        .with_snapshot(|snap| snap.structures.get(&structure_id).map(|s| s.state.clone()));
    assert_eq!(transitioned_state.as_deref(), Some("ShieldVulnerable"));

    // Preserved abyssal pocket restriction inside fleet_social_chat::dispatch_rpc.
    let prev_char_loc = ctx.db.with_mut_snapshot(|snap| {
        let ch = snap
            .characters
            .get_mut(&90_000_001)
            .expect("demo character 90_000_001 exists");
        let prev = (ch.solar_system_id, ch.station_id, ch.structure_id);
        ch.solar_system_id = 32_000_001;
        ch.station_id = None;
        ch.structure_id = None;
        prev
    });
    let mut abyss_sess = SessionState::new(98_503);
    abyss_sess.user_id = Some(1);
    abyss_sess.character_id = Some(90_000_001);
    abyss_sess.solar_system_id = Some(32_000_001);
    let chat_abyss_err = ctx
        .dispatch_rpc(
            &mut abyss_sess,
            "LSC",
            None,
            "JoinChannel",
            &[PyRep::String("solarsystemid2:32000001".into())],
            &kw,
            None,
        )
        .await
        .expect_err("abyssal guard must block LSC.JoinChannel on local");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            (ch.solar_system_id, ch.station_id, ch.structure_id) = prev_char_loc;
        }
    });
    assert_eq!(chat_abyss_err.code, "LocalChannelDisabledInAbyssalSpace");

    // Unregistered methods on newly migrated services must fail with UnimplementedRPC.
    for (svc, method) in [
        ("machoNet", "UnregisteredMachoNetMethod"), // rpc-lint: allow negative dispatch test fixture
        ("cluster", "UnregisteredClusterMethod"), // rpc-lint: allow negative dispatch test fixture
        ("marketProxy", "UnregisteredMarketMethod"), // rpc-lint: allow negative dispatch test fixture
        ("agentMgr", "UnregisteredAgentMethod"), // rpc-lint: allow negative dispatch test fixture
        ("ownerGroupManager", "UnregisteredAclGroupMethod"), // rpc-lint: allow negative dispatch test fixture
        ("warRegistry", "UnregisteredWarMethod"), // rpc-lint: allow negative dispatch test fixture
        ("officeManager", "UnregisteredOfficeMethod"), // rpc-lint: allow negative dispatch test fixture
        ("LSC", "UnregisteredLscMethod"), // rpc-lint: allow negative dispatch test fixture
        ("XmppChatMgr", "UnregisteredXmppMethod"), // rpc-lint: allow negative dispatch test fixture
        ("charMgr", "UnregisteredCharMgrMethod"), // rpc-lint: allow negative dispatch test fixture
        ("charUnboundMgr", "UnregisteredCharUnboundMethod"), // rpc-lint: allow negative dispatch test fixture
    ] {
        let err = ctx
            .dispatch_rpc(&mut sess, svc, None, method, &[], &kw, None)
            .await
            .expect_err("unregistered migrated method must fail with UnimplementedRPC");
        assert_eq!(err.code, "UnimplementedRPC"); // rpc-lint: allow MachoNet error code assertion
    }
}

#[tokio::test]
async fn track_2d_account_services_does_not_shadow_character_lifecycle_rpcs_and_preserves_guards() {
    let ctx = ServerContext::with_repository(Arc::new(eve_db::WorldRepository::with_demo_seed()));
    let kw = HashMap::new();

    let mut unauth_sess = SessionState::new(98_510);
    let mut auth_sess = SessionState::new(98_511);
    auth_sess.user_id = Some(98_511);
    auth_sess.character_id = Some(90_000_001);

    assert_eq!(crate::services::character_lifecycle::RPCS.len(), 16);
    for entry in crate::services::character_lifecycle::RPCS {
        assert!(
            ctx.rpc_registry.lookup(entry.service, entry.method).is_some(),
            "expected {}.{} in compiled RpcRegistry",
            entry.service,
            entry.method
        );
        assert!(
            crate::services::account_services::handle(
                &ctx,
                &mut unauth_sess,
                entry.service,
                entry.method,
                &[],
                &kw,
            )
            .await
            .is_none(),
            "account_services::handle must not shadow {}.{} when unauthenticated",
            entry.service,
            entry.method
        );
        assert!(
            crate::services::account_services::handle(
                &ctx,
                &mut auth_sess,
                entry.service,
                entry.method,
                &[],
                &kw,
            )
            .await
            .is_none(),
            "account_services::handle must not shadow {}.{} when authenticated",
            entry.service,
            entry.method
        );
    }

    // Unauthenticated charUnboundMgr calls reach compiled RpcRegistry -> character_lifecycle.
    let unauth_err = ctx
        .dispatch_rpc(
            &mut unauth_sess,
            "charUnboundMgr",
            None,
            "GetNumCharacters",
            &[],
            &kw,
            None,
        )
        .await
        .expect_err("unauthenticated GetNumCharacters must fail via compiled RpcRegistry");
    assert_eq!(unauth_err.code, "Unauthorized");

    // Populate MAX_CHARACTERS_PER_ACCOUNT (3) slots for uid 98_511 so CreateCharacterWithDoll
    // hits the slot-limit guard inside account_services::handle, and verify locked / pending-delete
    // SelectCharacterID guards remain enforced.
    let base_char = ctx
        .db
        .with_snapshot(|snap| snap.characters.get(&90_000_001).cloned())
        .expect("demo character 90_000_001 exists");
    let locked_cid = 98_511_001_i64;
    let deleting_cid = 98_511_002_i64;
    let third_cid = 98_511_003_i64;
    ctx.db.with_mut_snapshot(|snap| {
        for cid in [locked_cid, deleting_cid, third_cid] {
            let mut ch = base_char.clone();
            ch.character_id = cid;
            ch.user_id = 98_511;
            snap.characters.insert(cid, ch);
        }
        snap.cluster_and_runtime
            .character_locks
            .insert(locked_cid, "gm_lock".into());
        let mut lc = crate::services::character_lifecycle::load_store(snap);
        lc.delete_prepared_unix.insert(deleting_cid, 1_700_000_000);
        crate::services::character_lifecycle::save_store(snap, &lc);
    });

    let slot_err = crate::services::account_services::handle(
        &ctx,
        &mut auth_sess,
        "charUnboundMgr",
        "CreateCharacterWithDoll",
        &[PyRep::String("ValidPilot".into())],
        &kw,
    )
    .await
    .expect("slot-limit guard must intercept CreateCharacterWithDoll")
    .expect_err("CreateCharacterWithDoll at MAX_CHARACTERS_PER_ACCOUNT must error");
    assert_eq!(slot_err.code, "MaxCharacterSlotsReached");

    let locked_err = crate::services::account_services::handle(
        &ctx,
        &mut auth_sess,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(locked_cid)],
        &kw,
    )
    .await
    .expect("lock guard must intercept SelectCharacterID")
    .expect_err("locked character must error");
    assert_eq!(locked_err.code, "CharacterLocked");

    let mut kw_select = HashMap::new();
    kw_select.insert("charID".into(), PyRep::Integer(deleting_cid));
    let deleting_err = crate::services::account_services::handle(
        &ctx,
        &mut auth_sess,
        "charUnboundMgr",
        "SelectCharacterID",
        &[],
        &kw_select,
    )
    .await
    .expect("pending-deletion guard must intercept SelectCharacterID")
    .expect_err("pending-deletion character must error");
    assert_eq!(deleting_err.code, "CharacterLocked");
}
