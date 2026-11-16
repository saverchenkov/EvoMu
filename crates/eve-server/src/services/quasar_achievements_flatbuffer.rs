//! Binary FlatBuffer serializer for V24.02 `AchievementData` (`"ACHV"`).
//!
//! Matches `achievement_system/client/model/flatbuffer/generated.pyj` and
//! `achievement_system/client/repository/flatbuffer/repository.pyj` in V24.02 (`build 3698585`).

#![forbid(unsafe_code)]

use super::quasar_achievements::{u32_to_uuid, AchievementDef};
use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap};

const ACHIEVEMENT_FILE_IDENTIFIER: [u8; 4] = *b"ACHV";
const PROGRESS_CONFIG_CUMULATIVE_MODE: u8 = 1;
const ACHIEVEMENT_VISIBILITY_ALWAYS_VISIBLE: u8 = 0;

/// Root category UUIDs (bytes[7] = 1 so they never collide with `u32_to_uuid(id)`).
const ROOT_CAREER_PROGRAM_UUID: [u8; 16] = [0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 1];
const ROOT_CHALLENGES_UUID: [u8; 16] = [0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 2];
const ROOT_TIME_LIMITED_UUID: [u8; 16] = [0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 3];
const ROOT_MANUAL_UUID: [u8; 16] = [0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 4];
const CUMULATIVE_METHOD_UUID: [u8; 16] = [0, 0, 0, 0, 0, 0, 0, 2, 0, 0, 0, 0, 0, 0, 0, 1];

pub(crate) struct TableState {
    field_offsets: Vec<u32>,
    table_end_offset: u32,
}

/// Safe, zero-dependency, backward-growing FlatBuffer binary encoder with vtable deduplication.
pub(crate) struct FlatBufferWriter {
    buf: Vec<u8>,
    head: usize,
    minalign: usize,
    vtables: HashMap<Vec<u8>, u32>,
}

impl FlatBufferWriter {
    pub(crate) fn with_capacity(cap: usize) -> Self {
        let cap = cap.max(256);
        Self {
            buf: vec![0u8; cap],
            head: cap,
            minalign: 1,
            vtables: HashMap::new(),
        }
    }

    fn offset(&self) -> u32 {
        (self.buf.len() - self.head) as u32
    }

    fn ensure_space(&mut self, needed: usize) {
        if self.head >= needed {
            return;
        }
        let old_cap = self.buf.len();
        let written = old_cap - self.head;
        let new_cap = (old_cap * 2).max(written + needed + 256);
        let mut new_buf = vec![0u8; new_cap];
        let new_head = new_cap - written;
        new_buf[new_head..].copy_from_slice(&self.buf[self.head..]);
        self.buf = new_buf;
        self.head = new_head;
    }

    fn push_bytes(&mut self, bytes: &[u8]) {
        self.ensure_space(bytes.len());
        self.head -= bytes.len();
        self.buf[self.head..self.head + bytes.len()].copy_from_slice(bytes);
    }

    fn push_pad(&mut self, count: usize) {
        if count == 0 {
            return;
        }
        self.ensure_space(count);
        self.head -= count;
        self.buf[self.head..self.head + count].fill(0);
    }

    fn prep(&mut self, size: usize, additional_bytes: usize) {
        debug_assert!(size.is_power_of_two());
        if size > self.minalign {
            self.minalign = size;
        }
        let cur = (self.offset() as usize) + additional_bytes;
        let align_pad = ((!cur).wrapping_add(1)) & (size - 1);
        self.push_pad(align_pad);
    }

    fn push_u8(&mut self, v: u8) {
        self.push_bytes(&[v]);
    }

    fn push_u32(&mut self, v: u32) {
        self.push_bytes(&v.to_le_bytes());
    }

    fn push_u64(&mut self, v: u64) {
        self.push_bytes(&v.to_le_bytes());
    }

    pub(crate) fn create_string(&mut self, s: &str) -> u32 {
        let bytes = s.as_bytes();
        self.prep(4, bytes.len() + 1);
        self.push_u8(0);
        self.push_bytes(bytes);
        self.push_u32(bytes.len() as u32);
        self.offset()
    }

    pub(crate) fn create_offset_vector(&mut self, offsets: &[u32]) -> u32 {
        self.prep(4, 4 + offsets.len() * 4);
        for &target in offsets.iter().rev() {
            let rel = self.offset() + 4 - target;
            self.push_u32(rel);
        }
        self.push_u32(offsets.len() as u32);
        self.offset()
    }

