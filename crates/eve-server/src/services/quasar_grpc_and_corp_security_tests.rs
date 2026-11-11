//! Semantic unit tests split from `r6_t4_p0_security_tests`.

#![forbid(unsafe_code)]

use super::r6_t4_p0_security_tests::*;

use crate::esi::economy_routes::dispatch_economy_route;
use crate::esi::sample_routes::RouteExecutionResult;
use crate::quasar_gateway::{
    STATUS_FORBIDDEN, STATUS_UNAUTHORIZED,
};
use crate::router::ServerContext;
use eve_db::{
    ContractRecord, CorporationRecord, InventoryFlag,
    InventoryItemRecord, IskCents, MarketOrderMeta, MarketOrderRecord, OwnerNoteRecord,
    UpwellStructureRecord,
};
use eve_proto::quasar::{
    AuthoritativeContext, WireMessage,
};
use eve_proto::PyRep;
use serde_json::Value;
use std::collections::HashMap;

#[tokio::test]
async fn r6_t4_02_certificate_activation_enforces_locks_escrow_trades_location_and_replay() {
    let ctx = ServerContext::new();
    let uid = ctx.db.provision_player_account("r6_t4_mct_user", "pass_m");
    let char_id = 91_000_201i64;
    let corp_id = 98_000_201i64;
    let jita = 60_003_760i64;
    let amarr = 60_008_494i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters[&90_000_001].clone();
        ch.character_id = char_id;
        ch.name = "CertTester".into();
        ch.user_id = uid;
        ch.corporation_id = corp_id;
        ch.station_id = Some(jita);
        ch.solar_system_id = 30_000_142;
        snap.characters.insert(char_id, ch);

        for rec in [
            cert_item(910_001, 33027, char_id, jita, InventoryFlag::Locked),
            cert_item(910_002, 48582, char_id, jita, InventoryFlag::Hangar),
            cert_item(910_003, 28662, char_id, jita, InventoryFlag::Hangar),
            cert_item(910_004, 34189, char_id, jita, InventoryFlag::Hangar),
            cert_item(910_005, 33027, char_id, amarr, InventoryFlag::Hangar),
            cert_item(910_006, 28662, char_id, 910_999, InventoryFlag::Unlocked),
            cert_item(910_007, 33027, corp_id, jita, InventoryFlag::Hangar),
        ] {
            snap.items.insert(rec.item_id, rec);
        }
        snap.contracts.insert(
            910_100,
            ContractRecord {
                contract_id: 910_100,
                issuer_id: char_id,
                issuer_corp_id: corp_id,
                assignee_id: 0,
                acceptor_id: None,
                contract_type: "ItemExchange".into(),
                availability: "Public".into(),
                status: "Outstanding".into(),
                item_ids: vec![910_002, 910_003],
                expiration_unix: ctx.now_unix() + 86_400,
                ..Default::default()
            },
        );
        snap.owner_notes.insert(
            -7_300_000_000_999,
            OwnerNoteRecord {
                note_id: -7_300_000_000_999,
                character_id: -73,
                item_id: 999,
                label: "trade:999".into(),
                note: serde_json::json!({
                    "trade_id": 999,
                    "station_id": jita,
                    "char_a": char_id,
                    "char_b": 90_000_002,
                    "items_a": [[910_004, 1]],
                    "items_b": [],
                    "money_a_cents": 0,
                    "money_b_cents": 0,
                    "accept_a": false,
                    "accept_b": false,
                    "completed": false,
                    "cancelled": false
                })
                .to_string(),
                timestamp_unix: ctx.now_unix(),
            },
        );
    });

    let empty_kw = HashMap::new();
    let mut sess = rpc_session(uid, char_id, corp_id, Some(jita));

    for method in [
        "ActivateMultiTraining",
        "ActivateSoulboundMultiTraining",
        "ActivateCharacterReSculpt",
    ] {
        let err = ctx
            .dispatch_rpc(&mut sess, "userSvc", None, method, &[], &empty_kw, None)
            .await
            .unwrap_err();
        assert_eq!(err.code, "UserError", "{method} must reject non-consumable certificates");
    }
    ctx.db.with_snapshot(|snap| {
        for iid in 910_001..=910_007 {
            assert!(snap.items.contains_key(&iid), "Locked item {iid} must remain intact");
        }
    });

    // Unlocked certificates in Jita hangar must still be rejected while undocked in space!
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(910_008, cert_item(910_008, 33027, char_id, jita, InventoryFlag::Hangar));
        snap.items.insert(910_009, cert_item(910_009, 33502, char_id, jita, InventoryFlag::Hangar));
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
        }
    });
    let mut space_sess = rpc_session(uid, char_id, corp_id, None);
    for method in ["ActivateMultiTraining", "ActivateCharacterReSculpt"] {
        assert!(
            ctx.dispatch_rpc(&mut space_sess, "userSvc", None, method, &[], &empty_kw, None)
                .await
                .is_err(),
            "{method} must reject hangar certificate when character is undocked in space"
        );
    }

    // Dock back at Jita: each single certificate succeeds ONCE and replay immediately fails
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = Some(jita);
        }
    });
    assert!(ctx.dispatch_rpc(&mut sess, "userSvc", None, "ActivateMultiTraining", &[], &empty_kw, None).await.is_ok());
    assert!(ctx.dispatch_rpc(&mut sess, "userSvc", None, "ActivateMultiTraining", &[], &empty_kw, None).await.is_err());
    assert!(ctx.dispatch_rpc(&mut sess, "userSvc", None, "ActivateCharacterReSculpt", &[], &empty_kw, None).await.is_ok());
    assert!(ctx.dispatch_rpc(&mut sess, "userSvc", None, "ActivateCharacterReSculpt", &[], &empty_kw, None).await.is_err());
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&910_008));
        assert!(!snap.items.contains_key(&910_009));
    });
}

