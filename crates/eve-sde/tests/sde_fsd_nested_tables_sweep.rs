//! Raw client-file verification of the nested `.fsdbinary` resources (lists, dicts, inline
//! objects) that `SdeRegistry::load_cfsd_nested` exposes to server logic (`T-2`).
//!
//! Each file is read from `SharedCache/ResFiles/` with its byte size and md5 pinned to the
//! parity ledger. This test walks the `cfsd` hash buckets itself to obtain the header count and
//! every record key (string keys decoded through their own relative offsets), and checks them
//! against the importer's `sde_resources` row and the server table. Full spot records are the
//! client's own `*Loader.so` output (independent of the server's layout tables), and ID fields
//! are cross-checked against the SDE domain tables they reference.

#![forbid(unsafe_code)]

mod raw_client;

use eve_sde::SdeRegistry;
use raw_client::*;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

struct Nested {
    path: &'static str,
    md5: &'static str,
    bytes: u64,
    stride: usize,
    str_key: bool,
    count: usize,
    /// (record key, client-loader record JSON).
    spots: &'static [(&'static str, &'static str)],
}

/// Verifies one resource and returns the server table for further cross-checks.
fn verify(sde: &SdeRegistry, n: &Nested) -> BTreeMap<String, Value> {
    let info = sde
        .resources
        .get(n.path)
        .unwrap_or_else(|| panic!("{} not indexed", n.path));
    assert!(
        info.cdn_relative_path.ends_with(n.md5),
        "{} cdn path",
        n.path
    );
    let f = load_cfsd(&info.cdn_relative_path, n.md5, n.bytes, n.stride);
    assert_eq!(f.header_count, n.count, "{} header count", n.path);
    assert_eq!(
        info.record_count, n.count,
        "{} importer record_count",
        n.path
    );
    let raw_keys: BTreeSet<String> = f
        .records
        .iter()
        .map(|&rec| {
            if n.str_key {
                cfsd_str(&f.raw, u64_at(&f.raw, rec)).expect("string key")
            } else {
                u64_at(&f.raw, rec).to_string()
            }
        })
        .collect();
    assert_eq!(raw_keys.len(), n.count, "{} distinct raw keys", n.path);
    assert!(
        raw_keys.contains(&info.spot),
        "{} importer spot {} not a raw key",
        n.path,
        info.spot
    );
    let table = sde.load_cfsd_nested(n.path).expect("server nested table");
    assert_eq!(
        table.keys().cloned().collect::<BTreeSet<_>>(),
        raw_keys,
        "{} keys",
        n.path
    );
    for (key, json) in n.spots {
        let client: Value = serde_json::from_str(json).expect("spot JSON");
        assert_eq!(table[*key], client, "{} record {key}", n.path);
    }
    table
}

fn ints(v: &Value) -> Vec<u64> {
    v.as_array()
        .expect("array")
        .iter()
        .map(|x| x.as_u64().expect("int"))
        .collect()
}

