//! Behavioural parity tests for `feat:character.*`, `feat:clones.*`, `feat:skills.*` and
//! `feat:attributes.*` rows. Only rows whose every acceptance clause is asserted here are tagged.


use crate::common;

use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use std::collections::HashMap;

const PILOT: i64 = 90_000_001;

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    method: &str,
    args: Vec<PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, svc, None, method, &args, &HashMap::new(), None).await
}

fn doll() -> PyRep {
    PyRep::keyval([("hair", PyRep::String("short".into())), ("skin", PyRep::String("fair".into()))])
}

fn portrait() -> PyRep {
    PyRep::keyval([("camera", PyRep::String("front".into()))])
}

/// Reuses the seeded pilot's SDE-valid (bloodlineID, ancestryID, schoolID) so no IDs are hardcoded.
fn creation_ids(ctx: &ServerContext) -> (i64, i64, i64) {
    let ch = ctx.db.get_character(PILOT).unwrap();
    assert!(ctx.sde.bloodlines.contains_key(&ch.bloodline_id));
    assert!(ctx.sde.ancestries.get(&ch.ancestry_id).is_some_and(|a| a.bloodline_id == ch.bloodline_id));
    (ch.bloodline_id as i64, ch.ancestry_id as i64, ch.school_id as i64)
}

async fn create(ctx: &ServerContext, sess: &mut SessionState, name: &str) -> Result<PyRep, BridgeRpcError> {
    let (bl, anc, school) = creation_ids(ctx);
    rpc(
        ctx,
        sess,
        "charUnboundMgr",
        "CreateCharacterWithDoll",
        vec![
            PyRep::String(name.into()),
            PyRep::Integer(bl),
            PyRep::Integer(1),
            PyRep::Integer(anc),
            doll(),
            portrait(),
            PyRep::Integer(school),
        ],
    )
    .await
}

#[tokio::test]
async fn character_slots_limit_three_per_account() {
    // parity: feat:character.char-slots
    eve_testkit::attest(&["feat:character.char-slots"]);
    let ctx = ServerContext::new();
    let mut s = SessionState::new(52_001);
    common::login_pilot(&ctx, &mut s, "demo", "demo-secret").await;
    let uid = s.user_id.unwrap();
    let existing = ctx.db.get_characters_for_user(uid).len() as i64;
    assert!(existing < 3, "demo account must start below the slot cap");

    let n0 = rpc(&ctx, &mut s, "charUnboundMgr", "GetNumCharacters", vec![]).await.unwrap();
    assert_eq!(n0, PyRep::Tuple(vec![PyRep::Integer(existing), PyRep::Integer(3)]));

    let names = ["Slot Filler Aaa", "Slot Filler Bbb", "Slot Filler Ccc"];
    for (i, name) in names.iter().take((3 - existing) as usize).enumerate() {
        let cid = create(&ctx, &mut s, name).await.expect("create within limit").as_i64().unwrap();
        assert_eq!(ctx.db.get_character(cid).unwrap().user_id, uid);
        let n = rpc(&ctx, &mut s, "charUnboundMgr", "GetNumCharacters", vec![]).await.unwrap();
        assert_eq!(n, PyRep::Tuple(vec![PyRep::Integer(existing + i as i64 + 1), PyRep::Integer(3)]));
    }

    // The 4th character is rejected and nothing is created.
    let before = ctx.db.with_snapshot(|snap| snap.characters.len());
    let over = create(&ctx, &mut s, "Slot Overflow Ddd").await.expect_err("4th character must be rejected");
    assert_eq!(over.code, "MaxCharacterSlotsReached");
    assert_eq!(ctx.db.with_snapshot(|snap| snap.characters.len()), before);
    assert_eq!(ctx.db.get_characters_for_user(uid).len(), 3);

    // Another account is unaffected by this account's limit.
    let mut other = SessionState::new(52_002);
    common::login_pilot(&ctx, &mut other, "other", "other-secret").await;
    create(&ctx, &mut other, "Other Account Eee").await.expect("other account has free slots");
}

