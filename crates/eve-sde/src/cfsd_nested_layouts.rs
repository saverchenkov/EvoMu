//! Layouts of the nested `.fsdbinary` resources decoded by
//! [`SdeRegistry::load_cfsd_nested`](crate::SdeRegistry::load_cfsd_nested) (`T-2`).
//!
//! Offsets, encodings and optional-mask bits were established against the client's own
//! per-resource `*Loader.so` output; within each object, 8-byte attributes (strings, lists,
//! dicts) come first, then 4-byte and 1-byte attributes, each group in client attribute-name
//! order, followed by the optional-field mask.

#![forbid(unsafe_code)]

use super::cfsd_nested_tables::{CfsdAttr, CfsdNestedLayout, CfsdObject, CfsdType};
use CfsdType::{Bool, Dict, List, Obj, Str, F32, F64, U32, U64};

const fn a(name: &'static str, offset: usize, ty: CfsdType, bit: u32) -> CfsdAttr {
    CfsdAttr {
        name,
        offset,
        ty,
        bit,
    }
}

const fn obj(stride: usize, mask_offset: Option<usize>, attrs: &'static [CfsdAttr]) -> CfsdObject {
    CfsdObject {
        stride,
        mask_offset,
        attrs,
    }
}

const fn rec(
    res_path: &'static str,
    key: CfsdType,
    record: CfsdObject,
    bare: bool,
) -> CfsdNestedLayout {
    CfsdNestedLayout {
        res_path,
        key,
        record,
        bare,
    }
}

const U32_LIST: CfsdType = List(&U32, 4);

static MILESTONE: CfsdObject = obj(
    12,
    Some(8),
    &[a("level", 0, U32, 1), a("typeID", 4, U32, 0)],
);
static SKILL_REQ: CfsdObject = obj(8, None, &[a("level", 0, U32, 0), a("typeID", 4, U32, 0)]);
static TAX_BY_VALUE: CfsdObject = obj(
    16,
    None,
    &[a("taxModifierByValue", 0, Dict(&U32, &F32, 8, 4), 0)],
);
static RACE_DUNGEONS: CfsdObject = obj(
    16,
    None,
    &[
        a("amarr", 0, U32, 0),
        a("caldari", 4, U32, 0),
        a("gallente", 8, U32, 0),
        a("minmatar", 12, U32, 0),
    ],
);

