//! Behavioral edge-case tests for `social_corp_fleet.rs`.

#![forbid(unsafe_code)]

use super::{handle, sync_all_alliance_timers};
use crate::router::ServerContext;
use crate::services::wallet_services_ext::{bill_note_id, save_json_note, CorpBillRecord};
use eve_db::{
    CorpWalletDivisionRecord, CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents,
    ServerClock, UpwellStructureRecord, VirtualServerClock, WorldRepository,
    WorldStateSnapshot,
};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

async fn call_social(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    method: &str,
    args: Vec<PyRep>,
) -> Result<PyRep, eve_proto::BridgeRpcError> {
    handle(ctx, sess, svc, method, &args, &HashMap::new(), None)
        .await
        .unwrap_or_else(|| panic!("unhandled {svc}.{method}"))
}

fn setup_sandbox() -> (
    ServerContext,
    Arc<VirtualServerClock>,
    SessionState,
    SessionState,
    SessionState,
    i64,
) {
    let clock = Arc::new(VirtualServerClock::with_fixed_base(1_700_000_000));
    let ctx = ServerContext::with_sde_and_repository(
        shared_sde(),
        Arc::new(WorldRepository::with_demo_seed()),
    );
    ctx.set_clock(clock.clone());

    let exec_corp = 98_200_001_i64;
    let member_corp = 98_200_002_i64;
    let creditor_corp = 98_200_003_i64;
    let exec_ceo = 91_200_001_i64;
    let line_member = 91_200_002_i64;
    let outsider_ceo = 91_200_003_i64;
    let station_id = 60_003_760_i64;
    let sys_id = 30_000_142_u32;

    ctx.db.with_mut_snapshot(|snap: &mut WorldStateSnapshot| {
        let tpl = snap.characters.values().next().unwrap().clone();
        for (cid, name, corp_id, bal_isk) in [
            (exec_ceo, "Exec CEO", exec_corp, 5_000_000_000_i64),
            (line_member, "Line Member", exec_corp, 100_000_000_i64),
            (outsider_ceo, "Outsider CEO", member_corp, 5_000_000_000_i64),
        ] {
            let mut ch = tpl.clone();
            ch.character_id = cid;
            ch.name = name.to_string();
            ch.corporation_id = corp_id;
            ch.alliance_id = None;
            ch.station_id = Some(station_id);
            ch.structure_id = None;
            ch.solar_system_id = sys_id;
            ch.constellation_id = 20_000_020;
            ch.region_id = 10_000_002;
            ch.active_ship_id = 55_200_000 + cid;
            ch.balance = IskCents::from_isk(bal_isk);
            snap.characters.insert(cid, ch);
        }
        for (corp_id, ceo_id, ticker, members, roles) in [
            (
                exec_corp,
                exec_ceo,
                "EXEC",
                vec![exec_ceo, line_member],
                HashMap::from([(exec_ceo, u64::MAX), (line_member, 0u64)]),
            ),
            (
                member_corp,
                outsider_ceo,
                "MEMB",
                vec![outsider_ceo],
                HashMap::from([(outsider_ceo, u64::MAX)]),
            ),
            (
                creditor_corp,
                outsider_ceo,
                "CRED",
                vec![outsider_ceo],
                HashMap::from([(outsider_ceo, u64::MAX)]),
            ),
        ] {
            let mut divs =
                CorporationRecord::default_wallet_divisions(IskCents::from_isk(5_000_000_000));
            divs[0] = CorpWalletDivisionRecord {
                division_id: 1,
                name: "Master Wallet".into(),
                balance: IskCents::from_isk(5_000_000_000),
            };
            divs[1] = CorpWalletDivisionRecord {
                division_id: 2,
                name: "2nd Wallet".into(),
                balance: IskCents::from_isk(100_000),
            };
            snap.corporations.insert(
                corp_id,
                { let mut __corp = CorporationRecord::default(); __corp.corporation_id = corp_id; __corp.name = format!("Corp {corp_id}"); __corp.ticker = ticker.into(); __corp.ceo_id = ceo_id; __corp.alliance_id = None; __corp.tax_rate = 0.05; __corp.members = members.into_iter().collect::<HashSet<_>>(); __corp.member_roles = roles; __corp.applications = HashMap::new(); __corp.wallet_divisions = divs; __corp.description = String::new(); __corp.url = String::new(); __corp.hq_station_id = Some(station_id); let __mw = IskCents::from_isk(5_000_000_000); if __mw.0 != 0 { __corp.set_division_balance(1, __mw); } else { __corp.ensure_wallet_divisions(); } __corp },
            );
        }
    });

    let make_sess = |sid: u64, cid: i64, corp_id: i64, corp_role: u64| {
        let mut s = SessionState::new(sid);
        s.user_id = Some(cid as u64);
        s.character_id = Some(cid);
        s.corporation_id = Some(corp_id);
        s.corp_role = corp_role;
        s.station_id = Some(station_id);
        s.solar_system_id = Some(sys_id);
        s.ship_id = Some(55_200_000 + cid);
        s
    };

    let mut exec = make_sess(1, exec_ceo, exec_corp, u64::MAX);
    let line = make_sess(2, line_member, exec_corp, 0);
    let outsider = make_sess(3, outsider_ceo, member_corp, u64::MAX);

    let rt = tokio::runtime::Handle::current();
    let aid = tokio::task::block_in_place(|| {
        rt.block_on(async {
            let res = call_social(
                &ctx,
                &mut exec,
                "corpRegistry",
                "CreateAlliance",
                vec![
                    PyRep::String("Edge Alliance".into()),
                    PyRep::String("EDGA".into()),
                    PyRep::String("Edge test alliance".into()),
                    PyRep::String("https://edge.local".into()),
                ],
            )
            .await
            .expect("CreateAlliance");
            res.dict_get("allianceID")
                .and_then(|v| v.as_i64())
                .expect("allianceID")
        })
    });

    let mut line_synced = line;
    line_synced.alliance_id = Some(aid);
    (ctx, clock, exec, line_synced, outsider, aid)
}

