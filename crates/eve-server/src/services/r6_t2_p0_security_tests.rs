//! Security regression tests for Round-6 Track 2 P0/P1 exploits (`R6-T2-01`..`R6-T2-07` / `#370`..`#376`).

#![forbid(unsafe_code)]

use crate::quasar_gateway::{PublicGatewayRouter, STATUS_FORBIDDEN};
use crate::router::ServerContext;
use crate::services::mining_advanced::mining_compression::{
    caller_can_take_corp_hangar_item, inspect_metenox_drill,
};
use crate::services::sov_services::{
    handle_contract_delete_multiple, TYPE_ORBITAL_SKYHOOK, TYPE_SOVEREIGNTY_HUB,
};
use eve_db::corp_roles::{
    ROLE_CONTRACT_MANAGER, ROLE_DIRECTOR, ROLE_HANGAR_CAN_TAKE, ROLE_STATION_MANAGER,
};
use eve_db::{
    AllianceRecord, CharacterRecord, ContractRecord, CorpMemberRoleAssignment, CorporationRecord,
    InventoryFlag, InventoryItemRecord, PersistedMercenaryDen, PersistedSovHubData,
    UpwellStructureRecord, WorldRepository, WorldStateSnapshot,
};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::quasar::{
    AuthoritativeContext, DynamicQuasarMessage, DynamicValue, GoogleAny, QuasarDescriptorRegistry,
};
use eve_proto::PyRep;
use eve_sde::{InvType, SdeRegistry};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};

const TYPE_METENOX_MOON_DRILL: u32 = 81_826;
static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn make_inv_type(template: &InvType, tid: u32, gid: u32, vol: f64, cap: f64, name: &str) -> InvType {
    let mut t = template.clone();
    t.type_id = tid;
    t.group_id = gid;
    t.category_id = if matches!(gid, 1245 | 4736 | 4744) { 65 } else { 4 };
    t.name = name.into();
    t.volume = vol;
    t.capacity = cap;
    t.base_price = 1_000.0;
    t.portion_size = 100;
    if matches!(gid, 4768 | 4772 | 4838 | 4839) {
        t.default_attributes.insert(5780, 100.0);
        t.default_attributes.insert(5781, 0.0);
    }
    t
}

fn shared_sde() -> Arc<SdeRegistry> {
    Arc::clone(SHARED_SDE.get_or_init(|| {
        let mut sde = SdeRegistry::with_canonical_seed();
        let template = sde.types[&34].clone();
        for (tid, gid, vol, cap, name) in [
            (32458, 1245, 4_000.0, 0.0, "Territorial Claim Unit"),
            (TYPE_SOVEREIGNTY_HUB, 1245, 4_000.0, 0.0, "Sovereignty Hub"),
            (TYPE_ORBITAL_SKYHOOK, 4736, 4_000.0, 0.0, "Orbital Skyhook"),
            (TYPE_METENOX_MOON_DRILL, 4744, 4_000.0, 480_000.0, "Metenox Moon Drill"),
            (85_230, 4810, 100.0, 0.0, "Mercenary Den"),
            (81143, 427, 0.5, 0.0, "Magmatic Gas"),
            (4247, 1136, 5.0, 0.0, "Helium Fuel Block"),
            (82941, 4768, 100.0, 0.0, "Major Mining Upgrades"),
            (82942, 4772, 100.0, 0.0, "Cynosural Navigation"),
            (82943, 4838, 100.0, 0.0, "Workforce Regulator"),
            (82944, 4839, 100.0, 0.0, "Gamma Stability Generator"),
            (16633, 427, 0.2, 0.0, "Atmospheric Gases"),
            (16634, 427, 0.2, 0.0, "Evaporite Deposits"),
            (16635, 427, 0.2, 0.0, "Hydrocarbons"),
            (16636, 427, 0.2, 0.0, "Silicates"),
            (1228, 463, 0.1, 0.0, "Scordite"),
            (34, 18, 0.01, 0.0, "Tritanium"),
            (35, 18, 0.01, 0.0, "Pyerite"),
        ] {
            sde.types
                .entry(tid)
                .or_insert_with(|| make_inv_type(&template, tid, gid, vol, cap, name));
        }
        sde.type_materials
            .entry(1228)
            .or_insert_with(|| vec![eve_sde::TypeMaterialEntry {
                material_type_id: 34,
                quantity: 346,
            }]);
        Arc::new(sde)
    }))
}

fn test_ctx() -> ServerContext {
    ServerContext::with_sde_and_repository(
        shared_sde(),
        Arc::new(WorldRepository::with_demo_seed()),
    )
    .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
}

