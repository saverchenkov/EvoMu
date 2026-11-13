//! Additional edge-case and branch-coverage unit tests for `character_clones_implants.rs`.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::account_services::account_services_tests::PyRepTestExt;
use crate::services::character_skills::character_clones_implants::handle_clones_and_sheet;
use eve_db::types::{TrainedSkillRecord, UpwellStructureRecord};
use eve_db::{
    CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents,
    KillRightRecord, NeuralAttributes,
};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::{InvType, SdeRegistry};
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn test_ctx() -> ServerContext {
    ServerContext::with_sde_and_repository(
        shared_sde(),
        Arc::new(eve_db::WorldRepository::with_demo_seed()),
    )
    .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
}

#[test]
fn get_character_info_validation_alpha_cap_standings_kill_rights_and_neural_remap() {
    let mut ctx = test_ctx();
    let kw = HashMap::new();

    // Ensure non-apparel type & apparel type exist
    {
        let sde = Arc::make_mut(&mut ctx.sde);
        let seed = sde.types.values().next().cloned().unwrap();
        sde.types.insert(
            77_701,
            InvType {
                type_id: 77_701,
                group_id: 745,
                category_id: 20, // implant
                name: "TestImplant".into(),
                ..seed
            },
        );
    }

    let (user_id, char_id, other_char_id) = ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.values().next().unwrap().clone();
        let cid = ch.character_id;
        let uid = ch.user_id;
        let ch_mut = snap.characters.get_mut(&cid).unwrap();
        ch_mut.clone_grade = 0; // Alpha -> caps skill level at 4 in GetCharacterInfo
        ch_mut.active_ship_id = 999_888_777; // Missing ship item -> fallback (0, "")
        ch_mut.standings.insert(1_000_002, 3.5);
        ch_mut.neural = NeuralAttributes {
            charisma: 20,
            intelligence: 21,
            memory: 21,
            perception: 20,
            willpower: 17,
            bonus_remaps: 2,
            last_remap_unix: None,
            remap_cooldown_until_unix: None,
        };

        // Add skill at level 5 (should clamp to 4), active implant, standing, kill right, neural remap
        snap.skills.entry(cid).or_default().insert(
            3402,
            TrainedSkillRecord {
                type_id: 3402,
                level: 5,
                skill_points: 256_000.0,
            },
        );
        snap.active_implants.insert(cid, vec![77_701]);

        let mut other = ch.clone();
        let ocid = 90_777_001;
        other.character_id = ocid;
        other.user_id = uid + 50;
        snap.characters.insert(ocid, other);
        snap.kill_rights.insert(
            42,
            KillRightRecord {
                kill_right_id: 42,
                owner_id: cid,
                target_id: ocid,
                activation_fee: IskCents::from_isk(1_000_000),
                is_active_suspect: false,
            },
        );
        (uid, cid, ocid)
    });

    // CharacterNotSelected when user_id set but character_id None and no arg
    let mut no_char_sess = SessionState::new(10);
    no_char_sess.user_id = Some(user_id);
    assert_eq!(
        handle_clones_and_sheet(
            &ctx,
            &mut no_char_sess,
            "charMgr",
            "GetCharacterInfo",
            &[],
            &kw
        )
        .unwrap()
        .unwrap_err()
        .code,
        "CharacterNotSelected"
    );

    // CharacterOwnershipMismatch when querying another user's character
    let mut sess = SessionState::new(11);
    sess.user_id = Some(user_id);
    sess.character_id = Some(char_id);
    assert_eq!(
        handle_clones_and_sheet(
            &ctx,
            &mut sess,
            "charMgr",
            "GetCharacterInfo",
            &[PyRep::Integer(other_char_id)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "CharacterOwnershipMismatch"
    );

    // CharacterOwnershipMismatch for unknown character ID (user ownership checked before DB lookup)
    assert_eq!(
        handle_clones_and_sheet(
            &ctx,
            &mut sess,
            "charMgr",
            "GetCharacterInfo",
            &[PyRep::Integer(88_888_888)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "CharacterOwnershipMismatch"
    );

    // Success path
    let info = handle_clones_and_sheet(
        &ctx,
        &mut sess,
        "charUnboundMgr",
        "GetCharacterInfo",
        &[],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(info.dict_get("shipID"), Some(&PyRep::Integer(999_888_777)));
    assert_eq!(
        info.dict_get("shipName"),
        Some(&PyRep::String(String::new()))
    );
    let attrs = info.dict_get("attributes").unwrap();
    assert_eq!(attrs.dict_get("lastRemapDate"), Some(&PyRep::None));
    assert_eq!(
        attrs.dict_get("accruedRemapCooldownDate"),
        Some(&PyRep::None)
    );
    assert_eq!(
        info.dict_get("implants")
            .and_then(|v| v.as_list())
            .map(|l| l.len()),
        Some(1)
    );
    assert_eq!(
        info.dict_get("killRights")
            .and_then(|v| v.as_list())
            .map(|l| l.len()),
        Some(1)
    );
    let skills = info.dict_get("skills").and_then(|v| v.as_list()).unwrap();
    let s3402 = skills
        .iter()
        .find(|s| s.dict_get("typeID") == Some(&PyRep::Integer(3402)))
        .unwrap();
    assert_eq!(s3402.dict_get("skillLevel"), Some(&PyRep::Integer(5)));
    assert_eq!(s3402.dict_get("activeSkillLevel"), Some(&PyRep::Integer(4)));
}

#[test]
fn jump_clone_station_structure_clone_state_price_and_standings_edges() {
    let mut ctx = test_ctx();
    let kw = HashMap::new();

    let (no_clone_station, clone_station) = {
        let sde = Arc::make_mut(&mut ctx.sde);
        let template = sde.stations.values().next().cloned().unwrap();
        sde.station_operations.insert(
            99_001,
            eve_sde::StationOperationInfo {
                operation_id: 99_001,
                name: "NoCloneOp".into(),
                activity_id: None,
                services: Vec::new(),
            },
        );
        sde.station_operations.insert(
            99_002,
            eve_sde::StationOperationInfo {
                operation_id: 99_002,
                name: "CloneBayOp".into(),
                activity_id: None,
                services: vec![10, 24],
            },
        );
        let no_cb_id = 60_990_001_i64;
        let mut no_cb = template.clone();
        no_cb.station_id = no_cb_id;
        no_cb.operation_id = 99_001;
        no_cb.has_clone_bay = false;
        sde.stations.insert(no_cb_id, no_cb);

        let cb_id = 60_990_002_i64;
        let mut cb = template;
        cb.station_id = cb_id;
        cb.operation_id = 99_002;
        cb.has_clone_bay = true;
        cb.owner_corporation_id = 98_555_001;
        cb.owner_faction_id = None;
        sde.stations.insert(cb_id, cb);
        (no_cb_id, cb_id)
    };

    let owner_corp_id = 98_555_001_i64;
    let struct_no_cb = 1_049_000_001_i64;
    let struct_with_cb = 1_049_000_002_i64;

    let (user_id, char_id) = ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.values_mut().next().unwrap();
        ch.clone_grade = 2;
        ch.station_id = Some(no_clone_station);
        ch.structure_id = None;
        ch.corporation_id = owner_corp_id;
        ch.balance = IskCents::from_isk(10_000_000);
        let uid = ch.user_id;
        let cid = ch.character_id;
        snap.jump_clones.remove(&cid);
        snap.skills.remove(&cid);

        let mut corp = { let mut __corp = CorporationRecord::default(); __corp.corporation_id = owner_corp_id; __corp.name = "Clone Owner Corp".into(); __corp.ticker = "CLON".into(); __corp.ceo_id = cid; __corp.ensure_wallet_divisions(); __corp };
        corp.members.insert(cid);
        corp.ensure_wallet_divisions();
        snap.corporations.insert(owner_corp_id, corp);

        snap.structures.insert(
            struct_no_cb,
            UpwellStructureRecord {
                structure_id: struct_no_cb,
                type_id: 35_832,
                owner_corp_id,
                solar_system_id: 30_000_142,
                name: "NoCloneCitadel".into(),
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                ..Default::default()
            },
        );
        snap.structures.insert(
            struct_with_cb,
            UpwellStructureRecord {
                structure_id: struct_with_cb,
                type_id: 35_832,
                owner_corp_id,
                solar_system_id: 30_000_142,
                name: "CloneCitadel".into(),
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                ..Default::default()
            },
        );
        snap.next_entity_id += 1;
        let bay_id = snap.next_entity_id;
        snap.items.insert(
            bay_id,
            InventoryItemRecord {
                item_id: bay_id,
                type_id: 35_894,
                owner_id: owner_corp_id,
                location_id: struct_with_cb,
                flag: InventoryFlag::ServiceSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(bay_id, true);
        (uid, cid)
    });

    let mut sess = SessionState::new(50);
    sess.user_id = Some(user_id);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(owner_corp_id);
    sess.station_id = Some(no_clone_station);

    // GetPriceForClone at station without clone bay -> StationHasNoCloneBay
    assert_eq!(
        handle_clones_and_sheet(
            &ctx,
            &mut sess,
            "jumpCloneSvc",
            "GetPriceForClone",
            &[],
            &kw
        )
        .unwrap()
        .unwrap_err()
        .code,
        "StationHasNoCloneBay"
    );
    // InstallCloneInStation at station without clone bay -> StationHasNoCloneBay
    assert_eq!(
        handle_clones_and_sheet(
            &ctx,
            &mut sess,
            "jumpCloneSvc",
            "InstallCloneInStation",
            &[],
            &kw
        )
        .unwrap()
        .unwrap_err()
        .code,
        "StationHasNoCloneBay"
    );

    // Infomorph Psychology (24242 lvl 1) + Adv (33407 lvl 1) + Elite (73910 lvl 1) = 3 max clones
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().station_id = Some(clone_station);
        for sk in [24242_u32, 33407, 73910] {
            snap.skills.entry(char_id).or_default().insert(
                sk,
                TrainedSkillRecord {
                    type_id: sk,
                    level: 1,
                    skill_points: 1000.0,
                },
            );
        }
    });
    sess.station_id = Some(clone_station);

    // Install clone #1 in station -> succeeds, second in same station -> JumpCloneAlreadyAtStation
    assert!(handle_clones_and_sheet(
        &ctx,
        &mut sess,
        "jumpCloneSvc",
        "InstallCloneInStation",
        &[],
        &kw
    )
    .unwrap()
    .is_ok());
    assert_eq!(
        handle_clones_and_sheet(
            &ctx,
            &mut sess,
            "jumpCloneSvc",
            "InstallCloneInStation",
            &[],
            &kw
        )
        .unwrap()
        .unwrap_err()
        .code,
        "JumpCloneAlreadyAtStation"
    );

    // Dock at structure without clone_bay -> NoCloneBayService on InstallCloneInStructure & StationHasNoCloneBay on GetPriceForClone
    sess.station_id = None;
    sess.structure_id = Some(struct_no_cb);
    ctx.db.with_mut_snapshot(|snap| {
        let c = snap.characters.get_mut(&char_id).unwrap();
        c.station_id = None;
        c.structure_id = Some(struct_no_cb);
    });
    assert_eq!(
        handle_clones_and_sheet(
            &ctx,
            &mut sess,
            "jumpCloneSvc",
            "GetPriceForClone",
            &[],
            &kw
        )
        .unwrap()
        .unwrap_err()
        .code,
        "StationHasNoCloneBay"
    );
    assert_eq!(
        handle_clones_and_sheet(
            &ctx,
            &mut sess,
            "jumpCloneSvc",
            "InstallCloneInStructure",
            &[],
            &kw
        )
        .unwrap()
        .unwrap_err()
        .code,
        "NoCloneBayService"
    );

    // Switch to structure WITH clone_bay -> GetPriceForClone returns 900_000.0, InstallCloneInStructure credits corp wallet
    sess.structure_id = Some(struct_with_cb);
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().structure_id = Some(struct_with_cb);
    });
    assert_eq!(
        handle_clones_and_sheet(
            &ctx,
            &mut sess,
            "jumpCloneSvc",
            "GetPriceForClone",
            &[],
            &kw
        )
        .unwrap()
        .unwrap(),
        PyRep::Real(900_000.0)
    );
    assert!(handle_clones_and_sheet(
        &ctx,
        &mut sess,
        "jumpCloneSvc",
        "InstallCloneInStructure",
        &[],
        &kw
    )
    .unwrap()
    .is_ok());
    assert_eq!(
        ctx.db
            .with_snapshot(|s| s.corporations[&owner_corp_id].master_wallet_balance()),
        IskCents::from_isk(900_000)
    );
    assert_eq!(
        handle_clones_and_sheet(
            &ctx,
            &mut sess,
            "jumpCloneSvc",
            "InstallCloneInStructure",
            &[],
            &kw
        )
        .unwrap()
        .unwrap_err()
        .code,
        "JumpCloneAlreadyAtStation"
    );

    // GetCloneState with multiple clones returns KeyVal with timeLastJump = None (last_clone_jump_unix == 0)
    let state = handle_clones_and_sheet(&ctx, &mut sess, "jumpCloneSvc", "GetCloneState", &[], &kw)
        .unwrap()
        .unwrap();
    assert_eq!(state.dict_get("timeLastJump"), Some(&PyRep::None));
    assert!(matches!(
        state.dict_get("clones"),
        Some(PyRep::Rowset { rows, .. }) if rows.len() == 2
    ));

    // GetStationCloneState filters strictly by docked location (struct_with_cb -> 1)
    let st_state = handle_clones_and_sheet(
        &ctx,
        &mut sess,
        "jumpCloneSvc",
        "GetStationCloneState",
        &[],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert!(matches!(st_state, PyRep::Rowset { rows, .. } if rows.len() == 1));

    // DestroyInstalledClone: CloneNotFound for unknown ID, success for existing clone
    assert_eq!(
        handle_clones_and_sheet(
            &ctx,
            &mut sess,
            "jumpCloneSvc",
            "DestroyInstalledClone",
            &[PyRep::Integer(999_999)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "CloneNotFound"
    );
    let clone_id_1 = ctx
        .db
        .with_snapshot(|s| s.jump_clones[&char_id][0].jump_clone_id);
    assert_eq!(
        handle_clones_and_sheet(
            &ctx,
            &mut sess,
            "jumpCloneSvc",
            "DestroyInstalledClone",
            &[PyRep::Integer(clone_id_1)],
            &kw,
        )
        .unwrap()
        .unwrap(),
        PyRep::Bool(true)
    );

    // standingMgr.GetCharStandings & GetStandingTransactions (with & without from_filter)
    let ch_standings =
        handle_clones_and_sheet(&ctx, &mut sess, "standingMgr", "GetCharStandings", &[], &kw)
            .unwrap()
            .unwrap();
    assert!(matches!(ch_standings, PyRep::Rowset { .. }));
    let txs_all = handle_clones_and_sheet(
        &ctx,
        &mut sess,
        "standingMgr",
        "GetStandingTransactions",
        &[],
        &kw,
    )
    .unwrap()
    .unwrap();
    let txs_filtered = handle_clones_and_sheet(
        &ctx,
        &mut sess,
        "standingMgr",
        "GetStandingTransactions",
        &[PyRep::Integer(1_000_002)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert!(matches!(txs_all, PyRep::Rowset { .. }));
    assert!(matches!(txs_filtered, PyRep::Rowset { .. }));
}

#[test]
fn paper_doll_batch_portrait_apparel_split_and_list_stations() {
    let mut ctx = test_ctx();
    let kw = HashMap::new();

    // Inject two distinct apparel groups (3001, 3002) under category 30
    {
        let sde = Arc::make_mut(&mut ctx.sde);
        let seed = sde.types.values().next().cloned().unwrap();
        for (tid, gid) in [(78_001_u32, 1083_u32), (78_002_u32, 1084_u32)] {
            sde.types.insert(
                tid,
                InvType {
                    type_id: tid,
                    group_id: gid,
                    category_id: 30,
                    name: format!("Apparel_{tid}"),
                    ..seed.clone()
                },
            );
        }
    }

    let station_id = 60_003_760_i64;
    let (user_id, char_id, other_id, stack_apparel_id, single_apparel_id, foreign_apparel_id) =
        ctx.db.with_mut_snapshot(|snap| {
            let mut base = snap.characters.values().next().unwrap().clone();
            let cid = base.character_id;
            let uid = base.user_id;
            base.gender = 0; // Female
            base.station_id = None; // undocked initially
            base.structure_id = None;
            snap.characters.insert(cid, base.clone());

            let oid = 90_888_002_i64;
            let mut other = base;
            other.character_id = oid;
            other.user_id = uid + 99;
            snap.characters.insert(oid, other);

            snap.next_entity_id += 1;
            let s_id = snap.next_entity_id;
            snap.items.insert(
                s_id,
                InventoryItemRecord {
                    item_id: s_id,
                    type_id: 78_001,
                    owner_id: cid,
                    location_id: station_id,
                    flag: InventoryFlag::Hangar,
                    quantity: 3, // stack -> will split 1 singleton into Wardrobe
                    singleton: false,
                    ..Default::default()
                },
            );
            snap.next_entity_id += 1;
            let one_id = snap.next_entity_id;
            snap.items.insert(
                one_id,
                InventoryItemRecord {
                    item_id: one_id,
                    type_id: 78_002,
                    owner_id: cid,
                    location_id: station_id,
                    flag: InventoryFlag::Hangar,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
            snap.next_entity_id += 1;
            let for_id = snap.next_entity_id;
            snap.items.insert(
                for_id,
                InventoryItemRecord {
                    item_id: for_id,
                    type_id: 78_002,
                    owner_id: oid,
                    location_id: station_id,
                    flag: InventoryFlag::Hangar,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
            (uid, cid, oid, s_id, one_id, for_id)
        });

    let mut sess = SessionState::new(60);
    sess.user_id = Some(user_id);
    sess.character_id = Some(char_id);

    // GetPaperDollData & GetPaperDollPortraitDataFor errors
    sess.character_id = None;
    assert_eq!(
        handle_clones_and_sheet(
            &ctx,
            &mut sess,
            "paperDollServer",
            "GetPaperDollData",
            &[],
            &kw
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidArgument"
    );
    assert_eq!(
        handle_clones_and_sheet(
            &ctx,
            &mut sess,
            "paperDollServer",
            "GetPaperDollData",
            &[PyRep::Integer(77_777_777)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "CharacterNotFound"
    );
    assert_eq!(
        handle_clones_and_sheet(
            &ctx,
            &mut sess,
            "paperDollServer",
            "GetPaperDollPortraitDataFor",
            &[],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidArgument"
    );
    sess.character_id = Some(char_id);

    // Female GetPaperDollData (KeyVal with modifiers/colors/sculpts/appearance for V24 StartDeathScene) + Batch skipping unknown ID
    let pd = handle_clones_and_sheet(
        &ctx,
        &mut sess,
        "paperDollServer",
        "GetPaperDollData",
        &[PyRep::Integer(char_id)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(pd.dict_get("genderLabel").and_then(PyRep::as_str), Some("female"));
    assert_eq!(pd.dict_get("modifiers"), Some(&PyRep::List(Vec::new())));
    assert_eq!(pd.dict_get("colors"), Some(&PyRep::List(Vec::new())));
    let batch = handle_clones_and_sheet(
        &ctx,
        &mut sess,
        "paperDollServer",
        "GetPaperDollDataBatch",
        &[PyRep::List(vec![
            PyRep::Integer(char_id),
            PyRep::Integer(99_999_111),
        ])],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(batch.as_dict().map(|d| d.len()), Some(1));

    // UpdateExistingCharacterLimited errors: ownership, missing arg, NotDocked when apparel passed undocked
    assert_eq!(
        handle_clones_and_sheet(
            &ctx,
            &mut sess,
            "paperDollServer",
            "UpdateExistingCharacterLimited",
            &[PyRep::Integer(other_id), PyRep::Dict(vec![])],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "CharacterOwnershipMismatch"
    );
    assert_eq!(
        handle_clones_and_sheet(
            &ctx,
            &mut sess,
            "paperDollServer",
            "UpdateExistingCharacterLimited",
            &[PyRep::Integer(char_id)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidDollInfo"
    );
    let doll_dict = PyRep::Dict(vec![(
        PyRep::String("sculpts".into()),
        PyRep::List(vec![PyRep::Real(0.2)]),
    )]);
    let with_apparel = |ids: Vec<i64>| {
        let PyRep::Dict(mut pairs) = doll_dict.clone() else {
            unreachable!()
        };
        pairs.push((
            PyRep::String("apparel".into()),
            PyRep::List(ids.into_iter().map(PyRep::Integer).collect()),
        ));
        PyRep::Dict(pairs)
    };
    assert_eq!(
        handle_clones_and_sheet(
            &ctx,
            &mut sess,
            "paperDollServer",
            "UpdateExistingCharacterLimited",
            &[
                PyRep::Integer(char_id),
                with_apparel(vec![single_apparel_id]),
                PyRep::None,
            ],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "NotDocked"
    );

    // Dock at station, verify duplicate & foreign apparel rejection, then valid stack+single equip & portrait preservation
    sess.station_id = Some(station_id);
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().station_id = Some(station_id);
    });
    assert_eq!(
        handle_clones_and_sheet(
            &ctx,
            &mut sess,
            "paperDollServer",
            "UpdateExistingCharacterLimited",
            &[
                PyRep::Integer(char_id),
                with_apparel(vec![single_apparel_id, single_apparel_id]),
                PyRep::None,
            ],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidApparelItem"
    );
    assert_eq!(
        handle_clones_and_sheet(
            &ctx,
            &mut sess,
            "paperDollServer",
            "UpdateExistingCharacterLimited",
            &[
                PyRep::Integer(char_id),
                with_apparel(vec![foreign_apparel_id]),
                PyRep::None,
            ],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidApparelItem"
    );

    // Equip both stacked (3 -> split to 2 + new singleton in Wardrobe) and single apparel
    assert!(handle_clones_and_sheet(
        &ctx,
        &mut sess,
        "paperDollServer",
        "UpdateExistingCharacterLimited",
        &[
            PyRep::Integer(char_id),
            with_apparel(vec![stack_apparel_id, single_apparel_id]),
            PyRep::None,
        ],
        &kw,
    )
    .unwrap()
    .is_ok());

    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&stack_apparel_id].quantity, 2);
        assert_eq!(snap.items[&single_apparel_id].flag, InventoryFlag::Wardrobe);
    });

    // GetMyPaperDollData & ListStations
    let mine = handle_clones_and_sheet(
        &ctx,
        &mut sess,
        "paperDollServer",
        "GetMyPaperDollData",
        &[],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(mine.dict_get("characterID"), Some(&PyRep::Integer(char_id)));
    let st_list = handle_clones_and_sheet(&ctx, &mut sess, "charMgr", "ListStations", &[], &kw)
        .unwrap()
        .unwrap();
    assert!(matches!(st_list, PyRep::Rowset { rows, .. } if !rows.is_empty()));
}
