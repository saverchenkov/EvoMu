//! Layouts of the nested `.fsdbinary` resources with tagged unions, inline fixed arrays and
//! single top-level objects (agency operations, objectives, node graphs, conversations, crates,
//! cosmetics, login campaigns), decoded by
//! [`SdeRegistry::load_cfsd_nested`](crate::SdeRegistry::load_cfsd_nested) (`T-2`).
//!
//! Offsets, encodings, optional-mask bits and union variant tags were established against the
//! client's own per-resource `*Loader.so` output (every record of every file decoded and compared
//! value-for-value). Union variant tags are per-file: only tags observed in the V24.01 file are
//! listed, and any other tag fails the decode loudly. Lists that are empty in every client record
//! (e.g. `operations.skillRewards`) are decoded with a `u32` element encoding; their element type
//! is not observable in V24.01 data.

#![forbid(unsafe_code)]

use super::cfsd_nested_layouts::{a, obj, rec, STR_LIST, U32_LIST};
use super::cfsd_nested_tables::{CfsdNestedLayout, CfsdObject, CfsdType};
use CfsdType::{Bool, Dict, F32Arr, F64Arr, List, Obj, Str, Union, F32, I32, U32, U64};

const fn single(res_path: &'static str, record: CfsdObject) -> CfsdNestedLayout {
    CfsdNestedLayout {
        res_path,
        key: Str,
        record,
        bare: false,
        single: true,
    }
}

// ---- seasonalcampaignsloginrewards ----
static LOGIN_REWARD: CfsdObject = obj(
    48,
    Some(40),
    &[
        a("overlayTexturePath", 0, Str, 16),
        a("blueprintMaterialLevel", 8, U32, 1),
        a("blueprintProductivityLevel", 12, U32, 2),
        a("blueprintRuns", 16, U32, 4),
        a("quantity", 20, U32, 0),
        a("redeemedExpiryDays", 24, U32, 32),
        a("tier", 28, U32, 0),
        a("typeID", 32, U32, 0),
        a("isAutoInjectedBoosterOrSkin", 36, Bool, 8),
    ],
);
const LOGIN_REWARDS: CfsdType = Dict(&U32, &Obj(&LOGIN_REWARD), 56, 8);

// ---- challenges ----
static CHALLENGE_REWARD: CfsdObject = obj(
    16,
    Some(12),
    &[
        a("amount", 0, U32, 0),
        a("type_id", 4, U32, 0),
        a("reward_type_soul_bound", 8, Bool, 1),
    ],
);

// ---- atoms / nodetypes parameter value union ----
static ATOM_VALUE: [(u64, CfsdType); 19] = [
    (4, U32),
    (9, U32),
    (12, Str),
    (15, Str),
    (23, F32),
    (27, Str),
    (28, Str),
    (29, Str),
    (31, U32),
    (32, Str),
    (42, U32),
    (47, Str),
    (51, Str),
    (52, Str),
    (53, U32),
    (60, Str),
    (61, Str),
    (62, Str),
    (65, F64Arr(3)),
];
static ATOM_INPUT: CfsdObject = obj(
    64,
    Some(60),
    &[
        a("defaultValue", 0, Union(24, &ATOM_VALUE), 1),
        a("description", 32, Str, 0),
        a("parameterKey", 40, Str, 0),
        a("parameterType", 48, Str, 0),
        a("isRequired", 56, Bool, 0),
    ],
);
static NODE_INPUT: CfsdObject = obj(
    64,
    Some(60),
    &[
        a("defaultValue", 0, Union(24, &ATOM_VALUE), 1),
        a("description", 32, Str, 2),
        a("parameterKey", 40, Str, 0),
        a("parameterType", 48, Str, 0),
        a("isRequired", 56, Bool, 0),
    ],
);
static PARAM_OUTPUT: CfsdObject = obj(
    32,
    Some(24),
    &[
        a("description", 0, Str, 1),
        a("parameterKey", 8, Str, 0),
        a("parameterType", 16, Str, 0),
    ],
);
static ATOM_PARAMS: CfsdObject = obj(
    16,
    None,
    &[
        a("inputs", 0, List(&Obj(&ATOM_INPUT), 64), 0),
        a("outputs", 8, List(&Obj(&PARAM_OUTPUT), 32), 0),
    ],
);
static NODE_PARAMS: CfsdObject = obj(
    16,
    None,
    &[
        a("inputs", 0, List(&Obj(&NODE_INPUT), 64), 0),
        a("outputs", 8, List(&Obj(&PARAM_OUTPUT), 32), 0),
    ],
);
static SUBTITLE: CfsdObject = obj(8, None, &[a("text", 0, Str, 0)]);
static PORTS: CfsdObject = obj(
    16,
    None,
    &[a("inPorts", 0, STR_LIST, 0), a("outPorts", 8, STR_LIST, 0)],
);