#[tokio::test]
async fn character_delete_prepare_cancel_and_biomass_to_doomheim() {
    // parity: feat:character.delete-character
    eve_testkit::attest(&["feat:character.delete-character"]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut s = SessionState::new(52_010);
    common::login_pilot(&ctx, &mut s, "demo", "demo-secret").await;
    let victim = create(&ctx, &mut s, "Biomass Candidate").await.unwrap().as_i64().unwrap();

    // Deleting without preparing is refused.
    let unprepared = rpc(&ctx, &mut s, "charUnboundMgr", "DeleteCharacter", vec![PyRep::Integer(victim)])
        .await
        .expect_err("not prepared");
    assert_eq!(unprepared.code, "DeleteNotPrepared");

    // Another account cannot prepare this character for deletion.
    let mut other = SessionState::new(52_011);
    common::login_pilot(&ctx, &mut other, "other", "other-secret").await;
    let foreign = rpc(&ctx, &mut other, "charUnboundMgr", "PrepareCharacterForDelete", vec![PyRep::Integer(victim)])
        .await
        .expect_err("foreign account");
    assert_eq!(foreign.code, "CharacterOwnershipMismatch");

    // Prepare starts the timer; the character is locked from selection meanwhile.
    let now = ctx.now_unix();
    let ready = rpc(&ctx, &mut s, "charUnboundMgr", "PrepareCharacterForDelete", vec![PyRep::Integer(victim)])
        .await
        .expect("prepare")
        .as_i64()
        .unwrap();
    let delay = ready - now as i64;
    assert!(delay > 0, "deletion must be delayed");
    let locked = rpc(&ctx, &mut s, "charUnboundMgr", "SelectCharacterID", vec![PyRep::Integer(victim)])
        .await
        .expect_err("delete-pending character cannot be selected");
    assert_eq!(locked.code, "CharacterLocked");

    // Cancel aborts: the character is selectable again and delete is refused.
    let cancelled = rpc(&ctx, &mut s, "charUnboundMgr", "CancelCharacterDeletePrepare", vec![PyRep::Integer(victim)])
        .await
        .expect("cancel");
    assert_eq!(cancelled, PyRep::Bool(true));
    ctx.advance_virtual_clock(delay as u64 + 10);
    let after_cancel = rpc(&ctx, &mut s, "charUnboundMgr", "DeleteCharacter", vec![PyRep::Integer(victim)])
        .await
        .expect_err("cancelled prepare");
    assert_eq!(after_cancel.code, "DeleteNotPrepared");
    rpc(&ctx, &mut s, "charUnboundMgr", "SelectCharacterID", vec![PyRep::Integer(victim)])
        .await
        .expect("selectable after cancel");

    // Prepare again; deleting before the delay elapses is refused.
    let mut s2 = SessionState::new(52_012);
    common::login_pilot(&ctx, &mut s2, "demo", "demo-secret").await;
    let ready2 = rpc(&ctx, &mut s2, "charUnboundMgr", "PrepareCharacterForDelete", vec![PyRep::Integer(victim)])
        .await
        .unwrap()
        .as_i64()
        .unwrap();
    let wait = (ready2 - ctx.now_unix() as i64) as u64;
    ctx.advance_virtual_clock(wait.saturating_sub(60));
    let early = rpc(&ctx, &mut s2, "charUnboundMgr", "DeleteCharacter", vec![PyRep::Integer(victim)])
        .await
        .expect_err("too early");
    assert_eq!(early.code, "DeleteTimerNotExpired");

    // After the delay the character is biomassed into Doomheim.
    ctx.advance_virtual_clock(120);
    let n_before = rpc(&ctx, &mut s2, "charUnboundMgr", "GetNumCharacters", vec![]).await.unwrap();
    let del = rpc(&ctx, &mut s2, "charUnboundMgr", "DeleteCharacter", vec![PyRep::Integer(victim)])
        .await
        .expect("biomass");
    let PyRep::KeyVal(kv) = del else { panic!("expected KeyVal") };
    assert_eq!(kv.get("deleted").and_then(|v| v.as_bool()), Some(true));
    let corp = ctx.db.get_character(victim).unwrap().corporation_id;
    let doomheim = ctx.sde.npc_corporations.get(&corp).map(|c| c.name.clone());
    assert_eq!(doomheim.as_deref(), Some("Doomheim"), "biomassed character belongs to Doomheim (SDE)");
    let n_after = rpc(&ctx, &mut s2, "charUnboundMgr", "GetNumCharacters", vec![]).await.unwrap();
    let count = |r: &PyRep| match r {
        PyRep::Tuple(v) => v[0].as_i64().unwrap(),
        other => panic!("{other:?}"),
    };
    assert_eq!(count(&n_after), count(&n_before) - 1, "biomassed character frees its slot");
    let gone = rpc(&ctx, &mut s2, "charUnboundMgr", "SelectCharacterID", vec![PyRep::Integer(victim)])
        .await
        .expect_err("biomassed character cannot be selected");
    assert_eq!(gone.code, "CharacterSelectionDenied");
    assert_eq!(ctx.db.get_character(victim).unwrap().user_id, 0, "biomassed character left the account");
}

fn clone_rows(rep: PyRep) -> Vec<Vec<PyRep>> {
    match rep {
        PyRep::Rowset { columns, rows } => {
            assert_eq!(columns, vec!["jumpCloneID".to_string(), "locationID".to_string(), "cloneName".to_string()]);
            rows
        }
        PyRep::KeyVal(mut kv) => match kv.remove("clones") {
            Some(PyRep::Rowset { columns, rows }) => {
                assert_eq!(columns, vec!["jumpCloneID".to_string(), "locationID".to_string(), "cloneName".to_string()]);
                rows
            }
            other => panic!("expected clones rowset inside KeyVal, got {other:?}"),
        },
        other => panic!("expected rowset or KeyVal, got {other:?}"),
    }
}

#[tokio::test]
async fn jump_clone_name_is_persisted_and_shown_in_clone_state() {
    // parity: feat:clones.jump-clone-name
    eve_testkit::attest(&["feat:clones.jump-clone-name"]);
    let ctx = ServerContext::new();
    let mut s = SessionState::new(52_020);
    common::login_pilot(&ctx, &mut s, "demo", "demo-secret").await;
    rpc(&ctx, &mut s, "charUnboundMgr", "SelectCharacterID", vec![PyRep::Integer(PILOT)]).await.unwrap();
    // Infomorph Psychology I lets the pilot hold one jump clone (test setup).
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(PILOT).or_default().insert(
            24242,
            eve_db::TrainedSkillRecord { type_id: 24242, level: 1, skill_points: 250.0 },
        );
    });
    let install = rpc(&ctx, &mut s, "jumpCloneSvc", "InstallCloneInStation", vec![]).await.expect("install clone");
    let PyRep::KeyVal(kv) = install else { panic!("expected KeyVal") };
    let jc_id = kv.get("jumpCloneID").and_then(|v| v.as_i64()).unwrap();

    let before = clone_rows(rpc(&ctx, &mut s, "jumpCloneSvc", "GetCloneState", vec![]).await.unwrap());
    assert_eq!(before.len(), 1);
    assert_eq!(before[0][2].as_str(), Some(""));

    rpc(&ctx, &mut s, "jumpCloneSvc", "SetJumpCloneName", vec![PyRep::Integer(jc_id), PyRep::String("  Trade Hub Clone ".into())])
        .await
        .expect("SetJumpCloneName");
    let named = clone_rows(rpc(&ctx, &mut s, "jumpCloneSvc", "GetCloneState", vec![]).await.unwrap());
    assert_eq!(named[0][0].as_i64(), Some(jc_id));
    assert_eq!(named[0][2].as_str(), Some("Trade Hub Clone"));
    let here = clone_rows(rpc(&ctx, &mut s, "jumpCloneSvc", "GetStationCloneState", vec![]).await.unwrap());
    assert_eq!(here.len(), 1);
    assert_eq!(here[0][2].as_str(), Some("Trade Hub Clone"));

    // Persisted: visible from a fresh session after relog.
    let mut s2 = SessionState::new(52_021);
    common::login_pilot(&ctx, &mut s2, "demo", "demo-secret").await;
    rpc(&ctx, &mut s2, "charUnboundMgr", "SelectCharacterID", vec![PyRep::Integer(PILOT)]).await.unwrap();
    let relog = clone_rows(rpc(&ctx, &mut s2, "jumpCloneSvc", "GetCloneState", vec![]).await.unwrap());
    assert_eq!(relog[0][2].as_str(), Some("Trade Hub Clone"));

    // Another character cannot rename it; overlong names are refused; empty clears the label.
    let mut other = SessionState::new(52_022);
    common::login_pilot(&ctx, &mut other, "other", "other-secret").await;
    rpc(&ctx, &mut other, "charUnboundMgr", "SelectCharacterID", vec![PyRep::Integer(90_000_002)]).await.unwrap();
    let foreign = rpc(&ctx, &mut other, "jumpCloneSvc", "SetJumpCloneName", vec![PyRep::Integer(jc_id), PyRep::String("Stolen".into())])
        .await
        .expect_err("foreign rename");
    assert_eq!(foreign.code, "JumpCloneNotFound");
    let long = rpc(&ctx, &mut s2, "jumpCloneSvc", "SetJumpCloneName", vec![PyRep::Integer(jc_id), PyRep::String("x".repeat(101))])
        .await
        .expect_err("too long");
    assert_eq!(long.code, "JumpCloneNameTooLong");
    let still = clone_rows(rpc(&ctx, &mut s2, "jumpCloneSvc", "GetCloneState", vec![]).await.unwrap());
    assert_eq!(still[0][2].as_str(), Some("Trade Hub Clone"));
    rpc(&ctx, &mut s2, "jumpCloneSvc", "SetJumpCloneName", vec![PyRep::Integer(jc_id), PyRep::String(String::new())])
        .await
        .unwrap();
    let cleared = clone_rows(rpc(&ctx, &mut s2, "jumpCloneSvc", "GetCloneState", vec![]).await.unwrap());
    assert_eq!(cleared[0][2].as_str(), Some(""));
}

