//! Round-5 Track 3 Regression Tests (`#225`–`#237`):
//! Skills, Clones, Fitting, Containers, Fleet, Corporation Governance, Projects & Agents.

#![forbid(unsafe_code)]

use eve_db::{
    CharacterRecord, CorpApplicationRecord, CorporationRecord, FleetMemberRecord, FleetRecord,
    FleetRole, InventoryFlag, InventoryItemRecord, IskCents, SkillQueueEntry, TrainedSkillRecord,
};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::{service_roles, SessionState};
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use glam::DVec3;
use std::collections::{HashMap, HashSet};

fn make_session(char_id: i64, corp_id: i64, station_id: Option<i64>, sys_id: u32, ship_id: i64) -> SessionState {
    let mut s = SessionState::default();
    s.user_id = Some(1);
    s.character_id = Some(char_id);
    s.corporation_id = Some(corp_id);
    s.station_id = station_id;
    s.solar_system_id = Some(sys_id);
    s.ship_id = if ship_id > 0 { Some(ship_id) } else { None };
    s.role = service_roles::ROLE_PLAYER | service_roles::ROLE_LOGIN;
    s
}

fn clone_seeded_char(
    base: &CharacterRecord,
    char_id: i64,
    corp_id: i64,
    station_id: Option<i64>,
    sys_id: u32,
    ship_id: i64,
) -> CharacterRecord {
    let mut ch = base.clone();
    ch.character_id = char_id;
    ch.name = format!("R5 Pilot {char_id}");
    ch.corporation_id = corp_id;
    ch.station_id = station_id;
    ch.structure_id = None;
    ch.solar_system_id = sys_id;
    ch.active_ship_id = ship_id;
    ch.balance = IskCents::from_isk(50_000_000);
    ch.clone_grade = 2;
    ch
}

