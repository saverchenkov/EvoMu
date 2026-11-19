//! Unit tests for `character_clones_implants` (`charMgr`/`charUnboundMgr` sheet & stations, `jumpCloneSvc` install/state/price/destroy, `standingMgr`, and `paperDollServer`).

#![forbid(unsafe_code)]

use super::*;
use crate::router::ServerContext;
use eve_db::types::{
    KillRightRecord, StandingTransactionRecord, TrainedSkillRecord, UpwellStructureRecord,
};
use eve_db::{
    corp_roles, CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents, JumpCloneRecord,
    WorldStateSnapshot,
};
use eve_machonet::{service_roles, SessionState};
use eve_proto::PyRep;
use eve_sde::{InvType, SdeRegistry};
use std::collections::{HashMap, HashSet};
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

    // ListStations reports items in station_id with V24 typeID column
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
    let PyRep::Rowset { columns, rows } = stations_rs else {
        panic!("expected Rowset");
    };
    assert_eq!(
        columns,
        vec!["stationID", "solarSystemID", "itemCount", "typeID"]
    );
    let expected_st = ctx.sde.stations.get(&station_id).expect("station in SDE");
    let row = rows
        .iter()
        .find(|r| r.first().and_then(PyRep::as_i64) == Some(station_id))
        .expect("station row present");
    assert_eq!(row[1], PyRep::Integer(i64::from(expected_st.solar_system_id)));
    assert!(row[2].as_i64().unwrap_or(0) >= 1);
    assert_eq!(row[3], PyRep::Integer(i64::from(expected_st.station_type_id)));
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
        snap.cluster_and_runtime.resculpt_pending.insert(cid);
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
    assert_eq!(kv_get(&doll_data, "characterID").and_then(PyRep::as_i64), Some(char_id));
    assert_eq!(kv_get(&doll_data, "modifiers"), Some(&PyRep::List(Vec::new())));
    assert_eq!(kv_get(&doll_data, "colors"), Some(&PyRep::List(Vec::new())));
    assert_eq!(kv_get(&doll_data, "sculpts"), Some(&PyRep::List(Vec::new())));
    assert!(matches!(kv_get(&doll_data, "appearance"), Some(PyRep::KeyVal(_))));

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
    let PyRep::Tuple(parts) = portrait else {
        panic!("expected 2-tuple from GetPaperDollPortraitDataFor");
    };
    assert_eq!(parts.len(), 2);
    assert_eq!(parts[1], PyRep::None);
    let PyRep::List(rows) = &parts[0] else {
        panic!("expected list at [0]");
    };
    assert_eq!(rows.len(), 1);
    assert!(matches!(rows[0], PyRep::DBRow { .. }));
    assert!(rows[0].dict_get("backgroundID").is_some());
    assert!(rows[0].dict_get("cameraX").is_some());
}

