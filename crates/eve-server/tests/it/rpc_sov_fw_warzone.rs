//! Integration tests for Sovereignty (`sovMgr`), Faction Warfare Warzones (`fwWarzoneSolarsystem`),
//! and CrimeWatch Bounty Entity Isolation (`P1-02`).

use crate::common;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::combat_crimewatch::with_mut_combat_runtime;
use std::collections::{HashMap, HashSet};

const NULLSEC_SYS_ID: u32 = 30_004_759; // VFK-IV (Null-sec)
const LOWSEC_FW_SYS_ID: u32 = 30_002_055; // Amo (Warzone HQ)
const CHAR_ID: i64 = 90_000_001;
const CORP_ID: i64 = 98_000_001;
const ALLIANCE_ID: i64 = 99_000_001;
const HUB_ID: i64 = 950_001;
const SKYHOOK_ID: i64 = 950_002;

fn get_kv<'a>(rep: &'a PyRep, key: &str) -> Option<&'a PyRep> {
    match rep {
        PyRep::KeyVal(dict) => dict.get(key),
        PyRep::Tuple(t) => t.get(1).and_then(|v| get_kv(v, key)),
        PyRep::Dict(entries) => entries.iter().find_map(|(k, v)| {
            if let PyRep::String(s) | PyRep::Token(s) = k {
                if s == key {
                    Some(v)
                } else {
                    None
                }
            } else {
                None
            }
        }),
        _ => None,
    }
}

fn default_sys_state(
    sys_id: u32,
    dbs: f64,
    fw_pct: f64,
    aid: Option<i64>,
) -> eve_db::SystemDynamicState {
    eve_db::SystemDynamicState {
        solar_system_id: sys_id,
        dbs_multiplier: dbs,
        ess_main_bank: eve_db::IskCents(0),
        ess_reserve_bank: eve_db::IskCents(0),
        ess_linked_character_id: None,
        ess_link_started_tick: None,
        sov_alliance_id: aid,
        military_index: 3,
        industrial_index: 2,
        strategic_index: 4,
        fw_contested_pct: fw_pct,
        insurgency_corruption_stage: 0,
        insurgency_suppression_stage: 0,
        wormhole_remaining_mass_kg: 0.0,
        wormhole_destination_system_id: None,
        signature_position: None,
        fw_complex_position: None,
        fw_capture_seconds_elapsed: 0,
        fw_capture_duration_seconds: 0,
        last_fw_capture_tick: 0,
        last_sov_claim_tick: 0,
    }
}

fn upwell_struct(
    sid: i64,
    tid: u32,
    cid: i64,
    sys_id: u32,
    name: &str,
) -> eve_db::UpwellStructureRecord {
    eve_db::UpwellStructureRecord {
        structure_id: sid,
        type_id: tid,
        owner_corp_id: cid,
        solar_system_id: sys_id,
        name: name.into(),
        position: [0.0, 0.0, 0.0],
        state: "ShieldVulnerable".into(),
        quantum_core_installed: true,
        fuel_blocks: 1000,
        shield_hp: 1.0,
        armor_hp: 1.0,
        hull_hp: 1.0,
        reinforced_until_unix: 0,
        last_damage_tick: 0,
        last_damage_unix: 0, self_destruct_at_unix: None,
    }
}

