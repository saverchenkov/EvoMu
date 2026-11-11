//! Unit tests for `sov_services` (ADM math, hub ACLs, hub hacks, sov claims, skyhook destruction, contract helpers).

#![forbid(unsafe_code)]

use super::*;
use crate::router::ServerContext;
use crate::services::station_structure::structure_upwell_ops::{
    load_ext_state, save_ext_state, AccessGroupRecord,
};
use eve_db::{
    ContractRecord, CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents,
    UpwellStructureRecord,
};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn test_ctx() -> ServerContext {
    if let Some(sde) = SHARED_SDE.get() {
        ServerContext::with_sde_and_repository(
            Arc::clone(sde),
            Arc::new(eve_db::WorldRepository::with_demo_seed()),
        )
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
    } else {
        let ctx = ServerContext::new();
        let _ = SHARED_SDE.set(Arc::clone(&ctx.sde));
        ctx
    }
}

fn sov_session(char_id: i64, corp_id: i64, sys_id: u32, station_id: Option<i64>) -> SessionState {
    let mut s = SessionState::new(9401);
    s.user_id = Some(1);
    s.character_id = Some(char_id);
    s.corporation_id = Some(corp_id);
    s.solar_system_id = Some(sys_id);
    s.station_id = station_id;
    s
}

#[test]
fn adm_calculation_entosis_scaling_and_hub_resolution() {
    assert_eq!(calculate_adm(0, 0, 0), 1.0);
    assert_eq!(calculate_adm(1, 0, 0), 1.6);
    assert_eq!(calculate_adm(5, 5, 5), 6.0);
    assert_eq!(calculate_adm(99, 99, 99), 6.0);
    assert_eq!(calculate_adm(3, 2, 1), 4.3);

    assert_eq!(scale_entosis_capture_seconds(600, 1.0), 600);
    assert_eq!(scale_entosis_capture_seconds(600, 0.2), 600);
    assert_eq!(scale_entosis_capture_seconds(600, 3.5), 2100);

    let ctx = test_ctx();
    let hub_id = 1_060_000_001_i64;
    let sys_id = 30_000_142_u32;

    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            hub_id,
            UpwellStructureRecord {
                structure_id: hub_id,
                type_id: TYPE_SOVEREIGNTY_HUB,
                owner_corp_id: 98_000_001,
                solar_system_id: sys_id,
                name: "Test Sov Hub".to_string(),
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".to_string(),
                quantum_core_installed: true,
                fuel_blocks: 500,
                ..Default::default()
            },
        );
        assert_eq!(resolve_sov_hub_id(snap, Some(hub_id), None), Some(hub_id));
        assert_eq!(
            resolve_sov_hub_id(snap, Some(i64::from(sys_id)), None),
            Some(hub_id)
        );
        assert_eq!(resolve_sov_hub_id(snap, None, Some(sys_id)), Some(hub_id));
        assert_eq!(resolve_sov_hub_id(snap, None, None), None);
    });
}

