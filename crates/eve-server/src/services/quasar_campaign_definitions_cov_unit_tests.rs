//! Focused coverage and mutation-killing unit tests for `quasar_campaign_definitions`.

#![forbid(unsafe_code)]

use super::*;
use crate::quasar_gateway::{PublicGatewayRouter, STATUS_INTERNAL_ERROR};
use crate::router::ServerContext;
use eve_db::WorldRepository;
use eve_proto::quasar::{
    AuthoritativeContext, DynamicQuasarMessage, DynamicValue, QuasarDescriptorRegistry,
};
use eve_sde::{SdeRegistry, SdeResourceInfo};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::clone(&ServerContext::new().sde)))
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum WireField {
    Varint(u64),
    Bytes(Vec<u8>),
}

fn read_varint(buf: &[u8], pos: &mut usize) -> u64 {
    let mut out = 0_u64;
    let mut shift = 0_u32;
    loop {
        let b = buf[*pos];
        *pos += 1;
        out |= u64::from(b & 0x7F) << shift;
        if (b & 0x80) == 0 {
            return out;
        }
        shift += 7;
    }
}

fn decode_wire(buf: &[u8]) -> Vec<(u32, WireField)> {
    let mut out = Vec::new();
    let mut pos = 0;
    while pos < buf.len() {
        let tag = read_varint(buf, &mut pos);
        let fno = (tag >> 3) as u32;
        match tag & 0x07 {
            0 => out.push((fno, WireField::Varint(read_varint(buf, &mut pos)))),
            2 => {
                let len = read_varint(buf, &mut pos) as usize;
                out.push((fno, WireField::Bytes(buf[pos..pos + len].to_vec())));
                pos += len;
            }
            wt => panic!("unexpected wire type {wt}"),
        }
    }
    out
}

fn field_varint(fields: &[(u32, WireField)], fno: u32) -> Option<u64> {
    fields.iter().find_map(|(n, v)| match v {
        WireField::Varint(x) if *n == fno => Some(*x),
        _ => None,
    })
}

fn field_bytes_all(fields: &[(u32, WireField)], fno: u32) -> Vec<Vec<u8>> {
    fields
        .iter()
        .filter_map(|(n, v)| match v {
            WireField::Bytes(b) if *n == fno => Some(b.clone()),
            _ => None,
        })
        .collect()
}

fn field_str(fields: &[(u32, WireField)], fno: u32) -> Option<String> {
    field_bytes_all(fields, fno)
        .into_iter()
        .next()
        .map(|b| String::from_utf8(b).expect("utf8"))
}

struct TempStaticFile(PathBuf);

impl Drop for TempStaticFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn build_synthetic_trigger_types_static(entries: &[(u32, bool, bool, &str)]) -> Vec<u8> {
    let mut body = Vec::new();
    let mut index = Vec::new();
    for &(id, guard_obj, guards, name) in entries {
        let val_off = body.len() as u32;
        body.push(u8::from(guard_obj));
        body.push(u8::from(guards));
        body.extend_from_slice(&[0_u8; 12]);
        body.extend_from_slice(&(name.len() as u32).to_le_bytes());
        body.extend_from_slice(name.as_bytes());
        index.push((id, val_off));
    }
    let mut footer = (index.len() as u32).to_le_bytes().to_vec();
    for (id, off) in index {
        footer.extend_from_slice(&id.to_le_bytes());
        footer.extend_from_slice(&off.to_le_bytes());
    }
    let footer_len = footer.len() as u32;
    body.extend_from_slice(&footer);
    body.extend_from_slice(&footer_len.to_le_bytes());

    let mut out = Vec::new();
    out.extend_from_slice(&0_u32.to_le_bytes()); // sch_len = 0
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(&body);
    out
}

