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
// parity: feat:skills.queue-get
// parity: feat:skills.queue-save
// parity: feat:skills.prereq-check
// parity: feat:skills.alpha-skill-caps
async fn test_char_skill_queue_prerequisites_and_alpha_caps() {
    eve_testkit::attest(&[
        "feat:skills.queue-get",
        "feat:skills.queue-save",
        "feat:skills.prereq-check",
        "feat:skills.alpha-skill-caps",
    ]);

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
// parity: feat:skills.alpha-injector
async fn test_char_skill_injection_and_extraction_invariants() {
    eve_testkit::attest(&[
        "feat:skills.free-sp",
        "feat:skills.skill-extractor",
        "feat:skills.alpha-injector",
    ]);

    let ctx = ServerContext::new();
    let char_id = 90_000_001i64;
    let mut sess = setup_pilot(&ctx, 51_102, char_id).await;

    // Seed pilot docked at Jita 4-4 station
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
                type_id: 3300,
                level: 5,
                skill_points: 256_000.0,
            },
        );
        skills.insert(
            3302,
            TrainedSkillRecord {
                type_id: 3302,
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
            PyRep::Dict(vec![(PyRep::Integer(3300), PyRep::Real(250_000.0))]),
            PyRep::Integer(770_000_001),
        ],
    )
    .await
    .expect_err("Alpha character must be blocked from extracting skills");
    assert_eq!(alpha_extract_err.code, "OmegaCloneRequired");

    // 3. Skill Extraction Invariant: Requires >= 5,500,000 total SP
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.clone_grade = 2; // Omega
            ch.skill_points = 5_200_000.0;
            ch.free_skill_points = 0.0;
        }
    });
    let low_sp_err = rpc(
        &ctx,
        &mut sess,
        "skillMgr2",
        "ExtractSkills",
        &[
            PyRep::Dict(vec![(PyRep::Integer(3300), PyRep::Real(250_000.0))]),
            PyRep::Integer(770_000_001),
        ],
    )
    .await
    .expect_err("Character with < 5.5M total SP must be blocked from extracting");
    assert_eq!(low_sp_err.code, "InsufficientSkillPoints");

    // 4. Valid Skill Extraction with >= 5.5M SP:
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.skill_points = 5_600_000.0;
            ch.free_skill_points = 300_000.0;
        }
    });
    let extract_rep = rpc(
        &ctx,
        &mut sess,
        "skillMgr2",
        "ExtractSkills",
        &[
            PyRep::Dict(vec![(PyRep::Integer(3300), PyRep::Real(250_000.0))]),
            PyRep::Integer(770_000_001),
        ],
    )
    .await
    .expect("ExtractSkills valid Omega with >= 5.5M SP");
    let injector_item_id = extract_rep
        .dict_get("injectorItemID")
        .and_then(|v| v.as_i64())
        .expect("expected injector item ID");

    // Verify Large Skill Injector (type 40520) created in station hangar
    let injector_type = ctx.db.with_snapshot(|snap| {
        snap.items.get(&injector_item_id).map(|i| (i.type_id, i.location_id))
    });
    assert_eq!(injector_type, Some((40520, 60_003_760)));

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
    assert!(sp_yield > 0.0, "Skill injector must yield positive SP");
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
        // Seed Jump Clone 1 at station
        let clones = snap.jump_clones.entry(char_id).or_default();
        clones.push(JumpCloneRecord {
            jump_clone_id: 101,
            character_id: char_id,
            location_id: station_loc,
            implants: vec![],
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