#[test]
fn upwell_clone_bay_scalar_setting_23_and_legacy_35894_pricing_across_rpcs() {
    use crate::services::station_structure::structure_profiles_rpc::handle_save_profile_settings;
    use crate::services::station_structure::structure_upwell_ops::{
        load_ext_state, save_ext_state, StructureAccessProfile,
    };
    use crate::services::structure_profile_services::{
        SETTING_CLONINGBAY_TAX, SETTING_HOUSING_CAN_DOCK,
    };
    use eve_db::{corp_roles, CorporationRecord};

    let ctx = test_ctx();
    let empty_kw = HashMap::new();
    let sys_id = 30_000_142_u32;
    let owner_corp = 98_923_001_i64;
    let pilot_corp = 98_923_002_i64;
    let director_id = 90_923_001_i64;
    let char_struct_id = 90_923_002_i64;
    let char_station_id = 90_923_003_i64;
    let char_zero_id = 90_923_004_i64;

    let sid_default = 1_000_923_000_001_i64;
    let sid_paid = 1_000_923_000_002_i64;
    let sid_free = 1_000_923_000_003_i64;
    let sid_legacy_paid = 1_000_923_000_004_i64;
    let sid_legacy_free = 1_000_923_000_005_i64;

    let pid_default = 923_100_i64;
    let pid_paid = 923_101_i64;
    let pid_free = 923_102_i64;
    let pid_legacy_paid = 923_103_i64;
    let pid_legacy_free = 923_104_i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.insert(
            owner_corp,
            CorporationRecord::new(owner_corp, "Clone Owners", "CLWN", director_id),
        );
        snap.corporations.insert(
            pilot_corp,
            CorporationRecord::new(pilot_corp, "Clone Pilots", "CLPL", char_struct_id),
        );

        let template_ch = snap.characters.values().next().cloned().expect("demo char");
        for (cid, name, corp_id, bal) in [
            (director_id, "Director", owner_corp, IskCents::from_isk(10_000_000)),
            (char_struct_id, "StructPilot", pilot_corp, IskCents::from_isk(10_000_000)),
            (char_station_id, "StationPilot", pilot_corp, IskCents::from_isk(10_000_000)),
            (char_zero_id, "ZeroIskPilot", pilot_corp, IskCents(0)),
        ] {
            let mut ch = template_ch.clone();
            ch.character_id = cid;
            ch.user_id = cid as u64;
            ch.name = name.into();
            ch.corporation_id = corp_id;
            ch.alliance_id = None;
            ch.clone_grade = 2; // Omega
            ch.balance = bal;
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = Some(sid_default);
            snap.characters.insert(cid, ch);
            snap.jump_clones.remove(&cid);
            snap.skills.entry(cid).or_default().insert(
                24_242,
                TrainedSkillRecord {
                    type_id: 24_242,
                    level: 5,
                    skill_points: 256_000.0,
                },
            );
        }

        for sid in [sid_default, sid_paid, sid_free, sid_legacy_paid, sid_legacy_free] {
            snap.structures.insert(
                sid,
                UpwellStructureRecord {
                    structure_id: sid,
                    type_id: 35_832,
                    owner_corp_id: owner_corp,
                    solar_system_id: sys_id,
                    name: format!("CloneStruct-{sid}"),
                    state: "ShieldVulnerable".into(),
                    quantum_core_installed: true,
                    fuel_blocks: 200,
                    ..Default::default()
                },
            );
            let mod_id = put_item(
                snap,
                35_894,
                owner_corp,
                sid,
                InventoryFlag::ServiceSlot0,
                1,
                true,
            );
            snap.module_online_states.insert(mod_id, true);
        }

        let mut ext = load_ext_state(snap);
        for (sid, pid, legacy_override) in [
            (sid_default, pid_default, None),
            (sid_paid, pid_paid, None),
            (sid_free, pid_free, None),
            (sid_legacy_paid, pid_legacy_paid, Some(250_000_i64)),
            (sid_legacy_free, pid_legacy_free, Some(0_i64)),
        ] {
            let mut prof = StructureAccessProfile {
                profile_id: pid,
                owner_corp_id: owner_corp,
                corp_only: false,
                ..Default::default()
            };
            prof.general_settings.insert(SETTING_HOUSING_CAN_DOCK, true);
            if let Some(isk) = legacy_override {
                prof.service_settings.insert(35_894, isk);
            }
            ext.structure_profile_id.insert(sid, pid);
            ext.profiles_by_id.insert(pid, prof.clone());
            ext.access_profiles.insert(sid, prof);
        }
        save_ext_state(snap, &ext);
    });

    let mut director_sess = make_session(director_id as u64, director_id, owner_corp, None, None);
    director_sess.corp_role = corp_roles::ROLE_DIRECTOR;

    // Configure flat scalar SETTING_CLONINGBAY_TAX (23) = 250_000.0 ISK and 0.0 ISK via SaveProfileSettings.
    handle_save_profile_settings(
        &ctx,
        &director_sess,
        &[
            PyRep::Integer(pid_paid),
            PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::Integer(SETTING_CLONINGBAY_TAX),
                PyRep::Real(250_000.0),
            ])]),
        ],
    )
    .expect("save scalar 250_000 ISK");
    handle_save_profile_settings(
        &ctx,
        &director_sess,
        &[
            PyRep::Integer(pid_free),
            PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::Integer(SETTING_CLONINGBAY_TAX),
                PyRep::Real(0.0),
            ])]),
        ],
    )
    .expect("save scalar 0.0 ISK");

    let corp_bal = || {
        ctx.db.with_snapshot(|snap| {
            snap.corporations[&owner_corp].master_wallet_balance()
        })
    };
    let char_bal = |cid: i64| {
        ctx.db.with_snapshot(|snap| snap.characters[&cid].balance)
    };
    let set_dock = |cid: i64, sid: i64, as_station: bool| {
        ctx.db.with_mut_snapshot(|snap| {
            let ch = snap.characters.get_mut(&cid).unwrap();
            if as_station {
                ch.station_id = Some(sid);
                ch.structure_id = None;
            } else {
                ch.station_id = None;
                ch.structure_id = Some(sid);
            }
        });
    };

    for (sid, expected_isk) in [
        (sid_default, 900_000_i64),
        (sid_paid, 250_000_i64),
        (sid_free, 0_i64),
        (sid_legacy_paid, 250_000_i64),
        (sid_legacy_free, 0_i64),
    ] {
        let expected_cents = IskCents::from_isk(expected_isk);

        // 1. GetPriceForClone + InstallCloneInStructure
        let struct_pilot = if expected_isk == 0 { char_zero_id } else { char_struct_id };
        set_dock(struct_pilot, sid, false);
        let mut up_sess = make_session(struct_pilot as u64, struct_pilot, pilot_corp, None, Some(sid));
        let price = handle_clones_and_sheet(
            &ctx,
            &mut up_sess,
            "jumpCloneSvc",
            "GetPriceForClone",
            &[],
            &empty_kw,
        )
        .expect("handled")
        .expect("GetPriceForClone ok");
        assert_eq!(price.as_f64(), Some(expected_isk as f64));

        let pilot_before = char_bal(struct_pilot);
        let corp_before = corp_bal();
        let res = handle_clones_and_sheet(
            &ctx,
            &mut up_sess,
            "jumpCloneSvc",
            "InstallCloneInStructure",
            &[],
            &empty_kw,
        )
        .expect("handled")
        .expect("InstallCloneInStructure ok");
        assert_eq!(kv_get(&res, "locationID").and_then(PyRep::as_i64), Some(sid));
        assert_eq!(char_bal(struct_pilot), IskCents(pilot_before.0 - expected_cents.0));
        assert_eq!(corp_bal(), IskCents(corp_before.0 + expected_cents.0));

        // 2. GetPriceForClone + InstallCloneInStation (structure ID passed as station_id)
        set_dock(char_station_id, sid, true);
        let mut st_sess = make_session(char_station_id as u64, char_station_id, pilot_corp, Some(sid), None);
        let st_price = handle_clones_and_sheet(
            &ctx,
            &mut st_sess,
            "jumpCloneSvc",
            "GetPriceForClone",
            &[],
            &empty_kw,
        )
        .expect("handled")
        .expect("GetPriceForClone via station_id ok");
        assert_eq!(st_price.as_f64(), Some(expected_isk as f64));

        let st_pilot_before = char_bal(char_station_id);
        let corp_before = corp_bal();
        let st_res = handle_clones_and_sheet(
            &ctx,
            &mut st_sess,
            "jumpCloneSvc",
            "InstallCloneInStation",
            &[],
            &empty_kw,
        )
        .expect("handled")
        .expect("InstallCloneInStation at structure ok");
        assert_eq!(kv_get(&st_res, "locationID").and_then(PyRep::as_i64), Some(sid));
        assert_eq!(char_bal(char_station_id), IskCents(st_pilot_before.0 - expected_cents.0));
        assert_eq!(corp_bal(), IskCents(corp_before.0 + expected_cents.0));
    }

    // Disabling SETTING_CLONINGBAY_TAX (23) = false rejects GetPriceForClone with StructureAccessDenied.
    handle_save_profile_settings(
        &ctx,
        &director_sess,
        &[
            PyRep::Integer(pid_paid),
            PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::Integer(SETTING_CLONINGBAY_TAX),
                PyRep::Bool(false),
            ])]),
        ],
    )
    .expect("disable clone bay");
    set_dock(char_struct_id, sid_paid, false);
    let mut denied_sess = make_session(char_struct_id as u64, char_struct_id, pilot_corp, None, Some(sid_paid));
    let denied_err = handle_clones_and_sheet(
        &ctx,
        &mut denied_sess,
        "jumpCloneSvc",
        "GetPriceForClone",
        &[],
        &empty_kw,
    )
    .expect("handled")
    .expect_err("disabled clone bay must deny GetPriceForClone");
    assert_eq!(denied_err.code, "StructureAccessDenied");
}

