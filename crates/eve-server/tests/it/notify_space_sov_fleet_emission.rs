//! Real server-side emission tests for Increment 7 (`notify:*` across Domains 1, 2, and 3):
//! - Domain 1: Structures, Docking/Undocking, Asset Safety, Hangar View & Upwell (22 server-emitted rows)
//! - Domain 2: Sovereignty, Skyhooks, Mercenary Dens, Dynamic Bounty, Faction Warfare & Pirate Insurgency (29 server-emitted rows)
//! - Domain 3: Fleets (remaining rows), Crimewatch, Timers, Kill Rights, Bounties, Security Status & Duels (39 server-emitted rows)

use crate::common;

use eve_db::{
    corp_roles, AllianceRecord, CorporationRecord, InventoryFlag, InventoryItemRecord,
    TrainedSkillRecord, UpwellStructureRecord,
};
use eve_destiny::{Ball, DVec3, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::attr_ids;
use eve_server::router::ServerContext;
use eve_server::services::{
    combat_crimewatch, sov_services, station_structure::structure_lifecycle_rpc,
};
use std::collections::HashMap;

const P1: i64 = 90_000_001;
const P2: i64 = 90_000_002;
const JITA: u32 = 30_000_142;
const MAURASI: u32 = 30_000_141;
const JITA_44: i64 = 60_003_760;

fn new_ctx() -> ServerContext {
    let sde = std::sync::Arc::new(
        eve_sde::SdeRegistry::open(&eve_sde::SdeRegistry::default_db_path()).expect("open sde"),
    );
    let db = std::sync::Arc::new(eve_db::WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(sde, db);
    ctx.set_clock_paused(true);
    ctx
}

async fn pilot(ctx: &ServerContext, sid: u64, user: &str, pass: &str, char_id: i64) -> SessionState {
    let mut s = SessionState::new(sid);
    common::login_pilot(ctx, &mut s, user, pass).await;
    let args = [PyRep::Integer(char_id)];
    ctx.dispatch_rpc(&mut s, "charUnboundMgr", None, "SelectCharacterID", &args, &HashMap::new(), None)
        .await
        .expect("SelectCharacterID");
    s
}

async fn three_pilots(ctx: &ServerContext) -> (SessionState, SessionState, SessionState, i64) {
    let s1 = pilot(ctx, 701, "demo", "demo-secret", P1).await;
    let s2 = pilot(ctx, 702, "other", "other-secret", P2).await;
    let uid = ctx.db.provision_player_account("inc7p3", "inc7p3-secret");
    let p3 = ctx.db.create_character(uid, "Inc7 Outsider", 1, 1, 11, 17, &ctx.sde).expect("p3").character_id;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&p3) {
            ch.solar_system_id = MAURASI;
        }
    });
    let mut s3 = pilot(ctx, 703, "inc7p3", "inc7p3-secret", p3).await;
    s3.solar_system_id = Some(MAURASI);
    for c in [P1, P2, p3] {
        ctx.drain_notifications_for_char(c);
    }
    (s1, s2, s3, p3)
}

async fn call(ctx: &ServerContext, s: &mut SessionState, svc: &str, method: &str, args: &[PyRep]) -> PyRep {
    ctx.dispatch_rpc(s, svc, None, method, args, &HashMap::new(), None)
        .await
        .unwrap_or_else(|e| panic!("{svc}.{method} failed: {} {}", e.code, e.message))
}

fn events(ctx: &ServerContext, char_id: i64, event: &str) -> Vec<Vec<PyRep>> {
    let all = ctx.drain_notifications_for_char(char_id);
    let mut matched = Vec::new();
    let mut keep = Vec::new();
    for n in all {
        if n.event == event {
            matched.push(n.args);
        } else {
            keep.push(n);
        }
    }
    if !keep.is_empty() {
        if let Ok(mut guard) = ctx.pending_notifications.write() {
            guard.entry(char_id).or_default().extend(keep);
        }
    }
    matched
}

fn seed_item(ctx: &ServerContext, item_id: i64, type_id: u32, owner_id: i64, location_id: i64, flag: InventoryFlag, quantity: i32) {
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            item_id,
            InventoryItemRecord {
                item_id,
                type_id,
                owner_id,
                location_id,
                flag,
                quantity,
                singleton: quantity == 1,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });
}