#[tokio::test]
async fn test_jump_clone_state_clones_implants_and_timers() {
    // parity: feat:clones.jump-clone-state
    eve_testkit::attest(&["feat:clones.jump-clone-state"]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut s = SessionState::new(52_030);
    common::login_pilot(&ctx, &mut s, "demo", "demo-secret").await;
    rpc(&ctx, &mut s, "charUnboundMgr", "SelectCharacterID", vec![PyRep::Integer(PILOT)]).await.unwrap();

    // Find a second station with a clone bay in the SDE so we can test remote vs docked station clones.
    let remote_station_id = ctx
        .sde
        .solar_systems
        .values()
        .flat_map(|sys| sys.stations.iter())
        .find(|st| st.has_clone_bay && st.station_id != 60_003_760)
        .map(|st| st.station_id)
        .expect("second station with clone bay");

    let rorq_id = 77_000_901_i64;
    ctx.db.with_mut_snapshot(|snap| {
        let skills = snap.skills.entry(PILOT).or_default();
        skills.insert(24242, eve_db::TrainedSkillRecord { type_id: 24242, level: 5, skill_points: 256_000.0 });
        skills.insert(33399, eve_db::TrainedSkillRecord { type_id: 33399, level: 4, skill_points: 100_000.0 });
        snap.active_implants.insert(PILOT, vec![9899]);
        snap.jump_clones.insert(
            PILOT,
            vec![
                eve_db::JumpCloneRecord {
                    jump_clone_id: 8001,
                    character_id: PILOT,
                    location_id: 60_003_760,
                    implants: vec![10216, 13219],
                },
                eve_db::JumpCloneRecord {
                    jump_clone_id: 8002,
                    character_id: PILOT,
                    location_id: remote_station_id,
                    implants: vec![9941],
                },
                eve_db::JumpCloneRecord {
                    jump_clone_id: 8003,
                    character_id: 90_000_002,
                    location_id: rorq_id,
                    implants: vec![],
                },
            ],
        );
    });
    rpc(&ctx, &mut s, "jumpCloneSvc", "SetJumpCloneName", vec![PyRep::Integer(8001), PyRep::String("Jita Shield Clone".into())])
        .await
        .unwrap();
    rpc(&ctx, &mut s, "jumpCloneSvc", "SetJumpCloneName", vec![PyRep::Integer(8002), PyRep::String("Remote Armor Clone".into())])
        .await
        .unwrap();

    // 1. GetCloneState before any jump: returns KeyVal(clones, implants, timeLastJump=None).
    let state = rpc(&ctx, &mut s, "jumpCloneSvc", "GetCloneState", vec![]).await.expect("GetCloneState");
    assert_eq!(state.dict_get("timeLastJump"), Some(&PyRep::None));
    let Some(PyRep::Rowset { columns: c_cols, rows: c_rows }) = state.dict_get("clones") else {
        panic!("expected clones rowset in GetCloneState: {state:?}");
    };
    assert_eq!(c_cols, &vec!["jumpCloneID".to_string(), "locationID".to_string(), "cloneName".to_string()]);
    assert_eq!(c_rows.len(), 3);
    assert_eq!(c_rows[0], vec![PyRep::Integer(8001), PyRep::Integer(60_003_760), PyRep::String("Jita Shield Clone".into())]);
    assert_eq!(c_rows[1], vec![PyRep::Integer(8002), PyRep::Integer(remote_station_id), PyRep::String("Remote Armor Clone".into())]);

    let Some(PyRep::Rowset { columns: i_cols, rows: i_rows }) = state.dict_get("implants") else {
        panic!("expected implants rowset in GetCloneState: {state:?}");
    };
    assert_eq!(i_cols, &vec!["jumpCloneID".to_string(), "typeID".to_string()]);
    assert_eq!(
        i_rows,
        &vec![
            vec![PyRep::Integer(8001), PyRep::Integer(10216)],
            vec![PyRep::Integer(8001), PyRep::Integer(13219)],
            vec![PyRep::Integer(8002), PyRep::Integer(9941)],
        ]
    );

    // 2. GetStationCloneState returns only clones located at the currently docked station (60_003_760).
    let st_state = rpc(&ctx, &mut s, "jumpCloneSvc", "GetStationCloneState", vec![]).await.expect("GetStationCloneState");
    let PyRep::Rowset { columns: st_cols, rows: st_rows } = st_state else {
        panic!("expected Rowset from GetStationCloneState");
    };
    assert_eq!(st_cols, vec!["jumpCloneID".to_string(), "locationID".to_string(), "cloneName".to_string()]);
    assert_eq!(st_rows.len(), 1);
    assert_eq!(st_rows[0][0].as_i64(), Some(8001));
    assert_eq!(st_rows[0][1].as_i64(), Some(60_003_760));

    // 3. GetShipCloneState returns clones installed in the ship clone bay with ownerID and locationID.
    let ship_state = rpc(&ctx, &mut s, "jumpCloneSvc", "GetShipCloneState", vec![PyRep::Integer(rorq_id)])
        .await
        .expect("GetShipCloneState");
    let PyRep::Rowset { columns: sh_cols, rows: sh_rows } = ship_state else {
        panic!("expected Rowset from GetShipCloneState");
    };
    assert_eq!(sh_cols, vec!["jumpCloneID".to_string(), "ownerID".to_string(), "locationID".to_string(), "cloneName".to_string()]);
    assert_eq!(sh_rows.len(), 1);
    assert_eq!(sh_rows[0][0].as_i64(), Some(8003));
    assert_eq!(sh_rows[0][1].as_i64(), Some(90_000_002));
    assert_eq!(sh_rows[0][2].as_i64(), Some(rorq_id));

    // 4. Jump to remote clone 8002: updates timeLastJump to a filetime timestamp and swaps clone 8002's location/implants.
    rpc(&ctx, &mut s, "jumpCloneSvc", "CloneJump", vec![PyRep::Integer(8002)]).await.expect("CloneJump");
    let after = rpc(&ctx, &mut s, "jumpCloneSvc", "GetCloneState", vec![]).await.expect("GetCloneState after jump");
    let last_jump_ft = after.dict_get("timeLastJump").and_then(|v| v.as_i64()).expect("timeLastJump filetime");
    assert!(last_jump_ft > 116_444_736_000_000_000, "timeLastJump must be a valid Windows filetime");
    let Some(PyRep::Rowset { rows: after_i_rows, .. }) = after.dict_get("implants") else {
        panic!("expected implants rowset after jump");
    };
    // Clone 8002 (now left behind at 60_003_760) holds the old body's implant 9899, while active body holds 9941.
    assert!(after_i_rows.contains(&vec![PyRep::Integer(8002), PyRep::Integer(9899)]));
}

fn ship_attr(all_info: &PyRep, attr_id: i64) -> f64 {
    let dict = all_info.dict_get("shipAttributes").unwrap_or(all_info);
    match dict {
        PyRep::Dict(entries) => entries
            .iter()
            .find(|(k, _)| k.as_i64() == Some(attr_id))
            .and_then(|(_, v)| v.as_f64())
            .unwrap_or_else(|| panic!("missing attribute {attr_id} in {dict:?}")),
        other => panic!("expected Dict, got {other:?}"),
    }
}

#[tokio::test]
async fn test_implant_plug_and_remove_destroys_implant() {
    // parity: feat:attributes.implant-plug, feat:attributes.implant-remove
    eve_testkit::attest(&["feat:attributes.implant-plug", "feat:attributes.implant-remove"]);
    let ctx = ServerContext::new();
    let mut s = SessionState::new(52_040);
    common::login_pilot(&ctx, &mut s, "demo", "demo-secret").await;
    rpc(&ctx, &mut s, "charUnboundMgr", "SelectCharacterID", vec![PyRep::Integer(PILOT)]).await.unwrap();

    let ship_id = s.ship_id.expect("active ship");
    let imp_slot1_basic_id = 88_100_001_i64;    // 9899 Ocular Filter - Basic (slot 1, +3 perception, Cybernetics I)
    let imp_slot1_std_id = 88_100_002_i64;      // 10216 Ocular Filter - Standard (slot 1, +4 perception, Cybernetics IV)
    let imp_slot7_shield_id = 88_100_003_i64;   // 16246 Zainou 'Gnome' Shield Management SM-705 (slot 7, +5% shieldCapacity, Cybernetics IV)

    ctx.db.with_mut_snapshot(|snap| {
        snap.active_implants.insert(PILOT, Vec::new());
        // Cybernetics (3411) at level 1 initially.
        snap.skills.entry(PILOT).or_default().insert(
            3411,
            eve_db::TrainedSkillRecord { type_id: 3411, level: 1, skill_points: 250.0 },
        );
        for (iid, tid) in [
            (imp_slot1_basic_id, 9899_u32),
            (imp_slot1_std_id, 10216_u32),
            (imp_slot7_shield_id, 16246_u32),
        ] {
            snap.items.insert(
                iid,
                eve_db::InventoryItemRecord {
                    item_id: iid,
                    type_id: tid,
                    owner_id: PILOT,
                    location_id: 60_003_760,
                    flag: eve_db::InventoryFlag::Hangar,
                    quantity: 1,
                    singleton: false,
                    version: 1,
                    ..Default::default()
                },
            );
        }
    });

    // Baseline ship shieldCapacity (attr 263) and character perception before plugging implants.
    let base_ship_attrs = rpc(&ctx, &mut s, "dogmaIM", "GetAllInfo", vec![]).await.unwrap();
    let base_shield = ship_attr(&base_ship_attrs, 263);
    let base_per = ctx.db.with_snapshot(|snap| {
        eve_server::services::character_skills::effective_character_attributes(&ctx.sde, snap, PILOT).3
    });

    // 1. Skill check: plugging 16246 (requires Cybernetics IV) at Cybernetics I fails with SkillPrerequisitesNotMet.
    let skill_err = rpc(&ctx, &mut s, "dogmaIM", "InjectImplant", vec![PyRep::Integer(imp_slot7_shield_id)])
        .await
        .expect_err("Cybernetics IV required");
    assert_eq!(skill_err.code, "SkillPrerequisitesNotMet");

    // 2. Plug in 9899 (slot 1, requires Cybernetics I) -> succeeds and increases perception by +3.
    rpc(&ctx, &mut s, "dogmaIM", "InjectImplant", vec![PyRep::Integer(imp_slot1_basic_id)])
        .await
        .expect("InjectImplant 9899");
    let boosted_per = ctx.db.with_snapshot(|snap| {
        eve_server::services::character_skills::effective_character_attributes(&ctx.sde, snap, PILOT).3
    });
    assert!((boosted_per - (base_per + 3.0)).abs() < 1e-6, "expected +3 perception from 9899");

    // 3. Train Cybernetics IV, then try to plug 10216 into already-occupied slot 1 -> refused with ImplantSlotOccupied.
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(PILOT).or_default().insert(
            3411,
            eve_db::TrainedSkillRecord { type_id: 3411, level: 4, skill_points: 45_255.0 },
        );
    });
    let occupied_err = rpc(&ctx, &mut s, "dogmaIM", "InjectImplant", vec![PyRep::Integer(imp_slot1_std_id)])
        .await
        .expect_err("slot 1 occupied");
    assert_eq!(occupied_err.code, "ImplantSlotOccupied");

    // 4. Plug in 16246 (slot 7, +5% shieldCapacity) -> succeeds and increases ship shieldCapacity by 5%.
    rpc(&ctx, &mut s, "dogmaIM", "InjectImplant", vec![PyRep::Integer(imp_slot7_shield_id)])
        .await
        .expect("InjectImplant 16246");
    let boosted_ship_attrs = rpc(&ctx, &mut s, "dogmaIM", "GetAllInfo", vec![]).await.unwrap();
    let boosted_shield = ship_attr(&boosted_ship_attrs, 263);
    assert!(
        (boosted_shield - base_shield * 1.05).abs() < 1e-3,
        "expected +5% shieldCapacity ({} -> {}), got {}",
        base_shield,
        base_shield * 1.05,
        boosted_shield
    );

    let implants_dict = rpc(&ctx, &mut s, "skillMgr2", "GetImplants", vec![]).await.expect("GetImplants");
    let PyRep::Dict(imp_entries) = implants_dict else { panic!("expected Dict from GetImplants") };
    assert_eq!(imp_entries.len(), 2);

    // 5. DestroyImplant on 9899 permanently removes it from the world (not returned to hangar or cargo) and frees slot 1.
    rpc(&ctx, &mut s, "dogmaIM", "DestroyImplant", vec![PyRep::Integer(imp_slot1_basic_id)])
        .await
        .expect("DestroyImplant 9899");
    let item_exists = ctx.db.with_snapshot(|snap| {
        snap.items.contains_key(&imp_slot1_basic_id)
            || snap.items.values().any(|i| i.owner_id == PILOT && i.type_id == 9899)
    });
    assert!(!item_exists, "destroyed implant must not exist anywhere in inventory");
    let after_destroy_per = ctx.db.with_snapshot(|snap| {
        eve_server::services::character_skills::effective_character_attributes(&ctx.sde, snap, PILOT).3
    });
    assert!((after_destroy_per - base_per).abs() < 1e-6, "perception bonus removed after DestroyImplant");

    // Slot 1 is now free, so plugging 10216 (+4 perception) succeeds.
    rpc(&ctx, &mut s, "dogmaIM", "InjectImplant", vec![PyRep::Integer(imp_slot1_std_id)])
        .await
        .expect("InjectImplant 10216 after slot 1 freed");
    let std_per = ctx.db.with_snapshot(|snap| {
        eve_server::services::character_skills::effective_character_attributes(&ctx.sde, snap, PILOT).3
    });
    assert!((std_per - (base_per + 4.0)).abs() < 1e-6, "expected +4 perception from 10216");
    let _ = ship_id;
}

