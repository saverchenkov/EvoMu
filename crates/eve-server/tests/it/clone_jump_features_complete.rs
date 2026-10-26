//! Parity tests for clone features:
//! - feat:clones.alpha-jump-clone-access
//! - feat:clones.clone-jump-booster-and-implant-handling
//! - feat:clones.clone-jump-preconditions-and-fees
//! - feat:clones.pochven-clone-restrictions
//! - feat:clones.same-location-clone-swap
//! - feat:clones.wormhole-clone-jump-ban
//! - feat:clones.jump-clone-ship-bay
//! - feat:clones.jump-clone-structure-dock
//! - feat:clones.clone-destroyed-structure

use eve_db::{
    InventoryFlag, InventoryItemRecord, IskCents, JumpCloneRecord, TrainedSkillRecord,
    UpwellStructureRecord, WorldStateSnapshot,
};
use eve_machonet::{service_roles, SessionState};
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use std::collections::HashMap;

async fn rpc(ctx: &ServerContext, sess: &mut SessionState, svc: &str, method: &str, args: Vec<PyRep>) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, svc, None, method, &args, &HashMap::new(), None).await
}

fn make_session(char_id: i64, station_id: Option<i64>, structure_id: Option<i64>, ship_id: Option<i64>) -> SessionState {
    let mut s = SessionState::new((char_id as u64) + 50000);
    s.user_id = Some((char_id as u64) + 1000);
    s.character_id = Some(char_id);
    s.station_id = station_id;
    s.structure_id = structure_id;
    s.ship_id = ship_id;
    s.role = service_roles::ROLE_PLAYER | service_roles::ROLE_LOGIN;
    s
}

fn seed_pilot(snap: &mut WorldStateSnapshot, id: i64, corp: i64, station: Option<i64>, ship: i64) {
    let mut ch = snap.characters.get(&90_000_001).cloned().unwrap();
    ch.character_id = id;
    ch.corporation_id = corp;
    ch.station_id = station;
    ch.structure_id = None;
    ch.active_ship_id = ship;
    ch.home_station_id = 60_003_760;
    ch.clone_grade = 2;
    ch.last_clone_jump_unix = 0;
    snap.characters.insert(id, ch);
}

fn seed_item(snap: &mut WorldStateSnapshot, item_id: i64, type_id: u32, owner_id: i64, location_id: i64, flag: InventoryFlag) {
    snap.items.insert(item_id, InventoryItemRecord {
        item_id, type_id, owner_id, location_id, flag, quantity: 1, singleton: true, ..Default::default()
    });
}

fn seed_clone(snap: &mut WorldStateSnapshot, pilot: i64, id: i64, loc: i64, imps: Vec<u32>) {
    snap.jump_clones.entry(pilot).or_default().push(JumpCloneRecord {
        jump_clone_id: id, character_id: pilot, location_id: loc, implants: imps,
    });
}

fn seed_skill(snap: &mut WorldStateSnapshot, pilot: i64, type_id: u32, level: i32) {
    snap.skills.entry(pilot).or_default().insert(type_id, TrainedSkillRecord { type_id, level: level as u8, skill_points: (level as f64) * 15_000.0 });
}

fn setup_structure(snap: &mut WorldStateSnapshot, id: i64, sys: u32, corp: i64, svc_id: i64) {
    snap.structures.insert(id, UpwellStructureRecord {
        structure_id: id, type_id: 35832, owner_corp_id: corp, solar_system_id: sys,
        name: format!("St {id}"), position: [0.0, 0.0, 0.0], state: "online".into(),
        quantum_core_installed: true, fuel_blocks: 10_000,
        shield_hp: 10000.0, armor_hp: 10000.0, hull_hp: 10000.0,
        reinforced_until_unix: 0, last_damage_tick: 0, last_damage_unix: 0, self_destruct_at_unix: None,
    });
    if svc_id > 0 {
        seed_item(snap, svc_id, 35894, corp, id, InventoryFlag::ServiceSlot0);
        snap.module_online_states.insert(svc_id, true);
    }
}

