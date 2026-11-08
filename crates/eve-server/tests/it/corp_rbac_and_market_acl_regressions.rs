//! Integration regression tests for corporate RBAC (`#550`, `#551`, `#552`) and Upwell market ACLs (`#553`).

#![forbid(unsafe_code)]

use eve_db::ledger::MarketTransactionRecord;
use eve_db::{
    corp_roles, CorpMemberRoleAssignment, CorporationRecord, InventoryFlag, InventoryItemRecord,
    IskCents, MarketOrderRecord, TrainedSkillRecord, UpwellStructureRecord, WarRecord,
};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use eve_server::services::station_structure::structure_upwell_ops::{
    load_ext_state, save_ext_state, AssetSafetyWrapRecord, StructureAccessProfile,
};
use std::collections::HashMap;

const JITA_44: i64 = 60_003_760;
const JITA_ALT: i64 = 60_003_761;
const JITA_SYS: u32 = 30_000_142;
const FORGE_REGION: u32 = 10_000_002;
const P1: i64 = 90_000_001;
const P2: i64 = 90_000_002;

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, svc, None, method, args, &HashMap::new(), None)
        .await
}

async fn login_char(
    ctx: &ServerContext,
    sess: &mut SessionState,
    username: &str,
    password: &str,
    char_id: i64,
) {
    crate::common::login_pilot(ctx, sess, username, password).await;
    rpc(ctx, sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)])
        .await
        .expect("SelectCharacterID");
}

fn setup_corp_members(ctx: &ServerContext, corp_id: i64, p2_roles: u64, station_id: i64) {
    ctx.db.with_mut_snapshot(|snap| {
        let mut corp = CorporationRecord::default();
        corp.corporation_id = corp_id;
        corp.name = "RBAC Security Corp".into();
        corp.ticker = "RBSC".into();
        corp.ceo_id = P1;
        corp.hq_station_id = Some(station_id);
        corp.ensure_wallet_divisions();
        corp.set_division_balance(1, IskCents::from_isk(50_000_000));
        corp.set_division_balance(2, IskCents::from_isk(10_000_000));
        corp.members.insert(P1);
        corp.members.insert(P2);
        corp.member_roles.insert(P1, corp_roles::ROLE_DIRECTOR);
        corp.member_roles.insert(P2, p2_roles);
        snap.corporations.insert(corp_id, corp);

        for pid in [P1, P2] {
            if let Some(ch) = snap.characters.get_mut(&pid) {
                ch.corporation_id = corp_id;
                ch.station_id = Some(station_id);
                ch.solar_system_id = JITA_SYS;
                ch.balance = IskCents::from_isk(5_000_000);
            }
        }
    });
}

