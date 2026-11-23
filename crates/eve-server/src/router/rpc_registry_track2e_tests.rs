use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::Arc;

use eve_machonet::SessionState;
use eve_proto::PyRep;

use super::*;

#[test]
fn track_2e_registry_registers_all_modules_without_duplicates() {
    let files: HashSet<&str> = COMPILED_RPC_MODULES.iter().map(|m| m.file).collect();
    let expected_track_2e = [
        ("services/client_login.rs", 28usize),
        ("services/inventory_bound.rs", 7),
        ("services/character_skills.rs", 14),
        ("services/character_gm_tools.rs", 5),
        ("services/pve_special_sites.rs", 6),
        ("services/industry_pi.rs", 9),
        ("services/industry_jobs.rs", 1),
        ("services/industry_jobs_lifecycle.rs", 3),
        ("services/fitting_conduit_jump.rs", 10),
        ("services/wallet_services_ext.rs", 17),
        ("services/fitting_mining_lasers.rs", 8),
        ("services/fitting_scanners_superweapons.rs", 12),
        ("services/agents_missions_rpc.rs", 20),
        ("services/inventory_fitting_rpc.rs", 27),
        ("services/fleet_social_corps.rs", 47),
        ("services/fleet_social_scanning.rs", 4),
        ("services/sov_fw_insurgency.rs", 6),
        ("services/sov_services.rs", 10),
        ("services/fw_services.rs", 7),
        ("services/fw_insurgency.rs", 4),
    ];

    let mut track_2e_total = 0usize;
    for (file, expected_len) in expected_track_2e {
        assert!(files.contains(file), "missing Track 2E module {file}");
        let mod_entry = COMPILED_RPC_MODULES
            .iter()
            .find(|m| m.file == file)
            .expect("module exists");
        assert_eq!(
            mod_entry.rpcs.len(),
            expected_len,
            "unexpected RPC count for {file}"
        );
        track_2e_total += expected_len;
    }
    assert_eq!(track_2e_total, 245);

    let clone_entry = COMPILED_RPC_MODULES
        .iter()
        .find(|m| m.file == "services/character_clones_implants.rs")
        .expect("character_clones_implants module exists");
    assert_eq!(clone_entry.rpcs.len(), 23);

    let registry =
        RpcRegistry::try_from_modules(COMPILED_RPC_MODULES).expect("zero duplicate entries");
    assert_eq!(COMPILED_RPC_MODULES.len(), 107);
    assert_eq!(registry.all_entries().len(), 1721);

    let unique_pairs: BTreeSet<(&str, &str)> = registry
        .all_entries()
        .iter()
        .map(|entry| (entry.service, entry.method))
        .collect();
    assert_eq!(unique_pairs.len(), registry.all_entries().len());

    for (svc, method) in [
        ("operationsManager", "can_character_play_the_tutorial"),
        ("publicQaToolsServer", "SlashCmd"),
        ("charUnboundMgr", "CreateCharacterWithDoll"),
        ("jumpCloneSvc", "GetShipCloneState"),
        ("skillMgr2", "GetRespecInfo"),
        ("skillMgr2", "AbortTraining"),
        ("map", "GetAllRoamingWeatherSystems"),
        ("corpmgr", "allianceID"),
        ("corpRegistry", "UpdateLogo"),
        ("corpmgr", "createcorporation"),
        ("corpmgr", "getwalletdivisions"),
        ("industryManager", "InstallJob"),
        ("industryManager", "CancelJob"),
        ("industryManager", "CompleteManyJobs"),
        ("invbroker", "GetAllItems"),
        ("invbroker", "List"),
        ("trademgr", "InitiateTrade"),
        ("i2", "IsPrimed"), // rpc-lint: allow server-registered internal bound inventory dispatch endpoint
        ("planetMgr", "GetPlanetInfo"),
        ("agentMgr", "GetMyEpicArcStatus"),
        ("sovMgr", "SetSovHubFuelAccessGroup"),
        ("fwWarzoneSolarsystem", "GetAllWarzones"),
        ("scanMgr", "SignalTrackerRegister"),
        ("scanMgr", "GetScanResults"),
        ("scanSvc", "ConeScan"),
        ("map", "GetCurrentSovData"),
        ("superWeaponMgr", "ActivateSlashModule"),
        ("beyonce", "GetCynoJammerState"),
    ] {
        assert!(
            registry.lookup(svc, method).is_some(),
            "missing Track 2E registry lookup for {svc}.{method}"
        );
    }

    // Intentional exclusions, hooks, and fallback aliases must stay outside COMPILED_RPC_MODULES.
    for (svc, method) in [
        ("slash", "SlashCmd"),
        ("charUnboundMgr", "SelectCharacterID"),
        ("skillMgr2", "GetSkills"),
        ("stationSvc", "Dock"),
        ("structureDocking", "Dock"),
        ("dungeonInstanceCacheMgr", "GetSignatureInstances"),
        ("dungeonInstanceCacheMgr", "GetHomefrontSiteInstances"),
        ("dungeonInstanceCacheMgr", "GetFactionWarfareBattlefieldInstances"),
        ("agentMgr", "GetMessagesForEpicArcMissions"),
        ("beyonce", "GetFormations"),
        ("fleetObjectHandler", "MakeLeader"),
        ("fleetMgr", "MakeLeader"),
        ("fleetObjectHandler", "CreateFleet"),
        ("wormholeMgr", "GetWormholePolarization"),
        ("keeper", "Bind"),
        ("scanMgr", "ConeScan"),
        ("scanMgr", "RequestScans"),
        ("account", "GiveCashFromCorpAccount"),
        ("corpmgr", "CreateCorporation"), // rpc-lint: allow legacy PascalCase fallback alias check
        ("corpmgr", "GetWalletDivisions"), // rpc-lint: allow legacy PascalCase fallback alias check
        ("userSvc", "GetRedeemTokens"),
        ("miningScanMgr", "perform_scan"),
    ] {
        assert!(
            registry.lookup(svc, method).is_none(),
            "expected {svc}.{method} to stay outside compiled registry"
        );
    }
}