#[tokio::test]
async fn test_clone_jump_preconditions_fees_and_same_location_swap() {
    // parity: feat:clones.clone-jump-preconditions-and-fees, feat:clones.same-location-clone-swap
    eve_testkit::attest(&["feat:clones.clone-jump-preconditions-and-fees", "feat:clones.same-location-clone-swap"]);

    let ctx = ServerContext::new();
    let pilot_id = 90_000_201_i64;
    let dep_st_id = 60_003_760_i64; // Jita IV - 4
    let remote_st_id = ctx.sde.stations.values().find(|st| st.has_clone_bay && st.station_id != dep_st_id).map(|st| st.station_id).expect("remote station");
    let (cap_id, rifter_id) = (91_000_001_i64, 91_000_002_i64);

    ctx.db.with_mut_snapshot(|snap| {
        seed_pilot(snap, pilot_id, 1001, Some(dep_st_id), rifter_id);
        snap.characters.get_mut(&pilot_id).unwrap().balance = IskCents(50_000_000); // 500k ISK (< 900k fee)
        snap.skills.remove(&pilot_id); // clear seeded skills to test InfomorphPsychologyRequired

        seed_item(snap, cap_id, 670, pilot_id, dep_st_id, InventoryFlag::Hangar);
        seed_item(snap, rifter_id, 587, pilot_id, dep_st_id, InventoryFlag::Hangar);
        seed_clone(snap, pilot_id, 8101, dep_st_id, vec![]);
        seed_clone(snap, pilot_id, 8102, remote_st_id, vec![]);
    });

    // 1. MustBeDocked
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&pilot_id).unwrap();
        ch.station_id = None;
        ch.structure_id = None;
    });
    let mut sess_undocked = make_session(pilot_id, None, None, Some(cap_id));
    assert_eq!(rpc(&ctx, &mut sess_undocked, "jumpCloneSvc", "CloneJump", vec![PyRep::Integer(8102)]).await.unwrap_err().code, "MustBeDocked");

    // Dock pilot in departure station
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&pilot_id).unwrap().station_id = Some(dep_st_id);
    });

    // 2. MustBeInCapsule
    let mut sess = make_session(pilot_id, Some(dep_st_id), None, Some(rifter_id));
    assert_eq!(rpc(&ctx, &mut sess, "jumpCloneSvc", "CloneJump", vec![PyRep::Integer(8102)]).await.unwrap_err().code, "MustBeInCapsule");
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&pilot_id).unwrap().active_ship_id = cap_id;
    });
    sess.ship_id = Some(cap_id);

    // 3. InfomorphPsychologyRequired
    assert_eq!(rpc(&ctx, &mut sess, "jumpCloneSvc", "CloneJump", vec![PyRep::Integer(8102)]).await.unwrap_err().code, "InfomorphPsychologyRequired");
    ctx.db.with_mut_snapshot(|snap| {
        seed_skill(snap, pilot_id, 24242, 2);
        seed_skill(snap, pilot_id, 33399, 2);
    });

    // 4. SessionChangeTimerActive
    eve_server::services::space_destiny::record_session_change_timer(&ctx, pilot_id, ctx.now_unix(), 30_000_144, 1, "test");
    assert_eq!(rpc(&ctx, &mut sess, "jumpCloneSvc", "CloneJump", vec![PyRep::Integer(8102)]).await.unwrap_err().code, "SessionChangeTimerActive");
    ctx.advance_secs(12);

    // 5. InsufficientFunds (< 900,000 ISK)
    assert_eq!(rpc(&ctx, &mut sess, "jumpCloneSvc", "CloneJump", vec![PyRep::Integer(8102)]).await.unwrap_err().code, "InsufficientFunds");

    // 6. Successful jump: debits 900k ISK, records journal, sets cooldown
    ctx.db.with_mut_snapshot(|snap| snap.characters.get_mut(&pilot_id).unwrap().balance = IskCents(500_000_000));
    assert!(matches!(rpc(&ctx, &mut sess, "jumpCloneSvc", "CloneJump", vec![PyRep::Integer(8102)]).await.expect("CloneJump"), PyRep::KeyVal(_)));

    let (bal, journal_has_fee, last_jump) = ctx.db.with_snapshot(|snap| {
        let ch = snap.characters.get(&pilot_id).unwrap();
        let fee = snap.wallet_journal.iter().any(|e| e.character_id == pilot_id && e.entry_type == "CloneJumpFee" && e.amount.0 == -90_000_000);
        (ch.balance.0, fee, ch.last_clone_jump_unix)
    });
    assert_eq!(bal, 410_000_000);
    assert!(journal_has_fee);
    assert!(last_jump > 0);

    // 7. CloneJumpCooldownActive blocks remote jump
    ctx.advance_secs(12);
    sess.station_id = Some(remote_st_id);
    assert_eq!(rpc(&ctx, &mut sess, "jumpCloneSvc", "CloneJump", vec![PyRep::Integer(8102)]).await.unwrap_err().code, "CloneJumpCooldownActive");

    // 8. Same-location clone swap allowed with 0 cooldown while cooldown active
    ctx.db.with_mut_snapshot(|snap| seed_clone(snap, pilot_id, 8103, remote_st_id, vec![]));
    assert!(matches!(rpc(&ctx, &mut sess, "jumpCloneSvc", "CloneJump", vec![PyRep::Integer(8103)]).await.expect("Swap"), PyRep::KeyVal(_)));

    // 9. Upwell structure owner-set fee per standing (min 1 ISK)
    let (struct_id, owner_corp) = (1_000_000_000_801_i64, 98_000_001_i64);
    ctx.db.with_mut_snapshot(|snap| {
        setup_structure(snap, struct_id, 30_000_144, owner_corp, 91_000_010);
        let ch = snap.characters.get_mut(&pilot_id).unwrap();
        ch.corporation_id = owner_corp;
        ch.standings.insert(owner_corp, 6.0);
        snap.corporations.insert(owner_corp, eve_db::CorporationRecord {
            corporation_id: owner_corp, name: "CBAY".into(), ticker: "CB".into(), ceo_id: pilot_id, master_wallet: IskCents(0), ..Default::default()
        });
        seed_clone(snap, pilot_id, 8104, struct_id, vec![]);
    });

    ctx.advance_secs(24 * 3600 + 10);
    sess.station_id = Some(remote_st_id);
    rpc(&ctx, &mut sess, "jumpCloneSvc", "CloneJump", vec![PyRep::Integer(8104)]).await.expect("Jump to structure");

    ctx.advance_secs(24 * 3600 + 10);
    sess.station_id = None;
    sess.structure_id = Some(struct_id);
    let bal_before = ctx.db.with_snapshot(|snap| snap.characters.get(&pilot_id).unwrap().balance.0);
    rpc(&ctx, &mut sess, "jumpCloneSvc", "CloneJump", vec![PyRep::Integer(8102)]).await.expect("Jump from structure");

    let (bal_after, corp_wallet) = ctx.db.with_snapshot(|snap| (snap.characters.get(&pilot_id).unwrap().balance.0, snap.corporations.get(&owner_corp).unwrap().master_wallet.0));
    assert_eq!(bal_before - bal_after, 100, "1 ISK min fee debited");
    assert_eq!(corp_wallet, 100, "1 ISK fee credited to corp");
}

