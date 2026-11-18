//! Unit and mutation-killing tests for `quasar_economy_planetary_industry` (`Q-6`).

use super::*;
use crate::quasar_gateway::{
    PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_FORBIDDEN, STATUS_UNAUTHORIZED,
};
use eve_db::{
    AccountRecord, ContractRecord, CorporationRecord, IndustryJobRecord, InventoryItemRecord,
    IskCents, JournalMeta, WalletJournalEntry, WorldRepository,
};
use eve_proto::pb;
use eve_proto::quasar::{
    AuthoritativeContext, DynamicQuasarMessage, DynamicValue, GoogleAny, QuasarDescriptorRegistry,
    WireField, WireMessage, WireValue, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_sde::SdeRegistry;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};

const ROLE_PROGRAMMER: u64 = eve_machonet::service_roles::ROLE_GM_MASK;

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

trait TestRouterExt {
    fn dispatch(
        &self,
        ctx: &AuthoritativeContext,
        req: &GoogleAny,
    ) -> Result<GoogleAny, (u32, String)>;
}
impl TestRouterExt for PublicGatewayRouter {
    fn dispatch(
        &self,
        ctx: &AuthoritativeContext,
        req: &GoogleAny,
    ) -> Result<GoogleAny, (u32, String)> {
        let norm = req.normalized_type_name();
        let handler = self
            .handlers
            .read()
            .unwrap()
            .get(norm)
            .cloned()
            .expect("registered handler");
        let dyn_msg = self
            .descriptors
            .decode_dynamic_or_wire(norm, &req.value)
            .unwrap_or_else(|_| {
                crate::services::quasar_char_skill_services::decode_wire_dynamic(norm, &req.value)
            });
        handler(ctx, req, &dyn_msg)
    }
}

fn make_router_and_db() -> (Arc<PublicGatewayRouter>, Arc<WorldRepository>, Arc<SdeRegistry>) {
    let db = Arc::new(WorldRepository::with_demo_seed());
    let sde = shared_sde();
    let descriptors = QuasarDescriptorRegistry::open_default_or_empty();
    let router = Arc::new(PublicGatewayRouter::new(
        Arc::clone(&descriptors),
        Arc::clone(&db),
        Arc::clone(&sde),
    ));
    (router, db, sde)
}

fn seed_char(db: &WorldRepository, char_id: i64, corp_id: i64, isk: i64) {
    db.with_mut_snapshot(|s| {
        let mut rec = s.characters[&90_000_001].clone();
        rec.character_id = char_id;
        rec.corporation_id = corp_id;
        rec.balance = IskCents::from_isk(isk);
        s.characters.insert(char_id, rec);
    });
}

fn make_ctx(char_id: Option<u32>, role: u64) -> AuthoritativeContext {
    AuthoritativeContext::for_session_at(
        Some(1),
        char_id,
        None,
        0,
        "tranquility".to_string(),
        b"evomu-quasar-gw1".to_vec(),
        1_760_000_000,
    )
    .with_service_roles(role)
}

fn encode_id_req(type_name: &str, id: u64) -> GoogleAny {
    let req = WireMessage {
        fields: vec![WireField {
            field_number: 1,
            value: WireValue::LengthDelimited(
                WireMessage {
                    fields: vec![WireField {
                        field_number: 1,
                        value: WireValue::Varint(id),
                    }],
                }
                .encode(),
            ),
        }],
    }
    .encode();
    GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"),
        value: req,
    }
}

fn encode_user_id_req(type_name: &str, user_id: i32) -> GoogleAny {
    GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"),
        value: pb::message(type_name)
            .set("user.sequential", user_id)
            .must_encode(),
    }
}