#[tokio::test]
async fn test_booster_consume_and_side_effects_neurotoxin_skills() {
    // parity: feat:attributes.booster-consume, feat:attributes.booster-side-effects
    eve_testkit::attest(&[
        "feat:attributes.booster-consume",
        "feat:attributes.booster-side-effects",
    ]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut s = SessionState::new(52_050);
    common::login_pilot(&ctx, &mut s, "demo", "demo-secret").await;
    rpc(&ctx, &mut s, "charUnboundMgr", "SelectCharacterID", vec![PyRep::Integer(PILOT)]).await.unwrap();

    let blue_pill_1 = 88_200_001_i64; // 9950 Standard Blue Pill Booster (slot 1, 1800s base, requires Biology I)
    let blue_pill_2 = 88_200_002_i64; // second 9950 for slot conflict & Neurotoxin V test
    let accel_id = 88_200_003_i64;    // 49539 Standard Cerebral Accelerator (slot 14, 86400s base, +4 all attrs, followsJumpClones=1)

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(skills) = snap.skills.get_mut(&PILOT) {
            skills.remove(&3405);  // Biology
            skills.remove(&25530); // Neurotoxin Recovery
            skills.remove(&25538); // Neurotoxin Control
        }
        for (iid, tid) in [
            (blue_pill_1, 9950_u32),
            (blue_pill_2, 9950_u32),
            (accel_id, 49539_u32),
        ] {
            snap.items.insert(
                iid,
                eve_db::InventoryItemRecord {
                    item_id: iid,
                    type_id: tid,
                    owner_id: PILOT,
                    location_id: 60_003_760,
                    flag: eve_db::InventoryFlag::Hangar,
                    quantity: 1,
                    singleton: false,
                    version: 1,
                    ..Default::default()
                },
            );
        }
    });

    // Baseline ship shieldCapacity (attr 263).
    let base_ship_attrs = rpc(&ctx, &mut s, "dogmaIM", "GetAllInfo", vec![]).await.unwrap();
    let base_shield = ship_attr(&base_ship_attrs, 263);

    // 1. Consuming 9950 without Biology I fails with SkillPrerequisitesNotMet.
    let no_bio = rpc(&ctx, &mut s, "dogmaIM", "UseBooster", vec![PyRep::Integer(blue_pill_1)])
        .await
        .expect_err("Biology I required for 9950");
    assert_eq!(no_bio.code, "SkillPrerequisitesNotMet");

    // 2. Train Biology I (3405 lvl 1 -> +20% duration = 2160s), Neurotoxin Recovery 0, Neurotoxin Control 0.
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(PILOT).or_default().insert(
            3405,
            eve_db::TrainedSkillRecord { type_id: 3405, level: 1, skill_points: 250.0 },
        );
    });
    let use1 = rpc(&ctx, &mut s, "dogmaIM", "UseBooster", vec![PyRep::Integer(blue_pill_1)])
        .await
        .expect("UseBooster 1");
    assert_eq!(use1.dict_get("boosterSlot").and_then(|v| v.as_i64()), Some(1));
    assert_eq!(use1.dict_get("durationSeconds").and_then(|v| v.as_i64()), Some(2160));
    let Some(PyRep::List(se1)) = use1.dict_get("sideEffectIDs") else {
        panic!("expected sideEffectIDs list: {use1:?}");
    };
    let se1_ids: Vec<i64> = se1.iter().filter_map(|v| v.as_i64()).collect();
    // At base chance 0.20 (Neurotoxin Recovery 0), slots 0 (2737 shieldCapacityPenalty) and 1 (2739 turretOptimalRangePenalty) trigger.
    assert_eq!(se1_ids, vec![2737, 2739]);

    // 3. Slot conflict: consuming another slot-1 booster while blue_pill_1 is active fails with BoosterSlotOccupied.
    let slot_err = rpc(&ctx, &mut s, "dogmaIM", "UseBooster", vec![PyRep::Integer(blue_pill_2)])
        .await
        .expect_err("slot 1 occupied");
    assert_eq!(slot_err.code, "BoosterSlotOccupied");

    // 4. Side-effect penalty 2737 (-20% shieldCapacity) reduces ship shieldCapacity to 80% at Neurotoxin Control 0.
    let pen0_ship_attrs = rpc(&ctx, &mut s, "dogmaIM", "GetAllInfo", vec![]).await.unwrap();
    let pen0_shield = ship_attr(&pen0_ship_attrs, 263);
    assert!(
        (pen0_shield - base_shield * 0.80).abs() < 1e-3,
        "expected -20% shieldCapacity from side-effect 2737 ({} -> {}), got {}",
        base_shield,
        base_shield * 0.80,
        pen0_shield
    );

    // 5. Advance virtual clock by 2161s -> blue_pill_1 expires, slot 1 is free, and ship shieldCapacity returns to 100%.
    ctx.advance_virtual_clock(2161);
    let boosters_after_exp = rpc(&ctx, &mut s, "skillMgr2", "GetBoosters", vec![]).await.unwrap();
    let PyRep::Dict(b_entries) = boosters_after_exp else { panic!("expected Dict") };
    assert!(b_entries.is_empty(), "booster must expire after its Biology-scaled duration");
    let restored_attrs = rpc(&ctx, &mut s, "dogmaIM", "GetAllInfo", vec![]).await.unwrap();
    let restored_shield = ship_attr(&restored_attrs, 263);
    assert!((restored_shield - base_shield).abs() < 1e-3, "shieldCapacity restored after booster expiry");

    // 6. Train Biology V (duration = 1800 * 2.0 = 3600s), Neurotoxin Recovery V (chance = 0.20 * 0.75 = 0.15 -> only 2737 triggers),
    //    and Neurotoxin Control V (penalty severity reduced by 25% -> -15% shieldCapacity instead of -20%).
    ctx.db.with_mut_snapshot(|snap| {
        let skills = snap.skills.entry(PILOT).or_default();
        skills.insert(3405, eve_db::TrainedSkillRecord { type_id: 3405, level: 5, skill_points: 256_000.0 });
        skills.insert(25530, eve_db::TrainedSkillRecord { type_id: 25530, level: 5, skill_points: 1_280_000.0 });
        skills.insert(25538, eve_db::TrainedSkillRecord { type_id: 25538, level: 5, skill_points: 512_000.0 });
    });
    let use2 = rpc(&ctx, &mut s, "dogmaIM", "UseBooster", vec![PyRep::Integer(blue_pill_2)])
        .await
        .expect("UseBooster 2 with Biology V & Neurotoxin V");
    assert_eq!(use2.dict_get("durationSeconds").and_then(|v| v.as_i64()), Some(3600));
    let Some(PyRep::List(se2)) = use2.dict_get("sideEffectIDs") else {
        panic!("expected sideEffectIDs list");
    };
    let se2_ids: Vec<i64> = se2.iter().filter_map(|v| v.as_i64()).collect();
    assert_eq!(se2_ids, vec![2737], "Neurotoxin Recovery V reduces triggered side effects from [2737, 2739] to [2737]");

    let pen5_ship_attrs = rpc(&ctx, &mut s, "dogmaIM", "GetAllInfo", vec![]).await.unwrap();
    let pen5_shield = ship_attr(&pen5_ship_attrs, 263);
    assert!(
        (pen5_shield - base_shield * 0.85).abs() < 1e-3,
        "Neurotoxin Control V reduces shieldCapacity penalty to -15% ({} -> {}), got {}",
        base_shield,
        base_shield * 0.85,
        pen5_shield
    );

    // 7. Consume Cerebral Accelerator (49539, slot 14, followsJumpClones=1, requires Science I = 3402) and verify CloneJump removes
    //    combat booster 9950 while preserving 49539.
    ctx.db.with_mut_snapshot(|snap| {
        let skills = snap.skills.entry(PILOT).or_default();
        skills.insert(3402, eve_db::TrainedSkillRecord { type_id: 3402, level: 1, skill_points: 250.0 });
        skills.insert(24242, eve_db::TrainedSkillRecord { type_id: 24242, level: 1, skill_points: 250.0 });
    });
    rpc(&ctx, &mut s, "dogmaIM", "UseBooster", vec![PyRep::Integer(accel_id)])
        .await
        .expect("UseBooster Cerebral Accelerator");
    let jc = rpc(&ctx, &mut s, "jumpCloneSvc", "InstallCloneInStation", vec![]).await.unwrap();
    let jc_id = jc.dict_get("jumpCloneID").and_then(|v| v.as_i64()).unwrap();
    rpc(&ctx, &mut s, "jumpCloneSvc", "CloneJump", vec![PyRep::Integer(jc_id)]).await.expect("CloneJump");
    let boosters_after_jump = rpc(&ctx, &mut s, "skillMgr2", "GetBoosters", vec![]).await.unwrap();
    let PyRep::Dict(after_jump_entries) = boosters_after_jump else { panic!("expected Dict") };
    assert_eq!(after_jump_entries.len(), 1, "only followsJumpClones booster (49539) survives CloneJump");
    assert_eq!(after_jump_entries[0].0.as_i64(), Some(49539));
}