// ---- objectivechains ----
static CHAIN_DICT_VALUE: [(u64, CfsdType); 5] =
    [(17, U32), (18, U32), (19, U32), (31, U32), (32, U32)];
const CHAIN_DICT: CfsdType = Dict(&Str, &Union(24, &CHAIN_DICT_VALUE), 40, 8);
static CHAIN_LIST_ITEM: [(u64, CfsdType); 1] = [(0, CHAIN_DICT)];
static CHAIN_VALUE: [(u64, CfsdType); 12] = [
    (4, U32),
    (14, U32),
    (18, U32),
    (19, U32),
    (23, F32),
    (30, U32_LIST),
    (31, U32),
    (40, U32),
    (58, U32),
    (60, Str),
    (65, CHAIN_DICT),
    (66, List(&Union(24, &CHAIN_LIST_ITEM), 32)),
];
static CHAIN_PARAM: CfsdObject = obj(
    56,
    Some(52),
    &[
        a("defaultValue", 0, Union(24, &CHAIN_VALUE), 1),
        a("parameterKey", 32, Str, 0),
        a("parameterType", 40, Str, 0),
        a("isRequired", 48, Bool, 0),
    ],
);
static CHAIN_TRIGGER: CfsdObject = obj(
    16,
    None,
    &[a("action", 0, Str, 0), a("objectiveKey", 8, Str, 0)],
);
static TASK_OVERRIDE: CfsdObject =
    obj(24, Some(16), &[a("icon", 0, Str, 1), a("title", 8, Str, 2)]);
static CHAIN_OBJECTIVE: CfsdObject = obj(
    168,
    Some(164),
    &[
        a("description", 64, Str, 2),
        a("groups", 72, STR_LIST, 4),
        a("inputParametersMap", 80, Dict(&Str, &Str, 16, 8), 0),
        a("key", 96, Str, 0),
        a(
            "taskOverrides",
            104,
            Dict(&Str, &Obj(&TASK_OVERRIDE), 32, 8),
            16,
        ),
        a("title", 120, Str, 32),
        a("tooltip", 128, Str, 64),
        a(
            "triggers",
            136,
            Dict(&Str, &List(&Obj(&CHAIN_TRIGGER), 16), 16, 8),
            0,
        ),
        a("objectiveType", 152, U32, 0),
        a("renderingOrder", 156, U32, 8),
        a("showCompleted", 160, Bool, 0),
        a("startActive", 161, Bool, 0),
    ],
);

// ---- objectivetypes ----
static TYPE_PARAM: CfsdObject = obj(
    24,
    None,
    &[
        a("parameterKey", 0, Str, 0),
        a("parameterType", 8, Str, 0),
        a("isRequired", 16, Bool, 0),
    ],
);
static TASK_TRIGGER: CfsdObject = obj(16, None, &[a("action", 0, Str, 0), a("taskKey", 8, Str, 0)]);
static OBJECTIVE_TASK: CfsdObject = obj(
    72,
    Some(64),
    &[
        a("icon", 0, Str, 1),
        a("inputParameters", 8, Dict(&Str, &Str, 16, 8), 0),
        a("key", 24, Str, 0),
        a("title", 32, Str, 2),
        a(
            "triggers",
            40,
            Dict(&Str, &List(&Obj(&TASK_TRIGGER), 16), 16, 8),
            0,
        ),
        a("taskType", 56, U32, 0),
        a("showCompleted", 60, Bool, 0),
        a("startActive", 61, Bool, 0),
    ],
);

