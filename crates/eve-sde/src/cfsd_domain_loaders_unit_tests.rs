//! Unit and mutation-killing tests for `cfsd_domain_loaders`.

use super::*;

#[cfg(test)]
#[path = "cfsd_domain_assets_unit_tests.rs"]
mod cfsd_domain_assets_unit_tests;

pub(super) struct CfsdBuilder {
    pub(super) buf: Vec<u8>,
}

impl CfsdBuilder {
    pub(super) fn new(records: &[Vec<u8>]) -> Self {
        let mut buf = vec![0u8; 72];
        //cnt at 40, buckets = 2 at 48 (bucket 0 empty = 0, bucket 1 populated)
        buf[40..48].copy_from_slice(&(records.len() as u64).to_le_bytes());
        buf[48..56].copy_from_slice(&2u64.to_le_bytes());
        buf[56..64].copy_from_slice(&0u64.to_le_bytes());
        // Allocate bucket 1 at end of header: 8-byte count prefix + records
        let count_pos = buf.len();
        buf.extend_from_slice(&(records.len() as u64).to_le_bytes());
        let abs = buf.len();
        let boff = (abs - 32) as u64;
        buf[64..72].copy_from_slice(&boff.to_le_bytes());
        let _ = count_pos;
        for r in records {
            buf.extend_from_slice(r);
        }
        Self { buf }
    }

    pub(super) fn alloc_str(&mut self, s: &str) -> u64 {
        self.buf
            .extend_from_slice(&(s.len() as u64).to_le_bytes());
        let abs = self.buf.len();
        self.buf.extend_from_slice(s.as_bytes());
        (abs - 32) as u64
    }

    pub(super) fn write_u32(&mut self, abs_pos: usize, v: u32) {
        self.buf[abs_pos..abs_pos + 4].copy_from_slice(&v.to_le_bytes());
    }

    pub(super) fn write_i32(&mut self, abs_pos: usize, v: i32) {
        self.buf[abs_pos..abs_pos + 4].copy_from_slice(&v.to_le_bytes());
    }

    pub(super) fn write_u64(&mut self, abs_pos: usize, v: u64) {
        self.buf[abs_pos..abs_pos + 8].copy_from_slice(&v.to_le_bytes());
    }

    pub(super) fn write_i64(&mut self, abs_pos: usize, v: i64) {
        self.buf[abs_pos..abs_pos + 8].copy_from_slice(&v.to_le_bytes());
    }

    pub(super) fn write_f32(&mut self, abs_pos: usize, v: f32) {
        self.buf[abs_pos..abs_pos + 4].copy_from_slice(&v.to_le_bytes());
    }

    pub(super) fn write_f64(&mut self, abs_pos: usize, v: f64) {
        self.buf[abs_pos..abs_pos + 8].copy_from_slice(&v.to_le_bytes());
    }

    pub(super) fn rec_offset(idx: usize, stride: usize) -> usize {
        80 + idx * stride
    }
}

pub(super) fn register_cfsd_resource(
    sde: &mut SdeRegistry,
    res_path: &str,
    tag: &str,
    bytes: &[u8],
    record_count: usize,
) -> PathBuf {
    let file_path = std::env::temp_dir().join(format!(
        "evomu_cfsd_dom_{}_{}.fsdbinary",
        tag,
        std::process::id()
    ));
    std::fs::write(&file_path, bytes).expect("write temp cfsd file");
    sde.resources.insert(
        res_path.to_string(),
        SdeResourceInfo {
            res_path: res_path.to_string(),
            cdn_relative_path: file_path.display().to_string(),
            md5: String::new(),
            byte_size: bytes.len() as u64,
            format: "cfsd".to_string(),
            record_count,
            spot: String::new(),
        },
    );
    file_path
}

