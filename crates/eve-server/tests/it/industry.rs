//! Industry and reprocessing integration tests.

#![forbid(unsafe_code)]

use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::HashMap;

#[tokio::test]
async fn test_phase_0_s1_reprocessing_sde_materials_and_remainders() {
    // parity: plan:S-1, rpc:authentication.Login, rpc:charUnboundMgr.SelectCharacterID
    eve_testkit::attest(&["plan:S-1"]);
    use eve_db::{InventoryFlag, InventoryItemRecord};

    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut sess = SessionState::new(901);
    ctx.dispatch_rpc(
        &mut sess,
        "authentication",
        None,
        "Login",
        &[
            PyRep::String("demo".into()),
            PyRep::String("demo-secret".into()),
        ],
        &kw,
        None,
    )
    .await
    .expect("Login");
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_001)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");

    // 1. Non-reprocessable items (Tritanium 100_000_011 type 34, and Rifter Blueprint 100_000_007 type 691)
    // must return ItemNotReprocessable and remain untouched.
    assert!(
        ctx.sde.type_materials.get(&34).is_none(),
        "Tritanium (type 34) must not have SDE typeMaterials"
    );
    let trit_err = ctx
        .dispatch_rpc(
            &mut sess,
            "reprocessingSvc",
            None,
            "Reprocess",
            &[PyRep::Integer(100_000_011)],
            &kw,
            None,
        )
        .await;
    assert_eq!(trit_err.unwrap_err().code, "ItemNotReprocessable");
    assert_eq!(
        ctx.db.get_item(100_000_011).map(|i| i.quantity),
        Some(5_000)
    );

    let bp_err = ctx
        .dispatch_rpc(
            &mut sess,
            "reprocessingSvc",
            None,
            "Reprocess",
            &[PyRep::Integer(100_000_007)],
            &kw,
            None,
        )
        .await;
    assert_eq!(bp_err.unwrap_err().code, "ItemNotReprocessable");
    assert!(ctx.db.get_item(100_000_007).is_some());

    // 2. Active ship (100_000_001) and assembled (singleton = true) ships in hangar must be rejected.
    let active_ship_err = ctx
        .dispatch_rpc(
            &mut sess,
            "reprocessingSvc",
            None,
            "Reprocess",
            &[PyRep::Integer(100_000_001)],
            &kw,
            None,
        )
        .await;
    assert_eq!(
        active_ship_err.unwrap_err().code,
        "CannotReprocessActiveShip"
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            100_990_000,
            InventoryItemRecord {
                item_id: 100_990_000,
                type_id: 587, // Rifter (has SDE type_materials, but singleton = true means assembled)
                owner_id: 90_000_001,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });
    let assembled_err = ctx
        .dispatch_rpc(
            &mut sess,
            "reprocessingSvc",
            None,
            "Reprocess",
            &[PyRep::Integer(100_990_000)],
            &kw,
            None,
        )
        .await;
    assert_eq!(assembled_err.unwrap_err().code, "ItemMustBeRepackaged");
    assert!(ctx.db.get_item(100_990_000).is_some());

    // 3. Reprocess 10,000 Veldspar (100_000_012, type 1230, portionSize = 100, 34 -> 400 per batch).
    // With 0 skills, base_rate = 0.50 and Caldari Navy standing 2.0 -> station_tax_rate = 0.035 (charged in ISK) -> yield_rate = 0.50 -> 100 * 400 * 0.50 = 20,000 Tritanium.
    let reproc_veld = ctx
        .dispatch_rpc(
            &mut sess,
            "reprocessingSvc",
            None,
            "Reprocess",
            &[PyRep::Integer(100_000_012)],
            &kw,
            None,
        )
        .await
        .expect("Reprocess 10,000 Veldspar");
    let PyRep::KeyVal(veld_kv) = reproc_veld else {
        panic!("Expected KeyVal from Veldspar Reprocess");
    };
    let sde_veldspar_mats = ctx.sde.type_materials.get(&1230).expect("Veldspar typeMaterials");
    assert_eq!(sde_veldspar_mats.len(), 1);
    assert_eq!(sde_veldspar_mats[0].material_type_id, 34);
    assert_eq!(sde_veldspar_mats[0].quantity, 400);

    let outputs = match veld_kv.get("outputs") {
        Some(PyRep::List(l)) => l,
        _ => panic!("Expected outputs list in Reprocess result"),
    };
    assert_eq!(outputs.len(), 1, "Veldspar must yield exactly 1 material type per SDE invTypeMaterials");
    let out_mid = if let PyRep::KeyVal(mat_kv) = &outputs[0] {
        assert_eq!(mat_kv.get("typeID").and_then(|v| v.as_i64()), Some(34));
        assert_eq!(mat_kv.get("quantity").and_then(|v| v.as_i64()), Some(20_000));
        mat_kv.get("itemID").and_then(|v| v.as_i64()).expect("material itemID")
    } else {
        panic!("Expected KeyVal in outputs[0]");
    };
    let credited = ctx.db.get_item(out_mid).expect("Credited material item in hangar");
    assert_eq!(credited.type_id, 34);
    assert_eq!(credited.owner_id, 90_000_001);
    assert_eq!(credited.location_id, 60_003_760);
    assert_eq!(credited.flag, InventoryFlag::Hangar);
    assert_eq!(credited.quantity, 20_000);

    assert_eq!(
        veld_kv.get("tritaniumQuantity").and_then(|v| v.as_i64()),
        Some(20_000),
        "S-1: 10,000 Veldspar at 0.50 facility yield must yield 20,000 Tritanium (with 3.5% station tax charged in ISK)"
    );
    assert!(
        veld_kv.get("reprocessingTaxISK").and_then(|v| v.as_f64()).unwrap_or(0.0) > 0.0,
        "S-1: NPC station reprocessing tax must be charged in ISK"
    );
    assert!(
        ctx.db.get_item(100_000_012).is_none(),
        "S-1: Exact multiple of portionSize (10,000 Veldspar) must consume entire stack"
    );

    // 4. Reprocess 250 Plagioclase (100_990_001, type 18, portionSize = 100, 34 -> 175, 36 -> 70):
    // consumes 200 units (2 batches), leaves 50 remainder in stack, and outputs floor(2*175*0.50)=175 Tritanium + floor(2*70*0.50)=70 Mexallon.
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            100_990_001,
            InventoryItemRecord {
                item_id: 100_990_001,
                type_id: 18,
                owner_id: 90_000_001,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 250,
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
    let reproc_plag = ctx
        .dispatch_rpc(
            &mut sess,
            "reprocessingSvc",
            None,
            "Reprocess",
            &[PyRep::Integer(100_990_001)],
            &kw,
            None,
        )
        .await
        .expect("Reprocess 250 Plagioclase");
    let PyRep::KeyVal(rp_kv) = reproc_plag else {
        panic!("Expected KeyVal from Reprocess");
    };
    assert_eq!(
        ctx.db.get_item(100_990_001).map(|i| i.quantity),
        Some(50),
        "S-1: 50 un-reprocessed Plagioclase units (< portionSize 100) must remain in stack"
    );
    assert_eq!(
        rp_kv.get("tritaniumQuantity").and_then(|v| v.as_i64()),
        Some(175)
    );
    let Some(PyRep::List(outputs)) = rp_kv.get("outputs") else {
        panic!("Expected outputs list from Reprocess");
    };
    assert_eq!(outputs.len(), 2);
    let mexallon_qty = ctx.db.with_snapshot(|snap| {
        snap.items
            .values()
            .filter(|i| {
                i.owner_id == 90_000_001
                    && i.location_id == 60_003_760
                    && i.type_id == 36
                    && i.item_id != 100_000_010
            })
            .map(|i| i.quantity as i64)
            .sum::<i64>()
    });
    assert_eq!(
        mexallon_qty, 70,
        "S-1: Reprocessing 200 Plagioclase at 0.50 yield must deposit 70 Mexallon (36)"
    );

    // 5. SetSessionCheck (client_local -> UnimplementedRPC) and InsufficientFunds rejection when pilot wallet balance cannot cover station reprocessing tax:
    // ore stack and wallet balance must remain unchanged.
    let session_check = ctx
        .dispatch_rpc(&mut sess, "reprocessingSvc", None, "SetSessionCheck", &[], &kw, None)
        .await
        .expect_err("reprocessingSvc.SetSessionCheck");
    assert_eq!(session_check.code, "UnimplementedRPC");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.balance = eve_db::IskCents(0);
        }
        snap.items.insert(
            100_990_002,
            InventoryItemRecord {
                item_id: 100_990_002,
                type_id: 1230,
                owner_id: 90_000_001,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 1_000,
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
    let insuff_err = ctx
        .dispatch_rpc(
            &mut sess,
            "reprocessingSvc",
            None,
            "Reprocess",
            &[PyRep::Integer(100_990_002)],
            &kw,
            None,
        )
        .await;
    assert_eq!(
        insuff_err.unwrap_err().code,
        "InsufficientFunds",
        "Reprocess must reject with InsufficientFunds when pilot balance cannot cover station tax"
    );
    assert_eq!(
        ctx.db.get_item(100_990_002).map(|i| i.quantity),
        Some(1_000),
        "Failed Reprocess must leave ore item stack untouched"
    );
    assert_eq!(
        ctx.db.with_snapshot(|s| s.characters.get(&90_000_001).map(|c| c.balance)),
        Some(eve_db::IskCents(0)),
        "Failed Reprocess must leave pilot wallet balance untouched"
    );
}

#[tokio::test]
async fn test_blueprint_manager_get_blueprint_data_by_owner_market_research_copy_and_acl() {
    // parity: rpc:blueprintManager.GetBlueprintDataByOwner
    eve_testkit::attest(&["rpc:blueprintManager.GetBlueprintDataByOwner"]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();
    let mut sess = SessionState::new(902);
    ctx.dispatch_rpc(
        &mut sess,
        "authentication",
        None,
        "Login",
        &[
            PyRep::String("demo".into()),
            PyRep::String("demo-secret".into()),
        ],
        &kw,
        None,
    )
    .await
    .expect("Login");
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_001)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");

    let station_id = sess.station_id.expect("Docked station");
    let solar_system_id = sess.solar_system_id.expect("Docked solar system") as i64;
    let station_type_id = ctx
        .sde
        .stations
        .get(&station_id)
        .map(|s| s.station_type_id as i64)
        .expect("SDE station type");

    let limits = ctx
        .dispatch_rpc(&mut sess, "blueprintManager", None, "GetLimits", &[], &kw, None)
        .await
        .expect("blueprintManager.GetLimits");
    assert_eq!(
        limits.dict_get("maxBlueprintResults").and_then(|v| v.as_i64()),
        Some(25_000)
    );

    // Pick a seeded NPC T1 blueprint sell order at the pilot's docked station (distinct from starter Rifter BPO 691)
    let bp_type_id = ctx.db.with_snapshot(|snap| {
        let mut candidates: Vec<u32> = snap
            .market_orders
            .values()
            .filter(|o| {
                !o.is_bid
                    && o.is_npc
                    && o.station_id == station_id
                    && o.vol_remaining >= 1
                    && o.price.to_f64_isk() <= 5_000_000.0
                    && o.type_id != 691
                    && ctx.sde.blueprints.get(&o.type_id).is_some_and(|bp| {
                        bp.research_me_time_sec > 0
                            && bp.copy_time_sec > 0
                            && bp.max_production_limit >= 2
                    })
            })
            .map(|o| o.type_id)
            .collect();
        candidates.sort_unstable();
        candidates[0]
    });

    // Purchase 1 BPO from the NPC market via immediate PlaceBuyOrder (duration = 0)
    ctx.dispatch_rpc(
        &mut sess,
        "marketProxy",
        None,
        "PlaceBuyOrder",
        &[
            PyRep::Integer(station_id),
            PyRep::Integer(bp_type_id as i64),
            PyRep::Real(10_000_000.0),
            PyRep::Integer(1),
            PyRep::Integer(-1),
            PyRep::Integer(1),
            PyRep::Integer(0),
            PyRep::Bool(false),
        ],
        &kw,
        None,
    )
    .await
    .expect("Immediate market purchase of seeded NPC BPO");

    let count_for_facility = |counts_rep: &PyRep, fid: i64| -> Option<i64> {
        let PyRep::Dict(entries) = counts_rep else {
            return None;
        };
        entries
            .iter()
            .find(|(k, _)| k.as_i64() == Some(fid))
            .and_then(|(_, v)| v.as_i64())
    };

    // 1. V24 2-arg call (ownerID, None) returns (bps, bpCounts) with all 15 BlueprintInstance fields
    let initial_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "blueprintManager",
            None,
            "GetBlueprintDataByOwner",
            &[PyRep::Integer(90_000_001), PyRep::None],
            &kw,
            None,
        )
        .await
        .expect("GetBlueprintDataByOwner(char, None)");
    let PyRep::Tuple(initial_pair) = initial_rep else {
        panic!("Expected (bps, bpCounts) 2-tuple from V24 GetBlueprintDataByOwner");
    };
    assert_eq!(initial_pair.len(), 2);
    let PyRep::List(initial_bps) = &initial_pair[0] else {
        panic!("Expected bps List");
    };
    assert_eq!(
        count_for_facility(&initial_pair[1], station_id),
        Some(initial_bps.len() as i64)
    );
    let bought_bpo = initial_bps
        .iter()
        .find(|b| b.dict_get("typeID").and_then(|v| v.as_i64()) == Some(bp_type_id as i64))
        .expect("Purchased BPO present in GetBlueprintDataByOwner");
    let bought_item_id = bought_bpo
        .dict_get("itemID")
        .and_then(|v| v.as_i64())
        .expect("itemID");
    assert_eq!(bought_bpo.dict_get("ownerID").and_then(|v| v.as_i64()), Some(90_000_001));
    assert_eq!(bought_bpo.dict_get("facilityID").and_then(|v| v.as_i64()), Some(station_id));
    assert_eq!(bought_bpo.dict_get("locationID").and_then(|v| v.as_i64()), Some(station_id));
    assert_eq!(bought_bpo.dict_get("locationTypeID").and_then(|v| v.as_i64()), Some(station_type_id));
    assert_eq!(bought_bpo.dict_get("locationFlagID").and_then(|v| v.as_i64()), Some(4));
    assert_eq!(bought_bpo.dict_get("flagID").and_then(|v| v.as_i64()), Some(4));
    assert_eq!(bought_bpo.dict_get("solarSystemID").and_then(|v| v.as_i64()), Some(solar_system_id));
    assert_eq!(bought_bpo.dict_get("materialEfficiency").and_then(|v| v.as_i64()), Some(0));
    assert_eq!(bought_bpo.dict_get("timeEfficiency").and_then(|v| v.as_i64()), Some(0));
    assert_eq!(bought_bpo.dict_get("runs").and_then(|v| v.as_i64()), Some(-1));
    assert_eq!(bought_bpo.dict_get("quantity").and_then(|v| v.as_i64()), Some(1));
    assert_eq!(bought_bpo.dict_get("original").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(bought_bpo.dict_get("isImpounded").and_then(|v| v.as_bool()), Some(false));
    assert_eq!(bought_bpo.dict_get("jobID"), Some(&PyRep::None));

    // Facility filter on a different NPC station returns 0 bps while bpCounts still reports total at station_id
    let other_station_id = ctx
        .sde
        .stations
        .keys()
        .copied()
        .find(|&sid| sid != station_id)
        .expect("Other SDE station");
    let filtered_empty = ctx
        .dispatch_rpc(
            &mut sess,
            "blueprintManager",
            None,
            "GetBlueprintDataByOwner",
            &[PyRep::Integer(90_000_001), PyRep::Integer(other_station_id)],
            &kw,
            None,
        )
        .await
        .expect("GetBlueprintDataByOwner filtered to other station");
    let PyRep::Tuple(filtered_pair) = filtered_empty else {
        panic!("Expected Tuple");
    };
    let PyRep::List(filtered_list) = &filtered_pair[0] else {
        panic!("Expected List");
    };
    assert!(filtered_list.is_empty());
    assert_eq!(
        count_for_facility(&filtered_pair[1], station_id),
        Some(initial_bps.len() as i64)
    );

    // 2. Install ME research job (activityID = 4, runs = 1) -> verify active jobID & unpacked singleton quantity (-1)
    let me_job = ctx
        .dispatch_rpc(
            &mut sess,
            "industryManager",
            None,
            "InstallJob",
            &[
                PyRep::Integer(bought_item_id),
                PyRep::Integer(bp_type_id as i64),
                PyRep::Integer(4),
                PyRep::Integer(1),
                PyRep::Integer(station_id),
            ],
            &kw,
            None,
        )
        .await
        .expect("InstallJob ME research");
    let me_job_id = me_job.dict_get("jobID").and_then(|v| v.as_i64()).expect("me jobID");
    let me_dur = me_job.dict_get("durationSeconds").and_then(|v| v.as_i64()).expect("me duration") as u64;

    let active_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "blueprintManager",
            None,
            "GetBlueprintDataByOwner",
            &[PyRep::Integer(90_000_001), PyRep::Integer(station_id)],
            &kw,
            None,
        )
        .await
        .expect("GetBlueprintDataByOwner during active ME job");
    let PyRep::Tuple(active_pair) = active_rep else { panic!("Expected Tuple") };
    let PyRep::List(active_bps) = &active_pair[0] else { panic!("Expected List") };
    let locked_bpo = active_bps
        .iter()
        .find(|b| b.dict_get("itemID").and_then(|v| v.as_i64()) == Some(bought_item_id))
        .expect("Locked BPO visible in GetBlueprintDataByOwner");
    assert_eq!(locked_bpo.dict_get("jobID").and_then(|v| v.as_i64()), Some(me_job_id));
    assert_eq!(locked_bpo.dict_get("quantity").and_then(|v| v.as_i64()), Some(-1));
    assert_eq!(
        locked_bpo.dict_get("flagID").and_then(|v| v.as_i64()),
        Some(eve_db::InventoryFlag::Locked as i64)
    );
    assert_eq!(
        locked_bpo.dict_get("locationFlagID").and_then(|v| v.as_i64()),
        Some(eve_db::InventoryFlag::Locked as i64)
    );

    ctx.advance_virtual_clock(me_dur + 5);
    ctx.dispatch_rpc(
        &mut sess,
        "industryManager",
        None,
        "CompleteJob",
        &[PyRep::Integer(me_job_id)],
        &kw,
        None,
    )
    .await
    .expect("CompleteJob ME research");

    // 3. Install Copying job (activityID = 5, runs = 2) on researched ME1 BPO -> deliver BPC
    let copy_job = ctx
        .dispatch_rpc(
            &mut sess,
            "industryManager",
            None,
            "InstallJob",
            &[
                PyRep::Integer(bought_item_id),
                PyRep::Integer(bp_type_id as i64),
                PyRep::Integer(5),
                PyRep::Integer(2),
                PyRep::Integer(station_id),
            ],
            &kw,
            None,
        )
        .await
        .expect("InstallJob Copying");
    let copy_job_id = copy_job.dict_get("jobID").and_then(|v| v.as_i64()).expect("copy jobID");
    let copy_dur = copy_job.dict_get("durationSeconds").and_then(|v| v.as_i64()).expect("copy duration") as u64;
    ctx.advance_virtual_clock(copy_dur + 5);
    ctx.dispatch_rpc(
        &mut sess,
        "industryManager",
        None,
        "CompleteJob",
        &[PyRep::Integer(copy_job_id)],
        &kw,
        None,
    )
    .await
    .expect("CompleteJob Copying");

    let after_copy_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "blueprintManager",
            None,
            "GetBlueprintDataByOwner",
            &[PyRep::Integer(90_000_001), PyRep::Integer(station_id)],
            &kw,
            None,
        )
        .await
        .expect("GetBlueprintDataByOwner after ME + Copy");
    let PyRep::Tuple(after_pair) = after_copy_rep else { panic!("Expected Tuple") };
    let PyRep::List(after_bps) = &after_pair[0] else { panic!("Expected List") };
    assert_eq!(
        count_for_facility(&after_pair[1], station_id),
        Some((initial_bps.len() + 1) as i64)
    );
    let final_bpo = after_bps
        .iter()
        .find(|b| b.dict_get("itemID").and_then(|v| v.as_i64()) == Some(bought_item_id))
        .expect("Researched BPO");
    assert_eq!(final_bpo.dict_get("materialEfficiency").and_then(|v| v.as_i64()), Some(1));
    assert_eq!(final_bpo.dict_get("quantity").and_then(|v| v.as_i64()), Some(-1));
    assert_eq!(final_bpo.dict_get("runs").and_then(|v| v.as_i64()), Some(-1));
    assert_eq!(final_bpo.dict_get("jobID"), Some(&PyRep::None));

    let delivered_bpc = after_bps
        .iter()
        .find(|b| {
            b.dict_get("typeID").and_then(|v| v.as_i64()) == Some(bp_type_id as i64)
                && b.dict_get("itemID").and_then(|v| v.as_i64()) != Some(bought_item_id)
        })
        .expect("Copied BPC present in GetBlueprintDataByOwner");
    assert_eq!(delivered_bpc.dict_get("original").and_then(|v| v.as_bool()), Some(false));
    assert_eq!(delivered_bpc.dict_get("quantity").and_then(|v| v.as_i64()), Some(-2));
    assert_eq!(delivered_bpc.dict_get("runs").and_then(|v| v.as_i64()), Some(2));
    assert_eq!(delivered_bpc.dict_get("materialEfficiency").and_then(|v| v.as_i64()), Some(1));
    assert_eq!(delivered_bpc.dict_get("jobID"), Some(&PyRep::None));

    // 4. Reject unauthorized foreign pilot and NPC corporation blueprint queries with OwnerAccessDenied
    for denied_owner in [90_000_002_i64, 1_000_170_i64] {
        let err = ctx
            .dispatch_rpc(
                &mut sess,
                "blueprintManager",
                None,
                "GetBlueprintDataByOwner",
                &[PyRep::Integer(denied_owner), PyRep::None],
                &kw,
                None,
            )
            .await
            .expect_err("Unauthorized ownerID must be rejected");
        assert_eq!(err.code, "OwnerAccessDenied");
    }
}
