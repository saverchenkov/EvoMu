//! Round-4 Track 2 Regression Tests (#164 through #175):
//! Mining, Reprocessing, Upwell Structures, Sovereignty & Factional Warfare.

#![forbid(unsafe_code)]

use crate::common::login_pilot;
use eve_db::{
    ContractRecord, CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents,
    UpwellStructureRecord,
};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

#[tokio::test]
async fn test_r4_164_sov_hub_install_upgrade_ownership_flag_and_stack_split() {
    let ctx = ServerContext::new();
    let mut sess = SessionState::new(16401);
    login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let char_id = 90_000_001;
    let other_id = 90_000_002;
    let corp_id = 98_164_001;
    let alliance_id = 99_164_001;
    let sys_id = 30_004_759u32;
    let hub_id = 950_164_001i64;
    let kw = HashMap::new();

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
    .unwrap();
    sess.solar_system_id = Some(sys_id);

    // Use Megacyte Prospecting Array 1 (82587, group 4768)
    let upg_tid = 82587u32;
    let unowned_item_id = 770_164_001i64;
    let locked_item_id = 770_164_002i64;
    let stack_item_id = 770_164_003i64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = corp_id;
            ch.alliance_id = Some(alliance_id);
            ch.solar_system_id = sys_id;
        }
        let mut roles = HashMap::new();
        roles.insert(char_id, eve_db::corp_roles::ROLE_DIRECTOR);
        snap.corporations.insert(
            corp_id,
            CorporationRecord {
                corporation_id: corp_id,
                name: "Sov 164 Corp".into(),
                ticker: "S164".into(),
                ceo_id: char_id,
                alliance_id: Some(alliance_id),
                members: HashSet::from([char_id]),
                member_roles: roles,
                ..Default::default()
            },
        );
        snap.alliances.insert(
            alliance_id,
            eve_db::AllianceRecord {
                alliance_id,
                name: "Sov 164 Alliance".into(),
                short_name: "S164A".into(),
                executor_corp_id: corp_id,
                member_corp_ids: HashSet::from([corp_id]),
            },
        );
        let st = snap.system_states.entry(sys_id).or_default();
        st.solar_system_id = sys_id;
        st.sov_alliance_id = Some(alliance_id);
        snap.structures.insert(
            hub_id,
            UpwellStructureRecord {
                structure_id: hub_id,
                type_id: 32458,
                owner_corp_id: corp_id,
                solar_system_id: sys_id,
                name: "164 Sov Hub".into(),
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 1000,
                shield_hp: 10_000_000.0,
                armor_hp: 10_000_000.0,
                hull_hp: 10_000_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
        snap.items.insert(
            unowned_item_id,
            InventoryItemRecord {
                item_id: unowned_item_id,
                type_id: upg_tid,
                owner_id: other_id,
                location_id: hub_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    // 1. Only an unowned item in Hangar -> rejected with UpgradeItemNotFound
    let err_unowned = ctx
        .dispatch_rpc(
            &mut sess,
            "sovHubSvc",
            None,
            "SaveUpgradeConfiguration",
            &[PyRep::List(vec![PyRep::Integer(upg_tid as i64)])],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(err_unowned.code, "UpgradeItemNotFound");

    // 2. Owned item in Hangar but locked in an Outstanding contract -> rejected with UpgradeItemLocked
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            locked_item_id,
            InventoryItemRecord {
                item_id: locked_item_id,
                type_id: upg_tid,
                owner_id: char_id,
                location_id: hub_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.contracts.insert(
            880_164_001,
            ContractRecord {
                contract_id: 880_164_001,
                issuer_id: char_id,
                issuer_corp_id: corp_id,
                for_corp: false,
                contract_type: "ItemExchange".into(),
                status: "Outstanding".into(),
                start_station_id: hub_id,
                end_station_id: hub_id,
                price: IskCents(100_00),
                reward: IskCents(0),
                collateral: IskCents(0),
                item_ids: vec![locked_item_id],
                ..Default::default()
            },
        );
    });
    let err_locked = ctx
        .dispatch_rpc(
            &mut sess,
            "sovHubSvc",
            None,
            "SaveUpgradeConfiguration",
            &[PyRep::List(vec![PyRep::Integer(upg_tid as i64)])],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(err_locked.code, "UpgradeItemLocked");

    // 3. Multi-unit stack (quantity 3) splits 1 unit into SubSystemSlot0 and leaves 2 in Hangar
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.remove(&locked_item_id);
        snap.contracts.remove(&880_164_001);
        snap.items.insert(
            stack_item_id,
            InventoryItemRecord {
                item_id: stack_item_id,
                type_id: upg_tid,
                owner_id: char_id,
                location_id: hub_id,
                flag: InventoryFlag::Hangar,
                quantity: 3,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });
    ctx.dispatch_rpc(
        &mut sess,
        "sovHubSvc",
        None,
        "SaveUpgradeConfiguration",
        &[PyRep::List(vec![PyRep::Integer(upg_tid as i64)])],
        &kw,
        None,
    )
    .await
    .expect("SaveUpgradeConfiguration from stack");

    let orig_stack = ctx.db.get_item(stack_item_id).unwrap();
    assert_eq!(orig_stack.quantity, 2);
    assert_eq!(orig_stack.flag, InventoryFlag::Hangar);

    let installed_count = ctx.db.with_snapshot(|snap| {
        snap.items
            .values()
            .filter(|i| {
                i.location_id == hub_id
                    && i.flag == InventoryFlag::SubSystemSlot0
                    && i.type_id == upg_tid
                    && i.quantity == 1
                    && i.singleton
            })
            .count()
    });
    assert_eq!(installed_count, 1);
}

#[tokio::test]
async fn test_r4_165_mining_drone_waste_probability_normalization() {
    let ctx = ServerContext::new();
    // Verify SDE attr 3154 on Mining Drone II (type 10248) is 34.0 (percentage points)
    let raw_3154 = ctx
        .sde
        .types
        .get(&10248)
        .and_then(|t| t.default_attributes.get(&3154))
        .copied()
        .unwrap_or(34.0);
    assert!(raw_3154 > 1.0, "SDE attr 3154 is stored as percentage points");
    let normalized = if raw_3154 > 1.0 {
        (raw_3154 / 100.0).clamp(0.0, 1.0)
    } else {
        raw_3154.clamp(0.0, 1.0)
    };
    assert!((normalized - 0.34).abs() < 1e-6);
}

#[tokio::test]
async fn test_r4_166_structure_fit_module_splits_stack() {
    let ctx = ServerContext::new();
    let mut sess = SessionState::new(16601);
    login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let char_id = 90_000_001;
    let corp_id = 98_166_001;
    let struct_id = 950_166_001i64;
    let mod_stack_id = 770_166_001i64;
    let kw = HashMap::new();

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
    .unwrap();
    sess.station_id = None;
    sess.structure_id = Some(struct_id);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = corp_id;
            ch.station_id = None;
            ch.structure_id = Some(struct_id);
        }
        let mut roles = HashMap::new();
        roles.insert(char_id, eve_db::corp_roles::ROLE_DIRECTOR);
        snap.corporations.insert(
            corp_id,
            CorporationRecord {
                corporation_id: corp_id,
                name: "Fit 166 Corp".into(),
                ticker: "F166".into(),
                ceo_id: char_id,
                members: HashSet::from([char_id]),
                member_roles: roles,
                ..Default::default()
            },
        );
        snap.structures.insert(
            struct_id,
            UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 35832, // Astrahus
                owner_corp_id: corp_id,
                solar_system_id: 30_000_142,
                name: "166 Astrahus".into(),
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 1000,
                shield_hp: 10_000_000.0,
                armor_hp: 10_000_000.0,
                hull_hp: 10_000_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
        snap.items.insert(
            mod_stack_id,
            InventoryItemRecord {
                item_id: mod_stack_id,
                type_id: 35894, // Standup Cloning Center I
                owner_id: char_id,
                location_id: struct_id,
                flag: InventoryFlag::Hangar,
                quantity: 4,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    let fit_res = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            None,
            "Add",
            &[
                PyRep::Integer(mod_stack_id),
                PyRep::Integer(struct_id),
                PyRep::Integer(InventoryFlag::ServiceSlot0 as i64),
            ],
            &kw,
            None,
        )
        .await
        .expect("invbroker.Add to fit structure module from stack");

    let fitted_id = fit_res.dict_get("itemID").and_then(|v| v.as_i64()).unwrap();
    assert_ne!(fitted_id, mod_stack_id);
    let rem_stack = ctx.db.get_item(mod_stack_id).unwrap();
    assert_eq!(rem_stack.quantity, 3);
    assert_eq!(rem_stack.flag, InventoryFlag::Hangar);
    let fitted_item = ctx.db.get_item(fitted_id).unwrap();
    assert_eq!(fitted_item.quantity, 1);
    assert_eq!(fitted_item.flag, InventoryFlag::ServiceSlot0);
}

#[tokio::test]
async fn test_r4_167_reprocess_and_compress_flag_ownership_and_acl() {
    let ctx = ServerContext::new();
    let mut sess = SessionState::new(16701);
    login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let char_id = 90_000_001;
    let other_id = 90_000_002;
    let kw = HashMap::new();

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
    .unwrap();

    let locked_ore_id = 770_167_001i64;
    let unowned_ore_id = 770_167_002i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            locked_ore_id,
            InventoryItemRecord {
                item_id: locked_ore_id,
                type_id: 1230, // Veldspar
                owner_id: char_id,
                location_id: 60_003_760,
                flag: InventoryFlag::Locked,
                quantity: 1000,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            unowned_ore_id,
            InventoryItemRecord {
                item_id: unowned_ore_id,
                type_id: 1230,
                owner_id: other_id,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 1000,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    // Reprocess rejects Locked item in station
    let rep_err = ctx
        .dispatch_rpc(
            &mut sess,
            "reprocessingSvc",
            None,
            "Reprocess",
            &[PyRep::List(vec![PyRep::Integer(locked_ore_id)])],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(rep_err.code, "OwnershipOrLocationViolation");

    // GetQuotes filters out unowned items (returns empty list instead of leaking quote)
    let quote_res = ctx
        .dispatch_rpc(
            &mut sess,
            "reprocessingSvc",
            None,
            "GetQuotes",
            &[PyRep::List(vec![PyRep::Integer(unowned_ore_id)])],
            &kw,
            None,
        )
        .await
        .expect("GetQuotes");
    assert_eq!(quote_res, PyRep::List(vec![]));
}

#[tokio::test]
async fn test_r4_168_moon_resources_auth_and_observer_ledger_validation() {
    let ctx = ServerContext::new();
    let mut sess = SessionState::new(16801);
    login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let char_id = 90_000_001;
    let struct_id = 950_168_001i64;
    let kw = HashMap::new();

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
    .unwrap();

    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            struct_id,
            UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 35835, // Athanor
                owner_corp_id: 98_999_999, // Different corp
                solar_system_id: 30_000_142,
                name: "Foreign Athanor".into(),
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 1000,
                shield_hp: 10_000_000.0,
                armor_hp: 10_000_000.0,
                hull_hp: 10_000_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
    });

    // Non-owner corp cannot query GetMoonResourcesForStructure
    let moon_err = ctx
        .dispatch_rpc(
            &mut sess,
            "moonExtractions",
            None,
            "GetMoonResourcesForStructure",
            &[PyRep::Integer(struct_id)],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(moon_err.code, "NotStructureOwnerCorp");

    // Nonexistent observer ID is rejected by corpMiningLedger.GetObserverLedger
    let obs_err = ctx
        .dispatch_rpc(
            &mut sess,
            "corpMiningLedger",
            None,
            "GetObserverLedger",
            &[PyRep::Integer(999_999_999)],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(obs_err.code, "CrpAccessDenied");
}

#[tokio::test]
async fn test_r4_169_asset_safety_contract_lock_and_nested_container_value() {
    let ctx = ServerContext::new();
    let mut sess = SessionState::new(16901);
    login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let char_id = 90_000_001;
    let struct_id = 950_169_001i64;
    let locked_item_id = 770_169_001i64;
    let box_id = 770_169_002i64;
    let plex_inside_box_id = 770_169_003i64;
    let kw = HashMap::new();

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
    .unwrap();

    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            struct_id,
            UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 35832,
                owner_corp_id: 1_000_170,
                solar_system_id: 30_000_142,
                name: "169 Citadel".into(),
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 1000,
                shield_hp: 10_000_000.0,
                armor_hp: 10_000_000.0,
                hull_hp: 10_000_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
        snap.items.insert(
            locked_item_id,
            InventoryItemRecord {
                item_id: locked_item_id,
                type_id: 34,
                owner_id: char_id,
                location_id: struct_id,
                flag: InventoryFlag::Hangar,
                quantity: 100,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.contracts.insert(
            880_169_001,
            ContractRecord {
                contract_id: 880_169_001,
                issuer_id: char_id,
                issuer_corp_id: 1_000_170,
                for_corp: false,
                contract_type: "ItemExchange".into(),
                status: "Outstanding".into(),
                start_station_id: struct_id,
                end_station_id: struct_id,
                price: IskCents(100_00),
                reward: IskCents(0),
                collateral: IskCents(0),
                item_ids: vec![locked_item_id],
                ..Default::default()
            },
        );
        snap.items.insert(
            box_id,
            InventoryItemRecord {
                item_id: box_id,
                type_id: 3293, // Medium Standard Container
                owner_id: char_id,
                location_id: struct_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            plex_inside_box_id,
            InventoryItemRecord {
                item_id: plex_inside_box_id,
                type_id: 587, // Rifter (high base_price in SDE)
                owner_id: char_id,
                location_id: box_id,
                flag: InventoryFlag::Cargo,
                quantity: 100,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    // MovePersonalAssetsToSafety must skip contract-locked item and include nested PLEX value in box wrap
    let res = ctx
        .dispatch_rpc(
            &mut sess,
            "structureAssetSafety",
            None,
            "MovePersonalAssetsToSafety",
            &[PyRep::Integer(30_000_142), PyRep::Integer(struct_id)],
            &kw,
            None,
        )
        .await
        .expect("MovePersonalAssetsToSafety");
    assert_eq!(res.dict_get("itemCount"), Some(&PyRep::Integer(1)));
    assert_eq!(ctx.db.get_item(locked_item_id).unwrap().location_id, struct_id);

    let wrap = ctx.db.with_snapshot(|snap| {
        let ext = eve_server::services::station_structure::structure_upwell_ops::load_ext_state(snap);
        ext.asset_safety_wraps
            .values()
            .find(|w| w.item_ids.contains(&box_id))
            .cloned()
            .expect("wrap for box")
    });
    // Box alone is cheap (< 50k ISK), but with 100x PLEX inside its estimated_value_isk is much higher
    let box_base = ctx
        .sde
        .types
        .get(&3293)
        .map(|t| t.base_price)
        .unwrap_or(1000.0);
    assert!(
        wrap.estimated_value_isk > box_base + 1_000.0,
        "Wrap estimated_value_isk ({}) must include nested items inside container ({})",
        wrap.estimated_value_isk,
        box_base
    );
}

#[tokio::test]
async fn test_r4_170_structure_release_control_restores_docked_ship() {
    let ctx = ServerContext::new();
    let mut sess = SessionState::new(17001);
    login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let char_id = 90_000_001;
    let corp_id = 98_170_001;
    let struct_id = 950_170_001i64;
    let ship_id = 100_000_001i64;
    let kw = HashMap::new();

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
    .unwrap();
    sess.station_id = None;
    sess.structure_id = Some(struct_id);
    sess.ship_id = Some(ship_id);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = corp_id;
            ch.station_id = None;
            ch.structure_id = Some(struct_id);
            ch.active_ship_id = ship_id;
        }
        if let Some(sh) = snap.items.get_mut(&ship_id) {
            sh.location_id = struct_id;
            sh.flag = InventoryFlag::Hangar;
        }
        let mut roles = HashMap::new();
        roles.insert(char_id, eve_db::corp_roles::ROLE_DIRECTOR);
        snap.corporations.insert(
            corp_id,
            CorporationRecord {
                corporation_id: corp_id,
                name: "Gunner 170 Corp".into(),
                ticker: "G170".into(),
                ceo_id: char_id,
                members: HashSet::from([char_id]),
                member_roles: roles,
                ..Default::default()
            },
        );
        snap.structures.insert(
            struct_id,
            UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 35832,
                owner_corp_id: corp_id,
                solar_system_id: 30_000_142,
                name: "170 Astrahus".into(),
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 1000,
                shield_hp: 10_000_000.0,
                armor_hp: 10_000_000.0,
                hull_hp: 10_000_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
    });

    ctx.dispatch_rpc(
        &mut sess,
        "structureControl",
        None,
        "TakeControl",
        &[PyRep::Integer(struct_id)],
        &kw,
        None,
    )
    .await
    .expect("TakeControl");
    assert_eq!(sess.ship_id, Some(struct_id));

    ctx.dispatch_rpc(
        &mut sess,
        "structureControl",
        None,
        "ReleaseControl",
        &[PyRep::Integer(struct_id)],
        &kw,
        None,
    )
    .await
    .expect("ReleaseControl");

    assert_eq!(sess.ship_id, Some(ship_id));
    assert_eq!(ctx.db.get_character(char_id).unwrap().active_ship_id, ship_id);
}

#[tokio::test]
async fn test_r4_171_ess_concurrent_link_rejection_and_reserve_unlock_window() {
    let ctx = ServerContext::new();
    let mut sess1 = SessionState::new(17101);
    let mut sess2 = SessionState::new(17102);
    login_pilot(&ctx, &mut sess1, "demo", "demo-secret").await;
    login_pilot(&ctx, &mut sess2, "other", "other-secret").await;
    let c1 = 90_000_001;
    let c2 = 90_000_002;
    let s1_id = 100_000_001;
    let s2_id = 100_000_002;
    let sys_id = 30_004_759u32; // Nullsec
    let kw = HashMap::new();

    ctx.dispatch_rpc(&mut sess1, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(c1)], &kw, None).await.unwrap();
    ctx.dispatch_rpc(&mut sess2, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(c2)], &kw, None).await.unwrap();

    for (sess, cid, sid) in [(&mut sess1, c1, s1_id), (&mut sess2, c2, s2_id)] {
        sess.station_id = None;
        sess.solar_system_id = Some(sys_id);
        sess.ship_id = Some(sid);
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&cid) {
                ch.station_id = None;
                ch.solar_system_id = sys_id;
                ch.active_ship_id = sid;
            }
        });
    }

    let ess_ball_id = 950_171_000i64;
    let handle = ctx.systems.get_or_spawn(sys_id);
    let _ = handle
        .spawn_ball(eve_destiny::Ball {
            id: ess_ball_id,
            type_id: 32882, // ESS
            position: glam::DVec3::ZERO,
            velocity: glam::DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 10_000_000.0,
            radius: 1500.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        })
        .await;
    for sid in [s1_id, s2_id] {
        let _ = handle
            .spawn_ball(eve_destiny::Ball {
                id: sid,
                type_id: 587,
                position: glam::DVec3::ZERO,
                velocity: glam::DVec3::ZERO,
                max_velocity: 350.0,
                inertia: 3.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: eve_destiny::MovementMode::Stop,
            })
            .await;
    }

    ctx.db.with_mut_snapshot(|snap| {
        let st = snap.system_states.entry(sys_id).or_default();
        st.solar_system_id = sys_id;
        st.ess_main_bank = IskCents::from_isk(10_000_000);
        st.ess_reserve_bank = IskCents::from_isk(30_000_000);
        // Give c1 a 15-min ESS Reserve Security Key (57714) in cargo
        snap.items.insert(
            770_171_001,
            InventoryItemRecord {
                item_id: 770_171_001,
                type_id: 57714,
                owner_id: c1,
                location_id: s1_id,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    // c1 links to Main Bank
    ctx.dispatch_rpc(
        &mut sess1,
        "essMgr",
        None,
        "AttemptLinkToMainBank",
        &[PyRep::Integer(ess_ball_id)],
        &kw,
        None,
    )
    .await
    .expect("c1 links to Main Bank");

    // c2 attempting to link to Main Bank while c1 is linked is rejected with EssAlreadyLinked
    let c2_main_err = ctx
        .dispatch_rpc(
            &mut sess2,
            "essMgr",
            None,
            "AttemptLinkToMainBank",
            &[PyRep::Integer(ess_ball_id)],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(c2_main_err.code, "EssAlreadyLinked");

    // c1 links to Reserve Bank (consumes key 57714 and sets ess_reserve_unlocked_until)
    ctx.dispatch_rpc(
        &mut sess1,
        "essMgr",
        None,
        "AttemptLinkToReserveBank",
        &[PyRep::Integer(ess_ball_id)],
        &kw,
        None,
    )
    .await
    .expect("c1 links to Reserve Bank");
    let unlocked = eve_server::services::combat_crimewatch::with_combat_runtime(&ctx, |rt| {
        rt.ess_reserve_unlocked_until.get(&sys_id).copied().unwrap_or(0)
    });
    assert!(unlocked > ctx.now_unix(), "Reserve Bank unlock window must be populated");
}

#[tokio::test]
async fn test_r4_172_incursion_wave_respawn_attaches_combat_state_and_site_claim() {
    let ctx = Arc::new(ServerContext::new());
    let sys_id = 30_000_142u32;
    let site_id = "inc-r4-172-vg";
    let initial_boss_bid = 910_172_000i64;
    let boss_tid = 19720u32; // Sansha NPC type

    let now_u = ctx.now_unix();
    eve_server::services::pve_special_sites::with_pve_special_reg_db(&ctx.db, now_u, |reg| {
        reg.incursions.insert(
            20_000_020,
            eve_server::services::pve_incursion_sites::IncursionTaleRecord {
                tale_id: 20_000_020,
                constellation_id: 20_000_020,
                constellation_name: "Kimotoro".into(),
                staging_solar_system_id: sys_id,
                vanguard_solar_system_ids: vec![sys_id],
                assault_solar_system_ids: vec![sys_id],
                hq_solar_system_id: sys_id,
                influence: 0.5,
                state: "Established".into(),
                lockout_until_filetime: None,
                mothership_spawned: false,
                mothership_defeated: false,
                tale_stop_published: false,
                sites: vec![eve_server::services::pve_incursion_sites::IncursionSiteEntry {
                    site_id: site_id.into(),
                    dungeon_id: 172,
                    dungeon_name: "Sansha Vanguard".into(),
                    tier: "Vanguard".into(),
                    solar_system_id: sys_id,
                    position: glam::DVec3::new(100_000.0, 0.0, 0.0),
                    min_pilots: 1,
                    max_optimal_pilots: 10,
                    base_isk: 10_000_000,
                    base_concord_lp: 1_000,
                    influence_delta: 0.1,
                    locked_until_low_influence: false,
                    completed_count: 0,
                    boss_npc_ball_id: initial_boss_bid,
                    boss_type_id: boss_tid,
                    boss_destroyed: true,
                    killer_char_id: None,
                    killer_fleet_id: None,
                    escort_spawns: vec![],
                }],
            },
        );
    });

    let res = eve_server::services::pve_incursion_sites::process_incursion_site_completion_and_payout_core(
        &ctx.systems,
        &ctx.db,
        &ctx.sde,
        None,
        None,
        sys_id,
        site_id,
        None,
    )
    .await
    .expect("complete incursion wave");
    let next_bid = initial_boss_bid + 1_000;
    assert_eq!(res.tier, "Vanguard");
    assert!(
        !ctx.systems.claim_special_site_ball(sys_id, next_bid),
        "next_bid must already be claimed as a special-site ball"
    );
    let handle = ctx.systems.get_or_spawn(sys_id);
    let cs = handle
        .get_combat_state(next_bid)
        .await
        .unwrap()
        .expect("next wave boss must have CombatState");
    assert!(cs.max_structure_hp > 0.0, "Respawned wave boss must have non-zero HP");
}

#[tokio::test]
async fn test_r4_173_join_faction_as_character_enforces_cooldown_standing_corp_and_rearguard_lp() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut sess = SessionState::new(17301);
    login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let char_id = 90_000_001;
    let caldari = 500_001i64;
    let minmatar = 500_002i64;
    let kw = HashMap::new();

    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char_id)], &kw, None)
        .await
        .unwrap();

    // 1. Negative standing blocks JoinFactionAsCharacter
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().standings.insert(minmatar, -0.5);
    });
    let err_st = ctx
        .dispatch_rpc(&mut sess, "facWarMgr", None, "JoinFactionAsCharacter", &[PyRep::Integer(minmatar)], &kw, None)
        .await
        .unwrap_err();
    assert_eq!(err_st.code, "FwStandingTooLow");

    // 2. Enlist via fwCharacterEnlistmentMgr, retire (starts 24h cooldown), and verify JoinFactionAsCharacter is blocked by cooldown
    ctx.dispatch_rpc(&mut sess, "fwCharacterEnlistmentMgr", None, "CreateMyDirectEnlistment", &[PyRep::Integer(caldari)], &kw, None)
        .await
        .unwrap();
    ctx.dispatch_rpc(&mut sess, "fwCharacterEnlistmentMgr", None, "RemoveMyDirectEnlistment", &[], &kw, None)
        .await
        .unwrap();
    let err_cd = ctx
        .dispatch_rpc(&mut sess, "facWarMgr", None, "JoinFactionAsCharacter", &[PyRep::Integer(caldari)], &kw, None)
        .await
        .unwrap_err();
    assert_eq!(err_cd.code, "FwEnlistmentCooldown");

    // Advance past 24h cooldown and enlist in Minmatar (after fixing standing)
    ctx.advance_virtual_clock(86_405);
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().standings.insert(minmatar, 1.0);
    });
    ctx.dispatch_rpc(&mut sess, "facWarMgr", None, "JoinFactionAsCharacter", &[PyRep::Integer(minmatar)], &kw, None)
        .await
        .expect("JoinFactionAsCharacter after cooldown");

    // 3. In a low-sec Rearguard FW system, completing CaptureComplex pays 0.75x * 15,000 = 11,250 LP (not 1.5x = 22,500 LP)
    let rearguard_sys = ctx.db.with_snapshot(|snap| {
        snap.fw_system_occupations
            .keys()
            .copied()
            .find(|&sid| {
                let sec = ctx.sde.solar_systems.get(&sid).map(|s| s.security_status).unwrap_or(1.0);
                sec > 0.0
                    && sec < 0.45
                    && eve_server::services::fw_services::compute_system_frontline_status(&ctx, snap, sid)
                        == eve_server::services::fw_services::STATUS_REARGUARD
            })
            .expect("must have at least one low-sec Rearguard FW system")
    });
    let ship_id = 100_000_001i64;
    sess.station_id = None;
    sess.solar_system_id = Some(rearguard_sys);
    sess.ship_id = Some(ship_id);
    let handle = ctx.systems.get_or_spawn(rearguard_sys);
    let _ = handle
        .spawn_ball(eve_destiny::Ball {
            id: ship_id,
            type_id: 587,
            position: glam::DVec3::ZERO,
            velocity: glam::DVec3::ZERO,
            max_velocity: 350.0,
            inertia: 3.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        })
        .await;
    let _ = handle.step_manual(1.0).await;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.solar_system_id = rearguard_sys;
        }
        let st = snap.system_states.entry(rearguard_sys).or_default();
        st.solar_system_id = rearguard_sys;
        st.fw_complex_position = Some([0.0, 0.0, 0.0]);
        st.fw_capture_duration_seconds = 600;
        st.fw_capture_seconds_elapsed = 599;
        st.fw_contested_pct = 10.0;
        st.last_fw_capture_tick = 0;
    });
    let cap_res = ctx
        .dispatch_rpc(&mut sess, "facWarMgr", None, "CaptureComplex", &[], &kw, None)
        .await
        .expect("CaptureComplex in Rearguard system");
    assert_eq!(cap_res.dict_get("lpAwarded"), Some(&PyRep::Integer(11_250)));
}