#[test]
fn extract_u64_and_accounting_journal_entries_and_auth() {
    // parity: feat:quasar.accounting
    let mut m = DynamicQuasarMessage::new("test.Msg");
    m.set(1, DynamicValue::Uint64(42));
    assert_eq!(extract_u64_from_field(&m, 1), 42);
    m.set(2, DynamicValue::Uint32(43));
    assert_eq!(extract_u64_from_field(&m, 2), 43);
    m.set(3, DynamicValue::Int64(44));
    assert_eq!(extract_u64_from_field(&m, 3), 44);
    m.set(4, DynamicValue::Int32(45));
    assert_eq!(extract_u64_from_field(&m, 4), 45);
    assert_eq!(extract_u64_from_field(&m, 99), 0);

    let (router, db, _sde) = make_router_and_db();
    let cid = 211200100i64;
    let other_cid = 211200101i64;
    let corp_id = 98000100i64;

    seed_char(&db, cid, corp_id, 5_000_000);
    seed_char(&db, other_cid, 98000999, 1_000_000);

    db.try_with_mut_snapshot(|snap| {
        let corp = { let mut __corp = CorporationRecord::default();
            __corp.corporation_id = corp_id;
            __corp.name = "TestCorp".to_string();
            __corp.ticker = "TCRP".to_string();
            __corp.ceo_id = cid;
            __corp.hq_station_id = Some(60003760);
            __corp.alliance_id = None;
            __corp.tax_rate = 0.05;
            __corp.members = HashSet::from([cid]);
            __corp.member_roles = HashMap::new();
            __corp.applications = HashMap::new();
            __corp.description = String::new();
            __corp.url = String::new();
            __corp.wallet_divisions = Vec::new();
            let __mw = IskCents(0);
            if __mw.0 != 0 { __corp.set_division_balance(1, __mw); } else { __corp.ensure_wallet_divisions(); }
            __corp };
        snap.corporations.insert(corp_id, corp);
        snap.wallet_journal.push(WalletJournalEntry {
            entry_id: 1,
            character_id: cid,
            entry_type: "Bounty".to_string(),
            amount: IskCents::from_isk(100_000),
            balance_after: IskCents::from_isk(5_100_000),
            reason: "Pirate bounty".to_string(),
        });
        snap.wallet_journal.push(WalletJournalEntry {
            entry_id: 2,
            character_id: corp_id,
            entry_type: "Tax".to_string(),
            amount: IskCents::from_isk(5_000),
            balance_after: IskCents::from_isk(5_000),
            reason: "Corp tax".to_string(),
        });
        snap.wallet_journal_meta.insert(
            1,
            JournalMeta {
                ref_type_id: 85,
                first_party_id: 1000009,
                second_party_id: cid,
                context_id: 0,
                context: Default::default(),
                account_key: 1000,
                date_unix: 1_771_111_222,
            },
        );
        snap.wallet_journal_meta.insert(
            2,
            JournalMeta {
                ref_type_id: 0,
                first_party_id: cid,
                second_party_id: corp_id,
                context_id: 0,
                context: Default::default(),
                account_key: 1000,
                date_unix: 1_771_234_567,
            },
        );
        Ok::<(), String>(())
    })
    .unwrap();

    let anon = make_ctx(None, 0);
    let user = make_ctx(Some(cid as u32), 0);
    let outsider = make_ctx(Some(other_cid as u32), 0);

    // 1. GetCharacterEntriesRequest
    let char_req_empty = GoogleAny {
        type_url: format!(
            "{TYPE_GOOGLEAPIS_PREFIX}eve.accounting.character.journal.GetCharacterEntriesRequest"
        ),
        value: vec![],
    };
    assert_eq!(
        router.dispatch(&anon, &char_req_empty).unwrap_err().0,
        STATUS_UNAUTHORIZED
    );
    let char_resp = router.dispatch(&user, &char_req_empty).expect("GetCharacterEntries");
    assert!(!char_resp.value.is_empty());
    let char_ts_bytes = WireMessage {
        fields: vec![WireField {
            field_number: 1,
            value: WireValue::Varint(1_771_111_222),
        }],
    }
    .encode();
    assert!(char_resp
        .value
        .windows(char_ts_bytes.len())
        .any(|w| w == char_ts_bytes.as_slice()));

    // Requesting another character's journal -> FORBIDDEN
    let char_req_other = encode_id_req(
        "eve.accounting.character.journal.GetCharacterEntriesRequest",
        other_cid as u64,
    );
    assert_eq!(
        router.dispatch(&user, &char_req_other).unwrap_err().0,
        STATUS_FORBIDDEN
    );

    // 2. GetCorporationEntriesRequest
    let corp_req = encode_id_req(
        "eve.accounting.corporation.journal.GetCorporationEntriesRequest",
        corp_id as u64,
    );
    assert_eq!(
        router.dispatch(&anon, &corp_req).unwrap_err().0,
        STATUS_UNAUTHORIZED
    );
    assert_eq!(
        router.dispatch(&outsider, &corp_req).unwrap_err().0,
        STATUS_FORBIDDEN
    );
    // CEO of corp is authorized
    let corp_resp = router
        .dispatch(&user, &corp_req)
        .expect("CEO GetCorporationEntries");
    assert!(!corp_resp.value.is_empty());
    let corp_ts_bytes = WireMessage {
        fields: vec![WireField {
            field_number: 1,
            value: WireValue::Varint(1_771_234_567),
        }],
    }
    .encode();
    assert!(corp_resp
        .value
        .windows(corp_ts_bytes.len())
        .any(|w| w == corp_ts_bytes.as_slice()));
    assert!(corp_resp
        .value
        .windows(b"Corp tax".len())
        .any(|w| w == b"Corp tax"));

    // Non-CEO member without accountant/director/trader role -> FORBIDDEN
    let member_no_role = 211200102i64;
    seed_char(&db, member_no_role, corp_id, 100_000);
    db.try_with_mut_snapshot(|snap| {
        snap.corporations.get_mut(&corp_id).unwrap().members.insert(member_no_role);
        Ok::<(), String>(())
    })
    .unwrap();
    let mem_ctx = make_ctx(Some(member_no_role as u32), 0);
    assert_eq!(
        router.dispatch(&mem_ctx, &corp_req).unwrap_err().0,
        STATUS_FORBIDDEN
    );

    // Grant ROLE_ACCOUNTANT to member -> succeeds
    db.try_with_mut_snapshot(|snap| {
        snap.corporations
            .get_mut(&corp_id)
            .unwrap()
            .member_roles
            .insert(member_no_role, eve_db::corp_roles::ROLE_ACCOUNTANT);
        Ok::<(), String>(())
    })
    .unwrap();
    assert!(router.dispatch(&mem_ctx, &corp_req).is_ok());

    // 3. eve.accounting.entrytype.GetAllRequest
    let et_resp = router
        .dispatch(
            &user,
            &GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.accounting.entrytype.GetAllRequest"),
                value: vec![],
            },
        )
        .expect("entrytype GetAllRequest");
    assert!(!et_resp.value.is_empty());
    assert!(et_resp
        .value
        .windows(b"Manufacturing".len())
        .any(|w| w == b"Manufacturing"));
}

