//! Raw client-file verification of the final three nested `.fsdbinary` resources
//! (`contributionmethods`, `dungeonobjectives`, `surveys`) and full 213-resource `T-2` SDE
//! loader completeness (`T-2`).

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
    spots: &'static [(&'static str, &'static str)],
}

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

const REMAINING: &[Nested] = &[
    Nested {
        path: "res:/staticdata/contributionmethods.fsdbinary",
        md5: "c50469948a24b7136e29a1b856a959c3",
        bytes: 12800,
        stride: 72,
        str_key: true,
        count: 22,
        spots: &[
            (
                "ManufactureItem",
                r#"{"contentTags":["career_path_industrialist","activity_manufacturing"],"descriptionID":665412,"icon":"production","name":"Manufacture Item","parameters":[{"icon":"inventory","info":"UI/GameGoalConfig/Parameters/ManufactureItemTypeDescription","isRequired":1,"parameterKey":"item_type","parameterType":"manufacturable_item_type","title":"UI/GameGoalConfig/Parameters/DeliverItemTypeTitle"},{"icon":"user","info":"UI/GameGoalConfig/Parameters/ManufactureOwnerDescription","isRequired":1,"parameterKey":"owner","parameterType":"on_behalf_of","title":"UI/GameGoalConfig/Parameters/ManufactureOwnerTitle"},{"icon":"location","info":"UI/GameGoalConfig/Parameters/ManufactureLocationDescription","isRequired":0,"parameterKey":"facility_location","parameterType":"corporation_office_location","title":"UI/GameGoalConfig/Parameters/ManufactureLocationTitle"}],"progressDescriptionID":696246,"rewardingDescriptionID":695578,"targetValueDescriptionID":695579,"titleID":665394,"unit":138}"#,
            ),
            (
                "CaptureFWComplex",
                r#"{"contentTags":["career_path_soldier_of_fortune","activity_combat","feature_factional_warfare"],"descriptionID":665426,"icon":"attack_fw_complex","name":"Capture FW Complex","parameters":[{"icon":"corporation_management","info":"UI/GameGoalConfig/Parameters/CaptureFWComplexesFactionDescription","isRequired":0,"parameterKey":"faction","parameterType":"empire_faction","title":"UI/GameGoalConfig/Parameters/CaptureFWComplexesFactionTitle"},{"icon":"location","info":"UI/GameGoalConfig/Parameters/CaptureFWComplexesSolarSystemDescription","isRequired":0,"parameterKey":"solar_system","parameterType":"location","title":"UI/GameGoalConfig/Parameters/solarSystem"},{"icon":"fw_complex_type","info":"UI/GameGoalConfig/Parameters/CaptureFWComplexesComplexTypeDescription","isRequired":0,"parameterKey":"complex_type","parameterType":"factional_warfare_complex_type","title":"UI/GameGoalConfig/Parameters/CaptureFWComplexesComplexTypeTitle"}],"progressDescriptionID":696248,"rewardingDescriptionID":695574,"targetValueDescriptionID":695575,"titleID":665425}"#,
            ),
            (
                "DamageShip",
                r#"{"contentTags":["career_path_soldier_of_fortune","activity_combat"],"descriptionID":659202,"icon":"damage","name":"DamageShip","parameters":[{"icon":"location","info":"UI/GameGoalConfig/Parameters/DamageShipLocationDescription","isRequired":0,"maxEntries":10,"parameterKey":"solar_system","parameterType":"location","title":"UI/GameGoalConfig/Parameters/LocationsTitle"},{"icon":"spaceship_command","info":"UI/GameGoalConfig/Parameters/DamageShipShipDescription","isRequired":0,"maxEntries":10,"parameterKey":"ship","parameterType":"ship_type","title":"UI/GameGoalConfig/Parameters/ShipMixedTitle"},{"icon":"pilot_or_organization","info":"UI/GameGoalConfig/Parameters/DamageShipOrganizationDescription","isRequired":0,"maxEntries":10,"parameterKey":"organization","parameterType":"identity","title":"UI/GameGoalConfig/Parameters/OwnerIdentitiesTitle"}],"progressDescriptionID":696243,"rewardingDescriptionID":695563,"targetValueDescriptionID":695564,"titleID":659201,"unit":113}"#,
            ),
        ],
    },
    Nested {
        path: "res:/staticdata/dungeonobjectives.fsdbinary",
        md5: "a9f9e9bf643e0411d983cdd8a3552ea5",
        bytes: 8024,
        stride: 56,
        str_key: false,
        count: 85,
        spots: &[
            (
                "1",
                r#"{"clientObjectives":{"blackboardParameters":[{"parameterKey":"some_server_value_to_client"},{"parameterKey":"objective_timer"},{"defaultValue":"something random","parameterKey":"random_client_key"}],"nodeGraphID":1,"objectiveChainID":26,"overrides":{"destroy_targets":{"description":"UI/Objectives/Descriptions/HoldOut","title":"UI/Objectives/Titles/Fight"}}}}"#,
            ),
            (
                "24",
                r#"{"clientObjectives":{"blackboardParameters":[{"defaultValue":[1194449,1194460],"parameterKey":"dungeon_object_ids"}],"objectiveChainID":75}}"#,
            ),
            (
                "32",
                r#"{"clientObjectives":{"blackboardParameters":[{"defaultValue":[1234327,1234339],"parameterKey":"dungeon_object_ids"}],"objectiveChainID":75}}"#,
            ),
        ],
    },
    Nested {
        path: "res:/staticdata/surveys.fsdbinary",
        md5: "60ecb0ecb7902d555c8b42e1f16fa06a",
        bytes: 480,
        stride: 104,
        str_key: false,
        count: 1,
        spots: &[(
            "28",
            r#"{"apiClassName":"TwelveTraitsAPI","claimLabelID":577342,"description":"A psychological traits survey for 2021","endDate":"25/04/2021","gracePeriod":30,"itemRewards":[],"minAccountAgeHours":24,"name":"12traits 2021","resPath":"res:/UI/Texture/classes/Surveys/survey2021.jpg","startDate":"8/04/2021","url":"https://surveys.12traits.com/s3/CCP-EVE-Online?playerid=%s","vgsRewards":[5003]}"#,
        )],
    },
];