#[tokio::test]
async fn r5_225_226_227_231_corp_wallet_transfer_applications_titles_votes_and_ads() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let station_id = 60_003_760_i64;
    let sys_id = 30_000_142_u32;
    let (corp_a, corp_b) = (2_100_001_i64, 2_100_002_i64);
    let (ceo_a, jr_acct, member_no_shares, ceo_b) = (96_001_001_i64, 96_001_002_i64, 96_001_003_i64, 96_001_004_i64);
    let kw = HashMap::new();

    ctx.db.with_mut_snapshot(|snap| {
        let base = snap.characters.get(&90_000_001).cloned().expect("seeded");
        for &(cid, corp) in &[(ceo_a, corp_a), (jr_acct, corp_a), (member_no_shares, corp_a), (ceo_b, corp_b)] {
            snap.characters.insert(cid, clone_seeded_char(&base, cid, corp, Some(station_id), sys_id, 0));
        }
        let roles_a = HashMap::from([
            (ceo_a, u64::MAX),
            // Junior Accountant (512) + AccountCanTake1 (1 << 20), NO AccountCanTake2 (1 << 21)
            (jr_acct, 512 | (1u64 << 20)),
            (member_no_shares, 0),
        ]);
        let mut corp_rec_a = CorporationRecord {
            corporation_id: corp_a,
            name: "R5 Corp A".into(),
            ticker: "R5CA".into(),
            ceo_id: ceo_a,
            master_wallet: IskCents::from_isk(10_000_000),
            members: HashSet::from([ceo_a, jr_acct, member_no_shares]),
            member_roles: roles_a,
            ..Default::default()
        };
        corp_rec_a.ensure_wallet_divisions();
        corp_rec_a.set_division_balance(1, IskCents::from_isk(10_000_000));
        corp_rec_a.set_division_balance(2, IskCents::from_isk(5_000_000));
        snap.corporations.insert(corp_a, corp_rec_a);

        let mut corp_rec_b = CorporationRecord {
            corporation_id: corp_b,
            name: "R5 Corp B".into(),
            ticker: "R5CB".into(),
            ceo_id: ceo_b,
            members: HashSet::from([ceo_b]),
            member_roles: HashMap::from([(ceo_b, u64::MAX)]),
            ..Default::default()
        };
        corp_rec_b.ensure_wallet_divisions();
        snap.corporations.insert(corp_b, corp_rec_b);
    });

    // #225: GiveCashFromCorpAccount blocks self-transfer, enforces division take role, and debits source
    let mut sess_jr = make_session(jr_acct, corp_a, Some(station_id), sys_id, 0);
    let err_same = ctx
        .dispatch_rpc(&mut sess_jr, "account", None, "GiveCashFromCorpAccount", &[PyRep::Integer(1), PyRep::Integer(1), PyRep::Real(1_000_000.0)], &kw, None)
        .await
        .expect_err("same division must be rejected");
    assert_eq!(err_same.code, "InvalidTransferDivision");

    let err_div2 = ctx
        .dispatch_rpc(&mut sess_jr, "account", None, "GiveCashFromCorpAccount", &[PyRep::Integer(2), PyRep::Integer(1), PyRep::Real(1_000_000.0)], &kw, None)
        .await
        .expect_err("jr accountant without take role on div 2 must be rejected");
    assert_eq!(err_div2.code, "CorpRoleDenied");

    ctx.dispatch_rpc(&mut sess_jr, "account", None, "GiveCashFromCorpAccount", &[PyRep::Integer(1), PyRep::Integer(2), PyRep::Real(2_000_000.0)], &kw, None)
        .await
        .expect("valid transfer from div 1 to div 2");

    ctx.db.with_snapshot(|snap| {
        let c = snap.corporations.get(&corp_a).unwrap();
        assert_eq!(c.division_balance(1), IskCents::from_isk(8_000_000));
        assert_eq!(c.master_wallet, IskCents::from_isk(8_000_000));
        assert_eq!(c.division_balance(2), IskCents::from_isk(7_000_000));
        let j_div1 = snap.corp_wallet_journals.get(&(corp_a, 1)).map_or(0, |v| v.len());
        let j_div2 = snap.corp_wallet_journals.get(&(corp_a, 2)).map_or(0, |v| v.len());
        assert_eq!(j_div1 + j_div2, 2, "must record debit and credit journal entries");
    });

    // #226: UpdateApplicationOffer rejects closed applications, non-accepted actions, and CEOs
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.get_mut(&corp_b).unwrap().applications.insert(
            7001,
            CorpApplicationRecord { application_id: 7001, character_id: member_no_shares, corporation_id: corp_b, message: "Closed".into(), status: "Rejected".into(), created_unix: 1000 },
        );
        snap.corporations.get_mut(&corp_a).unwrap().applications.insert(
            7002,
            CorpApplicationRecord { application_id: 7002, character_id: ceo_b, corporation_id: corp_a, message: "CEO".into(), status: "Invited".into(), created_unix: 1000 },
        );
    });

    let mut sess_mem = make_session(member_no_shares, corp_a, Some(station_id), sys_id, 0);
    let err_closed = ctx
        .dispatch_rpc(&mut sess_mem, "corpRegistry", None, "UpdateApplicationOffer", &[PyRep::Integer(7001), PyRep::String("accepted".into())], &kw, None)
        .await
        .expect_err("rejected app cannot be accepted");
    assert_eq!(err_closed.code, "ApplicationAlreadyClosed");

    let mut sess_ceo_b = make_session(ceo_b, corp_b, Some(station_id), sys_id, 0);
    let err_decline = ctx
        .dispatch_rpc(&mut sess_ceo_b, "corpRegistry", None, "UpdateApplicationOffer", &[PyRep::Integer(7002), PyRep::String("declined".into())], &kw, None)
        .await
        .expect_err("declined offer must not transfer corp");
    assert_eq!(err_decline.code, "InvalidApplicationAction");

    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.get_mut(&corp_a).unwrap().applications.get_mut(&7002).unwrap().status = "Invited".into();
    });
    let err_ceo_join = ctx
        .dispatch_rpc(&mut sess_ceo_b, "corpRegistry", None, "UpdateApplicationOffer", &[PyRep::Integer(7002), PyRep::String("accepted".into())], &kw, None)
        .await
        .expect_err("CEO cannot abandon corp via application offer");
    assert!(err_ceo_join.message.contains("CeoCannotLeaveCorp"));

    // #227: UpdateTitle strips ROLE_DIRECTOR (1) and title hangar grant is queued 24h; InsertVoteCase gated
    let mut sess_ceo_a = make_session(ceo_a, corp_a, Some(station_id), sys_id, 0);
    ctx.dispatch_rpc(&mut sess_ceo_a, "corpRegistry", None, "UpdateTitle", &[PyRep::Integer(1), PyRep::String("Vault Officer".into()), PyRep::Integer(1 | (1 << 13))], &kw, None)
        .await
        .expect("UpdateTitle");
    ctx.dispatch_rpc(&mut sess_ceo_a, "corpRegistry", None, "UpdateMember", &[PyRep::Integer(member_no_shares), PyRep::String("".into()), PyRep::Integer(0), PyRep::Integer(1)], &kw, None)
        .await
        .expect("UpdateMember title mask");

    ctx.db.with_snapshot(|snap| {
        let c = snap.corporations.get(&corp_a).unwrap();
        let effective = c.member_roles.get(&member_no_shares).copied().unwrap_or(0);
        assert_eq!(effective & 1, 0, "ROLE_DIRECTOR must be stripped from title");
        assert_eq!(effective & (1 << 13), 0, "Title-granted HangarCanTake1 must wait in 24h pending queue");
    });

    let err_vote_outsider = ctx
        .dispatch_rpc(&mut sess_ceo_b, "corpRegistry", None, "InsertVoteCase", &[PyRep::Integer(corp_a), PyRep::String("Hijack".into()), PyRep::String("".into()), PyRep::Integer(4)], &kw, None)
        .await
        .expect_err("outsider cannot insert vote case");
    assert_eq!(err_vote_outsider.code, "CorpRoleDenied");

    let err_vote_noshares = ctx
        .dispatch_rpc(&mut sess_mem, "corpRegistry", None, "InsertVoteCase", &[PyRep::Integer(corp_a), PyRep::String("NoShares".into()), PyRep::String("".into()), PyRep::Integer(0)], &kw, None)
        .await
        .expect_err("zero-share non-director cannot insert vote case");
    assert_eq!(err_vote_noshares.code, "CorpRoleDenied");

    // #231: corpRecProxy requires player corp + Personnel Manager / Director and scopes ad update to own corp
    let err_ad_role = ctx
        .dispatch_rpc(
            &mut sess_mem,
            "corpRecProxy",
            None,
            "CreateRecruitmentAd",
            &[PyRep::Integer(7), PyRep::Integer(1), PyRep::Integer(0), PyRep::String("Ad".into()), PyRep::Integer(0), PyRep::List(vec![]), PyRep::String("T".into()), PyRep::Integer(0)],
            &kw,
            None,
        )
        .await
        .expect_err("member without personnel manager role cannot create ad");
    assert_eq!(err_ad_role.code, "CorpRoleDenied");

    let ad_rep = ctx
        .dispatch_rpc(
            &mut sess_ceo_a,
            "corpRecProxy",
            None,
            "CreateRecruitmentAd",
            &[PyRep::Integer(7), PyRep::Integer(1), PyRep::Integer(0), PyRep::String("Corp A Ad".into()), PyRep::Integer(0), PyRep::List(vec![]), PyRep::String("Title A".into()), PyRep::Integer(0)],
            &kw,
            None,
        )
        .await
        .expect("CEO creates ad");
    let ad_id = ad_rep.dict_get("adID").and_then(|v| v.as_i64()).or_else(|| ad_rep.as_i64()).unwrap_or(1);

    let err_cross_ad = ctx
        .dispatch_rpc(
            &mut sess_ceo_b,
            "corpRecProxy",
            None,
            "UpdateRecruitmentAd",
            &[PyRep::Integer(ad_id), PyRep::Integer(1), PyRep::Integer(0), PyRep::String("Hijacked".into()), PyRep::Integer(0), PyRep::List(vec![]), PyRep::String("Hijacked".into()), PyRep::Integer(0)],
            &kw,
            None,
        )
        .await
        .expect_err("other corp CEO cannot update corp_a ad");
    assert_eq!(err_cross_ad.code, "RecruitmentAdNotFound");
}

