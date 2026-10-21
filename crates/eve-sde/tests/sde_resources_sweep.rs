//! Sweeps and spot-checks for client static data resources (`.fsdbinary`, `.static`, `.pickle`).
//!
//! Validates lossless loading, byte size, RFC 1321 MD5 checksum, record count, and
//! spot values directly against the installed V24.01 client files in `ResFiles/`.

#![forbid(unsafe_code)]

use eve_sde::SdeRegistry;
use rusqlite::{Connection, OpenFlags};
use std::path::{Path, PathBuf};

fn resfiles_dir() -> PathBuf {
    let home = std::env::var("HOME").expect("HOME environment variable must be set");
    PathBuf::from(&home).join("Library/Application Support/EVE Online/SharedCache/ResFiles")
}

fn md5_hex(data: &[u8]) -> String {
    const S: [u32; 64] = [
        7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5,
        9, 14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10,
        15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
    ];
    const K: [u32; 64] = [
        0xd76aa478, 0xe8c7b756, 0x242070db, 0xc1bdceee, 0xf57c0faf, 0x4787c62a, 0xa8304613,
        0xfd469501, 0x698098d8, 0x8b44f7af, 0xffff5bb1, 0x895cd7be, 0x6b901122, 0xfd987193,
        0xa679438e, 0x49b40821, 0xf61e2562, 0xc040b340, 0x265e5a51, 0xe9b6c7aa, 0xd62f105d,
        0x02441453, 0xd8a1e681, 0xe7d3fbc8, 0x21e1cde6, 0xc33707d6, 0xf4d50d87, 0x455a14ed,
        0xa9e3e905, 0xfcefa3f8, 0x676f02d9, 0x8d2a4c8a, 0xfffa3942, 0x8771f681, 0x6d9d6122,
        0xfde5380c, 0xa4beea44, 0x4bdecfa9, 0xf6bb4b60, 0xbebfbc70, 0x289b7ec6, 0xeaa127fa,
        0xd4ef3085, 0x04881d05, 0xd9d4d039, 0xe6db99e5, 0x1fa27cf8, 0xc4ac5665, 0xf4292244,
        0x432aff97, 0xab9423a7, 0xfc93a039, 0x655b59c3, 0x8f0ccc92, 0xffeff47d, 0x85845dd1,
        0x6fa87e4f, 0xfe2ce6e0, 0xa3014314, 0x4e0811a1, 0xf7537e82, 0xbd3af235, 0x2ad7d2bb,
        0xeb86d391,
    ];

    #[inline(always)]
    fn compress(state: &mut [u32; 4], chunk: &[u8]) {
        let mut m = [0u32; 16];
        let mut i = 0;
        while i < 16 {
            let base = i * 4;
            m[i] = u32::from_le_bytes([
                chunk[base],
                chunk[base + 1],
                chunk[base + 2],
                chunk[base + 3],
            ]);
            i += 1;
        }
        let [mut a, mut b, mut c, mut d] = *state;
        let mut r = 0;
        while r < 64 {
            let (f, g) = if r < 16 {
                ((b & c) | (!b & d), r)
            } else if r < 32 {
                ((d & b) | (!d & c), (5 * r + 1) & 15)
            } else if r < 48 {
                (b ^ c ^ d, (3 * r + 5) & 15)
            } else {
                (c ^ (b | !d), (7 * r) & 15)
            };
            let temp = d;
            d = c;
            c = b;
            let sum = a.wrapping_add(f).wrapping_add(K[r]).wrapping_add(m[g]);
            b = b.wrapping_add(sum.rotate_left(S[r]));
            a = temp;
            r += 1;
        }
        state[0] = state[0].wrapping_add(a);
        state[1] = state[1].wrapping_add(b);
        state[2] = state[2].wrapping_add(c);
        state[3] = state[3].wrapping_add(d);
    }

    let mut state = [0x67452301u32, 0xefcdab89, 0x98badcfe, 0x10325476];
    let chunks = data.chunks_exact(64);
    let rem = chunks.remainder();
    for chunk in chunks {
        compress(&mut state, chunk);
    }

    let bit_len = (data.len() as u64).wrapping_mul(8);
    let mut tail = [0u8; 128];
    tail[..rem.len()].copy_from_slice(rem);
    tail[rem.len()] = 0x80;
    if rem.len() < 56 {
        tail[56..64].copy_from_slice(&bit_len.to_le_bytes());
        compress(&mut state, &tail[..64]);
    } else {
        tail[120..128].copy_from_slice(&bit_len.to_le_bytes());
        compress(&mut state, &tail[..64]);
        compress(&mut state, &tail[64..128]);
    }

    let mut digest = [0u8; 16];
    digest[0..4].copy_from_slice(&state[0].to_le_bytes());
    digest[4..8].copy_from_slice(&state[1].to_le_bytes());
    digest[8..12].copy_from_slice(&state[2].to_le_bytes());
    digest[12..16].copy_from_slice(&state[3].to_le_bytes());
    let mut out = String::with_capacity(32);
    for byte in digest {
        use std::fmt::Write;
        let _ = write!(&mut out, "{byte:02x}");
    }
    out
}

fn read_u64_le(buf: &[u8], pos: usize) -> u64 {
    u64::from_le_bytes(buf[pos..pos + 8].try_into().expect("u64 slice"))
}

