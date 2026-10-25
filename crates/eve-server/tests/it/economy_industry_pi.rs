//! Industry (`E-4`) and Planetary Interaction (`E-5`, `E-6`) integration tests.
//!
//! Covers:
//! - E-4: Industry jobs engine (`blueprintManager`, `facilityManager`, `industryManager`)
//! - E-5 / E-6: Planetary Interaction (`planetMgr` colonies, command centers, pins, links, extraction, schematics, customs)



use crate::common;
use eve_db::{InventoryFlag, InventoryItemRecord, IskCents, TrainedSkillRecord, TypeId};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use std::collections::HashMap;

const JITA_STATION_ID: i64 = 60_003_760;
const JITA_PLANET_ID: i64 = 40_009_082;
const TRITANIUM_TYPE_ID: TypeId = 34;
const RIFTER_BP_ITEM_ID: i64 = 100_000_007;
const RIFTER_BP_TYPE_ID: TypeId = 691;
const COMMAND_CENTER_UPGRADES_SKILL: TypeId = 2505;
const INTERPLANETARY_CONSOLIDATION_SKILL: TypeId = 2495;

async fn setup_pilot(
    ctx: &ServerContext,
    session_id: u64,
    username: &str,
    password: &str,
    char_id: i64,
) -> SessionState {
    let kw = HashMap::new();
    let mut sess = SessionState::new(session_id);
    common::login_pilot(ctx, &mut sess, username, password).await;
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");
    sess
}

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let kw = HashMap::new();
    ctx.dispatch_rpc(sess, service, None, method, args, &kw, None)
        .await
}

#[tokio::test]
async fn test_e4_industry_jobs_lifecycle_manufacturing_research_and_copying() {
    // parity: feat:industry.manufacturing, feat:industry.research-me, feat:industry.research-te, feat:industry.copying, plan:E-4
    eve_testkit::attest(&["feat:industry.manufacturing", "feat:industry.research-me", "feat:industry.research-te", "feat:industry.copying"]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut s1 = setup_pilot(&ctx, 401, "demo", "demo-secret", 90_000_001).await;

    // 1. Manufacturing Job (activity_id = 1)
    let trit_before = ctx.db.with_snapshot(|snap| {
        snap.items
            .values()
            .find(|i| {
                i.owner_id == 90_000_001
                    && i.location_id == JITA_STATION_ID
                    && i.flag == InventoryFlag::Hangar
                    && i.type_id == TRITANIUM_TYPE_ID
            })
            .map(|i| i.quantity)
            .unwrap_or(0)
    });
    assert!(trit_before >= 32_000, "Ensure pilot has enough Tritanium for manufacturing");

    let s1_bal_before_job = ctx.db.get_character(90_000_001).unwrap().balance;

    let mfg_install = rpc(
        &ctx,
        &mut s1,
        "industryManager",
        "InstallJob",
        &[
            PyRep::Integer(RIFTER_BP_TYPE_ID as i64),
            PyRep::Integer(1), // Manufacturing
            PyRep::Integer(1), // 1 run
        ],
    )
    .await
    .expect("InstallJob Manufacturing");

    let PyRep::KeyVal(mfg_kv) = mfg_install else { panic!("Expected KeyVal"); };
    let mfg_job_id = mfg_kv.get("jobID").and_then(|v| v.as_i64()).unwrap();
    let tax_charged = mfg_kv.get("facilityTaxISK").and_then(|v| v.as_f64()).unwrap();
    assert_eq!(tax_charged, 1_000.0);

    let s1_bal_after_install = ctx.db.get_character(90_000_001).unwrap().balance;
    assert_eq!(s1_bal_before_job.0 - s1_bal_after_install.0, 100_000);
    let has_tax_journal = ctx.db.with_snapshot(|snap| {
        snap.wallet_journal.iter().any(|j| {
            j.character_id == 90_000_001 && j.entry_type == "IndustryFacilityTax"
        })
    });
    assert!(has_tax_journal, "Wallet journal must record IndustryFacilityTax");

    let bp_flag = ctx.db.with_snapshot(|snap| { snap.items.get(&RIFTER_BP_ITEM_ID).map(|i| i.flag) });
    assert_eq!(bp_flag, Some(InventoryFlag::Locked));

    let jobs_rep = rpc(&ctx, &mut s1, "industryManager", "GetJobsByOwner", &[])
        .await
        .expect("GetJobsByOwner");
    let PyRep::List(jobs_list) = jobs_rep else { panic!("Expected List"); };
    assert!(jobs_list.iter().any(|j| match j {
        PyRep::KeyVal(kv) => kv.get("jobID").and_then(|v| v.as_i64()) == Some(mfg_job_id),
        _ => false,
    }));

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(j) = snap.industry_jobs.get_mut(&mfg_job_id) { j.start_unix = 0; }
    });

    let complete_mfg = rpc(&ctx, &mut s1, "industryManager", "CompleteJob", &[PyRep::Integer(mfg_job_id)],)
    .await
    .expect("CompleteJob Manufacturing");
    let PyRep::KeyVal(cmfg_kv) = complete_mfg else { panic!("Expected KeyVal"); };
    let delivered_ship_id = cmfg_kv.get("deliveredItemID").and_then(|v| v.as_i64()).unwrap();
    assert_eq!(cmfg_kv.get("productTypeID"), Some(&PyRep::Integer(587)));

    let ship_in_hangar = ctx.db.with_snapshot(|snap| {
        snap.items.get(&delivered_ship_id).map(|i| {
            i.owner_id == 90_000_001
                && i.location_id == JITA_STATION_ID
                && i.flag == InventoryFlag::Hangar
                && i.type_id == 587
        })
    });
    assert_eq!(ship_in_hangar, Some(true));

    let bp_unlocked = ctx.db.with_snapshot(|snap| { snap.items.get(&RIFTER_BP_ITEM_ID).map(|i| i.flag) });
    assert_eq!(bp_unlocked, Some(InventoryFlag::Hangar));

    // 2. Research TE (activity 3)
    let te_install = rpc(
        &ctx,
        &mut s1,
        "industryManager",
        "InstallJob",
        &[PyRep::Integer(RIFTER_BP_TYPE_ID as i64), PyRep::Integer(3), PyRep::Integer(1),],
    )
    .await
    .expect("InstallJob TE");
    let PyRep::KeyVal(te_kv) = te_install else { panic!("Expected KeyVal"); };
    let te_job_id = te_kv.get("jobID").and_then(|v| v.as_i64()).unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(j) = snap.industry_jobs.get_mut(&te_job_id) { j.start_unix = 0; }
    });
    rpc(&ctx, &mut s1, "industryManager", "CompleteJob", &[PyRep::Integer(te_job_id)])
        .await
        .expect("CompleteJob TE");
    let te_level = ctx.db.with_snapshot(|snap| {
        snap.items
            .get(&RIFTER_BP_ITEM_ID)
            .and_then(|bp| bp.dynamic_attributes.get(&1113).copied())
    });
    assert_eq!(te_level, Some(2.0));

    // 3. Research ME (activity 4)
    let me_install = rpc(
        &ctx,
        &mut s1,
        "industryManager",
        "InstallJob",
        &[PyRep::Integer(RIFTER_BP_TYPE_ID as i64), PyRep::Integer(4), PyRep::Integer(1),],
    )
    .await
    .expect("InstallJob ME");
    let PyRep::KeyVal(me_kv) = me_install else { panic!("Expected KeyVal"); };
    let me_job_id = me_kv.get("jobID").and_then(|v| v.as_i64()).unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(j) = snap.industry_jobs.get_mut(&me_job_id) { j.start_unix = 0; }
    });
    rpc(&ctx, &mut s1, "industryManager", "CompleteJob", &[PyRep::Integer(me_job_id)])
        .await
        .expect("CompleteJob ME");
    let me_level = ctx.db.with_snapshot(|snap| {
        snap.items
            .get(&RIFTER_BP_ITEM_ID)
            .and_then(|bp| bp.dynamic_attributes.get(&1112).copied())
    });
    assert_eq!(me_level, Some(1.0));

    // 4. Copying (activity 5)
    let copy_install = rpc(
        &ctx,
        &mut s1,
        "industryManager",
        "InstallJob",
        &[PyRep::Integer(RIFTER_BP_TYPE_ID as i64), PyRep::Integer(5), PyRep::Integer(3),],
    )
    .await
    .expect("InstallJob Copying");
    let PyRep::KeyVal(copy_kv) = copy_install else { panic!("Expected KeyVal"); };
    let copy_job_id = copy_kv.get("jobID").and_then(|v| v.as_i64()).unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(j) = snap.industry_jobs.get_mut(&copy_job_id) { j.start_unix = 0; }
    });
    let complete_copy = rpc(
        &ctx,
        &mut s1,
        "industryManager",
        "CompleteJob",
        &[PyRep::Integer(copy_job_id)],
    )
    .await
    .expect("CompleteJob Copying");
    let PyRep::KeyVal(ccopy_kv) = complete_copy else { panic!("Expected KeyVal"); };
    let bpc_item_id = ccopy_kv.get("deliveredItemID").and_then(|v| v.as_i64()).unwrap();
    let bpc_record = ctx.db.with_snapshot(|snap| snap.items.get(&bpc_item_id).cloned()).unwrap();
    assert!(bpc_record.is_blueprint_copy);
    assert_eq!(bpc_record.blueprint_runs_remaining, Some(3));
    assert_eq!(bpc_record.type_id, RIFTER_BP_TYPE_ID);
}

