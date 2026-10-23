//! Integration tests for character skill queue, skill points injection/extraction,
//! free SP application, and jump clone lifecycle.

#![forbid(unsafe_code)]

use crate::common;
use eve_db::{InventoryFlag, InventoryItemRecord, JumpCloneRecord, TrainedSkillRecord};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use std::collections::HashMap;

async fn setup_pilot(ctx: &ServerContext, sid: u64, char_id: i64) -> SessionState {
    let kw = HashMap::new();
    let mut sess = SessionState::new(sid);
    common::login_pilot(ctx, &mut sess, "demo", "demo-secret").await;
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
    svc: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, svc, None, method, args, &HashMap::new(), None)
        .await
}

#[tokio::test]
async fn test_char_skill_queue_prerequisites_and_alpha_caps() {

    let ctx = ServerContext::new();
    let char_id = 90_000_001i64;
    let mut sess = setup_pilot(&ctx, 51_101, char_id).await;

    // 1. Initial skill queue query via skillMgr2
    let init_queue = rpc(&ctx, &mut sess, "skillMgr2", "GetSkillQueue", &[])
        .await
        .expect("skillMgr2.GetSkillQueue");
    let PyRep::Rowset { rows, .. } = init_queue else {
        panic!("expected rowset from GetSkillQueue");
    };
    assert!(rows.is_empty(), "Initial skill queue should be empty");

    // 2. Enforce skill prerequisites from SDE dogma:
    // Torpedo Specialization (type 20213) requires Torpedoes (type 3325) at level 5
    let prereq_err = rpc(
        &ctx,
        &mut sess,
        "skillMgr2",
        "SaveSkillQueue",
        &[PyRep::List(vec![PyRep::Tuple(vec![
            PyRep::Integer(20213), // Torpedo Specialization
            PyRep::Integer(1),
        ])])],
    )
    .await
    .expect_err("Should fail due to unmet Torpedoes prerequisite");
    assert_eq!(prereq_err.code, "SkillPrerequisiteNotMet");

    // 3. Save a valid skill to the queue (Gunnery 3300 to Level 5)
    let save_rep = rpc(
        &ctx,
        &mut sess,
        "skillMgr2",
        "SaveSkillQueue",
        &[PyRep::List(vec![PyRep::Tuple(vec![
            PyRep::Integer(3300),
            PyRep::Integer(5),
        ])])],
    )
    .await
    .expect("SaveSkillQueue Gunnery 5");
    assert_eq!(save_rep.dict_get("typeID"), Some(&PyRep::Integer(3300)));
    assert_eq!(save_rep.dict_get("targetLevel"), Some(&PyRep::Integer(5)));

    // 4. Verify GetSkillQueue reflects the newly saved queue
    let cur_queue = rpc(&ctx, &mut sess, "skillMgr2", "GetSkillQueue", &[])
        .await
        .expect("skillMgr2.GetSkillQueue after save");
    let PyRep::Rowset { rows: cur_rows, .. } = cur_queue else {
        panic!("expected rowset from GetSkillQueue");
    };
    assert_eq!(cur_rows.len(), 1);
    assert_eq!(cur_rows[0][0], PyRep::Integer(3300));
    assert_eq!(cur_rows[0][1], PyRep::Integer(5));

    // 5. Enforce Alpha clone limits (clone_grade = 1):
    // Alpha clones cannot queue skills beyond their per-skill Alpha cap
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.clone_grade = 1;
        }
    });

    // Science (3402) has Alpha level cap of 4 in clonegrades
    let alpha_lvl_err = rpc(
        &ctx,
        &mut sess,
        "skillMgr2",
        "SaveSkillQueue",
        &[PyRep::List(vec![PyRep::Tuple(vec![
            PyRep::Integer(3402),
            PyRep::Integer(5),
        ])])],
    )
    .await
    .expect_err("Should fail because Alpha level cap is 4");
    assert_eq!(alpha_lvl_err.code, "AlphaCloneSkillLevelCapExceeded");

    // 6. Enforce Alpha clone 5,000,000 total SP cap
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.skill_points = 4_995_000.0;
            ch.free_skill_points = 0.0;
        }
    });

    let alpha_cap_err = rpc(
        &ctx,
        &mut sess,
        "skillMgr2",
        "SaveSkillQueue",
        &[PyRep::List(vec![PyRep::Tuple(vec![
            PyRep::Integer(3402), // Science (level 4 requires 45,255 SP)
            PyRep::Integer(4),
        ])])],
    )
    .await
    .expect_err("Should fail because Alpha total SP would exceed 5,000,000 SP limit");
    assert_eq!(alpha_cap_err.code, "AlphaCloneSpCapExceeded");
}