#[test]
fn destroy_installed_clone_unknown_id_does_not_create_empty_entry() {
    let ctx = test_ctx();
    let char_id = 90_000_088_i64;
    let uid = ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().expect("demo char");
        ch.character_id = char_id;
        ch.station_id = Some(60_003_760);
        let uid = ch.user_id;
        snap.characters.insert(char_id, ch);
        uid
    });
    let mut sess = make_session(uid, char_id, 98_000_001, Some(60_003_760), None);
    let empty_kw = HashMap::new();
    let commits_before = ctx.db.tx_commit_count();

    let err = handle_clones_and_sheet(
        &ctx,
        &mut sess,
        "jumpCloneSvc",
        "DestroyInstalledClone",
        &[PyRep::Integer(999_888)],
        &empty_kw,
    )
    .expect("handled")
    .expect_err("unknown clone must return CloneNotFound");
    assert_eq!(err.code, "CloneNotFound");
    assert_eq!(ctx.db.tx_commit_count(), commits_before);
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.jump_clones.contains_key(&char_id));
    });

    ctx.db.with_mut_snapshot(|snap| {
        snap.jump_clones.insert(
            char_id,
            vec![JumpCloneRecord {
                jump_clone_id: 777_001,
                character_id: char_id,
                location_id: 60_003_760,
                implants: Vec::new(),
            }],
        );
    });
    let ok = handle_clones_and_sheet(
        &ctx,
        &mut sess,
        "jumpCloneSvc",
        "DestroyInstalledClone",
        &[PyRep::Integer(777_001)],
        &empty_kw,
    )
    .expect("handled")
    .expect("existing jump clone must be destroyed");
    assert_eq!(ok, PyRep::Bool(true));
    ctx.db.with_snapshot(|snap| {
        assert!(snap.jump_clones.get(&char_id).is_some_and(Vec::is_empty));
    });
}

