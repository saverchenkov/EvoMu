use crate::router::ServerContext;
use crate::services::lp_store_catalog::LPOffer;
use eve_db::corp_roles::{
    ROLE_ACCOUNTANT, ROLE_ACCOUNT_CAN_TAKE, ROLE_BRAND_MANAGER, ROLE_DELIVERIES_HANGAR_TAKE,
    ROLE_DIRECTOR, ROLE_HANGAR_CAN_TAKE,
};
use eve_db::{
    CharacterRecord, ChatChannelAclRecord, ChatChannelRecord, CorpGovState, CorporationRecord,
    InventoryFlag, InventoryItemRecord, IskCents, TrainedSkillRecord, VirtualServerClock,
    WorldRepository, WorldStateSnapshot,
};
use eve_destiny::{Ball, CombatState, DVec3, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::{RaceInfo, SdeRegistry};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| {
        let mut sde = SdeRegistry::with_canonical_seed();
        for (rid, name, bl_id) in [(1u32, "Caldari", 1u32), (2u32, "Minmatar", 2u32)] {
            sde.races.entry(rid).or_insert(RaceInfo {
                race_id: rid,
                name: name.into(),
                description: String::new(),
                corvette_type_id: Some(606),
                starter_skills: HashMap::new(),
            });
            sde.bloodlines.entry(bl_id).or_insert(eve_sde::BloodlineInfo {
                bloodline_id: bl_id,
                name: name.into(),
                race_id: rid,
                corporation_id: 1_000_044 + rid as i64,
                ship_type_id: Some(606),
                description: String::new(),
                perception: 20,
                memory: 20,
                willpower: 20,
                intelligence: 20,
                charisma: 19,
            });
        }
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
    ch.solar_system_id = 30000142;
    ch.home_station_id = st;
    ch.active_ship_id = ship;
    ch.clone_grade = 2;
    ch.balance = IskCents(1_000_000_000_00);
    ch
}

fn sess(uid: u64, cid: i64, corp: i64, roles: u64, st: Option<i64>, ship: Option<i64>) -> SessionState {
    let mut s = SessionState::default();
    s.user_id = Some(uid);
    s.character_id = Some(cid);
    s.corporation_id = Some(corp);
    s.corp_role = roles;
    s.station_id = st;
    s.solar_system_id = Some(30000142);
    s.ship_id = ship;
    s
}

fn mk_ball(id: i64, pos: DVec3) -> Ball {
    Ball {
        id,
        type_id: 606,
        position: pos,
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 1.0,
        mass: 1_000_000.0,
        radius: 50.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    }
}

#[test]
fn r6_t3_07_corp_lp_store_enforces_roles_24h_delay_wallet_and_division_access() {
    // Regression for #390: TakeOfferForCorporation role, 24h grant delay, wallet take, and division take enforcement
    let now = 1_700_000_000u64;
    let ctx = new_ctx(now);
    let (st, corp, ceo, brand_mgr, acct, pending_dir, dir_no_wallet) =
        (60003760, 98_600_001, 90_600_001, 90_600_002, 90_600_003, 90_600_004, 90_600_005);
    let npc_corp = 1_000_035i64;
    let offer_id = 880_001i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut corp_rec = { let mut __corp = CorporationRecord::default();
            __corp.corporation_id = corp;
            __corp.name = "LPCorp".into();
            __corp.ticker = "LPC".into();
            __corp.ceo_id = ceo;
            __corp.hq_station_id = Some(st);
            __corp.members = [ceo, brand_mgr, acct, pending_dir, dir_no_wallet].into();
            __corp.member_roles = [
                (ceo, u64::MAX),
                (brand_mgr, ROLE_BRAND_MANAGER | ROLE_HANGAR_CAN_TAKE[0] | ROLE_DELIVERIES_HANGAR_TAKE | ROLE_ACCOUNT_CAN_TAKE[0]),
                (acct, ROLE_ACCOUNTANT | ROLE_HANGAR_CAN_TAKE[0] | ROLE_DELIVERIES_HANGAR_TAKE | ROLE_ACCOUNT_CAN_TAKE[0]),
                (pending_dir, ROLE_DIRECTOR | ROLE_HANGAR_CAN_TAKE[0] | ROLE_DELIVERIES_HANGAR_TAKE | ROLE_ACCOUNT_CAN_TAKE[0]),
                (dir_no_wallet, ROLE_HANGAR_CAN_TAKE[0] | ROLE_DELIVERIES_HANGAR_TAKE | ROLE_ACCOUNT_CAN_TAKE[0]),
            ].into();
            __corp.set_division_balance(1, IskCents::from_isk(100_000_000));
            __corp };
        corp_rec.ensure_wallet_divisions();
        corp_rec.wallet_divisions[0].balance = IskCents::from_isk(100_000_000);
        snap.corporations.insert(corp, corp_rec);

        let mut gov = CorpGovState::default();
        gov.pending_role_grants.insert(pending_dir, (ROLE_DIRECTOR, now + 86_400));
        snap.corp_governance.insert(corp, gov);

        for (i, cid) in [ceo, brand_mgr, acct, pending_dir, dir_no_wallet].into_iter().enumerate() {
            snap.characters.insert(cid, mk_char(snap, cid, 8000 + i as u64, corp, st, 0));
        }

        // Trade-in item placed ONLY in CorpSAG7; non-director without ROLE_HANGAR_CAN_TAKE[6] cannot consume it
        snap.items.insert(881_001, InventoryItemRecord {
            item_id: 881_001, type_id: 34, owner_id: corp, location_id: st,
            flag: InventoryFlag::CorpSAG7, quantity: 500, singleton: false, ..Default::default()
        });

        super::lp_store::save_corp_lp_balances(snap, corp, &HashMap::from([(npc_corp, 50_000)]));
        let offers = vec![LPOffer {
            offer_id,
            corp_id: npc_corp,
            type_id: 35,
            quantity: 10,
            lp_cost: 1_000,
            isk_cost: IskCents::from_isk(5_000_000),
            req_items: vec![(34, 100)],
            ak_cost: 0,
            min_standing: None,
        }];
        snap.fleet_and_social_ops.lp_store_offers = Some(offers);
    });

    let kw = HashMap::new();
    let args = [PyRep::Integer(npc_corp), PyRep::Integer(offer_id), PyRep::Integer(1)];

    // Brand Manager, Accountant, and 24h pending Director all rejected; corp wallet untouched
    for (uid, cid, roles) in [
        (8001, brand_mgr, ROLE_BRAND_MANAGER),
        (8002, acct, ROLE_ACCOUNTANT),
        (8003, pending_dir, 0),
        (8004, dir_no_wallet, ROLE_ACCOUNT_CAN_TAKE[0] | ROLE_HANGAR_CAN_TAKE[0]),
    ] {
        let mut s = sess(uid, cid, corp, roles, Some(st), None);
        let err = super::lp_store::handle(&ctx, &mut s, "LPStoreMgr", "TakeOfferForCorporation", &args, &kw)
            .unwrap()
            .unwrap_err();
        assert_eq!(err.code, "InsufficientCorporationRole");
    }
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.corporations[&corp].wallet_divisions[0].balance, IskCents::from_isk(100_000_000));
    });

    // CEO succeeds and consumes trade-in from CorpSAG7 + corp ISK wallet
    let mut s_ceo = sess(8000, ceo, corp, u64::MAX, Some(st), None);
    let ok = super::lp_store::handle(&ctx, &mut s_ceo, "LPStoreMgr", "TakeOfferForCorporation", &args, &kw)
        .unwrap()
        .unwrap();
    assert!(ok.dict_get("deliveredItemID").is_some());
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&881_001].quantity, 400);
        assert_eq!(snap.corporations[&corp].wallet_divisions[0].balance, IskCents::from_isk(95_000_000));
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r6_t3_08_bookmark_location_blocks_cloaked_and_offgrid_dynamic_entities() {
    // Regression for #391: Legacy bookmark.BookmarkLocation / beyonce.BookmarkLocation cloaked & off-grid recon bypass
    let now = 1_700_000_000u64;
    let ctx = new_ctx(now);
    let sys_id = 30000142u32;
    let (ch1, ship1, cloaked_ship, offgrid_ship) =
        (90_600_101i64, 88_100_001i64, 88_100_002i64, 88_100_003i64);

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    sys_handle.spawn_ball(mk_ball(ship1, DVec3::new(0.0, 0.0, 0.0))).await.unwrap();
    let mut cs = CombatState::new();
    cs.is_cloaked = true;
    sys_handle.spawn_ball_with_combat(mk_ball(cloaked_ship, DVec3::new(10_000.0, 0.0, 0.0)), cs).await.unwrap();
    sys_handle.spawn_ball(mk_ball(offgrid_ship, DVec3::new(50_000_000.0, 0.0, 0.0))).await.unwrap();

    let mut s = sess(8101, ch1, 1_000_044, 0, None, Some(ship1));
    let kw = HashMap::new();

    // Cloaked target rejected on bookmark.BookmarkLocation
    let err_cloak = ctx.dispatch_rpc(
        &mut s, "bookmark", None, "BookmarkLocation",
        &[PyRep::Integer(cloaked_ship), PyRep::Integer(0), PyRep::String("CloakRecon".into()), PyRep::String("".into())],
        &kw, None,
    ).await.unwrap_err();
    assert_eq!(err_cloak.code, "UserError");

    // Off-grid (> 8,000,000m) target rejected on beyonce.BookmarkLocation
    let err_far = ctx.dispatch_rpc(
        &mut s, "beyonce", None, "BookmarkLocation",
        &[PyRep::Integer(offgrid_ship), PyRep::Integer(0), PyRep::String("FarRecon".into()), PyRep::String("".into())],
        &kw, None,
    ).await.unwrap_err();
    assert_eq!(err_far.code, "UserError");

    // Docked caller cannot bookmark dynamic in-space entity
    let mut s_docked = sess(8101, ch1, 1_000_044, 0, Some(60003760), Some(ship1));
    assert_eq!(
        ctx.dispatch_rpc(
            &mut s_docked, "bookmark", None, "BookmarkLocation",
            &[PyRep::Integer(offgrid_ship), PyRep::Integer(0), PyRep::String("Docked".into()), PyRep::String("".into())],
            &kw, None,
        ).await.unwrap_err().code,
        "UserError"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r6_t3_09_chat_invite_resync_and_get_members_enforce_acl_and_membership() {
    // Regression for #392: LSC.Invite ACL bypass, XmppChatMgr.ResyncSystemChannelAccess spoofing, and LSC.GetMembers leak
    let now = 1_700_000_000u64;
    let ctx = new_ctx(now);
    let (owner, attacker, victim) = (90_600_201i64, 90_600_202i64, 90_600_203i64);
    let priv_chan = "channel:secret_ops".to_string();

    ctx.db.with_mut_snapshot(|snap| {
        for (i, cid) in [owner, attacker, victim].into_iter().enumerate() {
            snap.characters.insert(cid, mk_char(snap, cid, 8200 + i as u64, 1_000_044, 60003760, 0));
        }
        snap.chat_channels.insert(
            priv_chan.clone(),
            ChatChannelRecord {
                channel_id: priv_chan.clone(),
                name: "Secret Ops".into(),
                motd: "Secret Intel".into(),
                owner_id: Some(owner),
                members: HashSet::from([owner, victim]),
                is_system_local: false,
                solar_system_id: None,
                is_private: true,
                operators: HashSet::from([owner]),
                password: Some("hunter2".into()),
                is_delayed_local: false,
            },
        );
        snap.chat_channel_acl.insert(
            priv_chan.clone(),
            ChatChannelAclRecord {
                deny_list: HashSet::from([attacker]),
                ..Default::default()
            },
        );
    });

    let mut s_att = sess(8201, attacker, 1_000_044, 0, Some(60003760), None);
    let kw = HashMap::new();

    // 1. LSC.Invite rejects non-member / denied attacker without auto-adding them
    assert_eq!(
        ctx.dispatch_rpc(
            &mut s_att, "LSC", None, "Invite",
            &[PyRep::Integer(attacker), PyRep::String(priv_chan.clone())],
            &kw, None,
        ).await.unwrap_err().code,
        "ChannelAccessDenied"
    );
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.chat_channels[&priv_chan].members.contains(&attacker));
    });

    // 2. XmppChatMgr.ResyncSystemChannelAccess rejects private & foreign-system channel arguments
    assert_eq!(
        ctx.dispatch_rpc(
            &mut s_att, "XmppChatMgr", None, "ResyncSystemChannelAccess",
            &[PyRep::String(priv_chan.clone())],
            &kw, None,
        ).await.unwrap_err().code,
        "ChannelAccessDenied"
    );
    assert_eq!(
        ctx.dispatch_rpc(
            &mut s_att, "XmppChatMgr", None, "ResyncSystemChannelAccess",
            &[PyRep::String("solarsystemid2:30002187".into())],
            &kw, None,
        ).await.unwrap_err().code,
        "ChannelAccessDenied"
    );

    // 3. LSC.GetMembers rejects non-member attacker; allows actual channel owner
    assert_eq!(
        ctx.dispatch_rpc(
            &mut s_att, "LSC", None, "GetMembers",
            &[PyRep::String(priv_chan.clone())],
            &kw, None,
        ).await.unwrap_err().code,
        "ChannelAccessDenied"
    );
    let mut s_owner = sess(8200, owner, 1_000_044, 0, Some(60003760), None);
    assert!(ctx.dispatch_rpc(
        &mut s_owner, "LSC", None, "GetMembers",
        &[PyRep::String(priv_chan)],
        &kw, None,
    ).await.is_ok());
}