#[test]
fn agents_npc_characters_corp_activities_divisions_and_contraband() {
    let mut sde = SdeRegistry::default();
    let mut files = Vec::new();

    // 1. agentsinspace.fsdbinary (stride 20)
    let mut b_ais = CfsdBuilder::new(&[vec![0u8; 20]]);
    let r0 = CfsdBuilder::rec_offset(0, 20);
    b_ais.write_u32(r0, 3_019_480);
    b_ais.write_u32(r0 + 4, 1234);
    b_ais.write_u32(r0 + 8, 30_000_142);
    b_ais.write_u32(r0 + 12, 55);
    b_ais.write_u32(r0 + 16, 17_703);
    files.push(register_cfsd_resource(
        &mut sde,
        "res:/staticdata/agentsinspace.fsdbinary",
        "ais",
        &b_ais.buf,
        1,
    ));
    let ais = sde.load_agents_in_space().expect("load_agents_in_space");
    assert_eq!(
        ais.get(&3_019_480),
        Some(&AgentInSpaceInfo {
            agent_id: 3_019_480,
            dungeon_id: 1234,
            solar_system_id: 30_000_142,
            spawn_point_id: 55,
            type_id: 17_703,
        })
    );

    // 2. npccharacters.fsdbinary (stride 136): 3 records testing all mask bits & locator logic
    let mut b_npc = CfsdBuilder::new(&[vec![0u8; 136], vec![0u8; 136], vec![0u8; 136]]);
    let n0 = CfsdBuilder::rec_offset(0, 136);
    b_npc.write_u64(n0, 3_008_416);
    b_npc.write_u32(n0 + 16, 24); // agent_division_id
    b_npc.write_u32(n0 + 20, 4); // agent_level
    b_npc.write_u32(n0 + 24, 2); // agent_type_id
    b_npc.buf[n0 + 28] = 1; // is_locator
    b_npc.write_u32(n0 + 64, 11); // ancestry_id (mask & 2)
    b_npc.write_u32(n0 + 68, 5); // bloodline_id
    b_npc.write_u32(n0 + 80, 1_000_035); // corporation_id
    b_npc.write_u32(n0 + 92, 700_001); // name_id
    b_npc.write_u32(n0 + 100, 1); // race_id (mask & 128)
    b_npc.write_u32(n0 + 112, 60_003_760); // station_id (mask & 256)
    b_npc.write_u32(n0 + 128, 1 | 2 | 128 | 256);

    let n1 = CfsdBuilder::rec_offset(1, 136);
    b_npc.write_u64(n1, 3_008_417);
    b_npc.buf[n1 + 28] = 1; // locator byte non-zero, but is_agent == false -> is_locator must be false
    b_npc.write_u32(n1 + 68, 6);
    b_npc.write_u32(n1 + 80, 1_000_036);
    b_npc.write_u32(n1 + 92, 700_002);
    b_npc.write_u32(n1 + 128, 0);

    let n2 = CfsdBuilder::rec_offset(2, 136);
    b_npc.write_u64(n2, 3_008_418);
    b_npc.write_u32(n2 + 16, 22);
    b_npc.write_u32(n2 + 20, 1);
    b_npc.write_u32(n2 + 24, 3);
    b_npc.buf[n2 + 28] = 0; // agent with locator == false
    b_npc.write_u32(n2 + 128, 1);
    files.push(register_cfsd_resource(
        &mut sde,
        "res:/staticdata/npccharacters.fsdbinary",
        "npc",
        &b_npc.buf,
        3,
    ));
    let npcs = sde.load_npc_characters().expect("load_npc_characters");
    assert_eq!(
        npcs[&3_008_416],
        NpcCharacterRecord {
            character_id: 3_008_416,
            corporation_id: 1_000_035,
            race_id: Some(1),
            bloodline_id: 5,
            ancestry_id: Some(11),
            station_id: Some(60_003_760),
            name_id: 700_001,
            is_agent: true,
            agent_level: Some(4),
            agent_division_id: Some(24),
            agent_type_id: Some(2),
            is_locator: true,
        }
    );
    assert!(!npcs[&3_008_417].is_agent);
    assert!(!npcs[&3_008_417].is_locator);
    assert_eq!(npcs[&3_008_417].race_id, None);
    assert_eq!(npcs[&3_008_417].ancestry_id, None);
    assert_eq!(npcs[&3_008_417].station_id, None);
    assert_eq!(npcs[&3_008_417].agent_level, None);
    assert!(npcs[&3_008_418].is_agent);
    assert!(!npcs[&3_008_418].is_locator);

    // 3. corporationactivities.fsdbinary (stride 8)
    let mut b_ca = CfsdBuilder::new(&[vec![0u8; 8]]);
    let c0 = CfsdBuilder::rec_offset(0, 8);
    b_ca.write_u32(c0, 10);
    b_ca.write_u32(c0 + 4, 8888);
    files.push(register_cfsd_resource(
        &mut sde,
        "res:/staticdata/corporationactivities.fsdbinary",
        "ca",
        &b_ca.buf,
        1,
    ));
    let cas = sde
        .load_corporation_activities()
        .expect("load_corporation_activities");
    assert_eq!(
        cas[&10],
        CorporationActivityInfo {
            activity_id: 10,
            name_id: 8888
        }
    );

    // 4. npccorporationdivisions.fsdbinary (stride 40)
    let mut b_div = CfsdBuilder::new(&[vec![0u8; 40], vec![0u8; 40]]);
    let d_desc = b_div.alloc_str("Security Desc");
    let d_int = b_div.alloc_str("SECURITY");
    let d0 = CfsdBuilder::rec_offset(0, 40);
    b_div.write_u64(d0, 24);
    b_div.write_u64(d0 + 8, d_desc);
    b_div.write_u64(d0 + 16, d_int);
    b_div.write_u32(d0 + 24, 101);
    b_div.write_u32(d0 + 28, 102);
    b_div.write_u32(d0 + 32, 103);
    b_div.write_u32(d0 + 36, 1 | 2);

    let d1 = CfsdBuilder::rec_offset(1, 40);
    b_div.write_u64(d1, 25);
    b_div.write_u64(d1 + 16, 0); // rel == 0 -> empty string
    b_div.write_u32(d1 + 28, 202);
    b_div.write_u32(d1 + 32, 203);
    b_div.write_u32(d1 + 36, 0);
    files.push(register_cfsd_resource(
        &mut sde,
        "res:/staticdata/npccorporationdivisions.fsdbinary",
        "div",
        &b_div.buf,
        2,
    ));
    let divs = sde
        .load_npc_corporation_divisions()
        .expect("load_npc_corporation_divisions");
    assert_eq!(
        divs[&24],
        NpcCorporationDivisionInfo {
            division_id: 24,
            internal_name: "SECURITY".to_string(),
            description: Some("Security Desc".to_string()),
            name_id: 103,
            description_id: Some(101),
            leader_type_name_id: 102,
        }
    );
    assert_eq!(divs[&25].description, None);
    assert_eq!(divs[&25].description_id, None);
    assert_eq!(divs[&25].internal_name, "");

    // 5. contrabandtypes.fsdbinary (stride 24)
    let mut b_cb = CfsdBuilder::new(&[vec![0u8; 24]]);
    // Append inner dict: 2 buckets (bucket 0 = 0, bucket 1 -> 1 entry of 20 bytes)
    b_cb.buf.extend_from_slice(&2u64.to_le_bytes());
    let dict_abs = b_cb.buf.len();
    let d_rel = (dict_abs - 32) as u64;
    b_cb.buf.extend_from_slice(&0u64.to_le_bytes()); // bucket 0 empty
    b_cb.buf.extend_from_slice(&0u64.to_le_bytes()); // placeholder for bucket 1
    b_cb.buf.extend_from_slice(&1u64.to_le_bytes()); // bn = 1
    let babs = b_cb.buf.len();
    let boff = (babs - 32) as u64;
    b_cb.write_u64(dict_abs + 8, boff);
    b_cb.buf.extend_from_slice(&[0u8; 20]);
    b_cb.write_u32(babs, 500_001);
    b_cb.write_f32(babs + 4, 0.9);
    b_cb.write_f32(babs + 8, 0.5);
    b_cb.write_f32(babs + 12, 2.5);
    b_cb.write_f32(babs + 16, 0.25);

    let cb0 = CfsdBuilder::rec_offset(0, 24);
    b_cb.write_u64(cb0, 3721);
    b_cb.write_u64(cb0 + 8, d_rel);
    b_cb.write_u64(cb0 + 16, 1);
    files.push(register_cfsd_resource(
        &mut sde,
        "res:/staticdata/contrabandtypes.fsdbinary",
        "cb",
        &b_cb.buf,
        1,
    ));
    let cbs = sde.load_contraband_types().expect("load_contraband_types");
    assert_eq!(
        cbs[&3721][&500_001],
        ContrabandFactionRule {
            faction_id: 500_001,
            attack_min_sec: 0.9,
            confiscate_min_sec: 0.5,
            fine_by_value: 2.5,
            standing_loss: 0.25,
        }
    );

    for f in files {
        let _ = std::fs::remove_file(f);
    }
}