fn seed_char(snap: &mut WorldStateSnapshot, cid: i64, corp: i64, aid: Option<i64>, sys: u32) {
    let mut c: CharacterRecord = snap.characters[&90_000_001].clone();
    c.character_id = cid;
    c.name = format!("Char {cid}");
    c.corporation_id = corp;
    c.alliance_id = aid;
    c.solar_system_id = sys;
    snap.characters.insert(cid, c);
}

fn seed_corp(snap: &mut WorldStateSnapshot, corp: i64, ceo: i64, aid: Option<i64>) {
    snap.corporations.insert(corp, { let mut __corp = CorporationRecord::default();
        __corp.corporation_id = corp;
        __corp.name = format!("Corp {corp}");
        __corp.ticker = "TST".into();
        __corp.ceo_id = ceo;
        __corp.alliance_id = aid;
        __corp.tax_rate = 0.1;
        __corp.members = HashSet::from([ceo]);
        __corp.member_roles = HashMap::from([(ceo, ROLE_DIRECTOR)]);
        __corp });
}

fn sess(char_id: i64, corp_id: i64, sys_id: u32, station_id: Option<i64>, role: u64) -> SessionState {
    let mut s = SessionState::default();
    s.user_id = Some(1);
    s.character_id = Some(char_id);
    s.corporation_id = Some(corp_id);
    s.solar_system_id = Some(sys_id);
    s.station_id = station_id;
    s.structure_id = station_id;
    s.corp_role = role;
    s
}

fn id_sub(id: i64) -> DynamicValue {
    let mut m = DynamicQuasarMessage::new("Id");
    m.fields.insert(1, DynamicValue::Int64(id));
    DynamicValue::Message(Box::new(m))
}

fn invoke_quasar(
    router: &PublicGatewayRouter,
    req_type: &str,
    auth: &AuthoritativeContext,
    msg: &DynamicQuasarMessage,
) -> Result<GoogleAny, (u32, String)> {
    let h = router.handlers.read().unwrap().get(req_type).cloned().expect(req_type);
    h(auth, &GoogleAny::default(), msg)
}

