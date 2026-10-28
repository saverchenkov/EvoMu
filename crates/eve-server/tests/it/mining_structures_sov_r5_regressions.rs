//! Round-5 Track 2 Forensic Audit Regression Tests (`#212`–`#224`):
//! Mining, Reprocessing, Compression, Upwell Structures, Moon Extractions, Sov, FW & Insurgencies.

use eve_db::{corp_roles, InventoryFlag, InventoryItemRecord, UpwellStructureRecord};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::HashMap;

async fn login_pilot(ctx: &ServerContext, session: &mut SessionState, user: &str, pass: &str) {
    let kw = HashMap::new();
    ctx.dispatch_rpc(
        session,
        "authentication",
        None,
        "Login",
        &[PyRep::String(user.into()), PyRep::String(pass.into())],
        &kw,
        None,
    )
    .await
    .expect("Login failed");
}

#[tokio::test]
async fn test_r5_212_222_corp_hangar_reprocess_compress_decompress_acl_and_ownership() {
    let ctx = ServerContext::new();
    let mut sess = SessionState::new(521201);
    login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let char_id = 90_000_001;
    let corp_id = 98_000_001;
    let station_id = 60_003_760;
    let struct_id = 1_020_212_001;
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

    // Demote char_id from CEO and strip division 1 take roles
    let corp_ore_id = 880_212_001;
    let corp_comp_gas_id = 880_212_002;
    let compressed_gas_tid = *ctx
        .sde
        .types
        .values()
        .find(|t| {
            t.published
                && t.name.starts_with("Compressed ")
                && t.default_attributes.get(&2760).copied().unwrap_or(0.0) > 0.0
                && ctx
                    .sde
                    .types
                    .get(&(t.default_attributes.get(&2760).copied().unwrap_or(0.0).round() as u32))
                    .is_some_and(|raw| raw.group_id == 711)
        })
        .map(|t| &t.type_id)
        .unwrap_or(&62377);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = corp_id;
        }
        if let Some(corp) = snap.corporations.get_mut(&corp_id) {
            corp.ceo_id = 90_000_002;
            corp.member_roles.insert(char_id, 0);
        }
        snap.structures.insert(
            struct_id,
            UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 35835,
                owner_corp_id: corp_id,
                solar_system_id: 30_000_142,
                name: "R5 Refinery".into(),
                state: "ShieldVulnerable".into(),
                position: [100_000.0, 0.0, 0.0],
                shield_hp: 1.0,
                armor_hp: 1.0,
                hull_hp: 1.0,
                fuel_blocks: 5_000,
                quantum_core_installed: true,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
        // Online reprocessing service module at struct_id
        snap.items.insert(
            880_212_000,
            InventoryItemRecord {
                item_id: 880_212_000,
                type_id: 35899,
                owner_id: corp_id,
                location_id: struct_id,
                flag: InventoryFlag::ServiceSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            corp_ore_id,
            InventoryItemRecord {
                item_id: corp_ore_id,
                type_id: 1230, // Veldspar
                owner_id: corp_id,
                location_id: station_id,
                flag: InventoryFlag::CorpSAG1,
                quantity: 1000,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            corp_comp_gas_id,
            InventoryItemRecord {
                item_id: corp_comp_gas_id,
                type_id: compressed_gas_tid,
                owner_id: corp_id,
                location_id: struct_id,
                flag: InventoryFlag::CorpSAG2,
                quantity: 100,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    // #212: Member without division take role cannot reprocess CorpSAG1 ore
    let rep_err = ctx
        .dispatch_rpc(
            &mut sess,
            "reprocessingSvc",
            None,
            "Reprocess",
            &[PyRep::Integer(corp_ore_id)],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(rep_err.code, "OwnershipOrLocationViolation");

    // Grant ROLE_HANGER_CAN_TAKE_1 (`1 << 13`) and verify reprocessed minerals stay in CorpSAG1 with owner_id == corp_id
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(corp) = snap.corporations.get_mut(&corp_id) {
            corp.member_roles.insert(char_id, 1u64 << 13);
        }
    });
    let rep_ok = ctx
        .dispatch_rpc(
            &mut sess,
            "reprocessingSvc",
            None,
            "Reprocess",
            &[PyRep::Integer(corp_ore_id)],
            &kw,
            None,
        )
        .await
        .expect("Authorized CorpSAG1 reprocess");
    let trit_id = rep_ok.dict_get("tritaniumItemID").and_then(|v| v.as_i64()).unwrap();
    let trit_item = ctx.db.get_item(trit_id).unwrap();
    assert_eq!(trit_item.owner_id, corp_id);
    assert_eq!(trit_item.flag, InventoryFlag::CorpSAG1);

    // #222: Dock at structure; member without division 2 take role cannot decompress CorpSAG2 gas
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.structure_id = Some(struct_id);
        }
    });
    let dec_err = ctx
        .dispatch_rpc(
            &mut sess,
            "structureCompressionMgr",
            None,
            "DecompressGasInStructure",
            &[PyRep::Integer(corp_comp_gas_id)],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(dec_err.code, "CrpAccessDenied");

    // Grant division 2 take role (`1 << 14`) and verify decompressed gas stays in CorpSAG2 with owner_id == corp_id
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(corp) = snap.corporations.get_mut(&corp_id) {
            corp.member_roles.insert(char_id, 1u64 << 14);
        }
    });
    ctx.dispatch_rpc(
        &mut sess,
        "structureCompressionMgr",
        None,
        "DecompressGasInStructure",
        &[PyRep::Integer(corp_comp_gas_id)],
        &kw,
        None,
    )
    .await
    .expect("Authorized CorpSAG2 gas decompression");
    let out_item = ctx.db.with_snapshot(|s| {
        s.items
            .values()
            .find(|i| i.location_id == struct_id && i.flag == InventoryFlag::CorpSAG2 && i.item_id != corp_comp_gas_id)
            .cloned()
            .expect("decompressed raw gas item in CorpSAG2")
    });
    assert_eq!(out_item.owner_id, corp_id);
    assert_eq!(out_item.flag, InventoryFlag::CorpSAG2);
}

