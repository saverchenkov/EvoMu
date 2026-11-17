//! Comprehensive unit tests for `quasar_achievements` covering serde visitors, category keyword
//! classification, SDE definition hydration, filtered ISK/SP reward claims, and FlatBuffer seeding.

#![forbid(unsafe_code)]

use super::*;
use crate::quasar_gateway::PublicGatewayRouter;
use eve_db::WorldRepository;
use eve_proto::quasar::{DynamicQuasarMessage, QuasarDescriptorRegistry};
use eve_sde::SdeRegistry;
use std::sync::{Arc, OnceLock};

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn shared_sde() -> Arc<SdeRegistry> {
    Arc::clone(SHARED_SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

#[test]
fn serde_i64_or_f64_visitor_and_ident_extractors() {
    // deserialize_i64_or_f64: positive u64, negative i64, f64 rounding, and invalid type error (`expecting`)
    let d_float: AchievementDef = serde_json::from_str(
        r#"{"id":1,"category_id":1,"name":"n","target":5,"score":10,"milestone_targets":[5],"reward_isk":250000.6,"reward_sp":-12}"#,
    )
    .expect("float/i64 rewards");
    assert_eq!(d_float.reward_isk, 250_001);
    assert_eq!(d_float.reward_sp, -12);

    let bad = serde_json::from_str::<AchievementDef>(
        r#"{"id":1,"category_id":1,"name":"n","target":5,"score":10,"milestone_targets":[],"reward_isk":"not_a_num"}"#,
    );
    let err_msg = bad.unwrap_err().to_string();
    assert!(err_msg.contains("integer or floating point"));

    // extract_u32_ident & extract_u64_val across all variants & boundaries
    assert_eq!(extract_u32_ident(&DynamicValue::Uint64(77)), Some(77));
    assert_eq!(extract_u32_ident(&DynamicValue::Uint64(u64::MAX)), None);
    assert_eq!(extract_u32_ident(&DynamicValue::Int32(88)), Some(88));
    assert_eq!(extract_u32_ident(&DynamicValue::Int32(-1)), None);
    assert_eq!(extract_u32_ident(&DynamicValue::Int64(99)), Some(99));
    assert_eq!(
        extract_u32_ident(&DynamicValue::Bytes(vec![0, 0, 0, 0, 0, 0, 1, 2])),
        Some(258)
    );
    assert_eq!(extract_u32_ident(&DynamicValue::Bytes(vec![1, 2, 3])), None);
    assert_eq!(extract_u32_ident(&DynamicValue::Bool(true)), None);

    let mut sub_bytes = DynamicQuasarMessage::new("Ident");
    sub_bytes.fields.insert(1, DynamicValue::Bytes(vec![0; 12].into_iter().chain(42_u32.to_be_bytes()).collect()));
    assert_eq!(extract_u32_ident(&DynamicValue::Message(Box::new(sub_bytes))), Some(42));

    let mut sub_u64 = DynamicQuasarMessage::new("Ident");
    sub_u64.fields.insert(1, DynamicValue::Uint64(314));
    assert_eq!(extract_u32_ident(&DynamicValue::Message(Box::new(sub_u64))), Some(314));

    let mut sub_i32 = DynamicQuasarMessage::new("Ident");
    sub_i32.fields.insert(1, DynamicValue::Int32(512));
    assert_eq!(extract_u32_ident(&DynamicValue::Message(Box::new(sub_i32))), Some(512));

    let mut sub_i64 = DynamicQuasarMessage::new("Ident");
    sub_i64.fields.insert(1, DynamicValue::Int64(1024));
    assert_eq!(extract_u32_ident(&DynamicValue::Message(Box::new(sub_i64))), Some(1024));

    let mut sub_bad = DynamicQuasarMessage::new("Ident");
    sub_bad.fields.insert(1, DynamicValue::Bool(false));
    assert_eq!(extract_u32_ident(&DynamicValue::Message(Box::new(sub_bad))), None);

    assert_eq!(extract_u64_val(&DynamicValue::Uint64(500)), Some(500));
    assert_eq!(extract_u64_val(&DynamicValue::Uint32(400)), Some(400));
    assert_eq!(extract_u64_val(&DynamicValue::Int32(300)), Some(300));
    assert_eq!(extract_u64_val(&DynamicValue::Int32(-1)), None);
    assert_eq!(extract_u64_val(&DynamicValue::Int64(-5)), None);
    assert_eq!(extract_u64_val(&DynamicValue::String("x".into())), None);
}

#[test]
fn sde_hydration_db_reload_and_filtered_wallet_sp_reward_claims() {
    // Empty SDE without db_path hits `load_achievements() == Err` fallback (`b"[]"`)
    let empty_sde = SdeRegistry::default();
    let (empty_defs, empty_keys, empty_cnt, empty_hash, empty_json, _empty_fb) = build_definitions_from_sde(&empty_sde);
    assert!(empty_defs.is_empty() && empty_keys.is_empty() && empty_cnt == 0);
    assert_eq!(empty_json, b"[]");
    assert_eq!(empty_hash.len(), 16);

    // Real SDE hydration + DB reload refreshing stale `a.def` and parsing `quasar_achievement_cat_scores`
    let db = Arc::new(WorldRepository::with_demo_seed());
    let sde = shared_sde();
    let mgr_initial = {
        let _env_read = crate::lock_test_env_read();
        AchievementManager::new_with_sde(Some(Arc::clone(&db)), Some(Arc::clone(&sde)))
    };
    assert!(!mgr_initial.definitions.is_empty());
    let sample_id = mgr_initial.definitions[0].id;
    let cid = 90_000_001_u32;

    mgr_initial.unlock_achievement(cid, sample_id);
    mgr_initial.set_category_score(cid, 2, 120);
    // Inject a malformed key into `quasar_achievement_cat_scores` to verify graceful skip on reload
    db.with_mut_snapshot(|snap| {
        snap.quasar_state_blobs.insert(
            "quasar_achievement_cat_scores".into(),
            format!(r#"{{"{cid}:2":120,"bad_key":99,"{cid}:bad":50}}"#),
        );
    });

    let mgr_reloaded = {
        let _env_read = crate::lock_test_env_read();
        AchievementManager::new_with_sde(Some(Arc::clone(&db)), Some(Arc::clone(&sde)))
    };
    assert_eq!(mgr_reloaded.category_score(cid, 2), 120);
    assert!(mgr_reloaded
        .get_or_init_achievements(cid)
        .iter()
        .any(|a| a.def.id == sample_id && a.completed));

    // Also exercise `AchievementManager::default()`
    let def_mgr = AchievementManager::default();
    assert!(!def_mgr.sde_achievement_hash.is_empty());

    // Custom manager testing:
    // - `mark_completed` with `milestone_targets.is_empty()` and `reward_isk > 0` adding milestone 1 once
    // - `claim_rewards_filtered` by achievement_id, category_id, milestone_id, selected_reward_uuids,
    //   and `reached_milestones.is_empty()` fallback `(1..=unclaimed)`
    // - Live wallet ISK + SP + `"AIRAchievementReward"` journal entry on demo character `90_000_001`!
    let mut custom_mgr = AchievementManager::new_with_sde(Some(Arc::clone(&db)), None);
    custom_mgr.definitions = vec![
        AchievementDef {
            id: 301,
            is_client_achievement: true,
            category_id: 1,
            name: "No Milestones Rewarded".into(),
            description: String::new(),
            notification_text: String::new(),
            target: 2,
            score: 25,
            milestone_targets: Vec::new(),
            conditions: vec![AchievementConditionDef {
                condition_id: 11,
                target_value: 2,
                statistic_event_id: 901,
                event_name: "client_cam".into(),
            }],
            reward_isk: 150_000,
            reward_sp: 3_000,
        },
        AchievementDef {
            id: 302,
            is_client_achievement: false,
            category_id: 3,
            name: "Two Milestones".into(),
            description: String::new(),
            notification_text: String::new(),
            target: 10,
            score: 40,
            milestone_targets: vec![5, 10],
            conditions: Vec::new(),
            reward_isk: 200_000,
            reward_sp: 1_500,
        },
    ];

    custom_mgr.character_achievements.write().unwrap().clear();

    // Unknown achievement errors
    assert!(custom_mgr.add_progress(cid, 999_999, 1, 100).is_none());
    assert_eq!(custom_mgr.mark_completed(cid, 999_999, 100).unwrap_err().0, STATUS_NOT_FOUND);
    assert_eq!(custom_mgr.reset_achievement(cid, 999_999).unwrap_err().0, STATUS_NOT_FOUND);

    // Uninitialized character claim returns 0
    assert_eq!(custom_mgr.claim_rewards(88_888_888, &db), 0);

    // mark_completed on empty milestone_targets with reward_isk > 0 adds milestone 1 exactly once
    let mc1 = custom_mgr.mark_completed(cid, 301, 2_000).unwrap();
    let mc2 = custom_mgr.mark_completed(cid, 301, 2_001).unwrap();
    assert_eq!(mc1.reached_milestones, vec![1]);
    assert_eq!(mc2.unclaimed_reward_count, 1);

    // Advance 302 to both milestones, then clear `reached_milestones` while keeping `unclaimed_reward_count = 2`
    // to cover the `(1..=a.unclaimed_reward_count)` fallback branch in `claim_rewards_filtered`!
    custom_mgr.add_progress(cid, 302, 10, 2_002).unwrap();
    {
        let mut map = custom_mgr.character_achievements.write().unwrap();
        let a302 = map.get_mut(&cid).unwrap().iter_mut().find(|a| a.def.id == 302).unwrap();
        a302.reached_milestones.clear();
        assert_eq!(a302.unclaimed_reward_count, 2);
    }

    let (bal_before, sp_before, j_before) = db.with_snapshot(|s| {
        let ch = &s.characters[&(cid as i64)];
        (ch.balance, ch.free_skill_points, s.wallet_journal.len())
    });

    // Non-matching filters claim 0
    assert_eq!(
        custom_mgr.claim_rewards_filtered(
            cid,
            &db,
            &RewardClaimFilter {
                category_id: Some(5),
                ..Default::default()
            }
        ),
        0
    );

    // Claim only milestone 2 of achievement 302 by UUID
    let uuid_302_m2 = achievement_reward_uuid(302, 2);
    let claimed_one = custom_mgr.claim_rewards_filtered(
        cid,
        &db,
        &RewardClaimFilter {
            achievement_id: Some(302),
            milestone_id: Some(2),
            selected_reward_uuids: Some(vec![uuid_302_m2]),
            ..Default::default()
        },
    );
    assert_eq!(claimed_one, 1);

    // Claim remaining rewards across all categories
    let claimed_rest = custom_mgr.claim_rewards(cid, &db);
    assert_eq!(claimed_rest, 2); // 1 from 301 + 1 remaining from 302

    db.with_snapshot(|s| {
        let ch = &s.characters[&(cid as i64)];
        // Total ISK = 200_000 (first claim) + 150_000 + 200_000 (second claim) = 550_000 ISK
        assert_eq!(ch.balance, IskCents(bal_before.0 + IskCents::from_isk(550_000).0));
        assert!((ch.free_skill_points - (sp_before + 6_000.0)).abs() < 1e-6);
        assert_eq!(s.wallet_journal.len(), j_before + 2);
        assert!(s.wallet_journal.iter().any(|j| j.entry_type == "AIRAchievementReward"));
    });

    // Zero-delta guards on gameplay & client counter events
    let router = PublicGatewayRouter::new(
        Arc::new(QuasarDescriptorRegistry::empty_with_builtins()),
        Arc::clone(&db),
        Arc::clone(&sde),
    );
    custom_mgr.on_gameplay_event_by_name(cid, &[], 5, 3_000, &router);
    custom_mgr.on_gameplay_event_by_name(cid, &["client_cam"], 0, 3_000, &router);
    custom_mgr.on_client_counter_event(cid, "901", 0, 3_000, &router);
    custom_mgr.on_client_counter_event(cid, "901", 2, 3_000, &router);
}

#[test]
fn flatbuffer_disk_storage_seeding_ignores_host_appdata_and_uses_stage_dir() {
    let tmp_base = std::env::temp_dir().join(format!("evomu_ach_fb_test_{}", std::process::id()));
    let host_storage_dir = tmp_base
        .join("CCP")
        .join("EVE")
        .join("tq_profile")
        .join("cache")
        .join("Storage");
    let stage_dir = tmp_base.join("isolated_stage");
    let _ = std::fs::remove_dir_all(&tmp_base);
    std::fs::create_dir_all(&host_storage_dir).expect("create temp host Storage dir");

    struct EnvGuard {
        prev_local_appdata: Option<std::ffi::OsString>,
        prev_stage_dir: Option<std::ffi::OsString>,
        _lock: crate::TestEnvWriteGuard,
    }
    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match self.prev_local_appdata.take() {
                Some(v) => std::env::set_var("LOCALAPPDATA", v),
                None => std::env::remove_var("LOCALAPPDATA"),
            }
            match self.prev_stage_dir.take() {
                Some(v) => std::env::set_var("EVOMU_FLATBUFFER_STAGE_DIR", v),
                None => std::env::remove_var("EVOMU_FLATBUFFER_STAGE_DIR"),
            }
        }
    }
    let _lock = crate::lock_test_env_write();
    let _env_guard = EnvGuard {
        prev_local_appdata: std::env::var_os("LOCALAPPDATA"),
        prev_stage_dir: std::env::var_os("EVOMU_FLATBUFFER_STAGE_DIR"),
        _lock,
    };
    std::env::set_var("LOCALAPPDATA", &tmp_base);
    std::env::remove_var("EVOMU_FLATBUFFER_STAGE_DIR");

    let payload = empty_achievement_flatbuffer();
    // Without EVOMU_FLATBUFFER_STAGE_DIR, host %LOCALAPPDATA%\CCP\EVE must NEVER be mutated.
    ensure_client_flatbuffer_storage_seeded("test_ach_content", "v1_hash", &payload);
    assert!(
        !host_storage_dir.join("test_ach_content").exists(),
        "eve-server must not mutate host CCP/EVE cache directories"
    );

    // With EVOMU_FLATBUFFER_STAGE_DIR explicitly set, write into the isolated staging dir only.
    std::env::set_var("EVOMU_FLATBUFFER_STAGE_DIR", &stage_dir);
    ensure_client_flatbuffer_storage_seeded("test_ach_content", "v1_hash", &payload);
    assert!(
        !host_storage_dir.join("test_ach_content").exists(),
        "host CCP/EVE cache directory must stay untouched even when staging is enabled"
    );

    let data_path = stage_dir.join("test_ach_content").join("data.bin");
    let ver_path = stage_dir.join("test_ach_content").join("version.txt");
    assert_eq!(std::fs::read(&data_path).unwrap(), payload);
    assert_eq!(std::fs::read_to_string(&ver_path).unwrap(), "v1_hash");

    // Second call with non-empty data.bin must NOT overwrite existing file!
    ensure_client_flatbuffer_storage_seeded("test_ach_content", "v2_should_not_overwrite", b"DIFFERENT");
    assert_eq!(std::fs::read(&data_path).unwrap(), payload);
    assert_eq!(std::fs::read_to_string(&ver_path).unwrap(), "v1_hash");

    drop(_env_guard);
    let _ = std::fs::remove_dir_all(&tmp_base);
}

#[test]
fn save_to_db_and_rehydrate_refreshes_sde_definitions_kill_mutants() {
    let db = Arc::new(WorldRepository::with_demo_seed());
    let cid = 90_000_001_u32;

    let mgr = {
        let _env_read = crate::lock_test_env_read();
        let sde = shared_sde();
        AchievementManager::new_with_sde(Some(Arc::clone(&db)), Some(Arc::clone(&sde)))
    };
    let sample_def = mgr
        .definitions
        .first()
        .expect("SDE achievement definitions must be non-empty")
        .clone();

    // 1. save_to_db writes both `quasar_achievements` and `quasar_achievement_cat_scores` blobs:
    //    kills `AchievementManager::save_to_db -> ()`.
    mgr.mark_completed(cid, sample_def.id, 1_776_000_100)
        .expect("mark_completed succeeds");
    db.with_mut_snapshot(|s| {
        s.quasar_state_blobs.remove("quasar_achievements");
        s.quasar_state_blobs.remove("quasar_achievement_cat_scores");
    });
    mgr.save_to_db();
    db.with_snapshot(|s| {
        let ach_json = s
            .quasar_state_blobs
            .get("quasar_achievements")
            .expect("save_to_db must write quasar_achievements blob");
        let parsed: HashMap<u32, Vec<CharacterAchievement>> =
            serde_json::from_str(ach_json).unwrap();
        assert!(parsed.get(&cid).is_some_and(|v| v.iter().any(|a| a.def.id == sample_def.id && a.completed)));
        assert!(s.quasar_state_blobs.contains_key("quasar_achievement_cat_scores"));
    });

    // 2. rehydrate_from_db reloads DB progress & scores AND refreshes `a.def` from `self.definitions`:
    //    kills `AchievementManager::rehydrate_from_db -> ()` AND `delete !` on `!defs_by_id.is_empty()`.
    let mut stale_def = sample_def.clone();
    stale_def.name = "STALE_OUTDATED_DEF_NAME".into();
    stale_def.reward_isk = 0;
    let stale_entry = CharacterAchievement {
        def: stale_def,
        progress: sample_def.target,
        condition_progress: HashMap::new(),
        completed: true,
        completed_date: Some(1_776_000_200),
        reached_milestones: Vec::new(),
        unclaimed_reward_count: 0,
    };
    db.with_mut_snapshot(|s| {
        s.quasar_state_blobs.insert(
            "quasar_achievements".into(),
            serde_json::to_string(&HashMap::from([(cid, vec![stale_entry])])).unwrap(),
        );
        s.quasar_state_blobs.insert(
            "quasar_achievement_cat_scores".into(),
            serde_json::to_string(&HashMap::from([(format!("{cid}:{}", sample_def.category_id), 999_u32)])).unwrap(),
        );
    });
    mgr.character_achievements.write().unwrap().clear();
    mgr.category_scores.write().unwrap().clear();

    mgr.rehydrate_from_db();
    let list = mgr.get_or_init_achievements(cid);
    let refreshed = list
        .iter()
        .find(|a| a.def.id == sample_def.id)
        .expect("rehydrated achievement must be present");
    assert_eq!(refreshed.completed_date, Some(1_776_000_200));
    assert_eq!(refreshed.def.name, sample_def.name);
    assert_ne!(refreshed.def.name, "STALE_OUTDATED_DEF_NAME");
    assert_eq!(refreshed.def.reward_isk, sample_def.reward_isk);
    assert_eq!(
        mgr.category_scores.read().unwrap().get(&(cid, sample_def.category_id)).copied(),
        Some(999)
    );
}

#[derive(Clone, Copy)]
struct FbTableView<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> FbTableView<'a> {
    fn u16_at(&self, off: usize) -> u16 {
        u16::from_le_bytes([self.buf[off], self.buf[off + 1]])
    }
    fn u32_at(&self, off: usize) -> u32 {
        u32::from_le_bytes([self.buf[off], self.buf[off + 1], self.buf[off + 2], self.buf[off + 3]])
    }
    fn i32_at(&self, off: usize) -> i32 {
        i32::from_le_bytes([self.buf[off], self.buf[off + 1], self.buf[off + 2], self.buf[off + 3]])
    }
    fn u64_at(&self, off: usize) -> u64 {
        u64::from_le_bytes([
            self.buf[off],
            self.buf[off + 1],
            self.buf[off + 2],
            self.buf[off + 3],
            self.buf[off + 4],
            self.buf[off + 5],
            self.buf[off + 6],
            self.buf[off + 7],
        ])
    }
    fn root(buf: &'a [u8]) -> Self {
        let root_off = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
        Self { buf, pos: root_off }
    }
    fn field_offset(&self, vtable_byte_off: usize) -> Option<usize> {
        let vt_rel = self.i32_at(self.pos);
        let vt_pos = (self.pos as isize - vt_rel as isize) as usize;
        let vt_len = self.u16_at(vt_pos) as usize;
        if vtable_byte_off >= vt_len {
            return None;
        }
        let f_off = self.u16_at(vt_pos + vtable_byte_off) as usize;
        (f_off != 0).then_some(self.pos + f_off)
    }
    fn get_u8(&self, voff: usize, default: u8) -> u8 {
        self.field_offset(voff).map_or(default, |p| self.buf[p])
    }
    fn get_u32(&self, voff: usize, default: u32) -> u32 {
        self.field_offset(voff).map_or(default, |p| self.u32_at(p))
    }
    fn get_u64(&self, voff: usize, default: u64) -> u64 {
        self.field_offset(voff).map_or(default, |p| self.u64_at(p))
    }
    fn get_str(&self, voff: usize) -> Option<&'a str> {
        let p = self.field_offset(voff)?;
        let str_pos = p + self.u32_at(p) as usize;
        let len = self.u32_at(str_pos) as usize;
        std::str::from_utf8(&self.buf[str_pos + 4..str_pos + 4 + len]).ok()
    }
    fn get_table(&self, voff: usize) -> Option<Self> {
        let p = self.field_offset(voff)?;
        let target = p + self.u32_at(p) as usize;
        Some(Self {
            buf: self.buf,
            pos: target,
        })
    }
    fn get_table_vec(&self, voff: usize) -> Vec<Self> {
        let Some(p) = self.field_offset(voff) else {
            return Vec::new();
        };
        let vec_pos = p + self.u32_at(p) as usize;
        let len = self.u32_at(vec_pos) as usize;
        (0..len)
            .map(|i| {
                let elem_off_pos = vec_pos + 4 + i * 4;
                let elem_pos = elem_off_pos + self.u32_at(elem_off_pos) as usize;
                Self {
                    buf: self.buf,
                    pos: elem_pos,
                }
            })
            .collect()
    }
}