/// `#370` (`R6-T2-01`): Cross-corp alliance directors must not mutate Corp A's Sov Hub or Skyhook.
#[test]
fn r6_t2_01_issue_370_quasar_sov_cross_corp_takeover_blocked() {
    let sde = shared_sde();
    let db = Arc::new(WorldRepository::with_demo_seed());
    let router = PublicGatewayRouter::new(
        Arc::new(QuasarDescriptorRegistry::empty_with_builtins()),
        Arc::clone(&db),
        Arc::clone(&sde),
    );
    crate::services::quasar_sov_hub::register_handlers(&router, Arc::clone(&db), Arc::clone(&sde));
    crate::services::quasar_sov_workforce::register_handlers(&router, Arc::clone(&db), Arc::clone(&sde));
    crate::services::quasar_sov_services::register_handlers(&router, Arc::clone(&db), Arc::clone(&sde));

    let (corp_a, corp_b, alliance_id) = (98_700_001_i64, 98_700_002_i64, 99_700_001_i64);
    let (dir_a, dir_b) = (90_700_001_i64, 90_700_002_i64);
    let (hub_id, skyhook_id, sys_id) = (79_700_001_i64, 79_700_002_i64, 30_002_537_u32);

    db.with_mut_snapshot(|snap| {
        snap.alliances.insert(alliance_id, AllianceRecord {
            alliance_id,
            name: "Ally".into(),
            short_name: "ALY".into(),
            executor_corp_id: corp_a,
            member_corp_ids: HashSet::from([corp_a, corp_b]),
        });
        seed_corp(snap, corp_a, dir_a, Some(alliance_id));
        seed_corp(snap, corp_b, dir_b, Some(alliance_id));
        seed_char(snap, dir_a, corp_a, Some(alliance_id), sys_id);
        seed_char(snap, dir_b, corp_b, Some(alliance_id), sys_id);
        snap.structures.insert(hub_id, UpwellStructureRecord { structure_id: hub_id, type_id: TYPE_SOVEREIGNTY_HUB, owner_corp_id: corp_a, solar_system_id: sys_id, state: "ShieldVulnerable".into(), quantum_core_installed: true, ..Default::default() });
        snap.structures.insert(skyhook_id, UpwellStructureRecord { structure_id: skyhook_id, type_id: TYPE_ORBITAL_SKYHOOK, owner_corp_id: corp_a, solar_system_id: sys_id, state: "ShieldVulnerable".into(), quantum_core_installed: true, ..Default::default() });
        snap.sov_runtime.sov_hubs.insert(hub_id, PersistedSovHubData::default());
        snap.sov_runtime.skyhook_active.insert(skyhook_id, false);
    });

    let ctx_b = AuthoritativeContext::for_session_at(Some(2), Some(dir_b as u32), Some(corp_b as u32), ROLE_DIRECTOR, "tranquility", vec![1], 10_000);
    let ctx_a = AuthoritativeContext::for_session_at(Some(1), Some(dir_a as u32), Some(corp_a as u32), ROLE_DIRECTOR, "tranquility", vec![1], 10_000);

    // 1. Cross-corp director B in same alliance is denied hub config, uninstall, workforce config, and skyhook activation
    let mut cfg_req = DynamicQuasarMessage::new("Cfg");
    cfg_req.fields.insert(3, id_sub(hub_id));
    assert_eq!(invoke_quasar(&router, "eve_public.sovereignty.hub.upgrade.api.ProcessConfigurationRequest", &ctx_b, &cfg_req).unwrap_err().0, STATUS_FORBIDDEN);

    let mut uninst_req = DynamicQuasarMessage::new("Uninst");
    let mut up_id = DynamicQuasarMessage::new("UpId");
    up_id.fields.insert(1, id_sub(hub_id));
    up_id.fields.insert(2, id_sub(82941));
    uninst_req.fields.insert(1, DynamicValue::Message(Box::new(up_id)));
    assert_eq!(invoke_quasar(&router, "eve_public.sovereignty.hub.upgrade.api.UninstallRequest", &ctx_b, &uninst_req).unwrap_err().0, STATUS_FORBIDDEN);

    let mut wf_req = DynamicQuasarMessage::new("Wf");
    wf_req.fields.insert(1, id_sub(hub_id));
    let mut wf_cfg = DynamicQuasarMessage::new("WfCfg");
    wf_cfg.fields.insert(3, DynamicValue::Message(Box::new(DynamicQuasarMessage::new("Inactive"))));
    wf_req.fields.insert(2, DynamicValue::Message(Box::new(wf_cfg)));
    assert_eq!(invoke_quasar(&router, "eve_public.sovereignty.hub.workforce.api.ConfigureRequest", &ctx_b, &wf_req).unwrap_err().0, STATUS_FORBIDDEN);

    let mut sky_req = DynamicQuasarMessage::new("Sky");
    sky_req.fields.insert(1, id_sub(skyhook_id));
    assert_eq!(invoke_quasar(&router, "eve_public.sovereignty.skyhook.api.ActivateRequest", &ctx_b, &sky_req).unwrap_err().0, STATUS_FORBIDDEN);

    // 2. Owner Corp A's director succeeds on mutating operations
    assert!(invoke_quasar(&router, "eve_public.sovereignty.hub.upgrade.api.ProcessConfigurationRequest", &ctx_a, &cfg_req).is_ok());
    assert!(invoke_quasar(&router, "eve_public.sovereignty.hub.workforce.api.ConfigureRequest", &ctx_a, &wf_req).is_ok());
    assert!(invoke_quasar(&router, "eve_public.sovereignty.skyhook.api.ActivateRequest", &ctx_a, &sky_req).is_ok());
}