#[tokio::test]
async fn test_clone_jump_space_restrictions_and_alpha_access() {
    // parity: feat:clones.pochven-clone-restrictions, feat:clones.wormhole-clone-jump-ban, feat:clones.alpha-jump-clone-access
    eve_testkit::attest(&[
        "feat:clones.pochven-clone-restrictions",
        "feat:clones.wormhole-clone-jump-ban",
        "feat:clones.alpha-jump-clone-access",
    ]);

    let ctx = ServerContext::new();
    let pilot_id = 90_000_202_i64;
    let (jita_st_id, cap_id) = (60_003_760_i64, 92_000_001_i64);
    let poch_station_id = 60002002_i64; // In Pochven (region 10000070)
    let (wh_sys_id, wh_struct_id) = (31_000_007_u32, 1_000_000_000_802_i64);
    let thera_struct_id = 1_000_000_000_803_i64;

    ctx.db.with_mut_snapshot(|snap| {
        seed_pilot(snap, pilot_id, 1001, Some(jita_st_id), cap_id);
        seed_skill(snap, pilot_id, 24242, 3);
        seed_skill(snap, pilot_id, 33399, 3);
        seed_item(snap, cap_id, 670, pilot_id, jita_st_id, InventoryFlag::Hangar);
        setup_structure(snap, wh_struct_id, wh_sys_id, 1001, 92_000_010);
        setup_structure(snap, thera_struct_id, 31_000_005, 1001, 92_000_011);
        seed_clone(snap, pilot_id, 8201, poch_station_id, vec![]);
        seed_clone(snap, pilot_id, 8202, wh_struct_id, vec![]);
        seed_clone(snap, pilot_id, 8203, thera_struct_id, vec![]);
    });

    let mut sess = make_session(pilot_id, Some(jita_st_id), None, Some(cap_id));

    // 1. Pochven medical clone restriction (via charMgr and home_station)
    let err_med_mgr = rpc(&ctx, &mut sess, "charMgr", "SetHomeStation", vec![PyRep::Integer(poch_station_id)]).await.unwrap_err();
    // rpc-lint: allow error code check
    assert_eq!(err_med_mgr.code, "CannotSetMedicalCloneInPochven");
    let err_med = rpc(&ctx, &mut sess, "home_station", "set_home_station", vec![PyRep::Integer(poch_station_id)]).await.unwrap_err();
    // rpc-lint: allow error code check
    assert_eq!(err_med.code, "CannotSetMedicalCloneInPochven");

    // 2. Pochven jump clone ban: remote jump into Pochven refused
    let err_poch_jump = rpc(&ctx, &mut sess, "jumpCloneSvc", "CloneJump", vec![PyRep::Integer(8201)]).await.unwrap_err();
    assert_eq!(err_poch_jump.code, "CannotCloneJumpToPochven");

    // 3. Local clone swap in Pochven is allowed
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&pilot_id).unwrap();
        ch.station_id = Some(poch_station_id);
        ch.solar_system_id = 30002079;
        seed_clone(snap, pilot_id, 8204, poch_station_id, vec![]);
    });
    sess.station_id = Some(poch_station_id);
    assert!(matches!(rpc(&ctx, &mut sess, "jumpCloneSvc", "CloneJump", vec![PyRep::Integer(8204)]).await.expect("Pochven local swap"), PyRep::KeyVal(_)));

    // 4. Wormhole clone jump ban: remote jump into W-space refused
    ctx.advance_secs(24 * 3600 + 10);
    sess.station_id = Some(jita_st_id);
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&pilot_id).unwrap();
        ch.station_id = Some(jita_st_id);
        ch.solar_system_id = 30_000_144;
    });
    let err_wh = rpc(&ctx, &mut sess, "jumpCloneSvc", "CloneJump", vec![PyRep::Integer(8202)]).await.unwrap_err();
    assert_eq!(err_wh.code, "CannotCloneJumpInWormholeSpace");

    // 5. In-structure clone swap in W-space structure allowed
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&pilot_id).unwrap();
        ch.station_id = None;
        ch.structure_id = Some(wh_struct_id);
        ch.solar_system_id = wh_sys_id;
        seed_clone(snap, pilot_id, 8205, wh_struct_id, vec![]);
    });
    sess.station_id = None;
    sess.structure_id = Some(wh_struct_id);
    assert!(matches!(rpc(&ctx, &mut sess, "jumpCloneSvc", "CloneJump", vec![PyRep::Integer(8205)]).await.expect("WH local swap"), PyRep::KeyVal(_)));

    // 6. Thera exception: behaves as k-space, remote jump allowed
    ctx.advance_secs(24 * 3600 + 10);
    sess.station_id = Some(jita_st_id);
    sess.structure_id = None;
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&pilot_id).unwrap();
        ch.station_id = Some(jita_st_id);
        ch.solar_system_id = 30_000_144;
    });
    assert!(matches!(rpc(&ctx, &mut sess, "jumpCloneSvc", "CloneJump", vec![PyRep::Integer(8203)]).await.expect("Thera jump"), PyRep::KeyVal(_)));

    // 7. Alpha access: Alpha can jump between existing clones, but install requires Omega
    ctx.advance_secs(24 * 3600 + 10);
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&pilot_id).unwrap();
        ch.clone_grade = 1; // Alpha
        ch.station_id = Some(jita_st_id);
        ch.solar_system_id = 30_000_144;
        seed_clone(snap, pilot_id, 8206, jita_st_id, vec![]);
        seed_clone(snap, pilot_id, 8207, thera_struct_id, vec![]);
    });
    sess.station_id = Some(jita_st_id);
    sess.structure_id = None;
    assert!(matches!(rpc(&ctx, &mut sess, "jumpCloneSvc", "CloneJump", vec![PyRep::Integer(8207)]).await.expect("Alpha clone jump"), PyRep::KeyVal(_)));
    // Pilot is now docked at thera_struct_id, install in structure refused with OmegaCapacityRequired
    assert_eq!(rpc(&ctx, &mut sess, "jumpCloneSvc", "InstallCloneInStructure", vec![PyRep::Integer(thera_struct_id)]).await.unwrap_err().code, "OmegaCapacityRequired");

    // Dock pilot in station, install in station refused with OmegaCapacityRequired
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&pilot_id).unwrap();
        ch.station_id = Some(jita_st_id);
        ch.structure_id = None;
    });
    sess.station_id = Some(jita_st_id);
    sess.structure_id = None;
    assert_eq!(rpc(&ctx, &mut sess, "jumpCloneSvc", "InstallCloneInStation", vec![PyRep::Integer(jita_st_id)]).await.unwrap_err().code, "OmegaCapacityRequired");
}

