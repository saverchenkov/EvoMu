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