async fn setup_sov_fw_env(ctx: &ServerContext) -> SessionState {
    let kw = HashMap::new();
    let mut sess = SessionState::new(3301);
    common::login_pilot(ctx, &mut sess, "demo", "demo-secret").await;
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(CHAR_ID)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");

    sess.solar_system_id = Some(NULLSEC_SYS_ID);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&CHAR_ID) {
            ch.corporation_id = CORP_ID;
            ch.alliance_id = Some(ALLIANCE_ID);
            ch.solar_system_id = NULLSEC_SYS_ID;
        }

        let mut member_roles = HashMap::new();
        member_roles.insert(
            CHAR_ID,
            eve_db::corp_roles::ROLE_DIRECTOR | eve_db::corp_roles::ROLE_STATION_MANAGER,
        );
        snap.corporations.insert(
            CORP_ID,
            eve_db::CorporationRecord {
                corporation_id: CORP_ID,
                name: "Sov Test Holding Corp".into(),
                ticker: "STHC".into(),
                ceo_id: CHAR_ID,
                alliance_id: Some(ALLIANCE_ID),
                tax_rate: 0.05,
                master_wallet: eve_db::IskCents(0),
                members: HashSet::from([CHAR_ID]),
                member_roles,
                ..Default::default()
            },
        );

        snap.alliances.insert(
            ALLIANCE_ID,
            eve_db::AllianceRecord {
                alliance_id: ALLIANCE_ID,
                name: "Sov Test Alliance".into(),
                short_name: "STA".into(),
                executor_corp_id: CORP_ID,
                member_corp_ids: HashSet::from([CORP_ID]),
            },
        );

        snap.system_states.insert(
            NULLSEC_SYS_ID,
            default_sys_state(NULLSEC_SYS_ID, 1.35, 0.0, Some(ALLIANCE_ID)),
        );
        snap.system_states.insert(
            LOWSEC_FW_SYS_ID,
            default_sys_state(LOWSEC_FW_SYS_ID, 1.0, 45.0, None),
        );
        snap.structures.insert(
            HUB_ID,
            upwell_struct(HUB_ID, 32458, CORP_ID, NULLSEC_SYS_ID, "VFK-IV I-Hub"),
        );
        snap.structures.insert(
            SKYHOOK_ID,
            upwell_struct(
                SKYHOOK_ID,
                81143,
                CORP_ID,
                NULLSEC_SYS_ID,
                "VFK-IV Skyhook",
            ),
        );

        let mut ext = eve_server::services::station_structure::structure_upwell_ops::load_ext_state(snap);
        let mut members = HashMap::new();
        members.insert(CHAR_ID, 1i64);
        ext.owner_groups.insert(42, eve_server::services::station_structure::structure_upwell_ops::AccessGroupRecord {
            group_id: 42,
            creator_id: CHAR_ID,
            owner_corp_id: CORP_ID,
            name: "Fuel Group".into(),
            description: "Sov Hub Fuel ACL".into(),
            members,
        });
        eve_server::services::station_structure::structure_upwell_ops::save_ext_state(snap, &ext);
    });

    sess.corporation_id = Some(CORP_ID);
    sess.corp_role = eve_db::corp_roles::ROLE_DIRECTOR | eve_db::corp_roles::ROLE_STATION_MANAGER;

    sess
}

