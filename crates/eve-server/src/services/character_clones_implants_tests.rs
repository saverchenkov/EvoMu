//! Unit tests for `character_clones_implants` (`charMgr`/`charUnboundMgr` sheet & stations, `jumpCloneSvc` install/state/price/destroy, `standingMgr`, and `paperDollServer`).

#![forbid(unsafe_code)]

use super::*;
use crate::router::ServerContext;
use eve_db::types::{
    KillRightRecord, StandingTransactionRecord, TrainedSkillRecord, UpwellStructureRecord,
};
use eve_db::{InventoryFlag, InventoryItemRecord, IskCents, JumpCloneRecord, WorldStateSnapshot};
use eve_machonet::{service_roles, SessionState};
use eve_proto::PyRep;
use eve_sde::{InvType, SdeRegistry};
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

fn put_item(
    snap: &mut WorldStateSnapshot,
    type_id: u32,
    owner_id: i64,
    location_id: i64,
    flag: InventoryFlag,
    quantity: i32,
    singleton: bool,
) -> i64 {
    snap.next_entity_id += 1;
    let item_id = snap.next_entity_id;
    snap.items.insert(
        item_id,
        InventoryItemRecord {
            item_id,
            type_id,
            owner_id,
            location_id,
            flag,
            quantity,
            singleton,
            version: 1,
            ..Default::default()
        },
    );
    item_id
}

fn rep_list_len(rep: &PyRep) -> Option<usize> {
    match rep {
        PyRep::List(v) | PyRep::Tuple(v) => Some(v.len()),
        _ => None,
    }
}

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn test_ctx() -> ServerContext {
    if let Some(sde) = SHARED_SDE.get() {
        ServerContext::with_sde_and_repository(
            Arc::clone(sde),
            Arc::new(eve_db::WorldRepository::with_demo_seed()),
        )
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
    } else {
        let ctx = ServerContext::new();
        let _ = SHARED_SDE.set(Arc::clone(&ctx.sde));
        ctx
    }
}

fn clone_type_with(
    sde: &mut Arc<SdeRegistry>,
    base_tid: u32,
    new_tid: u32,
    group_id: u32,
    category_id: u32,
) {
    let reg = Arc::make_mut(sde);
    let mut rec: InvType = reg.types.values().next().cloned().expect("base sde type");
    if let Some(b) = reg.types.get(&base_tid) {
        rec = b.clone();
    }
    rec.type_id = new_tid;
    rec.group_id = group_id;
    rec.category_id = category_id;
    rec.name = format!("TestType_{new_tid}");
    rec.volume = 1.0;
    rec.capacity = 0.0;
    rec.default_attributes.clear();
    reg.types.insert(new_tid, rec);
}

fn make_session(
    user_id: u64,
    char_id: i64,
    corp_id: i64,
    station_id: Option<i64>,
    structure_id: Option<i64>,
) -> SessionState {
    let mut s = SessionState::new(750);
    s.user_id = Some(user_id);
    s.character_id = Some(char_id);
    s.corporation_id = Some(corp_id);
    s.solar_system_id = Some(30000142);
    s.station_id = station_id.or(structure_id);
    s.structure_id = structure_id;
    s
}

fn kv_get<'a>(rep: &'a PyRep, key: &str) -> Option<&'a PyRep> {
    let PyRep::KeyVal(kv) = rep else {
        return None;
    };
    kv.get(key)
}

