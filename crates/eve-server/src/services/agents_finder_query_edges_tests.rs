//! Edge-case unit tests for `agents_finder_query`:
//! positional `args` and `kwargs` filters (`level`, `divisionID`/`division`,
//! `factionID`/`faction`, `corporationID`/`corporation`, `agentTypeID`/`agentType`,
//! `securityStatus`/`security`), security bands (`highsec`/`high`, `lowsec`/`low`,
//! `nullsec`/`null`, numeric string, `Real`, `Integer`, fallback), and `hasAccess`
//! with/without character session & standing thresholds.

use super::*;
use eve_db::clock::VirtualServerClock;
use eve_db::WorldRepository;
use eve_sde::SdeRegistry;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn make_ctx_and_session(char_id: Option<i64>) -> (ServerContext, SessionState) {
    let sde = shared_sde();
    let db = Arc::new(WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(sde, db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_776_200_000)));
    let mut session = SessionState::default();
    session.character_id = char_id;
    (ctx, session)
}

fn rowset_rows(rep: PyRep) -> (Vec<String>, Vec<Vec<PyRep>>) {
    match rep {
        PyRep::Rowset { columns, rows } => (columns, rows),
        other => panic!("expected PyRep::Rowset, got {other:?}"),
    }
}

#[test]
fn get_agents_unfiltered_and_has_access_with_and_without_character() {
    let (ctx, no_char_sess) = make_ctx_and_session(None);
    let kw = HashMap::new();
    let res = handle_get_agents(&ctx, &no_char_sess, &[], &kw).unwrap();
    let (header, rows) = rowset_rows(res);
    assert_eq!(
        header,
        vec![
            "agentID",
            "agentTypeID",
            "divisionID",
            "corporationID",
            "factionID",
            "solarsystemID",
            "stationID",
            "level",
            "quality",
            "hasAccess",
            "isLocatorAgent",
        ]
    );
    assert!(!rows.is_empty());

    // Without character, level <= 1 hasAccess = true, level > 1 hasAccess = false
    for row in &rows {
        let lvl = row[7].as_i64().unwrap();
        let access = row[9].as_bool().unwrap();
        assert_eq!(access, lvl <= 1);
    }

    // Verify sorted by agentID
    assert!(rows.windows(2).all(|w| w[0][0].as_i64() < w[1][0].as_i64()));

    // With character having high standing toward a level 4 agent's corporation
    let sample_l4 = ctx
        .sde
        .agents
        .values()
        .find(|a| a.level >= 2)
        .expect("level >= 2 agent in SDE");
    let char_id = 90_000_001i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.standings.insert(sample_l4.corporation_id, 9.5);
        }
    });
    let (_, char_sess) = make_ctx_and_session(Some(char_id));
    let char_sess_res = handle_get_agents(
        &ctx,
        &char_sess,
        &[
            PyRep::Integer(sample_l4.level as i64),
            PyRep::None,
            PyRep::None,
            PyRep::Integer(sample_l4.corporation_id),
        ],
        &kw,
    )
    .unwrap();
    let (_, l4_rows) = rowset_rows(char_sess_res);
    assert!(!l4_rows.is_empty());
    assert!(l4_rows.iter().all(|r| r[9].as_bool() == Some(true)));
}

#[test]
fn get_agents_positional_and_kwarg_filters() {
    let (ctx, sess) = make_ctx_and_session(Some(90_000_001));
    let sample = ctx
        .sde
        .agents
        .values()
        .find(|a| {
            ctx.sde
                .npc_corporations
                .get(&a.corporation_id)
                .and_then(|c| c.faction_id)
                .is_some()
        })
        .expect("agent with faction in SDE");
    let fac_id = ctx.sde.npc_corporations[&sample.corporation_id]
        .faction_id
        .unwrap() as i64;

    // Positional args: level, divisionID, factionID, corporationID, agentTypeID
    let pos_res = handle_get_agents(
        &ctx,
        &sess,
        &[
            PyRep::Integer(sample.level as i64),
            PyRep::Integer(sample.division_id as i64),
            PyRep::Integer(fac_id),
            PyRep::Integer(sample.corporation_id),
            PyRep::Integer(sample.agent_type_id as i64),
        ],
        &HashMap::new(),
    )
    .unwrap();
    let (_, pos_rows) = rowset_rows(pos_res);
    assert!(!pos_rows.is_empty());
    assert!(pos_rows
        .iter()
        .any(|r| r[0].as_i64() == Some(sample.agent_id)));

    // Primary kwarg keys
    let kw_primary = HashMap::from([
        ("level".into(), PyRep::Integer(sample.level as i64)),
        ("divisionID".into(), PyRep::Integer(sample.division_id as i64)),
        ("factionID".into(), PyRep::Integer(fac_id)),
        ("corporationID".into(), PyRep::Integer(sample.corporation_id)),
        ("agentTypeID".into(), PyRep::Integer(sample.agent_type_id as i64)),
    ]);
    let (_, kw1_rows) = rowset_rows(handle_get_agents(&ctx, &sess, &[], &kw_primary).unwrap());
    assert_eq!(kw1_rows.len(), pos_rows.len());

    // Alias kwarg keys
    let kw_alias = HashMap::from([
        ("level".into(), PyRep::Integer(sample.level as i64)),
        ("division".into(), PyRep::Integer(sample.division_id as i64)),
        ("faction".into(), PyRep::Integer(fac_id)),
        ("corporation".into(), PyRep::Integer(sample.corporation_id)),
        ("agentType".into(), PyRep::Integer(sample.agent_type_id as i64)),
    ]);
    let (_, kw2_rows) = rowset_rows(handle_get_agents(&ctx, &sess, &[], &kw_alias).unwrap());
    assert_eq!(kw2_rows.len(), pos_rows.len());
}