#[tokio::test]
async fn test_sov_mgr_structures_and_fuel_access() {
    // parity: rpc:sovMgr.DestroySkyhooks
    // parity: rpc:sovMgr.GetInfrastructureHubInfo
    // parity: rpc:sovMgr.GetSovHubFuelAccessGroup
    // parity: rpc:sovMgr.GetSovStructuresInfoForLocalSolarSystem
    // parity: rpc:sovMgr.GetSovStructuresInfoForSolarSystem
    // parity: rpc:sovMgr.GetSystemSovereigntyInfo
    // parity: rpc:sovMgr.IsOnLocalSovHubFuelAccessGroup
    // parity: rpc:sovMgr.SetSovHubFuelAccessGroup
    // parity: rpc:sovMgr.remove_from_cache
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let kw = HashMap::new();
    let mut sess = setup_sov_fw_env(&ctx).await;

    // 1. GetSystemSovereigntyInfo
    let sov_info = ctx
        .dispatch_rpc(
            &mut sess,
            "sovMgr",
            None,
            "GetSystemSovereigntyInfo",
            &[PyRep::Integer(NULLSEC_SYS_ID as i64)],
            &kw,
            None,
        )
        .await
        .expect("GetSystemSovereigntyInfo");
    assert_eq!(
        get_kv(&sov_info, "solarSystemID").and_then(|v| v.as_i64()),
        Some(NULLSEC_SYS_ID as i64)
    );
    assert_eq!(
        get_kv(&sov_info, "allianceID").and_then(|v| v.as_i64()),
        Some(ALLIANCE_ID)
    );
    assert_eq!(
        get_kv(&sov_info, "infrastructureHubID").and_then(|v| v.as_i64()),
        Some(HUB_ID)
    );

    // 2. GetInfrastructureHubInfo
    let hub_info = ctx
        .dispatch_rpc(
            &mut sess,
            "sovMgr",
            None,
            "GetInfrastructureHubInfo",
            &[PyRep::Integer(NULLSEC_SYS_ID as i64)],
            &kw,
            None,
        )
        .await
        .expect("GetInfrastructureHubInfo");
    assert_eq!(
        get_kv(&hub_info, "hubID").and_then(|v| v.as_i64()),
        Some(HUB_ID)
    );

    // 3. GetSovStructuresInfoForSolarSystem & GetSovStructuresInfoForLocalSolarSystem
    let structures_sys = ctx
        .dispatch_rpc(
            &mut sess,
            "sovMgr",
            None,
            "GetSovStructuresInfoForSolarSystem",
            &[PyRep::Integer(NULLSEC_SYS_ID as i64)],
            &kw,
            None,
        )
        .await
        .expect("GetSovStructuresInfoForSolarSystem");
    assert!(get_kv(&structures_sys, "structures").is_some());

    let local_structures = ctx
        .dispatch_rpc(
            &mut sess,
            "sovMgr",
            None,
            "GetSovStructuresInfoForLocalSolarSystem",
            &[],
            &kw,
            None,
        )
        .await
        .expect("GetSovStructuresInfoForLocalSolarSystem");
    assert!(get_kv(&local_structures, "structures").is_some());

    // 4. SetSovHubFuelAccessGroup, GetSovHubFuelAccessGroup & IsOnLocalSovHubFuelAccessGroup
    let set_access = ctx
        .dispatch_rpc(
            &mut sess,
            "sovMgr",
            None,
            "SetSovHubFuelAccessGroup",
            &[PyRep::Integer(HUB_ID), PyRep::Integer(42)],
            &kw,
            None,
        )
        .await
        .expect("SetSovHubFuelAccessGroup");
    assert_eq!(set_access.as_i64(), Some(42));

    let get_access = ctx
        .dispatch_rpc(
            &mut sess,
            "sovMgr",
            None,
            "GetSovHubFuelAccessGroup",
            &[PyRep::Integer(HUB_ID)],
            &kw,
            None,
        )
        .await
        .expect("GetSovHubFuelAccessGroup");
    assert_eq!(get_access.as_i64(), Some(42));

    let is_on_group = ctx
        .dispatch_rpc(
            &mut sess,
            "sovMgr",
            None,
            "IsOnLocalSovHubFuelAccessGroup",
            &[PyRep::Integer(HUB_ID)],
            &kw,
            None,
        )
        .await
        .expect("IsOnLocalSovHubFuelAccessGroup");
    assert_eq!(is_on_group.as_bool(), Some(true));

    // 5. remove_from_cache
    let rfc = ctx
        .dispatch_rpc(
            &mut sess,
            "sovMgr",
            None,
            "remove_from_cache",
            &[PyRep::Integer(HUB_ID)],
            &kw,
            None,
        )
        .await
        .expect("remove_from_cache");
    assert_eq!(rfc, PyRep::None);

    // 6. DestroySkyhooks
    let destroy_res = ctx
        .dispatch_rpc(
            &mut sess,
            "sovMgr",
            None,
            "DestroySkyhooks",
            &[PyRep::Integer(NULLSEC_SYS_ID as i64)],
            &kw,
            None,
        )
        .await
        .expect("DestroySkyhooks");
    assert!(matches!(
        destroy_res,
        PyRep::List(_) | PyRep::Integer(_) | PyRep::None
    ));
}