// ---- operations / operationtasks ----
static OPERATION_REWARD: CfsdObject = obj(
    24,
    Some(20),
    &[
        a("amount", 0, U32, 0),
        a("description", 4, U32, 1),
        a("raceID", 8, U32, 2),
        a("title", 12, U32, 4),
        a("typeID", 16, U32, 0),
    ],
);
static SKIP_CONDITION: CfsdObject = obj(
    40,
    Some(32),
    &[
        a("identifier", 0, Str, 1),
        a("operand", 8, Str, 0),
        a("operator", 16, Str, 0),
        a("skipCondition", 24, Str, 0),
    ],
);
static BLACKBOARD_MESSAGE: CfsdObject = obj(
    16,
    None,
    &[a("messageName", 0, Str, 0), a("value", 8, Str, 0)],
);
static TASK_REWARD: CfsdObject = obj(16, None, &[a("amount", 0, U32, 0), a("typeID", 8, U32, 0)]);

// ---- ship_skin_design_components / crates / conversations ----
static LICENSE_GRANT: CfsdObject = obj(
    8,
    None,
    &[
        a("license_uses_granted", 0, I32, 0),
        a("type_id", 4, U32, 0),
    ],
);
static SEQUENCE_BINDER: CfsdObject = obj(
    8,
    None,
    &[a("count", 0, U32, 0), a("item_type_id", 4, U32, 0)],
);
static SPLASH: CfsdObject = obj(
    40,
    Some(32),
    &[
        a("resPath", 0, Str, 0),
        a("color", 8, F32Arr(4), 1),
        a("height", 24, U32, 0),
        a("width", 28, U32, 0),
    ],
);
static LOOT_BY_RACE: CfsdObject = obj(
    16,
    None,
    &[
        a("amarr", 0, U32, 0),
        a("caldari", 4, U32, 0),
        a("gallente", 8, U32, 0),
        a("minmatar", 12, U32, 0),
    ],
);
static LINE_HINT: CfsdObject = obj(
    32,
    Some(28),
    &[
        a("iconID", 0, U32, 1),
        a("imageColor", 4, F32Arr(4), 2),
        a("imageSize", 20, U32, 4),
        a("text", 24, U32, 0),
    ],
);
static CONVERSATION_LINE: CfsdObject = obj(
    72,
    Some(64),
    &[
        a("lineHints", 0, List(&Obj(&LINE_HINT), 32), 0),
        a("menuHighlights", 8, U32_LIST, 0),
        a("scenePath", 16, Str, 2),
        a("spaceObjectHighlights", 24, U32_LIST, 0),
        a("uiBlinks", 32, U32_LIST, 8),
        a("uiHighlights", 40, U32_LIST, 0),
        a("agentID", 48, U32, 0),
        a("musicTrigger", 52, U32, 1),
        a("sound", 56, U32, 4),
        a("text", 60, U32, 0),
    ],
);

// ---- contributionmethods / dungeonobjectives / surveys ----
static CONTRIBUTION_METHOD_PARAM: CfsdObject = obj(
    56,
    Some(48),
    &[
        a("icon", 0, Str, 1),
        a("info", 8, Str, 2),
        a("parameterKey", 16, Str, 0),
        a("parameterType", 24, Str, 0),
        a("title", 32, Str, 0),
        a("maxEntries", 40, U32, 4),
        a("isRequired", 44, Bool, 0),
    ],
);
static DUNGEON_BLACKBOARD_PARAM: CfsdObject = obj(
    48,
    Some(40),
    &[
        a("defaultValue", 0, Union(24, &CHAIN_VALUE), 1),
        a("parameterKey", 32, Str, 0),
    ],
);
static DUNGEON_OBJECTIVE_OVERRIDE: CfsdObject = obj(
    40,
    Some(32),
    &[
        a("description", 0, Str, 1),
        a(
            "taskOverrides",
            8,
            Dict(&Str, &Obj(&TASK_OVERRIDE), 32, 8),
            2,
        ),
        a("title", 24, Str, 4),
    ],
);
static DUNGEON_CLIENT_OBJECTIVES: CfsdObject = obj(
    40,
    Some(32),
    &[
        a(
            "blackboardParameters",
            0,
            List(&Obj(&DUNGEON_BLACKBOARD_PARAM), 48),
            1,
        ),
        a(
            "overrides",
            8,
            Dict(&Str, &Obj(&DUNGEON_OBJECTIVE_OVERRIDE), 48, 8),
            8,
        ),
        a("nodeGraphID", 24, I32, 2),
        a("objectiveChainID", 28, I32, 4),
    ],
);
static SURVEY_ITEM_REWARD: CfsdObject = obj(
    16,
    Some(12),
    &[
        a("amount", 0, I32, 0),
        a("type_id", 4, I32, 0),
        a("reward_type_soul_bound", 8, Bool, 1),
    ],
);