#[test]
fn sde_achievement_flatbuffer_encodes_all_roots_categories_methods_and_definitions() {
    let (sde_achs, mgr) = {
        let _env_read = crate::lock_test_env_read();
        let sde = shared_sde();
        let sde_achs = sde.load_achievements().expect("SDE achievements load");
        let mgr = AchievementManager::new_with_sde(None, Some(Arc::clone(&sde)));
        (sde_achs, mgr)
    };
    let fb = mgr.flatbuffer_bytes();

    assert_eq!(fb.len() % 4, 0, "FlatBuffer must preserve 4-byte alignment");
    assert_eq!(&fb[4..8], b"ACHV", "FlatBuffer file identifier must be ACHV");

    let root = FbTableView::root(&fb);
    let career_program = root.get_table(4).expect("CareerProgram root");
    let challenges = root.get_table(6).expect("Challenges root");
    let time_limited = root.get_table(8).expect("TimeLimited root");
    let methods = root.get_table_vec(10);
    let manual = root.get_table(12).expect("Manual root");

    let root_ids: std::collections::HashSet<&str> = [
        career_program.get_str(4).unwrap(),
        challenges.get_str(4).unwrap(),
        time_limited.get_str(4).unwrap(),
        manual.get_str(4).unwrap(),
    ]
    .into_iter()
    .collect();
    assert_eq!(root_ids.len(), 4, "all 4 root categories need distinct UUIDs");

    assert_eq!(methods.len(), 1);
    let method_id = methods[0].get_str(4).expect("ContributionMethod Id");
    assert_eq!(methods[0].get_u8(18, 0), 1, "ProgressType must be CumulativeMode (1)");
    let cum_mode = methods[0].get_table(20).expect("CumulativeMode Progress table");
    assert_eq!(cum_mode.get_u32(4, 1), 1, "ValueScale must be 1");

    let child_cats = challenges.get_table_vec(14);
    assert_eq!(child_cats.len(), 5, "categories 1..=5 present under Challenges");

    let mut encoded_achs = HashMap::new();
    for ach in challenges.get_table_vec(16) {
        let aid_str = ach.get_str(4).expect("Achievement Id").to_string();
        encoded_achs.insert(aid_str, ach);
    }
    for cat in &child_cats {
        let cat_id_str = cat.get_str(4).expect("Category Id");
        assert!(cat_id_str.starts_with("00000000-0000-0000-0000-"));
        for ach in cat.get_table_vec(16) {
            let aid_str = ach.get_str(4).expect("Achievement Id").to_string();
            encoded_achs.insert(aid_str, ach);
        }
    }

    assert_eq!(encoded_achs.len(), mgr.definitions.len());
    assert!(!encoded_achs.is_empty());

    for def in &mgr.definitions {
        let expected_uuid = crate::services::quasar_achievements_flatbuffer::uuid_bytes_to_hyphenated_string(
            u32_to_uuid(def.id),
        );
        let ach = encoded_achs
            .get(&expected_uuid)
            .unwrap_or_else(|| panic!("missing achievement UUID {expected_uuid} (id={})", def.id));
        let info = &sde_achs[&def.id];
        assert_eq!(ach.get_u32(6, 0), info.name_id);
        assert_eq!(ach.get_u32(8, 0), info.description_id);
        let contrib = ach.get_table(16).expect("Achievement ContributionConfig required");
        assert_eq!(contrib.get_str(4), Some(method_id));
        assert_eq!(ach.get_str(30), Some(def.name.as_str()));
        assert_eq!(ach.get_u8(32, 255), 0, "Visibility must be AlwaysVisible (0)");

        let ms = ach.get_table_vec(18);
        assert_eq!(ms.len(), def.milestone_targets.len());
        for (idx, (m, &target)) in ms.iter().zip(def.milestone_targets.iter()).enumerate() {
            let m_uuid = crate::services::quasar_achievements_flatbuffer::uuid_bytes_to_hyphenated_string(
                u32_to_uuid((idx as u32) + 1),
            );
            assert_eq!(m.get_str(4), Some(m_uuid.as_str()));
            assert_eq!(m.get_u64(6, 0), target);
            assert!(m.get_u32(8, 0) > 0);
        }
    }

    // Custom non-SDE manager with category_id=0 and category_id=3 also encodes cleanly.
    let mut custom = AchievementManager::new_with_sde(None, None);
    custom.definitions = vec![
        AchievementDef {
            id: 77,
            is_client_achievement: false,
            category_id: 0,
            name: "Root Ach".into(),
            description: String::new(),
            notification_text: String::new(),
            target: 4,
            score: 20,
            milestone_targets: vec![2, 4],
            conditions: Vec::new(),
            reward_isk: 0,
            reward_sp: 0,
        },
        AchievementDef {
            id: 88,
            is_client_achievement: true,
            category_id: 3,
            name: "Cat 3 Ach".into(),
            description: String::new(),
            notification_text: String::new(),
            target: 1,
            score: 10,
            milestone_targets: Vec::new(),
            conditions: Vec::new(),
            reward_isk: 0,
            reward_sp: 0,
        },
    ];
    let custom_fb = custom.flatbuffer_bytes();
    let custom_root = FbTableView::root(&custom_fb);
    let custom_ch = custom_root.get_table(6).unwrap();
    assert_eq!(custom_ch.get_table_vec(16).len(), 1);
    assert_eq!(
        custom_ch.get_table_vec(16)[0].get_str(4),
        Some("00000000-0000-0000-0000-00000000004d")
    );
    let custom_cat3 = &custom_ch.get_table_vec(14)[2];
    assert_eq!(custom_cat3.get_table_vec(16).len(), 1);
    assert_eq!(
        custom_cat3.get_table_vec(16)[0].get_str(4),
        Some("00000000-0000-0000-0000-000000000058")
    );
}