/// `#371` (`R6-T2-02`): Prevents corporate contract item embezzlement, roleless deletion, expired re-deletion, and unchecked item teleportation.
#[test]
fn r6_t2_02_issue_371_contract_delete_multiple_security() {
    let ctx = test_ctx();
    let (issuer_id, line_id, victim_id) = (90_000_001_i64, 90_000_002_i64, 90_000_003_i64);
    let corp_id = 98_000_001_i64;
    let station_id = 60_003_760_i64;

    ctx.db.acquire_online_lock(1, 501, Some(issuer_id), true, 10_000).unwrap();
    let (cid_corp, cid_personal, cid_expired, corp_item, escrow_item, victim_item) = ctx.db.with_mut_snapshot(|snap| {
        seed_corp(snap, corp_id, 90_000_009, None);
        snap.corporations.entry(corp_id).and_modify(|c| {
            c.member_roles.insert(issuer_id, ROLE_CONTRACT_MANAGER);
            c.member_roles.insert(line_id, 0);
        });
        let corp_item = 88_100_001_i64;
        snap.items.insert(corp_item, InventoryItemRecord { item_id: corp_item, type_id: 587, owner_id: corp_id, location_id: 77_100_001, flag: InventoryFlag::Locked, quantity: 1, singleton: true, version: 1, ..Default::default() });
        let escrow_item = 88_100_002_i64;
        snap.items.insert(escrow_item, InventoryItemRecord { item_id: escrow_item, type_id: 34, owner_id: issuer_id, location_id: station_id, flag: InventoryFlag::Locked, quantity: 50, version: 1, ..Default::default() });
        let victim_item = 88_100_003_i64;
        snap.items.insert(victim_item, InventoryItemRecord { item_id: victim_item, type_id: 587, owner_id: victim_id, location_id: station_id, flag: InventoryFlag::Hangar, quantity: 1, singleton: true, version: 1, ..Default::default() });

        let cid_corp = 77_100_001_i64;
        snap.contracts.insert(cid_corp, ContractRecord { contract_id: cid_corp, issuer_id: line_id, issuer_corp_id: corp_id, for_corp: true, contract_type: "ItemExchange".into(), status: "Outstanding".into(), start_station_id: station_id, end_station_id: station_id, item_ids: vec![corp_item], ..Default::default() });
        let cid_personal = 77_100_002_i64;
        snap.contracts.insert(cid_personal, ContractRecord { contract_id: cid_personal, issuer_id, issuer_corp_id: corp_id, for_corp: false, contract_type: "ItemExchange".into(), status: "Outstanding".into(), start_station_id: station_id, end_station_id: station_id, item_ids: vec![escrow_item, victim_item], ..Default::default() });
        let cid_expired = 77_100_003_i64;
        snap.contracts.insert(cid_expired, ContractRecord { contract_id: cid_expired, issuer_id, issuer_corp_id: corp_id, for_corp: false, contract_type: "ItemExchange".into(), status: "Expired".into(), start_station_id: station_id, end_station_id: station_id, item_ids: vec![victim_item], ..Default::default() });
        (cid_corp, cid_personal, cid_expired, corp_item, escrow_item, victim_item)
    });

    // 1. Line member without Contract Manager role cannot delete corp contract even if listed as issuer
    let line_sess = sess(line_id, corp_id, 30_000_142, Some(station_id), 0);
    let r1 = handle_contract_delete_multiple(&ctx, &line_sess, &[PyRep::List(vec![PyRep::Integer(cid_corp)])]).unwrap();
    if let PyRep::Tuple(parts) = r1 {
        assert_eq!(parts[0].row_count(), Some(0));
        assert_eq!(parts[1].row_count(), Some(1));
    }

    // 2. Contract Manager deleting corp contract restores item to corp_id + Deliveries (NOT personal hangar!)
    //    and rejects Expired contracts & unlocked foreign item_ids
    let mgr_sess = sess(issuer_id, corp_id, 30_000_142, Some(station_id), ROLE_CONTRACT_MANAGER);
    let r2 = handle_contract_delete_multiple(&ctx, &mgr_sess, &[PyRep::List(vec![
        PyRep::Integer(cid_corp), PyRep::Integer(cid_personal), PyRep::Integer(cid_expired),
    ])]).unwrap();
    if let PyRep::Tuple(parts) = r2 {
        assert_eq!(parts[0].row_count(), Some(2));
        assert_eq!(parts[1].row_count(), Some(1));
    }

    ctx.db.with_snapshot(|snap| {
        let ci = &snap.items[&corp_item];
        assert_eq!(ci.owner_id, corp_id);
        assert_eq!(ci.location_id, station_id);
        assert_eq!(ci.flag, InventoryFlag::Deliveries);
        assert!(ci.version > 1);

        let ei = &snap.items[&escrow_item];
        assert_eq!(ei.owner_id, issuer_id);
        assert_eq!(ei.flag, InventoryFlag::Hangar);
        assert!(ei.version > 1);

        let vi = &snap.items[&victim_item];
        assert_eq!(vi.owner_id, victim_id);
        assert_eq!(vi.location_id, station_id);
        assert_eq!(vi.flag, InventoryFlag::Hangar);
    });
    assert!(ctx.drain_notifications_for_char(issuer_id).iter().any(|n| n.event == "OnItemChange"));
}

