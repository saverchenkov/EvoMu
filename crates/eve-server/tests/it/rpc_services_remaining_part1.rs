//! Integration tests for 13 rock-solid `rpc:*` parity rows across `beyonce` and `contractProxy`.

use crate::common;
use eve_db::{
    BookmarkFolderRecord, ContractRecord, CorporationRecord, CosmicSignatureRecord, InventoryFlag,
    InventoryItemRecord, IskCents, UpwellStructureRecord, WorldRepository,
};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::HashMap;
use std::sync::Arc;

const P1: i64 = 90_000_001;
const P2: i64 = 90_000_002;
const JITA_44: i64 = 60_003_760;
const JITA_SYS: u32 = 30_000_142;

async fn login_select(ctx: &ServerContext, sid: u64, user: &str, pass: &str, cid: i64) -> SessionState {
    let mut s = SessionState::new(sid);
    let empty = HashMap::new();
    ctx.dispatch_rpc(&mut s, "authentication", None, "Login", &[PyRep::String(user.into()), PyRep::String(pass.into())], &empty, None).await.expect("Login");
    ctx.dispatch_rpc(&mut s, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(cid)], &empty, None).await.expect("SelectCharacterID");
    s
}

async fn call(ctx: &ServerContext, s: &mut SessionState, svc: &str, method: &str, args: &[PyRep]) -> PyRep {
    ctx.dispatch_rpc(s, svc, None, method, args, &HashMap::new(), None).await.unwrap_or_else(|e| panic!("{svc}.{method} failed: {e:?}"))
}

async fn call_err(ctx: &ServerContext, s: &mut SessionState, svc: &str, method: &str, args: &[PyRep]) -> String {
    ctx.dispatch_rpc(s, svc, None, method, args, &HashMap::new(), None).await.expect_err(&format!("{svc}.{method} should have failed")).code
}

fn mk_item(item_id: i64, type_id: u32, owner_id: i64, location_id: i64, flag: InventoryFlag, quantity: i32, singleton: bool) -> InventoryItemRecord {
    InventoryItemRecord { item_id, type_id, owner_id, location_id, flag, quantity, singleton, version: 1, ..Default::default() }
}

fn mk_structure(structure_id: i64, type_id: u32, owner_corp_id: i64) -> UpwellStructureRecord {
    UpwellStructureRecord {
        structure_id, type_id, owner_corp_id, solar_system_id: JITA_SYS, name: "Test Structure".into(),
        state: "ShieldVulnerable".into(), position: [100_000.0, 0.0, 0.0], fuel_blocks: 100,
        quantum_core_installed: true, shield_hp: 1.0, armor_hp: 1.0, hull_hp: 1.0,
        reinforced_until_unix: 0, last_damage_tick: 0, last_damage_unix: 0, self_destruct_at_unix: None,
    }
}

