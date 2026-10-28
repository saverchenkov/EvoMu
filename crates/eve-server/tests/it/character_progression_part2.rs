//! GM slash commands and neural attribute remap integration tests (`character_progression` part 2).

#![forbid(unsafe_code)]

use crate::character_progression::issue_token_with_scopes;
use crate::common;

use eve_db::InventoryFlag;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::esi::{EsiHttpRequest, EsiRouter};
use eve_server::router::ServerContext;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;

#[tokio::test]
async fn test_ch4_slash_gm_commands() {
    // parity: rpc:charUnboundMgr.SelectCharacterID, rpc:slash.SlashCmd
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut sess = SessionState::new(922);

    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;

    let char_id = 90_000_001;
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");

    // 1. Regular pilot without GM role rejected
    let unpriv = ctx
        .dispatch_rpc(
            &mut sess,
            "slash",
            None,
            "SlashCmd",
            &[PyRep::String("/giveitem 34 100".into())],
            &kw,
            None,
        )
        .await;
    assert_eq!(unpriv.unwrap_err().code, "InsufficientRole");

    // Elevate session to GM role
    sess.role = eve_machonet::service_roles::ROLE_GMH;
    assert!(sess.has_gm_role());

    // 2. /giveitem <type_id> <quantity>
    let giveitem_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "slash",
            None,
            "SlashCmd",
            &[PyRep::String("/giveitem 34 500".into())],
            &kw,
            None,
        )
        .await
        .expect("/giveitem 34 500");
    let PyRep::KeyVal(gi_kv) = giveitem_rep else {
        panic!("Expected KeyVal from /giveitem");
    };
    assert_eq!(gi_kv.get("typeID"), Some(&PyRep::Integer(34)));
    assert_eq!(gi_kv.get("quantity"), Some(&PyRep::Integer(500)));

    ctx.db.with_snapshot(|snap| {
        let has_tritanium = snap.items.values().any(|i| {
            i.owner_id == char_id
                && i.type_id == 34
                && i.quantity >= 500
                && i.flag == InventoryFlag::Hangar
        });
        assert!(has_tritanium, "Given item must be present in hangar");
    });

    // 3. /setskill <skill_id> <level>
    let setskill_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "slash",
            None,
            "SlashCmd",
            &[PyRep::String("/setskill 3300 5".into())],
            &kw,
            None,
        )
        .await
        .expect("/setskill 3300 5");
    let PyRep::KeyVal(sk_kv) = setskill_rep else {
        panic!("Expected KeyVal from /setskill");
    };
    assert_eq!(sk_kv.get("level"), Some(&PyRep::Integer(5)));

    ctx.db.with_snapshot(|snap| {
        let sk = snap.skills.get(&char_id).and_then(|m| m.get(&3300)).unwrap();
        assert_eq!(sk.level, 5);
        assert_eq!(sk.skill_points, 256_000.0);
    });

    // 4. /teleport <solar_system_id>
    let teleport_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "slash",
            None,
            "SlashCmd",
            &[PyRep::String("/teleport 30000142".into())],
            &kw,
            None,
        )
        .await
        .expect("/teleport 30000142");
    let PyRep::KeyVal(tp_kv) = teleport_rep else {
        panic!("Expected KeyVal from /teleport");
    };
    assert_eq!(tp_kv.get("solarSystemID"), Some(&PyRep::Integer(30000142)));
    assert_eq!(sess.solar_system_id, Some(30000142));

    ctx.db.with_snapshot(|snap| {
        let ch = &snap.characters[&char_id];
        assert_eq!(ch.solar_system_id, 30000142);
        assert_eq!(ch.station_id, None);
    });

    // Unknown solar system
    let bad_sys = ctx
        .dispatch_rpc(
            &mut sess,
            "slash",
            None,
            "SlashCmd",
            &[PyRep::String("/teleport 99999999".into())],
            &kw,
            None,
        )
        .await;
    assert_eq!(bad_sys.unwrap_err().code, "SolarSystemNotFound");

    // 5. /spawnnpc <type_id> in space
    sess.ship_id = Some(991_000_101);
    let spawn_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "slash",
            None,
            "SlashCmd",
            &[PyRep::String("/spawnnpc 601".into())],
            &kw,
            None,
        )
        .await
        .expect("/spawnnpc 601");
    let PyRep::KeyVal(sp_kv) = spawn_rep else {
        panic!("Expected KeyVal from /spawnnpc");
    };
    assert_eq!(sp_kv.get("typeID"), Some(&PyRep::Integer(601)));
    assert_eq!(sp_kv.get("solarSystemID"), Some(&PyRep::Integer(30000142)));

    // 6. Unknown slash command error
    let unknown_cmd = ctx
        .dispatch_rpc(
            &mut sess,
            "slash",
            None,
            "SlashCmd",
            &[PyRep::String("/unsupportedcmd 123".into())],
            &kw,
            None,
        )
        .await;
    assert_eq!(unknown_cmd.unwrap_err().code, "UnknownSlashCommand");
}