#[tokio::test]
async fn r6_t4_03_quasar_market_acl_redaction_and_esi_public_assigned_contracts() {
    let ctx = ServerContext::new();
    let owner_corp = 98_000_301i64;
    let outsider_corp = 98_000_302i64;
    let member_cid = 91_000_301i64;
    let outsider_cid = 91_000_302i64;
    let struct_id = 1_040_000_000_301i64;
    let jita = 60_003_760i64;
    let now_u = ctx.now_unix();

    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.insert(
            owner_corp,
            { let mut __corp = CorporationRecord::default();
                __corp.corporation_id = owner_corp;
                __corp.name = "OwnerCorp".into();
                __corp.ticker = "OWNR".into();
                __corp.ceo_id = member_cid;
                __corp },
        );
        for (cid, corp) in [(member_cid, owner_corp), (outsider_cid, outsider_corp)] {
            let mut ch = snap.characters[&90_000_001].clone();
            ch.character_id = cid;
            ch.corporation_id = corp;
            ch.user_id = cid as u64;
            snap.characters.insert(cid, ch);
        }
        snap.structures.insert(
            struct_id,
            UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 35833,
                owner_corp_id: owner_corp,
                solar_system_id: 30_000_142,
                name: "Private Market Fortizar".into(),
                position: [0.0; 3],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 500,
                shield_hp: 100.0,
                armor_hp: 100.0,
                hull_hp: 100.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
        snap.items.insert(
            910_350,
            cert_item(910_350, 35_892, owner_corp, struct_id, InventoryFlag::ServiceSlot0),
        );
        snap.module_online_states.insert(910_350, true);
        snap.market_orders.insert(
            88_001,
            MarketOrderRecord {
                order_id: 88_001,
                owner_id: member_cid,
                station_id: struct_id,
                region_id: 10_000_002,
                type_id: 34,
                is_bid: true,
                price: IskCents::from_isk(1_000),
                vol_entered: 500,
                vol_remaining: 500,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents::from_isk(500_000),
            },
        );
        snap.market_order_meta.insert(
            88_001,
            MarketOrderMeta {
                issued_unix: 1_765_432_100,
                duration_days: 14,
                range: "station".into(),
                issued_by: Some(member_cid),
                wallet_division: Some(1),
                is_corporation: false,
                state: Some("Open".into()),
            },
        );
        for (cid, assignee, avail) in [
            (92_001, 0, "Public"),
            (92_002, outsider_cid, "Public"),
            (92_003, outsider_cid, "Personal"),
        ] {
            snap.contracts.insert(
                cid,
                ContractRecord {
                    contract_id: cid,
                    issuer_id: member_cid,
                    issuer_corp_id: owner_corp,
                    assignee_id: assignee,
                    acceptor_id: None,
                    contract_type: "Auction".into(),
                    availability: avail.into(),
                    status: "Outstanding".into(),
                    title: format!("Contract {cid}"),
                    price: IskCents::from_isk(10_000),
                    start_station_id: jita,
                    end_station_id: jita,
                    date_issued_unix: now_u,
                    num_days: 7,
                    expiration_unix: now_u + 86_400,
                    item_ids: vec![910_350],
                    bid_history: vec![(outsider_cid, IskCents::from_isk(12_000), now_u)],
                    ..Default::default()
                },
            );
        }
    });

    // Part A: Quasar Structure Market ACL & Order Field Redaction
    let req_any = encode_id_req("eve.market.orders.GetOpenInStructureRequest", struct_id as u64);
    let anon_ctx = AuthoritativeContext::default();
    let outsider_ctx = make_auth_ctx(outsider_cid as u64, outsider_cid, outsider_corp, now_u);
    let member_ctx = make_auth_ctx(member_cid as u64, member_cid, owner_corp, now_u);

    assert_eq!(ctx.quasar_gateway.dispatch(&anon_ctx, &req_any).unwrap_err().0, STATUS_UNAUTHORIZED);
    assert_eq!(ctx.quasar_gateway.dispatch(&outsider_ctx, &req_any).unwrap_err().0, STATUS_FORBIDDEN);
    let ok_struct = ctx.quasar_gateway.dispatch(&member_ctx, &req_any).unwrap();
    let pub_wm = WireMessage::decode(&ok_struct.value).unwrap();
    let comp_wm = WireMessage::decode(pub_wm.fields[0].value.as_bytes().unwrap()).unwrap();
    let buy_entry_wm = WireMessage::decode(comp_wm.fields[0].value.as_bytes().unwrap()).unwrap();
    let pub_attrs_bytes = buy_entry_wm.fields.iter().find(|f| f.field_number == 2).unwrap().value.as_bytes().unwrap();
    let pub_attrs = WireMessage::decode(pub_attrs_bytes).unwrap();
    assert!(
        pub_attrs.fields.iter().all(|f| f.field_number != 1 && f.field_number != 15),
        "Public market order response must omit issuer_character (1) and escrow (15)"
    );
    assert_eq!(
        pub_attrs.fields.iter().find(|f| f.field_number == 6).and_then(|f| f.value.as_varint()),
        Some(14)
    );

    // Private GetOpenForCharacterRequest retains field 1 (issuer_character) & field 15 (escrow)
    let char_ord_any = encode_id_req("eve.market.orders.GetOpenForCharacterRequest", member_cid as u64);
    let priv_rsp = ctx.quasar_gateway.dispatch(&member_ctx, &char_ord_any).unwrap();
    let priv_comp = WireMessage::decode(WireMessage::decode(&priv_rsp.value).unwrap().fields[0].value.as_bytes().unwrap()).unwrap();
    let priv_buy = WireMessage::decode(priv_comp.fields[0].value.as_bytes().unwrap()).unwrap();
    let priv_attrs = WireMessage::decode(priv_buy.fields.iter().find(|f| f.field_number == 2).unwrap().value.as_bytes().unwrap()).unwrap();
    assert!(priv_attrs.fields.iter().any(|f| f.field_number == 1));
    assert!(priv_attrs.fields.iter().any(|f| f.field_number == 15));

    // Part B: ESI /contracts/public/* hides assignee_id != 0 and non-Public contracts, items, and bids
    let empty_q = HashMap::new();
    let reg_params = HashMap::from([("region_id".into(), "10000002".into())]);
    let pub_list: RouteExecutionResult =
        dispatch_economy_route(&ctx, "GET", "/contracts/public/{region_id}", &reg_params, &empty_q, None, 1).unwrap();
    assert_eq!(pub_list.status_code, 200);
    let ids: Vec<i64> = pub_list
        .body
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|row| row.get("contract_id").and_then(Value::as_i64))
        .collect();
    assert!(ids.contains(&92_001));
    assert!(!ids.contains(&92_002));
    assert!(!ids.contains(&92_003));

    for (path, cid, expected) in [
        ("/contracts/public/items/{contract_id}", "92001", 200),
        ("/contracts/public/items/{contract_id}", "92002", 404),
        ("/contracts/public/items/{contract_id}", "92003", 404),
        ("/contracts/public/bids/{contract_id}", "92001", 200),
        ("/contracts/public/bids/{contract_id}", "92002", 404),
        ("/contracts/public/bids/{contract_id}", "92003", 404),
    ] {
        let p = HashMap::from([("contract_id".into(), cid.into())]);
        let r = dispatch_economy_route(&ctx, "GET", path, &p, &empty_q, None, 1).unwrap();
        assert_eq!(r.status_code, expected, "{path} for {cid}");
    }

    // Part C: CreateContract with assigneeID > 0 normalizes default availability to "Personal"
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&member_cid) {
            ch.balance = IskCents::from_isk(1_000_000);
        }
        snap.items.insert(
            910_351,
            InventoryItemRecord {
                type_id: 34,
                quantity: 10,
                ..cert_item(910_351, 34, member_cid, jita, InventoryFlag::Hangar)
            },
        );
    });
    let mut issuer_sess = rpc_session(member_cid as u64, member_cid, owner_corp, Some(jita));
    let contract_payload = PyRep::keyval([
        ("contractType", PyRep::Integer(3)),
        ("assigneeID", PyRep::Integer(outsider_cid)),
        ("expireTime", PyRep::Integer(1440)),
        ("numDays", PyRep::Integer(3)),
        ("startStationID", PyRep::Integer(jita)),
        ("endStationID", PyRep::Integer(jita)),
        ("reward", PyRep::Real(5_000.0)),
        ("collateral", PyRep::Real(1_000.0)),
        ("items", PyRep::List(vec![PyRep::Integer(910_351)])),
    ]);
    let created_rep = ctx
        .dispatch_rpc(
            &mut issuer_sess,
            "contractProxy",
            None,
            "CreateContract",
            &[contract_payload],
            &HashMap::new(),
            None,
        )
        .await
        .unwrap();
    let created_cid = created_rep
        .as_i64()
        .or_else(|| created_rep.dict_get("contractID").and_then(|v| v.as_i64()))
        .expect("created contractID");
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.contracts[&created_cid].assignee_id, outsider_cid);
        assert_eq!(snap.contracts[&created_cid].availability, "Personal");
    });
}

