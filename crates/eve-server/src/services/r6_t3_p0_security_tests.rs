//! Round-6 Track 3 P0 Security & Exploit Regression Tests (`#384`–`#389`).

#![forbid(unsafe_code)]

use crate::router::{context::BoundObjectEntry, ServerContext};
use eve_db::*;
use eve_destiny::{Ball, CombatState, DVec3, MovementMode, SlimItemMeta};
use eve_dogma::{DogmaItemState, FittedModule, ModuleState, ShipFittingEngine};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::{DbuffCollectionInfo, SdeRegistry};
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| {
        let mut sde = SdeRegistry::with_canonical_seed();
        sde.dbuff_collections.insert(
            9901,
            DbuffCollectionInfo {
                dbuff_id: 9901,
                name: "TestDbuff".into(),
                operation: "PostPercent".into(),
                aggregate_mode: "Maximum".into(),
                display_name_id: None,
                show_output_value_in_ui: None,
                item_modifiers: vec![37],
                location_modifiers: vec![],
                location_group_modifiers: vec![],
                location_required_skill_modifiers: vec![],
                target_attribute_ids: vec![],
            },
        );
        Arc::new(sde)
    }))
}

fn new_ctx(now_unix: u64) -> ServerContext {
    let db = Arc::new(WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(shared_sde(), db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(now_unix)));
    ctx
}

fn mk_char(snap: &WorldStateSnapshot, id: i64, uid: u64, corp: i64, st: i64, ship: i64) -> CharacterRecord {
    let mut ch = snap.characters.values().next().unwrap().clone();
    ch.character_id = id;
    ch.user_id = uid;
    ch.name = format!("Pilot_{id}");
    ch.corporation_id = corp;
    ch.station_id = Some(st);
    ch.home_station_id = st;
    ch.active_ship_id = ship;
    ch.clone_grade = 2;
    ch.balance = IskCents(1_000_000_000_00);
    ch
}

fn sess(uid: i64, cid: i64, corp: i64, roles: u64, st: i64, ship: i64) -> SessionState {
    let mut s = SessionState::default();
    s.user_id = Some(uid as u64);
    s.character_id = Some(cid);
    s.corporation_id = Some(corp);
    s.corp_role = roles;
    s.station_id = Some(st);
    s.solar_system_id = Some(30000142);
    s.ship_id = Some(ship);
    s
}