#[tokio::test]
async fn test_e5_planetary_interaction_lifecycle() {
    // parity: feat:pi.extractor-program, feat:pi.factory-cycles, feat:pi.routes
    eve_testkit::attest(&["feat:pi.extractor-program", "feat:pi.factory-cycles", "feat:pi.routes"]);
    let ctx = ServerContext::new();
    let mut s1 = setup_pilot(&ctx, 501, "demo", "demo-secret", 90_000_001).await;

    let s1_ship_id = s1.ship_id.expect("s1 active ship");
    s1.station_id = None;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) { ch.station_id = None; }
        snap.next_entity_id += 1;
        let id = snap.next_entity_id;
        snap.items.insert(
            id,
            InventoryItemRecord {
                item_id: id,
                type_id: 2534,
                owner_id: 90_000_001,
                location_id: s1_ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });
    let create_colony = rpc(
        &ctx,
        &mut s1,
        "planetMgr",
        "UserUpdateNetwork",
        &[
            PyRep::Integer(JITA_PLANET_ID),
            PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::Integer(1),
                PyRep::Integer(1),
                PyRep::Integer(2534),
                PyRep::Real(0.1),
                PyRep::Real(0.2),
            ])]),
        ],
    )
    .await
    .expect("UserUpdateNetwork");
    s1.station_id = Some(JITA_STATION_ID);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) { ch.station_id = Some(JITA_STATION_ID); }
    });
    let PyRep::KeyVal(col_kv) = create_colony else { panic!("Expected KeyVal"); };
    assert_eq!(col_kv.get("commandCenterLevel"), Some(&PyRep::Integer(0)));
    assert_eq!(col_kv.get("pinsCount"), Some(&PyRep::Integer(1)));
    // After `UserUpdateNetwork` returns, the V24 client's `ClientPlanet._SubmitChanges` scatters
    // these itself; the server must not send them.
    let pi_notifs = ctx.drain_notifications_for_char(90_000_001);
    for local in [
        "OnPlanetChangesSubmitted",
        "OnColonyPinCountUpdated",
        "OnPlanetCommandCenterDeployedOrRemoved",
    ] {
        assert!(pi_notifs.iter().all(|n| n.event != local), "{local} is client-local");
    }

    ctx.db.with_mut_snapshot(|snap| {
        snap.skills
            .entry(90_000_001)
            .or_default()
            .remove(&COMMAND_CENTER_UPGRADES_SKILL);
    });

    let upgrade_err = rpc(
        &ctx,
        &mut s1,
        "planetMgr",
        "UpgradeCommandCenter",
        &[PyRep::Integer(JITA_PLANET_ID), PyRep::Integer(2)],
    )
    .await
    .unwrap_err();
    assert_eq!(upgrade_err.code, "MissingSkillForCommandCenterUpgrade");

    ctx.db.with_mut_snapshot(|snap| {
        snap.skills
            .entry(90_000_001)
            .or_default()
            .insert(
                COMMAND_CENTER_UPGRADES_SKILL,
                TrainedSkillRecord {
                    type_id: COMMAND_CENTER_UPGRADES_SKILL,
                    level: 3,
                    skill_points: 256_000.0,
                },
            );
    });

    let s1_bal_before_up = ctx.db.get_character(90_000_001).unwrap().balance;
    let upgrade_res = rpc(
        &ctx,
        &mut s1,
        "planetMgr",
        "UpgradeCommandCenter",
        &[PyRep::Integer(JITA_PLANET_ID), PyRep::Integer(2)],
    )
    .await
    .expect("UpgradeCommandCenter level 2");
    let PyRep::KeyVal(up_kv) = upgrade_res else { panic!("Expected KeyVal"); };
    assert_eq!(up_kv.get("commandCenterLevel"), Some(&PyRep::Integer(2)));
    assert_eq!(up_kv.get("pinsCount"), Some(&PyRep::Integer(3)));

    let s1_bal_after_up = ctx.db.get_character(90_000_001).unwrap().balance;
    assert_eq!(s1_bal_before_up.0 - s1_bal_after_up.0, 151_000_000);

    let has_cc_journal = ctx.db.with_snapshot(|snap| {
        snap.wallet_journal.iter().any(|j| {
            j.character_id == 90_000_001 && j.entry_type == "PlanetCommandCenterUpgrade"
        })
    });
    assert!(has_cc_journal);

    let p1_type: TypeId = 3645;
    ctx.db.with_mut_snapshot(|snap| {
        let mut net = eve_server::services::industry_pi::load_pi_network(snap, 90_000_001, JITA_PLANET_ID, 2);
        if let Some(pin) = net.pins.first_mut() {
            pin.contents.insert(p1_type, 40);
        }
        eve_server::services::industry_pi::save_pi_network(snap, 90_000_001, JITA_PLANET_ID, &net);
    });

    let route_res = rpc(
        &ctx,
        &mut s1,
        "planetMgr",
        "UserLaunchCommodities",
        &[
            PyRep::Integer(JITA_PLANET_ID),
            PyRep::Integer(p1_type as i64),
            PyRep::Integer(40),
            PyRep::String("Hangar".into()),
        ],
    )
    .await
    .expect("UserLaunchCommodities");
    let PyRep::KeyVal(route_kv) = route_res else { panic!("Expected KeyVal"); };
    assert_eq!(route_kv.get("routedQuantity"), Some(&PyRep::Integer(40)));

    let hangar_water = ctx.db.with_snapshot(|snap| {
        snap.items
            .values()
            .filter(|i| {
                i.owner_id == 90_000_001
                    && i.location_id == JITA_STATION_ID
                    && i.flag == InventoryFlag::Hangar
                    && i.type_id == p1_type
            })
            .map(|i| i.quantity)
            .sum::<i32>()
    });
    assert!(hangar_water >= 40);
}