// parity: rpc:beyonce.BookmarkLocation, rpc:beyonce.BookmarkScanResult
#[tokio::test]
async fn test_rpc_sweep_group1a_bookmarks() {
    eve_testkit::attest(&["rpc:beyonce.BookmarkLocation", "rpc:beyonce.BookmarkScanResult"]);
    let ctx = ServerContext::with_repository(Arc::new(WorldRepository::with_demo_seed()));
    common::pin_clock(&ctx);
    let mut s1 = login_select(&ctx, 8101, "demo", "demo-secret", P1).await;

    let p1_folder_id = 501i64;
    let foreign_folder_id = 999i64;
    let bmk_item_id = 1_910_000_001i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.bookmark_folders.insert(
            p1_folder_id,
            BookmarkFolderRecord {
                folder_id: p1_folder_id,
                owner_id: P1,
                folder_name: "Fleet Pins".into(),
                is_shared: false,
                corporation_id: None,
                ..Default::default()
            },
        );
        snap.items.insert(
            bmk_item_id,
            mk_item(bmk_item_id, 34, P1, JITA_44, InventoryFlag::Hangar, 100, false),
        );
        snap.cosmic_signatures.entry(JITA_SYS).or_default().push(CosmicSignatureRecord {
            signature_id: "BMK-101".into(),
            solar_system_id: JITA_SYS,
            sig_group: "Data".into(),
            type_name: "Central Data Core".into(),
            position: [12_000.0, 24_000.0, 36_000.0],
            signal_strength: 1.0,
            wormhole_type_id: Some(9988),
            destination_system_id: None,
            created_unix: 1000,
            lifetime_seconds: 0,
            max_mass_kg: 0.0,
            remaining_mass_kg: 0.0,
            max_jump_mass_kg: 0.0,
            collapsed: false,
        });
    });

    // Exploit regression tests for BookmarkLocation:
    // 1. Specifying an inaccessible / unowned folder must fail with FolderNotFound
    assert_eq!(
        call_err(
            &ctx,
            &mut s1,
            "beyonce",
            "BookmarkLocation",
            &[
                PyRep::Integer(bmk_item_id),
                PyRep::Integer(foreign_folder_id),
                PyRep::String("Pin".into()),
                PyRep::String("Note".into()),
                PyRep::Integer(1),
                PyRep::Integer(JITA_SYS as i64)
            ]
        )
        .await,
        "FolderNotFound"
    );

    // 2. Legitimate BookmarkLocation with owned folder derives typeID (34) and locationID (JITA_44) from item:
    let bm_loc = call(
        &ctx,
        &mut s1,
        "beyonce",
        "BookmarkLocation",
        &[
            PyRep::Integer(bmk_item_id),
            PyRep::Integer(p1_folder_id),
            PyRep::String("Jita Trit".into()),
            PyRep::String("Pin".into()),
            PyRep::Integer(1),
            PyRep::Integer(JITA_SYS as i64),
        ],
    )
    .await;
    let PyRep::Tuple(bm_tuple) = bm_loc else { panic!("expected 8-tuple from beyonce.BookmarkLocation") };
    assert_eq!(bm_tuple.len(), 8);
    let bm_id = bm_tuple[0].as_i64().unwrap();
    assert!(bm_id > 0);
    assert_eq!(bm_tuple[1], PyRep::Integer(bmk_item_id));
    assert_eq!(bm_tuple[2], PyRep::Integer(34)); // derived from item type
    assert_eq!(bm_tuple[6], PyRep::Integer(JITA_44)); // derived from item location
    assert_eq!(bm_tuple[7], PyRep::Integer(p1_folder_id));

    // Exploit regression tests for BookmarkScanResult:
    // 1. Caller docked in station must fail with NotInSpace
    assert_eq!(
        call_err(
            &ctx,
            &mut s1,
            "beyonce",
            "BookmarkScanResult",
            &[
                PyRep::Integer(JITA_SYS as i64),
                PyRep::String("Scanned Core".into()),
                PyRep::String("Note".into()),
                PyRep::String("BMK-101".into()),
                PyRep::Integer(p1_folder_id),
            ]
        )
        .await,
        "NotInSpace"
    );

    // Undock into space
    call(&ctx, &mut s1, "stationSvc", "Undock", &[]).await;

    // 2. Specifying a different solar system must fail with NotInSameSystem
    assert_eq!(
        call_err(
            &ctx,
            &mut s1,
            "beyonce",
            "BookmarkScanResult",
            &[
                PyRep::Integer(30_000_144),
                PyRep::String("Scanned Core".into()),
                PyRep::String("Note".into()),
                PyRep::String("BMK-101".into()),
                PyRep::Integer(p1_folder_id),
            ]
        )
        .await,
        "NotInSameSystem"
    );

    // 3. Specifying an invalid / foreign folder must fail with FolderNotFound
    assert_eq!(
        call_err(
            &ctx,
            &mut s1,
            "beyonce",
            "BookmarkScanResult",
            &[
                PyRep::Integer(JITA_SYS as i64),
                PyRep::String("Scanned Core".into()),
                PyRep::String("Note".into()),
                PyRep::String("BMK-101".into()),
                PyRep::Integer(foreign_folder_id),
            ]
        )
        .await,
        "FolderNotFound"
    );

    // 4. Legitimate BookmarkScanResult in space derives signature typeID (9988) on server:
    let bm_scan = call(
        &ctx,
        &mut s1,
        "beyonce",
        "BookmarkScanResult",
        &[
            PyRep::Integer(JITA_SYS as i64),
            PyRep::String("Scanned Data".into()),
            PyRep::String("Note".into()),
            PyRep::String("BMK-101".into()),
            PyRep::Integer(p1_folder_id),
        ],
    )
    .await;
    let PyRep::Tuple(scan_tuple) = bm_scan else { panic!("expected 8-tuple from beyonce.BookmarkScanResult") };
    assert_eq!(scan_tuple.len(), 8);
    assert_eq!(scan_tuple[1], PyRep::Integer(JITA_SYS as i64));
    assert_eq!(scan_tuple[2], PyRep::Integer(9988)); // server-derived signature typeID
    assert_eq!(scan_tuple[3], PyRep::Real(12_000.0));
    assert_eq!(scan_tuple[6], PyRep::Integer(JITA_SYS as i64));
    assert_eq!(scan_tuple[7], PyRep::Integer(p1_folder_id));
}

