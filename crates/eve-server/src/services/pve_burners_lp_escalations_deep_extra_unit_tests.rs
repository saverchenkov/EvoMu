use crate::router::ServerContext;
use crate::services::lp_store::CONCORD_CORP_ID;
use crate::services::pve_expeditions::{
    advance_expedition_step, create_escalation_expedition, get_my_escalating_path_details_rowset,
};
use eve_db::{
    ActiveMissionRecord, CharacterRecord, InventoryFlag, InventoryItemRecord, IskCents,
    LoyaltyStoreOfferItemRecord, LoyaltyStoreOfferRecord, NeuralAttributes, ServerClock,
    VirtualServerClock, WorldRepository,
};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::{
    CategoryId, DungeonTemplate, GroupId, InvType, MissionTemplate, NpcCorporationInfo,
    SdeRegistry, StationInfo, TypeId,
};
use glam::DVec3;
use std::collections::HashMap;
use std::sync::Arc;

fn mk_pve_type(
    type_id: TypeId,
    group_id: GroupId,
    category_id: CategoryId,
    name: &str,
    mass: f64,
    volume: f64,
) -> InvType {
    InvType {
        type_id,
        group_id,
        category_id,
        name: name.to_string(),
        mass,
        volume,
        capacity: 100.0,
        radius: 50.0,
        base_price: 100.0,
        published: true,
        portion_size: 1,
        market_group_id: None,
        race_id: None,
        faction_id: None,
        variation_parent_type_id: None,
        description_id: None,
        icon_id: None,
        graphic_id: None,
        sound_id: None,
        meta_group_id: None,
        meta_level: None,
        tech_level: None,
        default_effect_id: None,
        description: String::new(),
        effect_ids: Vec::new(),
        default_attributes: HashMap::new(),
    }
}

fn mk_char(character_id: i64, user_id: u64, name: &str, station_id: Option<i64>, ship_id: i64) -> CharacterRecord {
    CharacterRecord {
        activity_status: None,
        character_id,
        user_id,
        name: name.to_string(),
        corporation_id: 1_000_035,
        alliance_id: None,
        war_faction_id: None,
        station_id,
        structure_id: None,
        solar_system_id: 30_000_142,
        constellation_id: 20_000_020,
        region_id: 10_000_002,
        active_ship_id: ship_id,
        balance: IskCents::from_isk(10_000_000),
        plex_balance: 0,
        loyalty_points: Default::default(),
        standings: Default::default(),
        security_status: 5.0,
        clone_grade: 2,
        home_station_id: 60_003_760,
        race_id: 1,
        bloodline_id: 1,
        ancestry_id: 1,
        school_id: 1,
        neural: NeuralAttributes {
            intelligence: 20,
            memory: 20,
            perception: 20,
            willpower: 20,
            charisma: 19,
            bonus_remaps: 0,
            last_remap_unix: None,
            remap_cooldown_until_unix: None,
        },
        skill_points: 0.0,
        free_skill_points: 0.0,
        last_clone_jump_unix: 0,
        jump_fatigue_minutes: 0.0,
        jump_cooldown_until_unix: 0,
        jump_fatigue_until_unix: 0,
        description: String::new(),
        title: String::new(),
        gender: 1,
        create_date_unix: 0,
        fleet_id: None,
        autopilot_waypoints: Vec::new(),
        version: 1,
        system_epoch: 1,
    }
}