#[test]
fn contribution_method_and_trigger_wire_encoders_field_boundaries() {
    assert_eq!(AUTHENTIC_CONTRIBUTION_METHODS.len(), 22);

    // Encode every entry in AUTHENTIC_CONTRIBUTION_METHODS and verify exact wire fields
    for &(name, title_id, progress_desc_id, icon, params) in AUTHENTIC_CONTRIBUTION_METHODS {
        let mut buf = Vec::new();
        encode_contribution_method_definition(name, title_id, progress_desc_id, icon, params, &mut buf);
        let top = decode_wire(&buf);
        let id_msg = decode_wire(&field_bytes_all(&top, 1)[0]);
        assert_eq!(field_str(&id_msg, 1).as_deref(), Some(name));

        let attrs = decode_wire(&field_bytes_all(&top, 2)[0]);
        let t_msg = decode_wire(&field_bytes_all(&attrs, 2)[0]);
        assert_eq!(field_varint(&t_msg, 1), Some(u64::from(title_id)));
        let pd_msg = decode_wire(&field_bytes_all(&attrs, 3)[0]);
        assert_eq!(field_varint(&pd_msg, 1), Some(u64::from(progress_desc_id)));
        let ic_msg = decode_wire(&field_bytes_all(&attrs, 4)[0]);
        assert_eq!(field_str(&ic_msg, 1).as_deref(), Some(icon));

        let encoded_params = field_bytes_all(&attrs, 1);
        assert_eq!(encoded_params.len(), params.len());
        for (enc_p, &(p_key, p_type, p_icon)) in encoded_params.iter().zip(params.iter()) {
            let p_outer = decode_wire(enc_p);
            let m_inner = decode_wire(&field_bytes_all(&p_outer, 1)[0]);
            assert_eq!(field_str(&m_inner, 1).as_deref(), Some(p_key));
            assert_eq!(field_str(&m_inner, 2).as_deref(), Some(p_type));
            let pi_msg = decode_wire(&field_bytes_all(&m_inner, 6)[0]);
            assert_eq!(field_str(&pi_msg, 1).as_deref(), Some(p_icon));
        }
    }

    // Zero / empty boundaries omit optional title_id (2), progress_desc_id (3), icon (4), and param icon (6)
    let mut minimal = Vec::new();
    encode_contribution_method_definition(
        "MinimalMethod",
        0,
        0,
        "",
        &[("alpha_key", "alpha_type", ""), ("beta_key", "beta_type", "beta_icon")],
        &mut minimal,
    );
    let min_top = decode_wire(&minimal);
    let min_attrs = decode_wire(&field_bytes_all(&min_top, 2)[0]);
    assert!(field_bytes_all(&min_attrs, 2).is_empty(), "title_id == 0 must omit field 2");
    assert!(field_bytes_all(&min_attrs, 3).is_empty(), "progress_desc_id == 0 must omit field 3");
    assert!(field_bytes_all(&min_attrs, 4).is_empty(), "empty icon must omit field 4");
    let min_params = field_bytes_all(&min_attrs, 1);
    assert_eq!(min_params.len(), 2);
    let p0 = decode_wire(&field_bytes_all(&decode_wire(&min_params[0]), 1)[0]);
    assert!(field_bytes_all(&p0, 6).is_empty(), "empty param icon must omit field 6");
    let p1 = decode_wire(&field_bytes_all(&decode_wire(&min_params[1]), 1)[0]);
    assert_eq!(
        field_str(&decode_wire(&field_bytes_all(&p1, 6)[0]), 1).as_deref(),
        Some("beta_icon")
    );

    // Positive boundary values (1) include title_id and progress_desc_id
    let mut boundary_one = Vec::new();
    encode_contribution_method_definition("BoundaryOne", 1, 1, "i", &[], &mut boundary_one);
    let b_attrs = decode_wire(&field_bytes_all(&decode_wire(&boundary_one), 2)[0]);
    assert_eq!(field_varint(&decode_wire(&field_bytes_all(&b_attrs, 2)[0]), 1), Some(1));
    assert_eq!(field_varint(&decode_wire(&field_bytes_all(&b_attrs, 3)[0]), 1), Some(1));

    // Trigger eventmatch definition wire format
    let mut trig_buf = Vec::new();
    encode_trigger_eventmatch_definition(
        "EntityDamaged",
        &[("allow_on_guard_object", "true"), ("allow_on_guards", "false")],
        &mut trig_buf,
    );
    let trig_top = decode_wire(&trig_buf);
    assert_eq!(
        field_str(&decode_wire(&field_bytes_all(&trig_top, 1)[0]), 1).as_deref(),
        Some("EntityDamaged")
    );
    let trig_params = field_bytes_all(&decode_wire(&field_bytes_all(&trig_top, 2)[0]), 1);
    let keys: Vec<String> = trig_params
        .iter()
        .map(|b| field_str(&decode_wire(b), 1).unwrap())
        .collect();
    assert_eq!(keys, vec!["allow_on_guard_object", "allow_on_guards"]);

    // Re-exported `extract_campaign_id` works on valid CAMPAIGN UUID and rejects invalid inputs
    let mut uuid = [0_u8; 16];
    uuid[0..8].copy_from_slice(b"CAMPAIGN");
    uuid[8..16].copy_from_slice(&77_u64.to_be_bytes());
    let mut id_dyn = DynamicQuasarMessage::new("Id");
    id_dyn.fields.insert(1, DynamicValue::Bytes(uuid.to_vec()));
    assert_eq!(
        extract_campaign_id(Some(&DynamicValue::Message(Box::new(id_dyn)))),
        Some(77)
    );
    assert_eq!(extract_campaign_id(None), None);
}