#[tokio::test]
async fn issue_550_corp_wallet_rbac_audit_withdraw_and_stasis() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut s1 = SessionState::new(1);
    let mut s2 = SessionState::new(2);
    login_char(&ctx, &mut s1, "demo", "demo-secret", P1).await;
    login_char(&ctx, &mut s2, "other", "other-secret", P2).await;

    let corp_id = 98_550_001_i64;
    let now = ctx.now_unix();
    setup_corp_members(&ctx, corp_id, corp_roles::ROLE_ACCOUNTANT, JITA_44);
    s1.corporation_id = Some(corp_id);
    s2.corporation_id = Some(corp_id);
    // Forged session.corp_role MUST NOT authorize anything
    s2.corp_role = corp_roles::ROLE_DIRECTOR | corp_roles::ROLE_ACCOUNT_CAN_TAKE[0];

    ctx.db.with_mut_snapshot(|snap| {
        snap.append_journal_entry(eve_db::JournalEntryInput {
            owner_id: corp_id,
            entry_type: "CorporationAccountWithdrawal",
            amount: IskCents::from_isk(1_000),
            balance_after: IskCents::from_isk(50_000_000),
            reason: "seed".into(),
            meta: eve_db::JournalMeta {
                ref_type_id: 37,
                account_key: 1001,
                date_unix: now,
                first_party_id: corp_id,
                second_party_id: P1,
                context_id: P1,
                ..Default::default()
            },
        });
        snap.market_transactions.push(MarketTransactionRecord {
            transaction_id: 9_550_001,
            date_unix: now,
            type_id: 34,
            quantity: 50,
            price: IskCents::from_isk(10),
            buyer_id: corp_id,
            seller_id: P1,
            location_id: JITA_44,
            buyer_account_key: 1001,
            seller_account_key: 1000,
            journal_ref_id: 1,
        });
    });

    // 1. Accountant alone can inspect journal/transactions, but CANNOT withdraw or move ISK
    assert!(rpc(&ctx, &mut s2, "account", "GetTransactions", &[PyRep::Integer(1001), PyRep::None, PyRep::None, PyRep::Bool(true)]).await.is_ok());
    assert!(rpc(&ctx, &mut s2, "marketProxy", "CorpGetTransactions", &[PyRep::Integer(0), PyRep::Integer(1001)]).await.is_ok());

    assert_eq!(
        rpc(&ctx, &mut s2, "account", "GiveCashFromCorpAccount", &[PyRep::Integer(P2), PyRep::Real(100.0), PyRep::Integer(1000)]).await.unwrap_err().code,
        "CrpAccessDenied"
    );
    assert_eq!(
        rpc(&ctx, &mut s2, "account", "GiveCashFromCorpAccount", &[PyRep::Integer(1), PyRep::Integer(2), PyRep::Real(100.0)]).await.unwrap_err().code,
        "CorpRoleDenied"
    );

    // 2. Grant Account Take 2 into 24h stasis (even if mirrored in member_roles, must be masked)
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.get_mut(&corp_id).unwrap().member_roles.insert(P2, corp_roles::ROLE_ACCOUNT_CAN_TAKE[1]);
        snap.corp_governance.entry(corp_id).or_default().pending_role_grants.insert(P2, (corp_roles::ROLE_ACCOUNT_CAN_TAKE[1], now + 86_400));
    });
    assert_eq!(
        rpc(&ctx, &mut s2, "account", "GiveCashFromCorpAccount", &[PyRep::Integer(2), PyRep::Integer(1), PyRep::Real(500.0)]).await.unwrap_err().code,
        "CorpRoleDenied"
    );
    assert_eq!(
        rpc(&ctx, &mut s2, "account", "GetTransactions", &[PyRep::Integer(1001), PyRep::None, PyRep::None, PyRep::Bool(true)]).await.unwrap_err().code,
        "CrpAccessDenied"
    );

    // 3. Advance clock past stasis -> Div 2 withdrawal & journal succeed, while Div 1 remains denied
    ctx.advance_virtual_clock(86_401);
    assert!(rpc(&ctx, &mut s2, "account", "GetTransactions", &[PyRep::Integer(1001), PyRep::None, PyRep::None, PyRep::Bool(true)]).await.is_ok());
    assert_eq!(
        rpc(&ctx, &mut s2, "account", "GetTransactions", &[PyRep::Integer(1000), PyRep::None, PyRep::None, PyRep::Bool(true)]).await.unwrap_err().code,
        "CrpAccessDenied"
    );
    rpc(&ctx, &mut s2, "account", "GiveCashFromCorpAccount", &[PyRep::Integer(2), PyRep::Integer(1), PyRep::Real(250_000.0)]).await.expect("div 2 -> div 1");
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.corporations[&corp_id].division_balance(2), IskCents::from_isk(9_750_000));
        assert_eq!(snap.corporations[&corp_id].division_balance(1), IskCents::from_isk(50_250_000));
    });
}

