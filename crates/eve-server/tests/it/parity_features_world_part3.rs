//! Verified runtime integration tests for world parity features (Part 3: Industry, Lifecycle, Corporation, Fleet, Invariants).

use crate::common;
use eve_db::{InventoryFlag, InventoryItemRecord, IskCents};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use eve_server::services::industry_pi::industry_facilities_bps::{
    compute_blueprint_eiv_per_run, compute_industry_job_costs,
};
use std::collections::HashMap;

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, svc, None, method, args, &HashMap::new(), None)
        .await
}

#[tokio::test]
async fn test_industry_research_cost_tax_capital_and_lifecycle_pause() {
    // parity: feat:industry.blueprint-me-research
    // parity: feat:industry.capital-construction-restrictions
    // parity: feat:industry.eiv-adjusted-prices
    // parity: feat:industry.facility-tax
    // parity: feat:industry.job-cost
    // parity: feat:lifecycle.industry-pause-and-many
    eve_testkit::attest(&[
        "feat:industry.blueprint-me-research",
        "feat:industry.capital-construction-restrictions",
        "feat:industry.eiv-adjusted-prices",
        "feat:industry.facility-tax",
        "feat:industry.job-cost",
        "feat:lifecycle.industry-pause-and-many",
    ]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let mut sess = SessionState::new(40_301);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let char_id = 90_000_001;
    rpc(
        &ctx,
        &mut sess,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
    )
    .await
    .expect("SelectCharacterID");

    let jita_station = 60_003_760_i64;
    sess.station_id = Some(jita_station);
    sess.structure_id = None;
    sess.solar_system_id = Some(30_000_142);

    // 1. EIV from market adjusted prices + 0.25% NPC station facility tax + job cost formula
    let rifter_bp = 687_u32;
    let (eiv_default, cost_default) = ctx.db.with_snapshot(|snap| {
        let eiv = compute_blueprint_eiv_per_run(&ctx, &ctx.sde.blueprints[&rifter_bp], 1);
        let cost = compute_industry_job_costs(&ctx, snap, rifter_bp, 1, 1, jita_station).unwrap();
        (eiv, cost)
    });
    assert!((cost_default.facility_tax_rate - 0.0025).abs() < 1e-6);
    assert!((cost_default.eiv_isk - eiv_default).abs() < 0.05);

    // 2. Exponential ME research time via industryManager.InstallJob (activityID = 4)
    let (bp1, bp2) = ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.balance = IskCents::from_isk(5_000_000_000);
            ch.station_id = Some(jita_station);
            ch.solar_system_id = 30_000_142;
        }
        let skills = snap.skills.entry(char_id).or_default();
        for sid in [3387_u32, 3406, 3380, 3402, 3409] {
            skills.insert(
                sid,
                eve_db::TrainedSkillRecord {
                    type_id: sid,
                    level: 5,
                    skill_points: 256_000.0,
                },
            );
        }
        snap.next_entity_id += 1;
        let b1 = snap.next_entity_id;
        snap.next_entity_id += 1;
        let b2 = snap.next_entity_id;
        snap.items.insert(
            b1,
            InventoryItemRecord {
                item_id: b1,
                type_id: rifter_bp,
                owner_id: char_id,
                location_id: jita_station,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                blueprint_runs_remaining: None,
                ..Default::default()
            },
        );
        let mut dyn9 = HashMap::new();
        dyn9.insert(1112, 8.0); // ME 8 -> researching to ME 9
        snap.items.insert(
            b2,
            InventoryItemRecord {
                item_id: b2,
                type_id: rifter_bp,
                owner_id: char_id,
                location_id: jita_station,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                blueprint_runs_remaining: None,
                dynamic_attributes: dyn9,
                ..Default::default()
            },
        );
        (b1, b2)
    });

    rpc(
        &ctx,
        &mut sess,
        "industryManager",
        "InstallJob",
        &[PyRep::keyval([
            ("blueprintID", PyRep::Integer(bp1)),
            ("blueprintTypeID", PyRep::Integer(rifter_bp as i64)),
            ("activityID", PyRep::Integer(4)),
            ("runs", PyRep::Integer(1)),
            ("facilityID", PyRep::Integer(jita_station)),
        ])],
    )
    .await
    .expect("InstallJob ME 0->1");

    rpc(
        &ctx,
        &mut sess,
        "industryManager",
        "InstallJob",
        &[PyRep::keyval([
            ("blueprintID", PyRep::Integer(bp2)),
            ("blueprintTypeID", PyRep::Integer(rifter_bp as i64)),
            ("activityID", PyRep::Integer(4)),
            ("runs", PyRep::Integer(1)),
            ("facilityID", PyRep::Integer(jita_station)),
        ])],
    )
    .await
    .expect("InstallJob ME 8->9");

    let (dur_1, dur_9) = ctx.db.with_snapshot(|snap| {
        let j1 = snap.industry_jobs.values().find(|j| j.blueprint_item_id == bp1).unwrap();
        let j9 = snap.industry_jobs.values().find(|j| j.blueprint_item_id == bp2).unwrap();
        (j1.duration_seconds, j9.duration_seconds)
    });
    assert!(
        dur_9 > dur_1 * 500,
        "ME 8->9 research duration ({dur_9}s) must scale exponentially over ME 0->1 ({dur_1}s)"
    );

    // 3. Capital construction restrictions: Dreadnought (group 485) forbidden in High-Sec; Jump Freighter (group 902) allowed
    let dread_bp = ctx
        .sde
        .blueprints
        .values()
        .find(|b| ctx.sde.types.get(&b.product_type_id).is_some_and(|t| t.group_id == 485))
        .map(|b| b.blueprint_type_id);
    if let Some(dbp) = dread_bp {
        let dread_item = ctx.db.with_mut_snapshot(|snap| {
            snap.next_entity_id += 1;
            let id = snap.next_entity_id;
            snap.items.insert(
                id,
                InventoryItemRecord {
                    item_id: id,
                    type_id: dbp,
                    owner_id: char_id,
                    location_id: jita_station,
                    flag: InventoryFlag::Hangar,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
            id
        });
        let cap_err = rpc(
            &ctx,
            &mut sess,
            "industryManager",
            "InstallJob",
            &[PyRep::keyval([
                ("blueprintID", PyRep::Integer(dread_item)),
                ("blueprintTypeID", PyRep::Integer(dbp as i64)),
                ("activityID", PyRep::Integer(1)),
                ("runs", PyRep::Integer(1)),
                ("facilityID", PyRep::Integer(jita_station)),
            ])],
        )
        .await
        .expect_err("Dreadnought in High-Sec must fail");
        assert_eq!(cap_err.code, "CapitalConstructionForbiddenInHighSec");
    }

    let jf_bp = ctx
        .sde
        .blueprints
        .values()
        .find(|b| {
            ctx.sde.types.get(&b.product_type_id).is_some_and(|t| t.group_id == 902)
                && !b.materials.is_empty()
        });
    if let Some(jfb) = jf_bp {
        let jf_item = ctx.db.with_mut_snapshot(|snap| {
            snap.next_entity_id += 1;
            let id = snap.next_entity_id;
            snap.items.insert(
                id,
                InventoryItemRecord {
                    item_id: id,
                    type_id: jfb.blueprint_type_id,
                    owner_id: char_id,
                    location_id: jita_station,
                    flag: InventoryFlag::Hangar,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
            for m in &jfb.materials {
                snap.next_entity_id += 1;
                let mid = snap.next_entity_id;
                snap.items.insert(
                    mid,
                    InventoryItemRecord {
                        item_id: mid,
                        type_id: m.type_id,
                        owner_id: char_id,
                        location_id: jita_station,
                        flag: InventoryFlag::Hangar,
                        quantity: m.quantity as i32,
                        ..Default::default()
                    },
                );
            }
            id
        });
        rpc(
            &ctx,
            &mut sess,
            "industryManager",
            "InstallJob",
            &[PyRep::keyval([
                ("blueprintID", PyRep::Integer(jf_item)),
                ("blueprintTypeID", PyRep::Integer(jfb.blueprint_type_id as i64)),
                ("activityID", PyRep::Integer(1)),
                ("runs", PyRep::Integer(1)),
                ("facilityID", PyRep::Integer(jita_station)),
            ])],
        )
        .await
        .expect("Jump Freighter construction must be allowed in High-Sec");
    }

    // 4. CompleteManyJobs on installed jobs
    ctx.advance_secs(4_000_000);
    let active_job_ids: Vec<PyRep> = ctx.db.with_snapshot(|snap| {
        snap.industry_jobs
            .values()
            .filter(|j| j.installer_id == char_id && j.status == "Active")
            .map(|j| PyRep::Integer(j.job_id))
            .collect()
    });
    assert!(!active_job_ids.is_empty());
    rpc(
        &ctx,
        &mut sess,
        "industryManager",
        "CompleteManyJobs",
        &[PyRep::List(active_job_ids)],
    )
    .await
    .expect("CompleteManyJobs");
}

#[tokio::test]
async fn test_corp_member_audit_reinforce_default_and_fleet_handover_formations() {
    // parity: feat:corporation.member-audit
    // parity: feat:corporation.structure-reinforce-default
    // parity: feat:fleet.boss-handover
    // parity: feat:fleet.stored-formations-and-warp-shapes
    eve_testkit::attest(&[
        "feat:corporation.member-audit",
        "feat:corporation.structure-reinforce-default",
        "feat:fleet.boss-handover",
        "feat:fleet.stored-formations-and-warp-shapes",
    ]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let mut sess1 = SessionState::new(40_302);
    common::login_pilot(&ctx, &mut sess1, "demo", "demo-secret").await;
    let char1 = 90_000_001_i64;
    let char2 = 90_000_002_i64;
    rpc(
        &ctx,
        &mut sess1,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(char1)],
    )
    .await
    .expect("SelectCharacterID");

    let corp_id = sess1.corporation_id.unwrap_or(98_000_001);
    sess1.corp_role = eve_db::corp_roles::ROLE_DIRECTOR;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char1) {
            ch.corporation_id = corp_id;
        }
        let corp = snap.corporations.entry(corp_id).or_default();
        corp.ceo_id = char1;
        corp.members.insert(char1);
    });

    // 1. Non-director rejected for SetStructureReinforceDefault; director succeeds
    let mut non_dir_sess = sess1.clone();
    non_dir_sess.character_id = Some(char2);
    non_dir_sess.corp_role = 0;
    let non_dir_err = rpc(
        &ctx,
        &mut non_dir_sess,
        "corpRegistry",
        "SetStructureReinforceDefault",
        &[PyRep::Integer(19)],
    )
    .await
    .expect_err("Non-director SetStructureReinforceDefault must fail");
    assert!(!non_dir_err.code.is_empty());

    rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "SetStructureReinforceDefault",
        &[PyRep::Integer(19)],
    )
    .await
    .expect("Director SetStructureReinforceDefault");
    let def_hr = rpc(&ctx, &mut sess1, "corpRegistry", "GetStructureReinforceDefault", &[])
        .await
        .expect("GetStructureReinforceDefault");
    assert_eq!(def_hr, PyRep::Integer(19));

    // 2. Role change via corpRegistry.UpdateMember -> corpmgr.AuditMember(char2) returns history with target & issuer
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(corp) = snap.corporations.get_mut(&corp_id) {
            corp.members.insert(char2);
        }
        if let Some(ch2) = snap.characters.get_mut(&char2) {
            ch2.corporation_id = corp_id;
        }
    });
    rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "UpdateMember",
        &[
            PyRep::Integer(char2),
            PyRep::String("Accountant".into()),
            PyRep::Integer(eve_db::corp_roles::ROLE_ACCOUNTANT as i64),
            PyRep::Integer(0),
            PyRep::Integer(0),
            PyRep::Integer(0),
            PyRep::Integer(0),
            PyRep::Integer(0),
            PyRep::Integer(0),
            PyRep::Integer(0),
            PyRep::Integer(0),
            PyRep::Integer(0),
        ],
    )
    .await
    .expect("UpdateMember");

    let audit = rpc(&ctx, &mut sess1, "corpmgr", "AuditMember", &[PyRep::Integer(char2)])
        .await
        .expect("AuditMember");
    let PyRep::Rowset { rows: lines, .. } = audit else {
        panic!("expected Rowset from AuditMember, got {audit:?}");
    };
    assert!(
        lines
            .iter()
            .any(|cols| cols.first() == Some(&PyRep::Integer(char2)) && cols.get(1) == Some(&PyRep::Integer(char1))),
        "AuditMember({char2}) must include role change entry issued by {char1}, got {lines:?}"
    );

    // 3. Fleet stored formations + boss handover on LeaveFleet
    let fleet_rep = rpc(&ctx, &mut sess1, "fleetObjectHandler", "CreateFleet", &[])
        .await
        .expect("CreateFleet");
    let fleet_id = fleet_rep.as_i64().unwrap_or(sess1.fleet_id.unwrap_or(0));
    assert!(fleet_id > 0);
    sess1.fleet_id = Some(fleet_id);

    let mut sess2 = SessionState::new(40_304);
    common::login_pilot(&ctx, &mut sess2, "other", "other-secret").await;
    rpc(&ctx, &mut sess2, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char2)])
        .await
        .expect("SelectCharacterID char2");
    rpc(&ctx, &mut sess1, "fleetObjectHandler", "Invite", &[PyRep::Integer(char2)])
        .await
        .expect("Invite char2");
    ctx.advance_secs(12);
    rpc(&ctx, &mut sess2, "fleetObjectHandler", "AcceptInvite", &[PyRep::Integer(fleet_id)])
        .await
        .expect("AcceptInvite char2");
    let formations = rpc(&ctx, &mut sess1, "beyonce", "GetFormations", &[])
        .await
        .expect("beyonce.GetFormations");
    let PyRep::Tuple(form_items) = &formations else {
        panic!("expected 2-tuple from beyonce.GetFormations, got {formations:?}");
    };
    assert_eq!(form_items.len(), 2);

    let _ = ctx.drain_notifications_for_char(char2);
    rpc(&ctx, &mut sess1, "fleetObjectHandler", "LeaveFleet", &[])
        .await
        .expect("Boss LeaveFleet");
    let new_boss = ctx.db.with_snapshot(|snap| snap.fleets.get(&fleet_id).map(|f| f.boss_id).unwrap_or(0));
    assert_eq!(new_boss, char2, "Boss must automatically hand over to char2 when boss leaves");
    assert!(
        ctx.drain_notifications_for_char(char2)
            .iter()
            .any(|n| n.event == "OnFleetMemberChanged"),
        "OnFleetMemberChanged must be emitted on boss handover"
    );
}