#[tokio::test]
async fn test_clone_jump_boosters_implants_structure_dock_and_ship_bay() {
    // parity: feat:clones.clone-jump-booster-and-implant-handling, feat:clones.jump-clone-ship-bay, feat:clones.jump-clone-structure-dock, feat:clones.clone-destroyed-structure
    eve_testkit::attest(&[
        "feat:clones.clone-jump-booster-and-implant-handling",
        "feat:clones.jump-clone-ship-bay",
        "feat:clones.jump-clone-structure-dock",
        "feat:clones.clone-destroyed-structure",
    ]);

    let ctx = ServerContext::new();
    let (pilot1, pilot2) = (90_000_203_i64, 90_000_204_i64);
    let (jita_st_id, cap_id) = (60_003_760_i64, 93_000_001_i64);
    let (booster_item_id, accel_item_id) = (93_000_002_i64, 93_000_003_i64);
    let target_jc_id = 8301_i64;

    ctx.db.with_mut_snapshot(|snap| {
        seed_pilot(snap, pilot1, 1_000_001, Some(jita_st_id), cap_id);
        seed_skill(snap, pilot1, 24242, 3);
        seed_skill(snap, pilot1, 33399, 3);
        snap.active_implants.insert(pilot1, vec![9899]);

        seed_item(snap, cap_id, 670, pilot1, jita_st_id, InventoryFlag::Hangar);
        seed_item(snap, booster_item_id, 28668, pilot1, pilot1, InventoryFlag::Booster);
        seed_item(snap, accel_item_id, 60269, pilot1, pilot1, InventoryFlag::Booster);
        seed_clone(snap, pilot1, target_jc_id, jita_st_id, vec![10216]);
    });

    let mut sess1 = make_session(pilot1, Some(jita_st_id), None, Some(cap_id));

    // 1. Clone jump: combat booster removed, accelerator preserved, implants swapped
    assert!(matches!(rpc(&ctx, &mut sess1, "jumpCloneSvc", "CloneJump", vec![PyRep::Integer(target_jc_id)]).await.expect("Clone jump"), PyRep::KeyVal(_)));

    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&booster_item_id), "Booster removed");
        assert!(snap.items.contains_key(&accel_item_id), "Accelerator kept");
        assert_eq!(snap.active_implants.get(&pilot1).cloned().unwrap(), vec![10216]);
        let jc = snap.jump_clones.get(&pilot1).unwrap().iter().find(|c| c.jump_clone_id == target_jc_id).unwrap();
        assert_eq!(jc.implants, vec![9899]);
        assert_eq!(snap.characters.get(&pilot1).unwrap().corporation_id, 1_000_001);
    });

    // 2. Upwell structure clone bay service and ACL gating
    let (struct_id, owner_corp) = (1_000_000_000_805_i64, 1_000_001_i64);
    ctx.db.with_mut_snapshot(|snap| {
        setup_structure(snap, struct_id, 30_000_144, owner_corp, 0); // No clone bay service fitted yet
        let ch = snap.characters.get_mut(&pilot1).unwrap();
        ch.structure_id = Some(struct_id);
        ch.station_id = None;
    });

    let mut sess_struct = make_session(pilot1, None, Some(struct_id), Some(cap_id));
    assert_eq!(rpc(&ctx, &mut sess_struct, "jumpCloneSvc", "InstallCloneInStructure", vec![PyRep::Integer(struct_id)]).await.unwrap_err().code, "NoCloneBayService");

    ctx.db.with_mut_snapshot(|snap| {
        seed_item(snap, 93_000_020, 35894, owner_corp, struct_id, InventoryFlag::ServiceSlot0);
        snap.module_online_states.insert(93_000_020, true);
        snap.structure_docking_acl_denied.entry(struct_id).or_default().insert(pilot1);
    });
    assert_eq!(rpc(&ctx, &mut sess_struct, "jumpCloneSvc", "InstallCloneInStructure", vec![PyRep::Integer(struct_id)]).await.unwrap_err().code, "StructureAccessDenied");

    ctx.db.with_mut_snapshot(|snap| { snap.structure_docking_acl_denied.get_mut(&struct_id).unwrap().clear(); });
    assert!(matches!(rpc(&ctx, &mut sess_struct, "jumpCloneSvc", "InstallCloneInStructure", vec![PyRep::Integer(struct_id)]).await.expect("InstallCloneInStructure"), PyRep::KeyVal(_)));

    // 3. Capital ship clone bay
    let (rorq_id, cvb_mod_id, ozone_id) = (93_000_030_i64, 93_000_031_i64, 93_000_032_i64);
    ctx.db.with_mut_snapshot(|snap| {
        seed_item(snap, rorq_id, 28352, pilot1, 30_000_144, InventoryFlag::None);
        seed_item(snap, cvb_mod_id, 23735, pilot1, rorq_id, InventoryFlag::HiSlot0);
        snap.module_online_states.insert(cvb_mod_id, true);
        snap.items.insert(ozone_id, InventoryItemRecord {
            item_id: ozone_id, type_id: 16273, owner_id: pilot1, location_id: rorq_id, flag: InventoryFlag::Cargo, quantity: 500, singleton: false, ..Default::default()
        });
        seed_pilot(snap, pilot2, 1001, Some(jita_st_id), 0);
        seed_skill(snap, pilot2, 24242, 3);
    });

    let mut sess_rorq = make_session(pilot1, None, None, Some(rorq_id));
    let mut sess2 = make_session(pilot2, Some(jita_st_id), None, None);

    // Caller must pilot/own host ship
    let mut sess_stranger = make_session(pilot2, Some(jita_st_id), None, None);
    let err_not_owner = rpc(&ctx, &mut sess_stranger, "jumpCloneSvc", "OfferShipCloneInstallation", vec![PyRep::Integer(pilot1), PyRep::Integer(rorq_id)]).await.unwrap_err();
    assert_eq!(err_not_owner.code, "MustPilotHostShip");

    assert!(matches!(rpc(&ctx, &mut sess_rorq, "jumpCloneSvc", "OfferShipCloneInstallation", vec![PyRep::Integer(pilot2), PyRep::Integer(rorq_id)]).await.expect("Offer"), PyRep::KeyVal(_)));
    assert!(matches!(rpc(&ctx, &mut sess2, "jumpCloneSvc", "AcceptShipCloneInstallation", vec![PyRep::Integer(rorq_id)]).await.expect("Accept"), PyRep::KeyVal(_)));

    let ship_state = rpc(&ctx, &mut sess_rorq, "jumpCloneSvc", "GetShipCloneState", vec![PyRep::Integer(rorq_id)]).await.expect("GetShipCloneState");
    let PyRep::Rowset { rows, .. } = ship_state else { panic!("Rowset expected") };
    assert!(rows.iter().any(|r| r.get(1) == Some(&PyRep::Integer(pilot2))));

    // AcceptShipCloneInstallation fails with InsufficientLiquidOzone when host ship lacks liquid ozone
    let (rorq_no_oz, cvb_no_oz) = (93_000_035_i64, 93_000_036_i64);
    ctx.db.with_mut_snapshot(|snap| {
        seed_item(snap, rorq_no_oz, 28352, pilot1, 30_000_144, InventoryFlag::None);
        seed_item(snap, cvb_no_oz, 23735, pilot1, rorq_no_oz, InventoryFlag::HiSlot0);
        snap.module_online_states.insert(cvb_no_oz, true);
    });
    let mut sess_no_oz = make_session(pilot1, None, None, Some(rorq_no_oz));
    rpc(&ctx, &mut sess_no_oz, "jumpCloneSvc", "OfferShipCloneInstallation", vec![PyRep::Integer(pilot2), PyRep::Integer(rorq_no_oz)]).await.unwrap();
    let err_oz = rpc(&ctx, &mut sess2, "jumpCloneSvc", "AcceptShipCloneInstallation", vec![PyRep::Integer(rorq_no_oz)]).await.unwrap_err();
    assert_eq!(err_oz.code, "InsufficientLiquidOzone");

    // 4. Clones in destroyed structure moved home and lose implants
    let (destr_struct_id, clone_in_destr_id) = (1_000_000_000_806_i64, 8305_i64);
    ctx.db.with_mut_snapshot(|snap| {
        setup_structure(snap, destr_struct_id, 30_000_144, owner_corp, 93_000_040);
        seed_clone(snap, pilot2, clone_in_destr_id, destr_struct_id, vec![9941, 10216]);
        let mut ext = eve_server::services::station_structure::structure_upwell_ops::load_ext_state(snap);
        eve_server::services::station_structure::structure_lifecycle::destroy_structure(&ctx, snap, &mut ext, destr_struct_id, pilot1, ctx.now_unix());
        eve_server::services::station_structure::structure_upwell_ops::save_ext_state(snap, &ext);
    });

    ctx.db.with_snapshot(|snap| {
        let jc = snap.jump_clones.get(&pilot2).unwrap().iter().find(|c| c.jump_clone_id == clone_in_destr_id).unwrap();
        assert_eq!(jc.location_id, jita_st_id);
        assert!(jc.implants.is_empty());
    });

    // 5. Clones in unanchored/packaged structure moved home and lose implants
    let (pack_struct_id, clone_in_pack_id) = (1_000_000_000_807_i64, 8306_i64);
    ctx.db.with_mut_snapshot(|snap| {
        setup_structure(snap, pack_struct_id, 30_000_144, owner_corp, 93_000_041);
        seed_clone(snap, pilot2, clone_in_pack_id, pack_struct_id, vec![9941]);
        let mut ext = eve_server::services::station_structure::structure_upwell_ops::load_ext_state(snap);
        eve_server::services::station_structure::structure_lifecycle::package_structure(&ctx, snap, &mut ext, pack_struct_id, ctx.now_unix());
        eve_server::services::station_structure::structure_upwell_ops::save_ext_state(snap, &ext);
    });

    ctx.db.with_snapshot(|snap| {
        let jc = snap.jump_clones.get(&pilot2).unwrap().iter().find(|c| c.jump_clone_id == clone_in_pack_id).unwrap();
        assert_eq!(jc.location_id, jita_st_id);
        assert!(jc.implants.is_empty());
    });
}