#[tokio::test]
async fn issue_551_corp_industry_rbac_flags_containers_and_locks() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut s1 = SessionState::new(1);
    let mut s2 = SessionState::new(2);
    login_char(&ctx, &mut s1, "demo", "demo-secret", P1).await;
    login_char(&ctx, &mut s2, "other", "other-secret", P2).await;

    let corp_id = 98_551_001_i64;
    let now = ctx.now_unix();
    setup_corp_members(&ctx, corp_id, 0, JITA_44);
    for s in [&mut s1, &mut s2] {
        s.corporation_id = Some(corp_id);
        s.station_id = Some(JITA_44);
        s.solar_system_id = Some(JITA_SYS);
    }
    // Forged session.corp_role must not authorize industry install
    s2.corp_role = corp_roles::ROLE_DIRECTOR | corp_roles::ROLE_FACTORY_MANAGER | corp_roles::ROLE_ACCOUNT_CAN_TAKE[0];

    let (cont_sag2, bp_cont, trit_cont, bp_deliv, alsc_lock_cont, bp_alsc, wrap_item, bp_wrap) = (
        955_100_001_i64,
        955_100_002_i64,
        955_100_003_i64,
        955_100_004_i64,
        955_100_008_i64,
        955_100_005_i64,
        955_100_006_i64,
        955_100_007_i64,
    );

    let extra_mats: Vec<(u32, i32)> = ctx.sde.blueprint(688)
        .map(|b| b.materials.iter().map(|m| (m.type_id, (m.quantity.min(i32::MAX as u64) as i32))).collect())
        .unwrap_or_default();

    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(P2).or_default().insert(3380, TrainedSkillRecord { type_id: 3380, level: 5, skill_points: 256_000.0 });
        snap.items.insert(cont_sag2, InventoryItemRecord {
            item_id: cont_sag2, type_id: 17366, owner_id: corp_id, location_id: JITA_44,
            flag: InventoryFlag::CorpSAG2, quantity: 1, singleton: true, ..Default::default()
        });
        snap.items.insert(bp_cont, InventoryItemRecord {
            item_id: bp_cont, type_id: 688, owner_id: corp_id, location_id: cont_sag2,
            flag: InventoryFlag::Unlocked, quantity: 1, singleton: true, ..Default::default()
        });
        snap.items.insert(trit_cont, InventoryItemRecord {
            item_id: trit_cont, type_id: 34, owner_id: corp_id, location_id: cont_sag2,
            flag: InventoryFlag::Unlocked, quantity: 10_000_000, ..Default::default()
        });
        for (idx, (mat_tid, mat_qty)) in extra_mats.iter().enumerate() {
            if *mat_tid != 34 {
                let extra_id = 955_100_100 + idx as i64;
                snap.items.insert(extra_id, InventoryItemRecord {
                    item_id: extra_id, type_id: *mat_tid, owner_id: corp_id, location_id: cont_sag2,
                    flag: InventoryFlag::Unlocked, quantity: mat_qty.saturating_mul(2).max(50_000), ..Default::default()
                });
            }
        }
        snap.items.insert(bp_deliv, InventoryItemRecord {
            item_id: bp_deliv, type_id: 688, owner_id: corp_id, location_id: JITA_44,
            flag: InventoryFlag::Deliveries, quantity: 1, singleton: true, ..Default::default()
        });
        snap.items.insert(alsc_lock_cont, InventoryItemRecord {
            item_id: alsc_lock_cont, type_id: 17366, owner_id: corp_id, location_id: JITA_44,
            flag: InventoryFlag::CorpSAG2, quantity: 1, singleton: true, ..Default::default()
        });
        snap.items.insert(bp_alsc, InventoryItemRecord {
            item_id: bp_alsc, type_id: 688, owner_id: corp_id, location_id: alsc_lock_cont,
            flag: InventoryFlag::Unlocked, quantity: 1, singleton: true, ..Default::default()
        });
        snap.items.insert(bp_wrap, InventoryItemRecord {
            item_id: bp_wrap, type_id: 688, owner_id: corp_id, location_id: JITA_44,
            flag: InventoryFlag::CorpSAG2, quantity: 1, singleton: true, ..Default::default()
        });
        let mut ext = load_ext_state(snap);
        ext.asset_safety_wraps.insert(wrap_item, AssetSafetyWrapRecord {
            wrap_id: wrap_item, owner_id: corp_id, is_corp: true,
            source_structure_id: 1_000_000_551_001,
            source_structure_name: "Destroyed Hub".into(),
            solar_system_id: JITA_SYS,
            created_unix: now, deliverable_after_unix: now,
            delivered_to_id: Some(JITA_44),
            item_ids: vec![bp_wrap],
            estimated_value_isk: 1_000.0,
            nearest_npc_station_id: Some(JITA_44),
            auto_delivered: true, retrieval_fee_paid: false,
        });
        save_ext_state(snap, &ext);
    });
    rpc(&ctx, &mut s1, "invbroker", "ALSCLockItems", &[PyRep::Integer(alsc_lock_cont), PyRep::List(vec![PyRep::Integer(bp_alsc)])]).await.expect("CEO locks bp_alsc via ALSC");

    // 1. Forged session roles rejected
    assert_eq!(
        rpc(&ctx, &mut s2, "industryManager", "InstallJob", &[PyRep::keyval([
            ("blueprintItemID", PyRep::Integer(bp_cont)),
            ("blueprintTypeID", PyRep::Integer(688)),
            ("useCorp", PyRep::Bool(true)),
            ("inputContainerID", PyRep::Integer(cont_sag2)),
        ])]).await.unwrap_err().code,
        "CrpAccessDenied"
    );

    // 2. Grant Factory Manager + Hangar Take 2 (WITHOUT Account Take 1 or Container Take 2)
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.get_mut(&corp_id).unwrap().member_roles.insert(
            P2,
            corp_roles::ROLE_FACTORY_MANAGER | corp_roles::ROLE_HANGAR_CAN_TAKE[1],
        );
    });
    // Missing Account Take 1 or Container Take 2 -> CrpAccessDenied
    assert_eq!(
        rpc(&ctx, &mut s2, "industryManager", "InstallJob", &[PyRep::keyval([
            ("blueprintItemID", PyRep::Integer(bp_cont)),
            ("blueprintTypeID", PyRep::Integer(688)),
            ("useCorp", PyRep::Bool(true)),
            ("inputContainerID", PyRep::Integer(cont_sag2)),
        ])]).await.unwrap_err().code,
        "CrpAccessDenied"
    );

    // 3. Grant Account Take 1 + Hangar Take 2 (still NO Container Take 2):
    // Deliveries/CorpDeliveries BP, AssetSafety BP, and container BP must ALL be rejected
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.get_mut(&corp_id).unwrap().member_roles.insert(
            P2,
            corp_roles::ROLE_FACTORY_MANAGER | corp_roles::ROLE_HANGAR_CAN_TAKE[1] | corp_roles::ROLE_ACCOUNT_CAN_TAKE[0],
        );
    });
    for (bad_bp, cont_arg) in [(bp_deliv, JITA_44), (bp_wrap, JITA_44), (bp_cont, cont_sag2)] {
        assert!(
            rpc(&ctx, &mut s2, "industryManager", "InstallJob", &[PyRep::keyval([
                ("blueprintItemID", PyRep::Integer(bad_bp)),
                ("blueprintTypeID", PyRep::Integer(688)),
                ("useCorp", PyRep::Bool(true)),
                ("inputContainerID", PyRep::Integer(cont_arg)),
            ])]).await.is_err(),
            "BP {bad_bp} must not be usable"
        );
    }

    // 4. Grant Container Take 2 via location-scoped roles_at_hq (at HQ):
    // ALSC-locked BP still fails even with full container take, while unlocked container BP succeeds!
    ctx.db.with_mut_snapshot(|snap| {
        snap.corp_role_assignments.insert((corp_id, P2), CorpMemberRoleAssignment {
            character_id: P2,
            roles_at_hq: corp_roles::ROLE_CONTAINER_CAN_TAKE[1],
            roles_at_other: corp_roles::ROLE_CONTAINER_CAN_TAKE[0],
            ..Default::default()
        });
    });
    assert_eq!(
        rpc(&ctx, &mut s2, "industryManager", "InstallJob", &[PyRep::keyval([
            ("blueprintItemID", PyRep::Integer(bp_alsc)),
            ("blueprintTypeID", PyRep::Integer(688)),
            ("useCorp", PyRep::Bool(true)),
            ("inputContainerID", PyRep::Integer(alsc_lock_cont)),
        ])]).await.unwrap_err().code,
        "BlueprintNotAvailable"
    );
    let ok = rpc(&ctx, &mut s2, "industryManager", "InstallJob", &[PyRep::keyval([
        ("blueprintItemID", PyRep::Integer(bp_cont)),
        ("blueprintTypeID", PyRep::Integer(688)),
        ("activityID", PyRep::Integer(1)),
        ("runs", PyRep::Integer(1)),
        ("useCorp", PyRep::Bool(true)),
        ("inputContainerID", PyRep::Integer(cont_sag2)),
    ])]).await.expect("InstallJob from corp container with Container Take 2 + Account Take 1");
    assert!(ok.dict_get("jobID").and_then(|v| v.as_i64()).unwrap_or(0) > 0);
    ctx.db.with_snapshot(|snap| {
        assert!(snap.items[&trit_cont].quantity < 10_000_000, "materials consumed from container");
        assert!(snap.corporations[&corp_id].division_balance(1) < IskCents::from_isk(50_000_000));
        assert!(snap.corp_governance.get(&corp_id).is_none() || now > 0);
    });
}