#[test]
fn contract_item_exchange_and_lp_assetholding_handlers() {
    // parity: feat:quasar.contract
    let (router, db, _sde) = make_router_and_db();
    let issuer = 211200110i64;
    let assignee = 211200111i64;
    let stranger = 211200112i64;
    for id in [issuer, assignee, stranger] {
        seed_char(&db, id, 1000009, 1_000_000);
    }

    db.try_with_mut_snapshot(|snap| {
        snap.contracts.insert(
            1001,
            ContractRecord {
                contract_id: 1001,
                issuer_id: issuer,
                issuer_corp_id: 1000009,
                assignee_id: 0,
                acceptor_id: None,
                contract_type: "ItemExchange".to_string(),
                availability: "Public".to_string(),
                status: "Outstanding".to_string(),
                title: "Public Contract".to_string(),
                for_corp: false,
                price: IskCents::from_isk(2_500_000),
                reward: IskCents(0),
                collateral: IskCents(0),
                start_station_id: 60003760,
                end_station_id: 60003760,
                expiration_unix: 1_800_000_000,
                date_issued_unix: 1_755_000_000,
                num_days: 14,
                ..Default::default()
            },
        );
        snap.contracts.insert(
            1002,
            ContractRecord {
                contract_id: 1002,
                issuer_id: issuer,
                issuer_corp_id: 98000200,
                assignee_id: assignee,
                acceptor_id: None,
                contract_type: "ItemExchange".to_string(),
                availability: "Personal".to_string(),
                status: "Outstanding".to_string(),
                title: "Private Contract".to_string(),
                for_corp: false,
                price: IskCents::from_isk(9_000_000),
                reward: IskCents(0),
                collateral: IskCents(0),
                start_station_id: 60003760,
                end_station_id: 60003760,
                expiration_unix: 1_800_000_000,
                date_issued_unix: 1_755_000_000,
                num_days: 14,
                ..Default::default()
            },
        );
        Ok::<(), String>(())
    })
    .unwrap();

    let anon = make_ctx(None, 0);
    let issuer_ctx = make_ctx(Some(issuer as u32), 0);
    let assignee_ctx = make_ctx(Some(assignee as u32), 0);
    let stranger_ctx = make_ctx(Some(stranger as u32), 0);

    // Public contract readable by anyone (even anon); missing contract returns STATUS_NOT_FOUND
    let pub_req = encode_id_req("eve.contract.item_exchange.GetRequest", 1001);
    let pub_resp = router.dispatch(&anon, &pub_req).expect("public contract");
    let pub_ts_bytes = WireMessage {
        fields: vec![WireField {
            field_number: 1,
            value: WireValue::Varint(1_755_000_000),
        }],
    }
    .encode();
    assert!(pub_resp
        .value
        .windows(pub_ts_bytes.len())
        .any(|w| w == pub_ts_bytes.as_slice()));
    let missing_req = encode_id_req("eve.contract.item_exchange.GetRequest", 9999);
    assert_eq!(
        router.dispatch(&anon, &missing_req).unwrap_err().0,
        crate::quasar_gateway::STATUS_NOT_FOUND
    );

    // Private contract: anon -> UNAUTHORIZED, stranger -> FORBIDDEN, issuer/assignee -> OK
    let priv_req = encode_id_req("eve.contract.item_exchange.GetRequest", 1002);
    assert_eq!(
        router.dispatch(&anon, &priv_req).unwrap_err().0,
        STATUS_UNAUTHORIZED
    );
    assert_eq!(
        router.dispatch(&stranger_ctx, &priv_req).unwrap_err().0,
        STATUS_FORBIDDEN
    );
    assert!(router.dispatch(&issuer_ctx, &priv_req).is_ok());
    assert!(router.dispatch(&assignee_ctx, &priv_req).is_ok());

    // 4 & 5. LoyaltyPoints WithdrawRequest & DepositRequest (authoritative handlers in
    // `quasar_fleet_navigation_services.rs` require GM/programmer admin role and validate payload)
    let lp_admin_ctx = make_ctx(Some(issuer as u32), ROLE_PROGRAMMER);
    for rpc in [
        "eve.loyaltypoints.assetholding.operation.api.WithdrawRequest",
        "eve.loyaltypoints.assetholding.operation.api.DepositRequest",
    ] {
        let req = GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{rpc}"),
            value: vec![],
        };
        assert_eq!(router.dispatch(&anon, &req).unwrap_err().0, STATUS_FORBIDDEN);
        assert_eq!(
            router.dispatch(&issuer_ctx, &req).unwrap_err().0,
            STATUS_FORBIDDEN
        );
        assert_eq!(
            router.dispatch(&lp_admin_ctx, &req).unwrap_err().0,
            STATUS_BAD_REQUEST
        );
    }
}

