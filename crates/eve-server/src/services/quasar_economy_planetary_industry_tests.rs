//! Unit and mutation-killing tests for `quasar_economy_planetary_industry` (`Q-6`).

use super::*;
use crate::quasar_gateway::{
    PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_FORBIDDEN, STATUS_UNAUTHORIZED,
};
use eve_db::{
    AccountRecord, ContractRecord, CorporationRecord, IskCents, JournalMeta, WalletJournalEntry,
    WorldRepository,
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
        let dyn_msg =
            crate::services::quasar_char_skill_services::decode_wire_dynamic(norm, &req.value);
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
    register_economy_planetary_industry_handlers(
        &router,
        Arc::clone(&db),
        Arc::clone(&sde),
        descriptors,
    );
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
            2,
            JournalMeta {
                ref_type_id: 0,
                first_party_id: cid,
                second_party_id: corp_id,
                context_id: 0,
                context: Default::default(),
                account_key: 1000,
                date_unix: 1_760_000_000,
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

    // Public contract readable by anyone (even anon) and missing contract returns default public attrs
    let pub_req = encode_id_req("eve.contract.item_exchange.GetRequest", 1001);
    assert!(router.dispatch(&anon, &pub_req).is_ok());
    let missing_req = encode_id_req("eve.contract.item_exchange.GetRequest", 9999);
    assert!(router.dispatch(&anon, &missing_req).is_ok());

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

    // 4 & 5. LoyaltyPoints WithdrawRequest & DepositRequest
    for rpc in [
        "eve.loyaltypoints.assetholding.operation.api.WithdrawRequest",
        "eve.loyaltypoints.assetholding.operation.api.DepositRequest",
    ] {
        let req = GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{rpc}"),
            value: vec![],
        };
        assert_eq!(router.dispatch(&anon, &req).unwrap_err().0, STATUS_FORBIDDEN);
        assert!(router.dispatch(&issuer_ctx, &req).is_ok());
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
        Ok::<(), String>(())
    })
    .unwrap();

    let user = make_ctx(Some(cid as u32), 0);
    let admin = make_ctx(Some(cid as u32), ROLE_PROGRAMMER);

    for rpc in [
        "eve.character.industry.job.GetRequest",
        "eve.character.industry.job.GetActiveRequest",
        "eve.character.industry.job.GetCompletedRequest",
        "eve.character.industry.blueprints.GetRequest",
        "eve.industry.facility.GetRequest",
        "eve.industry.system.GetCostIndicesRequest",
        "eve.planet.skyhook.api.GetReinforcementExitHourRequest",
        "eve.planet.skyhook.api.GetStateRequest",
        "eve.bloodline.GetAllRequest",
        "eve.localization.cerberus.api.GetLocalizedTextRequest",
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
        if rpc != "eve.character.industry.blueprints.GetRequest"
            && rpc != "eve.killmail.Attributes"
        {
            assert!(!resp.value.is_empty(), "{rpc} should return non-empty payload");
        }
    }

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
        "eve.localization.message.api.GetLocalizedTextRequest",
        "eve.id.api.GetByUserRequest",
        "eve.id.legacy.api.GetUserRequest",
    ] {
        let resp = router.dispatch(&user, &encode_id_req(rpc, 77)).expect(rpc);
        assert!(!resp.value.is_empty(), "{rpc} should return non-empty payload");
    }

    // Skyhook ChangeOwnerRequest requires admin
    let sky_req = GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.planet.skyhook.admin.api.ChangeOwnerRequest"),
        value: vec![],
    };
    assert_eq!(router.dispatch(&user, &sky_req).unwrap_err().0, STATUS_FORBIDDEN);
    assert!(router.dispatch(&admin, &sky_req).is_ok());

    // TransferByOwnerRequest: default 10,000 ISK succeeds, then 100,000 ISK fails (Insufficient ISK)
    let xfer_type = format!("{TYPE_GOOGLEAPIS_PREFIX}eve.isk.wallet.api.TransferByOwnerRequest");
    assert!(router
        .dispatch(&user, &GoogleAny { type_url: xfer_type.clone(), value: vec![] })
        .is_ok());

    let big_xfer = pb::message("eve.isk.wallet.api.TransferByOwnerRequest")
        .set("amount.units", 100_000i64)
        .must_encode();
    assert_eq!(
        router
            .dispatch(&user, &GoogleAny { type_url: xfer_type, value: big_xfer })
            .unwrap_err()
            .0,
        STATUS_BAD_REQUEST
    );
}

#[cfg(test)]
#[path = "quasar_economy_planetary_industry_extra_unit_tests.rs"]
mod quasar_economy_planetary_industry_extra_unit_tests;