#[tokio::test]
async fn test_fw_warzone_solarsystem_states_and_queries() {
    // parity: rpc:fwWarzoneSolarsystem.GetAllWarzones
    // parity: rpc:fwWarzoneSolarsystem.GetAllWarzonesOccupationStates
    // parity: rpc:fwWarzoneSolarsystem.GetAllWarzonesOccupationStatesUncached
    // parity: rpc:fwWarzoneSolarsystem.GetHQSystemIDs
    // parity: rpc:fwWarzoneSolarsystem.GetLocalOccupationState
    // parity: rpc:fwWarzoneSolarsystem.iteritems
    // parity: rpc:fwWarzoneSolarsystem.itervalues
    // parity: rpc:fwWarzoneSolarsystem.keys
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let kw = HashMap::new();
    let mut sess = setup_sov_fw_env(&ctx).await;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&CHAR_ID) {
            ch.solar_system_id = LOWSEC_FW_SYS_ID;
        }
    });
    sess.solar_system_id = Some(LOWSEC_FW_SYS_ID);

    // 1. GetHQSystemIDs
    let hqs = ctx
        .dispatch_rpc(
            &mut sess,
            "fwWarzoneSolarsystem",
            None,
            "GetHQSystemIDs",
            &[],
            &kw,
            None,
        )
        .await
        .expect("GetHQSystemIDs");
    match hqs {
        PyRep::List(list) => assert_eq!(list.len(), 4, "Must contain 4 FW HQ system IDs"),
        other => panic!("Expected List for GetHQSystemIDs, got {other:?}"),
    }

    // 2. GetAllWarzones
    let warzones = ctx
        .dispatch_rpc(
            &mut sess,
            "fwWarzoneSolarsystem",
            None,
            "GetAllWarzones",
            &[],
            &kw,
            None,
        )
        .await
        .expect("GetAllWarzones");
    match warzones {
        PyRep::List(list) => assert_eq!(list.len(), 2, "Must contain 2 warzones"),
        other => panic!("Expected List for GetAllWarzones, got {other:?}"),
    }

    // 3. GetAllWarzonesOccupationStates & GetAllWarzonesOccupationStatesUncached
    let states_cached = ctx
        .dispatch_rpc(
            &mut sess,
            "fwWarzoneSolarsystem",
            None,
            "GetAllWarzonesOccupationStates",
            &[],
            &kw,
            None,
        )
        .await
        .expect("GetAllWarzonesOccupationStates");
    assert!(matches!(states_cached, PyRep::List(_)));

    let states_uncached = ctx
        .dispatch_rpc(
            &mut sess,
            "fwWarzoneSolarsystem",
            None,
            "GetAllWarzonesOccupationStatesUncached",
            &[],
            &kw,
            None,
        )
        .await
        .expect("GetAllWarzonesOccupationStatesUncached");
    assert!(matches!(states_uncached, PyRep::List(_)));

    // 4. GetLocalOccupationState
    let local_state = ctx
        .dispatch_rpc(
            &mut sess,
            "fwWarzoneSolarsystem",
            None,
            "GetLocalOccupationState",
            &[],
            &kw,
            None,
        )
        .await
        .expect("GetLocalOccupationState");
    match local_state {
        PyRep::Tuple(pair) => {
            assert_eq!(
                pair.first().and_then(|id| id.as_i64()),
                Some(LOWSEC_FW_SYS_ID as i64)
            );
            assert!(pair.get(1).is_some());
        }
        other => panic!("Expected Tuple for GetLocalOccupationState, got {other:?}"),
    }

    // 5. iteritems, itervalues, keys
    let iter_items = ctx
        .dispatch_rpc(
            &mut sess,
            "fwWarzoneSolarsystem",
            None,
            "iteritems",
            &[],
            &kw,
            None,
        )
        .await
        .expect("iteritems");
    assert!(matches!(iter_items, PyRep::List(_)));

    let iter_vals = ctx
        .dispatch_rpc(
            &mut sess,
            "fwWarzoneSolarsystem",
            None,
            "itervalues",
            &[],
            &kw,
            None,
        )
        .await
        .expect("itervalues");
    assert!(matches!(iter_vals, PyRep::List(_)));

    let keys = ctx
        .dispatch_rpc(
            &mut sess,
            "fwWarzoneSolarsystem",
            None,
            "keys",
            &[],
            &kw,
            None,
        )
        .await
        .expect("keys");
    match keys {
        PyRep::List(list) => assert!(list
            .iter()
            .any(|k| k.as_i64() == Some(LOWSEC_FW_SYS_ID as i64))),
        other => panic!("Expected List for keys, got {other:?}"),
    }

    // 6. P1-02 Remediation: verify entity type isolation when searching bounties by ID
    with_mut_combat_runtime(&ctx, |rt| {
        rt.player_bounty_pool.insert(CHAR_ID, 5_000_000.0);
        rt.player_bounty_pool.insert(CORP_ID, 25_000_000.0);
    });

    let corp_search_char = ctx
        .dispatch_rpc(
            &mut sess,
            "bountyProxy",
            None,
            "SearchCorpBounties",
            &[PyRep::Integer(CHAR_ID)],
            &kw,
            None,
        )
        .await
        .expect("SearchCorpBounties(char_id)");
    assert_eq!(
        corp_search_char,
        PyRep::List(vec![]),
        "P1-02: SearchCorpBounties must not return character bounty"
    );

    let char_search_corp = ctx
        .dispatch_rpc(
            &mut sess,
            "bountyProxy",
            None,
            "SearchCharBounties",
            &[PyRep::Integer(CORP_ID)],
            &kw,
            None,
        )
        .await
        .expect("SearchCharBounties(corp_id)");
    assert_eq!(
        char_search_corp,
        PyRep::List(vec![]),
        "P1-02: SearchCharBounties must not return corp bounty"
    );

    let corp_search_corp = ctx
        .dispatch_rpc(
            &mut sess,
            "bountyProxy",
            None,
            "SearchCorpBounties",
            &[PyRep::Integer(CORP_ID)],
            &kw,
            None,
        )
        .await
        .expect("SearchCorpBounties(corp_id)");
    match corp_search_corp {
        PyRep::List(list) => {
            assert_eq!(list.len(), 1);
            assert_eq!(
                get_kv(&list[0], "targetID").and_then(|v| v.as_i64()),
                Some(CORP_ID)
            );
        }
        other => panic!("Expected List for SearchCorpBounties, got {other:?}"),
    }
}