#[test]
fn get_character_info_and_list_stations_cover_alpha_omega_and_ownership() {
    let ctx = test_ctx();
    let station_id = ctx
        .sde
        .stations
        .keys()
        .copied()
        .next()
        .unwrap_or(60_003_760);

    let (user_id, char_id, other_cid) = ctx.db.with_mut_snapshot(|snap| {
        let mut iter = snap.characters.values_mut();
        let c1 = iter.next().expect("demo character 1");
        c1.clone_grade = 1; // Alpha cap check
        c1.station_id = Some(station_id);
        c1.standings.insert(1000035, 3.25);
        let uid = c1.user_id;
        let cid = c1.character_id;
        let other = iter
            .find(|c| c.user_id != uid)
            .map(|c| c.character_id)
            .unwrap_or(99_999_999);

        snap.skills.entry(cid).or_default().insert(
            3300,
            TrainedSkillRecord {
                type_id: 3300,
                level: 5,
                skill_points: 256_000.0,
            },
        );
        snap.active_implants.insert(cid, vec![9899, 9941]);
        snap.jump_clones.insert(
            cid,
            vec![JumpCloneRecord {
                jump_clone_id: 4001,
                character_id: cid,
                location_id: station_id,
                implants: vec![9899],
            }],
        );
        snap.kill_rights.insert(
            8001,
            KillRightRecord {
                kill_right_id: 8001,
                owner_id: cid,
                target_id: 95_001,
                is_active_suspect: true,
                activation_fee: IskCents::from_isk(5_000_000),
            },
        );
        snap.items.insert(
            901_001,
            InventoryItemRecord {
                item_id: 901_001,
                type_id: 34,
                owner_id: cid,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 100,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        (uid, cid, other)
    });

    let mut sess = make_session(user_id, char_id, 98000001, Some(station_id), None);
    let empty_kw = HashMap::new();

    // Ownership mismatch rejected
    let bad_own = handle_clones_and_sheet(
        &ctx,
        &mut sess,
        "charMgr",
        "GetCharacterInfo",
        &[PyRep::Integer(other_cid)],
        &empty_kw,
    )
    .expect("handled")
    .expect_err("ownership mismatch");
    assert_eq!(bad_own.code, "CharacterOwnershipMismatch");

    // Valid sheet returned
    let sheet = handle_clones_and_sheet(
        &ctx,
        &mut sess,
        "charUnboundMgr",
        "GetCharacterInfo",
        &[PyRep::Integer(char_id)],
        &empty_kw,
    )
    .expect("handled")
    .expect("sheet ok");

    assert_eq!(kv_get(&sheet, "characterID").and_then(PyRep::as_i64), Some(char_id));
    assert_eq!(kv_get(&sheet, "cloneGrade").and_then(PyRep::as_i64), Some(1));
    assert_eq!(
        kv_get(&sheet, "implants").and_then(rep_list_len),
        Some(2)
    );
    assert_eq!(
        kv_get(&sheet, "jumpClones").and_then(rep_list_len),
        Some(1)
    );
    assert_eq!(
        kv_get(&sheet, "killRights").and_then(rep_list_len),
        Some(1)
    );

    // ListStations reports items in station_id
    let stations_rs = handle_clones_and_sheet(
        &ctx,
        &mut sess,
        "charMgr",
        "ListStations",
        &[],
        &empty_kw,
    )
    .expect("handled")
    .expect("stations ok");
    let PyRep::Rowset { rows, .. } = stations_rs else {
        panic!("expected Rowset");
    };
    assert!(rows.iter().any(|r| r.first().and_then(PyRep::as_i64) == Some(station_id)));
}

#[test]
fn jump_clone_install_station_and_structure_lifecycle_and_destroy() {
    let ctx = test_ctx();
    let clone_station_id = ctx
        .sde
        .stations
        .values()
        .find(|st| st.has_clone_bay && st.station_id != 60_015_187)
        .map(|st| st.station_id)
        .expect("NPC station with clone bay");

    let struct_id = 1_040_777_001;
    let empty_kw = HashMap::new();

    let (user_id, char_id) = ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.values_mut().next().expect("demo char");
        let cid = ch.character_id;
        let uid = ch.user_id;
        let ch_corp = ch.corporation_id;
        ch.clone_grade = 2; // Omega
        ch.station_id = Some(clone_station_id);
        ch.balance = IskCents::from_isk(25_000_000);
        snap.jump_clones.remove(&cid);

        // Infomorph Psychology II + Adv Infomorph Psych II -> 4 slots
        let skills = snap.skills.entry(cid).or_default();
        skills.insert(
            24242,
            TrainedSkillRecord {
                type_id: 24242,
                level: 2,
                skill_points: 16_000.0,
            },
        );
        skills.insert(
            33407,
            TrainedSkillRecord {
                type_id: 33407,
                level: 2,
                skill_points: 64_000.0,
            },
        );

        // Upwell structure with Standup Cloning Center (35894)
        snap.structures.insert(
            struct_id,
            UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 35832,
                owner_corp_id: ch_corp,
                solar_system_id: 30000142,
                name: "CloneCitadel".into(),
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 200,
                position: [1_000_000.0, 0.0, 0.0],
                ..Default::default()
            },
        );
        let bay_id = put_item(
            snap,
            35894,
            ch_corp,
            struct_id,
            InventoryFlag::ServiceSlot0,
            1,
            true,
        );
        snap.module_online_states.insert(bay_id, true);
        (uid, cid)
    });

    let mut st_sess = make_session(user_id, char_id, 98000001, Some(clone_station_id), None);

    // 1. Price check at NPC station
    let price = handle_clones_and_sheet(
        &ctx,
        &mut st_sess,
        "jumpCloneSvc",
        "GetPriceForClone",
        &[],
        &empty_kw,
    )
    .expect("handled")
    .expect("price ok");
    assert_eq!(price.as_f64(), Some(900_000.0));

    // 2. Install at NPC station
    let installed = handle_clones_and_sheet(
        &ctx,
        &mut st_sess,
        "jumpCloneSvc",
        "InstallCloneInStation",
        &[],
        &empty_kw,
    )
    .expect("handled")
    .expect("install in station ok");
    let jc1 = kv_get(&installed, "jumpCloneID").and_then(PyRep::as_i64).expect("jc1 id");
    assert_eq!(kv_get(&installed, "locationID").and_then(PyRep::as_i64), Some(clone_station_id));

    // Duplicate install at same station fails
    let dup = handle_clones_and_sheet(
        &ctx,
        &mut st_sess,
        "jumpCloneSvc",
        "InstallCloneInStation",
        &[],
        &empty_kw,
    )
    .expect("handled")
    .expect_err("duplicate rejected");
    assert_eq!(dup.code, "JumpCloneAlreadyAtStation");

    // 3. Install in Upwell structure
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.structure_id = Some(struct_id);
        }
    });
    let mut up_sess = make_session(user_id, char_id, 98000001, None, Some(struct_id));
    let up_installed = handle_clones_and_sheet(
        &ctx,
        &mut up_sess,
        "jumpCloneSvc",
        "InstallCloneInStructure",
        &[],
        &empty_kw,
    )
    .expect("handled")
    .expect("install in structure ok");
    let jc2 = kv_get(&up_installed, "jumpCloneID").and_then(PyRep::as_i64).expect("jc2 id");

    // 4. Query station & full clone states
    let st_state = handle_clones_and_sheet(
        &ctx,
        &mut up_sess,
        "jumpCloneSvc",
        "GetStationCloneState",
        &[],
        &empty_kw,
    )
    .expect("handled")
    .expect("station clone state ok");
    let PyRep::Rowset { rows, .. } = st_state else {
        panic!("expected rowset");
    };
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0].as_i64(), Some(jc2));

    let full_state = handle_clones_and_sheet(
        &ctx,
        &mut up_sess,
        "jumpCloneSvc",
        "GetCloneState",
        &[],
        &empty_kw,
    )
    .expect("handled")
    .expect("full clone state ok");
    assert!(kv_get(&full_state, "clones").is_some());

    // 5. Destroy both installed clones
    let del1 = handle_clones_and_sheet(
        &ctx,
        &mut up_sess,
        "jumpCloneSvc",
        "DestroyInstalledClone",
        &[PyRep::Integer(jc1)],
        &empty_kw,
    )
    .expect("handled")
    .expect("destroy ok");
    assert_eq!(del1.as_bool(), Some(true));

    let del_missing = handle_clones_and_sheet(
        &ctx,
        &mut up_sess,
        "jumpCloneSvc",
        "DestroyInstalledClone",
        &[PyRep::Integer(jc1)],
        &empty_kw,
    )
    .expect("handled")
    .expect_err("already deleted");
    assert_eq!(del_missing.code, "CloneNotFound");
}

