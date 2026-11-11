//! Extra unit and mutation-killing tests for `services::character_skills::character_clones_implants`.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::character_skills::character_clones_implants::{
    handle_clones_and_sheet, jump_clone_install_fee_cents,
};
use eve_db::types::{TrainedSkillRecord, UpwellStructureRecord};
use eve_db::{
    InventoryFlag, InventoryItemRecord, IskCents, JumpCloneRecord, SkillQueueEntry,
    WorldStateSnapshot,
};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::{InvType, SdeRegistry, StationInfo};
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn test_ctx() -> ServerContext {
    let sde = SHARED_SDE
        .get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed()))
        .clone();
    ServerContext::with_sde_and_repository(sde, Arc::new(eve_db::WorldRepository::with_demo_seed()))
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
}

fn clone_type_with(
    sde: &mut Arc<SdeRegistry>,
    base_tid: u32,
    new_tid: u32,
    group_id: u32,
    category_id: u32,
) {
    let reg = Arc::make_mut(sde);
    let mut rec: InvType = reg
        .types
        .get(&base_tid)
        .cloned()
        .or_else(|| reg.types.values().next().cloned())
        .unwrap();
    rec.type_id = new_tid;
    rec.group_id = group_id;
    rec.category_id = category_id;
    rec.name = format!("ExtraType_{new_tid}");
    rec.volume = 1.0;
    rec.capacity = 0.0;
    rec.default_attributes.clear();
    reg.types.insert(new_tid, rec);
}

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