fn setup_player_corp_and_alliance(ctx: &ServerContext, corp_id: i64, alliance_id: i64) {
    ctx.db.with_mut_snapshot(|snap| {
        let mut roles = HashMap::new();
        roles.insert(P1, corp_roles::ROLE_DIRECTOR | corp_roles::ROLE_STATION_MANAGER);
        let mut members = std::collections::HashSet::new();
        members.insert(P1);
        snap.corporations.insert(
            corp_id,
            CorporationRecord {
                corporation_id: corp_id,
                name: "Upwell Parity Corp".into(),
                ticker: "UPWL".into(),
                ceo_id: P1,
                alliance_id: Some(alliance_id),
                tax_rate: 0.05,
                master_wallet: eve_db::IskCents::from_isk(100_000_000),
                members,
                member_roles: roles,
                applications: HashMap::new(),
                wallet_divisions: Vec::new(),
                description: String::new(),
                url: String::new(),
                hq_station_id: Some(JITA_44),
            },
        );
        let mut member_corp_ids = std::collections::HashSet::new();
        member_corp_ids.insert(corp_id);
        snap.alliances.insert(
            alliance_id,
            AllianceRecord {
                alliance_id,
                name: "Upwell Parity Alliance".into(),
                short_name: "UPWA".into(),
                executor_corp_id: corp_id,
                member_corp_ids,
            },
        );
        if let Some(ch) = snap.characters.get_mut(&P1) {
            ch.corporation_id = corp_id;
            ch.alliance_id = Some(alliance_id);
        }
    });
}