#[test]
fn standing_mgr_and_paper_doll_server_handle_queries_and_apparel() {
    let mut ctx = test_ctx();
    let apparel_tid = 99_901;
    clone_type_with(&mut ctx.sde, 34, apparel_tid, 1085, 30);

    let station_id = 60_003_760;
    let (user_id, char_id, apparel_item_id) = ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.values_mut().next().expect("demo char");
        let cid = ch.character_id;
        let uid = ch.user_id;
        ch.station_id = Some(station_id);
        ch.standings.insert(1000044, 4.75);
        snap.standing_transactions.insert(
            cid,
            vec![StandingTransactionRecord {
                event_id: 501,
                from_id: 1000044,
                to_id: cid,
                modification: 0.25,
                event_type_id: 12,
            }],
        );
        let iid = put_item(
            snap,
            apparel_tid,
            cid,
            station_id,
            InventoryFlag::Hangar,
            2,
            false,
        );
        snap.service_state_blobs.insert(format!("resculpt_pending:{cid}"), "true".into());
        (uid, cid, iid)
    });

    let mut sess = make_session(user_id, char_id, 98000001, Some(station_id), None);
    sess.role |= service_roles::ROLE_GMH;
    let empty_kw = HashMap::new();

    // Standing queries
    let standings = handle_clones_and_sheet(
        &ctx,
        &mut sess,
        "standingMgr",
        "GetCharStandings",
        &[],
        &empty_kw,
    )
    .expect("handled")
    .expect("standings ok");
    let PyRep::Rowset { rows: st_rows, .. } = standings else {
        panic!("expected rowset");
    };
    assert!(st_rows.iter().any(|r| r.first().and_then(PyRep::as_i64) == Some(1000044)));

    let txs = handle_clones_and_sheet(
        &ctx,
        &mut sess,
        "standingMgr",
        "GetStandingTransactions",
        &[PyRep::Integer(1000044)],
        &empty_kw,
    )
    .expect("handled")
    .expect("txs ok");
    let PyRep::Rowset { rows: tx_rows, .. } = txs else {
        panic!("expected rowset");
    };
    assert_eq!(tx_rows.len(), 1);

    // UpdateExistingCharacterFull equips 1 unit from stacked hangar apparel into Wardrobe
    let doll_payload = PyRep::keyval([
        ("hairDarkness", PyRep::Real(0.4)),
        ("apparel", PyRep::List(vec![PyRep::Integer(apparel_item_id)])),
    ]);
    let portrait_payload = PyRep::keyval([("cameraPose", PyRep::Real(1.2))]);
    let upd = handle_clones_and_sheet(
        &ctx,
        &mut sess,
        "paperDollServer",
        "UpdateExistingCharacterFull",
        &[doll_payload, portrait_payload],
        &empty_kw,
    )
    .expect("handled")
    .expect("update doll ok");
    assert_eq!(kv_get(&upd, "updated").and_then(PyRep::as_bool), Some(true));
    assert_eq!(
        ctx.db.get_item(apparel_item_id).map(|i| i.quantity),
        Some(1)
    );

    // PaperDoll read endpoints
    let my_doll = handle_clones_and_sheet(
        &ctx,
        &mut sess,
        "paperDollServer",
        "GetMyPaperDollData",
        &[],
        &empty_kw,
    )
    .expect("handled")
    .expect("GetMyPaperDollData ok");
    assert_eq!(kv_get(&my_doll, "characterID").and_then(PyRep::as_i64), Some(char_id));

    let doll_data = handle_clones_and_sheet(
        &ctx,
        &mut sess,
        "paperDollServer",
        "GetPaperDollData",
        &[PyRep::Integer(char_id)],
        &empty_kw,
    )
    .expect("handled")
    .expect("GetPaperDollData ok");
    assert!(
        matches!(
            &doll_data,
            PyRep::List(seq)
                if seq.len() == 1
                    && matches!(seq.first(), Some(PyRep::String(g)) if g == "female" || g == "male")
        ),
        "GetPaperDollData must return [gender_label] for Doll.MatchDNA, got {doll_data:?}"
    );

    let missing_doll = handle_clones_and_sheet(
        &ctx,
        &mut sess,
        "paperDollServer",
        "GetPaperDollData",
        &[PyRep::Integer(999_999_999)],
        &empty_kw,
    )
    .expect("handled")
    .expect_err("unknown charID must return CharacterNotFound");
    assert_eq!(missing_doll.code, "CharacterNotFound");

    let batch = handle_clones_and_sheet(
        &ctx,
        &mut sess,
        "paperDollServer",
        "GetPaperDollDataBatch",
        &[PyRep::List(vec![PyRep::Integer(char_id)])],
        &empty_kw,
    )
    .expect("handled")
    .expect("GetPaperDollDataBatch ok");
    assert!(matches!(batch, PyRep::Dict(d) if d.len() == 1));

    let portrait = handle_clones_and_sheet(
        &ctx,
        &mut sess,
        "paperDollServer",
        "GetPaperDollPortraitDataFor",
        &[PyRep::Integer(char_id)],
        &empty_kw,
    )
    .expect("handled")
    .expect("GetPaperDollPortraitDataFor ok");
    assert_eq!(kv_get(&portrait, "characterID").and_then(PyRep::as_i64), Some(char_id));
}