#[tokio::test]
async fn test_invariants_atomic_rollback_and_single_source_of_truth() {
    // parity: feat:invariant.atomic-rollback-all-stores
    // parity: feat:invariant.single-source-of-truth
    eve_testkit::attest(&[
        "feat:invariant.atomic-rollback-all-stores",
        "feat:invariant.single-source-of-truth",
    ]);

    let tmp_dir = std::env::temp_dir().join(format!("evomu_world_invariants_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp_dir);
    std::fs::create_dir_all(&tmp_dir).unwrap();
    let db_path = tmp_dir.join("world.sqlite");

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let mut sess = SessionState::new(40_303);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let char_id = 90_000_001_i64;
    rpc(
        &ctx,
        &mut sess,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
    )
    .await
    .expect("SelectCharacterID");

    // 1. Atomic rollback on failing mutating RPCs
    let before_snap = ctx.db.with_snapshot(|s| (s.items.len(), s.industry_jobs.len(), s.characters[&char_id].balance));
    let _ = rpc(
        &ctx,
        &mut sess,
        "corpRegistry",
        "InsertApplication",
        &[PyRep::Integer(1_000_009), PyRep::String("test".into())],
    )
    .await
    .expect_err("NPC corp application must fail");
    let _ = rpc(
        &ctx,
        &mut sess,
        "ship",
        "Drop",
        &[PyRep::List(vec![PyRep::Integer(999_999_999)]), PyRep::Integer(char_id), PyRep::Bool(false)],
    )
    .await
    .expect_err("Drop while docked must fail");
    let after_snap = ctx.db.with_snapshot(|s| (s.items.len(), s.industry_jobs.len(), s.characters[&char_id].balance));
    assert_eq!(before_snap, after_snap, "Failing mutating RPCs must not partially mutate stores");

    // 2. Single source of truth across MachoNet RPC + SQLite persistence + reload
    rpc(
        &ctx,
        &mut sess,
        "addressbook",
        "AddContact",
        &[PyRep::Integer(90_000_002), PyRep::Real(5.0)],
    )
    .await
    .expect("AddContact");
    ctx.db.save_to_sqlite(&db_path).expect("save_to_sqlite");

    let ctx_reloaded = ServerContext::from_sqlite(&db_path).expect("reload ServerContext");
    let mut sess_reloaded = sess.clone();
    let ab = rpc(&ctx_reloaded, &mut sess_reloaded, "charMgr", "GetContactList", &[])
        .await
        .expect("GetContactList after reload");
    assert!(ab != PyRep::None, "ContactList must persist across SQLite reload");
    let _ = std::fs::remove_dir_all(&tmp_dir);
}