#[test]
fn r6_t3_01_biomass_destroys_ephemeral_implants_boosters_clones_and_protects_contracts() {
    let now = 1_700_000_000;
    let ctx = new_ctx(now);
    let (st, corp, ceo, ch) = (60003760, 98_500_001, 90_500_001, 90_500_002);
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.insert(corp, CorporationRecord {
            corporation_id: corp, name: "BioCorp".into(), ticker: "BIO".into(), ceo_id: ceo,
            hq_station_id: Some(st), members: [ceo, ch].into(), member_roles: [(ceo, u64::MAX), (ch, 0)].into(),
            ..Default::default()
        });
        let c_rec = mk_char(snap, ch, 7001, corp, st, 77_000_002);
        snap.characters.insert(ch, c_rec);
        for (id, tid, oid, loc, flag) in [
            (77_000_001, 27, corp, st, InventoryFlag::CorpSAG1),
            (77_000_002, 670, ch, st, InventoryFlag::Hangar),
            (77_000_003, 9899, ch, ch, InventoryFlag::Implant),
            (77_000_004, 28674, ch, ch, InventoryFlag::Booster),
            (77_000_005, 3468, ch, st, InventoryFlag::Hangar),
            (77_000_006, 27, ch, st, InventoryFlag::Locked),
        ] {
            snap.items.insert(id, InventoryItemRecord {
                item_id: id, type_id: tid, owner_id: oid, location_id: loc, flag, quantity: 1, singleton: true, ..Default::default()
            });
        }
        snap.active_implants.insert(ch, vec![9899]);
        snap.jump_clones.insert(ch, vec![JumpCloneRecord { jump_clone_id: 1, location_id: st, character_id: ch, implants: vec![20499] }]);
        snap.contracts.insert(88_000_001, ContractRecord {
            contract_id: 88_000_001, issuer_id: ceo, acceptor_id: Some(ch), contract_type: "ItemExchange".into(),
            status: "InProgress".into(), title: "Locked".into(), start_station_id: st, end_station_id: st,
            item_ids: vec![77_000_006], expiration_unix: now + 86_400, ..Default::default()
        });
    });

    let mut s = sess(7001, ch, corp, 0, st, 77_000_002);
    let kw = HashMap::new();
    super::character_lifecycle::handle(&ctx, &mut s, "charUnboundMgr", "PrepareCharacterForDelete", &[PyRep::Integer(ch)], &kw).unwrap().unwrap();
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(now + 36_001)));
    super::character_lifecycle::handle(&ctx, &mut s, "charUnboundMgr", "DeleteCharacter", &[PyRep::Integer(ch)], &kw).unwrap().unwrap();
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&77_000_003));
        assert!(!snap.items.contains_key(&77_000_004));
        assert!(!snap.items.contains_key(&77_000_005));
        assert!(!snap.active_implants.contains_key(&ch));
        assert!(!snap.jump_clones.contains_key(&ch));
        assert_ne!(snap.items[&77_000_006].flag, InventoryFlag::CorpSAG1);
        assert_eq!(snap.items[&77_000_002].flag, InventoryFlag::CorpSAG1);
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r6_t3_02_non_division_corp_assets_and_corp_containers_protected_on_add_and_multiadd() {
    let ctx = new_ctx(1_700_000_000);
    let (st, corp, ceo, thief) = (60003760, 98_500_010, 90_500_010, 90_500_011);
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.insert(corp, CorporationRecord {
            corporation_id: corp, name: "VaultCorp".into(), ticker: "VALT".into(), ceo_id: ceo,
            hq_station_id: Some(st), members: [ceo, thief].into(),
            member_roles: [(ceo, u64::MAX), (thief, 1u64 << 13)].into(), ..Default::default()
        });
        let t_rec = mk_char(snap, thief, 7010, corp, st, 0);
        snap.characters.insert(thief, t_rec);
        for (id, tid, loc, flag, single) in [
            (77_100_001, 3293, st, InventoryFlag::CorpSAG2, true),
            (77_100_002, 2048, 77_100_001, InventoryFlag::Cargo, true),
            (77_100_003, 34, st, InventoryFlag::CorpDeliveries, false),
            (77_100_004, 34, st, InventoryFlag::CorpSAG1, false),
        ] {
            snap.items.insert(id, InventoryItemRecord {
                item_id: id, type_id: tid, owner_id: corp, location_id: loc, flag, quantity: 100, singleton: single, ..Default::default()
            });
        }
    });

    let mut s = sess(7010, thief, corp, 1u64 << 13, st, 0);
    let bound = BoundObjectEntry {
        bound_id: 1, session_id: 1, user_id: 7010, character_id: thief,
        service: "invbroker".into(), object_id: st, oid_str: format!("N=1:{st}"), ref_id: 1,
    };
    let kw = HashMap::new();
    let hflag = PyRep::Integer(InventoryFlag::Hangar as i64);

    assert_eq!(super::inventory_stacking::handle_multi_add(&ctx, &mut s, Some(&bound), &[PyRep::List(vec![PyRep::Integer(77_100_002)]), PyRep::Integer(77_100_001)], &kw).await.unwrap_err().code, "CorpRoleDenied");
    assert_eq!(super::inventory_fitting::handle(&ctx, &mut s, Some(&bound), "invbroker", "Add", &[PyRep::Integer(77_100_003), PyRep::Integer(st), hflag.clone()], &kw).await.unwrap().unwrap_err().code, "CorpRoleDenied");
    super::inventory_fitting::handle(&ctx, &mut s, Some(&bound), "invbroker", "Add", &[PyRep::Integer(77_100_004), PyRep::Integer(st), hflag], &kw).await.unwrap().unwrap();
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&77_100_002].owner_id, corp);
        assert_eq!(snap.items[&77_100_003].owner_id, corp);
        assert_eq!(snap.items[&77_100_004].owner_id, thief);
    });
}

