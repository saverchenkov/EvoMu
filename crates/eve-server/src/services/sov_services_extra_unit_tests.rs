//! Additional branch-coverage and mutation-killing unit tests for `sov_services.rs`.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::sov_services::*;
use crate::services::station_structure::structure_upwell_ops::{
    load_ext_state, save_ext_state, AccessGroupRecord,
};
use eve_db::{
    AllianceRecord, ContractRecord, CorporationRecord, InventoryFlag, InventoryItemRecord,
    IskCents, PersistedHubUpgrade, PersistedSovHubData, UpwellStructureRecord,
};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::SdeRegistry;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn make_ctx() -> ServerContext {
    let sde = SHARED_SDE
        .get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed()))
        .clone();
    ServerContext::with_sde_and_repository(sde, Arc::new(eve_db::WorldRepository::with_demo_seed()))
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
}

fn make_session(char_id: i64, corp_id: i64, sys_id: Option<u32>) -> SessionState {
    let mut s = SessionState::new(9501);
    s.user_id = Some(1);
    s.character_id = Some(char_id);
    s.corporation_id = Some(corp_id);
    s.solar_system_id = sys_id;
    s
}

async fn call_sov_rpc(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    handle(ctx, session, service, method, args, &HashMap::new()).await
}

#[test]
fn adm_all_index_tiers_hub_resolution_and_fuel_acl_precedence_kill_mutations() {
    let expected_mil_ind = [1.0, 1.6, 2.2, 2.7, 3.1, 3.5, 3.5];
    for (lvl, &exp) in expected_mil_ind.iter().enumerate() {
        assert_eq!(calculate_adm(lvl as u8, 0, 0), exp, "mil_idx={lvl}");
        assert_eq!(calculate_adm(0, lvl as u8, 0), exp, "ind_idx={lvl}");
    }
    let expected_strat = [1.0, 1.4, 1.6, 1.8, 1.9, 2.0, 2.0];
    for (lvl, &exp) in expected_strat.iter().enumerate() {
        assert_eq!(calculate_adm(0, 0, lvl as u8), exp, "strat_idx={lvl}");
    }
    assert_eq!(calculate_adm(5, 5, 5), 6.0);

    assert_eq!(scale_entosis_capture_seconds(300, 0.5), 300);
    assert_eq!(scale_entosis_capture_seconds(300, 1.6), 480);

    let ctx = make_ctx();
    let sys_id = 30_000_142_u32;
    let (destroyed_hub_id, active_hub_id) = (1_070_000_001_i64, 1_070_000_002_i64);
    let (owner_corp, other_corp, ally_id) = (98_000_101_i64, 98_000_102_i64, 99_000_101_i64);
    let (creator_cid, member_cid) = (90_000_201_i64, 90_000_202_i64);

    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            destroyed_hub_id,
            UpwellStructureRecord {
                structure_id: destroyed_hub_id,
                type_id: TYPE_SOVEREIGNTY_HUB,
                owner_corp_id: owner_corp,
                solar_system_id: sys_id,
                name: "Destroyed Hub".into(),
                state: "Destroyed".into(),
                ..Default::default()
            },
        );
        assert_eq!(resolve_sov_hub_id(snap, None, Some(sys_id)), None);
        assert_eq!(resolve_sov_hub_id(snap, Some(-55), Some(sys_id)), Some(-55));
        assert_eq!(
            resolve_sov_hub_id(snap, Some(i64::from(sys_id)), None),
            Some(i64::from(sys_id))
        );

        assert!(!can_access_sov_hub_fuel(
            snap,
            destroyed_hub_id,
            creator_cid,
            owner_corp,
            Some(ally_id),
            eve_db::corp_roles::ROLE_DIRECTOR,
            true,
        ));

        snap.structures.insert(
            active_hub_id,
            UpwellStructureRecord {
                structure_id: active_hub_id,
                type_id: TYPE_SOVEREIGNTY_HUB,
                owner_corp_id: owner_corp,
                solar_system_id: sys_id,
                name: "Active Hub".into(),
                state: "ShieldVulnerable".into(),
                ..Default::default()
            },
        );
        snap.corporations.insert(
            owner_corp,
            CorporationRecord {
                corporation_id: owner_corp,
                ceo_id: creator_cid,
                alliance_id: None,
                name: "Owner".into(),
                ticker: "OWN".into(),
                ..Default::default()
            },
        );

        assert!(!can_access_sov_hub_fuel(
            snap,
            active_hub_id,
            member_cid,
            other_corp,
            None,
            eve_db::corp_roles::ROLE_DIRECTOR,
            true,
        ));

        let mut ext = load_ext_state(snap);
        let mut members = HashMap::new();
        members.insert(member_cid, 0);
        members.insert(other_corp, -1);
        members.insert(ally_id, 0);
        ext.owner_groups.insert(
            88,
            AccessGroupRecord {
                group_id: 88,
                creator_id: creator_cid,
                owner_corp_id: owner_corp,
                name: "ACL 88".into(),
                description: String::new(),
                members,
            },
        );
        save_ext_state(snap, &ext);
        snap.sov_runtime.fuel_access_groups.insert(active_hub_id, 88);

        assert!(can_access_sov_hub_fuel(snap, active_hub_id, creator_cid, other_corp, None, 0, false));
        assert!(can_access_sov_hub_fuel(snap, active_hub_id, member_cid, other_corp, None, 0, false));
        assert!(!can_access_sov_hub_fuel(
            snap,
            active_hub_id,
            90_000_299,
            other_corp,
            Some(ally_id),
            0,
            false,
        ));
        assert!(can_access_sov_hub_fuel(
            snap,
            active_hub_id,
            90_000_299,
            98_000_199,
            Some(ally_id),
            0,
            false,
        ));
        assert!(!can_access_sov_hub_fuel(
            snap,
            active_hub_id,
            90_000_299,
            owner_corp,
            None,
            eve_db::corp_roles::ROLE_DIRECTOR,
            true,
        ));
    });
}