#[test]
fn r6_t3_14_and_15_select_character_kwargs_lock_and_bloodline_race_token_guards() {
    // Regression for #397 (kwargs SelectCharacterID bypass) and #398 (unconsumed resculpt token & cross-race switch)
    let now = 1_700_000_000u64;
    let ctx = new_ctx(now);
    let (uid, cid) = (8300u64, 90_600_301i64);

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = mk_char(snap, cid, uid, 1_000_044, 60003760, 0);
        ch.race_id = 1;
        ch.bloodline_id = 1;
        snap.characters.insert(cid, ch);
        snap.cluster_and_runtime.character_locks.insert(cid, "banned".into());
    });

    let mut s = SessionState::default();
    s.user_id = Some(uid);
    let mut kw_select = HashMap::new();
    kw_select.insert("charID".into(), PyRep::Integer(cid));

    // #397: SelectCharacterID with charID in kwargs must still enforce lock and pending deletion
    assert_eq!(
        super::character_lifecycle::handle(&ctx, &mut s, "charUnboundMgr", "SelectCharacterID", &[], &kw_select)
            .unwrap()
            .unwrap_err()
            .code,
        "CharacterLocked"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.cluster_and_runtime.character_locks.remove(&cid);
    });
    let kw = HashMap::new();
    super::character_lifecycle::handle(&ctx, &mut s, "charUnboundMgr", "PrepareCharacterForDelete", &[PyRep::Integer(cid)], &kw)
        .unwrap()
        .unwrap();
    assert_eq!(
        super::character_lifecycle::handle(&ctx, &mut s, "charUnboundMgr", "SelectCharacterID", &[], &kw_select)
            .unwrap()
            .unwrap_err()
            .code,
        "CharacterLocked"
    );

    // #398: Token is decremented (single-use) on valid resculpt; cross-race blocked once skills exist
    let minmatar_bl = *ctx.sde.bloodlines.iter().find(|(_, b)| b.race_id != 1).unwrap().0;
    let minmatar_race = ctx.sde.bloodlines[&minmatar_bl].race_id;
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.remove(&cid);
        snap.cluster_and_runtime.account_resculpt_tokens.insert(uid.to_string(), 1);
    });
    super::character_lifecycle::handle(
        &ctx, &mut s, "charUnboundMgr", "UpdateCharacterBloodline",
        &[PyRep::Integer(cid), PyRep::Integer(minmatar_bl as i64)], &kw,
    ).unwrap().unwrap();
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.cluster_and_runtime.account_resculpt_tokens.get(&uid.to_string()).copied(), Some(0));
        assert_eq!(snap.characters[&cid].race_id, minmatar_race);
    });
    assert_eq!(
        super::character_lifecycle::handle(
            &ctx, &mut s, "charUnboundMgr", "UpdateCharacterBloodline",
            &[PyRep::Integer(cid), PyRep::Integer(1)], &kw,
        ).unwrap().unwrap_err().code,
        "ResculptRequired"
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.cluster_and_runtime.account_resculpt_tokens.insert(uid.to_string(), 3);
        snap.skills.entry(cid).or_default().insert(3327, TrainedSkillRecord {
            type_id: 3327,
            level: 3,
            skill_points: 20_000.0,
        });
    });
    assert_eq!(
        super::character_lifecycle::handle(
            &ctx, &mut s, "charUnboundMgr", "UpdateCharacterBloodline",
            &[PyRep::Integer(cid), PyRep::Integer(1)], &kw,
        ).unwrap().unwrap_err().code,
        "RaceChangeNotAllowed"
    );
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.characters[&cid].race_id, minmatar_race);
    });
}