#[tokio::test]
async fn track_2e_dispatch_routes_and_preserves_hooks_and_rejects_unregistered() {
    let ctx = ServerContext::with_repository(Arc::new(eve_db::WorldRepository::with_demo_seed()));
    let mut sess = SessionState::new(98_601);
    sess.user_id = Some(1);
    sess.character_id = Some(90_000_001);
    sess.corporation_id = Some(98_000_001);
    sess.station_id = Some(60_003_760);
    sess.solar_system_id = Some(30_000_142);
    sess.role = ServerContext::ROLE_GM_CANONICAL_MASK;
    let kw = HashMap::new();

    // `skillMgr2.GetSkills` preserves raw Rowset vs `machoNet:26000` Dict conversion hook.
    let raw_skills = ctx
        .dispatch_rpc(&mut sess, "skillMgr2", None, "GetSkills", &[], &kw, None)
        .await
        .expect("skillMgr2.GetSkills succeeds");
    assert!(matches!(raw_skills, PyRep::Rowset { .. }));
    let dict_skills = ctx
        .dispatch_rpc(
            &mut sess,
            "skillMgr2",
            None,
            "GetSkills",
            &[],
            &kw,
            Some("machoNet:26000"),
        )
        .await
        .expect("skillMgr2.GetSkills with machoNet:26000 trace succeeds");
    assert!(matches!(dict_skills, PyRep::Dict(_)));

    // `charUnboundMgr.SelectCharacterID` fallback + secondary login hooks remain active.
    assert!(ctx
        .dispatch_rpc(
            &mut sess,
            "charUnboundMgr",
            None,
            "SelectCharacterID",
            &[PyRep::Integer(90_000_001)],
            &kw,
            None,
        )
        .await
        .is_ok());

    // `stationSvc.Dock` and `structureDocking.Dock` route to station_structure (not UnimplementedRPC).
    for svc in ["stationSvc", "structureDocking"] {
        let dock_err = ctx
            .dispatch_rpc(&mut sess, svc, None, "Dock", &[], &kw, None)
            .await
            .expect_err("Dock with no target must return domain error");
        assert_ne!(dock_err.code, "UnimplementedRPC"); // rpc-lint: allow MachoNet error code assertion
    }

    // `agentMgr.GetMessagesForEpicArcMissions` and `dungeonInstanceCacheMgr` fallbacks route cleanly.
    assert!(ctx
        .dispatch_rpc(
            &mut sess,
            "agentMgr",
            None,
            "GetMessagesForEpicArcMissions",
            &[],
            &kw,
            None,
        )
        .await
        .is_ok());
    assert!(ctx
        .dispatch_rpc(
            &mut sess,
            "dungeonInstanceCacheMgr",
            None,
            "GetSignatureInstances",
            &[],
            &kw,
            None,
        )
        .await
        .is_ok());
    assert!(ctx
        .dispatch_rpc(
            &mut sess,
            "dungeonInstanceCacheMgr",
            None,
            "GetHomefrontSiteInstances",
            &[],
            &kw,
            None,
        )
        .await
        .is_ok());

    // Fleet/corp fallbacks & PascalCase aliases work alongside compiled lowercase entries.
    let formations = ctx
        .dispatch_rpc(&mut sess, "beyonce", None, "GetFormations", &[], &kw, None)
        .await
        .expect("beyonce.GetFormations succeeds");
    assert!(matches!(formations, PyRep::Tuple(_)));

    for svc in ["fleetObjectHandler", "fleetMgr"] {
        let leader_err = ctx
            .dispatch_rpc(&mut sess, svc, None, "MakeLeader", &[], &kw, None)
            .await
            .expect_err("MakeLeader without fleet must return NotInFleet");
        assert_eq!(leader_err.code, "NotInFleet");
    }

    let create_fleet_res = ctx
        .dispatch_rpc(&mut sess, "fleetObjectHandler", None, "CreateFleet", &[], &kw, None)
        .await;
    if let Err(err) = create_fleet_res {
        assert_ne!(err.code, "UnimplementedRPC"); // rpc-lint: allow MachoNet error code assertion
    }

    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.entry(98_000_001).or_insert_with(|| {
            eve_db::CorporationRecord::new(98_000_001, "Track2E Test Corp", "T2ET", 90_000_001)
        });
    });
    let wdiv_pascal = ctx
        .dispatch_rpc(
            &mut sess,
            "corpmgr",
            None,
            "GetWalletDivisions", // rpc-lint: allow legacy PascalCase fallback alias check
            &[PyRep::Integer(98_000_001)],
            &kw,
            None,
        )
        .await
        .expect("corpmgr.GetWalletDivisions succeeds");
    assert!(matches!(wdiv_pascal, PyRep::Rowset { .. }));
    let wdiv_lower = ctx
        .dispatch_rpc(
            &mut sess,
            "corpmgr",
            None,
            "getwalletdivisions",
            &[PyRep::Integer(98_000_001)],
            &kw,
            None,
        )
        .await
        .expect("corpmgr.getwalletdivisions succeeds");
    assert!(matches!(wdiv_lower, PyRep::Rowset { .. }));
    let create_corp_err = ctx
        .dispatch_rpc(&mut sess, "corpmgr", None, "CreateCorporation", &[], &kw, None) // rpc-lint: allow legacy PascalCase fallback alias check
        .await
        .expect_err("CreateCorporation without args must return domain error");
    assert_ne!(create_corp_err.code, "UnimplementedRPC"); // rpc-lint: allow MachoNet error code assertion

    let give_cash_res = ctx
        .dispatch_rpc(
            &mut sess,
            "account",
            None,
            "GiveCashFromCorpAccount",
            &[PyRep::Integer(1), PyRep::Integer(90_000_002), PyRep::Real(1.0)],
            &kw,
            None,
        )
        .await;
    if let Err(err) = give_cash_res {
        assert_ne!(err.code, "UnimplementedRPC"); // rpc-lint: allow MachoNet error code assertion
    }

    // Scanning fallbacks and registry routes (`scanMgr.GetSignatures`) preserve routing.
    assert!(ctx
        .dispatch_rpc(&mut sess, "scanMgr", None, "GetSignatures", &[], &kw, None)
        .await
        .is_ok());
    for (svc, meth) in [
        ("wormholeMgr", "GetWormholePolarization"),
        ("keeper", "Bind"),
        ("scanMgr", "ConeScan"),
        ("scanMgr", "RequestScans"),
    ] {
        let res = ctx.dispatch_rpc(&mut sess, svc, None, meth, &[], &kw, None).await;
        if let Err(err) = res {
            assert_ne!(err.code, "UnimplementedRPC"); // rpc-lint: allow MachoNet error code assertion
        }
    }

    // Abyssal in-pocket restrictions remain enforced for migrated `fw_insurgency` and `sov_services`.
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.solar_system_id = 32_000_001;
            ch.station_id = None;
            ch.structure_id = None;
        }
    });
    let mut abyss_sess = SessionState::new(98_602);
    abyss_sess.user_id = Some(1);
    abyss_sess.character_id = Some(90_000_001);
    abyss_sess.solar_system_id = Some(32_000_001);

    let cone_err = ctx
        .dispatch_rpc(&mut abyss_sess, "scanSvc", None, "ConeScan", &[], &kw, None)
        .await
        .expect_err("abyssal guard must block scanSvc.ConeScan");
    assert_eq!(cone_err.code, "CannotDscanInThisSystem");
    let cone_mgr_err = ctx
        .dispatch_rpc(&mut abyss_sess, "scanMgr", None, "ConeScan", &[], &kw, None)
        .await
        .expect_err("abyssal guard must block scanMgr.ConeScan");
    assert_eq!(cone_mgr_err.code, "CannotDscanInThisSystem");
    let sov_err = ctx
        .dispatch_rpc(&mut abyss_sess, "map", None, "GetCurrentSovData", &[], &kw, None)
        .await
        .expect_err("abyssal guard must block map.GetCurrentSovData");
    assert_ne!(sov_err.code, "UnimplementedRPC"); // rpc-lint: allow MachoNet error code assertion

    // Unregistered methods on newly migrated Track 2E services fail with UnimplementedRPC.
    for (svc, method) in [
        ("planetMgr", "UnregisteredPlanetMethod"), // rpc-lint: allow negative dispatch test fixture
        ("sovMgr", "UnregisteredSovMethod"), // rpc-lint: allow negative dispatch test fixture
        ("fwWarzoneSolarsystem", "UnregisteredFwWarzoneMethod"), // rpc-lint: allow negative dispatch test fixture
        ("superWeaponMgr", "UnregisteredSuperWeaponMethod"), // rpc-lint: allow negative dispatch test fixture
        ("solarsystemInterferenceMgr", "UnregisteredInterferenceMethod"), // rpc-lint: allow negative dispatch test fixture
        ("industryMonitor", "UnregisteredIndustryMonitorMethod"), // rpc-lint: allow negative dispatch test fixture
        ("fleetProxy", "UnregisteredFleetProxyMethod"), // rpc-lint: allow negative dispatch test fixture
        ("corporationSvc", "UnregisteredCorpSvcMethod"), // rpc-lint: allow negative dispatch test fixture
    ] {
        let err = ctx
            .dispatch_rpc(&mut sess, svc, None, method, &[], &kw, None)
            .await
            .expect_err("unregistered migrated Track 2E method must fail with UnimplementedRPC");
        assert_eq!(err.code, "UnimplementedRPC"); // rpc-lint: allow MachoNet error code assertion
    }
}