#[tokio::test]
async fn sov_rpcs_dev_indices_map_beyonce_and_hub_hacks_kill_mutations() {
    let ctx = make_ctx();
    let char_id = 90_000_001_i64;
    let sys_id = 30_000_142_u32;
    let (hub_id, skyhook_id, jammer_id) = (1_070_000_050_i64, 1_070_000_051_i64, 1_070_000_052_i64);
    let ally_id = 99_000_050_i64;

    let corp_id = ctx.db.with_mut_snapshot(|snap| {
        let cid = snap.characters[&char_id].corporation_id;
        snap.corporations.insert(
            cid,
            CorporationRecord {
                corporation_id: cid,
                ceo_id: 90_000_999,
                alliance_id: Some(ally_id),
                name: "SovTestCorp".into(),
                ticker: "STC".into(),
                ..Default::default()
            },
        );
        snap.alliances.insert(
            ally_id,
            AllianceRecord {
                alliance_id: ally_id,
                name: "SovTestAlliance".into(),
                short_name: "STA".into(),
                executor_corp_id: cid,
                member_corp_ids: std::collections::HashSet::from([cid]),
            },
        );
        let st = snap.system_states.entry(sys_id).or_default();
        st.solar_system_id = sys_id;
        st.sov_alliance_id = Some(ally_id);
        st.military_index = 2;
        st.industrial_index = 3;
        st.strategic_index = 1;

        snap.structures.insert(
            hub_id,
            UpwellStructureRecord {
                structure_id: hub_id,
                type_id: TYPE_SOVEREIGNTY_HUB,
                owner_corp_id: cid,
                solar_system_id: sys_id,
                name: "Hub".into(),
                state: "ShieldVulnerable".into(),
                reinforced_until_unix: 99_999,
                ..Default::default()
            },
        );
        snap.structure_vulnerability_windows.insert(hub_id, (100, 200));
        snap.sov_runtime.sov_hubs.insert(
            hub_id,
            PersistedSovHubData {
                hub_id,
                upgrades: vec![
                    PersistedHubUpgrade { type_id: 81_001, online: true, low_power: false },
                    PersistedHubUpgrade { type_id: 81_002, online: false, low_power: false },
                ],
                ..Default::default()
            },
        );
        snap.structures.insert(
            skyhook_id,
            UpwellStructureRecord {
                structure_id: skyhook_id,
                type_id: TYPE_ORBITAL_SKYHOOK,
                owner_corp_id: cid,
                solar_system_id: sys_id,
                name: "Skyhook".into(),
                state: "Online".into(),
                ..Default::default()
            },
        );
        cid
    });

    set_skyhook_hackable(&ctx, sys_id, skyhook_id, false);
    read_sov_runtime(&ctx, |rt| {
        assert_eq!(rt.skyhook_hackable.get(&skyhook_id), Some(&false));
        assert!(!rt.skyhook_reagents.contains_key(&skyhook_id));
    });
    set_skyhook_hackable(&ctx, sys_id, skyhook_id, true);
    read_sov_runtime(&ctx, |rt| {
        assert_eq!(rt.skyhook_reagents.get(&skyhook_id), Some(&(TYPE_MAGMATIC_GAS, 500)));
    });

    record_hub_hack(&ctx, sys_id, hub_id, 0);
    assert!(!is_hub_hacked(&ctx, hub_id));
    ctx.db.with_snapshot(|snap| {
        let h = &snap.sov_runtime.sov_hubs[&hub_id];
        assert!(h.upgrades[0].low_power);
        assert!(!h.upgrades[1].low_power);
        assert_eq!(snap.structures[&hub_id].reinforced_until_unix, 99_999);
    });

    assert_eq!(
        claim_system_sovereignty(&ctx, 39_999_999, ally_id, corp_id, hub_id)
            .unwrap_err()
            .code,
        "SystemNotFound"
    );
    assert!(claim_system_sovereignty(&ctx, sys_id, ally_id, corp_id, hub_id).is_ok());
    emit_dev_index_changed(&ctx, sys_id);

    let mut sess_no_sys = make_session(char_id, corp_id, None);
    assert_eq!(
        call_sov_rpc(&ctx, &mut sess_no_sys, "sovMgr", "SetSovHubFuelAccessGroup", &[])
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidArguments"
    );
    assert_eq!(
        call_sov_rpc(&ctx, &mut sess_no_sys, "sovMgr", "GetSovHubFuelAccessGroup", &[])
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidArguments"
    );
    assert_eq!(
        call_sov_rpc(&ctx, &mut sess_no_sys, "sovMgr", "DestroySkyhooks", &[])
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "NoSolarSystem"
    );
    assert_eq!(
        call_sov_rpc(&ctx, &mut sess_no_sys, "sovMgr", "IsOnLocalSovHubFuelAccessGroup", &[])
            .await
            .unwrap()
            .unwrap(),
        PyRep::Bool(false)
    );

    let mut sess = make_session(char_id, corp_id, Some(sys_id));
    assert_eq!(
        call_sov_rpc(&ctx, &mut sess, "sovMgr", "SetSovHubFuelAccessGroup", &[])
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "CorpRoleDenied"
    );
    assert_eq!(
        call_sov_rpc(&ctx, &mut sess, "sovMgr", "DestroySkyhooks", &[])
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "CorpRoleDenied"
    );

    sess.corp_role = eve_db::corp_roles::ROLE_DIRECTOR;
    assert_eq!(
        call_sov_rpc(&ctx, &mut sess, "sovMgr", "IsOnLocalSovHubFuelAccessGroup", &[])
            .await
            .unwrap()
            .unwrap(),
        PyRep::Bool(true)
    );

    let skyhooks = call_sov_rpc(&ctx, &mut sess, "colonyResourcesMgr", "GetMyCorporationSkyhooks", &[])
        .await
        .unwrap()
        .unwrap();
    if let PyRep::List(list) = skyhooks {
        assert_eq!(list.len(), 1);
        assert_eq!(
            list[0].dict_get("reagentTypeID"),
            Some(&PyRep::Integer(i64::from(TYPE_MAGMATIC_GAS)))
        );
        assert_eq!(list[0].dict_get("reagentQuantity"), Some(&PyRep::Integer(500)));
    } else {
        panic!("expected List");
    }

    let all_idx = call_sov_rpc(&ctx, &mut sess, "devIndexManager", "GetAllDevelopmentIndices", &[])
        .await
        .unwrap()
        .unwrap();
    if let PyRep::Dict(entries) = all_idx {
        let jita_entry = entries
            .iter()
            .find(|(k, _)| k.as_i64() == Some(i64::from(sys_id)))
            .map(|(_, v)| v)
            .expect("Jita dev index");
        assert_eq!(jita_entry.dict_get("militaryPoints"), Some(&PyRep::Integer(2_000)));
        assert_eq!(jita_entry.dict_get("industrialPoints"), Some(&PyRep::Integer(3_000)));
        assert_eq!(jita_entry.dict_get("claimedFor"), Some(&PyRep::Integer(1)));
        assert_eq!(jita_entry.dict_get("adm"), Some(&PyRep::Real(calculate_adm(2, 3, 1))));
    } else {
        panic!("expected Dict");
    }

    assert_eq!(
        call_sov_rpc(
            &ctx,
            &mut sess,
            "devIndexManager",
            "GetDevelopmentIndicesForSystem",
            &[PyRep::Integer(99_999_999)]
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidSolarSystem"
    );
    let sys_idx = call_sov_rpc(&ctx, &mut sess, "devIndexManager", "GetDevelopmentIndicesForSystem", &[])
        .await
        .unwrap()
        .unwrap();
    if let PyRep::Dict(entries) = sys_idx {
        assert_eq!(entries.len(), 3);
        let mil = &entries[0].1;
        assert_eq!(mil.dict_get("points"), Some(&PyRep::Integer(40_000)));
        assert_eq!(mil.dict_get("level"), Some(&PyRep::Integer(2)));
        assert_eq!(mil.dict_get("increasing"), Some(&PyRep::Integer(1)));
    } else {
        panic!("expected Dict");
    }

    let sov_data = call_sov_rpc(&ctx, &mut sess, "map", "GetCurrentSovData", &[])
        .await
        .unwrap()
        .unwrap();
    if let PyRep::List(list) = sov_data {
        let row = list
            .iter()
            .find(|r| r.dict_get("solarSystemID") == Some(&PyRep::Integer(i64::from(sys_id))))
            .expect("sov row");
        assert_eq!(row.dict_get("allianceID"), Some(&PyRep::Integer(ally_id)));
        assert_eq!(row.dict_get("corporationID"), Some(&PyRep::Integer(corp_id)));
        assert_eq!(row.dict_get("infrastructureHubID"), Some(&PyRep::Integer(hub_id)));
    } else {
        panic!("expected List");
    }

    assert_eq!(
        call_sov_rpc(&ctx, &mut sess_no_sys, "beyonce", "GetCynoJammerState", &[])
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "NoSolarSystem"
    );
    let now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            jammer_id,
            UpwellStructureRecord {
                structure_id: jammer_id,
                type_id: TYPE_TENEBREX_CYNO_JAMMER,
                owner_corp_id: corp_id,
                solar_system_id: sys_id,
                name: "Tenebrex".into(),
                state: "Online".into(),
                fuel_blocks: 50,
                ..Default::default()
            },
        );
        snap.sov_runtime.tenebrex_spool_ready_unix.insert(jammer_id, now + 300);
        snap.items.insert(
            880_501,
            InventoryItemRecord {
                item_id: 880_501,
                type_id: 33_476,
                owner_id: char_id,
                location_id: i64::from(sys_id),
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });
    let jam_state = call_sov_rpc(&ctx, &mut sess, "beyonce", "GetCynoJammerState", &[])
        .await
        .unwrap()
        .unwrap();
    assert_eq!(jam_state.dict_get("isJammed"), Some(&PyRep::Bool(true)));
    assert_eq!(jam_state.dict_get("spoolingUp"), Some(&PyRep::Bool(true)));
    assert_eq!(jam_state.dict_get("structureInhibitor"), Some(&PyRep::Bool(true)));
}