// parity: rpc:contractProxy.SplitStack
#[tokio::test]
async fn test_rpc_sweep_group1b_stack() {
    eve_testkit::attest(&["rpc:contractProxy.SplitStack"]);
    let ctx = ServerContext::with_repository(Arc::new(WorldRepository::with_demo_seed()));
    common::pin_clock(&ctx);
    let mut s1 = login_select(&ctx, 8111, "demo", "demo-secret", P1).await;
    let mut s2 = login_select(&ctx, 8112, "other", "other-secret", P2).await;

    let stack_id = 1_910_000_010i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            stack_id,
            InventoryItemRecord {
                item_id: stack_id,
                type_id: 34,
                owner_id: P1,
                location_id: JITA_44,
                flag: InventoryFlag::Hangar,
                quantity: 100,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    // Exploit regression tests for SplitStack:
    // 1. Splitting with quantity <= 0 must fail
    assert_eq!(call_err(&ctx, &mut s1, "contractProxy", "SplitStack", &[PyRep::Integer(JITA_44), PyRep::Integer(stack_id), PyRep::Integer(0), PyRep::Bool(false), PyRep::Integer(4)]).await, "SplitFailed");
    // 2. Splitting with quantity >= stack size must fail
    assert_eq!(call_err(&ctx, &mut s1, "contractProxy", "SplitStack", &[PyRep::Integer(JITA_44), PyRep::Integer(stack_id), PyRep::Integer(100), PyRep::Bool(false), PyRep::Integer(4)]).await, "SplitFailed");
    assert_eq!(call_err(&ctx, &mut s1, "contractProxy", "SplitStack", &[PyRep::Integer(JITA_44), PyRep::Integer(stack_id), PyRep::Integer(150), PyRep::Bool(false), PyRep::Integer(4)]).await, "SplitFailed");
    // 3. Unauthorized pilot (P2) splitting P1's item must fail
    assert_eq!(call_err(&ctx, &mut s2, "contractProxy", "SplitStack", &[PyRep::Integer(JITA_44), PyRep::Integer(stack_id), PyRep::Integer(40), PyRep::Bool(false), PyRep::Integer(4)]).await, "SplitFailed");

    // Legitimate split succeeds
    let split = call(
        &ctx,
        &mut s1,
        "contractProxy",
        "SplitStack",
        &[PyRep::Integer(JITA_44), PyRep::Integer(stack_id), PyRep::Integer(40), PyRep::Bool(false), PyRep::Integer(4)],
    )
    .await;
    assert_eq!(split.dict_get("originalQuantity"), Some(&PyRep::Integer(60)));
}

// parity: rpc:beyonce.CmdAbandonLoot, rpc:beyonce.CmdTransferCorpAssetOwnership, rpc:beyonce.CmdTurboDock
#[tokio::test]
async fn test_rpc_sweep_group2_beyonce_commands() {
    eve_testkit::attest(&[
        "rpc:beyonce.CmdAbandonLoot",
        "rpc:beyonce.CmdTransferCorpAssetOwnership",
        "rpc:beyonce.CmdTurboDock",
    ]);

    let ctx = ServerContext::with_repository(Arc::new(WorldRepository::with_demo_seed()));
    common::pin_clock(&ctx);
    let mut s1 = login_select(&ctx, 8102, "demo", "demo-secret", P1).await;
    let mut s2 = login_select(&ctx, 8122, "other", "other-secret", P2).await;

    let loot_id = 1_920_000_001i64;
    let loot_hangar_id = 1_920_000_005i64;
    let st_id = 1_920_000_002i64;
    let corp_id = s1.corporation_id.unwrap_or(1_000_170);
    let target_player_corp = 98_000_001i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(loot_id, mk_item(loot_id, 23, P1, JITA_SYS as i64, InventoryFlag::None, 1, true));
        snap.items.insert(loot_hangar_id, mk_item(loot_hangar_id, 23, P1, JITA_44, InventoryFlag::Hangar, 1, true));
        snap.corporations.insert(
            target_player_corp,
            CorporationRecord {
                corporation_id: target_player_corp,
                name: "Allied Fleet Corp".into(),
                ticker: "AFC".into(),
                ceo_id: P2,
                tax_rate: 0.05,
                hq_station_id: Some(JITA_44),
                ..Default::default()
            },
        );
        snap.structures.insert(st_id, mk_structure(st_id, 35832, corp_id));
    });

    // Exploit regression tests for CmdAbandonLoot:
    // 1. Item in hangar (not in space) must fail
    assert_eq!(call_err(&ctx, &mut s1, "beyonce", "CmdAbandonLoot", &[PyRep::Integer(loot_hangar_id)]).await, "ItemNotInSpace");
    // 2. Item not owned by caller must fail
    assert_eq!(call_err(&ctx, &mut s2, "beyonce", "CmdAbandonLoot", &[PyRep::Integer(loot_id)]).await, "AccessDenied");
    // 3. Legitimate abandonment succeeds and clears owner
    assert_eq!(call(&ctx, &mut s1, "beyonce", "CmdAbandonLoot", &[PyRep::Integer(loot_id)]).await.dict_get("abandoned"), Some(&PyRep::Bool(true)));
    ctx.db.with_snapshot(|snap| assert_eq!(snap.items[&loot_id].owner_id, 0));

    // Exploit regression tests for CmdTransferCorpAssetOwnership:
    // 1. Caller docked in station must fail with NotInSpace (P2 #97)
    s1.corp_role = eve_db::corp_roles::ROLE_DIRECTOR;
    assert_eq!(call_err(&ctx, &mut s1, "beyonce", "CmdTransferCorpAssetOwnership", &[PyRep::Integer(st_id), PyRep::Integer(target_player_corp)]).await, "NotInSpace");

    // Undock into space
    call(&ctx, &mut s1, "stationSvc", "Undock", &[]).await;

    // 2. Unprivileged member (non-director) must fail with CrpAccessDenied
    s1.corp_role = 0;
    assert_eq!(call_err(&ctx, &mut s1, "beyonce", "CmdTransferCorpAssetOwnership", &[PyRep::Integer(st_id), PyRep::Integer(target_player_corp)]).await, "CrpAccessDenied");
    // 3. Transfer to NPC corporation (e.g. 1_000_045) must fail with InvalidTargetCorporation
    s1.corp_role = eve_db::corp_roles::ROLE_DIRECTOR;
    assert_eq!(call_err(&ctx, &mut s1, "beyonce", "CmdTransferCorpAssetOwnership", &[PyRep::Integer(st_id), PyRep::Integer(1_000_045)]).await, "InvalidTargetCorporation");
    // 4. Legitimate transfer by Director in space to player corp succeeds
    assert_eq!(
        call(&ctx, &mut s1, "beyonce", "CmdTransferCorpAssetOwnership", &[PyRep::Integer(st_id), PyRep::Integer(target_player_corp)]).await.dict_get("ownerID"),
        Some(&PyRep::Integer(target_player_corp))
    );

    // Exploit regression tests for CmdTurboDock:
    // 1. Non-GM pilot must fail
    assert_eq!(call_err(&ctx, &mut s1, "beyonce", "CmdTurboDock", &[PyRep::Integer(JITA_44)]).await, "GmRoleRequired");
    // 2. GM pilot succeeds
    s1.role = eve_db::service_roles::ROLE_GM_DEFAULT;
    assert_eq!(call(&ctx, &mut s1, "beyonce", "CmdTurboDock", &[PyRep::Integer(JITA_44)]).await.dict_get("docked"), Some(&PyRep::Bool(true)));
    assert_eq!(s1.station_id, Some(JITA_44));
    assert_eq!(call(&ctx, &mut s1, "beyonce", "CmdTurboDock", &[PyRep::Integer(st_id)]).await.dict_get("docked"), Some(&PyRep::Bool(true)));
    assert_eq!(s1.structure_id, Some(st_id));
    assert_eq!(s1.station_id, None);
}