    pub(crate) fn start_table(&self, num_slots: usize) -> TableState {
        TableState {
            field_offsets: vec![0; num_slots],
            table_end_offset: self.offset(),
        }
    }

    pub(crate) fn add_u8(&mut self, tbl: &mut TableState, slot: usize, val: u8, default: u8) {
        if val != default {
            self.prep(1, 0);
            self.push_u8(val);
            tbl.field_offsets[slot] = self.offset();
        }
    }

    fn add_u32(&mut self, tbl: &mut TableState, slot: usize, val: u32, default: u32) {
        if val != default {
            self.prep(4, 0);
            self.push_u32(val);
            tbl.field_offsets[slot] = self.offset();
        }
    }

    pub(crate) fn add_u64(&mut self, tbl: &mut TableState, slot: usize, val: u64, default: u64) {
        if val != default {
            self.prep(8, 0);
            self.push_u64(val);
            tbl.field_offsets[slot] = self.offset();
        }
    }

    pub(crate) fn add_offset(&mut self, tbl: &mut TableState, slot: usize, target_off: u32) {
        if target_off != 0 {
            self.prep(4, 0);
            let rel = self.offset() + 4 - target_off;
            self.push_u32(rel);
            tbl.field_offsets[slot] = self.offset();
        }
    }

    pub(crate) fn end_table(&mut self, tbl: TableState) -> u32 {
        self.prep(4, 4);
        self.push_u32(0); // placeholder for vtable soffset
        let table_start = self.offset();
        let table_len = (table_start - tbl.table_end_offset) as u16;

        let active_slots = tbl
            .field_offsets
            .iter()
            .rposition(|&off| off != 0)
            .map_or(0, |idx| idx + 1);
        let vtable_len = ((2 + active_slots) * 2) as u16;

        let mut vtable_bytes = Vec::with_capacity(vtable_len as usize);
        vtable_bytes.extend_from_slice(&vtable_len.to_le_bytes());
        vtable_bytes.extend_from_slice(&table_len.to_le_bytes());
        for &field_off in tbl.field_offsets.iter().take(active_slots) {
            let slot_off = if field_off != 0 {
                (table_start - field_off) as u16
            } else {
                0u16
            };
            vtable_bytes.extend_from_slice(&slot_off.to_le_bytes());
        }

        let vtable_offset = if let Some(&existing_off) = self.vtables.get(&vtable_bytes) {
            existing_off
        } else {
            self.prep(2, vtable_bytes.len());
            self.push_bytes(&vtable_bytes);
            let new_off = self.offset();
            self.vtables.insert(vtable_bytes, new_off);
            new_off
        };

        let table_pos = self.buf.len() - (table_start as usize);
        let soffset = (vtable_offset as i32) - (table_start as i32);
        self.buf[table_pos..table_pos + 4].copy_from_slice(&soffset.to_le_bytes());
        table_start
    }

    pub(crate) fn finish(self, root_table: u32, file_identifier: [u8; 4]) -> Vec<u8> {
        self.finish_inner(root_table, Some(file_identifier))
    }

    /// Finalize a FlatBuffer whose root schema defines no 4-byte file identifier
    /// (for example `maptags.generated.eve_public.map.Universe`).
    pub(crate) fn finish_untagged(self, root_table: u32) -> Vec<u8> {
        self.finish_inner(root_table, None)
    }

    fn finish_inner(mut self, root_table: u32, file_identifier: Option<[u8; 4]>) -> Vec<u8> {
        let minalign = self.minalign.max(4);
        let header_len = if file_identifier.is_some() { 8 } else { 4 };
        self.prep(minalign, header_len);
        if let Some(file_id) = file_identifier {
            self.push_bytes(&file_id);
        }
        let root_rel = self.offset() + 4 - root_table;
        self.push_u32(root_rel);
        self.buf[self.head..].to_vec()
    }
}

pub(crate) fn uuid_bytes_to_hyphenated_string(bytes: [u8; 16]) -> String {
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0], bytes[1], bytes[2], bytes[3],
        bytes[4], bytes[5],
        bytes[6], bytes[7],
        bytes[8], bytes[9],
        bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15],
    )
}

fn category_internal_name(category_id: u32) -> Cow<'static, str> {
    match category_id {
        1 => Cow::Borrowed("combat"),
        2 => Cow::Borrowed("exploration"),
        3 => Cow::Borrowed("industry"),
        4 => Cow::Borrowed("economy"),
        5 => Cow::Borrowed("social"),
        other => Cow::Owned(format!("category_{other}")),
    }
}