#[test]
fn get_latest_request_upgrades_stale_legacy_140_byte_achievements_flatbuffer() {
    let tmp_base = std::env::temp_dir().join(format!("evomu_ach_upgrade_test_{}", std::process::id()));
    let host_storage_dir = tmp_base
        .join("CCP")
        .join("EVE")
        .join("tq_profile")
        .join("cache")
        .join("Storage");
    let stage_dir = tmp_base.join("isolated_stage");
    let _ = std::fs::remove_dir_all(&tmp_base);
    std::fs::create_dir_all(host_storage_dir.join("achievements")).expect("create host achievements dir");
    std::fs::create_dir_all(stage_dir.join("achievements")).expect("create stage achievements dir");

    let host_data_path = host_storage_dir.join("achievements").join("data.bin");
    let host_ver_path = host_storage_dir.join("achievements").join("version.txt");
    let data_path = stage_dir.join("achievements").join("data.bin");
    let ver_path = stage_dir.join("achievements").join("version.txt");
    let legacy = empty_achievement_flatbuffer();
    assert_eq!(legacy.len(), 140);
    std::fs::write(&host_data_path, &legacy).unwrap();
    std::fs::write(&host_ver_path, "24.1.60").unwrap();
    std::fs::write(&data_path, &legacy).unwrap();
    std::fs::write(&ver_path, "24.1.60").unwrap();

    struct EnvGuard {
        prev_home: Option<std::ffi::OsString>,
        prev_local: Option<std::ffi::OsString>,
        prev_cdn: Option<std::ffi::OsString>,
        prev_stage: Option<std::ffi::OsString>,
        _lock: crate::TestEnvWriteGuard,
    }
    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match self.prev_home.take() {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
            match self.prev_local.take() {
                Some(v) => std::env::set_var("LOCALAPPDATA", v),
                None => std::env::remove_var("LOCALAPPDATA"),
            }
            match self.prev_cdn.take() {
                Some(v) => std::env::set_var("EVOMU_CDN_BASE_URL", v),
                None => std::env::remove_var("EVOMU_CDN_BASE_URL"),
            }
            match self.prev_stage.take() {
                Some(v) => std::env::set_var("EVOMU_FLATBUFFER_STAGE_DIR", v),
                None => std::env::remove_var("EVOMU_FLATBUFFER_STAGE_DIR"),
            }
        }
    }
    // Initialize shared SDE and load achievements under read lock BEFORE clearing HOME / mutating
    // LOCALAPPDATA so `eve_sde::resolve_resfiles_base_dir` discovers the real SharedCache cleanly.
    let db = Arc::new(WorldRepository::with_demo_seed());
    let sde = shared_sde();
    let mgr = {
        let _env_read = crate::lock_test_env_read();
        Arc::new(AchievementManager::new_with_sde(
            Some(Arc::clone(&db)),
            Some(Arc::clone(&sde)),
        ))
    };

    let _guard = EnvGuard {
        prev_home: std::env::var_os("HOME"),
        prev_local: std::env::var_os("LOCALAPPDATA"),
        prev_cdn: std::env::var_os("EVOMU_CDN_BASE_URL"),
        prev_stage: std::env::var_os("EVOMU_FLATBUFFER_STAGE_DIR"),
        _lock: crate::lock_test_env_write(),
    };
    std::env::remove_var("HOME");
    std::env::remove_var("EVOMU_CDN_BASE_URL");
    std::env::set_var("LOCALAPPDATA", &tmp_base);
    std::env::set_var("EVOMU_FLATBUFFER_STAGE_DIR", &stage_dir);

    let expected_fb = mgr.flatbuffer_bytes();
    assert!(expected_fb.len() > legacy.len());

    let router = PublicGatewayRouter::new(
        Arc::new(QuasarDescriptorRegistry::empty_with_builtins()),
        Arc::clone(&db),
        Arc::clone(&sde),
    );
    register_achievement_handlers(&router, Arc::clone(&db), Arc::clone(&mgr));
    assert_eq!(std::fs::read(&data_path).unwrap(), expected_fb);
    assert_eq!(
        std::fs::read_to_string(&ver_path).unwrap(),
        format!("24.1.{}", mgr.sde_achievement_count)
    );

    let ctx = eve_proto::quasar::AuthoritativeContext::for_session(
        Some(1),
        Some(90_000_001),
        None,
        0,
        "tranquility",
        Vec::new(),
    );
    let any = eve_proto::quasar::GoogleAny {
        type_url: "type.evetech.net/eve_public.achievement.definition.api.cdn.GetLatestRequest"
            .to_string(),
        value: Vec::new(),
    };
    let handlers = router.handlers.read().unwrap();
    let resp = handlers["eve_public.achievement.definition.api.cdn.GetLatestRequest"](
        &ctx,
        &any,
        &DynamicQuasarMessage::new("GetLatestRequest"),
    )
    .expect("GetLatestRequest succeeds");
    assert_eq!(
        resp.type_url,
        "type.googleapis.com/eve_public.achievement.definition.api.cdn.GetLatestResponse"
    );
    assert!(!resp.value.is_empty());

    // Host %LOCALAPPDATA%\CCP\EVE directory must remain completely untouched.
    assert_eq!(std::fs::read(&host_data_path).unwrap(), legacy);
    assert_eq!(std::fs::read_to_string(&host_ver_path).unwrap(), "24.1.60");

    drop(_guard);
    let _ = std::fs::remove_dir_all(&tmp_base);
}
