//! Direct unit tests covering all branches of `fleet_social_chat_auth_ops`.

use super::super::fleet_social_chat_auth_ops::*;
use super::super::*;
use crate::services::fleet_social::fleet_social_fleet_ops::fleet_social_fleet_ops_tests::{
    clone_char_with, test_ctx_with_clock,
};
use eve_db::{
    corp_roles, AllianceRecord, ChatChannelAclRecord, ChatChannelRecord, CorporationRecord,
    FleetMemberRecord, FleetRecord, FleetRole, IskCents, SocialContactRecord,
};
use eve_machonet::{service_roles, SessionState};
use eve_proto::PyRep;
use std::collections::{HashMap, HashSet};

#[test]
fn channel_classification_initial_record_and_org_operator_auth() {
    assert!(is_system_channel("incursion:123"));
    assert!(is_system_channel("global"));
    assert!(is_system_channel("local"));
    assert!(is_system_channel("help_en"));
    assert!(is_system_channel("rookie_help"));
    assert!(!is_system_channel("channel:custom"));
    assert!(is_org_channel("corpid:98000001"));
    assert!(is_org_channel("allianceid:99000001"));
    assert!(is_org_channel("fleetid:700001"));
    assert!(!is_org_channel("global"));
    assert!(is_system_or_org_channel("global"));
    assert!(is_system_or_org_channel("fleetid:1"));

    let sys_rec = build_initial_channel_record("solarsystemid2:30000142", 90_000_001, "ignored", Some("pw"), true, Some(30_000_142));
    assert_eq!(sys_rec.owner_id, None);
    assert!(sys_rec.is_system_local);
    assert!(sys_rec.is_delayed_local);
    assert!(sys_rec.password.is_none());

    let priv_rec = build_initial_channel_record("private:1:2", 90_000_001, "hi", Some(""), false, None);
    assert_eq!(priv_rec.owner_id, Some(90_000_001));
    assert!(priv_rec.is_private);
    assert!(priv_rec.password.is_none());
    assert!(priv_rec.operators.contains(&90_000_001));

    let ctx = test_ctx_with_clock(1_800_020_000);
    let ceo_id = 90_000_001i64;
    let dir_id = 90_000_002i64;
    let chat_mgr_id = 90_000_003i64;
    let grunt_id = 90_000_004i64;
    let corp_id = 98_100_001i64;
    let non_exec_corp = 98_100_002i64;
    let ally_id = 99_100_001i64;
    let fleet_id = 700_100i64;

    ctx.db.with_mut_snapshot(|snap| {
        for (cid, name, corp) in [
            (ceo_id, "CEO", corp_id),
            (dir_id, "Dir", corp_id),
            (chat_mgr_id, "ChatMgr", corp_id),
            (grunt_id, "Grunt", non_exec_corp),
        ] {
            let ch = clone_char_with(snap, cid, name, corp, 30_000_142, 100_000_001);
            snap.characters.insert(cid, ch);
        }
        let mut corp = { let mut __corp = CorporationRecord::default(); __corp.corporation_id = corp_id; __corp.name = "ExecCorp".into(); __corp.ticker = "EXEC".into(); __corp.ceo_id = ceo_id; __corp.ensure_wallet_divisions(); __corp };
        corp.member_roles.insert(dir_id, corp_roles::ROLE_DIRECTOR);
        snap.corporations.insert(corp_id, corp);
        snap.corporations.insert(
            non_exec_corp,
            { let mut __corp = CorporationRecord::default(); __corp.corporation_id = non_exec_corp; __corp.name = "OtherCorp".into(); __corp.ticker = "OTHR".into(); __corp.ceo_id = grunt_id; __corp.ensure_wallet_divisions(); __corp },
        );
        snap.alliances.insert(
            ally_id,
            AllianceRecord {
                alliance_id: ally_id,
                name: "TestAlly".into(),
                short_name: "TALY".into(),
                executor_corp_id: corp_id,
                member_corp_ids: HashSet::from([corp_id, non_exec_corp]),
                ..Default::default()
            },
        );
        let mk_member = |character_id: i64, role: FleetRole| FleetMemberRecord {
            character_id,
            role,
            wing_id: None,
            squad_id: None,
            takes_fleet_warp: true,
            accepts_regroup: true,
            accepts_conduit_jumps: true,
            job: 0,
            role_name: String::new(),
            join_time_unix: 0,
        };
        let mut fleet = FleetRecord {
            fleet_id,
            boss_id: ceo_id,
            members: vec![ceo_id, dir_id, chat_mgr_id, grunt_id],
            ..Default::default()
        };
        fleet.member_details.insert(dir_id, mk_member(dir_id, FleetRole::WingCommander));
        fleet.member_details.insert(chat_mgr_id, mk_member(chat_mgr_id, FleetRole::FleetCommander));
        fleet.member_details.insert(grunt_id, mk_member(grunt_id, FleetRole::SquadMember));
        snap.fleets.insert(fleet_id, fleet);
    });

    ctx.db.with_snapshot(|snap| {
        let mut ceo_sess = SessionState::new(1);
        ceo_sess.corporation_id = Some(corp_id);
        let dir_sess = SessionState::new(2);
        let mut mgr_sess = SessionState::new(3);
        mgr_sess.corporation_id = Some(corp_id);
        mgr_sess.corp_role = CORP_ROLE_CHAT_MANAGER;
        let grunt_sess = SessionState::new(4);

        // NPC corp always denied
        assert!(!has_dynamic_org_operator_role(snap, &ceo_sess, ceo_id, "corpid:1000009"));
        // Invalid/missing org ids
        assert!(!has_dynamic_org_operator_role(snap, &ceo_sess, ceo_id, "corpid:notnum"));
        assert!(!has_dynamic_org_operator_role(snap, &ceo_sess, ceo_id, "corpid:98999999"));
        assert!(!has_dynamic_org_operator_role(snap, &ceo_sess, ceo_id, "allianceid:99999999"));
        assert!(!has_dynamic_org_operator_role(snap, &ceo_sess, ceo_id, "fleetid:99999999"));
        assert!(!has_dynamic_org_operator_role(snap, &ceo_sess, ceo_id, "channel:other"));

        // Corp & Alliance operator rights
        assert!(has_dynamic_org_operator_role(snap, &ceo_sess, ceo_id, &format!("corpid:{corp_id}")));
        assert!(has_dynamic_org_operator_role(snap, &dir_sess, dir_id, &format!("corpid:{corp_id}")));
        assert!(has_dynamic_org_operator_role(snap, &mgr_sess, chat_mgr_id, &format!("corpid:{corp_id}")));
        assert!(!has_dynamic_org_operator_role(snap, &grunt_sess, grunt_id, &format!("corpid:{corp_id}")));
        assert!(has_dynamic_org_operator_role(snap, &ceo_sess, ceo_id, &format!("allianceid:{ally_id}")));
        assert!(!has_dynamic_org_operator_role(snap, &grunt_sess, grunt_id, &format!("allianceid:{ally_id}")));

        // Fleet boss / FC / WC vs SquadMember
        assert!(has_dynamic_org_operator_role(snap, &ceo_sess, ceo_id, &format!("fleetid:{fleet_id}")));
        assert!(has_dynamic_org_operator_role(snap, &dir_sess, dir_id, &format!("fleetid:{fleet_id}")));
        assert!(has_dynamic_org_operator_role(snap, &mgr_sess, chat_mgr_id, &format!("fleetid:{fleet_id}")));
        assert!(!has_dynamic_org_operator_role(snap, &grunt_sess, grunt_id, &format!("fleetid:{fleet_id}")));

        // has_channel_operator_auth & has_channel_owner_auth
        let mut gm_sess = SessionState::new(5);
        gm_sess.role = service_roles::ROLE_GMH;
        assert!(has_channel_operator_auth(snap, &gm_sess, 999, "global", None));
        assert!(!has_channel_operator_auth(snap, &ceo_sess, ceo_id, "global", None));
        assert!(has_channel_operator_auth(snap, &ceo_sess, ceo_id, &format!("corpid:{corp_id}"), None));

        let owned_chan = ChatChannelRecord {
            channel_id: "channel:test".into(),
            name: "test".into(),
            motd: String::new(),
            owner_id: Some(ceo_id),
            members: HashSet::new(),
            is_system_local: false,
            solar_system_id: None,
            is_private: false,
            operators: HashSet::from([dir_id]),
            password: None,
            is_delayed_local: false,
        };
        assert!(has_channel_operator_auth(snap, &dir_sess, dir_id, "channel:test", Some(&owned_chan)));
        assert!(!has_channel_operator_auth(snap, &grunt_sess, grunt_id, "channel:test", Some(&owned_chan)));
        assert!(!has_channel_operator_auth(snap, &ceo_sess, ceo_id, "channel:test", None));

        assert!(!has_channel_owner_auth(&gm_sess, ceo_id, "global", &owned_chan));
        assert!(!has_channel_owner_auth(&gm_sess, ceo_id, "corpid:98100001", &owned_chan));
        assert!(has_channel_owner_auth(&gm_sess, grunt_id, "channel:test", &owned_chan));
        assert!(has_channel_owner_auth(&ceo_sess, ceo_id, "channel:test", &owned_chan));
        assert!(!has_channel_owner_auth(&dir_sess, dir_id, "channel:test", &owned_chan));
    });
}