#[test]
fn domain_market_acl_structure_access_market_visibility_and_industry_activity() {
    use crate::domain::market_acl::{
        can_caller_view_market_location, can_caller_view_market_location_at,
        can_caller_view_structure_acl, character_has_structure_acl_access,
        primary_online_industry_activity, verify_structure_market_query_access,
    };
    use eve_db::{WarRecord, WorldStateSnapshot};

    let ctx = ServerContext::new();
    let owner_corp = 1_500_001_i64; // < 98_000_000 so public unless access profile denies
    let ally_corp = 98_500_002_i64;
    let neutral_corp = 98_500_003_i64;
    let enemy_corp = 98_500_004_i64;
    let alliance_id = 99_500_001_i64;
    let other_alliance_id = 99_500_002_i64;

    let owner_member = 91_500_001_i64;
    let ally_member = 91_500_002_i64;
    let neutral_member = 91_500_003_i64;
    let hostile_standing_member = 91_500_004_i64;
    let zero_standing_member = 91_500_005_i64;
    let war_aggressor_member = 91_500_006_i64;
    let war_defender_member = 91_500_007_i64;
    let no_ally_negative_member = 91_500_008_i64;

    let struct_id = 1_066_500_001_i64;
    let mkt_mod_id = 1_066_500_101_i64;
    let ind_mod_id = 1_066_500_102_i64;
    let npc_station = 60_003_760_i64;
    let now = ctx.now_unix();

    let base_ch = ctx
        .db
        .with_snapshot(|s| s.characters.values().next().cloned().expect("seed char"));
    let mk_char = |cid: i64, corp_id: i64, ally: Option<i64>, standing: Option<f64>| {
        let mut ch = base_ch.clone();
        ch.standings.clear();
        ch.character_id = cid;
        ch.name = format!("Char{cid}");
        ch.corporation_id = corp_id;
        ch.alliance_id = ally;
        if let Some(st_val) = standing {
            ch.standings.insert(owner_corp, st_val);
        }
        ch
    };

    let mut snap = WorldStateSnapshot::default();
    snap.corporations.insert(owner_corp, {
        let mut c = CorporationRecord::default();
        c.corporation_id = owner_corp;
        c.name = "OwnerCorp".into();
        c.ticker = "OWN".into();
        c.alliance_id = Some(alliance_id);
        c
    });
    snap.characters
        .insert(owner_member, mk_char(owner_member, owner_corp, Some(alliance_id), Some(-5.0)));
    snap.characters
        .insert(ally_member, mk_char(ally_member, ally_corp, Some(alliance_id), Some(-5.0)));
    snap.characters
        .insert(neutral_member, mk_char(neutral_member, neutral_corp, Some(other_alliance_id), Some(0.1)));
    snap.characters.insert(
        hostile_standing_member,
        mk_char(hostile_standing_member, neutral_corp, None, Some(-0.1)),
    );
    snap.characters.insert(
        zero_standing_member,
        mk_char(zero_standing_member, neutral_corp, None, Some(0.0)),
    );
    snap.characters.insert(
        war_aggressor_member,
        mk_char(war_aggressor_member, enemy_corp, None, Some(5.0)),
    );
    snap.characters.insert(
        war_defender_member,
        mk_char(war_defender_member, 98_500_005, None, Some(5.0)),
    );
    snap.characters.insert(
        no_ally_negative_member,
        mk_char(no_ally_negative_member, neutral_corp, None, Some(-0.5)),
    );

    snap.wars.insert(
        700_001,
        WarRecord {
            war_id: 700_001,
            aggressor_corp_id: enemy_corp,
            defender_corp_id: owner_corp,
            mutual: false,
            started_unix: now.saturating_sub(100),
            active: true,
        },
    );
    snap.wars.insert(
        700_002,
        WarRecord {
            war_id: 700_002,
            aggressor_corp_id: owner_corp,
            defender_corp_id: 98_500_005,
            mutual: false,
            started_unix: now.saturating_sub(100),
            active: true,
        },
    );
    snap.wars.insert(
        700_003,
        WarRecord {
            war_id: 700_003,
            aggressor_corp_id: neutral_corp,
            defender_corp_id: owner_corp,
            mutual: false,
            started_unix: now.saturating_sub(500),
            active: false, // inactive war must not block
        },
    );

    let st = UpwellStructureRecord {
        structure_id: struct_id,
        type_id: 35_826, // Fortizar (Large, allows market orders)
        owner_corp_id: owner_corp,
        solar_system_id: 30_000_144,
        name: "Market Fortizar".into(),
        position: [0.0; 3],
        state: "ShieldVulnerable".into(),
        quantum_core_installed: true,
        fuel_blocks: 100,
        shield_hp: 100_000.0,
        armor_hp: 100_000.0,
        hull_hp: 100_000.0,
        reinforced_until_unix: 0,
        last_damage_tick: 0,
        last_damage_unix: 0,
        self_destruct_at_unix: None,
    };
    snap.structures.insert(struct_id, st.clone());

    // 1. `character_has_structure_acl_access`
    assert!(character_has_structure_acl_access(&snap, owner_member, &st));
    assert!(character_has_structure_acl_access(&snap, ally_member, &st));
    assert!(character_has_structure_acl_access(&snap, neutral_member, &st));
    assert!(character_has_structure_acl_access(&snap, zero_standing_member, &st));
    assert!(!character_has_structure_acl_access(&snap, hostile_standing_member, &st));
    assert!(!character_has_structure_acl_access(&snap, war_aggressor_member, &st));
    assert!(!character_has_structure_acl_access(&snap, war_defender_member, &st));
    assert!(!character_has_structure_acl_access(&snap, 999_888_777, &st)); // unknown char

    // When both owner_corp and char have alliance_id == None, None == None must NOT grant alliance bypass.
    snap.corporations.get_mut(&owner_corp).unwrap().alliance_id = None;
    assert!(!character_has_structure_acl_access(&snap, no_ally_negative_member, &st));
    snap.corporations.get_mut(&owner_corp).unwrap().alliance_id = Some(alliance_id);

    // Explicit docking deny list blocks even owner corp members.
    snap.structure_docking_acl_denied
        .entry(struct_id)
        .or_default()
        .insert(owner_member);
    assert!(!character_has_structure_acl_access(&snap, owner_member, &st));
    snap.structure_docking_acl_denied.remove(&struct_id);

    // Destroyed state always denies.
    let mut destroyed_st = st.clone();
    destroyed_st.state = "Destroyed".into();
    assert!(!character_has_structure_acl_access(&snap, owner_member, &destroyed_st));

    // 2. `can_caller_view_structure_acl`
    assert!(can_caller_view_structure_acl(&snap, 999_888_777, npc_station, None));
    assert!(can_caller_view_structure_acl(&snap, 0, struct_id, Some(now)));
    assert!(can_caller_view_structure_acl(&snap, -1, struct_id, Some(now)));
    assert!(can_caller_view_structure_acl(&snap, neutral_member, struct_id, Some(now)));
    assert!(!can_caller_view_structure_acl(&snap, hostile_standing_member, struct_id, Some(now)));

    // Destroyed structure returns false for all callers (including <= 0).
    snap.structures.get_mut(&struct_id).unwrap().state = "Destroyed".into();
    assert!(!can_caller_view_structure_acl(&snap, 0, struct_id, None));
    assert!(!can_caller_view_structure_acl(&snap, owner_member, struct_id, None));
    snap.structures.get_mut(&struct_id).unwrap().state = "ShieldVulnerable".into();

    // Access profile blocking caller even when basic ACL allows, and blocking char 0 too.
    snap.item_custom_names.insert(
        -7_700_001,
        serde_json::json!({
            "access_profiles": {
                struct_id.to_string(): {
                    "blocked_characters": [neutral_member]
                }
            }
        })
        .to_string(),
    );
    assert!(!can_caller_view_structure_acl(&snap, neutral_member, struct_id, Some(now)));
    assert!(can_caller_view_structure_acl(&snap, zero_standing_member, struct_id, Some(now)));
    assert!(can_caller_view_structure_acl(&snap, -1, struct_id, Some(now)));
    snap.item_custom_names.insert(
        -7_700_001,
        serde_json::json!({
            "access_profiles": {
                struct_id.to_string(): {
                    "blocked_characters": [0]
                }
            }
        })
        .to_string(),
    );
    assert!(!can_caller_view_structure_acl(&snap, 0, struct_id, Some(now)));
    assert!(!can_caller_view_structure_acl(&snap, -5, struct_id, Some(now)));
    snap.item_custom_names.remove(&-7_700_001);

    // 3. `can_caller_view_market_location{,_at}` & `verify_structure_market_query_access`
    assert!(can_caller_view_market_location(&snap, hostile_standing_member, npc_station));
    assert!(can_caller_view_market_location_at(Some(&ctx), &snap, 0, npc_station, Some(now)));
    assert!(verify_structure_market_query_access(Some(&ctx), &snap, 0, 999_999_999, false).is_ok());
    let not_found_err =
        verify_structure_market_query_access(Some(&ctx), &snap, owner_member, 999_999_999, true)
            .unwrap_err();
    assert_eq!(not_found_err.code, "StructureNotFound");

    // Without online market hub module -> false / StructureServiceAccessDenied
    assert!(!can_caller_view_market_location(&snap, owner_member, struct_id));
    let no_mkt_err =
        verify_structure_market_query_access(Some(&ctx), &snap, owner_member, struct_id, true)
            .unwrap_err();
    assert_eq!(no_mkt_err.code, "StructureServiceAccessDenied");

    // Install Standup Market Hub I (35892) in ServiceSlot0 (164)
    snap.items.insert(
        mkt_mod_id,
        InventoryItemRecord {
            item_id: mkt_mod_id,
            type_id: 35_892,
            owner_id: owner_corp,
            location_id: struct_id,
            flag: InventoryFlag::ServiceSlot0,
            quantity: 1,
            singleton: true,
            version: 1,
            ..Default::default()
        },
    );
    assert!(can_caller_view_market_location(&snap, owner_member, struct_id));
    assert!(can_caller_view_market_location_at(Some(&ctx), &snap, neutral_member, struct_id, Some(now)));
    assert!(can_caller_view_market_location_at(Some(&ctx), &snap, 0, struct_id, Some(now)));
    assert!(can_caller_view_market_location_at(Some(&ctx), &snap, -1, struct_id, Some(now)));
    assert!(!can_caller_view_market_location_at(Some(&ctx), &snap, hostile_standing_member, struct_id, Some(now)));
    assert!(verify_structure_market_query_access(Some(&ctx), &snap, neutral_member, struct_id, true).is_ok());
    assert!(verify_structure_market_query_access(None, &snap, neutral_member, struct_id, true).is_ok());
    assert_eq!(
        verify_structure_market_query_access(Some(&ctx), &snap, hostile_standing_member, struct_id, true)
            .unwrap_err()
            .code,
        "StructureServiceAccessDenied"
    );

    // Non-market hull (35832 Astrahus Medium) fails `structure_allows_market_orders` when `ctx` is Some
    snap.structures.get_mut(&struct_id).unwrap().type_id = 35_832;
    assert!(!can_caller_view_market_location_at(Some(&ctx), &snap, owner_member, struct_id, Some(now)));
    assert_eq!(
        verify_structure_market_query_access(Some(&ctx), &snap, owner_member, struct_id, true)
            .unwrap_err()
            .code,
        "StructureServiceAccessDenied"
    );
    snap.structures.get_mut(&struct_id).unwrap().type_id = 35_826;

    // `SETTING_MARKET_TAX` = false denies Market service even when Docking is allowed
    snap.item_custom_names.insert(
        -7_700_001,
        serde_json::json!({
            "access_profiles": {
                struct_id.to_string(): {
                    "general_settings": { "4": false }
                }
            }
        })
        .to_string(),
    );
    assert!(!can_caller_view_market_location_at(Some(&ctx), &snap, neutral_member, struct_id, Some(now)));
    assert!(!can_caller_view_market_location(&snap, neutral_member, struct_id));
    snap.item_custom_names.remove(&-7_700_001);

    // Destroyed structure rejects market visibility and query
    snap.structures.get_mut(&struct_id).unwrap().state = "Destroyed".into();
    assert!(!can_caller_view_market_location(&snap, owner_member, struct_id));
    assert_eq!(
        verify_structure_market_query_access(None, &snap, owner_member, struct_id, true)
            .unwrap_err()
            .code,
        "StructureServiceAccessDenied"
    );
    snap.structures.get_mut(&struct_id).unwrap().state = "ShieldVulnerable".into();

    // 4. `primary_online_industry_activity`
    let st_ref = snap.structures[&struct_id].clone();
    assert_eq!(primary_online_industry_activity(&snap, &st_ref), None);

    snap.items.insert(
        ind_mod_id,
        InventoryItemRecord {
            item_id: ind_mod_id,
            type_id: 35_878, // Standup Manufacturing Plant I (activity 1)
            owner_id: owner_corp,
            location_id: struct_id,
            flag: InventoryFlag::ServiceSlot1,
            quantity: 1,
            singleton: true,
            version: 1,
            ..Default::default()
        },
    );
    assert_eq!(primary_online_industry_activity(&snap, &st_ref), Some(1));

    // Swap to Composite Reactor (35882 -> activity 9)
    snap.items.get_mut(&ind_mod_id).unwrap().type_id = 35_882;
    assert_eq!(primary_online_industry_activity(&snap, &st_ref), Some(9));

    // Destroyed overrides online module -> None
    assert_eq!(primary_online_industry_activity(&snap, &destroyed_st), None);
}