#[test]
fn can_access_sov_hub_fuel_acl_precedence_and_corp_roles() {
    let ctx = test_ctx();
    let hub_id = 1_060_000_010_i64;
    let owner_corp = 98_000_001_i64;
    let other_corp = 98_000_002_i64;
    let ally_id = 99_000_001_i64;
    let char_allowed = 90_000_001_i64;
    let char_blocked = 90_000_002_i64;
    let char_outsider = 90_000_003_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let corp = snap.corporations
            .entry(owner_corp)
            .or_insert_with(|| { let mut __corp = eve_db::CorporationRecord::default();
                __corp.corporation_id = owner_corp;
                __corp.ceo_id = 90_000_099;
                __corp.alliance_id = Some(ally_id);
                __corp.name = "OwnerCorp".into();
                __corp.ticker = "OWNC".into();
                __corp });
        corp.ceo_id = 90_000_099;
        corp.alliance_id = Some(ally_id);
        corp.member_roles.insert(char_allowed, 0);
        snap.structures.insert(
            hub_id,
            UpwellStructureRecord {
                structure_id: hub_id,
                type_id: TYPE_SOVEREIGNTY_HUB,
                owner_corp_id: owner_corp,
                solar_system_id: 30_000_142,
                name: "ACL Sov Hub".to_string(),
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".to_string(),
                ..Default::default()
            },
        );

        // No ACL configured: requires owning corp or alliance manager role
        assert!(!can_access_sov_hub_fuel(
            snap,
            hub_id,
            char_allowed,
            owner_corp,
            Some(ally_id),
            0,
            false
        ));
        assert!(can_access_sov_hub_fuel(
            snap,
            hub_id,
            char_allowed,
            owner_corp,
            Some(ally_id),
            eve_db::corp_roles::ROLE_DIRECTOR,
            false
        ));
        assert!(can_access_sov_hub_fuel(
            snap,
            hub_id,
            char_outsider,
            other_corp,
            Some(ally_id),
            eve_db::corp_roles::ROLE_DIRECTOR,
            true
        ));
        assert!(!can_access_sov_hub_fuel(
            snap,
            hub_id,
            char_outsider,
            other_corp,
            Some(ally_id),
            eve_db::corp_roles::ROLE_STATION_MANAGER,
            false
        ));

        // Configure ACL group 77: alliance allowed (+1), char_blocked explicitly blocked (EXCLUDED = 3)
        let mut ext = load_ext_state(snap);
        let mut members = HashMap::new();
        members.insert(ally_id, 1);
        members.insert(char_blocked, 3);
        ext.owner_groups.insert(
            77,
            AccessGroupRecord {
                group_id: 77,
                creator_id: char_allowed,
                owner_corp_id: owner_corp,
                name: "Fuel Access".to_string(),
                description: "Fuel ACL".to_string(),
                members,
            },
        );
        save_ext_state(snap, &ext);
        snap.sov_runtime.fuel_access_groups.insert(hub_id, 77);

        assert!(can_access_sov_hub_fuel(
            snap,
            hub_id,
            char_outsider,
            other_corp,
            Some(ally_id),
            0,
            false
        ));
        // Character-level block overrides alliance membership
        assert!(!can_access_sov_hub_fuel(
            snap,
            hub_id,
            char_blocked,
            owner_corp,
            Some(ally_id),
            eve_db::corp_roles::ROLE_DIRECTOR,
            false
        ));
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn record_hub_hack_claim_system_sovereignty_and_sov_mgr_rpcs() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let hub_id = 1_060_000_020_i64;
    let skyhook_id = 1_060_000_021_i64;
    let ally_id = 99_000_010_i64;

    let (corp_id, null_sys) = ctx.db.with_mut_snapshot(|snap| {
        let cid = snap.characters[&char_id].corporation_id;
        let corp = snap
            .corporations
            .entry(cid)
            .or_insert_with(|| { let mut __corp = eve_db::CorporationRecord::default();
                __corp.corporation_id = cid;
                __corp.ceo_id = char_id;
                __corp.alliance_id = Some(ally_id);
                __corp.name = "NullSovCorp".into();
                __corp.ticker = "NSOV".into();
                __corp });
        corp.ceo_id = char_id;
        corp.alliance_id = Some(ally_id);
        let nsys = ctx
            .sde
            .solar_systems
            .values()
            .find(|s| s.security_status < 0.0 && s.solar_system_id < 31_000_000)
            .map(|s| s.solar_system_id)
            .unwrap_or(30_000_142);
        snap.system_states.entry(nsys).or_default().solar_system_id = nsys;
        snap.structures.insert(
            hub_id,
            UpwellStructureRecord {
                structure_id: hub_id,
                type_id: TYPE_SOVEREIGNTY_HUB,
                owner_corp_id: cid,
                solar_system_id: nsys,
                name: "Null Sov Hub".to_string(),
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".to_string(),
                fuel_blocks: 100,
                ..Default::default()
            },
        );
        snap.structures.insert(
            skyhook_id,
            UpwellStructureRecord {
                structure_id: skyhook_id,
                type_id: TYPE_ORBITAL_SKYHOOK,
                owner_corp_id: cid,
                solar_system_id: nsys,
                name: "Null Skyhook".to_string(),
                position: [10_000.0, 0.0, 0.0],
                state: "ShieldVulnerable".to_string(),
                ..Default::default()
            },
        );
        (cid, nsys)
    });

    // Claim sovereignty for ally_id
    claim_system_sovereignty_async(&ctx, null_sys, ally_id, corp_id, hub_id)
        .await
        .unwrap();
    // Claiming by another alliance fails
    let conflict = claim_system_sovereignty(&ctx, null_sys, ally_id + 1, corp_id, hub_id).unwrap_err();
    assert_eq!(conflict.code, "SovereigntyHeldByAnotherAlliance");

    // Set & get fuel access group via sovMgr RPC
    let mut sess = sov_session(char_id, corp_id, null_sys, None);
    let kw = HashMap::new();
    let set_grp = handle(
        &ctx,
        &mut sess,
        "sovMgr",
        "SetSovHubFuelAccessGroup",
        &[PyRep::Integer(hub_id), PyRep::Integer(42)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(set_grp, PyRep::Integer(42));

    let get_grp = handle(
        &ctx,
        &mut sess,
        "sovMgr",
        "GetSovHubFuelAccessGroup",
        &[PyRep::Integer(hub_id)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(get_grp, PyRep::Integer(42));

    // Record hub hack & verify is_hub_hacked + ArmorReinforced state
    assert!(!is_hub_hacked(&ctx, hub_id));
    record_hub_hack(&ctx, null_sys, hub_id, 600);
    assert!(is_hub_hacked(&ctx, hub_id));
    ctx.db.with_snapshot(|s| {
        assert_eq!(s.structures[&hub_id].state, "ArmorReinforced");
    });

    // SetSkyhook hackable & reagents
    set_skyhook_reagents(&ctx, skyhook_id, TYPE_SUPERIONIC_ICE, 750);
    set_skyhook_hackable(&ctx, null_sys, skyhook_id, true);
    read_sov_runtime(&ctx, |rt| {
        assert_eq!(rt.skyhook_hackable.get(&skyhook_id), Some(&true));
        assert_eq!(
            rt.skyhook_reagents.get(&skyhook_id),
            Some(&(TYPE_SUPERIONIC_ICE, 750))
        );
    });

    // DestroySkyhooks removes corp-owned skyhooks in system
    let destroyed = handle(
        &ctx,
        &mut sess,
        "sovMgr",
        "DestroySkyhooks",
        &[PyRep::Integer(i64::from(null_sys))],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(destroyed, PyRep::Integer(1));
    ctx.db.with_snapshot(|s| {
        assert!(!s.structures.contains_key(&skyhook_id));
    });
}

#[test]
fn contract_notification_dismissal_bulk_delete_and_dockable_items() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let corp_id = ctx.db.with_snapshot(|s| s.characters[&char_id].corporation_id);
    let station_id = 60_003_760_i64;
    let mut sess = sov_session(char_id, corp_id, 30_000_142, Some(station_id));

    let (cid_del, cid_with_bid, item_in_hangar) = ctx.db.with_mut_snapshot(|s| {
        s.next_entity_id += 1;
        let iid1 = s.next_entity_id;
        s.items.insert(
            iid1,
            InventoryItemRecord {
                item_id: iid1,
                type_id: 34,
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Locked,
                quantity: 100,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        s.next_entity_id += 1;
        let iid_hangar = s.next_entity_id;
        s.items.insert(
            iid_hangar,
            InventoryItemRecord {
                item_id: iid_hangar,
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
        s.next_entity_id += 1;
        let c1 = s.next_entity_id;
        s.contracts.insert(
            c1,
            ContractRecord {
                contract_id: c1,
                issuer_id: char_id,
                contract_type: "ItemExchange".to_string(),
                status: "Outstanding".to_string(),
                start_station_id: station_id,
                end_station_id: station_id,
                reward: IskCents::from_isk(500),
                item_ids: vec![iid1],
                ..Default::default()
            },
        );
        s.next_entity_id += 1;
        let c2 = s.next_entity_id;
        s.contracts.insert(
            c2,
            ContractRecord {
                contract_id: c2,
                issuer_id: char_id,
                contract_type: "Auction".to_string(),
                status: "Outstanding".to_string(),
                start_station_id: station_id,
                end_station_id: station_id,
                current_bidder_id: Some(90_000_002),
                ..Default::default()
            },
        );
        (c1, c2, iid_hangar)
    });

    // DeleteContractNotification first time -> true, second time -> false
    assert_eq!(
        handle_contract_delete_notif(
            &ctx,
            &sess,
            "DeleteContractNotification",
            &[PyRep::Integer(cid_del), PyRep::Bool(false)]
        )
        .unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(
        handle_contract_delete_notif(
            &ctx,
            &sess,
            "DeleteContractNotification",
            &[PyRep::Integer(cid_del), PyRep::Bool(false)]
        )
        .unwrap(),
        PyRep::Bool(false)
    );

    // DeleteMultipleContracts: cid_del deleted, cid_with_bid (has active bid) failed
    let bulk = handle_contract_delete_multiple(
        &ctx,
        &sess,
        &[PyRep::List(vec![
            PyRep::Integer(cid_del),
            PyRep::Integer(cid_with_bid),
        ])],
    )
    .unwrap();
    if let PyRep::Tuple(parts) = bulk {
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0].row_count(), Some(1));
        assert_eq!(parts[1].row_count(), Some(1));
    } else {
        panic!("expected Tuple from DeleteMultipleContracts");
    }

    // GetItemsInDockableLocation excludes active ship_id
    sess.ship_id = Some(item_in_hangar);
    let dock_items = handle_contract_get_items_in_dockable(
        &ctx,
        &sess,
        &[PyRep::Integer(station_id), PyRep::Bool(false)],
    )
    .unwrap();
    if let PyRep::List(list) = dock_items {
        assert!(!list
            .iter()
            .any(|el| el.dict_get("itemID").and_then(PyRep::as_i64) == Some(item_in_hangar)));
    } else {
        panic!("expected List from GetItemsInDockableLocation");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sov_services_cyno_warp_clone_skyhook_guards_and_corp_contracts() {
    use eve_db::durable_domain_types::{PersistedHubUpgrade, PersistedMercenaryDen, PersistedSovHubData};

    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let other_char = 90_000_002_i64;
    let corp_id = ctx.db.with_snapshot(|s| s.characters[&char_id].corporation_id);
    let sys_id = 30_004_759_u32;
    let station_id = 60_003_760_i64;
    let hub_id = 1_088_000_001_i64;
    let skyhook_id = 1_088_000_002_i64;
    let den_id = 1_088_000_003_i64;
    let struct_inhib_id = 1_088_000_004_i64;
    let now = ctx.now_unix();
    let kw = HashMap::new();

    let (corp_item_cid, corp_item_locked, c_corp, c_missing) = ctx.db.with_mut_snapshot(|s| {
        let c = s.corporations.entry(corp_id).or_default();
        c.corporation_id = corp_id;
        c.ceo_id = char_id;
        c.members.insert(char_id);
        c.member_roles.insert(char_id, u64::MAX);
        s.structures.insert(
            hub_id,
            UpwellStructureRecord {
                structure_id: hub_id,
                type_id: TYPE_SOVEREIGNTY_HUB,
                owner_corp_id: corp_id,
                solar_system_id: sys_id,
                name: "Hack Hub".into(),
                state: "ShieldVulnerable".into(),
                ..Default::default()
            },
        );
        s.sov_runtime.sov_hubs.insert(
            hub_id,
            PersistedSovHubData {
                upgrades: vec![PersistedHubUpgrade {
                    type_id: 81_606,
                    online: true,
                    low_power: false,
                }],
                ..Default::default()
            },
        );
        s.structures.insert(
            skyhook_id,
            UpwellStructureRecord {
                structure_id: skyhook_id,
                type_id: TYPE_ORBITAL_SKYHOOK,
                owner_corp_id: corp_id,
                solar_system_id: sys_id,
                name: "Skyhook".into(),
                state: "Online".into(),
                shield_hp: 5_000.0,
                armor_hp: 5_000.0,
                hull_hp: 5_000.0,
                ..Default::default()
            },
        );
        s.sov_runtime.mercenary_dens.insert(
            den_id,
            PersistedMercenaryDen {
                den_id,
                owner_char_id: char_id,
                skyhook_id,
                solar_system_id: sys_id,
                planet_id: 40_000_001,
                type_id: crate::services::sov_gameplay::TYPE_MERCENARY_DEN,
                enabled: true,
                cargo_extraction_enabled: true,
                development_level: 0,
                anarchy_level: 0,
                simulated_at_unix: now,
                infomorphs: 0,
                next_activity_generation_unix: now,
            },
        );
        s.sov_runtime.skyhook_silo_links.insert(skyhook_id, (90_000_099, char_id, now + 300, 100.0));
        s.items.insert(
            777_001,
            InventoryItemRecord {
                item_id: 777_001,
                type_id: 33_476,
                owner_id: char_id,
                location_id: i64::from(sys_id),
                quantity: 1,
                ..Default::default()
            },
        );
        s.structures.insert(
            struct_inhib_id,
            UpwellStructureRecord {
                structure_id: struct_inhib_id,
                type_id: 81_926,
                owner_corp_id: corp_id,
                solar_system_id: sys_id,
                name: "Cyno Suppressor".into(),
                state: "Online".into(),
                fuel_blocks: 100,
                ..Default::default()
            },
        );

        s.next_entity_id += 1;
        let i_cid = s.next_entity_id;
        s.next_entity_id += 1;
        let i_lock = s.next_entity_id;
        s.next_entity_id += 1;
        let cc = s.next_entity_id;
        s.next_entity_id += 1;
        let cm = s.next_entity_id;
        s.items.insert(
            i_cid,
            InventoryItemRecord {
                item_id: i_cid,
                type_id: 34,
                owner_id: corp_id,
                location_id: cc,
                flag: InventoryFlag::Locked,
                quantity: 50,
                ..Default::default()
            },
        );
        s.items.insert(
            i_lock,
            InventoryItemRecord {
                item_id: i_lock,
                type_id: 34,
                owner_id: 0,
                location_id: station_id,
                flag: InventoryFlag::Locked,
                quantity: 25,
                ..Default::default()
            },
        );
        s.contracts.insert(
            cc,
            ContractRecord {
                contract_id: cc,
                issuer_id: other_char,
                issuer_corp_id: corp_id,
                for_corp: true,
                contract_type: "ItemExchange".into(),
                status: "Outstanding".into(),
                start_station_id: station_id,
                end_station_id: station_id,
                item_ids: vec![i_cid, i_lock],
                ..Default::default()
            },
        );
        (i_cid, i_lock, cc, cm)
    });

    // record_hub_hack moves hub to ArmorReinforced and lowers upgrade power
    record_hub_hack(&ctx, sys_id, hub_id, 600);
    ctx.db.with_snapshot(|s| {
        assert_eq!(s.structures[&hub_id].state, "ArmorReinforced");
        assert!(s.sov_runtime.sov_hubs[&hub_id].upgrades[0].low_power);
    });

    // beyonce.GetCynoJammerState detects both item inhibitor and suppressor structure
    let mut sess = sov_session(char_id, corp_id, sys_id, None);
    let cyno = handle(&ctx, &mut sess, "beyonce", "GetCynoJammerState", &[PyRep::Integer(i64::from(sys_id))], &kw)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(cyno.dict_get("isJammed"), Some(&PyRep::Bool(true)));
    assert_eq!(cyno.dict_get("structureInhibitor"), Some(&PyRep::Bool(true)));

    // handle_reset_last_clone_jump_time error without GM role, CharacterNotFound, then success with GM role
    assert_eq!(handle_reset_last_clone_jump_time(&ctx, &sess).unwrap_err().code, "GMRoleRequired");
    let mut gm_sess = sov_session(999_888_777, corp_id, sys_id, None);
    gm_sess.role = u64::MAX;
    assert_eq!(handle_reset_last_clone_jump_time(&ctx, &gm_sess).unwrap_err().code, "CharacterNotFound");
    sess.role = u64::MAX;
    assert_eq!(handle_reset_last_clone_jump_time(&ctx, &sess).unwrap(), PyRep::Integer(0));

    // handle_use_warp_vector in space (no structure control) delegates cleanly
    let _ = handle_use_warp_vector(&ctx, &mut sess, &[PyRep::Integer(1)]).await;

    // DestroySkyhooks guards: CorpRoleDenied -> ActiveReagentHeist -> StructureReinforcedOrDamaged -> success
    let mut unauth = sov_session(other_char, corp_id, sys_id, None);
    assert_eq!(
        handle(&ctx, &mut unauth, "sovMgr", "DestroySkyhooks", &[PyRep::Integer(i64::from(sys_id))], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "CorpRoleDenied"
    );
    assert_eq!(
        handle(&ctx, &mut sess, "sovMgr", "DestroySkyhooks", &[PyRep::Integer(i64::from(sys_id))], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "ActiveReagentHeist"
    );
    ctx.db.with_mut_snapshot(|s| {
        s.sov_runtime.skyhook_silo_links.remove(&skyhook_id);
        s.structures.get_mut(&skyhook_id).unwrap().state = "ArmorReinforced".into();
    });
    assert_eq!(
        handle(&ctx, &mut sess, "sovMgr", "DestroySkyhooks", &[PyRep::Integer(i64::from(sys_id))], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "StructureReinforcedOrDamaged"
    );
    ctx.db.with_mut_snapshot(|s| {
        s.structures.get_mut(&skyhook_id).unwrap().state = "Online".into();
    });
    let destroyed = handle(&ctx, &mut sess, "sovMgr", "DestroySkyhooks", &[PyRep::Integer(i64::from(sys_id))], &kw)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(destroyed, PyRep::Integer(1));
    ctx.db.with_snapshot(|s| {
        assert!(!s.structures.contains_key(&skyhook_id));
        assert!(!s.sov_runtime.mercenary_dens.contains_key(&den_id));
    });

    // handle_contract_delete_multiple for_corp restores escrow items to Deliveries & returns failed missing id
    sess.corp_role = eve_db::corp_roles::ROLE_CONTRACT_MANAGER;
    let del_res = handle_contract_delete_multiple(
        &ctx,
        &sess,
        &[PyRep::List(vec![PyRep::Integer(c_corp), PyRep::Integer(c_missing)])],
    )
    .unwrap();
    let PyRep::Tuple(parts) = del_res else { panic!("expected Tuple"); };
    assert_eq!(parts[0], PyRep::List(vec![PyRep::Integer(c_corp)]));
    assert_eq!(parts[1], PyRep::List(vec![PyRep::Integer(c_missing)]));
    ctx.db.with_snapshot(|s| {
        assert_eq!(s.items[&corp_item_cid].flag, InventoryFlag::Deliveries);
        assert_eq!(s.items[&corp_item_cid].owner_id, corp_id);
        assert_eq!(s.items[&corp_item_locked].flag, InventoryFlag::Deliveries);
        assert_eq!(s.items[&corp_item_locked].owner_id, corp_id);
    });

    // Direct checks for `corp_has_db_role_table` and `effective_contract_helper_corp_roles`
    let bare_corp = 98_999_901_i64;
    ctx.db.with_mut_snapshot(|s| {
        s.corporations.insert(bare_corp, {
            let mut c = CorporationRecord::default();
            c.corporation_id = bare_corp;
            c.ceo_id = char_id;
            c.name = "BareCorp".into();
            c.ticker = "BRC".into();
            c
        });
    });
    ctx.db.with_snapshot(|s| {
        assert!(!super::corp_has_db_role_table(s, 999_999_999));
        assert!(!super::corp_has_db_role_table(s, bare_corp));
        assert!(super::corp_has_db_role_table(s, corp_id));

        assert_eq!(
            super::effective_contract_helper_corp_roles(
                s,
                bare_corp,
                other_char,
                eve_db::corp_roles::ROLE_CONTRACT_MANAGER,
                0
            ),
            Some((false, eve_db::corp_roles::ROLE_CONTRACT_MANAGER))
        );
        assert_eq!(
            super::effective_contract_helper_corp_roles(
                s,
                corp_id,
                other_char,
                eve_db::corp_roles::ROLE_CONTRACT_MANAGER,
                0
            ),
            Some((false, 0))
        );
    });
}
