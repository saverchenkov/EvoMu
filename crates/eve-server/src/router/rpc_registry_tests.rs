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
    assert_eq!(COMPILED_RPC_MODULES.len(), 44);
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