#[test]
fn list_stations_includes_type_id_filters_and_isolates_non_gm_ownership() {
    let ctx = test_ctx();
    let mut st_iter = ctx.sde.stations.values();
    let st1 = st_iter.next().expect("first SDE station").clone();
    let st2 = st_iter
        .find(|s| s.station_id != st1.station_id)
        .expect("second SDE station")
        .clone();
    let bp_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.category_id == 9)
        .expect("blueprint SDE type")
        .type_id;

    let char_id = 90_000_099_i64;
    let other_char_id = 90_000_199_i64;
    let ceo_id = 90_000_299_i64;
    let plain_id = 90_000_399_i64;
    let sag1_id = 90_000_499_i64;
    let corp_id = 98_000_099_i64;
    let other_corp_id = 98_000_199_i64;
    let struct_a = 1_001_896_954_561_i64;
    let struct_b = 1_048_000_000_099_i64;
    let struct_other = 1_048_000_000_100_i64;
    let struct_destroyed = 1_048_000_000_101_i64;
    let struct_unanchored = 1_048_000_000_102_i64;
    let empty_kw = HashMap::new();

    let uid = ctx.db.with_mut_snapshot(|snap| {
        let base_ch = snap.characters.values().next().cloned().expect("demo char");
        let uid = base_ch.user_id;
        for (cid, cid_corp) in [
            (char_id, corp_id),
            (ceo_id, corp_id),
            (plain_id, corp_id),
            (sag1_id, corp_id),
            (other_char_id, other_corp_id),
        ] {
            let mut ch = base_ch.clone();
            ch.character_id = cid;
            ch.corporation_id = cid_corp;
            ch.station_id = Some(st1.station_id);
            snap.characters.insert(cid, ch);
        }

        let mut corp = CorporationRecord::new(corp_id, "ListStationsTest", "LSTT", ceo_id);
        corp.hq_station_id = Some(st1.station_id);
        corp.members = HashSet::from([ceo_id, char_id, plain_id, sag1_id]);
        corp.member_roles
            .insert(char_id, corp_roles::ROLE_DIRECTOR);
        corp.member_roles
            .insert(sag1_id, corp_roles::ROLE_HANGAR_CAN_QUERY[0]);
        snap.corporations.insert(corp_id, corp);

        let mut other_corp =
            CorporationRecord::new(other_corp_id, "ForeignCorp", "FCRP", other_char_id);
        other_corp.members = HashSet::from([other_char_id]);
        snap.corporations.insert(other_corp_id, other_corp);

        snap.structures.insert(
            struct_a,
            UpwellStructureRecord {
                structure_id: struct_a,
                type_id: 35_832,
                owner_corp_id: corp_id,
                solar_system_id: 30_000_142,
                name: "Player Astrahus".into(),
                ..Default::default()
            },
        );
        snap.structures.insert(
            struct_b,
            UpwellStructureRecord {
                structure_id: struct_b,
                type_id: 35_835,
                owner_corp_id: corp_id,
                solar_system_id: 30_000_144,
                name: "Player Raitaru".into(),
                ..Default::default()
            },
        );
        snap.structures.insert(
            struct_other,
            UpwellStructureRecord {
                structure_id: struct_other,
                type_id: 35_825,
                owner_corp_id: other_corp_id,
                solar_system_id: 30_000_142,
                name: "Foreign Structure".into(),
                ..Default::default()
            },
        );
        snap.structures.insert(
            struct_destroyed,
            UpwellStructureRecord {
                structure_id: struct_destroyed,
                type_id: 35_832,
                owner_corp_id: corp_id,
                solar_system_id: 30_000_142,
                state: "Destroyed".into(),
                name: "Destroyed Astrahus".into(),
                ..Default::default()
            },
        );
        snap.structures.insert(
            struct_unanchored,
            UpwellStructureRecord {
                structure_id: struct_unanchored,
                type_id: 35_832,
                owner_corp_id: corp_id,
                solar_system_id: 30_000_142,
                state: "Unanchored".into(),
                name: "Unanchored Astrahus".into(),
                ..Default::default()
            },
        );

        // Non-station/non-structure/unknown/destroyed locations & zero-qty/marker/fitted items
        // must never appear in ListStations.
        put_item(snap, 34, char_id, 30_000_142, InventoryFlag::None, 5, false);
        put_item(snap, 34, char_id, char_id, InventoryFlag::Wardrobe, 1, true);
        put_item(snap, 34, char_id, 9_900_100, InventoryFlag::Hangar, 1, false);
        put_item(snap, 34, char_id, 69_999_999, InventoryFlag::Hangar, 1, false);
        put_item(snap, 34, char_id, 1_999_999_999_999, InventoryFlag::Hangar, 1, false);
        put_item(snap, 34, char_id, st2.station_id, InventoryFlag::Hangar, 0, false);
        put_item(snap, 34, char_id, st2.station_id, InventoryFlag::HiSlot0, 1, false);
        put_item(snap, 34, char_id, struct_destroyed, InventoryFlag::Hangar, 10, false);
        put_item(snap, 34, char_id, struct_unanchored, InventoryFlag::Hangar, 10, false);
        uid
    });

    let mut sess = make_session(uid, char_id, corp_id, Some(st1.station_id), None);
    assert!(!sess.has_gm_role());

    let call_ls = |s: &mut SessionState, svc: &str, args: &[PyRep], kw: &HashMap<String, PyRep>| -> (Vec<String>, Vec<Vec<i64>>) {
        let rep = handle_clones_and_sheet(&ctx, s, svc, "ListStations", args, kw)
            .expect("handled")
            .expect("ListStations ok");
        let PyRep::Rowset { columns, rows } = rep else {
            panic!("expected Rowset, got {rep:?}");
        };
        let parsed = rows
            .into_iter()
            .map(|r| r.into_iter().map(|c| c.as_i64().expect("int cell")).collect())
            .collect();
        (columns, parsed)
    };

    let expected_cols = vec![
        "stationID".to_string(),
        "solarSystemID".to_string(),
        "itemCount".to_string(),
        "typeID".to_string(),
    ];

    // 1. Empty station/structure result set preserves the 4-column Rowset schema.
    for svc in ["charMgr", "charUnboundMgr"] {
        let (cols, rows) = call_ls(&mut sess, svc, &[], &empty_kw);
        assert_eq!(cols, expected_cols);
        assert!(rows.is_empty(), "non-station items must not appear: {rows:?}");
    }

    // Populate personal, foreign character, session corp, and foreign corp assets.
    ctx.db.with_mut_snapshot(|snap| {
        // char_id personal items
        put_item(snap, 34, char_id, st1.station_id, InventoryFlag::Hangar, 10, false);
        put_item(snap, bp_tid, char_id, st1.station_id, InventoryFlag::Hangar, 1, true);
        put_item(snap, 34, char_id, struct_a, InventoryFlag::Hangar, 20, false);
        put_item(snap, bp_tid, char_id, struct_b, InventoryFlag::Hangar, 1, false);

        // other_char_id items (must stay invisible to non-GM sess)
        put_item(snap, 34, other_char_id, st2.station_id, InventoryFlag::Hangar, 1, false);
        put_item(snap, bp_tid, other_char_id, struct_other, InventoryFlag::Hangar, 1, true);

        // corp_id items (office at st1 + direct SAG1 at st2 & struct_a + filtered markers/fittings/destroyed)
        let office_st1_id = put_item(snap, 27, corp_id, st1.station_id, InventoryFlag::Hangar, 1, true);
        put_item(snap, 34, corp_id, office_st1_id, InventoryFlag::CorpSAG2, 10, false);
        put_item(snap, 34, corp_id, st2.station_id, InventoryFlag::CorpSAG1, 5, false);
        put_item(snap, bp_tid, corp_id, struct_a, InventoryFlag::CorpSAG1, 1, false);
        put_item(snap, 27, corp_id, struct_b, InventoryFlag::Hangar, 1, true);
        put_item(snap, 35_894, corp_id, struct_b, InventoryFlag::ServiceSlot0, 1, true);
        put_item(snap, 4051, corp_id, struct_b, InventoryFlag::StructureFuel, 200, false);
        put_item(snap, 56_201, corp_id, struct_b, InventoryFlag::StructureDeedBay, 1, true);
        put_item(snap, 34, corp_id, struct_b, InventoryFlag::HiSlot0, 1, true);
        put_item(snap, 34, corp_id, struct_destroyed, InventoryFlag::CorpSAG1, 5, false);

        // other_corp_id item (must stay invisible even with is_corp=true)
        put_item(snap, 34, other_corp_id, struct_other, InventoryFlag::CorpSAG1, 5, false);
    });

    // 2. NPC station + Upwell structures include exact typeID, solarSystemID, and itemCount,
    //    and ignore extra caller-supplied IDs for non-GM sessions.
    let expected_personal = vec![
        vec![st1.station_id, i64::from(st1.solar_system_id), 2, i64::from(st1.station_type_id)],
        vec![struct_a, 30_000_142, 1, 35_832],
        vec![struct_b, 30_000_144, 1, 35_835],
    ];
    for svc in ["charMgr", "charUnboundMgr"] {
        let (cols, rows) = call_ls(&mut sess, svc, &[], &empty_kw);
        assert_eq!(cols, expected_cols);
        assert_eq!(rows, expected_personal);
    }
    let (_, spoofed_rows) = call_ls(
        &mut sess,
        "charMgr",
        &[PyRep::Bool(false), PyRep::Bool(false), PyRep::Integer(other_char_id)],
        &empty_kw,
    );
    assert_eq!(spoofed_rows, expected_personal);

    // 3. blueprint_only=true filters out non-blueprint stations/structures & decrements counts.
    let expected_bp_personal = vec![
        vec![st1.station_id, i64::from(st1.solar_system_id), 1, i64::from(st1.station_type_id)],
        vec![struct_b, 30_000_144, 1, 35_835],
    ];
    let (_, bp_bool_rows) = call_ls(&mut sess, "charMgr", &[PyRep::Bool(true), PyRep::Bool(false)], &empty_kw);
    assert_eq!(bp_bool_rows, expected_bp_personal);
    let (_, bp_int_rows) = call_ls(&mut sess, "charMgr", &[PyRep::Integer(1)], &empty_kw);
    assert_eq!(bp_int_rows, expected_bp_personal);
    let mut kw_bp = HashMap::new();
    kw_bp.insert("blueprintOnly".to_string(), PyRep::Bool(true));
    let (_, bp_kw_rows) = call_ls(&mut sess, "charMgr", &[], &kw_bp);
    assert_eq!(bp_kw_rows, expected_bp_personal);

    // 4. is_corp=true scopes strictly to authorized player corp, resolves office containers,
    //    excludes marker/fitted/fuel/core/destroyed items, and supports blueprint_only.
    let mut expected_corp = vec![
        vec![st1.station_id, i64::from(st1.solar_system_id), 1, i64::from(st1.station_type_id)],
        vec![st2.station_id, i64::from(st2.solar_system_id), 1, i64::from(st2.station_type_id)],
        vec![struct_a, 30_000_142, 1, 35_832],
    ];
    expected_corp.sort_by_key(|r| r[0]);
    let (corp_cols, corp_rows) = call_ls(&mut sess, "charMgr", &[PyRep::Bool(false), PyRep::Bool(true)], &empty_kw);
    assert_eq!(corp_cols, expected_cols);
    assert_eq!(corp_rows, expected_corp);

    let mut kw_corp_bp = HashMap::new();
    kw_corp_bp.insert("blueprintOnly".to_string(), PyRep::Integer(1));
    kw_corp_bp.insert("isCorporation".to_string(), PyRep::Bool(true));
    let (_, corp_bp_rows) = call_ls(&mut sess, "charUnboundMgr", &[], &kw_corp_bp);
    assert_eq!(corp_bp_rows, vec![vec![struct_a, 30_000_142, 1, 35_832]]);

    // 5. Corp RBAC: zero-role member is denied even with forged session.corp_role,
    //    hangar-query role sees only authorized division(s), and CEO sees all corp assets.
    let mut plain_sess = make_session(uid, plain_id, corp_id, Some(st1.station_id), None);
    plain_sess.corp_role = u64::MAX;
    let denied_err = handle_clones_and_sheet(
        &ctx,
        &mut plain_sess,
        "charMgr",
        "ListStations",
        &[PyRep::Bool(false), PyRep::Bool(true)],
        &empty_kw,
    )
    .expect("handled")
    .unwrap_err();
    assert_eq!(denied_err.code, "CorpRoleDenied");
    let (_, plain_personal) = call_ls(&mut plain_sess, "charMgr", &[], &empty_kw);
    assert!(plain_personal.is_empty());

    let mut sag1_sess = make_session(uid, sag1_id, corp_id, Some(st1.station_id), None);
    let mut expected_sag1 = vec![
        vec![st2.station_id, i64::from(st2.solar_system_id), 1, i64::from(st2.station_type_id)],
        vec![struct_a, 30_000_142, 1, 35_832],
    ];
    expected_sag1.sort_by_key(|r| r[0]);
    let (_, sag1_rows) = call_ls(&mut sag1_sess, "charMgr", &[PyRep::Bool(false), PyRep::Bool(true)], &empty_kw);
    assert_eq!(sag1_rows, expected_sag1);

    let mut ceo_sess = make_session(uid, ceo_id, corp_id, Some(st1.station_id), None);
    let (_, ceo_rows) = call_ls(&mut ceo_sess, "charUnboundMgr", &[PyRep::Bool(false), PyRep::Bool(true)], &empty_kw);
    assert_eq!(ceo_rows, expected_corp);

    // 6. Missing or NPC session corporation with is_corp=true fails closed (CorporationNotFound).
    sess.corporation_id = None;
    let err_missing = handle_clones_and_sheet(
        &ctx,
        &mut sess,
        "charMgr",
        "ListStations",
        &[PyRep::Bool(false), PyRep::Bool(true)],
        &empty_kw,
    )
    .expect("handled")
    .unwrap_err();
    assert_eq!(err_missing.code, "CorporationNotFound");

    sess.corporation_id = Some(1_000_009);
    let err_npc = handle_clones_and_sheet(
        &ctx,
        &mut sess,
        "charUnboundMgr",
        "ListStations",
        &[PyRep::Bool(false), PyRep::Bool(true)],
        &empty_kw,
    )
    .expect("handled")
    .unwrap_err();
    assert_eq!(err_npc.code, "CorporationNotFound");
}