fn build_pve_extra_sde() -> Arc<SdeRegistry> {
    let mut sde = SdeRegistry::with_canonical_seed();
    sde.types.insert(22474, mk_pve_type(22474, 324, 6, "Assault Frigate", 1_100_000.0, 27_500.0));
    sde.types.insert(638, mk_pve_type(638, 27, 6, "Raven Battleship", 100_000_000.0, 500_000.0));
    sde.types.insert(3242, mk_pve_type(3242, 324, 6, "Heavy Mass Special Hull", 50_000_000.0, 50_000.0));
    sde.types.insert(17202, mk_pve_type(17202, 314, 17, "Caldari Navy Tag", 1.0, 0.1));
    sde.types.insert(31942, mk_pve_type(31942, 74, 7, "Caldari Navy Ballistic Control System", 5.0, 5.0));
    sde.types.insert(19419, mk_pve_type(19419, 300, 17, "Overseer's Personal Effects", 1.0, 1.0));

    let corp_empire = 1_000_035i64;
    let corp_ore = 1_000_129i64;
    let corp_pirate = 1_000_127i64;
    let station_id = 60_003_760i64;

    for (cid, name, ticker, fid) in [
        (corp_empire, "Caldari Navy", "CN", Some(500_001)),
        (corp_ore, "Outer Ring Excavations", "ORE", Some(500_014)),
        (corp_pirate, "Guristas", "GURI", Some(500_010)),
    ] {
        sde.npc_corporations.insert(
            cid,
            NpcCorporationInfo {
                corporation_id: cid,
                name: name.into(),
                ticker_name: ticker.into(),
                faction_id: fid,
                solar_system_id: Some(30_000_142),
                station_id: Some(station_id),
                tax_rate: 0.1,
                ceo_id: None,
            },
        );
    }

    sde.stations.entry(station_id).or_insert(StationInfo {
        station_id,
        solar_system_id: 30_000_142,
        constellation_id: 20_000_020,
        region_id: 10_000_002,
        station_type_id: 1531,
        name: "Jita IV - Caldari Navy Assembly Plant".into(),
        position: DVec3::ZERO,
        owner_corporation_id: corp_empire,
        owner_faction_id: Some(500_001),
        operation_id: 1,
        reprocessing_efficiency: 0.5,
        reprocessing_stations_take: 0.05,
        is_conquerable: false,
        orbit_id: None,
        has_clone_bay: true,
    });

    sde.missions.insert(
        40_001,
        MissionTemplate {
            mission_id: 40_001,
            title: "Anomic Agent - Dramiel".into(),
            is_courier: false,
            lp_alpha: 4_000,
            lp_omega: 6_000,
            has_standing_rewards: true,
            content_template: Some("BasicKillMission".into()),
            dungeon_id: None,
        },
    );

    for did in [1229u32, 1432, 1433, 1434, 1435] {
        sde.dungeons.insert(
            did,
            DungeonTemplate {
                dungeon_id: did,
                name: format!("Escalation Site {did}"),
                archetype_id: None,
                faction_id: Some(500_012),
                difficulty: Some(4),
            },
        );
    }
    Arc::new(sde)
}

async fn call_keeper_gate(
    ctx: &ServerContext,
    sess: &mut SessionState,
    mission_id: u32,
) -> Option<Result<PyRep, eve_proto::BridgeRpcError>> {
    crate::services::agents_missions::agents_standings_arcs::handle_extended(
        ctx,
        sess,
        "keeper",
        "ActivateAccelerationGate",
        &[PyRep::Integer(mission_id as i64)],
        &HashMap::new(),
    )
    .await
}

#[tokio::test]
async fn test_anomic_burner_gate_ship_group_and_mass_restrictions() {
    let db = Arc::new(WorldRepository::new_empty());
    let clock = Arc::new(VirtualServerClock::with_fixed_base(50_000));
    let ctx = ServerContext::with_sde_and_repository(build_pve_extra_sde(), db);
    ctx.set_clock(clock);
    let char_id = 92_000_001i64;
    let mission_id = 40_001u32;
    let (bs_id, heavy_id, frig_id) = (5_000_001i64, 5_000_002i64, 5_000_003i64);

    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.insert(char_id, mk_char(char_id, 10, "BurnerRunner", None, bs_id));
        for (sid, tid) in [(bs_id, 638u32), (heavy_id, 3242u32), (frig_id, 22474u32)] {
            snap.items.insert(
                sid,
                InventoryItemRecord {
                    item_id: sid,
                    type_id: tid,
                    owner_id: char_id,
                    location_id: 30_000_142,
                    flag: InventoryFlag::Unlocked,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
        }
        snap.active_missions.insert(
            (char_id, mission_id),
            ActiveMissionRecord {
                mission_id,
                character_id: char_id,
                agent_id: 3_010_004,
                solar_system_id: 30_000_142,
                state: "Accepted".into(),
                current_room_id: 1,
                npcs_remaining: 1,
                spawned_npc_ball_ids: Vec::new(),
                bookmark_id: None,
            },
        );
    });

    let mut sess = SessionState::new(10);
    sess.user_id = Some(10);
    sess.character_id = Some(char_id);
    sess.solar_system_id = Some(30_000_142);
    sess.station_id = Some(60_003_760);
    sess.ship_id = Some(bs_id);

    assert_eq!(call_keeper_gate(&ctx, &mut sess, mission_id).await.unwrap().unwrap_err().code, "ShipDocked");
    sess.station_id = None;
    assert_eq!(call_keeper_gate(&ctx, &mut sess, mission_id).await.unwrap().unwrap_err().code, "ShipRestrictedByGate");
    sess.ship_id = Some(heavy_id);
    assert_eq!(call_keeper_gate(&ctx, &mut sess, mission_id).await.unwrap().unwrap_err().code, "ShipRestrictedByGate");
    sess.ship_id = Some(frig_id);
    assert_eq!(call_keeper_gate(&ctx, &mut sess, mission_id).await.unwrap().unwrap_err().code, "RoomNpcsRemaining");

    ctx.db.with_mut_snapshot(|snap| {
        snap.active_missions.get_mut(&(char_id, mission_id)).unwrap().npcs_remaining = 0;
    });
    let ok = call_keeper_gate(&ctx, &mut sess, mission_id).await.unwrap().expect("frigate unlocks gate");
    assert_eq!(ok.dict_get("currentRoomID").and_then(|v| v.as_i64()), Some(2));
    assert_eq!(ok.dict_get("gateUnlocked").and_then(|v| v.as_bool()), Some(true));
}