/// `#372` (`R6-T2-03`): Blocks uncored/anchoring Metenox harvesting and corp/guest hangar fuel & gas drain.
#[test]
fn r6_t2_03_issue_372_metenox_drill_security() {
    let ctx = test_ctx();
    let (corp_id, char_id) = (98_000_001_i64, 90_000_001_i64);
    let (cored_id, uncored_id, anchoring_id, sys_id) = (79_300_001_i64, 79_300_002_i64, 79_300_003_i64, 30_002_537_u32);
    let moon_pos = ctx.sde.celestials_in_system(sys_id).into_iter().find(|c| c.group_id == 8).unwrap().position;

    let (sag_fuel, guest_gas, sf_fuel, sf_gas) = (88_300_001_i64, 88_300_002_i64, 88_300_003_i64, 88_300_004_i64);
    ctx.db.with_mut_snapshot(|snap| {
        seed_corp(snap, corp_id, char_id, None);
        seed_char(snap, char_id, corp_id, None, sys_id);
        snap.structures.insert(uncored_id, UpwellStructureRecord { structure_id: uncored_id, type_id: TYPE_METENOX_MOON_DRILL, owner_corp_id: corp_id, solar_system_id: sys_id, state: "ShieldVulnerable".into(), quantum_core_installed: false, fuel_blocks: 50, position: [moon_pos.x, moon_pos.y, moon_pos.z], ..Default::default() });
        snap.structures.insert(anchoring_id, UpwellStructureRecord { structure_id: anchoring_id, type_id: TYPE_METENOX_MOON_DRILL, owner_corp_id: corp_id, solar_system_id: sys_id, state: "AnchorVulnerable".into(), quantum_core_installed: true, fuel_blocks: 50, position: [moon_pos.x, moon_pos.y, moon_pos.z], ..Default::default() });
        snap.structures.insert(cored_id, UpwellStructureRecord { structure_id: cored_id, type_id: TYPE_METENOX_MOON_DRILL, owner_corp_id: corp_id, solar_system_id: sys_id, state: "ShieldVulnerable".into(), quantum_core_installed: true, fuel_blocks: 5, position: [moon_pos.x, moon_pos.y, moon_pos.z], ..Default::default() });

        snap.items.insert(sf_fuel, InventoryItemRecord { item_id: sf_fuel, type_id: 4247, owner_id: corp_id, location_id: cored_id, flag: InventoryFlag::StructureFuel, quantity: 5, version: 1, ..Default::default() });
        snap.items.insert(sf_gas, InventoryItemRecord { item_id: sf_gas, type_id: 81143, owner_id: corp_id, location_id: cored_id, flag: InventoryFlag::StructureFuel, quantity: 300, version: 1, ..Default::default() });
        snap.items.insert(sag_fuel, InventoryItemRecord { item_id: sag_fuel, type_id: 4247, owner_id: corp_id, location_id: cored_id, flag: InventoryFlag::CorpSAG1, quantity: 100, version: 1, ..Default::default() });
        snap.items.insert(guest_gas, InventoryItemRecord { item_id: guest_gas, type_id: 81143, owner_id: 90_000_002, location_id: cored_id, flag: InventoryFlag::Hangar, quantity: 500, version: 1, ..Default::default() });
    });

    // Inspection ignores CorpSAG1 and guest Hangar items, reporting only synced StructureFuel item blocks (5) without double-counting st.fuel_blocks (#567)
    ctx.db.with_snapshot(|snap| {
        let insp = inspect_metenox_drill(&ctx.sde, snap, cored_id, char_id, 1.0).unwrap();
        assert_eq!(insp.total_struct_fuel, 5);
        assert_eq!(insp.total_struct_gas, 300);
    });

    crate::services::mining_advanced::tick_all_metenox_moon_drills(&ctx, 1_000, 1_000 + 3_600);

    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.structures[&uncored_id].fuel_blocks, 50);
        assert_eq!(snap.structures[&anchoring_id].fuel_blocks, 50);
        assert!(snap.items.values().all(|it| it.location_id != uncored_id && it.location_id != anchoring_id));

        assert_eq!(snap.structures[&cored_id].fuel_blocks, 0);
        assert!(!snap.items.contains_key(&sf_fuel));
        assert_eq!(snap.items[&sf_gas].quantity, 290);
        assert_eq!(snap.items[&sag_fuel].quantity, 100);
        assert_eq!(snap.items[&guest_gas].quantity, 500);
    });
}