#[tokio::test]
async fn test_r5_213_214_pi_orbital_interbus_spoofing_and_locked_import_protection() {
    let ctx = ServerContext::new();
    let mut sess = SessionState::new(521301);
    login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let char_id = 90_000_001;
    let corp_id = 98_000_001;
    let planet_id = 40_009_082;
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
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = corp_id;
        }
        if let Some(corp) = snap.corporations.get_mut(&corp_id) {
            corp.member_roles.insert(char_id, corp_roles::ROLE_DIRECTOR);
        }
        eve_server::services::industry_pi::save_orbital_settings(
            snap,
            planet_id,
            &eve_server::services::industry_pi::OrbitalCustomsSettings {
                planet_id,
                orbital_type_id: 2233,
                owner_corp_id: corp_id,
                corporation_tax_rate: 0.05,
                ..Default::default()
            },
        );
    });

    // #214: Non-GM player cannot spoof orbitalTypeID = 4318 (InterBus) in UpdateSettings
    let spoof_err = ctx
        .dispatch_rpc(
            &mut sess,
            "planetOrbitalRegistryBroker",
            None,
            "UpdateSettings",
            &[
                PyRep::Integer(planet_id),
                PyRep::Integer(12),
                PyRep::KeyVal(HashMap::from([("orbitalTypeID".into(), PyRep::Integer(4318))])),
            ],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(spoof_err.code, "InvalidOrbitalType");

    // RevertOrbitalsToInterBus sets 17% tax (not 0% tax)
    ctx.dispatch_rpc(
        &mut sess,
        "planetOrbitalRegistryBroker",
        None,
        "RevertOrbitalsToInterBus",
        &[PyRep::Integer(planet_id)],
        &kw,
        None,
    )
    .await
    .unwrap();
    let (rate, otid) = ctx.db.with_snapshot(|snap| {
        eve_server::services::industry_pi::resolve_character_orbital_access_and_tax(
            snap, char_id, planet_id,
        )
        .unwrap()
    });
    assert_eq!(otid, 4318);
    assert!((rate - 0.17).abs() < 1e-6, "InterBus POCO must charge 17% base tax, got {rate}");

    // #213: consume_pi_import_commodities must refuse contract-locked commodities
    let locked_pi_item = 880_213_001;
    let contract_id = 770_213_001;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.retain(|_, i| !(i.owner_id == char_id && i.type_id == 2389));
        snap.items.insert(
            locked_pi_item,
            InventoryItemRecord {
                item_id: locked_pi_item,
                type_id: 2389,
                owner_id: char_id,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 500,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.contracts.insert(
            contract_id,
            eve_db::ContractRecord {
                contract_id, issuer_id: char_id, issuer_corp_id: 98_000_001, assignee_id: 0, acceptor_id: None,
                contract_type: "item_exchange".into(), status: "Outstanding".into(), title: "Locked PI".into(),
                for_corp: false, availability: "public".into(), date_issued_unix: 1000, expiration_unix: 999999,
                num_days: 1, price: eve_db::IskCents(100), reward: eve_db::IskCents(0), collateral: eve_db::IskCents(0),
                buyout: eve_db::IskCents(0), current_bid: eve_db::IskCents(0), current_bidder_id: None,
                start_station_id: 60_003_760, end_station_id: 60_003_760, item_ids: vec![locked_pi_item],
                requested_items: Vec::new(), bid_history: Vec::new(),
            },
        );
    });
    let import_err = ctx.db.try_with_mut_snapshot(|snap| {
        eve_server::services::industry_pi::consume_pi_import_commodities(snap, char_id, planet_id, 2389, 100, None, Some(60_003_760), None)
    });
    assert!(import_err.is_err(), "Contract-locked PI items must not be consumed by PI import");
}