#[tokio::test]
async fn test_lp_store_trade_in_item_filtering_and_concord_lp_exchange_rates() {
    let db = Arc::new(WorldRepository::new_empty());
    let clock = Arc::new(VirtualServerClock::with_fixed_base(60_000));
    let ctx = ServerContext::with_sde_and_repository(build_pve_extra_sde(), db);
    ctx.set_clock(clock);
    let char_id = 92_000_002i64;
    let station_id = 60_003_760i64;
    let ship_id = 5_100_001i64;
    let corp_empire = 1_000_035i64;
    let corp_ore = 1_000_129i64;
    let offer_id = 9_900_001i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = mk_char(char_id, 11, "LPTrader", Some(station_id), ship_id);
        ch.loyalty_points.insert(CONCORD_CORP_ID, 200);
        ch.loyalty_points.insert(corp_empire, 5_000);
        ch.loyalty_points.insert(corp_ore, 100);
        snap.characters.insert(char_id, ch);

        snap.loyalty_store_offers.insert(
            corp_empire,
            vec![LoyaltyStoreOfferRecord {
                offer_id,
                type_id: 31942,
                quantity: 1,
                lp_cost: 1_000,
                isk_cost: 500_000,
                ak_cost: None,
                required_items: vec![LoyaltyStoreOfferItemRecord {
                    type_id: 17202,
                    quantity: 5,
                }],
            }],
        );
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 22474,
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );

        let base_tag = |item_id: i64, flag: InventoryFlag| InventoryItemRecord {
            item_id,
            type_id: 17202,
            owner_id: char_id,
            location_id: station_id,
            flag,
            quantity: 10,
            ..Default::default()
        };
        snap.items.insert(6_001, base_tag(6_001, InventoryFlag::Locked));
        let mut bpc = base_tag(6_002, InventoryFlag::Hangar);
        bpc.is_blueprint_copy = true;
        snap.items.insert(6_002, bpc);
        let mut damaged = base_tag(6_003, InventoryFlag::Hangar);
        damaged.dynamic_attributes.insert(3, 1.0);
        snap.items.insert(6_003, damaged);
        let mut muta = base_tag(6_004, InventoryFlag::Hangar);
        muta.source_mutaplasmid_id = Some(47_740);
        snap.items.insert(6_004, muta);
        snap.items.insert(6_005, base_tag(6_005, InventoryFlag::Hangar));
        snap.item_custom_names.insert(6_005, "courier_cargo:99".into());

        // Only 3 valid tags in Hangar + 2 valid tags in docked ship's Cargo (= 5 total)
        snap.items.insert(
            6_010,
            InventoryItemRecord {
                quantity: 3,
                ..base_tag(6_010, InventoryFlag::Hangar)
            },
        );
        snap.items.insert(
            6_011,
            InventoryItemRecord {
                location_id: ship_id,
                quantity: 2,
                ..base_tag(6_011, InventoryFlag::Cargo)
            },
        );
    });

    let mut sess = SessionState::new(11);
    sess.user_id = Some(11);
    sess.character_id = Some(char_id);
    sess.solar_system_id = Some(30_000_142);
    sess.station_id = Some(station_id);
    sess.ship_id = Some(ship_id);

    let err_missing = crate::services::lp_store::handle(
        &ctx,
        &mut sess,
        "LPStoreMgr",
        "TakeOfferForCharacter",
        &[PyRep::Integer(corp_empire), PyRep::Integer(offer_id), PyRep::Integer(2)],
        &HashMap::new(),
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(err_missing.code, "MissingRequiredItems");

    let ok_take = crate::services::lp_store::handle(
        &ctx,
        &mut sess,
        "LPStoreMgr",
        "TakeOfferForCharacter",
        &[PyRep::Integer(corp_empire), PyRep::Integer(offer_id), PyRep::Integer(1)],
        &HashMap::new(),
    )
    .unwrap()
    .expect("qty 1 succeeds");
    assert_eq!(ok_take.dict_get("deliveredTypeID").and_then(|v| v.as_i64()), Some(31942));
    assert_eq!(ok_take.dict_get("remainingLP").and_then(|v| v.as_i64()), Some(4_000));
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&6_010));
        assert!(!snap.items.contains_key(&6_011));
        assert!(snap.items.contains_key(&6_001));
    });

    let empire_ex = crate::services::lp_store::handle(
        &ctx,
        &mut sess,
        "LPStoreMgr",
        "ExchangeConcordLP",
        &[PyRep::Integer(corp_empire), PyRep::Integer(100)],
        &HashMap::new(),
    )
    .unwrap()
    .expect("empire exchange ok");
    assert_eq!(empire_ex.dict_get("convertedLP").and_then(|v| v.as_i64()), Some(80));
    assert_eq!(empire_ex.dict_get("remainingConcordLP").and_then(|v| v.as_i64()), Some(100));

    let ore_ex = crate::services::lp_store::handle(
        &ctx,
        &mut sess,
        "LPStoreMgr",
        "ExchangeConcordLP",
        &[PyRep::Integer(corp_ore), PyRep::Integer(100)],
        &HashMap::new(),
    )
    .unwrap()
    .expect("ORE exchange ok");
    assert_eq!(ore_ex.dict_get("convertedLP").and_then(|v| v.as_i64()), Some(40));
    assert_eq!(ore_ex.dict_get("remainingConcordLP").and_then(|v| v.as_i64()), Some(0));
}