#[tokio::test]
async fn test_structure_dock_undock_and_lifecycle_notifications() {
    // parity: notify:OnDockingAccepted, notify:OnDockingFinished, notify:OnCharacterEnteredStructure, notify:OnCharacterLeftStructure, notify:OnHangarViewStateUpdated, notify:OnInvulunOnUndockingUpdated, notify:OnStructureAdded, notify:OnCorporationStructuresUpdated, notify:OnStructureStateChanged, notify:OnCurrentStructureStateChanged, notify:OnStructureServiceChanged
    eve_testkit::attest(&[
        "notify:OnDockingAccepted",
        "notify:OnDockingFinished",
        "notify:OnCharacterEnteredStructure",
        "notify:OnCharacterLeftStructure",
        "notify:OnHangarViewStateUpdated",
        "notify:OnInvulunOnUndockingUpdated",
        "notify:OnStructureAdded",
        "notify:OnCorporationStructuresUpdated",
        "notify:OnStructureStateChanged",
        "notify:OnCurrentStructureStateChanged",
        "notify:OnStructureServiceChanged",
    ]);

    let ctx = new_ctx();
    let (mut s1, _s2, _s3, p3) = three_pilots(&ctx).await;
    let (corp_id, alliance_id) = (98_000_701, 99_000_701);
    setup_player_corp_and_alliance(&ctx, corp_id, alliance_id);
    s1.corporation_id = Some(corp_id);
    s1.alliance_id = Some(alliance_id);
    s1.corp_role = corp_roles::ROLE_DIRECTOR | corp_roles::ROLE_STATION_MANAGER;

    // 1. Undock from station so P1 is in space in Jita, move far from stations, and anchor an Astrahus (35832).
    call(&ctx, &mut s1, "structureDocking", "Undock", &[]).await;
    assert_eq!(events(&ctx, P1, "OnInvulunOnUndockingUpdated").len(), 1);
    assert!(events(&ctx, p3, "OnInvulunOnUndockingUpdated").is_empty());

    let ship_id = s1.ship_id.expect("ship");
    let open_pos = ctx.sde.stations[&JITA_44].position + DVec3::new(0.0, 5.0e9, 0.0);
    assert!(ctx.systems.get_or_spawn(JITA).set_ball_position(ship_id, open_pos).await);
    seed_item(&ctx, 770_000, 35832, P1, ship_id, InventoryFlag::Cargo, 1);

    let anchor_res = call(
        &ctx,
        &mut s1,
        "structureDeployment",
        "Anchor",
        &[PyRep::Integer(35832), PyRep::String("Jita Test Citadel".into())],
    )
    .await;
    let structure_id = anchor_res.dict_get("structureID").and_then(|v| v.as_i64()).expect("structureID");

    assert_eq!(events(&ctx, P1, "OnStructureAdded"), vec![vec![PyRep::Integer(i64::from(JITA)), PyRep::Integer(structure_id)]]);
    assert_eq!(events(&ctx, P1, "OnCorporationStructuresUpdated"), vec![vec![]]);
    assert_eq!(events(&ctx, P1, "OnStructureStateChanged"), vec![vec![PyRep::Integer(i64::from(JITA)), PyRep::Integer(structure_id), PyRep::String("AnchorVulnerable".into())]]);
    assert_eq!(events(&ctx, P1, "OnCurrentStructureStateChanged"), vec![vec![PyRep::Integer(i64::from(JITA)), PyRep::Integer(structure_id), PyRep::String("AnchorVulnerable".into())]]);
    assert!(events(&ctx, p3, "OnStructureAdded").is_empty());

    // Mark structure ShieldVulnerable with Quantum Core and fuel blocks so P1 can dock and fit a service module.
    let prof = eve_server::services::station_structure::structure_lifecycle::structure_profile(&ctx, 35832).expect("profile");
    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = eve_server::services::station_structure::structure_upwell_ops::load_ext_state(snap);
        ext.lifecycle.remove(&structure_id);
        eve_server::services::station_structure::structure_upwell_ops::save_ext_state(snap, &ext);
        if let Some(st) = snap.structures.get_mut(&structure_id) {
            st.state = "ShieldVulnerable".into();
            st.quantum_core_installed = true;
            st.fuel_blocks = 1_000;
            st.shield_hp = prof.shield_hp;
            st.armor_hp = prof.armor_hp;
            st.hull_hp = prof.hull_hp;
            st.position = open_pos.to_array();
        }
    });

    // 2. Dock into the structure: emits OnDockingAccepted, OnDockingFinished, OnCharacterEnteredStructure, OnHangarViewStateUpdated.
    call(&ctx, &mut s1, "structureDocking", "Dock", &[PyRep::Integer(structure_id)]).await;
    assert_eq!(events(&ctx, P1, "OnDockingAccepted"), vec![vec![PyRep::Integer(structure_id)]]);
    assert_eq!(events(&ctx, P1, "OnDockingFinished"), vec![vec![PyRep::Integer(structure_id)]]);
    let entered = events(&ctx, P1, "OnCharacterEnteredStructure");
    assert_eq!(entered.len(), 1);
    assert_eq!(entered[0][0], PyRep::Integer(P1));
    assert_eq!(entered[0][1], PyRep::Integer(corp_id));
    assert_eq!(entered[0][2], PyRep::Integer(alliance_id));
    let hangar_view = events(&ctx, P1, "OnHangarViewStateUpdated");
    assert_eq!(hangar_view.len(), 1);
    assert_eq!(hangar_view[0][0], PyRep::Integer(structure_id));
    assert!(events(&ctx, p3, "OnDockingAccepted").is_empty());

    // 3. Fit and online a Standup Cloning Center I (35894) in ServiceSlot0: emits OnStructureServiceChanged.
    let svc_item_id = 770_001;
    seed_item(&ctx, svc_item_id, 35894, P1, structure_id, InventoryFlag::Hangar, 1);
    structure_lifecycle_rpc::handle_structure_fit_move(&ctx, &mut s1, svc_item_id, structure_id, InventoryFlag::ServiceSlot0, true)
        .expect("fit service module");
    assert_eq!(events(&ctx, P1, "OnStructureServiceChanged"), vec![vec![PyRep::Integer(structure_id), PyRep::Integer(35894), PyRep::Bool(true)]]);

    // 4. Undock from the structure: emits OnCharacterLeftStructure and OnInvulunOnUndockingUpdated.
    call(&ctx, &mut s1, "structureDocking", "Undock", &[]).await;
    assert_eq!(events(&ctx, P1, "OnCharacterLeftStructure"), vec![vec![PyRep::Integer(P1)]]);
    let invuln = events(&ctx, P1, "OnInvulunOnUndockingUpdated");
    assert_eq!(invuln.len(), 1);
    assert_eq!(invuln[0][2], PyRep::Integer(30_000));
}