#[test]
fn test_t2_sde_fsdbinary_contribution_methods_dungeon_objectives_and_surveys() {
    // parity: sde:res:/staticdata/contributionmethods.fsdbinary
    // parity: sde:res:/staticdata/dungeonobjectives.fsdbinary
    // parity: sde:res:/staticdata/surveys.fsdbinary
    // parity: plan:T-2
    let sde = SdeRegistry::open(&SdeRegistry::default_db_path()).expect("real V24.01 SDE");
    let t: Vec<BTreeMap<String, Value>> = REMAINING.iter().map(|n| verify(&sde, n)).collect();

    assert_eq!(sde.contribution_methods().expect("contribution_methods"), t[0]);
    assert_eq!(sde.dungeon_objectives().expect("dungeon_objectives"), t[1]);
    assert_eq!(sde.surveys().expect("surveys"), t[2]);

    // contributionmethods: every contentTag references a real contenttags key.
    let content_tags = sde
        .load_cfsd_nested("res:/staticdata/contenttags.fsdbinary")
        .expect("contenttags");
    for method in t[0].values() {
        assert!(method["parameters"].is_array());
        if let Some(tags) = method.get("contentTags") {
            assert!(tags
                .as_array()
                .unwrap()
                .iter()
                .all(|tag| content_tags.contains_key(tag.as_str().unwrap())));
        }
    }

    // dungeonobjectives: every objectiveChainID exists in objectivechains.fsdbinary.
    let chains = sde
        .load_cfsd_nested("res:/staticdata/objectivechains.fsdbinary")
        .expect("objectivechains");
    for obj in t[1].values() {
        if let Some(chain_id) = obj
            .get("clientObjectives")
            .and_then(|co| co.get("objectiveChainID"))
            .and_then(Value::as_i64)
        {
            assert!(
                chains.contains_key(&chain_id.to_string()),
                "missing objectiveChainID {chain_id}"
            );
        }
    }

    // surveys: record 28 matches the 2021 TwelveTraits survey metadata.
    assert_eq!(t[2]["28"]["apiClassName"], "TwelveTraitsAPI");
    assert_eq!(t[2]["28"]["gracePeriod"], 30);
    assert_eq!(t[2]["28"]["minAccountAgeHours"], 24);
    assert_eq!(sde.resources.len(), 213);

    eve_testkit::attest(&[
        "sde:res:/staticdata/contributionmethods.fsdbinary",
        "sde:res:/staticdata/dungeonobjectives.fsdbinary",
        "sde:res:/staticdata/surveys.fsdbinary",
        "plan:T-2",
    ]);
}