#[tokio::test]
// parity: feat:skills.free-sp
// parity: feat:skills.skill-extractor
async fn test_char_skill_injection_and_extraction_invariants() {
    eve_testkit::attest(&[
        "feat:skills.free-sp",
        "feat:skills.skill-extractor",
    ]);

    let ctx = ServerContext::new();
    let char_id = 90_000_001i64;
    let mut sess = setup_pilot(&ctx, 51_102, char_id).await;

    // Seed pilot docked at Jita 4-4 station with 6M allocated SP
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = Some(60_003_760);
            ch.clone_grade = 2; // Omega
            ch.skill_points = 6_000_000.0;
            ch.free_skill_points = 100_000.0;
        }
        let skills = snap.skills.entry(char_id).or_default();
        skills.insert(
            3300,
            TrainedSkillRecord {
                type_id: 3300, // Gunnery (rank 1.0)
                level: 5,
                skill_points: 256_000.0,
            },
        );
        skills.insert(
            3301,
            TrainedSkillRecord {
                type_id: 3301, // Small Hybrid Turret (rank 1.0)
                level: 5,
                skill_points: 256_000.0,
            },
        );
        skills.insert(
            3302,
            TrainedSkillRecord {
                type_id: 3302, // Medium Hybrid Turret
                level: 4,
                skill_points: 45_255.0,
            },
        );
        // Seed Skill Extractor item (type 40519) in station hangar
        snap.items.insert(
            770_000_001,
            InventoryItemRecord {
                item_id: 770_000_001,
                type_id: 40519,
                owner_id: char_id,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    // 1. Apply free SP to skill (type 3302) via skillMgr2
    let apply_rep = rpc(
        &ctx,
        &mut sess,
        "skillMgr2",
        "ApplyFreeSkillPoints",
        &[PyRep::Integer(3302), PyRep::Real(10_000.0)],
    )
    .await
    .expect("ApplyFreeSkillPoints via skillMgr2");
    let PyRep::Real(applied_val) = apply_rep else {
        panic!("expected Real from ApplyFreeSkillPoints");
    };
    assert_eq!(applied_val, 55_255.0); // 45,255.0 base SP + 10,000.0 applied free SP

    // Verify character free SP decremented transactionally
    let remaining_fsp = ctx.db.with_snapshot(|snap| {
        snap.characters.get(&char_id).map(|c| c.free_skill_points).unwrap_or(0.0)
    });
    assert_eq!(remaining_fsp, 90_000.0);

    // 2. Skill Extraction Invariant: Requires Omega clone
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.clone_grade = 1; // Alpha
        }
    });
    let alpha_extract_err = rpc(
        &ctx,
        &mut sess,
        "skillMgr2",
        "ExtractSkills",
        &[
            PyRep::Dict(vec![
                (PyRep::Integer(3300), PyRep::Real(250_000.0)),
                (PyRep::Integer(3301), PyRep::Real(250_000.0)),
            ]),
            PyRep::Integer(770_000_001),
        ],
    )
    .await
    .expect_err("Alpha character must be blocked from extracting skills");
    assert_eq!(alpha_extract_err.code, "OmegaCloneRequired");

    // 3. Skill Extraction Invariant: Requires >= 5,500,000 allocated SP
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.clone_grade = 2; // Omega
            ch.skill_points = 5_200_000.0; // Allocated SP < 5.5M
            ch.free_skill_points = 500_000.0; // Free SP cannot satisfy floor
        }
    });
    let low_sp_err = rpc(
        &ctx,
        &mut sess,
        "skillMgr2",
        "ExtractSkills",
        &[
            PyRep::Dict(vec![
                (PyRep::Integer(3300), PyRep::Real(250_000.0)),
                (PyRep::Integer(3301), PyRep::Real(250_000.0)),
            ]),
            PyRep::Integer(770_000_001),
        ],
    )
    .await
    .expect_err("Character with < 5.5M allocated SP must be blocked from extracting");
    assert_eq!(low_sp_err.code, "InsufficientSkillPoints");

    // 3b. Exploit Protection (R-SKX-01): Reject negative, NaN, and duplicate amounts
    let neg_err = rpc(
        &ctx,
        &mut sess,
        "skillMgr2",
        "ExtractSkills",
        &[
            PyRep::Dict(vec![
                (PyRep::Integer(3300), PyRep::Real(600_000.0)),
                (PyRep::Integer(3301), PyRep::Real(-100_000.0)),
            ]),
            PyRep::Integer(770_000_001),
        ],
    )
    .await
    .expect_err("Negative extraction amount must be rejected");
    assert_eq!(neg_err.code, "InvalidSkillPointAmount");

    let nan_err = rpc(
        &ctx,
        &mut sess,
        "skillMgr2",
        "ExtractSkills",
        &[
            PyRep::Dict(vec![
                (PyRep::Integer(3300), PyRep::Real(f64::NAN)),
            ]),
            PyRep::Integer(770_000_001),
        ],
    )
    .await
    .expect_err("NaN extraction amount must be rejected");
    assert_eq!(nan_err.code, "InvalidSkillPointAmount");

    let dup_err = rpc(
        &ctx,
        &mut sess,
        "skillMgr2",
        "ExtractSkills",
        &[
            PyRep::Dict(vec![
                (PyRep::Integer(3300), PyRep::Real(250_000.0)),
                (PyRep::Integer(3300), PyRep::Real(250_000.0)),
            ]),
            PyRep::Integer(770_000_001),
        ],
    )
    .await
    .expect_err("Duplicate skill typeID in extraction must be rejected");
    assert_eq!(dup_err.code, "InvalidSkillPointAmount");

    // 3c. R-SKX-03: Reject non-extractable skills (e.g. Infomorph Psychology 24242, Cybernetics 3411)
    let non_ext_err = rpc(
        &ctx,
        &mut sess,
        "skillMgr2",
        "ExtractSkills",
        &[
            PyRep::Dict(vec![
                (PyRep::Integer(24242), PyRep::Real(500_000.0)),
            ]),
            PyRep::Integer(770_000_001),
        ],
    )
    .await
    .expect_err("Non-extractable skill (Infomorph Psychology) must be rejected");
    assert_eq!(non_ext_err.code, "SkillCannotBeExtracted");

    // 3d. R-SKX-03: Prerequisite dependency guard
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.skill_points = 5_800_000.0;
            ch.free_skill_points = 90_000.0;
        }
    });
    // Small Hybrid Turret (3301) requires Gunnery (3300) level 1.
    // If we attempt to extract all 256,000 SP from Gunnery (reducing it to level 0) while
    // Small Hybrid Turret remains trained at level 5, extraction must be blocked.
    let prereq_ext_err = rpc(
        &ctx,
        &mut sess,
        "skillMgr2",
        "ExtractSkills",
        &[
            PyRep::Dict(vec![
                (PyRep::Integer(3300), PyRep::Real(256_000.0)),
                (PyRep::Integer(3301), PyRep::Real(244_000.0)),
            ]),
            PyRep::Integer(770_000_001),
        ],
    )
    .await
    .expect_err("Extracting prerequisite skill below required level must be rejected");
    assert_eq!(prereq_ext_err.code, "SkillPrerequisiteRequired");

    // 3e. R-SKX-05 / R-SKX-07: 4th-slot prerequisite dependency guard
    // Heavy Assault Cruisers (16591) requires Capacitor Management (3418) level 4 strictly in slot 4 (attribute 1285).
    let hac_sde = ctx.sde.types.get(&16591).expect("Heavy Assault Cruisers in SDE");
    assert_eq!(hac_sde.default_attributes.get(&1285).copied(), Some(3418.0));
    assert_eq!(hac_sde.default_attributes.get(&1286).copied(), Some(4.0));
    for &aid in &[182u16, 183u16, 184u16] {
        assert_ne!(hac_sde.default_attributes.get(&aid).copied(), Some(3418.0));
    }
    ctx.db.with_mut_snapshot(|snap| {
        let skills = snap.skills.entry(char_id).or_default();
        skills.insert(16591, TrainedSkillRecord { type_id: 16591, level: 1, skill_points: 250.0 });
        skills.insert(3418, TrainedSkillRecord { type_id: 3418, level: 4, skill_points: 135_765.0 });
    });
    let prereq4_err = rpc(
        &ctx,
        &mut sess,
        "skillMgr2",
        "ExtractSkills",
        &[
            PyRep::Dict(vec![
                (PyRep::Integer(3418), PyRep::Real(10_000.0)),
                (PyRep::Integer(3300), PyRep::Real(245_000.0)),
                (PyRep::Integer(3301), PyRep::Real(245_000.0)),
            ]),
            PyRep::Integer(770_000_001),
        ],
    )
    .await
    .expect_err("Extracting 4th-slot prerequisite skill below required level must be rejected");
    assert_eq!(prereq4_err.code, "SkillPrerequisiteRequired");
    assert!(prereq4_err.message.contains("16591"));
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(m) = snap.skills.get_mut(&char_id) {
            m.remove(&16591);
            m.remove(&3418);
        }
    });

    // 4. Valid Skill Extraction with >= 5.5M allocated SP extracting exactly 500,000 SP:
    let extract_rep = rpc(
        &ctx,
        &mut sess,
        "skillMgr2",
        "ExtractSkills",
        &[
            PyRep::Dict(vec![
                (PyRep::Integer(3300), PyRep::Real(250_000.0)),
                (PyRep::Integer(3301), PyRep::Real(250_000.0)),
            ]),
            PyRep::Integer(770_000_001),
        ],
    )
    .await
    .expect("ExtractSkills valid Omega with >= 5.5M allocated SP");
    let injector_item_id = extract_rep
        .dict_get("injectorItemID")
        .and_then(|v| v.as_i64())
        .expect("expected injector item ID");

    // Assert: Extractor item (40519) consumed from inventory
    let extractor_exists = ctx.db.with_snapshot(|snap| snap.items.contains_key(&770_000_001));
    assert!(!extractor_exists, "Skill Extractor item must be consumed");

    // Assert: Large Skill Injector (40520) created in station hangar
    let injector_type = ctx.db.with_snapshot(|snap| {
        snap.items.get(&injector_item_id).map(|i| (i.type_id, i.location_id))
    });
    assert_eq!(injector_type, Some((40520, 60_003_760)));

    // Assert: Character SP decremented by exactly 500,000; free SP untouched
    let (char_sp, char_fsp) = ctx.db.with_snapshot(|snap| {
        let ch = snap.characters.get(&char_id).unwrap();
        (ch.skill_points, ch.free_skill_points)
    });
    assert_eq!(char_sp, 5_300_000.0);
    assert_eq!(char_fsp, 90_000.0);

    // Assert: Target skill levels recomputed accurately from remaining SP
    // Gunnery (rank 1): 256,000 - 250,000 = 6,000 SP -> Level 2 (lvl 2 req 1,415 SP, lvl 3 req 8,000 SP)
    let (g_sp, g_lvl) = ctx.db.with_snapshot(|snap| {
        let rec = snap.skills.get(&char_id).unwrap().get(&3300).unwrap();
        (rec.skill_points, rec.level)
    });
    assert_eq!(g_sp, 6_000.0);
    assert_eq!(g_lvl, 2);

    // 5. Inject skill points back using skillMgr2.InjectSkillpoints
    let inject_rep = rpc(
        &ctx,
        &mut sess,
        "skillMgr2",
        "InjectSkillpoints",
        &[PyRep::Integer(injector_item_id)],
    )
    .await
    .expect("InjectSkillpoints via skillMgr2");
    let PyRep::Real(sp_yield) = inject_rep else {
        panic!("expected Real from InjectSkillpoints");
    };
    assert_eq!(sp_yield, 400_000.0); // 5M..50M bracket yields 400,000 SP

    // Assert: Injector consumed and free SP credited
    let injector_after = ctx.db.with_snapshot(|snap| snap.items.contains_key(&injector_item_id));
    assert!(!injector_after, "Skill Injector must be consumed after injection");
    let fsp_after = ctx.db.with_snapshot(|snap| snap.characters.get(&char_id).unwrap().free_skill_points);
    assert_eq!(fsp_after, 490_000.0);

    // 6. Daily Alpha Injector (type 47414):
    // R-SKI-01: Alpha clones can use Daily Alpha Injectors even with >= 5,000,000 SP (up to Alpha skillset ~20M)
    let alpha_inj_id = 770_000_002i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.clone_grade = 1; // Alpha
            ch.skill_points = 5_000_000.0;
            ch.free_skill_points = 0.0;
        }
        snap.items.insert(
            alpha_inj_id,
            InventoryItemRecord {
                item_id: alpha_inj_id,
                type_id: 47414, // Daily Alpha Injector
                owner_id: char_id,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    let alpha_ok_rep = rpc(
        &ctx,
        &mut sess,
        "skillMgr2",
        "InjectSkillpoints",
        &[PyRep::Integer(alpha_inj_id)],
    )
    .await
    .expect("Alpha clone with 5M SP can use Daily Alpha Injector to inject free SP");
    assert_eq!(alpha_ok_rep, PyRep::Real(50_000.0));

    // 7. R-SKX-04: Ordinary skill Metallurgy (type 3409) can be extracted
    let meta_ext_id = 770_000_003i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.clone_grade = 2; // Omega
            ch.skill_points = 6_000_000.0;
        }
        snap.skills.entry(char_id).or_default().insert(
            3409,
            TrainedSkillRecord { type_id: 3409, level: 5, skill_points: 768_000.0 },
        );
        snap.items.insert(
            meta_ext_id,
            InventoryItemRecord {
                item_id: meta_ext_id,
                type_id: 40519,
                owner_id: char_id,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                ..Default::default()
            },
        );
    });
    let meta_rep = rpc(
        &ctx,
        &mut sess,
        "skillMgr2",
        "ExtractSkills",
        &[
            PyRep::Dict(vec![(PyRep::Integer(3409), PyRep::Real(500_000.0))]),
            PyRep::Integer(meta_ext_id),
        ],
    )
    .await
    .expect("Metallurgy (3409) is an ordinary skill and must be extractable");
    assert!(meta_rep.dict_get("injectorItemID").is_some());
    let (meta_sp, meta_lvl) = ctx.db.with_snapshot(|snap| {
        let rec = snap.skills.get(&char_id).unwrap().get(&3409).unwrap();
        (rec.skill_points, rec.level)
    });
    assert_eq!(meta_sp, 268_000.0);
    assert_eq!(meta_lvl, 4);
}

