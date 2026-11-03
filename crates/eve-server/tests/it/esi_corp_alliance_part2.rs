//! `ESI-5` integration tests (part 2) for `Corporation` blueprints, divisions, standings,
//! structures, facilities, and starbases.

#![forbid(unsafe_code)]

use super::esi_corp_alliance::{check_cached_get, issue_token_with_scopes};
use eve_db::corp_roles::{ROLE_DIRECTOR, ROLE_FACTORY_MANAGER, ROLE_STATION_MANAGER};
use eve_db::{
    CorpWalletDivisionRecord, CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents,
    StandingTransactionRecord, UpwellStructureRecord,
};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::esi::sample_routes::format_iso8601_utc;
use eve_server::esi::{EsiHttpRequest, EsiRouter, OpenApiSpec};
use eve_server::router::ServerContext;
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

async fn rpc_call(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> PyRep {
    ctx.dispatch_rpc(session, service, None, method, args, &HashMap::new(), None)
        .await
        .unwrap_or_else(|e| panic!("RPC {service}.{method} failed: {e:?}"))
}

#[tokio::test]
async fn test_esi5_corporation_divisions_blueprints_and_standings() {
    // parity: esi:GET /corporations/{corporation_id}/blueprints
    // parity: esi:GET /corporations/{corporation_id}/standings, feat:esi.corporation.blueprints
    // parity: feat:esi.corporation.standings
    eve_testkit::attest(&[
        "esi:GET /corporations/{corporation_id}/blueprints",
        "esi:GET /corporations/{corporation_id}/standings",
        "feat:esi.corporation.blueprints",
        "feat:esi.corporation.standings",
    ]);

    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);
    let router = EsiRouter::new(Arc::clone(&ctx));
    let spec = OpenApiSpec::pinned();

    let ceo_id = 90_000_001_i64;
    let corp_id = 98_500_020_i64;
    let jita_station = 60_003_760_i64;
    let bpo_id = 885_101_i64;
    let (&sde_agent_id, _) = ctx.sde.agents.iter().next().expect("SDE agent");

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&ceo_id) {
            ch.user_id = 1;
            ch.corporation_id = corp_id;
            ch.station_id = Some(jita_station);
            ch.solar_system_id = 30_000_142;
            ch.balance = IskCents::from_isk(500_000_000);
        }
        snap.corporations.insert(
            corp_id,
            CorporationRecord {
                corporation_id: corp_id,
                name: "Kaalakiota Industrial Foundry".to_string(),
                ticker: "KAAL".to_string(),
                ceo_id,
                master_wallet: IskCents::from_isk(100_000_000),
                tax_rate: 0.04,
                wallet_divisions: vec![
                    CorpWalletDivisionRecord {
                        division_id: 1,
                        name: "Capital Reserve".to_string(),
                        balance: IskCents::from_isk(500_000_000),
                    },
                    CorpWalletDivisionRecord {
                        division_id: 2,
                        name: "R&D Operations".to_string(),
                        balance: IskCents::from_isk(80_000_000),
                    },
                ],
                members: HashSet::from([ceo_id]),
                member_roles: HashMap::from([(
                    ceo_id,
                    ROLE_DIRECTOR | ROLE_FACTORY_MANAGER | ROLE_STATION_MANAGER,
                )]),
                hq_station_id: Some(jita_station),
                ..Default::default()
            },
        );

        // Seed unresearched corp BPO in Hangar (CorpSAG1); ME/TE research and BPC copy are driven via live RPCs.
        snap.items.insert(
            bpo_id,
            InventoryItemRecord {
                item_id: bpo_id,
                type_id: 688, // Rifter Blueprint
                owner_id: corp_id,
                location_id: jita_station,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );

        // Corporation standings from faction (500001 Caldari State), NPC corp (1000035 Caldari Navy), and SDE agent
        snap.standing_transactions
            .entry(ceo_id)
            .or_default()
            .extend([
                StandingTransactionRecord {
                    event_id: 1,
                    from_id: sde_agent_id,
                    to_id: corp_id,
                    modification: 2.5,
                    event_type_id: 1,
                },
                StandingTransactionRecord {
                    event_id: 2,
                    from_id: 1_000_035,
                    to_id: corp_id,
                    modification: 4.75,
                    event_type_id: 1,
                },
                StandingTransactionRecord {
                    event_id: 3,
                    from_id: 500_001,
                    to_id: corp_id,
                    modification: 6.1,
                    event_type_id: 1,
                },
            ]);
    });

    let mut sess = SessionState::new(9850);
    sess.user_id = Some(1);
    sess.role = eve_machonet::service_roles::ROLE_PLAYER_DEFAULT;
    sess.character_id = Some(ceo_id);
    sess.corporation_id = Some(corp_id);
    sess.station_id = Some(jita_station);
    sess.solar_system_id = Some(30_000_142);
    sess.corp_role = ROLE_DIRECTOR | ROLE_FACTORY_MANAGER | ROLE_STATION_MANAGER;

    // Drive ME research (8 runs -> ME 8), TE research (8 runs -> TE 16), and BPC copy (5 runs) via live RPCs.
    for (activity_id, runs) in [(4_i64, 8_i64), (3_i64, 8_i64)] {
        let job_dict = PyRep::Dict(vec![
            (PyRep::String("blueprintID".into()), PyRep::Integer(bpo_id)),
            (PyRep::String("activityID".into()), PyRep::Integer(activity_id)),
            (PyRep::String("runs".into()), PyRep::Integer(runs)),
            (PyRep::String("facilityID".into()), PyRep::Integer(jita_station)),
            (PyRep::String("useCorp".into()), PyRep::Bool(true)),
        ]);
        let job_rep = rpc_call(&ctx, &mut sess, "industryManager", "InstallJob", &[job_dict]).await;
        let job_id = job_rep.dict_get("jobID").and_then(|v| v.as_i64()).expect("research jobID");
        let dur = job_rep.dict_get("durationSeconds").and_then(|v| v.as_i64()).unwrap_or(100_000) as u64;
        ctx.db.clock().advance_secs(dur + 1);
        rpc_call(&ctx, &mut sess, "industryManager", "CompleteJob", &[PyRep::Integer(job_id)]).await;
    }

    let copy_dict = PyRep::Dict(vec![
        (PyRep::String("blueprintID".into()), PyRep::Integer(bpo_id)),
        (PyRep::String("activityID".into()), PyRep::Integer(5)),
        (PyRep::String("runs".into()), PyRep::Integer(5)),
        (PyRep::String("facilityID".into()), PyRep::Integer(jita_station)),
        (PyRep::String("useCorp".into()), PyRep::Bool(true)),
    ]);
    let copy_rep = rpc_call(&ctx, &mut sess, "industryManager", "InstallJob", &[copy_dict]).await;
    let copy_job_id = copy_rep.dict_get("jobID").and_then(|v| v.as_i64()).expect("copy jobID");
    let copy_dur = copy_rep.dict_get("durationSeconds").and_then(|v| v.as_i64()).unwrap_or(100_000) as u64;
    ctx.db.clock().advance_secs(copy_dur + 1);
    let copy_done =
        rpc_call(&ctx, &mut sess, "industryManager", "CompleteJob", &[PyRep::Integer(copy_job_id)]).await;
    let bpc_id = copy_done
        .dict_get("createdBpcItemID")
        .and_then(|v| v.as_i64())
        .expect("createdBpcItemID");

    let token = issue_token_with_scopes(
        &ctx,
        ceo_id,
        vec![
            "esi-corporations.read_blueprints.v1".to_string(),
            "esi-corporations.read_divisions.v1".to_string(),
            "esi-corporations.read_standings.v1".to_string(),
        ],
    );

    // 1. Blueprints (`GET /corporations/{id}/blueprints`, `feat:esi.corporation.blueprints`)
    let bps = check_cached_get(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", format!("/v2/corporations/{corp_id}/blueprints/"))
            .with_bearer_token(&token),
        200,
        true,
    );
    let bp_arr = bps.as_array().expect("blueprints array");
    assert_eq!(bp_arr.len(), 2);
    assert_eq!(bp_arr[0]["item_id"], json!(bpo_id));
    assert_eq!(bp_arr[0]["type_id"], json!(688));
    assert_eq!(bp_arr[0]["location_flag"], json!("CorpSAG1"));
    assert_eq!(bp_arr[0]["quantity"], json!(-1));
    assert_eq!(bp_arr[0]["runs"], json!(-1));
    assert_eq!(bp_arr[0]["material_efficiency"], json!(8));
    assert_eq!(bp_arr[0]["time_efficiency"], json!(16));
    assert_eq!(bp_arr[1]["item_id"], json!(bpc_id));
    assert_eq!(bp_arr[1]["location_flag"], json!("CorpSAG1"));
    assert_eq!(bp_arr[1]["quantity"], json!(-2));
    assert_eq!(bp_arr[1]["runs"], json!(5));
    assert_eq!(bp_arr[1]["material_efficiency"], json!(8));
    assert_eq!(bp_arr[1]["time_efficiency"], json!(16));
    assert_eq!(
        check_cached_get(
            &router,
            &spec,
            &EsiHttpRequest::new(
                "GET",
                format!("/v2/corporations/{corp_id}/blueprints/?page=2"),
            )
            .with_bearer_token(&token),
            404,
            false,
        )["status"],
        json!(404)
    );

    // 2. Divisions (`GET /corporations/{id}/divisions` — separate hangar_divisions vs wallet_divisions)
    let divs_resp = router.handle_request(
        &EsiHttpRequest::new("GET", format!("/v1/corporations/{corp_id}/divisions/"))
            .with_bearer_token(&token),
    );
    assert_eq!(divs_resp.status_code, 200);

    // 3. Standings (`GET /corporations/{id}/standings`, `feat:esi.corporation.standings`)
    let standings = check_cached_get(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", format!("/v1/corporations/{corp_id}/standings/"))
            .with_bearer_token(&token),
        200,
        true,
    );
    let st_arr = standings.as_array().expect("standings array");
    assert_eq!(st_arr.len(), 3);
    assert!(st_arr.iter().any(|s| {
        s["from_id"] == json!(sde_agent_id)
            && s["from_type"] == json!("agent")
            && s["standing"] == json!(2.5)
    }));
    assert!(st_arr.iter().any(|s| {
        s["from_id"] == json!(1_000_035)
            && s["from_type"] == json!("npc_corp")
            && s["standing"] == json!(4.75)
    }));
    assert!(st_arr.iter().any(|s| {
        s["from_id"] == json!(500_001)
            && s["from_type"] == json!("faction")
            && s["standing"] == json!(6.1)
    }));
}