#[test]
fn list_stations_corp_governance_office_and_role_matrix() {
    let ctx = test_ctx();
    let mut st_iter = ctx.sde.stations.values();
    let st_hq = st_iter.next().expect("first SDE station").clone();
    let st_other = st_iter
        .find(|s| s.station_id != st_hq.station_id)
        .expect("second SDE station")
        .clone();

    let corp_id = 98_000_299_i64;
    let ceo_id = 90_001_001_i64;
    let hq_sag1_id = 90_001_002_i64;
    let deliveries_id = 90_001_003_i64;
    let trader_id = 90_001_004_i64;
    let struct_citadel = 1_048_000_000_299_i64;
    let gov_office_station_id = 920_500_001_i64;
    let gov_office_struct_id = 920_500_002_i64;
    let empty_kw = HashMap::new();

    let uid = ctx.db.with_mut_snapshot(|snap| {
        let base_ch = snap.characters.values().next().cloned().expect("demo char");
        let uid = base_ch.user_id;
        for cid in [ceo_id, hq_sag1_id, deliveries_id, trader_id] {
            let mut ch = base_ch.clone();
            ch.character_id = cid;
            ch.corporation_id = corp_id;
            ch.station_id = None;
            snap.characters.insert(cid, ch);
        }

        let mut corp = CorporationRecord::new(corp_id, "GovOfficeCorp", "GOFC", ceo_id);
        corp.hq_station_id = Some(st_hq.station_id);
        corp.members = HashSet::from([ceo_id, hq_sag1_id, deliveries_id, trader_id]);
        corp.member_roles
            .insert(deliveries_id, corp_roles::ROLE_DELIVERIES_HANGAR_QUERY);
        corp.member_roles
            .insert(trader_id, corp_roles::ROLE_TRADER);
        snap.corporations.insert(corp_id, corp);

        snap.corp_role_assignments.insert(
            (corp_id, hq_sag1_id),
            eve_db::CorpMemberRoleAssignment {
                character_id: hq_sag1_id,
                roles_at_hq: corp_roles::ROLE_HANGAR_CAN_QUERY[0],
                ..Default::default()
            },
        );

        snap.structures.insert(
            struct_citadel,
            UpwellStructureRecord {
                structure_id: struct_citadel,
                type_id: 35_832,
                owner_corp_id: corp_id,
                solar_system_id: 30_000_142,
                state: "ShieldVulnerable".into(),
                name: "Governance Citadel".into(),
                ..Default::default()
            },
        );

        let gov = snap.corp_governance.entry(corp_id).or_default();
        gov.offices.insert(st_hq.station_id, gov_office_station_id);
        gov.offices.insert(gov_office_struct_id, struct_citadel);

        put_item(snap, 34, corp_id, gov_office_station_id, InventoryFlag::CorpSAG1, 10, false);
        put_item(snap, 34, corp_id, gov_office_struct_id, InventoryFlag::CorpSAG1, 5, false);
        put_item(snap, 34, corp_id, st_other.station_id, InventoryFlag::Deliveries, 3, false);
        put_item(snap, 34, corp_id, st_other.station_id, InventoryFlag::CorpSAG1, 2, false);
        uid
    });

    let call_corp = |cid: i64| -> Vec<Vec<i64>> {
        let mut s = make_session(uid, cid, corp_id, None, None);
        let rep = handle_clones_and_sheet(
            &ctx,
            &mut s,
            "charMgr",
            "ListStations",
            &[PyRep::Bool(false), PyRep::Bool(true)],
            &empty_kw,
        )
        .expect("handled")
        .expect("ListStations ok");
        let PyRep::Rowset { columns, rows } = rep else {
            panic!("expected Rowset, got {rep:?}");
        };
        assert_eq!(
            columns,
            vec!["stationID", "solarSystemID", "itemCount", "typeID"]
        );
        rows.into_iter()
            .map(|r| r.into_iter().map(|c| c.as_i64().expect("int")).collect())
            .collect()
    };

    // HQ-scoped ROLE_HANGAR_CAN_QUERY[0] sees only HQ CorpSAG1 resolved through gov.offices.
    assert_eq!(
        call_corp(hq_sag1_id),
        vec![vec![
            st_hq.station_id,
            i64::from(st_hq.solar_system_id),
            1,
            i64::from(st_hq.station_type_id),
        ]]
    );

    // Deliveries query role sees only st_other's Deliveries stack (count=1), not CorpSAG1.
    assert_eq!(
        call_corp(deliveries_id),
        vec![vec![
            st_other.station_id,
            i64::from(st_other.solar_system_id),
            1,
            i64::from(st_other.station_type_id),
        ]]
    );

    // Trader sees all 3 locations resolved across gov.offices (NPC station + structure) & Deliveries.
    let mut expected_all = vec![
        vec![st_hq.station_id, i64::from(st_hq.solar_system_id), 1, i64::from(st_hq.station_type_id)],
        vec![st_other.station_id, i64::from(st_other.solar_system_id), 2, i64::from(st_other.station_type_id)],
        vec![struct_citadel, 30_000_142, 1, 35_832],
    ];
    expected_all.sort_by_key(|r| r[0]);
    assert_eq!(call_corp(trader_id), expected_all);

    // Station Manager & Junior Accountant also have full corp asset station access.
    ctx.db.with_mut_snapshot(|snap| {
        let corp = snap.corporations.get_mut(&corp_id).unwrap();
        corp.member_roles
            .insert(deliveries_id, corp_roles::ROLE_STATION_MANAGER);
    });
    assert_eq!(call_corp(deliveries_id), expected_all);
}