#[test]
fn clone_jump_reset_and_contract_helpers_kill_mutations() {
    let ctx = make_ctx();
    let char_id = 90_000_001_i64;
    let player_corp = 98_000_555_i64;
    let station_id = 60_003_760_i64;
    let mut sess = make_session(char_id, 1_000_125, Some(30_000_142));
    sess.station_id = Some(station_id);

    assert_eq!(handle_reset_last_clone_jump_time(&ctx, &sess).unwrap_err().code, "GMRoleRequired");
    sess.role = eve_machonet::service_roles::ROLE_GMH;
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().last_clone_jump_unix = 123_456;
    });
    assert_eq!(handle_reset_last_clone_jump_time(&ctx, &sess).unwrap(), PyRep::Integer(0));
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.characters[&char_id].last_clone_jump_unix, 0);
    });

    assert_eq!(
        handle_contract_delete_notif(&ctx, &sess, "DeleteNotification", &[PyRep::Integer(0)])
            .unwrap_err()
            .code,
        "InvalidArgument"
    );
    assert_eq!(
        handle_contract_delete_notif(&ctx, &sess, "DeleteNotification", &[PyRep::Integer(1), PyRep::Bool(true)])
            .unwrap_err()
            .code,
        "CrpAccessDenied"
    );

    sess.corporation_id = Some(player_corp);
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.insert(
            player_corp,
            CorporationRecord {
                corporation_id: player_corp,
                ceo_id: 90_000_999,
                name: "PlayerCorp".into(),
                ticker: "PCRP".into(),
                ..Default::default()
            },
        );
    });
    assert_eq!(
        handle_contract_delete_notif(&ctx, &sess, "DeleteNotification", &[PyRep::Integer(1), PyRep::Integer(1)])
            .unwrap_err()
            .code,
        "CrpAccessDenied"
    );

    sess.corp_role = eve_db::corp_roles::ROLE_CONTRACT_MANAGER;
    assert_eq!(
        handle_contract_delete_notif(&ctx, &sess, "DeleteNotification", &[PyRep::Integer(999_999), PyRep::Bool(true)])
            .unwrap_err()
            .code,
        "ContractNotFound"
    );

    let (unrelated_cid, corp_cid) = (770_001_i64, 770_002_i64);
    ctx.db.with_mut_snapshot(|snap| {
        snap.contracts.insert(
            unrelated_cid,
            ContractRecord {
                contract_id: unrelated_cid,
                issuer_id: 90_000_888,
                issuer_corp_id: 98_000_888,
                status: "Outstanding".into(),
                ..Default::default()
            },
        );
        snap.contracts.insert(
            corp_cid,
            ContractRecord {
                contract_id: corp_cid,
                issuer_id: 90_000_888,
                issuer_corp_id: player_corp,
                for_corp: true,
                status: "Expired".into(),
                start_station_id: station_id,
                reward: IskCents(0),
                ..Default::default()
            },
        );
    });
    assert_eq!(
        handle_contract_delete_notif(
            &ctx,
            &sess,
            "DeleteNotification",
            &[PyRep::Integer(unrelated_cid), PyRep::Bool(false)]
        )
        .unwrap_err()
        .code,
        "ContractAccessDenied"
    );
    assert_eq!(
        handle_contract_delete_notif(
            &ctx,
            &sess,
            "DeleteNotification",
            &[PyRep::Integer(corp_cid), PyRep::Bool(true)]
        )
        .unwrap(),
        PyRep::Bool(true)
    );

    assert_eq!(handle_contract_delete_multiple(&ctx, &sess, &[]).unwrap_err().code, "InvalidArgument");
    let del_expired = handle_contract_delete_multiple(&ctx, &sess, &[PyRep::Integer(corp_cid)]).unwrap();
    if let PyRep::Tuple(parts) = del_expired {
        assert_eq!(parts[0], PyRep::List(vec![]));
        assert_eq!(parts[1], PyRep::List(vec![PyRep::Integer(corp_cid)]));
    } else {
        panic!("expected Tuple");
    }
    let corp_cid_out = 770_003_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.contracts.insert(
            corp_cid_out,
            ContractRecord {
                contract_id: corp_cid_out,
                issuer_id: 90_000_888,
                issuer_corp_id: player_corp,
                for_corp: true,
                status: "Outstanding".into(),
                start_station_id: station_id,
                reward: IskCents(0),
                ..Default::default()
            },
        );
    });
    let del_single = handle_contract_delete_multiple(&ctx, &sess, &[PyRep::Integer(corp_cid_out)]).unwrap();
    if let PyRep::Tuple(parts) = del_single {
        assert_eq!(parts[0], PyRep::List(vec![PyRep::Integer(corp_cid_out)]));
        assert_eq!(parts[1], PyRep::List(vec![]));
    } else {
        panic!("expected Tuple");
    }

    let (bpc_id, stack_id) = (880_601_i64, 880_602_i64);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            bpc_id,
            InventoryItemRecord {
                item_id: bpc_id,
                type_id: 687,
                owner_id: player_corp,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                is_blueprint_copy: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            stack_id,
            InventoryItemRecord {
                item_id: stack_id,
                type_id: 34,
                owner_id: player_corp,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 42,
                singleton: false,
                is_blueprint_copy: false,
                ..Default::default()
            },
        );
    });
    let corp_items =
        handle_contract_get_items_in_dockable(&ctx, &sess, &[PyRep::None, PyRep::Bool(true)]).unwrap();
    if let PyRep::List(rows) = corp_items {
        let bpc_row = rows
            .iter()
            .find(|r| r.dict_get("itemID") == Some(&PyRep::Integer(bpc_id)))
            .expect("bpc row");
        assert_eq!(bpc_row.dict_get("singleton"), Some(&PyRep::Integer(2)));
        assert_eq!(bpc_row.dict_get("quantity"), Some(&PyRep::Integer(-2)));

        let stack_row = rows
            .iter()
            .find(|r| r.dict_get("itemID") == Some(&PyRep::Integer(stack_id)))
            .expect("stack row");
        assert_eq!(stack_row.dict_get("singleton"), Some(&PyRep::Integer(0)));
        assert_eq!(stack_row.dict_get("quantity"), Some(&PyRep::Integer(42)));
    } else {
        panic!("expected List");
    }
}