/// `#373` (`R6-T2-04`): Blocks `sovMgr.DestroySkyhooks` during active heists or reinforcement and purges runtime/ballpark state.
#[tokio::test]
async fn r6_t2_04_issue_373_destroy_skyhooks_guards_and_cleanup() {
    let ctx = test_ctx();
    let (corp_id, char_id) = (98_000_001_i64, 90_000_001_i64);
    let (skyhook_id, den_id, sys_id) = (79_400_001_i64, 91_400_001_i64, 30_002_537_u32);
    let kw = HashMap::new();
    let mut s = sess(char_id, corp_id, sys_id, None, ROLE_DIRECTOR);

    let actor = ctx.systems.get_or_spawn(sys_id);
    for (bid, tid) in [(skyhook_id, TYPE_ORBITAL_SKYHOOK), (den_id, 85_230)] {
        actor.spawn_ball(Ball {
            id: bid, type_id: tid, position: glam::DVec3::new(2_000.0, 0.0, 0.0),
            velocity: glam::DVec3::ZERO, max_velocity: 0.0, inertia: 1.0, mass: 1_000.0, radius: 1_000.0,
            speed_fraction: 0.0, mode: MovementMode::Stop,
        }).await.unwrap();
    }
    ctx.db.with_mut_snapshot(|snap| {
        seed_corp(snap, corp_id, char_id, None);
        seed_char(snap, char_id, corp_id, None, sys_id);
        snap.structures.insert(skyhook_id, UpwellStructureRecord { structure_id: skyhook_id, type_id: TYPE_ORBITAL_SKYHOOK, owner_corp_id: corp_id, solar_system_id: sys_id, state: "ShieldVulnerable".into(), shield_hp: 5_000.0, armor_hp: 5_000.0, hull_hp: 5_000.0, ..Default::default() });
        snap.sov_runtime.skyhook_silo_links.insert(skyhook_id, (90_000_002, 98_000_002, 100, 1.0));
        snap.sov_runtime.skyhook_planets.insert(skyhook_id, 40_009_080);
        snap.sov_runtime.skyhook_reagents.insert(skyhook_id, (81143, 500));
        snap.sov_runtime.mercenary_dens.insert(den_id, PersistedMercenaryDen { den_id, owner_char_id: char_id, skyhook_id, solar_system_id: sys_id, planet_id: 40_009_080, type_id: 85_230, enabled: true, cargo_extraction_enabled: true, development_level: 10, anarchy_level: 10, infomorphs: 0, simulated_at_unix: 0, next_activity_generation_unix: 0 });
    });

    let err_heist = crate::services::sov_services::handle(&ctx, &mut s, "sovMgr", "DestroySkyhooks", &[PyRep::Integer(i64::from(sys_id))], &kw).await.unwrap().unwrap_err();
    assert_eq!(err_heist.code, "ActiveReagentHeist");

    ctx.db.with_mut_snapshot(|snap| {
        snap.sov_runtime.skyhook_silo_links.remove(&skyhook_id);
        snap.structures.get_mut(&skyhook_id).unwrap().state = "Reinforced".into();
    });
    let err_rf = crate::services::sov_services::handle(&ctx, &mut s, "sovMgr", "DestroySkyhooks", &[PyRep::Integer(i64::from(sys_id))], &kw).await.unwrap().unwrap_err();
    assert_eq!(err_rf.code, "StructureReinforcedOrDamaged");

    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.get_mut(&skyhook_id).unwrap().state = "ShieldVulnerable".into();
    });
    let ok = crate::services::sov_services::handle(&ctx, &mut s, "sovMgr", "DestroySkyhooks", &[PyRep::Integer(i64::from(sys_id))], &kw).await.unwrap().unwrap();
    assert_eq!(ok, PyRep::Integer(1));
    assert!(actor.get_ball_snapshot(skyhook_id).await.unwrap().is_none());
    assert!(actor.get_ball_snapshot(den_id).await.unwrap().is_none());
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.structures.contains_key(&skyhook_id));
        assert!(!snap.sov_runtime.skyhook_planets.contains_key(&skyhook_id));
        assert!(!snap.sov_runtime.skyhook_reagents.contains_key(&skyhook_id));
        assert!(!snap.sov_runtime.mercenary_dens.contains_key(&den_id));
    });
}

