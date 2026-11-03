//! Part 2 of end-to-end integration tests for corporation member role audit and
//! corporation structure reinforce default.
//!
//! Parity rows:
//! - `feat:corporation.member-audit`
//! - `feat:corporation.structure-reinforce-default`

#![forbid(unsafe_code)]

use std::collections::HashMap;

use super::parity_corp_combat_batch_closure::{
    field, rowset_rows, rpc, select_pilot, spawn_ship_in_space, PILOT_1, PILOT_2,
};
use crate::common;
use eve_db::corp_roles;
use eve_destiny::DamageVector;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use glam::DVec3;

#[tokio::test]
async fn test_feat_corporation_member_audit_e2e() {
    // parity: feat:corporation.member-audit
    eve_testkit::attest(&["feat:corporation.member-audit"]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let mut sess1 = select_pilot(&ctx, 64_201, "demo", "demo-secret", PILOT_1).await;
    let mut sess2 = select_pilot(&ctx, 64_202, "other", "other-secret", PILOT_2).await;

    let created = rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "AddCorporation",
        &[
            PyRep::String("Audit Test Corp".into()),
            PyRep::String("AUDT".into()),
            PyRep::Real(0.02),
        ],
    )
    .await
    .expect("AddCorporation");
    let corp_id = field(&created, "corporationID").as_i64().expect("corp_id");
    ctx.advance_secs(7 * 86_400 + 10);

    let director_id = 90_000_003i64;
    ctx.db.with_mut_snapshot(|s| {
        let mut dir_char = s.characters.get(&PILOT_2).unwrap().clone();
        dir_char.character_id = director_id;
        dir_char.name = "Director Three".into();
        dir_char.corporation_id = corp_id;
        s.characters.insert(director_id, dir_char);
        let corp = s.corporations.get_mut(&corp_id).unwrap();
        corp.members.insert(PILOT_2);
        corp.members.insert(director_id);
        s.characters.get_mut(&PILOT_2).unwrap().corporation_id = corp_id;
    });
    sess2.corporation_id = Some(corp_id);

    rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "UpdateMember",
        &[
            PyRep::Integer(director_id),
            PyRep::Integer(corp_roles::ROLE_DIRECTOR as i64),
        ],
    )
    .await
    .expect("Grant Director");

    ctx.advance_secs(120);
    let t1 = ctx.now_unix() as i64;
    let mut sess_dir = SessionState::new(64_203);
    sess_dir.user_id = Some(3);
    sess_dir.character_id = Some(director_id);
    sess_dir.corporation_id = Some(corp_id);
    sess_dir.corp_role = corp_roles::ROLE_DIRECTOR;

    let first_roles = (corp_roles::ROLE_ACCOUNTANT | corp_roles::ROLE_STATION_MANAGER) as i64;
    rpc(
        &ctx,
        &mut sess_dir,
        "corpRegistry",
        "UpdateMember",
        &[
            PyRep::Integer(PILOT_2),
            PyRep::Integer(first_roles),
        ],
    )
    .await
    .expect("Director UpdateMember");

    ctx.advance_secs(300);
    let t2 = ctx.now_unix() as i64;
    let second_roles = (first_roles as u64 | corp_roles::ROLE_PERSONNEL_MANAGER) as i64;
    rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "UpdateMember",
        &[
            PyRep::Integer(PILOT_2),
            PyRep::Integer(second_roles),
        ],
    )
    .await
    .expect("CEO UpdateMember");

    let audit = rpc(&ctx, &mut sess1, "corpmgr", "AuditMember", &[PyRep::Integer(PILOT_2)])
        .await
        .expect("AuditMember");
    let rows = rowset_rows(&audit);
    assert_eq!(rows.len(), 2, "AuditMember must return both role changes for PILOT_2");
    assert_eq!(
        rows[0],
        vec![
            PyRep::Integer(PILOT_2),
            PyRep::Integer(director_id),
            PyRep::Integer(0),
            PyRep::Integer(first_roles),
            PyRep::Integer(t1),
        ]
    );
    assert_eq!(
        rows[1],
        vec![
            PyRep::Integer(PILOT_2),
            PyRep::Integer(PILOT_1),
            PyRep::Integer(first_roles),
            PyRep::Integer(second_roles),
            PyRep::Integer(t2),
        ]
    );

    let mut pp = HashMap::new();
    pp.insert("corporation_id".to_string(), corp_id.to_string());
    let esi_rsp = eve_server::esi::sample_routes::dispatch_operation(
        &ctx,
        ctx.now_unix(),
        "GET",
        "/corporations/{corporation_id}/roles/history",
        &pp,
        &HashMap::new(),
        None,
        director_id as usize,
        &[],
    );
    assert_eq!(esi_rsp.status_code, 200, "ESI roles/history must return 200");
    let arr = esi_rsp.body.expect("json").as_array().cloned().expect("array");
    assert!(arr.len() >= 3, "ESI roles/history must include all corp role changes");
}