fn build_category_table(
    fb: &mut FlatBufferWriter,
    id_uuid: [u8; 16],
    internal_name: &str,
    child_categories: &[u32],
    child_achievements: &[u32],
) -> u32 {
    let id_off = fb.create_string(&uuid_bytes_to_hyphenated_string(id_uuid));
    let name_off = fb.create_string(internal_name);
    let cats_vec_off = if child_categories.is_empty() {
        0
    } else {
        fb.create_offset_vector(child_categories)
    };
    let achs_vec_off = if child_achievements.is_empty() {
        0
    } else {
        fb.create_offset_vector(child_achievements)
    };

    // Category has 12 slots:
    // 0: Id, 1: Name, 2: Description, 3: IconId, 4: Milestones,
    // 5: Categories, 6: Achievements, 7: InternalName, 8: UiAnnotations,
    // 9: AuraGuidance, 10: Presentation, 11: ContributesToTotalScore (default true)
    let mut tbl = fb.start_table(12);
    fb.add_offset(&mut tbl, 0, id_off);
    fb.add_offset(&mut tbl, 5, cats_vec_off);
    fb.add_offset(&mut tbl, 6, achs_vec_off);
    fb.add_offset(&mut tbl, 7, name_off);
    fb.end_table(tbl)
}

fn build_achievement_table(
    fb: &mut FlatBufferWriter,
    def: &AchievementDef,
    name_id: u32,
    description_id: u32,
    shared_contribution_off: u32,
) -> u32 {
    let id_off = fb.create_string(&uuid_bytes_to_hyphenated_string(u32_to_uuid(def.id)));
    let internal_name_off = if def.name.is_empty() {
        0
    } else {
        fb.create_string(&def.name)
    };

    let milestone_offs: Vec<u32> = def
        .milestone_targets
        .iter()
        .enumerate()
        .map(|(idx, &target)| {
            let m_id = (idx as u32) + 1;
            let m_id_off = fb.create_string(&uuid_bytes_to_hyphenated_string(u32_to_uuid(m_id)));
            let total_milestones = (def.milestone_targets.len() as u32).max(1);
            let category_points = (def.score / total_milestones).max(1);
            // AchievementMilestone (6 slots):
            // 0: Id, 1: Target, 2: CategoryPoints, 3: Rewards, 4: InternalName, 5: Presentation
            let mut m_tbl = fb.start_table(6);
            fb.add_offset(&mut m_tbl, 0, m_id_off);
            fb.add_u64(&mut m_tbl, 1, target, 0);
            fb.add_u32(&mut m_tbl, 2, category_points, 0);
            fb.end_table(m_tbl)
        })
        .collect();
    let milestones_vec_off = if milestone_offs.is_empty() {
        0
    } else {
        fb.create_offset_vector(&milestone_offs)
    };

    // Achievement has 19 slots:
    // 0: Id, 1: Name, 2: Description, 3: FlavorText, 4: IconId, 5: IssuerNpcCorp,
    // 6: Contribution (required), 7: Milestones, 8: ReleaseDateUnixMs, 9: ExpirationDateUnixMs,
    // 10: UiAnnotations, 11: Tags, 12: Archived (default false), 13: InternalName,
    // 14: Visibility (0 = AlwaysVisible), 15: EstimatedDurationInMinutes, 16: ThreatLevel,
    // 17: AuraGuidance, 18: Presentation
    let mut tbl = fb.start_table(19);
    fb.add_offset(&mut tbl, 0, id_off);
    fb.add_u32(&mut tbl, 1, name_id, 0);
    fb.add_u32(&mut tbl, 2, description_id, 0);
    fb.add_offset(&mut tbl, 6, shared_contribution_off);
    fb.add_offset(&mut tbl, 7, milestones_vec_off);
    fb.add_offset(&mut tbl, 13, internal_name_off);
    fb.add_u8(&mut tbl, 14, ACHIEVEMENT_VISIBILITY_ALWAYS_VISIBLE, u8::MAX);
    fb.end_table(tbl)
}