/// `#374` (`R6-T2-05`): Validates inventory flags across `caller_can_take_corp_hangar_item`, `reprocessingSvc.Reprocess`, and Quasar `item_at_hub`.
#[tokio::test]
async fn r6_t2_05_issue_374_inventory_flag_validation() {
    let ctx = test_ctx();
    let (corp_id, dir_id, member_id) = (98_000_001_i64, 90_000_001_i64, 90_000_002_i64);
    let station_id = 60_003_760_i64;
    let (locked_ore, div2_ore, sag1_ore) = (88_500_001_i64, 88_500_002_i64, 88_500_003_i64);

    ctx.db.with_mut_snapshot(|snap| {
        seed_corp(snap, corp_id, dir_id, None);
        seed_char(snap, member_id, corp_id, None, 30_000_142);
        snap.corporations.entry(corp_id).and_modify(|c| {
            c.ceo_id = dir_id;
            c.member_roles.insert(dir_id, ROLE_DIRECTOR);
            c.member_roles.insert(member_id, 0);
        });
        snap.corp_role_assignments.insert((corp_id, member_id), CorpMemberRoleAssignment { character_id: member_id, roles: ROLE_HANGAR_CAN_TAKE[0], grantable_roles: 0, roles_at_hq: 0, grantable_roles_at_hq: 0, roles_at_base: 0, grantable_roles_at_base: 0, roles_at_other: 0, grantable_roles_at_other: 0 });
        snap.items.insert(locked_ore, InventoryItemRecord { item_id: locked_ore, type_id: 1228, owner_id: member_id, location_id: station_id, flag: InventoryFlag::Locked, quantity: 200, ..Default::default() });
        snap.items.insert(div2_ore, InventoryItemRecord { item_id: div2_ore, type_id: 1228, owner_id: corp_id, location_id: station_id, flag: InventoryFlag::CorpSAG2, quantity: 200, ..Default::default() });
        snap.items.insert(sag1_ore, InventoryItemRecord { item_id: sag1_ore, type_id: 1228, owner_id: corp_id, location_id: station_id, flag: InventoryFlag::CorpSAG1, quantity: 200, ..Default::default() });

        assert!(!caller_can_take_corp_hangar_item(snap, dir_id, corp_id, InventoryFlag::Locked, ROLE_DIRECTOR));
        assert!(!caller_can_take_corp_hangar_item(snap, dir_id, corp_id, InventoryFlag::Deliveries, ROLE_DIRECTOR));
        assert!(caller_can_take_corp_hangar_item(snap, member_id, corp_id, InventoryFlag::CorpSAG1, 0));
        assert!(!caller_can_take_corp_hangar_item(snap, member_id, corp_id, InventoryFlag::CorpSAG2, 0));

        let caller = crate::services::quasar_sov_hub::Caller { cid: member_id, corp: corp_id, alliance: None, roles: 0 };
        let mut upg = InventoryItemRecord { item_id: 88_500_010, type_id: 82941, owner_id: corp_id, location_id: station_id, flag: InventoryFlag::Locked, quantity: 1, ..Default::default() };
        assert!(!crate::services::quasar_sov_hub::item_at_hub(snap, &caller, &upg, station_id, 30_000_142));
        upg.flag = InventoryFlag::CorpSAG2;
        assert!(!crate::services::quasar_sov_hub::item_at_hub(snap, &caller, &upg, station_id, 30_000_142));
        upg.flag = InventoryFlag::CorpSAG1;
        assert!(crate::services::quasar_sov_hub::item_at_hub(snap, &caller, &upg, station_id, 30_000_142));
    });

    let mut ms = sess(member_id, corp_id, 30_000_142, Some(station_id), 0);
    let r_locked = crate::services::space_entity_mining::handle(&ctx, &mut ms, "reprocessingSvc", "Reprocess", &[PyRep::List(vec![PyRep::Integer(locked_ore)])], None).await.unwrap().unwrap_err();
    assert_eq!(r_locked.code, "OwnershipOrLocationViolation");
    let r_div2 = crate::services::space_entity_mining::handle(&ctx, &mut ms, "reprocessingSvc", "Reprocess", &[PyRep::List(vec![PyRep::Integer(div2_ore)])], None).await.unwrap().unwrap_err();
    assert_eq!(r_div2.code, "OwnershipOrLocationViolation");
    let r_sag1 = crate::services::space_entity_mining::handle(&ctx, &mut ms, "reprocessingSvc", "Reprocess", &[PyRep::List(vec![PyRep::Integer(sag1_ore)])], None).await.unwrap();
    assert!(r_sag1.is_ok());
}

/// `#375` (`R6-T2-06`): Enforces division take roles and splits stacked upgrades (>1 qty) in `sovHubSvc.SaveUpgradeConfiguration`.
#[tokio::test]
async fn r6_t2_06_issue_375_sov_hub_save_upgrade_configuration_roles_and_split() {
    let ctx = test_ctx();
    let (corp_id, char_id, aid, hub_id) = (98_000_001_i64, 90_000_002_i64, 99_600_001_i64, 79_600_001_i64);
    let sys_id = ctx.sde.solar_systems.values().find(|s| s.security_status < 0.0).unwrap().solar_system_id;
    let kw = HashMap::new();
    let (div2_upg, sag1_stack) = (88_600_001_i64, 88_600_002_i64);

    ctx.db.with_mut_snapshot(|snap| {
        snap.alliances.insert(aid, AllianceRecord { alliance_id: aid, name: "Ally".into(), short_name: "ALY".into(), executor_corp_id: corp_id, member_corp_ids: HashSet::from([corp_id]) });
        snap.system_states.entry(sys_id).or_default().sov_alliance_id = Some(aid);
        seed_corp(snap, corp_id, 90_000_001, Some(aid));
        seed_char(snap, char_id, corp_id, Some(aid), sys_id);
        snap.corporations.entry(corp_id).and_modify(|c| {
            c.member_roles.insert(char_id, ROLE_STATION_MANAGER);
        });
        snap.corp_role_assignments.insert((corp_id, char_id), CorpMemberRoleAssignment { character_id: char_id, roles: ROLE_HANGAR_CAN_TAKE[0], grantable_roles: 0, roles_at_hq: 0, grantable_roles_at_hq: 0, roles_at_base: 0, grantable_roles_at_base: 0, roles_at_other: 0, grantable_roles_at_other: 0 });
        snap.structures.insert(hub_id, UpwellStructureRecord { structure_id: hub_id, type_id: TYPE_SOVEREIGNTY_HUB, owner_corp_id: corp_id, solar_system_id: sys_id, state: "ShieldVulnerable".into(), quantum_core_installed: true, fuel_blocks: 100, ..Default::default() });
        snap.sov_runtime.sov_hubs.insert(hub_id, PersistedSovHubData::default());
        snap.items.insert(div2_upg, InventoryItemRecord { item_id: div2_upg, type_id: 87702, owner_id: corp_id, location_id: hub_id, flag: InventoryFlag::CorpSAG2, quantity: 1, version: 1, ..Default::default() });
    });

    let mut s = sess(char_id, corp_id, sys_id, None, ROLE_STATION_MANAGER);
    let err = crate::services::mining_sites::handle(&ctx, &mut s, "sovHubSvc", "SaveUpgradeConfiguration", &[PyRep::Integer(87702)], &kw).await.unwrap().unwrap_err();
    assert_eq!(err.code, "UpgradeItemNotFound");
    let err_fw = crate::services::sov_fw_insurgency::fw_insurgency::handle_hub_and_scan(&ctx, &mut s, "sovHubSvc", "SaveUpgradeConfiguration", &[PyRep::Integer(87702)], &kw).await.unwrap().unwrap_err();
    assert_eq!(err_fw.code, "UpgradeItemNotFound");

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(sag1_stack, InventoryItemRecord { item_id: sag1_stack, type_id: 87702, owner_id: corp_id, location_id: hub_id, flag: InventoryFlag::CorpSAG1, quantity: 5, version: 1, ..Default::default() });
    });
    let res = crate::services::mining_sites::handle(&ctx, &mut s, "sovHubSvc", "SaveUpgradeConfiguration", &[PyRep::Integer(87702)], &kw).await.unwrap();
    assert!(res.is_ok(), "unexpected err: {res:?}");

    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&sag1_stack].quantity, 4);
        assert_eq!(snap.items[&sag1_stack].flag, InventoryFlag::CorpSAG1);
        let installed = snap.items.values().filter(|it| it.location_id == hub_id && it.flag == InventoryFlag::SubSystemSlot0 && it.type_id == 87702).collect::<Vec<_>>();
        assert_eq!(installed.len(), 1);
        assert_eq!(installed[0].quantity, 1);
        assert!(installed[0].singleton);
    });
}