#[tokio::test(flavor = "multi_thread")]
async fn deliver_to_corp_hangar_and_member_enforce_all_validation_and_role_guards() {
    let (ctx, _clock, mut exec, mut line, _outsider, _aid) = setup_sandbox();
    let station_id = 60_003_760_i64;
    let exec_corp = 98_200_001_i64;
    let exec_ceo = 91_200_001_i64;
    let line_member = 91_200_002_i64;

    let (
        normal_item,
        container_item,
        inside_container_item,
        courier_item,
        fitted_item,
        locked_item,
        foreign_item,
        elsewhere_item,
        corp_div2_item,
        active_ship_item,
    ) = (
        700_101_i64,
        700_102_i64,
        700_103_i64,
        700_104_i64,
        700_105_i64,
        700_106_i64,
        700_107_i64,
        700_108_i64,
        700_109_i64,
        55_200_000 + exec_ceo,
    );

    ctx.db.with_mut_snapshot(|snap| {
        let mut insert_item =
            |item_id: i64, type_id: u32, owner_id: i64, location_id: i64, flag: InventoryFlag| {
                snap.items.insert(
                    item_id,
                    InventoryItemRecord {
                        item_id,
                        type_id,
                        owner_id,
                        location_id,
                        flag,
                        quantity: 10,
                        singleton: false,
                        version: 1,
                        ..Default::default()
                    },
                );
            };
        insert_item(active_ship_item, 587, exec_ceo, station_id, InventoryFlag::Hangar);
        insert_item(normal_item, 34, exec_ceo, station_id, InventoryFlag::Hangar);
        // 3293 = Medium Standard Container (group 12 cargo container)
        insert_item(container_item, 3293, exec_ceo, station_id, InventoryFlag::Hangar);
        insert_item(inside_container_item, 34, exec_ceo, container_item, InventoryFlag::None);
        insert_item(courier_item, 34, exec_ceo, station_id, InventoryFlag::Hangar);
        snap.item_custom_names
            .insert(courier_item, "courier_cargo:999".into());
        insert_item(fitted_item, 34, exec_ceo, active_ship_item, InventoryFlag::HiSlot0);
        insert_item(locked_item, 34, exec_ceo, station_id, InventoryFlag::Locked);
        insert_item(foreign_item, 34, 91_200_003, station_id, InventoryFlag::Hangar);
        insert_item(elsewhere_item, 34, exec_ceo, 60_003_761, InventoryFlag::Hangar);
        insert_item(corp_div2_item, 34, exec_corp, station_id, InventoryFlag::CorpSAG2);
    });

    // 1. NotDocked when neither station_id nor structure_id is present
    let mut undocked = exec.clone();
    undocked.station_id = None;
    undocked.structure_id = None;
    assert_eq!(
        call_social(
            &ctx,
            &mut undocked,
            "invbroker",
            "DeliverToCorpHangar",
            vec![PyRep::Integer(normal_item), PyRep::Integer(1)],
        )
        .await
        .unwrap_err()
        .code,
        "NotDocked"
    );

    // 2. ItemNotFound for empty list and unknown item ID
    assert_eq!(
        call_social(
            &ctx,
            &mut exec,
            "invbroker",
            "DeliverToCorpHangar",
            vec![PyRep::List(vec![]), PyRep::Integer(1)],
        )
        .await
        .unwrap_err()
        .code,
        "ItemNotFound"
    );
    assert_eq!(
        call_social(
            &ctx,
            &mut exec,
            "invbroker",
            "DeliverToCorpHangar",
            vec![PyRep::Integer(999_999_999), PyRep::Integer(1)],
        )
        .await
        .unwrap_err()
        .code,
        "ItemNotFound"
    );

    // 3. ItemInsideContainer when parent is a container
    assert_eq!(
        call_social(
            &ctx,
            &mut exec,
            "invbroker",
            "DeliverToCorpHangar",
            vec![PyRep::Integer(inside_container_item), PyRep::Integer(1)],
        )
        .await
        .unwrap_err()
        .code,
        "ItemInsideContainer"
    );

    // 4. Courier cargo, fitted item, and locked item -> ItemCannotBeTransferred
    for bad_id in [courier_item, fitted_item, locked_item] {
        assert_eq!(
            call_social(
                &ctx,
                &mut exec,
                "invbroker",
                "DeliverToCorpHangar",
                vec![PyRep::Integer(bad_id), PyRep::Integer(1)],
            )
            .await
            .unwrap_err()
            .code,
            "ItemCannotBeTransferred"
        );
    }

    // 5. TargetNotInCorporation for DeliverToCorpMember to outsider
    assert_eq!(
        call_social(
            &ctx,
            &mut exec,
            "invbroker",
            "DeliverToCorpMember",
            vec![PyRep::Integer(normal_item), PyRep::Integer(91_200_003)],
        )
        .await
        .unwrap_err()
        .code,
        "TargetNotInCorporation"
    );

    // 6. CannotDeliverActiveShip, ItemNotOwned, ItemNotAtLocation
    assert_eq!(
        call_social(
            &ctx,
            &mut exec,
            "invbroker",
            "DeliverToCorpHangar",
            vec![PyRep::Integer(active_ship_item), PyRep::Integer(1)],
        )
        .await
        .unwrap_err()
        .code,
        "CannotDeliverActiveShip"
    );
    assert_eq!(
        call_social(
            &ctx,
            &mut exec,
            "invbroker",
            "DeliverToCorpHangar",
            vec![PyRep::Integer(foreign_item), PyRep::Integer(1)],
        )
        .await
        .unwrap_err()
        .code,
        "ItemNotOwned"
    );
    assert_eq!(
        call_social(
            &ctx,
            &mut exec,
            "invbroker",
            "DeliverToCorpHangar",
            vec![PyRep::Integer(elsewhere_item), PyRep::Integer(1)],
        )
        .await
        .unwrap_err()
        .code,
        "ItemNotAtLocation"
    );

    // 7. CorpRoleDenied when line member without Trader/Accountant/Director delivers corp item
    assert_eq!(
        call_social(
            &ctx,
            &mut line,
            "invbroker",
            "DeliverToCorpMember",
            vec![PyRep::Integer(corp_div2_item), PyRep::Integer(line_member)],
        )
        .await
        .unwrap_err()
        .code,
        "CorpRoleDenied"
    );

    // Trader role without CorpSAG2 take role still denied for division 2 corp item
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations
            .get_mut(&exec_corp)
            .unwrap()
            .member_roles
            .insert(line_member, eve_db::corp_roles::ROLE_TRADER);
    });
    assert_eq!(
        call_social(
            &ctx,
            &mut line,
            "invbroker",
            "DeliverToCorpMember",
            vec![PyRep::Integer(corp_div2_item), PyRep::Integer(line_member)],
        )
        .await
        .unwrap_err()
        .code,
        "CorpRoleDenied"
    );

    // 8. Valid DeliverToCorpHangar into division 2 -> owner=corp, flag=117 (CorpSAG2)
    let res_hangar = call_social(
        &ctx,
        &mut exec,
        "invbroker",
        "DeliverToCorpHangar",
        vec![PyRep::List(vec![PyRep::Integer(normal_item)]), PyRep::Integer(2)],
    )
    .await
    .unwrap();
    assert_eq!(res_hangar.dict_get("ownerID").and_then(|v| v.as_i64()), Some(exec_corp));
    assert_eq!(res_hangar.dict_get("flagID").and_then(|v| v.as_i64()), Some(117));
    assert_eq!(res_hangar.dict_get("delivered").and_then(|v| v.as_bool()), Some(true));

    // 9. Valid DeliverToCorpMember (by CEO from CorpSAG2 to line_member) -> owner=line_member, flag=Deliveries (4)
    let res_mem = call_social(
        &ctx,
        &mut exec,
        "invbroker",
        "DeliverToCorpMember",
        vec![PyRep::Integer(corp_div2_item), PyRep::Integer(line_member)],
    )
    .await
    .unwrap();
    assert_eq!(res_mem.dict_get("ownerID").and_then(|v| v.as_i64()), Some(line_member));
    assert_eq!(
        res_mem.dict_get("flagID").and_then(|v| v.as_i64()),
        Some(InventoryFlag::Deliveries as i64)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn alliance_capital_system_prime_hour_transitions_and_sov_structures() {
    let (ctx, clock, mut exec, mut line, _outsider, aid) = setup_sandbox();

    // 1. CancelCapitalSystemTransition before any transition -> NoCapitalSystemTransition
    assert_eq!(
        call_social(&ctx, &mut exec, "allianceRegistry", "CancelCapitalSystemTransition", vec![])
            .await
            .unwrap_err()
            .code,
        "NoCapitalSystemTransition"
    );

    // 2. SetCapitalSystem validation: missing arg -> InvalidArgument, unknown system -> InvalidSolarSystem
    assert_eq!(
        call_social(&ctx, &mut exec, "allianceRegistry", "SetCapitalSystem", vec![])
            .await
            .unwrap_err()
            .code,
        "InvalidArgument"
    );
    assert_eq!(
        call_social(
            &ctx,
            &mut exec,
            "allianceRegistry",
            "SetCapitalSystem",
            vec![PyRep::Integer(99_999_999)],
        )
        .await
        .unwrap_err()
        .code,
        "InvalidSolarSystem"
    );

    // 3. Valid SetCapitalSystem (30_002_187 Amarr) & CancelCapitalSystemTransition
    let target_sys = *ctx.sde.solar_systems.keys().find(|&&id| id != 30_000_142).unwrap();
    assert_eq!(
        call_social(
            &ctx,
            &mut exec,
            "allianceRegistry",
            "SetCapitalSystem",
            vec![PyRep::Integer(i64::from(target_sys))],
        )
        .await
        .unwrap(),
        PyRep::Bool(true)
    );
    let cap_pending = call_social(&ctx, &mut line, "allianceRegistry", "GetCapitalSystemInfo", vec![])
        .await
        .unwrap();
    assert_eq!(
        cap_pending.dict_get("newCapitalSystem").and_then(|v| v.as_i64()),
        Some(i64::from(target_sys))
    );
    assert!(
        cap_pending
            .dict_get("newCapitalSystemValidAfter")
            .and_then(|v| v.as_i64())
            .unwrap_or(0)
            > 0
    );

    // Cancel clears pending transition
    assert_eq!(
        call_social(&ctx, &mut exec, "allianceRegistry", "CancelCapitalSystemTransition", vec![])
            .await
            .unwrap(),
        PyRep::Bool(true)
    );
    let cap_cancelled = call_social(&ctx, &mut exec, "allianceRegistry", "GetCapitalSystemInfo", vec![])
        .await
        .unwrap();
    assert_eq!(cap_cancelled.dict_get("newCapitalSystem"), Some(&PyRep::None));

    // Re-set and advance 7 days to verify auto-promotion
    call_social(
        &ctx,
        &mut exec,
        "allianceRegistry",
        "SetCapitalSystem",
        vec![PyRep::Integer(i64::from(target_sys))],
    )
    .await
    .unwrap();
    clock.advance_secs(7 * 86_400 + 1);
    let cap_promoted = call_social(&ctx, &mut exec, "allianceRegistry", "GetCapitalSystemInfo", vec![])
        .await
        .unwrap();
    assert_eq!(
        cap_promoted.dict_get("currentCapitalSystem").and_then(|v| v.as_i64()),
        Some(i64::from(target_sys))
    );
    assert_eq!(cap_promoted.dict_get("newCapitalSystem"), Some(&PyRep::None));

    // 4. SetPrimeHour validation: missing -> InvalidArgument, -1 / 24 -> InvalidPrimeHour, 5 -> promotes after 48h
    assert_eq!(
        call_social(&ctx, &mut exec, "allianceRegistry", "SetPrimeHour", vec![])
            .await
            .unwrap_err()
            .code,
        "InvalidArgument"
    );
    for bad_hr in [-1_i64, 24_i64] {
        assert_eq!(
            call_social(
                &ctx,
                &mut exec,
                "allianceRegistry",
                "SetPrimeHour",
                vec![PyRep::Integer(bad_hr)],
            )
            .await
            .unwrap_err()
            .code,
            "InvalidPrimeHour"
        );
    }
    assert_eq!(
        call_social(
            &ctx,
            &mut exec,
            "allianceRegistry",
            "SetPrimeHour",
            vec![PyRep::Integer(5)],
        )
        .await
        .unwrap(),
        PyRep::Bool(true)
    );
    let prime_pending = call_social(&ctx, &mut exec, "allianceRegistry", "GetPrimeTimeInfo", vec![])
        .await
        .unwrap();
    assert_eq!(prime_pending.dict_get("newPrimeHour").and_then(|v| v.as_i64()), Some(5));
    clock.advance_secs(48 * 3600 + 1);
    let prime_promoted = call_social(&ctx, &mut exec, "allianceRegistry", "GetPrimeTimeInfo", vec![])
        .await
        .unwrap();
    assert_eq!(
        prime_promoted.dict_get("currentPrimeHour").and_then(|v| v.as_i64()),
        Some(5)
    );

    // 5. GetAllianceSovereigntyStructuresInfo with sov hub, adm vulnerability window, command nodes & scores
    let hub_id = 88_001_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            hub_id,
            UpwellStructureRecord {
                structure_id: hub_id,
                type_id: 81242, // Sovereignty Hub
                owner_corp_id: 98_200_001,
                solar_system_id: 30_000_142,
                position: [1000.0, 0.0, 0.0],
                name: "Test Sov Hub".into(),
                state: "ShieldVulnerable".into(),
                reinforced_until_unix: 0,
                shield_hp: 1000.0,
                armor_hp: 1000.0,
                hull_hp: 1000.0,
                quantum_core_installed: true,
                fuel_blocks: 100,
                ..Default::default()
            },
        );
        snap.sov_runtime
            .adm_vulnerability_windows
            .insert(hub_id, (1_700_100_000, 1_700_114_400));
        snap.sov_runtime
            .command_nodes
            .insert(99_001, (hub_id, 30_000_142, aid));
        snap.sov_runtime
            .command_node_scores
            .insert(hub_id, vec![(aid, 0.65)]);
    });
    let sov_info = call_social(
        &ctx,
        &mut exec,
        "allianceRegistry",
        "GetAllianceSovereigntyStructuresInfo",
        vec![],
    )
    .await
    .unwrap();
    let PyRep::Tuple(parts) = sov_info else {
        panic!("expected 3-tuple from GetAllianceSovereigntyStructuresInfo");
    };
    assert_eq!(parts.len(), 3);
    let PyRep::List(structs) = &parts[0] else { panic!("expected structs list") };
    assert_eq!(structs.len(), 1);
    assert_eq!(structs[0].dict_get("itemID").and_then(|v| v.as_i64()), Some(hub_id));
    assert_eq!(structs[0].dict_get("allianceID").and_then(|v| v.as_i64()), Some(aid));
    let PyRep::List(campaigns) = &parts[1] else { panic!("expected campaigns list") };
    assert_eq!(campaigns.len(), 1);
    assert_eq!(campaigns[0].dict_get("campaignID").and_then(|v| v.as_i64()), Some(hub_id));
    let PyRep::List(scores) = &parts[2] else { panic!("expected scores list") };
    assert_eq!(scores.len(), 1);
    assert_eq!(scores[0].dict_get("score").and_then(|v| v.as_f64()), Some(0.65));

    // Also exercise sync_all_alliance_timers on empty/active war state without panicking
    sync_all_alliance_timers(&ctx);
}

