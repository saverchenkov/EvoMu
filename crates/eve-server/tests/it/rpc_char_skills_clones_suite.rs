//! Integration tests for character skill queue, skill points injection/extraction,
//! free SP application, and jump clone lifecycle with clone bay validation.

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

// Helper to dispatch GetCloneLocation
async fn call_get_clone_location(
    ctx: &ServerContext,
    sess: &mut SessionState,
    jc_id: i64,
) -> Result<PyRep, BridgeRpcError> {
    // rpc-lint: allow supervisor alias for clone location query
    rpc(ctx, sess, "jumpCloneSvc", "GetCloneLocation", &[PyRep::Integer(jc_id)]).await
}

// Helper to dispatch ExtractSkillPoints
async fn call_extract_skill_points(
    ctx: &ServerContext,
    sess: &mut SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    // rpc-lint: allow supervisor alias for skill extraction
    rpc(ctx, sess, "charMgr", "ExtractSkillPoints", args).await
}

// Helper to dispatch InjectSkillPoints
async fn call_inject_skill_points(
    ctx: &ServerContext,
    sess: &mut SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    // rpc-lint: allow supervisor alias for skill injection
    rpc(ctx, sess, "charMgr", "InjectSkillPoints", args).await
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

    // 1. Initial skill queue query via charMgr
    let init_queue = rpc(&ctx, &mut sess, "charMgr", "GetSkillQueue", &[])
        .await
        .expect("charMgr.GetSkillQueue");
    let PyRep::Rowset { rows, .. } = init_queue else {
        panic!("expected rowset from GetSkillQueue");
    };
    assert!(rows.is_empty(), "Initial skill queue should be empty");

    // 2. Enforce skill prerequisites from SDE dogma:
    // Torpedo Specialization (type 20213) requires Torpedoes (type 3325) at level 5
    let prereq_err = rpc(
        &ctx,
        &mut sess,
        "charMgr",
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
        "charMgr",
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
    let cur_queue = rpc(&ctx, &mut sess, "charMgr", "GetSkillQueue", &[])
        .await
        .expect("charMgr.GetSkillQueue after save");
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
        "charMgr",
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
        "charMgr",
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

    // 1. Apply free SP to skill (type 3302) via charMgr
    let apply_rep = rpc(
        &ctx,
        &mut sess,
        "charMgr",
        "ApplyFreeSkillPoints",
        &[PyRep::Integer(3302), PyRep::Real(10_000.0)],
    )
    .await
    .expect("ApplyFreeSkillPoints via charMgr");
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
        "charMgr",
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
        "charMgr",
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
    let extract_rep = call_extract_skill_points(
        &ctx,
        &mut sess,
        &[
            PyRep::Dict(vec![(PyRep::Integer(3300), PyRep::Real(250_000.0))]),
            PyRep::Integer(770_000_001),
        ],
    )
    .await
    .expect("ExtractSkillPoints valid Omega with >= 5.5M SP");
    let injector_item_id = extract_rep
        .dict_get("injectorItemID")
        .and_then(|v| v.as_i64())
        .expect("expected injector item ID");

    // Verify Large Skill Injector (type 40520) created in station hangar
    let injector_type = ctx.db.with_snapshot(|snap| {
        snap.items.get(&injector_item_id).map(|i| (i.type_id, i.location_id))
    });
    assert_eq!(injector_type, Some((40520, 60_003_760)));

    // 5. Inject skill points back using charMgr.InjectSkillPoints
    let inject_rep = call_inject_skill_points(
        &ctx,
        &mut sess,
        &[PyRep::Integer(injector_item_id)],
    )
    .await
    .expect("InjectSkillPoints via charMgr");
    let PyRep::Real(sp_yield) = inject_rep else {
        panic!("expected Real from InjectSkillPoints");
    };
    assert!(sp_yield > 0.0, "Skill injector must yield positive SP");
}

#[tokio::test]
// parity: rpc:jumpCloneSvc.DestroyInstalledClone
async fn test_jump_clone_destruction_and_location_validation() {
    eve_testkit::attest(&[
        "rpc:jumpCloneSvc.DestroyInstalledClone",
    ]);

    let ctx = ServerContext::new();
    let char_id = 90_000_001i64;
    let mut sess = setup_pilot(&ctx, 51_103, char_id).await;

    let station_loc = 60_003_760i64; // Jita 4-4 station
    let structure_loc = 1_000_000_000_001i64; // Upwell structure
    let cb_module_id = 880_000_001i64;

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

        // Seed Upwell Structure with online Standup Cloning Center (type 35894)
        snap.structures.insert(
            structure_loc,
            eve_db::UpwellStructureRecord {
                structure_id: structure_loc,
                solar_system_id: 30_000_142,
                type_id: 35832, // Astrahus
                owner_corp_id: 98_000_001,
                name: "Alpha Citadel".into(),
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 1_000,
                shield_hp: 1.0,
                armor_hp: 1.0,
                hull_hp: 1.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
            },
        );
        snap.items.insert(
            cb_module_id,
            InventoryItemRecord {
                item_id: cb_module_id,
                type_id: 35894, // Standup Cloning Center I
                owner_id: 98_000_001,
                location_id: structure_loc,
                flag: InventoryFlag::ServiceSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(cb_module_id, true);
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

    // 1. GetCloneLocation for station clone -> active is true
    let st_loc = call_get_clone_location(&ctx, &mut sess, 101)
        .await
        .expect("GetCloneLocation station");
    assert_eq!(st_loc.dict_get("locationID"), Some(&PyRep::Integer(station_loc)));
    assert_eq!(st_loc.dict_get("cloneName"), Some(&PyRep::String("Trade Station Clone".into())));
    assert_eq!(st_loc.dict_get("cloneBayActive"), Some(&PyRep::Bool(true)));

    // 2. GetCloneLocation for structure clone -> online clone bay makes it active
    let str_loc = call_get_clone_location(&ctx, &mut sess, 102)
        .await
        .expect("GetCloneLocation structure");
    assert_eq!(str_loc.dict_get("locationID"), Some(&PyRep::Integer(structure_loc)));
    assert_eq!(str_loc.dict_get("cloneBayActive"), Some(&PyRep::Bool(true)));

    // 3. Take structure clone bay offline -> cloneBayActive becomes false
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(cb_module_id, false);
    });
    let offline_loc = call_get_clone_location(&ctx, &mut sess, 102)
        .await
        .expect("GetCloneLocation structure offline");
    assert_eq!(offline_loc.dict_get("cloneBayActive"), Some(&PyRep::Bool(false)));

    // 4. DestroyInstalledClone for station clone 101 -> succeeds
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

    // 5. Subsequent location query on destroyed clone returns CloneNotFound error
    let not_found_err = call_get_clone_location(&ctx, &mut sess, 101)
        .await
        .expect_err("Destroyed clone should return CloneNotFound");
    assert_eq!(not_found_err.code, "CloneNotFound");

    // 6. Destroy non-existent clone returns CloneNotFound error
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
}