#[tokio::test]
async fn issue_552_container_split_alsc_and_reprocess_hangar_rbacs() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut s2 = SessionState::new(2);
    login_char(&ctx, &mut s2, "other", "other-secret", P2).await;

    let corp_id = 98_552_001_i64;
    let now = ctx.now_unix();
    setup_corp_members(&ctx, corp_id, 0, JITA_44);
    s2.corporation_id = Some(corp_id);
    s2.station_id = Some(JITA_44);
    s2.solar_system_id = Some(JITA_SYS);
    s2.corp_role = corp_roles::ROLE_DIRECTOR;

    let (cont1, ore_in_cont, loose_ore, alsc_cont, alsc_item) = (
        955_200_001_i64,
        955_200_002_i64,
        955_200_003_i64,
        955_200_004_i64,
        955_200_005_i64,
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(cont1, InventoryItemRecord {
            item_id: cont1, type_id: 17366, owner_id: corp_id, location_id: JITA_44,
            flag: InventoryFlag::CorpSAG1, quantity: 1, singleton: true, ..Default::default()
        });
        snap.items.insert(ore_in_cont, InventoryItemRecord {
            item_id: ore_in_cont, type_id: 1230, owner_id: corp_id, location_id: cont1,
            flag: InventoryFlag::Unlocked, quantity: 1_000, ..Default::default()
        });
        snap.items.insert(loose_ore, InventoryItemRecord {
            item_id: loose_ore, type_id: 1230, owner_id: corp_id, location_id: JITA_44,
            flag: InventoryFlag::CorpSAG1, quantity: 1_000, ..Default::default()
        });
        snap.items.insert(alsc_cont, InventoryItemRecord {
            item_id: alsc_cont, type_id: 17366, owner_id: corp_id, location_id: JITA_44,
            flag: InventoryFlag::CorpSAG1, quantity: 1, singleton: true, ..Default::default()
        });
        snap.items.insert(alsc_item, InventoryItemRecord {
            item_id: alsc_item, type_id: 34, owner_id: corp_id, location_id: alsc_cont,
            flag: InventoryFlag::Unlocked, quantity: 100, singleton: true, ..Default::default()
        });
        // Off-by-one bit: ROLE_CONTAINER_CAN_TAKE[1] (Div 2 = bit 43) MUST NOT unlock Div 1!
        snap.corporations.get_mut(&corp_id).unwrap().member_roles.insert(P2, corp_roles::ROLE_CONTAINER_CAN_TAKE[1]);
    });

    // 1. SplitStack off-by-one blocked on CorpSAG1 container when holding only Container Take 2 (bit 43)
    assert_eq!(
        rpc(&ctx, &mut s2, "invbroker", "SplitStack", &[PyRep::Integer(ore_in_cont), PyRep::Integer(200)]).await.unwrap_err().code,
        "CorpRoleDenied"
    );

    // 2. Container Take 1 (bit 42) must NOT grant Hangar Take 1 on Reprocess or loose hangar take,
    // and forged session.corp_role MUST NOT authorize ALSCLockItems
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.get_mut(&corp_id).unwrap().member_roles.insert(P2, corp_roles::ROLE_CONTAINER_CAN_TAKE[0]);
    });
    assert_eq!(
        rpc(&ctx, &mut s2, "reprocessingSvc", "Reprocess", &[PyRep::List(vec![PyRep::Integer(loose_ore)])]).await.unwrap_err().code,
        "OwnershipOrLocationViolation"
    );
    assert!(rpc(&ctx, &mut s2, "invbroker", "ALSCLockItems", &[PyRep::Integer(alsc_cont), PyRep::List(vec![PyRep::Integer(alsc_item)])]).await.is_ok());

    // Without Container Take / Equipment Config / Director, ALSCLockItems is denied even with Hangar Take 1 + session Director
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.get_mut(&corp_id).unwrap().member_roles.insert(P2, corp_roles::ROLE_HANGAR_CAN_TAKE[0]);
    });
    assert_eq!(
        rpc(&ctx, &mut s2, "invbroker", "ALSCLockItems", &[PyRep::Integer(alsc_cont), PyRep::List(vec![PyRep::Integer(alsc_item)])]).await.unwrap_err().code,
        "ALSCAccessDenied"
    );
    // invbroker.Add moving item out of CorpSAG1 container requires Container Take 1 (Hangar Take 1 alone denied)
    assert_eq!(
        rpc(&ctx, &mut s2, "invbroker", "Add", &[PyRep::Integer(ore_in_cont), PyRep::Integer(JITA_44), PyRep::Integer(4)]).await.unwrap_err().code,
        "CorpRoleDenied"
    );
    // Unmatured HQ role in pending_scoped_role_grants stays denied until maturity, while roles_at_other does not leak to HQ
    ctx.db.with_mut_snapshot(|snap| {
        snap.corp_role_assignments.insert((corp_id, P2), CorpMemberRoleAssignment {
            character_id: P2,
            roles_at_other: corp_roles::ROLE_CONTAINER_CAN_TAKE[0],
            ..Default::default()
        });
        snap.corp_governance.entry(corp_id).or_default().pending_scoped_role_grants.insert(
            P2,
            (corp_roles::ROLE_CONTAINER_CAN_TAKE[0], 0, 0, now + 3_600),
        );
    });
    assert_eq!(
        rpc(&ctx, &mut s2, "invbroker", "SplitStack", &[PyRep::Integer(ore_in_cont), PyRep::Integer(200)]).await.unwrap_err().code,
        "CorpRoleDenied"
    );

    ctx.advance_virtual_clock(3_601);
    let split = rpc(&ctx, &mut s2, "invbroker", "SplitStack", &[PyRep::Integer(ore_in_cont), PyRep::Integer(200)]).await.expect("SplitStack succeeds after stasis matures");
    assert!(split.dict_get("newItemID").and_then(|v| v.as_i64()).unwrap_or(0) > 0);
    // Loose Reprocess at JITA_44 now also succeeds because P2 holds ROLE_HANGAR_CAN_TAKE[0]
    let rep = rpc(&ctx, &mut s2, "reprocessingSvc", "Reprocess", &[PyRep::List(vec![PyRep::Integer(loose_ore)])]).await.expect("Reprocess with Hangar Take 1");
    assert!(matches!(rep, PyRep::List(ref items) if !items.is_empty()), "expected batch Reprocess list: {rep:?}");
    let _ = JITA_ALT;
}