#[tokio::test]
async fn test_escalation_expedition_progression_and_expiry_pruning() {
    let db = Arc::new(WorldRepository::new_empty());
    let clock = Arc::new(VirtualServerClock::with_fixed_base(80_000));
    let ctx = ServerContext::with_sde_and_repository(build_pve_extra_sde(), db);
    ctx.set_clock(clock.clone());
    let char_id = 92_000_003i64;
    let sys_id = 30_000_142u32;

    let entry = create_escalation_expedition(
        &ctx,
        char_id,
        sys_id,
        1229,
        "Blood Raider Fleet Staging Point",
        [10_000.0, 0.0, 10_000.0],
    )
    .await
    .expect("escalation created");
    assert_eq!(entry.step_index, 1);
    assert_eq!(entry.total_steps, 4);
    ctx.db.with_snapshot(|snap| {
        assert!(snap
            .bookmarks
            .values()
            .any(|b| b.owner_id == char_id && b.item_id == Some(entry.instance_id)));
    });

    if let PyRep::Rowset { rows, .. } = get_my_escalating_path_details_rowset(&ctx, char_id) {
        assert_eq!(rows.len(), 1);
    } else {
        panic!("expected Rowset");
    }

    let step2 = advance_expedition_step(&ctx, char_id, entry.instance_id)
        .await
        .expect("advance ok")
        .expect("still active");
    assert_eq!(step2.step_index, 2);

    let secs_to_expiry = step2.expires_at_sec.saturating_sub(ctx.now_unix()) + 1;
    clock.advance_secs(secs_to_expiry);
    if let PyRep::Rowset { rows, .. } = get_my_escalating_path_details_rowset(&ctx, char_id) {
        assert!(rows.is_empty(), "expired escalation must be pruned");
    } else {
        panic!("expected Rowset");
    }
    ctx.db.with_snapshot(|snap| {
        assert!(!snap
            .bookmarks
            .values()
            .any(|b| b.owner_id == char_id && b.item_id == Some(entry.instance_id)));
    });
}
