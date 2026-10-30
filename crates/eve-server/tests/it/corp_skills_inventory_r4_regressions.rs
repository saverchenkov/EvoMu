//! Round-4 Track 3 Regression Tests (`#176`–`#187`):
//! Skills, Fitting, Inventory, Stacking, Corporation Governance & Agents.

#![forbid(unsafe_code)]

use eve_db::{
    CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents, NeuralAttributes,
    SkillQueueEntry, TrainedSkillRecord,
};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::{BoundObjectEntry, ServerContext};
use std::collections::{HashMap, HashSet};

fn make_session(char_id: i64, corp_id: i64, station_id: Option<i64>, sys_id: u32, ship_id: i64) -> SessionState {
    let mut s = SessionState::default();
    s.user_id = Some(1);
    s.character_id = Some(char_id);
    s.corporation_id = Some(corp_id);
    s.station_id = station_id;
    s.solar_system_id = Some(sys_id);
    s.ship_id = Some(ship_id);
    s
}

#[tokio::test]
async fn r4_176_180_181_183_ship_bay_passwords_alsc_locks_and_container_roles() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let station_id = 60_003_760_i64;
    let sys_id = 30_000_142_u32;
    let corp_id = 2_000_501_i64;
    let owner_id = 95_001_001_i64;
    let fleetmate_id = 95_001_002_i64;
    let host_ship_id = 880_001_001_i64;
    let target_ship_id = 880_001_002_i64;
    let alsc_id = 880_001_010_i64;
    let trit_a = 880_001_011_i64;
    let trit_b = 880_001_012_i64;
    let incompatible_item = 880_001_013_i64;
    let corp_alsc_id = 880_001_020_i64;
    let corp_item_id = 880_001_021_i64;

    let carrier_tid = ctx
        .sde
        .types
        .values()
        .find(|t| {
            t.category_id == 6
                && t.default_attributes.get(&908).copied().unwrap_or(0.0) >= 100_000.0
                && t.default_attributes.get(&912).copied().unwrap_or(0.0) >= 1_000.0
        })
        .map(|t| t.type_id)
        .unwrap_or(28606); // Orca

    let alsc_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.group_id == 448 && t.capacity >= 1_000.0)
        .map(|t| t.type_id)
        .unwrap_or(17366);

    ctx.db.with_mut_snapshot(|snap| {
        let base_ch = snap.characters.get(&90_000_001).cloned().expect("seeded char");
        for &cid in &[owner_id, fleetmate_id] {
            let mut ch = base_ch.clone();
            ch.character_id = cid;
            ch.name = format!("Pilot {cid}");
            ch.corporation_id = corp_id;
            ch.solar_system_id = sys_id;
            ch.station_id = Some(station_id);
            ch.active_ship_id = if cid == owner_id { host_ship_id } else { 0 };
            ch.balance = IskCents::from_isk(10_000_000);
            ch.clone_grade = 2;
            snap.characters.insert(cid, ch);
        }
        let mut members = HashSet::new();
        members.insert(owner_id);
        members.insert(fleetmate_id);
        let mut member_roles = HashMap::new();
        member_roles.insert(owner_id, u64::MAX);
        // Give fleetmate HangarCanTake1 (1 << 13) and HangarCanQuery1 (1 << 20), but NOT ContainerCanTake1 (1 << 43)
        member_roles.insert(fleetmate_id, (1u64 << 13) | (1u64 << 20));
        snap.corporations.insert(
            corp_id,
            CorporationRecord {
                corporation_id: corp_id,
                name: "R4 Test Corp".into(),
                ticker: "R4TC".into(),
                ceo_id: owner_id,
                members,
                member_roles,
                ..Default::default()
            },
        );
        snap.items.insert(
            host_ship_id,
            InventoryItemRecord {
                item_id: host_ship_id,
                type_id: carrier_tid,
                owner_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            target_ship_id,
            InventoryItemRecord {
                item_id: target_ship_id,
                type_id: 587, // Rifter
                owner_id: fleetmate_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            alsc_id,
            InventoryItemRecord {
                item_id: alsc_id,
                type_id: alsc_tid,
                owner_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            trit_a,
            InventoryItemRecord {
                item_id: trit_a,
                type_id: 34,
                owner_id,
                location_id: alsc_id,
                flag: InventoryFlag::Unlocked,
                quantity: 100,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            trit_b,
            InventoryItemRecord {
                item_id: trit_b,
                type_id: 34,
                owner_id,
                location_id: alsc_id,
                flag: InventoryFlag::Unlocked,
                quantity: 50,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            incompatible_item,
            InventoryItemRecord {
                item_id: incompatible_item,
                type_id: 35, // Pyerite (different type)
                owner_id,
                location_id: alsc_id,
                flag: InventoryFlag::Unlocked,
                quantity: 25,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            corp_alsc_id,
            InventoryItemRecord {
                item_id: corp_alsc_id,
                type_id: alsc_tid,
                owner_id: corp_id,
                location_id: station_id,
                flag: InventoryFlag::CorpSAG1,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            corp_item_id,
            InventoryItemRecord {
                item_id: corp_item_id,
                type_id: 34,
                owner_id: corp_id,
                location_id: corp_alsc_id,
                flag: InventoryFlag::Unlocked,
                quantity: 10,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    let mut owner_sess = make_session(owner_id, corp_id, Some(station_id), sys_id, host_ship_id);
    let mut mate_sess = make_session(fleetmate_id, corp_id, Some(station_id), sys_id, 0);
    let kw = HashMap::new();

    ctx.dispatch_rpc(
        &mut owner_sess,
        "ship",
        None,
        "ConfigureShip",
        &[
            PyRep::Integer(host_ship_id),
            PyRep::Dict(vec![
                (PyRep::String("FleetHangar_AllowFleetAccess".into()), PyRep::Bool(true)),
                (PyRep::String("FleetHangar_AllowCorpAccess".into()), PyRep::Bool(true)),
                (PyRep::String("SMB_AllowFleetAccess".into()), PyRep::Bool(true)),
                (PyRep::String("SMB_AllowCorpAccess".into()), PyRep::Bool(true)),
            ]),
        ],
        &kw,
        None,
    )
    .await
    .expect("ConfigureShip");
    ctx.dispatch_rpc(
        &mut owner_sess,
        "ship",
        None,
        "SetShipPassword",
        &[
            PyRep::Integer(host_ship_id),
            PyRep::String("secret-bay".into()),
        ],
        &kw,
        None,
    )
    .await
    .expect("SetShipPassword");

    // #176: StoreVessel without password must be rejected with SMAAccessDenied
    let no_pw_res = ctx
        .dispatch_rpc(
            &mut mate_sess,
            "ship",
            None,
            "StoreVessel",
            &[PyRep::Integer(target_ship_id), PyRep::Integer(host_ship_id)],
            &kw,
            None,
        )
        .await;
    assert!(
        matches!(&no_pw_res, Err(e) if e.code == "SMAAccessDenied"),
        "expected SMAAccessDenied when password omitted, got {no_pw_res:?}"
    );

    // #176: StoreVessel with correct password succeeds
    let mut pw_kw = HashMap::new();
    pw_kw.insert("password".into(), PyRep::String("secret-bay".into()));
    ctx.dispatch_rpc(
        &mut mate_sess,
        "ship",
        None,
        "StoreVessel",
        &[PyRep::Integer(target_ship_id), PyRep::Integer(host_ship_id)],
        &pw_kw,
        None,
    )
    .await
    .expect("StoreVessel with valid password should succeed");

    // #183: Lock trit_a inside ALSC and verify MultiMerge rejects it,
    // and partial MultiMerge onto incompatible_item does not leave split stacks behind.
    ctx.dispatch_rpc(
        &mut owner_sess,
        "invbroker",
        None,
        "ALSCLockItems",
        &[PyRep::Integer(alsc_id), PyRep::List(vec![PyRep::Integer(trit_a)])],
        &kw,
        None,
    )
    .await
    .expect("ALSCLockItems");

    let merge_locked = ctx
        .dispatch_rpc(
            &mut owner_sess,
            "invbroker",
            None,
            "MultiMerge",
            &[PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::Integer(trit_b),
                PyRep::Integer(trit_a),
                PyRep::Integer(10),
            ])])],
            &kw,
            None,
        )
        .await;
    assert!(
        matches!(&merge_locked, Err(e) if e.code == "ALSCItemLocked"),
        "expected ALSCItemLocked on MultiMerge with locked item, got {merge_locked:?}"
    );

    let item_count_before = ctx.db.with_snapshot(|s| s.items.len());
    let merge_incompat = ctx
        .dispatch_rpc(
            &mut owner_sess,
            "invbroker",
            None,
            "MultiMerge",
            &[PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::Integer(trit_b),
                PyRep::Integer(incompatible_item),
                PyRep::Integer(10),
            ])])],
            &kw,
            None,
        )
        .await;
    assert!(merge_incompat.is_err(), "merging different types must fail");
    let (item_count_after, trit_b_qty) = ctx.db.with_snapshot(|s| {
        (
            s.items.len(),
            s.items.get(&trit_b).map(|i| i.quantity).unwrap_or(0),
        )
    });
    assert_eq!(item_count_after, item_count_before, "failed partial MultiMerge must not leave split stacks behind");
    assert_eq!(trit_b_qty, 50, "source stack quantity must remain intact when merge fails");

    // #180 & #181: Set password on ALSC container; verify bound List requires password
    // and corp container take requires ROLE_CONTAINER_CAN_TAKE (1 << 43).
    ctx.dispatch_rpc(
        &mut owner_sess,
        "invbroker",
        None,
        "SetPassword",
        &[PyRep::Integer(alsc_id), PyRep::String("alsc-pass".into())],
        &kw,
        None,
    )
    .await
    .expect("SetPassword on ALSC");

    let bound_alsc = BoundObjectEntry {
        bound_id: 99991,
        session_id: mate_sess.session_id,
        user_id: 1,
        character_id: fleetmate_id,
        service: "invbroker".into(),
        object_id: alsc_id, oid_str: "N=888444:99991".into(), ref_id: 133_000_000_000_000_000,
    };
    ctx.bound_objects.write().unwrap().insert(99991, bound_alsc);
    let list_no_pw = ctx
        .dispatch_rpc(&mut mate_sess, "invbroker", Some(99991), "List", &[], &kw, None)
        .await;
    assert!(
        matches!(&list_no_pw, Err(e) if e.code == "ALSCPasswordRequired"),
        "expected ALSCPasswordRequired on bound List without password, got {list_no_pw:?}"
    );

    let take_no_cont_role = ctx
        .dispatch_rpc(
            &mut mate_sess,
            "invbroker",
            None,
            "Add",
            &[PyRep::Integer(corp_item_id), PyRep::Integer(station_id), PyRep::Integer(4)],
            &kw,
            None,
        )
        .await;
    assert!(
        matches!(&take_no_cont_role, Err(e) if e.code == "CorpRoleDenied"),
        "expected CorpRoleDenied when lacking ContainerCanTake1, got {take_no_cont_role:?}"
    );

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.corporations.get_mut(&corp_id) {
            *c.member_roles.entry(fleetmate_id).or_default() |= 1u64 << 43;
        }
    });
    ctx.dispatch_rpc(
        &mut mate_sess,
        "invbroker",
        None,
        "Add",
        &[PyRep::Integer(corp_item_id), PyRep::Integer(station_id), PyRep::Integer(4)],
        &kw,
        None,
    )
    .await
    .expect("Add from corp container with ContainerCanTake1 role");
}