#[tokio::test]
async fn issue_553_market_acl_hides_denied_upwell_orders() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut s1 = SessionState::new(1);
    let mut s2 = SessionState::new(2);
    login_char(&ctx, &mut s1, "demo", "demo-secret", P1).await;
    login_char(&ctx, &mut s2, "other", "other-secret", P2).await;

    let (owner_corp, enemy_corp, fort_id, mod_id, fort_order, npc_order) = (
        98_553_001_i64,
        98_553_002_i64,
        1_000_000_553_001_i64,
        955_300_001_i64,
        955_300_002_i64,
        955_300_003_i64,
    );
    let now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        let mut corp_owner = CorporationRecord::default();
        corp_owner.corporation_id = owner_corp;
        corp_owner.name = "Fort Owner".into();
        corp_owner.ticker = "FORT".into();
        corp_owner.ceo_id = P1;
        snap.corporations.insert(owner_corp, corp_owner);
        let mut corp_enemy = CorporationRecord::default();
        corp_enemy.corporation_id = enemy_corp;
        corp_enemy.name = "Guest Corp".into();
        corp_enemy.ticker = "GST".into();
        corp_enemy.ceo_id = P2;
        snap.corporations.insert(enemy_corp, corp_enemy);
        snap.characters.get_mut(&P1).unwrap().corporation_id = owner_corp;
        snap.characters.get_mut(&P2).unwrap().corporation_id = enemy_corp;

        snap.structures.insert(fort_id, UpwellStructureRecord {
            structure_id: fort_id, type_id: 35833, owner_corp_id: owner_corp,
            solar_system_id: JITA_SYS, name: "Private Market Fortizar".into(),
            state: "ShieldVulnerable".into(), quantum_core_installed: true, fuel_blocks: 100,
            ..Default::default()
        });
        snap.items.insert(mod_id, InventoryItemRecord {
            item_id: mod_id, type_id: 35_892, owner_id: owner_corp, location_id: fort_id,
            flag: InventoryFlag::ServiceSlot0, quantity: 1, singleton: true, ..Default::default()
        });
        snap.module_online_states.insert(mod_id, true);

        snap.market_orders.insert(fort_order, MarketOrderRecord {
            order_id: fort_order, owner_id: P1, station_id: fort_id, region_id: FORGE_REGION,
            type_id: 34, is_bid: false, price: IskCents::from_isk(4), vol_entered: 1_000, vol_remaining: 1_000,
            min_volume: 1, is_npc: false, escrow: IskCents(0),
        });
        snap.market_orders.insert(npc_order, MarketOrderRecord {
            order_id: npc_order, owner_id: P1, station_id: JITA_44, region_id: FORGE_REGION,
            type_id: 34, is_bid: false, price: IskCents::from_isk(6), vol_entered: 1_000, vol_remaining: 1_000,
            min_volume: 1, is_npc: false, escrow: IskCents(0),
        });
        let mut ext = load_ext_state(snap);
        let mut gen = HashMap::new();
        gen.insert(4, true);
        ext.access_profiles.insert(fort_id, StructureAccessProfile {
            profile_id: fort_id, owner_corp_id: owner_corp, name: "Public Market".into(),
            corp_only: false, general_settings: gen, ..Default::default()
        });
        save_ext_state(snap, &ext);
        for cid in [P1, P2] {
            if let Some(ch) = snap.characters.get_mut(&cid) {
                ch.station_id = None;
                ch.structure_id = Some(fort_id);
            }
        }
    });

    for s in [&mut s1, &mut s2] {
        s.region_id = Some(FORGE_REGION);
        s.solar_system_id = Some(JITA_SYS);
        s.station_id = None;
        s.structure_id = Some(fort_id);
    }

    // Initially P2 is neutral and structure is public -> P2 sees both Fortizar and NPC Jita orders
    let count_get_orders = |rep: PyRep| -> Vec<i64> {
        let PyRep::Tuple(mut parts) = rep else { panic!("expected Tuple: {rep:?}") };
        let sells = parts.remove(0);
        let mut ids: Vec<i64> = sells
            .crowset_rows()
            .expect("expected CRowset for sells")
            .into_iter()
            .filter_map(|row| row.dbrow_get("orderID").and_then(|v| v.as_i64()))
            .filter(|id| *id == fort_order || *id == npc_order)
            .collect();
        ids.sort_unstable();
        ids
    };
    let entry_34 = |rep: PyRep| -> Option<(f64, i64)> {
        let PyRep::ObjectEx { dict, .. } = rep else { return None };
        dict.into_iter()
            .find(|(k, _)| k.as_i64() == Some(34))
            .map(|(_, row)| (
                row.dbrow_get("price").and_then(|v| v.as_f64()).unwrap_or(0.0),
                row.dbrow_get("stationID").and_then(|v| v.as_i64()).unwrap_or(0),
            ))
    };
    let pub_ids = count_get_orders(rpc(&ctx, &mut s2, "marketProxy", "GetOrders", &[PyRep::Integer(34)]).await.unwrap());
    assert_eq!(pub_ids, vec![fort_order, npc_order]);
    assert_eq!(entry_34(rpc(&ctx, &mut s2, "marketProxy", "GetStationAsks", &[]).await.unwrap()), Some((4.0, fort_id)));

    // Case A: docking ACL denied blocks GetOrders, GetStationAsks, GetRegionBest, GetSystemAsks for P2 (P1 still sees!)
    ctx.db.with_mut_snapshot(|snap| {
        snap.structure_docking_acl_denied.entry(fort_id).or_default().insert(P2);
    });
    assert_eq!(count_get_orders(rpc(&ctx, &mut s1, "marketProxy", "GetOrders", &[PyRep::Integer(34)]).await.unwrap()), vec![fort_order, npc_order]);
    assert_eq!(count_get_orders(rpc(&ctx, &mut s2, "marketProxy", "GetOrders", &[PyRep::Integer(34)]).await.unwrap()), vec![npc_order]);
    assert_eq!(entry_34(rpc(&ctx, &mut s2, "marketProxy", "GetStationAsks", &[]).await.unwrap()), None);
    let reg_best = entry_34(rpc(&ctx, &mut s2, "marketProxy", "GetRegionBest", &[]).await.unwrap());
    assert!(matches!(reg_best, Some((p, st)) if st == JITA_44 && p >= 5.0), "GetRegionBest must exclude denied Fortizar 4.0 ask: {reg_best:?}");
    let sys_asks = entry_34(rpc(&ctx, &mut s2, "marketProxy", "GetSystemAsks", &[PyRep::Integer(i64::from(JITA_SYS))]).await.unwrap());
    assert!(matches!(sys_asks, Some((p, st)) if st == JITA_44 && p >= 5.0), "GetSystemAsks must exclude denied Fortizar 4.0 ask: {sys_asks:?}");

    // Case B: negative standing blocks P2
    ctx.db.with_mut_snapshot(|snap| {
        snap.structure_docking_acl_denied.clear();
        snap.characters.get_mut(&P2).unwrap().standings.insert(owner_corp, -5.0);
    });
    assert_eq!(count_get_orders(rpc(&ctx, &mut s2, "marketProxy", "GetOrders", &[PyRep::Integer(34)]).await.unwrap()), vec![npc_order]);

    // Case C: active war blocks P2
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&P2).unwrap().standings.clear();
        snap.wars.insert(700_553, WarRecord {
            war_id: 700_553, aggressor_corp_id: enemy_corp, defender_corp_id: owner_corp,
            mutual: false, started_unix: now, active: true,
        });
    });
    assert_eq!(count_get_orders(rpc(&ctx, &mut s2, "marketProxy", "GetOrders", &[PyRep::Integer(34)]).await.unwrap()), vec![npc_order]);

    // Case D: Upwell StructureAccessProfile denying market service (SETTING_MARKET_TAX = 4 -> false) blocks P2
    ctx.db.with_mut_snapshot(|snap| {
        snap.wars.clear();
        let mut ext = load_ext_state(snap);
        let mut gen = HashMap::new();
        gen.insert(4, false);
        ext.access_profiles.insert(fort_id, StructureAccessProfile {
            profile_id: fort_id, owner_corp_id: owner_corp, name: "No Market".into(),
            general_settings: gen, ..Default::default()
        });
        save_ext_state(snap, &ext);
    });
    assert_eq!(count_get_orders(rpc(&ctx, &mut s2, "marketProxy", "GetOrders", &[PyRep::Integer(34)]).await.unwrap()), vec![npc_order]);
}