#[test]
fn jump_clone_station_standing_fulcrum_skills_funds_and_structure_errors() {
    assert_eq!(jump_clone_install_fee_cents(), IskCents::from_isk(900_000));

    let mut ctx = test_ctx();
    let kw = HashMap::new();

    // Inject Fulcrum station 60_015_187 with clone bay service
    {
        let sde_mut = Arc::make_mut(&mut ctx.sde);
        let template_st: StationInfo = sde_mut
            .stations
            .values()
            .find(|st| st.has_clone_bay)
            .cloned()
            .unwrap();
        let mut fulcrum = template_st.clone();
        fulcrum.station_id = 60_015_187;
        fulcrum.name = "The Fulcrum".into();
        fulcrum.owner_corporation_id = 1_000_132;
        fulcrum.owner_faction_id = Some(500_010);
        sde_mut.stations.insert(60_015_187, fulcrum);
    }

    let clone_station_id = ctx
        .sde
        .stations
        .values()
        .find(|st| st.has_clone_bay && st.station_id != 60_015_187)
        .map(|st| st.station_id)
        .unwrap();
    let owner_corp_id = ctx.sde.stations[&clone_station_id].owner_corporation_id;
    let struct_id = 1_048_000_001i64;

    let (user_id, char_id, corp_id) = ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.values_mut().next().unwrap();
        ch.clone_grade = 2;
        ch.station_id = Some(60_015_187);
        ch.war_faction_id = None;
        ch.balance = IskCents::from_isk(5_000_000);
        let uid = ch.user_id;
        let cid = ch.character_id;
        let corp = ch.corporation_id;
        snap.jump_clones.remove(&cid);
        snap.skills.remove(&cid);
        (uid, cid, corp)
    });

    let mut sess = SessionState::new(760);
    sess.user_id = Some(user_id);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(corp_id);
    sess.solar_system_id = Some(30_000_142);

    // 1. Undocked errors on InstallCloneInStation, InstallCloneInStructure, GetStationCloneState, GetPriceForClone
    for m in [
        "InstallCloneInStation",
        "InstallCloneInStructure",
        "GetStationCloneState",
        "GetPriceForClone",
    ] {
        assert_eq!(
            handle_clones_and_sheet(&ctx, &mut sess, "jumpCloneSvc", m, &[], &kw)
                .unwrap()
                .unwrap_err()
                .code,
            "MustBeDocked"
        );
    }

    // 2. Fulcrum station without pirate militia enlistment -> FulcrumCloneRequiresPirateEnlistment
    sess.station_id = Some(60_015_187);
    assert_eq!(
        handle_clones_and_sheet(&ctx, &mut sess, "jumpCloneSvc", "InstallCloneInStation", &[], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "FulcrumCloneRequiresPirateEnlistment"
    );

    // 3. Unknown station in SDE -> StationNotFound
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().station_id = Some(69_999_998);
    });
    sess.station_id = Some(69_999_998);
    assert_eq!(
        handle_clones_and_sheet(&ctx, &mut sess, "jumpCloneSvc", "InstallCloneInStation", &[], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "StationNotFound"
    );

    // 4. Negative standing with station owner -> InsufficientStanding
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.station_id = Some(clone_station_id);
        ch.standings.insert(owner_corp_id, -0.5);
    });
    sess.station_id = Some(clone_station_id);
    assert_eq!(
        handle_clones_and_sheet(&ctx, &mut sess, "jumpCloneSvc", "InstallCloneInStation", &[], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "InsufficientStanding"
    );

    // 5. Clear negative standing, no Infomorph Psychology -> InfomorphPsychologyRequired
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().standings.clear();
    });
    assert_eq!(
        handle_clones_and_sheet(&ctx, &mut sess, "jumpCloneSvc", "InstallCloneInStation", &[], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "InfomorphPsychologyRequired"
    );

    // 6. Train Infomorph Psychology I (max 1 clone) + Synchronizing V (33399), but 0 ISK -> InsufficientFunds
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().balance = IskCents(0);
        let sk = snap.skills.entry(char_id).or_default();
        sk.insert(24_242, TrainedSkillRecord { type_id: 24_242, level: 1, skill_points: 1_000.0 });
        sk.insert(33_399, TrainedSkillRecord { type_id: 33_399, level: 5, skill_points: 256_000.0 });
    });
    assert_eq!(
        handle_clones_and_sheet(&ctx, &mut sess, "jumpCloneSvc", "InstallCloneInStation", &[], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "InsufficientFunds"
    );

    // 7. Fund character, install 1 clone (hitting max_clones = 1), then test MaxJumpClonesReached at structure
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().balance = IskCents::from_isk(5_000_000);
    });
    let jc_rep = handle_clones_and_sheet(&ctx, &mut sess, "jumpCloneSvc", "InstallCloneInStation", &[], &kw)
        .unwrap()
        .unwrap();
    // Synchronizing V reduces 24h (86400s) by 50% to 19h (68400s)
    assert_eq!(jc_rep.dict_get("cooldownSeconds"), Some(&PyRep::Integer(68_400)));

    // 8. Structure error branches: StructureNotFound, NoCloneBayService, MaxJumpClonesReached
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.station_id = None;
        ch.structure_id = Some(struct_id);
    });
    sess.station_id = Some(struct_id);
    sess.structure_id = Some(struct_id);
    assert_eq!(
        handle_clones_and_sheet(&ctx, &mut sess, "jumpCloneSvc", "InstallCloneInStructure", &[], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "StructureNotFound"
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            struct_id,
            UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 35_832,
                owner_corp_id: corp_id,
                solar_system_id: 30_000_142,
                name: "Test Citadel".into(),
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 0, // unpowered -> NoCloneBayService
                ..Default::default()
            },
        );
        let bay_id = put_item(snap, 35_894, corp_id, struct_id, InventoryFlag::ServiceSlot0, 1, true);
        snap.module_online_states.insert(bay_id, true);
    });
    assert_eq!(
        handle_clones_and_sheet(&ctx, &mut sess, "jumpCloneSvc", "InstallCloneInStructure", &[], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "NoCloneBayService"
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.get_mut(&struct_id).unwrap().fuel_blocks = 100;
    });
    assert_eq!(
        handle_clones_and_sheet(&ctx, &mut sess, "jumpCloneSvc", "InstallCloneInStructure", &[], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "MaxJumpClonesReached"
    );

    // InvalidJumpCloneID on DestroyInstalledClone(0)
    assert_eq!(
        handle_clones_and_sheet(&ctx, &mut sess, "jumpCloneSvc", "DestroyInstalledClone", &[PyRep::Integer(0)], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidJumpCloneID"
    );
}

#[test]
fn sheet_omega_custom_ship_clone_state_rowset_paperdoll_resculpt_and_apparel_unequip() {
    let mut ctx = test_ctx();
    let kw = HashMap::new();
    let shirt_tid = 99_911u32;
    let pants_tid = 99_912u32;
    let shirt2_tid = 99_913u32;
    clone_type_with(&mut ctx.sde, 34, shirt_tid, 1085, 30);
    clone_type_with(&mut ctx.sde, 34, pants_tid, 1086, 30);
    clone_type_with(&mut ctx.sde, 34, shirt2_tid, 1085, 30); // Same group 1085 as shirt_tid

    let station_id = 60_003_760i64;
    let struct_id = 1_048_000_000_002i64;
    let (user_id, char_id, corp_id, shirt_id, pants_id, shirt2_id) = ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.values_mut().next().unwrap();
        let uid = ch.user_id;
        let cid = ch.character_id;
        let corp = ch.corporation_id;
        let ship_id = ch.active_ship_id;
        ch.clone_grade = 2; // Omega
        ch.gender = 1; // male
        ch.station_id = Some(station_id);
        ch.balance = IskCents::from_isk(10_000_000);
        ch.last_clone_jump_unix = 1_750_000_000;
        ch.neural.last_remap_unix = Some(1_740_000_000);
        ch.neural.remap_cooldown_until_unix = Some(1_770_000_000);
        snap.skills.entry(cid).or_default().insert(
            24_242,
            TrainedSkillRecord {
                type_id: 24_242,
                level: 5,
                skill_points: 256_000.0,
            },
        );
        snap.item_custom_names.insert(ship_id, "Named Ship".into());
        snap.skill_queues.insert(
            cid,
            vec![SkillQueueEntry {
                type_id: 3300,
                target_level: 4,
                start_sp: 8_000.0,
                end_sp: 45_255.0,
                duration_seconds: 3_600.0,
                started_at_unix: None,
            }],
        );
        // Single unnamed jump clone with implants -> triggers direct Rowset return in GetCloneState
        snap.jump_clones.insert(
            cid,
            vec![JumpCloneRecord {
                jump_clone_id: 5001,
                character_id: cid,
                location_id: station_id,
                implants: vec![9899],
            }],
        );
        snap.structures.insert(
            struct_id,
            UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 35_832,
                owner_corp_id: corp,
                solar_system_id: 30_000_142,
                name: "Asset Structure".into(),
                state: "ShieldVulnerable".into(),
                ..Default::default()
            },
        );
        put_item(snap, 34, cid, struct_id, InventoryFlag::Hangar, 10, false);
        let s1 = put_item(snap, shirt_tid, cid, station_id, InventoryFlag::Hangar, 1, true);
        let p1 = put_item(snap, pants_tid, cid, cid, InventoryFlag::Wardrobe, 1, true);
        let s2 = put_item(snap, shirt2_tid, cid, station_id, InventoryFlag::Hangar, 1, true);
        (uid, cid, corp, s1, p1, s2)
    });

    let mut sess = SessionState::new(761);
    sess.user_id = Some(user_id);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(corp_id);
    sess.station_id = Some(station_id);
    sess.solar_system_id = Some(30_000_142);

    // 1. GetCharacterInfo with Omega, custom ship name, and skill queue
    let sheet = handle_clones_and_sheet(&ctx, &mut sess, "charMgr", "GetCharacterInfo", &[], &kw)
        .unwrap()
        .unwrap();
    assert_eq!(sheet.dict_get("shipName"), Some(&PyRep::String("Named Ship".into())));
    assert_eq!(sheet.dict_get("trainingSkillTypeID"), Some(&PyRep::Integer(3300)));

    // 2. GetCloneState with 1 unnamed clone + implants returns Rowset directly
    let cs = handle_clones_and_sheet(&ctx, &mut sess, "jumpCloneSvc", "GetCloneState", &[], &kw)
        .unwrap()
        .unwrap();
    assert!(matches!(cs, PyRep::Rowset { .. }));

    // 3. ListStations includes Upwell structure items
    let st_rs = handle_clones_and_sheet(&ctx, &mut sess, "charUnboundMgr", "ListStations", &[], &kw)
        .unwrap()
        .unwrap();
    let PyRep::Rowset { rows, .. } = st_rs else {
        panic!("expected rowset");
    };
    assert!(rows.iter().any(|r| r[0].as_i64() == Some(struct_id)));

    // 4. jumpCloneSvc delegate endpoints
    assert!(handle_clones_and_sheet(
        &ctx,
        &mut sess,
        "jumpCloneSvc",
        "SetJumpCloneName",
        &[PyRep::Integer(5001), PyRep::String("Alpha Clone".into())],
        &kw,
    )
    .unwrap()
    .is_ok());
    assert!(handle_clones_and_sheet(&ctx, &mut sess, "jumpCloneSvc", "ValidateInstallJumpClone", &[], &kw)
        .unwrap()
        .is_ok());
    assert!(handle_clones_and_sheet(&ctx, &mut sess, "jumpCloneSvc", "GetShipCloneState", &[], &kw)
        .unwrap()
        .is_ok());
    assert!(handle_clones_and_sheet(&ctx, &mut sess, "jumpCloneSvc", "ResetLastCloneJumpTime", &[], &kw)
        .unwrap()
        .is_err());

    // 5. PaperDoll DNA list (`[gender_label]` for `Doll.MatchDNA`) and empty-args batch fallback
    let pd = handle_clones_and_sheet(&ctx, &mut sess, "paperDollServer", "GetPaperDollData", &[], &kw)
        .unwrap()
        .unwrap();
    assert_eq!(pd, PyRep::List(vec![PyRep::String("male".into())]));
    let batch = handle_clones_and_sheet(&ctx, &mut sess, "paperDollServer", "GetPaperDollDataBatch", &[], &kw)
        .unwrap()
        .unwrap();
    assert!(matches!(batch, PyRep::Dict(d) if d.len() == 1 && d[0].1 == PyRep::List(vec![PyRep::String("male".into())])));

    // 6. UpdateExistingCharacterFull without resculpt token -> NoResculptAvailable; with account token -> succeeds
    let doll_kv = PyRep::keyval([("hairDarkness", PyRep::Real(0.5))]);
    assert_eq!(
        handle_clones_and_sheet(
            &ctx,
            &mut sess,
            "paperDollServer",
            "UpdateExistingCharacterFull",
            &[doll_kv.clone()],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .message,
        "NoResculptAvailable"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.cluster_and_runtime
            .account_resculpt_tokens
            .insert(user_id.to_string(), 1);
    });
    assert!(handle_clones_and_sheet(
        &ctx,
        &mut sess,
        "paperDollServer",
        "UpdateExistingCharacterFull",
        &[PyRep::Integer(char_id), doll_kv],
        &kw,
    )
    .unwrap()
    .is_ok());

    // 7. UpdateExistingCharacterLimited: duplicate apparel, same wearable group conflict, and unequipping pants_id
    assert_eq!(
        handle_clones_and_sheet(
            &ctx,
            &mut sess,
            "paperDollServer",
            "UpdateExistingCharacterLimited",
            &[PyRep::keyval([(
                "apparel",
                PyRep::List(vec![PyRep::Integer(shirt_id), PyRep::Integer(shirt_id)]),
            )])],
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
            &[PyRep::keyval([(
                "apparel",
                PyRep::List(vec![PyRep::Integer(shirt_id), PyRep::Integer(shirt2_id)]),
            )])],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidApparelItem"
    );
    // Equip only shirt_id -> pants_id (previously in Wardrobe) is unequipped to station_id Hangar!
    handle_clones_and_sheet(
        &ctx,
        &mut sess,
        "paperDollServer",
        "UpdateExistingCharacterLimited",
        &[PyRep::Dict(vec![(
            PyRep::String("apparel".into()),
            PyRep::List(vec![PyRep::Integer(shirt_id)]),
        )])],
        &kw,
    )
    .unwrap()
    .unwrap();
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&shirt_id].flag, InventoryFlag::Wardrobe);
        assert_eq!(snap.items[&pants_id].flag, InventoryFlag::Hangar);
        assert_eq!(snap.items[&pants_id].location_id, station_id);
    });
}