#[tokio::test]
async fn test_r5_215_acl_manager_cannot_demote_admin_or_leak_group_members() {
    let ctx = ServerContext::new();
    let mut s_admin = SessionState::new(521501);
    let mut s_mgr = SessionState::new(521502);
    let mut s_outsider = SessionState::new(521503);
    login_pilot(&ctx, &mut s_admin, "demo", "demo-secret").await;
    let admin_id = 90_000_001;
    let mgr_id = 90_000_002;
    let outsider_id = 90_000_003;
    let kw = HashMap::new();

    ctx.dispatch_rpc(&mut s_admin, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(admin_id)], &kw, None).await.unwrap();

    // Put mgr_id in a different corp so they aren't a corp director of admin_id's corp
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&admin_id) {
            ch.corporation_id = 98_000_001;
        }
        if let Some(ch) = snap.characters.get_mut(&mgr_id) {
            ch.corporation_id = 1_000_009;
        }
        let mut out_ch = snap.characters.get(&admin_id).unwrap().clone();
        out_ch.character_id = outsider_id;
        out_ch.user_id = 3;
        out_ch.name = "Outsider".into();
        out_ch.corporation_id = 1_000_009;
        snap.characters.insert(outsider_id, out_ch);
    });
    s_mgr.user_id = Some(2);
    s_mgr.character_id = Some(mgr_id);
    s_mgr.corporation_id = Some(1_000_009);
    s_mgr.corp_role = 0;
    s_outsider.user_id = Some(3);
    s_outsider.character_id = Some(outsider_id);
    s_outsider.corporation_id = Some(1_000_009);

    let grp_rep = ctx.dispatch_rpc(&mut s_admin, "ownerGroupManager", None, "Create", &[PyRep::String("R5 ACL".into()), PyRep::String("Desc".into())], &kw, None).await.unwrap();
    let grp_id = grp_rep.dict_get("groupID").and_then(|v| v.as_i64()).unwrap();

    // Admin grants Manager (role 2) to mgr_id via authentic V24 UpdateMembershipTypes
    ctx.dispatch_rpc(&mut s_admin, "ownerGroupManager", None, "UpdateMembershipTypes", &[PyRep::Integer(grp_id), PyRep::List(vec![PyRep::Integer(mgr_id)]), PyRep::Integer(2)], &kw, None).await.unwrap();

    // #215: Manager attempting to block/demote Admin (creator) via UpdateMembershipTypes is rejected
    let demote_err = ctx.dispatch_rpc(&mut s_mgr, "ownerGroupManager", None, "UpdateMembershipTypes", &[PyRep::Integer(grp_id), PyRep::List(vec![PyRep::Integer(admin_id)]), PyRep::Integer(-1)], &kw, None).await.unwrap_err();
    assert_eq!(demote_err.code, "InsufficientCorpRoles");

    // #215: Outsider cannot read GetMembers of private access group
    let leak_err = ctx.dispatch_rpc(&mut s_outsider, "ownerGroupManager", None, "GetMembers", &[PyRep::Integer(grp_id)], &kw, None).await.unwrap_err();
    assert_eq!(leak_err.code, "AccessGroupAccessDenied");
}