#[tokio::test]
async fn test_structure_upwell_asset_safety_and_access_notifications() {
    // parity: notify:OnAssetSafetyCreated, notify:OnAssetSafetyDelivered, notify:OnStructureHacked, notify:OnSlimItemChange, notify:OnStructureTargetRemoved, notify:OnProfileSettingsChange, notify:OnOwnerGroupsUpdated, notify:OnDockingAccessChangedForCurrentSolarSystem, notify:OnStructureRemoved, notify:OnStructureRemovedFromSpace, notify:OnHomeStationMovedFromStructure
    eve_testkit::attest(&[
        "notify:OnAssetSafetyCreated",
        "notify:OnAssetSafetyDelivered",
        "notify:OnStructureHacked",
        "notify:OnSlimItemChange",
        "notify:OnStructureTargetRemoved",
        "notify:OnProfileSettingsChange",
        "notify:OnOwnerGroupsUpdated",
        "notify:OnDockingAccessChangedForCurrentSolarSystem",
        "notify:OnStructureRemoved",
        "notify:OnStructureRemovedFromSpace",
        "notify:OnHomeStationMovedFromStructure",
    ]);

    let ctx = new_ctx();
    let (mut s1, _s2, _s3, p3) = three_pilots(&ctx).await;
    let (corp_id, alliance_id) = (98_000_702, 99_000_702);
    setup_player_corp_and_alliance(&ctx, corp_id, alliance_id);
    s1.corporation_id = Some(corp_id);
    s1.alliance_id = Some(alliance_id);
    s1.corp_role = corp_roles::ROLE_DIRECTOR | corp_roles::ROLE_STATION_MANAGER;

    let structure_id = 880_001_001;
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            structure_id,
            UpwellStructureRecord {
                structure_id,
                type_id: 35832,
                owner_corp_id: corp_id,
                solar_system_id: JITA,
                name: "Jita Upwell Alpha".into(),
                position: [50_000.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                fuel_blocks: 500,
                quantum_core_installed: true,
                shield_hp: 1_000_000.0,
                armor_hp: 1_000_000.0,
                hull_hp: 1_000_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
            },
        );
        if let Some(ch) = snap.characters.get_mut(&P1) {
            ch.home_station_id = structure_id;
        }
    });

    // 1. RenameStructure -> emits OnSlimItemChange(structureID, slimKeyVal).
    call(&ctx, &mut s1, "structureDeployment", "RenameStructure", &[PyRep::Integer(structure_id), PyRep::String("Jita Upwell Prime".into())]).await;
    let slim_changes = events(&ctx, P1, "OnSlimItemChange");
    assert_eq!(slim_changes.len(), 1);
    assert_eq!(slim_changes[0][0], PyRep::Integer(structure_id));
    assert_eq!(slim_changes[0][1].dict_get("name"), Some(&PyRep::String("Jita Upwell Prime".into())));
    assert!(events(&ctx, p3, "OnSlimItemChange").is_empty());

    // 2. UpdateReinforceTimesFromClient -> emits OnStructureHacked(structureID, timerTuple).
    call(&ctx, &mut s1, "structureVulnerability", "UpdateReinforceTimesFromClient", &[PyRep::Integer(structure_id), PyRep::Integer(20), PyRep::Integer(3)]).await;
    let hacked = events(&ctx, P1, "OnStructureHacked");
    assert_eq!(hacked.len(), 1);
    assert_eq!(hacked[0][0], PyRep::Integer(structure_id));

    // 3. SetStructureDescription -> emits OnProfileSettingsChange, OnOwnerGroupsUpdated, OnDockingAccessChangedForCurrentSolarSystem.
    call(&ctx, &mut s1, "structureDirectory", "SetStructureDescription", &[PyRep::Integer(structure_id), PyRep::String("Public Market Hub".into())]).await;
    assert_eq!(events(&ctx, P1, "OnProfileSettingsChange"), vec![vec![PyRep::List(vec![PyRep::Integer(structure_id)])]]);
    assert_eq!(events(&ctx, P1, "OnOwnerGroupsUpdated").len(), 1);
    assert_eq!(events(&ctx, P1, "OnDockingAccessChangedForCurrentSolarSystem"), vec![vec![]]);

    // 4. TakeControl + ReleaseControl -> emits OnStructureTargetRemoved(structureID, targetID).
    call(&ctx, &mut s1, "structureControl", "TakeControl", &[PyRep::Integer(structure_id)]).await;
    call(&ctx, &mut s1, "structureControl", "ReleaseControl", &[PyRep::Integer(structure_id)]).await;
    assert_eq!(events(&ctx, P1, "OnStructureTargetRemoved"), vec![vec![PyRep::Integer(structure_id), PyRep::Integer(0)]]);

    // 5. MovePersonalAssetsToSafety & MoveSafetyWrapToStructure -> emits OnAssetSafetyCreated and OnAssetSafetyDelivered.
    seed_item(&ctx, 880_002_001, 34, P1, structure_id, InventoryFlag::Hangar, 500);
    let wrap_res = call(&ctx, &mut s1, "structureAssetSafety", "MovePersonalAssetsToSafety", &[PyRep::Integer(i64::from(JITA)), PyRep::Integer(structure_id)]).await;
    let wrap_id = wrap_res.dict_get("wrapID").or_else(|| wrap_res.dict_get("wrapItemID")).and_then(|v| v.as_i64()).expect("wrapID");
    assert_eq!(events(&ctx, P1, "OnAssetSafetyCreated"), vec![vec![PyRep::Integer(P1), PyRep::Integer(i64::from(JITA)), PyRep::Integer(structure_id)]]);

    ctx.advance_virtual_clock(5 * 86_400 + 10);
    call(&ctx, &mut s1, "structureAssetSafety", "MoveSafetyWrapToStructure", &[PyRep::Integer(wrap_id), PyRep::Integer(structure_id)]).await;
    assert_eq!(events(&ctx, P1, "OnAssetSafetyDelivered"), vec![vec![PyRep::Integer(P1)]]);

    // 6. Unanchor -> emits OnStructureRemoved, OnStructureRemovedFromSpace, OnHomeStationMovedFromStructure.
    call(&ctx, &mut s1, "structureDeployment", "Unanchor", &[PyRep::Integer(structure_id)]).await;
    assert_eq!(events(&ctx, P1, "OnStructureRemoved"), vec![vec![PyRep::Integer(i64::from(JITA)), PyRep::Integer(structure_id)]]);
    assert_eq!(events(&ctx, P1, "OnStructureRemovedFromSpace"), vec![vec![PyRep::Integer(structure_id)]]);
    assert_eq!(events(&ctx, P1, "OnHomeStationMovedFromStructure"), vec![vec![]]);
}