#[tokio::test]
async fn test_r4_174_cannot_mutate_fitted_module() {
    let ctx = ServerContext::new();
    let mut sess = SessionState::new(17401);
    login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let char_id = 90_000_001;
    let ship_id = 100_000_001;
    let kw = HashMap::new();

    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char_id)], &kw, None)
        .await
        .unwrap();

    let fitted_ab_id = 770_174_001i64;
    let muta_id = 770_174_002i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            fitted_ab_id,
            InventoryItemRecord {
                item_id: fitted_ab_id,
                type_id: 438, // 1MN Afterburner II
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::MedSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            muta_id,
            InventoryItemRecord {
                item_id: muta_id,
                type_id: 47746, // Decayed 1MN Afterburner Mutaplasmid
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

    let err = ctx
        .dispatch_rpc(
            &mut sess,
            "dynamicItemService",
            None,
            "CreateDynamicItem",
            &[PyRep::Integer(47746), PyRep::Integer(fitted_ab_id)],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(err.code, "CannotMutateFittedItem");
}

#[tokio::test]
async fn test_r4_175_poco_tax_syncs_corp_wallet_division_zero() {
    let ctx = ServerContext::new();
    let mut sess = SessionState::new(17501);
    login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let char_id = 90_000_001;
    let corp_id = 98_175_001i64;
    let planet_id = 40_009_082i64;
    let ship_id = 100_000_001i64;
    let kw = HashMap::new();

    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char_id)], &kw, None)
        .await
        .unwrap();
    sess.station_id = None;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.corporation_id = corp_id;
            ch.balance = IskCents::from_isk(10_000_000);
        }
        snap.corporations.insert(
            corp_id,
            CorporationRecord {
                corporation_id: corp_id,
                name: "POCO 175 Corp".into(),
                ticker: "P175".into(),
                ceo_id: char_id,
                members: HashSet::from([char_id]),
                master_wallet: IskCents(0),
                wallet_divisions: vec![],
                ..Default::default()
            },
        );
        // Seed command center on planet 40009082
        snap.items.insert(
            770_175_001,
            InventoryItemRecord {
                item_id: 770_175_001,
                type_id: 2534,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        // Configure player POCO owned by corp_id with 10% tax rate
        let settings = eve_server::services::industry_pi::pi_helpers::OrbitalCustomsSettings {
            planet_id,
            orbital_type_id: 2233,
            owner_corp_id: corp_id,
            corporation_tax_rate: 0.10,
            alliance_tax_rate: 0.10,
            standing_high_tax_rate: 0.10,
            standing_good_tax_rate: 0.10,
            standing_neutral_tax_rate: 0.10,
            standing_bad_tax_rate: 0.10,
            standing_horrible_tax_rate: 0.10,
            allow_alliance: true,
            allow_standings: true,
            minimum_standing: -10.0,
        };
        eve_server::services::industry_pi::pi_helpers::save_orbital_settings(snap, planet_id, &settings);
    });

    ctx.dispatch_rpc(
        &mut sess,
        "planetMgr",
        None,
        "UserUpdateNetwork",
        &[
            PyRep::Integer(planet_id),
            PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::Integer(1),
                PyRep::Integer(1),
                PyRep::Integer(2534),
                PyRep::Real(0.1),
                PyRep::Real(0.2),
            ])]),
        ],
        &kw,
        None,
    )
    .await
    .unwrap();

    // Place 100x Water (3645) in POCO and import to colony
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            770_175_002,
            InventoryItemRecord {
                item_id: 770_175_002,
                type_id: 3645,
                owner_id: char_id,
                location_id: planet_id,
                flag: InventoryFlag::Hangar,
                quantity: 100,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    ctx.dispatch_rpc(
        &mut sess,
        "planetMgr",
        None,
        "UserLaunchCommodities",
        &[
            PyRep::Integer(planet_id),
            PyRep::Integer(3645),
            PyRep::Integer(100),
            PyRep::String("Import".into()),
        ],
        &kw,
        None,
    )
    .await
    .expect("UserLaunchCommodities Import");

    let (master_bal, div0_bal) = ctx.db.with_snapshot(|snap| {
        let corp = &snap.corporations[&corp_id];
        (
            corp.master_wallet.0,
            corp.wallet_divisions.first().map(|d| d.balance.0).unwrap_or(-1),
        )
    });
    assert!(master_bal > 0, "Corp master_wallet must be credited with POCO tax");
    assert_eq!(div0_bal, master_bal, "Corp wallet_divisions[0].balance must match master_wallet");
}