#[test]
fn validate_join_auth_mutes_and_char_mgr_contacts_cspa_notes() {
    let ctx = test_ctx_with_clock(1_800_021_000);
    let kw = HashMap::new();
    let owner_id = 90_000_001i64;
    let guest_id = 90_000_002i64;
    let corp_mate_id = 90_000_003i64;
    let ally_mate_id = 90_000_004i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut c1 = clone_char_with(snap, owner_id, "Owner", 98_000_001, 30_000_142, 100_000_001);
        c1.alliance_id = Some(99_000_001);
        c1.balance = IskCents::from_isk(20_000);
        let c2 = clone_char_with(snap, guest_id, "Guest", 98_000_002, 30_000_144, 100_000_002);
        let c3 = clone_char_with(snap, corp_mate_id, "CorpMate", 98_000_001, 30_000_142, 100_000_003);
        let mut c4 = clone_char_with(snap, ally_mate_id, "AllyMate", 98_000_009, 30_000_142, 100_000_004);
        c4.alliance_id = Some(99_000_001);
        snap.characters.insert(owner_id, c1);
        snap.characters.insert(guest_id, c2);
        snap.characters.insert(corp_mate_id, c3);
        snap.characters.insert(ally_mate_id, c4);

        snap.chat_channels.insert(
            "private:1:2".into(),
            ChatChannelRecord {
                channel_id: "private:1:2".into(),
                name: "priv".into(),
                motd: String::new(),
                owner_id: Some(owner_id),
                members: HashSet::from([owner_id]),
                is_system_local: false,
                solar_system_id: None,
                is_private: true,
                operators: HashSet::new(),
                password: None,
                is_delayed_local: false,
            },
        );
    });

    let mut owner_sess = SessionState::new(10);
    owner_sess.user_id = Some(owner_id as u64);
    owner_sess.character_id = Some(owner_id);
    owner_sess.solar_system_id = Some(30_000_142);

    // 1. validate_channel_join_auth: unparseable suffixes, target_char != caller_id, private invite, GM bypass
    ctx.db.with_snapshot(|snap| {
        let mut gm = owner_sess.clone();
        gm.role = service_roles::ROLE_GMH;
        assert!(validate_channel_join_auth(&ctx, snap, &gm, owner_id, owner_id, "private:1:2").is_ok());

        for bad in [
            "solarsystemid2:bad",
            "resourcewars:bad",
            "constellationid:bad",
            "regionid:bad",
            "corpid:bad",
            "allianceid:bad",
            "fleetid:bad",
            "factionid:bad",
        ] {
            assert_eq!(
                validate_channel_join_auth(&ctx, snap, &owner_sess, owner_id, owner_id, bad)
                    .unwrap_err()
                    .code,
                "ChannelAccessDenied"
            );
        }
        // target_char != caller_id checks target's snapshot solar system
        assert!(validate_channel_join_auth(&ctx, snap, &owner_sess, owner_id, guest_id, "solarsystemid2:30000144").is_ok());
        assert!(validate_channel_join_auth(&ctx, snap, &owner_sess, owner_id, guest_id, "solarsystemid2:30000142").is_err());

        // Private channel: non-invited guest denied, owner inviting guest succeeds
        assert!(validate_channel_join_auth(&ctx, snap, &owner_sess, guest_id, guest_id, "private:1:2").is_err());
        assert!(validate_channel_join_auth(&ctx, snap, &owner_sess, owner_id, guest_id, "private:1:2").is_ok());
    });

    // 2. check_channel_or_global_mute: DB channel mute, DB global mute, social_extras channel & global, expiry
    ctx.db.with_mut_snapshot(|snap| {
        snap.chat_channel_acl.insert(
            "channel:m".into(),
            ChatChannelAclRecord {
                muted_until: HashMap::from([(guest_id, (1_800_021_100, "db_chan".into()))]),
                ..Default::default()
            },
        );
        snap.chat_channel_acl.insert(
            "global".into(),
            ChatChannelAclRecord {
                muted_until: HashMap::from([(corp_mate_id, (1_800_021_200, "db_global".into()))]),
                ..Default::default()
            },
        );
    });
    assert_eq!(
        check_channel_or_global_mute(&ctx, "channel:m", guest_id, 1_800_021_050).as_deref(),
        Some("db_chan")
    );
    assert!(check_channel_or_global_mute(&ctx, "channel:m", guest_id, 1_800_021_100).is_none());
    assert_eq!(
        check_channel_or_global_mute(&ctx, "channel:any", corp_mate_id, 1_800_021_050).as_deref(),
        Some("db_global")
    );

    {
        let mut s = ctx.social_extras.lock().unwrap();
        s.muted_in_channel.insert(("channel:s".into(), ally_mate_id), (1_800_021_300, "mem_chan".into()));
        s.muted_in_channel.insert(("global".into(), owner_id), (1_800_021_400, "mem_global".into()));
    }
    assert_eq!(
        check_channel_or_global_mute(&ctx, "channel:s", ally_mate_id, 1_800_021_050).as_deref(),
        Some("mem_chan")
    );
    assert_eq!(
        check_channel_or_global_mute(&ctx, "channel:any", owner_id, 1_800_021_050).as_deref(),
        Some("mem_global")
    );
    assert!(check_channel_or_global_mute(&ctx, "channel:any", owner_id, 1_800_021_400).is_none());

    // 3. handle_contacts_notifications_calendar: LogSettings clamping, CSPA friendly paths, GetNote fallback, unauth
    let mut unauth = SessionState::new(99);
    for m in ["GetSettingsInfo", "LogSettings", "CSPAChargedAction", "GetNote"] {
        assert!(handle_contacts_notifications_calendar(&ctx, &mut unauth, "charMgr", m, &[], &kw)
            .unwrap()
            .is_err());
    }

    let clamped_hi = handle_contacts_notifications_calendar(
        &ctx,
        &mut owner_sess,
        "charMgr",
        "LogSettings",
        &[PyRep::Real(5_000_000.0)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(clamped_hi, PyRep::Real(1_000_000.0));
    let default_log = handle_contacts_notifications_calendar(
        &ctx,
        &mut owner_sess,
        "charMgr",
        "LogSettings",
        &[],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(default_log, PyRep::Real(2_950.0));

    // Self, corp mate, alliance mate, and DB positive contact standing all cost 0.0 ISK CSPA
    ctx.db.with_mut_snapshot(|snap| {
        snap.char_contacts.insert(
            guest_id,
            vec![SocialContactRecord {
                contact_id: owner_id,
                contact_type: "character".into(),
                standing: 5.0,
                is_watched: false,
                is_blocked: false,
                label_ids: Vec::new(),
            }],
        );
    });
    let zero_cspa = handle_contacts_notifications_calendar(
        &ctx,
        &mut owner_sess,
        "charMgr",
        "CSPAChargedActionForMany",
        &[PyRep::List(vec![
            PyRep::Integer(owner_id),
            PyRep::Integer(0),
            PyRep::Integer(corp_mate_id),
            PyRep::Integer(ally_mate_id),
            PyRep::Integer(guest_id),
        ])],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(zero_cspa, PyRep::Real(0.0));

    // Stranger with high CSPA (> owner balance 20_000) fails InsufficientFundsForCSPA
    let stranger_id = 90_000_999i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.character_cspa_charges.insert(stranger_id, IskCents::from_isk(50_000));
    });
    assert_eq!(
        handle_contacts_notifications_calendar(
            &ctx,
            &mut owner_sess,
            "charMgr",
            "CSPAChargedAction",
            &[PyRep::Integer(stranger_id)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InsufficientFundsForCSPA"
    );

    // GetNote empty when missing, falls back to social_extras.char_notes
    assert_eq!(
        handle_contacts_notifications_calendar(
            &ctx,
            &mut owner_sess,
            "charMgr",
            "GetNote",
            &[PyRep::Integer(guest_id)],
            &kw,
        )
        .unwrap()
        .unwrap(),
        PyRep::String(String::new())
    );
    ctx.social_extras
        .lock()
        .unwrap()
        .char_notes
        .insert((owner_id, guest_id), "fallback_note".into());
    assert_eq!(
        handle_contacts_notifications_calendar(
            &ctx,
            &mut owner_sess,
            "charMgr",
            "GetNote",
            &[PyRep::Integer(guest_id)],
            &kw,
        )
        .unwrap()
        .unwrap(),
        PyRep::String("fallback_note".into())
    );

    // Contact delegation arms
    for m in ["EditContact", "EditContactsRelationshipID", "DeleteContacts", "BlockOwners", "UnblockOwners"] {
        assert!(handle_contacts_notifications_calendar(&ctx, &mut owner_sess, "charMgr", m, &[], &kw).is_some());
    }
}

#[test]
fn mutant_kill_corp_chat_role_xor_and_char_mgr_unknown_setnote_cspa() {
    let ctx = test_ctx_with_clock(1_800_022_000);
    let kw = HashMap::new();
    let corp_id = 98_100_099_i64;
    let other_corp = 98_100_098_i64;
    let ceo_id = 90_000_101_i64;
    let both_dir_id = 90_000_102_i64;
    let sess_mgr_id = 90_000_103_i64;
    let grunt_id = 90_000_104_i64;
    let stranger_id = 90_000_105_i64;

    ctx.db.with_mut_snapshot(|snap| {
        for (cid, name, corp, bal) in [
            (ceo_id, "Ceo", corp_id, 50_000),
            (both_dir_id, "BothDir", corp_id, 50_000),
            (sess_mgr_id, "SessMgr", corp_id, 50_000),
            (grunt_id, "Grunt", corp_id, 50_000),
            (stranger_id, "Stranger", other_corp, 50_000),
        ] {
            let mut ch = clone_char_with(snap, cid, name, corp, 30_000_142, 100_000_001);
            ch.balance = IskCents::from_isk(bal);
            snap.characters.insert(cid, ch);
        }
        let mut corp = CorporationRecord::default();
        corp.corporation_id = corp_id;
        corp.name = "ChatAuthCorp".into();
        corp.ticker = "CAC".into();
        corp.ceo_id = ceo_id;
        corp.members = HashSet::from([ceo_id, both_dir_id, sess_mgr_id, grunt_id]);
        corp.member_roles.insert(both_dir_id, corp_roles::ROLE_DIRECTOR);
        corp.ensure_wallet_divisions();
        snap.corporations.insert(corp_id, corp);

        let mut corp2 = CorporationRecord::default();
        corp2.corporation_id = other_corp;
        corp2.name = "OtherChatCorp".into();
        corp2.ticker = "OCC".into();
        corp2.ceo_id = stranger_id;
        corp2.members = HashSet::from([stranger_id]);
        corp2.ensure_wallet_divisions();
        snap.corporations.insert(other_corp, corp2);

        snap.character_cspa_charges.insert(stranger_id, IskCents::from_isk(500));
    });

    let chan = format!("corpid:{corp_id}");
    ctx.db.with_snapshot(|snap| {
        // Kills `eff_roles | session_roles` -> `eff_roles ^ session_roles` (same bit in both!)
        let mut both_sess = SessionState::new(1);
        both_sess.corporation_id = Some(corp_id);
        both_sess.corp_role = corp_roles::ROLE_DIRECTOR;
        assert!(has_dynamic_org_operator_role(snap, &both_sess, both_dir_id, &chan));

        // Kills `eff_roles | session_roles` -> `&` when only session OR only DB holds role
        let mut db_only_sess = SessionState::new(2);
        db_only_sess.corporation_id = Some(corp_id);
        db_only_sess.corp_role = 0;
        assert!(has_dynamic_org_operator_role(snap, &db_only_sess, both_dir_id, &chan));

        let mut sess_only = SessionState::new(3);
        sess_only.corporation_id = Some(corp_id);
        sess_only.corp_role = CORP_ROLE_CHAT_MANAGER;
        assert!(has_dynamic_org_operator_role(snap, &sess_only, sess_mgr_id, &chan));

        // Kills `session.corporation_id == Some(corp_id)` -> `!= Some(corp_id)` and `-> true`
        let mut foreign_dir_sess = SessionState::new(4);
        foreign_dir_sess.corporation_id = Some(other_corp);
        foreign_dir_sess.corp_role = CORP_ROLE_CHAT_MANAGER;
        assert!(!has_dynamic_org_operator_role(snap, &foreign_dir_sess, stranger_id, &chan));

        let mut grunt_sess = SessionState::new(5);
        grunt_sess.corporation_id = Some(corp_id);
        grunt_sess.corp_role = 0;
        assert!(!has_dynamic_org_operator_role(snap, &grunt_sess, grunt_id, &chan));
    });

    // handle_contacts_notifications_calendar: unknown method -> None, SetNote + GetNote DB persistence, positive CSPA deduction
    let mut caller_sess = SessionState::new(6);
    caller_sess.user_id = Some(ceo_id as u64);
    caller_sess.character_id = Some(ceo_id);
    caller_sess.corporation_id = Some(corp_id);

    assert!(handle_contacts_notifications_calendar(&ctx, &mut caller_sess, "charMgr", "unmatched_method", &[], &kw).is_none());

    assert_eq!(
        handle_contacts_notifications_calendar(
            &ctx,
            &mut caller_sess,
            "charMgr",
            "SetNote",
            &[PyRep::Integer(stranger_id), PyRep::String("persisted_note".into())],
            &kw,
        )
        .unwrap()
        .unwrap(),
        PyRep::Bool(true)
    );
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.char_notes.get(&(ceo_id, stranger_id)).map(String::as_str), Some("persisted_note"));
    });
    assert_eq!(
        handle_contacts_notifications_calendar(
            &ctx,
            &mut caller_sess,
            "charMgr",
            "GetNote",
            &[PyRep::Integer(stranger_id)],
            &kw,
        )
        .unwrap()
        .unwrap(),
        PyRep::String("persisted_note".into())
    );

    let paid = handle_contacts_notifications_calendar(
        &ctx,
        &mut caller_sess,
        "charMgr",
        "CSPAChargedAction",
        &[PyRep::Integer(stranger_id)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(paid, PyRep::Real(500.0));
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.characters[&ceo_id].balance, IskCents::from_isk(49_500));
    });
}