#[test]
fn gateway_handlers_missing_sde_error_paths_and_blank_trigger_name_fallback() {
    let db = Arc::new(WorldRepository::with_demo_seed());
    let descriptors = Arc::new(QuasarDescriptorRegistry::empty_with_builtins());
    let real_sde = shared_sde();
    let auth = AuthoritativeContext::for_session(Some(1), Some(90_000_001), Some(1_000_009), 0, "tranquility", vec![1]);
    let any_empty = GoogleAny::default();
    let req_empty = DynamicQuasarMessage::new("Req");

    // 1. Manager with `sde: None` -> contributionmethod returns empty GetAllResponse; trigger returns 500
    let router_none = PublicGatewayRouter::new(Arc::clone(&descriptors), Arc::clone(&db), Arc::clone(&real_sde));
    let mgr_none = Arc::new(ProgressionExtraManager {
        sde: None,
        ..Default::default()
    });
    register_campaign_definition_handlers(&router_none, mgr_none);
    {
        let h = router_none.handlers.read().unwrap();
        let cm_none = h["eve_public.campaign.contributionmethod.definition.api.GetAllRequest"](
            &auth, &any_empty, &req_empty,
        )
        .expect("cm with sde=None");
        assert!(cm_none.value.is_empty());
        assert!(cm_none
            .type_url
            .ends_with("eve_public.campaign.contributionmethod.definition.api.GetAllResponse"));

        let em_err = h["eve_public.campaign.trigger.eventmatch.definition.api.GetAllRequest"](
            &auth, &any_empty, &req_empty,
        )
        .unwrap_err();
        assert_eq!(em_err.0, STATUS_INTERNAL_ERROR);
        assert!(em_err.1.contains("SDE trigger types unavailable"));
    }

    // 2. Manager with `SdeRegistry::default()` (missing resources) -> both return STATUS_INTERNAL_ERROR
    let router_broken = PublicGatewayRouter::new(Arc::clone(&descriptors), Arc::clone(&db), Arc::clone(&real_sde));
    let mgr_broken = Arc::new(ProgressionExtraManager {
        sde: Some(Arc::new(SdeRegistry::default())),
        ..Default::default()
    });
    register_campaign_definition_handlers(&router_broken, mgr_broken);
    {
        let h = router_broken.handlers.read().unwrap();
        let cm_err = h["eve_public.campaign.contributionmethod.definition.api.GetAllRequest"](
            &auth, &any_empty, &req_empty,
        )
        .unwrap_err();
        assert_eq!(cm_err.0, STATUS_INTERNAL_ERROR);
        assert!(cm_err.1.contains("Failed to load SDE contribution methods"));

        let em_err = h["eve_public.campaign.trigger.eventmatch.definition.api.GetAllRequest"](
            &auth, &any_empty, &req_empty,
        )
        .unwrap_err();
        assert_eq!(em_err.0, STATUS_INTERNAL_ERROR);
        assert!(em_err.1.contains("Failed to load SDE trigger types"));
    }

    // 3. Synthetic `triggertypes.static` exercises sorting by trigger_type_id AND blank name -> `TriggerType_{id}`
    static SEQ: AtomicU64 = AtomicU64::new(1);
    let tmp_path = std::env::temp_dir().join(format!(
        "evomu_synth_triggertypes_{}_{}.static",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let raw_bytes = build_synthetic_trigger_types_static(&[
        (42, true, false, "   "),
        (7, false, true, "NamedTrigger"),
    ]);
    std::fs::write(&tmp_path, &raw_bytes).expect("write synth triggertypes");
    let _guard = TempStaticFile(tmp_path.clone());

    let mut synth_sde = SdeRegistry::default();
    synth_sde.resources.insert(
        "res:/staticdata/triggertypes.static".into(),
        SdeResourceInfo {
            res_path: "res:/staticdata/triggertypes.static".into(),
            cdn_relative_path: tmp_path.display().to_string(),
            md5: String::new(),
            byte_size: raw_bytes.len() as u64,
            format: "fsd".into(),
            record_count: 2,
            spot: String::new(),
        },
    );
    let router_synth = PublicGatewayRouter::new(Arc::clone(&descriptors), Arc::clone(&db), Arc::clone(&real_sde));
    let mgr_synth = Arc::new(ProgressionExtraManager {
        sde: Some(Arc::new(synth_sde)),
        ..Default::default()
    });
    register_campaign_definition_handlers(&router_synth, mgr_synth);
    {
        let h = router_synth.handlers.read().unwrap();
        let em_ok = h["eve_public.campaign.trigger.eventmatch.definition.api.GetAllRequest"](
            &auth, &any_empty, &req_empty,
        )
        .expect("synth trigger GetAllRequest");
        let defs = field_bytes_all(&decode_wire(&em_ok.value), 1);
        let names: Vec<String> = defs
            .iter()
            .map(|b| field_str(&decode_wire(&field_bytes_all(&decode_wire(b), 1)[0]), 1).unwrap())
            .collect();
        // Sorted by trigger_type_id: 7 ("NamedTrigger") then 42 ("TriggerType_42")
        assert_eq!(names, vec!["NamedTrigger", "TriggerType_42"]);
    }

    // 4. Full V24 SDE round-trip count verification
    let router_real = PublicGatewayRouter::new(descriptors, db, Arc::clone(&real_sde));
    let mgr_real = Arc::new(ProgressionExtraManager {
        sde: Some(Arc::clone(&real_sde)),
        ..Default::default()
    });
    register_campaign_definition_handlers(&router_real, mgr_real);
    {
        let h = router_real.handlers.read().unwrap();
        let cm_real = h["eve_public.campaign.contributionmethod.definition.api.GetAllRequest"](
            &auth, &any_empty, &req_empty,
        )
        .expect("real cm");
        let expected_cm_len = real_sde.contribution_methods().expect("sde cm").len();
        assert_eq!(field_bytes_all(&decode_wire(&cm_real.value), 1).len(), expected_cm_len);

        let em_real = h["eve_public.campaign.trigger.eventmatch.definition.api.GetAllRequest"](
            &auth, &any_empty, &req_empty,
        )
        .expect("real em");
        let expected_em_len = real_sde.load_trigger_types().expect("sde triggers").len();
        assert_eq!(field_bytes_all(&decode_wire(&em_real.value), 1).len(), expected_em_len);
    }
}

#[test]
fn authentic_methods_uniqueness_and_trigger_encoder_param_ordering() {
    use super::{
        encode_contribution_method_definition, encode_trigger_eventmatch_definition,
        AUTHENTIC_CONTRIBUTION_METHODS,
    };

    // 1. All 22 canonical contribution methods have unique string keys, positive localization IDs, and non-empty icons
    assert_eq!(AUTHENTIC_CONTRIBUTION_METHODS.len(), 22);
    let mut keys = std::collections::HashSet::new();
    for (method_key, title_id, progress_desc_id, icon, params) in AUTHENTIC_CONTRIBUTION_METHODS {
        assert!(!method_key.trim().is_empty());
        assert!(*title_id > 0);
        assert!(*progress_desc_id > 0);
        assert!(!icon.trim().is_empty());
        assert!(keys.insert(*method_key), "duplicate method key {method_key}");
        for (p_key, _p_type, _p_opt) in *params {
            assert!(!p_key.trim().is_empty());
        }
    }

    // 2. Contribution method encoder emits ordered field tags for empty vs multi-param entries
    let mut cm_empty = Vec::new();
    encode_contribution_method_definition("CustomEmpty", 101, 102, "res:/icon.png", &[], &mut cm_empty);
    let cm_decoded = decode_wire(&cm_empty);
    assert_eq!(
        field_bytes_all(&decode_wire(&field_bytes_all(&cm_decoded, 1)[0]), 1)[0],
        b"CustomEmpty"
    );

    // 3. Trigger eventmatch encoder emits id (tag 1) and attrs with every param key in order (tag 2 -> tag 1)
    let mut trig_out = Vec::new();
    encode_trigger_eventmatch_definition(
        "OnSyntheticTrigger", // rpc-lint: allow test fixture
        &[("target_ship", "ui_ship"), ("solar_system", "ui_sys")],
        &mut trig_out,
    );
    let trig_wire = decode_wire(&trig_out);
    let id_msg = decode_wire(&field_bytes_all(&trig_wire, 1)[0]);
    assert_eq!(field_bytes_all(&id_msg, 1)[0], b"OnSyntheticTrigger"); // rpc-lint: allow test fixture
    let attr_msg = decode_wire(&field_bytes_all(&trig_wire, 2)[0]);
    let encoded_keys: Vec<Vec<u8>> = field_bytes_all(&attr_msg, 1)
        .into_iter()
        .map(|blob| field_bytes_all(&decode_wire(&blob), 1).remove(0))
        .collect();
    assert_eq!(encoded_keys, vec![b"target_ship".to_vec(), b"solar_system".to_vec()]);
}