#[tokio::test]
async fn test_feat_corporation_structure_reinforce_default_e2e() {
    // parity: feat:corporation.structure-reinforce-default
    eve_testkit::attest(&["feat:corporation.structure-reinforce-default"]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let mut sess1 = select_pilot(&ctx, 64_301, "demo", "demo-secret", PILOT_1).await;
    let mut sess2 = select_pilot(&ctx, 64_302, "other", "other-secret", PILOT_2).await;

    let created = rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "AddCorporation",
        &[
            PyRep::String("Citadel Builders".into()),
            PyRep::String("CITB".into()),
            PyRep::Real(0.03),
        ],
    )
    .await
    .expect("AddCorporation");
    let corp_id = field(&created, "corporationID").as_i64().expect("corp_id");
    ctx.advance_secs(7 * 86_400 + 10);

    let perimeter_sys = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| s.security_status > 0.0 && s.security_status < 0.45 && s.region_id != 10_000_070)
        .expect("lowsec system")
        .solar_system_id;
    let ship1 = ctx.db.with_mut_snapshot(|s| {
        s.corporations.get_mut(&corp_id).unwrap().members.insert(PILOT_2);
        s.characters.get_mut(&PILOT_2).unwrap().corporation_id = corp_id;
        s.characters.get(&PILOT_1).unwrap().active_ship_id
    });
    sess2.corporation_id = Some(corp_id);
    sess1.station_id = None;
    sess1.solar_system_id = Some(perimeter_sys);

    let err = rpc(
        &ctx,
        &mut sess2,
        "corpRegistry",
        "SetStructureReinforceDefault",
        &[PyRep::Integer(19)],
    )
    .await
    .expect_err("Non-director SetStructureReinforceDefault must fail");
    assert_eq!(err.code, "CorpRoleDenied");

    let set_rep = rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "SetStructureReinforceDefault",
        &[PyRep::Integer(19)],
    )
    .await
    .expect("SetStructureReinforceDefault");
    assert_eq!(set_rep, PyRep::Integer(19));

    let get_rep = rpc(
        &ctx,
        &mut sess2,
        "corpRegistry",
        "GetStructureReinforceDefault",
        &[],
    )
    .await
    .expect("GetStructureReinforceDefault");
    assert_eq!(get_rep, PyRep::Integer(19));

    spawn_ship_in_space(&ctx, PILOT_1, ship1, perimeter_sys, DVec3::new(50_000.0, 0.0, 0.0), None).await;
    ctx.db.with_mut_snapshot(|s| {
        s.next_entity_id += 1;
        let astra_id = s.next_entity_id;
        s.items.insert(
            astra_id,
            eve_db::InventoryItemRecord {
                item_id: astra_id,
                type_id: 35_832,
                owner_id: PILOT_1,
                location_id: ship1,
                flag: eve_db::InventoryFlag::Cargo,
                quantity: 1,
                singleton: false,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });
    let perimeter_handle = ctx.systems.get_or_spawn(perimeter_sys);

    let anchor_rep = rpc(
        &ctx,
        &mut sess1,
        "structureDeployment",
        "Anchor",
        &[
            PyRep::Integer(35_832),
            PyRep::List(vec![
                PyRep::Real(100_000.0),
                PyRep::Real(0.0),
                PyRep::Real(0.0),
            ]),
            PyRep::Integer(0),
            PyRep::None,
            PyRep::String("Corp Astrahus".into()),
        ],
    )
    .await
    .expect("Anchor");
    let structure_id = anchor_rep
        .as_i64()
        .unwrap_or_else(|| field(&anchor_rep, "structureID").as_i64().expect("structureID"));

    let core_type = eve_server::services::station_structure::structure_lifecycle::structure_profile(&ctx, 35_832)
        .and_then(|p| p.core_type_id)
        .unwrap_or(56_201);
    ctx.db.with_mut_snapshot(|s| {
        s.items.insert(
            105_000_002,
            eve_db::InventoryItemRecord {
                item_id: 105_000_002,
                type_id: core_type,
                owner_id: PILOT_1,
                location_id: structure_id,
                flag: eve_db::InventoryFlag::StructureDeedBay,
                quantity: 1,
                singleton: false,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });
    ctx.advance_secs(eve_server::services::station_structure::structure_lifecycle::ANCHOR_VULNERABLE_SECS + 10);
    eve_server::services::station_structure::structure_lifecycle::sync_all_structures(&ctx);
    ctx.advance_secs(86_400 + 10);
    eve_server::services::station_structure::structure_lifecycle::sync_all_structures(&ctx);
    ctx.advance_secs(eve_server::services::station_structure::structure_lifecycle::ONLINING_SECS + 10);
    eve_server::services::station_structure::structure_lifecycle::sync_all_structures(&ctx);

    let (adopted_hour, state) = ctx.db.with_snapshot(|s| {
        let w = s.structure_vulnerability_windows.get(&structure_id).copied().expect("vuln window");
        let st = s.structures.get(&structure_id).expect("structure");
        (w.0, st.state.clone())
    });
    assert_eq!(
        state,
        eve_server::services::station_structure::structure_lifecycle::STATE_SHIELD_VULNERABLE,
        "Newly onlined structure must be ShieldVulnerable"
    );
    assert_eq!(adopted_hour, 19, "Newly onlined corp structure must adopt corp default reinforce hour 19");

    ctx.db.with_mut_snapshot(|s| {
        if let Some(c2) = s.characters.get_mut(&PILOT_2) {
            c2.corporation_id = 1_000_009;
        }
    });
    eve_server::services::station_structure::structure_lifecycle_rpc::apply_damage_structure_or_ball(
        &ctx,
        &perimeter_handle,
        perimeter_sys,
        PILOT_2,
        structure_id,
        DamageVector { em: 10_000_000.0, thermal: 0.0, kinetic: 0.0, explosive: 0.0 },
        1.0,
        ctx.now_unix(),
    )
    .await
    .expect("damage structure");

    let reinforced_until = ctx.db.with_snapshot(|s| {
        s.structures.get(&structure_id).unwrap().reinforced_until_unix
    });
    assert_eq!(
        (reinforced_until / 3_600) % 24,
        19,
        "Structure reinforce timer must exit at adopted corp default hour 19 UTC"
    );
}