#[test]
fn domain_market_acl_contract_visibility_party_and_region_resolution() {
    use crate::domain::market_acl::{
        contract_in_region, is_character_party_to_contract, is_contract_visible_to_character,
        is_public_available_contract,
    };
    use eve_db::WorldStateSnapshot;
    use eve_sde::SdeRegistry;

    let sde = SdeRegistry::with_canonical_seed();
    let mut snap = WorldStateSnapshot::default();
    let issuer = 91_600_001_i64;
    let issuer_corp = 98_600_001_i64;
    let other_char = 91_600_002_i64;
    let other_corp = 98_600_002_i64;
    let corp_peer = 91_600_003_i64;
    let ally_id = 99_600_001_i64;

    let seed_ch = eve_db::WorldRepository::with_demo_seed()
        .with_snapshot(|s| s.characters.values().next().cloned().expect("seed char"));
    for (cid, corp, ally) in [
        (issuer, issuer_corp, Some(ally_id)),
        (corp_peer, issuer_corp, Some(ally_id)),
        (other_char, other_corp, Some(ally_id)),
    ] {
        let mut ch = seed_ch.clone();
        ch.standings.clear();
        ch.character_id = cid;
        ch.name = format!("C{cid}");
        ch.corporation_id = corp;
        ch.alliance_id = ally;
        snap.characters.insert(cid, ch);
    }

    let base = ContractRecord {
        contract_id: 1,
        issuer_id: issuer,
        issuer_corp_id: issuer_corp,
        assignee_id: 0,
        availability: "Public".into(),
        status: "Outstanding".into(),
        ..Default::default()
    };

    // 1. `is_public_available_contract`
    assert!(is_public_available_contract(&base));
    let mut empty_avail = base.clone();
    empty_avail.availability = "".into();
    assert!(is_public_available_contract(&empty_avail));
    let mut lower_pub = base.clone();
    lower_pub.availability = "public".into();
    assert!(is_public_available_contract(&lower_pub));
    let mut assigned_pub = base.clone();
    assigned_pub.assignee_id = other_char;
    assert!(!is_public_available_contract(&assigned_pub));
    let mut personal_unassigned = base.clone();
    personal_unassigned.availability = "Personal".into();
    assert!(!is_public_available_contract(&personal_unassigned));

    // 2. `is_contract_visible_to_character`
    let mut priv_c = base.clone();
    priv_c.availability = "Personal".into();
    priv_c.assignee_id = 777_777;
    assert!(is_contract_visible_to_character(&priv_c, issuer, 0, None));
    assert!(!is_contract_visible_to_character(&priv_c, other_char, other_corp, Some(ally_id)));

    // Assigned to char / corp (!= 0) / alliance (Some(a) && a != 0)
    priv_c.assignee_id = other_char;
    assert!(is_contract_visible_to_character(&priv_c, other_char, 0, None));
    priv_c.assignee_id = other_corp;
    assert!(is_contract_visible_to_character(&priv_c, 999, other_corp, None));
    assert!(!is_contract_visible_to_character(&priv_c, 999, 0, None));
    priv_c.assignee_id = ally_id;
    assert!(is_contract_visible_to_character(&priv_c, 999, 0, Some(ally_id)));
    assert!(!is_contract_visible_to_character(&priv_c, 999, 0, Some(0)));

    // Unassigned Corporation / Alliance / Private / Unknown availability
    let mut corp_c = base.clone();
    corp_c.availability = "Corporation".into();
    corp_c.assignee_id = 0;
    assert!(is_contract_visible_to_character(&corp_c, corp_peer, issuer_corp, None));
    assert!(!is_contract_visible_to_character(&corp_c, other_char, other_corp, None));
    corp_c.issuer_corp_id = 0;
    assert!(!is_contract_visible_to_character(&corp_c, 999, 0, None));

    let mut ally_c = base.clone();
    ally_c.availability = "Alliance".into();
    assert!(!is_contract_visible_to_character(&ally_c, 999, 0, None));
    assert!(!is_contract_visible_to_character(&ally_c, other_char, other_corp, Some(ally_id)));
    assert!(is_contract_visible_to_character(&ally_c, other_char, other_corp, Some(0)));

    let mut private_c = base.clone();
    private_c.availability = "Private".into();
    assert!(!is_contract_visible_to_character(&private_c, other_char, other_corp, Some(ally_id)));

    let mut custom_c = base.clone();
    custom_c.availability = "Regional".into();
    assert!(is_contract_visible_to_character(&custom_c, other_char, other_corp, None));

    // 3. `is_character_party_to_contract`
    let mut party_c = ContractRecord {
        contract_id: 2,
        issuer_id: issuer,
        issuer_corp_id: issuer_corp,
        for_corp: true,
        availability: "Personal".into(),
        ..Default::default()
    };
    // issuer with `for_corp == true` is ONLY party if matching corp/acceptor/etc; remove issuer's corp from snap temporarily:
    snap.characters.get_mut(&issuer).unwrap().corporation_id = 98_600_999;
    assert!(!is_character_party_to_contract(&snap, issuer, &party_c));
    party_c.for_corp = false;
    assert!(is_character_party_to_contract(&snap, issuer, &party_c));
    snap.characters.get_mut(&issuer).unwrap().corporation_id = issuer_corp;

    party_c.issuer_id = 1;
    assert!(!is_character_party_to_contract(&snap, other_char, &party_c));
    party_c.acceptor_id = Some(other_char);
    assert!(is_character_party_to_contract(&snap, other_char, &party_c));
    party_c.acceptor_id = None;
    party_c.assignee_id = other_char;
    assert!(is_character_party_to_contract(&snap, other_char, &party_c));
    party_c.assignee_id = 0;
    party_c.current_bidder_id = Some(other_char);
    assert!(is_character_party_to_contract(&snap, other_char, &party_c));
    party_c.current_bidder_id = None;
    party_c.bid_history = vec![(other_char, IskCents(10_000), 1)];
    assert!(is_character_party_to_contract(&snap, other_char, &party_c));
    party_c.bid_history.clear();

    // Corp assignee & corp availability
    party_c.assignee_id = other_corp;
    assert!(is_character_party_to_contract(&snap, other_char, &party_c));
    party_c.assignee_id = 0;
    party_c.availability = "Corporation".into();
    assert!(is_character_party_to_contract(&snap, corp_peer, &party_c));
    assert!(!is_character_party_to_contract(&snap, other_char, &party_c));

    // Alliance assignee (with non-zero alliance vs zero alliance)
    party_c.availability = "Personal".into();
    party_c.assignee_id = ally_id;
    assert!(is_character_party_to_contract(&snap, other_char, &party_c));
    assert!(!is_character_party_to_contract(&snap, 888_888_888, &party_c)); // missing char

    // 4. `contract_in_region`
    let jita_station = 60_003_760_i64;
    let forge_region = sde.stations[&jita_station].region_id;
    assert!(contract_in_region(&sde, &snap, jita_station, forge_region));
    assert!(!contract_in_region(&sde, &snap, jita_station, forge_region + 1));

    let struct_id = 1_066_500_009_i64;
    let perimeter_sys = 30_000_144_u32;
    let sys_region = sde.solar_systems[&perimeter_sys].region_id;
    snap.structures.insert(
        struct_id,
        UpwellStructureRecord {
            structure_id: struct_id,
            type_id: 35_832,
            owner_corp_id: issuer_corp,
            solar_system_id: perimeter_sys,
            name: "Perim".into(),
            position: [0.0; 3],
            state: "ShieldVulnerable".into(),
            quantum_core_installed: true,
            fuel_blocks: 10,
            shield_hp: 100.0,
            armor_hp: 100.0,
            hull_hp: 100.0,
            reinforced_until_unix: 0,
            last_damage_tick: 0,
            last_damage_unix: 0,
            self_destruct_at_unix: None,
        },
    );
    assert!(contract_in_region(&sde, &snap, struct_id, sys_region));
    assert!(!contract_in_region(&sde, &snap, struct_id, sys_region + 99));
    assert!(!contract_in_region(&sde, &snap, 77_777_777, forge_region));
}