#[tokio::test]
async fn test_esi5_corporation_structures_facilities_and_starbases() {
    // parity: esi:GET /corporations/{corporation_id}/facilities, esi:GET /corporations/{corporation_id}/starbases
    // parity: feat:esi.corporation.facilities, feat:esi.corporation.starbases
    eve_testkit::attest(&[
        "esi:GET /corporations/{corporation_id}/facilities",
        "esi:GET /corporations/{corporation_id}/starbases",
        "feat:esi.corporation.facilities",
        "feat:esi.corporation.starbases",
    ]);

    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);
    let router = EsiRouter::new(Arc::clone(&ctx));
    let spec = OpenApiSpec::pinned();

    let ceo_id = 90_000_001_i64;
    let corp_id = 98_500_030_i64;
    let struct_id = 1_035_000_030_001_i64;
    let undamaged_struct_id = 1_035_000_030_002_i64;
    let svc_online_id = 885_201_i64;
    let svc_offline_id = 885_202_i64;
    let starbase_id = 885_203_i64;
    let non_tower_pos_id = 885_204_i64;
    let timer_start_unix = 1_710_000_000_u64;
    let timer_end_unix = 1_710_010_800_u64;
    let next_apply_unix = 1_710_090_000_u64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&ceo_id) {
            ch.user_id = 1;
            ch.corporation_id = corp_id;
        }
        snap.corporations.insert(
            corp_id,
            CorporationRecord {
                corporation_id: corp_id,
                name: "Upwell Deep Space Engineering".to_string(),
                ticker: "UDSE".to_string(),
                ceo_id,
                tax_rate: 0.05,
                members: HashSet::from([ceo_id]),
                hq_station_id: Some(60_003_760),
                ..Default::default()
            },
        );

        snap.structures.insert(
            struct_id,
            UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 35_825,
                owner_corp_id: corp_id,
                solar_system_id: 30_000_144,
                name: "Perimeter Raitaru Citadel".to_string(),
                position: [12_000.0, -8_000.0, 44_000.0],
                state: "ArmorReinforced".to_string(),
                quantum_core_installed: true,
                fuel_blocks: 300,
                shield_hp: 0.0,
                armor_hp: 4_500.0,
                hull_hp: 5_000.0,
                reinforced_until_unix: timer_end_unix,
                last_damage_tick: 1,
                last_damage_unix: timer_start_unix,
                self_destruct_at_unix: None,
            },
        );
        snap.structure_vulnerability_windows
            .insert(struct_id, (16 * 3600, next_apply_unix));

        snap.structures.insert(
            undamaged_struct_id,
            UpwellStructureRecord {
                structure_id: undamaged_struct_id,
                type_id: 35_832,
                owner_corp_id: corp_id,
                solar_system_id: 30_000_144,
                name: "Perimeter Astrahus Bastion".to_string(),
                position: [-15_000.0, 4_000.0, 12_000.0],
                state: "ShieldVulnerable".to_string(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                shield_hp: 5_000.0,
                armor_hp: 5_000.0,
                hull_hp: 5_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
        snap.structure_vulnerability_windows
            .insert(undamaged_struct_id, (18 * 3600, next_apply_unix));

        snap.items.insert(
            svc_online_id,
            InventoryItemRecord {
                item_id: svc_online_id,
                type_id: 35_894,
                owner_id: corp_id,
                location_id: struct_id,
                flag: InventoryFlag::ServiceSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(svc_online_id, true);
        snap.items.insert(
            svc_offline_id,
            InventoryItemRecord {
                item_id: svc_offline_id,
                type_id: 35_892,
                owner_id: corp_id,
                location_id: struct_id,
                flag: InventoryFlag::ServiceSlot1,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });

    let token = issue_token_with_scopes(
        &ctx,
        ceo_id,
        vec![
            "esi-corporations.read_facilities.v1".to_string(),
            "esi-corporations.read_starbases.v1".to_string(),
            "esi-corporations.read_structures.v1".to_string(),
        ],
    );

    let facilities = check_cached_get(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", format!("/v1/corporations/{corp_id}/facilities/"))
            .with_bearer_token(&token),
        200,
        false,
    );
    assert_eq!(
        facilities,
        json!([{
            "facility_id": struct_id,
            "type_id": 35_825,
            "system_id": 30_000_144
        }])
    );

    let structures = check_cached_get(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", format!("/v4/corporations/{corp_id}/structures/"))
            .with_bearer_token(&token),
        200,
        true,
    );
    let st_arr = structures.as_array().expect("structures array");
    assert_eq!(st_arr.len(), 2);
    let st = &st_arr[0];
    assert_eq!(st["structure_id"], json!(struct_id));
    assert_eq!(st["type_id"], json!(35_825));
    assert_eq!(st["corporation_id"], json!(corp_id));
    assert_eq!(st["system_id"], json!(30_000_144));
    assert_eq!(st["name"], json!("Perimeter Raitaru Citadel"));
    assert_eq!(st["state"], json!("armor_reinforce"));
    assert_eq!(st["reinforce_hour"], json!(16));
    assert_eq!(
        st["next_reinforce_hour"],
        json!(((next_apply_unix / 3600) % 24) as i64)
    );
    assert_eq!(
        st["state_timer_start"],
        json!(format_iso8601_utc(timer_start_unix))
    );
    assert_eq!(
        st["state_timer_end"],
        json!(format_iso8601_utc(timer_end_unix))
    );
    assert_eq!(
        st["next_reinforce_apply"],
        json!(format_iso8601_utc(next_apply_unix))
    );
    assert!(st.get("fuel_expires").is_none());

    let svcs = st["services"].as_array().expect("structure services array");
    assert_eq!(svcs.len(), 2);
    let sde_svc0_name = &ctx.sde.types.get(&35_894).expect("35894 in SDE").name;
    let sde_svc1_name = &ctx.sde.types.get(&35_892).expect("35892 in SDE").name;
    assert_eq!(
        svcs[0],
        json!({ "name": sde_svc0_name, "state": "online" })
    );
    assert_eq!(
        svcs[1],
        json!({ "name": sde_svc1_name, "state": "offline" })
    );

    let st2 = &st_arr[1];
    assert_eq!(st2["structure_id"], json!(undamaged_struct_id));
    assert_eq!(st2["reinforce_hour"], json!(18));
    assert!(st2.get("state_timer_start").is_none());
    assert!(st2.get("state_timer_end").is_none());
    assert!(st2.get("fuel_expires").is_none());

    let sb_empty = check_cached_get(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", format!("/v1/corporations/{corp_id}/starbases/"))
            .with_bearer_token(&token),
        200,
        true,
    );
    assert_eq!(sb_empty, json!([]));

    let tower_type = ctx.sde.types.get(&12_235).expect("12235 in SDE");
    assert_eq!(tower_type.category_id, 23);
    assert_eq!(tower_type.group_id, 365);
    let non_tower_pos_type_id = ctx
        .sde
        .types
        .values()
        .find(|t| t.category_id == 23 && t.group_id != 365)
        .expect("non-tower category-23 POS module in SDE")
        .type_id;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            starbase_id,
            InventoryItemRecord {
                item_id: starbase_id,
                type_id: 12_235,
                owner_id: corp_id,
                location_id: 30_000_144,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            non_tower_pos_id,
            InventoryItemRecord {
                item_id: non_tower_pos_id,
                type_id: non_tower_pos_type_id,
                owner_id: corp_id,
                location_id: 30_000_144,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });

    let sb_populated = check_cached_get(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", format!("/v1/corporations/{corp_id}/starbases/"))
            .with_bearer_token(&token),
        200,
        true,
    );
    assert_eq!(
        sb_populated,
        json!([{
            "starbase_id": starbase_id,
            "type_id": 12_235,
            "system_id": 30_000_144,
            "state": "offline"
        }])
    );
}