#[tokio::test]
async fn track_2e_fw_battlefield_instances_preserves_require_character_and_lazy_spawn() {
    use crate::services::fw_services::{spawn_battlefield_site, FACTION_CALDARI, FACTION_GALLENTE};
    use eve_db::snapshot_ext::FwSystemOccupationRecord;

    let ctx = ServerContext::with_repository(Arc::new(eve_db::WorldRepository::with_demo_seed()));
    let (fw_sys, enemy_sys) = ctx
        .sde
        .jumps
        .values()
        .find(|j| {
            j.from_solar_system_id != j.to_solar_system_id
                && ctx.sde.solar_systems.contains_key(&j.from_solar_system_id)
        })
        .map(|j| (j.from_solar_system_id, j.to_solar_system_id))
        .expect("SDE contains at least one valid solar-system jump");

    ctx.db.with_mut_snapshot(|snap| {
        snap.fw_system_occupations.insert(
            fw_sys,
            FwSystemOccupationRecord {
                solar_system_id: fw_sys,
                owner_faction_id: FACTION_CALDARI,
                occupier_faction_id: FACTION_CALDARI,
                victory_points: 0,
                victory_points_threshold: 75_000,
            },
        );
        snap.fw_system_occupations.insert(
            enemy_sys,
            FwSystemOccupationRecord {
                solar_system_id: enemy_sys,
                owner_faction_id: FACTION_GALLENTE,
                occupier_faction_id: FACTION_GALLENTE,
                victory_points: 0,
                victory_points_threshold: 75_000,
            },
        );
    });
    spawn_battlefield_site(&ctx, enemy_sys, FACTION_GALLENTE, "Enemy Battlefield");

    let kw = HashMap::new();
    // Auth-only session (`character_id = None`) must fail require_character in pve_dungeon_cache
    // instead of leaking cluster-wide battlefield instances.
    let mut auth_only = SessionState::new(98_603);
    auth_only.user_id = Some(1);
    let no_char_err = ctx
        .dispatch_rpc(
            &mut auth_only,
            "dungeonInstanceCacheMgr",
            None,
            "GetFactionWarfareBattlefieldInstances",
            &[],
            &kw,
            None,
        )
        .await
        .expect_err("GetFactionWarfareBattlefieldInstances without selected character must fail");
    assert_eq!(no_char_err.code, "CharacterNotSelected");

    // Undocked character in an occupied Frontline FW system triggers lazy warzone spawning
    // via pve_dungeon_cache before fw_services filters and returns the spawned battlefield.
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.solar_system_id = fw_sys;
            ch.station_id = None;
            ch.structure_id = None;
        }
        snap.fw_runtime
            .fw_complexes
            .retain(|_, (sys, ..)| *sys != fw_sys);
        snap.fw_runtime
            .battlefield_instances
            .retain(|b| b.solar_system_id != fw_sys);
        snap.fw_runtime.battlefield_respawn_unix.remove(&fw_sys);
    });
    assert!(ctx.db.with_snapshot(|s| {
        s.fw_runtime
            .battlefield_instances
            .iter()
            .all(|b| b.solar_system_id != fw_sys)
    }));
    let mut undocked = SessionState::new(98_604);
    undocked.user_id = Some(1);
    undocked.character_id = Some(90_000_001);
    undocked.solar_system_id = Some(fw_sys);
    let res = ctx
        .dispatch_rpc(
            &mut undocked,
            "dungeonInstanceCacheMgr",
            None,
            "GetFactionWarfareBattlefieldInstances",
            &[],
            &kw,
            None,
        )
        .await
        .expect("undocked GetFactionWarfareBattlefieldInstances succeeds");
    let PyRep::List(entries) = res else {
        panic!("expected List from GetFactionWarfareBattlefieldInstances, got {res:?}");
    };
    assert_eq!(
        entries.len(),
        1,
        "lazy spawn must return the newly spawned frontline battlefield without enemy systems"
    );
    assert!(ctx.db.with_snapshot(|s| {
        s.fw_runtime
            .fw_complexes
            .values()
            .any(|(sys, ..)| *sys == fw_sys)
            && s.fw_runtime
                .battlefield_instances
                .iter()
                .any(|b| b.solar_system_id == fw_sys && !b.completed)
    }));
}