#[test]
fn get_agents_security_status_filters_all_variants() {
    let (ctx, sess) = make_ctx_and_session(None);

    let sec_for_row = |row: &[PyRep]| -> f64 {
        row[5]
            .as_i64()
            .and_then(|sid| ctx.sde.solar_systems.get(&(sid as u32)))
            .map(|s| s.security_status)
            .unwrap_or(1.0)
    };

    for label in ["highsec", "high"] {
        let kw = HashMap::from([("securityStatus".into(), PyRep::String(label.into()))]);
        let (_, rows) = rowset_rows(handle_get_agents(&ctx, &sess, &[], &kw).unwrap());
        assert!(!rows.is_empty());
        assert!(rows.iter().all(|r| sec_for_row(r) >= 0.45));
    }

    for label in ["lowsec", "low"] {
        let kw = HashMap::from([("security".into(), PyRep::String(label.into()))]);
        let (_, rows) = rowset_rows(handle_get_agents(&ctx, &sess, &[], &kw).unwrap());
        assert!(rows
            .iter()
            .all(|r| (0.05..0.45).contains(&sec_for_row(r))));
    }

    for label in ["nullsec", "null"] {
        let kw = HashMap::from([("security".into(), PyRep::String(label.into()))]);
        let (_, rows) = rowset_rows(handle_get_agents(&ctx, &sess, &[], &kw).unwrap());
        assert!(rows.iter().all(|r| sec_for_row(r) < 0.05));
    }

    // Numeric string, Real, Integer, unparseable string, and PyRep::None via args[5]
    let kw_num_str = HashMap::from([("securityStatus".into(), PyRep::String("0.5".into()))]);
    let (_, num_rows) = rowset_rows(handle_get_agents(&ctx, &sess, &[], &kw_num_str).unwrap());
    assert!(num_rows.iter().all(|r| sec_for_row(r) >= 0.45));

    let kw_real = HashMap::from([("securityStatus".into(), PyRep::Real(0.5))]);
    let (_, real_rows) = rowset_rows(handle_get_agents(&ctx, &sess, &[], &kw_real).unwrap());
    assert_eq!(real_rows.len(), num_rows.len());

    let kw_int = HashMap::from([("securityStatus".into(), PyRep::Integer(1))]);
    let (_, int_rows) = rowset_rows(handle_get_agents(&ctx, &sess, &[], &kw_int).unwrap());
    assert!(int_rows.iter().all(|r| sec_for_row(r) >= 0.95));

    let kw_bad_str = HashMap::from([("securityStatus".into(), PyRep::String("not-a-num".into()))]);
    let (_, all_rows) = rowset_rows(handle_get_agents(&ctx, &sess, &[], &HashMap::new()).unwrap());
    let (_, bad_rows) = rowset_rows(handle_get_agents(&ctx, &sess, &[], &kw_bad_str).unwrap());
    assert_eq!(bad_rows.len(), all_rows.len());

    // Non-numeric / non-string args[5] falls through to true
    let (_, none_sec_rows) = rowset_rows(
        handle_get_agents(
            &ctx,
            &sess,
            &[
                PyRep::None,
                PyRep::None,
                PyRep::None,
                PyRep::None,
                PyRep::None,
                PyRep::Bool(true),
            ],
            &HashMap::new(),
        )
        .unwrap(),
    );
    assert_eq!(none_sec_rows.len(), all_rows.len());
}