#[tokio::test(flavor = "multi_thread")]
async fn alliance_bills_pay_bill_bulletins_order_and_gm_force_accept_guards() {
    let (ctx, _clock, mut exec, mut line, mut outsider, aid) = setup_sandbox();
    let creditor_corp = 98_200_003_i64;
    let exec_corp = 98_200_001_i64;
    let now = ctx.now_unix();

    let bill_unpaid = 500_001_i64;
    let bill_paid = 500_002_i64;
    let bill_foreign = 500_003_i64;
    let bill_receivable = 500_004_i64;

    ctx.db.with_mut_snapshot(|snap| {
        for rec in [
            CorpBillRecord {
                bill_id: bill_unpaid,
                bill_type_id: 2,
                debtor_id: aid,
                creditor_id: creditor_corp,
                amount_cents: IskCents::from_isk(250_000).0,
                due_unix: now + 86_400,
                interest: 0.0,
                external_id: aid,
                external_id2: 0,
                paid: false,
                paid_by_division: None,
            },
            CorpBillRecord {
                bill_id: bill_paid,
                bill_type_id: 2,
                debtor_id: aid,
                creditor_id: creditor_corp,
                amount_cents: IskCents::from_isk(100_000).0,
                due_unix: now + 86_400,
                interest: 0.0,
                external_id: aid,
                external_id2: 0,
                paid: true,
                paid_by_division: Some(1000),
            },
            CorpBillRecord {
                bill_id: bill_foreign,
                bill_type_id: 2,
                debtor_id: 99_999_999,
                creditor_id: creditor_corp,
                amount_cents: IskCents::from_isk(500_000).0,
                due_unix: now + 86_400,
                interest: 0.0,
                external_id: 0,
                external_id2: 0,
                paid: false,
                paid_by_division: None,
            },
            CorpBillRecord {
                bill_id: bill_receivable,
                bill_type_id: 2,
                debtor_id: creditor_corp,
                creditor_id: aid,
                amount_cents: IskCents::from_isk(75_000).0,
                due_unix: now + 86_400,
                interest: 0.0,
                external_id: 0,
                external_id2: 0,
                paid: false,
                paid_by_division: None,
            },
        ] {
            save_json_note(
                snap,
                bill_note_id(rec.bill_id),
                -71,
                rec.bill_id,
                format!("bill:{}", rec.bill_id),
                &rec,
                now,
            );
        }
    });

    // 1. GetBillBalance edge cases
    assert_eq!(
        call_social(&ctx, &mut exec, "allianceRegistry", "GetBillBalance", vec![PyRep::Integer(999_888)])
            .await
            .unwrap_err()
            .code,
        "BillNotFound"
    );
    assert_eq!(
        call_social(&ctx, &mut exec, "allianceRegistry", "GetBillBalance", vec![PyRep::Integer(bill_foreign)])
            .await
            .unwrap_err()
            .code,
        "AccessDenied"
    );
    assert_eq!(
        call_social(&ctx, &mut exec, "allianceRegistry", "GetBillBalance", vec![PyRep::Integer(bill_paid)])
            .await
            .unwrap(),
        PyRep::Real(0.0)
    );
    assert_eq!(
        call_social(&ctx, &mut exec, "allianceRegistry", "GetBillBalance", vec![])
            .await
            .unwrap(),
        PyRep::Real(250_000.0)
    );

    // 2. GetBills vs GetBillsReceivable
    let payable = call_social(&ctx, &mut exec, "allianceRegistry", "GetBills", vec![])
        .await
        .unwrap();
    let PyRep::Rowset { rows: payable_rows, .. } = payable else { panic!("expected rowset") };
    assert_eq!(payable_rows.len(), 1);
    assert_eq!(payable_rows[0][0].as_i64(), Some(bill_unpaid));

    let recv = call_social(&ctx, &mut exec, "allianceRegistry", "GetBillsReceivable", vec![])
        .await
        .unwrap();
    let PyRep::Rowset { rows: recv_rows, .. } = recv else { panic!("expected rowset") };
    assert_eq!(recv_rows.len(), 1);
    assert_eq!(recv_rows[0][0].as_i64(), Some(bill_receivable));

    // 3. PayBill error guards: InvalidArgument, BillNotFound, BillAlreadyPaid, AccessDenied, CorpRoleRequired, NotEnoughMoney
    assert_eq!(
        call_social(&ctx, &mut exec, "allianceRegistry", "PayBill", vec![])
            .await
            .unwrap_err()
            .code,
        "InvalidArgument"
    );
    assert_eq!(
        call_social(&ctx, &mut exec, "allianceRegistry", "PayBill", vec![PyRep::Integer(999_888)])
            .await
            .unwrap_err()
            .code,
        "BillNotFound"
    );
    assert_eq!(
        call_social(&ctx, &mut exec, "allianceRegistry", "PayBill", vec![PyRep::Integer(bill_paid)])
            .await
            .unwrap_err()
            .code,
        "BillAlreadyPaid"
    );
    assert_eq!(
        call_social(&ctx, &mut exec, "allianceRegistry", "PayBill", vec![PyRep::Integer(bill_foreign)])
            .await
            .unwrap_err()
            .code,
        "AccessDenied"
    );
    assert_eq!(
        call_social(&ctx, &mut line, "allianceRegistry", "PayBill", vec![PyRep::Integer(bill_unpaid)])
            .await
            .unwrap_err()
            .code,
        "CorpRoleDenied"
    );
    // Division 2 (1001) only has 100k ISK while bill is 250k ISK -> NotEnoughMoney
    assert_eq!(
        call_social(
            &ctx,
            &mut exec,
            "allianceRegistry",
            "PayBill",
            vec![PyRep::Integer(bill_unpaid), PyRep::Integer(1001)],
        )
        .await
        .unwrap_err()
        .code,
        "NotEnoughMoney"
    );

    // Valid PayBill from Master Wallet (1000) succeeds, updates balances, and blocks double-pay
    assert_eq!(
        call_social(
            &ctx,
            &mut exec,
            "allianceRegistry",
            "PayBill",
            vec![PyRep::Integer(bill_unpaid), PyRep::Integer(1000)],
        )
        .await
        .unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(
        call_social(&ctx, &mut exec, "allianceRegistry", "GetBillBalance", vec![])
            .await
            .unwrap(),
        PyRep::Real(0.0)
    );
    assert_eq!(
        call_social(&ctx, &mut exec, "allianceRegistry", "PayBill", vec![PyRep::Integer(bill_unpaid)])
            .await
            .unwrap_err()
            .code,
        "BillAlreadyPaid"
    );
    let (exec_master, cred_div1) = ctx.db.with_snapshot(|snap| {
        (
            snap.corporations.get(&exec_corp).unwrap().master_wallet_balance().0,
            snap.corporations.get(&creditor_corp).unwrap().division_balance(1).0,
        )
    });
    // Initial 5B minus 1B alliance creation fee in setup_sandbox minus 250k bill
    assert_eq!(exec_master, IskCents::from_isk(4_000_000_000 - 250_000).0);
    assert_eq!(cred_div1, IskCents::from_isk(5_000_000_000 + 250_000).0);

    // 4. UpdateBulletinOrder validation & reordering
    assert_eq!(
        call_social(
            &ctx,
            &mut exec,
            "allianceRegistry",
            "UpdateBulletinOrder",
            vec![PyRep::List(vec![])],
        )
        .await
        .unwrap_err()
        .code,
        "InvalidArgument"
    );
    let b1 = call_social(
        &ctx,
        &mut exec,
        "allianceRegistry",
        "AddBulletin",
        vec![PyRep::String("First".into()), PyRep::String("Body1".into())],
    )
    .await
    .unwrap()
    .dict_get("bulletinID")
    .and_then(|v| v.as_i64())
    .unwrap();
    let b2 = call_social(
        &ctx,
        &mut exec,
        "allianceRegistry",
        "AddBulletin",
        vec![PyRep::String("Second".into()), PyRep::String("Body2".into())],
    )
    .await
    .unwrap()
    .dict_get("bulletinID")
    .and_then(|v| v.as_i64())
    .unwrap();
    assert_eq!(
        call_social(
            &ctx,
            &mut exec,
            "allianceRegistry",
            "UpdateBulletinOrder",
            vec![PyRep::List(vec![PyRep::Integer(b2), PyRep::Integer(b1)])],
        )
        .await
        .unwrap(),
        PyRep::Bool(true)
    );
    let PyRep::Rowset { rows: bulletins, .. } =
        call_social(&ctx, &mut exec, "allianceRegistry", "GetBulletins", vec![])
            .await
            .unwrap()
    else {
        panic!("expected bulletin rowset");
    };
    assert_eq!(bulletins.len(), 2);
    assert_eq!(bulletins[0][0].as_i64(), Some(b2));
    assert_eq!(bulletins[1][0].as_i64(), Some(b1));

    // 5. GMForceAcceptApplication error guards
    assert_eq!(
        call_social(
            &ctx,
            &mut outsider,
            "allianceRegistry",
            "GMForceAcceptApplication",
            vec![PyRep::Integer(98_200_002)],
        )
        .await
        .unwrap_err()
        .code,
        "GMRoleRequired"
    );
    let mut gm_exec = exec.clone();
    gm_exec.role = eve_db::service_roles::ROLE_GMH;
    assert_eq!(
        call_social(
            &ctx,
            &mut gm_exec,
            "allianceRegistry",
            "GMForceAcceptApplication",
            vec![],
        )
        .await
        .unwrap_err()
        .code,
        "InvalidArgument"
    );
    assert_eq!(
        call_social(
            &ctx,
            &mut gm_exec,
            "allianceRegistry",
            "GMForceAcceptApplication",
            vec![PyRep::Integer(98_200_002)],
        )
        .await
        .unwrap_err()
        .code,
        "ApplicationNotFound"
    );
}