// parity: rpc:contractProxy.CollectMyPageInfo, rpc:contractProxy.FinishAuction, rpc:contractProxy.GetCourierContractFromItemID
// parity: rpc:contractProxy.GetItemsInContainer, rpc:contractProxy.GetMyContractEscrow, rpc:contractProxy.GetNumItemsInContainers, rpc:contractProxy.NumOutstandingContracts
#[tokio::test]
async fn test_rpc_sweep_group3_contract_proxy_methods() {
    eve_testkit::attest(&[
        "rpc:contractProxy.CollectMyPageInfo",
        "rpc:contractProxy.FinishAuction",
        "rpc:contractProxy.GetCourierContractFromItemID",
        "rpc:contractProxy.GetItemsInContainer",
        "rpc:contractProxy.GetMyContractEscrow",
        "rpc:contractProxy.GetNumItemsInContainers",
        "rpc:contractProxy.NumOutstandingContracts",
    ]);

    let repo = Arc::new(WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_repository(repo);
    common::pin_clock(&ctx);
    let mut s1 = login_select(&ctx, 8103, "demo", "demo-secret", P1).await;

    let cid_del = 1_930_000_001i64;
    let cid_auc = 1_930_000_002i64;
    let cid_unexpired = 1_930_000_006i64;
    let cid_unauth = 1_930_000_007i64;
    let item_auc = 1_930_000_003i64;
    let box_id = 1_930_000_004i64;
    let box_child = 1_930_000_005i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            item_auc,
            InventoryItemRecord {
                item_id: item_auc,
                type_id: 34,
                owner_id: P1,
                location_id: JITA_44,
                flag: InventoryFlag::Locked,
                quantity: 500,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            box_id,
            InventoryItemRecord {
                item_id: box_id,
                type_id: 3293,
                owner_id: P1,
                location_id: JITA_44,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            box_child,
            InventoryItemRecord {
                item_id: box_child,
                type_id: 34,
                owner_id: P1,
                location_id: box_id,
                flag: InventoryFlag::Cargo,
                quantity: 25,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.contracts.insert(
            cid_del,
            ContractRecord {
                contract_id: cid_del,
                issuer_id: P1,
                assignee_id: 0,
                acceptor_id: None,
                contract_type: "Courier".into(),
                availability: "Public".into(),
                status: "Outstanding".into(),
                start_station_id: JITA_44,
                end_station_id: JITA_44,
                price: IskCents(0),
                reward: IskCents::from_f64_isk(50_000.0),
                collateral: IskCents(0),
                item_ids: vec![box_id],
                requested_items: vec![],
                current_bid: IskCents(0),
                current_bidder_id: None,
                title: "Courier".into(),
                date_issued_unix: 1000,
                expiration_unix: 2_000_000_000,
                ..Default::default()
            },
        );
        snap.contracts.insert(
            cid_auc,
            ContractRecord {
                contract_id: cid_auc,
                issuer_id: P1,
                assignee_id: 0,
                acceptor_id: None,
                contract_type: "Auction".into(),
                availability: "Public".into(),
                status: "Outstanding".into(),
                start_station_id: JITA_44,
                end_station_id: JITA_44,
                price: IskCents::from_f64_isk(100_000.0),
                reward: IskCents(0),
                collateral: IskCents(0),
                item_ids: vec![item_auc],
                requested_items: vec![],
                current_bid: IskCents::from_f64_isk(150_000.0),
                current_bidder_id: Some(P2),
                title: "Auction".into(),
                date_issued_unix: 100,
                expiration_unix: 500,
                ..Default::default()
            },
        );
        snap.contracts.insert(
            cid_unexpired,
            ContractRecord {
                contract_id: cid_unexpired,
                issuer_id: P1,
                assignee_id: 0,
                acceptor_id: None,
                contract_type: "Auction".into(),
                availability: "Public".into(),
                status: "Outstanding".into(),
                start_station_id: JITA_44,
                end_station_id: JITA_44,
                price: IskCents::from_f64_isk(100_000.0),
                reward: IskCents(0),
                collateral: IskCents(0),
                item_ids: vec![],
                requested_items: vec![],
                current_bid: IskCents(0),
                current_bidder_id: None,
                title: "Unexpired Auction".into(),
                date_issued_unix: 100,
                expiration_unix: 2_000_000_000,
                ..Default::default()
            },
        );
        snap.contracts.insert(
            cid_unauth,
            ContractRecord {
                contract_id: cid_unauth,
                issuer_id: 999_999,
                assignee_id: 0,
                acceptor_id: None,
                contract_type: "Auction".into(),
                availability: "Public".into(),
                status: "Outstanding".into(),
                start_station_id: JITA_44,
                end_station_id: JITA_44,
                price: IskCents::from_f64_isk(100_000.0),
                reward: IskCents(0),
                collateral: IskCents(0),
                item_ids: vec![],
                requested_items: vec![],
                current_bid: IskCents(0),
                current_bidder_id: None,
                title: "Other Auction".into(),
                date_issued_unix: 100,
                expiration_unix: 500,
                ..Default::default()
            },
        );
    });

    let page = call(&ctx, &mut s1, "contractProxy", "CollectMyPageInfo", &[]).await;
    assert_eq!(page.dict_get("numOutstandingContractsNonCorp"), Some(&PyRep::Integer(3)));
    assert_eq!(call(&ctx, &mut s1, "contractProxy", "NumOutstandingContracts", &[]).await, PyRep::Integer(3));
    let escrow = call(&ctx, &mut s1, "contractProxy", "GetMyContractEscrow", &[]).await;
    assert_eq!(escrow.dict_get("iskEscrow"), Some(&PyRep::Real(50_000.0)));
    assert_eq!(escrow.dict_get("itemsEscrow"), Some(&PyRep::Integer(2)));

    assert_eq!(call(&ctx, &mut s1, "contractProxy", "GetCourierContractFromItemID", &[PyRep::Integer(box_id)]).await.dict_get("contractID"), Some(&PyRep::Integer(cid_del)));
    assert_eq!(call(&ctx, &mut s1, "contractProxy", "GetCourierContractFromItemID", &[PyRep::Integer(item_auc)]).await, PyRep::None);

    let in_box = call(&ctx, &mut s1, "contractProxy", "GetItemsInContainer", &[PyRep::Integer(JITA_44), PyRep::Integer(box_id), PyRep::Bool(false), PyRep::Integer(4)]).await;
    assert!(matches!(in_box, PyRep::List(ref l) if l.len() == 1));
    let counts = call(&ctx, &mut s1, "contractProxy", "GetNumItemsInContainers", &[PyRep::Integer(JITA_44), PyRep::List(vec![PyRep::Integer(box_id)]), PyRep::Bool(false), PyRep::Integer(4)]).await;
    assert!(matches!(counts, PyRep::Dict(ref d) if d.contains(&(PyRep::Integer(box_id), PyRep::Integer(1)))));

    // Exploit regression tests for FinishAuction:
    // 1. Finishing unexpired auction must fail with AuctionNotExpired
    assert_eq!(call_err(&ctx, &mut s1, "contractProxy", "FinishAuction", &[PyRep::Integer(cid_unexpired), PyRep::Bool(true)]).await, "AuctionNotExpired");
    // 2. Finishing non-auction contract (e.g. Courier) must fail with InvalidContractType
    assert_eq!(call_err(&ctx, &mut s1, "contractProxy", "FinishAuction", &[PyRep::Integer(cid_del), PyRep::Bool(true)]).await, "InvalidContractType");
    // 3. Unauthorized caller (P1 who is neither issuer nor bidder on cid_unauth) must fail with ContractOwnershipViolation
    assert_eq!(call_err(&ctx, &mut s1, "contractProxy", "FinishAuction", &[PyRep::Integer(cid_unauth), PyRep::Bool(true)]).await, "ContractOwnershipViolation");
    // 4. Legitimate finish by issuer or winner after expiration succeeds
    assert_eq!(call(&ctx, &mut s1, "contractProxy", "FinishAuction", &[PyRep::Integer(cid_auc), PyRep::Bool(true)]).await, PyRep::Bool(true));
    // 5. Finishing already finished auction fails with InvalidContractState
    assert_eq!(call_err(&ctx, &mut s1, "contractProxy", "FinishAuction", &[PyRep::Integer(cid_auc), PyRep::Bool(true)]).await, "InvalidContractState");
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.contracts[&cid_auc].status, "Finished");
        assert_eq!(snap.items[&item_auc].owner_id, P2);
    });
}