#[test]
fn clone_fee_zero_vs_positive_notifications_female_paperdoll_tokens_and_apparel_live_listener() {
    use crate::services::station_structure::structure_upwell_ops::{
        load_ext_state, save_ext_state, StructureAccessProfile,
    };

    let mut ctx = test_ctx();
    let hat_a_tid = 900_981u32;
    let hat_b_tid = 900_982u32;
    clone_type_with(&mut ctx.sde, 34, hat_a_tid, 1083, 30);
    clone_type_with(&mut ctx.sde, 34, hat_b_tid, 1084, 30);
    let kw = HashMap::new();
    let station_id = 60_003_760_i64;
    let struct_free = 1_049_000_001_i64;
    let struct_paid = 1_049_000_002_i64;

    let (user_id, char_id, corp_id, female_cid, hat_a, hat_b) = ctx.db.with_mut_snapshot(|snap| {
        let mut keys: Vec<i64> = snap.characters.keys().copied().collect();
        keys.sort_unstable();
        let cid = keys[0];
        let fem = keys.get(1).copied().unwrap_or(90_000_002);
        let uid = snap.characters[&cid].user_id;
        let corp = snap.characters[&cid].corporation_id;

        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.clone_grade = 2;
            ch.gender = 1;
            ch.station_id = Some(station_id);
            ch.structure_id = None;
            ch.balance = IskCents::from_isk(10_000_000);
        }
        if let Some(fch) = snap.characters.get_mut(&fem) {
            fch.gender = 0; // female
        }
        let sk = snap.skills.entry(cid).or_default();
        sk.insert(24_242, TrainedSkillRecord { type_id: 24_242, level: 5, skill_points: 256_000.0 });
        sk.insert(33_407, TrainedSkillRecord { type_id: 33_407, level: 5, skill_points: 256_000.0 });
        snap.jump_clones.remove(&cid);

        for sid in [struct_free, struct_paid] {
            snap.structures.insert(
                sid,
                UpwellStructureRecord {
                    structure_id: sid,
                    type_id: 35_832,
                    owner_corp_id: corp,
                    solar_system_id: 30_000_142,
                    name: format!("CloneBay_{sid}"),
                    state: "ShieldVulnerable".into(),
                    quantum_core_installed: true,
                    fuel_blocks: 500,
                    ..Default::default()
                },
            );
            let bay = put_item(snap, 35_894, corp, sid, InventoryFlag::ServiceSlot0, 1, true);
            snap.module_online_states.insert(bay, true);
        }

        let mut ext = load_ext_state(snap);
        let mut p_free = StructureAccessProfile {
            profile_id: 91_001,
            owner_corp_id: corp,
            name: "FreeClone".into(),
            ..Default::default()
        };
        p_free.allowed_characters.insert(cid);
        p_free.service_acl_rules.insert(23, vec![(0, 0)]); // 0 ISK clone fee
        let mut p_paid = StructureAccessProfile {
            profile_id: 91_002,
            owner_corp_id: corp,
            name: "PaidClone".into(),
            ..Default::default()
        };
        p_paid.allowed_characters.insert(cid);
        p_paid.service_acl_rules.insert(23, vec![(0, 10_000)]); // 100 cents = 1 ISK clone fee
        ext.access_profiles.insert(struct_free, p_free.clone());
        ext.profiles_by_id.insert(91_001, p_free);
        ext.access_profiles.insert(struct_paid, p_paid.clone());
        ext.profiles_by_id.insert(91_002, p_paid);
        ext.structure_profile_id.insert(struct_free, 91_001);
        ext.structure_profile_id.insert(struct_paid, 91_002);
        save_ext_state(snap, &ext);

        let a = put_item(snap, hat_a_tid, cid, station_id, InventoryFlag::Hangar, 1, true);
        let b = put_item(snap, hat_b_tid, cid, station_id, InventoryFlag::Hangar, 1, true);
        (uid, cid, corp, fem, a, b)
    });

    ctx.db
        .acquire_online_lock(user_id, 78_100, Some(char_id), true, ctx.now_unix())
        .expect("online lock");

    let mut sess = SessionState::new(78_100);
    sess.user_id = Some(user_id);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(corp_id);
    sess.station_id = Some(station_id);
    sess.solar_system_id = Some(30_000_142);

    // 1. InstallCloneInStation: NPC station fee > 0 emits OnAccountChange (L231, L435)
    ctx.pending_notifications.write().unwrap().clear();
    let npc_jc = handle_clones_and_sheet(&ctx, &mut sess, "jumpCloneSvc", "InstallCloneInStation", &[], &kw)
        .unwrap()
        .unwrap();
    assert_eq!(npc_jc.dict_get("locationID"), Some(&PyRep::Integer(station_id)));
    assert!(
        ctx.pending_notifications
            .read()
            .unwrap()
            .get(&char_id)
            .is_some_and(|v| v.iter().any(|n| n.event == "OnAccountChange")),
        "positive NPC clone install fee must emit OnAccountChange"
    );

    // InstallCloneInStation docked at free ACL structure (install_fee == 0) must NOT emit OnAccountChange (L435 `>=`/`==`/`<`)
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().station_id = Some(struct_free);
    });
    sess.station_id = Some(struct_free);
    ctx.pending_notifications.write().unwrap().clear();
    let free_st_jc = handle_clones_and_sheet(&ctx, &mut sess, "jumpCloneSvc", "InstallCloneInStation", &[], &kw)
        .unwrap()
        .unwrap();
    assert_eq!(free_st_jc.dict_get("locationID"), Some(&PyRep::Integer(struct_free)));
    assert!(
        !ctx.pending_notifications
            .read()
            .unwrap()
            .get(&char_id)
            .is_some_and(|v| v.iter().any(|n| n.event == "OnAccountChange")),
        "zero clone install fee in station slot must not emit OnAccountChange"
    );

    // 2. InstallCloneInStructure: free fee == 0 vs paid fee > 0 (L453, L581)
    ctx.db.with_mut_snapshot(|snap| {
        snap.jump_clones.remove(&char_id);
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.station_id = None;
        ch.structure_id = Some(struct_free);
    });
    sess.station_id = None;
    sess.structure_id = Some(struct_free);
    ctx.pending_notifications.write().unwrap().clear();
    let free_struct_jc =
        handle_clones_and_sheet(&ctx, &mut sess, "jumpCloneSvc", "InstallCloneInStructure", &[], &kw)
            .unwrap()
            .unwrap();
    assert_eq!(free_struct_jc.dict_get("locationID"), Some(&PyRep::Integer(struct_free)));
    assert!(
        !ctx.pending_notifications
            .read()
            .unwrap()
            .get(&char_id)
            .is_some_and(|v| v.iter().any(|n| n.event == "OnAccountChange")),
        "zero fee InstallCloneInStructure must not emit OnAccountChange"
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().structure_id = Some(struct_paid);
    });
    sess.structure_id = Some(struct_paid);
    ctx.pending_notifications.write().unwrap().clear();
    let paid_struct_jc =
        handle_clones_and_sheet(&ctx, &mut sess, "jumpCloneSvc", "InstallCloneInStructure", &[], &kw)
            .unwrap()
            .unwrap();
    assert_eq!(paid_struct_jc.dict_get("locationID"), Some(&PyRep::Integer(struct_paid)));
    assert!(
        ctx.pending_notifications
            .read()
            .unwrap()
            .get(&char_id)
            .is_some_and(|v| v.iter().any(|n| n.event == "OnAccountChange")),
        "positive fee InstallCloneInStructure must emit OnAccountChange"
    );

    // 3. GetPaperDollData & GetPaperDollDataBatch distinguish female (gender=0) vs male (L859, L887, L891, L906)
    let fem_pd = handle_clones_and_sheet(
        &ctx,
        &mut sess,
        "paperDollServer",
        "GetPaperDollData",
        &[PyRep::Integer(female_cid)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(fem_pd, PyRep::List(vec![PyRep::String("female".into())]));
    let batch_pd = handle_clones_and_sheet(
        &ctx,
        &mut sess,
        "paperDollServer",
        "GetPaperDollDataBatch",
        &[PyRep::List(vec![PyRep::Integer(female_cid), PyRep::Integer(char_id)])],
        &kw,
    )
    .unwrap()
    .unwrap();
    let PyRep::Dict(batch_entries) = batch_pd else {
        panic!("expected batch dict");
    };
    assert!(batch_entries.iter().any(|(k, v)| *k == PyRep::Integer(female_cid) && *v == PyRep::List(vec![PyRep::String("female".into())])));
    assert!(batch_entries.iter().any(|(k, v)| *k == PyRep::Integer(char_id) && *v == PyRep::List(vec![PyRep::String("male".into())])));

    // 4. UpdateExistingCharacterFull: cluster_and_runtime.resculpt_pending ONLY (L1020) and tokens=2 -> 1 (L1034, L1037)
    let doll_kv = PyRep::keyval([("skinTone", PyRep::Real(0.25))]);
    ctx.db.with_mut_snapshot(|snap| {
        snap.cluster_and_runtime.resculpt_pending.insert(char_id);
        snap.cluster_and_runtime
            .account_resculpt_tokens
            .remove(&user_id.to_string());
    });
    assert!(handle_clones_and_sheet(
        &ctx,
        &mut sess,
        "paperDollServer",
        "UpdateExistingCharacterFull",
        &[doll_kv.clone()],
        &kw,
    )
    .unwrap()
    .is_ok());

    // Now account_resculpt_tokens = 2 -> consuming 1 yields exact 1 (kills `-` -> `+` => 3 and `/` => 2)
    ctx.db.with_mut_snapshot(|snap| {
        snap.cluster_and_runtime.resculpt_pending.remove(&char_id);
        snap.cluster_and_runtime
            .account_resculpt_tokens
            .insert(user_id.to_string(), 2);
    });
    assert!(handle_clones_and_sheet(
        &ctx,
        &mut sess,
        "paperDollServer",
        "UpdateExistingCharacterFull",
        &[doll_kv],
        &kw,
    )
    .unwrap()
    .is_ok());
    assert_eq!(
        ctx.db.with_snapshot(|s| s
            .cluster_and_runtime
            .account_resculpt_tokens
            .get(&user_id.to_string())
            .copied()),
        Some(1)
    );

    // 5. UpdateExistingCharacterLimited apparel OnItemChange obeys `!session.live_notify_listener` (L1236)
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.station_id = Some(station_id);
        ch.structure_id = None;
    });
    sess.station_id = Some(station_id);
    sess.structure_id = None;
    let _ = sess.drain_notifications();
    sess.live_notify_listener = false;
    handle_clones_and_sheet(
        &ctx,
        &mut sess,
        "paperDollServer",
        "UpdateExistingCharacterLimited",
        &[PyRep::keyval([(
            "apparel",
            PyRep::List(vec![PyRep::Integer(hat_a)]),
        )])],
        &kw,
    )
    .unwrap()
    .unwrap();
    let pushed = sess.drain_notifications();
    assert!(
        pushed.iter().any(|(ev, _)| ev == "OnItemChange"),
        "expected OnItemChange pushed to session when !live_notify_listener"
    );

    sess.live_notify_listener = true;
    handle_clones_and_sheet(
        &ctx,
        &mut sess,
        "paperDollServer",
        "UpdateExistingCharacterLimited",
        &[PyRep::keyval([(
            "apparel",
            PyRep::List(vec![PyRep::Integer(hat_b)]),
        )])],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert!(sess.drain_notifications().is_empty());
}