#[tokio::test]
async fn r4_177_178_179_182_184_185_186_187_skills_corp_ammo_agents_and_killcounter() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let station_id = 60_003_760_i64;
    let sys_id = 30_000_142_u32;
    let corp_id = 2_000_502_i64;
    let ceo_id = 95_002_001_i64;
    let member_id = 95_002_002_i64;
    let ship_id = 880_002_001_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let base_ch = snap.characters.get(&90_000_001).cloned().expect("seeded char");
        for &cid in &[ceo_id, member_id] {
            let mut ch = base_ch.clone();
            ch.character_id = cid;
            ch.name = format!("Pilot {cid}");
            ch.corporation_id = corp_id;
            ch.solar_system_id = sys_id;
            ch.station_id = Some(station_id);
            ch.active_ship_id = ship_id;
            ch.balance = IskCents::from_isk(50_000_000);
            ch.clone_grade = 2;
            ch.neural = NeuralAttributes {
                charisma: 20,
                intelligence: 20,
                memory: 20,
                perception: 20,
                willpower: 20,
                bonus_remaps: 1,
                last_remap_unix: None,
                remap_cooldown_until_unix: None,
            };
            snap.characters.insert(cid, ch);
        }
        let mut members = HashSet::new();
        members.insert(ceo_id);
        members.insert(member_id);
        let mut member_roles = HashMap::new();
        member_roles.insert(ceo_id, u64::MAX);
        snap.corporations.insert(
            corp_id,
            CorporationRecord {
                corporation_id: corp_id,
                name: "R4 Gov Corp".into(),
                ticker: "R4GC".into(),
                ceo_id,
                members,
                member_roles,
                ..Default::default()
            },
        );
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 24698, // Drake (Battlecruiser, supports Command Bursts)
                owner_id: ceo_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });

    let mut ceo_sess = make_session(ceo_id, corp_id, Some(station_id), sys_id, ship_id);
    ceo_sess.corp_role = u64::MAX;
    let kw = HashMap::new();

    // #187: SetShipKillCounter requires GM role
    let kill_non_gm = ctx
        .dispatch_rpc(
            &mut ceo_sess,
            "shipKillCounter",
            None,
            "SetShipKillCounter",
            &[PyRep::Integer(ship_id), PyRep::Integer(42)],
            &kw,
            None,
        )
        .await;
    assert!(
        matches!(&kill_non_gm, Err(e) if e.code == "GMRoleRequired"),
        "expected GMRoleRequired on SetShipKillCounter without GM role, got {kill_non_gm:?}"
    );

    // #182: ResignFromCEO rejects new_ceo = 0 when corp has > 1 member, and clears former CEO's roles on valid succession
    let resign_zero = ctx
        .dispatch_rpc(
            &mut ceo_sess,
            "corpRegistry",
            None,
            "ResignFromCEO",
            &[PyRep::Integer(0)],
            &kw,
            None,
        )
        .await;
    assert!(
        matches!(&resign_zero, Err(e) if e.code == "InvalidNewCEO"),
        "expected InvalidNewCEO when resigning with 0 while other members exist, got {resign_zero:?}"
    );

    ctx.dispatch_rpc(
        &mut ceo_sess,
        "corpRegistry",
        None,
        "ResignFromCEO",
        &[PyRep::Integer(member_id)],
        &kw,
        None,
    )
    .await
    .expect("ResignFromCEO to valid member");
    assert_eq!(ceo_sess.corp_role, 0, "resigning CEO session.corp_role must be cleared");
    let former_ceo_roles = ctx.db.with_snapshot(|s| {
        s.corporations
            .get(&corp_id)
            .and_then(|c| c.member_roles.get(&ceo_id).copied())
            .unwrap_or(0)
    });
    assert_eq!(former_ceo_roles, 0, "resigning CEO member_roles must be cleared to 0");

    // #178: AbortTraining catches up accrued skill training before pausing
    let now_unix = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(ceo_id).or_default().insert(
            3300,
            TrainedSkillRecord {
                type_id: 3300,
                level: 0,
                skill_points: 0.0,
            },
        );
        snap.skill_queues.insert(
            ceo_id,
            vec![SkillQueueEntry {
                type_id: 3300,
                target_level: 1,
                start_sp: 0.0,
                end_sp: 250.0,
                started_at_unix: Some(now_unix.saturating_sub(180)),
                duration_seconds: 600.0,
            }],
        );
    });
    ctx.dispatch_rpc(&mut ceo_sess, "skillMgr2", None, "AbortTraining", &[], &kw, None)
        .await
        .expect("AbortTraining");
    let accrued_sp = ctx.db.with_snapshot(|s| {
        s.skills
            .get(&ceo_id)
            .and_then(|m| m.get(&3300))
            .map(|sk| sk.skill_points)
            .unwrap_or(0.0)
    });
    assert!(accrued_sp > 0.0, "AbortTraining must catch up and preserve accrued SP (got {accrued_sp})");

    // #179: Command Burst charge with quantity == 1 is consumed and removed on cycle
    let burst_mod_id = 880_002_050_i64;
    let burst_charge_id = 880_002_051_i64;
    let burst_mod_tid = 42526_u32; // Shield Command Burst I
    let burst_charge_tid = 42832_u32; // Active Shielding Charge
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            burst_mod_id,
            InventoryItemRecord {
                item_id: burst_mod_id,
                type_id: burst_mod_tid,
                owner_id: ceo_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            burst_charge_id,
            InventoryItemRecord {
                item_id: burst_charge_id,
                type_id: burst_charge_tid,
                owner_id: ceo_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(burst_mod_id, true);
    });
    ctx.invalidate_ship_fitting(ship_id);
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let fit = ctx.get_or_rebuild_ship_fitting(ceo_id, ship_id).expect("fit");
    let mod_type = ctx.sde.types.get(&burst_mod_tid).expect("burst mod type");
    let mod_state = eve_dogma::DogmaItemState::from_sde(burst_mod_id, burst_mod_tid, &ctx.sde);
    eve_server::services::tactical_modules::activate_command_burst(
        &ctx,
        &sys_handle,
        ceo_id,
        ship_id,
        sys_id,
        burst_mod_id,
        mod_type,
        &mod_state,
        Some(burst_charge_tid),
        &fit,
    )
    .await
    .expect("activate_command_burst with 1 charge");
    let charge_still_exists = ctx.db.with_snapshot(|s| s.items.contains_key(&burst_charge_id));
    assert!(!charge_still_exists, "1-unit Command Burst charge must be consumed and removed");

    // #177: R&D StartResearch starts at 0.0 RP (no instant free RP on StartResearch)
    if let Some((&rnd_agent_id, rnd_agent)) = ctx.sde.agents.iter().find(|(_, a)| a.agent_type_id == 4) {
        let rnd_sys = rnd_agent.solar_system_id.unwrap_or(sys_id);
        let rnd_sta = rnd_agent.station_id.or(Some(station_id));
        let skill_tid = 11442_u32; // Hydromagnetic Physics
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&ceo_id) {
                ch.solar_system_id = rnd_sys;
                ch.station_id = rnd_sta;
                ch.standings.insert(rnd_agent_id, 8.0);
                ch.standings.insert(rnd_agent.corporation_id as i64, 8.0);
            }
            for sid in [3402, 3392, skill_tid] {
                snap.skills.entry(ceo_id).or_default().insert(
                    sid,
                    TrainedSkillRecord {
                        type_id: sid,
                        level: 5,
                        skill_points: 256_000.0,
                    },
                );
            }
        });
        let mut rnd_sess = make_session(ceo_id, corp_id, rnd_sta, rnd_sys, ship_id);
        ctx.dispatch_rpc(
            &mut rnd_sess,
            "agentMgr",
            None,
            "DoAction",
            &[
                PyRep::Integer(12),
                PyRep::Integer(rnd_agent_id),
                PyRep::Integer(skill_tid as i64),
            ],
            &kw,
            None,
        )
        .await
        .expect("StartResearch");
        // Immediately attempting to buy a datacore at t=0 must fail with InsufficientResearchPoints (0.0 initial RP)
        let buy_dc = ctx
            .dispatch_rpc(
                &mut rnd_sess,
                "agentMgr",
                None,
                "DoAction",
                &[
                    PyRep::Integer(14),
                    PyRep::Integer(rnd_agent_id),
                    PyRep::Integer(1),
                ],
                &kw,
                None,
            )
            .await;
        assert!(
            matches!(&buy_dc, Err(e) if e.code == "InsufficientResearchPoints"),
            "expected InsufficientResearchPoints at t=0 after StartResearch, got {buy_dc:?}"
        );
    }

    // #186: Locator agent rejects targets in Wormhole space (>= 31_000_000)
    if let Some((&loc_agent_id, loc_agent)) = ctx
        .sde
        .agents
        .iter()
        .find(|(_, a)| a.is_locator && a.level == 1 && a.solar_system_id.is_some())
    {
        let loc_sys = loc_agent.solar_system_id.unwrap();
        let loc_sta = loc_agent.station_id.or(Some(station_id));
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&ceo_id) {
                ch.solar_system_id = loc_sys;
                ch.station_id = loc_sta;
                ch.standings.insert(loc_agent_id, 5.0);
                ch.standings.insert(loc_agent.corporation_id as i64, 5.0);
            }
            if let Some(target) = snap.characters.get_mut(&member_id) {
                target.solar_system_id = 31_000_005; // J-space wormhole system
                target.station_id = None;
            }
        });
        let mut loc_sess = make_session(ceo_id, corp_id, loc_sta, loc_sys, ship_id);
        let wh_locate = ctx
            .dispatch_rpc(
                &mut loc_sess,
                "agentMgr",
                None,
                "DoAction",
                &[
                    PyRep::Integer(15),
                    PyRep::Integer(loc_agent_id),
                    PyRep::Integer(member_id),
                ],
                &kw,
                None,
            )
            .await;
        assert!(
            matches!(&wh_locate, Err(e) if e.code == "LocatorTargetInUnknownSpace" || e.code == "TargetInUnreachableSpace"),
            "expected LocatorTargetInUnknownSpace when locating character in wormhole space, got {wh_locate:?}"
        );
    }
}