/// `#376` (`R6-T2-07`): Blocks `entity.CmdUnanchor` on Upwell structures and roleless corp members on corporate deployables.
#[tokio::test]
async fn r6_t2_07_issue_376_cmd_unanchor_structures_and_corp_roles() {
    let ctx = test_ctx();
    let (corp_id, char_id, sys_id) = (98_000_001_i64, 90_000_002_i64, 30_000_142_u32);
    let (struct_id, mtu_id) = (79_800_001_i64, 88_800_001_i64);
    let kw = HashMap::new();

    let actor = ctx.systems.get_or_spawn(sys_id);
    for (bid, tid) in [(struct_id, TYPE_ORBITAL_SKYHOOK), (mtu_id, 33_475)] {
        actor.spawn_ball(Ball {
            id: bid, type_id: tid, position: glam::DVec3::new(1_000.0, 0.0, 0.0),
            velocity: glam::DVec3::ZERO, max_velocity: 0.0, inertia: 1.0, mass: 1_000.0, radius: 1_000.0,
            speed_fraction: 0.0, mode: MovementMode::Stop,
        }).await.unwrap();
    }
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(struct_id, UpwellStructureRecord { structure_id: struct_id, type_id: TYPE_ORBITAL_SKYHOOK, owner_corp_id: corp_id, solar_system_id: sys_id, ..Default::default() });
        snap.items.insert(mtu_id, InventoryItemRecord { item_id: mtu_id, type_id: 33_475, owner_id: corp_id, location_id: i64::from(sys_id), flag: InventoryFlag::Cargo, quantity: 1, singleton: true, ..Default::default() });
    });

    let mut line_sess = sess(char_id, corp_id, sys_id, None, 0);
    let err_st = crate::services::space_entity_ops::handle(&ctx, &mut line_sess, None, "entity", "CmdUnanchor", &[PyRep::Integer(struct_id)], &kw).await.unwrap().unwrap_err();
    assert_eq!(err_st.code, "CannotUnanchorStructure");

    let err_role = crate::services::space_entity_ops::handle(&ctx, &mut line_sess, None, "entity", "CmdUnanchor", &[PyRep::Integer(mtu_id)], &kw).await.unwrap().unwrap_err();
    assert_eq!(err_role.code, "CorpRoleDenied");

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(corp) = snap.corporations.get_mut(&corp_id) {
            corp.member_roles.insert(char_id, ROLE_DIRECTOR);
        }
    });
    let mut dir_sess = sess(char_id, corp_id, sys_id, None, ROLE_DIRECTOR);
    assert!(crate::services::space_entity_ops::handle(&ctx, &mut dir_sess, None, "entity", "CmdUnanchor", &[PyRep::Integer(mtu_id)], &kw).await.unwrap().is_ok());
}