const GROUP_A: &[Nested] = &[
    Nested {
        path: "res:/staticdata/globaltypes.fsdbinary",
        md5: "c923cc42b5ffe9ca1b3262bd56d54541",
        bytes: 2792,
        stride: 24,
        str_key: false,
        count: 8,
        spots: &[
            (
                "11",
                r#"{"resourceDistribution":{"2073":0.9,"2267":0.0,"2268":0.9,"2270":0.0,"2272":0.0,"2286":0.0,"2287":1.0,"2288":0.95,"2305":1.0,"2306":0.0,"2307":0.0,"2308":0.0,"2309":0.0,"2310":0.0,"2311":0.0}}"#,
            ),
            (
                "13",
                r#"{"resourceDistribution":{"2073":0.0,"2267":0.8,"2268":0.8,"2270":0.0,"2272":0.0,"2286":0.0,"2287":0.0,"2288":0.0,"2305":0.0,"2306":0.0,"2307":0.0,"2308":0.0,"2309":0.95,"2310":1.0,"2311":1.0}}"#,
            ),
        ],
    },
    Nested {
        path: "res:/staticdata/ship_skin_design_component_point_values.fsdbinary",
        md5: "5502664fdb320d92c93522e3a7cb1c8d",
        bytes: 424,
        stride: 24,
        str_key: false,
        count: 3,
        spots: &[
            ("1", r#"{"1":25,"2":50,"3":100,"4":200,"5":400,"6":600}"#),
            ("2", r#"{"1":75,"2":150,"3":250,"4":350,"5":500,"6":750}"#),
        ],
    },
    Nested {
        path: "res:/staticdata/ship_skin_design_tier_thresholds.fsdbinary",
        md5: "509e19d2aa66c0930f9b3b1c974f29bf",
        bytes: 13296,
        stride: 24,
        str_key: false,
        count: 49,
        spots: &[
            (
                "34",
                r#"{"1":1,"10":375,"11":550,"12":825,"13":1225,"14":1825,"15":2725,"16":3400,"17":4250,"18":5300,"19":6625,"2":2,"3":25,"4":50,"5":75,"6":100,"7":125,"8":175,"9":250}"#,
            ),
            (
                "2104",
                r#"{"1":1,"10":550,"11":825,"12":1225,"13":1825,"14":2725,"15":3400,"16":4250,"17":5300,"18":6625,"19":8275,"2":25,"3":50,"4":75,"5":100,"6":125,"7":175,"8":250,"9":375}"#,
            ),
        ],
    },
    Nested {
        path: "res:/staticdata/stationstandingsrestrictions.fsdbinary",
        md5: "bd16fc30da1aec30ca710e20e08d3ce6",
        bytes: 296,
        stride: 24,
        str_key: false,
        count: 2,
        spots: &[
            ("1000438", r#"{"services":{}}"#),
            (
                "500026",
                r#"{"services":{"13":-0.05,"14":3.0,"17":-0.05,"22":-0.05,"23":3.0,"24":3.0,"5":3.0}}"#,
            ),
        ],
    },
    Nested {
        path: "res:/staticdata/npcstructuresettings.fsdbinary",
        md5: "36a4a8c8dbc396d3d7fe827a888a4d05",
        bytes: 296,
        stride: 24,
        str_key: false,
        count: 2,
        spots: &[
            ("78260", r#"{"warFactionIdBased":{"19":{"500011":1.0}}}"#),
            ("79172", r#"{"warFactionIdBased":{"19":{"500010":1.0}}}"#),
        ],
    },
    Nested {
        path: "res:/staticdata/npcstationtaxmodifier.fsdbinary",
        md5: "efe6de492f3f74bbde796023b125842d",
        bytes: 224,
        stride: 24,
        str_key: false,
        count: 1,
        spots: &[(
            "60015187",
            r#"{"warFactionID":{"taxModifierByValue":{"500010":0.10000000149011612,"500011":0.10000000149011612}}}"#,
        )],
    },
    Nested {
        path: "res:/staticdata/community_track_templates.fsdbinary",
        md5: "91771921ed4acdce31dee2da9bcb67b6",
        bytes: 224,
        stride: 32,
        str_key: false,
        count: 1,
        spots: &[(
            "1",
            r#"{"goal":100,"name":"Vanguard Nemesis Event","tale_templates":[466,470,467,471,474,468,472,469,473,475,476,480,477,481,484,478,482,479,483,485]}"#,
        )],
    },
    Nested {
        path: "res:/staticdata/contenttagtypes.fsdbinary",
        md5: "6acda0aea52d1c9390853c3f78d22d29",
        bytes: 200,
        stride: 16,
        str_key: true,
        count: 3,
        spots: &[
            ("activity", r#"{"title":659160}"#),
            ("career_path", r#"{"title":659158}"#),
        ],
    },
    Nested {
        path: "res:/staticdata/contenttags.fsdbinary",
        md5: "4f5245a9f01385c053ba5980253a2023",
        bytes: 5397,
        stride: 40,
        str_key: true,
        count: 50,
        spots: &[
            (
                "feature_acp",
                r#"{"contentTagType":"feature","title":666943}"#,
            ),
            (
                "feature_capsuleer_day",
                r#"{"contentTagType":"feature","description":711412,"title":711413}"#,
            ),
        ],
    },
];

const GROUP_B: &[Nested] = &[
    Nested {
        path: "res:/staticdata/voidspaceencounters.fsdbinary",
        md5: "66d83d13c29b3d71154c269502ab021b",
        bytes: 680,
        stride: 48,
        str_key: false,
        count: 8,
        spots: &[
            (
                "62448",
                r#"{"dungeonIDs":[9264],"encounterDescriptionID":594763,"nebulaGraphicIDs":[25126],"numberOfPlayers":1,"shipRestrictionsDescriptionID":594764,"traceDescriptionID":594766,"traceDescriptionTitleID":594765,"validShipTypeListID":27}"#,
            ),
            (
                "79996",
                r#"{"dungeonIDs":[10589],"encounterDescriptionID":706300,"nebulaGraphicIDs":[22041],"numberOfPlayers":2,"shipRestrictionsDescriptionID":706301,"traceDescriptionID":706302,"traceDescriptionTitleID":706303,"validShipTypeListID":323}"#,
            ),
        ],
    },
    Nested {
        path: "res:/staticdata/skillplans.fsdbinary",
        md5: "38da240ee9fa6fc94f4993218d5aac53",
        bytes: 28788,
        stride: 56,
        str_key: false,
        count: 40,
        spots: &[(
            "33",
            r#"{"descriptionID":585731,"milestones":[{"typeID":523},{"level":2,"typeID":3393}],"name":"AIR - Repair Systems","nameID":585730,"skillRequirements":[{"level":1,"typeID":3392},{"level":1,"typeID":3393},{"level":2,"typeID":3393}]}"#,
        )],
    },
    Nested {
        path: "res:/staticdata/operationtreestructures.fsdbinary",
        md5: "d53da1db8d34dd48c2e5255eb04d525b",
        bytes: 5892,
        stride: 40,
        str_key: false,
        count: 28,
        spots: &[
            (
                "2465",
                r#"{"connections":{"2433":[]},"description":"","name":"Odyssey Experience Scene 2"}"#,
            ),
            (
                "2440",
                r#"{"connections":{"2404":[]},"description":"Combat 3 Tree","name":"Combat 3 Tree (1)"}"#,
            ),
        ],
    },
    Nested {
        path: "res:/staticdata/ship_cosmetic_slot_configurations.fsdbinary",
        md5: "e80fcb9f97f19c62bb4c699d9cfabae4",
        bytes: 952,
        stride: 48,
        str_key: false,
        count: 4,
        spots: &[
            (
                "5",
                r#"{"allow_all_ships":1,"config":[1,2,3,4,5,6,7,8],"name":"Default configuration","priority":3}"#,
            ),
            (
                "8",
                r#"{"config":[1,2,4,5,6,7,8],"name":"NoDetailingSlot","priority":2,"ships":[77283,28844,20185]}"#,
            ),
        ],
    },
    Nested {
        path: "res:/staticdata/objectives.fsdbinary",
        md5: "1a12ce2956e71644a53f9a46870d1e71",
        bytes: 1187,
        stride: 64,
        str_key: false,
        count: 8,
        spots: &[
            (
                "5",
                r#"{"name":"[AIR NPE] 4.3","text":595716,"title":595717,"titleIcon":"res:/UI/Texture/classes/MissionTracker/wreckNPC.png"}"#,
            ),
            (
                "8",
                r#"{"name":"[NES Intro] Do the mission","text":639670,"title":639671,"titleIcon":"res:/UI/Texture/classes/MissionTracker/tracker_dropoff.png"}"#,
            ),
        ],
    },
    Nested {
        path: "res:/staticdata/operationsites.fsdbinary",
        md5: "e385c129f35bad83e257296853a53475",
        bytes: 2159,
        stride: 56,
        str_key: false,
        count: 21,
        spots: &[
            (
                "15",
                r#"{"disableWarpToEntryPointRedirect":0,"dungeonIDs":{"amarr":6423,"caldari":6423,"gallente":6423,"minmatar":6423},"id":15,"name":"Combat Site 6","shouldSpawn":0,"spawning":"sameSolarSystem","title":536169}"#,
            ),
            (
                "39",
                r#"{"disableWarpToEntryPointRedirect":0,"dungeonIDs":{"amarr":6411,"caldari":6411,"gallente":6411,"minmatar":6411},"id":39,"name":"Combat Site 1","shouldSpawn":0,"spawning":"sameSolarSystem","title":542602}"#,
            ),
        ],
    },
    Nested {
        path: "res:/staticdata/petitionproperties.fsdbinary",
        md5: "96374ca99ac56228e223d4c91f021c25",
        bytes: 2713,
        stride: 64,
        str_key: false,
        count: 12,
        spots: &[
            (
                "12",
                r#"{"description":"Operating System","descriptionID":62189,"inputInfo":"Operating System","inputType":"DropDown","obsolete":0,"orderWeight":1,"outputTemplate":"propvalue","propertyName":"OS","required":1}"#,
            ),
            (
                "9",
                r#"{"description":"Web Address","descriptionID":62186,"inputInfo":"http://","inputType":"EditBox","obsolete":0,"orderWeight":1,"outputTemplate":"<A href=\"propvalue\" target=\"blank\">WebPage</A>","propertyName":"WebAddress","required":1}"#,
            ),
        ],
    },
    Nested {
        path: "res:/staticdata/operationcategories.fsdbinary",
        md5: "98004a30f33a63dfdb96cac15d2ace7b",
        bytes: 1088,
        stride: 56,
        str_key: false,
        count: 8,
        spots: &[
            (
                "32",
                r#"{"description":523043,"iconPath":"res:/UI/Texture/defaultFace.jpg","iskReward":0,"name":"Death Pod","notificationDescription":523045,"notificationTitle":523044,"operationTrees":[172],"title":523042}"#,
            ),
            (
                "31",
                r#"{"description":523039,"iconPath":"res:/UI/Texture/defaultFace.jpg","iskReward":0,"name":"Death Ship","notificationDescription":523041,"notificationTitle":523040,"operationTrees":[171],"title":523038}"#,
            ),
        ],
    },
    Nested {
        path: "res:/staticdata/expertsystems.fsdbinary",
        md5: "1beaa56c2f509ab9a824f22f98e4e892",
        bytes: 58696,
        stride: 56,
        str_key: false,
        count: 59,
        spots: &[
            (
                "57197",
                r#"{"durationDays":30,"esHidden":1,"esRetired":1,"internalName":"QA_ES_test","skillsGranted":{"3450":5}}"#,
            ),
            (
                "57192",
                r#"{"durationDays":1,"esHidden":1,"esRetired":1,"internalName":"QA_ES_omega_only","skillsGranted":{"13278":4}}"#,
            ),
        ],
    },
];

#[test]
fn test_t2_sde_fsdbinary_nested_dicts_economy_and_content_tags() {
    // parity: sde:res:/staticdata/globaltypes.fsdbinary, sde:res:/staticdata/ship_skin_design_component_point_values.fsdbinary
    // parity: sde:res:/staticdata/ship_skin_design_tier_thresholds.fsdbinary, sde:res:/staticdata/stationstandingsrestrictions.fsdbinary
    // parity: sde:res:/staticdata/npcstructuresettings.fsdbinary, sde:res:/staticdata/npcstationtaxmodifier.fsdbinary
    // parity: sde:res:/staticdata/community_track_templates.fsdbinary, sde:res:/staticdata/contenttagtypes.fsdbinary
    // parity: sde:res:/staticdata/contenttags.fsdbinary
    let sde = SdeRegistry::open(&SdeRegistry::default_db_path()).expect("real V24.01 SDE");
    let t: Vec<BTreeMap<String, Value>> = GROUP_A.iter().map(|n| verify(&sde, n)).collect();
    // globaltypes: every resourceDistribution key is an SDE planet-resource type, shares in [0, 1].
    for g in t[0].values() {
        for (tid, share) in g["resourceDistribution"].as_object().unwrap() {
            assert!(
                sde.types.contains_key(&tid.parse::<u32>().unwrap()),
                "globaltypes type {tid}"
            );
            assert!((0.0..=1.0).contains(&share.as_f64().unwrap()));
        }
    }
    assert!(
        t[0].keys()
            .all(|k| sde.types.contains_key(&k.parse::<u32>().unwrap())),
        "planet types"
    );
    // point values and tier thresholds are strictly increasing per rarity / tier.
    for table in [&t[1], &t[2]] {
        for v in table.values() {
            let mut by_key: Vec<(u32, u64)> = v
                .as_object()
                .unwrap()
                .iter()
                .map(|(k, x)| (k.parse().unwrap(), x.as_u64().unwrap()))
                .collect();
            by_key.sort();
            assert!(
                by_key.windows(2).all(|w| w[0].1 < w[1].1),
                "increasing values {by_key:?}"
            );
        }
    }
    // stationstandingsrestrictions keys are an SDE faction and an SDE NPC corporation.
    assert!(sde.factions.contains_key(&500_026) && sde.npc_corporations.contains_key(&1_000_438));
    assert_eq!(t[3]["500026"]["services"].as_object().unwrap().len(), 7);
    // npcstructuresettings: keyed by SDE types; inner keys are SDE factions.
    for (k, v) in &t[4] {
        assert!(sde.types.contains_key(&k.parse::<u32>().unwrap()));
        for inner in v["warFactionIdBased"].as_object().unwrap().values() {
            assert!(inner
                .as_object()
                .unwrap()
                .keys()
                .all(|f| sde.factions.contains_key(&f.parse::<u32>().unwrap())));
        }
    }
    // npcstationtaxmodifier is keyed by an SDE station.
    assert!(sde.stations.contains_key(&60_015_187));
    assert_eq!(ints(&t[6]["1"]["tale_templates"]).len(), 20);
    // contenttags reference a contenttagtypes key.
    assert!(t[8]
        .values()
        .all(|v| t[7].contains_key(v["contentTagType"].as_str().unwrap())));
    assert_eq!(
        t[8].values().filter(|v| v.get("icon").is_some()).count(),
        35
    );

    eve_testkit::attest(&[
        "sde:res:/staticdata/globaltypes.fsdbinary",
        "sde:res:/staticdata/ship_skin_design_component_point_values.fsdbinary",
        "sde:res:/staticdata/ship_skin_design_tier_thresholds.fsdbinary",
        "sde:res:/staticdata/stationstandingsrestrictions.fsdbinary",
        "sde:res:/staticdata/npcstructuresettings.fsdbinary",
        "sde:res:/staticdata/npcstationtaxmodifier.fsdbinary",
        "sde:res:/staticdata/community_track_templates.fsdbinary",
        "sde:res:/staticdata/contenttagtypes.fsdbinary",
        "sde:res:/staticdata/contenttags.fsdbinary",
    ]);
}

#[test]
fn test_t2_sde_fsdbinary_nested_skills_operations_and_encounters() {
    // parity: sde:res:/staticdata/voidspaceencounters.fsdbinary, sde:res:/staticdata/skillplans.fsdbinary
    // parity: sde:res:/staticdata/operationtreestructures.fsdbinary, sde:res:/staticdata/ship_cosmetic_slot_configurations.fsdbinary
    // parity: sde:res:/staticdata/objectives.fsdbinary, sde:res:/staticdata/operationsites.fsdbinary
    // parity: sde:res:/staticdata/petitionproperties.fsdbinary, sde:res:/staticdata/operationcategories.fsdbinary
    // parity: sde:res:/staticdata/expertsystems.fsdbinary
    let sde = SdeRegistry::open(&SdeRegistry::default_db_path()).expect("real V24.01 SDE");
    let t: Vec<BTreeMap<String, Value>> = GROUP_B.iter().map(|n| verify(&sde, n)).collect();
    // voidspaceencounters: dungeons and type lists exist in the SDE.
    for v in t[0].values() {
        assert!(ints(&v["dungeonIDs"])
            .iter()
            .all(|d| sde.dungeons.contains_key(&(*d as u32))));
        assert!(sde
            .type_lists
            .contains_key(&(v["validShipTypeListID"].as_u64().unwrap() as u32)));
    }
    // skillplans: every milestone / requirement type is an SDE type; requirement levels 1..=5.
    let mut reqs = 0usize;
    for v in t[1].values() {
        for m in v["milestones"].as_array().unwrap() {
            assert!(sde
                .types
                .contains_key(&(m["typeID"].as_u64().unwrap() as u32)));
        }
        for r in v["skillRequirements"].as_array().unwrap() {
            assert!(
                sde.types[&(r["typeID"].as_u64().unwrap() as u32)].category_id == 16,
                "skill type"
            );
            assert!((1..=5).contains(&r["level"].as_u64().unwrap()));
            reqs += 1;
        }
    }
    assert!(
        reqs > t[1].len(),
        "skill requirements decoded for {} plans",
        t[1].len()
    );
    // ship_cosmetic_slot_configurations: listed ships are SDE ship hulls (category 6).
    for v in t[3].values() {
        if let Some(ships) = v.get("ships") {
            assert!(ints(ships).iter().all(|s| sde
                .types
                .get(&(*s as u32))
                .is_some_and(|ty| ty.category_id == 6)));
        }
    }
    assert_eq!(t[3]["5"]["allow_all_ships"], Value::from(1));
    // operationsites: every per-race dungeon id is an SDE dungeon; key equals its uid=491563(sergeya) gid=89939(primarygroup) groups=89939(primarygroup),89275(plx-alerts-x20),95981(eval-metrics-other-automatic),77281(sslvpn-users),102941(desktop-onboarding-access),94337(ggrcq-app-risk-readers-pilot),103411(xr-winscope-read),70970(abi-users-qa),90434(youtube-cobalt-x20-access),78338(gwscatcher-dashboard-users),81472(yt-streampunk),86849(patternator-users),102050(ml-gemini-users-tier-1),103541(cerr-gmac-crowdstrike-tamper-protection),87558(eng-wide),103596(cerr-rollout-3),90899(android-tools-tvc),91041(terraform-readers),75209(android-gsearch),97231(winscope-read),103289(cerr-gmac-local-user-restriction),102535(agsa-folly-x20-access),85841(hw-configs-ro),102136(chrome-only-control),90537(guitar-x20-readers),103295(cerr-gmac-disallow-startup-disk),103568(software-clients-ban-rollout3),79149(yt-tv-html5-eng),97552(appliedvr-digital-twin-access),90673(tap-x20-readers),90387(guitar-tools-readers),25628(youtube),83042(gelato-users),92741(primetime-users),95333(ytk-ml-launch-report-readers),96089(genx-readers),90518(testing-x20-readers),100870(offsite-users),82712(chip-hwlibs-all),90321(adf-eligible),94344(ggrcq-app-sod-users-pilot),94372(youtube-classification-launch-report-readers),103345(cerr-network-file-share-disabled-glinux),94373(youtube-classification-launch-report-readers-auto-sphinx),92487(primetime-US),95521(laser-hawkeye-conflict-review-files),81169(googlenow-team),98356(papercut-all-users),90558(go-x20-readers),91784(proactive-team-sphinx),104177(patternator-access-users),94332(ggrcq-app-scoping-objects-editors-pilot),99388(test-ncde-approved-windows-policy),94335(ggrcq-app-control-creators-pilot),103328(rix-cloudtop-rollout),89266(print_access_exception_groups),90384(boq-tools-readers),66560(youtube-eng),94376(brain-pensa-chimera-art),105708(mac-mail-app-ban-rollout),98646(ncde-approved),103464(cerr-gmac-persistent-crowdstrike),103304(cerr-rollout-2),103323(cerr-gmac-recoveryos-password-enforced),87986(opensource-reviewers),103125(cerr-rollout),94338(ggrcq-app-contract-editors-pilot),103421(corp-airlock-enforcement),101387(ecp-config-deployment),94331(ggrcq-app-users-pilot),5000(eng),90415(buildifier-readers),93719(srlregistry-x20-readers),91952(pharos-users),94336(ggrcq-app-risk-admins-pilot),103963(gcloud-mtls-grpc-deployment),97829(data-glows-lrs-result-readers),103121(cerr-gmac-persistent-mdm),94835(yt-classifier-acl-emex-global-readers-corp-sphinx-auto),91471(gmac-trusted-testers),102030(dcservice-apk-read-acl),90968(glinux-autolist),91675(tidy-dart-readers),92930(aae-setup-wizard-apk-access),103343(cerr-network-file-share-disabled-windows),97644(fraud-review-opaque-access),104715(lheller-timeline-readers),980(corpam_login_users),94342(ggrcq-app-requirement-editors-pilot),66187(eng-managers),103142(cerr-icloud-drive-sync-blocked),103580(cerr-managed-software-enforcement),92188(python-team-x20-readers),89046(plx-x20-readers),103745(gcloud-ecp-http-proxy-deployment),103175(cerr-handoff-disabled),103291(cerr-gmac-inbuilt-vpn-restriction),91199(gmac-autolist),94191(appliedvr-x20),5825(managers-only),103311(cerr-rel2-percent-75),102426(youtube-ios-x20-readers),91607(proactive-team),103091(cerr-rdp-data-transfer-blocked),101493(monarch-donations-tool-users),91674(android-setup-wizard-apk-access),81448(streampunk),103096(cerr-webprotect-enforcements),90338(infra-planning-read-access),103462(cerr-gmac-inbuilt-vpn-ui-restriction),99567(test-ncde-approved-windows-employees-policy),93743(splice-join-allowed),90746(mobileharness-user-x20),82189(chip-hwlibs),103090(cerr-win-bluetooth-file-transfer-block),94333(ggrcq-app-compliance-program-managers-pilot),101457(pixel-setup-wizard-apk-access),87815(qst-users),981(corpam_root_users),103654(cerr-gmac-warden),79982(moglog-access),103093(cerr-universal-control-disabled),99309(dg-annotations-field-level-conflict-files),105006(corp-airlock-enforcement-windows),89971(piper-group-p4users),12(everyone),61(localaccounts),80(admin),81(_appserveradm),98(_lpadmin),398(com.apple.access_screensharing),399(com.apple.access_ssh),92278(android-build-access),82193(elearning-full-time-only),103631(cerr-glinux-crowdstrike-prevention-mode),100161(tabaccess-CXLabWaveDashboard_interactor),86931(gpg-docs-access),83243(machdoc-cli-access),94340(ggrcq-app-objective-editors-pilot),70967(abi-users),90128(copycat-debugger-users),94341(ggrcq-app-policy-editors-pilot),103080(gmac-filesharing-enforce),90578(Fishychip_forced_upgrade),66688(hwops-docs-access),77056(tracker-users),94339(ggrcq-app-directive-editors-pilot),94083(modem-apps-x20),103293(cerr-gmac-disallow-firewall-disablement),94334(ggrcq-app-control-admins-pilot),87253(growth-analysis-reader),93858(frameworks-protodb-x20-readers),96792(move-ml-x20-read),88209(yt-channel-map-sphinx),101389(gcloud-mTLS-deployment),76591(mobileharness-user),99581(tabaccess-employees),94330(ggrcq-app-admins-pilot),94278(arbo-dpor),93214(pi2gslb-tool-readers),86035(netdeploy-library-access-nonconf),93529(stadia-dogfood-ipa-access),93368(aaae-artifact-readers),79910(theloop-users),84796(gfiber-scm-vendor-alu),87465(google-camera-android-qa-accounts),88414(nest-engtraining),91750(tiktok-tools-read),94343(ggrcq-app-threat-editors-pilot),103143(cerr-notes-sync-disabled),82072(dremelguide-additions),103320(cerr-gmac-disallow-local-login),90680(mendel-x20-readers),96853(dms-guest),103594(cerr-rel3-percent-75),99553(CoreComputeAnalytics_broadaccess_noncloud),102286(krispop-jps-metrics),100113(tabaccess-GMSCore_interactor),103465(cerr-gmac-persistent-santa),33(_appstore),100(_lpoperator),204(_developer),250(_analyticsusers),395(com.apple.access_ftp),400(com.apple.access_remote_ae).
    for (k, v) in &t[5] {
        assert_eq!(v["id"].as_u64().unwrap().to_string(), *k);
        for d in v["dungeonIDs"].as_object().unwrap().values() {
            assert!(
                sde.dungeons.contains_key(&(d.as_u64().unwrap() as u32)),
                "operation site dungeon {d}"
            );
        }
    }
    // operationcategories reference operationtreestructures keys.
    for v in t[7].values() {
        assert!(ints(&v["operationTrees"])
            .iter()
            .all(|id| t[2].contains_key(&id.to_string())));
    }
    // expertsystems: every granted skill is an SDE skill at level 1..=5.
    let mut granted = 0usize;
    for v in t[8].values() {
        for (sid, lvl) in v["skillsGranted"].as_object().unwrap() {
            assert_eq!(
                sde.types[&sid.parse::<u32>().unwrap()].category_id,
                16,
                "expert system skill {sid}"
            );
            assert!((1..=5).contains(&lvl.as_u64().unwrap()));
            granted += 1;
        }
    }
    assert!(granted >= t[8].len());

    eve_testkit::attest(&[
        "sde:res:/staticdata/voidspaceencounters.fsdbinary",
        "sde:res:/staticdata/skillplans.fsdbinary",
        "sde:res:/staticdata/operationtreestructures.fsdbinary",
        "sde:res:/staticdata/ship_cosmetic_slot_configurations.fsdbinary",
        "sde:res:/staticdata/objectives.fsdbinary",
        "sde:res:/staticdata/operationsites.fsdbinary",
        "sde:res:/staticdata/petitionproperties.fsdbinary",
        "sde:res:/staticdata/operationcategories.fsdbinary",
        "sde:res:/staticdata/expertsystems.fsdbinary",
    ]);
}