#[tokio::test]
// parity: rpc:jumpCloneSvc.DestroyInstalledClone
// parity: feat:clones.jump-clone-destroy
async fn test_jump_clone_destruction_and_state_invariants() {
    eve_testkit::attest(&[
        "rpc:jumpCloneSvc.DestroyInstalledClone",
        "feat:clones.jump-clone-destroy",
    ]);

    let ctx = ServerContext::new();
    let char_id = 90_000_001i64;
    let mut sess = setup_pilot(&ctx, 51_103, char_id).await;

    let station_loc = 60_003_760i64; // Jita 4-4 station
    let structure_loc = 1_000_000_000_001i64; // Upwell structure

    ctx.db.with_mut_snapshot(|snap| {
        // Seed Jump Clone 1 at station with implants
        let clones = snap.jump_clones.entry(char_id).or_default();
        clones.push(JumpCloneRecord {
            jump_clone_id: 101,
            character_id: char_id,
            location_id: station_loc,
            implants: vec![3411, 24242],
        });
        // Seed Jump Clone 2 at Upwell structure
        clones.push(JumpCloneRecord {
            jump_clone_id: 102,
            character_id: char_id,
            location_id: structure_loc,
            implants: vec![],
        });
    });

    let _ = rpc(
        &ctx,
        &mut sess,
        "jumpCloneSvc",
        "SetJumpCloneName",
        &[PyRep::Integer(101), PyRep::String("Trade Station Clone".into())],
    )
    .await
    .expect("SetJumpCloneName");

    // 1. Initial GetCloneState reflects 2 clones
    let st_clones = rpc(&ctx, &mut sess, "jumpCloneSvc", "GetCloneState", &[])
        .await
        .expect("GetCloneState initial");
    let PyRep::Rowset { rows: initial_rows, .. } = st_clones.dict_get("clones").expect("clones rowset") else {
        panic!("expected rowset for clones");
    };
    assert_eq!(initial_rows.len(), 2);

    // 2. DestroyInstalledClone for station clone 101 -> succeeds
    let dest_rep = rpc(
        &ctx,
        &mut sess,
        "jumpCloneSvc",
        "DestroyInstalledClone",
        &[PyRep::Integer(101)],
    )
    .await
    .expect("DestroyInstalledClone 101");
    assert_eq!(dest_rep, PyRep::Bool(true));

    // Assert: clone 101 and its implants are completely removed from DB snapshot
    let (rem_clones, has_implant_101) = ctx.db.with_snapshot(|snap| {
        let list = snap.jump_clones.get(&char_id).cloned().unwrap_or_default();
        let any = list.iter().any(|c| c.jump_clone_id == 101 || c.implants.contains(&3411));
        (list.len(), any)
    });
    assert_eq!(rem_clones, 1);
    assert!(!has_implant_101, "DestroyInstalledClone must destroy clone and its implants");

    // 3. Subsequent GetCloneState shows clone 101 removed, clone 102 remaining
    let after_clones = rpc(&ctx, &mut sess, "jumpCloneSvc", "GetCloneState", &[])
        .await
        .expect("GetCloneState after destroy");
    let PyRep::Rowset { rows: after_rows, .. } = after_clones.dict_get("clones").expect("clones rowset") else {
        panic!("expected rowset for clones");
    };
    assert_eq!(after_rows.len(), 1);
    assert_eq!(after_rows[0][0], PyRep::Integer(102));

    // 4. Destroy non-existent clone returns CloneNotFound error
    let invalid_dest_err = rpc(
        &ctx,
        &mut sess,
        "jumpCloneSvc",
        "DestroyInstalledClone",
        &[PyRep::Integer(9999)],
    )
    .await
    .expect_err("Destroying non-existent clone should fail");
    assert_eq!(invalid_dest_err.code, "CloneNotFound");

    // 5. Non-positive jump clone ID returns InvalidJumpCloneID
    let non_pos_err = rpc(
        &ctx,
        &mut sess,
        "jumpCloneSvc",
        "DestroyInstalledClone",
        &[PyRep::Integer(0)],
    )
    .await
    .expect_err("Zero jump clone ID should fail");
    assert_eq!(non_pos_err.code, "InvalidJumpCloneID");
}
