//! Additional branch-coverage and mutation-killing unit tests for `quasar_career_goals`.

#![forbid(unsafe_code)]

use super::quasar_career_goals::*;
use crate::quasar_gateway::{
    PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_FORBIDDEN, STATUS_NOT_FOUND,
};
use eve_db::{IskCents, WorldRepository};
use eve_proto::quasar::{
    encode_varint_field, AuthoritativeContext, DynamicQuasarMessage, DynamicValue, GoogleAny,
    QuasarDescriptorRegistry,
};
use eve_sde::SdeRegistry;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};

#[cfg(test)]
#[path = "quasar_career_goals_gateway_unit_tests.rs"]
mod quasar_career_goals_gateway_unit_tests;

pub(super) static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

pub(super) fn shared_sde() -> Arc<SdeRegistry> {
    Arc::clone(SHARED_SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

pub(super) fn make_def(career_id: u32, activity_name: &str, activity_kind: &str) -> CareerGoalDef {
    CareerGoalDef {
        uuid: [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 5],
        career_id,
        activity_name: activity_name.into(),
        activity_kind: activity_kind.into(),
        name: "Goal".into(),
        description: "Desc".into(),
        target: 10,
        career_points: 20,
        isk_reward: 1_000,
        sp_reward: 0.0,
        name_message_id: 0,
        description_message_id: 0,
        threat: 0,
        ..Default::default()
    }
}

pub(super) fn uuid_req(field: u32, uuid: &[u8; 16]) -> DynamicQuasarMessage {
    let mut id_msg = DynamicQuasarMessage::new("Identifier");
    id_msg.fields.insert(1, DynamicValue::Bytes(uuid.to_vec()));
    let mut req = DynamicQuasarMessage::new("Req");
    req.fields
        .insert(field, DynamicValue::Message(Box::new(id_msg)));
    req
}

#[test]
fn effective_group_exhaustive_matrix_and_message_id_threat_boundaries() {
    // 1. Career 1 (Explorer) — all 9 named groups via both activity_name and activity_kind + default
    let c1_cases = [
        ("Scanning", "", "Scanning"),
        ("", "exploration", "Scanning"),
        ("Hacking", "", "Hacking"),
        ("", "hacking", "Hacking"),
        ("Advanced Hacking", "", "AdvancedHacking"),
        ("", "advanced_hacking", "AdvancedHacking"),
        ("Wormholes", "", "Wormholes"),
        ("", "wormholes", "Wormholes"),
        ("Gas Sites", "", "GasSites"),
        ("", "gas_sites", "GasSites"),
        ("Combat Sites", "", "CombatSites"),
        ("", "combat_sites", "CombatSites"),
        ("Navigation", "", "Navigation"),
        ("", "travel", "Navigation"),
        ("Advanced Navigation", "", "AdvancedNavigation"),
        ("", "advanced_travel", "AdvancedNavigation"),
        ("Project Discovery", "", "ProjectDiscovery"),
        ("", "project_discovery", "ProjectDiscovery"),
        ("Other", "other", "CareerAgentMissions"),
    ];
    for (name, kind, expected) in c1_cases {
        assert_eq!(make_def(1, name, kind).effective_group(), expected);
    }

    // 2. Career 2 (Industrialist) — all 8 named groups + default
    let c2_cases = [
        ("Market", "", "Market"),
        ("", "market", "Market"),
        ("Salvage", "", "Salvage"),
        ("", "salvage", "Salvage"),
        ("Resource Harvesting", "", "ResourceHarvesting"),
        ("", "mining", "ResourceHarvesting"),
        ("Refining", "", "Refining"),
        ("", "refining", "Refining"),
        ("Manufacturing", "", "Manufacturing"),
        ("", "manufacturing", "Manufacturing"),
        ("Distribution Agent", "", "DistributionAgent"),
        ("", "distribution_missions", "DistributionAgent"),
        ("Mining Agent", "", "MiningAgent"),
        ("", "mining_missions", "MiningAgent"),
        ("Research", "", "Research"),
        ("", "research", "Research"),
        ("Unknown", "unknown", "IndustrialistCareerAgentsMissions"),
    ];
    for (name, kind, expected) in c2_cases {
        assert_eq!(make_def(2, name, kind).effective_group(), expected);
    }

    // 3. Career 3 (Enforcer) — all 9 named groups + default
    let c3_cases = [
        ("Security Agent", "", "SecurityAgent"),
        ("", "combat_missions", "SecurityAgent"),
        ("Abyssal Deadspace", "", "AbyssalDeadspace"),
        ("", "abyssal", "AbyssalDeadspace"),
        ("Combat", "", "Combat"),
        ("", "combat", "Combat"),
        ("Bounties", "", "Bounties"),
        ("", "bounties", "Bounties"),
        ("Combat Sites", "", "CombatSites"),
        ("", "combat_sites", "CombatSites"),
        ("Epic Arc", "", "EpicArc"),
        ("", "epic_arc", "EpicArc"),
        ("Market", "", "Market"),
        ("", "market", "Market"),
        ("Standings", "", "Standings"),
        ("", "standings", "Standings"),
        ("Loyalty Points", "", "LoyaltyPoints"),
        ("", "loyalty_points", "LoyaltyPoints"),
        ("Unknown", "unknown", "CareerAgentMissions"),
    ];
    for (name, kind, expected) in c3_cases {
        assert_eq!(make_def(3, name, kind).effective_group(), expected);
    }

    // 4. Career 4 (Soldier of Fortune) — all 9 named groups + default
    let c4_cases = [
        ("Social", "", "Social"),
        ("", "social", "Social"),
        ("Electronic Warfare", "", "ElectronicWarfare"),
        ("", "ewar", "ElectronicWarfare"),
        ("Combat", "", "Combat"),
        ("", "combat", "Combat"),
        ("PvP", "", "PvP"),
        ("", "pvp", "PvP"),
        ("Capacitor Warfare", "", "CapacitorWarfare"),
        ("", "cap_warfare", "CapacitorWarfare"),
        ("Faction Warfare", "", "FactionWarfare"),
        ("", "faction_warfare", "FactionWarfare"),
        ("Support", "", "Support"),
        ("", "remote_repair", "Support"),
        ("Destruction", "", "Destruction"),
        ("", "destruction", "Destruction"),
        ("Duels", "", "Duels"),
        ("", "duels", "Duels"),
        ("Unknown", "unknown", "CareerAgentMissions"),
    ];
    for (name, kind, expected) in c4_cases {
        assert_eq!(make_def(4, name, kind).effective_group(), expected);
    }

    // 5. Message IDs for all 4 career paths + unknown career_id fallback (582_520 / 582_521)
    for &(cid, base_n, base_d) in &CAREER_PATH_MESSAGE_IDS {
        let d = make_def(cid, "Combat", "combat");
        assert_eq!(d.effective_name_message_id(), base_n);
        assert_eq!(d.effective_description_message_id(), base_d);
    }
    let unknown_career = make_def(99, "Combat", "combat");
    assert_eq!(unknown_career.effective_name_message_id(), 582_520);
    assert_eq!(
        unknown_career.effective_description_message_id(),
        582_521
    );

    // 5b. Stale persisted synthesized IDs (e.g. 665529..665537 / 696260..696439) heal from default_career_goal_definitions
    let stale_db = Arc::new(WorldRepository::with_demo_seed());
    let mut stale_defs = default_career_goal_definitions();
    let sample_uuid = stale_defs[0].uuid;
    let expected_name_id = stale_defs[0].name_message_id;
    let expected_desc_id = stale_defs[0].description_message_id;
    stale_defs[0].name_message_id = 665_529;
    stale_defs[0].description_message_id = 696_260;
    stale_defs[0].aura_text_id = None;
    stale_db.with_mut_snapshot(|snap| {
        snap.quasar_state_blobs.insert(
            "quasar_career_goal_defs".into(),
            serde_json::to_string(&stale_defs).unwrap(),
        );
    });
    let healed_mgr = CareerGoalManager::new(Some(Arc::clone(&stale_db)));
    let healed_def = healed_mgr
        .load_definitions()
        .into_iter()
        .find(|d| d.uuid == sample_uuid)
        .unwrap();
    assert_eq!(healed_def.effective_name_message_id(), expected_name_id);
    assert_eq!(healed_def.effective_description_message_id(), expected_desc_id);
    assert!(healed_def.aura_text_id.is_some());

    // 6. Threat boundary values (<= 15 -> 1, 16..=25 -> 2, > 25 -> 3, explicit clamp(1, 3))
    let mut d = make_def(1, "Scanning", "exploration");
    d.career_points = 15;
    assert_eq!(d.effective_threat(), 1);
    d.career_points = 16;
    assert_eq!(d.effective_threat(), 2);
    d.career_points = 25;
    assert_eq!(d.effective_threat(), 2);
    d.career_points = 26;
    assert_eq!(d.effective_threat(), 3);
    d.threat = 9;
    assert_eq!(d.effective_threat(), 3);
    d.threat = 1;
    assert_eq!(d.effective_threat(), 1);
}

#[test]
fn manager_persistence_claim_rollbacks_and_edge_cases() {
    let db = Arc::new(WorldRepository::with_demo_seed());
    let u1 = [0x11; 16];
    let u_stale = [0x99; 16];

    let def1 = CareerGoalDef {
        uuid: u1,
        career_id: 1,
        activity_name: "Hacking".into(),
        activity_kind: "hacking_persist".into(),
        name: "Persisted Goal".into(),
        description: "Desc".into(),
        target: 5,
        career_points: 750,
        isk_reward: 50_000,
        sp_reward: 250.0,
        name_message_id: 0,
        description_message_id: 0,
        threat: 2,
        ..Default::default()
    };

    // Pre-populate quasar_state_blobs with:
    // - quasar_career_goal_defs containing def1
    // - quasar_career_goals containing a goal for u1 with empty activity_kind (should be backfilled) AND u_stale (should be pruned)
    // - quasar_career_milestones with milestone 101 claimed
    let cid = 90_000_001_u32;
    let mut def_empty_kind = def1.clone();
    def_empty_kind.activity_kind = String::new();
    let mut init_goals = HashMap::new();
    init_goals.insert(
        cid,
        vec![
            CharacterCareerGoal {
                uuid: u1,
                def: def_empty_kind,
                progress: 2,
                completed: false,
                claimed: false,
                completed_time: None,
            },
            CharacterCareerGoal {
                uuid: u_stale,
                def: make_def(1, "Stale", "stale"),
                progress: 1,
                completed: false,
                claimed: false,
                completed_time: None,
            },
        ],
    );
    let mut init_ms = HashMap::new();
    init_ms.insert(cid, HashSet::from([101_u32]));

    db.with_mut_snapshot(|snap| {
        snap.quasar_state_blobs.insert(
            "quasar_career_goal_defs".into(),
            serde_json::to_string(&vec![def1.clone()]).unwrap(),
        );
        snap.quasar_state_blobs.insert(
            "quasar_career_goals".into(),
            serde_json::to_string(&init_goals).unwrap(),
        );
        snap.quasar_state_blobs.insert(
            "quasar_career_milestones".into(),
            serde_json::to_string(&init_ms).unwrap(),
        );
    });

    let mgr = CareerGoalManager::new(Some(Arc::clone(&db)));
    let loaded_goals = mgr.get_or_init_goals(cid);
    assert_eq!(loaded_goals.len(), 1);
    assert_eq!(loaded_goals[0].uuid, u1);
    assert_eq!(loaded_goals[0].def.activity_kind, "hacking_persist");

    // Empty definitions fallback in load_definitions & configure_definition updating existing def
    *mgr.definitions.write().unwrap() = Vec::new();
    assert!(!mgr.load_definitions().is_empty());
    *mgr.definitions.write().unwrap() = vec![def1.clone()];
    let mut updated_def = def1.clone();
    updated_def.isk_reward = 100_000;
    mgr.configure_definition(updated_def);
    assert_eq!(mgr.load_definitions()[0].isk_reward, 100_000);

    // add_progress & mark_completed for unknown UUID
    assert!(mgr.add_progress(cid, &[0xFF; 16], 1, 100).is_none());
    assert_eq!(
        mgr.mark_completed(cid, &[0xFF; 16], 100).unwrap_err().0,
        STATUS_NOT_FOUND
    );

    // claim_reward error cases:
    // 1. Character not in character_career_goals map
    assert_eq!(
        mgr.claim_reward(88_888_888, &u1, &db).unwrap_err().0,
        STATUS_NOT_FOUND
    );
    // 2. Goal not completed yet
    assert_eq!(
        mgr.claim_reward(cid, &u1, &db).unwrap_err().0,
        STATUS_FORBIDDEN
    );
    // 3. Goal not found for initialized character
    assert_eq!(
        mgr.claim_reward(cid, &[0xEE; 16], &db).unwrap_err().0,
        STATUS_NOT_FOUND
    );

    // Complete goal, then test rollback when character is missing in DB or balance overflows
    mgr.mark_completed(cid, &u1, 200).unwrap();
    let ghost_cid = 77_777_777_u32;
    mgr.mark_completed(ghost_cid, &u1, 200).unwrap();
    assert_eq!(
        mgr.claim_reward(ghost_cid, &u1, &db).unwrap_err().0,
        STATUS_BAD_REQUEST
    );
    // Verify ghost_cid's goal.claimed was rolled back to false!
    assert!(!mgr.get_or_init_goals(ghost_cid)[0].claimed);

    // Balance overflow rollback on cid
    let orig_bal = db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&(cid as i64)).unwrap();
        let b = ch.balance;
        ch.balance = IskCents::from_raw_cents(i64::MAX);
        b
    });
    assert_eq!(
        mgr.claim_reward(cid, &u1, &db).unwrap_err().0,
        STATUS_BAD_REQUEST
    );
    assert!(!mgr.get_or_init_goals(cid)[0].claimed);

    // Restore balance and succeed!
    db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&(cid as i64)).unwrap().balance = orig_bal;
    });
    let claimed_goal = mgr.claim_reward(cid, &u1, &db).expect("claim succeeds");
    assert!(claimed_goal.claimed);
    // Claiming a second time returns STATUS_FORBIDDEN
    assert_eq!(
        mgr.claim_reward(cid, &u1, &db).unwrap_err().0,
        STATUS_FORBIDDEN
    );

    // claim_milestone_reward error cases:
    // 1. Unknown milestone ID
    assert_eq!(
        mgr.claim_milestone_reward(cid, 999, &db).unwrap_err().0,
        STATUS_NOT_FOUND
    );
    // 2. Insufficient career points (career 2 has 0 CP, milestone 102 requires 1500 CP)
    assert_eq!(
        mgr.claim_milestone_reward(cid, 2, &db).unwrap_err().0,
        STATUS_FORBIDDEN
    );
    assert_eq!(
        mgr.claim_milestone_reward(cid, 102, &db).unwrap_err().0,
        STATUS_FORBIDDEN
    );
    // 3. Milestone 101 was pre-loaded as claimed -> already claimed
    assert_eq!(
        mgr.claim_milestone_reward(cid, 101, &db).unwrap_err().0,
        STATUS_FORBIDDEN
    );
    // 4. Ghost character rollback when claiming milestone 1 (ghost_cid has 750 CP in career 1)
    assert_eq!(
        mgr.claim_milestone_reward(ghost_cid, 1, &db).unwrap_err().0,
        STATUS_BAD_REQUEST
    );
    assert!(
        !mgr.claimed_milestones
            .read()
            .unwrap()
            .get(&ghost_cid)
            .unwrap()
            .contains(&1)
    );
}