#[test]
fn r6_t3_03_personnel_manager_escalation_title_delay_and_role_drop_stasis_enforced() {
    let ctx = new_ctx(1_700_000_000);
    let (st, corp, ceo, pm, target) = (60003760, 98_500_020, 90_500_020, 90_500_021, 90_500_022);
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.insert(corp, CorporationRecord {
            corporation_id: corp, name: "GovCorp".into(), ticker: "GOVC".into(), ceo_id: ceo,
            hq_station_id: Some(st), members: [ceo, pm, target].into(),
            member_roles: [(ceo, u64::MAX), (pm, corp_roles::ROLE_PERSONNEL_MANAGER), (target, 0)].into(), ..Default::default()
        });
        snap.corp_governance.entry(corp).or_default().titles.insert(1, ("Treasury".into(), 1u64 << 13));
        for (cid, r) in [(ceo, u64::MAX), (pm, corp_roles::ROLE_PERSONNEL_MANAGER), (target, 0)] {
            let c_rec = mk_char(snap, cid, cid as u64, corp, st, 0);
            snap.characters.insert(cid, c_rec);
            snap.corp_role_assignments.insert((corp, cid), CorpMemberRoleAssignment { character_id: cid, roles: r, ..Default::default() });
        }
    });

    let mut pm_s = sess(pm, pm, corp, corp_roles::ROLE_PERSONNEL_MANAGER, st, 0);
    let kw = HashMap::new();
    for (prop, val) in [("grantableRoles", (1i64 << 13) | (1i64 << 32)), ("titleMask", 1)] {
        assert_eq!(super::corp_registry_ops::handle(&ctx, &mut pm_s, "corpRegistry", "ExecuteActions", &[
            PyRep::List(vec![PyRep::Integer(pm)]),
            PyRep::List(vec![PyRep::Tuple(vec![PyRep::String("set".into()), PyRep::String(prop.into()), PyRep::Integer(val)])]),
        ], &kw).unwrap().unwrap_err().code, "CrpAccessDenied");
    }

    let mut ceo_s = sess(ceo, ceo, corp, u64::MAX, st, 0);
    super::corp_registry_ops::handle(&ctx, &mut ceo_s, "corpRegistry", "ExecuteActions", &[PyRep::List(vec![PyRep::Tuple(vec![
        PyRep::String("SetTitle".into()), PyRep::Integer(target), PyRep::Integer(1),
    ])])], &kw).unwrap().unwrap();
    ctx.db.with_snapshot(|snap| {
        assert!(snap.corp_governance[&corp].pending_title_role_grants.contains_key(&target));
        assert!(!snap.corp_governance[&corp].role_drop_timestamps.contains_key(&target));
    });

    super::corp_registry_ops::handle(&ctx, &mut ceo_s, "corpRegistry", "ExecuteActions", &[PyRep::List(vec![PyRep::Tuple(vec![
        PyRep::String("SetTitle".into()), PyRep::Integer(target), PyRep::Integer(0),
    ])])], &kw).unwrap().unwrap();
    ctx.db.with_snapshot(|snap| {
        assert!(snap.corp_governance[&corp].role_drop_timestamps.contains_key(&target));
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r6_t3_04_phenomena_generator_transient_dbuff_no_permanent_mutation_or_1unit_bypass() {
    let ctx = new_ctx(1_700_000_000);
    let sys_id = 30_000_142u32;
    let sys = ctx.systems.get_or_spawn(sys_id);
    let (t_pilot, v_pilot, titan, victim, mod_id, phen_tid) = (90_500_030, 90_500_031, 77_300_001, 77_300_002, 77_300_003, 43554u32);
    let fit = ShipFittingEngine::from_sde(titan, 587, &ctx.sde);

    for (id, x) in [(titan, 0.0), (victim, 1000.0)] {
        let ball = Ball {
            id, type_id: 587, position: DVec3::new(x, 0.0, 0.0), velocity: DVec3::ZERO,
            max_velocity: 300.0, inertia: 1.0, mass: 1_000_000.0, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop,
        };
        let meta = SlimItemMeta { group_id: 25, category_id: 6, owner_id: id, ..Default::default() };
        let _ = sys.spawn_ball_with_meta(ball, CombatState::default(), meta).await;
    }

    ctx.db.with_mut_snapshot(|snap| {
        for (cid, sid, tid) in [(t_pilot, titan, 11567u32), (v_pilot, victim, 670u32)] {
            let mut c_rec = mk_char(snap, cid, cid as u64, 1_000_002, 60003760, sid);
            c_rec.solar_system_id = sys_id;
            snap.characters.insert(cid, c_rec);
            snap.items.insert(sid, InventoryItemRecord {
                item_id: sid, type_id: tid, owner_id: cid, location_id: sys_id as i64,
                flag: InventoryFlag::None, quantity: 1, singleton: true, ..Default::default()
            });
        }
        snap.items.insert(77_300_004, InventoryItemRecord {
            item_id: 77_300_004, type_id: 16274, owner_id: t_pilot, location_id: titan,
            flag: InventoryFlag::SpecializedFuelBay, quantity: 1, singleton: false, ..Default::default()
        });
    });

    let fm = FittedModule {
        item_id: mod_id, type_id: phen_tid, flag_id: InventoryFlag::HiSlot0 as u16, state: ModuleState::Online,
        charge_type_id: None, charge_quantity: 0, damage_hp: 0.0, max_hp: 40.0, dynamic_attribute_overrides: HashMap::new(),
    };
    let mut mod_st = DogmaItemState {
        item_id: mod_id, type_id: phen_tid, group_id: None, category_id: None,
        base_attributes: HashMap::new(), modifiers: Vec::new(), skill_levels: HashMap::new(),
    };
    for (aid, v) in [(713, 16274.0), (714, 500.0), (2468, 9901.0), (2469, 25.0)] { mod_st.base_attributes.insert(aid, v); }
    let mut mod_ty = shared_sde().types.get(&587).unwrap().clone();
    mod_ty.type_id = phen_tid;
    mod_ty.group_id = 1815;
    mod_ty.category_id = 7;
    mod_ty.name = "PhenomenaGen".into();
    mod_ty.effect_ids = vec![6753];
    mod_ty.default_attributes.insert(713, 16274.0);
    mod_ty.default_attributes.insert(714, 500.0);
    let kw = HashMap::new();

    let err = super::inventory_fitting::fitting_ewar_capital::prepare_ewar_and_capital_module(
        &ctx, &sys, sys_id, t_pilot, titan, 30, mod_id, &fm, &mod_ty, &mod_st, &fit, None, false, false, false, &kw,
    ).await.unwrap().err().unwrap();
    assert_eq!(err.code, "InsufficientFuel");

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(77_300_004, InventoryItemRecord {
            item_id: 77_300_004, type_id: 16274, owner_id: t_pilot, location_id: titan,
            flag: InventoryFlag::SpecializedFuelBay, quantity: 1000, singleton: false, ..Default::default()
        });
    });
    for _ in 0..2 {
        let prep = super::inventory_fitting::fitting_ewar_capital::prepare_ewar_and_capital_module(
            &ctx, &sys, sys_id, t_pilot, titan, 30, mod_id, &fm, &mod_ty, &mod_st, &fit, None, false, false, false, &kw,
        ).await.expect("phenomena prep").expect("phenomena ok");
        super::inventory_fitting::fitting_ewar_capital::commit_post_cycle_activation_effects(
            &ctx, &sys, t_pilot, titan, mod_id, &fm, &mod_ty, &mod_st, None, prep.deferred_post_cycle,
        ).await.expect("phenomena commit");
    }
    ctx.db.with_snapshot(|snap| { assert!(snap.items[&victim].dynamic_attributes.is_empty()); });
    assert!(super::inventory_fitting::fitting_scanners_superweapons::get_aggregated_ship_dbuffs(&ctx, victim, ctx.now_unix_secs()).contains_key(&9901));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r6_t3_05_war_surrender_cannot_be_reaccepted_to_drain_corp_wallet() {
    let now = 1_700_000_000;
    let ctx = new_ctx(now);
    let (loser, winner, l_ceo, w_ceo, war_id) = (98_500_040, 98_500_041, 90_500_040, 90_500_041, 55_001i64);
    ctx.db.with_mut_snapshot(|snap| {
        for (cid, ceo, bal) in [(loser, l_ceo, 5_000_000_000i64), (winner, w_ceo, 100_000_000i64)] {
            let mut corp_rec = CorporationRecord {
                corporation_id: cid, name: "WarCorp".into(), ticker: "WAR".into(), ceo_id: ceo,
                master_wallet: IskCents(bal * 100), members: [ceo].into(), member_roles: [(ceo, u64::MAX)].into(),
                ..Default::default()
            };
            corp_rec.ensure_wallet_divisions();
            corp_rec.wallet_divisions[0].balance = IskCents(bal * 100);
            snap.corporations.insert(cid, corp_rec);
            let c_rec = mk_char(snap, ceo, ceo as u64, cid, 60003760, 0);
            snap.characters.insert(ceo, c_rec);
        }
        snap.structures.insert(
            1_000_000_999_001,
            UpwellStructureRecord {
                structure_id: 1_000_000_999_001,
                owner_corp_id: loser,
                state: "Online".into(),
                ..Default::default()
            },
        );
        snap.wars.insert(war_id, WarRecord {
            war_id, aggressor_corp_id: winner, defender_corp_id: loser, mutual: false, started_unix: now - 3600, active: true,
        });
        for nid in [1i64, 2i64] {
            snap.war_state.negotiations.push(PersistedWarNegotiation {
                id: nid, war_id, kind: PersistedNegKind::Surrender, from_corp: loser,
                isk: IskCents(1_000_000_000 * 100), message: "Surrender".into(), status: "Offered".into(), effective_unix: None,
            });
        }
        snap.war_state.next_negotiation_id = 2;
    });

    let mut w_s = sess(w_ceo, w_ceo, winner, u64::MAX, 60003760, 0);
    let kw = HashMap::new();
    super::social_corp_fleet::handle(&ctx, &mut w_s, "warRegistry", "AcceptSurrender", &[PyRep::Integer(1)], &kw, None).await.unwrap().unwrap();
    let err2 = super::social_corp_fleet::handle(&ctx, &mut w_s, "warRegistry", "AcceptSurrender", &[PyRep::Integer(2)], &kw, None).await.unwrap().unwrap_err();
    assert_eq!(err2.code, "NegotiationNotFound");
    let err_replay = super::social_corp_fleet::handle(&ctx, &mut w_s, "warRegistry", "AcceptSurrender", &[PyRep::Integer(1)], &kw, None).await.unwrap().unwrap_err();
    assert_eq!(err_replay.code, "NegotiationNotFound");

    let mut l_s = sess(l_ceo, l_ceo, loser, u64::MAX, 60003760, 0);
    assert_eq!(super::social_corp_fleet::handle(&ctx, &mut l_s, "warRegistry", "CreateSurrenderNegotiation", &[
        PyRep::Integer(war_id), PyRep::Real(1_000_000_000.0), PyRep::String("Retry".into()),
    ], &kw, None).await.unwrap().unwrap_err().code, "WarAlreadyEnding");

    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.corporations[&loser].wallet_divisions[0].balance, IskCents(4_000_000_000 * 100));
        assert_eq!(snap.corporations[&winner].wallet_divisions[0].balance, IskCents(1_100_000_000 * 100));
    });
}