#[tokio::test]
async fn test_industry_blueprint_data_facilities_jobs() {
    // parity: feat:industry.blueprint-data, feat:industry.facilities, feat:industry.facility-tax, feat:industry.install-job, feat:industry.job-cost
    eve_testkit::attest(&["feat:industry.blueprint-data", "feat:industry.facilities", "feat:industry.facility-tax", "feat:industry.install-job", "feat:industry.job-cost"]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut s1 = setup_pilot(&ctx, 402, "demo", "demo-secret", 90_000_001).await;

    let (rifter_bpo_id, rifter_bpc_id, raitaru_id) = ctx.db.with_mut_snapshot(|snap| {
        let corp_id = snap.characters[&90_000_001].corporation_id;
        snap.next_entity_id += 1;
        let bpo_id = snap.next_entity_id;
        snap.items.insert(
            bpo_id,
            InventoryItemRecord {
                item_id: bpo_id,
                type_id: RIFTER_BP_TYPE_ID,
                owner_id: 90_000_001,
                location_id: JITA_STATION_ID,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
                dynamic_attributes: HashMap::from([(1112, 8.0), (1113, 16.0)]),
                version: 1,
                ..Default::default()
            },
        );
        snap.next_entity_id += 1;
        let struct_id = snap.next_entity_id;
        snap.items.insert(
            struct_id,
            InventoryItemRecord {
                item_id: struct_id,
                type_id: 35825, // Raitaru
                owner_id: corp_id,
                location_id: 30_000_142,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.next_entity_id += 1;
        let bpc_id = snap.next_entity_id;
        snap.items.insert(
            bpc_id,
            InventoryItemRecord {
                item_id: bpc_id,
                type_id: RIFTER_BP_TYPE_ID,
                owner_id: 90_000_001,
                location_id: struct_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                is_blueprint_copy: true,
                blueprint_runs_remaining: Some(5),
                dynamic_attributes: HashMap::from([(1112, 10.0), (1113, 20.0)]),
                version: 1,
                ..Default::default()
            },
        );
        snap.skills.entry(90_000_001).or_default().remove(&3387);
        snap.skills.entry(90_000_001).or_default().remove(&24625);
        (bpo_id, bpc_id, struct_id)
    });

    // 1. GetBlueprintData & GetBlueprintDataByOwner return BPO/BPC, ME, TE, runs remaining, and location
    let bp_data = rpc(&ctx, &mut s1, "blueprintManager", "GetBlueprintData", &[PyRep::Integer(rifter_bpo_id)])
        .await
        .expect("blueprintManager.GetBlueprintData BPO");
    assert_eq!(bp_data.dict_get("blueprintID").and_then(|v| v.as_i64()), Some(rifter_bpo_id));
    assert_eq!(bp_data.dict_get("blueprintTypeID").and_then(|v| v.as_i64()), Some(RIFTER_BP_TYPE_ID as i64));
    assert_eq!(bp_data.dict_get("productTypeID").and_then(|v| v.as_i64()), Some(587));
    assert_eq!(bp_data.dict_get("original").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(bp_data.dict_get("materialEfficiency").and_then(|v| v.as_i64()), Some(8));
    assert_eq!(bp_data.dict_get("timeEfficiency").and_then(|v| v.as_i64()), Some(16));
    assert_eq!(bp_data.dict_get("runsRemaining").and_then(|v| v.as_i64()), Some(-1));
    assert_eq!(bp_data.dict_get("facilityID").and_then(|v| v.as_i64()), Some(JITA_STATION_ID));
    assert_eq!(bp_data.dict_get("locationID").and_then(|v| v.as_i64()), Some(JITA_STATION_ID));
    let PyRep::List(bp_mats) = bp_data.dict_get("materials").expect("materials") else { panic!("Expected List") };
    assert!(bp_mats.iter().any(|m| m.dict_get("typeID").and_then(|v| v.as_i64()) == Some(TRITANIUM_TYPE_ID as i64) && m.dict_get("quantity").and_then(|v| v.as_i64()) == Some(32_000)));
    let PyRep::List(bp_skills) = bp_data.dict_get("skills").expect("skills") else { panic!("Expected List") };
    assert!(bp_skills.iter().any(|sk| sk.dict_get("typeID").and_then(|v| v.as_i64()) == Some(3380) && sk.dict_get("level").and_then(|v| v.as_i64()) == Some(1)));

    let owner_bps = rpc(&ctx, &mut s1, "blueprintManager", "GetBlueprintDataByOwner", &[PyRep::Integer(90_000_001)])
        .await
        .expect("GetBlueprintDataByOwner");
    let PyRep::List(owner_bp_list) = owner_bps else { panic!("Expected List") };
    let bpo_entry = owner_bp_list.iter().find(|b| b.dict_get("itemID").and_then(|v| v.as_i64()) == Some(rifter_bpo_id)).expect("BPO in GetBlueprintDataByOwner");
    assert_eq!(bpo_entry.dict_get("original").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(bpo_entry.dict_get("materialEfficiency").and_then(|v| v.as_i64()), Some(8));
    assert_eq!(bpo_entry.dict_get("timeEfficiency").and_then(|v| v.as_i64()), Some(16));
    assert_eq!(bpo_entry.dict_get("runsRemaining").and_then(|v| v.as_i64()), Some(-1));
    assert_eq!(bpo_entry.dict_get("facilityID").and_then(|v| v.as_i64()), Some(JITA_STATION_ID));
    let bpc_entry = owner_bp_list.iter().find(|b| b.dict_get("itemID").and_then(|v| v.as_i64()) == Some(rifter_bpc_id)).expect("BPC in GetBlueprintDataByOwner");
    assert_eq!(bpc_entry.dict_get("original").and_then(|v| v.as_bool()), Some(false));
    assert_eq!(bpc_entry.dict_get("materialEfficiency").and_then(|v| v.as_i64()), Some(10));
    assert_eq!(bpc_entry.dict_get("timeEfficiency").and_then(|v| v.as_i64()), Some(20));
    assert_eq!(bpc_entry.dict_get("runsRemaining").and_then(|v| v.as_i64()), Some(5));
    assert_eq!(bpc_entry.dict_get("facilityID").and_then(|v| v.as_i64()), Some(raitaru_id));

    let limits_rep = rpc(&ctx, &mut s1, "blueprintManager", "GetLimits", &[]).await.expect("GetLimits");
    assert_eq!(limits_rep.dict_get("maxManufacturingJobs").and_then(|v| v.as_i64()), Some(1));

    // 2. GetFacilities / GetFacility return NPC stations and structures with activities, bonuses, and taxes
    let facilities_rep = rpc(&ctx, &mut s1, "facilityManager", "GetFacilities", &[]).await.expect("GetFacilities");
    let PyRep::List(fac_list) = facilities_rep else { panic!("Expected List") };
    let npc_fac = fac_list.iter().find(|f| f.dict_get("facilityID").and_then(|v| v.as_i64()) == Some(JITA_STATION_ID)).expect("NPC station in GetFacilities");
    assert_eq!(npc_fac.dict_get("materialModifier").and_then(|v| v.as_f64()), Some(1.0));
    assert_eq!(npc_fac.dict_get("timeModifier").and_then(|v| v.as_f64()), Some(1.0));
    assert_eq!(npc_fac.dict_get("costModifier").and_then(|v| v.as_f64()), Some(1.0));
    assert_eq!(npc_fac.dict_get("tax").and_then(|v| v.as_f64()), Some(0.10));
    let PyRep::List(npc_acts) = npc_fac.dict_get("activities").expect("NPC activities") else { panic!("Expected List") };
    assert!(npc_acts.contains(&PyRep::Integer(1)) && npc_acts.contains(&PyRep::Integer(8)));
    let struct_fac_in_list = fac_list.iter().find(|f| f.dict_get("facilityID").and_then(|v| v.as_i64()) == Some(raitaru_id)).expect("Structure in GetFacilities");
    assert_eq!(struct_fac_in_list.dict_get("typeID").and_then(|v| v.as_i64()), Some(35825));

    let raitaru_fac = rpc(&ctx, &mut s1, "facilityManager", "GetFacility", &[PyRep::Integer(raitaru_id)]).await.expect("GetFacility Raitaru");
    assert_eq!(raitaru_fac.dict_get("facilityID").and_then(|v| v.as_i64()), Some(raitaru_id));
    assert_eq!(raitaru_fac.dict_get("typeID").and_then(|v| v.as_i64()), Some(35825));
    assert_eq!(raitaru_fac.dict_get("costModifier").and_then(|v| v.as_f64()), Some(0.97));
    assert_eq!(raitaru_fac.dict_get("materialModifier").and_then(|v| v.as_f64()), Some(0.99));
    assert_eq!(raitaru_fac.dict_get("timeModifier").and_then(|v| v.as_f64()), Some(0.85));
    let PyRep::List(r_acts) = raitaru_fac.dict_get("activities").expect("Raitaru activities") else { panic!("Expected List") };
    assert!(r_acts.contains(&PyRep::Integer(1)));

    let max_mods = rpc(&ctx, &mut s1, "facilityManager", "GetMaxActivityModifiers", &[]).await.expect("GetMaxActivityModifiers");
    assert_eq!(max_mods.dict_get("materialMultiplier").and_then(|v| v.as_f64()), Some(0.99));

    let neg_tax_err = rpc(
        &ctx,
        &mut s1,
        "facilityManager",
        "SetFacilityTaxes",
        &[PyRep::Integer(raitaru_id), PyRep::Dict(vec![(PyRep::Integer(1), PyRep::Real(-0.02))])],
    )
    .await
    .unwrap_err();
    assert_eq!(neg_tax_err.code, "InvalidFacilityTaxRate");

    rpc(
        &ctx,
        &mut s1,
        "facilityManager",
        "SetFacilityTaxes",
        &[PyRep::Integer(raitaru_id), PyRep::Dict(vec![(PyRep::Integer(1), PyRep::Real(0.05)), (PyRep::Integer(4), PyRep::Real(0.025))])],
    )
    .await
    .expect("SetFacilityTaxes");

    let taxes_rep = rpc(&ctx, &mut s1, "facilityManager", "GetFacilityTaxes", &[PyRep::Integer(raitaru_id), PyRep::None]).await.expect("GetFacilityTaxes");
    assert!((taxes_rep.dict_get("1").and_then(|v| v.as_f64()).unwrap() - 0.05).abs() < 1e-6);
    assert!((taxes_rep.dict_get("4").and_then(|v| v.as_f64()).unwrap() - 0.025).abs() < 1e-6);

    // 3. InstallJob validations: remote range, skills, materials, wallet, blueprint ownership/lock, and BOM consumption
    let remote_err = rpc(
        &ctx,
        &mut s1,
        "industryManager",
        "InstallJob",
        &[PyRep::keyval([
            ("blueprintID", PyRep::Integer(rifter_bpo_id)),
            ("blueprintTypeID", PyRep::Integer(RIFTER_BP_TYPE_ID as i64)),
            ("facilityID", PyRep::Integer(60_008_494)), // Amarr VIII
            ("activityID", PyRep::Integer(1)),
            ("runs", PyRep::Integer(1)),
        ])],
    )
    .await
    .unwrap_err();
    assert_eq!(remote_err.code, "FacilityOutOfRemoteRange");

    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(90_000_001).or_default().remove(&3380);
    });
    let skill_err = rpc(
        &ctx,
        &mut s1,
        "industryManager",
        "InstallJob",
        &[PyRep::keyval([
            ("blueprintID", PyRep::Integer(rifter_bpo_id)),
            ("blueprintTypeID", PyRep::Integer(RIFTER_BP_TYPE_ID as i64)),
            ("facilityID", PyRep::Integer(raitaru_id)),
            ("activityID", PyRep::Integer(1)),
            ("runs", PyRep::Integer(1)),
        ])],
    )
    .await
    .unwrap_err();
    assert_eq!(skill_err.code, "MissingIndustrySkills");
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(90_000_001).or_default().insert(
            3380,
            TrainedSkillRecord { type_id: 3380, level: 5, skill_points: 256_000.0 },
        );
    });

    let trit_before_all = ctx.db.with_snapshot(|snap| {
        snap.items
            .values()
            .filter(|i| i.owner_id == 90_000_001 && i.location_id == JITA_STATION_ID && i.type_id == TRITANIUM_TYPE_ID)
            .map(|i| i.quantity)
            .sum::<i32>()
    });
    let mat_err = rpc(
        &ctx,
        &mut s1,
        "industryManager",
        "InstallJob",
        &[PyRep::keyval([
            ("blueprintID", PyRep::Integer(rifter_bpo_id)),
            ("blueprintTypeID", PyRep::Integer(RIFTER_BP_TYPE_ID as i64)),
            ("facilityID", PyRep::Integer(raitaru_id)),
            ("activityID", PyRep::Integer(1)),
            ("runs", PyRep::Integer(1_000)),
        ])],
    )
    .await
    .unwrap_err();
    assert_eq!(mat_err.code, "InsufficientManufacturingMaterials");

    let s1_bal_before_job = ctx.db.get_character(90_000_001).unwrap().balance;
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&90_000_001).unwrap().balance = IskCents(100);
    });
    let wallet_err = rpc(
        &ctx,
        &mut s1,
        "industryManager",
        "InstallJob",
        &[PyRep::keyval([
            ("blueprintID", PyRep::Integer(rifter_bpo_id)),
            ("blueprintTypeID", PyRep::Integer(RIFTER_BP_TYPE_ID as i64)),
            ("facilityID", PyRep::Integer(raitaru_id)),
            ("activityID", PyRep::Integer(1)),
            ("runs", PyRep::Integer(2)),
        ])],
    )
    .await
    .unwrap_err();
    assert_eq!(wallet_err.code, "InsufficientFunds");
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&90_000_001).unwrap().balance = s1_bal_before_job;
    });

    let job_arg = PyRep::keyval([
        ("blueprintID", PyRep::Integer(rifter_bpo_id)),
        ("blueprintTypeID", PyRep::Integer(RIFTER_BP_TYPE_ID as i64)),
        ("facilityID", PyRep::Integer(raitaru_id)),
        ("solarSystemID", PyRep::Integer(30_000_142)),
        ("activityID", PyRep::Integer(1)),
        ("runs", PyRep::Integer(2)),
        ("productTypeID", PyRep::Integer(587)),
        ("licensedRuns", PyRep::Integer(2)),
        ("status", PyRep::Integer(1)),
    ]);

    let mfg_install = rpc(&ctx, &mut s1, "industryManager", "InstallJob", &[job_arg])
        .await
        .expect("InstallJob V24");
    let PyRep::KeyVal(mfg_kv) = mfg_install else { panic!("Expected KeyVal"); };
    let mfg_job_id = mfg_kv.get("jobID").and_then(|v| v.as_i64()).unwrap();
    let job_dur = mfg_kv.get("durationSeconds").and_then(|v| v.as_i64()).unwrap() as u64;
    let eiv = mfg_kv.get("estimatedItemValue").and_then(|v| v.as_f64()).unwrap();
    let sci = mfg_kv.get("systemCostIndex").and_then(|v| v.as_f64()).unwrap();
    let struct_mod = mfg_kv.get("structureCostModifier").and_then(|v| v.as_f64()).unwrap();
    let base_job_cost = mfg_kv.get("baseJobCost").and_then(|v| v.as_f64()).unwrap();
    let fac_tax_isk = mfg_kv.get("facilityTaxISK").and_then(|v| v.as_f64()).unwrap();
    let scc_isk = mfg_kv.get("sccSurchargeISK").and_then(|v| v.as_f64()).unwrap();
    let total_cost_isk = mfg_kv.get("totalJobCost").and_then(|v| v.as_f64()).unwrap();
    let expected_trit = mfg_kv.get("tritaniumRequired").and_then(|v| v.as_i64()).unwrap() as i32;

    // Verify blueprint locked on install & materials consumed on install
    let (bp_flag_during, trit_after_install) = ctx.db.with_snapshot(|snap| {
        let f = snap.items.get(&rifter_bpo_id).map(|i| i.flag);
        let t = snap
            .items
            .values()
            .filter(|i| i.owner_id == 90_000_001 && i.location_id == JITA_STATION_ID && i.type_id == TRITANIUM_TYPE_ID)
            .map(|i| i.quantity)
            .sum::<i32>();
        (f, t)
    });
    assert_eq!(bp_flag_during, Some(InventoryFlag::Locked));
    assert_eq!(trit_before_all - trit_after_install, expected_trit);

    assert!(eiv > 0.0);
    assert!(sci > 0.0 && sci <= 0.50);
    assert!((struct_mod - 0.97).abs() < 1e-6);
    let expected_base = (eiv * sci * 0.97 * 2.0 * 100.0).round() / 100.0;
    let expected_fac_tax = (expected_base * 0.05 * 100.0).round() / 100.0;
    let expected_scc = (expected_base * 0.04 * 100.0).round() / 100.0;
    let expected_total = ((expected_base + expected_fac_tax + expected_scc) * 100.0).round() / 100.0;
    assert!((base_job_cost - expected_base).abs() < 0.02);
    assert!((fac_tax_isk - expected_fac_tax).abs() < 0.02);
    assert!((scc_isk - expected_scc).abs() < 0.02);
    assert!((total_cost_isk - expected_total).abs() < 0.02);

    let s1_bal_after_job = ctx.db.get_character(90_000_001).unwrap().balance;
    let debited_isk = (s1_bal_before_job.0 - s1_bal_after_job.0) as f64 / 100.0;
    assert!((debited_isk - total_cost_isk).abs() < 0.02);

    // Verify job cost components are journaled in wallet_journal
    let (has_base_j, has_tax_j, has_scc_j) = ctx.db.with_snapshot(|snap| {
        (
            snap.wallet_journal.iter().any(|e| e.character_id == 90_000_001 && e.entry_type == "IndustryJobInstallationFee" && e.amount == IskCents::from_f64_isk(base_job_cost)),
            snap.wallet_journal.iter().any(|e| e.character_id == 90_000_001 && e.entry_type == "IndustryFacilityTax" && e.amount == IskCents::from_f64_isk(fac_tax_isk)),
            snap.wallet_journal.iter().any(|e| e.character_id == 90_000_001 && e.entry_type == "IndustrySccSurcharge" && e.amount == IskCents::from_f64_isk(scc_isk)),
        )
    });
    assert!(has_base_j && has_tax_j && has_scc_j, "Job base cost, facility tax, and SCC surcharge must be journaled");

    let slot_err = rpc(
        &ctx,
        &mut s1,
        "industryManager",
        "InstallJob",
        &[PyRep::keyval([
            ("blueprintID", PyRep::Integer(0)),
            ("blueprintTypeID", PyRep::Integer(RIFTER_BP_TYPE_ID as i64)),
            ("facilityID", PyRep::Integer(JITA_STATION_ID)),
            ("activityID", PyRep::Integer(1)),
            ("runs", PyRep::Integer(1)),
        ])],
    )
    .await
    .unwrap_err();
    assert_eq!(slot_err.code, "IndustrySlotsFull");

    let counts_rep = rpc(&ctx, &mut s1, "industryManager", "GetJobCounts", &[PyRep::Integer(90_000_001)])
        .await
        .expect("GetJobCounts");
    assert_eq!(counts_rep.dict_get("manufacturingUsed").and_then(|v| v.as_i64()), Some(1));

    let job_detail = rpc(&ctx, &mut s1, "industryManager", "GetJob", &[PyRep::Integer(mfg_job_id)])
        .await
        .expect("GetJob");
    assert_eq!(job_detail.dict_get("jobID").and_then(|v| v.as_i64()), Some(mfg_job_id));

    ctx.advance_secs(job_dur + 5);

    let complete_res = rpc(&ctx, &mut s1, "industryManager", "CompleteJob", &[PyRep::Integer(mfg_job_id)])
        .await
        .expect("CompleteJob");
    assert_eq!(complete_res.dict_get("productTypeID").and_then(|v| v.as_i64()), Some(587));
    let delivered_id = complete_res.dict_get("deliveredItemID").and_then(|v| v.as_i64()).unwrap();
    let delivered_item = ctx.db.with_snapshot(|snap| {
        snap.items
            .get(&delivered_id)
            .map(|i| (i.owner_id, i.location_id, i.flag, i.quantity))
    });
    assert_eq!(delivered_item, Some((90_000_001, JITA_STATION_ID, InventoryFlag::Hangar, 2)));

    // Also install a job at NPC station JITA_STATION_ID to verify fixed NPC facility tax (10%) is charged on install
    let npc_install = rpc(
        &ctx,
        &mut s1,
        "industryManager",
        "InstallJob",
        &[PyRep::keyval([
            ("blueprintID", PyRep::Integer(rifter_bpo_id)),
            ("blueprintTypeID", PyRep::Integer(RIFTER_BP_TYPE_ID as i64)),
            ("facilityID", PyRep::Integer(JITA_STATION_ID)),
            ("activityID", PyRep::Integer(1)),
            ("runs", PyRep::Integer(1)),
        ])],
    )
    .await
    .expect("InstallJob at NPC station");
    let npc_base = npc_install.dict_get("baseJobCost").and_then(|v| v.as_f64()).unwrap();
    let npc_tax = npc_install.dict_get("facilityTaxISK").and_then(|v| v.as_f64()).unwrap();
    assert!((npc_tax - (npc_base * 0.10 * 100.0).round() / 100.0).abs() < 0.02);
}