#[test]
fn industry_planets_pi_moons_belts_bloodlines_wallet_and_id_handlers() {
    // parity: feat:quasar.planetinteraction
    let (router, db, _sde) = make_router_and_db();
    let cid = 211200120i64;
    seed_char(&db, cid, 1000009, 50_000);
    db.try_with_mut_snapshot(|snap| {
        snap.accounts.insert(
            "tester".to_string(),
            AccountRecord {
                user_id: 77,
                username: "tester_acct".to_string(),
                password_token: String::new(),
                role: 0,
                banned_until_unix: None,
                ban_reason: None,
            },
        );
        snap.items.insert(
            88_001,
            InventoryItemRecord {
                item_id: 88_001,
                type_id: 681,
                owner_id: cid,
                location_id: 60_003_760,
                singleton: true,
                quantity: 1,
                ..Default::default()
            },
        );
        snap.industry_jobs.insert(
            91_001,
            IndustryJobRecord {
                job_id: 91_001,
                installer_id: cid,
                facility_id: 60_003_760,
                blueprint_item_id: 88_001,
                blueprint_type_id: 681,
                activity_id: 1,
                runs: 2,
                product_type_id: 587,
                status: "Active".to_string(),
                duration_seconds: 3_600,
                start_unix: 1_760_000_000,
                blueprint_location_id: 60_003_760,
                output_location_id: 60_003_760,
                cost: Some(12_500.0),
                ..Default::default()
            },
        );
        snap.industry_jobs.insert(
            91_002,
            IndustryJobRecord {
                job_id: 91_002,
                installer_id: cid,
                facility_id: 60_003_760,
                blueprint_item_id: 88_001,
                blueprint_type_id: 681,
                activity_id: 1,
                runs: 1,
                product_type_id: 587,
                status: "Delivered".to_string(),
                duration_seconds: 1_800,
                start_unix: 1_759_990_000,
                blueprint_location_id: 60_003_760,
                output_location_id: 60_003_760,
                cost: Some(6_250.0),
                completed_unix: Some(1_759_991_800),
                completed_character_id: Some(cid),
                ..Default::default()
            },
        );
        snap.planetary_colony_layouts.insert(
            (cid, 40_000_002),
            eve_db::PlanetaryColonyLayoutRecord {
                pins: vec![
                    eve_db::PlanetaryPinRecord {
                        pin_id: 50_001,
                        type_id: 2848, // Extractor Control Unit
                        ..Default::default()
                    },
                    eve_db::PlanetaryPinRecord {
                        pin_id: 50_002,
                        type_id: 2469, // Basic Industry Facility
                        ..Default::default()
                    },
                    eve_db::PlanetaryPinRecord {
                        pin_id: 50_003,
                        type_id: 2541, // Storage Facility
                        ..Default::default()
                    },
                ],
                ..Default::default()
            },
        );
        Ok::<(), String>(())
    })
    .unwrap();

    let cid_rx = 211200121i64;
    seed_char(&db, cid_rx, 1000009, 1_000);
    let skyhook_id = 900_600_001i64;
    let target_corp_id = 98_000_777i64;
    db.with_mut_snapshot(|snap| {
        let mut alt_corp = snap.corporations[&98_000_001].clone();
        alt_corp.corporation_id = target_corp_id;
        alt_corp.name = "Skyhook Target Corp".to_string();
        snap.corporations.insert(target_corp_id, alt_corp);
        snap.corp_governance.entry(98_000_001).or_default().reinforce_hour = 18;
        snap.corp_governance.entry(target_corp_id).or_default().reinforce_hour = 18;
        snap.structures.insert(
            skyhook_id,
            eve_db::UpwellStructureRecord {
                structure_id: skyhook_id,
                type_id: crate::services::sov_services::TYPE_ORBITAL_SKYHOOK,
                owner_corp_id: 98_000_001,
                solar_system_id: 30_000_142,
                name: "Test Skyhook".to_string(),
                state: "ShieldVulnerable".to_string(),
                quantum_core_installed: true,
                ..Default::default()
            },
        );
    });

    let user = make_ctx(Some(cid as u32), 0);
    let admin = make_ctx(Some(cid as u32), ROLE_PROGRAMMER);

    for rpc in [
        "eve.character.industry.job.GetRequest",
        "eve.character.industry.job.GetActiveRequest",
        "eve.character.industry.job.GetCompletedRequest",
        "eve.character.industry.blueprints.GetRequest",
        "eve.industry.facility.GetRequest",
        "eve.industry.system.GetCostIndicesRequest",
        "eve.bloodline.GetAllRequest",
        "eve.killmail.Attributes",
        "eve.id.api.GetRequest",
    ] {
        let resp = router
            .dispatch(
                &user,
                &GoogleAny {
                    type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{rpc}"),
                    value: vec![],
                },
            )
            .expect(rpc);
        if rpc != "eve.killmail.Attributes" {
            assert!(!resp.value.is_empty(), "{rpc} should return non-empty payload");
        }
    }

    // Persisted Skyhook state & reinforcement exit hour (+ 404 on empty/missing Skyhook IDs)
    for rpc in [
        "eve.planet.skyhook.api.GetReinforcementExitHourRequest",
        "eve.planet.skyhook.api.GetStateRequest",
    ] {
        assert!(
            !router
                .dispatch(&user, &encode_id_req(rpc, skyhook_id as u64))
                .expect(rpc)
                .value
                .is_empty()
        );
        assert_eq!(
            router
                .dispatch(
                    &user,
                    &GoogleAny {
                        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{rpc}"),
                        value: vec![],
                    },
                )
                .unwrap_err()
                .0,
            crate::quasar_gateway::STATUS_NOT_FOUND
        );
        assert_eq!(
            router.dispatch(&user, &encode_id_req(rpc, 999_888_777)).unwrap_err().0,
            crate::quasar_gateway::STATUS_NOT_FOUND
        );
    }

    // Real SDE celestials, schematic & wrong-group negative check
    assert_eq!(
        router.dispatch(&user, &encode_id_req("eve.planet.GetRequest", 40_000_001)).unwrap_err().0,
        crate::quasar_gateway::STATUS_NOT_FOUND
    );
    let tanoo_i = router.dispatch(&user, &encode_id_req("eve.planet.GetRequest", 40_000_002)).expect("Tanoo I");
    assert!(!tanoo_i.value.is_empty());
    let schem_65 = router.dispatch(&user, &encode_id_req("eve.planetinteraction.schematic.GetRequest", 65)).expect("schematic 65");
    assert!(String::from_utf8_lossy(&schem_65.value).contains("Superconductors"));
    let jita_moon = router.dispatch(&user, &encode_id_req("eve.moon.GetRequest", 40_009_081)).expect("Jita III - Moon 1");
    assert!(String::from_utf8_lossy(&jita_moon.value).contains("Jita III - Moon 1"));
    let tanoo_belt = router.dispatch(&user, &encode_id_req("eve.asteroidbelt.GetRequest", 40_000_003)).expect("Tanoo I - Asteroid Belt 1");
    assert!(String::from_utf8_lossy(&tanoo_belt.value).contains("Tanoo I - Asteroid Belt 1"));

    for (rpc, id) in [
        ("eve.planet.GetRequest", 40_000_002_u64),
        ("eve.planet.GetPlanetsInSolarSystemRequest", 30_000_142),
        ("eve.planetinteraction.collector.GetRequest", 50_001),
        ("eve.planetinteraction.factory.GetRequest", 50_002),
        ("eve.planetinteraction.schematic.GetRequest", 65),
        ("eve.planetinteraction.storage.GetRequest", 50_003),
        ("eve.moon.GetRequest", 40_009_081),
        ("eve.moon.GetMoonsOfPlanetRequest", 40_009_080),
        ("eve.asteroidbelt.GetRequest", 40_000_003),
        ("eve.asteroidbelt.GetBeltsOfPlanetRequest", 40_000_002),
    ] {
        let resp = router.dispatch(&user, &encode_id_req(rpc, id)).expect(rpc);
        assert!(!resp.value.is_empty(), "{rpc} should return non-empty payload");
    }

    // `eve.user.Identifier.sequential` is protobuf `sint32` (ZigZag-encoded)
    for (rpc, id) in [
        ("eve.id.api.GetByUserRequest", 1),
        ("eve.id.legacy.api.GetUserRequest", 1),
    ] {
        let resp = router
            .dispatch(&user, &encode_user_id_req(rpc, id))
            .expect(rpc);
        assert!(!resp.value.is_empty(), "{rpc} should return non-empty payload");
    }

    // Cross-user ID queries require admin role; unknown user IDs return 404; missing localization returns 404/400.
    for rpc in ["eve.id.api.GetByUserRequest", "eve.id.legacy.api.GetUserRequest"] {
        assert_eq!(
            router
                .dispatch(&user, &encode_user_id_req(rpc, 77))
                .unwrap_err()
                .0,
            STATUS_FORBIDDEN
        );
        assert!(router.dispatch(&admin, &encode_user_id_req(rpc, 77)).is_ok());
        assert_eq!(
            router
                .dispatch(&admin, &encode_user_id_req(rpc, 999_888))
                .unwrap_err()
                .0,
            crate::quasar_gateway::STATUS_NOT_FOUND
        );
    }
    assert_eq!(
        router
            .dispatch(
                &user,
                &GoogleAny {
                    type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.localization.message.api.GetLocalizedTextRequest"),
                    value: vec![],
                },
            )
            .unwrap_err()
            .0,
        STATUS_BAD_REQUEST
    );
    assert_eq!(
        router
            .dispatch(
                &user,
                &encode_id_req("eve.localization.message.api.GetLocalizedTextRequest", 999_999_999),
            )
            .unwrap_err()
            .0,
        crate::quasar_gateway::STATUS_NOT_FOUND
    );
    assert_eq!(
        router
            .dispatch(
                &user,
                &GoogleAny {
                    type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.localization.cerberus.api.GetLocalizedTextRequest"),
                    value: vec![],
                },
            )
            .unwrap_err()
            .0,
        STATUS_BAD_REQUEST
    );

    // Missing/unknown IDs return STATUS_NOT_FOUND (no synthetic fallbacks)
    for rpc in [
        "eve.planet.GetRequest",
        "eve.planet.GetPlanetsInSolarSystemRequest",
        "eve.planetinteraction.collector.GetRequest",
        "eve.planetinteraction.factory.GetRequest",
        "eve.planetinteraction.schematic.GetRequest",
        "eve.planetinteraction.storage.GetRequest",
        "eve.moon.GetRequest",
        "eve.moon.GetMoonsOfPlanetRequest",
        "eve.asteroidbelt.GetRequest",
        "eve.asteroidbelt.GetBeltsOfPlanetRequest",
    ] {
        assert_eq!(
            router.dispatch(&user, &encode_id_req(rpc, 999_999)).unwrap_err().0,
            crate::quasar_gateway::STATUS_NOT_FOUND,
            "{rpc} must return STATUS_NOT_FOUND on unknown id 999_999"
        );
    }
    let other_user = make_ctx(Some(211_200_199), 0);
    assert_eq!(
        router
            .dispatch(&other_user, &encode_id_req("eve.planetinteraction.collector.GetRequest", 50_001))
            .unwrap_err()
            .0,
        STATUS_FORBIDDEN
    );
    // Undocked / anonymous facility.GetRequest returns empty facilities list
    let anon = make_ctx(None, 0);
    let anon_fac = router
        .dispatch(
            &anon,
            &GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.industry.facility.GetRequest"),
                value: vec![],
            },
        )
        .expect("anonymous facility.GetRequest");
    assert!(anon_fac.value.is_empty(), "anonymous facility list must be empty");

    // Skyhook ChangeOwnerRequest requires admin and persists new owner corporation
    let sky_req_bytes = pb::message("eve.planet.skyhook.admin.api.ChangeOwnerRequest")
        .set("skyhook.sequential", skyhook_id as u64)
        .set("solar_system.sequential", 30_000_142u32)
        .set("corporation.sequential", target_corp_id as u32)
        .must_encode();
    let sky_req = GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.planet.skyhook.admin.api.ChangeOwnerRequest"),
        value: sky_req_bytes,
    };
    assert_eq!(router.dispatch(&user, &sky_req).unwrap_err().0, STATUS_FORBIDDEN);
    assert!(router.dispatch(&admin, &sky_req).is_ok());
    assert_eq!(db.with_snapshot(|s| s.structures[&skyhook_id].owner_corp_id), target_corp_id);

    // TransferByOwnerRequest requires admin role, explicit positive amount & valid recipient
    let xfer_type = format!("{TYPE_GOOGLEAPIS_PREFIX}eve.isk.wallet.api.TransferByOwnerRequest");
    let valid_xfer = pb::message("eve.isk.wallet.api.TransferByOwnerRequest")
        .set("transfer_from.character.sequential", cid as u32)
        .set("transfer_to.character.sequential", cid_rx as u32)
        .set("amount.units", 10_000i64)
        .must_encode();
    assert_eq!(
        router
            .dispatch(&user, &GoogleAny { type_url: xfer_type.clone(), value: valid_xfer.clone() })
            .unwrap_err()
            .0,
        STATUS_FORBIDDEN
    );
    assert_eq!(
        router
            .dispatch(&admin, &GoogleAny { type_url: xfer_type.clone(), value: vec![] })
            .unwrap_err()
            .0,
        STATUS_BAD_REQUEST
    );
    assert!(router
        .dispatch(&admin, &GoogleAny { type_url: xfer_type.clone(), value: valid_xfer })
        .is_ok());
    db.with_snapshot(|s| {
        assert_eq!(s.characters[&cid].balance, IskCents::from_isk(40_000));
        assert_eq!(s.characters[&cid_rx].balance, IskCents::from_isk(11_000));
    });

    let big_xfer = pb::message("eve.isk.wallet.api.TransferByOwnerRequest")
        .set("transfer_from.character.sequential", cid as u32)
        .set("transfer_to.character.sequential", cid_rx as u32)
        .set("amount.units", 100_000i64)
        .must_encode();
    assert_eq!(
        router
            .dispatch(&admin, &GoogleAny { type_url: xfer_type, value: big_xfer })
            .unwrap_err()
            .0,
        STATUS_BAD_REQUEST
    );
}

#[cfg(test)]
#[path = "quasar_economy_planetary_industry_extra_unit_tests.rs"]
mod quasar_economy_planetary_industry_extra_unit_tests;