#[test]
fn r6_t3_06_mission_delivery_rejects_contract_locked_and_borrowed_courier_items() {
    let mut snap = WorldStateSnapshot::default();
    let (ch, st, mid) = (90_500_050, 60003760, 404);
    for (id, qty) in [(77_500_001, 1000), (77_500_002, 1000)] {
        snap.items.insert(id, InventoryItemRecord {
            item_id: id, type_id: 34, owner_id: ch, location_id: st, flag: InventoryFlag::Hangar, quantity: qty, singleton: false, ..Default::default()
        });
    }
    snap.contracts.insert(99_001, ContractRecord {
        contract_id: 99_001, issuer_id: ch, contract_type: "ItemExchange".into(), status: "Outstanding".into(), item_ids: vec![77_500_001], ..Default::default()
    });
    snap.item_custom_names.insert(77_500_002, format!("courier_cargo:{ch}:999"));

    let err = super::agents_mission_delivery::verify_and_consume_mission_delivery_items(
        &mut snap, ch, mid, 34, 500, false, false, Some(st), Some(st), None, None, None,
    ).unwrap_err();
    assert_eq!(err.code, "MissionIncomplete");
    assert_eq!(snap.items[&77_500_001].quantity, 1000);
    assert_eq!(snap.items[&77_500_002].quantity, 1000);

    snap.items.insert(77_500_003, InventoryItemRecord {
        item_id: 77_500_003, type_id: 34, owner_id: ch, location_id: st, flag: InventoryFlag::Hangar, quantity: 500, singleton: false, ..Default::default()
    });
    super::agents_mission_delivery::verify_and_consume_mission_delivery_items(
        &mut snap, ch, mid, 34, 500, false, false, Some(st), Some(st), None, None, None,
    ).unwrap();
    assert!(!snap.items.contains_key(&77_500_003));
    assert_eq!(snap.items[&77_500_001].quantity, 1000);
    assert_eq!(snap.items[&77_500_002].quantity, 1000);
}