#[tokio::test]
async fn test_r5_217_ess_reserve_bank_pulse_cooldown_and_exhaustion() {
    let ctx = ServerContext::new();
    let mut sess = SessionState::new(521701);
    login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let char_id = 90_000_001;
    let ship_id = 100_000_001;
    let null_sys = 30_002_904u32; // VFK-IV (Deklein, NW quadrant)
    let ess_ball_id = 820_000_000 + null_sys as i64;
    let kw = HashMap::new();

    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char_id)], &kw, None).await.unwrap();

    let sys_handle = ctx.systems.get_or_spawn(null_sys);
    for (bid, tid) in [(ship_id, 587u32), (ess_ball_id, 32882u32)] {
        sys_handle.spawn_ball(eve_destiny::Ball {
            id: bid, type_id: tid, position: glam::DVec3::ZERO, velocity: glam::DVec3::ZERO,
            max_velocity: 300.0, inertia: 1.0, mass: 1_000_000.0, radius: 50.0,
            speed_fraction: 0.0, mode: eve_destiny::MovementMode::Stop,
        }).await.unwrap();
    }

    let (trit_id, wrong_key_id, key_id) = (880_217_000, 880_217_002, 880_217_001);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.structure_id = None;
            ch.solar_system_id = null_sys;
            ch.active_ship_id = ship_id;
        }
        let st = snap.system_states.entry(null_sys).or_default();
        st.solar_system_id = null_sys;
        st.ess_reserve_bank = eve_db::IskCents(30_000_000_00);
        for (iid, tid) in [(trit_id, 34u32), (wrong_key_id, 57716u32), (key_id, 57714u32)] {
            snap.items.insert(iid, InventoryItemRecord {
                item_id: iid, type_id: tid, owner_id: char_id, location_id: ship_id,
                flag: InventoryFlag::Cargo, quantity: 1, singleton: false, version: 1, ..Default::default()
            });
        }
    });
    sess.solar_system_id = Some(null_sys);
    sess.station_id = None;
    sess.ship_id = Some(ship_id);

    // R6-P0-02: Non-key cargo item (Tritanium 34) and wrong-quadrant key (NE 57716 in NW Deklein) are rejected without consumption
    assert_eq!(ctx.dispatch_rpc(&mut sess, "essMgr", None, "RequestUnlockReserveBank", &[PyRep::Integer(34)], &kw, None).await.unwrap_err().code, "MissingEssReserveSecurityKey");
    assert_eq!(ctx.dispatch_rpc(&mut sess, "essMgr", None, "RequestUnlockReserveBank", &[PyRep::Integer(trit_id)], &kw, None).await.unwrap_err().code, "MissingEssReserveSecurityKey");
    assert_eq!(ctx.dispatch_rpc(&mut sess, "essMgr", None, "RequestUnlockReserveBank", &[PyRep::Integer(wrong_key_id)], &kw, None).await.unwrap_err().code, "EssReserveKeyWrongQuadrant");
    assert_eq!(ctx.db.with_snapshot(|s| s.items.get(&trit_id).map(|i| i.quantity)), Some(1));
    assert_eq!(ctx.db.with_snapshot(|s| s.items.get(&wrong_key_id).map(|i| i.quantity)), Some(1));
    let pre_ess = ctx.dispatch_rpc(&mut sess, "essMgr", None, "GetDataForClientSolarSystem", &[], &kw, None).await.unwrap();
    assert_eq!(pre_ess.dict_get("reserveBankPulsesTotal").and_then(|v| v.as_i64()), Some(0));

    // Unlock Reserve Bank via RequestUnlockReserveBank (15 pulses for NW 15-min key 57714)
    ctx.dispatch_rpc(&mut sess, "essMgr", None, "RequestUnlockReserveBank", &[PyRep::Integer(key_id)], &kw, None).await.expect("RequestUnlockReserveBank");

    // First pulse succeeds immediately after 5 ticks
    ctx.dispatch_rpc(&mut sess, "essMgr", None, "AttemptLinkToReserveBank", &[PyRep::Integer(ess_ball_id)], &kw, None).await.unwrap();
    for _ in 0..5 {
        let _ = sys_handle.step_manual(1.0).await.unwrap();
    }
    ctx.dispatch_rpc(&mut sess, "essMgr", None, "AttemptLinkToReserveBank", &[PyRep::Integer(ess_ball_id)], &kw, None).await.expect("First pulse payout");
    let ess_data = ctx.dispatch_rpc(&mut sess, "essMgr", None, "GetDataForClientSolarSystem", &[], &kw, None).await.unwrap();
    assert_eq!(ess_data.dict_get("reserveBankPulsesTotal").and_then(|v| v.as_i64()), Some(15));
    assert_eq!(ess_data.dict_get("reserveBankPulsesRemaining").and_then(|v| v.as_i64()), Some(14));

    // Immediate second link before 60s cooldown is rejected with EssReservePulseCooldown
    let cd_err = ctx.dispatch_rpc(&mut sess, "essMgr", None, "AttemptLinkToReserveBank", &[PyRep::Integer(ess_ball_id)], &kw, None).await.unwrap_err();
    assert_eq!(cd_err.code, "EssReservePulseCooldown");
}