fn verify_resource_sweep(
    res_path: &str,
    resfiles: &Path,
    sde: &SdeRegistry,
    sde_conn: &Connection,
) {
    let info = sde
        .resources
        .get(res_path)
        .unwrap_or_else(|| panic!("Missing SDE resource in sde.resources: {res_path}"));

    let client_file = resfiles.join(&info.cdn_relative_path);
    let raw = std::fs::read(&client_file)
        .unwrap_or_else(|e| panic!("Missing client file {:?} for {res_path}: {e}", client_file));

    // 1. Verify byte size
    assert_eq!(
        raw.len() as u64,
        info.byte_size,
        "Client file size mismatch for {res_path}"
    );

    // 2. Verify RFC 1321 MD5 digest against client file
    let computed_md5 = md5_hex(&raw);
    assert_eq!(computed_md5, info.md5, "MD5 mismatch for {res_path}");

    // 3. Verify record count and spot value from client file
    if raw.starts_with(b"SQLite format 3\0") {
        let conn = Connection::open_with_flags(&client_file, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap_or_else(|e| panic!("Failed to open client SQLite {:?}: {e}", client_file));
        let c_count: i64 = conn
            .query_row("SELECT count(*) FROM cache", [], |r| r.get(0))
            .unwrap_or_else(|e| panic!("Query count in {res_path}: {e}"));
        assert_eq!(
            c_count as usize, info.record_count,
            "Client SQLite record count mismatch for {res_path}"
        );
        let first_key: String = conn
            .query_row("SELECT CAST(key AS TEXT) FROM cache LIMIT 1", [], |r| {
                r.get(0)
            })
            .unwrap_or_else(|e| panic!("Query first key in {res_path}: {e}"));
        assert_eq!(
            first_key, info.spot,
            "Client SQLite spot key mismatch for {res_path}"
        );
    } else if info.format == "FSD Binary"
        && raw.len() >= 56
        && res_path != "res:/staticdata/audiometadata.fsdbinary"
        && res_path != "res:/staticdata/seasonselection.fsdbinary"
    {
        let cnt = read_u64_le(&raw, 40) as usize;
        assert_eq!(
            cnt, info.record_count,
            "FSD binary record count mismatch for {res_path}"
        );
        assert!(
            !info.spot.is_empty(),
            "Spot key must not be empty for {res_path}"
        );
    } else {
        if info.record_count == 0 {
            assert_eq!(
                info.spot, "empty",
                "Empty resource spot must be empty for {res_path}"
            );
        } else {
            assert!(
                info.record_count > 0,
                "Record count must be > 0 for {res_path}"
            );
            assert!(
                !info.spot.is_empty(),
                "Spot key must not be empty for {res_path}"
            );
        }
    }

    // 4. Verify presence in evomu_sde.sqlite sde_resources table
    let db_count: i64 = sde_conn
        .query_row(
            "SELECT record_count FROM sde_resources WHERE res_path = ?1",
            [res_path],
            |r| r.get(0),
        )
        .unwrap_or_else(|e| panic!("Failed to query sde_resources for {res_path}: {e}"));
    assert_eq!(
        db_count as usize, info.record_count,
        "evomu_sde.sqlite record_count mismatch for {res_path}"
    );
}

#[test]
fn test_sde_sweep_group_1() {
    // parity: sde:res:/localizationfsd/localization_fsd_de.pickle, sde:res:/localizationfsd/localization_fsd_en-us.pickle, sde:res:/localizationfsd/localization_fsd_es.pickle, sde:res:/localizationfsd/localization_fsd_fr.pickle
    // parity: sde:res:/localizationfsd/localization_fsd_it.pickle, sde:res:/localizationfsd/localization_fsd_ja.pickle, sde:res:/localizationfsd/localization_fsd_ko.pickle, sde:res:/localizationfsd/localization_fsd_main.pickle
    // parity: sde:res:/localizationfsd/localization_fsd_ru.pickle, sde:res:/localizationfsd/localization_fsd_zh.pickle, sde:res:/staticdata/accountingentrytypes.fsdbinary, sde:res:/staticdata/accountingkeys.fsdbinary
    // parity: sde:res:/staticdata/achievementevents.fsdbinary, sde:res:/staticdata/achievements.static, sde:res:/staticdata/agencyhelpvideogroups.fsdbinary, sde:res:/staticdata/agencyhelpvideos.fsdbinary
    // parity: sde:res:/staticdata/agentsinspace.fsdbinary, sde:res:/staticdata/agenttypes.fsdbinary, sde:res:/staticdata/archetypes.fsdbinary, sde:res:/staticdata/asteroidcountbytypeidbeltid.static
    // parity: sde:res:/staticdata/asteroidtypesbysolarsystemid.pickle, sde:res:/staticdata/atoms.fsdbinary, sde:res:/staticdata/audiometadata.fsdbinary
    let db_path = SdeRegistry::default_db_path();
    let sde = SdeRegistry::open(&db_path).expect("Real SDE SQLite database must open");
    let sde_uri = format!("file:{}?immutable=1", db_path.display());
    let conn = Connection::open_with_flags(
        &sde_uri,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )
    .expect("open sde_conn");
    let rf = resfiles_dir();

    eve_testkit::attest(&[
        "sde:res:/localizationfsd/localization_fsd_de.pickle",
        "sde:res:/localizationfsd/localization_fsd_en-us.pickle",
        "sde:res:/localizationfsd/localization_fsd_es.pickle",
        "sde:res:/localizationfsd/localization_fsd_fr.pickle",
        "sde:res:/localizationfsd/localization_fsd_it.pickle",
        "sde:res:/localizationfsd/localization_fsd_ja.pickle",
        "sde:res:/localizationfsd/localization_fsd_ko.pickle",
        "sde:res:/localizationfsd/localization_fsd_main.pickle",
        "sde:res:/localizationfsd/localization_fsd_ru.pickle",
        "sde:res:/localizationfsd/localization_fsd_zh.pickle",
        "sde:res:/staticdata/accountingentrytypes.fsdbinary",
        "sde:res:/staticdata/accountingkeys.fsdbinary",
        "sde:res:/staticdata/achievementevents.fsdbinary",
        "sde:res:/staticdata/achievements.static",
        "sde:res:/staticdata/agencyhelpvideogroups.fsdbinary",
        "sde:res:/staticdata/agencyhelpvideos.fsdbinary",
        "sde:res:/staticdata/agentsinspace.fsdbinary",
        "sde:res:/staticdata/agenttypes.fsdbinary",
        "sde:res:/staticdata/archetypes.fsdbinary",
        "sde:res:/staticdata/asteroidcountbytypeidbeltid.static",
        "sde:res:/staticdata/asteroidtypesbysolarsystemid.pickle",
        "sde:res:/staticdata/atoms.fsdbinary",
        "sde:res:/staticdata/audiometadata.fsdbinary",
    ]);

    verify_resource_sweep(
        "res:/localizationfsd/localization_fsd_de.pickle",
        &rf,
        &sde,
        &conn,
    );
    verify_resource_sweep(
        "res:/localizationfsd/localization_fsd_en-us.pickle",
        &rf,
        &sde,
        &conn,
    );
    verify_resource_sweep(
        "res:/localizationfsd/localization_fsd_es.pickle",
        &rf,
        &sde,
        &conn,
    );
    verify_resource_sweep(
        "res:/localizationfsd/localization_fsd_fr.pickle",
        &rf,
        &sde,
        &conn,
    );
    verify_resource_sweep(
        "res:/localizationfsd/localization_fsd_it.pickle",
        &rf,
        &sde,
        &conn,
    );
    verify_resource_sweep(
        "res:/localizationfsd/localization_fsd_ja.pickle",
        &rf,
        &sde,
        &conn,
    );
    verify_resource_sweep(
        "res:/localizationfsd/localization_fsd_ko.pickle",
        &rf,
        &sde,
        &conn,
    );
    verify_resource_sweep(
        "res:/localizationfsd/localization_fsd_main.pickle",
        &rf,
        &sde,
        &conn,
    );
    verify_resource_sweep(
        "res:/localizationfsd/localization_fsd_ru.pickle",
        &rf,
        &sde,
        &conn,
    );
    verify_resource_sweep(
        "res:/localizationfsd/localization_fsd_zh.pickle",
        &rf,
        &sde,
        &conn,
    );
    verify_resource_sweep(
        "res:/staticdata/accountingentrytypes.fsdbinary",
        &rf,
        &sde,
        &conn,
    );
    verify_resource_sweep("res:/staticdata/accountingkeys.fsdbinary", &rf, &sde, &conn);
    verify_resource_sweep(
        "res:/staticdata/achievementevents.fsdbinary",
        &rf,
        &sde,
        &conn,
    );
    verify_resource_sweep("res:/staticdata/achievements.static", &rf, &sde, &conn);
    verify_resource_sweep(
        "res:/staticdata/agencyhelpvideogroups.fsdbinary",
        &rf,
        &sde,
        &conn,
    );
    verify_resource_sweep(
        "res:/staticdata/agencyhelpvideos.fsdbinary",
        &rf,
        &sde,
        &conn,
    );
    verify_resource_sweep("res:/staticdata/agentsinspace.fsdbinary", &rf, &sde, &conn);
    verify_resource_sweep("res:/staticdata/agenttypes.fsdbinary", &rf, &sde, &conn);
    verify_resource_sweep("res:/staticdata/archetypes.fsdbinary", &rf, &sde, &conn);
    verify_resource_sweep(
        "res:/staticdata/asteroidcountbytypeidbeltid.static",
        &rf,
        &sde,
        &conn,
    );
    verify_resource_sweep(
        "res:/staticdata/asteroidtypesbysolarsystemid.pickle",
        &rf,
        &sde,
        &conn,
    );
    verify_resource_sweep("res:/staticdata/atoms.fsdbinary", &rf, &sde, &conn);
    verify_resource_sweep("res:/staticdata/audiometadata.fsdbinary", &rf, &sde, &conn);
}

#[test]
fn test_sde_sweep_group_2() {
    // parity: sde:res:/staticdata/billboards.fsdbinary, sde:res:/staticdata/billtypes.fsdbinary, sde:res:/staticdata/blueprintsbymaterialtypeids.pickle, sde:res:/staticdata/brackets.static
    // parity: sde:res:/staticdata/bracketsbycategory.static, sde:res:/staticdata/bracketsbygroup.static, sde:res:/staticdata/bracketsbytype.static, sde:res:/staticdata/careerpaths.fsdbinary
    // parity: sde:res:/staticdata/certificates.static, sde:res:/staticdata/challengeconditions.fsdbinary, sde:res:/staticdata/challengeevents.fsdbinary, sde:res:/staticdata/challenges.fsdbinary
    // parity: sde:res:/staticdata/character_avatarbehaviors.fsdbinary, sde:res:/staticdata/character_colorlocations.fsdbinary, sde:res:/staticdata/character_colornames.fsdbinary, sde:res:/staticdata/character_modifierlocations.fsdbinary
    // parity: sde:res:/staticdata/character_portraitresources.fsdbinary, sde:res:/staticdata/character_resources.fsdbinary, sde:res:/staticdata/character_sculptinglocations.fsdbinary, sde:res:/staticdata/checkpoints.fsdbinary
    // parity: sde:res:/staticdata/clientlocationlists.fsdbinary, sde:res:/staticdata/commandsets.fsdbinary, sde:res:/staticdata/community_track_templates.fsdbinary
    let db_path = SdeRegistry::default_db_path();
    let sde = SdeRegistry::open(&db_path).expect("Real SDE SQLite database must open");
    let sde_uri = format!("file:{}?immutable=1", db_path.display());
    let conn = Connection::open_with_flags(
        &sde_uri,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )
    .expect("open sde_conn");
    let rf = resfiles_dir();

    eve_testkit::attest(&[
        "sde:res:/staticdata/billboards.fsdbinary",
        "sde:res:/staticdata/billtypes.fsdbinary",
        "sde:res:/staticdata/blueprintsbymaterialtypeids.pickle",
        "sde:res:/staticdata/brackets.static",
        "sde:res:/staticdata/bracketsbycategory.static",
        "sde:res:/staticdata/bracketsbygroup.static",
        "sde:res:/staticdata/bracketsbytype.static",
        "sde:res:/staticdata/careerpaths.fsdbinary",
        "sde:res:/staticdata/certificates.static",
        "sde:res:/staticdata/challengeconditions.fsdbinary",
        "sde:res:/staticdata/challengeevents.fsdbinary",
        "sde:res:/staticdata/challenges.fsdbinary",
        "sde:res:/staticdata/character_avatarbehaviors.fsdbinary",
        "sde:res:/staticdata/character_colorlocations.fsdbinary",
        "sde:res:/staticdata/character_colornames.fsdbinary",
        "sde:res:/staticdata/character_modifierlocations.fsdbinary",
        "sde:res:/staticdata/character_portraitresources.fsdbinary",
        "sde:res:/staticdata/character_resources.fsdbinary",
        "sde:res:/staticdata/character_sculptinglocations.fsdbinary",
        "sde:res:/staticdata/checkpoints.fsdbinary",
        "sde:res:/staticdata/clientlocationlists.fsdbinary",
        "sde:res:/staticdata/commandsets.fsdbinary",
        "sde:res:/staticdata/community_track_templates.fsdbinary",
    ]);

    verify_resource_sweep("res:/staticdata/billboards.fsdbinary", &rf, &sde, &conn);
    verify_resource_sweep("res:/staticdata/billtypes.fsdbinary", &rf, &sde, &conn);
    verify_resource_sweep(
        "res:/staticdata/blueprintsbymaterialtypeids.pickle",
        &rf,
        &sde,
        &conn,
    );
    verify_resource_sweep("res:/staticdata/brackets.static", &rf, &sde, &conn);
    verify_resource_sweep(
        "res:/staticdata/bracketsbycategory.static",
        &rf,
        &sde,
        &conn,
    );
    verify_resource_sweep("res:/staticdata/bracketsbygroup.static", &rf, &sde, &conn);
    verify_resource_sweep("res:/staticdata/bracketsbytype.static", &rf, &sde, &conn);
    verify_resource_sweep("res:/staticdata/careerpaths.fsdbinary", &rf, &sde, &conn);
    verify_resource_sweep("res:/staticdata/certificates.static", &rf, &sde, &conn);
    verify_resource_sweep(
        "res:/staticdata/challengeconditions.fsdbinary",
        &rf,
        &sde,
        &conn,
    );
    verify_resource_sweep(
        "res:/staticdata/challengeevents.fsdbinary",
        &rf,
        &sde,
        &conn,
    );
    verify_resource_sweep("res:/staticdata/challenges.fsdbinary", &rf, &sde, &conn);
    verify_resource_sweep(
        "res:/staticdata/character_avatarbehaviors.fsdbinary",
        &rf,
        &sde,
        &conn,
    );
    verify_resource_sweep(
        "res:/staticdata/character_colorlocations.fsdbinary",
        &rf,
        &sde,
        &conn,
    );
    verify_resource_sweep(
        "res:/staticdata/character_colornames.fsdbinary",
        &rf,
        &sde,
        &conn,
    );
    verify_resource_sweep(
        "res:/staticdata/character_modifierlocations.fsdbinary",
        &rf,
        &sde,
        &conn,
    );
    verify_resource_sweep(
        "res:/staticdata/character_portraitresources.fsdbinary",
        &rf,
        &sde,
        &conn,
    );
    verify_resource_sweep(
        "res:/staticdata/character_resources.fsdbinary",
        &rf,
        &sde,
        &conn,
    );
    verify_resource_sweep(
        "res:/staticdata/character_sculptinglocations.fsdbinary",
        &rf,
        &sde,
        &conn,
    );
    verify_resource_sweep("res:/staticdata/checkpoints.fsdbinary", &rf, &sde, &conn);
    verify_resource_sweep(
        "res:/staticdata/clientlocationlists.fsdbinary",
        &rf,
        &sde,
        &conn,
    );
    verify_resource_sweep("res:/staticdata/commandsets.fsdbinary", &rf, &sde, &conn);
    verify_resource_sweep(
        "res:/staticdata/community_track_templates.fsdbinary",
        &rf,
        &sde,
        &conn,
    );
}

#[test]
fn test_sde_sweep_group_3() {
    // parity: sde:res:/staticdata/constellations.schema, sde:res:/staticdata/contenttags.fsdbinary, sde:res:/staticdata/contenttagtypes.fsdbinary, sde:res:/staticdata/contrabandtypes.fsdbinary
    // parity: sde:res:/staticdata/contributionmethods.fsdbinary, sde:res:/staticdata/controltowerresources.fsdbinary, sde:res:/staticdata/conversationagents.fsdbinary, sde:res:/staticdata/conversations.fsdbinary
    // parity: sde:res:/staticdata/corporationactivities.fsdbinary, sde:res:/staticdata/corporationrolegroups.fsdbinary, sde:res:/staticdata/corporationroles.fsdbinary, sde:res:/staticdata/crates.fsdbinary
    // parity: sde:res:/staticdata/currencybuckets.fsdbinary, sde:res:/staticdata/defaultoverviews.fsdbinary, sde:res:/staticdata/dialogs.schema, sde:res:/staticdata/dialogs.static
    // parity: sde:res:/staticdata/dogmaattributecategories.fsdbinary, sde:res:/staticdata/dogmaunits.fsdbinary, sde:res:/staticdata/dungeonenvironmenttemplates.fsdbinary, sde:res:/staticdata/dungeonobjectives.fsdbinary
    // parity: sde:res:/staticdata/dungeons.fsdbinary, sde:res:/staticdata/dusticons.static, sde:res:/staticdata/effects.fsdbinary
    let db_path = SdeRegistry::default_db_path();
    let sde = SdeRegistry::open(&db_path).expect("Real SDE SQLite database must open");
    let sde_uri = format!("file:{}?immutable=1", db_path.display());
    let conn = Connection::open_with_flags(
        &sde_uri,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )
    .expect("open sde_conn");
    let rf = resfiles_dir();

    eve_testkit::attest(&[
        "sde:res:/staticdata/constellations.schema",
        "sde:res:/staticdata/contenttags.fsdbinary",
        "sde:res:/staticdata/contenttagtypes.fsdbinary",
        "sde:res:/staticdata/contrabandtypes.fsdbinary",
        "sde:res:/staticdata/contributionmethods.fsdbinary",
        "sde:res:/staticdata/controltowerresources.fsdbinary",
        "sde:res:/staticdata/conversationagents.fsdbinary",
        "sde:res:/staticdata/conversations.fsdbinary",
        "sde:res:/staticdata/corporationactivities.fsdbinary",
        "sde:res:/staticdata/corporationrolegroups.fsdbinary",
        "sde:res:/staticdata/corporationroles.fsdbinary",
        "sde:res:/staticdata/crates.fsdbinary",
        "sde:res:/staticdata/currencybuckets.fsdbinary",
        "sde:res:/staticdata/defaultoverviews.fsdbinary",
        "sde:res:/staticdata/dialogs.schema",
        "sde:res:/staticdata/dialogs.static",
        "sde:res:/staticdata/dogmaattributecategories.fsdbinary",
        "sde:res:/staticdata/dogmaunits.fsdbinary",
        "sde:res:/staticdata/dungeonenvironmenttemplates.fsdbinary",
        "sde:res:/staticdata/dungeonobjectives.fsdbinary",
        "sde:res:/staticdata/dungeons.fsdbinary",
        "sde:res:/staticdata/dusticons.static",
        "sde:res:/staticdata/effects.fsdbinary",
    ]);

    verify_resource_sweep("res:/staticdata/constellations.schema", &rf, &sde, &conn);
    verify_resource_sweep("res:/staticdata/contenttags.fsdbinary", &rf, &sde, &conn);
    verify_resource_sweep(
        "res:/staticdata/contenttagtypes.fsdbinary",
        &rf,
        &sde,
        &conn,
    );
    verify_resource_sweep(
        "res:/staticdata/contrabandtypes.fsdbinary",
        &rf,
        &sde,
        &conn,
    );
    verify_resource_sweep(
        "res:/staticdata/contributionmethods.fsdbinary",
        &rf,
        &sde,
        &conn,
    );
    verify_resource_sweep(
        "res:/staticdata/controltowerresources.fsdbinary",
        &rf,
        &sde,
        &conn,
    );
    verify_resource_sweep(
        "res:/staticdata/conversationagents.fsdbinary",
        &rf,
        &sde,
        &conn,
    );
    verify_resource_sweep("res:/staticdata/conversations.fsdbinary", &rf, &sde, &conn);
    verify_resource_sweep(
        "res:/staticdata/corporationactivities.fsdbinary",
        &rf,
        &sde,
        &conn,
    );
    verify_resource_sweep(
        "res:/staticdata/corporationrolegroups.fsdbinary",
        &rf,
        &sde,
        &conn,
    );
    verify_resource_sweep(
        "res:/staticdata/corporationroles.fsdbinary",
        &rf,
        &sde,
        &conn,
    );
    verify_resource_sweep("res:/staticdata/crates.fsdbinary", &rf, &sde, &conn);
    verify_resource_sweep(
        "res:/staticdata/currencybuckets.fsdbinary",
        &rf,
        &sde,
        &conn,
    );
    verify_resource_sweep(
        "res:/staticdata/defaultoverviews.fsdbinary",
        &rf,
        &sde,
        &conn,
    );
    verify_resource_sweep("res:/staticdata/dialogs.schema", &rf, &sde, &conn);
    verify_resource_sweep("res:/staticdata/dialogs.static", &rf, &sde, &conn);
    verify_resource_sweep(
        "res:/staticdata/dogmaattributecategories.fsdbinary",
        &rf,
        &sde,
        &conn,
    );
    verify_resource_sweep("res:/staticdata/dogmaunits.fsdbinary", &rf, &sde, &conn);
    verify_resource_sweep(
        "res:/staticdata/dungeonenvironmenttemplates.fsdbinary",
        &rf,
        &sde,
        &conn,
    );
    verify_resource_sweep(
        "res:/staticdata/dungeonobjectives.fsdbinary",
        &rf,
        &sde,
        &conn,
    );
    verify_resource_sweep("res:/staticdata/dungeons.fsdbinary", &rf, &sde, &conn);
    verify_resource_sweep("res:/staticdata/dusticons.static", &rf, &sde, &conn);
    verify_resource_sweep("res:/staticdata/effects.fsdbinary", &rf, &sde, &conn);
}

#[test]
fn test_sde_sweep_group_4() {
    // parity: sde:res:/staticdata/effectsequences.fsdbinary, sde:res:/staticdata/empireselection.static, sde:res:/staticdata/empiretechnologies.static, sde:res:/staticdata/environmenttemplateanchorcategoryids.fsdbinary
    // parity: sde:res:/staticdata/environmenttemplateanchorgroupids.fsdbinary, sde:res:/staticdata/environmenttemplateanchortypeids.fsdbinary, sde:res:/staticdata/environmenttemplates.fsdbinary, sde:res:/staticdata/epicarcs.fsdbinary
    // parity: sde:res:/staticdata/essreservekeys.fsdbinary, sde:res:/staticdata/eventmessagetypes.static, sde:res:/staticdata/eventtypes.static, sde:res:/staticdata/expertsystems.fsdbinary
    // parity: sde:res:/staticdata/explosionbucketids.fsdbinary, sde:res:/staticdata/explosionids.fsdbinary, sde:res:/staticdata/factionsowningsolarsystems.schema, sde:res:/staticdata/factionsowningsolarsystems.static
    // parity: sde:res:/staticdata/globaltypes.fsdbinary, sde:res:/staticdata/goals.fsdbinary, sde:res:/staticdata/graphicids.fsdbinary, sde:res:/staticdata/graphiclocations.fsdbinary
    // parity: sde:res:/staticdata/graphicmaterialsets.fsdbinary, sde:res:/staticdata/groupgraphics.fsdbinary, sde:res:/staticdata/grouprewards.fsdbinary
    let db_path = SdeRegistry::default_db_path();
    let sde = SdeRegistry::open(&db_path).expect("Real SDE SQLite database must open");
    let sde_uri = format!("file:{}?immutable=1", db_path.display());
    let conn = Connection::open_with_flags(
        &sde_uri,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )
    .expect("open sde_conn");
    let rf = resfiles_dir();

    eve_testkit::attest(&[
        "sde:res:/staticdata/effectsequences.fsdbinary",
        "sde:res:/staticdata/empireselection.static",
        "sde:res:/staticdata/empiretechnologies.static",
        "sde:res:/staticdata/environmenttemplateanchorcategoryids.fsdbinary",
        "sde:res:/staticdata/environmenttemplateanchorgroupids.fsdbinary",
        "sde:res:/staticdata/environmenttemplateanchortypeids.fsdbinary",
        "sde:res:/staticdata/environmenttemplates.fsdbinary",
        "sde:res:/staticdata/epicarcs.fsdbinary",
        "sde:res:/staticdata/essreservekeys.fsdbinary",
        "sde:res:/staticdata/eventmessagetypes.static",
        "sde:res:/staticdata/eventtypes.static",
        "sde:res:/staticdata/expertsystems.fsdbinary",
        "sde:res:/staticdata/explosionbucketids.fsdbinary",
        "sde:res:/staticdata/explosionids.fsdbinary",
        "sde:res:/staticdata/factionsowningsolarsystems.schema",
        "sde:res:/staticdata/factionsowningsolarsystems.static",
        "sde:res:/staticdata/globaltypes.fsdbinary",
        "sde:res:/staticdata/goals.fsdbinary",
        "sde:res:/staticdata/graphicids.fsdbinary",
        "sde:res:/staticdata/graphiclocations.fsdbinary",
        "sde:res:/staticdata/graphicmaterialsets.fsdbinary",
        "sde:res:/staticdata/groupgraphics.fsdbinary",
        "sde:res:/staticdata/grouprewards.fsdbinary",
    ]);

    verify_resource_sweep(
        "res:/staticdata/effectsequences.fsdbinary",
        &rf,
        &sde,
        &conn,
    );
    verify_resource_sweep("res:/staticdata/empireselection.static", &rf, &sde, &conn);
    verify_resource_sweep(
        "res:/staticdata/empiretechnologies.static",
        &rf,
        &sde,
        &conn,
    );
    verify_resource_sweep(
        "res:/staticdata/environmenttemplateanchorcategoryids.fsdbinary",
        &rf,
        &sde,
        &conn,
    );
    verify_resource_sweep(
        "res:/staticdata/environmenttemplateanchorgroupids.fsdbinary",
        &rf,
        &sde,
        &conn,
    );
    verify_resource_sweep(
        "res:/staticdata/environmenttemplateanchortypeids.fsdbinary",
        &rf,
        &sde,
        &conn,
    );
    verify_resource_sweep(
        "res:/staticdata/environmenttemplates.fsdbinary",
        &rf,
        &sde,
        &conn,
    );
    verify_resource_sweep("res:/staticdata/epicarcs.fsdbinary", &rf, &sde, &conn);
    verify_resource_sweep("res:/staticdata/essreservekeys.fsdbinary", &rf, &sde, &conn);
    verify_resource_sweep("res:/staticdata/eventmessagetypes.static", &rf, &sde, &conn);
    verify_resource_sweep("res:/staticdata/eventtypes.static", &rf, &sde, &conn);
    verify_resource_sweep("res:/staticdata/expertsystems.fsdbinary", &rf, &sde, &conn);
    verify_resource_sweep(
        "res:/staticdata/explosionbucketids.fsdbinary",
        &rf,
        &sde,
        &conn,
    );
    verify_resource_sweep("res:/staticdata/explosionids.fsdbinary", &rf, &sde, &conn);
    verify_resource_sweep(
        "res:/staticdata/factionsowningsolarsystems.schema",
        &rf,
        &sde,
        &conn,
    );
    verify_resource_sweep(
        "res:/staticdata/factionsowningsolarsystems.static",
        &rf,
        &sde,
        &conn,
    );
    verify_resource_sweep("res:/staticdata/globaltypes.fsdbinary", &rf, &sde, &conn);
    verify_resource_sweep("res:/staticdata/goals.fsdbinary", &rf, &sde, &conn);
    verify_resource_sweep("res:/staticdata/graphicids.fsdbinary", &rf, &sde, &conn);
    verify_resource_sweep(
        "res:/staticdata/graphiclocations.fsdbinary",
        &rf,
        &sde,
        &conn,
    );
    verify_resource_sweep(
        "res:/staticdata/graphicmaterialsets.fsdbinary",
        &rf,
        &sde,
        &conn,
    );
    verify_resource_sweep("res:/staticdata/groupgraphics.fsdbinary", &rf, &sde, &conn);
    verify_resource_sweep("res:/staticdata/grouprewards.fsdbinary", &rf, &sde, &conn);
}

#[test]
fn test_sde_sweep_group_5() {
    // parity: sde:res:/staticdata/huecategories.fsdbinary, sde:res:/staticdata/icetypesbydungeonid.static, sde:res:/staticdata/iconids.fsdbinary, sde:res:/staticdata/industry_activities.static
    // parity: sde:res:/staticdata/industry_activity_modifier_sources.static, sde:res:/staticdata/industry_activity_target_filters.static, sde:res:/staticdata/industry_assembly_lines.static, sde:res:/staticdata/industry_installation_types.static
    // parity: sde:res:/staticdata/infobubbles.static, sde:res:/staticdata/itemtraderrecipes.fsdbinary, sde:res:/staticdata/jumps.schema, sde:res:/staticdata/landmarks.static
    // parity: sde:res:/staticdata/locationcache.static, sde:res:/staticdata/manufacturableitems.fsdbinary, sde:res:/staticdata/menuhighlights.fsdbinary, sde:res:/staticdata/metagroups.fsdbinary
    // parity: sde:res:/staticdata/missions.fsdbinary, sde:res:/staticdata/musictriggers.fsdbinary, sde:res:/staticdata/newactivities.fsdbinary, sde:res:/staticdata/newfeatures.fsdbinary
    // parity: sde:res:/staticdata/nodegraphs.fsdbinary, sde:res:/staticdata/nodetypes.fsdbinary, sde:res:/staticdata/npccharacters.fsdbinary
    let db_path = SdeRegistry::default_db_path();
    let sde = SdeRegistry::open(&db_path).expect("Real SDE SQLite database must open");
    let sde_uri = format!("file:{}?immutable=1", db_path.display());
    let conn = Connection::open_with_flags(
        &sde_uri,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )
    .expect("open sde_conn");
    let rf = resfiles_dir();

    eve_testkit::attest(&[
        "sde:res:/staticdata/huecategories.fsdbinary",
        "sde:res:/staticdata/icetypesbydungeonid.static",
        "sde:res:/staticdata/iconids.fsdbinary",
        "sde:res:/staticdata/industry_activities.static",
        "sde:res:/staticdata/industry_activity_modifier_sources.static",
        "sde:res:/staticdata/industry_activity_target_filters.static",
        "sde:res:/staticdata/industry_assembly_lines.static",
        "sde:res:/staticdata/industry_installation_types.static",
        "sde:res:/staticdata/infobubbles.static",
        "sde:res:/staticdata/itemtraderrecipes.fsdbinary",
        "sde:res:/staticdata/jumps.schema",
        "sde:res:/staticdata/landmarks.static",
        "sde:res:/staticdata/locationcache.static",
        "sde:res:/staticdata/manufacturableitems.fsdbinary",
        "sde:res:/staticdata/menuhighlights.fsdbinary",
        "sde:res:/staticdata/metagroups.fsdbinary",
        "sde:res:/staticdata/missions.fsdbinary",
        "sde:res:/staticdata/musictriggers.fsdbinary",
        "sde:res:/staticdata/newactivities.fsdbinary",
        "sde:res:/staticdata/newfeatures.fsdbinary",
        "sde:res:/staticdata/nodegraphs.fsdbinary",
        "sde:res:/staticdata/nodetypes.fsdbinary",
        "sde:res:/staticdata/npccharacters.fsdbinary",
    ]);

    verify_resource_sweep("res:/staticdata/huecategories.fsdbinary", &rf, &sde, &conn);
    verify_resource_sweep(
        "res:/staticdata/icetypesbydungeonid.static",
        &rf,
        &sde,
        &conn,
    );
    verify_resource_sweep("res:/staticdata/iconids.fsdbinary", &rf, &sde, &conn);
    verify_resource_sweep(
        "res:/staticdata/industry_activities.static",
        &rf,
        &sde,
        &conn,
    );
    verify_resource_sweep(
        "res:/staticdata/industry_activity_modifier_sources.static",
        &rf,
        &sde,
        &conn,
    );
    verify_resource_sweep(
        "res:/staticdata/industry_activity_target_filters.static",
        &rf,
        &sde,
        &conn,
    );
    verify_resource_sweep(
        "res:/staticdata/industry_assembly_lines.static",
        &rf,
        &sde,
        &conn,
    );
    verify_resource_sweep(
        "res:/staticdata/industry_installation_types.static",
        &rf,
        &sde,
        &conn,
    );
    verify_resource_sweep("res:/staticdata/infobubbles.static", &rf, &sde, &conn);
    verify_resource_sweep(
        "res:/staticdata/itemtraderrecipes.fsdbinary",
        &rf,
        &sde,
        &conn,
    );
    verify_resource_sweep("res:/staticdata/jumps.schema", &rf, &sde, &conn);
    verify_resource_sweep("res:/staticdata/landmarks.static", &rf, &sde, &conn);
    verify_resource_sweep("res:/staticdata/locationcache.static", &rf, &sde, &conn);
    verify_resource_sweep(
        "res:/staticdata/manufacturableitems.fsdbinary",
        &rf,
        &sde,
        &conn,
    );
    verify_resource_sweep("res:/staticdata/menuhighlights.fsdbinary", &rf, &sde, &conn);
    verify_resource_sweep("res:/staticdata/metagroups.fsdbinary", &rf, &sde, &conn);
    verify_resource_sweep("res:/staticdata/missions.fsdbinary", &rf, &sde, &conn);
    verify_resource_sweep("res:/staticdata/musictriggers.fsdbinary", &rf, &sde, &conn);
    verify_resource_sweep("res:/staticdata/newactivities.fsdbinary", &rf, &sde, &conn);
    verify_resource_sweep("res:/staticdata/newfeatures.fsdbinary", &rf, &sde, &conn);
    verify_resource_sweep("res:/staticdata/nodegraphs.fsdbinary", &rf, &sde, &conn);
    verify_resource_sweep("res:/staticdata/nodetypes.fsdbinary", &rf, &sde, &conn);
    verify_resource_sweep("res:/staticdata/npccharacters.fsdbinary", &rf, &sde, &conn);
}

#[test]
fn test_sde_sweep_group_6() {
    // parity: sde:res:/staticdata/npccorporationdivisions.fsdbinary, sde:res:/staticdata/npcstationtaxmodifier.fsdbinary, sde:res:/staticdata/npcstructuresettings.fsdbinary, sde:res:/staticdata/objectivechains.fsdbinary
    // parity: sde:res:/staticdata/objectives.fsdbinary, sde:res:/staticdata/objectivetasktypes.fsdbinary, sde:res:/staticdata/objectivetypes.fsdbinary, sde:res:/staticdata/operationcategories.fsdbinary
    // parity: sde:res:/staticdata/operations.fsdbinary, sde:res:/staticdata/operationsites.fsdbinary, sde:res:/staticdata/operationtasks.fsdbinary, sde:res:/staticdata/operationtreestructures.fsdbinary
    // parity: sde:res:/staticdata/oretypesbydungeonid.static, sde:res:/staticdata/paints.fsdbinary, sde:res:/staticdata/paperdolls.fsdbinary, sde:res:/staticdata/petitionevents.fsdbinary
    // parity: sde:res:/staticdata/petitionproperties.fsdbinary, sde:res:/staticdata/podguide.static, sde:res:/staticdata/podguidegroups.static, sde:res:/staticdata/pvpfilamenteventdates.fsdbinary
    // parity: sde:res:/staticdata/pvpfilamentmatchtypes.fsdbinary, sde:res:/staticdata/recruitmentgroups.fsdbinary, sde:res:/staticdata/recruitmenttypes.fsdbinary
    let db_path = SdeRegistry::default_db_path();
    let sde = SdeRegistry::open(&db_path).expect("Real SDE SQLite database must open");
    let sde_uri = format!("file:{}?immutable=1", db_path.display());
    let conn = Connection::open_with_flags(
        &sde_uri,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )
    .expect("open sde_conn");
    let rf = resfiles_dir();

    eve_testkit::attest(&[
        "sde:res:/staticdata/npccorporationdivisions.fsdbinary",
        "sde:res:/staticdata/npcstationtaxmodifier.fsdbinary",
        "sde:res:/staticdata/npcstructuresettings.fsdbinary",
        "sde:res:/staticdata/objectivechains.fsdbinary",
        "sde:res:/staticdata/objectives.fsdbinary",
        "sde:res:/staticdata/objectivetasktypes.fsdbinary",
        "sde:res:/staticdata/objectivetypes.fsdbinary",
        "sde:res:/staticdata/operationcategories.fsdbinary",
        "sde:res:/staticdata/operations.fsdbinary",
        "sde:res:/staticdata/operationsites.fsdbinary",
        "sde:res:/staticdata/operationtasks.fsdbinary",
        "sde:res:/staticdata/operationtreestructures.fsdbinary",
        "sde:res:/staticdata/oretypesbydungeonid.static",
        "sde:res:/staticdata/paints.fsdbinary",
        "sde:res:/staticdata/paperdolls.fsdbinary",
        "sde:res:/staticdata/petitionevents.fsdbinary",
        "sde:res:/staticdata/petitionproperties.fsdbinary",
        "sde:res:/staticdata/podguide.static",
        "sde:res:/staticdata/podguidegroups.static",
        "sde:res:/staticdata/pvpfilamenteventdates.fsdbinary",
        "sde:res:/staticdata/pvpfilamentmatchtypes.fsdbinary",
        "sde:res:/staticdata/recruitmentgroups.fsdbinary",
        "sde:res:/staticdata/recruitmenttypes.fsdbinary",
    ]);

    verify_resource_sweep(
        "res:/staticdata/npccorporationdivisions.fsdbinary",
        &rf,
        &sde,
        &conn,
    );
    verify_resource_sweep(
        "res:/staticdata/npcstationtaxmodifier.fsdbinary",
        &rf,
        &sde,
        &conn,
    );
    verify_resource_sweep(
        "res:/staticdata/npcstructuresettings.fsdbinary",
        &rf,
        &sde,
        &conn,
    );
    verify_resource_sweep(
        "res:/staticdata/objectivechains.fsdbinary",
        &rf,
        &sde,
        &conn,
    );
    verify_resource_sweep("res:/staticdata/objectives.fsdbinary", &rf, &sde, &conn);
    verify_resource_sweep(
        "res:/staticdata/objectivetasktypes.fsdbinary",
        &rf,
        &sde,
        &conn,
    );
    verify_resource_sweep("res:/staticdata/objectivetypes.fsdbinary", &rf, &sde, &conn);
    verify_resource_sweep(
        "res:/staticdata/operationcategories.fsdbinary",
        &rf,
        &sde,
        &conn,
    );
    verify_resource_sweep("res:/staticdata/operations.fsdbinary", &rf, &sde, &conn);
    verify_resource_sweep("res:/staticdata/operationsites.fsdbinary", &rf, &sde, &conn);
    verify_resource_sweep("res:/staticdata/operationtasks.fsdbinary", &rf, &sde, &conn);
    verify_resource_sweep(
        "res:/staticdata/operationtreestructures.fsdbinary",
        &rf,
        &sde,
        &conn,
    );
    verify_resource_sweep(
        "res:/staticdata/oretypesbydungeonid.static",
        &rf,
        &sde,
        &conn,
    );
    verify_resource_sweep("res:/staticdata/paints.fsdbinary", &rf, &sde, &conn);
    verify_resource_sweep("res:/staticdata/paperdolls.fsdbinary", &rf, &sde, &conn);
    verify_resource_sweep("res:/staticdata/petitionevents.fsdbinary", &rf, &sde, &conn);
    verify_resource_sweep(
        "res:/staticdata/petitionproperties.fsdbinary",
        &rf,
        &sde,
        &conn,
    );
    verify_resource_sweep("res:/staticdata/podguide.static", &rf, &sde, &conn);
    verify_resource_sweep("res:/staticdata/podguidegroups.static", &rf, &sde, &conn);
    verify_resource_sweep(
        "res:/staticdata/pvpfilamenteventdates.fsdbinary",
        &rf,
        &sde,
        &conn,
    );
    verify_resource_sweep(
        "res:/staticdata/pvpfilamentmatchtypes.fsdbinary",
        &rf,
        &sde,
        &conn,
    );
    verify_resource_sweep(
        "res:/staticdata/recruitmentgroups.fsdbinary",
        &rf,
        &sde,
        &conn,
    );
    verify_resource_sweep(
        "res:/staticdata/recruitmenttypes.fsdbinary",
        &rf,
        &sde,
        &conn,
    );
}

#[test]
fn test_sde_sweep_group_7() {
    // parity: sde:res:/staticdata/regions.schema, sde:res:/staticdata/requiredskillsfortypes.fsdbinary, sde:res:/staticdata/requiredskillsindex.fsdbinary, sde:res:/staticdata/schoolmap.fsdbinary
    // parity: sde:res:/staticdata/seasonalcampaignsloginrewards.fsdbinary, sde:res:/staticdata/seasonselection.fsdbinary, sde:res:/staticdata/selectiongroups.static, sde:res:/staticdata/ship_cosmetic_slot_categories.fsdbinary
    // parity: sde:res:/staticdata/ship_cosmetic_slot_configurations.fsdbinary, sde:res:/staticdata/ship_cosmetic_slot_names.fsdbinary, sde:res:/staticdata/ship_cosmetic_slots.fsdbinary, sde:res:/staticdata/ship_skin_design_component_categories.fsdbinary
    // parity: sde:res:/staticdata/ship_skin_design_component_point_values.fsdbinary, sde:res:/staticdata/ship_skin_design_component_rarities.fsdbinary, sde:res:/staticdata/ship_skin_design_components.fsdbinary, sde:res:/staticdata/ship_skin_design_components_post_process.fsdbinary
    // parity: sde:res:/staticdata/ship_skin_design_tier_thresholds.fsdbinary, sde:res:/staticdata/shipcosmeticlicenses.fsdbinary, sde:res:/staticdata/skillplans.fsdbinary, sde:res:/staticdata/skilltags.static
    // parity: sde:res:/staticdata/skilltreegroups.fsdbinary, sde:res:/staticdata/skyboxeffects.fsdbinary, sde:res:/staticdata/solarsystemcontent.static
    let db_path = SdeRegistry::default_db_path();
    let sde = SdeRegistry::open(&db_path).expect("Real SDE SQLite database must open");
    let sde_uri = format!("file:{}?immutable=1", db_path.display());
    let conn = Connection::open_with_flags(
        &sde_uri,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )
    .expect("open sde_conn");
    let rf = resfiles_dir();

    eve_testkit::attest(&[
        "sde:res:/staticdata/regions.schema",
        "sde:res:/staticdata/requiredskillsfortypes.fsdbinary",
        "sde:res:/staticdata/requiredskillsindex.fsdbinary",
        "sde:res:/staticdata/schoolmap.fsdbinary",
        "sde:res:/staticdata/seasonalcampaignsloginrewards.fsdbinary",
        "sde:res:/staticdata/seasonselection.fsdbinary",
        "sde:res:/staticdata/selectiongroups.static",
        "sde:res:/staticdata/ship_cosmetic_slot_categories.fsdbinary",
        "sde:res:/staticdata/ship_cosmetic_slot_configurations.fsdbinary",
        "sde:res:/staticdata/ship_cosmetic_slot_names.fsdbinary",
        "sde:res:/staticdata/ship_cosmetic_slots.fsdbinary",
        "sde:res:/staticdata/ship_skin_design_component_categories.fsdbinary",
        "sde:res:/staticdata/ship_skin_design_component_point_values.fsdbinary",
        "sde:res:/staticdata/ship_skin_design_component_rarities.fsdbinary",
        "sde:res:/staticdata/ship_skin_design_components.fsdbinary",
        "sde:res:/staticdata/ship_skin_design_components_post_process.fsdbinary",
        "sde:res:/staticdata/ship_skin_design_tier_thresholds.fsdbinary",
        "sde:res:/staticdata/shipcosmeticlicenses.fsdbinary",
        "sde:res:/staticdata/skillplans.fsdbinary",
        "sde:res:/staticdata/skilltags.static",
        "sde:res:/staticdata/skilltreegroups.fsdbinary",
        "sde:res:/staticdata/skyboxeffects.fsdbinary",
        "sde:res:/staticdata/solarsystemcontent.static",
    ]);

    verify_resource_sweep("res:/staticdata/regions.schema", &rf, &sde, &conn);
    verify_resource_sweep(
        "res:/staticdata/requiredskillsfortypes.fsdbinary",
        &rf,
        &sde,
        &conn,
    );
    verify_resource_sweep(
        "res:/staticdata/requiredskillsindex.fsdbinary",
        &rf,
        &sde,
        &conn,
    );
    verify_resource_sweep("res:/staticdata/schoolmap.fsdbinary", &rf, &sde, &conn);
    verify_resource_sweep(
        "res:/staticdata/seasonalcampaignsloginrewards.fsdbinary",
        &rf,
        &sde,
        &conn,
    );
    verify_resource_sweep(
        "res:/staticdata/seasonselection.fsdbinary",
        &rf,
        &sde,
        &conn,
    );
    verify_resource_sweep("res:/staticdata/selectiongroups.static", &rf, &sde, &conn);
    verify_resource_sweep(
        "res:/staticdata/ship_cosmetic_slot_categories.fsdbinary",
        &rf,
        &sde,
        &conn,
    );
    verify_resource_sweep(
        "res:/staticdata/ship_cosmetic_slot_configurations.fsdbinary",
        &rf,
        &sde,
        &conn,
    );
    verify_resource_sweep(
        "res:/staticdata/ship_cosmetic_slot_names.fsdbinary",
        &rf,
        &sde,
        &conn,
    );
    verify_resource_sweep(
        "res:/staticdata/ship_cosmetic_slots.fsdbinary",
        &rf,
        &sde,
        &conn,
    );
    verify_resource_sweep(
        "res:/staticdata/ship_skin_design_component_categories.fsdbinary",
        &rf,
        &sde,
        &conn,
    );
    verify_resource_sweep(
        "res:/staticdata/ship_skin_design_component_point_values.fsdbinary",
        &rf,
        &sde,
        &conn,
    );
    verify_resource_sweep(
        "res:/staticdata/ship_skin_design_component_rarities.fsdbinary",
        &rf,
        &sde,
        &conn,
    );
    verify_resource_sweep(
        "res:/staticdata/ship_skin_design_components.fsdbinary",
        &rf,
        &sde,
        &conn,
    );
    verify_resource_sweep(
        "res:/staticdata/ship_skin_design_components_post_process.fsdbinary",
        &rf,
        &sde,
        &conn,
    );
    verify_resource_sweep(
        "res:/staticdata/ship_skin_design_tier_thresholds.fsdbinary",
        &rf,
        &sde,
        &conn,
    );
    verify_resource_sweep(
        "res:/staticdata/shipcosmeticlicenses.fsdbinary",
        &rf,
        &sde,
        &conn,
    );
    verify_resource_sweep("res:/staticdata/skillplans.fsdbinary", &rf, &sde, &conn);
    verify_resource_sweep("res:/staticdata/skilltags.static", &rf, &sde, &conn);
    verify_resource_sweep(
        "res:/staticdata/skilltreegroups.fsdbinary",
        &rf,
        &sde,
        &conn,
    );
    verify_resource_sweep("res:/staticdata/skyboxeffects.fsdbinary", &rf, &sde, &conn);
    verify_resource_sweep(
        "res:/staticdata/solarsystemcontent.static",
        &rf,
        &sde,
        &conn,
    );
}

#[test]
fn test_sde_sweep_group_8() {
    // parity: sde:res:/staticdata/soundids.fsdbinary, sde:res:/staticdata/spaceobjecthighlights.fsdbinary, sde:res:/staticdata/starmapcache.pickle, sde:res:/staticdata/stationstandingsrestrictions.fsdbinary
    // parity: sde:res:/staticdata/surveys.fsdbinary, sde:res:/staticdata/systems.schema, sde:res:/staticdata/tags.fsdbinary, sde:res:/staticdata/traveleffects.fsdbinary
    // parity: sde:res:/staticdata/triggertypes.static, sde:res:/staticdata/typedogma.fsdbinary, sde:res:/staticdata/uiblinks.fsdbinary, sde:res:/staticdata/uielementtypes.fsdbinary
    // parity: sde:res:/staticdata/uihelppointers.fsdbinary, sde:res:/staticdata/uihidingtemplates.fsdbinary, sde:res:/staticdata/uihighlights.fsdbinary, sde:res:/staticdata/voidspaceencounters.fsdbinary
    // parity: sde:res:/staticdata/warzonespositiondata.fsdbinary, sde:res:/staticdata/wrecks.static
    let db_path = SdeRegistry::default_db_path();
    let sde = SdeRegistry::open(&db_path).expect("Real SDE SQLite database must open");
    let sde_uri = format!("file:{}?immutable=1", db_path.display());
    let conn = Connection::open_with_flags(
        &sde_uri,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )
    .expect("open sde_conn");
    let rf = resfiles_dir();

    eve_testkit::attest(&[
        "sde:res:/staticdata/soundids.fsdbinary",
        "sde:res:/staticdata/spaceobjecthighlights.fsdbinary",
        "sde:res:/staticdata/starmapcache.pickle",
        "sde:res:/staticdata/stationstandingsrestrictions.fsdbinary",
        "sde:res:/staticdata/surveys.fsdbinary",
        "sde:res:/staticdata/systems.schema",
        "sde:res:/staticdata/tags.fsdbinary",
        "sde:res:/staticdata/traveleffects.fsdbinary",
        "sde:res:/staticdata/triggertypes.static",
        "sde:res:/staticdata/typedogma.fsdbinary",
        "sde:res:/staticdata/uiblinks.fsdbinary",
        "sde:res:/staticdata/uielementtypes.fsdbinary",
        "sde:res:/staticdata/uihelppointers.fsdbinary",
        "sde:res:/staticdata/uihidingtemplates.fsdbinary",
        "sde:res:/staticdata/uihighlights.fsdbinary",
        "sde:res:/staticdata/voidspaceencounters.fsdbinary",
        "sde:res:/staticdata/warzonespositiondata.fsdbinary",
        "sde:res:/staticdata/wrecks.static",
    ]);

    verify_resource_sweep("res:/staticdata/soundids.fsdbinary", &rf, &sde, &conn);
    verify_resource_sweep(
        "res:/staticdata/spaceobjecthighlights.fsdbinary",
        &rf,
        &sde,
        &conn,
    );
    verify_resource_sweep("res:/staticdata/starmapcache.pickle", &rf, &sde, &conn);
    verify_resource_sweep(
        "res:/staticdata/stationstandingsrestrictions.fsdbinary",
        &rf,
        &sde,
        &conn,
    );
    verify_resource_sweep("res:/staticdata/surveys.fsdbinary", &rf, &sde, &conn);
    verify_resource_sweep("res:/staticdata/systems.schema", &rf, &sde, &conn);
    verify_resource_sweep("res:/staticdata/tags.fsdbinary", &rf, &sde, &conn);
    verify_resource_sweep("res:/staticdata/traveleffects.fsdbinary", &rf, &sde, &conn);
    verify_resource_sweep("res:/staticdata/triggertypes.static", &rf, &sde, &conn);
    verify_resource_sweep("res:/staticdata/typedogma.fsdbinary", &rf, &sde, &conn);
    verify_resource_sweep("res:/staticdata/uiblinks.fsdbinary", &rf, &sde, &conn);
    verify_resource_sweep("res:/staticdata/uielementtypes.fsdbinary", &rf, &sde, &conn);
    verify_resource_sweep("res:/staticdata/uihelppointers.fsdbinary", &rf, &sde, &conn);
    verify_resource_sweep(
        "res:/staticdata/uihidingtemplates.fsdbinary",
        &rf,
        &sde,
        &conn,
    );
    verify_resource_sweep("res:/staticdata/uihighlights.fsdbinary", &rf, &sde, &conn);
    verify_resource_sweep(
        "res:/staticdata/voidspaceencounters.fsdbinary",
        &rf,
        &sde,
        &conn,
    );
    verify_resource_sweep(
        "res:/staticdata/warzonespositiondata.fsdbinary",
        &rf,
        &sde,
        &conn,
    );
    verify_resource_sweep("res:/staticdata/wrecks.static", &rf, &sde, &conn);
}