pub static CFSD_NESTED_LAYOUTS: &[CfsdNestedLayout] = &[
    rec(
        "res:/staticdata/globaltypes.fsdbinary",
        U64,
        obj(
            24,
            None,
            &[a("resourceDistribution", 8, Dict(&U32, &F64, 16, 8), 0)],
        ),
        false,
    ),
    rec(
        "res:/staticdata/ship_skin_design_component_point_values.fsdbinary",
        U64,
        obj(24, None, &[a("value", 8, Dict(&U32, &U32, 8, 4), 0)]),
        true,
    ),
    rec(
        "res:/staticdata/ship_skin_design_tier_thresholds.fsdbinary",
        U64,
        obj(24, None, &[a("value", 8, Dict(&U32, &U32, 8, 4), 0)]),
        true,
    ),
    rec(
        "res:/staticdata/stationstandingsrestrictions.fsdbinary",
        U64,
        obj(24, None, &[a("services", 8, Dict(&U32, &F64, 16, 8), 0)]),
        false,
    ),
    rec(
        "res:/staticdata/npcstructuresettings.fsdbinary",
        U64,
        obj(
            24,
            None,
            &[a(
                "warFactionIdBased",
                8,
                Dict(&U32, &Dict(&U32, &F32, 8, 4), 24, 8),
                0,
            )],
        ),
        false,
    ),
    rec(
        "res:/staticdata/npcstationtaxmodifier.fsdbinary",
        U64,
        obj(
            24,
            None,
            &[a("value", 8, Dict(&Str, &Obj(&TAX_BY_VALUE), 24, 8), 0)],
        ),
        true,
    ),
    rec(
        "res:/staticdata/community_track_templates.fsdbinary",
        U64,
        obj(
            32,
            None,
            &[
                a("name", 8, Str, 0),
                a("tale_templates", 16, U32_LIST, 0),
                a("goal", 24, U32, 0),
            ],
        ),
        false,
    ),
    rec(
        "res:/staticdata/contenttagtypes.fsdbinary",
        Str,
        obj(16, None, &[a("title", 8, U32, 0)]),
        false,
    ),
    rec(
        "res:/staticdata/contenttags.fsdbinary",
        Str,
        obj(
            40,
            Some(32),
            &[
                a("contentTagType", 8, Str, 0),
                a("icon", 16, Str, 2),
                a("description", 24, U32, 1),
                a("title", 28, U32, 0),
            ],
        ),
        false,
    ),
    rec(
        "res:/staticdata/voidspaceencounters.fsdbinary",
        U64,
        obj(
            48,
            None,
            &[
                a("dungeonIDs", 8, U32_LIST, 0),
                a("nebulaGraphicIDs", 16, U32_LIST, 0),
                a("encounterDescriptionID", 24, U32, 0),
                a("numberOfPlayers", 28, U32, 0),
                a("shipRestrictionsDescriptionID", 32, U32, 0),
                a("traceDescriptionID", 36, U32, 0),
                a("traceDescriptionTitleID", 40, U32, 0),
                a("validShipTypeListID", 44, U32, 0),
            ],
        ),
        false,
    ),
    rec(
        "res:/staticdata/skillplans.fsdbinary",
        U64,
        obj(
            56,
            Some(52),
            &[
                a("milestones", 8, List(&Obj(&MILESTONE), 12), 0),
                a("name", 16, Str, 0),
                a("skillRequirements", 24, List(&Obj(&SKILL_REQ), 8), 0),
                a("careerPathID", 32, U32, 1),
                a("descriptionID", 36, U32, 0),
                a("factionID", 40, U32, 4),
                a("nameID", 44, U32, 0),
                a("npcCorporationDivision", 48, U32, 64),
            ],
        ),
        false,
    ),
    rec(
        "res:/staticdata/operationtreestructures.fsdbinary",
        U64,
        obj(
            40,
            None,
            &[
                a("connections", 8, Dict(&U32, &U32_LIST, 16, 8), 0),
                a("description", 24, Str, 0),
                a("name", 32, Str, 0),
            ],
        ),
        false,
    ),
    rec(
        "res:/staticdata/ship_cosmetic_slot_configurations.fsdbinary",
        U64,
        obj(
            48,
            Some(40),
            &[
                a("config", 8, U32_LIST, 0),
                a("name", 16, Str, 0),
                a("ships", 24, U32_LIST, 2),
                a("priority", 32, U32, 0),
                a("allow_all_ships", 36, Bool, 1),
            ],
        ),
        false,
    ),
    rec(
        "res:/staticdata/objectives.fsdbinary",
        U64,
        obj(
            64,
            Some(56),
            &[
                a("name", 24, Str, 0),
                a("titleIcon", 32, Str, 0),
                a("dungeonID", 40, U32, 2),
                a("iconTypeID", 44, U32, 8),
                a("text", 48, U32, 0),
                a("title", 52, U32, 0),
            ],
        ),
        false,
    ),
    rec(
        "res:/staticdata/operationsites.fsdbinary",
        U64,
        obj(
            56,
            None,
            &[
                a("name", 8, Str, 0),
                a("spawning", 16, Str, 0),
                a("dungeonIDs", 24, Obj(&RACE_DUNGEONS), 0),
                a("id", 40, U32, 0),
                a("title", 44, U32, 0),
                a("disableWarpToEntryPointRedirect", 48, Bool, 0),
                a("shouldSpawn", 49, Bool, 0),
            ],
        ),
        false,
    ),
    rec(
        "res:/staticdata/petitionproperties.fsdbinary",
        U64,
        obj(
            64,
            None,
            &[
                a("description", 8, Str, 0),
                a("inputInfo", 16, Str, 0),
                a("inputType", 24, Str, 0),
                a("outputTemplate", 32, Str, 0),
                a("propertyName", 40, Str, 0),
                a("descriptionID", 48, U32, 0),
                a("orderWeight", 52, U32, 0),
                a("obsolete", 56, Bool, 0),
                a("required", 57, Bool, 0),
            ],
        ),
        false,
    ),
    rec(
        "res:/staticdata/operationcategories.fsdbinary",
        U64,
        obj(
            56,
            None,
            &[
                a("iconPath", 8, Str, 0),
                a("name", 16, Str, 0),
                a("operationTrees", 24, U32_LIST, 0),
                a("description", 32, U32, 0),
                a("iskReward", 36, U32, 0),
                a("notificationDescription", 40, U32, 0),
                a("notificationTitle", 44, U32, 0),
                a("title", 48, U32, 0),
            ],
        ),
        false,
    ),
    rec(
        "res:/staticdata/expertsystems.fsdbinary",
        U64,
        obj(
            56,
            Some(48),
            &[
                a("associatedShipTypes", 8, U32_LIST, 1),
                a("internalName", 16, Str, 0),
                a("skillsGranted", 24, Dict(&U32, &U32, 8, 4), 0),
                a("durationDays", 40, U32, 0),
                a("esHidden", 44, Bool, 0),
                a("esRetired", 45, Bool, 0),
            ],
        ),
        false,
    ),
];