#[tokio::test]
async fn r5_228_229_230_233_fleet_warp_conduit_hierarchy_trash_and_alsc_cache() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let station_id = 60_003_760_i64;
    let (sys_jita, sys_per) = (30_000_142_u32, 30_000_144_u32);
    let corp_id = 2_100_010_i64;
    let (boss_id, sc_id, target_id) = (96_002_001_i64, 96_002_002_i64, 96_002_003_i64);
    let (boss_ship, sc_ship, space_unpiloted_ship) = (890_002_001_i64, 890_002_002_i64, 890_002_003_i64);
    let (alsc_id, locked_trit) = (890_002_010_i64, 890_002_011_i64);
    let kw = HashMap::new();

    ctx.db.with_mut_snapshot(|snap| {
        let base = snap.characters.get(&90_000_001).cloned().expect("seeded");
        snap.characters.insert(boss_id, clone_seeded_char(&base, boss_id, corp_id, None, sys_jita, boss_ship));
        snap.characters.insert(sc_id, clone_seeded_char(&base, sc_id, corp_id, None, sys_jita, sc_ship));
        snap.characters.insert(target_id, clone_seeded_char(&base, target_id, corp_id, Some(station_id), sys_jita, 0));
        for &(iid, tid, owner, loc, flag) in &[
            (boss_ship, 587_u32, boss_id, sys_jita as i64, InventoryFlag::None),
            (sc_ship, 587_u32, sc_id, sys_jita as i64, InventoryFlag::None),
            (space_unpiloted_ship, 587_u32, boss_id, sys_jita as i64, InventoryFlag::None),
            (alsc_id, 17366_u32, boss_id, station_id, InventoryFlag::Hangar),
            (locked_trit, 34_u32, boss_id, alsc_id, InventoryFlag::Unlocked),
        ] {
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: tid,
                    owner_id: owner,
                    location_id: loc,
                    flag,
                    quantity: 1,
                    singleton: flag != InventoryFlag::Unlocked,
                    version: 1,
                    ..Default::default()
                },
            );
        }
        let mut fleet = FleetRecord { fleet_id: 5501, boss_id, members: vec![boss_id, sc_id], ..Default::default() };
        fleet.member_details.insert(
            boss_id,
            FleetMemberRecord {
                character_id: boss_id,
                role: FleetRole::FleetCommander,
                wing_id: None,
                squad_id: None,
                takes_fleet_warp: true,
                accepts_conduit_jumps: true,
                accepts_regroup: true,
                job: 2,
                role_name: "Fleet Commander".into(),
                join_time_unix: 1000,
            },
        );
        fleet.member_details.insert(
            sc_id,
            FleetMemberRecord {
                character_id: sc_id,
                role: FleetRole::SquadCommander,
                wing_id: Some(1),
                squad_id: Some(10),
                takes_fleet_warp: true,
                accepts_conduit_jumps: true,
                accepts_regroup: true,
                job: 0,
                role_name: "Squad Commander".into(),
                join_time_unix: 1000,
            },
        );
        snap.fleets.insert(5501, fleet);
        if let Some(ch) = snap.characters.get_mut(&boss_id) { ch.fleet_id = Some(5501); }
        if let Some(ch) = snap.characters.get_mut(&sc_id) { ch.fleet_id = Some(5501); }
    });

    let jita_handle = ctx.systems.get_or_spawn(sys_jita);
    for &(sid, x) in &[(boss_ship, 0.0), (sc_ship, 1_500.0), (space_unpiloted_ship, 500.0)] {
        let mut cs = ctx.build_type_combat_state(587);
        if sid == sc_ship { cs.warp_scramble_strength = 2; }
        let _ = jita_handle
            .spawn_ball_with_combat(
                Ball {
                    id: sid,
                    type_id: 587,
                    position: DVec3::new(x, 0.0, 0.0),
                    velocity: DVec3::ZERO,
                    max_velocity: 300.0,
                    inertia: 2.0,
                    mass: 1_000_000.0,
                    radius: 40.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                cs,
            )
            .await;
    }

    // #228: WarpToMember rejects scrambled ship; ConduitJump rejects invalid system & moves unscrambled ship
    let mut sess_sc = make_session(sc_id, corp_id, None, sys_jita, sc_ship);
    sess_sc.fleet_id = Some(5501);
    let err_scram = ctx
        .dispatch_rpc(&mut sess_sc, "fleetObjectHandler", None, "WarpToMember", &[PyRep::Integer(boss_id), PyRep::Real(0.0)], &kw, None)
        .await
        .expect_err("scrambled member cannot fleet warp");
    assert_eq!(err_scram.code, "WarpScrambled");

    let mut sess_boss = make_session(boss_id, corp_id, None, sys_jita, boss_ship);
    sess_boss.fleet_id = Some(5501);
    let err_bad_sys = ctx
        .dispatch_rpc(&mut sess_boss, "fleetMgr", None, "ConduitJump", &[PyRep::Integer(5501), PyRep::Integer(99_999_999)], &kw, None)
        .await
        .expect_err("invalid solar system must fail");
    assert_eq!(err_bad_sys.code, "InvalidConduitDestination");

    let cj_res = ctx
        .dispatch_rpc(&mut sess_boss, "fleetMgr", None, "ConduitJump", &[PyRep::Integer(5501), PyRep::Integer(sys_per as i64)], &kw, None)
        .await
        .expect("ConduitJump");
    let Some(PyRep::List(jumped)) = cj_res.dict_get("jumpedMemberIDs") else { panic!("expected jumpedMemberIDs list") };
    assert_eq!(jumped, &vec![PyRep::Integer(boss_id)], "scrambled sc_id must be excluded from conduit jump");
    assert_eq!(
        ctx.db.with_snapshot(|snap| snap.items.get(&boss_ship).unwrap().location_id),
        sys_per as i64,
        "boss ship item location must move to destination solar system"
    );

    // #230: Squad Commander cannot invite a Fleet Commander (role 1), and AcceptInvite requires pending invite
    let err_rank = ctx
        .dispatch_rpc(&mut sess_sc, "fleetObjectHandler", None, "Invite", &[PyRep::Integer(target_id), PyRep::Integer(0), PyRep::Integer(0), PyRep::Integer(1)], &kw, None)
        .await
        .expect_err("SC cannot invite to FC role");
    assert_eq!(err_rank.code, "FleetPermissionDenied");

    let mut sess_target = make_session(target_id, corp_id, Some(station_id), sys_jita, 0);
    let err_no_inv = ctx
        .dispatch_rpc(&mut sess_target, "fleetObjectHandler", None, "AcceptInvite", &[], &kw, None)
        .await
        .expect_err("AcceptInvite without invite must fail");
    assert_eq!(err_no_inv.code, "InviteExpired");

    // #229: TrashItems blocks trashing unpiloted ships in space
    let inv_oid = ctx.bind_object(&sess_sc, "invbroker", sys_jita as i64).expect("bind invbroker");
    let err_trash_ship = ctx
        .dispatch_rpc(&mut sess_sc, "invbroker", Some(inv_oid), "TrashItems", &[PyRep::List(vec![PyRep::Integer(space_unpiloted_ship)])], &kw, None)
        .await
        .expect_err("cannot trash unpiloted ship in space");
    assert_eq!(err_trash_ship.code, "DeleteFailed");

    // #233: SetPassword invalidates unlocked_containers cache; ALSCLockItems requires co-location
    let err_remote_lock = ctx
        .dispatch_rpc(&mut sess_boss, "invbroker", None, "ALSCLockItems", &[PyRep::Integer(alsc_id), PyRep::List(vec![PyRep::Integer(locked_trit)])], &kw, None)
        .await
        .expect_err("undocked boss in another system cannot lock station ALSC items");
    assert_eq!(err_remote_lock.code, "ColocationRequired");

    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&boss_id).unwrap();
        ch.station_id = Some(station_id);
        ch.solar_system_id = sys_jita;
    });
    sess_boss.station_id = Some(station_id);
    sess_boss.solar_system_id = Some(sys_jita);
    ctx.dispatch_rpc(&mut sess_boss, "invbroker", None, "SetPassword", &[PyRep::Integer(alsc_id), PyRep::String("oldpass".into())], &kw, None)
        .await
        .expect("SetPassword oldpass");
    ctx.dispatch_rpc(
        &mut sess_target,
        "invbroker",
        None,
        "ALSCLockItems",
        &[PyRep::Integer(alsc_id), PyRep::List(vec![]), PyRep::String("oldpass".into())],
        &kw,
        None,
    )
    .await
    .ok();
    // Unlock container for target_id by taking with password="oldpass" or via SetPassword by target, then boss changes password
    let mut kw_pw = HashMap::new();
    kw_pw.insert("password".to_string(), PyRep::String("oldpass".into()));
    let st_inv_t = ctx.bind_object(&sess_target, "invbroker", station_id).expect("bind station inv");
    let _ = ctx.dispatch_rpc(&mut sess_target, "invbroker", Some(st_inv_t), "Add", &[PyRep::Integer(locked_trit), PyRep::Integer(station_id)], &kw_pw, None).await;
    // Now boss changes password -> must invalidate target_id's unlocked_containers entry
    ctx.dispatch_rpc(&mut sess_boss, "invbroker", None, "SetPassword", &[PyRep::Integer(alsc_id), PyRep::String("newpass".into())], &kw, None)
        .await
        .expect("SetPassword newpass");
    let err_need_pw = ctx
        .dispatch_rpc(&mut sess_target, "invbroker", None, "ALSCLogGet", &[PyRep::Integer(alsc_id)], &kw, None)
        .await
        .expect_err("SetPassword must invalidate cached unlocked_containers session");
    assert_eq!(err_need_pw.code, "ALSCPasswordNeededToViewAuditLog");
}

