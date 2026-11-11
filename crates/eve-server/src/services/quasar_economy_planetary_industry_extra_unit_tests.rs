//! Extra unit and mutation-killing tests for `services::quasar_economy_planetary_industry` (`Q-6`).

use super::super::*;
use crate::quasar_gateway::{
    PublicGatewayRouter, STATUS_FORBIDDEN,
};
use eve_db::{
    ContractRecord, CorpMemberRoleAssignment, CorporationRecord, IskCents, JournalMeta,
    WalletJournalEntry, WorldRepository,
};
use eve_proto::quasar::{
    AuthoritativeContext, DynamicQuasarMessage, DynamicValue, GoogleAny, QuasarDescriptorRegistry,
    TYPE_GOOGLEAPIS_PREFIX,
};
use eve_sde::SdeRegistry;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};

const ROLE_PROGRAMMER: u64 = eve_machonet::service_roles::ROLE_GM_MASK;

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn dispatch_dyn(
    router: &PublicGatewayRouter,
    ctx: &AuthoritativeContext,
    type_name: &str,
    dyn_msg: &DynamicQuasarMessage,
) -> Result<GoogleAny, (u32, String)> {
    let handler = router
        .handlers
        .read()
        .unwrap()
        .get(type_name)
        .cloned()
        .expect("registered handler");
    let any = GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"),
        value: Vec::new(),
    };
    handler(ctx, &any, dyn_msg)
}