#[tokio::test]
async fn test_r5_218_219_220_221_223_224_mining_moon_drill_asset_safety_fw_insurgency() {
    let ctx = ServerContext::new();
    let mut sess = SessionState::new(521801);
    login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let char_id = 90_000_001;
    let corp_id = 98_000_001;
    let sys_id = 30_000_142;
    let refinery_id = 1_020_219_001;
    let drill_mod_id = 880_219_001;
    let kw = HashMap::new();

    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char_id)], &kw, None).await.unwrap();

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = corp_id;
            ch.station_id = None;
            ch.structure_id = Some(refinery_id);
        }
        if let Some(corp) = snap.corporations.get_mut(&corp_id) {
            corp.member_roles.insert(char_id, corp_roles::ROLE_DIRECTOR | corp_roles::ROLE_STATION_MANAGER);
        }
        snap.structures.insert(
            refinery_id,
            UpwellStructureRecord {
                structure_id: refinery_id,
                type_id: 35835,
                owner_corp_id: corp_id,
                solar_system_id: sys_id,
                name: "R5 Athanor".into(),
                state: "ShieldVulnerable".into(),
                position: [150_000.0, 0.0, 0.0],
                shield_hp: 10_000_000.0,
                armor_hp: 10_000_000.0,
                hull_hp: 10_000_000.0,
                fuel_blocks: 5_000,
                quantum_core_installed: true,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
        // Seed Standup Moon Drill I (35894) in ServiceSlot0 but OFFLINE
        snap.items.insert(
            drill_mod_id,
            InventoryItemRecord {
                item_id: drill_mod_id,
                type_id: 35894,
                owner_id: corp_id,
                location_id: refinery_id,
                flag: InventoryFlag::ServiceSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(drill_mod_id, false);
    });

    // #219: StartNewExtraction fails when Standup Moon Drill I is offline
    let drill_err = ctx
        .dispatch_rpc(
            &mut sess,
            "moonExtractions",
            None,
            "StartNewExtraction",
            &[PyRep::Integer(refinery_id), PyRep::Integer(518_400)],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(drill_err.code, "StructureServiceOffline");

    // #220: Online the service module, then offline it -> shield_hp is clamped to LowPower max
    eve_server::services::station_structure::structure_lifecycle_rpc::handle_structure_module_online(
        &ctx,
        &mut sess,
        refinery_id,
        drill_mod_id,
        true,
    )
    .unwrap();
    let full_shield = ctx.db.with_snapshot(|s| s.structures.get(&refinery_id).unwrap().shield_hp);
    eve_server::services::station_structure::structure_lifecycle_rpc::handle_structure_module_online(
        &ctx,
        &mut sess,
        refinery_id,
        drill_mod_id,
        false,
    )
    .unwrap();
    let low_shield = ctx.db.with_snapshot(|s| s.structures.get(&refinery_id).unwrap().shield_hp);
    assert!(low_shield < full_shield, "Offlining last service module must clamp shield_hp to LowPower max");

    // #218: Observer ledger records moon ore mining
    eve_server::services::space_mining_harvest::deposit_mined_ore_and_record_ledger_with_waste(
        &ctx,
        char_id,
        100_000_001,
        sys_id,
        InventoryFlag::Cargo,
        45492, // Ubiquitous Moon Ore
        100,
        10,
    );
    let obs_entries = ctx.db.with_snapshot(|s| s.observer_mining_ledger.get(&refinery_id).cloned().unwrap_or_default());
    assert_eq!(obs_entries.len(), 1);
    assert_eq!(obs_entries[0].quantity, 100);

    // #224: Simulated clock governs stronghold invulnerability
    crate::common::pin_clock(&ctx);
    let now_u = ctx.db.clock().now_unix_secs();
    ctx.db.with_mut_snapshot(|snap| {
        snap.insurgency_runtime.campaigns.insert(
            1,
            eve_db::PersistedInsurgencyCampaign {
                campaign_id: 1,
                pirate_faction_id: 500_010,
                target_faction_id: 500_001,
                fob_solar_system_id: sys_id,
                systems: vec![sys_id],
                created_at_unix: now_u,
                forecasting_end_unix: now_u + 1_000,
                active_end_unix: now_u + 50_000,
                winddown_end_unix: now_u + 60_000,
                corruption_points: HashMap::new(),
                suppression_points: HashMap::new(),
            },
        );
    });
    assert!(
        eve_server::services::insurgency_services::is_insurgency_stronghold_invulnerable(&ctx, sys_id, 1_898_000_011),
        "Stronghold must be invulnerable during Forecasting under simulated clock"
    );
    ctx.db.clock().advance_secs(2_000);
    assert!(
        !eve_server::services::insurgency_services::is_insurgency_stronghold_invulnerable(&ctx, sys_id, 1_898_000_011),
        "Stronghold must become vulnerable once simulated clock enters Active phase"
    );

    // R6-P1-01: FW complex tick rejects pod/cloak, preserves tick across off-grid ally, pauses contested, and de-plexes for defender
    let fw_sys = 30_002_537u32; // Amamake (lowsec)
    let (ally_cid, def_cid) = (90_000_221i64, 90_000_222i64);
    let (atk_ship, ally_ship, def_ship) = (880_221_001i64, 880_221_002i64, 880_221_003i64);
    let uid2 = ctx.db.provision_player_account("fw_ally2", "secret2");
    let uid3 = ctx.db.provision_player_account("fw_def3", "secret3");
    ctx.db.with_mut_snapshot(|snap| {
        for (cid, uid, nm) in [(ally_cid, uid2, "FW Ally"), (def_cid, uid3, "FW Defender")] {
            let mut c = snap.characters[&char_id].clone();
            c.character_id = cid; c.user_id = uid; c.name = nm.into();
            snap.characters.insert(cid, c);
        }
    });
    let (mut s_ally, mut s_def) = (SessionState::new(521812), SessionState::new(521813));
    login_pilot(&ctx, &mut s_ally, "fw_ally2", "secret2").await;
    login_pilot(&ctx, &mut s_def, "fw_def3", "secret3").await;
    ctx.dispatch_rpc(&mut s_ally, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(ally_cid)], &kw, None).await.unwrap();
    ctx.dispatch_rpc(&mut s_def, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(def_cid)], &kw, None).await.unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        for (cid, sid, fid) in [(char_id, atk_ship, 500_002i64), (ally_cid, ally_ship, 500_002), (def_cid, def_ship, 500_003)] {
            if let Some(c) = snap.characters.get_mut(&cid) {
                c.solar_system_id = fw_sys; c.station_id = None; c.structure_id = None; c.active_ship_id = sid; c.war_faction_id = Some(fid);
            }
        }
        snap.fw_system_occupations.insert(fw_sys, eve_db::FwSystemOccupationRecord {
            solar_system_id: fw_sys, owner_faction_id: 500_002, occupier_faction_id: 500_003, victory_points: 0, victory_points_threshold: 3_000,
        });
        let st = snap.system_states.entry(fw_sys).or_default();
        st.solar_system_id = fw_sys; st.fw_complex_position = Some([0.0, 0.0, 0.0]);
        st.fw_capture_duration_seconds = 600; st.fw_capture_seconds_elapsed = 300; st.fw_contested_pct = 50.0; st.last_fw_capture_tick = 0;
    });
    let fw_h = ctx.systems.get_or_spawn(fw_sys);
    let mk_ball = |id: i64, tid: u32, x: f64| eve_destiny::Ball {
        id, type_id: tid, position: glam::DVec3::new(x, 0.0, 0.0), velocity: glam::DVec3::ZERO,
        max_velocity: 300.0, inertia: 1.0, mass: 1_000_000.0, radius: 40.0, speed_fraction: 0.0, mode: eve_destiny::MovementMode::Stop,
    };
    let mut cloaked_cs = ctx.build_type_combat_state(587);
    cloaked_cs.is_cloaked = true; cloaked_cs.gate_cloak_remaining_ticks = 30;
    fw_h.spawn_ball(mk_ball(atk_ship, 670, 0.0)).await.unwrap(); // Capsule cannot capture
    fw_h.spawn_ball(mk_ball(ally_ship, 587, 80_000.0)).await.unwrap(); // Off-grid ally (>30km)
    fw_h.spawn_ball_with_combat(mk_ball(def_ship, 587, 5_000.0), cloaked_cs.clone()).await.unwrap(); // Cloaked defender
    let _ = fw_h.step_manual(1.0).await.unwrap();
    eve_server::services::insurgency_services::step_system_insurgency_tick(&ctx, fw_sys).await;
    assert_eq!(ctx.db.with_snapshot(|s| s.system_states[&fw_sys].fw_capture_seconds_elapsed), 300, "Pod + cloaked ship must not advance or de-plex");

    fw_h.remove_ball(atk_ship).await;
    fw_h.spawn_ball(mk_ball(atk_ship, 587, 0.0)).await.unwrap(); // Uncloaked Minmatar attacker + off-grid ally
    let _ = fw_h.step_manual(1.0).await.unwrap();
    eve_server::services::insurgency_services::step_system_insurgency_tick(&ctx, fw_sys).await;
    ctx.db.with_snapshot(|s| {
        assert!(s.system_states[&fw_sys].fw_capture_seconds_elapsed > 300, "Attacker advances complex");
        assert!(s.system_states[&fw_sys].last_fw_capture_tick > 0, "Off-grid ally must not clobber last_fw_capture_tick to 0");
    });

    let elapsed_before_contest = ctx.db.with_snapshot(|s| s.system_states[&fw_sys].fw_capture_seconds_elapsed);
    fw_h.remove_ball(def_ship).await;
    fw_h.spawn_ball(mk_ball(def_ship, 587, 5_000.0)).await.unwrap(); // Uncloaked Amarr defender joins grid -> contested!
    let _ = fw_h.step_manual(1.0).await.unwrap();
    eve_server::services::insurgency_services::step_system_insurgency_tick(&ctx, fw_sys).await;
    assert_eq!(ctx.db.with_snapshot(|s| s.system_states[&fw_sys].fw_capture_seconds_elapsed), elapsed_before_contest, "Contested grid must pause complex");

    let _ = fw_h.set_ball_position(atk_ship, glam::DVec3::new(80_000.0, 0.0, 0.0)).await; // Attacker leaves grid -> defender de-plexes!
    let _ = fw_h.step_manual(1.0).await.unwrap();
    eve_server::services::insurgency_services::step_system_insurgency_tick(&ctx, fw_sys).await;
    assert!(ctx.db.with_snapshot(|s| s.system_states[&fw_sys].fw_capture_seconds_elapsed) < elapsed_before_contest, "Defender must de-plex toward 0%");
}