#[tokio::test]
async fn r5_232_234_235_236_237_projects_skills_fitting_clones_and_agents() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let station_id = 60_003_760_i64;
    let sys_id = 30_000_142_u32;
    let corp_id = 2_100_020_i64;
    let pilot_id = 96_003_001_i64;
    let (ship_id, cap_id, mod_locked, extractor_id) = (890_003_001_i64, 890_003_002_i64, 890_003_003_i64, 890_003_004_i64);
    let kw = HashMap::new();

    ctx.db.with_mut_snapshot(|snap| {
        let base = snap.characters.get(&90_000_001).cloned().expect("seeded");
        let mut ch = clone_seeded_char(&base, pilot_id, corp_id, Some(station_id), sys_id, ship_id);
        ch.free_skill_points = 50_000.0;
        snap.characters.insert(pilot_id, ch);

        let mut corp = CorporationRecord {
            corporation_id: corp_id,
            name: "R5 Proj Corp".into(),
            ticker: "R5PC".into(),
            ceo_id: 90_000_001,
            master_wallet: IskCents::from_isk(20_000_000),
            members: HashSet::from([pilot_id]),
            member_roles: HashMap::from([(pilot_id, (1u64 << 61) | (1u64 << 20))]),
            ..Default::default()
        };
        corp.ensure_wallet_divisions();
        corp.set_division_balance(1, IskCents::from_isk(20_000_000));
        corp.set_division_balance(2, IskCents::from_isk(20_000_000));
        snap.corporations.insert(corp_id, corp);

        for &(iid, tid, flag) in &[
            (ship_id, 587_u32, InventoryFlag::Hangar),
            (cap_id, 670_u32, InventoryFlag::Hangar),
            (mod_locked, 440_u32, InventoryFlag::Locked),
            (extractor_id, 40519_u32, InventoryFlag::Hangar),
        ] {
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: tid,
                    owner_id: pilot_id,
                    location_id: station_id,
                    flag,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
        }
        let sk = snap.skills.entry(pilot_id).or_default();
        sk.insert(24242, TrainedSkillRecord { type_id: 24242, level: 3, skill_points: 40_000.0 });
        sk.insert(33399, TrainedSkillRecord { type_id: 33399, level: 3, skill_points: 40_000.0 });
        sk.insert(3300, TrainedSkillRecord { type_id: 3300, level: 1, skill_points: 250.0 });
        let now = ctx.now_unix();
        snap.skill_queues.insert(
            pilot_id,
            vec![
                SkillQueueEntry { type_id: 3300, target_level: 2, start_sp: 250.0, end_sp: 1415.0, duration_seconds: 600.0, started_at_unix: Some(now) },
                SkillQueueEntry { type_id: 24242, target_level: 4, start_sp: 40_000.0, end_sp: 90_510.0, duration_seconds: 1800.0, started_at_unix: None },
            ],
        );
    });

    let mut sess = make_session(pilot_id, corp_id, Some(station_id), sys_id, ship_id);

    // #232: CreateProject enforces wallet division take role for non-Accountant Project Manager
    let err_proj_div = ctx
        .dispatch_rpc(
            &mut sess,
            "corporationSvc",
            None,
            "CreateProject",
            &[PyRep::String("Div2 Exploit".into()), PyRep::Integer(10), PyRep::Real(100_000.0), PyRep::String("DeliverItem".into()), PyRep::Bool(false), PyRep::Integer(1001)],
            &kw,
            None,
        )
        .await
        .expect_err("Project Manager without AccountCanTake2 cannot escrow from div 2");
    assert_eq!(err_proj_div.code, "CorpRoleDenied");

    // #234: ApplyFreeSkillPoints completing head starts next queue entry
    ctx.dispatch_rpc(&mut sess, "skillMgr2", None, "ApplyFreeSkillPoints", &[PyRep::Integer(3300), PyRep::Integer(2_000)], &kw, None)
        .await
        .expect("ApplyFreeSkillPoints");
    ctx.db.with_snapshot(|snap| {
        let q = snap.skill_queues.get(&pilot_id).unwrap();
        assert_eq!(q.first().map(|e| e.type_id), Some(24242));
        assert!(q.first().and_then(|e| e.started_at_unix).is_some(), "new queue head must have started_at_unix set");
    });

    // #235: FitShips skips ALSC-locked modules (InventoryFlag::Locked)
    let _ = ctx
        .dispatch_rpc(
            &mut sess,
            "ship",
            None,
            "FitShips",
            &[
                PyRep::List(vec![PyRep::Integer(ship_id)]),
                PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(440), PyRep::Integer(InventoryFlag::MedSlot0 as i64), PyRep::Integer(1)])]),
            ],
            &kw,
            None,
        )
        .await;
    assert_eq!(ctx.db.with_snapshot(|snap| snap.items.get(&mod_locked).unwrap().flag), InventoryFlag::Locked);

    // #236: InstallCloneInStation journals JumpCloneInstallationFee, and NPC same-station CloneJump charges 900k fee
    ctx.dispatch_rpc(&mut sess, "jumpCloneSvc", None, "InstallCloneInStation", &[], &kw, None)
        .await
        .expect("InstallCloneInStation");
    let jc_id = ctx.db.with_snapshot(|snap| {
        assert!(snap.wallet_journal.iter().any(|j| j.character_id == pilot_id && j.entry_type == "JumpCloneInstallationFee"));
        snap.jump_clones.get(&pilot_id).unwrap()[0].jump_clone_id
    });

    ctx.db.with_mut_snapshot(|snap| { snap.characters.get_mut(&pilot_id).unwrap().active_ship_id = cap_id; });
    sess.ship_id = Some(cap_id);
    let bal_before_swap = ctx.db.with_snapshot(|snap| snap.characters.get(&pilot_id).unwrap().balance);
    ctx.dispatch_rpc(&mut sess, "jumpCloneSvc", None, "CloneJump", &[PyRep::Integer(jc_id)], &kw, None)
        .await
        .expect("NPC same-station CloneJump");
    let bal_after_swap = ctx.db.with_snapshot(|snap| snap.characters.get(&pilot_id).unwrap().balance);
    assert_eq!(bal_before_swap.0 - bal_after_swap.0, 90_000_000);

    // #237: StartResearch rejects non-science skill & non-colocated caller; BuyDatacores journals DatacoreFee
    let rd_agent_id = ctx.sde.agents.values().find(|a| a.level == 1 && a.agent_type_id == 4 && a.station_id.is_some()).map(|a| a.agent_id).expect("R&D agent");
    let err_not_coloc = ctx
        .dispatch_rpc(&mut sess, "agentMgr", None, "DoAction", &[PyRep::Integer(12), PyRep::Integer(rd_agent_id), PyRep::Integer(11448)], &kw, None)
        .await
        .expect_err("non-colocated R&D start must fail");
    assert_eq!(err_not_coloc.code, "AgentNotCoLocated");

    let (rd_sys, rd_sta, rd_corp) = ctx.sde.agents.get(&rd_agent_id).map(|a| (a.solar_system_id.unwrap(), a.station_id.unwrap(), a.corporation_id)).unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&pilot_id).unwrap();
        ch.solar_system_id = rd_sys;
        ch.station_id = Some(rd_sta);
        ch.standings.insert(rd_corp, 5.0);
        snap.skills.entry(pilot_id).or_default().insert(11448, TrainedSkillRecord { type_id: 11448, level: 4, skill_points: 45_255.0 });
    });
    sess.solar_system_id = Some(rd_sys);
    sess.station_id = Some(rd_sta);

    let err_bad_skill = ctx
        .dispatch_rpc(&mut sess, "agentMgr", None, "DoAction", &[PyRep::Integer(12), PyRep::Integer(rd_agent_id), PyRep::Integer(3300)], &kw, None)
        .await
        .expect_err("non-science skill must be rejected for R&D");
    assert_eq!(err_bad_skill.code, "InvalidResearchSkill");

    ctx.dispatch_rpc(&mut sess, "agentMgr", None, "DoAction", &[PyRep::Integer(12), PyRep::Integer(rd_agent_id), PyRep::Integer(11448)], &kw, None)
        .await
        .expect("StartResearch");
    ctx.advance_secs(5 * 86_400);
    ctx.dispatch_rpc(&mut sess, "agentMgr", None, "DoAction", &[PyRep::Integer(14), PyRep::Integer(rd_agent_id), PyRep::Integer(1)], &kw, None)
        .await
        .expect("BuyDatacores");
    assert!(ctx.db.with_snapshot(|snap| snap.wallet_journal.iter().any(|j| j.character_id == pilot_id && j.entry_type == "DatacoreFee")));
}