pub static CFSD_NESTED_LAYOUTS_OPS: &[CfsdNestedLayout] = &[
    rec(
        "res:/staticdata/seasonalcampaignsloginrewards.fsdbinary",
        U64,
        obj(
            64,
            Some(56),
            &[
                a("alphaRewards", 8, LOGIN_REWARDS, 0),
                a("omegaRewards", 24, LOGIN_REWARDS, 0),
                a("windowHeaderBackground", 40, Str, 2),
                a("campaignName", 48, U32, 0),
                a("autoClaim", 52, Bool, 0),
                a("retroClaim", 53, Bool, 1),
            ],
        ),
        false,
    ),
    rec(
        "res:/staticdata/atoms.fsdbinary",
        U64,
        obj(
            64,
            Some(60),
            &[
                a("description", 8, Str, 0),
                a("name", 16, Str, 0),
                a("parameters", 24, Obj(&ATOM_PARAMS), 0),
                a("subtitle", 40, Obj(&SUBTITLE), 1),
                a("tags", 48, STR_LIST, 0),
                a("atomType", 56, U32, 0),
            ],
        ),
        false,
    ),
    rec(
        "res:/staticdata/nodetypes.fsdbinary",
        U64,
        obj(
            96,
            Some(88),
            &[
                a("description", 8, Str, 1),
                a("name", 16, Str, 0),
                a("parameters", 24, Obj(&NODE_PARAMS), 0),
                a("ports", 40, Obj(&PORTS), 0),
                a("subtitle", 56, Obj(&SUBTITLE), 2),
                a("tags", 64, STR_LIST, 0),
                a("color", 72, F32Arr(4), 0),
            ],
        ),
        false,
    ),
    rec(
        "res:/staticdata/objectivechains.fsdbinary",
        U64,
        obj(
            40,
            Some(36),
            &[
                a("contextParameters", 8, List(&Obj(&CHAIN_PARAM), 56), 0),
                a("objectives", 16, List(&Obj(&CHAIN_OBJECTIVE), 168), 0),
                a("tags", 24, STR_LIST, 0),
                a("nodeGraph", 32, U32, 1),
            ],
        ),
        false,
    ),
    rec(
        "res:/staticdata/objectivetypes.fsdbinary",
        U64,
        obj(
            40,
            None,
            &[
                a("inputParameters", 8, List(&Obj(&TYPE_PARAM), 24), 0),
                a("tags", 16, STR_LIST, 0),
                a("tasks", 24, List(&Obj(&OBJECTIVE_TASK), 72), 0),
                a("title", 32, Str, 0),
            ],
        ),
        false,
    ),
    rec(
        "res:/staticdata/operations.fsdbinary",
        U64,
        obj(
            152,
            Some(148),
            &[
                a("name", 40, Str, 0),
                a(
                    "onActivationRewards",
                    48,
                    List(&Obj(&OPERATION_REWARD), 24),
                    0,
                ),
                a(
                    "onCompletionRewards",
                    56,
                    List(&Obj(&OPERATION_REWARD), 24),
                    0,
                ),
                a("primaryTags", 64, U32_LIST, 4096),
                a("secondaryTags", 72, U32_LIST, 8192),
                a("skillRewards", 80, U32_LIST, 0),
                a("tasksInOperation", 88, U32_LIST, 0),
                a("agencyContentGroupID", 96, U32, 1),
                a("clientNodeGraph", 100, U32, 8),
                a("description", 104, U32, 0),
                a("extraHint", 108, U32, 16),
                a("functionDescription", 112, U32, 32),
                a("iskReward", 116, U32, 0),
                a("musicTrigger", 120, U32, 256),
                a("notificationDescription", 124, U32, 512),
                a("notificationTitle", 128, U32, 1024),
                a("operationPrerequisites", 132, U32, 2048),
                a("siteConnection", 136, U32, 16384),
                a("title", 140, U32, 0),
                a("isReplayable", 144, Bool, 0),
            ],
        ),
        false,
    ),
    rec(
        "res:/staticdata/operationtasks.fsdbinary",
        U64,
        obj(
            120,
            Some(112),
            &[
                a(
                    "clientSkipConditions",
                    8,
                    List(&Obj(&SKIP_CONDITION), 40),
                    0,
                ),
                a(
                    "dungeonBlackboardMessages",
                    16,
                    List(&Obj(&BLACKBOARD_MESSAGE), 16),
                    4,
                ),
                a("eventConditions", 24, Dict(&Str, &STR_LIST, 16, 8), 0),
                a("name", 40, Str, 0),
                a("onCompletionRewards", 48, List(&Obj(&TASK_REWARD), 16), 0),
                a(
                    "serverSkipConditions",
                    56,
                    List(&Obj(&SKIP_CONDITION), 40),
                    0,
                ),
                a("achievementEventType", 80, U32, 0),
                a("conversationOnTaskStarted", 88, U32, 2),
                a("delayBefore", 92, F32, 0),
                a("description", 96, U32, 0),
                a("targetValue", 100, U32, 0),
                a("title", 104, U32, 0),
                a("isCheckpoint", 108, Bool, 0),
                a("isOptional", 109, Bool, 0),
                a("isSiteRelevant", 110, Bool, 0),
                a("isSupreme", 111, Bool, 0),
            ],
        ),
        false,
    ),
    rec(
        "res:/staticdata/ship_skin_design_components.fsdbinary",
        U64,
        obj(
            112,
            Some(104),
            &[
                a("associated_type_ids", 8, List(&Obj(&LICENSE_GRANT), 8), 0),
                a("disallowed_slots", 16, U32_LIST, 0),
                a("finish", 24, Str, 0),
                a("icon_file", 32, Str, 0),
                a("internal_name", 40, Str, 0),
                a("projection_type_u", 48, Str, 0),
                a("projection_type_v", 56, Str, 0),
                a("resource_file", 64, Str, 0),
                a("category", 72, U32, 0),
                a("description", 76, U32, 1),
                a("name", 80, U32, 0),
                a("rarity", 88, U32, 0),
                a("sequence_binder", 92, Obj(&SEQUENCE_BINDER), 0),
                a("published", 100, Bool, 0),
            ],
        ),
        false,
    ),
    rec(
        "res:/staticdata/crates.fsdbinary",
        U64,
        obj(
            168,
            Some(160),
            &[
                a("animatedSplash", 8, Obj(&SPLASH), 1),
                a("fittings", 48, U32_LIST, 2),
                a("fixedLootItems", 56, U32_LIST, 4),
                a("lootPresentationType", 64, Str, 0),
                a("lootTableType", 72, Str, 0),
                a("staticSplash", 80, Obj(&SPLASH), 128),
                a("descriptionID", 120, U32, 0),
                a("lootDescriptionID", 124, U32, 16),
                a("lootTableByRace", 128, Obj(&LOOT_BY_RACE), 32),
                a("nameID", 144, U32, 0),
                a("skillBundleID", 148, U32, 64),
                a("typeID", 152, U32, 0),
            ],
        ),
        false,
    ),
    rec(
        "res:/staticdata/conversations.fsdbinary",
        U64,
        obj(
            24,
            None,
            &[
                a("lines", 8, List(&Obj(&CONVERSATION_LINE), 72), 0),
                a("name", 16, Str, 0),
            ],
        ),
        false,
    ),
    rec(
        "res:/staticdata/manufacturableitems.fsdbinary",
        Str,
        obj(16, None, &[a("value", 8, U32_LIST, 0)]),
        true,
    ),
    single(
        "res:/staticdata/seasonselection.fsdbinary",
        obj(
            32,
            Some(24),
            &[
                a("navigation_card_picture_path", 0, Str, 0),
                a("agencyLargeSeasonSelectionTitleID", 8, U32, 0),
                a("seasonSelectionDescriptionID", 12, U32, 0),
                a("seasonSelectionHintID", 16, U32, 0),
                a("seasonSelectionTitleID", 20, U32, 0),
            ],
        ),
    ),
    rec(
        "res:/staticdata/contributionmethods.fsdbinary",
        Str,
        obj(
            72,
            Some(64),
            &[
                a("contentTags", 8, STR_LIST, 1),
                a("icon", 16, Str, 4),
                a("name", 24, Str, 0),
                a(
                    "parameters",
                    32,
                    List(&Obj(&CONTRIBUTION_METHOD_PARAM), 56),
                    0,
                ),
                a("descriptionID", 40, I32, 2),
                a("progressDescriptionID", 44, I32, 8),
                a("rewardingDescriptionID", 48, I32, 16),
                a("targetValueDescriptionID", 52, I32, 32),
                a("titleID", 56, I32, 0),
                a("unit", 60, I32, 64),
            ],
        ),
        false,
    ),
    rec(
        "res:/staticdata/dungeonobjectives.fsdbinary",
        U64,
        obj(
            56,
            Some(48),
            &[a(
                "clientObjectives",
                8,
                Obj(&DUNGEON_CLIENT_OBJECTIVES),
                1,
            )],
        ),
        false,
    ),
    rec(
        "res:/staticdata/surveys.fsdbinary",
        U64,
        obj(
            104,
            Some(100),
            &[
                a("apiClassName", 8, Str, 0),
                a("description", 16, Str, 2),
                a("endDate", 24, Str, 4),
                a("itemRewards", 32, List(&Obj(&SURVEY_ITEM_REWARD), 16), 8),
                a("name", 40, Str, 0),
                a("region", 48, STR_LIST, 32),
                a("resPath", 56, Str, 0),
                a("startDate", 64, Str, 0),
                a("url", 72, Str, 64),
                a("vgsRewards", 80, U32_LIST, 0),
                a("claimLabelID", 88, U32, 1),
                a("gracePeriod", 92, U32, 0),
                a("minAccountAgeHours", 96, U32, 16),
            ],
        ),
        false,
    ),
    rec(
        "res:/staticdata/challengeconditions.fsdbinary",
        Str,
        obj(16, None, &[a("value", 8, U64, 0)]),
        true,
    ),
    rec(
        "res:/staticdata/challengeevents.fsdbinary",
        Str,
        obj(16, None, &[a("value", 8, U64, 0)]),
        true,
    ),
    rec(
        "res:/staticdata/challenges.fsdbinary",
        U64,
        obj(
            112,
            Some(104),
            &[
                a("achievement_event_type", 8, Str, 0),
                a("challenge_category", 16, Str, 0),
                a("challenge_security", 24, Str, 0),
                a("event_conditions", 32, Dict(&Str, &U32_LIST, 16, 8), 1),
                a("display_name_id", 48, U32, 0),
                a("expiration_time_minutes", 52, U32, 0),
                a("isk_awarded", 56, U32, 0),
                a("max_progress", 60, U32, 0),
                a("message_text_id", 64, U32, 0),
                a("next_challenge", 68, U32, 2),
                a("points_awarded", 72, U32, 0),
                a("progress_text_id", 76, U32, 0),
                a("reward", 80, Obj(&CHALLENGE_REWARD), 4),
                a("is_chained", 96, Bool, 0),
                a("is_coop", 97, Bool, 0),
                a("should_be_active_on_season_start", 98, Bool, 0),
                a("should_not_expire_at_downtime", 99, Bool, 0),
                a("should_only_appear_once_per_season", 100, Bool, 0),
            ],
        ),
        false,
    ),
];