#[tokio::test]
async fn test_sovereignty_and_enlistment_notifications() {
    // parity: notify:OnSovereigntyChanged, notify:OnSolarSystemSovStructuresUpdated, notify:OnCapitalSystemChanged, notify:OnSovereigntyAudioEvent, notify:ProcessSovStatusChanged, notify:OnEnlistmentStateUpdated, notify:OnEnlistmentCooldownCleared
    eve_testkit::attest(&[
        "notify:OnSovereigntyChanged",
        "notify:OnSolarSystemSovStructuresUpdated",
        "notify:OnCapitalSystemChanged",
        "notify:OnSovereigntyAudioEvent",
        "ProcessSovStatusChanged",
        "notify:OnEnlistmentStateUpdated",
        "notify:OnEnlistmentCooldownCleared",
    ]);

    let ctx = new_ctx();
    let (mut s1, _s2, _s3, p3) = three_pilots(&ctx).await;
    let (corp_id, alliance_id) = (98_000_703, 99_000_703);
    setup_player_corp_and_alliance(&ctx, corp_id, alliance_id);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(st) = snap.system_states.get_mut(&JITA) {
            st.sov_alliance_id = None;
        }
    });

    sov_services::claim_system_sovereignty(&ctx, JITA, alliance_id, corp_id, 990_001).expect("claim_system_sovereignty");
    assert_eq!(events(&ctx, P1, "OnSovereigntyChanged"), vec![vec![PyRep::Integer(i64::from(JITA)), PyRep::Integer(alliance_id), PyRep::Integer(corp_id)]]);
    assert_eq!(events(&ctx, P1, "OnSolarSystemSovStructuresUpdated").len(), 1);
    assert_eq!(events(&ctx, P1, "OnCapitalSystemChanged"), vec![vec![PyRep::Tuple(vec![PyRep::Integer(alliance_id), PyRep::None, PyRep::Integer(i64::from(JITA))])]] );
    assert_eq!(events(&ctx, P1, "OnSovereigntyAudioEvent").len(), 1);
    assert_eq!(events(&ctx, P1, "ProcessSovStatusChanged").len(), 1);
    assert!(events(&ctx, p3, "OnCapitalSystemChanged").is_empty());

    call(&ctx, &mut s1, "fwCharacterEnlistmentMgr", "CreateMyDirectEnlistment", &[PyRep::Integer(500_001)]).await;
    assert_eq!(events(&ctx, P1, "OnEnlistmentStateUpdated"), vec![vec![PyRep::Integer(500_001), PyRep::Integer(500_001), PyRep::None]]);
    call(&ctx, &mut s1, "fwCharacterEnlistmentMgr", "RemoveMyDirectEnlistment", &[]).await;
    let _ = events(&ctx, P1, "OnEnlistmentStateUpdated");
    ctx.advance_virtual_clock(24 * 3_600 + 10);
    let cd = call(&ctx, &mut s1, "fwCharacterEnlistmentMgr", "GetMyDirectEnlistmentCooldownTimestamp", &[]).await;
    assert_eq!(cd, PyRep::None);
    assert_eq!(events(&ctx, P1, "OnEnlistmentCooldownCleared"), vec![vec![]]);
}