#[tokio::test]
async fn test_pi_colony_command_center_pins_and_links() {
    // parity: feat:pi.planet-info, feat:pi.command-center, feat:pi.colony-limit, feat:pi.place-pins, feat:pi.links
    eve_testkit::attest(&["feat:pi.planet-info", "feat:pi.command-center", "feat:pi.colony-limit", "feat:pi.place-pins", "feat:pi.links"]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut s1 = setup_pilot(&ctx, 502, "demo", "demo-secret", 90_000_001).await;

    let pinfo = rpc(&ctx, &mut s1, "planetMgr", "GetPlanetInfo", &[PyRep::Integer(JITA_PLANET_ID)],)
    .await
    .expect("GetPlanetInfo");
    let PyRep::KeyVal(pkv) = pinfo else { panic!("Expected KeyVal"); };
    assert_eq!(pkv.get("planetID"), Some(&PyRep::Integer(JITA_PLANET_ID)));
    assert_eq!(pkv.get("hasColony"), Some(&PyRep::Bool(false)));
    assert!(pkv.get("planetTypeID").and_then(|v| v.as_i64()).unwrap() > 0);
    assert!(pkv.get("radius").and_then(|v| v.as_f64()).unwrap() > 0.0);

    let res_info = rpc(
        &ctx,
        &mut s1,
        "planetMgr",
        "GetPlanetResourceInfo",
        &[PyRep::Integer(JITA_PLANET_ID)],
    )
    .await
    .expect("GetPlanetResourceInfo");
    let PyRep::KeyVal(res_kv) = res_info else { panic!("Expected KeyVal"); };
    let Some(PyRep::List(res_list)) = res_kv.get("resources") else { panic!("Expected resources List"); };
    assert!(!res_list.is_empty());
    let first_res_type_id = match &res_list[0] {
        PyRep::KeyVal(kv) => kv.get("resourceTypeID").and_then(|v| v.as_i64()).unwrap(),
        _ => panic!("Expected KeyVal"),
    };

    ctx.db.with_mut_snapshot(|snap| { snap.skills.entry(90_000_001).or_default().remove(&13279); });
    let scan_err = rpc(
        &ctx,
        &mut s1,
        "planetMgr",
        "GetResourceData",
        &[PyRep::keyval([
            ("planetID", PyRep::Integer(JITA_PLANET_ID)),
            ("resourceTypeID", PyRep::Integer(first_res_type_id)),
        ])],
    )
    .await
    .unwrap_err();
    assert_eq!(scan_err.code, "PISkillRequiredToSurveyPlanetResources");

    ctx.db.with_mut_snapshot(|snap| {
        let sk = snap.skills.entry(90_000_001).or_default();
        sk.insert(13279, TrainedSkillRecord { type_id: 13279, level: 3, skill_points: 45_255.0 });
        sk.insert(2406, TrainedSkillRecord { type_id: 2406, level: 4, skill_points: 135_765.0 });
        sk.insert(2403, TrainedSkillRecord { type_id: 2403, level: 2, skill_points: 7_072.0 });
    });

    let res_data = rpc(
        &ctx,
        &mut s1,
        "planetMgr",
        "GetResourceData",
        &[PyRep::keyval([
            ("planetID", PyRep::Integer(JITA_PLANET_ID)),
            ("resourceTypeID", PyRep::Integer(first_res_type_id)),
            ("advancedPlanetology", PyRep::Integer(2)),
            ("planetology", PyRep::Integer(4)),
            ("remoteSensing", PyRep::Integer(3)),
        ])],
    )
    .await
    .expect("GetResourceData");
    let PyRep::KeyVal(rd_kv) = res_data else { panic!("Expected KeyVal"); };
    assert_eq!(rd_kv.get("numBands"), Some(&PyRep::Integer(10)));
    assert_eq!(rd_kv.get("proximity"), Some(&PyRep::Integer(5)));
    assert!(matches!(rd_kv.get("data"), Some(PyRep::Bytes(b)) if !b.is_empty()));

    let docked_cc_err = rpc(
        &ctx,
        &mut s1,
        "planetMgr",
        "UserUpdateNetwork",
        &[
            PyRep::Integer(JITA_PLANET_ID),
            PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::Integer(1),
                PyRep::Tuple(vec![PyRep::Integer(0), PyRep::Integer(1)]),
                PyRep::Integer(2534),
                PyRep::Real(0.35),
                PyRep::Real(-1.10),
            ])]),
        ],
    )
    .await
    .unwrap_err();
    assert_eq!(docked_cc_err.code, "CannotPlaceCommandCenterWhileDocked");

    let s1_ship_id = s1.ship_id.expect("s1 active ship");
    s1.station_id = None;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) { ch.station_id = None; }
    });
    let missing_cc_err = rpc(
        &ctx,
        &mut s1,
        "planetMgr",
        "UserUpdateNetwork",
        &[
            PyRep::Integer(JITA_PLANET_ID),
            PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::Integer(1),
                PyRep::Tuple(vec![PyRep::Integer(0), PyRep::Integer(1)]),
                PyRep::Integer(2534),
                PyRep::Real(0.35),
                PyRep::Real(-1.10),
            ])]),
        ],
    )
    .await
    .unwrap_err();
    assert_eq!(missing_cc_err.code, "CommandCenterItemRequired");

    let mismatch_cc_err = rpc(
        &ctx,
        &mut s1,
        "planetMgr",
        "UserUpdateNetwork",
        &[
            PyRep::Integer(JITA_PLANET_ID),
            PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::Integer(1),
                PyRep::Tuple(vec![PyRep::Integer(0), PyRep::Integer(1)]),
                PyRep::Integer(2254),
                PyRep::Real(0.35),
                PyRep::Real(-1.10),
            ])]),
        ],
    )
    .await
    .unwrap_err();
    assert_eq!(mismatch_cc_err.code, "IncompatibleCommandCenterPlanetType");

    let cc_item_id = ctx.db.with_mut_snapshot(|snap| {
        snap.next_entity_id += 1;
        let id = snap.next_entity_id;
        snap.items.insert(
            id,
            InventoryItemRecord {
                item_id: id,
                type_id: 2534,
                owner_id: 90_000_001,
                location_id: s1_ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 2,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        id
    });

    let create_col = rpc(
        &ctx,
        &mut s1,
        "planetMgr",
        "UserUpdateNetwork",
        &[
            PyRep::Integer(JITA_PLANET_ID),
            PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::Integer(1),
                PyRep::Tuple(vec![PyRep::Integer(0), PyRep::Integer(1)]),
                PyRep::Integer(2534),
                PyRep::Real(0.35),
                PyRep::Real(-1.10),
            ])]),
        ],
    )
    .await
    .expect("UserUpdateNetwork place Command Center");
    let PyRep::KeyVal(col_kv) = create_col else { panic!("Expected KeyVal"); };
    assert_eq!(col_kv.get("currentLevel"), Some(&PyRep::Integer(0)));
    assert_eq!(col_kv.get("commandCenterLevel"), Some(&PyRep::Integer(0)));
    assert_eq!(col_kv.get("cpuCapacity"), Some(&PyRep::Integer(1675)));
    assert_eq!(col_kv.get("powerCapacity"), Some(&PyRep::Integer(6000)));
    assert_eq!(col_kv.get("pinsCount"), Some(&PyRep::Integer(1)));
    assert_eq!(ctx.db.get_item(cc_item_id).unwrap().quantity, 1);
    let Some(PyRep::List(cc_pins)) = col_kv.get("pins") else { panic!("Expected pins list"); };
    let PyRep::KeyVal(cc_pin_kv) = &cc_pins[0] else { panic!("Expected KeyVal"); };
    let cc_pin_id = cc_pin_kv.get("pinID").and_then(|v| v.as_i64()).unwrap();
    assert!(cc_pin_id > 1);

    let second_planet_id = 40_009_098;
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills
            .entry(90_000_001)
            .or_default()
            .remove(&INTERPLANETARY_CONSOLIDATION_SKILL);
    });
    let col_limit_err = rpc(
        &ctx,
        &mut s1,
        "planetMgr",
        "UserUpdateNetwork",
        &[
            PyRep::Integer(second_planet_id),
            PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::Integer(1),
                PyRep::Tuple(vec![PyRep::Integer(0), PyRep::Integer(1)]),
                PyRep::Integer(2534),
                PyRep::Real(0.10),
                PyRep::Real(0.20),
            ])]),
        ],
    )
    .await
    .unwrap_err();
    assert_eq!(col_limit_err.code, "MaxColoniesReached");

    s1.station_id = Some(JITA_STATION_ID);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) { ch.station_id = Some(JITA_STATION_ID); }
        snap.skills
            .entry(90_000_001)
            .or_default()
            .insert(
                COMMAND_CENTER_UPGRADES_SKILL,
                TrainedSkillRecord {
                    type_id: COMMAND_CENTER_UPGRADES_SKILL,
                    level: 3,
                    skill_points: 256_000.0,
                },
            );
    });

    let up_skill_err = rpc(
        &ctx,
        &mut s1,
        "planetMgr",
        "UserUpdateNetwork",
        &[
            PyRep::Integer(JITA_PLANET_ID),
            PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::Integer(9),
                PyRep::Integer(cc_pin_id),
                PyRep::Integer(4),
            ])]),
        ],
    )
    .await
    .unwrap_err();
    assert_eq!(up_skill_err.code, "MissingSkillForCommandCenterUpgrade");

    let s1_bal_before_up1 = ctx.db.get_character(90_000_001).unwrap().balance;
    let up1_res = rpc(
        &ctx,
        &mut s1,
        "planetMgr",
        "UserUpdateNetwork",
        &[
            PyRep::Integer(JITA_PLANET_ID),
            PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::Integer(9),
                PyRep::Integer(cc_pin_id),
                PyRep::Integer(1),
            ])]),
        ],
    )
    .await
    .expect("Upgrade CC level 1");
    let PyRep::KeyVal(up1_kv) = up1_res else { panic!("Expected KeyVal"); };
    assert_eq!(up1_kv.get("currentLevel"), Some(&PyRep::Integer(1)));
    assert_eq!(up1_kv.get("cpuCapacity"), Some(&PyRep::Integer(7057)));
    assert_eq!(up1_kv.get("powerCapacity"), Some(&PyRep::Integer(9000)));
    let s1_bal_after_up1 = ctx.db.get_character(90_000_001).unwrap().balance;
    assert_eq!(s1_bal_before_up1.0 - s1_bal_after_up1.0, 58_000_000);

    let up_res = rpc(
        &ctx,
        &mut s1,
        "planetMgr",
        "UserUpdateNetwork",
        &[
            PyRep::Integer(JITA_PLANET_ID),
            PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::Integer(9),
                PyRep::Integer(cc_pin_id),
                PyRep::Integer(3),
            ])]),
        ],
    )
    .await
    .expect("Upgrade CC level 3");
    let PyRep::KeyVal(up_kv) = up_res else { panic!("Expected KeyVal"); };
    assert_eq!(up_kv.get("currentLevel"), Some(&PyRep::Integer(3)));
    assert_eq!(up_kv.get("cpuCapacity"), Some(&PyRep::Integer(17215)));
    assert_eq!(up_kv.get("powerCapacity"), Some(&PyRep::Integer(15000)));
    let s1_bal_after_up = ctx.db.get_character(90_000_001).unwrap().balance;
    assert_eq!(s1_bal_after_up1.0 - s1_bal_after_up.0, 213_000_000);
    assert_eq!(s1_bal_before_up1.0 - s1_bal_after_up.0, 271_000_000);

    let cc_lvl = rpc(&ctx, &mut s1, "planetMgr", "GetCommandCenterLevel", &[PyRep::Integer(JITA_PLANET_ID)])
        .await
        .expect("GetCommandCenterLevel");
    assert_eq!(cc_lvl, PyRep::Integer(3));

    // Place all 6 required pin categories: Extractor Control Unit (2848), Basic factory (2469),
    // Advanced factory (2480), High-Tech factory (2482), Storage facility (2541), and Launchpad (2544),
    // plus links (including level-0 link and level-2 upgraded link) within CPU/PG budget with ISK cost
    let bal_before_pins_links = ctx.db.get_character(90_000_001).unwrap().balance;
    let net_res = rpc(
        &ctx,
        &mut s1,
        "planetMgr",
        "UserUpdateNetwork",
        &[
            PyRep::Integer(JITA_PLANET_ID),
            PyRep::List(vec![
                PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(2)]), PyRep::Integer(2848), PyRep::Real(0.36), PyRep::Real(-1.10)]),
                PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(3)]), PyRep::Integer(2469), PyRep::Real(0.37), PyRep::Real(-1.09)]),
                PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(4)]), PyRep::Integer(2480), PyRep::Real(0.38), PyRep::Real(-1.08)]),
                PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(5)]), PyRep::Integer(2482), PyRep::Real(0.39), PyRep::Real(-1.07)]),
                PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(6)]), PyRep::Integer(2541), PyRep::Real(0.40), PyRep::Real(-1.06)]),
                PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(7)]), PyRep::Integer(2544), PyRep::Real(0.41), PyRep::Real(-1.05)]),
                PyRep::Tuple(vec![PyRep::Integer(3), PyRep::Integer(cc_pin_id), PyRep::Integer(2), PyRep::Integer(0)]),
                PyRep::Tuple(vec![PyRep::Integer(3), PyRep::Integer(2), PyRep::Integer(3), PyRep::Integer(0)]),
                PyRep::Tuple(vec![PyRep::Integer(5), PyRep::Integer(cc_pin_id), PyRep::Integer(2), PyRep::Integer(2)]),
            ]),
        ],
    )
    .await
    .expect("UserUpdateNetwork place pins and links");

    let PyRep::KeyVal(net_kv) = net_res else { panic!("Expected KeyVal"); };
    assert_eq!(net_kv.get("pinsCount"), Some(&PyRep::Integer(7)));
    assert_eq!(net_kv.get("linksCount"), Some(&PyRep::Integer(2)));
    let bal_after_pins_links = ctx.db.get_character(90_000_001).unwrap().balance;
    let isk_spent_reported = net_kv.get("totalCostISK").and_then(|v| v.as_f64()).unwrap();
    assert!(isk_spent_reported > 100_000.0);
    assert_eq!(
        bal_before_pins_links.0 - bal_after_pins_links.0,
        IskCents::from_f64_isk(isk_spent_reported).0
    );
    let Some(PyRep::List(net_pins)) = net_kv.get("pins") else { panic!("Expected pins list"); };
    let mut sum_pin_cpu = 0.0;
    let mut sum_pin_pg = 0.0;
    for p in net_pins {
        let PyRep::KeyVal(pkv) = p else { panic!() };
        sum_pin_cpu += pkv.get("cpuUsage").and_then(|v| v.as_f64()).unwrap();
        sum_pin_pg += pkv.get("powergridUsage").and_then(|v| v.as_f64()).unwrap();
    }
    for req_type in [2848i64, 2469, 2480, 2482, 2541, 2544] {
        assert!(
            net_pins.iter().any(|p| matches!(p, PyRep::KeyVal(kv) if kv.get("typeID") == Some(&PyRep::Integer(req_type)))),
            "Placed colony network must include pin type {req_type}"
        );
    }
    let proc_pin_id = net_pins
        .iter()
        .find_map(|p| match p {
            PyRep::KeyVal(kv) if kv.get("typeID").and_then(|v| v.as_i64()) == Some(2469) => {
                kv.get("pinID").and_then(|v| v.as_i64())
            }
            _ => None,
        })
        .unwrap();
    assert!(proc_pin_id > cc_pin_id);

    let Some(PyRep::List(net_links)) = net_kv.get("links") else { panic!("Expected links list"); };
    assert_eq!(net_links.len(), 2);
    let mut total_link_cpu = 0.0;
    let mut total_link_pg = 0.0;
    for link_rep in net_links {
        let PyRep::KeyVal(lkv) = link_rep else { panic!("Expected KeyVal"); };
        let lvl = lkv.get("level").and_then(|v| v.as_i64()).unwrap() as u8;
        let len_km = lkv.get("lengthKm").and_then(|v| v.as_f64()).unwrap();
        assert!(len_km > 0.0, "Planetary link must have positive geodesic lengthKm");
        let l_cpu = lkv.get("cpuUsage").and_then(|v| v.as_f64()).unwrap();
        let l_pg = lkv.get("powergridUsage").and_then(|v| v.as_f64()).unwrap();
        let l_bw = lkv.get("bandwidth").and_then(|v| v.as_f64()).unwrap();
        let (expected_pg, expected_cpu) = eve_server::formulas::compute_pi_link_cost(len_km, lvl);
        assert!((l_cpu - expected_cpu).abs() < 1e-6);
        assert!((l_pg - expected_pg).abs() < 1e-6);
        assert!((l_bw - 1_250.0 * 2.0_f64.powi(lvl as i32)).abs() < 1e-6);
        total_link_cpu += l_cpu;
        total_link_pg += l_pg;
    }
    let cpu_used = net_kv.get("cpuUsed").and_then(|v| v.as_f64()).unwrap();
    let power_used = net_kv.get("powerUsed").and_then(|v| v.as_f64()).unwrap();
    assert!((cpu_used - (sum_pin_cpu + total_link_cpu)).abs() < 1e-5);
    assert!((power_used - (sum_pin_pg + total_link_pg)).abs() < 1e-5);

    let budget_err = rpc(
        &ctx,
        &mut s1,
        "planetMgr",
        "UserUpdateNetwork",
        &[
            PyRep::Integer(JITA_PLANET_ID),
            PyRep::List(
                (10..=14)
                    .map(|idx| PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(idx)]), PyRep::Integer(2848), PyRep::Real(0.40 + (idx as f64) * 0.005), PyRep::Real(-1.05)]))
                    .collect(),
            ),
        ],
    )
    .await
    .unwrap_err();
    assert_eq!(budget_err.code, "InsufficientColonyCpuOrPowergrid");

    let full_net = rpc(
        &ctx,
        &mut s1,
        "planetMgr",
        "GetFullNetworkForOwner",
        &[PyRep::Integer(JITA_PLANET_ID), PyRep::Integer(90_000_001)],
    )
    .await
    .expect("GetFullNetworkForOwner");
    let PyRep::KeyVal(fn_kv) = full_net else { panic!("Expected KeyVal"); };
    assert!(matches!(fn_kv.get("pins"), Some(PyRep::List(l)) if l.len() == 7));
    assert!(matches!(fn_kv.get("links"), Some(PyRep::List(l)) if l.len() == 2));

    let extractors = rpc(
        &ctx,
        &mut s1,
        "planetMgr",
        "GetExtractorsForPlanet",
        &[PyRep::Integer(JITA_PLANET_ID)],
    )
    .await
    .expect("GetExtractorsForPlanet");
    assert!(matches!(extractors, PyRep::List(l) if l.len() == 1));

    ctx.db.with_mut_snapshot(|snap| { snap.skills.entry(90_000_001).or_default().remove(&33467); });
    let flush_res = rpc(&ctx, &mut s1, "planetMgr", "Flush", &[PyRep::Integer(JITA_PLANET_ID)],)
    .await
    .expect("planetMgr.Flush");
    let PyRep::KeyVal(fl_kv) = flush_res else { panic!("Expected KeyVal"); };
    let extracted_type_id = fl_kv.get("producedTypeID").and_then(|v| v.as_i64()).unwrap();
    let produced_qty = fl_kv.get("producedQuantity").and_then(|v| v.as_i64()).unwrap() as f64;
    let flush_tax_isk = fl_kv.get("pocoExportTaxISK").and_then(|v| v.as_f64()).unwrap();
    assert!((flush_tax_isk - (5.0 * produced_qty * 0.15)).abs() < 1e-4);

    let res_after = rpc(
        &ctx,
        &mut s1,
        "planetMgr",
        "GetPlanetResourceInfo",
        &[PyRep::Integer(JITA_PLANET_ID)],
    )
    .await
    .expect("GetPlanetResourceInfo after Flush");
    let PyRep::KeyVal(ra_kv) = res_after else { panic!("Expected KeyVal"); };
    let Some(PyRep::List(ra_list)) = ra_kv.get("resources") else { panic!("Expected resources List"); };
    let depleted_qual = ra_list
        .iter()
        .find_map(|r| match r {
            PyRep::KeyVal(kv)
                if kv.get("resourceTypeID").and_then(|v| v.as_i64()) == Some(extracted_type_id) =>
            { kv.get("quality").and_then(|v| v.as_f64()) }
            _ => None,
        })
        .unwrap();
    assert!(depleted_qual < 0.85);

    let bad_schem_err = rpc(
        &ctx,
        &mut s1,
        "planetMgr",
        "UserUpdateNetwork",
        &[
            PyRep::Integer(JITA_PLANET_ID),
            PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::Integer(8),
                PyRep::Integer(proc_pin_id),
                PyRep::Integer(999_999),
            ])]),
        ],
    )
    .await
    .unwrap_err();
    assert_eq!(bad_schem_err.code, "InvalidSchematic");

    rpc(
        &ctx,
        &mut s1,
        "planetMgr",
        "UserUpdateNetwork",
        &[
            PyRep::Integer(JITA_PLANET_ID),
            PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::Integer(8),
                PyRep::Integer(proc_pin_id),
                PyRep::Integer(65),
            ])]),
        ],
    )
    .await
    .expect("Set valid schematic 65");
    let pin3_info = rpc(
        &ctx,
        &mut s1,
        "planetMgr",
        "GetPin",
        &[PyRep::Integer(JITA_PLANET_ID), PyRep::Integer(proc_pin_id)],
    )
    .await
    .expect("GetPin");
    let PyRep::KeyVal(p3_kv) = pin3_info else { panic!("Expected KeyVal"); };
    assert_eq!(p3_kv.get("schematicID"), Some(&PyRep::Integer(65)));
    assert_eq!(p3_kv.get("cycleTimeSeconds"), Some(&PyRep::Integer(1800)));

    let cap_err = rpc(
        &ctx,
        &mut s1,
        "planetMgr",
        "UserTransferCommodities",
        &[
            PyRep::Integer(JITA_PLANET_ID),
            PyRep::Integer(2288),
            PyRep::Integer(3645),
            PyRep::Integer(50_000),
        ],
    )
    .await
    .unwrap_err();
    assert_eq!(cap_err.code, "PinNotFound");

    ctx.db.with_mut_snapshot(|snap| {
        let mut net = eve_server::services::industry_pi::load_pi_network(snap, 90_000_001, JITA_PLANET_ID, 5);
        if let Some(pin) = net.pins.first_mut() {
            pin.contents.insert(3645, 500);
        }
        eve_server::services::industry_pi::save_pi_network(snap, 90_000_001, JITA_PLANET_ID, &net);
        snap.skills.entry(90_000_001).or_default().insert(
            33467,
            TrainedSkillRecord { type_id: 33467, level: 4, skill_points: 135_765.0, },
        );
    });
    let bal_before_launch = ctx.db.get_character(90_000_001).unwrap().balance;
    let launch_res = rpc(
        &ctx,
        &mut s1,
        "planetMgr",
        "UserLaunchCommodities",
        &[
            PyRep::Integer(JITA_PLANET_ID),
            PyRep::Integer(3645),
            PyRep::Integer(200),
            PyRep::String("CustomsOffice".into()),
        ],
    )
    .await
    .expect("UserLaunchCommodities to CustomsOffice");
    let PyRep::KeyVal(ln_kv) = launch_res else { panic!("Expected KeyVal"); };
    let launch_tax = ln_kv.get("exportTaxISK").and_then(|v| v.as_f64()).unwrap();
    let water_base = ctx
        .sde
        .types
        .get(&3645)
        .map(|t| t.base_price.max(400.0))
        .unwrap_or(400.0);
    assert!((launch_tax - (water_base * 200.0 * 0.11)).abs() < 1e-4);
    let bal_after_launch = ctx.db.get_character(90_000_001).unwrap().balance;
    assert_eq!(bal_before_launch.0 - bal_after_launch.0, IskCents::from_f64_isk(launch_tax).0);
    let has_poco_journal = ctx.db.with_snapshot(|snap| {
        snap.wallet_journal
            .iter()
            .any(|j| j.character_id == 90_000_001 && j.entry_type == "CustomOfficeTax")
    });
    assert!(has_poco_journal);
}