fn make_router_and_db() -> (Arc<PublicGatewayRouter>, Arc<WorldRepository>) {
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
    (router, db)
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

fn make_ctx(
    user_id: Option<i32>,
    char_id: Option<u32>,
    corp_id: Option<u32>,
    role: u64,
) -> AuthoritativeContext {
    AuthoritativeContext::for_session_at(
        user_id,
        char_id,
        corp_id,
        role,
        "tranquility".to_string(),
        b"evomu-quasar-gw1".to_vec(),
        1_760_000_000,
    )
    .with_service_roles(role)
}

#[test]
fn nested_extract_u64_and_corp_journal_role_assignments_and_starter_corp_override() {
    let (router, db) = make_router_and_db();
    let ceo_id = 211200501i64;
    let trader_id = 211200502i64;
    let starter_char_id = 211200503i64;
    let no_corp_char_id = 211200504i64;
    let corp_id = 98000500i64;

    seed_char(&db, ceo_id, corp_id, 1_000_000);
    seed_char(&db, trader_id, corp_id, 1_000_000);
    // starter_char_id has NPC starter corp 1_000_009 in DB, but ctx has corp_id!
    seed_char(&db, starter_char_id, 1_000_009, 1_000_000);
    seed_char(&db, no_corp_char_id, 0, 1_000_000);

    db.with_mut_snapshot(|snap| {
        snap.corporations.insert(
            corp_id,
            { let mut __corp = CorporationRecord::default();
                __corp.corporation_id = corp_id;
                __corp.name = "IndustryCorp".into();
                __corp.ticker = "ICRP".into();
                __corp.ceo_id = ceo_id;
                __corp.hq_station_id = Some(60003760);
                __corp.alliance_id = Some(99000500);
                __corp.tax_rate = 0.1;
                __corp.members = HashSet::from([ceo_id, trader_id, starter_char_id]);
                __corp.member_roles = HashMap::new();
                __corp.applications = HashMap::new();
                __corp.description = String::new();
                __corp.url = String::new();
                __corp.wallet_divisions = Vec::new();
                let __mw = IskCents(0);
                if __mw.0 != 0 { __corp.set_division_balance(1, __mw); } else { __corp.ensure_wallet_divisions(); }
                __corp },
        );
        // Grant ROLE_TRADER via corp_role_assignments table
        snap.corp_role_assignments.insert(
            (corp_id, trader_id),
            CorpMemberRoleAssignment {
                character_id: trader_id,
                roles: eve_db::corp_roles::ROLE_TRADER,
                grantable_roles: 0,
                roles_at_hq: 0,
                grantable_roles_at_hq: 0,
                roles_at_base: 0,
                grantable_roles_at_base: 0,
                roles_at_other: 0,
                grantable_roles_at_other: 0,
            },
        );
        // Add a journal entry matched via wallet_journal_meta.first_party_id == corp_id
        snap.wallet_journal.push(WalletJournalEntry {
            entry_id: 901,
            character_id: ceo_id,
            entry_type: "Contract".into(),
            amount: IskCents::from_isk(50_000),
            balance_after: IskCents::from_isk(950_000),
            reason: "Corp first party".into(),
        });
        snap.wallet_journal_meta.insert(
            901,
            JournalMeta {
                ref_type_id: 0,
                first_party_id: corp_id,
                second_party_id: ceo_id,
                context_id: 0,
                context: Default::default(),
                account_key: 1000,
                date_unix: 1_760_000_000,
            },
        );
    });

    let rpc = "eve.accounting.corporation.journal.GetCorporationEntriesRequest";
    let empty_d = DynamicQuasarMessage::new("req");

    // 1. Character with corp_id == 0 and requested_corp_id == 0 -> STATUS_FORBIDDEN ("No target corporation available")
    let ctx_no_corp = make_ctx(Some(1), Some(no_corp_char_id as u32), None, 0);
    assert_eq!(
        dispatch_dyn(&router, &ctx_no_corp, rpc, &empty_d)
            .unwrap_err()
            .0,
        STATUS_FORBIDDEN
    );

    // 2. Trader authorized via corp_role_assignments, using nested Message(Message(Int64)) in field 1
    let mut inner = DynamicQuasarMessage::new("inner");
    inner.set(1, DynamicValue::Int64(corp_id));
    let mut sub = DynamicQuasarMessage::new("sub");
    sub.set(1, DynamicValue::Message(Box::new(inner)));
    let mut d_nested = DynamicQuasarMessage::new("req");
    d_nested.set(1, DynamicValue::Message(Box::new(sub)));

    let ctx_trader = make_ctx(Some(1), Some(trader_id as u32), Some(corp_id as u32), 0);
    let rsp_trader = dispatch_dyn(&router, &ctx_trader, rpc, &d_nested).expect("trader access");
    assert!(!rsp_trader.value.is_empty());

    // 3. Starter corp (1_000_009) overridden by ctx_corp (corp_id) + ctx role bitmask ROLE_JUNIOR_ACCOUNTANT
    let ctx_starter = make_ctx(
        Some(1),
        Some(starter_char_id as u32),
        Some(corp_id as u32),
        eve_db::corp_roles::ROLE_JUNIOR_ACCOUNTANT,
    );
    let rsp_starter =
        dispatch_dyn(&router, &ctx_starter, rpc, &empty_d).expect("starter corp override");
    assert!(!rsp_starter.value.is_empty());

    // 4. Test nested Message(Uint32 / Int32 / Int64) in extract_u64_from_field via GetCharacterEntriesRequest
    for val in [
        DynamicValue::Uint32(ceo_id as u32),
        DynamicValue::Int32(ceo_id as i32),
        DynamicValue::Int64(ceo_id),
    ] {
        let mut sub_m = DynamicQuasarMessage::new("sub");
        sub_m.set(1, val);
        let mut d_m = DynamicQuasarMessage::new("req");
        d_m.set(1, DynamicValue::Message(Box::new(sub_m)));
        let ctx_ceo = make_ctx(Some(1), Some(ceo_id as u32), Some(corp_id as u32), 0);
        assert!(dispatch_dyn(
            &router,
            &ctx_ceo,
            "eve.accounting.character.journal.GetCharacterEntriesRequest",
            &d_m
        )
        .is_ok());
    }
}

#[test]
fn contract_authorization_branches_corp_acceptor_alliance_and_empty_availability() {
    let (router, db) = make_router_and_db();
    let issuer = 211200510i64;
    let char_acceptor = 211200511i64;
    let corp_member = 211200512i64;
    let alliance_member = 211200513i64;
    let corp_id = 98000510i64;
    let ally_corp_id = 98000511i64;
    let alliance_id = 99000510i64;

    seed_char(&db, issuer, 98000999, 1_000_000);
    seed_char(&db, char_acceptor, 1000009, 1_000_000);
    seed_char(&db, corp_member, corp_id, 1_000_000);
    seed_char(&db, alliance_member, ally_corp_id, 1_000_000);

    db.with_mut_snapshot(|snap| {
        snap.corporations.insert(
            ally_corp_id,
            { let mut __corp = CorporationRecord::default();
                __corp.corporation_id = ally_corp_id;
                __corp.name = "AllyCorp".into();
                __corp.ticker = "ALYC".into();
                __corp.ceo_id = alliance_member;
                __corp.hq_station_id = Some(60003760);
                __corp.alliance_id = Some(alliance_id);
                __corp.tax_rate = 0.0;
                __corp.members = HashSet::from([alliance_member]);
                __corp.member_roles = HashMap::new();
                __corp.applications = HashMap::new();
                __corp.description = String::new();
                __corp.url = String::new();
                __corp.wallet_divisions = Vec::new();
                let __mw = IskCents(0);
                if __mw.0 != 0 { __corp.set_division_balance(1, __mw); } else { __corp.ensure_wallet_divisions(); }
                __corp },
        );

        let mk_contract = |id: i64,
                           avail: &str,
                           for_corp: bool,
                           iss_corp: i64,
                           assignee: i64,
                           acceptor: Option<i64>| ContractRecord {
            contract_id: id,
            issuer_id: issuer,
            issuer_corp_id: iss_corp,
            assignee_id: assignee,
            acceptor_id: acceptor,
            contract_type: "ItemExchange".into(),
            availability: avail.into(),
            status: "Outstanding".into(),
            title: format!("C{id}"),
            for_corp,
            price: IskCents::from_isk(100_000),
            reward: IskCents::from_isk(50_000),
            collateral: IskCents(0),
            start_station_id: 60003760,
            end_station_id: 60003760,
            expiration_unix: 1_800_000_000,
            num_days: 7,
            ..Default::default()
        };

        // 2001: empty availability & assignee_id == 0 -> public
        snap.contracts
            .insert(2001, mk_contract(2001, "", false, 98000999, 0, None));
        // 2002: private, accepted by char_acceptor
        snap.contracts.insert(
            2002,
            mk_contract(2002, "Personal", false, 98000999, issuer, Some(char_acceptor)),
        );
        // 2003: private, for_corp == true && issuer_corp_id == corp_id
        snap.contracts.insert(
            2003,
            mk_contract(2003, "Personal", true, corp_id, issuer, None),
        );
        // 2004: availability == "Corporation" && issuer_corp_id == corp_id
        snap.contracts.insert(
            2004,
            mk_contract(2004, "Corporation", false, corp_id, 0, None),
        );
        // 2005: assignee_id == corp_id
        snap.contracts.insert(
            2005,
            mk_contract(2005, "Personal", false, 98000999, corp_id, None),
        );
        // 2006: acceptor_id == Some(corp_id)
        snap.contracts.insert(
            2006,
            mk_contract(2006, "Personal", false, 98000999, issuer, Some(corp_id)),
        );
        // 2007: assignee_id == alliance_id
        snap.contracts.insert(
            2007,
            mk_contract(2007, "Personal", false, 98000999, alliance_id, None),
        );
    });

    let rpc = "eve.contract.item_exchange.GetRequest";
    let mk_req = |cid: u64| {
        let mut d = DynamicQuasarMessage::new("req");
        d.set(1, DynamicValue::Uint64(cid));
        d
    };

    let anon = make_ctx(None, None, None, 0);
    let ctx_acc = make_ctx(Some(1), Some(char_acceptor as u32), None, 0);
    let ctx_corp = make_ctx(Some(1), Some(corp_member as u32), Some(corp_id as u32), 0);
    let ctx_ally = make_ctx(
        Some(1),
        Some(alliance_member as u32),
        Some(ally_corp_id as u32),
        0,
    );

    assert!(dispatch_dyn(&router, &anon, rpc, &mk_req(2001)).is_ok());
    assert!(dispatch_dyn(&router, &ctx_acc, rpc, &mk_req(2002)).is_ok());
    for cid in [2003, 2004, 2005, 2006] {
        assert!(
            dispatch_dyn(&router, &ctx_corp, rpc, &mk_req(cid)).is_ok(),
            "corp_member should access contract {cid}"
        );
    }
    assert!(dispatch_dyn(&router, &ctx_ally, rpc, &mk_req(2007)).is_ok());
}

#[test]
fn admin_lp_ops_structure_facility_isk_transfer_variants_and_id_handlers() {
    let (router, db) = make_router_and_db();
    let cid = 211200520i64;
    seed_char(&db, cid, 1000009, 100_000);
    // Dock character in a structure (station_id = None, structure_id = Some)
    db.with_mut_snapshot(|s| {
        let ch = s.characters.get_mut(&cid).unwrap();
        ch.station_id = None;
        ch.structure_id = Some(1_030_000_000_777);
    });

    let admin_no_char = make_ctx(Some(99), None, None, ROLE_PROGRAMMER);
    let user = make_ctx(Some(99), Some(cid as u32), None, 0);
    let empty_d = DynamicQuasarMessage::new("req");

    // 1. Admin without active character can call LP WithdrawRequest and DepositRequest
    for rpc in [
        "eve.loyaltypoints.assetholding.operation.api.WithdrawRequest",
        "eve.loyaltypoints.assetholding.operation.api.DepositRequest",
    ] {
        assert!(dispatch_dyn(&router, &admin_no_char, rpc, &empty_d).is_ok());
    }

    // 2. Facility & CostIndices for both structure-docked character and anonymous caller
    for ctx in [&user, &admin_no_char] {
        assert!(
            dispatch_dyn(&router, ctx, "eve.industry.facility.GetRequest", &empty_d).is_ok()
        );
        assert!(dispatch_dyn(
            &router,
            ctx,
            "eve.industry.system.GetCostIndicesRequest",
            &empty_d
        )
        .is_ok());
    }

    // 3. TransferByOwnerRequest with Int64(2500) in field 5 submessage
    let mut amt_m = DynamicQuasarMessage::new("amt");
    amt_m.set(1, DynamicValue::Int64(2_500));
    let mut xfer_d = DynamicQuasarMessage::new("req");
    xfer_d.set(5, DynamicValue::Message(Box::new(amt_m)));
    dispatch_dyn(
        &router,
        &user,
        "eve.isk.wallet.api.TransferByOwnerRequest",
        &xfer_d,
    )
    .expect("TransferByOwner Int64(2500)");
    assert_eq!(
        db.with_snapshot(|s| s.characters[&cid].balance),
        IskCents::from_isk(97_500)
    );

    // 4. GetLocalizedTextRequest, GetByUserRequest, GetUserRequest with Uint32 and empty fallback
    let mut u32_sub = DynamicQuasarMessage::new("sub");
    u32_sub.set(1, DynamicValue::Uint32(888));
    let mut d_u32 = DynamicQuasarMessage::new("req");
    d_u32.set(1, DynamicValue::Message(Box::new(u32_sub)));

    for rpc in [
        "eve.localization.message.api.GetLocalizedTextRequest",
        "eve.id.api.GetByUserRequest",
        "eve.id.legacy.api.GetUserRequest",
    ] {
        assert!(dispatch_dyn(&router, &user, rpc, &d_u32).is_ok());
        assert!(dispatch_dyn(&router, &user, rpc, &empty_d).is_ok());
    }
}

#[test]
fn authorize_corp_financial_access_roles_and_transfer_by_owner_positive_id_kill_mutants() {
    use crate::quasar_gateway::STATUS_UNAUTHORIZED;
    use eve_db::corp_roles::ROLE_ACCOUNTANT;

    let (router, db) = make_router_and_db();
    let corp_a = 98_000_591_i64;
    let corp_b = 98_000_592_i64;
    let ceo_id = 211_200_590_i64;
    let sess_role_only = 211_200_591_i64;
    let db_role_only = 211_200_592_i64;
    let both_roles = 211_200_593_i64;
    let other_corp_member = 211_200_594_i64;
    let outsider = 211_200_595_i64;

    seed_char(&db, ceo_id, corp_a, 100_000);
    seed_char(&db, sess_role_only, corp_a, 100_000);
    seed_char(&db, db_role_only, corp_a, 100_000);
    seed_char(&db, both_roles, corp_a, 100_000);
    seed_char(&db, other_corp_member, corp_b, 100_000);
    seed_char(&db, outsider, corp_b, 100_000);

    db.with_mut_snapshot(|snap| {
        let mut rec_a = CorporationRecord::default();
        rec_a.corporation_id = corp_a;
        rec_a.name = "AlphaFin".into();
        rec_a.ticker = "AFIN".into();
        rec_a.ceo_id = ceo_id;
        rec_a.members = HashSet::from([
            ceo_id,
            sess_role_only,
            db_role_only,
            both_roles,
            other_corp_member,
        ]);
        rec_a.ensure_wallet_divisions();
        snap.corporations.insert(corp_a, rec_a);

        let mut rec_b = CorporationRecord::default();
        rec_b.corporation_id = corp_b;
        rec_b.name = "BetaFin".into();
        rec_b.ticker = "BFIN".into();
        rec_b.ceo_id = outsider;
        rec_b.members = HashSet::from([other_corp_member, outsider]);
        rec_b.ensure_wallet_divisions();
        snap.corporations.insert(corp_b, rec_b);

        for cid in [db_role_only, both_roles] {
            snap.corp_role_assignments.insert(
                (corp_a, cid),
                CorpMemberRoleAssignment {
                    character_id: cid,
                    roles: ROLE_ACCOUNTANT,
                    grantable_roles: 0,
                    roles_at_hq: 0,
                    grantable_roles_at_hq: 0,
                    roles_at_base: 0,
                    grantable_roles_at_base: 0,
                    roles_at_other: 0,
                    grantable_roles_at_other: 0,
                },
            );
        }
    });

    db.with_snapshot(|snap| {
        // 1. Unauthenticated / invalid character -> Err(STATUS_UNAUTHORIZED) (kills Ok(0)/Ok(1)/Ok(-1))
        let unauth_ctx = make_ctx(Some(1), None, None, ROLE_ACCOUNTANT);
        assert_eq!(
            authorize_corp_financial_access(&unauth_ctx, snap, corp_a)
                .unwrap_err()
                .0,
            STATUS_UNAUTHORIZED
        );

        // 2. Non-member -> Err(STATUS_FORBIDDEN)
        let outsider_ctx = make_ctx(Some(1), Some(outsider as u32), None, ROLE_ACCOUNTANT);
        assert_eq!(
            authorize_corp_financial_access(&outsider_ctx, snap, corp_a)
                .unwrap_err()
                .0,
            STATUS_FORBIDDEN
        );

        // 3. Session-only role in same corp (`caller_corp == target_corp`, `eff_roles = 0`, `ctx_roles = ROLE_ACCOUNTANT`):
        //    kills `== -> !=` (positive branch) and `| -> &` (session-only branch) and `Ok(0/1/-1)`.
        let sess_ctx = make_ctx(Some(1), Some(sess_role_only as u32), None, ROLE_ACCOUNTANT);
        assert_eq!(
            authorize_corp_financial_access(&sess_ctx, snap, corp_a),
            Ok(corp_a)
        );
        // Zero requested_corp_id falls back to caller_corp == corp_a
        assert_eq!(
            authorize_corp_financial_access(&sess_ctx, snap, 0),
            Ok(corp_a)
        );

        // 4. DB-only role (`eff_roles = ROLE_ACCOUNTANT`, `ctx_roles = 0`): kills `| -> &` (db-only branch).
        let db_ctx = make_ctx(Some(1), Some(db_role_only as u32), None, 0);
        assert_eq!(
            authorize_corp_financial_access(&db_ctx, snap, corp_a),
            Ok(corp_a)
        );

        // 5. Same role present in BOTH `eff_roles` AND `session_roles`:
        //    `eff_roles | session_roles == ROLE_ACCOUNTANT` succeeds; XOR mutant gives 0 -> FORBIDDEN!
        let both_ctx = make_ctx(Some(1), Some(both_roles as u32), None, ROLE_ACCOUNTANT);
        assert_eq!(
            authorize_corp_financial_access(&both_ctx, snap, corp_a),
            Ok(corp_a)
        );

        // 6. Cross-corp member (`caller_corp = corp_b != corp_a`, member of `corp_a` with 0 DB roles in `corp_a`,
        //    carrying session `ROLE_ACCOUNTANT`): MUST be denied because session roles apply only to `caller_corp == target_corp`!
        //    Kills `== -> !=` (negative branch).
        let cross_ctx = make_ctx(Some(1), Some(other_corp_member as u32), None, ROLE_ACCOUNTANT);
        assert_eq!(
            authorize_corp_financial_access(&cross_ctx, snap, corp_a)
                .unwrap_err()
                .0,
            STATUS_FORBIDDEN
        );
    });

    // 7. TransferByOwnerRequest (`effective_character_id().filter(|&v| v > 0)`):
    //    - `Some(0)` and `None` MUST return `STATUS_UNAUTHORIZED` (kills `> -> ==` and `> -> >=`)
    //    - `Some(positive_cid)` MUST succeed, debit balance, and emit notice (kills `> -> <` and `register -> ()`)
    let mut amt_m = DynamicQuasarMessage::new("amt");
    amt_m.set(1, DynamicValue::Int64(1_250));
    let mut xfer_d = DynamicQuasarMessage::new("req");
    xfer_d.set(5, DynamicValue::Message(Box::new(amt_m)));

    let zero_char_ctx = make_ctx(Some(99), Some(0), None, 0);
    assert_eq!(
        dispatch_dyn(
            &router,
            &zero_char_ctx,
            "eve.isk.wallet.api.TransferByOwnerRequest",
            &xfer_d
        )
        .unwrap_err()
        .0,
        STATUS_UNAUTHORIZED
    );

    let none_char_ctx = make_ctx(Some(99), None, None, 0);
    assert_eq!(
        dispatch_dyn(
            &router,
            &none_char_ctx,
            "eve.isk.wallet.api.TransferByOwnerRequest",
            &xfer_d
        )
        .unwrap_err()
        .0,
        STATUS_UNAUTHORIZED
    );

    let notices_before = router.pending_notices.read().unwrap().len();
    let valid_ctx = make_ctx(Some(99), Some(sess_role_only as u32), None, 0);
    let rsp = dispatch_dyn(
        &router,
        &valid_ctx,
        "eve.isk.wallet.api.TransferByOwnerRequest",
        &xfer_d,
    )
    .expect("positive character id must pass TransferByOwnerRequest");
    assert!(rsp.type_url.ends_with("eve.isk.wallet.api.TransferByOwnerResponse"));
    assert_eq!(
        db.with_snapshot(|s| s.characters[&sess_role_only].balance),
        IskCents::from_isk(98_750)
    );
    assert_eq!(
        router.pending_notices.read().unwrap().len(),
        notices_before + 1
    );
}