#[tokio::test]
async fn test_fleet_management_and_cyno_module_notifications() {
    // parity: notify:OnFleetInvite, notify:OnChangeFleetBoss, notify:OnFleetMove, notify:OnFleetMoveFailed, notify:OnJoinRequestUpdate, notify:OnMemberlessFleetUnregistered, notify:OnFleetStateChange, notify:OnBridgeModeChange, notify:OnFleetJumpBeaconModuleChange
    eve_testkit::attest(&[
        "notify:OnFleetInvite",
        "notify:OnChangeFleetBoss",
        "notify:OnFleetMove",
        "notify:OnFleetMoveFailed",
        "notify:OnJoinRequestUpdate",
        "notify:OnMemberlessFleetUnregistered",
        "notify:OnFleetStateChange",
        "notify:OnBridgeModeChange",
        "notify:OnFleetJumpBeaconModuleChange",
    ]);

    let ctx = new_ctx();
    let (mut s1, mut s2, mut s3, p3) = three_pilots(&ctx).await;
    const FH: &str = "fleetObjectHandler";

    call(&ctx, &mut s1, "fleetMgr", "CreateFleet", &[]).await;
    let fid = s1.fleet_id.expect("fleet id");

    call(&ctx, &mut s1, FH, "Invite", &[PyRep::Integer(P2)]).await;
    assert_eq!(events(&ctx, P2, "OnFleetInvite")[0][0], PyRep::Integer(fid));
    assert!(events(&ctx, p3, "OnFleetInvite").is_empty());
    call(&ctx, &mut s2, FH, "AcceptInvite", &[PyRep::Integer(fid)]).await;

    call(&ctx, &mut s1, FH, "MoveMember", &[PyRep::Integer(P2), PyRep::Integer(1), PyRep::Integer(1), PyRep::Integer(4)]).await;
    assert_eq!(events(&ctx, P2, "OnFleetMove"), vec![vec![]]);

    let move_err = ctx.dispatch_rpc(&mut s2, FH, None, "MoveMember", &[PyRep::Integer(P1), PyRep::Integer(1), PyRep::Integer(1), PyRep::Integer(4)], &HashMap::new(), None).await;
    assert!(move_err.is_err());
    assert_eq!(events(&ctx, P2, "OnFleetMoveFailed"), vec![vec![PyRep::Integer(P1), PyRep::Bool(false)]]);

    call(&ctx, &mut s1, "fleetProxy", "AddFleetFinderAdvert", &[PyRep::String("Ops".into()), PyRep::String("Ask".into()), PyRep::Integer(3), PyRep::Bool(false)]).await;
    call(&ctx, &mut s3, "fleetProxy", "ApplyToJoinFleet", &[PyRep::Integer(fid)]).await;
    call(&ctx, &mut s1, FH, "RejectJoinRequest", &[PyRep::Integer(p3)]).await;
    assert_eq!(events(&ctx, P1, "OnJoinRequestUpdate"), vec![vec![PyRep::Dict(vec![])]]);

    call(&ctx, &mut s1, "fleetMgr", "AddToWatchlist", &[PyRep::Integer(P2)]).await;
    assert_eq!(events(&ctx, P1, "OnFleetStateChange")[0][0], PyRep::Integer(P2));


    let mut cands: Vec<u32> = ctx.sde.solar_systems.values().filter(|s| s.security_status < 0.45 && s.solar_system_id < 31_000_000 && s.galactic_pos_ly != [0.0; 3]).map(|s| s.solar_system_id).collect();
    cands.sort_unstable();
    let mut found = (30_002_537, 30_002_538);
    'outer: for &a in &cands {
        let pa = ctx.sde.solar_systems[&a].galactic_pos_ly;
        for &b in &cands {
            if a != b {
                let pb = ctx.sde.solar_systems[&b].galactic_pos_ly;
                let d = ((pa[0] - pb[0]).powi(2) + (pa[1] - pb[1]).powi(2) + (pa[2] - pb[2]).powi(2)).sqrt();
                if (0.3..2.0).contains(&d) {
                    found = (a, b);
                    break 'outer;
                }
            }
        }
    }
    let (sys_a, sys_b) = found;

    for (cid, tid) in [(P1, 11957u32), (P1, 28646), (P2, 44996), (P2, 28652)] {
        ctx.db.with_mut_snapshot(|snap| {
            let entry = snap.skills.entry(cid).or_default();
            for aid in [attr_ids::REQUIRED_SKILL_1, attr_ids::REQUIRED_SKILL_2, attr_ids::REQUIRED_SKILL_3] {
                if let Some(&sk) = ctx.sde.types[&tid].default_attributes.get(&aid) {
                    if sk as u32 > 0 {
                        entry.insert(sk as u32, TrainedSkillRecord { type_id: sk as u32, level: 5, skill_points: 256_000.0 });
                    }
                }
            }
        });
    }
    seed_item(&ctx, 960_001, 11957, P1, sys_b as i64, InventoryFlag::None, 1);
    seed_item(&ctx, 960_002, 28646, P1, 960_001, InventoryFlag::HiSlot0, 1);
    seed_item(&ctx, 960_003, ctx.sde.types[&28646].default_attributes[&713] as u32, P1, 960_001, InventoryFlag::Cargo, 1_000);
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(960_002, true);
        if let Some(ch) = snap.characters.get_mut(&P1) {
            ch.active_ship_id = 960_001;
            ch.station_id = None;
            ch.solar_system_id = sys_b;
        }
    });
    s1.ship_id = Some(960_001);
    s1.station_id = None;
    s1.solar_system_id = Some(sys_b);
    ctx.invalidate_ship_fitting(960_001);
    let (cs1, meta1) = (ctx.build_ship_combat_state(P1, 960_001, 11957), ctx.build_ship_slim_meta(P1, 11957));
    ctx.systems.get_or_spawn(sys_b).spawn_ball_with_meta(Ball { id: 960_001, type_id: 11957, position: DVec3::ZERO, velocity: DVec3::ZERO, max_velocity: 150.0, inertia: 1.0, mass: 1e7, radius: 100.0, speed_fraction: 0.0, mode: MovementMode::Stop }, cs1, meta1).await.expect("spawn falcon");

    let cyno_res = call(&ctx, &mut s1, "dogmaIM", "Activate", &[PyRep::Integer(960_002), PyRep::String("cynosuralGeneration".into())]).await;
    let beacon_id = cyno_res.dict_get("beaconID").and_then(|v| v.as_i64()).expect("beaconID");
    assert_eq!(events(&ctx, P2, "OnFleetJumpBeaconModuleChange").len(), 1);

    seed_item(&ctx, 960_010, 44996, P2, sys_a as i64, InventoryFlag::None, 1);
    seed_item(&ctx, 960_011, 28652, P2, 960_010, InventoryFlag::HiSlot0, 1);
    seed_item(&ctx, 960_012, ctx.sde.types[&44996].default_attributes[&866] as u32, P2, 960_010, InventoryFlag::Cargo, 500_000);
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(960_011, true);
        if let Some(ch) = snap.characters.get_mut(&P2) {
            ch.active_ship_id = 960_010;
            ch.station_id = None;
            ch.solar_system_id = sys_a;
        }
    });
    s2.ship_id = Some(960_010);
    s2.station_id = None;
    s2.solar_system_id = Some(sys_a);
    ctx.invalidate_ship_fitting(960_010);
    let (cs2, meta2) = (ctx.build_ship_combat_state(P2, 960_010, 44996), ctx.build_ship_slim_meta(P2, 44996));
    ctx.systems.get_or_spawn(sys_a).spawn_ball_with_meta(Ball { id: 960_010, type_id: 44996, position: DVec3::ZERO, velocity: DVec3::ZERO, max_velocity: 100.0, inertia: 1.0, mass: 1e8, radius: 400.0, speed_fraction: 0.0, mode: MovementMode::Stop }, cs2, meta2).await.expect("spawn marshal");

    call(&ctx, &mut s2, "beyonce", "CmdBridgeToFleetModuleBeacon", &[PyRep::Integer(P1), PyRep::Integer(i64::from(sys_b)), PyRep::Integer(beacon_id)]).await;
    assert_eq!(events(&ctx, P1, "OnBridgeModeChange"), vec![vec![PyRep::Integer(960_010), PyRep::Integer(i64::from(sys_b)), PyRep::Integer(beacon_id), PyRep::Bool(true)]]);

    call(&ctx, &mut s1, FH, "MakeLeader", &[PyRep::Integer(P2)]).await;
    assert_eq!(events(&ctx, P2, "OnChangeFleetBoss"), vec![vec![PyRep::Integer(P1), PyRep::Integer(P2)]]);
    call(&ctx, &mut s2, FH, "DisbandFleet", &[]).await;
    assert_eq!(events(&ctx, P2, "OnMemberlessFleetUnregistered"), vec![vec![]]);
}