#[test]
fn list_stations_blocks_docked_scoped_escalation_and_covers_titles_pending_deliveries_and_bays() {
    let ctx = test_ctx();
    let mut st_iter = ctx.sde.stations.values();
    let st_hq = st_iter.next().expect("HQ SDE station").clone();
    let st_base = st_iter
        .find(|s| s.station_id != st_hq.station_id)
        .expect("base SDE station")
        .clone();
    let st_other = ctx
        .sde
        .stations
        .values()
        .find(|s| s.station_id != st_hq.station_id && s.station_id != st_base.station_id)
        .expect("other SDE station")
        .clone();

    let corp_id = 98_000_399_i64;
    let ceo_id = 90_002_000_i64;
    let global_dir_id = 90_002_001_i64;
    let docked_hq_dir_id = 90_002_002_i64;
    let docked_base_trader_id = 90_002_003_i64;
    let title_member_id = 90_002_004_i64;
    let title_mask_only_id = 90_002_005_i64;
    let unmatured_title_only_id = 90_002_006_i64;
    let pend_scoped_id = 90_002_007_i64;
    let deliv_only_id = 90_002_008_i64;
    let personal_id = 90_002_009_i64;

    let struct_hq = 1_048_000_000_399_i64;
    let struct_other = 1_048_000_000_400_i64;
    let now_unix = ctx.now_unix();
    let empty_kw = HashMap::new();

    let uid = ctx.db.with_mut_snapshot(|snap| {
        let base_ch = snap.characters.values().next().cloned().expect("demo char");
        let uid = base_ch.user_id;
        for (cid, dock_st) in [
            (ceo_id, None),
            (global_dir_id, None),
            (docked_hq_dir_id, Some(st_hq.station_id)),
            (docked_base_trader_id, Some(st_base.station_id)),
            (title_member_id, Some(st_hq.station_id)),
            (title_mask_only_id, None),
            (unmatured_title_only_id, Some(st_hq.station_id)),
            (pend_scoped_id, Some(st_hq.station_id)),
            (deliv_only_id, None),
            (personal_id, None),
        ] {
            let mut ch = base_ch.clone();
            ch.character_id = cid;
            ch.corporation_id = corp_id;
            ch.station_id = dock_st;
            snap.characters.insert(cid, ch);
        }

        let mut corp = CorporationRecord::new(corp_id, "ScopedListCorp", "SLC", ceo_id);
        corp.hq_station_id = Some(st_hq.station_id);
        corp.members = HashSet::from([
            ceo_id,
            global_dir_id,
            docked_hq_dir_id,
            docked_base_trader_id,
            title_member_id,
            title_mask_only_id,
            unmatured_title_only_id,
            pend_scoped_id,
            deliv_only_id,
        ]);
        corp.member_roles
            .insert(global_dir_id, corp_roles::ROLE_DIRECTOR);
        corp.member_roles
            .insert(deliv_only_id, corp_roles::ROLE_DELIVERIES_HANGAR_QUERY);
        snap.corporations.insert(corp_id, corp);

        snap.corp_role_assignments.insert(
            (corp_id, docked_hq_dir_id),
            eve_db::CorpMemberRoleAssignment {
                character_id: docked_hq_dir_id,
                roles_at_hq: corp_roles::ROLE_DIRECTOR,
                ..Default::default()
            },
        );
        snap.corp_role_assignments.insert(
            (corp_id, docked_base_trader_id),
            eve_db::CorpMemberRoleAssignment {
                character_id: docked_base_trader_id,
                roles_at_base: corp_roles::ROLE_TRADER,
                ..Default::default()
            },
        );

        let gov = snap.corp_governance.entry(corp_id).or_default();
        gov.member_bases
            .insert(docked_base_trader_id, st_base.station_id);
        gov.member_bases.insert(title_member_id, st_base.station_id);
        gov.pending_title_role_grants.insert(
            title_member_id,
            (corp_roles::ROLE_HANGAR_CAN_QUERY[1], now_unix + 3_600),
        );
        gov.pending_title_role_grants.insert(
            unmatured_title_only_id,
            (corp_roles::ROLE_HANGAR_CAN_QUERY[1], now_unix + 3_600),
        );
        gov.pending_scoped_role_grants.insert(
            pend_scoped_id,
            (corp_roles::ROLE_HANGAR_CAN_QUERY[0], 0, 0, now_unix + 86_400),
        );
        gov.member_titles.insert(title_mask_only_id, 1u64 << 1);

        snap.corp_member_titles
            .insert((corp_id, title_member_id), vec![1]);
        snap.corp_member_titles
            .insert((corp_id, unmatured_title_only_id), vec![4]);
        snap.corp_titles.insert(
            corp_id,
            vec![
                eve_db::snapshot_ext::CorpTitleEntry {
                    title_id: 1,
                    name: "ScopedMulti".into(),
                    roles_at_hq: vec!["Hangar Can Query 2".into()],
                    roles_at_base: vec!["Deliveries Hangar Query".into()],
                    roles_at_other: vec!["Hangar Can Query 3".into()],
                    ..Default::default()
                },
                eve_db::snapshot_ext::CorpTitleEntry {
                    title_id: 2,
                    name: "OtherSag3".into(),
                    roles_at_other: vec!["Hangar Can Query 3".into()],
                    ..Default::default()
                },
                eve_db::snapshot_ext::CorpTitleEntry {
                    title_id: 4,
                    name: "HqSag2Only".into(),
                    roles_at_hq: vec!["Hangar Can Query 2".into()],
                    ..Default::default()
                },
            ],
        );

        snap.structures.insert(
            struct_hq,
            UpwellStructureRecord {
                structure_id: struct_hq,
                type_id: 35_832,
                owner_corp_id: corp_id,
                solar_system_id: 30_000_142,
                state: "ShieldVulnerable".into(),
                name: "BayOnly Astrahus".into(),
                ..Default::default()
            },
        );
        snap.structures.insert(
            struct_other,
            UpwellStructureRecord {
                structure_id: struct_other,
                type_id: 35_835,
                owner_corp_id: corp_id,
                solar_system_id: 30_000_144,
                state: "ShieldVulnerable".into(),
                name: "Mixed Raitaru".into(),
                ..Default::default()
            },
        );

        // HQ: SAG1 + SAG2
        put_item(snap, 34, corp_id, st_hq.station_id, InventoryFlag::CorpSAG1, 10, false);
        put_item(snap, 34, corp_id, st_hq.station_id, InventoryFlag::CorpSAG2, 10, false);
        // Base: CorpDeliveries (62) + CapsuleerDeliveries (187)
        put_item(snap, 34, corp_id, st_base.station_id, InventoryFlag::CorpDeliveries, 4, false);
        put_item(snap, 34, corp_id, st_base.station_id, InventoryFlag::CapsuleerDeliveries, 6, false);
        // Other station: SAG3 + SAG1 + Deliveries (173)
        put_item(snap, 34, corp_id, st_other.station_id, InventoryFlag::CorpSAG3, 3, false);
        put_item(snap, 34, corp_id, st_other.station_id, InventoryFlag::CorpSAG1, 3, false);
        put_item(snap, 34, corp_id, st_other.station_id, InventoryFlag::Deliveries, 3, false);

        // struct_hq: ONLY structure fuel/fighter bays (must NEVER appear in corp ListStations)
        put_item(snap, 4051, corp_id, struct_hq, InventoryFlag::SpecializedFuelBay, 50, false);
        put_item(snap, 4051, corp_id, struct_hq, InventoryFlag::StructureFuel, 50, false);
        put_item(snap, 23_057, corp_id, struct_hq, InventoryFlag::FighterBay, 5, false);

        // struct_other: 1 CorpSAG1 item + fuel/fighter/ammo/core/service/hangar items (count must be 1)
        put_item(snap, 34, corp_id, struct_other, InventoryFlag::CorpSAG1, 7, false);
        put_item(snap, 4051, corp_id, struct_other, InventoryFlag::SpecializedFuelBay, 50, false);
        put_item(snap, 4051, corp_id, struct_other, InventoryFlag::StructureFuel, 50, false);
        put_item(snap, 23_057, corp_id, struct_other, InventoryFlag::FighterBay, 5, false);
        put_item(snap, 37_847, corp_id, struct_other, InventoryFlag::SpecializedAmmoHold, 10, false);
        put_item(snap, 56_201, corp_id, struct_other, InventoryFlag::StructureDeedBay, 1, true);
        put_item(snap, 35_894, corp_id, struct_other, InventoryFlag::ServiceSlot0, 1, true);
        put_item(snap, 34, corp_id, struct_other, InventoryFlag::Hangar, 9, false);

        // Personal assets at struct_hq: docked ship (Hangar) with nested fighters (FighterBay)
        // plus direct structure bay items that must be excluded.
        let ship_id = put_item(snap, 587, personal_id, struct_hq, InventoryFlag::Hangar, 1, true);
        put_item(snap, 23_057, personal_id, ship_id, InventoryFlag::FighterBay, 5, false);
        put_item(snap, 4051, personal_id, struct_hq, InventoryFlag::SpecializedFuelBay, 100, false);
        put_item(snap, 4051, personal_id, struct_hq, InventoryFlag::StructureFuel, 100, false);
        put_item(snap, 23_057, personal_id, struct_hq, InventoryFlag::FighterBay, 5, false);
        uid
    });

    let call_ls = |cid: i64, dock_st: Option<i64>, is_corp: bool| -> Result<Vec<Vec<i64>>, BridgeRpcError> {
        let mut s = make_session(uid, cid, corp_id, dock_st, None);
        let rep = handle_clones_and_sheet(
            &ctx,
            &mut s,
            "charMgr",
            "ListStations",
            &[PyRep::Bool(false), PyRep::Bool(is_corp)],
            &empty_kw,
        )
        .expect("handled")?;
        let PyRep::Rowset { columns, rows } = rep else {
            panic!("expected Rowset, got {rep:?}");
        };
        assert_eq!(
            columns,
            vec!["stationID", "solarSystemID", "itemCount", "typeID"]
        );
        Ok(rows
            .into_iter()
            .map(|r| r.into_iter().map(|c| c.as_i64().expect("int")).collect())
            .collect())
    };
    let row_for = |st: &eve_sde::StationInfo, count: i64| -> Vec<i64> {
        vec![
            st.station_id,
            i64::from(st.solar_system_id),
            count,
            i64::from(st.station_type_id),
        ]
    };

    // 1. Docked HQ Director and Base Trader must NOT escalate globally via ch.station_id.
    assert_eq!(
        call_ls(docked_hq_dir_id, Some(st_hq.station_id), true).unwrap(),
        vec![row_for(&st_hq, 2)]
    );
    assert_eq!(
        call_ls(docked_base_trader_id, Some(st_base.station_id), true).unwrap(),
        vec![row_for(&st_base, 2)]
    );

    // 2. Deliveries role sees CorpDeliveries (62), CapsuleerDeliveries (187), and Deliveries (173).
    let mut expected_deliv = vec![row_for(&st_base, 2), row_for(&st_other, 1)];
    expected_deliv.sort_by_key(|r| r[0]);
    assert_eq!(call_ls(deliv_only_id, None, true).unwrap(), expected_deliv);

    // 3. Title-granted scoped roles: unmatured HQ title role suppressed while matured base/other work;
    //    unmatured-only title member fails closed with CorpRoleDenied until maturation.
    let mut expected_title_pre = vec![row_for(&st_base, 2), row_for(&st_other, 1)];
    expected_title_pre.sort_by_key(|r| r[0]);
    assert_eq!(
        call_ls(title_member_id, Some(st_hq.station_id), true).unwrap(),
        expected_title_pre
    );
    assert_eq!(
        call_ls(title_mask_only_id, None, true).unwrap(),
        vec![row_for(&st_other, 1)]
    );
    assert_eq!(
        call_ls(unmatured_title_only_id, Some(st_hq.station_id), true)
            .unwrap_err()
            .code,
        "CorpRoleDenied"
    );
    assert_eq!(
        call_ls(pend_scoped_id, Some(st_hq.station_id), true)
            .unwrap_err()
            .code,
        "CorpRoleDenied"
    );

    // Advance 3600s: pending_title_role_grants matures; pending_scoped_role_grants still unmatured.
    ctx.advance_secs(3_600);
    let mut expected_title_post = vec![
        row_for(&st_hq, 1),
        row_for(&st_base, 2),
        row_for(&st_other, 1),
    ];
    expected_title_post.sort_by_key(|r| r[0]);
    assert_eq!(
        call_ls(title_member_id, Some(st_hq.station_id), true).unwrap(),
        expected_title_post
    );
    assert_eq!(
        call_ls(unmatured_title_only_id, Some(st_hq.station_id), true).unwrap(),
        vec![row_for(&st_hq, 1)]
    );
    assert_eq!(
        call_ls(pend_scoped_id, Some(st_hq.station_id), true)
            .unwrap_err()
            .code,
        "CorpRoleDenied"
    );

    // Advance remaining 82800s (total 86400s): matured pending_scoped_role_grants unlocks HQ SAG1.
    ctx.advance_secs(82_800);
    assert_eq!(
        call_ls(pend_scoped_id, Some(st_hq.station_id), true).unwrap(),
        vec![row_for(&st_hq, 1)]
    );

    // 4. CEO and global Director only see asset-visible corp flags (fuel/fighter/slot/hangar excluded).
    let mut expected_full_corp = vec![
        row_for(&st_hq, 2),
        row_for(&st_base, 2),
        row_for(&st_other, 3),
        vec![struct_other, 30_000_144, 1, 35_835],
    ];
    expected_full_corp.sort_by_key(|r| r[0]);
    assert_eq!(call_ls(ceo_id, None, true).unwrap(), expected_full_corp);
    assert_eq!(
        call_ls(global_dir_id, None, true).unwrap(),
        expected_full_corp
    );

    // 5. Personal ListStations excludes structure SpecializedFuelBay/StructureFuel/FighterBay
    //    while still counting the docked ship carrying fighters in its own FighterBay.
    assert_eq!(
        call_ls(personal_id, None, false).unwrap(),
        vec![vec![struct_hq, 30_000_142, 1, 35_832]]
    );
}

#[cfg(test)]
#[path = "character_clones_implants_extra_unit_tests.rs"]
mod character_clones_implants_extra_unit_tests;