#[tokio::test]
async fn test_ch5_neural_attribute_remap_and_cooldown() {
    // parity: feat:attributes.neural-remap, feat:attributes.bonus-remap, rpc:charUnboundMgr.SelectCharacterID
    eve_testkit::attest(&[
        "feat:attributes.neural-remap",
        "feat:attributes.bonus-remap",
    ]);
    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();
    let mut sess = SessionState::new(923);

    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;

    let char_id = 90_000_001;
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");

    // 1. Validation bounds: each attribute must be 17..=27 and sum must equal 99
    // Sum != 99
    let bad_sum = ctx
        .dispatch_rpc(
            &mut sess,
            "charMgr",
            None,
            "RespecCharacter",
            &[
                PyRep::Integer(17),
                PyRep::Integer(17),
                PyRep::Integer(17),
                PyRep::Integer(17),
                PyRep::Integer(17), // sum = 85
            ],
            &kw,
            None,
        )
        .await;
    assert_eq!(bad_sum.unwrap_err().code, "RemapFailed");

    // Under min (< 17)
    let under_min = ctx
        .dispatch_rpc(
            &mut sess,
            "charMgr",
            None,
            "RespecCharacter",
            &[
                PyRep::Integer(16),
                PyRep::Integer(28),
                PyRep::Integer(21),
                PyRep::Integer(17),
                PyRep::Integer(17),
            ],
            &kw,
            None,
        )
        .await;
    assert_eq!(under_min.unwrap_err().code, "RemapFailed");

    // Over max (> 27)
    let over_max = ctx
        .dispatch_rpc(
            &mut sess,
            "charMgr",
            None,
            "RespecCharacter",
            &[
                PyRep::Integer(17),
                PyRep::Integer(28),
                PyRep::Integer(20),
                PyRep::Integer(17),
                PyRep::Integer(17),
            ],
            &kw,
            None,
        )
        .await;
    assert_eq!(over_max.unwrap_err().code, "RemapFailed");

    // 2. Timed yearly remap (365 days / 31,536,000s)
    let now_unix = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.neural.last_remap_unix = None;
        ch.neural.remap_cooldown_until_unix = None;
        ch.neural.bonus_remaps = 0;
    });

    let remap_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "charMgr",
            None,
            "RespecCharacter",
            &[
                PyRep::Integer(17),
                PyRep::Integer(27),
                PyRep::Integer(21),
                PyRep::Integer(17),
                PyRep::Integer(17), // sum = 99
            ],
            &kw,
            None,
        )
        .await
        .expect("RespecCharacter");
    let PyRep::KeyVal(rm_kv) = remap_rep else {
        panic!("Expected KeyVal from RespecCharacter");
    };
    assert_eq!(rm_kv.get("intelligence"), Some(&PyRep::Integer(27)));
    assert_eq!(rm_kv.get("memory"), Some(&PyRep::Integer(21)));

    ctx.db.with_snapshot(|snap| {
        let ch = &snap.characters[&char_id];
        assert_eq!(ch.neural.charisma, 17);
        assert_eq!(ch.neural.intelligence, 27);
        assert_eq!(ch.neural.memory, 21);
        assert_eq!(ch.neural.perception, 17);
        assert_eq!(ch.neural.willpower, 17);
        assert!(ch.neural.last_remap_unix.is_some());
        let cooldown = ch.neural.remap_cooldown_until_unix.unwrap();
        // Cooldown is 31,536,000s (1 year)
        assert!(cooldown >= now_unix + 31_535_900 && cooldown <= now_unix + 31_536_100);
    });

    // 3. Second remap with no bonus remaps rejected due to cooldown
    let cooldown_err = ctx
        .dispatch_rpc(
            &mut sess,
            "charMgr",
            None,
            "RespecCharacter",
            &[
                PyRep::Integer(21),
                PyRep::Integer(21),
                PyRep::Integer(21),
                PyRep::Integer(18),
                PyRep::Integer(18),
            ],
            &kw,
            None,
        )
        .await;
    assert_eq!(cooldown_err.unwrap_err().code, "RemapFailed");

    // 4. Consume bonus remap allows remapping during cooldown
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.neural.bonus_remaps = 1;
    });

    let bonus_remap = ctx
        .dispatch_rpc(
            &mut sess,
            "charMgr",
            None,
            "RespecCharacter",
            &[
                PyRep::Integer(21),
                PyRep::Integer(21),
                PyRep::Integer(21),
                PyRep::Integer(18),
                PyRep::Integer(18), // sum = 99
            ],
            &kw,
            None,
        )
        .await
        .expect("RespecCharacter with bonus remap");
    let PyRep::KeyVal(b_kv) = bonus_remap else {
        panic!("Expected KeyVal from RespecCharacter");
    };
    assert_eq!(b_kv.get("bonusRemaps"), Some(&PyRep::Integer(0)));

    ctx.db.with_snapshot(|snap| {
        let ch = &snap.characters[&char_id];
        assert_eq!(ch.neural.bonus_remaps, 0);
        assert_eq!(ch.neural.charisma, 21);
        assert_eq!(ch.neural.intelligence, 21);
        assert_eq!(ch.neural.memory, 21);
        assert_eq!(ch.neural.perception, 18);
        assert_eq!(ch.neural.willpower, 18);
        let cooldown = ch.neural.remap_cooldown_until_unix.unwrap();
        assert!(cooldown >= now_unix + 31_535_900 && cooldown <= now_unix + 31_536_100);
    });

    let respec_info = ctx
        .dispatch_rpc(&mut sess, "skillMgr2", None, "GetRespecInfo", &[], &kw, None)
        .await
        .expect("GetRespecInfo after bonus remap");
    let PyRep::KeyVal(respec_kv) = respec_info else {
        panic!("Expected KeyVal from GetRespecInfo");
    };
    assert_eq!(respec_kv.get("freeRespecs"), Some(&PyRep::Integer(0)));

    // 5. ESI GET /characters/{character_id}/attributes includes remap timestamps
    let token = issue_token_with_scopes(
        &ctx,
        char_id,
        vec!["esi-skills.read_skills.v1".to_string()],
    );
    let esi_router = EsiRouter::new(Arc::clone(&ctx));
    let req = EsiHttpRequest::new("GET", &format!("/v1/characters/{char_id}/attributes/"))
        .with_bearer_token(&token);
    let resp = esi_router.handle_request(&req);
    assert_eq!(resp.status_code, 200);
    let json: Value = serde_json::from_slice(&resp.body).expect("parse json");
    assert_eq!(json["charisma"], 21);
    assert_eq!(json["intelligence"], 21);
    assert!(json["last_remap_date"].as_str().is_some());
    assert!(json["accrued_remap_cooldown_date"].as_str().is_some());
}