#[tokio::test]
async fn test_crimewatch_duels_kill_rights_and_bounties_notifications() {
    // parity: notify:OnDuelChallenge, notify:OnCrimewatchEngagementCreated, notify:OnCrimewatchEngagementStartTimeout, notify:OnCrimewatchEngagementStopTimeout, notify:OnCrimewatchEngagementEnded, notify:OnKillRightCreated, notify:OnKillRightActivated, notify:OnKillRightForYouSold, notify:OnKillRightUsed, notify:OnBountyPlaced, notify:OnBountyAddedToPayout, notify:OnSecurityStatusUpdate
    eve_testkit::attest(&[
        "notify:OnDuelChallenge",
        "notify:OnCrimewatchEngagementCreated",
        "notify:OnCrimewatchEngagementStartTimeout",
        "notify:OnCrimewatchEngagementStopTimeout",
        "notify:OnCrimewatchEngagementEnded",
        "notify:OnKillRightCreated",
        "notify:OnKillRightActivated",
        "notify:OnKillRightForYouSold",
        "notify:OnKillRightUsed",
        "notify:OnBountyPlaced",
        "notify:OnBountyAddedToPayout",
        "notify:OnSecurityStatusUpdate",
    ]);

    let ctx = new_ctx();
    let (mut s1, mut s2, _s3, p3) = three_pilots(&ctx).await;

    call(&ctx, &mut s1, "crimewatch", "StartDuelChallenge", &[PyRep::Integer(P2)]).await;
    assert_eq!(events(&ctx, P2, "OnDuelChallenge")[0][0], PyRep::Integer(P1));
    assert!(events(&ctx, p3, "OnDuelChallenge").is_empty());

    call(&ctx, &mut s2, "crimewatch", "RespondToDuelChallenge", &[PyRep::Integer(P1), PyRep::Bool(true)]).await;
    assert_eq!(events(&ctx, P1, "OnCrimewatchEngagementCreated")[0][0], PyRep::Integer(P2));

    combat_crimewatch::update_crimewatch_engagement_timers(&ctx, P1, P2);
    assert_eq!(events(&ctx, P1, "OnCrimewatchEngagementStartTimeout")[0][0], PyRep::Integer(P2));
    assert_eq!(events(&ctx, P1, "OnCrimewatchEngagementStopTimeout"), vec![vec![PyRep::Integer(P2)]]);
    assert_eq!(events(&ctx, P1, "OnCrimewatchEngagementEnded"), vec![vec![PyRep::Integer(P2)]]);

    combat_crimewatch::create_kill_right(&ctx, 7_001, P1, P2, 0.0);
    let kr_created = events(&ctx, P1, "OnKillRightCreated");
    assert_eq!(kr_created.len(), 1);
    assert_eq!(kr_created[0][0], PyRep::Integer(7_001));

    call(&ctx, &mut s1, "killRightMgr", "ActivateKillRight", &[PyRep::Integer(7_001)]).await;
    assert_eq!(events(&ctx, P1, "OnKillRightActivated"), vec![vec![]]);
    assert_eq!(events(&ctx, P1, "OnKillRightUsed"), vec![vec![PyRep::Integer(7_001), PyRep::Integer(P2)]]);

    combat_crimewatch::create_kill_right(&ctx, 7_002, P1, p3, 500_000.0);
    let _ = events(&ctx, P1, "OnKillRightCreated");
    call(&ctx, &mut s2, "killRightMgr", "BuyKillRight", &[PyRep::Integer(7_002)]).await;
    assert_eq!(events(&ctx, P1, "OnKillRightForYouSold"), vec![vec![PyRep::Integer(7_002)]]);
    assert_eq!(events(&ctx, P2, "OnKillRightActivated"), vec![vec![]]);

    call(&ctx, &mut s1, "bountyProxy", "AddToBounty", &[PyRep::Integer(P2), PyRep::Real(200_000.0)]).await;
    assert_eq!(events(&ctx, P2, "OnBountyPlaced"), vec![vec![PyRep::Integer(P1), PyRep::Integer(P2), PyRep::Real(200_000.0)]]);

    let claimed = combat_crimewatch::claim_player_bounty_on_kill(&ctx, P1, P2, 587);
    assert!(claimed > 0.0);
    let payout = events(&ctx, P1, "OnBountyAddedToPayout");
    assert_eq!(payout.len(), 1);
    assert_eq!(payout[0][0].dict_get("amount"), Some(&PyRep::Real(claimed)));
    assert_eq!(payout[0][0].dict_get("enemyTypeID"), Some(&PyRep::Integer(587)));

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&P1) {
            ch.security_status = -1.5;
        }
    });
    seed_item(&ctx, 970_001, 35664, P1, JITA_44, InventoryFlag::Hangar, 2);
    call(&ctx, &mut s1, "crimewatch", "ApplyTagsForSecStatus", &[PyRep::Integer(2)]).await;
    assert_eq!(events(&ctx, P1, "OnSecurityStatusUpdate"), vec![vec![PyRep::Real(-0.5)]]);
}