/// Encodes all provided `AchievementDef` entries (plus categories `1..=5` and a cumulative
/// contribution method) into a V24.02-compatible `AchievementData` FlatBuffer (`"ACHV"`).
pub(crate) fn encode_achievement_data_flatbuffer(
    defs: &[AchievementDef],
    localization_ids: &HashMap<u32, (u32, u32)>,
) -> Vec<u8> {
    let mut fb = FlatBufferWriter::with_capacity(defs.len().saturating_mul(192) + 2048);

    // 1. CumulativeMode (1 slot: 0: ValueScale default 1) + ContributionMethodDefinition (11 slots)
    let mut cum_tbl = fb.start_table(1);
    fb.add_u32(&mut cum_tbl, 0, 1, 0);
    let cum_off = fb.end_table(cum_tbl);

    let method_id_off = fb.create_string(&uuid_bytes_to_hyphenated_string(CUMULATIVE_METHOD_UUID));
    let mut method_tbl = fb.start_table(11);
    fb.add_offset(&mut method_tbl, 0, method_id_off);
    fb.add_u8(&mut method_tbl, 7, PROGRESS_CONFIG_CUMULATIVE_MODE, 0);
    fb.add_offset(&mut method_tbl, 8, cum_off);
    let method_off = fb.end_table(method_tbl);
    let methods_vec_off = fb.create_offset_vector(&[method_off]);

    // 2. Shared ContributionConfig (2 slots: 0: MethodId, 1: Parameters)
    let contrib_cfg_method_id =
        fb.create_string(&uuid_bytes_to_hyphenated_string(CUMULATIVE_METHOD_UUID));
    let mut contrib_cfg_tbl = fb.start_table(2);
    fb.add_offset(&mut contrib_cfg_tbl, 0, contrib_cfg_method_id);
    let shared_contrib_off = fb.end_table(contrib_cfg_tbl);

    // 3. Bucket achievements by category_id, keeping categories 1..=5 present even when empty.
    let mut direct_root_achs: Vec<&AchievementDef> = Vec::new();
    let mut by_category: BTreeMap<u32, Vec<&AchievementDef>> = BTreeMap::new();
    for cat_id in 1..=5 {
        by_category.entry(cat_id).or_default();
    }
    for def in defs {
        if def.category_id == 0 {
            direct_root_achs.push(def);
        } else {
            by_category.entry(def.category_id).or_default().push(def);
        }
    }

    let mut child_category_offs = Vec::with_capacity(by_category.len());
    for (cat_id, cat_defs) in by_category {
        let mut ach_offs = Vec::with_capacity(cat_defs.len());
        for def in cat_defs {
            let (name_id, desc_id) = localization_ids.get(&def.id).copied().unwrap_or((0, 0));
            ach_offs.push(build_achievement_table(
                &mut fb,
                def,
                name_id,
                desc_id,
                shared_contrib_off,
            ));
        }
        let internal_name = category_internal_name(cat_id);
        child_category_offs.push(build_category_table(
            &mut fb,
            u32_to_uuid(cat_id),
            &internal_name,
            &[],
            &ach_offs,
        ));
    }

    let direct_root_ach_offs: Vec<u32> = direct_root_achs
        .into_iter()
        .map(|def| {
            let (name_id, desc_id) = localization_ids.get(&def.id).copied().unwrap_or((0, 0));
            build_achievement_table(&mut fb, def, name_id, desc_id, shared_contrib_off)
        })
        .collect();

    // 4. All 4 AchievementCategoryRoot categories required by `_load_root_category`
    let career_program_off =
        build_category_table(&mut fb, ROOT_CAREER_PROGRAM_UUID, "career_program", &[], &[]);
    let challenges_off = build_category_table(
        &mut fb,
        ROOT_CHALLENGES_UUID,
        "challenges",
        &child_category_offs,
        &direct_root_ach_offs,
    );
    let time_limited_off =
        build_category_table(&mut fb, ROOT_TIME_LIMITED_UUID, "time_limited", &[], &[]);
    let manual_off = build_category_table(&mut fb, ROOT_MANUAL_UUID, "manual", &[], &[]);

    // 5. Root AchievementData (5 slots):
    // 0: CareerProgram, 1: Challenges, 2: TimeLimited, 3: ContributionMethods, 4: Manual
    let mut root_tbl = fb.start_table(5);
    fb.add_offset(&mut root_tbl, 0, career_program_off);
    fb.add_offset(&mut root_tbl, 1, challenges_off);
    fb.add_offset(&mut root_tbl, 2, time_limited_off);
    fb.add_offset(&mut root_tbl, 3, methods_vec_off);
    fb.add_offset(&mut root_tbl, 4, manual_off);
    let root_off = fb.end_table(root_tbl);

    fb.finish(root_off, ACHIEVEMENT_FILE_IDENTIFIER)
}
